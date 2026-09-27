/**
 * Start `qagent supervise <agent> <project>` as a detached background process, with its
 * output appended to <home>/logs/<agent>.out. Nothing in core imports this module.
 */
import { spawn } from "node:child_process";
import { mkdirSync, openSync } from "node:fs";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { homeFor, resolveDbPath } from "./core/db.js";
const QAGENT_BIN = fileURLToPath(new URL("./qagent.js", import.meta.url));
export function launchSupervisor(options) {
    const dbPath = resolveDbPath(options.dbPath ?? null);
    const projectRoot = resolve(options.projectRoot);
    const configPath = options.configPath ? resolve(options.configPath) : null;
    const logDir = join(homeFor(dbPath), "logs");
    mkdirSync(logDir, { recursive: true, mode: 0o700 });
    const log = openSync(join(logDir, `${options.agentId}.out`), "a");
    const args = [options.qagentBin ?? QAGENT_BIN, "--db", dbPath, "supervise", options.agentId, projectRoot];
    if (configPath)
        args.push("--config", configPath);
    const child = spawn(process.execPath, args, { detached: true, stdio: ["ignore", log, log], env: process.env });
    child.unref();
    return { agentId: options.agentId, pid: child.pid ?? 0, projectRoot, dbPath, configPath };
}
//# sourceMappingURL=supervisor-launch.js.map