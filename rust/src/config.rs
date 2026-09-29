//! Project configuration — the slice of src/config.ts that `qagent doctor`
//! needs: load a bus config JSON, resolve an agent to its harness definition,
//! and find the config file for a project directory.

use crate::error::{BusError, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
pub struct HarnessDef {
    pub id: String,
    #[serde(default)]
    pub adapter: String,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub providers: Vec<String>,
    #[serde(default)]
    pub enabled: bool,
}

#[derive(Debug, Deserialize)]
pub struct ModelDef {
    pub id: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub harness: String,
    #[serde(default)]
    pub enabled: bool,
}

#[derive(Debug, Deserialize)]
pub struct ProviderDef {
    pub id: String,
    #[serde(default)]
    pub enabled: bool,
}

#[derive(Debug, Deserialize)]
pub struct AgentDef {
    pub id: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub enabled: bool,
    /// Defaults to false: validateConfig treats a missing or non-true value as false.
    #[serde(default, rename = "autoStart")]
    pub auto_start: bool,
}

/// Only the fields doctor reads are strongly typed; routing/constraints/etc are
/// validated structurally (they must be objects) but not parsed field by field.
#[derive(Debug, Deserialize)]
pub struct BusConfig {
    pub version: i64,
    #[serde(default)]
    pub providers: HashMap<String, ProviderDef>,
    #[serde(default)]
    pub harnesses: HashMap<String, HarnessDef>,
    #[serde(default)]
    pub models: HashMap<String, ModelDef>,
    #[serde(default)]
    pub agents: HashMap<String, AgentDef>,
    #[serde(default)]
    pub roles: HashMap<String, serde_json::Value>,
    #[serde(default)]
    pub routing: serde_json::Value,
    #[serde(default)]
    pub constraints: serde_json::Value,
}

pub struct ResolvedAgent<'a> {
    pub agent: &'a AgentDef,
    pub model: &'a ModelDef,
    pub provider: &'a ProviderDef,
    pub harness: &'a HarnessDef,
}

fn validate(config: &BusConfig) -> Result<()> {
    if config.version != 1 {
        return Err(BusError::invalid("configuration.version must be 1"));
    }
    for (id, provider) in &config.providers {
        if provider.id != *id {
            return Err(BusError::invalid(format!(
                "provider key {id} must match provider.id"
            )));
        }
    }
    for (id, harness) in &config.harnesses {
        if harness.id != *id {
            return Err(BusError::invalid(format!(
                "harness key {id} must match harness.id"
            )));
        }
        if harness.command.is_empty() {
            return Err(BusError::invalid(format!("harness {id} has no command")));
        }
        for provider_id in &harness.providers {
            if !config.providers.contains_key(provider_id) {
                return Err(BusError::invalid(format!(
                    "harness {id} references unknown provider {provider_id}"
                )));
            }
        }
    }
    for (id, model) in &config.models {
        if model.id != *id {
            return Err(BusError::invalid(format!(
                "model key {id} must match model.id"
            )));
        }
        if !config.providers.contains_key(&model.provider) {
            return Err(BusError::invalid(format!(
                "model {id} references unknown provider {}",
                model.provider
            )));
        }
        if !config.harnesses.contains_key(&model.harness) {
            return Err(BusError::invalid(format!(
                "model {id} references unknown harness {}",
                model.harness
            )));
        }
    }
    for (id, agent) in &config.agents {
        if agent.id != *id {
            return Err(BusError::invalid(format!(
                "agent key {id} must match agent.id"
            )));
        }
        if !config.models.contains_key(&agent.model) {
            return Err(BusError::invalid(format!(
                "agent {id} references unknown model {}",
                agent.model
            )));
        }
        if !config.roles.contains_key(&agent.role) {
            return Err(BusError::invalid(format!(
                "agent {id} references unknown role {}",
                agent.role
            )));
        }
    }
    Ok(())
}

/// loadConfig(): JSON parse + the reference checks doctor's output relies on.
pub fn load_config(path: &Path) -> Result<BusConfig> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    if !absolute.exists() {
        return Err(BusError::invalid(format!(
            "Qagent configuration not found: {}",
            absolute.display()
        )));
    }
    let text = fs::read_to_string(&absolute)?;
    let config: BusConfig = serde_json::from_str(&text).map_err(|error| {
        BusError::invalid(format!("could not parse {}: {}", absolute.display(), error))
    })?;
    validate(&config)?;
    Ok(config)
}

/// resolveAgent(): agent -> model -> provider + harness.
pub fn resolve_agent<'a>(config: &'a BusConfig, id: &str) -> Result<ResolvedAgent<'a>> {
    let agent = config.agents.get(id).ok_or_else(|| {
        let mut ids: Vec<&str> = config.agents.keys().map(|s| s.as_str()).collect();
        ids.sort();
        BusError::invalid(format!(
            "unknown agent \"{id}\" — configured agents: {}",
            ids.join(", ")
        ))
    })?;
    let model = config.models.get(&agent.model).unwrap();
    let provider = config.providers.get(&model.provider).unwrap();
    let harness = config.harnesses.get(&model.harness).unwrap();
    Ok(ResolvedAgent {
        agent,
        model,
        provider,
        harness,
    })
}

/**
 * configPathFromProject(): explicit env config first, then the project-local
 * files, then the package default. The TS default resolves next to the npm
 * package; here it resolves next to the binary, then the cwd as a dev fallback.
 */
pub fn config_path_from_project(
    project_root: &Path,
    env: &dyn Fn(&str) -> Option<String>,
) -> PathBuf {
    if let Some(explicit) = env("QAGENT_CONFIG").or_else(|| env("AGENT_BUS_CONFIG")) {
        if !explicit.trim().is_empty() {
            return PathBuf::from(explicit);
        }
    }
    let next = project_root.join(".qagent/config.json");
    if next.exists() {
        return next;
    }
    let previous = project_root.join(".agent-bus/config.json");
    if previous.exists() {
        return previous;
    }
    if let Ok(exe) = std::env::current_exe() {
        let beside = exe
            .parent()
            .unwrap_or(Path::new("."))
            .join("agent-bus.config.json");
        if beside.exists() {
            return beside;
        }
    }
    PathBuf::from("agent-bus.config.json")
}
