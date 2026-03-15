use crate::storage::ConversationTurn;

#[derive(Debug, Clone, Default)]
pub struct DeterministicSummarizer;

impl DeterministicSummarizer {
    pub fn summarize(&self, turns: &[ConversationTurn]) -> String {
        if turns.is_empty() {
            return "No conversation yet.".to_string();
        }

        let mut lines = Vec::new();
        let head = turns.iter().take(3);
        for turn in head {
            lines.push(format!("{}: {}", turn.role, truncate(&turn.content, 140)));
        }

        if turns.len() > 6 {
            lines.push("...".to_string());
        }

        let tail_start = turns.len().saturating_sub(3);
        for turn in &turns[tail_start..] {
            lines.push(format!("{}: {}", turn.role, truncate(&turn.content, 140)));
        }

        lines.join("\n")
    }
}

fn truncate(input: &str, max: usize) -> String {
    if input.len() <= max {
        return input.to_string();
    }
    format!("{}...", &input[..max])
}
