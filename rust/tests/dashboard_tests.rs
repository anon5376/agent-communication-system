//! Dashboard E2E: real `qagent dashboard` process, raw HTTP over TcpStream.
//! Covers sign-in ticket -> session cookie, /api/state, SSE change delivery,
//! /api/send, and the auth guards (Host + session + same-origin).
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

fn qagent() -> &'static str {
    env!("CARGO_BIN_EXE_qagent")
}

struct TempDir(std::path::PathBuf);
static NEXT_TEMP_DIR_ID: AtomicU64 = AtomicU64::new(0);

fn fresh_dir() -> TempDir {
    loop {
        let dir = std::env::temp_dir().join(format!(
            "acs-dash-test-{}-{}",
            std::process::id(),
            NEXT_TEMP_DIR_ID.fetch_add(1, Ordering::Relaxed)
        ));
        match std::fs::create_dir(&dir) {
            Ok(()) => return TempDir(dir),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => panic!("create dashboard test directory {}: {error}", dir.display()),
        }
    }
}
impl std::ops::Deref for TempDir {
    type Target = std::path::Path;
    fn deref(&self) -> &std::path::Path {
        &self.0
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run(db: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(qagent())
        .args(args)
        .arg("--db")
        .arg(db)
        .output()
        .unwrap()
}

struct Dash {
    child: Child,
    port: u16,
}
impl Drop for Dash {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start_dashboard(db: &std::path::Path) -> Dash {
    let port = portpicker();
    let mut child = Command::new(qagent())
        .args(["dashboard", "--port", &port.to_string(), "--db"])
        .arg(db)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    // Wait for the listener to accept.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match TcpStream::connect(("127.0.0.1", port)) {
            Ok(_) => break,
            Err(_) => {
                assert!(Instant::now() < deadline, "dashboard did not start");
                assert!(child.try_wait().unwrap().is_none(), "dashboard exited");
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
    Dash { child, port }
}

fn portpicker() -> u16 {
    TcpListener::bind_or_free()
}

struct HttpResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

fn request(port: u16, req: &str) -> HttpResponse {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.write_all(req.as_bytes()).unwrap();
    stream.flush().unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).unwrap();
    parse_response(&response)
}

fn parse_response(raw: &[u8]) -> HttpResponse {
    let text = String::from_utf8_lossy(raw).to_string();
    let mut split = text.splitn(2, "\r\n\r\n");
    let head = split.next().unwrap_or("");
    let body = split.next().unwrap_or("").to_string();
    let mut lines = head.split("\r\n");
    let status: u16 = lines
        .next()
        .unwrap_or("")
        .split_whitespace()
        .nth(1)
        .unwrap_or("0")
        .parse()
        .unwrap_or(0);
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_lowercase(), v.trim().to_string()))
        .collect();
    HttpResponse {
        status,
        headers,
        body,
    }
}

struct TcpListener;
impl TcpListener {
    fn bind_or_free() -> u16 {
        std::net::TcpListener::bind(("127.0.0.1", 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }
}

fn get(port: u16, path: &str, cookie: Option<&str>) -> HttpResponse {
    let cookie = cookie
        .map(|c| format!("cookie: {c}\r\n"))
        .unwrap_or_default();
    request(
        port,
        &format!(
            "GET {path} HTTP/1.1\r\nhost: 127.0.0.1:{port}\r\n{cookie}connection: close\r\n\r\n"
        ),
    )
}

fn post_json(
    port: u16,
    path: &str,
    body: &str,
    cookie: Option<&str>,
    origin: bool,
) -> HttpResponse {
    let cookie = cookie
        .map(|c| format!("cookie: {c}\r\n"))
        .unwrap_or_default();
    let origin = if origin {
        format!("origin: http://127.0.0.1:{port}\r\nsec-fetch-site: same-origin\r\n")
    } else {
        String::new()
    };
    request(
        port,
        &format!(
            "POST {path} HTTP/1.1\r\nhost: 127.0.0.1:{port}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n{origin}{cookie}connection: close\r\n\r\n{body}",
            body.len()
        ),
    )
}

fn operator_token(db: &std::path::Path) -> String {
    std::fs::read_to_string(db.parent().unwrap().join("operator.token"))
        .unwrap()
        .trim()
        .to_string()
}

fn session_cookie(dash: &Dash, db: &std::path::Path) -> String {
    // Local sign-in: POST /login with the operator token (no Origin — a local
    // process is not a browser), then trade the ticket for a cookie.
    let token = operator_token(db);
    let login = post_json(
        dash.port,
        "/login",
        &format!("{{\"operatorToken\":\"{token}\"}}"),
        None,
        false,
    );
    assert_eq!(login.status, 200, "login: {}", login.body);
    let url = serde_json::from_str::<serde_json::Value>(&login.body).unwrap()["url"]
        .as_str()
        .unwrap()
        .to_string();
    let ticket = url.split("#t=").nth(1).unwrap();
    let session = post_json(
        dash.port,
        "/session",
        &format!("{{\"ticket\":\"{ticket}\"}}"),
        None,
        true,
    );
    assert_eq!(session.status, 200, "session: {}", session.body);
    session
        .headers
        .iter()
        .find(|(k, _)| k == "set-cookie")
        .map(|(_, v)| v.split(';').next().unwrap().to_string())
        .unwrap()
}

#[test]
fn dashboard_auth_flow_and_state() {
    let dir = fresh_dir();
    let db = dir.join("bus.db");
    let out = run(&db, &["init"]);
    assert!(out.status.success(), "init: {:?}", out);
    let out = run(&db, &["agent", "add", "web", "--role", "coder"]);
    assert!(out.status.success());
    let dash = start_dashboard(&db);

    // / without a session -> signed-out page.
    let page = get(dash.port, "/", None);
    assert_eq!(page.status, 200);
    assert!(page.body.contains("not signed in"), "body: {}", page.body);
    assert!(page
        .headers
        .iter()
        .any(|(k, v)| k == "content-security-policy" && v.contains("default-src 'none'")));

    // Bad Host -> 421 (DNS-rebinding guard).
    let bad = request(
        dash.port,
        "GET /health HTTP/1.1\r\nhost: evil.example.com\r\nconnection: close\r\n\r\n",
    );
    assert_eq!(bad.status, 421, "expected 421 for foreign Host");

    // /api/state without session -> 401.
    let noauth = get(dash.port, "/api/state", None);
    assert_eq!(noauth.status, 401);

    // Full sign-in -> authed page + state.
    let cookie = session_cookie(&dash, &db);
    let page = get(dash.port, "/", Some(&cookie));
    assert!(page.body.contains("Qagent"), "authed body missing");
    assert!(page.body.contains("web"), "agent row missing");

    let state = get(dash.port, "/api/state", Some(&cookie));
    assert_eq!(state.status, 200);
    let state: serde_json::Value = serde_json::from_str(&state.body).unwrap();
    assert_eq!(state["agents"].as_array().unwrap()[0]["id"], "web");
    assert!(state["seq"].as_i64().unwrap() > 0);

    // /api/send posts as operator; the agent's inbox sees it.
    let sent = post_json(
        dash.port,
        "/api/send",
        &serde_json::json!({ "to": "web", "text": "hello from the page" }).to_string(),
        Some(&cookie),
        true,
    );
    assert_eq!(sent.status, 200, "send: {}", sent.body);
    let inbox = run(&db, &["inbox", "--as", "web"]);
    assert!(String::from_utf8(inbox.stdout)
        .unwrap()
        .contains("hello from the page"));
}

#[test]
fn dashboard_sse_pushes_changes() {
    let dir = fresh_dir();
    let db = dir.join("bus.db");
    assert!(run(&db, &["init"]).status.success());
    assert!(run(&db, &["agent", "add", "bot", "--role", "coder"])
        .status
        .success());
    let dash = start_dashboard(&db);
    let cookie = session_cookie(&dash, &db);

    // Open the SSE stream.
    let mut stream = TcpStream::connect(("127.0.0.1", dash.port)).unwrap();
    stream
        .write_all(
            format!(
                "GET /api/events?since=1 HTTP/1.1\r\nhost: 127.0.0.1:{}\r\ncookie: {cookie}\r\nconnection: keep-alive\r\n\r\n",
                dash.port
            )
            .as_bytes(),
        )
        .unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut head = String::new();
    while !head.ends_with("\r\n\r\n") {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        head.push_str(&line);
    }
    assert!(head.contains("text/event-stream"), "SSE head: {head}");

    // A send from the CLI must arrive as a change event.
    let out = run(&db, &["send", "bot", "ping", "body", "--as", "operator"]);
    assert!(out.status.success());

    let deadline = Instant::now() + Duration::from_secs(15);
    let mut got_change = false;
    let mut buf = String::new();
    while Instant::now() < deadline {
        stream
            .set_read_timeout(Some(deadline.saturating_duration_since(Instant::now())))
            .unwrap();
        let mut byte = [0u8; 512];
        match stream.read(&mut byte) {
            Ok(0) => break,
            Ok(n) => {
                buf.push_str(&String::from_utf8_lossy(&byte[..n]));
                if buf.contains("event: change") && buf.contains("message") {
                    got_change = true;
                    break;
                }
            }
            Err(_) => break,
        }
    }
    assert!(got_change, "SSE stream never saw the send; got: {buf}");

    // health reports the open stream.
    let health = get(dash.port, "/health", None);
    assert!(
        health.body.contains("\"streams\":1") || health.body.contains("\"streams\": 1"),
        "health: {}",
        health.body
    );
}

#[test]
fn dashboard_link_prints_signin_url() {
    let dir = fresh_dir();
    let db = dir.join("bus.db");
    assert!(run(&db, &["init"]).status.success());
    let dash = start_dashboard(&db);
    let out = run(
        &db,
        &["dashboard", "link", "--port", &dash.port.to_string()],
    );
    assert!(out.status.success());
    let url = String::from_utf8(out.stdout).unwrap().trim().to_string();
    assert!(url.contains("/#t="), "link output: {url}");
}
