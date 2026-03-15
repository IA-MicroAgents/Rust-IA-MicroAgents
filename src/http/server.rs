use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use chrono::Utc;
use serde::Serialize;
use tower_http::services::ServeDir;
use tracing::info;

use crate::{
    app::runtime::SharedAppState,
    channel::telegram::{normalize_update, TelegramUpdate},
    dashboard,
    errors::{AppError, AppResult},
    http::health,
    telemetry::metrics as telemetry_metrics,
};

#[derive(Debug, Serialize)]
struct ReadyResponse {
    status: String,
    skills_loaded: usize,
    identity_id: String,
    team_size: usize,
}

pub async fn serve(state: SharedAppState) -> AppResult<()> {
    let bind = state.config.bind_addr.clone();
    let app = build_router(state.clone());

    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .map_err(|e| AppError::Http(format!("bind failed on {bind}: {e}")))?;
    info!(bind, "http server listening");

    axum::serve(listener, app)
        .await
        .map_err(|e| AppError::Http(format!("http server error: {e}")))
}

pub fn build_router(state: SharedAppState) -> Router {
    let mut app = Router::new()
        .route("/healthz", get(health::healthz))
        .route("/readyz", get(readyz))
        .route("/metrics", get(metrics_endpoint))
        .nest_service("/static", ServeDir::new("static"));

    if state.config.telegram.enabled && state.config.telegram.webhook_enabled {
        let webhook_path = state.config.telegram.webhook_path.clone();
        app = app.route(webhook_path.as_str(), post(telegram_webhook));
    }

    if state.config.dashboard.enable_dashboard {
        app = app.merge(dashboard::dashboard_router(state.clone()));
    }

    app.with_state(state)
}
async fn readyz(State(state): State<SharedAppState>) -> Json<ReadyResponse> {
    let identity = state.identity.get();
    Json(ReadyResponse {
        status: if state.controls.is_paused() {
            "paused".to_string()
        } else {
            "ready".to_string()
        },
        skills_loaded: state.skills.count(),
        identity_id: identity.frontmatter.id,
        team_size: state.team.list().len(),
    })
}

async fn metrics_endpoint() -> Response {
    (
        StatusCode::OK,
        [("content-type", "text/plain; version=0.0.4")],
        telemetry_metrics::gather(),
    )
        .into_response()
}

async fn telegram_webhook(
    State(state): State<SharedAppState>,
    headers: HeaderMap,
    Json(update): Json<TelegramUpdate>,
) -> impl IntoResponse {
    if !state.config.telegram.webhook_secret.trim().is_empty() {
        let header_value = headers
            .get("x-telegram-bot-api-secret-token")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        if header_value != state.config.telegram.webhook_secret {
            return StatusCode::UNAUTHORIZED;
        }
    }

    let Ok(Some(mut event)) = normalize_update(update) else {
        return StatusCode::OK;
    };

    let wrapper = serde_json::json!({
        "raw": event.raw_payload.clone(),
        "normalized": event.clone(),
    });

    event.queued_at = Some(Utc::now());

    match state
        .store
        .insert_inbound_event(&event.event_id, "telegram", &wrapper)
        .await
    {
        Ok(Some(_)) => {
            let _ = state.events.publish(
                "telegram_message_received",
                serde_json::json!({
                    "event_id": event.event_id,
                    "user_id": event.user_id,
                    "conversation_external_id": event.conversation_external_id,
                    "text": event.text,
                    "channel": event.channel,
                }),
            );
            match state.queue_tx.try_send(event) {
                Ok(_) => {
                    let depth = state
                        .queue_depth
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                        + 1;
                    metrics::gauge!("ferrum_queue_depth").set(depth as f64);
                    StatusCode::OK
                }
                Err(_) => StatusCode::SERVICE_UNAVAILABLE,
            }
        }
        Ok(None) => {
            let _ = state.events.publish(
                "event_deduplicated",
                serde_json::json!({"event_id": event.event_id, "channel": "telegram"}),
            );
            StatusCode::OK
        }
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}
