import { execFileSync } from "node:child_process";
import {
  mkdirSync,
  readFileSync,
  readlinkSync,
  readdirSync,
  renameSync,
  unlinkSync,
  writeFileSync,
} from "node:fs";
import { join, resolve } from "node:path";

export type AgentBusProcessKind = "broker" | "supervisor";

export interface AgentBusProcessRecord {
  pid: number;
  kind: AgentBusProcessKind;
  agentId: string | null;
  busHome: string;
  port: number;
  applicationRoot: string;
  command: string;
  startFingerprint: string;
  recordedAt: number;
}

function psField(pid: number, field: string): string {
  if (process.platform === "win32") return "";
  try {
    return execFileSync("ps", ["-p", String(pid), "-o", `${field}=`], {
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
    }).trim();
  } catch {
    return "";
  }
}

export function processCommand(pid: number): string {
  return psField(pid, "command");
}

export function processStartFingerprint(pid: number): string {
  return psField(pid, "lstart").replace(/\s+/g, " ").trim();
}

export function processParentPid(pid: number): number | null {
  const value = Number(psField(pid, "ppid"));
  return Number.isInteger(value) && value > 0 ? value : null;
}

export function processCwd(pid: number): string {
  if (process.platform === "linux") {
    try { return resolve(readlinkSync(`/proc/${pid}/cwd`)); } catch {}
  }
  if (process.platform !== "win32") {
    try {
      const output = execFileSync("lsof", ["-a", "-p", String(pid), "-d", "cwd", "-Fn"], { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] });
      const line = output.split("\n").find((row) => row.startsWith("n"));
      return line ? resolve(line.slice(1)) : "";
    } catch {}
  }
  return "";
}

function processDirectory(busHome: string): string {
  return join(resolve(busHome), "runtime", "processes");
}

function recordPath(busHome: string, pid: number): string {
  return join(processDirectory(busHome), `${pid}.json`);
}

function removeRecord(path: string): void {
  try { unlinkSync(path); } catch {}
}

export function recordCurrentAgentBusProcess(options: {
  busHome: string;
  port: number;
  applicationRoot: string;
  kind: AgentBusProcessKind;
  agentId?: string | null;
}): () => void {
  if (process.platform === "win32") return () => {};
  const command = processCommand(process.pid);
  const startFingerprint = processStartFingerprint(process.pid);
  if (!command || !startFingerprint) return () => {};

  const directory = processDirectory(options.busHome);
  mkdirSync(directory, { recursive: true, mode: 0o700 });
  const path = recordPath(options.busHome, process.pid);
  const temp = `${path}.${process.pid}.tmp`;
  const record: AgentBusProcessRecord = {
    pid: process.pid,
    kind: options.kind,
    agentId: options.agentId ?? null,
    busHome: resolve(options.busHome),
    port: options.port,
    applicationRoot: resolve(options.applicationRoot),
    command,
    startFingerprint,
    recordedAt: Date.now(),
  };
  writeFileSync(temp, `${JSON.stringify(record, null, 2)}\n`, { mode: 0o600 });
  renameSync(temp, path);

  return () => {
    try {
      const current = JSON.parse(readFileSync(path, "utf8")) as Partial<AgentBusProcessRecord>;
      if (Number(current.pid) === process.pid && current.startFingerprint === startFingerprint) removeRecord(path);
    } catch {}
  };
}

export function ownedAgentBusPids(options: {
  busHome: string;
  port: number;
  includeSupervisors: boolean;
}): number[] {
  if (process.platform === "win32") return [];
  const directory = processDirectory(options.busHome);
  let names: string[];
  try { names = readdirSync(directory); } catch { return []; }

  const owned: number[] = [];
  for (const name of names) {
    if (!name.endsWith(".json")) continue;
    const path = join(directory, name);
    let record: AgentBusProcessRecord;
    try { record = JSON.parse(readFileSync(path, "utf8")) as AgentBusProcessRecord; }
    catch { removeRecord(path); continue; }

    const pid = Number(record.pid);
    const validShape = Number.isInteger(pid) && pid > 0
      && (record.kind === "broker" || record.kind === "supervisor")
      && resolve(String(record.busHome ?? "")) === resolve(options.busHome)
      && Number.isInteger(Number(record.port)) && Number(record.port) > 0
      && typeof record.command === "string" && record.command.length > 0
      && typeof record.startFingerprint === "string" && record.startFingerprint.length > 0;
    if (!validShape) { removeRecord(path); continue; }
    // A process directory is shared by all ports for one AGENT_BUS_HOME. Querying
    // one instance must never delete another instance's valid ownership record.
    if (Number(record.port) !== options.port) continue;
    if (!options.includeSupervisors && record.kind === "supervisor") continue;

    const command = processCommand(pid);
    const startFingerprint = processStartFingerprint(pid);
    if (command !== record.command || startFingerprint !== record.startFingerprint) {
      removeRecord(path);
      continue;
    }
    if (pid !== process.pid) owned.push(pid);
  }
  return [...new Set(owned)];
}

function escapeRegex(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function supervisorCommandMatches(record: AgentBusProcessRecord, applicationRoot: string, agentId: string): boolean {
  const id = escapeRegex(agentId);
  const roots = [
    resolve(applicationRoot),
    resolve(join(record.busHome, "app", "current")),
  ];
  for (const candidate of [...new Set(roots)]) {
    const root = escapeRegex(candidate);
    const absolute = new RegExp(`(?:^|\\s)(?:\\S*node\\S*\\s+)?${root}/(?:dist/cli\\.js|cli\\.js)\\s+supervise\\s+${id}(?:\\s|$)`, "i");
    if (absolute.test(record.command)) return true;
  }
  const relative = new RegExp(`(?:^|\\s)(?:\\S*node\\S*\\s+)?(?:\\./)?(?:dist/cli\\.js|cli\\.js)\\s+supervise\\s+${id}(?:\\s|$)`, "i");
  return relative.test(record.command) && processCwd(record.pid) === resolve(applicationRoot);
}

export function verifiedSupervisorProcess(options: {
  busHome: string;
  port: number;
  applicationRoot: string;
  agentId: string;
  pid: number;
}): AgentBusProcessRecord | null {
  if (process.platform === "win32") return null;
  const pid = Number(options.pid);
  if (!Number.isInteger(pid) || pid <= 0) return null;
  const path = recordPath(options.busHome, pid);
  let record: AgentBusProcessRecord;
  try { record = JSON.parse(readFileSync(path, "utf8")) as AgentBusProcessRecord; }
  catch { return null; }
  const valid = record.pid === pid
    && record.kind === "supervisor"
    && record.agentId === options.agentId
    && resolve(record.busHome) === resolve(options.busHome)
    && record.port === options.port
    && resolve(record.applicationRoot) === resolve(options.applicationRoot)
    && Boolean(record.command)
    && Boolean(record.startFingerprint)
    && supervisorCommandMatches(record, options.applicationRoot, options.agentId);
  if (!valid) return null;
  if (processCommand(pid) !== record.command) return null;
  if (processStartFingerprint(pid) !== record.startFingerprint) return null;
  return record;
}
