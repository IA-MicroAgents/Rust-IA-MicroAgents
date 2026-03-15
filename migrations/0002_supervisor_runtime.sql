CREATE TABLE IF NOT EXISTS plans (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  plan_id TEXT NOT NULL UNIQUE,
  conversation_id INTEGER NOT NULL,
  goal TEXT NOT NULL,
  plan_json TEXT NOT NULL,
  status TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  FOREIGN KEY(conversation_id) REFERENCES conversations(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS tasks (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  task_id TEXT NOT NULL UNIQUE,
  plan_id TEXT NOT NULL,
  task_json TEXT NOT NULL,
  state TEXT NOT NULL,
  assigned_subagent TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  FOREIGN KEY(plan_id) REFERENCES plans(plan_id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS task_attempts (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  task_id TEXT NOT NULL,
  attempt_no INTEGER NOT NULL,
  subagent_id TEXT,
  status TEXT NOT NULL,
  started_at TEXT,
  ended_at TEXT,
  error TEXT,
  duration_ms INTEGER,
  created_at TEXT NOT NULL,
  FOREIGN KEY(task_id) REFERENCES tasks(task_id) ON DELETE CASCADE,
  UNIQUE(task_id, attempt_no)
);

CREATE TABLE IF NOT EXISTS task_artifacts (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  task_id TEXT NOT NULL,
  attempt_id INTEGER,
  subagent_id TEXT,
  artifact_json TEXT NOT NULL,
  created_at TEXT NOT NULL,
  FOREIGN KEY(task_id) REFERENCES tasks(task_id) ON DELETE CASCADE,
  FOREIGN KEY(attempt_id) REFERENCES task_attempts(id) ON DELETE SET NULL
);

CREATE TABLE IF NOT EXISTS task_reviews (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  task_id TEXT NOT NULL,
  attempt_no INTEGER NOT NULL,
  reviewer TEXT NOT NULL,
  action TEXT NOT NULL,
  score REAL,
  notes TEXT,
  decision_json TEXT,
  created_at TEXT NOT NULL,
  FOREIGN KEY(task_id) REFERENCES tasks(task_id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS subagent_states (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  subagent_id TEXT NOT NULL UNIQUE,
  role TEXT NOT NULL,
  state_json TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS subagent_heartbeats (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  subagent_id TEXT NOT NULL,
  heartbeat_at TEXT NOT NULL,
  state TEXT NOT NULL,
  task_id TEXT,
  created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS runtime_events (
  id TEXT PRIMARY KEY,
  event_type TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS config_snapshots (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  snapshot_type TEXT NOT NULL,
  source_path TEXT,
  payload_json TEXT NOT NULL,
  created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_plans_conversation_id ON plans(conversation_id);
CREATE INDEX IF NOT EXISTS idx_tasks_plan_id ON tasks(plan_id);
CREATE INDEX IF NOT EXISTS idx_task_attempts_task ON task_attempts(task_id, attempt_no);
CREATE INDEX IF NOT EXISTS idx_task_artifacts_task ON task_artifacts(task_id);
CREATE INDEX IF NOT EXISTS idx_task_reviews_task ON task_reviews(task_id, attempt_no);
CREATE INDEX IF NOT EXISTS idx_subagent_heartbeats_subagent ON subagent_heartbeats(subagent_id, heartbeat_at);
CREATE INDEX IF NOT EXISTS idx_runtime_events_created_at ON runtime_events(created_at);
