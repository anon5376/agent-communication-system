//! aos watch and autostart: a supervisor that died without being stopped is
//! started again; one stopped on purpose is left alone. Only the stand-in CLI runs.

use acs::aos::crew::{self, Paths};
use acs::aos::ensure_operator;
use acs::aos::keeper::{self, Watcher, MAX_RESTARTS};
use acs::bus::Bus;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn fresh(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "aos-keeper-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// A bus with a one-agent crew on the stand-in CLI, working in a trusted folder.
fn crew_on(tag: &str) -> (PathBuf, Paths, PathBuf) {
    let home = fresh(tag);
    let work = fresh(&format!("{tag}-work"));
    let db = home.join("bus.db");
    let bus = Bus::open(Some(&db)).unwrap();
    ensure_operator(&bus).unwrap();
    let paths = Paths::for_db(&db);
    fs::create_dir_all(&paths.dir).unwrap();
    let caps = serde_json::json!({
        "coding": 0.5, "reasoning": 0.5, "planning": 0.5, "debugging": 0.5,
        "research": 0.5, "toolUse": 0.5, "speed": 0.5, "tokenEfficiency": 0.5,
        "reliability": 0.5, "autonomy": 0.5, "contextTokens": 1000,
        "costClass": "local", "source": "heuristic-default",
    });
    let config = serde_json::json!({
        "version": 1, "capabilityNotice": "",
        "providers": { "fake-provider": { "id": "fake-provider", "enabled": true, "subscriptionBacked": false } },
        "harnesses": { "fake-harness": {
            "id": "fake-harness", "adapter": "fake", "command": "fake",
            "providers": ["fake-provider"], "enabled": true,
            "features": { "mcp": false, "resume": true } } },
        "models": { "fake-model": {
            "id": "fake-model", "provider": "fake-provider", "harness": "fake-harness",
            "family": "fake", "enabled": true, "capabilities": caps } },
        "agents": { "w1": {
            "id": "w1", "model": "fake-model", "role": "worker", "authority": "worker",
            "description": "", "enabled": true, "permissions": { "maxDelegationDepth": 0 },
            "harnessOptions": {} } },
        "roles": { "worker": { "id": "worker", "description": "" } },
        "routing": {}, "constraints": {},
    });
    fs::write(paths.crew(), serde_json::to_string_pretty(&config).unwrap()).unwrap();
    let loaded = crew::load_crew(&paths).unwrap().unwrap();
    crew::sync_bus(&bus, &loaded).unwrap();
    crew::trust(&paths, &work).unwrap();
    fs::write(paths.workdir_file(), format!("{}\n", work.display())).unwrap();
    (db, paths, work)
}

fn aos_exe() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_aos"))
}

fn watcher(db: &Path) -> Watcher {
    Watcher::new(db, aos_exe(), Box::new(|_| {}))
}

fn wait_for(what: &str, check: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if check() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("timed out waiting for {what}");
}

fn kill9(pid: i32) {
    acs::platform::kill_group(pid as u32, acs::platform::SIGKILL);
    wait_for("pid gone", || !acs::platform::pid_alive(pid));
}

/// A process that exits at once, for a pid file whose owner is gone.
fn instant_exit() -> std::process::Child {
    #[cfg(unix)]
    return std::process::Command::new("true").spawn().unwrap();
    #[cfg(windows)]
    return std::process::Command::new("cmd")
        .args(["/c", "exit"])
        .spawn()
        .unwrap();
}

#[test]
fn a_crashed_supervisor_is_restarted_and_a_stopped_one_is_not() {
    let (db, paths, work) = crew_on("restart");
    let ids = vec!["w1".to_string()];
    // Nothing started: nothing to watch.
    let mut w = watcher(&db);
    assert_eq!(w.check(), 0);

    // A pid file left by a crash (or a reboot): its pid is no longer a supervisor.
    let mut gone = instant_exit();
    gone.wait().unwrap();
    fs::create_dir_all(paths.pid_file("w1").parent().unwrap()).unwrap();
    fs::write(paths.pid_file("w1"), format!("{}\n", gone.id())).unwrap();
    assert_eq!(keeper::crashed(&paths, &ids), ids);
    assert_eq!(w.check(), 1);
    wait_for("w1 running", || crew::running_pid(&paths, "w1").is_some());
    let first = crew::running_pid(&paths, "w1").unwrap();

    // Killed outright: restarted again.
    kill9(first);
    assert_eq!(w.check(), 1);
    wait_for("w1 running again", || {
        crew::running_pid(&paths, "w1").is_some_and(|p| p != first)
    });

    // Stopped on purpose: its pid file goes, and the watcher leaves it alone.
    let stopped = crew::stop(&paths, &ids);
    assert!(matches!(stopped[0].1, Ok(true)), "{stopped:?}");
    assert!(!paths.pid_file("w1").exists());
    assert_eq!(w.check(), 0);
    assert!(crew::running_pid(&paths, "w1").is_none());
    let _ = work;
}

#[test]
fn the_watcher_gives_up_on_an_agent_that_keeps_dying() {
    let (db, paths, _work) = crew_on("giveup");
    let mut w = watcher(&db);
    let mut gone = instant_exit();
    gone.wait().unwrap();
    fs::create_dir_all(paths.pid_file("w1").parent().unwrap()).unwrap();
    fs::write(paths.pid_file("w1"), format!("{}\n", gone.id())).unwrap();
    for round in 0..MAX_RESTARTS {
        assert_eq!(w.check(), 1, "round {round}");
        wait_for("w1 running", || crew::running_pid(&paths, "w1").is_some());
        kill9(crew::running_pid(&paths, "w1").unwrap());
    }
    // One crash too many within the hour: not restarted, no longer watched, operator told.
    assert_eq!(w.check(), 0);
    assert!(crew::running_pid(&paths, "w1").is_none());
    assert!(!paths.pid_file("w1").exists());
    let bus = Bus::open(Some(&db)).unwrap();
    let op = bus.identify(Some(acs::types::OPERATOR_ID)).unwrap();
    let mail = bus.inbox(&op, true, None).unwrap().messages;
    assert!(
        mail.iter()
            .any(|m| m.subject.starts_with("w1 keeps stopping")),
        "{mail:?}"
    );
}

#[test]
fn the_watcher_never_restarts_into_an_untrusted_folder() {
    let (db, paths, _work) = crew_on("untrusted");
    fs::write(paths.trusted_file(), "").unwrap();
    let mut gone = instant_exit();
    gone.wait().unwrap();
    fs::create_dir_all(paths.pid_file("w1").parent().unwrap()).unwrap();
    fs::write(paths.pid_file("w1"), format!("{}\n", gone.id())).unwrap();
    let mut w = watcher(&db);
    // Not restarted, but still watched: trusting the folder later lets it restart.
    assert_eq!(w.check(), 1);
    assert_eq!(w.check(), 1);
    assert!(crew::running_pid(&paths, "w1").is_none());
    assert!(paths.pid_file("w1").exists());
}

#[test]
fn autostart_files_name_the_watcher_and_keep_path() {
    let unit = keeper::systemd_unit(
        Path::new("/opt/aos/bin/aos"),
        Path::new("/srv/crew/.agent-bus/bus.db"),
        "/srv/crew/.local/bin:/usr/bin",
    );
    assert!(
        unit.contains("ExecStart=\"/opt/aos/bin/aos\" --db \"/srv/crew/.agent-bus/bus.db\" watch")
    );
    assert!(unit.contains("Environment=\"PATH=/srv/crew/.local/bin:/usr/bin\""));
    assert!(unit.contains("Restart=on-failure"));
    let plist = keeper::launchd_plist(
        Path::new("/opt/aos & co/aos"),
        Path::new("/srv/crew/.agent-bus/bus.db"),
        "/usr/bin",
        Path::new("/srv/crew/.agent-bus/logs/aos-watch.out"),
    );
    assert!(plist.contains("<string>/opt/aos &amp; co/aos</string><string>--db</string>"));
    assert!(plist.contains("<key>RunAtLoad</key><true/>"));
}

#[test]
fn a_default_budget_is_given_once_and_off_stays_off() {
    let (db, paths, _work) = crew_on("budget");
    let bus = Bus::open(Some(&db)).unwrap();
    let ids = vec!["w1".to_string()];
    assert_eq!(crew::apply_default_budget(&bus, &paths, &ids).unwrap(), ids);
    let agent = bus.get_agent("w1").unwrap().unwrap();
    let budget = bus.budget_of(&agent).unwrap();
    assert_eq!(budget.limits, crew::DEFAULT_BUDGET);

    // The operator turns it off: the next start does not put it back.
    let op = bus.identify(Some(acs::types::OPERATOR_ID)).unwrap();
    bus.set_budget(&op, "w1", None).unwrap();
    assert!(crew::apply_default_budget(&bus, &paths, &ids)
        .unwrap()
        .is_empty());
    let agent = bus.get_agent("w1").unwrap().unwrap();
    assert!(agent.meta.get("budget").is_none());
}
