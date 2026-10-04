//! The v2 coordination library: messages, cursors, tasks, reviews and leases over
//! one SQLite file. Every write is one BEGIN IMMEDIATE transaction that also
//! appends an events row. Semantics mirror src/core/bus.ts (efficiency-pass head):
//! priority-aware auto-claim, paginated candidates, batched dependency reads,
//! single-scan inbox, and the cancel notice reaching an expired claimer.

use crate::db::{append_event, home_for, latest_event_seq, open_database, resolve_db_path};
use crate::error::{BusError, Result};
use crate::identity::{self, Identity};
use crate::types::*;
use rand::RngCore;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::json;
use std::cell::{Cell, RefCell};

/// (id, state, assignee, project, path_scopes) row for the claim scan.
type ClaimCandidate = (i64, String, Option<String>, Option<String>, Vec<String>);
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn msg_id() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    // UUID v4 layout (version + variant bits), matching randomUUID() shape.
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "msg_{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
    )
}

fn json_parse<T: serde::de::DeserializeOwned>(value: Option<&str>, fallback: T) -> T {
    match value {
        Some(text) if !text.is_empty() => serde_json::from_str(text).unwrap_or(fallback),
        _ => fallback,
    }
}

fn placeholders(count: usize) -> String {
    vec!["?"; count].join(", ")
}

/// resolve(root, scope) then require the result inside root, mirroring
/// normalizeScope in bus.ts. Returns the forward-slash relative path.
pub fn normalize_scope(project_root: &str, scope: &str) -> Result<String> {
    let root = crate::db::absolutize(Path::new(project_root));
    let absolute = crate::db::absolutize(&root.join(scope));
    let rel = absolute
        .strip_prefix(&root)
        .map_err(|_| BusError::invalid(format!("path scope escapes project root: {scope}")))?;
    let rel = rel.to_string_lossy().replace('\\', "/");
    Ok(if rel.is_empty() { ".".to_string() } else { rel })
}

fn scopes_overlap(a: &str, b: &str) -> bool {
    a == "."
        || b == "."
        || a == b
        || a.starts_with(&format!("{b}/"))
        || b.starts_with(&format!("{a}/"))
}

pub struct SendInput {
    pub to: String,
    pub subject: Option<String>,
    pub body: String,
    pub msg_type: Option<String>,
    pub thread: Option<String>,
    pub task_id: Option<i64>,
    pub refs: Option<serde_json::Value>,
    pub requires_ack: bool,
}

#[derive(Default)]
pub struct CreateTaskInput {
    pub title: String,
    pub brief: Option<String>,
    pub acceptance: Option<String>,
    pub to: Option<String>,
    pub reviewer: Option<String>,
    pub role: Option<String>,
    pub priority: Option<String>,
    pub parent_id: Option<i64>,
    pub dependencies: Vec<i64>,
    pub path_scopes: Vec<String>,
    pub project: Option<String>,
    pub refs: Option<serde_json::Value>,
    pub max_retries: Option<i64>,
}

#[derive(Default)]
pub struct SubmitInput {
    pub summary: String,
    pub details: Option<String>,
    pub changed_files: Vec<String>,
    pub artifacts: Option<serde_json::Value>,
    pub validation: Option<serde_json::Value>,
}

#[derive(Default)]
pub struct ListTasksInput {
    /// Only tasks assigned to, created by, or to be reviewed by this agent.
    pub mine: Option<String>,
    pub states: Option<Vec<String>>,
    pub include_closed: bool,
    pub limit: Option<i64>,
}

pub struct InboxResult {
    pub messages: Vec<Message>,
    pub cursor: i64,
    pub remaining: i64,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InitResult {
    pub db_path: String,
    pub home: String,
    pub operator_token_path: String,
    pub operator: String,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WaitResult {
    pub status: String,
    pub messages: Vec<Message>,
    pub events: Vec<BusEvent>,
    pub seq: i64,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Whoami {
    pub agent: Option<Agent>,
    pub authority: String,
    pub unread: i64,
    pub cursor: i64,
    pub db_path: String,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusResult {
    pub db_path: String,
    pub seq: i64,
    pub agents: Vec<serde_json::Value>,
    pub counts: BTreeMap<String, i64>,
    pub open_tasks: Vec<Task>,
}

pub struct Bus {
    pub conn: Connection,
    pub db_path: PathBuf,
    pub home: PathBuf,
    clock: Box<dyn Fn() -> i64 + Send>,
    claim_ttl_ms: i64,
    tx_depth: Cell<u32>,
    pending_signals: RefCell<BTreeMap<String, i64>>,
}

impl Bus {
    pub fn open(db_path: Option<&Path>) -> Result<Bus> {
        let db_path = match db_path {
            Some(path) => crate::db::absolutize(path),
            None => resolve_db_path(None),
        };
        let home = home_for(&db_path);
        Ok(Bus {
            conn: open_database(&db_path)?,
            db_path,
            home,
            clock: Box::new(now_ms),
            claim_ttl_ms: CLAIM_TTL_MS,
            tx_depth: Cell::new(0),
            pending_signals: RefCell::new(BTreeMap::new()),
        })
    }

    /// Test clock override.
    pub fn with_clock(
        db_path: Option<&Path>,
        clock: impl Fn() -> i64 + Send + 'static,
    ) -> Result<Bus> {
        let mut bus = Self::open(db_path)?;
        bus.clock = Box::new(clock);
        Ok(bus)
    }

    /// Claim lifetime override (tests use a short TTL).
    pub fn set_claim_ttl_ms(&mut self, ms: i64) {
        self.claim_ttl_ms = ms;
    }

    pub fn now(&self) -> i64 {
        (self.clock)()
    }

    /// One BEGIN IMMEDIATE transaction. Nested calls join the outer transaction.
    /// After commit, inbox signal files for delivered messages are rewritten.
    pub(crate) fn write<T>(&self, f: impl FnOnce(&Self) -> Result<T>) -> Result<T> {
        self.pending_signals.borrow_mut().clear();
        let outermost = self.tx_depth.get() == 0;
        if outermost {
            self.conn.execute_batch("BEGIN IMMEDIATE")?;
        }
        self.tx_depth.set(self.tx_depth.get() + 1);
        let result = f(self);
        self.tx_depth.set(self.tx_depth.get() - 1);
        if !outermost {
            return result;
        }
        match result {
            Ok(value) => {
                self.conn.execute_batch("COMMIT")?;
                self.flush_signals();
                Ok(value)
            }
            Err(error) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                self.pending_signals.borrow_mut().clear();
                Err(error)
            }
        }
    }

    pub(crate) fn event(
        &self,
        actor: &str,
        kind: &str,
        entity: &str,
        entity_id: impl ToString,
        data: serde_json::Value,
    ) -> Result<i64> {
        append_event(
            &self.conn,
            self.now(),
            actor,
            kind,
            entity,
            &entity_id.to_string(),
            &data,
        )
    }

    // ------------------------------------------------------------- identity

    /// Resolve the calling identity from --as / QAGENT_AGENT_ID / AGENT_ID and its token file.
    pub fn identify(&self, agent_id: Option<&str>) -> Result<Identity> {
        let owned;
        let id = match agent_id {
            Some(id) => id,
            None => match identity::agent_id_from_env() {
                Some(id) => {
                    owned = id;
                    owned.as_str()
                }
                None => {
                    return Err(BusError::unauthorized(
                        "no agent identity: set QAGENT_AGENT_ID or pass --as <id>",
                    ))
                }
            },
        };
        identity::resolve_identity(&self.conn, &self.home, id)
    }

    /// Create the operator agent and token. Adopts an existing operator.token;
    /// rotates when file and hash disagree.
    pub fn init(&self) -> Result<InitResult> {
        identity::ensure_private_directories(&self.home)?;
        let token_path = identity::operator_token_path(&self.home);
        let operator = self.write(|bus| {
            let now = bus.now();
            bus.conn.prepare_cached(
                "INSERT INTO agents(id, role, model, harness, status, last_seen_ms, created_ms) VALUES(?, 'operator', 'human', 'cli', 'idle', ?, ?)
                 ON CONFLICT(id) DO NOTHING",
            )?.execute(params![OPERATOR_ID, now, now])?;
            let stored = identity::stored_identity(&bus.conn, OPERATOR_ID)?;
            let file_token = identity::read_token_file(&token_path);
            if let (Some((stored_hash, stored_authority)), Some(token)) = (&stored, &file_token) {
                if identity::hash_token(token) == *stored_hash && stored_authority == "operator" {
                    return Ok("unchanged".to_string());
                }
            }
            let owner: Option<String> = match &file_token {
                Some(token) => bus
                    .conn
                    .prepare_cached("SELECT agent_id FROM identities WHERE token_hash = ?")?
                    .query_row([identity::hash_token(token)], |row| row.get(0))
                    .ok(),
                None => None,
            };
            let outcome = if stored.is_none() && file_token.is_some() && owner.is_none() {
                identity::adopt_token(&bus.conn, OPERATOR_ID, "operator", file_token.as_deref().unwrap(), now)?;
                "adopted"
            } else {
                let token = identity::store_new_token(&bus.conn, OPERATOR_ID, "operator", now, None)?;
                // Written after commit below — but write needs the token now; the file
                // write is post-commit in the TS code too (token file write is outside
                // the transaction by design: it is idempotent on retry).
                identity::write_private_token(&bus.home, &token_path, &token)?;
                if stored.is_some() { "rotated" } else { "created" }
            };
            bus.event(OPERATOR_ID, "operator_token", "agent", OPERATOR_ID, json!({ "outcome": outcome }))?;
            Ok(outcome.to_string())
        })?;
        Ok(InitResult {
            db_path: self.db_path.display().to_string(),
            home: self.home.display().to_string(),
            operator_token_path: token_path.display().to_string(),
            operator,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn add_agent(
        &self,
        actor: &Identity,
        id: &str,
        role: Option<&str>,
        model: Option<&str>,
        harness: Option<&str>,
        parent: Option<&str>,
        authority: Option<&str>,
    ) -> Result<(Agent, PathBuf)> {
        identity::require_operator(actor, "add agents")?;
        let id = identity::assert_safe_agent_id(id)?;
        if id == OPERATOR_ID {
            return Err(BusError::invalid(
                "the operator agent is created by `qagent init`",
            ));
        }
        let authority = authority.unwrap_or("worker");
        if authority != "worker" && authority != "manager" {
            return Err(BusError::invalid(format!(
                "authority must be worker or manager, not {authority}"
            )));
        }
        let role = bounded_string(role, "role", limits::ROLE, false)?;
        let model = bounded_string(model, "model", limits::MODEL, false)?;
        let harness = bounded_string(harness, "harness", limits::MODEL, false)?;
        let parent = match parent {
            Some(parent) => Some(identity::assert_safe_agent_id(parent)?.to_string()),
            None => None,
        };
        let token_path = identity::token_path_for(&self.home, id)?;
        self.write(|bus| {
            let now = bus.now();
            if let Some(parent) = &parent {
                if !bus.agent_exists(parent)? {
                    return Err(BusError::not_found(format!("unknown parent agent: {parent}")));
                }
            }
            let existing = bus.agent_exists(id)?;
            if existing && identity::stored_identity(&bus.conn, id)?.is_some() {
                return Err(BusError::conflict(format!(
                    "agent {id} already exists; use `qagent token rotate {id}`"
                )));
            }
            if existing {
                bus.conn.prepare_cached(
                    "UPDATE agents SET role = COALESCE(NULLIF(?, ''), role), model = COALESCE(NULLIF(?, ''), model),
                       harness = COALESCE(NULLIF(?, ''), harness), parent_id = COALESCE(?, parent_id) WHERE id = ?",
                )?.execute(params![role, model, harness, parent, id])?;
            } else {
                bus.conn.prepare_cached(
                    "INSERT INTO agents(id, role, model, harness, parent_id, status, created_ms) VALUES(?, ?, ?, ?, ?, 'offline', ?)",
                )?.execute(params![id, role, model, harness, parent, now])?;
            }
            let token = identity::store_new_token(&bus.conn, id, authority, now, None)?;
            identity::write_private_token(&bus.home, &token_path, &token)?;
            bus.event(&actor.agent_id, "agent_added", "agent", id, json!({
                "role": role, "model": model, "harness": harness, "parent": parent, "authority": authority,
            }))?;
            Ok(())
        })?;
        Ok((self.get_agent(id)?.expect("agent just written"), token_path))
    }

    pub fn rotate_token(&self, actor: &Identity, agent_id: &str) -> Result<PathBuf> {
        identity::require_operator(actor, "rotate tokens")?;
        let id = if agent_id == OPERATOR_ID {
            OPERATOR_ID.to_string()
        } else {
            identity::assert_safe_agent_id(agent_id)?.to_string()
        };
        let token_path = identity::token_path_for(&self.home, &id)?;
        self.write(|bus| {
            if !bus.agent_exists(&id)? {
                return Err(BusError::not_found(format!("unknown agent: {id}")));
            }
            let authority = identity::stored_identity(&bus.conn, &id)?
                .map(|(_, authority)| authority)
                .unwrap_or_else(|| {
                    if id == OPERATOR_ID {
                        "operator"
                    } else {
                        "worker"
                    }
                    .to_string()
                });
            let token = identity::store_new_token(&bus.conn, &id, &authority, bus.now(), None)?;
            identity::write_private_token(&bus.home, &token_path, &token)?;
            bus.event(&actor.agent_id, "token_rotated", "agent", &id, json!({}))?;
            Ok(())
        })?;
        Ok(token_path)
    }

    /// Store the policy that narrows `agent_id`'s permissions (None clears it). The
    /// operator may set any policy; an agent may set its own only when the new one
    /// allows nothing the stored one forbids, so a supervisor can apply its project
    /// configuration without the operator token but can never widen what the operator
    /// or an earlier policy allowed. Mirror: setAgentPolicy in src/core/bus.ts.
    pub fn set_agent_policy(
        &self,
        actor: &Identity,
        agent_id: &str,
        policy: Option<&identity::AgentPolicy>,
    ) -> Result<identity::Permissions> {
        let id = identity::assert_safe_agent_id(agent_id)?.to_string();
        if actor.authority != "operator" && actor.agent_id != id {
            return Err(BusError::forbidden(format!(
                "only the operator or {id} itself may set {id}'s policy"
            )));
        }
        let next = policy.cloned().unwrap_or_default();
        // Normalise exactly as a reader would, so what is stored is what is enforced.
        let next = identity::parse_policy(&next.to_json());
        self.write(|bus| {
            if identity::stored_identity(&bus.conn, &id)?.is_none() {
                return Err(BusError::not_found(format!("unknown agent: {id}")));
            }
            let mut value = identity::stored_permissions_json(&bus.conn, &id)?;
            let current = identity::parse_policy(value.get("policy").unwrap_or(&serde_json::Value::Null));
            if actor.authority != "operator" && identity::policy_widens(&current, &next) {
                return Err(BusError::forbidden(format!(
                    "{id} may only narrow its own policy; ask the operator to widen it"
                )));
            }
            let now = bus.now();
            if next.is_empty() {
                value.remove("policy");
            } else {
                let mut stored = next.to_json();
                stored["updatedMs"] = json!(now);
                stored["by"] = json!(actor.agent_id);
                value.insert("policy".into(), stored);
            }
            bus.conn
                .prepare_cached("UPDATE identities SET permissions_json = ?, updated_ms = ? WHERE agent_id = ?")?
                .execute(params![serde_json::to_string(&value)?, now, id])?;
            bus.event(&actor.agent_id, "agent_policy", "agent", &id, json!({
                "policy": value.get("policy").cloned().unwrap_or(serde_json::Value::Null),
            }))?;
            Ok(identity::current_permissions(&bus.conn, &id)?.expect("identity checked above"))
        })
    }

    // ----------------------------------------------------- pause and budgets
    // Stored in agents.meta_json; see control.rs for the shape.

    fn update_agent_meta(
        &self,
        id: &str,
        f: impl FnOnce(&mut serde_json::Map<String, serde_json::Value>),
    ) -> Result<()> {
        let raw: Option<String> = self
            .conn
            .prepare_cached("SELECT meta_json FROM agents WHERE id = ?")?
            .query_row([id], |row| row.get(0))
            .optional()?;
        let Some(raw) = raw else {
            return Err(BusError::not_found(format!("unknown agent: {id}")));
        };
        let mut meta = serde_json::from_str::<serde_json::Value>(&raw)
            .ok()
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        f(&mut meta);
        self.conn
            .prepare_cached("UPDATE agents SET meta_json = ? WHERE id = ?")?
            .execute(params![serde_json::Value::Object(meta).to_string(), id])?;
        Ok(())
    }

    /// Pause an agent: its supervisor starts no new turn until it is resumed. The
    /// operator may pause anyone; an agent may pause itself (a supervisor does when
    /// a budget runs out). A turn already running finishes.
    pub fn pause_agent(&self, actor: &Identity, id: &str, reason: Option<&str>) -> Result<Agent> {
        if actor.authority != "operator" && actor.agent_id != id {
            return Err(BusError::forbidden(format!(
                "only the operator or {id} itself may pause {id}"
            )));
        }
        let reason = bounded_string(reason, "reason", limits::REASON, false)?;
        self.write(|bus| {
            let now = bus.now();
            bus.update_agent_meta(id, |meta| {
                meta.insert(
                    "paused".into(),
                    json!({"atMs": now, "by": actor.agent_id, "reason": reason}),
                );
            })?;
            bus.event(
                &actor.agent_id,
                "agent_paused",
                "agent",
                id,
                json!({"reason": reason}),
            )?;
            Ok(())
        })?;
        Ok(self.get_agent(id)?.expect("agent exists"))
    }

    /// Resume a paused agent. A budget, if set, starts a fresh allowance of the same size.
    pub fn resume_agent(&self, actor: &Identity, id: &str) -> Result<Agent> {
        identity::require_operator(actor, "resume agents")?;
        let usage = crate::control::session_usage(&self.home, id);
        self.write(|bus| {
            let now = bus.now();
            bus.update_agent_meta(id, |meta| {
                meta.remove("paused");
                if let Some(b) = meta.get_mut("budget").filter(|b| b.is_object()) {
                    b["base"] = usage.to_json();
                    b["setMs"] = json!(now);
                }
            })?;
            bus.event(&actor.agent_id, "agent_resumed", "agent", id, json!({}))?;
            Ok(())
        })?;
        Ok(self.get_agent(id)?.expect("agent exists"))
    }

    /// Set an agent's budget, counted from now, or clear it with None.
    pub fn set_budget(
        &self,
        actor: &Identity,
        id: &str,
        limits: Option<crate::control::Limits>,
    ) -> Result<Agent> {
        identity::require_operator(actor, "set budgets")?;
        if let Some(l) = &limits {
            if l.is_empty() {
                return Err(BusError::invalid(
                    "a budget needs --turns, --minutes or --usd (or --clear)",
                ));
            }
            for (v, name) in [(l.turns, "turns"), (l.minutes, "minutes"), (l.usd, "usd")] {
                if v.is_some_and(|v| !v.is_finite() || v <= 0.0) {
                    return Err(BusError::invalid(format!("--{name} must be above 0")));
                }
            }
        }
        let usage = crate::control::session_usage(&self.home, id);
        self.write(|bus| {
            let now = bus.now();
            bus.update_agent_meta(id, |meta| match limits {
                Some(l) => {
                    meta.insert("budget".into(), crate::control::budget_json(l, usage, now));
                }
                None => {
                    meta.remove("budget");
                }
            })?;
            let data = match limits {
                Some(l) => json!({"turns": l.turns, "minutes": l.minutes, "usd": l.usd}),
                None => json!({"cleared": true}),
            };
            bus.event(&actor.agent_id, "agent_budget", "agent", id, data)?;
            Ok(())
        })?;
        Ok(self.get_agent(id)?.expect("agent exists"))
    }

    /// The agent's budget with what it has used, if it has one.
    pub fn budget_of(&self, agent: &Agent) -> Option<crate::control::Budget> {
        crate::control::budget(
            &agent.meta,
            crate::control::session_usage(&self.home, &agent.id),
        )
    }

    /// Mark the caller as seen without changing its status or writing an event.
    pub fn heartbeat(&self, actor: &Identity) -> Result<()> {
        self.write(|bus| bus.touch(&actor.agent_id, None))
    }

    /// Keep a long turn's claims alive: push the expiry of every task the caller
    /// holds out by one claim TTL and mark it seen. Returns how many claims moved.
    pub fn renew_claims(&self, actor: &Identity) -> Result<usize> {
        self.write(|bus| {
            let now = bus.now();
            let renewed = bus
                .conn
                .prepare_cached(
                    "UPDATE tasks SET claim_expires_ms = ? WHERE state = 'claimed' AND assignee = ? AND claim_expires_ms IS NOT NULL AND claim_expires_ms >= ?",
                )?
                .execute(params![now + bus.claim_ttl_ms, actor.agent_id, now])?;
            bus.touch(&actor.agent_id, None)?;
            Ok(renewed)
        })
    }

    /// Mark the caller's inbox read through `seq` (never moves the cursor back).
    /// The supervisor reads with peek and calls this only once a turn has used the mail.
    pub fn mark_read_through(&self, actor: &Identity, seq: i64, count: usize) -> Result<()> {
        let me = &actor.agent_id;
        self.write(|bus| {
            if bus.cursor(me)? >= seq {
                return Ok(());
            }
            bus.conn.prepare_cached(
                "INSERT INTO cursors(agent_id, last_seq) VALUES(?, ?)
                 ON CONFLICT(agent_id) DO UPDATE SET last_seq = MAX(cursors.last_seq, excluded.last_seq)",
            )?.execute(params![me, seq])?;
            bus.event(me, "inbox_read", "agent", me, json!({ "cursor": seq, "count": count }))?;
            bus.touch(me, None)
        })
    }

    // --------------------------------------------------------------- agents

    fn agent_exists(&self, id: &str) -> Result<bool> {
        Ok(self
            .conn
            .prepare_cached("SELECT 1 FROM agents WHERE id = ?")?
            .exists([id])?)
    }

    fn agent_role(&self, id: &str) -> Result<String> {
        Ok(self
            .conn
            .prepare_cached("SELECT role FROM agents WHERE id = ?")?
            .query_row([id], |row| row.get::<_, String>(0))
            .unwrap_or_default())
    }

    fn to_agent(&self, row: &rusqlite::Row<'_>) -> rusqlite::Result<Agent> {
        let now = self.now();
        let stored: String = row
            .get::<_, Option<String>>("status")?
            .unwrap_or_else(|| "offline".into());
        let wait_until: Option<i64> = row.get("wait_until_ms")?;
        let last_seen: Option<i64> = row.get("last_seen_ms")?;
        let status = if stored == "waiting" {
            if wait_until.is_some_and(|until| until >= now) {
                "waiting"
            } else {
                "offline"
            }
        } else if stored == "offline" {
            "offline"
        } else if last_seen.is_some_and(|seen| now - seen <= STALE_AGENT_MS) {
            stored.as_str()
        } else {
            "offline"
        };
        let meta_json: Option<String> = row.get("meta_json")?;
        Ok(Agent {
            id: row.get("id")?,
            role: row.get::<_, Option<String>>("role")?.unwrap_or_default(),
            model: row.get::<_, Option<String>>("model")?.unwrap_or_default(),
            harness: row.get::<_, Option<String>>("harness")?.unwrap_or_default(),
            parent_id: row.get("parent_id")?,
            status: status.to_string(),
            stored_status: stored,
            wait_until_ms: wait_until,
            last_seen_ms: last_seen,
            created_ms: row.get("created_ms")?,
            authority: row.get("authority").ok().flatten(),
            meta: json_parse(meta_json.as_deref(), json!({})),
        })
    }

    /// Record that an agent acted. An agent that acts is not waiting, so a stored 'waiting'
    /// left behind by a killed waiter is cleared here instead of turning into 'offline'.
    fn touch(&self, agent_id: &str, status: Option<&str>) -> Result<()> {
        if let Some(status) = status {
            self.conn
                .prepare_cached("UPDATE agents SET last_seen_ms = ?, status = ?, wait_until_ms = NULL WHERE id = ?")?
                .execute(params![self.now(), status, agent_id])?;
        } else {
            self.conn
                .prepare_cached(
                    "UPDATE agents SET last_seen_ms = ?, status = CASE WHEN status IN ('offline', 'waiting') THEN 'idle' ELSE status END,
                       wait_until_ms = CASE WHEN status = 'waiting' THEN NULL ELSE wait_until_ms END
                     WHERE id = ?",
                )?
                .execute(params![self.now(), agent_id])?;
        }
        Ok(())
    }

    /// Set the caller's own status to working or idle.
    pub fn set_status(&self, actor: &Identity, status: &str) -> Result<Agent> {
        if status != "working" && status != "idle" {
            return Err(BusError::invalid(format!(
                "status must be working or idle, not {status}"
            )));
        }
        self.write(|bus| {
            if !bus.agent_exists(&actor.agent_id)? {
                return Err(BusError::not_found(format!(
                    "unknown agent: {}",
                    actor.agent_id
                )));
            }
            bus.touch(&actor.agent_id, Some(status))?;
            bus.event(
                &actor.agent_id,
                &format!("agent_{status}"),
                "agent",
                &actor.agent_id,
                json!({}),
            )?;
            Ok(())
        })?;
        Ok(self.get_agent(&actor.agent_id)?.expect("agent exists"))
    }

    pub fn get_agent(&self, id: &str) -> Result<Option<Agent>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT a.*, i.authority FROM agents a LEFT JOIN identities i ON i.agent_id = a.id WHERE a.id = ?",
        )?;
        let row = stmt.query_row([id], |row| self.to_agent(row)).optional()?;
        Ok(row)
    }

    /// Agents plus their unread counts, sorted by id.
    pub fn list_agents(&self) -> Result<Vec<(Agent, i64)>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT a.*, i.authority,
               (SELECT COUNT(*) FROM messages m
                  WHERE m.seq > COALESCE(c.last_seq, 0) AND (m.recipient = a.id OR (m.recipient IS NULL AND m.sender <> a.id))) AS unread
             FROM agents a LEFT JOIN identities i ON i.agent_id = a.id LEFT JOIN cursors c ON c.agent_id = a.id
             ORDER BY a.id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((self.to_agent(row)?, row.get::<_, i64>("unread")?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Stored status columns for the given agent ids in one query.
    pub fn agent_summaries(&self, ids: &[String]) -> Result<Vec<AgentSummary>> {
        let mut wanted: Vec<String> = ids.iter().filter(|id| !id.is_empty()).cloned().collect();
        wanted.sort();
        wanted.dedup();
        wanted.truncate(500);
        if wanted.is_empty() {
            return Ok(vec![]);
        }
        let sql = format!(
            "SELECT id, status, wait_until_ms, last_seen_ms FROM agents WHERE id IN ({}) ORDER BY id",
            placeholders(wanted.len())
        );
        let args: Vec<&dyn rusqlite::ToSql> =
            wanted.iter().map(|id| id as &dyn rusqlite::ToSql).collect();
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(args.as_slice(), |row| {
            Ok(AgentSummary {
                id: row.get(0)?,
                stored_status: row.get(1)?,
                wait_until_ms: row.get(2)?,
                last_seen_ms: row.get(3)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn whoami(&self, actor: &Identity) -> Result<Whoami> {
        Ok(Whoami {
            agent: self.get_agent(&actor.agent_id)?,
            authority: actor.authority.clone(),
            unread: self.unread_count(&actor.agent_id)?,
            cursor: self.cursor(&actor.agent_id)?,
            db_path: self.db_path.display().to_string(),
        })
    }

    // ------------------------------------------------------------- messages

    fn to_message(&self, row: &rusqlite::Row<'_>) -> rusqlite::Result<Message> {
        let refs_json: Option<String> = row.get("refs_json")?;
        Ok(Message {
            seq: row.get("seq")?,
            id: row.get("id")?,
            ts_ms: row.get("ts_ms")?,
            sender: row.get("sender")?,
            recipient: row.get("recipient")?,
            msg_type: row
                .get::<_, Option<String>>("type")?
                .unwrap_or_else(|| "info".into()),
            subject: row.get::<_, Option<String>>("subject")?.unwrap_or_default(),
            body: row.get::<_, Option<String>>("body")?.unwrap_or_default(),
            thread: row.get::<_, Option<String>>("thread")?.unwrap_or_default(),
            task_id: row.get("task_id")?,
            refs: json_parse(refs_json.as_deref(), vec![]),
            requires_ack: row.get::<_, i64>("requires_ack")? == 1,
            source: row
                .get::<_, Option<String>>("source")?
                .unwrap_or_else(|| "v2".into()),
        })
    }

    /// Insert one message inside the current transaction and queue the recipient's signal file.
    #[allow(clippy::too_many_arguments)]
    fn insert_message(
        &self,
        sender: &str,
        recipient: Option<&str>,
        msg_type: &str,
        subject: &str,
        body: &str,
        thread: &str,
        task_id: Option<i64>,
        refs: &[ContextReference],
        requires_ack: bool,
    ) -> Result<Message> {
        let id = msg_id();
        let ts = self.now();
        self.conn
            .prepare_cached(
                "INSERT INTO messages(id, ts_ms, sender, recipient, type, subject, body, thread, task_id, refs_json, requires_ack)
                 VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )?
            .execute(params![
                id, ts, sender, recipient, msg_type, subject, body, thread, task_id,
                serde_json::to_string(refs)?, requires_ack as i64
            ])?;
        let seq = self.conn.last_insert_rowid();
        self.event(sender, "message", "message", seq, json!({
            "recipient": recipient, "type": msg_type, "subject": subject.chars().take(200).collect::<String>(), "taskId": task_id,
        }))?;
        match recipient {
            Some(recipient) => {
                self.pending_signals
                    .borrow_mut()
                    .insert(recipient.to_string(), seq);
            }
            None => {
                let mut stmt = self
                    .conn
                    .prepare_cached("SELECT id FROM agents WHERE id <> ?")?;
                let ids = stmt.query_map([sender], |row| row.get::<_, String>(0))?;
                for id in ids {
                    self.pending_signals.borrow_mut().insert(id?, seq);
                }
            }
        }
        Ok(Message {
            seq,
            id,
            ts_ms: ts,
            sender: sender.to_string(),
            recipient: recipient.map(|s| s.to_string()),
            msg_type: msg_type.to_string(),
            subject: subject.to_string(),
            body: body.to_string(),
            thread: thread.to_string(),
            task_id,
            refs: refs.to_vec(),
            requires_ack,
            source: "v2".into(),
        })
    }

    /// Atomically rewrite inbox/<agent>.seq for harness hooks and shell loops that cannot open SQLite.
    fn flush_signals(&self) {
        let pending: Vec<(String, i64)> = self
            .pending_signals
            .borrow()
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        if pending.is_empty() {
            return;
        }
        let dir = self.home.join("inbox");
        let _ = (|| -> Result<()> {
            fs::create_dir_all(&dir)?;
            let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
            for (agent_id, seq) in &pending {
                let path = dir.join(format!("{agent_id}.seq"));
                let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
                fs::write(&temporary, format!("{seq}\n"))?;
                fs::rename(&temporary, &path)?;
            }
            Ok(())
        })();
        self.pending_signals.borrow_mut().clear();
    }

    pub fn send(&self, actor: &Identity, input: SendInput) -> Result<Vec<Message>> {
        let to = input.to.trim();
        if to.is_empty() {
            return Err(BusError::invalid("recipient is required (id, a,b or *)"));
        }
        let recipients: Vec<Option<String>> = if to == "*" {
            vec![None]
        } else {
            let mut seen = Vec::new();
            for part in to.split(',') {
                let part = part.trim();
                if !part.is_empty() && !seen.contains(&part.to_string()) {
                    seen.push(part.to_string());
                }
            }
            seen.into_iter().map(Some).collect()
        };
        if recipients.is_empty() {
            return Err(BusError::invalid("recipient is required (id, a,b or *)"));
        }
        if recipients.len() > 50 {
            return Err(BusError::invalid("at most 50 recipients"));
        }
        for recipient in recipients.iter().flatten() {
            identity::assert_safe_agent_id(recipient)?;
        }
        let msg_type = input.msg_type.unwrap_or_else(|| "info".into());
        if !MESSAGE_TYPES.contains(&msg_type.as_str()) {
            return Err(BusError::invalid(format!(
                "invalid message type: {msg_type}"
            )));
        }
        let subject = bounded_string(input.subject.as_deref(), "subject", limits::SUBJECT, false)?;
        let body = bounded_string(Some(&input.body), "body", limits::BODY, false)?;
        if subject.trim().is_empty() && body.trim().is_empty() {
            return Err(BusError::invalid("subject or body is required"));
        }
        let thread = bounded_string(input.thread.as_deref(), "thread", limits::THREAD, false)?;
        let refs = context_references(input.refs.as_ref())?;
        let task_id = input.task_id;
        self.write(|bus| {
            for recipient in recipients.iter().flatten() {
                if !bus.agent_exists(recipient)? {
                    return Err(BusError::not_found(format!(
                        "unknown recipient: {recipient}"
                    )));
                }
            }
            if let Some(task_id) = task_id {
                if !bus.task_exists(task_id)? {
                    return Err(BusError::not_found(format!("unknown task: {task_id}")));
                }
            }
            let thread = if !thread.is_empty() {
                thread.clone()
            } else if let Some(task_id) = task_id {
                format!("task-{task_id}")
            } else {
                String::new()
            };
            let mut sent = Vec::new();
            for recipient in &recipients {
                sent.push(bus.insert_message(
                    &actor.agent_id,
                    recipient.as_deref(),
                    &msg_type,
                    &subject,
                    &body,
                    &thread,
                    task_id,
                    &refs,
                    input.requires_ack,
                )?);
            }
            bus.touch(&actor.agent_id, None)?;
            Ok(sent)
        })
    }

    pub fn cursor(&self, agent_id: &str) -> Result<i64> {
        Ok(self
            .conn
            .prepare_cached("SELECT last_seq FROM cursors WHERE agent_id = ?")?
            .query_row([agent_id], |row| row.get::<_, i64>(0))
            .unwrap_or(0))
    }

    /// Unread rows plus the full unread count in one scan; `total` counts everything
    /// past the cursor, not just the fetched page.
    fn unread_rows(&self, agent_id: &str, cursor: i64, limit: i64) -> Result<(Vec<Message>, i64)> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT *, (SELECT COUNT(*) FROM messages WHERE seq > ? AND (recipient = ? OR (recipient IS NULL AND sender <> ?))) AS total
             FROM messages
             WHERE seq > ? AND (recipient = ? OR (recipient IS NULL AND sender <> ?))
             ORDER BY seq LIMIT ?",
        )?;
        let rows = stmt.query_map(
            params![cursor, agent_id, agent_id, cursor, agent_id, agent_id, limit],
            |row| Ok((self.to_message(row)?, row.get::<_, i64>("total")?)),
        )?;
        let mut messages = Vec::new();
        let mut total = 0;
        for row in rows {
            let (message, row_total) = row?;
            total = row_total;
            messages.push(message);
        }
        Ok((messages, total))
    }

    pub fn unread_count(&self, agent_id: &str) -> Result<i64> {
        let cursor = self.cursor(agent_id)?;
        Ok(self
            .conn
            .prepare_cached(
                "SELECT COUNT(*) AS n FROM messages WHERE seq > ? AND (recipient = ? OR (recipient IS NULL AND sender <> ?))",
            )?
            .query_row(params![cursor, agent_id, agent_id], |row| row.get(0))?)
    }

    /// New mail since the cursor. Advances the cursor unless peek is set.
    pub fn inbox(&self, actor: &Identity, peek: bool, limit: Option<i64>) -> Result<InboxResult> {
        let limit = limit.unwrap_or(50).clamp(1, limits::INBOX_LIMIT);
        let me = &actor.agent_id;
        if peek {
            let cursor = self.cursor(me)?;
            let (messages, total) = self.unread_rows(me, cursor, limit)?;
            return Ok(InboxResult {
                cursor,
                remaining: total - messages.len() as i64,
                messages,
            });
        }
        self.write(|bus| {
            let read_at = bus.cursor(me)?;
            let (messages, total) = bus.unread_rows(me, read_at, limit)?;
            if let Some(last) = messages.last() {
                bus.conn.prepare_cached(
                    "INSERT INTO cursors(agent_id, last_seq) VALUES(?, ?)
                     ON CONFLICT(agent_id) DO UPDATE SET last_seq = MAX(cursors.last_seq, excluded.last_seq)",
                )?.execute(params![me, last.seq])?;
                bus.event(me, "inbox_read", "agent", me, json!({ "cursor": last.seq, "count": messages.len() }))?;
                bus.touch(me, None)?;
                return Ok(InboxResult { cursor: last.seq, remaining: total - messages.len() as i64, messages });
            }
            Ok(InboxResult { cursor: read_at, remaining: 0, messages })
        })
    }

    pub fn ack(&self, actor: &Identity, seq: i64) -> Result<(i64, i64)> {
        self.write(|bus| {
            let found: Option<Option<String>> = bus
                .conn
                .prepare_cached("SELECT recipient FROM messages WHERE seq = ?")?
                .query_row([seq], |row| row.get(0))
                .optional()?;
            // A row exists and is addressed to this agent or broadcast to everyone.
            let ok = match &found {
                Some(None) => true,
                Some(Some(recipient)) => recipient == &actor.agent_id,
                None => false,
            };
            if !ok {
                return Err(BusError::not_found(format!(
                    "no message {seq} for {}",
                    actor.agent_id
                )));
            }
            let ack_ms = bus.now();
            bus.conn
                .prepare_cached(
                    "INSERT OR IGNORE INTO acks(seq, agent_id, ack_ms) VALUES(?, ?, ?)",
                )?
                .execute(params![seq, actor.agent_id, ack_ms])?;
            bus.event(&actor.agent_id, "ack", "message", seq, json!({}))?;
            bus.touch(&actor.agent_id, None)?;
            Ok((seq, ack_ms))
        })
    }

    pub fn get_messages(
        &self,
        since_seq: Option<i64>,
        limit: Option<i64>,
        thread: Option<&str>,
        task_id: Option<i64>,
    ) -> Result<Vec<Message>> {
        let limit = limit.unwrap_or(100).clamp(1, 1000);
        let mut messages = Vec::new();
        if let Some(task_id) = task_id {
            let mut stmt = self.conn.prepare_cached(
                "SELECT * FROM messages WHERE task_id = ? AND seq > ? ORDER BY seq LIMIT ?",
            )?;
            let rows = stmt.query_map(params![task_id, since_seq.unwrap_or(0), limit], |row| {
                self.to_message(row)
            })?;
            for row in rows {
                messages.push(row?);
            }
        } else if let Some(thread) = thread {
            let mut stmt = self.conn.prepare_cached(
                "SELECT * FROM messages WHERE thread = ? AND seq > ? ORDER BY seq LIMIT ?",
            )?;
            let rows = stmt.query_map(params![thread, since_seq.unwrap_or(0), limit], |row| {
                self.to_message(row)
            })?;
            for row in rows {
                messages.push(row?);
            }
        } else if let Some(since) = since_seq {
            let mut stmt = self
                .conn
                .prepare_cached("SELECT * FROM messages WHERE seq > ? ORDER BY seq LIMIT ?")?;
            let rows = stmt.query_map(params![since, limit], |row| self.to_message(row))?;
            for row in rows {
                messages.push(row?);
            }
        } else {
            let mut stmt = self
                .conn
                .prepare_cached("SELECT * FROM messages ORDER BY seq DESC LIMIT ?")?;
            let rows = stmt.query_map([limit], |row| self.to_message(row))?;
            for row in rows {
                messages.push(row?);
            }
            messages.reverse();
        }
        Ok(messages)
    }

    /// Subject/body columns for the given message seqs in one query.
    pub fn message_summaries(&self, seqs: &[i64]) -> Result<Vec<MessageSummary>> {
        let mut wanted: Vec<i64> = seqs.iter().copied().filter(|seq| *seq > 0).collect();
        wanted.sort();
        wanted.dedup();
        wanted.truncate(500);
        if wanted.is_empty() {
            return Ok(vec![]);
        }
        let sql = format!(
            "SELECT seq, ts_ms, sender, recipient, subject, body FROM messages WHERE seq IN ({}) ORDER BY seq",
            placeholders(wanted.len())
        );
        let args: Vec<&dyn rusqlite::ToSql> = wanted
            .iter()
            .map(|seq| seq as &dyn rusqlite::ToSql)
            .collect();
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(args.as_slice(), |row| {
            Ok(MessageSummary {
                seq: row.get(0)?,
                ts_ms: row.get(1)?,
                sender: row.get(2)?,
                recipient: row.get(3)?,
                subject: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
                body: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    // --------------------------------------------------------------- events

    fn to_event(&self, row: &rusqlite::Row<'_>) -> rusqlite::Result<BusEvent> {
        let data_json: Option<String> = row.get("data_json")?;
        Ok(BusEvent {
            seq: row.get("seq")?,
            ts_ms: row.get("ts_ms")?,
            actor: row.get("actor")?,
            kind: row.get("kind")?,
            entity: row.get("entity")?,
            entity_id: row.get("entity_id")?,
            data: json_parse(data_json.as_deref(), json!({})),
            source: row
                .get::<_, Option<String>>("source")?
                .unwrap_or_else(|| "v2".into()),
        })
    }

    pub fn latest_seq(&self) -> Result<i64> {
        latest_event_seq(&self.conn)
    }

    pub fn events(&self, since_seq: i64, limit: i64) -> Result<Vec<BusEvent>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT * FROM events WHERE seq > ? ORDER BY seq LIMIT ?")?;
        let rows = stmt.query_map(params![since_seq, limit.clamp(1, 5000)], |row| {
            self.to_event(row)
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Task events after `after_seq` by someone else on a task this agent owns,
    /// reviews, or could claim.
    pub fn task_events_for(
        &self,
        agent_id: &str,
        after_seq: i64,
        upto_seq: i64,
    ) -> Result<Vec<BusEvent>> {
        let role = self.agent_role(agent_id)?;
        let mut stmt = self.conn.prepare_cached(
            "SELECT e.* FROM events e JOIN tasks t ON e.entity = 'task' AND t.id = CAST(e.entity_id AS INTEGER)
             WHERE e.seq > ? AND e.seq <= ? AND e.actor <> ? AND e.source = 'v2'
               AND (t.assignee = ? OR t.reviewer = ? OR (t.reviewer IS NULL AND t.creator = ?)
                    OR (t.assignee IS NULL AND t.state = 'open' AND (t.role = '' OR t.role = ?)))
             ORDER BY e.seq",
        )?;
        let rows = stmt.query_map(
            params![after_seq, upto_seq, agent_id, agent_id, agent_id, agent_id, role],
            |row| self.to_event(row),
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    // ---------------------------------------------------------------- tasks

    fn task_exists(&self, id: i64) -> Result<bool> {
        Ok(self
            .conn
            .prepare_cached("SELECT 1 FROM tasks WHERE id = ?")?
            .exists([id])?)
    }

    fn task_row(&self, id: i64) -> Result<Option<Task>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT * FROM tasks WHERE id = ?")?;
        let task = stmt
            .query_row([id], |row| self.to_task(row, None))
            .optional()?;
        Ok(task)
    }

    fn task_deps(&self, id: i64) -> Result<Vec<i64>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT depends_on FROM task_deps WHERE task_id = ? ORDER BY depends_on",
        )?;
        let rows = stmt.query_map([id], |row| row.get::<_, i64>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// All dependencies for `ids` in one query, keyed by task id.
    fn dependencies_for(&self, ids: &[i64]) -> Result<HashMap<i64, Vec<i64>>> {
        let mut map: HashMap<i64, Vec<i64>> = HashMap::new();
        if ids.is_empty() {
            return Ok(map);
        }
        let sql = format!(
            "SELECT task_id, depends_on FROM task_deps WHERE task_id IN ({}) ORDER BY depends_on",
            placeholders(ids.len())
        );
        let args: Vec<&dyn rusqlite::ToSql> =
            ids.iter().map(|id| id as &dyn rusqlite::ToSql).collect();
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(args.as_slice(), |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (task_id, depends_on) = row?;
            map.entry(task_id).or_default().push(depends_on);
        }
        Ok(map)
    }

    fn to_task(
        &self,
        row: &rusqlite::Row<'_>,
        dependencies: Option<Vec<i64>>,
    ) -> rusqlite::Result<Task> {
        let id: i64 = row.get("id")?;
        let dependencies = match dependencies {
            Some(deps) => deps,
            None => self
                .task_deps(id)
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
        };
        Ok(Task {
            id,
            legacy_id: row.get("legacy_id")?,
            project: row.get("project")?,
            parent_id: row.get("parent_id")?,
            title: row.get("title")?,
            brief: row.get::<_, Option<String>>("brief")?.unwrap_or_default(),
            acceptance: row
                .get::<_, Option<String>>("acceptance")?
                .unwrap_or_default(),
            role: row.get::<_, Option<String>>("role")?.unwrap_or_default(),
            priority: row
                .get::<_, Option<String>>("priority")?
                .unwrap_or_else(|| "normal".into()),
            state: row.get("state")?,
            creator: row.get("creator")?,
            assignee: row.get("assignee")?,
            reviewer: row.get("reviewer")?,
            path_scopes: json_parse(
                row.get::<_, Option<String>>("path_scopes_json")?.as_deref(),
                vec![],
            ),
            refs: json_parse(
                row.get::<_, Option<String>>("refs_json")?.as_deref(),
                vec![],
            ),
            result: json_parse(
                row.get::<_, Option<String>>("result_json")?.as_deref(),
                None,
            ),
            review: json_parse(
                row.get::<_, Option<String>>("review_json")?.as_deref(),
                None,
            ),
            round: row.get("round")?,
            attempts: row.get("attempts")?,
            max_retries: row.get("max_retries")?,
            claim_expires_ms: row.get("claim_expires_ms")?,
            created_ms: row.get("created_ms")?,
            updated_ms: row.get("updated_ms")?,
            dependencies,
        })
    }

    /// Rows -> tasks with one shared dependency query instead of one per task.
    fn to_tasks(&self, sql: &str, args: &[&dyn rusqlite::ToSql]) -> Result<Vec<Task>> {
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(args, |row| self.to_task(row, Some(vec![])))?;
        let mut tasks = Vec::new();
        for row in rows {
            tasks.push(row?);
        }
        drop(stmt);
        let dependencies =
            self.dependencies_for(&tasks.iter().map(|task| task.id).collect::<Vec<_>>())?;
        for task in &mut tasks {
            task.dependencies = dependencies.get(&task.id).cloned().unwrap_or_default();
        }
        Ok(tasks)
    }

    fn require_task(&self, id: i64) -> Result<Task> {
        if id <= 0 {
            return Err(BusError::invalid(format!("invalid task id: {id}")));
        }
        match self.task_row(id)? {
            Some(task) => Ok(task),
            None => Err(BusError::not_found(format!("unknown task: {id}"))),
        }
    }

    pub fn get_task(&self, id: i64) -> Result<TaskDetail> {
        let task = self.require_task(id)?;
        let notes = {
            let mut stmt = self
                .conn
                .prepare_cached("SELECT * FROM task_notes WHERE task_id = ? ORDER BY id")?;
            let rows = stmt.query_map([id], |row| {
                Ok(TaskNote {
                    id: row.get("id")?,
                    task_id: row.get("task_id")?,
                    author: row.get("author")?,
                    ts_ms: row.get("ts_ms")?,
                    body: row.get("body")?,
                })
            })?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            out
        };
        let dependents = {
            let mut stmt = self.conn.prepare_cached(
                "SELECT task_id FROM task_deps WHERE depends_on = ? ORDER BY task_id",
            )?;
            let rows = stmt.query_map([id], |row| row.get::<_, i64>(0))?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            out
        };
        let leases = {
            let mut stmt = self
                .conn
                .prepare_cached("SELECT path FROM leases WHERE task_id = ? ORDER BY path")?;
            let rows = stmt.query_map([id], |row| row.get::<_, String>(0))?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            out
        };
        Ok(TaskDetail {
            task,
            notes,
            dependents,
            messages: self.get_messages(None, Some(1000), None, Some(id))?,
            leases,
        })
    }

    /// Summary columns for the given task ids, without deps, notes or messages.
    pub fn task_summaries(&self, ids: &[i64]) -> Result<Vec<TaskSummary>> {
        let mut wanted: Vec<i64> = ids.iter().copied().filter(|id| *id > 0).collect();
        wanted.sort();
        wanted.dedup();
        wanted.truncate(500);
        if wanted.is_empty() {
            return Ok(vec![]);
        }
        let sql = format!(
            "SELECT id, title, assignee, state, created_ms, updated_ms FROM tasks WHERE id IN ({}) ORDER BY id",
            placeholders(wanted.len())
        );
        let args: Vec<&dyn rusqlite::ToSql> =
            wanted.iter().map(|id| id as &dyn rusqlite::ToSql).collect();
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(args.as_slice(), |row| {
            Ok(TaskSummary {
                id: row.get(0)?,
                title: row.get(1)?,
                assignee: row.get(2)?,
                state: row.get(3)?,
                created_ms: row.get(4)?,
                updated_ms: row.get(5)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn list_tasks(&self, input: ListTasksInput) -> Result<Vec<Task>> {
        let mut where_parts: Vec<String> = Vec::new();
        let mut args: Vec<rusqlite::types::Value> = Vec::new();
        if let Some(states) = &input.states {
            if !states.is_empty() {
                for state in states {
                    if !TASK_STATES.contains(&state.as_str()) {
                        return Err(BusError::invalid(format!("invalid task state: {state}")));
                    }
                }
                where_parts.push(format!("state IN ({})", placeholders(states.len())));
                for state in states {
                    args.push(state.clone().into());
                }
            } else if !input.include_closed {
                where_parts.push(format!(
                    "state NOT IN ({})",
                    placeholders(CLOSED_STATES.len())
                ));
                for state in CLOSED_STATES {
                    args.push((*state).to_string().into());
                }
            }
        } else if !input.include_closed {
            where_parts.push(format!(
                "state NOT IN ({})",
                placeholders(CLOSED_STATES.len())
            ));
            for state in CLOSED_STATES {
                args.push((*state).to_string().into());
            }
        }
        if let Some(mine) = &input.mine {
            where_parts.push("(assignee = ? OR creator = ? OR reviewer = ?)".into());
            args.push(mine.clone().into());
            args.push(mine.clone().into());
            args.push(mine.clone().into());
        }
        let limit = input.limit.unwrap_or(200).clamp(1, 1000);
        let sql = format!(
            "SELECT * FROM tasks {} ORDER BY id LIMIT ?",
            if where_parts.is_empty() {
                String::new()
            } else {
                format!("WHERE {}", where_parts.join(" AND "))
            }
        );
        args.push(limit.into());
        let refs: Vec<&dyn rusqlite::ToSql> =
            args.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
        self.to_tasks(&sql, &refs)
    }

    /// Claimed tasks with no claim/note activity for `stall_ms` — a probably-dead claim.
    /// `updated_ms` moves on claim and on every note, so it is the last-activity clock.
    /// Whether open work is waiting that `agent_id` may claim: assigned to it, or
    /// unassigned for its role (or for no role).
    /// Nothing is claimable once the agent holds its claim limit.
    pub fn has_claimable(&self, agent_id: &str, role: &str) -> Result<bool> {
        let limit = identity::current_permissions(&self.conn, agent_id)?
            .and_then(|p| p.max_concurrent_tasks);
        if let Some(limit) = limit {
            if self.claimed_count(agent_id)? >= limit {
                return Ok(false);
            }
        }
        Ok(self
            .conn
            .prepare_cached(
                "SELECT 1 FROM tasks WHERE state IN ('open', 'changes_requested')
                   AND (assignee = ? OR (assignee IS NULL AND (role = '' OR role = ?))) LIMIT 1",
            )?
            .query_row(params![agent_id, role], |_| Ok(()))
            .optional()?
            .is_some())
    }

    pub fn stalled_tasks(&self, stall_ms: i64) -> Result<Vec<Task>> {
        let cutoff = self.now() - stall_ms.max(0);
        self.to_tasks(
            "SELECT * FROM tasks WHERE state = 'claimed' AND updated_ms < ? ORDER BY id",
            &[&cutoff as &dyn rusqlite::ToSql],
        )
    }

    /// Claims the bus can treat as dead: the lease expired, or the task has been idle
    /// for stall_ms AND the assignee has not touched the bus in that same window. An
    /// active worker keeps refreshing last_seen_ms, so a live claim survives both tests.
    pub fn dead_claims(&self, stall_ms: i64) -> Result<Vec<Task>> {
        let now = self.now();
        let cutoff = now - stall_ms.max(0);
        self.to_tasks(
            "SELECT t.* FROM tasks t LEFT JOIN agents a ON a.id = t.assignee
             WHERE t.state = 'claimed' AND (
               (t.claim_expires_ms IS NOT NULL AND t.claim_expires_ms < ?)
               OR (t.updated_ms < ? AND (a.last_seen_ms IS NULL OR a.last_seen_ms < ?))
             ) ORDER BY t.id",
            &[&now as &dyn rusqlite::ToSql, &cutoff, &cutoff],
        )
    }

    /// The task's causal chain: its events, its notes, and the mail the bus sent about it,
    /// merged into one chronological timeline — the bus is the trace.
    pub fn trace_task(&self, id: i64) -> Result<TaskTrace> {
        let task = self.get_task(id)?;
        // Imported tasks' events are keyed by legacy_id, not the new numeric id.
        let ids = match &task.task.legacy_id {
            Some(legacy) => vec![id.to_string(), legacy.clone()],
            None => vec![id.to_string()],
        };
        let events = {
            let mut stmt = self.conn.prepare_cached(
                "SELECT * FROM events WHERE entity = 'task' AND entity_id IN (SELECT value FROM json_each(?)) ORDER BY seq",
            )?;
            let rows = stmt
                .query_map([serde_json::to_string(&ids).unwrap_or_default()], |row| {
                    self.to_event(row)
                })?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            out
        };
        let mut timeline: Vec<TraceItem> = Vec::new();
        for event in events {
            timeline.push(TraceItem {
                seq: event.seq,
                ts_ms: event.ts_ms,
                kind: event.kind.clone(),
                actor: event.actor,
                summary: event.kind.replace('_', " "),
                body: None,
                to: None,
                data: Some(event.data),
            });
        }
        for note in &task.notes {
            timeline.push(TraceItem {
                seq: note.id,
                ts_ms: note.ts_ms,
                kind: "note".into(),
                actor: note.author.clone(),
                summary: note
                    .body
                    .lines()
                    .next()
                    .unwrap_or("")
                    .chars()
                    .take(200)
                    .collect(),
                body: Some(note.body.clone()),
                to: None,
                data: None,
            });
        }
        // Uncapped: get_task() limits messages to 1000, a trace wants the whole chain.
        let mail = {
            let mut stmt = self
                .conn
                .prepare_cached("SELECT * FROM messages WHERE task_id = ? ORDER BY seq")?;
            let rows = stmt.query_map([id], |row| self.to_message(row))?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            out
        };
        for message in &mail {
            timeline.push(TraceItem {
                seq: message.seq,
                ts_ms: message.ts_ms,
                kind: "mail".into(),
                actor: message.sender.clone(),
                summary: message.subject.clone(),
                body: Some(message.body.clone()),
                to: Some(message.recipient.clone()),
                data: None,
            });
        }
        timeline.sort_by(|a, b| a.ts_ms.cmp(&b.ts_ms).then(a.seq.cmp(&b.seq)));
        Ok(TaskTrace {
            dependencies: task.task.dependencies.clone(),
            dependents: task.dependents.clone(),
            task,
            timeline,
        })
    }

    /// Reopen claims past their expiry. There is no sweeper process; every task write calls this first.
    fn reopen_expired_claims(&self) -> Result<()> {
        let now = self.now();
        let expired: Vec<(i64, Option<String>)> = {
            let mut stmt = self.conn.prepare_cached(
                "SELECT id, assignee FROM tasks WHERE state = 'claimed' AND claim_expires_ms IS NOT NULL AND claim_expires_ms < ?",
            )?;
            let rows = stmt.query_map([now], |row| Ok((row.get(0)?, row.get(1)?)))?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            out
        };
        for (id, assignee) in expired {
            let created: Option<String> = self
                .conn
                .prepare_cached(
                    "SELECT data_json FROM events WHERE entity = 'task' AND entity_id = ? AND kind = 'task_created' ORDER BY seq LIMIT 1",
                )?
                .query_row([id.to_string()], |row| row.get(0))
                .ok();
            let preassigned: Option<String> = created
                .as_deref()
                .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
                .and_then(|data| {
                    data.get("assignee")
                        .and_then(|v| v.as_str().map(|s| s.to_string()))
                });
            self.conn
                .prepare_cached("UPDATE tasks SET state = 'open', assignee = ?, claim_expires_ms = NULL, updated_ms = ? WHERE id = ?")?
                .execute(params![preassigned, now, id])?;
            self.conn
                .prepare_cached("DELETE FROM leases WHERE task_id = ?")?
                .execute([id])?;
            self.event(
                "system",
                "claim_expired",
                "task",
                id,
                json!({ "previousAssignee": assignee }),
            )?;
        }
        Ok(())
    }

    pub fn create_task(&self, actor: &Identity, input: CreateTaskInput) -> Result<Task> {
        let title = bounded_string(Some(&input.title), "title", limits::TITLE, true)?;
        let brief = bounded_string(input.brief.as_deref(), "brief", limits::BRIEF, false)?;
        let acceptance = bounded_string(
            input.acceptance.as_deref(),
            "acceptance",
            limits::ACCEPTANCE,
            false,
        )?;
        let role = bounded_string(input.role.as_deref(), "role", limits::ROLE, false)?;
        let priority = input.priority.clone().unwrap_or_else(|| "normal".into());
        if !PRIORITIES.contains(&priority.as_str()) {
            return Err(BusError::invalid(format!("invalid priority: {priority}")));
        }
        let to = match &input.to {
            Some(to) if !to.is_empty() => Some(identity::assert_safe_agent_id(to)?.to_string()),
            _ => None,
        };
        let reviewer = match &input.reviewer {
            Some(reviewer) if !reviewer.is_empty() => {
                Some(identity::assert_safe_agent_id(reviewer)?.to_string())
            }
            _ => None,
        };
        let refs = context_references(input.refs.as_ref())?;
        let mut seen_deps = std::collections::HashSet::new();
        let dependencies: Vec<i64> = input
            .dependencies
            .iter()
            .copied()
            .filter(|dep| seen_deps.insert(*dep))
            .collect();
        let project = match &input.project {
            Some(project) if !project.is_empty() => {
                let raw = bounded_string(Some(project), "project", limits::PATH, false)?;
                Some(crate::db::absolutize(Path::new(&raw)).display().to_string())
            }
            _ => None,
        };
        let raw_scopes: Vec<String> = input
            .path_scopes
            .iter()
            .map(|scope| bounded_string(Some(scope), "path scope", limits::PATH, true))
            .collect::<Result<Vec<_>>>()?;
        if !raw_scopes.is_empty() && project.is_none() {
            return Err(BusError::invalid("path scopes need a project directory"));
        }
        let mut path_scopes: Vec<String> = match &project {
            Some(project) => raw_scopes
                .iter()
                .map(|scope| normalize_scope(project, scope))
                .collect::<Result<Vec<_>>>()?,
            None => vec![],
        };
        path_scopes.sort();
        path_scopes.dedup();
        let max_retries = input.max_retries.unwrap_or(2);
        self.write(|bus| {
            bus.reopen_expired_claims()?;
            let now = bus.now();
            if let Some(to) = &to {
                if !bus.agent_exists(to)? {
                    return Err(BusError::not_found(format!("unknown assignee: {to}")));
                }
            }
            if let Some(reviewer) = &reviewer {
                if !bus.agent_exists(reviewer)? {
                    return Err(BusError::not_found(format!("unknown reviewer: {reviewer}")));
                }
            }
            if let Some(parent_id) = input.parent_id {
                bus.require_task(parent_id)?;
            }
            bus.assert_may_delegate(actor, to.as_deref(), input.parent_id)?;
            let mut blocked = false;
            for dep in &dependencies {
                blocked = bus.require_task(*dep)?.state != "accepted" || blocked;
            }
            let state = if blocked { "blocked" } else { "open" };
            bus.conn
                .prepare_cached(
                    "INSERT INTO tasks(project, parent_id, title, brief, acceptance, role, priority, state, creator, assignee, reviewer,
                       path_scopes_json, refs_json, max_retries, created_ms, updated_ms)
                     VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                )?
                .execute(params![
                    project, input.parent_id, title, brief, acceptance, role, priority, state,
                    actor.agent_id, to, reviewer,
                    serde_json::to_string(&path_scopes)?, serde_json::to_string(&refs)?, max_retries, now, now
                ])?;
            let id = bus.conn.last_insert_rowid();
            {
                let mut insert_dep = bus.conn.prepare_cached("INSERT OR IGNORE INTO task_deps(task_id, depends_on) VALUES(?, ?)")?;
                for dep in &dependencies {
                    insert_dep.execute(params![id, dep])?;
                }
            }
            bus.event(&actor.agent_id, "task_created", "task", id, json!({
                "title": title.chars().take(200).collect::<String>(), "assignee": to, "role": role, "state": state,
            }))?;
            if let Some(to) = &to {
                if to != &actor.agent_id {
                    let body = [
                        brief.clone(),
                        if acceptance.is_empty() { String::new() } else { format!("Acceptance:\n{acceptance}") },
                        format!("Claim with `qagent task claim {id}`, submit with `qagent task submit {id} --summary ...`."),
                    ]
                    .into_iter()
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n\n");
                    bus.insert_message(&actor.agent_id, Some(to), "task", &format!("[TASK #{id}] {title}"), &body, &format!("task-{id}"), Some(id), &refs, false)?;
                }
            }
            bus.touch(&actor.agent_id, None)?;
            bus.require_task(id)
        })
    }

    /// Delegation rules, checked inside the creating transaction against the permissions
    /// stored now (not those resolved when the caller identified itself). Creating work for
    /// someone else, or for anyone to claim, needs canDelegate; a policy can also name the
    /// only agents this one may assign and how deep under existing tasks it may create work.
    /// Mirror: assertMayDelegate in src/core/bus.ts.
    fn assert_may_delegate(&self, actor: &Identity, to: Option<&str>, parent_id: Option<i64>) -> Result<()> {
        if actor.authority == "operator" {
            return Ok(());
        }
        let permissions = identity::current_permissions(&self.conn, &actor.agent_id)?
            .unwrap_or_else(|| actor.permissions.clone());
        let me = actor.agent_id.as_str();
        let for_someone_else = to != Some(me);
        if for_someone_else && !permissions.can_delegate {
            return Err(BusError::forbidden(format!(
                "{me} may not delegate: it can only create tasks assigned to itself (--to {me})"
            )));
        }
        if let (Some(to), Some(allowed)) = (to, &permissions.allowed_child_agent_ids) {
            if to != me && !allowed.is_empty() && !allowed.iter().any(|id| id == to) {
                return Err(BusError::forbidden(format!(
                    "{me} may only assign work to {}, not {to}",
                    allowed.join(", ")
                )));
            }
        }
        if let (true, Some(max), Some(parent_id)) =
            (for_someone_else, permissions.max_delegation_depth, parent_id)
        {
            let mut depth: i64 = 0;
            let mut next = Some(parent_id);
            while let Some(id) = next {
                if depth > max {
                    break;
                }
                next = self
                    .conn
                    .prepare_cached("SELECT parent_id FROM tasks WHERE id = ?")?
                    .query_row([id], |row| row.get::<_, Option<i64>>(0))
                    .optional()?
                    .flatten();
                depth += 1;
            }
            if depth > max {
                return Err(BusError::forbidden(format!(
                    "{me} may create work at most {max} level(s) below a top-level task"
                )));
            }
        }
        Ok(())
    }

    /// How many tasks the agent holds claimed right now.
    fn claimed_count(&self, agent_id: &str) -> Result<i64> {
        Ok(self
            .conn
            .prepare_cached("SELECT COUNT(*) FROM tasks WHERE state = 'claimed' AND assignee = ?")?
            .query_row([agent_id], |row| row.get(0))?)
    }

    /**
     * Claim a task atomically. The claim is one UPDATE ... WHERE state IN ('open','changes_requested')
     * AND (assignee IS NULL OR assignee = me) RETURNING; it wins only if that row came back.
     * Without an id, the most urgent claimable task assigned to me, or unassigned for my role, is taken.
     */
    pub fn claim_task(&self, actor: &Identity, task_id: Option<i64>) -> Result<Task> {
        let me = actor.agent_id.clone();
        let explicit = task_id.is_some();
        self.write(|bus| {
            bus.reopen_expired_claims()?;
            let limit = identity::current_permissions(&bus.conn, &me)?
                .unwrap_or_else(|| actor.permissions.clone())
                .max_concurrent_tasks;
            if let Some(limit) = limit {
                let held = bus.claimed_count(&me)?;
                if held >= limit {
                    return Err(BusError::conflict(format!(
                        "{me} already holds {held} claimed task(s), its limit; submit or release one first"
                    )));
                }
            }
            let role = if explicit { String::new() } else { bus.agent_role(&me)? };
            // Assigned-to-me first, then urgent before older ordinary work. Pages past the first
            // 100 too: a pile of lease-conflicted urgent tasks must not starve later claimable ones.
            let mut page_start: i64 = 0;
            loop {
                let candidates: Vec<ClaimCandidate> = if explicit {
                    let id = task_id.unwrap();
                    if id <= 0 {
                        return Err(BusError::invalid(format!("invalid task id: {}", task_id.unwrap())));
                    }
                    match bus.task_row(id)? {
                        Some(task) => vec![(task.id, task.state.clone(), task.assignee.clone(), task.project.clone(), task.path_scopes.clone())],
                        None => return Err(BusError::not_found(format!("unknown task: {id}"))),
                    }
                } else {
                    let mut stmt = bus.conn.prepare_cached(
                        "SELECT id, state, assignee, project, path_scopes_json FROM tasks
                         WHERE state IN ('open', 'changes_requested')
                           AND (assignee = ? OR (assignee IS NULL AND (role = '' OR role = ?)))
                         ORDER BY CASE WHEN assignee = ? THEN 0 ELSE 1 END,
                                  CASE priority WHEN 'urgent' THEN 0 WHEN 'high' THEN 1 WHEN 'normal' THEN 2 ELSE 3 END,
                                  id LIMIT 100 OFFSET ?",
                    )?;
                    let rows = stmt.query_map(params![me, role, me, page_start], |row| {
                        let scopes_json: Option<String> = row.get(4)?;
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, Option<String>>(2)?,
                            row.get::<_, Option<String>>(3)?,
                            json_parse(scopes_json.as_deref(), Vec::<String>::new()),
                        ))
                    })?;
                    let mut page = Vec::new();
                    for row in rows {
                        page.push(row?);
                    }
                    page_start += 100;
                    if page.is_empty() {
                        break;
                    }
                    page
                };
                for (id, state, assignee, project, path_scopes) in candidates {
                    let conflicts = bus.lease_conflicts(id, project.as_deref(), &path_scopes)?;
                    if !conflicts.is_empty() {
                        if explicit {
                            let list = conflicts.iter().map(|(task_id, path)| format!("#{task_id}:{path}")).collect::<Vec<_>>().join(", ");
                            return Err(BusError::conflict(format!(
                                "task {id} path scopes overlap leases held by {list}"
                            )));
                        }
                        continue;
                    }
                    let now = bus.now();
                    let claimed: Option<Task> = bus
                        .conn
                        .prepare_cached(
                            "UPDATE tasks SET state = 'claimed', assignee = ?, claim_expires_ms = ?, updated_ms = ?
                             WHERE id = ? AND state IN ('open', 'changes_requested') AND (assignee IS NULL OR assignee = ?)
                             RETURNING *",
                        )?
                        .query_row(params![me, now + bus.claim_ttl_ms, now, id, me], |row| bus.to_task(row, None))
                        .optional()?;
                    let Some(task) = claimed else {
                        if explicit {
                            let reason = match &assignee {
                                Some(assignee) if assignee != &me => format!("is {state} and assigned to {assignee}"),
                                _ => format!("is {state}"),
                            };
                            return Err(BusError::conflict(format!("task {id} cannot be claimed: it {reason}")));
                        }
                        continue;
                    };
                    if let Some(project) = &task.project {
                        let mut insert = bus.conn.prepare_cached(
                            "INSERT OR REPLACE INTO leases(project, path, task_id, created_ms) VALUES(?, ?, ?, ?)",
                        )?;
                        for path in &task.path_scopes {
                            insert.execute(params![project, path, task.id, now])?;
                        }
                    }
                    bus.event(&me, "task_claimed", "task", task.id, json!({ "round": task.round, "leases": task.path_scopes }))?;
                    bus.touch(&me, Some("working"))?;
                    return Ok(task);
                }
                if explicit {
                    break;
                }
            }
            Err(BusError::not_found("no claimable task"))
        })
    }

    fn lease_conflicts(
        &self,
        task_id: i64,
        project: Option<&str>,
        path_scopes: &[String],
    ) -> Result<Vec<(i64, String)>> {
        if project.is_none() || path_scopes.is_empty() {
            return Ok(vec![]);
        }
        let mut stmt = self.conn.prepare_cached(
            "SELECT task_id, path FROM leases WHERE project = ? AND task_id <> ?",
        )?;
        let rows = stmt.query_map(params![project.unwrap(), task_id], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut conflicts = Vec::new();
        for row in rows {
            let (held_task, held_path) = row?;
            for wanted in path_scopes {
                if scopes_overlap(wanted, &held_path) {
                    conflicts.push((held_task, held_path.clone()));
                }
            }
        }
        Ok(conflicts)
    }

    pub fn note_task(&self, actor: &Identity, task_id: i64, text: &str) -> Result<TaskNote> {
        let body = bounded_string(Some(text), "note", limits::NOTE, true)?;
        self.write(|bus| {
            bus.reopen_expired_claims()?;
            let task = bus.require_task(task_id)?;
            let now = bus.now();
            bus.conn
                .prepare_cached(
                    "INSERT INTO task_notes(task_id, author, ts_ms, body) VALUES(?, ?, ?, ?)",
                )?
                .execute(params![task.id, actor.agent_id, now, body])?;
            let id = bus.conn.last_insert_rowid();
            if task.state == "claimed" && task.assignee.as_deref() == Some(actor.agent_id.as_str())
            {
                bus.conn
                    .prepare_cached(
                        "UPDATE tasks SET claim_expires_ms = ?, updated_ms = ? WHERE id = ?",
                    )?
                    .execute(params![now + bus.claim_ttl_ms, now, task.id])?;
            } else {
                bus.conn
                    .prepare_cached("UPDATE tasks SET updated_ms = ? WHERE id = ?")?
                    .execute(params![now, task.id])?;
            }
            bus.event(
                &actor.agent_id,
                "task_note",
                "task",
                task.id,
                json!({ "noteId": id }),
            )?;
            bus.touch(&actor.agent_id, None)?;
            Ok(TaskNote {
                id,
                task_id: task.id,
                author: actor.agent_id.clone(),
                ts_ms: now,
                body,
            })
        })
    }

    pub fn submit_task(&self, actor: &Identity, task_id: i64, input: SubmitInput) -> Result<Task> {
        let summary = bounded_string(Some(&input.summary), "summary", limits::SUMMARY, true)?;
        let details = bounded_string(input.details.as_deref(), "details", limits::DETAILS, false)?;
        let mut seen_files = std::collections::HashSet::new();
        let changed_files: Vec<String> = input
            .changed_files
            .iter()
            .map(|file| {
                bounded_string(Some(file), "changed file", limits::REF_VALUE, true)
                    .map(|s| s.trim().to_string())
            })
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .filter(|file| seen_files.insert(file.clone()))
            .collect();
        if changed_files.len() > limits::CHANGED_FILES {
            return Err(BusError::invalid(format!(
                "at most {} changed files",
                limits::CHANGED_FILES
            )));
        }
        let artifacts = context_references(input.artifacts.as_ref())?;
        let validation = match &input.validation {
            None | Some(serde_json::Value::Null) => Vec::new(),
            Some(value) => {
                let list = value
                    .as_array()
                    .ok_or_else(|| BusError::invalid("validation must be a list"))?;
                if list.len() > limits::VALIDATION {
                    return Err(BusError::invalid(format!(
                        "at most {} validation entries",
                        limits::VALIDATION
                    )));
                }
                list.iter()
                    .map(|item| {
                        let row = item.as_object();
                        let get = |key: &str| row.and_then(|r| r.get(key));
                        Ok(ValidationObservation {
                            passed: row
                                .and_then(|r| r.get("passed"))
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false),
                            summary: bounded_string(
                                get("summary").and_then(|v| v.as_str()).or(item.as_str()),
                                "validation summary",
                                4096,
                                true,
                            )?,
                            command: match get("command").and_then(|v| v.as_str()) {
                                Some(_) => Some(bounded_string(
                                    get("command").and_then(|v| v.as_str()),
                                    "validation command",
                                    8192,
                                    false,
                                )?),
                                None => None,
                            },
                        })
                    })
                    .collect::<Result<Vec<_>>>()?
            }
        };
        self.write(|bus| {
            bus.reopen_expired_claims()?;
            let task = bus.require_task(task_id)?;
            if task.assignee.as_deref() != Some(actor.agent_id.as_str()) {
                return Err(BusError::forbidden(format!(
                    "task {} is assigned to {}, not {}",
                    task.id,
                    task.assignee.as_deref().unwrap_or("nobody"),
                    actor.agent_id
                )));
            }
            if task.state != "claimed" && task.state != "changes_requested" {
                return Err(BusError::conflict(format!("task {} is {}; claim it before submitting", task.id, task.state)));
            }
            let now = bus.now();
            let result = TaskResult {
                summary: summary.clone(), details: details.clone(), changed_files: changed_files.clone(),
                artifacts: artifacts.clone(), validation: validation.clone(), completed_ms: now,
            };
            bus.conn
                .prepare_cached("UPDATE tasks SET state = 'submitted', result_json = ?, claim_expires_ms = NULL, updated_ms = ? WHERE id = ?")?
                .execute(params![serde_json::to_string(&result)?, now, task.id])?;
            let reviewer = task.reviewer.clone().unwrap_or_else(|| task.creator.clone());
            bus.event(&actor.agent_id, "task_submitted", "task", task.id, json!({ "round": task.round, "reviewer": reviewer }))?;
            if reviewer != actor.agent_id {
                let body = [
                    summary.clone(),
                    if details.is_empty() { String::new() } else { format!("Details:\n{details}") },
                    if changed_files.is_empty() { String::new() } else { format!("Changed files:\n{}", changed_files.iter().map(|f| format!("- {f}")).collect::<Vec<_>>().join("\n")) },
                    if validation.is_empty() {
                        String::new()
                    } else {
                        format!("Validation:\n{}", validation.iter().map(|v| format!("- {}: {}{}", if v.passed { "passed" } else { "failed" }, v.summary, v.command.as_deref().map(|c| format!(" ({c})")).unwrap_or_default())).collect::<Vec<_>>().join("\n"))
                    },
                    format!("Review with `qagent task review {} --accept|--revise --feedback ...`.", task.id),
                ]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join("\n\n");
                bus.insert_message(&actor.agent_id, Some(&reviewer), "result", &format!("[DONE #{} r{}] {}", task.id, task.round, task.title), &body, &format!("task-{}", task.id), Some(task.id), &artifacts, false)?;
            }
            bus.touch(&actor.agent_id, Some("idle"))?;
            bus.require_task(task.id)
        })
    }

    pub fn review_task(
        &self,
        actor: &Identity,
        task_id: i64,
        accepted: bool,
        feedback: &str,
    ) -> Result<Task> {
        let feedback = bounded_string(Some(feedback), "feedback", limits::FEEDBACK, true)?;
        self.write(|bus| {
            bus.reopen_expired_claims()?;
            let task = bus.require_task(task_id)?;
            let reviewer = task.reviewer.clone().unwrap_or_else(|| task.creator.clone());
            let operator = actor.authority == "operator";
            if !operator && actor.agent_id != reviewer {
                return Err(BusError::forbidden(format!("only {reviewer} or the operator may review task {}", task.id)));
            }
            if !operator && actor.agent_id == task.assignee.clone().unwrap_or_default() {
                return Err(BusError::forbidden(format!("{} cannot review its own work on task {}", actor.agent_id, task.id)));
            }
            if task.state != "submitted" {
                return Err(BusError::conflict(format!("task {} is {}, not submitted", task.id, task.state)));
            }
            let now = bus.now();
            let review = TaskReview { reviewer: actor.agent_id.clone(), accepted, feedback: feedback.clone(), reviewed_ms: now };
            let thread = format!("task-{}", task.id);
            if accepted {
                bus.conn
                    .prepare_cached("UPDATE tasks SET state = 'accepted', review_json = ?, updated_ms = ? WHERE id = ?")?
                    .execute(params![serde_json::to_string(&review)?, now, task.id])?;
                bus.conn.prepare_cached("DELETE FROM leases WHERE task_id = ?")?.execute([task.id])?;
                bus.event(&actor.agent_id, "task_accepted", "task", task.id, json!({ "round": task.round }))?;
                if let Some(assignee) = &task.assignee {
                    if assignee != &actor.agent_id {
                        bus.insert_message(&actor.agent_id, Some(assignee), "feedback",
                            &format!("[ACCEPTED #{}] {}", task.id, task.title),
                            &format!("{feedback}\n\nNo further action is required on this task."),
                            &thread, Some(task.id), &[], false)?;
                    }
                }
                bus.unblock_dependents(task.id, &actor.agent_id)?;
            } else {
                let round = task.round + 1;
                if round - 1 > task.max_retries {
                    bus.conn
                        .prepare_cached("UPDATE tasks SET state = 'failed', round = ?, review_json = ?, claim_expires_ms = NULL, updated_ms = ? WHERE id = ?")?
                        .execute(params![round, serde_json::to_string(&review)?, now, task.id])?;
                    bus.conn.prepare_cached("DELETE FROM leases WHERE task_id = ?")?.execute([task.id])?;
                    bus.event(&actor.agent_id, "task_failed", "task", task.id, json!({ "round": round, "reason": "review retry limit exceeded" }))?;
                    if task.creator != actor.agent_id {
                        bus.insert_message(&actor.agent_id, Some(&task.creator), "control",
                            &format!("[ESCALATE #{}] review retry limit exceeded", task.id),
                            &feedback, &thread, Some(task.id), &[], false)?;
                    }
                } else {
                    bus.conn
                        .prepare_cached("UPDATE tasks SET state = 'changes_requested', round = ?, review_json = ?, claim_expires_ms = NULL, updated_ms = ? WHERE id = ?")?
                        .execute(params![round, serde_json::to_string(&review)?, now, task.id])?;
                    bus.event(&actor.agent_id, "task_changes_requested", "task", task.id, json!({ "round": round }))?;
                    if let Some(assignee) = &task.assignee {
                        bus.insert_message(&actor.agent_id, Some(assignee), "feedback",
                            &format!("[CHANGES #{} r{}] {}", task.id, round, task.title),
                            &format!("{feedback}\n\nRevise the existing work and submit the same task again."),
                            &thread, Some(task.id), &[], false)?;
                    }
                }
            }
            bus.touch(&actor.agent_id, None)?;
            bus.require_task(task.id)
        })
    }

    fn unblock_dependents(&self, task_id: i64, actor: &str) -> Result<()> {
        let dependents: Vec<i64> = {
            let mut stmt = self.conn.prepare_cached(
                "SELECT t.id FROM task_deps d JOIN tasks t ON t.id = d.task_id WHERE d.depends_on = ? AND t.state = 'blocked'",
            )?;
            let rows = stmt.query_map([task_id], |row| row.get(0))?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            out
        };
        for id in dependents {
            let open: i64 = self
                .conn
                .prepare_cached(
                    "SELECT COUNT(*) AS n FROM task_deps d JOIN tasks t ON t.id = d.depends_on WHERE d.task_id = ? AND t.state <> 'accepted'",
                )?
                .query_row([id], |row| row.get(0))?;
            if open == 0 {
                self.conn
                    .prepare_cached("UPDATE tasks SET state = 'open', updated_ms = ? WHERE id = ?")?
                    .execute(params![self.now(), id])?;
                self.event(
                    actor,
                    "task_unblocked",
                    "task",
                    id,
                    json!({ "releasedBy": task_id }),
                )?;
            }
        }
        Ok(())
    }

    /// Give a claimed task back without failing it: state 'open', the creator's original
    /// assignee (or nobody) restored, leases released. The assignee or the operator may release.
    pub fn release_task(
        &self,
        actor: &Identity,
        task_id: i64,
        reason: Option<&str>,
    ) -> Result<Task> {
        let text = {
            let t = bounded_string(reason, "reason", limits::REASON, false)?;
            if t.is_empty() {
                "released".to_string()
            } else {
                t
            }
        };
        self.write(|bus| {
            let before = bus.require_task(task_id)?;
            if before.state != "claimed" {
                return Err(BusError::conflict(format!("task {} is {}, not claimed", before.id, before.state)));
            }
            if actor.authority != "operator" && Some(&actor.agent_id) != before.assignee.as_ref() {
                return Err(BusError::forbidden(format!(
                    "only {} or the operator may release task {}",
                    before.assignee.as_deref().unwrap_or("the assignee"),
                    before.id
                )));
            }
            // Snapshot first: an expired claim is reopened by this sweep, which must not
            // count as "not claimed" (and must not be rolled back by throwing after it).
            let claim_expired = before.claim_expires_ms.is_some_and(|expiry| expiry < bus.now());
            bus.reopen_expired_claims()?;
            let task = bus.require_task(task_id)?;
            if !claim_expired && task.state != "claimed" {
                return Err(BusError::conflict(format!("task {} is {}, not claimed", task.id, task.state)));
            }
            let now = bus.now();
            if !claim_expired {
                let created: Option<String> = bus
                    .conn
                    .prepare_cached(
                        "SELECT data_json FROM events WHERE entity = 'task' AND entity_id = ? AND kind = 'task_created' ORDER BY seq LIMIT 1",
                    )?
                    .query_row([task.id.to_string()], |row| row.get(0))
                    .ok();
                let preassigned: Option<String> = created
                    .as_deref()
                    .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
                    .and_then(|data| data.get("assignee").and_then(|v| v.as_str().map(|s| s.to_string())));
                bus.conn
                    .prepare_cached("UPDATE tasks SET state = 'open', assignee = ?, claim_expires_ms = NULL, updated_ms = ? WHERE id = ?")?
                    .execute(params![preassigned, now, task.id])?;
                bus.conn.prepare_cached("DELETE FROM leases WHERE task_id = ?")?.execute([task.id])?;
            }
            bus.event(&actor.agent_id, "task_released", "task", task.id, json!({
                "reason": text.chars().take(500).collect::<String>(), "previousAssignee": before.assignee,
            }))?;
            bus.touch(&actor.agent_id, Some("idle"))?;
            bus.require_task(task.id)
        })
    }

    /// Release a claimed task back to the pool with no assignee — anyone may claim it.
    pub fn requeue_task(
        &self,
        actor: &Identity,
        task_id: i64,
        reason: Option<&str>,
    ) -> Result<Task> {
        let text = {
            let t = bounded_string(reason, "reason", limits::REASON, false)?;
            if t.is_empty() {
                "requeued".to_string()
            } else {
                t
            }
        };
        self.write(|bus| {
            let before = bus.require_task(task_id)?;
            if before.state != "claimed" && before.state != "open" {
                return Err(BusError::conflict(format!(
                    "task {} is {}, not claimed or open",
                    before.id, before.state
                )));
            }
            if actor.authority != "operator" && Some(&actor.agent_id) != before.assignee.as_ref() {
                return Err(BusError::forbidden(format!(
                    "only {} or the operator may requeue task {}",
                    before.assignee.as_deref().unwrap_or("the assignee"),
                    before.id
                )));
            }
            bus.reopen_expired_claims()?;
            let now = bus.now();
            bus.conn
                .prepare_cached("UPDATE tasks SET state = 'open', assignee = NULL, claim_expires_ms = NULL, updated_ms = ? WHERE id = ?")?
                .execute(params![now, before.id])?;
            bus.conn.prepare_cached("DELETE FROM leases WHERE task_id = ?")?.execute([before.id])?;
            bus.event(&actor.agent_id, "task_released", "task", before.id, json!({
                "reason": text.chars().take(500).collect::<String>(),
                "previousAssignee": before.assignee,
                "requeued": true,
            }))?;
            bus.touch(&actor.agent_id, Some("idle"))?;
            bus.require_task(before.id)
        })
    }

    /// Report that work on a claimed task failed. The attempt count rises; within
    /// max_retries the task reopens for the same assignee with a retry message,
    /// beyond it the task fails and the creator gets an escalation.
    pub fn fail_task(&self, actor: &Identity, task_id: i64, error: &str) -> Result<Task> {
        let note = bounded_string(Some(error), "failure", limits::NOTE, true)?;
        self.write(|bus| {
            bus.reopen_expired_claims()?;
            let task = bus.require_task(task_id)?;
            if actor.authority != "operator" && Some(&actor.agent_id) != task.assignee.as_ref() {
                return Err(BusError::forbidden(format!(
                    "only {} or the operator may report failure for task {}",
                    task.assignee.as_deref().unwrap_or("the assignee"),
                    task.id
                )));
            }
            if task.state != "claimed" {
                return Err(BusError::conflict(format!("task {} is {}, not claimed", task.id, task.state)));
            }
            let now = bus.now();
            let attempts = task.attempts + 1;
            let thread = format!("task-{}", task.id);
            bus.conn
                .prepare_cached("INSERT INTO task_notes(task_id, author, ts_ms, body) VALUES(?, ?, ?, ?)")?
                .execute(params![task.id, actor.agent_id, now, format!("failure (attempt {attempts}): {note}")])?;
            bus.conn.prepare_cached("DELETE FROM leases WHERE task_id = ?")?.execute([task.id])?;
            if attempts <= task.max_retries {
                bus.conn
                    .prepare_cached("UPDATE tasks SET state = 'open', attempts = ?, claim_expires_ms = NULL, updated_ms = ? WHERE id = ?")?
                    .execute(params![attempts, now, task.id])?;
                bus.event(&actor.agent_id, "task_retry", "task", task.id, json!({ "attempts": attempts, "maxRetries": task.max_retries }))?;
                if let Some(assignee) = &task.assignee {
                    bus.insert_message("system", Some(assignee), "task",
                        &format!("[RETRY #{}] attempt {}: {}", task.id, attempts + 1, task.title),
                        &format!("{note}\n\nRetry the original scoped task. Do not broaden scope."),
                        &thread, Some(task.id), &task.refs, false)?;
                }
            } else {
                bus.conn
                    .prepare_cached("UPDATE tasks SET state = 'failed', attempts = ?, claim_expires_ms = NULL, updated_ms = ? WHERE id = ?")?
                    .execute(params![attempts, now, task.id])?;
                bus.event(&actor.agent_id, "task_failed", "task", task.id, json!({ "attempts": attempts, "reason": "retry limit exceeded" }))?;
                if task.creator != actor.agent_id {
                    bus.insert_message(&actor.agent_id, Some(&task.creator), "control",
                        &format!("[ESCALATE #{}] attempts exhausted", task.id),
                        &note, &thread, Some(task.id), &[], false)?;
                }
            }
            bus.touch(&actor.agent_id, Some("idle"))?;
            bus.require_task(task.id)
        })
    }

    pub fn cancel_task(
        &self,
        actor: &Identity,
        task_id: i64,
        reason: Option<&str>,
    ) -> Result<Task> {
        let text = {
            let t = bounded_string(reason, "reason", limits::REASON, false)?;
            if t.is_empty() {
                "cancelled".to_string()
            } else {
                t
            }
        };
        self.write(|bus| {
            // The expired-claim sweep can clear this task's assignee; the former claimer still
            // deserves the cancelled notice, so remember who held it.
            let prior_assignee: Option<String> = bus
                .conn
                .prepare_cached("SELECT assignee FROM tasks WHERE id = ?")?
                .query_row([task_id], |row| row.get(0))
                .ok()
                .flatten();
            bus.reopen_expired_claims()?;
            let task = bus.require_task(task_id)?;
            if actor.authority != "operator" && actor.agent_id != task.creator {
                return Err(BusError::forbidden(format!("only {} or the operator may cancel task {}", task.creator, task.id)));
            }
            if CLOSED_STATES.contains(&task.state.as_str()) {
                return Err(BusError::conflict(format!("task {} is already {}", task.id, task.state)));
            }
            let now = bus.now();
            bus.conn
                .prepare_cached("UPDATE tasks SET state = 'cancelled', claim_expires_ms = NULL, updated_ms = ? WHERE id = ?")?
                .execute(params![now, task.id])?;
            bus.conn.prepare_cached("DELETE FROM leases WHERE task_id = ?")?.execute([task.id])?;
            bus.event(&actor.agent_id, "task_cancelled", "task", task.id, json!({ "reason": text.chars().take(500).collect::<String>() }))?;
            let notify = task.assignee.clone().or(prior_assignee);
            if let Some(notify) = notify {
                if notify != actor.agent_id {
                    bus.insert_message(&actor.agent_id, Some(&notify), "control",
                        &format!("[CANCELLED #{}] {}", task.id, task.title),
                        &text, &format!("task-{}", task.id), Some(task.id), &[], false)?;
                }
            }
            bus.touch(&actor.agent_id, None)?;
            bus.require_task(task.id)
        })
    }

    // ---------------------------------------------------------------- status

    pub fn status(&self) -> Result<StatusResult> {
        let mut counts = BTreeMap::new();
        {
            let mut stmt = self
                .conn
                .prepare_cached("SELECT state, COUNT(*) AS n FROM tasks GROUP BY state")?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?;
            for row in rows {
                let (state, n) = row?;
                counts.insert(state, n);
            }
        }
        let agents = self
            .list_agents()?
            .into_iter()
            .map(|(agent, unread)| {
                let mut value = serde_json::to_value(&agent).unwrap_or(json!({}));
                value
                    .as_object_mut()
                    .map(|o| o.insert("unread".into(), json!(unread)));
                value
            })
            .collect();
        Ok(StatusResult {
            db_path: self.db_path.display().to_string(),
            seq: self.latest_seq()?,
            agents,
            counts,
            open_tasks: self.list_tasks(ListTasksInput {
                limit: Some(200),
                ..Default::default()
            })?,
        })
    }
}
