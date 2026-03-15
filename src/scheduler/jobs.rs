use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReminderSendJob {
    #[serde(default)]
    pub job_id: Option<i64>,
    pub reminder_id: i64,
    pub conversation_id: Option<i64>,
    pub channel: String,
    pub user_id: String,
    pub text: String,
}
