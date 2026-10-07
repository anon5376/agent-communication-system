use acs::bus::{Bus, CreateTaskInput, SubmitInput};
use acs::types::OPERATOR_ID;
use proptest::prelude::*;
use proptest::test_runner::{Config, TestRunner};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn workspace(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "acs-launch-{label}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn retry_busy<T>(mut operation: impl FnMut() -> acs::error::Result<T>) -> acs::error::Result<T> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match operation() {
            Err(error)
                if error.code == acs::error::Code::Conflict
                    && error.message == "database is busy"
                    && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            result => return result,
        }
    }
}

#[test]
fn fifty_agents_complete_five_hundred_tasks_without_duplicate_claims() {
    let home = workspace("race");
    let db = home.join("bus.db");
    let bus = Bus::open(Some(&db)).unwrap();
    bus.init().unwrap();
    let operator = bus.identify(Some(OPERATOR_ID)).unwrap();
    for index in 0..50 {
        bus.add_agent(
            &operator,
            &format!("worker-{index}"),
            Some("worker"),
            None,
            None,
            None,
            None,
        )
        .unwrap();
    }
    for index in 0..500 {
        bus.create_task(
            &operator,
            CreateTaskInput {
                title: format!("Concurrent task {index}"),
                reviewer: Some(OPERATOR_ID.into()),
                ..Default::default()
            },
        )
        .unwrap();
    }
    let barrier = Arc::new(Barrier::new(50));
    let workers: Vec<_> = (0..50)
        .map(|index| {
            let db = db.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                let connection = Bus::open(Some(&db));
                barrier.wait();
                let bus = connection.unwrap();
                let actor = bus.identify(Some(&format!("worker-{index}"))).unwrap();
                let mut claimed = Vec::new();
                loop {
                    let task = match retry_busy(|| bus.claim_task(&actor, None)) {
                        Ok(task) => task,
                        Err(error) if error.code.as_str() == "not_found" => break,
                        Err(error) => panic!("unexpected claim failure: {error}"),
                    };
                    assert_eq!(task.assignee.as_deref(), Some(actor.agent_id.as_str()));
                    assert_eq!(
                        retry_busy(|| bus.submit_task(
                            &actor,
                            task.id,
                            SubmitInput {
                                summary: "simulated work, independently reviewed by test operator"
                                    .into(),
                                ..Default::default()
                            }
                        ))
                        .unwrap()
                        .state,
                        "submitted"
                    );
                    claimed.push(task.id);
                }
                claimed
            })
        })
        .collect();
    let mut all = Vec::new();
    for worker in workers {
        all.extend(worker.join().unwrap());
    }
    assert_eq!(all.len(), 500);
    assert_eq!(
        all.iter().collect::<HashSet<_>>().len(),
        500,
        "duplicate task claim"
    );
    for id in all {
        assert_eq!(
            bus.review_task(&operator, id, true, "verified by independent operator")
                .unwrap()
                .state,
            "accepted"
        );
    }
    let connection = rusqlite::Connection::open(&db).unwrap();
    let integrity: String = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
    drop(connection);
    drop(bus);
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
#[ignore = "two-hour release soak; run explicitly with --ignored --nocapture"]
fn release_two_hour_soak() {
    let home = workspace("soak");
    let db = home.join("bus.db");
    let bus = Bus::open(Some(&db)).unwrap();
    bus.init().unwrap();
    let operator = bus.identify(Some(OPERATOR_ID)).unwrap();
    bus.add_agent(&operator, "worker", Some("worker"), None, None, None, None)
        .unwrap();
    drop(bus);
    let started = Instant::now();
    let mut completed = 0i64;
    while started.elapsed() < Duration::from_secs(7_200) {
        let bus = Bus::open(Some(&db)).unwrap();
        let operator = bus.identify(Some(OPERATOR_ID)).unwrap();
        let worker = bus.identify(Some("worker")).unwrap();
        let task = bus
            .create_task(
                &operator,
                CreateTaskInput {
                    title: format!("Soak task {completed}"),
                    brief: Some("preserve this brief".into()),
                    acceptance: Some("preserve these criteria".into()),
                    reviewer: Some(OPERATOR_ID.into()),
                    path_scopes: vec!["src/soak.rs".into()],
                    ..Default::default()
                },
            )
            .unwrap();
        bus.claim_task(&worker, Some(task.id)).unwrap();
        if completed % 10 == 0 {
            let requeued = bus
                .requeue_task(&operator, task.id, Some("simulated stalled worker"))
                .unwrap();
            assert_eq!(requeued.brief, task.brief);
            assert_eq!(requeued.acceptance, task.acceptance);
            assert_eq!(requeued.path_scopes, task.path_scopes);
            assert_eq!(requeued.reviewer, task.reviewer);
            bus.claim_task(&worker, Some(task.id)).unwrap();
        }
        bus.submit_task(
            &worker,
            task.id,
            SubmitInput {
                summary: "simulated complete".into(),
                ..Default::default()
            },
        )
        .unwrap();
        bus.review_task(&operator, task.id, true, "checked")
            .unwrap();
        drop(bus);
        let reopened = Bus::open(Some(&db)).unwrap();
        assert_eq!(reopened.get_task(task.id).unwrap().task.state, "accepted");
        drop(reopened);
        completed += 1;
        if completed % 100 == 0 {
            println!(
                "soak: {completed} accepted tasks, {}s elapsed",
                started.elapsed().as_secs()
            );
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    let connection = rusqlite::Connection::open(&db).unwrap();
    let (total, accepted): (i64, i64) = connection
        .query_row(
            "SELECT count(*), sum(state = 'accepted') FROM tasks",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((total, accepted), (completed, completed));
    let integrity: String = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
    println!(
        "SOAK PASSED: {completed} accepted tasks, {}s elapsed, integrity {integrity}",
        started.elapsed().as_secs()
    );
    drop(connection);
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn deterministic_json_and_cli_mutations_return_errors_without_panicking() {
    let home = workspace("mutations");
    let db = home.join("bus.db");
    let bus = Bus::open(Some(&db)).unwrap();
    bus.init().unwrap();
    let seed =
        serde_json::json!({ "version": 1, "dbPath": db, "action": "snapshot", "payload": {} })
            .to_string();
    let alphabet = b"{}[]\"\\:,0123456789nullfalsexyz \n";
    for index in 0..512 {
        let mut candidate = seed.as_bytes().to_vec();
        let position = index % candidate.len();
        match index % 3 {
            0 => {
                candidate[position] = alphabet[index % alphabet.len()];
            }
            1 => {
                candidate.remove(position);
            }
            _ => {
                candidate.insert(position, alphabet[index % alphabet.len()]);
            }
        }
        let input = String::from_utf8(candidate).unwrap();
        let (body, code) = acs::desktop::respond(&input);
        let reply: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(reply["ok"].is_boolean(), "{body}");
        assert_eq!(code == 0, reply["ok"] == true, "{input}: {body}");
    }
    for invalid in [
        "abc",
        "-1",
        "0",
        "1.1",
        "NaN",
        "999999999999999999999999999999999",
        "../bus.db",
        "☃",
    ] {
        let args = [
            "--db",
            db.to_str().unwrap(),
            "--as",
            "operator",
            "task",
            "show",
            invalid,
        ]
        .into_iter()
        .map(String::from)
        .collect::<Vec<_>>();
        let mut io = acs::cli::Io::default();
        assert_ne!(
            acs::cli::run(&args, &mut io),
            0,
            "accepted invalid task id {invalid}"
        );
    }
    let count: i64 = rusqlite::Connection::open(&db)
        .unwrap()
        .query_row("SELECT count(*) FROM tasks", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 0, "read-only malformed requests created work");
    drop(bus);
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn property_based_protocol_and_cli_inputs_preserve_read_only_state() {
    let home = workspace("properties");
    let db = home.join("bus.db");
    let bus = Bus::open(Some(&db)).unwrap();
    bus.init().unwrap();
    let text = proptest::collection::vec(any::<char>(), 0..128)
        .prop_map(|chars| chars.into_iter().collect::<String>());
    let leaf = prop_oneof![
        Just(serde_json::Value::Null),
        any::<bool>().prop_map(serde_json::Value::from),
        any::<i64>().prop_map(serde_json::Value::from),
        text.clone().prop_map(serde_json::Value::from),
    ];
    let payload = leaf.prop_recursive(3, 32, 8, |inner| {
        prop_oneof![
            proptest::collection::vec(inner.clone(), 0..8).prop_map(serde_json::Value::Array),
            proptest::collection::btree_map("[a-z_]{0,12}", inner, 0..8)
                .prop_map(|map| serde_json::Value::Object(map.into_iter().collect())),
        ]
    });
    let args = proptest::collection::vec(
        prop_oneof![
            text.clone(),
            text.clone().prop_map(|value| format!("--scope={value}")),
            Just("--".to_string()),
            Just("--json".to_string()),
        ],
        0..24,
    );
    let mut runner = TestRunner::new(Config {
        cases: 2048,
        ..Config::default()
    });
    runner
        .run(
            &(text, payload, args, any::<bool>()),
            |(raw, value, args, snapshot)| {
                if serde_json::from_str::<serde_json::Value>(&raw).is_err() {
                    let (body, code) = acs::desktop::respond(&raw);
                    let reply: serde_json::Value = serde_json::from_str(&body).unwrap();
                    prop_assert_ne!(code, 0);
                    prop_assert_eq!(reply["ok"].as_bool(), Some(false));
                    prop_assert!(reply["error"]["message"].is_string());
                }
                let input = serde_json::json!({
                    "version": 1, "dbPath": db,
                    "action": if snapshot { "snapshot" } else { "task" },
                    "payload": { "id": value },
                });
                let (body, code) = acs::desktop::respond(&input.to_string());
                let reply: serde_json::Value = serde_json::from_str(&body).unwrap();
                prop_assert!(reply["ok"].is_boolean());
                prop_assert_eq!(code == 0, reply["ok"] == true);
                let _ = acs::cli::parse_args(&args);
                let escaped = std::iter::once("--".to_string())
                    .chain(args.clone())
                    .collect::<Vec<_>>();
                let parsed = acs::cli::parse_args(&escaped).unwrap();
                prop_assert_eq!(parsed.positionals, args);
                prop_assert!(parsed.flags.is_empty());
                Ok(())
            },
        )
        .unwrap();
    let connection = rusqlite::Connection::open(&db).unwrap();
    let count: i64 = connection
        .query_row("SELECT count(*) FROM tasks", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 0);
    let integrity: String = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
    drop(connection);
    drop(bus);
    std::fs::remove_dir_all(home).unwrap();
}
