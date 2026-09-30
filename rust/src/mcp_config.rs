//! `qagent mcp-config [--agent ID] [--client claude|codex] [--operator]`
//!
//! Port of src/mcp/config.ts: prints the registration snippet that starts
//! `qagent mcp` for one agent — Claude Code (`~/.claude.json` "mcpServers") or
//! Codex (`~/.codex/config.toml` `[mcp_servers.NAME]`). The command is the
//! absolute path of the current `qagent` binary, so it works without qagent on
//! the client's PATH. Tokens stay in files; the snippet carries only
//! QAGENT_AGENT_ID (and QAGENT_BUS_DB when the bus is not the default).

use crate::db::{home_for, resolve_db_path_with};
use crate::error::{BusError, Result};
use crate::identity::{assert_safe_agent_id, token_path_for};
use crate::types::{MAX_WAIT_SEC, OPERATOR_ID};
use serde_json::json;

const USAGE: &str = "usage: qagent mcp-config [--agent ID] [--client claude|codex] [--operator] [--name NAME]\n\nPrints the MCP registration for Claude Code (~/.claude.json \"mcpServers\") and\nCodex (~/.codex/config.toml [mcp_servers.NAME]). --agent defaults to QAGENT_AGENT_ID.\n";

pub struct McpLaunch {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

pub fn launch_for(
    agent_id: &str,
    db_path: &str,
    operator: bool,
    name: Option<&str>,
    qagent_bin: &str,
) -> Result<McpLaunch> {
    if agent_id != OPERATOR_ID {
        assert_safe_agent_id(agent_id)?;
    }
    let mut env = vec![("QAGENT_AGENT_ID".to_string(), agent_id.to_string())];
    // Only pin the database when it differs from what the client resolves with
    // no bus variables set.
    let default_db = resolve_db_path_with(None, |_| None);
    if db_path != default_db.to_string_lossy() {
        env.push(("QAGENT_BUS_DB".to_string(), db_path.to_string()));
    }
    let mut args = vec!["mcp".to_string()];
    if operator {
        args.push("--operator".to_string());
    }
    Ok(McpLaunch {
        name: name.unwrap_or("qagent").to_string(),
        command: qagent_bin.to_string(),
        args,
        env,
    })
}

pub fn claude_snippet(launch: &McpLaunch) -> String {
    let env: serde_json::Map<String, serde_json::Value> = launch
        .env
        .iter()
        .map(|(k, v)| (k.clone(), json!(v)))
        .collect();
    serde_json::to_string_pretty(&json!({
        "mcpServers": {
            launch.name.clone(): {
                "type": "stdio",
                "command": launch.command,
                "args": launch.args,
                "env": env,
            }
        }
    }))
    .unwrap_or_default()
}

/// TOML basic strings accept JSON string escapes for the characters paths and
/// ids contain.
fn toml_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

fn is_plain_toml_key(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn toml_key(value: &str) -> String {
    if is_plain_toml_key(value) {
        value.to_string()
    } else {
        toml_string(value)
    }
}

pub fn codex_snippet(launch: &McpLaunch) -> String {
    let env = launch
        .env
        .iter()
        .map(|(k, v)| format!("{} = {}", toml_key(k), toml_string(v)))
        .collect::<Vec<_>>()
        .join(", ");
    let args = launch
        .args
        .iter()
        .map(|a| toml_string(a))
        .collect::<Vec<_>>()
        .join(", ");
    [
        format!("[mcp_servers.{}]", toml_key(&launch.name)),
        format!("command = {}", toml_string(&launch.command)),
        format!("args = [{args}]"),
        format!("env = {{ {env} }}"),
        // bus_wait blocks for up to an hour; Codex's default tool timeout is shorter.
        format!("tool_timeout_sec = {}", MAX_WAIT_SEC + 60),
    ]
    .join("\n")
}

pub struct Args {
    pub agent: Option<String>,
    pub client: Option<String>,
    pub operator: bool,
    pub name: Option<String>,
    pub help: bool,
}

/// Entry point for `qagent mcp-config` (port of config.ts main). Returns the
/// process exit code.
pub fn run(
    parsed: Args,
    db_path: &str,
    env: &dyn Fn(&str) -> Option<String>,
    stdout: &mut dyn std::io::Write,
    stderr: &mut dyn std::io::Write,
) -> i32 {
    if parsed.help {
        let _ = write!(stdout, "{USAGE}");
        return 0;
    }
    let mut work = || -> Result<()> {
        let agent_id = match parsed.agent.clone() {
            Some(id) => Some(id),
            None if parsed.operator => Some(OPERATOR_ID.to_string()),
            None => env("QAGENT_AGENT_ID")
                .or_else(|| env("AGENT_ID"))
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty()),
        };
        let agent_id = agent_id
            .ok_or_else(|| BusError::invalid("pass --agent <id> (or set QAGENT_AGENT_ID)"))?;
        if parsed.operator && agent_id != OPERATOR_ID {
            return Err(BusError::invalid(
                "--operator registers the operator identity; drop --agent",
            ));
        }
        if let Some(name) = &parsed.name {
            if !is_plain_toml_key(name) {
                return Err(BusError::invalid(
                    "--name may use letters, digits, '_' and '-'",
                ));
            }
        }
        // The snippet runs this same binary.
        let qagent_bin = std::env::current_exe()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| "qagent".to_string());
        let launch = launch_for(
            &agent_id,
            db_path,
            parsed.operator,
            parsed.name.as_deref(),
            &qagent_bin,
        )?;
        let home = home_for(std::path::Path::new(db_path));
        let token_path = token_path_for(&home, &agent_id)?;
        if !token_path.exists() {
            let _ = writeln!(
                stderr,
                "qagent mcp-config: warning: no token file at {}; run `qagent agent add {agent_id} --role ...` first.",
                token_path.display()
            );
        }
        match parsed.client.as_deref() {
            Some("claude") => {
                let _ = writeln!(stdout, "{}", claude_snippet(&launch));
            }
            Some("codex") => {
                let _ = writeln!(stdout, "{}", codex_snippet(&launch));
            }
            _ => {
                let _ = writeln!(
                    stdout,
                    "{}",
                    [
                        "# Claude Code: merge into ~/.claude.json (user scope) or .mcp.json (project scope)".to_string(),
                        claude_snippet(&launch),
                        String::new(),
                        "# Codex: add to ~/.codex/config.toml".to_string(),
                        codex_snippet(&launch),
                        String::new(),
                    ]
                    .join("\n")
                );
            }
        }
        Ok(())
    };
    match work() {
        Ok(()) => 0,
        Err(e) => {
            let _ = writeln!(stderr, "qagent mcp-config: {}", e.message);
            1
        }
    }
}
