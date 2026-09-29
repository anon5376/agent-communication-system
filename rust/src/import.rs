//! Import of the three legacy ACS stores into a v2 bus. Ported verbatim from
//! src/core/import.ts: bus.jsonl lines, the qagent state.sqlite, and the
//! prototype_0.2 coordinator.db all become v2 messages/tasks/agents/events.
//! Idempotent per source file sha256, recorded in meta.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::db::{absolutize, append_event, get_meta, open_database, set_meta, SCHEMA_SQL};
use crate::error::Result;
use crate::identity::is_safe_agent_id;
use crate::types::OPERATOR_ID;
use crate::types::{ContextReference, PRIORITIES};

const CLAIM_TTL_MS: i64 = 2 * 60 * 60 * 1000;

fn qagent_state(legacy: &str) -> Option<&'static str> {
    Some(match legacy {
        "blocked" => "blocked",
        "ready" | "assigned" => "open",
        "in_progress" => "claimed",
        "submitted" => "submitted",
        "changes_requested" => "changes_requested",
        "accepted" => "accepted",
        "failed" => "failed",
        "cancelled" => "cancelled",
        _ => return None,
    })
}

fn prototype_state(legacy: &str) -> Option<&'static str> {
    Some(match legacy {
        "todo" | "assigned" => "open",
        "in_progress" => "claimed",
        "blocked" => "blocked",
        "review" => "submitted",
        "done" => "accepted",
        "failed" => "failed",
        "cancelled" => "cancelled",
        _ => return None,
    })
}

fn message_type(legacy: &str) -> String {
    match legacy {
        "info" | "question" | "answer" | "task" | "result" | "feedback" | "control" => {
            legacy.to_string()
        }
        _ => "info".to_string(),
    }
}

fn bump(record: &mut HashMap<String, u64>, key: &str) {
    *record.entry(key.to_string()).or_insert(0) += 1;
}

fn file_sha256(path: &Path) -> String {
    let mut hasher = Sha256::new();
    if let Ok(bytes) = fs::read(path) {
        hasher.update(&bytes);
    }
    // A live SQLite source may hold committed pages in its WAL.
    let wal = PathBuf::from(format!("{}-wal", path.display()));
    if let Ok(bytes) = fs::read(&wal) {
        hasher.update(&bytes);
    }
    format!("{:x}", hasher.finalize())
}

fn text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        _ => String::new(),
    }
}

fn parse_json(value: &str, fallback: serde_json::Value) -> serde_json::Value {
    if value.is_empty() {
        return fallback;
    }
    serde_json::from_str(value).unwrap_or(fallback)
}

/// `Number(value) || fallback`: a finite, nonzero number wins; else fallback.
fn num_or(value: &serde_json::Value, fallback: i64) -> i64 {
    match value.as_f64() {
        Some(n) if n.is_finite() && n != 0.0 => n as i64,
        _ => fallback,
    }
}

/// isoMs: numbers pass through (including 0); strings get Date.parse'd; else fallback.
fn iso_ms(value: &serde_json::Value, fallback: i64) -> i64 {
    if let Some(n) = value.as_f64() {
        if n.is_finite() {
            return n as i64;
        }
    }
    if let serde_json::Value::String(s) = value {
        if let Ok(d) = chrono::DateTime::parse_from_rfc3339(s) {
            return d.timestamp_millis();
        }
    }
    fallback
}

fn refs_from(value: &serde_json::Value) -> Vec<ContextReference> {
    let Some(list) = value.as_array() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in list.iter().take(100) {
        if item.is_object() {
            let t = text(&item["type"]);
            let ref_type = if crate::types::REFERENCE_TYPES.contains(&t.as_str()) {
                t
            } else {
                "artifact".to_string()
            };
            let v = {
                let v = text(&item["value"]);
                if v.is_empty() {
                    let v = text(&item["path"]);
                    if v.is_empty() {
                        text(&item["url"])
                    } else {
                        v
                    }
                } else {
                    v
                }
            };
            if v.is_empty() {
                continue;
            }
            out.push(ContextReference {
                ref_type,
                value: v.chars().take(4096).collect(),
                description: {
                    let d = text(&item["description"]);
                    if d.is_empty() {
                        None
                    } else {
                        Some(d.chars().take(2048).collect())
                    }
                },
                digest: None,
            });
        } else {
            let v = text(item);
            if !v.is_empty() {
                out.push(ContextReference {
                    ref_type: "artifact".to_string(),
                    value: v.chars().take(4096).collect(),
                    description: None,
                    digest: None,
                });
            }
        }
    }
    out
}

// ------------------------------------------------------------------- reports

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceReport {
    pub kind: String,
    pub path: String,
    pub sha256: String,
    pub already_imported: bool,
    pub read: HashMap<String, u64>,
    pub inserted: HashMap<String, u64>,
    pub duplicates: HashMap<String, u64>,
    pub invalid: HashMap<String, u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub db_path: String,
    pub dry_run: bool,
    pub sources: Vec<SourceReport>,
    pub cursor_seq: i64,
}

#[derive(Debug, Clone)]
pub struct AgentIn {
    pub id: String,
    pub role: String,
    pub model: String,
    pub harness: String,
    pub parent: Option<String>,
    pub created_ms: i64,
    pub last_seen_ms: Option<i64>,
    pub meta: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct IdentityIn {
    pub agent_id: String,
    pub token_hash: String,
    pub authority: String,
    pub permissions_json: String,
    pub created_ms: i64,
    pub updated_ms: i64,
}

#[derive(Debug, Clone)]
pub struct TaskIn {
    pub legacy_id: String,
    pub parent_legacy: Option<String>,
    pub title: String,
    pub brief: String,
    pub acceptance: String,
    pub role: String,
    pub priority: String,
    pub state: String,
    pub creator: String,
    pub assignee: Option<String>,
    pub reviewer: Option<String>,
    pub path_scopes: Vec<String>,
    pub refs: Vec<ContextReference>,
    pub result: Option<serde_json::Value>,
    pub review: Option<serde_json::Value>,
    pub round: i64,
    pub attempts: i64,
    pub max_retries: i64,
    pub claim_expires_ms: Option<i64>,
    pub created_ms: i64,
    pub updated_ms: i64,
}

#[derive(Debug, Clone)]
pub struct DepIn {
    pub legacy_task_id: String,
    pub legacy_depends_on: String,
}

#[derive(Debug, Clone)]
pub struct MessageIn {
    pub id: String,
    pub ts_ms: i64,
    pub sender: String,
    pub recipient: Option<String>,
    pub msg_type: String,
    pub subject: String,
    pub body: String,
    pub thread: String,
    pub legacy_task_id: Option<String>,
    pub refs: Vec<ContextReference>,
    pub requires_ack: bool,
}

#[derive(Debug, Clone)]
pub struct AckIn {
    pub message_id: String,
    pub agent_id: String,
    pub ack_ms: i64,
}

#[derive(Debug, Clone)]
pub struct NoteIn {
    pub legacy_task_id: String,
    pub author: String,
    pub ts_ms: i64,
    pub body: String,
}

#[derive(Debug, Clone)]
pub struct EventIn {
    pub ts_ms: i64,
    pub actor: String,
    pub kind: String,
    pub entity: String,
    pub entity_id: String,
    pub data: serde_json::Value,
}

#[derive(Debug, Default)]
pub struct Loaded {
    pub agents: Vec<AgentIn>,
    pub identities: Vec<IdentityIn>,
    pub tasks: Vec<TaskIn>,
    pub deps: Vec<DepIn>,
    pub messages: Vec<MessageIn>,
    pub acks: Vec<AckIn>,
    pub notes: Vec<NoteIn>,
    pub events: Vec<EventIn>,
    pub report: SourceReport,
}

fn empty_report(kind: &str, path: &Path, sha256: String) -> SourceReport {
    SourceReport {
        kind: kind.to_string(),
        path: path.display().to_string(),
        sha256,
        ..Default::default()
    }
}

// ------------------------------------------------------------------- sources

pub struct ImportSources {
    pub jsonl: Option<PathBuf>,
    pub qagent_state: Option<PathBuf>,
    pub prototype: Option<PathBuf>,
}

/// bus.jsonl and state.sqlite next to the database, and ~/prototype_0.2/prototype.db.
pub fn default_import_sources(home: &Path) -> ImportSources {
    let jsonl = home.join("bus.jsonl");
    let qagent_state = home.join("state.sqlite");
    let prototype = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("prototype_0.2/prototype.db");
    ImportSources {
        jsonl: jsonl.exists().then_some(jsonl),
        qagent_state: qagent_state.exists().then_some(qagent_state),
        prototype: prototype.exists().then_some(prototype),
    }
}

fn load_jsonl(path: &Path, now: i64) -> Loaded {
    let mut loaded = Loaded {
        report: empty_report("bus.jsonl", path, file_sha256(path)),
        ..Default::default()
    };
    let mut registrations: HashMap<String, AgentIn> = HashMap::new();
    let mut registration_order: Vec<String> = Vec::new();
    let mut seen_messages: HashSet<String> = HashSet::new();
    let content = fs::read_to_string(path).unwrap_or_default();
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        bump(&mut loaded.report.read, "lines");
        let entry: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => {
                bump(&mut loaded.report.invalid, "lines");
                continue;
            }
        };
        let kind = text(&entry["kind"]);
        let empty = serde_json::json!({});
        let data = if entry.get("data").map(|d| d.is_object()).unwrap_or(false) {
            &entry["data"]
        } else {
            &empty
        };
        let ts = iso_ms(&entry["ts"], now);
        if kind == "message" {
            bump(&mut loaded.report.read, "messages");
            let id = text(&data["id"]);
            if id.is_empty() {
                bump(&mut loaded.report.invalid, "messages");
                continue;
            }
            if !seen_messages.insert(id.clone()) {
                bump(&mut loaded.report.read, "duplicateLines");
                continue;
            }
            let legacy_task_id = if data["taskId"].is_null() {
                None
            } else {
                Some(text(&data["taskId"]))
            };
            loaded.messages.push(MessageIn {
                id,
                ts_ms: iso_ms(&data["ts"], ts),
                sender: {
                    let s = text(&data["from"]);
                    if s.is_empty() {
                        "unknown".to_string()
                    } else {
                        s
                    }
                },
                recipient: {
                    let to = text(&data["to"]);
                    if to == "*" || to.is_empty() {
                        None
                    } else {
                        Some(to)
                    }
                },
                msg_type: message_type(&text(&data["type"])),
                subject: text(&data["subject"]),
                body: text(&data["body"]),
                thread: legacy_task_id.clone().unwrap_or_default(),
                legacy_task_id,
                refs: refs_from(&data["refs"]),
                requires_ack: false,
            });
        } else if kind == "register" {
            bump(&mut loaded.report.read, "registrations");
            let id = text(&data["id"]);
            if !is_safe_agent_id(&id) {
                bump(&mut loaded.report.invalid, "registrations");
                continue;
            }
            let previous = registrations.get(&id);
            if previous.is_none() {
                registration_order.push(id.clone());
            }
            let prev_role = previous.map(|p| p.role.clone());
            let prev_model = previous.map(|p| p.model.clone());
            let prev_harness = previous.map(|p| p.harness.clone());
            let prev_created = previous.map(|p| p.created_ms);
            registrations.insert(
                id.clone(),
                AgentIn {
                    id,
                    role: {
                        let r = text(&data["role"]);
                        if !r.is_empty() {
                            r
                        } else {
                            prev_role.unwrap_or_default()
                        }
                    },
                    model: {
                        let m = text(&data["model"]);
                        if !m.is_empty() {
                            m
                        } else {
                            prev_model.unwrap_or_default()
                        }
                    },
                    harness: {
                        let h = text(&data["harness"]);
                        if !h.is_empty() {
                            h
                        } else {
                            prev_harness.unwrap_or_default()
                        }
                    },
                    parent: None,
                    created_ms: prev_created.unwrap_or(ts),
                    last_seen_ms: Some(ts),
                    meta: serde_json::json!({ "importedFrom": "bus.jsonl" }),
                },
            );
        } else {
            bump(&mut loaded.report.read, "events");
            let task_id = {
                let t = text(&data["taskId"]);
                if t.is_empty() {
                    text(&data["id"])
                } else {
                    t
                }
            };
            let entity = if kind.starts_with("task_") {
                "task"
            } else if !data["target"].is_null()
                || !data["id"].is_null()
                || kind.starts_with("operator_")
            {
                "agent"
            } else {
                "system"
            };
            let entity_id = if entity == "task" {
                task_id
            } else {
                let t = text(&data["target"]);
                let t = if t.is_empty() { text(&data["id"]) } else { t };
                if t.is_empty() {
                    if kind.starts_with("operator_") {
                        OPERATOR_ID.to_string()
                    } else {
                        String::new()
                    }
                } else {
                    t
                }
            };
            let summary = if kind == "task_create" {
                let summary_task_id = {
                    let i = text(&data["id"]);
                    if i.is_empty() {
                        text(&data["taskId"])
                    } else {
                        i
                    }
                };
                serde_json::json!({
                    "taskId": summary_task_id,
                    "title": text(&data["title"]).chars().take(200).collect::<String>(),
                    "assigner": data["assigner"].clone(),
                    "assignee": data["assignee"].clone(),
                    "state": data["state"].clone(),
                })
            } else {
                data.clone()
            };
            loaded.events.push(EventIn {
                ts_ms: ts,
                actor: {
                    let a = text(&data["actor"]);
                    let a = if a.is_empty() { text(&data["by"]) } else { a };
                    let a = if a.is_empty() {
                        text(&data["assigner"])
                    } else {
                        a
                    };
                    if a.is_empty() {
                        "system".to_string()
                    } else {
                        a
                    }
                },
                kind,
                entity: entity.to_string(),
                entity_id,
                data: summary,
            });
        }
    }
    loaded.agents = registration_order
        .iter()
        .filter_map(|id| registrations.get(id))
        .cloned()
        .collect();
    loaded
        .report
        .read
        .insert("agents".to_string(), loaded.agents.len() as u64);
    loaded
        .report
        .read
        .entry("messages".to_string())
        .or_insert(0);
    loaded
}

fn open_source(path: &Path) -> Result<Connection> {
    Ok(Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?)
}

fn load_qagent_state(path: &Path, now: i64) -> Result<Loaded> {
    let mut loaded = Loaded {
        report: empty_report("qagent", path, file_sha256(path)),
        ..Default::default()
    };
    let db = open_source(path)?;

    {
        let mut stmt = db.prepare("SELECT id, json, updated_at FROM agents ORDER BY id")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;
        for row in rows.flatten() {
            bump(&mut loaded.report.read, "agents");
            let (row_id, json, updated_at) = row;
            let agent = parse_json(&json, serde_json::json!({}));
            let id = row_id;
            if !is_safe_agent_id(&id) {
                bump(&mut loaded.report.invalid, "agents");
                continue;
            }
            loaded.agents.push(AgentIn {
                id,
                role: text(&agent["role"]),
                model: text(&agent["model"]),
                harness: text(&agent["harness"]),
                parent: None,
                created_ms: num_or(
                    &agent["registeredAt"],
                    num_or(&serde_json::json!(updated_at), now),
                ),
                last_seen_ms: {
                    let v = num_or(&agent["lastSeen"], 0);
                    if v == 0 {
                        None
                    } else {
                        Some(v)
                    }
                },
                meta: serde_json::json!({
                    "importedFrom": "qagent",
                    "family": agent["family"].clone(),
                    "provider": agent["provider"].clone(),
                    "description": agent["description"].clone(),
                }),
            });
        }
    }
    {
        let mut stmt = db.prepare("SELECT * FROM identities ORDER BY id")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>("id")?,
                row.get::<_, String>("token_hash")?,
                row.get::<_, String>("authority")?,
                row.get::<_, String>("permissions_json")?,
                row.get::<_, i64>("created_at")?,
                row.get::<_, i64>("updated_at")?,
            ))
        })?;
        for row in rows.flatten() {
            bump(&mut loaded.report.read, "identities");
            let (agent_id, token_hash, authority, permissions_json, created_at, updated_at) = row;
            if !is_safe_agent_id(&agent_id) || token_hash.is_empty() {
                bump(&mut loaded.report.invalid, "identities");
                continue;
            }
            loaded.identities.push(IdentityIn {
                agent_id,
                token_hash,
                authority: if ["operator", "manager", "worker"].contains(&authority.as_str()) {
                    authority
                } else {
                    "worker".to_string()
                },
                permissions_json: if permissions_json.is_empty() {
                    "{}".to_string()
                } else {
                    permissions_json
                },
                created_ms: if created_at != 0 { created_at } else { now },
                updated_ms: if updated_at != 0 { updated_at } else { now },
            });
        }
    }
    {
        let mut stmt =
            db.prepare("SELECT seq, id, to_agent, json, created_at FROM messages ORDER BY seq")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })?;
        for row in rows.flatten() {
            bump(&mut loaded.report.read, "messages");
            let (_seq, row_id, to_agent, json, created_at) = row;
            let message = parse_json(&json, serde_json::json!({}));
            let id = row_id;
            if id.is_empty() {
                bump(&mut loaded.report.invalid, "messages");
                continue;
            }
            let legacy_task_id = if message["taskId"].is_null() {
                None
            } else {
                Some(text(&message["taskId"]))
            };
            loaded.messages.push(MessageIn {
                id,
                ts_ms: num_or(&message["ts"], num_or(&serde_json::json!(created_at), now)),
                sender: {
                    let s = text(&message["from"]);
                    if s.is_empty() {
                        "unknown".to_string()
                    } else {
                        s
                    }
                },
                recipient: {
                    if to_agent == "*" || to_agent.is_empty() {
                        None
                    } else {
                        Some(to_agent)
                    }
                },
                msg_type: message_type(&text(&message["type"])),
                subject: text(&message["subject"]),
                body: text(&message["body"]),
                thread: legacy_task_id.clone().unwrap_or_default(),
                legacy_task_id,
                refs: refs_from(&message["refs"]),
                requires_ack: false,
            });
        }
    }
    {
        let mut stmt = db.prepare("SELECT id, json FROM tasks ORDER BY id")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows.flatten() {
            bump(&mut loaded.report.read, "tasks");
            let (legacy_id, json) = row;
            let task = parse_json(&json, serde_json::json!({}));
            let state = qagent_state(&text(&task["state"]));
            if legacy_id.is_empty() || state.is_none() || text(&task["title"]).is_empty() {
                bump(&mut loaded.report.invalid, "tasks");
                continue;
            }
            let state = state.unwrap();
            let updated_ms = num_or(&task["updatedAt"], now);
            let requirements = task["validationRequirements"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let acceptance = requirements
                .iter()
                .map(|req| text(&req["description"]))
                .filter(|d| !d.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            let result = if task["result"].is_object() {
                &task["result"]
            } else {
                &serde_json::Value::Null
            };
            let review = if task["review"].is_object() {
                &task["review"]
            } else {
                &serde_json::Value::Null
            };
            loaded.tasks.push(TaskIn {
                legacy_id: legacy_id.clone(),
                parent_legacy: if task["parentTaskId"].is_null() {
                    None
                } else {
                    Some(text(&task["parentTaskId"]))
                },
                title: text(&task["title"]).chars().take(500).collect(),
                brief: text(&task["brief"]),
                acceptance,
                role: text(&task["role"]),
                priority: "normal".to_string(),
                state: state.to_string(),
                creator: {
                    let c = text(&task["assigner"]);
                    if c.is_empty() {
                        OPERATOR_ID.to_string()
                    } else {
                        c
                    }
                },
                assignee: {
                    let a = text(&task["assignee"]);
                    if a.is_empty() {
                        None
                    } else {
                        Some(a)
                    }
                },
                reviewer: {
                    let r = text(&task["reviewerId"]);
                    if r.is_empty() {
                        None
                    } else {
                        Some(r)
                    }
                },
                path_scopes: task["pathScopes"]
                    .as_array()
                    .map(|a| a.iter().map(text).collect())
                    .unwrap_or_default(),
                refs: refs_from(&task["contextRefs"]),
                result: if result.is_object() {
                    Some(serde_json::json!({
                        "summary": text(&result["summary"]),
                        "details": text(&result["details"]),
                        "changedFiles": result["changedFiles"].clone(),
                        "artifacts": result["artifacts"].clone(),
                        "validation": result["validation"].clone(),
                        "completedMs": num_or(&result["completedAt"], updated_ms),
                    }))
                } else {
                    None
                },
                review: if review.is_object() {
                    Some(serde_json::json!({
                        "reviewer": text(&review["reviewer"]),
                        "accepted": review["accepted"].as_bool().unwrap_or(false),
                        "feedback": text(&review["feedback"]),
                        "reviewedMs": num_or(&review["reviewedAt"], updated_ms),
                    }))
                } else {
                    None
                },
                round: num_or(&task["round"], 1),
                attempts: num_or(&task["attempts"], 0),
                max_retries: {
                    let m = num_or(&task["maxRetries"], 0);
                    if task["maxRetries"].is_null() {
                        2
                    } else {
                        m
                    }
                },
                claim_expires_ms: if state == "claimed" {
                    Some(updated_ms + CLAIM_TTL_MS)
                } else {
                    None
                },
                created_ms: num_or(&task["createdAt"], updated_ms),
                updated_ms,
            });
            if let Some(deps) = task["dependencyIds"].as_array() {
                for dep in deps {
                    bump(&mut loaded.report.read, "dependencies");
                    loaded.deps.push(DepIn {
                        legacy_task_id: legacy_id.clone(),
                        legacy_depends_on: text(dep),
                    });
                }
            }
            if let Some(history) = task["history"].as_array() {
                for entry in history {
                    bump(&mut loaded.report.read, "history");
                    loaded.events.push(EventIn {
                        ts_ms: num_or(&entry["ts"], updated_ms),
                        actor: {
                            let a = text(&entry["actor"]);
                            if a.is_empty() {
                                "system".to_string()
                            } else {
                                a
                            }
                        },
                        kind: format!("task_{}", {
                            let k = text(&entry["kind"]);
                            if k.is_empty() {
                                "event".to_string()
                            } else {
                                k
                            }
                        }),
                        entity: "task".to_string(),
                        entity_id: legacy_id.clone(),
                        data: serde_json::json!({
                            "state": entry["state"].clone(),
                            "note": text(&entry["note"]).chars().take(500).collect::<String>(),
                        }),
                    });
                }
            }
        }
    }
    Ok(loaded)
}

fn load_prototype(path: &Path, now: i64) -> Result<Loaded> {
    let mut loaded = Loaded {
        report: empty_report("prototype", path, file_sha256(path)),
        ..Default::default()
    };
    let db = open_source(path)?;
    let legacy = |v: serde_json::Value| format!("prototype:{}", text(&v));

    {
        let mut stmt = db.prepare("SELECT * FROM agents ORDER BY id")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>("id")?,
                row.get::<_, String>("role")?,
                row.get::<_, String>("model")?,
                row.get::<_, Option<String>>("parent_id")?,
                row.get::<_, String>("display_name")?,
                row.get::<_, String>("capabilities")?,
                row.get::<_, String>("permissions")?,
                row.get::<_, String>("meta")?,
                row.get::<_, String>("created_ts")?,
                row.get::<_, Option<String>>("heartbeat_ts")?,
            ))
        })?;
        for row in rows.flatten() {
            bump(&mut loaded.report.read, "agents");
            let (
                id,
                role,
                model,
                parent_id,
                display_name,
                capabilities,
                permissions,
                meta,
                created_ts,
                heartbeat_ts,
            ) = row;
            if !is_safe_agent_id(&id) {
                bump(&mut loaded.report.invalid, "agents");
                continue;
            }
            loaded.agents.push(AgentIn {
                id,
                role,
                model,
                harness: String::new(),
                parent: parent_id.filter(|p| !p.is_empty()),
                created_ms: iso_ms(&serde_json::Value::String(created_ts), now),
                last_seen_ms: heartbeat_ts.map(|t| iso_ms(&serde_json::Value::String(t), now)),
                meta: serde_json::json!({
                    "importedFrom": "prototype",
                    "displayName": display_name,
                    "capabilities": parse_json(&capabilities, serde_json::json!([])),
                    "permissions": parse_json(&permissions, serde_json::json!([])),
                    "meta": parse_json(&meta, serde_json::json!({})),
                }),
            });
        }
    }
    {
        let mut stmt = db.prepare("SELECT * FROM messages ORDER BY id")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>("id")?,
                row.get::<_, String>("ts")?,
                row.get::<_, String>("sender")?,
                row.get::<_, Option<String>>("recipient")?,
                row.get::<_, String>("subject")?,
                row.get::<_, String>("body")?,
                row.get::<_, String>("thread")?,
                row.get::<_, i64>("requires_ack")?,
            ))
        })?;
        for row in rows.flatten() {
            bump(&mut loaded.report.read, "messages");
            let (id, ts, sender, recipient, subject, body, thread, requires_ack) = row;
            loaded.messages.push(MessageIn {
                id: legacy(serde_json::json!(id)),
                ts_ms: iso_ms(&serde_json::Value::String(ts), now),
                sender: if sender.is_empty() {
                    "unknown".to_string()
                } else {
                    sender
                },
                recipient,
                msg_type: "info".to_string(),
                subject,
                body,
                thread,
                legacy_task_id: None,
                refs: vec![],
                requires_ack: requires_ack == 1,
            });
        }
    }
    {
        let mut stmt = db.prepare("SELECT message_id, agent_id, ack_ts FROM message_receipts ORDER BY message_id, agent_id")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?;
        for row in rows.flatten() {
            bump(&mut loaded.report.read, "receipts");
            let (message_id, agent_id, ack_ts) = row;
            let Some(ack_ts) = ack_ts else { continue };
            bump(&mut loaded.report.read, "acks");
            loaded.acks.push(AckIn {
                message_id: legacy(serde_json::json!(message_id)),
                agent_id,
                ack_ms: iso_ms(&serde_json::Value::String(ack_ts), now),
            });
        }
    }
    {
        let mut stmt = db.prepare("SELECT * FROM tasks ORDER BY id")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>("id")?,
                row.get::<_, String>("title")?,
                row.get::<_, String>("description")?,
                row.get::<_, String>("acceptance")?,
                row.get::<_, String>("priority")?,
                row.get::<_, String>("status")?,
                row.get::<_, String>("creator")?,
                row.get::<_, Option<String>>("assignee")?,
                row.get::<_, Option<i64>>("parent_id")?,
                row.get::<_, String>("artifacts")?,
                row.get::<_, String>("created_ts")?,
                row.get::<_, String>("updated_ts")?,
            ))
        })?;
        for row in rows.flatten() {
            bump(&mut loaded.report.read, "tasks");
            let (
                id,
                title,
                description,
                acceptance,
                priority,
                status,
                creator,
                assignee,
                parent_id,
                artifacts,
                created_ts,
                updated_ts,
            ) = row;
            let state = match prototype_state(&status) {
                Some(s) if !title.is_empty() => s,
                _ => {
                    bump(&mut loaded.report.invalid, "tasks");
                    continue;
                }
            };
            let updated_ms = iso_ms(&serde_json::Value::String(updated_ts), now);
            loaded.tasks.push(TaskIn {
                legacy_id: legacy(serde_json::json!(id)),
                parent_legacy: parent_id.map(|p| legacy(serde_json::json!(p))),
                title: title.chars().take(500).collect(),
                brief: description,
                acceptance,
                role: String::new(),
                priority: if PRIORITIES.contains(&priority.as_str()) {
                    priority
                } else {
                    "normal".to_string()
                },
                state: state.to_string(),
                creator: if creator.is_empty() {
                    "unknown".to_string()
                } else {
                    creator
                },
                assignee: assignee.filter(|a| !a.is_empty()),
                reviewer: None,
                path_scopes: vec![],
                refs: refs_from(&parse_json(&artifacts, serde_json::json!([]))),
                result: None,
                review: None,
                round: 1,
                attempts: 0,
                max_retries: 2,
                claim_expires_ms: if state == "claimed" {
                    Some(updated_ms + CLAIM_TTL_MS)
                } else {
                    None
                },
                created_ms: iso_ms(&serde_json::Value::String(created_ts), updated_ms),
                updated_ms,
            });
        }
    }
    {
        let mut stmt = db.prepare("SELECT task_id, depends_on_task_id FROM task_dependencies")?;
        let rows = stmt.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))?;
        for row in rows.flatten() {
            bump(&mut loaded.report.read, "dependencies");
            loaded.deps.push(DepIn {
                legacy_task_id: legacy(serde_json::json!(row.0)),
                legacy_depends_on: legacy(serde_json::json!(row.1)),
            });
        }
    }
    {
        let mut stmt =
            db.prepare("SELECT task_id, author, ts, note FROM task_notes ORDER BY id")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        for row in rows.flatten() {
            bump(&mut loaded.report.read, "notes");
            loaded.notes.push(NoteIn {
                legacy_task_id: legacy(serde_json::json!(row.0)),
                author: if row.1.is_empty() {
                    "unknown".to_string()
                } else {
                    row.1
                },
                ts_ms: iso_ms(&serde_json::Value::String(row.2), now),
                body: row.3,
            });
        }
    }
    {
        let mut stmt = db.prepare(
            "SELECT ts, actor, kind, entity_type, entity_id, data FROM events ORDER BY id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        })?;
        for row in rows.flatten() {
            bump(&mut loaded.report.read, "events");
            let (ts, actor, kind, entity_type, entity_id, data) = row;
            let entity = if entity_type.is_empty() {
                "system"
            } else {
                entity_type.as_str()
            };
            let entity_id = if entity == "task" || entity == "message" {
                legacy(serde_json::json!(entity_id))
            } else {
                entity_id
            };
            loaded.events.push(EventIn {
                ts_ms: iso_ms(&serde_json::Value::String(ts), now),
                actor: if actor.is_empty() {
                    "system".to_string()
                } else {
                    actor
                },
                kind,
                entity: entity.to_string(),
                entity_id,
                data: parse_json(&data, serde_json::json!({})),
            });
        }
    }
    Ok(loaded)
}

// ------------------------------------------------------------------- writer

fn meta_key(sha256: &str) -> String {
    format!("import:{sha256}")
}

fn open_target(db_path: &Path, dry_run: bool) -> Result<Connection> {
    if dry_run && !db_path.exists() {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA_SQL)?;
        return Ok(conn);
    }
    open_database(db_path)
}

#[derive(Debug, Default)]
pub struct ImportOptions {
    pub now: Option<i64>,
    pub dry_run: bool,
    pub force: bool,
    pub actor: Option<String>,
}

fn lookup_task(stmt: &mut rusqlite::Statement, legacy_id: &Option<String>) -> Result<Option<i64>> {
    let Some(legacy_id) = legacy_id else {
        return Ok(None);
    };
    Ok(stmt.query_row([legacy_id], |row| row.get::<_, i64>(0)).ok())
}

pub fn run_import(
    db_path: &Path,
    sources: ImportSources,
    options: ImportOptions,
) -> Result<ImportReport> {
    let now = options
        .now
        .unwrap_or_else(|| chrono::Utc::now().timestamp_millis());
    let dry_run = options.dry_run;
    let actor = options.actor.unwrap_or_else(|| OPERATOR_ID.to_string());
    let mut loaded: Vec<Loaded> = Vec::new();
    // Order matters for which copy of an agent wins: the richest source first.
    if let Some(p) = &sources.qagent_state {
        loaded.push(load_qagent_state(&absolutize(p), now)?);
    }
    if let Some(p) = &sources.prototype {
        loaded.push(load_prototype(&absolutize(p), now)?);
    }
    if let Some(p) = &sources.jsonl {
        loaded.push(load_jsonl(&absolutize(p), now));
    }

    let db_path = absolutize(db_path);
    let db = open_target(&db_path, dry_run)?;
    let mut cursor_seq: i64 = 0;

    // Dry runs keep every page in memory so the rolled-back transaction never spills to the WAL.
    if dry_run {
        db.execute_batch("PRAGMA cache_size = -1048576")?;
    }
    db.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| -> Result<()> {
        for source in loaded.iter_mut() {
            source.report.already_imported =
                !options.force && get_meta(&db, &meta_key(&source.report.sha256))?.is_some();
        }
        let active: Vec<usize> = (0..loaded.len())
            .filter(|&i| !loaded[i].report.already_imported)
            .collect();

        // Agents: insert with no parent first, then link parents that exist.
        {
            let mut insert_agent = db.prepare(
                "INSERT OR IGNORE INTO agents(id, role, model, harness, status, last_seen_ms, created_ms, meta_json)
                 VALUES(?, ?, ?, ?, 'offline', ?, ?, ?)",
            )?;
            for &i in &active {
                let source = &mut loaded[i];
                for ai in 0..source.agents.len() {
                    let (id, role, model, harness, last_seen_ms, created_ms, meta) = {
                        let a = &source.agents[ai];
                        (
                            a.id.clone(),
                            a.role.clone(),
                            a.model.clone(),
                            a.harness.clone(),
                            a.last_seen_ms,
                            a.created_ms,
                            a.meta.to_string(),
                        )
                    };
                    let changes = insert_agent.execute(rusqlite::params![
                        id,
                        role,
                        model,
                        harness,
                        last_seen_ms,
                        created_ms,
                        meta,
                    ])?;
                    let report = if changes > 0 {
                        &mut source.report.inserted
                    } else {
                        &mut source.report.duplicates
                    };
                    bump(report, "agents");
                }
            }
            let mut link_parent = db.prepare(
                "UPDATE agents SET parent_id = ? WHERE id = ? AND parent_id IS NULL AND EXISTS (SELECT 1 FROM agents WHERE id = ?)",
            )?;
            for &i in &active {
                for agent in &loaded[i].agents {
                    if let Some(parent) = &agent.parent {
                        if parent != &agent.id {
                            link_parent.execute(rusqlite::params![parent, agent.id, parent])?;
                        }
                    }
                }
            }
        }

        {
            let mut insert_identity = db.prepare(
                "INSERT OR IGNORE INTO identities(agent_id, token_hash, authority, permissions_json, created_ms, updated_ms) VALUES(?, ?, ?, ?, ?, ?)",
            )?;
            let mut ensure_agent =
                db.prepare("INSERT OR IGNORE INTO agents(id, role, status, created_ms, meta_json) VALUES(?, ?, 'offline', ?, '{}')")?;
            for &i in &active {
                let source = &mut loaded[i];
                for ii in 0..source.identities.len() {
                    let (agent_id, token_hash, authority, permissions_json, created_ms, updated_ms) = {
                        let idn = &source.identities[ii];
                        (
                            idn.agent_id.clone(),
                            idn.token_hash.clone(),
                            idn.authority.clone(),
                            idn.permissions_json.clone(),
                            idn.created_ms,
                            idn.updated_ms,
                        )
                    };
                    ensure_agent.execute(rusqlite::params![
                        agent_id,
                        if authority == "operator" {
                            "operator"
                        } else {
                            ""
                        },
                        created_ms
                    ])?;
                    let changes = insert_identity.execute(rusqlite::params![
                        agent_id,
                        token_hash,
                        authority,
                        permissions_json,
                        created_ms,
                        updated_ms,
                    ])?;
                    let report = if changes > 0 {
                        &mut source.report.inserted
                    } else {
                        &mut source.report.duplicates
                    };
                    bump(report, "identities");
                }
            }
        }

        // Tasks, then parents and dependencies by legacy id.
        // Existence is checked first: an ignored INSERT would still advance the AUTOINCREMENT counter.
        {
            let mut task_exists = db.prepare("SELECT 1 AS ok FROM tasks WHERE legacy_id = ?")?;
            let mut insert_task = db.prepare(
                "INSERT INTO tasks(legacy_id, title, brief, acceptance, role, priority, state, creator, assignee, reviewer,
                    path_scopes_json, refs_json, result_json, review_json, round, attempts, max_retries, claim_expires_ms, created_ms, updated_ms)
                 VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )?;
            for &i in &active {
                let source = &mut loaded[i];
                for ti in 0..source.tasks.len() {
                    let t = &source.tasks[ti];
                    if task_exists.exists([&t.legacy_id])? {
                        bump(&mut source.report.duplicates, "tasks");
                        continue;
                    }
                    let changes = insert_task.execute(rusqlite::params![
                        t.legacy_id,
                        t.title,
                        t.brief,
                        t.acceptance,
                        t.role,
                        t.priority,
                        t.state,
                        t.creator,
                        t.assignee,
                        t.reviewer,
                        serde_json::to_string(&t.path_scopes).unwrap_or_else(|_| "[]".to_string()),
                        serde_json::to_string(&t.refs).unwrap_or_else(|_| "[]".to_string()),
                        t.result.as_ref().map(|v| v.to_string()),
                        t.review.as_ref().map(|v| v.to_string()),
                        t.round,
                        t.attempts,
                        t.max_retries,
                        t.claim_expires_ms,
                        t.created_ms,
                        t.updated_ms,
                    ])?;
                    let report = if changes > 0 {
                        &mut source.report.inserted
                    } else {
                        &mut source.report.duplicates
                    };
                    bump(report, "tasks");
                }
            }
            let mut task_id_for = db.prepare("SELECT id FROM tasks WHERE legacy_id = ?")?;
            let mut link_task =
                db.prepare("UPDATE tasks SET parent_id = ? WHERE id = ? AND parent_id IS NULL")?;
            let mut insert_dep =
                db.prepare("INSERT OR IGNORE INTO task_deps(task_id, depends_on) VALUES(?, ?)")?;
            for &i in &active {
                let source = &mut loaded[i];
                for ti in 0..source.tasks.len() {
                    let legacy_id = source.tasks[ti].legacy_id.clone();
                    let parent_legacy = source.tasks[ti].parent_legacy.clone();
                    let id = lookup_task(&mut task_id_for, &Some(legacy_id))?;
                    let parent = lookup_task(&mut task_id_for, &parent_legacy)?;
                    if let (Some(id), Some(parent)) = (id, parent) {
                        if id != parent {
                            link_task.execute(rusqlite::params![parent, id])?;
                        }
                    }
                }
                for di in 0..source.deps.len() {
                    let dep_task = source.deps[di].legacy_task_id.clone();
                    let dep_on = source.deps[di].legacy_depends_on.clone();
                    let id = lookup_task(&mut task_id_for, &Some(dep_task))?;
                    let on = lookup_task(&mut task_id_for, &Some(dep_on))?;
                    match (id, on) {
                        (Some(id), Some(on)) => {
                            let changes = insert_dep.execute(rusqlite::params![id, on])?;
                            let report = if changes > 0 {
                                &mut source.report.inserted
                            } else {
                                &mut source.report.duplicates
                            };
                            bump(report, "dependencies");
                        }
                        _ => bump(&mut source.report.invalid, "dependencies"),
                    }
                }
            }
        }

        // Messages from every source in time order, deduplicated by id.
        {
            let mut message_exists = db.prepare("SELECT 1 AS ok FROM messages WHERE id = ?")?;
            let mut insert_message = db.prepare(
                "INSERT INTO messages(id, ts_ms, sender, recipient, type, subject, body, thread, task_id, refs_json, requires_ack, source)
                 VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )?;
            let mut task_id_for = db.prepare("SELECT id FROM tasks WHERE legacy_id = ?")?;
            let mut queue: Vec<(i64, usize, usize)> = Vec::new(); // (tsMs, source rank, order)
            for (rank, &i) in active.iter().enumerate() {
                for order in 0..loaded[i].messages.len() {
                    queue.push((loaded[i].messages[order].ts_ms, rank, order));
                }
            }
            let source_at_rank: Vec<usize> = active.clone();
            queue.sort();
            for (ts_ms, rank, order) in queue {
                let _ = ts_ms;
                let i = source_at_rank[rank];
                let source = &mut loaded[i];
                let m = &source.messages[order];
                let (
                    id,
                    ts_ms,
                    sender,
                    recipient,
                    msg_type,
                    subject,
                    body,
                    thread,
                    legacy_task_id,
                    refs_json,
                    requires_ack,
                ) = (
                    m.id.clone(),
                    m.ts_ms,
                    m.sender.clone(),
                    m.recipient.clone(),
                    m.msg_type.clone(),
                    m.subject.clone(),
                    m.body.clone(),
                    m.thread.clone(),
                    m.legacy_task_id.clone(),
                    serde_json::to_string(&m.refs).unwrap_or_else(|_| "[]".to_string()),
                    m.requires_ack,
                );
                if message_exists.exists([&id])? {
                    bump(&mut source.report.duplicates, "messages");
                    continue;
                }
                let task_id: Option<i64> = legacy_task_id
                    .as_ref()
                    .and_then(|l| task_id_for.query_row([l], |row| row.get(0)).ok());
                let changes = insert_message.execute(rusqlite::params![
                    id,
                    ts_ms,
                    sender,
                    recipient,
                    msg_type,
                    subject,
                    body,
                    thread,
                    task_id,
                    refs_json,
                    if requires_ack { 1 } else { 0 },
                    source.report.kind,
                ])?;
                if changes > 0 {
                    bump(&mut source.report.inserted, "messages");
                    cursor_seq = cursor_seq.max(db.last_insert_rowid());
                } else {
                    bump(&mut source.report.duplicates, "messages");
                }
            }
        }

        {
            let mut seq_for = db.prepare("SELECT seq FROM messages WHERE id = ?")?;
            let mut insert_ack =
                db.prepare("INSERT OR IGNORE INTO acks(seq, agent_id, ack_ms) VALUES(?, ?, ?)")?;
            let mut insert_note = db.prepare(
                "INSERT INTO task_notes(task_id, author, ts_ms, body) SELECT ?, ?, ?, ?
                 WHERE NOT EXISTS (SELECT 1 FROM task_notes WHERE task_id = ? AND author = ? AND ts_ms = ? AND body = ?)",
            )?;
            let mut insert_event = db.prepare(
                "INSERT INTO events(ts_ms, actor, kind, entity, entity_id, data_json, source) SELECT ?, ?, ?, ?, ?, ?, ?
                 WHERE NOT EXISTS (SELECT 1 FROM events WHERE entity = ? AND entity_id = ? AND ts_ms = ? AND kind = ? AND actor = ? AND source = ? AND data_json = ?)",
            )?;
            let mut task_id_for = db.prepare("SELECT id FROM tasks WHERE legacy_id = ?")?;
            for &i in &active {
                let source = &mut loaded[i];
                for ack in &source.acks {
                    let seq: Option<i64> =
                        seq_for.query_row([&ack.message_id], |row| row.get(0)).ok();
                    match seq {
                        None => bump(&mut source.report.invalid, "acks"),
                        Some(seq) => {
                            let changes = insert_ack.execute(rusqlite::params![
                                seq,
                                ack.agent_id,
                                ack.ack_ms
                            ])?;
                            let report = if changes > 0 {
                                &mut source.report.inserted
                            } else {
                                &mut source.report.duplicates
                            };
                            bump(report, "acks");
                        }
                    }
                }
                for note in &source.notes {
                    let task_id: Option<i64> = task_id_for
                        .query_row([&note.legacy_task_id], |row| row.get(0))
                        .ok();
                    match task_id {
                        None => bump(&mut source.report.invalid, "notes"),
                        Some(task_id) => {
                            let changes = insert_note.execute(rusqlite::params![
                                task_id,
                                note.author,
                                note.ts_ms,
                                note.body,
                                task_id,
                                note.author,
                                note.ts_ms,
                                note.body,
                            ])?;
                            let report = if changes > 0 {
                                &mut source.report.inserted
                            } else {
                                &mut source.report.duplicates
                            };
                            bump(report, "notes");
                        }
                    }
                }
                let kind = source.report.kind.clone();
                for event in &source.events {
                    let data = event.data.to_string();
                    let changes = insert_event.execute(rusqlite::params![
                        event.ts_ms,
                        event.actor,
                        event.kind,
                        event.entity,
                        event.entity_id,
                        data,
                        kind,
                        event.entity,
                        event.entity_id,
                        event.ts_ms,
                        event.kind,
                        event.actor,
                        kind,
                        data,
                    ])?;
                    let report = if changes > 0 {
                        &mut source.report.inserted
                    } else {
                        &mut source.report.duplicates
                    };
                    bump(report, "events");
                }
            }
        }

        // No agent wakes to months of history: raise every cursor past the imported mail.
        if cursor_seq > 0 {
            db.execute(
                "INSERT INTO cursors(agent_id, last_seq) SELECT id, ? FROM agents WHERE true
                 ON CONFLICT(agent_id) DO UPDATE SET last_seq = MAX(cursors.last_seq, excluded.last_seq)",
                [cursor_seq],
            )?;
        }

        for &i in &active {
            let inserted: u64 = loaded[i].report.inserted.values().sum();
            let key = meta_key(&loaded[i].report.sha256);
            if get_meta(&db, &key)?.is_none() {
                set_meta(
                    &db,
                    &key,
                    &serde_json::json!({
                        "kind": loaded[i].report.kind,
                        "path": loaded[i].report.path,
                        "file": Path::new(&loaded[i].report.path).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default(),
                        "importedMs": now,
                        "read": loaded[i].report.read,
                        "inserted": loaded[i].report.inserted,
                    })
                    .to_string(),
                )?;
            }
            if inserted > 0 || get_meta(&db, &key)?.is_none() {
                append_event(
                    &db,
                    now,
                    &actor,
                    "import",
                    "system",
                    &loaded[i].report.kind.clone(),
                    &serde_json::json!({
                        "path": loaded[i].report.path,
                        "sha256": loaded[i].report.sha256,
                        "inserted": loaded[i].report.inserted,
                    }),
                )?;
            }
        }
        Ok(())
    })();
    match result {
        Ok(()) => db.execute_batch(if dry_run { "ROLLBACK" } else { "COMMIT" })?,
        Err(error) => {
            let _ = db.execute_batch("ROLLBACK");
            return Err(error);
        }
    }
    drop(db);

    Ok(ImportReport {
        db_path: db_path.display().to_string(),
        dry_run,
        sources: loaded.into_iter().map(|s| s.report).collect(),
        cursor_seq,
    })
}
