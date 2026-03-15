pub mod telegram;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InboundAttachmentKind {
    Image,
    Audio,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboundAttachment {
    pub kind: InboundAttachmentKind,
    pub file_id: String,
    #[serde(default)]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub file_name: Option<String>,
    #[serde(default)]
    pub telegram_unique_id: Option<String>,
    #[serde(default)]
    pub size_bytes: Option<u64>,
    #[serde(default)]
    pub duration_secs: Option<u32>,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InboundKind {
    UserMessage,
    MessageStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NormalizedInboundEvent {
    pub event_id: String,
    pub channel: String,
    pub conversation_external_id: String,
    pub user_id: String,
    pub text: String,
    pub kind: InboundKind,
    pub timestamp: DateTime<Utc>,
    #[serde(default)]
    pub queued_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub attachments: Vec<InboundAttachment>,
    pub raw_payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutboundSendResult {
    pub provider_message_id: Option<String>,
    pub status: String,
}
