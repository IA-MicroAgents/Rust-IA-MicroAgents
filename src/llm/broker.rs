use crate::team::config::{EscalationTier, PerformancePolicy};
use crate::{identity::schema::ModelRoutes, llm::models::ModelMetadata};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreferredTier {
    RouterFast,
    FastText,
    ToolUse,
    ReviewerFast,
    ReviewerStrict,
    Planner,
    Reasoning,
    Vision,
    Audio,
    ImageGenerate,
}

#[derive(Debug, Clone, Default)]
pub struct ModelBroker {
    catalog: Vec<ModelMetadata>,
}

impl ModelBroker {
    pub fn new(catalog: Vec<ModelMetadata>) -> Self {
        Self { catalog }
    }

    pub fn resolve(
        &self,
        routes: &ModelRoutes,
        request: ModelSelectionRequest<'_>,
    ) -> ModelSelection {
        let mut candidates = Vec::new();
        if let Some(primary) = routes.route_value(request.route_key) {
            candidates.push(primary.to_string());
        }
        candidates.extend(routes.fallback.iter().cloned());
        candidates.sort();
        candidates.dedup();

        let mut best: Option<(&ModelMetadata, f64)> = None;
        for candidate in &candidates {
            if is_auto_model_id(candidate) {
                continue;
            }
            let Some(meta) = self.catalog.iter().find(|meta| meta.id == *candidate) else {
                continue;
            };
            let Some(score) = score_candidate(meta, &request, routes) else {
                continue;
            };
            let Some((best_meta, best_score)) = best else {
                best = Some((meta, score));
                continue;
            };
            if score > best_score
                || (score == best_score
                    && compare_estimated_turn_cost(meta, &request, best_meta)
                        == std::cmp::Ordering::Less)
            {
                best = Some((meta, score));
            }
        }

        if let Some((meta, score)) = best {
            return ModelSelection {
                route_key: request.route_key.to_string(),
                resolved_model: meta.id.clone(),
                reason: selection_reason(meta, &request, score),
            };
        }

        if let Some(meta) = self
            .catalog
            .iter()
            .filter_map(|meta| {
                (!is_auto_model_id(&meta.id))
                    .then_some(meta)
                    .and_then(|meta| {
                        score_candidate(meta, &request, routes).map(|score| (meta, score))
                    })
            })
            .max_by(|(left, left_score), (right, right_score)| {
                left_score
                    .partial_cmp(right_score)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| compare_estimated_turn_cost(right, &request, left))
            })
        {
            let (meta, score) = meta;
            return ModelSelection {
                route_key: request.route_key.to_string(),
                resolved_model: meta.id.clone(),
                reason: format!(
                    "catalog_fallback: {}",
                    selection_reason(meta, &request, score)
                ),
            };
        }

        let fallback_model = candidates
            .iter()
            .find(|candidate| !is_auto_model_id(candidate))
            .cloned()
            .or_else(|| {
                routes
                    .fallback
                    .iter()
                    .find(|candidate| !is_auto_model_id(candidate))
                    .cloned()
            })
            .unwrap_or_else(|| {
                routes
                    .route_value(request.route_key)
                    .unwrap_or(routes.fast.as_str())
                    .to_string()
            });

        ModelSelection {
            route_key: request.route_key.to_string(),
            resolved_model: fallback_model,
            reason: "fallback_to_primary_route".to_string(),
        }
    }
}

fn is_auto_model_id(model_id: &str) -> bool {
    let normalized = model_id.trim().to_ascii_lowercase();
    normalized == "openrouter/auto" || normalized.ends_with("/auto")
}

fn supports_request(meta: &ModelMetadata, request: &ModelSelectionRequest<'_>) -> bool {
    if !meta.supports_text {
        return false;
    }
    if request.requires_tools && !meta.supports_tools {
        return false;
    }
    if matches!(request.input_modality, InputModality::Image) && !meta.supports_vision {
        return false;
    }
    if matches!(request.input_modality, InputModality::Audio) && !meta.supports_audio_input {
        return false;
    }
    if matches!(request.output_modality, OutputModality::Image) && !meta.supports_image_output {
        return false;
    }
    if matches!(request.reasoning_level, ReasoningLevel::High)
        && meta.context_length.unwrap_or(0) < 16_000
    {
        return false;
    }
    if let Some(max_cost) = request.max_cost_usd {
        if let Some(estimated) = estimated_turn_cost(meta, request) {
            if estimated > max_cost * 1.20 {
                return false;
            }
        }
    }
    if let Some(max_latency_ms) = request.max_latency_ms {
        if max_latency_ms <= 2_000 && speed_rank(meta) < 1.6 {
            return false;
        }
    }
    true
}

fn estimated_turn_cost(meta: &ModelMetadata, request: &ModelSelectionRequest<'_>) -> Option<f64> {
    let prompt_rate = meta.prompt_cost_per_million?;
    let completion_rate = meta.completion_cost_per_million?;
    let (prompt_tokens, completion_tokens) = estimated_token_profile(request);
    Some(
        (prompt_tokens as f64 / 1_000_000.0) * prompt_rate
            + (completion_tokens as f64 / 1_000_000.0) * completion_rate,
    )
}

fn estimated_token_profile(request: &ModelSelectionRequest<'_>) -> (u32, u32) {
    let prompt_tokens = match request.route_key {
        "router_fast" => 500,
        "fast_text" => 900,
        "tool_use" => 1_600,
        "reviewer_fast" => 1_200,
        "reviewer_strict" => 1_800,
        "planner" => 2_400,
        "reasoning" => 2_600,
        "integrator_complex" => 3_000,
        "vision_understand" => 1_400,
        "audio_transcribe" => 2_500,
        "image_generate" => 900,
        _ => 1_200,
    };
    let completion_tokens = match request.output_modality {
        OutputModality::Json => match request.reasoning_level {
            ReasoningLevel::Low => 220,
            ReasoningLevel::Medium => 320,
            ReasoningLevel::High => 500,
        },
        OutputModality::Text => match request.reasoning_level {
            ReasoningLevel::Low => 260,
            ReasoningLevel::Medium => 520,
            ReasoningLevel::High => 850,
        },
        OutputModality::Image => 120,
    };
    (prompt_tokens, completion_tokens)
}

fn compare_estimated_turn_cost(
    left: &ModelMetadata,
    request: &ModelSelectionRequest<'_>,
    right: &ModelMetadata,
) -> std::cmp::Ordering {
    estimated_turn_cost(left, request)
        .unwrap_or(f64::MAX)
        .partial_cmp(&estimated_turn_cost(right, request).unwrap_or(f64::MAX))
        .unwrap_or(std::cmp::Ordering::Equal)
}

fn preferred_tier(request: &ModelSelectionRequest<'_>) -> PreferredTier {
    match request.route_key {
        "router_fast" => PreferredTier::RouterFast,
        "fast_text" => PreferredTier::FastText,
        "tool_use" => PreferredTier::ToolUse,
        "reviewer_fast" => PreferredTier::ReviewerFast,
        "reviewer_strict" => PreferredTier::ReviewerStrict,
        "planner" => PreferredTier::Planner,
        "reasoning" | "integrator_complex" => PreferredTier::Reasoning,
        "vision_understand" | "vision" => PreferredTier::Vision,
        "audio_transcribe" => PreferredTier::Audio,
        "image_generate" => PreferredTier::ImageGenerate,
        _ => match request.reasoning_level {
            ReasoningLevel::High => PreferredTier::Reasoning,
            ReasoningLevel::Medium => PreferredTier::FastText,
            ReasoningLevel::Low => PreferredTier::RouterFast,
        },
    }
}

fn score_candidate(
    meta: &ModelMetadata,
    request: &ModelSelectionRequest<'_>,
    routes: &ModelRoutes,
) -> Option<f64> {
    if !supports_request(meta, request) {
        return None;
    }

    let tier = preferred_tier(request);
    let mut score = match tier {
        PreferredTier::RouterFast => speed_rank(meta) * 1.3 + reasoning_rank(meta) * 0.25,
        PreferredTier::FastText => speed_rank(meta) * 1.05 + reasoning_rank(meta) * 0.45,
        PreferredTier::ToolUse => reasoning_rank(meta) * 0.72 + speed_rank(meta) * 0.72,
        PreferredTier::ReviewerFast => reasoning_rank(meta) * 0.85 + speed_rank(meta) * 0.4,
        PreferredTier::ReviewerStrict | PreferredTier::Planner | PreferredTier::Reasoning => {
            reasoning_rank(meta) * 1.35 + context_rank(meta) * 0.25
        }
        PreferredTier::Vision => vision_rank(meta) * 1.2 + reasoning_rank(meta) * 0.25,
        PreferredTier::Audio => audio_rank(meta) * 1.25 + speed_rank(meta) * 0.15,
        PreferredTier::ImageGenerate => image_generation_rank(meta) * 1.25,
    };

    if request.requires_tools {
        score += 0.35;
    }

    score += match request.reasoning_level {
        ReasoningLevel::Low => speed_rank(meta) * 0.15,
        ReasoningLevel::Medium => reasoning_rank(meta) * 0.15,
        ReasoningLevel::High => reasoning_rank(meta) * 0.35,
    };

    if let Some(primary) = routes.route_value(request.route_key) {
        if meta.id.eq_ignore_ascii_case(primary) {
            score += 0.35;
        }
    }

    score += performance_policy_adjustment(meta, request, routes);
    score += escalation_adjustment(meta, request, routes);

    if let Some(estimated_cost) = estimated_turn_cost(meta, request) {
        if let Some(max_cost) = request.max_cost_usd {
            let ratio = estimated_cost / max_cost.max(0.000_001);
            if ratio <= 0.35 {
                score += 0.50;
            } else if ratio <= 0.70 {
                score += 0.30;
            } else if ratio <= 1.0 {
                score += 0.10;
            } else {
                score -= (ratio - 1.0).min(1.0) * 0.75;
            }
        }
        score -= estimated_cost.min(0.05) * 12.0;
    } else {
        score -= 0.2;
    }

    if let Some(max_latency_ms) = request.max_latency_ms {
        if max_latency_ms <= 2_500 {
            score += speed_rank(meta) * 0.35;
            score -= reasoning_rank(meta).max(0.0) * 0.08;
        } else if max_latency_ms >= 8_000 {
            score += reasoning_rank(meta) * 0.1;
        }
    }

    Some(score)
}

fn performance_policy_adjustment(
    meta: &ModelMetadata,
    request: &ModelSelectionRequest<'_>,
    routes: &ModelRoutes,
) -> f64 {
    let primary_reasoning_rank = routes
        .route_value(request.route_key)
        .and_then(|primary| {
            (!is_auto_model_id(primary))
                .then_some(primary)
                .and_then(|primary| routes_model_match_reasoning(primary, meta))
        })
        .unwrap_or_else(|| reasoning_rank(meta));
    let cost = estimated_turn_cost(meta, request).unwrap_or(0.0);
    match request.performance_policy {
        PerformancePolicy::Fast => {
            speed_rank(meta) * 0.55
                - reasoning_rank(meta).max(primary_reasoning_rank) * 0.06
                - cost.min(0.05) * 22.0
        }
        PerformancePolicy::BalancedFast => {
            let route_bias = match request.route_key {
                "router_fast" | "fast_text" | "tool_use" | "reviewer_fast" => {
                    speed_rank(meta) * 0.42 - reasoning_rank(meta) * 0.05
                }
                "planner" | "reasoning" | "reviewer_strict" | "integrator_complex" => {
                    reasoning_rank(meta) * 0.18 + context_rank(meta) * 0.06
                }
                _ => speed_rank(meta) * 0.16,
            };
            route_bias - cost.min(0.05) * 10.0
        }
        PerformancePolicy::MaxQuality => {
            reasoning_rank(meta) * 0.45 + context_rank(meta) * 0.12 - cost.min(0.05) * 3.0
        }
    }
}

fn escalation_adjustment(
    meta: &ModelMetadata,
    request: &ModelSelectionRequest<'_>,
    routes: &ModelRoutes,
) -> f64 {
    let primary_reasoning = routes
        .route_value(request.route_key)
        .and_then(|primary| {
            if is_auto_model_id(primary) {
                None
            } else {
                Some(primary.to_ascii_lowercase())
            }
        })
        .map(|id| {
            reasoning_rank(&ModelMetadata {
                id,
                context_length: meta.context_length,
                supports_text: meta.supports_text,
                supports_tools: meta.supports_tools,
                supports_vision: meta.supports_vision,
                supports_audio_input: meta.supports_audio_input,
                supports_image_output: meta.supports_image_output,
                prompt_cost_per_million: meta.prompt_cost_per_million,
                completion_cost_per_million: meta.completion_cost_per_million,
            })
        })
        .unwrap_or(0.0);
    let delta = (reasoning_rank(meta) - primary_reasoning).max(0.0);
    match request.escalation_tier {
        EscalationTier::Conservative => {
            if delta > 0.35 {
                -0.8
            } else {
                -0.15
            }
        }
        EscalationTier::Standard => {
            if matches!(request.reasoning_level, ReasoningLevel::High) {
                delta * 0.08
            } else {
                0.0
            }
        }
        EscalationTier::Aggressive => {
            if matches!(
                request.reasoning_level,
                ReasoningLevel::High | ReasoningLevel::Medium
            ) {
                delta * 0.25
            } else {
                delta * 0.08
            }
        }
    }
}

fn routes_model_match_reasoning(route_model: &str, fallback_meta: &ModelMetadata) -> Option<f64> {
    Some(reasoning_rank(&ModelMetadata {
        id: route_model.to_ascii_lowercase(),
        context_length: fallback_meta.context_length,
        supports_text: fallback_meta.supports_text,
        supports_tools: fallback_meta.supports_tools,
        supports_vision: fallback_meta.supports_vision,
        supports_audio_input: fallback_meta.supports_audio_input,
        supports_image_output: fallback_meta.supports_image_output,
        prompt_cost_per_million: fallback_meta.prompt_cost_per_million,
        completion_cost_per_million: fallback_meta.completion_cost_per_million,
    }))
}

fn selection_reason(
    meta: &ModelMetadata,
    request: &ModelSelectionRequest<'_>,
    score: f64,
) -> String {
    format!(
        "route={} input={:?} output={:?} reasoning={:?} tools={} policy={:?} escalation={:?} score={:.2} estimated_turn_cost_usd={:?} prompt_cost_per_million={:?} completion_cost_per_million={:?}",
        request.route_key,
        request.input_modality,
        request.output_modality,
        request.reasoning_level,
        request.requires_tools,
        request.performance_policy,
        request.escalation_tier,
        score,
        estimated_turn_cost(meta, request),
        meta.prompt_cost_per_million,
        meta.completion_cost_per_million
    )
}

fn speed_rank(meta: &ModelMetadata) -> f64 {
    let id = meta.id.to_ascii_lowercase();
    if contains_any(&id, &["nano"]) {
        4.0
    } else if contains_any(&id, &["flash", "haiku", "mini", "small"]) {
        3.4
    } else if contains_any(&id, &["4o", "4.1-mini"]) {
        3.0
    } else if contains_any(&id, &["4.1", "sonnet"]) {
        2.3
    } else if contains_any(&id, &["o3", "o4", "opus", "r1"]) {
        1.6
    } else {
        2.4
    }
}

fn reasoning_rank(meta: &ModelMetadata) -> f64 {
    let id = meta.id.to_ascii_lowercase();
    if contains_any(
        &id,
        &[
            "o3",
            "o4",
            "gpt-5",
            "opus",
            "r1",
            "gemini-2.5-pro",
            "claude-opus",
        ],
    ) {
        4.8
    } else if contains_any(
        &id,
        &[
            "gpt-4.1",
            "sonnet",
            "gemini-2.5-flash",
            "deepseek",
            "reason",
        ],
    ) {
        if id.contains("mini") {
            3.6
        } else {
            4.2
        }
    } else if contains_any(&id, &["4o-mini", "4o", "flash", "haiku", "mini"]) {
        2.8
    } else if contains_any(&id, &["nano", "small"]) {
        1.4
    } else {
        2.6
    }
}

fn context_rank(meta: &ModelMetadata) -> f64 {
    let context = meta.context_length.unwrap_or(8_192) as f64;
    (context / 32_000.0).min(4.0)
}

fn vision_rank(meta: &ModelMetadata) -> f64 {
    if !meta.supports_vision {
        return 0.0;
    }
    let id = meta.id.to_ascii_lowercase();
    if contains_any(&id, &["4o", "vision", "gemini", "claude"]) {
        4.2
    } else {
        3.1
    }
}

fn audio_rank(meta: &ModelMetadata) -> f64 {
    if !meta.supports_audio_input {
        return 0.0;
    }
    let id = meta.id.to_ascii_lowercase();
    if contains_any(&id, &["transcribe", "audio"]) {
        4.6
    } else if contains_any(&id, &["4o", "gemini"]) {
        3.8
    } else {
        3.0
    }
}

fn image_generation_rank(meta: &ModelMetadata) -> f64 {
    if !meta.supports_image_output {
        return 0.0;
    }
    let id = meta.id.to_ascii_lowercase();
    if contains_any(&id, &["image", "gpt-image", "flux", "sdxl"]) {
        4.4
    } else {
        3.0
    }
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
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
            fast: "openai/gpt-4o-mini".to_string(),
            reasoning: "openai/gpt-4.1".to_string(),
            tool_use: "openai/gpt-4.1-mini".to_string(),
            vision: "openai/gpt-4o-mini".to_string(),
            reviewer: "openai/gpt-4o-mini".to_string(),
            planner: "openai/gpt-4.1".to_string(),
            router_fast: Some("openai/gpt-4o-mini".to_string()),
            fast_text: Some("openai/gpt-4.1-mini".to_string()),
            reviewer_fast: Some("openai/gpt-4o-mini".to_string()),
            reviewer_strict: Some("openai/gpt-4.1".to_string()),
            integrator_complex: Some("openai/gpt-4.1".to_string()),
            vision_understand: None,
            audio_transcribe: None,
            image_generate: None,
            fallback: vec!["openai/gpt-4o-mini".to_string()],
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
                id: "openai/gpt-4.1-mini".to_string(),
                context_length: Some(128_000),
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
        ]
    }

    #[test]
    fn prefers_stronger_reasoning_model_for_reasoning_tasks() {
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
        assert_eq!(selection.resolved_model, "openai/gpt-4.1");
    }

    #[test]
    fn prefers_fast_model_for_router_fast() {
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
        assert_eq!(selection.resolved_model, "openai/gpt-4o-mini");
    }
}
