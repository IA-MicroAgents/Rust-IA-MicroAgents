use chrono::{DateTime, Duration, Utc};

#[derive(Debug, Clone)]
pub struct TaskLease {
    pub task_id: String,
    pub subagent_id: String,
    pub acquired_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

impl TaskLease {
    pub fn new(task_id: String, subagent_id: String, ttl_ms: u64) -> Self {
        let now = Utc::now();
        let ttl_ms_i64 = i64::try_from(ttl_ms).unwrap_or(5000);
        Self {
            task_id,
            subagent_id,
            acquired_at: now,
            expires_at: now + Duration::milliseconds(ttl_ms_i64),
        }
    }

    pub fn refresh(&mut self, ttl_ms: u64) {
        let now = Utc::now();
        let ttl_ms_i64 = i64::try_from(ttl_ms).unwrap_or(5000);
        self.expires_at = now + Duration::milliseconds(ttl_ms_i64);
    }

    pub fn is_expired(&self) -> bool {
        Utc::now() > self.expires_at
    }
}
