//! A fake `qagent` for tests: does what a supervisor's first breath does —
//! writes <db home>/supervisors/<id>.pid with its own pid, then stays alive
//! honouring the detached-supervisor stop convention (<id>.stop next to the
//! pid file). Built as an example so `cargo test` produces the binary; the
//! desktop tests point ACS_DESKTOP_QAGENT at it.
//!
//! Usage: fake-qagent --db <bus.db> supervise <agent-id> <workdir> --config <crew>

use std::path::Path;
use std::time::{Duration, Instant};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let Some(db) = arg("--db") else {
        eprintln!("fake-qagent: missing --db");
        std::process::exit(2);
    };
    let Some(id) = arg("supervise") else {
        eprintln!("fake-qagent: missing supervise <id>");
        std::process::exit(2);
    };
    let home = Path::new(&db).parent().expect("db has a parent").to_path_buf();
    let supervisors = home.join("supervisors");
    std::fs::create_dir_all(&supervisors).expect("supervisors dir");
    let pid_file = supervisors.join(format!("{id}.pid"));
    std::fs::write(&pid_file, format!("{}\n", std::process::id())).expect("pid file");
    let stop_file = pid_file.with_extension("stop");
    let deadline = Instant::now() + Duration::from_secs(120);
    while Instant::now() < deadline {
        if stop_file.exists() {
            let _ = std::fs::remove_file(&stop_file);
            let _ = std::fs::remove_file(&pid_file);
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}
