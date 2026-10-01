// Small shared helpers for the benchmark scripts. Plain Node, no dependencies.
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, readFileSync, readdirSync, statSync } from "node:fs";
import { join, relative, sep } from "node:path";

export class BenchError extends Error {}

/** `--flag`, `--key value`, `--key=value`; everything else is positional. */
export function parseArgs(argv, booleans = []) {
  const flags = {};
  const positionals = [];
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (!arg.startsWith("--")) { positionals.push(arg); continue; }
    const eq = arg.indexOf("=");
    const name = eq >= 0 ? arg.slice(2, eq) : arg.slice(2);
    if (eq >= 0) flags[name] = arg.slice(eq + 1);
    else if (booleans.includes(name)) flags[name] = true;
    else flags[name] = argv[(i += 1)];
  }
  return { flags, positionals };
}

export function readJson(path) {
  try {
    return JSON.parse(readFileSync(path, "utf8"));
  } catch (error) {
    throw new BenchError(`cannot read ${path}: ${error.message}`);
  }
}

export function run(command, args, options = {}) {
  const result = spawnSync(command, args, { encoding: "utf8", maxBuffer: 64 * 1024 * 1024, ...options });
  return { code: result.status ?? -1, stdout: result.stdout ?? "", stderr: result.stderr ?? "", error: result.error };
}

export function git(cwd, args) {
  return run("git", args, { cwd });
}

export function mustGit(cwd, args) {
  const result = git(cwd, args);
  if (result.code !== 0) throw new BenchError(`git ${args.join(" ")} failed in ${cwd}: ${result.stderr.trim() || result.stdout.trim()}`);
  return result.stdout.trim();
}

export function median(values) {
  const sorted = values.filter((value) => Number.isFinite(value)).sort((a, b) => a - b);
  if (!sorted.length) return null;
  const mid = sorted.length >> 1;
  return sorted.length % 2 ? sorted[mid] : (sorted[mid - 1] + sorted[mid]) / 2;
}

export function range(values) {
  const finite = values.filter((value) => Number.isFinite(value));
  return finite.length ? [Math.min(...finite), Math.max(...finite)] : null;
}

export function round(value, digits = 3) {
  if (value === null || value === undefined || !Number.isFinite(value)) return null;
  const factor = 10 ** digits;
  return Math.round(value * factor) / factor;
}

/** mulberry32: a small seeded generator so audit samples repeat for a series. */
export function seededRandom(seed) {
  let state = seed >>> 0;
  return () => {
    state = (state + 0x6d2b79f5) >>> 0;
    let t = state;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

export function seededShuffle(items, seed) {
  const random = seededRandom(seed);
  const out = [...items];
  for (let i = out.length - 1; i > 0; i -= 1) {
    const j = Math.floor(random() * (i + 1));
    [out[i], out[j]] = [out[j], out[i]];
  }
  return out;
}

/** One digest over every file below `dir`, so a run can show validators and truth were not edited mid-run. */
export function hashTree(dir) {
  const hash = createHash("sha256");
  const walk = (current) => {
    for (const name of readdirSync(current).sort()) {
      const path = join(current, name);
      if (statSync(path).isDirectory()) walk(path);
      else hash.update(relative(dir, path).split(sep).join("/")).update("\0").update(readFileSync(path)).update("\0");
    }
  };
  if (existsSync(dir)) walk(dir);
  return hash.digest("hex").slice(0, 16);
}

export function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

export function minutes(ms) {
  return round(ms / 60_000, 2);
}
