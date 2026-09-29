// Perf probe (not part of the test suite): seeds a bus and counts prepare()
// calls + wall time for hot read paths. Run: node scripts/v2-perf-probe.mjs [buildDir]
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const buildDir = process.argv[2] ?? "dist";
const { Bus } = await import(`../${buildDir}/core/bus.js`);

const home = mkdtempSync(join(tmpdir(), "qagent-perf-"));
const dbPath = join(home, "bus.db");
const bus = Bus.open({ dbPath });
bus.init();
const operator = bus.identify("operator");
bus.addAgent(operator, { id: "lead", role: "manager", authority: "manager" });
for (let i = 0; i < 20; i++) bus.addAgent(operator, { id: `w${i}`, role: "worker" });
const lead = bus.identify("lead");

// 200 tasks, every other one carrying 2 dependencies.
let previous = [];
for (let i = 0; i < 200; i++) {
  const task = bus.createTask(lead, {
    title: `task ${i}`,
    dependencies: i > 1 && i % 2 === 0 ? previous.slice(-2) : [],
    priority: i % 10 === 0 ? "urgent" : i % 5 === 0 ? "high" : "normal",
  });
  previous.push(task.id);
}
const worker = bus.identify("w0");
for (let i = 0; i < 50; i++) bus.send(lead, { to: "w0", subject: `m${i}`, body: "x" });

// Count prepares by wrapping prepare() on the live connection.
let prepares = 0;
const original = bus.db.prepare.bind(bus.db);
bus.db.prepare = (sql) => { prepares += 1; return original(sql); };

function measure(name, fn, rounds = 5) {
  const times = [];
  let calls = 0;
  for (let i = 0; i < rounds; i++) {
    prepares = 0;
    const t0 = performance.now();
    fn();
    times.push(performance.now() - t0);
    calls = prepares;
  }
  times.sort((a, b) => a - b);
  console.log(`${name.padEnd(34)} median ${times[Math.floor(rounds / 2)].toFixed(1)} ms   prepares/call ${calls}`);
}

measure("listTasks(200)", () => bus.listTasks({ limit: 200 }));
measure("status()", () => bus.status());
measure("inbox(peek)", () => bus.inbox(worker, { peek: true }));
measure("inbox()", () => bus.inbox(worker));
measure("claimTask(auto)", () => { try { bus.claimTask(bus.identify(`w${(measure.i = (measure.i ?? 0) + 1) % 20}`)); } catch { /* exhausted */ } }, 20);
measure("listAgents()", () => bus.listAgents());
measure("agentSummaries(20)", () => bus.agentSummaries(Array.from({ length: 20 }, (_, i) => `w${i}`)));
measure("messageSummaries(50)", () => bus.messageSummaries(Array.from({ length: 50 }, (_, i) => i + 1)));
measure("getTask()", () => bus.getTask(1));
measure("taskEventsFor()", () => bus.taskEventsFor("w0", 0));
measure("events(500)", () => bus.events(0, 500));

bus.db.prepare = original;
bus.close();
rmSync(home, { recursive: true, force: true });

// Router probe (pure CPU): 50 enabled agents x 50 telemetry + availability rows.
const { loadConfig } = await import(`../${buildDir}/config.js`);
const { routeTask } = await import(`../${buildDir}/router.js`);
const config = loadConfig(join(process.cwd(), "tests", "fixtures", "test-bus.config.json"));
for (const model of Object.values(config.models)) model.enabled = model.provider === "fake";
for (const provider of Object.values(config.providers)) provider.enabled = provider.id === "fake";
for (const harness of Object.values(config.harnesses)) harness.enabled = harness.id === "fake";
const small = config.agents["fake-small"];
const strong = config.agents["fake-strong"];
config.agents = {};
for (let i = 0; i < 50; i++) {
  const base = i % 2 === 0 ? small : strong;
  config.agents[`a${i}`] = { ...base, id: `a${i}`, role: "implementation", enabled: true };
}
const telemetry = Array.from({ length: 50 }, (_, i) => ({
  agentId: `a${i}`, taskCount: 10, acceptedCount: 8, failedCount: 1,
  reviewRejectedCount: 0, averageLatencyMs: 30_000, averageTokens: 20_000,
}));
const availability = Array.from({ length: 50 }, (_, i) => ({
  agentId: `a${i}`, status: i % 3 === 0 ? "working" : "idle", openTasks: i % 3,
}));
const routingTask = {
  role: "implementation", complexity: 3, contextTokens: 8_000,
  writeAccess: true, shell: true, network: false,
};
{
  const times = [];
  for (let i = 0; i < 50; i++) {
    const t0 = performance.now();
    routeTask(config, routingTask, telemetry, availability);
    times.push(performance.now() - t0);
  }
  times.sort((a, b) => a - b);
  console.log(`${"routeTask(50 agents)".padEnd(34)} median ${times[25].toFixed(3)} ms   (pure CPU)`);
}
