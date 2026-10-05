//! Delegation and claim limits enforced by the bus core — the Rust side of
//! tests/reliability.test.ts, so both implementations refuse the same calls on one bus.db.

use acs::bus::{Bus, CreateTaskInput, ListTasksInput};
use acs::identity::{self, AgentPolicy};
use acs::types::OPERATOR_ID;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

struct Fixture {
    home: PathBuf,
    bus: Bus,
    op: identity::Identity,
}

fn fixture() -> Fixture {
    let home = std::env::temp_dir().join(format!(
        "acs-rust-reliability-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&home).unwrap();
    let bus = Bus::open(Some(&home.join("bus.db"))).unwrap();
    bus.init().unwrap();
    let op = bus.identify(Some(OPERATOR_ID)).unwrap();
    Fixture { home, bus, op }
}

impl Fixture {
    fn add(&self, id: &str, role: &str, authority: &str) -> identity::Identity {
        self.bus
            .add_agent(&self.op, id, Some(role), None, None, None, Some(authority))
            .unwrap();
        self.bus.identify(Some(id)).unwrap()
    }

    fn task(&self, actor: &identity::Identity, title: &str, to: Option<&str>, parent: Option<i64>) -> acs::error::Result<acs::types::Task> {
        self.bus.create_task(
            actor,
            CreateTaskInput {
                title: title.to_string(),
                to: to.map(str::to_string),
                parent_id: parent,
                ..Default::default()
            },
        )
    }

    fn qagent(&self, args: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_qagent"))
            .arg("--db")
            .arg(self.home.join("bus.db"))
            .args(args)
            .output()
            .unwrap()
    }
}

fn message(result: acs::error::Result<acs::types::Task>) -> String {
    result.map(|t| format!("created #{}", t.id)).unwrap_or_else(|e| e.message)
}

#[test]
fn a_worker_cannot_delegate_through_the_core_but_may_file_work_for_itself() {
    let f = fixture();
    let worker = f.add("w1", "worker", "worker");
    f.add("w2", "worker", "worker");
    assert!(message(f.task(&worker, "for w2", Some("w2"), None)).contains("w1 may not delegate"));
    assert!(message(f.task(&worker, "for anyone", None, None)).contains("w1 may not delegate"));
    assert_eq!(
        f.task(&worker, "my own follow-up", Some("w1"), None).unwrap().assignee.as_deref(),
        Some("w1")
    );
    let out = f.qagent(&["--as", "w1", "task", "add", "via cli", "--to", "w2"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("may not delegate"));
}

#[test]
fn a_policy_narrows_a_manager_children_and_depth_and_survives_rotation() {
    let f = fixture();
    let lead = f.add("lead", "manager", "manager");
    f.add("a", "worker", "worker");
    f.add("b", "worker", "worker");
    assert_eq!(f.task(&lead, "before any policy", Some("a"), None).unwrap().assignee.as_deref(), Some("a"));

    let no_delegation = AgentPolicy { can_delegate: Some(false), ..Default::default() };
    f.bus.set_agent_policy(&lead, "lead", Some(&no_delegation)).unwrap();
    assert!(message(f.task(&lead, "now forbidden", Some("a"), None)).contains("lead may not delegate"));
    let widen = AgentPolicy { can_delegate: Some(true), ..Default::default() };
    let refused = f.bus.set_agent_policy(&lead, "lead", Some(&widen)).unwrap_err();
    assert!(refused.message.contains("may only narrow"), "{}", refused.message);

    let policy = AgentPolicy {
        can_delegate: Some(true),
        allowed_child_agent_ids: Some(vec!["a".into()]),
        max_delegation_depth: Some(1),
        ..Default::default()
    };
    f.bus.set_agent_policy(&f.op, "lead", Some(&policy)).unwrap();
    assert_eq!(f.task(&lead, "to a", Some("a"), None).unwrap().assignee.as_deref(), Some("a"));
    assert!(message(f.task(&lead, "to b", Some("b"), None)).contains("may only assign work to a, not b"));
    let root = f.task(&f.op, "goal", None, None).unwrap();
    let child = f.task(&lead, "one level down", Some("a"), Some(root.id)).unwrap();
    assert!(message(f.task(&lead, "two levels down", Some("a"), Some(child.id))).contains("at most 1 level"));

    f.bus.rotate_token(&f.op, "lead").unwrap();
    let rotated = f.bus.identify(Some("lead")).unwrap();
    assert!(message(f.task(&rotated, "after rotation", Some("b"), None)).contains("not b"));
}

#[test]
fn the_policy_is_read_inside_the_creating_transaction() {
    let f = fixture();
    let lead = f.add("lead", "manager", "manager");
    f.add("a", "worker", "worker");
    let no_delegation = AgentPolicy { can_delegate: Some(false), ..Default::default() };
    f.bus.set_agent_policy(&f.op, "lead", Some(&no_delegation)).unwrap();
    // `lead` was resolved before the policy changed; the core must still refuse.
    assert!(lead.permissions.can_delegate);
    assert!(message(f.task(&lead, "stale identity", Some("a"), None)).contains("may not delegate"));
}

#[test]
fn max_concurrent_tasks_is_enforced_at_claim_time_also_against_concurrent_claimers() {
    let f = fixture();
    let w = f.add("w1", "worker", "worker");
    for index in 0..8 {
        f.task(&f.op, &format!("t{index}"), Some("w1"), None).unwrap();
    }
    let limit = AgentPolicy { max_concurrent_tasks: Some(2), ..Default::default() };
    f.bus.set_agent_policy(&f.op, "w1", Some(&limit)).unwrap();
    f.bus.claim_task(&w, None).unwrap();
    f.bus.claim_task(&w, None).unwrap();
    let refused = f.bus.claim_task(&w, None).unwrap_err();
    assert!(refused.message.contains("already holds 2 claimed task"), "{}", refused.message);
    assert!(!f.bus.has_claimable("w1", "worker").unwrap(), "no backlog is offered at the limit");

    let claimed = |bus: &Bus| {
        bus.list_tasks(ListTasksInput {
            states: Some(vec!["claimed".into()]),
            ..Default::default()
        })
        .unwrap()
    };
    for task in claimed(&f.bus) {
        f.bus.release_task(&w, task.id, None).unwrap();
    }
    let db = f.home.join("bus.db");
    let claimers: Vec<_> = (0..8)
        .map(|_| {
            Command::new(env!("CARGO_BIN_EXE_qagent"))
                .arg("--db")
                .arg(&db)
                .args(["--as", "w1", "task", "claim"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap()
        })
        .collect();
    let wins = claimers
        .into_iter()
        .map(|mut child| child.wait().unwrap().success())
        .filter(|ok| *ok)
        .count();
    assert_eq!(wins, 2);
    assert_eq!(claimed(&f.bus).len(), 2);
}

#[test]
fn whoami_shows_the_policy_limits() {
    let f = fixture();
    f.add("w1", "worker", "worker");
    let limit = AgentPolicy { max_concurrent_tasks: Some(3), ..Default::default() };
    f.bus.set_agent_policy(&f.op, "w1", Some(&limit)).unwrap();
    let me = f.bus.identify(Some("w1")).unwrap();
    assert!(!me.permissions.can_delegate);
    assert_eq!(me.permissions.max_concurrent_tasks, Some(3));
    let json = serde_json::to_value(&me).unwrap();
    assert_eq!(json["permissions"]["maxConcurrentTasks"], 3);
}
