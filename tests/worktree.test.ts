import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { Bus } from "../src/core/bus.js";
import { ensureTaskWorktree, pruneTaskWorktrees, removeTaskWorktree, taskWorktreePath } from "../src/worktree.js";

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

test("a task worktree is a separate checkout on qagent/task-<id>, reused while it exists", (t) => {
  const f = fixture(t);
  const task = f.bus.createTask(f.operator, { title: "edit a", project: join(f.repo, "pkg") });
  const first = ensureTaskWorktree(task, f.bus.home);
  assert.equal(first.created, true);
  assert.equal(first.branch, `qagent/task-${task.id}`);
  assert.equal(first.path, taskWorktreePath(f.bus.home, f.repo, task.id));
  assert.equal(first.workdir, join(first.path, "pkg"));
  assert.ok(existsSync(join(first.workdir, "a.txt")));
  assert.equal(git(first.path, ["rev-parse", "--abbrev-ref", "HEAD"]), first.branch);
  // The main checkout is untouched by work in the worktree.
  writeFileSync(join(first.workdir, "a.txt"), "two\n");
  assert.equal(git(f.repo, ["status", "--porcelain"]), "");

  const again = ensureTaskWorktree(task, f.bus.home);
  assert.equal(again.created, false);
  assert.equal(again.path, first.path);
});

test("removal keeps the branch, refuses uncommitted changes without force, and a re-claim resumes the branch", (t) => {
  const f = fixture(t);
  const task = f.bus.createTask(f.operator, { title: "edit a", project: f.repo });
  const wt = ensureTaskWorktree(task, f.bus.home);
  writeFileSync(join(wt.path, "pkg", "a.txt"), "committed\n");
  git(wt.path, ["commit", "-q", "-am", "work"]);
  writeFileSync(join(wt.path, "scratch.txt"), "dirty\n");

  assert.throws(() => removeTaskWorktree(task, f.bus.home), /uncommitted changes/);
  assert.ok(existsSync(wt.path));
  assert.equal(removeTaskWorktree(task, f.bus.home, { force: true }).removed, true);
  assert.ok(!existsSync(wt.path));
  assert.equal(removeTaskWorktree(task, f.bus.home).removed, false);

  const back = ensureTaskWorktree(task, f.bus.home);
  assert.equal(back.created, true);
  assert.equal(git(back.path, ["log", "-1", "--format=%s"]), "work");
});

test("tasks without a git project get a clear error", (t) => {
  const f = fixture(t);
  const noProject = f.bus.createTask(f.operator, { title: "no project" });
  assert.throws(() => ensureTaskWorktree(noProject, f.bus.home), /has no project directory/);
  const plain = join(f.home, "plain");
  mkdirSync(plain);
  const notGit = f.bus.createTask(f.operator, { title: "not git", project: plain });
  // A temp dir could sit inside some outer repository; only assert when it does not.
  if (spawnSync("git", ["rev-parse"], { cwd: plain }).status !== 0) {
    assert.throws(() => ensureTaskWorktree(notGit, f.bus.home), /not inside a git repository/);
  }
});

test("prune removes worktrees of closed tasks only", (t) => {
  const f = fixture(t);
  const open = f.bus.createTask(f.operator, { title: "still open", project: f.repo });
  const done = f.bus.createTask(f.operator, { title: "cancelled", project: f.repo });
  const openTree = ensureTaskWorktree(open, f.bus.home);
  const doneTree = ensureTaskWorktree(done, f.bus.home);
  f.bus.cancelTask(f.operator, done.id, "not needed");

  const results = pruneTaskWorktrees(f.bus);
  assert.deepEqual(results.map((r) => [r.taskId, r.removed]).sort(), [[open.id, false], [done.id, true]].sort());
  assert.ok(existsSync(openTree.path));
  assert.ok(!existsSync(doneTree.path));
  assert.ok(git(f.repo, ["branch", "--list", doneTree.branch]).includes(doneTree.branch));
});

test("`qagent task claim --worktree` claims and prints the checkout; `task worktree --remove` cleans it up", (t) => {
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
  assert.equal(claimed.worktree.branch, `qagent/task-${task.id}`);
  assert.ok(existsSync(join(claimed.worktree.workdir, "pkg", "a.txt")));

  const shown = cli(["task", "worktree", String(task.id)]);
  assert.equal(shown.created, false);
  assert.equal(shown.path, claimed.worktree.path);
  const removed = cli(["task", "worktree", String(task.id), "--remove"]);
  assert.equal(removed.removed, true);
  assert.ok(!existsSync(claimed.worktree.path));
});
