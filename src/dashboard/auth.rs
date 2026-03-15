use axum::http::HeaderMap;

use crate::app::runtime::SharedAppState;

pub fn is_authorized(headers: &HeaderMap, state: &SharedAppState) -> bool {
    let configured = &state.config.dashboard.auth_token;
    if configured.is_empty() {
        return true;
    }

    headers
        .get("x-ai-microagents-dashboard-token")
        .or_else(|| headers.get("x-ferrum-dashboard-token"))
        .and_then(|v| v.to_str().ok())
        .map(|provided| provided == configured)
        .unwrap_or(false)
}
