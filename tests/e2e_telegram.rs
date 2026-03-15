mod support;

use std::sync::Arc;

use ferrum::{
    channel::telegram::TelegramClient,
    config::{
        AppConfig, CacheConfig, DashboardConfig, DatabaseConfig, OpenRouterConfig, PolicyConfig,
        RuntimeConfig, TelegramConfig,
    },
    identity::IdentityManager,
    llm::{openrouter::OpenRouterClient, LlmProvider},
    orchestrator::Orchestrator,
    skills::SkillRegistry,
    storage::Store,
    team::{config::SubagentMode, supervisor::SupervisorControls, TeamConfig, TeamManager},
    telemetry::event_bus::EventBus,
};
use serde_json::json;
use tempfile::tempdir;
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

#[tokio::test]
async fn incoming_telegram_update_produces_persisted_reply() {
    let temp = tempdir().expect("tempdir");
    let identity_path = temp.path().join("IDENTITY.md");
    let skills_dir = temp.path().join("skills");
    std::fs::create_dir_all(&skills_dir).expect("skills dir");
    write_identity(&identity_path);
    write_builtin_skill(&skills_dir, "agent.status");

    let openrouter_server = MockServer::start().await;
    let telegram_server = MockServer::start().await;
    let backend = support::TestBackend::new("e2e_telegram");

    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(models_payload()))
        .mount(&openrouter_server)
        .await;

    let decision_json = json!({
        "route": "direct_reply",
        "assistant_reply": "Hola desde Telegram",
        "tool_calls": [],
        "memory_writes": [],
        "should_summarize": false,
        "confidence": 0.9,
        "safe_to_send": true
    })
    .to_string();

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "model-fast",
            "choices": [{"message": {"content": decision_json}}],
            "usage": {"prompt_tokens": 10, "completion_tokens": 8, "cost": 0.001}
        })))
        .mount(&openrouter_server)
        .await;

    Mock::given(path("/bottest-token/GetUpdates"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "result": [{
                "update_id": 9001,
                "message": {
                    "message_id": 77,
                    "date": 1700000000,
                    "chat": {"id": 123456, "type": "private", "username": "alice"},
                    "from": {"id": 123456, "is_bot": false, "username": "alice", "first_name": "Alice", "language_code": "en"},
                    "text": "hola ferrum"
                }
            }]
        })))
        .mount(&telegram_server)
        .await;

    Mock::given(path("/bottest-token/SendMessage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "result": {
                "message_id": 88,
                "date": 1700000001,
                "chat": {"id": 123456, "type": "private", "username": "alice"},
                "text": "Hola desde Telegram"
            }
        })))
        .mount(&telegram_server)
        .await;

    let cfg = test_config(
        backend.database.clone(),
        backend.cache.clone(),
        identity_path.clone(),
        skills_dir.clone(),
        &openrouter_server.uri(),
        &telegram_server.uri(),
    );

    let store = Store::from_config(&cfg).await.expect("store");
    let identity = IdentityManager::load(identity_path).expect("identity");
    let skills = SkillRegistry::load(skills_dir).expect("skills");
    let provider = OpenRouterClient::new(cfg.openrouter.clone()).expect("provider");
    provider
        .validate_models(&identity.get().frontmatter.model_routes)
        .await
        .expect("validate models");
    let llm: Arc<dyn LlmProvider> = Arc::new(provider);

    let telegram = TelegramClient::new(cfg.telegram.clone()).expect("telegram");
    let team = TeamManager::new(cfg.team.clone(), &identity.get())
        .await
        .expect("team");
    let controls = SupervisorControls::default();
    controls.set_outbound_kill_switch(cfg.policy.outbound_kill_switch);
    let events = EventBus::new(store.clone());
    let orchestrator = Orchestrator::new(
        cfg.clone(),
        store.clone(),
        identity.clone(),
        skills.clone(),
        llm.clone(),
        telegram.clone(),
        team,
        controls,
        events,
    )
    .expect("orchestrator");

    let updates = telegram.poll_updates().await.expect("poll updates");
    assert_eq!(updates.len(), 1);
    let event = updates.into_iter().next().expect("telegram event");
    let wrapper = json!({
        "raw": event.raw_payload.clone(),
        "normalized": event.clone(),
    });
    store
        .insert_inbound_event(&event.event_id, "telegram", &wrapper)
        .await
        .expect("insert inbound")
        .expect("new event");

    let outcome = orchestrator
        .process_inbound_event(event.clone())
        .await
        .expect("process event");

    assert_eq!(outcome.sent_chunks, 1);
    let mut status = None;
    for _ in 0..30 {
        status = store
            .inbound_event_status(&event.event_id)
            .await
            .expect("event status");
        if status.as_deref() == Some("processed") {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(status, Some("processed".to_string()));
    let requests = telegram_server
        .received_requests()
        .await
        .expect("recorded requests");
    assert!(requests
        .iter()
        .any(|req| req.url.path() == "/bottest-token/SendMessage"));
}

fn write_identity(path: &std::path::Path) {
    let content = r#"---
id: ferrum-test
display_name: Ferrum Test
description: Deterministic Telegram orchestrator
locale: en-US
timezone: UTC
model_routes:
  fast: model-fast
  reasoning: model-reasoning
  tool_use: model-tools
  vision: model-vision
  reviewer: model-reviewer
  planner: model-planner
  fallback: [model-fast]
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
Keep Telegram replies useful and compact.
## Planning Principles
Decompose into bounded tasks with explicit dependencies.
## Review Standards
Reject outputs that do not satisfy acceptance criteria.
"#;
    std::fs::write(path, content).expect("write identity");
}

fn write_builtin_skill(skills_root: &std::path::Path, skill_name: &str) {
    let folder = skills_root.join(skill_name);
    std::fs::create_dir_all(&folder).expect("skill folder");
    let content = format!(
        r#"---
name: {skill_name}
version: 1.0.0
description: builtin skill
kind: builtin
entrypoint: {skill_name}
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
Returns a builtin response.
## When to use
Use for test coverage.
## When NOT to use
Never for side effects.
## Input notes
No inputs.
## Output notes
Returns json.
## Failure handling
Should not fail.
## Examples
{{}}
"#
    );
    std::fs::write(folder.join("SKILL.md"), content).expect("write skill");
}

fn models_payload() -> serde_json::Value {
    json!({
        "data": [
            {"id": "model-fast", "context_length": 128000, "modality": ["text"]},
            {"id": "model-reasoning", "context_length": 128000, "modality": ["text"]},
            {"id": "model-tools", "context_length": 128000, "modality": ["text"]},
            {"id": "model-vision", "context_length": 128000, "modality": ["text", "image"]},
            {"id": "model-reviewer", "context_length": 128000, "modality": ["text"]},
            {"id": "model-planner", "context_length": 128000, "modality": ["text"]}
        ]
    })
}

fn test_config(
    database: DatabaseConfig,
    cache: CacheConfig,
    identity_path: std::path::PathBuf,
    skills_dir: std::path::PathBuf,
    openrouter_base_url: &str,
    telegram_base_url: &str,
) -> AppConfig {
    AppConfig {
        bind_addr: "127.0.0.1:0".to_string(),
        database,
        cache,
        bus: ferrum::config::BusConfig {
            enabled: true,
            stream_prefix: "ferrum-test".to_string(),
            stream_maxlen: 2000,
            outbox_publish_batch: 32,
            outbox_poll_ms: 200,
            outbox_max_retries: 4,
            stream_reclaim_idle_ms: 60_000,
            consumer_name: "e2e-telegram".to_string(),
            memory_consumer_concurrency: 1,
            jobs_consumer_concurrency: 1,
        },
        identity_path,
        skills_dir,
        openrouter: OpenRouterConfig {
            api_key: "test-key".to_string(),
            base_url: openrouter_base_url.to_string(),
            app_name: None,
            site_url: None,
            timeout_ms: 5000,
            validate_models_on_start: true,
            mock_mode: false,
        },
        telegram: TelegramConfig {
            enabled: true,
            bot_token: "test-token".to_string(),
            base_url: telegram_base_url.to_string(),
            poll_timeout_secs: 1,
            poll_backoff_ms: 50,
            max_reply_chars: 3500,
            bot_username: "ferrum_test_bot".to_string(),
            webhook_enabled: false,
            webhook_path: "/telegram/webhook".to_string(),
            webhook_secret: String::new(),
            typing_delay_ms: 800,
        },
        policy: PolicyConfig {
            outbound_enabled: true,
            dry_run: false,
            http_skill_allowlist: vec!["example.com".to_string()],
            outbound_kill_switch: false,
        },
        runtime: RuntimeConfig {
            queue_capacity: 64,
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
            performance_policy: ferrum::team::config::PerformancePolicy::BalancedFast,
            planner_aggressiveness: 60,
            max_escalation_tier: ferrum::team::config::EscalationTier::Standard,
            typing_delay_ms: 800,
            require_final_review: true,
            progress_updates_enabled: false,
            progress_update_threshold_ms: 1000,
        },
        dashboard: DashboardConfig {
            enable_dashboard: false,
            bind_addr: "127.0.0.1:0".to_string(),
            auth_token: String::new(),
        },
    }
}
