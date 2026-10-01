// I07: `qagent log --task N --json` prints one JSON event per line, each with top-level `authority` and `purpose`; `--why` sets the purpose.
import { check, newBus } from "./lib.mjs";

const bus = newBus();
try {
  bus.json(null, ["agent", "add", "lead", "--role", "manager", "--authority", "manager"]);
  bus.json(null, ["agent", "add", "worker", "--role", "worker"]);
  const task = bus.json("lead", ["task", "add", "Audit me", "--to", "worker"]);
  const other = bus.json("lead", ["task", "add", "Not this one", "--to", "worker"]);
  check(bus.cli("worker", ["task", "claim", String(task.id), "--why", "picked up because it is first in the queue"]).code === 0, "task claim --why failed");
  check(bus.cli("worker", ["task", "note", String(task.id), "halfway", "--why", "progress for the reviewer"]).code === 0, "task note --why failed");
  check(bus.cli("worker", ["task", "submit", String(task.id), "--summary", "done", "--why", "work is finished"]).code === 0, "task submit --why failed");
  check(bus.cli("lead", ["task", "review", String(task.id), "--accept", "--feedback", "ok", "--why", "meets acceptance"]).code === 0, "task review --why failed");
  bus.json("worker", ["task", "claim", String(other.id)]);

  const result = bus.cli(null, ["log", "--task", String(task.id), "--json"]);
  check(result.code === 0, `log --task failed: ${result.stderr.trim()}`);
  const events = result.stdout.split("\n").filter(Boolean).map((line) => JSON.parse(line));
  check(events.length >= 5, `expected at least 5 events for the task, got ${events.length}`);
  for (const event of events) {
    check(String(event.entityId) === String(task.id) && event.entity === "task", `log --task ${task.id} printed an unrelated event: ${JSON.stringify(event)}`);
    check(typeof event.authority === "string" && event.authority.length > 0, `event ${event.seq} (${event.kind}) has no authority`);
    check("purpose" in event, `event ${event.seq} (${event.kind}) has no purpose field`);
  }
  const byKind = Object.fromEntries(events.map((event) => [event.kind, event]));
  check(byKind.task_claimed.purpose === "picked up because it is first in the queue", "task_claimed lost its --why");
  check(byKind.task_accepted.purpose === "meets acceptance", "task_accepted lost its --why");
  check(byKind.task_claimed.authority === "worker" && byKind.task_accepted.authority === "manager", "authority must be the actor's authority");
  check(byKind.task_created.purpose === null, "an event without --why must carry purpose null");
} finally {
  bus.cleanup();
}
