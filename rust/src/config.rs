//! Project configuration — the slice of src/config.ts that `qagent doctor`
//! needs: load a bus config JSON, resolve an agent to its harness definition,
//! and find the config file for a project directory.

use crate::error::{BusError, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Deserialize)]
pub struct HarnessFeatures {
    #[serde(default)]
    pub headless: bool,
    #[serde(default)]
    pub resume: bool,
    #[serde(default)]
    pub mcp: bool,
    #[serde(default, rename = "structuredOutput")]
    pub structured_output: bool,
    #[serde(default)]
    pub streaming: bool,
    #[serde(default)]
    pub cancellation: bool,
    #[serde(default, rename = "modelSelection")]
    pub model_selection: bool,
    #[serde(default, rename = "reasoningControl")]
    pub reasoning_control: bool,
    #[serde(default, rename = "usageReporting")]
    pub usage_reporting: bool,
}

#[derive(Debug, Deserialize)]
pub struct ModelDiscovery {
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub format: String,
}

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
    pub features: HarnessFeatures,
    #[serde(default, rename = "probeArgs")]
    pub probe_args: Option<Vec<String>>,
    #[serde(default, rename = "modelDiscovery")]
    pub model_discovery: Option<ModelDiscovery>,
    #[serde(default)]
    pub enabled: bool,
    /// Defaults for every agent on this harness; an agent's own harnessOptions win.
    #[serde(default)]
    pub options: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub struct ModelDef {
    pub id: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub harness: String,
    #[serde(default)]
    pub family: String,
    #[serde(default, rename = "exactModel")]
    pub exact_model: Option<String>,
    #[serde(default)]
    pub capabilities: serde_json::Value,
    #[serde(default)]
    pub enabled: bool,
}

#[derive(Debug, Deserialize)]
pub struct ProviderDef {
    pub id: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default, rename = "subscriptionBacked")]
    pub subscription_backed: bool,
}

#[derive(Debug, Deserialize)]
pub struct AgentDef {
    pub id: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub authority: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub enabled: bool,
    /// Defaults to false: validateConfig treats a missing or non-true value as false.
    #[serde(default, rename = "autoStart")]
    pub auto_start: bool,
    #[serde(default)]
    pub permissions: serde_json::Value,
    #[serde(default, rename = "resumeSessionId")]
    pub resume_session_id: Option<String>,
    #[serde(default, rename = "harnessOptions")]
    pub harness_options: serde_json::Value,
}

/// Only the fields doctor reads are strongly typed; routing/constraints/etc are
/// validated structurally (they must be objects) but not parsed field by field.
#[derive(Debug, Deserialize)]
pub struct BusConfig {
    #[serde(default)]
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
        validate_capabilities(&model.capabilities, &format!("models.{id}.capabilities"))?;
    }
    for (id, role) in &config.roles {
        if role["id"].as_str() != Some(id.as_str()) {
            return Err(BusError::invalid(format!(
                "role key {id} must match role.id"
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
        if agent.permissions["maxDelegationDepth"]
            .as_f64()
            .map(|v| v < 0.0)
            .unwrap_or(false)
        {
            return Err(BusError::invalid(format!(
                "agent {id} maxDelegationDepth must be >= 0"
            )));
        }
        if let Some(session_id) = &agent.resume_session_id {
            if session_id.trim().is_empty() {
                return Err(BusError::invalid(format!(
                    "agent {id} resumeSessionId must be a non-empty string"
                )));
            }
            let harness = &config.harnesses[&config.models[&agent.model].harness];
            if !harness.features.resume {
                return Err(BusError::invalid(format!(
                    "agent {id} pins session {session_id}, but harness {} does not support resume",
                    harness.id
                )));
            }
            if harness.adapter == "command" {
                let options = &agent.harness_options;
                let raw_args = options["resumeArgs"]
                    .as_array()
                    .or_else(|| options["args"].as_array());
                let has_session = raw_args
                    .map(|args| {
                        args.iter().any(|item| {
                            item.as_str()
                                .map(|s| s.contains("{session}"))
                                .unwrap_or(false)
                        })
                    })
                    .unwrap_or(false);
                if !has_session {
                    return Err(BusError::invalid(format!(
                        "agent {id} uses the command adapter with resumeSessionId, but harnessOptions.resumeArgs/args has no {{session}} placeholder"
                    )));
                }
            }
        }
    }
    if config.constraints["maxDelegationDepth"]
        .as_f64()
        .map(|v| v < 0.0)
        .unwrap_or(false)
    {
        return Err(BusError::invalid("maxDelegationDepth must be >= 0"));
    }
    if config.constraints["maxConcurrentTasks"]
        .as_f64()
        .map(|v| v < 1.0)
        .unwrap_or(false)
    {
        return Err(BusError::invalid("maxConcurrentTasks must be >= 1"));
    }
    if config.constraints["maxRetries"]
        .as_f64()
        .map(|v| v < 0.0)
        .unwrap_or(false)
    {
        return Err(BusError::invalid("maxRetries must be >= 0"));
    }
    Ok(())
}

fn validate_capabilities(profile: &serde_json::Value, label: &str) -> Result<()> {
    const NAMES: &[&str] = &[
        "coding",
        "reasoning",
        "planning",
        "debugging",
        "research",
        "toolUse",
        "speed",
        "tokenEfficiency",
        "reliability",
        "autonomy",
    ];
    for name in NAMES {
        let value = &profile[*name];
        match value.as_f64() {
            Some(v) if (0.0..=1.0).contains(&v) => {}
            _ => {
                return Err(BusError::invalid(format!(
                    "{label}.{name} must be between 0 and 1"
                )))
            }
        }
    }
    match profile["contextTokens"].as_f64() {
        Some(v) if v >= 1.0 => {}
        _ => {
            return Err(BusError::invalid(format!(
                "{label}.contextTokens must be a positive number"
            )))
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
    let value: serde_json::Value = serde_json::from_str(&text).map_err(|error| {
        BusError::invalid(format!("could not parse {}: {}", absolute.display(), error))
    })?;
    if !value.is_object() {
        return Err(BusError::invalid("configuration must be an object"));
    }
    for key in [
        "providers",
        "harnesses",
        "models",
        "agents",
        "roles",
        "routing",
        "constraints",
    ] {
        if !value[key].is_object() {
            return Err(BusError::invalid(format!(
                "configuration.{key} must be an object"
            )));
        }
    }
    let config: BusConfig = serde_json::from_value(value).map_err(|error| {
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
