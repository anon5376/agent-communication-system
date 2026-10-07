//! Transport to the bundled `acs-desktop` helper, the Windows counterpart of
//! macOS's `ACSClient`: one JSON request on stdin, one JSON envelope on
//! stdout, nothing else on stdout (diagnostics go to stderr).
//!
//! Each request spawns a fresh helper process directly (no shell,
//! CREATE_NO_WINDOW), waits on the child handle — never on pipe EOF, so a
//! descendant that inherited our pipes cannot keep a finished request waiting
//! — with a 30s timeout, kill on expiry, and a 16 MiB bound on retained
//! stdout/stderr. An `ok:true` reply from a nonzero exit is rejected, and an
//! error envelope stays authoritative even on nonzero exit.
//!
//! `data` is handed back as raw JSON text so i64 fields (task ids) keep their
//! exact digits across the webview boundary — serde_json's arbitrary-precision
//! feature is enabled so this passthrough never re-encodes through f64.

use serde::Serialize;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// Maximum bytes retained from each of the helper's stdout and stderr.
const OUTPUT_LIMIT: usize = 16 * 1024 * 1024;
/// Default request timeout; after it the helper is killed.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a pipe drainer keeps sweeping for trailing bytes once the child
/// has been observed dead (a descendant holding the pipe forfeits the rest).
const POST_EXIT_SWEEP: Duration = Duration::from_millis(150);
/// After a timeout kill, how long we keep waiting for the exit to land before
/// abandoning the request entirely.
const ABANDON_GRACE: Duration = Duration::from_secs(3);

/// Errors surfaced to the webview. Serialized as `{kind, code, message}`.
#[derive(Debug)]
pub enum AcsError {
    /// The executable is missing, not runnable, or failed to start.
    HelperUnavailable(String),
    /// The helper returned `{ok: false, error: {code, message}}`.
    HelperError { code: String, message: String },
    /// The helper exited nonzero without a trusted error envelope.
    HelperExited { status: i32, detail: String },
    /// stdout was not the expected single JSON envelope.
    MalformedResponse(String),
    /// The helper produced more output than the retained limit allows.
    OutputTruncated,
    /// No reply within the timeout; the helper was killed.
    TimedOut(u64),
}

impl AcsError {
    fn kind(&self) -> &'static str {
        match self {
            AcsError::HelperUnavailable(_) => "helper_unavailable",
            AcsError::HelperError { .. } => "helper_error",
            AcsError::HelperExited { .. } => "helper_exited",
            AcsError::MalformedResponse(_) => "malformed",
            AcsError::OutputTruncated => "output_truncated",
            AcsError::TimedOut(_) => "timeout",
        }
    }
}

impl std::fmt::Display for AcsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AcsError::HelperUnavailable(detail) => {
                write!(f, "The ACS helper is unavailable: {detail}")
            }
            AcsError::HelperError { message, .. } => f.write_str(message),
            AcsError::HelperExited { status, detail } => {
                write!(f, "The ACS helper exited with status {status}.")?;
                if !detail.is_empty() {
                    write!(f, " {detail}")?;
                }
                Ok(())
            }
            AcsError::MalformedResponse(detail) => {
                write!(f, "The ACS helper returned an unreadable response: {detail}")
            }
            AcsError::OutputTruncated => {
                f.write_str("The ACS helper produced more output than ACS can accept.")
            }
            AcsError::TimedOut(seconds) => {
                write!(f, "The ACS helper did not answer within {seconds} seconds.")
            }
        }
    }
}

impl std::error::Error for AcsError {}

impl Serialize for AcsError {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let code = match self {
            AcsError::HelperError { code, .. } => code.clone(),
            _ => self.kind().to_string(),
        };
        json!({
            "kind": self.kind(),
            "code": code,
            "message": self.to_string(),
        })
        .serialize(serializer)
    }
}

/// Where the bundled helper sits. `ACS_DESKTOP_HELPER` overrides (tests and
/// dev); otherwise it is the `acs-desktop.exe` next to the app executable —
/// the path the installer drops the real Windows bridge on.
pub fn helper_path() -> PathBuf {
    if let Some(path) = std::env::var_os("ACS_DESKTOP_HELPER") {
        return PathBuf::from(path);
    }
    let mut dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));
    dir.push("acs-desktop.exe");
    dir
}

/// Send one action to `helper` for the bus at `db_path`. Returns the reply
/// `data` payload re-encoded as JSON text (arbitrary precision preserved).
pub fn request(
    helper: &Path,
    db_path: &Path,
    action: &str,
    payload: Value,
) -> Result<String, AcsError> {
    request_with_timeout(helper, db_path, action, payload, DEFAULT_TIMEOUT)
}

pub fn request_with_timeout(
    helper: &Path,
    db_path: &Path,
    action: &str,
    payload: Value,
    timeout: Duration,
) -> Result<String, AcsError> {
    let envelope = json!({
        "version": 1,
        "dbPath": db_path.to_string_lossy(),
        "action": action,
        "payload": payload,
    });
    let reply = execute(helper, &serde_json::to_vec(&envelope).unwrap_or_default(), timeout)?;
    decode_reply(reply)
}

// ------------------------------------------------------------ process pump

struct Reply {
    status: i32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    stdout_truncated: bool,
}

/// Byte sink that retains up to `limit` bytes while still draining the rest,
/// so the child never blocks on a full pipe.
struct BoundedBuffer {
    limit: usize,
    storage: Mutex<Vec<u8>>,
    overflow: AtomicBool,
}

impl BoundedBuffer {
    fn new(limit: usize) -> Self {
        BoundedBuffer { limit, storage: Mutex::new(Vec::new()), overflow: AtomicBool::new(false) }
    }
    fn push(&self, bytes: &[u8]) {
        let mut storage = self.storage.lock().unwrap();
        let room = self.limit.saturating_sub(storage.len());
        if room > 0 {
            storage.extend_from_slice(&bytes[..room.min(bytes.len())]);
        }
        if bytes.len() > room {
            self.overflow.store(true, Ordering::SeqCst);
        }
    }
    fn did_overflow(&self) -> bool {
        self.overflow.load(Ordering::SeqCst)
    }
    fn data(&self) -> Vec<u8> {
        self.storage.lock().unwrap().clone()
    }
}

/// Poll the pipe with PeekNamedPipe instead of blocking in ReadFile: after
/// `done` is set (the child handle reaped) only a short final sweep runs, so a
/// descendant holding the pipe open cannot wedge the request.
fn drain_pipe<R: Read + std::os::windows::io::AsRawHandle>(
    mut reader: R,
    done: Arc<AtomicBool>,
    out: Arc<BoundedBuffer>,
) {
    use windows_sys::Win32::System::Pipes::PeekNamedPipe;
    let handle = reader.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;
    let sweep_deadline = |done: &AtomicBool| {
        if done.load(Ordering::SeqCst) {
            Some(Instant::now() + POST_EXIT_SWEEP)
        } else {
            None
        }
    };
    let mut sweep_until: Option<Instant> = sweep_deadline(&done);
    loop {
        let mut available: u32 = 0;
        let ok = unsafe {
            PeekNamedPipe(
                handle,
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                &mut available,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return; // pipe closed or broken
        }
        if available > 0 {
            let mut chunk = vec![0u8; (available as usize).min(256 * 1024)];
            match reader.read(&mut chunk) {
                Ok(0) => return, // EOF
                Ok(n) => out.push(&chunk[..n]),
                Err(_) => return,
            }
            continue;
        }
        if let Some(deadline) = sweep_until {
            if Instant::now() >= deadline {
                return;
            }
        } else if let Some(deadline) = sweep_deadline(&done) {
            sweep_until = Some(deadline);
        }
        thread::sleep(Duration::from_millis(2));
    }
}

fn terminate(child: &mut Child) {
    let _ = child.kill();
}

fn execute(helper: &Path, input: &[u8], timeout: Duration) -> Result<Reply, AcsError> {
    if !helper.is_file() {
        return Err(AcsError::HelperUnavailable(format!(
            "not executable: {}",
            helper.display()
        )));
    }

    let mut command = Command::new(helper);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // The helper is a console binary; keep any console window off screen.
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
    }

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return Err(AcsError::HelperUnavailable(format!(
                "{} failed to start: {error}",
                helper
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "acs-desktop.exe".to_string())
            )))
        }
    };

    // Write the single request on a worker thread, then close stdin so the
    // helper sees EOF and can reply.
    let mut stdin = child.stdin.take();
    let input_vec = input.to_vec();
    thread::spawn(move || {
        if let Some(mut pipe) = stdin.take() {
            let _ = pipe.write_all(&input_vec);
        }
        drop(stdin);
    });

    let done = Arc::new(AtomicBool::new(false));
    let out = Arc::new(BoundedBuffer::new(OUTPUT_LIMIT));
    let err = Arc::new(BoundedBuffer::new(OUTPUT_LIMIT));
    let mut readers = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        readers.push(thread::spawn({
            let done = done.clone();
            let out = out.clone();
            move || drain_pipe(stdout, done, out)
        }));
    }
    if let Some(stderr) = child.stderr.take() {
        readers.push(thread::spawn({
            let done = done.clone();
            let err = err.clone();
            move || drain_pipe(stderr, done, err)
        }));
    }

    // Wait on the child handle, never on pipe EOF. On timeout kill the direct
    // child only — descendants are the helper's own business; we just stop
    // reading their pipes.
    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let status: Option<i32> = loop {
        match child.try_wait() {
            Ok(Some(exit)) => break exit.code(),
            Ok(None) => {
                if timeout > Duration::ZERO && Instant::now() >= deadline {
                    timed_out = true;
                    terminate(&mut child);
                    break None;
                }
                thread::sleep(Duration::from_millis(5));
            }
            Err(_) => break None,
        }
    };

    // If we killed it, keep waiting briefly for the exit to be observed.
    let status = match status {
        Some(code) => Some(code),
        None => {
            let give_up = Instant::now() + ABANDON_GRACE;
            loop {
                match child.try_wait() {
                    Ok(Some(exit)) => break exit.code(),
                    Ok(None) if Instant::now() < give_up => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    _ => break None,
                }
            }
        }
    };

    // The owning process is finished (or abandoned): drainer threads do one
    // bounded final sweep rather than waiting on the pipes' EOF.
    done.store(true, Ordering::SeqCst);
    for reader in readers {
        let _ = reader.join();
    }

    if timed_out {
        return Err(AcsError::TimedOut(timeout.as_secs()));
    }
    Ok(Reply {
        status: status.unwrap_or(-1),
        stdout: out.data(),
        stderr: err.data(),
        stdout_truncated: out.did_overflow(),
    })
}

fn snippet(data: &[u8], limit: usize) -> String {
    let end = data.len().min(limit);
    String::from_utf8_lossy(&data[..end]).trim().to_string()
}

fn decode_reply(reply: Reply) -> Result<String, AcsError> {
    // A truncated stdout can look like valid JSON only because its tail was
    // dropped — reject before trusting any of it.
    if reply.stdout_truncated {
        return Err(AcsError::OutputTruncated);
    }
    let stdout = String::from_utf8_lossy(&reply.stdout);
    let probe: Option<Value> = serde_json::from_str(stdout.trim()).ok();
    let probe_obj = probe.as_ref().and_then(|v| v.as_object());
    let ok = probe_obj.and_then(|o| o.get("ok")).and_then(Value::as_bool);

    // An error envelope is authoritative even when the helper exits nonzero.
    if ok == Some(false) {
        let error = probe_obj.and_then(|o| o.get("error"));
        let code = error
            .and_then(|e| e.get("code"))
            .map(|c| {
                c.as_str()
                    .map(str::to_string)
                    .or_else(|| c.as_i64().map(|n| n.to_string()))
                    .or_else(|| c.as_f64().map(|n| n.to_string()))
                    .unwrap_or_else(|| "helper_error".to_string())
            })
            .unwrap_or_else(|| "helper_error".to_string());
        let message = error
            .and_then(|e| e.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("The helper reported failure.")
            .to_string();
        return Err(AcsError::HelperError { code, message });
    }
    if ok != Some(true) {
        if reply.status != 0 {
            return Err(AcsError::HelperExited {
                status: reply.status,
                detail: snippet(&reply.stderr, 400),
            });
        }
        return Err(AcsError::MalformedResponse(format!(
            "stdout was not a JSON envelope ({} bytes)",
            reply.stdout.len()
        )));
    }
    // Success requires a clean exit; an ok:true reply from a crashed helper
    // cannot be trusted.
    if reply.status != 0 {
        return Err(AcsError::HelperExited {
            status: reply.status,
            detail: snippet(&reply.stderr, 400),
        });
    }
    let data = probe_obj
        .and_then(|o| o.get("data"))
        .ok_or_else(|| AcsError::MalformedResponse("reply had no \"data\" payload".to_string()))?;
    serde_json::to_string(data)
        .map_err(|e| AcsError::MalformedResponse(format!("reply data did not serialize: {e}")))
}
