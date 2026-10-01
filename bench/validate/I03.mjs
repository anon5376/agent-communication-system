// I03: `qagent status --json` lists stalled claims (`stalled`) and the operator's unread count (`operatorUnread`).
import { DatabaseSync } from "node:sqlite";
import { check, newBus } from "./lib.mjs";

const bus = newBus();
try {
  bus.json(null, ["agent", "add", "lead", "--role", "manager", "--authority", "manager"]);
  bus.json(null, ["agent", "add", "worker", "--role", "worker"]);
  const task = bus.json("lead", ["task", "add", "Stall me", "--to", "worker"]);
  bus.json("worker", ["task", "claim", String(task.id)]);
  const fresh = bus.json(null, ["status"]);
  check(Array.isArray(fresh.stalled), "status --json has no `stalled` array");
  check(fresh.stalled.length === 0, "a claim that just started must not be listed as stalled");
  check(Number.isInteger(fresh.operatorUnread), "status --json has no integer `operatorUnread`");

  // Age the claim by three hours: its last activity is then far in the past.
  const db = new DatabaseSync(bus.dbPath);
  const past = Date.now() - 3 * 3_600_000;
  db.prepare("UPDATE events SET ts_ms = ? WHERE entity = 'task' AND entity_id = ?").run(past, String(task.id));
  db.prepare("UPDATE tasks SET updated_ms = ? WHERE id = ?").run(past, task.id);
  db.close();
  bus.json("worker", ["send", "operator", "need input", "the parser needs a decision"]);
  bus.json("worker", ["send", "operator", "second", "another question"]);

  const status = bus.json(null, ["status"]);
  check(status.stalled.map((entry) => entry.id).includes(task.id), `the stalled claim #${task.id} is not listed: ${JSON.stringify(status.stalled)}`);
  check(status.operatorUnread === 2, `operatorUnread should be 2, got ${status.operatorUnread}`);
  const text = bus.cli(null, ["status"]);
  const stalledLine = text.stdout.split("\n").find((line) => /stalled/i.test(line)) ?? "";
  check(text.code === 0 && new RegExp(`#?${task.id}\\b`).test(stalledLine) && /\b2\b/.test(text.stdout), "the text form of status must list the stalled ids and the operator unread count");
} finally {
  bus.cleanup();
}
