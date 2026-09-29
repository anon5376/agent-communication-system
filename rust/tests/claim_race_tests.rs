//! Multi-process claim race — porting tests/core-claim.test.ts:
//! N simultaneous `qagent task claim` processes produce exactly one winner.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const QAGENT: &str = env!("CARGO_BIN_EXE_qagent");
const PROCESSES: usize = 8;

fn fresh_home() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "acs-rust-race-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn run(args: &[&str]) -> (i32, String, String) {
    let output = Command::new(QAGENT)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

#[test]
fn simultaneous_claims_produce_exactly_one_winner() {
    let home = fresh_home();
    let db = home.join("bus.db");
    let db_str = db.to_str().unwrap();

    run(&["--db", db_str, "init"]);
    for i in 0..PROCESSES {
        let (code, _, err) = run(&[
            "--db",
            db_str,
            "--as",
            "operator",
            "agent",
            "add",
            &format!("w{i}"),
            "--role",
            "implementation",
        ]);
        assert_eq!(code, 0, "agent add failed: {err}");
    }
    let (code, out, err) = run(&[
        "--db",
        db_str,
        "--as",
        "operator",
        "task",
        "add",
        "race target",
        "--role",
        "implementation",
    ]);
    assert_eq!(code, 0, "task add failed: {err} {out}");

    // Fire all claim processes at once.
    let mut children = Vec::new();
    for i in 0..PROCESSES {
        children.push(
            Command::new(QAGENT)
                .args(["--db", db_str, "--as", &format!("w{i}"), "task", "claim"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
    }
    let mut wins = 0;
    let mut conflicts = 0;
    for child in children {
        let output = child.wait_with_output().unwrap();
        let code = output.status.code().unwrap_or(-1);
        let stderr = String::from_utf8_lossy(&output.stderr);
        match code {
            0 => wins += 1,
            1 => {
                assert!(
                    stderr.contains("cannot be claimed") || stderr.contains("no claimable task"),
                    "unexpected loser error: {stderr}"
                );
                conflicts += 1;
            }
            other => panic!("claim exited {other}: {stderr}"),
        }
    }
    assert_eq!(wins, 1, "expected exactly one claim winner");
    assert_eq!(conflicts, PROCESSES - 1);

    // The task is claimed once.
    let (_, out, _) = run(&[
        "--db", db_str, "--as", "operator", "task", "show", "1", "--json",
    ]);
    let json: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(json["state"], "claimed");
}

#[test]
fn doctor_reports_bus_agents_and_config() {
    let home = fresh_home();
    let db = home.join("bus.db");
    let db_str = db.to_str().unwrap();

    // Missing bus: exit 1 with the missing hint.
    let (code, out, _) = run(&["--db", db_str, "doctor"]);
    assert_eq!(code, 1);
    assert!(out.contains("(missing: run `qagent init`)"));

    run(&["--db", db_str, "init"]);
    let (code, out, _) = run(&["--db", db_str, "doctor"]);
    assert_eq!(code, 0, "doctor output: {out}");
    assert!(out.contains("agents operator"));
    assert!(out.trim_end().ends_with("ok"));

    // A config-resolvable agent with no token: problem listed, exit 1.
    let project = fresh_home();
    fs::create_dir_all(project.join(".qagent")).unwrap();
    fs::write(
        project.join(".qagent/config.json"),
        r#"{
          "version": 1,
          "capabilityNotice": "",
          "providers": { "p": { "id": "p", "enabled": true } },
          "harnesses": { "h": { "id": "h", "adapter": "fake", "command": "qagent-fake-harness", "providers": ["p"], "features": {}, "enabled": true } },
          "models": { "m": { "id": "m", "provider": "p", "harness": "h", "family": "f", "capabilities": { "coding": 0.5, "reasoning": 0.5, "planning": 0.5, "debugging": 0.5, "research": 0.5, "toolUse": 0.5, "speed": 0.5, "tokenEfficiency": 0.5, "reliability": 0.5, "autonomy": 0.5, "contextTokens": 1000 }, "enabled": true } },
          "agents": { "a1": { "id": "a1", "model": "m", "role": "r", "authority": "worker", "description": "", "enabled": true, "autoStart": true, "permissions": { "canDelegate": false, "canReview": false, "filesystem": "none", "shell": false, "network": false, "maxDelegationDepth": 0 } } },
          "roles": { "r": { "id": "r" } },
          "routing": { "weights": {}, "fallbackRoles": {}, "minimumScore": 0 },
          "constraints": { "maxDelegationDepth": 0, "maxConcurrentTasks": 1, "maxRetries": 0 }
        }"#,
    )
    .unwrap();
    let project_str = project.to_str().unwrap();
    let (code, out, _) = run(&["--db", db_str, "doctor", "a1", project_str]);
    assert_eq!(code, 1);
    assert!(out.contains("config "), "doctor output: {out}");
    assert!(
        out.contains("h (qagent-fake-harness), autoStart true"),
        "doctor output: {out}"
    );
    assert!(
        out.contains("problem: identity a1:"),
        "doctor output: {out}"
    );
    assert!(out.contains("1 problem(s)"));
}
