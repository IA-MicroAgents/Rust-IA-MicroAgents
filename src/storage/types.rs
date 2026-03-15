use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboundEventRecord {
    pub id: i64,
    pub event_id: String,
    pub source: String,
    pub payload_json: Value,
    pub received_at: DateTime<Utc>,
    pub processed_at: Option<DateTime<Utc>>,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationTurn {
    pub role: String,
    pub content: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationContextSnapshot {
    pub recent_turns: Vec<ConversationTurn>,
    pub latest_summary: Option<String>,
    pub memories: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ToolTraceInsert<'a> {
    pub trace_id: &'a str,
    pub skill_name: &'a str,
    pub input_json: &'a serde_json::Value,
    pub output_json: Option<&'a serde_json::Value>,
    pub status: &'a str,
    pub duration_ms: u64,
    pub error: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub struct OutboundMessageInsert<'a> {
    pub trace_id: &'a str,
    pub conversation_id: Option<i64>,
    pub channel: &'a str,
    pub recipient: &'a str,
    pub content: &'a str,
    pub provider_message_id: Option<&'a str>,
    pub status: &'a str,
}

#[derive(Debug, Clone)]
pub struct TaskAttemptInsert<'a> {
    pub task_id: &'a str,
    pub attempt_no: u32,
    pub subagent_id: &'a str,
    pub status: &'a str,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub error: Option<&'a str>,
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct TaskReviewInsert<'a> {
    pub task_id: &'a str,
    pub attempt_no: u32,
    pub reviewer: &'a str,
    pub action: &'a str,
    pub score: f64,
    pub notes: &'a str,
    pub decision_json: &'a Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationTraceBundle {
    pub conversation_id: i64,
    pub turns: Vec<ConversationTurn>,
    pub outbound_messages: Vec<Value>,
    pub model_usages: Vec<Value>,
    pub plans: Vec<Value>,
    pub tasks: Vec<Value>,
    pub runtime_events: Vec<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BusEventEnvelope {
    pub id: Uuid,
    pub event_kind: String,
    pub stream_key: String,
    pub aggregate_id: Option<String>,
    pub conversation_id: Option<i64>,
    pub trace_id: Option<String>,
    pub task_id: Option<String>,
    pub subagent_id: Option<String>,
    pub route_key: Option<String>,
    pub resolved_model: Option<String>,
    pub evidence_count: Option<u32>,
    pub reasoning_tier: Option<String>,
    pub fallback_kind: Option<String>,
    pub created_at: DateTime<Utc>,
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OutboxEventRecord {
    pub envelope: BusEventEnvelope,
    pub publish_attempts: u32,
    pub published_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct OutboxStats {
    pub pending: i64,
    pub published: i64,
    pub failed: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct StreamPendingStats {
    pub stream: String,
    pub group: String,
    pub pending: u64,
    pub first_id: Option<String>,
    pub last_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use serde_json::json;
    use uuid::Uuid;

    use super::BusEventEnvelope;

    #[test]
    fn bus_event_envelope_round_trips() {
        let envelope = BusEventEnvelope {
            id: Uuid::new_v4(),
            event_kind: "runtime.event".to_string(),
            stream_key: "events".to_string(),
            aggregate_id: Some("turn:1".to_string()),
            conversation_id: Some(42),
            trace_id: Some("trace-1".to_string()),
            task_id: Some("task-1".to_string()),
            subagent_id: Some("subagent-1".to_string()),
            route_key: Some("fast_text".to_string()),
            resolved_model: Some("openai/gpt-4o-mini".to_string()),
            evidence_count: Some(2),
            reasoning_tier: Some("medium".to_string()),
            fallback_kind: None,
            created_at: Utc::now(),
            payload: json!({"hello":"world"}),
        };

        let raw = serde_json::to_string(&envelope).expect("serialize envelope");
        let decoded: BusEventEnvelope = serde_json::from_str(&raw).expect("deserialize envelope");
        assert_eq!(decoded, envelope);
    }
}
