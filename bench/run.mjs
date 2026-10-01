#!/usr/bin/env node
// One benchmark run: fresh bus, fresh clone at base_sha, roster, tasks, `qagent supervise --roster`, stop on all-terminal or a cap.
//
//   node bench/run.mjs <arm> [--mini] [--fake] [--rep N] [--out DIR] [--source REPO] [--script FILE]
//                      [--no-install] [--no-validate] [--allow-unreconciled-truth] [--bench DIR] [--poll-ms N]
//   node bench/run.mjs validate <run-dir>          integrate, build and run the frozen validators again
//   node bench/run.mjs touch <run-dir> <note>      log a manual action that is not a bus event (metric 4)
//   node bench/run.mjs invalidate <run-dir> <why>  mark a run invalid (infrastructure fault); it is listed, never dropped
import { appendFileSync, existsSync, mkdirSync, openSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { readBus, readSessions } from "./lib/bus.mjs";
import { buildConfig } from "./lib/config.mjs";
import { BENCH_DIR, loadManifest, manifestManager } from "./lib/manifest.mjs";
import { loadTasks } from "./lib/tasks.mjs";
import { validateFake, validateReal } from "./lib/validate.mjs";
import { extractRun } from "./extract.mjs";
import { renderReport } from "./lib/report.mjs";
import { BenchError, git, hashTree, mustGit, parseArgs, readJson, run, sleep } from "./lib/util.mjs";

const REPO_ROOT = join(BENCH_DIR, "..");
const BECOMES_OPEN = new Set(["task_unblocked", "task_released", "task_retry", "claim_expired"]);
const TERMINAL = new Set(["accepted", "failed", "cancelled"]);
const BOOLEANS = ["mini", "fake", "no-install", "no-validate", "allow-unreconciled-truth", "help"];

function writeRun(runDir, record) {
  writeFileSync(join(runDir, "run.json"), `${JSON.stringify(record, null, 2)}\n`);
}

function scratchRepo(dir) {
  mkdirSync(join(dir, "src"), { recursive: true });
  writeFileSync(join(dir, "README.md"), "# scratch\nfake benchmark repository\nthree lines long\n");
  writeFileSync(join(dir, "src", "a.txt"), "a\n");
  mustGit(dir, ["init", "--quiet", "-b", "main"]);
  mustGit(dir, ["config", "user.name", "bench"]);
  mustGit(dir, ["config", "user.email", "bench@localhost"]);
  mustGit(dir, ["add", "-A"]);
  mustGit(dir, ["commit", "--quiet", "-m", "scratch"]);
  return mustGit(dir, ["rev-parse", "HEAD"]);
}

function cannedReport(truth) {
  const ids = truth.items.map((item) => item.id);
  return JSON.stringify({
    items: [...ids.slice(0, -1), "not-in-truth"],
    claims: [
      { text: "claim with evidence that resolves", evidence: ["README.md:1"] },
      { text: "claim whose line does not exist", evidence: ["README.md:99"] },
      { text: "claim with a link only", evidence: ["https://example.com/page"] },
    ],
    unresolved: ["one open question"],
  });
}

class Bus {
  constructor(bin, home, base) {
    this.bin = bin;
    const env = { ...process.env, ...base, QAGENT_HOME: home, QAGENT_BUS_DB: join(home, "bus.db") };
    for (const name of ["QAGENT_AGENT_ID", "AGENT_ID", "AGENT_BUS_HOME", "QAGENT_BLOCK_SEC", "QAGENT_CONFIG", "AGENT_BUS_CONFIG"]) delete env[name];
    this.env = env;
  }

  q(agent, args) {
    const env = { ...this.env, ...(agent ? { QAGENT_AGENT_ID: agent } : {}) };
    const result = run(process.execPath, [this.bin, ...args], { env });
    if (result.code !== 0) throw new BenchError(`qagent ${args.slice(0, 3).join(" ")} (as ${agent ?? "operator"}) failed: ${result.stderr.trim() || result.stdout.trim()}`);
    return result.stdout;
  }

  json(agent, args) {
    return JSON.parse(this.q(agent, [...args, "--json"]));
  }
}

function preambleFor(benchRoot, task) {
  const path = join(benchRoot, "preamble", `${task.kind}.md`);
  return existsSync(path) ? readFileSync(path, "utf8").trim() : "";
}

function loadScript(path) {
  return path ? readJson(resolve(path)) : { tasks: {} };
}

async function startRun(armName, flags) {
  const benchRoot = resolve(flags.bench ?? BENCH_DIR);
  const manifest = loadManifest(join(benchRoot, "MANIFEST.json"));
  const arm = manifest.arms[armName];
  if (!arm) throw new BenchError(`unknown arm "${armName}"; the manifest defines: ${Object.keys(manifest.arms).join(", ")}`);
  const fake = flags.fake === true;
  const size = flags.mini === true ? "mini" : "endurance";
  const all = loadTasks(join(benchRoot, "tasks"));
  const tasks = size === "mini" ? all.filter((task) => task.mini) : all;
  for (const task of tasks) for (const dep of task.depends) if (!tasks.some((other) => other.id === dep)) throw new BenchError(`${task.id} depends on ${dep}, which is not in the ${size} set`);
  const researchTasks = tasks.filter((task) => task.kind === "research");
  const truthFiles = Object.fromEntries(researchTasks.map((task) => [task.id, join(benchRoot, task.truth)]));
  const unreconciled = researchTasks.filter((task) => readJson(truthFiles[task.id]).reconciled !== true).map((task) => task.id);
  if (!fake && unreconciled.length && flags["allow-unreconciled-truth"] !== true) {
    throw new BenchError(`truth for ${unreconciled.join(", ")} is not reconciled. The spec requires a second, independent derivation before the first run: set "reconciled": true in each bench/truth file once it is, or pass --allow-unreconciled-truth to run anyway (the report then says so).`);
  }

  const qagentBin = resolve(arm.qagent ?? join(REPO_ROOT, "dist", "qagent.js"));
  if (!existsSync(qagentBin)) throw new BenchError(`${qagentBin} not found: run npm run build in the ACS checkout`);
  const isolation = manifest.isolation ?? "worktree";
  const config = await buildConfig({ manifest, qagentBin, fake, isolation, overrides: arm.configOverrides ?? {} });

  const harnessVersions = {};
  if (!fake) {
    for (const harness of Object.values(config.harnesses).filter((candidate) => candidate.enabled)) {
      const probe = run(harness.command, harness.probeArgs ?? ["--version"]);
      if (probe.code !== 0) throw new BenchError(`${harness.command} did not run (${probe.error?.message ?? probe.stderr.trim()}). Install it and log in before a real run; see bench/README.md.`);
      harnessVersions[harness.id] = probe.stdout.trim().split("\n")[0];
    }
  }

  const stamp = new Date().toISOString().replace(/[:.]/g, "-");
  const repetition = Number(flags.rep ?? 1);
  const runDir = join(resolve(flags.out ?? "bench-runs"), manifest.series, `${armName}-${repetition}-${stamp}`);
  const home = join(runDir, "home");
  const workdir = join(runDir, "workdir");
  mkdirSync(home, { recursive: true });
  const script = loadScript(flags.script);
  let baseSha;
  if (fake) {
    baseSha = scratchRepo((mkdirSync(workdir, { recursive: true }), workdir));
  } else {
    mustGit(runDir, ["clone", "--quiet", flags.source ?? manifest.repo, workdir]);
    mustGit(workdir, ["checkout", "--quiet", "-B", "bench/base", manifest.base_sha]);
    baseSha = mustGit(workdir, ["rev-parse", "HEAD"]);
    if (existsSync(join(workdir, "bench"))) throw new BenchError(`${manifest.base_sha} already contains bench/: the agents would be able to read the validators and truth. Choose a base commit from before bench/ was added.`);
    if (flags["no-install"] !== true) {
      const install = run("npm", ["ci", "--no-audit", "--no-fund"], { cwd: workdir });
      if (install.code !== 0) throw new BenchError(`npm ci failed in the clone: ${install.stderr.trim().split("\n").slice(-5).join("\n")}`);
    }
  }
  const configPath = join(runDir, "config.json");
  writeFileSync(configPath, `${JSON.stringify(config, null, 2)}\n`);

  const bus = new Bus(qagentBin, home, { ...(arm.env ?? {}), ...(fake ? { FAKE_HARNESS_REPORTS: join(runDir, "fake-reports") } : {}) });
  if (fake) {
    mkdirSync(join(runDir, "fake-reports"));
    for (const task of researchTasks) writeFileSync(join(runDir, "fake-reports", `${task.id}.txt`), cannedReport(readJson(truthFiles[task.id])));
  }
  bus.q(null, ["init"]);
  const manager = manifestManager(manifest);
  for (const agent of manifest.roster) {
    const resolved = config.agents[agent.id];
    const args = ["agent", "add", agent.id, "--role", agent.role, "--authority", agent.authority, "--model", resolved.model, "--harness", config.models[resolved.model].harness];
    if (agent.id !== manager.id) args.push("--parent", manager.id);
    bus.q(null, args);
  }
  const ids = {};
  for (const task of tasks) {
    const args = ["task", "add", `${task.id}: ${task.title}`, "--brief", [preambleFor(benchRoot, task), task.brief].filter(Boolean).join("\n\n"), "--acceptance", task.acceptance, "--role", task.role, "--project", workdir];
    for (const dep of task.depends) args.push("--dep", String(ids[dep]));
    ids[task.id] = bus.json(manager.id, args).id;
  }

  const budget = manifest.budgets[size];
  const hashesStart = { validate: hashTree(join(benchRoot, "validate")), truth: hashTree(join(benchRoot, "truth")) };
  const record = {
    schema: 1, arm: armName, series: manifest.series, mode: fake ? "fake" : "real", size, repetition, baseSha, benchRoot, isolation,
    qagent: { bin: qagentBin, commit: git(dirname(dirname(qagentBin)), ["rev-parse", "HEAD"]).stdout.trim() || null },
    harnessVersions,
    roster: manifest.roster.map((agent) => ({ id: agent.id, role: agent.role, authority: agent.authority, family: agent.family, provider: agent.provider, exactModel: config.models[config.agents[agent.id].model].exactModel ?? null })),
    taskKeys: tasks.map((task) => task.id), taskIds: ids, truthFiles,
    caps: { runUsd: budget.runUsd, wallMinutes: budget.wallMinutes, taskMinutes: manifest.budgets.taskMinutes },
    thresholds: { stuckMs: (manifest.stuckThresholdMin ?? 10) * 60_000 }, auditSize: manifest.audit?.sample ?? 8, seed: manifest.seed,
    truth: unreconciled.length ? "unreconciled" : "reconciled", hashes: { start: hashesStart },
    startedMs: null, endedMs: null, endCause: null, capped: [], invalid: null,
  };
  writeRun(runDir, record);

  const log = openSync(join(runDir, "supervisor.log"), "a");
  const { spawn } = await import("node:child_process");
  const supervisor = spawn(process.execPath, [qagentBin, "supervise", "--roster", workdir, "--config", configPath], { env: bus.env, stdio: ["ignore", log, log], detached: true });
  const exited = new Promise((resolveExit) => supervisor.on("exit", resolveExit));
  let endCause = null;
  const stop = (cause) => { endCause ??= cause; };
  process.on("SIGINT", () => stop("aborted"));
  process.on("SIGTERM", () => stop("aborted"));
  supervisor.on("exit", (code) => stop(`supervisor-exited-${code}`));

  const dbPath = join(home, "bus.db");
  for (let waited = 0; ; waited += 500) {
    const agents = bus.json(null, ["agent", "list"]);
    if (manifest.roster.every((agent) => agents.find((row) => row.id === agent.id)?.status === "waiting")) break;
    if (endCause || waited > 120_000) { killGroup(supervisor, "SIGKILL"); throw new BenchError(`supervisors did not come up (see ${join(runDir, "supervisor.log")})`); }
    await sleep(500);
  }

  record.startedMs = Date.now();
  writeRun(runDir, record);
  process.stdout.write(`run ${runDir}\n${fake ? "fake harness" : "real harness"}, arm ${armName}, ${tasks.length} tasks, caps ${budget.runUsd} USD / ${budget.wallMinutes} min / ${manifest.budgets.taskMinutes} min per task\n`);

  const dispatched = new Map();
  const turn = { implementation: 0, research: 0 };
  const workers = manifest.roster.filter((agent) => agent.role === "implementation" || agent.role === "research");
  const pollMs = Number(flags["poll-ms"] ?? (fake ? 250 : 2000));
  const capped = new Set();
  while (!endCause) {
    const snapshot = readBus(dbPath);
    const keyed = snapshot.tasks.filter((task) => task.key);
    dispatch(bus, manager, workers, keyed, snapshot, dispatched, turn);
    if (fake) reviewFake(bus, keyed, script);
    for (const task of keyed) {
      const claimed = snapshot.events.filter((event) => event.entity === "task" && Number(event.entityId) === task.id && event.kind === "task_claimed").at(-1);
      if (claimed && !TERMINAL.has(task.state) && Date.now() - claimed.tsMs > record.caps.taskMinutes * 60_000) capped.add(task.key);
    }
    const settled = record.taskKeys.every((key) => capped.has(key) || TERMINAL.has(keyed.find((task) => task.key === key)?.state));
    const spent = Object.values(readSessions(home)).reduce((total, row) => total + (row.costUSD ?? 0), 0);
    if (settled) stop("all-terminal");
    else if (spent >= budget.runUsd) stop("run-usd-cap");
    else if (Date.now() - record.startedMs >= budget.wallMinutes * 60_000) stop("wall-cap");
    else await sleep(pollMs);
  }

  killGroup(supervisor, "SIGTERM");
  const settledExit = await Promise.race([exited, sleep(20_000).then(() => "timeout")]);
  if (settledExit === "timeout") killGroup(supervisor, "SIGKILL");
  record.endedMs = Date.now();
  record.endCause = endCause;
  record.capped = [...capped];
  record.hashes.end = { validate: hashTree(join(benchRoot, "validate")), truth: hashTree(join(benchRoot, "truth")) };
  if (record.hashes.end.validate !== hashesStart.validate || record.hashes.end.truth !== hashesStart.truth) {
    record.invalid = { reason: "bench/validate or bench/truth changed during the run", ms: record.endedMs };
  }
  writeRun(runDir, record);

  if (flags["no-validate"] !== true) validateRunDir(runDir, record, tasks, script);
  const metrics = extractRun(runDir);
  writeFileSync(join(runDir, "report.md"), `${renderReport([metrics])}\n`);
  process.stdout.write(`ended: ${record.endCause}; accepted ${metrics.tasks.accepted}/${metrics.tasks.total}; metrics ${join(runDir, "metrics.json")}\n`);
  return runDir;
}

function killGroup(child, signal) {
  if (!child.pid) return;
  try { process.kill(-child.pid, signal); } catch { try { child.kill(signal); } catch { /* gone */ } }
}

/** The task feeder: one task per worker at a time, sent as a [TASK #n] mail from the manager. Identical in every arm. */
function dispatch(bus, manager, workers, keyed, snapshot, dispatched, turn) {
  const busy = (worker) => keyed.some((task) =>
    (task.assignee === worker.id && (task.state === "claimed" || task.state === "changes_requested"))
    || (dispatched.get(task.id)?.worker === worker.id && task.state === "open" && task.assignee === null));
  for (const task of keyed) {
    if (task.state !== "open" || task.assignee !== null) continue;
    const opened = snapshot.events
      .filter((event) => event.entity === "task" && Number(event.entityId) === task.id && (event.kind === "task_created" ? event.data?.state === "open" : BECOMES_OPEN.has(event.kind)))
      .reduce((latest, event) => Math.max(latest, event.tsMs), 0);
    const previous = dispatched.get(task.id);
    if (previous && previous.tsMs >= opened) continue;
    const free = workers.filter((worker) => worker.role === task.role && !busy(worker));
    if (!free.length) continue;
    const worker = free[turn[task.role]++ % free.length];
    const body = [task.brief, task.acceptance ? `Acceptance:\n${task.acceptance}` : "", `Claim with \`qagent task claim ${task.id}\`, submit with \`qagent task submit ${task.id} --summary ...\`.`].filter(Boolean).join("\n\n");
    bus.q(manager.id, ["send", worker.id, `[TASK #${task.id}] ${task.title}`, body, "--task", String(task.id), "--type", "task"]);
    dispatched.set(task.id, { worker: worker.id, tsMs: Date.now() });
  }
}

/** Stand-in for the manager's judgement in fake runs: verdicts come from the script, by round. */
function reviewFake(bus, keyed, script) {
  for (const task of keyed.filter((candidate) => candidate.state === "submitted")) {
    const verdicts = script.tasks?.[task.key]?.verdicts ?? ["accept"];
    const verdict = verdicts[Math.min(task.round - 1, verdicts.length - 1)];
    const reviewer = task.reviewer ?? task.creator;
    bus.q(reviewer, ["task", "review", String(task.id), verdict === "accept" ? "--accept" : "--revise", "--feedback", `scripted ${verdict} (round ${task.round})`]);
  }
}

function validateRunDir(runDir, record, tasks, script) {
  if (record.mode === "fake") return validateFake({ runDir, runRecord: record, tasks, script });
  return validateReal({ runDir, runRecord: record, tasks });
}

async function main() {
  const { flags, positionals } = parseArgs(process.argv.slice(2), BOOLEANS);
  const [command, ...rest] = positionals;
  if (!command || flags.help) {
    process.stdout.write(readFileSync(import.meta.filename, "utf8").split("\n").filter((line) => line.startsWith("//")).map((line) => line.slice(3)).join("\n").concat("\n"));
    return;
  }
  if (command === "touch" || command === "invalidate") {
    const [runDir, ...words] = rest;
    if (!runDir || !words.length) throw new BenchError(`usage: node bench/run.mjs ${command} <run-dir> <${command === "touch" ? "note" : "reason"}>`);
    const dir = resolve(runDir);
    if (command === "touch") appendFileSync(join(dir, "touches.jsonl"), `${JSON.stringify({ tsMs: Date.now(), note: words.join(" ") })}\n`);
    else {
      const record = readJson(join(dir, "run.json"));
      record.invalid = { reason: words.join(" "), ms: Date.now() };
      writeRun(dir, record);
    }
    return;
  }
  if (command === "validate") {
    const dir = resolve(rest[0] ?? "");
    const record = readJson(join(dir, "run.json"));
    const tasks = loadTasks(join(record.benchRoot, "tasks"));
    const script = loadScript(flags.script);
    const result = validateRunDir(dir, record, tasks, script);
    process.stdout.write(`${Object.entries(result.validators).map(([key, row]) => `${key}: ${row.passed ? "pass" : "FAIL"}`).join("\n")}\n`);
    return;
  }
  await startRun(command, flags);
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().then(() => process.exit(0), (error) => {
    process.stderr.write(`${error instanceof BenchError ? error.message : error.stack}\n`);
    process.exit(1);
  });
}
