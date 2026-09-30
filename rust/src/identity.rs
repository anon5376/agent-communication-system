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
    if authority == "worker" {
        Permissions {
            can_delegate: false,
            can_review: false,
        }
    } else {
        Permissions {
            can_delegate: true,
            can_review: true,
        }
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
    created_ms: i64,
}

fn row_for(conn: &Connection, agent_id: &str) -> Result<Option<IdentityRow>> {
    let mut stmt = conn.prepare_cached(
        "SELECT token_hash, authority, created_ms FROM identities WHERE agent_id = ?",
    )?;
    let row = stmt
        .query_row([agent_id], |row| {
            Ok(IdentityRow {
                token_hash: row.get(0)?,
                authority: row.get(1)?,
                created_ms: row.get(2)?,
            })
        })
        .ok();
    Ok(row)
}

fn parse_permissions(json: &str, authority: &str) -> Permissions {
    let base = default_permissions(authority);
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
        return base;
    };
    Permissions {
        can_delegate: value
            .get("canDelegate")
            .and_then(|v| v.as_bool())
            .unwrap_or(base.can_delegate),
        can_review: value
            .get("canReview")
            .and_then(|v| v.as_bool())
            .unwrap_or(base.can_review),
    }
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
        serde_json::to_string(permissions.unwrap_or(&default_permissions(authority)))?,
        existing.map(|row| row.created_ms).unwrap_or(now_ms),
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
