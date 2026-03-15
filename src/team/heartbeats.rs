use chrono::{DateTime, Duration, Utc};

pub fn is_stale(last_heartbeat: DateTime<Utc>, ttl_ms: u64) -> bool {
    let Ok(ttl_ms_i64) = i64::try_from(ttl_ms) else {
        return false;
    };
    let threshold = last_heartbeat + Duration::milliseconds(ttl_ms_i64);
    Utc::now() > threshold
}
