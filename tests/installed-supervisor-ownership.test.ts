import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { existsSync, mkdirSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import test from "node:test";
import { verifiedSupervisorProcess } from "../src/instance-processes.js";
import { temporaryDirectory } from "./helpers.js";

function alive(pid: number): boolean {
  try { process.kill(pid, 0); return true; } catch { return false; }
}

test("installed canonical current symlink retains verified supervisor ownership", {
  skip: process.platform === "win32",
  timeout: 10_000,
}, async () => {
  const root = temporaryDirectory("agent-bus-installed-supervisor-");
  const busHome = join(root, "home");
  const release = join(busHome, "app", "releases", "0123456789abcdef0123");
  const current = join(busHome, "app", "current");
  const project = join(root, "project");
  const agentId = "fake-small";
  const port = 17717;
  mkdirSync(join(release, "dist"), { recursive: true });
  mkdirSync(project, { recursive: true });
  writeFileSync(join(release, "package.json"), `${JSON.stringify({ type: "module" })}\n`);
  symlinkSync(release, current, "dir");

  const registryModule = pathToFileURL(resolve("dist/instance-processes.js")).href;
  const script = join(release, "dist", "cli.js");
  writeFileSync(script, `
    import { recordCurrentAgentBusProcess } from ${JSON.stringify(registryModule)};
    const cleanup = recordCurrentAgentBusProcess({
      busHome: ${JSON.stringify(busHome)},
      port: ${port},
      applicationRoot: ${JSON.stringify(release)},
      kind: "supervisor",
      agentId: ${JSON.stringify(agentId)},
    });
    process.on("SIGTERM", () => { cleanup(); process.exit(0); });
    setInterval(() => {}, 1000);
  `);

  const child = spawn(process.execPath, [join(current, "dist", "cli.js"), "supervise", agentId, project], {
    cwd: root,
    stdio: "ignore",
    detached: true,
  });
  try {
    assert.ok(child.pid);
    const recordPath = join(busHome, "runtime", "processes", `${child.pid}.json`);
    const deadline = Date.now() + 5_000;
    while (!existsSync(recordPath) && Date.now() < deadline) {
      await new Promise((resolveDelay) => setTimeout(resolveDelay, 50));
    }
    assert.equal(existsSync(recordPath), true, "supervisor must publish an ownership record");
    const verified = verifiedSupervisorProcess({
      busHome,
      port,
      applicationRoot: release,
      agentId,
      pid: child.pid!,
    });
    assert.ok(verified, "real release root must recognize the canonical app/current launcher path");
  } finally {
    if (child.pid && alive(child.pid)) {
      try { process.kill(-child.pid, "SIGTERM"); } catch { try { process.kill(child.pid, "SIGTERM"); } catch {} }
    }
    rmSync(root, { recursive: true, force: true });
  }
});
