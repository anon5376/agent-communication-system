import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { DatabaseSync } from "node:sqlite";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { SCHEMA_SQL } from "../src/core/db.js";

const BENCH = join(process.cwd(), "bench");
const MIN = 60_000;
const T0 = 1_800_000_000_000;

function node(args: string[], options: { cwd?: string; env?: NodeJS.ProcessEnv } = {}) {
  const result = spawnSync(process.execPath, ["--no-warnings", ...args], { encoding: "utf8", ...options });
  return { status: result.status, stdout: result.stdout, stderr: result.stderr };
}

function git(cwd: string, args: string[]): string {
  const result = spawnSync("git", args, { cwd, encoding: "utf8" });
  assert.equal(result.status, 0, `git ${args.join(" ")}: ${result.stderr}`);
  return result.stdout.trim();
}

/**
 * A bus built by hand so every metric has a known value (minutes after T0, run starts at 0):
 *   I01  wa (claude)  claimed 1, note 3, submitted 5, accepted 6 by mgr (claude)         round 1, same family
 *   I02  wb (gpt)     claimed 2, silent until note 20, submitted 22, revised 23,
 *                     claimed 24, submitted 30, accepted 31 by mgr                       round 2, cross family, 18 min stall
 *   I03  never claimed                                                                   open for the whole run
 *   R01  ra (gpt)     claimed 4, submitted 10 with a report, accepted 12 by the operator round 1, one human touch
 */
function buildRun(root: string, arm: string, invalid: { reason: string; ms: number } | null = null): string {
  const runDir = join(root, arm);
  const home = join(runDir, "home");
  const workdir = join(runDir, "workdir");
  mkdirSync(join(home, "sessions"), { recursive: true });
  mkdirSync(workdir, { recursive: true });
  writeFileSync(join(workdir, "README.md"), "line one\nline two\nline three\n");
  git(workdir, ["init", "--quiet", "-b", "main"]);
  git(workdir, ["config", "user.name", "t"]);
  git(workdir, ["config", "user.email", "t@localhost"]);
  git(workdir, ["add", "-A"]);
  git(workdir, ["commit", "--quiet", "-m", "base"]);
  const baseSha = git(workdir, ["rev-parse", "HEAD"]);

  const db = new DatabaseSync(join(home, "bus.db"));
  db.exec(SCHEMA_SQL);
  const report = JSON.stringify({
    items: ["alpha", "Beta", "epsilon"],
    claims: [
      { text: "resolves", evidence: ["README.md:1"] },
      { text: "line out of range", evidence: ["README.md:99"] },
      { text: "link only", evidence: ["https://example.com/page"] },
    ],
    unresolved: ["one open question"],
  });
  const insertTask = db.prepare(`INSERT INTO tasks(id, title, state, creator, assignee, reviewer, role, result_json, review_json, round, created_ms, updated_ms)
    VALUES(?, ?, ?, 'mgr', ?, NULL, ?, ?, ?, ?, ?, ?)`);
  const review = (reviewer: string) => JSON.stringify({ reviewer, accepted: true, feedback: "ok", reviewedMs: T0 });
  insertTask.run(1, "I01: first", "accepted", "wa", "implementation", JSON.stringify({ summary: "done", changedFiles: ["README.md"] }), review("mgr"), 1, T0 - MIN, T0 + 6 * MIN);
  insertTask.run(2, "I02: second", "accepted", "wb", "implementation", JSON.stringify({ summary: "done too" }), review("mgr"), 2, T0 - MIN, T0 + 31 * MIN);
  insertTask.run(3, "I03: third", "open", null, "implementation", null, null, 1, T0 - MIN, T0 - MIN);
  insertTask.run(4, "R01: question", "accepted", "ra", "research", JSON.stringify({ summary: "answer", details: `Report:\n${report}` }), review("operator"), 1, T0 - MIN, T0 + 12 * MIN);
  const insertEvent = db.prepare("INSERT INTO events(ts_ms, actor, kind, entity, entity_id, data_json) VALUES(?, ?, ?, 'task', ?, ?)");
  const event = (minute: number, actor: string, kind: string, id: number, data: object = {}) => insertEvent.run(T0 + minute * MIN, actor, kind, String(id), JSON.stringify(data));
  for (const id of [1, 2, 3, 4]) event(-1, "mgr", "task_created", id, { state: "open" });
  event(1, "wa", "task_claimed", 1); event(3, "wa", "task_note", 1); event(5, "wa", "task_submitted", 1); event(6, "mgr", "task_accepted", 1, { round: 1 });
  event(2, "wb", "task_claimed", 2); event(20, "wb", "task_note", 2); event(22, "wb", "task_submitted", 2); event(23, "mgr", "task_changes_requested", 2, { round: 2 });
  event(24, "wb", "task_claimed", 2); event(30, "wb", "task_submitted", 2); event(31, "mgr", "task_accepted", 2, { round: 2 });
  event(4, "ra", "task_claimed", 4); event(10, "ra", "task_submitted", 4); event(12, "operator", "task_accepted", 4, { round: 1 });
  const insertMessage = db.prepare("INSERT INTO messages(id, ts_ms, sender, recipient, body) VALUES(?, ?, 'mgr', 'wa', 'x')");
  for (let n = 0; n < 6; n += 1) insertMessage.run(`m${n}`, T0 + n);
  db.close();

  const session = (turns: number, inputTokens: number, outputTokens: number, costUSD: number) => JSON.stringify({ sessionId: null, turns, inputTokens, outputTokens, totalTokens: inputTokens + outputTokens, costUSD, latencyMs: 1 });
  writeFileSync(join(home, "sessions", "mgr.json"), session(3, 1000, 500, 0.5));
  writeFileSync(join(home, "sessions", "wa.json"), session(1, 2000, 1000, 1.25));
  writeFileSync(join(home, "sessions", "wb.json"), session(2, 3000, 1500, 2));
  writeFileSync(join(home, "sessions", "ra.json"), session(1, 500, 500, 0.25));
  writeFileSync(join(runDir, "validators.json"), JSON.stringify({ I01: { passed: true }, I02: { passed: false }, I03: { passed: false } }));
  writeFileSync(join(runDir, "grades.json"), JSON.stringify({ I01: "correct", I02: "correct", R01: "wrong" }));
  writeFileSync(join(runDir, "touches.jsonl"), `${JSON.stringify({ tsMs: T0 + MIN, note: "approved a permission prompt" })}\n${JSON.stringify({ tsMs: T0 + 2 * MIN, note: "restarted the VPN" })}\n`);
  const truthFile = join(root, "R01.truth.json");
  writeFileSync(truthFile, JSON.stringify({ id: "R01", reconciled: true, items: [{ id: "alpha", aliases: ["a1"] }, { id: "beta" }, { id: "gamma" }, { id: "delta" }] }));
  writeFileSync(join(runDir, "run.json"), JSON.stringify({
    schema: 1, arm, series: "unit", mode: "real", size: "mini", repetition: 1, baseSha, benchRoot: BENCH, isolation: "worktree",
    roster: [
      { id: "mgr", role: "manager", authority: "manager", family: "claude" },
      { id: "wa", role: "implementation", authority: "worker", family: "claude" },
      { id: "wb", role: "implementation", authority: "worker", family: "gpt" },
      { id: "ra", role: "research", authority: "worker", family: "gpt" },
    ],
    taskKeys: ["I01", "I02", "I03", "R01"], truthFiles: { R01: truthFile },
    caps: { runUsd: 12, wallMinutes: 150, taskMinutes: 45 }, thresholds: { stuckMs: 10 * MIN }, auditSize: 3, seed: 7,
    truth: "reconciled", startedMs: T0, endedMs: T0 + 40 * MIN, endCause: "wall-cap", capped: [], invalid,
  }));
  return runDir;
}

test("extract computes metrics 1-10 from a hand-built bus", () => {
  const root = mkdtempSync(join(tmpdir(), "bench-metrics-"));
  try {
    const runDir = buildRun(root, "baseline");
    const result = node([join(BENCH, "extract.mjs"), runDir]);
    assert.equal(result.status, 0, result.stderr);
    const m = JSON.parse(readFileSync(join(runDir, "metrics.json"), "utf8"));
    assert.deepEqual(m.tasks, { total: 4, accepted: 3, acceptanceRate: 0.75, firstRoundAccepted: 2, firstRoundRate: 0.5, failed: 0, cancelled: 0, unfinished: ["I03"], overCap: [] });
    assert.deepEqual(m.crossFamily, { accepted: 1, rate: 0.25, acceptedByOperator: 1, unknownFamily: 0 });
    assert.equal(m.stuck.minutes, 18);
    assert.equal(m.stuck.longestMinutes, 18);
    assert.deepEqual(m.stuck.stalls, [{ task: "I02", minutes: 18 }]);
    assert.equal(m.stuck.queuedMinutes, 51);
    assert.deepEqual(m.humanTouches, { count: 3, operatorEvents: 1, logged: 2 });
    assert.equal(m.cost.source, "session-files");
    assert.equal(m.cost.granularity, "agent");
    assert.equal(m.cost.usd, 4);
    assert.equal(m.cost.tokens, 10_000);
    assert.equal(m.cost.turns, 7);
    assert.deepEqual(m.defectEscape, { acceptedImplementation: 2, failedValidator: ["I02"], rate: 0.5 });
    assert.deepEqual(m.audit.sample, ["I01", "I02", "R01"]);
    assert.equal(m.audit.graded, 3);
    assert.equal(m.audit.agreement, 0.5);
    assert.deepEqual(m.research.R01, { reportFound: true, claims: 3, claimsWithEvidence: 3, verifiedEvidence: 1, evidenceRate: 1 / 3, precision: 2 / 3, recall: 0.5, reported: 3, unresolved: 1 });
    assert.deepEqual(m.coordination, { messages: 6, turns: 7, messagesPerAccepted: 2, turnsPerAccepted: 2.33, coordinationTokenShare: 0.15 });
    assert.equal(m.collisions, null);
    assert.equal(m.perTask.I02.round, 2);
    assert.match(readFileSync(join(runDir, "audit-sheet.md"), "utf8"), /I01: first/);
    const report = readFileSync(join(runDir, "report.md"), "utf8");
    assert.match(report, /\| baseline \| 1 \| 75% \| 50% \| 25% \| 18 \| 3 \| \$4 \| 50% \|/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("the report separates arms, shows ranges, and lists invalid runs without counting them", () => {
  const root = mkdtempSync(join(tmpdir(), "bench-report-"));
  try {
    const a = buildRun(root, "a");
    const b = buildRun(root, "b");
    const c = buildRun(root, "c", { reason: "vendor outage over 10 minutes", ms: T0 });
    const result = node([join(BENCH, "extract.mjs"), a, b, c, "--out", join(root, "report.md")]);
    assert.equal(result.status, 0, result.stderr);
    const report = readFileSync(join(root, "report.md"), "utf8");
    assert.match(report, /\| a \| 1 \|/);
    assert.match(report, /\| b \| 1 \|/);
    assert.doesNotMatch(report, /\| c \| 1 \|/);
    assert.match(report, /## Invalid runs[\s\S]*c #1: vendor outage over 10 minutes/);
    assert.match(report, /fewer than 3 runs is noisy/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("validate integrates accepted task branches, reports collisions, and runs the frozen validators on the result", () => {
  const root = mkdtempSync(join(tmpdir(), "bench-integrate-"));
  try {
    const benchRoot = join(root, "bench");
    mkdirSync(join(benchRoot, "tasks"), { recursive: true });
    mkdirSync(join(benchRoot, "validate"));
    const task = (id: string, scope: string, title: string) => writeFileSync(join(benchRoot, "tasks", `${id}-x.md`),
      `---\nid: ${id}\nkind: implementation\nrole: implementation\ntitle: ${title}\nscope: [${scope}]\nvalidator: validate/${id}.mjs\n---\n\n## Brief\nb\n\n## Acceptance\na\n`);
    task("I01", "a.txt", "one"); task("I02", "b.txt", "two"); task("I03", "c.txt", "three");
    const validator = (id: string, file: string, content: string) => writeFileSync(join(benchRoot, "validate", `${id}.mjs`),
      `import { readFileSync } from "node:fs";\nimport { join } from "node:path";\nlet text = "";\ntry { text = readFileSync(join(process.env.BENCH_CHECKOUT, ${JSON.stringify(file)}), "utf8").trim(); } catch {}\nprocess.exit(text === ${JSON.stringify(content)} ? 0 : 1);\n`);
    validator("I01", "a.txt", "one"); validator("I02", "b.txt", "two"); validator("I03", "c.txt", "three");

    const runDir = join(root, "run");
    const workdir = join(runDir, "workdir");
    const home = join(runDir, "home");
    mkdirSync(workdir, { recursive: true });
    mkdirSync(home);
    git(workdir, ["init", "--quiet", "-b", "main"]);
    git(workdir, ["config", "user.name", "t"]);
    git(workdir, ["config", "user.email", "t@localhost"]);
    mkdirSync(join(workdir, "dist"));
    writeFileSync(join(workdir, "a.txt"), "zero\n");
    writeFileSync(join(workdir, "dist", "out.js"), "base\n");
    git(workdir, ["add", "-A"]);
    git(workdir, ["commit", "--quiet", "-m", "base"]);
    const baseSha = git(workdir, ["rev-parse", "HEAD"]);
    const branch = (id: number, files: Record<string, string>) => {
      git(workdir, ["checkout", "--quiet", "-b", `qagent/task-${id}-abc`, baseSha]);
      for (const [name, text] of Object.entries(files)) writeFileSync(join(workdir, name), text);
      git(workdir, ["add", "-A"]);
      git(workdir, ["commit", "--quiet", "-m", `task ${id}`]);
    };
    branch(1, { "a.txt": "one\n", "dist/out.js": "from one\n" });
    branch(2, { "a.txt": "two-conflicts\n", "b.txt": "two\n", "other.txt": "outside scope\n" });
    branch(3, { "c.txt": "three\n", "dist/out.js": "from three\n" });
    git(workdir, ["checkout", "--quiet", "main"]);

    const db = new DatabaseSync(join(home, "bus.db"));
    db.exec(SCHEMA_SQL);
    const insertTask = db.prepare("INSERT INTO tasks(id, title, state, creator, assignee, role, round, created_ms, updated_ms) VALUES(?, ?, 'accepted', 'mgr', 'w', 'implementation', 1, ?, ?)");
    const insertEvent = db.prepare("INSERT INTO events(ts_ms, actor, kind, entity, entity_id, data_json) VALUES(?, 'mgr', 'task_accepted', 'task', ?, '{\"round\":1}')");
    for (const id of [1, 2, 3]) { insertTask.run(id, `I0${id}: t`, T0, T0); insertEvent.run(T0 + id, String(id)); }
    db.close();
    writeFileSync(join(runDir, "run.json"), JSON.stringify({ benchRoot, baseSha, isolation: "worktree", taskKeys: ["I01", "I02", "I03"] }));

    const result = node([join(BENCH, "run.mjs"), "validate", runDir]);
    assert.equal(result.status, 0, result.stderr);
    const integration = JSON.parse(readFileSync(join(runDir, "integration.json"), "utf8"));
    assert.deepEqual(integration.mergeConflicts, [{ key: "I02", files: ["a.txt"] }]);
    assert.deepEqual(integration.outsideScope, { I02: ["a.txt", "other.txt"] });
    assert.deepEqual(integration.distConflictsResolved, ["I03"]);
    assert.deepEqual(integration.overlapPairs, [{ a: "I01", b: "I02", files: ["a.txt"] }]);
    assert.deepEqual(integration.noBranch, []);
    const validators = JSON.parse(readFileSync(join(runDir, "validators.json"), "utf8"));
    assert.deepEqual(Object.fromEntries(Object.entries(validators).map(([key, row]) => [key, (row as { passed: boolean }).passed])), { I01: true, I02: false, I03: true });
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

function stableProjection(metrics: any) {
  const research: Record<string, unknown> = {};
  for (const [key, row] of Object.entries<any>(metrics.research)) {
    research[key] = { reportFound: row.reportFound, claims: row.claims, claimsWithEvidence: row.claimsWithEvidence, verifiedEvidence: row.verifiedEvidence };
  }
  const perTask: Record<string, unknown> = {};
  for (const [key, row] of Object.entries<any>(metrics.perTask)) {
    perTask[key] = { state: row.state, round: row.round, accepted: row.accepted, validatorPassed: row.validatorPassed };
  }
  const { total, accepted, firstRoundAccepted, failed, cancelled, unfinished } = metrics.tasks;
  return { tasks: { total, accepted, firstRoundAccepted, failed, cancelled, unfinished }, defectEscape: metrics.defectEscape, research, perTask, endCause: metrics.run.endCause };
}

function fakeRun(out: string, script: string | null): any {
  const args = [join(BENCH, "run.mjs"), "baseline", "--fake", "--mini", "--out", out];
  if (script) args.push("--script", script);
  const result = node(args);
  assert.equal(result.status, 0, result.stderr + result.stdout);
  const series = join(out, "series-1");
  const [dir] = readdirSync(series);
  assert.ok(existsSync(join(series, dir, "report.md")));
  return JSON.parse(readFileSync(join(series, dir, "metrics.json"), "utf8"));
}

test("fake mini run scores to the golden projection", () => {
  const root = mkdtempSync(join(tmpdir(), "bench-fake-"));
  try {
    const golden = join(BENCH, "fixtures", "fake-mini.metrics.json");
    const actual = stableProjection(fakeRun(join(root, "scripted"), join(BENCH, "fixtures", "fake-mini.script.json")));
    if (process.env.BENCH_UPDATE_GOLDEN) writeFileSync(golden, `${JSON.stringify(actual, null, 2)}\n`);
    assert.deepEqual(actual, JSON.parse(readFileSync(golden, "utf8")));
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("fake mini run without a script accepts every task", () => {
  const root = mkdtempSync(join(tmpdir(), "bench-fake-"));
  try {
    const metrics = fakeRun(join(root, "plain"), null);
    assert.equal(metrics.tasks.total, 8);
    assert.equal(metrics.tasks.accepted, 8);
    assert.equal(metrics.run.endCause, "all-terminal");
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
