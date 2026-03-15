CREATE TABLE IF NOT EXISTS plans (
  id BIGSERIAL PRIMARY KEY,
  plan_id TEXT NOT NULL UNIQUE,
  conversation_id BIGINT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
  goal TEXT NOT NULL,
  plan_json JSONB NOT NULL,
  status TEXT NOT NULL,
  created_at TIMESTAMPTZ NOT NULL,
  updated_at TIMESTAMPTZ NOT NULL
);

CREATE TABLE IF NOT EXISTS tasks (
  id BIGSERIAL PRIMARY KEY,
  task_id TEXT NOT NULL UNIQUE,
  plan_id TEXT NOT NULL REFERENCES plans(plan_id) ON DELETE CASCADE,
  task_json JSONB NOT NULL,
  state TEXT NOT NULL,
  assigned_subagent TEXT,
  created_at TIMESTAMPTZ NOT NULL,
  updated_at TIMESTAMPTZ NOT NULL
);

CREATE TABLE IF NOT EXISTS task_attempts (
  id BIGSERIAL PRIMARY KEY,
  task_id TEXT NOT NULL REFERENCES tasks(task_id) ON DELETE CASCADE,
  attempt_no BIGINT NOT NULL,
  subagent_id TEXT,
  status TEXT NOT NULL,
  started_at TIMESTAMPTZ,
  ended_at TIMESTAMPTZ,
  error TEXT,
  duration_ms BIGINT,
  created_at TIMESTAMPTZ NOT NULL,
  UNIQUE(task_id, attempt_no)
);

CREATE TABLE IF NOT EXISTS task_artifacts (
  id BIGSERIAL PRIMARY KEY,
  task_id TEXT NOT NULL REFERENCES tasks(task_id) ON DELETE CASCADE,
  attempt_id BIGINT REFERENCES task_attempts(id) ON DELETE SET NULL,
  subagent_id TEXT,
  artifact_json JSONB NOT NULL,
  created_at TIMESTAMPTZ NOT NULL
);

CREATE TABLE IF NOT EXISTS task_reviews (
  id BIGSERIAL PRIMARY KEY,
  task_id TEXT NOT NULL REFERENCES tasks(task_id) ON DELETE CASCADE,
  attempt_no BIGINT NOT NULL,
  reviewer TEXT NOT NULL,
  action TEXT NOT NULL,
  score DOUBLE PRECISION,
  notes TEXT,
  decision_json JSONB,
  created_at TIMESTAMPTZ NOT NULL
);

CREATE TABLE IF NOT EXISTS subagent_states (
  id BIGSERIAL PRIMARY KEY,
  subagent_id TEXT NOT NULL UNIQUE,
  role TEXT NOT NULL,
  state_json JSONB NOT NULL,
  updated_at TIMESTAMPTZ NOT NULL
);

CREATE TABLE IF NOT EXISTS subagent_heartbeats (
  id BIGSERIAL PRIMARY KEY,
  subagent_id TEXT NOT NULL,
  heartbeat_at TIMESTAMPTZ NOT NULL,
  state TEXT NOT NULL,
  task_id TEXT,
  created_at TIMESTAMPTZ NOT NULL
);

CREATE TABLE IF NOT EXISTS runtime_events (
  id UUID PRIMARY KEY,
  event_type TEXT NOT NULL,
  payload_json JSONB NOT NULL,
  created_at TIMESTAMPTZ NOT NULL
);

CREATE TABLE IF NOT EXISTS config_snapshots (
  id BIGSERIAL PRIMARY KEY,
  snapshot_type TEXT NOT NULL,
  source_path TEXT,
  payload_json JSONB NOT NULL,
  created_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_plans_conversation_id ON plans(conversation_id);
CREATE INDEX IF NOT EXISTS idx_tasks_plan_id ON tasks(plan_id);
CREATE INDEX IF NOT EXISTS idx_task_attempts_task ON task_attempts(task_id, attempt_no);
CREATE INDEX IF NOT EXISTS idx_task_artifacts_task ON task_artifacts(task_id);
CREATE INDEX IF NOT EXISTS idx_task_reviews_task ON task_reviews(task_id, attempt_no);
CREATE INDEX IF NOT EXISTS idx_subagent_heartbeats_subagent ON subagent_heartbeats(subagent_id, heartbeat_at DESC);
CREATE INDEX IF NOT EXISTS idx_runtime_events_created_at ON runtime_events(created_at DESC);
