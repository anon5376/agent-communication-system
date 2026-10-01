// derive-R05.mjs: probe each of the 12 findings from the PR #17 review against the merged
// code at commit 4d4cf5a (dist/ is committed, so no build).
// Usage: node derive-R05.mjs [repoCopy]   (repoCopy defaults to ./repo next to this script)
// Each section prints "F<n>: REPRODUCES" / "F<n>: does not reproduce" / "F<n>: cannot verify here"
// with the observations that decided it. Everything runs in throwaway temp dirs.
import { execFileSync, spawn, spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const REPO = resolve(process.argv[2] ?? join(here, "repo"));
const imp = (p) => import(pathToFileURL(join(REPO, p)).href);
const { Bus } = await imp("dist/core/bus.js");
const wt = await imp("dist/worktree.js");
const { getHarnessAdapter } = await imp("dist/adapters.js");
const QAGENT = join(REPO, "dist/qagent.js");

const GITENV = { ...process.env, GIT_AUTHOR_NAME: "t", GIT_AUTHOR_EMAIL: "t@example.com", GIT_COMMITTER_NAME: "t", GIT_COMMITTER_EMAIL: "t@example.com" };
const git = (cwd, args) => execFileSync("git", args, { cwd, env: GITENV, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] }).trim();
const roots = [];
function fixture({ files = 1 } = {}) {
  const home = realpathSync(mkdtempSync(join(tmpdir(), "r05-")));
  roots.push(home);
  const repo = join(home, "repo");
  mkdirSync(join(repo, "pkg"), { recursive: true });
  git(repo, ["init", "-q", "-b", "main"]);
  for (let i = 0; i < files; i++) writeFileSync(join(repo, "pkg", `f${i}.txt`), `file ${i}\n`.repeat(20));
  writeFileSync(join(repo, ".gitignore"), ".env\n");
  git(repo, ["add", "."]);
  git(repo, ["commit", "-q", "-m", "init"]);
  const dbPath = join(home, "bus", "bus.db");
  const bus = Bus.open({ dbPath });
  bus.init();
  return { home, repo, dbPath, bus, operator: bus.identify("operator") };
}
const verdict = (id, v, ...lines) => { console.log(`\n${id}: ${v}`); for (const l of lines) console.log(`  ${l}`); };
const tryAsync = async (fn) => { try { return { ok: true, value: await fn() }; } catch (e) { return { ok: false, error: e.message }; } };
const cli = (dbPath, args, extraEnv = {}) => {
  const env = { ...process.env, NODE_NO_WARNINGS: "1", QAGENT_BUS_DB: dbPath, ...extraEnv };
  for (const n of ["QAGENT_AGENT_ID", "AGENT_ID", "QAGENT_HOME", "AGENT_BUS_HOME"]) if (!(n in extraEnv)) delete env[n];
  const r = spawnSync(process.execPath, [QAGENT, ...args], { env, encoding: "utf8" });
  return { status: r.status, out: (r.stdout + r.stderr).trim().split("\n").slice(-2).join(" | ") };
};

// ---- F1: stale branch from another bus reused; two buses on one repo collide
{
  const f = fixture();
  git(f.repo, ["branch", "qagent/task-1"]); // stale branch at "init"
  writeFileSync(join(f.repo, "pkg", "f0.txt"), "newer\n");
  git(f.repo, ["commit", "-q", "-am", "newer main"]);
  const mainHead = git(f.repo, ["rev-parse", "HEAD"]);
  const task = f.bus.createTask(f.operator, { title: "t", project: f.repo });
  const a = await wt.ensureTaskWorktree(task, f.bus.home);
  const aHead = git(a.path, ["rev-parse", "HEAD"]);
  // a second bus (other home) on the same repo, also task 1
  const dbB = join(f.home, "busB", "bus.db");
  const busB = Bus.open({ dbPath: dbB }); busB.init();
  await new Promise((r) => setTimeout(r, 5));
  const taskB = busB.createTask(busB.identify("operator"), { title: "t", project: f.repo });
  const b = await tryAsync(() => wt.ensureTaskWorktree(taskB, busB.home));
  const reproduces = a.branch === "qagent/task-1" || aHead !== mainHead || !b.ok;
  verdict("F1", reproduces ? "REPRODUCES" : "does not reproduce",
    `task ids: A=${task.id} B=${taskB.id}; bus A branch ${a.branch}; worktree HEAD == main HEAD: ${aHead === mainHead}`,
    `second bus: ${b.ok ? `ok, branch ${b.value.branch}` : `FAILED ${b.error}`}`);
  busB.close(); f.bus.close();
}

// ---- F2: concurrent creation of one task's worktree from 4 processes
{
  const f = fixture({ files: 200 });
  const task = f.bus.createTask(f.operator, { title: "race", project: f.repo });
  const ref = JSON.stringify({ id: task.id, project: task.project, createdMs: task.createdMs });
  const code = `const wt = await import(${JSON.stringify(pathToFileURL(join(REPO, "dist/worktree.js")).href)});
    try { const t = await wt.ensureTaskWorktree(${ref}, ${JSON.stringify(f.bus.home)}); console.log(JSON.stringify({ ok: true, path: t.path, created: t.created })); }
    catch (e) { console.log(JSON.stringify({ ok: false, error: e.message })); }`;
  const runs = await Promise.all(Array.from({ length: 4 }, () => new Promise((done) => {
    const child = spawn(process.execPath, ["--input-type=module", "-e", code], { env: GITENV });
    let out = ""; child.stdout.on("data", (c) => { out += c; }); child.on("close", () => done(JSON.parse(out.trim().split("\n").pop()))); })));
  const ok = runs.filter((r) => r.ok);
  const created = ok.filter((r) => r.created).length;
  const paths = new Set(ok.map((r) => r.path)).size;
  const sup = readFileSync(join(REPO, "dist/supervisor.js"), "utf8");
  const claimedOnly = /task\.assignee === me\.agentId\)\s*worktree = await ensureTaskWorktree/.test(sup);
  verdict("F2", ok.length === 4 && created === 1 && paths === 1 && claimedOnly ? "does not reproduce" : "REPRODUCES",
    `4 processes: ${ok.length} ok, ${runs.length - ok.length} errors, ${created} created, ${paths} distinct path(s)`,
    ...runs.filter((r) => !r.ok).map((r) => `error: ${r.error}`),
    `supervisor isolates only tasks assigned to itself (dist/supervisor.js check): ${claimedOnly}`);
  f.bus.close();
}

// ---- F3: `task claim --worktree` not atomic (CLI) on a non-git project
{
  const f = fixture();
  const plain = join(f.home, "plain-not-git");
  mkdirSync(plain);
  const inGit = spawnSync("git", ["rev-parse"], { cwd: plain }).status === 0;
  cli(f.dbPath, ["agent", "add", "w1", "--role", "implementation"]);
  const t1 = JSON.parse(spawnSync(process.execPath, [QAGENT, "--as", "operator", "task", "add", "a", "--to", "w1", "--project", plain, "--json"], { env: { ...process.env, QAGENT_BUS_DB: f.dbPath }, encoding: "utf8" }).stdout);
  const explicit = cli(f.dbPath, ["--as", "w1", "task", "claim", String(t1.id), "--worktree"]);
  const stateAfter = f.bus.getTask(t1.id).state;
  const retry = cli(f.dbPath, ["--as", "w1", "task", "claim", String(t1.id)]);
  const t2 = JSON.parse(spawnSync(process.execPath, [QAGENT, "--as", "operator", "task", "add", "b", "--to", "w1", "--project", plain, "--json"], { env: { ...process.env, QAGENT_BUS_DB: f.dbPath }, encoding: "utf8" }).stdout);
  const bare = cli(f.dbPath, ["--as", "w1", "task", "claim", "--worktree"]);
  const reproduces = explicit.status !== 0 && stateAfter !== "open";
  verdict("F3", inGit ? "cannot verify here (temp dir is inside a git repo)" : reproduces ? "REPRODUCES" : "does not reproduce",
    `explicit claim --worktree: exit ${explicit.status}: ${explicit.out}`,
    `task #${t1.id} state after failed claim: ${stateAfter}; plain retry: exit ${retry.status}: ${retry.out}`,
    `bare claim --worktree (task #${t2.id}): exit ${bare.status}: ${bare.out}`);
  f.bus.close();
}

// ---- F4: workdir may not exist (untracked project dir)
{
  const f = fixture();
  const fresh = join(f.repo, "newpkg");
  mkdirSync(fresh); writeFileSync(join(fresh, "x.txt"), "untracked\n");
  const task = f.bus.createTask(f.operator, { title: "u", project: fresh });
  const r = await tryAsync(() => wt.ensureTaskWorktree(task, f.bus.home));
  const path = wt.taskWorktreePath(f.bus.home, f.repo, task);
  verdict("F4", r.ok && !existsSync(r.value.workdir) ? "REPRODUCES" : "does not reproduce",
    r.ok ? `returned workdir ${r.value.workdir} exists: ${existsSync(r.value.workdir)}` : `refused: ${r.error}`,
    `checkout left behind: ${existsSync(path)}`);
  f.bus.close();
}

// ---- F5: harness config files make the worktree dirty (real adapter prepare())
{
  const ctx = (workdir) => ({
    agent: { id: "w1", role: "implementation", modelDefinition: { id: "m", family: "f", provider: "p" }, harnessDefinition: { id: "x" } },
    prompt: "", sessionId: null, pinnedSessionId: null, workdir, mcpServerPath: "/nonexistent/qagent.js", fakeHarnessPath: "", busEnvironment: {}, mcpCommand: null,
  });
  const lines = [];
  let dirtyAny = false;
  for (const [label, sub] of [["project = repo root", ""], ["project = repo/pkg subdirectory", "pkg"]]) {
    const f = fixture();
    const task = f.bus.createTask(f.operator, { title: "a", project: sub ? join(f.repo, sub) : f.repo });
    const tree = await wt.ensureTaskWorktree(task, f.bus.home);
    for (const id of ["cursor", "opencode", "fake"]) {
      const adapter = getHarnessAdapter(id);
      await adapter.prepare?.(ctx(tree.workdir));
      const status = git(tree.path, ["status", "--porcelain", "-uall"]);
      if (status) dirtyAny = true;
      lines.push(`${label}, ${id} adapter: git status = ${JSON.stringify(status)}`);
    }
    const removal = await tryAsync(() => wt.removeTaskWorktree(task, f.bus.home));
    lines.push(`${label}: remove without --force: ${removal.ok ? `removed=${removal.value.removed}` : `REFUSED: ${removal.error.slice(0, 90)}`}`);
    f.bus.close();
  }
  verdict("F5", dirtyAny ? "REPRODUCES" : "does not reproduce", ...lines);
}

// ---- F6: git runs synchronously in the supervisor loop (event-loop stall during checkout)
{
  const f = fixture({ files: 3000 });
  const task = f.bus.createTask(f.operator, { title: "big", project: f.repo });
  let last = Date.now(), maxGap = 0;
  const timer = setInterval(() => { const now = Date.now(); maxGap = Math.max(maxGap, now - last); last = now; }, 5);
  const t0 = Date.now();
  await wt.ensureTaskWorktree(task, f.bus.home);
  const took = Date.now() - t0;
  clearInterval(timer);
  // same checkout done with spawnSync for comparison
  const t1 = Date.now();
  spawnSync("git", ["worktree", "add", "-q", "-b", "sync-compare", join(f.home, "sync-compare"), "HEAD"], { cwd: f.repo });
  const syncTook = Date.now() - t1;
  const src = readFileSync(join(REPO, "dist/worktree.js"), "utf8");
  const syncCalls = (src.match(/spawnSync|execFileSync|execSync/g) ?? []).length;
  verdict("F6", syncCalls > 0 || maxGap > took / 2 ? "REPRODUCES" : "does not reproduce",
    `ensureTaskWorktree on a 3000-file repo took ${took} ms; max event-loop gap ${maxGap} ms (5 ms timer)`,
    `same checkout via spawnSync blocks the loop for ${syncTook} ms; sync child_process calls in dist/worktree.js: ${syncCalls}`);
  f.bus.close();
}

// ---- F7: worktree turns never resume a session (code trace; needs a real CLI harness to probe)
{
  const sup = readFileSync(join(REPO, "src/supervisor.ts"), "utf8").split("\n");
  const find = (re) => { const i = sup.findIndex((l) => re.test(l)); return i < 0 ? "MISSING" : `src/supervisor.ts:${i + 1}: ${sup[i].trim()}`; };
  const a = find(/const taskSession = worktree \?/);
  const b = find(/sessionId: worktree \? taskSession/);
  const c = find(/session\.taskSessions = \{ \.\.\.session\.taskSessions, \[worktree\.branch\]/);
  verdict("F7", [a, b, c].some((x) => x === "MISSING") ? "REPRODUCES" : "does not reproduce (code trace)", a, b, c);
}

// ---- F8: each creation prunes the user's other (missing) worktrees
{
  const f = fixture();
  const elsewhere = join(f.home, "unmounted-drive", "wt");
  git(f.repo, ["worktree", "add", "-q", "-b", "user-branch", elsewhere]);
  rmSync(join(f.home, "unmounted-drive"), { recursive: true, force: true }); // directory gone, like an unmounted drive
  const before = git(f.repo, ["worktree", "list", "--porcelain"]).includes(elsewhere);
  const task = f.bus.createTask(f.operator, { title: "mine", project: f.repo });
  await wt.ensureTaskWorktree(task, f.bus.home);
  const after = git(f.repo, ["worktree", "list", "--porcelain"]).includes(elsewhere);
  verdict("F8", before && !after ? "REPRODUCES" : "does not reproduce",
    `user's worktree registration (directory missing, not locked) before: ${before}, after creating a task worktree: ${after}`);
  f.bus.close();
}

// ---- F9: gitignored files deleted without warning on remove
{
  const f = fixture();
  const task = f.bus.createTask(f.operator, { title: "env", project: f.repo });
  const tree = await wt.ensureTaskWorktree(task, f.bus.home);
  writeFileSync(join(tree.path, ".env"), "SECRET=1\n");
  const status = git(tree.path, ["status", "--porcelain"]);
  const r = await tryAsync(() => wt.removeTaskWorktree(task, f.bus.home));
  verdict("F9", r.ok && r.value.removed && !existsSync(join(tree.path, ".env")) ? "REPRODUCES" : "does not reproduce",
    `git status --porcelain with .env present: ${JSON.stringify(status)}`,
    `removeTaskWorktree (no force): ${r.ok ? `removed=${r.value.removed}` : `refused: ${r.error}`}; .env still exists: ${existsSync(join(tree.path, ".env"))}`);
  f.bus.close();
}

// ---- F10: `task worktree` / prune skip identity checks (CLI)
{
  const f = fixture();
  cli(f.dbPath, ["agent", "add", "w1", "--role", "implementation"]);
  cli(f.dbPath, ["agent", "add", "w2", "--role", "implementation"]);
  const t = JSON.parse(spawnSync(process.execPath, [QAGENT, "--as", "operator", "task", "add", "a", "--to", "w1", "--project", f.repo, "--json"], { env: { ...process.env, QAGENT_BUS_DB: f.dbPath }, encoding: "utf8" }).stdout);
  const claim = cli(f.dbPath, ["--as", "w1", "task", "claim", String(t.id), "--worktree"]);
  const tree = wt.taskWorktreePath(f.bus.home, f.repo, t);
  writeFileSync(join(tree, "dirty.txt"), "x");
  const other = cli(f.dbPath, ["--as", "w2", "task", "worktree", String(t.id), "--remove", "--force"]);
  const otherNoForce = cli(f.dbPath, ["--as", "w2", "task", "worktree", String(t.id), "--remove"]);
  const owner = cli(f.dbPath, ["--as", "w1", "task", "worktree", String(t.id), "--remove", "--force"]);
  const prune = cli(f.dbPath, ["--as", "w2", "task", "worktree", "prune", "--force"]);
  const stillThere = existsSync(join(tree, "dirty.txt"));
  verdict("F10", !stillThere ? "REPRODUCES" : "does not reproduce",
    `claim by w1: exit ${claim.status}`,
    `w2 --remove --force: exit ${other.status}: ${other.out}`,
    `w2 --remove: exit ${otherNoForce.status}: ${otherNoForce.out}`,
    `w1 (assignee) --remove --force: exit ${owner.status}: ${owner.out}`,
    `w2 prune --force: exit ${prune.status}: ${prune.out}`,
    `dirty checkout survived: ${stillThere}`);
  f.bus.close();
}

// ---- F11: leftover unregistered directory; branch reported without checking
{
  const f = fixture();
  const task = f.bus.createTask(f.operator, { title: "leftover", project: f.repo });
  mkdirSync(wt.taskWorktreePath(f.bus.home, f.repo, task), { recursive: true });
  const r = await tryAsync(() => wt.ensureTaskWorktree(task, f.bus.home));
  const task2 = f.bus.createTask(f.operator, { title: "switch", project: f.repo });
  const tree = await wt.ensureTaskWorktree(task2, f.bus.home);
  git(tree.path, ["switch", "-q", "-c", "agent-switched"]);
  const again = await wt.ensureTaskWorktree(task2, f.bus.home);
  const generic = !r.ok && /already exists/.test(r.error);
  verdict("F11", generic || again.branch !== "agent-switched" ? "REPRODUCES" : "does not reproduce",
    `leftover dir: ${r.ok ? "created?!" : `error: ${r.error}`}`,
    `after agent switched branch, reported branch: ${again.branch}`);
  f.bus.close();
}

// ---- F12: macOS case-insensitive paths / Windows MAX_PATH: cannot be probed on this Linux host
{
  const src = readFileSync(join(REPO, "src/worktree.ts"), "utf8").split("\n");
  const lines = src.map((l, i) => [i + 1, l]).filter(([, l]) => /realpathSync\.native|startsWith\(tree\.path \+ sep\)/.test(l)).map(([n, l]) => `src/worktree.ts:${n}: ${l.trim()}`);
  verdict("F12", `cannot verify here (host ${process.platform})`, ...lines, "no core.longpaths handling found for Windows MAX_PATH");
}

for (const r of roots) rmSync(r, { recursive: true, force: true });
