use std::{collections::HashMap, sync::Arc};

use chrono::{DateTime, Utc};
use serde_json::Value;
use tokio::sync::RwLock;

use crate::{
    config::AppConfig,
    config::BusConfig,
    errors::{AppError, AppResult},
    memory::{BrainMemory, BrainWriteCandidate},
    scheduler::jobs::ReminderSendJob,
    storage::{
        cache::{CacheLayer, CacheScope},
        postgres::PostgresStore,
        types::{
            BusEventEnvelope, ConversationContextSnapshot, ConversationTraceBundle,
            ConversationTurn, InboundEventRecord, OutboundMessageInsert, OutboxEventRecord,
            OutboxStats, StreamPendingStats, TaskAttemptInsert, TaskReviewInsert, ToolTraceInsert,
        },
    },
};

#[derive(Clone)]
pub struct Store {
    backend: PostgresStore,
    cache: CacheLayer,
    bus: BusConfig,
    hot_conversations: Arc<RwLock<HashMap<i64, HotConversationState>>>,
}

#[derive(Debug, Clone, Default)]
struct HotConversationState {
    recent_turns: Vec<ConversationTurn>,
    latest_summary: Option<String>,
}

impl Store {
    pub async fn from_config(config: &AppConfig) -> AppResult<Self> {
        let backend = PostgresStore::new_with_schema(&config.database).await?;
        let cache = CacheLayer::from_config(&config.cache).await?;
        Ok(Self {
            backend,
            cache,
            bus: config.bus.clone(),
            hot_conversations: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    pub fn backend_name(&self) -> &'static str {
        "postgres"
    }

    pub fn cache_enabled(&self) -> bool {
        self.cache.is_enabled()
    }

    pub fn bus_config(&self) -> &BusConfig {
        &self.bus
    }

    pub fn bus_enabled(&self) -> bool {
        self.bus.enabled
    }

    pub async fn insert_inbound_event(
        &self,
        event_id: &str,
        source: &str,
        payload_json: &Value,
    ) -> AppResult<Option<i64>> {
        self.backend
            .insert_inbound_event(event_id, source, payload_json)
            .await
    }

    pub async fn mark_inbound_processed(&self, event_id: &str) -> AppResult<()> {
        self.backend.mark_inbound_processed(event_id).await
    }

    pub async fn get_inbound_event_by_event_id(
        &self,
        event_id: &str,
    ) -> AppResult<InboundEventRecord> {
        self.backend.get_inbound_event_by_event_id(event_id).await
    }

    pub async fn upsert_conversation(&self, channel: &str, external_id: &str) -> AppResult<i64> {
        self.backend.upsert_conversation(channel, external_id).await
    }

    pub async fn append_turn(
        &self,
        conversation_id: i64,
        role: &str,
        content: &str,
        trace_id: &str,
        route: &str,
        usage: Option<(u32, u32, f64)>,
    ) -> AppResult<i64> {
        let out = self
            .backend
            .append_turn(conversation_id, role, content, trace_id, route, usage)
            .await?;
        let _ = self
            .cache
            .delete_prefix(&format!("conversation:{conversation_id}:recent:"))
            .await;
        let _ = self
            .cache
            .delete_prefix(&format!("conversation:{conversation_id}:context:"))
            .await;
        let _ = self
            .cache
            .delete_prefix(&format!("memory:search:{conversation_id}:"))
            .await;
        Ok(out)
    }

    pub async fn publish_hot_turn(&self, conversation_id: i64, role: &str, content: &str) {
        let mut cache = self.hot_conversations.write().await;
        let state = cache.entry(conversation_id).or_default();
        state.recent_turns.push(ConversationTurn {
            role: role.to_string(),
            content: content.to_string(),
            created_at: Utc::now(),
        });
        const HOT_TURN_CAP: usize = 50;
        if state.recent_turns.len() > HOT_TURN_CAP {
            let overflow = state.recent_turns.len() - HOT_TURN_CAP;
            state.recent_turns.drain(0..overflow);
        }
    }

    pub async fn publish_hot_summary(&self, conversation_id: i64, summary: &str) {
        let mut cache = self.hot_conversations.write().await;
        let state = cache.entry(conversation_id).or_default();
        state.latest_summary = Some(summary.to_string());
    }

    pub async fn recent_turns(
        &self,
        conversation_id: i64,
        limit: usize,
    ) -> AppResult<Vec<ConversationTurn>> {
        if let Some(turns) = self.hot_recent_turns(conversation_id, limit).await {
            return Ok(turns);
        }
        let key = format!("conversation:{conversation_id}:recent:{limit}");
        self.cache
            .cached_json(&key, CacheScope::Memory, || async move {
                self.backend.recent_turns(conversation_id, limit).await
            })
            .await
    }

    pub async fn write_summary(&self, conversation_id: i64, summary: &str) -> AppResult<()> {
        self.publish_hot_summary(conversation_id, summary).await;
        let out = self.backend.write_summary(conversation_id, summary).await;
        let _ = self
            .cache
            .delete(&format!("conversation:{conversation_id}:summary:latest"))
            .await;
        let _ = self
            .cache
            .delete_prefix(&format!("conversation:{conversation_id}:context:"))
            .await;
        out
    }

    pub async fn queue_summary_write(
        &self,
        conversation_id: i64,
        trace_id: Option<&str>,
        summary: &str,
    ) -> AppResult<()> {
        if !self.bus_enabled() {
            return self.write_summary(conversation_id, summary).await;
        }
        let envelope = BusEventEnvelope {
            id: uuid::Uuid::new_v4(),
            event_kind: "memory.summary.write".to_string(),
            stream_key: "memory".to_string(),
            aggregate_id: Some(format!("conversation:{conversation_id}")),
            conversation_id: Some(conversation_id),
            trace_id: trace_id.map(ToString::to_string),
            task_id: None,
            subagent_id: None,
            route_key: None,
            resolved_model: None,
            evidence_count: None,
            reasoning_tier: None,
            fallback_kind: None,
            created_at: Utc::now(),
            payload: serde_json::json!({
                "conversation_id": conversation_id,
                "summary": summary,
            }),
        };
        self.enqueue_bus_event(&envelope).await
    }

    pub async fn count_turns(&self, conversation_id: i64) -> AppResult<i64> {
        self.backend.count_turns(conversation_id).await
    }

    pub async fn latest_summary(&self, conversation_id: i64) -> AppResult<Option<String>> {
        if let Some(summary) = self.hot_latest_summary(conversation_id).await {
            return Ok(summary);
        }
        let key = format!("conversation:{conversation_id}:summary:latest");
        self.cache
            .cached_json(&key, CacheScope::Memory, || async move {
                self.backend.latest_summary(conversation_id).await
            })
            .await
    }

    pub async fn write_fact(
        &self,
        conversation_id: Option<i64>,
        fact_key: &str,
        fact_value: &str,
        confidence: f64,
        source_turn_id: Option<i64>,
    ) -> AppResult<()> {
        let out = self
            .backend
            .write_fact(
                conversation_id,
                fact_key,
                fact_value,
                confidence,
                source_turn_id,
            )
            .await;
        if let Some(conversation_id) = conversation_id {
            let _ = self
                .cache
                .delete_prefix(&format!("conversation:{conversation_id}:context:"))
                .await;
            let _ = self
                .cache
                .delete_prefix(&format!("memory:search:{conversation_id}:"))
                .await;
        }
        out
    }

    pub async fn queue_fact_write(
        &self,
        conversation_id: Option<i64>,
        trace_id: Option<&str>,
        fact_key: &str,
        fact_value: &str,
        confidence: f64,
        source_turn_id: Option<i64>,
    ) -> AppResult<()> {
        if !self.bus_enabled() {
            return self
                .write_fact(
                    conversation_id,
                    fact_key,
                    fact_value,
                    confidence,
                    source_turn_id,
                )
                .await;
        }
        let envelope = BusEventEnvelope {
            id: uuid::Uuid::new_v4(),
            event_kind: "memory.fact.write".to_string(),
            stream_key: "memory".to_string(),
            aggregate_id: conversation_id.map(|id| format!("conversation:{id}")),
            conversation_id,
            trace_id: trace_id.map(ToString::to_string),
            task_id: None,
            subagent_id: None,
            route_key: None,
            resolved_model: None,
            evidence_count: None,
            reasoning_tier: None,
            fallback_kind: None,
            created_at: Utc::now(),
            payload: serde_json::json!({
                "conversation_id": conversation_id,
                "fact_key": fact_key,
                "fact_value": fact_value,
                "confidence": confidence,
                "source_turn_id": source_turn_id,
            }),
        };
        self.enqueue_bus_event(&envelope).await
    }

    pub async fn save_or_merge_brain_candidates(
        &self,
        candidates: &[BrainWriteCandidate],
    ) -> AppResult<()> {
        self.backend
            .save_or_merge_brain_candidates(candidates)
            .await
    }

    pub async fn queue_brain_write(
        &self,
        trace_id: Option<&str>,
        candidates: &[BrainWriteCandidate],
    ) -> AppResult<()> {
        if candidates.is_empty() {
            return Ok(());
        }
        if !self.bus_enabled() {
            return self.save_or_merge_brain_candidates(candidates).await;
        }

        let aggregate_id = candidates
            .iter()
            .find_map(|candidate| {
                candidate
                    .conversation_id
                    .map(|conversation_id| format!("conversation:{conversation_id}"))
            })
            .or_else(|| {
                candidates.iter().find_map(|candidate| {
                    candidate
                        .user_id
                        .as_ref()
                        .map(|user_id| format!("user:{user_id}"))
                })
            });
        let conversation_id = candidates
            .iter()
            .find_map(|candidate| candidate.conversation_id);
        let envelope = BusEventEnvelope {
            id: uuid::Uuid::new_v4(),
            event_kind: "memory.brain.write".to_string(),
            stream_key: "memory".to_string(),
            aggregate_id,
            conversation_id,
            trace_id: trace_id.map(ToString::to_string),
            task_id: None,
            subagent_id: None,
            route_key: None,
            resolved_model: None,
            evidence_count: None,
            reasoning_tier: None,
            fallback_kind: None,
            created_at: Utc::now(),
            payload: serde_json::json!({
                "candidates": candidates,
            }),
        };
        self.enqueue_bus_event(&envelope).await
    }

    pub async fn search_memory_docs(
        &self,
        conversation_id: Option<i64>,
        query: &str,
        limit: usize,
    ) -> AppResult<Vec<String>> {
        let convo_key = conversation_id
            .map(|id| id.to_string())
            .unwrap_or_else(|| "global".to_string());
        let key = format!(
            "memory:search:{convo_key}:{}:{limit}",
            CacheLayer::key_suffix(query)
        );
        self.cache
            .cached_json(&key, CacheScope::Memory, || async move {
                self.backend
                    .search_memory_docs(conversation_id, query, limit)
                    .await
            })
            .await
    }

    pub async fn search_active_brain(
        &self,
        conversation_id: Option<i64>,
        user_id: Option<&str>,
        query: &str,
        conversation_limit: usize,
        user_limit: usize,
    ) -> AppResult<Vec<BrainMemory>> {
        self.backend
            .search_active_brain(
                conversation_id,
                user_id,
                query,
                conversation_limit,
                user_limit,
            )
            .await
    }

    pub async fn recent_active_brain(
        &self,
        conversation_id: Option<i64>,
        user_id: Option<&str>,
        conversation_limit: usize,
        user_limit: usize,
    ) -> AppResult<Vec<BrainMemory>> {
        self.backend
            .recent_active_brain(conversation_id, user_id, conversation_limit, user_limit)
            .await
    }

    pub async fn search_memory(
        &self,
        conversation_id: Option<i64>,
        user_id: Option<&str>,
        query: &str,
        limit: usize,
    ) -> AppResult<Vec<String>> {
        let brain_limit = limit.min(4);
        let brain = self
            .search_active_brain(conversation_id, user_id, query, brain_limit, brain_limit)
            .await
            .unwrap_or_default();
        let docs = self
            .search_memory_docs(conversation_id, query, limit)
            .await?;
        let mut seen = std::collections::HashSet::new();
        let mut merged = Vec::new();

        for item in brain
            .into_iter()
            .map(|memory| memory.render_for_search_result())
            .chain(docs.into_iter())
        {
            if item.trim().is_empty() || !seen.insert(item.clone()) {
                continue;
            }
            merged.push(item);
            if merged.len() >= limit {
                break;
            }
        }

        Ok(merged)
    }

    pub async fn conversation_context_snapshot(
        &self,
        conversation_id: i64,
        query: &str,
        turn_limit: usize,
        memory_limit: usize,
    ) -> AppResult<ConversationContextSnapshot> {
        if let Some(snapshot) = self
            .hot_context_snapshot(conversation_id, query, turn_limit, memory_limit)
            .await
        {
            return Ok(snapshot);
        }
        let key = format!(
            "conversation:{conversation_id}:context:{}:{turn_limit}:{memory_limit}",
            CacheLayer::key_suffix(query)
        );
        self.cache
            .cached_json(&key, CacheScope::Memory, || async move {
                let recent_turns_fut = self.backend.recent_turns(conversation_id, turn_limit);
                let latest_summary_fut = self.backend.latest_summary(conversation_id);
                let memories_fut =
                    self.backend
                        .search_memory_docs(Some(conversation_id), query, memory_limit);

                let (recent_turns, latest_summary, memories) =
                    tokio::join!(recent_turns_fut, latest_summary_fut, memories_fut);

                Ok(ConversationContextSnapshot {
                    recent_turns: recent_turns?,
                    latest_summary: latest_summary?,
                    memories: memories?,
                })
            })
            .await
    }

    async fn hot_recent_turns(
        &self,
        conversation_id: i64,
        limit: usize,
    ) -> Option<Vec<ConversationTurn>> {
        let cache = self.hot_conversations.read().await;
        let state = cache.get(&conversation_id)?;
        if state.recent_turns.is_empty() {
            return None;
        }
        Some(
            state
                .recent_turns
                .iter()
                .rev()
                .take(limit)
                .cloned()
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect(),
        )
    }

    async fn hot_latest_summary(&self, conversation_id: i64) -> Option<Option<String>> {
        let cache = self.hot_conversations.read().await;
        cache
            .get(&conversation_id)
            .map(|state| state.latest_summary.clone())
    }

    async fn hot_context_snapshot(
        &self,
        conversation_id: i64,
        query: &str,
        turn_limit: usize,
        memory_limit: usize,
    ) -> Option<ConversationContextSnapshot> {
        let (recent_turns, latest_summary) = {
            let cache = self.hot_conversations.read().await;
            let state = cache.get(&conversation_id)?;
            if state.recent_turns.is_empty() {
                return None;
            }
            let recent_turns = state
                .recent_turns
                .iter()
                .rev()
                .take(turn_limit)
                .cloned()
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>();
            (recent_turns, state.latest_summary.clone())
        };

        let memories = match self
            .search_memory_docs(Some(conversation_id), query, memory_limit)
            .await
        {
            Ok(memories) => memories,
            Err(_) => return None,
        };

        Some(ConversationContextSnapshot {
            recent_turns,
            latest_summary,
            memories,
        })
    }

    pub async fn insert_tool_trace(&self, row: ToolTraceInsert<'_>) -> AppResult<()> {
        self.backend.insert_tool_trace(row).await
    }

    pub async fn insert_outbound_message(&self, row: OutboundMessageInsert<'_>) -> AppResult<()> {
        self.backend.insert_outbound_message(row).await
    }

    pub async fn insert_model_usage(
        &self,
        trace_id: &str,
        model: &str,
        prompt_tokens: u32,
        completion_tokens: u32,
        estimated_cost_usd: f64,
        latency_ms: u64,
    ) -> AppResult<()> {
        let out = self
            .backend
            .insert_model_usage(
                trace_id,
                model,
                prompt_tokens,
                completion_tokens,
                estimated_cost_usd,
                latency_ms,
            )
            .await;
        let _ = self.cache.delete("dashboard:cost:total").await;
        out
    }

    pub async fn enqueue_job(
        &self,
        kind: &str,
        payload_json: &Value,
        run_at: DateTime<Utc>,
    ) -> AppResult<i64> {
        self.backend.enqueue_job(kind, payload_json, run_at).await
    }

    pub async fn fetch_due_jobs(
        &self,
        now: DateTime<Utc>,
        limit: usize,
    ) -> AppResult<Vec<(i64, String, Value)>> {
        self.backend.fetch_due_jobs(now, limit).await
    }

    pub async fn claim_due_reminder_jobs(
        &self,
        now: DateTime<Utc>,
        limit: usize,
    ) -> AppResult<Vec<ReminderSendJob>> {
        match self.backend.claim_due_reminder_jobs(now, limit).await {
            Ok(jobs) => Ok(jobs),
            Err(err) if should_retry_transient_storage_error(&err) => {
                self.backend.claim_due_reminder_jobs(now, limit).await
            }
            Err(err) => Err(err),
        }
    }

    pub async fn complete_job(&self, job_id: i64) -> AppResult<()> {
        self.backend.complete_job(job_id).await
    }

    pub async fn fail_job(&self, job_id: i64, reason: &str) -> AppResult<()> {
        self.backend.fail_job(job_id, reason).await
    }

    pub async fn dedupe_processed_event(&self, event_id: &str) -> AppResult<bool> {
        self.backend.dedupe_processed_event(event_id).await
    }

    pub async fn list_reminders(
        &self,
        user_id: &str,
        limit: usize,
    ) -> AppResult<Vec<(i64, String, String, String)>> {
        self.backend.list_reminders(user_id, limit).await
    }

    pub async fn create_reminder(
        &self,
        conversation_id: Option<i64>,
        user_id: &str,
        reminder_text: &str,
        due_at: DateTime<Utc>,
    ) -> AppResult<i64> {
        self.backend
            .create_reminder(conversation_id, user_id, reminder_text, due_at)
            .await
    }

    pub async fn mark_reminder_sent(&self, reminder_id: i64) -> AppResult<()> {
        self.backend.mark_reminder_sent(reminder_id).await
    }

    pub async fn mark_reminder_failed(&self, reminder_id: i64, reason: &str) -> AppResult<()> {
        self.backend.mark_reminder_failed(reminder_id, reason).await
    }

    pub async fn count_outbound_messages(&self) -> AppResult<i64> {
        self.backend.count_outbound_messages().await
    }

    pub async fn count_model_usages(&self) -> AppResult<i64> {
        self.backend.count_model_usages().await
    }

    pub async fn inbound_event_status(&self, event_id: &str) -> AppResult<Option<String>> {
        self.backend.inbound_event_status(event_id).await
    }

    pub async fn upsert_plan_json(
        &self,
        plan_id: &str,
        conversation_id: i64,
        goal: &str,
        plan_json: &Value,
        status: &str,
    ) -> AppResult<()> {
        let out = self
            .backend
            .upsert_plan_json(plan_id, conversation_id, goal, plan_json, status)
            .await;
        let _ = self
            .cache
            .delete(&format!("dashboard:plan:{plan_id}"))
            .await;
        let _ = self.cache.delete("dashboard:plan:latest").await;
        out
    }

    pub async fn upsert_task_json(
        &self,
        task_id: &str,
        plan_id: &str,
        task_json: &Value,
        state: &str,
        assigned_subagent: Option<&str>,
    ) -> AppResult<()> {
        let out = self
            .backend
            .upsert_task_json(task_id, plan_id, task_json, state, assigned_subagent)
            .await;
        let _ = self
            .cache
            .delete(&format!("dashboard:task:{task_id}"))
            .await;
        let _ = self.cache.delete("dashboard:plan:latest").await;
        out
    }

    pub async fn insert_task_attempt(&self, row: TaskAttemptInsert<'_>) -> AppResult<i64> {
        self.backend.insert_task_attempt(row).await
    }

    pub async fn insert_task_artifact(
        &self,
        task_id: &str,
        attempt_id: i64,
        subagent_id: &str,
        artifact_json: &Value,
    ) -> AppResult<i64> {
        self.backend
            .insert_task_artifact(task_id, attempt_id, subagent_id, artifact_json)
            .await
    }

    pub async fn insert_task_review(&self, row: TaskReviewInsert<'_>) -> AppResult<i64> {
        self.backend.insert_task_review(row).await
    }

    pub async fn upsert_subagent_state(
        &self,
        subagent_id: &str,
        role: &str,
        state_json: &Value,
    ) -> AppResult<()> {
        self.backend
            .upsert_subagent_state(subagent_id, role, state_json)
            .await
    }

    pub async fn insert_subagent_heartbeat(
        &self,
        subagent_id: &str,
        state: &str,
        task_id: Option<&str>,
    ) -> AppResult<()> {
        self.backend
            .insert_subagent_heartbeat(subagent_id, state, task_id)
            .await
    }

    pub async fn insert_runtime_event_fields(
        &self,
        id: &str,
        event_type: &str,
        payload_json: &Value,
        created_at: DateTime<Utc>,
    ) -> AppResult<()> {
        let out = self
            .backend
            .insert_runtime_event_fields(id, event_type, payload_json, created_at)
            .await;
        let _ = self.cache.delete_prefix("dashboard:runtime_events:").await;
        out
    }

    pub async fn latest_runtime_events(&self, limit: usize) -> AppResult<Vec<Value>> {
        if let Some(mut projected) = self.runtime_events_projection(limit).await? {
            if projected.len() > limit {
                projected.truncate(limit);
            }
            return Ok(projected);
        }
        let key = format!("dashboard:runtime_events:{limit}");
        self.cache
            .cached_json(&key, CacheScope::Dashboard, || async move {
                self.backend.latest_runtime_events(limit).await
            })
            .await
    }

    pub async fn update_runtime_events_projection(
        &self,
        event: &Value,
        max_items: usize,
    ) -> AppResult<()> {
        let key = "dashboard:projection:runtime_events";
        let mut events = self
            .cache
            .get_json::<Vec<Value>>(key)
            .await?
            .unwrap_or_default();
        let event_id = event.get("id").and_then(Value::as_str).unwrap_or_default();
        events.retain(|existing| existing.get("id").and_then(Value::as_str) != Some(event_id));
        events.insert(0, event.clone());
        if events.len() > max_items {
            events.truncate(max_items);
        }
        self.cache
            .set_json(key, CacheScope::Dashboard, &events)
            .await
    }

    pub async fn runtime_events_projection(&self, limit: usize) -> AppResult<Option<Vec<Value>>> {
        let Some(mut events) = self
            .cache
            .get_json::<Vec<Value>>("dashboard:projection:runtime_events")
            .await?
        else {
            return Ok(None);
        };
        if events.len() > limit {
            events.truncate(limit);
        }
        Ok(Some(events))
    }

    pub async fn get_plan_json(&self, plan_id: &str) -> AppResult<Option<Value>> {
        let key = format!("dashboard:plan:{plan_id}");
        self.cache
            .cached_json(&key, CacheScope::Dashboard, || async move {
                self.backend.get_plan_json(plan_id).await
            })
            .await
    }

    pub async fn get_task_json(&self, task_id: &str) -> AppResult<Option<Value>> {
        let key = format!("dashboard:task:{task_id}");
        self.cache
            .cached_json(&key, CacheScope::Dashboard, || async move {
                self.backend.get_task_json(task_id).await
            })
            .await
    }

    pub async fn latest_plan_snapshot(&self) -> AppResult<Option<Value>> {
        self.cache
            .cached_json(
                "dashboard:plan:latest",
                CacheScope::Dashboard,
                || async move { self.backend.latest_plan_snapshot().await },
            )
            .await
    }

    pub async fn insert_config_snapshot(
        &self,
        snapshot_type: &str,
        source_path: Option<&str>,
        payload_json: &Value,
    ) -> AppResult<()> {
        let out = self
            .backend
            .insert_config_snapshot(snapshot_type, source_path, payload_json)
            .await;
        let _ = self
            .cache
            .delete(&format!("dashboard:config_snapshot:{snapshot_type}"))
            .await;
        out
    }

    pub async fn latest_config_snapshot(&self, snapshot_type: &str) -> AppResult<Option<Value>> {
        let key = format!("dashboard:config_snapshot:{snapshot_type}");
        self.cache
            .cached_json(&key, CacheScope::Dashboard, || async move {
                self.backend.latest_config_snapshot(snapshot_type).await
            })
            .await
    }

    pub async fn total_estimated_cost(&self) -> AppResult<f64> {
        if let Some(raw) = self.cache.get_string("dashboard:cost:total").await? {
            if let Ok(parsed) = raw.parse::<f64>() {
                return Ok(parsed);
            }
        }
        let value = self.backend.total_estimated_cost().await?;
        let _ = self
            .cache
            .set_string(
                "dashboard:cost:total",
                CacheScope::Dashboard,
                &value.to_string(),
            )
            .await;
        Ok(value)
    }

    pub async fn reset_memory_data(&self) -> AppResult<()> {
        self.backend.reset_memory_data().await?;
        self.cache.clear_namespace().await?;
        Ok(())
    }

    pub async fn enqueue_bus_event(&self, envelope: &BusEventEnvelope) -> AppResult<()> {
        self.backend.insert_outbox_event(envelope).await
    }

    pub async fn fetch_pending_outbox_events(
        &self,
        limit: usize,
    ) -> AppResult<Vec<OutboxEventRecord>> {
        match self.backend.fetch_pending_outbox_events(limit).await {
            Ok(events) => Ok(events),
            Err(err) if should_retry_transient_storage_error(&err) => {
                self.backend.fetch_pending_outbox_events(limit).await
            }
            Err(err) => Err(err),
        }
    }

    pub async fn mark_outbox_published(&self, id: uuid::Uuid) -> AppResult<()> {
        match self.backend.mark_outbox_published(id).await {
            Ok(()) => Ok(()),
            Err(err) if should_retry_transient_storage_error(&err) => {
                self.backend.mark_outbox_published(id).await
            }
            Err(err) => Err(err),
        }
    }

    pub async fn record_outbox_failure(
        &self,
        id: uuid::Uuid,
        error: &str,
        max_retries: u32,
    ) -> AppResult<()> {
        self.backend
            .record_outbox_failure(id, error, max_retries)
            .await
    }

    pub async fn outbox_stats(&self) -> AppResult<OutboxStats> {
        self.backend.outbox_stats().await
    }

    pub async fn dispatch_due_jobs_to_bus(
        &self,
        now: DateTime<Utc>,
        limit: usize,
    ) -> AppResult<usize> {
        if !self.bus_enabled() {
            return Ok(0);
        }
        self.backend
            .dispatch_due_jobs_to_outbox(now, limit, "jobs")
            .await
    }

    pub async fn ensure_stream_group(&self, stream_key: &str, group: &str) -> AppResult<()> {
        self.cache
            .ensure_stream_group(&self.bus.stream_prefix, stream_key, group)
            .await
    }

    pub async fn publish_to_stream(
        &self,
        stream_key: &str,
        envelope: &BusEventEnvelope,
    ) -> AppResult<String> {
        self.cache
            .xadd_bus_event(
                &self.bus.stream_prefix,
                stream_key,
                self.bus.stream_maxlen,
                envelope,
            )
            .await
    }

    pub async fn read_stream_group(
        &self,
        stream_key: &str,
        group: &str,
        consumer: &str,
        count: usize,
        block_ms: usize,
    ) -> AppResult<Vec<(String, BusEventEnvelope)>> {
        self.cache
            .xreadgroup_bus_events(
                &self.bus.stream_prefix,
                stream_key,
                group,
                consumer,
                count,
                block_ms,
            )
            .await
    }

    pub async fn claim_stale_stream_events(
        &self,
        stream_key: &str,
        group: &str,
        consumer: &str,
        count: usize,
    ) -> AppResult<Vec<(String, BusEventEnvelope)>> {
        self.cache
            .xautoclaim_bus_events(
                &self.bus.stream_prefix,
                stream_key,
                group,
                consumer,
                self.bus.stream_reclaim_idle_ms,
                count,
            )
            .await
    }

    pub async fn ack_stream_event(
        &self,
        stream_key: &str,
        group: &str,
        redis_id: &str,
    ) -> AppResult<()> {
        self.cache
            .xack_bus_event(&self.bus.stream_prefix, stream_key, group, redis_id)
            .await
    }

    pub async fn mark_stream_processed_once(
        &self,
        group: &str,
        outbox_event_id: &str,
    ) -> AppResult<bool> {
        self.cache
            .mark_processed_once(group, outbox_event_id, 60 * 60)
            .await
    }

    pub async fn increment_stream_failure(
        &self,
        group: &str,
        outbox_event_id: &str,
    ) -> AppResult<u64> {
        self.cache
            .increment_stream_failure(group, outbox_event_id, 24 * 60 * 60)
            .await
    }

    pub async fn clear_stream_failure(&self, group: &str, outbox_event_id: &str) -> AppResult<()> {
        self.cache
            .clear_stream_failure(group, outbox_event_id)
            .await
    }

    pub async fn stream_pending_summary(
        &self,
        stream_key: &str,
        group: &str,
    ) -> AppResult<StreamPendingStats> {
        self.cache
            .stream_pending_summary(&self.bus.stream_prefix, stream_key, group)
            .await
    }

    pub async fn export_conversation_trace(
        &self,
        conversation_id: i64,
    ) -> AppResult<ConversationTraceBundle> {
        self.backend
            .export_conversation_trace(conversation_id)
            .await
    }
}

fn should_retry_transient_storage_error(err: &AppError) -> bool {
    let text = err.to_string().to_ascii_lowercase();
    text.contains("connection closed")
        || text.contains("connection reset")
        || text.contains("broken pipe")
        || text.contains("unexpected eof")
}
