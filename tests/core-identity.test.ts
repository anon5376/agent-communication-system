import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { copyFileSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { Bus } from "../src/core/bus.js";
import { hashToken } from "../src/core/identity.js";

const QAGENT = fileURLToPath(new URL("../src/qagent.js", import.meta.url));

function setup(t: { after: (fn: () => void) => void }) {
  const home = mkdtempSync(join(tmpdir(), "qagent-v2-identity-"));
  t.after(() => rmSync(home, { recursive: true, force: true }));
  const dbPath = join(home, "bus.db");
  const bus = Bus.open({ dbPath });
  t.after(() => bus.close());
  const init = bus.init();
  const operator = bus.identify("operator");
  bus.addAgent(operator, { id: "alice", role: "manager", authority: "manager" });
  bus.addAgent(operator, { id: "bob", role: "worker" });
  bus.addAgent(operator, { id: "carol", role: "worker" });
  const cli = (agent: string | undefined, args: string[]) => {
    const env: NodeJS.ProcessEnv = { ...process.env, QAGENT_BUS_DB: dbPath };
    for (const name of ["QAGENT_AGENT_ID", "AGENT_ID", "QAGENT_HOME", "AGENT_BUS_HOME"]) delete env[name];
    if (agent) env.QAGENT_AGENT_ID = agent;
    const result = spawnSync(process.execPath, [QAGENT, ...args], { env, encoding: "utf8" });
    return { status: result.status, stdout: result.stdout, stderr: result.stderr };
  };
  return { home, dbPath, bus, operator, init, cli };
}

test("a token for agent A cannot act as agent B", (t) => {
  const { home, bus, cli } = setup(t);
  const aliceToken = readFileSync(join(home, "tokens", "alice.token"), "utf8");
  writeFileSync(join(home, "tokens", "bob.token"), aliceToken);
  assert.throws(() => bus.identify("bob"), (error: Error & { code?: string }) => error.code === "unauthorized" && /does not belong to bob/.test(error.message));
  const forged = cli("bob", ["send", "carol", "hi", "from bob?"]);
  assert.equal(forged.status, 3);
  assert.match(forged.stderr, /token does not belong to bob/);
  assert.equal(bus.getMessages({ sinceSeq: 0 }).length, 0, "nothing was sent");

  // The sender is always the identity whose token was presented: alice's own name still works.
  assert.equal(cli("alice", ["send", "carol", "hi", "from alice"]).status, 0);
  assert.equal(bus.getMessages({ sinceSeq: 0 })[0].sender, "alice");

  // The operator token placed under an agent name is rejected too.
  copyFileSync(join(home, "operator.token"), join(home, "tokens", "carol.token"));
  assert.throws(() => bus.identify("carol"), /does not belong to carol/);
  // A missing or unknown token is rejected.
  rmSync(join(home, "tokens", "carol.token"));
  assert.throws(() => bus.identify("carol"), /no token file for carol/);
  writeFileSync(join(home, "tokens", "carol.token"), "made-up\n");
  assert.throws(() => bus.identify("carol"), /is not registered/);
  assert.throws(() => bus.identify("../escape"), /unsafe agent id/);
});

test("operator commands reject agent tokens", (t) => {
  const { bus, cli } = setup(t);
  const alice = bus.identify("alice");
  const bob = bus.identify("bob");
  assert.throws(() => bus.addAgent(alice, { id: "eve" }), /only the operator may add agents/);
  assert.throws(() => bus.rotateToken(bob, "alice"), /only the operator may rotate tokens/);

  const add = cli("alice", ["agent", "add", "eve", "--role", "worker"]);
  assert.equal(add.status, 3);
  assert.match(add.stderr, /only the operator may add agents/);
  const addAs = cli(undefined, ["--as", "bob", "agent", "add", "eve"]);
  assert.equal(addAs.status, 3);
  const rotate = cli("bob", ["token", "rotate", "alice"]);
  assert.equal(rotate.status, 3);
  assert.match(rotate.stderr, /only the operator may rotate tokens/);
  const importRun = cli("alice", ["import", "--jsonl", "/nonexistent.jsonl"]);
  assert.equal(importRun.status, 3);
  assert.match(importRun.stderr, /only the operator may import/);
  assert.equal(bus.getAgent("eve"), null);

  // Cancelling or reviewing any task needs the operator; the creator and reviewer keep their own rights.
  const task = bus.createTask(alice, { title: "t", to: "bob" });
  bus.claimTask(bob, task.id);
  assert.throws(() => bus.cancelTask(bob, task.id), /only alice or the operator may cancel/);
  bus.submitTask(bob, task.id, { summary: "done" });
  assert.throws(() => bus.reviewTask(bob, task.id, { accepted: true, feedback: "self" }), /only alice or the operator may review/);
  assert.equal(cli(undefined, ["--as", "operator", "task", "review", String(task.id), "--accept", "--feedback", "ok"]).status, 0);
  assert.equal(bus.getTask(task.id).state, "accepted");
  const other = bus.createTask(alice, { title: "u" });
  assert.equal(cli(undefined, ["--as", "operator", "task", "cancel", String(other.id)]).status, 0);
  assert.equal(cli(undefined, ["agent", "add", "eve", "--role", "worker"]).status, 0, "no identity given: operator commands use the operator token");
});

test("tokens are stored hashed with private files, rotation invalidates the old token, init adopts or repairs the operator token", (t) => {
  const { home, dbPath, bus, operator, init } = setup(t);
  assert.equal(init.operator, "created");
  const aliceToken = readFileSync(join(home, "tokens", "alice.token"), "utf8").trim();
  const operatorToken = readFileSync(join(home, "operator.token"), "utf8").trim();
  assert.equal(statSync(join(home, "tokens", "alice.token")).mode & 0o777, 0o600);
  assert.equal(statSync(join(home, "operator.token")).mode & 0o777, 0o600);
  assert.equal(statSync(join(home, "tokens")).mode & 0o777, 0o700);
  assert.equal(statSync(home).mode & 0o777, 0o700);
  const stored = bus.db.prepare("SELECT token_hash FROM identities WHERE agent_id = 'alice'").get() as { token_hash: string };
  assert.equal(stored.token_hash, hashToken(aliceToken));
  bus.db.exec("PRAGMA wal_checkpoint(TRUNCATE)");
  const raw = readFileSync(dbPath);
  assert.equal(raw.includes(Buffer.from(aliceToken)), false, "the raw token is not in the database");
  assert.equal(raw.includes(Buffer.from(operatorToken)), false);

  bus.rotateToken(operator, "alice");
  assert.notEqual(readFileSync(join(home, "tokens", "alice.token"), "utf8").trim(), aliceToken);
  assert.equal(bus.identify("alice").agentId, "alice");
  const stale = join(home, "stale.token");
  writeFileSync(stale, aliceToken);
  copyFileSync(stale, join(home, "tokens", "alice.token"));
  assert.throws(() => bus.identify("alice"), /is not registered/);

  assert.equal(bus.init().operator, "unchanged");
  writeFileSync(join(home, "operator.token"), "tampered\n");
  assert.equal(bus.init().operator, "rotated");
  assert.equal(bus.identify("operator").authority, "operator");

  // A fresh bus adopts an operator.token that already exists (the Qagent home keeps its token).
  const other = mkdtempSync(join(tmpdir(), "qagent-v2-adopt-"));
  t.after(() => rmSync(other, { recursive: true, force: true }));
  writeFileSync(join(other, "operator.token"), "existing-operator-token\n", { mode: 0o600 });
  const fresh = Bus.open({ dbPath: join(other, "bus.db") });
  t.after(() => fresh.close());
  assert.equal(fresh.init().operator, "adopted");
  assert.equal(readFileSync(join(other, "operator.token"), "utf8").trim(), "existing-operator-token");
  assert.equal(fresh.identify("operator").authority, "operator");
});
