#!/usr/bin/env node
// v2 interop smoke: the TypeScript and Rust qagent binaries share one bus.db —
// mail, cursors, task claims and `wait` wake-ups must cross the language
// boundary. Run after `npm run build:core` and `cargo build` (rust/).
import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

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
  const task = jsonOut("ts", "alpha", ["task", "add", "interop task", "--role", "coder"]);
  const claimed = ok("rs", "alpha", ["task", "claim", String(task.id)]).stdout;
  assert.match(claimed, /claimed|assigned|task #\d+/i);
  const shown = jsonOut("ts", null, ["task", "show", String(task.id)]);
  assert.equal(shown.assignee, "alpha");
  assert.equal(shown.state, "claimed");

  // Rust `wait` is woken by a TS send (signal file + data_version poll cross over).
  const waiter = spawn(QAGENT_RS, ["wait", "--seconds", "10"], { env: env("beta") });
  await new Promise((resolve) => setTimeout(resolve, 300));
  ok("ts", "alpha", ["send", "beta", "wake up", "still there?"]);
  const code = await new Promise((resolve) => waiter.once("exit", resolve));
  assert.equal(code, 0, `rust wait exited ${code}`);

  // And a TS `wait` wakes on a Rust send.
  const tsWaiter = spawn(process.execPath, [QAGENT_TS, "wait", "--seconds", "10"], { env: env("alpha") });
  await new Promise((resolve) => setTimeout(resolve, 300));
  ok("rs", "beta", ["send", "alpha", "back at you", "ping"]);
  const tsCode = await new Promise((resolve) => tsWaiter.once("exit", resolve));
  assert.equal(tsCode, 0, `ts wait exited ${tsCode}`);

  process.stdout.write("v2 interop smoke: ok\n");
} finally {
  rmSync(home, { recursive: true, force: true });
}
