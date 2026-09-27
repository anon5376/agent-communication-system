/**
 * Browser sessions for the dashboard. Ported from product-server.ts:65-133.
 *
 * The operator token never reaches the browser. A caller that holds the token
 * (the `qagent dashboard` process itself, or `qagent dashboard link`) gets a
 * single-use ticket; the page trades the ticket for an HttpOnly, SameSite=Strict
 * cookie. Sessions live in memory and end when the dashboard process stops.
 */
import { randomBytes } from "node:crypto";
import type { IncomingMessage } from "node:http";

export const SESSION_COOKIE = "qagent_dash";
export const DEFAULT_TICKET_TTL_MS = 5 * 60_000;
export const DEFAULT_SESSION_TTL_MS = 12 * 60 * 60_000;

export class AuthError extends Error {
  constructor(readonly status: 401 | 403 | 421, message: string) {
    super(message);
    this.name = "AuthError";
  }
}

export class Sessions {
  private readonly tickets = new Map<string, number>();
  private readonly sessions = new Map<string, number>();

  constructor(readonly sessionTtlMs = DEFAULT_SESSION_TTL_MS, readonly ticketTtlMs = DEFAULT_TICKET_TTL_MS) {}

  private prune(): void {
    const now = Date.now();
    for (const [key, expires] of this.tickets) if (expires <= now) this.tickets.delete(key);
    for (const [key, expires] of this.sessions) if (expires <= now) this.sessions.delete(key);
  }

  /** A single-use ticket. Only call this after the caller proved it holds the operator token. */
  issueTicket(): string {
    this.prune();
    const ticket = randomBytes(24).toString("base64url");
    this.tickets.set(ticket, Date.now() + this.ticketTtlMs);
    return ticket;
  }

  /** Trade a ticket for a session id. The ticket is consumed whether or not it is valid. */
  exchange(ticket: string): string {
    this.prune();
    const expires = this.tickets.get(ticket);
    this.tickets.delete(ticket);
    if (!expires || expires <= Date.now()) throw new AuthError(401, "sign-in link is invalid or expired; run `qagent dashboard link`");
    const session = randomBytes(32).toString("base64url");
    this.sessions.set(session, Date.now() + this.sessionTtlMs);
    return session;
  }

  valid(session: string): boolean {
    if (!session) return false;
    const expires = this.sessions.get(session);
    if (!expires) return false;
    if (expires <= Date.now()) { this.sessions.delete(session); return false; }
    return true;
  }

  revoke(session: string): void {
    this.sessions.delete(session);
  }

  cookie(session: string): string {
    return `${SESSION_COOKIE}=${session}; HttpOnly; SameSite=Strict; Path=/; Max-Age=${Math.ceil(this.sessionTtlMs / 1000)}`;
  }
}

export function cookies(req: IncomingMessage): Record<string, string> {
  const out: Record<string, string> = {};
  for (const piece of (req.headers.cookie ?? "").split(";")) {
    const index = piece.indexOf("=");
    if (index <= 0) continue;
    try { out[piece.slice(0, index).trim()] = decodeURIComponent(piece.slice(index + 1).trim()); } catch { /* ignore malformed */ }
  }
  return out;
}

export function sessionOf(req: IncomingMessage): string {
  return cookies(req)[SESSION_COOKIE] ?? "";
}

export function requireSession(req: IncomingMessage, sessions: Sessions): string {
  const session = sessionOf(req);
  if (!sessions.valid(session)) throw new AuthError(401, "dashboard session missing or expired; run `qagent dashboard link`");
  return session;
}

/**
 * Reject requests whose Host is not this loopback listener. A DNS-rebinding page
 * reaches 127.0.0.1 with its own host name, so this check stops it before any
 * cookie or ticket is looked at.
 */
export function requireLoopbackHost(req: IncomingMessage, port: number): void {
  const host = String(req.headers.host ?? "").toLowerCase();
  if (host !== `127.0.0.1:${port}` && host !== `localhost:${port}`) throw new AuthError(421, "unexpected Host header");
}

/** Writes must come from this page: Origin equal to our own, and no cross-site fetch metadata. */
export function requireSameOrigin(req: IncomingMessage): void {
  const origin = req.headers.origin;
  const host = req.headers.host;
  if (!origin || !host) throw new AuthError(403, "same-origin request required");
  if (origin !== `http://${host}`) throw new AuthError(403, "cross-origin request rejected");
  const site = req.headers["sec-fetch-site"];
  if (site !== undefined && site !== "same-origin") throw new AuthError(403, "cross-site request rejected");
}
