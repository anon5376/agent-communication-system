
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS agents (
  id TEXT PRIMARY KEY CHECK (id <> '' AND id NOT GLOB '*[^A-Za-z0-9._-]*'),
  role TEXT NOT NULL DEFAULT '', model TEXT NOT NULL DEFAULT '', harness TEXT NOT NULL DEFAULT '',
  parent_id TEXT REFERENCES agents(id) ON DELETE SET NULL,
  status TEXT NOT NULL DEFAULT 'offline',
  wait_until_ms INTEGER, last_seen_ms INTEGER, created_ms INTEGER NOT NULL, meta_json TEXT NOT NULL DEFAULT '{}');
CREATE TABLE IF NOT EXISTS identities (
  agent_id TEXT PRIMARY KEY, token_hash TEXT NOT NULL UNIQUE,
  authority TEXT NOT NULL CHECK (authority IN ('operator','manager','worker')),
  permissions_json TEXT NOT NULL, created_ms INTEGER NOT NULL, updated_ms INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS messages (
  seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT NOT NULL UNIQUE, ts_ms INTEGER NOT NULL,
  sender TEXT NOT NULL, recipient TEXT,
  type TEXT NOT NULL DEFAULT 'info', subject TEXT NOT NULL DEFAULT '', body TEXT NOT NULL,
  thread TEXT NOT NULL DEFAULT '', task_id INTEGER, refs_json TEXT NOT NULL DEFAULT '[]',
  requires_ack INTEGER NOT NULL DEFAULT 0, source TEXT NOT NULL DEFAULT 'v2');
CREATE INDEX IF NOT EXISTS messages_inbox ON messages(recipient, seq);
CREATE INDEX IF NOT EXISTS messages_thread ON messages(thread, seq);
CREATE INDEX IF NOT EXISTS messages_task ON messages(task_id, seq);
CREATE TABLE IF NOT EXISTS cursors (agent_id TEXT PRIMARY KEY, last_seq INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS acks (seq INTEGER NOT NULL, agent_id TEXT NOT NULL, ack_ms INTEGER NOT NULL, PRIMARY KEY (seq, agent_id));
CREATE TABLE IF NOT EXISTS tasks (
  id INTEGER PRIMARY KEY AUTOINCREMENT, legacy_id TEXT UNIQUE, project TEXT,
  parent_id INTEGER REFERENCES tasks(id) ON DELETE SET NULL,
  title TEXT NOT NULL, brief TEXT NOT NULL DEFAULT '', acceptance TEXT NOT NULL DEFAULT '',
  role TEXT NOT NULL DEFAULT '', priority TEXT NOT NULL DEFAULT 'normal',
  state TEXT NOT NULL CHECK (state IN ('open','blocked','claimed','submitted','changes_requested',
                                        'accepted','failed','cancelled')),
  creator TEXT NOT NULL, assignee TEXT, reviewer TEXT,
  path_scopes_json TEXT NOT NULL DEFAULT '[]', refs_json TEXT NOT NULL DEFAULT '[]',
  result_json TEXT, review_json TEXT, round INTEGER NOT NULL DEFAULT 1,
  attempts INTEGER NOT NULL DEFAULT 0, max_retries INTEGER NOT NULL DEFAULT 2,
  claim_expires_ms INTEGER, created_ms INTEGER NOT NULL, updated_ms INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS tasks_board ON tasks(state, assignee, updated_ms);
CREATE INDEX IF NOT EXISTS tasks_parent ON tasks(parent_id);
CREATE INDEX IF NOT EXISTS tasks_project ON tasks(project, state);
CREATE TABLE IF NOT EXISTS task_deps (task_id INTEGER NOT NULL, depends_on INTEGER NOT NULL, PRIMARY KEY (task_id, depends_on));
CREATE INDEX IF NOT EXISTS task_deps_rev ON task_deps(depends_on);
CREATE TABLE IF NOT EXISTS task_notes (id INTEGER PRIMARY KEY AUTOINCREMENT, task_id INTEGER NOT NULL, author TEXT NOT NULL,
  ts_ms INTEGER NOT NULL, body TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS task_notes_task ON task_notes(task_id);
CREATE TABLE IF NOT EXISTS leases (project TEXT NOT NULL, path TEXT NOT NULL, task_id INTEGER NOT NULL,
  created_ms INTEGER NOT NULL, PRIMARY KEY (task_id, path));
CREATE INDEX IF NOT EXISTS leases_project ON leases(project, path);
CREATE TABLE IF NOT EXISTS events (seq INTEGER PRIMARY KEY AUTOINCREMENT, ts_ms INTEGER NOT NULL, actor TEXT NOT NULL,
  kind TEXT NOT NULL, entity TEXT NOT NULL, entity_id TEXT NOT NULL, data_json TEXT NOT NULL DEFAULT '{}',
  source TEXT NOT NULL DEFAULT 'v2');
CREATE INDEX IF NOT EXISTS events_entity ON events(entity, entity_id, seq);
CREATE TABLE IF NOT EXISTS usage (agent_id TEXT NOT NULL, day TEXT NOT NULL, turns INTEGER, input_tokens INTEGER,
  output_tokens INTEGER, cost_usd REAL, latency_ms INTEGER, PRIMARY KEY (agent_id, day));
