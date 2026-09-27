/**
 * `qagent mcp-config [--agent ID] [--client claude|codex] [--operator]`
 *
 * Prints the registration snippet that starts `qagent mcp` for one agent:
 *   Claude Code  ~/.claude.json            "mcpServers": { "qagent": {...} }
 *   Codex        ~/.codex/config.toml      [mcp_servers.qagent]
 * The command is this Node binary plus the absolute path of dist/qagent.js, so it
 * works without qagent on the client's PATH. Tokens stay in files; the snippet
 * carries only QAGENT_AGENT_ID (and QAGENT_BUS_DB when the bus is not the default).
 */
import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { homeFor, resolveDbPath } from "../core/db.js";
import { agentIdFromEnv, assertSafeAgentId, tokenPathFor } from "../core/identity.js";
import { BusError, MAX_WAIT_SEC, OPERATOR_ID } from "../core/types.js";

export type Client = "claude" | "codex";

export interface McpLaunch {
  name: string;
  command: string;
  args: string[];
  env: Record<string, string>;
}

const USAGE = `usage: qagent mcp-config [--agent ID] [--client claude|codex] [--operator] [--name NAME]

Prints the MCP registration for Claude Code (~/.claude.json "mcpServers") and
Codex (~/.codex/config.toml [mcp_servers.NAME]). --agent defaults to QAGENT_AGENT_ID.
`;

export function launchFor(agentId: string, dbPath: string, options: { operator?: boolean; name?: string; env?: NodeJS.ProcessEnv } = {}): McpLaunch {
  if (agentId !== OPERATOR_ID) assertSafeAgentId(agentId);
  const entry = fileURLToPath(new URL("../qagent.js", import.meta.url));
  const env: Record<string, string> = { QAGENT_AGENT_ID: agentId };
  // Only pin the database when it differs from what the client would resolve with no bus variables set.
  if (dbPath !== resolveDbPath(null, {})) env.QAGENT_BUS_DB = dbPath;
  return {
    name: options.name ?? "qagent",
    command: process.execPath,
    args: [entry, "mcp", ...(options.operator ? ["--operator"] : [])],
    env,
  };
}

export function claudeSnippet(launch: McpLaunch): string {
  return JSON.stringify({ mcpServers: { [launch.name]: { type: "stdio", command: launch.command, args: launch.args, env: launch.env } } }, null, 2);
}

/** TOML basic strings accept JSON string escapes for the characters paths and ids contain. */
function tomlString(value: string): string {
  return JSON.stringify(value);
}

function tomlKey(value: string): string {
  return /^[A-Za-z0-9_-]+$/.test(value) ? value : tomlString(value);
}

export function codexSnippet(launch: McpLaunch): string {
  const env = Object.entries(launch.env).map(([key, value]) => `${tomlKey(key)} = ${tomlString(value)}`).join(", ");
  return [
    `[mcp_servers.${tomlKey(launch.name)}]`,
    `command = ${tomlString(launch.command)}`,
    `args = [${launch.args.map(tomlString).join(", ")}]`,
    `env = { ${env} }`,
    // bus_wait blocks for up to an hour; Codex's default tool timeout is much shorter.
    `tool_timeout_sec = ${MAX_WAIT_SEC + 60}`,
  ].join("\n");
}

function parseArgs(argv: string[]): { agent?: string; client?: Client; operator: boolean; name?: string; help: boolean } {
  const out: { agent?: string; client?: Client; operator: boolean; name?: string; help: boolean } = { operator: false, help: false };
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    const [flag, inline] = arg.includes("=") ? [arg.slice(0, arg.indexOf("=")), arg.slice(arg.indexOf("=") + 1)] : [arg, undefined];
    const value = (): string => {
      if (inline !== undefined) return inline;
      if (index + 1 >= argv.length) throw new BusError("invalid", `${flag} needs a value`);
      return argv[index += 1];
    };
    if (flag === "--agent" || flag === "--as") out.agent = value();
    else if (flag === "--client") {
      const client = value();
      if (client !== "claude" && client !== "codex") throw new BusError("invalid", "--client must be claude or codex");
      out.client = client;
    } else if (flag === "--name") out.name = value();
    else if (flag === "--operator") out.operator = true;
    else if (flag === "--db") value(); // consumed by the CLI dispatcher
    else if (flag === "--help" || flag === "-h") out.help = true;
    else throw new BusError("invalid", `unknown argument: ${arg}\n\n${USAGE}`);
  }
  return out;
}

/** Entry point loaded lazily by src/cli/main.ts. */
export async function main(argv: string[], context: { dbPath: string }): Promise<number> {
  try {
    const args = parseArgs(argv);
    if (args.help) {
      process.stdout.write(USAGE);
      return 0;
    }
    const agentId = args.agent ?? (args.operator ? OPERATOR_ID : agentIdFromEnv(process.env));
    if (!agentId) throw new BusError("invalid", "pass --agent <id> (or set QAGENT_AGENT_ID)");
    if (args.operator && agentId !== OPERATOR_ID) throw new BusError("invalid", "--operator registers the operator identity; drop --agent");
    if (args.name !== undefined && !/^[A-Za-z0-9_-]+$/.test(args.name)) throw new BusError("invalid", "--name may use letters, digits, '_' and '-'");
    const launch = launchFor(agentId, context.dbPath, { operator: args.operator, name: args.name });
    const tokenPath = tokenPathFor(homeFor(context.dbPath), agentId);
    if (!existsSync(tokenPath)) {
      process.stderr.write(`qagent mcp-config: warning: no token file at ${tokenPath}; run \`qagent agent add ${agentId} --role ...\` first.\n`);
    }
    if (args.client === "claude") process.stdout.write(`${claudeSnippet(launch)}\n`);
    else if (args.client === "codex") process.stdout.write(`${codexSnippet(launch)}\n`);
    else {
      process.stdout.write([
        `# Claude Code: merge into ~/.claude.json (user scope) or .mcp.json (project scope)`,
        claudeSnippet(launch),
        "",
        `# Codex: add to ~/.codex/config.toml`,
        codexSnippet(launch),
        "",
      ].join("\n"));
    }
    return 0;
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    process.stderr.write(`qagent mcp-config: ${message}\n`);
    return 1;
  }
}
