use serde::{Deserialize, Serialize};

use super::schema::{
    IdentityBudgets, IdentityDoc, IdentityFrontmatter, IdentityMemory, IdentityPermissions,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompiledIdentitySections {
    pub mission: String,
    pub persona: String,
    pub tone: String,
    pub hard_rules: String,
    pub do_not_do: String,
    pub escalation: String,
    pub memory_preferences: String,
    pub channel_notes: String,
    pub planning_principles: String,
    pub review_standards: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemIdentity {
    pub frontmatter: IdentityFrontmatter,
    pub sections: CompiledIdentitySections,
    pub compiled_system_prompt: String,
}

impl SystemIdentity {
    pub fn compile(doc: IdentityDoc) -> Self {
        let sections = CompiledIdentitySections {
            mission: doc.sections.get("Mission").cloned().unwrap_or_default(),
            persona: doc.sections.get("Persona").cloned().unwrap_or_default(),
            tone: doc.sections.get("Tone").cloned().unwrap_or_default(),
            hard_rules: doc.sections.get("Hard Rules").cloned().unwrap_or_default(),
            do_not_do: doc.sections.get("Do Not Do").cloned().unwrap_or_default(),
            escalation: doc.sections.get("Escalation").cloned().unwrap_or_default(),
            memory_preferences: doc
                .sections
                .get("Memory Preferences")
                .cloned()
                .unwrap_or_default(),
            channel_notes: doc
                .sections
                .get("Channel Notes")
                .cloned()
                .unwrap_or_default(),
            planning_principles: doc
                .sections
                .get("Planning Principles")
                .cloned()
                .unwrap_or_default(),
            review_standards: doc
                .sections
                .get("Review Standards")
                .cloned()
                .unwrap_or_default(),
        };

        let prompt = compile_prompt(&doc.frontmatter, &sections);
        Self {
            frontmatter: doc.frontmatter,
            sections,
            compiled_system_prompt: prompt,
        }
    }

    pub fn budgets(&self) -> &IdentityBudgets {
        &self.frontmatter.budgets
    }

    pub fn permissions(&self) -> &IdentityPermissions {
        &self.frontmatter.permissions
    }

    pub fn memory(&self) -> &IdentityMemory {
        &self.frontmatter.memory
    }
}

fn compile_prompt(
    frontmatter: &IdentityFrontmatter,
    sections: &CompiledIdentitySections,
) -> String {
    format!(
        "# Identity\n\
id: {}\n\
display_name: {}\n\
description: {}\n\
locale: {}\n\
timezone: {}\n\
\n\
# Mission\n{}\n\n\
# Persona\n{}\n\n\
# Tone\n{}\n\n\
# Hard Rules\n{}\n\n\
# Do Not Do\n{}\n\n\
# Escalation\n{}\n\n\
# Memory Preferences\n{}\n\n\
# Channel Notes\n{}\n\n\
# Planning Principles\n{}\n\n\
# Review Standards\n{}\n",
        frontmatter.id,
        frontmatter.display_name,
        frontmatter.description,
        frontmatter.locale,
        frontmatter.timezone,
        sections.mission,
        sections.persona,
        sections.tone,
        sections.hard_rules,
        sections.do_not_do,
        sections.escalation,
        sections.memory_preferences,
        sections.channel_notes,
        sections.planning_principles,
        sections.review_standards
    )
}
