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

/// One pid file per agent so two supervisors never drive the same CLI session.
fn acquire_lock(dir: &Path, agent_id: &str) -> Result<impl FnOnce()> {
    fs::create_dir_all(dir)?;
    let path = dir.join(format!("{agent_id}.pid"));
    let holder: i32 = fs::read_to_string(&path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    let my_pid = std::process::id() as i32;
    if holder != 0 && holder != my_pid {
        let alive = unsafe { libc::kill(holder, 0) } == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
        if alive {
            return Err(BusError::conflict(format!(
                "a supervisor for {agent_id} is already running (pid {holder})"
            )));
        }
    }
    fs::write(&path, format!("{}\n", my_pid))?;
    Ok(move || {
        if fs::read_to_string(&path)
            .ok()
            .and_then(|s| s.trim().parse::<i32>().ok())
            == Some(my_pid)
        {
            let _ = fs::remove_file(&path);
        }
    })
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
    let bus = Bus::open(Some(&options.db_path))?;
    let home = home_for(&bus.db_path);
    let log_dir = home.join("logs");
    let session_dir = home.join("sessions");
    fs::create_dir_all(&log_dir)?;
    fs::create_dir_all(&session_dir)?;

    let write = options
        .log
        .unwrap_or_else(|| Box::new(|line: &str| println!("{line}")));
    let log_path = log_dir.join(format!("{}.log", options.agent_id));
    let log = move |line: &str| {
        let stamped = format!(
            "[{}] {}",
            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            line
        );
        write(&stamped);
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
        let config = load_config(&options.config_path.clone().unwrap_or_else(|| {
            config_path_from_project(&workdir, &|name| std::env::var(name).ok())
        }))?;
        let agent = resolve_agent(&config, &options.agent_id)?;
        if !agent.agent.enabled {
            return Err(BusError::invalid(format!(
                "agent {} is disabled in the harness configuration",
                agent.agent.id
            )));
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

        while !options.stop.load(Ordering::SeqCst) {
            let waited = crate::wait::wait_for_mail(
                &bus,
                &me,
                Duration::from_millis(wait_ms),
                &options.stop,
            )?;
            if options.stop.load(Ordering::SeqCst) {
                break;
            }
            if waited.status == "timeout" {
                continue;
            }

            // Consume what the wait saw, so the next wait does not deliver it again.
            let messages = if waited.status == "mail" {
                bus.inbox(&me, false, Some(50))?.messages
            } else {
                Vec::new()
            };
            if cancellation_only(&messages) {
                log("received cancellation control; no model turn started");
                continue;
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
                        Err(error) => {
                            if error.code.as_str() != "not_found" {
                                return Err(error);
                            }
                        }
                    }
                }
            } else if messages.is_empty() {
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
                continue;
            }

            let context = AdapterContext {
                agent: &agent,
                qagent_bin: qagent_bin.clone(),
                prompt: build_brief(&agent, &messages, &tasks, managed),
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
                ]),
                mcp_command: Some(mcp_command.clone()),
            };
            if let Some(prepare) = adapter.prepare {
                prepare(&context)?;
            }
            let invocation = (adapter.build)(&context);
            bus.set_status(&me, "working")?;
            let process_result = run_harness_process(
                &invocation.command,
                &invocation.args,
                &sanitized_environment(&agent, &invocation.environment),
                &workdir.display().to_string(),
                invocation.timeout_ms,
                &child_pid,
            );
            *child_pid.lock().unwrap() = None;
            if bus
                .get_agent(&me.agent_id)?
                .map(|a| a.stored_status == "working")
                .unwrap_or(false)
            {
                bus.set_status(&me, "idle")?;
            }
            if options.stop.load(Ordering::SeqCst) {
                log(&format!(
                    "stopped during a turn; {} process group killed",
                    agent.harness.id
                ));
                break;
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
            session.latency_ms += process_result.duration_ms as f64;
            if let Some(pinned) = &pinned_session_id {
                session.session_id = Some(pinned.clone());
            } else if let Some(id) = normalized.session_id.clone() {
                session.session_id = Some(id);
            }
            fs::write(
                &session_path,
                serde_json::to_string_pretty(&session).unwrap(),
            )?;

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
                for id in &report_ids {
                    match bus.get_task(*id) {
                        Ok(task) => {
                            if task.task.assignee.as_deref() == Some(me.agent_id.as_str())
                                && task.task.state == "claimed"
                            {
                                if let Err(fail_error) =
                                    bus.fail_task(&me, *id, &format!("supervisor: {error}"))
                                {
                                    log(&format!(
                                        "failure report on task #{id} rejected: {fail_error}"
                                    ));
                                }
                            }
                        }
                        Err(fail_error) => log(&format!(
                            "failure report on task #{id} rejected: {fail_error}"
                        )),
                    }
                }
                let delay = retry_delay_ms(consecutive_failures);
                log(&format!("{error}; backing off {}s", delay / 1000));
                sleep_interruptible(delay, &options.stop);
                continue;
            }

            consecutive_failures = 0;
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
