// derive-R06.mjs: check public claims in README.md, CHANGELOG.md and docs/*.md against the code
// at commit 4d4cf5a. Runs commands against a throwaway bus (QAGENT_HOME/QAGENT_BUS_DB in a temp dir),
// reads config files, and probes behaviour. No network.
// Usage: node derive-R06.mjs [repoCopy]   (defaults to ./repo next to this script)
import { execFileSync, spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const REPO = resolve(process.argv[2] ?? join(here, "repo"));
const QAGENT = join(REPO, "dist/qagent.js");
const tmp = realpathSync(mkdtempSync(join(tmpdir(), "r06-")));
const env = { ...process.env, NODE_NO_WARNINGS: "1", QAGENT_HOME: join(tmp, "home"), QAGENT_BUS_DB: join(tmp, "home", "bus.db") };
for (const n of ["QAGENT_AGENT_ID", "AGENT_ID", "AGENT_BUS_HOME"]) delete env[n];
const pkg = JSON.parse(readFileSync(join(REPO, "package.json"), "utf8"));
const show = (title, lines) => { console.log(`\n## ${title}`); for (const l of [].concat(lines)) console.log(`  ${l}`); };
function q(args, opts = {}) {
  const r = spawnSync(process.execPath, [QAGENT, ...args], { env, encoding: "utf8", timeout: opts.timeout ?? 20_000, cwd: opts.cwd ?? tmp });
  const text = `${r.stdout}${r.stderr}`.trim().replace(/\s+/g, " ").slice(0, 220);
  return `$ qagent ${args.join(" ")}  -> exit ${r.status ?? r.signal}: ${text}`;
}
const git = (cwd, args) => execFileSync("git", args, { cwd, encoding: "utf8", env: { ...process.env, GIT_AUTHOR_NAME: "t", GIT_AUTHOR_EMAIL: "t@e", GIT_COMMITTER_NAME: "t", GIT_COMMITTER_EMAIL: "t@e" } }).trim();

// ---- bus setup exactly as README Quick start
show("README quick start commands", [
  q(["init"]),
  q(["agent", "add", "claude", "--role", "manager", "--authority", "manager"]),
  q(["agent", "add", "codex", "--role", "worker"]),
  q(["--as", "claude", "send", "codex", "parser", "Please take the parser task."]),
  q(["--as", "codex", "inbox"]),
  q(["--as", "codex", "wait", "--timeout", "1"]),
  q(["mcp-config", "--agent", "claude", "--client", "claude"]),
  q(["mcp-config", "--agent", "codex", "--client", "codex"]),
]);

// ---- CHANGELOG [0.2.0] lists `task ... release`; CHANGELOG [Unreleased] says trace has `--export`
q(["--as", "claude", "task", "add", "Write the parser", "--to", "codex"]);
show("CHANGELOG task release / trace --export / FULL-GUIDE trace html", [
  q(["--as", "codex", "task", "claim", "1"]),
  q(["--as", "codex", "task", "release", "1"]),
  q(["trace", "1", "--export"]),
  q(["trace", "1", "--export", join(tmp, "t.html")]),
  `file written by --export: ${existsSync(join(tmp, "t.html"))}`,
  q(["trace", "1", "--format", "html", "--out", join(tmp, "t2.html")]),
]);

// ---- free-ai-setup.md:286 "(independent review requires it)" (a different model family)
q(["agent", "add", "rev", "--role", "reviewer", "--model", "same-model"]);
q(["agent", "add", "wk", "--role", "worker", "--model", "same-model"]);
q(["--as", "rev", "task", "add", "family test", "--to", "wk"]);
const famCount = (readFileSync(join(REPO, "src/core/bus.ts"), "utf8").match(/family/g) ?? []).length;
show("review gate vs model family", [
  q(["--as", "wk", "task", "claim", "2"]),
  q(["--as", "wk", "task", "submit", "2", "--summary", "done"]),
  q(["--as", "rev", "task", "review", "2", "--accept", "--feedback", "ok"]),
  `occurrences of "family" in src/core/bus.ts: ${famCount}`,
]);

// ---- doctor as documented (free-ai-setup.md, README)
show("doctor / supervise", [
  q(["doctor"]),
  q(["supervise", "--help"]),
]);

// ---- package.json facts (promotion-playbook.md, submission-pack.md, releasing.md)
show("package.json", [
  `name=${pkg.name} version=${pkg.version} license=${pkg.license} private=${pkg.private}`,
  `bin: ${Object.keys(pkg.bin).join(", ")}`,
  `scripts: ${Object.keys(pkg.scripts).join(", ")}`,
]);

// ---- releasing.md:154 `npx -y agent-communication-system@latest --help`: npx bin resolution with this bin map.
// Offline replica: a package with the same name and bin map (no deps), run through npx from a local folder.
{
  const fake = join(tmp, "fakepkg");
  mkdirSync(join(fake, "dist"), { recursive: true });
  for (const f of new Set(Object.values(pkg.bin))) writeFileSync(join(fake, f), "#!/usr/bin/env node\nconsole.log('ran ' + process.argv[1]);\n", { mode: 0o755 });
  writeFileSync(join(fake, "package.json"), JSON.stringify({ name: pkg.name, version: pkg.version, bin: pkg.bin }));
  const r = spawnSync("npx", ["-y", "--offline", fake, "--help"], { encoding: "utf8", cwd: tmp, env: { ...process.env, npm_config_cache: join(tmp, "npmcache") }, timeout: 60_000 });
  const r2 = spawnSync("npx", ["-y", "--offline", "-p", fake, "qagent", "--help"], { encoding: "utf8", cwd: tmp, env: { ...process.env, npm_config_cache: join(tmp, "npmcache") }, timeout: 60_000 });
  show("npx bin resolution (replica of package.json bin map)", [
    `npx -y <pkg> --help -> exit ${r.status}: ${(r.stdout + r.stderr).trim().replace(/\s+/g, " ").slice(0, 200)}`,
    `npx -y -p <pkg> qagent --help -> exit ${r2.status}: ${(r2.stdout + r2.stderr).trim().replace(/\s+/g, " ").slice(0, 200)}`,
  ]);
}

// ---- CHANGELOG.md:39 "~129 KB tarball"
{
  const r = spawnSync("npm", ["pack", "--dry-run", "--json", "--ignore-scripts"], { cwd: REPO, encoding: "utf8", env: { ...process.env, npm_config_cache: join(tmp, "npmcache") } });
  try {
    const [info] = JSON.parse(r.stdout);
    show("npm pack --dry-run", [`size(tarball)=${info.size} bytes (${(info.size / 1024).toFixed(1)} KB), unpackedSize=${info.unpackedSize}, files=${info.entryCount}`]);
  } catch { show("npm pack --dry-run", [`failed: ${r.stderr.slice(0, 200)}`]); }
}

// ---- git facts: releasing.md "no tags exist yet"; competitive-analysis "no built-in git worktree isolation"
show("git / source facts", [
  `git tag: ${git(REPO, ["tag"]) || "(none)"}`,
  `src/worktree.ts exists: ${existsSync(join(REPO, "src/worktree.ts"))}; CLI usage line: ${spawnSync(process.execPath, [QAGENT, "--help"], { env, encoding: "utf8" }).stdout.split("\n").find((l) => l.includes("task worktree"))?.trim()}`,
  `acs/TUI in package.json bin: ${Object.keys(pkg.bin).includes("acs")}; files under src mentioning ratatui/tui: ${spawnSync("grep", ["-rli", "ratatui\\|terminal ui\\|\\btui\\b", join(REPO, "src")], { encoding: "utf8" }).stdout.trim() || "(none)"}`,
  `docs/assets/acs-demo.gif exists: ${existsSync(join(REPO, "docs/assets/acs-demo.gif"))}`,
]);

// ---- FULL-GUIDE.md:260 adapter files "do not make a checkout dirty" (project in a subdirectory, cursor adapter)
{
  const { Bus } = await import(pathToFileURL(join(REPO, "dist/core/bus.js")).href);
  const wt = await import(pathToFileURL(join(REPO, "dist/worktree.js")).href);
  const { getHarnessAdapter } = await import(pathToFileURL(join(REPO, "dist/adapters.js")).href);
  const repo = join(tmp, "gitrepo");
  mkdirSync(join(repo, "pkg"), { recursive: true });
  git(repo, ["init", "-q", "-b", "main"]);
  writeFileSync(join(repo, "pkg", "a.txt"), "a\n");
  git(repo, ["add", "."]); git(repo, ["commit", "-q", "-m", "init"]);
  const bus = Bus.open({ dbPath: join(tmp, "wtbus", "bus.db") }); bus.init();
  const task = bus.createTask(bus.identify("operator"), { title: "sub", project: join(repo, "pkg") });
  const tree = await wt.ensureTaskWorktree(task, bus.home);
  await getHarnessAdapter("cursor").prepare({ agent: { id: "w", role: "r", modelDefinition: { id: "m", family: "f", provider: "p" }, harnessDefinition: { id: "cursor" } }, workdir: tree.workdir, busEnvironment: {}, mcpServerPath: "/x/qagent.js", mcpCommand: null });
  const status = git(tree.path, ["status", "--porcelain", "-uall"]);
  let removal;
  try { removal = JSON.stringify(await wt.removeTaskWorktree(task, bus.home)); } catch (e) { removal = `refused: ${e.message.slice(0, 80)}`; }
  show("worktree with project in a subdirectory + cursor adapter", [`workdir=${tree.workdir.replace(tmp, "$TMP")}`, `git status: ${JSON.stringify(status)}`, `remove without --force: ${removal}`]);
  bus.close();
}

rmSync(tmp, { recursive: true, force: true });
