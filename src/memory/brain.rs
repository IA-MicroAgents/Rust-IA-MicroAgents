use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum BrainScopeKind {
    User,
    Conversation,
}

impl BrainScopeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Conversation => "conversation",
        }
    }

    pub fn from_db(value: &str) -> Self {
        match value {
            "conversation" => Self::Conversation,
            _ => Self::User,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum BrainMemoryKind {
    Preference,
    Goal,
    Constraint,
    Decision,
    Lesson,
    SourceLocation,
    ProfileFact,
}

impl BrainMemoryKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Preference => "preference",
            Self::Goal => "goal",
            Self::Constraint => "constraint",
            Self::Decision => "decision",
            Self::Lesson => "lesson",
            Self::SourceLocation => "source_location",
            Self::ProfileFact => "profile_fact",
        }
    }

    pub fn from_db(value: &str) -> Self {
        match value {
            "goal" => Self::Goal,
            "constraint" => Self::Constraint,
            "decision" => Self::Decision,
            "lesson" => Self::Lesson,
            "source_location" => Self::SourceLocation,
            "profile_fact" => Self::ProfileFact,
            _ => Self::Preference,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum BrainMemoryStatus {
    Active,
    Superseded,
}

impl BrainMemoryStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Superseded => "superseded",
        }
    }

    pub fn from_db(value: &str) -> Self {
        match value {
            "superseded" => Self::Superseded,
            _ => Self::Active,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct BrainMemoryProvenance {
    pub channel: Option<String>,
    pub user_id: Option<String>,
    pub conversation_id: Option<i64>,
    pub source_turn_id: Option<i64>,
    pub trace_id: Option<String>,
    pub tool_name: Option<String>,
    pub url: Option<String>,
}

impl BrainMemoryProvenance {
    pub fn merge(&self, newer: &BrainMemoryProvenance) -> BrainMemoryProvenance {
        BrainMemoryProvenance {
            channel: newer.channel.clone().or_else(|| self.channel.clone()),
            user_id: newer.user_id.clone().or_else(|| self.user_id.clone()),
            conversation_id: newer.conversation_id.or(self.conversation_id),
            source_turn_id: newer.source_turn_id.or(self.source_turn_id),
            trace_id: newer.trace_id.clone().or_else(|| self.trace_id.clone()),
            tool_name: newer.tool_name.clone().or_else(|| self.tool_name.clone()),
            url: newer.url.clone().or_else(|| self.url.clone()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BrainMemory {
    pub id: i64,
    pub scope_kind: BrainScopeKind,
    pub user_id: Option<String>,
    pub conversation_id: Option<i64>,
    pub memory_kind: BrainMemoryKind,
    pub memory_key: String,
    pub subject: String,
    pub what_value: String,
    pub why_value: Option<String>,
    pub where_context: Option<String>,
    pub learned_value: Option<String>,
    pub provenance: BrainMemoryProvenance,
    pub confidence: f64,
    pub status: BrainMemoryStatus,
    pub superseded_by: Option<i64>,
    pub source_turn_id: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl BrainMemory {
    pub fn render_for_prompt(&self) -> String {
        let mut parts = vec![format!(
            "[{}/{}] {} => {}",
            self.scope_kind.as_str(),
            self.memory_kind.as_str(),
            self.subject,
            self.what_value
        )];
        if let Some(why_value) = compact_option(&self.why_value) {
            parts.push(format!("why: {why_value}"));
        }
        if let Some(where_context) = compact_option(&self.where_context) {
            parts.push(format!("where: {where_context}"));
        }
        if let Some(learned_value) = compact_option(&self.learned_value) {
            parts.push(format!("learned: {learned_value}"));
        }
        parts.join(" | ")
    }

    pub fn render_for_search_result(&self) -> String {
        self.render_for_prompt()
    }

    pub fn same_content(&self, candidate: &BrainWriteCandidate) -> bool {
        normalize_field(&self.what_value) == normalize_field(&candidate.what_value)
            && normalize_optional(&self.why_value) == normalize_optional(&candidate.why_value)
            && normalize_optional(&self.where_context)
                == normalize_optional(&candidate.where_context)
            && normalize_optional(&self.learned_value)
                == normalize_optional(&candidate.learned_value)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BrainWriteCandidate {
    pub scope_kind: BrainScopeKind,
    pub user_id: Option<String>,
    pub conversation_id: Option<i64>,
    pub memory_kind: BrainMemoryKind,
    pub memory_key: String,
    pub subject: String,
    pub what_value: String,
    pub why_value: Option<String>,
    pub where_context: Option<String>,
    pub learned_value: Option<String>,
    pub provenance: BrainMemoryProvenance,
    pub confidence: f64,
    pub source_turn_id: Option<i64>,
}

impl BrainWriteCandidate {
    pub fn search_text(&self) -> String {
        build_brain_search_text(
            &self.subject,
            &self.what_value,
            self.why_value.as_deref(),
            self.where_context.as_deref(),
            self.learned_value.as_deref(),
        )
    }

    pub fn render_for_search_result(&self) -> String {
        let provisional = BrainMemory {
            id: 0,
            scope_kind: self.scope_kind.clone(),
            user_id: self.user_id.clone(),
            conversation_id: self.conversation_id,
            memory_kind: self.memory_kind.clone(),
            memory_key: self.memory_key.clone(),
            subject: self.subject.clone(),
            what_value: self.what_value.clone(),
            why_value: self.why_value.clone(),
            where_context: self.where_context.clone(),
            learned_value: self.learned_value.clone(),
            provenance: self.provenance.clone(),
            confidence: self.confidence,
            status: BrainMemoryStatus::Active,
            superseded_by: None,
            source_turn_id: self.source_turn_id,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        provisional.render_for_search_result()
    }
}

pub fn build_brain_search_text(
    subject: &str,
    what_value: &str,
    why_value: Option<&str>,
    where_context: Option<&str>,
    learned_value: Option<&str>,
) -> String {
    let mut parts = vec![subject.trim().to_string(), what_value.trim().to_string()];
    if let Some(why_value) = why_value {
        if !why_value.trim().is_empty() {
            parts.push(why_value.trim().to_string());
        }
    }
    if let Some(where_context) = where_context {
        if !where_context.trim().is_empty() {
            parts.push(where_context.trim().to_string());
        }
    }
    if let Some(learned_value) = learned_value {
        if !learned_value.trim().is_empty() {
            parts.push(learned_value.trim().to_string());
        }
    }
    parts.join(" ")
}

pub fn candidate_should_supersede(existing: &BrainMemory, candidate: &BrainWriteCandidate) -> bool {
    if existing.same_content(candidate) {
        return false;
    }

    let newer_turn = match (existing.source_turn_id, candidate.source_turn_id) {
        (Some(existing_turn), Some(candidate_turn)) => candidate_turn >= existing_turn,
        (None, Some(_)) => true,
        _ => false,
    };

    newer_turn || candidate.confidence + 0.05 >= existing.confidence
}

fn compact_option(value: &Option<String>) -> Option<&str> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn normalize_optional(value: &Option<String>) -> String {
    value.as_deref().map(normalize_field).unwrap_or_default()
}

fn normalize_field(value: &str) -> String {
    value
        .trim()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::{
        candidate_should_supersede, BrainMemory, BrainMemoryKind, BrainMemoryProvenance,
        BrainMemoryStatus, BrainScopeKind, BrainWriteCandidate,
    };
    use chrono::Utc;

    #[test]
    fn render_for_prompt_includes_key_fields() {
        let memory = BrainMemory {
            id: 1,
            scope_kind: BrainScopeKind::User,
            user_id: Some("u1".to_string()),
            conversation_id: None,
            memory_kind: BrainMemoryKind::Preference,
            memory_key: "preference.assistant_language".to_string(),
            subject: "assistant_language".to_string(),
            what_value: "Responder en espanol".to_string(),
            why_value: Some("Preferencia explicita del usuario.".to_string()),
            where_context: Some("Future assistant replies".to_string()),
            learned_value: None,
            provenance: BrainMemoryProvenance::default(),
            confidence: 0.9,
            status: BrainMemoryStatus::Active,
            superseded_by: None,
            source_turn_id: Some(10),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        let rendered = memory.render_for_prompt();
        assert!(rendered.contains("assistant_language"));
        assert!(rendered.contains("Responder en espanol"));
        assert!(rendered.contains("why:"));
    }

    #[test]
    fn newer_candidate_supersedes_conflicting_memory() {
        let existing = BrainMemory {
            id: 1,
            scope_kind: BrainScopeKind::User,
            user_id: Some("u1".to_string()),
            conversation_id: None,
            memory_kind: BrainMemoryKind::Preference,
            memory_key: "preference.assistant_language".to_string(),
            subject: "assistant_language".to_string(),
            what_value: "Reply in English".to_string(),
            why_value: None,
            where_context: None,
            learned_value: None,
            provenance: BrainMemoryProvenance::default(),
            confidence: 0.8,
            status: BrainMemoryStatus::Active,
            superseded_by: None,
            source_turn_id: Some(3),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let candidate = BrainWriteCandidate {
            scope_kind: BrainScopeKind::User,
            user_id: Some("u1".to_string()),
            conversation_id: None,
            memory_kind: BrainMemoryKind::Preference,
            memory_key: "preference.assistant_language".to_string(),
            subject: "assistant_language".to_string(),
            what_value: "Responder en espanol".to_string(),
            why_value: None,
            where_context: None,
            learned_value: None,
            provenance: BrainMemoryProvenance::default(),
            confidence: 0.75,
            source_turn_id: Some(4),
        };

        assert!(candidate_should_supersede(&existing, &candidate));
    }
}
