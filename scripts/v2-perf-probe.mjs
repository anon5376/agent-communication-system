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
measure("getTask()", () => bus.getTask(1));
measure("taskEventsFor()", () => bus.taskEventsFor("w0", 0));
measure("events(500)", () => bus.events(0, 500));

bus.db.prepare = original;
bus.close();
rmSync(home, { recursive: true, force: true });
