import assert from "node:assert/strict";
import { ChildProcess, spawn, spawnSync } from "node:child_process";
import { once } from "node:events";
import { existsSync, mkdirSync, mkdtempSync, openSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { resolveAgent } from "../src/config.js";
import { Bus } from "../src/core/bus.js";
import { DatabaseSync } from "node:sqlite";
import { MAX_FAILED_TURNS, buildBrief, mcpCommandFor, runHarnessProcess, sanitizedEnvironment, supervise } from "../src/supervisor.js";
import { testConfig } from "./helpers.js";

const QAGENT = fileURLToPath(new URL("../src/qagent.js", import.meta.url));
const FAKE_HARNESS = fileURLToPath(new URL("../src/fake-harness.js", import.meta.url));

interface Fixture {
  home: string;
  dbPath: string;
  project: string;
  bus: Bus;
  cli: (agent: string | undefined, args: string[], input?: string) => { status: number | null; stdout: string; stderr: string };
  json: (agent: string | undefined, args: string[]) => any;
  env: (agent?: string, extra?: NodeJS.ProcessEnv) => NodeJS.ProcessEnv;
}

function fixture(t: { after: (fn: () => void | Promise<void>) => void }, agents: { id: string; role: string }[]): Fixture {
  const home = mkdtempSync(join(tmpdir(), "qagent-v2-supervisor-"));
  const dbPath = join(home, "bus.db");
  const project = join(home, "project");
  mkdirSync(project);
  const env = (agent?: string, extra: NodeJS.ProcessEnv = {}): NodeJS.ProcessEnv => {
    const value: NodeJS.ProcessEnv = { ...process.env, QAGENT_BUS_DB: dbPath, QAGENT_HOME: home, AGENT_BUS_HOME: home, ...extra };
    for (const name of ["QAGENT_AGENT_ID", "AGENT_ID", "QAGENT_BLOCK_SEC", "QAGENT_CONFIG", "AGENT_BUS_CONFIG", "QAGENT_ALLOW_API_KEY", "AGENT_BUS_ALLOW_API_KEY"]) {
      if (!(name in extra)) delete value[name];
    }
    if (agent) value.QAGENT_AGENT_ID = agent;
    return value;
  };
  const cli = (agent: string | undefined, args: string[], input?: string) => {
    const result = spawnSync(process.execPath, [QAGENT, ...args], { env: env(agent), encoding: "utf8", input });
    return { status: result.status, stdout: result.stdout, stderr: result.stderr };
  };
  const json = (agent: string | undefined, args: string[]) => {
    const result = cli(agent, [...args, "--json"]);
    assert.equal(result.status, 0, `qagent ${args.join(" ")}: ${result.stderr}`);
    return JSON.parse(result.stdout);
  };
  json(undefined, ["init"]);
  for (const agent of agents) json(undefined, ["agent", "add", agent.id, "--role", agent.role, "--harness", "fake"]);
  const bus = Bus.open({ dbPath });
  t.after(() => { bus.close(); rmSync(home, { recursive: true, force: true }); });
  return { home, dbPath, project, bus, cli, json, env };
}

function writeConfig(home: string, mutate: (config: ReturnType<typeof testConfig>) => void): string {
  const config = testConfig();
  mutate(config);
  const path = join(home, "config.json");
  writeFileSync(path, `${JSON.stringify(config, null, 2)}\n`);
  return path;
}

function startSupervisor(f: Fixture, agentId: string, configPath: string, extraEnv: NodeJS.ProcessEnv = {}): ChildProcess {
  const log = openSync(join(f.home, `supervisor-${agentId}.log`), "a");
  const child = spawn(process.execPath, [QAGENT, "supervise", agentId, f.project, "--config", configPath], {
    env: f.env(undefined, extraEnv),
    stdio: ["ignore", log, log],
    detached: true,
  });
  return child;
}

function startRosterSupervisor(f: Fixture, configPath: string, extraEnv: NodeJS.ProcessEnv = {}): ChildProcess {
  const log = openSync(join(f.home, "supervisor-roster.log"), "a");
  return spawn(process.execPath, [QAGENT, "supervise", "--roster", f.project, "--config", configPath], {
    env: f.env(undefined, extraEnv),
    stdio: ["ignore", log, log],
    detached: true,
  });
}

function supervisorLog(f: Fixture, agentId: string): string {
  try { return readFileSync(join(f.home, `supervisor-${agentId}.log`), "utf8"); } catch { return ""; }
}

function rosterLog(f: Fixture): string {
  try { return readFileSync(join(f.home, "supervisor-roster.log"), "utf8"); } catch { return ""; }
}

async function until<T>(label: string, timeoutMs: number, probe: () => T | null | undefined | false, onTimeout = () => ""): Promise<T> {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const value = probe();
    if (value) return value;
    if (Date.now() > deadline) assert.fail(`timed out waiting for ${label}\n${onTimeout()}`);
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
}

function alive(pid: number): boolean {
  try { process.kill(pid, 0); return true; } catch (error) { return (error as NodeJS.ErrnoException).code === "EPERM"; }
}

async function stop(child: ChildProcess): Promise<number | null> {
  if (child.exitCode !== null) return child.exitCode;
  const exited = once(child, "exit");
  child.kill("SIGTERM");
  const [code] = await exited as [number | null];
  return code;
}

function killAll(children: ChildProcess[]): void {
  for (const child of children) {
    if (!child.pid || child.exitCode !== null) continue;
    try { process.kill(-child.pid, "SIGKILL"); } catch { try { child.kill("SIGKILL"); } catch { /* gone */ } }
  }
}

test("roster contains one supervisor failure without stopping healthy siblings", { timeout: 60_000 }, async (t) => {
  // testConfig enables fake-small and fake-strong, but only fake-small exists on the bus.
  // fake-strong therefore fails immediately while fake-small must keep running.
  const f = fixture(t, [{ id: "fake-small", role: "cheap-worker" }]);
  const configPath = writeConfig(f.home, (config) => {
    config.harnesses.fake.features.mcp = true;
    config.agents["fake-small"].harnessOptions = { mode: "bus-cli" };
  });
  const roster = startRosterSupervisor(f, configPath);
  t.after(() => killAll([roster]));

  await until(
    "the healthy roster supervisor to hold the wait",
    15_000,
    () => f.bus.getAgent("fake-small")?.storedStatus === "waiting",
    () => rosterLog(f),
  );
  await until(
    "the failed roster supervisor to be reported",
    5_000,
    () => /supervisor for fake-strong exited with error/.test(rosterLog(f)),
    () => rosterLog(f),
  );
  assert.equal(roster.exitCode, null, "one supervisor failure must not terminate the roster");

  const created = f.json("operator", ["task", "add", "Still works", "--to", "fake-small"]);
  const task = await until(
    "the healthy sibling to submit after another supervisor failed",
    20_000,
    () => {
      const current = f.bus.getTask(created.id);
      return current.state === "submitted" ? current : null;
    },
    () => rosterLog(f),
  );
  assert.equal(task.assignee, "fake-small");
  assert.match(readFileSync(join(f.home, "logs", "fake-small.log"), "utf8"), /supervising fake-small/);

  // SIGTERM still reaches the shared AbortController; the non-zero exit records
  // that the roster suffered a partial failure without killing healthy siblings early.
  assert.equal(await stop(roster), 1, rosterLog(f));
  assert.equal(
    existsSync(join(f.home, "supervisors", "fake-small.pid")),
    false,
    "healthy sibling releases its lock during shared shutdown",
  );
});

test("a fake-harness agent under `qagent supervise` claims and submits a CLI-created task as itself, and stays reachable after the supervisor stops", { timeout: 60_000 }, async (t) => {
  const f = fixture(t, [{ id: "fake-small", role: "cheap-worker" }]);
  // MCP-capable harness in bus-cli mode: the child, not the supervisor, claims, notes and submits,
  // on the `qagent mcp` command line the supervisor handed it (QAGENT_MCP_COMMAND).
  const configPath = writeConfig(f.home, (config) => {
    config.harnesses.fake.features.mcp = true;
    config.agents["fake-small"].harnessOptions = { mode: "bus-cli" };
  });
  const supervisor = startSupervisor(f, "fake-small", configPath);
  t.after(() => killAll([supervisor]));
  await until("the supervisor to hold the wait", 15_000, () => f.bus.getAgent("fake-small")?.storedStatus === "waiting", () => supervisorLog(f, "fake-small"));

  const created = f.json("operator", ["task", "add", "Write the parser", "--to", "fake-small", "--brief", "Parse the config file"]);
  const task = await until("the task to be submitted", 20_000, () => {
    const current = f.bus.getTask(created.id);
    return current.state === "submitted" ? current : null;
  }, () => supervisorLog(f, "fake-small"));

  assert.equal(task.assignee, "fake-small");
  assert.match(task.result?.summary ?? "", /fake fake-small submitted task #\d+ through qagent/);
  assert.equal(task.notes.length, 1);
  assert.equal(task.notes[0].author, "fake-small");
  const actors = new Map(f.bus.events(0, 500).filter((event) => event.entity === "task" && event.entityId === String(task.id)).map((event) => [event.kind, event.actor]));
  assert.equal(actors.get("task_created"), "operator");
  assert.equal(actors.get("task_claimed"), "fake-small", "the agent claimed with its own identity");
  assert.equal(actors.get("task_submitted"), "fake-small", "the agent submitted with its own identity");
  const done = f.bus.getMessages({ taskId: task.id }).find((message) => message.subject.startsWith("[DONE"));
  assert.equal(done?.sender, "fake-small");
  assert.equal(done?.recipient, "operator");

  assert.equal(await stop(supervisor), 0, supervisorLog(f, "fake-small"));
  assert.match(supervisorLog(f, "fake-small"), /supervisor stopped/);
  assert.equal(existsSync(join(f.home, "supervisors", "fake-small.pid")), false, "the lock is released on stop");

  // With no supervisor, a core wait still reaches the agent and the signal file tracks delivery.
  const waiter = spawn(process.execPath, [QAGENT, "wait", "--timeout", "15", "--json"], { env: f.env("fake-small"), stdio: ["ignore", "pipe", "pipe"] });
  t.after(() => killAll([waiter]));
  let waited = "";
  waiter.stdout!.on("data", (data) => { waited += String(data); });
  const exited = once(waiter, "exit");
  await until("the core wait to start", 10_000, () => f.bus.getAgent("fake-small")?.storedStatus === "waiting");
  const sent = f.json("operator", ["send", "fake-small", "ping", "still there?"]);
  const [code] = await exited as [number | null];
  assert.equal(code, 0, waited);
  const result = JSON.parse(waited);
  assert.equal(result.status, "mail");
  assert.equal(result.messages[0].subject, "ping");
  assert.equal(result.messages[0].seq, sent[0].seq);
  assert.equal(readFileSync(join(f.home, "inbox", "fake-small.seq"), "utf8").trim(), String(sent[0].seq));
});

test("a harness without bus tools is supervisor-managed: it claims an unassigned role task and auto-submits, with API keys stripped from the child", { timeout: 60_000 }, async (t) => {
  const f = fixture(t, [{ id: "fake-small", role: "cheap-worker" }]);
  // A command-adapter harness on a subscription-backed provider that reports its own environment.
  const probe = "process.stdout.write(JSON.stringify({result: 'key=' + (process.env.ANTHROPIC_API_KEY || 'absent') + ' token=' + (process.env.ANTHROPIC_AUTH_TOKEN || 'absent') + ' id=' + process.env.QAGENT_AGENT_ID + ' db=' + process.env.QAGENT_BUS_DB}) + '\\n')";
  const configPath = writeConfig(f.home, (config) => {
    config.harnesses.fake.adapter = "command";
    config.harnesses.fake.command = process.execPath;
    config.harnesses.fake.features.mcp = false;
    config.models["fake-small"].provider = "anthropic";
    config.providers.anthropic.enabled = true;
    config.agents["fake-small"].harnessOptions = { args: ["-e", probe] };
  });
  const supervisor = startSupervisor(f, "fake-small", configPath, { ANTHROPIC_API_KEY: "sk-must-not-leak", ANTHROPIC_AUTH_TOKEN: "tok-must-not-leak" });
  t.after(() => killAll([supervisor]));
  await until("the supervisor to hold the wait", 15_000, () => f.bus.getAgent("fake-small")?.storedStatus === "waiting", () => supervisorLog(f, "fake-small"));

  const created = f.json("operator", ["task", "add", "Summarise the log", "--role", "cheap-worker"]);
  assert.equal(created.assignee, null);
  const task = await until("the task to be submitted", 20_000, () => {
    const current = f.bus.getTask(created.id);
    return current.state === "submitted" ? current : null;
  }, () => supervisorLog(f, "fake-small"));
  assert.equal(task.assignee, "fake-small");
  assert.equal(task.result?.summary, `key=absent token=absent id=fake-small db=${f.dbPath}`);
  assert.match(task.result?.details ?? "", /auto-submitted by the supervisor/);
  assert.equal(await stop(supervisor), 0, supervisorLog(f, "fake-small"));
});

test("a Devin CLI agent runs one unattended print-mode turn and the supervisor submits its answer", { timeout: 60_000 }, async (t) => {
  const f = fixture(t, [{ id: "fake-small", role: "cheap-worker" }]);
  // A stand-in `devin` binary that answers with the arguments it was given.
  const devin = join(f.home, "devin");
  writeFileSync(devin, `#!${process.execPath}\nprocess.stdout.write("devin-stub " + JSON.stringify(process.argv.slice(2)) + "\\n");\n`, { mode: 0o755 });
  const configPath = writeConfig(f.home, (config) => {
    config.harnesses.fake.adapter = "devin";
    config.harnesses.fake.command = devin;
    config.harnesses.fake.features.mcp = false;
  });
  const supervisor = startSupervisor(f, "fake-small", configPath);
  t.after(() => killAll([supervisor]));
  await until("the supervisor to hold the wait", 15_000, () => f.bus.getAgent("fake-small")?.storedStatus === "waiting", () => supervisorLog(f, "fake-small"));

  const created = f.json("operator", ["task", "add", "Summarise the log", "--role", "cheap-worker"]);
  const task = await until("the task to be submitted", 20_000, () => {
    const current = f.bus.getTask(created.id);
    return current.state === "submitted" ? current : null;
  }, () => supervisorLog(f, "fake-small"));
  const summary = task.result?.summary ?? "";
  assert.ok(summary.startsWith('devin-stub ["-p","=== qagent:'), summary);
  // The fixture model's exactModel is "fake-small".
  assert.ok(summary.endsWith('","--permission-mode","dangerous","--model","fake-small"]'), summary);
  assert.match(summary, /Summarise the log/);
  assert.match(task.result?.details ?? "", /auto-submitted by the supervisor/);
  assert.equal(await stop(supervisor), 0, supervisorLog(f, "fake-small"));
});

test("isolation \"worktree\" runs a single-task turn inside that task's git worktree", { timeout: 60_000 }, async (t) => {
  const f = fixture(t, [{ id: "fake-small", role: "cheap-worker" }]);
  const gitIn = (args: string[]) => {
    const result = spawnSync("git", ["-c", "user.email=test@example.com", "-c", "user.name=test", ...args], { cwd: f.project, encoding: "utf8" });
    assert.equal(result.status, 0, result.stderr);
  };
  gitIn(["init", "-q"]);
  writeFileSync(join(f.project, "README"), "x\n");
  gitIn(["add", "."]);
  gitIn(["commit", "-q", "-m", "init"]);
  const probe = "process.stdout.write(JSON.stringify({result: 'cwd=' + process.cwd()}) + '\\n')";
  const configPath = writeConfig(f.home, (config) => {
    config.harnesses.fake.adapter = "command";
    config.harnesses.fake.command = process.execPath;
    config.harnesses.fake.features.mcp = false;
    config.constraints.isolation = "worktree";
    config.agents["fake-small"].harnessOptions = { args: ["-e", probe] };
  });
  const supervisor = startSupervisor(f, "fake-small", configPath);
  t.after(() => killAll([supervisor]));
  await until("the supervisor to hold the wait", 15_000, () => f.bus.getAgent("fake-small")?.storedStatus === "waiting", () => supervisorLog(f, "fake-small"));

  const created = f.json("operator", ["task", "add", "Edit the readme", "--role", "cheap-worker", "--project", f.project]);
  const task = await until("the task to be submitted", 20_000, () => {
    const current = f.bus.getTask(created.id);
    return current.state === "submitted" ? current : null;
  }, () => supervisorLog(f, "fake-small"));
  const worktree = f.json(undefined, ["task", "worktree", String(created.id)]);
  assert.equal(worktree.created, false);
  assert.equal(realpathSync(task.result?.summary?.replace(/^cwd=/, "") ?? ""), realpathSync(worktree.workdir));
  assert.equal(await stop(supervisor), 0, supervisorLog(f, "fake-small"));
});

test("worktree backlog retries after a concurrent-task limit releases capacity", { timeout: 60_000 }, async (t) => {
  const f = fixture(t, [{ id: "fake-small", role: "cheap-worker" }]);
  const gitIn = (args: string[]) => {
    const result = spawnSync("git", ["-c", "user.email=test@example.com", "-c", "user.name=test", ...args], { cwd: f.project, encoding: "utf8" });
    assert.equal(result.status, 0, result.stderr);
  };
  gitIn(["init", "-q"]);
  writeFileSync(join(f.project, "README"), "x\n");
  gitIn(["add", "."]);
  gitIn(["commit", "-q", "-m", "init"]);

  const operator = f.bus.identify("operator");
  const worker = f.bus.identify("fake-small");
  const holding = f.bus.createTask(operator, { title: "already claimed", to: worker.agentId, project: f.project });
  f.bus.claimTask(worker, holding.id);
  const available = f.bus.createTask(operator, { title: "backlog task", role: "cheap-worker", project: f.project });
  f.bus.inbox(worker, { limit: 50 });

  const probe = "process.stdout.write(JSON.stringify({result: 'cwd=' + process.cwd()}) + '\\n')";
  const config = testConfig();
  config.harnesses.fake.adapter = "command";
  config.harnesses.fake.command = process.execPath;
  config.harnesses.fake.features.mcp = false;
  config.constraints.isolation = "worktree";
  config.constraints.maxConcurrentTasks = 1;
  config.agents["fake-small"].harnessOptions = { args: ["-e", probe] };
  const controller = new AbortController();
  const logLines: string[] = [];
  const running = supervise({
    agentId: "fake-small",
    workdir: f.project,
    dbPath: f.dbPath,
    config,
    waitMs: 50,
    retryBaseMs: 50,
    signal: controller.signal,
    qagentBin: QAGENT,
    fakeHarnessPath: FAKE_HARNESS,
    log: (line) => { logLines.push(line); },
  });
  t.after(async () => { controller.abort(); await running; });
  await until("the supervisor to wait at capacity", 15_000, () =>
    f.bus.getAgent("fake-small")?.storedStatus === "waiting" &&
    logLines.some((line) => line.includes("at concurrent task limit; waiting for capacity")),
  () => logLines.join("\n"));
  assert.equal(f.bus.getTask(available.id).state, "open");

  f.bus.submitTask(worker, holding.id, { summary: "capacity released" });
  const submitted = await until("the backlog task to be submitted after capacity is released", 30_000, () => {
    const task = f.bus.getTask(available.id);
    return task.state === "submitted" ? task : null;
  }, () => logLines.join("\n"));
  assert.equal(submitted.assignee, worker.agentId);
  const worktree = f.json(undefined, ["task", "worktree", String(available.id)]);
  assert.equal(realpathSync(submitted.result?.summary?.replace(/^cwd=/, "") ?? ""), realpathSync(worktree.workdir));
  controller.abort();
  await running;
});

test("sanitizedEnvironment strips subscription provider keys unless explicitly allowed", () => {
  const config = testConfig();
  config.providers.anthropic.enabled = true;
  const opus = resolveAgent(config, "opus");
  const fake = resolveAgent(config, "fake-small");
  const saved = { key: process.env.ANTHROPIC_API_KEY, token: process.env.ANTHROPIC_AUTH_TOKEN, allow: process.env.QAGENT_ALLOW_API_KEY };
  try {
    process.env.ANTHROPIC_API_KEY = "sk-test";
    process.env.ANTHROPIC_AUTH_TOKEN = "tok-test";
    delete process.env.QAGENT_ALLOW_API_KEY;
    const stripped = sanitizedEnvironment(opus, { QAGENT_AGENT_ID: "opus" });
    assert.equal(stripped.ANTHROPIC_API_KEY, undefined);
    assert.equal(stripped.ANTHROPIC_AUTH_TOKEN, undefined);
    assert.equal(stripped.QAGENT_AGENT_ID, "opus");
    assert.equal(sanitizedEnvironment(fake, {}).ANTHROPIC_API_KEY, "sk-test", "non-subscription providers keep their keys");
    process.env.QAGENT_ALLOW_API_KEY = "1";
    assert.equal(sanitizedEnvironment(opus, {}).ANTHROPIC_API_KEY, "sk-test", "QAGENT_ALLOW_API_KEY=1 keeps the key");
  } finally {
    for (const [name, value] of [["ANTHROPIC_API_KEY", saved.key], ["ANTHROPIC_AUTH_TOKEN", saved.token], ["QAGENT_ALLOW_API_KEY", saved.allow]] as const) {
      if (value === undefined) delete process.env[name]; else process.env[name] = value;
    }
  }
});

test("a harness timeout kills the whole process group, grandchildren included", { timeout: 20_000 }, async (t) => {
  const dir = mkdtempSync(join(tmpdir(), "qagent-v2-pgkill-"));
  const state = join(dir, "pids.json");
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const agent = resolveAgent(testConfig(), "fake-small");
  const saved = process.env.FAKE_HARNESS_STATE;
  process.env.FAKE_HARNESS_STATE = state;
  let result;
  try {
    result = await runHarnessProcess(
      { command: process.execPath, args: [FAKE_HARNESS, "--mode", "hang", "--agent", "fake-small"], environment: {}, autoReport: false, timeoutMs: 1_000 },
      agent,
      dir,
    );
  } finally {
    if (saved === undefined) delete process.env.FAKE_HARNESS_STATE; else process.env.FAKE_HARNESS_STATE = saved;
  }
  assert.equal(result.timedOut, true);
  const pids = JSON.parse(readFileSync(state, "utf8")) as { pid: number; grandchild: number };
  await until("the grandchild to die", 5_000, () => !alive(pids.pid) && !alive(pids.grandchild));
});

test("stopping the supervisor mid-turn kills the running CLI's process group", { timeout: 60_000 }, async (t) => {
  const f = fixture(t, [{ id: "fake-small", role: "cheap-worker" }]);
  const state = join(f.home, "hang-pids.json");
  const configPath = writeConfig(f.home, (config) => { config.agents["fake-small"].harnessOptions = { mode: "hang" }; });
  const supervisor = startSupervisor(f, "fake-small", configPath, { FAKE_HARNESS_STATE: state });
  let pids: { pid: number; grandchild: number } | null = null;
  t.after(() => {
    killAll([supervisor]);
    for (const pid of pids ? [pids.pid, pids.grandchild] : []) { try { process.kill(pid, "SIGKILL"); } catch { /* gone */ } }
  });
  await until("the supervisor to hold the wait", 15_000, () => f.bus.getAgent("fake-small")?.storedStatus === "waiting", () => supervisorLog(f, "fake-small"));
  const created = f.json("operator", ["task", "add", "Never finishes", "--to", "fake-small"]);
  pids = await until("the harness to start", 15_000, () => existsSync(state) && JSON.parse(readFileSync(state, "utf8")), () => supervisorLog(f, "fake-small"));
  assert.equal(f.bus.getTask(created.id).state, "claimed", "the supervisor claimed for a harness without bus tools");
  assert.ok(alive(pids!.pid) && alive(pids!.grandchild));
  assert.equal(await stop(supervisor), 0, supervisorLog(f, "fake-small"));
  await until("the harness process group to die", 5_000, () => !alive(pids!.pid) && !alive(pids!.grandchild));
  assert.match(supervisorLog(f, "fake-small"), /process group killed/);
});

test("the brief and MCP command carry the agent's own identity and no bus_wait", () => {
  const command = mcpCommandFor("hands-bravo", "/tmp/qagent-test/bus.db", "/opt/qagent/dist/qagent.js");
  assert.deepEqual(command.args, ["/opt/qagent/dist/qagent.js", "mcp"]);
  assert.deepEqual(command.env, { QAGENT_AGENT_ID: "hands-bravo", QAGENT_BUS_DB: "/tmp/qagent-test/bus.db" });
  const agent = resolveAgent(testConfig(), "fake-small");
  const message = {
    seq: 7, id: "m", tsMs: 0, sender: "lead", recipient: "fake-small", type: "task" as const, subject: "[TASK #3] Parse", body: "brief",
    thread: "task-3", taskId: 3, refs: [], requiresAck: false, source: "v2",
  };
  const native = buildBrief(agent, [message], [], false);
  assert.match(native, /\[TASK #3\] Parse/);
  assert.match(native, /bus_task_claim/);
  assert.match(native, /Do NOT call bus_wait/);
  assert.match(buildBrief(agent, [message], [], true), /supervisor has claimed/);
});
test("a supervisor pauses its agent when the budget runs out, holds new work, and carries on after resume", { timeout: 60_000 }, async (t) => {
  const f = fixture(t, [{ id: "fake-small", role: "cheap-worker" }]);
  const configPath = writeConfig(f.home, () => {});
  f.json("operator", ["agent", "budget", "fake-small", "--turns", "1"]);
  const supervisor = startSupervisor(f, "fake-small", configPath);
  t.after(() => killAll([supervisor]));
  const log = () => supervisorLog(f, "fake-small");

  const first = f.json("operator", ["task", "add", "first", "--to", "fake-small"]);
  await until("the first task to be submitted", 20_000, () => f.bus.getTask(first.id).state === "submitted", log);
  const paused = await until("the budget pause", 20_000, () => f.bus.getAgent("fake-small")?.meta.paused as { reason: string } | undefined, log);
  assert.equal(paused.reason, "budget reached: 1 of 1 turns");
  const mail = f.bus.getMessages({}).filter((message) => message.recipient === "operator").map((message) => message.subject);
  assert.ok(mail.includes("fake-small paused: budget reached (1 of 1 turns)"), mail.join("\n"));

  const second = f.json("operator", ["task", "add", "second", "--to", "fake-small"]);
  f.json("operator", ["send", "fake-small", "paused mail", "keep this until resume"]);
  await new Promise((resolve) => setTimeout(resolve, 2500));
  assert.equal(f.bus.getTask(second.id).state, "open", "a paused agent starts no turn");
  assert.equal(f.bus.unreadCount("fake-small"), 2, "task and ordinary mail remain unread while paused");

  f.json("operator", ["agent", "resume", "fake-small"]);
  await until("paused mail to be consumed after resume", 20_000, () => f.bus.unreadCount("fake-small") === 0, log);
  await until("the turn budget to pause the agent again", 20_000, () => f.bus.getAgent("fake-small")?.meta.paused, log);
  await until("the second task to be submitted", 20_000, () => f.bus.getTask(second.id).state === "submitted", log);
  assert.equal(f.bus.getTask(second.id).state, "submitted");
  assert.equal(await stop(supervisor), 0, log());
});

test("after repeated failed turns the supervisor stops itself, tells the operator, and offers the mail to every retry", { timeout: 60_000 }, async (t) => {
  const f = fixture(t, [{ id: "fake-small", role: "cheap-worker" }]);
  const config = testConfig();
  config.harnesses.fake.features.mcp = false;
  config.agents["fake-small"].harnessOptions = { mode: "fail" };
  f.bus.send(f.bus.identify("operator"), { to: "fake-small", subject: "look at this", body: "please" });
  await supervise({
    agentId: "fake-small",
    workdir: f.project,
    dbPath: f.dbPath,
    config,
    waitMs: 1_000,
    retryBaseMs: 20,
    qagentBin: QAGENT,
    fakeHarnessPath: FAKE_HARNESS,
    log: () => {},
  });
  // One plain message drove every retry: a failed turn hands its mail to the next one.
  const session = JSON.parse(readFileSync(join(f.home, "sessions", "fake-small.json"), "utf8"));
  assert.equal(session.turns, MAX_FAILED_TURNS);
  // The mail no turn managed to use waits in the session file for the next start.
  assert.deepEqual(session.pendingMail.map((m: { subject: string }) => m.subject), ["look at this"]);
  const toOperator = f.bus.inbox(f.bus.identify("operator"), { peek: true }).messages;
  assert.ok(toOperator.some((m) => m.subject === `fake-small stopped: ${MAX_FAILED_TURNS} turns failed in a row`));
  assert.equal(existsSync(join(f.home, "supervisors", "fake-small.pid")), false, "the lock is released");
});

test("a supervisor keeps going through a bus locked past the busy timeout", { timeout: 60_000 }, async (t) => {
  const f = fixture(t, [{ id: "fake-small", role: "cheap-worker" }]);
  const config = testConfig();
  config.harnesses.fake.features.mcp = false;
  const lines: string[] = [];
  const holder = new DatabaseSync(f.dbPath);
  holder.exec("PRAGMA busy_timeout = 0; BEGIN IMMEDIATE");
  const controller = new AbortController();
  t.after(() => controller.abort());
  const running = supervise({
    agentId: "fake-small",
    workdir: f.project,
    dbPath: f.dbPath,
    config,
    waitMs: 1_000,
    retryBaseMs: 50,
    signal: controller.signal,
    qagentBin: QAGENT,
    fakeHarnessPath: FAKE_HARNESS,
    log: (line) => { lines.push(line); },
  });
  let settled = false;
  void running.then(() => { settled = true; }, () => { settled = true; });
  // Locked at startup: the configuration limits wait for the bus instead of being skipped.
  await until("a startup retry", 30_000, () => lines.some((l) => l.includes("configuration limits not applied yet")), () => lines.join("\n"));
  assert.equal(settled, false, "the supervisor must not exit on a locked bus");
  holder.exec("COMMIT");
  const created = f.json("operator", ["task", "add", "after the lock", "--to", "fake-small"]);
  await until("the task to be submitted", 30_000, () => f.bus.getTask(created.id).state === "submitted", () => lines.join("\n"));
  // Locked while running: the round fails and is retried.
  holder.exec("BEGIN IMMEDIATE");
  await until("a failed round", 30_000, () => lines.some((l) => l.includes("round failed")), () => lines.join("\n"));
  assert.equal(settled, false, "the supervisor must not exit on a locked bus");
  holder.exec("COMMIT");
  holder.close();
  const second = f.json("operator", ["task", "add", "after the second lock", "--to", "fake-small"]);
  await until("the second task to be submitted", 30_000, () => f.bus.getTask(second.id).state === "submitted", () => lines.join("\n"));
  controller.abort();
  await running;
});

test("a round that fails before its turn runs gives back the task it claimed", { timeout: 60_000 }, async (t) => {
  const f = fixture(t, [{ id: "fake-small", role: "cheap-worker" }]);
  const config = testConfig();
  config.harnesses.fake.features.mcp = false;
  // The fake adapter prepares <workdir>/.agent-bus; a file in its place fails every round.
  writeFileSync(join(f.project, ".agent-bus"), "not a directory");
  const created = f.json("operator", ["task", "add", "never started", "--to", "fake-small"]);
  const lines: string[] = [];
  const controller = new AbortController();
  t.after(() => controller.abort());
  const running = supervise({
    agentId: "fake-small",
    workdir: f.project,
    dbPath: f.dbPath,
    config,
    waitMs: 1_000,
    retryBaseMs: 20,
    signal: controller.signal,
    qagentBin: QAGENT,
    fakeHarnessPath: FAKE_HARNESS,
    log: (line) => { lines.push(line); },
  });
  await until("two failed rounds", 30_000, () => lines.filter((l) => l.includes("round failed")).length >= 2, () => lines.join("\n"));
  controller.abort();
  await running;
  const task = f.bus.getTask(created.id);
  assert.notEqual(task.state, "claimed", lines.join("\n"));
});
