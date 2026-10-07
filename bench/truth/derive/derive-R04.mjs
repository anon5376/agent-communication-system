// derive-R04.mjs: measure idle database reads of the qagent dashboard at commit 4d4cf5a.
// Usage: node derive-R04.mjs [repoCopy] [idleMs]
//   repoCopy defaults to ./repo next to this script (a copy of the commit with node_modules symlinked).
// Method: wrap node:sqlite StatementSync.prototype.{get,all,run,iterate} process-wide, so every
// prepared-statement execution by the dashboard's connection is counted with its SQL text and a
// timestamp. Also report the dashboard's own stats.queries counter. No writes during the window.
import { mkdtempSync, rmSync } from "node:fs";
import { request } from "node:http";
import { tmpdir } from "node:os";
import { join, resolve, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { StatementSync } from "node:sqlite";

const here = dirname(fileURLToPath(import.meta.url));
const repo = resolve(process.argv[2] ?? join(here, "repo"));
const IDLE_MS = Number(process.argv[3] ?? 60_000);
const { Bus } = await import(pathToFileURL(join(repo, "dist/core/bus.js")).href);
const { startDashboard } = await import(pathToFileURL(join(repo, "dist/dashboard/server.js")).href);

const log = [];
let counting = false;
let t0 = 0;
for (const method of ["get", "all", "run", "iterate"]) {
  const original = StatementSync.prototype[method];
  StatementSync.prototype[method] = function (...args) {
    if (counting) log.push({ at: Date.now() - t0, sql: this.sourceSQL ?? "?" });
    return original.apply(this, args);
  };
}

const home = mkdtempSync(join(tmpdir(), "r04-dash-"));
const dbPath = join(home, "bus.db");
const bus = Bus.open({ dbPath });
bus.init();
const operator = bus.identify("operator");
bus.addAgent(operator, { id: "alice", role: "manager", authority: "manager" });
bus.addAgent(operator, { id: "bob", role: "worker" });
bus.createTask(bus.identify("alice"), { title: "Write the parser", to: "bob" });

const dashboard = await startDashboard({ dbPath, port: 0 }); // default safetyMs (10 s), as `qagent dashboard`
const port = dashboard.port;
function req(method, path, headers = {}, body) {
  return new Promise((ok, fail) => {
    const r = request({ host: "127.0.0.1", port, method, path, headers: { host: `127.0.0.1:${port}`, ...headers } }, (res) => {
      let text = ""; res.setEncoding("utf8").on("data", (c) => { text += c; }); res.on("end", () => ok({ status: res.statusCode, headers: res.headers, body: text }));
    });
    r.on("error", fail); r.end(body === undefined ? undefined : JSON.stringify(body));
  });
}
const ticket = dashboard.signInUrl().split("#t=")[1];
const session = await req("POST", "/session", { origin: `http://127.0.0.1:${port}`, "content-type": "application/json" }, { ticket });
const cookie = String(session.headers["set-cookie"]?.[0] ?? "").split(";")[0];
const seq = bus.latestSeq();

const before = { ...dashboard.stats };
t0 = Date.now();
counting = true;
let frames = 0;
const stream = request({ host: "127.0.0.1", port, path: `/api/events?since=${seq}`, headers: { host: `127.0.0.1:${port}`, cookie } }, (res) => {
  console.log(`stream status ${res.statusCode}`);
  res.setEncoding("utf8").on("data", (chunk) => { frames += (chunk.match(/\n\n/g) ?? []).length; });
});
stream.on("error", () => {});
stream.end();
await new Promise((r) => setTimeout(r, IDLE_MS));
counting = false;
const after = { ...dashboard.stats };

const bySql = {};
for (const e of log) bySql[e.sql] = (bySql[e.sql] ?? 0) + 1;
console.log(`idle window ${IDLE_MS} ms, 1 stream, no writes, default safetyMs`);
console.log(`statement executions: ${log.length}`);
for (const [sql, n] of Object.entries(bySql)) console.log(`  ${n}  ${sql}`);
console.log(`dashboard stats.queries delta: ${after.queries - before.queries}, deltas: ${after.deltas - before.deltas}, streams: ${after.streams}`);
console.log(`timestamps (ms since stream open): ${log.map((e) => e.at).join(" ")}`);
const lastHalf = log.filter((e) => e.at >= IDLE_MS / 2).length;
console.log(`executions in second half (${IDLE_MS / 2}-${IDLE_MS} ms): ${lastHalf}`);
console.log(`SSE frames received (incl. retry line): ${frames}`);
stream.destroy();
await dashboard.close();
bus.close();
rmSync(home, { recursive: true, force: true });
