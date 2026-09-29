//! Plain-text rendering for the CLI — mirrors src/cli/format.ts (no colour,
//! so output pipes cleanly).

use crate::bus::StatusResult;
use crate::types::{Agent, BusEvent, Message, Task, TaskDetail};

fn iso(ts_ms: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ts_ms)
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
        .unwrap_or_else(|| "-".into())
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub fn fmt_ago(ts: Option<i64>, now: i64) -> String {
    let Some(ts) = ts else { return "-".into() };
    if ts == 0 {
        return "-".into();
    }
    let seconds = ((now - ts) / 1000).max(0);
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h", seconds / 3600)
    } else {
        format!("{}d", seconds / 86_400)
    }
}

fn clip(value: &str, width: usize) -> String {
    let flat: Vec<&str> = value.split_whitespace().collect();
    let flat = flat.join(" ");
    if flat.chars().count() > width {
        let mut clipped: String = flat.chars().take(width - 1).collect();
        clipped.push('…');
        clipped
    } else {
        flat
    }
}

fn char_len(s: &str) -> usize {
    s.chars().count()
}

fn table(rows: Vec<Vec<String>>) -> String {
    if rows.is_empty() {
        return String::new();
    }
    let cols = rows[0].len();
    let mut widths = vec![0usize; cols];
    for row in &rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(char_len(cell));
        }
    }
    rows.iter()
        .map(|row| {
            let mut line = String::new();
            for (i, cell) in row.iter().enumerate() {
                if i == row.len() - 1 {
                    line.push_str(cell);
                } else {
                    line.push_str(cell);
                    for _ in char_len(cell)..widths[i] {
                        line.push(' ');
                    }
                }
                if i < row.len() - 1 {
                    line.push_str("  ");
                }
            }
            line.trim_end().to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn render_agents(agents: &[(Agent, i64)]) -> String {
    if agents.is_empty() {
        return "(no agents)".into();
    }
    let mut rows = vec![vec![
        "ID".into(),
        "ROLE".into(),
        "STATUS".into(),
        "UNREAD".into(),
        "SEEN".into(),
        "MODEL".into(),
        "HARNESS".into(),
    ]];
    for (agent, unread) in agents {
        rows.push(vec![
            agent.id.clone(),
            clip(&agent.role, 24),
            agent.status.clone(),
            unread.to_string(),
            fmt_ago(agent.last_seen_ms, now_ms()),
            clip(&agent.model, 24),
            agent.harness.clone(),
        ]);
    }
    table(rows)
}

pub fn render_tasks(tasks: &[Task]) -> String {
    if tasks.is_empty() {
        return "(no tasks)".into();
    }
    let mut rows = vec![vec![
        "#".into(),
        "STATE".into(),
        "ROUND".into(),
        "ASSIGNEE".into(),
        "CREATOR".into(),
        "UPDATED".into(),
        "TITLE".into(),
    ]];
    for task in tasks {
        rows.push(vec![
            task.id.to_string(),
            task.state.clone(),
            format!("r{}", task.round),
            task.assignee.clone().unwrap_or_else(|| "-".into()),
            task.creator.clone(),
            fmt_ago(Some(task.updated_ms), now_ms()),
            clip(&task.title, 70),
        ]);
    }
    table(rows)
}

pub fn render_message(message: &Message) -> String {
    let to = message.recipient.clone().unwrap_or_else(|| "*".into());
    let task = message
        .task_id
        .map(|id| format!(" task {id}"))
        .unwrap_or_default();
    let ack = if message.requires_ack {
        " (ack requested)"
    } else {
        ""
    };
    let head = format!(
        "#{} {} {} -> {} [{}]{task}{ack}",
        message.seq,
        iso(message.ts_ms),
        message.sender,
        to,
        message.msg_type
    );
    let mut lines = vec![head];
    if !message.subject.is_empty() {
        lines.push(format!("  {}", message.subject));
    }
    if !message.body.is_empty() {
        for line in message.body.split('\n') {
            lines.push(format!("    {line}"));
        }
    }
    lines.join("\n")
}

pub fn render_messages(messages: &[Message], empty: &str) -> String {
    if messages.is_empty() {
        return empty.into();
    }
    messages
        .iter()
        .map(render_message)
        .collect::<Vec<_>>()
        .join("\n\n")
}

pub fn render_task(task: &TaskDetail) -> String {
    let mut lines = vec![
        format!("Task #{}: {}", task.task.id, task.task.title),
        format!(
            "  state      {} (round {}, max retries {})",
            task.task.state, task.task.round, task.task.max_retries
        ),
        format!("  creator    {}", task.task.creator),
        format!(
            "  assignee   {}",
            task.task.assignee.clone().unwrap_or_else(|| "-".into())
        ),
        format!(
            "  reviewer   {}",
            task.task
                .reviewer
                .clone()
                .unwrap_or_else(|| format!("{} (creator)", task.task.creator))
        ),
        format!(
            "  priority   {}{}",
            task.task.priority,
            if task.task.role.is_empty() {
                String::new()
            } else {
                format!(", role {}", task.task.role)
            }
        ),
    ];
    if let Some(legacy) = &task.task.legacy_id {
        lines.push(format!("  legacy id  {legacy}"));
    }
    if let Some(project) = &task.task.project {
        lines.push(format!("  project    {project}"));
    }
    if !task.task.path_scopes.is_empty() {
        let leases = if task.leases.is_empty() {
            String::new()
        } else {
            format!(" (leased: {})", task.leases.join(", "))
        };
        lines.push(format!(
            "  scopes     {}{leases}",
            task.task.path_scopes.join(", ")
        ));
    }
    if let Some(parent) = task.task.parent_id {
        lines.push(format!("  parent     #{parent}"));
    }
    if !task.task.dependencies.is_empty() {
        lines.push(format!(
            "  depends on {}",
            task.task
                .dependencies
                .iter()
                .map(|id| format!("#{id}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !task.dependents.is_empty() {
        lines.push(format!(
            "  blocks     {}",
            task.dependents
                .iter()
                .map(|id| format!("#{id}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if let Some(claim) = task.task.claim_expires_ms {
        lines.push(format!("  claim ends {}", iso(claim)));
    }
    if !task.task.brief.is_empty() {
        lines.push(String::new());
        lines.push("Brief:".into());
        for line in task.task.brief.split('\n') {
            lines.push(format!("  {line}"));
        }
    }
    if !task.task.acceptance.is_empty() {
        lines.push(String::new());
        lines.push("Acceptance:".into());
        for line in task.task.acceptance.split('\n') {
            lines.push(format!("  {line}"));
        }
    }
    if let Some(result) = &task.task.result {
        lines.push(String::new());
        lines.push(format!("Result: {}", result.summary));
        if !result.changed_files.is_empty() {
            lines.push(format!("  files: {}", result.changed_files.join(", ")));
        }
    }
    if let Some(review) = &task.task.review {
        lines.push(String::new());
        lines.push(format!(
            "Review by {}: {} - {}",
            review.reviewer,
            if review.accepted {
                "accepted"
            } else {
                "changes requested"
            },
            review.feedback
        ));
    }
    if !task.notes.is_empty() {
        lines.push(String::new());
        lines.push("Notes:".into());
        for note in &task.notes {
            lines.push(format!(
                "  {} {}: {}",
                iso(note.ts_ms),
                note.author,
                note.body
            ));
        }
    }
    if !task.messages.is_empty() {
        lines.push(String::new());
        lines.push("Thread:".into());
        for message in &task.messages {
            lines.push(format!(
                "  #{} {} -> {}: {}",
                message.seq,
                message.sender,
                message.recipient.clone().unwrap_or_else(|| "*".into()),
                clip(
                    if message.subject.is_empty() {
                        &message.body
                    } else {
                        &message.subject
                    },
                    100
                )
            ));
        }
    }
    lines.join("\n")
}

pub fn render_event(event: &BusEvent) -> String {
    let data = if event
        .data
        .as_object()
        .map(|o| !o.is_empty())
        .unwrap_or(false)
    {
        format!(" {}", clip(&event.data.to_string(), 140))
    } else {
        String::new()
    };
    let source = if event.source == "v2" {
        String::new()
    } else {
        format!(" ({})", event.source)
    };
    format!(
        "{} {} {} {} {}:{}{}{}",
        event.seq,
        iso(event.ts_ms),
        event.actor,
        event.kind,
        event.entity,
        event.entity_id,
        source,
        data
    )
}

pub fn render_status(status: &StatusResult) -> String {
    let online = status
        .agents
        .iter()
        .filter(|agent| {
            agent
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("offline")
                != "offline"
        })
        .count();
    let counts = status
        .counts
        .iter()
        .map(|(state, n)| format!("{state} {n}"))
        .collect::<Vec<_>>()
        .join(", ");
    let agents: Vec<(Agent, i64)> = status
        .agents
        .iter()
        .filter_map(|value| {
            let agent: Agent = serde_json::from_value({
                let mut v = value.clone();
                v.as_object_mut().map(|o| o.remove("unread"));
                v
            })
            .ok()?;
            let unread = value.get("unread").and_then(|v| v.as_i64()).unwrap_or(0);
            Some((agent, unread))
        })
        .collect();
    format!(
        "bus {}  seq {}  agents {} ({} online)  tasks: {}\n\nAGENTS\n{}\n\nOPEN TASKS\n{}",
        status.db_path,
        status.seq,
        agents.len(),
        online,
        if counts.is_empty() {
            "none".into()
        } else {
            counts
        },
        render_agents(&agents),
        render_tasks(&status.open_tasks)
    )
}
