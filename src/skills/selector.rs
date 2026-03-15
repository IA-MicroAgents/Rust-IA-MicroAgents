use std::collections::HashSet;

use super::registry::SkillDefinition;

#[derive(Debug, Clone, Default)]
pub struct SkillSelector;

impl SkillSelector {
    pub fn select(
        &self,
        available: &[SkillDefinition],
        user_text: &str,
        recent_hints: &[String],
        max_skills: usize,
    ) -> Vec<SkillDefinition> {
        let user_lower = user_text.to_lowercase();
        let hint_set: HashSet<String> = recent_hints.iter().map(|h| h.to_lowercase()).collect();

        let mut scored = available
            .iter()
            .map(|skill| {
                let mut score = 0_i32;

                for trigger in &skill.manifest.triggers {
                    if user_lower.contains(&trigger.to_lowercase()) {
                        score += 5;
                    }
                }

                for tag in &skill.manifest.tags {
                    if user_lower.contains(&tag.to_lowercase()) {
                        score += 3;
                    }
                    if hint_set.contains(&tag.to_lowercase()) {
                        score += 2;
                    }
                }

                for token in skill.manifest.name.split('.') {
                    if user_lower.contains(&token.to_lowercase()) {
                        score += 1;
                    }
                }

                if skill.manifest.name.starts_with("agent.") {
                    score += 1;
                }

                (score, skill.clone())
            })
            .collect::<Vec<_>>();

        scored.sort_by(|a, b| b.0.cmp(&a.0));
        scored
            .into_iter()
            .filter(|(score, _)| *score > 0)
            .take(max_skills)
            .map(|(_, skill)| skill)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::SkillSelector;
    use crate::skills::manifest::{SkillKind, SkillManifest};
    use crate::skills::registry::SkillDefinition;

    #[test]
    fn selects_triggered_skill() {
        let selector = SkillSelector;
        let skill = SkillDefinition {
            manifest: SkillManifest {
                name: "reminders.create".to_string(),
                version: "1.0.0".to_string(),
                description: "Create reminders".to_string(),
                kind: SkillKind::Builtin,
                entrypoint: "reminders.create".to_string(),
                input_schema: None,
                output_schema: None,
                permissions: vec![],
                timeout_ms: 1000,
                max_retries: 0,
                cache_ttl_secs: 0,
                idempotent: false,
                side_effects: "creates job".to_string(),
                tags: vec!["reminder".to_string()],
                triggers: vec!["remind".to_string()],
            },
            sections: std::collections::HashMap::new(),
            folder: std::path::PathBuf::from("."),
        };

        let selected = selector.select(&[skill], "please remind me tomorrow", &[], 4);
        assert_eq!(selected.len(), 1);
    }
}
