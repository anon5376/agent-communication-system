#!/usr/bin/env node
// Metrics for finished runs: node bench/extract.mjs <run-dir>... [--out report.md]
// Writes metrics.json (and audit-sheet.md) into each run directory and one report.md.
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { readBus, readSessions } from "./lib/bus.mjs";
import { computeMetrics } from "./lib/metrics.mjs";
import { renderReport } from "./lib/report.mjs";
import { extractReport, scoreResearch } from "./lib/research.mjs";
import { BenchError, git, parseArgs, readJson } from "./lib/util.mjs";

function optionalJson(path) {
  return existsSync(path) ? readJson(path) : null;
}

function touchesOf(runDir) {
  const path = join(runDir, "touches.jsonl");
  if (!existsSync(path)) return [];
  return readFileSync(path, "utf8").split("\n").filter(Boolean).map((line) => JSON.parse(line));
}

function researchScores(bus, run, runDir) {
  const keys = run.taskKeys.filter((key) => key.startsWith("R"));
  if (!keys.length) return null;
  const readFile = (path) => {
    const result = git(join(runDir, "workdir"), ["show", `${run.baseSha}:${path}`]);
    return result.code === 0 ? result.stdout : null;
  };
  const scores = {};
  for (const key of keys) {
    const task = bus.tasks.find((candidate) => candidate.key === key);
    const truth = readJson(run.truthFiles[key]);
    scores[key] = scoreResearch(task ? extractReport(task.result) : null, truth, readFile);
  }
  return scores;
}

function auditSheet(bus, metrics) {
  const lines = ["# Blind audit sheet", "", "Grade each task `correct`, `partly` or `wrong` in grades.json, e.g. `{ \"I01\": \"correct\" }`. The arm and the reviewer are left out on purpose.", ""];
  for (const key of metrics.audit.sample) {
    const task = bus.tasks.find((candidate) => candidate.key === key);
    if (!task) continue;
    lines.push(`## ${task.title}`, "", `Summary: ${task.result?.summary ?? "(none)"}`, "", `Changed files: ${(task.result?.changedFiles ?? []).join(", ") || "(none reported)"}`, "");
  }
  return lines.join("\n");
}

export function extractRun(runDir) {
  const run = readJson(join(runDir, "run.json"));
  const bus = readBus(join(runDir, "home", "bus.db"));
  const metrics = computeMetrics({
    bus, run,
    sessions: readSessions(join(runDir, "home")),
    validators: optionalJson(join(runDir, "validators.json")),
    research: researchScores(bus, run, runDir),
    integration: optionalJson(join(runDir, "integration.json")),
    grades: optionalJson(join(runDir, "grades.json")),
    touches: touchesOf(runDir),
    thresholds: { stuckMs: run.thresholds.stuckMs },
    auditSize: run.auditSize,
    seed: run.seed,
  });
  writeFileSync(join(runDir, "metrics.json"), `${JSON.stringify(metrics, null, 2)}\n`);
  if (metrics.audit.sample.length) writeFileSync(join(runDir, "audit-sheet.md"), auditSheet(bus, metrics));
  return metrics;
}

function main() {
  const { flags, positionals } = parseArgs(process.argv.slice(2));
  if (!positionals.length) throw new BenchError("usage: node bench/extract.mjs <run-dir>... [--out report.md]");
  const runDirs = positionals.map((dir) => resolve(dir));
  const all = runDirs.map(extractRun);
  const out = resolve(flags.out ?? (runDirs.length === 1 ? join(runDirs[0], "report.md") : join(dirname(runDirs[0]), "report.md")));
  writeFileSync(out, `${renderReport(all)}\n`);
  process.stdout.write(`${all.length} run(s) -> ${out}\n`);
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  try { main(); } catch (error) {
    process.stderr.write(`${error instanceof BenchError ? error.message : error.stack}\n`);
    process.exit(1);
  }
}
