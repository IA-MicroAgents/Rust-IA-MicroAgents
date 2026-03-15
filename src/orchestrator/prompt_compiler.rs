use crate::{
    identity::compiler::SystemIdentity,
    llm::{response_types::DecisionRoute, ChatMessage},
    memory::BrainMemory,
    orchestrator::context::TurnContext,
    skills::SkillDefinition,
    storage::ConversationTurn,
};

pub fn compile_classifier_prompt(
    identity: &SystemIdentity,
    user_text: &str,
    recent_turns: &[ConversationTurn],
    latest_summary: Option<&str>,
    brain_memories: &[BrainMemory],
) -> Vec<ChatMessage> {
    let turns_block = recent_turns
        .iter()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|turn| format!("{}: {}", turn.role, turn.content))
        .collect::<Vec<_>>()
        .join("\n");

    let brain_block = render_brain_memories(brain_memories);
    let system = format!(
        "{}\n\nYou are the AI MicroAgents route classifier. Return STRICT JSON only with fields: route, assistant_reply, tool_calls, memory_writes, should_summarize, confidence, safe_to_send.\
\nassistant_reply must be an empty string. tool_calls and memory_writes must be empty arrays. should_summarize=false. safe_to_send=true unless the request is unsafe.\
\nChoose only one route: direct_reply, tool_use, plan_then_act, ignore, ask_clarification.\
\nPrefer plan_then_act for decomposable or delegable work. Prefer tool_use when external tools are clearly required. Prefer ask_clarification only when the user intent is materially underspecified after using the recent conversation context.\
\nIf the user refers to previous items like 'estos', 'los 3', 'ese', 'como antes', or corrections such as 'no no', resolve them from recent turns before asking again.\
\nWhen inferring the final reply language, prefer the user's language; if unclear, prefer the identity locale.",
        identity.compiled_system_prompt
    );

    let user = format!(
        "User message:\n{}\n\nRecent turns:\n{}\n\nLatest summary:\n{}\n\nRelevant brain memories:\n{}",
        user_text,
        if turns_block.is_empty() {
            "(none)".to_string()
        } else {
            turns_block
        },
        latest_summary.unwrap_or("(none)"),
        brain_block
    );

    vec![
        ChatMessage::text("system", system),
        ChatMessage::text("user", user),
    ]
}

pub fn compile_decision_prompt(
    identity: &SystemIdentity,
    route_hint: DecisionRoute,
    user_text: &str,
    context: &TurnContext,
) -> Vec<ChatMessage> {
    let skills_block = render_skills(&context.selected_skills);
    let turns_block = context
        .recent_turns
        .iter()
        .map(|t| format!("{}: {}", t.role, t.content))
        .collect::<Vec<_>>()
        .join("\n");
    let memories_block = context.memories.join("\n- ");
    let brain_block = context.brain_memories_block(8);

    let summary_block = context
        .latest_summary
        .clone()
        .unwrap_or_else(|| "(none)".to_string());
    let working_set_block = context.working_set.render_for_prompt();
    let evidence_block = context
        .current_evidence
        .as_ref()
        .map(|bundle| bundle.render_for_prompt())
        .unwrap_or_else(|| "(none)".to_string());

    let system = format!(
        "{}\n\nReturn STRICT JSON only with fields: route, assistant_reply, tool_calls, memory_writes, should_summarize, confidence, safe_to_send.\
\nAllowed routes: direct_reply, tool_use, plan_then_act, ignore, ask_clarification.\
\nRoute hint: {:?}\n\nCandidate skills:\n{}\
\nBefore choosing ask_clarification, resolve references to previous turns whenever possible. If the user says things like 'los 3 que me pasaste', use the recent conversation and summary instead of asking for the same entities again unless the antecedent is still ambiguous.\
\nThe final answer must be in the user's language; if unclear, prefer the identity locale.",
        identity.compiled_system_prompt, route_hint, skills_block
    );

    let user = format!(
        "User message:\n{}\n\nRecent turns:\n{}\n\nLatest summary:\n{}\n\nRelevant brain memories:\n- {}\n\nRelevant memories:\n- {}\n\nConversation working set:\n{}\n\nEvidence bundle:\n{}",
        user_text,
        if turns_block.is_empty() {
            "(none)".to_string()
        } else {
            turns_block
        },
        summary_block,
        if brain_block.is_empty() {
            "(none)".to_string()
        } else {
            brain_block
        },
        if memories_block.is_empty() {
            "(none)".to_string()
        } else {
            memories_block
        },
        working_set_block,
        evidence_block,
    );

    vec![
        ChatMessage::text("system", system),
        ChatMessage::text("user", user),
    ]
}

pub fn compile_planning_prompt(
    identity: &SystemIdentity,
    user_text: &str,
    context: &TurnContext,
    roles: &[String],
    max_tasks: usize,
    max_depth: usize,
) -> Vec<ChatMessage> {
    let skills_block = render_skills(&context.selected_skills);
    let turns_block = context
        .recent_turns
        .iter()
        .map(|turn| format!("{}: {}", turn.role, turn.content))
        .collect::<Vec<_>>()
        .join("\n");
    let memories_block = context.memories.join("\n- ");
    let brain_block = context.brain_memories_block(8);
    let summary_block = context
        .latest_summary
        .clone()
        .unwrap_or_else(|| "(none)".to_string());
    let working_set_block = context.working_set.render_for_prompt();
    let evidence_block = context
        .current_evidence
        .as_ref()
        .map(|bundle| bundle.render_for_prompt())
        .unwrap_or_else(|| "(none)".to_string());
    let allowed_roles = if roles.is_empty() {
        "(none)".to_string()
    } else {
        roles.join(", ")
    };

    let system = format!(
        "{}\n\nYou are the AI MicroAgents supervisor planner. Always try to decompose non-trivial work into the smallest useful tasks that can run in parallel.\
\nReturn STRICT JSON only with fields: goal, assumptions, risks, tasks, parallelizable_groups.\
\nEach task must include: id, title, description, dependencies, acceptance_criteria, candidate_role, model_route, expected_artifact, estimated_cost_usd, estimated_ms, requires_live_data, evidence_inputs, analysis_track.\
\nDo NOT create the final integration task; runtime adds that itself.\
\nAllowed candidate_role values: {}\
\nAllowed model_route values: router_fast, fast_text, tool_use, reviewer_fast, reviewer_strict, planner, reasoning, integrator_complex, vision_understand, audio_transcribe, image_generate.\
\nChoose the most suitable capability route for each task. Runtime will resolve the exact model from the OpenRouter catalog using cost, capability, latency, and reasoning needs.\
\nUse recent conversation context to resolve references such as 'estos', 'los 3', 'el segundo', or corrections like 'no no'. Do not ask for entities already available in recent turns unless they are genuinely unclear.\
\nIf evidence bundle exists, treat it as mandatory source material for current-data tasks.\
\nHard limits: max_tasks={}, max_depth={}.\
\nPreserve the user's language in task descriptions and outputs; if unclear, prefer the identity locale.",
        identity.compiled_system_prompt, allowed_roles, max_tasks, max_depth
    );

    let user = format!(
        "User message:\n{}\n\nRecent turns:\n{}\n\nLatest summary:\n{}\n\nRelevant brain memories:\n- {}\n\nRelevant memories:\n- {}\n\nConversation working set:\n{}\n\nEvidence bundle:\n{}\n\nCandidate skills:\n{}",
        user_text,
        if turns_block.is_empty() {
            "(none)".to_string()
        } else {
            turns_block
        },
        summary_block,
        if brain_block.is_empty() {
            "(none)".to_string()
        } else {
            brain_block
        },
        if memories_block.is_empty() {
            "(none)".to_string()
        } else {
            memories_block
        },
        working_set_block,
        evidence_block,
        skills_block
    );

    vec![
        ChatMessage::text("system", system),
        ChatMessage::text("user", user),
    ]
}

pub fn compile_final_answer_prompt(
    identity: &SystemIdentity,
    user_text: &str,
    tool_results_json: &str,
    brain_memories: &[BrainMemory],
) -> Vec<ChatMessage> {
    let brain_block = render_brain_memories(brain_memories);
    vec![
        ChatMessage::text(
            "system",
            format!(
                "{}\n\nWrite the final reply for Telegram. Keep it concise, mobile-friendly, and factual. Do not expose internal JSON. Preserve references from the ongoing chat correctly. Reply in the user's language; if unclear, prefer the identity locale.",
                identity.compiled_system_prompt
            ),
        ),
        ChatMessage::text(
            "user",
            format!(
                "Original user message:\n{}\n\nRelevant brain memories:\n{}\n\nTool results JSON:\n{}\n\nProduce only the assistant reply text.",
                user_text, brain_block, tool_results_json
            ),
        ),
    ]
}

pub fn compile_fast_reply_prompt(
    identity: &SystemIdentity,
    route: &DecisionRoute,
    user_text: &str,
    recent_turns: &[ConversationTurn],
    brain_memories: &[BrainMemory],
) -> Vec<ChatMessage> {
    let turns_block = recent_turns
        .iter()
        .rev()
        .take(6)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|turn| format!("{}: {}", turn.role, turn.content))
        .collect::<Vec<_>>()
        .join("\n");
    let brain_block = render_brain_memories(brain_memories);
    let route_instruction = match route {
        DecisionRoute::AskClarification => {
            "Ask one concise clarification question that unblocks the next step."
        }
        DecisionRoute::DirectReply => {
            "Reply directly. Keep it concise, factual, and mobile-friendly."
        }
        DecisionRoute::Ignore => "Return an empty reply.",
        DecisionRoute::ToolUse => {
            "Do not invent tool results. If tool use is still required, ask a precise clarification."
        }
        DecisionRoute::PlanThenAct => {
            "Summarize the next step briefly. Do not expose internal planning."
        }
    };

    vec![
        ChatMessage::text(
            "system",
            format!(
                "{}\n\nYou are AI MicroAgents replying on Telegram. {} Resolve short follow-ups and corrections using the recent turns before asking for clarification. Reply in the user's language; if unclear, prefer the identity locale. Return plain text only.",
                identity.compiled_system_prompt, route_instruction
            ),
        ),
        ChatMessage::text(
            "user",
            format!(
                "User message:\n{}\n\nRecent turns:\n{}\n\nRelevant brain memories:\n{}",
                user_text,
                if turns_block.is_empty() {
                    "(none)".to_string()
                } else {
                    turns_block
                },
                brain_block
            ),
        ),
    ]
}

fn render_brain_memories(memories: &[BrainMemory]) -> String {
    if memories.is_empty() {
        return "(none)".to_string();
    }

    memories
        .iter()
        .map(|memory| format!("- {}", memory.render_for_prompt()))
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_skills(skills: &[SkillDefinition]) -> String {
    if skills.is_empty() {
        return "(none)".to_string();
    }

    skills
        .iter()
        .map(|s| {
            format!(
                "- {} ({:?}): {}\n  InputSchema: {}\n  OutputSchema: {}",
                s.manifest.name,
                s.manifest.kind,
                s.manifest.description,
                s.manifest
                    .input_schema
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "null".to_string()),
                s.manifest
                    .output_schema
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "null".to_string()),
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::{
        identity::{compiler::SystemIdentity, schema::*},
        orchestrator::context::TurnContext,
    };

    use super::{compile_decision_prompt, compile_planning_prompt};

    #[test]
    fn prompt_compiler_contains_json_contract() {
        let identity = SystemIdentity {
            frontmatter: IdentityFrontmatter {
                id: "ai-microagents".to_string(),
                display_name: "AI MicroAgents".to_string(),
                description: "test".to_string(),
                locale: "en-US".to_string(),
                timezone: "UTC".to_string(),
                model_routes: ModelRoutes {
                    fast: "a".to_string(),
                    reasoning: "b".to_string(),
                    tool_use: "c".to_string(),
                    vision: "d".to_string(),
                    reviewer: "e".to_string(),
                    planner: "f".to_string(),
                    router_fast: None,
                    fast_text: None,
                    reviewer_fast: None,
                    reviewer_strict: None,
                    integrator_complex: None,
                    vision_understand: None,
                    audio_transcribe: None,
                    image_generate: None,
                    fallback: vec![],
                },
                budgets: IdentityBudgets {
                    max_steps: 4,
                    max_turn_cost_usd: 0.1,
                    max_input_tokens: 2000,
                    max_output_tokens: 500,
                    max_tool_calls: 2,
                    timeout_ms: 10000,
                },
                memory: IdentityMemory {
                    save_facts: true,
                    save_summaries: true,
                    summarize_every_n_turns: 4,
                    brain_enabled: true,
                    precheck_each_turn: true,
                    auto_write_mode: "aggressive".to_string(),
                    conversation_limit: 4,
                    user_limit: 4,
                },
                permissions: IdentityPermissions {
                    allowed_skills: vec!["*".to_string()],
                    denied_skills: vec![],
                },
                channels: IdentityChannels {
                    telegram: TelegramIdentityChannel {
                        enabled: true,
                        max_reply_chars: 3500,
                        style_overrides: "short".to_string(),
                    },
                },
            },
            sections: crate::identity::compiler::CompiledIdentitySections {
                mission: "m".to_string(),
                persona: "p".to_string(),
                tone: "t".to_string(),
                hard_rules: "h".to_string(),
                do_not_do: "d".to_string(),
                escalation: "e".to_string(),
                memory_preferences: "m".to_string(),
                channel_notes: "c".to_string(),
                planning_principles: "pp".to_string(),
                review_standards: "rs".to_string(),
            },
            compiled_system_prompt: "identity".to_string(),
        };

        let context = TurnContext {
            conversation_id: 1,
            trace_id: "trace-test".to_string(),
            recent_turns: vec![],
            latest_summary: None,
            brain_memories: vec![],
            memories: vec![],
            working_set: crate::usecase::ConversationWorkingSet::default(),
            current_evidence: None,
            selected_skills: vec![],
            performance_policy: crate::team::config::PerformancePolicy::BalancedFast,
            max_escalation_tier: crate::team::config::EscalationTier::Standard,
            analysis_complexity: crate::usecase::AnalysisComplexity::Simple,
        };

        let messages = compile_decision_prompt(
            &identity,
            crate::llm::response_types::DecisionRoute::DirectReply,
            "hello",
            &context,
        );

        assert!(messages[0].content.contains("Return STRICT JSON only"));
        let _ = HashMap::<String, String>::new();
    }

    #[test]
    fn planning_prompt_contains_route_constraints() {
        let identity = SystemIdentity {
            frontmatter: IdentityFrontmatter {
                id: "ai-microagents".to_string(),
                display_name: "AI MicroAgents".to_string(),
                description: "test".to_string(),
                locale: "en-US".to_string(),
                timezone: "UTC".to_string(),
                model_routes: ModelRoutes {
                    fast: "a".to_string(),
                    reasoning: "b".to_string(),
                    tool_use: "c".to_string(),
                    vision: "d".to_string(),
                    reviewer: "e".to_string(),
                    planner: "f".to_string(),
                    router_fast: None,
                    fast_text: None,
                    reviewer_fast: None,
                    reviewer_strict: None,
                    integrator_complex: None,
                    vision_understand: None,
                    audio_transcribe: None,
                    image_generate: None,
                    fallback: vec![],
                },
                budgets: IdentityBudgets {
                    max_steps: 4,
                    max_turn_cost_usd: 0.1,
                    max_input_tokens: 2000,
                    max_output_tokens: 500,
                    max_tool_calls: 2,
                    timeout_ms: 10000,
                },
                memory: IdentityMemory {
                    save_facts: true,
                    save_summaries: true,
                    summarize_every_n_turns: 4,
                    brain_enabled: true,
                    precheck_each_turn: true,
                    auto_write_mode: "aggressive".to_string(),
                    conversation_limit: 4,
                    user_limit: 4,
                },
                permissions: IdentityPermissions {
                    allowed_skills: vec!["*".to_string()],
                    denied_skills: vec![],
                },
                channels: IdentityChannels {
                    telegram: TelegramIdentityChannel {
                        enabled: true,
                        max_reply_chars: 3500,
                        style_overrides: "short".to_string(),
                    },
                },
            },
            sections: crate::identity::compiler::CompiledIdentitySections {
                mission: "m".to_string(),
                persona: "p".to_string(),
                tone: "t".to_string(),
                hard_rules: "h".to_string(),
                do_not_do: "d".to_string(),
                escalation: "e".to_string(),
                memory_preferences: "m".to_string(),
                channel_notes: "c".to_string(),
                planning_principles: "pp".to_string(),
                review_standards: "rs".to_string(),
            },
            compiled_system_prompt: "identity".to_string(),
        };
        let context = TurnContext {
            conversation_id: 1,
            trace_id: "trace-test".to_string(),
            recent_turns: vec![],
            latest_summary: None,
            brain_memories: vec![],
            memories: vec![],
            working_set: crate::usecase::ConversationWorkingSet::default(),
            current_evidence: None,
            selected_skills: vec![],
            performance_policy: crate::team::config::PerformancePolicy::BalancedFast,
            max_escalation_tier: crate::team::config::EscalationTier::Standard,
            analysis_complexity: crate::usecase::AnalysisComplexity::Simple,
        };

        let messages = compile_planning_prompt(
            &identity,
            "Analiza, verifica e integra",
            &context,
            &[
                "researcher".to_string(),
                "verifier".to_string(),
                "integrator".to_string(),
            ],
            6,
            3,
        );

        assert!(messages[0].content.contains("Allowed model_route values"));
        assert!(messages[0].content.contains("max_tasks=6"));
    }
}
