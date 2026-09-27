#!/usr/bin/env node
// v2 dashboard smoke: starts `node dist/qagent.js dashboard` on a temporary database, signs in with
// headless Chrome through the printed single-use link, and checks the page at 375 px and 1280 px:
// no horizontal scroll, a CLI message appears live over SSE, and the page's send form reaches an
// agent's `qagent wait`. Needs `npm run build:core` first and CHROME_BIN (or Chrome on PATH).
// Set QAGENT_SMOKE_SCREENSHOTS=<dir> to save a PNG per width.
import assert from "node:assert/strict";
import { execFileSync, spawn, spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const QAGENT = resolve("dist/qagent.js");
if (!existsSync(QAGENT) || !existsSync(resolve("dist/dashboard/entry.js"))) {
  process.stderr.write("dist/qagent.js or dist/dashboard/entry.js is missing; run `npm run build:core` first\n");
  process.exit(1);
}

function chromeBinary() {
  if (process.env.CHROME_BIN) return process.env.CHROME_BIN;
  for (const path of [
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
    "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
  ]) {
    if (existsSync(path)) return path;
  }
  for (const name of ["google-chrome", "google-chrome-stable", "chromium", "chromium-browser"]) {
    try { return execFileSync("which", [name], { encoding: "utf8" }).trim(); } catch { /* next */ }
  }
  throw new Error("Chrome/Chromium not found; set CHROME_BIN");
}

const home = mkdtempSync(join(tmpdir(), "qagent-v2-dashboard-smoke-"));
const dbPath = join(home, "bus.db");
const sleep = (ms) => new Promise((done) => setTimeout(done, ms));
const children = [];

function env(agent) {
  const value = { ...process.env, QAGENT_BUS_DB: dbPath };
  for (const name of ["QAGENT_AGENT_ID", "AGENT_ID", "QAGENT_HOME", "AGENT_BUS_HOME", "QAGENT_BLOCK_SEC", "QAGENT_DASHBOARD_PORT"]) delete value[name];
  if (agent) value.QAGENT_AGENT_ID = agent;
  return value;
}

function ok(agent, args) {
  const result = spawnSync(process.execPath, [QAGENT, ...args], { env: env(agent), encoding: "utf8" });
  assert.equal(result.status, 0, `qagent ${args.join(" ")}: ${result.stderr}`);
  return result.stdout;
}

function freePort() {
  return new Promise((done, fail) => {
    const server = createServer();
    server.once("error", fail);
    server.listen(0, "127.0.0.1", () => {
      const { port } = server.address();
      server.close(() => done(port));
    });
  });
}

async function waitFor(label, fn, timeoutMs = 10_000, stepMs = 50) {
  const deadline = Date.now() + timeoutMs;
  let last;
  while (Date.now() < deadline) {
    last = await fn();
    if (last) return last;
    await sleep(stepMs);
  }
  throw new Error(`timed out: ${label}`);
}

function cdp(url) {
  const ws = new WebSocket(url);
  const pending = new Map();
  let id = 0;
  ws.addEventListener("message", (event) => {
    const message = JSON.parse(String(event.data));
    if (message.id && pending.has(message.id)) {
      const { done, fail } = pending.get(message.id);
      pending.delete(message.id);
      if (message.error) fail(new Error(message.error.message)); else done(message.result);
    }
  });
  const ready = new Promise((done, fail) => { ws.addEventListener("open", done, { once: true }); ws.addEventListener("error", fail, { once: true }); });
  return {
    ready,
    close: () => ws.close(),
    async send(method, params = {}) {
      await ready;
      const callId = ++id;
      ws.send(JSON.stringify({ id: callId, method, params }));
      return await new Promise((done, fail) => pending.set(callId, { done, fail }));
    },
  };
}

async function evaluate(page, expression) {
  const result = await page.send("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true });
  if (result.exceptionDetails) throw new Error(`page exception: ${result.exceptionDetails.text} ${result.exceptionDetails.exception?.description ?? ""}`);
  return result.result.value;
}

let dashboard;
let browser;
try {
  // A bus with enough content to stress the narrow layout.
  ok(undefined, ["init"]);
  ok(undefined, ["agent", "add", "lead", "--role", "manager", "--authority", "manager"]);
  ok(undefined, ["agent", "add", "worker-with-a-rather-long-identifier", "--role", "worker"]);
  ok("lead", ["task", "add", "Port the parser and keep every existing configuration file loading exactly as before", "--to", "worker-with-a-rather-long-identifier"]);
  ok("lead", ["task", "add", "Unassigned follow-up"]);
  ok("lead", ["send", "worker-with-a-rather-long-identifier", "kickoff", "x".repeat(400)]);
  ok("lead", ["send", "*", "/workspace/a/very/long/path/without/spaces/that/must/wrap/instead/of/scrolling/sideways.ts"]);
  const inboxDrain = () => ok("worker-with-a-rather-long-identifier", ["inbox", "--limit", "200"]);
  inboxDrain();

  // The dashboard as a separate process, exactly as an operator starts it.
  const port = await freePort();
  dashboard = spawn(process.execPath, [QAGENT, "dashboard", "--port", String(port)], { env: env(), stdio: ["ignore", "pipe", "pipe"] });
  children.push(dashboard);
  let out = "";
  let err = "";
  dashboard.stdout.setEncoding("utf8").on("data", (chunk) => { out += chunk; });
  dashboard.stderr.setEncoding("utf8").on("data", (chunk) => { err += chunk; });
  const signIn = await waitFor("dashboard sign-in link", () => /sign in[^:]*: (http:\/\/127\.0\.0\.1:\d+\/#t=[\w-]+)/.exec(out)?.[1] || (dashboard.exitCode !== null && Promise.reject(new Error(err))), 10_000);
  process.stdout.write(`dashboard up on 127.0.0.1:${port}\n`);

  // Headless Chrome over the DevTools protocol.
  const debugPort = await freePort();
  const profile = join(home, "chrome-profile");
  mkdirSync(profile);
  browser = spawn(chromeBinary(), [
    "--headless=new", "--no-sandbox", "--disable-gpu", "--disable-dev-shm-usage", "--no-first-run", "--no-default-browser-check",
    "--remote-debugging-address=127.0.0.1", `--remote-debugging-port=${debugPort}`, `--user-data-dir=${profile}`, "about:blank",
  ], { stdio: ["ignore", "ignore", "pipe"] });
  children.push(browser);
  const targets = await waitFor("Chrome DevTools", async () => {
    try { return await (await fetch(`http://127.0.0.1:${debugPort}/json/list`)).json(); } catch { return null; }
  }, 20_000, 150);
  const page = cdp(targets.find((target) => target.type === "page").webSocketDebuggerUrl);
  await page.send("Page.enable");
  await page.send("Runtime.enable");
  await page.send("Emulation.setDeviceMetricsOverride", { width: 375, height: 812, deviceScaleFactor: 2, mobile: true });
  await page.send("Page.navigate", { url: signIn });
  await waitFor("signed-in page with a live stream", () => evaluate(page, `document.getElementById("live")?.textContent === "live" && !!document.getElementById("agent-rows")`), 10_000, 100);
  process.stdout.write("signed in through the single-use link; SSE stream live\n");
  assert.ok(!(await evaluate(page, "location.href")).includes("#t="), "the ticket is removed from the address bar");
  const cookieVisible = await evaluate(page, "document.cookie");
  assert.equal(cookieVisible, "", "the session cookie is HttpOnly");

  const layout = (width) => evaluate(page, `(() => ({
    innerWidth: window.innerWidth,
    scrollWidth: Math.max(document.documentElement.scrollWidth, document.body.scrollWidth),
    sections: Array.from(document.querySelectorAll("h2")).map((h) => h.textContent),
    text: document.body.innerText,
  }))()`).then((value) => { value.width = width; return value; });

  const shot = async (name) => {
    const dir = process.env.QAGENT_SMOKE_SCREENSHOTS;
    if (!dir) return;
    mkdirSync(dir, { recursive: true });
    const { data } = await page.send("Page.captureScreenshot", { format: "png", captureBeyondViewport: true });
    writeFileSync(join(dir, name), Buffer.from(data, "base64"));
  };

  const narrow = await layout(375);
  assert.equal(narrow.innerWidth, 375);
  assert.ok(narrow.scrollWidth <= narrow.innerWidth, `horizontal scroll at 375 px: scrollWidth ${narrow.scrollWidth}`);
  assert.deepEqual(narrow.sections, ["Agents", "Open tasks", "Recent messages", "Send a message"]);
  for (const word of ["worker-with-a-rather-long-identifier", "Port the parser", "unassigned", "kickoff"]) assert.ok(narrow.text.includes(word), `page shows ${word}`);
  process.stdout.write(`375 px: scrollWidth ${narrow.scrollWidth} <= innerWidth ${narrow.innerWidth}; four sections present\n`);
  await shot("dashboard-375.png");

  // An agent's CLI message appears without a reload.
  const marker = `live check ${Date.now()}`;
  const sentAt = Date.now();
  ok("lead", ["send", "worker-with-a-rather-long-identifier", marker]);
  await waitFor("CLI message on the page", () => evaluate(page, `document.getElementById("message-rows").textContent.includes(${JSON.stringify(marker)})`), 5000, 20);
  process.stdout.write(`CLI message rendered on the page ${Date.now() - sentAt} ms after the send command started\n`);
  inboxDrain();

  // The page's form reaches the agent's qagent wait.
  const waiter = spawn(process.execPath, [QAGENT, "wait", "--timeout", "30", "--json"], { env: env("worker-with-a-rather-long-identifier"), stdio: ["ignore", "pipe", "pipe"] });
  children.push(waiter);
  let waitOut = "";
  waiter.stdout.setEncoding("utf8").on("data", (chunk) => { waitOut += chunk; });
  const waited = new Promise((done) => waiter.on("close", done));
  await waitFor("agent shown as waiting", () => evaluate(page, `document.getElementById("agent-rows").textContent.includes("waiting")`), 10_000, 50);
  await evaluate(page, `(() => {
    document.getElementById("send-to").value = "worker-with-a-rather-long-identifier";
    document.getElementById("send-text").value = "from the dashboard page\\nsecond line";
    document.getElementById("send").requestSubmit();
    return true;
  })()`);
  assert.equal(await waited, 0, "qagent wait exits 0 on mail");
  const received = JSON.parse(waitOut);
  assert.equal(received.status, "mail");
  assert.equal(received.messages[0].sender, "operator");
  assert.equal(received.messages[0].body, "from the dashboard page\nsecond line");
  await waitFor("send status", () => evaluate(page, `document.getElementById("send-status").textContent.startsWith("sent to")`), 5000);
  process.stdout.write("page send form reached the agent's qagent wait\n");

  await page.send("Emulation.setDeviceMetricsOverride", { width: 1280, height: 900, deviceScaleFactor: 1, mobile: false });
  await sleep(200);
  const wide = await layout(1280);
  assert.ok(wide.scrollWidth <= wide.innerWidth, `horizontal scroll at 1280 px: scrollWidth ${wide.scrollWidth}`);
  const column = await evaluate(page, `document.querySelector("main").getBoundingClientRect().width`);
  assert.ok(column <= 960, `reading column ${column} px wider than 960`);
  process.stdout.write(`1280 px: no horizontal scroll; reading column ${column} px\n`);
  await shot("dashboard-1280.png");
  page.close();

  // The listener is loopback only.
  const listening = spawnSync("lsof", ["-nP", `-iTCP:${port}`, "-sTCP:LISTEN"], { encoding: "utf8" });
  if (listening.status === 0) {
    assert.match(listening.stdout, /127\.0\.0\.1:\d+ \(LISTEN\)/);
    assert.doesNotMatch(listening.stdout, /\*:\d+ \(LISTEN\)/);
    process.stdout.write("lsof: listener bound to 127.0.0.1 only\n");
  }
  process.stdout.write("v2 dashboard smoke passed\n");
} catch (error) {
  process.stderr.write(`v2 dashboard smoke failed: ${error.stack ?? error}\n`);
  process.exitCode = 1;
} finally {
  for (const child of children.reverse()) if (child.exitCode === null) child.kill("SIGTERM");
  await sleep(300);
  for (const child of children) if (child.exitCode === null) child.kill("SIGKILL");
  rmSync(home, { recursive: true, force: true });
}
