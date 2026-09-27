import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { setTimeout as sleep } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import { Bus } from "../src/core/bus.js";
import type { Message } from "../src/core/types.js";
import { readSignalFile, waitForMail } from "../src/notify/wait.js";

const QAGENT = fileURLToPath(new URL("../src/qagent.js", import.meta.url));

type Hooks = { after: (fn: () => void | Promise<void>) => void };

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
  const home = mkdtempSync(join(tmpdir(), "qagent-v2-wait-"));
  t.after(() => rmSync(home, { recursive: true, force: true }));
  const dbPath = join(home, "bus.db");
  const bus = Bus.open({ dbPath });
  t.after(() => bus.close());
  bus.init();
  const operator = bus.identify("operator");
  bus.addAgent(operator, { id: "alice", role: "manager", authority: "manager" });
  bus.addAgent(operator, { id: "bob", role: "worker" });
  return { home, dbPath, bus, preload: noNetworkPreload(home) };
}

function env(dbPath: string, agent?: string): Record<string, string> {
  return { PATH: process.env.PATH ?? "", HOME: process.env.HOME ?? "", QAGENT_BUS_DB: dbPath, ...(agent ? { QAGENT_AGENT_ID: agent } : {}) };
}

async function startServer(t: Hooks, dbPath: string, preload: string, agent: string) {
  const transport = new StdioClientTransport({ command: process.execPath, args: ["--import", preload, QAGENT, "mcp"], env: env(dbPath, agent), stderr: "pipe" });
  const client = new Client({ name: "lane2-wait", version: "1.0.0" });
  await client.connect(transport);
  t.after(() => client.close().catch(() => undefined));
  return { client, transport };
}

/** `qagent send` as a separate process, without blocking this event loop. */
function cliSend(dbPath: string, from: string, to: string, subject: string): Promise<Message[]> {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [QAGENT, "send", to, subject, "body", "--json"], { env: env(dbPath, from), stdio: ["ignore", "pipe", "pipe"] });
    let out = "";
    let err = "";
    child.stdout.setEncoding("utf8").on("data", (chunk: string) => { out += chunk; });
    child.stderr.setEncoding("utf8").on("data", (chunk: string) => { err += chunk; });
    child.on("close", (code) => code === 0 ? resolve(JSON.parse(out) as Message[]) : reject(new Error(`qagent send exited ${code}: ${err}`)));
  });
}

async function until(check: () => boolean, timeoutMs = 10_000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (!check()) {
    if (Date.now() > deadline) throw new Error("condition not met in time");
    await sleep(10);
  }
}

function text(result: unknown): string {
  return ((result as { content?: { text?: string }[] }).content ?? []).map((part) => part.text ?? "").join("\n");
}

const LONG = { timeout: 120_000 };

test("bus_wait returns within 500 ms of a send from the CLI", async (t) => {
  const { dbPath, preload, bus } = setup(t);
  const bob = await startServer(t, dbPath, preload, "bob");
  const latencies: number[] = [];
  for (let round = 1; round <= 3; round += 1) {
    const waiting = bob.client.callTool({ name: "bus_wait", arguments: { timeout_sec: 60 } }, undefined, LONG).then((result) => ({ result, at: Date.now() }));
    await until(() => bus.getAgent("bob")?.storedStatus === "waiting");
    await sleep(50 + round * 40);
    const [sent] = await cliSend(dbPath, "alice", "bob", `ping ${round}`);
    const { result, at } = await waiting;
    assert.notEqual((result as { isError?: boolean }).isError, true, text(result));
    assert.match(text(result), new RegExp(`alice -> bob \\[info\\]\\n  ping ${round}`));
    latencies.push(at - sent.tsMs);
    assert.equal(bus.unreadCount("bob"), 0, "the delivered mail is marked read");
  }
  t.diagnostic(`bus_wait latency ms (reply received - message written): ${latencies.join(", ")}`);
  for (const latency of latencies) assert.ok(latency < 500, `latency ${latency} ms`);
});

test("bus_wait times out cleanly and the server keeps serving", async (t) => {
  const { dbPath, preload, bus } = setup(t);
  const bob = await startServer(t, dbPath, preload, "bob");
  const started = Date.now();
  const result = await bob.client.callTool({ name: "bus_wait", arguments: { timeout_sec: 1 } }, undefined, LONG);
  const elapsed = Date.now() - started;
  t.diagnostic(`1 s wait returned after ${elapsed} ms`);
  assert.notEqual((result as { isError?: boolean }).isError, true);
  assert.match(text(result), /Nothing arrived within 1s/);
  assert.ok(elapsed >= 950 && elapsed < 3000, `elapsed ${elapsed} ms`);
  const agent = bus.getAgent("bob")!;
  assert.equal(agent.storedStatus, "idle");
  assert.equal(agent.waitUntilMs, null);
  assert.match(text(await bob.client.callTool({ name: "bus_whoami", arguments: {} })), /You are bob/);

  const invalid = await bob.client.callTool({ name: "bus_wait", arguments: { timeout_sec: 0 } });
  assert.equal((invalid as { isError?: boolean }).isError, true);
});

test("killing the server with SIGKILL during a wait loses no mail", async (t) => {
  const { dbPath, preload, bus } = setup(t);
  const first = await startServer(t, dbPath, preload, "bob");
  const pending = first.client.callTool({ name: "bus_wait", arguments: { timeout_sec: 60 } }, undefined, LONG).then(() => "returned", () => "failed");
  await until(() => bus.getAgent("bob")?.storedStatus === "waiting");
  const pid = first.transport.pid!;
  process.kill(pid, "SIGKILL");
  await until(() => { try { process.kill(pid, 0); return false; } catch { return true; } });
  assert.equal(await pending, "failed", "the killed wait did not return a reply");

  const [sent] = await cliSend(dbPath, "alice", "bob", "after the kill");
  assert.equal(bus.unreadCount("bob"), 1, "the mail is stored and unread");

  const second = await startServer(t, dbPath, preload, "bob");
  const started = Date.now();
  const result = await second.client.callTool({ name: "bus_wait", arguments: { timeout_sec: 30 } }, undefined, LONG);
  assert.match(text(result), /after the kill/);
  assert.match(text(result), new RegExp(`#${sent.seq} `));
  assert.ok(Date.now() - started < 2000, "the restarted wait returned at once");
  assert.equal(bus.unreadCount("bob"), 0);
});

test("the inbox signal file updates on every delivery", async (t) => {
  const { dbPath, bus } = setup(t);
  assert.equal(readSignalFile(dbPath, "bob"), null);
  const [first] = await cliSend(dbPath, "alice", "bob", "one");
  assert.equal(readSignalFile(dbPath, "bob"), first.seq);
  const [second] = await cliSend(dbPath, "alice", "bob", "two");
  assert.equal(readSignalFile(dbPath, "bob"), second.seq);
  const aliceBefore = readSignalFile(dbPath, "alice");
  const [broadcast] = await cliSend(dbPath, "alice", "*", "everyone");
  assert.equal(readSignalFile(dbPath, "bob"), broadcast.seq);
  assert.equal(readSignalFile(dbPath, "operator"), broadcast.seq);
  assert.equal(readSignalFile(dbPath, "alice"), aliceBefore, "the sender's own signal file is untouched");
  const task = bus.createTask(bus.identify("alice"), { title: "t", brief: "b", to: "bob" });
  assert.equal(readSignalFile(dbPath, "bob"), bus.getTask(task.id).messages[0].seq);
});

test("the signal file alone wakes a waiter when the database watcher is slow", async (t) => {
  const { dbPath, bus } = setup(t);
  const bob = bus.identify("bob");
  // No fs.watch on the database and a 5 s poll: only the inbox signal file can wake this waiter quickly.
  const waiting = waitForMail(bus, bob, { timeoutMs: 30_000, watcherOptions: { fsWatch: false, minPollMs: 5000, maxPollMs: 5000 } })
    .then((result) => ({ result, at: Date.now() }));
  await sleep(200);
  const [sent] = await cliSend(dbPath, "alice", "bob", "wake");
  const { result, at } = await waiting;
  t.diagnostic(`signal-file wake latency ${at - sent.tsMs} ms`);
  assert.equal(result.status, "mail");
  assert.equal(result.messages[0].subject, "wake");
  assert.ok(at - sent.tsMs < 500, `latency ${at - sent.tsMs} ms`);
  assert.equal(bus.unreadCount("bob"), 1, "waitForMail does not advance the cursor");
});
