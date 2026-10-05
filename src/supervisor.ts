/**
 * Optional supervisor (`qagent supervise <agent> [dir]`). It holds the blocking wait for
 * one agent through the core library (Bus.waitForMail), renders a brief, and starts the
 * vendor CLI through its harness adapter with an MCP entry of `qagent mcp` under the
 * agent's own identity. Claims, failures and auto-submits for harnesses without bus tools
 * go through core as that agent. Nothing here listens on a port, and nothing in core
 * imports this module.
 */
import { spawn } from "node:child_process";
import { randomBytes } from "node:crypto";
import { appendFileSync, copyFileSync, fstatSync, ftruncateSync, linkSync, mkdirSync, readFileSync, renameSync, rmSync, statSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { AdapterContext, HarnessInvocation, McpCommand, getHarnessAdapter } from "./adapters.js";
import { AgentDefinition, BusConfig, ResolvedAgent, configPathFromProject, loadConfig, resolveAgent } from "./config.js";
import { Bus } from "./core/bus.js";
import type { AgentPolicy, Identity } from "./core/identity.js";
import { BusError, DEFAULT_WAIT_SEC, Message, OPERATOR_ID, Task } from "./core/types.js";
import { TaskWorktree, ensureTaskWorktree } from "./worktree.js";

/** How long an unchanged claimable task the agent left alone waits before it is offered again. */
const REOFFER_MS = 30 * 60_000;

/** First non-empty environment variable among `names` (new name first, old name second). */
function envValue(...names: string[]): string | undefined {
  for (const name of names) {
    const value = process.env[name];
    if (typeof value === "string" && value.trim()) return value;
  }
  return undefined;
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
    // Chunked with head+tail caps: a 60-minute run can log more than memory justifies.
    // Parsers need the head (session/thread ids are announced at turn start) and the tail
    // (the result line); the middle is expendable.
    const MAX_HEAD_BYTES = 256 * 1024;
    const MAX_TAIL_BYTES = 8 * 1024 * 1024;
    const head: Buffer[] = [];
    const tail: Buffer[] = [];
    let headBytes = 0;
    let tailBytes = 0;
    const capture = (data: Buffer) => {
      if (headBytes < MAX_HEAD_BYTES) {
        head.push(data);
        headBytes += data.length;
        return;
      }
      tail.push(data);
      tailBytes += data.length;
      while (tailBytes > MAX_TAIL_BYTES && tail.length > 1) tailBytes -= tail.shift()!.length;
    };
    let settled = false;
    let timedOut = false;
    const finish = (code: number) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      onSpawn?.(null);
      resolve({ code, output: Buffer.concat([...head, ...tail]).toString("utf8"), durationMs: Date.now() - started, timedOut });
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
    child.stdout.on("data", (data: Buffer) => {
      capture(data);
      process.stdout.write(data);
    });
    child.stderr.on("data", (data: Buffer) => {
      capture(data);
      process.stderr.write(data);
    });
    child.on("error", (error) => {
      capture(Buffer.from(`\nspawn error: ${error.message}`));
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
  /** When set, each loop iteration requeues dead claims (expired, or idle this long with the assignee silent on the bus that long). Operator token required. */
  autoRequeueMs?: number;
  fakeHarnessPath?: string;
  log?: (line: string) => void;
  /** First retry delay after a failed turn or round, doubling up to 30x. Default 2000. */
  retryBaseMs?: number;
}

/**
 * Turns that fail in a row before the supervisor stops itself and tells the operator:
 * a CLI that lost its login should not burn every task and the night.
 */
export const MAX_FAILED_TURNS = 5;
/** Size at which logs/<agent>.log and logs/<agent>.out move to <file>.1 (one older copy kept). */
export const MAX_LOG_BYTES = 10 * 1024 * 1024;
/** How often a running turn renews the agent's claims. */
export const KEEPALIVE_MS = 60_000;

function rotateLog(path: string): void {
  try { if (statSync(path).size > MAX_LOG_BYTES) renameSync(path, `${path}.1`); } catch { /* no log yet */ }
}

/**
 * The launcher appends the supervisor's stdout to logs/<agent>.out and every turn's CLI output
 * is teed there. When that file is our stdout and past the cap, copy it to .out.1 and truncate
 * it in place (it is open for append, so writing carries on at the new end).
 */
function capStdoutFile(path: string): void {
  try {
    const file = statSync(path);
    if (file.size <= MAX_LOG_BYTES) return;
    const out = fstatSync(1);
    if (out.ino !== file.ino || out.dev !== file.dev) return;
    copyFileSync(path, `${path}.1`);
    ftruncateSync(1, 0);
  } catch { /* best effort */ }
}

export interface SessionRecord {
  sessionId: string | null;
  turns: number;
  inputTokens: number;
  outputTokens: number;
  totalTokens: number;
  costUSD: number;
  latencyMs: number;
  /** Session ids of worktree turns, by task branch: each task checkout has its own directory. */
  taskSessions?: Record<string, string>;
  /** Mail read from the inbox that no turn used when the supervisor last stopped; offered first on start. */
  pendingMail?: Message[];
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

/** The MCP launch line handed to the vendor CLI: `qagent mcp` as this agent, on this database. */
export function mcpCommandFor(agentId: string, dbPath: string, qagentBin = DEFAULT_QAGENT_BIN, extraEnv: Record<string, string> = {}): McpCommand {
  return { command: process.execPath, args: [qagentBin, "mcp"], env: { QAGENT_AGENT_ID: agentId, QAGENT_BUS_DB: resolve(dbPath), ...extraEnv } };
}

/**
 * The project configuration's limits for this agent, as the policy the bus core enforces on
 * every call path (MCP, CLI, supervisor): delegation, the agents it may assign, delegation
 * depth, and how many tasks it may hold claimed at once.
 */
export function policyFromConfig(config: BusConfig, agent: AgentDefinition): AgentPolicy {
  const policy: AgentPolicy = {
    canDelegate: agent.permissions.canDelegate,
    maxDelegationDepth: Math.min(agent.permissions.maxDelegationDepth, config.constraints.maxDelegationDepth),
    maxConcurrentTasks: config.constraints.maxConcurrentTasks,
  };
  if (agent.permissions.allowedChildAgentIds?.length) policy.allowedChildAgentIds = [...agent.permissions.allowedChildAgentIds];
  return policy;
}

/**
 * Why the configuration's usage budget stops new turns, or null. The budget counts what the
 * CLI itself reported into sessions/<agent>.json and is checked between turns, so one turn
 * can overshoot it, and a CLI that reports no cost never moves the dollar count.
 */
export function budgetReached(config: BusConfig, session: Pick<SessionRecord, "totalTokens" | "costUSD">): string | null {
  const tokens = config.constraints.optionalTokenBudget;
  const dollars = config.constraints.optionalApiCostBudgetUSD;
  if (tokens !== null && tokens !== undefined && session.totalTokens >= tokens) return `${session.totalTokens} of ${tokens} reported tokens used`;
  if (dollars !== null && dollars !== undefined && session.costUSD >= dollars) return `$${session.costUSD.toFixed(2)} of $${dollars.toFixed(2)} reported cost used`;
  return null;
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

function pidAlive(pid: number): boolean {
  try { process.kill(pid, 0); return true; } catch (error) { return (error as NodeJS.ErrnoException).code === "EPERM"; }
}

/**
 * Whether `pid` is a live supervisor for `agentId`. After a reboot a stale pid file can name
 * an unrelated process that reused the pid, so where /proc shows the command line it must
 * mention supervise; elsewhere any live pid counts (the safe direction: refuse to start).
 */
export function supervisorAlive(pid: number, agentId: string): boolean {
  if (pid <= 0 || !pidAlive(pid)) return false;
  if (pid === process.pid) return true;
  try {
    const words = readFileSync(`/proc/${pid}/cmdline`, "utf8").split("\0");
    return words.includes("supervise") && (words.includes(agentId) || words.includes("--roster"));
  } catch {
    return true;
  }
}

function readPid(path: string): number {
  try { return Number(readFileSync(path, "utf8").trim()) || 0; } catch { return 0; }
}

/**
 * One pid file per agent so two supervisors never drive the same CLI session. Taking it is
 * atomic: the pid is written to a private file first and hard-linked into place, which fails
 * if any supervisor holds it, so the file is never empty and two starters cannot both win.
 * A stale file (its pid is gone) is removed only while holding <agent>.pid.reap, so a
 * supervisor that just took the lock is never removed by a slower starter.
 */
export function acquireSupervisorLock(dir: string, agentId: string): () => void {
  mkdirSync(dir, { recursive: true, mode: 0o700 });
  const path = join(dir, `${agentId}.pid`);
  const mine = `${path}.${process.pid}.${randomBytes(4).toString("hex")}.tmp`;
  writeFileSync(mine, `${process.pid}\n`, { mode: 0o600 });
  try {
    for (let attempt = 0; attempt < 20; attempt += 1) {
      try {
        linkSync(mine, path);
        return () => { if (readPid(path) === process.pid) rmSync(path, { force: true }); };
      } catch (error) {
        if ((error as NodeJS.ErrnoException).code !== "EEXIST") throw error;
      }
      const holder = readPid(path);
      if (holder === process.pid) return () => { if (readPid(path) === process.pid) rmSync(path, { force: true }); };
      if (holder && supervisorAlive(holder, agentId)) throw new BusError("conflict", `a supervisor for ${agentId} is already running (pid ${holder})`);
      reapStaleLock(path, holder);
    }
    throw new BusError("conflict", `could not take the supervisor lock for ${agentId}; another supervisor keeps starting`);
  } finally {
    rmSync(mine, { force: true });
  }
}

function reapStaleLock(path: string, stalePid: number): void {
  const reap = `${path}.reap`;
  const mine = `${reap}.${process.pid}.tmp`;
  writeFileSync(mine, `${process.pid}\n`, { mode: 0o600 });
  try {
    try {
      linkSync(mine, reap);
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== "EEXIST") throw error;
      // Another starter is reaping. A reaper that died mid-way leaves its file behind; clear it once it is clearly abandoned.
      const reaper = readPid(reap);
      let age = 0;
      try { age = Date.now() - statSync(reap).mtimeMs; } catch { return; }
      if (!reaper || (!pidAlive(reaper) && age > 2_000)) rmSync(reap, { force: true });
      return;
    }
    try {
      // Holding the reap lock, nobody else can remove the pid file; only a link can create it, which fails while it exists.
      if (readPid(path) === stalePid) rmSync(path, { force: true });
    } finally {
      rmSync(reap, { force: true });
    }
  } finally {
    rmSync(mine, { force: true });
  }
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
    const logPath = join(logDir, `${options.agentId}.log`);
    rotateLog(logPath);
    try { appendFileSync(logPath, `${stamped}\n`); } catch { /* best effort */ }
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
    const worktreeMode = config.constraints.isolation === "worktree";
    const pinnedForCheck = agent.resumeSessionId?.trim() || null;
    if (worktreeMode && pinnedForCheck) {
      throw new BusError("invalid", `isolation "worktree" cannot run ${agent.id}: it pins session ${pinnedForCheck}, and a CLI session is tied to one directory. Remove resumeSessionId or set isolation to "path-locks".`);
    }
    release = acquireSupervisorLock(join(home, "supervisors"), agent.id);

    const adapter = getHarnessAdapter(agent.harnessDefinition.adapter);
    const managed = supervisorManaged(agent);
    const sessionPath = join(sessionDir, `${agent.id}.json`);
    const session = readSession(sessionPath);
    const pinnedSessionId = agent.resumeSessionId?.trim() || null;
    if (pinnedSessionId && session.sessionId !== pinnedSessionId) {
      session.sessionId = pinnedSessionId;
      writeFileSync(sessionPath, JSON.stringify(session, null, 2));
    }
    const dollarBudget = config.constraints.optionalApiCostBudgetUSD;
    if (dollarBudget !== null && dollarBudget !== undefined && !agent.harnessDefinition.features.usageReporting) {
      throw new BusError("invalid", `optionalApiCostBudgetUSD is set, but ${agent.harnessDefinition.id} reports no usage, so the budget could never be counted. Remove it or use a CLI that reports cost.`);
    }
    let operator: Identity | null = null;
    try { operator = bus.identify(OPERATOR_ID); } catch { /* no operator token on this bus */ }
    // The configuration's limits go into the bus, where every call path enforces them. No work
    // runs until they are in: a rejected policy stops the supervisor, a locked bus is waited out.
    const startupRetryBaseMs = options.retryBaseMs ?? 2_000;
    for (let attempt = 1; ; attempt++) {
      try {
        bus.setAgentPolicy(operator ?? me, me.agentId, policyFromConfig(config, agent));
        break;
      } catch (error) {
        if (error instanceof BusError || !/database is (locked|busy)/i.test((error as Error).message)) throw error;
        if (options.signal?.aborted) return;
        const delay = retryDelayMs(attempt) * startupRetryBaseMs / 2_000;
        log(`configuration limits not applied yet (${(error as Error).message}); retrying in ${delay / 1000}s`);
        await sleep(delay, options.signal);
        if (options.signal?.aborted) return;
      }
    }
    const qagentBin = options.qagentBin ?? DEFAULT_QAGENT_BIN;
    const isolationEnv: Record<string, string> = worktreeMode ? { QAGENT_REQUIRE_WORKTREE: "1" } : {};
    const mcpCommand = mcpCommandFor(me.agentId, bus.dbPath, qagentBin, isolationEnv);
    const blockSec = agent.harnessDefinition.id === "claude" ? "900" : "240";
    const waitMs = options.waitMs ?? DEFAULT_WAIT_SEC * 1000;
    const sweepStalled = () => {
      if (!options.autoRequeueMs) return;
      if (!operator) {
        log("auto-requeue off: no operator token on this bus");
        options.autoRequeueMs = undefined;
        return;
      }
      for (const task of bus.deadClaims(options.autoRequeueMs)) {
        try {
          bus.requeueTask(operator, task.id, `auto-requeue: claim idle beyond ${Math.round(options.autoRequeueMs / 60_000)} min`);
          log(`auto-requeued stalled task #${task.id} (was claimed by ${task.assignee ?? "nobody"})`);
        } catch (error) {
          log(`auto-requeue of task #${task.id} failed: ${(error as Error).message}`);
        }
      }
    };
    // Work already waiting is offered once per change (task updated_ms) so an agent that leaves a
    // task alone is not woken (a paid turn) for it again and again; an unchanged task it left
    // alone is offered once more after REOFFER_MS.
    const offered = new Map<number, { updatedMs: number; at: number }>();
    const backlog = (): Task[] => bus.claimableTasks(me.agentId).filter((task) => {
      const seen = offered.get(task.id);
      return !seen || seen.updatedMs !== task.updatedMs || Date.now() - seen.at >= REOFFER_MS;
    });
    const markOffered = (task: Task): void => { offered.set(task.id, { updatedMs: task.updatedMs, at: Date.now() }); };
    // Worktree mode shows only mail about the turn's task; the rest is kept here for later turns,
    // since reading the inbox has already moved the cursor past it.
    let deferred: Message[] = session.pendingMail ?? [];
    delete session.pendingMail;
    // Tasks claimed in this round before its turn ran: released again if the round fails.
    let claimedThisRound: number[] = [];
    const holdOrClaim = (id: number): Task | null => {
      const current = bus.getTask(id);
      if (current.state === "claimed" && current.assignee === me.agentId) return current;
      if (claimableBy(current, me.agentId, busAgent.role)) {
        const claimed = bus.claimTask(me, id);
        claimedThisRound.push(claimed.id);
        return claimed;
      }
      return null;
    };
    let budgetNoticed: string | null = null;
    let costNoticed = false;
    let consecutiveFailures = 0;
    const retryBaseMs = options.retryBaseMs ?? 2_000;
    const backoff = (failures: number) => retryDelayMs(failures) * retryBaseMs / 2_000;
    // A bus or file error inside a round (a locked database, a full disk) ends the round, not
    // the supervisor. Mail the round read but no turn used goes back to deferred for the next one.
    let errorStreak = 0;
    let unused: Message[] = [];
    log(`supervising ${agent.id} via ${agent.harnessDefinition.id} in ${workdir} (bus ${bus.dbPath}${managed ? ", supervisor-managed tasks" : ""}${worktreeMode ? ", one task per turn in its own git worktree" : ""})`);

    while (!options.signal?.aborted) {
      unused = [];
      claimedThisRound = [];
      try {
        const over = budgetReached(config, session);
        if (over) {
          if (budgetNoticed !== over) log(`budget reached (${over}); no new turns until the configuration budget is raised`);
          budgetNoticed = over;
          await sleep(waitMs, options.signal);
          continue;
        }
        // Existing claimable work or unread mail starts a turn at once; only an idle agent waits.
        const waiting = backlog();
        if (!waiting.length && !deferred.length && bus.unreadCount(me.agentId) === 0) {
          const waited = await bus.waitForMail(me, { timeoutMs: waitMs, signal: options.signal });
          if (options.signal?.aborted) break;
          sweepStalled();
          if (waited.status === "timeout") continue;
        } else {
          sweepStalled();
        }

        // Consume what is unread, so the next wait does not deliver it again.
        let messages = [...deferred, ...bus.inbox(me, { limit: 50 }).messages];
        deferred = [];
        unused = messages;
        if (cancellationOnly(messages)) {
          log("received cancellation control; no model turn started");
          continue;
        }

        const role = busAgent.role;
        const taskIds = new Set<number>();
        for (const message of messages) {
          if (message.taskId && (message.type === "task" || message.type === "feedback") && !message.subject.startsWith("[ACCEPTED")) taskIds.add(message.taskId);
        }
        const candidates = backlog();
        for (const task of candidates) markOffered(task);
        const tasks: Task[] = [];
        let worktree: TaskWorktree | null = null;
        if (worktreeMode) {
          // isolation "worktree": the supervisor claims exactly one task per turn and runs the turn in
          // that task's checkout. If no checkout can be made, the claim is released and no turn runs:
          // the work never falls back to the shared directory. Other tasks wait for later turns.
          const order = [...taskIds, ...candidates.map((task) => task.id)];
          let focus: Task | null = null;
          let claimedNow = false;
          for (const id of new Set(order)) {
            try {
              const before = bus.getTask(id);
              const held = holdOrClaim(id);
              if (held) { focus = held; claimedNow = before.state !== "claimed"; break; }
            } catch (error) {
              log(`claim of task #${id} failed: ${(error as Error).message}`);
            }
          }
          if (focus) {
            try {
              worktree = await ensureTaskWorktree(focus, home);
            } catch (error) {
              const reason = `worktree isolation unavailable, so no turn ran in the shared checkout: ${(error as Error).message}`;
              log(`task #${focus.id}: ${reason}`);
              try {
                bus.noteTask(me, focus.id, reason);
                if (claimedNow) bus.releaseTask(me, focus.id, "worktree isolation unavailable");
              } catch (releaseError) {
                log(`task #${focus.id}: could not release after the worktree failure: ${(releaseError as Error).message}`);
              }
              markOffered(bus.getTask(focus.id));
              // Mail about the refused task is answered by its note; all other mail waits for a later turn.
              const refused = focus.id;
              deferred = messages.filter((message) => message.taskId !== refused);
              continue;
            }
            tasks.push(focus);
            // Mail about tasks this turn does not hold would invite work outside the checkout.
            deferred = messages.filter((message) => message.taskId && message.taskId !== focus!.id);
            messages = messages.filter((message) => !message.taskId || message.taskId === focus!.id);
            taskIds.clear();
            taskIds.add(focus.id);
          }
        } else if (managed) {
          // Port of /task/start: claim for a CLI that has no bus tools.
          for (const id of taskIds) {
            try {
              const held = holdOrClaim(id);
              if (held) tasks.push(held);
            } catch (error) {
              log(`claim of task #${id} failed: ${(error as Error).message}`);
            }
          }
          if (!messages.length) {
            try {
              const claimed = bus.claimTask(me, null);
              claimedThisRound.push(claimed.id);
              tasks.push(claimed);
            } catch (error) {
              if (!(error instanceof BusError && (error.code === "not_found" || error.code === "conflict"))) throw error;
            }
          }
        } else if (!messages.length) {
          tasks.push(...candidates);
        }
        if (!messages.length && !tasks.length) continue;

        const turnDir = worktree?.workdir ?? workdir;
        // CLI sessions are tied to their directory: a worktree turn resumes that task's own session.
        const taskSession = worktree ? session.taskSessions?.[worktree.branch] ?? null : null;
        if (worktree) log(`task #${worktree.taskId}: running in worktree ${worktree.workdir} (branch ${worktree.branch})`);
        let prompt = buildBrief(agent, messages, tasks, managed);
        if (worktree) {
          prompt += `\nTask #${worktree.taskId} is claimed for you. Work only in the git worktree ${worktree.workdir} on branch ${worktree.branch}; commit your changes there.`;
        } else if (worktreeMode) {
          prompt += "\nThis bus isolates each task in its own git worktree: claim a task before editing files, and edit only in the worktree the claim returns.";
        }

        const context: AdapterContext = {
          agent,
          prompt,
          sessionId: worktree ? taskSession : session.sessionId,
          pinnedSessionId,
          workdir: turnDir,
          mcpServerPath: qagentBin,
          fakeHarnessPath: options.fakeHarnessPath ?? DEFAULT_FAKE_HARNESS,
          busEnvironment: { QAGENT_AGENT_ID: me.agentId, QAGENT_BUS_DB: bus.dbPath, QAGENT_BLOCK_SEC: blockSec, AGENT_BUS_BLOCK_SEC: blockSec, ...isolationEnv },
          mcpCommand,
        };
        await adapter.prepare?.(context);
        const invocation = adapter.build(context);
        bus.setStatus(me, "working");
        // From here a paid turn runs: its mail is used, and an error must not hand it to another turn.
        // Its claims are the turn's too, and a failed turn fails them back.
        unused = [];
        claimedThisRound = [];
        let processResult: Awaited<ReturnType<typeof runHarnessProcess>>;
        // A turn longer than the claim TTL keeps its tasks: renew the claims once a minute.
        const keepalive = setInterval(() => { try { bus.renewClaims(me); } catch { /* next beat */ } }, KEEPALIVE_MS);
        try {
          processResult = await runHarnessProcess(invocation, agent, turnDir, (pid) => { childPid = pid; });
        } finally {
          clearInterval(keepalive);
          capStdoutFile(join(logDir, `${options.agentId}.out`));
          childPid = null;
          try {
            if (bus.getAgent(me.agentId)?.storedStatus === "working") bus.setStatus(me, "idle");
          } catch (error) {
            log(`could not set status idle: ${(error as Error).message}`);
          }
        }
        if (options.signal?.aborted) {
          log(`stopped during a turn; ${agent.harnessDefinition.id} process group killed`);
          break;
        }
        let normalized: ReturnType<typeof adapter.parse>;
        try {
          normalized = adapter.parse(processResult.output, processResult.code);
        } catch (error) {
          log(`could not read the turn's output: ${(error as Error).message}`);
          normalized = { text: "", sessionId: null, usage: { inputTokens: 0, outputTokens: 0, totalTokens: 0, costUSD: 0 }, structured: null, malformed: true };
        }
        const sessionMismatch = resumedUnexpectedSession(pinnedSessionId, normalized.sessionId);
        session.turns += 1;
        session.inputTokens += normalized.usage.inputTokens;
        session.outputTokens += normalized.usage.outputTokens;
        session.totalTokens += normalized.usage.totalTokens;
        session.costUSD += normalized.usage.costUSD;
        session.latencyMs += processResult.durationMs;
        if (dollarBudget !== null && dollarBudget !== undefined && !costNoticed && normalized.usage.costUSD === 0) {
          costNoticed = true;
          log(`${agent.harnessDefinition.id} reported no cost for this turn; the $${dollarBudget} budget only counts cost the CLI reports`);
        }
        if (pinnedSessionId) session.sessionId = pinnedSessionId;
        else if (normalized.sessionId && worktree) session.taskSessions = { ...session.taskSessions, [worktree.branch]: normalized.sessionId };
        else if (normalized.sessionId) session.sessionId = normalized.sessionId;
        try {
          writeFileSync(sessionPath, JSON.stringify(session, null, 2));
        } catch (error) {
          log(`could not save usage to ${sessionPath}: ${(error as Error).message}`);
        }

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
          // Mail that names no task is offered to the next turn instead of being lost with this
          // one. Task mail is not: a failed-back task gets a retry message, and any other task's
          // state on the bus (claimable or not) already decides whether it comes back. Keeping it
          // would rerun turns for tasks this agent can no longer claim.
          deferred = [...messages.filter((m) => m.taskId === null), ...deferred];
          if (consecutiveFailures >= MAX_FAILED_TURNS) {
            const why = `${consecutiveFailures} turns failed in a row; last: ${error}`;
            try {
              bus.send(me, {
                to: OPERATOR_ID,
                subject: `${me.agentId} stopped: ${consecutiveFailures} turns failed in a row`,
                body: `${me.agentId}'s supervisor stopped after ${consecutiveFailures} failed turns in a row, so it stops using up tasks and spend. `
                  + `The last one: ${error}. Its log is logs/${me.agentId}.log under the bus folder. `
                  + `Fix the cause (often the CLI's login, or the CLI missing from PATH), then start the supervisor again.`,
                type: "info",
              });
            } catch (sendError) {
              log(`could not tell the operator: ${(sendError as Error).message}`);
            }
            log(`stopping: ${why}`);
            break;
          }
          const delay = backoff(consecutiveFailures);
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
        errorStreak = 0;
      } catch (error) {
        const kept = new Set(deferred.map((m) => m.seq));
        deferred = [...unused.filter((m) => !kept.has(m.seq)), ...deferred];
        unused = [];
        // A round that failed before its turn ran gives back what it claimed, so a round that keeps
        // failing does not take a new task each time without working on it.
        for (const id of claimedThisRound) {
          try {
            const task = bus.getTask(id);
            if (task.assignee === me.agentId && task.state === "claimed") bus.releaseTask(me, id, "supervisor round failed before the turn ran");
          } catch (releaseError) {
            log(`task #${id}: could not release after the failed round: ${(releaseError as Error).message}`);
          }
        }
        claimedThisRound = [];
        errorStreak += 1;
        const delay = backoff(errorStreak);
        log(`round failed: ${(error as Error).message}; retrying in ${delay / 1000}s`);
        await sleep(delay, options.signal);
      }
    }
    // Mail already read from the inbox that no turn used waits in the session file for the next start.
    const pending = [...unused, ...deferred.filter((m) => !unused.some((u) => u.seq === m.seq))];
    if (pending.length) session.pendingMail = pending;
    try {
      writeFileSync(sessionPath, JSON.stringify(session, null, 2));
    } catch (error) {
      log(`could not save unread mail to ${sessionPath}: ${(error as Error).message}`);
    }
  } finally {
    options.signal?.removeEventListener("abort", stopChild);
    stopChild();
    release?.();
    bus.close();
    log("supervisor stopped");
  }
}
