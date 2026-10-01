/**
 * Per-task git worktrees. A task with a project inside a git repository can get its own
 * checkout under <bus home>/worktrees, on a branch named for the task, so concurrent agents
 * never write into each other's files. The bus records nothing here: the path and branch are
 * derived from the task (id and creation time), so any process can find or remove a task's
 * worktree, and a task on another bus whose id happens to match never shares a branch.
 * Everything is async so a supervisor running a whole roster never blocks on a large checkout.
 * Nothing in core imports this module.
 */
import { execFile } from "node:child_process";
import { createHash, randomBytes } from "node:crypto";
import { appendFileSync, existsSync, mkdirSync, readFileSync, readdirSync, realpathSync, renameSync, rmSync, statSync, utimesSync, writeFileSync } from "node:fs";
import { basename, dirname, isAbsolute, join, relative, resolve, sep } from "node:path";
import { BusError, CLOSED_STATES } from "./core/types.js";
/** Files that harness adapters write into their working directory; kept out of `git status`. */
const ADAPTER_FILES = [".cursor/mcp.json", "opencode.json", ".agent-bus/", ".qagent/"];
function git(cwd, args) {
    return new Promise((resolveGit, reject) => {
        execFile("git", args, { cwd, encoding: "utf8", maxBuffer: 16 * 1024 * 1024 }, (error, stdout, stderr) => {
            const code = error?.code;
            if (error && (code === "ENOENT" || code === "EACCES")) {
                reject(new BusError("invalid", `git is not available: ${error.message}`));
                return;
            }
            resolveGit({ ok: !error, stdout: String(stdout).trim(), stderr: String(stderr).trim() });
        });
    });
}
async function mustGit(cwd, args) {
    const result = await git(cwd, args);
    if (!result.ok)
        throw new BusError("conflict", `git ${args.join(" ")} failed: ${result.stderr || result.stdout}`);
    return result.stdout;
}
function real(path) {
    try {
        return realpathSync.native(path);
    }
    catch {
        return resolve(path);
    }
}
/** Unique per task, not per id: ids restart on a new bus, creation times do not repeat. */
export function taskBranch(task) {
    return `qagent/task-${task.id}-${task.createdMs.toString(36)}`;
}
/** The repository root for the task's project, or a BusError saying why it has none. */
export async function repoRootFor(task) {
    if (!task.project)
        throw new BusError("invalid", `task ${task.id} has no project directory; create it with --project DIR`);
    if (!existsSync(task.project))
        throw new BusError("not_found", `task ${task.id} project directory is missing: ${task.project}`);
    const top = await git(task.project, ["rev-parse", "--show-toplevel"]);
    if (!top.ok)
        throw new BusError("invalid", `task ${task.id} project is not inside a git repository: ${task.project}`);
    const head = await git(task.project, ["rev-parse", "--verify", "--quiet", "HEAD"]);
    if (!head.ok)
        throw new BusError("invalid", `task ${task.id} repository has no commits yet: ${top.stdout}`);
    return real(top.stdout);
}
/** <home>/worktrees/<repo name>-<hash of repo path>/task-<id>-<created>: one directory per repository, outside it. */
export function taskWorktreePath(home, repoRoot, task) {
    return join(repoDir(home, repoRoot), `task-${task.id}-${task.createdMs.toString(36)}`);
}
function repoDir(home, repoRoot) {
    const digest = createHash("sha256").update(resolve(repoRoot)).digest("hex").slice(0, 8);
    return join(home, "worktrees", `${basename(repoRoot)}-${digest}`);
}
async function listWorktrees(repoRoot) {
    const out = await mustGit(repoRoot, ["worktree", "list", "--porcelain"]);
    const entries = [];
    for (const block of out.split(/\n\s*\n/)) {
        const lines = block.split("\n");
        const head = lines.find((line) => line.startsWith("worktree "));
        if (!head)
            continue;
        const branch = lines.find((line) => line.startsWith("branch "));
        entries.push({
            path: real(head.slice("worktree ".length)),
            branch: branch ? branch.slice("branch refs/heads/".length) : null,
            prunable: lines.some((line) => line.startsWith("prunable")),
        });
    }
    return entries;
}
async function findRegistered(repoRoot, path) {
    const wanted = real(path);
    return (await listWorktrees(repoRoot)).find((entry) => entry.path === wanted && !entry.prunable) ?? null;
}
const LOCK_WAIT_MS = 120_000;
/** A held lock refreshes its mtime this often; one untouched for LOCK_STALE_MS has no live owner. */
const LOCK_HEARTBEAT_MS = 10_000;
const LOCK_STALE_MS = 60_000;
const sleep = (ms) => new Promise((done) => setTimeout(done, ms));
function pidAlive(pid) {
    try {
        process.kill(pid, 0);
        return true;
    }
    catch (error) {
        return error.code === "EPERM";
    }
}
function readOwner(lock) {
    try {
        return readFileSync(join(lock, "owner"), "utf8").trim() || null;
    }
    catch {
        return null;
    }
}
/** Why the lock has no live owner, or null while it looks held. `owner` is "<pid>:<token>". */
function staleReason(lock, owner) {
    const pid = owner ? Number(owner.split(":")[0]) : 0;
    if (pid && !pidAlive(pid))
        return `holder ${pid} is gone`;
    let idleMs = 0;
    try {
        idleMs = Date.now() - statSync(lock).mtimeMs;
    }
    catch {
        return null;
    } // released meanwhile
    // No owner file: the holder died between mkdir and writing it. A live pid with an
    // untouched lock: the pid was reused, or the holder hung. Either way the heartbeat stopped.
    return idleMs > LOCK_STALE_MS ? `no heartbeat for ${Math.round(idleMs / 1000)}s` : null;
}
/** Move a lock aside atomically (one contender wins the rename) and delete it. Returns the owner it held. */
function sweepLock(lock) {
    const aside = `${lock}.stale.${process.pid}.${Date.now()}`;
    try {
        renameSync(lock, aside);
    }
    catch {
        return null;
    } // someone else took it, or it was released
    const owner = readOwner(aside);
    rmSync(aside, { recursive: true, force: true });
    return owner;
}
/**
 * Serialise worktree creation per repository across processes: several supervisors can reach
 * for the same task at once. A mkdir lock with an owner token and a heartbeat; a lock whose
 * owner died, or stopped heartbeating, is swept so a crashed holder never blocks a repository.
 */
async function withRepoLock(dir, fn) {
    mkdirSync(dir, { recursive: true, mode: 0o700 });
    const lock = join(dir, ".lock");
    const mine = `${process.pid}:${randomBytes(6).toString("hex")}`;
    const deadline = Date.now() + LOCK_WAIT_MS;
    for (;;) {
        try {
            mkdirSync(lock);
            writeFileSync(join(lock, "owner"), `${mine}\n`);
            break;
        }
        catch (error) {
            if (error.code !== "EEXIST")
                throw error;
            const owner = readOwner(lock);
            if (staleReason(lock, owner)) {
                // Check and rename are not one step: a lock replaced in between is swept too. Its
                // owner notices at release (the owner file no longer matches) and the work it
                // guards is idempotent, so the worst case is two creators retrying against git.
                sweepLock(lock);
                continue;
            }
            if (Date.now() > deadline)
                throw new BusError("conflict", `timed out waiting for the worktree lock ${lock}`);
            await sleep(50);
        }
    }
    const beat = setInterval(() => { try {
        const now = new Date();
        utimesSync(lock, now, now);
    }
    catch { /* released or swept */ } }, LOCK_HEARTBEAT_MS);
    beat.unref();
    try {
        return await fn();
    }
    finally {
        clearInterval(beat);
        // Only remove a lock that is still ours: a sweep may have replaced it.
        if (readOwner(lock) === mine)
            sweepLock(lock);
    }
}
/** Keep files the harness adapters generate out of `git status`, so a checkout stays removable. */
async function excludeAdapterFiles(worktreePath) {
    const raw = await mustGit(worktreePath, ["rev-parse", "--git-path", "info/exclude"]);
    const file = isAbsolute(raw) ? raw : resolve(worktreePath, raw);
    mkdirSync(dirname(file), { recursive: true });
    let existing = "";
    try {
        existing = readFileSync(file, "utf8");
    }
    catch { /* no exclude file yet */ }
    const have = new Set(existing.split("\n").map((line) => line.trim()));
    const missing = ADAPTER_FILES.filter((entry) => !have.has(entry));
    if (!missing.length)
        return;
    appendFileSync(file, `${existing && !existing.endsWith("\n") ? "\n" : ""}# qagent harness files\n${missing.join("\n")}\n`);
}
function describe(task, repoRoot, entry, created) {
    const workdir = join(entry.path, relative(repoRoot, real(task.project)));
    return { taskId: task.id, repoRoot, path: entry.path, workdir, branch: entry.branch ?? taskBranch(task), created };
}
/**
 * Find or create the task's worktree. A new one branches from the repository's current HEAD;
 * the task's own branch (from an earlier claim whose checkout was removed) is reused so its
 * commits carry over. Fails when the task's project directory is not tracked in git, because
 * the checkout would not contain it.
 */
export async function ensureTaskWorktree(task, home) {
    const repoRoot = await repoRootFor(task);
    const path = taskWorktreePath(home, repoRoot, task);
    const branch = taskBranch(task);
    return withRepoLock(repoDir(home, repoRoot), async () => {
        const existing = await findRegistered(repoRoot, path);
        let created = false;
        if (!existing) {
            if (existsSync(path))
                throw new BusError("conflict", `${path} exists but is not a git worktree; move or delete it`);
            // Only registrations whose directory is gone are dropped; other worktrees are left alone.
            if ((await listWorktrees(repoRoot)).some((entry) => entry.prunable))
                await mustGit(repoRoot, ["worktree", "prune"]);
            const hasBranch = (await git(repoRoot, ["rev-parse", "--verify", "--quiet", `refs/heads/${branch}`])).ok;
            await mustGit(repoRoot, hasBranch ? ["worktree", "add", path, branch] : ["worktree", "add", "-b", branch, path, "HEAD"]);
            created = true;
        }
        await excludeAdapterFiles(path);
        const tree = describe(task, repoRoot, (await findRegistered(repoRoot, path)) ?? { path: real(path), branch }, created);
        if (!tree.workdir.startsWith(tree.path + sep) && tree.workdir !== tree.path) {
            throw new BusError("invalid", `task ${task.id} project ${task.project} is outside its repository checkout`);
        }
        if (!existsSync(tree.workdir)) {
            // An untracked or gitignored project directory is not in the checkout.
            if (created)
                await mustGit(repoRoot, ["worktree", "remove", "--force", path]).catch(() => undefined);
            throw new BusError("invalid", `task ${task.id} project ${task.project} is not tracked in git, so it is not in the worktree; commit it first`);
        }
        return tree;
    });
}
/**
 * The repository root for cleanup. The project directory may have been deleted since the
 * task was made, so start from its nearest existing ancestor; null when no repository is left.
 */
async function cleanupRepoRoot(task) {
    if (!task.project)
        throw new BusError("invalid", `task ${task.id} has no project directory`);
    let dir = resolve(task.project);
    while (!existsSync(dir)) {
        const parent = dirname(dir);
        if (parent === dir)
            return null;
        dir = parent;
    }
    const top = await git(dir, ["rev-parse", "--show-toplevel"]);
    return top.ok ? real(top.stdout) : null;
}
/** A task's checkout directory under the bus home, found by name when its repository is gone. */
function findWorktreeDirByName(home, task) {
    const root = join(home, "worktrees");
    if (!existsSync(root))
        return null;
    const name = `task-${task.id}-${task.createdMs.toString(36)}`;
    for (const repo of readdirSync(root, { withFileTypes: true })) {
        const candidate = join(root, repo.name, name);
        if (repo.isDirectory() && existsSync(candidate))
            return candidate;
    }
    return null;
}
/** True when the checkout's `.git` file points at a repository that no longer exists. */
function repositoryGone(checkout) {
    try {
        const pointer = /^gitdir:\s*(.+)$/m.exec(readFileSync(join(checkout, ".git"), "utf8"));
        return pointer ? !existsSync(pointer[1].trim()) : false;
    }
    catch {
        return false;
    }
}
/**
 * Remove the task's worktree directory. The branch stays, so committed work survives for
 * review and merge. Uncommitted or untracked changes block removal unless `force` is set;
 * gitignored files (build output, .env) go with the directory either way. Works after the
 * task's project directory, or its whole repository, has been deleted: a checkout whose
 * repository is gone cannot be inspected, so it needs `force`.
 */
export async function removeTaskWorktree(task, home, options = {}) {
    const repoRoot = await cleanupRepoRoot(task);
    const branch = taskBranch(task);
    let path = "";
    if (repoRoot) {
        path = taskWorktreePath(home, repoRoot, task);
        const removed = await withRepoLock(repoDir(home, repoRoot), async () => {
            if (!(await findRegistered(repoRoot, path)))
                return false;
            if (!options.force && await mustGit(path, ["status", "--porcelain"])) {
                throw new BusError("conflict", `worktree for task ${task.id} has uncommitted changes (${path}); commit them or pass --force`);
            }
            await mustGit(repoRoot, options.force ? ["worktree", "remove", "--force", path] : ["worktree", "remove", path]);
            return true;
        });
        if (removed)
            return { path, branch, removed: true };
    }
    // No repository, or one that does not know this checkout: only an orphan whose repository is gone is ours to delete.
    const orphan = findWorktreeDirByName(home, task);
    if (!orphan || !repositoryGone(orphan))
        return { path: orphan ?? path, branch, removed: false };
    if (!options.force) {
        throw new BusError("conflict", `the repository for task ${task.id} is gone, so its checkout ${orphan} cannot be checked for uncommitted work; pass --force to delete it`);
    }
    rmSync(orphan, { recursive: true, force: true });
    return { path: orphan, branch, removed: true };
}
/** Remove the worktrees of accepted, failed and cancelled tasks. Dirty ones are kept unless `force`. */
export async function pruneTaskWorktrees(bus, options = {}) {
    const root = join(bus.home, "worktrees");
    if (!existsSync(root))
        return [];
    const results = [];
    for (const repo of readdirSync(root, { withFileTypes: true })) {
        if (!repo.isDirectory())
            continue;
        for (const entry of readdirSync(join(root, repo.name), { withFileTypes: true })) {
            const match = /^task-(\d+)-([0-9a-z]+)$/.exec(entry.name);
            if (!entry.isDirectory() || !match)
                continue;
            const taskId = Number(match[1]);
            const path = join(root, repo.name, entry.name);
            let task;
            try {
                task = bus.getTask(taskId);
            }
            catch {
                results.push({ taskId, path, removed: false, reason: "task not on this bus" });
                continue;
            }
            if (task.createdMs.toString(36) !== match[2]) {
                results.push({ taskId, path, removed: false, reason: "belongs to an earlier task with this id" });
                continue;
            }
            if (!CLOSED_STATES.includes(task.state)) {
                results.push({ taskId, path, removed: false, reason: `task is ${task.state}` });
                continue;
            }
            try {
                const removed = await removeTaskWorktree(task, bus.home, options);
                results.push({ taskId, path, removed: removed.removed, reason: removed.removed ? `task is ${task.state}; branch ${removed.branch} kept` : "not a registered worktree" });
            }
            catch (error) {
                results.push({ taskId, path, removed: false, reason: error.message });
            }
        }
    }
    return results;
}
//# sourceMappingURL=worktree.js.map