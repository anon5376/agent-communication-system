/**
 * qagent v2 command dispatch. Every command opens the SQLite file directly; there
 * is no broker. mcp, mcp-config, supervise, doctor and dashboard are loaded lazily;
 * each module exports
 *   main(argv: string[], context: { dbPath: string; command: string }): Promise<number>.
 */
import { readFileSync, writeFileSync } from "node:fs";
import { Bus } from "../core/bus.js";
import { ChangeWatcher } from "../core/changes.js";
import { homeFor, resolveDbPath } from "../core/db.js";
import type { Identity } from "../core/identity.js";
import { agentIdFromEnv } from "../core/identity.js";
import { defaultImportSources, runImport } from "../core/import.js";
import { waitForMail, waitSeconds } from "../notify/wait.js";
import { BusError, MessageType, OPERATOR_ID, Priority, TaskState } from "../core/types.js";
import { renderAgents, renderEvent, renderImport, renderMessages, renderStatus, renderTask, renderTasks, renderTrace, renderTraceHtml } from "./format.js";

export interface CliIo {
  stdout: (text: string) => void;
  stderr: (text: string) => void;
  readStdin: () => string;
  env: NodeJS.ProcessEnv;
}

const defaultIo: CliIo = {
  stdout: (text) => { process.stdout.write(text); },
  stderr: (text) => { process.stderr.write(text); },
  readStdin: () => readFileSync(0, "utf8"),
  env: process.env,
};

const BOOLEAN_FLAGS = new Set(["json", "peek", "all", "mine", "ack", "accept", "revise", "dry-run", "force", "follow", "operator", "open", "help"]);
const REPEATED_FLAGS = new Set(["dep", "scope", "state", "file"]);

interface Parsed {
  positionals: string[];
  flags: Map<string, string | boolean | string[]>;
}

export function parseArgs(argv: string[]): Parsed {
  const positionals: string[] = [];
  const flags = new Map<string, string | boolean | string[]>();
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === "--") { positionals.push(...argv.slice(index + 1)); break; }
    if (arg === "-h") { flags.set("help", true); continue; }
    if (!arg.startsWith("--") || arg === "-") { positionals.push(arg); continue; }
    const eq = arg.indexOf("=");
    const name = eq > 0 ? arg.slice(2, eq) : arg.slice(2);
    let value: string | boolean;
    if (BOOLEAN_FLAGS.has(name)) {
      value = eq > 0 ? !/^(0|false|no)$/i.test(arg.slice(eq + 1)) : true;
    } else if (eq > 0) {
      value = arg.slice(eq + 1);
    } else {
      if (index + 1 >= argv.length) throw new BusError("invalid", `--${name} needs a value`);
      value = argv[index += 1];
    }
    if (REPEATED_FLAGS.has(name)) {
      const list = (flags.get(name) as string[] | undefined) ?? [];
      list.push(String(value));
      flags.set(name, list);
    } else {
      flags.set(name, value);
    }
  }
  return { positionals, flags };
}

export const USAGE = `qagent - coordination over one SQLite file (no daemon)

Global: --db PATH (or QAGENT_BUS_DB; default ~/.agent-bus/bus.db)  --as ID|operator  --json

  qagent init                                     create bus.db and the operator token
  qagent agent add <id> --role R [--model M --harness H --parent P --authority worker|manager]
  qagent agent list | qagent token rotate <id>
  qagent whoami | qagent status
  qagent send <to|a,b|*> <subject> [body|-] [--thread T] [--task N] [--type T] [--ack]
  qagent inbox [--peek] [--limit N]
  qagent ack <seq>
  qagent wait [--timeout SEC]                     exit 0 = mail or task event, 2 = timeout
  qagent task add <title> [--brief B|-] [--to ID] [--reviewer ID] [--role R] [--priority P]
                  [--acceptance A] [--parent N] [--dep N]... [--scope PATH]... [--project DIR]
  qagent task list [--mine] [--state S]... [--all] [--limit N] | task show <N>
  qagent task claim [<N>] | task note <N> <text> | task submit <N> --summary S [--details D] [--file F]...
  qagent task review <N> --accept|--revise --feedback F | task cancel <N> [--reason R]
  qagent task stalled [--stall-min M] | task requeue <N> [--reason R]
  qagent log [--follow] [--since SEQ] [--limit N]
  qagent trace <task-N> [--format text|json|html] [--out FILE]   the task's causal chain
  qagent import [--jsonl P] [--qagent-state P] [--prototype P] [--dry-run] [--force]
  qagent mcp [--operator] | mcp-config | supervise <agent> [dir] | doctor | dashboard
`;

const LAZY: Record<string, { path: string; lane: string }> = {
  "mcp": { path: "../mcp/server.js", lane: "lane 2" },
  "mcp-config": { path: "../mcp/config.js", lane: "lane 2" },
  "supervise": { path: "../supervisor/entry.js", lane: "lane 3" },
  "doctor": { path: "../supervisor/entry.js", lane: "lane 3" },
  "dashboard": { path: "../dashboard/entry.js", lane: "lane 4" },
};

class Context {
  private busInstance: Bus | null = null;
  constructor(readonly parsed: Parsed, readonly io: CliIo, readonly dbPath: string) {}

  get json(): boolean { return this.flag("json") === true; }

  flag(name: string): string | boolean | string[] | undefined {
    return this.parsed.flags.get(name);
  }

  str(name: string): string | undefined {
    const value = this.flag(name);
    if (value === undefined || typeof value === "boolean") return undefined;
    return Array.isArray(value) ? value[value.length - 1] : value;
  }

  list(name: string): string[] {
    const value = this.flag(name);
    return Array.isArray(value) ? value : typeof value === "string" ? [value] : [];
  }

  int(name: string): number | undefined {
    const value = this.str(name);
    if (value === undefined) return undefined;
    const parsed = Number(value);
    if (!Number.isInteger(parsed)) throw new BusError("invalid", `--${name} must be an integer`);
    return parsed;
  }

  position(index: number, label: string): string {
    const value = this.parsed.positionals[index];
    if (value === undefined || value === "") throw new BusError("invalid", `missing ${label}`);
    return value;
  }

  taskId(index: number): number {
    const raw = this.position(index, "task number").replace(/^#/, "");
    const id = Number(raw);
    if (!Number.isInteger(id) || id <= 0) throw new BusError("invalid", `invalid task number: ${raw}`);
    return id;
  }

  /** Read "-" from stdin. */
  maybeStdin(value: string | undefined): string | undefined {
    return value === "-" ? this.io.readStdin() : value;
  }

  get bus(): Bus {
    if (!this.busInstance) this.busInstance = Bus.open({ dbPath: this.dbPath });
    return this.busInstance;
  }

  /** The caller: --as, then QAGENT_AGENT_ID / AGENT_ID; operator commands fall back to the operator. */
  identity(operatorDefault = false): Identity {
    const chosen = this.str("as") ?? agentIdFromEnv(this.io.env) ?? (operatorDefault ? OPERATOR_ID : null);
    if (!chosen) throw new BusError("unauthorized", "no agent identity: set QAGENT_AGENT_ID or pass --as <id>");
    return this.bus.identify(chosen);
  }

  out(value: unknown, text: string): void {
    this.io.stdout(this.json ? `${JSON.stringify(value, null, 2)}\n` : `${text}\n`);
  }

  close(): void {
    this.busInstance?.close();
  }
}

/** Index of the command word: the first positional, skipping the values of flags that take one. */
export function commandPosition(argv: string[]): number {
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === "--") return index + 1 < argv.length ? index + 1 : -1;
    if (arg === "-h" || arg === "-") continue;
    if (arg.startsWith("--")) {
      if (!arg.includes("=") && !BOOLEAN_FLAGS.has(arg.slice(2))) index += 1;
      continue;
    }
    return index;
  }
  return -1;
}

async function runLazy(command: string, ctx: Context, argv: string[]): Promise<number> {
  const target = LAZY[command];
  const url = new URL(target.path, import.meta.url).href;
  let module: { main?: (argv: string[], context: { dbPath: string; command: string }) => Promise<number> };
  try {
    module = await import(url) as typeof module;
  } catch (error) {
    if ((error as { code?: string }).code === "ERR_MODULE_NOT_FOUND" && String((error as Error).message).includes(target.path.replace("../", ""))) {
      ctx.io.stderr(`qagent: \`${command}\` is not built yet (${target.lane}).\n`);
      return 1;
    }
    throw error;
  }
  if (typeof module.main !== "function") {
    ctx.io.stderr(`qagent: ${target.path} does not export main().\n`);
    return 1;
  }
  return module.main(argv, { dbPath: ctx.dbPath, command });
}

async function waitCommand(ctx: Context): Promise<number> {
  const me = ctx.identity();
  const seconds = waitSeconds(ctx.int("timeout"), ctx.io.env);
  const controller = new AbortController();
  let interrupted = false;
  const stop = () => { interrupted = true; controller.abort(); };
  process.once("SIGINT", stop);
  process.once("SIGTERM", stop);
  try {
    const result = await waitForMail(ctx.bus, me, { timeoutMs: seconds * 1000, signal: controller.signal });
    const text = result.status === "mail"
      ? renderMessages(result.messages)
      : result.status === "task"
        ? result.events.map(renderEvent).join("\n")
        : `no mail for ${me.agentId} within ${seconds}s`;
    ctx.out(result, text);
    if (interrupted) return 130;
    return result.status === "timeout" ? 2 : 0;
  } finally {
    process.off("SIGINT", stop);
    process.off("SIGTERM", stop);
  }
}

async function logCommand(ctx: Context): Promise<number> {
  const bus = ctx.bus;
  let since = ctx.int("since") ?? (ctx.flag("follow") ? bus.latestSeq() : 0);
  const limit = ctx.int("limit") ?? 200;
  const print = () => {
    const events = bus.events(since, ctx.flag("follow") ? 5000 : limit);
    for (const event of events) ctx.io.stdout(ctx.json ? `${JSON.stringify(event)}\n` : `${renderEvent(event)}\n`);
    if (events.length) since = events[events.length - 1].seq;
    return events.length;
  };
  if (!ctx.flag("follow")) {
    if (print() === 0 && !ctx.json) ctx.io.stdout("(no events)\n");
    return 0;
  }
  const watcher = new ChangeWatcher(bus.db, bus.dbPath, { maxPollMs: 250 });
  const controller = new AbortController();
  const stop = () => controller.abort();
  process.once("SIGINT", stop);
  process.once("SIGTERM", stop);
  try {
    print();
    while (!controller.signal.aborted) {
      const seq = await watcher.next(since, 60_000, controller.signal);
      if (seq > since) print();
    }
    return 0;
  } finally {
    watcher.close();
    process.off("SIGINT", stop);
    process.off("SIGTERM", stop);
  }
}

async function dispatch(ctx: Context): Promise<number> {
  const [command, sub] = ctx.parsed.positionals;
  switch (command) {
    case "init": {
      const result = ctx.bus.init();
      ctx.out(result, `bus ${result.dbPath}\noperator token ${result.operatorTokenPath} (${result.operator})`);
      return 0;
    }
    case "whoami": {
      const me = ctx.identity();
      const who = ctx.bus.whoami(me);
      ctx.out(who, `${me.agentId} (${me.authority}${who.agent?.role ? `, role ${who.agent.role}` : ""}) unread ${who.unread} cursor ${who.cursor}\nbus ${who.dbPath}`);
      return 0;
    }
    case "status": {
      const status = ctx.bus.status();
      ctx.out(status, renderStatus(status));
      return 0;
    }
    case "agent": {
      if (sub === "list") {
        const agents = ctx.bus.listAgents();
        ctx.out(agents, renderAgents(agents));
        return 0;
      }
      if (sub === "add") {
        const authority = ctx.str("authority") ?? "worker";
        if (authority !== "worker" && authority !== "manager") throw new BusError("invalid", "--authority must be worker or manager");
        const result = ctx.bus.addAgent(ctx.identity(true), {
          id: ctx.position(2, "agent id"), role: ctx.str("role") ?? "", model: ctx.str("model"), harness: ctx.str("harness"),
          parent: ctx.str("parent") ?? null, authority,
        });
        ctx.out(result, `added ${result.agent.id}; token ${result.tokenPath}`);
        return 0;
      }
      throw new BusError("invalid", "usage: qagent agent add <id> --role R | qagent agent list");
    }
    case "token": {
      if (sub !== "rotate") throw new BusError("invalid", "usage: qagent token rotate <id>");
      const result = ctx.bus.rotateToken(ctx.identity(true), ctx.position(2, "agent id"));
      ctx.out(result, `rotated; token ${result.tokenPath}`);
      return 0;
    }
    case "send": {
      const me = ctx.identity();
      const to = ctx.position(1, "recipient");
      const subject = ctx.position(2, "subject");
      const body = ctx.maybeStdin(ctx.parsed.positionals[3]) ?? "";
      const task = ctx.int("task");
      const type = (ctx.str("type") ?? "info") as MessageType;
      const sent = ctx.bus.send(me, { to, subject, body, type, thread: ctx.str("thread"), taskId: task ?? null, requiresAck: ctx.flag("ack") === true });
      ctx.out(sent, sent.map((message) => `sent #${message.seq} to ${message.recipient ?? "*"}`).join("\n"));
      return 0;
    }
    case "inbox": {
      const me = ctx.identity();
      const result = ctx.bus.inbox(me, { peek: ctx.flag("peek") === true, limit: ctx.int("limit") });
      ctx.out(result, `${renderMessages(result.messages)}${result.remaining > 0 ? `\n(${result.remaining} more unread)` : ""}`);
      return 0;
    }
    case "ack": {
      const seq = Number(ctx.position(1, "message seq").replace(/^#/, ""));
      if (!Number.isInteger(seq)) throw new BusError("invalid", "ack needs a message sequence number");
      const result = ctx.bus.ack(ctx.identity(), seq);
      ctx.out(result, `acknowledged #${result.seq}`);
      return 0;
    }
    case "wait":
      return waitCommand(ctx);
    case "log":
      return logCommand(ctx);
    case "task":
      return taskCommand(ctx, sub);
    case "trace": {
      const trace = ctx.bus.traceTask(ctx.taskId(1));
      const format = ctx.str("format") ?? (ctx.str("out")?.endsWith(".html") ? "html" : "text");
      if (format === "html") {
        const out = ctx.str("out");
        if (!out) throw new BusError("invalid", "--format html requires --out FILE");
        writeFileSync(out, renderTraceHtml(trace));
        ctx.out({ out }, `wrote ${out}`);
        return 0;
      }
      if (format === "json") {
        ctx.out(trace, renderTrace(trace));
        return 0;
      }
      if (format !== "text") throw new BusError("invalid", "--format must be text, json, or html");
      ctx.out(trace, renderTrace(trace));
      return 0;
    }
    case "import": {
      const explicit = ["jsonl", "qagent-state", "prototype"].some((name) => ctx.str(name) !== undefined);
      const sources = explicit
        ? { jsonl: ctx.str("jsonl") ?? null, qagentState: ctx.str("qagent-state") ?? null, prototype: ctx.str("prototype") ?? null }
        : defaultImportSources(homeFor(ctx.dbPath));
      const dryRun = ctx.flag("dry-run") === true;
      // A dry run never opens bus.db for writing (and never creates it).
      if (!dryRun) {
        const me = ctx.identity(true);
        if (me.authority !== "operator") throw new BusError("forbidden", "only the operator may import");
        ctx.close();
      }
      const report = runImport(ctx.dbPath, sources, { dryRun, force: ctx.flag("force") === true, actor: OPERATOR_ID });
      ctx.out(report, renderImport(report));
      return 0;
    }
    default:
      throw new BusError("invalid", `unknown command: ${command}\n\n${USAGE}`);
  }
}

async function taskCommand(ctx: Context, sub: string | undefined): Promise<number> {
  const bus = ctx.bus;
  switch (sub) {
    case "add": {
      const me = ctx.identity();
      const scopes = ctx.list("scope");
      const project = ctx.str("project") ?? (scopes.length ? process.cwd() : undefined);
      const task = bus.createTask(me, {
        title: ctx.position(2, "task title"), brief: ctx.maybeStdin(ctx.str("brief")) ?? "", acceptance: ctx.maybeStdin(ctx.str("acceptance")),
        to: ctx.str("to") ?? null, reviewer: ctx.str("reviewer") ?? null, role: ctx.str("role"), priority: ctx.str("priority") as Priority | undefined,
        parentId: ctx.int("parent") ?? null, dependencies: ctx.list("dep").map(Number), pathScopes: scopes, project: project ?? null,
      });
      ctx.out(task, `created task #${task.id} (${task.state})`);
      return 0;
    }
    case "list": {
      const mine = ctx.flag("mine") === true ? ctx.identity().agentId : null;
      const tasks = bus.listTasks({ mine, states: ctx.list("state") as TaskState[], includeClosed: ctx.flag("all") === true, limit: ctx.int("limit") });
      ctx.out(tasks, renderTasks(tasks));
      return 0;
    }
    case "show": {
      const task = bus.getTask(ctx.taskId(2));
      ctx.out(task, renderTask(task));
      return 0;
    }
    case "claim": {
      const me = ctx.identity();
      const id = ctx.parsed.positionals[2] === undefined ? null : ctx.taskId(2);
      const task = bus.claimTask(me, id);
      ctx.out(task, `claimed task #${task.id}: ${task.title}`);
      return 0;
    }
    case "note": {
      const note = bus.noteTask(ctx.identity(), ctx.taskId(2), ctx.maybeStdin(ctx.position(3, "note text")) ?? "");
      ctx.out(note, `noted on task #${note.taskId}`);
      return 0;
    }
    case "submit": {
      const summary = ctx.maybeStdin(ctx.str("summary"));
      if (!summary) throw new BusError("invalid", "--summary is required");
      const task = bus.submitTask(ctx.identity(), ctx.taskId(2), { summary, details: ctx.maybeStdin(ctx.str("details")), changedFiles: ctx.list("file") });
      ctx.out(task, `submitted task #${task.id} round ${task.round}`);
      return 0;
    }
    case "review": {
      const accept = ctx.flag("accept") === true;
      const revise = ctx.flag("revise") === true;
      if (accept === revise) throw new BusError("invalid", "task review needs exactly one of --accept or --revise");
      const feedback = ctx.maybeStdin(ctx.str("feedback"));
      if (!feedback) throw new BusError("invalid", "--feedback is required");
      const task = bus.reviewTask(ctx.identity(), ctx.taskId(2), { accepted: accept, feedback });
      ctx.out(task, `task #${task.id} is ${task.state}${task.state === "changes_requested" ? ` (round ${task.round})` : ""}`);
      return 0;
    }
    case "cancel": {
      const task = bus.cancelTask(ctx.identity(), ctx.taskId(2), ctx.str("reason"));
      ctx.out(task, `cancelled task #${task.id}`);
      return 0;
    }
    case "stalled": {
      const minutes = Number(ctx.str("stall-min") ?? "60");
      if (!Number.isFinite(minutes) || minutes <= 0) throw new BusError("invalid", "--stall-min must be a positive number of minutes");
      const tasks = bus.stalledTasks(minutes * 60_000);
      ctx.out(tasks, renderTasks(tasks));
      return 0;
    }
    case "requeue": {
      const task = bus.requeueTask(ctx.identity(), ctx.taskId(2), ctx.str("reason"));
      ctx.out(task, `requeued task #${task.id}`);
      return 0;
    }
    default:
      throw new BusError("invalid", "usage: qagent task add|list|show|claim|note|submit|review|cancel|stalled|requeue");
  }
}

export async function main(argv: string[], io: CliIo = defaultIo): Promise<number> {
  let ctx: Context | null = null;
  let json = false;
  try {
    const commandIndex = commandPosition(argv);
    const command = commandIndex >= 0 ? argv[commandIndex] : undefined;
    if (command && LAZY[command]) {
      // Global flags before the command (for example `--db X`) are parsed here; the lazy
      // module gets everything after the command name, plus the command name itself.
      const before = parseArgs(argv.slice(0, commandIndex));
      const rest = argv.slice(commandIndex + 1);
      const restDb = rest.findIndex((arg) => arg === "--db" || arg.startsWith("--db="));
      const dbFlag = restDb >= 0
        ? (rest[restDb].startsWith("--db=") ? rest[restDb].slice("--db=".length) : rest[restDb + 1])
        : before.flags.get("db") as string | undefined;
      const lazyCtx = new Context(before, io, resolveDbPath(dbFlag ?? null, io.env));
      return await runLazy(command, lazyCtx, rest);
    }
    const parsed = parseArgs(argv);
    json = parsed.flags.get("json") === true;
    if (!parsed.positionals.length || parsed.flags.get("help") === true || parsed.positionals[0] === "help") {
      io.stdout(USAGE);
      return 0;
    }
    ctx = new Context(parsed, io, resolveDbPath((parsed.flags.get("db") as string | undefined) ?? null, io.env));
    return await dispatch(ctx);
  } catch (error) {
    const code = error instanceof BusError ? error.code : "error";
    const message = error instanceof Error ? error.message : String(error);
    io.stderr(`qagent: ${message}\n`);
    if (json) io.stdout(`${JSON.stringify({ error: message, code })}\n`);
    return code === "unauthorized" || code === "forbidden" ? 3 : 1;
  } finally {
    ctx?.close();
  }
}
