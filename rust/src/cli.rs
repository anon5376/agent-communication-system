//! qagent command dispatch — mirrors src/cli/main.ts. Every command opens the
//! SQLite file directly; there is no broker.

use crate::bus::{Bus, CreateTaskInput, ListTasksInput, SendInput, SubmitInput};
use crate::db::{home_for, resolve_db_path_with};
use crate::error::{BusError, Code, Result};
use crate::identity::{self, Identity};
use crate::import::{default_import_sources, run_import, ImportOptions, ImportSources};
use crate::render::*;
use crate::types::OPERATOR_ID;
use crate::wait;
use crate::watcher::{ChangeWatcher, ChangeWatcherOptions};
use serde_json::json;
use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub struct Io {
    pub stdout: Box<dyn FnMut(&str)>,
    pub stderr: Box<dyn FnMut(&str)>,
    pub read_stdin: Box<dyn FnMut() -> String>,
    pub env: HashMap<String, String>,
}

impl Default for Io {
    fn default() -> Self {
        Io {
            stdout: Box::new(|text| print!("{text}")),
            stderr: Box::new(|text| eprint!("{text}")),
            read_stdin: Box::new(|| {
                let mut buf = String::new();
                let _ = std::io::stdin().read_to_string(&mut buf);
                buf
            }),
            env: std::env::vars().collect(),
        }
    }
}

const BOOLEAN_FLAGS: &[&str] = &[
    "json", "peek", "all", "mine", "ack", "accept", "revise", "dry-run", "force", "follow",
    "operator", "open", "help",
];
const REPEATED_FLAGS: &[&str] = &["dep", "scope", "state", "file"];

#[derive(Debug, Clone)]
pub enum FlagValue {
    Bool(bool),
    Str(String),
    List(Vec<String>),
}

#[derive(Default)]
pub struct Parsed {
    pub positionals: Vec<String>,
    pub flags: HashMap<String, FlagValue>,
}

pub fn parse_args(argv: &[String]) -> Result<Parsed> {
    let mut parsed = Parsed::default();
    let mut index = 0;
    while index < argv.len() {
        let arg = &argv[index];
        if arg == "--" {
            parsed.positionals.extend(argv[index + 1..].iter().cloned());
            break;
        }
        if arg == "-h" {
            parsed.flags.insert("help".into(), FlagValue::Bool(true));
            index += 1;
            continue;
        }
        if !arg.starts_with("--") || arg == "-" {
            parsed.positionals.push(arg.clone());
            index += 1;
            continue;
        }
        let body = &arg[2..];
        let (name, inline) = match body.find('=') {
            Some(eq) => (&body[..eq], Some(body[eq + 1..].to_string())),
            None => (body, None),
        };
        let value: FlagValue = if BOOLEAN_FLAGS.contains(&name) {
            let parsed_bool = match inline.as_deref() {
                Some(v) => !matches!(v.to_ascii_lowercase().as_str(), "0" | "false" | "no"),
                None => true,
            };
            FlagValue::Bool(parsed_bool)
        } else if let Some(inline) = inline {
            FlagValue::Str(inline)
        } else {
            index += 1;
            if index >= argv.len() {
                return Err(BusError::invalid(format!("--{name} needs a value")));
            }
            FlagValue::Str(argv[index].clone())
        };
        if REPEATED_FLAGS.contains(&name) {
            let entry = parsed
                .flags
                .entry(name.to_string())
                .or_insert_with(|| FlagValue::List(vec![]));
            if let FlagValue::List(list) = entry {
                match value {
                    FlagValue::Str(v) => list.push(v),
                    FlagValue::Bool(v) => list.push(v.to_string()),
                    FlagValue::List(v) => list.extend(v),
                }
            }
        } else {
            parsed.flags.insert(name.to_string(), value);
        }
        index += 1;
    }
    Ok(parsed)
}

pub const USAGE: &str = "qagent - coordination over one SQLite file (no daemon)

Global: --db PATH (or QAGENT_BUS_DB; default ~/.agent-bus/bus.db)  --as ID|operator  --json

  qagent init                                     create bus.db and the operator token
  qagent agent add <id> --role R [--model M --harness H --parent P --authority worker|manager]
  qagent agent list | qagent token rotate <id>
  qagent whoami | qagent status
  qagent send <to|a,b|*> <subject> [body|-] [--thread T] [--task N] [--type T] [--ack]
  qagent inbox [--peek] [--limit N]
  qagent ack <seq>
  qagent wait [--timeout SEC]                     exit 0 = mail or task event, 2 = timeout
  qagent task add <title> [--brief B|-] [--to ID] [--reviewer ID] [--role R] [--priority P]
                  [--acceptance A] [--parent N] [--dep N]... [--scope PATH]... [--project DIR]
  qagent task list [--mine] [--state S]... [--all] [--limit N] | task show <N>
  qagent task claim [<N>] | task note <N> <text> | task submit <N> --summary S [--details D] [--file F]...
  qagent task review <N> --accept|--revise --feedback F | task cancel <N> [--reason R]
  qagent log [--follow] [--since SEQ] [--limit N]
  qagent import [--jsonl P] [--qagent-state P] [--prototype P] [--dry-run] [--force]
  qagent mcp [--operator] | mcp-config | supervise <agent> [dir] | doctor | dashboard
";

const LAZY: &[&str] = &["mcp", "mcp-config", "dashboard"];

/// Index of the command word: the first positional, skipping the values of flags that take one.
fn command_position(argv: &[String]) -> Option<usize> {
    let mut index = 0;
    while index < argv.len() {
        let arg = &argv[index];
        if arg == "--" {
            return if index + 1 < argv.len() {
                Some(index + 1)
            } else {
                None
            };
        }
        if arg == "-h" || arg == "-" {
            index += 1;
            continue;
        }
        if let Some(name) = arg.strip_prefix("--") {
            if !arg.contains('=') && !BOOLEAN_FLAGS.contains(&name) {
                index += 1;
            }
            index += 1;
            continue;
        }
        return Some(index);
    }
    None
}

struct Context<'a> {
    parsed: Parsed,
    io: &'a mut Io,
    db_path: PathBuf,
    bus: Option<Bus>,
}

impl<'a> Context<'a> {
    fn new(parsed: Parsed, io: &'a mut Io, db_path: PathBuf) -> Self {
        Context {
            parsed,
            io,
            db_path,
            bus: None,
        }
    }

    fn json(&self) -> bool {
        matches!(self.parsed.flags.get("json"), Some(FlagValue::Bool(true)))
    }

    fn flag(&self, name: &str) -> Option<&FlagValue> {
        self.parsed.flags.get(name)
    }

    fn bool_flag(&self, name: &str) -> bool {
        matches!(self.flag(name), Some(FlagValue::Bool(true)))
    }

    fn str_flag(&self, name: &str) -> Option<String> {
        match self.flag(name) {
            Some(FlagValue::Str(value)) => Some(value.clone()),
            Some(FlagValue::List(list)) => list.last().cloned(),
            _ => None,
        }
    }

    fn list_flag(&self, name: &str) -> Vec<String> {
        match self.flag(name) {
            Some(FlagValue::List(list)) => list.clone(),
            Some(FlagValue::Str(value)) => vec![value.clone()],
            _ => vec![],
        }
    }

    fn int_flag(&self, name: &str) -> Result<Option<i64>> {
        let Some(value) = self.str_flag(name) else {
            return Ok(None);
        };
        let parsed: i64 = value
            .trim()
            .parse()
            .map_err(|_| BusError::invalid(format!("--{name} must be an integer")))?;
        Ok(Some(parsed))
    }

    fn position(&self, index: usize, label: &str) -> Result<String> {
        match self.parsed.positionals.get(index) {
            Some(value) if !value.is_empty() => Ok(value.clone()),
            _ => Err(BusError::invalid(format!("missing {label}"))),
        }
    }

    fn task_id(&self, index: usize) -> Result<i64> {
        let raw = self.position(index, "task number")?;
        let raw = raw.strip_prefix('#').unwrap_or(&raw).to_string();
        let id: i64 = raw.parse().unwrap_or(-1);
        if id <= 0 {
            return Err(BusError::invalid(format!("invalid task number: {raw}")));
        }
        Ok(id)
    }

    /// Read "-" from stdin.
    fn maybe_stdin(&mut self, value: Option<String>) -> Option<String> {
        if value.as_deref() == Some("-") {
            Some((self.io.read_stdin)())
        } else {
            value
        }
    }

    fn env(&self, name: &str) -> Option<String> {
        self.io.env.get(name).cloned()
    }

    fn bus(&mut self) -> Result<&Bus> {
        if self.bus.is_none() {
            self.bus = Some(Bus::open(Some(&self.db_path))?);
        }
        Ok(self.bus.as_ref().unwrap())
    }

    /// The caller: --as, then QAGENT_AGENT_ID / AGENT_ID; operator commands fall back to the operator.
    fn identity(&mut self, operator_default: bool) -> Result<Identity> {
        let chosen = self
            .str_flag("as")
            .or_else(|| self.env("QAGENT_AGENT_ID"))
            .or_else(|| self.env("AGENT_ID"))
            .or_else(|| {
                if operator_default {
                    Some(OPERATOR_ID.to_string())
                } else {
                    None
                }
            });
        let Some(chosen) = chosen else {
            return Err(BusError::unauthorized(
                "no agent identity: set QAGENT_AGENT_ID or pass --as <id>",
            ));
        };
        let bus = self.bus()?;
        identity::resolve_identity(&bus.conn, &bus.home, &chosen)
    }

    fn out(&mut self, value: serde_json::Value, text: &str) {
        if self.json() {
            (self.io.stdout)(&format!(
                "{}\n",
                serde_json::to_string_pretty(&value).unwrap_or_default()
            ));
        } else {
            (self.io.stdout)(&format!("{text}\n"));
        }
    }

    fn out_event(&mut self, event: &crate::types::BusEvent) {
        if self.json() {
            (self.io.stdout)(&format!(
                "{}\n",
                serde_json::to_string(event).unwrap_or_default()
            ));
        } else {
            (self.io.stdout)(&format!("{}\n", render_event(event)));
        }
    }
}

fn wait_command(ctx: &mut Context) -> Result<i32> {
    let me = ctx.identity(false)?;
    let seconds = wait::wait_seconds(ctx.int_flag("timeout")?, None)?;
    let interrupted = Arc::new(AtomicBool::new(false));
    ctx.bus()?;
    let interrupt_flag = interrupted.clone();
    let stop = {
        let stop = Arc::new(AtomicBool::new(false));
        let flagged = stop.clone();
        let _ = ctrlc::set_handler(move || {
            flagged.store(true, Ordering::SeqCst);
            interrupt_flag.store(true, Ordering::SeqCst);
        });
        stop
    };
    let bus = ctx.bus.as_ref().unwrap();
    let result = wait::wait_for_mail(bus, &me, Duration::from_secs(seconds as u64), &stop)?;
    let text = match result.status.as_str() {
        "mail" => render_messages(&result.messages, "(no new messages)"),
        "task" => result
            .events
            .iter()
            .map(render_event)
            .collect::<Vec<_>>()
            .join("\n"),
        _ => format!("no mail for {} within {seconds}s", me.agent_id),
    };
    ctx.out(serde_json::to_value(&result)?, &text);
    if interrupted.load(Ordering::SeqCst) {
        return Ok(130);
    }
    Ok(if result.status == "timeout" || result.status == "none" {
        2
    } else {
        0
    })
}

fn log_command(ctx: &mut Context) -> Result<i32> {
    let follow = ctx.bool_flag("follow");
    let limit = ctx.int_flag("limit")?.unwrap_or(200);
    let mut since = match ctx.int_flag("since")? {
        Some(since) => since,
        None => {
            if follow {
                ctx.bus()?.latest_seq()?
            } else {
                0
            }
        }
    };
    let print = |ctx: &mut Context, since: &mut i64| -> Result<usize> {
        let events = ctx
            .bus()?
            .events(*since, if follow { 5000 } else { limit })?;
        let count = events.len();
        for event in &events {
            ctx.out_event(event);
        }
        if let Some(last) = events.last() {
            *since = last.seq;
        }
        Ok(count)
    };
    if !follow {
        if print(ctx, &mut since)? == 0 && !ctx.json() {
            (ctx.io.stdout)("(no events)\n");
        }
        return Ok(0);
    }
    // fs.watch wakes promptly; the data_version poll is a missed-event fallback at ~1 read/s idle.
    ctx.bus()?;
    let watcher = ChangeWatcher::new(
        &ctx.db_path.clone(),
        ChangeWatcherOptions {
            min_poll_ms: 10,
            max_poll_ms: 1000,
            fs_watch: true,
        },
    )?;
    let wake = watcher.wake_sender();
    let stop = {
        let stop = Arc::new(AtomicBool::new(false));
        let flagged = stop.clone();
        let _ = ctrlc::set_handler(move || {
            flagged.store(true, Ordering::SeqCst);
            let _ = wake.send(());
        });
        stop
    };
    print(ctx, &mut since)?;
    while !stop.load(Ordering::SeqCst) {
        let seq = watcher.next(since, Duration::from_secs(60), &stop)?;
        if seq > since {
            print(ctx, &mut since)?;
        }
    }
    Ok(0)
}

fn task_command(ctx: &mut Context, sub: Option<&String>) -> Result<i32> {
    match sub.map(|s| s.as_str()) {
        Some("add") => {
            let me = ctx.identity(false)?;
            let scopes = ctx.list_flag("scope");
            let project = ctx.str_flag("project").or_else(|| {
                if scopes.is_empty() {
                    None
                } else {
                    std::env::current_dir()
                        .ok()
                        .map(|p| p.display().to_string())
                }
            });
            let brief = ctx.maybe_stdin(ctx.str_flag("brief"));
            let acceptance = ctx.maybe_stdin(ctx.str_flag("acceptance"));
            let title = ctx.position(2, "task title")?;
            let to = ctx.str_flag("to");
            let reviewer = ctx.str_flag("reviewer");
            let role = ctx.str_flag("role");
            let priority = ctx.str_flag("priority");
            let parent_id = ctx.int_flag("parent")?;
            let dependencies = ctx
                .list_flag("dep")
                .iter()
                .map(|d| {
                    d.trim()
                        .parse::<i64>()
                        .map_err(|_| BusError::invalid(format!("invalid --dep value: {d}")))
                })
                .collect::<Result<Vec<_>>>()?;
            let task = ctx.bus()?.create_task(
                &me,
                CreateTaskInput {
                    title,
                    brief,
                    acceptance,
                    to,
                    reviewer,
                    role,
                    priority,
                    parent_id,
                    dependencies,
                    path_scopes: scopes,
                    project,
                    refs: None,
                    max_retries: None,
                },
            )?;
            let id = task.id;
            let state = task.state.clone();
            ctx.out(
                serde_json::to_value(&task)?,
                &format!("created task #{id} ({state})"),
            );
            Ok(0)
        }
        Some("list") => {
            let mine = if ctx.bool_flag("mine") {
                Some(ctx.identity(false)?.agent_id)
            } else {
                None
            };
            let states = {
                let states = ctx.list_flag("state");
                if states.is_empty() {
                    None
                } else {
                    Some(states)
                }
            };
            let include_closed = ctx.bool_flag("all");
            let limit = ctx.int_flag("limit")?;
            let tasks = ctx.bus()?.list_tasks(ListTasksInput {
                mine,
                states,
                include_closed,
                limit,
            })?;
            ctx.out(serde_json::to_value(&tasks)?, &render_tasks(&tasks));
            Ok(0)
        }
        Some("show") => {
            let id = ctx.task_id(2)?;
            let task = ctx.bus()?.get_task(id)?;
            ctx.out(serde_json::to_value(&task)?, &render_task(&task));
            Ok(0)
        }
        Some("claim") => {
            let me = ctx.identity(false)?;
            let id = if ctx.parsed.positionals.get(2).is_none() {
                None
            } else {
                Some(ctx.task_id(2)?)
            };
            let task = ctx.bus()?.claim_task(&me, id)?;
            ctx.out(
                serde_json::to_value(&task)?,
                &format!("claimed task #{}: {}", task.id, task.title),
            );
            Ok(0)
        }
        Some("note") => {
            let me = ctx.identity(false)?;
            let id = ctx.task_id(2)?;
            let text = ctx
                .maybe_stdin(Some(ctx.position(3, "note text")?))
                .unwrap_or_default();
            let note = ctx.bus()?.note_task(&me, id, &text)?;
            let note_id = note.id;
            ctx.out(
                serde_json::to_value(&note)?,
                &format!("noted on task #{note_id}"),
            );
            Ok(0)
        }
        Some("submit") => {
            let summary = ctx.maybe_stdin(ctx.str_flag("summary"));
            let Some(summary) = summary.filter(|s| !s.is_empty()) else {
                return Err(BusError::invalid("--summary is required"));
            };
            let details = ctx.maybe_stdin(ctx.str_flag("details"));
            let me = ctx.identity(false)?;
            let id = ctx.task_id(2)?;
            let changed_files = ctx.list_flag("file");
            let task = ctx.bus()?.submit_task(
                &me,
                id,
                SubmitInput {
                    summary,
                    details,
                    changed_files,
                    artifacts: None,
                    validation: None,
                },
            )?;
            let out = format!("submitted task #{} round {}", task.id, task.round);
            ctx.out(serde_json::to_value(&task)?, &out);
            Ok(0)
        }
        Some("review") => {
            let accept = ctx.bool_flag("accept");
            let revise = ctx.bool_flag("revise");
            if accept == revise {
                return Err(BusError::invalid(
                    "task review needs exactly one of --accept or --revise",
                ));
            }
            let feedback = ctx.maybe_stdin(ctx.str_flag("feedback"));
            let Some(feedback) = feedback.filter(|s| !s.is_empty()) else {
                return Err(BusError::invalid("--feedback is required"));
            };
            let me = ctx.identity(false)?;
            let id = ctx.task_id(2)?;
            let task = ctx.bus()?.review_task(&me, id, accept, &feedback)?;
            let suffix = if task.state == "changes_requested" {
                format!(" (round {})", task.round)
            } else {
                String::new()
            };
            let out = format!("task #{} is {}{suffix}", task.id, task.state);
            ctx.out(serde_json::to_value(&task)?, &out);
            Ok(0)
        }
        Some("cancel") => {
            let me = ctx.identity(false)?;
            let id = ctx.task_id(2)?;
            let reason = ctx.str_flag("reason");
            let task = ctx.bus()?.cancel_task(&me, id, reason.as_deref())?;
            let out = format!("cancelled task #{}", task.id);
            ctx.out(serde_json::to_value(&task)?, &out);
            Ok(0)
        }
        _ => Err(BusError::invalid(
            "usage: qagent task add|list|show|claim|note|submit|review|cancel",
        )),
    }
}

/// PATH lookup with the exec bit — doctor never runs the harness, only finds it.
fn on_path(command: &str) -> bool {
    let candidates: Vec<PathBuf> = if Path::new(command).is_absolute() || command.contains('/') {
        vec![PathBuf::from(command)]
    } else {
        std::env::var_os("PATH")
            .map(|paths| {
                std::env::split_paths(&paths)
                    .map(|dir| dir.join(command))
                    .collect()
            })
            .unwrap_or_default()
    };
    candidates.iter().any(|path| {
        let meta = match fs::metadata(path) {
            Ok(meta) => meta,
            Err(_) => return false,
        };
        if !meta.is_file() {
            return false;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            meta.permissions().mode() & 0o111 != 0
        }
        #[cfg(not(unix))]
        {
            true
        }
    })
}

/// `qagent fake-harness --mode <m> --agent <a> --prompt <p> [--session <s>]` —
/// port of src/fake-harness.ts: the deterministic stand-in vendor CLI used by
/// supervisor tests and the `fake` adapter.
fn fake_harness_command(argv: &[String]) -> i32 {
    let arg = |name: &str| -> String {
        argv.iter()
            .position(|a| a == name)
            .and_then(|i| argv.get(i + 1))
            .cloned()
            .unwrap_or_default()
    };
    let mode = {
        let m = arg("--mode");
        if m.is_empty() {
            "success".to_string()
        } else {
            m
        }
    };
    let agent = {
        let a = arg("--agent");
        if a.is_empty() {
            "fake".to_string()
        } else {
            a
        }
    };
    let prompt = arg("--prompt");
    let existing_session = arg("--session");
    let state_file = std::env::var("FAKE_HARNESS_STATE")
        .ok()
        .filter(|s| !s.is_empty());

    if mode == "malformed" {
        println!("this is deliberately malformed provider output");
        return 0;
    }
    if mode == "fail" {
        eprintln!("deterministic fake harness failure");
        return 23;
    }
    if mode == "fail-once" {
        let mut seen = false;
        if let Some(state_file) = &state_file {
            seen = fs::read_to_string(state_file)
                .map(|s| s.trim() == "failed")
                .unwrap_or(false);
            if !seen {
                let _ = fs::write(state_file, "failed");
            }
        }
        if !seen {
            eprintln!("deterministic first-attempt failure");
            return 24;
        }
    }
    if mode == "hang-inner" {
        loop {
            std::thread::sleep(std::time::Duration::from_secs(3600));
        }
    }
    if mode == "hang" {
        // A grandchild in the same process group; only a process-group kill clears both.
        let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("qagent"));
        let grandchild = std::process::Command::new(exe)
            .args(["fake-harness", "--mode", "hang-inner"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        if let Some(state_file) = &state_file {
            let grandchild_pid = grandchild.as_ref().map(|c| c.id()).unwrap_or(0);
            let _ = fs::write(
                state_file,
                json!({ "pid": std::process::id(), "grandchild": grandchild_pid }).to_string(),
            );
        }
        loop {
            std::thread::sleep(std::time::Duration::from_secs(3600));
        }
    }

    let bus_task = if mode == "bus-cli" {
        let raw = match std::env::var("QAGENT_MCP_COMMAND") {
            Ok(raw) if !raw.is_empty() => raw,
            _ => {
                eprintln!("bus-cli mode needs QAGENT_MCP_COMMAND");
                return 25;
            }
        };
        let launch: serde_json::Value = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(_) => {
                eprintln!("bus-cli mode needs valid QAGENT_MCP_COMMAND JSON");
                return 25;
            }
        };
        let command = launch["command"].as_str().unwrap_or("").to_string();
        let mut base: Vec<String> = launch["args"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        if base.last().map(|s| s.as_str()) == Some("mcp") {
            base.pop();
        }
        let mut env: HashMap<String, String> = std::env::vars().collect();
        if let Some(extra) = launch["env"].as_object() {
            for (k, v) in extra {
                if let Some(v) = v.as_str() {
                    env.insert(k.clone(), v.to_string());
                }
            }
        }
        let qagent = |args: &[&str]| -> serde_json::Value {
            let mut full: Vec<String> = base.clone();
            full.extend(args.iter().map(|s| s.to_string()));
            full.push("--json".to_string());
            match std::process::Command::new(&command)
                .args(&full)
                .envs(&env)
                .output()
            {
                Ok(out) if out.status.success() => {
                    serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).unwrap_or(json!({}))
                }
                Ok(out) => {
                    eprintln!(
                        "qagent {} failed ({:?}): {}",
                        args.join(" "),
                        out.status.code(),
                        String::from_utf8_lossy(&out.stderr)
                    );
                    std::process::exit(26);
                }
                Err(error) => {
                    eprintln!("qagent {} failed to spawn: {error}", args.join(" "));
                    std::process::exit(26);
                }
            }
        };
        let wanted = {
            let marker = "[TASK #";
            prompt.find(marker).map(|pos| {
                let rest = &prompt[pos + marker.len()..];
                let end = rest
                    .find(|c: char| !c.is_ascii_digit())
                    .unwrap_or(rest.len());
                rest[..end].to_string()
            })
        };
        let claimed = if let Some(wanted) = &wanted {
            qagent(&["task", "claim", wanted])
        } else {
            qagent(&["task", "claim"])
        };
        let id = claimed["id"]
            .as_i64()
            .map(|i| i.to_string())
            .unwrap_or_default();
        qagent(&[
            "task",
            "note",
            &id,
            &format!("fake {agent} started task #{id}"),
        ]);
        qagent(&[
            "task",
            "submit",
            &id,
            "--summary",
            &format!("fake {agent} submitted task #{id} through qagent"),
        ]);
        id.parse::<i64>().ok()
    } else {
        None
    };

    let input_tokens = (prompt.len() as f64 / 4.0).ceil().max(1.0) as i64;
    let output_tokens = 24i64;
    let mut out = json!({
        "sessionId": if existing_session.is_empty() { format!("fake-{agent}-session") } else { existing_session },
        "result": format!("fake {agent} completed the assigned work"),
        "usage": { "inputTokens": input_tokens, "outputTokens": output_tokens, "totalTokens": input_tokens + output_tokens, "costUSD": 0 },
        "changedFiles": [],
        "validation": [{ "passed": true, "summary": "deterministic fake harness completed" }],
    });
    if let Some(id) = bus_task {
        out["busTask"] = json!(id);
    }
    println!("{}", out);
    0
}

/// `qagent supervise <agent> [project-dir] [--config PATH]` — drives one agent's
/// harness CLI against the bus until interrupted (Ctrl-C or SIGTERM).
fn supervise_command(ctx: &mut Context) -> Result<i32> {
    let agent_id = ctx.parsed.positionals.get(1).cloned().ok_or_else(|| {
        BusError::invalid("usage: qagent supervise <agent> [project-dir] [--config PATH]")
    })?;
    let dir = ctx
        .parsed
        .positionals
        .get(2)
        .cloned()
        .unwrap_or_else(|| ".".to_string());
    let config_flag = ctx.str_flag("config").map(PathBuf::from);

    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = Arc::clone(&stop);
        let _ = ctrlc::set_handler(move || {
            stop.store(true, Ordering::SeqCst);
        });
    }
    crate::supervisor::supervise(crate::supervisor::SuperviseOptions {
        agent_id,
        workdir: dir,
        db_path: ctx.db_path.clone(),
        config_path: config_flag,
        stop,
        wait_ms: None,
        fake_harness_path: None,
        qagent_bin: None,
        log: None,
    })?;
    Ok(0)
}

/// `qagent doctor [agent] [project-dir] [--config PATH]` — read-only checks.
/// Never runs a harness binary; it only looks for it on PATH.
fn doctor_command(ctx: &mut Context) -> Result<i32> {
    let agent_id = ctx.parsed.positionals.get(1).cloned();
    let dir = ctx.parsed.positionals.get(2).cloned();
    let config_flag = ctx.str_flag("config");
    let mut problems: Vec<String> = vec![];
    let mut lines: Vec<String> = vec![];
    let db_exists = ctx.db_path.exists();
    let missing = if db_exists {
        String::new()
    } else {
        " (missing: run `qagent init`)".to_string()
    };
    lines.push(format!("bus {}{missing}", ctx.db_path.display()));
    if !db_exists {
        for line in &lines {
            (ctx.io.stdout)(&format!("{line}\n"));
        }
        return Ok(1);
    }
    {
        let bus = ctx.bus()?;
        let operator_token_path = crate::identity::operator_token_path(&bus.home);
        if crate::identity::read_token_file(&operator_token_path).is_none() {
            problems.push(format!(
                "no operator token at {}",
                operator_token_path.display()
            ));
        }
        let agents = bus.list_agents()?;
        let ids = agents
            .iter()
            .map(|(agent, _)| agent.id.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!(
            "agents {}",
            if ids.is_empty() {
                "(none)".to_string()
            } else {
                ids
            }
        ));
        if let Some(agent_id) = agent_id.as_deref() {
            match crate::identity::resolve_identity(&bus.conn, &bus.home, agent_id) {
                Ok(_) => lines.push(format!("identity {agent_id} ok")),
                Err(error) => problems.push(format!("identity {agent_id}: {}", error.message)),
            }
            let project_root = match dir.as_deref() {
                Some(dir) => crate::db::absolutize(Path::new(dir)),
                None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            };
            let env_map = ctx.io.env.clone();
            let config_path = config_flag.map(PathBuf::from).unwrap_or_else(|| {
                crate::config::config_path_from_project(&project_root, &|name| {
                    env_map.get(name).cloned()
                })
            });
            let resolved = crate::config::load_config(&config_path).and_then(|config| {
                crate::config::resolve_agent(&config, agent_id).map(|r| {
                    (
                        r.harness.id.clone(),
                        r.harness.command.clone(),
                        r.harness.adapter.clone(),
                        r.agent.auto_start,
                        r.agent.enabled,
                    )
                })
            });
            match resolved {
                Ok((harness_id, harness_command, harness_adapter, auto_start, agent_enabled)) => {
                    lines.push(format!(
                        "config {}: {} ({}), autoStart {}",
                        config_path.display(),
                        harness_id,
                        harness_command,
                        auto_start
                    ));
                    if !agent_enabled {
                        problems.push(format!(
                            "{agent_id} is disabled in {}",
                            config_path.display()
                        ));
                    }
                    if harness_adapter != "fake" && !on_path(&harness_command) {
                        problems.push(format!("{harness_command} is not on PATH"));
                    }
                }
                Err(error) => problems.push(format!("config: {}", error.message)),
            }
        }
    }
    for problem in &problems {
        lines.push(format!("problem: {problem}"));
    }
    lines.push(if problems.is_empty() {
        "ok".to_string()
    } else {
        format!("{} problem(s)", problems.len())
    });
    for line in &lines {
        (ctx.io.stdout)(&format!("{line}\n"));
    }
    Ok(if problems.is_empty() { 0 } else { 1 })
}

fn dispatch(ctx: &mut Context) -> Result<i32> {
    let command = ctx.parsed.positionals.first().cloned();
    let sub = ctx.parsed.positionals.get(1).cloned();
    match command.as_deref() {
        Some("init") => {
            let result = ctx.bus()?.init()?;
            ctx.out(
                serde_json::to_value(&result)?,
                &format!(
                    "bus {}\noperator token {} ({})",
                    result.db_path, result.operator_token_path, result.operator
                ),
            );
            Ok(0)
        }
        Some("whoami") => {
            let me = ctx.identity(false)?;
            let who = ctx.bus()?.whoami(&me)?;
            let role = who
                .agent
                .as_ref()
                .map(|a| {
                    if a.role.is_empty() {
                        String::new()
                    } else {
                        format!(", role {}", a.role)
                    }
                })
                .unwrap_or_default();
            ctx.out(
                serde_json::to_value(&who)?,
                &format!(
                    "{} ({}{}) unread {} cursor {}\nbus {}",
                    me.agent_id, me.authority, role, who.unread, who.cursor, who.db_path
                ),
            );
            Ok(0)
        }
        Some("doctor") => doctor_command(ctx),
        Some("supervise") => supervise_command(ctx),
        Some("fake-harness") => unreachable!("handled before dispatch"),
        Some("status") => {
            let status = ctx.bus()?.status()?;
            ctx.out(serde_json::to_value(&status)?, &render_status(&status));
            Ok(0)
        }
        Some("agent") => match sub.as_deref() {
            Some("list") => {
                let agents = ctx.bus()?.list_agents()?;
                let json = serde_json::to_value(
                    agents
                        .iter()
                        .map(|(agent, unread)| {
                            let mut value = serde_json::to_value(agent).unwrap_or(json!({}));
                            value
                                .as_object_mut()
                                .map(|o| o.insert("unread".into(), json!(unread)));
                            value
                        })
                        .collect::<Vec<_>>(),
                )?;
                ctx.out(json, &render_agents(&agents));
                Ok(0)
            }
            Some("add") => {
                let authority = ctx.str_flag("authority").unwrap_or_else(|| "worker".into());
                if authority != "worker" && authority != "manager" {
                    return Err(BusError::invalid("--authority must be worker or manager"));
                }
                let me = ctx.identity(true)?;
                let id = ctx.position(2, "agent id")?;
                let role = ctx.str_flag("role");
                let model = ctx.str_flag("model");
                let harness = ctx.str_flag("harness");
                let parent = ctx.str_flag("parent");
                let (agent, token_path) = ctx.bus()?.add_agent(
                    &me,
                    &id,
                    role.as_deref(),
                    model.as_deref(),
                    harness.as_deref(),
                    parent.as_deref(),
                    Some(&authority),
                )?;
                ctx.out(
                    json!({ "agent": serde_json::to_value(&agent)?, "tokenPath": token_path.display().to_string() }),
                    &format!("added {}; token {}", agent.id, token_path.display()),
                );
                Ok(0)
            }
            _ => Err(BusError::invalid(
                "usage: qagent agent add <id> --role R | qagent agent list",
            )),
        },
        Some("token") => {
            if sub.as_deref() != Some("rotate") {
                return Err(BusError::invalid("usage: qagent token rotate <id>"));
            }
            let me = ctx.identity(true)?;
            let id = ctx.position(2, "agent id")?;
            let token_path = ctx.bus()?.rotate_token(&me, &id)?;
            ctx.out(
                json!({ "tokenPath": token_path.display().to_string() }),
                &format!("rotated; token {}", token_path.display()),
            );
            Ok(0)
        }
        Some("send") => {
            let me = ctx.identity(false)?;
            let to = ctx.position(1, "recipient")?;
            let subject = ctx.position(2, "subject")?;
            let body = ctx
                .maybe_stdin(ctx.parsed.positionals.get(3).cloned())
                .unwrap_or_default();
            let task_id = ctx.int_flag("task")?;
            let msg_type = ctx.str_flag("type");
            let thread = ctx.str_flag("thread");
            let requires_ack = ctx.bool_flag("ack");
            let sent = ctx.bus()?.send(
                &me,
                SendInput {
                    to,
                    subject: Some(subject),
                    body,
                    msg_type,
                    thread,
                    task_id,
                    refs: None,
                    requires_ack,
                },
            )?;
            let text = sent
                .iter()
                .map(|message| {
                    format!(
                        "sent #{} to {}",
                        message.seq,
                        message.recipient.clone().unwrap_or_else(|| "*".into())
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            ctx.out(serde_json::to_value(&sent)?, &text);
            Ok(0)
        }
        Some("inbox") => {
            let me = ctx.identity(false)?;
            let peek = ctx.bool_flag("peek");
            let limit = ctx.int_flag("limit")?;
            let result = ctx.bus()?.inbox(&me, peek, limit)?;
            let extra = if result.remaining > 0 {
                format!("\n({} more unread)", result.remaining)
            } else {
                String::new()
            };
            let text = format!(
                "{}{extra}",
                render_messages(&result.messages, "(no new messages)")
            );
            ctx.out(
                json!({ "messages": serde_json::to_value(&result.messages)?, "cursor": result.cursor, "remaining": result.remaining }),
                &text,
            );
            Ok(0)
        }
        Some("ack") => {
            let raw = ctx.position(1, "message seq")?;
            let raw = raw.strip_prefix('#').unwrap_or(&raw).to_string();
            let seq: i64 = raw
                .parse()
                .map_err(|_| BusError::invalid("ack needs a message sequence number"))?;
            let me = ctx.identity(false)?;
            let (seq, ack_ms) = ctx.bus()?.ack(&me, seq)?;
            ctx.out(
                json!({ "seq": seq, "ackMs": ack_ms }),
                &format!("acknowledged #{seq}"),
            );
            Ok(0)
        }
        Some("wait") => wait_command(ctx),
        Some("log") => log_command(ctx),
        Some("task") => task_command(ctx, sub.as_ref()),
        Some("import") => {
            let explicit = ["jsonl", "qagent-state", "prototype"]
                .iter()
                .any(|name| ctx.str_flag(name).is_some());
            let sources = if explicit {
                ImportSources {
                    jsonl: ctx.str_flag("jsonl").map(PathBuf::from),
                    qagent_state: ctx.str_flag("qagent-state").map(PathBuf::from),
                    prototype: ctx.str_flag("prototype").map(PathBuf::from),
                }
            } else {
                default_import_sources(&home_for(&ctx.db_path))
            };
            let dry_run = ctx.bool_flag("dry-run");
            // A dry run never opens bus.db for writing (and never creates it).
            if !dry_run {
                let me = ctx.identity(true)?;
                if me.authority != "operator" {
                    return Err(BusError::forbidden("only the operator may import"));
                }
                ctx.bus = None;
            }
            let report = run_import(
                &ctx.db_path,
                sources,
                ImportOptions {
                    dry_run,
                    force: ctx.bool_flag("force"),
                    actor: Some(OPERATOR_ID.to_string()),
                    ..Default::default()
                },
            )?;
            let text = render_import(&report);
            ctx.out(serde_json::to_value(&report).unwrap_or_default(), &text);
            Ok(0)
        }
        Some(other) => Err(BusError::invalid(format!(
            "unknown command: {other}\n\n{USAGE}"
        ))),
        None => Err(BusError::invalid(format!("unknown command\n\n{USAGE}"))),
    }
}

pub fn run(argv: &[String], io: &mut Io) -> i32 {
    if let Some(index) = command_position(argv) {
        if argv[index] == "fake-harness" {
            return fake_harness_command(&argv[index + 1..]);
        }
    }
    let parsed = match parse_args(argv) {
        Ok(parsed) => parsed,
        Err(error) => {
            (io.stderr)(&format!("qagent: {}\n", error.message));
            return 1;
        }
    };
    if parsed.positionals.is_empty()
        || matches!(parsed.flags.get("help"), Some(FlagValue::Bool(true)))
        || parsed.positionals.first().map(|p| p.as_str()) == Some("help")
    {
        (io.stdout)(USAGE);
        return 0;
    }
    // Lazy-module commands: announce the port boundary rather than silently doing nothing.
    if let Some(index) = command_position(argv) {
        let command = &argv[index];
        if LAZY.contains(&command.as_str()) {
            (io.stderr)(&format!("qagent: `{command}` is not implemented in the Rust port yet — use the Node qagent.\n"));
            return 1;
        }
    }
    let db_flag = parsed.flags.get("db").and_then(|v| match v {
        FlagValue::Str(s) => Some(s.clone()),
        _ => None,
    });
    let env_for_db = io.env.clone();
    let db_path = resolve_db_path_with(db_flag.as_deref(), |name| env_for_db.get(name).cloned());
    let mut ctx = Context::new(parsed, io, db_path);
    match dispatch(&mut ctx) {
        Ok(code) => code,
        Err(error) => {
            let code = error.code;
            (ctx.io.stderr)(&format!("qagent: {}\n", error.message));
            if ctx.json() {
                let message = error.message;
                (ctx.io.stdout)(&format!(
                    "{}\n",
                    json!({ "error": message, "code": code.as_str() })
                ));
            }
            if code == Code::Unauthorized || code == Code::Forbidden {
                3
            } else {
                1
            }
        }
    }
}
