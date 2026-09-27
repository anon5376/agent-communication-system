import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { request as httpRequest, type IncomingMessage } from "node:http";
import { connect } from "node:net";
import { networkInterfaces, tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { setTimeout as sleep } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { Bus } from "../src/core/bus.js";
import { startDashboard, type DashboardHandle } from "../src/dashboard/server.js";

const QAGENT = fileURLToPath(new URL("../src/qagent.js", import.meta.url));
/** Shortened idle window for the unit suite. The acceptance run uses QAGENT_DASHBOARD_IDLE_MS=300000 QAGENT_DASHBOARD_SAFETY_MS=10000. */
const IDLE_MS = Number(process.env.QAGENT_DASHBOARD_IDLE_MS ?? 6000);
const SAFETY_MS = Number(process.env.QAGENT_DASHBOARD_SAFETY_MS ?? 1000);

function cliEnv(dbPath: string, agent?: string): NodeJS.ProcessEnv {
  const env: NodeJS.ProcessEnv = { ...process.env, QAGENT_BUS_DB: dbPath };
  for (const name of ["QAGENT_AGENT_ID", "AGENT_ID", "QAGENT_HOME", "AGENT_BUS_HOME", "QAGENT_BLOCK_SEC"]) delete env[name];
  if (agent) env.QAGENT_AGENT_ID = agent;
  return env;
}

async function setup(t: { after: (fn: () => unknown) => void }, safetyMs = 10_000) {
  const home = mkdtempSync(join(tmpdir(), "qagent-v2-dashboard-"));
  t.after(() => rmSync(home, { recursive: true, force: true }));
  const dbPath = join(home, "bus.db");
  const bus = Bus.open({ dbPath });
  t.after(() => bus.close());
  bus.init();
  const operator = bus.identify("operator");
  bus.addAgent(operator, { id: "alice", role: "manager", authority: "manager" });
  bus.addAgent(operator, { id: "bob", role: "worker" });
  bus.createTask(bus.identify("alice"), { title: "Write the <parser>", to: "bob" });
  const dashboard = await startDashboard({ dbPath, port: 0, safetyMs });
  t.after(() => dashboard.close());
  return { home, dbPath, bus, dashboard };
}

function run(args: string[], env: NodeJS.ProcessEnv): Promise<{ status: number | null; stdout: string; stderr: string }> {
  return new Promise((resolve) => {
    const child = spawn(process.execPath, args, { env, stdio: ["ignore", "pipe", "pipe"] });
    let stdout = "";
    let stderr = "";
    child.stdout.setEncoding("utf8").on("data", (chunk: string) => { stdout += chunk; });
    child.stderr.setEncoding("utf8").on("data", (chunk: string) => { stderr += chunk; });
    const timer = setTimeout(() => child.kill(), 15_000);
    child.on("close", (status) => { clearTimeout(timer); resolve({ status, stdout, stderr }); });
  });
}

interface Reply { status: number; headers: IncomingMessage["headers"]; body: string }

function request(port: number, method: string, path: string, headers: Record<string, string> = {}, body?: unknown): Promise<Reply> {
  return new Promise((resolve, reject) => {
    const payload = body === undefined ? undefined : JSON.stringify(body);
    const req = httpRequest({ host: "127.0.0.1", port, method, path, headers: { host: `127.0.0.1:${port}`, ...headers } }, (res) => {
      let text = "";
      res.setEncoding("utf8").on("data", (chunk: string) => { text += chunk; });
      res.on("end", () => resolve({ status: res.statusCode ?? 0, headers: res.headers, body: text }));
    });
    req.on("error", reject);
    req.end(payload);
  });
}

async function signIn(dashboard: DashboardHandle): Promise<string> {
  const ticket = dashboard.signInUrl().split("#t=")[1];
  const reply = await request(dashboard.port, "POST", "/session", { origin: `http://127.0.0.1:${dashboard.port}`, "content-type": "application/json" }, { ticket });
  assert.equal(reply.status, 200, reply.body);
  const cookie = String(reply.headers["set-cookie"]?.[0] ?? "").split(";")[0];
  assert.match(cookie, /^qagent_dash=/);
  return cookie;
}

interface Frame { event: string; data: Record<string, unknown>; at: number }

function openStream(port: number, cookie: string, since: number) {
  const frames: Frame[] = [];
  let notify: (() => void) | null = null;
  let response: IncomingMessage | null = null;
  const req = httpRequest({ host: "127.0.0.1", port, path: `/api/events?since=${since}`, headers: { host: `127.0.0.1:${port}`, cookie } }, (res) => {
    response = res;
    let buffer = "";
    res.setEncoding("utf8").on("data", (chunk: string) => {
      buffer += chunk;
      let index: number;
      while ((index = buffer.indexOf("\n\n")) >= 0) {
        const raw = buffer.slice(0, index);
        buffer = buffer.slice(index + 2);
        let event = "message";
        let data = "";
        for (const line of raw.split("\n")) {
          if (line.startsWith("event: ")) event = line.slice(7);
          else if (line.startsWith("data: ")) data += line.slice(6);
        }
        if (data) frames.push({ event, data: JSON.parse(data), at: Date.now() });
        notify?.();
      }
    });
  });
  req.on("error", () => { /* closed by the test */ });
  req.end();
  return {
    frames,
    get status() { return response?.statusCode ?? 0; },
    async waitFor(predicate: (frame: Frame) => boolean, timeoutMs: number): Promise<Frame> {
      const deadline = Date.now() + timeoutMs;
      for (;;) {
        const found = frames.find(predicate);
        if (found) return found;
        const remaining = deadline - Date.now();
        if (remaining <= 0) throw new Error(`no matching SSE frame within ${timeoutMs} ms; got ${JSON.stringify(frames).slice(0, 500)}`);
        await new Promise<void>((resolve) => { notify = resolve; setTimeout(resolve, Math.min(remaining, 50)); });
      }
    },
    close() { req.destroy(); },
  };
}

function lanAddresses(): string[] {
  const out: string[] = [];
  for (const list of Object.values(networkInterfaces())) {
    for (const info of list ?? []) if (!info.internal && (info.family === "IPv4" || info.family === "IPv6") && !info.address.startsWith("fe80")) out.push(info.address);
  }
  return out;
}

function tryConnect(host: string, port: number): Promise<string> {
  return new Promise((resolve) => {
    const socket = connect({ host, port, timeout: 1000 });
    socket.once("connect", () => { socket.destroy(); resolve("connected"); });
    socket.once("timeout", () => { socket.destroy(); resolve("timeout"); });
    socket.once("error", (error: NodeJS.ErrnoException) => resolve(error.code ?? "error"));
  });
}

test("dashboard binds 127.0.0.1 only", async (t) => {
  const { dashboard } = await setup(t);
  assert.equal(await tryConnect("127.0.0.1", dashboard.port), "connected");
  assert.notEqual(await tryConnect("::1", dashboard.port), "connected", "IPv6 loopback must not reach the dashboard");
  const addresses = lanAddresses();
  if (!addresses.length) t.diagnostic("no non-loopback interface on this machine; LAN check skipped");
  for (const address of addresses) {
    const outcome = await tryConnect(address, dashboard.port);
    t.diagnostic(`connect ${address}:${dashboard.port} -> ${outcome}`);
    assert.notEqual(outcome, "connected", `dashboard reachable on ${address}`);
  }
});

test("/api/* needs a session, writes need the same origin, and Host must be loopback", async (t) => {
  const { dashboard, home } = await setup(t);
  const port = dashboard.port;
  const origin = `http://127.0.0.1:${port}`;
  const json = { "content-type": "application/json" };
  for (const path of ["/api/state", "/api/events", "/api/task/1"]) assert.equal((await request(port, "GET", path)).status, 401, path);
  assert.equal((await request(port, "POST", "/api/send", { ...json, origin }, { to: "bob", text: "x" })).status, 401);
  assert.equal((await request(port, "GET", "/api/state", { cookie: "qagent_dash=forged" })).status, 401);

  const cookie = await signIn(dashboard);
  assert.equal((await request(port, "GET", "/api/state", { cookie })).status, 200);
  const crossOrigin = await request(port, "POST", "/api/send", { ...json, cookie, origin: "http://evil.example" }, { to: "bob", text: "x" });
  assert.equal(crossOrigin.status, 403, crossOrigin.body);
  assert.equal((await request(port, "POST", "/api/send", { ...json, cookie }, { to: "bob", text: "x" })).status, 403, "no Origin header");
  assert.equal((await request(port, "POST", "/api/send", { ...json, cookie, origin, "sec-fetch-site": "cross-site" }, { to: "bob", text: "x" })).status, 403);
  assert.equal((await request(port, "POST", "/api/send", { "content-type": "text/plain", cookie, origin }, { to: "bob", text: "x" })).status, 400);
  assert.equal((await request(port, "GET", "/api/state", { cookie, host: `evil.example:${port}` })).status, 421, "DNS-rebinding Host");
  const sent = await request(port, "POST", "/api/send", { ...json, cookie, origin }, { to: "bob", text: "hello bob" });
  assert.equal(sent.status, 200, sent.body);

  // Tickets: single use, only for the operator token, and never exchanged cross-origin.
  const ticket = dashboard.signInUrl().split("#t=")[1];
  assert.equal((await request(port, "POST", "/session", { ...json, origin: "http://evil.example" }, { ticket })).status, 403);
  assert.equal((await request(port, "POST", "/session", { ...json, origin }, { ticket })).status, 200);
  assert.equal((await request(port, "POST", "/session", { ...json, origin }, { ticket })).status, 401, "a ticket works once");
  assert.equal((await request(port, "POST", "/login", json, { operatorToken: "wrong" })).status, 401);
  const bobToken = readFileSync(join(home, "tokens", "bob.token"), "utf8").trim();
  assert.equal((await request(port, "POST", "/login", json, { operatorToken: bobToken })).status, 401, "an agent token is not the operator token");
  // Async spawn: the server runs in this process, so a synchronous child would deadlock it.
  const link = await run([QAGENT, "dashboard", "link", "--port", String(port)], cliEnv(join(home, "bus.db")));
  assert.equal(link.status, 0, link.stderr);
  assert.match(link.stdout.trim(), new RegExp(`^http://127\\.0\\.0\\.1:${port}/#t=[A-Za-z0-9_-]+$`));
});

test("the page is server-rendered, escaped, and shows data only with a session", async (t) => {
  const { dashboard } = await setup(t);
  const signedOut = await request(dashboard.port, "GET", "/");
  assert.equal(signedOut.status, 200);
  assert.ok(!signedOut.body.includes("parser"), "no data without a session");
  const cookie = await signIn(dashboard);
  const page = await request(dashboard.port, "GET", "/", { cookie });
  const nonce = /script-src 'nonce-([^']+)'/.exec(String(page.headers["content-security-policy"]))?.[1];
  assert.ok(nonce, "CSP carries a nonce");
  assert.ok(page.body.includes(`<script nonce="${nonce}">`));
  for (const heading of ["Agents", "Open tasks", "Recent messages", "Send a message"]) assert.ok(page.body.includes(`>${heading}</h2>`), heading);
  assert.ok(page.body.includes("Write the &lt;parser&gt;"), "task title rendered and escaped");
  assert.ok(!page.body.includes("<parser>"), "raw markup never reaches the page");
  assert.ok(page.body.includes("function agentRows("), "the client reuses the server's row renderers");
  assert.ok(!/<script[^>]+src=|<link[^>]+stylesheet/.test(page.body), "no external scripts or styles");
});

test("an agent's CLI message reaches an open SSE stream within 500 ms", async (t) => {
  const { dashboard, dbPath, bus } = await setup(t);
  const cookie = await signIn(dashboard);
  const stream = openStream(dashboard.port, cookie, bus.latestSeq());
  t.after(() => stream.close());
  await sleep(300);
  assert.equal(stream.status, 200);
  const latencies: number[] = [];
  for (let round = 0; round < 3; round += 1) {
    const subject = `ping ${round}`;
    const sent = await run([QAGENT, "send", "bob", subject, "body", "--json"], cliEnv(dbPath, "alice"));
    assert.equal(sent.status, 0, sent.stderr);
    const committedMs = Number(JSON.parse(sent.stdout)[0].tsMs);
    const frame = await stream.waitFor((f) => f.event === "change" && (f.data.messages as { line: string }[]).some((m) => m.line === subject), 3000);
    latencies.push(frame.at - committedMs);
    await sleep(150);
  }
  t.diagnostic(`SSE latency ms (received - committed): ${latencies.join(", ")}`);
  for (const latency of latencies) assert.ok(latency < 500, `latency ${latency} ms`);

  // A burst: two processes write within tens of milliseconds (read the inbox, then start waiting).
  // fs.watch can fold these into one event; the second write must still arrive promptly.
  assert.equal((await run([QAGENT, "inbox"], cliEnv(dbPath, "bob"))).status, 0);
  const waiter = run([QAGENT, "wait", "--timeout", "3"], cliEnv(dbPath, "bob"));
  const frame = await stream.waitFor((f) => (f.data.events as { kind: string }[] | undefined)?.some((e) => e.kind === "agent_waiting") ?? false, 2000);
  const waitingEvent = (frame.data.events as { kind: string; tsMs: number }[]).find((e) => e.kind === "agent_waiting")!;
  t.diagnostic(`burst: agent_waiting reached the stream ${frame.at - waitingEvent.tsMs} ms after commit`);
  assert.ok(frame.at - waitingEvent.tsMs < 500, "burst write delivered within 500 ms");
  await waiter;
});

test("a dashboard send reaches the agent's qagent wait", async (t) => {
  const { dashboard, dbPath, bus } = await setup(t);
  const cookie = await signIn(dashboard);
  bus.inbox(bus.identify("bob")); // drain the task-assignment mail so the wait blocks
  const child = spawn(process.execPath, [QAGENT, "wait", "--timeout", "30", "--json"], { env: cliEnv(dbPath, "bob"), stdio: ["ignore", "pipe", "pipe"] });
  let out = "";
  let err = "";
  child.stdout.setEncoding("utf8").on("data", (chunk: string) => { out += chunk; });
  child.stderr.setEncoding("utf8").on("data", (chunk: string) => { err += chunk; });
  const exited = new Promise<number | null>((resolve) => child.on("close", resolve));
  t.after(() => { child.kill(); });
  const deadline = Date.now() + 10_000;
  while (bus.getAgent("bob")?.storedStatus !== "waiting") {
    assert.ok(Date.now() < deadline, `bob never started waiting: ${err}`);
    await sleep(50);
  }
  // The waiter has already drained any earlier mail check; send the one message it should receive.
  const started = Date.now();
  const reply = await request(dashboard.port, "POST", "/api/send", { "content-type": "application/json", cookie, origin: `http://127.0.0.1:${dashboard.port}` }, { to: "bob", text: "review the parser\nsecond line" });
  assert.equal(reply.status, 200, reply.body);
  assert.equal(await exited, 0, err);
  t.diagnostic(`qagent wait returned ${Date.now() - started} ms after the dashboard send`);
  const result = JSON.parse(out) as { status: string; messages: { sender: string; body: string }[] };
  assert.equal(result.status, "mail");
  assert.equal(result.messages[0].sender, "operator");
  assert.equal(result.messages[0].body, "review the parser\nsecond line");
});

test(`three idle streams cost at most one database query per safety interval (${IDLE_MS} ms window, ${SAFETY_MS} ms interval)`, { timeout: IDLE_MS + 60_000 }, async (t) => {
  const { dashboard, bus } = await setup(t, SAFETY_MS);
  const cookie = await signIn(dashboard);
  const streams = [0, 1, 2].map(() => openStream(dashboard.port, cookie, bus.latestSeq()));
  t.after(() => { for (const stream of streams) stream.close(); });
  // Let the post-change back-off (25 ms doubling up to the safety interval, about 2x the interval in total) settle.
  await sleep(SAFETY_MS * 3 + 500);
  assert.equal(dashboard.stats.streams, 3);
  const before = { ...dashboard.stats };
  await sleep(IDLE_MS);
  const after = { ...dashboard.stats };
  const queries = after.queries - before.queries;
  const allowed = Math.ceil(IDLE_MS / SAFETY_MS);
  t.diagnostic(`idle: ${queries} queries in ${IDLE_MS} ms with 3 streams (allowed ${allowed}); deltas ${after.deltas - before.deltas}`);
  assert.ok(queries <= allowed, `${queries} queries > ${allowed}`);
  assert.equal(after.deltas, before.deltas, "no delta reads while idle");
  for (const stream of streams) assert.equal(stream.frames.length, 0, "no frames pushed while idle");
});
