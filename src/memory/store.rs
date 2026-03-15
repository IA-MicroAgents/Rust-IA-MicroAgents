use crate::{
    errors::AppResult,
    memory::{BrainMemory, BrainWriteCandidate},
    storage::{types::ConversationContextSnapshot, ConversationTurn, Store},
};

#[derive(Clone)]
pub struct MemoryStore {
    store: Store,
}

impl MemoryStore {
    pub fn new(store: Store) -> Self {
        Self { store }
    }

    pub async fn recent_turns(
        &self,
        conversation_id: i64,
        limit: usize,
    ) -> AppResult<Vec<ConversationTurn>> {
        self.store.recent_turns(conversation_id, limit).await
    }

    pub async fn latest_summary(&self, conversation_id: i64) -> AppResult<Option<String>> {
        self.store.latest_summary(conversation_id).await
    }

    pub async fn conversation_context_snapshot(
        &self,
        conversation_id: i64,
        query: &str,
        turn_limit: usize,
        memory_limit: usize,
    ) -> AppResult<ConversationContextSnapshot> {
        self.store
            .conversation_context_snapshot(conversation_id, query, turn_limit, memory_limit)
            .await
    }

    pub async fn search(
        &self,
        conversation_id: Option<i64>,
        user_id: Option<&str>,
        query: &str,
        limit: usize,
    ) -> AppResult<Vec<String>> {
        self.store
            .search_memory(conversation_id, user_id, query, limit)
            .await
    }

    pub async fn search_brain(
        &self,
        conversation_id: Option<i64>,
        user_id: Option<&str>,
        query: &str,
        conversation_limit: usize,
        user_limit: usize,
    ) -> AppResult<Vec<BrainMemory>> {
        self.store
            .search_active_brain(
                conversation_id,
                user_id,
                query,
                conversation_limit,
                user_limit,
            )
            .await
    }

    pub async fn recent_brain(
        &self,
        conversation_id: Option<i64>,
        user_id: Option<&str>,
        conversation_limit: usize,
        user_limit: usize,
    ) -> AppResult<Vec<BrainMemory>> {
        self.store
            .recent_active_brain(conversation_id, user_id, conversation_limit, user_limit)
            .await
    }

    pub async fn write_summary(&self, conversation_id: i64, summary: &str) -> AppResult<()> {
        self.store.write_summary(conversation_id, summary).await
    }

    pub async fn write_fact(
        &self,
        conversation_id: Option<i64>,
        key: &str,
        value: &str,
        confidence: f64,
        source_turn_id: Option<i64>,
    ) -> AppResult<()> {
        self.store
            .write_fact(conversation_id, key, value, confidence, source_turn_id)
            .await
    }

    pub async fn save_or_merge_brain_candidates(
        &self,
        candidates: &[BrainWriteCandidate],
    ) -> AppResult<()> {
        self.store.save_or_merge_brain_candidates(candidates).await
    }
}
