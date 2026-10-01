import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, realpathSync, rmSync, utimesSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { Bus } from "../src/core/bus.js";
import { ensureTaskWorktree, pruneTaskWorktrees, removeTaskWorktree, taskBranch, taskWorktreePath } from "../src/worktree.js";

const QAGENT = fileURLToPath(new URL("../src/qagent.js", import.meta.url));

function git(cwd: string, args: string[]): string {
  const result = spawnSync("git", ["-c", "user.email=test@example.com", "-c", "user.name=test", ...args], { cwd, encoding: "utf8" });
  assert.equal(result.status, 0, `git ${args.join(" ")}: ${result.stderr}`);
  return result.stdout.trim();
}

function fixture(t: { after: (fn: () => void) => void }) {
  const home = realpathSync(mkdtempSync(join(tmpdir(), "qagent-worktree-")));
  const repo = join(home, "repo");
  mkdirSync(join(repo, "pkg"), { recursive: true });
  git(repo, ["init", "-q", "-b", "main"]);
  writeFileSync(join(repo, "pkg", "a.txt"), "one\n");
  git(repo, ["add", "."]);
  git(repo, ["commit", "-q", "-m", "init"]);
  const dbPath = join(home, "bus", "bus.db");
  const bus = Bus.open({ dbPath });
  bus.init();
  const operator = bus.identify("operator");
  t.after(() => { bus.close(); rmSync(home, { recursive: true, force: true }); });
  return { home, repo, dbPath, bus, operator };
}

test("a task worktree is a separate checkout on qagent/task-<id>, reused while it exists", async (t) => {
  const f = fixture(t);
  const task = f.bus.createTask(f.operator, { title: "edit a", project: join(f.repo, "pkg") });
  const first = await ensureTaskWorktree(task, f.bus.home);
  assert.equal(first.created, true);
  assert.equal(first.branch, taskBranch(task));
  assert.equal(first.path, taskWorktreePath(f.bus.home, realpathSync(f.repo), task));
  assert.equal(first.workdir, join(first.path, "pkg"));
  assert.ok(existsSync(join(first.workdir, "a.txt")));
  assert.equal(git(first.path, ["rev-parse", "--abbrev-ref", "HEAD"]), first.branch);
  // The main checkout is untouched by work in the worktree.
  writeFileSync(join(first.workdir, "a.txt"), "two\n");
  assert.equal(git(f.repo, ["status", "--porcelain"]), "");

  const again = await ensureTaskWorktree(task, f.bus.home);
  assert.equal(again.created, false);
  assert.equal(again.path, first.path);
});

test("removal keeps the branch, refuses uncommitted changes without force, and a re-claim resumes the branch", async (t) => {
  const f = fixture(t);
  const task = f.bus.createTask(f.operator, { title: "edit a", project: f.repo });
  const wt = await ensureTaskWorktree(task, f.bus.home);
  writeFileSync(join(wt.path, "pkg", "a.txt"), "committed\n");
  git(wt.path, ["commit", "-q", "-am", "work"]);
  writeFileSync(join(wt.path, "scratch.txt"), "dirty\n");

  await assert.rejects(removeTaskWorktree(task, f.bus.home), /uncommitted changes/);
  assert.ok(existsSync(wt.path));
  assert.equal((await removeTaskWorktree(task, f.bus.home, { force: true })).removed, true);
  assert.ok(!existsSync(wt.path));
  assert.equal((await removeTaskWorktree(task, f.bus.home)).removed, false);

  const back = await ensureTaskWorktree(task, f.bus.home);
  assert.equal(back.created, true);
  assert.equal(git(back.path, ["log", "-1", "--format=%s"]), "work");
});

test("tasks without a git project get a clear error", async (t) => {
  const f = fixture(t);
  const noProject = f.bus.createTask(f.operator, { title: "no project" });
  await assert.rejects(ensureTaskWorktree(noProject, f.bus.home), /has no project directory/);
  const plain = join(f.home, "plain");
  mkdirSync(plain);
  const notGit = f.bus.createTask(f.operator, { title: "not git", project: plain });
  // A temp dir could sit inside some outer repository; only assert when it does not.
  if (spawnSync("git", ["rev-parse"], { cwd: plain }).status !== 0) {
    await assert.rejects(ensureTaskWorktree(notGit, f.bus.home), /not inside a git repository/);
  }
});

test("prune removes worktrees of closed tasks only", async (t) => {
  const f = fixture(t);
  const open = f.bus.createTask(f.operator, { title: "still open", project: f.repo });
  const done = f.bus.createTask(f.operator, { title: "cancelled", project: f.repo });
  const openTree = await ensureTaskWorktree(open, f.bus.home);
  const doneTree = await ensureTaskWorktree(done, f.bus.home);
  f.bus.cancelTask(f.operator, done.id, "not needed");

  const results = await pruneTaskWorktrees(f.bus);
  assert.deepEqual(results.map((r) => [r.taskId, r.removed]).sort(), [[open.id, false], [done.id, true]].sort());
  assert.ok(existsSync(openTree.path));
  assert.ok(!existsSync(doneTree.path));
  assert.ok(git(f.repo, ["branch", "--list", doneTree.branch]).includes(doneTree.branch));
});

test("`qagent task claim --worktree` claims and prints the checkout; `task worktree --remove` cleans it up", async (t) => {
  const f = fixture(t);
  const env: NodeJS.ProcessEnv = { ...process.env, QAGENT_BUS_DB: f.dbPath };
  for (const name of ["QAGENT_AGENT_ID", "AGENT_ID", "QAGENT_HOME", "AGENT_BUS_HOME"]) delete env[name];
  const cli = (args: string[]) => {
    const result = spawnSync(process.execPath, [QAGENT, ...args, "--json"], { env, encoding: "utf8" });
    assert.equal(result.status, 0, `qagent ${args.join(" ")}: ${result.stderr}`);
    return JSON.parse(result.stdout);
  };
  cli(["agent", "add", "w1", "--role", "implementation"]);
  const task = cli(["--as", "operator", "task", "add", "edit", "--to", "w1", "--project", f.repo]);
  const claimed = cli(["--as", "w1", "task", "claim", String(task.id), "--worktree"]);
  assert.equal(claimed.state, "claimed");
  assert.equal(claimed.worktree.branch, taskBranch(task));
  assert.ok(existsSync(join(claimed.worktree.workdir, "pkg", "a.txt")));

  const shown = cli(["task", "worktree", String(task.id)]);
  assert.equal(shown.created, false);
  assert.equal(shown.path, claimed.worktree.path);
  const removed = cli(["task", "worktree", String(task.id), "--remove"]);
  assert.equal(removed.removed, true);
  assert.ok(!existsSync(claimed.worktree.path));
});

test("a branch from another bus's task with the same id is never reused", async (t) => {
  const f = fixture(t);
  const first = f.bus.createTask(f.operator, { title: "one", project: f.repo });
  const a = await ensureTaskWorktree(first, f.bus.home);
  git(a.path, ["commit", "-q", "--allow-empty", "-m", "stale work"]);

  // A second bus (separate home) restarts ids at 1 and works on the same repository.
  const other = Bus.open({ dbPath: join(f.home, "bus2", "bus.db" ) });
  t.after(() => other.close());
  other.init();
  const twin = other.createTask(other.identify("operator"), { title: "twin", project: f.repo });
  assert.equal(twin.id, first.id);
  const b = await ensureTaskWorktree({ ...twin, createdMs: first.createdMs + 1 }, other.home);
  assert.notEqual(b.branch, a.branch);
  assert.equal(git(b.path, ["log", "-1", "--format=%s"]), "init");
});

test("concurrent creation of one task's worktree yields one checkout and no errors", async (t) => {
  const f = fixture(t);
  const task = f.bus.createTask(f.operator, { title: "race", project: f.repo });
  const trees = await Promise.all(Array.from({ length: 6 }, () => ensureTaskWorktree(task, f.bus.home)));
  assert.equal(new Set(trees.map((tree) => tree.path)).size, 1);
  assert.equal(trees.filter((tree) => tree.created).length, 1);
});

test("a project directory that is not tracked in git is refused and leaves no checkout behind", async (t) => {
  const f = fixture(t);
  const fresh = join(f.repo, "newpkg");
  mkdirSync(fresh);
  writeFileSync(join(fresh, "x.txt"), "untracked\n");
  const task = f.bus.createTask(f.operator, { title: "untracked", project: fresh });
  await assert.rejects(ensureTaskWorktree(task, f.bus.home), /not tracked in git/);
  assert.ok(!existsSync(taskWorktreePath(f.bus.home, realpathSync(f.repo), task)));
});

test("harness config files do not make a worktree dirty", async (t) => {
  const f = fixture(t);
  const task = f.bus.createTask(f.operator, { title: "adapters", project: f.repo });
  const wt = await ensureTaskWorktree(task, f.bus.home);
  mkdirSync(join(wt.path, ".cursor"));
  writeFileSync(join(wt.path, ".cursor", "mcp.json"), "{}");
  writeFileSync(join(wt.path, "opencode.json"), "{}");
  mkdirSync(join(wt.path, ".agent-bus"));
  writeFileSync(join(wt.path, ".agent-bus", "x"), "x");
  assert.equal(git(wt.path, ["status", "--porcelain"]), "");
  assert.equal((await removeTaskWorktree(task, f.bus.home)).removed, true);
});

test("an unregistered directory at the worktree path gets a specific error", async (t) => {
  const f = fixture(t);
  const task = f.bus.createTask(f.operator, { title: "leftover", project: f.repo });
  mkdirSync(taskWorktreePath(f.bus.home, realpathSync(f.repo), task), { recursive: true });
  await assert.rejects(ensureTaskWorktree(task, f.bus.home), /not a git worktree/);
});

test("creating a worktree leaves other worktrees' registrations alone", async (t) => {
  const f = fixture(t);
  const elsewhere = join(f.home, "elsewhere");
  git(f.repo, ["worktree", "add", "-q", "-b", "other", elsewhere]);
  const task = f.bus.createTask(f.operator, { title: "mine", project: f.repo });
  await ensureTaskWorktree(task, f.bus.home);
  assert.ok(git(f.repo, ["worktree", "list"]).includes("elsewhere"));
});

test("`task claim --worktree` on a non-git project: explicit id leaves the task unclaimed, bare claim keeps the claim with a warning", (t) => {
  const f = fixture(t);
  const env: NodeJS.ProcessEnv = { ...process.env, QAGENT_BUS_DB: f.dbPath };
  for (const name of ["QAGENT_AGENT_ID", "AGENT_ID", "QAGENT_HOME", "AGENT_BUS_HOME"]) delete env[name];
  const run = (args: string[]) => spawnSync(process.execPath, [QAGENT, ...args, "--json"], { env, encoding: "utf8" });
  const plain = join(f.home, "plain-not-git");
  mkdirSync(plain);
  if (spawnSync("git", ["rev-parse"], { cwd: plain }).status === 0) return; // temp dir sits inside a repository
  assert.equal(run(["agent", "add", "w1", "--role", "implementation"]).status, 0);
  const explicit = JSON.parse(run(["--as", "operator", "task", "add", "a", "--to", "w1", "--project", plain]).stdout);
  const refused = run(["--as", "w1", "task", "claim", String(explicit.id), "--worktree"]);
  assert.notEqual(refused.status, 0);
  assert.equal(f.bus.getTask(explicit.id).state, "open");

  const bare = JSON.parse(run(["--as", "operator", "task", "add", "b", "--to", "w1", "--project", plain]).stdout);
  const claimed = run(["--as", "w1", "task", "claim", "--worktree"]);
  assert.equal(claimed.status, 0, claimed.stderr);
  const out = JSON.parse(claimed.stdout);
  assert.equal(out.worktree, null);
  assert.match(out.worktreeError, /not inside a git repository/);
  assert.equal(f.bus.getTask(out.id).state, "claimed");
  void bare;
});

test("only the assignee or the operator may open or remove a task's worktree; --force is operator-only", async (t) => {
  const f = fixture(t);
  const env: NodeJS.ProcessEnv = { ...process.env, QAGENT_BUS_DB: f.dbPath };
  for (const name of ["QAGENT_AGENT_ID", "AGENT_ID", "QAGENT_HOME", "AGENT_BUS_HOME"]) delete env[name];
  const run = (args: string[]) => spawnSync(process.execPath, [QAGENT, ...args, "--json"], { env, encoding: "utf8" });
  for (const id of ["w1", "w2"]) assert.equal(run(["agent", "add", id, "--role", "implementation"]).status, 0);
  const task = JSON.parse(run(["--as", "operator", "task", "add", "t", "--to", "w1", "--project", f.repo]).stdout);
  assert.notEqual(run(["--as", "w2", "task", "worktree", String(task.id)]).status, 0);
  assert.equal(run(["--as", "w1", "task", "worktree", String(task.id)]).status, 0);
  assert.notEqual(run(["--as", "w2", "task", "worktree", String(task.id), "--remove"]).status, 0);
  assert.notEqual(run(["--as", "w1", "task", "worktree", String(task.id), "--remove", "--force"]).status, 0);
  assert.equal(run(["--as", "w1", "task", "worktree", String(task.id), "--remove"]).status, 0);
  assert.notEqual(run(["--as", "w1", "task", "worktree", "prune", "--force"]).status, 0);
});

/** The lock directory worktree creation uses for this task's repository. */
function lockDirFor(f: { home: string; repo: string; bus: Bus }, task: { id: number; createdMs: number }): string {
  return join(dirname(taskWorktreePath(f.bus.home, realpathSync(f.repo), task)), ".lock");
}

function backdate(path: string, seconds: number): void {
  const when = new Date(Date.now() - seconds * 1000);
  utimesSync(path, when, when);
}

const within = <T>(label: string, ms: number, work: Promise<T>): Promise<T> =>
  Promise.race([work, new Promise<never>((_, reject) => setTimeout(() => reject(new Error(`${label}: still blocked after ${ms} ms`)), ms))]);

test("a lock left by a holder that died before writing its owner file is swept", async (t) => {
  const f = fixture(t);
  const task = f.bus.createTask(f.operator, { title: "first", project: f.repo });
  await ensureTaskWorktree(task, f.bus.home);
  const lock = lockDirFor(f, task);
  mkdirSync(lock);
  backdate(lock, 120);
  const next = f.bus.createTask(f.operator, { title: "second", project: f.repo });
  assert.equal((await within("no-owner lock", 10_000, ensureTaskWorktree(next, f.bus.home))).created, true);
  assert.ok(!existsSync(lock));
});

test("a lock whose owner process is dead is swept at once, even while fresh", async (t) => {
  const f = fixture(t);
  const task = f.bus.createTask(f.operator, { title: "first", project: f.repo });
  await ensureTaskWorktree(task, f.bus.home);
  const gone = spawnSync(process.execPath, ["-e", "process.stdout.write(String(process.pid))"], { encoding: "utf8" });
  const lock = lockDirFor(f, task);
  mkdirSync(lock);
  writeFileSync(join(lock, "owner"), `${gone.stdout.trim()}:deadbeef\n`);
  const next = f.bus.createTask(f.operator, { title: "second", project: f.repo });
  assert.equal((await within("dead-owner lock", 10_000, ensureTaskWorktree(next, f.bus.home))).created, true);
});

test("a lock owned by a live pid that stopped heartbeating (pid reuse) is swept; a fresh one is respected", async (t) => {
  const f = fixture(t);
  const task = f.bus.createTask(f.operator, { title: "first", project: f.repo });
  await ensureTaskWorktree(task, f.bus.home);
  const lock = lockDirFor(f, task);
  mkdirSync(lock);
  writeFileSync(join(lock, "owner"), `${process.pid}:someone-else\n`);

  // Fresh heartbeat: the contender must wait, not steal.
  const next = f.bus.createTask(f.operator, { title: "second", project: f.repo });
  let finished = false;
  const pending = ensureTaskWorktree(next, f.bus.home).then((tree) => { finished = true; return tree; });
  await new Promise((resolve) => setTimeout(resolve, 400));
  assert.equal(finished, false, "took a lock whose owner is alive and heartbeating");

  // The heartbeat stops: now it is stale.
  backdate(lock, 120);
  assert.equal((await within("stale heartbeat", 10_000, pending)).created, true);
});

test("cleanup works after the task's project directory was deleted", async (t) => {
  const f = fixture(t);
  const sub = join(f.repo, "pkg2");
  mkdirSync(sub);
  writeFileSync(join(sub, "b.txt"), "y\n");
  git(f.repo, ["add", "."]);
  git(f.repo, ["commit", "-q", "-m", "pkg2"]);
  const task = f.bus.createTask(f.operator, { title: "gone dir", project: sub });
  const wt = await ensureTaskWorktree(task, f.bus.home);
  rmSync(sub, { recursive: true, force: true });
  assert.equal((await removeTaskWorktree(task, f.bus.home)).removed, true);
  assert.ok(!existsSync(wt.path));
  assert.ok(!git(f.repo, ["worktree", "list"]).includes(wt.path));
});

test("cleanup after the whole repository was deleted needs --force, then removes the orphan; prune reports it", async (t) => {
  const f = fixture(t);
  const task = f.bus.createTask(f.operator, { title: "gone repo", project: f.repo });
  const wt = await ensureTaskWorktree(task, f.bus.home);
  f.bus.cancelTask(f.operator, task.id, "repo deleted");
  rmSync(f.repo, { recursive: true, force: true });

  await assert.rejects(removeTaskWorktree(task, f.bus.home), /repository .* is gone.*--force/);
  const kept = await pruneTaskWorktrees(f.bus);
  assert.deepEqual(kept.map((r) => [r.taskId, r.removed]), [[task.id, false]]);
  assert.match(kept[0].reason, /is gone/);
  assert.ok(existsSync(wt.path));

  assert.equal((await removeTaskWorktree(task, f.bus.home, { force: true })).removed, true);
  assert.ok(!existsSync(wt.path));
});
