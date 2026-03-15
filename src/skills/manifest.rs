use std::{collections::HashMap, path::Path};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::errors::{AppError, AppResult};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SkillKind {
    Builtin,
    Command,
    Http,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillManifest {
    pub name: String,
    pub version: String,
    pub description: String,
    pub kind: SkillKind,
    pub entrypoint: String,
    pub input_schema: Option<Value>,
    pub output_schema: Option<Value>,
    pub permissions: Vec<String>,
    pub timeout_ms: u64,
    pub max_retries: u32,
    pub cache_ttl_secs: u64,
    pub idempotent: bool,
    pub side_effects: String,
    pub tags: Vec<String>,
    pub triggers: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillDoc {
    pub frontmatter: SkillManifest,
    pub sections: HashMap<String, String>,
}

impl SkillDoc {
    pub fn parse(markdown: &str) -> AppResult<Self> {
        let (frontmatter_raw, body) = split_frontmatter(markdown)?;
        let frontmatter: SkillManifest = serde_yaml::from_str(frontmatter_raw)
            .map_err(|e| AppError::Skill(format!("invalid skill frontmatter yaml: {e}")))?;

        validate_manifest(&frontmatter)?;

        let sections = parse_sections(body);
        for required in [
            "What it does",
            "When to use",
            "When NOT to use",
            "Input notes",
            "Output notes",
            "Failure handling",
            "Examples",
        ] {
            if !sections.contains_key(required) {
                return Err(AppError::Skill(format!(
                    "skill {} missing section '{required}'",
                    frontmatter.name
                )));
            }
        }

        Ok(Self {
            frontmatter,
            sections,
        })
    }

    pub fn parse_file(path: &Path) -> AppResult<Self> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| AppError::Skill(format!("failed reading {}: {e}", path.display())))?;
        Self::parse(&content)
    }
}

fn split_frontmatter(markdown: &str) -> AppResult<(&str, &str)> {
    if !markdown.starts_with("---\n") {
        return Err(AppError::Skill(
            "SKILL.md must start with YAML frontmatter".to_string(),
        ));
    }

    let rest = &markdown[4..];
    if let Some(idx) = rest.find("\n---\n") {
        let fm = &rest[..idx];
        let body = &rest[idx + 5..];
        return Ok((fm, body));
    }

    Err(AppError::Skill(
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

fn validate_manifest(manifest: &SkillManifest) -> AppResult<()> {
    if manifest.name.trim().is_empty() {
        return Err(AppError::Skill("skill name must not be empty".to_string()));
    }
    if manifest.timeout_ms == 0 {
        return Err(AppError::Skill(format!(
            "skill {} timeout_ms must be > 0",
            manifest.name
        )));
    }
    if manifest.kind != SkillKind::Builtin && manifest.entrypoint.trim().is_empty() {
        return Err(AppError::Skill(format!(
            "skill {} entrypoint must not be empty for non-builtin",
            manifest.name
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::SkillDoc;

    #[test]
    fn parses_skill_doc() {
        let sample = r#"---
name: agent.status
version: 1.0.0
description: status
kind: builtin
entrypoint: agent.status
input_schema:
  type: object
output_schema:
  type: object
permissions: []
timeout_ms: 1000
max_retries: 0
cache_ttl_secs: 1
idempotent: true
side_effects: none
tags: [agent]
triggers: [status]
---
## What it does
x
## When to use
y
## When NOT to use
z
## Input notes
i
## Output notes
o
## Failure handling
f
## Examples
e
"#;
        let parsed = SkillDoc::parse(sample);
        assert!(parsed.is_ok());
    }
}
