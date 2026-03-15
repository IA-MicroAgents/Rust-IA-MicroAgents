use crate::{errors::AppResult, memory::store::MemoryStore};

#[derive(Clone)]
pub struct MemoryRetriever {
    memory: MemoryStore,
}

impl MemoryRetriever {
    pub fn new(memory: MemoryStore) -> Self {
        Self { memory }
    }

    pub async fn retrieve(
        &self,
        conversation_id: Option<i64>,
        query: &str,
        limit: usize,
    ) -> AppResult<Vec<String>> {
        self.memory.search(conversation_id, query, limit).await
    }
}
