// I10: `qagent status --json` reports every open task, with a true `openTaskCount`; 250 open tasks are not cut to 200.
import { DatabaseSync } from "node:sqlite";
import { check, newBus } from "./lib.mjs";

const bus = newBus();
try {
  bus.json(null, ["agent", "add", "lead", "--role", "manager", "--authority", "manager"]);
  bus.json("lead", ["task", "add", "Seed", "--to", "lead"]);
  // Fill the rest straight into the table: 249 more rows are far quicker than 249 process starts.
  const db = new DatabaseSync(bus.dbPath);
  const insert = db.prepare("INSERT INTO tasks(title, state, creator, created_ms, updated_ms) VALUES(?, 'open', 'lead', ?, ?)");
  const now = Date.now();
  for (let n = 2; n <= 250; n += 1) insert.run(`Task ${n}`, now + n, now + n);
  db.close();
  const status = bus.json(null, ["status"]);
  check(status.openTaskCount === 250, `openTaskCount should be 250, got ${status.openTaskCount}`);
  check(Array.isArray(status.openTasks) && status.openTasks.length === 250, `openTasks should list all 250, got ${status.openTasks?.length}`);
  const text = bus.cli(null, ["status"]);
  check(text.code === 0 && /250/.test(text.stdout), "the text form of status must show the true open-task count");
} finally {
  bus.cleanup();
}
