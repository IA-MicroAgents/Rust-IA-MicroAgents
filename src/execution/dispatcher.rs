use std::time::Instant;
use tracing::{info, warn};

use crate::{
    execution::artifacts::TaskExecutionResult,
    orchestrator::context::TurnContext,
    planner::plan::PlanTask,
    team::{
        reviewer::{review_task, ReviewAction},
        worker::execute_task,
        TeamManager,
    },
    telemetry::event_bus::EventBus,
};

pub async fn run_one_task(
    team: TeamManager,
    identity: crate::identity::compiler::SystemIdentity,
    llm: std::sync::Arc<dyn crate::llm::LlmProvider>,
    task: PlanTask,
    turn_context: TurnContext,
    events: EventBus,
) -> TaskExecutionResult {
    let started = Instant::now();

    let Some(acquired) = team.acquire_for_task(&task.id, task.candidate_role.as_deref()) else {
        warn!(task_id = %task.id, role = ?task.candidate_role, "no subagent available for task");
        return TaskExecutionResult {
            task_id: task.id,
            subagent_id: "none".to_string(),
            subagent_role: "none".to_string(),
            ephemeral: false,
            destroyed_on_release: false,
            artifact: None,
            review: None,
            accepted: false,
            error: Some("no subagent available".to_string()),
            duration_ms: 0,
            attempts: task.attempts,
        };
    };
    let subagent = acquired.subagent;
    info!(
        task_id = %task.id,
        title = %task.title,
        subagent_id = %subagent.id,
        role = %subagent.role,
        route_key = %task.route_key,
        resolved_model = %task.resolved_model,
        ephemeral = subagent.ephemeral,
        spawned = acquired.spawned,
        "task assigned to subagent"
    );
    if acquired.spawned {
        let _ = events.publish(
            "subagent_spawned",
            serde_json::json!({
                "conversation_id": turn_context.conversation_id,
                "trace_id": turn_context.trace_id,
                "subagent_id": subagent.id,
                "role": subagent.role,
                "ephemeral": true,
                "task_id": task.id,
                "reason": "elastic_parallelism"
            }),
        );
    }
    let _ = events.publish(
        "task_assigned",
        serde_json::json!({
            "conversation_id": turn_context.conversation_id,
            "trace_id": turn_context.trace_id,
            "task_id": task.id,
            "title": task.title,
            "description": preview_text(&task.description, 180),
            "prompt_preview": preview_text(
                if task.description.trim().is_empty() {
                    &task.title
                } else {
                    &task.description
                },
                180
            ),
            "subagent_id": subagent.id,
            "role": subagent.role,
            "route_key": task.route_key,
            "model_route": task.model_route.clone().unwrap_or_else(|| subagent.model_route.clone()),
            "resolved_model": if task.resolved_model.is_empty() {
                task.model_route.clone().unwrap_or_else(|| subagent.model_route.clone())
            } else {
                task.resolved_model.clone()
            },
            "ephemeral": subagent.ephemeral,
            "dependencies": task.dependencies,
        }),
    );

    team.mark_running(&subagent.id);
    let _ = events.publish(
        "task_started",
        serde_json::json!({
            "conversation_id": turn_context.conversation_id,
            "trace_id": turn_context.trace_id,
            "task_id": task.id,
            "title": task.title,
            "description": preview_text(&task.description, 180),
            "prompt_preview": preview_text(
                if task.description.trim().is_empty() {
                    &task.title
                } else {
                    &task.description
                },
                180
            ),
            "subagent_id": subagent.id,
            "role": subagent.role,
            "route_key": task.route_key,
            "model_route": task.model_route.clone().unwrap_or_else(|| subagent.model_route.clone()),
            "resolved_model": if task.resolved_model.is_empty() {
                task.model_route.clone().unwrap_or_else(|| subagent.model_route.clone())
            } else {
                task.resolved_model.clone()
            },
            "ephemeral": subagent.ephemeral,
        }),
    );
    let artifact_result =
        execute_task(llm.as_ref(), &identity, &subagent, &task, &turn_context).await;

    match artifact_result {
        Ok(artifact) => {
            let _ = events.publish(
                "task_artifact_submitted",
                serde_json::json!({
                    "conversation_id": turn_context.conversation_id,
                    "trace_id": turn_context.trace_id,
                    "task_id": task.id,
                    "subagent_id": subagent.id,
                    "role": subagent.role,
                    "artifact_summary": preview_text(&artifact.summary, 180),
                    "artifact_output_preview": preview_text(&artifact.output, 220),
                    "evidence_count": artifact.evidence.len(),
                    "ephemeral": subagent.ephemeral,
                }),
            );
            let review =
                review_task(llm.as_ref(), &identity, &task, &artifact, &turn_context).await;
            let accepted = matches!(review.action, ReviewAction::Accept);
            info!(
                task_id = %task.id,
                subagent_id = %subagent.id,
                review_action = ?review.action,
                score = review.score,
                accepted,
                "task reviewed"
            );
            let release = team.release(
                &subagent.id,
                review.score,
                if accepted {
                    None
                } else {
                    Some(review.notes.clone())
                },
            );
            let _ = events.publish(
                "task_reviewed",
                serde_json::json!({
                    "conversation_id": turn_context.conversation_id,
                    "trace_id": turn_context.trace_id,
                    "task_id": task.id,
                    "subagent_id": subagent.id,
                    "role": subagent.role,
                    "score": review.score,
                    "action": format!("{:?}", review.action),
                    "notes": preview_text(&review.notes, 180),
                    "ephemeral": subagent.ephemeral,
                }),
            );
            let _ = events.publish(
                if accepted {
                    "task_accepted"
                } else {
                    "task_rejected"
                },
                serde_json::json!({
                    "conversation_id": turn_context.conversation_id,
                    "trace_id": turn_context.trace_id,
                    "task_id": task.id,
                    "subagent_id": subagent.id,
                    "role": subagent.role,
                    "score": review.score,
                    "artifact_summary": preview_text(&artifact.summary, 180),
                    "review_notes": preview_text(&review.notes, 180),
                    "ephemeral": subagent.ephemeral,
                }),
            );
            if let Some(release) = release {
                if release.destroyed {
                    let _ = events.publish(
                        "subagent_destroyed",
                        serde_json::json!({
                            "conversation_id": turn_context.conversation_id,
                            "trace_id": turn_context.trace_id,
                            "subagent_id": release.subagent.id,
                            "role": release.subagent.role,
                            "task_id": task.id,
                            "ephemeral": true,
                        }),
                    );
                }
            }
            TaskExecutionResult {
                task_id: task.id,
                subagent_id: subagent.id,
                subagent_role: subagent.role,
                ephemeral: subagent.ephemeral,
                destroyed_on_release: acquired.spawned,
                artifact: Some(artifact),
                review: Some(review),
                accepted,
                error: None,
                duration_ms: started.elapsed().as_millis() as u64,
                attempts: task.attempts.saturating_add(1),
            }
        }
        Err(err) => {
            warn!(
                task_id = %task.id,
                subagent_id = %subagent.id,
                error = %err,
                "task execution failed"
            );
            let release = team.release(&subagent.id, 0.0, Some(err.clone()));
            let _ = events.publish(
                "task_failed",
                serde_json::json!({
                    "conversation_id": turn_context.conversation_id,
                    "trace_id": turn_context.trace_id,
                    "task_id": task.id,
                    "subagent_id": subagent.id,
                    "role": subagent.role,
                    "error": err,
                    "ephemeral": subagent.ephemeral,
                }),
            );
            if let Some(release) = release {
                if release.destroyed {
                    let _ = events.publish(
                        "subagent_destroyed",
                        serde_json::json!({
                            "conversation_id": turn_context.conversation_id,
                            "trace_id": turn_context.trace_id,
                            "subagent_id": release.subagent.id,
                            "role": release.subagent.role,
                            "task_id": task.id,
                            "ephemeral": true,
                        }),
                    );
                }
            }
            TaskExecutionResult {
                task_id: task.id,
                subagent_id: subagent.id,
                subagent_role: subagent.role,
                ephemeral: subagent.ephemeral,
                destroyed_on_release: acquired.spawned,
                artifact: None,
                review: None,
                accepted: false,
                error: Some(err),
                duration_ms: started.elapsed().as_millis() as u64,
                attempts: task.attempts.saturating_add(1),
            }
        }
    }
}

fn preview_text(input: &str, max_chars: usize) -> String {
    let collapsed = input.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    let mut count = 0usize;
    for ch in trimmed.chars() {
        if count >= max_chars {
            out.push('…');
            return out;
        }
        out.push(ch);
        count += 1;
    }
    out
}
