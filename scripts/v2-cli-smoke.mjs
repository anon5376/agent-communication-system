#!/usr/bin/env node
// v2 CLI smoke: drives dist/qagent.js (built by `npm run build:core`) through init, two agents,
// messaging, the full task cycle and a wait, on a temporary database. Nothing listens on a port.
import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const QAGENT = resolve("dist/qagent.js");
if (!existsSync(QAGENT)) {
  process.stderr.write("dist/qagent.js is missing; run `npm run build:core` first\n");
  process.exit(1);
}
const home = mkdtempSync(join(tmpdir(), "qagent-v2-smoke-"));
const dbPath = join(home, "bus.db");

function env(agent) {
  const value = { ...process.env, QAGENT_BUS_DB: dbPath };
  for (const name of ["QAGENT_AGENT_ID", "AGENT_ID", "QAGENT_HOME", "AGENT_BUS_HOME", "QAGENT_BLOCK_SEC"]) delete value[name];
  if (agent) value.QAGENT_AGENT_ID = agent;
  return value;
}

function q(agent, args, input) {
  const result = spawnSync(process.execPath, [QAGENT, ...args], { env: env(agent), encoding: "utf8", input });
  const shown = `${agent ?? "operator"}$ qagent ${args.map((arg) => (/\s/.test(arg) ? JSON.stringify(arg) : arg)).join(" ")}`;
  process.stdout.write(`${shown}  -> exit ${result.status}\n`);
  return result;
}

function ok(agent, args, input) {
  const result = q(agent, args, input);
  assert.equal(result.status, 0, result.stderr);
  return result;
}

function json(agent, args, input) {
  return JSON.parse(ok(agent, [...args, "--json"], input).stdout);
}

try {
  ok(undefined, ["init"]);
  ok(undefined, ["agent", "add", "lead", "--role", "manager", "--authority", "manager"]);
  ok(undefined, ["agent", "add", "worker", "--role", "worker"]);

  const sent = json("lead", ["send", "worker", "kickoff", "-"], "Parser work is coming.");
  assert.equal(sent[0].recipient, "worker");
  assert.equal(json("worker", ["inbox"]).messages[0].subject, "kickoff");
  ok("worker", ["send", "lead", "ack", "ready"]);

  const task = json("lead", ["task", "add", "Write the parser", "--to", "worker", "--brief", "Parse the config file"]);
  const id = String(task.id);
  assert.equal(json("worker", ["task", "claim", id]).state, "claimed");
  ok("worker", ["task", "note", id, "skeleton done"]);
  assert.equal(json("worker", ["task", "submit", id, "--summary", "parser done", "--file", "src/parser.ts"]).state, "submitted");
  assert.equal(json("lead", ["task", "review", id, "--revise", "--feedback", "add tests"]).state, "changes_requested");
  assert.equal(json("worker", ["task", "submit", id, "--summary", "parser plus tests"]).state, "submitted");
  assert.equal(json("lead", ["task", "review", id, "--accept", "--feedback", "good"]).state, "accepted");

  const forged = q("worker", ["agent", "add", "mallory"]);
  assert.equal(forged.status, 3, "an agent token cannot run operator commands");

  const timeout = q("worker", ["wait", "--timeout", "1"]);
  assert.equal(timeout.status, 0, "worker has unread review mail, so wait returns at once");
  json("worker", ["inbox"]);
  const waiter = spawn(process.execPath, [QAGENT, "wait", "--timeout", "20", "--json"], { env: env("worker"), stdio: ["ignore", "pipe", "pipe"] });
  let waited = "";
  waiter.stdout.setEncoding("utf8").on("data", (chunk) => { waited += chunk; });
  const exited = new Promise((resolveExit) => waiter.on("close", resolveExit));
  for (let i = 0; i < 100 && !JSON.parse(ok(undefined, ["agent", "list", "--json"]).stdout).some((a) => a.id === "worker" && a.status === "waiting"); i += 1) {
    await new Promise((r) => setTimeout(r, 50));
  }
  const sentAt = Date.now();
  ok("lead", ["send", "worker", "next", "one more thing"]);
  const code = await exited;
  process.stdout.write(`worker$ qagent wait --timeout 20  -> exit ${code} (${Date.now() - sentAt} ms after the send)\n`);
  assert.equal(code, 0);
  assert.equal(JSON.parse(waited).messages[0].subject, "next");

  process.stdout.write(`\n${ok(undefined, ["status"]).stdout}`);
  process.stdout.write(`\n${ok(undefined, ["task", "show", id]).stdout}`);
  const events = ok(undefined, ["log"]).stdout.trim().split("\n");
  process.stdout.write(`\nlog: ${events.length} events, last: ${events.at(-1)}\n`);
  process.stdout.write("\nv2 CLI smoke passed\n");
} finally {
  rmSync(home, { recursive: true, force: true });
}
