use axum::{
    extract::{Path, State},
    Json,
};
use serde_json::json;

use crate::app::runtime::SharedAppState;

pub async fn api_state(State(state): State<SharedAppState>) -> Json<serde_json::Value> {
    let resources = state.team.resource_snapshot();
    let outbox = state.store.outbox_stats().await.unwrap_or_default();
    let events_pending = state
        .store
        .stream_pending_summary("events", "dashboard-projection")
        .await
        .ok();
    let memory_pending = state
        .store
        .stream_pending_summary("memory", "memory-workers")
        .await
        .ok();
    let jobs_pending = state
        .store
        .stream_pending_summary("jobs", "jobs-workers")
        .await
        .ok();
    Json(json!({
        "runtime_paused": state.controls.is_paused(),
        "identity": state.identity.get().frontmatter.id,
        "database_backend": state.store.backend_name(),
        "cache_backend": "redis",
        "skills_loaded": state.skills.count(),
        "team_size": state.team.list().len(),
        "persistent_subagents": state.team.persistent_count(),
        "ephemeral_subagents": state.team.ephemeral_count(),
        "configured_team_size": state.team.config().team_size,
        "effective_ephemeral_capacity": state.team.effective_ephemeral_capacity(),
        "max_ephemeral_subagents": state.team.runtime_settings().max_ephemeral_subagents,
        "effective_parallel_limit": state.team.effective_parallel_limit(),
        "queue_depth": state.queue_depth.load(std::sync::atomic::Ordering::SeqCst),
        "active_channel": "telegram",
        "resources": resources,
        "bus": {
            "enabled": state.store.bus_enabled(),
            "stream_prefix": state.store.bus_config().stream_prefix,
            "outbox": outbox,
            "events_pending": events_pending,
            "memory_pending": memory_pending,
            "jobs_pending": jobs_pending,
        }
    }))
}

pub async fn api_events(State(state): State<SharedAppState>) -> Json<serde_json::Value> {
    Json(json!({"events": state.store.latest_runtime_events(100).await.unwrap_or_default()}))
}

pub async fn api_flow(State(state): State<SharedAppState>) -> Json<serde_json::Value> {
    Json(json!({
        "team": state.team.list(),
        "recent_events": state.store.latest_runtime_events(150).await.unwrap_or_default(),
        "resources": state.team.resource_snapshot(),
        "effective_parallel_limit": state.team.effective_parallel_limit(),
        "effective_ephemeral_capacity": state.team.effective_ephemeral_capacity(),
    }))
}

pub async fn api_plan(
    Path(plan_id): Path<String>,
    State(state): State<SharedAppState>,
) -> Json<serde_json::Value> {
    Json(json!({"plan": state.store.get_plan_json(&plan_id).await.ok().flatten()}))
}

pub async fn api_task(
    Path(task_id): Path<String>,
    State(state): State<SharedAppState>,
) -> Json<serde_json::Value> {
    Json(json!({"task": state.store.get_task_json(&task_id).await.ok().flatten()}))
}

pub async fn api_team(State(state): State<SharedAppState>) -> Json<serde_json::Value> {
    Json(json!({
        "team": state.team.list(),
        "persistent_subagents": state.team.persistent_count(),
        "ephemeral_subagents": state.team.ephemeral_count(),
        "effective_ephemeral_capacity": state.team.effective_ephemeral_capacity(),
        "resources": state.team.resource_snapshot(),
        "settings": state.team.runtime_settings(),
    }))
}

pub async fn api_config(State(state): State<SharedAppState>) -> Json<serde_json::Value> {
    Json(json!({
        "identity": state.identity.get().frontmatter,
        "team": state.team.runtime_settings(),
        "team_subagents": state.team.list(),
        "team_resources": state.team.resource_snapshot(),
        "policy": {
            "outbound_kill_switch": state.controls.outbound_kill_switch(),
            "dashboard_enabled": state.config.dashboard.enable_dashboard,
        },
        "storage": {
            "database_backend": state.store.backend_name(),
            "cache_backend": "redis",
        },
        "bus": {
            "enabled": state.store.bus_enabled(),
            "stream_prefix": state.store.bus_config().stream_prefix,
            "stream_maxlen": state.store.bus_config().stream_maxlen,
            "outbox_publish_batch": state.store.bus_config().outbox_publish_batch,
            "outbox_poll_ms": state.store.bus_config().outbox_poll_ms,
        },
        "skills_catalog": state.skills.list().into_iter().map(|skill| json!({
            "name": skill.manifest.name,
            "description": skill.manifest.description,
            "tags": skill.manifest.tags,
        })).collect::<Vec<_>>(),
        "channels": {
            "telegram": {
                "enabled": state.config.telegram.enabled,
                "bot_username": state.telegram.bot_username(),
                "bot_link": state.telegram.bot_link(),
            }
        },
    }))
}
