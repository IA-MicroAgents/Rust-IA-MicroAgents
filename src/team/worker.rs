use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    identity::compiler::SystemIdentity,
    llm::{ChatMessage, LlmProvider, LlmRequest},
    orchestrator::context::TurnContext,
    planner::plan::PlanTask,
    team::subagent::Subagent,
};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TaskArtifact {
    pub task_id: String,
    pub summary: String,
    pub evidence: Vec<String>,
    pub output: String,
}

pub async fn execute_task(
    provider: &dyn LlmProvider,
    identity: &SystemIdentity,
    subagent: &Subagent,
    task: &PlanTask,
    turn_context: &TurnContext,
) -> Result<TaskArtifact, String> {
    let task_timeout_ms = task
        .max_latency_ms
        .max(2_500)
        .min(identity.frontmatter.budgets.timeout_ms)
        .min(6_000);
    let task_output_tokens =
        base_task_output_tokens(task).min(identity.frontmatter.budgets.max_output_tokens);
    let recent_turns = turn_context.recent_turns_block(6);
    let memories = turn_context.memories_block(3);
    let latest_summary = turn_context
        .latest_summary
        .clone()
        .unwrap_or_else(|| "(none)".to_string());
    let working_set = turn_context.working_set.render_for_prompt();
    let evidence_block = turn_context
        .current_evidence
        .as_ref()
        .map(|bundle| bundle.render_for_prompt())
        .unwrap_or_else(|| "(none)".to_string());
    // Paso 1: usar una instruccion corta y enfocada para bajar tokens y latencia por subtarea.
    let system = format!(
        "Idioma base: {}.\nRol: {}.\nObjetivo: resolver la tarea asignada con precision, brevedad y consistencia con el chat.\nReglas: no inventes datos externos ni detalles internos. Devuelve solo JSON estricto con task_id, summary, evidence[], output.",
        identity.frontmatter.locale, subagent.role
    );
    // Paso 2: entregar solo el contexto minimo util para no sobredimensionar cada llamada al modelo.
    let user = format!(
        "Task: {}\nDescription: {}\nAcceptance: {}\nAnalysis track: {}\nRequires live data: {}\nEvidence inputs: {}\n\nRecent conversation:\n{}\n\nLatest summary:\n{}\n\nRelevant memories:\n- {}\n\nConversation working set:\n{}\n\nEvidence bundle:\n{}",
        task.title,
        task.description,
        task.acceptance_criteria.join(" | "),
        task.analysis_track,
        task.requires_live_data,
        if task.evidence_inputs.is_empty() {
            "(none)".to_string()
        } else {
            task.evidence_inputs.join(", ")
        },
        if recent_turns.is_empty() { "(none)".to_string() } else { recent_turns },
        latest_summary,
        if memories.is_empty() { "(none)".to_string() } else { memories },
        working_set,
        evidence_block,
    );

    let response = provider
        .chat_completion(LlmRequest {
            model: if !task.resolved_model.is_empty() {
                task.resolved_model.clone()
            } else {
                task.model_route
                    .clone()
                    .unwrap_or_else(|| subagent.model_route.clone())
            },
            messages: vec![
                ChatMessage::text("system", system),
                ChatMessage::text("user", user),
            ],
            max_output_tokens: task_output_tokens,
            temperature: 0.2,
            require_json: true,
            timeout_ms: task_timeout_ms,
        })
        .await
        .map_err(|e| e.to_string())?;

    if let Ok(parsed) = serde_json::from_str::<TaskArtifact>(&response.content) {
        return Ok(parsed);
    }
    let raw_response_content = response.content.clone();

    // Paso 3: si el modelo ya devolvio texto util, preferimos degradar a artifact valido
    // antes que pagar otra llamada de reparacion que solo agrega latencia.
    if let Some(artifact) = best_effort_artifact_from_text(task, &raw_response_content) {
        return Ok(artifact);
    }

    // Paso 4: reparar una sola vez solo cuando el primer intento no sirvio ni como salida degradada.
    let repair = provider
        .chat_completion(LlmRequest {
            model: if !task.resolved_model.is_empty() {
                task.resolved_model.clone()
            } else {
                task.model_route
                    .clone()
                    .unwrap_or_else(|| subagent.model_route.clone())
            },
            messages: vec![
                ChatMessage::text(
                    "system",
                    "Repair malformed JSON. Return valid JSON object only with fields task_id, summary, evidence, output. evidence must be an array of strings.",
                ),
                ChatMessage::text("user", raw_response_content.clone()),
            ],
            max_output_tokens: 300,
            temperature: 0.0,
            require_json: true,
            timeout_ms: task_timeout_ms.min(2_000),
        })
        .await
        .map_err(|e| format!("task artifact repair failed: {e}"))?;

    if let Ok(parsed) = serde_json::from_str::<TaskArtifact>(&repair.content) {
        return Ok(parsed);
    }

    // Paso 5: si el repair tampoco devolvio JSON valido, intentar una ultima degradacion util
    // en lugar de perder toda la subtarea y obligar a mas retries.
    if let Some(artifact) = best_effort_artifact_from_text(task, &repair.content) {
        return Ok(artifact);
    }

    Err("task artifact parse failed: unable to salvage structured output".to_string())
}

fn base_task_output_tokens(task: &PlanTask) -> u32 {
    let complexity_bonus = if task.description.len() >= 180 || task.acceptance_criteria.len() >= 3 {
        80
    } else {
        0
    };
    let base = match task.route_key.as_str() {
        "reasoning" => 360,
        "tool_use" => 300,
        "reviewer_fast" | "reviewer_strict" => 240,
        _ => 220,
    };
    base + complexity_bonus
}

fn best_effort_artifact_from_text(task: &PlanTask, content: &str) -> Option<TaskArtifact> {
    let cleaned = content.trim();
    if cleaned.is_empty() || cleaned.len() < 48 {
        return None;
    }

    let evidence = cleaned
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(3)
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    let summary = cleaned
        .split_terminator(['.', '\n'])
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or(cleaned)
        .chars()
        .take(180)
        .collect::<String>();

    Some(TaskArtifact {
        task_id: task.id.clone(),
        summary,
        evidence,
        output: cleaned.to_string(),
    })
}
