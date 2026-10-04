//! Harness adapters — ported verbatim from src/adapters.ts. Each adapter knows
//! how to build the CLI invocation for one vendor tool (plus a generic
//! `command` escape hatch) and how to normalize its stdout into a result.
//! probeHarness/discoverHarnessModels are not ported — nothing in supervise
//! calls them. Since then the Rust side has moved ahead: harness-level
//! `options`, the `{mcpConfig}`/`{mcpJson}` placeholders, per-agent MCP wiring
//! for Gemini, Kimi and OpenCode, and `autoApprove: false`.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use crate::config::ResolvedAgent;
use crate::error::Result;

/// How a harness launches the bus MCP server; v2 passes `qagent mcp` with the agent's own identity.
#[derive(Debug, Clone)]
pub struct McpCommand {
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
}

pub struct AdapterContext<'a> {
    pub agent: &'a ResolvedAgent<'a>,
    pub prompt: String,
    pub session_id: Option<String>,
    /// Configured native chat binding. Dynamic sessions leave this null.
    pub pinned_session_id: Option<String>,
    pub workdir: String,
    /// The qagent binary the fake harness / MCP fallback invokes (process.execPath in TS).
    pub qagent_bin: String,
    pub mcp_server_path: String,
    pub fake_harness_path: String,
    pub bus_environment: HashMap<String, String>,
    /// When set, MCP-capable adapters launch this instead of `node <mcp_server_path>`.
    pub mcp_command: Option<McpCommand>,
}

#[derive(Debug)]
pub struct HarnessInvocation {
    pub command: String,
    pub args: Vec<String>,
    pub environment: HashMap<String, String>,
    pub auto_report: bool,
    pub timeout_ms: u64,
}

#[derive(Debug, Default)]
pub struct Usage {
    pub input_tokens: f64,
    pub output_tokens: f64,
    pub total_tokens: f64,
    pub cost_usd: f64,
}

#[derive(Debug)]
pub struct NormalizedHarnessResult {
    pub text: String,
    pub session_id: Option<String>,
    pub usage: Usage,
    pub structured: Option<serde_json::Value>,
    pub malformed: bool,
}

pub struct HarnessAdapter {
    pub id: &'static str,
    pub prepare: Option<fn(&AdapterContext) -> Result<()>>,
    pub build: fn(&AdapterContext) -> HarnessInvocation,
    pub parse: fn(&str, i32) -> NormalizedHarnessResult,
}

// ------------------------------------------------------------------- helpers

/// `/\x1b\[[0-9;?]*[A-Za-z]/g` — CSI sequences.
fn strip_ansi(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = String::with_capacity(value.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            let mut j = i + 2;
            while j < bytes.len()
                && (bytes[j].is_ascii_digit() || bytes[j] == b';' || bytes[j] == b'?')
            {
                j += 1;
            }
            if j < bytes.len() && bytes[j].is_ascii_alphabetic() {
                i = j + 1;
                continue;
            }
        }
        // Copy one UTF-8 char.
        let ch_len = utf8_len(bytes[i]);
        out.push_str(&value[i..i + ch_len]);
        i += ch_len;
    }
    out
}

fn utf8_len(first: u8) -> usize {
    if first < 0x80 {
        1
    } else if first >> 5 == 0b110 {
        2
    } else if first >> 4 == 0b1110 {
        3
    } else {
        4
    }
}

/// Non-empty JSON object per line; prose between JSON lines is normal for several CLIs.
fn json_lines(stdout: &str) -> Vec<serde_json::Value> {
    stdout
        .lines()
        .filter_map(|line| {
            let parsed: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
            parsed.is_object().then_some(parsed)
        })
        .collect()
}

fn as_number(value: &serde_json::Value) -> f64 {
    match value {
        serde_json::Value::Number(n) => n.as_f64().unwrap_or(0.0),
        serde_json::Value::String(s) => s.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// `a ?? b ?? c` on JSON values: first non-null wins (0 is a real value).
fn coalesce_number(values: &[&serde_json::Value]) -> f64 {
    for v in values {
        if !v.is_null() {
            return as_number(v);
        }
    }
    0.0
}

fn str_field(row: &serde_json::Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| row[*key].as_str().map(str::to_string))
}

fn default_result(stdout: &str, exit_code: i32) -> NormalizedHarnessResult {
    let text = strip_ansi(stdout).trim().to_string();
    NormalizedHarnessResult {
        malformed: exit_code == 0 && text.is_empty(),
        text: if text.is_empty() {
            "(no textual output captured)".to_string()
        } else {
            text
        },
        session_id: None,
        usage: Usage::default(),
        structured: None,
    }
}

fn common_environment(context: &AdapterContext) -> HashMap<String, String> {
    let mut env = context.bus_environment.clone();
    env.insert("AGENT_ID".to_string(), context.agent.agent.id.clone());
    env.insert("AGENT_ROLE".to_string(), context.agent.agent.role.clone());
    env.insert("AGENT_MODEL".to_string(), context.agent.model.id.clone());
    env.insert(
        "AGENT_FAMILY".to_string(),
        context.agent.model.family.clone(),
    );
    env.insert(
        "AGENT_PROVIDER".to_string(),
        context.agent.model.provider.clone(),
    );
    env.insert(
        "AGENT_HARNESS".to_string(),
        context.agent.harness.id.clone(),
    );
    env
}

/// The MCP server launch line: the v2 mcpCommand when given, otherwise the legacy `node <mcp_server_path>`.
fn mcp_launch(
    context: &AdapterContext,
    env: &HashMap<String, String>,
) -> (String, Vec<String>, HashMap<String, String>) {
    match &context.mcp_command {
        Some(command) => {
            let mut merged = env.clone();
            merged.extend(command.env.clone());
            (command.command.clone(), command.args.clone(), merged)
        }
        None => (
            context.qagent_bin.clone(),
            vec![context.mcp_server_path.clone()],
            env.clone(),
        ),
    }
}

/// An option for this agent's harness: the agent's harnessOptions first, then
/// the harness's own `options` (shared by every agent on it).
fn option<'a>(context: &'a AdapterContext, key: &str) -> &'a serde_json::Value {
    let own = &context.agent.agent.harness_options[key];
    if own.is_null() {
        &context.agent.harness.options[key]
    } else {
        own
    }
}

/// Whether the CLI may run tools without asking. Unset keeps each adapter's
/// unattended default; `autoApprove: false` drops the approval-skipping flag.
fn auto_approve(context: &AdapterContext) -> bool {
    option(context, "autoApprove").as_bool().unwrap_or(true)
}

/// The bus MCP server in the common `{"mcpServers": {...}}` shape most CLIs read.
fn mcp_servers_json(context: &AdapterContext) -> serde_json::Value {
    let (command, args, env) = mcp_launch(context, &common_environment(context));
    serde_json::json!({
        "mcpServers": { "qagent": { "command": command, "args": args, "env": env } },
    })
}

/// Per-agent file under `<bus home>/mcp/`, so no agent writes into the project
/// folder and two agents sharing a folder never swap identities.
fn agent_file(context: &AdapterContext, suffix: &str) -> PathBuf {
    let db = context
        .bus_environment
        .get("QAGENT_BUS_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(&context.workdir)
                .join(".agent-bus")
                .join("bus.db")
        });
    crate::db::home_for(&db)
        .join("mcp")
        .join(format!("{}{suffix}", context.agent.agent.id))
}

fn write_agent_file(path: &PathBuf, value: &serde_json::Value) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(path, serde_json::to_string_pretty(value).unwrap())?;
    Ok(())
}

fn command_template_value(value: &str, context: &AdapterContext) -> String {
    let mcp_json = mcp_servers_json(context).to_string();
    let mcp_file = agent_file(context, ".mcp.json").display().to_string();
    let replacements: [(&str, &str); 13] = [
        ("{prompt}", context.prompt.as_str()),
        (
            "{model}",
            context
                .agent
                .model
                .exact_model
                .as_deref()
                .unwrap_or(&context.agent.model.id),
        ),
        ("{modelId}", context.agent.model.id.as_str()),
        ("{family}", context.agent.model.family.as_str()),
        ("{provider}", context.agent.model.provider.as_str()),
        ("{agentId}", context.agent.agent.id.as_str()),
        ("{role}", context.agent.agent.role.as_str()),
        ("{session}", context.session_id.as_deref().unwrap_or("")),
        ("{workdir}", context.workdir.as_str()),
        ("{mcpServer}", context.mcp_server_path.as_str()),
        ("{mcpConfig}", mcp_file.as_str()),
        ("{mcpJson}", mcp_json.as_str()),
        ("{qagent}", context.qagent_bin.as_str()),
    ];
    let mut output = value.to_string();
    for (token, replacement) in replacements {
        output = output.replace(token, replacement);
    }
    output
}

fn generic_command_result(stdout: &str, exit_code: i32) -> NormalizedHarnessResult {
    for row in json_lines(stdout).iter().rev() {
        let text = str_field(row, &["result", "text", "content", "message"]);
        let Some(text) = text else { continue };
        let usage = &row["usage"];
        let input = coalesce_number(&[
            &usage["input_tokens"],
            &usage["inputTokens"],
            &row["inputTokens"],
        ]);
        let output = coalesce_number(&[
            &usage["output_tokens"],
            &usage["outputTokens"],
            &row["outputTokens"],
        ]);
        let total = {
            let t = coalesce_number(&[
                &usage["total_tokens"],
                &usage["totalTokens"],
                &row["totalTokens"],
            ]);
            if t != 0.0 {
                t
            } else {
                input + output
            }
        };
        return NormalizedHarnessResult {
            text,
            session_id: str_field(row, &["session_id", "sessionId"]),
            usage: Usage {
                input_tokens: input,
                output_tokens: output,
                total_tokens: total,
                cost_usd: coalesce_number(&[
                    &usage["cost_usd"],
                    &usage["costUSD"],
                    &row["costUSD"],
                ]),
            },
            structured: Some(row.clone()),
            malformed: false,
        };
    }
    default_result(stdout, exit_code)
}

// --------------------------------------------------------- tiny regex ports

/// `/tokens used\s*\n\s*([\d,]+)/i` — codex's plain-text token footer.
fn codex_footer_tokens(clean: &str) -> Option<f64> {
    let lower = clean.to_lowercase();
    let mut search = 0;
    while let Some(pos) = lower[search..].find("tokens used") {
        let mut i = search + pos + "tokens used".len();
        while i < lower.len() && lower.as_bytes()[i].is_ascii_whitespace() {
            i += 1;
        }
        let start = i;
        while i < lower.len()
            && (lower.as_bytes()[i].is_ascii_digit() || lower.as_bytes()[i] == b',')
        {
            i += 1;
        }
        if i > start {
            return lower[start..i].replace(',', "").parse::<f64>().ok();
        }
        search += pos + 1;
    }
    None
}

/// `/kimi (?:-S|--session) (session_[\w-]+)/`
fn kimi_session_id(stdout: &str) -> Option<String> {
    for marker in ["kimi -S ", "kimi --session "] {
        if let Some(pos) = stdout.find(marker) {
            let rest = &stdout[pos + marker.len()..];
            if rest.starts_with("session_") {
                let end = rest
                    .char_indices()
                    .take_while(|(_, c)| c.is_alphanumeric() || *c == '_' || *c == '-')
                    .map(|(i, c)| i + c.len_utf8())
                    .last()
                    .unwrap_or(0);
                return Some(rest[..end].to_string());
            }
        }
    }
    None
}

/// `/"tokens":\{"total":(\d+)/g` — max.
fn kimi_total_tokens(stdout: &str) -> f64 {
    let mut max: f64 = 0.0;
    let mut search = 0;
    while let Some(pos) = stdout[search..].find("\"tokens\":{\"total\":") {
        let mut i = search + pos + "\"tokens\":{\"total\":".len();
        let start = i;
        while i < stdout.len() && stdout.as_bytes()[i].is_ascii_digit() {
            i += 1;
        }
        if i > start {
            if let Ok(n) = stdout[start..i].parse::<f64>() {
                max = max.max(n);
            }
        }
        search = search + pos + 1;
    }
    max
}

/// `/--resume\s+(\S+)/`
fn hermes_resume_session(clean: &str) -> Option<String> {
    let mut search = 0;
    while let Some(pos) = clean[search..].find("--resume") {
        let mut i = search + pos + "--resume".len();
        while i < clean.len() && clean.as_bytes()[i].is_ascii_whitespace() {
            i += 1;
        }
        let start = i;
        while i < clean.len() && !clean.as_bytes()[i].is_ascii_whitespace() {
            i += 1;
        }
        if i > start {
            return Some(clean[start..i].to_string());
        }
        search += pos + 1;
    }
    None
}

/// `/~?([\d,]+)\s*tokens/i` — the first "… tokens" count.
fn hermes_tokens(clean: &str) -> Option<f64> {
    let lower = clean.to_lowercase();
    let mut search = 0;
    while let Some(pos) = lower[search..].find("tokens") {
        // Walk back over digits/commas before "tokens" (whitespace between them allowed).
        let mut i = search + pos;
        while i > 0 && lower.as_bytes()[i - 1].is_ascii_whitespace() {
            i -= 1;
        }
        let end = i;
        while i > 0 && (lower.as_bytes()[i - 1].is_ascii_digit() || lower.as_bytes()[i - 1] == b',')
        {
            i -= 1;
        }
        // Optional leading '~'
        if i > 0 && lower.as_bytes()[i - 1] == b'~' {
            i -= 1;
        }
        if end > i {
            let digits = lower[i..end].trim_start_matches('~').replace(',', "");
            if let Ok(n) = digits.parse::<f64>() {
                return Some(n);
            }
        }
        search += pos + 1;
    }
    None
}

/// Split on `\n─{20,}\n` — lines that are only box-drawing dashes.
fn hermes_blocks(clean: &str) -> Vec<&str> {
    let mut blocks = Vec::new();
    let mut start = 0usize;
    let bytes = clean.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\n' {
            let line_start = i + 1;
            if let Some(end) = clean[line_start..].find('\n').map(|p| line_start + p) {
                let content = &clean[line_start..end];
                if content.chars().count() >= 20 && content.chars().all(|c| c == '─') {
                    blocks.push(&clean[start..i]);
                    start = end + 1;
                    i = end + 1;
                    continue;
                }
            }
        }
        i += 1;
    }
    blocks.push(&clean[start..]);
    blocks
}

// ------------------------------------------------------------------ adapters

/// Writes the `{mcpConfig}` file when the command line or env asks for it.
fn command_prepare(context: &AdapterContext) -> Result<()> {
    let mentions = |value: &serde_json::Value| value.to_string().contains("{mcpConfig}");
    if ["args", "resumeArgs", "env"]
        .iter()
        .any(|key| mentions(option(context, key)))
    {
        write_agent_file(
            &agent_file(context, ".mcp.json"),
            &mcp_servers_json(context),
        )?;
    }
    Ok(())
}

fn command_build(context: &AdapterContext) -> HarnessInvocation {
    let configured = if context.session_id.is_some() && option(context, "resumeArgs").is_array() {
        option(context, "resumeArgs")
    } else {
        option(context, "args")
    };
    let raw_args: Vec<String> = configured
        .as_array()
        .map(|a| {
            a.iter()
                .map(|v| {
                    v.as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| v.to_string())
                })
                .collect()
        })
        .unwrap_or_else(|| vec!["{prompt}".to_string()]);
    let args: Vec<String> = raw_args
        .iter()
        .map(|item| command_template_value(item, context))
        .zip(raw_args.iter())
        .filter(|(item, raw)| !item.is_empty() || raw.is_empty())
        .map(|(item, _)| item)
        .collect();
    let mut environment = common_environment(context);
    if let Some(raw_env) = option(context, "env").as_object() {
        for (key, value) in raw_env {
            let text = value
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| value.to_string());
            environment.insert(key.clone(), command_template_value(&text, context));
        }
    }
    let timeout_ms = {
        let n = as_number(option(context, "timeoutMs"));
        let n = if n == 0.0 { 60.0 * 60_000.0 } else { n };
        n.clamp(1_000.0, 24.0 * 60.0 * 60_000.0) as u64
    };
    let auto_report = if option(context, "autoReport").is_null() {
        !context.agent.harness.features.mcp
    } else {
        option(context, "autoReport").as_bool().unwrap_or(false)
    };
    HarnessInvocation {
        command: context.agent.harness.command.clone(),
        args,
        environment,
        auto_report,
        timeout_ms,
    }
}

fn claude_build(context: &AdapterContext) -> HarnessInvocation {
    let env = common_environment(context);
    let (command, args, env_merged) = mcp_launch(context, &env);
    let mcp = serde_json::json!({
        "mcpServers": {
            "qagent": { "command": command, "args": args, "env": env_merged },
        },
    })
    .to_string();
    let mut args = vec![
        "-p".to_string(),
        context.prompt.clone(),
        "--mcp-config".to_string(),
        mcp,
        "--output-format".to_string(),
        "json".to_string(),
        "--permission-mode".to_string(),
        "acceptEdits".to_string(),
        "--allowedTools".to_string(),
        "mcp__qagent,Bash,Read,Write,Edit,Glob,Grep".to_string(),
    ];
    if let Some(session) = &context.session_id {
        args.push("--resume".to_string());
        args.push(session.clone());
    }
    if let Some(model) = &context.agent.model.exact_model {
        args.push("--model".to_string());
        args.push(model.clone());
    }
    let effort = context.agent.agent.harness_options["effort"]
        .as_str()
        .unwrap_or("")
        .to_string();
    if !effort.is_empty() {
        args.push("--effort".to_string());
        args.push(effort);
    }
    let mut environment = env;
    environment.insert("MCP_TOOL_TIMEOUT".to_string(), "3600000".to_string());
    environment.insert("QAGENT_BLOCK_SEC".to_string(), "900".to_string());
    environment.insert("AGENT_BUS_BLOCK_SEC".to_string(), "900".to_string());
    HarnessInvocation {
        command: context.agent.harness.command.clone(),
        args,
        environment,
        auto_report: false,
        timeout_ms: 60 * 60_000,
    }
}

fn claude_parse(stdout: &str, exit_code: i32) -> NormalizedHarnessResult {
    for row in json_lines(stdout).iter().rev() {
        let Some(result) = row["result"].as_str() else {
            continue;
        };
        let usage = &row["usage"];
        let input = as_number(&usage["input_tokens"])
            + as_number(&usage["cache_read_input_tokens"])
            + as_number(&usage["cache_creation_input_tokens"]);
        let output = as_number(&usage["output_tokens"]);
        return NormalizedHarnessResult {
            text: result.to_string(),
            session_id: row["session_id"].as_str().map(str::to_string),
            usage: Usage {
                input_tokens: input,
                output_tokens: output,
                total_tokens: input + output,
                cost_usd: as_number(&row["total_cost_usd"]),
            },
            structured: Some(row.clone()),
            malformed: false,
        };
    }
    default_result(stdout, exit_code)
}

fn codex_build(context: &AdapterContext) -> HarnessInvocation {
    let env = common_environment(context);
    let (command, args_mcp, env_merged) = mcp_launch(context, &env);
    let mut cfg: Vec<String> = vec![
        format!("mcp_servers.agent_bus.command=\"{command}\""),
        if context.mcp_command.is_some() {
            format!(
                "mcp_servers.agent_bus.args={}",
                serde_json::to_string(&args_mcp).unwrap()
            )
        } else {
            format!(
                "mcp_servers.agent_bus.args=[\"{}\"]",
                context.mcp_server_path
            )
        },
        "mcp_servers.agent_bus.startup_timeout_sec=30".to_string(),
        "mcp_servers.agent_bus.tool_timeout_sec=300".to_string(),
        format!(
            "mcp_servers.agent_bus.env={}",
            serde_json::to_string(&env_merged).unwrap()
        ),
    ];
    let reasoning = context.agent.agent.harness_options["reasoning"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let local_provider = context.agent.agent.harness_options["localProvider"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let mut flag_args: Vec<String> = cfg
        .drain(..)
        .flat_map(|item| ["-c".to_string(), item])
        .collect();
    if !reasoning.is_empty() {
        flag_args.push("-c".to_string());
        flag_args.push(format!("model_reasoning_effort=\"{reasoning}\""));
    }
    if !local_provider.is_empty() {
        flag_args.extend([
            "--oss".to_string(),
            "--local-provider".to_string(),
            local_provider,
        ]);
    }
    // Current Codex builds cancel stdio MCP calls in every sandboxed mode.
    // Bus permissions remain enforced by the broker; the CLI must run with full
    // access for its MCP calls to reach that broker at all.
    const FULL_ACCESS: &[&str] = &["--dangerously-bypass-approvals-and-sandbox"];
    let mut args: Vec<String>;
    if let Some(pinned) = &context.pinned_session_id {
        // `queue` is the exact-thread API and lets the original chat visibly process mail.
        args = vec!["queue".to_string()];
        args.append(&mut flag_args);
        args.extend(FULL_ACCESS.iter().map(|s| s.to_string()));
        if let Some(model) = &context.agent.model.exact_model {
            args.push("-m".to_string());
            args.push(model.clone());
        }
        args.push("--thread".to_string());
        args.push(pinned.clone());
        args.push("--message".to_string());
        args.push(context.prompt.clone());
    } else {
        args = if context.session_id.is_some() {
            vec!["exec".to_string(), "resume".to_string()]
        } else {
            vec!["exec".to_string()]
        };
        args.append(&mut flag_args);
        args.extend(FULL_ACCESS.iter().map(|s| s.to_string()));
        args.extend(["--skip-git-repo-check".to_string(), "--json".to_string()]);
        if let Some(model) = &context.agent.model.exact_model {
            args.push("-m".to_string());
            args.push(model.clone());
        }
        if let Some(session) = &context.session_id {
            args.push(session.clone());
        }
        args.push(context.prompt.clone());
    }
    let mut environment = env;
    environment.insert("QAGENT_BLOCK_SEC".to_string(), "240".to_string());
    environment.insert("AGENT_BUS_BLOCK_SEC".to_string(), "240".to_string());
    HarnessInvocation {
        command: context.agent.harness.command.clone(),
        args,
        environment,
        auto_report: false,
        timeout_ms: 60 * 60_000,
    }
}

fn codex_parse(stdout: &str, exit_code: i32) -> NormalizedHarnessResult {
    let rows = json_lines(stdout);
    let mut session_id: Option<String> = None;
    let mut input_tokens: f64 = 0.0;
    let mut output_tokens: f64 = 0.0;
    let mut text: Vec<String> = Vec::new();
    for row in &rows {
        if row["type"].as_str() == Some("thread.started") {
            if let Some(id) = row["thread_id"].as_str() {
                session_id = Some(id.to_string());
            }
        }
        if row["item"]["type"].as_str() == Some("agent_message") {
            if let Some(t) = row["item"]["text"].as_str() {
                text.push(t.to_string());
            }
        }
        let usage = &row["usage"];
        input_tokens = input_tokens
            .max(as_number(&usage["input_tokens"]).max(as_number(&usage["inputTokens"])));
        output_tokens = output_tokens
            .max(as_number(&usage["output_tokens"]).max(as_number(&usage["outputTokens"])));
    }
    if !text.is_empty() {
        return NormalizedHarnessResult {
            text: text.join("\n"),
            session_id,
            usage: Usage {
                input_tokens,
                output_tokens,
                total_tokens: input_tokens + output_tokens,
                cost_usd: 0.0,
            },
            structured: Some(serde_json::json!({ "events": rows.len() })),
            malformed: false,
        };
    }
    let mut result = default_result(stdout, exit_code);
    let clean = strip_ansi(stdout);
    if let Some(tokens) = codex_footer_tokens(&clean) {
        result.usage.total_tokens = tokens;
    }
    result.session_id = session_id;
    result
}

fn kimi_build(context: &AdapterContext) -> HarnessInvocation {
    let env = common_environment(context);
    let mut args: Vec<String> = context
        .session_id
        .as_ref()
        .map(|s| vec!["--session".to_string(), s.clone()])
        .unwrap_or_default();
    // --print is Kimi's non-interactive mode; it implies --afk (tools run unasked).
    args.extend([
        "--print".to_string(),
        "--prompt".to_string(),
        context.prompt.clone(),
        "--output-format".to_string(),
        "stream-json".to_string(),
        "--mcp-config".to_string(),
        mcp_servers_json(context).to_string(),
    ]);
    if let Some(model) = &context.agent.model.exact_model {
        args.push("-m".to_string());
        args.push(model.clone());
    }
    HarnessInvocation {
        command: context.agent.harness.command.clone(),
        args,
        environment: env,
        auto_report: false,
        timeout_ms: 60 * 60_000,
    }
}

fn kimi_parse(stdout: &str, exit_code: i32) -> NormalizedHarnessResult {
    let mut result = default_result(stdout, exit_code);
    if let Some(session) = kimi_session_id(stdout) {
        result.session_id = Some(session);
    }
    for row in json_lines(stdout) {
        if let Some(id) = str_field(&row, &["session_id", "sessionId", "sessionID"]) {
            result.session_id = Some(id);
        }
    }
    result.usage.total_tokens = kimi_total_tokens(stdout).max(result.usage.total_tokens);
    result
}

/// Gemini merges the settings file named by GEMINI_CLI_SYSTEM_SETTINGS_PATH over
/// the user's, so a per-agent file hands each agent the bus as itself.
fn gemini_prepare(context: &AdapterContext) -> Result<()> {
    write_agent_file(
        &agent_file(context, ".gemini.json"),
        &mcp_servers_json(context),
    )
}

fn gemini_build(context: &AdapterContext) -> HarnessInvocation {
    let mut env = common_environment(context);
    env.insert(
        "GEMINI_CLI_SYSTEM_SETTINGS_PATH".to_string(),
        agent_file(context, ".gemini.json").display().to_string(),
    );
    let mut args = vec![
        "-p".to_string(),
        context.prompt.clone(),
        "--output-format".to_string(),
        "json".to_string(),
    ];
    if auto_approve(context) {
        // An untrusted folder forces approval back to default; the operator trusted it in aos.
        env.insert("GEMINI_CLI_TRUST_WORKSPACE".to_string(), "true".to_string());
        args.push("--approval-mode".to_string());
        args.push("yolo".to_string());
    }
    if let Some(session) = &context.session_id {
        args.push("--resume".to_string());
        args.push(session.clone());
    }
    if let Some(model) = &context.agent.model.exact_model {
        args.push("-m".to_string());
        args.push(model.clone());
    }
    HarnessInvocation {
        command: context.agent.harness.command.clone(),
        args,
        environment: env,
        auto_report: false,
        timeout_ms: 60 * 60_000,
    }
}

fn cursor_prepare(context: &AdapterContext) -> Result<()> {
    let dir = PathBuf::from(&context.workdir).join(".cursor");
    fs::create_dir_all(&dir)?;
    let cfg_path = dir.join("mcp.json");
    let mut cfg: serde_json::Value = fs::read_to_string(&cfg_path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    if !cfg.is_object() {
        cfg = serde_json::json!({});
    }
    let (command, args, env) = mcp_launch(context, &common_environment(context));
    if !cfg["mcpServers"].is_object() {
        cfg["mcpServers"] = serde_json::json!({});
    }
    cfg["mcpServers"]["qagent"] =
        serde_json::json!({ "command": command, "args": args, "env": env });
    fs::write(&cfg_path, serde_json::to_string_pretty(&cfg).unwrap())?;
    Ok(())
}

fn cursor_build(context: &AdapterContext) -> HarnessInvocation {
    let env = common_environment(context);
    let mut args = vec![
        "-p".to_string(),
        context.prompt.clone(),
        "--output-format".to_string(),
        "json".to_string(),
        "--force".to_string(),
        "--trust".to_string(),
        "--approve-mcps".to_string(),
    ];
    if let Some(session) = &context.session_id {
        args.push("--resume".to_string());
        args.push(session.clone());
    }
    if let Some(model) = &context.agent.model.exact_model {
        args.push("--model".to_string());
        args.push(model.clone());
    }
    let mut environment = env;
    environment.insert("QAGENT_BLOCK_SEC".to_string(), "900".to_string());
    environment.insert("AGENT_BUS_BLOCK_SEC".to_string(), "900".to_string());
    HarnessInvocation {
        command: context.agent.harness.command.clone(),
        args,
        environment,
        auto_report: false,
        timeout_ms: 60 * 60_000,
    }
}

fn cursor_parse(stdout: &str, exit_code: i32) -> NormalizedHarnessResult {
    for row in json_lines(stdout).iter().rev() {
        let Some(text) = str_field(row, &["result", "text", "content", "message"]) else {
            continue;
        };
        let usage = &row["usage"];
        let input = coalesce_number(&[&usage["input_tokens"], &usage["inputTokens"]]);
        let output = coalesce_number(&[&usage["output_tokens"], &usage["outputTokens"]]);
        return NormalizedHarnessResult {
            text,
            session_id: str_field(row, &["session_id", "sessionId", "chatId"]),
            usage: Usage {
                input_tokens: input,
                output_tokens: output,
                total_tokens: {
                    let t = coalesce_number(&[&usage["total_tokens"], &usage["totalTokens"]]);
                    if t != 0.0 {
                        t
                    } else {
                        input + output
                    }
                },
                cost_usd: coalesce_number(&[&usage["cost_usd"], &usage["costUSD"]]),
            },
            structured: Some(row.clone()),
            malformed: false,
        };
    }
    default_result(stdout, exit_code)
}

fn grok_build(context: &AdapterContext) -> HarnessInvocation {
    let env = common_environment(context);
    let mut args = vec![
        "-p".to_string(),
        context.prompt.clone(),
        "--output-format".to_string(),
        "json".to_string(),
    ];
    if auto_approve(context) {
        args.push("--always-approve".to_string());
    }
    if let Some(session) = &context.session_id {
        args.push("-r".to_string());
        args.push(session.clone());
    }
    if let Some(model) = &context.agent.model.exact_model {
        args.push("-m".to_string());
        args.push(model.clone());
    }
    HarnessInvocation {
        command: context.agent.harness.command.clone(),
        args,
        environment: env,
        auto_report: false,
        timeout_ms: 60 * 60_000,
    }
}

fn grok_parse(stdout: &str, exit_code: i32) -> NormalizedHarnessResult {
    for row in json_lines(stdout).iter().rev() {
        let Some(text) = str_field(row, &["result", "text", "content"]) else {
            continue;
        };
        let usage = &row["usage"];
        let input = coalesce_number(&[&usage["input_tokens"], &usage["inputTokens"]]);
        let output = coalesce_number(&[&usage["output_tokens"], &usage["outputTokens"]]);
        return NormalizedHarnessResult {
            text,
            session_id: str_field(row, &["session_id", "sessionId"]),
            usage: Usage {
                input_tokens: input,
                output_tokens: output,
                total_tokens: {
                    let t = coalesce_number(&[&usage["total_tokens"], &usage["totalTokens"]]);
                    if t != 0.0 {
                        t
                    } else {
                        input + output
                    }
                },
                cost_usd: as_number(&usage["cost_usd"]),
            },
            structured: Some(row.clone()),
            malformed: false,
        };
    }
    default_result(stdout, exit_code)
}

/// OpenCode's inline config (OPENCODE_CONFIG_CONTENT) adds the bus server for
/// this one run; the project's opencode.json is left alone.
fn opencode_config(context: &AdapterContext) -> serde_json::Value {
    let (command, mut args, env) = mcp_launch(context, &common_environment(context));
    let mut launch_args = vec![command];
    launch_args.append(&mut args);
    serde_json::json!({
        "$schema": "https://opencode.ai/config.json",
        "mcp": { "qagent": {
            "type": "local",
            "command": launch_args,
            "environment": env,
            "enabled": true,
        } },
    })
}

fn opencode_build(context: &AdapterContext) -> HarnessInvocation {
    let mut env = common_environment(context);
    env.insert(
        "OPENCODE_CONFIG_CONTENT".to_string(),
        opencode_config(context).to_string(),
    );
    let mut args = vec!["run".to_string()];
    if auto_approve(context) {
        args.push("--auto".to_string());
    }
    args.extend(["--format".to_string(), "json".to_string()]);
    if let Some(model) = &context.agent.model.exact_model {
        args.push("-m".to_string());
        args.push(model.clone());
    }
    let variant = context.agent.agent.harness_options["variant"]
        .as_str()
        .unwrap_or("")
        .to_string();
    if !variant.is_empty() {
        args.push("--variant".to_string());
        args.push(variant);
    }
    if let Some(session) = &context.session_id {
        args.push("-s".to_string());
        args.push(session.clone());
    }
    args.push(context.prompt.clone());
    HarnessInvocation {
        command: context.agent.harness.command.clone(),
        args,
        environment: env,
        auto_report: false,
        timeout_ms: 60 * 60_000,
    }
}

fn opencode_parse(stdout: &str, exit_code: i32) -> NormalizedHarnessResult {
    let rows = json_lines(stdout);
    let mut parts: Vec<String> = Vec::new();
    let mut session_id: Option<String> = None;
    let mut total_tokens: f64 = 0.0;
    for row in &rows {
        if row["part"]["type"].as_str() == Some("text") {
            if let Some(t) = row["part"]["text"].as_str() {
                parts.push(t.to_string());
            }
        }
        if let Some(id) = row["sessionID"].as_str() {
            session_id = Some(id.to_string());
        }
        total_tokens = total_tokens.max(as_number(&row["tokens"]["total"]));
    }
    if parts.is_empty() {
        return default_result(stdout, exit_code);
    }
    NormalizedHarnessResult {
        text: parts.join(""),
        session_id,
        usage: Usage {
            total_tokens,
            ..Default::default()
        },
        structured: Some(serde_json::json!({ "events": rows.len() })),
        malformed: false,
    }
}

fn hermes_build(context: &AdapterContext) -> HarnessInvocation {
    let mut env = common_environment(context);
    env.insert("HERMES_DISABLE_STREAMING".to_string(), "1".to_string());
    let profile = context.agent.agent.harness_options["profile"]
        .as_str()
        .unwrap_or("default")
        .to_string();
    let mut args = vec![
        "--profile".to_string(),
        profile,
        "chat".to_string(),
        "-q".to_string(),
        context.prompt.clone(),
        "-Q".to_string(),
        "--pass-session-id".to_string(),
    ];
    if auto_approve(context) {
        args.push("--yolo".to_string());
    }
    if let Some(session) = &context.session_id {
        args.push("--resume".to_string());
        args.push(session.clone());
    }
    HarnessInvocation {
        command: context.agent.harness.command.clone(),
        args,
        environment: env,
        auto_report: false,
        timeout_ms: 60 * 60_000,
    }
}

fn hermes_parse(stdout: &str, exit_code: i32) -> NormalizedHarnessResult {
    let clean = strip_ansi(stdout);
    let blocks = hermes_blocks(&clean);
    let text = if blocks.len() >= 2 {
        blocks[blocks.len() - 2].trim().to_string()
    } else {
        clean.trim().to_string()
    };
    let empty = text.is_empty();
    NormalizedHarnessResult {
        text: if empty {
            "(no textual output captured)".to_string()
        } else {
            text
        },
        session_id: hermes_resume_session(&clean),
        usage: Usage {
            total_tokens: hermes_tokens(&clean).unwrap_or(0.0),
            ..Default::default()
        },
        structured: None,
        malformed: exit_code == 0 && empty,
    }
}

fn fake_prepare(context: &AdapterContext) -> Result<()> {
    fs::create_dir_all(PathBuf::from(&context.workdir).join(".agent-bus"))?;
    Ok(())
}

fn fake_build(context: &AdapterContext) -> HarnessInvocation {
    let mut env = common_environment(context);
    // The fake harness cannot speak MCP; its bus-cli mode runs the same `qagent` command line instead.
    if context.mcp_command.is_some() {
        let (command, args, env_map) = mcp_launch(context, &HashMap::new());
        env.insert(
            "QAGENT_MCP_COMMAND".to_string(),
            serde_json::json!({ "command": command, "args": args, "env": env_map }).to_string(),
        );
    }
    let mode = context.agent.agent.harness_options["mode"]
        .as_str()
        .unwrap_or("success")
        .to_string();
    let mut args = vec![
        context.fake_harness_path.clone(),
        "--mode".to_string(),
        mode,
        "--agent".to_string(),
        context.agent.agent.id.clone(),
        "--prompt".to_string(),
        context.prompt.clone(),
    ];
    if let Some(session) = &context.session_id {
        args.push("--session".to_string());
        args.push(session.clone());
    }
    HarnessInvocation {
        command: context.qagent_bin.clone(),
        args,
        environment: env,
        auto_report: true,
        timeout_ms: 30_000,
    }
}

fn fake_parse(stdout: &str, exit_code: i32) -> NormalizedHarnessResult {
    let rows = json_lines(stdout);
    let Some(row) = rows.last() else {
        let mut fallback = default_result(stdout, exit_code);
        fallback.malformed = exit_code == 0;
        return fallback;
    };
    let Some(text) = row["result"].as_str() else {
        let mut fallback = default_result(stdout, exit_code);
        fallback.malformed = exit_code == 0;
        return fallback;
    };
    let usage = &row["usage"];
    NormalizedHarnessResult {
        text: text.to_string(),
        session_id: row["sessionId"].as_str().map(str::to_string),
        usage: Usage {
            input_tokens: as_number(&usage["inputTokens"]),
            output_tokens: as_number(&usage["outputTokens"]),
            total_tokens: as_number(&usage["totalTokens"]),
            cost_usd: as_number(&usage["costUSD"]),
        },
        structured: Some(row.clone()),
        malformed: false,
    }
}

static ADAPTERS: &[HarnessAdapter] = &[
    HarnessAdapter {
        id: "claude",
        prepare: None,
        build: claude_build,
        parse: claude_parse,
    },
    HarnessAdapter {
        id: "codex",
        prepare: None,
        build: codex_build,
        parse: codex_parse,
    },
    HarnessAdapter {
        id: "kimi",
        prepare: None,
        build: kimi_build,
        parse: kimi_parse,
    },
    HarnessAdapter {
        id: "gemini",
        prepare: Some(gemini_prepare),
        build: gemini_build,
        parse: default_result,
    },
    HarnessAdapter {
        id: "cursor",
        prepare: Some(cursor_prepare),
        build: cursor_build,
        parse: cursor_parse,
    },
    HarnessAdapter {
        id: "grok",
        prepare: None,
        build: grok_build,
        parse: grok_parse,
    },
    HarnessAdapter {
        id: "opencode",
        prepare: None,
        build: opencode_build,
        parse: opencode_parse,
    },
    HarnessAdapter {
        id: "hermes",
        prepare: None,
        build: hermes_build,
        parse: hermes_parse,
    },
    HarnessAdapter {
        id: "fake",
        prepare: Some(fake_prepare),
        build: fake_build,
        parse: fake_parse,
    },
    HarnessAdapter {
        id: "command",
        prepare: Some(command_prepare),
        build: command_build,
        parse: generic_command_result,
    },
];

pub fn get_harness_adapter(id: &str) -> Result<&'static HarnessAdapter> {
    ADAPTERS.iter().find(|a| a.id == id).ok_or_else(|| {
        crate::error::BusError::invalid(format!(
            "unknown harness adapter: {id}. Use adapter=command for configurable custom CLIs."
        ))
    })
}
