//! OS split for the Windows port: permission hardening, process liveness and
//! termination, detached spawns, and PATH/PATHEXT program resolution.
//! Every cfg(unix) block is the original code moved here unchanged; the
//! cfg(windows) blocks implement the same semantics with Win32 primitives.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::Result;
#[cfg(windows)]
use crate::error::BusError;

// ------------------------------------------------------------- permissions

/// chmod a file or directory private (0600 token/db files, 0700 directories).
/// On Unix this is the POSIX mode. On Windows there is no POSIX mode: the bus
/// home lives under the user's profile (~/.agent-bus, %LOCALAPPDATA%), whose
/// per-user ACLs already keep other local users out — so this is a no-op
/// there. The limitation is documented in rust/README.md.
pub fn chmod_private(path: &Path, mode: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
    }
    #[cfg(windows)]
    {
        let _ = (path, mode);
    }
}

// --------------------------------------------------------------- liveness

/// Is `pid` a running process? Unix: `kill(pid, 0)` (EPERM counts — the
/// process is alive, just owned by someone else). Windows:
/// OpenProcess + GetExitCodeProcess == STILL_ACTIVE.
pub fn pid_alive(pid: i32) -> bool {
    #[cfg(unix)]
    {
        (unsafe { libc::kill(pid, 0) }) == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        unsafe {
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid as u32);
            if process.is_null() {
                return false;
            }
            let mut code = 0u32;
            let alive =
                GetExitCodeProcess(process, &mut code) != 0 && code == 0x103; // STILL_ACTIVE
            CloseHandle(process);
            alive
        }
    }
}

/// The pid's command line words, when the OS lets us read them. Linux reads
/// /proc; other Unix asks `ps` (cached per pid — aos checks on every screen
/// refresh). Windows does not read it: a live pid counts, the documented rule
/// in supervisor::process_running.
pub fn process_args(pid: i32) -> Option<Vec<String>> {
    #[cfg(target_os = "linux")]
    {
        fs::read(format!("/proc/{pid}/cmdline")).ok().map(|raw| {
            raw.split(|b| *b == 0)
                .map(|a| String::from_utf8_lossy(a).to_string())
                .collect()
        })
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        use std::collections::HashMap;
        use std::process::Stdio;
        use std::sync::Mutex;
        static SEEN: Mutex<Option<HashMap<i32, Vec<String>>>> = Mutex::new(None);
        let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
        let seen = seen.get_or_insert_with(HashMap::new);
        if let Some(args) = seen.get(&pid) {
            Some(args.clone())
        } else {
            let args = Command::new("ps")
                .args(["-o", "command=", "-p", &pid.to_string()])
                .stderr(Stdio::null())
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| {
                    String::from_utf8_lossy(&o.stdout)
                        .split_whitespace()
                        .map(str::to_string)
                        .collect::<Vec<_>>()
                });
            if let Some(args) = &args {
                seen.insert(pid, args.clone());
            }
            args
        }
    }
    #[cfg(windows)]
    {
        let _ = pid;
        None
    }
}

// ------------------------------------------------------------ termination

#[cfg(unix)]
pub use libc::{SIGKILL, SIGTERM};

// Windows has no POSIX signals; these stand-ins keep call sites identical.
// SIGTERM asks for a graceful stop, SIGKILL forces it.
#[cfg(windows)]
pub const SIGTERM: i32 = 15;
#[cfg(windows)]
pub const SIGKILL: i32 = 9;

/// Signal/terminate the whole tree rooted at `pid`. Unix sends the signal to
/// the child's process group (falling back to the lone pid). Windows has no
/// process-group signals: the graceful step is a best-effort CTRL_BREAK to the
/// detached group — usually a no-op for a windowless child — and the forceful
/// step ends the Job Object the spawn was assigned to (the whole tree).
#[cfg(unix)]
pub fn kill_group(pid: u32, signal: libc::c_int) {
    unsafe {
        if libc::kill(-(pid as i32), signal) != 0 {
            libc::kill(pid as i32, signal);
        }
    }
}

#[cfg(windows)]
pub fn kill_group(pid: u32, signal: i32) {
    if signal == SIGKILL {
        jobs::terminate_tree(pid);
    } else {
        graceful_break(pid);
    }
}

#[cfg(windows)]
fn graceful_break(pid: u32) {
    // CTRL_BREAK addressed to the child's own process group id (== its pid,
    // it was spawned CREATE_NEW_PROCESS_GROUP). A CREATE_NO_WINDOW child has
    // no console to receive it, so this is typically a no-op — the SIGKILL
    // follow-up through the Job Object is what guarantees the stop.
    unsafe {
        windows_sys::Win32::System::Console::GenerateConsoleCtrlEvent(
            windows_sys::Win32::System::Console::CTRL_BREAK_EVENT,
            pid,
        );
    }
}

/// Job Objects give Windows the "kill the whole tree" semantics a Unix process
/// group gives: each supervised child is assigned at spawn so a later
/// terminate_tree ends its children too. KILL_ON_JOB_CLOSE is never set —
/// supervised trees must outlive their launcher.
#[cfg(windows)]
pub mod jobs {
    use std::collections::HashMap;
    use std::sync::Mutex;
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, TerminateJobObject,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, TerminateProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
    };

    // pid -> job handle (raw HANDLE pointer, kept as usize so the map is Send),
    // for trees launched by this process.
    static JOBS: Mutex<Option<HashMap<u32, usize>>> = Mutex::new(None);

    /// Put the just-spawned `pid` under a new Job Object so stop/timeout can
    /// kill its children too. Best-effort: if assignment fails the root
    /// process alone is terminated later.
    pub fn watch_tree(pid: u32) {
        unsafe {
            let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
            if process.is_null() {
                return;
            }
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if !job.is_null() && AssignProcessToJobObject(job, process) != 0 {
                JOBS.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get_or_insert_with(HashMap::new)
                    .insert(pid, job as usize);
            } else if !job.is_null() {
                CloseHandle(job);
            }
            CloseHandle(process);
        }
    }

    /// Terminate `pid` and every descendant in its Job Object. Falls back to
    /// killing the root process alone when no job was assigned.
    pub fn terminate_tree(pid: u32) {
        let job = JOBS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
            .and_then(|jobs| jobs.remove(&pid));
        unsafe {
            if let Some(job) = job {
                // Terminating kills the whole tree; the handle can then close.
                let job = job as windows_sys::Win32::Foundation::HANDLE;
                TerminateJobObject(job, 1);
                CloseHandle(job);
                return;
            }
            let process = OpenProcess(PROCESS_TERMINATE, 0, pid);
            if !process.is_null() {
                TerminateProcess(process, 1);
                CloseHandle(process);
            }
        }
    }

    /// Release the Job Object when the supervised child exits on its own.
    /// Closing the handle kills nothing (no KILL_ON_JOB_CLOSE was set).
    pub fn forget(pid: u32) {
        let job = JOBS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
            .and_then(|jobs| jobs.remove(&pid));
        if let Some(job) = job {
            unsafe {
                CloseHandle(job as windows_sys::Win32::Foundation::HANDLE);
            }
        }
    }
}

// --------------------------------------------------------------- spawning

/// Creation flags for a detached child: its own process group and no console
/// window, so it survives the app or terminal closing — the Windows
/// counterpart of setsid.
#[cfg(windows)]
pub const DETACHED_SPAWN_FLAGS: u32 =
    windows_sys::Win32::System::Threading::CREATE_NEW_PROCESS_GROUP
        | windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

/// Detach a Command from its launcher: setsid on Unix (the original pre_exec,
/// unchanged), CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW on Windows. The
/// child keeps running after the parent exits.
pub fn detach(cmd: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(DETACHED_SPAWN_FLAGS);
    }
}

/// The file a supervisor polls as its stop request on Windows:
/// `<agent>.stop` beside `<agent>.pid`. A windowless child cannot take console
/// Ctrl events, so `aos stop` / desktop `stop` writes this file instead of
/// sending SIGINT.
pub fn stop_file_for(pid_file: &Path) -> PathBuf {
    pid_file.with_extension("stop")
}

// ----------------------------------------------------- program resolution

/// Does `s` name an explicit path rather than a bare program? A separator or
/// absolute form (`/x`, `C:\x`, `\\host\x`) makes it a path on every OS.
pub fn looks_like_path(s: &str) -> bool {
    Path::new(s).is_absolute() || s.contains('/') || (cfg!(windows) && s.contains('\\'))
}

#[cfg(windows)]
fn pathext() -> Vec<String> {
    std::env::var("PATHEXT")
        .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string())
        .split(';')
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .map(|e| {
            if e.starts_with('.') {
                e.to_string()
            } else {
                format!(".{e}")
            }
        })
        .collect()
}

/// Every file `dir/name` can mean. Unix: just `dir/name` (unchanged). Windows:
/// the PATHEXT expansion — `.exe/.cmd/.bat/...` — so npm-installed shims such
/// as claude.cmd and codex.cmd are found the way cmd.exe would find them.
pub fn dir_candidates(dir: &Path, name: &str) -> Vec<PathBuf> {
    let base = dir.join(name);
    #[cfg(unix)]
    {
        vec![base]
    }
    #[cfg(windows)]
    {
        if Path::new(name).extension().is_some() {
            return vec![base];
        }
        let mut out = vec![base];
        for ext in pathext() {
            out.push(dir.join(format!("{name}{ext}")));
        }
        out
    }
}

/// Filesystem candidates for a command: each PATH dir times every extension on
/// Windows; each PATH dir on Unix. An explicit path stays a single candidate —
/// on Windows with PATHEXT expansion when it carries no extension.
pub fn path_candidates(command: &str) -> Vec<PathBuf> {
    if looks_like_path(command) {
        #[cfg(unix)]
        return vec![PathBuf::from(command)];
        #[cfg(windows)]
        {
            if Path::new(command).extension().is_some() {
                return vec![PathBuf::from(command)];
            }
            let mut out = vec![PathBuf::from(command)];
            for ext in pathext() {
                out.push(PathBuf::from(format!("{command}{ext}")));
            }
            return out;
        }
    }
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths)
                .flat_map(|dir| dir_candidates(&dir, command))
                .collect()
        })
        .unwrap_or_default()
}

/// Is `path` a runnable program? Unix: a regular file with an exec bit
/// (unchanged). Windows: a regular file whose extension PATHEXT runs.
pub fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path)
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(windows)]
    {
        path.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| {
                pathext()
                    .iter()
                    .any(|e| e.trim_start_matches('.').eq_ignore_ascii_case(ext))
            })
            && path.is_file()
    }
}

/// Resolve a bare command name through PATH/PATHEXT so `claude` finds
/// `claude.cmd`. An explicit path or an unresolved name comes back unchanged —
/// the OS's own lookup still applies.
#[cfg(windows)]
fn resolve_program(program: &Path) -> PathBuf {
    let name = program.to_string_lossy();
    if looks_like_path(&name) {
        return program.to_path_buf();
    }
    path_candidates(&name)
        .into_iter()
        .find(|p| is_executable(p))
        .unwrap_or_else(|| program.to_path_buf())
}

/// Does `program` need cmd.exe to run — a batch shim?
#[cfg(windows)]
fn needs_cmd_shim(program: &Path) -> bool {
    program
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("cmd") || ext.eq_ignore_ascii_case("bat"))
}

/// Quote one piece of a `cmd /c` command line: always wrapped in double
/// quotes, which makes cmd metacharacters (& | < > ^ ( )) inert. Anything that
/// could still let cmd.exe expand or split the line — % (env expansion), a
/// quote (breaks out), a newline — is refused: a clear error is safer than
/// command injection.
#[cfg(windows)]
fn cmd_quote(arg: &str) -> Result<String> {
    if arg.chars().any(|c| matches!(c, '"' | '%' | '\r' | '\n')) {
        return Err(BusError::invalid(format!(
            "cannot run a .cmd/.bat provider safely: {arg:?} cannot be quoted for cmd.exe"
        )));
    }
    Ok(format!("\"{arg}\""))
}

/// Build a Command for `program` + `args`.
///
/// Windows: a bare name resolves through PATH/PATHEXT first. A resolved
/// .cmd/.bat shim then runs as `cmd.exe /d /s /c " "<prog>" "<arg>" ..."` —
/// the whole line goes on the command line verbatim (raw_arg), so cmd strips
/// the outer pair of quotes and reads each inner quoted piece as-is: no shell
/// interpolation, and any piece that cannot be quoted safely is refused with
/// a clear error. /d keeps AutoRun registry commands out of the line.
///
/// Unix and non-shim programs: a plain Command::new + args, unchanged.
///
/// ALL of the process's arguments must come through `args`: a .cmd line is
/// sealed inside one quoted string at construction, and anything `.arg()`ed
/// onto the returned Command afterwards lands outside it.
pub fn program_command<S: AsRef<std::ffi::OsStr>>(program: &Path, args: &[S]) -> Result<Command> {
    #[cfg(windows)]
    {
        let program = resolve_program(program);
        if needs_cmd_shim(&program) {
            let mut line = cmd_quote(&program.to_string_lossy())?;
            for arg in args {
                line.push(' ');
                line.push_str(&cmd_quote(&arg.as_ref().to_string_lossy())?);
            }
            let mut cmd = Command::new("cmd.exe");
            cmd.args(["/d", "/s", "/c"]);
            use std::os::windows::process::CommandExt;
            // Verbatim on the command line — the quoting above already did the
            // escaping cmd understands; std's own quoting would double-escape it.
            cmd.raw_arg(format!("\"{line}\""));
            return Ok(cmd);
        }
        let mut cmd = Command::new(program);
        for arg in args {
            cmd.arg(arg);
        }
        return Ok(cmd);
    }
    #[cfg(unix)]
    {
        let mut cmd = Command::new(program);
        for arg in args {
            cmd.arg(arg);
        }
        Ok(cmd)
    }
}

// --------------------------------------------------------------- .out file

/// Is our standard output (fd/handle 1) still this file? Used before
/// truncating the `.out` log in place — a rotated or redirected stdout means
/// the file is not ours to empty. Unix compares dev+inode of fd 1 against the
/// path (unchanged); Windows compares the file identity (volume + index)
/// behind the standard output handle.
#[cfg(unix)]
pub fn stdout_is_file(path: &Path, meta: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    let _ = path;
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    (unsafe { libc::fstat(1, &mut st) }) == 0
        && st.st_dev as u64 == meta.dev()
        && st.st_ino as u64 == meta.ino()
}

#[cfg(windows)]
fn file_identity(
    handle: windows_sys::Win32::Foundation::HANDLE,
) -> Option<(u32, u64)> {
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };
    unsafe {
        let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
        if GetFileInformationByHandle(handle, &mut info) == 0 {
            return None;
        }
        Some((
            info.dwVolumeSerialNumber,
            ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64,
        ))
    }
}

#[cfg(windows)]
pub fn stdout_is_file(path: &Path, meta: &fs::Metadata) -> bool {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::Console::{GetStdHandle, STD_OUTPUT_HANDLE};
    let _ = meta;
    unsafe {
        let stdout = GetStdHandle(STD_OUTPUT_HANDLE);
        if stdout.is_null() || stdout == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
            return false;
        }
        let Ok(file) = fs::File::open(path) else {
            return false;
        };
        file_identity(stdout) == file_identity(file.as_raw_handle())
    }
}

/// Empty a file that is also our stdout. Unix ftruncates fd 1 (unchanged);
/// Windows truncates through a fresh handle — same effect, the append-mode
/// stdout handle resumes writing at the new end.
#[cfg(unix)]
pub fn truncate_stdout(path: &Path) {
    let _ = path;
    unsafe {
        libc::ftruncate(1, 0);
    }
}

#[cfg(windows)]
pub fn truncate_stdout(path: &Path) {
    if let Ok(file) = fs::OpenOptions::new().write(true).open(path) {
        let _ = file.set_len(0);
    }
}
