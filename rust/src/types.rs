//! Shared v2 types, mirroring src/core/types.ts. JSON shapes keep the same
//! camelCase keys so `--json` output is interchangeable.

use crate::error::{BusError, Result};
use serde::{Deserialize, Serialize};

pub type Authority = String;
pub type AgentStatus = String;
pub type TaskState = String;
pub type MessageType = String;
pub type Priority = String;

pub const TASK_STATES: &[&str] = &[
    "open",
    "blocked",
    "claimed",
    "submitted",
    "changes_requested",
    "accepted",
    "failed",
    "cancelled",
];
pub const CLOSED_STATES: &[&str] = &["accepted", "failed", "cancelled"];
pub const MESSAGE_TYPES: &[&str] = &[
    "info", "question", "answer", "task", "result", "feedback", "control",
];
pub const PRIORITIES: &[&str] = &["low", "normal", "high", "urgent"];
pub const REFERENCE_TYPES: &[&str] = &["path", "artifact", "summary", "commit", "url"];

pub const OPERATOR_ID: &str = "operator";
/// Claims expire after two hours; the next task write reopens them.
pub const CLAIM_TTL_MS: i64 = 2 * 60 * 60_000;
/// An agent not seen for this long shows offline.
pub const STALE_AGENT_MS: i64 = 15 * 60_000;
pub const DEFAULT_WAIT_SEC: i64 = 240;
pub const MAX_WAIT_SEC: i64 = 3600;

pub mod limits {
    pub const SUBJECT: usize = 1000;
    pub const BODY: usize = 200_000;
    pub const TITLE: usize = 500;
    pub const BRIEF: usize = 50_000;
    pub const ACCEPTANCE: usize = 20_000;
    pub const SUMMARY: usize = 20_000;
    pub const DETAILS: usize = 100_000;
    pub const FEEDBACK: usize = 50_000;
    pub const NOTE: usize = 20_000;
    pub const REASON: usize = 20_000;
    pub const REF_VALUE: usize = 4096;
    pub const REF_DESCRIPTION: usize = 2048;
    pub const REF_COUNT: usize = 100;
    pub const CHANGED_FILES: usize = 500;
    pub const VALIDATION: usize = 100;
    pub const THREAD: usize = 200;
    pub const ROLE: usize = 200;
    pub const MODEL: usize = 200;
    pub const PATH: usize = 8192;
    pub const INBOX_LIMIT: i64 = 200;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ContextReference {
    #[serde(rename = "type")]
    pub ref_type: String,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Agent {
    pub id: String,
    pub role: String,
    pub model: String,
    pub harness: String,
    pub parent_id: Option<String>,
    /// Derived: a waiter whose wait_until has passed, or an agent unseen for 15 minutes, is offline.
    pub status: AgentStatus,
    pub stored_status: String,
    pub wait_until_ms: Option<i64>,
    pub last_seen_ms: Option<i64>,
    pub created_ms: i64,
    pub authority: Option<String>,
    pub meta: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub seq: i64,
    pub id: String,
    pub ts_ms: i64,
    pub sender: String,
    /// None means broadcast.
    pub recipient: Option<String>,
    #[serde(rename = "type")]
    pub msg_type: String,
    pub subject: String,
    pub body: String,
    pub thread: String,
    pub task_id: Option<i64>,
    pub refs: Vec<ContextReference>,
    pub requires_ack: bool,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidationObservation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    pub passed: bool,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskResult {
    pub summary: String,
    pub details: String,
    pub changed_files: Vec<String>,
    pub artifacts: Vec<ContextReference>,
    pub validation: Vec<ValidationObservation>,
    pub completed_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskReview {
    pub reviewer: String,
    pub accepted: bool,
    pub feedback: String,
    pub reviewed_ms: i64,
}

/// The columns a board needs, without deps, notes or messages.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskSummary {
    pub id: i64,
    pub title: String,
    pub assignee: Option<String>,
    pub state: TaskState,
    pub created_ms: i64,
    pub updated_ms: i64,
}

/// The columns a status view needs, without role, authority or meta.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSummary {
    pub id: String,
    pub stored_status: String,
    pub wait_until_ms: Option<i64>,
    pub last_seen_ms: Option<i64>,
}

/// The columns a message-line view needs, without refs, thread or ack flags.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageSummary {
    pub seq: i64,
    pub ts_ms: i64,
    pub sender: String,
    pub recipient: Option<String>,
    pub subject: String,
    pub body: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    pub id: i64,
    pub legacy_id: Option<String>,
    pub project: Option<String>,
    pub parent_id: Option<i64>,
    pub title: String,
    pub brief: String,
    pub acceptance: String,
    pub role: String,
    pub priority: String,
    pub state: TaskState,
    pub creator: String,
    pub assignee: Option<String>,
    pub reviewer: Option<String>,
    pub path_scopes: Vec<String>,
    pub refs: Vec<ContextReference>,
    pub result: Option<TaskResult>,
    pub review: Option<TaskReview>,
    pub round: i64,
    pub attempts: i64,
    pub max_retries: i64,
    pub claim_expires_ms: Option<i64>,
    pub created_ms: i64,
    pub updated_ms: i64,
    pub dependencies: Vec<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskNote {
    pub id: i64,
    pub task_id: i64,
    pub author: String,
    pub ts_ms: i64,
    pub body: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskDetail {
    #[serde(flatten)]
    pub task: Task,
    pub notes: Vec<TaskNote>,
    pub dependents: Vec<i64>,
    pub messages: Vec<Message>,
    pub leases: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BusEvent {
    pub seq: i64,
    pub ts_ms: i64,
    pub actor: String,
    pub kind: String,
    pub entity: String,
    pub entity_id: String,
    pub data: serde_json::Value,
    pub source: String,
}

/// boundedString() from types.ts: trims nothing, rejects oversize and
/// (with `required`) blank input.
pub fn bounded_string(
    value: Option<&str>,
    label: &str,
    max: usize,
    required: bool,
) -> Result<String> {
    let text = value.unwrap_or("").to_string();
    if required && text.trim().is_empty() {
        return Err(BusError::invalid(format!("{label} is required")));
    }
    if text.chars().count() > max {
        return Err(BusError::invalid(format!(
            "{label} exceeds {max} characters"
        )));
    }
    Ok(text)
}

/// contextReferences() from types.ts over a JSON value.
pub fn context_references(value: Option<&serde_json::Value>) -> Result<Vec<ContextReference>> {
    let Some(value) = value else {
        return Ok(vec![]);
    };
    if value.is_null() {
        return Ok(vec![]);
    }
    let list = value
        .as_array()
        .ok_or_else(|| BusError::invalid("refs must be a list"))?;
    if list.len() > limits::REF_COUNT {
        return Err(BusError::invalid(format!(
            "at most {} refs are allowed",
            limits::REF_COUNT
        )));
    }
    list.iter()
        .map(|item| {
            let object = item.as_object();
            let get = |key: &str| object.and_then(|row| row.get(key)).and_then(|v| v.as_str());
            let ref_type = get("type").unwrap_or("path").to_string();
            if !REFERENCE_TYPES.contains(&ref_type.as_str()) {
                return Err(BusError::invalid(format!(
                    "invalid reference type: {ref_type}"
                )));
            }
            let scalar = if object.is_none() {
                item.as_str()
            } else {
                None
            };
            let reference = ContextReference {
                ref_type,
                value: bounded_string(
                    get("value").or(scalar),
                    "reference value",
                    limits::REF_VALUE,
                    true,
                )?,
                description: match get("description") {
                    Some(_) => Some(bounded_string(
                        get("description"),
                        "reference description",
                        limits::REF_DESCRIPTION,
                        false,
                    )?),
                    None => None,
                },
                digest: match get("digest") {
                    Some(_) => Some(bounded_string(
                        get("digest"),
                        "reference digest",
                        256,
                        false,
                    )?),
                    None => None,
                },
            };
            Ok(reference)
        })
        .collect()
}
