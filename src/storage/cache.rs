use std::{
    future::Future,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use parking_lot::RwLock;
use redis::streams::{
    StreamAutoClaimOptions, StreamAutoClaimReply, StreamId, StreamReadOptions, StreamReadReply,
};
use redis::{aio::MultiplexedConnection, AsyncCommands};
use serde::{de::DeserializeOwned, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    config::CacheConfig,
    errors::{AppError, AppResult},
    storage::{BusEventEnvelope, StreamPendingStats},
};

#[derive(Debug, Clone, Copy)]
pub enum CacheScope {
    Default,
    Dashboard,
    Memory,
}

#[derive(Clone)]
pub struct CacheLayer {
    backend: Arc<RedisCache>,
}

struct RedisCache {
    client: redis::Client,
    connections: RwLock<Vec<MultiplexedConnection>>,
    next_index: AtomicUsize,
    namespace: String,
    default_ttl_secs: usize,
    dashboard_ttl_secs: usize,
    memory_ttl_secs: usize,
}

impl CacheLayer {
    pub async fn from_config(config: &CacheConfig) -> AppResult<Self> {
        let client = redis::Client::open(config.redis_url.as_str())
            .map_err(|e| AppError::Storage(format!("redis client init failed: {e}")))?;

        let pool_size = config.pool_max.max(1);
        let mut connections = Vec::with_capacity(pool_size);
        for _ in 0..pool_size {
            let connection = client
                .get_multiplexed_async_connection()
                .await
                .map_err(|e| AppError::Storage(format!("redis connection failed: {e}")))?;
            connections.push(connection);
        }

        Ok(Self {
            backend: Arc::new(RedisCache {
                client,
                connections: RwLock::new(connections),
                next_index: AtomicUsize::new(0),
                namespace: config.namespace.clone(),
                default_ttl_secs: config.default_ttl_secs as usize,
                dashboard_ttl_secs: config.dashboard_ttl_secs as usize,
                memory_ttl_secs: config.memory_ttl_secs as usize,
            }),
        })
    }

    pub fn is_enabled(&self) -> bool {
        true
    }

    pub fn key_suffix(input: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(input.as_bytes());
        format!("{:x}", hasher.finalize())
    }

    pub async fn get_json<T: DeserializeOwned>(&self, key: &str) -> AppResult<Option<T>> {
        let Some(raw) = self.get_string(key).await? else {
            return Ok(None);
        };
        serde_json::from_str(&raw)
            .map(Some)
            .map_err(|e| AppError::Storage(format!("redis json decode failed for {key}: {e}")))
    }

    pub async fn set_json<T: Serialize>(
        &self,
        key: &str,
        scope: CacheScope,
        value: &T,
    ) -> AppResult<()> {
        let raw = serde_json::to_string(value)
            .map_err(|e| AppError::Storage(format!("redis json encode failed for {key}: {e}")))?;
        self.set_string(key, scope, &raw).await
    }

    pub async fn get_string(&self, key: &str) -> AppResult<Option<String>> {
        let redis_key = self.backend.full_key(key);
        let (slot, mut conn) = self.backend.connection_with_slot();
        match conn.get(redis_key.clone()).await {
            Ok(value) => Ok(value),
            Err(err) if is_retryable_transport_error(&err) => {
                self.backend.reconnect_slot(slot).await?;
                let (_slot, mut retry_conn) = self.backend.connection_with_preferred_slot(slot);
                retry_conn.get(redis_key).await.map_err(|retry_err| {
                    AppError::Storage(format!("redis GET failed for {key}: {retry_err}"))
                })
            }
            Err(err) => Err(AppError::Storage(format!(
                "redis GET failed for {key}: {err}"
            ))),
        }
    }

    pub async fn set_string(&self, key: &str, scope: CacheScope, value: &str) -> AppResult<()> {
        let ttl = self.backend.ttl_for(scope);
        let redis_key = self.backend.full_key(key);
        let (slot, mut conn) = self.backend.connection_with_slot();
        match conn.set_ex(redis_key.clone(), value, ttl as u64).await {
            Ok(()) => Ok(()),
            Err(err) if is_retryable_transport_error(&err) => {
                self.backend.reconnect_slot(slot).await?;
                let (_slot, mut retry_conn) = self.backend.connection_with_preferred_slot(slot);
                let _: () = retry_conn
                    .set_ex(redis_key, value, ttl as u64)
                    .await
                    .map_err(|retry_err| {
                        AppError::Storage(format!("redis SETEX failed for {key}: {retry_err}"))
                    })?;
                Ok(())
            }
            Err(err) => Err(AppError::Storage(format!(
                "redis SETEX failed for {key}: {err}"
            ))),
        }
    }

    pub async fn delete(&self, key: &str) -> AppResult<()> {
        let mut conn = self.backend.connection();
        let _: usize = conn
            .del(self.backend.full_key(key))
            .await
            .map_err(|e| AppError::Storage(format!("redis DEL failed for {key}: {e}")))?;
        Ok(())
    }

    pub async fn delete_prefix(&self, prefix: &str) -> AppResult<()> {
        let full_prefix = self.backend.full_key(prefix);
        let pattern = format!("{}*", full_prefix);
        let mut cursor: u64 = 0;
        loop {
            let mut conn = self.backend.connection();
            let (next, keys): (u64, Vec<String>) = redis::cmd("SCAN")
                .cursor_arg(cursor)
                .arg("MATCH")
                .arg(&pattern)
                .arg("COUNT")
                .arg(256)
                .query_async(&mut conn)
                .await
                .map_err(|e| AppError::Storage(format!("redis SCAN failed for {prefix}: {e}")))?;
            if !keys.is_empty() {
                let _: usize = conn.del(keys).await.map_err(|e| {
                    AppError::Storage(format!("redis DEL prefix failed for {prefix}: {e}"))
                })?;
            }
            if next == 0 {
                break;
            }
            cursor = next;
        }
        Ok(())
    }

    pub async fn clear_namespace(&self) -> AppResult<()> {
        let pattern = format!("{}:*", self.backend.namespace);
        let mut cursor: u64 = 0;
        loop {
            let mut conn = self.backend.connection();
            let (next, keys): (u64, Vec<String>) = redis::cmd("SCAN")
                .cursor_arg(cursor)
                .arg("MATCH")
                .arg(&pattern)
                .arg("COUNT")
                .arg(512)
                .query_async(&mut conn)
                .await
                .map_err(|e| {
                    AppError::Storage(format!("redis SCAN failed for namespace clear: {e}"))
                })?;
            if !keys.is_empty() {
                let _: usize = conn.del(keys).await.map_err(|e| {
                    AppError::Storage(format!("redis DEL namespace clear failed: {e}"))
                })?;
            }
            if next == 0 {
                break;
            }
            cursor = next;
        }
        Ok(())
    }

    pub async fn cached_json<T, F, Fut>(
        &self,
        key: &str,
        scope: CacheScope,
        loader: F,
    ) -> AppResult<T>
    where
        T: Serialize + DeserializeOwned + Send,
        F: FnOnce() -> Fut,
        Fut: Future<Output = AppResult<T>> + Send,
    {
        if let Some(value) = self.get_json(key).await? {
            return Ok(value);
        }
        let value = loader().await?;
        let _ = self.set_json(key, scope, &value).await;
        Ok(value)
    }

    pub async fn cached_string<F, Fut>(
        &self,
        key: &str,
        scope: CacheScope,
        loader: F,
    ) -> AppResult<String>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = AppResult<String>> + Send,
    {
        if let Some(value) = self.get_string(key).await? {
            return Ok(value);
        }
        let value = loader().await?;
        let _ = self.set_string(key, scope, &value).await;
        Ok(value)
    }

    pub fn stream_name(&self, stream_prefix: &str, stream_key: &str) -> String {
        self.backend
            .full_key(&format!("stream:{stream_prefix}:{stream_key}"))
    }

    pub async fn ensure_stream_group(
        &self,
        stream_prefix: &str,
        stream_key: &str,
        group: &str,
    ) -> AppResult<()> {
        let stream = self.stream_name(stream_prefix, stream_key);
        let (slot, mut conn) = self.backend.connection_with_slot();
        let result: redis::RedisResult<()> = redis::cmd("XGROUP")
            .arg("CREATE")
            .arg(&stream)
            .arg(group)
            .arg("0")
            .arg("MKSTREAM")
            .query_async(&mut conn)
            .await;
        match result {
            Ok(()) => Ok(()),
            Err(err) if err.to_string().contains("BUSYGROUP") => Ok(()),
            Err(err) if is_retryable_transport_error(&err) => {
                self.backend.reconnect_slot(slot).await?;
                let (_slot, mut retry_conn) = self.backend.connection_with_preferred_slot(slot);
                let retry: redis::RedisResult<()> = redis::cmd("XGROUP")
                    .arg("CREATE")
                    .arg(&stream)
                    .arg(group)
                    .arg("0")
                    .arg("MKSTREAM")
                    .query_async(&mut retry_conn)
                    .await;
                match retry {
                    Ok(()) => Ok(()),
                    Err(retry_err) if retry_err.to_string().contains("BUSYGROUP") => Ok(()),
                    Err(retry_err) => Err(AppError::Storage(format!(
                        "redis XGROUP CREATE failed for {stream}/{group}: {retry_err}"
                    ))),
                }
            }
            Err(err) => Err(AppError::Storage(format!(
                "redis XGROUP CREATE failed for {stream}/{group}: {err}"
            ))),
        }
    }

    pub async fn xadd_bus_event(
        &self,
        stream_prefix: &str,
        stream_key: &str,
        maxlen: usize,
        envelope: &BusEventEnvelope,
    ) -> AppResult<String> {
        let stream = self.stream_name(stream_prefix, stream_key);
        let payload = serde_json::to_string(&envelope.payload)
            .map_err(|e| AppError::Storage(format!("redis XADD payload encode failed: {e}")))?;
        let args = BusStreamArgs::from_envelope(envelope, payload);
        let (slot, mut conn) = self.backend.connection_with_slot();
        match xadd_with_args(&mut conn, &stream, maxlen, &args).await {
            Ok(redis_id) => Ok(redis_id),
            Err(err) if is_retryable_transport_error(&err) => {
                self.backend.reconnect_slot(slot).await?;
                let (_slot, mut retry_conn) = self.backend.connection_with_preferred_slot(slot);
                xadd_with_args(&mut retry_conn, &stream, maxlen, &args)
                    .await
                    .map_err(|retry_err| {
                        AppError::Storage(format!("redis XADD failed for {stream}: {retry_err}"))
                    })
            }
            Err(err) => Err(AppError::Storage(format!(
                "redis XADD failed for {stream}: {err}"
            ))),
        }
    }

    pub async fn xreadgroup_bus_events(
        &self,
        stream_prefix: &str,
        stream_key: &str,
        group: &str,
        consumer: &str,
        count: usize,
        block_ms: usize,
    ) -> AppResult<Vec<(String, BusEventEnvelope)>> {
        let stream = self.stream_name(stream_prefix, stream_key);
        let (slot, mut conn) = self.backend.connection_with_slot();
        let options = StreamReadOptions::default()
            .group(group, consumer)
            .count(count)
            .block(block_ms);
        let reply: StreamReadReply = match conn.xread_options(&[&stream], &[">"], &options).await {
            Ok(reply) => reply,
            Err(err) if is_retryable_transport_error(&err) => {
                self.backend.reconnect_slot(slot).await?;
                let (_slot, mut retry_conn) = self.backend.connection_with_preferred_slot(slot);
                retry_conn
                    .xread_options(&[&stream], &[">"], &options)
                    .await
                    .map_err(|retry_err| {
                        AppError::Storage(format!(
                            "redis XREADGROUP failed for {stream}: {retry_err}"
                        ))
                    })?
            }
            Err(err) => {
                return Err(AppError::Storage(format!(
                    "redis XREADGROUP failed for {stream}: {err}"
                )));
            }
        };

        stream_read_reply_to_bus_events(reply)
    }

    pub async fn xautoclaim_bus_events(
        &self,
        stream_prefix: &str,
        stream_key: &str,
        group: &str,
        consumer: &str,
        min_idle_ms: u64,
        count: usize,
    ) -> AppResult<Vec<(String, BusEventEnvelope)>> {
        let stream = self.stream_name(stream_prefix, stream_key);
        let (slot, mut conn) = self.backend.connection_with_slot();
        let reply: StreamAutoClaimReply = match conn
            .xautoclaim_options(
                &stream,
                group,
                consumer,
                min_idle_ms,
                "0-0",
                StreamAutoClaimOptions::default().count(count),
            )
            .await
        {
            Ok(reply) => reply,
            Err(err) if is_retryable_transport_error(&err) => {
                self.backend.reconnect_slot(slot).await?;
                let (_slot, mut retry_conn) = self.backend.connection_with_preferred_slot(slot);
                retry_conn
                    .xautoclaim_options(
                        &stream,
                        group,
                        consumer,
                        min_idle_ms,
                        "0-0",
                        StreamAutoClaimOptions::default().count(count),
                    )
                    .await
                    .map_err(|retry_err| {
                        AppError::Storage(format!(
                            "redis XAUTOCLAIM failed for {stream}: {retry_err}"
                        ))
                    })?
            }
            Err(err) => {
                return Err(AppError::Storage(format!(
                    "redis XAUTOCLAIM failed for {stream}: {err}"
                )));
            }
        };

        reply
            .claimed
            .into_iter()
            .map(stream_id_to_bus_event)
            .collect()
    }

    pub async fn xack_bus_event(
        &self,
        stream_prefix: &str,
        stream_key: &str,
        group: &str,
        redis_id: &str,
    ) -> AppResult<()> {
        let stream = self.stream_name(stream_prefix, stream_key);
        let (slot, mut conn) = self.backend.connection_with_slot();
        let result: redis::RedisResult<i64> = redis::cmd("XACK")
            .arg(&stream)
            .arg(group)
            .arg(redis_id)
            .query_async(&mut conn)
            .await;
        match result {
            Ok(_) => Ok(()),
            Err(err) if is_retryable_transport_error(&err) => {
                self.backend.reconnect_slot(slot).await?;
                let (_slot, mut retry_conn) = self.backend.connection_with_preferred_slot(slot);
                let _: i64 = redis::cmd("XACK")
                    .arg(&stream)
                    .arg(group)
                    .arg(redis_id)
                    .query_async(&mut retry_conn)
                    .await
                    .map_err(|retry_err| {
                        AppError::Storage(format!("redis XACK failed for {stream}: {retry_err}"))
                    })?;
                Ok(())
            }
            Err(err) => Err(AppError::Storage(format!(
                "redis XACK failed for {stream}: {err}"
            ))),
        }
    }

    pub async fn mark_processed_once(
        &self,
        group: &str,
        outbox_event_id: &str,
        ttl_secs: usize,
    ) -> AppResult<bool> {
        let dedupe_key = self
            .backend
            .full_key(&format!("stream_dedupe:{group}:{outbox_event_id}"));
        let (slot, mut conn) = self.backend.connection_with_slot();
        let result: redis::RedisResult<Option<String>> = redis::cmd("SET")
            .arg(&dedupe_key)
            .arg("1")
            .arg("EX")
            .arg(ttl_secs)
            .arg("NX")
            .query_async(&mut conn)
            .await;
        match result {
            Ok(value) => Ok(value.is_some()),
            Err(err) if is_retryable_transport_error(&err) => {
                self.backend.reconnect_slot(slot).await?;
                let (_slot, mut retry_conn) = self.backend.connection_with_preferred_slot(slot);
                let value: Option<String> = redis::cmd("SET")
                    .arg(&dedupe_key)
                    .arg("1")
                    .arg("EX")
                    .arg(ttl_secs)
                    .arg("NX")
                    .query_async(&mut retry_conn)
                    .await
                    .map_err(|retry_err| {
                        AppError::Storage(format!("redis SET NX failed: {retry_err}"))
                    })?;
                Ok(value.is_some())
            }
            Err(err) => Err(AppError::Storage(format!("redis SET NX failed: {err}"))),
        }
    }

    pub async fn increment_stream_failure(
        &self,
        group: &str,
        outbox_event_id: &str,
        ttl_secs: usize,
    ) -> AppResult<u64> {
        let key = self
            .backend
            .full_key(&format!("stream_fail:{group}:{outbox_event_id}"));
        let (slot, mut conn) = self.backend.connection_with_slot();
        let result: redis::RedisResult<(u64, bool)> = async {
            let count: u64 = conn.incr(&key, 1).await?;
            let expired: bool = conn.expire(&key, ttl_secs as i64).await?;
            Ok((count, expired))
        }
        .await;
        match result {
            Ok((count, _)) => Ok(count),
            Err(err) if is_retryable_transport_error(&err) => {
                self.backend.reconnect_slot(slot).await?;
                let (_slot, mut retry_conn) = self.backend.connection_with_preferred_slot(slot);
                let count: u64 = retry_conn.incr(&key, 1).await.map_err(|retry_err| {
                    AppError::Storage(format!("redis INCR failed: {retry_err}"))
                })?;
                let _: bool =
                    retry_conn
                        .expire(&key, ttl_secs as i64)
                        .await
                        .map_err(|retry_err| {
                            AppError::Storage(format!("redis EXPIRE failed: {retry_err}"))
                        })?;
                Ok(count)
            }
            Err(err) => Err(AppError::Storage(format!("redis INCR failed: {err}"))),
        }
    }

    pub async fn clear_stream_failure(&self, group: &str, outbox_event_id: &str) -> AppResult<()> {
        let key = self
            .backend
            .full_key(&format!("stream_fail:{group}:{outbox_event_id}"));
        let (slot, mut conn) = self.backend.connection_with_slot();
        match conn.del(key.clone()).await {
            Ok::<usize, redis::RedisError>(_) => Ok(()),
            Err(err) if is_retryable_transport_error(&err) => {
                self.backend.reconnect_slot(slot).await?;
                let (_slot, mut retry_conn) = self.backend.connection_with_preferred_slot(slot);
                let _: usize = retry_conn.del(key).await.map_err(|retry_err| {
                    AppError::Storage(format!("redis DEL failure key failed: {retry_err}"))
                })?;
                Ok(())
            }
            Err(err) => Err(AppError::Storage(format!(
                "redis DEL failure key failed: {err}"
            ))),
        }
    }

    pub async fn stream_pending_summary(
        &self,
        stream_prefix: &str,
        stream_key: &str,
        group: &str,
    ) -> AppResult<StreamPendingStats> {
        let stream = self.stream_name(stream_prefix, stream_key);
        let mut conn = self.backend.connection();
        let raw: redis::Value = redis::cmd("XPENDING")
            .arg(&stream)
            .arg(group)
            .query_async(&mut conn)
            .await
            .map_err(|e| AppError::Storage(format!("redis XPENDING failed for {stream}: {e}")))?;
        parse_pending_summary(stream_key, group, raw)
    }
}

impl RedisCache {
    fn connection(&self) -> MultiplexedConnection {
        self.connection_with_slot().1
    }

    fn connection_with_slot(&self) -> (usize, MultiplexedConnection) {
        let connections = self.connections.read();
        let idx = self.next_index.fetch_add(1, Ordering::Relaxed) % connections.len();
        (idx, connections[idx].clone())
    }

    fn connection_with_preferred_slot(
        &self,
        preferred_slot: usize,
    ) -> (usize, MultiplexedConnection) {
        let connections = self.connections.read();
        let idx = preferred_slot.min(connections.len().saturating_sub(1));
        (idx, connections[idx].clone())
    }

    async fn reconnect_slot(&self, slot: usize) -> AppResult<()> {
        let connection = self
            .client
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| AppError::Storage(format!("redis reconnect failed: {e}")))?;
        let mut connections = self.connections.write();
        let idx = slot.min(connections.len().saturating_sub(1));
        connections[idx] = connection;
        Ok(())
    }

    fn full_key(&self, key: &str) -> String {
        format!("{}:{}", self.namespace, key)
    }

    fn ttl_for(&self, scope: CacheScope) -> usize {
        match scope {
            CacheScope::Default => self.default_ttl_secs,
            CacheScope::Dashboard => self.dashboard_ttl_secs,
            CacheScope::Memory => self.memory_ttl_secs,
        }
    }
}

#[derive(Clone)]
struct BusStreamArgs {
    outbox_event_id: String,
    event_kind: String,
    stream_key: String,
    aggregate_id: String,
    conversation_id: i64,
    trace_id: String,
    task_id: String,
    subagent_id: String,
    route_key: String,
    resolved_model: String,
    evidence_count: u32,
    reasoning_tier: String,
    fallback_kind: String,
    created_at: String,
    payload: String,
}

impl BusStreamArgs {
    fn from_envelope(envelope: &BusEventEnvelope, payload: String) -> Self {
        Self {
            outbox_event_id: envelope.id.to_string(),
            event_kind: envelope.event_kind.clone(),
            stream_key: envelope.stream_key.clone(),
            aggregate_id: envelope.aggregate_id.clone().unwrap_or_default(),
            conversation_id: envelope.conversation_id.unwrap_or_default(),
            trace_id: envelope.trace_id.clone().unwrap_or_default(),
            task_id: envelope.task_id.clone().unwrap_or_default(),
            subagent_id: envelope.subagent_id.clone().unwrap_or_default(),
            route_key: envelope.route_key.clone().unwrap_or_default(),
            resolved_model: envelope.resolved_model.clone().unwrap_or_default(),
            evidence_count: envelope.evidence_count.unwrap_or_default(),
            reasoning_tier: envelope.reasoning_tier.clone().unwrap_or_default(),
            fallback_kind: envelope.fallback_kind.clone().unwrap_or_default(),
            created_at: envelope.created_at.to_rfc3339(),
            payload,
        }
    }
}

async fn xadd_with_args(
    conn: &mut MultiplexedConnection,
    stream: &str,
    maxlen: usize,
    args: &BusStreamArgs,
) -> redis::RedisResult<String> {
    redis::cmd("XADD")
        .arg(stream)
        .arg("MAXLEN")
        .arg("~")
        .arg(maxlen)
        .arg("*")
        .arg("outbox_event_id")
        .arg(&args.outbox_event_id)
        .arg("event_kind")
        .arg(&args.event_kind)
        .arg("stream_key")
        .arg(&args.stream_key)
        .arg("aggregate_id")
        .arg(&args.aggregate_id)
        .arg("conversation_id")
        .arg(args.conversation_id)
        .arg("trace_id")
        .arg(&args.trace_id)
        .arg("task_id")
        .arg(&args.task_id)
        .arg("subagent_id")
        .arg(&args.subagent_id)
        .arg("route_key")
        .arg(&args.route_key)
        .arg("resolved_model")
        .arg(&args.resolved_model)
        .arg("evidence_count")
        .arg(args.evidence_count)
        .arg("reasoning_tier")
        .arg(&args.reasoning_tier)
        .arg("fallback_kind")
        .arg(&args.fallback_kind)
        .arg("created_at")
        .arg(&args.created_at)
        .arg("payload")
        .arg(&args.payload)
        .query_async(conn)
        .await
}

fn is_retryable_transport_error(err: &redis::RedisError) -> bool {
    if err.is_connection_dropped() || err.is_timeout() {
        return true;
    }
    let message = err.to_string().to_ascii_lowercase();
    message.contains("broken pipe")
        || message.contains("connection reset")
        || message.contains("connection closed")
        || message.contains("unexpected eof")
        || message.contains("io error")
}

fn field_as_string(map: &std::collections::HashMap<String, redis::Value>, key: &str) -> String {
    optional_field_as_string(map, key).unwrap_or_default()
}

fn optional_field_as_string(
    map: &std::collections::HashMap<String, redis::Value>,
    key: &str,
) -> Option<String> {
    map.get(key)
        .and_then(redis_value_to_string)
        .filter(|value| !value.is_empty())
}

fn optional_field_as_i64(
    map: &std::collections::HashMap<String, redis::Value>,
    key: &str,
) -> Option<i64> {
    optional_field_as_string(map, key).and_then(|value| {
        if value == "0" {
            None
        } else {
            value.parse::<i64>().ok()
        }
    })
}

fn redis_value_to_string(value: &redis::Value) -> Option<String> {
    match value {
        redis::Value::BulkString(bytes) => String::from_utf8(bytes.clone()).ok(),
        redis::Value::SimpleString(text) => Some(text.clone()),
        redis::Value::Int(value) => Some(value.to_string()),
        redis::Value::Okay => Some("OK".to_string()),
        _ => None,
    }
}

fn stream_read_reply_to_bus_events(
    reply: StreamReadReply,
) -> AppResult<Vec<(String, BusEventEnvelope)>> {
    let mut out = Vec::new();
    for key in reply.keys {
        for entry in key.ids {
            out.push(stream_id_to_bus_event(entry)?);
        }
    }
    Ok(out)
}

fn stream_id_to_bus_event(entry: StreamId) -> AppResult<(String, BusEventEnvelope)> {
    let outbox_event_id = entry
        .map
        .get("outbox_event_id")
        .and_then(redis_value_to_string)
        .ok_or_else(|| {
            AppError::Storage(format!("stream entry {} missing outbox_event_id", entry.id))
        })?;
    let envelope = BusEventEnvelope {
        id: uuid::Uuid::parse_str(&outbox_event_id).map_err(|e| {
            AppError::Storage(format!("invalid outbox_event_id {outbox_event_id}: {e}"))
        })?,
        event_kind: field_as_string(&entry.map, "event_kind"),
        stream_key: field_as_string(&entry.map, "stream_key"),
        aggregate_id: optional_field_as_string(&entry.map, "aggregate_id"),
        conversation_id: optional_field_as_i64(&entry.map, "conversation_id"),
        trace_id: optional_field_as_string(&entry.map, "trace_id"),
        task_id: optional_field_as_string(&entry.map, "task_id"),
        subagent_id: optional_field_as_string(&entry.map, "subagent_id"),
        route_key: optional_field_as_string(&entry.map, "route_key"),
        resolved_model: optional_field_as_string(&entry.map, "resolved_model"),
        evidence_count: optional_field_as_i64(&entry.map, "evidence_count")
            .map(|value| value.max(0) as u32),
        reasoning_tier: optional_field_as_string(&entry.map, "reasoning_tier"),
        fallback_kind: optional_field_as_string(&entry.map, "fallback_kind"),
        created_at: chrono::DateTime::parse_from_rfc3339(&field_as_string(
            &entry.map,
            "created_at",
        ))
        .map_err(|e| AppError::Storage(format!("invalid stream created_at: {e}")))?
        .with_timezone(&chrono::Utc),
        payload: serde_json::from_str(&field_as_string(&entry.map, "payload"))
            .map_err(|e| AppError::Storage(format!("invalid stream payload json: {e}")))?,
    };
    Ok((entry.id, envelope))
}

fn parse_pending_summary(
    stream: &str,
    group: &str,
    raw: redis::Value,
) -> AppResult<StreamPendingStats> {
    match raw {
        redis::Value::Array(items) if items.len() >= 4 => {
            let pending = match &items[0] {
                redis::Value::Int(value) => (*value).max(0) as u64,
                redis::Value::BulkString(bytes) => String::from_utf8(bytes.clone())
                    .ok()
                    .and_then(|value| value.parse::<u64>().ok())
                    .unwrap_or(0),
                _ => 0,
            };
            let first_id = redis_value_to_string(&items[1]).filter(|value| !value.is_empty());
            let last_id = redis_value_to_string(&items[2]).filter(|value| !value.is_empty());
            Ok(StreamPendingStats {
                stream: stream.to_string(),
                group: group.to_string(),
                pending,
                first_id,
                last_id,
            })
        }
        other => Err(AppError::Storage(format!(
            "unexpected XPENDING reply for {stream}/{group}: {other:?}"
        ))),
    }
}
