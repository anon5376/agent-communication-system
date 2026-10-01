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
import { getHarnessAdapter } from "./adapters.js";
import { configPathFromProject, loadConfig, resolveAgent } from "./config.js";
import { Bus } from "./core/bus.js";
import { BusError, DEFAULT_WAIT_SEC, OPERATOR_ID } from "./core/types.js";
import { ensureTaskWorktree } from "./worktree.js";
/** First non-empty environment variable among `names` (new name first, old name second). */
function envValue(...names) {
    for (const name of names) {
        const value = process.env[name];
        if (typeof value === "string" && value.trim())
            return value;
    }
    return undefined;
}
export function sanitizedEnvironment(agent, additions) {
    const env = { ...process.env, ...additions, MCP_TOOL_TIMEOUT: "3600000" };
    if (envValue("QAGENT_ALLOW_API_KEY", "AGENT_BUS_ALLOW_API_KEY") === "1" || !agent.providerDefinition.subscriptionBacked)
        return env;
    const providerKeys = {
        anthropic: ["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN"],
        openai: ["OPENAI_API_KEY"],
        google: ["GEMINI_API_KEY", "GOOGLE_API_KEY"],
        moonshot: ["MOONSHOT_API_KEY", "KIMI_API_KEY"],
        xai: ["XAI_API_KEY"],
    };
    for (const key of providerKeys[agent.modelDefinition.provider] ?? [])
        delete env[key];
    return env;
}
export function retryDelayMs(consecutiveFailures) {
    return Math.min(60_000, 2_000 * 2 ** Math.max(0, consecutiveFailures - 1));
}
export function resumedUnexpectedSession(pinnedSessionId, observedSessionId) {
    return Boolean(pinnedSessionId && observedSessionId && pinnedSessionId !== observedSessionId);
}
export function runHarnessProcess(invocation, agent, workdir, onSpawn) {
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
        const head = [];
        const tail = [];
        let headBytes = 0;
        let tailBytes = 0;
        const capture = (data) => {
            if (headBytes < MAX_HEAD_BYTES) {
                head.push(data);
                headBytes += data.length;
                return;
            }
            tail.push(data);
            tailBytes += data.length;
            while (tailBytes > MAX_TAIL_BYTES && tail.length > 1)
                tailBytes -= tail.shift().length;
        };
        let settled = false;
        let timedOut = false;
        const finish = (code) => {
            if (settled)
                return;
            settled = true;
            clearTimeout(timer);
            onSpawn?.(null);
            resolve({ code, output: Buffer.concat([...head, ...tail]).toString("utf8"), durationMs: Date.now() - started, timedOut });
        };
        const timer = setTimeout(() => {
            timedOut = true;
            if (child.pid && process.platform !== "win32") {
                try {
                    process.kill(-child.pid, "SIGTERM");
                }
                catch {
                    child.kill("SIGTERM");
                }
            }
            else {
                child.kill("SIGTERM");
            }
            setTimeout(() => {
                if (!settled && child.pid && process.platform !== "win32") {
                    try {
                        process.kill(-child.pid, "SIGKILL");
                    }
                    catch {
                        child.kill("SIGKILL");
                    }
                }
            }, 3_000).unref();
        }, invocation.timeoutMs);
        child.stdout.on("data", (data) => {
            capture(data);
            process.stdout.write(data);
        });
        child.stderr.on("data", (data) => {
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
function emptySession() {
    return { sessionId: null, turns: 0, inputTokens: 0, outputTokens: 0, totalTokens: 0, costUSD: 0, latencyMs: 0 };
}
function readSession(path) {
    try {
        const parsed = JSON.parse(readFileSync(path, "utf8"));
        return { ...emptySession(), ...parsed, sessionId: parsed.sessionId ?? null };
    }
    catch {
        return emptySession();
    }
}
/** The MCP launch line handed to the vendor CLI: `qagent mcp` as this agent, on this database. */
export function mcpCommandFor(agentId, dbPath, qagentBin = DEFAULT_QAGENT_BIN) {
    return { command: process.execPath, args: [qagentBin, "mcp"], env: { QAGENT_AGENT_ID: agentId, QAGENT_BUS_DB: resolve(dbPath) } };
}
/** Whether the supervisor claims and submits for this CLI. Harnesses without MCP cannot call bus tools. */
export function supervisorManaged(agent) {
    return !agent.harnessDefinition.features.mcp;
}
function renderMessage(message) {
    return [
        `-- from ${message.sender} · ${message.type}${message.taskId ? ` · task #${message.taskId}` : ""}`,
        message.subject,
        "",
        message.body,
        message.refs.length ? `\nReferences:\n${message.refs.map((ref) => `- ${ref.type}: ${ref.value}`).join("\n")}` : "",
    ].filter(Boolean).join("\n");
}
function renderTask(task) {
    return [
        `-- task #${task.id} · ${task.state}${task.assignee ? ` · ${task.assignee}` : " · unassigned"}${task.role ? ` · role ${task.role}` : ""}`,
        task.title,
        task.brief ? `\n${task.brief}` : "",
        task.acceptance ? `\nAcceptance:\n${task.acceptance}` : "",
        task.pathScopes.length ? `\nPath scopes (${task.project}): ${task.pathScopes.join(", ")}` : "",
    ].filter(Boolean).join("\n");
}
/** The brief for one turn. It names only the v2 bus_* tools. */
export function buildBrief(agent, messages, tasks, managed) {
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
        lines.push("The supervisor has claimed the task(s) above for you and will submit your final answer as the result.", "End the turn with the result or your question.");
    }
    else {
        lines.push("Claim a task with bus_task_claim before you start it, record progress with bus_task_note,", "and finish with bus_task_submit (summary, changed_files, validation). Reply with bus_send;", "review with bus_task_review when you are the reviewer.", "Do NOT call bus_wait: the supervisor holds the wait and will wake you again.", "End the turn after reporting the result or question.");
    }
    return lines.join("\n");
}
function cancellationOnly(messages) {
    return messages.length > 0 && messages.every((message) => message.type === "control" && message.subject.startsWith("[CANCELLED"));
}
function structuredArray(value) {
    return Array.isArray(value) ? value : [];
}
function claimableBy(task, agentId, role) {
    return (task.state === "open" || task.state === "changes_requested")
        && (task.assignee === agentId || (task.assignee === null && (task.role === "" || task.role === role)));
}
/** One pid file per agent so two supervisors never drive the same CLI session. */
function acquireLock(dir, agentId) {
    mkdirSync(dir, { recursive: true, mode: 0o700 });
    const path = join(dir, `${agentId}.pid`);
    let holder = 0;
    try {
        holder = Number(readFileSync(path, "utf8").trim()) || 0;
    }
    catch { /* no holder */ }
    if (holder && holder !== process.pid) {
        let alive = true;
        try {
            process.kill(holder, 0);
        }
        catch (error) {
            alive = error.code === "EPERM";
        }
        if (alive)
            throw new BusError("conflict", `a supervisor for ${agentId} is already running (pid ${holder})`);
    }
    writeFileSync(path, `${process.pid}\n`, { mode: 0o600 });
    return () => {
        try {
            if (Number(readFileSync(path, "utf8").trim()) === process.pid)
                rmSync(path, { force: true });
        }
        catch { /* already gone */ }
    };
}
function killGroup(pid, signal) {
    try {
        if (process.platform !== "win32")
            process.kill(-pid, signal);
        else
            process.kill(pid, signal);
    }
    catch {
        try {
            process.kill(pid, signal);
        }
        catch { /* already gone */ }
    }
}
function sleep(ms, signal) {
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
export async function supervise(options) {
    const workdir = resolve(options.workdir);
    const bus = Bus.open({ dbPath: options.dbPath });
    const home = bus.home;
    const logDir = join(home, "logs");
    const sessionDir = join(home, "sessions");
    mkdirSync(logDir, { recursive: true, mode: 0o700 });
    mkdirSync(sessionDir, { recursive: true, mode: 0o700 });
    const write = options.log ?? ((line) => { process.stdout.write(`${line}\n`); });
    const log = (line) => {
        const stamped = `[${new Date().toISOString()}] ${line}`;
        write(stamped);
        try {
            appendFileSync(join(logDir, `${options.agentId}.log`), `${stamped}\n`);
        }
        catch { /* best effort */ }
    };
    let release = null;
    let childPid = null;
    const stopChild = () => {
        const pid = childPid;
        if (!pid)
            return;
        killGroup(pid, "SIGTERM");
        setTimeout(() => { if (childPid === pid)
            killGroup(pid, "SIGKILL"); }, 3_000).unref();
    };
    options.signal?.addEventListener("abort", stopChild, { once: true });
    try {
        const me = bus.identify(options.agentId);
        const busAgent = bus.getAgent(options.agentId);
        if (!busAgent)
            throw new BusError("not_found", `agent ${options.agentId} is not on the bus; add it with \`qagent agent add ${options.agentId}\``);
        const config = options.config ?? loadConfig(options.configPath ?? configPathFromProject(workdir));
        const agent = resolveAgent(config, options.agentId);
        if (!agent.enabled)
            throw new Error(`agent ${agent.id} is disabled in the harness configuration`);
        release = acquireLock(join(home, "supervisors"), agent.id);
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
        let sweeper = null;
        let sweeperTried = false;
        const sweepStalled = () => {
            if (!options.autoRequeueMs)
                return;
            if (!sweeperTried) {
                sweeperTried = true;
                try {
                    sweeper = bus.identify(OPERATOR_ID);
                }
                catch {
                    log("auto-requeue off: no operator token on this bus");
                }
            }
            if (!sweeper)
                return;
            for (const task of bus.deadClaims(options.autoRequeueMs)) {
                try {
                    bus.requeueTask(sweeper, task.id, `auto-requeue: claim idle beyond ${Math.round(options.autoRequeueMs / 60_000)} min`);
                    log(`auto-requeued stalled task #${task.id} (was claimed by ${task.assignee ?? "nobody"})`);
                }
                catch (error) {
                    log(`auto-requeue of task #${task.id} failed: ${error.message}`);
                }
            }
        };
        let consecutiveFailures = 0;
        log(`supervising ${agent.id} via ${agent.harnessDefinition.id} in ${workdir} (bus ${bus.dbPath}${managed ? ", supervisor-managed tasks" : ""})`);
        while (!options.signal?.aborted) {
            const waited = await bus.waitForMail(me, { timeoutMs: waitMs, signal: options.signal });
            if (options.signal?.aborted)
                break;
            sweepStalled();
            if (waited.status === "timeout")
                continue;
            // Consume what the wait saw, so the next wait does not deliver it again.
            const messages = waited.status === "mail" ? bus.inbox(me, { limit: 50 }).messages : [];
            if (cancellationOnly(messages)) {
                log("received cancellation control; no model turn started");
                continue;
            }
            const role = busAgent.role;
            const taskIds = new Set();
            for (const message of messages) {
                if (message.taskId && (message.type === "task" || message.type === "feedback") && !message.subject.startsWith("[ACCEPTED"))
                    taskIds.add(message.taskId);
            }
            const tasks = [];
            if (managed) {
                // Port of /task/start: claim for a CLI that has no bus tools.
                for (const id of taskIds) {
                    try {
                        const current = bus.getTask(id);
                        if (claimableBy(current, me.agentId, role))
                            tasks.push(bus.claimTask(me, id));
                        else if (current.state === "claimed" && current.assignee === me.agentId)
                            tasks.push(current);
                    }
                    catch (error) {
                        log(`claim of task #${id} failed: ${error.message}`);
                    }
                }
                if (!messages.length) {
                    try {
                        tasks.push(bus.claimTask(me, null));
                    }
                    catch (error) {
                        if (!(error instanceof BusError && error.code === "not_found"))
                            throw error;
                    }
                }
            }
            else if (!messages.length) {
                tasks.push(...bus.listTasks({ states: ["open", "changes_requested"], limit: 50 }).filter((task) => claimableBy(task, me.agentId, role)));
            }
            if (!messages.length && !tasks.length)
                continue;
            // isolation "worktree": a turn about exactly one task this agent holds (or is assigned)
            // runs in that task's checkout. Unclaimed candidates are never isolated: every same-role
            // supervisor would race for the same checkout.
            let worktree = null;
            const focus = new Set([...taskIds, ...tasks.map((task) => task.id)]);
            if (config.constraints.isolation === "worktree" && focus.size === 1) {
                const [focusId] = focus;
                try {
                    const task = bus.getTask(focusId);
                    if (pinnedSessionId)
                        log(`task #${focusId}: worktree isolation skipped, ${agent.id} pins session ${pinnedSessionId}`);
                    else if (task.project && task.assignee === me.agentId)
                        worktree = await ensureTaskWorktree(task, home);
                }
                catch (error) {
                    log(`task #${focusId}: worktree unavailable, running in ${workdir}: ${error.message}`);
                }
            }
            const turnDir = worktree?.workdir ?? workdir;
            // CLI sessions are tied to their directory: a worktree turn resumes that task's own session.
            const taskSession = worktree ? session.taskSessions?.[worktree.branch] ?? null : null;
            if (worktree)
                log(`task #${worktree.taskId}: running in worktree ${worktree.workdir} (branch ${worktree.branch})`);
            let prompt = buildBrief(agent, messages, tasks, managed);
            if (worktree)
                prompt += `\nWork only in the git worktree ${worktree.workdir} on branch ${worktree.branch}; commit your changes there.`;
            const context = {
                agent,
                prompt,
                sessionId: worktree ? taskSession : session.sessionId,
                pinnedSessionId,
                workdir: turnDir,
                mcpServerPath: qagentBin,
                fakeHarnessPath: options.fakeHarnessPath ?? DEFAULT_FAKE_HARNESS,
                busEnvironment: { QAGENT_AGENT_ID: me.agentId, QAGENT_BUS_DB: bus.dbPath, QAGENT_BLOCK_SEC: blockSec, AGENT_BUS_BLOCK_SEC: blockSec },
                mcpCommand,
            };
            await adapter.prepare?.(context);
            const invocation = adapter.build(context);
            bus.setStatus(me, "working");
            let processResult;
            try {
                processResult = await runHarnessProcess(invocation, agent, turnDir, (pid) => { childPid = pid; });
            }
            finally {
                childPid = null;
                if (bus.getAgent(me.agentId)?.storedStatus === "working")
                    bus.setStatus(me, "idle");
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
            if (pinnedSessionId)
                session.sessionId = pinnedSessionId;
            else if (normalized.sessionId && worktree)
                session.taskSessions = { ...session.taskSessions, [worktree.branch]: normalized.sessionId };
            else if (normalized.sessionId)
                session.sessionId = normalized.sessionId;
            writeFileSync(sessionPath, JSON.stringify(session, null, 2));
            const reportIds = new Set([...taskIds, ...tasks.map((task) => task.id)]);
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
                        if (task.assignee === me.agentId && task.state === "claimed")
                            bus.failTask(me, id, `supervisor: ${error}`);
                    }
                    catch (failError) {
                        log(`failure report on task #${id} rejected: ${failError.message}`);
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
                        if (task.assignee !== me.agentId || task.state !== "claimed")
                            continue;
                        bus.submitTask(me, id, {
                            summary: normalized.text.slice(0, 20_000),
                            details: "auto-submitted by the supervisor for a harness without bus tool calls",
                            changedFiles: structuredArray(structured.changedFiles).map(String),
                            artifacts: structuredArray(structured.artifacts),
                            validation: structuredArray(structured.validation),
                        });
                    }
                    catch (error) {
                        log(`auto-submit failed for task #${id}: ${error.message}`);
                    }
                }
            }
            log(`turn complete in ${processResult.durationMs} ms`);
        }
    }
    finally {
        options.signal?.removeEventListener("abort", stopChild);
        stopChild();
        release?.();
        bus.close();
        log("supervisor stopped");
    }
}
//# sourceMappingURL=supervisor.js.map