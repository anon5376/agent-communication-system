// Port of tests/core-import.test.ts — same fixture stores, same expected
// counts. The fixture builder mirrors the TS buildFixtures exactly so a Rust
// run and a Node run read identical bytes.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use acs::bus::Bus;
use acs::identity::hash_token;
use acs::import::{run_import, ImportOptions, ImportSources};
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};

const T0: i64 = 1_784_653_403_000;

const QAGENT_SCHEMA: &str = r#"
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE identities (id TEXT PRIMARY KEY, token_hash TEXT NOT NULL UNIQUE, authority TEXT NOT NULL, permissions_json TEXT NOT NULL, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);
CREATE TABLE agents (id TEXT PRIMARY KEY, json TEXT NOT NULL, updated_at INTEGER NOT NULL);
CREATE TABLE messages (seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT NOT NULL UNIQUE, to_agent TEXT NOT NULL, delivered INTEGER NOT NULL DEFAULT 0, json TEXT NOT NULL, created_at INTEGER NOT NULL);
CREATE TABLE tasks (id TEXT PRIMARY KEY, run_id TEXT, state TEXT NOT NULL, assignee TEXT NOT NULL, parent_task_id TEXT, updated_at INTEGER NOT NULL, json TEXT NOT NULL);
CREATE TABLE runs (id TEXT PRIMARY KEY, status TEXT NOT NULL, updated_at INTEGER NOT NULL, json TEXT NOT NULL);
CREATE TABLE path_leases (task_id TEXT NOT NULL, run_id TEXT NOT NULL, path TEXT NOT NULL, created_at INTEGER NOT NULL, PRIMARY KEY(task_id, path));
"#;

const PROTOTYPE_SCHEMA: &str = r#"
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT);
CREATE TABLE agents (id TEXT PRIMARY KEY, display_name TEXT NOT NULL DEFAULT '', model TEXT NOT NULL DEFAULT '', role TEXT NOT NULL DEFAULT '', capabilities TEXT NOT NULL DEFAULT '[]', permissions TEXT NOT NULL DEFAULT '[]', parent_id TEXT, status TEXT NOT NULL DEFAULT 'idle', heartbeat_ts TEXT, created_ts TEXT NOT NULL, updated_ts TEXT NOT NULL, meta TEXT NOT NULL DEFAULT '{}');
CREATE TABLE messages (id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT NOT NULL, sender TEXT NOT NULL, recipient TEXT, subject TEXT NOT NULL DEFAULT '', body TEXT NOT NULL DEFAULT '', thread TEXT NOT NULL DEFAULT '', priority TEXT NOT NULL DEFAULT 'normal', requires_ack INTEGER NOT NULL DEFAULT 0);
CREATE TABLE message_receipts (message_id INTEGER NOT NULL, agent_id TEXT NOT NULL, read_ts TEXT, ack_ts TEXT, PRIMARY KEY (message_id, agent_id));
CREATE TABLE cursors (agent_id TEXT PRIMARY KEY, last_read_message_id INTEGER NOT NULL DEFAULT 0);
CREATE TABLE tasks (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL, description TEXT NOT NULL DEFAULT '', creator TEXT NOT NULL DEFAULT '', assignee TEXT, parent_id INTEGER, priority TEXT NOT NULL DEFAULT 'normal', status TEXT NOT NULL DEFAULT 'todo', acceptance TEXT NOT NULL DEFAULT '', artifacts TEXT NOT NULL DEFAULT '[]', created_ts TEXT NOT NULL, updated_ts TEXT NOT NULL);
CREATE TABLE task_notes (id INTEGER PRIMARY KEY AUTOINCREMENT, task_id INTEGER NOT NULL, author TEXT NOT NULL DEFAULT '', ts TEXT NOT NULL, note TEXT NOT NULL DEFAULT '');
CREATE TABLE task_dependencies (task_id INTEGER NOT NULL, depends_on_task_id INTEGER NOT NULL, PRIMARY KEY (task_id, depends_on_task_id));
CREATE TABLE events (id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT NOT NULL, actor TEXT NOT NULL DEFAULT '', kind TEXT NOT NULL, entity_type TEXT NOT NULL DEFAULT '', entity_id TEXT NOT NULL DEFAULT '', data TEXT NOT NULL DEFAULT '{}');
"#;

fn iso(offset: i64) -> String {
    chrono::DateTime::from_timestamp_millis(T0 - 86_400_000 + offset)
        .unwrap()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("qagent-rs-import-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn build_fixtures(dir: &Path) -> ImportSources {
    let state_path = dir.join("state.sqlite");
    let state = Connection::open(&state_path).unwrap();
    state.execute_batch("PRAGMA journal_mode = WAL").unwrap();
    state.execute_batch(QAGENT_SCHEMA).unwrap();
    let agent = |id: &str, role: &str| {
        serde_json::json!({
            "id": id, "role": role, "model": format!("{id}-model"), "family": "f",
            "provider": "p", "harness": "claude", "registeredAt": T0, "lastSeen": T0 + 5,
        })
        .to_string()
    };
    for (id, role) in [
        ("operator", "operator"),
        ("fable5", "manager"),
        ("bad id", "x"),
    ] {
        state
            .execute(
                "INSERT INTO agents VALUES(?, ?, ?)",
                params![id, agent(id, role), T0],
            )
            .unwrap();
    }
    state
        .execute(
            "INSERT INTO identities VALUES(?, ?, ?, ?, ?, ?)",
            params![
                "operator",
                hash_token("old-operator-token"),
                "operator",
                "{}",
                T0,
                T0
            ],
        )
        .unwrap();
    state
        .execute(
            "INSERT INTO identities VALUES(?, ?, ?, ?, ?, ?)",
            params![
                "fable5",
                hash_token("fable5-token"),
                "worker",
                "{\"canReview\":true}",
                T0,
                T0
            ],
        )
        .unwrap();
    let message = |id: &str, ts: i64, from: &str, to: &str, task_id: Option<&str>| {
        serde_json::json!({
            "id": id, "seq": 0, "ts": ts, "from": from, "to": to, "type": "task",
            "subject": format!("subject {id}"), "body": format!("body {id}"), "taskId": task_id,
        })
        .to_string()
    };
    state
        .execute(
            "INSERT INTO messages(id, to_agent, delivered, json, created_at) VALUES(?, ?, 1, ?, ?)",
            params![
                "msg_shared_1",
                "fable5",
                message(
                    "msg_shared_1",
                    T0 + 100,
                    "operator",
                    "fable5",
                    Some("task_a")
                ),
                T0 + 100
            ],
        )
        .unwrap();
    state
        .execute(
            "INSERT INTO messages(id, to_agent, delivered, json, created_at) VALUES(?, ?, 0, ?, ?)",
            params![
                "msg_state_only",
                "operator",
                message(
                    "msg_state_only",
                    T0 + 200,
                    "fable5",
                    "operator",
                    Some("task_a")
                ),
                T0 + 200
            ],
        )
        .unwrap();
    let task = |fields: serde_json::Value| {
        let mut base = serde_json::json!({
            "runId": null, "childTaskIds": [], "dependencyIds": [], "parentTaskId": null,
            "context": "", "contextRefs": [{"type": "path", "value": "src/a.ts"}],
            "assigner": "operator", "assignee": "fable5", "role": "research",
            "pathScopes": ["src"],
            "validationRequirements": [{"id": "v1", "description": "tests pass", "required": true}],
            "round": 1, "attempts": 0, "maxRetries": 2, "reviewerId": null,
            "result": null, "review": null, "createdAt": T0, "updatedAt": T0 + 300, "history": [],
        });
        for (key, value) in fields.as_object().unwrap() {
            base[key] = value.clone();
        }
        base.to_string()
    };
    state
        .execute(
            "INSERT INTO tasks VALUES(?, NULL, ?, ?, NULL, ?, ?)",
            params![
                "task_a",
                "submitted",
                "fable5",
                T0,
                task(serde_json::json!({
                    "id": "task_a", "title": "Task A", "brief": "brief A", "state": "submitted",
                    "result": {"summary": "done A", "details": "", "changedFiles": ["src/a.ts"], "artifacts": [], "validation": [], "completedAt": T0 + 250},
                    "history": [
                        {"ts": T0, "actor": "operator", "kind": "assigned", "state": "assigned", "note": "Task A"},
                        {"ts": T0 + 250, "actor": "fable5", "kind": "submitted", "state": "submitted", "note": "done A"},
                    ],
                })),
            ],
        )
        .unwrap();
    state
        .execute(
            "INSERT INTO tasks VALUES(?, NULL, ?, ?, ?, ?, ?)",
            params![
                "task_b",
                "in_progress",
                "fable5",
                "task_a",
                T0,
                task(serde_json::json!({
                    "id": "task_b", "title": "Task B", "brief": "brief B", "state": "in_progress",
                    "parentTaskId": "task_a", "dependencyIds": ["task_a"],
                    "history": [{"ts": T0 + 10, "actor": "operator", "kind": "assigned", "state": "assigned", "note": "Task B"}],
                })),
            ],
        )
        .unwrap();
    state
        .execute(
            "INSERT INTO path_leases VALUES('task_b', 'run', 'src', ?)",
            params![T0],
        )
        .unwrap();
    drop(state);

    let proto_path = dir.join("prototype.db");
    let proto = Connection::open(&proto_path).unwrap();
    proto.execute_batch(PROTOTYPE_SCHEMA).unwrap();
    proto
        .execute(
            "INSERT INTO agents(id, model, role, parent_id, created_ts, updated_ts) VALUES(?, ?, ?, ?, ?, ?)",
            params!["worker-1", "sonnet", "worker", "lead", iso(0), iso(0)],
        )
        .unwrap();
    proto
        .execute(
            "INSERT INTO agents(id, model, role, parent_id, created_ts, updated_ts) VALUES(?, ?, ?, ?, ?, ?)",
            params!["lead", "opus", "supervisor", Option::<String>::None, iso(0), iso(0)],
        )
        .unwrap();
    proto
        .execute(
            "INSERT INTO agents(id, model, role, parent_id, created_ts, updated_ts) VALUES(?, ?, ?, ?, ?, ?)",
            params!["bad agent", "", "", Option::<String>::None, iso(0), iso(0)],
        )
        .unwrap();
    proto
        .execute(
            "INSERT INTO messages(ts, sender, recipient, subject, body, thread) VALUES(?, ?, ?, ?, ?, ?)",
            params![iso(10), "lead", "worker-1", "go", "start", "t1"],
        )
        .unwrap();
    proto
        .execute(
            "INSERT INTO messages(ts, sender, recipient, subject, body, thread, requires_ack) VALUES(?, ?, NULL, ?, ?, ?, 1)",
            params![iso(20), "lead", "all", "broadcast body", ""],
        )
        .unwrap();
    proto
        .execute(
            "INSERT INTO messages(ts, sender, recipient, subject, body, thread) VALUES(?, ?, ?, ?, ?, ?)",
            params![iso(30), "worker-1", "lead", "ok", "on it", "t1"],
        )
        .unwrap();
    proto
        .execute(
            "INSERT INTO message_receipts VALUES(1, 'worker-1', ?, ?)",
            params![iso(11), iso(12)],
        )
        .unwrap();
    proto
        .execute(
            "INSERT INTO message_receipts VALUES(2, 'worker-1', ?, NULL)",
            params![iso(21)],
        )
        .unwrap();
    proto
        .execute("INSERT INTO cursors VALUES('worker-1', 2)", [])
        .unwrap();
    for (title, desc, assignee, parent, status, artifacts, created, updated) in [
        (
            "P1",
            "d1",
            Some("worker-1"),
            Option::<i64>::None,
            "review",
            "[\"out/report.html\"]",
            iso(40),
            iso(50),
        ),
        ("P2", "d2", None, Some(1i64), "todo", "[]", iso(41), iso(51)),
        (
            "P3",
            "d3",
            Some("worker-1"),
            None,
            "done",
            "[]",
            iso(42),
            iso(52),
        ),
        ("P4", "d4", None, None, "bogus", "[]", iso(43), iso(53)),
    ] {
        proto
            .execute(
                "INSERT INTO tasks(title, description, creator, assignee, parent_id, status, artifacts, created_ts, updated_ts) VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?)",
                params![title, desc, "lead", assignee, parent, status, artifacts, created, updated],
            )
            .unwrap();
    }
    proto
        .execute("INSERT INTO task_dependencies VALUES(2, 1)", [])
        .unwrap();
    proto
        .execute("INSERT INTO task_notes(task_id, author, ts, note) VALUES(1, 'worker-1', ?, 'note one')", params![iso(45)])
        .unwrap();
    proto
        .execute(
            "INSERT INTO task_notes(task_id, author, ts, note) VALUES(1, 'lead', ?, 'note two')",
            params![iso(46)],
        )
        .unwrap();
    proto
        .execute(
            "INSERT INTO events(ts, actor, kind, entity_type, entity_id, data) VALUES(?, 'lead', 'agent_registered', 'agent', 'lead', '{}')",
            params![iso(0)],
        )
        .unwrap();
    proto
        .execute(
            "INSERT INTO events(ts, actor, kind, entity_type, entity_id, data) VALUES(?, 'lead', 'task_created', 'task', '1', '{\"title\":\"P1\"}')",
            params![iso(40)],
        )
        .unwrap();
    proto
        .execute(
            "INSERT INTO events(ts, actor, kind, entity_type, entity_id, data) VALUES(?, 'lead', 'message_sent', 'message', '1', '{}')",
            params![iso(10)],
        )
        .unwrap();
    drop(proto);

    let jsonl_path = dir.join("bus.jsonl");
    let mut lines: Vec<String> = [
        serde_json::json!({"ts": T0, "kind": "register", "data": {"id": "fable5", "role": "manager", "model": "fable-5"}}),
        serde_json::json!({"ts": T0 + 1, "kind": "register", "data": {"id": "gpt", "role": "worker", "model": "gpt-5.6"}}),
        serde_json::json!({"ts": T0 + 2, "kind": "register", "data": {"id": "fable5", "role": "manager", "model": "fable-5", "harness": "claude"}}),
        serde_json::json!({"ts": T0 + 3, "kind": "register", "data": {"id": "bad id", "role": "worker", "model": "x"}}),
        serde_json::json!({"ts": T0 + 100, "kind": "message", "data": {"id": "msg_shared_1", "ts": T0 + 100, "from": "operator", "to": "fable5", "type": "task", "subject": "subject msg_shared_1", "body": "body msg_shared_1", "taskId": "task_a"}}),
        serde_json::json!({"ts": T0 + 400, "kind": "message", "data": {"id": "msg_jsonl_1", "seq": 7, "ts": T0 + 400, "from": "gpt", "to": "fable5", "type": "result", "subject": "s", "body": "b", "taskId": "task_a"}}),
        serde_json::json!({"ts": T0 + 500, "kind": "message", "data": {"id": "msg_jsonl_2", "ts": T0 + 500, "from": "fable5", "to": "gpt", "type": "feedback", "subject": "s2", "body": "b2", "taskId": null, "refs": [{"type": "path", "value": "x"}]}}),
        serde_json::json!({"ts": T0 + 90, "kind": "task_create", "data": {"id": "task_a", "title": "Task A", "brief": "long brief", "assigner": "operator", "assignee": "fable5", "state": "assigned"}}),
        serde_json::json!({"ts": T0 + 250, "kind": "task_submit", "data": {"taskId": "task_a", "actor": "fable5"}}),
        serde_json::json!({"ts": T0 + 600, "kind": "kill", "data": {"target": "gpt", "pid": 1, "killed": true, "by": "operator"}}),
    ]
    .iter()
    .map(|v| v.to_string())
    .collect();
    lines.insert(6, "{not json".to_string());
    fs::write(&jsonl_path, format!("{}\n", lines.join("\n"))).unwrap();

    ImportSources {
        jsonl: Some(jsonl_path),
        qagent_state: Some(state_path),
        prototype: Some(proto_path),
    }
}

/// Independent counts of the fixture sources, read without the importer.
fn source_counts(paths: &ImportSources) -> HashMap<&'static str, HashMap<String, i64>> {
    let mut out: HashMap<&'static str, HashMap<String, i64>> = HashMap::new();
    let jsonl = paths.jsonl.as_ref().unwrap();
    let content = fs::read_to_string(jsonl).unwrap();
    let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
    let parsed: Vec<serde_json::Value> = lines
        .iter()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let kind_is = |v: &serde_json::Value, k: &str| v["kind"].as_str() == Some(k);
    let mut agents = std::collections::HashSet::new();
    for v in &parsed {
        if kind_is(v, "register") {
            if let Some(id) = v["data"]["id"].as_str() {
                if !id.is_empty()
                    && id
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
                {
                    agents.insert(id.to_string());
                }
            }
        }
    }
    out.insert(
        "bus.jsonl",
        HashMap::from([
            ("lines".to_string(), lines.len() as i64),
            (
                "messages".to_string(),
                parsed.iter().filter(|v| kind_is(v, "message")).count() as i64,
            ),
            (
                "registrations".to_string(),
                parsed.iter().filter(|v| kind_is(v, "register")).count() as i64,
            ),
            ("agents".to_string(), agents.len() as i64),
            (
                "events".to_string(),
                parsed
                    .iter()
                    .filter(|v| !kind_is(v, "message") && !kind_is(v, "register"))
                    .count() as i64,
            ),
        ]),
    );

    let count = |db: &Connection, sql: &str| -> i64 {
        db.query_row(sql, [], |row| row.get::<_, i64>(0)).unwrap()
    };
    let state = Connection::open_with_flags(
        paths.qagent_state.as_ref().unwrap(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let mut history = 0i64;
    let mut dependencies = 0i64;
    {
        let mut stmt = state.prepare("SELECT json FROM tasks").unwrap();
        let rows = stmt.query_map([], |row| row.get::<_, String>(0)).unwrap();
        for row in rows.flatten() {
            let v: serde_json::Value = serde_json::from_str(&row).unwrap();
            history += v["history"].as_array().map(|a| a.len() as i64).unwrap_or(0);
            dependencies += v["dependencyIds"]
                .as_array()
                .map(|a| a.len() as i64)
                .unwrap_or(0);
        }
    }
    out.insert(
        "qagent",
        HashMap::from([
            (
                "agents".to_string(),
                count(&state, "SELECT COUNT(*) FROM agents"),
            ),
            (
                "identities".to_string(),
                count(&state, "SELECT COUNT(*) FROM identities"),
            ),
            (
                "messages".to_string(),
                count(&state, "SELECT COUNT(*) FROM messages"),
            ),
            (
                "tasks".to_string(),
                count(&state, "SELECT COUNT(*) FROM tasks"),
            ),
            ("history".to_string(), history),
            ("dependencies".to_string(), dependencies),
        ]),
    );

    let proto = Connection::open_with_flags(
        paths.prototype.as_ref().unwrap(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    out.insert(
        "prototype",
        HashMap::from([
            (
                "agents".to_string(),
                count(&proto, "SELECT COUNT(*) FROM agents"),
            ),
            (
                "messages".to_string(),
                count(&proto, "SELECT COUNT(*) FROM messages"),
            ),
            (
                "receipts".to_string(),
                count(&proto, "SELECT COUNT(*) FROM message_receipts"),
            ),
            (
                "acks".to_string(),
                count(
                    &proto,
                    "SELECT COUNT(*) FROM message_receipts WHERE ack_ts IS NOT NULL",
                ),
            ),
            (
                "tasks".to_string(),
                count(&proto, "SELECT COUNT(*) FROM tasks"),
            ),
            (
                "dependencies".to_string(),
                count(&proto, "SELECT COUNT(*) FROM task_dependencies"),
            ),
            (
                "notes".to_string(),
                count(&proto, "SELECT COUNT(*) FROM task_notes"),
            ),
            (
                "events".to_string(),
                count(&proto, "SELECT COUNT(*) FROM events"),
            ),
        ]),
    );
    out
}

/// sha256 over every table name + every row ordered by rowid — change detector.
fn db_digest(path: &Path) -> String {
    let db = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let mut hasher = Sha256::new();
    let mut tables: Vec<String> = db
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .flatten()
        .collect();
    tables.sort();
    for name in tables {
        hasher.update(name.as_bytes());
        let mut stmt = db
            .prepare(&format!("SELECT * FROM \"{name}\" ORDER BY rowid"))
            .unwrap();
        let col_count = stmt.column_count();
        let mut rows = stmt.query([]).unwrap();
        while let Some(row) = rows.next().unwrap() {
            for i in 0..col_count {
                let v = row.get::<_, rusqlite::types::Value>(i).unwrap();
                hasher.update(format!("{v:?}|").as_bytes());
            }
        }
    }
    format!("{:x}", hasher.finalize())
}

fn sum(record: &HashMap<String, u64>) -> u64 {
    record.values().sum()
}

#[test]
fn importer_dry_run_counts_match_and_reruns_change_nothing() {
    let dir = scratch("run");
    let src_dir = dir.join("src-a");
    fs::create_dir_all(&src_dir).unwrap();
    let paths = build_fixtures(&src_dir);
    let expected = source_counts(&paths);

    // Dry run against a database that does not exist yet: exact source counts, nothing created.
    let fresh_path = dir.join("fresh/bus.db");
    let fresh = run_import(
        &fresh_path,
        ImportSources {
            jsonl: paths.jsonl.clone(),
            qagent_state: paths.qagent_state.clone(),
            prototype: paths.prototype.clone(),
        },
        ImportOptions {
            dry_run: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!fresh_path.exists(), "a dry run does not create bus.db");
    let fresh_by_kind: HashMap<&str, &acs::import::SourceReport> =
        fresh.sources.iter().map(|s| (s.kind.as_str(), s)).collect();
    for kind in ["bus.jsonl", "qagent", "prototype"] {
        for (key, n) in &expected[kind] {
            assert_eq!(
                fresh_by_kind[kind].read.get(key).copied().unwrap_or(0) as i64,
                *n,
                "{kind} read.{key}"
            );
        }
    }
    assert_eq!(fresh_by_kind["bus.jsonl"].invalid["lines"], 1);
    assert_eq!(fresh_by_kind["bus.jsonl"].invalid["registrations"], 1);
    assert_eq!(fresh_by_kind["qagent"].invalid["agents"], 1);
    assert_eq!(fresh_by_kind["prototype"].invalid["agents"], 1);
    assert_eq!(fresh_by_kind["prototype"].invalid["tasks"], 1);
    assert_eq!(fresh_by_kind["qagent"].inserted["messages"], 2);
    assert_eq!(fresh_by_kind["bus.jsonl"].inserted["messages"], 2);
    assert_eq!(
        fresh_by_kind["bus.jsonl"].duplicates["messages"], 1,
        "a message present in state.sqlite and bus.jsonl is stored once"
    );

    // Initialise the real target, as the migration does, then dry-run it: content unchanged.
    let home = dir.join("home");
    let db_path = home.join("bus.db");
    fs::create_dir_all(&home).unwrap();
    {
        let bus = Bus::open(Some(&db_path)).unwrap();
        bus.init().unwrap();
    }
    let tokens = home.join("tokens");
    fs::create_dir_all(&tokens).unwrap();
    fs::write(tokens.join("fable5.token"), "fable5-token\n").unwrap();
    let initial = db_digest(&db_path);
    let dry = run_import(
        &db_path,
        ImportSources {
            jsonl: paths.jsonl.clone(),
            qagent_state: paths.qagent_state.clone(),
            prototype: paths.prototype.clone(),
        },
        ImportOptions {
            dry_run: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        db_digest(&db_path),
        initial,
        "a dry run leaves bus.db unchanged"
    );

    let real = run_import(
        &db_path,
        ImportSources {
            jsonl: paths.jsonl.clone(),
            qagent_state: paths.qagent_state.clone(),
            prototype: paths.prototype.clone(),
        },
        ImportOptions::default(),
    )
    .unwrap();
    for source in &real.sources {
        let plan = dry.sources.iter().find(|s| s.kind == source.kind).unwrap();
        assert_eq!(
            source.inserted, plan.inserted,
            "{}: import matches the dry run",
            source.kind
        );
        assert_eq!(source.duplicates, plan.duplicates);
    }
    let after: HashMap<&str, &acs::import::SourceReport> =
        real.sources.iter().map(|s| (s.kind.as_str(), s)).collect();
    assert_eq!(
        after["qagent"].duplicates["agents"], 1,
        "the operator row from init is kept"
    );
    assert_eq!(
        after["qagent"].duplicates["identities"], 1,
        "the operator identity from init is kept"
    );
    assert_eq!(after["qagent"].inserted["identities"], 1);

    let check = Bus::open(Some(&db_path)).unwrap();
    let count = |sql: &str| -> i64 {
        check
            .conn
            .query_row(sql, [], |row| row.get::<_, i64>(0))
            .unwrap()
    };
    assert_eq!(count("SELECT COUNT(*) FROM messages"), 2 + 3 + 2);
    assert_eq!(
        count("SELECT COUNT(*) FROM messages WHERE recipient IS NULL AND source = 'prototype'"),
        1,
        "broadcast kept"
    );
    assert_eq!(count("SELECT COUNT(*) FROM tasks"), 2 + 3);
    assert_eq!(count("SELECT COUNT(*) FROM task_deps"), 2);
    assert_eq!(count("SELECT COUNT(*) FROM task_notes"), 2);
    assert_eq!(count("SELECT COUNT(*) FROM acks"), 1);
    assert_eq!(count("SELECT COUNT(*) FROM agents"), 5);
    assert_eq!(
        count("SELECT COUNT(*) FROM events WHERE source <> 'v2'"),
        3 + 3 + 3
    );
    let max_seq = count("SELECT MAX(seq) FROM messages");
    assert_eq!(real.cursor_seq, max_seq);
    assert_eq!(
        count(&format!(
            "SELECT COUNT(*) FROM cursors WHERE last_seq = {max_seq}"
        )),
        5,
        "every agent's cursor is at the newest imported message"
    );
    let states: HashMap<String, String> = check
        .conn
        .prepare("SELECT legacy_id, state FROM tasks")
        .unwrap()
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .unwrap()
        .flatten()
        .collect();
    assert_eq!(
        states,
        HashMap::from([
            ("task_a".to_string(), "submitted".to_string()),
            ("task_b".to_string(), "claimed".to_string()),
            ("prototype:1".to_string(), "submitted".to_string()),
            ("prototype:2".to_string(), "open".to_string()),
            ("prototype:3".to_string(), "accepted".to_string()),
        ])
    );
    let task_a_id: i64 = check
        .conn
        .query_row("SELECT id FROM tasks WHERE legacy_id = 'task_a'", [], |r| {
            r.get(0)
        })
        .unwrap();
    let task_b_id: i64 = check
        .conn
        .query_row("SELECT id FROM tasks WHERE legacy_id = 'task_b'", [], |r| {
            r.get(0)
        })
        .unwrap();
    let task_b = check.get_task(task_b_id).unwrap();
    assert_eq!(task_b.task.parent_id, Some(task_a_id));
    assert_eq!(task_b.task.dependencies, vec![task_a_id]);
    assert!(
        task_b.task.claim_expires_ms.is_some(),
        "an imported in-progress claim gets an expiry so the next board write can reopen it"
    );
    assert_eq!(
        check
            .get_agent("worker-1")
            .unwrap()
            .unwrap()
            .parent_id
            .as_deref(),
        Some("lead")
    );
    assert_eq!(
        check
            .get_messages(Some(0), Some(100), None, None)
            .unwrap()
            .iter()
            .filter(|m| m.task_id == Some(task_a_id))
            .count(),
        3
    );
    assert_eq!(
        check.identify(Some("fable5")).unwrap().agent_id,
        "fable5",
        "an imported identity keeps its existing token file working"
    );
    assert_eq!(
        check.unread_count("fable5").unwrap(),
        0,
        "no agent wakes to imported history"
    );
    drop(check);

    let imported = db_digest(&db_path);
    let rerun = run_import(
        &db_path,
        ImportSources {
            jsonl: paths.jsonl.clone(),
            qagent_state: paths.qagent_state.clone(),
            prototype: paths.prototype.clone(),
        },
        ImportOptions::default(),
    )
    .unwrap();
    for source in &rerun.sources {
        assert!(
            source.already_imported,
            "{} recognised by sha256",
            source.kind
        );
        assert_eq!(sum(&source.inserted), 0);
    }
    assert_eq!(
        db_digest(&db_path),
        imported,
        "a second run changes nothing"
    );
    let forced = run_import(
        &db_path,
        ImportSources {
            jsonl: paths.jsonl.clone(),
            qagent_state: paths.qagent_state.clone(),
            prototype: paths.prototype.clone(),
        },
        ImportOptions {
            force: true,
            ..Default::default()
        },
    )
    .unwrap();
    for source in &forced.sources {
        assert_eq!(
            sum(&source.inserted),
            0,
            "{} --force inserts nothing new",
            source.kind
        );
    }
    assert_eq!(
        db_digest(&db_path),
        imported,
        "a forced rerun changes nothing either"
    );

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn cli_import_dry_run_json_reports_counts_and_creates_nothing() {
    let dir = scratch("cli");
    let src_dir = dir.join("src-b");
    fs::create_dir_all(&src_dir).unwrap();
    let paths = build_fixtures(&src_dir);
    let expected = source_counts(&paths);

    let db_path = dir.join("cli-home/bus.db");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_qagent"))
        .args([
            "import",
            "--json",
            "--dry-run",
            "--jsonl",
            paths.jsonl.as_ref().unwrap().to_str().unwrap(),
            "--qagent-state",
            paths.qagent_state.as_ref().unwrap().to_str().unwrap(),
            "--prototype",
            paths.prototype.as_ref().unwrap().to_str().unwrap(),
        ])
        .env("QAGENT_BUS_DB", &db_path)
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(!db_path.exists(), "a CLI dry run does not create bus.db");
    let by_kind: HashMap<&str, &serde_json::Value> = report["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| (s["kind"].as_str().unwrap(), s))
        .collect();
    for kind in ["bus.jsonl", "qagent", "prototype"] {
        for (key, n) in &expected[kind] {
            assert_eq!(
                by_kind[kind]["read"][key].as_i64().unwrap_or(0),
                *n,
                "{kind} read.{key}"
            );
        }
    }
    assert_eq!(report["dryRun"], true);

    fs::remove_dir_all(&dir).ok();
}
