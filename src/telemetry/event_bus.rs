use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::{
    sync::broadcast,
    time::{sleep, Duration},
};
use tracing::error;
use uuid::Uuid;

use crate::{
    errors::AppResult,
    storage::{BusEventEnvelope, Store},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeEvent {
    pub id: String,
    pub event_type: String,
    pub payload: Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone)]
pub struct EventBus {
    sender: broadcast::Sender<RuntimeEvent>,
    store: Option<Store>,
}

impl EventBus {
    pub fn new(store: Store) -> Self {
        let (sender, _) = broadcast::channel(2048);
        Self {
            sender,
            store: Some(store),
        }
    }

    pub fn ephemeral() -> Self {
        let (sender, _) = broadcast::channel(2048);
        Self {
            sender,
            store: None,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<RuntimeEvent> {
        self.sender.subscribe()
    }

    pub fn publish(&self, event_type: &str, payload: Value) -> AppResult<()> {
        let event = RuntimeEvent {
            id: Uuid::new_v4().to_string(),
            event_type: event_type.to_string(),
            payload,
            created_at: Utc::now(),
        };
        let _ = self.sender.send(event.clone());
        if let Some(store) = &self.store {
            let store = store.clone();
            let event_for_store = event.clone();
            match tokio::runtime::Handle::try_current() {
                Ok(handle) => {
                    handle.spawn(async move {
                        if store.bus_enabled() {
                            let envelope = runtime_event_envelope(&event_for_store);
                            if let Err(err) = store.enqueue_bus_event(&envelope).await {
                                error!(
                                    event_type = %event_for_store.event_type,
                                    event_id = %event_for_store.id,
                                    error = %err,
                                    "failed to enqueue runtime event into outbox"
                                );
                            }
                        }
                        for attempt in 1..=3 {
                            match store
                                .insert_runtime_event_fields(
                                    &event_for_store.id,
                                    &event_for_store.event_type,
                                    &event_for_store.payload,
                                    event_for_store.created_at,
                                )
                                .await
                            {
                                Ok(()) => return,
                                Err(err) if attempt < 3 => {
                                    sleep(Duration::from_millis(75 * attempt as u64)).await;
                                    if attempt == 2 {
                                        error!(
                                            event_type = %event_for_store.event_type,
                                            event_id = %event_for_store.id,
                                            attempt,
                                            error = %err,
                                            "runtime event persistence still failing; final retry pending"
                                        );
                                    }
                                }
                                Err(err) => {
                                    error!(
                                        event_type = %event_for_store.event_type,
                                        event_id = %event_for_store.id,
                                        attempt,
                                        error = %err,
                                        "failed to persist runtime event"
                                    );
                                }
                            }
                        }
                    });
                }
                Err(_) => {
                    std::thread::spawn(move || {
                        let runtime = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build();
                        let Ok(runtime) = runtime else {
                            return;
                        };
                        if store.bus_enabled() {
                            let envelope = runtime_event_envelope(&event_for_store);
                            if let Err(err) = runtime.block_on(store.enqueue_bus_event(&envelope)) {
                                error!(
                                    event_type = %event_for_store.event_type,
                                    event_id = %event_for_store.id,
                                    error = %err,
                                    "failed to enqueue runtime event into outbox"
                                );
                            }
                        }
                        for attempt in 1..=3 {
                            match runtime.block_on(store.insert_runtime_event_fields(
                                &event_for_store.id,
                                &event_for_store.event_type,
                                &event_for_store.payload,
                                event_for_store.created_at,
                            )) {
                                Ok(()) => return,
                                Err(err) if attempt < 3 => {
                                    std::thread::sleep(std::time::Duration::from_millis(
                                        75 * attempt as u64,
                                    ));
                                    if attempt == 2 {
                                        error!(
                                            event_type = %event_for_store.event_type,
                                            event_id = %event_for_store.id,
                                            attempt,
                                            error = %err,
                                            "runtime event persistence still failing; final retry pending"
                                        );
                                    }
                                }
                                Err(err) => {
                                    error!(
                                        event_type = %event_for_store.event_type,
                                        event_id = %event_for_store.id,
                                        attempt,
                                        error = %err,
                                        "failed to persist runtime event"
                                    );
                                }
                            }
                        }
                    });
                }
            }
        }
        Ok(())
    }
}

fn runtime_event_envelope(event: &RuntimeEvent) -> BusEventEnvelope {
    let payload = &event.payload;
    BusEventEnvelope {
        id: Uuid::parse_str(&event.id).unwrap_or_else(|_| Uuid::new_v4()),
        event_kind: event.event_type.clone(),
        stream_key: "events".to_string(),
        aggregate_id: pick_string(payload, &["event_id", "plan_id", "task_id"]),
        conversation_id: pick_i64(payload, &["conversation_id"]),
        trace_id: pick_string(payload, &["trace_id"]),
        task_id: pick_string(payload, &["task_id"]),
        subagent_id: pick_string(payload, &["subagent_id"]),
        route_key: pick_string(payload, &["route_key", "model_route"]),
        resolved_model: pick_string(payload, &["resolved_model", "model"]),
        evidence_count: pick_i64(payload, &["evidence_count"]).map(|value| value.max(0) as u32),
        reasoning_tier: pick_string(payload, &["reasoning_tier"]),
        fallback_kind: pick_string(payload, &["fallback_kind"]),
        created_at: event.created_at,
        payload: payload.clone(),
    }
}

fn pick_string(payload: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        payload
            .get(*key)
            .and_then(Value::as_str)
            .map(ToString::to_string)
    })
}

fn pick_i64(payload: &Value, keys: &[&str]) -> Option<i64> {
    keys.iter()
        .find_map(|key| payload.get(*key).and_then(Value::as_i64))
}
