use askama::Template;
use axum::{
    extract::{Json, Path, Request, State},
    middleware::{from_fn_with_state, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Router,
};
use serde_json::json;

use crate::{
    app::runtime::SharedAppState,
    dashboard::{
        api,
        auth::is_authorized,
        sse::stream_events,
        views::{DashboardConversationTemplate, DashboardIndexTemplate},
    },
    team::config::TeamRuntimeSettings,
};

pub fn dashboard_router(state: SharedAppState) -> Router<SharedAppState> {
    Router::new()
        .route("/dashboard", get(dashboard_index))
        .route("/dashboard/conversations/{id}", get(dashboard_conversation))
        .route("/dashboard/plans/{id}", get(dashboard_plan_placeholder))
        .route("/dashboard/tasks/{id}", get(dashboard_task_placeholder))
        .route("/dashboard/team", get(dashboard_team_placeholder))
        .route("/dashboard/config", get(dashboard_config_placeholder))
        .route("/api/state", get(api::api_state))
        .route("/api/events", get(api::api_events))
        .route("/api/flow", get(api::api_flow))
        .route("/api/plans/{id}", get(api::api_plan))
        .route("/api/tasks/{id}", get(api::api_task))
        .route("/api/team", get(api::api_team))
        .route("/api/config", get(api::api_config))
        .route("/events/stream", get(events_stream))
        .route("/api/operator/pause", post(operator_pause))
        .route("/api/operator/resume", post(operator_resume))
        .route(
            "/api/operator/reload-identity",
            post(operator_reload_identity),
        )
        .route("/api/operator/reload-skills", post(operator_reload_skills))
        .route(
            "/api/operator/toggle-kill-switch",
            post(operator_toggle_kill_switch),
        )
        .route("/api/operator/reset-data", post(operator_reset_data))
        .route("/api/operator/team-settings", post(operator_team_settings))
        .route("/api/operator/replay/{event_id}", post(operator_replay))
        .route_layer(from_fn_with_state(state.clone(), dashboard_auth_guard))
}

async fn dashboard_index(State(state): State<SharedAppState>) -> Html<String> {
    let identity = state.identity.get();
    let team_settings = state.team.runtime_settings();
    let total_cost_usd = state.store.total_estimated_cost().await.unwrap_or(0.0);
    let template = DashboardIndexTemplate {
        runtime_state: if state.controls.is_paused() {
            "paused".to_string()
        } else {
            "running".to_string()
        },
        identity_id: identity.frontmatter.id,
        database_backend: state.store.backend_name().to_string(),
        cache_backend: if state.store.cache_enabled() {
            "redis".to_string()
        } else {
            "disabled".to_string()
        },
        team_size: state.team.persistent_count(),
        loaded_skills: state.skills.count(),
        queue_depth: state.queue_depth_value(),
        total_cost_usd,
        active_channel: "telegram".to_string(),
        telegram_enabled: state.config.telegram.enabled,
        telegram_bot_username: state.telegram.bot_username().unwrap_or_default(),
        telegram_bot_link: state.telegram.bot_link().unwrap_or_default(),
        effective_parallel_limit: state.team.effective_parallel_limit(),
        configured_team_size: team_settings.team_size,
    };
    Html(
        template
            .render()
            .unwrap_or_else(|_| "template error".to_string()),
    )
}

async fn dashboard_conversation(
    Path(conversation_id): Path<i64>,
    State(state): State<SharedAppState>,
) -> Html<String> {
    let turns = state
        .store
        .recent_turns(conversation_id, 1)
        .await
        .unwrap_or_default();
    let final_answer = turns.last().map(|t| t.content.clone()).unwrap_or_default();

    let inbound = state
        .store
        .recent_turns(conversation_id, 2)
        .await
        .ok()
        .and_then(|v| v.first().map(|t| t.content.clone()))
        .unwrap_or_default();

    let template = DashboardConversationTemplate {
        conversation_id,
        inbound_len: inbound.chars().count(),
        final_answer_len: final_answer.chars().count(),
        inbound,
        final_answer,
    };

    Html(
        template
            .render()
            .unwrap_or_else(|_| "template error".to_string()),
    )
}

async fn dashboard_plan_placeholder(Path(id): Path<String>) -> Html<String> {
    Html(format!("<h1>Plan {id}</h1>"))
}

async fn dashboard_task_placeholder(Path(id): Path<String>) -> Html<String> {
    Html(format!("<h1>Task {id}</h1>"))
}

async fn dashboard_team_placeholder(State(state): State<SharedAppState>) -> Html<String> {
    let team = state
        .team
        .list()
        .into_iter()
        .map(|a| format!("<li>{} - {} - {:?}</li>", a.id, a.role, a.state))
        .collect::<Vec<_>>()
        .join("\n");
    Html(format!("<h1>Team</h1><ul>{team}</ul>"))
}

async fn dashboard_config_placeholder(State(state): State<SharedAppState>) -> Html<String> {
    let runtime = state.team.runtime_settings();
    Html(format!(
        "<h1>Config</h1><pre>{}</pre>",
        serde_json::to_string_pretty(&runtime).unwrap_or_default()
    ))
}

async fn events_stream(State(state): State<SharedAppState>) -> Response {
    stream_events(state.events.subscribe()).into_response()
}

async fn operator_pause(State(state): State<SharedAppState>) -> Html<String> {
    state.controls.set_paused(true);
    let _ = state
        .events
        .publish("runtime_paused", serde_json::json!({}));
    Html("paused".to_string())
}

async fn operator_resume(State(state): State<SharedAppState>) -> Html<String> {
    state.controls.set_paused(false);
    let _ = state
        .events
        .publish("runtime_resumed", serde_json::json!({}));
    Html("running".to_string())
}

async fn operator_reload_identity(State(state): State<SharedAppState>) -> Html<String> {
    let _ = state.identity.spawn_watcher();
    let _ = state
        .events
        .publish("identity_reloaded", serde_json::json!({}));
    Html("ok".to_string())
}

async fn operator_reload_skills(State(state): State<SharedAppState>) -> Html<String> {
    let _ = state.skills.spawn_watcher();
    let _ = state
        .events
        .publish("skills_reloaded", serde_json::json!({}));
    Html("ok".to_string())
}

async fn operator_toggle_kill_switch(State(state): State<SharedAppState>) -> Html<String> {
    let next = !state.controls.outbound_kill_switch();
    state.controls.set_outbound_kill_switch(next);
    Html(format!("kill_switch={next}"))
}

async fn operator_reset_data(State(state): State<SharedAppState>) -> Json<serde_json::Value> {
    match state.store.reset_memory_data().await {
        Ok(()) => Json(json!({
            "ok": true,
            "message": "memory data reset",
        })),
        Err(err) => Json(json!({
            "ok": false,
            "error": err.to_string(),
        })),
    }
}

async fn operator_team_settings(
    State(state): State<SharedAppState>,
    Json(settings): Json<TeamRuntimeSettings>,
) -> Json<serde_json::Value> {
    match state.team.apply_runtime_settings(settings).await {
        Ok(applied) => {
            let _ = state.events.publish(
                "team_settings_updated",
                json!({
                    "team_size": applied.team_size,
                    "max_parallel_tasks": applied.max_parallel_tasks,
                    "max_ephemeral_subagents": applied.max_ephemeral_subagents,
                    "roleset": applied.subagent_roleset,
                    "performance_policy": applied.performance_policy,
                    "planner_aggressiveness": applied.planner_aggressiveness,
                    "max_escalation_tier": applied.max_escalation_tier,
                    "typing_delay_ms": applied.typing_delay_ms,
                }),
            );
            Json(json!({
                "ok": true,
                "settings": applied,
                "resources": state.team.resource_snapshot(),
                "effective_parallel_limit": state.team.effective_parallel_limit(),
                "effective_ephemeral_capacity": state.team.effective_ephemeral_capacity(),
            }))
        }
        Err(err) => Json(json!({
            "ok": false,
            "error": err.to_string(),
        })),
    }
}

async fn operator_replay(
    Path(event_id): Path<String>,
    State(state): State<SharedAppState>,
) -> Html<String> {
    let _ = state.events.publish(
        "operator_replay_requested",
        serde_json::json!({"event_id": event_id}),
    );
    Html("queued".to_string())
}

async fn dashboard_auth_guard(
    State(state): State<SharedAppState>,
    req: Request,
    next: Next,
) -> Result<Response, (axum::http::StatusCode, &'static str)> {
    if is_authorized(req.headers(), &state) {
        return Ok(next.run(req).await);
    }
    Err((
        axum::http::StatusCode::UNAUTHORIZED,
        "dashboard auth required",
    ))
}
