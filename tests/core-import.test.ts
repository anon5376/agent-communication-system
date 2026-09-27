import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { Bus } from "../src/core/bus.js";
import { hashToken } from "../src/core/identity.js";
import { runImport, type ImportReport, type SourceReport } from "../src/core/import.js";

const QAGENT = fileURLToPath(new URL("../src/qagent.js", import.meta.url));

// Schemas copied from ~/.agent-bus/state.sqlite (store.ts) and ~/prototype_0.2/coordinator/schema.sql.
const QAGENT_SCHEMA = `
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE identities (id TEXT PRIMARY KEY, token_hash TEXT NOT NULL UNIQUE, authority TEXT NOT NULL, permissions_json TEXT NOT NULL, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);
CREATE TABLE agents (id TEXT PRIMARY KEY, json TEXT NOT NULL, updated_at INTEGER NOT NULL);
CREATE TABLE messages (seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT NOT NULL UNIQUE, to_agent TEXT NOT NULL, delivered INTEGER NOT NULL DEFAULT 0, json TEXT NOT NULL, created_at INTEGER NOT NULL);
CREATE TABLE tasks (id TEXT PRIMARY KEY, run_id TEXT, state TEXT NOT NULL, assignee TEXT NOT NULL, parent_task_id TEXT, updated_at INTEGER NOT NULL, json TEXT NOT NULL);
CREATE TABLE runs (id TEXT PRIMARY KEY, status TEXT NOT NULL, updated_at INTEGER NOT NULL, json TEXT NOT NULL);
CREATE TABLE path_leases (task_id TEXT NOT NULL, run_id TEXT NOT NULL, path TEXT NOT NULL, created_at INTEGER NOT NULL, PRIMARY KEY(task_id, path));
`;
const PROTOTYPE_SCHEMA = `
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT);
CREATE TABLE agents (id TEXT PRIMARY KEY, display_name TEXT NOT NULL DEFAULT '', model TEXT NOT NULL DEFAULT '', role TEXT NOT NULL DEFAULT '', capabilities TEXT NOT NULL DEFAULT '[]', permissions TEXT NOT NULL DEFAULT '[]', parent_id TEXT, status TEXT NOT NULL DEFAULT 'idle', heartbeat_ts TEXT, created_ts TEXT NOT NULL, updated_ts TEXT NOT NULL, meta TEXT NOT NULL DEFAULT '{}');
CREATE TABLE messages (id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT NOT NULL, sender TEXT NOT NULL, recipient TEXT, subject TEXT NOT NULL DEFAULT '', body TEXT NOT NULL DEFAULT '', thread TEXT NOT NULL DEFAULT '', priority TEXT NOT NULL DEFAULT 'normal', requires_ack INTEGER NOT NULL DEFAULT 0);
CREATE TABLE message_receipts (message_id INTEGER NOT NULL, agent_id TEXT NOT NULL, read_ts TEXT, ack_ts TEXT, PRIMARY KEY (message_id, agent_id));
CREATE TABLE cursors (agent_id TEXT PRIMARY KEY, last_read_message_id INTEGER NOT NULL DEFAULT 0);
CREATE TABLE tasks (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL, description TEXT NOT NULL DEFAULT '', creator TEXT NOT NULL DEFAULT '', assignee TEXT, parent_id INTEGER, priority TEXT NOT NULL DEFAULT 'normal', status TEXT NOT NULL DEFAULT 'todo', acceptance TEXT NOT NULL DEFAULT '', artifacts TEXT NOT NULL DEFAULT '[]', created_ts TEXT NOT NULL, updated_ts TEXT NOT NULL);
CREATE TABLE task_notes (id INTEGER PRIMARY KEY AUTOINCREMENT, task_id INTEGER NOT NULL, author TEXT NOT NULL DEFAULT '', ts TEXT NOT NULL, note TEXT NOT NULL DEFAULT '');
CREATE TABLE task_dependencies (task_id INTEGER NOT NULL, depends_on_task_id INTEGER NOT NULL, PRIMARY KEY (task_id, depends_on_task_id));
CREATE TABLE events (id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT NOT NULL, actor TEXT NOT NULL DEFAULT '', kind TEXT NOT NULL, entity_type TEXT NOT NULL DEFAULT '', entity_id TEXT NOT NULL DEFAULT '', data TEXT NOT NULL DEFAULT '{}');
`;

const T0 = 1_784_653_403_000;

function buildFixtures(dir: string): { jsonl: string; qagentState: string; prototype: string } {
  const state = new DatabaseSync(join(dir, "state.sqlite"));
  state.exec("PRAGMA journal_mode = WAL");
  state.exec(QAGENT_SCHEMA);
  const agent = (id: string, role: string) => JSON.stringify({ id, role, model: `${id}-model`, family: "f", provider: "p", harness: "claude", registeredAt: T0, lastSeen: T0 + 5 });
  for (const [id, role] of [["operator", "operator"], ["fable5", "manager"], ["bad id", "x"]]) state.prepare("INSERT INTO agents VALUES(?, ?, ?)").run(id, agent(id, role), T0);
  state.prepare("INSERT INTO identities VALUES(?, ?, ?, ?, ?, ?)").run("operator", hashToken("old-operator-token"), "operator", "{}", T0, T0);
  state.prepare("INSERT INTO identities VALUES(?, ?, ?, ?, ?, ?)").run("fable5", hashToken("fable5-token"), "worker", "{\"canReview\":true}", T0, T0);
  const message = (id: string, ts: number, from: string, to: string, taskId: string | null) => JSON.stringify({ id, seq: 0, ts, from, to, type: "task", subject: `subject ${id}`, body: `body ${id}`, taskId });
  state.prepare("INSERT INTO messages(id, to_agent, delivered, json, created_at) VALUES(?, ?, 1, ?, ?)").run("msg_shared_1", "fable5", message("msg_shared_1", T0 + 100, "operator", "fable5", "task_a"), T0 + 100);
  state.prepare("INSERT INTO messages(id, to_agent, delivered, json, created_at) VALUES(?, ?, 0, ?, ?)").run("msg_state_only", "operator", message("msg_state_only", T0 + 200, "fable5", "operator", "task_a"), T0 + 200);
  const task = (fields: Record<string, unknown>) => JSON.stringify({
    runId: null, childTaskIds: [], dependencyIds: [], parentTaskId: null, context: "", contextRefs: [{ type: "path", value: "src/a.ts" }],
    assigner: "operator", assignee: "fable5", role: "research", pathScopes: ["src"], validationRequirements: [{ id: "v1", description: "tests pass", required: true }],
    round: 1, attempts: 0, maxRetries: 2, reviewerId: null, result: null, review: null, createdAt: T0, updatedAt: T0 + 300, history: [], ...fields,
  });
  state.prepare("INSERT INTO tasks VALUES(?, NULL, ?, ?, NULL, ?, ?)").run("task_a", "submitted", "fable5", T0, task({
    id: "task_a", title: "Task A", brief: "brief A", state: "submitted",
    result: { summary: "done A", details: "", changedFiles: ["src/a.ts"], artifacts: [], validation: [], completedAt: T0 + 250 },
    history: [{ ts: T0, actor: "operator", kind: "assigned", state: "assigned", note: "Task A" }, { ts: T0 + 250, actor: "fable5", kind: "submitted", state: "submitted", note: "done A" }],
  }));
  state.prepare("INSERT INTO tasks VALUES(?, NULL, ?, ?, ?, ?, ?)").run("task_b", "in_progress", "fable5", "task_a", T0, task({
    id: "task_b", title: "Task B", brief: "brief B", state: "in_progress", parentTaskId: "task_a", dependencyIds: ["task_a"],
    history: [{ ts: T0 + 10, actor: "operator", kind: "assigned", state: "assigned", note: "Task B" }],
  }));
  state.prepare("INSERT INTO path_leases VALUES('task_b', 'run', 'src', ?)").run(T0);
  state.close();

  const proto = new DatabaseSync(join(dir, "prototype.db"));
  proto.exec(PROTOTYPE_SCHEMA);
  const iso = (offset: number) => new Date(T0 - 86_400_000 + offset).toISOString();
  proto.prepare("INSERT INTO agents(id, model, role, parent_id, created_ts, updated_ts) VALUES(?, ?, ?, ?, ?, ?)").run("worker-1", "sonnet", "worker", "lead", iso(0), iso(0));
  proto.prepare("INSERT INTO agents(id, model, role, parent_id, created_ts, updated_ts) VALUES(?, ?, ?, ?, ?, ?)").run("lead", "opus", "supervisor", null, iso(0), iso(0));
  proto.prepare("INSERT INTO agents(id, model, role, parent_id, created_ts, updated_ts) VALUES(?, ?, ?, ?, ?, ?)").run("bad agent", "", "", null, iso(0), iso(0));
  proto.prepare("INSERT INTO messages(ts, sender, recipient, subject, body, thread) VALUES(?, ?, ?, ?, ?, ?)").run(iso(10), "lead", "worker-1", "go", "start", "t1");
  proto.prepare("INSERT INTO messages(ts, sender, recipient, subject, body, thread, requires_ack) VALUES(?, ?, NULL, ?, ?, ?, 1)").run(iso(20), "lead", "all", "broadcast body", "");
  proto.prepare("INSERT INTO messages(ts, sender, recipient, subject, body, thread) VALUES(?, ?, ?, ?, ?, ?)").run(iso(30), "worker-1", "lead", "ok", "on it", "t1");
  proto.prepare("INSERT INTO message_receipts VALUES(1, 'worker-1', ?, ?)").run(iso(11), iso(12));
  proto.prepare("INSERT INTO message_receipts VALUES(2, 'worker-1', ?, NULL)").run(iso(21));
  proto.prepare("INSERT INTO cursors VALUES('worker-1', 2)").run();
  const insertTask = proto.prepare("INSERT INTO tasks(title, description, creator, assignee, parent_id, status, artifacts, created_ts, updated_ts) VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?)");
  insertTask.run("P1", "d1", "lead", "worker-1", null, "review", "[\"out/report.html\"]", iso(40), iso(50));
  insertTask.run("P2", "d2", "lead", null, 1, "todo", "[]", iso(41), iso(51));
  insertTask.run("P3", "d3", "lead", "worker-1", null, "done", "[]", iso(42), iso(52));
  insertTask.run("P4", "d4", "lead", null, null, "bogus", "[]", iso(43), iso(53));
  proto.prepare("INSERT INTO task_dependencies VALUES(2, 1)").run();
  proto.prepare("INSERT INTO task_notes(task_id, author, ts, note) VALUES(1, 'worker-1', ?, 'note one')").run(iso(45));
  proto.prepare("INSERT INTO task_notes(task_id, author, ts, note) VALUES(1, 'lead', ?, 'note two')").run(iso(46));
  proto.prepare("INSERT INTO events(ts, actor, kind, entity_type, entity_id, data) VALUES(?, 'lead', 'agent_registered', 'agent', 'lead', '{}')").run(iso(0));
  proto.prepare("INSERT INTO events(ts, actor, kind, entity_type, entity_id, data) VALUES(?, 'lead', 'task_created', 'task', '1', '{\"title\":\"P1\"}')").run(iso(40));
  proto.prepare("INSERT INTO events(ts, actor, kind, entity_type, entity_id, data) VALUES(?, 'lead', 'message_sent', 'message', '1', '{}')").run(iso(10));
  proto.close();

  const lines = [
    { ts: T0, kind: "register", data: { id: "fable5", role: "manager", model: "fable-5" } },
    { ts: T0 + 1, kind: "register", data: { id: "gpt", role: "worker", model: "gpt-5.6" } },
    { ts: T0 + 2, kind: "register", data: { id: "fable5", role: "manager", model: "fable-5", harness: "claude" } },
    { ts: T0 + 3, kind: "register", data: { id: "bad id", role: "worker", model: "x" } },
    { ts: T0 + 100, kind: "message", data: { id: "msg_shared_1", ts: T0 + 100, from: "operator", to: "fable5", type: "task", subject: "subject msg_shared_1", body: "body msg_shared_1", taskId: "task_a" } },
    { ts: T0 + 400, kind: "message", data: { id: "msg_jsonl_1", seq: 7, ts: T0 + 400, from: "gpt", to: "fable5", type: "result", subject: "s", body: "b", taskId: "task_a" } },
    { ts: T0 + 500, kind: "message", data: { id: "msg_jsonl_2", ts: T0 + 500, from: "fable5", to: "gpt", type: "feedback", subject: "s2", body: "b2", taskId: null, refs: [{ type: "path", value: "x" }] } },
    { ts: T0 + 90, kind: "task_create", data: { id: "task_a", title: "Task A", brief: "long brief", assigner: "operator", assignee: "fable5", state: "assigned" } },
    { ts: T0 + 250, kind: "task_submit", data: { taskId: "task_a", actor: "fable5" } },
    { ts: T0 + 600, kind: "kill", data: { target: "gpt", pid: 1, killed: true, by: "operator" } },
  ].map((entry) => JSON.stringify(entry));
  lines.splice(6, 0, "{not json");
  writeFileSync(join(dir, "bus.jsonl"), `${lines.join("\n")}\n`);
  return { jsonl: join(dir, "bus.jsonl"), qagentState: join(dir, "state.sqlite"), prototype: join(dir, "prototype.db") };
}

/** Independent counts of the fixture sources, read without the importer. */
function sourceCounts(paths: { jsonl: string; qagentState: string; prototype: string }): Record<string, Record<string, number>> {
  const lines = readFileSync(paths.jsonl, "utf8").split("\n").filter((line) => line.trim());
  const parsed = lines.flatMap((line) => { try { return [JSON.parse(line) as { kind: string; data: { id?: string } }]; } catch { return []; } });
  const count = (db: DatabaseSync, sql: string) => Number((db.prepare(sql).get() as { n: number }).n);
  const state = new DatabaseSync(paths.qagentState, { readOnly: true });
  const proto = new DatabaseSync(paths.prototype, { readOnly: true });
  try {
    const tasks = (state.prepare("SELECT json FROM tasks").all() as { json: string }[]).map((row) => JSON.parse(row.json) as { history: unknown[]; dependencyIds: unknown[] });
    return {
      "bus.jsonl": {
        lines: lines.length,
        messages: parsed.filter((entry) => entry.kind === "message").length,
        registrations: parsed.filter((entry) => entry.kind === "register").length,
        agents: new Set(parsed.filter((entry) => entry.kind === "register" && /^[A-Za-z0-9._-]+$/.test(String(entry.data.id))).map((entry) => entry.data.id)).size,
        events: parsed.filter((entry) => entry.kind !== "message" && entry.kind !== "register").length,
      },
      qagent: {
        agents: count(state, "SELECT COUNT(*) AS n FROM agents"),
        identities: count(state, "SELECT COUNT(*) AS n FROM identities"),
        messages: count(state, "SELECT COUNT(*) AS n FROM messages"),
        tasks: count(state, "SELECT COUNT(*) AS n FROM tasks"),
        history: tasks.reduce((sum, task) => sum + task.history.length, 0),
        dependencies: tasks.reduce((sum, task) => sum + task.dependencyIds.length, 0),
      },
      prototype: {
        agents: count(proto, "SELECT COUNT(*) AS n FROM agents"),
        messages: count(proto, "SELECT COUNT(*) AS n FROM messages"),
        receipts: count(proto, "SELECT COUNT(*) AS n FROM message_receipts"),
        acks: count(proto, "SELECT COUNT(*) AS n FROM message_receipts WHERE ack_ts IS NOT NULL"),
        tasks: count(proto, "SELECT COUNT(*) AS n FROM tasks"),
        dependencies: count(proto, "SELECT COUNT(*) AS n FROM task_dependencies"),
        notes: count(proto, "SELECT COUNT(*) AS n FROM task_notes"),
        events: count(proto, "SELECT COUNT(*) AS n FROM events"),
      },
    };
  } finally {
    state.close();
    proto.close();
  }
}

function dbDigest(path: string): string {
  const db = new DatabaseSync(path, { readOnly: true });
  try {
    const hash = createHash("sha256");
    for (const { name } of db.prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name").all() as { name: string }[]) {
      hash.update(name);
      for (const row of db.prepare(`SELECT * FROM "${name}" ORDER BY rowid`).all()) hash.update(JSON.stringify(row));
    }
    return hash.digest("hex");
  } finally {
    db.close();
  }
}

function bySource(report: ImportReport): Record<string, SourceReport> {
  return Object.fromEntries(report.sources.map((source) => [source.kind, source]));
}

function sum(record: Record<string, number>): number {
  return Object.values(record).reduce((total, n) => total + n, 0);
}

test("importer dry-run counts match the fixture sources, the import matches the dry run, and reruns change nothing", (t) => {
  const dir = mkdtempSync(join(tmpdir(), "qagent-v2-import-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const paths = buildFixtures(join(mkdtempSync(join(dir, "src-")), ""));
  const expected = sourceCounts(paths);

  // Dry run against a database that does not exist yet: exact source counts, nothing created.
  const freshPath = join(dir, "fresh", "bus.db");
  const fresh = bySource(runImport(freshPath, paths, { dryRun: true }));
  assert.equal(existsSync(freshPath), false, "a dry run does not create bus.db");
  for (const kind of ["bus.jsonl", "qagent", "prototype"]) {
    for (const [key, n] of Object.entries(expected[kind])) assert.equal(fresh[kind].read[key] ?? 0, n, `${kind} read.${key}`);
  }
  assert.equal(fresh["bus.jsonl"].invalid.lines, 1);
  assert.equal(fresh["bus.jsonl"].invalid.registrations, 1);
  assert.equal(fresh.qagent.invalid.agents, 1);
  assert.equal(fresh.prototype.invalid.agents, 1);
  assert.equal(fresh.prototype.invalid.tasks, 1);
  assert.equal(fresh.qagent.inserted.messages, 2);
  assert.equal(fresh["bus.jsonl"].inserted.messages, 2);
  assert.equal(fresh["bus.jsonl"].duplicates.messages, 1, "a message present in state.sqlite and bus.jsonl is stored once");

  // Initialise the real target, as the migration does, then dry-run it: content unchanged.
  const dbPath = join(dir, "home", "bus.db");
  const bus = Bus.open({ dbPath });
  bus.init();
  bus.close();
  writeFileSync(join(dir, "home", "tokens", "fable5.token"), "fable5-token\n", { mode: 0o600 });
  const initial = dbDigest(dbPath);
  const dry = runImport(dbPath, paths, { dryRun: true });
  assert.equal(dbDigest(dbPath), initial, "a dry run leaves bus.db unchanged");

  const real = runImport(dbPath, paths);
  for (const source of real.sources) {
    const plan = dry.sources.find((entry) => entry.kind === source.kind)!;
    assert.deepEqual(source.inserted, plan.inserted, `${source.kind}: import matches the dry run`);
    assert.deepEqual(source.duplicates, plan.duplicates);
  }
  const after = bySource(real);
  assert.equal(after.qagent.duplicates.agents, 1, "the operator row from init is kept");
  assert.equal(after.qagent.duplicates.identities, 1, "the operator identity from init is kept");
  assert.equal(after.qagent.inserted.identities, 1);

  const check = Bus.open({ dbPath });
  t.after(() => check.close());
  const count = (sql: string) => Number((check.db.prepare(sql).get() as { n: number }).n);
  assert.equal(count("SELECT COUNT(*) AS n FROM messages"), 2 + 3 + 2);
  assert.equal(count("SELECT COUNT(*) AS n FROM messages WHERE recipient IS NULL AND source = 'prototype'"), 1, "broadcast kept");
  assert.equal(count("SELECT COUNT(*) AS n FROM tasks"), 2 + 3);
  assert.equal(count("SELECT COUNT(*) AS n FROM task_deps"), 2);
  assert.equal(count("SELECT COUNT(*) AS n FROM task_notes"), 2);
  assert.equal(count("SELECT COUNT(*) AS n FROM acks"), 1);
  assert.equal(count("SELECT COUNT(*) AS n FROM agents"), 5);
  assert.equal(count("SELECT COUNT(*) AS n FROM events WHERE source <> 'v2'"), 3 + 3 + 3);
  const maxSeq = count("SELECT MAX(seq) AS n FROM messages");
  assert.equal(real.cursorSeq, maxSeq);
  assert.equal(count(`SELECT COUNT(*) AS n FROM cursors WHERE last_seq = ${maxSeq}`), 5, "every agent's cursor is at the newest imported message");
  const states = Object.fromEntries((check.db.prepare("SELECT legacy_id, state FROM tasks").all() as { legacy_id: string; state: string }[]).map((row) => [row.legacy_id, row.state]));
  assert.deepEqual(states, { task_a: "submitted", task_b: "claimed", "prototype:1": "submitted", "prototype:2": "open", "prototype:3": "accepted" });
  const taskA = check.db.prepare("SELECT id FROM tasks WHERE legacy_id = 'task_a'").get() as { id: number };
  const taskB = check.getTask(Number((check.db.prepare("SELECT id FROM tasks WHERE legacy_id = 'task_b'").get() as { id: number }).id));
  assert.equal(taskB.parentId, taskA.id);
  assert.deepEqual(taskB.dependencies, [taskA.id]);
  assert.ok(taskB.claimExpiresMs, "an imported in-progress claim gets an expiry so the next board write can reopen it");
  assert.equal(check.getAgent("worker-1")?.parentId, "lead");
  assert.equal(check.getMessages({ sinceSeq: 0, limit: 100 }).filter((m) => m.taskId === taskA.id).length, 3);
  assert.equal(check.identify("fable5").agentId, "fable5", "an imported identity keeps its existing token file working");
  assert.equal(check.unreadCount("fable5"), 0, "no agent wakes to imported history");
  check.close();

  const imported = dbDigest(dbPath);
  const rerun = runImport(dbPath, paths);
  for (const source of rerun.sources) {
    assert.equal(source.alreadyImported, true, `${source.kind} recognised by sha256`);
    assert.equal(sum(source.inserted), 0);
  }
  assert.equal(dbDigest(dbPath), imported, "a second run changes nothing");
  const forced = runImport(dbPath, paths, { force: true });
  for (const source of forced.sources) assert.equal(sum(source.inserted), 0, `${source.kind} --force inserts nothing new`);
  assert.equal(dbDigest(dbPath), imported, "a forced rerun changes nothing either");
});

test("`qagent import --dry-run --json` reports the same counts and creates nothing", (t) => {
  const dir = mkdtempSync(join(tmpdir(), "qagent-v2-importcli-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const paths = buildFixtures(join(mkdtempSync(join(dir, "src-")), ""));
  const expected = sourceCounts(paths);
  const dbPath = join(dir, "target", "bus.db");
  const env: NodeJS.ProcessEnv = { ...process.env, QAGENT_BUS_DB: dbPath };
  for (const name of ["QAGENT_AGENT_ID", "AGENT_ID", "QAGENT_HOME", "AGENT_BUS_HOME"]) delete env[name];
  const result = spawnSync(process.execPath, [QAGENT, "import", "--dry-run", "--json", "--jsonl", paths.jsonl, "--qagent-state", paths.qagentState, "--prototype", paths.prototype], { env, encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr);
  const report = bySource(JSON.parse(result.stdout) as ImportReport);
  for (const kind of ["bus.jsonl", "qagent", "prototype"]) {
    for (const [key, n] of Object.entries(expected[kind])) assert.equal(report[kind].read[key] ?? 0, n, `${kind} read.${key}`);
  }
  assert.equal(existsSync(dbPath), false);
});

// Opt-in check against copies of the real stores: QAGENT_REAL_IMPORT_DIR=<dir with bus.jsonl, state.sqlite, prototype.db>.
test("dry-run counts on copies of the real stores match their source counts", { skip: !process.env.QAGENT_REAL_IMPORT_DIR }, (t) => {
  const dir = process.env.QAGENT_REAL_IMPORT_DIR!;
  const paths = { jsonl: join(dir, "bus.jsonl"), qagentState: join(dir, "state.sqlite"), prototype: join(dir, "prototype.db") };
  const expected = sourceCounts(paths);
  const scratch = mkdtempSync(join(tmpdir(), "qagent-v2-real-"));
  t.after(() => rmSync(scratch, { recursive: true, force: true }));
  const report = bySource(runImport(join(scratch, "bus.db"), paths, { dryRun: true }));
  for (const kind of ["bus.jsonl", "qagent", "prototype"]) {
    t.diagnostic(`${kind} read ${JSON.stringify(report[kind].read)} inserted ${JSON.stringify(report[kind].inserted)} duplicates ${JSON.stringify(report[kind].duplicates)} invalid ${JSON.stringify(report[kind].invalid)}`);
    for (const [key, n] of Object.entries(expected[kind])) assert.equal(report[kind].read[key] ?? 0, n, `${kind} read.${key}`);
  }
});
