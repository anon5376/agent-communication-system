//! Keeping a crew running without anyone watching: `aos watch` restarts any
//! supervisor that died without being stopped, and `aos autostart on` has the
//! operating system run `aos watch` at login or boot, so a crew that was running
//! before a reboot comes back on its own.
//!
//! A supervisor that stops cleanly (aos stop, Ctrl-C) removes its pid file. One
//! that crashed, was killed or went down with the machine leaves its pid file
//! behind with a pid that is no longer a supervisor. Those are the ones restarted.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::aos::crew::{self, Paths};
use crate::error::{BusError, Result};

/// How often the watcher looks.
pub const WATCH_INTERVAL: Duration = Duration::from_secs(10);
/// Restarts of one agent allowed within `RESTART_WINDOW` before the watcher gives up on it.
pub const MAX_RESTARTS: usize = 5;
pub const RESTART_WINDOW: Duration = Duration::from_secs(60 * 60);

pub fn watch_pid_file(paths: &Paths) -> PathBuf {
    paths.home.join("supervisors").join("aos-watch.pid")
}

fn watch_log(paths: &Paths) -> PathBuf {
    paths.home.join("logs").join("aos-watch.log")
}

/// The watcher's pid, if one is running for this bus.
pub fn watcher_pid(paths: &Paths) -> Option<i32> {
    let pid: i32 = fs::read_to_string(watch_pid_file(paths))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    crate::supervisor::process_running(pid, &["watch"]).then_some(pid)
}

/// Agents whose supervisor died without being stopped: a pid file whose pid is
/// not that agent's supervisor any more.
pub fn crashed(paths: &Paths, ids: &[String]) -> Vec<String> {
    ids.iter()
        .filter(|id| paths.pid_file(id).exists() && crew::running_pid(paths, id).is_none())
        .cloned()
        .collect()
}

/// Agents the watcher is responsible for: running, or crashed and due a restart.
fn watched(paths: &Paths, ids: &[String]) -> usize {
    ids.iter().filter(|id| paths.pid_file(id).exists()).count()
}

pub struct Watcher {
    pub db_path: PathBuf,
    pub paths: Paths,
    /// The binary started as each supervisor (`aos`, or the test's built binary).
    pub exe: PathBuf,
    restarts: HashMap<String, Vec<Instant>>,
    log: Box<dyn Fn(&str) + Send>,
    /// The last reason nothing could be restarted, so it is logged once, not every look.
    held: Option<String>,
}

impl Watcher {
    pub fn new(db_path: &Path, exe: PathBuf, log: Box<dyn Fn(&str) + Send>) -> Watcher {
        Watcher {
            db_path: db_path.to_path_buf(),
            paths: Paths::for_db(db_path),
            exe,
            restarts: HashMap::new(),
            log,
            held: None,
        }
    }

    /// Nothing can be restarted right now: log why (once), and keep watching the
    /// crashed agents, so a watcher started at login does not exit over a crew file
    /// it could not read yet or a folder that is not trusted yet.
    fn hold(&mut self, why: String, waiting: usize) -> usize {
        if self.held.as_deref() != Some(why.as_str()) {
            (self.log)(&why);
            self.held = Some(why);
        }
        waiting
    }

    /// One look: restart what crashed. Returns how many agents are still watched.
    pub fn check(&mut self) -> usize {
        let config = match crew::load_crew(&self.paths) {
            Ok(Some(config)) => config,
            Ok(None) => return 0,
            Err(error) => return self.hold(format!("could not read the crew: {error}"), 1),
        };
        let ids = crew::member_ids(&config);
        let down = crashed(&self.paths, &ids);
        if down.is_empty() {
            return watched(&self.paths, &ids);
        }
        let waiting = down.len();
        let Some(dir) = crew::crew_workdir(&self.paths) else {
            return self.hold("crew has no working folder on record; nothing restarted".into(), waiting);
        };
        // The same checks aos start makes: never in ~ or /, only in a folder the operator trusted.
        if let Some(why) = crew::unsafe_workdir(&dir) {
            return self.hold(format!("not restarting: {why}"), waiting);
        }
        if !crew::is_trusted(&self.paths, &dir) {
            return self.hold(
                format!("not restarting: {} is not a trusted folder", dir.display()),
                waiting,
            );
        }
        self.held = None;
        let now = Instant::now();
        let mut due = Vec::new();
        for id in down {
            let history = self.restarts.entry(id.clone()).or_default();
            history.retain(|t| now.duration_since(*t) < RESTART_WINDOW);
            // The stale pid file goes either way: restarted, it is rewritten; given up on,
            // the agent is no longer watched until someone starts it again.
            let _ = fs::remove_file(self.paths.pid_file(&id));
            if history.len() >= MAX_RESTARTS {
                (self.log)(&format!(
                    "{id} stopped {MAX_RESTARTS} times within an hour; not restarting it. \
                     See logs/{id}.out, then start it again with aos start {id}"
                ));
                notify_operator(&self.db_path, &id);
                continue;
            }
            history.push(now);
            due.push(id);
        }
        if !due.is_empty() {
            for (id, r) in crew::start_with(&self.exe, &self.db_path, &self.paths, &due, &dir) {
                match r {
                    Ok(pid) => (self.log)(&format!("{id} was down; restarted (pid {pid})")),
                    Err(e) => (self.log)(&format!("{id} was down; restart failed: {}", e.message)),
                }
            }
        }
        watched(&self.paths, &ids)
    }
}

/// Tell the operator through the bus that an agent keeps going down.
fn notify_operator(db_path: &Path, id: &str) {
    let Ok(bus) = crate::bus::Bus::open(Some(db_path)) else {
        return;
    };
    let Ok(me) = bus.identify(Some(crate::types::OPERATOR_ID)) else {
        return;
    };
    let _ = bus.send(
        &me,
        crate::bus::SendInput {
            to: crate::types::OPERATOR_ID.into(),
            subject: Some(format!("{id} keeps stopping; aos watch gave up restarting it")),
            body: format!(
                "{id}'s supervisor stopped {MAX_RESTARTS} times within an hour without being asked to. \
                 Its output is logs/{id}.out under the bus folder. Start it again with aos start {id}."
            ),
            msg_type: Some("info".into()),
            thread: None,
            task_id: None,
            refs: None,
            requires_ack: false,
        },
    );
}

/// `aos watch`: look every few seconds until nothing is left to watch or `stop` is set.
/// One watcher per bus. Returns the exit code.
pub fn watch(db_path: &Path, stop: Arc<AtomicBool>) -> i32 {
    let paths = Paths::for_db(db_path);
    if let Some(pid) = watcher_pid(&paths) {
        if pid as u32 != std::process::id() {
            eprintln!("aos: a watcher is already running (pid {pid})");
            return 0;
        }
    }
    let _ = fs::create_dir_all(paths.home.join("supervisors"));
    let _ = fs::create_dir_all(paths.home.join("logs"));
    let pid_file = watch_pid_file(&paths);
    if fs::write(&pid_file, format!("{}\n", std::process::id())).is_err() {
        eprintln!("aos: cannot write {}", pid_file.display());
        return 1;
    }
    let log_path = watch_log(&paths);
    let log = move |line: &str| {
        let stamped = format!(
            "[{}] {}",
            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            line
        );
        println!("{stamped}");
        if fs::metadata(&log_path)
            .map(|m| m.len() > crate::supervisor::MAX_LOG_BYTES)
            .unwrap_or(false)
        {
            let _ = fs::rename(&log_path, log_path.with_extension("log.1"));
        }
        if let Ok(mut f) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
        {
            let _ = writeln!(f, "{stamped}");
        }
    };
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("aos"));
    let mut watcher = Watcher::new(db_path, exe, Box::new(log));
    (watcher.log)("watching the crew");
    while !stop.load(Ordering::SeqCst) {
        if watcher.check() == 0 {
            (watcher.log)("no agent left to watch; stopping");
            break;
        }
        let until = Instant::now() + WATCH_INTERVAL;
        while Instant::now() < until && !stop.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    if fs::read_to_string(&pid_file)
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        == Some(std::process::id())
    {
        let _ = fs::remove_file(&pid_file);
    }
    0
}

/// Start `aos watch` in the background unless one is already running.
/// Only from the real `aos` binary: tests run under a test harness binary.
pub fn ensure_watcher(db_path: &Path) -> Option<i32> {
    let paths = Paths::for_db(db_path);
    if let Some(pid) = watcher_pid(&paths) {
        return Some(pid);
    }
    let exe = std::env::current_exe().ok()?;
    if exe.file_stem().and_then(|s| s.to_str()) != Some("aos") {
        return None;
    }
    spawn_watcher(&exe, db_path)
}

pub fn spawn_watcher(exe: &Path, db_path: &Path) -> Option<i32> {
    use std::os::unix::process::CommandExt;
    let paths = Paths::for_db(db_path);
    let _ = fs::create_dir_all(paths.home.join("logs"));
    let out = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths.home.join("logs").join("aos-watch.out"))
        .ok()?;
    let err = out.try_clone().ok()?;
    let mut cmd = Command::new(exe);
    cmd.arg("--db")
        .arg(db_path)
        .arg("watch")
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err);
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = cmd.spawn().ok()?;
    let pid = child.id() as i32;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Some(pid)
}

// ------------------------------------------------------------------ autostart

const UNIT_NAME: &str = "aos-watch.service";
const LAUNCHD_LABEL: &str = "dev.aos.watch";

pub fn systemd_unit_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("systemd").join("user").join(UNIT_NAME))
}

pub fn launchd_plist_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| {
        h.join("Library")
            .join("LaunchAgents")
            .join(format!("{LAUNCHD_LABEL}.plist"))
    })
}

/// The systemd user unit. PATH is the one aos was run with, so the watcher (and the
/// agents it restarts) find the same CLIs a terminal does.
pub fn systemd_unit(exe: &Path, db_path: &Path, path_env: &str) -> String {
    format!(
        "[Unit]\n\
         Description=aos: bring the agent crew back after a crash or reboot\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart=\"{}\" --db \"{}\" watch\n\
         Environment=\"PATH={}\"\n\
         Restart=on-failure\n\
         RestartSec=30\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        exe.display(),
        db_path.display(),
        path_env.replace('"', "")
    )
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

pub fn launchd_plist(exe: &Path, db_path: &Path, path_env: &str, log: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{LAUNCHD_LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{}</string><string>--db</string><string>{}</string><string>watch</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict><key>PATH</key><string>{}</string></dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
  <key>StandardOutPath</key><string>{}</string>
  <key>StandardErrorPath</key><string>{}</string>
</dict>
</plist>
"#,
        xml(&exe.display().to_string()),
        xml(&db_path.display().to_string()),
        xml(path_env),
        xml(&log.display().to_string()),
        xml(&log.display().to_string()),
    )
}

fn run_quiet(cmd: &str, args: &[&str]) -> std::result::Result<(), String> {
    match Command::new(cmd).args(args).stdin(Stdio::null()).output() {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => Err(String::from_utf8_lossy(&o.stderr).trim().to_string()),
        Err(e) => Err(format!("{cmd}: {e}")),
    }
}

/// `aos autostart on|off|status`. Prints what it did; returns the exit code.
pub fn autostart(db_path: &Path, action: &str) -> i32 {
    match autostart_inner(db_path, action) {
        Ok(lines) => {
            for l in lines {
                println!("{l}");
            }
            0
        }
        Err(e) => {
            eprintln!("aos: {}", e.message);
            1
        }
    }
}

fn autostart_inner(db_path: &Path, action: &str) -> Result<Vec<String>> {
    let exe = std::env::current_exe()?;
    let path_env = std::env::var("PATH").unwrap_or_default();
    let paths = Paths::for_db(db_path);
    let macos = cfg!(target_os = "macos");
    let file = if macos {
        launchd_plist_path()
    } else {
        systemd_unit_path()
    }
    .ok_or_else(|| BusError::invalid("cannot find your home folder"))?;
    match action {
        "on" => {
            if let Some(dir) = file.parent() {
                fs::create_dir_all(dir)?;
            }
            let mut lines = vec![];
            if macos {
                let log = paths.home.join("logs").join("aos-watch.out");
                fs::write(&file, launchd_plist(&exe, db_path, &path_env, &log))?;
                let target = format!("gui/{}", unsafe { libc::getuid() });
                let _ = run_quiet("launchctl", &["bootout", &target, &file.display().to_string()]);
                run_quiet("launchctl", &["bootstrap", &target, &file.display().to_string()])
                    .or_else(|_| run_quiet("launchctl", &["load", "-w", &file.display().to_string()]))
                    .map_err(|e| {
                        let _ = fs::remove_file(&file);
                        BusError::invalid(format!("launchctl refused {} ({e}), so autostart is off", file.display()))
                    })?;
                lines.push(format!("ok autostart on / {}", crew::Paths::show(&file)));
                lines.push("aos watch runs at login and restarts any agent that went down without aos stop".into());
            } else {
                fs::write(&file, systemd_unit(&exe, db_path, &path_env))?;
                run_quiet("systemctl", &["--user", "daemon-reload"])
                    .and_then(|_| run_quiet("systemctl", &["--user", "enable", "--now", UNIT_NAME]))
                    .map_err(|e| {
                        let _ = fs::remove_file(&file);
                        BusError::invalid(format!(
                            "systemctl --user failed ({e}), so autostart is off. Without systemd, add this line to crontab -e: @reboot PATH=\"{}\" \"{}\" --db \"{}\" watch",
                            path_env.replace('"', ""),
                            exe.display(),
                            db_path.display()
                        ))
                    })?;
                lines.push(format!("ok autostart on / {}", crew::Paths::show(&file)));
                lines.push("aos watch runs at login and restarts any agent that went down without aos stop".into());
                lines.push("to have it run at boot before you log in: loginctl enable-linger".into());
            }
            Ok(lines)
        }
        "off" => {
            if macos {
                let target = format!("gui/{}", unsafe { libc::getuid() });
                let _ = run_quiet("launchctl", &["bootout", &target, &file.display().to_string()]);
            } else {
                let _ = run_quiet("systemctl", &["--user", "disable", "--now", UNIT_NAME]);
            }
            let existed = fs::remove_file(&file).is_ok();
            if !macos {
                let _ = run_quiet("systemctl", &["--user", "daemon-reload"]);
            }
            Ok(vec![if existed {
                "ok autostart off / the crew no longer comes back by itself after a reboot".into()
            } else {
                "autostart was already off".into()
            }])
        }
        "status" | "" => {
            let on = file.exists();
            let watching = watcher_pid(&paths)
                .map(|p| format!("watcher running (pid {p})"))
                .unwrap_or_else(|| "no watcher running".into());
            Ok(vec![format!(
                "autostart {} / {watching}",
                if on { "on" } else { "off" }
            )])
        }
        _ => Err(BusError::invalid("usage: aos autostart on|off|status")),
    }
}
