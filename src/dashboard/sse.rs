use std::{convert::Infallible, time::Duration};

use axum::response::sse::{Event, KeepAlive, Sse};
use futures::{Stream, StreamExt};
use tokio_stream::wrappers::BroadcastStream;

use crate::telemetry::event_bus::RuntimeEvent;

pub fn stream_events(
    receiver: tokio::sync::broadcast::Receiver<RuntimeEvent>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stream = BroadcastStream::new(receiver).filter_map(|event| async move {
        match event {
            Ok(event) => {
                let payload = serde_json::to_string(&event).ok()?;
                Some(Ok(Event::default().event("runtime_event").data(payload)))
            }
            Err(_) => None,
        }
    });

    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(10))
            .text("keepalive"),
    )
}
