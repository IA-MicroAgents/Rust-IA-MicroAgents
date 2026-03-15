use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelMetadata {
    pub id: String,
    pub context_length: Option<u64>,
    pub supports_text: bool,
    pub supports_tools: bool,
    pub supports_vision: bool,
    pub supports_audio_input: bool,
    pub supports_image_output: bool,
    pub prompt_cost_per_million: Option<f64>,
    pub completion_cost_per_million: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCapabilities {
    pub id: String,
    pub context_length: Option<u64>,
    pub supports_text: bool,
    pub supports_tools: bool,
    pub supports_vision: bool,
    pub supports_audio_input: bool,
    pub supports_image_output: bool,
    pub prompt_cost_per_million: Option<f64>,
    pub completion_cost_per_million: Option<f64>,
}
