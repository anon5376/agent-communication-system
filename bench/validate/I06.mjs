// I06: a worker (canDelegate=false) cannot create tasks; managers and the operator can.
import { check, newBus } from "./lib.mjs";

const bus = newBus();
try {
  bus.json(null, ["agent", "add", "lead", "--role", "manager", "--authority", "manager"]);
  bus.json(null, ["agent", "add", "worker", "--role", "worker"]);
  bus.json(null, ["agent", "add", "other", "--role", "worker"]);
  const refused = bus.cli("worker", ["task", "add", "Delegate this", "--to", "other"]);
  check(refused.code !== 0, "a worker created a task for another agent");
  check(/canDelegate/.test(refused.stderr), `the refusal must name canDelegate: ${refused.stderr.trim()}`);
  check(refused.code === 3, `forbidden maps to exit 3, got ${refused.code}`);
  check(bus.cli("worker", ["task", "add", "For nobody"]).code !== 0, "a worker created an unassigned task");
  check(bus.cli("lead", ["task", "add", "Manager task", "--to", "worker"]).code === 0, "a manager must still create tasks");
  check(bus.cli(null, ["task", "add", "Operator task", "--to", "worker"]).code === 0, "the operator must still create tasks");
  const tasks = bus.json(null, ["task", "list", "--all"]);
  check(tasks.length === 2, `expected 2 tasks, found ${tasks.length}`);
} finally {
  bus.cleanup();
}
