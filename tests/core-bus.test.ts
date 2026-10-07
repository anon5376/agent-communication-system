import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { Bus } from "../src/core/bus.js";
import type { Identity } from "../src/core/identity.js";

const QAGENT = fileURLToPath(new URL("../src/qagent.js", import.meta.url));

function tempHome(t: { after: (fn: () => void) => void }): string {
  const dir = mkdtempSync(join(tmpdir(), "qagent-v2-bus-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  return dir;
}

function cliEnv(dbPath: string, agent?: string): NodeJS.ProcessEnv {
  const env: NodeJS.ProcessEnv = { ...process.env, QAGENT_BUS_DB: dbPath, QAGENT_URL: "http://127.0.0.1:9" };
  for (const name of ["QAGENT_AGENT_ID", "AGENT_ID", "QAGENT_HOME", "AGENT_BUS_HOME", "QAGENT_BLOCK_SEC"]) delete env[name];
  if (agent) env.QAGENT_AGENT_ID = agent;
  return env;
}

/** A preload that makes any listen, outbound connect or fetch crash the CLI process. */
function noNetworkPreload(dir: string): string {
  const path = join(dir, "no-network.mjs");
  writeFileSync(path, [
    "import net from 'node:net';",
    "const deny = (what) => function () { process.stderr.write('NETWORK_' + what + '\\n'); process.exit(97); };",
    "net.Server.prototype.listen = deny('LISTEN');",
    "net.Socket.prototype.connect = deny('CONNECT');",
    "globalThis.fetch = deny('FETCH');",
  ].join("\n"));
  return path;
}

function makeCli(dbPath: string, preload: string) {
  return (agent: string | undefined, args: string[], input?: string) => {
    const result = spawnSync(process.execPath, ["--import", preload, QAGENT, ...args], { env: cliEnv(dbPath, agent), encoding: "utf8", input });
    return { status: result.status, stdout: result.stdout, stderr: result.stderr, json: () => JSON.parse(result.stdout) };
  };
}

test("two agents exchange messages and finish create-claim-note-submit-revise-submit-accept through the CLI with no listener", (t) => {
  const home = tempHome(t);
  const dbPath = join(home, "bus.db");
  const cli = makeCli(dbPath, noNetworkPreload(home));
  const ok = (result: ReturnType<typeof cli>) => {
    assert.equal(result.status, 0, `exit ${result.status}: ${result.stderr}`);
    assert.doesNotMatch(result.stderr, /NETWORK_/);
    return result;
  };

  ok(cli(undefined, ["init", "--json"]));
  ok(cli(undefined, ["agent", "add", "alice", "--role", "manager", "--authority", "manager"]));
  ok(cli(undefined, ["agent", "add", "bob", "--role", "worker", "--model", "fake"]));

  ok(cli("alice", ["send", "bob", "hello", "-"], "can you take the parser?"));
  const bobInbox = ok(cli("bob", ["inbox", "--json"])).json();
  assert.equal(bobInbox.messages.length, 1);
  assert.equal(bobInbox.messages[0].sender, "alice");
  assert.equal(bobInbox.messages[0].body, "can you take the parser?");
  assert.equal(ok(cli("bob", ["inbox", "--json"])).json().messages.length, 0, "cursor advanced");
  ok(cli("bob", ["send", "alice", "re: hello", "yes"]));
  assert.equal(ok(cli("alice", ["inbox", "--json"])).json().messages[0].subject, "re: hello");

  const created = ok(cli("alice", ["task", "add", "Write the parser", "--to", "bob", "--brief", "Parse config", "--json"])).json();
  const id = String(created.id);
  assert.equal(created.state, "open");
  const claimed = ok(cli("bob", ["task", "claim", id, "--json"])).json();
  assert.equal(claimed.state, "claimed");
  assert.equal(claimed.assignee, "bob");
  ok(cli("bob", ["task", "note", id, "parser skeleton in place"]));
  assert.equal(ok(cli("bob", ["task", "submit", id, "--summary", "parser done", "--file", "src/parser.ts", "--json"])).json().state, "submitted");
  const revised = ok(cli("alice", ["task", "review", id, "--revise", "--feedback", "add tests", "--json"])).json();
  assert.equal(revised.state, "changes_requested");
  assert.equal(revised.round, 2);
  assert.equal(ok(cli("bob", ["task", "submit", id, "--summary", "parser done with tests", "--json"])).json().state, "submitted");
  const accepted = ok(cli("alice", ["task", "review", id, "--accept", "--feedback", "good", "--json"])).json();
  assert.equal(accepted.state, "accepted");

  const detail = ok(cli(undefined, ["task", "show", id, "--json"])).json();
  assert.equal(detail.state, "accepted");
  assert.equal(detail.round, 2);
  assert.equal(detail.notes.length, 1);
  assert.deepEqual(detail.messages.map((m: { subject: string }) => m.subject.split("]")[0] + "]"), [
    `[TASK #${id}]`, `[DONE #${id} r1]`, `[CHANGES #${id} r2]`, `[DONE #${id} r2]`, `[ACCEPTED #${id}]`,
  ]);
  const kinds = (ok(cli(undefined, ["log", "--json"])).stdout.trim().split("\n").map((line) => JSON.parse(line)) as { kind: string; entity: string }[])
    .filter((event) => event.entity === "task").map((event) => event.kind);
  assert.deepEqual(kinds, ["task_created", "task_claimed", "task_note", "task_submitted", "task_changes_requested", "task_submitted", "task_accepted"]);
  const bobMail = ok(cli("bob", ["inbox", "--json"])).json().messages.map((m: { type: string }) => m.type);
  assert.deepEqual(bobMail, ["task", "feedback", "feedback"]);
  const status = ok(cli(undefined, ["status", "--json"])).json();
  assert.equal(status.counts.accepted, 1);
});

function setup(t: { after: (fn: () => void) => void }, now?: () => number) {
  const home = tempHome(t);
  const bus = Bus.open({ dbPath: join(home, "bus.db"), now });
  t.after(() => bus.close());
  bus.init();
  const operator = bus.identify("operator");
  const add = (id: string, role = "worker", authority: "worker" | "manager" = "worker"): Identity => {
    bus.addAgent(operator, { id, role, authority });
    return bus.identify(id);
  };
  return { home, bus, operator, add };
}

test("broadcast reaches everyone but the sender, peek keeps the cursor, ack is recorded, signal files track delivery", (t) => {
  const { home, bus, add } = setup(t);
  const alice = add("alice", "manager", "manager");
  const bob = add("bob");
  const carol = add("carol");
  const [broadcast] = bus.send(alice, { to: "*", subject: "all hands", body: "standup", requiresAck: true });
  assert.equal(broadcast.recipient, null);
  bus.send(alice, { to: "bob,carol", subject: "pair", body: "pair on it" });
  assert.equal(bus.unreadCount("alice"), 0, "sender does not receive its own broadcast");
  assert.equal(bus.inbox(bob, { peek: true }).messages.length, 2);
  assert.equal(bus.inbox(bob, { peek: true }).messages.length, 2, "peek does not advance");
  const read = bus.inbox(bob, { limit: 1 });
  assert.equal(read.messages.length, 1);
  assert.equal(read.remaining, 1);
  assert.equal(bus.inbox(bob).messages.length, 1);
  assert.equal(bus.inbox(bob).messages.length, 0);
  assert.equal(bus.unreadCount("carol"), 2);
  bus.ack(carol, broadcast.seq);
  assert.equal((bus.db.prepare("SELECT COUNT(*) AS n FROM acks WHERE seq = ? AND agent_id = 'carol'").get(broadcast.seq) as { n: number }).n, 1);
  assert.throws(() => bus.ack(carol, 999), /no message 999/);
  const latestForCarol = bus.getMessages({ sinceSeq: 0 }).filter((m) => m.recipient === "carol" || m.recipient === null).at(-1)!.seq;
  assert.equal(readFileSync(join(home, "inbox", "carol.seq"), "utf8").trim(), String(latestForCarol));
  assert.throws(() => bus.send(alice, { to: "nobody", body: "x" }), /unknown recipient/);
  assert.throws(() => bus.send(alice, { to: "bob", body: "x".repeat(200_001) }), /exceeds/);
});

test("dependencies block and unblock, path leases conflict, claims expire, and too many revisions fail the task", (t) => {
  let clock = 1_000_000;
  const { home, bus, add } = setup(t, () => clock);
  const lead = add("lead", "manager", "manager");
  const w1 = add("w1");
  const w2 = add("w2");
  const first = bus.createTask(lead, { title: "first", project: home, pathScopes: ["src/core"] });
  const second = bus.createTask(lead, { title: "second", dependencies: [first.id] });
  assert.equal(second.state, "blocked");
  const overlapping = bus.createTask(lead, { title: "overlap", project: home, pathScopes: ["src/core/parser"] });
  bus.claimTask(w1, first.id);
  assert.throws(() => bus.claimTask(w2, overlapping.id), /overlap leases/);
  assert.throws(() => bus.claimTask(w2, first.id), /cannot be claimed/);
  bus.submitTask(w1, first.id, { summary: "done" });
  bus.reviewTask(lead, first.id, { accepted: true, feedback: "ok" });
  assert.equal(bus.getTask(second.id).state, "open", "accepting the dependency unblocks the dependent");
  assert.equal(bus.claimTask(w2, overlapping.id).state, "claimed", "accept released the lease");

  clock += 2 * 60 * 60_000 + 1;
  const next = bus.createTask(lead, { title: "any write reopens expired claims" });
  assert.equal(bus.getTask(overlapping.id).state, "open");
  assert.equal(bus.getTask(overlapping.id).assignee, null);
  assert.ok(bus.events().some((event) => event.kind === "claim_expired" && event.entityId === String(overlapping.id)));

  const retry = bus.createTask(lead, { title: "retry", to: "w1", maxRetries: 1 });
  bus.claimTask(w1, retry.id);
  bus.submitTask(w1, retry.id, { summary: "try 1" });
  assert.equal(bus.reviewTask(lead, retry.id, { accepted: false, feedback: "no" }).state, "changes_requested");
  assert.throws(() => bus.claimTask(w2, retry.id), /assigned to w1/);
  bus.submitTask(w1, retry.id, { summary: "try 2" });
  assert.equal(bus.reviewTask(lead, retry.id, { accepted: false, feedback: "still no" }).state, "failed");
  assert.equal(bus.claimTask(w2).id, second.id, "claim without an id takes the oldest claimable task");
  assert.equal(bus.claimTask(w2).id, overlapping.id);
  assert.equal(bus.claimTask(w1).id, next.id);
  assert.throws(() => bus.claimTask(w1), /no claimable task/);
});

test("every write appends an events row in the same transaction", (t) => {
  const { bus, add } = setup(t);
  const alice = add("alice", "manager", "manager");
  const before = bus.latestSeq();
  bus.send(alice, { to: "operator", body: "hi" });
  assert.equal(bus.latestSeq(), before + 1);
  assert.throws(() => bus.createTask(alice, { title: "bad", dependencies: [999] }), /unknown task: 999/);
  assert.equal(bus.latestSeq(), before + 1, "a failed write leaves no event and no row");
  assert.equal((bus.db.prepare("SELECT COUNT(*) AS n FROM tasks").get() as { n: number }).n, 0);
  assert.equal(String((bus.db.prepare("PRAGMA journal_mode").get() as { journal_mode: string }).journal_mode), "wal");
});

test("renewClaims keeps a long turn's claim past the claim TTL", (t) => {
  let clock = 1_700_000_000_000;
  const { bus, operator, add } = setup(t, () => clock);
  const w = add("w1");
  const task = bus.createTask(operator, { title: "long", to: "w1" });
  bus.claimTask(w, task.id);
  const hour = 60 * 60_000;
  for (let i = 0; i < 4; i += 1) {
    clock += hour;
    assert.equal(bus.renewClaims(w), 1);
  }
  bus.noteTask(operator, task.id, "still yours?");
  assert.equal(bus.getTask(task.id).state, "claimed");
  clock += 3 * hour;
  bus.noteTask(operator, task.id, "and now?");
  assert.equal(bus.getTask(task.id).state, "open");
});
