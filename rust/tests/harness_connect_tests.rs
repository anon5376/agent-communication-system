//! Connecting any agent CLI to the bus: `aos connect` for the CLIs aos knows
//! and for any other command line, the per-agent bus wiring each adapter gets,
//! and a supervisor running a stand-in CLI end to end. No real agent CLI runs.

use acs::adapters::{get_harness_adapter, AdapterContext};
use acs::aos::crew::{self, Connect, Found, Paths, CLIS};
use acs::aos::ensure_operator;
use acs::bus::{Bus, CreateTaskInput};
use acs::config::{load_config, resolve_agent};
use acs::supervisor::*;
use acs::types::OPERATOR_ID;
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn fresh_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "acs-connect-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn bus_in(dir: &Path) -> (Bus, Paths) {
    let db = dir.join("bus.db");
    let bus = Bus::open(Some(&db)).unwrap();
    assert!(ensure_operator(&bus).unwrap());
    let paths = Paths::for_db(&bus.db_path);
    (bus, paths)
}

/// Detection results with the named CLIs "installed".
fn only(ids: &[&str]) -> Vec<Found> {
    CLIS.iter()
        .map(|cli| Found {
            cli,
            path: ids.contains(&cli.id).then(|| PathBuf::from("/bin/true")),
            version: None,
        })
        .collect()
}

fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn req(name: &str, seat: Option<&str>, command: &[&str], auto_approve: bool) -> Connect {
    Connect {
        name: name.to_string(),
        seat: seat.map(str::to_string),
        command: command.iter().map(|s| s.to_string()).collect(),
        auto_approve,
    }
}

#[test]
fn auto_approving_clis_need_the_operators_word() {
    let dir = fresh_dir("gate");
    let (bus, paths) = bus_in(&dir);
    let found = only(&["claude", "gemini", "hermes"]);
    crew::setup(&bus, &paths, &found, false).unwrap();

    let err = crew::connect(&bus, &paths, &found, &req("gemini", None, &[], false)).unwrap_err();
    assert!(err.message.contains("--auto-approve"), "{}", err.message);
    assert!(!fs::read_to_string(paths.crew())
        .unwrap()
        .contains("\"gemini\""));

    let said = crew::connect(
        &bus,
        &paths,
        &found,
        &req("gemini", Some("reviewer"), &[], true),
    )
    .unwrap();
    assert!(
        said.iter()
            .any(|l| l.contains("reviewer now runs on gemini")),
        "{said:?}"
    );
    assert_eq!(crew::allowed(&paths), vec!["gemini".to_string()]);
    let config = load_config(&paths.crew()).unwrap();
    assert_eq!(config.agents["reviewer"].model, "gemini");
    assert!(config.harnesses["gemini"].features.mcp);
    assert_eq!(config.harnesses["gemini"].options["autoApprove"], true);

    // Once allowed, a rebuilt crew may use it too; still never as the lead
    // when the CLI has no bus tools.
    assert!(crew::plan_with(&only(&["hermes"]), &["hermes".to_string()]).is_empty());
    let plan = crew::plan_with(&only(&["gemini"]), &["gemini".to_string()]);
    assert_eq!(plan.len(), 3);
    assert!(plan.iter().all(|m| m.cli.id == "gemini"));
    assert!(crew::plan(&only(&["gemini"])).is_empty());

    // Not installed is a clear failure, not a crew pointing at nothing.
    let err = crew::connect(&bus, &paths, &found, &req("kimi", None, &[], true)).unwrap_err();
    assert!(err.message.contains("isn't installed"), "{}", err.message);
}

#[test]
fn any_cli_joins_through_its_command_line() {
    let dir = fresh_dir("custom");
    let (bus, paths) = bus_in(&dir);
    let found = only(&["claude"]);
    crew::setup(&bus, &paths, &found, false).unwrap();
    let tool = script(&dir, "mytool", "echo hi");

    // No bus tools: fine as a worker, refused as the lead.
    let err = crew::connect(
        &bus,
        &paths,
        &found,
        &req(
            "mytool",
            Some("lead"),
            &[tool.to_str().unwrap(), "--yes", "{prompt}"],
            false,
        ),
    )
    .unwrap_err();
    assert!(err.message.contains("bus tools"), "{}", err.message);

    let said = crew::connect(
        &bus,
        &paths,
        &found,
        &req("mytool", None, &[tool.to_str().unwrap(), "--yes"], false),
    )
    .unwrap();
    assert!(
        said.iter().any(|l| l.contains("mytool joined the crew")),
        "{said:?}"
    );
    assert!(
        said.iter().any(|l| l.contains("brief goes last")),
        "{said:?}"
    );
    let config = load_config(&paths.crew()).unwrap();
    let h = &config.harnesses["mytool"];
    assert_eq!(h.adapter, "command");
    assert!(!h.features.mcp);
    assert_eq!(h.options["args"], serde_json::json!(["--yes", "{prompt}"]));
    assert!(paths.roles().join("mytool.md").exists());
    assert!(bus.get_agent("mytool").unwrap().is_some());

    // {mcpConfig} in the command line means it gets the bus tools, so it can lead.
    crew::connect(
        &bus,
        &paths,
        &found,
        &req(
            "mcptool",
            Some("lead"),
            &[
                tool.to_str().unwrap(),
                "--mcp",
                "{mcpConfig}",
                "-p",
                "{prompt}",
            ],
            false,
        ),
    )
    .unwrap();
    let config = load_config(&paths.crew()).unwrap();
    assert!(config.harnesses["mcptool"].features.mcp);
    assert_eq!(config.agents["lead"].model, "mcptool");

    let lines = crew::connections(&paths, &found);
    let line = |id: &str| lines.iter().find(|l| l.1 == id).unwrap().2.clone();
    assert!(line("mytool").contains("supervisor-managed"), "{lines:?}");
    assert!(
        line("mcptool").contains("in crew: lead / bus tools"),
        "{lines:?}"
    );
    assert!(line("gemini").contains("not installed"), "{lines:?}");

    // A seat still on it blocks disconnect; a teammate named after it leaves with it.
    let err = crew::disconnect(&paths, "mcptool").unwrap_err();
    assert!(err.message.contains("lead still run"), "{}", err.message);
    crew::disconnect(&paths, "mytool").unwrap();
    let config = load_config(&paths.crew()).unwrap();
    assert!(!config.harnesses.contains_key("mytool"));
    assert!(!config.agents.contains_key("mytool"));
}

#[test]
fn a_broken_edit_leaves_the_crew_file_as_it_was() {
    let dir = fresh_dir("broken");
    let (bus, paths) = bus_in(&dir);
    let found = only(&["claude"]);
    crew::setup(&bus, &paths, &found, false).unwrap();
    let before = fs::read_to_string(paths.crew()).unwrap();
    let err = crew::connect(
        &bus,
        &paths,
        &found,
        &req("nosuch", None, &["definitely-not-on-path-xyz"], false),
    )
    .unwrap_err();
    assert!(err.message.contains("no program"), "{}", err.message);
    let err = crew::connect(&bus, &paths, &found, &req("Bad Name", None, &[], false)).unwrap_err();
    assert!(!err.message.is_empty());
    assert_eq!(fs::read_to_string(paths.crew()).unwrap(), before);
}

/// The invocation an adapter builds for one agent of a crew file.
fn invocation(
    paths: &Paths,
    db: &Path,
    agent: &str,
    workdir: &Path,
) -> (acs::adapters::HarnessInvocation, Vec<PathBuf>) {
    let config = load_config(&paths.crew()).unwrap();
    let resolved = resolve_agent(&config, agent).unwrap();
    let adapter = get_harness_adapter(&resolved.harness.adapter).unwrap();
    let context = AdapterContext {
        agent: &resolved,
        prompt: "do the work".to_string(),
        session_id: None,
        pinned_session_id: None,
        workdir: workdir.display().to_string(),
        qagent_bin: "/opt/aos".to_string(),
        mcp_server_path: "mcp".to_string(),
        fake_harness_path: "fake-harness".to_string(),
        bus_environment: HashMap::from([
            ("QAGENT_AGENT_ID".to_string(), agent.to_string()),
            ("QAGENT_BUS_DB".to_string(), db.display().to_string()),
        ]),
        mcp_command: Some(mcp_command_for(agent, db, "/opt/aos")),
    };
    if let Some(prepare) = adapter.prepare {
        prepare(&context).unwrap();
    }
    let inv = (adapter.build)(&context);
    let written = fs::read_dir(paths.home.join("mcp"))
        .map(|d| d.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    (inv, written)
}

fn server_in(config: &serde_json::Value, agent: &str) {
    let server = &config["mcpServers"]["qagent"];
    assert_eq!(server["command"], "/opt/aos", "{config}");
    assert_eq!(server["args"], serde_json::json!(["mcp"]));
    assert_eq!(server["env"]["QAGENT_AGENT_ID"], agent);
}

#[test]
fn each_adapter_hands_its_agent_the_bus_as_itself() {
    let dir = fresh_dir("wiring");
    let (bus, paths) = bus_in(&dir);
    let workdir = fresh_dir("wiring-work");
    let found = only(&["claude", "gemini", "kimi", "opencode", "grok", "hermes"]);
    crew::setup(&bus, &paths, &found, false).unwrap();
    for (cli, seat) in [
        ("gemini", "g1"),
        ("kimi", "k1"),
        ("opencode", "o1"),
        ("grok", "x1"),
        ("hermes", "h1"),
    ] {
        crew::connect(&bus, &paths, &found, &req(cli, Some(seat), &[], true)).unwrap();
    }
    let db = bus.db_path.clone();

    // Gemini: a per-agent settings file named by GEMINI_CLI_SYSTEM_SETTINGS_PATH.
    let (inv, _) = invocation(&paths, &db, "g1", &workdir);
    let settings = PathBuf::from(&inv.environment["GEMINI_CLI_SYSTEM_SETTINGS_PATH"]);
    assert!(
        settings.starts_with(paths.home.join("mcp")),
        "{}",
        settings.display()
    );
    server_in(
        &serde_json::from_str(&fs::read_to_string(&settings).unwrap()).unwrap(),
        "g1",
    );
    assert!(inv
        .args
        .windows(2)
        .any(|w| w == ["--approval-mode", "yolo"]));
    assert_eq!(inv.environment["GEMINI_CLI_TRUST_WORKSPACE"], "true");

    // Kimi: print mode plus the bus server inline.
    let (inv, _) = invocation(&paths, &db, "k1", &workdir);
    assert!(inv.args.contains(&"--print".to_string()), "{:?}", inv.args);
    assert!(!inv.args.contains(&"--auto".to_string()));
    let at = inv.args.iter().position(|a| a == "--mcp-config").unwrap();
    server_in(&serde_json::from_str(&inv.args[at + 1]).unwrap(), "k1");

    // OpenCode: inline config in the environment, nothing written to the project.
    let (inv, _) = invocation(&paths, &db, "o1", &workdir);
    let cfg: serde_json::Value =
        serde_json::from_str(&inv.environment["OPENCODE_CONFIG_CONTENT"]).unwrap();
    assert_eq!(
        cfg["mcp"]["qagent"]["command"],
        serde_json::json!(["/opt/aos", "mcp"])
    );
    assert_eq!(cfg["mcp"]["qagent"]["environment"]["QAGENT_AGENT_ID"], "o1");
    assert!(!workdir.join("opencode.json").exists());

    // Grok and Hermes have no bus tools: the supervisor claims and submits.
    let config = load_config(&paths.crew()).unwrap();
    assert!(supervisor_managed(&resolve_agent(&config, "x1").unwrap()));
    assert!(supervisor_managed(&resolve_agent(&config, "h1").unwrap()));
    let (inv, _) = invocation(&paths, &db, "h1", &workdir);
    assert!(inv.args.contains(&"--yolo".to_string()));

    // autoApprove false in crew.json drops every approval-skipping flag.
    let mut crew_json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(paths.crew()).unwrap()).unwrap();
    for h in ["gemini", "opencode", "grok", "hermes"] {
        crew_json["harnesses"][h]["options"]["autoApprove"] = serde_json::json!(false);
    }
    fs::write(
        paths.crew(),
        serde_json::to_string_pretty(&crew_json).unwrap(),
    )
    .unwrap();
    let (inv, _) = invocation(&paths, &db, "g1", &workdir);
    assert!(!inv.args.contains(&"yolo".to_string()));
    assert!(!inv.environment.contains_key("GEMINI_CLI_TRUST_WORKSPACE"));
    let (inv, _) = invocation(&paths, &db, "o1", &workdir);
    assert!(!inv.args.contains(&"--auto".to_string()));
    let (inv, _) = invocation(&paths, &db, "x1", &workdir);
    assert!(!inv.args.contains(&"--always-approve".to_string()));
    let (inv, _) = invocation(&paths, &db, "h1", &workdir);
    assert!(!inv.args.contains(&"--yolo".to_string()));
}

// ------------------------------------------------------------- end to end

struct Running {
    stop: Arc<AtomicBool>,
    handle: std::thread::JoinHandle<acs::error::Result<()>>,
}

fn supervise_in_thread(db: &Path, paths: &Paths, agent: &str, workdir: &Path) -> Running {
    let stop = Arc::new(AtomicBool::new(false));
    let options = SuperviseOptions {
        agent_id: agent.to_string(),
        workdir: workdir.display().to_string(),
        db_path: db.to_path_buf(),
        config_path: Some(paths.crew()),
        stop: Arc::clone(&stop),
        wait_ms: Some(2_000),
        fake_harness_path: None,
        qagent_bin: Some(env!("CARGO_BIN_EXE_qagent").to_string()),
        log: Some(Box::new(|_| {})),
    };
    Running {
        stop,
        handle: std::thread::spawn(move || supervise(options)),
    }
}

fn wait_for<F: Fn() -> bool>(what: &str, check: F) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if check() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("timed out waiting for {what}");
}

#[test]
fn a_cli_without_bus_tools_works_a_task_and_talks_from_its_shell() {
    let dir = fresh_dir("e2e-managed");
    let (bus, paths) = bus_in(&dir);
    let workdir = fresh_dir("e2e-managed-work");
    let found = only(&["claude"]);
    crew::setup(&bus, &paths, &found, false).unwrap();
    // A stand-in CLI: messages the operator through $QAGENT_CLI, then answers.
    let tool = script(
        &dir,
        "echoer",
        r#""$QAGENT_CLI" send operator "progress" "started as $QAGENT_AGENT_ID" >/dev/null || exit 3
printf '%s' "$1" | grep -q 'do the thing' && echo "echoer finished the task it was briefed on""#,
    );
    crew::connect(
        &bus,
        &paths,
        &found,
        &req("echoer", None, &[tool.to_str().unwrap(), "{prompt}"], false),
    )
    .unwrap();

    let run = supervise_in_thread(&bus.db_path, &paths, "echoer", &workdir);
    let op = bus.identify(Some(OPERATOR_ID)).unwrap();
    bus.create_task(
        &op,
        CreateTaskInput {
            title: "do the thing".to_string(),
            to: Some("echoer".to_string()),
            ..Default::default()
        },
    )
    .unwrap();
    wait_for("task submitted", || {
        bus.get_task(1)
            .map(|t| t.task.state == "submitted")
            .unwrap_or(false)
    });
    run.stop.store(true, Ordering::SeqCst);
    run.handle.join().unwrap().unwrap();

    let task = bus.get_task(1).unwrap();
    let summary = task.task.result.unwrap().summary;
    assert!(
        summary.contains("echoer finished the task it was briefed on"),
        "{summary}"
    );
    let inbox = Command::new(env!("CARGO_BIN_EXE_qagent"))
        .args([
            "--db",
            bus.db_path.to_str().unwrap(),
            "--as",
            OPERATOR_ID,
            "inbox",
            "--peek",
        ])
        .output()
        .unwrap();
    let inbox = String::from_utf8_lossy(&inbox.stdout);
    assert!(inbox.contains("started as echoer"), "{inbox}");
}

#[test]
fn mcp_config_file_starts_a_bus_server_for_that_agent() {
    let dir = fresh_dir("e2e-mcp");
    let (bus, paths) = bus_in(&dir);
    let workdir = fresh_dir("e2e-mcp-work");
    let found = only(&["claude"]);
    crew::setup(&bus, &paths, &found, false).unwrap();
    // A stand-in CLI that keeps a copy of the MCP config it was handed.
    let tool = script(
        &dir,
        "mcpcli",
        r#"cp "$2" "$PWD/seen-mcp.json" && echo "ok""#,
    );
    crew::connect(
        &bus,
        &paths,
        &found,
        &req(
            "mcpcli",
            None,
            &[
                tool.to_str().unwrap(),
                "--mcp-config",
                "{mcpConfig}",
                "{prompt}",
            ],
            false,
        ),
    )
    .unwrap();

    let run = supervise_in_thread(&bus.db_path, &paths, "mcpcli", &workdir);
    let op = bus.identify(Some(OPERATOR_ID)).unwrap();
    bus.send(
        &op,
        acs::bus::SendInput {
            to: "mcpcli".to_string(),
            subject: Some("hello".to_string()),
            body: "hi".to_string(),
            msg_type: None,
            thread: None,
            task_id: None,
            refs: None,
            requires_ack: false,
        },
    )
    .unwrap();
    wait_for("the CLI ran", || workdir.join("seen-mcp.json").exists());
    run.stop.store(true, Ordering::SeqCst);
    run.handle.join().unwrap().unwrap();

    // Launch the server exactly as the file says and ask who it is.
    let cfg: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(workdir.join("seen-mcp.json")).unwrap()).unwrap();
    let server = &cfg["mcpServers"]["qagent"];
    let mut cmd = Command::new(server["command"].as_str().unwrap());
    for a in server["args"].as_array().unwrap() {
        cmd.arg(a.as_str().unwrap());
    }
    for (k, v) in server["env"].as_object().unwrap() {
        cmd.env(k, v.as_str().unwrap());
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    for (id, method, params) in [
        (
            1,
            "initialize",
            serde_json::json!({"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}}),
        ),
        (
            2,
            "tools/call",
            serde_json::json!({"name": "bus_whoami", "arguments": {}}),
        ),
    ] {
        writeln!(
            stdin,
            "{}",
            serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
        )
        .unwrap();
    }
    let mut whoami = String::new();
    for line in lines.by_ref() {
        let line = line.unwrap();
        if line.contains("\"id\":2") {
            whoami = line;
            break;
        }
    }
    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
    assert!(whoami.contains("mcpcli"), "{whoami}");
}
