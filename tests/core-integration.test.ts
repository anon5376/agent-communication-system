import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { commandPosition } from "../src/cli/main.js";
import { Bus } from "../src/core/bus.js";

const QAGENT = fileURLToPath(new URL("../src/qagent.js", import.meta.url));

function setup(t: { after: (fn: () => void) => void }) {
  const dir = mkdtempSync(join(tmpdir(), "qagent-v2-integration-"));
  const dbPath = join(dir, "bus.db");
  const bus = Bus.open({ dbPath });
  t.after(() => { bus.close(); rmSync(dir, { recursive: true, force: true }); });
  bus.init();
  const operator = bus.identify("operator");
  bus.addAgent(operator, { id: "alice", role: "manager", authority: "manager" });
  bus.addAgent(operator, { id: "bob", role: "worker" });
  return { dir, dbPath, bus, operator, alice: bus.identify("alice"), bob: bus.identify("bob") };
}

test("commandPosition skips the values of global flags placed before the command", () => {
  assert.equal(commandPosition(["--db", "/x/bus.db", "doctor"]), 2);
  assert.equal(commandPosition(["--db=/x/bus.db", "mcp-config"]), 1);
  assert.equal(commandPosition(["--json", "status"]), 1);
  assert.equal(commandPosition(["--as", "bob", "--json"]), -1);
});

test("`qagent --db X doctor` dispatches to doctor with the given database", (t) => {
  const { dbPath } = setup(t);
  const env: NodeJS.ProcessEnv = { ...process.env };
  for (const name of ["QAGENT_BUS_DB", "QAGENT_AGENT_ID", "AGENT_ID"]) delete env[name];
  const result = spawnSync(process.execPath, [QAGENT, "--db", dbPath, "doctor"], { env, encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, new RegExp(`bus ${dbPath.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}`));
  assert.match(result.stdout, /agents .*alice/);
});

test("a stale 'waiting' left by a killed waiter is cleared by the next action or early-returning wait", async (t) => {
  const { bus, alice, bob } = setup(t);
  // Simulate a waiter killed with SIGKILL: status 'waiting' stored, never reset.
  bus.db.prepare("UPDATE agents SET status = 'waiting', wait_until_ms = ? WHERE id = 'bob'").run(Date.now() + 60_000);
  bus.send(alice, { to: "bob", subject: "hi", body: "x" });
  const result = await bus.waitForMail(bob, { timeoutMs: 1000 });
  assert.equal(result.status, "mail");
  assert.equal(bus.getAgent("bob")?.storedStatus, "idle");
  assert.equal(bus.getAgent("bob")?.waitUntilMs, null);

  bus.db.prepare("UPDATE agents SET status = 'waiting', wait_until_ms = ? WHERE id = 'bob'").run(Date.now() + 60_000);
  bus.send(bob, { to: "alice", subject: "back", body: "y" });
  assert.equal(bus.getAgent("bob")?.storedStatus, "idle");
});

test("taskSummaries, setStatus, releaseTask and failTask", (t) => {
  const { bus, alice, bob } = setup(t);
  const task = bus.createTask(alice, { title: "parse", brief: "b", to: "bob", maxRetries: 1 });
  const other = bus.createTask(alice, { title: "other", brief: "b" });
  assert.deepEqual(bus.taskSummaries([other.id, task.id, 999]).map((s) => [s.id, s.state]), [[task.id, "open"], [other.id, "open"]]);

  assert.equal(bus.setStatus(bob, "working").storedStatus, "working");
  bus.claimTask(bob, task.id);
  assert.throws(() => bus.releaseTask(alice, task.id), /forbidden|only/);
  const released = bus.releaseTask(bob, task.id, "not now");
  assert.equal(released.state, "open");
  assert.equal(released.assignee, "bob");

  bus.claimTask(bob, task.id);
  const retried = bus.failTask(bob, task.id, "harness exited 1");
  assert.equal(retried.state, "open");
  assert.equal(retried.attempts, 1);
  assert.ok(bus.inbox(bob, { peek: true }).messages.some((m) => m.subject.startsWith(`[RETRY #${task.id}]`)));

  bus.claimTask(bob, task.id);
  const failed = bus.failTask(bob, task.id, "harness exited 1 again");
  assert.equal(failed.state, "failed");
  assert.ok(bus.inbox(alice, { peek: true }).messages.some((m) => m.subject.startsWith(`[ESCALATE #${task.id}]`)));
  assert.equal(bus.getAgent("bob")?.storedStatus, "idle");
});

test("stalledTasks surfaces idle claims, notes clear the stall, and the operator can requeue", (t) => {
  const { bus, operator, alice, bob } = setup(t);
  const task = bus.createTask(alice, { title: "stuck", brief: "b", to: "bob" });
  bus.claimTask(bob, task.id);
  assert.deepEqual(bus.stalledTasks(60_000), []);

  bus.db.prepare("UPDATE tasks SET updated_ms = ? WHERE id = ?").run(Date.now() - 3_600_000, task.id);
  assert.deepEqual(bus.stalledTasks(1_800_000).map((s) => s.id), [task.id]);

  bus.noteTask(bob, task.id, "still on it");
  assert.deepEqual(bus.stalledTasks(1_800_000), []);

  bus.db.prepare("UPDATE tasks SET updated_ms = ? WHERE id = ?").run(Date.now() - 3_600_000, task.id);
  const requeued = bus.releaseTask(operator, task.id, "auto-requeue: stalled claim");
  assert.equal(requeued.state, "open");
  assert.equal(requeued.assignee, "bob");
  assert.ok(bus.events().some((event) => event.kind === "task_released" && event.entityId === String(task.id)));
});
