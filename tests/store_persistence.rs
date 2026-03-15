mod support;

use chrono::Utc;
use ferrum::{
    config::{
        AppConfig, DashboardConfig, OpenRouterConfig, PolicyConfig, RuntimeConfig, TelegramConfig,
    },
    storage::{BusEventEnvelope, OutboundMessageInsert, Store},
    team::{config::SubagentMode, TeamConfig},
};
use serde_json::json;
use uuid::Uuid;

#[tokio::test]
async fn postgres_store_persists_model_usage_and_outbound_messages() {
    let backend = support::TestBackend::new("store_persistence");
    let cfg = AppConfig {
        bind_addr: "127.0.0.1:0".to_string(),
        database: backend.database.clone(),
        cache: backend.cache.clone(),
        bus: ferrum::config::BusConfig {
            enabled: true,
            stream_prefix: "ferrum-test".to_string(),
            stream_maxlen: 2000,
            outbox_publish_batch: 32,
            outbox_poll_ms: 200,
            outbox_max_retries: 4,
            stream_reclaim_idle_ms: 60_000,
            consumer_name: "store-persistence".to_string(),
            memory_consumer_concurrency: 1,
            jobs_consumer_concurrency: 1,
        },
        identity_path: "./IDENTITY.md".into(),
        skills_dir: "./skills".into(),
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
        policy: PolicyConfig {
            outbound_enabled: true,
            dry_run: false,
            http_skill_allowlist: vec![],
            outbound_kill_switch: false,
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
    };

    let store = Store::from_config(&cfg).await.expect("store");
    let conversation_id = store
        .upsert_conversation("telegram", "user-1")
        .await
        .expect("conversation");
    store
        .append_turn(
            conversation_id,
            "user",
            "hola",
            "trace-1",
            "direct_reply",
            Some((10, 8, 0.001)),
        )
        .await
        .expect("turn");
    store
        .insert_model_usage("trace-1", "model-fast", 10, 8, 0.001, 42)
        .await
        .expect("usage");
    store
        .insert_outbound_message(OutboundMessageInsert {
            trace_id: "trace-1",
            conversation_id: Some(conversation_id),
            channel: "telegram",
            recipient: "user-1",
            content: "Hola desde Ferrum",
            provider_message_id: Some("msg-1"),
            status: "sent",
        })
        .await
        .expect("outbound");

    assert_eq!(store.count_model_usages().await.expect("usage count"), 1);
    assert_eq!(
        store
            .count_outbound_messages()
            .await
            .expect("outbound count"),
        1
    );
    assert_eq!(
        store
            .recent_turns(conversation_id, 4)
            .await
            .expect("recent")
            .len(),
        1
    );
}

#[tokio::test]
async fn outbox_event_round_trip_persists_and_marks_published() {
    let backend = support::TestBackend::new("outbox_round_trip");
    let cfg = AppConfig {
        bind_addr: "127.0.0.1:0".to_string(),
        database: backend.database.clone(),
        cache: backend.cache.clone(),
        bus: ferrum::config::BusConfig {
            enabled: true,
            stream_prefix: "ferrum-test".to_string(),
            stream_maxlen: 2000,
            outbox_publish_batch: 32,
            outbox_poll_ms: 200,
            outbox_max_retries: 4,
            stream_reclaim_idle_ms: 60_000,
            consumer_name: "store-persistence".to_string(),
            memory_consumer_concurrency: 1,
            jobs_consumer_concurrency: 1,
        },
        identity_path: "./IDENTITY.md".into(),
        skills_dir: "./skills".into(),
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
        policy: PolicyConfig {
            outbound_enabled: true,
            dry_run: false,
            http_skill_allowlist: vec![],
            outbound_kill_switch: false,
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
    };

    let store = Store::from_config(&cfg).await.expect("store");
    let conversation_id = store
        .upsert_conversation("telegram", "outbox-user")
        .await
        .expect("conversation");
    let envelope = BusEventEnvelope {
        id: Uuid::new_v4(),
        event_kind: "memory.fact.write".to_string(),
        stream_key: "memory".to_string(),
        aggregate_id: Some(format!("conversation:{conversation_id}")),
        conversation_id: Some(conversation_id),
        trace_id: Some("trace-1".to_string()),
        task_id: None,
        subagent_id: None,
        route_key: Some("fast_text".to_string()),
        resolved_model: Some("openai/gpt-4o-mini".to_string()),
        evidence_count: None,
        reasoning_tier: None,
        fallback_kind: None,
        created_at: Utc::now(),
        payload: json!({
            "conversation_id": conversation_id,
            "fact_key":"lang",
            "fact_value":"es",
            "confidence":0.9
        }),
    };

    store
        .enqueue_bus_event(&envelope)
        .await
        .expect("enqueue outbox");
    let pending = store
        .fetch_pending_outbox_events(10)
        .await
        .expect("fetch pending");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].envelope.id, envelope.id);

    store
        .mark_outbox_published(envelope.id)
        .await
        .expect("mark published");
    let stats = store.outbox_stats().await.expect("outbox stats");
    assert_eq!(stats.pending, 0);
    assert_eq!(stats.published, 1);
}
