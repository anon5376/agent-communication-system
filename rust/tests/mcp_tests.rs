//! MCP stdio server tests: spawn the real `qagent mcp` binary, talk JSON-RPC
//! over pipes, assert protocol + tool behaviour matches the TS server.
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

fn qagent() -> &'static str {
    env!("CARGO_BIN_EXE_qagent")
}

struct TempDir(std::path::PathBuf);

fn fresh_dir() -> TempDir {
    let dir = std::env::temp_dir().join(format!(
        "acs-mcp-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    TempDir(dir)
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

fn init_bus(dir: &std::path::Path) -> std::path::PathBuf {
    let db = dir.join("bus.db");
    let out = Command::new(qagent())
        .args(["init", "--db"])
        .arg(&db)
        .output()
        .unwrap();
    assert!(out.status.success(), "init failed: {:?}", out);
    db
}

fn add_agent(db: &std::path::Path, id: &str, role: &str) {
    // Delegating needs manager authority; a "manager" role here stands for that.
    let authority = if role == "manager" {
        "manager"
    } else {
        "worker"
    };
    let out = Command::new(qagent())
        .args([
            "agent",
            "add",
            id,
            "--role",
            role,
            "--authority",
            authority,
            "--db",
        ])
        .arg(db)
        .output()
        .unwrap();
    assert!(out.status.success(), "agent add failed: {:?}", out);
}

struct Mcp {
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
    next_id: u64,
}

impl Mcp {
    fn spawn(db: &std::path::Path, agent: &str, extra_args: &[&str]) -> Mcp {
        let mut args: Vec<String> = vec!["--db".into(), db.display().to_string(), "mcp".into()];
        args.extend(extra_args.iter().map(|s| s.to_string()));
        let mut child = Command::new(qagent())
            .args(&args)
            .env("QAGENT_AGENT_ID", agent)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        Mcp {
            child,
            stdin,
            reader: BufReader::new(stdout),
            next_id: 0,
        }
    }

    fn send(&mut self, method: &str, params: serde_json::Value) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        let line = serde_json::json!({
            "jsonrpc": "2.0", "id": id, "method": method, "params": params,
        })
        .to_string();
        writeln!(self.stdin, "{line}").unwrap();
        self.stdin.flush().unwrap();
        id
    }

    fn call(&mut self, name: &str, args: serde_json::Value) -> u64 {
        self.send(
            "tools/call",
            serde_json::json!({ "name": name, "arguments": args }),
        )
    }

    fn read_reply(&mut self, timeout: Duration) -> serde_json::Value {
        let deadline = Instant::now() + timeout;
        loop {
            let mut line = String::new();
            self.reader.read_line(&mut line).unwrap();
            if line.trim().is_empty() {
                assert!(Instant::now() < deadline, "timed out waiting for reply");
                continue;
            }
            return serde_json::from_str(&line).unwrap();
        }
    }

    fn reply_for(&mut self, want_id: u64, timeout: Duration) -> serde_json::Value {
        let deadline = Instant::now() + timeout;
        loop {
            let reply = self.read_reply(deadline.saturating_duration_since(Instant::now()));
            if reply["id"] == want_id {
                return reply;
            }
        }
    }

    fn text_of(&mut self, want_id: u64) -> String {
        self.reply_for(want_id, Duration::from_secs(15))["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string()
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn mcp_initialize_and_tools() {
    let dir = fresh_dir();
    let db = init_bus(&dir);
    add_agent(&db, "worker", "coder");
    let mut mcp = Mcp::spawn(&db, "worker", &[]);

    let id = mcp.send(
        "initialize",
        serde_json::json!({ "protocolVersion": "2024-11-05", "capabilities": {} }),
    );
    let reply = mcp.reply_for(id, Duration::from_secs(10));
    assert_eq!(reply["result"]["serverInfo"]["name"], "qagent");
    assert_eq!(reply["result"]["protocolVersion"], "2024-11-05");

    let id = mcp.send("tools/list", serde_json::json!({}));
    let reply = mcp.reply_for(id, Duration::from_secs(10));
    let tools = reply["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    for expected in [
        "bus_whoami",
        "bus_send",
        "bus_inbox",
        "bus_wait",
        "bus_task_create",
        "bus_task_claim",
        "bus_task_submit",
    ] {
        assert!(names.contains(&expected), "missing tool {expected}");
    }
    // Non-operator server must not expose bus_agent_add.
    assert!(!names.contains(&"bus_agent_add"));
}

#[test]
fn mcp_whoami_send_inbox_round_trip() {
    let dir = fresh_dir();
    let db = init_bus(&dir);
    add_agent(&db, "alice", "coder");
    let mut mcp = Mcp::spawn(&db, "alice", &[]);

    let id = mcp.call("bus_whoami", serde_json::json!({}));
    let text = mcp.text_of(id);
    assert!(text.contains("You are alice"), "whoami: {text}");
    assert!(text.contains("Agents:"));

    let id = mcp.call(
        "bus_send",
        serde_json::json!({ "to": "alice", "subject": "hi", "body": "hello self" }),
    );
    let text = mcp.text_of(id);
    assert!(text.contains("Sent to alice (#1)"), "send: {text}");

    let id = mcp.call("bus_inbox", serde_json::json!({}));
    let text = mcp.text_of(id);
    assert!(text.contains("1 message(s)"), "inbox: {text}");
    assert!(text.contains("hello self"));

    // Second inbox is empty.
    let id = mcp.call("bus_inbox", serde_json::json!({}));
    assert_eq!(mcp.text_of(id), "Inbox is empty.");
}

#[test]
fn mcp_wait_times_out_then_wakes() {
    let dir = fresh_dir();
    let db = init_bus(&dir);
    add_agent(&db, "bob", "coder");
    add_agent(&db, "carol", "coder");
    let mut mcp = Mcp::spawn(&db, "bob", &[]);
    mcp.send(
        "initialize",
        serde_json::json!({ "protocolVersion": "2024-11-05", "capabilities": {} }),
    );
    let _ = mcp.read_reply(Duration::from_secs(10));

    // Short wait -> timeout reply.
    let id = mcp.call("bus_wait", serde_json::json!({ "timeout_sec": 1 }));
    let text = mcp.text_of(id);
    assert!(text.contains("Nothing arrived within"), "wait: {text}");

    // A mail sent while waiting is delivered into the wait reply.
    let id = mcp.call("bus_wait", serde_json::json!({ "timeout_sec": 10 }));
    std::thread::sleep(Duration::from_millis(300));
    let out = Command::new(qagent())
        .args(["send", "bob", "wake", "mail body", "--as", "carol", "--db"])
        .arg(&db)
        .output()
        .unwrap();
    assert!(out.status.success(), "send failed: {:?}", out);
    let text = mcp.text_of(id);
    assert!(
        text.contains("new message(s)") && text.contains("mail body"),
        "wait woke: {text}"
    );
}

#[test]
fn mcp_task_lifecycle_via_tools() {
    let dir = fresh_dir();
    let db = init_bus(&dir);
    add_agent(&db, "maker", "manager");
    add_agent(&db, "doer", "coder");
    let mut maker = Mcp::spawn(&db, "maker", &[]);
    let mut doer = Mcp::spawn(&db, "doer", &[]);

    let id = maker.call(
        "bus_task_create",
        serde_json::json!({ "title": "do a thing", "brief": "the brief", "to": "doer" }),
    );
    let text = maker.text_of(id);
    assert!(text.contains("Created task #1"), "create: {text}");

    let id = doer.call("bus_task_claim", serde_json::json!({ "task_id": 1 }));
    let text = doer.text_of(id);
    assert!(text.contains("Claimed task #1"), "claim: {text}");

    let id = doer.call(
        "bus_task_submit",
        serde_json::json!({ "task_id": 1, "summary": "did it" }),
    );
    let text = doer.text_of(id);
    assert!(text.contains("Submitted task #1"), "submit: {text}");

    let id = maker.call(
        "bus_task_review",
        serde_json::json!({ "task_id": 1, "accepted": true, "feedback": "ok" }),
    );
    let text = maker.text_of(id);
    assert!(text.contains("Accepted task #1"), "review: {text}");
}

#[test]
fn mcp_operator_agent_add() {
    let dir = fresh_dir();
    let db = init_bus(&dir);
    let mut mcp = Mcp::spawn(&db, "operator", &["--operator"]);
    let id = mcp.call(
        "bus_agent_add",
        serde_json::json!({ "id": "newbie", "role": "coder" }),
    );
    let text = mcp.text_of(id);
    assert!(text.contains("Added newbie"), "agent_add: {text}");
    assert!(
        db.parent().unwrap().join("tokens/newbie.token").exists(),
        "token file written"
    );
}

#[test]
fn mcp_config_claude_and_codex() {
    let dir = fresh_dir();
    let db = init_bus(&dir);
    add_agent(&db, "greg", "coder");

    let out = Command::new(qagent())
        .args(["--db"])
        .arg(&db)
        .args(["mcp-config", "--agent", "greg", "--client", "claude"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        parsed["mcpServers"]["qagent"]["env"]["QAGENT_AGENT_ID"],
        "greg"
    );
    assert_eq!(parsed["mcpServers"]["qagent"]["args"][0], "mcp");

    let out = Command::new(qagent())
        .args(["--db"])
        .arg(&db)
        .args(["mcp-config", "--agent", "greg", "--client", "codex"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("[mcp_servers.qagent]"), "codex: {text}");
    assert!(text.contains("tool_timeout_sec = 3660"), "codex: {text}");
    assert!(text.contains("QAGENT_AGENT_ID = \"greg\""), "codex: {text}");
}
