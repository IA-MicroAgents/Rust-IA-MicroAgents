use serde::{Deserialize, Serialize};

use crate::team::{reviewer::TaskReview, worker::TaskArtifact};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskExecutionResult {
    pub task_id: String,
    pub subagent_id: String,
    pub subagent_role: String,
    pub ephemeral: bool,
    pub destroyed_on_release: bool,
    pub artifact: Option<TaskArtifact>,
    pub review: Option<TaskReview>,
    pub accepted: bool,
    pub error: Option<String>,
    pub duration_ms: u64,
    pub attempts: u32,
}
