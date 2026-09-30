/**
 * The v2 coordination library: messages, cursors, tasks, reviews and leases over
 * one SQLite file. Every write is one BEGIN IMMEDIATE transaction that also
 * appends an events row.
 *
 * Task, review and lease rules are ported from broker.ts:113-176, 566-777,
 * 1095-1333 and store.ts:341-385; the read cursor, broadcast and task notes from
 * prototype_0.2/coordinator/store.py:287-336, 477-483.
 */
import { randomUUID } from "node:crypto";
import { mkdirSync, renameSync, writeFileSync } from "node:fs";
import { join, relative, resolve, sep } from "node:path";
import type { DatabaseSync } from "node:sqlite";
import { ChangeWatcher, type ChangeWatcherOptions } from "./changes.js";
import { appendEvent, homeFor, latestEventSeq, openDatabase, resolveDbPath, transaction } from "./db.js";
import {
  adoptToken, agentIdFromEnv, assertSafeAgentId, ensurePrivateDirectories, hashToken, Identity, operatorTokenPath,
  readTokenFile, requireOperator, resolveIdentity, storedIdentity, storeNewToken, tokenPathFor, writePrivateToken,
} from "./identity.js";
import {
  Agent, AgentStatus, Authority, boundedString, BusError, BusEvent, CLAIM_TTL_MS, CLOSED_STATES, ContextReference,
  contextReferences, LIMITS, Message, MESSAGE_TYPES, MessageType, OPERATOR_ID, PRIORITIES, Priority, STALE_AGENT_MS,
  Task, TASK_STATES, TaskDetail, TaskNote, TaskResult, TaskReview, TaskState, TaskSummary, ValidationObservation,
} from "./types.js";

type Row = Record<string, unknown>;

export interface BusOptions {
  /** Database path; defaults to resolveDbPath() (QAGENT_BUS_DB or ~/.agent-bus/bus.db). */
  dbPath?: string;
  /** Clock override for tests. */
  now?: () => number;
  claimTtlMs?: number;
}

export interface SendInput {
  to: string;
  subject?: string;
  body: string;
  type?: MessageType;
  thread?: string;
  taskId?: number | null;
  refs?: unknown;
  requiresAck?: boolean;
}

export interface CreateTaskInput {
  title: string;
  brief?: string;
  acceptance?: string;
  to?: string | null;
  reviewer?: string | null;
  role?: string;
  priority?: Priority;
  parentId?: number | null;
  dependencies?: number[];
  pathScopes?: string[];
  project?: string | null;
  refs?: unknown;
  maxRetries?: number;
}

export interface SubmitInput {
  summary: string;
  details?: string;
  changedFiles?: string[];
  artifacts?: unknown;
  validation?: unknown;
}

export interface ListTasksInput {
  /** Only tasks assigned to, created by, or to be reviewed by this agent. */
  mine?: string | null;
  states?: TaskState[];
  includeClosed?: boolean;
  limit?: number;
}

export interface WaitResult {
  status: "mail" | "task" | "timeout";
  messages: Message[];
  events: BusEvent[];
  seq: number;
}

export interface InitResult {
  dbPath: string;
  home: string;
  operatorTokenPath: string;
  operator: "created" | "adopted" | "unchanged" | "rotated";
}

function json<T>(value: unknown, fallback: T): T {
  if (typeof value !== "string" || !value) return fallback;
  try { return JSON.parse(value) as T; } catch { return fallback; }
}

function num(value: unknown): number | null {
  return value === null || value === undefined ? null : Number(value);
}

function placeholders(count: number): string {
  return Array.from({ length: count }, () => "?").join(", ");
}

export function normalizeScope(projectRoot: string, scope: string): string {
  const root = resolve(projectRoot);
  const absolute = resolve(root, scope || ".");
  if (absolute !== root && !absolute.startsWith(root + sep)) throw new BusError("invalid", `path scope escapes project root: ${scope}`);
  const rel = relative(root, absolute).split(sep).join("/");
  return rel || ".";
}

function scopesOverlap(a: string, b: string): boolean {
  return a === "." || b === "." || a === b || a.startsWith(`${b}/`) || b.startsWith(`${a}/`);
}

export class Bus {
  readonly db: DatabaseSync;
  readonly dbPath: string;
  readonly home: string;
  private readonly clock: () => number;
  private readonly claimTtlMs: number;
  private readonly pendingSignals = new Map<string, number>();

  constructor(options: BusOptions = {}) {
    this.dbPath = options.dbPath ? resolve(options.dbPath) : resolveDbPath();
    this.home = homeFor(this.dbPath);
    this.clock = options.now ?? Date.now;
    this.claimTtlMs = options.claimTtlMs ?? CLAIM_TTL_MS;
    this.db = openDatabase(this.dbPath);
  }

  static open(options: BusOptions = {}): Bus {
    return new Bus(options);
  }

  close(): void {
    if (this.db.isOpen) this.db.close();
  }

  now(): number {
    return this.clock();
  }

  /** Run a write transaction, then refresh inbox signal files for the messages it delivered. */
  private write<T>(fn: () => T): T {
    this.pendingSignals.clear();
    let value: T;
    try {
      value = transaction(this.db, fn);
    } catch (error) {
      this.pendingSignals.clear();
      throw error;
    }
    this.flushSignals();
    return value;
  }

  private event(actor: string, kind: string, entity: string, entityId: string | number, data: unknown = {}): number {
    return appendEvent(this.db, { tsMs: this.now(), actor, kind, entity, entityId, data });
  }

  /**
   * Record that an agent acted. An agent that acts is not waiting, so a stored 'waiting'
   * left behind by a killed waiter is cleared here instead of turning into 'offline'.
   */
  private touch(agentId: string, status?: AgentStatus): void {
    if (status) {
      this.db.prepare("UPDATE agents SET last_seen_ms = ?, status = ?, wait_until_ms = NULL WHERE id = ?").run(this.now(), status, agentId);
    } else {
      this.db.prepare(`
        UPDATE agents SET last_seen_ms = ?, status = CASE WHEN status IN ('offline', 'waiting') THEN 'idle' ELSE status END,
          wait_until_ms = CASE WHEN status = 'waiting' THEN NULL ELSE wait_until_ms END
        WHERE id = ?
      `).run(this.now(), agentId);
    }
  }

  /** Set the caller's own status to working or idle (the supervisor marks a running turn). */
  setStatus(actor: Identity, status: "working" | "idle"): Agent {
    if (status !== "working" && status !== "idle") throw new BusError("invalid", `status must be working or idle, not ${String(status)}`);
    this.write(() => {
      if (!this.agentRow(actor.agentId)) throw new BusError("not_found", `unknown agent: ${actor.agentId}`);
      this.touch(actor.agentId, status);
      this.event(actor.agentId, `agent_${status}`, "agent", actor.agentId);
    });
    return this.getAgent(actor.agentId)!;
  }

  // ---------------------------------------------------------------- identity

  /** Resolve the calling identity from --as / QAGENT_AGENT_ID / AGENT_ID and its token file. */
  identify(agentId?: string | null): Identity {
    const id = agentId ?? agentIdFromEnv();
    if (!id) throw new BusError("unauthorized", "no agent identity: set QAGENT_AGENT_ID or pass --as <id>");
    return resolveIdentity(this.db, this.home, id);
  }

  /** Create the operator agent and token. Adopts an existing operator.token; rotates when file and hash disagree. */
  init(): InitResult {
    ensurePrivateDirectories(this.home);
    const tokenPath = operatorTokenPath(this.home);
    const operator = this.write((): InitResult["operator"] => {
      const now = this.now();
      this.db.prepare(`
        INSERT INTO agents(id, role, model, harness, status, last_seen_ms, created_ms) VALUES(?, 'operator', 'human', 'cli', 'idle', ?, ?)
        ON CONFLICT(id) DO NOTHING
      `).run(OPERATOR_ID, now, now);
      const stored = storedIdentity(this.db, OPERATOR_ID);
      const fileToken = readTokenFile(tokenPath);
      if (stored && fileToken && hashToken(fileToken) === stored.tokenHash && stored.authority === "operator") return "unchanged";
      const owner = fileToken
        ? this.db.prepare("SELECT agent_id FROM identities WHERE token_hash = ?").get(hashToken(fileToken)) as { agent_id: string } | undefined
        : undefined;
      let outcome: InitResult["operator"];
      if (!stored && fileToken && !owner) {
        adoptToken(this.db, OPERATOR_ID, "operator", fileToken, now);
        outcome = "adopted";
      } else {
        const token = storeNewToken(this.db, OPERATOR_ID, "operator", now);
        writePrivateToken(this.home, tokenPath, token);
        outcome = stored ? "rotated" : "created";
      }
      this.event(OPERATOR_ID, "operator_token", "agent", OPERATOR_ID, { outcome });
      return outcome;
    });
    return { dbPath: this.dbPath, home: this.home, operatorTokenPath: tokenPath, operator };
  }

  addAgent(actor: Identity, input: { id: string; role?: string; model?: string; harness?: string; parent?: string | null; authority?: Authority }): { agent: Agent; tokenPath: string } {
    requireOperator(actor, "add agents");
    const id = assertSafeAgentId(String(input.id ?? ""));
    if (id === OPERATOR_ID) throw new BusError("invalid", "the operator agent is created by `qagent init`");
    const authority = input.authority ?? "worker";
    if (authority !== "worker" && authority !== "manager") throw new BusError("invalid", `authority must be worker or manager, not ${authority}`);
    const role = boundedString(input.role, "role", LIMITS.role);
    const model = boundedString(input.model, "model", LIMITS.model);
    const harness = boundedString(input.harness, "harness", LIMITS.model);
    const parent = input.parent ? assertSafeAgentId(input.parent) : null;
    const tokenPath = tokenPathFor(this.home, id);
    this.write(() => {
      const now = this.now();
      if (parent && !this.agentRow(parent)) throw new BusError("not_found", `unknown parent agent: ${parent}`);
      const existing = this.agentRow(id);
      if (existing && storedIdentity(this.db, id)) throw new BusError("conflict", `agent ${id} already exists; use \`qagent token rotate ${id}\``);
      if (existing) {
        this.db.prepare(`
          UPDATE agents SET role = COALESCE(NULLIF(?, ''), role), model = COALESCE(NULLIF(?, ''), model),
            harness = COALESCE(NULLIF(?, ''), harness), parent_id = COALESCE(?, parent_id) WHERE id = ?
        `).run(role, model, harness, parent, id);
      } else {
        this.db.prepare(`
          INSERT INTO agents(id, role, model, harness, parent_id, status, created_ms) VALUES(?, ?, ?, ?, ?, 'offline', ?)
        `).run(id, role, model, harness, parent, now);
      }
      const token = storeNewToken(this.db, id, authority, now);
      writePrivateToken(this.home, tokenPath, token);
      this.event(actor.agentId, "agent_added", "agent", id, { role, model, harness, parent, authority });
    });
    return { agent: this.getAgent(id)!, tokenPath };
  }

  rotateToken(actor: Identity, agentId: string): { tokenPath: string } {
    requireOperator(actor, "rotate tokens");
    const id = agentId === OPERATOR_ID ? OPERATOR_ID : assertSafeAgentId(agentId);
    const tokenPath = tokenPathFor(this.home, id);
    this.write(() => {
      if (!this.agentRow(id)) throw new BusError("not_found", `unknown agent: ${id}`);
      const authority = storedIdentity(this.db, id)?.authority ?? (id === OPERATOR_ID ? "operator" : "worker");
      const token = storeNewToken(this.db, id, authority, this.now());
      writePrivateToken(this.home, tokenPath, token);
      this.event(actor.agentId, "token_rotated", "agent", id);
    });
    return { tokenPath };
  }

  // ------------------------------------------------------------------ agents

  private agentRow(id: string): Row | undefined {
    return this.db.prepare("SELECT * FROM agents WHERE id = ?").get(id) as Row | undefined;
  }

  private toAgent(row: Row): Agent {
    const now = this.now();
    const stored = String(row.status ?? "offline");
    const waitUntil = num(row.wait_until_ms);
    const lastSeen = num(row.last_seen_ms);
    let status: AgentStatus;
    if (stored === "waiting") status = waitUntil !== null && waitUntil >= now ? "waiting" : "offline";
    else if (stored === "offline") status = "offline";
    else status = lastSeen !== null && now - lastSeen <= STALE_AGENT_MS ? (stored as AgentStatus) : "offline";
    return {
      id: String(row.id),
      role: String(row.role ?? ""),
      model: String(row.model ?? ""),
      harness: String(row.harness ?? ""),
      parentId: row.parent_id ? String(row.parent_id) : null,
      status,
      storedStatus: stored,
      waitUntilMs: waitUntil,
      lastSeenMs: lastSeen,
      createdMs: Number(row.created_ms),
      authority: (row.authority as Authority | null) ?? null,
      meta: json<Record<string, unknown>>(row.meta_json, {}),
    };
  }

  getAgent(id: string): Agent | null {
    const row = this.db.prepare(`
      SELECT a.*, i.authority FROM agents a LEFT JOIN identities i ON i.agent_id = a.id WHERE a.id = ?
    `).get(id) as Row | undefined;
    return row ? this.toAgent(row) : null;
  }

  listAgents(): (Agent & { unread: number })[] {
    const rows = this.db.prepare(`
      SELECT a.*, i.authority,
        (SELECT COUNT(*) FROM messages m
          WHERE m.seq > COALESCE(c.last_seq, 0) AND (m.recipient = a.id OR (m.recipient IS NULL AND m.sender <> a.id))) AS unread
      FROM agents a LEFT JOIN identities i ON i.agent_id = a.id LEFT JOIN cursors c ON c.agent_id = a.id
      ORDER BY a.id
    `).all() as Row[];
    return rows.map((row) => ({ ...this.toAgent(row), unread: Number(row.unread) }));
  }

  whoami(actor: Identity): { agent: Agent | null; authority: Authority; unread: number; cursor: number; dbPath: string } {
    return { agent: this.getAgent(actor.agentId), authority: actor.authority, unread: this.unreadCount(actor.agentId), cursor: this.cursor(actor.agentId), dbPath: this.dbPath };
  }

  // ---------------------------------------------------------------- messages

  private toMessage(row: Row): Message {
    return {
      seq: Number(row.seq),
      id: String(row.id),
      tsMs: Number(row.ts_ms),
      sender: String(row.sender),
      recipient: row.recipient === null || row.recipient === undefined ? null : String(row.recipient),
      type: String(row.type) as MessageType,
      subject: String(row.subject ?? ""),
      body: String(row.body ?? ""),
      thread: String(row.thread ?? ""),
      taskId: num(row.task_id),
      refs: json<ContextReference[]>(row.refs_json, []),
      requiresAck: Number(row.requires_ack) === 1,
      source: String(row.source ?? "v2"),
    };
  }

  /** Insert one message inside the current transaction and queue the recipient's signal file. */
  private insertMessage(sender: string, recipient: string | null, fields: { type: MessageType; subject: string; body: string; thread: string; taskId: number | null; refs: ContextReference[]; requiresAck: boolean }): Message {
    const id = `msg_${randomUUID()}`;
    const ts = this.now();
    const result = this.db.prepare(`
      INSERT INTO messages(id, ts_ms, sender, recipient, type, subject, body, thread, task_id, refs_json, requires_ack)
      VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
    `).run(id, ts, sender, recipient, fields.type, fields.subject, fields.body, fields.thread, fields.taskId, JSON.stringify(fields.refs), fields.requiresAck ? 1 : 0);
    const seq = Number(result.lastInsertRowid);
    this.event(sender, "message", "message", seq, { recipient, type: fields.type, subject: fields.subject.slice(0, 200), taskId: fields.taskId });
    if (recipient) {
      this.pendingSignals.set(recipient, seq);
    } else {
      for (const row of this.db.prepare("SELECT id FROM agents WHERE id <> ?").all(sender) as Row[]) this.pendingSignals.set(String(row.id), seq);
    }
    return { seq, id, tsMs: ts, sender, recipient, ...fields, source: "v2" };
  }

  /** Atomically rewrite inbox/<agent>.seq for harness hooks and shell loops that cannot open SQLite. */
  private flushSignals(): void {
    if (!this.pendingSignals.size) return;
    const dir = join(this.home, "inbox");
    try {
      mkdirSync(dir, { recursive: true, mode: 0o700 });
      for (const [agentId, seq] of this.pendingSignals) {
        const path = join(dir, `${agentId}.seq`);
        const temporary = `${path}.${process.pid}.tmp`;
        writeFileSync(temporary, `${seq}\n`, { mode: 0o600 });
        renameSync(temporary, path);
      }
    } catch {
      // Signal files are a convenience; the database stays authoritative.
    } finally {
      this.pendingSignals.clear();
    }
  }

  send(actor: Identity, input: SendInput): Message[] {
    const to = String(input.to ?? "").trim();
    if (!to) throw new BusError("invalid", "recipient is required (id, a,b or *)");
    const recipients: (string | null)[] = to === "*" ? [null] : [...new Set(to.split(",").map((part) => part.trim()).filter(Boolean))];
    if (!recipients.length) throw new BusError("invalid", "recipient is required (id, a,b or *)");
    if (recipients.length > 50) throw new BusError("invalid", "at most 50 recipients");
    for (const recipient of recipients) if (recipient !== null) assertSafeAgentId(recipient);
    const type = input.type ?? "info";
    if (!MESSAGE_TYPES.includes(type)) throw new BusError("invalid", `invalid message type: ${type}`);
    const subject = boundedString(input.subject, "subject", LIMITS.subject);
    const body = boundedString(input.body, "body", LIMITS.body);
    if (!subject.trim() && !body.trim()) throw new BusError("invalid", "subject or body is required");
    const thread = boundedString(input.thread, "thread", LIMITS.thread);
    const refs = contextReferences(input.refs);
    const taskId = input.taskId === undefined || input.taskId === null ? null : Number(input.taskId);
    return this.write(() => {
      for (const recipient of recipients) {
        if (recipient !== null && !this.agentRow(recipient)) throw new BusError("not_found", `unknown recipient: ${recipient}`);
      }
      if (taskId !== null && !this.taskRow(taskId)) throw new BusError("not_found", `unknown task: ${taskId}`);
      const sent = recipients.map((recipient) => this.insertMessage(actor.agentId, recipient, {
        type, subject, body, thread: thread || (taskId !== null ? `task-${taskId}` : ""), taskId, refs, requiresAck: Boolean(input.requiresAck),
      }));
      this.touch(actor.agentId);
      return sent;
    });
  }

  cursor(agentId: string): number {
    const row = this.db.prepare("SELECT last_seq FROM cursors WHERE agent_id = ?").get(agentId) as { last_seq: number } | undefined;
    return Number(row?.last_seq ?? 0);
  }

  private unreadRows(agentId: string, limit: number): Row[] {
    return this.db.prepare(`
      SELECT * FROM messages
      WHERE seq > ? AND (recipient = ? OR (recipient IS NULL AND sender <> ?))
      ORDER BY seq LIMIT ?
    `).all(this.cursor(agentId), agentId, agentId, limit) as Row[];
  }

  unreadCount(agentId: string): number {
    const row = this.db.prepare(`
      SELECT COUNT(*) AS n FROM messages WHERE seq > ? AND (recipient = ? OR (recipient IS NULL AND sender <> ?))
    `).get(this.cursor(agentId), agentId, agentId) as { n: number };
    return Number(row.n);
  }

  /** New mail since the cursor. Advances the cursor unless peek is set. */
  inbox(actor: Identity, options: { peek?: boolean; limit?: number } = {}): { messages: Message[]; cursor: number; remaining: number } {
    const limit = Math.max(1, Math.min(LIMITS.inboxLimit, Math.floor(options.limit ?? 50)));
    const me = actor.agentId;
    if (options.peek) {
      const messages = this.unreadRows(me, limit).map((row) => this.toMessage(row));
      return { messages, cursor: this.cursor(me), remaining: this.unreadCount(me) - messages.length };
    }
    return this.write(() => {
      const messages = this.unreadRows(me, limit).map((row) => this.toMessage(row));
      if (messages.length) {
        const last = messages[messages.length - 1].seq;
        this.db.prepare(`
          INSERT INTO cursors(agent_id, last_seq) VALUES(?, ?)
          ON CONFLICT(agent_id) DO UPDATE SET last_seq = MAX(cursors.last_seq, excluded.last_seq)
        `).run(me, last);
        this.event(me, "inbox_read", "agent", me, { cursor: last, count: messages.length });
        this.touch(me);
      }
      return { messages, cursor: this.cursor(me), remaining: this.unreadCount(me) };
    });
  }

  ack(actor: Identity, seq: number): { seq: number; ackMs: number } {
    return this.write(() => {
      const row = this.db.prepare("SELECT * FROM messages WHERE seq = ?").get(seq) as Row | undefined;
      if (!row || !(row.recipient === actor.agentId || row.recipient === null)) throw new BusError("not_found", `no message ${seq} for ${actor.agentId}`);
      const ackMs = this.now();
      this.db.prepare("INSERT OR IGNORE INTO acks(seq, agent_id, ack_ms) VALUES(?, ?, ?)").run(seq, actor.agentId, ackMs);
      this.event(actor.agentId, "ack", "message", seq);
      this.touch(actor.agentId);
      return { seq, ackMs };
    });
  }

  getMessages(options: { sinceSeq?: number; limit?: number; thread?: string; taskId?: number } = {}): Message[] {
    const limit = Math.max(1, Math.min(1000, options.limit ?? 100));
    let rows: Row[];
    if (options.taskId !== undefined) rows = this.db.prepare("SELECT * FROM messages WHERE task_id = ? AND seq > ? ORDER BY seq LIMIT ?").all(options.taskId, options.sinceSeq ?? 0, limit) as Row[];
    else if (options.thread) rows = this.db.prepare("SELECT * FROM messages WHERE thread = ? AND seq > ? ORDER BY seq LIMIT ?").all(options.thread, options.sinceSeq ?? 0, limit) as Row[];
    else if (options.sinceSeq !== undefined) rows = this.db.prepare("SELECT * FROM messages WHERE seq > ? ORDER BY seq LIMIT ?").all(options.sinceSeq, limit) as Row[];
    else rows = (this.db.prepare("SELECT * FROM messages ORDER BY seq DESC LIMIT ?").all(limit) as Row[]).reverse();
    return rows.map((row) => this.toMessage(row));
  }

  // ----------------------------------------------------------------- waiting

  /**
   * Block until the caller has unread mail or a task event that concerns it.
   * Writes status='waiting' once at the start and 'idle' once at the end;
   * nothing is written while idle.
   */
  async waitForMail(actor: Identity, options: { timeoutMs: number; signal?: AbortSignal; watcher?: ChangeWatcher; watcherOptions?: ChangeWatcherOptions }): Promise<WaitResult> {
    const me = actor.agentId;
    const pending = this.inbox(actor, { peek: true, limit: 50 }).messages;
    if (pending.length) {
      // A waiter killed earlier (kill -9) can leave 'waiting' stored; clear it without writing otherwise.
      if (String(this.agentRow(me)?.status ?? "") === "waiting") {
        this.write(() => {
          this.db.prepare("UPDATE agents SET status = 'idle', wait_until_ms = NULL, last_seen_ms = ? WHERE id = ? AND status = 'waiting'").run(this.now(), me);
        });
      }
      return { status: "mail", messages: pending, events: [], seq: latestEventSeq(this.db) };
    }
    const timeoutMs = Math.max(0, options.timeoutMs);
    const deadline = Date.now() + timeoutMs;
    let since = this.write(() => {
      this.db.prepare("UPDATE agents SET status = 'waiting', wait_until_ms = ?, last_seen_ms = ? WHERE id = ?").run(this.now() + timeoutMs, this.now(), me);
      return this.event(me, "agent_waiting", "agent", me, { until: this.now() + timeoutMs });
    });
    const watcher = options.watcher ?? new ChangeWatcher(this.db, this.dbPath, options.watcherOptions);
    let result: WaitResult = { status: "timeout", messages: [], events: [], seq: since };
    try {
      while (!options.signal?.aborted) {
        const remaining = deadline - Date.now();
        if (remaining <= 0) break;
        const seq = await watcher.next(since, remaining, options.signal);
        if (seq <= since) break;
        const messages = this.inbox(actor, { peek: true, limit: 50 }).messages;
        if (messages.length) { result = { status: "mail", messages, events: [], seq }; break; }
        const events = this.taskEventsFor(me, since, seq);
        if (events.length) { result = { status: "task", messages: [], events, seq }; break; }
        since = seq;
        result.seq = seq;
      }
    } finally {
      if (!options.watcher) watcher.close();
      if (this.db.isOpen) {
        const reason = result.status;
        this.write(() => {
          this.db.prepare("UPDATE agents SET status = 'idle', wait_until_ms = NULL, last_seen_ms = ? WHERE id = ? AND status = 'waiting'").run(this.now(), me);
          this.event(me, "agent_idle", "agent", me, { reason });
        });
      }
    }
    return result;
  }

  /** Task events after `afterSeq` by someone else on a task this agent owns, reviews, or could claim. */
  taskEventsFor(agentId: string, afterSeq: number, uptoSeq = Number.MAX_SAFE_INTEGER): BusEvent[] {
    const role = String(this.agentRow(agentId)?.role ?? "");
    const rows = this.db.prepare(`
      SELECT e.* FROM events e JOIN tasks t ON e.entity = 'task' AND t.id = CAST(e.entity_id AS INTEGER)
      WHERE e.seq > ? AND e.seq <= ? AND e.actor <> ? AND e.source = 'v2'
        AND (t.assignee = ? OR t.reviewer = ? OR (t.reviewer IS NULL AND t.creator = ?)
             OR (t.assignee IS NULL AND t.state = 'open' AND (t.role = '' OR t.role = ?)))
      ORDER BY e.seq
    `).all(afterSeq, uptoSeq, agentId, agentId, agentId, agentId, role) as Row[];
    return rows.map((row) => this.toEvent(row));
  }

  // ------------------------------------------------------------------ events

  private toEvent(row: Row): BusEvent {
    return {
      seq: Number(row.seq),
      tsMs: Number(row.ts_ms),
      actor: String(row.actor),
      kind: String(row.kind),
      entity: String(row.entity),
      entityId: String(row.entity_id),
      data: json<Record<string, unknown>>(row.data_json, {}),
      source: String(row.source ?? "v2"),
    };
  }

  latestSeq(): number {
    return latestEventSeq(this.db);
  }

  events(sinceSeq = 0, limit = 200): BusEvent[] {
    const rows = this.db.prepare("SELECT * FROM events WHERE seq > ? ORDER BY seq LIMIT ?").all(sinceSeq, Math.max(1, Math.min(5000, limit))) as Row[];
    return rows.map((row) => this.toEvent(row));
  }

  // ------------------------------------------------------------------- tasks

  private taskRow(id: number): Row | undefined {
    return this.db.prepare("SELECT * FROM tasks WHERE id = ?").get(id) as Row | undefined;
  }

  private toTask(row: Row): Task {
    const id = Number(row.id);
    const deps = this.db.prepare("SELECT depends_on FROM task_deps WHERE task_id = ? ORDER BY depends_on").all(id) as Row[];
    return {
      id,
      legacyId: row.legacy_id ? String(row.legacy_id) : null,
      project: row.project ? String(row.project) : null,
      parentId: num(row.parent_id),
      title: String(row.title),
      brief: String(row.brief ?? ""),
      acceptance: String(row.acceptance ?? ""),
      role: String(row.role ?? ""),
      priority: String(row.priority ?? "normal"),
      state: String(row.state) as TaskState,
      creator: String(row.creator),
      assignee: row.assignee ? String(row.assignee) : null,
      reviewer: row.reviewer ? String(row.reviewer) : null,
      pathScopes: json<string[]>(row.path_scopes_json, []),
      refs: json<ContextReference[]>(row.refs_json, []),
      result: json<TaskResult | null>(row.result_json, null),
      review: json<TaskReview | null>(row.review_json, null),
      round: Number(row.round),
      attempts: Number(row.attempts),
      maxRetries: Number(row.max_retries),
      claimExpiresMs: num(row.claim_expires_ms),
      createdMs: Number(row.created_ms),
      updatedMs: Number(row.updated_ms),
      dependencies: deps.map((dep) => Number(dep.depends_on)),
    };
  }

  private requireTask(id: number): Task {
    if (!Number.isInteger(id) || id <= 0) throw new BusError("invalid", `invalid task id: ${id}`);
    const row = this.taskRow(id);
    if (!row) throw new BusError("not_found", `unknown task: ${id}`);
    return this.toTask(row);
  }

  getTask(id: number): TaskDetail {
    const task = this.requireTask(id);
    const notes = (this.db.prepare("SELECT * FROM task_notes WHERE task_id = ? ORDER BY id").all(id) as Row[]).map((row): TaskNote => ({
      id: Number(row.id), taskId: Number(row.task_id), author: String(row.author), tsMs: Number(row.ts_ms), body: String(row.body),
    }));
    const dependents = (this.db.prepare("SELECT task_id FROM task_deps WHERE depends_on = ? ORDER BY task_id").all(id) as Row[]).map((row) => Number(row.task_id));
    const leases = (this.db.prepare("SELECT path FROM leases WHERE task_id = ? ORDER BY path").all(id) as Row[]).map((row) => String(row.path));
    return { ...task, notes, dependents, messages: this.getMessages({ taskId: id, limit: 1000 }), leases };
  }

  /** Summary columns for the given task ids, without deps, notes or messages (dashboard deltas). */
  taskSummaries(ids: number[]): TaskSummary[] {
    const wanted = [...new Set(ids.map(Number))].filter((id) => Number.isInteger(id) && id > 0).slice(0, 500);
    if (!wanted.length) return [];
    const rows = this.db.prepare(`SELECT id, title, assignee, state, created_ms, updated_ms FROM tasks WHERE id IN (${placeholders(wanted.length)}) ORDER BY id`)
      .all(...wanted) as Row[];
    return rows.map((row) => ({
      id: Number(row.id), title: String(row.title), assignee: row.assignee ? String(row.assignee) : null,
      state: String(row.state) as TaskState, createdMs: Number(row.created_ms), updatedMs: Number(row.updated_ms),
    }));
  }

  listTasks(input: ListTasksInput = {}): Task[] {
    const where: string[] = [];
    const args: (string | number)[] = [];
    if (input.states?.length) {
      for (const state of input.states) if (!TASK_STATES.includes(state)) throw new BusError("invalid", `invalid task state: ${state}`);
      where.push(`state IN (${placeholders(input.states.length)})`);
      args.push(...input.states);
    } else if (!input.includeClosed) {
      where.push(`state NOT IN (${placeholders(CLOSED_STATES.length)})`);
      args.push(...CLOSED_STATES);
    }
    if (input.mine) {
      where.push("(assignee = ? OR creator = ? OR reviewer = ?)");
      args.push(input.mine, input.mine, input.mine);
    }
    const limit = Math.max(1, Math.min(1000, input.limit ?? 200));
    const sql = `SELECT * FROM tasks ${where.length ? `WHERE ${where.join(" AND ")}` : ""} ORDER BY id LIMIT ?`;
    return (this.db.prepare(sql).all(...args, limit) as Row[]).map((row) => this.toTask(row));
  }

  /**
   * Claimed tasks with no claim/note activity for `stallMs` — a probably-dead claim.
   * `updated_ms` moves on claim and on every note, so it is the last-activity clock.
   */
  stalledTasks(stallMs: number): Task[] {
    const cutoff = this.now() - Math.max(0, stallMs);
    return (this.db.prepare("SELECT * FROM tasks WHERE state = 'claimed' AND updated_ms < ? ORDER BY id").all(cutoff) as Row[]).map((row) => this.toTask(row));
  }

  /** Reopen claims past their expiry. There is no sweeper process; every task write calls this first. */
  private reopenExpiredClaims(): void {
    const now = this.now();
    const expired = this.db.prepare("SELECT id, assignee FROM tasks WHERE state = 'claimed' AND claim_expires_ms IS NOT NULL AND claim_expires_ms < ?").all(now) as Row[];
    for (const row of expired) {
      const id = Number(row.id);
      const created = this.db.prepare("SELECT data_json FROM events WHERE entity = 'task' AND entity_id = ? AND kind = 'task_created' ORDER BY seq LIMIT 1").get(String(id)) as Row | undefined;
      const preassigned = json<{ assignee?: string | null }>(created?.data_json, {}).assignee ?? null;
      this.db.prepare("UPDATE tasks SET state = 'open', assignee = ?, claim_expires_ms = NULL, updated_ms = ? WHERE id = ?").run(preassigned, now, id);
      this.db.prepare("DELETE FROM leases WHERE task_id = ?").run(id);
      this.event("system", "claim_expired", "task", id, { previousAssignee: row.assignee ?? null });
    }
  }

  createTask(actor: Identity, input: CreateTaskInput): Task {
    const title = boundedString(input.title, "title", LIMITS.title, true);
    const brief = boundedString(input.brief, "brief", LIMITS.brief);
    const acceptance = boundedString(input.acceptance, "acceptance", LIMITS.acceptance);
    const role = boundedString(input.role, "role", LIMITS.role);
    const priority = input.priority ?? "normal";
    if (!PRIORITIES.includes(priority)) throw new BusError("invalid", `invalid priority: ${priority}`);
    const to = input.to ? assertSafeAgentId(input.to) : null;
    const reviewer = input.reviewer ? assertSafeAgentId(input.reviewer) : null;
    const refs = contextReferences(input.refs);
    const dependencies = [...new Set((input.dependencies ?? []).map(Number))];
    const project = input.project ? resolve(boundedString(input.project, "project", LIMITS.path)) : null;
    const rawScopes = (input.pathScopes ?? []).map((scope) => boundedString(scope, "path scope", LIMITS.path, true));
    if (rawScopes.length && !project) throw new BusError("invalid", "path scopes need a project directory");
    const pathScopes = project ? [...new Set(rawScopes.map((scope) => normalizeScope(project, scope)))].sort() : [];
    const maxRetries = input.maxRetries ?? 2;
    return this.write(() => {
      this.reopenExpiredClaims();
      const now = this.now();
      if (to && !this.agentRow(to)) throw new BusError("not_found", `unknown assignee: ${to}`);
      if (reviewer && !this.agentRow(reviewer)) throw new BusError("not_found", `unknown reviewer: ${reviewer}`);
      if (input.parentId !== undefined && input.parentId !== null) this.requireTask(Number(input.parentId));
      let blocked = false;
      for (const dep of dependencies) blocked = this.requireTask(dep).state !== "accepted" || blocked;
      const state: TaskState = blocked ? "blocked" : "open";
      const result = this.db.prepare(`
        INSERT INTO tasks(project, parent_id, title, brief, acceptance, role, priority, state, creator, assignee, reviewer,
          path_scopes_json, refs_json, max_retries, created_ms, updated_ms)
        VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
      `).run(project, input.parentId ?? null, title, brief, acceptance, role, priority, state, actor.agentId, to, reviewer,
        JSON.stringify(pathScopes), JSON.stringify(refs), maxRetries, now, now);
      const id = Number(result.lastInsertRowid);
      const insertDep = this.db.prepare("INSERT OR IGNORE INTO task_deps(task_id, depends_on) VALUES(?, ?)");
      for (const dep of dependencies) insertDep.run(id, dep);
      this.event(actor.agentId, "task_created", "task", id, { title: title.slice(0, 200), assignee: to, role, state });
      if (to && to !== actor.agentId) {
        const body = [brief, acceptance ? `Acceptance:\n${acceptance}` : "", `Claim with \`qagent task claim ${id}\`, submit with \`qagent task submit ${id} --summary ...\`.`].filter(Boolean).join("\n\n");
        this.insertMessage(actor.agentId, to, { type: "task", subject: `[TASK #${id}] ${title}`, body, thread: `task-${id}`, taskId: id, refs, requiresAck: false });
      }
      this.touch(actor.agentId);
      return this.requireTask(id);
    });
  }

  /**
   * Claim a task atomically. The claim is one UPDATE ... WHERE state IN ('open','changes_requested')
   * AND (assignee IS NULL OR assignee = me) RETURNING; it wins only if that row came back.
   * Without an id, the oldest claimable task assigned to me, or unassigned for my role, is taken.
   */
  claimTask(actor: Identity, taskId?: number | null): Task {
    const me = actor.agentId;
    const explicit = taskId !== undefined && taskId !== null;
    return this.write(() => {
      this.reopenExpiredClaims();
      let candidates: number[];
      if (explicit) {
        candidates = [this.requireTask(Number(taskId)).id];
      } else {
        const role = String(this.agentRow(me)?.role ?? "");
        candidates = (this.db.prepare(`
          SELECT id FROM tasks WHERE state IN ('open', 'changes_requested')
            AND (assignee = ? OR (assignee IS NULL AND (role = '' OR role = ?)))
          ORDER BY CASE WHEN assignee = ? THEN 0 ELSE 1 END, id LIMIT 100
        `).all(me, role, me) as Row[]).map((row) => Number(row.id));
      }
      for (const id of candidates) {
        const current = this.requireTask(id);
        const conflicts = this.leaseConflicts(current);
        if (conflicts.length) {
          if (explicit) throw new BusError("conflict", `task ${id} path scopes overlap leases held by ${conflicts.map((c) => `#${c.taskId}:${c.path}`).join(", ")}`);
          continue;
        }
        const now = this.now();
        const row = this.db.prepare(`
          UPDATE tasks SET state = 'claimed', assignee = ?, claim_expires_ms = ?, updated_ms = ?
          WHERE id = ? AND state IN ('open', 'changes_requested') AND (assignee IS NULL OR assignee = ?)
          RETURNING *
        `).get(me, now + this.claimTtlMs, now, id, me) as Row | undefined;
        if (!row) {
          if (explicit) {
            const reason = current.assignee && current.assignee !== me ? `is ${current.state} and assigned to ${current.assignee}` : `is ${current.state}`;
            throw new BusError("conflict", `task ${id} cannot be claimed: it ${reason}`);
          }
          continue;
        }
        const task = this.toTask(row);
        if (task.project) {
          const insert = this.db.prepare("INSERT OR REPLACE INTO leases(project, path, task_id, created_ms) VALUES(?, ?, ?, ?)");
          for (const path of task.pathScopes) insert.run(task.project, path, id, now);
        }
        this.event(me, "task_claimed", "task", id, { round: task.round, leases: task.pathScopes });
        this.touch(me, "working");
        return task;
      }
      throw new BusError("not_found", "no claimable task");
    });
  }

  private leaseConflicts(task: Task): { taskId: number; path: string }[] {
    if (!task.project || !task.pathScopes.length) return [];
    const active = this.db.prepare("SELECT task_id, path FROM leases WHERE project = ? AND task_id <> ?").all(task.project, task.id) as Row[];
    const conflicts: { taskId: number; path: string }[] = [];
    for (const wanted of task.pathScopes) {
      for (const lease of active) if (scopesOverlap(wanted, String(lease.path))) conflicts.push({ taskId: Number(lease.task_id), path: String(lease.path) });
    }
    return conflicts;
  }

  noteTask(actor: Identity, taskId: number, text: string): TaskNote {
    const body = boundedString(text, "note", LIMITS.note, true);
    return this.write(() => {
      this.reopenExpiredClaims();
      const task = this.requireTask(taskId);
      const now = this.now();
      const result = this.db.prepare("INSERT INTO task_notes(task_id, author, ts_ms, body) VALUES(?, ?, ?, ?)").run(task.id, actor.agentId, now, body);
      if (task.state === "claimed" && task.assignee === actor.agentId) {
        this.db.prepare("UPDATE tasks SET claim_expires_ms = ?, updated_ms = ? WHERE id = ?").run(now + this.claimTtlMs, now, task.id);
      } else {
        this.db.prepare("UPDATE tasks SET updated_ms = ? WHERE id = ?").run(now, task.id);
      }
      const id = Number(result.lastInsertRowid);
      this.event(actor.agentId, "task_note", "task", task.id, { noteId: id });
      this.touch(actor.agentId);
      return { id, taskId: task.id, author: actor.agentId, tsMs: now, body };
    });
  }

  submitTask(actor: Identity, taskId: number, input: SubmitInput): Task {
    const summary = boundedString(input.summary, "summary", LIMITS.summary, true);
    const details = boundedString(input.details, "details", LIMITS.details);
    const changedFiles = [...new Set((input.changedFiles ?? []).map((file) => boundedString(file, "changed file", LIMITS.refValue, true).trim()))];
    if (changedFiles.length > LIMITS.changedFiles) throw new BusError("invalid", `at most ${LIMITS.changedFiles} changed files`);
    const artifacts = contextReferences(input.artifacts);
    const validationInput = input.validation === undefined || input.validation === null ? [] : input.validation;
    if (!Array.isArray(validationInput)) throw new BusError("invalid", "validation must be a list");
    if (validationInput.length > LIMITS.validation) throw new BusError("invalid", `at most ${LIMITS.validation} validation entries`);
    const validation: ValidationObservation[] = validationInput.map((item) => {
      const row = item && typeof item === "object" ? item as Record<string, unknown> : { summary: String(item) };
      const entry: ValidationObservation = { passed: Boolean(row.passed), summary: boundedString(row.summary, "validation summary", 4096, true) };
      if (row.command) entry.command = boundedString(row.command, "validation command", 8192);
      return entry;
    });
    return this.write(() => {
      this.reopenExpiredClaims();
      const task = this.requireTask(taskId);
      if (task.assignee !== actor.agentId) throw new BusError("forbidden", `task ${task.id} is assigned to ${task.assignee ?? "nobody"}, not ${actor.agentId}`);
      if (task.state !== "claimed" && task.state !== "changes_requested") throw new BusError("conflict", `task ${task.id} is ${task.state}; claim it before submitting`);
      const now = this.now();
      const result: TaskResult = { summary, details, changedFiles, artifacts, validation, completedMs: now };
      this.db.prepare("UPDATE tasks SET state = 'submitted', result_json = ?, claim_expires_ms = NULL, updated_ms = ? WHERE id = ?")
        .run(JSON.stringify(result), now, task.id);
      const reviewer = task.reviewer ?? task.creator;
      this.event(actor.agentId, "task_submitted", "task", task.id, { round: task.round, reviewer });
      if (reviewer !== actor.agentId) {
        const body = [
          summary,
          details ? `Details:\n${details}` : "",
          changedFiles.length ? `Changed files:\n${changedFiles.map((file) => `- ${file}`).join("\n")}` : "",
          validation.length ? `Validation:\n${validation.map((v) => `- ${v.passed ? "passed" : "failed"}: ${v.summary}${v.command ? ` (${v.command})` : ""}`).join("\n")}` : "",
          `Review with \`qagent task review ${task.id} --accept|--revise --feedback ...\`.`,
        ].filter(Boolean).join("\n\n");
        this.insertMessage(actor.agentId, reviewer, { type: "result", subject: `[DONE #${task.id} r${task.round}] ${task.title}`, body, thread: `task-${task.id}`, taskId: task.id, refs: artifacts, requiresAck: false });
      }
      this.touch(actor.agentId, "idle");
      return this.requireTask(task.id);
    });
  }

  reviewTask(actor: Identity, taskId: number, input: { accepted: boolean; feedback: string }): Task {
    const feedback = boundedString(input.feedback, "feedback", LIMITS.feedback, true);
    return this.write(() => {
      this.reopenExpiredClaims();
      const task = this.requireTask(taskId);
      const reviewer = task.reviewer ?? task.creator;
      const operator = actor.authority === "operator";
      if (!operator && actor.agentId !== reviewer) throw new BusError("forbidden", `only ${reviewer} or the operator may review task ${task.id}`);
      if (!operator && actor.agentId === task.assignee) throw new BusError("forbidden", `${actor.agentId} cannot review its own work on task ${task.id}`);
      if (task.state !== "submitted") throw new BusError("conflict", `task ${task.id} is ${task.state}, not submitted`);
      const now = this.now();
      const review: TaskReview = { reviewer: actor.agentId, accepted: Boolean(input.accepted), feedback, reviewedMs: now };
      const thread = `task-${task.id}`;
      if (review.accepted) {
        this.db.prepare("UPDATE tasks SET state = 'accepted', review_json = ?, updated_ms = ? WHERE id = ?").run(JSON.stringify(review), now, task.id);
        this.db.prepare("DELETE FROM leases WHERE task_id = ?").run(task.id);
        this.event(actor.agentId, "task_accepted", "task", task.id, { round: task.round });
        if (task.assignee && task.assignee !== actor.agentId) {
          this.insertMessage(actor.agentId, task.assignee, { type: "feedback", subject: `[ACCEPTED #${task.id}] ${task.title}`, body: `${feedback}\n\nNo further action is required on this task.`, thread, taskId: task.id, refs: [], requiresAck: false });
        }
        this.unblockDependents(task.id, actor.agentId);
      } else {
        const round = task.round + 1;
        if (round - 1 > task.maxRetries) {
          this.db.prepare("UPDATE tasks SET state = 'failed', round = ?, review_json = ?, claim_expires_ms = NULL, updated_ms = ? WHERE id = ?").run(round, JSON.stringify(review), now, task.id);
          this.db.prepare("DELETE FROM leases WHERE task_id = ?").run(task.id);
          this.event(actor.agentId, "task_failed", "task", task.id, { round, reason: "review retry limit exceeded" });
          if (task.creator !== actor.agentId) {
            this.insertMessage(actor.agentId, task.creator, { type: "control", subject: `[ESCALATE #${task.id}] review retry limit exceeded`, body: feedback, thread, taskId: task.id, refs: [], requiresAck: false });
          }
        } else {
          this.db.prepare("UPDATE tasks SET state = 'changes_requested', round = ?, review_json = ?, claim_expires_ms = NULL, updated_ms = ? WHERE id = ?").run(round, JSON.stringify(review), now, task.id);
          this.event(actor.agentId, "task_changes_requested", "task", task.id, { round });
          if (task.assignee) {
            this.insertMessage(actor.agentId, task.assignee, { type: "feedback", subject: `[CHANGES #${task.id} r${round}] ${task.title}`, body: `${feedback}\n\nRevise the existing work and submit the same task again.`, thread, taskId: task.id, refs: [], requiresAck: false });
          }
        }
      }
      this.touch(actor.agentId);
      return this.requireTask(task.id);
    });
  }

  private unblockDependents(taskId: number, actor: string): void {
    const dependents = this.db.prepare(`
      SELECT t.id FROM task_deps d JOIN tasks t ON t.id = d.task_id WHERE d.depends_on = ? AND t.state = 'blocked'
    `).all(taskId) as Row[];
    for (const row of dependents) {
      const id = Number(row.id);
      const open = this.db.prepare(`
        SELECT COUNT(*) AS n FROM task_deps d JOIN tasks t ON t.id = d.depends_on WHERE d.task_id = ? AND t.state <> 'accepted'
      `).get(id) as { n: number };
      if (Number(open.n) === 0) {
        this.db.prepare("UPDATE tasks SET state = 'open', updated_ms = ? WHERE id = ?").run(this.now(), id);
        this.event(actor, "task_unblocked", "task", id, { releasedBy: taskId });
      }
    }
  }

  /**
   * Give a claimed task back without failing it: state 'open', the creator's original
   * assignee (or nobody) restored, leases released. The assignee or the operator may release.
   */
  releaseTask(actor: Identity, taskId: number, reason?: string): Task {
    const text = boundedString(reason, "reason", LIMITS.reason) || "released";
    return this.write(() => {
      this.reopenExpiredClaims();
      const task = this.requireTask(taskId);
      if (actor.authority !== "operator" && actor.agentId !== task.assignee) throw new BusError("forbidden", `only ${task.assignee ?? "the assignee"} or the operator may release task ${task.id}`);
      if (task.state !== "claimed") throw new BusError("conflict", `task ${task.id} is ${task.state}, not claimed`);
      const created = this.db.prepare("SELECT data_json FROM events WHERE entity = 'task' AND entity_id = ? AND kind = 'task_created' ORDER BY seq LIMIT 1").get(String(task.id)) as Row | undefined;
      const preassigned = json<{ assignee?: string | null }>(created?.data_json, {}).assignee ?? null;
      const now = this.now();
      this.db.prepare("UPDATE tasks SET state = 'open', assignee = ?, claim_expires_ms = NULL, updated_ms = ? WHERE id = ?").run(preassigned, now, task.id);
      this.db.prepare("DELETE FROM leases WHERE task_id = ?").run(task.id);
      this.event(actor.agentId, "task_released", "task", task.id, { reason: text.slice(0, 500), previousAssignee: task.assignee });
      this.touch(actor.agentId, "idle");
      return this.requireTask(task.id);
    });
  }

  /**
   * Report that work on a claimed task failed (port of /task/failure, broker.ts:1214-1283, without
   * rerouting). The attempt count rises; within max_retries the task reopens for the same assignee
   * with a retry message, beyond it the task fails and the creator gets an escalation.
   */
  failTask(actor: Identity, taskId: number, error: string): Task {
    const note = boundedString(error, "failure", LIMITS.note, true);
    return this.write(() => {
      this.reopenExpiredClaims();
      const task = this.requireTask(taskId);
      if (actor.authority !== "operator" && actor.agentId !== task.assignee) throw new BusError("forbidden", `only ${task.assignee ?? "the assignee"} or the operator may report failure for task ${task.id}`);
      if (task.state !== "claimed") throw new BusError("conflict", `task ${task.id} is ${task.state}, not claimed`);
      const now = this.now();
      const attempts = task.attempts + 1;
      const thread = `task-${task.id}`;
      this.db.prepare("INSERT INTO task_notes(task_id, author, ts_ms, body) VALUES(?, ?, ?, ?)").run(task.id, actor.agentId, now, `failure (attempt ${attempts}): ${note}`);
      this.db.prepare("DELETE FROM leases WHERE task_id = ?").run(task.id);
      if (attempts <= task.maxRetries) {
        this.db.prepare("UPDATE tasks SET state = 'open', attempts = ?, claim_expires_ms = NULL, updated_ms = ? WHERE id = ?").run(attempts, now, task.id);
        this.event(actor.agentId, "task_retry", "task", task.id, { attempts, maxRetries: task.maxRetries });
        if (task.assignee) {
          this.insertMessage("system", task.assignee, { type: "task", subject: `[RETRY #${task.id}] attempt ${attempts + 1}: ${task.title}`, body: `${note}\n\nRetry the original scoped task. Do not broaden scope.`, thread, taskId: task.id, refs: task.refs, requiresAck: false });
        }
      } else {
        this.db.prepare("UPDATE tasks SET state = 'failed', attempts = ?, claim_expires_ms = NULL, updated_ms = ? WHERE id = ?").run(attempts, now, task.id);
        this.event(actor.agentId, "task_failed", "task", task.id, { attempts, reason: "retry limit exceeded" });
        if (task.creator !== actor.agentId) {
          this.insertMessage(actor.agentId, task.creator, { type: "control", subject: `[ESCALATE #${task.id}] attempts exhausted`, body: note, thread, taskId: task.id, refs: [], requiresAck: false });
        }
      }
      this.touch(actor.agentId, "idle");
      return this.requireTask(task.id);
    });
  }

  cancelTask(actor: Identity, taskId: number, reason?: string): Task {
    const text = boundedString(reason, "reason", LIMITS.reason) || "cancelled";
    return this.write(() => {
      const task = this.requireTask(taskId);
      if (actor.authority !== "operator" && actor.agentId !== task.creator) throw new BusError("forbidden", `only ${task.creator} or the operator may cancel task ${task.id}`);
      if (CLOSED_STATES.includes(task.state)) throw new BusError("conflict", `task ${task.id} is already ${task.state}`);
      const now = this.now();
      this.db.prepare("UPDATE tasks SET state = 'cancelled', claim_expires_ms = NULL, updated_ms = ? WHERE id = ?").run(now, task.id);
      this.db.prepare("DELETE FROM leases WHERE task_id = ?").run(task.id);
      this.event(actor.agentId, "task_cancelled", "task", task.id, { reason: text.slice(0, 500) });
      if (task.assignee && task.assignee !== actor.agentId) {
        this.insertMessage(actor.agentId, task.assignee, { type: "control", subject: `[CANCELLED #${task.id}] ${task.title}`, body: text, thread: `task-${task.id}`, taskId: task.id, refs: [], requiresAck: false });
      }
      this.touch(actor.agentId);
      return this.requireTask(task.id);
    });
  }

  // ------------------------------------------------------------------ status

  status(): { dbPath: string; seq: number; agents: (Agent & { unread: number })[]; counts: Record<string, number>; openTasks: Task[] } {
    const counts: Record<string, number> = {};
    for (const row of this.db.prepare("SELECT state, COUNT(*) AS n FROM tasks GROUP BY state").all() as Row[]) counts[String(row.state)] = Number(row.n);
    return { dbPath: this.dbPath, seq: this.latestSeq(), agents: this.listAgents(), counts, openTasks: this.listTasks({ limit: 200 }) };
  }
}
