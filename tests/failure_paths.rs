mod support;

use ai_microagents::{
    identity::IdentityManager,
    llm::{
        openrouter::OpenRouterClient, ChatMessage, LlmProvider, LlmRequest, LlmResponse,
        ProviderError, ProviderResult, Usage,
    },
    orchestrator::response_parser::parse_or_repair_decision,
    skills::{SkillCall, SkillRegistry, SkillRunner},
    storage::Store,
};
use async_trait::async_trait;
use serde_json::json;
use tempfile::tempdir;
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

#[tokio::test]
async fn invalid_model_id_is_reported() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"id": "known-model", "modality": ["text"]}]
        })))
        .mount(&server)
        .await;

    let client = OpenRouterClient::new(ai_microagents::config::OpenRouterConfig {
        api_key: "x".to_string(),
        base_url: server.uri(),
        app_name: None,
        site_url: None,
        timeout_ms: 5000,
        validate_models_on_start: true,
        mock_mode: false,
    })
    .expect("client");

    let routes = ai_microagents::identity::schema::ModelRoutes {
        fast: "missing".to_string(),
        reasoning: "missing".to_string(),
        tool_use: "missing".to_string(),
        vision: "missing".to_string(),
        reviewer: "missing".to_string(),
        planner: "missing".to_string(),
        router_fast: None,
        fast_text: None,
        reviewer_fast: None,
        reviewer_strict: None,
        integrator_complex: None,
        vision_understand: None,
        audio_transcribe: None,
        image_generate: None,
        fallback: vec![],
    };

    let result = client.validate_models(&routes).await;
    assert!(matches!(result, Err(ProviderError::BadModelId(_))));
}

#[tokio::test]
async fn openrouter_rate_limit_bubbles_up() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(429).set_body_string("rate limited"))
        .mount(&server)
        .await;

    let client = OpenRouterClient::new(ai_microagents::config::OpenRouterConfig {
        api_key: "x".to_string(),
        base_url: server.uri(),
        app_name: None,
        site_url: None,
        timeout_ms: 1000,
        validate_models_on_start: false,
        mock_mode: false,
    })
    .expect("client");

    let out = client
        .chat_completion(LlmRequest {
            model: ai_microagents::llm::OPENROUTER_FREE_MODEL.to_string(),
            messages: vec![ChatMessage::text("user", "hi")],
            max_output_tokens: 64,
            temperature: 0.0,
            require_json: false,
            timeout_ms: 1000,
        })
        .await;

    assert!(matches!(out, Err(ProviderError::RateLimit)));
}

#[tokio::test]
async fn malformed_llm_json_falls_back_safely() {
    #[derive(Clone)]
    struct BadProvider;

    #[async_trait]
    impl LlmProvider for BadProvider {
        async fn validate_models(
            &self,
            _routes: &ai_microagents::identity::schema::ModelRoutes,
        ) -> ProviderResult<Vec<ai_microagents::llm::ModelCapabilities>> {
            Ok(vec![])
        }

        async fn chat_completion(&self, _request: LlmRequest) -> ProviderResult<LlmResponse> {
            Ok(LlmResponse {
                model: "m".to_string(),
                content: "not json".to_string(),
                usage: Usage::default(),
                latency_ms: 1,
            })
        }

        fn model_catalog(&self) -> Vec<ai_microagents::llm::ModelMetadata> {
            vec![]
        }
    }

    let provider = BadProvider;
    let decision = parse_or_repair_decision(
        &provider,
        ai_microagents::llm::OPENROUTER_FREE_MODEL,
        "{broken",
        1000,
    )
        .await
        .expect("decision");
    assert_eq!(
        decision.route,
        ai_microagents::llm::response_types::DecisionRoute::AskClarification
    );
}

#[tokio::test]
async fn invalid_skill_schema_fails_closed() {
    let temp = tempdir().expect("tempdir");
    let identity_path = temp.path().join("IDENTITY.md");
    let skills_dir = temp.path().join("skills");
    std::fs::create_dir_all(&skills_dir).expect("skills dir");
    write_identity(&identity_path);
    write_validation_skill(&skills_dir);

    let identity = IdentityManager::load(identity_path.clone()).expect("identity");
    let skills = SkillRegistry::load(skills_dir.clone()).expect("skills");
    let backend = support::TestBackend::new("failure_invalid_skill_schema");
    let cfg = ai_microagents::config::AppConfig {
        bind_addr: "127.0.0.1:0".to_string(),
        database: backend.database.clone(),
        cache: backend.cache.clone(),
        bus: ai_microagents::config::BusConfig {
            enabled: true,
            stream_prefix: "ai-microagents-test".to_string(),
            stream_maxlen: 2000,
            outbox_publish_batch: 32,
            outbox_poll_ms: 200,
            outbox_max_retries: 4,
            stream_reclaim_idle_ms: 60_000,
            consumer_name: "failure-paths".to_string(),
            memory_consumer_concurrency: 1,
            jobs_consumer_concurrency: 1,
        },
        identity_path: identity_path.clone(),
        skills_dir: skills_dir.clone(),
        openrouter: ai_microagents::config::OpenRouterConfig {
            api_key: "x".to_string(),
            base_url: "http://127.0.0.1:1".to_string(),
            app_name: None,
            site_url: None,
            timeout_ms: 1000,
            validate_models_on_start: false,
            mock_mode: true,
        },
        telegram: ai_microagents::config::TelegramConfig {
            enabled: true,
            bot_token: "test-token".to_string(),
            base_url: "https://api.telegram.org".to_string(),
            poll_timeout_secs: 1,
            poll_backoff_ms: 50,
            max_reply_chars: 3500,
            bot_username: String::new(),
            webhook_enabled: false,
            webhook_path: "/telegram/webhook".to_string(),
            webhook_secret: String::new(),
            typing_delay_ms: 800,
        },
        policy: ai_microagents::config::PolicyConfig {
            outbound_enabled: false,
            dry_run: true,
            http_skill_allowlist: vec![],
            outbound_kill_switch: true,
        },
        runtime: ai_microagents::config::RuntimeConfig {
            queue_capacity: 8,
            worker_concurrency: 1,
            reminder_poll_ms: 1000,
        },
        team: ai_microagents::team::TeamConfig::from_env().expect("team config"),
        dashboard: ai_microagents::config::DashboardConfig {
            enable_dashboard: false,
            bind_addr: "127.0.0.1:0".to_string(),
            auth_token: String::new(),
        },
    };
    let store = Store::from_config(&cfg).await.expect("db");
    let runner = SkillRunner::new(skills, store, vec![]).expect("runner");

    let result = runner
        .execute(
            &identity.get(),
            SkillCall {
                name: "memory.write".to_string(),
                arguments: json!({"key": 12, "value": "x"}),
            },
            "trace",
            None,
            "user",
        )
        .await;

    assert!(!result.ok);
    assert!(result
        .error
        .unwrap_or_default()
        .contains("schema validation failed"));
}

#[tokio::test]
async fn command_skill_timeout_is_enforced() {
    let temp = tempdir().expect("tempdir");
    let identity_path = temp.path().join("IDENTITY.md");
    let skills_dir = temp.path().join("skills");
    std::fs::create_dir_all(&skills_dir).expect("skills dir");
    write_identity(&identity_path);
    write_timeout_skill(&skills_dir);

    let identity = IdentityManager::load(identity_path.clone()).expect("identity");
    let skills = SkillRegistry::load(skills_dir.clone()).expect("skills");
    let backend = support::TestBackend::new("failure_skill_timeout");
    let cfg = ai_microagents::config::AppConfig {
        bind_addr: "127.0.0.1:0".to_string(),
        database: backend.database.clone(),
        cache: backend.cache.clone(),
        bus: ai_microagents::config::BusConfig {
            enabled: true,
            stream_prefix: "ai-microagents-test".to_string(),
            stream_maxlen: 2000,
            outbox_publish_batch: 32,
            outbox_poll_ms: 200,
            outbox_max_retries: 4,
            stream_reclaim_idle_ms: 60_000,
            consumer_name: "failure-paths".to_string(),
            memory_consumer_concurrency: 1,
            jobs_consumer_concurrency: 1,
        },
        identity_path: identity_path.clone(),
        skills_dir: skills_dir.clone(),
        openrouter: ai_microagents::config::OpenRouterConfig {
            api_key: "x".to_string(),
            base_url: "http://127.0.0.1:1".to_string(),
            app_name: None,
            site_url: None,
            timeout_ms: 1000,
            validate_models_on_start: false,
            mock_mode: true,
        },
        telegram: ai_microagents::config::TelegramConfig {
            enabled: true,
            bot_token: "test-token".to_string(),
            base_url: "https://api.telegram.org".to_string(),
            poll_timeout_secs: 1,
            poll_backoff_ms: 50,
            max_reply_chars: 3500,
            bot_username: String::new(),
            webhook_enabled: false,
            webhook_path: "/telegram/webhook".to_string(),
            webhook_secret: String::new(),
            typing_delay_ms: 800,
        },
        policy: ai_microagents::config::PolicyConfig {
            outbound_enabled: false,
            dry_run: true,
            http_skill_allowlist: vec![],
            outbound_kill_switch: true,
        },
        runtime: ai_microagents::config::RuntimeConfig {
            queue_capacity: 8,
            worker_concurrency: 1,
            reminder_poll_ms: 1000,
        },
        team: ai_microagents::team::TeamConfig::from_env().expect("team config"),
        dashboard: ai_microagents::config::DashboardConfig {
            enable_dashboard: false,
            bind_addr: "127.0.0.1:0".to_string(),
            auth_token: String::new(),
        },
    };
    let store = Store::from_config(&cfg).await.expect("db");
    let runner = SkillRunner::new(skills, store, vec![]).expect("runner");

    let result = runner
        .execute(
            &identity.get(),
            SkillCall {
                name: "sleepy.command".to_string(),
                arguments: json!({}),
            },
            "trace",
            None,
            "user",
        )
        .await;

    assert!(!result.ok);
    assert!(result.error.unwrap_or_default().contains("timed out"));
}

fn write_identity(path: &std::path::Path) {
    let content = r#"---
id: ai-microagents-test
display_name: AI MicroAgents Test
description: Test identity
locale: en-US
timezone: UTC
model_routes:
  fast: openrouter/free
  reasoning: openrouter/free
  tool_use: openrouter/free
  vision: openrouter/free
  reviewer: openrouter/free
  planner: openrouter/free
  fallback: [openrouter/free]
budgets:
  max_steps: 4
  max_turn_cost_usd: 1.0
  max_input_tokens: 4096
  max_output_tokens: 512
  max_tool_calls: 3
  timeout_ms: 10000
memory:
  save_facts: true
  save_summaries: true
  summarize_every_n_turns: 10
permissions:
  allowed_skills: ["*"]
  denied_skills: []
channels:
  telegram:
    enabled: true
    max_reply_chars: 3500
    style_overrides: concise
---
## Mission
Assist quickly.
## Persona
Reliable operator assistant.
## Tone
Concise and factual.
## Hard Rules
Never skip safety checks.
## Do Not Do
Do not fabricate external results.
## Escalation
Ask clarifying question when uncertain.
## Memory Preferences
Store only stable facts.
## Channel Notes
Keep Telegram replies short.
## Planning Principles
Decompose into bounded tasks with explicit dependencies.
## Review Standards
Reject outputs that do not satisfy acceptance criteria.
"#;
    std::fs::write(path, content).expect("write identity");
}

fn write_validation_skill(skills_root: &std::path::Path) {
    let folder = skills_root.join("memory_write");
    std::fs::create_dir_all(&folder).expect("skill folder");
    let content = r#"---
name: memory.write
version: 1.0.0
description: Write memory
kind: builtin
entrypoint: memory.write
input_schema:
  type: object
  properties:
    key: {type: string}
    value: {type: string}
  required: [key, value]
output_schema:
  type: object
permissions: []
timeout_ms: 1000
max_retries: 0
cache_ttl_secs: 0
idempotent: false
side_effects: writes fact
tags: [memory]
triggers: [remember]
---
## What it does
Writes facts.
## When to use
When user states stable fact.
## When NOT to use
For volatile data.
## Input notes
Key and value are required strings.
## Output notes
Returns success object.
## Failure handling
Validation failure on bad input.
## Examples
{"key":"city","value":"Montevideo"}
"#;
    std::fs::write(folder.join("SKILL.md"), content).expect("write skill");
}

fn write_timeout_skill(skills_root: &std::path::Path) {
    let folder = skills_root.join("sleepy_command");
    std::fs::create_dir_all(&folder).expect("skill folder");
    let script = folder.join("sleepy.sh");
    std::fs::write(&script, "#!/bin/sh\nsleep 2\necho '{\"ok\":true}'\n").expect("write script");

    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(&script).expect("meta").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&script, perms).expect("chmod");

    let content = r#"---
name: sleepy.command
version: 1.0.0
description: intentionally slow command
kind: command
entrypoint: sleepy.sh
input_schema:
  type: object
output_schema:
  type: object
permissions: []
timeout_ms: 100
max_retries: 0
cache_ttl_secs: 0
idempotent: true
side_effects: none
tags: [slow]
triggers: [sleep]
---
## What it does
Sleeps.
## When to use
Never in production.
## When NOT to use
Always.
## Input notes
No input.
## Output notes
Returns object.
## Failure handling
Times out.
## Examples
{}
"#;
    std::fs::write(folder.join("SKILL.md"), content).expect("write skill");
}
