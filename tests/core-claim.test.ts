import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { Bus } from "../src/core/bus.js";

const QAGENT = fileURLToPath(new URL("../src/qagent.js", import.meta.url));
const BUS_MODULE = new URL("../src/core/bus.js", import.meta.url).href;
const PROCESSES = 20;

function cleanEnv(extra: Record<string, string>): NodeJS.ProcessEnv {
  const env: NodeJS.ProcessEnv = { ...process.env, ...extra };
  for (const name of ["AGENT_ID", "QAGENT_HOME", "AGENT_BUS_HOME"]) delete env[name];
  return env;
}

interface ChildResult { code: number | null; stdout: string; stderr: string }

function run(args: string[], env: NodeJS.ProcessEnv, onLine?: (line: string) => void): { done: Promise<ChildResult>; release: () => void } {
  const child = spawn(process.execPath, args, { env, stdio: ["pipe", "pipe", "pipe"] });
  let stdout = "";
  let stderr = "";
  child.stdout.setEncoding("utf8").on("data", (chunk: string) => {
    stdout += chunk;
    if (onLine) for (const line of chunk.split("\n")) if (line.trim()) onLine(line.trim());
  });
  child.stderr.setEncoding("utf8").on("data", (chunk: string) => { stderr += chunk; });
  return { done: new Promise((resolve) => child.on("close", (code) => resolve({ code, stdout, stderr }))), release: () => { child.stdin.end("go"); } };
}

/**
 * Each child opens its own connection, reports ready, then blocks (using no CPU) reading
 * stdin until the parent closes every child's stdin at once, then claims.
 */
const CHILD = `
import { readFileSync } from "node:fs";
const { Bus } = await import(process.env.BUS_MODULE);
const bus = Bus.open({ dbPath: process.env.QAGENT_BUS_DB });
const me = bus.identify(process.env.QAGENT_AGENT_ID);
process.stdout.write("ready\\n");
readFileSync(0);
try {
  const task = bus.claimTask(me, Number(process.env.TASK_ID));
  process.stdout.write(JSON.stringify({ won: true, assignee: task.assignee, at: Date.now() }) + "\\n");
} catch (error) {
  process.stdout.write(JSON.stringify({ won: false, code: error.code, message: error.message, at: Date.now() }) + "\\n");
}
bus.close();
`;

test(`${PROCESSES} processes claim one task at once and exactly one wins (3 rounds)`, async (t) => {
  const home = mkdtempSync(join(tmpdir(), "qagent-v2-claim-"));
  t.after(() => rmSync(home, { recursive: true, force: true }));
  const dbPath = join(home, "bus.db");
  const childPath = join(home, "claim-child.mjs");
  writeFileSync(childPath, CHILD);
  const bus = Bus.open({ dbPath });
  t.after(() => bus.close());
  bus.init();
  const operator = bus.identify("operator");
  const workers = Array.from({ length: PROCESSES }, (_, index) => `w${String(index).padStart(2, "0")}`);
  for (const id of workers) bus.addAgent(operator, { id, role: "worker" });

  for (let round = 1; round <= 3; round += 1) {
    const task = bus.createTask(operator, { title: `contested ${round}` });
    let ready = 0;
    let release!: () => void;
    const allReady = new Promise<void>((resolve) => { release = resolve; });
    const children = workers.map((id) => run([childPath], cleanEnv({
      BUS_MODULE, QAGENT_BUS_DB: dbPath, QAGENT_AGENT_ID: id, TASK_ID: String(task.id),
    }), (line) => { if (line === "ready" && ++ready === PROCESSES) release(); }));
    await allReady;
    for (const child of [...children].sort(() => Math.random() - 0.5)) child.release();
    const results = await Promise.all(children.map((child) => child.done));
    for (const result of results) assert.equal(result.code, 0, result.stderr);
    const outcomes = results.map((result) => JSON.parse(result.stdout.trim().split("\n").at(-1)!) as { won: boolean; assignee?: string; code?: string; at: number });
    const winners = outcomes.filter((outcome) => outcome.won);
    const losers = outcomes.filter((outcome) => !outcome.won);
    const spread = Math.max(...outcomes.map((o) => o.at)) - Math.min(...outcomes.map((o) => o.at));
    t.diagnostic(`round ${round}: winner ${winners.map((w) => w.assignee).join(",")}, ${losers.length} conflicts, finish spread ${spread} ms`);
    assert.equal(winners.length, 1, `exactly one winner, got ${winners.length}`);
    assert.equal(losers.length, PROCESSES - 1);
    for (const loser of losers) assert.equal(loser.code, "conflict", "losers see a clean conflict, not SQLITE_BUSY");
    const stored = bus.getTask(task.id);
    assert.equal(stored.state, "claimed");
    assert.equal(stored.assignee, winners[0].assignee);
    const claimEvents = bus.events(0, 5000).filter((event) => event.kind === "task_claimed" && event.entityId === String(task.id));
    assert.equal(claimEvents.length, 1);
  }
});

test(`${PROCESSES} simultaneous \`qagent task claim\` CLI processes produce one success`, async (t) => {
  const home = mkdtempSync(join(tmpdir(), "qagent-v2-claimcli-"));
  t.after(() => rmSync(home, { recursive: true, force: true }));
  const dbPath = join(home, "bus.db");
  const bus = Bus.open({ dbPath });
  t.after(() => bus.close());
  bus.init();
  const operator = bus.identify("operator");
  const workers = Array.from({ length: PROCESSES }, (_, index) => `cli${index}`);
  for (const id of workers) bus.addAgent(operator, { id, role: "worker" });
  const task = bus.createTask(operator, { title: "contested via CLI" });
  const runs = workers.map((id) => run([QAGENT, "task", "claim", String(task.id), "--json"], cleanEnv({ QAGENT_BUS_DB: dbPath, QAGENT_AGENT_ID: id })));
  for (const child of runs) child.release();
  const results = await Promise.all(runs.map((child) => child.done));
  const successes = results.filter((result) => result.code === 0);
  assert.equal(successes.length, 1, results.map((r) => `${r.code} ${r.stderr.trim()}`).join("\n"));
  for (const failure of results.filter((result) => result.code !== 0)) {
    assert.equal(failure.code, 1);
    assert.match(failure.stderr, /cannot be claimed/);
  }
  assert.equal(bus.getTask(task.id).assignee, JSON.parse(successes[0].stdout).assignee);
});
