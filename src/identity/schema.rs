use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::errors::{AppError, AppResult};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityDoc {
    pub frontmatter: IdentityFrontmatter,
    pub sections: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityFrontmatter {
    pub id: String,
    pub display_name: String,
    pub description: String,
    pub locale: String,
    pub timezone: String,
    pub model_routes: ModelRoutes,
    pub budgets: IdentityBudgets,
    pub memory: IdentityMemory,
    pub permissions: IdentityPermissions,
    pub channels: IdentityChannels,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRoutes {
    pub fast: String,
    pub reasoning: String,
    pub tool_use: String,
    pub vision: String,
    pub reviewer: String,
    pub planner: String,
    #[serde(default)]
    pub router_fast: Option<String>,
    #[serde(default)]
    pub fast_text: Option<String>,
    #[serde(default)]
    pub reviewer_fast: Option<String>,
    #[serde(default)]
    pub reviewer_strict: Option<String>,
    #[serde(default)]
    pub integrator_complex: Option<String>,
    #[serde(default)]
    pub vision_understand: Option<String>,
    #[serde(default)]
    pub audio_transcribe: Option<String>,
    #[serde(default)]
    pub image_generate: Option<String>,
    pub fallback: Vec<String>,
}

impl ModelRoutes {
    pub fn route_value<'a>(&'a self, route_key: &str) -> Option<&'a str> {
        match route_key {
            "fast" => Some(self.fast.as_str()),
            "router_fast" => Some(self.router_fast.as_deref().unwrap_or(self.fast.as_str())),
            "fast_text" => Some(self.fast_text.as_deref().unwrap_or(self.fast.as_str())),
            "reasoning" => Some(self.reasoning.as_str()),
            "tool_use" => Some(self.tool_use.as_str()),
            "vision" => Some(self.vision.as_str()),
            "vision_understand" => Some(
                self.vision_understand
                    .as_deref()
                    .unwrap_or(self.vision.as_str()),
            ),
            "reviewer" => Some(self.reviewer.as_str()),
            "reviewer_fast" => Some(
                self.reviewer_fast
                    .as_deref()
                    .unwrap_or(self.reviewer.as_str()),
            ),
            "reviewer_strict" => Some(
                self.reviewer_strict
                    .as_deref()
                    .unwrap_or(self.reasoning.as_str()),
            ),
            "integrator_complex" => Some(
                self.integrator_complex
                    .as_deref()
                    .unwrap_or(self.reasoning.as_str()),
            ),
            "planner" => Some(self.planner.as_str()),
            "audio_transcribe" => Some(
                self.audio_transcribe
                    .as_deref()
                    .unwrap_or(self.fast.as_str()),
            ),
            "image_generate" => Some(
                self.image_generate
                    .as_deref()
                    .unwrap_or(self.vision.as_str()),
            ),
            _ => None,
        }
    }

    pub fn all_model_ids(&self) -> Vec<String> {
        let mut ids = vec![
            self.fast.clone(),
            self.reasoning.clone(),
            self.tool_use.clone(),
            self.vision.clone(),
            self.reviewer.clone(),
            self.planner.clone(),
        ];
        ids.extend(
            [
                self.router_fast.clone(),
                self.fast_text.clone(),
                self.reviewer_fast.clone(),
                self.reviewer_strict.clone(),
                self.integrator_complex.clone(),
                self.vision_understand.clone(),
                self.audio_transcribe.clone(),
                self.image_generate.clone(),
            ]
            .into_iter()
            .flatten(),
        );
        ids.extend(self.fallback.clone());
        ids.retain(|id| !id.trim().to_ascii_lowercase().starts_with("openrouter/"));
        ids.sort();
        ids.dedup();
        ids
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityBudgets {
    pub max_steps: u32,
    pub max_turn_cost_usd: f64,
    pub max_input_tokens: u32,
    pub max_output_tokens: u32,
    pub max_tool_calls: u32,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityMemory {
    pub save_facts: bool,
    pub save_summaries: bool,
    pub summarize_every_n_turns: u32,
    #[serde(default = "default_brain_enabled")]
    pub brain_enabled: bool,
    #[serde(default = "default_precheck_each_turn")]
    pub precheck_each_turn: bool,
    #[serde(default = "default_auto_write_mode")]
    pub auto_write_mode: String,
    #[serde(default = "default_brain_conversation_limit")]
    pub conversation_limit: usize,
    #[serde(default = "default_brain_user_limit")]
    pub user_limit: usize,
}

impl IdentityMemory {
    pub fn brain_enabled(&self) -> bool {
        self.brain_enabled
    }

    pub fn precheck_each_turn(&self) -> bool {
        self.precheck_each_turn
    }

    pub fn auto_write_mode(&self) -> &str {
        self.auto_write_mode.as_str()
    }

    pub fn conversation_limit(&self) -> usize {
        self.conversation_limit.max(1)
    }

    pub fn user_limit(&self) -> usize {
        self.user_limit.max(1)
    }
}

fn default_brain_enabled() -> bool {
    true
}

fn default_precheck_each_turn() -> bool {
    true
}

fn default_auto_write_mode() -> String {
    "aggressive".to_string()
}

fn default_brain_conversation_limit() -> usize {
    4
}

fn default_brain_user_limit() -> usize {
    4
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityPermissions {
    pub allowed_skills: Vec<String>,
    pub denied_skills: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityChannels {
    pub telegram: TelegramIdentityChannel,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramIdentityChannel {
    pub enabled: bool,
    pub max_reply_chars: usize,
    pub style_overrides: String,
}

impl IdentityDoc {
    pub fn parse(markdown: &str) -> AppResult<Self> {
        let (frontmatter_raw, body) = split_frontmatter(markdown)?;
        let frontmatter: IdentityFrontmatter = serde_yaml::from_str(frontmatter_raw)
            .map_err(|e| AppError::Identity(format!("invalid identity frontmatter yaml: {e}")))?;

        validate_frontmatter(&frontmatter)?;

        let sections = parse_sections(body);

        for required in [
            "Mission",
            "Persona",
            "Tone",
            "Hard Rules",
            "Do Not Do",
            "Escalation",
            "Memory Preferences",
            "Channel Notes",
            "Planning Principles",
            "Review Standards",
        ] {
            if !sections.contains_key(required) {
                return Err(AppError::Identity(format!(
                    "missing markdown section '{required}'"
                )));
            }
        }

        Ok(Self {
            frontmatter,
            sections,
        })
    }
}

fn split_frontmatter(markdown: &str) -> AppResult<(&str, &str)> {
    if !markdown.starts_with("---\n") {
        return Err(AppError::Identity(
            "IDENTITY.md must start with YAML frontmatter".to_string(),
        ));
    }

    let rest = &markdown[4..];
    if let Some(idx) = rest.find("\n---\n") {
        let fm = &rest[..idx];
        let body = &rest[idx + 5..];
        return Ok((fm, body));
    }

    Err(AppError::Identity(
        "frontmatter closing delimiter not found".to_string(),
    ))
}

fn parse_sections(markdown_body: &str) -> HashMap<String, String> {
    let mut sections = HashMap::new();
    let mut current_title = String::new();
    let mut current_body = String::new();

    for line in markdown_body.lines() {
        if let Some(title) = line.strip_prefix("## ") {
            if !current_title.is_empty() {
                sections.insert(current_title.clone(), current_body.trim().to_string());
            }
            current_title = title.trim().to_string();
            current_body.clear();
            continue;
        }

        if !current_title.is_empty() {
            current_body.push_str(line);
            current_body.push('\n');
        }
    }

    if !current_title.is_empty() {
        sections.insert(current_title, current_body.trim().to_string());
    }

    sections
}

fn validate_frontmatter(frontmatter: &IdentityFrontmatter) -> AppResult<()> {
    if frontmatter.id.trim().is_empty() {
        return Err(AppError::Identity("id must not be empty".to_string()));
    }
    if frontmatter.model_routes.fast.trim().is_empty()
        || frontmatter.model_routes.reasoning.trim().is_empty()
        || frontmatter.model_routes.tool_use.trim().is_empty()
        || frontmatter.model_routes.reviewer.trim().is_empty()
        || frontmatter.model_routes.planner.trim().is_empty()
    {
        return Err(AppError::Identity(
            "model_routes.fast/reasoning/tool_use/reviewer/planner must not be empty".to_string(),
        ));
    }
    if frontmatter.budgets.max_steps == 0
        || frontmatter.budgets.max_tool_calls == 0
        || frontmatter.budgets.timeout_ms == 0
    {
        return Err(AppError::Identity(
            "budgets max_steps/max_tool_calls/timeout_ms must be > 0".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_identity_document() {
        let markdown = r#"---
id: ai-microagents
ndisplay_name: AI MicroAgents
description: test
locale: en-US
timezone: UTC
model_routes:
  fast: model-a
  reasoning: model-b
  tool_use: model-c
  vision: model-v
  reviewer: model-r
  planner: model-p
  fallback: [model-d]
budgets:
  max_steps: 3
  max_turn_cost_usd: 0.1
  max_input_tokens: 1200
  max_output_tokens: 400
  max_tool_calls: 2
  timeout_ms: 10000
memory:
  save_facts: true
  save_summaries: true
  summarize_every_n_turns: 6
permissions:
  allowed_skills: [agent.status]
  denied_skills: [dangerous]
channels:
  telegram:
    enabled: true
    max_reply_chars: 3500
    style_overrides: concise
---
## Mission
m
## Persona
p
## Tone
t
## Hard Rules
h
## Do Not Do
d
## Escalation
e
## Memory Preferences
mp
## Channel Notes
cn
## Planning Principles
pp
## Review Standards
rs
"#;

        let result = IdentityDoc::parse(&markdown.replace("ndisplay_name", "display_name"));
        assert!(result.is_ok());
    }

    #[test]
    fn applies_brain_defaults_when_fields_are_missing() {
        let markdown = r#"---
id: ai-microagents
display_name: AI MicroAgents
description: test
locale: en-US
timezone: UTC
model_routes:
  fast: model-a
  reasoning: model-b
  tool_use: model-c
  vision: model-v
  reviewer: model-r
  planner: model-p
  fallback: [model-d]
budgets:
  max_steps: 3
  max_turn_cost_usd: 0.1
  max_input_tokens: 1200
  max_output_tokens: 400
  max_tool_calls: 2
  timeout_ms: 10000
memory:
  save_facts: true
  save_summaries: true
  summarize_every_n_turns: 6
permissions:
  allowed_skills: [agent.status]
  denied_skills: [dangerous]
channels:
  telegram:
    enabled: true
    max_reply_chars: 3500
    style_overrides: concise
---
## Mission
m
## Persona
p
## Tone
t
## Hard Rules
h
## Do Not Do
d
## Escalation
e
## Memory Preferences
mp
## Channel Notes
cn
## Planning Principles
pp
## Review Standards
rs
"#;

        let result = IdentityDoc::parse(markdown).expect("identity parse");
        assert!(result.frontmatter.memory.brain_enabled);
        assert!(result.frontmatter.memory.precheck_each_turn);
        assert_eq!(result.frontmatter.memory.auto_write_mode, "aggressive");
        assert_eq!(result.frontmatter.memory.conversation_limit, 4);
        assert_eq!(result.frontmatter.memory.user_limit, 4);
    }
}
