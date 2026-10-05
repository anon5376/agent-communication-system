#!/usr/bin/env node
// v2 interop smoke: the TypeScript and Rust qagent binaries share one bus.db —
// mail, cursors, task claims and `wait` wake-ups must cross the language
// boundary. Run after `npm run build:core` and `cargo build` (rust/).
import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { DatabaseSync } from "node:sqlite";

const QAGENT_TS = resolve("dist/qagent.js");
const QAGENT_RS = resolve("rust/target/debug/qagent");
for (const bin of [[QAGENT_TS, "npm run build:core"], [QAGENT_RS, "cargo build --manifest-path rust/Cargo.toml"]]) {
  if (!existsSync(bin[0])) {
    process.stderr.write(`${bin[0]} is missing; run \`${bin[1]}\` first\n`);
    process.exit(1);
  }
}
const home = mkdtempSync(join(tmpdir(), "qagent-interop-"));
const dbPath = join(home, "bus.db");

function env(agent) {
  const value = { ...process.env, QAGENT_BUS_DB: dbPath };
  for (const name of ["QAGENT_AGENT_ID", "AGENT_ID", "QAGENT_HOME", "AGENT_BUS_HOME", "QAGENT_BLOCK_SEC"]) delete value[name];
  if (agent) value.QAGENT_AGENT_ID = agent;
  return value;
}

function run(impl, agent, args, input) {
  const command = impl === "ts" ? [process.execPath, QAGENT_TS, ...args] : [QAGENT_RS, ...args];
  const result = spawnSync(command[0], command.slice(1), { env: env(agent), encoding: "utf8", input });
  if (result.status !== 0) process.stderr.write(`${impl}$ ${args.join(" ")} -> ${result.status}: ${result.stderr}\n`);
  return result;
}
const ok = (impl, agent, args, input) => {
  const result = run(impl, agent, args, input);
  assert.equal(result.status, 0, result.stderr);
  return result;
};
const jsonOut = (impl, agent, args, input) => JSON.parse(ok(impl, agent, [...args, "--json"], input).stdout);

try {
  // One shared home: operator token + two agents, created by the TS side.
  ok("ts", null, ["init"]);
  ok("ts", null, ["agent", "add", "alpha", "--role", "coder"]);
  ok("ts", null, ["agent", "add", "beta", "--role", "reviewer"]);

  // Both binaries see the same roster.
  for (const impl of ["ts", "rs"]) {
    const agents = jsonOut(impl, null, ["status"]).agents.map((a) => a.id).sort();
    assert.deepEqual(agents, ["alpha", "beta", "operator"], `${impl} status roster`);
  }

  // Mail crosses the boundary both ways, and cursors are shared state.
  ok("ts", "alpha", ["send", "beta", "from ts", "hello over sqlite"]);
  const betaInbox = jsonOut("rs", "beta", ["inbox"]);
  assert.equal(betaInbox.messages[0].sender, "alpha");
  assert.equal(betaInbox.messages[0].subject, "from ts");
  ok("rs", "beta", ["send", "alpha", "from rust", "acknowledged"]);
  const alphaInbox = jsonOut("ts", "alpha", ["inbox"]);
  assert.equal(alphaInbox.messages[0].sender, "beta");
  assert.equal(alphaInbox.messages[0].subject, "from rust");

  // A task created by TS is claimed by Rust and visible back in TS.
  const task = jsonOut("ts", "alpha", ["task", "add", "interop task", "--role", "coder", "--to", "alpha"]);
  const claimed = ok("rs", "alpha", ["task", "claim", String(task.id)]).stdout;
  assert.match(claimed, /claimed|assigned|task #\d+/i);
  const shown = jsonOut("ts", null, ["task", "show", String(task.id)]);
  assert.equal(shown.assignee, "alpha");
  assert.equal(shown.state, "claimed");

  // Rust-compatible stored policy, enforced through both real binaries.
  ok("ts", null, ["agent", "add", "limited", "--role", "coder", "--authority", "manager"]);
  const db = new DatabaseSync(dbPath);
  try {
    const setPolicy = (policy) => {
      const stored = JSON.stringify({ canDelegate: true, canReview: true, policy: { ...policy, updatedMs: 123, by: "operator" } });
      db.prepare("UPDATE identities SET permissions_json = ? WHERE agent_id = 'limited'").run(stored);
      return stored;
    };
    const refused = (impl, args, reason) => {
      const result = run(impl, "limited", ["task", "add", ...args]);
      assert.notEqual(result.status, 0, `${impl} bypassed policy`);
      assert.match(result.stderr, reason);
    };
    setPolicy({ canDelegate: false, maxConcurrentTasks: 1 });
    for (const impl of ["ts", "rs"]) {
      refused(impl, ["unassigned"], /may not delegate/);
      refused(impl, ["assigned to another", "--to", "alpha"], /may not delegate/);
      ok(impl, "limited", ["task", "add", "own work", "--to", "limited"]);
    }
    const stored = setPolicy({ allowedChildAgentIds: ["alpha"], maxDelegationDepth: 1, maxConcurrentTasks: 1 });
    for (const impl of ["ts", "rs"]) {
      refused(impl, ["wrong child", "--to", "beta"], /may only assign/);
      const child = jsonOut(impl, "limited", ["task", "add", "allowed child", "--to", "alpha", "--parent", String(task.id)]);
      refused(impl, ["too deep", "--to", "alpha", "--parent", String(child.id)], /at most 1 level/);
      ok(impl, "operator", ["task", "cancel", String(child.id)]);
      ok(impl, null, ["token", "rotate", "limited"]);
      assert.equal(db.prepare("SELECT permissions_json FROM identities WHERE agent_id = 'limited'").get().permissions_json, stored);
    }
    const results = await Promise.all(Array.from({ length: 6 }, (_, i) => new Promise((resolve, reject) => {
      const command = i % 2 === 0 ? [process.execPath, QAGENT_TS] : [QAGENT_RS];
      const child = spawn(command[0], [...command.slice(1), "task", "claim"], { env: env("limited"), stdio: "ignore" });
      child.once("error", reject);
      child.once("close", resolve);
    })));
    assert.equal(results.filter((code) => code === 0).length, 1, `mixed claim exits: ${results}`);
    assert.equal(db.prepare("SELECT COUNT(*) AS n FROM tasks WHERE state = 'claimed' AND assignee = 'limited'").get().n, 1);
  } finally {
    db.close();
  }

  // Pause and budgets live in agent meta, so either side reads what the other wrote.
  const agentIn = (impl, id) => jsonOut(impl, null, ["status"]).agents.find((a) => a.id === id);
  ok("ts", null, ["agent", "pause", "alpha", "lunch"]);
  assert.equal(agentIn("rs", "alpha").meta.paused.reason, "lunch");
  ok("rs", null, ["agent", "budget", "alpha", "--turns", "5", "--minutes", "30"]);
  const budget = jsonOut("ts", null, ["agent", "budget", "alpha"]);
  assert.equal(budget.limits.turns, 5);
  assert.equal(budget.limits.minutes, 30);
  assert.equal(budget.over, null);
  ok("rs", null, ["agent", "resume", "alpha"]);
  assert.equal(agentIn("ts", "alpha").meta.paused, undefined);
  ok("ts", null, ["agent", "budget", "alpha", "--clear"]);
  assert.equal(jsonOut("rs", null, ["agent", "budget", "alpha"]).limits, null);

  // Rust `wait` is woken by a TS send (signal file + data_version poll cross over).
  const waiter = spawn(QAGENT_RS, ["wait", "--seconds", "10"], { env: env("beta") });
  const waiterExit = new Promise((resolve, reject) => {
    waiter.once("error", reject);
    waiter.once("exit", resolve);
  });
  await new Promise((resolve) => setTimeout(resolve, 300));
  ok("ts", "alpha", ["send", "beta", "wake up", "still there?"]);
  const code = await waiterExit;
  assert.equal(code, 0, `rust wait exited ${code}`);

  // And a TS `wait` wakes on a Rust send.
  const tsWaiter = spawn(process.execPath, [QAGENT_TS, "wait", "--seconds", "10"], { env: env("alpha") });
  const tsWaiterExit = new Promise((resolve, reject) => {
    tsWaiter.once("error", reject);
    tsWaiter.once("exit", resolve);
  });
  await new Promise((resolve) => setTimeout(resolve, 300));
  ok("rs", "beta", ["send", "alpha", "back at you", "ping"]);
  const tsCode = await tsWaiterExit;
  assert.equal(tsCode, 0, `ts wait exited ${tsCode}`);

  process.stdout.write("v2 interop smoke: ok\n");
} finally {
  rmSync(home, { recursive: true, force: true });
}
