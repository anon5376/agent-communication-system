// I05: claimTask refuses a fifth held task for one agent (limit 4) with a `conflict` that names maxConcurrentTasks.
import { check, newBus } from "./lib.mjs";

const bus = newBus();
try {
  bus.json(null, ["agent", "add", "lead", "--role", "manager", "--authority", "manager"]);
  bus.json(null, ["agent", "add", "worker", "--role", "worker"]);
  const ids = [1, 2, 3, 4, 5].map((n) => bus.json("lead", ["task", "add", `Task ${n}`, "--to", "worker"]).id);
  for (const id of ids.slice(0, 4)) check(bus.cli("worker", ["task", "claim", String(id)]).code === 0, `claim of #${id} within the limit must succeed`);
  const fifth = bus.cli("worker", ["task", "claim", String(ids[4])]);
  check(fifth.code !== 0, "the fifth concurrent claim succeeded");
  check(/\b4\b/.test(fifth.stderr), `the refusal must state the limit of 4: ${fifth.stderr.trim()}`);
  check(/maxConcurrentTasks/.test(fifth.stderr), `the refusal must name maxConcurrentTasks: ${fifth.stderr.trim()}`);
  const state = bus.json(null, ["task", "show", String(ids[4])]);
  check(state.state === "open" && state.assignee === "worker", "the refused task must stay open");
  check(bus.cli("worker", ["task", "submit", String(ids[0]), "--summary", "done"]).code === 0, "submit of a held task must work");
  check(bus.cli("worker", ["task", "claim", String(ids[4])]).code === 0, "after one task is submitted the fifth claim must succeed");
} finally {
  bus.cleanup();
}
