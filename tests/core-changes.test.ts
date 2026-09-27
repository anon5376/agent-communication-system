import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { setTimeout as sleep } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { Bus } from "../src/core/bus.js";
import { ChangeWatcher } from "../src/core/changes.js";

const QAGENT = fileURLToPath(new URL("../src/qagent.js", import.meta.url));
const BUS_MODULE = new URL("../src/core/bus.js", import.meta.url).href;
/** Idle window for the WAL test. 10 s in the unit suite; set QAGENT_IDLE_TEST_MS=60000 for the full acceptance run. */
const IDLE_MS = Number(process.env.QAGENT_IDLE_TEST_MS ?? 10_000);

function cleanEnv(extra: Record<string, string>): NodeJS.ProcessEnv {
  const env: NodeJS.ProcessEnv = { ...process.env, ...extra };
  for (const name of ["AGENT_ID", "QAGENT_HOME", "AGENT_BUS_HOME", "QAGENT_BLOCK_SEC"]) delete env[name];
  return env;
}

function setup(t: { after: (fn: () => void) => void }) {
  const home = mkdtempSync(join(tmpdir(), "qagent-v2-changes-"));
  t.after(() => rmSync(home, { recursive: true, force: true }));
  const dbPath = join(home, "bus.db");
  const bus = Bus.open({ dbPath });
  t.after(() => bus.close());
  bus.init();
  const operator = bus.identify("operator");
  bus.addAgent(operator, { id: "alice", role: "manager", authority: "manager" });
  bus.addAgent(operator, { id: "bob", role: "worker" });
  return { home, dbPath, bus };
}

/** Child: open the bus, wait a moment, send one message, print the time the commit returned. */
const WRITER = `
const { Bus } = await import(process.env.BUS_MODULE);
const bus = Bus.open({ dbPath: process.env.QAGENT_BUS_DB });
const me = bus.identify("alice");
await new Promise((resolve) => setTimeout(resolve, Number(process.env.DELAY_MS)));
bus.send(me, { to: "bob", subject: "ping", body: "x" });
process.stdout.write(String(Date.now()) + "\\n");
bus.close();
`;

function runWriter(script: string, dbPath: string, delayMs: number): Promise<number> {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [script], { env: cleanEnv({ BUS_MODULE, QAGENT_BUS_DB: dbPath, DELAY_MS: String(delayMs) }), stdio: ["ignore", "pipe", "pipe"] });
    let out = "";
    let err = "";
    child.stdout.setEncoding("utf8").on("data", (chunk: string) => { out += chunk; });
    child.stderr.setEncoding("utf8").on("data", (chunk: string) => { err += chunk; });
    child.on("close", (code) => code === 0 ? resolve(Number(out.trim())) : reject(new Error(`writer exited ${code}: ${err}`)));
  });
}

for (const fsWatch of [true, false]) {
  test(`changes.next() fires within 200 ms of a write from another process (fs.watch ${fsWatch ? "on" : "off, polling only"})`, async (t) => {
    const { home, dbPath, bus } = setup(t);
    const script = join(home, "writer.mjs");
    writeFileSync(script, WRITER);
    const watcher = new ChangeWatcher(bus.db, dbPath, { fsWatch });
    t.after(() => watcher.close());
    const latencies: number[] = [];
    for (let round = 0; round < 5; round += 1) {
      const since = watcher.currentSeq();
      const writer = runWriter(script, dbPath, 300 + round * 37);
      const seq = await watcher.next(since, 10_000);
      const firedAt = Date.now();
      const committedAt = await writer;
      assert.ok(seq > since, "next() returned a newer sequence number");
      latencies.push(firedAt - committedAt);
    }
    t.diagnostic(`latency ms (fired - committed): ${latencies.join(", ")}`);
    for (const latency of latencies) assert.ok(latency < 200, `latency ${latency} ms`);
  });
}

test("changes.next() times out without a change and honours abort", async (t) => {
  const { dbPath, bus } = setup(t);
  const watcher = new ChangeWatcher(bus.db, dbPath);
  t.after(() => watcher.close());
  const seq = watcher.currentSeq();
  const started = Date.now();
  assert.equal(await watcher.next(seq, 300), seq);
  assert.ok(Date.now() - started >= 290);
  const controller = new AbortController();
  setTimeout(() => controller.abort(), 100);
  const abortStarted = Date.now();
  assert.equal(await watcher.next(seq, 10_000, controller.signal), seq);
  assert.ok(Date.now() - abortStarted < 1000);
  assert.ok(await watcher.next(seq - 1, 10) >= seq, "an already newer sequence returns at once");
});

function walSnapshot(path: string): { size: number; mtimeMs: number; sha256: string } {
  const stat = statSync(path);
  return { size: stat.size, mtimeMs: stat.mtimeMs, sha256: createHash("sha256").update(readFileSync(path)).digest("hex") };
}

test(`an idle \`qagent wait\` leaves bus.db-wal unchanged for ${IDLE_MS / 1000} s, then wakes on mail`, { timeout: IDLE_MS + 60_000 }, async (t) => {
  const { dbPath, bus } = setup(t);
  const child = spawn(process.execPath, [QAGENT, "wait", "--timeout", String(Math.ceil(IDLE_MS / 1000) + 60), "--json"], {
    env: cleanEnv({ QAGENT_BUS_DB: dbPath, QAGENT_AGENT_ID: "bob" }), stdio: ["ignore", "pipe", "pipe"],
  });
  let out = "";
  let err = "";
  child.stdout.setEncoding("utf8").on("data", (chunk: string) => { out += chunk; });
  child.stderr.setEncoding("utf8").on("data", (chunk: string) => { err += chunk; });
  const exited = new Promise<number | null>((resolve) => child.on("close", (code) => resolve(code)));
  t.after(() => { if (child.exitCode === null) child.kill("SIGKILL"); });

  const deadline = Date.now() + 10_000;
  while (bus.getAgent("bob")?.status !== "waiting") {
    assert.ok(Date.now() < deadline, `waiter never reached status=waiting: ${err}`);
    await sleep(50);
  }
  await sleep(500);
  const wal = `${dbPath}-wal`;
  assert.ok(existsSync(wal), "the WAL file exists while the waiter holds a connection");
  const before = walSnapshot(wal);
  await sleep(IDLE_MS);
  const after = walSnapshot(wal);
  t.diagnostic(`WAL before ${JSON.stringify(before)} after ${JSON.stringify(after)}`);
  assert.equal(child.exitCode, null, "the waiter is still waiting");
  assert.deepEqual(after, before, "bus.db-wal size, mtime and content unchanged while idle");

  const sentAt = Date.now();
  bus.send(bus.identify("alice"), { to: "bob", subject: "wake up", body: "mail" });
  const code = await exited;
  const wokeMs = Date.now() - sentAt;
  t.diagnostic(`waiter exited ${code} ${wokeMs} ms after the send`);
  assert.equal(code, 0, err);
  const result = JSON.parse(out) as { status: string; messages: { subject: string }[] };
  assert.equal(result.status, "mail");
  assert.equal(result.messages[0].subject, "wake up");
  assert.equal(bus.getAgent("bob")?.storedStatus, "idle", "the waiter clears its waiting status on exit");
});

test("`qagent wait` times out with exit 2, and wakes for a claimable task with exit 0", async (t) => {
  const { dbPath, bus } = setup(t);
  const run = (seconds: string) => new Promise<{ code: number | null; out: string }>((resolve) => {
    const child = spawn(process.execPath, [QAGENT, "wait", "--timeout", seconds, "--json"], { env: cleanEnv({ QAGENT_BUS_DB: dbPath, QAGENT_AGENT_ID: "bob" }), stdio: ["ignore", "pipe", "pipe"] });
    let out = "";
    child.stdout.setEncoding("utf8").on("data", (chunk: string) => { out += chunk; });
    child.on("close", (code) => resolve({ code, out }));
  });
  const timedOut = await run("1");
  assert.equal(timedOut.code, 2);
  assert.equal(JSON.parse(timedOut.out).status, "timeout");
  const pending = run("30");
  while (bus.getAgent("bob")?.status !== "waiting") await sleep(25);
  bus.createTask(bus.identify("alice"), { title: "anyone with role worker", role: "worker" });
  const woke = await pending;
  assert.equal(woke.code, 0);
  assert.equal(JSON.parse(woke.out).status, "task");
});
