//! Supervisor tests — porting the expectations of tests/supervisor.test.ts
//! plus the adapter parse checks from tests/adapters.test.ts.

use acs::bus::{Bus, CreateTaskInput, SendInput};
use acs::config::{load_config, resolve_agent, BusConfig, ResolvedAgent};
use acs::identity;
use acs::supervisor::*;
use acs::types::OPERATOR_ID;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn fresh_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "acs-rust-supervise-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn capabilities() -> serde_json::Value {
    serde_json::json!({
        "coding": 0.5, "reasoning": 0.5, "planning": 0.5, "debugging": 0.5,
        "research": 0.5, "toolUse": 0.5, "speed": 0.5, "tokenEfficiency": 0.5,
        "reliability": 0.5, "autonomy": 0.5, "contextTokens": 1000,
        "costClass": "local", "source": "heuristic-default",
    })
}

/// A fixture config with a fake provider/harness/model and one worker agent,
/// plus an optional anthropic subscription-backed agent for env tests.
fn write_config(dir: &std::path::Path, agents: &[(&str, &str)], harness_mode: &str) -> PathBuf {
    let mut agent_entries = serde_json::Map::new();
    for (id, model) in agents {
        agent_entries.insert(
            id.to_string(),
            serde_json::json!({
                "id": id, "model": model, "role": "worker", "authority": "worker",
                "description": "", "enabled": true,
                "permissions": { "maxDelegationDepth": 0 },
                "harnessOptions": if harness_mode.is_empty() { serde_json::json!({}) } else { serde_json::json!({ "mode": harness_mode }) },
            }),
        );
    }
    let config = serde_json::json!({
        "version": 1,
        "capabilityNotice": "",
        "providers": {
            "fake-provider": { "id": "fake-provider", "enabled": true, "subscriptionBacked": false },
            "anthropic": { "id": "anthropic", "enabled": true, "subscriptionBacked": true },
        },
        "harnesses": {
            "fake-harness": {
                "id": "fake-harness", "adapter": "fake", "command": "fake",
                "providers": ["fake-provider"], "enabled": true,
                "features": { "mcp": false, "resume": true },
            },
            "claude-code": {
                "id": "claude-code", "adapter": "claude", "command": "claude",
                "providers": ["anthropic"], "enabled": true,
                "features": { "mcp": true, "resume": true },
            },
        },
        "models": {
            "fake-model": {
                "id": "fake-model", "provider": "fake-provider", "harness": "fake-harness",
                "family": "fake", "enabled": true, "capabilities": capabilities(),
            },
            "claude-model": {
                "id": "claude-model", "provider": "anthropic", "harness": "claude-code",
                "family": "claude", "enabled": true, "capabilities": capabilities(),
            },
        },
        "agents": serde_json::Value::Object(agent_entries),
        "roles": { "worker": { "id": "worker", "description": "" } },
        "routing": {},
        "constraints": {},
    });
    let path = dir.join("agent-bus.config.json");
    fs::write(&path, serde_json::to_string_pretty(&config).unwrap()).unwrap();
    path
}

fn fixture<'a>(config: &'a BusConfig, agent_id: &str) -> ResolvedAgent<'a> {
    resolve_agent(config, agent_id).unwrap()
}

// ---------------------------------------------------------------- unit tests

#[test]
fn retry_delay_backoff() {
    assert_eq!(retry_delay_ms(1), 2_000);
    assert_eq!(retry_delay_ms(2), 4_000);
    assert_eq!(retry_delay_ms(3), 8_000);
    assert_eq!(retry_delay_ms(6), 60_000);
    assert_eq!(retry_delay_ms(30), 60_000);
}

#[test]
fn resumed_unexpected_session_rules() {
    assert!(!resumed_unexpected_session(None, Some("x")));
    assert!(!resumed_unexpected_session(Some("x"), None));
    assert!(!resumed_unexpected_session(Some("x"), Some("x")));
    assert!(resumed_unexpected_session(Some("x"), Some("y")));
}

#[test]
fn supervisor_managed_without_mcp() {
    let dir = fresh_dir("managed");
    let config_path = write_config(&dir, &[("w1", "fake-model"), ("c1", "claude-model")], "");
    let config = load_config(&config_path).unwrap();
    assert!(supervisor_managed(&fixture(&config, "w1")));
    assert!(!supervisor_managed(&fixture(&config, "c1")));
}

#[test]
fn sanitized_environment_strips_subscription_provider_keys() {
    let dir = fresh_dir("env");
    let config_path = write_config(&dir, &[("c1", "claude-model"), ("w1", "fake-model")], "");
    let config = load_config(&config_path).unwrap();

    std::env::set_var("ANTHROPIC_API_KEY", "sk-test-value");
    std::env::set_var("OPENAI_API_KEY", "sk-other");
    std::env::remove_var("QAGENT_ALLOW_API_KEY");
    std::env::remove_var("AGENT_BUS_ALLOW_API_KEY");

    let env = sanitized_environment(&fixture(&config, "c1"), &HashMap::new());
    assert!(
        !env.contains_key("ANTHROPIC_API_KEY"),
        "subscription-backed provider key must be stripped"
    );
    assert_eq!(
        env.get("OPENAI_API_KEY").map(|s| s.as_str()),
        Some("sk-other"),
        "other providers' keys stay"
    );
    assert_eq!(
        env.get("MCP_TOOL_TIMEOUT").map(|s| s.as_str()),
        Some("3600000")
    );

    // Non-subscription providers keep their keys.
    std::env::set_var("FAKE_PROVIDER_API_KEY", "x");
    let env = sanitized_environment(&fixture(&config, "w1"), &HashMap::new());
    assert!(env.contains_key("ANTHROPIC_API_KEY") == env.contains_key("ANTHROPIC_API_KEY"));

    // Explicit opt-in keeps everything.
    std::env::set_var("QAGENT_ALLOW_API_KEY", "1");
    let env = sanitized_environment(&fixture(&config, "c1"), &HashMap::new());
    assert_eq!(
        env.get("ANTHROPIC_API_KEY").map(|s| s.as_str()),
        Some("sk-test-value")
    );
    std::env::remove_var("QAGENT_ALLOW_API_KEY");
    std::env::remove_var("ANTHROPIC_API_KEY");
    std::env::remove_var("OPENAI_API_KEY");
    std::env::remove_var("FAKE_PROVIDER_API_KEY");
}

#[test]
fn build_brief_names_task_and_managed_lines() {
    let dir = fresh_dir("brief");
    let config_path = write_config(&dir, &[("w1", "fake-model")], "");
    let config = load_config(&config_path).unwrap();
    let agent = fixture(&config, "w1");
    let task = acs::types::Task {
        id: 7,
        legacy_id: None,
        project: None,
        parent_id: None,
        title: "fix the thing".to_string(),
        brief: "brief text".to_string(),
        acceptance: "it works".to_string(),
        role: "worker".to_string(),
        priority: "normal".to_string(),
        state: "claimed".to_string(),
        creator: OPERATOR_ID.to_string(),
        assignee: Some("w1".to_string()),
        reviewer: None,
        path_scopes: vec![],
        refs: vec![],
        result: None,
        review: None,
        round: 0,
        attempts: 1,
        max_retries: 2,
        claim_expires_ms: None,
        created_ms: 0,
        updated_ms: 0,
        dependencies: vec![],
    };
    let brief = build_brief(&agent, &[], &[task], true);
    assert!(brief.contains("=== qagent: 0 new message(s), 1 task(s) for w1 ==="));
    assert!(brief.contains("-- task #7 · claimed · w1 · role worker"));
    assert!(brief.contains("fix the thing"));
    assert!(brief.contains("Acceptance:\nit works"));
    assert!(brief.contains("claimed the task(s) above for you"));
    assert!(!brief.contains("bus_task_claim"));

    let brief = build_brief(&agent, &[], &[], false);
    assert!(brief.contains("bus_task_claim"));
    assert!(brief.contains("Do NOT call bus_wait"));
}

// ------------------------------------------------------------ adapter parses

#[test]
fn adapter_parse_normalizes_each_harness() {
    let claude = acs::adapters::get_harness_adapter("claude").unwrap();
    let parsed = (claude.parse)(
        "{\"result\":\"done\",\"session_id\":\"s-1\",\"usage\":{\"input_tokens\":10,\"output_tokens\":5,\"cache_read_input_tokens\":2},\"total_cost_usd\":0.5}\n",
        0,
    );
    assert_eq!(parsed.text, "done");
    assert_eq!(parsed.session_id.as_deref(), Some("s-1"));
    assert_eq!(parsed.usage.input_tokens, 12.0);
    assert_eq!(parsed.usage.total_tokens, 17.0);
    assert_eq!(parsed.usage.cost_usd, 0.5);
    assert!(!parsed.malformed);

    let codex = acs::adapters::get_harness_adapter("codex").unwrap();
    let parsed = (codex.parse)(
        "{\"type\":\"thread.started\",\"thread_id\":\"t-9\"}\n{\"item\":{\"type\":\"agent_message\",\"text\":\"hello\"}}\n{\"item\":{\"type\":\"agent_message\",\"text\":\"world\"}}\n{\"usage\":{\"input_tokens\":3,\"output_tokens\":4}}\n",
        0,
    );
    assert_eq!(parsed.text, "hello\nworld");
    assert_eq!(parsed.session_id.as_deref(), Some("t-9"));
    assert_eq!(parsed.usage.total_tokens, 7.0);

    let parsed = (codex.parse)("noise\ntokens used\n1,234\n", 0);
    assert_eq!(parsed.usage.total_tokens, 1234.0);

    let gemini = acs::adapters::get_harness_adapter("gemini").unwrap();
    let parsed = (gemini.parse)("\x1b[33msome output\x1b[0m\n", 0);
    assert_eq!(parsed.text, "some output");
    assert!(!parsed.malformed);
    let parsed = (gemini.parse)("", 0);
    assert!(parsed.malformed);
    assert_eq!(parsed.text, "(no textual output captured)");

    let opencode = acs::adapters::get_harness_adapter("opencode").unwrap();
    let parsed = (opencode.parse)(
        "{\"sessionID\":\"oc-2\",\"part\":{\"type\":\"text\",\"text\":\"hi\"},\"tokens\":{\"total\":42}}\n",
        0,
    );
    assert_eq!(parsed.text, "hi");
    assert_eq!(parsed.session_id.as_deref(), Some("oc-2"));
    assert_eq!(parsed.usage.total_tokens, 42.0);

    let hermes = acs::adapters::get_harness_adapter("hermes").unwrap();
    let output = "header\n────────────────────────\nfinal answer text\n────────────────────────\n--resume hs-77\n~1,500 tokens\n";
    let parsed = (hermes.parse)(output, 0);
    assert_eq!(parsed.text, "final answer text");
    assert_eq!(parsed.session_id.as_deref(), Some("hs-77"));
    assert_eq!(parsed.usage.total_tokens, 1500.0);

    let fake = acs::adapters::get_harness_adapter("fake").unwrap();
    let parsed = (fake.parse)(
        "{\"result\":\"ok\",\"sessionId\":\"s\",\"usage\":{\"inputTokens\":1,\"outputTokens\":2,\"totalTokens\":3,\"costUSD\":0}}\n",
        0,
    );
    assert_eq!(parsed.text, "ok");
    assert_eq!(parsed.usage.total_tokens, 3.0);
    let parsed = (fake.parse)("garbage\n", 0);
    assert!(parsed.malformed);

    let command = acs::adapters::get_harness_adapter("command").unwrap();
    let parsed = (command.parse)(
        "{\"result\":\"r\",\"session_id\":\"cs\",\"usage\":{\"input_tokens\":4,\"output_tokens\":6,\"total_tokens\":10,\"cost_usd\":0.01}}\n",
        0,
    );
    assert_eq!(parsed.text, "r");
    assert_eq!(parsed.session_id.as_deref(), Some("cs"));
    assert_eq!(parsed.usage.total_tokens, 10.0);

    assert!(acs::adapters::get_harness_adapter("nonexistent").is_err());
}

// ------------------------------------------------------------- e2e supervise

struct E2E {
    home: PathBuf,
    workdir: PathBuf,
    bus: Bus,
    operator: identity::Identity,
}

fn e2e(agent_id: &str, harness_mode: &str) -> E2E {
    let home = fresh_dir("home");
    let workdir = fresh_dir("work");
    let bus = Bus::open(Some(&home.join("bus.db"))).unwrap();
    bus.init().unwrap();
    let operator = bus.identify(Some(OPERATOR_ID)).unwrap();
    bus.add_agent(
        &operator,
        agent_id,
        Some("worker"),
        None,
        None,
        None,
        Some("worker"),
    )
    .unwrap();
    let config_path = write_config(&workdir, &[(agent_id, "fake-model")], harness_mode);
    // configPathFromProject finds agent-bus.config.json at the workdir root? It looks
    // in .qagent/config.json then .agent-bus/config.json — pass --config explicitly.
    let _ = config_path;
    E2E {
        home,
        workdir,
        bus,
        operator,
    }
}

fn wait_for<F: Fn() -> bool>(timeout: Duration, what: &str, check: F) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if check() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("timed out waiting for {what}");
}

#[test]
fn supervise_fake_harness_claims_and_submits() {
    let e = e2e("w1", "");
    let config_path = e.workdir.join("agent-bus.config.json");
    let stop = Arc::new(AtomicBool::new(false));
    let handle = {
        let stop = Arc::clone(&stop);
        let db_path = e.home.join("bus.db");
        let workdir = e.workdir.display().to_string();
        let logs: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(vec![]));
        let logs2 = Arc::clone(&logs);
        std::thread::spawn(move || {
            let r = supervise(SuperviseOptions {
                agent_id: "w1".to_string(),
                workdir,
                db_path,
                config_path: Some(config_path),
                stop,
                wait_ms: Some(2_000),
                retry_base_ms: None,
                fake_harness_path: Some("fake-harness".to_string()),
                qagent_bin: Some(env!("CARGO_BIN_EXE_qagent").to_string()),
                log: Some(Box::new(move |line| {
                    logs2.lock().unwrap().push(line.to_string())
                })),
            });
            (r, logs)
        })
    };

    e.bus
        .create_task(
            &e.operator,
            CreateTaskInput {
                title: "do the thing".to_string(),
                brief: Some("please work".to_string()),
                to: Some("w1".to_string()),
                ..Default::default()
            },
        )
        .unwrap();

    wait_for(Duration::from_secs(20), "task submitted", || {
        e.bus
            .get_task(1)
            .map(|t| t.task.state == "submitted")
            .unwrap_or(false)
    });

    let task = e.bus.get_task(1).unwrap();
    assert_eq!(task.task.state, "submitted");
    assert_eq!(task.task.assignee.as_deref(), Some("w1"));
    let summary = task.task.result.as_ref().unwrap().summary.clone();
    assert!(
        summary.contains("fake w1 completed the assigned work"),
        "summary: {summary}"
    );

    // Session record persisted.
    let session: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(e.home.join("sessions/w1.json")).unwrap())
            .unwrap();
    assert!(session["turns"].as_i64().unwrap() >= 1);

    stop.store(true, Ordering::SeqCst);
    let (result, _logs) = handle.join().unwrap();
    result.unwrap();
}

#[test]
fn supervise_stop_kills_hung_process_group() {
    let e = e2e("w2", "hang");
    let config_path = e.workdir.join("agent-bus.config.json");
    let state_file = e.workdir.join("hang-state.json");
    std::env::set_var("FAKE_HARNESS_STATE", &state_file);
    let stop = Arc::new(AtomicBool::new(false));
    let handle = {
        let stop = Arc::clone(&stop);
        let db_path = e.home.join("bus.db");
        let workdir = e.workdir.display().to_string();
        std::thread::spawn(move || {
            supervise(SuperviseOptions {
                agent_id: "w2".to_string(),
                workdir,
                db_path,
                config_path: Some(config_path),
                stop,
                wait_ms: Some(2_000),
                retry_base_ms: None,
                fake_harness_path: Some("fake-harness".to_string()),
                qagent_bin: Some(env!("CARGO_BIN_EXE_qagent").to_string()),
                log: Some(Box::new(|_| {})),
            })
        })
    };

    e.bus
        .create_task(
            &e.operator,
            CreateTaskInput {
                title: "hang forever".to_string(),
                to: Some("w2".to_string()),
                ..Default::default()
            },
        )
        .unwrap();

    // Wait for the harness to spawn and record its pids.
    wait_for(Duration::from_secs(15), "hang state file", || {
        state_file.exists()
    });
    let state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&state_file).unwrap()).unwrap();
    let pid = state["pid"].as_i64().unwrap() as i32;
    let grandchild = state["grandchild"].as_i64().unwrap() as i32;

    stop.store(true, Ordering::SeqCst);
    handle.join().unwrap().unwrap();

    // Both the harness and its grandchild must be dead — process-group kill.
    wait_for(Duration::from_secs(8), "process group dead", || {
        let dead = |p: i32| unsafe { libc::kill(p, 0) } != 0;
        dead(pid) && dead(grandchild)
    });
    std::env::remove_var("FAKE_HARNESS_STATE");
}

#[test]
fn supervise_fails_task_on_malformed_output() {
    let e = e2e("w3", "malformed");
    let config_path = e.workdir.join("agent-bus.config.json");
    let stop = Arc::new(AtomicBool::new(false));
    let handle = {
        let stop = Arc::clone(&stop);
        let db_path = e.home.join("bus.db");
        let workdir = e.workdir.display().to_string();
        std::thread::spawn(move || {
            supervise(SuperviseOptions {
                agent_id: "w3".to_string(),
                workdir,
                db_path,
                config_path: Some(config_path),
                stop,
                wait_ms: Some(2_000),
                retry_base_ms: None,
                fake_harness_path: Some("fake-harness".to_string()),
                qagent_bin: Some(env!("CARGO_BIN_EXE_qagent").to_string()),
                log: Some(Box::new(|_| {})),
            })
        })
    };

    e.bus
        .create_task(
            &e.operator,
            CreateTaskInput {
                title: "will be malformed".to_string(),
                to: Some("w3".to_string()),
                ..Default::default()
            },
        )
        .unwrap();

    // attempts < max_retries (2): the task is failed back to 'open', not 'failed'.
    wait_for(
        Duration::from_secs(20),
        "task back to open with attempts bump",
        || {
            e.bus
                .get_task(1)
                .map(|t| t.task.state == "open" && t.task.attempts >= 1)
                .unwrap_or(false)
        },
    );

    stop.store(true, Ordering::SeqCst);
    handle.join().unwrap().unwrap();
}

#[test]
fn supervise_pauses_on_budget_and_resumes() {
    let e = e2e("w1", "");
    let config_path = e.workdir.join("agent-bus.config.json");
    // One turn allowed: the first task is worked, then the supervisor pauses itself.
    e.bus
        .set_budget(
            &e.operator,
            "w1",
            Some(acs::control::Limits {
                turns: Some(1.0),
                ..Default::default()
            }),
        )
        .unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let handle = {
        let stop = Arc::clone(&stop);
        let db_path = e.home.join("bus.db");
        let workdir = e.workdir.display().to_string();
        std::thread::spawn(move || {
            supervise(SuperviseOptions {
                agent_id: "w1".to_string(),
                workdir,
                db_path,
                config_path: Some(config_path),
                stop,
                wait_ms: Some(1_000),
                retry_base_ms: None,
                fake_harness_path: Some("fake-harness".to_string()),
                qagent_bin: Some(env!("CARGO_BIN_EXE_qagent").to_string()),
                log: Some(Box::new(|_| {})),
            })
        })
    };
    let add = |title: &str| {
        e.bus
            .create_task(
                &e.operator,
                CreateTaskInput {
                    title: title.to_string(),
                    to: Some("w1".to_string()),
                    ..Default::default()
                },
            )
            .unwrap()
            .id
    };
    let first = add("first");
    wait_for(Duration::from_secs(20), "first task submitted", || {
        e.bus
            .get_task(first)
            .map(|t| t.task.state == "submitted")
            .unwrap_or(false)
    });
    let paused = || {
        e.bus
            .get_agent("w1")
            .unwrap()
            .and_then(|a| acs::control::paused(&a.meta))
    };
    wait_for(Duration::from_secs(20), "budget pause", || {
        paused().is_some()
    });
    assert_eq!(paused().unwrap().reason, "budget reached: 1 of 1 turns");
    let mail = e.bus.inbox(&e.operator, true, None).unwrap().messages;
    assert!(
        mail.iter()
            .any(|m| m.subject == "w1 paused: budget reached (1 of 1 turns)"),
        "{mail:?}"
    );

    // Paused: new work waits.
    let second = add("second");
    std::thread::sleep(Duration::from_millis(2_500));
    assert_eq!(e.bus.get_task(second).unwrap().task.state, "open");

    // Resumed with a fresh allowance of one turn: the waiting task is worked.
    e.bus.resume_agent(&e.operator, "w1").unwrap();
    wait_for(Duration::from_secs(20), "second task submitted", || {
        e.bus
            .get_task(second)
            .map(|t| t.task.state == "submitted")
            .unwrap_or(false)
    });

    stop.store(true, Ordering::SeqCst);
    handle.join().unwrap().unwrap();
}

#[test]
fn supervise_pauses_after_repeated_failures_and_keeps_the_mail() {
    let e = e2e("w4", "fail");
    let config_path = e.workdir.join("agent-bus.config.json");
    // A plain message (no task): every turn on it fails, and it must not be lost.
    e.bus
        .send(
            &e.operator,
            SendInput {
                to: "w4".into(),
                subject: Some("look at this".into()),
                body: "please".into(),
                msg_type: None,
                thread: None,
                task_id: None,
                refs: None,
                requires_ack: false,
            },
        )
        .unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let handle = {
        let stop = Arc::clone(&stop);
        let db_path = e.home.join("bus.db");
        let workdir = e.workdir.display().to_string();
        std::thread::spawn(move || {
            supervise(SuperviseOptions {
                agent_id: "w4".to_string(),
                workdir,
                db_path,
                config_path: Some(config_path),
                stop,
                wait_ms: Some(1_000),
                retry_base_ms: Some(20),
                fake_harness_path: Some("fake-harness".to_string()),
                qagent_bin: Some(env!("CARGO_BIN_EXE_qagent").to_string()),
                log: Some(Box::new(|_| {})),
            })
        })
    };
    let paused = || {
        e.bus
            .get_agent("w4")
            .unwrap()
            .and_then(|a| acs::control::paused(&a.meta))
    };
    wait_for(Duration::from_secs(30), "failure pause", || paused().is_some());
    let reason = paused().unwrap().reason;
    assert!(
        reason.starts_with(&format!("{MAX_FAILED_TURNS} turns failed in a row")),
        "{reason}"
    );
    let mail = e.bus.inbox(&e.operator, true, None).unwrap().messages;
    assert!(
        mail.iter()
            .any(|m| m.subject == format!("w4 paused: {MAX_FAILED_TURNS} turns failed in a row")),
        "{mail:?}"
    );
    let session: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(e.home.join("sessions/w4.json")).unwrap())
            .unwrap();
    assert_eq!(session["turns"].as_i64(), Some(MAX_FAILED_TURNS as i64));
    // Paused, no further turns.
    std::thread::sleep(Duration::from_millis(1_500));
    let session: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(e.home.join("sessions/w4.json")).unwrap())
            .unwrap();
    assert_eq!(session["turns"].as_i64(), Some(MAX_FAILED_TURNS as i64));
    // The message no turn managed to handle is still unread.
    let w4 = e.bus.identify(Some("w4")).unwrap();
    let unread = e.bus.inbox(&w4, true, None).unwrap().messages;
    assert!(unread.iter().any(|m| m.subject == "look at this"), "{unread:?}");

    stop.store(true, Ordering::SeqCst);
    handle.join().unwrap().unwrap();
}

#[test]
fn supervise_survives_a_locked_bus() {
    let e = e2e("w5", "");
    let config_path = e.workdir.join("agent-bus.config.json");
    // Another process holds the write lock past the busy timeout.
    let blocker = rusqlite::Connection::open(e.home.join("bus.db")).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
    let logs: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(vec![]));
    let stop = Arc::new(AtomicBool::new(false));
    let handle = {
        let stop = Arc::clone(&stop);
        let logs = Arc::clone(&logs);
        let db_path = e.home.join("bus.db");
        let workdir = e.workdir.display().to_string();
        std::thread::spawn(move || {
            supervise(SuperviseOptions {
                agent_id: "w5".to_string(),
                workdir,
                db_path,
                config_path: Some(config_path),
                stop,
                wait_ms: Some(1_000),
                retry_base_ms: Some(50),
                fake_harness_path: Some("fake-harness".to_string()),
                qagent_bin: Some(env!("CARGO_BIN_EXE_qagent").to_string()),
                log: Some(Box::new(move |line| logs.lock().unwrap().push(line.to_string()))),
            })
        })
    };
    wait_for(Duration::from_secs(20), "a failed round", || {
        logs.lock().unwrap().iter().any(|l| l.contains("round failed"))
    });
    assert!(!handle.is_finished(), "supervisor exited on a locked bus");
    blocker.execute_batch("COMMIT").unwrap();

    let task = e
        .bus
        .create_task(
            &e.operator,
            CreateTaskInput {
                title: "after the lock".to_string(),
                to: Some("w5".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
    wait_for(Duration::from_secs(30), "task submitted", || {
        e.bus
            .get_task(task.id)
            .map(|t| t.task.state == "submitted")
            .unwrap_or(false)
    });
    stop.store(true, Ordering::SeqCst);
    handle.join().unwrap().unwrap();
}

#[test]
fn supervisor_alive_checks_the_command_line() {
    assert!(supervisor_alive(std::process::id() as i32, "anyone"));
    assert!(!supervisor_alive(0, "w1"));
    // A live process that is not a supervisor (a reused pid after a reboot).
    let mut other = std::process::Command::new("sleep").arg("30").spawn().unwrap();
    let pid = other.id() as i32;
    assert!(!supervisor_alive(pid, "w1"));
    other.kill().unwrap();
    other.wait().unwrap();
    assert!(!supervisor_alive(pid, "w1"));
}

#[test]
fn guard_drops_credentials_and_blocks_git_push_only() {
    let mut env: HashMap<String, String> = std::env::vars().collect();
    env.insert("GITHUB_TOKEN".into(), "ghp_x".into());
    env.insert("SSH_AUTH_SOCK".into(), "/tmp/agent.sock".into());
    env.insert("ANTHROPIC_API_KEY".into(), "kept".into());
    env.insert("GIT_CONFIG_COUNT".into(), "1".into());
    env.insert("GIT_CONFIG_KEY_0".into(), "user.name".into());
    env.insert("GIT_CONFIG_VALUE_0".into(), "Agent".into());
    guard_environment(&mut env);
    assert!(!env.contains_key("GITHUB_TOKEN"));
    assert!(!env.contains_key("SSH_AUTH_SOCK"));
    assert_eq!(env.get("ANTHROPIC_API_KEY").map(String::as_str), Some("kept"));
    assert_eq!(env.get("GIT_CONFIG_COUNT").map(String::as_str), Some("2"));
    assert_eq!(env.get("GIT_CONFIG_KEY_0").map(String::as_str), Some("user.name"));
    assert_eq!(env.get("GIT_TERMINAL_PROMPT").map(String::as_str), Some("0"));

    // A real repository: push is refused, fetch still works.
    let dir = fresh_dir("guard-git");
    let git = |args: &[&str], cwd: &std::path::Path, env: Option<&HashMap<String, String>>| {
        let mut c = std::process::Command::new("git");
        c.args(args).current_dir(cwd);
        if let Some(env) = env {
            c.env_clear().envs(env);
        }
        c.output().unwrap()
    };
    assert!(git(&["init", "-q", "--bare", "remote.git"], &dir, None).status.success());
    assert!(git(&["clone", "-q", "remote.git", "work"], &dir, None).status.success());
    let work = dir.join("work");
    assert!(git(
        &["-c", "user.email=a@b", "-c", "user.name=a", "commit", "-q", "--allow-empty", "-m", "x"],
        &work,
        None
    )
    .status
    .success());
    let pushed = git(&["push", "-q", "origin", "HEAD"], &work, Some(&env));
    assert!(!pushed.status.success(), "push must be refused");
    assert!(String::from_utf8_lossy(&pushed.stderr).contains("aos-blocked-git-push"));
    assert!(git(&["fetch", "-q", "origin"], &work, Some(&env)).status.success());
    // Unguarded, the same push goes through.
    assert!(git(&["push", "-q", "origin", "HEAD"], &work, None).status.success());
}

#[test]
fn guard_is_on_unless_the_agent_turns_it_off() {
    let dir = fresh_dir("guard-flag");
    let config_path = write_config(&dir, &[("c1", "claude-model")], "");
    let mut raw: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
    let config = load_config(&config_path).unwrap();
    let agent = fixture(&config, "c1");
    assert!(guarded(&agent));
    let context = acs::adapters::AdapterContext {
        agent: &agent,
        qagent_bin: "qagent".into(),
        prompt: "p".into(),
        session_id: None,
        pinned_session_id: None,
        workdir: dir.display().to_string(),
        mcp_server_path: "mcp".into(),
        fake_harness_path: "fake-harness".into(),
        bus_environment: HashMap::new(),
        mcp_command: None,
    };
    let claude = acs::adapters::get_harness_adapter("claude").unwrap();
    let args = (claude.build)(&context).args;
    let i = args.iter().position(|a| a == "--disallowedTools").unwrap();
    assert_eq!(args[i + 1], "Bash(git push:*),Bash(sudo:*)");

    raw["agents"]["c1"]["harnessOptions"] = serde_json::json!({ "guard": false });
    fs::write(&config_path, raw.to_string()).unwrap();
    let config = load_config(&config_path).unwrap();
    let agent = fixture(&config, "c1");
    assert!(!guarded(&agent));
    let context = acs::adapters::AdapterContext { agent: &agent, ..context };
    assert!(!(claude.build)(&context).args.iter().any(|a| a == "--disallowedTools"));
}

// ------------------------------------------------- backlog without fresh mail

fn spawn_supervisor(
    e: &E2E,
    agent_id: &str,
    wait_ms: u64,
) -> (Arc<AtomicBool>, std::thread::JoinHandle<acs::error::Result<()>>) {
    let stop = Arc::new(AtomicBool::new(false));
    let handle = {
        let stop = Arc::clone(&stop);
        let db_path = e.home.join("bus.db");
        let workdir = e.workdir.display().to_string();
        let config_path = e.workdir.join("agent-bus.config.json");
        let agent_id = agent_id.to_string();
        std::thread::spawn(move || {
            supervise(SuperviseOptions {
                agent_id,
                workdir,
                db_path,
                config_path: Some(config_path),
                stop,
                wait_ms: Some(wait_ms),
                retry_base_ms: None,
                fake_harness_path: Some("fake-harness".to_string()),
                qagent_bin: Some(env!("CARGO_BIN_EXE_qagent").to_string()),
                log: Some(Box::new(|_| {})),
            })
        })
    };
    (stop, handle)
}

fn unassigned_task(e: &E2E, title: &str) -> i64 {
    e.bus
        .create_task(
            &e.operator,
            CreateTaskInput {
                title: title.to_string(),
                role: Some("worker".to_string()),
                ..Default::default()
            },
        )
        .unwrap()
        .id
}

#[test]
fn supervise_works_backlog_already_queued_at_startup() {
    let e = e2e("w1", "");
    // Queued before the supervisor starts: no mail and no fresh event will ever arrive for it.
    let id = unassigned_task(&e, "queued before start");
    // A wait far longer than the test allows: the backlog must not wait for a timeout.
    let (stop, handle) = spawn_supervisor(&e, "w1", 60_000);
    wait_for(Duration::from_secs(15), "backlog task submitted", || {
        e.bus
            .get_task(id)
            .map(|t| t.task.state == "submitted")
            .unwrap_or(false)
    });
    stop.store(true, Ordering::SeqCst);
    handle.join().unwrap().unwrap();
}

#[test]
fn supervise_works_backlog_queued_while_paused_after_resume() {
    let e = e2e("w1", "");
    e.bus.pause_agent(&e.operator, "w1", Some("test")).unwrap();
    let (stop, handle) = spawn_supervisor(&e, "w1", 60_000);
    std::thread::sleep(Duration::from_millis(500));
    // Its creation event passes while the agent is paused and not waiting.
    let id = unassigned_task(&e, "queued while paused");
    std::thread::sleep(Duration::from_millis(500));
    e.bus.resume_agent(&e.operator, "w1").unwrap();
    wait_for(Duration::from_secs(15), "task queued while paused submitted", || {
        e.bus
            .get_task(id)
            .map(|t| t.task.state == "submitted")
            .unwrap_or(false)
    });
    stop.store(true, Ordering::SeqCst);
    handle.join().unwrap().unwrap();
}

// ------------------------------------------------- worktree isolation fails closed

fn set_constraint(e: &E2E, key: &str, value: serde_json::Value) {
    let path = e.workdir.join("agent-bus.config.json");
    let mut config: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    config["constraints"][key] = value;
    fs::write(&path, serde_json::to_string_pretty(&config).unwrap()).unwrap();
}

#[test]
fn supervise_refuses_worktree_isolation_instead_of_using_the_shared_checkout() {
    let e = e2e("w1", "");
    set_constraint(&e, "isolation", serde_json::json!("worktree"));
    let id = unassigned_task(&e, "must not run in the shared checkout");
    let (stop, handle) = spawn_supervisor(&e, "w1", 1_000);
    let finished = Instant::now() + Duration::from_secs(10);
    while !handle.is_finished() && Instant::now() < finished {
        std::thread::sleep(Duration::from_millis(50));
    }
    stop.store(true, Ordering::SeqCst);
    let error = handle.join().unwrap().unwrap_err();
    assert!(
        error.message.contains("worktree isolation is not available"),
        "{}",
        error.message
    );
    assert_eq!(e.bus.get_task(id).unwrap().task.state, "open");
    assert!(!e.home.join("supervisors/w1.pid").exists());
}

#[test]
fn task_claim_with_worktree_fails_without_claiming() {
    let e = e2e("w1", "");
    let id = unassigned_task(&e, "asked for a worktree");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_qagent"))
        .args(["--db", &e.home.join("bus.db").display().to_string(), "--as", "w1"])
        .args(["task", "claim", "--worktree", &id.to_string()])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("worktree isolation is not available"), "{stderr}");
    assert_eq!(e.bus.get_task(id).unwrap().task.state, "open");
}

// ------------------------------------------------- one supervisor per agent

#[test]
fn simultaneous_supervisors_for_one_agent_leave_exactly_one_running() {
    // Check-then-write ownership lets several starters win only now and then, so race a few rounds.
    const STARTERS: usize = 16;
    let e = e2e("w1", "");
    let config_path = e.workdir.join("agent-bus.config.json");
    let db = e.home.join("bus.db");
    for round in 0..4 {
        // A stale pid file left by a crash: every starter has to decide to take it over.
        fs::create_dir_all(e.home.join("supervisors")).unwrap();
        fs::write(e.home.join("supervisors/w1.pid"), "999999\n").unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(STARTERS));
        let starters: Vec<_> = (0..STARTERS)
            .map(|_| {
                let barrier = Arc::clone(&barrier);
                let (db, workdir, config) = (db.clone(), e.workdir.clone(), config_path.clone());
                std::thread::spawn(move || {
                    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_qagent"));
                    command
                        .arg("--db").arg(&db).args(["--as", "w1", "supervise", "w1"])
                        .arg(&workdir).arg("--config").arg(&config)
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::piped());
                    barrier.wait();
                    command.spawn().unwrap()
                })
            })
            .collect();
        let mut children: Vec<_> = starters.into_iter().map(|t| t.join().unwrap()).collect();
        std::thread::sleep(Duration::from_millis(1_500));
        let mut running = 0;
        for child in &mut children {
            if child.try_wait().unwrap().is_none() {
                running += 1;
            }
        }
        let mut refusals = 0;
        for child in &mut children {
            if child.try_wait().unwrap().is_none() {
                unsafe { libc::kill(child.id() as i32, libc::SIGINT) };
            }
            if child.wait_with_output_ref().contains("is already running") {
                refusals += 1;
            }
        }
        assert_eq!(running, 1, "round {round}: supervisors left running");
        assert_eq!(refusals, STARTERS - 1, "round {round}");
    }
}

trait WaitOutput {
    fn wait_with_output_ref(&mut self) -> String;
}

impl WaitOutput for std::process::Child {
    fn wait_with_output_ref(&mut self) -> String {
        use std::io::Read;
        let mut text = String::new();
        if let Some(mut stderr) = self.stderr.take() {
            let _ = stderr.read_to_string(&mut text);
        }
        let _ = self.wait();
        text
    }
}

// ------------------------------------------------- configuration limits and budgets

#[test]
fn supervise_applies_the_configuration_limits_to_the_bus_at_start() {
    let e = e2e("w1", "");
    set_constraint(&e, "maxConcurrentTasks", serde_json::json!(3));
    let (stop, handle) = spawn_supervisor(&e, "w1", 60_000);
    wait_for(Duration::from_secs(10), "the supervisor to wait", || {
        e.bus
            .get_agent("w1")
            .unwrap()
            .is_some_and(|a| a.stored_status == "waiting")
    });
    let me = e.bus.identify(Some("w1")).unwrap();
    assert!(!me.permissions.can_delegate);
    assert_eq!(me.permissions.max_concurrent_tasks, Some(3));
    assert_eq!(me.permissions.max_delegation_depth, Some(0));
    stop.store(true, Ordering::SeqCst);
    handle.join().unwrap().unwrap();
}

#[test]
fn supervise_stops_new_turns_once_the_configuration_token_budget_is_used() {
    let e = e2e("w1", "");
    // The fake harness reports well over 10 tokens a turn.
    set_constraint(&e, "optionalTokenBudget", serde_json::json!(10));
    let logs: Arc<std::sync::Mutex<Vec<String>>> = Arc::default();
    let stop = Arc::new(AtomicBool::new(false));
    let handle = {
        let (stop, logs) = (Arc::clone(&stop), Arc::clone(&logs));
        let db_path = e.home.join("bus.db");
        let workdir = e.workdir.display().to_string();
        let config_path = e.workdir.join("agent-bus.config.json");
        std::thread::spawn(move || {
            supervise(SuperviseOptions {
                agent_id: "w1".into(),
                workdir,
                db_path,
                config_path: Some(config_path),
                stop,
                wait_ms: Some(500),
                retry_base_ms: None,
                fake_harness_path: Some("fake-harness".to_string()),
                qagent_bin: Some(env!("CARGO_BIN_EXE_qagent").to_string()),
                log: Some(Box::new(move |line| logs.lock().unwrap().push(line.to_string()))),
            })
        })
    };
    let first = unassigned_task(&e, "first");
    wait_for(Duration::from_secs(15), "first task submitted", || {
        e.bus.get_task(first).is_ok_and(|t| t.task.state == "submitted")
    });
    let second = unassigned_task(&e, "second");
    wait_for(Duration::from_secs(10), "the budget notice", || {
        logs.lock().unwrap().iter().any(|l| l.contains("budget reached (") && l.contains("reported tokens used"))
    });
    std::thread::sleep(Duration::from_millis(1_000));
    assert_eq!(e.bus.get_task(second).unwrap().task.state, "open");
    stop.store(true, Ordering::SeqCst);
    handle.join().unwrap().unwrap();
}

#[test]
fn supervise_refuses_a_dollar_budget_on_a_cli_that_reports_no_usage() {
    let e = e2e("w1", "");
    set_constraint(&e, "optionalApiCostBudgetUSD", serde_json::json!(5));
    let (stop, handle) = spawn_supervisor(&e, "w1", 1_000);
    let finished = Instant::now() + Duration::from_secs(10);
    while !handle.is_finished() && Instant::now() < finished {
        std::thread::sleep(Duration::from_millis(50));
    }
    stop.store(true, Ordering::SeqCst);
    let error = handle.join().unwrap().unwrap_err();
    assert!(
        error.message.contains("reports no usage, so the budget could never be counted"),
        "{}",
        error.message
    );
}

#[test]
fn supervise_offers_no_turn_to_an_agent_already_at_its_claim_limit() {
    let e = e2e("w1", "");
    set_constraint(&e, "maxConcurrentTasks", serde_json::json!(1));
    // A CLI with bus tools: the supervisor offers it open work instead of claiming for it.
    let path = e.workdir.join("agent-bus.config.json");
    let mut config: serde_json::Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    config["harnesses"]["fake-harness"]["features"]["mcp"] = serde_json::json!(true);
    fs::write(&path, serde_json::to_string_pretty(&config).unwrap()).unwrap();
    let held = e
        .bus
        .create_task(
            &e.operator,
            CreateTaskInput {
                title: "already held".to_string(),
                to: Some("w1".to_string()),
                ..Default::default()
            },
        )
        .unwrap()
        .id;
    let w1 = e.bus.identify(Some("w1")).unwrap();
    e.bus.claim_task(&w1, Some(held)).unwrap();
    e.bus.inbox(&w1, false, None).unwrap();
    let other = unassigned_task(&e, "open but over the limit");
    let logs: Arc<std::sync::Mutex<Vec<String>>> = Arc::default();
    let stop = Arc::new(AtomicBool::new(false));
    let handle = {
        let (stop, logs) = (Arc::clone(&stop), Arc::clone(&logs));
        let db_path = e.home.join("bus.db");
        let workdir = e.workdir.display().to_string();
        let config_path = e.workdir.join("agent-bus.config.json");
        std::thread::spawn(move || {
            supervise(SuperviseOptions {
                agent_id: "w1".into(),
                workdir,
                db_path,
                config_path: Some(config_path),
                stop,
                wait_ms: Some(300),
                retry_base_ms: None,
                fake_harness_path: Some("fake-harness".to_string()),
                qagent_bin: Some(env!("CARGO_BIN_EXE_qagent").to_string()),
                log: Some(Box::new(move |line| logs.lock().unwrap().push(line.to_string()))),
            })
        })
    };
    // Several wait periods end with no mail; none may start a turn for the open task.
    std::thread::sleep(Duration::from_millis(2_500));
    stop.store(true, Ordering::SeqCst);
    handle.join().unwrap().unwrap();
    let turns = logs.lock().unwrap().iter().filter(|l| l.contains("turn complete")).count();
    assert_eq!(turns, 0, "turns started while at the claim limit: {:?}", logs.lock().unwrap());
    assert_eq!(e.bus.get_task(other).unwrap().task.state, "open");
}
