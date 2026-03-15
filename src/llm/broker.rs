use crate::team::config::{EscalationTier, PerformancePolicy};
use crate::{
    identity::schema::ModelRoutes,
    llm::{models::ModelMetadata, OPENROUTER_FREE_MODEL},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputModality {
    Text,
    Image,
    Audio,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputModality {
    Text,
    Json,
    Image,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningLevel {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone)]
pub struct ModelSelectionRequest<'a> {
    pub route_key: &'a str,
    pub input_modality: InputModality,
    pub output_modality: OutputModality,
    pub reasoning_level: ReasoningLevel,
    pub requires_tools: bool,
    pub max_cost_usd: Option<f64>,
    pub max_latency_ms: Option<u64>,
    pub performance_policy: PerformancePolicy,
    pub escalation_tier: EscalationTier,
}

#[derive(Debug, Clone)]
pub struct ModelSelection {
    pub route_key: String,
    pub resolved_model: String,
    pub reason: String,
}

#[derive(Debug, Clone, Default)]
pub struct ModelBroker {
    _catalog: Vec<ModelMetadata>,
}

impl ModelBroker {
    pub fn new(catalog: Vec<ModelMetadata>) -> Self {
        Self { _catalog: catalog }
    }

    pub fn resolve(
        &self,
        _routes: &ModelRoutes,
        request: ModelSelectionRequest<'_>,
    ) -> ModelSelection {
        ModelSelection {
            route_key: request.route_key.to_string(),
            resolved_model: OPENROUTER_FREE_MODEL.to_string(),
            reason: "forced_openrouter_free".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::identity::schema::ModelRoutes;

    use super::{
        InputModality, ModelBroker, ModelSelectionRequest, OutputModality, ReasoningLevel,
    };
    use crate::llm::models::ModelMetadata;

    fn routes() -> ModelRoutes {
        ModelRoutes {
            fast: crate::llm::OPENROUTER_FREE_MODEL.to_string(),
            reasoning: crate::llm::OPENROUTER_FREE_MODEL.to_string(),
            tool_use: crate::llm::OPENROUTER_FREE_MODEL.to_string(),
            vision: crate::llm::OPENROUTER_FREE_MODEL.to_string(),
            reviewer: crate::llm::OPENROUTER_FREE_MODEL.to_string(),
            planner: crate::llm::OPENROUTER_FREE_MODEL.to_string(),
            router_fast: Some(crate::llm::OPENROUTER_FREE_MODEL.to_string()),
            fast_text: Some(crate::llm::OPENROUTER_FREE_MODEL.to_string()),
            reviewer_fast: Some(crate::llm::OPENROUTER_FREE_MODEL.to_string()),
            reviewer_strict: Some(crate::llm::OPENROUTER_FREE_MODEL.to_string()),
            integrator_complex: Some(crate::llm::OPENROUTER_FREE_MODEL.to_string()),
            vision_understand: None,
            audio_transcribe: None,
            image_generate: None,
            fallback: vec![crate::llm::OPENROUTER_FREE_MODEL.to_string()],
        }
    }

    fn catalog() -> Vec<ModelMetadata> {
        vec![
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
                id: "google/gemini-2.5-flash:free".to_string(),
                context_length: Some(128_000),
                supports_text: true,
                supports_tools: true,
                supports_vision: false,
                supports_audio_input: false,
                supports_image_output: false,
                prompt_cost_per_million: Some(0.0),
                completion_cost_per_million: Some(0.0),
            },
        ]
    }

    #[test]
    fn forces_openrouter_free_for_reasoning_tasks() {
        let broker = ModelBroker::new(catalog());
        let selection = broker.resolve(
            &routes(),
            ModelSelectionRequest {
                route_key: "reasoning",
                input_modality: InputModality::Text,
                output_modality: OutputModality::Text,
                reasoning_level: ReasoningLevel::High,
                requires_tools: false,
                max_cost_usd: Some(0.05),
                max_latency_ms: Some(12_000),
                performance_policy: crate::team::config::PerformancePolicy::BalancedFast,
                escalation_tier: crate::team::config::EscalationTier::Standard,
            },
        );
        assert_eq!(selection.resolved_model, crate::llm::OPENROUTER_FREE_MODEL);
        assert_eq!(selection.reason, "forced_openrouter_free");
    }

    #[test]
    fn forces_openrouter_free_for_router_fast() {
        let broker = ModelBroker::new(catalog());
        let selection = broker.resolve(
            &routes(),
            ModelSelectionRequest {
                route_key: "router_fast",
                input_modality: InputModality::Text,
                output_modality: OutputModality::Json,
                reasoning_level: ReasoningLevel::Low,
                requires_tools: false,
                max_cost_usd: Some(0.01),
                max_latency_ms: Some(2_000),
                performance_policy: crate::team::config::PerformancePolicy::BalancedFast,
                escalation_tier: crate::team::config::EscalationTier::Standard,
            },
        );
        assert_eq!(selection.resolved_model, crate::llm::OPENROUTER_FREE_MODEL);
    }

    #[test]
    fn ignores_catalog_and_keeps_openrouter_free() {
        let broker = ModelBroker::new(catalog());
        let selection = broker.resolve(
            &routes(),
            ModelSelectionRequest {
                route_key: "fast_text",
                input_modality: InputModality::Text,
                output_modality: OutputModality::Text,
                reasoning_level: ReasoningLevel::Medium,
                requires_tools: false,
                max_cost_usd: Some(0.05),
                max_latency_ms: Some(12_000),
                performance_policy: crate::team::config::PerformancePolicy::BalancedFast,
                escalation_tier: crate::team::config::EscalationTier::Standard,
            },
        );
        assert_eq!(selection.resolved_model, crate::llm::OPENROUTER_FREE_MODEL);
        assert_eq!(selection.reason, "forced_openrouter_free");
    }
}
