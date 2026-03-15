use crate::llm::response_types::DecisionRoute;

pub fn pick_route_hint(user_text: &str) -> DecisionRoute {
    let text = user_text.trim().to_lowercase();
    if text.is_empty() {
        return DecisionRoute::Ignore;
    }

    let greetings = ["hi", "hello", "hey", "hola", "buenas"];
    if greetings
        .iter()
        .any(|g| text == *g || text.starts_with(&format!("{g} ")))
    {
        return DecisionRoute::DirectReply;
    }

    let tool_triggers = [
        "remind",
        "recordatorio",
        "buscar",
        "search",
        "remember",
        "memo",
        "status",
        "help",
        "http",
        "fetch",
    ];

    if tool_triggers.iter().any(|needle| text.contains(needle)) {
        return DecisionRoute::ToolUse;
    }

    let risky_markers = ["bank", "password", "transfer", "wire", "legal", "medical"];
    if risky_markers.iter().any(|needle| text.contains(needle)) {
        return DecisionRoute::AskClarification;
    }

    let planning_markers = [
        "plan",
        "subagente",
        "subagentes",
        "delegate",
        "deleg",
        "paralelo",
        "parallel",
        "divide",
        "ranking",
        "forecast",
        "trading",
        "btc",
        "bitcoin",
    ];
    if text.len() > 240 || planning_markers.iter().any(|needle| text.contains(needle)) {
        return DecisionRoute::PlanThenAct;
    }

    DecisionRoute::DirectReply
}

#[cfg(test)]
mod tests {
    use crate::llm::response_types::DecisionRoute;

    use super::pick_route_hint;

    #[test]
    fn picks_tool_route_for_reminder_requests() {
        assert_eq!(
            pick_route_hint("please remind me tomorrow"),
            DecisionRoute::ToolUse
        );
    }
}
