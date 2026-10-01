// Metrics 1-10 of proposals/benchmark.md (AOS), computed from a bus.db, the run record and git. No self-reports.
import { minutes, round, seededShuffle } from "./util.mjs";

const TERMINAL = new Set(["accepted", "failed", "cancelled"]);
// Events that end a claim: the task left `claimed` (submitted, given back, failed, cancelled, or the lease lapsed).
const LEAVES_CLAIM = new Set(["task_submitted", "task_released", "task_retry", "task_failed", "task_cancelled", "claim_expired"]);
// Events after which a task is open and runnable again.
const BECOMES_OPEN = new Set(["task_unblocked", "task_released", "task_retry", "claim_expired"]);

function byTask(events) {
  const map = new Map();
  for (const event of events) {
    if (event.entity !== "task") continue;
    const id = Number(event.entityId);
    if (!map.has(id)) map.set(id, []);
    map.get(id).push(event);
  }
  return map;
}

function sum(values) {
  return values.reduce((total, value) => total + (Number.isFinite(value) ? value : 0), 0);
}

/** Metric 3, claimed part: quiet stretches of at least `thresholdMs` inside each claim by the assignee's own events. */
export function stuckClaims(tasks, grouped, thresholdMs, endMs) {
  const stalls = [];
  for (const task of tasks) {
    let open = null;
    const close = (end) => {
      if (!open) return;
      const points = [...open.activity, end];
      for (let i = 1; i < points.length; i += 1) {
        const gap = points[i] - points[i - 1];
        if (gap >= thresholdMs) stalls.push({ task: task.key ?? `#${task.id}`, startMs: points[i - 1], ms: gap });
      }
      open = null;
    };
    for (const event of grouped.get(task.id) ?? []) {
      if (event.kind === "task_claimed") {
        close(event.tsMs);
        open = { assignee: event.actor, activity: [event.tsMs] };
      } else if (open) {
        if (event.actor === open.assignee) open.activity.push(event.tsMs);
        if (LEAVES_CLAIM.has(event.kind)) close(event.tsMs);
      }
    }
    close(endMs);
  }
  return stalls;
}

/** Metric 3, open part: time tasks sat runnable and unclaimed. An upper bound; the bus does not record whether an agent was idle. */
export function queuedMs(tasks, grouped, endMs) {
  let total = 0;
  for (const task of tasks) {
    let since = null;
    for (const event of grouped.get(task.id) ?? []) {
      if (event.kind === "task_created") since = event.data?.state === "open" ? event.tsMs : null;
      else if (BECOMES_OPEN.has(event.kind)) since = event.tsMs;
      else if (since !== null && (event.kind === "task_claimed" || event.kind === "task_cancelled")) { total += event.tsMs - since; since = null; }
    }
    if (since !== null) total += (TERMINAL.has(task.state) ? task.updatedMs : endMs) - since;
  }
  return total;
}

export function pickAuditSample(acceptedKeys, seed, size) {
  return seededShuffle([...acceptedKeys].sort(), seed).slice(0, size).sort();
}

/**
 * @param input.bus        readBus() result
 * @param input.run        run record (run.json)
 * @param input.sessions   readSessions() result
 * @param input.validators { <key>: { passed } } or null when validators did not run
 * @param input.research   { <key>: scoreResearch() result } or null
 * @param input.integration { outsideScope, mergeConflicts, overlapPairs } or null
 * @param input.grades     { <key>: "correct"|"partly"|"wrong" } human audit grades, or null
 * @param input.touches    logged manual actions (touches.jsonl rows)
 */
export function computeMetrics({ bus, run, sessions, validators, research, integration, grades, touches, thresholds, auditSize, seed }) {
  const { tasks, events } = bus;
  const keyed = tasks.filter((task) => task.key);
  const total = run.taskKeys.length;
  const endMs = run.endedMs ?? events.at(-1)?.tsMs ?? run.startedMs;
  const grouped = byTask(events);
  const family = (agentId) => run.roster.find((agent) => agent.id === agentId)?.family ?? null;
  const roleOf = (agentId) => run.roster.find((agent) => agent.id === agentId)?.role ?? null;

  const accepted = keyed.filter((task) => task.state === "accepted");
  const acceptEvent = (task) => (grouped.get(task.id) ?? []).filter((event) => event.kind === "task_accepted").at(-1);
  const firstRound = accepted.filter((task) => acceptEvent(task)?.data?.round === 1);

  let byOperator = 0;
  let unknownFamily = 0;
  let cross = 0;
  for (const task of accepted) {
    const reviewer = task.review?.reviewer ?? null;
    if (reviewer === "operator") { byOperator += 1; continue; }
    const reviewerFamily = family(reviewer);
    const workerFamily = family(task.assignee);
    if (!reviewerFamily || !workerFamily) unknownFamily += 1;
    else if (reviewerFamily !== workerFamily) cross += 1;
  }

  const stalls = stuckClaims(keyed, grouped, thresholds.stuckMs, endMs);
  const queued = queuedMs(keyed, grouped, endMs);

  const operatorEvents = events.filter((event) => event.actor === "operator" && event.tsMs >= run.startedMs
    && !(event.kind === "task_released" && /^auto-requeue/.test(String(event.data?.reason ?? ""))));

  // Cost: task-level rows if the bus writes them, else the supervisor's per-agent session totals.
  const usageRows = bus.usage.filter((row) => Number.isFinite(row.cost_usd) || Number.isFinite(row.input_tokens));
  const perAgent = {};
  let source = "none";
  if (usageRows.length) {
    source = "usage-table";
    for (const row of usageRows) {
      const entry = (perAgent[row.agent_id] ??= { turns: 0, inputTokens: 0, outputTokens: 0, costUSD: 0 });
      entry.turns += row.turns ?? 0; entry.inputTokens += row.input_tokens ?? 0; entry.outputTokens += row.output_tokens ?? 0; entry.costUSD += row.cost_usd ?? 0;
    }
  } else if (Object.keys(sessions).length) {
    source = "session-files";
    for (const [id, row] of Object.entries(sessions)) perAgent[id] = { turns: row.turns ?? 0, inputTokens: row.inputTokens ?? 0, outputTokens: row.outputTokens ?? 0, costUSD: row.costUSD ?? 0 };
  }
  const agents = Object.entries(perAgent);
  const totalTokens = sum(agents.map(([, row]) => row.inputTokens + row.outputTokens));
  const coordinationTokens = sum(agents.filter(([id]) => ["manager", "reviewer"].includes(roleOf(id))).map(([, row]) => row.inputTokens + row.outputTokens));
  const turns = sum(agents.map(([, row]) => row.turns));

  let defectEscape = null;
  const acceptedImplementation = accepted.filter((task) => task.key.startsWith("I"));
  if (validators) {
    const failed = acceptedImplementation.filter((task) => validators[task.key]?.passed === false);
    defectEscape = { acceptedImplementation: acceptedImplementation.length, failedValidator: failed.map((task) => task.key), rate: acceptedImplementation.length ? failed.length / acceptedImplementation.length : null };
  }

  const sample = pickAuditSample(accepted.map((task) => task.key), seed, auditSize);
  const graded = grades ? sample.filter((key) => grades[key]) : [];
  const agreeing = graded.filter((key) => validators?.[key] && (grades[key] === "correct") === validators[key].passed);
  const auditable = graded.filter((key) => validators?.[key]);

  const perTask = {};
  for (const key of run.taskKeys) {
    const task = keyed.find((candidate) => candidate.key === key);
    perTask[key] = task
      ? { id: task.id, state: task.state, round: task.round, attempts: task.attempts, assignee: task.assignee, reviewer: task.review?.reviewer ?? task.reviewer ?? task.creator, accepted: task.state === "accepted", validatorPassed: validators?.[key]?.passed ?? null }
      : { id: null, state: "not created", round: null, attempts: null, assignee: null, reviewer: null, accepted: false, validatorPassed: null };
  }
  const failedTasks = keyed.filter((task) => task.state === "failed").map((task) => ({ key: task.key, reason: task.review?.feedback ?? null }));

  return {
    schema: 1,
    run: {
      arm: run.arm, series: run.series, mode: run.mode, size: run.size, repetition: run.repetition, baseSha: run.baseSha,
      startedMs: run.startedMs, endedMs: run.endedMs ?? null, wallMinutes: minutes(endMs - run.startedMs),
      endCause: run.endCause ?? null, invalid: run.invalid ?? null, truth: run.truth ?? null,
    },
    tasks: {
      total, accepted: accepted.length, acceptanceRate: round(accepted.length / total),
      firstRoundAccepted: firstRound.length, firstRoundRate: round(firstRound.length / total),
      failed: keyed.filter((task) => task.state === "failed").length,
      cancelled: keyed.filter((task) => task.state === "cancelled").length,
      unfinished: run.taskKeys.filter((key) => !TERMINAL.has(perTask[key].state)),
      overCap: run.capped ?? [],
    },
    crossFamily: { accepted: cross, rate: round(cross / total), acceptedByOperator: byOperator, unknownFamily },
    stuck: {
      minutes: minutes(sum(stalls.map((stall) => stall.ms))),
      longestMinutes: minutes(Math.max(0, ...stalls.map((stall) => stall.ms))),
      stalls: stalls.map((stall) => ({ task: stall.task, minutes: minutes(stall.ms) })),
      queuedMinutes: minutes(queued),
    },
    humanTouches: { count: operatorEvents.length + touches.length, operatorEvents: operatorEvents.length, logged: touches.length },
    cost: { source, granularity: usageRows.length ? "task-or-agent-day" : "agent", usd: round(sum(agents.map(([, row]) => row.costUSD)), 4), tokens: totalTokens, turns, perAgent },
    defectEscape,
    audit: { sample, graded: graded.length, agreement: auditable.length ? round(agreeing.length / auditable.length) : null },
    research,
    coordination: {
      messages: bus.messages, turns,
      messagesPerAccepted: accepted.length ? round(bus.messages / accepted.length, 2) : null,
      turnsPerAccepted: accepted.length ? round(turns / accepted.length, 2) : null,
      coordinationTokenShare: totalTokens ? round(coordinationTokens / totalTokens) : null,
    },
    collisions: integration,
    failedTasks,
    perTask,
  };
}

/** The headline figures of metrics 1-6, one number each, for the cross-run table. */
export function headline(metrics) {
  return {
    acceptanceRate: metrics.tasks.acceptanceRate,
    firstRoundRate: metrics.tasks.firstRoundRate,
    crossFamilyRate: metrics.crossFamily.rate,
    stuckMinutes: metrics.stuck.minutes,
    humanTouches: metrics.humanTouches.count,
    costUsd: metrics.cost.usd,
    defectEscapeRate: metrics.defectEscape?.rate ?? null,
  };
}
