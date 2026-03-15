use crate::{
    errors::AppResult,
    llm::{
        response_types::{ExecutionPlanContract, OrchestrationDecision},
        ChatMessage, LlmProvider, LlmRequest,
    },
};

pub async fn parse_or_repair_decision(
    provider: &dyn LlmProvider,
    model: &str,
    raw_content: &str,
    timeout_ms: u64,
) -> AppResult<OrchestrationDecision> {
    if let Ok(parsed) = serde_json::from_str::<OrchestrationDecision>(raw_content) {
        return Ok(parsed);
    }

    let repair_prompt = vec![
        ChatMessage::text(
            "system",
            "Repair malformed JSON. Return valid JSON object only with required fields route, assistant_reply, tool_calls, memory_writes, should_summarize, confidence, safe_to_send",
        ),
        ChatMessage::text("user", raw_content),
    ];

    let repair = provider
        .chat_completion(LlmRequest {
            model: model.to_string(),
            messages: repair_prompt,
            max_output_tokens: 700,
            temperature: 0.0,
            require_json: true,
            timeout_ms,
        })
        .await;

    if let Ok(repaired) = repair {
        if let Ok(parsed) = serde_json::from_str::<OrchestrationDecision>(&repaired.content) {
            return Ok(parsed);
        }
    }

    Ok(OrchestrationDecision::safe_fallback(
        "I hit an internal parsing issue. Can you rephrase in one sentence?",
    ))
}

pub async fn parse_or_repair_execution_plan(
    provider: &dyn LlmProvider,
    model: &str,
    raw_content: &str,
    timeout_ms: u64,
) -> AppResult<ExecutionPlanContract> {
    if let Ok(parsed) = serde_json::from_str::<ExecutionPlanContract>(raw_content) {
        return Ok(parsed);
    }

    let repair_prompt = vec![
        ChatMessage::text(
            "system",
            "Repair malformed JSON. Return valid JSON object only with fields goal, assumptions, risks, tasks, parallelizable_groups. Every task needs id, title, description, dependencies, acceptance_criteria, candidate_role, model_route, expected_artifact, estimated_cost_usd, estimated_ms. model_route must be one of router_fast, fast_text, tool_use, reviewer_fast, reviewer_strict, reasoning, vision_understand, audio_transcribe, image_generate and represents a capability route, not a concrete model id.",
        ),
        ChatMessage::text("user", raw_content),
    ];

    let repair = provider
        .chat_completion(LlmRequest {
            model: model.to_string(),
            messages: repair_prompt,
            max_output_tokens: 1400,
            temperature: 0.0,
            require_json: true,
            timeout_ms,
        })
        .await;

    if let Ok(repaired) = repair {
        if let Ok(parsed) = serde_json::from_str::<ExecutionPlanContract>(&repaired.content) {
            return Ok(parsed);
        }
    }

    Ok(ExecutionPlanContract {
        goal: String::new(),
        assumptions: vec!["planner repair failed".to_string()],
        risks: vec!["planner output could not be repaired".to_string()],
        tasks: Vec::new(),
        parallelizable_groups: Vec::new(),
    })
}
