//! The aos crew: first run, goals typed as sentences, missions, and the trust
//! check before agents start. No agent CLI is run here.

use acs::aos::crew::{self, Found, Paths, CLIS};
use acs::aos::view::{PendingKind, Route};
use acs::aos::{demo, ensure_operator, App, Key, DEFAULT_STALL_MIN};
use acs::bus::Bus;
use std::path::PathBuf;

fn temp_db(tag: &str) -> PathBuf {
    std::env::temp_dir()
        .join(format!(
            "aos-crew-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
        .join("bus.db")
}

fn app_on(db: &std::path::Path) -> App {
    let bus = Bus::open(Some(db)).unwrap();
    assert!(ensure_operator(&bus).unwrap());
    let mut app = App::new(bus, DEFAULT_STALL_MIN * 60_000).unwrap();
    app.ui.width = 80;
    app.ui.height = 24;
    app
}

fn text(app: &App) -> String {
    app.screen()
        .iter()
        .map(|l| l.text())
        .collect::<Vec<_>>()
        .join("\n")
}

fn only(ids: &[&str]) -> Vec<Found> {
    CLIS.iter()
        .map(|cli| Found {
            cli,
            path: ids.contains(&cli.id).then(|| PathBuf::from("/bin/true")),
            version: None,
        })
        .collect()
}

#[test]
fn first_run_offers_the_welcome_screen() {
    let db = temp_db("welcome");
    let mut app = app_on(&db);
    assert!(app.first_run());
    app.found = only(&["claude", "codex"]);
    app.refresh().unwrap();
    app.ui.route = Route::Welcome;
    let all = text(&app);
    assert!(all.contains("welcome to aos"), "{all}");
    assert!(all.contains("+ claude"), "{all}");
    assert!(all.contains("builder   codex"), "{all}");
    assert!(all.contains("reviewer  claude"), "{all}");
    assert!(all.contains("can run commands and edit files"), "{all}");
}

#[test]
fn a_sentence_becomes_a_goal_after_one_enter() {
    let db = temp_db("sentence");
    assert!(demo::seed(&db).unwrap());
    let mut app = app_on(&db);
    let unread = app.frame.operator_unread;
    app.ui.route = Route::Home;
    // Starts with a command word, but it is a sentence: it must not mark mail read.
    app.command_line("read the budget notes and summarize them")
        .unwrap();
    assert_eq!(
        app.ui.pending.as_ref().map(|p| p.kind),
        Some(PendingKind::Goal)
    );
    assert_eq!(app.frame.operator_unread, unread);
    app.key(Key::Enter).unwrap();
    let bus = Bus::open(Some(&db)).unwrap();
    let t = bus.get_task(8).unwrap().task;
    assert_eq!(t.title, "read the budget notes and summarize them");
    assert!(t
        .brief
        .contains("The operator's goal: read the budget notes"));
}

#[test]
fn a_mission_word_expands_its_template() {
    let db = temp_db("mission");
    assert!(demo::seed(&db).unwrap());
    let mut app = app_on(&db);
    app.ui.route = Route::Home;
    app.command_line("fix the flaky budget test").unwrap();
    let t = Bus::open(Some(&db)).unwrap().get_task(8).unwrap().task;
    assert_eq!(t.title, "fix the flaky budget test");
    assert!(t.brief.contains("Reproduce it first"), "{}", t.brief);
    assert!(t.acceptance.contains("reproduction"), "{}", t.acceptance);
}

#[test]
fn setup_writes_the_crew_and_starting_asks_to_trust_the_folder() {
    let db = temp_db("trust");
    let mut app = app_on(&db);
    let paths = Paths::for_db(&db);
    let report = crew::setup(&app.bus, &paths, &only(&["claude"]), false).unwrap();
    assert!(report.wrote_crew);
    assert_eq!(report.added, ["lead", "builder", "reviewer"]);
    assert!(paths.roles().join("lead.md").exists());
    assert!(paths.missions().join("fix.md").exists());
    app.refresh().unwrap();
    assert!(!app.first_run());
    app.ui.route = Route::Home;
    app.command_line("start").unwrap();
    assert_eq!(
        app.ui.pending.as_ref().map(|p| p.kind),
        Some(PendingKind::Trust)
    );
    app.key(Key::Esc).unwrap();
    assert_eq!(app.frame.crew.running(), 0);
    assert!(crew::crew_workdir(&paths).is_none(), "nothing started");
    // Edits survive: setup never overwrites a preset or the crew file.
    std::fs::write(paths.roles().join("lead.md"), "my own lead prompt").unwrap();
    let again = crew::setup(&app.bus, &paths, &only(&["claude"]), false).unwrap();
    assert!(!again.wrote_crew);
    assert_eq!(
        std::fs::read_to_string(paths.roles().join("lead.md")).unwrap(),
        "my own lead prompt"
    );
}

#[test]
fn agents_never_start_in_the_home_folder() {
    let home = dirs::home_dir().unwrap();
    assert!(crew::unsafe_workdir(&home).is_some());
    assert!(crew::unsafe_workdir(std::path::Path::new("/")).is_some());
    assert!(crew::unsafe_workdir(&home.join("project")).is_none());
}

#[test]
fn the_role_prompt_goes_before_the_brief() {
    let out = acs::supervisor::with_role_prompt(Some("be careful".into()), "do it".into());
    assert!(out.starts_with("=== how you work (your role prompt) ===\nbe careful"));
    assert!(out.ends_with("do it"));
    assert_eq!(
        acs::supervisor::with_role_prompt(None, "do it".into()),
        "do it"
    );
}

#[test]
fn tab_completes_and_history_lists_past_goals() {
    let db = temp_db("history");
    assert!(demo::seed(&db).unwrap());
    let mut app = app_on(&db);
    app.ui.route = Route::Home;
    app.command_line("fix the flaky budget test").unwrap();
    for c in "/his".chars() {
        app.key(Key::Char(c)).unwrap();
    }
    let all = text(&app);
    assert!(all.contains("tab history"), "{all}");
    app.key(Key::Tab).unwrap();
    assert_eq!(app.ui.prompt, "history ");
    app.key(Key::Enter).unwrap();
    let all = text(&app);
    assert!(all.contains("#8    open"), "{all}");
    assert!(all.contains("fix the flaky budget test"), "{all}");
}

#[test]
fn up_brings_back_earlier_lines_after_a_restart() {
    let db = temp_db("recall");
    assert!(demo::seed(&db).unwrap());
    let mut app = app_on(&db);
    app.ui.route = Route::Home;
    for line in ["status", "missions"] {
        for c in line.chars() {
            app.key(Key::Char(c)).unwrap();
        }
        app.key(Key::Enter).unwrap();
    }
    let mut app = app_on(&db);
    app.ui.route = Route::Home;
    app.key(Key::Up).unwrap();
    assert_eq!(app.ui.prompt, "missions");
    app.key(Key::Up).unwrap();
    assert_eq!(app.ui.prompt, "status");
    app.key(Key::Down).unwrap();
    app.key(Key::Down).unwrap();
    assert_eq!(app.ui.prompt, "");
}
