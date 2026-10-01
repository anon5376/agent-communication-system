import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { Bus } from "../src/core/bus.js";
import { hookStatePaths, renderWake, waitForWake } from "../src/hook/claude-code.js";

const QAGENT = fileURLToPath(new URL("../src/qagent.js", import.meta.url));

type Hooks = { after: (fn: () => void | Promise<void>) => void };

function setup(t: Hooks) {
  const home = mkdtempSync(join(tmpdir(), "qagent-hook-"));
  t.after(() => rmSync(home, { recursive: true, force: true }));
  const dbPath = join(home, "bus.db");
  const bus = Bus.open({ dbPath });
  t.after(() => bus.close());
  bus.init();
  const operator = bus.identify("operator");
  bus.addAgent(operator, { id: "alice", role: "manager", authority: "manager" });
  bus.addAgent(operator, { id: "bob", role: "worker" });
  // A second connection, as another agent's process would have: the waiter's change watcher sees its commits.
  const other = Bus.open({ dbPath });
  t.after(() => other.close());
  return { home, dbPath, bus, alice: bus.identify("alice"), bob: other.identify("bob"), other };
}

function qagent(dbPath: string, args: string[]) {
  return spawnSync(process.execPath, [QAGENT, ...args], {
    encoding: "utf8",
    env: { PATH: process.env.PATH ?? "", HOME: process.env.HOME ?? "", QAGENT_BUS_DB: dbPath },
  });
}

test("hook announces unread mail once, then only mail that arrives later", async (t) => {
  const { bus, alice, bob, other } = setup(t);
  const [first] = other.send(bob, { to: "alice", subject: "one", body: "first body" });
  const [second] = other.send(bob, { to: "*", subject: "two", body: "" });
  other.send(other.identify("alice"), { to: "*", subject: "own broadcast", body: "" });

  const woke = await waitForWake(bus, alice, { timeoutMs: 1000 });
  assert.equal(woke.status, "mail");
  assert.deepEqual(woke.messages.map((message) => message.seq), [first.seq, second.seq]);
  assert.equal(woke.total, 2);
  assert.equal(Number(readFileSync(hookStatePaths(bus.home, "alice").seq, "utf8")), second.seq);
  assert.equal(bus.unreadCount("alice"), 2, "the hook only peeks");

  const again = await waitForWake(bus, alice, { timeoutMs: 50, sliceMs: 10 });
  assert.equal(again.status, "timeout", "announced mail does not wake the session twice");

  const [third] = other.send(bob, { to: "alice", subject: "three", body: "" });
  const next = await waitForWake(bus, alice, { timeoutMs: 1000 });
  assert.deepEqual(next.messages.map((message) => message.seq), [third.seq]);
});

test("hook wakes on mail sent while it waits", async (t) => {
  const { bus, alice, bob, other } = setup(t);
  const waiting = waitForWake(bus, alice, { timeoutMs: 10_000 });
  setTimeout(() => other.send(bob, { to: "alice", subject: "ready for review", body: "" }), 50);
  const result = await waiting;
  assert.equal(result.status, "mail");
  assert.equal(result.messages[0].subject, "ready for review");
});

test("a newer hook for the same agent supersedes the older one", async (t) => {
  const { bus, alice } = setup(t);
  const older = waitForWake(bus, alice, { timeoutMs: 10_000, sliceMs: 20 });
  const newer = waitForWake(bus, alice, { timeoutMs: 200, sliceMs: 20 });
  assert.equal((await older).status, "superseded");
  assert.equal((await newer).status, "timeout");
});

test("a wake-up lists ten headers, counts the rest, and announces all of them", async (t) => {
  const { bus, alice, bob, other } = setup(t);
  for (let index = 1; index <= 12; index += 1) other.send(bob, { to: "alice", subject: `line one ${index}\nline two`, body: `body ${index}` });
  const result = await waitForWake(bus, alice, { timeoutMs: 1000 });
  assert.equal(result.messages.length, 10);
  assert.equal(result.total, 12);
  const text = renderWake("alice", result);
  assert.match(text, /^qagent: 12 new messages for alice on the bus\./);
  assert.match(text, new RegExp(`#${result.messages[0].seq} bob -> alice \\[info\\] line one 1 line two\\n`));
  assert.match(text, /\.\.\. and 2 more\n/);
  assert.doesNotMatch(text, /body \d/, "bodies stay out of the reminder");
  assert.equal((await waitForWake(bus, alice, { timeoutMs: 50, sliceMs: 10 })).status, "timeout");
});

test("qagent hook claude-code exits 2 with headers on stderr, then 0 when nothing is new", (t) => {
  const { dbPath, bob, other } = setup(t);
  other.send(bob, { to: "alice", subject: "parser done", body: "details" });
  const woke = qagent(dbPath, ["--as", "alice", "hook", "claude-code", "--timeout", "5"]);
  assert.equal(woke.status, 2, woke.stderr);
  assert.equal(woke.stdout, "");
  assert.match(woke.stderr, /bob -> alice \[info\] parser done/);
  const quiet = qagent(dbPath, ["--as", "alice", "hook", "claude-code", "--timeout", "1"]);
  assert.equal(quiet.status, 0, quiet.stderr);
  assert.equal(quiet.stderr, "");
});

test("qagent hook claude-code --settings prints an asyncRewake Stop hook for the agent", (t) => {
  const { dbPath } = setup(t);
  const result = qagent(dbPath, ["--as", "alice", "hook", "claude-code", "--settings", "--timeout", "600"]);
  assert.equal(result.status, 0, result.stderr);
  const hook = JSON.parse(result.stdout).hooks.Stop[0].hooks[0];
  assert.equal(hook.type, "command");
  assert.equal(hook.asyncRewake, true);
  assert.equal(hook.timeout, 660);
  assert.ok(hook.command.includes(`--as alice --db ${dbPath} hook claude-code --timeout 600`), hook.command);
  assert.equal(qagent(dbPath, ["--as", "alice", "hook", "codex"]).status, 1);
});
