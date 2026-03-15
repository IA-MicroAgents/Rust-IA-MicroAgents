CREATE TABLE IF NOT EXISTS outbox_events (
  id UUID PRIMARY KEY,
  event_kind TEXT NOT NULL,
  stream_key TEXT NOT NULL,
  aggregate_id TEXT,
  conversation_id BIGINT REFERENCES conversations(id) ON DELETE SET NULL,
  trace_id TEXT,
  task_id TEXT,
  subagent_id TEXT,
  route_key TEXT,
  resolved_model TEXT,
  payload_json JSONB NOT NULL,
  created_at TIMESTAMPTZ NOT NULL,
  publish_attempts BIGINT NOT NULL DEFAULT 0,
  published_at TIMESTAMPTZ,
  last_error TEXT
);

CREATE INDEX IF NOT EXISTS idx_outbox_events_pending
  ON outbox_events(published_at, created_at)
  WHERE published_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_outbox_events_stream_key
  ON outbox_events(stream_key, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_outbox_events_trace_id
  ON outbox_events(trace_id);
