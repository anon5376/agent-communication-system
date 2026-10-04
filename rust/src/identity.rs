//! Hashed agent and operator tokens — mirrors src/core/identity.ts.
//!
//! A process names itself (QAGENT_AGENT_ID, old name AGENT_ID, or --as <id>); the
//! library hashes <home>/tokens/<id>.token, or <home>/operator.token for the
//! operator, and accepts the identity only when that hash is stored for that id.

use crate::error::{BusError, Result};
use crate::types::OPERATOR_ID;
use rand::RngCore;
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Permissions {
    pub can_delegate: bool,
    pub can_review: bool,
    /// When set and non-empty, the only agents this one may assign work to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_child_agent_ids: Option<Vec<String>>,
    /// Deepest parent chain (ancestors of the new task) this agent may create work under.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_delegation_depth: Option<i64>,
    /// Most tasks this agent may hold claimed at once.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_concurrent_tasks: Option<i64>,
}

/// Limits applied on top of the authority's permissions, usually copied from the
/// project configuration by the supervisor. Stored as `policy` inside
/// identities.permissions_json, so both implementations read it without a schema
/// change (mirror: AgentPolicy in src/core/identity.ts). A policy only ever narrows:
/// the effective permission is the authority's AND the policy's.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgentPolicy {
    pub can_delegate: Option<bool>,
    pub allowed_child_agent_ids: Option<Vec<String>>,
    pub max_delegation_depth: Option<i64>,
    pub max_concurrent_tasks: Option<i64>,
}

impl AgentPolicy {
    pub fn is_empty(&self) -> bool {
        *self == AgentPolicy::default()
    }

    pub fn to_json(&self) -> serde_json::Value {
        let mut value = serde_json::Map::new();
        if let Some(v) = self.can_delegate {
            value.insert("canDelegate".into(), v.into());
        }
        if let Some(v) = &self.allowed_child_agent_ids {
            value.insert("allowedChildAgentIds".into(), v.clone().into());
        }
        if let Some(v) = self.max_delegation_depth {
            value.insert("maxDelegationDepth".into(), v.into());
        }
        if let Some(v) = self.max_concurrent_tasks {
            value.insert("maxConcurrentTasks".into(), v.into());
        }
        serde_json::Value::Object(value)
    }
}

fn whole_number(value: &serde_json::Value) -> Option<i64> {
    let n = value.as_f64()?;
    (n >= 0.0 && n.fract() == 0.0 && n <= i64::MAX as f64).then_some(n as i64)
}

/// The policy in a permissions_json `policy` value, with anything malformed dropped.
pub fn parse_policy(value: &serde_json::Value) -> AgentPolicy {
    let Some(raw) = value.as_object() else {
        return AgentPolicy::default();
    };
    AgentPolicy {
        can_delegate: raw.get("canDelegate").and_then(|v| v.as_bool()),
        allowed_child_agent_ids: raw.get("allowedChildAgentIds").and_then(|v| v.as_array()).map(|ids| {
            ids.iter().filter_map(|id| id.as_str().map(str::to_string)).collect()
        }),
        max_delegation_depth: raw.get("maxDelegationDepth").and_then(whole_number),
        max_concurrent_tasks: raw
            .get("maxConcurrentTasks")
            .and_then(whole_number)
            .filter(|n| *n >= 1),
    }
}

/// Whether `next` would allow anything `current` forbids.
pub fn policy_widens(current: &AgentPolicy, next: &AgentPolicy) -> bool {
    if current.can_delegate == Some(false) && next.can_delegate != Some(false) {
        return true;
    }
    let non_empty = |ids: &Option<Vec<String>>| ids.clone().filter(|ids| !ids.is_empty());
    if let Some(current_ids) = non_empty(&current.allowed_child_agent_ids) {
        match non_empty(&next.allowed_child_agent_ids) {
            None => return true,
            Some(next_ids) if next_ids.iter().any(|id| !current_ids.contains(id)) => return true,
            _ => {}
        }
    }
    for (was, now) in [
        (current.max_delegation_depth, next.max_delegation_depth),
        (current.max_concurrent_tasks, next.max_concurrent_tasks),
    ] {
        if let Some(was) = was {
            if now.map(|now| now > was).unwrap_or(true) {
                return true;
            }
        }
    }
    false
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Identity {
    pub agent_id: String,
    pub authority: String,
    pub permissions: Permissions,
}

pub(crate) fn is_safe_agent_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
}

pub fn assert_safe_agent_id(id: &str) -> Result<&str> {
    if !is_safe_agent_id(id) {
        return Err(BusError::invalid(format!(
            "unsafe agent id: {} (letters, digits, '.', '_' and '-' only)",
            serde_json::to_string(id).unwrap_or_else(|_| id.to_string())
        )));
    }
    Ok(id)
}

pub fn hash_token(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

const B64URL: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// base64url without padding, matching Node's Buffer.toString("base64url").
pub(crate) fn base64url(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 4 / 3 + 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64URL[(n >> 18) as usize & 63] as char);
        out.push(B64URL[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(B64URL[(n >> 6) as usize & 63] as char);
        }
        if chunk.len() > 2 {
            out.push(B64URL[n as usize & 63] as char);
        }
    }
    out
}

pub fn create_bearer_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    base64url(&bytes)
}

pub fn token_dir(home: &Path) -> PathBuf {
    home.join("tokens")
}

pub fn operator_token_path(home: &Path) -> PathBuf {
    home.join("operator.token")
}

pub fn agent_token_path(home: &Path, agent_id: &str) -> Result<PathBuf> {
    assert_safe_agent_id(agent_id)?;
    Ok(token_dir(home).join(format!("{agent_id}.token")))
}

pub fn token_path_for(home: &Path, agent_id: &str) -> Result<PathBuf> {
    if agent_id == OPERATOR_ID {
        Ok(operator_token_path(home))
    } else {
        agent_token_path(home, agent_id)
    }
}

pub fn ensure_private_directories(home: &Path) -> Result<()> {
    fs::create_dir_all(home)?;
    fs::create_dir_all(token_dir(home))?;
    let _ = fs::set_permissions(home, fs::Permissions::from_mode(0o700));
    let _ = fs::set_permissions(token_dir(home), fs::Permissions::from_mode(0o700));
    Ok(())
}

/// Write a token file with mode 0600, atomically (temp file then rename).
pub fn write_private_token(home: &Path, path: &Path, token: &str) -> Result<()> {
    ensure_private_directories(home)?;
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    fs::write(&temporary, format!("{token}\n"))?;
    let _ = fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600));
    fs::rename(&temporary, path)?;
    Ok(())
}

pub fn read_token_file(path: &Path) -> Option<String> {
    let token = fs::read_to_string(path).ok()?.trim().to_string();
    if token.is_empty() {
        None
    } else {
        Some(token)
    }
}

pub fn default_permissions(authority: &str) -> Permissions {
    let allowed = authority != "worker";
    Permissions {
        can_delegate: allowed,
        can_review: allowed,
        allowed_child_agent_ids: None,
        max_delegation_depth: None,
        max_concurrent_tasks: None,
    }
}

/// The agent id this process claims, from QAGENT_AGENT_ID or the older AGENT_ID.
pub fn agent_id_from_env() -> Option<String> {
    for name in ["QAGENT_AGENT_ID", "AGENT_ID"] {
        if let Ok(value) = std::env::var(name) {
            let trimmed = value.trim().to_string();
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }
    }
    None
}

struct IdentityRow {
    token_hash: String,
    authority: String,
    permissions_json: String,
    created_ms: i64,
}

fn row_for(conn: &Connection, agent_id: &str) -> Result<Option<IdentityRow>> {
    let mut stmt = conn.prepare_cached(
        "SELECT token_hash, authority, permissions_json, created_ms FROM identities WHERE agent_id = ?",
    )?;
    let row = stmt
        .query_row([agent_id], |row| {
            Ok(IdentityRow {
                token_hash: row.get(0)?,
                authority: row.get(1)?,
                permissions_json: row.get(2)?,
                created_ms: row.get(3)?,
            })
        })
        .ok();
    Ok(row)
}

fn json_object(text: &str) -> serde_json::Map<String, serde_json::Value> {
    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(serde_json::Value::Object(map)) => map,
        _ => serde_json::Map::new(),
    }
}

pub fn parse_permissions(json: &str, authority: &str) -> Permissions {
    let base = default_permissions(authority);
    let value = json_object(json);
    let policy = parse_policy(value.get("policy").unwrap_or(&serde_json::Value::Null));
    Permissions {
        can_delegate: value
            .get("canDelegate")
            .and_then(|v| v.as_bool())
            .unwrap_or(base.can_delegate)
            && policy.can_delegate != Some(false),
        can_review: value
            .get("canReview")
            .and_then(|v| v.as_bool())
            .unwrap_or(base.can_review),
        allowed_child_agent_ids: policy.allowed_child_agent_ids.filter(|ids| !ids.is_empty()),
        max_delegation_depth: policy.max_delegation_depth,
        max_concurrent_tasks: policy.max_concurrent_tasks,
    }
}

/// The permissions_json value stored for `agent_id`, parsed (empty when missing or malformed).
pub fn stored_permissions_json(
    conn: &Connection,
    agent_id: &str,
) -> Result<serde_json::Map<String, serde_json::Value>> {
    Ok(row_for(conn, agent_id)?
        .map(|row| json_object(&row.permissions_json))
        .unwrap_or_default())
}

/// The effective permissions stored for `agent_id` right now (read inside a write transaction).
pub fn current_permissions(conn: &Connection, agent_id: &str) -> Result<Option<Permissions>> {
    Ok(row_for(conn, agent_id)?.map(|row| parse_permissions(&row.permissions_json, &row.authority)))
}

/// Resolve the identity for `agent_id` from its token file. The token's hash must
/// be stored for exactly that id: a copy of agent A's token under B's name fails.
pub fn resolve_identity(conn: &Connection, home: &Path, agent_id: &str) -> Result<Identity> {
    if agent_id != OPERATOR_ID {
        assert_safe_agent_id(agent_id)?;
    }
    let path = token_path_for(home, agent_id)?;
    let token = read_token_file(&path).ok_or_else(|| {
        BusError::unauthorized(format!(
            "no token file for {agent_id} at {}",
            path.display()
        ))
    })?;
    identity_for_token(conn, agent_id, &token)
}

/// Resolve an identity from a token value (used when a caller holds the token in memory).
pub fn identity_for_token(conn: &Connection, agent_id: &str, token: &str) -> Result<Identity> {
    let token_hash = hash_token(token);
    let row: Option<(String, String, String)> = conn
        .prepare_cached(
            "SELECT agent_id, authority, permissions_json FROM identities WHERE token_hash = ?",
        )?
        .query_row([&token_hash], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .ok();
    let Some((stored_id, authority, permissions_json)) = row else {
        return Err(BusError::unauthorized(format!(
            "token for {agent_id} is not registered (rotate it with `qagent token rotate {agent_id}`)"
        )));
    };
    if stored_id != agent_id {
        return Err(BusError::unauthorized(format!(
            "token does not belong to {agent_id}"
        )));
    }
    if agent_id == OPERATOR_ID && authority != "operator" {
        return Err(BusError::unauthorized(
            "operator identity is not an operator",
        ));
    }
    if agent_id != OPERATOR_ID && authority == "operator" {
        return Err(BusError::unauthorized(format!(
            "{agent_id} cannot hold operator authority"
        )));
    }
    Ok(Identity {
        agent_id: stored_id,
        authority: authority.clone(),
        permissions: parse_permissions(&permissions_json, &authority),
    })
}

pub fn require_operator(identity: &Identity, action: &str) -> Result<()> {
    if identity.authority != "operator" {
        return Err(BusError::forbidden(format!(
            "only the operator may {action}"
        )));
    }
    Ok(())
}

/// Store a new token hash for `agent_id` and return the token. The caller writes
/// the token file after the transaction commits. Must run inside a transaction.
pub fn store_new_token(
    conn: &Connection,
    agent_id: &str,
    authority: &str,
    now_ms: i64,
    permissions: Option<&Permissions>,
) -> Result<String> {
    let token = create_bearer_token();
    let existing = row_for(conn, agent_id)?;
    conn.prepare_cached(
        "INSERT INTO identities(agent_id, token_hash, authority, permissions_json, created_ms, updated_ms)
         VALUES(?, ?, ?, ?, ?, ?)
         ON CONFLICT(agent_id) DO UPDATE SET token_hash = excluded.token_hash, authority = excluded.authority,
           permissions_json = excluded.permissions_json, updated_ms = excluded.updated_ms",
    )?
    .execute(rusqlite::params![
        agent_id,
        hash_token(&token),
        authority,
        // A rotation keeps the stored permissions and policy; only a new identity or a
        // new authority starts from the defaults.
        match (permissions, &existing) {
            (Some(permissions), _) => serde_json::to_string(permissions)?,
            (None, Some(row)) if row.authority == authority => row.permissions_json.clone(),
            (None, _) => serde_json::to_string(&default_permissions(authority))?,
        },
        existing.as_ref().map(|row| row.created_ms).unwrap_or(now_ms),
        now_ms
    ])?;
    Ok(token)
}

/// Register an existing token file's hash for `agent_id` (used by init to adopt operator.token).
pub fn adopt_token(
    conn: &Connection,
    agent_id: &str,
    authority: &str,
    token: &str,
    now_ms: i64,
) -> Result<()> {
    conn.prepare_cached(
        "INSERT INTO identities(agent_id, token_hash, authority, permissions_json, created_ms, updated_ms)
         VALUES(?, ?, ?, ?, ?, ?)
         ON CONFLICT(agent_id) DO UPDATE SET token_hash = excluded.token_hash, authority = excluded.authority, updated_ms = excluded.updated_ms",
    )?
    .execute(rusqlite::params![
        agent_id,
        hash_token(token),
        authority,
        serde_json::to_string(&default_permissions(authority))?,
        now_ms,
        now_ms
    ])?;
    Ok(())
}

pub fn stored_identity(conn: &Connection, agent_id: &str) -> Result<Option<(String, String)>> {
    let row = row_for(conn, agent_id)?;
    Ok(row.map(|row| (row.token_hash, row.authority)))
}
