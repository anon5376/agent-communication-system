//! `acs-desktop`: the local bridge a desktop companion (ACS.app) shells out to.
//!
//! One JSON request on stdin, one JSON reply on stdout, nothing else on stdout
//! (diagnostics go to stderr). The wire contract is `protocol/desktop-v1.md`:
//! all keys camelCase, `{version, dbPath, action, payload}` in, `{ok, data}` or
//! `{ok, error:{code,message}}` out; an error reply exits nonzero.
//!
//! Every write goes through the Bus API as the operator — the bridge never
//! opens a transaction of its own for writes and never returns tokens or agent
//! meta. Anything but `init`, `demo` and `detect` refuses a `dbPath` that does
//! not already exist, so a typo can never grow a fresh database.

use crate::aos::crew;
use crate::aos::demo;
use crate::aos::keeper;
use crate::bus::{Bus, CreateTaskInput, SendInput};
use crate::error::{BusError, Result};
use crate::identity::Identity;
use crate::types::{Agent, Message, Task, TaskReview, OPERATOR_ID};
use serde::Serialize;
use serde_json::{json, Value};
use std::io::Read;
use std::path::{Path, PathBuf};

/// Newest tasks a snapshot carries (closed states included).
const TASK_LIMIT: i64 = 1000;
/// Characters of brief, acceptance and submission details a snapshot carries
/// per task; `task` returns them in full. Keeps a large bus's snapshot well
/// under the client's output limit.
const PREVIEW_CHARS: usize = 1000;
/// Newest messages a snapshot carries.
const MESSAGE_LIMIT: i64 = 100;

/// Where the bundled `qagent` is expected: this binary's folder. The Swift
/// client may point elsewhere with ACS_DESKTOP_QAGENT (tests do).
const QAGENT_ENV: &str = "ACS_DESKTOP_QAGENT";
const AOS_ENV: &str = "ACS_DESKTOP_AOS";

struct Request {
    db_path: PathBuf,
    action: String,
    payload: Value,
}

/// The process entry point: stdin -> one reply line on stdout -> exit code.
pub fn main() -> i32 {
    let mut input = String::new();
    let (body, code) = match std::io::stdin().read_to_string(&mut input) {
        Ok(_) => respond(&input),
        Err(error) => reply_line(Err(BusError::invalid(format!(
            "cannot read stdin: {error}"
        )))),
    };
    println!("{body}");
    code
}

/// One request in, `(body, exit_code)` out — what `main` prints and returns.
/// Tests drive the bridge through this instead of a subprocess.
pub fn respond(input: &str) -> (String, i32) {
    reply_line(handle(input))
}

fn reply_line(result: Result<Value>) -> (String, i32) {
    match result {
        Ok(data) => (json!({ "ok": true, "data": data }).to_string(), 0),
        Err(error) => (
            json!({
                "ok": false,
                "error": { "code": error.code.as_str(), "message": error.message }
            })
            .to_string(),
            1,
        ),
    }
}

fn handle(input: &str) -> Result<Value> {
    let request = parse_request(input)?;
    let db_path = crate::db::absolutize(&request.db_path);
    match request.action.as_str() {
        // Reads of the machine or of a new destination need no existing bus.
        "detect" => action_detect(),
        "init" => action_init(&db_path),
        "demo" => action_demo(&db_path),
        // Everything else opens an existing bus; a missing file is an error,
        // never a reason to create one (Bus::open would).
        "snapshot" => action_snapshot(&open_existing(&db_path)?),
        "task" => action_task(&open_existing(&db_path)?, &request.payload),
        "setup" => action_setup(&open_existing(&db_path)?),
        "start" => action_start(&open_existing(&db_path)?, &request.payload),
        "stop" => action_stop(&open_existing(&db_path)?, &request.payload),
        "pause" => {
            let bus = open_existing(&db_path)?;
            let operator = operator(&bus)?;
            let id = want_str(&request.payload, "id")?;
            bus.pause_agent(&operator, id, Some("paused from the desktop app"))?;
            Ok(message(format!("{id} paused")))
        }
        "resume" => {
            let bus = open_existing(&db_path)?;
            let operator = operator(&bus)?;
            let id = want_str(&request.payload, "id")?;
            bus.resume_agent(&operator, id)?;
            Ok(message(format!("{id} resumed")))
        }
        "createTask" => {
            let bus = open_existing(&db_path)?;
            action_create_task(&bus, &request.payload)
        }
        "reviewTask" => {
            let bus = open_existing(&db_path)?;
            let operator = operator(&bus)?;
            let task = bus.review_task(
                &operator,
                want_i64(&request.payload, "id")?,
                want_bool(&request.payload, "accept")?,
                want_str(&request.payload, "feedback")?,
            )?;
            Ok(message(format!(
                "task #{} {}",
                task.id,
                if task.state == "accepted" {
                    "accepted".to_string()
                } else {
                    format!("is {}", task.state)
                }
            )))
        }
        "requeue" => {
            let bus = open_existing(&db_path)?;
            let operator = operator(&bus)?;
            let task = bus.requeue_task(
                &operator,
                want_i64(&request.payload, "id")?,
                Some(want_str(&request.payload, "reason")?),
            )?;
            Ok(message(format!("task #{} requeued", task.id)))
        }
        "cancel" => {
            let bus = open_existing(&db_path)?;
            let operator = operator(&bus)?;
            let task = bus.cancel_task(
                &operator,
                want_i64(&request.payload, "id")?,
                Some(want_str(&request.payload, "reason")?),
            )?;
            Ok(message(format!("task #{} cancelled", task.id)))
        }
        "send" => {
            let bus = open_existing(&db_path)?;
            let operator = operator(&bus)?;
            bus.send(
                &operator,
                SendInput {
                    to: want_str(&request.payload, "to")?.to_string(),
                    subject: Some(want_str(&request.payload, "subject")?.to_string()),
                    body: want_str(&request.payload, "body")?.to_string(),
                    msg_type: None,
                    thread: None,
                    task_id: None,
                    refs: None,
                    requires_ack: false,
                },
            )?;
            Ok(message("sent".to_string()))
        }
        "orchestration" => action_orchestration(&open_existing(&db_path)?),
        "startGoal" => action_start_goal(&open_existing(&db_path)?, &request.payload),
        "saveMission" => action_save_prompt(
            &open_existing(&db_path)?,
            &request.payload,
            PromptKind::Mission,
        ),
        "saveRole" => action_save_prompt(
            &open_existing(&db_path)?,
            &request.payload,
            PromptKind::Role,
        ),
        "setAgent" => action_set_agent(&open_existing(&db_path)?, &request.payload),
        other => Err(BusError::invalid(format!("unknown action: {other}"))),
    }
}

// ------------------------------------------------------------ request shape

fn parse_request(input: &str) -> Result<Request> {
    let request: Value = serde_json::from_str(input.trim())
        .map_err(|e| BusError::invalid(format!("request is not a JSON object: {e}")))?;
    if !request.is_object() {
        return Err(BusError::invalid("request is not a JSON object"));
    }
    if request.get("version").and_then(Value::as_i64) != Some(1) {
        return Err(BusError::invalid(
            "unsupported protocol version / this acs-desktop speaks version 1",
        ));
    }
    let raw = request
        .get("dbPath")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if raw.is_empty() {
        return Err(BusError::invalid("dbPath is required"));
    }
    let db_path = PathBuf::from(raw);
    if !db_path.is_absolute() {
        return Err(BusError::invalid(format!("dbPath must be absolute: {raw}")));
    }
    let action = request
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if action.is_empty() {
        return Err(BusError::invalid("action is required"));
    }
    let payload = request.get("payload").cloned().unwrap_or_else(|| json!({}));
    if !payload.is_object() {
        return Err(BusError::invalid("payload must be an object"));
    }
    Ok(Request {
        db_path,
        action,
        payload,
    })
}

fn want<'a>(payload: &'a Value, key: &str) -> Result<&'a Value> {
    payload
        .get(key)
        .filter(|v| !v.is_null())
        .ok_or_else(|| BusError::invalid(format!("payload.{key} is required")))
}

fn want_str<'a>(payload: &'a Value, key: &str) -> Result<&'a str> {
    want(payload, key)?
        .as_str()
        .ok_or_else(|| BusError::invalid(format!("payload.{key} must be a string")))
}

fn opt_str(payload: &Value, key: &str) -> Option<String> {
    payload.get(key).and_then(Value::as_str).map(str::to_string)
}

fn want_i64(payload: &Value, key: &str) -> Result<i64> {
    want(payload, key)?
        .as_i64()
        .ok_or_else(|| BusError::invalid(format!("payload.{key} must be an integer")))
}

fn want_bool(payload: &Value, key: &str) -> Result<bool> {
    want(payload, key)?
        .as_bool()
        .ok_or_else(|| BusError::invalid(format!("payload.{key} must be a boolean")))
}

fn want_str_list(payload: &Value, key: &str) -> Result<Vec<String>> {
    let items = want(payload, key)?
        .as_array()
        .ok_or_else(|| BusError::invalid(format!("payload.{key} must be a list")))?;
    items
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_string)
                .ok_or_else(|| BusError::invalid(format!("payload.{key} must be strings")))
        })
        .collect()
}

fn str_list_or(payload: &Value, key: &str) -> Result<Vec<String>> {
    match payload.get(key) {
        None | Some(Value::Null) => Ok(vec![]),
        _ => want_str_list(payload, key),
    }
}

fn message(text: String) -> Value {
    json!({ "message": text })
}

// ------------------------------------------------------------------ shared

fn open_existing(db_path: &Path) -> Result<Bus> {
    if !db_path.is_file() {
        return Err(BusError::not_found(format!(
            "no bus at {} / nothing was created; initialize the workspace first",
            db_path.display()
        )));
    }
    if !looks_like_a_bus(db_path)? {
        return Err(BusError::invalid(format!(
            "{} is not an agent bus / nothing was written; point dbPath at an initialized workspace",
            db_path.display()
        )));
    }
    Bus::open(Some(db_path))
}

/// Read-only schema check before Bus::open can run its migrations on whatever
/// file dbPath names. An empty file, a non-database, or a SQLite file without
/// the core bus tables is not ours to write into.
fn looks_like_a_bus(db_path: &Path) -> Result<bool> {
    if db_path.metadata().map(|m| m.len() == 0).unwrap_or(true) {
        return Ok(false);
    }
    let conn =
        rusqlite::Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| BusError::invalid(format!("cannot read {}: {e}", db_path.display())))?;
    let not_a_db = |e: rusqlite::Error| {
        BusError::invalid(format!("{} is not a database: {e}", db_path.display()))
    };
    let tables = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
        .and_then(|mut stmt| {
            stmt.query_map([], |row| row.get::<_, String>(0))
                .map(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
        })
        .map_err(not_a_db)?
        .map_err(not_a_db)?;
    Ok(["agents", "tasks", "messages"]
        .iter()
        .all(|table| tables.iter().any(|t| t == table)))
}

fn operator(bus: &Bus) -> Result<Identity> {
    bus.identify(Some(OPERATOR_ID))
}

fn bundled_sibling(name: &str, env: &str) -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(env) {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }
    let exe = std::env::current_exe().ok()?;
    // dir_candidates adds PATHEXT variants on Windows (qagent -> qagent.exe).
    exe.parent()
        .map(|dir| crate::platform::dir_candidates(dir, name))?
        .into_iter()
        .find(|path| path.is_file())
}

// --------------------------------------------------------------- records

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentRecord {
    id: String,
    role: String,
    model: String,
    harness: String,
    status: String,
    last_seen_ms: Option<i64>,
    running: bool,
    paused: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TaskRecord<'a> {
    id: i64,
    title: &'a str,
    brief: &'a str,
    acceptance: &'a str,
    state: &'a str,
    priority: &'a str,
    assignee: Option<&'a str>,
    reviewer: Option<&'a str>,
    project: Option<&'a str>,
    path_scopes: &'a [String],
    dependencies: &'a [i64],
    updated_ms: i64,
    created_ms: i64,
    result: Option<&'a crate::types::TaskResult>,
    review: Option<&'a TaskReview>,
}

impl<'a> From<&'a Task> for TaskRecord<'a> {
    fn from(task: &'a Task) -> Self {
        TaskRecord {
            id: task.id,
            title: &task.title,
            brief: &task.brief,
            acceptance: &task.acceptance,
            state: &task.state,
            priority: &task.priority,
            assignee: task.assignee.as_deref(),
            reviewer: task.reviewer.as_deref(),
            project: task.project.as_deref(),
            path_scopes: &task.path_scopes,
            dependencies: &task.dependencies,
            updated_ms: task.updated_ms,
            created_ms: task.created_ms,
            result: task.result.as_ref(),
            review: task.review.as_ref(),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MessageRecord<'a> {
    seq: i64,
    ts_ms: i64,
    sender: &'a str,
    recipient: Option<&'a str>,
    subject: &'a str,
    body: &'a str,
}

impl<'a> From<&'a Message> for MessageRecord<'a> {
    fn from(message: &'a Message) -> Self {
        MessageRecord {
            seq: message.seq,
            ts_ms: message.ts_ms,
            sender: &message.sender,
            recipient: message.recipient.as_deref(),
            subject: &message.subject,
            body: &message.body,
        }
    }
}

fn agent_record(paths: &crew::Paths, agent: &Agent) -> AgentRecord {
    AgentRecord {
        id: agent.id.clone(),
        role: agent.role.clone(),
        model: agent.model.clone(),
        harness: agent.harness.clone(),
        status: agent.status.clone(),
        last_seen_ms: agent.last_seen_ms,
        running: crew::running_pid(paths, &agent.id).is_some(),
        // The pause flag lives in agent meta; meta itself never leaves the bridge.
        paused: agent.meta.get("paused").is_some(),
    }
}

// ------------------------------------------------------------------ actions

/// `snapshot` — everything the dashboard needs in one consistent read.
/// `canOperate` only resolves the operator token; it never creates one.
fn action_snapshot(bus: &Bus) -> Result<Value> {
    let paths = crew::Paths::for_db(&bus.db_path);
    let simulated = demo::is_simulated(&bus.home);
    // One deferred transaction so agents, tasks and messages agree with each other.
    bus.conn.execute_batch("BEGIN DEFERRED")?;
    let inner = (|| {
        let agents = bus
            .list_agents()?
            .into_iter()
            .map(|(agent, _unread)| agent)
            .collect::<Vec<_>>();
        let (mut tasks, truncated) = bus.recent_tasks(TASK_LIMIT)?;
        if truncated {
            if let Some(oldest) = tasks.last().map(|task| task.id) {
                tasks.extend(bus.submitted_before(oldest, TASK_LIMIT)?);
            }
        }
        let can_operate = bus.identify(Some(OPERATOR_ID)).is_ok();
        let messages = bus.get_messages(None, Some(MESSAGE_LIMIT), None, None)?;
        Ok::<_, BusError>((agents, tasks, truncated, can_operate, messages))
    })();
    let _ = bus
        .conn
        .execute_batch(if inner.is_ok() { "COMMIT" } else { "ROLLBACK" });
    let (agents, tasks, truncated, can_operate, messages) = inner?;
    Ok(json!({
        "dbPath": bus.db_path.display().to_string(),
        "simulated": simulated,
        "canOperate": can_operate,
        "agents": agents.iter().map(|a| agent_record(&paths, a)).collect::<Vec<_>>(),
        "tasks": tasks.iter().map(task_preview).collect::<Result<Vec<_>>>()?,
        "messages": messages.iter().map(MessageRecord::from).collect::<Vec<_>>(),
        "truncated": truncated,
    }))
}

/// A snapshot task record with long prose cut to `PREVIEW_CHARS`.
fn task_preview(task: &Task) -> Result<Value> {
    let mut record = serde_json::to_value(TaskRecord::from(task))?;
    for pointer in ["/brief", "/acceptance", "/result/details"] {
        if let Some(Value::String(text)) = record.pointer_mut(pointer) {
            if let Some((cut, _)) = text.char_indices().nth(PREVIEW_CHARS) {
                text.truncate(cut);
                text.push('…');
            }
        }
    }
    Ok(record)
}

/// `task {id}` — the flattened TaskDetail: TaskRecord fields + notes + messages.
fn action_task(bus: &Bus, payload: &Value) -> Result<Value> {
    let detail = bus.get_task(want_i64(payload, "id")?)?;
    let mut record = serde_json::to_value(TaskRecord::from(&detail.task))?;
    let object = record
        .as_object_mut()
        .ok_or_else(|| BusError::invalid("task record did not serialize"))?;
    object.insert(
        "notes".into(),
        json!(detail
            .notes
            .iter()
            .map(|note| {
                json!({
                    "id": note.id,
                    "author": note.author,
                    "tsMs": note.ts_ms,
                    "body": note.body,
                })
            })
            .collect::<Vec<_>>()),
    );
    object.insert(
        "messages".into(),
        json!(detail
            .messages
            .iter()
            .map(MessageRecord::from)
            .collect::<Vec<_>>()),
    );
    Ok(record)
}

/// `init` — first-run only, called explicitly by the app. `Bus::init` creates
/// or adopts the operator token and never wipes existing state.
fn action_init(db_path: &Path) -> Result<Value> {
    if db_path.is_file() && !looks_like_a_bus(db_path)? {
        return Err(BusError::conflict(format!(
            "{} exists but is not an agent bus / init never writes over an unrelated file",
            db_path.display()
        )));
    }
    let bus = Bus::open(Some(db_path))?;
    let result = bus.init()?;
    Ok(message(format!(
        "operator ready (token {}) / bus at {}",
        result.operator, result.db_path
    )))
}

/// `demo` — a simulated sample bus, only at a destination that does not exist.
/// The SIMULATED marker next to it keeps `start` and `setup` away for good.
fn action_demo(db_path: &Path) -> Result<Value> {
    if db_path.exists() {
        return Err(BusError::conflict(format!(
            "{} already exists / the demo only seeds a fresh destination",
            db_path.display()
        )));
    }
    match demo::seed(db_path)? {
        true => Ok(message(format!(
            "simulated sample bus at {} / nothing on it is real and no agent can start there",
            db_path.display()
        ))),
        false => Err(BusError::conflict(format!(
            "{} already holds a bus; the demo left it alone",
            db_path.display()
        ))),
    }
}

/// `detect` — the provider CLIs on this machine. Finding an executable is not
/// being signed in; `signIn` only reports what files and env vars show.
fn action_detect() -> Result<Value> {
    let found = crew::detect();
    Ok(Value::Array(
        found
            .iter()
            .map(|item| {
                json!({
                    "id": item.cli.id,
                    "name": item.cli.name,
                    "path": item.path.as_ref().map(|p| p.display().to_string()),
                    "signIn": sign_in_text(item),
                    "requiresApproval": item.cli.auto_approve,
                })
            })
            .collect(),
    ))
}

fn sign_in_text(found: &crew::Found) -> String {
    if found.path.is_none() {
        return "not installed".to_string();
    }
    match &found.sign_in {
        crew::SignIn::Found(evidence) => format!("found: {evidence}"),
        crew::SignIn::Missing => "missing".to_string(),
        crew::SignIn::Unknown(why) => format!("unknown: {why}"),
    }
}

/// `setup` — write the crew (never over an existing crew.json) and put its
/// members on the bus. Starts nothing.
fn action_setup(bus: &Bus) -> Result<Value> {
    // Authenticate before any filesystem write or provider detection: setup
    // touches the disk well before sync_bus would check the operator.
    let _operator = operator(bus)?;
    let paths = crew::Paths::for_db(&bus.db_path);
    let report = crew::setup(bus, &paths, &crew::detect(), false)?;
    let members = report
        .members
        .iter()
        .map(|(id, harness)| format!("{id} ({harness})"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut text = format!("crew ready: {members}");
    if !report.wrote_crew {
        text = format!("kept your existing crew / {text}");
    }
    if report.presets_written > 0 {
        text.push_str(&format!(" / wrote {} preset files", report.presets_written));
    }
    Ok(message(text))
}

/// `start` — supervisors for named crew members in a folder the operator
/// confirmed. Fails before touching anything when the request is not
/// confirmed, the folder is unsafe or missing, the bus is the simulated demo,
/// an id is not in the crew, or no bundled qagent sits next to acs-desktop.
/// Per-agent outcomes are reported verbatim: a partial start is an error that
/// still says who did start.
fn action_start(bus: &Bus, payload: &Value) -> Result<Value> {
    // Authenticate before validation, trust writes or process effects.
    let _operator = operator(bus)?;
    if payload.get("confirmed").and_then(Value::as_bool) != Some(true) {
        return Err(BusError::invalid(
            "start needs confirmed: true / the app asks the operator first",
        ));
    }
    let ids = want_str_list(payload, "ids")?;
    if ids.is_empty() {
        return Err(BusError::invalid("name at least one agent to start"));
    }
    let raw_workdir = want_str(payload, "workdir")?;
    let workdir = std::fs::canonicalize(raw_workdir)
        .map_err(|e| BusError::invalid(format!("workdir {raw_workdir}: {e}")))?;
    if !workdir.is_dir() {
        return Err(BusError::invalid(format!(
            "workdir {} is not a folder",
            workdir.display()
        )));
    }
    if let Some(why) = crew::unsafe_workdir(&workdir) {
        return Err(BusError::invalid(why));
    }
    let paths = crew::Paths::for_db(&bus.db_path);
    if demo::is_simulated(&paths.home) {
        return Err(BusError::invalid(crew::SIMULATED_NOTE));
    }
    let config = crew::load_crew(&paths)?
        .ok_or_else(|| BusError::invalid("no crew configured / run setup first"))?;
    let members = crew::member_ids(&config);
    if let Some(bad) = ids.iter().find(|id| !members.contains(*id)) {
        return Err(BusError::invalid(format!(
            "{bad} is not in the crew / crew.json lists who is"
        )));
    }
    let qagent = bundled_sibling("qagent", QAGENT_ENV).ok_or_else(|| {
        BusError::invalid(format!(
            "no bundled qagent found next to this binary or at ${QAGENT_ENV} / cannot start supervisors"
        ))
    })?;
    // Confirmation granted: this folder becomes trusted for this crew, like the
    // trust prompt in aos start.
    crew::trust(&paths, &workdir)?;
    crew::sync_bus(bus, &config)?;
    let budgeted = crew::apply_default_budget(bus, &paths, &ids)?;
    let results = crew::start_with(&qagent, &bus.db_path, &paths, &ids, &workdir);
    let mut started = Vec::new();
    let mut failed = Vec::new();
    for (id, result) in results {
        match result {
            Ok(pid) => started.push(format!("{id} running (pid {pid})")),
            Err(error) => failed.push(format!("{id} did not start: {}", error.message)),
        }
    }
    // Like aos: a watcher brings back an agent that went down unasked, when a
    // bundled aos binary is there to run it.
    if !started.is_empty() {
        if let Some(aos) = bundled_sibling("aos", AOS_ENV) {
            let _ = keeper::spawn_watcher(&aos, &bus.db_path);
        }
    }
    let mut lines = started.clone();
    if !budgeted.is_empty() {
        lines.push(format!(
            "{} configured defaults: {} each. Limits are checked between turns; cost accounting depends on the provider. This is not a guaranteed dollar cap.",
            budgeted.join(", "),
            crew::DEFAULT_BUDGET.describe()
        ));
    }
    lines.push(format!("agents work in {}", workdir.display()));
    if failed.is_empty() {
        return Ok(message(lines.join(" / ")));
    }
    let mut text = String::new();
    if !started.is_empty() {
        text.push_str(&format!("started {} / ", started.join(", ")));
    }
    text.push_str(&failed.join(" / "));
    Err(BusError::invalid(text))
}

/// `stop` — SIGINT each named agent's supervisor, crew members only.
fn action_stop(bus: &Bus, payload: &Value) -> Result<Value> {
    // Authenticate before the crew check or any signal to a supervisor.
    let _operator = operator(bus)?;
    let ids = want_str_list(payload, "ids")?;
    if ids.is_empty() {
        return Err(BusError::invalid("name at least one agent to stop"));
    }
    let paths = crew::Paths::for_db(&bus.db_path);
    let config = crew::load_crew(&paths)?
        .ok_or_else(|| BusError::invalid("no crew configured / run setup first"))?;
    let members = crew::member_ids(&config);
    if let Some(bad) = ids.iter().find(|id| !members.contains(*id)) {
        return Err(BusError::invalid(format!(
            "{bad} is not in the crew / crew.json lists who is"
        )));
    }
    let mut stopped = Vec::new();
    let mut idle = Vec::new();
    let mut failed = Vec::new();
    for (id, result) in crew::stop(&paths, &ids) {
        match result {
            Ok(true) => stopped.push(id),
            Ok(false) => idle.push(format!("{id} was not running")),
            Err(error) => failed.push(format!("{id}: {}", error.message)),
        }
    }
    if !failed.is_empty() {
        return Err(BusError::invalid(format!(
            "{} / {}",
            if stopped.is_empty() {
                String::new()
            } else {
                format!("stopped {}", stopped.join(", "))
            },
            [idle, failed].concat().join(" / ")
        )));
    }
    let mut text = format!("stopped {}", stopped.join(", "));
    if !idle.is_empty() {
        text.push_str(&format!(" / {}", idle.join(", ")));
    }
    Ok(message(text))
}

/// `createTask` — a full bus task from the operator, reviewer defaulting to
/// the operator. Missing optional strings are the bus's own defaults.
fn action_create_task(bus: &Bus, payload: &Value) -> Result<Value> {
    let operator = operator(bus)?;
    let task = bus.create_task(
        &operator,
        CreateTaskInput {
            title: want_str(payload, "title")?.to_string(),
            brief: opt_str(payload, "brief"),
            acceptance: opt_str(payload, "acceptance"),
            to: opt_str(payload, "to"),
            reviewer: Some(
                opt_str(payload, "reviewer")
                    .filter(|r| !r.trim().is_empty())
                    .unwrap_or_else(|| OPERATOR_ID.to_string()),
            ),
            priority: opt_str(payload, "priority"),
            path_scopes: str_list_or(payload, "pathScopes")?,
            project: opt_str(payload, "project"),
            ..Default::default()
        },
    )?;
    Ok(message(format!("task #{} created", task.id)))
}

// ------------------------------------------------------------ orchestration
//
// The aos side of the app: goals, the mission and role prompt files in
// `<bus home>/aos/`, and the crew file. Prompt files are plain text the
// operator owns; the bridge writes only the one file it is asked to.

/// Most recent operator goals `orchestration` returns.
const GOAL_LIMIT: i64 = 20;
/// Largest prompt file the bridge will write.
const PROMPT_MAX_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy)]
enum PromptKind {
    Mission,
    Role,
}

fn prompt_name(payload: &Value) -> Result<String> {
    let name = want_str(payload, "name")?.trim().to_lowercase();
    let ok = !name.is_empty()
        && name.len() <= 40
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !ok {
        return Err(BusError::invalid(
            "name must be 1-40 letters, digits, - or _ (it becomes the file name)",
        ));
    }
    Ok(name)
}

/// The role prompt files: what is in roles/, or the built-in presets before
/// that folder exists. Sorted by name.
fn role_prompts(paths: &crew::Paths) -> Vec<Value> {
    let mut out: Vec<(String, String, bool)> = match std::fs::read_dir(paths.roles()) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "md"))
            .filter_map(|p| {
                let name = p.file_stem()?.to_string_lossy().to_string();
                let text = std::fs::read_to_string(&p).ok()?;
                let custom = crew::ROLE_PRESETS
                    .iter()
                    .find(|(n, _)| *n == name)
                    .is_none_or(|(_, t)| *t != text);
                Some((name, crew::strip_note(&text).to_string(), custom))
            })
            .collect(),
        Err(_) => crew::ROLE_PRESETS
            .iter()
            .map(|(n, t)| (n.to_string(), crew::strip_note(t).to_string(), false))
            .collect(),
    };
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out.into_iter()
        .map(|(name, text, custom)| json!({ "name": name, "text": text, "custom": custom }))
        .collect()
}

/// `orchestration` — missions, role prompts, the crew and recent goals.
fn action_orchestration(bus: &Bus) -> Result<Value> {
    let paths = crew::Paths::for_db(&bus.db_path);
    let missions: Vec<Value> = crew::missions(&paths)
        .into_iter()
        .map(|m| {
            let file = paths.missions().join(format!("{}.md", m.name));
            let raw = std::fs::read_to_string(&file).ok();
            let preset = crew::MISSION_PRESETS
                .iter()
                .find(|(n, _)| *n == m.name)
                .map(|(_, t)| *t);
            let custom = match (&raw, preset) {
                (Some(r), Some(p)) => r != p,
                (Some(_), None) => true,
                (None, _) => false,
            };
            let text = raw
                .as_deref()
                .or(preset)
                .map(|t| crew::strip_note(t).to_string())
                .unwrap_or_default();
            json!({
                "name": m.name, "summary": m.summary, "brief": m.brief,
                "acceptance": m.acceptance, "text": text, "custom": custom,
            })
        })
        .collect();
    let raw_crew: Value = std::fs::read_to_string(paths.crew())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null);
    let (configured, crew_error, members, owner) = match crew::load_crew(&paths) {
        Ok(None) => (false, None, vec![], None),
        Err(e) => (true, Some(e.message), vec![], None),
        Ok(Some(config)) => {
            let mut ids: Vec<&String> = config.agents.keys().collect();
            ids.sort_by_key(|id| (config.agents[*id].authority != "manager", (*id).clone()));
            let members = ids
                .into_iter()
                .map(|id| {
                    let a = &config.agents[id];
                    json!({
                        "id": id,
                        "role": a.role,
                        "authority": a.authority,
                        "cli": config.models.get(&a.model).map(|m| m.harness.clone()).unwrap_or_default(),
                        "description": a.description,
                        "enabled": a.enabled,
                        "instructions": raw_crew["agents"][id]["instructions"].as_str(),
                        "running": crew::running_pid(&paths, id).is_some(),
                    })
                })
                .collect();
            (true, None, members, crew::goal_owner(&config))
        }
    };
    let goals: Vec<Value> = {
        let mut stmt = bus.conn.prepare_cached(
            "SELECT id FROM tasks WHERE parent_id IS NULL AND creator = ? ORDER BY id DESC LIMIT ?",
        )?;
        let ids = stmt
            .query_map(rusqlite::params![OPERATOR_ID, GOAL_LIMIT], |row| {
                row.get::<_, i64>(0)
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        ids.into_iter()
            .map(|id| {
                let t = bus.get_task(id)?.task;
                Ok(json!({
                    "id": t.id, "title": t.title, "state": t.state,
                    "assignee": t.assignee, "reviewer": t.reviewer, "updatedMs": t.updated_ms,
                }))
            })
            .collect::<Result<_>>()?
    };
    Ok(json!({
        "simulated": demo::is_simulated(&bus.home),
        "configured": configured,
        "crewError": crew_error,
        "crewDir": crew::Paths::show(&paths.dir),
        "workdir": crew::crew_workdir(&paths).map(|p| p.display().to_string()),
        "goalOwner": owner,
        "missions": missions,
        "roles": role_prompts(&paths),
        "crew": members,
        "goals": goals,
    }))
}

/// `startGoal` — a mission template filled with the goal, handed to the crew's
/// lead (or `to`), reviewed independently when the crew allows. Never starts
/// an agent: that stays an explicit `start`.
fn action_start_goal(bus: &Bus, payload: &Value) -> Result<Value> {
    let operator = operator(bus)?;
    let goal = want_str(payload, "goal")?.trim().to_string();
    if goal.is_empty() {
        return Err(BusError::invalid("say what you want done"));
    }
    let name = opt_str(payload, "mission").unwrap_or_else(|| "run".to_string());
    let paths = crew::Paths::for_db(&bus.db_path);
    let mission = crew::missions(&paths)
        .into_iter()
        .find(|m| m.name == name)
        .ok_or_else(|| BusError::not_found(format!("no mission called {name}")))?;
    let (title, brief, acceptance) = crew::expand(&mission, &goal);
    let to = opt_str(payload, "to").filter(|t| !t.trim().is_empty());
    let config = crew::load_crew(&paths).ok().flatten();
    let owner = to
        .clone()
        .or_else(|| config.as_ref().and_then(crew::goal_owner));
    let reviewer = match (&config, owner.as_deref()) {
        (Some(config), Some(id))
            if config
                .agents
                .get(id)
                .is_some_and(|a| a.authority != "manager") =>
        {
            crew::reviewer_for(config, id)
        }
        _ => OPERATOR_ID.to_string(),
    };
    let task = bus.create_task(
        &operator,
        CreateTaskInput {
            title,
            brief: Some(brief),
            acceptance: Some(acceptance).filter(|a| !a.is_empty()),
            to: owner.clone(),
            reviewer: Some(reviewer.clone()),
            project: opt_str(payload, "project"),
            ..Default::default()
        },
    )?;
    let who = owner.unwrap_or_else(|| "the first free agent".to_string());
    let review = if reviewer == OPERATOR_ID {
        "you review the result".to_string()
    } else {
        format!("{reviewer} reviews it")
    };
    Ok(message(format!(
        "goal #{} started / {who} takes it / {review}",
        task.id
    )))
}

/// `saveMission` / `saveRole` — write one prompt file. The presets are written
/// first so the folder never holds only the edited file.
fn action_save_prompt(bus: &Bus, payload: &Value, kind: PromptKind) -> Result<Value> {
    let _operator = operator(bus)?;
    let name = prompt_name(payload)?;
    let text = want_str(payload, "text")?;
    if text.trim().is_empty() {
        return Err(BusError::invalid("the prompt is empty"));
    }
    if text.len() > PROMPT_MAX_BYTES {
        return Err(BusError::invalid("the prompt is over 64 KB"));
    }
    let paths = crew::Paths::for_db(&bus.db_path);
    crew::write_presets(&paths)?;
    let (dir, what) = match kind {
        PromptKind::Mission => (paths.missions(), "mission"),
        PromptKind::Role => (paths.roles(), "role prompt"),
    };
    let mut body = text.trim_end().to_string();
    body.push('\n');
    std::fs::write(dir.join(format!("{name}.md")), body)?;
    Ok(message(format!("{what} {name} saved")))
}

/// `setAgent` — change an existing crew member's `enabled` or `description`
/// in crew.json, leaving every other field as written.
fn action_set_agent(bus: &Bus, payload: &Value) -> Result<Value> {
    let _operator = operator(bus)?;
    let id = want_str(payload, "id")?;
    let paths = crew::Paths::for_db(&bus.db_path);
    let file = paths.crew();
    let text = std::fs::read_to_string(&file)
        .map_err(|_| BusError::invalid("no crew configured / set up a crew first"))?;
    let mut raw: Value = serde_json::from_str(&text)
        .map_err(|e| BusError::invalid(format!("crew.json does not parse: {e}")))?;
    let agent = raw["agents"]
        .get_mut(id)
        .filter(|a| a.is_object())
        .ok_or_else(|| BusError::not_found(format!("{id} is not in the crew")))?;
    let mut changed = Vec::new();
    if let Some(enabled) = payload.get("enabled").and_then(Value::as_bool) {
        agent["enabled"] = json!(enabled);
        changed.push(if enabled { "on" } else { "off" }.to_string());
    }
    if let Some(description) = payload.get("description").and_then(Value::as_str) {
        if description.len() > 500 {
            return Err(BusError::invalid("description is over 500 characters"));
        }
        agent["description"] = json!(description.trim());
        changed.push("description updated".to_string());
    }
    if changed.is_empty() {
        return Err(BusError::invalid(
            "nothing to change / send enabled or description",
        ));
    }
    let mut out = serde_json::to_string_pretty(&raw)?;
    out.push('\n');
    // Validate before replacing the file so a bad edit never lands.
    let tmp = file.with_extension("json.tmp");
    std::fs::write(&tmp, &out)?;
    if let Err(e) = crate::config::load_config(&tmp) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::rename(&tmp, &file)?;
    Ok(message(format!("{id}: {}", changed.join(", "))))
}
