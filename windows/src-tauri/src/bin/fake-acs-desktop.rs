//! `fake-acs-desktop` — a protocol-v1 stand-in for the real `acs-desktop`
//! bridge while the Windows port of `rust/` is in flight. Same wire contract:
//! one `{version, dbPath, action, payload}` JSON object on stdin, exactly one
//! `{ok:true,data}` / `{ok:false,error:{code,message}}` line on stdout,
//! diagnostics on stderr, exit 0 on success and nonzero on error.
//!
//! Instead of SQLite it keeps a JSON document at `dbPath`, seeded by `demo`
//! with the same fixture `aos demo` writes. Besides the real action set it
//! understands `__test_*` actions used by the transport tests (hang, flood,
//! exit codes, a descendant holding our pipes open) — a real helper would
//! never see those actions and errors on them.

use serde_json::{json, Value};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::exit;
use std::time::Duration;

const MARKER: &str = "SIMULATED";
/// Same preview contract as the real bridge: snapshot cuts brief, acceptance
/// and result.details at 1000 chars + '…'; `task` returns them in full.
const PREVIEW_CHARS: usize = 1000;
/// Same page size as the real bridge's snapshot.
const TASK_LIMIT: usize = 1000;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn ok(data: Value) -> ! {
    println!("{}", json!({ "ok": true, "data": data }));
    exit(0);
}

fn fail(code: &str, message: &str) -> ! {
    println!(
        "{}",
        json!({ "ok": false, "error": { "code": code, "message": message } })
    );
    exit(1);
}

fn invalid(message: impl Into<String>) -> ! {
    fail("invalid", &message.into())
}

fn message(text: impl Into<String>) -> Value {
    json!({ "message": text.into() })
}

// --------------------------------------------------------------- state file

fn load(db: &Path) -> Value {
    match std::fs::read_to_string(db) {
        Ok(text) => match serde_json::from_str(&text) {
            Ok(Value::Object(state)) => Value::Object(state),
            _ => invalid(format!(
                "{} is not an agent bus / nothing was written; point dbPath at an initialized workspace",
                db.display()
            )),
        },
        Err(_) => fail(
            "not_found",
            &format!(
                "no bus at {} / nothing was created; initialize the workspace first",
                db.display()
            ),
        ),
    }
}

fn save(db: &Path, state: &Value) {
    if let Some(dir) = db.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if std::fs::write(db, serde_json::to_string(state).unwrap()).is_err() {
        invalid(format!("cannot write {}", db.display()));
    }
}

fn open_existing(db: &Path) -> Value {
    if !db.is_file() {
        fail(
            "not_found",
            &format!(
                "no bus at {} / nothing was created; initialize the workspace first",
                db.display()
            ),
        );
    }
    load(db)
}

fn want<'a>(payload: &'a Value, key: &str) -> &'a Value {
    match payload.get(key).filter(|v| !v.is_null()) {
        Some(v) => v,
        None => invalid(format!("payload.{key} is required")),
    }
}

fn want_str<'a>(payload: &'a Value, key: &str) -> &'a str {
    match want(payload, key).as_str() {
        Some(s) => s,
        None => invalid(format!("payload.{key} must be a string")),
    }
}

fn want_i64(payload: &Value, key: &str) -> i64 {
    match want(payload, key).as_i64() {
        Some(n) => n,
        None => invalid(format!("payload.{key} must be an integer")),
    }
}

fn want_bool(payload: &Value, key: &str) -> bool {
    match want(payload, key).as_bool() {
        Some(b) => b,
        None => invalid(format!("payload.{key} must be a boolean")),
    }
}

fn str_list(payload: &Value, key: &str) -> Vec<String> {
    match payload.get(key) {
        None | Some(Value::Null) => vec![],
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|i| i.as_str().map(str::to_string))
            .collect(),
        _ => invalid(format!("payload.{key} must be a list")),
    }
}

// --------------------------------------------------------------- demo fixture

/// The same cast and task list `aos demo` seeds, as plain JSON state.
fn fixture() -> Value {
    let now = now_ms();
    let early = now - 3 * 3_600_000;
    let mid = now - 50 * 60_000;
    let agent = |id: &str, role: &str, model: &str, harness: &str, status: &str, seen: i64| {
        json!({
            "id": id, "role": role, "model": model, "harness": harness,
            "status": status, "lastSeenMs": seen, "running": false, "paused": false,
        })
    };
    let task = |id: i64,
                title: &str,
                brief: &str,
                state: &str,
                assignee: Option<&str>,
                reviewer: Option<&str>,
                deps: Vec<i64>,
                updated: i64,
                result: Value,
                review: Value,
                notes: Vec<Value>| {
        json!({
            "id": id, "title": title, "brief": brief, "acceptance": "",
            "state": state, "priority": "normal",
            "assignee": assignee, "reviewer": reviewer, "project": Value::Null,
            "pathScopes": [], "dependencies": deps,
            "updatedMs": updated, "createdMs": early,
            "result": result, "review": review,
            "_notes": notes,
        })
    };
    json!({
        "simulated": true,
        "seq": 1,
        "agents": [
            agent("operator", "operator", "human", "cli", "idle", now),
            agent("impl-a", "worker", "gpt-5-codex", "codex", "working", now),
            agent("impl-b", "worker", "claude-sonnet-4", "claude", "working", mid),
            agent("lead", "manager", "claude-opus-4", "claude", "working", now),
            agent("rev-1", "reviewer", "gemini-2.5-pro", "gemini", "idle", early),
            agent("scout", "researcher", "qwen3-coder", "opencode", "idle", early),
        ],
        "tasks": [
            task(7, "hard usd cap per run",
                "A run with a usd budget stops at the cap.",
                "submitted", Some("impl-b"), Some("operator"), vec![], now,
                json!({
                    "summary": "Runs stop at the usd cap; agents with unknown cost are refused when a cap is set.",
                    "details": "",
                    "changedFiles": ["src/core/bus.ts", "rust/src/bus.rs"],
                    // One reference so the inspector's "Attached references"
                    // section has something to show (the real demo has none).
                    "artifacts": [
                        { "type": "file", "value": "docs/usd-cap-notes.md",
                          "description": "Comparison of per-run cost signals by provider" }
                    ],
                    "validation": [
                        { "command": "cargo test budget", "passed": true, "summary": "6 passed" },
                        { "command": "node scripts/v2-interop-smoke.mjs", "passed": false, "summary": "usd cap column missing from the TS schema" }
                    ],
                    "completedMs": now
                }),
                Value::Null, vec![]),
            task(6, "cost on the status screen",
                "Show spend next to stuck and needs-you.",
                "blocked", None, None, vec![3], early, Value::Null, Value::Null, vec![]),
            task(5, "pause and resume from the cli",
                "qagent pause and qagent resume keep claims and leases.",
                "changes_requested", Some("impl-a"), Some("rev-1"), vec![], now,
                json!({
                    "summary": "qagent pause and resume added.",
                    "details": "",
                    "changedFiles": ["src/cli/main.ts"],
                    "artifacts": [],
                    "validation": [],
                    "completedMs": now
                }),
                json!({ "reviewer": "rev-1", "accepted": false,
                        "feedback": "Resume does not restore the claim lease; a paused task can be stolen.",
                        "reviewedMs": now }),
                vec![]),
            task(4, "loop guard: stop the same tool call 5x",
                "Detect five identical tool calls in a row and pause the agent.",
                "claimed", Some("impl-b"), None, vec![], mid, Value::Null, Value::Null,
                vec![json!({ "id": 1, "author": "impl-b", "tsMs": mid,
                    "body": "repeated call detection fires on retries too; trying a hash of name and arguments" })]),
            task(3, "add per-task token cap in core",
                "A claim fails once the task's token budget is spent.",
                "submitted", Some("impl-a"), Some("operator"), vec![], now,
                json!({
                    "summary": "Per-task token cap enforced; a claim over budget fails with budget_exceeded.",
                    "details": "Cap lives on the task row; the TS schema gets the same column in a migration.",
                    "changedFiles": ["src/core/bus.ts", "src/core/db.ts", "tests/budget.test.ts"],
                    "artifacts": [],
                    "validation": [
                        { "command": "npm run test:unit", "passed": true, "summary": "212 passed" },
                        { "command": "node scripts/v2-interop-smoke.mjs", "passed": true, "summary": "both implementations agree" }
                    ],
                    "completedMs": now
                }),
                Value::Null, vec![]),
            task(2, "research budget prior art",
                "How other harnesses cap spend per run and per task.",
                "accepted", Some("scout"), Some("rev-1"), vec![], early,
                json!({
                    "summary": "Three harnesses cap per run; none caps per task; two treat unknown cost as zero.",
                    "details": "Sources linked in the task notes.",
                    "changedFiles": [],
                    "artifacts": [],
                    "validation": [
                        { "command": "link-check notes.md", "passed": true, "summary": "9 of 9 links resolve" }
                    ],
                    "completedMs": early
                }),
                json!({ "reviewer": "rev-1", "accepted": true,
                        "feedback": "Sources check out; the unknown-cost finding matters for #7.",
                        "reviewedMs": early }),
                vec![]),
            task(1, "ship budget enforcement for runs",
                "Runs stop at a token or usd cap, and the operator can see spend.",
                "claimed", Some("lead"), None, vec![], now, Value::Null, Value::Null, vec![]),
        ],
        "messages": [
            {
                "seq": 1, "tsMs": now, "sender": "impl-b", "recipient": "operator",
                "subject": "unknown cost under a usd cap",
                "body": "Codex and Hermes report no per-turn cost. Should a run with a usd cap refuse them, or record cost as unknown and continue?",
                "_taskId": 7
            }
        ]
    })
}

// --------------------------------------------------------------- actions

/// Snapshot preview of one task record: strips the fake's private `_notes`
/// and cuts long prose exactly like the real bridge's `task_preview`.
fn task_preview(task: &Value) -> Value {
    let mut record = task.clone();
    if let Some(object) = record.as_object_mut() {
        object.remove("_notes");
        for key in ["brief", "acceptance"] {
            if let Some(Value::String(text)) = object.get_mut(key) {
                preview_cut(text);
            }
        }
        if let Some(mut text) = object
            .get("result")
            .and_then(|r| r.get("details"))
            .and_then(Value::as_str)
            .map(str::to_string)
        {
            preview_cut(&mut text);
            if let Some(result) = object.get_mut("result").and_then(Value::as_object_mut) {
                result.insert("details".into(), json!(text));
            }
        }
    }
    record
}

/// Truncate at the char boundary past `PREVIEW_CHARS` and append '…'.
fn preview_cut(text: &mut String) {
    if let Some((cut, _)) = text.char_indices().nth(PREVIEW_CHARS) {
        text.truncate(cut);
        text.push('…');
    }
}

fn action_snapshot(db: &Path, state: &Value) -> Value {
    let all = state["tasks"].as_array().cloned().unwrap_or_default();
    // Same paging rule as the real bridge: newest TASK_LIMIT tasks, then —
    // only when the page is full — older 'submitted' tasks appended after it.
    let truncated = all.len() > TASK_LIMIT;
    let mut tasks: Vec<Value> = all.iter().take(TASK_LIMIT).map(task_preview).collect();
    if truncated {
        let oldest = all[TASK_LIMIT - 1]["id"].as_i64().unwrap_or(0);
        tasks.extend(
            all[TASK_LIMIT..]
                .iter()
                .filter(|t| t["state"].as_str() == Some("submitted") && t["id"].as_i64() < Some(oldest))
                .map(task_preview),
        );
    }
    let public_tasks = tasks;
    let public_messages: Vec<Value> = state["messages"]
        .as_array()
        .map(|ms| {
            ms.iter()
                .map(|m| {
                    let mut m = m.clone();
                    m.as_object_mut().map(|o| o.remove("_taskId"));
                    m
                })
                .collect()
        })
        .unwrap_or_default();
    json!({
        "dbPath": db.to_string_lossy(),
        "simulated": state["simulated"].as_bool().unwrap_or(false),
        "canOperate": true,
        "agents": state["agents"],
        "tasks": public_tasks,
        "messages": public_messages,
        "truncated": false,
    })
}

fn action_task(state: &Value, payload: &Value) -> Value {
    let id = want_i64(payload, "id");
    let Some(task) = state["tasks"]
        .as_array()
        .and_then(|ts| ts.iter().find(|t| t["id"].as_i64() == Some(id)))
    else {
        fail("not_found", &format!("task #{id} does not exist"));
    };
    let mut record = task.clone();
    let notes = record
        .as_object_mut()
        .and_then(|o| o.remove("_notes"))
        .unwrap_or_else(|| json!([]));
    let messages: Vec<Value> = state["messages"]
        .as_array()
        .map(|ms| {
            ms.iter()
                .filter(|m| m["_taskId"].as_i64() == Some(id))
                .map(|m| {
                    let mut m = m.clone();
                    m.as_object_mut().map(|o| o.remove("_taskId"));
                    m
                })
                .collect()
        })
        .unwrap_or_default();
    let object = record.as_object_mut().unwrap();
    object.insert("notes".into(), notes);
    object.insert("messages".into(), json!(messages));
    record
}

fn find_task_mut<'a>(state: &'a mut Value, id: i64) -> &'a mut Value {
    let tasks = state["tasks"].as_array_mut().unwrap();
    match tasks.iter_mut().find(|t| t["id"].as_i64() == Some(id)) {
        Some(task) => task,
        None => fail("not_found", &format!("task #{id} does not exist")),
    }
}

fn simulated(state: &Value) -> bool {
    state["simulated"].as_bool().unwrap_or(false)
}

// --------------------------------------------------------------- test hooks

/// Every `__test_*` action diverges (writes and exits); unknown ones fall
/// through to the caller, which reports them as unknown actions.
fn run_test_hook(action: &str) {
    match action {
        // Never replies; the transport must kill us at its timeout.
        "__test_hang" => {
            std::thread::sleep(Duration::from_secs(600));
            ok(Value::Null);
        }
        // Spews past the 16 MiB stdout bound; drainer must keep reading.
        "__test_flood" => {
            let chunk = "x".repeat(1024 * 1024);
            for _ in 0..24 {
                println!("{chunk}");
            }
            ok(Value::Null);
        }
        // Valid ok envelope followed by a nonzero exit — must be rejected.
        "__test_ok_exit1" => {
            println!("{}", json!({ "ok": true, "data": {} }));
            std::io::Write::flush(&mut std::io::stdout()).ok();
            exit(1);
        }
        // Nothing but stderr noise and a nonzero exit.
        "__test_exit1" => {
            eprintln!("fake-acs-desktop: deliberate failure for the test");
            exit(1);
        }
        // Garbage on stdout, clean exit — malformed response.
        "__test_garbage" => {
            println!("this is not json");
            exit(0);
        }
        // Reply, then leave a descendant holding our stdout open — a correct
        // transport waits on this process's exit, not on pipe EOF.
        "__test_leak_pipe" => {
            println!("{}", json!({ "ok": true, "data": { "leaked": true } }));
            std::io::Write::flush(&mut std::io::stdout()).ok();
            if let Ok(exe) = std::env::current_exe() {
                let _ = std::process::Command::new(exe)
                    .env("FAKE_MODE", "hold")
                    .spawn();
            }
            exit(0);
        }
        _ => {}
    }
}

fn main() {
    // Descendant mode: `__test_leak_pipe` respawns us holding inherited pipes.
    if std::env::var("FAKE_MODE").as_deref() == Ok("hold") {
        std::thread::sleep(Duration::from_secs(30));
        exit(0);
    }

    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        fail("invalid", "cannot read stdin");
    }
    let request: Value = match serde_json::from_str(input.trim()) {
        Ok(v @ Value::Object(_)) => v,
        _ => invalid("request is not a JSON object"),
    };
    if request["version"].as_i64() != Some(1) {
        invalid("unsupported protocol version / this acs-desktop speaks version 1");
    }
    let db = PathBuf::from(request["dbPath"].as_str().unwrap_or_default());
    let action = request["action"].as_str().unwrap_or_default().to_string();
    let payload = request
        .get("payload")
        .cloned()
        .unwrap_or_else(|| json!({}));

    if action.starts_with("__test_") {
        run_test_hook(&action);
        invalid(format!("unknown action: {action}"));
    }

    match action.as_str() {
        "detect" => {
            let cli = |id: &str, name: &str, approval: bool| {
                json!({
                    "id": id, "name": name, "path": Value::Null,
                    "signIn": "not installed", "requiresApproval": approval,
                })
            };
            ok(Value::Array(vec![
                cli("claude", "Claude Code", false),
                cli("codex", "Codex CLI", false),
                cli("cursor", "Cursor CLI", false),
                cli("gemini", "Gemini CLI", true),
                cli("hermes", "Hermes Agent", true),
                cli("opencode", "OpenCode", true),
                cli("kimi", "Kimi Code", true),
                cli("grok", "Grok CLI", true),
                cli("devin", "Devin CLI", true),
                cli("qwen", "Qwen Code", true),
                cli("copilot", "GitHub Copilot CLI", true),
                cli("amp", "Amp", true),
                cli("auggie", "Auggie (Augment)", true),
                cli("kilo", "Kilo CLI", true),
                cli("goose", "Goose", true),
                cli("crush", "Crush", true),
                cli("vibe", "Mistral Vibe", true),
                cli("cline", "Cline CLI", true),
                cli("continue", "Continue CLI", true),
                cli("aider", "Aider", true),
                cli("amazonq", "Amazon Q Developer CLI", true),
            ]));
        }
        "init" => {
            if db.is_file() {
                // Existing state we wrote counts; anything else is a conflict.
                let _ = load(&db);
            } else {
                save(
                    &db,
                    &json!({
                        "simulated": false, "seq": 0,
                        "agents": [{
                            "id": "operator", "role": "operator", "model": "human",
                            "harness": "cli", "status": "idle",
                            "lastSeenMs": now_ms(), "running": false, "paused": false
                        }],
                        "tasks": [], "messages": [],
                    }),
                );
            }
            ok(message(format!(
                "operator ready / bus at {}",
                db.display()
            )));
        }
        "demo" => {
            if db.exists() {
                fail(
                    "conflict",
                    &format!(
                        "{} already exists / the demo only seeds a fresh destination",
                        db.display()
                    ),
                );
            }
            save(&db, &fixture());
            if let Some(dir) = db.parent() {
                let _ = std::fs::write(
                    dir.join(MARKER),
                    "A sample bus made by aos demo. Its agents, tasks and results are made up; nothing runs.\n",
                );
            }
            ok(message(format!(
                "simulated sample bus at {} / nothing on it is real and no agent can start there",
                db.display()
            )));
        }
        "snapshot" => {
            let state = open_existing(&db);
            ok(action_snapshot(&db, &state));
        }
        "task" => {
            let state = open_existing(&db);
            ok(action_task(&state, &payload));
        }
        "createTask" => {
            let mut state = open_existing(&db);
            let title = want_str(&payload, "title").to_string();
            let tasks = state["tasks"].as_array_mut().unwrap();
            let id = tasks
                .iter()
                .filter_map(|t| t["id"].as_i64())
                .max()
                .unwrap_or(0)
                + 1;
            let now = now_ms();
            tasks.push(json!({
                "id": id, "title": title,
                "brief": payload["brief"].as_str().unwrap_or(""),
                "acceptance": payload["acceptance"].as_str().unwrap_or(""),
                "state": "open",
                "priority": payload["priority"].as_str().unwrap_or("normal"),
                "assignee": payload["to"],
                "reviewer": payload["reviewer"].as_str().filter(|r| !r.trim().is_empty())
                    .map(Value::from).unwrap_or_else(|| json!("operator")),
                "project": payload["project"],
                "pathScopes": str_list(&payload, "pathScopes"),
                "dependencies": [],
                "updatedMs": now, "createdMs": now,
                "result": Value::Null, "review": Value::Null, "_notes": [],
            }));
            save(&db, &state);
            ok(message(format!("task #{id} created")));
        }
        "reviewTask" => {
            let mut state = open_existing(&db);
            let id = want_i64(&payload, "id");
            let accept = want_bool(&payload, "accept");
            let feedback = want_str(&payload, "feedback").to_string();
            let task = find_task_mut(&mut state, id);
            if task["state"].as_str() != Some("submitted") {
                invalid(format!("task #{id} is not awaiting review"));
            }
            task["state"] = json!(if accept { "accepted" } else { "changes_requested" });
            task["review"] = json!({
                "reviewer": "operator", "accepted": accept,
                "feedback": feedback, "reviewedMs": now_ms(),
            });
            task["updatedMs"] = json!(now_ms());
            save(&db, &state);
            ok(message(format!(
                "task #{} {}",
                id,
                if accept { "accepted".to_string() } else { "is changes_requested".to_string() }
            )));
        }
        "requeue" => {
            let mut state = open_existing(&db);
            let id = want_i64(&payload, "id");
            let _reason = want_str(&payload, "reason");
            let task = find_task_mut(&mut state, id);
            task["state"] = json!("open");
            task["assignee"] = Value::Null;
            task["updatedMs"] = json!(now_ms());
            save(&db, &state);
            ok(message(format!("task #{id} requeued")));
        }
        "cancel" => {
            let mut state = open_existing(&db);
            let id = want_i64(&payload, "id");
            let _reason = want_str(&payload, "reason");
            let task = find_task_mut(&mut state, id);
            task["state"] = json!("cancelled");
            task["updatedMs"] = json!(now_ms());
            save(&db, &state);
            ok(message(format!("task #{id} cancelled")));
        }
        "send" => {
            let mut state = open_existing(&db);
            let to = want_str(&payload, "to").to_string();
            let subject = want_str(&payload, "subject").to_string();
            let body = want_str(&payload, "body").to_string();
            let seq = state["seq"].as_i64().unwrap_or(0) + 1;
            state["seq"] = json!(seq);
            state["messages"].as_array_mut().unwrap().push(json!({
                "seq": seq, "tsMs": now_ms(), "sender": "operator", "recipient": to,
                "subject": subject, "body": body, "_taskId": Value::Null,
            }));
            save(&db, &state);
            ok(message("sent"));
        }
        "pause" | "resume" => {
            let mut state = open_existing(&db);
            let id = want_str(&payload, "id").to_string();
            let paused = action == "pause";
            let agents = state["agents"].as_array_mut().unwrap();
            match agents.iter_mut().find(|a| a["id"].as_str() == Some(&id)) {
                Some(agent) => {
                    agent["paused"] = json!(paused);
                    save(&db, &state);
                    ok(message(format!(
                        "{id} {}",
                        if paused { "paused" } else { "resumed" }
                    )));
                }
                None => fail("not_found", &format!("agent {id} does not exist")),
            }
        }
        "setup" => {
            let _state = open_existing(&db);
            ok(message(
                "crew ready: impl-a (codex), impl-b (claude), rev-1 (gemini) / wrote 3 preset files",
            ));
        }
        "start" => {
            let state = open_existing(&db);
            if payload["confirmed"].as_bool() != Some(true) {
                invalid("start needs confirmed: true / the app asks the operator first");
            }
            if simulated(&state) {
                invalid(
                    "this is the simulated sample from aos demo, so no agent starts here / leave (q), cd to your project and run aos",
                );
            }
            invalid("no bundled qagent found next to this binary / cannot start supervisors");
        }
        "stop" => {
            let _state = open_existing(&db);
            let ids: Vec<String> = str_list(&payload, "ids");
            if ids.is_empty() {
                invalid("name at least one agent to stop");
            }
            ok(message(format!("{} was not running", ids.join(", "))));
        }
        other => invalid(format!("unknown action: {other}")),
    }
}
