CREATE TABLE IF NOT EXISTS brain_memories (
  id BIGSERIAL PRIMARY KEY,
  scope_kind TEXT NOT NULL,
  user_id TEXT,
  conversation_id BIGINT REFERENCES conversations(id) ON DELETE SET NULL,
  memory_kind TEXT NOT NULL,
  memory_key TEXT NOT NULL,
  subject TEXT NOT NULL,
  what_value TEXT NOT NULL,
  why_value TEXT,
  where_context TEXT,
  learned_value TEXT,
  provenance_json JSONB NOT NULL DEFAULT '{}'::jsonb,
  confidence DOUBLE PRECISION NOT NULL DEFAULT 0.5,
  status TEXT NOT NULL DEFAULT 'active',
  superseded_by BIGINT REFERENCES brain_memories(id) ON DELETE SET NULL,
  source_turn_id BIGINT REFERENCES turns(id) ON DELETE SET NULL,
  search_text TEXT NOT NULL,
  created_at TIMESTAMPTZ NOT NULL,
  updated_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_brain_memories_conversation_scope
  ON brain_memories(conversation_id, status, id DESC);
CREATE INDEX IF NOT EXISTS idx_brain_memories_user_scope
  ON brain_memories(user_id, status, id DESC);
CREATE INDEX IF NOT EXISTS idx_brain_memories_lookup
  ON brain_memories(scope_kind, memory_key, status, id DESC);
CREATE INDEX IF NOT EXISTS idx_brain_memories_fts
  ON brain_memories USING GIN (to_tsvector('simple', search_text));
