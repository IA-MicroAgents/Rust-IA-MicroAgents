use std::{path::PathBuf, time::Instant};

use jsonschema::Validator;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::{
    io::AsyncWriteExt,
    process::Command,
    time::{sleep, timeout, Duration},
};
use tracing::warn;

use crate::{
    errors::{AppError, AppResult},
    identity::compiler::SystemIdentity,
    policy::permissions,
    storage::{Store, ToolTraceInsert},
};

use super::{
    builtin::execute_builtin,
    manifest::{SkillKind, SkillManifest},
    registry::{SkillDefinition, SkillRegistry},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillCall {
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillResult {
    pub skill_name: String,
    pub ok: bool,
    pub output: Value,
    pub error: Option<String>,
    pub duration_ms: u64,
}

#[derive(Clone)]
pub struct SkillRunner {
    registry: SkillRegistry,
    store: Store,
    http_allowlist: Vec<String>,
    http_client: reqwest::Client,
}

impl SkillRunner {
    pub fn new(
        registry: SkillRegistry,
        store: Store,
        http_allowlist: Vec<String>,
    ) -> AppResult<Self> {
        let http_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|e| AppError::Http(format!("http client init failed: {e}")))?;

        Ok(Self {
            registry,
            store,
            http_allowlist,
            http_client,
        })
    }

    pub async fn execute(
        &self,
        identity: &SystemIdentity,
        call: SkillCall,
        trace_id: &str,
        conversation_id: Option<i64>,
        user_id: &str,
    ) -> SkillResult {
        let started = Instant::now();

        let result = match self.registry.get(&call.name) {
            Some(skill) => {
                self.execute_with_definition(
                    identity,
                    &skill,
                    &call.arguments,
                    conversation_id,
                    user_id,
                )
                .await
            }
            None => Err(AppError::Skill(format!("skill {} not found", call.name))),
        };

        let duration_ms = started.elapsed().as_millis() as u64;
        match result {
            Ok(output) => {
                let _ = self
                    .store
                    .insert_tool_trace(ToolTraceInsert {
                        trace_id,
                        skill_name: &call.name,
                        input_json: &call.arguments,
                        output_json: Some(&output),
                        status: "ok",
                        duration_ms,
                        error: None,
                    })
                    .await;
                SkillResult {
                    skill_name: call.name,
                    ok: true,
                    output,
                    error: None,
                    duration_ms,
                }
            }
            Err(err) => {
                let error_text = err.to_string();
                let _ = self
                    .store
                    .insert_tool_trace(ToolTraceInsert {
                        trace_id,
                        skill_name: &call.name,
                        input_json: &call.arguments,
                        output_json: None,
                        status: "error",
                        duration_ms,
                        error: Some(&error_text),
                    })
                    .await;
                SkillResult {
                    skill_name: call.name,
                    ok: false,
                    output: json!({}),
                    error: Some(error_text),
                    duration_ms,
                }
            }
        }
    }

    async fn execute_with_definition(
        &self,
        identity: &SystemIdentity,
        skill: &SkillDefinition,
        arguments: &Value,
        conversation_id: Option<i64>,
        user_id: &str,
    ) -> AppResult<Value> {
        if !permissions::is_skill_allowed(identity.permissions(), &skill.manifest.name) {
            return Err(AppError::PermissionDenied(format!(
                "skill {} is not allowed by identity",
                skill.manifest.name
            )));
        }

        validate_schema("input", skill.manifest.input_schema.as_ref(), arguments)?;

        let mut attempts = 0_u32;
        let mut last_err: Option<AppError> = None;

        while attempts <= skill.manifest.max_retries {
            attempts += 1;
            let out = timeout(
                Duration::from_millis(skill.manifest.timeout_ms),
                self.execute_once(skill, arguments, conversation_id, user_id),
            )
            .await;

            match out {
                Ok(Ok(payload)) => {
                    validate_schema("output", skill.manifest.output_schema.as_ref(), &payload)?;
                    return Ok(payload);
                }
                Ok(Err(err)) => {
                    last_err = Some(err);
                }
                Err(_) => {
                    last_err = Some(AppError::Timeout(format!(
                        "skill {} timed out after {}ms",
                        skill.manifest.name, skill.manifest.timeout_ms
                    )));
                }
            }

            if attempts <= skill.manifest.max_retries {
                sleep(Duration::from_millis(120 * attempts as u64)).await;
            }
        }

        Err(last_err.unwrap_or_else(|| {
            AppError::Skill(format!(
                "skill {} failed without details",
                skill.manifest.name
            ))
        }))
    }

    async fn execute_once(
        &self,
        skill: &SkillDefinition,
        arguments: &Value,
        conversation_id: Option<i64>,
        user_id: &str,
    ) -> AppResult<Value> {
        match skill.manifest.kind {
            SkillKind::Builtin => {
                execute_builtin(
                    &skill.manifest.name,
                    arguments,
                    &self.store,
                    conversation_id,
                    user_id,
                    &self.http_allowlist,
                    &self.http_client,
                )
                .await
            }
            SkillKind::Command => {
                let entrypoint = resolve_command_entrypoint(skill)?;
                execute_command_skill(&entrypoint, arguments).await
            }
            SkillKind::Http => {
                execute_http_skill(
                    &self.http_client,
                    &skill.manifest,
                    arguments,
                    &self.http_allowlist,
                )
                .await
            }
        }
    }
}

fn validate_schema(label: &str, schema: Option<&Value>, payload: &Value) -> AppResult<()> {
    let Some(schema) = schema else {
        return Ok(());
    };

    let validator: Validator = jsonschema::validator_for(schema)
        .map_err(|e| AppError::Validation(format!("invalid {label}_schema: {e}")))?;
    if validator.is_valid(payload) {
        return Ok(());
    }

    let errors = validator
        .iter_errors(payload)
        .map(|e| e.to_string())
        .collect::<Vec<_>>()
        .join("; ");
    Err(AppError::Validation(format!(
        "{label} schema validation failed: {errors}"
    )))
}

fn resolve_command_entrypoint(skill: &SkillDefinition) -> AppResult<PathBuf> {
    let ep = PathBuf::from(&skill.manifest.entrypoint);
    if ep.is_absolute() {
        return Ok(ep);
    }

    let candidate = skill.folder.join(&skill.manifest.entrypoint);
    if candidate.exists() {
        return Ok(candidate);
    }

    Err(AppError::Skill(format!(
        "command skill {} entrypoint '{}' not found",
        skill.manifest.name, skill.manifest.entrypoint
    )))
}

async fn execute_command_skill(entrypoint: &PathBuf, arguments: &Value) -> AppResult<Value> {
    let mut command = Command::new(entrypoint);
    command.stdin(std::process::Stdio::piped());
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());

    let mut child = command.spawn().map_err(|e| {
        AppError::Skill(format!(
            "failed to spawn command skill {}: {e}",
            entrypoint.display()
        ))
    })?;

    if let Some(stdin) = child.stdin.as_mut() {
        let body = json!({"input": arguments}).to_string();
        stdin
            .write_all(body.as_bytes())
            .await
            .map_err(|e| AppError::Skill(format!("command stdin write failed: {e}")))?;
    }

    let output = child
        .wait_with_output()
        .await
        .map_err(|e| AppError::Skill(format!("command wait failed: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AppError::Skill(format!(
            "command skill failed: {}",
            stderr.trim()
        )));
    }

    let stdout = String::from_utf8(output.stdout)
        .map_err(|e| AppError::Skill(format!("command stdout utf8 failed: {e}")))?;
    let parsed: Value = serde_json::from_str(stdout.trim())
        .map_err(|e| AppError::Skill(format!("command output is not json: {e}")))?;
    Ok(parsed)
}

async fn execute_http_skill(
    client: &reqwest::Client,
    manifest: &SkillManifest,
    arguments: &Value,
    allowlist: &[String],
) -> AppResult<Value> {
    let url = reqwest::Url::parse(&manifest.entrypoint)
        .map_err(|e| AppError::Skill(format!("invalid http skill entrypoint: {e}")))?;
    let host = url.host_str().unwrap_or_default();
    if !allowlist
        .iter()
        .any(|allowed| allowed.eq_ignore_ascii_case(host))
    {
        return Err(AppError::PermissionDenied(format!(
            "http skill host {host} is not allowlisted"
        )));
    }

    let response = client
        .post(url)
        .json(arguments)
        .send()
        .await
        .map_err(|e| AppError::Skill(format!("http skill request failed: {e}")))?;

    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|e| AppError::Skill(format!("http skill response read failed: {e}")))?;
    if !status.is_success() {
        return Err(AppError::Skill(format!(
            "http skill non-2xx status {} body {}",
            status,
            text.chars().take(512).collect::<String>()
        )));
    }

    serde_json::from_str(&text).map_err(|e| {
        warn!(error = %e, payload = %text, "http skill returned non-json payload");
        AppError::Skill(format!("http skill output json parse failed: {e}"))
    })
}
