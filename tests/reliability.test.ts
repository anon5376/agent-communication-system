import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { Bus } from "../src/core/bus.js";
import type { Identity } from "../src/core/identity.js";
import { acquireSupervisorLock, supervise } from "../src/supervisor.js";
import { testConfig } from "./helpers.js";

const QAGENT = fileURLToPath(new URL("../src/qagent.js", import.meta.url));
const SUPERVISOR_MODULE = new URL("../src/supervisor.js", import.meta.url).href;

interface Fixture {
  home: string;
  dbPath: string;
  project: string;
  bus: Bus;
  op: Identity;
  add: (id: string, role: string, authority?: "worker" | "manager") => Identity;
  lines: string[];
}

function fixture(t: { after: (fn: () => void | Promise<void>) => void }): Fixture {
  const home = realpathSync(mkdtempSync(join(tmpdir(), "qagent-reliability-")));
  const dbPath = join(home, "bus.db");
  const project = join(home, "project");
  mkdirSync(project);
  const bus = Bus.open({ dbPath });
  bus.init();
  const op = bus.identify("operator");
  const add = (id: string, role: string, authority: "worker" | "manager" = "worker") => {
    bus.addAgent(op, { id, role, harness: "fake", authority });
    return bus.identify(id);
  };
  t.after(() => { bus.close(); rmSync(home, { recursive: true, force: true }); });
  return { home, dbPath, project, bus, op, add, lines: [] };
}

function gitInit(dir: string): void {
  for (const args of [["init", "-q"], ["add", "."], ["commit", "-q", "-m", "init"]]) {
    if (args[0] === "add") writeFileSync(join(dir, "README"), "x\n");
    const result = spawnSync("git", ["-c", "user.email=test@example.com", "-c", "user.name=test", ...args], { cwd: dir, encoding: "utf8" });
    assert.equal(result.status, 0, result.stderr);
  }
}

/** A harness without bus tools: the supervisor claims for it and submits what it prints (its working directory). */
function probeConfig(mutate: (config: ReturnType<typeof testConfig>) => void = () => {}) {
  const config = testConfig();
  const probe = "process.stdout.write(JSON.stringify({result: 'cwd=' + process.cwd(), usage: {inputTokens: 5, outputTokens: 5, totalTokens: 10, costUSD: 0}}) + '\\n')";
  config.harnesses.fake.adapter = "command";
  config.harnesses.fake.command = process.execPath;
  config.harnesses.fake.features.mcp = false;
  config.agents["fake-small"].harnessOptions = { args: ["-e", probe] };
  mutate(config);
  return config;
}

function run(f: Fixture, config: ReturnType<typeof testConfig>, waitMs = 300): { stop: () => Promise<void>; done: Promise<void> } {
  const controller = new AbortController();
  const done = supervise({ agentId: "fake-small", workdir: f.project, dbPath: f.dbPath, config, signal: controller.signal, waitMs, log: (line) => { f.lines.push(line); } });
  done.catch(() => { /* asserted by the caller when it matters */ });
  return { done, stop: async () => { controller.abort(); await done.catch(() => {}); } };
}

async function until<T>(label: string, timeoutMs: number, probe: () => T | null | undefined | false, context = () => ""): Promise<T> {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const value = probe();
    if (value) return value;
    if (Date.now() > deadline) assert.fail(`timed out waiting for ${label}\n${context()}`);
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
}

const pause = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

// ------------------------------------------------------------- delegation

test("a worker cannot delegate through the core, whatever the caller; it may still file work for itself", (t) => {
  const f = fixture(t);
  const worker = f.add("w1", "worker");
  f.add("w2", "worker");
  assert.throws(() => f.bus.createTask(worker, { title: "for w2", to: "w2" }), /w1 may not delegate/);
  assert.throws(() => f.bus.createTask(worker, { title: "for anyone" }), /w1 may not delegate/);
  assert.equal(f.bus.createTask(worker, { title: "my own follow-up", to: "w1" }).assignee, "w1");
  const cli = spawnSync(process.execPath, [QAGENT, "--as", "w1", "task", "add", "via cli", "--to", "w2"], {
    env: { ...process.env, QAGENT_BUS_DB: f.dbPath, QAGENT_HOME: f.home, AGENT_BUS_HOME: f.home }, encoding: "utf8",
  });
  assert.notEqual(cli.status, 0);
  assert.match(cli.stderr, /may not delegate/);
});

test("a configuration policy narrows a manager in the core: no delegation, only named children, bounded depth", (t) => {
  const f = fixture(t);
  const lead = f.add("lead", "manager", "manager");
  f.add("a", "worker");
  f.add("b", "worker");
  assert.equal(f.bus.createTask(lead, { title: "allowed before any policy", to: "a" }).assignee, "a");

  f.bus.setAgentPolicy(lead, "lead", { canDelegate: false });
  assert.throws(() => f.bus.createTask(lead, { title: "now forbidden", to: "a" }), /lead may not delegate/);
  assert.throws(() => f.bus.setAgentPolicy(lead, "lead", { canDelegate: true }), /may only narrow/, "an agent cannot widen its own policy");

  f.bus.setAgentPolicy(f.op, "lead", { canDelegate: true, allowedChildAgentIds: ["a"], maxDelegationDepth: 1 });
  assert.equal(f.bus.createTask(lead, { title: "to a", to: "a" }).assignee, "a");
  assert.throws(() => f.bus.createTask(lead, { title: "to b", to: "b" }), /may only assign work to a, not b/);
  const root = f.bus.createTask(f.op, { title: "goal" });
  const child = f.bus.createTask(lead, { title: "one level down", to: "a", parentId: root.id });
  assert.throws(() => f.bus.createTask(lead, { title: "two levels down", to: "a", parentId: child.id }), /at most 1 level/);

  f.bus.rotateToken(f.op, "lead");
  assert.throws(() => f.bus.createTask(f.bus.identify("lead"), { title: "after rotation", to: "b" }), /not b/, "a token rotation keeps the policy");
});

test("the policy is read inside the creating transaction, not from the caller's cached identity", (t) => {
  const f = fixture(t);
  const lead = f.add("lead", "manager", "manager");
  f.add("a", "worker");
  // `lead` was resolved before the policy changed; the core must still refuse.
  f.bus.setAgentPolicy(f.op, "lead", { canDelegate: false });
  assert.equal(lead.permissions.canDelegate, true);
  assert.throws(() => f.bus.createTask(lead, { title: "stale identity", to: "a" }), /may not delegate/);
});

// ------------------------------------------------------------ claim limit

test("maxConcurrentTasks is enforced at claim time, also against concurrent claimers", async (t) => {
  const f = fixture(t);
  const w = f.add("w1", "worker");
  for (let index = 0; index < 8; index += 1) f.bus.createTask(f.op, { title: `t${index}`, to: "w1" });
  f.bus.setAgentPolicy(f.op, "w1", { maxConcurrentTasks: 2 });
  f.bus.claimTask(w, null);
  f.bus.claimTask(w, null);
  assert.throws(() => f.bus.claimTask(w, null), /already holds 2 claimed task/);
  assert.deepEqual(f.bus.claimableTasks("w1"), [], "no backlog is offered at the limit");

  // Release both, then let eight processes race for claims as w1.
  for (const task of f.bus.listTasks({ states: ["claimed"] })) f.bus.releaseTask(w, task.id);
  const env = { ...process.env, QAGENT_BUS_DB: f.dbPath, QAGENT_HOME: f.home, AGENT_BUS_HOME: f.home };
  const results = await Promise.all(Array.from({ length: 8 }, () => new Promise<number | null>((resolve) => {
    const child = spawn(process.execPath, [QAGENT, "--as", "w1", "task", "claim"], { env, stdio: "ignore" });
    child.on("close", (code) => resolve(code));
  })));
  assert.equal(results.filter((code) => code === 0).length, 2, `exit codes ${results.join(",")}`);
  assert.equal(f.bus.listTasks({ states: ["claimed"] }).length, 2);
});

test("the supervisor applies the configuration's limits to the bus at start", { timeout: 20_000 }, async (t) => {
  const f = fixture(t);
  f.add("fake-small", "cheap-worker");
  f.add("other", "worker");
  const supervisor = run(f, probeConfig((config) => { config.constraints.maxConcurrentTasks = 3; }));
  t.after(supervisor.stop);
  await until("the supervisor to start waiting", 10_000, () => f.bus.getAgent("fake-small")?.storedStatus === "waiting", () => f.lines.join("\n"));
  const me = f.bus.identify("fake-small");
  assert.equal(me.permissions.canDelegate, false);
  assert.equal(me.permissions.maxConcurrentTasks, 3);
  await supervisor.stop();
});

// --------------------------------------------------------------- backlog

test("a supervisor started after work was queued picks it up without fresh mail", { timeout: 20_000 }, async (t) => {
  const f = fixture(t);
  f.add("fake-small", "cheap-worker");
  const queued = f.bus.createTask(f.op, { title: "queued before the supervisor started", role: "cheap-worker" });
  const supervisor = run(f, probeConfig(), 60_000);
  t.after(supervisor.stop);
  const task = await until("the queued task to be submitted", 10_000, () => {
    const current = f.bus.getTask(queued.id);
    return current.state === "submitted" ? current : null;
  }, () => f.lines.join("\n"));
  assert.equal(task.assignee, "fake-small");
  await supervisor.stop();
});

test("work that becomes claimable without an event for this agent is picked up after the wait times out", { timeout: 20_000 }, async (t) => {
  const f = fixture(t);
  f.add("fake-small", "cheap-worker");
  const holder = f.add("w2", "worker");
  const blocking = f.bus.createTask(f.op, { title: "holds the lease", to: "w2", project: f.project, pathScopes: ["src"] });
  f.bus.claimTask(holder, blocking.id);
  const supervisor = run(f, probeConfig(), 300);
  t.after(supervisor.stop);
  await until("the supervisor to start waiting", 10_000, () => f.bus.getAgent("fake-small")?.storedStatus === "waiting", () => f.lines.join("\n"));
  const waiting = f.bus.createTask(f.op, { title: "same scope", role: "cheap-worker", project: f.project, pathScopes: ["src/parser"] });
  await pause(700);
  assert.equal(f.bus.getTask(waiting.id).state, "open", "blocked by w2's lease");
  // w2 gives its task back: the event concerns w2's task, not fake-small, so no mail or task event wakes it.
  f.bus.releaseTask(holder, blocking.id);
  await until("the freed task to be submitted", 10_000, () => f.bus.getTask(waiting.id).state === "submitted", () => f.lines.join("\n"));
  await supervisor.stop();
});

// ------------------------------------------------------- worktree isolation

test("requested worktree isolation fails closed: no turn runs in the shared checkout and the claim is given back", { timeout: 20_000 }, async (t) => {
  const f = fixture(t);
  f.add("fake-small", "cheap-worker");
  const supervisor = run(f, probeConfig((config) => { config.constraints.isolation = "worktree"; }));
  t.after(supervisor.stop);
  await until("the supervisor to start waiting", 10_000, () => f.bus.getAgent("fake-small")?.storedStatus === "waiting", () => f.lines.join("\n"));
  // The project is not a git repository, so no worktree can exist.
  const task = f.bus.createTask(f.op, { title: "edit in isolation", to: "fake-small", project: f.project });
  const noted = await until("the refusal note", 10_000, () => {
    const current = f.bus.getTask(task.id);
    return current.notes.some((note) => /worktree isolation unavailable/.test(note.body)) ? current : null;
  }, () => f.lines.join("\n"));
  await pause(500);
  const current = f.bus.getTask(task.id);
  assert.equal(current.state, "open", "the claim was released");
  assert.equal(current.result, null, "no turn ran");
  assert.ok(!f.lines.some((line) => /turn complete/.test(line)), f.lines.join("\n"));
  assert.ok(noted);
  await supervisor.stop();
});

test("with worktree isolation, two queued tasks run one per turn, each in its own worktree", { timeout: 30_000 }, async (t) => {
  const f = fixture(t);
  gitInit(f.project);
  f.add("fake-small", "cheap-worker");
  const first = f.bus.createTask(f.op, { title: "first", to: "fake-small", project: f.project });
  const second = f.bus.createTask(f.op, { title: "second", to: "fake-small", project: f.project });
  const supervisor = run(f, probeConfig((config) => { config.constraints.isolation = "worktree"; }));
  t.after(supervisor.stop);
  await until("both tasks to be submitted", 20_000, () => [first, second].every((task) => f.bus.getTask(task.id).state === "submitted"), () => f.lines.join("\n"));
  const dirs = [first, second].map((task) => realpathSync(String(f.bus.getTask(task.id).result?.summary).replace(/^cwd=/, "")));
  assert.notEqual(dirs[0], dirs[1]);
  for (const dir of dirs) assert.notEqual(dir, realpathSync(f.project), "never the shared checkout");
  await supervisor.stop();
});

test("with worktree isolation, mail about a task the turn does not hold is kept for a later turn", { timeout: 30_000 }, async (t) => {
  const f = fixture(t);
  gitInit(f.project);
  f.add("fake-small", "cheap-worker");
  const prompts = join(f.home, "prompts.log");
  const first = f.bus.createTask(f.op, { title: "first", to: "fake-small", project: f.project });
  const second = f.bus.createTask(f.op, { title: "second", to: "fake-small", project: f.project });
  f.bus.send(f.op, { to: "fake-small", type: "question", taskId: second.id, subject: "about the second task", body: "SENTINEL-SECOND" });
  const record = `require('fs').appendFileSync(${JSON.stringify(prompts)}, process.argv[1] + '\\n----\\n'); process.stdout.write(JSON.stringify({result: 'cwd=' + process.cwd(), usage: {inputTokens: 5, outputTokens: 5, totalTokens: 10, costUSD: 0}}) + '\\n')`;
  const supervisor = run(f, probeConfig((config) => {
    config.constraints.isolation = "worktree";
    config.agents["fake-small"].harnessOptions = { args: ["-e", record, "{prompt}"] };
  }));
  t.after(supervisor.stop);
  await until("both tasks to be submitted", 20_000, () => [first, second].every((task) => f.bus.getTask(task.id).state === "submitted"), () => f.lines.join("\n"));
  const turns = readFileSync(prompts, "utf8").split("\n----\n").filter(Boolean);
  const shown = turns.filter((prompt) => prompt.includes("SENTINEL-SECOND"));
  assert.equal(shown.length, 1, `the message about #${second.id} reaches exactly one turn\n${turns.join("\n====\n")}`);
  assert.match(shown[0], new RegExp(`#${second.id}\\b`));
  await supervisor.stop();
});

test("after a worktree refusal, mail that names no task still reaches a later turn", { timeout: 20_000 }, async (t) => {
  const f = fixture(t);
  f.add("fake-small", "cheap-worker");
  const prompts = join(f.home, "prompts.log");
  // The project is not a git repository, so the task is refused; the plain message must survive that.
  f.bus.createTask(f.op, { title: "edit in isolation", to: "fake-small", project: f.project });
  f.bus.send(f.op, { to: "fake-small", type: "question", subject: "plain note", body: "SENTINEL-PLAIN" });
  const record = `require('fs').appendFileSync(${JSON.stringify(prompts)}, process.argv[1] + '\\n----\\n'); process.stdout.write(JSON.stringify({result: 'ok', usage: {inputTokens: 5, outputTokens: 5, totalTokens: 10, costUSD: 0}}) + '\\n')`;
  const supervisor = run(f, probeConfig((config) => {
    config.constraints.isolation = "worktree";
    config.agents["fake-small"].harnessOptions = { args: ["-e", record, "{prompt}"] };
  }));
  t.after(supervisor.stop);
  await until("a turn that shows the plain message", 10_000, () => existsSync(prompts) && readFileSync(prompts, "utf8").includes("SENTINEL-PLAIN"), () => f.lines.join("\n"));
  await supervisor.stop();
});

test("an unchanged task the agent leaves alone is not offered again at every wait timeout", { timeout: 20_000 }, async (t) => {
  const f = fixture(t);
  f.add("fake-small", "cheap-worker");
  // The agent has bus tools, so the supervisor offers the task and leaves claiming to the CLI, which ignores it.
  const supervisor = run(f, probeConfig((config) => { config.harnesses.fake.features.mcp = true; }), 200);
  t.after(supervisor.stop);
  f.bus.createTask(f.op, { title: "left alone", role: "cheap-worker" });
  await until("the first offer", 10_000, () => f.lines.some((line) => /turn complete/.test(line)), () => f.lines.join("\n"));
  await pause(2_000);
  const turns = f.lines.filter((line) => /turn complete/.test(line)).length;
  assert.equal(turns, 1, f.lines.join("\n"));
  await supervisor.stop();
});

test("worktree isolation refuses to start for an agent that pins one CLI session", async (t) => {
  const f = fixture(t);
  f.add("fake-small", "cheap-worker");
  const supervisor = run(f, probeConfig((config) => {
    config.constraints.isolation = "worktree";
    config.agents["fake-small"].resumeSessionId = "pinned-session";
  }));
  t.after(supervisor.stop);
  const outcome = await Promise.race([supervisor.done.then(() => "returned", (error: Error) => error.message), pause(3_000).then(() => "still running")]);
  assert.match(outcome, /cannot run fake-small: it pins session pinned-session/);
});

// ------------------------------------------------------------ ownership

test("supervisor ownership is atomic: of many simultaneous starters exactly one wins", { skip: process.platform === "win32", timeout: 30_000 }, async (t) => {
  const home = mkdtempSync(join(tmpdir(), "qagent-lock-"));
  t.after(() => rmSync(home, { recursive: true, force: true }));
  const dir = join(home, "supervisors");
  const startAt = Date.now() + 1_500;
  const script = `
    const { acquireSupervisorLock } = await import(${JSON.stringify(SUPERVISOR_MODULE)});
    while (Date.now() < ${startAt}) {}
    try { acquireSupervisorLock(${JSON.stringify(dir)}, "w1"); process.stdout.write("won"); await new Promise((r) => setTimeout(r, 1500)); }
    catch (error) { process.stdout.write("lost: " + error.message); }
  `;
  const outputs = await Promise.all(Array.from({ length: 12 }, () => new Promise<string>((resolve) => {
    // The argv mentions supervise and w1, as a real supervisor's does.
    const child = spawn(process.execPath, ["--input-type=module", "-e", script, "supervise", "w1"], { stdio: ["ignore", "pipe", "inherit"] });
    let out = "";
    child.stdout.setEncoding("utf8").on("data", (chunk: string) => { out += chunk; });
    child.on("close", () => resolve(out));
  })));
  assert.equal(outputs.filter((out) => out === "won").length, 1, outputs.join("\n"));
  assert.ok(outputs.filter((out) => out !== "won").every((out) => /already running|keeps starting/.test(out)), outputs.join("\n"));
});

test("a stale pid file is taken over, and the lock is released on return", (t) => {
  const home = mkdtempSync(join(tmpdir(), "qagent-lock-"));
  t.after(() => rmSync(home, { recursive: true, force: true }));
  const dir = join(home, "supervisors");
  mkdirSync(dir);
  const dead = spawnSync(process.execPath, ["-e", "process.stdout.write(String(process.pid))"], { encoding: "utf8" });
  writeFileSync(join(dir, "w1.pid"), `${dead.stdout}\n`);
  const release = acquireSupervisorLock(dir, "w1");
  assert.equal(readFileSync(join(dir, "w1.pid"), "utf8").trim(), String(process.pid));
  release();
  assert.equal(existsSync(join(dir, "w1.pid")), false);
});

// --------------------------------------------------------------- budgets

test("the configuration's token budget stops new turns once the reported usage reaches it", { timeout: 20_000 }, async (t) => {
  const f = fixture(t);
  f.add("fake-small", "cheap-worker");
  const supervisor = run(f, probeConfig((config) => { config.constraints.optionalTokenBudget = 10; }));
  t.after(supervisor.stop);
  const first = f.bus.createTask(f.op, { title: "first", to: "fake-small" });
  await until("the first task to be submitted", 10_000, () => f.bus.getTask(first.id).state === "submitted", () => f.lines.join("\n"));
  const second = f.bus.createTask(f.op, { title: "second", to: "fake-small" });
  await until("the budget notice", 10_000, () => f.lines.some((line) => /budget reached \(10 of 10 reported tokens used\)/.test(line)), () => f.lines.join("\n"));
  await pause(500);
  assert.equal(f.bus.getTask(second.id).state, "open");
  await supervisor.stop();
});

test("a dollar budget on a CLI that reports no usage is refused instead of silently uncounted", async (t) => {
  const f = fixture(t);
  f.add("fake-small", "cheap-worker");
  const supervisor = run(f, probeConfig((config) => {
    config.constraints.optionalApiCostBudgetUSD = 5;
    config.harnesses.fake.features.usageReporting = false;
  }));
  t.after(supervisor.stop);
  const outcome = await Promise.race([supervisor.done.then(() => "returned", (error: Error) => error.message), pause(3_000).then(() => "still running")]);
  assert.match(outcome, /reports no usage, so the budget could never be counted/);
});

test("a message or open task that lands just as a wait starts wakes the waiter instead of waiting out the timeout", async (t) => {
  const f = fixture(t);
  const worker = f.add("fake-small", "cheap-worker");
  const other = Bus.open({ dbPath: f.dbPath });
  t.after(() => other.close());
  const operator = other.identify("operator");
  // Land the write between waitForMail's empty inbox check and the event that marks the
  // start of the wait, the window a concurrent sender can hit.
  const internals = f.bus as unknown as { write: <T>(fn: () => T) => T };
  const write = internals.write.bind(f.bus);
  let inject: (() => void) | null = null;
  internals.write = <T>(fn: () => T): T => { const pending = inject; inject = null; pending?.(); return write(fn); };

  inject = () => { other.send(operator, { to: "fake-small", type: "question", subject: "racing mail", body: "x" }); };
  let started = Date.now();
  const mail = await f.bus.waitForMail(worker, { timeoutMs: 3000 });
  assert.equal(mail.status, "mail");
  assert.ok(Date.now() - started < 2000, `woke after ${Date.now() - started} ms`);
  f.bus.inbox(worker, { limit: 50 });

  inject = () => { other.createTask(operator, { title: "racing task", role: "cheap-worker" }); };
  started = Date.now();
  const task = await f.bus.waitForMail(worker, { timeoutMs: 3000 });
  assert.equal(task.status, "task");
  assert.ok(Date.now() - started < 2000, `woke after ${Date.now() - started} ms`);
});
