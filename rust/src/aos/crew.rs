//! The crew: which agent CLIs are on this computer, the team aos builds from
//! them, the files that describe it, and the supervisor processes that run it.
//!
//! Everything lives in `<bus home>/aos/` as plain files:
//!   crew.json        the team, in the same configuration format `qagent supervise --config` reads
//!   roles/*.md       role prompts, sent to each agent at the start of every turn
//!   missions/*.md    mission templates; each file name is a command in aos
//!   workdir          the folder the running crew works in
//!   trusted          folders the operator has allowed agents to work in
//! aos writes a preset file only when it is missing, so edits are never overwritten.

use crate::bus::Bus;
use crate::config::{load_config, BusConfig};
use crate::db::home_for;
use crate::error::{BusError, Result};
use crate::types::OPERATOR_ID;
use serde_json::{json, Value};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// One agent CLI aos knows how to find.
pub struct Cli {
    pub id: &'static str,
    pub name: &'static str,
    pub binaries: &'static [&'static str],
    pub provider: &'static str,
    pub family: &'static str,
    pub adapter: &'static str,
    /// aos puts this CLI in a crew without asking first.
    pub crew_ready: bool,
    /// It gets the bus tools (MCP) on every turn. Without them the supervisor
    /// claims and submits for it, and it reaches the bus with `$QAGENT_CLI`.
    pub tools: bool,
    /// Its adapter runs tools without approval prompts, so it joins a crew only
    /// after the operator allows that with `connect <id> --auto-approve`.
    pub auto_approve: bool,
    pub install: &'static str,
    pub sign_in: &'static str,
}

/// Crew-ready CLIs first, in the order aos prefers them. Any other CLI joins
/// through `connect <name> -- <command line>` (the generic command adapter).
pub const CLIS: &[Cli] = &[
    Cli {
        id: "claude",
        name: "Claude Code",
        binaries: &["claude"],
        provider: "anthropic",
        family: "claude",
        adapter: "claude",
        crew_ready: true,
        tools: true,
        auto_approve: false,
        install: "curl -fsSL https://claude.ai/install.sh | bash",
        sign_in: "run claude once and sign in",
    },
    Cli {
        id: "codex",
        name: "Codex CLI",
        binaries: &["codex"],
        provider: "openai",
        family: "gpt",
        adapter: "codex",
        crew_ready: true,
        tools: true,
        auto_approve: false,
        install: "npm install -g @openai/codex",
        sign_in: "run codex login",
    },
    Cli {
        id: "cursor",
        name: "Cursor CLI",
        binaries: &["cursor-agent"],
        provider: "cursor",
        family: "cursor",
        adapter: "cursor",
        crew_ready: true,
        tools: true,
        auto_approve: false,
        install: "curl https://cursor.com/install -fsS | bash",
        sign_in: "run cursor-agent login",
    },
    Cli {
        id: "gemini",
        name: "Gemini CLI",
        binaries: &["gemini"],
        provider: "google",
        family: "gemini",
        adapter: "gemini",
        crew_ready: false,
        tools: true,
        auto_approve: true,
        install: "npm install -g @google/gemini-cli",
        sign_in: "run gemini once and sign in",
    },
    Cli {
        id: "hermes",
        name: "Hermes Agent",
        binaries: &["hermes"],
        provider: "hermes",
        family: "hermes",
        adapter: "hermes",
        crew_ready: false,
        tools: false,
        auto_approve: true,
        install: "curl -fsSL https://hermes-agent.nousresearch.com/install.sh | bash",
        sign_in: "run hermes setup",
    },
    Cli {
        id: "opencode",
        name: "OpenCode",
        binaries: &["opencode"],
        provider: "opencode",
        family: "opencode",
        adapter: "opencode",
        crew_ready: false,
        tools: true,
        auto_approve: true,
        install: "see opencode.ai",
        sign_in: "run opencode auth login",
    },
    Cli {
        id: "kimi",
        name: "Kimi Code",
        binaries: &["kimi"],
        provider: "moonshot",
        family: "kimi",
        adapter: "kimi",
        crew_ready: false,
        tools: true,
        auto_approve: true,
        install: "see the Kimi Code docs",
        sign_in: "run kimi once and sign in",
    },
    Cli {
        id: "grok",
        name: "Grok CLI",
        binaries: &["grok"],
        provider: "xai",
        family: "grok",
        adapter: "grok",
        crew_ready: false,
        tools: false,
        auto_approve: true,
        install: "see the Grok CLI docs",
        sign_in: "run grok login",
    },
    Cli {
        id: "devin",
        name: "Devin CLI",
        binaries: &["devin"],
        provider: "cognition",
        family: "devin",
        adapter: "devin",
        crew_ready: false,
        tools: false,
        auto_approve: true,
        install: "curl -fsSL https://cli.devin.ai/install.sh | bash",
        sign_in: "run devin auth login",
    },
    Cli {
        id: "qwen",
        name: "Qwen Code",
        binaries: &["qwen"],
        provider: "qwen",
        family: "qwen",
        adapter: "qwen",
        crew_ready: false,
        tools: true,
        auto_approve: true,
        install: "npm install -g @qwen-code/qwen-code",
        sign_in: "run qwen once and sign in",
    },
    Cli {
        id: "copilot",
        name: "GitHub Copilot CLI",
        binaries: &["copilot"],
        provider: "github",
        family: "copilot",
        adapter: "copilot",
        crew_ready: false,
        tools: true,
        auto_approve: true,
        install: "npm install -g @github/copilot",
        sign_in: "run copilot login",
    },
    Cli {
        id: "amp",
        name: "Amp",
        binaries: &["amp"],
        provider: "sourcegraph",
        family: "amp",
        adapter: "amp",
        crew_ready: false,
        tools: true,
        auto_approve: true,
        install: "npm install -g @sourcegraph/amp",
        sign_in: "run amp login",
    },
    Cli {
        id: "auggie",
        name: "Auggie (Augment)",
        binaries: &["auggie"],
        provider: "augment",
        family: "auggie",
        adapter: "auggie",
        crew_ready: false,
        tools: true,
        auto_approve: true,
        install: "npm install -g @augmentcode/auggie",
        sign_in: "run auggie login",
    },
    Cli {
        id: "kilo",
        name: "Kilo CLI",
        binaries: &["kilo", "kilocode"],
        provider: "kilo",
        family: "kilo",
        adapter: "kilo",
        crew_ready: false,
        tools: true,
        auto_approve: true,
        install: "npm install -g @kilocode/cli",
        sign_in: "run kilo auth login",
    },
    Cli {
        id: "goose",
        name: "Goose",
        binaries: &["goose"],
        provider: "block",
        family: "goose",
        adapter: "goose",
        crew_ready: false,
        tools: true,
        auto_approve: true,
        install: "see block.github.io/goose for the CLI installer",
        sign_in: "run goose configure",
    },
    Cli {
        id: "crush",
        name: "Crush",
        binaries: &["crush"],
        provider: "charm",
        family: "crush",
        adapter: "crush",
        crew_ready: false,
        tools: false,
        auto_approve: true,
        install: "npm install -g @charmland/crush",
        sign_in: "run crush once and pick a provider",
    },
    Cli {
        id: "vibe",
        name: "Mistral Vibe",
        binaries: &["vibe"],
        provider: "mistral",
        family: "vibe",
        adapter: "vibe",
        crew_ready: false,
        tools: false,
        auto_approve: true,
        install: "uv tool install --python 3.12 mistral-vibe",
        sign_in: "run vibe once and add your Mistral key",
    },
    Cli {
        id: "cline",
        name: "Cline CLI",
        binaries: &["cline"],
        provider: "cline",
        family: "cline",
        adapter: "cline",
        crew_ready: false,
        tools: false,
        auto_approve: true,
        install: "npm install -g cline",
        sign_in: "run cline auth",
    },
    Cli {
        id: "continue",
        name: "Continue CLI",
        binaries: &["cn"],
        provider: "continue",
        family: "continue",
        adapter: "continue",
        crew_ready: false,
        tools: false,
        auto_approve: true,
        install: "npm install -g @continuedev/cli",
        sign_in: "run cn login",
    },
    Cli {
        id: "aider",
        name: "Aider",
        binaries: &["aider"],
        provider: "aider",
        family: "aider",
        adapter: "aider",
        crew_ready: false,
        tools: false,
        auto_approve: true,
        install: "python -m pip install aider-install && aider-install",
        sign_in: "set your model's API key, as aider's docs say",
    },
    Cli {
        id: "amazonq",
        name: "Amazon Q Developer CLI",
        binaries: &["q"],
        provider: "aws",
        family: "amazonq",
        adapter: "amazonq",
        crew_ready: false,
        tools: false,
        auto_approve: true,
        install: "see aws.amazon.com/q/developer for the CLI installer",
        sign_in: "run q login",
    },
];

pub fn cli(id: &str) -> Option<&'static Cli> {
    CLIS.iter().find(|c| c.id == id)
}

/// What detection found for one CLI.
#[derive(Clone)]
pub struct Found {
    pub cli: &'static Cli,
    pub path: Option<PathBuf>,
    pub version: Option<String>,
}

impl Found {
    pub fn usable(&self) -> bool {
        self.path.is_some() && self.cli.crew_ready
    }
    /// Usable, or allowed by the operator to run with auto-approval.
    pub fn joinable(&self, allowed: &[String]) -> bool {
        self.usable()
            || (self.path.is_some()
                && self.cli.auto_approve
                && allowed.iter().any(|a| a == self.cli.id))
    }
}

fn search_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    if let Some(home) = dirs::home_dir() {
        for d in [
            ".local/bin",
            "bin",
            ".claude/local",
            ".npm-global/bin",
            ".cursor/bin",
        ] {
            dirs.push(home.join(d));
        }
    }
    for d in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"] {
        dirs.push(PathBuf::from(d));
    }
    dirs
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

pub fn find_binary(name: &str) -> Option<PathBuf> {
    search_dirs()
        .into_iter()
        .map(|d| d.join(name))
        .find(|p| is_executable(p))
}

/// `<cli> --version`, first line, or None if it does not answer within 3s.
pub fn probe_version(path: &Path) -> Option<String> {
    let mut child = Command::new(path)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    let first = out
        .lines()
        .find(|l| !l.trim().is_empty())?
        .trim()
        .to_string();
    Some(first.chars().take(40).collect())
}

pub fn detect() -> Vec<Found> {
    CLIS.iter()
        .map(|cli| {
            let path = cli.binaries.iter().find_map(|b| find_binary(b));
            let version = path.as_deref().and_then(probe_version);
            Found { cli, path, version }
        })
        .collect()
}

// ------------------------------------------------------------------ files

pub struct Paths {
    pub home: PathBuf,
    pub dir: PathBuf,
}

impl Paths {
    pub fn for_db(db_path: &Path) -> Paths {
        let home = home_for(db_path);
        let dir = home.join("aos");
        Paths { home, dir }
    }
    pub fn crew(&self) -> PathBuf {
        self.dir.join("crew.json")
    }
    pub fn roles(&self) -> PathBuf {
        self.dir.join("roles")
    }
    pub fn missions(&self) -> PathBuf {
        self.dir.join("missions")
    }
    pub fn workdir_file(&self) -> PathBuf {
        self.dir.join("workdir")
    }
    /// Lines typed in command home, newest last, for up and down.
    pub fn history_file(&self) -> PathBuf {
        self.dir.join("history")
    }
    pub fn trusted_file(&self) -> PathBuf {
        self.dir.join("trusted")
    }
    /// CLIs the operator allowed to run tools without approval prompts.
    pub fn auto_approve_file(&self) -> PathBuf {
        self.dir.join("auto-approve")
    }
    pub fn pid_file(&self, agent: &str) -> PathBuf {
        self.home.join("supervisors").join(format!("{agent}.pid"))
    }
    pub fn out_file(&self, agent: &str) -> PathBuf {
        self.home.join("logs").join(format!("{agent}.out"))
    }
    pub fn log_file(&self, agent: &str) -> PathBuf {
        self.home.join("logs").join(format!("{agent}.log"))
    }
    pub fn session_file(&self, agent: &str) -> PathBuf {
        self.home.join("sessions").join(format!("{agent}.json"))
    }
    /// `~/...` when under the home directory: shorter and the same on screen as in docs.
    pub fn show(path: &Path) -> String {
        match dirs::home_dir() {
            Some(h) if path.starts_with(&h) && path != h => {
                format!("~/{}", path.strip_prefix(&h).unwrap().display())
            }
            _ => path.display().to_string(),
        }
    }
}

pub const ROLE_PRESETS: &[(&str, &str)] = &[
    ("lead", include_str!("../../presets/roles/lead.md")),
    ("builder", include_str!("../../presets/roles/builder.md")),
    ("reviewer", include_str!("../../presets/roles/reviewer.md")),
    (
        "researcher",
        include_str!("../../presets/roles/researcher.md"),
    ),
];

pub const MISSION_PRESETS: &[(&str, &str)] = &[
    ("run", include_str!("../../presets/missions/run.md")),
    ("build", include_str!("../../presets/missions/build.md")),
    ("fix", include_str!("../../presets/missions/fix.md")),
    (
        "research",
        include_str!("../../presets/missions/research.md"),
    ),
    ("review", include_str!("../../presets/missions/review.md")),
    ("explain", include_str!("../../presets/missions/explain.md")),
    ("docs", include_str!("../../presets/missions/docs.md")),
];

/// Write each preset that is missing. Returns how many were written.
pub fn write_presets(paths: &Paths) -> Result<usize> {
    let mut written = 0;
    for (dir, presets) in [
        (paths.roles(), ROLE_PRESETS),
        (paths.missions(), MISSION_PRESETS),
    ] {
        fs::create_dir_all(&dir)?;
        for (name, text) in presets {
            let path = dir.join(format!("{name}.md"));
            if !path.exists() {
                fs::write(&path, text)?;
                written += 1;
            }
        }
    }
    Ok(written)
}

/// A prompt file without its leading `<!-- ... -->` note for humans.
pub fn strip_note(text: &str) -> &str {
    let t = text.trim_start();
    if let Some(rest) = t.strip_prefix("<!--") {
        if let Some(end) = rest.find("-->") {
            return rest[end + 3..].trim_start();
        }
    }
    t
}

// ------------------------------------------------------------------ the crew

pub struct Member {
    pub id: &'static str,
    pub role: &'static str,
    pub authority: &'static str,
    pub cli: &'static Cli,
    pub description: &'static str,
}

fn pick<'a>(ready: &[&'a Found], order: &[&str], not_family: Option<&str>) -> Option<&'a Found> {
    order
        .iter()
        .filter_map(|id| ready.iter().find(|f| f.cli.id == *id).copied())
        .find(|f| not_family.is_none_or(|fam| f.cli.family != fam))
}

/// The default crew: a lead, a builder and a reviewer. With more than one CLI
/// the reviewer comes from a different model family than the builder, so
/// every change is checked by a second vendor.
pub fn plan(found: &[Found]) -> Vec<Member> {
    plan_with(found, &[])
}

/// The default crew, also using the auto-approving CLIs in `allowed`. The lead
/// hands out tasks through the bus tools, so only a CLI with them can lead.
pub fn plan_with(found: &[Found], allowed: &[String]) -> Vec<Member> {
    let ready: Vec<&Found> = found.iter().filter(|f| f.joinable(allowed)).collect();
    const LATER: [&str; 18] = [
        "gemini", "opencode", "kimi", "hermes", "grok", "devin", "qwen", "copilot", "amp",
        "auggie", "kilo", "goose", "crush", "vibe", "cline", "continue", "aider", "amazonq",
    ];
    let order = |first: [&'static str; 3]| -> Vec<&'static str> {
        first.into_iter().chain(LATER).collect()
    };
    let leads: Vec<&Found> = ready.iter().copied().filter(|f| f.cli.tools).collect();
    let Some(lead) = pick(&leads, &order(["claude", "codex", "cursor"]), None) else {
        return Vec::new();
    };
    let builder = pick(&ready, &order(["codex", "claude", "cursor"]), None).unwrap();
    let reviewer = pick(
        &ready,
        &order(["claude", "codex", "cursor"]),
        Some(builder.cli.family),
    )
    .unwrap_or(builder);
    vec![
        Member {
            id: "lead",
            role: "manager",
            authority: "manager",
            cli: lead.cli,
            description: "plans the goal, hands out tasks, hands back the result",
        },
        Member {
            id: "builder",
            role: "implementation",
            authority: "worker",
            cli: builder.cli,
            description: "makes the changes and runs the checks",
        },
        Member {
            id: "reviewer",
            role: "reviewer",
            authority: "worker",
            cli: reviewer.cli,
            description: "checks every change before you see it",
        },
    ]
}

fn capabilities() -> Value {
    // Placeholder heuristics; nothing in aos routes on them. Required by the
    // configuration format.
    json!({
        "coding": 0.8, "reasoning": 0.8, "planning": 0.8, "debugging": 0.8, "research": 0.7,
        "toolUse": 0.8, "speed": 0.5, "tokenEfficiency": 0.6, "reliability": 0.8, "autonomy": 0.8,
        "contextTokens": 200000, "costClass": "subscription",
        "source": "heuristic-default", "notes": "Placeholder, not a benchmark claim."
    })
}

fn roles() -> Value {
    json!({
        "manager": {"id":"manager","description":"Owns objective decomposition, escalation and final integration.","capabilityWeights":{"reasoning":1,"planning":1,"reliability":0.8,"autonomy":0.8,"toolUse":0.5}},
        "implementation": {"id":"implementation","description":"Implements scoped code changes.","capabilityWeights":{"coding":1,"debugging":0.8,"toolUse":0.8,"reliability":0.6,"autonomy":0.7}},
        "research": {"id":"research","description":"Investigates repository or external evidence and returns structured findings.","capabilityWeights":{"research":1,"reasoning":0.7,"toolUse":0.6}},
        "reviewer": {"id":"reviewer","description":"Independently reviews code, evidence and validation results.","capabilityWeights":{"reasoning":0.8,"coding":0.8,"debugging":0.8,"reliability":1}}
    })
}

/// The crew in the `qagent supervise --config` format, plus the aos fields
/// (`instructions`, `description`) that the TypeScript side ignores.
pub fn crew_json(members: &[Member], found: &[Found]) -> Value {
    let mut providers = serde_json::Map::new();
    let mut harnesses = serde_json::Map::new();
    let mut models = serde_json::Map::new();
    let mut agents = serde_json::Map::new();
    for m in members {
        let c = m.cli;
        let command = found
            .iter()
            .find(|f| f.cli.id == c.id)
            .and_then(|f| f.path.clone())
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| c.binaries[0].to_string());
        providers.insert(
            c.provider.into(),
            json!({"id": c.provider, "displayName": c.name, "enabled": true, "subscriptionBacked": true,
                   "authKind": "subscription", "authSource": c.sign_in}),
        );
        harnesses.insert(c.id.into(), harness_json(c, &command));
        models.insert(c.id.into(), model_json(c.id, c.provider, c.family));
        agents.insert(
            m.id.into(),
            json!({"id": m.id, "model": c.id, "role": m.role, "authority": m.authority,
                   "description": m.description, "enabled": true, "autoStart": false,
                   "instructions": format!("roles/{}.md", m.id),
                   "permissions": {"canDelegate": m.authority == "manager", "canReview": true,
                                   "filesystem": if m.id == "reviewer" { "read" } else { "write" },
                                   "shell": true, "network": true, "maxDelegationDepth": 2}}),
        );
    }
    json!({
        "version": 1,
        "about": "Your aos crew. Plain JSON you can edit: change an agent's model to another CLI listed under models, add exactModel to a model to pin one, or copy an agent block to add a teammate (its instructions file lives in roles/). Run aos doctor after editing.",
        "providers": providers, "harnesses": harnesses, "models": models, "agents": agents,
        "roles": roles(), "routing": {},
        "constraints": {"maxDelegationDepth": 2, "maxConcurrentTasks": 4, "maxRetries": 2}
    })
}

fn harness_json(c: &Cli, command: &str) -> Value {
    let mut h = json!({"id": c.id, "adapter": c.adapter, "command": command, "providers": [c.provider],
           "features": {"headless": true, "resume": true, "mcp": c.tools, "structuredOutput": true,
                        "streaming": true, "cancellation": true, "modelSelection": true,
                        "reasoningControl": c.id != "cursor", "usageReporting": true},
           "probeArgs": ["--version"], "enabled": true});
    if c.auto_approve {
        // Written only once the operator allowed it; set false to keep approval prompts.
        h["options"] = json!({"autoApprove": true});
    }
    h
}

fn model_json(id: &str, provider: &str, family: &str) -> Value {
    json!({"id": id, "provider": provider, "harness": id, "family": family,
           "capabilities": capabilities(), "enabled": true,
           "notes": "No exactModel: the CLI uses its own default model. Set exactModel to pin one."})
}

pub fn load_crew(paths: &Paths) -> Result<Option<BusConfig>> {
    if !paths.crew().exists() {
        return Ok(None);
    }
    load_config(&paths.crew()).map(Some)
}

/// Agent ids in the crew file, lead first.
pub fn member_ids(config: &BusConfig) -> Vec<String> {
    let mut ids: Vec<String> = config
        .agents
        .values()
        .filter(|a| a.enabled)
        .map(|a| a.id.clone())
        .collect();
    ids.sort_by_key(|id| (config.agents[id].authority != "manager", id.clone()));
    ids
}

/// Put every crew member on the bus. Existing agents are left as they are.
pub fn sync_bus(bus: &Bus, config: &BusConfig) -> Result<Vec<String>> {
    let op = bus.identify(Some(OPERATOR_ID))?;
    let lead = config
        .agents
        .values()
        .find(|a| a.authority == "manager" && a.enabled)
        .map(|a| a.id.clone());
    let mut added = Vec::new();
    for id in member_ids(config) {
        if bus.get_agent(&id)?.is_some() {
            continue;
        }
        let a = &config.agents[&id];
        let harness = config.models.get(&a.model).map(|m| m.harness.clone());
        let parent = lead.as_deref().filter(|l| *l != id.as_str());
        let authority = if a.authority == "manager" {
            "manager"
        } else {
            "worker"
        };
        bus.add_agent(
            &op,
            &id,
            Some(&a.role),
            Some(&a.model),
            harness.as_deref(),
            parent,
            Some(authority),
        )?;
        added.push(id);
    }
    Ok(added)
}

pub struct SetupReport {
    pub members: Vec<(String, String)>,
    pub wrote_crew: bool,
    pub presets_written: usize,
    pub added: Vec<String>,
}

/// Write presets, the crew file (unless one exists and `force` is false) and
/// put the crew on the bus.
pub fn setup(bus: &Bus, paths: &Paths, found: &[Found], force: bool) -> Result<SetupReport> {
    fs::create_dir_all(&paths.dir)?;
    let presets_written = write_presets(paths)?;
    let members = plan_with(found, &allowed(paths));
    let mut wrote_crew = false;
    if force || !paths.crew().exists() {
        if members.is_empty() {
            return Err(BusError::invalid(
                "no crew-ready agent CLI found / install Claude Code, Codex CLI or Cursor CLI, then run aos setup, or add any CLI with aos connect",
            ));
        }
        let text = serde_json::to_string_pretty(&crew_json(&members, found))?;
        fs::write(paths.crew(), format!("{text}\n"))?;
        wrote_crew = true;
    }
    let config = load_crew(paths)?.ok_or_else(|| BusError::invalid("crew.json is missing"))?;
    let added = sync_bus(bus, &config)?;
    let members = member_ids(&config)
        .into_iter()
        .map(|id| {
            let h = config
                .models
                .get(&config.agents[&id].model)
                .map(|m| m.harness.clone())
                .unwrap_or_default();
            (id, h)
        })
        .collect();
    Ok(SetupReport {
        members,
        wrote_crew,
        presets_written,
        added,
    })
}

// ------------------------------------------------------------------ connect

/// CLIs the operator allowed to run with auto-approval (one id per line).
pub fn allowed(paths: &Paths) -> Vec<String> {
    fs::read_to_string(paths.auto_approve_file())
        .unwrap_or_default()
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect()
}

fn set_allowed(paths: &Paths, id: &str, on: bool) -> Result<()> {
    let mut ids = allowed(paths);
    ids.retain(|a| a != id);
    if on {
        ids.push(id.to_string());
    }
    fs::create_dir_all(&paths.dir)?;
    let mut text = String::from(
        "# CLIs allowed to run tools without approval prompts (aos connect <id> --auto-approve)\n",
    );
    for a in ids {
        text.push_str(&a);
        text.push('\n');
    }
    fs::write(paths.auto_approve_file(), text)?;
    Ok(())
}

/// What `connect` was asked to do.
#[derive(Debug, Default)]
pub struct Connect {
    /// A CLI aos knows (claude, gemini, ...) or a new name for any other CLI.
    pub name: String,
    /// The agent to give it: an existing seat (lead, builder, reviewer) switches
    /// to it, a new id adds a teammate. Defaults to `name`.
    pub seat: Option<String>,
    /// For any other CLI: its command line, with {prompt} where the brief goes.
    pub command: Vec<String>,
    pub auto_approve: bool,
}

/// The role, authority and preset prompt for a new seat, from its name.
fn seat_kind(seat: &str) -> (&'static str, &'static str, &'static str) {
    match seat {
        "lead" | "manager" => ("manager", "manager", "lead"),
        "reviewer" | "review" => ("reviewer", "worker", "reviewer"),
        "researcher" | "research" => ("research", "worker", "researcher"),
        _ => ("implementation", "worker", "builder"),
    }
}

/// Put a CLI in the crew: register it in crew.json (harness, model, provider)
/// and give it a seat. Any CLI works: one aos knows uses its own adapter;
/// any other runs through the generic command adapter, with the bus tools
/// when its command line passes {mcpConfig} or {mcpJson}, otherwise with the
/// supervisor claiming and submitting for it. Returns what changed.
pub fn connect(bus: &Bus, paths: &Paths, found: &[Found], req: &Connect) -> Result<Vec<String>> {
    let name = crate::identity::assert_safe_agent_id(req.name.trim())?.to_string();
    let seat =
        crate::identity::assert_safe_agent_id(req.seat.as_deref().unwrap_or(&name))?.to_string();
    let mut said = Vec::new();
    let mut allow: Option<&str> = None;
    let (provider, harness, model, tools, label) = if req.command.is_empty() {
        let Some(c) = cli(&name) else {
            let known: Vec<&str> = CLIS.iter().map(|c| c.id).collect();
            return Err(BusError::invalid(format!(
                "aos doesn't know {name} / known: {}; for any other CLI give its command line: connect {name} -- <command> {{prompt}}",
                known.join(" ")
            )));
        };
        let Some(path) = found
            .iter()
            .find(|f| f.cli.id == c.id)
            .and_then(|f| f.path.clone())
        else {
            return Err(BusError::invalid(format!(
                "{} isn't installed / {}",
                c.name, c.install
            )));
        };
        if c.auto_approve && !req.auto_approve && !allowed(paths).iter().any(|a| a == c.id) {
            return Err(BusError::invalid(format!(
                "{} would run commands and edit files without asking you / type connect {} --auto-approve to allow that",
                c.name, c.id
            )));
        }
        if req.auto_approve && c.auto_approve {
            allow = Some(c.id);
        }
        (
            json!({"id": c.provider, "displayName": c.name, "enabled": true, "subscriptionBacked": true,
                   "authKind": "subscription", "authSource": c.sign_in}),
            harness_json(c, &path.display().to_string()),
            model_json(c.id, c.provider, c.family),
            c.tools,
            c.provider.to_string(),
        )
    } else {
        let bin = &req.command[0];
        let path = if bin.contains('/') {
            Some(PathBuf::from(bin)).filter(|p| is_executable(p))
        } else {
            find_binary(bin)
        };
        let Some(path) = path else {
            return Err(BusError::invalid(format!("no program {bin} found on PATH")));
        };
        let mut args: Vec<String> = req.command[1..].to_vec();
        if !args.iter().any(|a| a.contains("{prompt}")) {
            args.push("{prompt}".to_string());
            said.push("no {prompt} in the command line / the brief goes last".to_string());
        }
        let tools = args
            .iter()
            .any(|a| a.contains("{mcpConfig}") || a.contains("{mcpJson}"));
        let resume = args.iter().any(|a| a.contains("{session}"));
        (
            json!({"id": name, "displayName": name, "enabled": true, "subscriptionBacked": false,
                   "authKind": "cli", "authSource": "the CLI's own sign-in"}),
            json!({"id": name, "adapter": "command", "command": path.display().to_string(), "providers": [name],
                   "features": {"headless": true, "resume": resume, "mcp": tools, "structuredOutput": false,
                                "streaming": false, "cancellation": true, "modelSelection": false,
                                "reasoningControl": false, "usageReporting": false},
                   "options": {"args": args}, "enabled": true}),
            model_json(&name, &name, &name),
            tools,
            name.clone(),
        )
    };

    // The crew file to change: the existing one, a fresh default, or an empty one.
    fs::create_dir_all(&paths.dir)?;
    write_presets(paths)?;
    let before = fs::read_to_string(paths.crew()).ok();
    let mut crew: Value = match &before {
        Some(text) => serde_json::from_str(text)
            .map_err(|e| BusError::invalid(format!("crew.json isn't valid JSON: {e}")))?,
        None => crew_json(&plan_with(found, &allowed(paths)), found),
    };
    crew["providers"][&label] = provider;
    crew["harnesses"][&name] = harness;
    crew["models"][&name] = model;
    let (role, authority, preset) = match crew["agents"][&seat].as_object() {
        Some(a) => (
            a.get("role")
                .and_then(Value::as_str)
                .unwrap_or("implementation")
                .to_string(),
            a.get("authority")
                .and_then(Value::as_str)
                .unwrap_or("worker")
                .to_string(),
            "",
        ),
        None => {
            let (r, a, p) = seat_kind(&seat);
            (r.to_string(), a.to_string(), p)
        }
    };
    if authority == "manager" && !tools {
        return Err(BusError::invalid(format!(
            "{seat} hands out tasks through the bus tools, and {name} has none / give {name} another seat, or pass {{mcpConfig}} in its command line"
        )));
    }
    if crew["agents"][&seat].is_object() {
        crew["agents"][&seat]["model"] = json!(name);
        said.push(format!("{seat} now runs on {name}"));
    } else {
        let role_file = paths.roles().join(format!("{seat}.md"));
        if !role_file.exists() {
            let text = ROLE_PRESETS
                .iter()
                .find(|(n, _)| *n == preset)
                .map(|(_, t)| *t)
                .unwrap_or("");
            fs::write(&role_file, text)?;
        }
        crew["agents"][&seat] = json!({"id": seat, "model": name, "role": role, "authority": authority,
            "description": format!("{role} on {name}"), "enabled": true, "autoStart": false,
            "instructions": format!("roles/{seat}.md"),
            "permissions": {"canDelegate": authority == "manager", "canReview": true,
                            "filesystem": if role == "reviewer" { "read" } else { "write" },
                            "shell": true, "network": true, "maxDelegationDepth": 2}});
        said.push(format!("{seat} joined the crew on {name} as {role}"));
    }
    said.push(if tools {
        format!("{name} gets the bus tools every turn")
    } else {
        format!("{name} has no bus tools: its supervisor claims tasks for it and submits its answer; it can message the team with $QAGENT_CLI")
    });

    let text = format!("{}\n", serde_json::to_string_pretty(&crew)?);
    fs::write(paths.crew(), &text)?;
    let config = match load_config(&paths.crew()) {
        Ok(c) => c,
        Err(e) => {
            match &before {
                Some(old) => fs::write(paths.crew(), old)?,
                None => fs::remove_file(paths.crew())?,
            }
            return Err(BusError::invalid(format!(
                "crew.json left as it was / {}",
                e.message
            )));
        }
    };
    sync_bus(bus, &config)?;
    if let Some(id) = allow {
        set_allowed(paths, id, true)?;
        said.push(format!(
            "{id} may run tools without asking (remove it from {} to undo)",
            Paths::show(&paths.auto_approve_file())
        ));
    }
    if running_pid(paths, &seat).is_some() {
        said.push(format!(
            "{seat} is running on its old CLI / stop {seat}, then start {seat}"
        ));
    }
    Ok(said)
}

/// Take a CLI out of the crew. A teammate named after it goes too; any other
/// seat on it has to move first, so the crew never points at a missing CLI.
pub fn disconnect(paths: &Paths, name: &str) -> Result<Vec<String>> {
    let text = fs::read_to_string(paths.crew())
        .map_err(|_| BusError::invalid("no crew yet / setup makes one"))?;
    let mut crew: Value = serde_json::from_str(&text)
        .map_err(|e| BusError::invalid(format!("crew.json isn't valid JSON: {e}")))?;
    if !crew["harnesses"][name].is_object() {
        return Err(BusError::invalid(format!("{name} isn't in your crew")));
    }
    let models: Vec<String> = crew["models"]
        .as_object()
        .map(|m| {
            m.iter()
                .filter(|(_, v)| v["harness"].as_str() == Some(name))
                .map(|(k, _)| k.clone())
                .collect()
        })
        .unwrap_or_default();
    let on_it: Vec<String> = crew["agents"]
        .as_object()
        .map(|a| {
            a.iter()
                .filter(|(_, v)| {
                    v["model"]
                        .as_str()
                        .is_some_and(|m| models.iter().any(|x| x == m))
                })
                .map(|(k, _)| k.clone())
                .collect()
        })
        .unwrap_or_default();
    let others: Vec<&String> = on_it.iter().filter(|id| *id != name).collect();
    if !others.is_empty() {
        let ids: Vec<&str> = others.iter().map(|s| s.as_str()).collect();
        return Err(BusError::invalid(format!(
            "{} still run on {name} / connect another CLI to them first, e.g. connect claude as {}",
            ids.join(", "),
            ids[0]
        )));
    }
    if running_pid(paths, name).is_some() {
        return Err(BusError::invalid(format!(
            "{name} is running / stop {name} first"
        )));
    }
    let mut said = Vec::new();
    if let Some(a) = crew["agents"].as_object_mut() {
        if a.remove(name).is_some() {
            said.push(format!("{name} left the crew"));
        }
    }
    let provider = crew["models"][&models.first().cloned().unwrap_or_default()]["provider"]
        .as_str()
        .map(str::to_string);
    for m in &models {
        crew["models"].as_object_mut().map(|o| o.remove(m));
    }
    crew["harnesses"].as_object_mut().map(|o| o.remove(name));
    if let Some(p) = provider {
        let used = crew["models"].as_object().is_some_and(|o| {
            o.values()
                .any(|v| v["provider"].as_str() == Some(p.as_str()))
        });
        if !used {
            crew["providers"].as_object_mut().map(|o| o.remove(&p));
        }
    }
    fs::write(
        paths.crew(),
        format!("{}\n", serde_json::to_string_pretty(&crew)?),
    )?;
    load_config(&paths.crew())?;
    if allowed(paths).iter().any(|a| a == name) {
        set_allowed(paths, name, false)?;
    }
    said.push(format!("{name} is no longer in crew.json"));
    Ok(said)
}

/// One line per CLI: installed or not, in the crew or not, and how it reaches the bus.
pub fn connections(paths: &Paths, found: &[Found]) -> Vec<(char, String, String)> {
    let config = load_crew(paths).ok().flatten();
    let ok = allowed(paths);
    let seats = |harness: &str| -> Vec<String> {
        let Some(c) = &config else { return Vec::new() };
        let mut ids: Vec<String> = c
            .agents
            .values()
            .filter(|a| c.models.get(&a.model).is_some_and(|m| m.harness == harness))
            .map(|a| a.id.clone())
            .collect();
        ids.sort();
        ids
    };
    let link = |tools: bool| {
        if tools {
            "bus tools"
        } else {
            "supervisor-managed"
        }
    };
    let mut out = Vec::new();
    for f in found {
        let c = f.cli;
        let on = seats(c.id);
        let (mark, what) = if !on.is_empty() {
            (
                '+',
                format!("in crew: {} / {}", on.join(", "), link(c.tools)),
            )
        } else if f.path.is_none() {
            ('-', format!("not installed / {}", c.install))
        } else if f.joinable(&ok) {
            (
                '~',
                format!(
                    "found / connect {} [as <agent>] adds it ({})",
                    c.id,
                    link(c.tools)
                ),
            )
        } else {
            (
                '~',
                format!(
                    "found / runs tools without asking; connect {} --auto-approve adds it",
                    c.id
                ),
            )
        };
        out.push((mark, c.id.to_string(), what));
    }
    if let Some(c) = &config {
        let mut custom: Vec<&crate::config::HarnessDef> = c
            .harnesses
            .values()
            .filter(|h| cli(&h.id).is_none())
            .collect();
        custom.sort_by(|a, b| a.id.cmp(&b.id));
        for h in custom {
            let on = seats(&h.id);
            let what = if on.is_empty() {
                format!("{} / no agent on it", h.command)
            } else {
                format!(
                    "in crew: {} / {} / {}",
                    on.join(", "),
                    link(h.features.mcp),
                    h.command
                )
            };
            out.push(('+', h.id.clone(), what));
        }
    }
    out
}

// ------------------------------------------------------------------ missions

#[derive(Debug, Clone)]
pub struct Mission {
    pub name: String,
    pub summary: String,
    pub brief: String,
    pub acceptance: String,
}

pub fn parse_mission(name: &str, text: &str) -> Mission {
    let body = strip_note(text);
    let mut summary = String::new();
    let mut brief = String::new();
    let mut acceptance = String::new();
    let mut section = "";
    for line in body.lines() {
        if line.starts_with("# ") {
            section = "head";
            continue;
        }
        if let Some(h) = line.strip_prefix("## ") {
            section = match h.trim().to_lowercase().as_str() {
                "brief" => "brief",
                "acceptance" => "acceptance",
                _ => "",
            };
            continue;
        }
        let target = match section {
            "head" if summary.is_empty() && !line.trim().is_empty() => {
                summary = line.trim().to_string();
                continue;
            }
            "brief" => &mut brief,
            "acceptance" => &mut acceptance,
            _ => continue,
        };
        target.push_str(line);
        target.push('\n');
    }
    if !brief.contains("{goal}") {
        brief = format!("{{goal}}\n\n{brief}");
    }
    Mission {
        name: name.to_string(),
        summary,
        brief: brief.trim().to_string(),
        acceptance: acceptance.trim().to_string(),
    }
}

/// Mission files in the operator's folder, falling back to the presets when
/// the folder does not exist yet. Sorted with `run` first.
pub fn missions(paths: &Paths) -> Vec<Mission> {
    let mut out: Vec<Mission> = match fs::read_dir(paths.missions()) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "md"))
            .filter_map(|p| {
                let name = p.file_stem()?.to_string_lossy().to_lowercase();
                let ok = !name.is_empty()
                    && name
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
                let text = fs::read_to_string(&p).ok()?;
                ok.then(|| parse_mission(&name, &text))
            })
            .collect(),
        Err(_) => MISSION_PRESETS
            .iter()
            .map(|(n, t)| parse_mission(n, t))
            .collect(),
    };
    out.sort_by_key(|m| (m.name != "run", m.name.clone()));
    out
}

pub fn expand(m: &Mission, goal: &str) -> (String, String, String) {
    let title: String = if m.name == "run" {
        goal.to_string()
    } else {
        format!("{} {}", m.name, goal)
    };
    let title = if title.chars().count() > 120 {
        format!("{}...", title.chars().take(117).collect::<String>())
    } else {
        title
    };
    (
        title,
        m.brief.replace("{goal}", goal),
        m.acceptance.replace("{goal}", goal),
    )
}

// ------------------------------------------------------------------ role prompts

/// The role prompt for one agent, from its `instructions` file (relative to
/// the crew file), with `{team}` filled in. Read on every turn so edits apply
/// at once.
pub fn role_prompt(config_path: &Path, agent_id: &str) -> Option<String> {
    let raw: Value = serde_json::from_str(&fs::read_to_string(config_path).ok()?).ok()?;
    let rel = raw["agents"][agent_id]["instructions"].as_str()?;
    let base = config_path.parent()?;
    let text = fs::read_to_string(base.join(rel)).ok()?;
    let mut team: Vec<String> = Vec::new();
    if let Some(agents) = raw["agents"].as_object() {
        let mut ids: Vec<&String> = agents.keys().collect();
        ids.sort();
        for id in ids {
            let a = &agents[id];
            if id == agent_id || a["enabled"] == false {
                continue;
            }
            let model = a["model"].as_str().unwrap_or("");
            team.push(format!(
                "- {id} ({}, {}): {}",
                a["role"].as_str().unwrap_or(""),
                model,
                a["description"].as_str().unwrap_or("")
            ));
        }
    }
    team.push("- operator: the person you work for; they accept or return results in aos".into());
    Some(strip_note(&text).replace("{team}", &team.join("\n")))
}

// ------------------------------------------------------------------ processes

fn alive(pid: i32) -> bool {
    pid > 0
        && (unsafe { libc::kill(pid, 0) } == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM))
}

/// The pid of this agent's supervisor, if one is running.
pub fn running_pid(paths: &Paths, agent: &str) -> Option<i32> {
    let pid: i32 = fs::read_to_string(paths.pid_file(agent))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    alive(pid).then_some(pid)
}

pub fn crew_workdir(paths: &Paths) -> Option<PathBuf> {
    fs::read_to_string(paths.workdir_file())
        .ok()
        .map(|s| PathBuf::from(s.trim()))
        .filter(|p| !p.as_os_str().is_empty())
}

pub fn is_trusted(paths: &Paths, dir: &Path) -> bool {
    fs::read_to_string(paths.trusted_file())
        .map(|s| s.lines().any(|l| Path::new(l.trim()) == dir))
        .unwrap_or(false)
}

pub fn trust(paths: &Paths, dir: &Path) -> Result<()> {
    if is_trusted(paths, dir) {
        return Ok(());
    }
    fs::create_dir_all(&paths.dir)?;
    let mut text = fs::read_to_string(paths.trusted_file()).unwrap_or_default();
    text.push_str(&format!("{}\n", dir.display()));
    fs::write(paths.trusted_file(), text)?;
    Ok(())
}

/// Why agents must not work in this folder, if they must not.
pub fn unsafe_workdir(dir: &Path) -> Option<String> {
    if dir == Path::new("/") {
        return Some("aos is in / . cd into a project folder first".into());
    }
    if dirs::home_dir().is_some_and(|h| h == dir) {
        return Some(
            "aos is in your home folder, and agents can edit any file where they work. cd into a project folder first (or make one: mkdir ~/aos-work && cd ~/aos-work)".into(),
        );
    }
    None
}

/// Start a supervisor for each agent in the background, working in `workdir`.
/// Each keeps running after aos exits. Returns one line per agent.
pub fn start(
    db_path: &Path,
    paths: &Paths,
    ids: &[String],
    workdir: &Path,
) -> Vec<(String, Result<i32>)> {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("aos"));
    let _ = fs::create_dir_all(paths.home.join("logs"));
    let _ = fs::write(paths.workdir_file(), format!("{}\n", workdir.display()));
    let mut spawned: Vec<(String, Result<i32>)> = Vec::new();
    for id in ids {
        if let Some(pid) = running_pid(paths, id) {
            spawned.push((id.clone(), Ok(pid)));
            continue;
        }
        let out = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(paths.out_file(id));
        let r = out.map_err(BusError::from).and_then(|out| {
            let err = out.try_clone()?;
            use std::os::unix::process::CommandExt;
            let mut cmd = Command::new(&exe);
            cmd.arg("--db")
                .arg(db_path)
                .arg("supervise")
                .arg(id)
                .arg(workdir)
                .arg("--config")
                .arg(paths.crew())
                .current_dir(workdir)
                .stdin(Stdio::null())
                .stdout(out)
                .stderr(err);
            // Its own session: closing the terminal or quitting aos leaves it running.
            unsafe {
                cmd.pre_exec(|| {
                    libc::setsid();
                    Ok(())
                });
            }
            let mut child = cmd.spawn()?;
            let pid = child.id() as i32;
            // Reap it if it exits while aos is still open.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            Ok(pid)
        });
        spawned.push((id.clone(), r));
    }
    // A supervisor that cannot start (bad config, agent missing) exits at once;
    // give it a moment and report its last words instead of a false start.
    let deadline = Instant::now() + Duration::from_millis(1200);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
        if spawned
            .iter()
            .all(|(id, r)| r.is_err() || running_pid(paths, id).is_some())
        {
            break;
        }
    }
    spawned
        .into_iter()
        .map(|(id, r)| {
            let r = r.and_then(|pid| {
                if alive(pid) {
                    Ok(pid)
                } else {
                    Err(BusError::invalid(last_words(paths, &id)))
                }
            });
            (id, r)
        })
        .collect()
}

/// The last meaningful line a supervisor wrote, for error messages.
pub fn last_words(paths: &Paths, agent: &str) -> String {
    let text = fs::read_to_string(paths.out_file(agent)).unwrap_or_default();
    text.lines()
        .rev()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .map(|l| {
            // Drop the supervisor's timestamp: "[2026-...Z] text".
            let l = match l.find("] ") {
                Some(i) if l.starts_with('[') => &l[i + 2..],
                _ => l,
            };
            let l = l.strip_prefix("qagent: ").unwrap_or(l);
            l.chars().take(200).collect()
        })
        .unwrap_or_else(|| "it stopped right after starting / see aos doctor".into())
}

/// Ask each agent's supervisor to stop (SIGINT, the signal it handles: it
/// stops its CLI's process group first), then wait for it.
pub fn stop(paths: &Paths, ids: &[String]) -> Vec<(String, Result<bool>)> {
    let mut asked = Vec::new();
    for id in ids {
        match running_pid(paths, id) {
            Some(pid) => {
                unsafe {
                    libc::kill(pid, libc::SIGINT);
                }
                asked.push((id.clone(), Some(pid)));
            }
            None => asked.push((id.clone(), None)),
        }
    }
    let deadline = Instant::now() + Duration::from_secs(6);
    while Instant::now() < deadline && asked.iter().any(|(_, p)| p.is_some_and(alive)) {
        std::thread::sleep(Duration::from_millis(100));
    }
    asked
        .into_iter()
        .map(|(id, pid)| {
            let r = match pid {
                None => Ok(false),
                Some(p) if alive(p) => Err(BusError::invalid(format!(
                    "{id} (pid {p}) is still stopping / it finishes its current step first"
                ))),
                Some(_) => Ok(true),
            };
            (id, r)
        })
        .collect()
}

/// Cost and tokens the CLIs reported, per agent, from the supervisor's session files.
pub fn usage(paths: &Paths, ids: &[String]) -> (f64, f64) {
    let mut cost = 0.0;
    let mut tokens = 0.0;
    for id in ids {
        if let Ok(text) = fs::read_to_string(paths.session_file(id)) {
            if let Ok(v) = serde_json::from_str::<Value>(&text) {
                cost += v["costUSD"].as_f64().unwrap_or(0.0);
                tokens += v["totalTokens"].as_f64().unwrap_or(0.0);
            }
        }
    }
    (cost, tokens)
}

// ------------------------------------------------------------------ the view

#[derive(Debug, Clone, Default)]
pub struct MemberInfo {
    pub id: String,
    pub role: String,
    pub cli: String,
    pub description: String,
    pub pid: Option<i32>,
    /// When stopped: the last line its supervisor wrote, if any.
    pub last_words: Option<String>,
}

/// What the screens show about the crew. Read from files, never from the bus.
#[derive(Debug, Clone, Default)]
pub struct CrewInfo {
    /// False when there is no crew file: the welcome screen applies.
    pub configured: bool,
    /// The crew file exists but does not load.
    pub error: Option<String>,
    pub members: Vec<MemberInfo>,
    pub workdir: Option<String>,
    pub dir: String,
    pub cost_usd: f64,
    pub tokens: f64,
    pub missions: Vec<(String, String)>,
    /// Detection results, when they were gathered (welcome, setup, crew screen).
    pub found: Vec<Found>,
}

impl std::fmt::Debug for Found {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{:?}", self.cli.id, self.path)
    }
}

impl CrewInfo {
    pub fn running(&self) -> usize {
        self.members.iter().filter(|m| m.pid.is_some()).count()
    }
    pub fn lead(&self) -> Option<&MemberInfo> {
        self.members.first()
    }
}

pub fn gather(paths: &Paths, found: Vec<Found>) -> CrewInfo {
    let mut info = CrewInfo {
        dir: Paths::show(&paths.dir),
        workdir: crew_workdir(paths).map(|p| Paths::show(&p)),
        missions: missions(paths)
            .into_iter()
            .map(|m| (m.name, m.summary))
            .collect(),
        found,
        ..Default::default()
    };
    match load_crew(paths) {
        Ok(None) => {}
        Ok(Some(config)) => {
            info.configured = true;
            let ids = member_ids(&config);
            let (cost, tokens) = usage(paths, &ids);
            info.cost_usd = cost;
            info.tokens = tokens;
            info.members = ids
                .iter()
                .map(|id| {
                    let a = &config.agents[id];
                    let pid = running_pid(paths, id);
                    let last = (pid.is_none() && paths.out_file(id).exists())
                        .then(|| last_words(paths, id))
                        .filter(|l| l != "supervisor stopped");
                    MemberInfo {
                        id: id.clone(),
                        role: a.role.clone(),
                        cli: config
                            .models
                            .get(&a.model)
                            .map(|m| m.harness.clone())
                            .unwrap_or_default(),
                        description: a.description.clone(),
                        pid,
                        last_words: last,
                    }
                })
                .collect();
        }
        Err(e) => {
            info.configured = true;
            info.error = Some(e.message);
        }
    }
    info
}

// ------------------------------------------------------------------ doctor

pub struct Check {
    /// Some(true) fine, Some(false) needs fixing, None for information.
    pub ok: Option<bool>,
    pub label: String,
    pub detail: String,
}

impl Check {
    pub fn mark(&self) -> &'static str {
        match self.ok {
            Some(true) => "+",
            Some(false) => "x",
            None => "-",
        }
    }
}

fn check(ok: Option<bool>, label: &str, detail: impl Into<String>) -> Check {
    Check {
        ok,
        label: label.into(),
        detail: detail.into(),
    }
}

/// Read-only checks, each with what to do when it fails. Runs each crew CLI's
/// `--version`, nothing else.
pub fn doctor(db_path: &Path) -> Vec<Check> {
    let paths = Paths::for_db(db_path);
    let mut out = vec![check(
        Some(true),
        "aos",
        format!(
            "{} / {}",
            env!("CARGO_PKG_VERSION"),
            std::env::current_exe()
                .map(|p| Paths::show(&p))
                .unwrap_or_default()
        ),
    )];
    if !db_path.exists() {
        out.push(check(
            None,
            "bus",
            format!(
                "{} not created yet / aos makes it the first time it opens",
                Paths::show(db_path)
            ),
        ));
    } else {
        match Bus::open(Some(db_path)) {
            Ok(bus) => {
                if bus.identify(Some(OPERATOR_ID)).is_ok() {
                    out.push(check(
                        Some(true),
                        "bus",
                        format!("{} / you can write", Paths::show(db_path)),
                    ));
                } else {
                    out.push(check(
                        Some(false),
                        "bus",
                        format!(
                            "{} / your operator token is missing or belongs to another bus, so aos opens read only. Put the right operator.token back, or run aos init (which issues a new token)",
                            Paths::show(db_path)
                        ),
                    ));
                }
            }
            Err(e) => out.push(check(
                Some(false),
                "bus",
                format!("{} does not open: {}", Paths::show(db_path), e.message),
            )),
        }
    }
    let config = match load_crew(&paths) {
        Ok(None) => {
            out.push(check(None, "crew", "none yet / run aos setup, or just aos"));
            None
        }
        Ok(Some(c)) => {
            out.push(check(
                Some(true),
                "crew",
                format!(
                    "{} / {}",
                    Paths::show(&paths.crew()),
                    member_ids(&c).join(", ")
                ),
            ));
            Some(c)
        }
        Err(e) => {
            out.push(check(
                Some(false),
                "crew",
                format!(
                    "{} does not load: {} / fix it, or aos setup --force",
                    Paths::show(&paths.crew()),
                    e.message
                ),
            ));
            None
        }
    };
    if let Some(config) = &config {
        let raw: Value = fs::read_to_string(paths.crew())
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(Value::Null);
        for id in member_ids(config) {
            let a = &config.agents[&id];
            let Some(model) = config.models.get(&a.model) else {
                continue;
            };
            let Some(h) = config.harnesses.get(&model.harness) else {
                continue;
            };
            let known = cli(&h.id);
            let path = if h.command.contains('/') {
                Some(PathBuf::from(&h.command)).filter(|p| is_executable(p))
            } else {
                find_binary(&h.command)
            };
            match path {
                Some(p) => {
                    let v = probe_version(&p).unwrap_or_else(|| "no --version answer".into());
                    out.push(check(
                        Some(true),
                        &id,
                        format!(
                            "{} at {} ({v}){}",
                            h.id,
                            Paths::show(&p),
                            known
                                .map(|c| format!(" / signed in? if turns fail: {}", c.sign_in))
                                .unwrap_or_default()
                        ),
                    ));
                }
                None => out.push(check(
                    Some(false),
                    &id,
                    format!(
                        "{} not found at {} / install: {}, or fix its command in crew.json",
                        h.id,
                        h.command,
                        known.map(|c| c.install).unwrap_or("see its docs")
                    ),
                )),
            }
            match running_pid(&paths, &id) {
                Some(pid) => out.push(check(
                    Some(true),
                    "",
                    format!(
                        "running / pid {pid} / log {}",
                        Paths::show(&paths.log_file(&id))
                    ),
                )),
                None => {
                    let last = if paths.out_file(&id).exists() {
                        format!(" / last: {}", last_words(&paths, &id))
                    } else {
                        String::new()
                    };
                    out.push(check(None, "", format!("stopped{last}")));
                }
            }
            if let Some(rel) = raw["agents"][&id]["instructions"].as_str() {
                let file = paths.dir.join(rel);
                if !file.exists() {
                    out.push(check(
                        Some(false),
                        "",
                        format!(
                            "role prompt {} is missing / aos setup restores it",
                            Paths::show(&file)
                        ),
                    ));
                }
            }
        }
        match crew_workdir(&paths) {
            Some(d) => out.push(check(
                None,
                "folder",
                format!("the crew last worked in {}", Paths::show(&d)),
            )),
            None => out.push(check(
                None,
                "folder",
                "the crew has not run yet / it works where you start it",
            )),
        }
    }
    let ms = missions(&paths);
    out.push(check(
        None,
        "missions",
        format!(
            "{} / {}",
            ms.len(),
            ms.iter()
                .map(|m| m.name.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        ),
    ));
    let others: Vec<(String, bool)> = detect()
        .into_iter()
        .filter(|f| f.path.is_some())
        .filter(|f| {
            config
                .as_ref()
                .is_none_or(|c| !c.harnesses.contains_key(f.cli.id))
        })
        .map(|f| (f.cli.id.to_string(), f.cli.crew_ready))
        .collect();
    let ready: Vec<String> = others.iter().filter(|o| o.1).map(|o| o.0.clone()).collect();
    let not_ready: Vec<String> = others
        .iter()
        .filter(|o| !o.1)
        .map(|o| o.0.clone())
        .collect();
    if !ready.is_empty() {
        let detail = if config.is_some() {
            format!(
                "{} / not in your crew; aos setup --force rebuilds it",
                ready.join(", ")
            )
        } else {
            format!("{} / aos setup puts them in a crew", ready.join(", "))
        };
        out.push(check(None, "found", detail));
    }
    if !not_ready.is_empty() {
        out.push(check(
            None,
            "found",
            format!(
                "{} / runs tools without asking; aos connect <name> --auto-approve adds it",
                not_ready.join(", ")
            ),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(ids: &[&str]) -> Vec<Found> {
        CLIS.iter()
            .map(|cli| Found {
                cli,
                path: ids
                    .contains(&cli.id)
                    .then(|| PathBuf::from(format!("/bin/{}", cli.id))),
                version: None,
            })
            .collect()
    }

    #[test]
    fn one_cli_fills_every_seat() {
        let m = plan(&found(&["claude"]));
        assert_eq!(
            m.iter().map(|m| m.cli.id).collect::<Vec<_>>(),
            ["claude"; 3]
        );
    }

    #[test]
    fn reviewer_comes_from_another_family_when_it_can() {
        let m = plan(&found(&["claude", "codex"]));
        let ids: Vec<_> = m.iter().map(|m| (m.id, m.cli.id)).collect();
        assert_eq!(
            ids,
            [
                ("lead", "claude"),
                ("builder", "codex"),
                ("reviewer", "claude")
            ]
        );
    }

    #[test]
    fn cli_that_cannot_join_is_never_planned() {
        assert!(plan(&found(&["hermes", "gemini"])).is_empty());
    }

    #[test]
    fn crew_file_loads_as_a_supervise_config() {
        let dir = std::env::temp_dir().join(format!("aos-crew-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let f = found(&["claude", "codex"]);
        let path = dir.join("crew.json");
        fs::write(&path, crew_json(&plan(&f), &f).to_string()).unwrap();
        let config = load_config(&path).unwrap();
        assert_eq!(member_ids(&config), ["lead", "builder", "reviewer"]);
        let agent = crate::config::resolve_agent(&config, "builder").unwrap();
        assert_eq!(agent.harness.adapter, "codex");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn mission_expands_goal_into_brief() {
        let m = parse_mission("fix", MISSION_PRESETS[2].1);
        assert_eq!(m.summary, "Fix something that is broken.");
        let (title, brief, acc) = expand(&m, "login fails on empty password");
        assert_eq!(title, "fix login fails on empty password");
        assert!(brief.contains("The operator reports this problem: login fails on empty password"));
        assert!(acc.contains("reproduction"));
        assert!(!brief.contains("<!--"));
    }

    #[test]
    fn role_prompt_names_the_team() {
        let dir = std::env::temp_dir().join(format!("aos-role-{}", std::process::id()));
        let paths = Paths {
            home: dir.clone(),
            dir: dir.join("aos"),
        };
        write_presets(&paths).unwrap();
        let f = found(&["claude"]);
        fs::write(paths.crew(), crew_json(&plan(&f), &f).to_string()).unwrap();
        let text = role_prompt(&paths.crew(), "lead").unwrap();
        assert!(text.starts_with("You are the lead"));
        assert!(text.contains("- builder (implementation, claude)"));
        assert!(!text.contains("{team}"));
        fs::remove_dir_all(&dir).unwrap();
    }
}
