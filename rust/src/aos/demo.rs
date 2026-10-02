//! `aos demo`: a sample bus with agents and tasks in every state the screens
//! draw, written through the ordinary bus API so it is a real bus.db.

use crate::bus::{Bus, CreateTaskInput, SendInput, SubmitInput};
use crate::error::Result;
use crate::types::OPERATOR_ID;
use serde_json::json;
use std::path::Path;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// Seed `path` unless it already holds agents. Returns true when it seeded.
pub fn seed(path: &Path) -> Result<bool> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| crate::error::BusError::invalid(format!("{}: {e}", dir.display())))?;
    }
    let now = now_ms();
    {
        let probe = Bus::open(Some(path))?;
        if probe
            .list_agents()?
            .iter()
            .any(|(a, _)| a.id != OPERATOR_ID)
        {
            return Ok(false);
        }
    }

    // Three hours ago: the team and the plan.
    let early = Bus::with_clock(Some(path), move || now - 3 * 3_600_000)?;
    early.init()?;
    let op = early.identify(Some(OPERATOR_ID))?;
    // id, role, model, harness, parent, authority
    #[allow(clippy::type_complexity)]
    let team: [(&str, &str, &str, &str, Option<&str>, &str); 5] = [
        (
            "lead",
            "manager",
            "claude-opus-4",
            "claude",
            None,
            "manager",
        ),
        (
            "impl-a",
            "worker",
            "gpt-5-codex",
            "codex",
            Some("lead"),
            "worker",
        ),
        (
            "impl-b",
            "worker",
            "claude-sonnet-4",
            "claude",
            Some("lead"),
            "worker",
        ),
        (
            "rev-1",
            "reviewer",
            "gemini-2.5-pro",
            "gemini",
            Some("lead"),
            "worker",
        ),
        (
            "scout",
            "researcher",
            "qwen3-coder",
            "opencode",
            Some("lead"),
            "worker",
        ),
    ];
    for (id, role, model, harness, parent, authority) in team {
        early.add_agent(
            &op,
            id,
            Some(role),
            Some(model),
            Some(harness),
            parent,
            Some(authority),
        )?;
    }
    let task = |title: &str,
                to: Option<&str>,
                reviewer: Option<&str>,
                parent: Option<i64>,
                deps: Vec<i64>,
                brief: &str| CreateTaskInput {
        title: title.into(),
        brief: Some(brief.into()),
        to: to.map(String::from),
        reviewer: reviewer.map(String::from),
        parent_id: parent,
        dependencies: deps,
        ..Default::default()
    };
    let root = early.create_task(
        &op,
        task(
            "ship budget enforcement for runs",
            Some("lead"),
            None,
            None,
            vec![],
            "Runs stop at a token or usd cap, and the operator can see spend.",
        ),
    )?;
    let prior = early.create_task(
        &op,
        task(
            "research budget prior art",
            Some("scout"),
            Some("rev-1"),
            Some(root.id),
            vec![],
            "How other harnesses cap spend per run and per task.",
        ),
    )?;
    let cap = early.create_task(
        &op,
        task(
            "add per-task token cap in core",
            Some("impl-a"),
            Some(OPERATOR_ID),
            Some(root.id),
            vec![],
            "A claim fails once the task's token budget is spent.",
        ),
    )?;
    let guard = early.create_task(
        &op,
        task(
            "loop guard: stop the same tool call 5x",
            Some("impl-b"),
            None,
            Some(root.id),
            vec![],
            "Detect five identical tool calls in a row and pause the agent.",
        ),
    )?;
    let pause = early.create_task(
        &op,
        task(
            "pause and resume from the cli",
            Some("impl-a"),
            Some("rev-1"),
            Some(root.id),
            vec![],
            "qagent pause and qagent resume keep claims and leases.",
        ),
    )?;
    early.create_task(
        &op,
        task(
            "cost on the status screen",
            None,
            None,
            Some(root.id),
            vec![cap.id],
            "Show spend next to stuck and needs-you.",
        ),
    )?;
    let usd = early.create_task(
        &op,
        task(
            "hard usd cap per run",
            Some("impl-b"),
            Some(OPERATOR_ID),
            Some(root.id),
            vec![],
            "A run with a usd budget stops at the cap.",
        ),
    )?;
    let scout = early.identify(Some("scout"))?;
    early.claim_task(&scout, Some(prior.id))?;
    early.submit_task(&scout, prior.id, SubmitInput {
        summary: "Three harnesses cap per run; none caps per task; two treat unknown cost as zero.".into(),
        details: Some("Sources linked in the task notes.".into()),
        validation: Some(json!([{ "command": "link-check notes.md", "passed": true, "summary": "9 of 9 links resolve" }])),
        ..Default::default()
    })?;
    let rev = early.identify(Some("rev-1"))?;
    early.review_task(
        &rev,
        prior.id,
        true,
        "Sources check out; the unknown-cost finding matters for #7.",
    )?;
    drop(early);

    // Fifty minutes ago: impl-b claims the loop guard and goes quiet.
    let mid = Bus::with_clock(Some(path), move || now - 50 * 60_000)?;
    let impl_b = mid.identify(Some("impl-b"))?;
    mid.claim_task(&impl_b, Some(guard.id))?;
    mid.note_task(
        &impl_b,
        guard.id,
        "repeated call detection fires on retries too; trying a hash of name and arguments",
    )?;
    drop(mid);

    // Now.
    let bus = Bus::open(Some(path))?;
    let lead = bus.identify(Some("lead"))?;
    bus.claim_task(&lead, Some(root.id))?;
    let impl_a = bus.identify(Some("impl-a"))?;
    bus.claim_task(&impl_a, Some(cap.id))?;
    bus.submit_task(&impl_a, cap.id, SubmitInput {
        summary: "Per-task token cap enforced; a claim over budget fails with budget_exceeded.".into(),
        details: Some("Cap lives on the task row; the TS schema gets the same column in a migration.".into()),
        changed_files: vec!["src/core/bus.ts".into(), "src/core/db.ts".into(), "tests/budget.test.ts".into()],
        validation: Some(json!([
            { "command": "npm run test:unit", "passed": true, "summary": "212 passed" },
            { "command": "node scripts/v2-interop-smoke.mjs", "passed": true, "summary": "both implementations agree" }
        ])),
        ..Default::default()
    })?;
    bus.claim_task(&impl_a, Some(pause.id))?;
    bus.submit_task(
        &impl_a,
        pause.id,
        SubmitInput {
            summary: "qagent pause and resume added.".into(),
            changed_files: vec!["src/cli/main.ts".into()],
            ..Default::default()
        },
    )?;
    let rev = bus.identify(Some("rev-1"))?;
    bus.review_task(
        &rev,
        pause.id,
        false,
        "Resume does not restore the claim lease; a paused task can be stolen.",
    )?;
    let impl_b = bus.identify(Some("impl-b"))?;
    bus.claim_task(&impl_b, Some(usd.id))?;
    bus.submit_task(&impl_b, usd.id, SubmitInput {
        summary: "Runs stop at the usd cap; agents with unknown cost are refused when a cap is set.".into(),
        changed_files: vec!["src/core/bus.ts".into(), "rust/src/bus.rs".into()],
        validation: Some(json!([
            { "command": "cargo test budget", "passed": true, "summary": "6 passed" },
            { "command": "node scripts/v2-interop-smoke.mjs", "passed": false, "summary": "usd cap column missing from the TS schema" }
        ])),
        ..Default::default()
    })?;
    bus.send(&impl_b, SendInput {
        to: OPERATOR_ID.into(),
        subject: Some("unknown cost under a usd cap".into()),
        body: "Codex and Hermes report no per-turn cost. Should a run with a usd cap refuse them, or record cost as unknown and continue?".into(),
        msg_type: Some("question".into()),
        thread: None,
        task_id: Some(usd.id),
        refs: None,
        requires_ack: false,
    })?;
    Ok(true)
}
