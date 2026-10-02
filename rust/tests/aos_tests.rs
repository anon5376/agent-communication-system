//! `aos` screens and gate actions against a real bus seeded by `aos demo`.

use acs::aos::view::{Route, Target};
use acs::aos::{demo, paint, App, Key, Tier, DEFAULT_STALL_MIN};
use acs::bus::Bus;
use std::path::PathBuf;

fn demo_app(w: usize, h: usize) -> (App, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "aos-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let db = dir.join("bus.db");
    assert!(demo::seed(&db).unwrap());
    let bus = Bus::open(Some(&db)).unwrap();
    let mut app = App::new(bus, DEFAULT_STALL_MIN * 60_000).unwrap();
    app.ui.width = w;
    app.ui.height = h;
    (app, db)
}

fn text(app: &App) -> Vec<String> {
    app.screen().iter().map(|l| l.text()).collect()
}

fn keys(app: &mut App, ks: &[Key]) {
    for k in ks {
        app.key(*k).unwrap();
    }
}

fn typed(app: &mut App, s: &str) {
    for c in s.chars() {
        app.key(Key::Char(c)).unwrap();
    }
}

const ROUTES: &[Route] = &[
    Route::Swarm,
    Route::Goal,
    Route::Evidence,
    Route::Memory,
    Route::Retro,
    Route::Providers,
    Route::Help,
    Route::Home,
    Route::Gate,
];

#[test]
fn every_screen_fits_its_terminal_exactly() {
    for (w, h) in [
        (60, 20),
        (80, 24),
        (100, 30),
        (120, 36),
        (160, 48),
        (200, 60),
    ] {
        let (mut app, _) = demo_app(w, h);
        for r in ROUTES {
            app.ui.route = *r;
            let lines = text(&app);
            assert_eq!(lines.len(), h, "{r:?} at {w}x{h} has {} lines", lines.len());
            for l in &lines {
                assert_eq!(l.chars().count(), w, "{r:?} at {w}x{h}: {l:?}");
                assert!(l.is_ascii(), "{r:?} at {w}x{h} is not ASCII: {l:?}");
            }
        }
        app.ui.route = Route::Inspect;
        app.ui.inspect = Some(Target::Agent("impl-b".into()));
        assert_eq!(text(&app).len(), h);
        app.ui.inspect = Some(Target::Task(4));
        assert!(text(&app).iter().all(|l| l.chars().count() == w));
    }
}

#[test]
fn first_two_lines_give_scope_and_state() {
    let (app, _) = demo_app(80, 24);
    let lines = text(&app);
    assert!(lines[0].starts_with("AOS > <  "));
    assert!(lines[0].contains("? GATE"), "{}", lines[0]);
    assert!(lines[0].contains("live #"));
    let all = lines.join("\n");
    // The spine carries marker and word for every state the demo creates.
    for s in [
        "* RUNNING",
        "! BLOCKED",
        ". WAITING",
        "~ DISCONNECTED",
        "? GATE",
    ] {
        assert!(all.contains(s), "missing {s}:\n{all}");
    }
    // ACS records no cost; the screen says so instead of showing zero.
    assert!(all.contains("cost        ? UNKNOWN"));
}

#[test]
fn accept_needs_a_reason_and_writes_one_event() {
    let (mut app, db) = demo_app(80, 24);
    let before = app.frame.seq;
    keys(&mut app, &[Key::Char('1')]);
    assert!(app.ui.pending.is_some());
    keys(&mut app, &[Key::Enter]);
    assert!(app.ui.pending.is_some(), "an empty reason must not commit");
    assert_eq!(app.frame.seq, before);
    typed(&mut app, "cap and interop test both pass");
    keys(&mut app, &[Key::Enter]);
    assert!(app.ui.pending.is_none());
    let bus = Bus::open(Some(&db)).unwrap();
    let t = bus.get_task(3).unwrap().task;
    assert_eq!(t.state, "accepted");
    assert_eq!(t.review.unwrap().feedback, "cap and interop test both pass");
    assert!(app.frame.seq > before);
    let status = text(&app).last().unwrap().clone();
    assert!(
        status.starts_with("[ ok ] #3 accepted / event #"),
        "{status}"
    );
    // #6 depended on #3 and is now open.
    assert_eq!(bus.get_task(6).unwrap().task.state, "open");
}

#[test]
fn hold_writes_nothing_and_moves_to_the_next_gate() {
    let (mut app, _) = demo_app(80, 24);
    let before = app.frame.seq;
    keys(&mut app, &[Key::Char('3')]);
    assert_eq!(app.frame.seq, before);
    let all = text(&app).join("\n");
    assert!(all.contains("#7 hard usd cap per run"), "{all}");
    assert!(all.contains("x 1 failed"), "{all}");
}

#[test]
fn cancel_on_a_stalled_claim_needs_the_word() {
    let (mut app, db) = demo_app(80, 24);
    keys(&mut app, &[Key::Char('3'), Key::Char('3')]); // hold #3, hold #7: now the stalled #4
    let all = text(&app).join("\n");
    assert!(all.contains("! BLOCKED   #4 loop guard"), "{all}");
    keys(&mut app, &[Key::Char('3')]);
    typed(&mut app, "cancle");
    keys(&mut app, &[Key::Enter]);
    assert_eq!(
        Bus::open(Some(&db))
            .unwrap()
            .get_task(4)
            .unwrap()
            .task
            .state,
        "claimed"
    );
    keys(&mut app, &[Key::Esc]);
    assert!(app.ui.pending.is_none());
    keys(&mut app, &[Key::Char('3')]);
    typed(&mut app, "cancel");
    keys(&mut app, &[Key::Enter]);
    assert_eq!(
        Bus::open(Some(&db))
            .unwrap()
            .get_task(4)
            .unwrap()
            .task
            .state,
        "cancelled"
    );
}

#[test]
fn requeue_returns_a_stalled_claim_to_the_pool() {
    let (mut app, db) = demo_app(80, 24);
    keys(
        &mut app,
        &[Key::Char('3'), Key::Char('3'), Key::Char('1'), Key::Enter],
    );
    let t = Bus::open(Some(&db)).unwrap().get_task(4).unwrap().task;
    assert_eq!(t.state, "open");
    assert_eq!(t.assignee, None);
}

#[test]
fn enter_on_a_gate_opens_it_and_never_approves() {
    let (mut app, db) = demo_app(80, 24);
    keys(&mut app, &[Key::Tab, Key::Enter]);
    assert_eq!(app.ui.route, Route::Gate);
    keys(&mut app, &[Key::Enter, Key::Enter]);
    assert_eq!(
        Bus::open(Some(&db))
            .unwrap()
            .get_task(3)
            .unwrap()
            .task
            .state,
        "submitted"
    );
    keys(&mut app, &[Key::Esc]);
    assert_eq!(app.ui.route, Route::Swarm);
}

#[test]
fn inspect_an_agent_and_step_through_the_spine() {
    let (mut app, _) = demo_app(80, 24);
    keys(&mut app, &[Key::Char('j'), Key::Enter]);
    assert_eq!(app.ui.route, Route::Inspect);
    let all = text(&app).join("\n");
    assert!(all.contains("impl-b worker"), "{all}");
    assert!(all.contains("task_claimed"), "{all}");
    keys(&mut app, &[Key::Char('j')]);
    assert_eq!(app.ui.inspect, Some(Target::Agent("impl-a".into())));
}

#[test]
fn filter_narrows_the_spine_without_writing() {
    let (mut app, _) = demo_app(80, 24);
    let before = app.frame.seq;
    keys(&mut app, &[Key::Char('/')]);
    typed(&mut app, "gemini");
    keys(&mut app, &[Key::Enter]);
    let all = text(&app).join("\n");
    assert!(all.contains("rev-1"));
    assert!(!all.contains("impl-b  !"));
    assert_eq!(app.frame.seq, before);
}

#[test]
fn narrow_terminal_shows_one_object_with_state_first() {
    let (mut app, _) = demo_app(60, 20);
    let lines = text(&app);
    assert!(lines[1].starts_with("? GATE      #3"), "{}", lines[1]);
    assert!(lines[2].starts_with("next"));
    keys(&mut app, &[Key::Char('n'), Key::Char('n'), Key::Char('n')]);
    let lines = text(&app);
    assert!(lines[1].starts_with("* RUNNING   lead"), "{}", lines[1]);
}

#[test]
fn home_commands_write_through_the_bus() {
    let (mut app, db) = demo_app(80, 24);
    keys(&mut app, &[Key::Char('c')]);
    typed(&mut app, "task add write the budget docs");
    keys(&mut app, &[Key::Enter]);
    let all = text(&app).join("\n");
    assert!(all.contains("[ ok ] task #8 created"), "{all}");
    assert_eq!(
        Bus::open(Some(&db))
            .unwrap()
            .get_task(8)
            .unwrap()
            .task
            .title,
        "write the budget docs"
    );
    typed(&mut app, "frobnicate");
    keys(&mut app, &[Key::Enter]);
    assert!(text(&app)
        .join("\n")
        .contains("? UNKNOWN / no command \"frobnicate\""));
}

#[test]
fn quitting_asks_first() {
    let (mut app, _) = demo_app(80, 24);
    keys(&mut app, &[Key::Char('q'), Key::Char('n')]);
    assert!(!app.quit);
    keys(&mut app, &[Key::CtrlC, Key::CtrlC]);
    assert!(app.quit);
}

#[test]
fn no_color_keeps_every_state_in_words() {
    let (mut app, _) = demo_app(80, 24);
    keys(&mut app, &[Key::Char('j')]);
    let painted = paint(&app.screen(), Tier::Mono);
    let mut selected = 0;
    for line in &painted {
        assert!(line.style.fg.is_none() && line.style.bg.is_none());
        for span in &line.spans {
            assert_eq!(
                span.style,
                ratatui::style::Style::default(),
                "mono must not style {:?}",
                span.content
            );
        }
        let t: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        if t.starts_with('>') {
            selected += 1;
        }
    }
    assert_eq!(selected, 1, "the selected row is marked with >");
}
