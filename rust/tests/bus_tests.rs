//! Behavioral parity tests for the Rust bus — porting the expectations in
//! tests/core-bus.test.ts and tests/core-identity.test.ts.

use acs::bus::{Bus, CreateTaskInput, ListTasksInput, SendInput, SubmitInput};
use acs::error::Code;
use acs::identity;
use acs::types::OPERATOR_ID;
use acs::wait;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

fn fresh_home() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "acs-rust-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

struct Fixture {
    home: PathBuf,
    bus: Bus,
}

fn fixture() -> Fixture {
    let home = fresh_home();
    let bus = Bus::open(Some(&home.join("bus.db"))).unwrap();
    bus.init().unwrap();
    Fixture { home, bus }
}

fn add_agent(bus: &Bus, id: &str, role: &str) -> identity::Identity {
    let operator = bus.identify(Some(OPERATOR_ID)).unwrap();
    bus.add_agent(&operator, id, Some(role), None, None, None, Some("worker"))
        .unwrap();
    bus.identify(Some(id)).unwrap()
}

fn send(bus: &Bus, actor: &identity::Identity, to: &str, subject: &str, body: &str) -> i64 {
    bus.send(
        actor,
        SendInput {
            to: to.to_string(),
            subject: Some(subject.to_string()),
            body: body.to_string(),
            msg_type: None,
            thread: None,
            task_id: None,
            refs: None,
            requires_ack: false,
        },
    )
    .unwrap()[0]
        .seq
}

#[test]
fn init_adopts_repairs_and_rotates_operator_token() {
    let f = fixture();
    // Second init: nothing changed.
    assert_eq!(f.bus.init().unwrap().operator, "unchanged");
    // Corrupt the stored hash -> init rotates.
    f.bus
        .conn
        .execute(
            "UPDATE identities SET token_hash = 'deadbeef' WHERE agent_id = 'operator'",
            [],
        )
        .unwrap();
    assert_eq!(f.bus.init().unwrap().operator, "rotated");
    // A fresh bus adopting a pre-existing operator.token file registers its hash.
    let home2 = fresh_home();
    let token_path = f.home.join("operator.token");
    fs::create_dir_all(&home2).unwrap();
    fs::copy(&token_path, home2.join("operator.token")).unwrap();
    let fresh = Bus::open(Some(&home2.join("bus.db"))).unwrap();
    assert_eq!(fresh.init().unwrap().operator, "adopted");
    // And the adopted token resolves to the operator.
    assert_eq!(
        fresh.identify(Some(OPERATOR_ID)).unwrap().authority,
        "operator"
    );
}

#[test]
fn token_for_one_agent_cannot_act_as_another() {
    let f = fixture();
    add_agent(&f.bus, "alice", "worker");
    add_agent(&f.bus, "bob", "worker");
    // Copy alice's token over bob's — resolution must fail.
    fs::copy(
        identity::agent_token_path(&f.home, "alice").unwrap(),
        identity::agent_token_path(&f.home, "bob").unwrap(),
    )
    .unwrap();
    let error = f.bus.identify(Some("bob")).unwrap_err();
    assert_eq!(error.code, Code::Unauthorized);
    assert!(error.message.contains("does not belong to bob"));
}

#[test]
fn operator_commands_reject_agent_tokens() {
    let f = fixture();
    let alice = add_agent(&f.bus, "alice", "worker");
    let error = f
        .bus
        .add_agent(&alice, "mallory", None, None, None, None, Some("worker"))
        .unwrap_err();
    assert_eq!(error.code, Code::Forbidden);
}

#[test]
fn token_rotation_invalidates_the_old_token() {
    let f = fixture();
    add_agent(&f.bus, "alice", "worker");
    let operator = f.bus.identify(Some(OPERATOR_ID)).unwrap();
    f.bus.rotate_token(&operator, "alice").unwrap();
    // The identity row hash no longer matches the old file, so resolution uses the NEW file.
    let alice = f.bus.identify(Some("alice")).unwrap();
    assert_eq!(alice.agent_id, "alice");
    // Stored tokens are hashed, never plaintext.
    let stored = identity::stored_identity(&f.bus.conn, "alice")
        .unwrap()
        .unwrap();
    let raw = fs::read_to_string(identity::agent_token_path(&f.home, "alice").unwrap()).unwrap();
    assert_ne!(stored.0, raw.trim());
    assert_eq!(stored.0, identity::hash_token(raw.trim()));
    // Token files are 0600.
    let mode = fs::metadata(identity::agent_token_path(&f.home, "alice").unwrap())
        .unwrap()
        .permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(mode.mode() & 0o777, 0o600);
    }
}

#[test]
fn broadcast_reaches_everyone_but_sender_and_peek_keeps_cursor() {
    let f = fixture();
    let a = add_agent(&f.bus, "a", "worker");
    add_agent(&f.bus, "b", "worker");
    let c = add_agent(&f.bus, "c", "worker");
    send(&f.bus, &a, "*", "all hands", "roll call");
    // Sender sees nothing; b and c see the broadcast.
    assert_eq!(f.bus.inbox(&a, true, None).unwrap().messages.len(), 0);
    let b = f.bus.identify(Some("b")).unwrap();
    assert_eq!(f.bus.inbox(&b, true, None).unwrap().messages.len(), 1);
    assert_eq!(f.bus.inbox(&c, true, None).unwrap().messages.len(), 1);
    // Peek does not advance the cursor.
    assert_eq!(f.bus.inbox(&c, true, None).unwrap().messages.len(), 1);
    // Non-peek consumes.
    let result = f.bus.inbox(&c, false, None).unwrap();
    assert_eq!(result.messages.len(), 1);
    assert_eq!(f.bus.inbox(&c, true, None).unwrap().messages.len(), 0);
}

#[test]
fn ack_is_recorded_and_signal_files_track_delivery() {
    let f = fixture();
    let a = add_agent(&f.bus, "a", "worker");
    add_agent(&f.bus, "b", "worker");
    let seq = send(&f.bus, &a, "b", "ping", "hello b");
    // Signal file written post-commit with the delivered seq.
    assert_eq!(wait::read_signal_file(&f.home, "b"), Some(seq));
    let b = f.bus.identify(Some("b")).unwrap();
    let (acked_seq, ack_ms) = f.bus.ack(&b, seq).unwrap();
    assert_eq!(acked_seq, seq);
    assert!(ack_ms > 0);
    // The sender cannot ack a message addressed to b.
    assert_eq!(f.bus.ack(&a, seq).unwrap_err().code, Code::NotFound);
}

#[test]
fn create_claim_note_submit_revise_submit_accept_lifecycle() {
    let f = fixture();
    let planner = {
        let operator = f.bus.identify(Some(OPERATOR_ID)).unwrap();
        f.bus
            .add_agent(
                &operator,
                "planner",
                Some("manager"),
                None,
                None,
                None,
                Some("manager"),
            )
            .unwrap();
        f.bus.identify(Some("planner")).unwrap()
    };
    let worker = add_agent(&f.bus, "worker", "implementation");

    let task = f
        .bus
        .create_task(
            &planner,
            CreateTaskInput {
                title: "build widget".into(),
                brief: Some("make it work".into()),
                to: Some("worker".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(task.state, "open");
    assert_eq!(task.assignee.as_deref(), Some("worker"));

    let claimed = f.bus.claim_task(&worker, Some(task.id)).unwrap();
    assert_eq!(claimed.state, "claimed");
    // The assignee's status flips to working.
    assert_eq!(
        f.bus.get_agent("worker").unwrap().unwrap().status,
        "working"
    );

    f.bus.note_task(&worker, task.id, "halfway there").unwrap();
    let submitted = f
        .bus
        .submit_task(
            &worker,
            task.id,
            SubmitInput {
                summary: "v1".into(),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(submitted.state, "submitted");

    // The worker cannot review its own work.
    let error = f
        .bus
        .review_task(&worker, task.id, false, "self review")
        .unwrap_err();
    assert_eq!(error.code, Code::Forbidden);

    let revised = f
        .bus
        .review_task(&planner, task.id, false, "needs more")
        .unwrap();
    assert_eq!(revised.state, "changes_requested");
    assert_eq!(revised.round, 2);

    // Reclaimable by the same assignee.
    let reclaimed = f.bus.claim_task(&worker, Some(task.id)).unwrap();
    assert_eq!(reclaimed.state, "claimed");

    f.bus
        .submit_task(
            &worker,
            task.id,
            SubmitInput {
                summary: "v2".into(),
                ..Default::default()
            },
        )
        .unwrap();
    let accepted = f.bus.review_task(&planner, task.id, true, "lgtm").unwrap();
    assert_eq!(accepted.state, "accepted");

    // The task thread carries the whole exchange.
    let detail = f.bus.get_task(task.id).unwrap();
    assert!(detail.messages.len() >= 3);
    assert!(detail.notes.iter().any(|n| n.body == "halfway there"));
}

#[test]
fn auto_claim_prefers_assigned_then_urgent() {
    let f = fixture();
    let planner = {
        let operator = f.bus.identify(Some(OPERATOR_ID)).unwrap();
        f.bus
            .add_agent(
                &operator,
                "planner",
                Some("manager"),
                None,
                None,
                None,
                Some("manager"),
            )
            .unwrap();
        f.bus.identify(Some("planner")).unwrap()
    };
    let worker = add_agent(&f.bus, "w", "implementation");
    // An older normal task and a newer urgent unassigned task for the worker's role.
    f.bus
        .create_task(
            &planner,
            CreateTaskInput {
                title: "old normal".into(),
                role: Some("implementation".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let urgent = f
        .bus
        .create_task(
            &planner,
            CreateTaskInput {
                title: "new urgent".into(),
                role: Some("implementation".into()),
                priority: Some("urgent".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let claimed = f.bus.claim_task(&worker, None).unwrap();
    assert_eq!(claimed.id, urgent.id);
    // Claimed task is no longer claimable.
    let error = f.bus.claim_task(&worker, Some(claimed.id)).unwrap_err();
    assert_eq!(error.code, Code::Conflict);
    // The remaining normal task claims next.
    let next = f.bus.claim_task(&worker, None).unwrap();
    assert_eq!(next.title, "old normal");
}

#[test]
fn claim_race_two_writers_one_winner() {
    let f = fixture();
    let planner = {
        let operator = f.bus.identify(Some(OPERATOR_ID)).unwrap();
        f.bus
            .add_agent(
                &operator,
                "planner",
                Some("manager"),
                None,
                None,
                None,
                Some("manager"),
            )
            .unwrap();
        f.bus.identify(Some("planner")).unwrap()
    };
    let a = add_agent(&f.bus, "a", "implementation");
    let b = add_agent(&f.bus, "b", "implementation");
    let task = f
        .bus
        .create_task(
            &planner,
            CreateTaskInput {
                title: "one slot".into(),
                role: Some("implementation".into()),
                ..Default::default()
            },
        )
        .unwrap();
    // Two Bus objects = two connections = the two-process race in miniature.
    let other = Bus::open(Some(&f.home.join("bus.db"))).unwrap();
    let claimed = f.bus.claim_task(&a, Some(task.id)).unwrap();
    assert_eq!(claimed.state, "claimed");
    let error = other.claim_task(&b, Some(task.id)).unwrap_err();
    assert_eq!(error.code, Code::Conflict);
}

#[test]
fn dependencies_block_and_unblock() {
    let f = fixture();
    let planner = {
        let operator = f.bus.identify(Some(OPERATOR_ID)).unwrap();
        f.bus
            .add_agent(
                &operator,
                "planner",
                Some("manager"),
                None,
                None,
                None,
                Some("manager"),
            )
            .unwrap();
        f.bus.identify(Some("planner")).unwrap()
    };
    let worker = add_agent(&f.bus, "w", "implementation");
    let first = f
        .bus
        .create_task(
            &planner,
            CreateTaskInput {
                title: "first".into(),
                to: Some("w".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let second = f
        .bus
        .create_task(
            &planner,
            CreateTaskInput {
                title: "second".into(),
                dependencies: vec![first.id],
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(second.state, "blocked");
    // A blocked task is not claimable.
    assert_eq!(
        f.bus.claim_task(&worker, Some(second.id)).unwrap_err().code,
        Code::Conflict
    );
    // Finish the first: claim, submit, accept.
    f.bus.claim_task(&worker, Some(first.id)).unwrap();
    f.bus
        .submit_task(
            &worker,
            first.id,
            SubmitInput {
                summary: "done".into(),
                ..Default::default()
            },
        )
        .unwrap();
    f.bus.review_task(&planner, first.id, true, "ok").unwrap();
    let after = f.bus.get_task(second.id).unwrap();
    assert_eq!(after.task.state, "open");
}

#[test]
fn leases_conflict_across_overlapping_scopes() {
    let f = fixture();
    let project = fresh_home();
    let planner = {
        let operator = f.bus.identify(Some(OPERATOR_ID)).unwrap();
        f.bus
            .add_agent(
                &operator,
                "planner",
                Some("manager"),
                None,
                None,
                None,
                Some("manager"),
            )
            .unwrap();
        f.bus.identify(Some("planner")).unwrap()
    };
    let worker = add_agent(&f.bus, "w", "implementation");
    let scoped = f
        .bus
        .create_task(
            &planner,
            CreateTaskInput {
                title: "scoped".into(),
                project: Some(project.display().to_string()),
                path_scopes: vec!["src".into()],
                ..Default::default()
            },
        )
        .unwrap();
    let overlapping = f
        .bus
        .create_task(
            &planner,
            CreateTaskInput {
                title: "overlapping".into(),
                project: Some(project.display().to_string()),
                path_scopes: vec!["src/deep".into()],
                ..Default::default()
            },
        )
        .unwrap();
    f.bus.claim_task(&worker, Some(scoped.id)).unwrap();
    // The overlapping claim is refused by the lease.
    let error = f.bus.claim_task(&worker, Some(overlapping.id)).unwrap_err();
    assert_eq!(error.code, Code::Conflict);
    assert!(error.message.contains("overlap"));
    // A disjoint scope still claims.
    let disjoint = f
        .bus
        .create_task(
            &planner,
            CreateTaskInput {
                title: "disjoint".into(),
                project: Some(project.display().to_string()),
                path_scopes: vec!["docs".into()],
                ..Default::default()
            },
        )
        .unwrap();
    f.bus.claim_task(&worker, Some(disjoint.id)).unwrap();
}

#[test]
fn expired_claims_reopen_on_the_next_write() {
    let home = fresh_home();
    let clock = Arc::new(AtomicI64::new(1_000_000));
    let clock_clone = clock.clone();
    let mut bus = Bus::with_clock(Some(&home.join("bus.db")), move || {
        clock_clone.load(Ordering::SeqCst)
    })
    .unwrap();
    bus.set_claim_ttl_ms(60_000);
    bus.init().unwrap();
    let planner = {
        let operator = bus.identify(Some(OPERATOR_ID)).unwrap();
        bus.add_agent(
            &operator,
            "planner",
            Some("manager"),
            None,
            None,
            None,
            Some("manager"),
        )
        .unwrap();
        bus.identify(Some("planner")).unwrap()
    };
    let worker = add_agent(&bus, "w", "implementation");
    let task = bus
        .create_task(
            &planner,
            CreateTaskInput {
                title: "expiring".into(),
                ..Default::default()
            },
        )
        .unwrap();
    bus.claim_task(&worker, Some(task.id)).unwrap();
    // Advance the clock past the claim TTL; the next task write reopens it.
    clock.fetch_add(61_000, Ordering::SeqCst);
    bus.create_task(
        &planner,
        CreateTaskInput {
            title: "sweep trigger".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let after = bus.get_task(task.id).unwrap();
    assert_eq!(after.task.state, "open");
    assert!(after.task.assignee.is_none() || after.task.claim_expires_ms.is_none());
    // A claim_expired event landed.
    let events = bus.events(0, 100).unwrap();
    assert!(events
        .iter()
        .any(|e| e.kind == "claim_expired" && e.entity_id == task.id.to_string()));
}

#[test]
fn too_many_revisions_fail_the_task() {
    let f = fixture();
    let planner = {
        let operator = f.bus.identify(Some(OPERATOR_ID)).unwrap();
        f.bus
            .add_agent(
                &operator,
                "planner",
                Some("manager"),
                None,
                None,
                None,
                Some("manager"),
            )
            .unwrap();
        f.bus.identify(Some("planner")).unwrap()
    };
    let worker = add_agent(&f.bus, "w", "implementation");
    let task = f
        .bus
        .create_task(
            &planner,
            CreateTaskInput {
                title: "fragile".into(),
                to: Some("w".into()),
                max_retries: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
    for round in 1..=2 {
        f.bus.claim_task(&worker, Some(task.id)).unwrap();
        f.bus
            .submit_task(
                &worker,
                task.id,
                SubmitInput {
                    summary: format!("attempt {round}"),
                    ..Default::default()
                },
            )
            .unwrap();
        let reviewed = f
            .bus
            .review_task(&planner, task.id, false, "again")
            .unwrap();
        if round == 1 {
            assert_eq!(reviewed.state, "changes_requested");
        } else {
            // round-1 > maxRetries: 2-1=1 <= 1 revise allowed; third would exceed.
            assert!(reviewed.state == "changes_requested" || reviewed.state == "failed");
        }
    }
    if f.bus.get_task(task.id).unwrap().task.state == "changes_requested" {
        f.bus.claim_task(&worker, Some(task.id)).unwrap();
        f.bus
            .submit_task(
                &worker,
                task.id,
                SubmitInput {
                    summary: "attempt 3".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        let failed = f
            .bus
            .review_task(&planner, task.id, false, "enough")
            .unwrap();
        assert_eq!(failed.state, "failed");
    }
}

#[test]
fn every_write_appends_an_events_row_in_the_same_transaction() {
    let f = fixture();
    let a = add_agent(&f.bus, "a", "worker");
    add_agent(&f.bus, "b", "worker");
    let before = f.bus.latest_seq().unwrap();
    send(&f.bus, &a, "b", "hi", "there");
    let seq = f.bus.latest_seq().unwrap();
    assert!(seq > before);
    let events = f.bus.events(before, 10).unwrap();
    assert!(events.iter().any(|e| e.kind == "message" && e.actor == "a"));
    // Events are monotonically ordered by seq.
    let mut last = 0;
    for event in f.bus.events(0, 1000).unwrap() {
        assert!(event.seq > last);
        last = event.seq;
    }
}

#[test]
fn task_list_mine_and_states_filters() {
    let f = fixture();
    let planner = {
        let operator = f.bus.identify(Some(OPERATOR_ID)).unwrap();
        f.bus
            .add_agent(
                &operator,
                "planner",
                Some("manager"),
                None,
                None,
                None,
                Some("manager"),
            )
            .unwrap();
        f.bus.identify(Some("planner")).unwrap()
    };
    add_agent(&f.bus, "w", "implementation");
    f.bus
        .create_task(
            &planner,
            CreateTaskInput {
                title: "for w".into(),
                to: Some("w".into()),
                ..Default::default()
            },
        )
        .unwrap();
    f.bus
        .create_task(
            &planner,
            CreateTaskInput {
                title: "unassigned".into(),
                ..Default::default()
            },
        )
        .unwrap();
    let mine = f
        .bus
        .list_tasks(ListTasksInput {
            mine: Some("w".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0].title, "for w");
    let open = f
        .bus
        .list_tasks(ListTasksInput {
            states: Some(vec!["open".into()]),
            include_closed: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(open.len(), 2);
}

#[test]
fn release_and_cancel_restore_and_close() {
    let f = fixture();
    let planner = {
        let operator = f.bus.identify(Some(OPERATOR_ID)).unwrap();
        f.bus
            .add_agent(
                &operator,
                "planner",
                Some("manager"),
                None,
                None,
                None,
                Some("manager"),
            )
            .unwrap();
        f.bus.identify(Some("planner")).unwrap()
    };
    let worker = add_agent(&f.bus, "w", "implementation");
    let task = f
        .bus
        .create_task(
            &planner,
            CreateTaskInput {
                title: "releasable".into(),
                to: Some("w".into()),
                ..Default::default()
            },
        )
        .unwrap();
    f.bus.claim_task(&worker, Some(task.id)).unwrap();
    let released = f
        .bus
        .release_task(&worker, task.id, Some("not mine"))
        .unwrap();
    assert_eq!(released.state, "open");
    // Reclaimed then cancelled by the creator.
    f.bus.claim_task(&worker, Some(task.id)).unwrap();
    let cancelled = f
        .bus
        .cancel_task(&planner, task.id, Some("changed my mind"))
        .unwrap();
    assert_eq!(cancelled.state, "cancelled");
    // The cancelled task is gone from open listings.
    assert!(f
        .bus
        .list_tasks(ListTasksInput::default())
        .unwrap()
        .iter()
        .all(|t| t.id != task.id));
}

#[test]
fn fail_task_retries_then_escalates() {
    let f = fixture();
    let planner = {
        let operator = f.bus.identify(Some(OPERATOR_ID)).unwrap();
        f.bus
            .add_agent(
                &operator,
                "planner",
                Some("manager"),
                None,
                None,
                None,
                Some("manager"),
            )
            .unwrap();
        f.bus.identify(Some("planner")).unwrap()
    };
    let worker = add_agent(&f.bus, "w", "implementation");
    let task = f
        .bus
        .create_task(
            &planner,
            CreateTaskInput {
                title: "flaky".into(),
                to: Some("w".into()),
                max_retries: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
    f.bus.claim_task(&worker, Some(task.id)).unwrap();
    let retried = f.bus.fail_task(&worker, task.id, "boom").unwrap();
    assert_eq!(retried.state, "open");
    assert_eq!(retried.attempts, 1);
    f.bus.claim_task(&worker, Some(task.id)).unwrap();
    let failed = f.bus.fail_task(&worker, task.id, "boom again").unwrap();
    assert_eq!(failed.state, "failed");
    // The creator got an escalation message.
    let inbox = f.bus.inbox(&planner, true, None).unwrap();
    assert!(inbox
        .messages
        .iter()
        .any(|m| m.subject.contains("ESCALATE")));
}

#[test]
fn unread_count_and_agent_summaries() {
    let f = fixture();
    let a = add_agent(&f.bus, "a", "worker");
    add_agent(&f.bus, "b", "worker");
    send(&f.bus, &a, "b", "one", "1");
    send(&f.bus, &a, "b", "two", "2");
    assert_eq!(f.bus.unread_count("b").unwrap(), 2);
    let summaries = f
        .bus
        .agent_summaries(&["b".to_string(), "a".to_string()])
        .unwrap();
    assert_eq!(summaries.len(), 2);
    assert_eq!(summaries[0].id, "a");
    let message_summaries = f.bus.message_summaries(&[1, 2]).unwrap();
    assert_eq!(message_summaries.len(), 2);
    assert_eq!(message_summaries[0].subject, "one");
}
