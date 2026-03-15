pub mod broker;
pub mod models;
pub mod openrouter;
pub mod response_types;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::identity::schema::ModelRoutes;

pub use models::{ModelCapabilities, ModelMetadata};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    #[serde(default)]
    pub parts: Vec<ChatMessagePart>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChatMessagePart {
    Text { text: String },
    ImageUrl { url: String },
    InputAudio { data: String, format: String },
}

impl ChatMessage {
    pub fn text(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
            parts: Vec::new(),
        }
    }

    pub fn parts(role: impl Into<String>, parts: Vec<ChatMessagePart>) -> Self {
        Self {
            role: role.into(),
            content: String::new(),
            parts,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub max_output_tokens: u32,
    pub temperature: f32,
    pub require_json: bool,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub estimated_cost_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmResponse {
    pub model: String,
    pub content: String,
    pub usage: Usage,
    pub latency_ms: u64,
}

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("auth failure")]
    AuthFailure,
    #[error("bad model id: {0}")]
    BadModelId(String),
    #[error("rate limit")]
    RateLimit,
    #[error("malformed upstream response: {0}")]
    MalformedResponse(String),
    #[error("upstream failure: {0}")]
    UpstreamFailure(String),
    #[error("unsupported capability request: {0}")]
    UnsupportedCapability(String),
    #[error("timeout")]
    Timeout,
    #[error("network: {0}")]
    Network(String),
}

pub type ProviderResult<T> = Result<T, ProviderError>;

#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn validate_models(&self, routes: &ModelRoutes)
        -> ProviderResult<Vec<ModelCapabilities>>;
    async fn chat_completion(&self, request: LlmRequest) -> ProviderResult<LlmResponse>;
    fn model_catalog(&self) -> Vec<ModelMetadata>;
}
