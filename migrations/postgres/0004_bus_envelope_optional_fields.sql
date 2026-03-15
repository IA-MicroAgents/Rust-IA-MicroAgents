ALTER TABLE outbox_events
  ADD COLUMN IF NOT EXISTS evidence_count BIGINT,
  ADD COLUMN IF NOT EXISTS reasoning_tier TEXT,
  ADD COLUMN IF NOT EXISTS fallback_kind TEXT;
