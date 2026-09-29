//! One SQLite file in WAL mode, opened directly by every process.
//! Mirrors src/core/db.ts: pragmas, schema bootstrap, transaction helper.

use crate::error::Result;
use rusqlite::Connection;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

pub const SCHEMA_VERSION: &str = "1";
pub const BUSY_TIMEOUT_MS: u32 = 5000;

pub const SCHEMA_SQL: &str = r#"
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
"#;

/// Database path: --db flag, then QAGENT_BUS_DB, then <QAGENT_HOME or AGENT_BUS_HOME or ~/.agent-bus>/bus.db.
pub fn resolve_db_path(flag: Option<&str>) -> PathBuf {
    resolve_db_path_with(flag, |name| {
        std::env::var(name)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    })
}

/// Same resolution with an explicit environment lookup (the CLI carries its own env map).
pub fn resolve_db_path_with(flag: Option<&str>, env: impl Fn(&str) -> Option<String>) -> PathBuf {
    if let Some(flag) = flag {
        let trimmed = flag.trim();
        if !trimmed.is_empty() {
            return absolutize(Path::new(trimmed));
        }
    }
    if let Some(direct) = env("QAGENT_BUS_DB") {
        return absolutize(Path::new(&direct));
    }
    let home = env("QAGENT_HOME")
        .or_else(|| env("AGENT_BUS_HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".agent-bus")
        });
    absolutize(&home.join("bus.db"))
}

/// Absolute path with . / .. normalized lexically (like Node's path.resolve —
/// does not consult the filesystem).
pub fn absolutize(path: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .join(path)
    };
    let mut out: Vec<std::ffi::OsString> = Vec::new();
    for component in joined.components() {
        use std::path::Component::*;
        match component {
            CurDir => {}
            ParentDir => {
                if out.len() > 1 {
                    out.pop();
                }
            }
            other => out.push(other.as_os_str().to_os_string()),
        }
    }
    let mut result = PathBuf::from("/");
    for part in out {
        result.push(part);
    }
    result
}

pub fn home_for(db_path: &Path) -> PathBuf {
    absolutize(db_path)
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

fn schema_ready(conn: &Connection) -> Result<bool> {
    let table: Option<i64> = conn
        .query_row(
            "SELECT 1 AS ok FROM sqlite_master WHERE type = 'table' AND name = 'meta'",
            [],
            |row| row.get(0),
        )
        .ok();
    if table.is_none() {
        return Ok(false);
    }
    let version: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )
        .ok();
    Ok(version.as_deref() == Some(SCHEMA_VERSION))
}

/// Open the bus. The busy timeout is set before anything else so concurrent first
/// opens wait for each other. The schema is only written when it is missing, so an
/// ordinary open performs no write at all.
pub fn open_database(path: &Path) -> Result<Connection> {
    let dir = path.parent().unwrap_or(Path::new("."));
    if !dir.exists() {
        fs::create_dir_all(dir)?;
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
    }
    let created = !path.exists();
    let conn = Connection::open(path)?;
    if created {
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    conn.busy_timeout(std::time::Duration::from_millis(BUSY_TIMEOUT_MS as u64))?;
    let mode: String = conn.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
    if mode.to_lowercase() != "wal" {
        conn.execute_batch("PRAGMA journal_mode = WAL")?;
    }
    conn.execute_batch("PRAGMA foreign_keys = ON")?;
    if !schema_ready(&conn)? {
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let outcome = (|| -> Result<()> {
            conn.execute_batch(SCHEMA_SQL)?;
            conn.execute(
                "INSERT INTO meta(key, value) VALUES('schema_version', ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [SCHEMA_VERSION],
            )?;
            Ok(())
        })();
        match outcome {
            Ok(()) => conn.execute_batch("COMMIT")?,
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(error);
            }
        }
    }
    Ok(conn)
}

pub fn append_event(
    conn: &Connection,
    ts_ms: i64,
    actor: &str,
    kind: &str,
    entity: &str,
    entity_id: &str,
    data: &serde_json::Value,
) -> Result<i64> {
    let mut stmt = conn.prepare_cached(
        "INSERT INTO events(ts_ms, actor, kind, entity, entity_id, data_json, source) VALUES(?, ?, ?, ?, ?, ?, 'v2')",
    )?;
    stmt.execute(rusqlite::params![
        ts_ms,
        actor,
        kind,
        entity,
        entity_id,
        data.to_string()
    ])?;
    Ok(conn.last_insert_rowid())
}

pub fn latest_event_seq(conn: &Connection) -> Result<i64> {
    Ok(conn
        .prepare_cached("SELECT COALESCE(MAX(seq), 0) AS seq FROM events")?
        .query_row([], |row| row.get(0))?)
}

pub fn get_meta(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn
        .prepare_cached("SELECT value FROM meta WHERE key = ?")?
        .query_row([key], |row| row.get(0))
        .ok())
}

pub fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.prepare_cached("INSERT INTO meta(key, value) VALUES(?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value")?
        .execute(rusqlite::params![key, value])?;
    Ok(())
}
