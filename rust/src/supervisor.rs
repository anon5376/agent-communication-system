//! `qagent supervise <agent> [dir]` — ported verbatim from src/supervisor.ts.
//! Holds the blocking wait for one agent, renders a brief, and runs the vendor
//! CLI through its harness adapter. Claims/failures/auto-submits for harnesses
//! without bus tools go through the bus as that agent.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::adapters::{get_harness_adapter, AdapterContext, McpCommand};
use crate::bus::{Bus, ListTasksInput, SubmitInput};
use crate::config::{config_path_from_project, load_config, resolve_agent, ResolvedAgent};
use crate::db::home_for;
use crate::error::{BusError, Result};
use crate::types::{Message, Task, DEFAULT_WAIT_SEC};

/// First non-empty environment variable among `names` (new name first).
fn env_value(names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|name| std::env::var(name).ok().filter(|v| !v.trim().is_empty()))
}

const PROVIDER_KEYS: &[(&str, &[&str])] = &[
    ("anthropic", &["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN"]),
    ("openai", &["OPENAI_API_KEY"]),
    ("google", &["GEMINI_API_KEY", "GOOGLE_API_KEY"]),
    ("moonshot", &["MOONSHOT_API_KEY", "KIMI_API_KEY"]),
    ("xai", &["XAI_API_KEY"]),
];

/// Provider API keys are stripped from harness env for subscription-backed
/// providers unless the operator opts in — matches src/supervisor.ts.
pub fn sanitized_environment(
    agent: &ResolvedAgent,
    additions: &HashMap<String, String>,
) -> HashMap<String, String> {
    let mut env: HashMap<String, String> = std::env::vars().collect();
    env.extend(additions.iter().map(|(k, v)| (k.clone(), v.clone())));
    env.insert("MCP_TOOL_TIMEOUT".to_string(), "3600000".to_string());
    if env_value(&["QAGENT_ALLOW_API_KEY", "AGENT_BUS_ALLOW_API_KEY"]).as_deref() == Some("1")
        || !agent.provider.subscription_backed
    {
        return env;
    }
    for (provider, keys) in PROVIDER_KEYS {
        if *provider == agent.model.provider {
            for key in *keys {
                env.remove(*key);
            }
        }
    }
    env
}

/// Credentials an unattended agent has no business holding: code hosting, package
/// registries, cloud accounts, and the SSH agent (which would let it push or log in
/// anywhere the operator can). The agent's own model provider keys are not here.
pub const GUARDED_SECRETS: &[&str] = &[
    "GITHUB_TOKEN",
    "GH_TOKEN",
    "GH_ENTERPRISE_TOKEN",
    "GITLAB_TOKEN",
    "GL_TOKEN",
    "BITBUCKET_TOKEN",
    "NPM_TOKEN",
    "NODE_AUTH_TOKEN",
    "CARGO_REGISTRY_TOKEN",
    "PYPI_TOKEN",
    "TWINE_PASSWORD",
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "AZURE_CLIENT_SECRET",
    "GOOGLE_APPLICATION_CREDENTIALS",
    "CLOUDFLARE_API_TOKEN",
    "DIGITALOCEAN_ACCESS_TOKEN",
    "HEROKU_API_KEY",
    "VERCEL_TOKEN",
    "NETLIFY_AUTH_TOKEN",
    "FLY_API_TOKEN",
    "HF_TOKEN",
    "HUGGING_FACE_HUB_TOKEN",
    "OP_SERVICE_ACCOUNT_TOKEN",
    "VAULT_TOKEN",
    "SLACK_BOT_TOKEN",
    "DOCKER_AUTH_CONFIG",
    "SSH_AUTH_SOCK",
];

/// Whether this agent runs with the guard (the default). `"guard": false` in the
/// agent's harnessOptions turns it off.
pub fn guarded(agent: &ResolvedAgent) -> bool {
    agent.agent.harness_options["guard"].as_bool() != Some(false)
}

/// Credentials in GUARDED_SECRETS that are the agent's own model login in some
/// setups, so the guard keeps them there: Copilot signs in with a GitHub token,
/// Claude Code on Bedrock and Amazon Q use AWS keys, and Claude or Gemini on Vertex
/// use Google application credentials.
fn provider_credentials(env: &HashMap<String, String>, harness: &str) -> Vec<&'static str> {
    let on = |key: &str| {
        env.get(key)
            .map(|v| !matches!(v.trim(), "" | "0" | "false"))
            .unwrap_or(false)
    };
    let mut keep = Vec::new();
    if harness == "copilot" {
        keep.extend(["GITHUB_TOKEN", "GH_TOKEN", "GH_ENTERPRISE_TOKEN"]);
    }
    if harness == "amazonq" || on("CLAUDE_CODE_USE_BEDROCK") {
        keep.extend(["AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY", "AWS_SESSION_TOKEN"]);
    }
    if on("CLAUDE_CODE_USE_VERTEX") || on("GOOGLE_GENAI_USE_VERTEXAI") {
        keep.push("GOOGLE_APPLICATION_CREDENTIALS");
    }
    keep
}

/// The push URLs set explicitly on the remotes of the repository at `workdir`.
/// git skips pushInsteadOf for those, so the guard rewrites them with insteadOf.
fn explicit_push_urls(workdir: &Path) -> Vec<String> {
    std::process::Command::new("git")
        .args(["config", "--get-regexp", r"^remote\..*\.pushurl$"])
        .current_dir(workdir)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .map(|out| {
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter_map(|line| line.split_once(' ').map(|(_, url)| url.trim().to_string()))
                .filter(|url| !url.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// The guard for an unattended agent's environment: drop the credentials in
/// GUARDED_SECRETS (except the agent's own model login, see provider_credentials),
/// make every `git push` fail (an empty pushInsteadOf rewrites every push URL to a
/// scheme git cannot reach, and remotes in `workdir` with their own pushurl get an
/// insteadOf for that URL; fetch and pull still work), and never let git wait on a
/// password prompt. This is a guardrail against mistakes and injected instructions,
/// not a sandbox: a determined agent with a shell can still read files the operator
/// can, or change the repository's git config.
pub fn guard_environment(env: &mut HashMap<String, String>, harness: &str, workdir: &Path) {
    let keep = provider_credentials(env, harness);
    for key in GUARDED_SECRETS {
        if !keep.contains(key) {
            env.remove(*key);
        }
    }
    env.insert("GIT_TERMINAL_PROMPT".into(), "0".into());
    let mut n: usize = env
        .get("GIT_CONFIG_COUNT")
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    const BLOCKED: &str = "url.aos-blocked-git-push-ask-the-operator:///";
    let mut rules = vec![(format!("{BLOCKED}.pushInsteadOf"), String::new())];
    for url in explicit_push_urls(workdir) {
        rules.push((format!("{BLOCKED}.insteadOf"), url));
    }
    for (key, value) in rules {
        env.insert(format!("GIT_CONFIG_KEY_{n}"), key);
        env.insert(format!("GIT_CONFIG_VALUE_{n}"), value);
        n += 1;
    }
    env.insert("GIT_CONFIG_COUNT".into(), n.to_string());
}

pub fn retry_delay_ms(consecutive_failures: u32) -> u64 {
    60_000.min(2_000 * 2u64.saturating_pow(consecutive_failures.saturating_sub(1)))
}

pub fn resumed_unexpected_session(pinned: Option<&str>, observed: Option<&str>) -> bool {
    match (pinned, observed) {
        (Some(p), Some(o)) => p != o,
        _ => false,
    }
}

pub struct ProcessResult {
    pub code: i32,
    pub output: String,
    pub duration_ms: u64,
    pub timed_out: bool,
}

fn kill_group(pid: u32, signal: libc::c_int) {
    unsafe {
        if libc::kill(-(pid as i32), signal) != 0 {
            libc::kill(pid as i32, signal);
        }
    }
}

struct OutputCapture {
    head: Vec<u8>,
    tail: Vec<u8>,
}

const MAX_HEAD_BYTES: usize = 256 * 1024;
const MAX_TAIL_BYTES: usize = 8 * 1024 * 1024;

impl OutputCapture {
    fn push(&mut self, data: &[u8]) {
        if self.head.len() < MAX_HEAD_BYTES {
            self.head.extend_from_slice(data);
            return;
        }
        self.tail.extend_from_slice(data);
        if self.tail.len() > MAX_TAIL_BYTES {
            let excess = self.tail.len() - MAX_TAIL_BYTES;
            self.tail.drain(..excess);
        }
    }

    fn into_bytes(self) -> Vec<u8> {
        let mut out = self.head;
        out.extend_from_slice(&self.tail);
        out
    }
}

/// Spawn the harness CLI detached in its own process group, tee stdout+stderr
/// to ours while capturing (head 256KB + tail 8MB), SIGTERM→SIGKILL the group
/// on timeout. Port of runHarnessProcess (with the head+tail output cap).
pub fn run_harness_process(
    command: &str,
    args: &[String],
    environment: &HashMap<String, String>,
    workdir: &str,
    timeout_ms: u64,
    child_pid: &Arc<Mutex<Option<u32>>>,
) -> ProcessResult {
    let started = Instant::now();
    use std::os::unix::process::CommandExt;
    let spawn_result = Command::new(command)
        .args(args)
        .current_dir(workdir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_clear()
        .envs(environment)
        // Own process group so timeout/stop can kill the whole tree.
        .process_group(0)
        .spawn();

    let mut child: Child = match spawn_result {
        Ok(c) => c,
        Err(error) => {
            return ProcessResult {
                code: -1,
                output: format!("\nspawn error: {error}"),
                duration_ms: started.elapsed().as_millis() as u64,
                timed_out: false,
            };
        }
    };
    let pid = child.id();
    *child_pid.lock().unwrap() = Some(pid);

    let capture = Arc::new(Mutex::new(OutputCapture {
        head: Vec::new(),
        tail: Vec::new(),
    }));
    let settled = Arc::new(AtomicBool::new(false));
    let timed_out = Arc::new(AtomicBool::new(false));

    // Reader threads: append to the capture and tee to our own stdout/stderr.
    let mut stdout_reader = child.stdout.take().unwrap();
    let mut stderr_reader = child.stderr.take().unwrap();
    let capture_out = Arc::clone(&capture);
    let out_thread = std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match std::io::Read::read(&mut stdout_reader, &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    capture_out.lock().unwrap().push(&buf[..n]);
                    let _ = std::io::stdout().write_all(&buf[..n]);
                    let _ = std::io::stdout().flush();
                }
            }
        }
    });
    let capture_err = Arc::clone(&capture);
    let err_thread = std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match std::io::Read::read(&mut stderr_reader, &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    capture_err.lock().unwrap().push(&buf[..n]);
                    let _ = std::io::stderr().write_all(&buf[..n]);
                    let _ = std::io::stderr().flush();
                }
            }
        }
    });

    // Timeout watcher: SIGTERM the group, then SIGKILL after 3s if still running.
    {
        let settled = Arc::clone(&settled);
        let timed_out = Arc::clone(&timed_out);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(timeout_ms.max(1)));
            if settled.load(Ordering::SeqCst) {
                return;
            }
            timed_out.store(true, Ordering::SeqCst);
            kill_group(pid, libc::SIGTERM);
            std::thread::sleep(Duration::from_millis(3_000));
            if !settled.load(Ordering::SeqCst) {
                kill_group(pid, libc::SIGKILL);
            }
        });
    }

    let status = child.wait();
    settled.store(true, Ordering::SeqCst);
    *child_pid.lock().unwrap() = None;
    let _ = out_thread.join();
    let _ = err_thread.join();

    let code = match status {
        Ok(exit) => exit.code().unwrap_or(-1),
        Err(_) => -1,
    };
    let captured = Arc::try_unwrap(capture)
        .map(|m| m.into_inner().unwrap())
        .unwrap_or_else(|m| {
            let locked = m.lock().unwrap();
            OutputCapture {
                head: locked.head.clone(),
                tail: locked.tail.clone(),
            }
        });
    ProcessResult {
        code,
        output: String::from_utf8_lossy(&captured.into_bytes()).to_string(),
        duration_ms: started.elapsed().as_millis() as u64,
        timed_out: timed_out.load(Ordering::SeqCst),
    }
}

/// The MCP launch line handed to the vendor CLI: `qagent mcp` as this agent, on this database.
pub fn mcp_command_for(agent_id: &str, db_path: &Path, qagent_bin: &str) -> McpCommand {
    McpCommand {
        command: qagent_bin.to_string(),
        args: vec!["mcp".to_string()],
        env: HashMap::from([
            ("QAGENT_AGENT_ID".to_string(), agent_id.to_string()),
            (
                "QAGENT_BUS_DB".to_string(),
                db_path
                    .canonicalize()
                    .unwrap_or_else(|_| db_path.to_path_buf())
                    .display()
                    .to_string(),
            ),
        ]),
    }
}

/// Whether the supervisor claims and submits for this CLI — harnesses without MCP cannot call bus tools.
pub fn supervisor_managed(agent: &ResolvedAgent) -> bool {
    !agent.harness.features.mcp
}

fn render_message(message: &Message) -> String {
    let task_bit = message
        .task_id
        .map(|id| format!(" · task #{id}"))
        .unwrap_or_default();
    let refs = if message.refs.is_empty() {
        String::new()
    } else {
        let list: Vec<String> = message
            .refs
            .iter()
            .map(|r| format!("- {}: {}", r.ref_type, r.value))
            .collect();
        format!("\nReferences:\n{}", list.join("\n"))
    };
    format!(
        "-- from {} · {}{}\n{}\n\n{}{}",
        message.sender, message.msg_type, task_bit, message.subject, message.body, refs
    )
}

fn render_task(task: &Task) -> String {
    let assignee = task
        .assignee
        .as_deref()
        .map(|a| format!(" · {a}"))
        .unwrap_or_else(|| " · unassigned".to_string());
    let role = if task.role.is_empty() {
        String::new()
    } else {
        format!(" · role {}", task.role)
    };
    let brief = if task.brief.is_empty() {
        String::new()
    } else {
        format!("\n{}", task.brief)
    };
    let acceptance = if task.acceptance.is_empty() {
        String::new()
    } else {
        format!("\nAcceptance:\n{}", task.acceptance)
    };
    let scopes = if task.path_scopes.is_empty() {
        String::new()
    } else {
        format!(
            "\nPath scopes ({}): {}",
            task.project.as_deref().unwrap_or(""),
            task.path_scopes.join(", ")
        )
    };
    format!(
        "-- task #{} · {}{}{}\n{}{}{}{}",
        task.id, task.state, assignee, role, task.title, brief, acceptance, scopes
    )
}

/// The brief for one turn. It names only the v2 bus_* tools.
pub fn build_brief(
    agent: &ResolvedAgent,
    messages: &[Message],
    tasks: &[Task],
    managed: bool,
) -> String {
    let mut lines: Vec<String> = vec![
        format!(
            "=== qagent: {} new message(s){} for {} ===",
            messages.len(),
            if tasks.is_empty() {
                String::new()
            } else {
                format!(", {} task(s)", tasks.len())
            },
            agent.agent.id
        ),
        format!(
            "Role: {}; model: {}; family: {}; harness: {}.",
            agent.agent.role, agent.model.id, agent.model.family, agent.harness.id
        ),
        String::new(),
    ];
    for block in messages.iter().map(render_message) {
        lines.push(block);
        lines.push(String::new());
    }
    for block in tasks.iter().map(render_task) {
        lines.push(block);
        lines.push(String::new());
    }
    lines.push("=== end of scoped messages ===".to_string());
    lines.push(String::new());
    lines.push(
        "Do the actual work now. Retrieve only the files or evidence needed for this task."
            .to_string(),
    );
    lines.push(
        "Use file paths and artifacts for handoff instead of pasting large outputs into messages."
            .to_string(),
    );
    if managed {
        lines.push("The supervisor has claimed the task(s) above for you and will submit your final answer as the result.".to_string());
        lines.push(
            "To reach the team from your shell, you are already signed in as yourself:".to_string(),
        );
        lines.push("  \"$QAGENT_CLI\" send <agent> \"<subject>\" \"<body>\"    message a teammate (operator = the human)".to_string());
        lines.push(
            "  \"$QAGENT_CLI\" task note <N> \"<progress>\"              record progress on task N"
                .to_string(),
        );
        lines.push("End the turn with the result or your question.".to_string());
    } else {
        lines.push("Claim a task with bus_task_claim before you start it, record progress with bus_task_note,".to_string());
        lines.push("and finish with bus_task_submit (summary, changed_files, validation). Reply with bus_send;".to_string());
        lines.push("review with bus_task_review when you are the reviewer.".to_string());
        lines.push(
            "Do NOT call bus_wait: the supervisor holds the wait and will wake you again."
                .to_string(),
        );
        lines.push("End the turn after reporting the result or question.".to_string());
    }
    lines.join("\n")
}

/// An agent's role prompt (its `instructions` file, read on every turn) goes
/// before the brief, so the agent knows how to work before it reads what to do.
pub fn with_role_prompt(role: Option<String>, brief: String) -> String {
    match role {
        Some(text) if !text.trim().is_empty() => format!(
            "=== how you work (your role prompt) ===\n{}\n\n{}",
            text.trim(),
            brief
        ),
        _ => brief,
    }
}

fn cancellation_only(messages: &[Message]) -> bool {
    !messages.is_empty()
        && messages
            .iter()
            .all(|m| m.msg_type == "control" && m.subject.starts_with("[CANCELLED"))
}

fn structured_array(value: &serde_json::Value) -> Vec<serde_json::Value> {
    value.as_array().cloned().unwrap_or_default()
}

fn claimable_by(task: &Task, agent_id: &str, role: &str) -> bool {
    (task.state == "open" || task.state == "changes_requested")
        && (task.assignee.as_deref() == Some(agent_id)
            || (task.assignee.is_none() && (task.role.is_empty() || task.role == role)))
}

/// Turns that fail in a row before the agent pauses itself: a CLI that lost its
/// login, or a task it cannot do, should not burn every task and the night.
pub const MAX_FAILED_TURNS: u32 = 5;

/// Size at which an agent's log (`logs/<agent>.log`) or captured output
/// (`logs/<agent>.out`) is moved to `<file>.1`, keeping one older copy.
pub const MAX_LOG_BYTES: u64 = 10 * 1024 * 1024;

fn rotated(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".1");
    path.with_file_name(name)
}

fn rotate_log(path: &Path) {
    if fs::metadata(path).map(|m| m.len() > MAX_LOG_BYTES).unwrap_or(false) {
        let _ = fs::rename(path, rotated(path));
    }
}

/// aos starts a supervisor with stdout and stderr appended to `logs/<agent>.out`,
/// and every turn's CLI output is teed there. When that file is our stdout and
/// has grown past the cap, copy it to `.out.1` and truncate it in place (the file
/// is open in append mode, so writing carries on at the new end).
fn cap_stdout_file(path: &Path) {
    use std::os::unix::fs::MetadataExt;
    let Ok(meta) = fs::metadata(path) else {
        return;
    };
    if meta.len() <= MAX_LOG_BYTES {
        return;
    }
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(1, &mut st) } != 0
        || st.st_dev as u64 != meta.dev()
        || st.st_ino as u64 != meta.ino()
    {
        return;
    }
    let _ = std::io::stdout().flush();
    if fs::copy(path, rotated(path)).is_ok() {
        unsafe {
            libc::ftruncate(1, 0);
        }
    }
}

/// While a turn runs, renew the claims the agent holds and mark it seen once a
/// minute, so a turn longer than the claim TTL does not lose its task to another
/// agent. Runs on its own connection; errors are retried on the next beat.
struct Keepalive {
    done: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Keepalive {
    fn finish(self) {}
}

/// Stops the keepalive thread however the turn ends, a panic included.
impl Drop for Keepalive {
    fn drop(&mut self) {
        self.done.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

pub const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(60);

fn keep_claims_alive(db_path: &Path, me: &crate::identity::Identity) -> Keepalive {
    let done = Arc::new(AtomicBool::new(false));
    let handle = {
        let done = Arc::clone(&done);
        let db_path = db_path.to_path_buf();
        let me = me.clone();
        std::thread::spawn(move || {
            let mut bus: Option<Bus> = None;
            let mut next = Instant::now() + KEEPALIVE_INTERVAL;
            while !done.load(Ordering::SeqCst) {
                if Instant::now() < next {
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                }
                next = Instant::now() + KEEPALIVE_INTERVAL;
                if bus.is_none() {
                    bus = Bus::open(Some(&db_path)).ok();
                }
                if let Some(b) = &bus {
                    if b.renew_claims(&me).is_err() {
                        bus = None;
                    }
                }
            }
        })
    };
    Keepalive {
        done,
        handle: Some(handle),
    }
}

/// Pause this agent after too many failed turns in a row and tell the operator, once.
fn pause_after_failures(
    bus: &Bus,
    me: &crate::identity::Identity,
    failures: u32,
    last_error: &str,
    log: &dyn Fn(&str),
) -> Result<()> {
    let id = &me.agent_id;
    let reason = format!("{failures} turns failed in a row; last: {last_error}");
    bus.pause_agent(me, id, Some(&reason.chars().take(200).collect::<String>()))?;
    bus.send(
        me,
        crate::bus::SendInput {
            to: crate::types::OPERATOR_ID.into(),
            subject: Some(format!("{id} paused: {failures} turns failed in a row")),
            body: format!(
                "{id} paused itself after {failures} failed turns in a row, so it stops using up tasks and spend. \
                 The last one: {last_error}. Its log is logs/{id}.log under the bus folder. \
                 Fix the cause (often the CLI's login, or the CLI missing from PATH), then resume {id} in aos \
                 (or qagent agent resume {id})."
            ),
            msg_type: Some("info".into()),
            thread: None,
            task_id: None,
            refs: None,
            requires_ack: false,
        },
    )?;
    log(&format!("paused: {reason}"));
    Ok(())
}

/// Whether `pid` is a live supervisor for `agent_id`, not just any process: after
/// a reboot a stale pid file can name an unrelated process that reused the pid.
/// When the command line cannot be read, a live pid counts.
pub fn supervisor_alive(pid: i32, agent_id: &str) -> bool {
    process_running(pid, &["supervise", agent_id])
}

/// Whether `pid` is alive and its command line has every word in `words`.
/// When the command line cannot be read, a live pid counts.
pub fn process_running(pid: i32, words: &[&str]) -> bool {
    if pid <= 0 {
        return false;
    }
    let alive = unsafe { libc::kill(pid, 0) } == 0
        || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
    if !alive {
        return false;
    }
    if pid as u32 == std::process::id() {
        return true;
    }
    let args: Option<Vec<String>> = if cfg!(target_os = "linux") {
        fs::read(format!("/proc/{pid}/cmdline")).ok().map(|raw| {
            raw.split(|b| *b == 0)
                .map(|a| String::from_utf8_lossy(a).to_string())
                .collect()
        })
    } else {
        // No /proc: ask ps once per live pid (aos checks on every screen refresh).
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
    };
    match args {
        Some(args) if !args.is_empty() => words.iter().all(|w| args.iter().any(|a| a == w)),
        _ => true,
    }
}

/// The project configuration's limits for this agent, as the policy the bus core
/// enforces: delegation, the agents it may assign, delegation depth, and how many
/// tasks it may hold claimed at once. Mirror: policyFromConfig in src/supervisor.ts on
/// main (#29); the TypeScript copy on rust-port does not have it yet.
pub fn policy_from_config(
    config: &crate::config::BusConfig,
    agent: &crate::config::AgentDef,
) -> crate::identity::AgentPolicy {
    let number = |v: &serde_json::Value| v.as_f64().filter(|n| *n >= 0.0).map(|n| n as i64);
    let depth = match (
        number(&agent.permissions["maxDelegationDepth"]),
        number(&config.constraints["maxDelegationDepth"]),
    ) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    crate::identity::AgentPolicy {
        can_delegate: agent.permissions["canDelegate"].as_bool(),
        allowed_child_agent_ids: agent.permissions["allowedChildAgentIds"]
            .as_array()
            .map(|ids| ids.iter().filter_map(|id| id.as_str().map(str::to_string)).collect::<Vec<_>>())
            .filter(|ids| !ids.is_empty()),
        max_delegation_depth: depth,
        max_concurrent_tasks: number(&config.constraints["maxConcurrentTasks"]).filter(|n| *n >= 1),
    }
}

/// Why the configuration's usage budget (`optionalTokenBudget`,
/// `optionalApiCostBudgetUSD`) stops new turns, or None. It counts what the CLI
/// itself reported and is checked between turns, so one turn can overshoot it, and a
/// CLI that reports no cost never moves the dollar count. Counted per agent from its
/// session file. Mirror: budgetReached in src/supervisor.ts on main (#29).
pub fn config_budget_reached(
    config: &crate::config::BusConfig,
    total_tokens: f64,
    cost_usd: f64,
) -> Option<String> {
    if let Some(tokens) = config.constraints["optionalTokenBudget"].as_f64() {
        if total_tokens >= tokens {
            return Some(format!(
                "{} of {} reported tokens used",
                crate::control::short(total_tokens),
                crate::control::short(tokens)
            ));
        }
    }
    if let Some(dollars) = config.constraints["optionalApiCostBudgetUSD"].as_f64() {
        if cost_usd >= dollars {
            return Some(format!("${cost_usd:.2} of ${dollars:.2} reported cost used"));
        }
    }
    None
}

/// Why this build refuses work that asked for per-task git worktrees.
pub const NO_WORKTREES: &str = "worktree isolation is not available in this build (the Rust qagent/aos); \
     nothing was claimed or started. Use the TypeScript qagent for worktrees, or set \
     constraints.isolation to \"path-locks\" in the config";

fn read_pid(path: &Path) -> i32 {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

/// Write `<pid>\n` to a private file and hard-link it to `path`: atomic, and it fails
/// with AlreadyExists while `path` exists, so the file is never seen half written.
fn link_pid_file(path: &Path, suffix: &str) -> std::io::Result<()> {
    let mine = path.with_file_name(format!(
        "{}.{}.{suffix}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id()
    ));
    fs::write(&mine, format!("{}\n", std::process::id()))?;
    let _ = fs::set_permissions(&mine, std::os::unix::fs::PermissionsExt::from_mode(0o600));
    let linked = fs::hard_link(&mine, path);
    let _ = fs::remove_file(&mine);
    linked
}

/// One pid file per agent so two supervisors never drive the same CLI session.
///
/// Taking it is atomic: the pid is hard-linked into place, which fails if any
/// supervisor holds it, so two starters cannot both win. A stale file (its pid is
/// gone) is removed only while holding `<agent>.pid.reap`, so a supervisor that just
/// took the lock is never removed by a slower starter. acquireSupervisorLock in
/// src/supervisor.ts on main (#29) uses the same protocol, so those two contend safely
/// on one file; the TypeScript copy on rust-port still checks then writes, and can
/// overwrite a lock held here.
fn acquire_lock(dir: &Path, agent_id: &str) -> Result<impl FnOnce()> {
    fs::create_dir_all(dir)?;
    let path = dir.join(format!("{agent_id}.pid"));
    let my_pid = std::process::id() as i32;
    let release = {
        let path = path.clone();
        move || {
            if read_pid(&path) == my_pid {
                let _ = fs::remove_file(&path);
            }
        }
    };
    for _ in 0..20 {
        match link_pid_file(&path, "lock") {
            Ok(()) => return Ok(release),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        let holder = read_pid(&path);
        if holder == my_pid {
            return Ok(release);
        }
        if holder != 0 && supervisor_alive(holder, agent_id) {
            return Err(BusError::conflict(format!(
                "a supervisor for {agent_id} is already running (pid {holder})"
            )));
        }
        reap_stale_lock(&path, holder)?;
    }
    Err(BusError::conflict(format!(
        "could not take the supervisor lock for {agent_id}; another supervisor keeps starting"
    )))
}

fn reap_stale_lock(path: &Path, stale_pid: i32) -> Result<()> {
    let reap = path.with_file_name(format!(
        "{}.reap",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    match link_pid_file(&reap, "reap") {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            // Another starter is reaping. A reaper that died midway leaves its file
            // behind; clear it once it is clearly abandoned. A pid of 0 means the file
            // is already gone (it is only ever linked in whole), and removing the path
            // then could delete a newer reaper's file and let two reap at once.
            let reaper = read_pid(&reap);
            let age = fs::metadata(&reap)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .unwrap_or_default();
            let reaper_alive = reaper > 0 && unsafe { libc::kill(reaper, 0) } == 0;
            if reaper != 0 && !reaper_alive && age > Duration::from_secs(2) {
                let _ = fs::remove_file(&reap);
            }
            std::thread::sleep(Duration::from_millis(5));
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    }
    // Holding the reap lock nobody else removes the pid file, and only a link creates
    // it, which fails while it exists.
    if read_pid(path) == stale_pid {
        let _ = fs::remove_file(path);
    }
    let _ = fs::remove_file(&reap);
    Ok(())
}

fn sleep_interruptible(ms: u64, stop: &AtomicBool) {
    let deadline = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < deadline {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        std::thread::sleep(
            Duration::from_millis(25).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct SessionRecord {
    session_id: Option<String>,
    turns: i64,
    input_tokens: f64,
    output_tokens: f64,
    total_tokens: f64,
    #[serde(rename = "costUSD")]
    cost_usd: f64,
    latency_ms: f64,
}

fn read_session(path: &Path) -> SessionRecord {
    fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

pub type LogFn = dyn Fn(&str) + Send;

/// How one round of the supervise loop ended.
enum Round {
    Next,
    Stop,
}

/// True while this agent is paused. When its budget has run out, the agent pauses
/// itself here and tells the operator, once.
fn hold_for_pause(bus: &Bus, me: &crate::identity::Identity, log: &dyn Fn(&str)) -> Result<bool> {
    let Some(agent) = bus.get_agent(&me.agent_id)? else {
        return Ok(false);
    };
    if crate::control::paused(&agent.meta).is_some() {
        return Ok(true);
    }
    let Some(over) = bus.budget_of(&agent).and_then(|b| b.over()) else {
        return Ok(false);
    };
    let id = &me.agent_id;
    bus.pause_agent(me, id, Some(&format!("budget reached: {over}")))?;
    bus.send(
        me,
        crate::bus::SendInput {
            to: crate::types::OPERATOR_ID.into(),
            subject: Some(format!("{id} paused: budget reached ({over})")),
            body: format!(
                "{id} used its budget ({over}) and paused itself before starting another turn. \
                 Open work stays where it is. To carry on: resume {id} in aos (or qagent agent resume {id}). \
                 To change the budget: budget {id} 20 turns 60 min in aos, or budget {id} off."
            ),
            msg_type: Some("info".into()),
            thread: None,
            task_id: None,
            refs: None,
            requires_ack: false,
        },
    )?;
    log(&format!("paused: budget reached ({over})"));
    Ok(true)
}

pub struct SuperviseOptions {
    pub agent_id: String,
    pub workdir: String,
    pub db_path: PathBuf,
    /// Harness configuration file; defaults to configPathFromProject(workdir).
    pub config_path: Option<PathBuf>,
    /// Stops the loop and kills the running CLI's process group.
    pub stop: Arc<AtomicBool>,
    /// Length of each blocking wait before it is renewed. Default DEFAULT_WAIT_SEC.
    pub wait_ms: Option<u64>,
    /// First retry delay after a failed turn or round, doubling up to 30x. Default 2000.
    pub retry_base_ms: Option<u64>,
    pub fake_harness_path: Option<String>,
    /// The qagent binary MCP/fake harnesses run (tests inject the built binary).
    pub qagent_bin: Option<String>,
    /// Optional log sink (tests); defaults to stdout.
    pub log: Option<Box<LogFn>>,
}

/// Supervise one agent until `stop` is set. The agent must exist on the bus
/// (token file) and in the harness configuration.
pub fn supervise(options: SuperviseOptions) -> Result<()> {
    let workdir =
        std::fs::canonicalize(&options.workdir).unwrap_or_else(|_| PathBuf::from(&options.workdir));
    let mut bus = Bus::open(Some(&options.db_path))?;
    let home = home_for(&bus.db_path);
    let log_dir = home.join("logs");
    let session_dir = home.join("sessions");
    fs::create_dir_all(&log_dir)?;
    fs::create_dir_all(&session_dir)?;

    let write = options
        .log
        .unwrap_or_else(|| Box::new(|line: &str| println!("{line}")));
    let log_path = log_dir.join(format!("{}.log", options.agent_id));
    let out_path = log_dir.join(format!("{}.out", options.agent_id));
    let log = move |line: &str| {
        let stamped = format!(
            "[{}] {}",
            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            line
        );
        write(&stamped);
        rotate_log(&log_path);
        if let Ok(mut f) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
        {
            let _ = writeln!(f, "{stamped}");
        }
    };

    let child_pid: Arc<Mutex<Option<u32>>> = Arc::new(Mutex::new(None));
    let mut release: Option<Box<dyn FnOnce()>> = None;

    // Abort listener: stop -> SIGTERM the running CLI's process group (SIGKILL after 3s).
    {
        let stop = options.stop.clone();
        let child_pid = Arc::clone(&child_pid);
        std::thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(50));
            }
            let pid = *child_pid.lock().unwrap();
            if let Some(pid) = pid {
                kill_group(pid, libc::SIGTERM);
                std::thread::sleep(Duration::from_millis(3_000));
                if child_pid.lock().unwrap().map(|p| p == pid).unwrap_or(false) {
                    kill_group(pid, libc::SIGKILL);
                }
            }
        });
    }

    let result = (|| -> Result<()> {
        let me = bus.identify(Some(&options.agent_id))?;
        let bus_agent = bus.get_agent(&options.agent_id)?;
        if bus_agent.is_none() {
            return Err(BusError::not_found(format!(
                "agent {} is not on the bus; add it with `qagent agent add {}`",
                options.agent_id, options.agent_id
            )));
        }
        let bus_agent = bus_agent.unwrap();
        let config_path = options.config_path.clone().unwrap_or_else(|| {
            config_path_from_project(&workdir, &|name| std::env::var(name).ok())
        });
        let config = load_config(&config_path)?;
        let agent = resolve_agent(&config, &options.agent_id)?;
        if !agent.agent.enabled {
            return Err(BusError::invalid(format!(
                "agent {} is disabled in the harness configuration",
                agent.agent.id
            )));
        }
        if config.constraints["isolation"].as_str() == Some("worktree") {
            // Fail closed: running in the shared checkout is what isolation was asked to prevent.
            return Err(BusError::invalid(NO_WORKTREES));
        }
        release = Some(Box::new(acquire_lock(
            &home.join("supervisors"),
            &agent.agent.id,
        )?));

        let adapter = get_harness_adapter(&agent.harness.adapter)?;
        let managed = supervisor_managed(&agent);
        let session_path = session_dir.join(format!("{}.json", agent.agent.id));
        let mut session = read_session(&session_path);
        let pinned_session_id = agent
            .agent
            .resume_session_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        if let Some(pinned) = &pinned_session_id {
            if session.session_id.as_deref() != Some(pinned.as_str()) {
                session.session_id = Some(pinned.clone());
                fs::write(
                    &session_path,
                    serde_json::to_string_pretty(&session).unwrap(),
                )?;
            }
        }
        let dollar_budget = config.constraints["optionalApiCostBudgetUSD"].as_f64();
        if dollar_budget.is_some() && !agent.harness.features.usage_reporting {
            return Err(BusError::invalid(format!(
                "optionalApiCostBudgetUSD is set, but {} reports no usage, so the budget could never be counted. \
                 Remove it or use a CLI that reports cost.",
                agent.harness.id
            )));
        }
        if !agent.harness.features.usage_reporting
            && bus.budget_of(&bus_agent).is_some_and(|b| b.limits.usd.is_some())
        {
            log(&format!(
                "{} reports no usage, so the dollar part of {}'s budget never counts; its turn and minute limits still apply",
                agent.harness.id, agent.agent.id
            ));
        }
        // The configuration's limits go into the bus, where every Rust call path (MCP,
        // CLI, this supervisor) and the TypeScript qagent on main enforce them.
        // Failing to apply them fails the start: running unenforced is worse than not running.
        let operator = bus.identify(Some(crate::types::OPERATOR_ID)).ok();
        bus.set_agent_policy(
            operator.as_ref().unwrap_or(&me),
            &me.agent_id,
            Some(&policy_from_config(&config, agent.agent)),
        )?;
        let qagent_bin = options.qagent_bin.clone().unwrap_or_else(|| {
            std::env::current_exe()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| "qagent".to_string())
        });
        let mcp_command = mcp_command_for(&me.agent_id, &bus.db_path, &qagent_bin);
        let block_sec = if agent.harness.id == "claude" {
            "900"
        } else {
            "240"
        };
        let wait_ms = options.wait_ms.unwrap_or(DEFAULT_WAIT_SEC as u64 * 1000);
        let mut consecutive_failures: u32 = 0;
        // Turns whose usage could not be saved (a full disk). The budget reads that
        // file, so after MAX_FAILED_TURNS of these the agent pauses instead.
        let mut unrecorded_turns: u32 = 0;
        // The last message seq a finished turn used. If marking it read failed, the
        // mail is still unread on the bus, but it is not handed to another turn.
        let mut used_through: i64 = 0;
        let retry_base_ms = options.retry_base_ms.unwrap_or(2_000);
        let backoff = |failures: u32| retry_delay_ms(failures) * retry_base_ms / 2_000;
        log(&format!(
            "supervising {} via {} in {} (bus {}{})",
            agent.agent.id,
            agent.harness.id,
            workdir.display(),
            bus.db_path.display(),
            if managed {
                ", supervisor-managed tasks"
            } else {
                ""
            }
        ));

        let mut budget_noticed: Option<String> = None;
        let mut cost_noticed = false;
        let mut paused_since: Option<Instant> = None;
        let mut last_beat = Instant::now();
        // Bus or file errors outside a turn (a locked database, a full disk) end
        // the round, not the supervisor: it logs, backs off and tries again.
        let mut error_streak: u32 = 0;
        // Queued work that no fresh mail or event will announce: what was waiting before
        // this supervisor started, or was queued while the agent was paused or mid-turn.
        // Checked before the next wait instead of after a whole wait period.
        let mut backlog_due = true;
        while !options.stop.load(Ordering::SeqCst) {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<Round> {
                // Pause and budget come first, so a paused agent's mail stays unread for later.
                if hold_for_pause(&bus, &me, &log)? {
                    if paused_since.is_none() {
                        log("paused; no new turn until the operator resumes this agent");
                        bus.set_status(&me, "idle")?;
                        paused_since = Some(Instant::now());
                    }
                    if last_beat.elapsed() >= Duration::from_secs(60) {
                        bus.heartbeat(&me)?;
                        last_beat = Instant::now();
                    }
                    let until = Instant::now() + Duration::from_secs(2);
                    while Instant::now() < until && !options.stop.load(Ordering::SeqCst) {
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    return Ok(Round::Next);
                }
                // The configuration's usage budget: counted from what the CLI reported into
                // sessions/<agent>.json and checked between turns, so one turn can overshoot it.
                if let Some(over) = config_budget_reached(&config, session.total_tokens, session.cost_usd) {
                    if budget_noticed.as_deref() != Some(over.as_str()) {
                        log(&format!(
                            "budget reached ({over}); no new turns until the configuration budget is raised"
                        ));
                        budget_noticed = Some(over);
                    }
                    sleep_interruptible(wait_ms, &options.stop);
                    return Ok(Round::Next);
                }
                if paused_since.take().is_some() {
                    log("resumed");
                    consecutive_failures = 0;
                    backlog_due = true;
                }
                let backlog = std::mem::take(&mut backlog_due)
                    && bus.unread_count(&me.agent_id)? == 0
                    && bus.has_claimable(&me.agent_id, &bus_agent.role)?;
                let waited = if backlog {
                    log("queued work is waiting; starting without new mail");
                    crate::bus::WaitResult {
                        status: "backlog".into(),
                        messages: vec![],
                        events: vec![],
                        seq: bus.latest_seq()?,
                    }
                } else {
                    crate::wait::wait_for_mail(
                        &bus,
                        &me,
                        Duration::from_millis(wait_ms),
                        &options.stop,
                    )?
                };
                if options.stop.load(Ordering::SeqCst) {
                    return Ok(Round::Stop);
                }
                // A wait that ends with no mail ("none") falls through: the queue is
                // checked below, so claimable work never needs fresh mail.
                if hold_for_pause(&bus, &me, &log)? {
                    return Ok(Round::Next);
                }

                // Read what the wait saw without consuming it: the mail is marked read
                // only once a turn has used it, so a turn that dies does not lose it.
                let messages = if waited.status == "mail" {
                    bus.inbox(&me, true, Some(50))?.messages
                } else {
                    Vec::new()
                };
                let already_used = messages.iter().filter(|m| m.seq <= used_through).count();
                if already_used > 0 {
                    // A turn used this mail but marking it read failed: mark it now
                    // instead of paying for the same turn again.
                    let last = messages.iter().filter(|m| m.seq <= used_through).last().unwrap();
                    bus.mark_read_through(&me, last.seq, already_used)?;
                    return Ok(Round::Next);
                }
                let mark_read = |bus: &Bus| -> Result<()> {
                    match messages.last() {
                        Some(last) => bus.mark_read_through(&me, last.seq, messages.len()),
                        None => Ok(()),
                    }
                };
                if cancellation_only(&messages) {
                    mark_read(&bus)?;
                    log("received cancellation control; no model turn started");
                    return Ok(Round::Next);
                }

                let role = bus_agent.role.clone();
                let mut task_ids: std::collections::BTreeSet<i64> = Default::default();
                for message in &messages {
                    if let Some(task_id) = message.task_id {
                        if (message.msg_type == "task" || message.msg_type == "feedback")
                            && !message.subject.starts_with("[ACCEPTED")
                        {
                            task_ids.insert(task_id);
                        }
                    }
                }
                let mut tasks: Vec<Task> = Vec::new();
                if managed {
                    // Port of /task/start: claim for a CLI that has no bus tools.
                    for id in &task_ids {
                        match bus.get_task(*id) {
                            Ok(current) => {
                                if claimable_by(&current.task, &me.agent_id, &role) {
                                    match bus.claim_task(&me, Some(*id)) {
                                        Ok(task) => tasks.push(task),
                                        Err(error) => {
                                            log(&format!("claim of task #{id} failed: {error}"))
                                        }
                                    }
                                } else if current.task.state == "claimed"
                                    && current.task.assignee.as_deref() == Some(me.agent_id.as_str())
                                {
                                    tasks.push(current.task);
                                }
                            }
                            Err(error) => log(&format!("claim of task #{id} failed: {error}")),
                        }
                    }
                    if messages.is_empty() {
                        match bus.claim_task(&me, None) {
                            Ok(task) => tasks.push(task),
                            // Nothing to claim, or the agent already holds its claim limit.
                            Err(error) => {
                                if !matches!(error.code.as_str(), "not_found" | "conflict") {
                                    return Err(error);
                                }
                            }
                        }
                    }
                } else if messages.is_empty() && bus.has_claimable(&me.agent_id, &bus_agent.role)? {
                    // has_claimable is false at the claim limit, so a full agent gets no turn.
                    tasks.extend(
                        bus.list_tasks(ListTasksInput {
                            states: Some(vec!["open".to_string(), "changes_requested".to_string()]),
                            limit: Some(50),
                            ..Default::default()
                        })?
                        .into_iter()
                        .filter(|task| claimable_by(task, &me.agent_id, &role)),
                    );
                }
                if messages.is_empty() && tasks.is_empty() {
                    return Ok(Round::Next);
                }

                let context = AdapterContext {
                    agent: &agent,
                    qagent_bin: qagent_bin.clone(),
                    prompt: with_role_prompt(
                        crate::aos::crew::role_prompt(&config_path, &agent.agent.id),
                        build_brief(&agent, &messages, &tasks, managed),
                    ),
                    session_id: session.session_id.clone(),
                    pinned_session_id: pinned_session_id.clone(),
                    workdir: workdir.display().to_string(),
                    mcp_server_path: "mcp".to_string(),
                    fake_harness_path: options
                        .fake_harness_path
                        .clone()
                        .unwrap_or_else(|| "fake-harness".to_string()),
                    bus_environment: HashMap::from([
                        ("QAGENT_AGENT_ID".to_string(), me.agent_id.clone()),
                        (
                            "QAGENT_BUS_DB".to_string(),
                            bus.db_path.display().to_string(),
                        ),
                        ("QAGENT_BLOCK_SEC".to_string(), block_sec.to_string()),
                        ("AGENT_BUS_BLOCK_SEC".to_string(), block_sec.to_string()),
                        // A CLI without bus tools reaches the bus from its shell with this binary.
                        ("QAGENT_CLI".to_string(), qagent_bin.clone()),
                    ]),
                    mcp_command: Some(mcp_command.clone()),
                };
                if let Some(prepare) = adapter.prepare {
                    prepare(&context)?;
                }
                let invocation = (adapter.build)(&context);
                bus.set_status(&me, "working")?;
                let keepalive = keep_claims_alive(&bus.db_path, &me);
                let mut environment = sanitized_environment(&agent, &invocation.environment);
                if guarded(&agent) {
                    guard_environment(&mut environment, &agent.harness.id, &workdir);
                }
                let process_result = run_harness_process(
                    &invocation.command,
                    &invocation.args,
                    &environment,
                    &workdir.display().to_string(),
                    invocation.timeout_ms,
                    &child_pid,
                );
                keepalive.finish();
                cap_stdout_file(&out_path);
                *child_pid.lock().unwrap() = None;
                // From here a paid turn has run: an error must not make the round
                // fail and run the same mail again, so these steps only log.
                match bus.get_agent(&me.agent_id) {
                    Ok(Some(a)) if a.stored_status == "working" => {
                        if let Err(error) = bus.set_status(&me, "idle") {
                            log(&format!("could not set status idle: {error}"));
                        }
                    }
                    Ok(_) => {}
                    Err(error) => log(&format!("could not read agent status: {error}")),
                }
                if options.stop.load(Ordering::SeqCst) {
                    log(&format!(
                        "stopped during a turn; {} process group killed",
                        agent.harness.id
                    ));
                    return Ok(Round::Stop);
                }
                let normalized = (adapter.parse)(&process_result.output, process_result.code);
                let session_mismatch = resumed_unexpected_session(
                    pinned_session_id.as_deref(),
                    normalized.session_id.as_deref(),
                );
                session.turns += 1;
                session.input_tokens += normalized.usage.input_tokens;
                session.output_tokens += normalized.usage.output_tokens;
                session.total_tokens += normalized.usage.total_tokens;
                session.cost_usd += normalized.usage.cost_usd;
                if let Some(dollars) = dollar_budget {
                    if !cost_noticed && normalized.usage.cost_usd == 0.0 {
                        cost_noticed = true;
                        log(&format!(
                            "{} reported no cost for this turn; the ${dollars} budget only counts cost the CLI reports",
                            agent.harness.id
                        ));
                    }
                }
                session.latency_ms += process_result.duration_ms as f64;
                if let Some(pinned) = &pinned_session_id {
                    session.session_id = Some(pinned.clone());
                } else if let Some(id) = normalized.session_id.clone() {
                    session.session_id = Some(id);
                }
                match fs::write(
                    &session_path,
                    serde_json::to_string_pretty(&session).unwrap(),
                ) {
                    Ok(()) => unrecorded_turns = 0,
                    Err(error) => {
                        unrecorded_turns += 1;
                        log(&format!("could not save usage to {}: {error}", session_path.display()));
                    }
                }
                if unrecorded_turns >= MAX_FAILED_TURNS {
                    unrecorded_turns = 0;
                    let reason = format!(
                        "usage could not be saved to {} after {MAX_FAILED_TURNS} turns, so the budget cannot be enforced",
                        session_path.display()
                    );
                    if let Err(error) =
                        pause_after_failures(&bus, &me, MAX_FAILED_TURNS, &reason, &log)
                    {
                        log(&format!("could not pause: {error}"));
                    }
                }

                let mut report_ids: std::collections::BTreeSet<i64> = task_ids;
                report_ids.extend(tasks.iter().map(|t| t.id));
                let failed = process_result.code != 0
                    || process_result.timed_out
                    || normalized.malformed
                    || session_mismatch;
                if failed {
                    consecutive_failures += 1;
                    let error = if process_result.timed_out {
                        format!("harness timed out after {} ms", process_result.duration_ms)
                    } else if session_mismatch {
                        format!(
                            "harness resumed unexpected session {}; expected {}",
                            normalized.session_id.as_deref().unwrap_or(""),
                            pinned_session_id.as_deref().unwrap_or("")
                        )
                    } else if normalized.malformed {
                        "harness returned malformed output".to_string()
                    } else {
                        format!("harness exited {}", process_result.code)
                    };
                    // Each task this turn still holds is failed back (retry or escalate).
                    let mut failed_back: std::collections::BTreeSet<i64> = Default::default();
                    for id in &report_ids {
                        match bus.get_task(*id) {
                            Ok(task) => {
                                if task.task.assignee.as_deref() == Some(me.agent_id.as_str())
                                    && task.task.state == "claimed"
                                {
                                    match bus.fail_task(&me, *id, &format!("supervisor: {error}")) {
                                        Ok(_) => {
                                            failed_back.insert(*id);
                                        }
                                        Err(fail_error) => log(&format!(
                                            "failure report on task #{id} rejected: {fail_error}"
                                        )),
                                    }
                                }
                            }
                            Err(fail_error) => log(&format!(
                                "failure report on task #{id} rejected: {fail_error}"
                            )),
                        }
                    }
                    // Mail about tasks that were failed back is carried on by their
                    // retry messages; anything else stays unread for the next turn.
                    if messages
                        .iter()
                        .all(|m| m.task_id.is_some_and(|id| failed_back.contains(&id)))
                    {
                        if let Some(last) = messages.last() {
                            used_through = last.seq;
                        }
                        if let Err(error) = mark_read(&bus) {
                            log(&format!("could not mark mail read: {error}"));
                        }
                    }
                    if consecutive_failures >= MAX_FAILED_TURNS {
                        if let Err(pause_error) =
                            pause_after_failures(&bus, &me, consecutive_failures, &error, &log)
                        {
                            log(&format!("could not pause: {pause_error}"));
                            sleep_interruptible(backoff(consecutive_failures), &options.stop);
                        }
                        consecutive_failures = 0;
                        return Ok(Round::Next);
                    }
                    let delay = backoff(consecutive_failures);
                    log(&format!("{error}; backing off {}s", delay / 1000));
                    sleep_interruptible(delay, &options.stop);
                    return Ok(Round::Next);
                }

                consecutive_failures = 0;
                if let Some(last) = messages.last() {
                    used_through = last.seq;
                }
                if let Err(error) = mark_read(&bus) {
                    log(&format!("could not mark mail read: {error}"));
                }
                // Work queued during the turn announced itself to nobody. A managed agent
                // claims it next round; an agent with bus tools sees it at its next wait's end.
                backlog_due = managed;
                if invocation.auto_report {
                    let structured = normalized.structured.unwrap_or(serde_json::json!({}));
                    for id in &report_ids {
                        match bus.get_task(*id) {
                            Ok(task) => {
                                // A CLI that submitted through its own bus tools leaves nothing to report.
                                if task.task.assignee.as_deref() != Some(me.agent_id.as_str())
                                    || task.task.state != "claimed"
                                {
                                    continue;
                                }
                                if let Err(error) = bus.submit_task(&me, *id, SubmitInput {
                                    summary: normalized.text.chars().take(20_000).collect(),
                                    details: Some(
                                        "auto-submitted by the supervisor for a harness without bus tool calls".to_string(),
                                    ),
                                    changed_files: structured_array(&structured["changedFiles"])
                                        .iter()
                                        .map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string()))
                                        .collect(),
                                    artifacts: Some(serde_json::Value::Array(structured_array(&structured["artifacts"]))),
                                    validation: Some(serde_json::Value::Array(structured_array(&structured["validation"]))),
                                }) {
                                    log(&format!("auto-submit failed for task #{id}: {error}"));
                                }
                            }
                            Err(error) => log(&format!("auto-submit failed for task #{id}: {error}")),
                        }
                    }
                }
                log(&format!(
                    "turn complete in {} ms",
                    process_result.duration_ms
                ));
                Ok(Round::Next)
            }));
            match outcome {
                Ok(Ok(Round::Next)) => error_streak = 0,
                Ok(Ok(Round::Stop)) => break,
                Ok(Err(error)) => {
                    error_streak += 1;
                    let delay = backoff(error_streak);
                    log(&format!("round failed: {error}; retrying in {}s", delay / 1000));
                    sleep_interruptible(delay, &options.stop);
                }
                Err(panic) => {
                    error_streak += 1;
                    let delay = backoff(error_streak);
                    let what = panic
                        .downcast_ref::<String>()
                        .cloned()
                        .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
                        .unwrap_or_else(|| "unknown panic".into());
                    log(&format!("round panicked: {what}; reopening the bus, retrying in {}s", delay / 1000));
                    sleep_interruptible(delay, &options.stop);
                    // A panic can leave a transaction half open on this connection.
                    match Bus::open(Some(&options.db_path)) {
                        Ok(fresh) => bus = fresh,
                        Err(error) => log(&format!("could not reopen the bus: {error}")),
                    }
                }
            }
        }
        Ok(())
    })();

    // finally: kill the running CLI's process group, release the lock, close.
    if let Some(pid) = child_pid.lock().unwrap().take() {
        kill_group(pid, libc::SIGTERM);
        let pid_arc = Arc::clone(&child_pid);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(3_000));
            if pid_arc.lock().unwrap().is_none() {
                kill_group(pid, libc::SIGKILL);
            }
        });
    }
    if let Some(release) = release.take() {
        release();
    }
    log("supervisor stopped");
    result
}
