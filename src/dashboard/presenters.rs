use serde::Serialize;

use crate::team::Subagent;

#[derive(Debug, Clone, Serialize)]
pub struct TeamPresenter {
    pub id: String,
    pub role: String,
    pub state: String,
    pub current_task_id: Option<String>,
    pub heartbeat_at: String,
    pub last_review_score: f64,
}

impl From<Subagent> for TeamPresenter {
    fn from(value: Subagent) -> Self {
        Self {
            id: value.id,
            role: value.role,
            state: format!("{:?}", value.state),
            current_task_id: value.current_task_id,
            heartbeat_at: value.heartbeat_at.to_rfc3339(),
            last_review_score: value.last_review_score,
        }
    }
}
