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
    /// aos hands this CLI the bus tools on every turn, so it can work in a crew.
    pub crew_ready: bool,
    pub install: &'static str,
    pub sign_in: &'static str,
}

/// Crew-ready CLIs first, in the order aos prefers them.
pub const CLIS: &[Cli] = &[
    Cli {
        id: "claude",
        name: "Claude Code",
        binaries: &["claude"],
        provider: "anthropic",
        family: "claude",
        adapter: "claude",
        crew_ready: true,
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
        install: "see the Grok CLI docs",
        sign_in: "run grok login",
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
    pub sign_in: SignIn,
}

impl Found {
    /// Installed, able to join a crew, and not known to be signed out.
    pub fn usable(&self) -> bool {
        self.path.is_some() && self.cli.crew_ready && !matches!(self.sign_in, SignIn::Missing)
    }
}

/// Whether a CLI is signed in, judged only from files and environment
/// variables. aos never runs a turn to find out, so where the CLI keeps its
/// sign-in somewhere aos cannot read (a keychain), it says it does not know.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SignIn {
    /// Evidence of a sign-in: what was found.
    Found(String),
    /// The CLI keeps its sign-in only where aos looked, and there is none.
    Missing,
    /// aos cannot tell: why.
    Unknown(String),
}

/// Where each crew-ready CLI keeps its sign-in. `env` reads a variable,
/// `home` is the user's home folder.
pub fn sign_in_for(
    cli_id: &str,
    env: &dyn Fn(&str) -> Option<String>,
    home: &Path,
    macos: bool,
) -> SignIn {
    let set = |name: &str| env(name).is_some_and(|v| !v.trim().is_empty());
    let first_set = |names: &[&str]| names.iter().find(|n| set(n)).map(|n| format!("{n} is set"));
    match cli_id {
        "claude" => {
            if let Some(e) = first_set(&[
                "ANTHROPIC_API_KEY",
                "CLAUDE_CODE_OAUTH_TOKEN",
                "CLAUDE_CODE_USE_BEDROCK",
                "CLAUDE_CODE_USE_VERTEX",
            ]) {
                return SignIn::Found(e);
            }
            let dir = env("CLAUDE_CONFIG_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".claude"));
            let creds = dir.join(".credentials.json");
            if creds.is_file() {
                return SignIn::Found(format!("{} exists", Paths::show(&creds)));
            }
            let state = home.join(".claude.json");
            if fs::read_to_string(&state).is_ok_and(|t| t.contains("\"oauthAccount\"")) {
                return SignIn::Found(format!("{} has an account", Paths::show(&state)));
            }
            if macos {
                SignIn::Unknown("Claude Code may keep it in the macOS Keychain".into())
            } else {
                SignIn::Missing
            }
        }
        "codex" => {
            if let Some(e) = first_set(&["OPENAI_API_KEY", "CODEX_API_KEY"]) {
                return SignIn::Found(e);
            }
            let dir = env("CODEX_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".codex"));
            let auth = dir.join("auth.json");
            if auth.is_file() {
                return SignIn::Found(format!("{} exists", Paths::show(&auth)));
            }
            // Codex can be told to keep its sign-in in the system keyring instead.
            let keyring = fs::read_to_string(dir.join("config.toml"))
                .is_ok_and(|t| t.contains("cli_auth_credentials_store"));
            if keyring {
                SignIn::Unknown("Codex is set to keep it in the system keyring".into())
            } else {
                SignIn::Missing
            }
        }
        "cursor" => match first_set(&["CURSOR_API_KEY"]) {
            Some(e) => SignIn::Found(e),
            None => SignIn::Unknown("Cursor CLI keeps it where aos can't read".into()),
        },
        _ => SignIn::Unknown("aos does not check this CLI".into()),
    }
}

fn sign_in_here(cli_id: &str) -> SignIn {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
    sign_in_for(
        cli_id,
        &|name| std::env::var(name).ok(),
        &home,
        cfg!(target_os = "macos"),
    )
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
            let sign_in = if path.is_some() && cli.crew_ready {
                sign_in_here(cli.id)
            } else {
                SignIn::Unknown("not checked".into())
            };
            Found {
                cli,
                path,
                version,
                sign_in,
            }
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

/// The default crew: a builder and a reviewer from a different model family,
/// so every change gets an independent check. With only one family installed
/// there is no reviewer agent: the operator reviews, because a reviewer from
/// the same family is not independent. A lead (manager) is not in the default
/// crew; add one to crew.json to have goals planned and split.
pub fn plan(found: &[Found]) -> Vec<Member> {
    let ready: Vec<&Found> = found.iter().filter(|f| f.usable()).collect();
    if ready.is_empty() {
        return Vec::new();
    }
    let builder = pick(&ready, &["codex", "claude", "cursor"], None).unwrap();
    let mut crew = vec![Member {
        id: "builder",
        role: "implementation",
        authority: "worker",
        cli: builder.cli,
        description: "does the task and runs the checks",
    }];
    if let Some(reviewer) = pick(
        &ready,
        &["claude", "codex", "cursor"],
        Some(builder.cli.family),
    ) {
        crew.push(Member {
            id: "reviewer",
            role: "reviewer",
            authority: "worker",
            cli: reviewer.cli,
            description: "reviews every result independently before it counts as done",
        });
    }
    crew
}

/// Who reviews a goal in this crew: the reviewer agent when its model family
/// differs from the agent doing the work, otherwise the operator.
pub fn reviewer_for(config: &BusConfig, worker: &str) -> String {
    let family = |id: &str| {
        config
            .agents
            .get(id)
            .and_then(|a| config.models.get(&a.model))
            .map(|m| m.family.clone())
    };
    config
        .agents
        .values()
        .filter(|a| a.enabled && a.role == "reviewer" && a.id != worker)
        .find(|a| family(&a.id).is_some() && family(&a.id) != family(worker))
        .map(|a| a.id.clone())
        .unwrap_or_else(|| OPERATOR_ID.to_string())
}

/// The agent a goal goes to: the lead when the crew has one, else the first
/// worker that is not a reviewer.
pub fn goal_owner(config: &BusConfig) -> Option<String> {
    let ids = member_ids(config);
    ids.iter()
        .find(|id| config.agents[*id].authority == "manager")
        .or_else(|| ids.iter().find(|id| config.agents[*id].role != "reviewer"))
        .or_else(|| ids.first())
        .cloned()
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
        harnesses.insert(
            c.id.into(),
            json!({"id": c.id, "adapter": c.adapter, "command": command, "providers": [c.provider],
                   "features": {"headless": true, "resume": true, "mcp": true, "structuredOutput": true,
                                "streaming": true, "cancellation": true, "modelSelection": true,
                                "reasoningControl": c.id != "cursor", "usageReporting": true},
                   "probeArgs": ["--version"], "enabled": true}),
        );
        models.insert(
            c.id.into(),
            json!({"id": c.id, "provider": c.provider, "harness": c.id, "family": c.family,
                   "capabilities": capabilities(), "enabled": true,
                   "notes": "No exactModel: the CLI uses its own default model. Set exactModel to pin one."}),
        );
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
    if super::demo::is_simulated(&paths.home) {
        return Err(BusError::invalid(SIMULATED_NOTE));
    }
    fs::create_dir_all(&paths.dir)?;
    let presets_written = write_presets(paths)?;
    let members = plan(found);
    let mut wrote_crew = false;
    if force || !paths.crew().exists() {
        if members.is_empty() {
            return Err(BusError::invalid(
                "no crew-ready agent CLI found / install Claude Code, Codex CLI or Cursor CLI, then run aos setup",
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
/// Why nothing real starts on a sample bus.
pub const SIMULATED_NOTE: &str =
    "this is the simulated sample from aos demo, so no agent starts here / leave (q), cd to your project and run aos";

pub fn start(
    db_path: &Path,
    paths: &Paths,
    ids: &[String],
    workdir: &Path,
) -> Vec<(String, Result<i32>)> {
    if super::demo::is_simulated(&paths.home) {
        return ids
            .iter()
            .map(|id| (id.clone(), Err(BusError::invalid(SIMULATED_NOTE))))
            .collect();
    }
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
    /// The bus is a sample made by `aos demo`: nothing on it is real.
    pub simulated: bool,
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
        simulated: super::demo::is_simulated(&paths.home),
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
                        format!("{} installed at {} ({v})", h.id, Paths::show(&p)),
                    ));
                    if let Some(c) = known {
                        out.push(match sign_in_here(c.id) {
                            SignIn::Found(why) => {
                                check(Some(true), "", format!("signed in: {why}"))
                            }
                            SignIn::Missing => check(
                                Some(false),
                                "",
                                format!("not signed in, so its turns will fail / {}", c.sign_in),
                            ),
                            SignIn::Unknown(why) => check(
                                None,
                                "",
                                format!(
                                    "sign-in not checked: {why} / if turns fail: {}",
                                    c.sign_in
                                ),
                            ),
                        });
                    }
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
            format!("{} / can't join a crew yet", not_ready.join(", ")),
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
                sign_in: SignIn::Unknown("test".into()),
            })
            .collect()
    }

    #[test]
    fn one_family_means_the_operator_reviews() {
        let m = plan(&found(&["claude"]));
        assert_eq!(
            m.iter().map(|m| (m.id, m.cli.id)).collect::<Vec<_>>(),
            [("builder", "claude")]
        );
    }

    #[test]
    fn reviewer_comes_from_another_family() {
        let m = plan(&found(&["claude", "codex"]));
        let ids: Vec<_> = m.iter().map(|m| (m.id, m.cli.id)).collect();
        assert_eq!(ids, [("builder", "codex"), ("reviewer", "claude")]);
    }

    #[test]
    fn a_cli_known_to_be_signed_out_is_not_planned() {
        let mut f = found(&["claude", "codex"]);
        for x in f.iter_mut().filter(|x| x.cli.id == "codex") {
            x.sign_in = SignIn::Missing;
        }
        let m = plan(&f);
        assert_eq!(
            m.iter().map(|m| (m.id, m.cli.id)).collect::<Vec<_>>(),
            [("builder", "claude")]
        );
    }

    #[test]
    fn sign_in_is_judged_from_files_and_env_only() {
        let home = std::env::temp_dir().join(format!("aos-signin-{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        fs::create_dir_all(home.join(".claude")).unwrap();
        let none = |_: &str| None;
        assert_eq!(sign_in_for("claude", &none, &home, false), SignIn::Missing);
        assert!(matches!(
            sign_in_for("claude", &none, &home, true),
            SignIn::Unknown(_)
        ));
        fs::write(home.join(".claude/.credentials.json"), "{}").unwrap();
        assert!(matches!(
            sign_in_for("claude", &none, &home, false),
            SignIn::Found(_)
        ));
        assert_eq!(sign_in_for("codex", &none, &home, false), SignIn::Missing);
        let key = |n: &str| (n == "OPENAI_API_KEY").then(|| "sk-test".to_string());
        assert_eq!(
            sign_in_for("codex", &key, &home, false),
            SignIn::Found("OPENAI_API_KEY is set".into())
        );
        fs::create_dir_all(home.join(".codex")).unwrap();
        fs::write(
            home.join(".codex/config.toml"),
            "cli_auth_credentials_store = \"keyring\"\n",
        )
        .unwrap();
        assert!(matches!(
            sign_in_for("codex", &none, &home, false),
            SignIn::Unknown(_)
        ));
        assert!(matches!(
            sign_in_for("cursor", &none, &home, false),
            SignIn::Unknown(_)
        ));
        fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn goals_go_to_the_lead_when_there_is_one() {
        let dir = std::env::temp_dir().join(format!("aos-owner-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("crew.json");
        let f = found(&["claude", "codex"]);
        fs::write(&path, crew_json(&plan(&f), &f).to_string()).unwrap();
        let config = load_config(&path).unwrap();
        assert_eq!(goal_owner(&config).as_deref(), Some("builder"));
        assert_eq!(reviewer_for(&config, "builder"), "reviewer");
        let one = found(&["claude"]);
        fs::write(&path, crew_json(&plan(&one), &one).to_string()).unwrap();
        let config = load_config(&path).unwrap();
        assert_eq!(reviewer_for(&config, "builder"), OPERATOR_ID);
        fs::remove_dir_all(&dir).unwrap();
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
        assert_eq!(member_ids(&config), ["builder", "reviewer"]);
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
        let f = found(&["claude", "codex"]);
        fs::write(paths.crew(), crew_json(&plan(&f), &f).to_string()).unwrap();
        let text = role_prompt(&paths.crew(), "builder").unwrap();
        assert!(text.starts_with("You are the builder"), "{text}");
        assert!(text.contains("- reviewer (reviewer, claude)"), "{text}");
        assert!(!text.contains("{team}"));
        fs::remove_dir_all(&dir).unwrap();
    }
}
