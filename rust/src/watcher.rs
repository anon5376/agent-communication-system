//! Change watcher over the events table — mirrors src/core/changes.ts.
//!
//! An idle waiter never writes. It polls `PRAGMA data_version`, which changes only
//! when another connection commits, with a bounded backoff (min_poll doubling up
//! to max_poll). A filesystem watch on the database directory wakes the loop
//! early when bus.db or bus.db-wal changes. Only when data_version moves does it
//! read max(events.seq), a primary-key lookup.

use crate::error::{BusError, Result};
use notify::{RecursiveMode, Watcher};
use rusqlite::Connection;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

pub struct ChangeWatcherOptions {
    /// First poll interval after a change. Default 10 ms.
    pub min_poll_ms: u64,
    /// Poll interval ceiling. Default 100 ms — a missed file event costs at most ~100 ms.
    pub max_poll_ms: u64,
    /// Use a filesystem watch on the database directory as an early wake-up. Default true.
    pub fs_watch: bool,
}

impl Default for ChangeWatcherOptions {
    fn default() -> Self {
        ChangeWatcherOptions {
            min_poll_ms: 10,
            max_poll_ms: 100,
            fs_watch: true,
        }
    }
}

struct FsWatch {
    _watcher: notify::RecommendedWatcher,
}

fn dir_watch(dir: &Path, names: Vec<String>, tx: Sender<()>) -> Option<FsWatch> {
    let watcher = notify::recommended_watcher(
        move |result: std::result::Result<notify::Event, notify::Error>| {
            let Ok(event) = result else { return };
            let hits = event.paths.iter().any(|path| {
                path.file_name()
                    .map(|name| names.contains(&name.to_string_lossy().to_string()))
                    .unwrap_or(false)
            });
            if hits || event.paths.is_empty() {
                let _ = tx.send(());
            }
        },
    );
    match watcher {
        Ok(mut watcher) => {
            if watcher.watch(dir, RecursiveMode::NonRecursive).is_err() {
                return None;
            }
            Some(FsWatch { _watcher: watcher })
        }
        Err(_) => None, // polling alone still bounds latency by max_poll
    }
}

/// Shared wake pipe: the db watcher and the optional inbox-signal watcher feed the
/// same channel, so the poll loop sleeps on one receiver.
///
/// The watcher holds its own SQLite connection: `PRAGMA data_version` only tracks
/// commits made by other connections, so a dedicated connection sees every write —
/// including the bus's own.
pub struct ChangeWatcher {
    conn: Connection,
    min_poll: Duration,
    max_poll: Duration,
    rx: Receiver<()>,
    tx: Sender<()>,
    closed: AtomicBool,
    _db: Option<FsWatch>,
}

impl ChangeWatcher {
    pub fn new(db_path: &Path, options: ChangeWatcherOptions) -> Result<ChangeWatcher> {
        let conn = Connection::open(db_path)?;
        conn.busy_timeout(Duration::from_millis(crate::db::BUSY_TIMEOUT_MS as u64))?;
        let min_poll = Duration::from_millis(options.min_poll_ms.max(1));
        let max_poll = Duration::from_millis(options.max_poll_ms.max(options.min_poll_ms.max(1)));
        let (tx, rx) = channel::<()>();
        let name = db_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let names = vec![name.clone(), format!("{name}-wal")];
        let dir = db_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let db = if options.fs_watch {
            dir_watch(&dir, names, tx.clone())
        } else {
            None
        };
        Ok(ChangeWatcher {
            conn,
            min_poll,
            max_poll,
            rx,
            tx,
            closed: AtomicBool::new(false),
            _db: db,
        })
    }

    pub fn data_version(&self) -> Result<i64> {
        Ok(self
            .conn
            .prepare_cached("PRAGMA data_version")?
            .query_row([], |row| row.get(0))?)
    }

    pub fn current_seq(&self) -> Result<i64> {
        Ok(self
            .conn
            .prepare_cached("SELECT COALESCE(MAX(seq), 0) AS seq FROM events")?
            .query_row([], |row| row.get(0))?)
    }

    fn sleep(&self, ms: Duration) -> bool {
        // Wakes early on a filesystem event or a poke on the wake pipe;
        // otherwise waits out the interval.
        self.rx.recv_timeout(ms).is_ok()
    }

    /// The wake pipe, for a second watcher (e.g. the inbox signal file) to feed.
    pub fn wake_sender(&self) -> Sender<()> {
        self.tx.clone()
    }

    /// Resolve with the latest events.seq once it is greater than `since_seq`,
    /// or with the unchanged sequence number when `timeout` elapses or `stop` is set.
    pub fn next(&self, since_seq: i64, timeout: Duration, stop: &AtomicBool) -> Result<i64> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(BusError::invalid("change watcher is closed"));
        }
        let mut seq = self.current_seq()?;
        if seq > since_seq {
            return Ok(seq);
        }
        let deadline = Instant::now() + timeout;
        let mut version = self.data_version()?;
        let mut interval = self.min_poll;
        let mut fs_dirty = false;
        while !self.closed.load(Ordering::SeqCst) && !stop.load(Ordering::SeqCst) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            if self.sleep(interval.min(remaining)) {
                fs_dirty = true;
            }
            if self.closed.load(Ordering::SeqCst) {
                break;
            }
            let current = self.data_version()?;
            if current != version {
                version = current;
                seq = self.current_seq()?;
                if seq > since_seq {
                    return Ok(seq);
                }
                interval = self.min_poll;
                fs_dirty = false;
            } else {
                interval = if fs_dirty {
                    fs_dirty = false;
                    self.min_poll
                } else {
                    (interval * 2).min(self.max_poll)
                };
            }
        }
        if self.closed.load(Ordering::SeqCst) {
            Ok(seq)
        } else {
            self.current_seq()
        }
    }

    /// Attach a second filesystem watch (the inbox signal file) feeding the same wake pipe.
    pub fn watch_dir(&self, dir: &Path, names: Vec<String>) -> Option<FsWatchGuard> {
        dir_watch(dir, names, self.wake_sender()).map(|watch| FsWatchGuard { _watch: watch })
    }

    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        let _ = self.tx.send(());
    }
}

/// Holds a directory watch alive; drop to stop it.
pub struct FsWatchGuard {
    _watch: FsWatch,
}
