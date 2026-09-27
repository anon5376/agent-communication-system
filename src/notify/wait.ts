/**
 * Waiting for mail: the shared implementation behind bus_wait (and, after
 * integration, `qagent wait`).
 *
 * The wait itself is Bus.waitForMail: it writes status='waiting' once, blocks on
 * the change watcher without writing, and returns on unread mail or a task event.
 * This module adds the inbox signal file. bus.ts rewrites inbox/<agent>.seq after
 * every delivery; SignalFileWatcher watches that file and wakes the waiter the
 * moment it changes, instead of at the change watcher's next poll.
 */
import { mkdirSync, readFileSync, watch, type FSWatcher } from "node:fs";
import { join } from "node:path";
import type { DatabaseSync } from "node:sqlite";
import type { Bus, WaitResult } from "../core/bus.js";
import { ChangeWatcher, type ChangeWatcherOptions } from "../core/changes.js";
import { homeFor } from "../core/db.js";
import { assertSafeAgentId, type Identity } from "../core/identity.js";
import { BusError, DEFAULT_WAIT_SEC, MAX_WAIT_SEC, OPERATOR_ID } from "../core/types.js";

export function inboxDir(dbPath: string): string {
  return join(homeFor(dbPath), "inbox");
}

/** inbox/<agent>.seq next to the database. */
export function signalFilePath(dbPath: string, agentId: string): string {
  if (agentId !== OPERATOR_ID) assertSafeAgentId(agentId);
  return join(inboxDir(dbPath), `${agentId}.seq`);
}

/** The latest delivered sequence number in the signal file, or null when there is none. */
export function readSignalFile(dbPath: string, agentId: string): number | null {
  try {
    const value = Number(readFileSync(signalFilePath(dbPath, agentId), "utf8").trim());
    return Number.isInteger(value) && value > 0 ? value : null;
  } catch {
    return null;
  }
}

/** Wait length in seconds: the explicit value, else QAGENT_BLOCK_SEC (old name AGENT_BUS_BLOCK_SEC), else 240. */
export function waitSeconds(explicit: number | undefined | null, env: NodeJS.ProcessEnv = process.env): number {
  let seconds = explicit;
  if (seconds === undefined || seconds === null) {
    const fromEnv = Number(env.QAGENT_BLOCK_SEC ?? env.AGENT_BUS_BLOCK_SEC ?? DEFAULT_WAIT_SEC);
    seconds = Number.isFinite(fromEnv) ? Math.floor(fromEnv) : DEFAULT_WAIT_SEC;
  }
  if (!Number.isInteger(seconds) || seconds < 1 || seconds > MAX_WAIT_SEC) {
    throw new BusError("invalid", `wait timeout must be a whole number of seconds between 1 and ${MAX_WAIT_SEC}`);
  }
  return seconds;
}

/**
 * A ChangeWatcher that also wakes when inbox/<agent>.seq is rewritten. A wake-up
 * that finds no new event (file events can arrive late or twice) is ignored and
 * the wait continues to its deadline.
 */
export class SignalFileWatcher extends ChangeWatcher {
  private fileWatcher: FSWatcher | null = null;
  private readonly pokes = new Set<() => void>();
  private stopped = false;

  constructor(db: DatabaseSync, dbPath: string, agentId: string, options: ChangeWatcherOptions = {}) {
    super(db, dbPath, options);
    const name = `${agentId}.seq`;
    signalFilePath(dbPath, agentId); // validates the id
    try {
      const dir = inboxDir(dbPath);
      mkdirSync(dir, { recursive: true, mode: 0o700 });
      this.fileWatcher = watch(dir, { persistent: false }, (_event, filename) => {
        if (filename && String(filename) !== name) return;
        for (const poke of [...this.pokes]) poke();
      });
      this.fileWatcher.on("error", () => { this.fileWatcher?.close(); this.fileWatcher = null; });
      this.fileWatcher.unref?.();
    } catch {
      this.fileWatcher = null; // the database watcher alone still bounds latency
    }
  }

  override async next(sinceSeq: number, timeoutMs: number, signal?: AbortSignal): Promise<number> {
    const deadline = Date.now() + Math.max(0, timeoutMs);
    for (;;) {
      const local = new AbortController();
      let poked = false;
      const poke = () => { poked = true; local.abort(); };
      const forward = () => local.abort();
      this.pokes.add(poke);
      signal?.addEventListener("abort", forward, { once: true });
      if (signal?.aborted) local.abort();
      let seq: number;
      try {
        seq = await super.next(sinceSeq, Math.max(0, deadline - Date.now()), local.signal);
      } finally {
        this.pokes.delete(poke);
        signal?.removeEventListener("abort", forward);
      }
      if (seq > sinceSeq || !poked || this.stopped || signal?.aborted || Date.now() >= deadline) return seq;
    }
  }

  override close(): void {
    this.stopped = true;
    this.fileWatcher?.close();
    this.fileWatcher = null;
    for (const poke of [...this.pokes]) poke();
    super.close();
  }
}

export interface WaitOptions {
  timeoutMs: number;
  signal?: AbortSignal;
  watcherOptions?: ChangeWatcherOptions;
}

/** Block until `actor` has unread mail or a task event (Bus.waitForMail). The read cursor is not advanced. */
export async function waitForMail(bus: Bus, actor: Identity, options: WaitOptions): Promise<WaitResult> {
  const watcher = new SignalFileWatcher(bus.db, bus.dbPath, actor.agentId, options.watcherOptions);
  try {
    return await bus.waitForMail(actor, { timeoutMs: options.timeoutMs, signal: options.signal, watcher });
  } finally {
    watcher.close();
  }
}
