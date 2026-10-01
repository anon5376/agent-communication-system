/**
 * Per-task git worktrees. A task with a project inside a git repository can get its own
 * checkout on branch qagent/task-<id>, under <bus home>/worktrees, so concurrent agents
 * never write into each other's files. The bus records nothing here: the path and branch
 * are derived from the task id, so any process can find or remove a task's worktree.
 * Nothing in core imports this module.
 */
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, readdirSync, realpathSync } from "node:fs";
import { basename, join, relative, resolve } from "node:path";
import type { Bus } from "./core/bus.js";
import { BusError, CLOSED_STATES, Task } from "./core/types.js";

export interface TaskWorktree {
  taskId: number;
  repoRoot: string;
  /** The worktree's root directory. */
  path: string;
  /** Where the task's project directory sits inside the worktree; the agent's working directory. */
  workdir: string;
  branch: string;
  created: boolean;
}

export interface PrunedWorktree {
  taskId: number;
  path: string;
  removed: boolean;
  reason: string;
}

type TaskRef = Pick<Task, "id" | "project">;

function git(cwd: string, args: string[]): { ok: boolean; stdout: string; stderr: string } {
  const result = spawnSync("git", args, { cwd, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] });
  if (result.error) throw new BusError("invalid", `git is not available: ${result.error.message}`);
  return { ok: result.status === 0, stdout: result.stdout.trim(), stderr: result.stderr.trim() };
}

function mustGit(cwd: string, args: string[]): string {
  const result = git(cwd, args);
  if (!result.ok) throw new BusError("conflict", `git ${args.join(" ")} failed: ${result.stderr || result.stdout}`);
  return result.stdout;
}

export function taskBranch(taskId: number): string {
  return `qagent/task-${taskId}`;
}

function repoRootFor(task: TaskRef): string {
  if (!task.project) throw new BusError("invalid", `task ${task.id} has no project directory; create it with --project DIR`);
  if (!existsSync(task.project)) throw new BusError("not_found", `task ${task.id} project directory is missing: ${task.project}`);
  const top = git(task.project, ["rev-parse", "--show-toplevel"]);
  if (!top.ok) throw new BusError("invalid", `task ${task.id} project is not inside a git repository: ${task.project}`);
  return resolve(top.stdout);
}

/** <home>/worktrees/<repo name>-<hash of repo path>/task-<id>: one directory per repository, outside it. */
export function taskWorktreePath(home: string, repoRoot: string, taskId: number): string {
  const digest = createHash("sha256").update(resolve(repoRoot)).digest("hex").slice(0, 8);
  return join(home, "worktrees", `${basename(repoRoot)}-${digest}`, `task-${taskId}`);
}

function real(path: string): string {
  try { return realpathSync(path); } catch { return resolve(path); }
}

/** Git prints resolved paths (macOS /var vs /private/var), so compare real paths. */
function registered(repoRoot: string, path: string): boolean {
  const wanted = real(path);
  const list = mustGit(repoRoot, ["worktree", "list", "--porcelain"]);
  return list.split("\n").some((line) => line.startsWith("worktree ") && real(line.slice("worktree ".length)) === wanted);
}

function describe(task: TaskRef, repoRoot: string, path: string, created: boolean): TaskWorktree {
  return { taskId: task.id, repoRoot, path, workdir: join(path, relative(repoRoot, real(task.project!))), branch: taskBranch(task.id), created };
}

/**
 * Find or create the task's worktree. A new one branches from the repository's current
 * HEAD; an existing qagent/task-<id> branch (a released and re-claimed task) is reused
 * so earlier commits carry over.
 */
export function ensureTaskWorktree(task: TaskRef, home: string): TaskWorktree {
  const repoRoot = repoRootFor(task);
  const path = taskWorktreePath(home, repoRoot, task.id);
  if (existsSync(path) && registered(repoRoot, path)) return describe(task, repoRoot, path, false);
  // A worktree directory deleted by hand leaves a stale registration that blocks `add`.
  mustGit(repoRoot, ["worktree", "prune"]);
  const branch = taskBranch(task.id);
  const hasBranch = git(repoRoot, ["rev-parse", "--verify", "--quiet", `refs/heads/${branch}`]).ok;
  mustGit(repoRoot, hasBranch ? ["worktree", "add", path, branch] : ["worktree", "add", "-b", branch, path, "HEAD"]);
  return describe(task, repoRoot, path, true);
}

/**
 * Remove the task's worktree directory. The branch stays, so committed work survives for
 * review and merge. Uncommitted changes block removal unless `force` is set.
 */
export function removeTaskWorktree(task: TaskRef, home: string, options: { force?: boolean } = {}): { path: string; branch: string; removed: boolean } {
  const repoRoot = repoRootFor(task);
  const path = taskWorktreePath(home, repoRoot, task.id);
  const branch = taskBranch(task.id);
  if (!existsSync(path) || !registered(repoRoot, path)) return { path, branch, removed: false };
  if (!options.force && mustGit(path, ["status", "--porcelain"])) {
    throw new BusError("conflict", `worktree for task ${task.id} has uncommitted changes (${path}); commit them or pass --force`);
  }
  mustGit(repoRoot, options.force ? ["worktree", "remove", "--force", path] : ["worktree", "remove", path]);
  return { path, branch, removed: true };
}

/** Remove the worktrees of accepted, failed and cancelled tasks. Dirty ones are kept unless `force`. */
export function pruneTaskWorktrees(bus: Bus, options: { force?: boolean } = {}): PrunedWorktree[] {
  const root = join(bus.home, "worktrees");
  if (!existsSync(root)) return [];
  const results: PrunedWorktree[] = [];
  for (const repoDir of readdirSync(root, { withFileTypes: true })) {
    if (!repoDir.isDirectory()) continue;
    for (const entry of readdirSync(join(root, repoDir.name), { withFileTypes: true })) {
      const match = /^task-(\d+)$/.exec(entry.name);
      if (!entry.isDirectory() || !match) continue;
      const taskId = Number(match[1]);
      const path = join(root, repoDir.name, entry.name);
      let task: Task;
      try { task = bus.getTask(taskId); } catch { results.push({ taskId, path, removed: false, reason: "task not on this bus" }); continue; }
      if (!CLOSED_STATES.includes(task.state)) { results.push({ taskId, path, removed: false, reason: `task is ${task.state}` }); continue; }
      try {
        const removed = removeTaskWorktree(task, bus.home, options);
        results.push({ taskId, path, removed: removed.removed, reason: removed.removed ? `task is ${task.state}; branch ${removed.branch} kept` : "not a registered worktree" });
      } catch (error) {
        results.push({ taskId, path, removed: false, reason: (error as Error).message });
      }
    }
  }
  return results;
}
