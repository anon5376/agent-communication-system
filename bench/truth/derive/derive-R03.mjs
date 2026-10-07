// derive-R03.mjs — CLI commands and MCP tools present in the TypeScript build but missing in the Rust port.
// Usage: node derive-R03.mjs <ts-repo-root> <rust-target-debug-dir>
//   <ts-repo-root>: checkout of 4d4cf5a9f4ca0e819dfc9b97d6610cc05fbf1893 (reads src/cli/main.ts, src/mcp/server.ts)
//   <rust-target-debug-dir>: rust/target/debug of a `cargo build` of origin/rust-port (c6df26b3ec0235e38404b33b382c1fa824ba12a5)
// Unit: `command` or `command subcommand` (subcommand only for the groups agent, token, task); flags are not units.
// TS side: parsed from the USAGE text and registerTool(...) calls. Rust side: `qagent --help` output,
// a runtime probe of every group subcommand, and the tools/list answer of `qagent mcp --operator`.
import { readFileSync, mkdtempSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { join, resolve } from "node:path";
import { tmpdir } from "node:os";

const tsRoot = resolve(process.argv[2] ?? ".");
const bin = resolve(process.argv[3] ?? "rust/target/debug");
const GROUPS = new Set(["agent", "token", "task"]);

function unitsFromUsage(usage) {
  const units = new Set();
  for (const line of usage.split("\n")) {
    if (!/^\s+qagent /.test(line)) continue;
    for (let seg of line.trim().split(/\s+\|\s+/)) {
      const words = seg.replace(/^qagent\s+/, "").split(/\s+/);
      const [cmd, sub] = words;
      units.add(GROUPS.has(cmd) && sub && /^[a-z-]+$/.test(sub) ? `${cmd} ${sub}` : cmd);
    }
  }
  return units;
}

const mainTs = readFileSync(join(tsRoot, "src/cli/main.ts"), "utf8");
const tsUsage = /export const USAGE = `([\s\S]*?)`;/.exec(mainTs)[1];
const tsCli = unitsFromUsage(tsUsage);
const tsMcp = [...readFileSync(join(tsRoot, "src/mcp/server.ts"), "utf8").matchAll(/registerTool\("([a-z_]+)"/g)].map((m) => m[1]);

const home = mkdtempSync(join(tmpdir(), "r03-"));
const env = { ...process.env, HOME: home, QAGENT_BUS_DB: join(home, "bus.db") };
const run = (args, input = "") => spawnSync(join(bin, "qagent"), args, { env, input, encoding: "utf8", timeout: 10000 });

const help = run(["--help"]);
console.log(`rust qagent --help exit=${help.status}`);
const rsCli = unitsFromUsage(help.stdout + help.stderr);
run(["init"]);
// Runtime probe of group subcommands: the dispatcher's fallthrough answers "usage: qagent <group> ..." or "unknown".
const probe = {};
for (const u of tsCli) {
  const [cmd, sub] = u.split(" ");
  if (!sub) continue;
  const r = run(["--as", "operator", cmd, sub, ...(sub === "prune" ? [] : ["1"])]);
  const out = (r.stdout + r.stderr).trim().split("\n")[0];
  probe[u] = { dispatched: !new RegExp(`usage: qagent ${cmd} [a-z|-]+$|unknown (command|subcommand)`).test(out), out };
}
const init = '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"probe","version":"1"}}}';
const mcp = run(["--as", "operator", "mcp", "--operator"], `${init}\n{"jsonrpc":"2.0","method":"notifications/initialized"}\n{"jsonrpc":"2.0","id":2,"method":"tools/list"}\n`);
let rsMcp = [];
for (const l of mcp.stdout.split("\n")) { try { const j = JSON.parse(l); if (j.id === 2) rsMcp = j.result.tools.map((t) => t.name); } catch {} }

console.log(`TS cli units (${tsCli.size}): ${[...tsCli].join(", ")}`);
console.log(`Rust --help units (${rsCli.size}): ${[...rsCli].join(", ")}`);
for (const [u, p] of Object.entries(probe)) console.log(`probe rust '${u}': ${p.dispatched ? "dispatched" : "NOT dispatched"}  | ${p.out.slice(0, 90)}`);
console.log(`TS mcp tools (${tsMcp.length}): ${tsMcp.join(" ")}`);
console.log(`Rust mcp tools/list (${rsMcp.length}): ${rsMcp.join(" ")}`);
const missing = [
  ...[...tsCli].filter((u) => !rsCli.has(u) && (probe[u] ? !probe[u].dispatched : true)).map((u) => `cli:${u}`),
  ...tsMcp.filter((t) => !rsMcp.includes(t)).map((t) => `mcp:${t}`),
];
const disagree = [...tsCli].filter((u) => probe[u] && (rsCli.has(u) !== probe[u].dispatched));
if (disagree.length) console.log(`WARNING help/probe disagree: ${disagree.join(", ")}`);
console.log(`\nMISSING in Rust (${missing.length}):\n${missing.join("\n")}`);
