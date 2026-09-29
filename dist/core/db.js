/**
 * One SQLite file in WAL mode, opened directly by every process.
 * Pragmas and the transaction helper are ported from store.ts:43-138.
 */
import { DatabaseSync } from "node:sqlite";
import { chmodSync, existsSync, mkdirSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";
export const SCHEMA_VERSION = "1";
export const BUSY_TIMEOUT_MS = 5000;
export const SCHEMA_SQL = `
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
`;
function envValue(env, ...names) {
    for (const name of names) {
        const value = env[name];
        if (typeof value === "string" && value.trim())
            return value.trim();
    }
    return undefined;
}
/**
 * Database path: --db flag, then QAGENT_BUS_DB, then <QAGENT_HOME or AGENT_BUS_HOME or ~/.agent-bus>/bus.db.
 * Token files and inbox signal files live next to the database, so one path fixes the whole home.
 */
export function resolveDbPath(flag, env = process.env) {
    if (flag && flag.trim())
        return resolve(flag.trim());
    const direct = envValue(env, "QAGENT_BUS_DB");
    if (direct)
        return resolve(direct);
    const home = envValue(env, "QAGENT_HOME", "AGENT_BUS_HOME") ?? join(homedir(), ".agent-bus");
    return resolve(home, "bus.db");
}
export function homeFor(dbPath) {
    return dirname(resolve(dbPath));
}
function schemaReady(db) {
    const table = db.prepare("SELECT 1 AS ok FROM sqlite_master WHERE type = 'table' AND name = 'meta'").get();
    if (!table)
        return false;
    const row = db.prepare("SELECT value FROM meta WHERE key = 'schema_version'").get();
    return row?.value === SCHEMA_VERSION;
}
/**
 * Open the bus. The busy timeout is set before anything else so concurrent first
 * opens wait for each other. The schema is only written when it is missing, so an
 * ordinary open performs no write at all.
 */
export function openDatabase(path, options = {}) {
    if (options.readOnly) {
        if (!existsSync(path))
            throw new Error(`bus database not found: ${path} (run \`qagent init\`)`);
        const db = new DatabaseSync(path, { readOnly: true, timeout: BUSY_TIMEOUT_MS });
        db.exec(`PRAGMA busy_timeout = ${BUSY_TIMEOUT_MS}`);
        return db;
    }
    const dir = dirname(path);
    if (!existsSync(dir)) {
        mkdirSync(dir, { recursive: true, mode: 0o700 });
        try {
            chmodSync(dir, 0o700);
        }
        catch { /* best effort */ }
    }
    const created = !existsSync(path);
    const db = new DatabaseSync(path, { timeout: BUSY_TIMEOUT_MS });
    if (created) {
        try {
            chmodSync(path, 0o600);
        }
        catch { /* best effort */ }
    }
    db.exec(`PRAGMA busy_timeout = ${BUSY_TIMEOUT_MS}`);
    const mode = db.prepare("PRAGMA journal_mode").get();
    if (String(mode.journal_mode).toLowerCase() !== "wal")
        db.exec("PRAGMA journal_mode = WAL");
    db.exec("PRAGMA foreign_keys = ON");
    if (!schemaReady(db)) {
        transaction(db, () => {
            db.exec(SCHEMA_SQL);
            db.prepare("INSERT INTO meta(key, value) VALUES('schema_version', ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value")
                .run(SCHEMA_VERSION);
        });
    }
    return db;
}
/** One BEGIN IMMEDIATE transaction. Nested calls join the outer transaction. */
export function transaction(db, fn) {
    if (db.isTransaction)
        return fn();
    db.exec("BEGIN IMMEDIATE");
    try {
        const value = fn();
        db.exec("COMMIT");
        return value;
    }
    catch (error) {
        if (db.isTransaction)
            db.exec("ROLLBACK");
        throw error;
    }
}
/** Constant SQL prepared once per connection instead of on every call. Entries die with the connection. */
const statements = new WeakMap();
export function prepared(db, sql) {
    let perDb = statements.get(db);
    if (!perDb) {
        perDb = new Map();
        statements.set(db, perDb);
    }
    let statement = perDb.get(sql);
    if (!statement) {
        statement = db.prepare(sql);
        perDb.set(sql, statement);
    }
    return statement;
}
export function getMeta(db, key) {
    const row = prepared(db, "SELECT value FROM meta WHERE key = ?").get(key);
    return row?.value ?? null;
}
export function setMeta(db, key, value) {
    prepared(db, "INSERT INTO meta(key, value) VALUES(?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value").run(key, value);
}
export function appendEvent(db, event) {
    const result = prepared(db, `
    INSERT INTO events(ts_ms, actor, kind, entity, entity_id, data_json, source) VALUES(?, ?, ?, ?, ?, ?, ?)
  `).run(event.tsMs, event.actor, event.kind, event.entity, String(event.entityId), JSON.stringify(event.data ?? {}), event.source ?? "v2");
    return Number(result.lastInsertRowid);
}
export function latestEventSeq(db) {
    const row = prepared(db, "SELECT COALESCE(MAX(seq), 0) AS seq FROM events").get();
    return Number(row.seq);
}
//# sourceMappingURL=db.js.map