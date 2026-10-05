//! Transport tests for the acs-desktop pipe protocol, driven against the
//! `fake-acs-desktop` double. The cases mirror what the macOS client guards
//! against: a normal exchange, error envelopes, crashed helpers, timeouts,
//! oversized output, a missing executable, and a descendant that inherits our
//! pipes (where waiting on EOF would hang forever).

use acs_windows::helper::{request, request_with_timeout, AcsError};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn fake() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_fake-acs-desktop"))
}

fn scratch_db(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("acs-transport-test-{test}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("bus.db")
}

fn call(db: &Path, action: &str, payload: Value) -> Result<String, AcsError> {
    request(&fake(), db, action, payload)
}

#[test]
fn success_snapshot_roundtrip() {
    let db = scratch_db("success");
    call(&db, "demo", json!({})).expect("demo should seed");
    let data = call(&db, "snapshot", json!({})).expect("snapshot should reply");
    let snapshot: Value = serde_json::from_str(&data).unwrap();
    assert_eq!(snapshot["simulated"], true);
    assert_eq!(snapshot["canOperate"], true);
    assert!(snapshot["agents"].as_array().unwrap().len() >= 5);
    assert_eq!(snapshot["tasks"].as_array().unwrap().len(), 7);
}

#[test]
fn error_envelope_is_authoritative() {
    let db = scratch_db("missing");
    // No bus at dbPath: the helper says so with a structured error + exit 1.
    match call(&db, "snapshot", json!({})) {
        Err(AcsError::HelperError { code, message }) => {
            assert_eq!(code, "not_found");
            assert!(message.contains("no bus at"), "got: {message}");
        }
        other => panic!("expected HelperError, got {other:?}"),
    }
}

#[test]
fn nonzero_exit_without_envelope() {
    let db = scratch_db("exit1");
    match call(&db, "__test_exit1", json!({})) {
        Err(AcsError::HelperExited { status, .. }) => assert_eq!(status, 1),
        other => panic!("expected HelperExited, got {other:?}"),
    }
}

#[test]
fn ok_envelope_with_nonzero_exit_is_rejected() {
    let db = scratch_db("okexit1");
    match call(&db, "__test_ok_exit1", json!({})) {
        Err(AcsError::HelperExited { status, .. }) => assert_eq!(status, 1),
        other => panic!("expected HelperExited, got {other:?}"),
    }
}

#[test]
fn timeout_kills_the_helper() {
    let db = scratch_db("hang");
    let started = Instant::now();
    match request_with_timeout(&fake(), &db, "__test_hang", json!({}), Duration::from_secs(2)) {
        Err(AcsError::TimedOut(2)) => {}
        other => panic!("expected TimedOut, got {other:?}"),
    }
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "timeout path took too long"
    );
}

#[test]
fn oversized_stdout_is_rejected() {
    let db = scratch_db("flood");
    match call(&db, "__test_flood", json!({})) {
        Err(AcsError::OutputTruncated) => {}
        other => panic!("expected OutputTruncated, got {other:?}"),
    }
}

#[test]
fn malformed_stdout_is_rejected() {
    let db = scratch_db("garbage");
    match call(&db, "__test_garbage", json!({})) {
        Err(AcsError::MalformedResponse(_)) => {}
        other => panic!("expected MalformedResponse, got {other:?}"),
    }
}

#[test]
fn missing_helper_reports_unavailable() {
    let nowhere = Path::new("C:/definitely/not/a/real/acs-desktop.exe");
    match request(nowhere, Path::new("C:/definitely/not/bus.db"), "snapshot", json!({})) {
        Err(AcsError::HelperUnavailable(_)) => {}
        other => panic!("expected HelperUnavailable, got {other:?}"),
    }
}

#[test]
fn descendant_holding_pipes_does_not_hang() {
    // The fake replies, then spawns a child that inherits our pipes and lives
    // 30s. Waiting on pipe EOF would block for those 30s; waiting on the
    // child handle returns immediately.
    let db = scratch_db("leak");
    let started = Instant::now();
    let data = call(&db, "__test_leak_pipe", json!({})).expect("leak pipe reply");
    let value: Value = serde_json::from_str(&data).unwrap();
    assert_eq!(value["leaked"], true);
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "a descendant holding our pipes stalled the request"
    );
}

#[test]
fn i64_ids_pass_through_exactly() {
    // Above f64's 2^53 precise range: serde_json arbitrary_precision must
    // preserve the digits verbatim in the reply text.
    let db = scratch_db("i64");
    call(&db, "demo", json!({})).expect("demo should seed");
    let id = 9_007_199_254_740_993i64;
    let mut payload = serde_json::Map::new();
    payload.insert("id".into(), Value::from(id));
    // The fake rejects unknown ids with not_found — but the request itself
    // must carry the digits exactly, which we prove by the error mentioning
    // the full id rather than a rounded value.
    match call(&db, "task", Value::Object(payload)) {
        Err(AcsError::HelperError { code, message }) => {
            assert_eq!(code, "not_found");
            assert!(message.contains("9007199254740993"), "got: {message}");
        }
        other => panic!("expected HelperError, got {other:?}"),
    }
}
