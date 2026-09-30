//! `qagent mcp [--operator]`: a stdio MCP server over the core library.
//!
//! Port of src/mcp/server.ts: newline-delimited JSON-RPC 2.0 on stdin/stdout —
//! no sockets. Identity comes from QAGENT_AGENT_ID (or AGENT_ID); the token is
//! QAGENT_TOKEN/AGENT_TOKEN, or the token file next to the database when unset.
//! The identity is re-checked on every call, so a rotated token takes effect at
//! once. `bus_wait` runs on a worker thread with its own database connection so
//! a blocking wait does not stall other calls.

use crate::bus::{Bus, CreateTaskInput, ListTasksInput, SendInput, SubmitInput, WaitResult};
use crate::error::{BusError, Result};
use crate::identity::{identity_for_token, resolve_identity, Identity};
use crate::render::{render_agents, render_event, render_messages, render_task, render_tasks};
use crate::types::{limits, Message, MAX_WAIT_SEC};
use crate::wait::{wait_for_mail, wait_seconds};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

const USAGE: &str = "usage: qagent mcp [--operator]\n\nStdio MCP server for one agent. Set QAGENT_AGENT_ID; the token is read from\nQAGENT_TOKEN or from the token file next to the bus database. --operator adds\nbus_agent_add and requires the operator identity (operator.token).\n";

const PROTOCOL_VERSION: &str = "2025-03-26";

pub type EnvLookup = dyn Fn(&str) -> Option<String> + Send + Sync;

pub struct McpOptions {
    pub agent_id: String,
    /// Token held in memory (QAGENT_TOKEN); None means read the token file on each call.
    pub token: Option<String>,
    pub operator: bool,
    /// Lookup for QAGENT_BLOCK_SEC (testable env).
    pub env: Arc<EnvLookup>,
    pub cwd: PathBuf,
}

struct WaitSlot {
    stop: Arc<AtomicBool>,
    handle: JoinHandle<()>,
}

type WaitMap = Arc<Mutex<HashMap<String, WaitSlot>>>;

pub struct McpServer {
    bus: Bus,
    options: McpOptions,
    waits: WaitMap,
    tx: Sender<Value>,
}

fn render_error(error: &BusError) -> String {
    format!("qagent error ({}): {}", error.code.as_str(), error.message)
}

fn error_reply(id: &Value, error: &BusError) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "content": [{ "type": "text", "text": render_error(error) }],
            "isError": true,
        }
    })
}

fn text_reply(id: &Value, text: String) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": { "content": [{ "type": "text", "text": text }] }
    })
}

fn rpc_error(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// Abort every wait in flight and join them so each records its idle status.
fn drain_waits(waits: &WaitMap) {
    let slots: Vec<WaitSlot> = {
        let mut map = waits.lock().unwrap();
        map.drain().map(|(_, slot)| slot).collect()
    };
    for slot in &slots {
        slot.stop.store(true, Ordering::SeqCst);
    }
    for slot in slots {
        let _ = slot.handle.join();
    }
}

impl McpServer {
    pub fn new(db_path: PathBuf, options: McpOptions, tx: Sender<Value>) -> Result<McpServer> {
        let bus = Bus::open(Some(&db_path))?;
        let server = McpServer {
            bus,
            options,
            waits: Arc::new(Mutex::new(HashMap::new())),
            tx,
        };
        // Fail fast on a bad identity/operator mismatch, as the TS main() does.
        server.identify()?;
        Ok(server)
    }

    fn identify(&self) -> Result<Identity> {
        let identity = match &self.options.token {
            Some(token) => identity_for_token(&self.bus.conn, &self.options.agent_id, token)?,
            None => resolve_identity(&self.bus.conn, &self.bus.home, &self.options.agent_id)?,
        };
        if self.options.operator && identity.authority != "operator" {
            return Err(BusError::forbidden(
                "qagent mcp --operator needs the operator identity",
            ));
        }
        Ok(identity)
    }

    fn run(&self, id: &Value, f: impl FnOnce(&Identity) -> Result<String>) {
        let reply = match self.identify().and_then(|identity| f(&identity)) {
            Ok(text) => text_reply(id, text),
            Err(error) => error_reply(id, &error),
        };
        let _ = self.tx.send(reply);
    }

    /// Handle one JSON-RPC request/notification line.
    pub fn handle(&self, line: &str) {
        let request: Value = match serde_json::from_str(line.trim()) {
            Ok(value) => value,
            Err(_) => {
                let _ = self.tx.send(rpc_error(&Value::Null, -32700, "parse error"));
                return;
            }
        };
        let method = request.get("method").and_then(Value::as_str).unwrap_or("");
        let id = request.get("id").cloned();
        let params = request.get("params").cloned().unwrap_or(Value::Null);

        // Notifications carry no id.
        let Some(id) = id else {
            if method == "notifications/cancelled" {
                if let Some(request_id) = params.get("requestId") {
                    let key = request_id.to_string();
                    if let Some(slot) = self.waits.lock().unwrap().get(&key) {
                        slot.stop.store(true, Ordering::SeqCst);
                    }
                }
            }
            return;
        };

        match method {
            "initialize" => {
                let requested = params
                    .get("protocolVersion")
                    .and_then(Value::as_str)
                    .unwrap_or(PROTOCOL_VERSION);
                let _ = self.tx.send(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "protocolVersion": requested,
                        "capabilities": { "tools": {} },
                        "serverInfo": { "name": "qagent", "version": "2.0.0" },
                    }
                }));
            }
            "ping" => {
                let _ = self
                    .tx
                    .send(json!({ "jsonrpc": "2.0", "id": id, "result": {} }));
            }
            "tools/list" => {
                let tools = tool_descriptors(self.options.operator);
                let _ = self
                    .tx
                    .send(json!({ "jsonrpc": "2.0", "id": id, "result": { "tools": tools } }));
            }
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                let args = params
                    .get("arguments")
                    .cloned()
                    .filter(|a| a.is_object())
                    .unwrap_or_else(|| json!({}));
                if name == "bus_wait" {
                    self.spawn_wait(id, args);
                } else {
                    self.run(&id, |identity| self.call_tool(identity, name, &args));
                }
            }
            _ => {
                let _ = self.tx.send(rpc_error(
                    &id,
                    -32601,
                    &format!("method not found: {method}"),
                ));
            }
        }
    }

    fn spawn_wait(&self, id: Value, args: Value) {
        let timeout_arg = match arg_i64(&args, "timeout_sec") {
            Ok(v) => v,
            Err(e) => {
                let _ = self.tx.send(error_reply(&id, &e));
                return;
            }
        };
        // Resolve block seconds the way the TS does: arg, else QAGENT_BLOCK_SEC / AGENT_BUS_BLOCK_SEC.
        let env_default = (self.options.env)("QAGENT_BLOCK_SEC")
            .or_else(|| (self.options.env)("AGENT_BUS_BLOCK_SEC"))
            .and_then(|raw| raw.trim().parse::<i64>().ok());
        let seconds = match wait_seconds(timeout_arg, env_default) {
            Ok(s) => s,
            Err(e) => {
                let _ = self.tx.send(error_reply(&id, &e));
                return;
            }
        };

        let key = id.to_string();
        let stop = Arc::new(AtomicBool::new(false));
        let db_path = self.bus.db_path.clone();
        let agent_id = self.options.agent_id.clone();
        let token = self.options.token.clone();
        let operator = self.options.operator;
        let tx = self.tx.clone();
        let waits = self.waits.clone();
        let stop_for_worker = stop.clone();

        // Hold the map lock across spawn + insert: a worker that finishes
        // immediately must see its slot present when it removes itself.
        let mut map = self.waits.lock().unwrap();
        let key_for_worker = key.clone();
        let handle = std::thread::spawn(move || {
            let reply = wait_reply(
                &db_path,
                &agent_id,
                token.as_deref(),
                operator,
                seconds,
                &stop_for_worker,
                &id,
            );
            waits.lock().unwrap().remove(&key_for_worker);
            let _ = tx.send(reply);
        });
        map.insert(key, WaitSlot { stop, handle });
        drop(map);
    }

    fn call_tool(&self, identity: &Identity, name: &str, args: &Value) -> Result<String> {
        match name {
            "bus_whoami" => {
                let who = self.bus.whoami(identity)?;
                let roster = self.bus.list_agents()?;
                Ok(render_whoami(identity, &who, &roster))
            }
            "bus_agents" => Ok(render_agents(&self.bus.list_agents()?)),
            "bus_send" => {
                let to = arg_str_req(args, "to")?;
                let body = arg_str_req(args, "body")?;
                let messages = self.bus.send(
                    identity,
                    SendInput {
                        to,
                        subject: arg_str(args, "subject")?,
                        body,
                        msg_type: arg_str(args, "type")?,
                        thread: arg_str(args, "thread")?,
                        task_id: arg_i64(args, "task_id")?,
                        refs: args.get("refs").filter(|v| !v.is_null()).cloned(),
                        requires_ack: arg_bool(args, "requires_ack")?.unwrap_or(false),
                    },
                )?;
                Ok(render_sent(&messages))
            }
            "bus_inbox" => {
                let peek = arg_bool(args, "peek")?.unwrap_or(false);
                let limit = arg_i64(args, "limit")?;
                let result = self.bus.inbox(identity, peek, limit)?;
                Ok(render_inbox(&result.messages, result.remaining, peek))
            }
            "bus_ack" => {
                let seq = arg_i64(args, "seq")?
                    .ok_or_else(|| BusError::invalid("bus_ack: missing required argument 'seq'"))?;
                let (seq, _) = self.bus.ack(identity, seq)?;
                Ok(format!("Acknowledged #{seq}."))
            }
            "bus_task_create" => {
                let path_scopes = arg_str_vec(args, "path_scopes")?.unwrap_or_default();
                let project = match arg_str(args, "project")? {
                    Some(p) => Some(p),
                    None if !path_scopes.is_empty() => {
                        Some(self.options.cwd.to_string_lossy().into_owned())
                    }
                    None => None,
                };
                let task = self.bus.create_task(
                    identity,
                    CreateTaskInput {
                        title: arg_str_req(args, "title")?,
                        brief: Some(arg_str_req(args, "brief")?),
                        acceptance: arg_str(args, "acceptance")?,
                        to: arg_str(args, "to")?,
                        reviewer: None,
                        role: arg_str(args, "role")?,
                        priority: arg_str(args, "priority")?,
                        parent_id: arg_i64(args, "parent_id")?,
                        dependencies: arg_i64_vec(args, "dependencies")?,
                        path_scopes,
                        project,
                        refs: None,
                        max_retries: None,
                    },
                )?;
                Ok(render_task_line(&task, "Created"))
            }
            "bus_task_list" => {
                let mine = match arg_bool(args, "mine")? {
                    Some(false) => None,
                    _ => Some(identity.agent_id.clone()),
                };
                let tasks = self.bus.list_tasks(ListTasksInput {
                    mine,
                    states: arg_str_vec(args, "state")?,
                    include_closed: arg_bool(args, "include_closed")?.unwrap_or(false),
                    limit: arg_i64(args, "limit")?,
                })?;
                Ok(render_tasks(&tasks))
            }
            "bus_task_get" => {
                let task_id = arg_i64(args, "task_id")?.ok_or_else(|| {
                    BusError::invalid("bus_task_get: missing required argument 'task_id'")
                })?;
                Ok(render_task(&self.bus.get_task(task_id)?))
            }
            "bus_task_claim" => {
                let task = self.bus.claim_task(identity, arg_i64(args, "task_id")?)?;
                let detail = self.bus.get_task(task.id)?;
                Ok(format!(
                    "{}\n\n{}",
                    render_task_line(&task, "Claimed"),
                    render_task(&detail)
                ))
            }
            "bus_task_note" => {
                let task_id = arg_i64(args, "task_id")?.ok_or_else(|| {
                    BusError::invalid("bus_task_note: missing required argument 'task_id'")
                })?;
                let note_text = arg_str_req(args, "note")?;
                let note = self.bus.note_task(identity, task_id, &note_text)?;
                Ok(format!(
                    "Noted on task #{} (note {}).",
                    note.task_id, note.id
                ))
            }
            "bus_task_submit" => {
                let task_id = arg_i64(args, "task_id")?.ok_or_else(|| {
                    BusError::invalid("bus_task_submit: missing required argument 'task_id'")
                })?;
                let task = self.bus.submit_task(
                    identity,
                    task_id,
                    SubmitInput {
                        summary: arg_str_req(args, "summary")?,
                        details: arg_str(args, "details")?,
                        changed_files: arg_str_vec(args, "changed_files")?.unwrap_or_default(),
                        artifacts: args.get("artifacts").filter(|v| !v.is_null()).cloned(),
                        validation: args.get("validation").filter(|v| !v.is_null()).cloned(),
                    },
                )?;
                let reviewer = task
                    .reviewer
                    .clone()
                    .unwrap_or_else(|| task.creator.clone());
                Ok(format!(
                    "{}. Reviewer: {}.",
                    render_task_line(&task, "Submitted"),
                    reviewer
                ))
            }
            "bus_task_review" => {
                let task_id = arg_i64(args, "task_id")?.ok_or_else(|| {
                    BusError::invalid("bus_task_review: missing required argument 'task_id'")
                })?;
                let accepted = arg_bool(args, "accepted")?.ok_or_else(|| {
                    BusError::invalid("bus_task_review: missing required argument 'accepted'")
                })?;
                let feedback = arg_str_req(args, "feedback")?;
                let task = self
                    .bus
                    .review_task(identity, task_id, accepted, &feedback)?;
                let verb = match task.state.as_str() {
                    "accepted" => "Accepted",
                    "failed" => "Failed (retry limit)",
                    _ => "Requested changes on",
                };
                Ok(render_task_line(&task, verb))
            }
            "bus_task_cancel" => {
                let task_id = arg_i64(args, "task_id")?.ok_or_else(|| {
                    BusError::invalid("bus_task_cancel: missing required argument 'task_id'")
                })?;
                let reason = arg_str(args, "reason")?;
                let task = self.bus.cancel_task(identity, task_id, reason.as_deref())?;
                Ok(render_task_line(&task, "Cancelled"))
            }
            "bus_agent_add" if self.options.operator => {
                let id = arg_str_req(args, "id")?;
                let authority = arg_str(args, "authority")?;
                let (agent, token_path) = self.bus.add_agent(
                    identity,
                    &id,
                    arg_str(args, "role")?.as_deref(),
                    arg_str(args, "model")?.as_deref(),
                    arg_str(args, "harness")?.as_deref(),
                    arg_str(args, "parent")?.as_deref(),
                    authority.as_deref(),
                )?;
                Ok(format!(
                    "Added {} ({}). Token file: {}. Start its MCP server with QAGENT_AGENT_ID={}.",
                    agent.id,
                    authority.as_deref().unwrap_or("worker"),
                    token_path.display(),
                    agent.id
                ))
            }
            _ => Err(BusError::invalid(format!("unknown tool {name}"))),
        }
    }
}

fn wait_reply(
    db_path: &std::path::Path,
    agent_id: &str,
    token: Option<&str>,
    operator: bool,
    seconds: i64,
    stop: &AtomicBool,
    id: &Value,
) -> Value {
    // A waiting call gets its own connection: the main Bus is single-threaded.
    let work = || -> Result<String> {
        let bus = Bus::open(Some(db_path))?;
        let identity = match token {
            Some(token) => identity_for_token(&bus.conn, agent_id, token)?,
            None => resolve_identity(&bus.conn, &bus.home, agent_id)?,
        };
        if operator && identity.authority != "operator" {
            return Err(BusError::forbidden(
                "qagent mcp --operator needs the operator identity",
            ));
        }
        let result = wait_for_mail(&bus, &identity, Duration::from_secs(seconds as u64), stop)?;
        // A cancelled wait leaves the mail unread: nobody is listening for the reply.
        let cancelled = stop.load(Ordering::SeqCst);
        let delivered = if result.status == "mail" && !cancelled {
            Some(bus.inbox(&identity, false, Some(50))?)
        } else {
            None
        };
        Ok(render_wait(&result, delivered.as_ref(), seconds, cancelled))
    };
    match work() {
        Ok(text) => text_reply(id, text),
        Err(error) => error_reply(id, &error),
    }
}

// ---- renderers (port of src/mcp/render.ts) ----

fn render_whoami(
    identity: &Identity,
    who: &crate::bus::Whoami,
    roster: &[(crate::types::Agent, i64)],
) -> String {
    let agent = who.agent.as_ref();
    format!(
        "You are {} ({}{}{}).\nUnread: {}. Read cursor: {}. Bus: {}.\nMay delegate: {}. May review: {}.\n\nAgents:\n{}",
        identity.agent_id,
        identity.authority,
        agent
            .filter(|a| !a.role.is_empty())
            .map(|a| format!(", role {}", a.role))
            .unwrap_or_default(),
        agent
            .filter(|a| !a.model.is_empty())
            .map(|a| format!(", model {}", a.model))
            .unwrap_or_default(),
        who.unread,
        who.cursor,
        who.db_path,
        if identity.permissions.can_delegate { "yes" } else { "no" },
        if identity.permissions.can_review { "yes" } else { "no" },
        render_agents(roster),
    )
}

fn render_sent(messages: &[Message]) -> String {
    let to = messages
        .iter()
        .map(|m| format!("{} (#{})", m.recipient.as_deref().unwrap_or("*"), m.seq))
        .collect::<Vec<_>>()
        .join(", ");
    format!("Sent to {to}.")
}

fn render_inbox(messages: &[Message], remaining: i64, peek: bool) -> String {
    if messages.is_empty() {
        return "Inbox is empty.".into();
    }
    let tail = if remaining > 0 {
        format!("\n\n{remaining} more unread; call bus_inbox again.")
    } else {
        String::new()
    };
    format!(
        "{} message(s){}:\n\n{}{}",
        messages.len(),
        if peek { " (peek; not marked read)" } else { "" },
        render_messages(messages, ""),
        tail
    )
}

fn render_wait(
    result: &WaitResult,
    delivered: Option<&crate::bus::InboxResult>,
    seconds: i64,
    cancelled: bool,
) -> String {
    if let Some(delivered) = delivered {
        if !delivered.messages.is_empty() {
            let tail = if delivered.remaining > 0 {
                format!("\n\n{} more unread; call bus_inbox.", delivered.remaining)
            } else {
                String::new()
            };
            return format!(
                "{} new message(s):\n\n{}{}",
                delivered.messages.len(),
                render_messages(&delivered.messages, ""),
                tail
            );
        }
    }
    if result.status == "task" {
        let lines = result
            .events
            .iter()
            .map(render_event)
            .collect::<Vec<_>>()
            .join("\n");
        return format!(
            "Task activity that concerns you:\n{lines}\n\nUse bus_task_get for details."
        );
    }
    if cancelled {
        return "Wait cancelled.".into();
    }
    format!("Nothing arrived within {seconds}s. This is normal; call bus_wait again while work is outstanding.")
}

fn render_task_line(task: &crate::types::Task, verb: &str) -> String {
    format!(
        "{verb} task #{} [{}, round {}] {}{}",
        task.id,
        task.state,
        task.round,
        task.title,
        task.assignee
            .as_ref()
            .map(|a| format!(" (assignee {a})"))
            .unwrap_or_default()
    )
}

// ---- argument extraction ----

fn invalid(key: &str, msg: impl Into<String>) -> BusError {
    BusError::invalid(format!("{key}: {}", msg.into()))
}

fn arg_str(args: &Value, key: &str) -> Result<Option<String>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(invalid(key, "expected a string")),
    }
}

fn arg_str_req(args: &Value, key: &str) -> Result<String> {
    arg_str(args, key)?.ok_or_else(|| invalid(key, "missing required argument"))
}

fn arg_i64(args: &Value, key: &str) -> Result<Option<i64>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => n
            .as_i64()
            .or_else(|| n.as_f64().filter(|f| f.fract() == 0.0).map(|f| f as i64))
            .map(Some)
            .ok_or_else(|| invalid(key, "expected an integer")),
        Some(_) => Err(invalid(key, "expected an integer")),
    }
}

fn arg_bool(args: &Value, key: &str) -> Result<Option<bool>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(invalid(key, "expected a boolean")),
    }
}

fn arg_str_vec(args: &Value, key: &str) -> Result<Option<Vec<String>>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    Value::String(s) => out.push(s.clone()),
                    _ => return Err(invalid(key, "expected an array of strings")),
                }
            }
            Ok(Some(out))
        }
        Some(_) => Err(invalid(key, "expected an array of strings")),
    }
}

fn arg_i64_vec(args: &Value, key: &str) -> Result<Vec<i64>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                match item.as_i64() {
                    Some(v) => out.push(v),
                    None => return Err(invalid(key, "expected an array of integers")),
                }
            }
            Ok(out)
        }
        Some(_) => Err(invalid(key, "expected an array of integers")),
    }
}

// ---- tool descriptors for tools/list ----

fn str_prop(max: usize) -> Value {
    json!({ "type": "string", "maxLength": max })
}

fn ref_schema() -> Value {
    json!({
        "type": "object",
        "required": ["type", "value"],
        "properties": {
            "type": { "type": "string", "enum": ["path", "artifact", "summary", "commit", "url"] },
            "value": { "type": "string", "maxLength": limits::REF_VALUE },
            "description": { "type": "string", "maxLength": limits::REF_DESCRIPTION },
        },
        "additionalProperties": false,
    })
}

fn tool(name: &str, description: &str, schema: Value) -> Value {
    json!({ "name": name, "description": description, "inputSchema": schema })
}

pub fn tool_descriptors(operator: bool) -> Vec<Value> {
    let task_state_enum = json!({
        "type": "array",
        "items": { "type": "string", "enum": ["open", "blocked", "claimed", "submitted", "changes_requested", "accepted", "failed", "cancelled"] },
    });
    let mut tools = vec![
        tool(
            "bus_whoami",
            "Show your identity, authority, unread count and the agent roster. Call this first.",
            json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        ),
        tool(
            "bus_agents",
            "List every agent with role, status (idle, waiting, working, offline), unread count and model.",
            json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        ),
        tool(
            "bus_send",
            "Send a message as yourself. `to` is an agent id, a comma-separated list, or \"*\" for everyone. Point at large material with refs instead of pasting it.",
            json!({
                "type": "object",
                "required": ["to", "subject", "body"],
                "properties": {
                    "to": { "type": "string", "description": "Recipient id, \"a,b\", or \"*\"" },
                    "subject": str_prop(limits::SUBJECT),
                    "body": str_prop(limits::BODY),
                    "type": { "type": "string", "enum": ["info", "question", "answer"] },
                    "thread": str_prop(limits::THREAD),
                    "task_id": { "type": "integer", "exclusiveMinimum": 0 },
                    "refs": { "type": "array", "items": ref_schema(), "maxItems": limits::REF_COUNT },
                    "requires_ack": { "type": "boolean" },
                },
                "additionalProperties": false,
            }),
        ),
        tool(
            "bus_inbox",
            "Read new messages without blocking. Marks them read unless peek is true.",
            json!({
                "type": "object",
                "properties": {
                    "peek": { "type": "boolean" },
                    "limit": { "type": "integer", "minimum": 1 },
                },
                "additionalProperties": false,
            }),
        ),
        tool(
            "bus_wait",
            "Sleep without using tokens until mail or task activity for you arrives, then return it (mail is marked read). A timeout with nothing is normal; call again. Do not call this when a supervisor woke you.",
            json!({
                "type": "object",
                "properties": {
                    "timeout_sec": { "type": "integer", "minimum": 1, "maximum": MAX_WAIT_SEC, "description": "Default QAGENT_BLOCK_SEC or 240" },
                },
                "additionalProperties": false,
            }),
        ),
        tool(
            "bus_ack",
            "Acknowledge a message that asked for an acknowledgement, by its sequence number.",
            json!({
                "type": "object",
                "required": ["seq"],
                "properties": { "seq": { "type": "integer", "exclusiveMinimum": 0 } },
                "additionalProperties": false,
            }),
        ),
        tool(
            "bus_task_create",
            "Create a task. With `to` it is assigned and the assignee is sent the brief; without it any agent of the matching role may claim it. The brief must stand alone: the worker has none of your context. You review the result unless the operator does.",
            json!({
                "type": "object",
                "required": ["title", "brief"],
                "properties": {
                    "title": str_prop(limits::TITLE),
                    "brief": str_prop(limits::BRIEF),
                    "to": { "type": "string" },
                    "parent_id": { "type": "integer", "exclusiveMinimum": 0 },
                    "dependencies": { "type": "array", "items": { "type": "integer", "exclusiveMinimum": 0 } },
                    "path_scopes": { "type": "array", "items": { "type": "string" }, "description": "Paths the task will write, relative to project; leased while claimed" },
                    "acceptance": str_prop(limits::ACCEPTANCE),
                    "role": str_prop(limits::ROLE),
                    "project": { "type": "string", "description": "Project directory; defaults to the server's working directory when path_scopes are given" },
                    "priority": { "type": "string", "enum": ["low", "normal", "high", "urgent"] },
                },
                "additionalProperties": false,
            }),
        ),
        tool(
            "bus_task_list",
            "List tasks. By default only open tasks you created, hold, or review.",
            json!({
                "type": "object",
                "properties": {
                    "mine": { "type": "boolean", "description": "Default true" },
                    "state": task_state_enum,
                    "include_closed": { "type": "boolean" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 1000 },
                },
                "additionalProperties": false,
            }),
        ),
        tool(
            "bus_task_get",
            "Show one task: brief, acceptance, state, result, review, notes and its message thread.",
            json!({
                "type": "object",
                "required": ["task_id"],
                "properties": { "task_id": { "type": "integer", "exclusiveMinimum": 0 } },
                "additionalProperties": false,
            }),
        ),
        tool(
            "bus_task_claim",
            "Claim a task. Without task_id, takes the oldest open task assigned to you, or unassigned for your role. Claims expire after two hours without a note or submit.",
            json!({
                "type": "object",
                "properties": { "task_id": { "type": "integer", "exclusiveMinimum": 0 } },
                "additionalProperties": false,
            }),
        ),
        tool(
            "bus_task_note",
            "Add a progress note to a task. A note from the assignee also renews the claim.",
            json!({
                "type": "object",
                "required": ["task_id", "note"],
                "properties": {
                    "task_id": { "type": "integer", "exclusiveMinimum": 0 },
                    "note": str_prop(limits::NOTE),
                },
                "additionalProperties": false,
            }),
        ),
        tool(
            "bus_task_submit",
            "Submit your work on a claimed task for review: what you did, files changed, and validation you actually ran. Report failures honestly.",
            json!({
                "type": "object",
                "required": ["task_id", "summary"],
                "properties": {
                    "task_id": { "type": "integer", "exclusiveMinimum": 0 },
                    "summary": str_prop(limits::SUMMARY),
                    "details": str_prop(limits::DETAILS),
                    "changed_files": { "type": "array", "items": { "type": "string" }, "maxItems": limits::CHANGED_FILES },
                    "artifacts": { "type": "array", "items": ref_schema(), "maxItems": limits::REF_COUNT },
                    "validation": {
                        "type": "array",
                        "maxItems": limits::VALIDATION,
                        "items": {
                            "type": "object",
                            "required": ["passed", "summary"],
                            "properties": {
                                "passed": { "type": "boolean" },
                                "summary": { "type": "string" },
                                "command": { "type": "string" },
                            },
                            "additionalProperties": false,
                        },
                    },
                },
                "additionalProperties": false,
            }),
        ),
        tool(
            "bus_task_review",
            "Review submitted work: accepted true closes the task; false sends your feedback back for another round. Check the work before accepting.",
            json!({
                "type": "object",
                "required": ["task_id", "accepted", "feedback"],
                "properties": {
                    "task_id": { "type": "integer", "exclusiveMinimum": 0 },
                    "accepted": { "type": "boolean" },
                    "feedback": str_prop(limits::FEEDBACK),
                },
                "additionalProperties": false,
            }),
        ),
        tool(
            "bus_task_cancel",
            "Cancel a task you created (the operator may cancel any task).",
            json!({
                "type": "object",
                "required": ["task_id"],
                "properties": {
                    "task_id": { "type": "integer", "exclusiveMinimum": 0 },
                    "reason": str_prop(limits::REASON),
                },
                "additionalProperties": false,
            }),
        ),
    ];
    if operator {
        tools.push(tool(
            "bus_agent_add",
            "Operator only: register an agent and write its token file. The token itself is never returned.",
            json!({
                "type": "object",
                "required": ["id", "role"],
                "properties": {
                    "id": { "type": "string" },
                    "role": str_prop(limits::ROLE),
                    "model": str_prop(limits::MODEL),
                    "harness": str_prop(limits::MODEL),
                    "parent": { "type": "string" },
                    "authority": { "type": "string", "enum": ["worker", "manager"] },
                },
                "additionalProperties": false,
            }),
        ));
    }
    tools
}

/// Parse `qagent mcp` args (port of server.ts parseArgs). Returns (operator, help).
pub fn parse_args(argv: &[String]) -> Result<(bool, bool)> {
    let mut operator = false;
    let mut help = false;
    let mut index = 0;
    while index < argv.len() {
        let arg = &argv[index];
        match arg.as_str() {
            "--operator" => operator = true,
            "--help" | "-h" => help = true,
            "--db" => index += 1,
            _ if arg.starts_with("--db=") => {}
            _ => {
                return Err(BusError::invalid(format!(
                    "unknown argument: {arg}\n\n{USAGE}"
                )))
            }
        }
        index += 1;
    }
    Ok((operator, help))
}

/// Entry point for `qagent mcp` (port of server.ts main). Resolves when stdin
/// closes or on SIGINT/SIGTERM.
pub fn serve(db_path: PathBuf, options: McpOptions) -> i32 {
    let (tx, rx) = channel::<Value>();
    let server = match McpServer::new(db_path.clone(), options, tx.clone()) {
        Ok(server) => server,
        Err(error) => {
            eprintln!("qagent mcp: {}", render_error(&error));
            let code = error.code.as_str();
            return if code == "unauthorized" || code == "forbidden" {
                3
            } else {
                1
            };
        }
    };
    drop(tx);

    // Single writer: replies can complete out of order (waits block).
    let writer = std::thread::spawn(move || {
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        for reply in rx {
            let _ = writeln!(out, "{}", serde_json::to_string(&reply).unwrap_or_default());
            let _ = out.flush();
        }
    });

    // SIGINT/SIGTERM: abort waits so each records its idle status, then exit.
    let waits = server.waits.clone();
    let _ = ctrlc::set_handler(move || {
        let slots: Vec<Arc<AtomicBool>> = {
            let map = waits.lock().unwrap();
            map.values().map(|slot| slot.stop.clone()).collect()
        };
        for stop in slots {
            stop.store(true, Ordering::SeqCst);
        }
        std::process::exit(0);
    });

    let agent_id = server.options.agent_id.clone();
    eprintln!(
        "qagent mcp: serving {agent_id} over stdio ({})",
        db_path.display()
    );

    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        match line {
            Ok(line) if line.trim().is_empty() => continue,
            Ok(line) => server.handle(&line),
            Err(_) => break,
        }
    }
    drain_waits(&server.waits);
    drop(server);
    let _ = writer.join();
    0
}
