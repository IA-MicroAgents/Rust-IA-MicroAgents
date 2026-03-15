use std::{collections::HashMap, sync::Arc};

use futures::stream::{FuturesUnordered, StreamExt};
use tokio::sync::Semaphore;
use tracing::info;

use crate::{
    execution::{artifacts::TaskExecutionResult, dispatcher::run_one_task},
    orchestrator::context::TurnContext,
    planner::{
        dag::ready_task_ids,
        plan::{ExecutionPlan, TaskState},
    },
    team::TeamManager,
    telemetry::event_bus::EventBus,
};

#[allow(clippy::too_many_arguments)]
pub async fn execute_plan_parallel(
    mut plan: ExecutionPlan,
    team: TeamManager,
    identity: crate::identity::compiler::SystemIdentity,
    llm: Arc<dyn crate::llm::LlmProvider>,
    turn_context: TurnContext,
    events: EventBus,
    max_parallel: usize,
    max_retries: u32,
    max_review_loops: u32,
) -> (ExecutionPlan, HashMap<String, TaskExecutionResult>) {
    let mut task_results = HashMap::new();
    let adaptive_parallel_limit = if should_burst_parallel(&plan) {
        max_parallel
            .max(
                team.persistent_count()
                    .saturating_add(team.effective_ephemeral_capacity()),
            )
            .max(1)
    } else {
        max_parallel.max(1)
    };
    let semaphore = Arc::new(Semaphore::new(adaptive_parallel_limit));

    loop {
        let ready_ids = ready_task_ids(&plan.tasks);
        if ready_ids.is_empty() {
            break;
        }
        info!(
            plan_id = %plan.id,
            width = ready_ids.len(),
            parallel_limit = adaptive_parallel_limit,
            task_ids = ?ready_ids,
            "scheduler starting parallel batch"
        );
        let _ = events.publish(
            "parallel_batch_started",
            serde_json::json!({
                "conversation_id": turn_context.conversation_id,
                "trace_id": turn_context.trace_id,
                "plan_id": plan.id,
                "task_ids": ready_ids,
                "width": ready_ids.len(),
                "parallel_limit": adaptive_parallel_limit,
            }),
        );

        let mut futures = FuturesUnordered::new();
        for task_id in ready_ids {
            let Some(index) = plan.tasks.iter().position(|t| t.id == task_id) else {
                continue;
            };

            let task = plan.tasks[index].clone();
            plan.tasks[index].state = TaskState::Running;
            let team_clone = team.clone();
            let identity_clone = identity.clone();
            let llm_clone = llm.clone();
            let turn_context_clone = turn_context.clone();
            let events_clone = events.clone();
            let sem = semaphore.clone();

            futures.push(tokio::spawn(async move {
                let permit = sem.acquire_owned().await.ok();
                let result = run_one_task(
                    team_clone,
                    identity_clone,
                    llm_clone,
                    task,
                    turn_context_clone,
                    events_clone,
                )
                .await;
                drop(permit);
                result
            }));
        }

        while let Some(joined) = futures.next().await {
            if let Ok(result) = joined {
                if let Some(task) = plan.tasks.iter_mut().find(|t| t.id == result.task_id) {
                    task.attempts = result.attempts;
                    if result.accepted {
                        task.state = TaskState::Accepted;
                    } else if task.attempts <= max_retries && task.review_loops < max_review_loops {
                        task.state = TaskState::Retrying;
                        task.review_loops = task.review_loops.saturating_add(1);
                    } else {
                        task.state = TaskState::Failed;
                    }
                }
                task_results.insert(result.task_id.clone(), result);
            }
        }

        let state_snapshot = plan
            .tasks
            .iter()
            .map(|t| (t.id.clone(), t.state.clone()))
            .collect::<HashMap<_, _>>();

        for task in &mut plan.tasks {
            if matches!(task.state, TaskState::Pending | TaskState::Retrying)
                && task.dependencies.iter().all(|dep| {
                    matches!(
                        state_snapshot.get(dep),
                        Some(TaskState::Accepted | TaskState::Completed)
                    )
                })
            {
                task.state = TaskState::Ready;
            }
        }
    }

    for task in &mut plan.tasks {
        if task.id.ends_with(":task-integrate") && matches!(task.state, TaskState::Accepted) {
            task.state = TaskState::Completed;
        }
    }

    (plan, task_results)
}

fn should_burst_parallel(plan: &ExecutionPlan) -> bool {
    let normalized_goal = plan.goal.to_lowercase();
    let explicit_burst = [
        "muchos subagentes",
        "varios subagentes",
        "subtareas paralelas",
        "parallel workstreams",
        "parallel workstream",
        "parallel tracks",
        "paralelas",
    ]
    .iter()
    .any(|needle| normalized_goal.contains(needle));
    explicit_burst && plan.tasks.len() >= 6
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;

    use crate::{
        identity::schema::{
            IdentityBudgets, IdentityChannels, IdentityFrontmatter, IdentityMemory,
            IdentityPermissions, ModelRoutes, TelegramIdentityChannel,
        },
        llm::{
            models::{ModelCapabilities, ModelMetadata},
            LlmProvider, LlmRequest, LlmResponse, ProviderResult, Usage,
        },
        orchestrator::context::TurnContext,
        planner::{decomposition::build_initial_plan, plan::TaskState},
        team::{manager::TeamManager, TeamConfig},
    };

    use super::execute_plan_parallel;

    #[derive(Clone)]
    struct StubProvider;

    #[async_trait]
    impl LlmProvider for StubProvider {
        async fn validate_models(
            &self,
            _routes: &ModelRoutes,
        ) -> ProviderResult<Vec<ModelCapabilities>> {
            Ok(vec![])
        }

        async fn chat_completion(&self, _request: LlmRequest) -> ProviderResult<LlmResponse> {
            Ok(LlmResponse {
                model: "stub".to_string(),
                content: "{\"task_id\":\"x\",\"summary\":\"ok\",\"evidence\":[\"e\"],\"output\":\"done\"}".to_string(),
                usage: Usage::default(),
                latency_ms: 1,
            })
        }

        fn model_catalog(&self) -> Vec<ModelMetadata> {
            vec![]
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn executes_ready_tasks_and_updates_states() {
        let identity = crate::identity::compiler::SystemIdentity {
            frontmatter: IdentityFrontmatter {
                id: "x".to_string(),
                display_name: "x".to_string(),
                description: "x".to_string(),
                locale: "en-US".to_string(),
                timezone: "UTC".to_string(),
                model_routes: ModelRoutes {
                    fast: "m".to_string(),
                    reasoning: "m".to_string(),
                    tool_use: "m".to_string(),
                    vision: "m".to_string(),
                    reviewer: "m".to_string(),
                    planner: "m".to_string(),
                    router_fast: None,
                    fast_text: None,
                    reviewer_fast: None,
                    reviewer_strict: None,
                    integrator_complex: None,
                    vision_understand: None,
                    audio_transcribe: None,
                    image_generate: None,
                    fallback: vec!["m".to_string()],
                },
                budgets: IdentityBudgets {
                    max_steps: 4,
                    max_turn_cost_usd: 1.0,
                    max_input_tokens: 1000,
                    max_output_tokens: 400,
                    max_tool_calls: 3,
                    timeout_ms: 10_000,
                },
                memory: IdentityMemory {
                    save_facts: true,
                    save_summaries: true,
                    summarize_every_n_turns: 10,
                },
                permissions: IdentityPermissions {
                    allowed_skills: vec!["*".to_string()],
                    denied_skills: vec![],
                },
                channels: IdentityChannels {
                    telegram: TelegramIdentityChannel {
                        enabled: true,
                        max_reply_chars: 3500,
                        style_overrides: "concise".to_string(),
                    },
                },
            },
            sections: crate::identity::compiler::CompiledIdentitySections {
                mission: "m".to_string(),
                persona: "p".to_string(),
                tone: "t".to_string(),
                hard_rules: "h".to_string(),
                do_not_do: "d".to_string(),
                escalation: "e".to_string(),
                memory_preferences: "m".to_string(),
                channel_notes: "c".to_string(),
                planning_principles: "p".to_string(),
                review_standards: "r".to_string(),
            },
            compiled_system_prompt: "sys".to_string(),
        };

        let team_cfg = TeamConfig {
            team_size: 2,
            max_parallel_tasks: 2,
            allow_ephemeral_subagents: true,
            max_ephemeral_subagents: 4,
            subagent_mode: crate::team::config::SubagentMode::Generalist,
            subagent_roleset: vec!["researcher".to_string(), "integrator".to_string()],
            subagent_profile_path: None,
            supervisor_review_interval_ms: 200,
            max_review_loops_per_task: 2,
            max_task_retries: 2,
            plan_max_tasks: 4,
            plan_max_depth: 2,
            performance_policy: crate::team::config::PerformancePolicy::BalancedFast,
            planner_aggressiveness: 60,
            max_escalation_tier: crate::team::config::EscalationTier::Standard,
            typing_delay_ms: 800,
            require_final_review: true,
            progress_updates_enabled: false,
            progress_update_threshold_ms: 1000,
        };
        let manager = TeamManager::new(team_cfg.clone(), &identity)
            .await
            .expect("manager");
        let plan = build_initial_plan(
            1,
            "research this and integrate",
            &team_cfg,
            &team_cfg.subagent_roleset,
            &identity,
            &crate::team::config::TeamRuntimeSettings::from_bootstrap(&team_cfg, &identity),
            &[],
        );
        let (updated, _results) = execute_plan_parallel(
            plan,
            manager,
            identity,
            Arc::new(StubProvider),
            TurnContext {
                conversation_id: 1,
                trace_id: "trace-test".to_string(),
                recent_turns: Vec::new(),
                latest_summary: None,
                memories: Vec::new(),
                working_set: crate::usecase::ConversationWorkingSet::default(),
                current_evidence: None,
                selected_skills: Vec::new(),
                performance_policy: crate::team::config::PerformancePolicy::BalancedFast,
                max_escalation_tier: crate::team::config::EscalationTier::Standard,
                analysis_complexity: crate::usecase::AnalysisComplexity::Simple,
            },
            crate::telemetry::event_bus::EventBus::ephemeral(),
            2,
            2,
            2,
        )
        .await;

        assert!(updated.tasks.iter().any(|t| {
            matches!(
                t.state,
                TaskState::Accepted | TaskState::Completed | TaskState::Failed
            )
        }));
    }
}
