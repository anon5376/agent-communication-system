---
id: R05
kind: research
role: research
title: Determine which findings of a code review of the worktree change still reproduce on this commit
mini: true
truth: truth/R05.json
---

## Brief
This commit contains a merged feature: per-task git worktrees (`src/worktree.ts`, `qagent task claim --worktree`, `qagent task worktree`, and `"isolation": "worktree"` in the supervisor). A code review was written against an earlier head of that change, before it was merged; `git log` shows what landed after it. Line numbers in the review refer to that earlier head, not to this commit.

The review's 12 findings follow, verbatim and numbered 1 to 12:

1. **HIGH: src/worktree.ts:47-48, 92-93. A branch left over from another bus is reused without any check.**
   Task ids restart at 1 on every bus (new DB, `QAGENT_BUS_DB` for another project, a bus reset), but the branch is just `qagent/task-<id>`.
   `ensureTaskWorktree` checks out any existing `refs/heads/qagent/task-<id>` as "a re-claimed task".
   Reproduced: a stale `qagent/task-1` at commit "init" was checked out while main was at "newer main". The agent builds on old code, and the reviewer later merges commits from an unrelated task.
   The same cause breaks two buses on one repo: the second fails with "already checked out", reproduced.
   *Fix:* put the bus in the branch name (`qagent/<sha8(bus.dbPath)>/task-<id>`), or record the branch and base commit on the task (a note or column) and reuse only on a match. Otherwise refuse with a clear error.

2. **HIGH: src/worktree.ts:85-94 and src/supervisor.ts:397-404. Concurrent creation of the same task's worktree races.**
   There is no lock between `existsSync`/`registered` and `worktree add`. Four processes calling `ensureTaskWorktree` on one task gave two successes and two `BusError("conflict")`, reproduced.
   In the supervisor this is the normal case. With no mail, a non-managed supervisor takes the *unclaimed* open tasks from `listTasks`. If there is exactly one, every same-role supervisor isolates into it simultaneously. The losers log "worktree unavailable" and quietly run in the shared checkout, which defeats isolation. A sequential winner and loser can also share one worktree.
   *Fix:* serialize per repo with a lock file under `<home>/worktrees/<repo>-<hash>/.lock` (the supervisor already has `acquireLock`). After a failed `add`, re-check `registered()` and return the existing tree. Isolate only tasks this agent has claimed, not open candidates.

3. **MED: src/cli/main.ts:404-412. `task claim --worktree` is not atomic.**
   The claim is committed first. If `ensureTaskWorktree` then throws (project not in git, repo with no commits, git missing), the CLI exits non-zero but the claim stays.
   A retry then fails with "cannot be claimed: it is claimed". MCP (server.ts:205-213) catches the error and keeps the claim, so the two front ends behave differently.
   *Fix:* run a preflight (export `repoRootFor`) before `claimTask`, or catch and print the claim plus a warning with exit 0, as MCP does.

4. **MED: src/worktree.ts:76-77. `workdir` may not exist.**
   If the task's project directory is untracked or gitignored (a new package not yet committed), it does not exist in the new checkout. Reproduced: `.../task-2/newpkg exists: false`.
   The supervisor then spawns the harness with a missing cwd, so the spawn fails with ENOENT, the turn fails, and it counts toward `consecutiveFailures` instead of falling back. CLI and MCP print a path that does not exist.
   *Fix:* in `ensureTaskWorktree`, `if (!existsSync(workdir)) throw new BusError("invalid", "project dir is not tracked in git; commit it first")`. The supervisor's catch then falls back.

5. **MED: src/adapters.ts:384-385, 461, 536 (exposed by supervisor.ts:421). Harness config files make every worktree dirty.**
   Several adapters write config into `context.workdir`: cursor writes `.cursor/mcp.json`, opencode writes `opencode.json`, fake writes `.agent-bus/`. In worktree mode these land in the checkout as untracked files.
   As a result, `task worktree --remove` refuses ("uncommitted changes") and `task worktree prune` keeps these worktrees forever unless `--force` is passed. An agent running `git add -A` also commits the bus DB path and agent env into the branch that gets merged.
   *Fix:* when creating the worktree, append these names to the worktree's exclude file (`git rev-parse --git-path info/exclude`), or keep adapter config outside the checkout.

6. **MED: src/worktree.ts:35-36 (called from supervisor.ts:403). Git runs synchronously in the supervisor loop.**
   `spawnSync("git worktree add")` does a full checkout. Under `supervise --roster`, all agents share one process, so their wait loops, sweeps and the SIGTERM handling in `stopChild` stall for the whole checkout of a large repo.
   *Fix:* use async `execFile` in the supervisor path; the sync version is fine for the CLI.

7. **MED: src/supervisor.ts:420, 450. Worktree turns never resume a session.**
   `sessionId: worktree ? null : ...`, and the new session id is thrown away. Every turn on the task starts cold, including a `changes_requested` revision that needs the earlier context.
   The docs mention this, but per-task resume is cheap: store `session.byTask[taskId]` (the cwd is stable per task) and resume it.

8. **LOW-MED: src/worktree.ts:90. Each creation prunes the user's other worktrees.**
   An unconditional `git worktree prune` runs on every creation and drops the registration of *any* missing worktree in the user's repo, for example one on an unmounted drive, unless it is locked.
   *Fix:* prune only when this path appears as `prunable` in `worktree list --porcelain`.

9. **LOW: src/worktree.ts:106-109. Gitignored files are deleted without warning.**
   The dirty check uses `status --porcelain`, which skips ignored files, and `git worktree remove` deletes them. Reproduced: a `.env` in the worktree was removed without `--force`. Prune does the same.
   *Fix:* also check `status --porcelain --ignored` and warn or refuse, or document it.

10. **LOW: src/cli/main.ts:454-470. `task worktree` and `prune` skip `ctx.identity()`.**
    Every other mutating `task` subcommand checks identity. Without it, any process that can open the DB can `--remove --force` another agent's dirty checkout.
    *Fix:* require that the caller is the assignee or the operator for `--remove`, and the operator for `--force` and for `prune --force`.

11. **LOW: src/worktree.ts:88-93. A leftover directory blocks creation for good.**
    A directory that exists at the path but is not registered (a crashed add, a copy left by hand) makes `worktree add` fail with "already exists", reproduced.
    A registered worktree is also returned without checking which branch it is on (an agent's `git checkout main` is reported as `qagent/task-N`).
    *Fix:* give a specific error for an unregistered existing directory, and compare the `branch` line from `worktree list --porcelain`.

12. **LOW (not verified, Linux host): src/worktree.ts:65-77. Path assumptions on macOS and Windows.**
    `realpathSync` (the JS version) keeps the input's case. On case-insensitive APFS, a project path like `~/repo` spelled with different case than git's `~/Repo` gives `relative()` = `../../...`, so `workdir` points back into the shared checkout. Use `realpathSync.native` and assert that `workdir` starts with `path`.
    On Windows, the nested `~/.agent-bus/worktrees/<repo>-<hash>/task-N/...` path can exceed MAX_PATH unless `core.longpaths` is set.

Question: which of these 12 findings still reproduce on the code at this commit?

- A finding reproduces when the problem it describes still happens with this commit's code: the behaviour it reports can be shown, by running the committed `dist/` (for example `dist/worktree.js`, or the CLI `node dist/qagent.js` against a throwaway bus via `QAGENT_BUS_DB` and a throwaway git repository in a temp dir), or, where running it is impossible, by a code trace. Judge each finding by the problem it states, not by whether the exact fix it proposes was applied, and not by its old line numbers.
- Check every finding, including variants of its scenario that the finding's own wording covers.
- A finding you cannot verify on this machine (for example one specific to another operating system) is not an item; list it in `unresolved` with the reason.
- Name each item `F<n>` (for example `F3`).

The `dist/` directory is committed, so no build is needed. Do not run `npm install`. Do not modify the repository; work in temp directories.

## Acceptance
- `items` lists exactly the findings that still reproduce, as `F<n>`.
- Each reported finding has a claim with a reproduction script or command and its real output (or, if it cannot be run, a code trace with `path:line`) showing the problem on this commit.
- Each of the other findings has a claim saying why it no longer reproduces, with the probe output or `path:line` evidence, or is listed in `unresolved` with the reason it cannot be verified here.
