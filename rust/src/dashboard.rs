//! `qagent dashboard`: the optional read-mostly status page.
//!
//! Port of src/dashboard/{server,session,page,assets,entry}.ts — a hand-rolled
//! HTTP/1.1 server on 127.0.0.1, one thread per connection, SSE streams fed by
//! a single watch loop (one ChangeWatcher + one delta read per change, fanned
//! out to every open stream). Operator token never reaches the browser:
//! `qagent dashboard` / `qagent dashboard link` trade it for a single-use
//! ticket; the page trades the ticket for an HttpOnly SameSite=Strict cookie.

use crate::bus::Bus;
use crate::error::{BusError, Result};
use crate::identity::{base64url, identity_for_token, operator_token_path, read_token_file};
use crate::types::{Message, OPERATOR_ID};
use crate::watcher::{ChangeWatcher, ChangeWatcherOptions};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const DEFAULT_PORT: u16 = 11511;
pub const HOST: &str = "127.0.0.1";
pub const DEFAULT_SAFETY_MS: u64 = 10_000;
pub const DEFAULT_PING_MS: u64 = 25_000;
const LOOP_TIMEOUT_MS: u64 = 24 * 60 * 60_000;
const FAST_POLL_MS: u64 = 25;
const MAX_DELTA_EVENTS: i64 = 1000;
const MAX_STREAMS: usize = 32;
const MAX_BODY_BYTES: usize = 512 * 1024;
const SESSION_COOKIE: &str = "qagent_dash";
const DEFAULT_TICKET_TTL_MS: i64 = 5 * 60_000;
const DEFAULT_SESSION_TTL_MS: i64 = 12 * 60 * 60_000;

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---------- sessions (port of session.ts) ----------

pub struct Sessions {
    tickets: Mutex<HashMap<String, i64>>,
    sessions: Mutex<HashMap<String, i64>>,
    pub session_ttl_ms: i64,
    pub ticket_ttl_ms: i64,
}

impl Sessions {
    pub fn new(session_ttl_ms: Option<i64>, ticket_ttl_ms: Option<i64>) -> Sessions {
        Sessions {
            tickets: Mutex::new(HashMap::new()),
            sessions: Mutex::new(HashMap::new()),
            session_ttl_ms: session_ttl_ms.unwrap_or(DEFAULT_SESSION_TTL_MS),
            ticket_ttl_ms: ticket_ttl_ms.unwrap_or(DEFAULT_TICKET_TTL_MS),
        }
    }

    fn prune(&self) {
        let now = now_ms();
        self.tickets.lock().unwrap().retain(|_, exp| *exp > now);
        self.sessions.lock().unwrap().retain(|_, exp| *exp > now);
    }

    /// A single-use ticket. Only call this after the caller proved it holds the operator token.
    pub fn issue_ticket(&self) -> String {
        self.prune();
        let ticket = base64url(&rand::random::<[u8; 24]>());
        self.tickets
            .lock()
            .unwrap()
            .insert(ticket.clone(), now_ms() + self.ticket_ttl_ms);
        ticket
    }

    /// Trade a ticket for a session id. The ticket is consumed whether or not it is valid.
    pub fn exchange(&self, ticket: &str) -> std::result::Result<String, AuthError> {
        self.prune();
        let expires = self.tickets.lock().unwrap().remove(ticket);
        match expires {
            Some(exp) if exp > now_ms() => {
                let session = base64url(&rand::random::<[u8; 32]>());
                self.sessions
                    .lock()
                    .unwrap()
                    .insert(session.clone(), now_ms() + self.session_ttl_ms);
                Ok(session)
            }
            _ => Err(AuthError(
                401,
                "sign-in link is invalid or expired; run `qagent dashboard link`".into(),
            )),
        }
    }

    pub fn valid(&self, session: &str) -> bool {
        if session.is_empty() {
            return false;
        }
        let mut sessions = self.sessions.lock().unwrap();
        match sessions.get(session) {
            Some(&exp) if exp > now_ms() => true,
            Some(_) => {
                sessions.remove(session);
                false
            }
            None => false,
        }
    }

    pub fn revoke(&self, session: &str) {
        self.sessions.lock().unwrap().remove(session);
    }

    pub fn cookie(&self, session: &str) -> String {
        format!(
            "{SESSION_COOKIE}={session}; HttpOnly; SameSite=Strict; Path=/; Max-Age={}",
            (self.session_ttl_ms + 999) / 1000
        )
    }
}

pub struct AuthError(pub u16, pub String);

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.1)
    }
}

fn cookies(headers: &HashMap<String, String>) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for piece in headers
        .get("cookie")
        .cloned()
        .unwrap_or_default()
        .split(';')
    {
        let Some(index) = piece.find('=') else {
            continue;
        };
        if index == 0 {
            continue;
        }
        out.insert(
            piece[..index].trim().to_string(),
            piece[index + 1..].trim().to_string(),
        );
    }
    out
}

fn session_of(headers: &HashMap<String, String>) -> String {
    cookies(headers)
        .get(SESSION_COOKIE)
        .cloned()
        .unwrap_or_default()
}

/// Reject requests whose Host is not this loopback listener (DNS-rebinding guard).
fn require_loopback_host(
    headers: &HashMap<String, String>,
    port: u16,
) -> std::result::Result<(), AuthError> {
    let host = headers
        .get("host")
        .cloned()
        .unwrap_or_default()
        .to_lowercase();
    if host != format!("127.0.0.1:{port}") && host != format!("localhost:{port}") {
        return Err(AuthError(421, "unexpected Host header".into()));
    }
    Ok(())
}

/// Writes must come from this page: Origin equal to our own, no cross-site fetch metadata.
fn require_same_origin(headers: &HashMap<String, String>) -> std::result::Result<(), AuthError> {
    let (Some(origin), Some(host)) = (headers.get("origin"), headers.get("host")) else {
        return Err(AuthError(403, "same-origin request required".into()));
    };
    if origin != &format!("http://{host}") {
        return Err(AuthError(403, "cross-origin request rejected".into()));
    }
    if let Some(site) = headers.get("sec-fetch-site") {
        if site != "same-origin" {
            return Err(AuthError(403, "cross-site request rejected".into()));
        }
    }
    Ok(())
}

fn require_session(
    headers: &HashMap<String, String>,
    sessions: &Sessions,
) -> std::result::Result<(), AuthError> {
    let session = session_of(headers);
    if !sessions.valid(&session) {
        return Err(AuthError(
            401,
            "dashboard session missing or expired; run `qagent dashboard link`".into(),
        ));
    }
    Ok(())
}

// ---------- minimal HTTP ----------

pub struct Request {
    pub method: String,
    pub path: String,
    pub query: HashMap<String, String>,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

fn read_request(stream: &mut TcpStream) -> std::io::Result<Option<Request>> {
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    // Header block ends at CRLFCRLF.
    while !buf.ends_with(b"\r\n\r\n") {
        match stream.read(&mut byte) {
            Ok(0) => {
                if buf.is_empty() {
                    return Ok(None);
                }
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "eof",
                ));
            }
            Ok(_) => {
                buf.push(byte[0]);
                if buf.len() > 64 * 1024 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "headers too large",
                    ));
                }
            }
            Err(e) => return Err(e),
        }
    }
    let head = String::from_utf8_lossy(&buf).to_string();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_ascii_uppercase();
    let target = parts.next().unwrap_or("/").to_string();
    let (path, query_string) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target, String::new()),
    };
    let mut query = HashMap::new();
    for pair in query_string.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        query.insert(url_decode(k), url_decode(v));
    }
    let mut headers = HashMap::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    let mut body = Vec::new();
    if let Some(length) = headers
        .get("content-length")
        .and_then(|v| v.parse::<usize>().ok())
    {
        if length > MAX_BODY_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "request body too large",
            ));
        }
        body = vec![0u8; length];
        stream.read_exact(&mut body)?;
    }
    Ok(Some(Request {
        method,
        path,
        query,
        headers,
        body,
    }))
}

fn url_decode(value: &str) -> String {
    let mut out = Vec::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                if let Ok(b) = u8::from_str_radix(hex, 16) {
                    out.push(b);
                    i += 3;
                } else {
                    out.push(b'%');
                    i += 1;
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

fn security_headers(nonce: Option<&str>) -> Vec<(String, String)> {
    let csp = match nonce {
        Some(nonce) => format!(
            "default-src 'none'; script-src 'nonce-{nonce}'; style-src 'nonce-{nonce}'; connect-src 'self'; form-action 'none'; base-uri 'none'; frame-ancestors 'none'"
        ),
        None => "default-src 'none'; frame-ancestors 'none'".to_string(),
    };
    vec![
        ("content-security-policy".into(), csp),
        ("x-content-type-options".into(), "nosniff".into()),
        ("x-frame-options".into(), "DENY".into()),
        ("referrer-policy".into(), "no-referrer".into()),
        ("cache-control".into(), "no-store".into()),
    ]
}

fn status_text(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        421 => "Misdirected Request",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Status",
    }
}

fn send_response(
    stream: &mut TcpStream,
    status: u16,
    headers: Vec<(String, String)>,
    body: &[u8],
    head_only: bool,
) -> std::io::Result<()> {
    let mut head = format!("HTTP/1.1 {status} {}\r\n", status_text(status));
    for (name, value) in &headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str(&format!("content-length: {}\r\n", body.len()));
    head.push_str("connection: close\r\n\r\n");
    stream.write_all(head.as_bytes())?;
    if !head_only {
        stream.write_all(body)?;
    }
    stream.flush()
}

fn send_json(
    stream: &mut TcpStream,
    status: u16,
    value: Value,
    extra: Vec<(String, String)>,
    head_only: bool,
) -> std::io::Result<()> {
    let mut headers = security_headers(None);
    headers.push((
        "content-type".into(),
        "application/json; charset=utf-8".into(),
    ));
    headers.extend(extra);
    send_response(
        stream,
        status,
        headers,
        serde_json::to_string(&value).unwrap_or_default().as_bytes(),
        head_only,
    )
}

fn error_status(error: &ErrorKind) -> u16 {
    match error {
        ErrorKind::Internal(_) => 500,
        ErrorKind::Auth(status, _) => *status,
        ErrorKind::Bus(code, _) => match code {
            crate::error::Code::Unauthorized => 401,
            crate::error::Code::Forbidden => 403,
            crate::error::Code::NotFound => 404,
            crate::error::Code::Invalid => 400,
            crate::error::Code::Conflict => 409,
        },
    }
}

/// Routes throw either an AuthError (its own status), a BusError (code-mapped),
/// or an internal I/O error (500).
pub enum ErrorKind {
    Auth(u16, String),
    Bus(crate::error::Code, String),
    Internal(String),
}

fn handle_result(
    stream: &mut TcpStream,
    result: std::result::Result<(), ErrorKind>,
    head_only: bool,
) {
    if let Err(error) = result {
        let status = error_status(&error);
        let message = match &error {
            ErrorKind::Auth(_, m) | ErrorKind::Bus(_, m) | ErrorKind::Internal(m) => {
                if status == 500 {
                    "internal error".to_string()
                } else {
                    m.clone()
                }
            }
        };
        let _ = send_json(
            stream,
            status,
            json!({ "error": message }),
            vec![],
            head_only,
        );
        if status == 500 {
            eprintln!("qagent dashboard: {message}");
        }
    }
}

// ---------- reader (delta computation, using the batched queries) ----------

fn agent_view(summary: &crate::types::AgentSummary) -> Value {
    json!({
        "id": summary.id,
        "status": summary.stored_status,
        "waitUntilMs": summary.wait_until_ms,
        "lastSeenMs": summary.last_seen_ms,
    })
}

fn message_view(message: &Message) -> Value {
    let first = if !message.subject.trim().is_empty() {
        message.subject.trim()
    } else {
        message.body.trim()
    }
    .split('\n')
    .next()
    .unwrap_or("")
    .to_string();
    let line: String = first.chars().take(300).collect();
    json!({
        "seq": message.seq,
        "tsMs": message.ts_ms,
        "sender": message.sender,
        "recipient": message.recipient,
        "line": line,
    })
}

fn message_summary_view(message: &crate::types::MessageSummary) -> Value {
    let first = if !message.subject.trim().is_empty() {
        message.subject.trim()
    } else {
        message.body.trim()
    }
    .split('\n')
    .next()
    .unwrap_or("")
    .to_string();
    let line: String = first.chars().take(300).collect();
    json!({
        "seq": message.seq,
        "tsMs": message.ts_ms,
        "sender": message.sender,
        "recipient": message.recipient,
        "line": line,
    })
}

struct Reader<'a> {
    bus: &'a Bus,
}

impl<'a> Reader<'a> {
    fn state(&self) -> Result<Value> {
        let seq = self.bus.latest_seq()?;
        let last = if seq > 0 {
            self.bus.events(seq - 1, 1)?.into_iter().next()
        } else {
            None
        };
        let agents: Vec<Value> = self
            .bus
            .list_agents()?
            .into_iter()
            .filter(|(agent, _)| agent.id != OPERATOR_ID)
            .map(|(agent, _)| {
                json!({
                    "id": agent.id,
                    "status": agent.stored_status,
                    "waitUntilMs": agent.wait_until_ms,
                    "lastSeenMs": agent.last_seen_ms,
                })
            })
            .collect();
        let tasks: Vec<Value> = self
            .bus
            .list_tasks(crate::bus::ListTasksInput {
                mine: None,
                states: None,
                include_closed: false,
                limit: Some(200),
            })?
            .into_iter()
            .map(|task| {
                json!({
                    "id": task.id, "title": task.title, "assignee": task.assignee,
                    "state": task.state, "createdMs": task.created_ms, "closed": false,
                })
            })
            .collect();
        let mut messages: Vec<Value> = self
            .bus
            .get_messages(None, Some(100), None, None)?
            .iter()
            .map(message_view)
            .collect();
        messages.reverse();
        Ok(json!({
            "dbPath": self.bus.db_path,
            "seq": seq,
            "lastChangeMs": last.map(|e| e.ts_ms),
            "agents": agents,
            "tasks": tasks,
            "messages": messages,
        }))
    }

    /// Rows named by events (from, to]. None when the gap is too large for a
    /// delta; the page then reloads.
    fn delta(&self, from: i64, to: i64) -> Result<Option<Value>> {
        let events: Vec<crate::types::BusEvent> = self
            .bus
            .events(from, MAX_DELTA_EVENTS)?
            .into_iter()
            .filter(|e| e.seq <= to)
            .collect();
        if events.len() as i64 == MAX_DELTA_EVENTS
            && events.last().map(|e| e.seq < to).unwrap_or(false)
        {
            return Ok(None);
        }
        let mut agent_ids: Vec<String> = Vec::new();
        let mut task_ids: Vec<i64> = Vec::new();
        let mut message_seqs: Vec<i64> = Vec::new();
        for event in &events {
            if !agent_ids.contains(&event.actor) {
                agent_ids.push(event.actor.clone());
            }
            match event.entity.as_str() {
                "agent" => {
                    if !agent_ids.contains(&event.entity_id) {
                        agent_ids.push(event.entity_id.clone());
                    }
                }
                "task" => {
                    if let Ok(id) = event.entity_id.parse::<i64>() {
                        if !task_ids.contains(&id) {
                            task_ids.push(id);
                        }
                    }
                }
                "message" => {
                    if let Ok(seq) = event.entity_id.parse::<i64>() {
                        if !message_seqs.contains(&seq) {
                            message_seqs.push(seq);
                        }
                    }
                }
                _ => {}
            }
        }
        agent_ids.retain(|id| id != OPERATOR_ID && id != "system");
        let agents: Vec<Value> = self
            .bus
            .agent_summaries(&agent_ids)?
            .iter()
            .map(agent_view)
            .collect();
        let ids: Vec<i64> = task_ids
            .iter()
            .copied()
            .filter(|id| *id > 0)
            .take(500)
            .collect();
        let tasks: Vec<Value> = if ids.is_empty() {
            Vec::new()
        } else {
            self.bus
                .task_summaries(&ids)?
                .iter()
                .map(|task| {
                    json!({
                        "id": task.id, "title": task.title, "assignee": task.assignee,
                        "state": task.state, "createdMs": task.created_ms,
                        "closed": crate::types::CLOSED_STATES.contains(&task.state.as_str()),
                    })
                })
                .collect()
        };
        let mut seqs = message_seqs.clone();
        seqs.sort_unstable();
        let seqs: Vec<i64> = seqs.iter().copied().rev().take(100).collect();
        let messages: Vec<Value> = if seqs.is_empty() {
            Vec::new()
        } else {
            self.bus
                .message_summaries(&seqs)?
                .iter()
                .map(message_summary_view)
                .collect()
        };
        Ok(Some(json!({
            "seq": to,
            "lastChangeMs": events.last().map(|e| e.ts_ms),
            "events": events.iter().map(|e| json!({
                "seq": e.seq, "tsMs": e.ts_ms, "actor": e.actor,
                "kind": e.kind, "entity": e.entity, "entityId": e.entity_id,
            })).collect::<Vec<_>>(),
            "agents": agents,
            "tasks": tasks,
            "messages": messages,
        })))
    }
}

// ---------- hub: one watch loop fanning deltas to every SSE stream ----------

struct Hub {
    streams: Mutex<HashMap<u64, Sender<String>>>,
    seq: Mutex<i64>,
    abort: Arc<AtomicBool>,
    closed: AtomicBool,
    running: AtomicBool,
    next_id: AtomicU64,
    ping: Duration,
}

impl Hub {
    fn size(&self) -> usize {
        self.streams.lock().unwrap().len()
    }

    /// Wake the loop now; used after this process's own write, which data_version does not report.
    fn poke(&self) {
        self.abort.store(true, Ordering::SeqCst);
    }

    fn publish(&self, reader: &Reader, to: i64) -> Result<()> {
        let delta = reader.delta(*self.seq.lock().unwrap(), to)?;
        *self.seq.lock().unwrap() = to;
        let frame = match delta {
            Some(d) => format!(
                "id: {to}\nevent: change\ndata: {}\n\n",
                serde_json::to_string(&d)?
            ),
            None => format!("id: {to}\nevent: reset\ndata: {{}}\n\n"),
        };
        for sender in self.streams.lock().unwrap().values() {
            let _ = sender.send(frame.clone());
        }
        Ok(())
    }

    fn start_loop(self: &Arc<Self>, bus: &Bus) {
        if self.closed.load(Ordering::SeqCst) {
            return;
        }
        // Spawn the watch loop only once; it exits when no streams remain.
        if self.running.swap(true, Ordering::SeqCst) {
            return;
        }
        let hub = self.clone();
        let db_path = bus.db_path.clone();
        // The loop needs a Bus for delta reads; open a second connection.
        let loop_bus = match Bus::open(Some(&db_path)) {
            Ok(b) => b,
            Err(_) => {
                self.running.store(false, Ordering::SeqCst);
                return;
            }
        };
        let watcher = match ChangeWatcher::new(
            &db_path,
            ChangeWatcherOptions {
                min_poll_ms: FAST_POLL_MS,
                max_poll_ms: DEFAULT_SAFETY_MS,
                fs_watch: true,
            },
        ) {
            Ok(w) => w,
            Err(_) => {
                self.running.store(false, Ordering::SeqCst);
                return;
            }
        };
        std::thread::spawn(move || {
            let loop_reader = Reader { bus: &loop_bus };
            while !hub.closed.load(Ordering::SeqCst) && hub.size() > 0 {
                hub.abort.store(false, Ordering::SeqCst);
                let since = *hub.seq.lock().unwrap();
                match watcher.next(since, Duration::from_millis(LOOP_TIMEOUT_MS), &hub.abort) {
                    Ok(seq) => {
                        if seq > since {
                            let _ = hub.publish(&loop_reader, seq);
                        }
                    }
                    Err(_) => break,
                }
            }
            hub.running.store(false, Ordering::SeqCst);
        });
    }

    /// Register an SSE stream. Writes happen on the connection's own thread via
    /// a channel, so a slow client never blocks the watch loop.
    /// Register an SSE stream. Writes happen on the connection's own thread via
    /// a channel, so a slow client never blocks the watch loop.
    fn add(
        self: &Arc<Self>,
        mut stream: TcpStream,
        since: Option<i64>,
        bus: &Bus,
        reader: &Reader,
    ) -> std::io::Result<()> {
        let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream; charset=utf-8\r\ncache-control: no-store\r\nx-content-type-options: nosniff\r\nconnection: keep-alive\r\n\r\n";
        stream.write_all(head.as_bytes())?;
        stream.set_nodelay(true)?;
        stream.write_all(b"retry: 3000\n\n")?;
        stream.flush()?;

        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = channel::<String>();
        // Catch up a reconnecting client before it joins the broadcast.
        if let Some(since) = since {
            let seq = *self.seq.lock().unwrap();
            if since >= 0 && since < seq {
                let delta = reader
                    .delta(since, seq)
                    .map_err(|e| std::io::Error::other(e.message))?;
                match delta {
                    Some(d) => {
                        stream.write_all(
                            format!(
                                "id: {seq}\nevent: change\ndata: {}\n\n",
                                serde_json::to_string(&d).unwrap_or_default()
                            )
                            .as_bytes(),
                        )?;
                    }
                    None => {
                        stream.write_all(
                            format!("id: {seq}\nevent: reset\ndata: {{}}\n\n").as_bytes(),
                        )?;
                    }
                }
            }
        }
        self.streams.lock().unwrap().insert(id, tx);
        self.start_loop(bus);

        let hub = self.clone();
        std::thread::spawn(move || {
            let ping = hub.ping;
            loop {
                match rx.recv_timeout(ping) {
                    Ok(frame) => {
                        if stream.write_all(frame.as_bytes()).is_err() || stream.flush().is_err() {
                            break;
                        }
                    }
                    Err(RecvTimeoutError::Timeout) => {
                        if stream.write_all(b": ping\n\n").is_err() || stream.flush().is_err() {
                            break;
                        }
                    }
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
            let was_present = hub.streams.lock().unwrap().remove(&id).is_some();
            if was_present && hub.size() == 0 {
                hub.abort.store(true, Ordering::SeqCst); // idle()
            }
        });
        Ok(())
    }

    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.abort.store(true, Ordering::SeqCst);
        self.streams.lock().unwrap().clear(); // drops senders -> writer threads end
    }
}

// ---------- page rendering (port of page.ts + assets.ts) ----------

mod page {
    include!("dashboard_page.rs");
}

// ---------- server ----------

pub struct DashboardOptions {
    pub db_path: PathBuf,
    /// 0 picks a free port. Default 11511. Host is always 127.0.0.1.
    pub port: u16,
    pub safety_ms: Option<u64>,
    pub ping_ms: Option<u64>,
    pub session_ttl_ms: Option<i64>,
    pub ticket_ttl_ms: Option<i64>,
}

pub struct Dashboard {
    pub url: String,
    pub port: u16,
    pub stats: Arc<DashboardStats>,
    sessions: Arc<Sessions>,
    shutdown: Arc<AtomicBool>,
    hub: Arc<Hub>,
}

#[derive(Default)]
pub struct DashboardStats {
    pub queries: AtomicU64,
    pub deltas: AtomicU64,
    pub streams: AtomicU64,
}

impl Dashboard {
    /// A single-use sign-in address. The caller has already proved it holds the operator token.
    pub fn sign_in_url(&self) -> String {
        let ticket = self.sessions.issue_ticket();
        format!("http://{HOST}:{}/#t={ticket}", self.port)
    }

    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
        self.hub.close();
    }
}

pub fn start_dashboard(options: DashboardOptions) -> Result<Dashboard> {
    if !options.db_path.exists() {
        return Err(BusError::invalid(format!(
            "bus database not found: {} (run `qagent init`)",
            options.db_path.display()
        )));
    }
    let bus = Bus::open(Some(&options.db_path))?;
    let stats = Arc::new(DashboardStats::default());
    let safety_ms = options.safety_ms.unwrap_or(DEFAULT_SAFETY_MS).max(50);
    let ping_ms = options.ping_ms.unwrap_or(DEFAULT_PING_MS).max(1000);
    let sessions = Arc::new(Sessions::new(options.session_ttl_ms, options.ticket_ttl_ms));
    let hub = Arc::new(Hub {
        streams: Mutex::new(HashMap::new()),
        seq: Mutex::new(bus.latest_seq()?),
        abort: Arc::new(AtomicBool::new(false)),
        closed: AtomicBool::new(false),
        running: AtomicBool::new(false),
        next_id: AtomicU64::new(0),
        ping: Duration::from_millis(ping_ms),
    });

    let listener = TcpListener::bind((HOST, options.port)).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AddrInUse {
            BusError::invalid(format!(
                "port {} on {HOST} is in use; pass --port",
                options.port
            ))
        } else {
            BusError::invalid(format!("dashboard listen failed: {e}"))
        }
    })?;
    let port = listener.local_addr()?.port();

    let shutdown = Arc::new(AtomicBool::new(false));
    let dashboard = Dashboard {
        url: format!("http://{HOST}:{port}/"),
        port,
        stats: stats.clone(),
        sessions: sessions.clone(),
        shutdown: shutdown.clone(),
        hub: hub.clone(),
    };

    // Accept loop on a dedicated thread.
    let hub_loop = hub.clone();
    let sessions_loop = sessions.clone();
    let stats_loop = stats.clone();
    let shutdown_loop = shutdown.clone();
    let bus_path = options.db_path.clone();
    let _safety = safety_ms;
    std::thread::spawn(move || {
        let _ = listener.set_nonblocking(true);
        while !shutdown_loop.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((stream, _)) => {
                    let hub = hub_loop.clone();
                    let sessions = sessions_loop.clone();
                    let stats = stats_loop.clone();
                    let bus_path = bus_path.clone();
                    std::thread::spawn(move || {
                        handle_connection(stream, &bus_path, hub, sessions, stats, port);
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }
    });

    Ok(dashboard)
}

fn handle_connection(
    mut stream: TcpStream,
    db_path: &Path,
    hub: Arc<Hub>,
    sessions: Arc<Sessions>,
    stats: Arc<DashboardStats>,
    port: u16,
) {
    let request = match read_request(&mut stream) {
        Ok(Some(request)) => request,
        _ => return,
    };
    let result = route(
        &mut stream,
        &request,
        &ServerCtx {
            db_path,
            hub: &hub,
            sessions: &sessions,
            stats: &stats,
            port,
        },
    );
    if result.is_err() {
        // The route handler already answered, or the connection dropped.
    }
}

struct ServerCtx<'a> {
    db_path: &'a Path,
    hub: &'a Arc<Hub>,
    sessions: &'a Arc<Sessions>,
    stats: &'a Arc<DashboardStats>,
    port: u16,
}

fn route(stream: &mut TcpStream, req: &Request, cx: &ServerCtx) -> std::io::Result<()> {
    let db_path = cx.db_path;
    let hub = cx.hub;
    let sessions = cx.sessions;
    let stats = cx.stats;
    let port = cx.port;
    let head_only = req.method == "HEAD";
    let answer: std::result::Result<(), ErrorKind> = (|| {
        require_loopback_host(&req.headers, port).map_err(|e| ErrorKind::Auth(e.0, e.1))?;

        if req.path == "/health" && req.method == "GET" {
            return send_json(
                stream,
                200,
                json!({
                    "ok": true,
                    "streams": stats.streams.load(Ordering::SeqCst),
                    "queries": stats.queries.load(Ordering::SeqCst),
                    "deltas": stats.deltas.load(Ordering::SeqCst),
                }),
                vec![],
                head_only,
            )
            .map_err(|e| ErrorKind::Internal(e.to_string()));
        }

        let bus =
            Bus::open(Some(db_path)).map_err(|e| ErrorKind::Bus(e.code, e.message.clone()))?;
        let reader = Reader { bus: &bus };

        if req.path == "/" && (req.method == "GET" || req.method == "HEAD") {
            let nonce = base64url(&rand::random::<[u8; 16]>());
            let html = if sessions.valid(&session_of(&req.headers)) {
                let state = reader
                    .state()
                    .map_err(|e| ErrorKind::Bus(e.code, e.message.clone()))?;
                page::render_page(&state, &nonce, now_ms())
            } else {
                page::render_signed_out(&nonce)
            };
            let mut headers = security_headers(Some(&nonce));
            headers.push(("content-type".into(), "text/html; charset=utf-8".into()));
            return send_response(stream, 200, headers, html.as_bytes(), head_only)
                .map_err(|e| ErrorKind::Internal(e.to_string()));
        }

        if req.path == "/login" && req.method == "POST" {
            if req.headers.contains_key("origin") {
                require_same_origin(&req.headers).map_err(|e| ErrorKind::Auth(e.0, e.1))?;
            }
            require_json(req)?;
            let body = read_json(req)?;
            let token = body
                .get("operatorToken")
                .and_then(Value::as_str)
                .unwrap_or("");
            identity_for_token(&bus.conn, OPERATOR_ID, token)
                .map_err(|e| ErrorKind::Bus(e.code, e.message.clone()))?;
            let ticket = sessions.issue_ticket();
            return send_json(
                stream,
                200,
                json!({
                    "ticket": ticket,
                    "url": format!("http://{HOST}:{port}/#t={ticket}"),
                    "expiresInSeconds": (sessions.ticket_ttl_ms + 999) / 1000,
                }),
                vec![],
                head_only,
            )
            .map_err(|e| ErrorKind::Internal(e.to_string()));
        }

        if req.path == "/session" && req.method == "POST" {
            require_same_origin(&req.headers).map_err(|e| ErrorKind::Auth(e.0, e.1))?;
            require_json(req)?;
            let body = read_json(req)?;
            let ticket = body.get("ticket").and_then(Value::as_str).unwrap_or("");
            let session = sessions
                .exchange(ticket)
                .map_err(|e| ErrorKind::Auth(e.0, e.1))?;
            return send_json(
                stream,
                200,
                json!({ "authenticated": true }),
                vec![("set-cookie".into(), sessions.cookie(&session))],
                head_only,
            )
            .map_err(|e| ErrorKind::Internal(e.to_string()));
        }

        if req.path == "/logout" && req.method == "POST" {
            require_same_origin(&req.headers).map_err(|e| ErrorKind::Auth(e.0, e.1))?;
            sessions.revoke(&session_of(&req.headers));
            return send_json(
                stream,
                200,
                json!({ "authenticated": false }),
                vec![(
                    "set-cookie".into(),
                    format!("{SESSION_COOKIE}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0"),
                )],
                head_only,
            )
            .map_err(|e| ErrorKind::Internal(e.to_string()));
        }

        if req.path == "/api" || req.path.starts_with("/api/") {
            require_session(&req.headers, sessions).map_err(|e| ErrorKind::Auth(e.0, e.1))?;
            if req.method != "GET" && req.method != "HEAD" {
                require_same_origin(&req.headers).map_err(|e| ErrorKind::Auth(e.0, e.1))?;
            }

            if req.path == "/api/state" && req.method == "GET" {
                stats.queries.fetch_add(5, Ordering::SeqCst);
                let state = reader
                    .state()
                    .map_err(|e| ErrorKind::Bus(e.code, e.message.clone()))?;
                return send_json(stream, 200, state, vec![], head_only)
                    .map_err(|e| ErrorKind::Internal(e.to_string()));
            }

            if req.path == "/api/events" && req.method == "GET" {
                if hub.size() >= MAX_STREAMS {
                    return send_json(
                        stream,
                        503,
                        json!({ "error": "too many open streams" }),
                        vec![],
                        head_only,
                    )
                    .map_err(|e| ErrorKind::Internal(e.to_string()));
                }
                let since = req
                    .headers
                    .get("last-event-id")
                    .and_then(|v| v.parse::<i64>().ok())
                    .or_else(|| req.query.get("since").and_then(|v| v.parse::<i64>().ok()));
                // Steal the socket: the SSE writer thread owns it from here.
                let owned = stream
                    .try_clone()
                    .map_err(|e| ErrorKind::Internal(e.to_string()))?;
                stats.streams.fetch_add(1, Ordering::SeqCst);
                let _ = hub.add(owned, since, &bus, &reader);
                stats.streams.store(hub.size() as u64, Ordering::SeqCst);
                return Ok(()); // SSE lives on its own thread
            }

            if let Some(rest) = req.path.strip_prefix("/api/task/") {
                if req.method == "GET" {
                    if let Ok(id) = rest.parse::<i64>() {
                        stats.queries.fetch_add(1, Ordering::SeqCst);
                        let detail = bus
                            .get_task(id)
                            .map_err(|e| ErrorKind::Bus(e.code, e.message.clone()))?;
                        return send_json(
                            stream,
                            200,
                            serde_json::to_value(&detail).unwrap_or_default(),
                            vec![],
                            head_only,
                        )
                        .map_err(|e| ErrorKind::Internal(e.to_string()));
                    }
                }
            }

            if req.path == "/api/send" && req.method == "POST" {
                require_json(req)?;
                let body = read_json(req)?;
                let text = body
                    .get("text")
                    .or_else(|| body.get("body"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                stats.queries.fetch_add(1, Ordering::SeqCst);
                let operator = bus
                    .identify(Some(OPERATOR_ID))
                    .map_err(|e| ErrorKind::Bus(e.code, e.message.clone()))?;
                let sent = bus
                    .send(
                        &operator,
                        crate::bus::SendInput {
                            to: body
                                .get("to")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string(),
                            subject: body
                                .get("subject")
                                .and_then(Value::as_str)
                                .map(str::to_string),
                            body: text,
                            msg_type: None,
                            thread: None,
                            task_id: None,
                            refs: None,
                            requires_ack: false,
                        },
                    )
                    .map_err(|e| ErrorKind::Bus(e.code, e.message.clone()))?;
                hub.poke();
                return send_json(
                    stream,
                    200,
                    json!({
                        "sent": sent.iter().map(|m| json!({ "seq": m.seq, "to": m.recipient.clone().unwrap_or_else(|| "*".into()) })).collect::<Vec<_>>(),
                    }),
                    vec![],
                    head_only,
                )
                .map_err(|e| ErrorKind::Internal(e.to_string()));
            }

            return send_json(
                stream,
                404,
                json!({ "error": "not found" }),
                vec![],
                head_only,
            )
            .map_err(|e| ErrorKind::Internal(e.to_string()));
        }

        send_json(
            stream,
            404,
            json!({ "error": "not found" }),
            vec![],
            head_only,
        )
        .map_err(|e| ErrorKind::Internal(e.to_string()))
    })();

    handle_result(stream, answer, head_only);
    Ok(())
}

fn require_json(req: &Request) -> std::result::Result<(), ErrorKind> {
    let ok = req
        .headers
        .get("content-type")
        .map(|v| v.to_lowercase().starts_with("application/json"))
        .unwrap_or(false);
    if !ok {
        return Err(ErrorKind::Bus(
            crate::error::Code::Invalid,
            "content-type must be application/json".into(),
        ));
    }
    Ok(())
}

fn read_json(req: &Request) -> std::result::Result<serde_json::Map<String, Value>, ErrorKind> {
    let text = String::from_utf8_lossy(&req.body);
    let value: Value = if text.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(&text).map_err(|_| {
            ErrorKind::Bus(
                crate::error::Code::Invalid,
                "request body must be a JSON object".into(),
            )
        })?
    };
    match value {
        Value::Object(map) => Ok(map),
        _ => Ok(serde_json::Map::new()),
    }
}

// ---------- `qagent dashboard` + `qagent dashboard link` (port of entry.ts) ----------

pub const DASHBOARD_USAGE: &str = "usage: qagent dashboard [--port N]        serve on 127.0.0.1 (default 11511, or QAGENT_DASHBOARD_PORT)\n       qagent dashboard link [--port N]   print a fresh single-use sign-in address for a running dashboard\n";

fn operator_token(db_path: &Path) -> Result<String> {
    let path = operator_token_path(&crate::db::home_for(db_path));
    read_token_file(&path).ok_or_else(|| {
        BusError::invalid(format!(
            "no operator token at {} (run `qagent init`)",
            path.display()
        ))
    })
}

/// POST /login with the operator token; print the single-use sign-in address.
pub fn dashboard_link(db_path: &Path, port: u16) -> Result<i32> {
    let token = operator_token(db_path)?;
    let request = format!(
        "POST /login HTTP/1.1\r\nhost: {HOST}:{port}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        token.len() + 20,
        json!({ "operatorToken": token })
    );
    let mut stream = match TcpStream::connect((HOST, port)) {
        Ok(s) => s,
        Err(_) => {
            eprintln!("qagent: no dashboard on {HOST}:{port}; start it with `qagent dashboard`");
            return Ok(1);
        }
    };
    stream.write_all(request.as_bytes())?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response)?;
    let text = String::from_utf8_lossy(&response).to_string();
    let body = text.split("\r\n\r\n").nth(1).unwrap_or("");
    let parsed: Value = serde_json::from_str(body).unwrap_or(json!({}));
    let status_ok = text.starts_with("HTTP/1.1 200") || text.starts_with("HTTP/1.0 200");
    match (status_ok, parsed.get("url").and_then(Value::as_str)) {
        (true, Some(url)) => {
            println!("{url}");
            Ok(0)
        }
        _ => {
            let error = parsed
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            eprintln!("qagent: dashboard refused the operator token: {error}");
            Ok(3)
        }
    }
}

/// Serve the dashboard until SIGINT/SIGTERM.
pub fn dashboard_serve(db_path: &Path, port: u16) -> Result<i32> {
    let token = operator_token(db_path)?;
    // Fail fast on a wrong operator token, as the TS entry does.
    {
        let conn = rusqlite::Connection::open(db_path)?;
        identity_for_token(&conn, OPERATOR_ID, &token)?;
    }
    let dashboard = start_dashboard(DashboardOptions {
        db_path: db_path.to_path_buf(),
        port,
        safety_ms: None,
        ping_ms: None,
        session_ttl_ms: None,
        ticket_ttl_ms: None,
    })?;
    println!(
        "qagent dashboard on {} (database {})",
        dashboard.url,
        db_path.display()
    );
    println!(
        "sign in (single use, 5 minutes): {}",
        dashboard.sign_in_url()
    );
    println!(
        "later links: qagent dashboard link{}",
        if port == DEFAULT_PORT {
            String::new()
        } else {
            format!(" --port {port}")
        }
    );
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let _ = ctrlc::set_handler(move || stop2.store(true, Ordering::SeqCst));
    while !stop.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(100));
    }
    dashboard.shutdown();
    Ok(0)
}
