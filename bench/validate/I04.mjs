// I04: a supervised turn writes the `usage` table (one row per agent and day, with the turn's tokens).
import { DatabaseSync } from "node:sqlite";
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { QAGENT, check, fixtureConfig, killGroup, newBus, spawnChild, until } from "./lib.mjs";

const bus = newBus();
let supervisor;
try {
  bus.json(null, ["agent", "add", "fake-small", "--role", "cheap-worker", "--harness", "fake"]);
  const project = join(bus.home, "project");
  mkdirSync(project);
  const configPath = join(bus.home, "config.json");
  writeFileSync(configPath, JSON.stringify(fixtureConfig((config) => { config.agents["fake-small"].harnessOptions = { mode: "success" }; })));
  supervisor = spawnChild(process.execPath, [QAGENT, "supervise", "fake-small", project, "--config", configPath], { env: bus.env() });
  await until("the supervisor to hold its wait", 20_000, () => bus.json(null, ["agent", "list"]).find((agent) => agent.id === "fake-small")?.status === "waiting");
  const task = bus.json(null, ["task", "add", "Do a turn", "--to", "fake-small", "--brief", "anything"]);
  await until("the task to be submitted", 30_000, () => bus.json(null, ["task", "show", String(task.id)]).state === "submitted");
  const db = new DatabaseSync(bus.dbPath, { readOnly: true });
  const rows = db.prepare("SELECT agent_id, day, turns, input_tokens, output_tokens FROM usage").all();
  db.close();
  check(rows.length === 1, `expected one usage row after one turn, found ${rows.length}`);
  check(rows[0].agent_id === "fake-small" && rows[0].turns === 1, `usage row is wrong: ${JSON.stringify(rows[0])}`);
  check(rows[0].input_tokens > 0 && rows[0].output_tokens > 0, `usage row has no tokens: ${JSON.stringify(rows[0])}`);
  check(/^\d{4}-\d{2}-\d{2}$/.test(rows[0].day), `usage.day should be YYYY-MM-DD, got ${rows[0].day}`);
} finally {
  killGroup(supervisor);
  bus.cleanup();
}
