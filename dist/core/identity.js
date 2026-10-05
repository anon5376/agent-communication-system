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
import { BusError, OPERATOR_ID } from "./types.js";
const SAFE_ID = /^[A-Za-z0-9._-]+$/;
export function isSafeAgentId(id) {
    return SAFE_ID.test(id) && id.length <= 128;
}
export function assertSafeAgentId(id) {
    if (!isSafeAgentId(id))
        throw new BusError("invalid", `unsafe agent id: ${JSON.stringify(id)} (letters, digits, '.', '_' and '-' only)`);
    return id;
}
export function hashToken(token) {
    return createHash("sha256").update(token, "utf8").digest("hex");
}
export function createBearerToken() {
    return randomBytes(32).toString("base64url");
}
export function tokenDir(home) {
    return join(home, "tokens");
}
export function operatorTokenPath(home) {
    return join(home, "operator.token");
}
export function agentTokenPath(home, agentId) {
    assertSafeAgentId(agentId);
    return join(tokenDir(home), `${agentId}.token`);
}
export function tokenPathFor(home, agentId) {
    return agentId === OPERATOR_ID ? operatorTokenPath(home) : agentTokenPath(home, agentId);
}
export function ensurePrivateDirectories(home) {
    mkdirSync(home, { recursive: true, mode: 0o700 });
    mkdirSync(tokenDir(home), { recursive: true, mode: 0o700 });
    try {
        chmodSync(home, 0o700);
    }
    catch { /* best effort on non-POSIX filesystems */ }
    try {
        chmodSync(tokenDir(home), 0o700);
    }
    catch { /* best effort */ }
}
/** Write a token file with mode 0600, atomically (temp file then rename). */
export function writePrivateToken(home, path, token) {
    ensurePrivateDirectories(home);
    const temporary = `${path}.${process.pid}.tmp`;
    writeFileSync(temporary, `${token}\n`, { mode: 0o600 });
    try {
        chmodSync(temporary, 0o600);
    }
    catch { /* best effort */ }
    renameSync(temporary, path);
}
export function readTokenFile(path) {
    try {
        const token = readFileSync(path, "utf8").trim();
        return token || null;
    }
    catch {
        return null;
    }
}
export function defaultPermissions(authority) {
    return authority === "worker" ? { canDelegate: false, canReview: false } : { canDelegate: true, canReview: true };
}
/** The agent id this process claims, from QAGENT_AGENT_ID or the older AGENT_ID. */
export function agentIdFromEnv(env = process.env) {
    for (const name of ["QAGENT_AGENT_ID", "AGENT_ID"]) {
        const value = env[name];
        if (typeof value === "string" && value.trim())
            return value.trim();
    }
    return null;
}
function rowFor(db, agentId) {
    return db.prepare("SELECT * FROM identities WHERE agent_id = ?").get(agentId);
}
function wholeNumber(value) {
    return typeof value === "number" && Number.isInteger(value) && value >= 0 ? value : undefined;
}
export function parsePolicy(value) {
    if (!value || typeof value !== "object" || Array.isArray(value))
        return {};
    const raw = value;
    const policy = {};
    if (typeof raw.canDelegate === "boolean")
        policy.canDelegate = raw.canDelegate;
    if (Array.isArray(raw.allowedChildAgentIds))
        policy.allowedChildAgentIds = raw.allowedChildAgentIds.filter((id) => typeof id === "string");
    const depth = wholeNumber(raw.maxDelegationDepth);
    if (depth !== undefined)
        policy.maxDelegationDepth = depth;
    const concurrent = wholeNumber(raw.maxConcurrentTasks);
    if (concurrent !== undefined && concurrent >= 1)
        policy.maxConcurrentTasks = concurrent;
    return policy;
}
export function parsePermissions(json, authority) {
    let value;
    try {
        const parsed = JSON.parse(json);
        value = parsed && typeof parsed === "object" && !Array.isArray(parsed) ? parsed : {};
    }
    catch {
        value = {};
    }
    const base = defaultPermissions(authority);
    const policy = parsePolicy(value.policy);
    const permissions = {
        canDelegate: (typeof value.canDelegate === "boolean" ? value.canDelegate : base.canDelegate) && policy.canDelegate !== false,
        canReview: typeof value.canReview === "boolean" ? value.canReview : base.canReview,
    };
    if (policy.allowedChildAgentIds?.length)
        permissions.allowedChildAgentIds = policy.allowedChildAgentIds;
    if (policy.maxDelegationDepth !== undefined)
        permissions.maxDelegationDepth = policy.maxDelegationDepth;
    if (policy.maxConcurrentTasks !== undefined)
        permissions.maxConcurrentTasks = policy.maxConcurrentTasks;
    return permissions;
}
export function policyWidens(current, next) {
    if (current.canDelegate === false && next.canDelegate !== false)
        return true;
    const currentIds = current.allowedChildAgentIds?.length ? current.allowedChildAgentIds : null;
    const nextIds = next.allowedChildAgentIds?.length ? next.allowedChildAgentIds : null;
    if (currentIds && (!nextIds || nextIds.some((id) => !currentIds.includes(id))))
        return true;
    for (const key of ["maxDelegationDepth", "maxConcurrentTasks"]) {
        const was = current[key];
        const now = next[key];
        if (was !== undefined && (now === undefined || now > was))
            return true;
    }
    return false;
}
export function storedPermissionsJson(db, agentId) {
    const row = rowFor(db, agentId);
    if (!row)
        return {};
    try {
        const parsed = JSON.parse(row.permissions_json);
        return parsed && typeof parsed === "object" && !Array.isArray(parsed) ? parsed : {};
    }
    catch {
        return {};
    }
}
/** Read inside the write transaction so an already resolved identity cannot bypass a new limit. */
export function currentPermissions(db, agentId) {
    const row = rowFor(db, agentId);
    return row ? parsePermissions(row.permissions_json, row.authority) : null;
}
/**
 * Resolve the identity for `agentId` from its token file. The token's hash must
 * be stored for exactly that id: a copy of agent A's token under B's name fails.
 */
export function resolveIdentity(db, home, agentId) {
    if (agentId !== OPERATOR_ID)
        assertSafeAgentId(agentId);
    const path = tokenPathFor(home, agentId);
    const token = readTokenFile(path);
    if (!token)
        throw new BusError("unauthorized", `no token file for ${agentId} at ${path}`);
    return identityForToken(db, agentId, token);
}
/** Resolve an identity from a token value (used when a caller holds the token in memory). */
export function identityForToken(db, agentId, token) {
    const tokenHash = hashToken(token);
    const row = db.prepare("SELECT * FROM identities WHERE token_hash = ?").get(tokenHash);
    if (!row)
        throw new BusError("unauthorized", `token for ${agentId} is not registered (rotate it with \`qagent token rotate ${agentId}\`)`);
    if (row.agent_id !== agentId)
        throw new BusError("unauthorized", `token does not belong to ${agentId}`);
    if (agentId === OPERATOR_ID && row.authority !== "operator")
        throw new BusError("unauthorized", "operator identity is not an operator");
    if (agentId !== OPERATOR_ID && row.authority === "operator")
        throw new BusError("unauthorized", `${agentId} cannot hold operator authority`);
    return { agentId: row.agent_id, authority: row.authority, permissions: parsePermissions(row.permissions_json, row.authority) };
}
export function requireOperator(identity, action) {
    if (identity.authority !== "operator")
        throw new BusError("forbidden", `only the operator may ${action}`);
}
/**
 * Store a new token hash for `agentId` and return the token. The caller writes
 * the token file after the transaction commits. Must run inside a transaction.
 */
export function storeNewToken(db, agentId, authority, nowMs, permissions) {
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
export function adoptToken(db, agentId, authority, token, nowMs) {
    db.prepare(`
    INSERT INTO identities(agent_id, token_hash, authority, permissions_json, created_ms, updated_ms)
    VALUES(?, ?, ?, ?, ?, ?)
    ON CONFLICT(agent_id) DO UPDATE SET token_hash = excluded.token_hash, authority = excluded.authority, updated_ms = excluded.updated_ms
  `).run(agentId, hashToken(token), authority, JSON.stringify(defaultPermissions(authority)), nowMs, nowMs);
}
export function storedIdentity(db, agentId) {
    const row = rowFor(db, agentId);
    return row ? { tokenHash: row.token_hash, authority: row.authority } : null;
}
//# sourceMappingURL=identity.js.map