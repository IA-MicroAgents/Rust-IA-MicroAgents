use std::{collections::HashMap, sync::Arc, time::Instant};

use async_trait::async_trait;
use parking_lot::RwLock;
use reqwest::StatusCode;
use reqwest_middleware::{ClientWithMiddleware, RequestBuilder};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::time::{sleep, timeout, Duration};

use crate::{
    config::OpenRouterConfig,
    http::client::build_retrying_client,
    identity::schema::ModelRoutes,
    llm::{
        ChatMessage, ChatMessagePart, LlmProvider, LlmRequest, LlmResponse, ModelCapabilities,
        ModelMetadata, ProviderError, ProviderResult, Usage,
    },
};

#[derive(Clone)]
pub struct OpenRouterClient {
    cfg: OpenRouterConfig,
    http: ClientWithMiddleware,
    capabilities: Arc<RwLock<HashMap<String, ModelCapabilities>>>,
    catalog: Arc<RwLock<HashMap<String, ModelMetadata>>>,
}

#[derive(Debug, Deserialize)]
struct ModelsResponse {
    data: Vec<ModelEntry>,
}

#[derive(Debug, Deserialize)]
struct ModelEntry {
    id: String,
    context_length: Option<u64>,
    architecture: Option<ModelArchitecture>,
    modality: Option<Vec<String>>,
    #[serde(default)]
    pricing: Option<ModelPricing>,
}

#[derive(Debug, Deserialize)]
struct ModelArchitecture {
    #[serde(default)]
    input_modalities: Vec<String>,
    #[serde(default)]
    output_modalities: Vec<String>,
}

#[derive(Debug, Deserialize, Default)]
struct ModelPricing {
    #[serde(default, deserialize_with = "deserialize_cost_field")]
    prompt: Option<f64>,
    #[serde(default, deserialize_with = "deserialize_cost_field")]
    completion: Option<f64>,
}

#[derive(Debug, Serialize)]
struct ChatCompletionRequest {
    model: String,
    messages: Vec<OpenRouterMessage>,
    max_tokens: u32,
    temperature: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
struct OpenRouterMessage {
    role: String,
    content: OpenRouterContent,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum OpenRouterContent {
    Text(String),
    Parts(Vec<OpenRouterContentPart>),
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum OpenRouterContentPart {
    Text { text: String },
    ImageUrl { image_url: OpenRouterImageUrl },
    InputAudio { input_audio: OpenRouterInputAudio },
}

#[derive(Debug, Serialize)]
struct OpenRouterImageUrl {
    url: String,
}

#[derive(Debug, Serialize)]
struct OpenRouterInputAudio {
    data: String,
    format: String,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionResponse {
    model: String,
    choices: Vec<Choice>,
    usage: Option<ResponseUsage>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: ChoiceMessage,
}

#[derive(Debug, Deserialize)]
struct ChoiceMessage {
    content: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct ResponseUsage {
    prompt_tokens: Option<u32>,
    completion_tokens: Option<u32>,
    cost: Option<f64>,
}

impl OpenRouterClient {
    pub fn new(cfg: OpenRouterConfig) -> ProviderResult<Self> {
        let http = build_retrying_client(Duration::from_millis(cfg.timeout_ms), 2)
            .map_err(|e| ProviderError::Network(e.to_string()))?;

        Ok(Self {
            cfg,
            http,
            capabilities: Arc::new(RwLock::new(HashMap::new())),
            catalog: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    fn auth_headers(&self, request: RequestBuilder) -> RequestBuilder {
        let mut req = request
            .header("Authorization", format!("Bearer {}", self.cfg.api_key))
            .header("Content-Type", "application/json");

        if let Some(site_url) = &self.cfg.site_url {
            req = req.header("HTTP-Referer", site_url);
        }
        if let Some(app_name) = &self.cfg.app_name {
            req = req.header("X-Title", app_name);
        }

        req
    }

    async fn fetch_models(&self) -> ProviderResult<Vec<ModelMetadata>> {
        if self.cfg.mock_mode {
            return Ok(vec![
                ModelMetadata {
                    id: "openai/gpt-4o-mini".to_string(),
                    context_length: Some(128_000),
                    supports_text: true,
                    supports_tools: true,
                    supports_vision: true,
                    supports_audio_input: false,
                    supports_image_output: false,
                    prompt_cost_per_million: Some(0.15),
                    completion_cost_per_million: Some(0.60),
                },
                ModelMetadata {
                    id: "openai/gpt-4.1-mini".to_string(),
                    context_length: Some(64_000),
                    supports_text: true,
                    supports_tools: true,
                    supports_vision: false,
                    supports_audio_input: false,
                    supports_image_output: false,
                    prompt_cost_per_million: Some(0.40),
                    completion_cost_per_million: Some(1.60),
                },
                ModelMetadata {
                    id: "openai/gpt-4.1".to_string(),
                    context_length: Some(128_000),
                    supports_text: true,
                    supports_tools: true,
                    supports_vision: false,
                    supports_audio_input: false,
                    supports_image_output: false,
                    prompt_cost_per_million: Some(2.00),
                    completion_cost_per_million: Some(8.00),
                },
            ]);
        }

        let url = format!("{}/models", self.cfg.base_url.trim_end_matches('/'));
        let req = self.auth_headers(self.http.get(url));

        let response = req.send().await.map_err(map_middleware_error)?;
        let status = response.status();
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(ProviderError::AuthFailure);
        }
        if !status.is_success() {
            return Err(ProviderError::UpstreamFailure(format!(
                "models endpoint status {status}"
            )));
        }

        let parsed: ModelsResponse = response
            .json()
            .await
            .map_err(|e| ProviderError::MalformedResponse(e.to_string()))?;

        let models = parsed
            .data
            .into_iter()
            .map(|entry| {
                let mut modalities = entry.modality.unwrap_or_default();
                if let Some(arch) = entry.architecture {
                    modalities.extend(arch.input_modalities);
                    modalities.extend(arch.output_modalities);
                }
                let lower = modalities
                    .into_iter()
                    .map(|m| m.to_lowercase())
                    .collect::<Vec<_>>();

                let supports_text = lower.iter().any(|m| m.contains("text")) || lower.is_empty();
                let supports_vision = lower
                    .iter()
                    .any(|m| m.contains("image") || m.contains("vision"));
                let supports_audio_input = lower.iter().any(|m| m.contains("audio"));
                let supports_image_output = lower.iter().any(|m| m.contains("image"));
                let supports_tools = true;

                ModelMetadata {
                    id: entry.id,
                    context_length: entry.context_length,
                    supports_text,
                    supports_tools,
                    supports_vision,
                    supports_audio_input,
                    supports_image_output,
                    prompt_cost_per_million: entry.pricing.as_ref().and_then(|p| p.prompt),
                    completion_cost_per_million: entry.pricing.as_ref().and_then(|p| p.completion),
                }
            })
            .collect::<Vec<_>>();

        Ok(models)
    }
}

#[async_trait]
impl LlmProvider for OpenRouterClient {
    async fn validate_models(
        &self,
        routes: &ModelRoutes,
    ) -> ProviderResult<Vec<ModelCapabilities>> {
        let metadata = self.fetch_models().await?;
        let mut lookup = HashMap::new();
        for model in metadata {
            self.catalog.write().insert(model.id.clone(), model.clone());
            self.capabilities.write().insert(
                model.id.clone(),
                ModelCapabilities {
                    id: model.id.clone(),
                    context_length: model.context_length,
                    supports_text: model.supports_text,
                    supports_tools: model.supports_tools,
                    supports_vision: model.supports_vision,
                    supports_audio_input: model.supports_audio_input,
                    supports_image_output: model.supports_image_output,
                    prompt_cost_per_million: model.prompt_cost_per_million,
                    completion_cost_per_million: model.completion_cost_per_million,
                },
            );
            lookup.insert(model.id.clone(), model);
        }

        let required = routes.all_model_ids();

        let mut capabilities = Vec::new();
        for model_id in required {
            let meta = lookup
                .get(&model_id)
                .ok_or_else(|| ProviderError::BadModelId(model_id.clone()))?;
            let caps = ModelCapabilities {
                id: meta.id.clone(),
                context_length: meta.context_length,
                supports_text: meta.supports_text,
                supports_tools: meta.supports_tools,
                supports_vision: meta.supports_vision,
                supports_audio_input: meta.supports_audio_input,
                supports_image_output: meta.supports_image_output,
                prompt_cost_per_million: meta.prompt_cost_per_million,
                completion_cost_per_million: meta.completion_cost_per_million,
            };
            capabilities.push(caps.clone());
            self.capabilities.write().insert(caps.id.clone(), caps);
            self.catalog.write().insert(meta.id.clone(), meta.clone());
        }

        if let Some(vision_model) = routes
            .route_value("vision_understand")
            .or_else(|| routes.route_value("vision"))
        {
            if let Some(vision) = self.capabilities.read().get(vision_model) {
                if !vision.supports_vision {
                    return Err(ProviderError::UnsupportedCapability(format!(
                        "vision route model {} lacks vision capability",
                        vision_model
                    )));
                }
            }
        }

        Ok(capabilities)
    }

    async fn chat_completion(&self, request: LlmRequest) -> ProviderResult<LlmResponse> {
        if self.cfg.mock_mode {
            let content = if request.require_json {
                json!({
                    "route": "direct_reply",
                    "assistant_reply": "Mock response from ferrum",
                    "tool_calls": [],
                    "memory_writes": [],
                    "should_summarize": false,
                    "confidence": 0.7,
                    "safe_to_send": true
                })
                .to_string()
            } else {
                "Mock response from ferrum".to_string()
            };
            return Ok(LlmResponse {
                model: request.model,
                content,
                usage: Usage {
                    prompt_tokens: 25,
                    completion_tokens: 20,
                    estimated_cost_usd: 0.0,
                },
                latency_ms: 5,
            });
        }

        if let Some(caps) = self.capabilities.read().get(&request.model) {
            if !caps.supports_text {
                return Err(ProviderError::UnsupportedCapability(format!(
                    "model {} does not support text",
                    request.model
                )));
            }
            let _has_image = request
                .messages
                .iter()
                .flat_map(|message| message.parts.iter())
                .any(|part| matches!(part, ChatMessagePart::ImageUrl { .. }));
            let _has_audio = request
                .messages
                .iter()
                .flat_map(|message| message.parts.iter())
                .any(|part| matches!(part, ChatMessagePart::InputAudio { .. }));
        }

        let url = format!(
            "{}/chat/completions",
            self.cfg.base_url.trim_end_matches('/')
        );
        let payload = ChatCompletionRequest {
            model: request.model.clone(),
            messages: request
                .messages
                .into_iter()
                .map(OpenRouterMessage::from)
                .collect(),
            max_tokens: request.max_output_tokens,
            temperature: request.temperature,
            response_format: if request.require_json {
                Some(json!({"type": "json_object"}))
            } else {
                None
            },
        };

        let mut backoff_ms = 250_u64;
        let max_attempts = 4_u32;

        for attempt in 1..=max_attempts {
            let started = Instant::now();
            let req = self.auth_headers(self.http.post(&url)).json(&payload);
            let send_fut = req.send();

            let response = timeout(Duration::from_millis(request.timeout_ms), send_fut)
                .await
                .map_err(|_| ProviderError::Timeout)?;

            match response {
                Ok(resp) => {
                    let status = resp.status();
                    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
                        return Err(ProviderError::AuthFailure);
                    }
                    if status == StatusCode::TOO_MANY_REQUESTS {
                        if attempt < max_attempts {
                            sleep(Duration::from_millis(backoff_ms)).await;
                            backoff_ms *= 2;
                            continue;
                        }
                        return Err(ProviderError::RateLimit);
                    }
                    if status == StatusCode::NOT_FOUND {
                        return Err(ProviderError::BadModelId(request.model.clone()));
                    }
                    if status.is_server_error() {
                        if attempt < max_attempts {
                            sleep(Duration::from_millis(backoff_ms)).await;
                            backoff_ms *= 2;
                            continue;
                        }
                        let body = resp.text().await.unwrap_or_default();
                        return Err(ProviderError::UpstreamFailure(format!(
                            "status {status} body {}",
                            body.chars().take(256).collect::<String>()
                        )));
                    }
                    if !status.is_success() {
                        let body = resp.text().await.unwrap_or_default();
                        return Err(ProviderError::UpstreamFailure(format!(
                            "status {status} body {}",
                            body.chars().take(256).collect::<String>()
                        )));
                    }

                    let parsed: ChatCompletionResponse = resp
                        .json()
                        .await
                        .map_err(|e| ProviderError::MalformedResponse(e.to_string()))?;
                    let content = parsed
                        .choices
                        .first()
                        .and_then(|c| extract_message_content(c.message.content.as_ref()))
                        .ok_or_else(|| {
                            ProviderError::MalformedResponse(
                                "missing choices[0].message.content".to_string(),
                            )
                        })?;
                    let usage = parsed.usage.unwrap_or(ResponseUsage {
                        prompt_tokens: Some(0),
                        completion_tokens: Some(0),
                        cost: Some(0.0),
                    });

                    return Ok(LlmResponse {
                        model: parsed.model,
                        content,
                        usage: Usage {
                            prompt_tokens: usage.prompt_tokens.unwrap_or(0),
                            completion_tokens: usage.completion_tokens.unwrap_or(0),
                            estimated_cost_usd: usage.cost.unwrap_or(0.0),
                        },
                        latency_ms: started.elapsed().as_millis() as u64,
                    });
                }
                Err(err) => {
                    if attempt < max_attempts {
                        sleep(Duration::from_millis(backoff_ms)).await;
                        backoff_ms *= 2;
                        continue;
                    }
                    return Err(map_middleware_error(err));
                }
            }
        }

        Err(ProviderError::UpstreamFailure(
            "exhausted retries without response".to_string(),
        ))
    }

    fn model_catalog(&self) -> Vec<ModelMetadata> {
        self.catalog.read().values().cloned().collect()
    }
}

fn into_openrouter_content(message: ChatMessage) -> OpenRouterContent {
    if message.parts.is_empty() {
        return OpenRouterContent::Text(message.content);
    }

    let mut parts = Vec::new();
    if !message.content.trim().is_empty() {
        parts.push(OpenRouterContentPart::Text {
            text: message.content,
        });
    }
    for part in message.parts {
        match part {
            ChatMessagePart::Text { text } => parts.push(OpenRouterContentPart::Text { text }),
            ChatMessagePart::ImageUrl { url } => {
                parts.push(OpenRouterContentPart::ImageUrl {
                    image_url: OpenRouterImageUrl { url },
                });
            }
            ChatMessagePart::InputAudio { data, format } => {
                parts.push(OpenRouterContentPart::InputAudio {
                    input_audio: OpenRouterInputAudio { data, format },
                });
            }
        }
    }
    OpenRouterContent::Parts(parts)
}

fn extract_message_content(content: Option<&serde_json::Value>) -> Option<String> {
    let content = content?;
    match content {
        serde_json::Value::String(text) => Some(text.clone()),
        serde_json::Value::Array(parts) => {
            let joined = parts
                .iter()
                .filter_map(|part| {
                    part.get("text")
                        .and_then(|value| value.as_str())
                        .map(ToString::to_string)
                        .or_else(|| {
                            part.get("text")
                                .and_then(|value| value.get("value"))
                                .and_then(|value| value.as_str())
                                .map(ToString::to_string)
                        })
                })
                .collect::<Vec<_>>()
                .join("");
            if joined.trim().is_empty() {
                None
            } else {
                Some(joined)
            }
        }
        _ => None,
    }
}

fn deserialize_cost_field<'de, D>(deserializer: D) -> Result<Option<f64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;

    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(match value {
        Some(serde_json::Value::String(raw)) => raw.parse::<f64>().ok().map(|v| v * 1_000_000.0),
        Some(serde_json::Value::Number(n)) => n.as_f64().map(|v| v * 1_000_000.0),
        _ => None,
    })
}

fn map_reqwest_error(err: reqwest::Error) -> ProviderError {
    if err.is_timeout() {
        return ProviderError::Timeout;
    }
    ProviderError::Network(err.to_string())
}

fn map_middleware_error(err: reqwest_middleware::Error) -> ProviderError {
    match err {
        reqwest_middleware::Error::Reqwest(err) => map_reqwest_error(err),
        other => {
            let message = other.to_string().to_ascii_lowercase();
            if message.contains("429")
                || message.contains("too many requests")
                || message.contains("rate limit")
            {
                return ProviderError::RateLimit;
            }
            if message.contains("timed out") || message.contains("timeout") {
                return ProviderError::Timeout;
            }
            ProviderError::Network(other.to_string())
        }
    }
}

impl From<ModelEntry> for ModelMetadata {
    fn from(value: ModelEntry) -> Self {
        let pricing = value.pricing;
        let mut modalities = value.modality.unwrap_or_default();
        if let Some(arch) = value.architecture {
            modalities.extend(arch.input_modalities);
            modalities.extend(arch.output_modalities);
        }
        let lower = modalities
            .into_iter()
            .map(|m| m.to_lowercase())
            .collect::<Vec<_>>();
        Self {
            id: value.id,
            context_length: value.context_length,
            supports_text: lower.iter().any(|m| m.contains("text")) || lower.is_empty(),
            supports_tools: true,
            supports_vision: lower
                .iter()
                .any(|m| m.contains("image") || m.contains("vision")),
            supports_audio_input: lower.iter().any(|m| m.contains("audio")),
            supports_image_output: lower.iter().any(|m| m.contains("image")),
            prompt_cost_per_million: pricing.as_ref().and_then(|p| p.prompt),
            completion_cost_per_million: pricing.as_ref().and_then(|p| p.completion),
        }
    }
}

impl From<ChatMessage> for OpenRouterMessage {
    fn from(value: ChatMessage) -> Self {
        let ChatMessage {
            role,
            content,
            parts,
        } = value;
        Self {
            role,
            content: into_openrouter_content(ChatMessage {
                role: String::new(),
                content,
                parts,
            }),
        }
    }
}
