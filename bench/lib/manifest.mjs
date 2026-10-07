import { join } from "node:path";
import { BenchError, readJson } from "./util.mjs";

export const BENCH_DIR = join(import.meta.dirname, "..");

const SIZES = ["mini", "endurance"];

export function loadManifest(path = join(BENCH_DIR, "MANIFEST.json")) {
  const manifest = readJson(path);
  for (const key of ["series", "base_sha", "seed", "roster", "budgets", "arms"]) {
    if (manifest[key] === undefined) throw new BenchError(`${path}: missing "${key}"`);
  }
  if (!Array.isArray(manifest.roster) || !manifest.roster.length) throw new BenchError(`${path}: roster is empty`);
  const ids = new Set();
  for (const agent of manifest.roster) {
    for (const key of ["id", "role", "authority", "family", "provider"]) {
      if (!agent[key]) throw new BenchError(`${path}: roster entry ${JSON.stringify(agent.id ?? agent)} lacks "${key}"`);
    }
    if (ids.has(agent.id)) throw new BenchError(`${path}: duplicate roster id ${agent.id}`);
    ids.add(agent.id);
  }
  const managers = manifest.roster.filter((agent) => agent.authority === "manager");
  if (managers.length !== 1) throw new BenchError(`${path}: the roster needs exactly one manager (it creates and reviews the tasks)`);
  for (const size of SIZES) {
    const budget = manifest.budgets[size];
    if (!budget || !(budget.runUsd > 0) || !(budget.wallMinutes > 0)) throw new BenchError(`${path}: budgets.${size} needs runUsd and wallMinutes`);
  }
  if (!(manifest.budgets.taskMinutes > 0)) throw new BenchError(`${path}: budgets.taskMinutes is required`);
  return manifest;
}

export function manifestManager(manifest) {
  return manifest.roster.find((agent) => agent.authority === "manager");
}

export function familyOf(manifest, agentId) {
  return manifest.roster.find((agent) => agent.id === agentId)?.family ?? null;
}
