//! Wait-for-mail and inbox signal files — mirrors src/notify/wait.ts and the
//! SignalFileWatcher part of changes.ts.

use crate::bus::{Bus, WaitResult};
use crate::error::Result;
use crate::identity::Identity;
use crate::types::{DEFAULT_WAIT_SEC, MAX_WAIT_SEC};
use crate::watcher::{ChangeWatcher, ChangeWatcherOptions, FsWatchGuard};
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

pub fn inbox_dir(home: &Path) -> PathBuf {
    home.join("inbox")
}

pub fn signal_file_path(home: &Path, agent_id: &str) -> PathBuf {
    inbox_dir(home).join(format!("{agent_id}.seq"))
}

/// The seq written by flush_signals, or None.
pub fn read_signal_file(home: &Path, agent_id: &str) -> Option<i64> {
    let value = fs::read_to_string(signal_file_path(home, agent_id))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    Some(value)
}

/// Seconds between 1 and MAX_WAIT_SEC; 0 or blank means the default.
pub fn wait_seconds(value: Option<i64>, default: Option<i64>) -> Result<i64> {
    match value {
        None | Some(0) => Ok(default.unwrap_or(DEFAULT_WAIT_SEC).clamp(1, MAX_WAIT_SEC)),
        Some(seconds) if seconds > 0 => Ok(seconds.min(MAX_WAIT_SEC)),
        Some(_) => Ok(default.unwrap_or(DEFAULT_WAIT_SEC).clamp(1, MAX_WAIT_SEC)),
    }
}

/**
 * Wait for mail or task events affecting this agent. Stored 'waiting' with
 * wait_until_ms — the process that wrote it is live, and the coordinator's derived
 * status honors wait_until only while it is in the future: a dead waiter times
 * out to offline on its own.
 *
 * Synchronous port of Bus.waitForMail: returns status "mail" | "task" | "none".
 */
pub fn wait_for_mail(
    bus: &Bus,
    actor: &Identity,
    timeout: Duration,
    stop: &AtomicBool,
) -> Result<WaitResult> {
    let me = &actor.agent_id;
    let pending = bus.inbox(actor, true, None)?;
    if !pending.messages.is_empty() {
        // Waiters that die mid-poll leave 'waiting' behind; this clear covers that.
        bus.write(|bus| {
            bus.conn
                .prepare_cached("UPDATE agents SET status = 'idle', wait_until_ms = NULL WHERE id = ? AND status = 'waiting'")?
                .execute([me])?;
            Ok(())
        })?;
        return Ok(WaitResult {
            status: "mail".into(),
            messages: pending.messages,
            events: vec![],
            seq: bus.latest_seq()?,
        });
    }
    let watcher = ChangeWatcher::new(
        &bus.db_path,
        ChangeWatcherOptions {
            min_poll_ms: 10,
            max_poll_ms: 1000,
            fs_watch: true,
        },
    )?;
    let _inbox: Option<FsWatchGuard> =
        watcher.watch_dir(&inbox_dir(&bus.home), vec![format!("{me}.seq")]);
    let mut since = bus.write(|bus| {
        let now = bus.now();
        bus.conn
            .prepare_cached("UPDATE agents SET status = 'waiting', wait_until_ms = ?, last_seen_ms = ? WHERE id = ?")?
            .execute(rusqlite::params![
                now + timeout.as_millis() as i64,
                now,
                me
            ])?;
        bus.event(
            me,
            "agent_waiting",
            "agent",
            me,
            json!({ "until": now + timeout.as_millis() as i64 }),
        )?;
        bus.latest_seq()
    })?;
    let mut status = "none".to_string();
    let mut messages = vec![];
    let mut events = vec![];
    let pending = bus.inbox(actor, true, None)?;
    if !pending.messages.is_empty() {
        status = "mail".into();
        messages = pending.messages;
    }
    let deadline = Instant::now() + timeout;
    let mut seq = since;
    let mut last_error: Option<crate::error::BusError> = None;
    while status == "none" {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() || stop.load(std::sync::atomic::Ordering::SeqCst) {
            break;
        }
        match watcher.next(seq, remaining, stop) {
            Ok(next) => {
                seq = next;
            }
            Err(error) => {
                last_error = Some(error);
                break;
            }
        }
        if seq <= since || stop.load(std::sync::atomic::Ordering::SeqCst) {
            break;
        }
        let pending = bus.inbox(actor, true, None)?;
        if !pending.messages.is_empty() {
            status = "mail".into();
            messages = pending.messages;
            break;
        }
        let task_events = bus.task_events_for(me, since, seq)?;
        if !task_events.is_empty() {
            status = "task".into();
            events = task_events;
            break;
        }
        // Somebody else's event arrived; keep waiting.
        since = seq;
    }
    // The idle status update runs regardless of outcome — the waiter is gone.
    // Guarded on 'waiting' like the TS original: another command may have marked
    // this agent working while the wait was running.
    let _ = bus.write(|bus| {
        let now = bus.now();
        bus.conn
            .prepare_cached("UPDATE agents SET status = 'idle', wait_until_ms = NULL, last_seen_ms = ? WHERE id = ? AND status = 'waiting'")?
            .execute(rusqlite::params![now, me])?;
        bus.event(me, "agent_idle", "agent", me, json!({ "reason": status }))?;
        Ok(())
    });
    if let Some(error) = last_error {
        return Err(error);
    }
    Ok(WaitResult {
        status,
        messages,
        events,
        seq,
    })
}
