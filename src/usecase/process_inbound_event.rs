use std::{sync::Arc, time::Instant};

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use chrono::Utc;
use metrics::{counter, histogram};
use serde_json::json;
use tracing::{debug, error, info, warn};

use crate::team::config::PerformancePolicy;
use crate::{
    channel::{
        telegram::TelegramClient, InboundAttachment, InboundAttachmentKind, InboundKind,
        NormalizedInboundEvent, OutboundSendResult,
    },
    config::AppConfig,
    errors::{AppError, AppResult},
    identity::IdentityManager,
    llm::{
        broker::{
            InputModality, ModelBroker, ModelSelectionRequest, OutputModality, ReasoningLevel,
        },
        response_types::{DecisionRoute, OrchestrationDecision, ToolCall},
        ChatMessage, ChatMessagePart, LlmProvider, LlmRequest, ProviderError,
        OPENROUTER_FREE_MODEL,
    },
    memory::{BrainMemory, DeterministicSummarizer, MemoryStore},
    orchestrator::{
        context::{ContextBuildRequest, ContextBuilder, TurnContext},
        prompt_compiler::{
            compile_classifier_prompt, compile_decision_prompt, compile_fast_reply_prompt,
            compile_final_answer_prompt, compile_planning_prompt,
        },
        response_parser::{parse_or_repair_decision, parse_or_repair_execution_plan},
        router::pick_route_hint,
    },
    out::research::ResearchGateway,
    planner::decomposition::{build_initial_plan, build_plan_from_contract},
    policy::{budgets::TurnBudget, permissions},
    skills::{SkillCall, SkillRegistry, SkillRunner, SkillSelector},
    storage::{OutboundMessageInsert, Store, TaskAttemptInsert, TaskReviewInsert},
    team::{supervisor::SupervisorControls, TeamManager},
    telemetry::{event_bus::EventBus, tracing_ids},
    usecase::{
        classify_analysis_complexity, detect_current_data_requirement, AnalysisComplexity,
        CaptureBrainMemoryRequest, CaptureBrainMemoryUseCase, CurrentDataIntent,
        CurrentDataRequirement, EvidenceBundle, EvidenceItem, RetrieveBrainMemoryRequest,
        RetrieveBrainMemoryUseCase,
    },
};
use tokio::sync::oneshot;

#[derive(Debug, Clone)]
pub struct TurnOutcome {
    pub trace_id: String,
    pub route: DecisionRoute,
    pub reply: Option<String>,
    pub sent_chunks: usize,
}

#[derive(Clone)]
pub struct ProcessInboundEventUseCase {
    config: AppConfig,
    store: Store,
    identity: IdentityManager,
    context_builder: ContextBuilder,
    llm: Arc<dyn LlmProvider>,
    skill_runner: SkillRunner,
    research: ResearchGateway,
    telegram: TelegramClient,
    summarizer: DeterministicSummarizer,
    brain_capture: CaptureBrainMemoryUseCase,
    brain_retrieval: RetrieveBrainMemoryUseCase,
    team: TeamManager,
    controls: SupervisorControls,
    events: EventBus,
}

impl ProcessInboundEventUseCase {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: AppConfig,
        store: Store,
        identity: IdentityManager,
        registry: SkillRegistry,
        llm: Arc<dyn LlmProvider>,
        telegram: TelegramClient,
        team: TeamManager,
        controls: SupervisorControls,
        events: EventBus,
    ) -> AppResult<Self> {
        let memory = MemoryStore::new(store.clone());
        let context_builder = ContextBuilder::new(memory.clone(), registry.clone(), SkillSelector);
        let skill_runner = SkillRunner::new(
            registry,
            store.clone(),
            config.policy.http_skill_allowlist.clone(),
        )?;
        let research = ResearchGateway::new(skill_runner.clone());
        let brain_capture = CaptureBrainMemoryUseCase::new();
        let brain_retrieval = RetrieveBrainMemoryUseCase::new(memory.clone());

        Ok(Self {
            config,
            store,
            identity,
            context_builder,
            llm,
            skill_runner,
            research,
            telegram,
            summarizer: DeterministicSummarizer,
            brain_capture,
            brain_retrieval,
            team,
            controls,
            events,
        })
    }

    pub async fn process_inbound_event(
        &self,
        event: NormalizedInboundEvent,
    ) -> AppResult<TurnOutcome> {
        self.execute(event).await
    }

    pub async fn execute(&self, event: NormalizedInboundEvent) -> AppResult<TurnOutcome> {
        // Paso 1: iniciar la traza del turno y tomar las métricas base antes de tocar negocio.
        let turn_started = Instant::now();
        let ingest_started = Instant::now();
        let trace_id = tracing_ids::new_trace_id();
        info!(
            trace_id = %trace_id,
            event_id = %event.event_id,
            channel = %event.channel,
            user_id = %event.user_id,
            preview = %truncate_for_log(&event.text, 120),
            "processing inbound event"
        );
        counter!("ai_microagents_inbound_events_total", "channel" => event.channel.clone())
            .increment(1);
        let queue_wait_ms = event
            .queued_at
            .map(|queued_at| (Utc::now() - queued_at).num_milliseconds().max(0) as f64)
            .unwrap_or(0.0);
        if queue_wait_ms > 0.0 {
            histogram!("ai_microagents_queue_wait_ms").record(queue_wait_ms);
        }

        // Paso 2: deduplicar el evento para garantizar que el caso de uso sea idempotente.
        if !self.store.dedupe_processed_event(&event.event_id).await? {
            counter!("ai_microagents_duplicate_events_total").increment(1);
            info!(event_id = %event.event_id, "duplicate event skipped");
            return Ok(TurnOutcome {
                trace_id,
                route: DecisionRoute::Ignore,
                reply: None,
                sent_chunks: 0,
            });
        }

        // Paso 3: cortar temprano los eventos que solo informan estado del canal.
        if event.kind == InboundKind::MessageStatus {
            self.store.mark_inbound_processed(&event.event_id).await?;
            return Ok(TurnOutcome {
                trace_id,
                route: DecisionRoute::Ignore,
                reply: None,
                sent_chunks: 0,
            });
        }

        // Paso 4: cargar identidad, permisos y enriquecer la entrada con contexto multimodal.
        let identity = self.identity.get();
        let team_settings = self.team.runtime_settings();
        let team_config = team_settings.as_team_config();
        let principal_permissions = self.team.effective_principal_permissions();
        let enriched_user_text = self
            .enrich_inbound_text(&identity, &event, &trace_id)
            .await
            .unwrap_or_else(|err| {
                warn!(trace_id = %trace_id, error = %err, "media enrichment failed; using raw text");
                event.text.clone()
            });
        let analysis_complexity = classify_analysis_complexity(&enriched_user_text);
        let current_data_requirement = detect_current_data_requirement(&enriched_user_text);
        let effective_performance_policy = effective_turn_performance_policy(
            &team_settings.performance_policy,
            &analysis_complexity,
            current_data_requirement.required,
        );
        let raw_fast_path = if current_data_requirement.required {
            None
        } else {
            fast_path_decision(&enriched_user_text, &principal_permissions)
        };
        // Paso 5: resolver la conversación y recuperar el contexto reciente necesario.
        let conversation_id = self
            .store
            .upsert_conversation(&event.channel, &event.conversation_external_id)
            .await?;
        let prior_turns = if raw_fast_path.is_some() {
            Vec::new()
        } else {
            self.store.recent_turns(conversation_id, 6).await?
        };
        let resolved_user_text = enriched_user_text.clone();
        let contextual_follow_up = looks_like_follow_up(&enriched_user_text)
            || looks_like_conversational_correction(&enriched_user_text.to_lowercase());
        let contextual_synthesis_request = looks_like_contextual_synthesis(&enriched_user_text);
        let topic_shift_detected = !contextual_follow_up
            && !contextual_synthesis_request
            && looks_like_topic_shift(&resolved_user_text, &prior_turns);
        let effective_prior_turns = if topic_shift_detected {
            Vec::new()
        } else {
            prior_turns.clone()
        };
        // Paso 5.1: cuando la consulta depende de actualidad o URLs, reunir evidencia externa antes de decidir.
        let live_evidence = if current_data_requirement.required {
            self.publish_turn_stage(
                conversation_id,
                &trace_id,
                "evidence_collect",
                &resolved_user_text,
                json!({
                    "requirement": current_data_requirement.reason,
                    "intent": format!("{:?}", current_data_requirement.intent),
                }),
            );
            match self
                .collect_live_evidence(
                    &identity,
                    conversation_id,
                    &event.user_id,
                    &trace_id,
                    &current_data_requirement,
                )
                .await
            {
                Ok(bundle) if !bundle.is_empty() => {
                    let _ = self.events.publish(
                        "live_evidence_collected",
                        json!({
                            "conversation_id": conversation_id,
                            "trace_id": trace_id,
                            "evidence_count": bundle.evidence_count(),
                            "reasoning_tier": reasoning_tier_label(&analysis_complexity),
                            "intent": format!("{:?}", current_data_requirement.intent),
                        }),
                    );
                    Some(bundle)
                }
                Ok(_) | Err(_) => {
                    return self
                        .safe_abort(
                            &event,
                            conversation_id,
                            &trace_id,
                            DecisionRoute::AskClarification,
                            "Necesito evidencia externa válida para responder esa consulta actual. Reintenta en unos segundos o pásame una URL/fuente permitida.",
                        )
                        .await;
                }
            }
        } else {
            None
        };
        self.publish_turn_stage(
            conversation_id,
            &trace_id,
            "ingest",
            &resolved_user_text,
            json!({
                "event_id": event.event_id,
                "queue_wait_ms": queue_wait_ms,
                "evidence_count": live_evidence.as_ref().map(|bundle| bundle.evidence_count()).unwrap_or(0),
                "reasoning_tier": reasoning_tier_label(&analysis_complexity),
            }),
        );

        let deferred_user_turn_content = Some(
            if !event.attachments.is_empty() || event.text.trim().is_empty() {
                enriched_user_text.clone()
            } else {
                event.text.clone()
            },
        );
        let prechecked_brain_memories = self
            .precheck_brain_memories(
                &identity,
                conversation_id,
                &event.user_id,
                &resolved_user_text,
                topic_shift_detected,
            )
            .await;
        histogram!("ai_microagents_turn_ingest_seconds")
            .record(ingest_started.elapsed().as_secs_f64());

        // Paso 6: preparar presupuesto y decidir si conviene planificar o responder directo.
        let mut budget = TurnBudget::from_identity(identity.budgets());
        let planning_decision = planning_decision(
            &enriched_user_text,
            team_settings.planner_aggressiveness,
            &effective_performance_policy,
        );
        let route_hint = planning_decision.route_hint.clone();
        debug!(
            trace_id = %trace_id,
            team_size = team_config.team_size,
            max_parallel = team_config.max_parallel_tasks,
            allowed_skills = principal_permissions.allowed_skills.len(),
            planning_reason = planning_decision.reason,
            follow_up_detected = contextual_follow_up,
            topic_shift_detected = topic_shift_detected,
            "turn context bootstrap ready"
        );
        if topic_shift_detected {
            let _ = self.events.publish(
                "conversation_topic_shift_detected",
                json!({
                    "conversation_id": conversation_id,
                    "trace_id": trace_id,
                    "preview": truncate_for_log(&resolved_user_text, 120),
                }),
            );
        }
        let _ = self.events.publish(
            "supervisor_started",
            json!({
                "conversation_id": conversation_id,
                "event_id": event.event_id,
                "trace_id": trace_id,
                "preview": truncate_for_log(&enriched_user_text, 120),
                "route_hint": format!("{route_hint:?}"),
            }),
        );
        if !budget.consume_step() {
            return self
                .safe_abort(
                    &event,
                    conversation_id,
                    &trace_id,
                    DecisionRoute::AskClarification,
                    "I need a shorter request to stay within budget.",
                )
                .await;
        }

        let mut preplanned = None;
        let mut shared_context: Option<TurnContext> = None;
        let mut typing_notifier = None;
        let deterministic_tool_decision = if live_evidence.is_none() {
            deterministic_market_tool_decision(
                &resolved_user_text,
                &principal_permissions,
                &self.config.policy.http_skill_allowlist,
            )
        } else {
            None
        };
        // Paso 7: elegir la estrategia del turno: fast-path, tools, respuesta contextual o flujo completo.
        let decision = if let Some((route, assistant_reply, strategy)) = raw_fast_path.clone() {
            let _ = self.events.publish(
                "supervisor_fast_path",
                json!({
                    "conversation_id": conversation_id,
                    "trace_id": trace_id,
                    "route": format!("{route:?}"),
                    "strategy": strategy,
                    "preview": truncate_for_log(&assistant_reply, 120),
                }),
            );
            OrchestrationDecision {
                route,
                assistant_reply,
                tool_calls: Vec::new(),
                memory_writes: Vec::new(),
                should_summarize: false,
                confidence: 0.98,
                safe_to_send: true,
            }
        } else if let Some(tool_decision) = deterministic_tool_decision {
            self.publish_turn_stage(
                conversation_id,
                &trace_id,
                "execute",
                &resolved_user_text,
                json!({
                    "route": "tool_use",
                    "strategy": "deterministic_market_data",
                }),
            );
            let _ = self.events.publish(
                "supervisor_tooling",
                json!({
                    "conversation_id": conversation_id,
                    "trace_id": trace_id,
                    "tool_calls": tool_decision.tool_calls.len(),
                    "strategy": "deterministic_market_data",
                }),
            );
            tool_decision
        } else if live_evidence.is_none()
            && !planning_decision.should_plan
            && (contextual_follow_up
                || planning_decision.reason == "simple_comparison"
                || planning_decision.reason == "contextual_synthesis")
        {
            typing_notifier = self.spawn_typing_notifier(&identity, &event.channel, &event.user_id);
            self.publish_turn_stage(
                conversation_id,
                &trace_id,
                "classify",
                &resolved_user_text,
                json!({
                    "reason": if contextual_follow_up {
                        "contextual_follow_up_fast_reply"
                    } else if planning_decision.reason == "contextual_synthesis" {
                        "contextual_synthesis_fast_reply"
                    } else {
                        "simple_comparison_fast_reply"
                    },
                    "route_hint": format!("{:?}", route_hint),
                }),
            );
            match self
                .generate_fast_reply(
                    &identity,
                    conversation_id,
                    &trace_id,
                    route_hint.clone(),
                    &resolved_user_text,
                    &effective_prior_turns,
                    &prechecked_brain_memories,
                )
                .await
            {
                Ok((assistant_reply, usage, model, latency_ms)) => {
                    let _ = budget.add_cost(usage.estimated_cost_usd);
                    let _ = self
                        .store
                        .insert_model_usage(
                            trace_id.as_str(),
                            &model,
                            usage.prompt_tokens,
                            usage.completion_tokens,
                            usage.estimated_cost_usd,
                            latency_ms,
                        )
                        .await;
                    OrchestrationDecision {
                        route: route_hint,
                        assistant_reply,
                        tool_calls: Vec::new(),
                        memory_writes: Vec::new(),
                        should_summarize: false,
                        confidence: 0.88,
                        safe_to_send: true,
                    }
                }
                Err(err) => {
                    warn!(error = %err, "contextual follow-up fast reply failed; falling back to classifier");
                    let context = self
                        .ensure_turn_context(
                            &mut shared_context,
                            conversation_id,
                            &event.user_id,
                            &resolved_user_text,
                            &principal_permissions.allowed_skills,
                            &principal_permissions.denied_skills,
                            &trace_id,
                            topic_shift_detected,
                            effective_performance_policy.clone(),
                            analysis_complexity.clone(),
                            live_evidence.as_ref(),
                        )
                        .await?;
                    self.request_decision(
                        &identity,
                        conversation_id,
                        &context,
                        route_hint.clone(),
                        &resolved_user_text,
                        &trace_id,
                    )
                    .await
                    .unwrap_or_else(|decision_err| {
                        warn!(error = %decision_err, "decision request failed; using fallback");
                        OrchestrationDecision::safe_fallback(
                            "I hit a temporary issue. Please rephrase briefly and retry.",
                        )
                    })
                }
            }
        } else {
            let _ = self.events.publish(
                "supervisor_thinking",
                json!({
                    "conversation_id": conversation_id,
                    "trace_id": trace_id,
                    "preview": truncate_for_log(&resolved_user_text, 120),
                }),
            );
            typing_notifier = self.spawn_typing_notifier(&identity, &event.channel, &event.user_id);

            if planning_decision.should_plan {
                self.publish_turn_stage(
                    conversation_id,
                    &trace_id,
                    "plan_if_needed",
                    &resolved_user_text,
                    json!({
                        "reason": planning_decision.reason,
                        "route_hint": format!("{:?}", planning_decision.route_hint),
                    }),
                );
                info!(
                    trace_id = %trace_id,
                    reason = planning_decision.reason,
                    deterministic = planning_decision.prefer_deterministic_plan,
                    route_hint = ?planning_decision.route_hint,
                    "supervisor planning selected"
                );
                let _ = self.events.publish(
                    "supervisor_planning",
                    json!({
                        "conversation_id": conversation_id,
                        "trace_id": trace_id,
                        "preview": truncate_for_log(&resolved_user_text, 120),
                        "reason": planning_decision.reason,
                        "deterministic": planning_decision.prefer_deterministic_plan,
                    }),
                );
                if planning_decision.prefer_deterministic_plan {
                    let _ = self.events.publish(
                        "supervisor_planning_local",
                        json!({
                            "conversation_id": conversation_id,
                            "trace_id": trace_id,
                            "reason": planning_decision.reason,
                        }),
                    );
                    preplanned = Some(build_initial_plan(
                        conversation_id,
                        &resolved_user_text,
                        &team_config,
                        &team_settings.subagent_roleset,
                        &identity,
                        &team_settings,
                        &self.llm.model_catalog(),
                    ));
                    OrchestrationDecision {
                        route: DecisionRoute::PlanThenAct,
                        assistant_reply: String::new(),
                        tool_calls: Vec::new(),
                        memory_writes: Vec::new(),
                        should_summarize: false,
                        confidence: 0.92,
                        safe_to_send: true,
                    }
                } else {
                    let context = self
                        .ensure_turn_context(
                            &mut shared_context,
                            conversation_id,
                            &event.user_id,
                            &resolved_user_text,
                            &principal_permissions.allowed_skills,
                            &principal_permissions.denied_skills,
                            &trace_id,
                            topic_shift_detected,
                            effective_performance_policy.clone(),
                            analysis_complexity.clone(),
                            live_evidence.as_ref(),
                        )
                        .await?;
                    match self
                        .request_plan(
                            &identity,
                            &context,
                            &team_config,
                            &team_settings,
                            conversation_id,
                            &resolved_user_text,
                            &trace_id,
                        )
                        .await
                    {
                        Ok((plan, usage)) => {
                            if !budget.add_cost(usage.estimated_cost_usd) {
                                warn!(
                                    trace_id = %trace_id,
                                    estimated_cost_usd = usage.estimated_cost_usd,
                                    used_cost_usd = budget.used_cost_usd,
                                    max_turn_cost_usd = budget.max_turn_cost_usd,
                                    "planner exceeded budget; using deterministic fallback plan"
                                );
                                let _ = self.events.publish(
                                    "supervisor_planning_budget_fallback",
                                    json!({
                                        "conversation_id": conversation_id,
                                        "trace_id": trace_id,
                                        "estimated_cost_usd": usage.estimated_cost_usd,
                                        "used_cost_usd": budget.used_cost_usd,
                                        "max_turn_cost_usd": budget.max_turn_cost_usd,
                                    }),
                                );
                                preplanned = Some(build_initial_plan(
                                    conversation_id,
                                    &resolved_user_text,
                                    &team_config,
                                    &team_settings.subagent_roleset,
                                    &identity,
                                    &team_settings,
                                    &self.llm.model_catalog(),
                                ));
                            } else {
                                preplanned = Some(plan);
                            }
                            OrchestrationDecision {
                                route: DecisionRoute::PlanThenAct,
                                assistant_reply: String::new(),
                                tool_calls: Vec::new(),
                                memory_writes: Vec::new(),
                                should_summarize: false,
                                confidence: 0.92,
                                safe_to_send: true,
                            }
                        }
                        Err(err) => {
                            warn!(error = %err, "planner request failed; falling back to decision route");
                            self.request_decision(
                                &identity,
                                conversation_id,
                                &context,
                                route_hint.clone(),
                                &resolved_user_text,
                                &trace_id,
                            )
                            .await
                            .unwrap_or_else(|decision_err| {
                                warn!(error = %decision_err, "decision request failed; using fallback");
                                OrchestrationDecision::safe_fallback(
                                    "I hit a temporary issue. Please rephrase briefly and retry.",
                                )
                            })
                        }
                    }
                }
            } else {
                self.publish_turn_stage(
                    conversation_id,
                    &trace_id,
                    "classify",
                    &resolved_user_text,
                    json!({
                        "reason": planning_decision.reason,
                        "route_hint": format!("{:?}", planning_decision.route_hint),
                    }),
                );
                info!(
                    trace_id = %trace_id,
                    reason = planning_decision.reason,
                    route_hint = ?planning_decision.route_hint,
                    "supervisor planning skipped"
                );
                let _ = self.events.publish(
                    "supervisor_planning_skipped",
                    json!({
                        "conversation_id": conversation_id,
                        "trace_id": trace_id,
                        "reason": planning_decision.reason,
                        "preview": truncate_for_log(&resolved_user_text, 120),
                    }),
                );
                let classifier_started = Instant::now();
                let latest_summary = if let Some(context) = shared_context.as_ref() {
                    context.latest_summary.clone()
                } else {
                    self.store.latest_summary(conversation_id).await?
                };
                let classifier = self
                    .request_classifier(
                        &identity,
                        conversation_id,
                        &prechecked_brain_memories,
                        &resolved_user_text,
                        &effective_prior_turns,
                        latest_summary.as_deref(),
                        &trace_id,
                    )
                    .await
                    .unwrap_or_else(|err| {
                        warn!(error = %err, "classifier request failed; falling back to full decision");
                        OrchestrationDecision {
                            route: route_hint.clone(),
                            assistant_reply: String::new(),
                            tool_calls: Vec::new(),
                            memory_writes: Vec::new(),
                            should_summarize: false,
                            confidence: 0.0,
                            safe_to_send: true,
                        }
                    });
                let classifier_elapsed = classifier_started.elapsed();
                histogram!("ai_microagents_classifier_latency_seconds")
                    .record(classifier_elapsed.as_secs_f64());
                histogram!("ai_microagents_classifier_ms")
                    .record(classifier_elapsed.as_millis() as f64);
                let _ = self.events.publish(
                    "supervisor_classifier",
                    json!({
                        "conversation_id": conversation_id,
                        "trace_id": trace_id,
                        "route": format!("{:?}", classifier.route),
                        "confidence": classifier.confidence,
                        "preview": truncate_for_log(&resolved_user_text, 120),
                    }),
                );

                match classifier.route {
                    DecisionRoute::Ignore if classifier.confidence >= 0.8 => classifier,
                    DecisionRoute::DirectReply | DecisionRoute::AskClarification
                        if classifier.confidence >= 0.55 && live_evidence.is_none() =>
                    {
                        match self
                            .generate_fast_reply(
                                &identity,
                                conversation_id,
                                &trace_id,
                                classifier.route.clone(),
                                &resolved_user_text,
                                &effective_prior_turns,
                                &prechecked_brain_memories,
                            )
                            .await
                        {
                            Ok((assistant_reply, usage, model, latency_ms)) => {
                                let _ = budget.add_cost(usage.estimated_cost_usd);
                                let _ = self
                                    .store
                                    .insert_model_usage(
                                        trace_id.as_str(),
                                        &model,
                                        usage.prompt_tokens,
                                        usage.completion_tokens,
                                        usage.estimated_cost_usd,
                                        latency_ms,
                                    )
                                    .await;
                                OrchestrationDecision {
                                    route: classifier.route,
                                    assistant_reply,
                                    tool_calls: Vec::new(),
                                    memory_writes: Vec::new(),
                                    should_summarize: false,
                                    confidence: classifier.confidence,
                                    safe_to_send: classifier.safe_to_send,
                                }
                            }
                            Err(err) => {
                                warn!(error = %err, "fast reply generation failed; falling back to full decision");
                                let context = self
                                    .ensure_turn_context(
                                        &mut shared_context,
                                        conversation_id,
                                        &event.user_id,
                                        &resolved_user_text,
                                        &principal_permissions.allowed_skills,
                                        &principal_permissions.denied_skills,
                                        &trace_id,
                                        topic_shift_detected,
                                        effective_performance_policy.clone(),
                                        analysis_complexity.clone(),
                                        live_evidence.as_ref(),
                                    )
                                    .await?;
                                self.request_decision(
                                    &identity,
                                    conversation_id,
                                    &context,
                                    classifier.route.clone(),
                                    &resolved_user_text,
                                    &trace_id,
                                )
                                .await
                                .unwrap_or_else(|decision_err| {
                                    warn!(error = %decision_err, "decision request failed; using fallback");
                                    OrchestrationDecision::safe_fallback(
                                        "I hit a temporary issue. Please rephrase briefly and retry.",
                                    )
                                })
                            }
                        }
                    }
                    DecisionRoute::PlanThenAct => {
                        self.publish_turn_stage(
                            conversation_id,
                            &trace_id,
                            "plan_if_needed",
                            &resolved_user_text,
                            json!({
                                "reason": "classifier_selected_plan",
                                "route_hint": format!("{:?}", classifier.route),
                            }),
                        );
                        let context = self
                            .ensure_turn_context(
                                &mut shared_context,
                                conversation_id,
                                &event.user_id,
                                &resolved_user_text,
                                &principal_permissions.allowed_skills,
                                &principal_permissions.denied_skills,
                                &trace_id,
                                topic_shift_detected,
                                effective_performance_policy.clone(),
                                analysis_complexity.clone(),
                                live_evidence.as_ref(),
                            )
                            .await?;
                        match self
                            .request_plan(
                                &identity,
                                &context,
                                &team_config,
                                &team_settings,
                                conversation_id,
                                &resolved_user_text,
                                &trace_id,
                            )
                            .await
                        {
                            Ok((plan, usage)) => {
                                let _ = budget.add_cost(usage.estimated_cost_usd);
                                preplanned = Some(plan);
                                OrchestrationDecision {
                                    route: DecisionRoute::PlanThenAct,
                                    assistant_reply: String::new(),
                                    tool_calls: Vec::new(),
                                    memory_writes: Vec::new(),
                                    should_summarize: false,
                                    confidence: classifier.confidence.max(0.8),
                                    safe_to_send: true,
                                }
                            }
                            Err(err) => {
                                warn!(error = %err, "classifier selected planning but planner failed; falling back to full decision");
                                let context = self
                                    .ensure_turn_context(
                                        &mut shared_context,
                                        conversation_id,
                                        &event.user_id,
                                        &resolved_user_text,
                                        &principal_permissions.allowed_skills,
                                        &principal_permissions.denied_skills,
                                        &trace_id,
                                        topic_shift_detected,
                                        effective_performance_policy.clone(),
                                        analysis_complexity.clone(),
                                        live_evidence.as_ref(),
                                    )
                                    .await?;
                                self.request_decision(
                                    &identity,
                                    conversation_id,
                                    &context,
                                    DecisionRoute::PlanThenAct,
                                    &resolved_user_text,
                                    &trace_id,
                                )
                                .await
                                .unwrap_or_else(|decision_err| {
                                    warn!(error = %decision_err, "decision request failed; using fallback");
                                    OrchestrationDecision::safe_fallback(
                                        "I hit a temporary issue. Please rephrase briefly and retry.",
                                    )
                                })
                            }
                        }
                    }
                    _ => {
                        let context = self
                            .ensure_turn_context(
                                &mut shared_context,
                                conversation_id,
                                &event.user_id,
                                &resolved_user_text,
                                &principal_permissions.allowed_skills,
                                &principal_permissions.denied_skills,
                                &trace_id,
                                topic_shift_detected,
                                effective_performance_policy.clone(),
                                analysis_complexity.clone(),
                                live_evidence.as_ref(),
                            )
                            .await?;
                        self.request_decision(
                            &identity,
                            conversation_id,
                            &context,
                            classifier.route.clone(),
                            &resolved_user_text,
                            &trace_id,
                        )
                        .await
                        .unwrap_or_else(|err| {
                            warn!(error = %err, "decision request failed; using fallback");
                            OrchestrationDecision::safe_fallback(
                                "I hit a temporary issue. Please rephrase briefly and retry.",
                            )
                        })
                    }
                }
            }
        };
        Self::cancel_typing_notifier(typing_notifier);
        info!(
            trace_id = %trace_id,
            route = ?decision.route,
            tool_calls = decision.tool_calls.len(),
            safe_to_send = decision.safe_to_send,
            summarize = decision.should_summarize,
            "orchestration decision produced"
        );

        let mut final_route = decision.route.clone();
        let mut reply = decision.assistant_reply.clone();
        let mut sent_chunks = 0_usize;
        let mut tool_results = Vec::new();

        if matches!(decision.route, DecisionRoute::PlanThenAct) {
            // Paso 8: construir el plan, ejecutar subtareas en paralelo y persistir el estado del DAG.
            self.publish_turn_stage(
                conversation_id,
                &trace_id,
                "execute",
                &resolved_user_text,
                json!({
                    "route": "plan_then_act",
                }),
            );
            let roles = self.team.roleset();
            let plan = preplanned.take().unwrap_or_else(|| {
                build_initial_plan(
                    conversation_id,
                    &resolved_user_text,
                    &team_config,
                    &roles,
                    &identity,
                    &team_settings,
                    &self.llm.model_catalog(),
                )
            });
            let execution_context = if let Some(context) = shared_context.clone() {
                context
            } else {
                self.ensure_turn_context(
                    &mut shared_context,
                    conversation_id,
                    &event.user_id,
                    &resolved_user_text,
                    &principal_permissions.allowed_skills,
                    &principal_permissions.denied_skills,
                    &trace_id,
                    topic_shift_detected,
                    effective_performance_policy.clone(),
                    analysis_complexity.clone(),
                    live_evidence.as_ref(),
                )
                .await?
            };
            info!(
                trace_id = %trace_id,
                plan_id = %plan.id,
                task_count = plan.tasks.len(),
                parallel_groups = plan.parallelizable_groups.len(),
                "plan created"
            );

            self.store
                .upsert_plan_json(
                    &plan.id,
                    conversation_id,
                    &plan.goal,
                    &serde_json::to_value(&plan).map_err(|e| {
                        AppError::Internal(format!("plan serialization failed: {e}"))
                    })?,
                    "running",
                )
                .await?;
            for task in &plan.tasks {
                self.store
                    .upsert_task_json(
                        &task.id,
                        &plan.id,
                        &serde_json::to_value(task).map_err(|e| {
                            AppError::Internal(format!("task serialization failed: {e}"))
                        })?,
                        &format!("{:?}", task.state),
                        None,
                    )
                    .await?;
            }
            let _ = self.events.publish(
                "plan_created",
                json!({
                    "conversation_id": conversation_id,
                    "plan_id": plan.id,
                    "goal": resolved_user_text,
                    "task_count": plan.tasks.len(),
                    "parallelizable_groups": plan.parallelizable_groups,
                    "tasks": plan.tasks.iter().map(|task| json!({
                        "id": task.id,
                        "title": task.title,
                        "dependencies": task.dependencies,
                        "role": task.candidate_role,
                        "route_key": task.route_key,
                        "model_route": task.model_route,
                        "resolved_model": task.resolved_model,
                        "state": format!("{:?}", task.state),
                    })).collect::<Vec<_>>()
                }),
            );

            let (updated_plan, task_results) = crate::execution::scheduler::execute_plan_parallel(
                plan,
                self.team.clone(),
                identity.clone(),
                self.llm.clone(),
                execution_context.clone(),
                self.events.clone(),
                self.team.effective_parallel_limit(),
                team_config.max_task_retries,
                team_config.max_review_loops_per_task,
            )
            .await;
            info!(
                trace_id = %trace_id,
                plan_id = %updated_plan.id,
                accepted = task_results.values().filter(|r| r.accepted).count(),
                failed = task_results.values().filter(|r| !r.accepted).count(),
                "plan execution finished"
            );

            for task in &updated_plan.tasks {
                self.store
                    .upsert_task_json(
                        &task.id,
                        &updated_plan.id,
                        &serde_json::to_value(task).map_err(|e| {
                            AppError::Internal(format!("task serialization failed: {e}"))
                        })?,
                        &format!("{:?}", task.state),
                        None,
                    )
                    .await?;
            }
            self.store
                .upsert_plan_json(
                    &updated_plan.id,
                    conversation_id,
                    &updated_plan.goal,
                    &serde_json::to_value(&updated_plan).map_err(|e| {
                        AppError::Internal(format!("plan serialization failed: {e}"))
                    })?,
                    "completed",
                )
                .await?;

            for result in task_results.values() {
                let started_at = Utc::now()
                    - chrono::Duration::milliseconds(result.duration_ms.saturating_add(1) as i64);
                let ended_at = Utc::now();
                let attempt_id = self
                    .store
                    .insert_task_attempt(TaskAttemptInsert {
                        task_id: &result.task_id,
                        attempt_no: result.attempts.max(1),
                        subagent_id: &result.subagent_id,
                        status: if result.accepted {
                            "accepted"
                        } else {
                            "rejected"
                        },
                        started_at,
                        ended_at: Some(ended_at),
                        error: result.error.as_deref(),
                        duration_ms: Some(result.duration_ms),
                    })
                    .await?;

                if let Some(artifact) = &result.artifact {
                    self.store
                        .insert_task_artifact(
                            &result.task_id,
                            attempt_id,
                            &result.subagent_id,
                            &serde_json::to_value(artifact).map_err(|e| {
                                AppError::Internal(format!("artifact serialization failed: {e}"))
                            })?,
                        )
                        .await?;
                }
                if let Some(review) = &result.review {
                    self.store
                        .insert_task_review(TaskReviewInsert {
                            task_id: &result.task_id,
                            attempt_no: result.attempts.max(1),
                            reviewer: "supervisor",
                            action: &format!("{:?}", review.action),
                            score: review.score,
                            notes: &review.notes,
                            decision_json: &serde_json::to_value(review).map_err(|e| {
                                AppError::Internal(format!("review serialization failed: {e}"))
                            })?,
                        })
                        .await?;
                }
            }

            let _ = self.events.publish(
                "plan_updated",
                json!({
                    "conversation_id": conversation_id,
                    "plan_id": updated_plan.id,
                    "tasks": updated_plan.tasks.iter().map(|task| json!({
                        "id": task.id,
                        "state": format!("{:?}", task.state),
                        "attempts": task.attempts,
                        "review_loops": task.review_loops,
                    })).collect::<Vec<_>>()
                }),
            );

            let _ = self.events.publish(
                "supervisor_integrating",
                json!({
                    "conversation_id": conversation_id,
                    "trace_id": trace_id,
                    "plan_id": updated_plan.id,
                    "accepted_tasks": task_results.values().filter(|r| r.accepted).count(),
                }),
            );
            self.publish_turn_stage(
                conversation_id,
                &trace_id,
                "integrate",
                &resolved_user_text,
                json!({
                    "plan_id": updated_plan.id,
                    "accepted_tasks": task_results.values().filter(|r| r.accepted).count(),
                }),
            );
            let integration_started = Instant::now();
            // Paso 9: integrar los artifacts aceptados en una única respuesta final coherente.
            reply = crate::orchestrator::integration::integrate_artifacts(
                self.llm.as_ref(),
                &identity,
                &resolved_user_text,
                &task_results,
                &execution_context,
            )
            .await;
            histogram!("ai_microagents_integration_ms")
                .record(integration_started.elapsed().as_millis() as f64);
        } else if matches!(decision.route, DecisionRoute::ToolUse)
            && !decision.tool_calls.is_empty()
        {
            // Paso 8 alternativa: ejecutar tools directas cuando el caso no necesita un plan completo.
            self.publish_turn_stage(
                conversation_id,
                &trace_id,
                "execute",
                &resolved_user_text,
                json!({
                    "route": "tool_use",
                    "tool_calls": decision.tool_calls.len(),
                }),
            );
            let _ = self.events.publish(
                "supervisor_tooling",
                json!({
                    "conversation_id": conversation_id,
                    "trace_id": trace_id,
                    "tool_calls": decision.tool_calls.len(),
                }),
            );
            info!(
                trace_id = %trace_id,
                tool_calls = decision.tool_calls.len(),
                "executing tool calls"
            );
            for call in decision.tool_calls {
                let allowed = principal_permissions
                    .allowed_skills
                    .iter()
                    .any(|name| name == "*" || name.eq_ignore_ascii_case(&call.name))
                    && !principal_permissions
                        .denied_skills
                        .iter()
                        .any(|name| name.eq_ignore_ascii_case(&call.name));
                if !allowed {
                    warn!(trace_id = %trace_id, skill = %call.name, "tool blocked by dashboard principal skill policy");
                    continue;
                }
                if !budget.consume_tool_call() {
                    warn!(trace_id = %trace_id, "tool call budget exceeded");
                    break;
                }

                let skill_result = self
                    .skill_runner
                    .execute(
                        &crate::identity::compiler::SystemIdentity {
                            frontmatter: crate::identity::schema::IdentityFrontmatter {
                                permissions: principal_permissions.clone(),
                                ..identity.frontmatter.clone()
                            },
                            ..identity.clone()
                        },
                        SkillCall {
                            name: call.name,
                            arguments: call.arguments,
                        },
                        &trace_id,
                        Some(conversation_id),
                        &event.user_id,
                    )
                    .await;
                info!(
                    trace_id = %trace_id,
                    skill = %skill_result.skill_name,
                    ok = skill_result.ok,
                    duration_ms = skill_result.duration_ms,
                    "tool call completed"
                );
                counter!("ai_microagents_skill_calls_total", "ok" => skill_result.ok.to_string())
                    .increment(1);
                histogram!("ai_microagents_skill_latency_seconds")
                    .record(skill_result.duration_ms as f64 / 1000.0);
                tool_results.push(skill_result);
            }

            if !tool_results.is_empty() && budget.consume_step() {
                let synthesized = self
                    .synthesize_reply_with_tools(
                        &identity,
                        conversation_id,
                        &trace_id,
                        &resolved_user_text,
                        &tool_results,
                        &prechecked_brain_memories,
                    )
                    .await;
                if let Ok((text, usage, model, latency_ms)) = synthesized {
                    reply = text;
                    if !budget.add_cost(usage.estimated_cost_usd) {
                        warn!(trace_id = %trace_id, "cost budget exceeded after tool synthesis");
                    }
                    let _ = self
                        .store
                        .insert_model_usage(
                            &trace_id,
                            &model,
                            usage.prompt_tokens,
                            usage.completion_tokens,
                            usage.estimated_cost_usd,
                            latency_ms,
                        )
                        .await;
                }
            }
        }

        for write in decision.memory_writes {
            if identity.memory().save_facts {
                let _ = self
                    .store
                    .queue_fact_write(
                        Some(conversation_id),
                        Some(&trace_id),
                        &write.key,
                        &write.value,
                        write.confidence,
                        None,
                    )
                    .await;
            }
        }

        if !decision.safe_to_send {
            final_route = DecisionRoute::AskClarification;
            reply =
                "I cannot safely do that. Please clarify your request in safer terms.".to_string();
            warn!(trace_id = %trace_id, "decision marked unsafe_to_send; downgraded to clarification");
        }

        if matches!(final_route, DecisionRoute::Ignore) {
            if let Some(user_turn_content) = deferred_user_turn_content.as_deref() {
                self.store
                    .append_turn(
                        conversation_id,
                        "user",
                        user_turn_content,
                        &trace_id,
                        "ingest",
                        None,
                    )
                    .await?;
            }
            self.store.mark_inbound_processed(&event.event_id).await?;
            return Ok(TurnOutcome {
                trace_id,
                route: final_route,
                reply: None,
                sent_chunks: 0,
            });
        }

        let max_chars = self.max_reply_chars(&identity, &event.channel).max(128);
        let chunks = self.chunk_text(&event.channel, &reply, max_chars);
        // Paso 10: entregar la respuesta al canal respetando límites de chunking y políticas de salida.
        self.publish_turn_stage(
            conversation_id,
            &trace_id,
            "deliver",
            &reply,
            json!({
                "route": format!("{:?}", final_route),
                "chunks": chunks.len(),
            }),
        );
        info!(
            trace_id = %trace_id,
            route = ?final_route,
            chunks = chunks.len(),
            reply_preview = %truncate_for_log(&reply, 120),
            "final reply ready"
        );

        let outbound_started = Instant::now();
        let mut deferred_outbound_rows: Vec<(String, Option<String>, String)> = Vec::new();
        if self.config.policy.outbound_enabled
            && !self.config.policy.dry_run
            && self.channel_enabled(&identity, &event.channel)
            && !self.controls.outbound_kill_switch()
        {
            for chunk in &chunks {
                let send_result = self.send_text(&event.channel, &event.user_id, chunk).await;
                match send_result {
                    Ok(result) => {
                        sent_chunks += 1;
                        info!(
                            trace_id = %trace_id,
                            recipient = %event.user_id,
                            status = %result.status,
                            provider_message_id = ?result.provider_message_id,
                            preview = %truncate_for_log(chunk, 120),
                            "outbound chunk sent"
                        );
                        deferred_outbound_rows.push((
                            chunk.clone(),
                            result.provider_message_id.clone(),
                            result.status.clone(),
                        ));
                        let _ = self.events.publish(
                            "outbound_message_sent",
                            json!({
                                "conversation_id": conversation_id,
                                "channel": event.channel,
                                "recipient": event.user_id,
                                "content": chunk,
                                "provider_message_id": result.provider_message_id,
                            }),
                        );
                    }
                    Err(err) => {
                        counter!("ai_microagents_outbound_failures_total").increment(1);
                        error!(
                            trace_id = %trace_id,
                            recipient = %event.user_id,
                            preview = %truncate_for_log(chunk, 120),
                            error = %err,
                            "outbound chunk failed"
                        );
                        deferred_outbound_rows.push((chunk.clone(), None, "failed".to_string()));
                        let _ = self.events.publish(
                            "outbound_message_failed",
                            json!({
                                "conversation_id": conversation_id,
                                "channel": event.channel,
                                "recipient": event.user_id,
                                "content": chunk,
                                "error": err.to_string(),
                            }),
                        );
                        return Err(err);
                    }
                }
            }
        } else {
            deferred_outbound_rows.push((reply.clone(), None, "suppressed".to_string()));
        }
        let outbound_elapsed = outbound_started.elapsed();
        histogram!("ai_microagents_outbound_seconds").record(outbound_elapsed.as_secs_f64());
        histogram!("ai_microagents_outbound_ms").record(outbound_elapsed.as_millis() as f64);

        if let Some(user_turn_content) = deferred_user_turn_content.as_deref() {
            self.store
                .publish_hot_turn(conversation_id, "user", user_turn_content)
                .await;
            let _ = self.events.publish(
                "conversation_turn_created",
                json!({
                    "conversation_id": conversation_id,
                    "role": "user",
                    "channel": event.channel,
                    "user_id": event.user_id,
                    "content": user_turn_content,
                }),
            );
        }

        self.store
            .publish_hot_turn(conversation_id, "assistant", &reply)
            .await;
        let _ = self.events.publish(
            "conversation_turn_created",
            json!({
                "conversation_id": conversation_id,
                "role": "assistant",
                "channel": event.channel,
                "user_id": event.user_id,
                "content": reply,
            }),
        );
        let _ = self.events.publish(
            "supervisor_completed",
            json!({
                "conversation_id": conversation_id,
                "trace_id": trace_id,
                "route": format!("{final_route:?}"),
                "preview": truncate_for_log(&reply, 120),
            }),
        );

        counter!("ai_microagents_processed_turns_total", "route" => format!("{:?}", final_route))
            .increment(1);
        info!(
            trace_id = %trace_id,
            event_id = %event.event_id,
            route = ?final_route,
            sent_chunks,
            "turn completed"
        );
        let turn_elapsed = turn_started.elapsed();
        histogram!("ai_microagents_turn_total_seconds").record(turn_elapsed.as_secs_f64());
        histogram!("ai_microagents_turn_total_ms").record(turn_elapsed.as_millis() as f64);

        let store = self.store.clone();
        let summarizer = self.summarizer.clone();
        let brain_capture = self.brain_capture.clone();
        let event_id = event.event_id.clone();
        let event_channel = event.channel.clone();
        let event_user_id = event.user_id.clone();
        let trace_id_for_finalize = trace_id.clone();
        let save_summaries = identity.memory().save_summaries;
        let summarize_every_n_turns = identity.memory().summarize_every_n_turns;
        let deferred_user_turn_content = deferred_user_turn_content.clone();
        let reply_for_finalize = reply.clone();
        let final_route_label = format!("{:?}", final_route);
        let deferred_outbound_rows = deferred_outbound_rows.clone();
        let brain_enabled = identity.memory().brain_enabled();
        let brain_auto_write_mode = identity.memory().auto_write_mode().to_string();
        let brain_tool_name = tool_results
            .iter()
            .find(|result| result.ok)
            .map(|result| result.skill_name.clone());
        tokio::spawn(async move {
            // Paso 11.1: cerrar el evento lo antes posible para liberar el turno visible del usuario.
            if let Err(err) = store.mark_inbound_processed(&event_id).await {
                warn!(
                    trace_id = %trace_id_for_finalize,
                    event_id = %event_id,
                    error = %err,
                    "deferred inbound processed mark failed"
                );
            }

            // Paso 11.2: sacar del hot-path la persistencia secundaria para no bloquear el siguiente turno.
            for (content, provider_message_id, status) in deferred_outbound_rows {
                if let Err(err) = store
                    .insert_outbound_message(OutboundMessageInsert {
                        trace_id: &trace_id_for_finalize,
                        conversation_id: Some(conversation_id),
                        channel: &event_channel,
                        recipient: &event_user_id,
                        content: &content,
                        provider_message_id: provider_message_id.as_deref(),
                        status: &status,
                    })
                    .await
                {
                    warn!(
                        trace_id = %trace_id_for_finalize,
                        error = %err,
                        "deferred outbound persist failed"
                    );
                }
            }

            let mut persisted_user_turn_id = None;
            if let Some(user_turn_content) = deferred_user_turn_content.as_deref() {
                match store
                    .append_turn(
                        conversation_id,
                        "user",
                        user_turn_content,
                        &trace_id_for_finalize,
                        "ingest",
                        None,
                    )
                    .await
                {
                    Ok(turn_id) => persisted_user_turn_id = Some(turn_id),
                    Err(err) => warn!(
                        trace_id = %trace_id_for_finalize,
                        error = %err,
                        "deferred user turn persist failed"
                    ),
                }
            }

            let persisted_assistant_turn_id = match store
                .append_turn(
                    conversation_id,
                    "assistant",
                    &reply_for_finalize,
                    &trace_id_for_finalize,
                    &final_route_label,
                    None,
                )
                .await
            {
                Ok(turn_id) => Some(turn_id),
                Err(err) => {
                    warn!(
                        trace_id = %trace_id_for_finalize,
                        error = %err,
                        "deferred assistant turn persist failed"
                    );
                    None
                }
            };

            if save_summaries && summarize_every_n_turns > 0 {
                match store.count_turns(conversation_id).await {
                    Ok(count) if count % summarize_every_n_turns as i64 == 0 => {
                        match store.recent_turns(conversation_id, 50).await {
                            Ok(turns) => {
                                let summary = summarizer.summarize(&turns);
                                if let Err(err) = store
                                    .queue_summary_write(
                                        conversation_id,
                                        Some(&trace_id_for_finalize),
                                        &summary,
                                    )
                                    .await
                                {
                                    warn!(
                                        trace_id = %trace_id_for_finalize,
                                        error = %err,
                                        "deferred summary write failed"
                                    );
                                }
                            }
                            Err(err) => warn!(
                                trace_id = %trace_id_for_finalize,
                                error = %err,
                                "deferred summary load failed"
                            ),
                        }
                    }
                    Ok(_) => {}
                    Err(err) => warn!(
                        trace_id = %trace_id_for_finalize,
                        error = %err,
                        "deferred turn count failed"
                    ),
                }
            }

            // Paso 11.3: extraer y persistir memoria estructurada sin bloquear la respuesta ya entregada.
            if brain_enabled {
                let recent_turns = match store.recent_turns(conversation_id, 8).await {
                    Ok(turns) => turns,
                    Err(err) => {
                        warn!(
                            trace_id = %trace_id_for_finalize,
                            error = %err,
                            "deferred brain context load failed"
                        );
                        Vec::new()
                    }
                };
                let user_turn_for_brain = deferred_user_turn_content.as_deref().unwrap_or_default();
                let candidates = brain_capture.execute(CaptureBrainMemoryRequest {
                    enabled: brain_enabled,
                    auto_write_mode: &brain_auto_write_mode,
                    user_id: &event_user_id,
                    conversation_id,
                    channel: &event_channel,
                    trace_id: &trace_id_for_finalize,
                    user_text: user_turn_for_brain,
                    assistant_reply: &reply_for_finalize,
                    recent_turns: &recent_turns,
                    source_turn_id: persisted_user_turn_id.or(persisted_assistant_turn_id),
                    tool_name: brain_tool_name.as_deref(),
                    url: None,
                });
                if !candidates.is_empty() {
                    if let Err(err) = store
                        .queue_brain_write(Some(&trace_id_for_finalize), &candidates)
                        .await
                    {
                        warn!(
                            trace_id = %trace_id_for_finalize,
                            error = %err,
                            "deferred brain write failed"
                        );
                    }
                }
            }
        });

        Ok(TurnOutcome {
            trace_id,
            route: final_route,
            reply: Some(reply),
            sent_chunks,
        })
    }

    pub async fn process_local_message(&self, user_id: &str, text: &str) -> AppResult<TurnOutcome> {
        let event = NormalizedInboundEvent {
            event_id: format!("local-{}", tracing_ids::new_trace_id()),
            channel: "local".to_string(),
            conversation_external_id: user_id.to_string(),
            user_id: user_id.to_string(),
            text: text.to_string(),
            kind: InboundKind::UserMessage,
            timestamp: Utc::now(),
            queued_at: None,
            attachments: Vec::new(),
            raw_payload: json!({"source": "local"}),
        };

        let payload = json!({"normalized": event.clone(), "raw": event.raw_payload});
        let _ = self
            .store
            .insert_inbound_event(&event.event_id, "local", &payload)
            .await?;
        self.process_inbound_event(event).await
    }

    #[allow(clippy::too_many_arguments)]
    async fn request_plan(
        &self,
        identity: &crate::identity::compiler::SystemIdentity,
        context: &TurnContext,
        team_config: &crate::team::config::TeamConfig,
        team_settings: &crate::team::config::TeamRuntimeSettings,
        conversation_id: i64,
        user_text: &str,
        trace_id: &str,
    ) -> AppResult<(crate::planner::plan::ExecutionPlan, crate::llm::Usage)> {
        let planning_started = Instant::now();
        let messages = compile_planning_prompt(
            identity,
            user_text,
            context,
            &team_settings.subagent_roleset,
            team_config.plan_max_tasks,
            team_config.plan_max_depth,
        );
        let selection = self.resolve_model_selection_for_route(
            identity,
            "planner",
            InputModality::Text,
            OutputModality::Json,
            ReasoningLevel::High,
            false,
            Some(identity.frontmatter.budgets.max_turn_cost_usd.min(0.03)),
            Some(identity.frontmatter.budgets.timeout_ms),
        );
        info!(
            trace_id = %trace_id,
            route_key = %selection.route_key,
            resolved_model = %selection.resolved_model,
            reason = %selection.reason,
            "supervisor model selected for planning"
        );
        let _ = self.events.publish(
            "supervisor_model_selected",
            json!({
                "conversation_id": conversation_id,
                "trace_id": trace_id,
                "phase": "planner",
                "route_key": selection.route_key,
                "resolved_model": selection.resolved_model,
                "reason": selection.reason,
            }),
        );
        let response = match self
            .chat_completion_with_route_retry(
                identity,
                conversation_id,
                trace_id,
                "planner",
                &selection,
                &messages,
                identity.frontmatter.budgets.max_output_tokens,
                0.0,
                true,
                identity.frontmatter.budgets.timeout_ms,
            )
            .await
        {
            Ok(response) => response,
            Err(err) => {
                warn!(error = %err, "planner model failed; using deterministic fallback plan");
                return Ok((
                    build_initial_plan(
                        conversation_id,
                        user_text,
                        team_config,
                        &team_settings.subagent_roleset,
                        identity,
                        team_settings,
                        &self.llm.model_catalog(),
                    ),
                    crate::llm::Usage::default(),
                ));
            }
        };

        let _ = self
            .store
            .insert_model_usage(
                trace_id,
                &response.model,
                response.usage.prompt_tokens,
                response.usage.completion_tokens,
                response.usage.estimated_cost_usd,
                response.latency_ms,
            )
            .await;
        histogram!("ai_microagents_model_latency_seconds", "model" => response.model.clone())
            .record(response.latency_ms as f64 / 1000.0);

        let mut contract = parse_or_repair_execution_plan(
            self.llm.as_ref(),
            &response.model,
            &response.content,
            identity.frontmatter.budgets.timeout_ms,
        )
        .await?;
        if plan_contract_looks_like_parser_fallback(&contract) {
            if let Some(retry_model) =
                retry_model_for_route(identity, &selection.route_key, &response.model)
            {
                match self
                    .retry_chat_completion(
                        conversation_id,
                        trace_id,
                        "planner_retry",
                        &retry_model,
                        &messages,
                        identity.frontmatter.budgets.max_output_tokens,
                        0.0,
                        true,
                        identity.frontmatter.budgets.timeout_ms,
                    )
                    .await
                {
                    Ok(retry_response) => {
                        let _ = self
                            .store
                            .insert_model_usage(
                                trace_id,
                                &retry_response.model,
                                retry_response.usage.prompt_tokens,
                                retry_response.usage.completion_tokens,
                                retry_response.usage.estimated_cost_usd,
                                retry_response.latency_ms,
                            )
                            .await;
                        contract = parse_or_repair_execution_plan(
                            self.llm.as_ref(),
                            &retry_response.model,
                            &retry_response.content,
                            identity.frontmatter.budgets.timeout_ms,
                        )
                        .await?;
                    }
                    Err(err) => warn!(error = %err, "planner retry after parser fallback failed"),
                }
            }
        }
        let plan = build_plan_from_contract(
            conversation_id,
            user_text,
            contract,
            team_config,
            &team_settings.subagent_roleset,
            identity,
            team_settings,
            &self.llm.model_catalog(),
        );
        histogram!("ai_microagents_planning_seconds")
            .record(planning_started.elapsed().as_secs_f64());
        Ok((plan, response.usage))
    }

    async fn request_classifier(
        &self,
        identity: &crate::identity::compiler::SystemIdentity,
        conversation_id: i64,
        brain_memories: &[BrainMemory],
        user_text: &str,
        prior_turns: &[crate::storage::ConversationTurn],
        latest_summary: Option<&str>,
        trace_id: &str,
    ) -> AppResult<OrchestrationDecision> {
        let messages = compile_classifier_prompt(
            identity,
            user_text,
            prior_turns,
            latest_summary,
            brain_memories,
        );
        let selection = self.resolve_model_selection_for_route(
            identity,
            "router_fast",
            InputModality::Text,
            OutputModality::Json,
            ReasoningLevel::Low,
            false,
            Some(0.005),
            Some(2_000),
        );
        info!(
            trace_id = %trace_id,
            route_key = %selection.route_key,
            resolved_model = %selection.resolved_model,
            reason = %selection.reason,
            "supervisor model selected for classifier"
        );
        let _ = self.events.publish(
            "supervisor_model_selected",
            json!({
                "conversation_id": conversation_id,
                "trace_id": trace_id,
                "phase": "classifier",
                "route_key": selection.route_key,
                "resolved_model": selection.resolved_model,
                "reason": selection.reason,
            }),
        );

        let response = self
            .chat_completion_with_route_retry(
                identity,
                conversation_id,
                trace_id,
                "classifier",
                &selection,
                &messages,
                180,
                0.0,
                true,
                2_000,
            )
            .await?;

        let _ = self
            .store
            .insert_model_usage(
                trace_id,
                &response.model,
                response.usage.prompt_tokens,
                response.usage.completion_tokens,
                response.usage.estimated_cost_usd,
                response.latency_ms,
            )
            .await;

        let mut decision =
            parse_or_repair_decision(self.llm.as_ref(), &response.model, &response.content, 2_000)
                .await?;
        if decision_is_internal_parse_fallback(&decision) {
            if let Some(retry_model) =
                retry_model_for_route(identity, &selection.route_key, &response.model)
            {
                match self
                    .retry_chat_completion(
                        conversation_id,
                        trace_id,
                        "classifier_retry",
                        &retry_model,
                        &messages,
                        180,
                        0.0,
                        true,
                        2_000,
                    )
                    .await
                {
                    Ok(retry_response) => {
                        let _ = self
                            .store
                            .insert_model_usage(
                                trace_id,
                                &retry_response.model,
                                retry_response.usage.prompt_tokens,
                                retry_response.usage.completion_tokens,
                                retry_response.usage.estimated_cost_usd,
                                retry_response.latency_ms,
                            )
                            .await;
                        decision = parse_or_repair_decision(
                            self.llm.as_ref(),
                            &retry_response.model,
                            &retry_response.content,
                            2_000,
                        )
                        .await?;
                    }
                    Err(err) => {
                        warn!(error = %err, "classifier retry after parser fallback failed")
                    }
                }
            }
        }
        Ok(decision)
    }

    async fn request_decision(
        &self,
        identity: &crate::identity::compiler::SystemIdentity,
        conversation_id: i64,
        context: &TurnContext,
        route_hint: DecisionRoute,
        user_text: &str,
        trace_id: &str,
    ) -> AppResult<OrchestrationDecision> {
        let messages = compile_decision_prompt(identity, route_hint.clone(), user_text, context);
        let (route_key, reasoning_level, requires_tools) = match route_hint {
            DecisionRoute::DirectReply | DecisionRoute::Ignore => {
                ("router_fast", ReasoningLevel::Low, false)
            }
            DecisionRoute::ToolUse => ("tool_use", ReasoningLevel::Medium, true),
            DecisionRoute::PlanThenAct => ("reasoning", ReasoningLevel::High, false),
            DecisionRoute::AskClarification => ("router_fast", ReasoningLevel::Low, false),
        };
        let selection = self.resolve_model_selection_for_route(
            identity,
            route_key,
            InputModality::Text,
            OutputModality::Json,
            reasoning_level,
            requires_tools,
            Some(identity.frontmatter.budgets.max_turn_cost_usd.min(0.02)),
            Some(identity.frontmatter.budgets.timeout_ms),
        );
        info!(
            trace_id = %trace_id,
            route_key = %selection.route_key,
            resolved_model = %selection.resolved_model,
            reason = %selection.reason,
            "supervisor model selected for decision"
        );
        let _ = self.events.publish(
            "supervisor_model_selected",
            json!({
                "conversation_id": conversation_id,
                "trace_id": trace_id,
                "phase": "decision",
                "route_key": selection.route_key,
                "resolved_model": selection.resolved_model,
                "reason": selection.reason,
            }),
        );

        let response = self
            .chat_completion_with_route_retry(
                identity,
                conversation_id,
                trace_id,
                "decision",
                &selection,
                &messages,
                identity.frontmatter.budgets.max_output_tokens,
                0.1,
                true,
                identity.frontmatter.budgets.timeout_ms,
            )
            .await?;

        let _ = self
            .store
            .insert_model_usage(
                trace_id,
                &response.model,
                response.usage.prompt_tokens,
                response.usage.completion_tokens,
                response.usage.estimated_cost_usd,
                response.latency_ms,
            )
            .await;
        histogram!("ai_microagents_model_latency_seconds", "model" => response.model.clone())
            .record(response.latency_ms as f64 / 1000.0);

        let mut decision = parse_or_repair_decision(
            self.llm.as_ref(),
            &response.model,
            &response.content,
            identity.frontmatter.budgets.timeout_ms,
        )
        .await?;
        if decision_is_internal_parse_fallback(&decision) {
            if let Some(retry_model) =
                retry_model_for_route(identity, &selection.route_key, &response.model)
            {
                match self
                    .retry_chat_completion(
                        conversation_id,
                        trace_id,
                        "decision_retry",
                        &retry_model,
                        &messages,
                        identity.frontmatter.budgets.max_output_tokens,
                        0.1,
                        true,
                        identity.frontmatter.budgets.timeout_ms,
                    )
                    .await
                {
                    Ok(retry_response) => {
                        let _ = self
                            .store
                            .insert_model_usage(
                                trace_id,
                                &retry_response.model,
                                retry_response.usage.prompt_tokens,
                                retry_response.usage.completion_tokens,
                                retry_response.usage.estimated_cost_usd,
                                retry_response.latency_ms,
                            )
                            .await;
                        decision = parse_or_repair_decision(
                            self.llm.as_ref(),
                            &retry_response.model,
                            &retry_response.content,
                            identity.frontmatter.budgets.timeout_ms,
                        )
                        .await?;
                    }
                    Err(err) => warn!(error = %err, "decision retry after parser fallback failed"),
                }
            }
        }
        if decision_is_internal_parse_fallback(&decision) {
            if let Some(reply) = contextual_follow_up_fallback(user_text, context) {
                return Ok(OrchestrationDecision {
                    route: DecisionRoute::AskClarification,
                    assistant_reply: reply,
                    tool_calls: Vec::new(),
                    memory_writes: Vec::new(),
                    should_summarize: false,
                    confidence: 0.2,
                    safe_to_send: true,
                });
            }
        }
        Ok(decision)
    }

    async fn generate_fast_reply(
        &self,
        identity: &crate::identity::compiler::SystemIdentity,
        conversation_id: i64,
        trace_id: &str,
        route: DecisionRoute,
        user_text: &str,
        prior_turns: &[crate::storage::ConversationTurn],
        brain_memories: &[BrainMemory],
    ) -> AppResult<(String, crate::llm::Usage, String, u64)> {
        let messages =
            compile_fast_reply_prompt(identity, &route, user_text, prior_turns, brain_memories);
        let selection = self.resolve_model_selection_for_route(
            identity,
            "fast_text",
            InputModality::Text,
            OutputModality::Text,
            if matches!(route, DecisionRoute::AskClarification) {
                ReasoningLevel::Low
            } else {
                ReasoningLevel::Medium
            },
            false,
            Some(0.01),
            Some(identity.frontmatter.budgets.timeout_ms.min(4_000)),
        );
        info!(
            route_key = %selection.route_key,
            resolved_model = %selection.resolved_model,
            reason = %selection.reason,
            "supervisor model selected for fast reply"
        );
        let _ = self.events.publish(
            "supervisor_model_selected",
            json!({
                "conversation_id": conversation_id,
                "trace_id": trace_id,
                "phase": "fast_reply",
                "route_key": selection.route_key,
                "resolved_model": selection.resolved_model,
                "reason": selection.reason,
            }),
        );
        let response = self
            .chat_completion_with_route_retry(
                identity,
                conversation_id,
                trace_id,
                "fast_reply",
                &selection,
                &messages,
                identity.frontmatter.budgets.max_output_tokens.min(280),
                0.2,
                false,
                identity.frontmatter.budgets.timeout_ms.min(4_000),
            )
            .await?;

        Ok((
            response.content,
            response.usage,
            response.model,
            response.latency_ms,
        ))
    }

    async fn synthesize_reply_with_tools(
        &self,
        identity: &crate::identity::compiler::SystemIdentity,
        conversation_id: i64,
        trace_id: &str,
        user_text: &str,
        tool_results: &[crate::skills::SkillResult],
        brain_memories: &[BrainMemory],
    ) -> AppResult<(String, crate::llm::Usage, String, u64)> {
        let messages = compile_final_answer_prompt(
            identity,
            user_text,
            &serde_json::to_string(tool_results)
                .map_err(|e| AppError::Internal(format!("serialize tool results failed: {e}")))?,
            brain_memories,
        );

        let selection = self.resolve_model_selection_for_route(
            identity,
            "fast_text",
            InputModality::Text,
            OutputModality::Text,
            ReasoningLevel::Medium,
            false,
            Some(identity.frontmatter.budgets.max_turn_cost_usd.min(0.02)),
            Some(identity.frontmatter.budgets.timeout_ms),
        );
        info!(
            route_key = %selection.route_key,
            resolved_model = %selection.resolved_model,
            reason = %selection.reason,
            "supervisor model selected for tool answer synthesis"
        );
        let _ = self.events.publish(
            "supervisor_model_selected",
            json!({
                "conversation_id": conversation_id,
                "trace_id": trace_id,
                "phase": "tool_synthesis",
                "route_key": selection.route_key,
                "resolved_model": selection.resolved_model,
                "reason": selection.reason,
            }),
        );
        let response = self
            .chat_completion_with_route_retry(
                identity,
                conversation_id,
                trace_id,
                "tool_synthesis",
                &selection,
                &messages,
                identity.frontmatter.budgets.max_output_tokens,
                0.2,
                false,
                identity.frontmatter.budgets.timeout_ms,
            )
            .await?;

        Ok((
            response.content,
            response.usage,
            response.model,
            response.latency_ms,
        ))
    }

    #[allow(clippy::too_many_arguments)]
    fn resolve_model_selection_for_route(
        &self,
        identity: &crate::identity::compiler::SystemIdentity,
        route_key: &'static str,
        input_modality: InputModality,
        output_modality: OutputModality,
        reasoning_level: ReasoningLevel,
        requires_tools: bool,
        max_cost_usd: Option<f64>,
        max_latency_ms: Option<u64>,
    ) -> crate::llm::broker::ModelSelection {
        let settings = self.team.runtime_settings();
        let effective_policy =
            if matches!(
                route_key,
                "planner" | "reasoning" | "reviewer_strict" | "integrator_complex"
            ) && !matches!(settings.performance_policy, PerformancePolicy::Fast)
            {
                PerformancePolicy::MaxQuality
            } else {
                settings.performance_policy
            };
        ModelBroker::new(self.llm.model_catalog()).resolve(
            &identity.frontmatter.model_routes,
            ModelSelectionRequest {
                route_key,
                input_modality,
                output_modality,
                reasoning_level,
                requires_tools,
                max_cost_usd,
                max_latency_ms,
                performance_policy: effective_policy,
                escalation_tier: settings.max_escalation_tier,
            },
        )
    }

    #[allow(clippy::too_many_arguments)]
    async fn chat_completion_with_route_retry(
        &self,
        identity: &crate::identity::compiler::SystemIdentity,
        conversation_id: i64,
        trace_id: &str,
        phase: &str,
        selection: &crate::llm::broker::ModelSelection,
        messages: &[ChatMessage],
        max_output_tokens: u32,
        temperature: f32,
        require_json: bool,
        timeout_ms: u64,
    ) -> AppResult<crate::llm::LlmResponse> {
        match self
            .llm
            .chat_completion(LlmRequest {
                model: selection.resolved_model.clone(),
                messages: messages.to_vec(),
                max_output_tokens,
                temperature,
                require_json,
                timeout_ms,
            })
            .await
        {
            Ok(response) => Ok(response),
            Err(err) => {
                let mapped = map_provider_error(err);
                if !should_retry_provider_error(&mapped) {
                    return Err(mapped);
                }

                let Some(retry_model) = retry_model_for_route(
                    identity,
                    &selection.route_key,
                    &selection.resolved_model,
                ) else {
                    return Err(mapped);
                };
                self.retry_chat_completion(
                    conversation_id,
                    trace_id,
                    phase,
                    &retry_model,
                    messages,
                    max_output_tokens,
                    temperature,
                    require_json,
                    timeout_ms,
                )
                .await
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn retry_chat_completion(
        &self,
        conversation_id: i64,
        trace_id: &str,
        phase: &str,
        retry_model: &str,
        messages: &[ChatMessage],
        max_output_tokens: u32,
        temperature: f32,
        require_json: bool,
        timeout_ms: u64,
    ) -> AppResult<crate::llm::LlmResponse> {
        warn!(
            trace_id = %trace_id,
            phase,
            retry_model = %retry_model,
            "retrying model call with alternate route model"
        );
        let _ = self.events.publish(
            "supervisor_model_retry",
            json!({
                "conversation_id": conversation_id,
                "trace_id": trace_id,
                "phase": phase,
                "retry_model": retry_model,
            }),
        );
        self.llm
            .chat_completion(LlmRequest {
                model: retry_model.to_string(),
                messages: messages.to_vec(),
                max_output_tokens,
                temperature,
                require_json,
                timeout_ms,
            })
            .await
            .map_err(map_provider_error)
    }

    async fn collect_live_evidence(
        &self,
        identity: &crate::identity::compiler::SystemIdentity,
        conversation_id: i64,
        user_id: &str,
        trace_id: &str,
        requirement: &CurrentDataRequirement,
    ) -> AppResult<EvidenceBundle> {
        let documents = match requirement.intent {
            CurrentDataIntent::None => Vec::new(),
            CurrentDataIntent::MarketData => {
                self.research
                    .fetch_market_documents(
                        identity,
                        trace_id,
                        conversation_id,
                        user_id,
                        &requirement.entities,
                    )
                    .await?
            }
            CurrentDataIntent::UrlInspection => {
                self.research
                    .inspect_urls(
                        identity,
                        trace_id,
                        conversation_id,
                        user_id,
                        &requirement.extracted_urls,
                    )
                    .await?
            }
            CurrentDataIntent::WebResearch => {
                if !requirement.extracted_urls.is_empty() {
                    self.research
                        .inspect_urls(
                            identity,
                            trace_id,
                            conversation_id,
                            user_id,
                            &requirement.extracted_urls,
                        )
                        .await?
                } else if !requirement.entities.is_empty() {
                    self.research
                        .fetch_market_documents(
                            identity,
                            trace_id,
                            conversation_id,
                            user_id,
                            &requirement.entities,
                        )
                        .await?
                } else {
                    return Err(AppError::Validation(
                        "la consulta requiere evidencia externa pero no aporta una fuente verificable"
                            .to_string(),
                    ));
                }
            }
        };

        let items = documents
            .into_iter()
            .map(|document| EvidenceItem {
                source: document.source,
                kind: document.kind,
                title: document.title,
                url: Some(document.url),
                snippet: document.excerpt,
                fetched_at: Utc::now(),
            })
            .collect::<Vec<_>>();

        Ok(EvidenceBundle {
            requirement: requirement.clone(),
            summary: if items.is_empty() {
                "sin evidencia".to_string()
            } else {
                format!(
                    "{} fuente(s) verificadas para {}",
                    items.len(),
                    requirement.reason
                )
            },
            items,
        })
    }

    async fn precheck_brain_memories(
        &self,
        identity: &crate::identity::compiler::SystemIdentity,
        conversation_id: i64,
        user_id: &str,
        user_text: &str,
        ignore_conversation_scope: bool,
    ) -> Vec<BrainMemory> {
        if !identity.memory().brain_enabled() || !identity.memory().precheck_each_turn() {
            return Vec::new();
        }

        match self
            .brain_retrieval
            .execute(RetrieveBrainMemoryRequest {
                enabled: true,
                conversation_id: if ignore_conversation_scope {
                    None
                } else {
                    Some(conversation_id)
                },
                user_id: Some(user_id),
                query: user_text,
                conversation_limit: if ignore_conversation_scope {
                    0
                } else {
                    identity.memory().conversation_limit()
                },
                user_limit: identity.memory().user_limit(),
            })
            .await
        {
            Ok(memories) => memories,
            Err(err) => {
                warn!(error = %err, "brain precheck failed; continuing without structured memory");
                Vec::new()
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn load_turn_context(
        &self,
        conversation_id: i64,
        user_id: &str,
        trace_id: &str,
        user_text: &str,
        allowed_skills: &[String],
        denied_skills: &[String],
        ignore_prior_context: bool,
        performance_policy: PerformancePolicy,
        analysis_complexity: AnalysisComplexity,
    ) -> AppResult<TurnContext> {
        let hints = collect_hints(conversation_id, &self.store).await;
        let settings = self.team.runtime_settings();
        let identity = self.identity.get();
        let locale = identity.frontmatter.locale.clone();
        self.context_builder
            .build(ContextBuildRequest {
                conversation_id,
                user_id,
                trace_id,
                user_text,
                hints: &hints,
                allowed_skills,
                denied_skills,
                ignore_prior_context,
                performance_policy,
                max_escalation_tier: settings.max_escalation_tier,
                analysis_complexity,
                brain_enabled: identity.memory().brain_enabled(),
                precheck_each_turn: identity.memory().precheck_each_turn(),
                brain_conversation_limit: identity.memory().conversation_limit(),
                brain_user_limit: identity.memory().user_limit(),
                locale: locale.as_str(),
            })
            .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn ensure_turn_context(
        &self,
        cached: &mut Option<TurnContext>,
        conversation_id: i64,
        user_id: &str,
        user_text: &str,
        allowed_skills: &[String],
        denied_skills: &[String],
        trace_id: &str,
        ignore_prior_context: bool,
        performance_policy: PerformancePolicy,
        analysis_complexity: AnalysisComplexity,
        current_evidence: Option<&EvidenceBundle>,
    ) -> AppResult<TurnContext> {
        if let Some(mut context) = cached.clone() {
            context.current_evidence = current_evidence.cloned();
            context.performance_policy = performance_policy;
            context.analysis_complexity = analysis_complexity;
            return Ok(context);
        }

        self.publish_turn_stage(
            conversation_id,
            trace_id,
            "context_load",
            user_text,
            json!({}),
        );
        let context_started = Instant::now();
        let context = self
            .load_turn_context(
                conversation_id,
                user_id,
                trace_id,
                user_text,
                allowed_skills,
                denied_skills,
                ignore_prior_context,
                performance_policy.clone(),
                analysis_complexity.clone(),
            )
            .await?;
        let mut context = context;
        context.current_evidence = current_evidence.cloned();
        histogram!("ai_microagents_context_load_ms")
            .record(context_started.elapsed().as_millis() as f64);
        *cached = Some(context.clone());
        Ok(context)
    }

    fn publish_turn_stage(
        &self,
        conversation_id: i64,
        trace_id: &str,
        stage: &str,
        preview: &str,
        extra: serde_json::Value,
    ) {
        let mut payload = json!({
            "conversation_id": conversation_id,
            "trace_id": trace_id,
            "stage": stage,
            "preview": truncate_for_log(preview, 120),
        });
        if let (Some(payload_obj), Some(extra_obj)) = (payload.as_object_mut(), extra.as_object()) {
            for (key, value) in extra_obj {
                payload_obj.insert(key.clone(), value.clone());
            }
        }
        let _ = self.events.publish("turn_stage_changed", payload);
    }

    fn spawn_typing_notifier(
        &self,
        identity: &crate::identity::compiler::SystemIdentity,
        channel: &str,
        recipient: &str,
    ) -> Option<oneshot::Sender<()>> {
        if channel != "telegram"
            || !self.config.policy.outbound_enabled
            || self.config.policy.dry_run
            || self.controls.outbound_kill_switch()
            || !self.channel_enabled(identity, channel)
        {
            return None;
        }

        let telegram = self.telegram.clone();
        let recipient = recipient.to_string();
        let delay_ms = self.team.runtime_settings().typing_delay_ms;
        let (cancel_tx, mut cancel_rx) = oneshot::channel();
        tokio::spawn(async move {
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_millis(delay_ms)) => {
                    let _ = telegram.send_chat_action(&recipient, "typing").await;
                }
                _ = &mut cancel_rx => {}
            }
        });
        Some(cancel_tx)
    }

    fn cancel_typing_notifier(cancel: Option<oneshot::Sender<()>>) {
        if let Some(cancel) = cancel {
            let _ = cancel.send(());
        }
    }

    async fn safe_abort(
        &self,
        event: &NormalizedInboundEvent,
        conversation_id: i64,
        trace_id: &str,
        route: DecisionRoute,
        message: &str,
    ) -> AppResult<TurnOutcome> {
        self.store
            .append_turn(
                conversation_id,
                "assistant",
                message,
                trace_id,
                "budget_abort",
                None,
            )
            .await?;

        if self.config.policy.outbound_enabled && !self.config.policy.dry_run {
            let send = self
                .send_text(&event.channel, &event.user_id, message)
                .await;
            if let Err(err) = send {
                error!(error = %err, "failed sending safe abort message");
            }
        }

        self.store.mark_inbound_processed(&event.event_id).await?;
        Ok(TurnOutcome {
            trace_id: trace_id.to_string(),
            route,
            reply: Some(message.to_string()),
            sent_chunks: 1,
        })
    }

    fn channel_enabled(
        &self,
        identity: &crate::identity::compiler::SystemIdentity,
        channel: &str,
    ) -> bool {
        match channel {
            "telegram" => identity.frontmatter.channels.telegram.enabled,
            _ => false,
        }
    }

    fn max_reply_chars(
        &self,
        identity: &crate::identity::compiler::SystemIdentity,
        channel: &str,
    ) -> usize {
        match channel {
            "telegram" => identity.frontmatter.channels.telegram.max_reply_chars,
            _ => self.config.telegram.max_reply_chars,
        }
    }

    fn chunk_text(&self, channel: &str, text: &str, max_chars: usize) -> Vec<String> {
        match channel {
            "telegram" => self.telegram.chunk_text(text, max_chars),
            _ => vec![text.to_string()],
        }
    }

    async fn send_text(
        &self,
        channel: &str,
        recipient: &str,
        text: &str,
    ) -> AppResult<OutboundSendResult> {
        match channel {
            "telegram" => self.telegram.send_text(recipient, text).await,
            "local" => Ok(OutboundSendResult {
                provider_message_id: None,
                status: "suppressed".to_string(),
            }),
            other => Err(AppError::Validation(format!(
                "unsupported outbound channel: {other}"
            ))),
        }
    }

    async fn enrich_inbound_text(
        &self,
        identity: &crate::identity::compiler::SystemIdentity,
        event: &NormalizedInboundEvent,
        trace_id: &str,
    ) -> AppResult<String> {
        if event.attachments.is_empty() {
            return Ok(event.text.clone());
        }

        let mut sections = Vec::new();
        let raw_text = event.text.trim();
        if !raw_text.is_empty() && !raw_text.starts_with('[') {
            sections.push(format!("Mensaje/caption del usuario: {raw_text}"));
        }

        for (index, attachment) in event.attachments.iter().enumerate() {
            let segment = match attachment.kind {
                InboundAttachmentKind::Image => {
                    self.describe_image_attachment(identity, attachment, trace_id, index + 1)
                        .await?
                }
                InboundAttachmentKind::Audio => {
                    self.transcribe_audio_attachment(identity, attachment, trace_id, index + 1)
                        .await?
                }
            };
            sections.push(segment);
        }

        if sections.is_empty() {
            Ok(event.text.clone())
        } else {
            Ok(sections.join("\n\n"))
        }
    }

    async fn describe_image_attachment(
        &self,
        identity: &crate::identity::compiler::SystemIdentity,
        attachment: &InboundAttachment,
        trace_id: &str,
        ordinal: usize,
    ) -> AppResult<String> {
        let bytes = self
            .telegram
            .download_attachment(&attachment.file_id, 8 * 1024 * 1024)
            .await?;
        let mime_type = attachment
            .mime_type
            .clone()
            .unwrap_or_else(|| "image/jpeg".to_string());
        let data_url = format!(
            "data:{};base64,{}",
            mime_type,
            BASE64_STANDARD.encode(&bytes)
        );
        let broker = ModelBroker::new(self.llm.model_catalog());
        let settings = self.team.runtime_settings();
        let selection = broker.resolve(
            &identity.frontmatter.model_routes,
            ModelSelectionRequest {
                route_key: "vision_understand",
                input_modality: InputModality::Image,
                output_modality: OutputModality::Text,
                reasoning_level: ReasoningLevel::Low,
                requires_tools: false,
                max_cost_usd: Some(0.03),
                max_latency_ms: Some(identity.frontmatter.budgets.timeout_ms),
                performance_policy: settings.performance_policy,
                escalation_tier: settings.max_escalation_tier,
            },
        );
        info!(
            trace_id = %trace_id,
            file_id = %attachment.file_id,
            resolved_model = %selection.resolved_model,
            "processing telegram image attachment"
        );
        let response = self
            .llm
            .chat_completion(LlmRequest {
                model: selection.resolved_model,
                messages: vec![
                    ChatMessage::text(
                        "system",
                        format!(
                            "{}\n\nAnaliza la imagen para el flujo de Telegram. Describe con precisión lo visible, extrae texto legible si existe y resume lo útil para responder al usuario. Responde solo en texto plano y en español si no hay otro idioma claro.",
                            identity.compiled_system_prompt
                        ),
                    ),
                    ChatMessage::parts(
                        "user",
                        vec![
                            ChatMessagePart::Text {
                                text: "Describe esta imagen con foco en detalles útiles para la conversación actual.".to_string(),
                            },
                            ChatMessagePart::ImageUrl { url: data_url },
                        ],
                    ),
                ],
                max_output_tokens: 300,
                temperature: 0.1,
                require_json: false,
                timeout_ms: identity.frontmatter.budgets.timeout_ms,
            })
            .await
            .map_err(map_provider_error)?;

        Ok(format!(
            "Imagen {} procesada: {}",
            ordinal,
            response.content.trim()
        ))
    }

    async fn transcribe_audio_attachment(
        &self,
        identity: &crate::identity::compiler::SystemIdentity,
        attachment: &InboundAttachment,
        trace_id: &str,
        ordinal: usize,
    ) -> AppResult<String> {
        let bytes = self
            .telegram
            .download_attachment(&attachment.file_id, 16 * 1024 * 1024)
            .await?;
        let format = infer_audio_format(attachment)?;
        let broker = ModelBroker::new(self.llm.model_catalog());
        let settings = self.team.runtime_settings();
        let selection = broker.resolve(
            &identity.frontmatter.model_routes,
            ModelSelectionRequest {
                route_key: "audio_transcribe",
                input_modality: InputModality::Audio,
                output_modality: OutputModality::Text,
                reasoning_level: ReasoningLevel::Low,
                requires_tools: false,
                max_cost_usd: Some(0.03),
                max_latency_ms: Some(identity.frontmatter.budgets.timeout_ms),
                performance_policy: settings.performance_policy,
                escalation_tier: settings.max_escalation_tier,
            },
        );
        info!(
            trace_id = %trace_id,
            file_id = %attachment.file_id,
            resolved_model = %selection.resolved_model,
            "processing telegram audio attachment"
        );
        let response = self
            .llm
            .chat_completion(LlmRequest {
                model: selection.resolved_model,
                messages: vec![
                    ChatMessage::text(
                        "system",
                        format!(
                            "{}\n\nTranscribe fielmente el audio. Si detectas partes no claras, marca [inaudible]. Después de la transcripción, agrega una línea final breve que empiece con 'Resumen:' resumiendo la intención del usuario. Responde en texto plano.",
                            identity.compiled_system_prompt
                        ),
                    ),
                    ChatMessage::parts(
                        "user",
                        vec![
                            ChatMessagePart::Text {
                                text: "Transcribe este audio de Telegram y resume su intención.".to_string(),
                            },
                            ChatMessagePart::InputAudio {
                                data: BASE64_STANDARD.encode(&bytes),
                                format,
                            },
                        ],
                    ),
                ],
                max_output_tokens: 500,
                temperature: 0.0,
                require_json: false,
                timeout_ms: identity.frontmatter.budgets.timeout_ms,
            })
            .await
            .map_err(map_provider_error)?;

        Ok(format!(
            "Audio {} procesado: {}",
            ordinal,
            response.content.trim()
        ))
    }
}

fn truncate_for_log(input: &str, max_chars: usize) -> String {
    let mut out = input.chars().take(max_chars).collect::<String>();
    if input.chars().count() > max_chars {
        out.push_str("...");
    }
    out
}

fn map_provider_error(err: ProviderError) -> AppError {
    AppError::Provider(err.to_string())
}

fn should_retry_provider_error(err: &AppError) -> bool {
    match err {
        AppError::Provider(message) => {
            let normalized = message.to_ascii_lowercase();
            normalized.contains("malformed upstream response")
                || normalized.contains("upstream failure")
                || normalized.contains("network")
                || normalized.contains("timeout")
                || normalized.contains("rate limit")
        }
        AppError::Timeout(_) | AppError::Http(_) => true,
        _ => false,
    }
}

fn retry_model_for_route(
    _identity: &crate::identity::compiler::SystemIdentity,
    _route_key: &str,
    _attempted_model: &str,
) -> Option<String> {
    // Paso 1: mantener el runtime fijado al route interno de OpenRouter y evitar
    // escapes silenciosos hacia otros modelos durante retries de parser o proveedor.
    let _ = OPENROUTER_FREE_MODEL;
    None
}

fn decision_is_internal_parse_fallback(decision: &OrchestrationDecision) -> bool {
    matches!(decision.route, DecisionRoute::AskClarification)
        && decision.confidence <= 0.11
        && decision
            .assistant_reply
            .to_ascii_lowercase()
            .contains("internal parsing issue")
}

fn plan_contract_looks_like_parser_fallback(
    contract: &crate::llm::response_types::ExecutionPlanContract,
) -> bool {
    contract.tasks.is_empty()
        && contract
            .assumptions
            .iter()
            .any(|assumption| assumption.contains("planner repair failed"))
}

fn contextual_follow_up_fallback(user_text: &str, context: &TurnContext) -> Option<String> {
    if !looks_like_follow_up(user_text) {
        return None;
    }

    let entities = recent_context_entities(context);
    let entities_block = if entities.is_empty() {
        "el tema anterior".to_string()
    } else if entities.len() == 1 {
        entities[0].clone()
    } else {
        format!(
            "{} y {}",
            entities[..entities.len() - 1].join(", "),
            entities[entities.len() - 1]
        )
    };

    Some(format!(
        "Tomo que sigues sobre {entities_block}. Si quieres, continúo esa comparación sin reiniciar y la bajo por criterios concretos como precio, autonomía, rendimiento, espacio o software."
    ))
}

fn recent_context_entities(context: &TurnContext) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut entities = Vec::new();

    for token in context
        .brain_memories
        .iter()
        .flat_map(|memory| [memory.subject.clone(), memory.what_value.clone()])
        .chain(context.recent_turns.iter().map(|turn| turn.content.clone()))
        .flat_map(|content| extract_named_tokens(&content))
    {
        let normalized = token.to_ascii_lowercase();
        if normalized.len() < 3 || !seen.insert(normalized) {
            continue;
        }
        entities.push(token);
        if entities.len() >= 3 {
            break;
        }
    }

    entities
}

fn extract_named_tokens(text: &str) -> Vec<String> {
    text.split_whitespace()
        .filter_map(|raw| {
            let cleaned = raw
                .trim_matches(|ch: char| !ch.is_alphanumeric())
                .trim()
                .to_string();
            if cleaned.len() < 2 {
                return None;
            }
            let is_named = cleaned.chars().any(|ch| ch.is_uppercase())
                || cleaned.chars().all(|ch| ch.is_ascii_uppercase());
            if !is_named {
                return None;
            }
            match cleaned.as_str() {
                "User" | "Assistant" | "Recent" | "Latest" | "Relevant" => None,
                _ => Some(cleaned),
            }
        })
        .collect()
}

fn effective_turn_performance_policy(
    base: &PerformancePolicy,
    analysis_complexity: &AnalysisComplexity,
    current_data_required: bool,
) -> PerformancePolicy {
    if current_data_required {
        return PerformancePolicy::MaxQuality;
    }

    match analysis_complexity {
        AnalysisComplexity::Deep | AnalysisComplexity::Theoretical
            if !matches!(base, PerformancePolicy::Fast) =>
        {
            PerformancePolicy::MaxQuality
        }
        _ => base.clone(),
    }
}

fn reasoning_tier_label(analysis_complexity: &AnalysisComplexity) -> &'static str {
    match analysis_complexity {
        AnalysisComplexity::Simple => "low",
        AnalysisComplexity::Structured => "medium",
        AnalysisComplexity::Deep => "high",
        AnalysisComplexity::Theoretical => "strict",
    }
}

fn infer_audio_format(attachment: &InboundAttachment) -> AppResult<String> {
    let mime = attachment
        .mime_type
        .clone()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if mime.contains("mpeg") || mime.contains("mp3") {
        return Ok("mp3".to_string());
    }
    if mime.contains("wav") {
        return Ok("wav".to_string());
    }
    if mime.contains("ogg") || mime.contains("oga") || mime.contains("opus") {
        return Ok("ogg".to_string());
    }
    if mime.contains("mp4") || mime.contains("m4a") || mime.contains("aac") {
        return Ok("mp4".to_string());
    }

    if let Some(name) = &attachment.file_name {
        let lower = name.to_ascii_lowercase();
        if lower.ends_with(".mp3") {
            return Ok("mp3".to_string());
        }
        if lower.ends_with(".wav") {
            return Ok("wav".to_string());
        }
        if lower.ends_with(".ogg") || lower.ends_with(".oga") || lower.ends_with(".opus") {
            return Ok("ogg".to_string());
        }
        if lower.ends_with(".m4a") || lower.ends_with(".mp4") {
            return Ok("mp4".to_string());
        }
    }

    Err(AppError::Validation(
        "unsupported telegram audio format".to_string(),
    ))
}

fn fast_path_decision(
    user_text: &str,
    permissions: &crate::identity::schema::IdentityPermissions,
) -> Option<(DecisionRoute, String, &'static str)> {
    let normalized = user_text
        .trim()
        .to_lowercase()
        .replace('á', "a")
        .replace('é', "e")
        .replace('í', "i")
        .replace('ó', "o")
        .replace('ú', "u");
    if normalized.is_empty() {
        return Some((DecisionRoute::Ignore, String::new(), "empty"));
    }

    let is_greeting = [
        "hi",
        "hello",
        "hey",
        "hola",
        "buenas",
        "buen dia",
        "buenos dias",
    ]
    .iter()
    .any(|needle| normalized == *needle || normalized.starts_with(&format!("{needle} ")));
    if is_greeting {
        let summary = capability_summary(permissions);
        return Some((
            DecisionRoute::DirectReply,
            format!(
                "Hola. Soy AI MicroAgents. {summary} Dime la tarea concreta y la tomo desde ahi."
            ),
            "greeting",
        ));
    }

    let wants_capabilities = [
        "que puedes hacer",
        "que podes hacer",
        "resumen de lo que puedes hacer",
        "resumen de lo que podes hacer",
        "que haces",
        "que hace",
        "what can you do",
        "capabilities",
        "ayuda",
        "help",
    ]
    .iter()
    .any(|needle| normalized.contains(needle));
    if wants_capabilities {
        return Some((
            DecisionRoute::DirectReply,
            capability_summary(permissions),
            "capabilities",
        ));
    }

    let wants_status = ["status", "estado", "estas vivo", "sigues ahi", "online"]
        .iter()
        .any(|needle| normalized.contains(needle));
    if wants_status {
        return Some((
            DecisionRoute::DirectReply,
            format!(
                "Estoy operativo por Telegram. {}",
                capability_summary(permissions)
            ),
            "status",
        ));
    }

    let language_correction = [
        "hablame en español",
        "háblame en español",
        "me hablas en español",
        "respóndeme en español",
        "respondeme en español",
    ]
    .iter()
    .any(|needle| normalized.contains(needle));
    let has_substantive_follow_up = [
        "compara",
        "compare",
        "recomienda",
        "recomendacion",
        "recomendación",
        "analiza",
        "resume",
        "cual",
        "cuál",
        "ranking",
    ]
    .iter()
    .any(|needle| normalized.contains(needle));
    if language_correction && !has_substantive_follow_up {
        return Some((
            DecisionRoute::DirectReply,
            "Claro. A partir de ahora te respondo en español y mantengo el contexto de esta conversación.".to_string(),
            "language_correction",
        ));
    }

    None
}

#[derive(Debug, Clone)]
struct PlanningDecision {
    should_plan: bool,
    route_hint: DecisionRoute,
    reason: &'static str,
    prefer_deterministic_plan: bool,
}

fn planning_decision(
    user_text: &str,
    planner_aggressiveness: u8,
    performance_policy: &PerformancePolicy,
) -> PlanningDecision {
    let route_hint = pick_route_hint(user_text);
    let normalized = user_text.trim().to_lowercase();
    if normalized.is_empty() {
        return PlanningDecision {
            should_plan: false,
            route_hint,
            reason: "empty",
            prefer_deterministic_plan: false,
        };
    }

    if (normalized.contains("subagente") || normalized.contains("subagentes"))
        && (normalized.contains("divide")
            || normalized.contains("separa por")
            || normalized.contains("ranking"))
    {
        return PlanningDecision {
            should_plan: true,
            route_hint: DecisionRoute::PlanThenAct,
            reason: "explicit_parallel_decomposition",
            prefer_deterministic_plan: true,
        };
    }

    if looks_like_conversational_correction(&normalized) {
        return PlanningDecision {
            should_plan: false,
            route_hint,
            reason: "follow_up_correction",
            prefer_deterministic_plan: false,
        };
    }

    let explicit_delegation = [
        "sub agente",
        "subagente",
        "subagentes",
        "worker",
        "workers",
        "delegate",
        "deleg",
        "parallel",
        "paralelo",
        "divide",
        "split",
        "descomp",
    ]
    .iter()
    .any(|marker| normalized.contains(marker));
    if explicit_delegation {
        return PlanningDecision {
            should_plan: true,
            route_hint: DecisionRoute::PlanThenAct,
            reason: "explicit_delegation",
            prefer_deterministic_plan: true,
        };
    }

    let market_or_current_data = [
        "al día de hoy",
        "al dia de hoy",
        "hoy",
        "btc",
        "bitcoin",
        "btcusd",
        "btc/usd",
        "subir o bajar",
        "forecast",
        "predic",
        "mercado",
        "precio actual",
        "current price",
        "trading",
        "finanzas",
        "finance",
        "market",
    ]
    .iter()
    .any(|marker| normalized.contains(marker));
    if market_or_current_data {
        return PlanningDecision {
            should_plan: true,
            route_hint: DecisionRoute::PlanThenAct,
            reason: "market_or_current_data",
            prefer_deterministic_plan: true,
        };
    }

    if looks_like_follow_up(&normalized) {
        return PlanningDecision {
            should_plan: false,
            route_hint,
            reason: "follow_up_reference",
            prefer_deterministic_plan: false,
        };
    }

    if looks_like_complex_comparison(&normalized) {
        return PlanningDecision {
            should_plan: true,
            route_hint: DecisionRoute::PlanThenAct,
            reason: "complex_comparison",
            prefer_deterministic_plan: true,
        };
    }

    if matches!(route_hint, DecisionRoute::PlanThenAct) {
        return PlanningDecision {
            should_plan: true,
            route_hint,
            reason: "route_hint_plan",
            prefer_deterministic_plan: false,
        };
    }

    if looks_like_contextual_synthesis(&normalized) {
        return PlanningDecision {
            should_plan: false,
            route_hint: DecisionRoute::DirectReply,
            reason: "contextual_synthesis",
            prefer_deterministic_plan: false,
        };
    }

    if looks_like_simple_comparison(&normalized) {
        return PlanningDecision {
            should_plan: false,
            route_hint,
            reason: "simple_comparison",
            prefer_deterministic_plan: false,
        };
    }

    let multi_step = [
        " y ",
        " and ",
        " luego ",
        " despues ",
        " then ",
        "first",
        "primero",
        "ademas",
        "analiza",
        "analyze",
        "resume",
        "resuelve",
        "investiga",
        "busca",
        "search",
        "find",
        "\n",
        ";",
    ]
    .iter()
    .any(|marker| normalized.contains(marker));
    if multi_step {
        return PlanningDecision {
            should_plan: true,
            route_hint,
            reason: "multi_step_markers",
            prefer_deterministic_plan: false,
        };
    }

    let word_count = normalized.split_whitespace().count();
    let (length_threshold, word_threshold) = match performance_policy {
        PerformancePolicy::Fast => (72, 12),
        PerformancePolicy::BalancedFast => {
            if planner_aggressiveness >= 75 {
                (40, 7)
            } else if planner_aggressiveness <= 30 {
                (64, 10)
            } else {
                (48, 8)
            }
        }
        PerformancePolicy::MaxQuality => (36, 6),
    };
    if normalized.len() >= length_threshold || word_count >= word_threshold {
        return PlanningDecision {
            should_plan: true,
            route_hint,
            reason: "complexity_threshold",
            prefer_deterministic_plan: false,
        };
    }

    PlanningDecision {
        should_plan: false,
        route_hint,
        reason: "simple_request",
        prefer_deterministic_plan: false,
    }
}

fn looks_like_conversational_correction(normalized: &str) -> bool {
    [
        "hablame en español",
        "háblame en español",
        "respondeme en español",
        "respóndeme en español",
        "en español",
        "no no",
        "pero",
        "la comparación era",
        "la comparacion era",
        "los 3 que me pasaste",
        "los tres que me pasaste",
        "el segundo",
        "ese",
        "esos",
        "estos",
        "esas",
    ]
    .iter()
    .any(|needle| normalized.contains(needle))
}

fn looks_like_simple_comparison(normalized: &str) -> bool {
    let comparison_marker = normalized.contains("compara")
        || normalized.contains("compare")
        || normalized.contains("comparación")
        || normalized.contains("comparacion");
    if !comparison_marker {
        return false;
    }

    let tool_or_market_marker = [
        "al día de hoy",
        "al dia de hoy",
        "hoy",
        "busca",
        "search",
        "investiga",
        "precio actual",
        "current price",
        "btc",
        "bitcoin",
        "mercado",
        "trading",
        "forecast",
    ]
    .iter()
    .any(|needle| normalized.contains(needle));
    if tool_or_market_marker {
        return false;
    }

    let word_count = normalized.split_whitespace().count();
    word_count <= 24 && normalized.len() <= 180
}

fn looks_like_contextual_synthesis(normalized: &str) -> bool {
    let refers_to_prior_context = [
        "todo lo anterior",
        "todo lo que vimos",
        "con eso",
        "en base a eso",
        "de lo anterior",
        "los 3 que me pasaste",
        "los tres que me pasaste",
    ]
    .iter()
    .any(|needle| normalized.contains(needle));

    let synthesis_request = [
        "recomendacion",
        "recomendación",
        "conservadora",
        "balanceada",
        "divertida",
        "sintetiza",
        "sintesis",
        "síntesis",
        "conclusion",
        "conclusión",
        "arma una recomendacion",
        "arma una recomendación",
    ]
    .iter()
    .any(|needle| normalized.contains(needle));

    refers_to_prior_context && synthesis_request
}

fn looks_like_topic_shift(
    current_text: &str,
    prior_turns: &[crate::storage::ConversationTurn],
) -> bool {
    if prior_turns.is_empty() {
        return false;
    }

    let current_tokens = topical_tokens(current_text);
    if current_tokens.len() < 4 {
        return false;
    }

    let previous_tokens = prior_turns
        .iter()
        .flat_map(|turn| topical_tokens(&turn.content))
        .collect::<std::collections::HashSet<_>>();
    if previous_tokens.is_empty() {
        return false;
    }

    let overlap = current_tokens
        .iter()
        .filter(|token| previous_tokens.contains(*token))
        .count();
    let overlap_ratio = overlap as f64 / current_tokens.len() as f64;

    overlap_ratio <= 0.25
}

fn looks_like_complex_comparison(normalized: &str) -> bool {
    let comparison_marker = normalized.contains("compara")
        || normalized.contains("compare")
        || normalized.contains("comparación")
        || normalized.contains("comparacion")
        || normalized.contains("ranking")
        || normalized.contains("rank");
    if !comparison_marker {
        return false;
    }

    let criteria_markers = [
        "motor",
        "consumo",
        "reventa",
        "repuestos",
        "confiabilidad",
        "fiabilidad",
        "diversion",
        "diversión",
        "costo",
        "coste",
        "riesgo",
        "seguridad",
        "mantenimiento",
        "valor",
    ];
    let criteria_hits = criteria_markers
        .iter()
        .filter(|needle| normalized.contains(**needle))
        .count();
    let separator_hits = normalized.matches(',').count()
        + normalized.matches(';').count()
        + normalized.matches(" y ").count();
    let entity_hits = [
        "toyota", "honda", "hyundai", "nissan", "mazda", "civic", "corolla",
    ]
    .iter()
    .filter(|needle| normalized.contains(**needle))
    .count();

    normalized.contains("ranking")
        || criteria_hits >= 3
        || (criteria_hits >= 2 && separator_hits >= 4)
        || (criteria_hits >= 2 && entity_hits >= 3 && normalized.len() > 140)
}

fn topical_tokens(text: &str) -> std::collections::HashSet<String> {
    const STOPWORDS: &[&str] = &[
        "para",
        "entre",
        "sobre",
        "donde",
        "cuando",
        "desde",
        "hasta",
        "luego",
        "ademas",
        "además",
        "quiero",
        "quieres",
        "puedes",
        "puedo",
        "hacer",
        "hagas",
        "analisis",
        "análisis",
        "recomendacion",
        "recomendación",
        "resumen",
        "final",
        "uruguay",
        "decidir",
        "conviene",
        "varios",
        "muchos",
        "subagentes",
        "subagente",
        "workers",
        "worker",
        "task",
        "tarea",
        "tareas",
        "lanzamiento",
        "luego",
        "integra",
        "todo",
        "anterior",
        "ahora",
        "esto",
        "estos",
        "esas",
        "esos",
        "como",
        "porque",
        "which",
        "with",
        "that",
        "this",
        "from",
        "have",
        "will",
        "into",
        "then",
        "used",
        "usados",
    ];

    text.to_lowercase()
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|token| token.len() >= 4)
        .filter(|token| !STOPWORDS.iter().any(|stop| stop == token))
        .map(ToOwned::to_owned)
        .collect()
}

fn deterministic_market_tool_decision(
    user_text: &str,
    permissions_config: &crate::identity::schema::IdentityPermissions,
    allowlisted_domains: &[String],
) -> Option<OrchestrationDecision> {
    if !permissions::is_skill_allowed(permissions_config, "http.fetch") {
        return None;
    }

    let normalized = user_text.trim().to_lowercase();
    let current_data_markers = [
        "al día de hoy",
        "al dia de hoy",
        "hoy",
        "precio actual",
        "current price",
        "cotiza",
        "subir o bajar",
        "forecast",
        "predic",
        "market",
        "mercado",
    ]
    .iter()
    .any(|needle| normalized.contains(needle));
    if !current_data_markers {
        return None;
    }

    let coingecko_allowed = allowlisted_domains
        .iter()
        .any(|host| host.eq_ignore_ascii_case("api.coingecko.com"));
    if !coingecko_allowed {
        return None;
    }

    let (asset_id, vs_currency) = if normalized.contains("btc")
        || normalized.contains("bitcoin")
        || normalized.contains("btc/usd")
        || normalized.contains("btcusd")
    {
        ("bitcoin", "usd")
    } else if normalized.contains("eth") || normalized.contains("ethereum") {
        ("ethereum", "usd")
    } else if normalized.contains("sol") || normalized.contains("solana") {
        ("solana", "usd")
    } else {
        return None;
    };

    let url = format!(
        "https://api.coingecko.com/api/v3/simple/price?ids={asset_id}&vs_currencies={vs_currency}&include_24hr_change=true&include_market_cap=true&include_24hr_vol=true"
    );
    Some(OrchestrationDecision {
        route: DecisionRoute::ToolUse,
        assistant_reply: String::new(),
        tool_calls: vec![ToolCall {
            name: "http.fetch".to_string(),
            arguments: json!({
                "url": url,
                "method": "GET",
                "timeout_ms": 4500,
            }),
        }],
        memory_writes: Vec::new(),
        should_summarize: false,
        confidence: 0.94,
        safe_to_send: true,
    })
}

#[cfg(test)]
fn expand_follow_up_text(
    user_text: &str,
    prior_turns: &[crate::storage::ConversationTurn],
) -> String {
    let trimmed = user_text.trim();
    if trimmed.is_empty() || !looks_like_follow_up(trimmed) || prior_turns.is_empty() {
        return user_text.to_string();
    }

    let context_window = prior_turns
        .iter()
        .rev()
        .filter(|turn| !turn.content.trim().is_empty())
        .take(10)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|turn| format!("{}: {}", turn.role, turn.content.trim()))
        .collect::<Vec<_>>()
        .join("\n");

    if context_window.is_empty() {
        return user_text.to_string();
    }

    format!(
        "Conversation context (resolve references from here before asking again):\n{context_window}\n\nFollow-up user message:\n{trimmed}"
    )
}

fn looks_like_follow_up(user_text: &str) -> bool {
    let normalized = user_text.trim().to_lowercase();
    let normalized = normalized
        .trim_start_matches(|c: char| !c.is_alphanumeric())
        .to_string();
    if normalized.is_empty() || looks_like_plain_ack_or_greeting(&normalized) {
        return false;
    }

    let explicit_markers = [
        "el nombre",
        "la persona",
        "esta persona",
        "no no",
        "pero",
        "sus autos",
        "sus modelos",
        "los 3",
        "las 3",
        "que me pasaste",
        "que me dijiste",
        "me hablas en español",
        "hablame en español",
        "háblame en español",
        "la comparación era",
        "la comparacion era",
    ]
    .iter()
    .any(|marker| normalized == *marker || normalized.starts_with(&format!("{marker} ")));
    if explicit_markers {
        return true;
    }

    let possessive_follow_up = [
        "compara sus",
        "compare their",
        "sus autos",
        "sus modelos",
        "sus opciones",
    ]
    .iter()
    .any(|marker| normalized.starts_with(marker) || normalized.contains(marker));
    if possessive_follow_up {
        return true;
    }

    let word_count = normalized.split_whitespace().count();
    let question_reference = [
        "cual es el",
        "cuál es el",
        "cual es la",
        "cuál es la",
        "y cual",
        "y cuál",
    ]
    .iter()
    .any(|marker| normalized == *marker || normalized.starts_with(marker));
    if question_reference && word_count <= 6 {
        return true;
    }

    if word_count > 4 {
        return false;
    }

    [
        "ese", "esa", "eso", "estos", "estas", "esos", "esas", "este", "esta", "el ", "la ",
        "los ", "las ", "segundo", "tercero", "primero", "anterior", "mismo",
    ]
    .iter()
    .any(|marker| normalized == *marker || normalized.starts_with(marker))
}

fn looks_like_plain_ack_or_greeting(normalized: &str) -> bool {
    [
        "hola",
        "hola otra vez",
        "hi",
        "hello",
        "hey",
        "buenas",
        "buen dia",
        "buenos dias",
        "gracias",
        "muchas gracias",
        "ok",
        "okay",
        "dale",
        "perfecto",
        "listo",
        "entendido",
    ]
    .iter()
    .any(|marker| normalized == *marker || normalized.starts_with(&format!("{marker} ")))
}

fn capability_summary(permissions: &crate::identity::schema::IdentityPermissions) -> String {
    let mut capabilities = vec![
        "Puedo responder preguntas y resumir contenido.".to_string(),
        "Tambien puedo coordinar subtareas, revisar resultados e integrar una respuesta final."
            .to_string(),
    ];

    if skill_allowed(permissions, "memory.search") || skill_allowed(permissions, "memory.write") {
        capabilities
            .push("Tengo memoria operativa para guardar y recuperar contexto util.".to_string());
    }
    if skill_allowed(permissions, "reminders.create")
        || skill_allowed(permissions, "reminders.list")
    {
        capabilities.push("Puedo crear y listar recordatorios.".to_string());
    }
    if skill_allowed(permissions, "http.fetch") {
        capabilities.push("Puedo consultar HTTP cuando la politica lo permite.".to_string());
    }
    if skill_allowed(permissions, "quality.verify") {
        capabilities.push(
            "Tambien paso resultados por verificacion antes de cerrar tareas delicadas."
                .to_string(),
        );
    }

    capabilities.join(" ")
}

fn skill_allowed(
    permissions: &crate::identity::schema::IdentityPermissions,
    skill_name: &str,
) -> bool {
    let denied = permissions
        .denied_skills
        .iter()
        .any(|name| name.eq_ignore_ascii_case(skill_name));
    if denied {
        return false;
    }

    permissions
        .allowed_skills
        .iter()
        .any(|name| name == "*" || name.eq_ignore_ascii_case(skill_name))
}

async fn collect_hints(conversation_id: i64, store: &Store) -> Vec<String> {
    store
        .recent_turns(conversation_id, 6)
        .await
        .map(|turns| turns.into_iter().map(|t| t.content).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use crate::identity::schema::IdentityPermissions;
    use crate::llm::response_types::DecisionRoute;
    use crate::memory::{
        BrainMemory, BrainMemoryKind, BrainMemoryProvenance, BrainMemoryStatus, BrainScopeKind,
    };
    use crate::orchestrator::context::TurnContext;
    use crate::team::config::PerformancePolicy;

    use super::{
        capability_summary, contextual_follow_up_fallback, decision_is_internal_parse_fallback,
        expand_follow_up_text, fast_path_decision, looks_like_follow_up, planning_decision,
        retry_model_for_route,
    };

    fn permissions() -> IdentityPermissions {
        IdentityPermissions {
            allowed_skills: vec![
                "*".to_string(),
                "memory.search".to_string(),
                "memory.write".to_string(),
                "reminders.create".to_string(),
                "quality.verify".to_string(),
            ],
            denied_skills: vec![],
        }
    }

    #[test]
    fn fast_path_handles_greeting() {
        let decision = fast_path_decision("Hola", &permissions()).expect("fast path");
        assert_eq!(decision.0, DecisionRoute::DirectReply);
        assert!(decision.1.contains("AI MicroAgents"));
    }

    #[test]
    fn fast_path_handles_capability_queries() {
        let decision = fast_path_decision("Dame un resumen de lo que puedes hacer", &permissions())
            .expect("fast path");
        assert_eq!(decision.0, DecisionRoute::DirectReply);
        assert!(decision.1.contains("recordatorios"));
    }

    #[test]
    fn language_correction_does_not_swallow_real_request() {
        let decision = fast_path_decision(
            "No no, háblame en español y compara los 3 autos que me pasaste",
            &permissions(),
        );
        assert!(decision.is_none());
    }

    #[test]
    fn capability_summary_respects_permissions() {
        let summary = capability_summary(&permissions());
        assert!(summary.contains("memoria operativa"));
        assert!(summary.contains("recordatorios"));
        assert!(summary.contains("verificacion"));
    }

    #[test]
    fn planning_trigger_detects_composite_requests() {
        assert!(
            planning_decision(
                "Analiza el problema, compara opciones y dame un plan de accion",
                60,
                &PerformancePolicy::BalancedFast,
            )
            .should_plan
        );
        assert!(!planning_decision("de barra", 60, &PerformancePolicy::BalancedFast).should_plan);
        assert!(
            !planning_decision(
                "Compara Toyota Corolla, Honda Civic y Hyundai Elantra por motor y diversion",
                60,
                &PerformancePolicy::BalancedFast,
            )
            .should_plan
        );
        assert!(
            planning_decision(
                "Divide entre varios subagentes un análisis para elegir entre Toyota Corolla, Honda Civic y Hyundai Elantra usados en Uruguay. Separa por consumo, reventa, repuestos, confiabilidad, diversión, costo total y riesgo, y dame un ranking final.",
                60,
                &PerformancePolicy::BalancedFast,
            )
            .should_plan
        );
        assert!(
            !planning_decision(
                "Ahora toma todo lo anterior y arma una recomendación conservadora, una balanceada y una divertida, explicando por qué.",
                60,
                &PerformancePolicy::BalancedFast,
            )
            .should_plan
        );
    }

    #[test]
    fn follow_up_short_text_gets_expanded() {
        let turns = vec![
            crate::storage::ConversationTurn {
                role: "user".to_string(),
                content: "Pedirle al sub agente 1 que busque en Google el Nobel 2024".to_string(),
                created_at: chrono::Utc::now(),
            },
            crate::storage::ConversationTurn {
                role: "assistant".to_string(),
                content: "Que informacion especifica necesitas?".to_string(),
                created_at: chrono::Utc::now(),
            },
        ];
        assert!(looks_like_follow_up("El nombre"));
        let expanded = expand_follow_up_text("El nombre", &turns);
        assert!(expanded
            .contains("Conversation context (resolve references from here before asking again):"));
        assert!(
            expanded.contains("user: Pedirle al sub agente 1 que busque en Google el Nobel 2024")
        );
        assert!(expanded.contains("Follow-up user message"));
    }

    #[test]
    fn greetings_do_not_count_as_follow_ups() {
        assert!(!looks_like_follow_up("Hola"));
        assert!(!looks_like_follow_up("Hola otra vez"));
        assert!(!looks_like_follow_up("Gracias"));
        assert!(!looks_like_follow_up("Ok"));
    }

    #[test]
    fn short_reference_questions_count_as_follow_ups() {
        assert!(looks_like_follow_up("¿Cuál es el más divertido?"));
    }

    #[test]
    fn contextual_follow_up_fallback_mentions_recent_entities() {
        let context = TurnContext {
            conversation_id: 1,
            trace_id: "trace".to_string(),
            recent_turns: vec![
                crate::storage::ConversationTurn {
                    role: "user".to_string(),
                    content: "Hazme una comparacion entre Tesla y BYD".to_string(),
                    created_at: Utc::now(),
                },
                crate::storage::ConversationTurn {
                    role: "assistant".to_string(),
                    content: "Tesla destaca por software y BYD por integracion vertical."
                        .to_string(),
                    created_at: Utc::now(),
                },
            ],
            latest_summary: None,
            brain_memories: vec![BrainMemory {
                id: 1,
                scope_kind: BrainScopeKind::Conversation,
                user_id: Some("u1".to_string()),
                conversation_id: Some(1),
                memory_kind: BrainMemoryKind::Goal,
                memory_key: "goal.compare_tesla_byd".to_string(),
                subject: "compare_tesla_byd".to_string(),
                what_value: "Comparar Tesla y BYD".to_string(),
                why_value: None,
                where_context: None,
                learned_value: None,
                provenance: BrainMemoryProvenance::default(),
                confidence: 0.8,
                status: BrainMemoryStatus::Active,
                superseded_by: None,
                source_turn_id: Some(1),
                created_at: Utc::now(),
                updated_at: Utc::now(),
            }],
            memories: vec![],
            working_set: crate::usecase::ConversationWorkingSet::default(),
            current_evidence: None,
            selected_skills: vec![],
            performance_policy: crate::team::config::PerformancePolicy::BalancedFast,
            max_escalation_tier: crate::team::config::EscalationTier::Standard,
            analysis_complexity: crate::usecase::AnalysisComplexity::Simple,
        };

        let fallback = contextual_follow_up_fallback("Compara sus autos mas tops", &context)
            .expect("fallback");
        assert!(fallback.contains("Tesla"));
        assert!(fallback.contains("BYD"));
    }

    #[test]
    fn parser_safe_fallback_is_detected() {
        let decision = crate::llm::response_types::OrchestrationDecision::safe_fallback(
            "I hit an internal parsing issue. Can you rephrase in one sentence?",
        );
        assert!(decision_is_internal_parse_fallback(&decision));
    }

    #[test]
    fn retry_model_is_disabled_when_runtime_is_forced_to_openrouter_free() {
        let identity = crate::identity::compiler::SystemIdentity {
            frontmatter: crate::identity::schema::IdentityFrontmatter {
                id: "ai-microagents".to_string(),
                display_name: "AI MicroAgents".to_string(),
                description: "test".to_string(),
                locale: "es-UY".to_string(),
                timezone: "UTC".to_string(),
                model_routes: crate::identity::schema::ModelRoutes {
                    fast: crate::llm::OPENROUTER_FREE_MODEL.to_string(),
                    reasoning: crate::llm::OPENROUTER_FREE_MODEL.to_string(),
                    tool_use: crate::llm::OPENROUTER_FREE_MODEL.to_string(),
                    vision: crate::llm::OPENROUTER_FREE_MODEL.to_string(),
                    reviewer: crate::llm::OPENROUTER_FREE_MODEL.to_string(),
                    planner: crate::llm::OPENROUTER_FREE_MODEL.to_string(),
                    router_fast: None,
                    fast_text: None,
                    reviewer_fast: None,
                    reviewer_strict: None,
                    integrator_complex: None,
                    vision_understand: None,
                    audio_transcribe: None,
                    image_generate: None,
                    fallback: vec![crate::llm::OPENROUTER_FREE_MODEL.to_string()],
                },
                budgets: crate::identity::schema::IdentityBudgets {
                    max_steps: 3,
                    max_turn_cost_usd: 1.0,
                    max_input_tokens: 1000,
                    max_output_tokens: 500,
                    max_tool_calls: 2,
                    timeout_ms: 10_000,
                },
                memory: crate::identity::schema::IdentityMemory {
                    save_facts: true,
                    save_summaries: true,
                    summarize_every_n_turns: 4,
                    brain_enabled: true,
                    precheck_each_turn: true,
                    auto_write_mode: "aggressive".to_string(),
                    conversation_limit: 4,
                    user_limit: 4,
                },
                permissions: permissions(),
                channels: crate::identity::schema::IdentityChannels {
                    telegram: crate::identity::schema::TelegramIdentityChannel {
                        enabled: true,
                        max_reply_chars: 3500,
                        style_overrides: "concise".to_string(),
                    },
                },
            },
            sections: crate::identity::compiler::CompiledIdentitySections {
                mission: String::new(),
                persona: String::new(),
                tone: String::new(),
                hard_rules: String::new(),
                do_not_do: String::new(),
                escalation: String::new(),
                memory_preferences: String::new(),
                channel_notes: String::new(),
                planning_principles: String::new(),
                review_standards: String::new(),
            },
            compiled_system_prompt: String::new(),
        };

        let retry = retry_model_for_route(
            &identity,
            "fast_text",
            "nvidia/nemotron-3-nano-30b-a3b:free",
        );
        assert!(retry.is_none());
    }
}
