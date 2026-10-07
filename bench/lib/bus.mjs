// Read-only view of a bus.db, written by either the TypeScript or the Rust implementation.
import { existsSync, readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { taskKey } from "./tasks.mjs";
import { BenchError } from "./util.mjs";

function parse(text, fallback) {
  try { return text ? JSON.parse(text) : fallback; } catch { return fallback; }
}

export function readBus(dbPath) {
  if (!existsSync(dbPath)) throw new BenchError(`bus database not found: ${dbPath}`);
  const db = new DatabaseSync(dbPath, { readOnly: true, timeout: 5000 });
  try {
    const tasks = db.prepare("SELECT * FROM tasks ORDER BY id").all().map((row) => ({
      id: row.id,
      key: taskKey(row.title),
      title: row.title,
      brief: row.brief,
      acceptance: row.acceptance,
      role: row.role,
      state: row.state,
      creator: row.creator,
      assignee: row.assignee,
      reviewer: row.reviewer,
      round: row.round,
      attempts: row.attempts,
      createdMs: row.created_ms,
      updatedMs: row.updated_ms,
      result: parse(row.result_json, null),
      review: parse(row.review_json, null),
    }));
    const events = db.prepare("SELECT * FROM events ORDER BY seq").all().map((row) => ({
      seq: row.seq, tsMs: row.ts_ms, actor: row.actor, kind: row.kind, entity: row.entity, entityId: row.entity_id, data: parse(row.data_json, {}),
    }));
    const messages = Number(db.prepare("SELECT count(*) AS n FROM messages").get().n);
    const usage = db.prepare("SELECT agent_id, turns, input_tokens, output_tokens, cost_usd, latency_ms FROM usage").all();
    return { tasks, events, messages, usage };
  } finally {
    db.close();
  }
}

/** Per-agent session totals the supervisor keeps in <home>/sessions/<agent>.json. */
export function readSessions(home) {
  const dir = join(home, "sessions");
  const sessions = {};
  if (!existsSync(dir)) return sessions;
  for (const name of readdirSync(dir)) {
    if (!name.endsWith(".json")) continue;
    const row = parse(readFileSync(join(dir, name), "utf8"), null);
    if (row) sessions[name.slice(0, -5)] = row;
  }
  return sessions;
}
