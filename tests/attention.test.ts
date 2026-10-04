import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { agentViews, attentionList, taskViews } from "../src/attention.js";
import { Bus } from "../src/core/bus.js";
import { STALE_AGENT_MS } from "../src/core/types.js";
import { renderPage } from "../src/dashboard/page.js";

const QAGENT = fileURLToPath(new URL("../src/qagent.js", import.meta.url));

/**
 * One bus with a task in every situation the operator view distinguishes. The old work happens
 * 90 minutes in the past (past the 60 min stall rule, inside the 2 h claim lease), the rest now.
 */
function scenario(t: { after: (fn: () => unknown) => void }) {
  const home = mkdtempSync(join(tmpdir(), "qagent-attention-"));
  t.after(() => rmSync(home, { recursive: true, force: true }));
  const dbPath = join(home, "bus.db");
  let clock = Date.now() - 90 * 60_000;
  const bus = Bus.open({ dbPath, now: () => clock });
  t.after(() => bus.close());
  bus.init();
  const operator = bus.identify("operator");
  bus.addAgent(operator, { id: "alice", role: "reviewer", authority: "manager" });
  bus.addAgent(operator, { id: "bob", role: "worker" });
  bus.addAgent(operator, { id: "carol", role: "worker" });
  bus.addAgent(operator, { id: "dave", role: "worker" });
  const alice = bus.identify("alice");
  const bob = bus.identify("bob");
  const carol = bus.identify("carol");

  const review = bus.createTask(alice, { title: "Add retry to the HTTP client", to: "bob", reviewer: "alice" });
  const failed = bus.createTask(alice, { title: "Fix the flaky parser test", to: "carol", maxRetries: 1 });
  const blocked = bus.createTask(alice, { title: "Write the migration notes", dependencies: [failed.id] });
  const stalled = bus.createTask(alice, { title: "Port the config loader", to: "carol" });
  const active = bus.createTask(alice, { title: "Refactor logging", to: "bob" });
  const queued = bus.createTask(alice, { title: "Tidy the changelog" });
  const offline = bus.createTask(alice, { title: "Bump dependencies", to: "dave" });
  const waiting = bus.createTask(alice, { title: "Ship after logging", dependencies: [active.id] });

  // 90 minutes ago: carol fails twice, then claims a task and goes quiet.
  bus.claimTask(carol, failed.id);
  bus.failTask(carol, failed.id, "parser test times out on CI");
  bus.claimTask(carol, failed.id);
  bus.failTask(carol, failed.id, "still times out after raising the limit");
  bus.claimTask(carol, stalled.id);

  // Now: bob submits one task and works on another; alice is around.
  clock = Date.now();
  bus.claimTask(bob, review.id);
  bus.submitTask(bob, review.id, { summary: "Retries with backoff; 3 new tests" });
  bus.claimTask(bob, active.id);
  bus.setStatus(alice, "idle");
  return { bus, dbPath, ids: { review: review.id, failed: failed.id, blocked: blocked.id, stalled: stalled.id, active: active.id, queued: queued.id, offline: offline.id, waiting: waiting.id } };
}

function cli(dbPath: string, args: string[]) {
  const env: NodeJS.ProcessEnv = { ...process.env, QAGENT_BUS_DB: dbPath };
  for (const name of ["QAGENT_AGENT_ID", "AGENT_ID", "QAGENT_HOME", "AGENT_BUS_HOME"]) delete env[name];
  return spawnSync(process.execPath, [QAGENT, ...args], { env, encoding: "utf8" });
}

test("attention ranks needs review, then failed or blocked, then stalled, then active, then queued, with evidence and a next command", (t) => {
  const { bus, ids } = scenario(t);
  const now = Date.now();
  const list = attentionList(taskViews(bus, now), agentViews(bus), now, STALE_AGENT_MS);
  const byId = new Map(list.map((item) => [item.task.id, item.attention]));

  assert.deepEqual(list.map((item) => item.task.id), [ids.review, ids.failed, ids.blocked, ids.stalled, ids.offline, ids.active, ids.queued, ids.waiting]);

  const review = byId.get(ids.review)!;
  assert.equal(review.label, "needs review");
  assert.match(review.reason, /waiting for review by alice/);
  assert.match(review.evidence, /Retries with backoff/);
  assert.match(review.next, new RegExp(`qagent task review ${ids.review} --accept`));

  const failed = byId.get(ids.failed)!;
  assert.equal(failed.label, "failed");
  assert.match(failed.reason, /Failed 2 time\(s\)/);
  assert.match(failed.evidence, /still times out after raising the limit/, "the latest failure note is the evidence");
  assert.equal(failed.next, `qagent trace ${ids.failed}`);

  const blocked = byId.get(ids.blocked)!;
  assert.equal(blocked.label, "blocked");
  assert.match(blocked.reason, new RegExp(`#${ids.failed} \\(failed\\)`));
  assert.match(blocked.next, new RegExp(`qagent task cancel ${ids.blocked}`));

  const stalled = byId.get(ids.stalled)!;
  assert.equal(stalled.label, "stalled");
  assert.match(stalled.evidence, /carol last seen 1 h ago/);
  assert.match(stalled.next, new RegExp(`qagent task requeue ${ids.stalled}`));

  const offline = byId.get(ids.offline)!;
  assert.equal(offline.label, "stalled");
  assert.match(offline.reason, /dave, who is offline/);
  assert.equal(offline.next, "qagent supervise dave");

  assert.equal(byId.get(ids.active)!.label, "active");
  assert.match(byId.get(ids.active)!.reason, /bob is working on it/);
  assert.equal(byId.get(ids.queued)!.label, "queued");
  assert.match(byId.get(ids.waiting)!.reason, new RegExp(`Waiting on #${ids.active} \\(claimed\\)`));
});

test("a live claim becomes stalled with time alone, without a write", (t) => {
  const { bus, ids } = scenario(t);
  const later = Date.now() + 61 * 60_000;
  const list = attentionList(taskViews(bus, later), agentViews(bus), later, STALE_AGENT_MS);
  assert.equal(list.find((item) => item.task.id === ids.active)!.attention.label, "stalled");
});

test("the dashboard leads with what needs the operator and inlines the same rules for its re-render", (t) => {
  const { bus, dbPath, ids } = scenario(t);
  const now = Date.now();
  const html = renderPage({ dbPath, seq: bus.latestSeq(), lastChangeMs: now, agents: agentViews(bus), tasks: taskViews(bus, now), messages: [] }, "n", now);
  const headings = [...html.matchAll(/<h2 id="[^"]+">([^<]+)<\/h2>/g)].map((match) => match[1]);
  assert.deepEqual(headings, ["Needs you", "Active and queued", "Agents", "Recent messages", "Send a message"]);
  assert.match(html, /5 tasks need you/);
  const attentionBody = html.slice(html.indexOf('id="attention-rows"'), html.indexOf('id="h-tasks"'));
  assert.ok(attentionBody.indexOf(`#${ids.review}<`) < attentionBody.indexOf(`#${ids.failed}<`));
  assert.match(attentionBody, /qagent task requeue/);
  assert.ok(!attentionBody.includes(`#${ids.active}<`), "active work is not in the attention list");
  assert.ok(html.includes("function attention("), "the client re-applies the attention rules");
});

test("qagent doctor lists what needs the operator; qagent trace says where the task stands", (t) => {
  const { dbPath, ids } = scenario(t);
  const doctor = cli(dbPath, ["doctor"]);
  assert.equal(doctor.status, 0, doctor.stderr);
  assert.match(doctor.stdout, /attention 5 task\(s\) need you/);
  assert.match(doctor.stdout, new RegExp(`#${ids.review} needs review: .*\\n    evidence: .*\\n    next: qagent task review ${ids.review}`));
  assert.ok(doctor.stdout.indexOf(`#${ids.review} `) < doctor.stdout.indexOf(`#${ids.stalled} `));

  const trace = cli(dbPath, ["trace", `task-${ids.stalled}`]);
  assert.equal(trace.status, 0, trace.stderr);
  assert.match(trace.stdout, /now: stalled\. Claimed by carol/);
  assert.match(trace.stdout, new RegExp(`next: qagent task requeue ${ids.stalled}`));

  const json = cli(dbPath, ["trace", String(ids.failed), "--format", "json"]);
  assert.equal(JSON.parse(json.stdout).now.label, "failed");
});
