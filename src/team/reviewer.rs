use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    identity::compiler::SystemIdentity,
    llm::broker::{
        InputModality, ModelBroker, ModelSelectionRequest, OutputModality, ReasoningLevel,
    },
    llm::{ChatMessage, LlmProvider, LlmRequest},
    orchestrator::context::TurnContext,
    planner::{acceptance::deterministic_acceptance_score, plan::PlanTask},
    team::worker::TaskArtifact,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReviewAction {
    Accept,
    RequestRevision,
    Retry,
    SplitTask,
    Reassign,
    EscalateToReasoningModel,
    FailTask,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TaskReview {
    pub action: ReviewAction,
    pub score: f64,
    pub notes: String,
}

pub async fn review_task(
    provider: &dyn LlmProvider,
    identity: &SystemIdentity,
    task: &PlanTask,
    artifact: &TaskArtifact,
    turn_context: &TurnContext,
) -> TaskReview {
    if task.id.ends_with(":task-integrate") && !artifact.output.trim().is_empty() {
        return TaskReview {
            action: ReviewAction::Accept,
            score: 0.9,
            notes: "integration artifact accepted deterministically".to_string(),
        };
    }

    let review_timeout_ms = task
        .max_latency_ms
        .max(2_000)
        .min(identity.frontmatter.budgets.timeout_ms)
        .min(4_000);
    let deterministic = deterministic_acceptance_score(
        &artifact.output,
        &task.acceptance_criteria,
        &task.description,
    );

    let deterministic_accept_threshold = match task.route_key.as_str() {
        "fast_text" => 0.58,
        "tool_use" => 0.6,
        "reviewer_fast" => 0.62,
        "reasoning" => 0.65,
        _ => 0.65,
    };

    if deterministic >= deterministic_accept_threshold {
        return TaskReview {
            action: ReviewAction::Accept,
            score: deterministic,
            notes: "deterministic acceptance passed".to_string(),
        };
    }

    if matches!(task.route_key.as_str(), "fast_text" | "reviewer_fast") && deterministic >= 0.45 {
        return TaskReview {
            action: ReviewAction::RequestRevision,
            score: deterministic,
            notes: "deterministic review requested revision".to_string(),
        };
    }

    let recent_turns = turn_context.recent_turns_block(8);
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
    let system = format!(
        "Idioma base: {}.\nRol: reviewer.\nEvalua si el artifact responde la intencion del usuario usando el contexto reciente. Devuelve solo JSON estricto con action, score, notes.",
        identity.frontmatter.locale
    );
    let user = format!(
        "Task: {}\nAcceptance: {}\nAnalysis track: {}\nRequires live data: {}\nRecent conversation:\n{}\n\nLatest summary:\n{}\n\nConversation working set:\n{}\n\nEvidence bundle:\n{}\n\nArtifact summary: {}\nArtifact output: {}",
        task.description,
        task.acceptance_criteria.join(" | "),
        task.analysis_track,
        task.requires_live_data,
        if recent_turns.is_empty() { "(none)".to_string() } else { recent_turns },
        latest_summary,
        working_set,
        evidence_block,
        artifact.summary,
        artifact.output
    );

    let response = provider
        .chat_completion(LlmRequest {
            model: ModelBroker::new(provider.model_catalog())
                .resolve(
                    &identity.frontmatter.model_routes,
                    ModelSelectionRequest {
                        route_key: if deterministic >= 0.5 {
                            "reviewer_fast"
                        } else {
                            "reviewer_strict"
                        },
                        input_modality: InputModality::Text,
                        output_modality: OutputModality::Json,
                        reasoning_level: if deterministic >= 0.5 {
                            ReasoningLevel::Medium
                        } else {
                            ReasoningLevel::High
                        },
                        requires_tools: false,
                        max_cost_usd: Some(0.02),
                        max_latency_ms: Some(review_timeout_ms),
                        performance_policy: turn_context.performance_policy.clone(),
                        escalation_tier: turn_context.max_escalation_tier.clone(),
                    },
                )
                .resolved_model,
            messages: vec![
                ChatMessage::text("system", system),
                ChatMessage::text("user", user),
            ],
            max_output_tokens: 180,
            temperature: 0.0,
            require_json: true,
            timeout_ms: review_timeout_ms,
        })
        .await;

    if let Ok(resp) = response {
        if let Ok(parsed) = serde_json::from_str::<TaskReview>(&resp.content) {
            return parsed;
        }
    }

    TaskReview {
        action: if deterministic >= 0.5 {
            ReviewAction::RequestRevision
        } else {
            ReviewAction::Retry
        },
        score: deterministic,
        notes: "fallback reviewer decision".to_string(),
    }
}
