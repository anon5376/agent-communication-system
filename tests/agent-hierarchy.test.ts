import assert from "node:assert/strict";
import { ChildProcess, spawn } from "node:child_process";
import { once } from "node:events";
import { mkdirSync, mkdtempSync, openSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { BusConfig } from "../src/config.js";
import { Bus } from "../src/core/bus.js";
import { BusError } from "../src/core/types.js";
import { testConfig } from "./helpers.js";

// Ported from the broker to the v2 core library and the v2 supervisor. Routing previews,
// runs and delegation-depth gates lived in the broker and have no core equivalent; the
// named roster, mailbox threads, parent/child tasks and independent review carry over.

const QAGENT = fileURLToPath(new URL("../src/qagent.js", import.meta.url));
const STOCK_IDS = new Set(["fake-small", "fake-strong", "opus", "gpt", "kimi", "gem"]);
const ROSTER = [
  { id: "lead-alpha", role: "manager", authority: "manager" as const, model: "fake-strong" },
  { id: "hands-bravo", role: "implementation", authority: "worker" as const, model: "fake-small" },
  { id: "crit-charlie", role: "reviewer", authority: "worker" as const, model: "fake-strong" },
  { id: "spare-delta", role: "cheap-worker", authority: "worker" as const, model: "fake-small" },
];

function namedRoster(): BusConfig {
  const config = structuredClone(testConfig());
  for (const agent of Object.values(config.agents)) agent.enabled = false;
  const perms = (canDelegate: boolean, canReview: boolean, depth: number) => ({
    canDelegate, canReview, filesystem: "write" as const, shell: true, network: false, maxDelegationDepth: depth, allowedPaths: ["."],
  });
  for (const row of ROSTER) {
    config.agents[row.id] = {
      id: row.id,
      model: row.model,
      role: row.role,
      authority: row.authority,
      description: `Named ${row.role} for hierarchy tests`,
      enabled: true,
      autoStart: false,
      permissions: perms(row.authority === "manager", row.role === "reviewer" || row.authority === "manager", row.authority === "manager" ? 2 : 0),
    };
  }
  return config;
}

function assertNamed(id: string | null | undefined, label: string): void {
  assert.ok(id, `${label} missing`);
  assert.equal(STOCK_IDS.has(id), false, `${label} resolved to stock id ${id}`);
}

/** One Bus per agent: each is its own SQLite connection, as separate agent processes would be. */
function roster(t: { after: (fn: () => void) => void }) {
  const home = mkdtempSync(join(tmpdir(), "qagent-v2-hierarchy-"));
  const dbPath = join(home, "bus.db");
  const operatorBus = Bus.open({ dbPath });
  operatorBus.init();
  const operator = operatorBus.identify("operator");
  for (const row of ROSTER) operatorBus.addAgent(operator, { id: row.id, role: row.role, authority: row.authority, harness: "fake", model: row.model });
  const buses = new Map<string, Bus>([["operator", operatorBus]]);
  for (const row of ROSTER) buses.set(row.id, Bus.open({ dbPath }));
  t.after(() => {
    for (const bus of buses.values()) bus.close();
    rmSync(home, { recursive: true, force: true });
  });
  const as = (id: string) => ({ bus: buses.get(id)!, me: buses.get(id)!.identify(id) });
  return { home, dbPath, operatorBus, operator, as };
}

test("named roster mailbox threads, parent/child tasks, and independent review on the core bus", { timeout: 20_000 }, async (t) => {
  const r = roster(t);
  const alpha = r.as("lead-alpha");
  const bravo = r.as("hands-bravo");
  const charlie = r.as("crit-charlie");
  const delta = r.as("spare-delta");

  const waitBravo = bravo.bus.waitForMail(bravo.me, { timeoutMs: 4_000 });
  await new Promise((resolve) => setTimeout(resolve, 40));
  const sent = alpha.bus.send(alpha.me, { to: "hands-bravo", type: "question", subject: "thread-coord-1", body: "Please take the scoped implementation; reply on this thread." });
  assert.equal(sent[0].recipient, "hands-bravo");
  const incoming = await waitBravo;
  assert.equal(incoming.status, "mail", "blocking wait must wake on peer mail");
  assert.equal(incoming.messages[0].sender, "lead-alpha");
  assert.equal(incoming.messages[0].recipient, "hands-bravo");
  assert.equal(incoming.messages[0].subject, "thread-coord-1");

  const waitAlpha = alpha.bus.waitForMail(alpha.me, { timeoutMs: 4_000 });
  await new Promise((resolve) => setTimeout(resolve, 40));
  bravo.bus.send(bravo.me, { to: "lead-alpha", type: "answer", subject: "thread-coord-1", body: "Acknowledged." });
  const back = await waitAlpha;
  assert.equal(back.messages[0].sender, "hands-bravo");
  assert.equal(back.messages[0].recipient, "lead-alpha");

  for (const who of [alpha, bravo]) who.bus.inbox(who.me, { limit: 200 });
  const broadcast = r.operatorBus.send(r.operator, { to: "*", subject: "all-hands", body: "Operator ping to every named agent." });
  assert.equal(broadcast.length, 1);
  assert.equal(broadcast[0].recipient, null);
  for (const who of [alpha, bravo, charlie, delta]) {
    const got = who.bus.inbox(who.me, { limit: 200 }).messages;
    assert.deepEqual(got.map((message) => message.subject), ["all-hands"], `${who.me.agentId} broadcast`);
  }
  assert.equal(r.operatorBus.unreadCount("operator"), 0, "the sender does not receive its own broadcast");

  const waitBravoThread = bravo.bus.waitForMail(bravo.me, { timeoutMs: 4_000 });
  const waitCharlieThread = charlie.bus.waitForMail(charlie.me, { timeoutMs: 4_000 });
  await new Promise((resolve) => setTimeout(resolve, 40));
  const targeted = alpha.bus.send(alpha.me, { to: "hands-bravo,crit-charlie", type: "question", subject: "thread-coord-2", body: "Named subset thread." });
  assert.deepEqual(targeted.map((message) => message.recipient).sort(), ["crit-charlie", "hands-bravo"]);
  const [bravoThread, charlieThread] = await Promise.all([waitBravoThread, waitCharlieThread]);
  assert.equal(bravoThread.messages[0].subject, "thread-coord-2");
  assert.equal(charlieThread.messages[0].subject, "thread-coord-2");
  assert.equal(delta.bus.unreadCount("spare-delta"), 0, "a subset thread is not a broadcast");
  for (const who of [bravo, charlie]) {
    who.bus.inbox(who.me, { limit: 200 });
    who.bus.send(who.me, { to: "lead-alpha", type: "answer", subject: "thread-coord-2", body: `${who.me.agentId} is on the thread.` });
  }
  const alphaThread = alpha.bus.inbox(alpha.me, { limit: 200 }).messages;
  assert.deepEqual(new Set(alphaThread.map((message) => message.sender)), new Set(["hands-bravo", "crit-charlie"]));

  const rootTask = r.operatorBus.createTask(r.operator, { title: "Ship a named-roster hierarchy", brief: "Root objective", to: "lead-alpha", role: "manager" });
  assertNamed(rootTask.assignee, "root manager");
  assert.equal(rootTask.assignee, "lead-alpha");
  const childTask = alpha.bus.createTask(alpha.me, {
    title: "Implement scoped change", brief: "Do the work the manager split out.", to: "hands-bravo", reviewer: "crit-charlie",
    role: "implementation", parentId: rootTask.id,
  });
  assertNamed(childTask.assignee, "delegated implementer");
  assert.equal(childTask.assignee, "hands-bravo");
  assert.equal(childTask.creator, "lead-alpha");
  assert.equal(childTask.parentId, rootTask.id);
  assert.equal(childTask.reviewer, "crit-charlie");
  assert.deepEqual(r.operatorBus.listTasks().filter((task) => task.parentId === rootTask.id).map((task) => task.id), [childTask.id]);

  const taskMail = bravo.bus.inbox(bravo.me, { limit: 200 }).messages.find((message) => message.taskId === childTask.id && message.type === "task");
  assert.ok(taskMail, "implementer must receive the delegated TASK on its own thread");
  assert.equal(taskMail.sender, "lead-alpha");
  assert.equal(taskMail.thread, `task-${childTask.id}`);
  assert.throws(() => delta.bus.claimTask(delta.me, childTask.id), (error: unknown) => error instanceof BusError && error.code === "conflict");

  assert.equal(bravo.bus.claimTask(bravo.me, childTask.id).state, "claimed");
  const submitted = bravo.bus.submitTask(bravo.me, childTask.id, { summary: "Named implementer finished the scoped change.", details: "not a stock id" });
  assert.equal(submitted.state, "submitted");
  assert.notEqual(submitted.reviewer, submitted.assignee);
  assert.throws(() => bravo.bus.reviewTask(bravo.me, childTask.id, { accepted: true, feedback: "self-review" }), (error: unknown) => error instanceof BusError && error.code === "forbidden");
  assert.throws(() => alpha.bus.reviewTask(alpha.me, childTask.id, { accepted: true, feedback: "not the named reviewer" }), (error: unknown) => error instanceof BusError && error.code === "forbidden");

  const review = charlie.bus.inbox(charlie.me, { limit: 200 }).messages.find((message) => message.taskId === childTask.id);
  assert.ok(review, "the named reviewer receives the result on the task thread");
  assert.equal(review.recipient, "crit-charlie");
  assert.match(review.subject, /^\[DONE #\d+ r1\]/);

  const accepted = charlie.bus.reviewTask(charlie.me, childTask.id, { accepted: true, feedback: "Independent review passed." });
  assert.equal(accepted.state, "accepted");
  assert.equal(accepted.review?.reviewer, "crit-charlie");
  const passed = bravo.bus.inbox(bravo.me, { limit: 200 }).messages.find((message) => message.subject.startsWith("[ACCEPTED"));
  assert.ok(passed, "the implementer is told the review passed");
  assert.equal(passed.sender, "crit-charlie");
});

test("live v2 supervisors complete a delegated child on a named roster", { timeout: 60_000 }, async (t) => {
  const r = roster(t);
  const project = join(r.home, "project");
  mkdirSync(project);
  const configPath = join(r.home, "config.json");
  writeFileSync(configPath, `${JSON.stringify(namedRoster(), null, 2)}\n`);
  const env: NodeJS.ProcessEnv = { ...process.env, QAGENT_BUS_DB: r.dbPath, QAGENT_HOME: r.home, AGENT_BUS_HOME: r.home };
  for (const name of ["QAGENT_AGENT_ID", "AGENT_ID", "QAGENT_CONFIG", "AGENT_BUS_CONFIG"]) delete env[name];
  const workers: ChildProcess[] = [];
  t.after(() => {
    for (const child of workers) {
      if (!child.pid || child.exitCode !== null) continue;
      try { process.kill(-child.pid, "SIGKILL"); } catch { /* already gone */ }
    }
  });
  const logFor = (id: string) => join(r.home, `supervisor-${id}.log`);
  const logs = () => ["hands-bravo", "crit-charlie"].map((id) => { try { return readFileSync(logFor(id), "utf8"); } catch { return ""; } }).join("\n");
  for (const id of ["hands-bravo", "crit-charlie"]) {
    const log = openSync(logFor(id), "a");
    workers.push(spawn(process.execPath, [QAGENT, "supervise", id, project, "--config", configPath], { env, stdio: ["ignore", log, log], detached: true }));
  }

  const alpha = r.as("lead-alpha");
  const charlie = r.as("crit-charlie");
  const deadline = Date.now() + 15_000;
  while (!["hands-bravo", "crit-charlie"].every((id) => r.operatorBus.getAgent(id)?.storedStatus === "waiting")) {
    assert.ok(Date.now() < deadline, `named supervisors did not start waiting\n${logs()}`);
    await new Promise((resolve) => setTimeout(resolve, 80));
  }

  const rootTask = r.operatorBus.createTask(r.operator, { title: "Live named-roster objective", to: "lead-alpha" });
  const child = alpha.bus.createTask(alpha.me, {
    title: "Live implementation", brief: "Supervisor must pick this TASK off the mailbox and submit.",
    to: "hands-bravo", reviewer: "crit-charlie", role: "implementation", parentId: rootTask.id,
  });

  const submitDeadline = Date.now() + 20_000;
  let submitted = r.operatorBus.getTask(child.id);
  while (submitted.state !== "submitted") {
    assert.ok(Date.now() < submitDeadline, `hands-bravo supervisor did not submit the delegated child\n${logs()}`);
    await new Promise((resolve) => setTimeout(resolve, 100));
    submitted = r.operatorBus.getTask(child.id);
  }
  assert.equal(submitted.assignee, "hands-bravo");
  assert.equal(submitted.reviewer, "crit-charlie");
  assert.match(submitted.result?.summary ?? "", /fake hands-bravo completed/);

  // The reviewer's fake harness has no bus tools, so it cannot review; review as crit-charlie through core.
  const reviewed = charlie.bus.reviewTask(charlie.me, child.id, { accepted: true, feedback: "Reviewed through core as crit-charlie." });
  assert.equal(reviewed.state, "accepted");
  assert.equal(reviewed.review?.reviewer, "crit-charlie");

  for (const worker of workers) {
    const exited = once(worker, "exit");
    worker.kill("SIGTERM");
    const [code] = await exited as [number | null];
    assert.equal(code, 0, logs());
  }
});
