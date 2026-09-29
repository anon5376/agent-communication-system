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
