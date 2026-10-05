import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { Bus } from "../src/core/bus.js";
import { storeNewToken, tokenPathFor, writePrivateToken } from "../src/core/identity.js";
import { supervise } from "../src/supervisor.js";
import { testConfig } from "./helpers.js";

function fixture(t: { after: (fn: () => void) => void }) {
  const home = mkdtempSync(join(tmpdir(), "qagent-policy-"));
  const bus = Bus.open({ dbPath: join(home, "bus.db") });
  bus.init();
  const op = bus.identify("operator");
  bus.addAgent(op, { id: "lead", role: "manager", authority: "manager" });
  bus.addAgent(op, { id: "child", role: "worker" });
  bus.addAgent(op, { id: "other", role: "worker" });
  const lead = bus.identify("lead");
  t.after(() => { bus.close(); rmSync(home, { recursive: true, force: true }); });
  const store = (id: string, policy: Record<string, unknown>) => {
    const value = JSON.stringify({ canDelegate: id === "lead", canReview: true, policy: { ...policy, by: "operator", updatedMs: 123 } });
    bus.db.prepare("UPDATE identities SET permissions_json = ? WHERE agent_id = ?").run(value, id);
    return value;
  };
  return { home, bus, op, lead, store };
}

async function waitFor<T>(probe: () => T | undefined, timeoutMs = 5000): Promise<T> {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const result = probe();
    if (result !== undefined) return result;
    if (Date.now() >= deadline) assert.fail("timed out waiting for condition");
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
}

test("Rust-style delegation policy is enforced even with a previously resolved identity", (t) => {
  const f = fixture(t);
  f.store("lead", { canDelegate: false });
  assert.equal(f.bus.identify("lead").permissions.canDelegate, false);
  assert.throws(() => f.bus.createTask(f.lead, { title: "unassigned" }), /may not delegate/);
  assert.throws(() => f.bus.createTask(f.lead, { title: "other agent", to: "child" }), /may not delegate/);
  assert.equal(f.bus.createTask(f.lead, { title: "self", to: "lead" }).assignee, "lead");
});

test("Rust-style child and depth limits constrain TypeScript delegation", (t) => {
  const f = fixture(t);
  f.store("lead", { allowedChildAgentIds: ["child"], maxDelegationDepth: 1 });
  const parent = f.bus.createTask(f.op, { title: "parent" });
  const child = f.bus.createTask(f.lead, { title: "allowed", to: "child", parentId: parent.id });
  assert.throws(() => f.bus.createTask(f.lead, { title: "not allowed", to: "other" }), /may only assign/);
  assert.throws(() => f.bus.createTask(f.lead, { title: "too deep", to: "child", parentId: child.id }), /at most 1 level/);
});

test("TypeScript rotation and re-registration preserve the entire Rust policy JSON", (t) => {
  const f = fixture(t);
  const stored = f.store("lead", { canDelegate: false, maxConcurrentTasks: 1 });
  for (const rotate of [
    () => f.bus.rotateToken(f.op, "lead"),
    () => {
      const token = storeNewToken(f.bus.db, "lead", "manager", Date.now());
      writePrivateToken(f.home, tokenPathFor(f.home, "lead"), token);
    },
  ]) {
    rotate();
    const row = f.bus.db.prepare("SELECT permissions_json FROM identities WHERE agent_id = 'lead'").get()!;
    assert.equal(row.permissions_json, stored);
    assert.equal(f.bus.identify("lead").permissions.canDelegate, false);
  }
});

test("stored Rust claim limits hold across concurrent TypeScript processes", { timeout: 10_000 }, async (t) => {
  const f = fixture(t);
  f.store("child", { maxConcurrentTasks: 1 });
  for (let i = 0; i < 5; i++) f.bus.createTask(f.op, { title: `task ${i}`, to: "child" });
  const qagent = fileURLToPath(new URL("../src/qagent.js", import.meta.url));
  const results = await Promise.all(Array.from({ length: 5 }, () => new Promise<number | null>((resolve, reject) => {
    const child = spawn(process.execPath, [qagent, "--db", f.bus.dbPath, "--as", "child", "task", "claim"], { stdio: "ignore" });
    child.once("error", reject);
    child.once("close", resolve);
  })));
  assert.equal(results.filter((code) => code === 0).length, 1, `exit codes: ${results}`);
  assert.equal(f.bus.listTasks({ states: ["claimed"] }).length, 1);
  const worker = f.bus.identify("child");
  assert.throws(() => f.bus.claimTask(worker, null), /already holds 1/);
  f.bus.releaseTask(worker, f.bus.listTasks({ states: ["claimed"] })[0].id);
  assert.equal(f.bus.claimTask(worker, null).state, "claimed");
});

test("TypeScript supervisor stops when mixed configuration limits cannot be applied", { timeout: 5000 }, async (t) => {
  const f = fixture(t);
  f.store("lead", { maxConcurrentTasks: 1 });
  rmSync(join(f.home, "operator.token"));
  const config = testConfig();
  config.agents.lead = { ...structuredClone(config.agents["fake-small"]), id: "lead", enabled: true, authority: "manager" };
  config.agents.lead.permissions.canDelegate = false;
  config.constraints.maxConcurrentTasks = 3;
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), 1000);
  t.after(() => clearTimeout(timer));
  await assert.rejects(supervise({
    agentId: "lead", workdir: f.home, dbPath: f.bus.dbPath, config,
    signal: controller.signal, waitMs: 100, log: () => {},
  }), /may only narrow its own policy/);
  assert.equal(f.bus.listTasks({ states: ["claimed"] }).length, 0);
});

test("TypeScript supervisor waits at its concurrent task limit and resumes after capacity is released", { timeout: 10_000 }, async (t) => {
  const f = fixture(t);
  f.store("lead", { maxConcurrentTasks: 1 });
  const holding = f.bus.createTask(f.op, { title: "holding task", to: "lead" });
  f.bus.claimTask(f.lead, holding.id);
  f.bus.inbox(f.lead);

  const config = testConfig();
  config.agents.lead = { ...structuredClone(config.agents["fake-small"]), id: "lead", enabled: true, authority: "manager" };
  config.harnesses.fake.features.mcp = false;
  config.constraints.maxConcurrentTasks = 1;

  const controller = new AbortController();
  let completion: { error: unknown | null } | undefined;
  let running: Promise<{ error: unknown | null }> | undefined;
  t.after(async () => {
    controller.abort();
    if (running) await running;
  });

  const logs: string[] = [];
  running = supervise({
    agentId: "lead", workdir: f.home, dbPath: f.bus.dbPath, config,
    signal: controller.signal, waitMs: 50, log: (message) => logs.push(message),
  }).then(
    () => ({ error: null }),
    (error: unknown) => ({ error }),
  );
  void running.then((result) => { completion = result; });

  await waitFor(() => {
    if (completion) assert.fail(`supervisor stopped before waiting: ${String(completion.error)}`);
    return f.bus.getAgent("lead")?.status === "waiting" ? true : undefined;
  });
  const available = f.bus.createTask(f.op, { title: "available task", role: "manager" });
  assert.equal(f.bus.inbox(f.lead, { peek: true }).messages.length, 0);

  try {
    await waitFor(() => {
      if (completion) assert.fail(`supervisor stopped at capacity: ${String(completion.error)}; logs=${logs.join(" | ")}`);
      return logs.some((line) => line.includes("at concurrent task limit; waiting for capacity")) ? true : undefined;
    });
  } catch (error) {
    assert.fail(`${String(error)}; logs=${logs.join(" | ")}; agent=${JSON.stringify(f.bus.getAgent("lead"))}`);
  }
  assert.equal(completion, undefined);
  assert.equal(f.bus.getTask(available.id).state, "open");

  f.bus.submitTask(f.lead, holding.id, { summary: "capacity released" });
  f.bus.createTask(f.op, { title: "resume trigger", role: "manager" });
  const resumed = await waitFor(() => {
    if (completion) assert.fail(`supervisor stopped after capacity was released: ${String(completion.error)}`);
    const current = f.bus.getTask(available.id);
    return current.state === "submitted" ? current : undefined;
  });
  assert.equal(resumed.assignee, "lead");
  assert.equal(completion, undefined);

  controller.abort();
  const result = await running;
  assert.equal(result.error, null);
});

test("TypeScript supervisor retries a claim after its own submit frees capacity on timeout", { timeout: 10_000 }, async (t) => {
  const f = fixture(t);
  f.store("lead", { maxConcurrentTasks: 1 });
  const holding = f.bus.createTask(f.op, { title: "holding task", to: "lead" });
  f.bus.claimTask(f.lead, holding.id);
  f.bus.inbox(f.lead);

  const config = testConfig();
  config.agents.lead = { ...structuredClone(config.agents["fake-small"]), id: "lead", enabled: true, authority: "manager" };
  config.harnesses.fake.features.mcp = false;
  config.constraints.maxConcurrentTasks = 1;

  const controller = new AbortController();
  let completion: { error: unknown | null } | undefined;
  let running: Promise<{ error: unknown | null }> | undefined;
  t.after(async () => {
    controller.abort();
    if (running) await running;
  });

  const logs: string[] = [];
  running = supervise({
    agentId: "lead", workdir: f.home, dbPath: f.bus.dbPath, config,
    signal: controller.signal, waitMs: 50, log: (message) => logs.push(message),
  }).then(
    () => ({ error: null }),
    (error: unknown) => ({ error }),
  );
  void running.then((result) => { completion = result; });

  await waitFor(() => {
    if (completion) assert.fail(`supervisor stopped before waiting: ${String(completion.error)}`);
    return f.bus.getAgent("lead")?.status === "waiting" ? true : undefined;
  });
  const available = f.bus.createTask(f.op, { title: "available after release", role: "manager" });
  assert.equal(f.bus.inbox(f.lead, { peek: true }).messages.length, 0);

  await waitFor(() => {
    if (completion) assert.fail(`supervisor stopped at capacity: ${String(completion.error)}; logs=${logs.join(" | ")}`);
    return logs.some((line) => line.includes("at concurrent task limit; waiting for capacity")) ? true : undefined;
  });
  assert.equal(f.bus.getTask(available.id).state, "open");

  f.bus.submitTask(f.lead, holding.id, { summary: "released by supervisor" });
  const resumed = await waitFor(() => {
    if (completion) assert.fail(`supervisor stopped after capacity was released: ${String(completion.error)}`);
    const current = f.bus.getTask(available.id);
    return current.state === "submitted" ? current : undefined;
  });
  assert.equal(resumed.assignee, "lead");
  assert.equal(completion, undefined);

  controller.abort();
  const result = await running;
  assert.equal(result.error, null);
});

test("only the operator may widen policies, and default workers can only create their own work", (t) => {
  const f = fixture(t);
  const child = f.bus.identify("child");
  assert.throws(() => f.bus.createTask(child, { title: "unassigned" }), /may not delegate/);
  assert.throws(() => f.bus.createTask(child, { title: "for lead", to: "lead" }), /may not delegate/);
  assert.equal(f.bus.createTask(child, { title: "for self", to: "child" }).assignee, "child");
  f.bus.setAgentPolicy(f.op, "lead", { canDelegate: false, maxConcurrentTasks: 1 });
  assert.throws(() => f.bus.setAgentPolicy(f.lead, "lead", { canDelegate: true, maxConcurrentTasks: 3 }), /may only narrow/);
  assert.throws(() => f.bus.setAgentPolicy(child, "lead", null), /only the operator/);
  f.bus.setAgentPolicy(f.op, "lead", { canDelegate: true, maxConcurrentTasks: 3 });
  assert.equal(f.bus.identify("lead").permissions.canDelegate, true);
  f.bus.setAgentPolicy(f.op, "lead", null);
  assert.deepEqual(f.bus.identify("lead").permissions, { canDelegate: true, canReview: true });
});
