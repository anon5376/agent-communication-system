//! The read model behind `aos`: one snapshot of the bus, computed on every change.
//!
//! Everything here comes from the bus. Where ACS records nothing (token use, cost,
//! memory), the frame says so instead of inventing a value.

use crate::bus::{Bus, ListTasksInput};
use crate::error::Result;
use crate::types::{BusEvent, Message, Task, OPERATOR_ID};
use std::collections::{BTreeMap, BTreeSet};

/// The terminal contract's state vocabulary. Marker and word always travel together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum St {
    Running,
    Waiting,
    Blocked,
    Failed,
    Complete,
    Verified,
    Conflict,
    Gate,
    Unknown,
    Disconnected,
    Unavailable,
}

impl St {
    pub fn marker(self) -> char {
        match self {
            St::Running => '*',
            St::Waiting => '.',
            St::Blocked | St::Conflict => '!',
            St::Failed => 'x',
            St::Complete | St::Verified => '+',
            St::Gate | St::Unknown => '?',
            St::Disconnected => '~',
            St::Unavailable => '-',
        }
    }
    pub fn word(self) -> &'static str {
        match self {
            St::Running => "RUNNING",
            St::Waiting => "WAITING",
            St::Blocked => "BLOCKED",
            St::Failed => "FAILED",
            St::Complete => "COMPLETE",
            St::Verified => "VERIFIED",
            St::Conflict => "CONFLICT",
            St::Gate => "GATE",
            St::Unknown => "UNKNOWN",
            St::Disconnected => "DISCONNECTED",
            St::Unavailable => "UNAVAILABLE",
        }
    }
    pub fn label(self) -> String {
        format!("{} {}", self.marker(), self.word())
    }
    /// Lower sorts first: intervention, then work in progress, then quiet.
    fn rank(self) -> u8 {
        match self {
            St::Blocked | St::Conflict | St::Failed => 0,
            St::Gate => 1,
            St::Running => 2,
            St::Waiting => 3,
            St::Unknown => 4,
            St::Complete | St::Verified => 5,
            St::Disconnected | St::Unavailable => 6,
        }
    }
}

/// How a task state reads in the contract vocabulary.
pub fn task_state(t: &Task, stalled: bool) -> (St, String) {
    let who = t.assignee.clone().unwrap_or_else(|| "nobody".into());
    match t.state.as_str() {
        "open" => (
            St::Waiting,
            format!(
                "open / {}",
                if t.assignee.is_some() {
                    format!("for {who}")
                } else {
                    "unassigned".into()
                }
            ),
        ),
        "blocked" => (
            St::Blocked,
            format!("waits on {}", deps_text(&t.dependencies)),
        ),
        "claimed" if stalled => (St::Blocked, format!("stalled / {who}")),
        "claimed" => (St::Running, who),
        "submitted" => (St::Gate, format!("review by {}", reviewer_of(t))),
        "changes_requested" => (
            St::Waiting,
            format!("changes requested / round {}", t.round),
        ),
        "accepted" => (
            St::Verified,
            match &t.review {
                Some(r) => format!("accepted by {}", r.reviewer),
                None => "accepted".into(),
            },
        ),
        "failed" => (St::Failed, "failed".into()),
        "cancelled" => (St::Failed, "cancelled".into()),
        other => (St::Unknown, other.to_string()),
    }
}

fn deps_text(deps: &[i64]) -> String {
    if deps.is_empty() {
        "dependencies".into()
    } else {
        deps.iter()
            .map(|d| format!("#{d}"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

pub fn reviewer_of(t: &Task) -> String {
    t.reviewer.clone().unwrap_or_else(|| t.creator.clone())
}

#[derive(Debug, Clone)]
pub struct AgentView {
    pub id: String,
    pub role: String,
    pub harness: String,
    pub model: String,
    pub parent: Option<String>,
    pub authority: String,
    pub stored_status: String,
    pub last_seen_ms: Option<i64>,
    pub unread: i64,
    pub st: St,
    pub phrase: String,
    pub task: Option<(i64, String)>,
    pub accepted: usize,
    pub depth: usize,
    /// For drawing the spine: is this the last child at each ancestor level.
    pub lasts: Vec<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateKind {
    /// A submitted task whose reviewer is the operator.
    Review,
    /// A claimed task with no claim or note activity for the stall window.
    Stalled,
}

#[derive(Debug, Clone)]
pub struct Gate {
    pub kind: GateKind,
    pub task: Task,
    pub dependents: Vec<i64>,
}

#[derive(Debug, Clone)]
pub struct TaskNode {
    pub task: Task,
    pub depth: usize,
    pub lasts: Vec<bool>,
    pub stalled: bool,
}

pub struct Frame {
    pub db_path: String,
    pub seq: i64,
    pub now: i64,
    pub stall_ms: i64,
    pub agents: Vec<AgentView>,
    pub gates: Vec<Gate>,
    /// Open tasks plus recently closed ones, in tree order.
    pub tree: Vec<TaskNode>,
    pub goal: Option<Task>,
    pub open_tasks: usize,
    /// Tasks carrying a submitted result, newest first.
    pub results: Vec<Task>,
    pub events: Vec<BusEvent>,
    pub mail: Vec<Message>,
    pub operator_unread: i64,
    /// The aos crew around the bus: filled in by the app, empty in tests and --print of a bare bus.
    pub crew: super::crew::CrewInfo,
}

const CLOSED_RECENT: i64 = 40;
const EVENT_WINDOW: i64 = 400;

impl Frame {
    pub fn load(bus: &Bus, stall_ms: i64) -> Result<Frame> {
        let now = bus.now();
        let seq = bus.latest_seq()?;
        let open = bus.list_tasks(ListTasksInput {
            mine: None,
            states: None,
            include_closed: false,
            limit: Some(1000),
        })?;
        let mut closed_ids: Vec<i64> = Vec::new();
        {
            let mut stmt = bus.conn.prepare_cached(
                "SELECT id FROM tasks WHERE state IN ('accepted','failed','cancelled') ORDER BY updated_ms DESC LIMIT ?",
            )?;
            let rows = stmt.query_map([CLOSED_RECENT], |row| row.get::<_, i64>(0))?;
            for row in rows {
                closed_ids.push(row?);
            }
        }
        let mut tasks: Vec<Task> = open.clone();
        for id in closed_ids {
            tasks.push(bus.get_task(id)?.task);
        }
        tasks.sort_by_key(|t| t.id);
        // A lead waiting on its team is not stuck: a claim counts as stalled only
        // when nothing open under it has moved inside the window either.
        let cutoff = now - stall_ms.max(0);
        let busy_under = |id: i64| -> bool {
            let mut stack = vec![id];
            let mut seen = 0;
            while let Some(p) = stack.pop() {
                seen += 1;
                if seen > 1000 {
                    break;
                }
                for c in open.iter().filter(|t| t.parent_id == Some(p)) {
                    if c.updated_ms >= cutoff {
                        return true;
                    }
                    stack.push(c.id);
                }
            }
            false
        };
        let stalled: BTreeSet<i64> = bus
            .stalled_tasks(stall_ms)?
            .into_iter()
            .filter(|t| !busy_under(t.id))
            .map(|t| t.id)
            .collect();

        // Dependents, for impact lines.
        let mut dependents: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
        for t in &tasks {
            for d in &t.dependencies {
                dependents.entry(*d).or_default().push(t.id);
            }
        }

        let mut gates: Vec<Gate> = Vec::new();
        for t in &open {
            if t.state == "submitted" && reviewer_of(t) == OPERATOR_ID {
                gates.push(Gate {
                    kind: GateKind::Review,
                    task: t.clone(),
                    dependents: dependents.get(&t.id).cloned().unwrap_or_default(),
                });
            }
        }
        for t in &open {
            if stalled.contains(&t.id) {
                gates.push(Gate {
                    kind: GateKind::Stalled,
                    task: t.clone(),
                    dependents: dependents.get(&t.id).cloned().unwrap_or_default(),
                });
            }
        }

        let agents = agent_tree(bus, &tasks, &stalled, now)?;
        let tree = task_tree(&tasks, &stalled);
        let goal = pick_goal(&open);
        let mut results: Vec<Task> = tasks
            .iter()
            .filter(|t| t.result.is_some())
            .cloned()
            .collect();
        results.sort_by_key(|t| std::cmp::Reverse(t.updated_ms));
        let events = bus.events((seq - EVENT_WINDOW).max(0), EVENT_WINDOW)?;
        let operator_unread = bus.unread_count(OPERATOR_ID).unwrap_or(0);
        let mut mail: Vec<Message> = bus
            .get_messages(None, Some(200), None, None)?
            .into_iter()
            .filter(|m| {
                m.recipient.as_deref() == Some(OPERATOR_ID)
                    || (m.recipient.is_none() && m.sender != OPERATOR_ID)
            })
            .collect();
        let keep = mail.len().saturating_sub(20);
        mail.drain(..keep);
        Ok(Frame {
            db_path: bus.db_path.display().to_string(),
            seq,
            now,
            stall_ms,
            agents,
            gates,
            tree,
            goal,
            open_tasks: open.len(),
            results,
            events,
            mail,
            operator_unread,
            crew: Default::default(),
        })
    }

    pub fn running(&self) -> usize {
        self.agents.iter().filter(|a| a.st == St::Running).count()
    }

    pub fn reviews(&self) -> usize {
        self.gates
            .iter()
            .filter(|g| g.kind == GateKind::Review)
            .count()
    }

    pub fn stalled(&self) -> usize {
        self.gates
            .iter()
            .filter(|g| g.kind == GateKind::Stalled)
            .count()
    }

    /// The run-level state the rail shows.
    pub fn overall(&self) -> St {
        if self.reviews() > 0 {
            St::Gate
        } else if self.stalled() > 0 {
            St::Blocked
        } else if self.running() > 0 {
            St::Running
        } else {
            St::Waiting
        }
    }

    pub fn last_event_for_task(&self, id: i64) -> Option<&BusEvent> {
        let key = id.to_string();
        self.events
            .iter()
            .rev()
            .find(|e| e.entity == "task" && e.entity_id == key)
    }

    pub fn last_event_by(&self, actor: &str) -> Option<&BusEvent> {
        self.events.iter().rev().find(|e| e.actor == actor)
    }
}

fn agent_tree(
    bus: &Bus,
    tasks: &[Task],
    stalled: &BTreeSet<i64>,
    now: i64,
) -> Result<Vec<AgentView>> {
    let mut flat: Vec<AgentView> = Vec::new();
    for (a, unread) in bus.list_agents()? {
        if a.id == OPERATOR_ID {
            continue;
        }
        let claimed = tasks
            .iter()
            .find(|t| t.state == "claimed" && t.assignee.as_deref() == Some(a.id.as_str()));
        let awaiting = tasks
            .iter()
            .find(|t| t.state == "submitted" && t.assignee.as_deref() == Some(a.id.as_str()));
        let accepted = tasks
            .iter()
            .filter(|t| t.state == "accepted" && t.assignee.as_deref() == Some(a.id.as_str()))
            .count();
        let (st, phrase, task) = if let Some(t) = claimed {
            if stalled.contains(&t.id) {
                (
                    St::Blocked,
                    format!("stalled {} / #{}", span(now - t.updated_ms), t.id),
                    Some((t.id, t.title.clone())),
                )
            } else {
                (
                    St::Running,
                    format!("{} / {}", blank_dash(&a.harness), blank_dash(&a.model)),
                    Some((t.id, t.title.clone())),
                )
            }
        } else if let Some(t) = awaiting {
            (
                St::Waiting,
                format!("review of #{} by {}", t.id, reviewer_of(t)),
                Some((t.id, t.title.clone())),
            )
        } else {
            match a.status.as_str() {
                "offline" => (
                    St::Disconnected,
                    format!("last seen {}", age(a.last_seen_ms, now)),
                    None,
                ),
                "waiting" => (St::Waiting, "waiting for work".into(), None),
                "working" => (
                    St::Running,
                    format!("{} / {}", blank_dash(&a.harness), blank_dash(&a.model)),
                    None,
                ),
                _ => (St::Waiting, "idle".into(), None),
            }
        };
        flat.push(AgentView {
            id: a.id.clone(),
            role: a.role.clone(),
            harness: a.harness.clone(),
            model: a.model.clone(),
            parent: a.parent_id.clone().filter(|p| p != OPERATOR_ID),
            authority: a.authority.clone().unwrap_or_else(|| "worker".into()),
            stored_status: a.stored_status.clone(),
            last_seen_ms: a.last_seen_ms,
            unread,
            st,
            phrase,
            task,
            accepted,
            depth: 0,
            lasts: Vec::new(),
        });
    }
    let ids: BTreeSet<String> = flat.iter().map(|a| a.id.clone()).collect();
    let mut children: BTreeMap<Option<String>, Vec<usize>> = BTreeMap::new();
    for (i, a) in flat.iter().enumerate() {
        let parent = a.parent.clone().filter(|p| ids.contains(p) && p != &a.id);
        children.entry(parent).or_default().push(i);
    }
    for list in children.values_mut() {
        list.sort_by(|&x, &y| {
            flat[x]
                .st
                .rank()
                .cmp(&flat[y].st.rank())
                .then(flat[x].id.cmp(&flat[y].id))
        });
    }
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    fn walk(
        key: Option<String>,
        depth: usize,
        lasts: Vec<bool>,
        flat: &[AgentView],
        children: &BTreeMap<Option<String>, Vec<usize>>,
        seen: &mut BTreeSet<usize>,
        out: &mut Vec<AgentView>,
    ) {
        let Some(list) = children.get(&key) else {
            return;
        };
        for (n, &i) in list.iter().enumerate() {
            if !seen.insert(i) {
                continue;
            }
            let mut l = lasts.clone();
            l.push(n + 1 == list.len());
            let mut a = flat[i].clone();
            a.depth = depth;
            a.lasts = l.clone();
            out.push(a);
            walk(
                Some(flat[i].id.clone()),
                depth + 1,
                l,
                flat,
                children,
                seen,
                out,
            );
        }
    }
    walk(None, 0, Vec::new(), &flat, &children, &mut seen, &mut out);
    // Parent cycles leave agents unreached; show them at the root rather than drop them.
    for (i, a) in flat.iter().enumerate() {
        if !seen.contains(&i) {
            let mut a = a.clone();
            a.lasts = vec![true];
            out.push(a);
        }
    }
    Ok(out)
}

fn task_tree(tasks: &[Task], stalled: &BTreeSet<i64>) -> Vec<TaskNode> {
    let ids: BTreeSet<i64> = tasks.iter().map(|t| t.id).collect();
    let mut children: BTreeMap<Option<i64>, Vec<usize>> = BTreeMap::new();
    for (i, t) in tasks.iter().enumerate() {
        let parent = t.parent_id.filter(|p| ids.contains(p) && *p != t.id);
        children.entry(parent).or_default().push(i);
    }
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    #[allow(clippy::too_many_arguments)]
    fn walk(
        key: Option<i64>,
        depth: usize,
        lasts: Vec<bool>,
        tasks: &[Task],
        stalled: &BTreeSet<i64>,
        children: &BTreeMap<Option<i64>, Vec<usize>>,
        seen: &mut BTreeSet<usize>,
        out: &mut Vec<TaskNode>,
    ) {
        let Some(list) = children.get(&key) else {
            return;
        };
        for (n, &i) in list.iter().enumerate() {
            if !seen.insert(i) {
                continue;
            }
            let mut l = lasts.clone();
            l.push(n + 1 == list.len());
            out.push(TaskNode {
                task: tasks[i].clone(),
                depth,
                lasts: l.clone(),
                stalled: stalled.contains(&tasks[i].id),
            });
            walk(
                Some(tasks[i].id),
                depth + 1,
                l,
                tasks,
                stalled,
                children,
                seen,
                out,
            );
        }
    }
    walk(
        None,
        0,
        Vec::new(),
        tasks,
        stalled,
        &children,
        &mut seen,
        &mut out,
    );
    for (i, t) in tasks.iter().enumerate() {
        if !seen.contains(&i) {
            out.push(TaskNode {
                task: t.clone(),
                depth: 0,
                lasts: vec![true],
                stalled: stalled.contains(&t.id),
            });
        }
    }
    out
}

/// ACS has no mission object. The goal shown is the open top-level task with the
/// most open subtasks; ties go to the oldest.
fn pick_goal(open: &[Task]) -> Option<Task> {
    let roots: Vec<&Task> = open.iter().filter(|t| t.parent_id.is_none()).collect();
    roots
        .iter()
        .max_by(|a, b| {
            let ca = open.iter().filter(|t| t.parent_id == Some(a.id)).count();
            let cb = open.iter().filter(|t| t.parent_id == Some(b.id)).count();
            ca.cmp(&cb).then(b.id.cmp(&a.id))
        })
        .map(|t| (*t).clone())
}

fn blank_dash(s: &str) -> &str {
    if s.is_empty() {
        "-"
    } else {
        s
    }
}

pub fn age(ms: Option<i64>, now: i64) -> String {
    let Some(ms) = ms else { return "never".into() };
    let s = ((now - ms) / 1000).max(0);
    if s < 60 {
        format!("{s}s ago")
    } else if s < 3600 {
        format!("{}m ago", s / 60)
    } else if s < 48 * 3600 {
        format!("{}h ago", s / 3600)
    } else {
        format!("{}d ago", s / 86400)
    }
}

/// Minutes or hours without the "ago", for durations.
pub fn span(ms: i64) -> String {
    let s = (ms / 1000).max(0);
    if s < 3600 {
        format!("{}m", s / 60)
    } else {
        format!("{}h{:02}m", s / 3600, (s / 60) % 60)
    }
}

pub fn clock(ms: i64) -> String {
    let s = (ms / 1000).rem_euclid(86400);
    format!("{:02}:{:02}:{:02}Z", s / 3600, (s / 60) % 60, s % 60)
}
