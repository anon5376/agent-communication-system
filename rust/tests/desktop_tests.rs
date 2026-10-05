//! The acs-desktop JSON bridge: one request in, one reply out. Every call goes
//! through `desktop::respond`, the same path the binary's stdin/stdout takes.
//! No real agent CLI ever runs: `start` is exercised against a fake `qagent`
//! script in a temp dir.

use acs::aos::{crew, demo};
use acs::bus::{Bus, ListTasksInput, SubmitInput};
use acs::desktop;
use acs::types::OPERATOR_ID;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

fn temp_dir(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "acs-desktop-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

fn temp_db(tag: &str) -> PathBuf {
    temp_dir(tag).join("bus.db")
}

/// The full wire call: request JSON -> (parsed reply, exit code).
fn call_raw(request: &Value) -> (Value, i32) {
    let (body, code) = desktop::respond(&request.to_string());
    let reply: Value = serde_json::from_str(&body).expect("reply is one JSON object");
    (reply, code)
}

fn call(db: &Path, action: &str, payload: Value) -> (Value, i32) {
    call_raw(&json!({
        "version": 1,
        "dbPath": db.display().to_string(),
        "action": action,
        "payload": payload,
    }))
}

fn ok(reply: &Value, code: i32) -> Value {
    assert_eq!(code, 0, "ok replies exit 0: {reply}");
    assert_eq!(reply["ok"], true, "{reply}");
    reply["data"].clone()
}

fn fail(reply: &Value, code: i32) -> Value {
    assert_ne!(code, 0, "error replies exit nonzero: {reply}");
    assert_eq!(reply["ok"], false, "{reply}");
    reply["error"].clone()
}

fn init_bus(db: &Path) {
    let (reply, code) = call(db, "init", json!({}));
    let data = ok(&reply, code);
    assert!(
        data["message"].as_str().unwrap().contains("operator"),
        "{data}"
    );
}

// ------------------------------------------------------------- envelope

#[test]
fn rejects_non_json_unknown_version_and_unknown_action() {
    let (body, code) = desktop::respond("not json");
    let reply: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(fail(&reply, code)["code"], "invalid");

    let db = temp_db("ver");
    let (reply, code) = call_raw(&json!({
        "version": 2,
        "dbPath": db.display().to_string(),
        "action": "snapshot",
        "payload": {},
    }));
    assert_eq!(fail(&reply, code)["code"], "invalid");

    let (reply, code) = call(&db, "explode", json!({}));
    assert_eq!(fail(&reply, code)["code"], "invalid");
}

#[test]
fn db_path_must_be_absolute() {
    let (reply, code) = call_raw(&json!({
        "version": 1,
        "dbPath": "relative/bus.db",
        "action": "snapshot",
        "payload": {},
    }));
    assert_eq!(fail(&reply, code)["code"], "invalid");
}

#[test]
fn snapshot_on_a_missing_db_creates_nothing() {
    let db = temp_db("missing");
    let (reply, code) = call(&db, "snapshot", json!({}));
    let error = fail(&reply, code);
    assert_eq!(error["code"], "not_found", "{error}");
    assert!(!db.exists(), "a snapshot must never grow a database");
    // The parent directory the open would have made stays absent too.
    assert!(!db.parent().unwrap().exists());
}

#[test]
fn snapshot_rejects_a_file_that_is_not_a_bus() {
    let dir = temp_dir("foreign");
    std::fs::create_dir_all(&dir).unwrap();

    // An unrelated SQLite database: no ACS tables may be grafted on.
    let foreign = dir.join("cookies.db");
    let conn = rusqlite::Connection::open(&foreign).unwrap();
    conn.execute_batch("CREATE TABLE cookies (name TEXT)")
        .unwrap();
    drop(conn);
    let (reply, code) = call(&foreign, "snapshot", json!({}));
    assert_eq!(fail(&reply, code)["code"], "invalid");
    let conn = rusqlite::Connection::open(&foreign).unwrap();
    let acs_tables: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name IN ('agents','tasks','messages')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(acs_tables, 0, "snapshot grew ACS tables in a foreign db");
    let cookie_rows: i64 = conn
        .query_row("SELECT count(*) FROM cookies", [], |row| row.get(0))
        .unwrap();
    assert_eq!(cookie_rows, 0);
    drop(conn);

    // An empty file is a path mistake too, and init refuses to claim it.
    let empty = dir.join("empty.db");
    std::fs::write(&empty, b"").unwrap();
    let (reply, code) = call(&empty, "snapshot", json!({}));
    assert_eq!(fail(&reply, code)["code"], "invalid");
    assert_eq!(std::fs::metadata(&empty).unwrap().len(), 0);

    let (reply, code) = call(&foreign, "init", json!({}));
    assert_eq!(fail(&reply, code)["code"], "conflict");
}

#[test]
fn init_then_snapshot_reports_operable() {
    let db = temp_db("init");
    init_bus(&db);
    let (reply, code) = call(&db, "snapshot", json!({}));
    let data = ok(&reply, code);
    assert_eq!(data["dbPath"], db.display().to_string());
    assert_eq!(data["canOperate"], true);
    assert_eq!(data["simulated"], false);
    assert_eq!(data["truncated"], false);
    let agents = data["agents"].as_array().unwrap();
    let operator = agents
        .iter()
        .find(|a| a["id"] == OPERATOR_ID)
        .expect("operator in agents");
    // The record is the public shape only: no meta, authority or token fields.
    for key in [
        "id",
        "role",
        "model",
        "harness",
        "status",
        "lastSeenMs",
        "running",
        "paused",
    ] {
        assert!(operator.get(key).is_some(), "missing {key}");
    }
    for key in ["meta", "authority", "parentId", "token"] {
        assert!(operator.get(key).is_none(), "leaked {key}");
    }
}

#[test]
fn a_bus_without_operator_reports_cannot_operate() {
    let dir = temp_dir("no-operator");
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("bus.db");
    Bus::open(Some(&db)).unwrap(); // schema only, no operator identity
    let (reply, code) = call(&db, "snapshot", json!({}));
    let data = ok(&reply, code);
    assert_eq!(data["canOperate"], false);
    // And mutations refuse rather than making an operator on the sly.
    let (reply, code) = call(
        &db,
        "createTask",
        json!({"title": "work", "priority": "normal"}),
    );
    assert_eq!(fail(&reply, code)["code"], "unauthorized");
}

// ------------------------------------------------------------------ demo

#[test]
fn demo_seeds_simulated_and_start_is_denied() {
    let dir = temp_dir("demo");
    let db = dir.join("bus.db");
    let (reply, code) = call(&db, "demo", json!({}));
    ok(&reply, code);
    assert!(demo::is_simulated(&dir));

    let (reply, code) = call(&db, "snapshot", json!({}));
    let data = ok(&reply, code);
    assert_eq!(data["simulated"], true);
    assert_eq!(data["canOperate"], true, "the demo bus has an operator");

    let workdir = temp_dir("demo-work");
    std::fs::create_dir_all(&workdir).unwrap();
    let (reply, code) = call(
        &db,
        "start",
        json!({"ids": ["builder"], "workdir": workdir.display().to_string(), "confirmed": true}),
    );
    let error = fail(&reply, code);
    assert!(
        error["message"].as_str().unwrap().contains("simulated"),
        "{error}"
    );

    // demo refuses an existing destination, simulated or not.
    let (reply, code) = call(&db, "demo", json!({}));
    assert_eq!(fail(&reply, code)["code"], "conflict");
}

// --------------------------------------------------------------- lifecycle

#[test]
fn task_lifecycle_through_bus_and_bridge() {
    let db = temp_db("lifecycle");
    init_bus(&db);

    let (reply, code) = call(
        &db,
        "createTask",
        json!({
            "title": "build a widget",
            "brief": "make the widget",
            "acceptance": "widget exists",
            "to": "worker-1",
        }),
    );
    // worker-1 does not exist yet: the bus says so honestly.
    assert_eq!(fail(&reply, code)["code"], "not_found");

    let bus = Bus::open(Some(&db)).unwrap();
    let operator = bus.identify(Some(OPERATOR_ID)).unwrap();
    bus.add_agent(
        &operator,
        "worker-1",
        Some("worker"),
        Some("test-model"),
        Some("test"),
        None,
        Some("worker"),
    )
    .unwrap();
    let worker = bus.identify(Some("worker-1")).unwrap();

    let (reply, code) = call(
        &db,
        "createTask",
        json!({
            "title": "build a widget",
            "brief": "make the widget",
            "acceptance": "widget exists",
            "to": "worker-1",
            "priority": "high",
        }),
    );
    ok(&reply, code);

    let task = bus
        .list_tasks(ListTasksInput {
            include_closed: true,
            ..Default::default()
        })
        .unwrap()
        .into_iter()
        .find(|t| t.title == "build a widget")
        .unwrap();
    assert_eq!(task.reviewer.as_deref(), Some(OPERATOR_ID));
    assert_eq!(task.priority, "high");

    bus.claim_task(&worker, Some(task.id)).unwrap();
    bus.submit_task(
        &worker,
        task.id,
        SubmitInput {
            summary: "widget built".into(),
            details: Some("src/widget.rs".into()),
            changed_files: vec!["src/widget.rs".into()],
            validation: Some(
                json!([{"command": "cargo check", "passed": true, "summary": "clean"}]),
            ),
            ..Default::default()
        },
    )
    .unwrap();
    bus.note_task(&worker, task.id, "a note from the floor")
        .unwrap();

    let (reply, code) = call(
        &db,
        "reviewTask",
        json!({"id": task.id, "accept": true, "feedback": "ship it"}),
    );
    ok(&reply, code);

    let (reply, code) = call(&db, "task", json!({"id": task.id}));
    let detail = ok(&reply, code);
    assert_eq!(detail["state"], "accepted");
    assert_eq!(detail["result"]["summary"], "widget built");
    assert_eq!(detail["result"]["changedFiles"], json!(["src/widget.rs"]));
    assert_eq!(detail["result"]["validation"][0]["passed"], true);
    assert_eq!(detail["review"]["reviewer"], OPERATOR_ID);
    assert_eq!(detail["review"]["accepted"], true);
    assert_eq!(detail["review"]["feedback"], "ship it");
    assert!(
        detail["notes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["body"] == "a note from the floor" && n["author"] == "worker-1"),
        "{detail}"
    );
    assert!(detail["messages"].as_array().is_some());

    // Snapshot sees the same closed task, newest first.
    let (reply, code) = call(&db, "snapshot", json!({}));
    let data = ok(&reply, code);
    let tasks = data["tasks"].as_array().unwrap();
    assert_eq!(tasks[0]["id"], task.id);
    assert_eq!(tasks[0]["state"], "accepted");

    // Unknown task id is an honest error.
    let (reply, code) = call(&db, "task", json!({"id": 9999}));
    assert_eq!(fail(&reply, code)["code"], "not_found");
}

#[test]
fn send_pause_resume_requeue_cancel() {
    let db = temp_db("controls");
    init_bus(&db);
    let bus = Bus::open(Some(&db)).unwrap();
    let operator = bus.identify(Some(OPERATOR_ID)).unwrap();
    bus.add_agent(&operator, "a-1", None, None, None, None, None)
        .unwrap();
    let task = bus
        .create_task(
            &operator,
            acs::bus::CreateTaskInput {
                title: "queued work".into(),
                to: Some("a-1".into()),
                ..Default::default()
            },
        )
        .unwrap();

    let (reply, code) = call(
        &db,
        "send",
        json!({"to": "a-1", "subject": "ping", "body": "hello"}),
    );
    ok(&reply, code);

    let a1 = bus.identify(Some("a-1")).unwrap();
    bus.claim_task(&a1, Some(task.id)).unwrap();

    let (reply, code) = call(&db, "pause", json!({"id": "a-1"}));
    ok(&reply, code);
    let (reply, code) = call(&db, "snapshot", json!({}));
    let data = ok(&reply, code);
    let a = data["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == "a-1")
        .unwrap();
    assert_eq!(a["paused"], true);
    assert_eq!(a["running"], false);

    let (reply, code) = call(&db, "resume", json!({"id": "a-1"}));
    ok(&reply, code);
    assert!(bus
        .get_agent("a-1")
        .unwrap()
        .unwrap()
        .meta
        .get("paused")
        .is_none());

    let (reply, code) = call(&db, "requeue", json!({"id": task.id, "reason": "busy"}));
    ok(&reply, code);
    let task = bus.get_task(task.id).unwrap().task;
    assert_eq!(task.state, "open");
    assert_eq!(task.assignee, None);

    let (reply, code) = call(
        &db,
        "cancel",
        json!({"id": task.id, "reason": "no longer needed"}),
    );
    ok(&reply, code);
    assert_eq!(bus.get_task(task.id).unwrap().task.state, "cancelled");

    // A second cancel is a conflict, not a success.
    let (reply, code) = call(&db, "cancel", json!({"id": task.id, "reason": "again"}));
    assert_eq!(fail(&reply, code)["code"], "conflict");
}

#[test]
fn setup_start_stop_require_the_operator_before_any_writes() {
    let dir = temp_dir("gated");
    let db = dir.join("bus.db");
    init_bus(&db);
    std::fs::remove_file(dir.join("operator.token")).unwrap();

    let workdir = temp_dir("gated-work");
    std::fs::create_dir_all(&workdir).unwrap();

    let (reply, code) = call(&db, "setup", json!({}));
    assert_eq!(fail(&reply, code)["code"], "unauthorized");
    let (reply, code) = call(
        &db,
        "start",
        json!({"ids": ["builder"], "workdir": workdir.display().to_string(), "confirmed": true}),
    );
    assert_eq!(fail(&reply, code)["code"], "unauthorized");
    let (reply, code) = call(&db, "stop", json!({"ids": ["builder"]}));
    assert_eq!(fail(&reply, code)["code"], "unauthorized");

    // No aos config, presets, trust file or supervisor records were written.
    assert!(!dir.join("aos").exists(), "setup wrote into {dir:?}");
    assert!(!dir.join("supervisors").exists());
}

// ------------------------------------------------------------------ detect

#[test]
fn detect_lists_providers_without_a_bus() {
    let db = temp_db("detect");
    let (reply, code) = call(&db, "detect", json!({}));
    let providers = ok(&reply, code);
    let providers = providers.as_array().unwrap();
    assert_eq!(providers.len(), crew::CLIS.len());
    let claude = providers.iter().find(|p| p["id"] == "claude").unwrap();
    assert_eq!(claude["name"], "Claude Code");
    assert_eq!(claude["requiresApproval"], false);
    for p in providers {
        for key in ["id", "name", "signIn", "requiresApproval"] {
            assert!(p.get(key).is_some(), "provider missing {key}: {p}");
        }
        assert!(p.get("path").is_some());
    }
}

// ------------------------------------------------------------------ start

/// A fake `qagent` that does what a supervisor's first breath does: writes its
/// pid file under <home>/supervisors/, then stays alive so `running` is true.
#[cfg(unix)]
fn fake_qagent(dir: &Path) -> PathBuf {
    let script = dir.join("qagent");
    std::fs::write(
        &script,
        "#!/bin/sh\n\
         db=\"\"\n\
         id=\"\"\n\
         while [ $# -gt 0 ]; do\n\
         \x20 case \"$1\" in\n\
         \x20   --db) db=\"$2\"; shift 2;;\n\
         \x20   supervise) id=\"$2\"; shift 2;;\n\
         \x20   *) shift;;\n\
         \x20 esac\n\
         done\n\
         home=$(dirname \"$db\")\n\
         mkdir -p \"$home/supervisors\"\n\
         echo $$ > \"$home/supervisors/$id.pid\"\n\
         sleep 30 &\n\
         wait\n",
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    script
}

/// Windows: the same fake as a .cmd shim wrapping a PowerShell body (cmd.exe
/// cannot report its own pid). Besides staying alive it honours the stop
/// convention a detached supervisor watches for: <id>.stop next to <id>.pid.
#[cfg(windows)]
fn fake_qagent(dir: &Path) -> PathBuf {
    let ps1 = dir.join("qagent.ps1");
    std::fs::write(
        &ps1,
        "$db=\"\"; $id=\"\"\r\n\
         for ($i=0; $i -lt $args.Count; $i++) {\r\n\
         \x20 if ($args[$i] -eq \"--db\") { $db=$args[$i+1]; $i++ }\r\n\
         \x20 elseif ($args[$i] -eq \"supervise\") { $id=$args[$i+1]; $i++ }\r\n\
         }\r\n\
         $busHome = Split-Path $db -Parent\r\n\
         New-Item -ItemType Directory -Force \"$busHome\\supervisors\" | Out-Null\r\n\
         $pidFile = \"$busHome\\supervisors\\$id.pid\"\r\n\
         Set-Content $pidFile \"$PID`n\"\r\n\
         $stopFile = \"$busHome\\supervisors\\$id.stop\"\r\n\
         $deadline = (Get-Date).AddSeconds(120)\r\n\
         while ((Get-Date) -lt $deadline) {\r\n\
         \x20 if (Test-Path $stopFile) {\r\n\
         \x20   Remove-Item $stopFile,$pidFile -Force -ErrorAction SilentlyContinue\r\n\
         \x20   exit 0\r\n\
         \x20 }\r\n\
         \x20 Start-Sleep -Milliseconds 200\r\n\
         }\r\n",
    )
    .unwrap();
    let script = dir.join("qagent.cmd");
    std::fs::write(
        &script,
        format!(
            "@echo off\r\npowershell -NoProfile -ExecutionPolicy Bypass -File \"{}\" %*\r\n",
            ps1.display()
        ),
    )
    .unwrap();
    script
}

fn crew_with(paths: &crew::Paths, bus: &Bus, cli_ids: &[&str]) {
    let found: Vec<crew::Found> = crew::CLIS
        .iter()
        .map(|cli| crew::Found {
            cli,
            path: cli_ids
                .contains(&cli.id)
                .then(|| PathBuf::from("/bin/true")),
            version: None,
            sign_in: crew::SignIn::Found("test".into()),
        })
        .collect();
    crew::setup(bus, paths, &found, false).unwrap();
}

#[test]
fn start_then_stop_with_a_fake_qagent() {
    let dir = temp_dir("start");
    let db = dir.join("bus.db");
    init_bus(&db);
    let paths = crew::Paths::for_db(&db);
    let bus = Bus::open(Some(&db)).unwrap();
    crew_with(&paths, &bus, &["claude"]); // one family: builder only
    drop(bus);

    let bin_dir = temp_dir("start-bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let fake = fake_qagent(&bin_dir);
    std::env::set_var("ACS_DESKTOP_QAGENT", &fake);

    let workdir = temp_dir("start-work");
    std::fs::create_dir_all(&workdir).unwrap();
    let workdir = workdir.display().to_string();

    // No confirmation, no start.
    let (reply, code) = call(
        &db,
        "start",
        json!({"ids": ["builder"], "workdir": workdir, "confirmed": false}),
    );
    assert_eq!(fail(&reply, code)["code"], "invalid");

    // An id outside the crew is refused before anything spawns.
    let (reply, code) = call(
        &db,
        "start",
        json!({"ids": ["ghost"], "workdir": workdir, "confirmed": true}),
    );
    assert_eq!(fail(&reply, code)["code"], "invalid");

    let (reply, code) = call(
        &db,
        "start",
        json!({"ids": ["builder"], "workdir": workdir, "confirmed": true}),
    );
    let data = ok(&reply, code);
    assert!(
        data["message"]
            .as_str()
            .unwrap()
            .contains("builder running"),
        "{data}"
    );
    // The workdir was trusted by the confirmed start.
    assert!(crew::is_trusted(&paths, &workdir_canonical(&workdir)));

    // The fake writes its pid file on its own clock (a cold PowerShell start
    // on Windows can outlive start's 1.2s wait): poll the snapshot.
    let mut builder = json!(null);
    for _ in 0..100 {
        let (reply, code) = call(&db, "snapshot", json!({}));
        let data = ok(&reply, code);
        let found = data["agents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["id"] == "builder")
            .cloned()
            .unwrap();
        builder = found;
        if builder["running"] == true {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert_eq!(builder["running"], true, "{builder}");

    let (reply, code) = call(&db, "stop", json!({"ids": ["builder"]}));
    let data = ok(&reply, code);
    assert!(
        data["message"].as_str().unwrap().contains("stopped"),
        "{data}"
    );

    let (reply, code) = call(&db, "snapshot", json!({}));
    let data = ok(&reply, code);
    let builder = data["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == "builder")
        .unwrap();
    assert_eq!(builder["running"], false, "{builder}");
}

fn workdir_canonical(dir: &str) -> PathBuf {
    std::fs::canonicalize(dir).unwrap()
}

// -------------------------------------------------------------- edge paths

#[test]
fn whitespace_paths_work() {
    let dir = temp_dir("white space dir");
    let spaced = dir.join("my bus files");
    let db = spaced.join("bus.db");
    init_bus(&db);
    let (reply, code) = call(&db, "snapshot", json!({}));
    let data = ok(&reply, code);
    assert_eq!(data["canOperate"], true);
}

#[test]
fn snapshot_orders_newest_first_and_truncates_honestly() {
    let db = temp_db("paging");
    init_bus(&db);
    let bus = Bus::open(Some(&db)).unwrap();
    let operator = bus.identify(Some(OPERATOR_ID)).unwrap();
    // One over the snapshot cap, so truncation must be reported.
    for i in 0..1001 {
        bus.create_task(
            &operator,
            acs::bus::CreateTaskInput {
                title: format!("task {i}"),
                ..Default::default()
            },
        )
        .unwrap();
    }
    let (reply, code) = call(&db, "snapshot", json!({}));
    let data = ok(&reply, code);
    assert_eq!(data["truncated"], true);
    let tasks = data["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 1000);
    let ids: Vec<i64> = tasks.iter().map(|t| t["id"].as_i64().unwrap()).collect();
    assert_eq!(ids[0], 1001, "newest first");
    assert_eq!(ids[999], 2, "the oldest task is truncated away");
    let mut sorted = ids.clone();
    sorted.sort_unstable_by(|a, b| b.cmp(a));
    assert_eq!(ids, sorted, "descending by id");
}

#[test]
fn snapshot_keeps_submitted_tasks_older_than_recent_page() {
    let db = temp_db("old-submission");
    init_bus(&db);
    let bus = Bus::open(Some(&db)).unwrap();
    let operator = bus.identify(Some(OPERATOR_ID)).unwrap();
    bus.add_agent(
        &operator,
        "review-worker",
        Some("worker"),
        Some("test-model"),
        Some("test"),
        None,
        Some("worker"),
    )
    .unwrap();
    let worker = bus.identify(Some("review-worker")).unwrap();
    let submitted = bus
        .create_task(
            &operator,
            acs::bus::CreateTaskInput {
                title: "older submission".into(),
                to: Some("review-worker".into()),
                ..Default::default()
            },
        )
        .unwrap();
    bus.claim_task(&worker, Some(submitted.id)).unwrap();
    bus.submit_task(
        &worker,
        submitted.id,
        SubmitInput {
            summary: "ready for review".into(),
            ..Default::default()
        },
    )
    .unwrap();
    for i in 0..1000 {
        bus.create_task(
            &operator,
            acs::bus::CreateTaskInput {
                title: format!("newer task {i}"),
                ..Default::default()
            },
        )
        .unwrap();
    }

    let (reply, code) = call(&db, "snapshot", json!({}));
    let data = ok(&reply, code);
    assert_eq!(data["truncated"], true);
    let tasks = data["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 1001);
    assert_eq!(tasks[0]["id"], 1001);
    assert_eq!(tasks[999]["id"], 2);
    assert_eq!(tasks[1000]["id"], submitted.id);
    assert_eq!(tasks[1000]["state"], "submitted");
}

#[test]
fn submitted_before_caps_results_and_orders_newest_first() {
    let db = temp_db("submitted-limit");
    init_bus(&db);
    let bus = Bus::open(Some(&db)).unwrap();
    let operator = bus.identify(Some(OPERATOR_ID)).unwrap();
    bus.add_agent(
        &operator,
        "review-worker",
        Some("worker"),
        Some("test-model"),
        Some("test"),
        None,
        Some("worker"),
    )
    .unwrap();
    let worker = bus.identify(Some("review-worker")).unwrap();
    let mut ids = Vec::new();
    for i in 0..3 {
        let task = bus
            .create_task(
                &operator,
                acs::bus::CreateTaskInput {
                    title: format!("submission {i}"),
                    to: Some("review-worker".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        bus.claim_task(&worker, Some(task.id)).unwrap();
        bus.submit_task(
            &worker,
            task.id,
            SubmitInput {
                summary: format!("submitted {i}"),
                ..Default::default()
            },
        )
        .unwrap();
        ids.push(task.id);
    }

    let tasks = bus.submitted_before(ids[2] + 1, 2).unwrap();
    assert_eq!(tasks.len(), 2);
    assert_eq!(
        tasks.iter().map(|task| task.id).collect::<Vec<_>>(),
        vec![ids[2], ids[1]]
    );
}

#[test]
fn snapshot_previews_multibyte_brief_but_task_action_returns_full_text() {
    let db = temp_db("brief-preview");
    init_bus(&db);
    let bus = Bus::open(Some(&db)).unwrap();
    let operator = bus.identify(Some(OPERATOR_ID)).unwrap();
    let brief = "界".repeat(1001);
    let task = bus
        .create_task(
            &operator,
            acs::bus::CreateTaskInput {
                title: "long brief".into(),
                brief: Some(brief.clone()),
                ..Default::default()
            },
        )
        .unwrap();

    let (reply, code) = call(&db, "snapshot", json!({}));
    let data = ok(&reply, code);
    assert_eq!(data["truncated"], false);
    let snapshot_task = data["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|record| record["id"] == task.id)
        .unwrap();
    let expected_preview = format!("{}…", "界".repeat(1000));
    assert_eq!(
        snapshot_task["brief"].as_str(),
        Some(expected_preview.as_str())
    );

    let (reply, code) = call(&db, "task", json!({"id": task.id}));
    let detail = ok(&reply, code);
    assert_eq!(detail["brief"].as_str(), Some(brief.as_str()));
}

#[test]
fn snapshot_returns_the_latest_100_messages() {
    let db = temp_db("latest");
    init_bus(&db);
    let bus = Bus::open(Some(&db)).unwrap();
    let operator = bus.identify(Some(OPERATOR_ID)).unwrap();
    bus.add_agent(&operator, "mail-fan", None, None, None, None, None)
        .unwrap();
    for i in 0..105 {
        bus.send(
            &operator,
            acs::bus::SendInput {
                to: "mail-fan".into(),
                subject: Some(format!("note {i}")),
                body: format!("body {i}"),
                msg_type: None,
                thread: None,
                task_id: None,
                refs: None,
                requires_ack: false,
            },
        )
        .unwrap();
    }
    let (reply, code) = call(&db, "snapshot", json!({}));
    let data = ok(&reply, code);
    let messages = data["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 100, "{messages:?}");
    // Oldest-to-newest within the window; the newest message overall is last.
    let last = messages.last().unwrap();
    assert_eq!(last["subject"], "note 104", "{last}");
    assert_eq!(last["sender"], OPERATOR_ID);
    assert_eq!(last["recipient"], "mail-fan");
    let seqs: Vec<i64> = messages
        .iter()
        .map(|m| m["seq"].as_i64().unwrap())
        .collect();
    let mut sorted = seqs.clone();
    sorted.sort_unstable();
    assert_eq!(seqs, sorted, "ascending seq within the latest window");
}
