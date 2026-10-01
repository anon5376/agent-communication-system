/**
 * Lazy entry for `qagent supervise <agent> [dir] [--config PATH]` and `qagent doctor [agent] [dir]`,
 * loaded by src/cli/main.ts. The CLI passes the arguments after the command name and the
 * command name itself in context.command.
 */
import { accessSync, constants, existsSync } from "node:fs";
import { delimiter, isAbsolute, join, resolve } from "node:path";
import { configPathFromProject, loadConfig, resolveAgent } from "../config.js";
import { Bus } from "../core/bus.js";
import { operatorTokenPath, readTokenFile } from "../core/identity.js";
import { BusError } from "../core/types.js";
import { supervise } from "../supervisor.js";

export interface EntryContext {
  dbPath: string;
  command?: string;
  stdout?: (text: string) => void;
  stderr?: (text: string) => void;
}

const USAGE = `usage: qagent supervise <agent> [project-dir] [--config PATH] [--auto-requeue-min M]
       qagent doctor [agent] [project-dir] [--config PATH]
`;

function split(argv: string[]): { positionals: string[]; config?: string; autoRequeueMin?: number; help: boolean } {
  const positionals: string[] = [];
  let config: string | undefined;
  let autoRequeueMin: number | undefined;
  let help = false;
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === "-h" || arg === "--help") help = true;
    else if (arg === "--config") config = argv[index += 1];
    else if (arg.startsWith("--config=")) config = arg.slice("--config=".length);
    else if (arg === "--auto-requeue-min") autoRequeueMin = Number(argv[index += 1]);
    else if (arg.startsWith("--auto-requeue-min=")) autoRequeueMin = Number(arg.slice("--auto-requeue-min=".length));
    else if (arg === "--db") index += 1; // already resolved by the CLI into context.dbPath
    else if (arg.startsWith("--db=")) continue;
    else if (arg.startsWith("--")) throw new BusError("invalid", `unknown flag ${arg}\n${USAGE}`);
    else positionals.push(arg);
  }
  if (autoRequeueMin !== undefined && (!Number.isFinite(autoRequeueMin) || autoRequeueMin <= 0)) {
    throw new BusError("invalid", "--auto-requeue-min must be a positive number of minutes");
  }
  return { positionals, config, autoRequeueMin, help };
}

function commandName(context: EntryContext): string {
  return context.command === "doctor" ? "doctor" : "supervise";
}

function onPath(command: string): boolean {
  const candidates = isAbsolute(command) || command.includes("/")
    ? [command]
    : (process.env.PATH ?? "").split(delimiter).filter(Boolean).map((dir) => join(dir, command));
  return candidates.some((path) => {
    try { accessSync(path, constants.X_OK); return true; } catch { return false; }
  });
}

/** Read-only checks. Never runs a harness binary; it only looks for it on PATH. */
function doctor(positionals: string[], configPath: string | undefined, context: EntryContext, out: (text: string) => void): number {
  const problems: string[] = [];
  out(`bus ${context.dbPath}${existsSync(context.dbPath) ? "" : " (missing: run `qagent init`)"}\n`);
  if (!existsSync(context.dbPath)) return 1;
  const bus = Bus.open({ dbPath: context.dbPath });
  try {
    const operatorToken = readTokenFile(operatorTokenPath(bus.home));
    if (!operatorToken) problems.push(`no operator token at ${operatorTokenPath(bus.home)}`);
    out(`agents ${bus.listAgents().map((agent) => agent.id).join(", ") || "(none)"}\n`);
    const [agentId, dir] = positionals;
    if (agentId) {
      try { bus.identify(agentId); out(`identity ${agentId} ok\n`); } catch (error) { problems.push(`identity ${agentId}: ${(error as Error).message}`); }
      try {
        const path = configPath ?? configPathFromProject(resolve(dir ?? process.cwd()));
        const agent = resolveAgent(loadConfig(path), agentId);
        out(`config ${path}: ${agent.harnessDefinition.id} (${agent.harnessDefinition.command}), autoStart ${agent.autoStart}\n`);
        if (!agent.enabled) problems.push(`${agentId} is disabled in ${path}`);
        if (agent.harnessDefinition.adapter !== "fake" && !onPath(agent.harnessDefinition.command)) problems.push(`${agent.harnessDefinition.command} is not on PATH`);
      } catch (error) {
        problems.push(`config: ${(error as Error).message}`);
      }
    }
  } finally {
    bus.close();
  }
  for (const problem of problems) out(`problem: ${problem}\n`);
  out(problems.length ? `${problems.length} problem(s)\n` : "ok\n");
  return problems.length ? 1 : 0;
}

export async function main(argv: string[], context: EntryContext): Promise<number> {
  const out = context.stdout ?? ((text: string) => { process.stdout.write(text); });
  const err = context.stderr ?? ((text: string) => { process.stderr.write(text); });
  try {
    const { positionals, config, autoRequeueMin, help } = split(argv);
    if (help) { out(USAGE); return 0; }
    if (commandName(context) === "doctor") return doctor(positionals, config, context, out);
    const [agentId, dir] = positionals;
    if (!agentId) { err(USAGE); return 1; }
    const controller = new AbortController();
    const stop = () => controller.abort();
    process.once("SIGINT", stop);
    process.once("SIGTERM", stop);
    try {
      await supervise({ agentId, workdir: resolve(dir ?? process.cwd()), dbPath: context.dbPath, configPath: config, autoRequeueMs: autoRequeueMin !== undefined ? autoRequeueMin * 60_000 : undefined, signal: controller.signal });
    } finally {
      process.off("SIGINT", stop);
      process.off("SIGTERM", stop);
    }
    return 0;
  } catch (error) {
    err(`qagent: ${(error as Error).message}\n`);
    return error instanceof BusError && (error.code === "unauthorized" || error.code === "forbidden") ? 3 : 1;
  }
}
