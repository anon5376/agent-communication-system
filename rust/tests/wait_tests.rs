//! Waiter + watcher tests — porting tests/core-changes.test.ts and tests/wait-notify.test.ts.

use acs::bus::{Bus, CreateTaskInput, SendInput};
use acs::types::OPERATOR_ID;
use acs::wait::{self, wait_for_mail};
use acs::watcher::{ChangeWatcher, ChangeWatcherOptions};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn fresh_home() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "acs-rust-wait-{}-{}",
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

fn add_worker(bus: &Bus, id: &str) -> acs::identity::Identity {
    let operator = bus.identify(Some(OPERATOR_ID)).unwrap();
    bus.add_agent(
        &operator,
        id,
        Some("implementation"),
        None,
        None,
        None,
        Some("worker"),
    )
    .unwrap();
    bus.identify(Some(id)).unwrap()
}

fn send(bus: &Bus, actor: &acs::identity::Identity, to: &str, subject: &str, body: &str) {
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
    .unwrap();
}

#[test]
fn watcher_fires_within_200ms_of_a_write_from_another_connection() {
    let f = fixture();
    let watcher =
        ChangeWatcher::new(&f.home.join("bus.db"), ChangeWatcherOptions::default()).unwrap();
    let since = f.bus.latest_seq().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let started = Instant::now();
    std::thread::sleep(Duration::from_millis(50));
    send(
        &f.bus,
        &f.bus.identify(Some(OPERATOR_ID)).unwrap(),
        "operator",
        "poke",
        "now",
    );
    let next = watcher.next(since, Duration::from_secs(2), &stop).unwrap();
    assert!(next > since);
    assert_eq!(next, f.bus.latest_seq().unwrap());
    assert!(
        started.elapsed() < Duration::from_millis(200),
        "watcher took {:?}",
        started.elapsed()
    );
}

#[test]
fn watcher_times_out_cleanly() {
    let f = fixture();
    let watcher =
        ChangeWatcher::new(&f.home.join("bus.db"), ChangeWatcherOptions::default()).unwrap();
    let since = f.bus.latest_seq().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let started = Instant::now();
    let next = watcher
        .next(since, Duration::from_millis(120), &stop)
        .unwrap();
    assert_eq!(next, since);
    assert!(started.elapsed() >= Duration::from_millis(100));
}

#[test]
fn wait_for_mail_wakes_on_send_within_a_second() {
    let f = fixture();
    let worker = add_worker(&f.bus, "w");
    let stop = Arc::new(AtomicBool::new(false));
    let started = Instant::now();
    send(
        &f.bus,
        &f.bus.identify(Some(OPERATOR_ID)).unwrap(),
        "w",
        "incoming",
        "hi",
    );
    let result = wait_for_mail(&f.bus, &worker, Duration::from_secs(2), &stop).unwrap();
    assert_eq!(result.status, "mail");
    assert_eq!(result.messages.len(), 1);
    assert!(
        started.elapsed() < Duration::from_millis(1000),
        "wait took {:?}",
        started.elapsed()
    );
}

#[test]
fn wait_for_mail_wakes_on_a_claimable_task() {
    let f = fixture();
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
    let worker = add_worker(&f.bus, "w");
    let stop = Arc::new(AtomicBool::new(false));
    // A task created BEFORE the wait began does not wake the waiter; the
    // task_created event has to land after `since`. Fire one mid-wait from a
    // second connection, like the CLI test does.
    let db_path = f.home.join("bus.db");
    let creator = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(60));
        let other = Bus::open(Some(&db_path)).unwrap();
        let planner = other.identify(Some("planner")).unwrap();
        other
            .create_task(
                &planner,
                CreateTaskInput {
                    title: "wake me".into(),
                    role: Some("implementation".into()),
                    ..Default::default()
                },
            )
            .unwrap()
            .id
    });
    let result = wait_for_mail(&f.bus, &worker, Duration::from_secs(3), &stop).unwrap();
    let task_id = creator.join().unwrap();
    assert_eq!(result.status, "task");
    assert!(result
        .events
        .iter()
        .any(|e| e.kind == "task_created" && e.entity_id == task_id.to_string()));
}

#[test]
fn wait_for_mail_times_out_with_status_none() {
    let f = fixture();
    let worker = add_worker(&f.bus, "w");
    let stop = Arc::new(AtomicBool::new(false));
    let result = wait_for_mail(&f.bus, &worker, Duration::from_millis(200), &stop).unwrap();
    assert_eq!(result.status, "none");
    assert_eq!(f.bus.get_agent("w").unwrap().unwrap().status, "idle");
}

#[test]
fn signal_file_alone_updates_on_every_delivery() {
    let f = fixture();
    let operator = f.bus.identify(Some(OPERATOR_ID)).unwrap();
    add_worker(&f.bus, "w");
    let first = f
        .bus
        .send(
            &operator,
            SendInput {
                to: "w".into(),
                subject: Some("a".into()),
                body: "1".into(),
                msg_type: None,
                thread: None,
                task_id: None,
                refs: None,
                requires_ack: false,
            },
        )
        .unwrap()[0]
        .seq;
    assert_eq!(wait::read_signal_file(&f.home, "w"), Some(first));
    let second = f
        .bus
        .send(
            &operator,
            SendInput {
                to: "w".into(),
                subject: Some("b".into()),
                body: "2".into(),
                msg_type: None,
                thread: None,
                task_id: None,
                refs: None,
                requires_ack: false,
            },
        )
        .unwrap()[0]
        .seq;
    assert_eq!(wait::read_signal_file(&f.home, "w"), Some(second));
}
