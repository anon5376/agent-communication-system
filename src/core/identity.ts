/**
 * Hashed agent and operator tokens. hashToken, createBearerToken and the 0600
 * token files are reused from security.ts:9-42, with the home directory passed in.
 *
 * A process names itself (QAGENT_AGENT_ID, old name AGENT_ID, or --as <id>); the
 * library hashes <home>/tokens/<id>.token, or <home>/operator.token for the
 * operator, and accepts the identity only when that hash is stored for that id.
 */
import { createHash, randomBytes } from "node:crypto";
import { chmodSync, mkdirSync, readFileSync, renameSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import type { DatabaseSync } from "node:sqlite";
import { Authority, BusError, OPERATOR_ID } from "./types.js";

export interface Permissions {
  canDelegate: boolean;
  canReview: boolean;
  allowedChildAgentIds?: string[];
  maxDelegationDepth?: number;
  maxConcurrentTasks?: number;
}

/** Shared with Rust: restrictions stored under `policy` in permissions_json. */
export interface AgentPolicy {
  canDelegate?: boolean;
  allowedChildAgentIds?: string[];
  maxDelegationDepth?: number;
  maxConcurrentTasks?: number;
}

export interface Identity {
  agentId: string;
  authority: Authority;
  permissions: Permissions;
}

interface IdentityRow {
  agent_id: string;
  token_hash: string;
  authority: Authority;
  permissions_json: string;
  created_ms: number;
  updated_ms: number;
}

const SAFE_ID = /^[A-Za-z0-9._-]+$/;

export function isSafeAgentId(id: string): boolean {
  return SAFE_ID.test(id) && id.length <= 128;
}

export function assertSafeAgentId(id: string): string {
  if (!isSafeAgentId(id)) throw new BusError("invalid", `unsafe agent id: ${JSON.stringify(id)} (letters, digits, '.', '_' and '-' only)`);
  return id;
}

export function hashToken(token: string): string {
  return createHash("sha256").update(token, "utf8").digest("hex");
}

export function createBearerToken(): string {
  return randomBytes(32).toString("base64url");
}

export function tokenDir(home: string): string {
  return join(home, "tokens");
}

export function operatorTokenPath(home: string): string {
  return join(home, "operator.token");
}

export function agentTokenPath(home: string, agentId: string): string {
  assertSafeAgentId(agentId);
  return join(tokenDir(home), `${agentId}.token`);
}

export function tokenPathFor(home: string, agentId: string): string {
  return agentId === OPERATOR_ID ? operatorTokenPath(home) : agentTokenPath(home, agentId);
}

export function ensurePrivateDirectories(home: string): void {
  mkdirSync(home, { recursive: true, mode: 0o700 });
  mkdirSync(tokenDir(home), { recursive: true, mode: 0o700 });
  try { chmodSync(home, 0o700); } catch { /* best effort on non-POSIX filesystems */ }
  try { chmodSync(tokenDir(home), 0o700); } catch { /* best effort */ }
}

/** Write a token file with mode 0600, atomically (temp file then rename). */
export function writePrivateToken(home: string, path: string, token: string): void {
  ensurePrivateDirectories(home);
  const temporary = `${path}.${process.pid}.tmp`;
  writeFileSync(temporary, `${token}\n`, { mode: 0o600 });
  try { chmodSync(temporary, 0o600); } catch { /* best effort */ }
  renameSync(temporary, path);
}

export function readTokenFile(path: string): string | null {
  try {
    const token = readFileSync(path, "utf8").trim();
    return token || null;
  } catch {
    return null;
  }
}

export function defaultPermissions(authority: Authority): Permissions {
  return authority === "worker" ? { canDelegate: false, canReview: false } : { canDelegate: true, canReview: true };
}

/** The agent id this process claims, from QAGENT_AGENT_ID or the older AGENT_ID. */
export function agentIdFromEnv(env: NodeJS.ProcessEnv = process.env): string | null {
  for (const name of ["QAGENT_AGENT_ID", "AGENT_ID"]) {
    const value = env[name];
    if (typeof value === "string" && value.trim()) return value.trim();
  }
  return null;
}

function rowFor(db: DatabaseSync, agentId: string): IdentityRow | undefined {
  return db.prepare("SELECT * FROM identities WHERE agent_id = ?").get(agentId) as IdentityRow | undefined;
}

function wholeNumber(value: unknown): number | undefined {
  return typeof value === "number" && Number.isInteger(value) && value >= 0 ? value : undefined;
}

export function parsePolicy(value: unknown): AgentPolicy {
  if (!value || typeof value !== "object" || Array.isArray(value)) return {};
  const raw = value as Record<string, unknown>;
  const policy: AgentPolicy = {};
  if (typeof raw.canDelegate === "boolean") policy.canDelegate = raw.canDelegate;
  if (Array.isArray(raw.allowedChildAgentIds)) policy.allowedChildAgentIds = raw.allowedChildAgentIds.filter((id): id is string => typeof id === "string");
  const depth = wholeNumber(raw.maxDelegationDepth);
  if (depth !== undefined) policy.maxDelegationDepth = depth;
  const concurrent = wholeNumber(raw.maxConcurrentTasks);
  if (concurrent !== undefined && concurrent >= 1) policy.maxConcurrentTasks = concurrent;
  return policy;
}

export function parsePermissions(json: string, authority: Authority): Permissions {
  let value: Record<string, unknown>;
  try {
    const parsed = JSON.parse(json) as unknown;
    value = parsed && typeof parsed === "object" && !Array.isArray(parsed) ? parsed as Record<string, unknown> : {};
  } catch {
    value = {};
  }
  const base = defaultPermissions(authority);
  const policy = parsePolicy(value.policy);
  const permissions: Permissions = {
    canDelegate: (typeof value.canDelegate === "boolean" ? value.canDelegate : base.canDelegate) && policy.canDelegate !== false,
    canReview: typeof value.canReview === "boolean" ? value.canReview : base.canReview,
  };
  if (policy.allowedChildAgentIds?.length) permissions.allowedChildAgentIds = policy.allowedChildAgentIds;
  if (policy.maxDelegationDepth !== undefined) permissions.maxDelegationDepth = policy.maxDelegationDepth;
  if (policy.maxConcurrentTasks !== undefined) permissions.maxConcurrentTasks = policy.maxConcurrentTasks;
  return permissions;
}

export function policyWidens(current: AgentPolicy, next: AgentPolicy): boolean {
  if (current.canDelegate === false && next.canDelegate !== false) return true;
  const currentIds = current.allowedChildAgentIds?.length ? current.allowedChildAgentIds : null;
  const nextIds = next.allowedChildAgentIds?.length ? next.allowedChildAgentIds : null;
  if (currentIds && (!nextIds || nextIds.some((id) => !currentIds.includes(id)))) return true;
  for (const key of ["maxDelegationDepth", "maxConcurrentTasks"] as const) {
    const was = current[key];
    const now = next[key];
    if (was !== undefined && (now === undefined || now > was)) return true;
  }
  return false;
}

export function storedPermissionsJson(db: DatabaseSync, agentId: string): Record<string, unknown> {
  const row = rowFor(db, agentId);
  if (!row) return {};
  try {
    const parsed = JSON.parse(row.permissions_json) as unknown;
    return parsed && typeof parsed === "object" && !Array.isArray(parsed) ? parsed as Record<string, unknown> : {};
  } catch {
    return {};
  }
}

/** Read inside the write transaction so an already resolved identity cannot bypass a new limit. */
export function currentPermissions(db: DatabaseSync, agentId: string): Permissions | null {
  const row = rowFor(db, agentId);
  return row ? parsePermissions(row.permissions_json, row.authority) : null;
}

/**
 * Resolve the identity for `agentId` from its token file. The token's hash must
 * be stored for exactly that id: a copy of agent A's token under B's name fails.
 */
export function resolveIdentity(db: DatabaseSync, home: string, agentId: string): Identity {
  if (agentId !== OPERATOR_ID) assertSafeAgentId(agentId);
  const path = tokenPathFor(home, agentId);
  const token = readTokenFile(path);
  if (!token) throw new BusError("unauthorized", `no token file for ${agentId} at ${path}`);
  return identityForToken(db, agentId, token);
}

/** Resolve an identity from a token value (used when a caller holds the token in memory). */
export function identityForToken(db: DatabaseSync, agentId: string, token: string): Identity {
  const tokenHash = hashToken(token);
  const row = db.prepare("SELECT * FROM identities WHERE token_hash = ?").get(tokenHash) as IdentityRow | undefined;
  if (!row) throw new BusError("unauthorized", `token for ${agentId} is not registered (rotate it with \`qagent token rotate ${agentId}\`)`);
  if (row.agent_id !== agentId) throw new BusError("unauthorized", `token does not belong to ${agentId}`);
  if (agentId === OPERATOR_ID && row.authority !== "operator") throw new BusError("unauthorized", "operator identity is not an operator");
  if (agentId !== OPERATOR_ID && row.authority === "operator") throw new BusError("unauthorized", `${agentId} cannot hold operator authority`);
  return { agentId: row.agent_id, authority: row.authority, permissions: parsePermissions(row.permissions_json, row.authority) };
}

export function requireOperator(identity: Identity, action: string): void {
  if (identity.authority !== "operator") throw new BusError("forbidden", `only the operator may ${action}`);
}

/**
 * Store a new token hash for `agentId` and return the token. The caller writes
 * the token file after the transaction commits. Must run inside a transaction.
 */
export function storeNewToken(db: DatabaseSync, agentId: string, authority: Authority, nowMs: number, permissions?: Permissions): string {
  const token = createBearerToken();
  const existing = rowFor(db, agentId);
  const permissionsJson = permissions
    ? JSON.stringify(permissions)
    : existing && existing.authority === authority ? existing.permissions_json : JSON.stringify(defaultPermissions(authority));
  db.prepare(`
    INSERT INTO identities(agent_id, token_hash, authority, permissions_json, created_ms, updated_ms)
    VALUES(?, ?, ?, ?, ?, ?)
    ON CONFLICT(agent_id) DO UPDATE SET token_hash = excluded.token_hash, authority = excluded.authority,
      permissions_json = excluded.permissions_json, updated_ms = excluded.updated_ms
  `).run(agentId, hashToken(token), authority, permissionsJson, existing?.created_ms ?? nowMs, nowMs);
  return token;
}

/** Register an existing token file's hash for `agentId` (used by init to adopt operator.token). */
export function adoptToken(db: DatabaseSync, agentId: string, authority: Authority, token: string, nowMs: number): void {
  db.prepare(`
    INSERT INTO identities(agent_id, token_hash, authority, permissions_json, created_ms, updated_ms)
    VALUES(?, ?, ?, ?, ?, ?)
    ON CONFLICT(agent_id) DO UPDATE SET token_hash = excluded.token_hash, authority = excluded.authority, updated_ms = excluded.updated_ms
  `).run(agentId, hashToken(token), authority, JSON.stringify(defaultPermissions(authority)), nowMs, nowMs);
}

export function storedIdentity(db: DatabaseSync, agentId: string): { tokenHash: string; authority: Authority } | null {
  const row = rowFor(db, agentId);
  return row ? { tokenHash: row.token_hash, authority: row.authority } : null;
}
