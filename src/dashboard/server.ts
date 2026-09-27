/**
 * The optional dashboard process: one page, one SSE stream per tab, one write.
 *
 * Cost does not grow with tabs. The process holds one database connection and
 * one ChangeWatcher. A single loop waits in changes.next() (fs.watch wake-ups,
 * plus a PRAGMA data_version check that backs off from 25 ms after a change to
 * the safety interval) and, on a change, runs one delta read that is written to
 * every open stream. With no writes the loop settles at one data_version read
 * per safety interval, whatever the number of tabs, and stops entirely when no
 * tab is open.
 */
import { randomBytes } from "node:crypto";
import { existsSync } from "node:fs";
import { createServer, type IncomingMessage, type Server, type ServerResponse } from "node:http";
import type { AddressInfo } from "node:net";
import { Bus } from "../core/bus.js";
import { ChangeWatcher } from "../core/changes.js";
import { identityForToken } from "../core/identity.js";
import { BusError, CLOSED_STATES, type Agent, type Message, OPERATOR_ID } from "../core/types.js";
import { type AgentView, type MessageView, renderPage, renderSignedOut, type TaskView, type ViewState } from "./page.js";
import { AuthError, requireLoopbackHost, requireSameOrigin, requireSession, Sessions, sessionOf } from "./session.js";

export const DEFAULT_PORT = 11511;
export const HOST = "127.0.0.1";
export const DEFAULT_SAFETY_MS = 10_000;
export const DEFAULT_PING_MS = 25_000;
const LOOP_TIMEOUT_MS = 24 * 60 * 60_000;
/**
 * First re-check after a change. fs.watch on macOS can fold a second write that lands within tens of
 * milliseconds of the first into one event, so each next() call starts polling data_version at this
 * interval and doubles it up to the safety interval (25, 50, 100 ... 10 000 ms). A burst is caught
 * within about 100 ms; an idle bus settles at one read per safety interval about 20 s after its last write.
 */
const FAST_POLL_MS = 25;
const MAX_DELTA_EVENTS = 1000;
const MAX_STREAMS = 32;
const MAX_BODY_BYTES = 512 * 1024;

export interface DashboardOptions {
  dbPath: string;
  /** 0 picks a free port (tests). Default 11511. The host is always 127.0.0.1. */
  port?: number;
  /** Safety check interval for missed file events. Default 10 s. */
  safetyMs?: number;
  pingMs?: number;
  sessionTtlMs?: number;
  ticketTtlMs?: number;
}

/** Instrumentation for the idle-cost test: every database read this process makes. */
export interface DashboardStats {
  queries: number;
  deltas: number;
  streams: number;
}

export interface DashboardHandle {
  url: string;
  port: number;
  stats: DashboardStats;
  /** A single-use sign-in address. The caller has already proved it holds the operator token. */
  signInUrl(): string;
  close(): Promise<void>;
}

/** A ChangeWatcher that counts the reads it makes. */
class CountingWatcher extends ChangeWatcher {
  constructor(bus: Bus, private readonly stats: DashboardStats, safetyMs: number) {
    super(bus.db, bus.dbPath, { minPollMs: Math.min(FAST_POLL_MS, safetyMs), maxPollMs: safetyMs });
  }
  override dataVersion(): number { this.stats.queries += 1; return super.dataVersion(); }
  override currentSeq(): number { this.stats.queries += 1; return super.currentSeq(); }
}

function agentView(agent: Agent): AgentView {
  return { id: agent.id, status: agent.storedStatus, waitUntilMs: agent.waitUntilMs, lastSeenMs: agent.lastSeenMs };
}

function messageView(message: Message): MessageView {
  const first = (message.subject.trim() || message.body.trim()).split("\n")[0] ?? "";
  return { seq: message.seq, tsMs: message.tsMs, sender: message.sender, recipient: message.recipient, line: first.slice(0, 300) };
}

interface Delta {
  seq: number;
  lastChangeMs: number | null;
  events: { seq: number; tsMs: number; actor: string; kind: string; entity: string; entityId: string }[];
  agents: AgentView[];
  tasks: TaskView[];
  messages: MessageView[];
}

class Reader {
  constructor(private readonly bus: Bus, private readonly stats: DashboardStats) {}

  state(): ViewState {
    this.stats.queries += 5;
    const seq = this.bus.latestSeq();
    const last = seq > 0 ? this.bus.events(seq - 1, 1)[0] : undefined;
    return {
      dbPath: this.bus.dbPath,
      seq,
      lastChangeMs: last?.tsMs ?? null,
      agents: this.bus.listAgents().filter((agent) => agent.id !== OPERATOR_ID).map(agentView),
      tasks: this.bus.listTasks({ limit: 200 }).map((task) => ({
        id: task.id, title: task.title, assignee: task.assignee, state: task.state, createdMs: task.createdMs, closed: false,
      })),
      messages: this.bus.getMessages({ limit: 100 }).map(messageView).reverse(),
    };
  }

  /** Rows named by events (from, to]. Null when the gap is too large for a delta; the page then reloads. */
  delta(from: number, to: number): Delta | null {
    this.stats.queries += 1;
    const events = this.bus.events(from, MAX_DELTA_EVENTS).filter((event) => event.seq <= to);
    if (events.length === MAX_DELTA_EVENTS && events[events.length - 1].seq < to) return null;
    const agentIds = new Set<string>();
    const taskIds = new Set<number>();
    const messageSeqs = new Set<number>();
    for (const event of events) {
      agentIds.add(event.actor);
      if (event.entity === "agent") agentIds.add(event.entityId);
      else if (event.entity === "task") taskIds.add(Number(event.entityId));
      else if (event.entity === "message") messageSeqs.add(Number(event.entityId));
    }
    agentIds.delete(OPERATOR_ID);
    agentIds.delete("system");
    const agents: AgentView[] = [];
    for (const id of agentIds) {
      this.stats.queries += 1;
      const agent = this.bus.getAgent(id);
      if (agent) agents.push(agentView(agent));
    }
    let tasks: TaskView[] = [];
    const ids = [...taskIds].filter((id) => Number.isInteger(id) && id > 0).slice(0, 500);
    if (ids.length) {
      this.stats.queries += 1;
      tasks = this.bus.taskSummaries(ids).map((task) => ({
        id: task.id, title: task.title, assignee: task.assignee, state: task.state,
        createdMs: task.createdMs, closed: CLOSED_STATES.includes(task.state),
      }));
    }
    let messages: MessageView[] = [];
    if (messageSeqs.size) {
      this.stats.queries += 1;
      const seqs = [...messageSeqs].sort((a, b) => a - b);
      const newest = seqs.slice(-100);
      messages = this.bus.getMessages({ sinceSeq: newest[0] - 1, limit: newest[newest.length - 1] - newest[0] + 1 })
        .filter((message) => messageSeqs.has(message.seq)).map(messageView);
    }
    return {
      seq: to,
      lastChangeMs: events.length ? events[events.length - 1].tsMs : null,
      events: events.map((event) => ({ seq: event.seq, tsMs: event.tsMs, actor: event.actor, kind: event.kind, entity: event.entity, entityId: event.entityId })),
      agents,
      tasks,
      messages,
    };
  }
}

/** Fans one change loop out to every open SSE stream. */
class Hub {
  private readonly streams = new Set<ServerResponse>();
  private seq: number;
  private running = false;
  private abort: AbortController | null = null;
  private ping: NodeJS.Timeout | null = null;
  private closed = false;

  constructor(private readonly watcher: ChangeWatcher, private readonly reader: Reader, private readonly stats: DashboardStats, private readonly pingMs: number) {
    this.seq = watcher.currentSeq();
  }

  get size(): number { return this.streams.size; }

  add(req: IncomingMessage, res: ServerResponse, since: number): void {
    res.writeHead(200, {
      "content-type": "text/event-stream; charset=utf-8",
      "cache-control": "no-store",
      "x-content-type-options": "nosniff",
      connection: "keep-alive",
    });
    req.socket.setNoDelay(true);
    req.socket.setTimeout(0);
    res.write("retry: 3000\n\n");
    if (Number.isInteger(since) && since >= 0 && since < this.seq) this.send(res, since, this.seq);
    this.streams.add(res);
    this.stats.streams = this.streams.size;
    const drop = () => {
      if (!this.streams.delete(res)) return;
      this.stats.streams = this.streams.size;
      if (!this.streams.size) this.idle();
    };
    req.on("close", drop);
    res.on("error", drop);
    this.start();
  }

  /** Wake the loop now; used after this process's own write, which data_version does not report. */
  poke(): void {
    this.abort?.abort();
  }

  private send(res: ServerResponse, from: number, to: number): void {
    const delta = this.reader.delta(from, to);
    this.stats.deltas += 1;
    res.write(delta ? `id: ${to}\nevent: change\ndata: ${JSON.stringify(delta)}\n\n` : `id: ${to}\nevent: reset\ndata: {}\n\n`);
  }

  private publish(to: number): void {
    const delta = this.reader.delta(this.seq, to);
    this.stats.deltas += 1;
    this.seq = to;
    const frame = delta ? `id: ${to}\nevent: change\ndata: ${JSON.stringify(delta)}\n\n` : `id: ${to}\nevent: reset\ndata: {}\n\n`;
    for (const res of this.streams) res.write(frame);
  }

  private start(): void {
    if (!this.ping) {
      this.ping = setInterval(() => { for (const res of this.streams) res.write(": ping\n\n"); }, this.pingMs);
      this.ping.unref();
    }
    if (this.running || this.closed) return;
    this.running = true;
    void (async () => {
      try {
        while (this.streams.size && !this.closed) {
          const controller = new AbortController();
          this.abort = controller;
          const seq = await this.watcher.next(this.seq, LOOP_TIMEOUT_MS, controller.signal);
          if (this.closed) break;
          if (seq > this.seq) this.publish(seq);
        }
      } catch {
        // The watcher closed under us; the process is shutting down.
      } finally {
        this.running = false;
        this.abort = null;
      }
    })();
  }

  private idle(): void {
    if (this.ping) { clearInterval(this.ping); this.ping = null; }
    this.abort?.abort();
  }

  close(): void {
    this.closed = true;
    this.idle();
    for (const res of this.streams) res.end();
    this.streams.clear();
    this.stats.streams = 0;
  }
}

function securityHeaders(nonce?: string): Record<string, string> {
  const csp = nonce
    ? `default-src 'none'; script-src 'nonce-${nonce}'; style-src 'nonce-${nonce}'; connect-src 'self'; form-action 'none'; base-uri 'none'; frame-ancestors 'none'`
    : "default-src 'none'; frame-ancestors 'none'";
  return {
    "content-security-policy": csp,
    "x-content-type-options": "nosniff",
    "x-frame-options": "DENY",
    "referrer-policy": "no-referrer",
    "cache-control": "no-store",
  };
}

function sendJson(res: ServerResponse, status: number, value: unknown, headers: Record<string, string> = {}): void {
  res.writeHead(status, { ...securityHeaders(), "content-type": "application/json; charset=utf-8", ...headers });
  res.end(JSON.stringify(value));
}

function readJson(req: IncomingMessage): Promise<Record<string, unknown>> {
  return new Promise((resolve, reject) => {
    const chunks: Buffer[] = [];
    let size = 0;
    req.on("data", (chunk: Buffer) => {
      size += chunk.length;
      if (size > MAX_BODY_BYTES) { reject(new BusError("invalid", "request body too large")); req.destroy(); return; }
      chunks.push(chunk);
    });
    req.on("end", () => {
      try {
        const text = Buffer.concat(chunks).toString("utf8");
        const value = text ? JSON.parse(text) : {};
        resolve(value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {});
      } catch {
        reject(new BusError("invalid", "request body must be a JSON object"));
      }
    });
    req.on("error", reject);
  });
}

function requireJson(req: IncomingMessage): void {
  if (!String(req.headers["content-type"] ?? "").toLowerCase().startsWith("application/json")) {
    throw new BusError("invalid", "content-type must be application/json");
  }
}

function errorStatus(error: unknown): number {
  if (error instanceof AuthError) return error.status;
  if (error instanceof BusError) {
    return { unauthorized: 401, forbidden: 403, not_found: 404, invalid: 400, conflict: 409 }[error.code];
  }
  return 500;
}

export async function startDashboard(options: DashboardOptions): Promise<DashboardHandle> {
  if (!existsSync(options.dbPath)) throw new Error(`bus database not found: ${options.dbPath} (run \`qagent init\`)`);
  const bus = Bus.open({ dbPath: options.dbPath });
  const stats: DashboardStats = { queries: 0, deltas: 0, streams: 0 };
  const safetyMs = Math.max(50, options.safetyMs ?? DEFAULT_SAFETY_MS);
  const watcher = new CountingWatcher(bus, stats, safetyMs);
  const reader = new Reader(bus, stats);
  const hub = new Hub(watcher, reader, stats, Math.max(1000, options.pingMs ?? DEFAULT_PING_MS));
  const sessions = new Sessions(options.sessionTtlMs, options.ticketTtlMs);
  let port = options.port ?? DEFAULT_PORT;

  async function handle(req: IncomingMessage, res: ServerResponse): Promise<void> {
    const url = new URL(req.url ?? "/", `http://${HOST}:${port}`);
    const path = url.pathname;
    const method = req.method ?? "GET";
    try {
      requireLoopbackHost(req, port);

      if (path === "/health" && method === "GET") {
        return sendJson(res, 200, { ok: true, streams: stats.streams, queries: stats.queries, deltas: stats.deltas });
      }

      if (path === "/" && (method === "GET" || method === "HEAD")) {
        const nonce = randomBytes(16).toString("base64");
        const html = sessions.valid(sessionOf(req)) ? renderPage(reader.state(), nonce) : renderSignedOut(nonce);
        res.writeHead(200, { ...securityHeaders(nonce), "content-type": "text/html; charset=utf-8" });
        res.end(method === "HEAD" ? undefined : html);
        return;
      }

      if (path === "/login" && method === "POST") {
        // Proof of the operator token, from a local process (`qagent dashboard link`). Browsers never send it.
        if (req.headers.origin !== undefined) requireSameOrigin(req);
        requireJson(req);
        const body = await readJson(req);
        stats.queries += 1;
        identityForToken(bus.db, OPERATOR_ID, String(body.operatorToken ?? ""));
        const ticket = sessions.issueTicket();
        return sendJson(res, 200, { ticket, url: `http://${HOST}:${port}/#t=${ticket}`, expiresInSeconds: Math.ceil(sessions.ticketTtlMs / 1000) });
      }

      if (path === "/session" && method === "POST") {
        requireSameOrigin(req);
        requireJson(req);
        const body = await readJson(req);
        const session = sessions.exchange(String(body.ticket ?? ""));
        return sendJson(res, 200, { authenticated: true }, { "set-cookie": sessions.cookie(session) });
      }

      if (path === "/logout" && method === "POST") {
        requireSameOrigin(req);
        sessions.revoke(sessionOf(req));
        return sendJson(res, 200, { authenticated: false }, { "set-cookie": "qagent_dash=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0" });
      }

      if (path === "/api" || path.startsWith("/api/")) {
        requireSession(req, sessions);
        if (method !== "GET" && method !== "HEAD") requireSameOrigin(req);

        if (path === "/api/state" && method === "GET") return sendJson(res, 200, reader.state());

        if (path === "/api/events" && method === "GET") {
          if (hub.size >= MAX_STREAMS) return sendJson(res, 503, { error: "too many open streams" });
          const header = req.headers["last-event-id"];
          const since = Number(typeof header === "string" && header ? header : url.searchParams.get("since") ?? "NaN");
          hub.add(req, res, since);
          return;
        }

        const taskMatch = /^\/api\/task\/(\d+)$/.exec(path);
        if (taskMatch && method === "GET") {
          stats.queries += 1;
          return sendJson(res, 200, bus.getTask(Number(taskMatch[1])));
        }

        if (path === "/api/send" && method === "POST") {
          requireJson(req);
          const body = await readJson(req);
          const text = String(body.text ?? body.body ?? "");
          stats.queries += 1;
          const operator = bus.identify(OPERATOR_ID);
          const sent = bus.send(operator, { to: String(body.to ?? ""), subject: String(body.subject ?? ""), body: text });
          hub.poke();
          return sendJson(res, 200, { sent: sent.map((message) => ({ seq: message.seq, to: message.recipient ?? "*" })) });
        }
        return sendJson(res, 404, { error: "not found" });
      }

      return sendJson(res, 404, { error: "not found" });
    } catch (error) {
      const status = errorStatus(error);
      if (!res.headersSent) sendJson(res, status, { error: status === 500 ? "internal error" : (error as Error).message });
      else res.end();
      if (status === 500) process.stderr.write(`qagent dashboard: ${(error as Error).stack ?? error}\n`);
    }
  }

  const server: Server = createServer((req, res) => { void handle(req, res); });
  await new Promise<void>((resolve, reject) => {
    server.once("error", (error: NodeJS.ErrnoException) => {
      reject(error.code === "EADDRINUSE" ? new Error(`port ${port} on ${HOST} is in use; pass --port`) : error);
    });
    server.listen(port, HOST, () => resolve());
  });
  port = (server.address() as AddressInfo).port;

  let closing: Promise<void> | null = null;
  return {
    url: `http://${HOST}:${port}/`,
    port,
    stats,
    signInUrl: () => `http://${HOST}:${port}/#t=${sessions.issueTicket()}`,
    close: () => {
      closing ??= new Promise<void>((resolve) => {
        hub.close();
        watcher.close();
        server.close(() => { bus.close(); resolve(); });
        server.closeAllConnections();
      });
      return closing;
    },
  };
}
