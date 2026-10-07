// report.md: the headline table (metrics 1-6) by arm, medians with ranges, then what could not be measured.
import { headline } from "./metrics.mjs";
import { median, range, round } from "./util.mjs";

const COLUMNS = [
  ["acceptanceRate", "Accepted / N", "pct"],
  ["firstRoundRate", "Accepted in round 1", "pct"],
  ["crossFamilyRate", "Accepted by another family", "pct"],
  ["stuckMinutes", "Minutes stuck", "min"],
  ["humanTouches", "Human touches", "int"],
  ["costUsd", "Cost (USD)", "usd"],
  ["defectEscapeRate", "Defect escape", "pct"],
];

function fmt(value, kind) {
  if (value === null || value === undefined) return "n/a";
  if (kind === "pct") return `${round(value * 100, 1)}%`;
  if (kind === "usd") return `$${round(value, 2)}`;
  if (kind === "min") return `${round(value, 1)}`;
  return String(round(value, 1));
}

function cell(values, kind) {
  const have = values.filter((value) => value !== null && value !== undefined);
  if (!have.length) return "n/a";
  const mid = median(have);
  const [lo, hi] = range(have);
  return have.length === 1 ? fmt(mid, kind) : `${fmt(mid, kind)} (${fmt(lo, kind)} to ${fmt(hi, kind)})`;
}

function runLabel(m) {
  return `${m.run.arm} #${m.run.repetition}`;
}

export function renderReport(allMetrics) {
  const valid = allMetrics.filter((m) => !m.run.invalid);
  const invalid = allMetrics.filter((m) => m.run.invalid);
  const first = allMetrics[0];
  const lines = [`# Benchmark report: ${first.run.series}`, ""];
  const modes = new Set(allMetrics.map((m) => m.run.mode));
  if (modes.has("fake")) {
    lines.push("> **Fake harness.** These runs exercise the pipeline with deterministic stand-in agents. The numbers describe nothing about any model.", "");
  }
  const sizes = [...new Set(allMetrics.map((m) => m.run.size))].join(", ");
  lines.push(`Base \`${first.run.baseSha.slice(0, 12)}\`, size ${sizes}, ${valid.length} valid run(s)${invalid.length ? `, ${invalid.length} invalid` : ""}. Medians with ranges; a best run is never reported.`, "");

  const arms = [...new Set(valid.map((m) => m.run.arm))];
  lines.push(`| Arm | Runs | ${COLUMNS.map(([, label]) => label).join(" | ")} |`, `|---|---|${COLUMNS.map(() => "---").join("|")}|`);
  for (const arm of arms) {
    const runs = valid.filter((m) => m.run.arm === arm).map(headline);
    lines.push(`| ${arm} | ${runs.length} | ${COLUMNS.map(([key, , kind]) => cell(runs.map((row) => row[key]), kind)).join(" | ")} |`);
  }
  lines.push("");
  if (arms.some((arm) => valid.filter((m) => m.run.arm === arm).length < 3)) {
    lines.push("An arm with fewer than 3 runs is noisy: a difference of less than 10 points of acceptance rate between arms is not a claim.", "");
  }

  lines.push("## Runs", "", "| Run | Wall (min) | Ended | Accepted | Failed | Unfinished | Longest stall (min) | Truth |", "|---|---|---|---|---|---|---|---|");
  for (const m of allMetrics) {
    lines.push(`| ${runLabel(m)}${m.run.invalid ? " (INVALID)" : ""} | ${fmt(m.run.wallMinutes, "min")} | ${m.run.endCause ?? "n/a"} | ${m.tasks.accepted}/${m.tasks.total} | ${m.tasks.failed} | ${m.tasks.unfinished.join(", ") || "none"} | ${fmt(m.stuck.longestMinutes, "min")} | ${m.run.truth ?? "n/a"} |`);
  }
  lines.push("");
  if (invalid.length) {
    lines.push("## Invalid runs (excluded above)", "", ...invalid.map((m) => `- ${runLabel(m)}: ${m.run.invalid.reason}`), "");
  }

  const researchRuns = valid.filter((m) => m.research);
  if (researchRuns.length) {
    lines.push("## Research tasks", "", "| Run | Task | Report | Claims | Evidence verified | Precision | Recall | Unresolved |", "|---|---|---|---|---|---|---|---|");
    for (const m of researchRuns) {
      for (const [key, r] of Object.entries(m.research)) {
        lines.push(`| ${runLabel(m)} | ${key} | ${r.reportFound ? "yes" : "none"} | ${r.claims} | ${fmt(r.evidenceRate, "pct")} | ${fmt(r.precision, "pct")} | ${fmt(r.recall, "pct")} | ${r.unresolved} |`);
      }
    }
    lines.push("");
  }

  const failures = valid.flatMap((m) => m.failedTasks.map((f) => `- ${runLabel(m)} ${f.key}: ${f.reason ?? "no reason recorded"} (a human grades whether this is the true cause)`));
  if (failures.length) lines.push("## Failed tasks to grade", "", ...failures, "");

  const notes = new Set();
  for (const m of allMetrics) {
    if (m.cost.granularity === "agent") notes.add("Cost is agent-level (supervisor session files), not task-level: the bus does not record usage per task yet.");
    if (m.defectEscape === null) notes.add("Defect escape is n/a: validators did not run for at least one run.");
    if (m.run.truth === "unreconciled") notes.add("Research truth files are not reconciled by a second derivation; treat research precision and recall as provisional.");
    if (m.crossFamily.unknownFamily) notes.add("Some accepted tasks had a reviewer or assignee outside the roster, so their family is unknown and they are not counted as cross-family.");
    notes.add("Minutes stuck counts quiet stretches of 10 minutes or more inside a claim; queued minutes (time runnable and unclaimed) is an upper bound because the bus does not record whether an agent was idle.");
    if (m.audit.sample.length && m.audit.graded === 0) notes.add("Human audit: grade the blind sample (audit-sheet.md in each run) and save grades.json, then re-run extract.");
  }
  lines.push("## Limits of this report", "", ...[...notes].map((note) => `- ${note}`), "");
  return lines.join("\n");
}
