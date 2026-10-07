use acs::db::{current_schema_version, migrate_with, open_database, Migration, MIGRATIONS};
use rusqlite::Connection;
use std::path::PathBuf;

const BASELINE: &str = include_str!("../../schema/001-baseline.sql");
const V2: Migration = Migration {
    version: 2,
    name: "task-cost",
    sql: "ALTER TABLE tasks ADD COLUMN cost_usd REAL NOT NULL DEFAULT 0;",
};
const V3: Migration = Migration {
    version: 3,
    name: "verdicts",
    sql: "CREATE TABLE IF NOT EXISTS verdicts (task_id INTEGER NOT NULL, verdict TEXT NOT NULL);",
};

fn baseline() -> Migration {
    Migration {
        version: 1,
        name: "baseline",
        sql: BASELINE,
    }
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "acs-schema-{tag}-{}-{}",
        std::process::id(),
        rand::random::<u32>()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("bus.db")
}

fn populate(conn: &Connection) {
    conn.execute_batch(
        "INSERT INTO agents(id, role, created_ms) VALUES('alice', 'worker', 1);
         INSERT INTO tasks(title, state, creator, created_ms, updated_ms) VALUES('t1', 'open', 'alice', 1, 1);
         INSERT INTO messages(id, ts_ms, sender, body) VALUES('m1', 1, 'alice', 'hello');
         INSERT INTO events(ts_ms, actor, kind, entity, entity_id) VALUES(1, 'alice', 'x', 'task', '1');",
    )
    .unwrap();
}

fn counts(conn: &Connection) -> Vec<i64> {
    ["agents", "tasks", "messages", "events"]
        .iter()
        .map(|t| {
            conn.query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get(0))
                .unwrap()
        })
        .collect()
}

fn schema_rows(conn: &Connection) -> Vec<String> {
    let mut stmt = conn
        .prepare("SELECT name || '|' || COALESCE(sql, '') FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' AND name <> 'meta' ORDER BY name")
        .unwrap();
    stmt.query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
}

#[test]
fn migrations_match_the_schema_directory() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../schema");
    let mut files: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|f| f.ends_with(".sql"))
        .collect();
    files.sort();
    assert_eq!(
        files.len(),
        MIGRATIONS.len(),
        "schema/ and MIGRATIONS drifted: {files:?}"
    );
    for (index, migration) in MIGRATIONS.iter().enumerate() {
        assert_eq!(migration.version as usize, index + 1);
        assert_eq!(
            files[index],
            format!("{:03}-{}.sql", migration.version, migration.name)
        );
        if index > 0 {
            let sql = migration.sql.to_uppercase();
            assert!(
                !sql.contains("DROP") && !sql.contains("RENAME"),
                "migrations must be additive"
            );
        }
    }
}

#[test]
fn fresh_open_carries_latest_version() {
    let path = scratch("fresh");
    let conn = open_database(&path).unwrap();
    assert_eq!(
        current_schema_version(&conn).unwrap(),
        MIGRATIONS.len() as u32
    );
}

#[test]
fn legacy_v1_database_opens_unchanged() {
    let path = scratch("legacy");
    let legacy = Connection::open(&path).unwrap();
    legacy.execute_batch(BASELINE).unwrap();
    legacy
        .execute_batch("INSERT INTO meta(key, value) VALUES('schema_version', '1')")
        .unwrap();
    populate(&legacy);
    let (before, schema) = (counts(&legacy), schema_rows(&legacy));
    drop(legacy);
    let conn = open_database(&path).unwrap();
    assert_eq!(current_schema_version(&conn).unwrap(), 1);
    assert_eq!(counts(&conn), before);
    assert_eq!(schema_rows(&conn), schema);
}

#[test]
fn markerless_database_is_adopted() {
    let path = scratch("markerless");
    let legacy = Connection::open(&path).unwrap();
    legacy.execute_batch(BASELINE).unwrap();
    populate(&legacy);
    let before = counts(&legacy);
    drop(legacy);
    let conn = open_database(&path).unwrap();
    assert_eq!(
        current_schema_version(&conn).unwrap(),
        MIGRATIONS.len() as u32
    );
    assert_eq!(counts(&conn), before);
}

#[test]
fn every_prior_version_migrates_forward_keeping_rows() {
    let chain = [baseline(), V2, V3];
    for from in 1..chain.len() {
        let path = scratch("prior");
        let old = Connection::open(&path).unwrap();
        migrate_with(&old, "t", &chain[..from]).unwrap();
        assert_eq!(current_schema_version(&old).unwrap(), from as u32);
        populate(&old);
        let before = counts(&old);
        drop(old);
        let conn = Connection::open(&path).unwrap();
        migrate_with(&conn, "t", &chain).unwrap();
        assert_eq!(current_schema_version(&conn).unwrap(), 3);
        assert_eq!(counts(&conn), before);
        let cost: f64 = conn
            .query_row("SELECT cost_usd FROM tasks", [], |r| r.get(0))
            .unwrap();
        assert_eq!(cost, 0.0);
        migrate_with(&conn, "t", &chain).unwrap();
        assert_eq!(current_schema_version(&conn).unwrap(), 3);
    }
}

#[test]
fn newer_database_is_refused_without_a_write() {
    let path = scratch("newer");
    let conn = Connection::open(&path).unwrap();
    migrate_with(&conn, "t", &[baseline(), V2, V3]).unwrap();
    populate(&conn);
    conn.execute(
        "UPDATE meta SET value = '999' WHERE key = 'schema_version'",
        [],
    )
    .unwrap();
    let before = counts(&conn);
    drop(conn);
    let error = open_database(&path).unwrap_err();
    assert!(
        error.message.contains("upgrade qagent"),
        "{}",
        error.message
    );
    let after = Connection::open(&path).unwrap();
    assert_eq!(current_schema_version(&after).unwrap(), 999);
    assert_eq!(counts(&after), before);
    assert!(migrate_with(&after, "t", &[baseline(), V2]).is_err());
    assert_eq!(current_schema_version(&after).unwrap(), 999);
}

#[test]
fn unreadable_marker_is_refused_not_replaced() {
    let path = scratch("garbage");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(BASELINE).unwrap();
    conn.execute_batch("INSERT INTO meta(key, value) VALUES('schema_version', 'banana')")
        .unwrap();
    drop(conn);
    assert!(open_database(&path).is_err());
    let after = Connection::open(&path).unwrap();
    let value: String = after
        .query_row(
            "SELECT value FROM meta WHERE key = 'schema_version'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(value, "banana");
}

#[test]
fn failing_migration_rolls_back() {
    let path = scratch("rollback");
    let conn = Connection::open(&path).unwrap();
    migrate_with(&conn, "t", &[baseline()]).unwrap();
    let broken = Migration {
        version: 2,
        name: "broken",
        sql: "ALTER TABLE tasks ADD COLUMN ok TEXT; ALTER TABLE no_such_table ADD COLUMN x TEXT;",
    };
    assert!(migrate_with(&conn, "t", &[baseline(), broken]).is_err());
    assert_eq!(current_schema_version(&conn).unwrap(), 1);
    assert!(conn.prepare("SELECT ok FROM tasks").is_err());
}
