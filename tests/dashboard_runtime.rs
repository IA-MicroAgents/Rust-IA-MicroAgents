mod support;

use std::sync::{
    atomic::{AtomicI64, Ordering},
    Arc,
};

use ai_microagents::{
    app::runtime::SharedAppState,
    channel::telegram::TelegramClient,
    config::{
        AppConfig, DashboardConfig, OpenRouterConfig, PolicyConfig, RuntimeConfig, TelegramConfig,
    },
    http::server::build_router,
    identity::IdentityManager,
    llm::{
        models::{ModelCapabilities, ModelMetadata},
        LlmProvider, LlmRequest, LlmResponse, ProviderResult, Usage,
    },
    orchestrator::Orchestrator,
    skills::SkillRegistry,
    storage::Store,
    team::{config::SubagentMode, supervisor::SupervisorControls, TeamConfig, TeamManager},
    telemetry::event_bus::EventBus,
};
use async_trait::async_trait;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use serde_json::json;
use tokio::sync::mpsc;
use tower::ServiceExt;

#[derive(Clone)]
struct StubProvider;

#[async_trait]
impl LlmProvider for StubProvider {
    async fn validate_models(
        &self,
        _routes: &ai_microagents::identity::schema::ModelRoutes,
    ) -> ProviderResult<Vec<ModelCapabilities>> {
        Ok(vec![])
    }

    async fn chat_completion(&self, _request: LlmRequest) -> ProviderResult<LlmResponse> {
        Ok(LlmResponse {
            model: "stub".to_string(),
            content: json!({
                "route": "direct_reply",
                "assistant_reply": "ok",
                "tool_calls": [],
                "memory_writes": [],
                "should_summarize": false,
                "confidence": 0.9,
                "safe_to_send": true
            })
            .to_string(),
            usage: Usage::default(),
            latency_ms: 1,
        })
    }

    fn model_catalog(&self) -> Vec<ModelMetadata> {
        vec![]
    }
}

#[tokio::test]
async fn dashboard_auth_rejects_missing_token() {
    let state = build_state("secret").await;
    let app = build_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/dashboard")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn dashboard_sse_works_with_token() {
    let state = build_state("secret").await;
    let app = build_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/events/stream")
                .header("x-ai-microagents-dashboard-token", "secret")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert!(content_type.starts_with("text/event-stream"));
}

async fn build_state(auth_token: &str) -> SharedAppState {
    let base = std::env::temp_dir().join(format!(
        "ai-microagents-dashboard-test-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&base).expect("base dir");
    let identity_path = base.join("IDENTITY.md");
    let skills_dir = base.join("skills");
    std::fs::create_dir_all(&skills_dir).expect("skills");
    let backend = support::TestBackend::new("dashboard_runtime");

    write_identity(&identity_path);
    write_skill(&skills_dir);

    let cfg = AppConfig {
        bind_addr: "127.0.0.1:0".to_string(),
        database: backend.database.clone(),
        cache: backend.cache.clone(),
        identity_path: identity_path.clone(),
        skills_dir: skills_dir.clone(),
        bus: ai_microagents::config::BusConfig {
            enabled: true,
            stream_prefix: "ai-microagents-test".to_string(),
            stream_maxlen: 2000,
            outbox_publish_batch: 32,
            outbox_poll_ms: 200,
            outbox_max_retries: 4,
            stream_reclaim_idle_ms: 60_000,
            consumer_name: "dashboard-runtime".to_string(),
            memory_consumer_concurrency: 1,
            jobs_consumer_concurrency: 1,
        },
        openrouter: OpenRouterConfig {
            api_key: "dummy".to_string(),
            base_url: "http://127.0.0.1:1".to_string(),
            app_name: None,
            site_url: None,
            timeout_ms: 1000,
            validate_models_on_start: false,
            mock_mode: true,
        },
        telegram: TelegramConfig {
            enabled: false,
            bot_token: String::new(),
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
        policy: PolicyConfig {
            outbound_enabled: false,
            dry_run: true,
            http_skill_allowlist: vec![],
            outbound_kill_switch: true,
        },
        runtime: RuntimeConfig {
            queue_capacity: 8,
            worker_concurrency: 1,
            reminder_poll_ms: 1000,
        },
        team: TeamConfig {
            team_size: 2,
            max_parallel_tasks: 2,
            allow_ephemeral_subagents: true,
            max_ephemeral_subagents: 4,
            subagent_mode: SubagentMode::Generalist,
            subagent_roleset: vec!["researcher".to_string(), "integrator".to_string()],
            subagent_profile_path: None,
            supervisor_review_interval_ms: 500,
            max_review_loops_per_task: 2,
            max_task_retries: 2,
            plan_max_tasks: 8,
            plan_max_depth: 3,
            performance_policy: ai_microagents::team::config::PerformancePolicy::BalancedFast,
            planner_aggressiveness: 60,
            max_escalation_tier: ai_microagents::team::config::EscalationTier::Standard,
            typing_delay_ms: 800,
            require_final_review: true,
            progress_updates_enabled: false,
            progress_update_threshold_ms: 1000,
        },
        dashboard: DashboardConfig {
            enable_dashboard: true,
            bind_addr: "127.0.0.1:0".to_string(),
            auth_token: auth_token.to_string(),
        },
    };

    let store = Store::from_config(&cfg).await.expect("store");
    let identity = IdentityManager::load(identity_path).expect("identity");
    let skills = SkillRegistry::load(skills_dir).expect("skills");
    let llm: Arc<dyn LlmProvider> = Arc::new(StubProvider);
    let telegram = TelegramClient::new(cfg.telegram.clone()).expect("telegram");
    let team = TeamManager::new(cfg.team.clone(), &identity.get())
        .await
        .expect("team");
    let controls = SupervisorControls::default();
    controls.set_outbound_kill_switch(cfg.policy.outbound_kill_switch);
    let events = EventBus::new(store.clone());
    let orchestrator = Arc::new(
        Orchestrator::new(
            cfg.clone(),
            store.clone(),
            identity.clone(),
            skills.clone(),
            llm.clone(),
            telegram.clone(),
            team.clone(),
            controls.clone(),
            events.clone(),
        )
        .expect("orchestrator"),
    );

    let (queue_tx, mut queue_rx) = mpsc::channel(8);
    let queue_depth = Arc::new(AtomicI64::new(0));
    let worker = orchestrator.clone();
    let queue_depth_worker = queue_depth.clone();
    tokio::spawn(async move {
        while let Some(event) = queue_rx.recv().await {
            queue_depth_worker.fetch_sub(1, Ordering::SeqCst);
            let _ = worker.process_inbound_event(event).await;
        }
    });

    SharedAppState {
        config: cfg,
        store,
        identity,
        skills,
        llm,
        telegram,
        team,
        controls,
        events,
        orchestrator,
        queue_tx,
        queue_depth,
    }
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
Use bounded decomposition with explicit dependencies.
## Review Standards
Reject outputs that miss acceptance criteria.
"#;
    std::fs::write(path, content).expect("write identity");
}

fn write_skill(skills_root: &std::path::Path) {
    let folder = skills_root.join("agent_status");
    std::fs::create_dir_all(&folder).expect("skill folder");
    let content = r#"---
name: agent.status
version: 1.0.0
description: status skill
kind: builtin
entrypoint: agent.status
input_schema:
  type: object
output_schema:
  type: object
permissions: []
timeout_ms: 1000
max_retries: 0
cache_ttl_secs: 0
idempotent: true
side_effects: none
tags: [agent]
triggers: [status]
---
## What it does
Returns status.
## When to use
When user asks state.
## When NOT to use
When side effects are needed.
## Input notes
No required fields.
## Output notes
Returns object.
## Failure handling
Return validation error.
## Examples
{}
"#;
    std::fs::write(folder.join("SKILL.md"), content).expect("write skill");
}
