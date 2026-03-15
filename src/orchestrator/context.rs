use crate::{
    errors::AppResult,
    memory::{BrainMemory, MemoryStore},
    skills::{SkillDefinition, SkillRegistry, SkillSelector},
    storage::ConversationTurn,
    team::config::{EscalationTier, PerformancePolicy},
    usecase::{
        build_conversation_working_set, ConversationWorkingSet, EvidenceBundle,
        RetrieveBrainMemoryRequest, RetrieveBrainMemoryUseCase,
    },
};
use tracing::warn;

#[derive(Debug, Clone)]
pub struct TurnContext {
    pub conversation_id: i64,
    pub trace_id: String,
    pub recent_turns: Vec<ConversationTurn>,
    pub latest_summary: Option<String>,
    pub brain_memories: Vec<BrainMemory>,
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
    brain_retrieval: RetrieveBrainMemoryUseCase,
    registry: SkillRegistry,
    selector: SkillSelector,
}

pub struct ContextBuildRequest<'a> {
    pub conversation_id: i64,
    pub user_id: &'a str,
    pub trace_id: &'a str,
    pub user_text: &'a str,
    pub hints: &'a [String],
    pub allowed_skills: &'a [String],
    pub denied_skills: &'a [String],
    pub ignore_prior_context: bool,
    pub performance_policy: PerformancePolicy,
    pub max_escalation_tier: EscalationTier,
    pub analysis_complexity: crate::usecase::AnalysisComplexity,
    pub brain_enabled: bool,
    pub precheck_each_turn: bool,
    pub brain_conversation_limit: usize,
    pub brain_user_limit: usize,
    pub locale: &'a str,
}

impl ContextBuilder {
    pub fn new(memory: MemoryStore, registry: SkillRegistry, selector: SkillSelector) -> Self {
        Self {
            brain_retrieval: RetrieveBrainMemoryUseCase::new(memory.clone()),
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
        let brain_memories = match self
            .brain_retrieval
            .execute(RetrieveBrainMemoryRequest {
                enabled: request.brain_enabled && request.precheck_each_turn,
                conversation_id: if request.ignore_prior_context {
                    None
                } else {
                    Some(request.conversation_id)
                },
                user_id: Some(request.user_id),
                query: request.user_text,
                conversation_limit: if request.ignore_prior_context {
                    0
                } else {
                    request.brain_conversation_limit
                },
                user_limit: request.brain_user_limit,
            })
            .await
        {
            Ok(brain_memories) => brain_memories,
            Err(err) => {
                warn!(error = %err, "brain retrieval failed during context build; continuing without brain memories");
                Vec::new()
            }
        };
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
            brain_memories,
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

    pub fn brain_memories_block(&self, limit: usize) -> String {
        self.brain_memories
            .iter()
            .take(limit)
            .map(BrainMemory::render_for_prompt)
            .collect::<Vec<_>>()
            .join("\n- ")
    }
}
