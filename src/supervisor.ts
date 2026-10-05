/**
 * Optional supervisor (`qagent supervise <agent> [dir]`). It holds the blocking wait for
 * one agent through the core library (Bus.waitForMail), renders a brief, and starts the
 * vendor CLI through its harness adapter with an MCP entry of `qagent mcp` under the
 * agent's own identity. Claims, failures and auto-submits for harnesses without bus tools
 * go through core as that agent. Nothing here listens on a port, and nothing in core
 * imports this module.
 */
import { spawn } from "node:child_process";
import { appendFileSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { AdapterContext, HarnessInvocation, McpCommand, getHarnessAdapter } from "./adapters.js";
import { AgentDefinition, BusConfig, ResolvedAgent, configPathFromProject, loadConfig, resolveAgent } from "./config.js";
import { Bus } from "./core/bus.js";
import type { AgentPolicy, Identity } from "./core/identity.js";
import { budgetOver, pausedOf } from "./core/control.js";
import { BusError, DEFAULT_WAIT_SEC, Message, OPERATOR_ID, Task } from "./core/types.js";

/** First non-empty environment variable among `names` (new name first, old name second). */
function envValue(...names: string[]): string | undefined {
  for (const name of names) {
    const value = process.env[name];
    if (typeof value === "string" && value.trim()) return value;
  }
  return undefined;
}

export function policyFromConfig(config: BusConfig, agent: AgentDefinition): AgentPolicy {
  const policy: AgentPolicy = {
    canDelegate: agent.permissions.canDelegate,
    maxDelegationDepth: Math.min(agent.permissions.maxDelegationDepth, config.constraints.maxDelegationDepth),
    maxConcurrentTasks: config.constraints.maxConcurrentTasks,
  };
  if (agent.permissions.allowedChildAgentIds?.length) policy.allowedChildAgentIds = [...agent.permissions.allowedChildAgentIds];
  return policy;
}

function isTaskCapacityConflict(error: unknown, agentId: string): boolean {
  return error instanceof BusError && error.code === "conflict" &&
    error.message.startsWith(`${agentId} already holds `) &&
    error.message.endsWith("claimed task(s), its limit; submit or release one first");
}

export interface ProcessResult {
  code: number;
  output: string;
  durationMs: number;
  timedOut: boolean;
}

export function sanitizedEnvironment(agent: ResolvedAgent, additions: Record<string, string>): NodeJS.ProcessEnv {
  const env: NodeJS.ProcessEnv = { ...process.env, ...additions, MCP_TOOL_TIMEOUT: "3600000" };
  if (envValue("QAGENT_ALLOW_API_KEY", "AGENT_BUS_ALLOW_API_KEY") === "1" || !agent.providerDefinition.subscriptionBacked) return env;
  const providerKeys: Record<string, string[]> = {
    anthropic: ["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN"],
    openai: ["OPENAI_API_KEY"],
    google: ["GEMINI_API_KEY", "GOOGLE_API_KEY"],
    moonshot: ["MOONSHOT_API_KEY", "KIMI_API_KEY"],
    xai: ["XAI_API_KEY"],
  };
  for (const key of providerKeys[agent.modelDefinition.provider] ?? []) delete env[key];
  return env;
}

export function retryDelayMs(consecutiveFailures: number): number {
  return Math.min(60_000, 2_000 * 2 ** Math.max(0, consecutiveFailures - 1));
}

export function resumedUnexpectedSession(pinnedSessionId: string | null, observedSessionId: string | null): boolean {
  return Boolean(pinnedSessionId && observedSessionId && pinnedSessionId !== observedSessionId);
}

export function runHarnessProcess(
  invocation: HarnessInvocation,
  agent: ResolvedAgent,
  workdir: string,
  onSpawn?: (pid: number | null) => void,
): Promise<ProcessResult> {
  const started = Date.now();
  return new Promise((resolve) => {
    const child = spawn(invocation.command, invocation.args, {
      cwd: workdir,
      env: sanitizedEnvironment(agent, invocation.environment),
      stdio: ["ignore", "pipe", "pipe"],
      detached: process.platform !== "win32",
    });
    onSpawn?.(child.pid ?? null);
    let output = "";
    let settled = false;
    let timedOut = false;
    const finish = (code: number) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      onSpawn?.(null);
      resolve({ code, output, durationMs: Date.now() - started, timedOut });
    };
    const timer = setTimeout(() => {
      timedOut = true;
      if (child.pid && process.platform !== "win32") {
        try { process.kill(-child.pid, "SIGTERM"); } catch { child.kill("SIGTERM"); }
      } else {
        child.kill("SIGTERM");
      }
      setTimeout(() => {
        if (!settled && child.pid && process.platform !== "win32") {
          try { process.kill(-child.pid, "SIGKILL"); } catch { child.kill("SIGKILL"); }
        }
      }, 3_000).unref();
    }, invocation.timeoutMs);
    child.stdout.on("data", (data) => {
      output += data.toString();
      process.stdout.write(data);
    });
    child.stderr.on("data", (data) => {
      output += data.toString();
      process.stderr.write(data);
    });
    child.on("error", (error) => {
      output += `\nspawn error: ${error.message}`;
      finish(-1);
    });
    child.on("close", (code) => finish(code ?? -1));
  });
}

export const DEFAULT_QAGENT_BIN = fileURLToPath(new URL("./qagent.js", import.meta.url));
export const DEFAULT_FAKE_HARNESS = fileURLToPath(new URL("./fake-harness.js", import.meta.url));

export interface SuperviseOptions {
  agentId: string;
  workdir: string;
  dbPath: string;
  /** Harness configuration file; defaults to configPathFromProject(workdir). */
  configPath?: string;
  /** Already-loaded configuration (tests); wins over configPath. */
  config?: BusConfig;
  /** Stops the loop and kills the running CLI's process group. */
  signal?: AbortSignal;
  /** Length of each blocking wait before it is renewed. Default DEFAULT_WAIT_SEC. */
  waitMs?: number;
  /** The qagent bin the CLI's MCP entry runs (`node <bin> mcp`). */
  qagentBin?: string;
  fakeHarnessPath?: string;
  log?: (line: string) => void;
}

interface SessionRecord {
  sessionId: string | null;
  turns: number;
  inputTokens: number;
  outputTokens: number;
  totalTokens: number;
  costUSD: number;
  latencyMs: number;
}

function emptySession(): SessionRecord {
  return { sessionId: null, turns: 0, inputTokens: 0, outputTokens: 0, totalTokens: 0, costUSD: 0, latencyMs: 0 };
}

function readSession(path: string): SessionRecord {
  try {
    const parsed = JSON.parse(readFileSync(path, "utf8")) as Partial<SessionRecord>;
    return { ...emptySession(), ...parsed, sessionId: parsed.sessionId ?? null };
  } catch {
    return emptySession();
  }
}

/**
 * True while this agent is paused. When its budget has run out, the agent pauses
 * itself here and tells the operator, once. Mirror of hold_for_pause in rust/src/supervisor.rs.
 */
function holdForPause(bus: Bus, me: Identity, log: (line: string) => void): boolean {
  const agent = bus.getAgent(me.agentId);
  if (!agent) return false;
  if (pausedOf(agent.meta)) return true;
  const budget = bus.budgetOf(agent);
  const over = budget ? budgetOver(budget) : null;
  if (!over) return false;
  const id = me.agentId;
  bus.pauseAgent(me, id, `budget reached: ${over}`);
  bus.send(me, {
    to: OPERATOR_ID,
    subject: `${id} paused: budget reached (${over})`,
    body: `${id} used its budget (${over}) and paused itself before starting another turn. `
      + `Open work stays where it is. To carry on: resume ${id} in aos (or qagent agent resume ${id}). `
      + `To change the budget: budget ${id} 20 turns 60 min in aos, or budget ${id} off.`,
    type: "info",
  });
  log(`paused: budget reached (${over})`);
  return true;
}

function sleepUnlessAborted(ms: number, signal?: AbortSignal): Promise<void> {
  return new Promise((resolveSleep) => {
    if (signal?.aborted) return resolveSleep();
    const timer = setTimeout(done, ms);
    function done(): void {
      clearTimeout(timer);
      signal?.removeEventListener("abort", done);
      resolveSleep();
    }
    signal?.addEventListener("abort", done, { once: true });
  });
}

/** The MCP launch line handed to the vendor CLI: `qagent mcp` as this agent, on this database. */
export function mcpCommandFor(agentId: string, dbPath: string, qagentBin = DEFAULT_QAGENT_BIN): McpCommand {
  return { command: process.execPath, args: [qagentBin, "mcp"], env: { QAGENT_AGENT_ID: agentId, QAGENT_BUS_DB: resolve(dbPath) } };
}

/** Whether the supervisor claims and submits for this CLI. Harnesses without MCP cannot call bus tools. */
export function supervisorManaged(agent: ResolvedAgent): boolean {
  return !agent.harnessDefinition.features.mcp;
}

function renderMessage(message: Message): string {
  return [
    `-- from ${message.sender} · ${message.type}${message.taskId ? ` · task #${message.taskId}` : ""}`,
    message.subject,
    "",
    message.body,
    message.refs.length ? `\nReferences:\n${message.refs.map((ref) => `- ${ref.type}: ${ref.value}`).join("\n")}` : "",
  ].filter(Boolean).join("\n");
}

function renderTask(task: Task): string {
  return [
    `-- task #${task.id} · ${task.state}${task.assignee ? ` · ${task.assignee}` : " · unassigned"}${task.role ? ` · role ${task.role}` : ""}`,
    task.title,
    task.brief ? `\n${task.brief}` : "",
    task.acceptance ? `\nAcceptance:\n${task.acceptance}` : "",
    task.pathScopes.length ? `\nPath scopes (${task.project}): ${task.pathScopes.join(", ")}` : "",
  ].filter(Boolean).join("\n");
}

/** The brief for one turn. It names only the v2 bus_* tools. */
export function buildBrief(agent: ResolvedAgent, messages: Message[], tasks: Task[], managed: boolean): string {
  const lines = [
    `=== qagent: ${messages.length} new message(s)${tasks.length ? `, ${tasks.length} task(s)` : ""} for ${agent.id} ===`,
    `Role: ${agent.role}; model: ${agent.modelDefinition.id}; family: ${agent.modelDefinition.family}; harness: ${agent.harnessDefinition.id}.`,
    "",
    ...messages.map(renderMessage).flatMap((block) => [block, ""]),
    ...tasks.map(renderTask).flatMap((block) => [block, ""]),
    "=== end of scoped messages ===",
    "",
    "Do the actual work now. Retrieve only the files or evidence needed for this task.",
    "Use file paths and artifacts for handoff instead of pasting large outputs into messages.",
  ];
  if (managed) {
    lines.push(
      "The supervisor has claimed the task(s) above for you and will submit your final answer as the result.",
      "End the turn with the result or your question.",
    );
  } else {
    lines.push(
      "Claim a task with bus_task_claim before you start it, record progress with bus_task_note,",
      "and finish with bus_task_submit (summary, changed_files, validation). Reply with bus_send;",
      "review with bus_task_review when you are the reviewer.",
      "Do NOT call bus_wait: the supervisor holds the wait and will wake you again.",
      "End the turn after reporting the result or question.",
    );
  }
  return lines.join("\n");
}

function cancellationOnly(messages: Message[]): boolean {
  return messages.length > 0 && messages.every((message) => message.type === "control" && message.subject.startsWith("[CANCELLED"));
}

function structuredArray(value: unknown): unknown[] {
  return Array.isArray(value) ? value : [];
}

function claimableBy(task: Task, agentId: string, role: string): boolean {
  return (task.state === "open" || task.state === "changes_requested")
    && (task.assignee === agentId || (task.assignee === null && (task.role === "" || task.role === role)));
}

/** One pid file per agent so two supervisors never drive the same CLI session. */
function acquireLock(dir: string, agentId: string): () => void {
  mkdirSync(dir, { recursive: true, mode: 0o700 });
  const path = join(dir, `${agentId}.pid`);
  let holder = 0;
  try { holder = Number(readFileSync(path, "utf8").trim()) || 0; } catch { /* no holder */ }
  if (holder && holder !== process.pid) {
    let alive = true;
    try { process.kill(holder, 0); } catch (error) { alive = (error as NodeJS.ErrnoException).code === "EPERM"; }
    if (alive) throw new BusError("conflict", `a supervisor for ${agentId} is already running (pid ${holder})`);
  }
  writeFileSync(path, `${process.pid}\n`, { mode: 0o600 });
  return () => {
    try { if (Number(readFileSync(path, "utf8").trim()) === process.pid) rmSync(path, { force: true }); } catch { /* already gone */ }
  };
}

function killGroup(pid: number, signal: NodeJS.Signals): void {
  try {
    if (process.platform !== "win32") process.kill(-pid, signal);
    else process.kill(pid, signal);
  } catch {
    try { process.kill(pid, signal); } catch { /* already gone */ }
  }
}

function sleep(ms: number, signal?: AbortSignal): Promise<void> {
  return new Promise((resolveSleep) => {
    const timer = setTimeout(done, ms);
    function done() { clearTimeout(timer); signal?.removeEventListener("abort", done); resolveSleep(); }
    signal?.addEventListener("abort", done, { once: true });
  });
}

/**
 * Supervise one agent until `signal` aborts. The agent must exist in the bus (with a
 * token file) and in the harness configuration. Returns after the running CLI, if any,
 * has been stopped.
 */
export async function supervise(options: SuperviseOptions): Promise<void> {
  const workdir = resolve(options.workdir);
  const bus = Bus.open({ dbPath: options.dbPath });
  const home = bus.home;
  const logDir = join(home, "logs");
  const sessionDir = join(home, "sessions");
  mkdirSync(logDir, { recursive: true, mode: 0o700 });
  mkdirSync(sessionDir, { recursive: true, mode: 0o700 });
  const write = options.log ?? ((line: string) => { process.stdout.write(`${line}\n`); });
  const log = (line: string) => {
    const stamped = `[${new Date().toISOString()}] ${line}`;
    write(stamped);
    try { appendFileSync(join(logDir, `${options.agentId}.log`), `${stamped}\n`); } catch { /* best effort */ }
  };

  let release: (() => void) | null = null;
  let childPid: number | null = null;
  const stopChild = () => {
    const pid = childPid;
    if (!pid) return;
    killGroup(pid, "SIGTERM");
    setTimeout(() => { if (childPid === pid) killGroup(pid, "SIGKILL"); }, 3_000).unref();
  };
  options.signal?.addEventListener("abort", stopChild, { once: true });

  try {
    const me: Identity = bus.identify(options.agentId);
    const busAgent = bus.getAgent(options.agentId);
    if (!busAgent) throw new BusError("not_found", `agent ${options.agentId} is not on the bus; add it with \`qagent agent add ${options.agentId}\``);
    const config = options.config ?? loadConfig(options.configPath ?? configPathFromProject(workdir));
    const agent = resolveAgent(config, options.agentId);
    if (!agent.enabled) throw new Error(`agent ${agent.id} is disabled in the harness configuration`);
    release = acquireLock(join(home, "supervisors"), agent.id);

    let operator: Identity | null = null;
    try { operator = bus.identify(OPERATOR_ID); } catch { /* no operator token on this bus */ }
    bus.setAgentPolicy(operator ?? me, me.agentId, policyFromConfig(config, agent));

    const adapter = getHarnessAdapter(agent.harnessDefinition.adapter);
    const managed = supervisorManaged(agent);
    const sessionPath = join(sessionDir, `${agent.id}.json`);
    const session = readSession(sessionPath);
    const pinnedSessionId = agent.resumeSessionId?.trim() || null;
    if (pinnedSessionId && session.sessionId !== pinnedSessionId) {
      session.sessionId = pinnedSessionId;
      writeFileSync(sessionPath, JSON.stringify(session, null, 2));
    }
    const qagentBin = options.qagentBin ?? DEFAULT_QAGENT_BIN;
    const mcpCommand = mcpCommandFor(me.agentId, bus.dbPath, qagentBin);
    const blockSec = agent.harnessDefinition.id === "claude" ? "900" : "240";
    const waitMs = options.waitMs ?? DEFAULT_WAIT_SEC * 1000;
    let retryClaimAtCapacity = false;
    let consecutiveFailures = 0;
    log(`supervising ${agent.id} via ${agent.harnessDefinition.id} in ${workdir} (bus ${bus.dbPath}${managed ? ", supervisor-managed tasks" : ""})`);

    let pausedSince: number | null = null;
    let lastBeat = Date.now();
    while (!options.signal?.aborted) {
      // Pause and budget come first, so a paused agent's mail stays unread for later.
      if (holdForPause(bus, me, log)) {
        if (pausedSince === null) {
          log("paused; no new turn until the operator resumes this agent");
          bus.setStatus(me, "idle");
          pausedSince = Date.now();
        }
        if (Date.now() - lastBeat >= 60_000) {
          bus.heartbeat(me);
          lastBeat = Date.now();
        }
        await sleepUnlessAborted(2000, options.signal);
        continue;
      }
      if (pausedSince !== null) {
        pausedSince = null;
        log("resumed");
      }
      const waited = await bus.waitForMail(me, { timeoutMs: waitMs, signal: options.signal });
      if (options.signal?.aborted) break;
      if (waited.status === "timeout" && !(managed && retryClaimAtCapacity)) continue;
      if (holdForPause(bus, me, log)) continue;

      // Consume what the wait saw, so the next wait does not deliver it again.
      const messages = waited.status === "mail" ? bus.inbox(me, { limit: 50 }).messages : [];
      if (cancellationOnly(messages)) {
        log("received cancellation control; no model turn started");
        continue;
      }

      const role = busAgent.role;
      const taskIds = new Set<number>();
      for (const message of messages) {
        if (message.taskId && (message.type === "task" || message.type === "feedback") && !message.subject.startsWith("[ACCEPTED")) taskIds.add(message.taskId);
      }
      const tasks: Task[] = [];
      if (managed) {
        // Port of /task/start: claim for a CLI that has no bus tools.
        for (const id of taskIds) {
          try {
            const current = bus.getTask(id);
            if (claimableBy(current, me.agentId, role)) tasks.push(bus.claimTask(me, id));
            else if (current.state === "claimed" && current.assignee === me.agentId) tasks.push(current);
          } catch (error) {
            log(`claim of task #${id} failed: ${(error as Error).message}`);
          }
        }
        if (!messages.length) {
          try {
            tasks.push(bus.claimTask(me, null));
            retryClaimAtCapacity = false;
          } catch (error) {
            if (error instanceof BusError && error.code === "not_found") {
              retryClaimAtCapacity = false;
              continue;
            }
            if (isTaskCapacityConflict(error, me.agentId)) {
              // Our own submit/release frees capacity without waking us; retry on timeout.
              if (!retryClaimAtCapacity) log("at concurrent task limit; waiting for capacity");
              retryClaimAtCapacity = true;
              continue;
            }
            throw error;
          }
        }
      } else if (!messages.length) {
        tasks.push(...bus.listTasks({ states: ["open", "changes_requested"], limit: 50 }).filter((task) => claimableBy(task, me.agentId, role)));
      }
      if (!messages.length && !tasks.length) continue;

      const context: AdapterContext = {
        agent,
        prompt: buildBrief(agent, messages, tasks, managed),
        sessionId: session.sessionId,
        pinnedSessionId,
        workdir,
        mcpServerPath: qagentBin,
        fakeHarnessPath: options.fakeHarnessPath ?? DEFAULT_FAKE_HARNESS,
        busEnvironment: { QAGENT_AGENT_ID: me.agentId, QAGENT_BUS_DB: bus.dbPath, QAGENT_BLOCK_SEC: blockSec, AGENT_BUS_BLOCK_SEC: blockSec },
        mcpCommand,
      };
      await adapter.prepare?.(context);
      const invocation = adapter.build(context);
      bus.setStatus(me, "working");
      let processResult: Awaited<ReturnType<typeof runHarnessProcess>>;
      try {
        processResult = await runHarnessProcess(invocation, agent, workdir, (pid) => { childPid = pid; });
      } finally {
        childPid = null;
        if (bus.getAgent(me.agentId)?.storedStatus === "working") bus.setStatus(me, "idle");
      }
      if (options.signal?.aborted) {
        log(`stopped during a turn; ${agent.harnessDefinition.id} process group killed`);
        break;
      }
      const normalized = adapter.parse(processResult.output, processResult.code);
      const sessionMismatch = resumedUnexpectedSession(pinnedSessionId, normalized.sessionId);
      session.turns += 1;
      session.inputTokens += normalized.usage.inputTokens;
      session.outputTokens += normalized.usage.outputTokens;
      session.totalTokens += normalized.usage.totalTokens;
      session.costUSD += normalized.usage.costUSD;
      session.latencyMs += processResult.durationMs;
      if (pinnedSessionId) session.sessionId = pinnedSessionId;
      else if (normalized.sessionId) session.sessionId = normalized.sessionId;
      writeFileSync(sessionPath, JSON.stringify(session, null, 2));

      const reportIds = new Set<number>([...taskIds, ...tasks.map((task) => task.id)]);
      const failed = processResult.code !== 0 || processResult.timedOut || normalized.malformed || sessionMismatch;
      if (failed) {
        consecutiveFailures += 1;
        const error = processResult.timedOut
          ? `harness timed out after ${processResult.durationMs} ms`
          : sessionMismatch
            ? `harness resumed unexpected session ${normalized.sessionId}; expected ${pinnedSessionId}`
            : normalized.malformed ? "harness returned malformed output" : `harness exited ${processResult.code}`;
        // Port of /task/failure: each task this turn still holds is failed back (retry or escalate).
        for (const id of reportIds) {
          try {
            const task = bus.getTask(id);
            if (task.assignee === me.agentId && task.state === "claimed") bus.failTask(me, id, `supervisor: ${error}`);
          } catch (failError) {
            log(`failure report on task #${id} rejected: ${(failError as Error).message}`);
          }
        }
        const delay = retryDelayMs(consecutiveFailures);
        log(`${error}; backing off ${delay / 1000}s`);
        await sleep(delay, options.signal);
        continue;
      }

      consecutiveFailures = 0;
      if (invocation.autoReport) {
        const structured = normalized.structured ?? {};
        for (const id of reportIds) {
          try {
            const task = bus.getTask(id);
            // A CLI that submitted through its own bus tools leaves nothing to report.
            if (task.assignee !== me.agentId || task.state !== "claimed") continue;
            bus.submitTask(me, id, {
              summary: normalized.text.slice(0, 20_000),
              details: "auto-submitted by the supervisor for a harness without bus tool calls",
              changedFiles: structuredArray(structured.changedFiles).map(String),
              artifacts: structuredArray(structured.artifacts),
              validation: structuredArray(structured.validation),
            });
          } catch (error) {
            log(`auto-submit failed for task #${id}: ${(error as Error).message}`);
          }
        }
      }
      log(`turn complete in ${processResult.durationMs} ms`);
    }
  } finally {
    options.signal?.removeEventListener("abort", stopChild);
    stopChild();
    release?.();
    bus.close();
    log("supervisor stopped");
  }
}
