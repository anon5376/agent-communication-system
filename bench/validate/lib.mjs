// Helpers shared by the frozen validators. A validator exits 0 when the task's acceptance holds in $BENCH_CHECKOUT, else non-zero.
import { spawn, spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

export const CHECKOUT = process.env.BENCH_CHECKOUT ?? process.cwd();
export const BASE_SHA = process.env.BENCH_BASE_SHA ?? "";
export const QAGENT = join(CHECKOUT, "dist", "qagent.js");

export function fail(message) {
  process.stderr.write(`FAIL: ${message}\n`);
  process.exit(1);
}

export function check(condition, message) {
  if (!condition) fail(message);
}

export function read(path) {
  try { return readFileSync(join(CHECKOUT, path), "utf8"); } catch { return null; }
}

export function sh(command, args, options = {}) {
  const result = spawnSync(command, args, { cwd: CHECKOUT, encoding: "utf8", maxBuffer: 64 * 1024 * 1024, ...options });
  return { code: result.status ?? -1, stdout: result.stdout ?? "", stderr: result.stderr ?? "" };
}

/** A throwaway bus: `cli(agent, args, input)` runs qagent as that agent (null = the operator); `env()` with no argument carries no identity. */
export function newBus() {
  const home = mkdtempSync(join(tmpdir(), "bench-validate-"));
  const dbPath = join(home, "bus.db");
  const env = (agent) => {
    const value = { ...process.env, QAGENT_BUS_DB: dbPath, QAGENT_HOME: home };
    for (const name of ["QAGENT_AGENT_ID", "AGENT_ID", "AGENT_BUS_HOME", "QAGENT_BLOCK_SEC", "QAGENT_CONFIG", "AGENT_BUS_CONFIG"]) delete value[name];
    if (agent) value.QAGENT_AGENT_ID = agent;
    return value;
  };
  const cli = (agent, args, input) => sh(process.execPath, [QAGENT, ...args], { env: env(agent ?? "operator"), input });
  const json = (agent, args, input) => {
    const result = cli(agent, [...args, "--json"], input);
    check(result.code === 0, `qagent ${args.join(" ")} (as ${agent ?? "operator"}) exited ${result.code}: ${result.stderr.trim()}`);
    return JSON.parse(result.stdout);
  };
  const first = cli(null, ["init"]);
  check(first.code === 0, `qagent init failed: ${first.stderr}`);
  return { home, dbPath, env, cli, json, cleanup: () => rmSync(home, { recursive: true, force: true }) };
}

export function spawnChild(command, args, options) {
  return spawn(command, args, { cwd: CHECKOUT, detached: true, stdio: "ignore", ...options });
}

export function killGroup(child, signal = "SIGKILL") {
  if (!child?.pid) return;
  try { process.kill(-child.pid, signal); } catch { try { child.kill(signal); } catch { /* gone */ } }
}

export async function until(label, timeoutMs, probe) {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const value = probe();
    if (value) return value;
    if (Date.now() > deadline) fail(`timed out waiting for ${label}`);
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
}

/** A supervisor config for the fake or command harness, derived from the repo's test fixture. */
export function fixtureConfig(mutate) {
  const config = JSON.parse(readFileSync(join(CHECKOUT, "tests", "fixtures", "test-bus.config.json"), "utf8"));
  for (const model of Object.values(config.models)) model.enabled = model.provider === "fake";
  for (const agent of Object.values(config.agents)) agent.enabled = agent.id === "fake-small";
  for (const provider of Object.values(config.providers)) provider.enabled = provider.id === "fake";
  for (const harness of Object.values(config.harnesses)) harness.enabled = harness.id === "fake";
  config.constraints.defaultWriteScopes = ["."];
  mutate?.(config);
  return config;
}
