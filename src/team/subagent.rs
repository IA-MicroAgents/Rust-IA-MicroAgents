use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SubagentState {
    Idle,
    Assigned,
    Running,
    WaitingReview,
    Paused,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subagent {
    pub id: String,
    pub role: String,
    pub model_route: String,
    pub resolved_model: String,
    pub allowed_skills: Vec<String>,
    pub ephemeral: bool,
    pub state: SubagentState,
    pub current_task_id: Option<String>,
    pub heartbeat_at: DateTime<Utc>,
    pub retries: u32,
    pub last_review_score: f64,
    pub last_error: Option<String>,
}
