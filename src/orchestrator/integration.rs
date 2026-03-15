use std::collections::HashMap;

use crate::{
    execution::artifacts::TaskExecutionResult,
    identity::compiler::SystemIdentity,
    llm::{
        broker::{
            InputModality, ModelBroker, ModelSelectionRequest, OutputModality, ReasoningLevel,
        },
        ChatMessage, LlmProvider, LlmRequest,
    },
    orchestrator::context::TurnContext,
};

pub async fn integrate_artifacts(
    provider: &dyn LlmProvider,
    identity: &SystemIdentity,
    user_goal: &str,
    task_results: &HashMap<String, TaskExecutionResult>,
    turn_context: &TurnContext,
) -> String {
    let accepted = task_results
        .values()
        .filter(|r| r.accepted)
        .filter_map(|r| r.artifact.as_ref())
        .map(|a| format!("- {}: {}", a.task_id, a.output))
        .collect::<Vec<_>>();

    let provisional = task_results
        .values()
        .filter_map(|result| {
            result.artifact.as_ref().map(|artifact| {
                (
                    result
                        .review
                        .as_ref()
                        .map(|review| review.score)
                        .unwrap_or(0.0),
                    artifact,
                )
            })
        })
        .filter(|(_, artifact)| !artifact.output.trim().is_empty())
        .collect::<Vec<_>>();
    let selected_artifacts = if accepted.is_empty() {
        provisional
            .iter()
            .filter(|(score, _)| *score >= 0.45)
            .map(|(_, artifact)| format!("- {}: {}", artifact.task_id, artifact.output))
            .collect::<Vec<_>>()
    } else {
        accepted
    };

    if selected_artifacts.is_empty() {
        return "I could not complete the request with sufficient quality. Please clarify and retry.".to_string();
    }

    let recent_turns = turn_context.recent_turns_block(8);
    let latest_summary = turn_context
        .latest_summary
        .clone()
        .unwrap_or_else(|| "(none)".to_string());
    let memories = turn_context.memories_block(4);
    let working_set = turn_context.working_set.render_for_prompt();
    let evidence_block = turn_context
        .current_evidence
        .as_ref()
        .map(|bundle| bundle.render_for_prompt())
        .unwrap_or_else(|| "(none)".to_string());
    let integration_timeout_ms = (4_000 + (selected_artifacts.len() as u64 * 250))
        .max(3_500)
        .min(identity.frontmatter.budgets.timeout_ms)
        .min(8_000);
    let strict_reasoning = requires_strict_reasoning(user_goal);
    let system = format!(
        "Idioma base: {}.\nRol: integrador final.\nObjetivo: producir una respuesta final clara, correcta y util para Telegram usando solo los artifacts aceptados y el contexto reciente.\nReglas: no inventes datos externos, no muestres protocolo interno, responde solo texto plano.",
        identity.frontmatter.locale
    );
    let user = format!(
        "Original request: {}\n\nRecent conversation:\n{}\n\nLatest summary:\n{}\n\nRelevant memories:\n- {}\n\nConversation working set:\n{}\n\nEvidence bundle:\n{}\n\nAccepted artifacts:\n{}",
        user_goal,
        if recent_turns.is_empty() { "(none)".to_string() } else { recent_turns },
        latest_summary,
        if memories.is_empty() { "(none)".to_string() } else { memories },
        working_set,
        evidence_block,
        selected_artifacts.join("\n")
    );

    let response = provider
        .chat_completion(LlmRequest {
            model: ModelBroker::new(provider.model_catalog())
                .resolve(
                    &identity.frontmatter.model_routes,
                    ModelSelectionRequest {
                        route_key: if strict_reasoning {
                            "integrator_complex"
                        } else {
                            "fast_text"
                        },
                        input_modality: InputModality::Text,
                        output_modality: OutputModality::Text,
                        reasoning_level: if strict_reasoning {
                            ReasoningLevel::High
                        } else {
                            ReasoningLevel::Medium
                        },
                        requires_tools: false,
                        max_cost_usd: Some(
                            identity.frontmatter.budgets.max_turn_cost_usd.min(0.03),
                        ),
                        max_latency_ms: Some(integration_timeout_ms),
                        performance_policy: turn_context.performance_policy.clone(),
                        escalation_tier: turn_context.max_escalation_tier.clone(),
                    },
                )
                .resolved_model,
            messages: vec![
                ChatMessage::text("system", system),
                ChatMessage::text("user", user),
            ],
            max_output_tokens: identity.frontmatter.budgets.max_output_tokens.min(420),
            temperature: 0.2,
            require_json: false,
            timeout_ms: integration_timeout_ms,
        })
        .await;

    match response {
        Ok(resp) => resp.content,
        Err(_) => selected_artifacts.join("\n"),
    }
}

fn requires_strict_reasoning(goal: &str) -> bool {
    let normalized = goal.to_lowercase();
    [
        "btc",
        "bitcoin",
        "eth",
        "ethereum",
        "solana",
        "forecast",
        "predic",
        "trading",
        "mercado",
        "market",
        "macro",
        "probabilidad",
        "scenario",
        "escenario",
        "thesis",
        "tesis",
        "al dia de hoy",
        "al día de hoy",
        "ranking",
        "compara",
        "comparacion",
        "comparación",
        "recommend",
        "recomend",
    ]
    .iter()
    .any(|needle| normalized.contains(needle))
}
