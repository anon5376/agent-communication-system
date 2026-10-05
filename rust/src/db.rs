//! One SQLite file in WAL mode, opened directly by every process.
//! Mirrors src/core/db.ts: pragmas, schema bootstrap, transaction helper.

use crate::error::{BusError, Result};
use rusqlite::Connection;
use std::fs;
use std::path::{Path, PathBuf};

pub const BUSY_TIMEOUT_MS: u32 = 5000;

/// Ordered, additive-only migrations shared with the TypeScript build (`schema/NNN-name.sql` at the
/// repo root). Adding a file means adding a line here; `tests/schema_tests.rs` fails if they drift.
pub const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    name: "baseline",
    sql: include_str!("../../schema/001-baseline.sql"),
}];

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
    // Lexical normalization (Node's path.resolve — no filesystem lookups).
    // Components keep the platform's prefix/root (C:\, \\server\share, /)
    // instead of being re-rooted on "/", and ".." cannot cross the root:
    // PathBuf::pop on a bare root does nothing.
    let mut result = PathBuf::new();
    for component in joined.components() {
        use std::path::Component::*;
        match component {
            CurDir => {}
            ParentDir => {
                result.pop();
            }
            other => result.push(other.as_os_str()),
        }
    }
    result
}

pub fn home_for(db_path: &Path) -> PathBuf {
    absolutize(db_path)
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

pub struct Migration {
    pub version: u32,
    pub name: &'static str,
    pub sql: &'static str,
}

/// The version marker: 0 for an empty database, otherwise the highest applied migration.
pub fn current_schema_version(conn: &Connection) -> Result<u32> {
    let table: Option<i64> = conn
        .query_row(
            "SELECT 1 AS ok FROM sqlite_master WHERE type = 'table' AND name = 'meta'",
            [],
            |row| row.get(0),
        )
        .ok();
    if table.is_none() {
        return Ok(0);
    }
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )
        .ok();
    match value {
        None => Ok(0),
        Some(text) => {
            if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
                return Err(BusError::invalid(format!(
                    "bus database has an unreadable schema_version: {text:?}"
                )));
            }
            text.parse::<u32>().map_err(|_| {
                BusError::invalid(format!("bus database has an unreadable schema_version: {text:?}"))
            })
        }
    }
}

fn newer_schema_error(found: u32, known: u32, path: &str) -> BusError {
    BusError::invalid(format!(
        "bus database {path} is at schema version {found}, but this qagent only knows up to {known}: upgrade qagent (it will not write to a newer schema)"
    ))
}

/// Bring the database up to the newest migration in one BEGIN IMMEDIATE transaction. The version
/// is re-read inside the transaction so concurrent first opens apply each migration once. A
/// database newer than the migrations is refused, never rewritten.
pub fn migrate_with(conn: &Connection, path: &str, migrations: &[Migration]) -> Result<()> {
    let known = migrations.len() as u32;
    let found = current_schema_version(conn)?;
    if found > known {
        return Err(newer_schema_error(found, known, path));
    }
    if found == known {
        return Ok(());
    }
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let outcome = (|| -> Result<()> {
        let start = current_schema_version(conn)?;
        if start > known {
            return Err(newer_schema_error(start, known, path));
        }
        for migration in &migrations[start as usize..] {
            conn.execute_batch(migration.sql)?;
            set_meta(conn, "schema_version", &migration.version.to_string())?;
        }
        Ok(())
    })();
    match outcome {
        Ok(()) => conn.execute_batch("COMMIT")?,
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(error);
        }
    }
    Ok(())
}

pub fn migrate(conn: &Connection, path: &str) -> Result<()> {
    migrate_with(conn, path, MIGRATIONS)
}

/// Open the bus. The busy timeout is set before anything else so concurrent first
/// opens wait for each other. The schema is only written when it is missing or behind, so an
/// ordinary open performs no write at all. A newer schema is refused.
pub fn open_database(path: &Path) -> Result<Connection> {
    let dir = path.parent().unwrap_or(Path::new("."));
    if !dir.exists() {
        fs::create_dir_all(dir)?;
        crate::platform::chmod_private(dir, 0o700);
    }
    let created = !path.exists();
    let conn = Connection::open(path)?;
    if created {
        crate::platform::chmod_private(path, 0o600);
    }
    conn.busy_timeout(std::time::Duration::from_millis(BUSY_TIMEOUT_MS as u64))?;
    let mode: String = conn.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
    if mode.to_lowercase() != "wal" {
        conn.execute_batch("PRAGMA journal_mode = WAL")?;
    }
    conn.execute_batch("PRAGMA foreign_keys = ON")?;
    migrate(&conn, &path.display().to_string())?;
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
