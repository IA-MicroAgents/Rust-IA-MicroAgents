use std::time::Duration;

use serde_json::Value;
use tokio::time::{interval, MissedTickBehavior};
use tracing::{debug, error, info, warn};

use crate::{
    errors::AppResult,
    memory::BrainWriteCandidate,
    storage::{BusEventEnvelope, Store},
};

const EVENTS_STREAM: &str = "events";
const MEMORY_STREAM: &str = "memory";
const DASHBOARD_GROUP: &str = "dashboard-projection";
const MEMORY_GROUP: &str = "memory-workers";

#[derive(Clone)]
pub struct OutboxPublisher {
    store: Store,
}

impl OutboxPublisher {
    pub fn new(store: Store) -> Self {
        Self { store }
    }

    pub fn spawn(self) {
        tokio::spawn(async move {
            let mut ticker = interval(Duration::from_millis(
                self.store.bus_config().outbox_poll_ms,
            ));
            ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
            let mut error_backoff_ms = 250_u64;
            loop {
                ticker.tick().await;
                match self.tick_once().await {
                    Ok(_) => error_backoff_ms = 250,
                    Err(err) => {
                        error!(error = %err, backoff_ms = error_backoff_ms, "outbox publisher tick failed");
                        tokio::time::sleep(Duration::from_millis(error_backoff_ms)).await;
                        error_backoff_ms = (error_backoff_ms * 2).min(5_000);
                    }
                }
            }
        });
    }

    async fn tick_once(&self) -> AppResult<()> {
        let pending = self
            .store
            .fetch_pending_outbox_events(self.store.bus_config().outbox_publish_batch)
            .await?;
        if pending.is_empty() {
            return Ok(());
        }

        debug!(pending = pending.len(), "publishing outbox batch");
        for event in pending {
            match self
                .store
                .publish_to_stream(&event.envelope.stream_key, &event.envelope)
                .await
            {
                Ok(redis_id) => {
                    self.store.mark_outbox_published(event.envelope.id).await?;
                    debug!(
                        outbox_event_id = %event.envelope.id,
                        stream = %event.envelope.stream_key,
                        redis_id = %redis_id,
                        "outbox event published to stream"
                    );
                }
                Err(err) => {
                    self.store
                        .record_outbox_failure(
                            event.envelope.id,
                            &err.to_string(),
                            self.store.bus_config().outbox_max_retries,
                        )
                        .await?;
                    warn!(
                        outbox_event_id = %event.envelope.id,
                        stream = %event.envelope.stream_key,
                        error = %err,
                        "outbox publish failed"
                    );
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct DashboardProjectionConsumer {
    store: Store,
    consumer_name: String,
}

impl DashboardProjectionConsumer {
    pub fn new(store: Store) -> Self {
        let consumer_name = format!("{}-dashboard", store.bus_config().consumer_name);
        Self {
            store,
            consumer_name,
        }
    }

    pub fn spawn(self) {
        tokio::spawn(async move {
            if let Err(err) = self
                .store
                .ensure_stream_group(EVENTS_STREAM, DASHBOARD_GROUP)
                .await
            {
                error!(error = %err, "failed to ensure dashboard projection stream group");
                return;
            }

            let mut error_backoff_ms = 500_u64;
            loop {
                match self.tick_once().await {
                    Ok(_) => error_backoff_ms = 500,
                    Err(err) => {
                        error!(
                            error = %err,
                            backoff_ms = error_backoff_ms,
                            "dashboard projection consumer failed"
                        );
                        tokio::time::sleep(Duration::from_millis(error_backoff_ms)).await;
                        error_backoff_ms = (error_backoff_ms * 2).min(5_000);
                    }
                }
            }
        });
    }

    async fn tick_once(&self) -> AppResult<()> {
        let mut entries = self
            .store
            .claim_stale_stream_events(EVENTS_STREAM, DASHBOARD_GROUP, &self.consumer_name, 64)
            .await?;
        if entries.is_empty() {
            entries = self
                .store
                .read_stream_group(
                    EVENTS_STREAM,
                    DASHBOARD_GROUP,
                    &self.consumer_name,
                    64,
                    1_000,
                )
                .await?;
        }
        if entries.is_empty() {
            return Ok(());
        }
        for (redis_id, envelope) in entries {
            let projection = serde_json::json!({
                "id": envelope.id.to_string(),
                "event_type": envelope.event_kind,
                "payload": envelope.payload,
                "created_at": envelope.created_at.to_rfc3339(),
            });
            match self
                .store
                .update_runtime_events_projection(&projection, 300)
                .await
            {
                Ok(_) => {
                    let _ = self
                        .store
                        .clear_stream_failure(DASHBOARD_GROUP, &envelope.id.to_string())
                        .await;
                    let _ = self
                        .store
                        .mark_stream_processed_once(DASHBOARD_GROUP, &envelope.id.to_string())
                        .await?;
                    self.store
                        .ack_stream_event(EVENTS_STREAM, DASHBOARD_GROUP, &redis_id)
                        .await?;
                }
                Err(err) => {
                    handle_consumer_failure(
                        &self.store,
                        EVENTS_STREAM,
                        DASHBOARD_GROUP,
                        &redis_id,
                        &envelope,
                        &err.to_string(),
                    )
                    .await?;
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct MemoryConsumer {
    store: Store,
    consumer_name: String,
}

impl MemoryConsumer {
    pub fn new(store: Store, suffix: usize) -> Self {
        let consumer_name = format!("{}-memory-{suffix}", store.bus_config().consumer_name);
        Self {
            store,
            consumer_name,
        }
    }

    pub fn spawn(self) {
        tokio::spawn(async move {
            if let Err(err) = self
                .store
                .ensure_stream_group(MEMORY_STREAM, MEMORY_GROUP)
                .await
            {
                error!(error = %err, "failed to ensure memory stream group");
                return;
            }

            let mut error_backoff_ms = 500_u64;
            loop {
                match self.tick_once().await {
                    Ok(_) => error_backoff_ms = 500,
                    Err(err) => {
                        error!(
                            error = %err,
                            backoff_ms = error_backoff_ms,
                            "memory consumer tick failed"
                        );
                        tokio::time::sleep(Duration::from_millis(error_backoff_ms)).await;
                        error_backoff_ms = (error_backoff_ms * 2).min(5_000);
                    }
                }
            }
        });
    }

    async fn tick_once(&self) -> AppResult<()> {
        let mut entries = self
            .store
            .claim_stale_stream_events(MEMORY_STREAM, MEMORY_GROUP, &self.consumer_name, 32)
            .await?;
        if entries.is_empty() {
            entries = self
                .store
                .read_stream_group(MEMORY_STREAM, MEMORY_GROUP, &self.consumer_name, 32, 1_000)
                .await?;
        }
        if entries.is_empty() {
            return Ok(());
        }

        for (redis_id, envelope) in entries {
            match self.handle_memory_event(&envelope).await {
                Ok(_) => {
                    let _ = self
                        .store
                        .clear_stream_failure(MEMORY_GROUP, &envelope.id.to_string())
                        .await;
                    let _ = self
                        .store
                        .mark_stream_processed_once(MEMORY_GROUP, &envelope.id.to_string())
                        .await?;
                    self.store
                        .ack_stream_event(MEMORY_STREAM, MEMORY_GROUP, &redis_id)
                        .await?;
                }
                Err(err) => {
                    handle_consumer_failure(
                        &self.store,
                        MEMORY_STREAM,
                        MEMORY_GROUP,
                        &redis_id,
                        &envelope,
                        &err.to_string(),
                    )
                    .await?;
                }
            }
        }
        Ok(())
    }

    async fn handle_memory_event(&self, envelope: &BusEventEnvelope) -> AppResult<()> {
        match envelope.event_kind.as_str() {
            "memory.summary.write" => {
                let conversation_id = required_i64(&envelope.payload, "conversation_id")?;
                let summary = required_str(&envelope.payload, "summary")?;
                self.store.write_summary(conversation_id, summary).await?;
            }
            "memory.fact.write" => {
                let conversation_id = envelope
                    .payload
                    .get("conversation_id")
                    .and_then(Value::as_i64);
                let fact_key = required_str(&envelope.payload, "fact_key")?;
                let fact_value = required_str(&envelope.payload, "fact_value")?;
                let confidence = envelope
                    .payload
                    .get("confidence")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.7);
                let source_turn_id = envelope
                    .payload
                    .get("source_turn_id")
                    .and_then(Value::as_i64);
                self.store
                    .write_fact(
                        conversation_id,
                        fact_key,
                        fact_value,
                        confidence,
                        source_turn_id,
                    )
                    .await?;
            }
            "memory.brain.write" => {
                let payload = envelope
                    .payload
                    .get("candidates")
                    .cloned()
                    .unwrap_or_else(|| Value::Array(Vec::new()));
                let candidates: Vec<BrainWriteCandidate> = serde_json::from_value(payload)
                    .map_err(|e| {
                        crate::errors::AppError::Storage(format!(
                            "decode brain memory candidates failed: {e}"
                        ))
                    })?;
                self.store
                    .save_or_merge_brain_candidates(&candidates)
                    .await?;
            }
            other => {
                debug!(event_kind = %other, "memory consumer ignored unsupported event");
            }
        }
        Ok(())
    }
}

pub fn spawn_redis_bus_workers(store: Store) {
    if !store.bus_enabled() {
        return;
    }
    OutboxPublisher::new(store.clone()).spawn();
    DashboardProjectionConsumer::new(store.clone()).spawn();
    for idx in 0..store.bus_config().memory_consumer_concurrency.max(1) {
        MemoryConsumer::new(store.clone(), idx).spawn();
    }
    info!(
        stream_prefix = %store.bus_config().stream_prefix,
        memory_workers = store.bus_config().memory_consumer_concurrency.max(1),
        "redis bus workers started"
    );
}

async fn handle_consumer_failure(
    store: &Store,
    stream_key: &str,
    group: &str,
    redis_id: &str,
    envelope: &BusEventEnvelope,
    error_message: &str,
) -> AppResult<()> {
    let failures = store
        .increment_stream_failure(group, &envelope.id.to_string())
        .await?;
    warn!(
        stream = %stream_key,
        group = %group,
        outbox_event_id = %envelope.id,
        failures,
        error = %error_message,
        "redis stream consumer failed to process event"
    );

    if failures >= u64::from(store.bus_config().outbox_max_retries) {
        let dlq_stream = format!("{stream_key}-dlq");
        let mut payload = envelope.payload.clone();
        if let Value::Object(map) = &mut payload {
            map.insert(
                "dlq_error".to_string(),
                Value::String(error_message.to_string()),
            );
            map.insert(
                "dlq_original_stream".to_string(),
                Value::String(stream_key.to_string()),
            );
        }
        let dlq_envelope = BusEventEnvelope {
            payload,
            stream_key: dlq_stream.clone(),
            ..envelope.clone()
        };
        let _ = store.publish_to_stream(&dlq_stream, &dlq_envelope).await?;
        let _ = store
            .clear_stream_failure(group, &envelope.id.to_string())
            .await;
        let _ = store
            .mark_stream_processed_once(group, &envelope.id.to_string())
            .await?;
        store.ack_stream_event(stream_key, group, redis_id).await?;
        warn!(
            stream = %stream_key,
            dlq_stream = %dlq_stream,
            outbox_event_id = %envelope.id,
            "moved stream event to DLQ after repeated failures"
        );
    }
    Ok(())
}

fn required_i64(payload: &Value, key: &str) -> AppResult<i64> {
    payload
        .get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| crate::errors::AppError::Storage(format!("memory event missing {key}")))
}

fn required_str<'a>(payload: &'a Value, key: &str) -> AppResult<&'a str> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| crate::errors::AppError::Storage(format!("memory event missing {key}")))
}
