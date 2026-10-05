//! What needs the operator, in order, with the reason, the evidence and the
//! next command; and the first run: the simulated demo and the crew's
//! independent reviewer. No agent CLI is run here.

use acs::aos::crew::{self, MemberInfo, Paths};
use acs::aos::frame::{Frame, GateKind};
use acs::aos::view::Route;
use acs::aos::{demo, ensure_operator, App, Key, DEFAULT_STALL_MIN};
use acs::bus::{Bus, CreateTaskInput, SubmitInput};
use acs::types::OPERATOR_ID;
use std::path::PathBuf;

const STALL_MS: i64 = DEFAULT_STALL_MIN * 60_000;

fn temp_db(tag: &str) -> PathBuf {
    std::env::temp_dir()
        .join(format!(
            "aos-op-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
        .join("bus.db")
}

fn text(app: &App) -> String {
    app.screen()
        .iter()
        .map(|l| l.text())
        .collect::<Vec<_>>()
        .join("\n")
}

fn task(title: &str, to: &str) -> CreateTaskInput {
    CreateTaskInput {
        title: title.into(),
        to: Some(to.into()),
        ..Default::default()
    }
}

/// A bus with one of each thing that needs the operator, created out of order.
fn troubled_bus(db: &std::path::Path) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let early = Bus::with_clock(Some(db), move || now - 50 * 60_000).unwrap();
    early.init().unwrap();
    let op = early.identify(Some(OPERATOR_ID)).unwrap();
    for id in ["slow", "quitter", "napper", "doer"] {
        early
            .add_agent(&op, id, Some("worker"), None, None, None, Some("worker"))
            .unwrap();
    }
    // #1 the goal everything sits under.
    let goal = early
        .create_task(
            &op,
            CreateTaskInput {
                title: "the goal".into(),
                ..Default::default()
            },
        )
        .unwrap();
    // #2 claimed fifty minutes ago and never touched again: stalled.
    let stuck = early
        .create_task(
            &op,
            CreateTaskInput {
                parent_id: Some(goal.id),
                ..task("quiet work", "slow")
            },
        )
        .unwrap();
    let slow = early.identify(Some("slow")).unwrap();
    early.claim_task(&slow, Some(stuck.id)).unwrap();
    early
        .note_task(&slow, stuck.id, "looking at the parser")
        .unwrap();
    drop(early);

    let bus = Bus::open(Some(db)).unwrap();
    let op = bus.identify(Some(OPERATOR_ID)).unwrap();
    // #3 failed past its retries, under the open goal.
    let doomed = bus
        .create_task(
            &op,
            CreateTaskInput {
                parent_id: Some(goal.id),
                max_retries: Some(0),
                ..task("flaky step", "quitter")
            },
        )
        .unwrap();
    let quitter = bus.identify(Some("quitter")).unwrap();
    bus.claim_task(&quitter, Some(doomed.id)).unwrap();
    bus.fail_task(&quitter, doomed.id, "the build would not start")
        .unwrap();
    // #5 waits on #4, which gets cancelled.
    let dep = bus
        .create_task(
            &op,
            CreateTaskInput {
                parent_id: Some(goal.id),
                ..task("prerequisite", "doer")
            },
        )
        .unwrap();
    let waiting = bus
        .create_task(
            &op,
            CreateTaskInput {
                parent_id: Some(goal.id),
                dependencies: vec![dep.id],
                ..task("after the prerequisite", "doer")
            },
        )
        .unwrap();
    bus.cancel_task(&op, dep.id, Some("not needed")).unwrap();
    assert_eq!(bus.get_task(waiting.id).unwrap().task.state, "blocked");
    // #6 open work for an agent that is paused.
    bus.create_task(
        &op,
        CreateTaskInput {
            parent_id: Some(goal.id),
            ..task("napper's job", "napper")
        },
    )
    .unwrap();
    bus.pause_agent(&op, "napper", Some("budget used up"))
        .unwrap();
    // #7 a result for the operator to review, created last.
    let done = bus
        .create_task(
            &op,
            CreateTaskInput {
                parent_id: Some(goal.id),
                reviewer: Some(OPERATOR_ID.into()),
                ..task("finished piece", "doer")
            },
        )
        .unwrap();
    let doer = bus.identify(Some("doer")).unwrap();
    bus.claim_task(&doer, Some(done.id)).unwrap();
    bus.submit_task(
        &doer,
        done.id,
        SubmitInput {
            summary: "done".into(),
            ..Default::default()
        },
    )
    .unwrap();
}

#[test]
fn needs_you_comes_in_order_with_reason_evidence_and_next() {
    let db = temp_db("order");
    troubled_bus(&db);
    let bus = Bus::open(Some(&db)).unwrap();
    let f = Frame::load(&bus, STALL_MS).unwrap();
    let got: Vec<(GateKind, i64)> = f.gates.iter().map(|g| (g.kind, g.task.id)).collect();
    assert_eq!(
        got,
        [
            (GateKind::Review, 7),
            (GateKind::Failed, 3),
            (GateKind::Blocker, 6),
            (GateKind::Blocker, 5),
            (GateKind::Stalled, 2),
        ],
        "{:#?}",
        f.gates
            .iter()
            .map(|g| (&g.reason, &g.next))
            .collect::<Vec<_>>()
    );
    let g = |id: i64| f.gates.iter().find(|g| g.task.id == id).unwrap();
    assert_eq!(g(3).reason, "failed: retry limit exceeded");
    assert_eq!(g(3).next, "task add flaky step --to quitter --under 1");
    assert_eq!(g(6).reason, "napper is paused, so #6 cannot move");
    assert!(
        g(6).evidence.contains("budget used up"),
        "{}",
        g(6).evidence
    );
    assert_eq!(g(6).next, "resume napper");
    assert_eq!(g(5).reason, "waits on #4, which was cancelled");
    assert_eq!(g(5).next, "cancel 5");
    assert_eq!(
        g(2).reason,
        "slow has not checked in for 50m while holding it"
    );
    assert!(
        g(2).evidence.contains("looking at the parser"),
        "{}",
        g(2).evidence
    );
    assert!(g(2).next.starts_with("requeue 2"), "{}", g(2).next);
}

#[test]
fn a_stopped_crew_member_is_a_blocker_not_a_stall() {
    let db = temp_db("stopped");
    troubled_bus(&db);
    let bus = Bus::open(Some(&db)).unwrap();
    let mut f = Frame::load(&bus, STALL_MS).unwrap();
    f.crew.configured = true;
    f.crew.members = vec![MemberInfo {
        id: "slow".into(),
        pid: None,
        last_words: Some("claude: not logged in".into()),
        ..Default::default()
    }];
    f.add_crew_blockers();
    let g = f.gates.iter().find(|g| g.task.id == 2).unwrap();
    assert_eq!(g.kind, GateKind::Blocker);
    assert_eq!(g.reason, "slow is stopped, so #2 cannot move");
    assert!(g.evidence.contains("not logged in"), "{}", g.evidence);
    assert_eq!(g.next, "start slow");
    assert_eq!(f.count(GateKind::Stalled), 0);
}

#[test]
fn the_swarm_says_what_needs_you_and_one_types_the_fix() {
    let db = temp_db("swarm");
    troubled_bus(&db);
    let bus = Bus::open(Some(&db)).unwrap();
    assert!(ensure_operator(&bus).unwrap());
    let mut app = App::new(bus, STALL_MS).unwrap();
    app.ui.width = 80;
    app.ui.height = 24;
    app.refresh().unwrap();
    let all = text(&app);
    assert!(
        all.contains(
            "needs you   1 to review, 1 failed, 2 blocked, 1 stalled / 0 working, 2 queued"
        ),
        "{all}"
    );
    // Hold the review, land on the failure.
    app.key(Key::Char('3')).unwrap();
    let all = text(&app);
    assert!(
        all.contains("why         failed: retry limit exceeded"),
        "{all}"
    );
    assert!(
        all.contains("next        task add flaky step --to quitter --under 1"),
        "{all}"
    );
    app.key(Key::Char('1')).unwrap();
    assert_eq!(app.ui.route, Route::Home);
    assert_eq!(app.ui.prompt, "task add flaky step --to quitter --under 1");
}

#[test]
fn the_demo_is_marked_simulated_and_starts_nothing() {
    let db = temp_db("demo");
    assert!(demo::seed(&db).unwrap());
    let bus = Bus::open(Some(&db)).unwrap();
    assert!(ensure_operator(&bus).unwrap());
    let mut app = App::new(bus, STALL_MS).unwrap();
    app.ui.width = 80;
    app.ui.height = 24;
    app.refresh().unwrap();
    assert!(text(&app)
        .lines()
        .next()
        .unwrap()
        .contains("SIMULATED demo"));
    let paths = Paths::for_db(&db);
    let started = crew::start(&db, &paths, &["lead".to_string()], &std::env::temp_dir());
    assert!(
        started[0].1.is_err(),
        "a sample bus must never start an agent"
    );
    assert!(crew::setup(&app.bus, &paths, &[], true).is_err());
    app.ui.route = Route::Home;
    app.command_line("start").unwrap();
    assert!(text(&app).contains("simulated sample"), "{}", text(&app));
    assert!(app.ui.pending.is_none(), "no trust prompt on a sample bus");
}

#[test]
fn a_redone_failure_stops_needing_you() {
    let db = temp_db("redone");
    troubled_bus(&db);
    let bus = Bus::open(Some(&db)).unwrap();
    let op = bus.identify(Some(OPERATOR_ID)).unwrap();
    bus.create_task(
        &op,
        CreateTaskInput {
            parent_id: Some(1),
            ..task("flaky step", "quitter")
        },
    )
    .unwrap();
    let f = Frame::load(&bus, STALL_MS).unwrap();
    assert_eq!(f.count(GateKind::Failed), 0);
}

#[test]
fn the_demo_never_marks_a_bus_that_was_already_in_use() {
    let db = temp_db("real");
    troubled_bus(&db);
    assert!(!demo::seed(&db).unwrap());
    let home = db.parent().unwrap();
    assert!(!demo::is_simulated(home));
    // A bus with tasks but no agents besides you is in use too.
    let bare = temp_db("bare");
    let bus = Bus::open(Some(&bare)).unwrap();
    bus.init().unwrap();
    let op = bus.identify(Some(OPERATOR_ID)).unwrap();
    bus.create_task(
        &op,
        CreateTaskInput {
            title: "mine".into(),
            ..Default::default()
        },
    )
    .unwrap();
    drop(bus);
    assert!(!demo::seed(&bare).unwrap());
    assert!(!demo::is_simulated(bare.parent().unwrap()));
}
