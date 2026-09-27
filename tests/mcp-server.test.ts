import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, readdirSync, readFileSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import { Bus } from "../src/core/bus.js";

const QAGENT = fileURLToPath(new URL("../src/qagent.js", import.meta.url));

const AGENT_TOOLS = [
  "bus_whoami", "bus_agents", "bus_send", "bus_inbox", "bus_wait", "bus_ack",
  "bus_task_create", "bus_task_list", "bus_task_get", "bus_task_claim", "bus_task_note",
  "bus_task_submit", "bus_task_review", "bus_task_cancel",
];

type Hooks = { after: (fn: () => void | Promise<void>) => void };

/** A preload that makes any listen, outbound connect or fetch crash the process. */
function noNetworkPreload(dir: string): string {
  const path = join(dir, "no-network.mjs");
  writeFileSync(path, [
    "import net from 'node:net';",
    "const deny = (what) => function () { process.stderr.write('NETWORK_' + what + '\\n'); process.exit(97); };",
    "net.Server.prototype.listen = deny('LISTEN');",
    "net.Socket.prototype.connect = deny('CONNECT');",
    "globalThis.fetch = deny('FETCH');",
  ].join("\n"));
  return path;
}

function setup(t: Hooks) {
  const home = mkdtempSync(join(tmpdir(), "qagent-v2-mcp-"));
  t.after(() => rmSync(home, { recursive: true, force: true }));
  const dbPath = join(home, "bus.db");
  const bus = Bus.open({ dbPath });
  t.after(() => bus.close());
  bus.init();
  const operator = bus.identify("operator");
  bus.addAgent(operator, { id: "alice", role: "manager", authority: "manager" });
  bus.addAgent(operator, { id: "bob", role: "worker", model: "fake" });
  return { home, dbPath, bus, preload: noNetworkPreload(home) };
}

/** Only what the server needs; QAGENT_URL points at a closed port to prove no broker is contacted. */
function serverEnv(dbPath: string, extra: Record<string, string> = {}): Record<string, string> {
  return { PATH: process.env.PATH ?? "", HOME: process.env.HOME ?? "", QAGENT_BUS_DB: dbPath, QAGENT_URL: "http://127.0.0.1:9", ...extra };
}

async function connect(t: Hooks, preload: string, env: Record<string, string>, args: string[] = []) {
  const transport = new StdioClientTransport({ command: process.execPath, args: ["--import", preload, QAGENT, "mcp", ...args], env, stderr: "pipe" });
  let stderr = "";
  transport.stderr?.on("data", (chunk: Buffer) => { stderr += chunk.toString("utf8"); });
  const client = new Client({ name: "lane2-test", version: "1.0.0" });
  await client.connect(transport);
  t.after(() => client.close());
  return { client, transport, stderr: () => stderr };
}

function text(result: unknown): string {
  const reply = result as { content?: { type: string; text?: string }[]; isError?: boolean };
  return reply.content?.map((part) => part.text ?? "").join("\n") ?? "";
}

async function call(client: Client, name: string, args: Record<string, unknown> = {}): Promise<string> {
  const result = await client.callTool({ name, arguments: args });
  assert.notEqual((result as { isError?: boolean }).isError, true, `${name} failed: ${text(result)}`);
  return text(result);
}

test("the MCP SDK client lists exactly the 14 agent tools, 15 with --operator, and bus_send has no sender field", async (t) => {
  const { dbPath, preload, home } = setup(t);
  const bob = await connect(t, preload, serverEnv(dbPath, { QAGENT_AGENT_ID: "bob" }));
  const { tools } = await bob.client.listTools();
  assert.deepEqual(tools.map((tool) => tool.name).sort(), [...AGENT_TOOLS].sort());

  const send = tools.find((tool) => tool.name === "bus_send")!;
  const fields = Object.keys(send.inputSchema.properties ?? {}).sort();
  assert.deepEqual(fields, ["body", "refs", "requires_ack", "subject", "task_id", "thread", "to", "type"]);
  for (const forbidden of ["sender", "from", "as", "agent_id", "token"]) assert.ok(!fields.includes(forbidden), `bus_send exposes ${forbidden}`);
  for (const tool of tools) {
    const keys = Object.keys(tool.inputSchema.properties ?? {});
    assert.ok(!keys.includes("sender") && !keys.includes("from"), `${tool.name} accepts a sender`);
  }

  const operator = await connect(t, preload, serverEnv(dbPath), ["--operator"]);
  const operatorTools = (await operator.client.listTools()).tools.map((tool) => tool.name).sort();
  assert.deepEqual(operatorTools, [...AGENT_TOOLS, "bus_agent_add"].sort());
  const added = await call(operator.client, "bus_agent_add", { id: "carol", role: "reviewer" });
  assert.match(added, /Added carol/);
  assert.ok(readdirSync(join(home, "tokens")).includes("carol.token"));
  assert.doesNotMatch(added, new RegExp(readFileSync(join(home, "tokens", "carol.token"), "utf8").trim()));

  // An agent token cannot start the operator server.
  const refused = spawnSync(process.execPath, ["--import", preload, QAGENT, "mcp", "--operator"], { env: serverEnv(dbPath, { QAGENT_AGENT_ID: "bob" }), encoding: "utf8", input: "" });
  assert.equal(refused.status, 3, refused.stderr);
  assert.match(refused.stderr, /operator/);
});

test("the server works with no broker present and no listener: full task cycle, mail and ack over MCP", async (t) => {
  const { dbPath, preload, bus } = setup(t);
  const alice = await connect(t, preload, serverEnv(dbPath, { QAGENT_AGENT_ID: "alice" }));
  const bob = await connect(t, preload, serverEnv(dbPath, { QAGENT_AGENT_ID: "bob" }));

  assert.match(await call(bob.client, "bus_whoami"), /You are bob \(worker, role worker, model fake\)/);
  assert.match(await call(alice.client, "bus_agents"), /alice\s+manager/);

  assert.match(await call(alice.client, "bus_task_create", { title: "Parse the config", brief: "Write the parser.", to: "bob", acceptance: "tests pass" }), /Created task #1 \[open/);
  assert.match(await call(bob.client, "bus_inbox"), /\[TASK #1\] Parse the config/);
  assert.match(await call(bob.client, "bus_task_claim"), /Claimed task #1 \[claimed/);
  assert.match(await call(bob.client, "bus_task_note", { task_id: 1, note: "halfway" }), /Noted on task #1/);
  assert.match(await call(bob.client, "bus_task_submit", { task_id: 1, summary: "done", changed_files: ["src/parse.ts"], validation: [{ passed: true, summary: "unit tests", command: "npm test" }] }), /Submitted task #1 \[submitted/);
  assert.match(await call(alice.client, "bus_task_review", { task_id: 1, accepted: false, feedback: "handle empty input" }), /Requested changes on task #1 \[changes_requested, round 2/);
  assert.match(await call(bob.client, "bus_task_submit", { task_id: 1, summary: "empty input handled" }), /Submitted task #1/);
  assert.match(await call(alice.client, "bus_task_review", { task_id: 1, accepted: true, feedback: "good" }), /Accepted task #1 \[accepted/);
  const detail = await call(bob.client, "bus_task_get", { task_id: 1 });
  assert.match(detail, /state\s+accepted/);
  assert.match(detail, /halfway/);
  assert.match(await call(bob.client, "bus_task_list", { include_closed: true }), /accepted/);

  assert.match(await call(alice.client, "bus_send", { to: "bob", subject: "ping", body: "please ack", requires_ack: true }), /Sent to bob/);
  const inbox = await call(bob.client, "bus_inbox");
  const seq = Number(/#(\d+) \S+ alice -> bob \[info\]/.exec(inbox)?.[1]);
  assert.ok(seq > 0, inbox);
  assert.match(await call(bob.client, "bus_ack", { seq }), new RegExp(`Acknowledged #${seq}`));
  assert.equal(bus.getMessages({ sinceSeq: seq - 1, limit: 1 })[0].sender, "alice", "the sender is the server's identity");

  // Errors come back as tool errors, not crashes.
  const bad = await bob.client.callTool({ name: "bus_task_review", arguments: { task_id: 1, accepted: true, feedback: "x" } });
  assert.equal((bad as { isError?: boolean }).isError, true);

  for (const server of [alice, bob]) {
    assert.doesNotMatch(server.stderr(), /NETWORK_/);
    const pid = server.transport.pid;
    assert.ok(pid, "server pid");
    const lsof = spawnSync("lsof", ["-a", "-p", String(pid), "-i"], { encoding: "utf8" });
    if (lsof.error) t.diagnostic("lsof unavailable; relying on the no-network preload");
    else assert.equal(lsof.stdout.trim(), "", `server ${pid} holds internet sockets:\n${lsof.stdout}`);
  }
});

test("identity comes from QAGENT_AGENT_ID plus QAGENT_TOKEN or the token file; another agent's token is refused", async (t) => {
  const { dbPath, preload, home } = setup(t);
  const aliceToken = readFileSync(join(home, "tokens", "alice.token"), "utf8").trim();
  const bobToken = readFileSync(join(home, "tokens", "bob.token"), "utf8").trim();

  const wrong = spawnSync(process.execPath, ["--import", preload, QAGENT, "mcp"], { env: serverEnv(dbPath, { QAGENT_AGENT_ID: "bob", QAGENT_TOKEN: aliceToken }), encoding: "utf8", input: "" });
  assert.equal(wrong.status, 3, wrong.stderr);
  assert.match(wrong.stderr, /does not belong to bob/);

  const nobody = spawnSync(process.execPath, ["--import", preload, QAGENT, "mcp"], { env: serverEnv(dbPath), encoding: "utf8", input: "" });
  assert.equal(nobody.status, 1);
  assert.match(nobody.stderr, /QAGENT_AGENT_ID/);

  renameSync(join(home, "tokens", "bob.token"), join(home, "bob.token.moved"));
  const missing = spawnSync(process.execPath, ["--import", preload, QAGENT, "mcp"], { env: serverEnv(dbPath, { QAGENT_AGENT_ID: "bob" }), encoding: "utf8", input: "" });
  assert.equal(missing.status, 3);
  assert.match(missing.stderr, /no token file/);

  const viaEnv = await connect(t, preload, serverEnv(dbPath, { QAGENT_AGENT_ID: "bob", QAGENT_TOKEN: bobToken }));
  assert.match(await call(viaEnv.client, "bus_whoami"), /You are bob/);
});

test("src/mcp and src/notify contain no HTTP or socket code", () => {
  const offenders: string[] = [];
  for (const dir of ["src/mcp", "src/notify"]) {
    const root = join(process.cwd(), dir);
    for (const name of readdirSync(root).filter((file) => file.endsWith(".ts"))) {
      const source = readFileSync(join(root, name), "utf8");
      for (const pattern of [/\bfetch\s*\(/, /["']node:(https?|http2|net|dgram|tls)["']/, /\.listen\s*\(/, /createServer\s*\(/, /https?:\/\//, /WebSocket/]) {
        if (pattern.test(source)) offenders.push(`${dir}/${name}: ${pattern}`);
      }
    }
  }
  assert.deepEqual(offenders, []);
});

test("qagent mcp-config prints working Claude and Codex registrations with QAGENT_AGENT_ID", async (t) => {
  const { dbPath, preload } = setup(t);
  const run = (...args: string[]) => spawnSync(process.execPath, [QAGENT, "mcp-config", ...args], { env: serverEnv(dbPath), encoding: "utf8" });

  const claude = run("--client", "claude", "--agent", "bob");
  assert.equal(claude.status, 0, claude.stderr);
  const entry = JSON.parse(claude.stdout).mcpServers.qagent;
  assert.equal(entry.type, "stdio");
  assert.equal(entry.command, process.execPath);
  assert.deepEqual(entry.args, [QAGENT, "mcp"]);
  assert.deepEqual(entry.env, { QAGENT_AGENT_ID: "bob", QAGENT_BUS_DB: dbPath });

  const codex = run("--client", "codex", "--agent", "alice");
  assert.equal(codex.status, 0, codex.stderr);
  assert.match(codex.stdout, /^\[mcp_servers\.qagent\]$/m);
  assert.match(codex.stdout, new RegExp(`^command = ${JSON.stringify(process.execPath).replace(/[\\/.]/g, "\\$&")}$`, "m"));
  assert.match(codex.stdout, /^args = \[".*qagent\.js", "mcp"\]$/m);
  assert.match(codex.stdout, /^env = \{ QAGENT_AGENT_ID = "alice", QAGENT_BUS_DB = ".*bus\.db" \}$/m);
  assert.match(codex.stdout, /^tool_timeout_sec = \d+$/m);

  const both = run("--agent", "bob");
  assert.equal(both.status, 0);
  assert.match(both.stdout, /# Claude Code/);
  assert.match(both.stdout, /# Codex/);
  assert.match(run("--agent", "ghost").stderr, /no token file/);

  // The printed Claude entry starts a server that answers.
  const transport = new StdioClientTransport({ command: entry.command, args: ["--import", preload, ...entry.args], env: { PATH: process.env.PATH ?? "", ...entry.env } });
  const client = new Client({ name: "lane2-config", version: "1.0.0" });
  await client.connect(transport);
  t.after(() => client.close());
  assert.equal((await client.listTools()).tools.length, 14);
  assert.match(await call(client, "bus_whoami"), /You are bob/);
});
