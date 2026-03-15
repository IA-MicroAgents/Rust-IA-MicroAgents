use std::{str::FromStr, time::Duration};

use tokio::time::sleep;

use chrono::{DateTime, Utc};
use deadpool_postgres::{Manager, ManagerConfig, Pool, RecyclingMethod};
use serde_json::Value;
use tokio_postgres::{
    error::SqlState, types::ToSql, Client, Config as PgConfig, NoTls, Transaction,
};
use uuid::Uuid;

use crate::{
    config::DatabaseConfig,
    errors::{AppError, AppResult},
    memory::{
        build_brain_search_text, candidate_should_supersede, BrainMemory, BrainMemoryKind,
        BrainMemoryProvenance, BrainMemoryStatus, BrainScopeKind, BrainWriteCandidate,
    },
    scheduler::jobs::ReminderSendJob,
    storage::{
        schema,
        types::{
            BusEventEnvelope, ConversationTraceBundle, ConversationTurn, InboundEventRecord,
            OutboundMessageInsert, OutboxEventRecord, OutboxStats, TaskAttemptInsert,
            TaskReviewInsert, ToolTraceInsert,
        },
    },
};

#[derive(Clone)]
pub struct PostgresStore {
    pool: Pool,
}

impl PostgresStore {
    pub async fn new(config: &DatabaseConfig) -> AppResult<Self> {
        Self::new_with_schema(config).await
    }

    pub async fn new_with_schema(config: &DatabaseConfig) -> AppResult<Self> {
        let schema_name = validate_schema_name(&config.schema)?;
        let mut bootstrap_cfg = PgConfig::from_str(&config.postgres_url)
            .map_err(|e| AppError::Storage(format!("failed to parse postgres url: {e}")))?;
        bootstrap_cfg.application_name("ai-microagents");
        bootstrap_cfg.connect_timeout(Duration::from_millis(config.connect_timeout_ms));

        {
            let (client, connection) = bootstrap_cfg
                .connect(NoTls)
                .await
                .map_err(|e| AppError::Storage(format!("failed to connect postgres: {e}")))?;
            tokio::spawn(async move {
                if let Err(err) = connection.await {
                    tracing::error!(error = %err, "postgres bootstrap connection error");
                }
            });
            client
                .batch_execute(&format!("CREATE SCHEMA IF NOT EXISTS {schema_name};"))
                .await
                .map_err(|e| {
                    AppError::Storage(format!("failed to configure postgres schema: {e}"))
                })?;
        }

        let mut pooled_cfg = PgConfig::from_str(&config.postgres_url)
            .map_err(|e| AppError::Storage(format!("failed to parse postgres url: {e}")))?;
        pooled_cfg.application_name("ai-microagents");
        pooled_cfg.connect_timeout(Duration::from_millis(config.connect_timeout_ms));
        pooled_cfg.options(format!("-c search_path={},public", schema_name));

        let manager = Manager::from_config(
            pooled_cfg,
            NoTls,
            ManagerConfig {
                recycling_method: RecyclingMethod::Fast,
            },
        );
        let pool = Pool::builder(manager)
            .max_size(config.pool_max.max(1))
            .build()
            .map_err(|e| AppError::Storage(format!("failed to build postgres pool: {e}")))?;

        let store = Self { pool };
        store.apply_migrations().await?;
        store.warm_pool(config.pool_min_idle).await;
        Ok(store)
    }

    async fn warm_pool(&self, min_idle: usize) {
        for _ in 0..min_idle {
            let pool = self.pool.clone();
            tokio::spawn(async move {
                let _ = pool.get().await;
            });
        }
    }

    async fn apply_migrations(&self) -> AppResult<()> {
        let migrations = schema::load_postgres_migrations_from_disk()?;
        let client = self.client().await?;
        for migration in migrations {
            client
                .batch_execute(&migration)
                .await
                .map_err(|e| AppError::Storage(format!("postgres migration failed: {e}")))?;
        }
        Ok(())
    }

    async fn client(&self) -> AppResult<deadpool_postgres::Object> {
        let mut last_error = None;
        for attempt in 1..=3 {
            match self.pool.get().await {
                Ok(client) => return Ok(client),
                Err(err) => {
                    last_error = Some(err.to_string());
                    if attempt < 3 {
                        tracing::warn!(attempt, error = %err, "failed to acquire postgres client; retrying");
                        sleep(Duration::from_millis(50 * attempt as u64)).await;
                    }
                }
            }
        }
        Err(AppError::Storage(format!(
            "failed to acquire postgres client: {}",
            last_error.unwrap_or_else(|| "unknown error".to_string())
        )))
    }

    pub async fn insert_inbound_event(
        &self,
        event_id: &str,
        source: &str,
        payload_json: &Value,
    ) -> AppResult<Option<i64>> {
        let now = Utc::now();
        let client = self.client().await?;
        match client
            .query_opt(
                "INSERT INTO inbound_events(event_id, source, payload_json, received_at, status)
                 VALUES ($1, $2, $3, $4, 'received')
                 RETURNING id",
                &[&event_id, &source, payload_json, &now],
            )
            .await
        {
            Ok(Some(row)) => Ok(Some(row.get::<_, i64>(0))),
            Ok(None) => Ok(None),
            Err(err) if err.code() == Some(&SqlState::UNIQUE_VIOLATION) => Ok(None),
            Err(err) => Err(AppError::Storage(format!(
                "insert inbound event failed: {err}"
            ))),
        }
    }

    pub async fn mark_inbound_processed(&self, event_id: &str) -> AppResult<()> {
        let now = Utc::now();
        let client = self.client().await?;
        client
            .execute(
                "UPDATE inbound_events SET processed_at = $1, status = 'processed' WHERE event_id = $2",
                &[&now, &event_id],
            )
            .await
            .map_err(|e| AppError::Storage(format!("mark inbound processed failed: {e}")))?;
        Ok(())
    }

    pub async fn get_inbound_event_by_event_id(
        &self,
        event_id: &str,
    ) -> AppResult<InboundEventRecord> {
        let client = self.client().await?;
        let row = client
            .query_opt(
                "SELECT id, event_id, source, payload_json, received_at, processed_at, status
                 FROM inbound_events WHERE event_id = $1",
                &[&event_id],
            )
            .await
            .map_err(|e| AppError::Storage(format!("get inbound event failed: {e}")))?
            .ok_or_else(|| AppError::NotFound(format!("event {event_id} not found")))?;

        Ok(InboundEventRecord {
            id: row.get(0),
            event_id: row.get(1),
            source: row.get(2),
            payload_json: row.get(3),
            received_at: row.get(4),
            processed_at: row.get(5),
            status: row.get(6),
        })
    }

    pub async fn upsert_conversation(&self, channel: &str, external_id: &str) -> AppResult<i64> {
        let now = Utc::now();
        let client = self.client().await?;
        let row = client
            .query_one(
                "INSERT INTO conversations(channel, external_id, created_at)
                 VALUES ($1, $2, $3)
                 ON CONFLICT (external_id) DO UPDATE SET channel = EXCLUDED.channel
                 RETURNING id",
                &[&channel, &external_id, &now],
            )
            .await
            .map_err(|e| AppError::Storage(format!("upsert conversation failed: {e}")))?;
        Ok(row.get(0))
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
        let (input_tokens, output_tokens, estimated_cost_usd) = usage.unwrap_or((0, 0, 0.0));
        let now = Utc::now();
        let mut client = self.client().await?;
        let tx = client
            .transaction()
            .await
            .map_err(|e| AppError::Storage(format!("append turn transaction failed: {e}")))?;
        let turn_row = tx
            .query_one(
                "INSERT INTO turns(conversation_id, role, content, trace_id, route, input_tokens, output_tokens, estimated_cost_usd, created_at)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                 RETURNING id",
                &[&conversation_id, &role, &content, &trace_id, &route, &(input_tokens as i64), &(output_tokens as i64), &estimated_cost_usd, &now],
            )
            .await
            .map_err(|e| AppError::Storage(format!("append turn failed: {e}")))?;
        tx.execute(
            "INSERT INTO memory_docs(conversation_id, doc_type, content, created_at) VALUES ($1, 'turn', $2, $3)",
            &[&conversation_id, &content, &now],
        )
        .await
        .map_err(|e| AppError::Storage(format!("append memory doc failed: {e}")))?;
        tx.commit()
            .await
            .map_err(|e| AppError::Storage(format!("append turn commit failed: {e}")))?;
        Ok(turn_row.get(0))
    }

    pub async fn recent_turns(
        &self,
        conversation_id: i64,
        limit: usize,
    ) -> AppResult<Vec<ConversationTurn>> {
        let client = self.client().await?;
        let rows = client
            .query(
                "SELECT role, content, created_at FROM turns WHERE conversation_id = $1 ORDER BY id DESC LIMIT $2",
                &[&conversation_id, &(limit as i64)],
            )
            .await
            .map_err(|e| AppError::Storage(format!("recent turns failed: {e}")))?;
        let mut turns = rows
            .into_iter()
            .map(|row| ConversationTurn {
                role: row.get(0),
                content: row.get(1),
                created_at: row.get(2),
            })
            .collect::<Vec<_>>();
        turns.reverse();
        Ok(turns)
    }

    pub async fn write_summary(&self, conversation_id: i64, summary: &str) -> AppResult<()> {
        let client = self.client().await?;
        client
            .execute(
                "INSERT INTO summaries(conversation_id, summary, created_at) VALUES ($1, $2, $3)",
                &[&conversation_id, &summary, &Utc::now()],
            )
            .await
            .map_err(|e| AppError::Storage(format!("write summary failed: {e}")))?;
        Ok(())
    }

    pub async fn count_turns(&self, conversation_id: i64) -> AppResult<i64> {
        let client = self.client().await?;
        client
            .query_one(
                "SELECT COUNT(*) FROM turns WHERE conversation_id = $1",
                &[&conversation_id],
            )
            .await
            .map(|row| row.get::<_, i64>(0))
            .map_err(|e| AppError::Storage(format!("count turns failed: {e}")))
    }

    pub async fn latest_summary(&self, conversation_id: i64) -> AppResult<Option<String>> {
        let client = self.client().await?;
        client
            .query_opt(
                "SELECT summary FROM summaries WHERE conversation_id = $1 ORDER BY id DESC LIMIT 1",
                &[&conversation_id],
            )
            .await
            .map(|opt| opt.map(|row| row.get::<_, String>(0)))
            .map_err(|e| AppError::Storage(format!("latest summary failed: {e}")))
    }

    pub async fn write_fact(
        &self,
        conversation_id: Option<i64>,
        fact_key: &str,
        fact_value: &str,
        confidence: f64,
        source_turn_id: Option<i64>,
    ) -> AppResult<()> {
        let client = self.client().await?;
        client
            .execute(
                "INSERT INTO facts(conversation_id, fact_key, fact_value, confidence, source_turn_id, created_at)
                 VALUES ($1, $2, $3, $4, $5, $6)",
                &[&conversation_id, &fact_key, &fact_value, &confidence, &source_turn_id, &Utc::now()],
            )
            .await
            .map_err(|e| AppError::Storage(format!("write fact failed: {e}")))?;
        Ok(())
    }

    pub async fn save_or_merge_brain_candidates(
        &self,
        candidates: &[BrainWriteCandidate],
    ) -> AppResult<()> {
        if candidates.is_empty() {
            return Ok(());
        }

        let mut client = self.client().await?;
        let tx = client.transaction().await.map_err(|e| {
            AppError::Storage(format!("brain memory transaction start failed: {e}"))
        })?;

        for candidate in candidates {
            save_or_merge_brain_candidate(&tx, candidate).await?;
        }

        tx.commit().await.map_err(|e| {
            AppError::Storage(format!("brain memory transaction commit failed: {e}"))
        })?;
        Ok(())
    }

    pub async fn search_active_brain(
        &self,
        conversation_id: Option<i64>,
        user_id: Option<&str>,
        query: &str,
        conversation_limit: usize,
        user_limit: usize,
    ) -> AppResult<Vec<BrainMemory>> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }

        let client = self.client().await?;
        let mut results = Vec::new();

        if let Some(conversation_id) = conversation_id.filter(|_| conversation_limit > 0) {
            let rows = client
                .query(
                    "SELECT id, scope_kind, user_id, conversation_id, memory_kind, memory_key, subject,
                            what_value, why_value, where_context, learned_value, provenance_json,
                            confidence, status, superseded_by, source_turn_id, created_at, updated_at
                     FROM brain_memories
                     WHERE status = 'active'
                       AND scope_kind = 'conversation'
                       AND conversation_id = $2
                       AND to_tsvector('simple', search_text) @@ plainto_tsquery('simple', $1)
                     ORDER BY ts_rank_cd(to_tsvector('simple', search_text), plainto_tsquery('simple', $1)) DESC,
                              confidence DESC,
                              updated_at DESC
                     LIMIT $3",
                    &[&query, &conversation_id, &(conversation_limit as i64)],
                )
                .await
                .map_err(|e| AppError::Storage(format!("search conversation brain failed: {e}")))?;
            results.extend(
                rows.into_iter()
                    .map(scan_brain_memory_row)
                    .collect::<AppResult<Vec<_>>>()?,
            );
        }

        if let Some(user_id) =
            user_id.filter(|user_id| !user_id.trim().is_empty() && user_limit > 0)
        {
            let rows = client
                .query(
                    "SELECT id, scope_kind, user_id, conversation_id, memory_kind, memory_key, subject,
                            what_value, why_value, where_context, learned_value, provenance_json,
                            confidence, status, superseded_by, source_turn_id, created_at, updated_at
                     FROM brain_memories
                     WHERE status = 'active'
                       AND scope_kind = 'user'
                       AND user_id = $2
                       AND to_tsvector('simple', search_text) @@ plainto_tsquery('simple', $1)
                     ORDER BY ts_rank_cd(to_tsvector('simple', search_text), plainto_tsquery('simple', $1)) DESC,
                              confidence DESC,
                              updated_at DESC
                     LIMIT $3",
                    &[&query, &user_id, &(user_limit as i64)],
                )
                .await
                .map_err(|e| AppError::Storage(format!("search user brain failed: {e}")))?;
            results.extend(
                rows.into_iter()
                    .map(scan_brain_memory_row)
                    .collect::<AppResult<Vec<_>>>()?,
            );
        }

        Ok(results)
    }

    pub async fn recent_active_brain(
        &self,
        conversation_id: Option<i64>,
        user_id: Option<&str>,
        conversation_limit: usize,
        user_limit: usize,
    ) -> AppResult<Vec<BrainMemory>> {
        let client = self.client().await?;
        let mut results = Vec::new();

        if let Some(conversation_id) = conversation_id.filter(|_| conversation_limit > 0) {
            let rows = client
                .query(
                    "SELECT id, scope_kind, user_id, conversation_id, memory_kind, memory_key, subject,
                            what_value, why_value, where_context, learned_value, provenance_json,
                            confidence, status, superseded_by, source_turn_id, created_at, updated_at
                     FROM brain_memories
                     WHERE status = 'active'
                       AND scope_kind = 'conversation'
                       AND conversation_id = $1
                       AND memory_kind IN ('goal', 'decision', 'constraint', 'source_location')
                     ORDER BY updated_at DESC, confidence DESC
                     LIMIT $2",
                    &[&conversation_id, &(conversation_limit.min(2) as i64)],
                )
                .await
                .map_err(|e| AppError::Storage(format!("load recent conversation brain failed: {e}")))?;
            results.extend(
                rows.into_iter()
                    .map(scan_brain_memory_row)
                    .collect::<AppResult<Vec<_>>>()?,
            );
        }

        if let Some(user_id) =
            user_id.filter(|user_id| !user_id.trim().is_empty() && user_limit > 0)
        {
            let rows = client
                .query(
                    "SELECT id, scope_kind, user_id, conversation_id, memory_kind, memory_key, subject,
                            what_value, why_value, where_context, learned_value, provenance_json,
                            confidence, status, superseded_by, source_turn_id, created_at, updated_at
                     FROM brain_memories
                     WHERE status = 'active'
                       AND scope_kind = 'user'
                       AND user_id = $1
                       AND memory_kind IN ('preference', 'constraint', 'profile_fact')
                     ORDER BY updated_at DESC, confidence DESC
                     LIMIT $2",
                    &[&user_id, &(user_limit.min(2) as i64)],
                )
                .await
                .map_err(|e| AppError::Storage(format!("load recent user brain failed: {e}")))?;
            results.extend(
                rows.into_iter()
                    .map(scan_brain_memory_row)
                    .collect::<AppResult<Vec<_>>>()?,
            );
        }

        Ok(results)
    }

    pub async fn search_memory_docs(
        &self,
        conversation_id: Option<i64>,
        query: &str,
        limit: usize,
    ) -> AppResult<Vec<String>> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        let client = self.client().await?;
        let rows = client
            .query(
                "SELECT content
                 FROM memory_docs
                 WHERE ($2::BIGINT IS NULL OR conversation_id = $2)
                   AND to_tsvector('simple', content) @@ plainto_tsquery('simple', $1)
                 ORDER BY ts_rank_cd(to_tsvector('simple', content), plainto_tsquery('simple', $1)) DESC,
                          id DESC
                 LIMIT $3",
                &[&query, &conversation_id, &(limit as i64)],
            )
            .await
            .map_err(|e| AppError::Storage(format!("search memory docs failed: {e}")))?;
        Ok(rows
            .into_iter()
            .map(|row| row.get::<_, String>(0))
            .collect())
    }

    pub async fn insert_tool_trace(&self, row: ToolTraceInsert<'_>) -> AppResult<()> {
        let client = self.client().await?;
        client
            .execute(
                "INSERT INTO tool_traces(trace_id, skill_name, input_json, output_json, status, duration_ms, error, created_at)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
                &[&row.trace_id, &row.skill_name, row.input_json, &row.output_json, &row.status, &(row.duration_ms as i64), &row.error, &Utc::now()],
            )
            .await
            .map_err(|e| AppError::Storage(format!("insert tool trace failed: {e}")))?;
        Ok(())
    }

    pub async fn insert_outbound_message(&self, row: OutboundMessageInsert<'_>) -> AppResult<()> {
        let client = self.client().await?;
        client
            .execute(
                "INSERT INTO outbound_messages(trace_id, conversation_id, channel, recipient, content, provider_message_id, status, created_at)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
                &[&row.trace_id, &row.conversation_id, &row.channel, &row.recipient, &row.content, &row.provider_message_id, &row.status, &Utc::now()],
            )
            .await
            .map_err(|e| AppError::Storage(format!("insert outbound message failed: {e}")))?;
        Ok(())
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
        let client = self.client().await?;
        client
            .execute(
                "INSERT INTO model_usages(trace_id, model, prompt_tokens, completion_tokens, estimated_cost_usd, latency_ms, created_at)
                 VALUES ($1, $2, $3, $4, $5, $6, $7)",
                &[&trace_id, &model, &(prompt_tokens as i64), &(completion_tokens as i64), &estimated_cost_usd, &(latency_ms as i64), &Utc::now()],
            )
            .await
            .map_err(|e| AppError::Storage(format!("insert model usage failed: {e}")))?;
        Ok(())
    }

    pub async fn enqueue_job(
        &self,
        kind: &str,
        payload_json: &Value,
        run_at: DateTime<Utc>,
    ) -> AppResult<i64> {
        let client = self.client().await?;
        let row = client
            .query_one(
                "INSERT INTO jobs(kind, payload_json, run_at, status, retries, created_at)
                 VALUES ($1, $2, $3, 'scheduled', 0, $4)
                 RETURNING id",
                &[&kind, payload_json, &run_at, &Utc::now()],
            )
            .await
            .map_err(|e| AppError::Storage(format!("enqueue job failed: {e}")))?;
        Ok(row.get(0))
    }

    pub async fn fetch_due_jobs(
        &self,
        now: DateTime<Utc>,
        limit: usize,
    ) -> AppResult<Vec<(i64, String, Value)>> {
        let client = self.client().await?;
        let rows = client
            .query(
                "SELECT id, kind, payload_json FROM jobs WHERE status = 'scheduled' AND run_at <= $1 ORDER BY run_at ASC LIMIT $2",
                &[&now, &(limit as i64)],
            )
            .await
            .map_err(|e| AppError::Storage(format!("fetch due jobs failed: {e}")))?;
        Ok(rows
            .into_iter()
            .map(|row| {
                (
                    row.get::<_, i64>(0),
                    row.get::<_, String>(1),
                    row.get::<_, Value>(2),
                )
            })
            .collect())
    }

    pub async fn complete_job(&self, job_id: i64) -> AppResult<()> {
        let client = self.client().await?;
        client
            .execute(
                "UPDATE jobs SET status = 'done', last_run_at = $1 WHERE id = $2",
                &[&Utc::now(), &job_id],
            )
            .await
            .map_err(|e| AppError::Storage(format!("complete job failed: {e}")))?;
        Ok(())
    }

    pub async fn fail_job(&self, job_id: i64, reason: &str) -> AppResult<()> {
        let client = self.client().await?;
        client
            .execute(
                "UPDATE jobs
                 SET status = CASE WHEN retries >= 3 THEN 'failed' ELSE 'scheduled' END,
                     retries = retries + 1,
                     last_error = $1,
                     last_run_at = $2,
                     run_at = NOW() + interval '1 minute'
                 WHERE id = $3",
                &[&reason, &Utc::now(), &job_id],
            )
            .await
            .map_err(|e| AppError::Storage(format!("fail job failed: {e}")))?;
        Ok(())
    }

    pub async fn dispatch_due_jobs_to_outbox(
        &self,
        now: DateTime<Utc>,
        limit: usize,
        stream_key: &str,
    ) -> AppResult<usize> {
        let mut client = self.client().await?;
        let tx = client
            .transaction()
            .await
            .map_err(|e| AppError::Storage(format!("dispatch due jobs transaction failed: {e}")))?;

        let rows = tx
            .query(
                "SELECT id, kind, payload_json
                 FROM jobs
                 WHERE status = 'scheduled' AND run_at <= $1
                 ORDER BY run_at ASC
                 LIMIT $2
                 FOR UPDATE SKIP LOCKED",
                &[&now, &(limit as i64)],
            )
            .await
            .map_err(|e| AppError::Storage(format!("dispatch due jobs select failed: {e}")))?;

        let mut dispatched = 0usize;
        for row in rows {
            let job_id: i64 = row.get(0);
            let kind: String = row.get(1);
            let mut payload_json: Value = row.get(2);
            if let Some(obj) = payload_json.as_object_mut() {
                obj.insert("job_id".to_string(), Value::from(job_id));
                obj.insert("job_kind".to_string(), Value::from(kind.clone()));
            }
            let outbox_id = Uuid::new_v4();
            tx.execute(
                "UPDATE jobs SET status = 'queued', last_run_at = $1 WHERE id = $2",
                &[&now, &job_id],
            )
            .await
            .map_err(|e| AppError::Storage(format!("dispatch due jobs update failed: {e}")))?;
            tx.execute(
                "INSERT INTO outbox_events(
                    id, event_kind, stream_key, aggregate_id, conversation_id, trace_id,
                    task_id, subagent_id, route_key, resolved_model, payload_json,
                    created_at, publish_attempts, published_at, last_error
                 )
                 VALUES ($1, $2, $3, $4, NULL, NULL, NULL, NULL, NULL, NULL, $5, $6, 0, NULL, NULL)",
                &[
                    &outbox_id,
                    &format!("job.dispatch.{kind}"),
                    &stream_key,
                    &format!("job:{job_id}"),
                    &payload_json,
                    &now,
                ],
            )
            .await
            .map_err(|e| AppError::Storage(format!("dispatch due jobs outbox insert failed: {e}")))?;
            dispatched += 1;
        }

        tx.commit()
            .await
            .map_err(|e| AppError::Storage(format!("dispatch due jobs commit failed: {e}")))?;
        Ok(dispatched)
    }

    pub async fn claim_due_reminder_jobs(
        &self,
        now: DateTime<Utc>,
        limit: usize,
    ) -> AppResult<Vec<ReminderSendJob>> {
        let mut client = self.client().await?;
        let tx = client.transaction().await.map_err(|e| {
            AppError::Storage(format!("claim due reminder jobs transaction failed: {e}"))
        })?;

        let rows = tx
            .query(
                "SELECT id, payload_json
                 FROM jobs
                 WHERE kind = 'reminder.send' AND status = 'scheduled' AND run_at <= $1
                 ORDER BY run_at ASC
                 LIMIT $2
                 FOR UPDATE SKIP LOCKED",
                &[&now, &(limit as i64)],
            )
            .await
            .map_err(|e| {
                AppError::Storage(format!("claim due reminder jobs select failed: {e}"))
            })?;

        let mut jobs = Vec::with_capacity(rows.len());
        for row in rows {
            let job_id: i64 = row.get(0);
            let payload_json: Value = row.get(1);
            let mut job: ReminderSendJob = serde_json::from_value(payload_json).map_err(|e| {
                AppError::Storage(format!("claim due reminder jobs decode failed: {e}"))
            })?;
            job.job_id = Some(job_id);

            tx.execute(
                "UPDATE jobs SET status = 'queued', last_run_at = $1 WHERE id = $2",
                &[&now, &job_id],
            )
            .await
            .map_err(|e| {
                AppError::Storage(format!("claim due reminder jobs update failed: {e}"))
            })?;

            jobs.push(job);
        }

        tx.commit().await.map_err(|e| {
            AppError::Storage(format!("claim due reminder jobs commit failed: {e}"))
        })?;
        Ok(jobs)
    }

    pub async fn dedupe_processed_event(&self, event_id: &str) -> AppResult<bool> {
        let client = self.client().await?;
        let rows = client
            .execute(
                "INSERT INTO processed_event_dedup(event_id, processed_at) VALUES ($1, $2) ON CONFLICT (event_id) DO NOTHING",
                &[&event_id, &Utc::now()],
            )
            .await
            .map_err(|e| AppError::Storage(format!("processed dedupe failed: {e}")))?;
        Ok(rows > 0)
    }

    pub async fn list_reminders(
        &self,
        user_id: &str,
        limit: usize,
    ) -> AppResult<Vec<(i64, String, String, String)>> {
        let client = self.client().await?;
        let rows = client
            .query(
                "SELECT id, reminder_text, due_at, status FROM reminders WHERE user_id = $1 ORDER BY due_at ASC LIMIT $2",
                &[&user_id, &(limit as i64)],
            )
            .await
            .map_err(|e| AppError::Storage(format!("list reminders failed: {e}")))?;
        Ok(rows
            .into_iter()
            .map(|row| {
                let due_at: DateTime<Utc> = row.get(2);
                (row.get(0), row.get(1), due_at.to_rfc3339(), row.get(3))
            })
            .collect())
    }

    pub async fn create_reminder(
        &self,
        conversation_id: Option<i64>,
        user_id: &str,
        reminder_text: &str,
        due_at: DateTime<Utc>,
    ) -> AppResult<i64> {
        let now = Utc::now();
        let mut client = self.client().await?;
        let tx = client
            .transaction()
            .await
            .map_err(|e| AppError::Storage(format!("create reminder transaction failed: {e}")))?;
        let channel = if let Some(id) = conversation_id {
            tx.query_opt("SELECT channel FROM conversations WHERE id = $1", &[&id])
                .await
                .map_err(|e| AppError::Storage(format!("lookup reminder channel failed: {e}")))?
                .map(|row| row.get::<_, String>(0))
                .unwrap_or_else(|| "local".to_string())
        } else {
            "local".to_string()
        };

        let reminder_id = tx
            .query_one(
                "INSERT INTO reminders(conversation_id, user_id, reminder_text, due_at, status, retries, created_at, updated_at)
                 VALUES ($1, $2, $3, $4, 'scheduled', 0, $5, $5)
                 RETURNING id",
                &[&conversation_id, &user_id, &reminder_text, &due_at, &now],
            )
            .await
            .map_err(|e| AppError::Storage(format!("create reminder failed: {e}")))?
            .get::<_, i64>(0);

        let payload = serde_json::json!({
            "reminder_id": reminder_id,
            "conversation_id": conversation_id,
            "channel": channel,
            "user_id": user_id,
            "text": reminder_text,
        });
        tx.execute(
            "INSERT INTO jobs(kind, payload_json, run_at, status, retries, created_at)
             VALUES ('reminder.send', $1, $2, 'scheduled', 0, $3)",
            &[&payload, &due_at, &Utc::now()],
        )
        .await
        .map_err(|e| AppError::Storage(format!("enqueue reminder job failed: {e}")))?;
        tx.commit()
            .await
            .map_err(|e| AppError::Storage(format!("create reminder commit failed: {e}")))?;
        Ok(reminder_id)
    }

    pub async fn mark_reminder_sent(&self, reminder_id: i64) -> AppResult<()> {
        let client = self.client().await?;
        client
            .execute(
                "UPDATE reminders SET status = 'sent', updated_at = $1 WHERE id = $2",
                &[&Utc::now(), &reminder_id],
            )
            .await
            .map_err(|e| AppError::Storage(format!("mark reminder sent failed: {e}")))?;
        Ok(())
    }

    pub async fn mark_reminder_failed(&self, reminder_id: i64, reason: &str) -> AppResult<()> {
        let client = self.client().await?;
        client
            .execute(
                "UPDATE reminders SET status = 'failed', retries = retries + 1, last_error = $1, updated_at = $2 WHERE id = $3",
                &[&reason, &Utc::now(), &reminder_id],
            )
            .await
            .map_err(|e| AppError::Storage(format!("mark reminder failed failed: {e}")))?;
        Ok(())
    }

    pub async fn count_outbound_messages(&self) -> AppResult<i64> {
        self.count_scalar("SELECT COUNT(*) FROM outbound_messages", &[])
            .await
    }

    pub async fn count_model_usages(&self) -> AppResult<i64> {
        self.count_scalar("SELECT COUNT(*) FROM model_usages", &[])
            .await
    }

    pub async fn inbound_event_status(&self, event_id: &str) -> AppResult<Option<String>> {
        let client = self.client().await?;
        client
            .query_opt(
                "SELECT status FROM inbound_events WHERE event_id = $1",
                &[&event_id],
            )
            .await
            .map(|row| row.map(|r| r.get::<_, String>(0)))
            .map_err(|e| AppError::Storage(format!("inbound event status failed: {e}")))
    }

    pub async fn upsert_plan_json(
        &self,
        plan_id: &str,
        conversation_id: i64,
        goal: &str,
        plan_json: &Value,
        status: &str,
    ) -> AppResult<()> {
        let now = Utc::now();
        let client = self.client().await?;
        client
            .execute(
                "INSERT INTO plans(plan_id, conversation_id, goal, plan_json, status, created_at, updated_at)
                 VALUES ($1, $2, $3, $4, $5, $6, $6)
                 ON CONFLICT (plan_id) DO UPDATE SET
                   goal = EXCLUDED.goal,
                   plan_json = EXCLUDED.plan_json,
                   status = EXCLUDED.status,
                   updated_at = EXCLUDED.updated_at",
                &[&plan_id, &conversation_id, &goal, plan_json, &status, &now],
            )
            .await
            .map_err(|e| AppError::Storage(format!("upsert plan failed: {e}")))?;
        Ok(())
    }

    pub async fn upsert_task_json(
        &self,
        task_id: &str,
        plan_id: &str,
        task_json: &Value,
        state: &str,
        assigned_subagent: Option<&str>,
    ) -> AppResult<()> {
        let now = Utc::now();
        let client = self.client().await?;
        client
            .execute(
                "INSERT INTO tasks(task_id, plan_id, task_json, state, assigned_subagent, created_at, updated_at)
                 VALUES ($1, $2, $3, $4, $5, $6, $6)
                 ON CONFLICT (task_id) DO UPDATE SET
                   task_json = EXCLUDED.task_json,
                   state = EXCLUDED.state,
                   assigned_subagent = EXCLUDED.assigned_subagent,
                   updated_at = EXCLUDED.updated_at",
                &[&task_id, &plan_id, task_json, &state, &assigned_subagent, &now],
            )
            .await
            .map_err(|e| AppError::Storage(format!("upsert task failed: {e}")))?;
        Ok(())
    }

    pub async fn insert_task_attempt(&self, row: TaskAttemptInsert<'_>) -> AppResult<i64> {
        let client = self.client().await?;
        let inserted = client
            .query_one(
                "INSERT INTO task_attempts(task_id, attempt_no, subagent_id, status, started_at, ended_at, error, duration_ms, created_at)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                 ON CONFLICT (task_id, attempt_no) DO UPDATE SET
                   subagent_id = EXCLUDED.subagent_id,
                   status = EXCLUDED.status,
                   started_at = EXCLUDED.started_at,
                   ended_at = EXCLUDED.ended_at,
                   error = EXCLUDED.error,
                   duration_ms = EXCLUDED.duration_ms
                 RETURNING id",
                &[&row.task_id, &(row.attempt_no as i64), &row.subagent_id, &row.status, &row.started_at, &row.ended_at, &row.error, &row.duration_ms.map(|d| d as i64), &Utc::now()],
            )
            .await
            .map_err(|e| AppError::Storage(format!("insert task attempt failed: {e}")))?;
        Ok(inserted.get(0))
    }

    pub async fn insert_task_artifact(
        &self,
        task_id: &str,
        attempt_id: i64,
        subagent_id: &str,
        artifact_json: &Value,
    ) -> AppResult<i64> {
        let client = self.client().await?;
        let inserted = client
            .query_one(
                "INSERT INTO task_artifacts(task_id, attempt_id, subagent_id, artifact_json, created_at)
                 VALUES ($1, $2, $3, $4, $5)
                 RETURNING id",
                &[&task_id, &attempt_id, &subagent_id, artifact_json, &Utc::now()],
            )
            .await
            .map_err(|e| AppError::Storage(format!("insert task artifact failed: {e}")))?;
        Ok(inserted.get(0))
    }

    pub async fn insert_task_review(&self, row: TaskReviewInsert<'_>) -> AppResult<i64> {
        let client = self.client().await?;
        let inserted = client
            .query_one(
                "INSERT INTO task_reviews(task_id, attempt_no, reviewer, action, score, notes, decision_json, created_at)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
                 RETURNING id",
                &[&row.task_id, &(row.attempt_no as i64), &row.reviewer, &row.action, &row.score, &row.notes, row.decision_json, &Utc::now()],
            )
            .await
            .map_err(|e| AppError::Storage(format!("insert task review failed: {e}")))?;
        Ok(inserted.get(0))
    }

    pub async fn upsert_subagent_state(
        &self,
        subagent_id: &str,
        role: &str,
        state_json: &Value,
    ) -> AppResult<()> {
        let client = self.client().await?;
        client
            .execute(
                "INSERT INTO subagent_states(subagent_id, role, state_json, updated_at)
                 VALUES ($1, $2, $3, $4)
                 ON CONFLICT (subagent_id) DO UPDATE SET
                   role = EXCLUDED.role,
                   state_json = EXCLUDED.state_json,
                   updated_at = EXCLUDED.updated_at",
                &[&subagent_id, &role, state_json, &Utc::now()],
            )
            .await
            .map_err(|e| AppError::Storage(format!("upsert subagent state failed: {e}")))?;
        Ok(())
    }

    pub async fn insert_subagent_heartbeat(
        &self,
        subagent_id: &str,
        state: &str,
        task_id: Option<&str>,
    ) -> AppResult<()> {
        let now = Utc::now();
        let client = self.client().await?;
        client
            .execute(
                "INSERT INTO subagent_heartbeats(subagent_id, heartbeat_at, state, task_id, created_at)
                 VALUES ($1, $2, $3, $4, $5)",
                &[&subagent_id, &now, &state, &task_id, &now],
            )
            .await
            .map_err(|e| AppError::Storage(format!("insert subagent heartbeat failed: {e}")))?;
        Ok(())
    }

    pub async fn insert_runtime_event_fields(
        &self,
        id: &str,
        event_type: &str,
        payload_json: &Value,
        created_at: DateTime<Utc>,
    ) -> AppResult<()> {
        let event_id = Uuid::parse_str(id)
            .map_err(|e| AppError::Storage(format!("invalid runtime event uuid {id}: {e}")))?;
        let client = self.client().await?;
        client
            .execute(
                "INSERT INTO runtime_events(id, event_type, payload_json, created_at) VALUES ($1, $2, $3, $4)
                 ON CONFLICT (id) DO NOTHING",
                &[&event_id, &event_type, payload_json, &created_at],
            )
            .await
            .map_err(|e| AppError::Storage(format!("insert runtime event failed: {e}")))?;
        Ok(())
    }

    pub async fn insert_outbox_event(&self, envelope: &BusEventEnvelope) -> AppResult<()> {
        let client = self.client().await?;
        client
            .execute(
                "INSERT INTO outbox_events(
                    id, event_kind, stream_key, aggregate_id, conversation_id, trace_id,
                    task_id, subagent_id, route_key, resolved_model, evidence_count,
                    reasoning_tier, fallback_kind, payload_json,
                    created_at, publish_attempts, published_at, last_error
                 )
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, 0, NULL, NULL)
                 ON CONFLICT (id) DO NOTHING",
                &[
                    &envelope.id,
                    &envelope.event_kind,
                    &envelope.stream_key,
                    &envelope.aggregate_id,
                    &envelope.conversation_id,
                    &envelope.trace_id,
                    &envelope.task_id,
                    &envelope.subagent_id,
                    &envelope.route_key,
                    &envelope.resolved_model,
                    &envelope.evidence_count.map(|value| value as i64),
                    &envelope.reasoning_tier,
                    &envelope.fallback_kind,
                    &envelope.payload,
                    &envelope.created_at,
                ],
            )
            .await
            .map_err(|e| AppError::Storage(format!("insert outbox event failed: {e}")))?;
        Ok(())
    }

    pub async fn fetch_pending_outbox_events(
        &self,
        limit: usize,
    ) -> AppResult<Vec<OutboxEventRecord>> {
        let client = self.client().await?;
        let rows = client
            .query(
                "SELECT
                    id, event_kind, stream_key, aggregate_id, conversation_id, trace_id,
                    task_id, subagent_id, route_key, resolved_model, evidence_count,
                    reasoning_tier, fallback_kind, payload_json,
                    created_at, publish_attempts, published_at, last_error
                 FROM outbox_events
                 WHERE published_at IS NULL
                 ORDER BY created_at ASC
                 LIMIT $1",
                &[&(limit as i64)],
            )
            .await
            .map_err(|e| AppError::Storage(format!("fetch pending outbox events failed: {e}")))?;
        Ok(rows
            .into_iter()
            .map(|row| OutboxEventRecord {
                envelope: BusEventEnvelope {
                    id: row.get(0),
                    event_kind: row.get(1),
                    stream_key: row.get(2),
                    aggregate_id: row.get(3),
                    conversation_id: row.get(4),
                    trace_id: row.get(5),
                    task_id: row.get(6),
                    subagent_id: row.get(7),
                    route_key: row.get(8),
                    resolved_model: row.get(9),
                    evidence_count: row
                        .get::<_, Option<i64>>(10)
                        .map(|value| value.max(0) as u32),
                    reasoning_tier: row.get(11),
                    fallback_kind: row.get(12),
                    payload: row.get(13),
                    created_at: row.get(14),
                },
                publish_attempts: row.get::<_, i64>(15).max(0) as u32,
                published_at: row.get(16),
                last_error: row.get(17),
            })
            .collect())
    }

    pub async fn mark_outbox_published(&self, id: Uuid) -> AppResult<()> {
        let client = self.client().await?;
        client
            .execute(
                "UPDATE outbox_events
                 SET published_at = $1, publish_attempts = publish_attempts + 1, last_error = NULL
                 WHERE id = $2",
                &[&Utc::now(), &id],
            )
            .await
            .map_err(|e| AppError::Storage(format!("mark outbox published failed: {e}")))?;
        Ok(())
    }

    pub async fn record_outbox_failure(
        &self,
        id: Uuid,
        error: &str,
        max_retries: u32,
    ) -> AppResult<()> {
        let client = self.client().await?;
        client
            .execute(
                "UPDATE outbox_events
                 SET publish_attempts = publish_attempts + 1,
                     last_error = CASE WHEN publish_attempts + 1 >= $1 THEN $2 ELSE $2 END
                 WHERE id = $3",
                &[&(max_retries as i64), &error, &id],
            )
            .await
            .map_err(|e| AppError::Storage(format!("record outbox failure failed: {e}")))?;
        Ok(())
    }

    pub async fn outbox_stats(&self) -> AppResult<OutboxStats> {
        let client = self.client().await?;
        let row = client
            .query_one(
                "SELECT
                    COALESCE(COUNT(*) FILTER (WHERE published_at IS NULL), 0),
                    COALESCE(COUNT(*) FILTER (WHERE published_at IS NOT NULL), 0),
                    COALESCE(COUNT(*) FILTER (WHERE published_at IS NULL AND publish_attempts > 0), 0)
                 FROM outbox_events",
                &[],
            )
            .await
            .map_err(|e| AppError::Storage(format!("outbox stats failed: {e}")))?;
        Ok(OutboxStats {
            pending: row.get::<_, i64>(0),
            published: row.get::<_, i64>(1),
            failed: row.get::<_, i64>(2),
        })
    }

    pub async fn latest_runtime_events(&self, limit: usize) -> AppResult<Vec<Value>> {
        let client = self.client().await?;
        let rows = client
            .query(
                "SELECT id, event_type, payload_json, created_at FROM runtime_events ORDER BY created_at DESC LIMIT $1",
                &[&(limit as i64)],
            )
            .await
            .map_err(|e| AppError::Storage(format!("latest runtime events failed: {e}")))?;
        Ok(rows
            .into_iter()
            .map(|row| {
                serde_json::json!({
                    "id": row.get::<_, Uuid>(0).to_string(),
                    "event_type": row.get::<_, String>(1),
                    "payload": row.get::<_, Value>(2),
                    "created_at": row.get::<_, DateTime<Utc>>(3).to_rfc3339(),
                })
            })
            .collect())
    }

    pub async fn get_plan_json(&self, plan_id: &str) -> AppResult<Option<Value>> {
        let client = self.client().await?;
        client
            .query_opt(
                "SELECT plan_json FROM plans WHERE plan_id = $1",
                &[&plan_id],
            )
            .await
            .map(|row| row.map(|r| r.get::<_, Value>(0)))
            .map_err(|e| AppError::Storage(format!("get plan json failed: {e}")))
    }

    pub async fn get_task_json(&self, task_id: &str) -> AppResult<Option<Value>> {
        let client = self.client().await?;
        client
            .query_opt(
                "SELECT task_json FROM tasks WHERE task_id = $1",
                &[&task_id],
            )
            .await
            .map(|row| row.map(|r| r.get::<_, Value>(0)))
            .map_err(|e| AppError::Storage(format!("get task json failed: {e}")))
    }

    pub async fn latest_plan_snapshot(&self) -> AppResult<Option<Value>> {
        let client = self.client().await?;
        let Some(plan_row) = client
            .query_opt(
                "SELECT plan_id, goal, plan_json, status, updated_at
                 FROM plans
                 ORDER BY updated_at DESC
                 LIMIT 1",
                &[],
            )
            .await
            .map_err(|e| AppError::Storage(format!("latest plan snapshot failed: {e}")))?
        else {
            return Ok(None);
        };

        let plan_id: String = plan_row.get(0);
        let tasks = client
            .query(
                "SELECT task_id, task_json, state, assigned_subagent, updated_at
                 FROM tasks
                 WHERE plan_id = $1
                 ORDER BY created_at ASC, task_id ASC",
                &[&plan_id],
            )
            .await
            .map_err(|e| AppError::Storage(format!("latest plan tasks failed: {e}")))?;

        let task_json = tasks
            .into_iter()
            .map(|row| {
                let mut task = row.get::<_, Value>(1);
                if let Some(obj) = task.as_object_mut() {
                    obj.insert("id".to_string(), Value::String(row.get::<_, String>(0)));
                    obj.insert("state".to_string(), Value::String(row.get::<_, String>(2)));
                    obj.insert(
                        "assigned_to".to_string(),
                        row.get::<_, Option<String>>(3)
                            .map(Value::String)
                            .unwrap_or(Value::Null),
                    );
                    obj.insert(
                        "updated_at".to_string(),
                        Value::String(row.get::<_, DateTime<Utc>>(4).to_rfc3339()),
                    );
                }
                task
            })
            .collect::<Vec<_>>();

        Ok(Some(serde_json::json!({
            "plan_id": plan_row.get::<_, String>(0),
            "goal": plan_row.get::<_, String>(1),
            "plan": plan_row.get::<_, Value>(2),
            "status": plan_row.get::<_, String>(3),
            "updated_at": plan_row.get::<_, DateTime<Utc>>(4).to_rfc3339(),
            "tasks": task_json,
        })))
    }

    pub async fn insert_config_snapshot(
        &self,
        snapshot_type: &str,
        source_path: Option<&str>,
        payload_json: &Value,
    ) -> AppResult<()> {
        let client = self.client().await?;
        client
            .execute(
                "INSERT INTO config_snapshots(snapshot_type, source_path, payload_json, created_at)
                 VALUES ($1, $2, $3, $4)",
                &[&snapshot_type, &source_path, payload_json, &Utc::now()],
            )
            .await
            .map_err(|e| AppError::Storage(format!("insert config snapshot failed: {e}")))?;
        Ok(())
    }

    pub async fn latest_config_snapshot(&self, snapshot_type: &str) -> AppResult<Option<Value>> {
        let client = self.client().await?;
        client
            .query_opt(
                "SELECT payload_json
                 FROM config_snapshots
                 WHERE snapshot_type = $1
                 ORDER BY created_at DESC
                 LIMIT 1",
                &[&snapshot_type],
            )
            .await
            .map(|row| row.map(|r| r.get::<_, Value>(0)))
            .map_err(|e| AppError::Storage(format!("latest config snapshot failed: {e}")))
    }

    pub async fn total_estimated_cost(&self) -> AppResult<f64> {
        let client = self.client().await?;
        client
            .query_one(
                "SELECT COALESCE(SUM(estimated_cost_usd), 0) FROM model_usages",
                &[],
            )
            .await
            .map(|row| row.get::<_, f64>(0))
            .map_err(|e| AppError::Storage(format!("total estimated cost failed: {e}")))
    }

    pub async fn reset_memory_data(&self) -> AppResult<()> {
        let client = self.client().await?;
        client
            .batch_execute(
                "
                TRUNCATE TABLE
                    task_reviews,
                    task_artifacts,
                    task_attempts,
                    tasks,
                    plans,
                    subagent_heartbeats,
                    subagent_states,
                    runtime_events,
                    outbox_events,
                    outbound_messages,
                    tool_traces,
                    model_usages,
                    reminders,
                    processed_event_dedup,
                    memory_docs,
                    facts,
                    summaries,
                    turns,
                    conversations,
                    inbound_events,
                    jobs
                RESTART IDENTITY CASCADE;
                ",
            )
            .await
            .map_err(|e| AppError::Storage(format!("reset memory data failed: {e}")))?;
        Ok(())
    }

    pub async fn export_conversation_trace(
        &self,
        conversation_id: i64,
    ) -> AppResult<ConversationTraceBundle> {
        let turns = self.recent_turns(conversation_id, 500).await?;
        let client = self.client().await?;

        let outbound_messages = self
            .query_json_objects(
                &client,
                "SELECT jsonb_build_object(
                    'trace_id', trace_id,
                    'recipient', recipient,
                    'content', content,
                    'status', status,
                    'created_at', created_at
                )
                FROM outbound_messages
                WHERE conversation_id = $1
                ORDER BY id ASC",
                &[&conversation_id],
            )
            .await?;
        let model_usages = self
            .query_json_objects(
                &client,
                "SELECT jsonb_build_object(
                    'trace_id', trace_id,
                    'model', model,
                    'prompt_tokens', prompt_tokens,
                    'completion_tokens', completion_tokens,
                    'estimated_cost_usd', estimated_cost_usd,
                    'latency_ms', latency_ms,
                    'created_at', created_at
                )
                FROM model_usages
                ORDER BY id ASC",
                &[],
            )
            .await?;
        let plans = self
            .query_json_objects(
                &client,
                "SELECT jsonb_build_object(
                    'plan_id', plan_id,
                    'conversation_id', conversation_id,
                    'goal', goal,
                    'plan_json', plan_json,
                    'status', status,
                    'created_at', created_at,
                    'updated_at', updated_at
                )
                FROM plans
                WHERE conversation_id = $1
                ORDER BY created_at ASC",
                &[&conversation_id],
            )
            .await?;
        let tasks = self
            .query_json_objects(
                &client,
                "SELECT jsonb_build_object(
                    'task_id', task_id,
                    'plan_id', plan_id,
                    'task_json', task_json,
                    'state', state,
                    'assigned_subagent', assigned_subagent,
                    'created_at', created_at,
                    'updated_at', updated_at
                )
                FROM tasks
                WHERE plan_id IN (SELECT plan_id FROM plans WHERE conversation_id = $1)
                ORDER BY created_at ASC",
                &[&conversation_id],
            )
            .await?;
        let runtime_events = self
            .query_json_objects(
                &client,
                "SELECT jsonb_build_object(
                    'id', id,
                    'event_type', event_type,
                    'payload_json', payload_json,
                    'created_at', created_at
                )
                FROM runtime_events
                ORDER BY created_at ASC",
                &[],
            )
            .await?;

        Ok(ConversationTraceBundle {
            conversation_id,
            turns,
            outbound_messages,
            model_usages,
            plans,
            tasks,
            runtime_events,
        })
    }

    async fn query_json_objects(
        &self,
        client: &Client,
        sql: &str,
        params: &[&(dyn ToSql + Sync)],
    ) -> AppResult<Vec<Value>> {
        let rows = client
            .query(sql, params)
            .await
            .map_err(|e| AppError::Storage(format!("json query failed: {e}")))?;
        Ok(rows.into_iter().map(|row| row.get::<_, Value>(0)).collect())
    }

    async fn count_scalar(&self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> AppResult<i64> {
        let client = self.client().await?;
        client
            .query_one(sql, params)
            .await
            .map(|row| row.get::<_, i64>(0))
            .map_err(|e| AppError::Storage(format!("count query failed: {e}")))
    }
}

async fn save_or_merge_brain_candidate(
    tx: &Transaction<'_>,
    candidate: &BrainWriteCandidate,
) -> AppResult<()> {
    let existing = tx
        .query_opt(
            "SELECT id, scope_kind, user_id, conversation_id, memory_kind, memory_key, subject,
                    what_value, why_value, where_context, learned_value, provenance_json,
                    confidence, status, superseded_by, source_turn_id, created_at, updated_at
             FROM brain_memories
             WHERE status = 'active'
               AND scope_kind = $1
               AND memory_key = $2
               AND (($3::TEXT IS NULL AND user_id IS NULL) OR user_id = $3)
               AND (($4::BIGINT IS NULL AND conversation_id IS NULL) OR conversation_id = $4)
             ORDER BY id DESC
             LIMIT 1",
            &[
                &candidate.scope_kind.as_str(),
                &candidate.memory_key,
                &candidate.user_id,
                &candidate.conversation_id,
            ],
        )
        .await
        .map_err(|e| AppError::Storage(format!("load existing brain memory failed: {e}")))?;

    let Some(existing) = existing.map(scan_brain_memory_row).transpose()? else {
        insert_brain_candidate(tx, candidate).await?;
        return Ok(());
    };

    if existing.same_content(candidate) {
        let merged_provenance = existing.provenance.merge(&candidate.provenance);
        let merged_confidence = existing.confidence.max(candidate.confidence);
        let search_text = candidate.search_text();
        let merged_source_turn_id = candidate.source_turn_id.or(existing.source_turn_id);
        let merged_provenance_json = serde_json::to_value(&merged_provenance).map_err(|e| {
            AppError::Internal(format!("serialize merged brain provenance failed: {e}"))
        })?;
        tx.execute(
            "UPDATE brain_memories
             SET subject = $2,
                 what_value = $3,
                 why_value = $4,
                 where_context = $5,
                 learned_value = $6,
                 provenance_json = $7,
                 confidence = $8,
                 source_turn_id = $9,
                 search_text = $10,
                 updated_at = $11
             WHERE id = $1",
            &[
                &existing.id,
                &candidate.subject,
                &candidate.what_value,
                &candidate.why_value,
                &candidate.where_context,
                &candidate.learned_value,
                &merged_provenance_json,
                &merged_confidence,
                &merged_source_turn_id,
                &search_text,
                &Utc::now(),
            ],
        )
        .await
        .map_err(|e| AppError::Storage(format!("update merged brain memory failed: {e}")))?;
        return Ok(());
    }

    if candidate_should_supersede(&existing, candidate) {
        let new_id = insert_brain_candidate(tx, candidate).await?;
        tx.execute(
            "UPDATE brain_memories
             SET status = 'superseded',
                 superseded_by = $2,
                 updated_at = $3
             WHERE id = $1",
            &[&existing.id, &new_id, &Utc::now()],
        )
        .await
        .map_err(|e| AppError::Storage(format!("supersede brain memory failed: {e}")))?;
    }

    Ok(())
}

async fn insert_brain_candidate(
    tx: &Transaction<'_>,
    candidate: &BrainWriteCandidate,
) -> AppResult<i64> {
    let now = Utc::now();
    let provenance_json = serde_json::to_value(&candidate.provenance)
        .map_err(|e| AppError::Internal(format!("serialize brain provenance failed: {e}")))?;
    let search_text = build_brain_search_text(
        &candidate.subject,
        &candidate.what_value,
        candidate.why_value.as_deref(),
        candidate.where_context.as_deref(),
        candidate.learned_value.as_deref(),
    );
    let row = tx
        .query_one(
            "INSERT INTO brain_memories(
                scope_kind, user_id, conversation_id, memory_kind, memory_key, subject,
                what_value, why_value, where_context, learned_value, provenance_json,
                confidence, status, superseded_by, source_turn_id, search_text, created_at, updated_at
             )
             VALUES (
                $1, $2, $3, $4, $5, $6,
                $7, $8, $9, $10, $11,
                $12, 'active', NULL, $13, $14, $15, $15
             )
             RETURNING id",
            &[
                &candidate.scope_kind.as_str(),
                &candidate.user_id,
                &candidate.conversation_id,
                &candidate.memory_kind.as_str(),
                &candidate.memory_key,
                &candidate.subject,
                &candidate.what_value,
                &candidate.why_value,
                &candidate.where_context,
                &candidate.learned_value,
                &provenance_json,
                &candidate.confidence,
                &candidate.source_turn_id,
                &search_text,
                &now,
            ],
        )
        .await
        .map_err(|e| AppError::Storage(format!("insert brain memory failed: {e}")))?;
    Ok(row.get(0))
}

fn scan_brain_memory_row(row: tokio_postgres::Row) -> AppResult<BrainMemory> {
    let provenance_json: Value = row.get(11);
    let provenance: BrainMemoryProvenance = serde_json::from_value(provenance_json)
        .map_err(|e| AppError::Storage(format!("decode brain provenance failed: {e}")))?;

    Ok(BrainMemory {
        id: row.get(0),
        scope_kind: BrainScopeKind::from_db(row.get::<_, String>(1).as_str()),
        user_id: row.get(2),
        conversation_id: row.get(3),
        memory_kind: BrainMemoryKind::from_db(row.get::<_, String>(4).as_str()),
        memory_key: row.get(5),
        subject: row.get(6),
        what_value: row.get(7),
        why_value: row.get(8),
        where_context: row.get(9),
        learned_value: row.get(10),
        provenance,
        confidence: row.get(12),
        status: BrainMemoryStatus::from_db(row.get::<_, String>(13).as_str()),
        superseded_by: row.get(14),
        source_turn_id: row.get(15),
        created_at: row.get(16),
        updated_at: row.get(17),
    })
}

fn validate_schema_name(value: &str) -> AppResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(AppError::Config(
            "postgres schema cannot be empty".to_string(),
        ));
    }
    let mut chars = trimmed.chars();
    let Some(first) = chars.next() else {
        return Err(AppError::Config(
            "postgres schema cannot be empty".to_string(),
        ));
    };
    if !(first == '_' || first.is_ascii_alphabetic()) {
        return Err(AppError::Config(format!(
            "invalid postgres schema `{trimmed}`: must start with a letter or underscore"
        )));
    }
    if !chars.all(|c| c == '_' || c.is_ascii_alphanumeric()) {
        return Err(AppError::Config(format!(
            "invalid postgres schema `{trimmed}`: only letters, digits, and underscores are allowed"
        )));
    }
    Ok(trimmed.to_string())
}
