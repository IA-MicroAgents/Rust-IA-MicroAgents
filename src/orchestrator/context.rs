use crate::{
    errors::AppResult,
    memory::MemoryStore,
    skills::{SkillDefinition, SkillRegistry, SkillSelector},
    storage::ConversationTurn,
    team::config::{EscalationTier, PerformancePolicy},
    usecase::{build_conversation_working_set, ConversationWorkingSet, EvidenceBundle},
};

#[derive(Debug, Clone)]
pub struct TurnContext {
    pub conversation_id: i64,
    pub trace_id: String,
    pub recent_turns: Vec<ConversationTurn>,
    pub latest_summary: Option<String>,
    pub memories: Vec<String>,
    pub working_set: ConversationWorkingSet,
    pub current_evidence: Option<EvidenceBundle>,
    pub selected_skills: Vec<SkillDefinition>,
    pub performance_policy: PerformancePolicy,
    pub max_escalation_tier: EscalationTier,
    pub analysis_complexity: crate::usecase::AnalysisComplexity,
}

#[derive(Clone)]
pub struct ContextBuilder {
    memory: MemoryStore,
    registry: SkillRegistry,
    selector: SkillSelector,
}

pub struct ContextBuildRequest<'a> {
    pub conversation_id: i64,
    pub trace_id: &'a str,
    pub user_text: &'a str,
    pub hints: &'a [String],
    pub allowed_skills: &'a [String],
    pub denied_skills: &'a [String],
    pub ignore_prior_context: bool,
    pub performance_policy: PerformancePolicy,
    pub max_escalation_tier: EscalationTier,
    pub analysis_complexity: crate::usecase::AnalysisComplexity,
    pub locale: &'a str,
}

impl ContextBuilder {
    pub fn new(memory: MemoryStore, registry: SkillRegistry, selector: SkillSelector) -> Self {
        Self {
            memory,
            registry,
            selector,
        }
    }

    pub async fn build(&self, request: ContextBuildRequest<'_>) -> AppResult<TurnContext> {
        let mut snapshot = self
            .memory
            .conversation_context_snapshot(request.conversation_id, request.user_text, 30, 10)
            .await?;
        if request.ignore_prior_context {
            snapshot.recent_turns.clear();
            snapshot.latest_summary = None;
            snapshot.memories.clear();
        }
        let available = self
            .registry
            .list()
            .into_iter()
            .filter(|skill| {
                let denied = request
                    .denied_skills
                    .iter()
                    .any(|name| name.eq_ignore_ascii_case(&skill.manifest.name));
                if denied {
                    return false;
                }
                request
                    .allowed_skills
                    .iter()
                    .any(|name| name == "*" || name.eq_ignore_ascii_case(&skill.manifest.name))
            })
            .collect::<Vec<_>>();
        let selected_skills = self
            .selector
            .select(&available, request.user_text, request.hints, 8);
        let working_set = build_conversation_working_set(
            request.locale,
            request.user_text,
            &snapshot.recent_turns,
            snapshot.latest_summary.as_deref(),
            &snapshot.memories,
        );

        Ok(TurnContext {
            conversation_id: request.conversation_id,
            trace_id: request.trace_id.to_string(),
            recent_turns: snapshot.recent_turns,
            latest_summary: snapshot.latest_summary,
            memories: snapshot.memories,
            working_set,
            current_evidence: None,
            selected_skills,
            performance_policy: request.performance_policy,
            max_escalation_tier: request.max_escalation_tier,
            analysis_complexity: request.analysis_complexity,
        })
    }
}

impl TurnContext {
    pub fn recent_turns_block(&self, limit: usize) -> String {
        self.recent_turns
            .iter()
            .rev()
            .take(limit)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(|turn| format!("{}: {}", turn.role, turn.content))
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn memories_block(&self, limit: usize) -> String {
        self.memories
            .iter()
            .take(limit)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n- ")
    }
}
