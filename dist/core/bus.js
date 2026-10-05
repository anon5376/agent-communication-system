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
import { ChangeWatcher } from "./changes.js";
import { appendEvent, homeFor, latestEventSeq, openDatabase, prepared, resolveDbPath, transaction } from "./db.js";
import { adoptToken, agentIdFromEnv, assertSafeAgentId, currentPermissions, ensurePrivateDirectories, hashToken, operatorTokenPath, parsePolicy, policyWidens, readTokenFile, requireOperator, resolveIdentity, storedIdentity, storedPermissionsJson, storeNewToken, tokenPathFor, writePrivateToken, } from "./identity.js";
import { boundedString, BusError, CLAIM_TTL_MS, CLOSED_STATES, contextReferences, LIMITS, MESSAGE_TYPES, OPERATOR_ID, PRIORITIES, STALE_AGENT_MS, TASK_STATES, } from "./types.js";
function json(value, fallback) {
    if (typeof value !== "string" || !value)
        return fallback;
    try {
        return JSON.parse(value);
    }
    catch {
        return fallback;
    }
}
function num(value) {
    return value === null || value === undefined ? null : Number(value);
}
function placeholders(count) {
    return Array.from({ length: count }, () => "?").join(", ");
}
export function normalizeScope(projectRoot, scope) {
    const root = resolve(projectRoot);
    const absolute = resolve(root, scope || ".");
    if (absolute !== root && !absolute.startsWith(root + sep))
        throw new BusError("invalid", `path scope escapes project root: ${scope}`);
    const rel = relative(root, absolute).split(sep).join("/");
    return rel || ".";
}
function scopesOverlap(a, b) {
    return a === "." || b === "." || a === b || a.startsWith(`${b}/`) || b.startsWith(`${a}/`);
}
export class Bus {
    db;
    dbPath;
    home;
    clock;
    claimTtlMs;
    pendingSignals = new Map();
    constructor(options = {}) {
        this.dbPath = options.dbPath ? resolve(options.dbPath) : resolveDbPath();
        this.home = homeFor(this.dbPath);
        this.clock = options.now ?? Date.now;
        this.claimTtlMs = options.claimTtlMs ?? CLAIM_TTL_MS;
        this.db = openDatabase(this.dbPath);
    }
    static open(options = {}) {
        return new Bus(options);
    }
    close() {
        if (this.db.isOpen)
            this.db.close();
    }
    now() {
        return this.clock();
    }
    /** Run a write transaction, then refresh inbox signal files for the messages it delivered. */
    write(fn) {
        this.pendingSignals.clear();
        let value;
        try {
            value = transaction(this.db, fn);
        }
        catch (error) {
            this.pendingSignals.clear();
            throw error;
        }
        this.flushSignals();
        return value;
    }
    event(actor, kind, entity, entityId, data = {}) {
        return appendEvent(this.db, { tsMs: this.now(), actor, kind, entity, entityId, data });
    }
    /**
     * Record that an agent acted. An agent that acts is not waiting, so a stored 'waiting'
     * left behind by a killed waiter is cleared here instead of turning into 'offline'.
     */
    touch(agentId, status) {
        if (status) {
            prepared(this.db, "UPDATE agents SET last_seen_ms = ?, status = ?, wait_until_ms = NULL WHERE id = ?").run(this.now(), status, agentId);
        }
        else {
            prepared(this.db, `
        UPDATE agents SET last_seen_ms = ?, status = CASE WHEN status IN ('offline', 'waiting') THEN 'idle' ELSE status END,
          wait_until_ms = CASE WHEN status = 'waiting' THEN NULL ELSE wait_until_ms END
        WHERE id = ?
      `).run(this.now(), agentId);
        }
    }
    /** Set the caller's own status to working or idle (the supervisor marks a running turn). */
    setStatus(actor, status) {
        if (status !== "working" && status !== "idle")
            throw new BusError("invalid", `status must be working or idle, not ${String(status)}`);
        this.write(() => {
            if (!this.agentRow(actor.agentId))
                throw new BusError("not_found", `unknown agent: ${actor.agentId}`);
            this.touch(actor.agentId, status);
            this.event(actor.agentId, `agent_${status}`, "agent", actor.agentId);
        });
        return this.getAgent(actor.agentId);
    }
    // ---------------------------------------------------------------- identity
    /** Resolve the calling identity from --as / QAGENT_AGENT_ID / AGENT_ID and its token file. */
    identify(agentId) {
        const id = agentId ?? agentIdFromEnv();
        if (!id)
            throw new BusError("unauthorized", "no agent identity: set QAGENT_AGENT_ID or pass --as <id>");
        return resolveIdentity(this.db, this.home, id);
    }
    /** Create the operator agent and token. Adopts an existing operator.token; rotates when file and hash disagree. */
    init() {
        ensurePrivateDirectories(this.home);
        const tokenPath = operatorTokenPath(this.home);
        const operator = this.write(() => {
            const now = this.now();
            prepared(this.db, `
        INSERT INTO agents(id, role, model, harness, status, last_seen_ms, created_ms) VALUES(?, 'operator', 'human', 'cli', 'idle', ?, ?)
        ON CONFLICT(id) DO NOTHING
      `).run(OPERATOR_ID, now, now);
            const stored = storedIdentity(this.db, OPERATOR_ID);
            const fileToken = readTokenFile(tokenPath);
            if (stored && fileToken && hashToken(fileToken) === stored.tokenHash && stored.authority === "operator")
                return "unchanged";
            const owner = fileToken
                ? prepared(this.db, "SELECT agent_id FROM identities WHERE token_hash = ?").get(hashToken(fileToken))
                : undefined;
            let outcome;
            if (!stored && fileToken && !owner) {
                adoptToken(this.db, OPERATOR_ID, "operator", fileToken, now);
                outcome = "adopted";
            }
            else {
                const token = storeNewToken(this.db, OPERATOR_ID, "operator", now);
                writePrivateToken(this.home, tokenPath, token);
                outcome = stored ? "rotated" : "created";
            }
            this.event(OPERATOR_ID, "operator_token", "agent", OPERATOR_ID, { outcome });
            return outcome;
        });
        return { dbPath: this.dbPath, home: this.home, operatorTokenPath: tokenPath, operator };
    }
    addAgent(actor, input) {
        requireOperator(actor, "add agents");
        const id = assertSafeAgentId(String(input.id ?? ""));
        if (id === OPERATOR_ID)
            throw new BusError("invalid", "the operator agent is created by `qagent init`");
        const authority = input.authority ?? "worker";
        if (authority !== "worker" && authority !== "manager")
            throw new BusError("invalid", `authority must be worker or manager, not ${authority}`);
        const role = boundedString(input.role, "role", LIMITS.role);
        const model = boundedString(input.model, "model", LIMITS.model);
        const harness = boundedString(input.harness, "harness", LIMITS.model);
        const parent = input.parent ? assertSafeAgentId(input.parent) : null;
        const tokenPath = tokenPathFor(this.home, id);
        this.write(() => {
            const now = this.now();
            if (parent && !this.agentRow(parent))
                throw new BusError("not_found", `unknown parent agent: ${parent}`);
            const existing = this.agentRow(id);
            if (existing && storedIdentity(this.db, id))
                throw new BusError("conflict", `agent ${id} already exists; use \`qagent token rotate ${id}\``);
            if (existing) {
                prepared(this.db, `
          UPDATE agents SET role = COALESCE(NULLIF(?, ''), role), model = COALESCE(NULLIF(?, ''), model),
            harness = COALESCE(NULLIF(?, ''), harness), parent_id = COALESCE(?, parent_id) WHERE id = ?
        `).run(role, model, harness, parent, id);
            }
            else {
                prepared(this.db, `
          INSERT INTO agents(id, role, model, harness, parent_id, status, created_ms) VALUES(?, ?, ?, ?, ?, 'offline', ?)
        `).run(id, role, model, harness, parent, now);
            }
            const token = storeNewToken(this.db, id, authority, now);
            writePrivateToken(this.home, tokenPath, token);
            this.event(actor.agentId, "agent_added", "agent", id, { role, model, harness, parent, authority });
        });
        return { agent: this.getAgent(id), tokenPath };
    }
    rotateToken(actor, agentId) {
        requireOperator(actor, "rotate tokens");
        const id = agentId === OPERATOR_ID ? OPERATOR_ID : assertSafeAgentId(agentId);
        const tokenPath = tokenPathFor(this.home, id);
        this.write(() => {
            if (!this.agentRow(id))
                throw new BusError("not_found", `unknown agent: ${id}`);
            const authority = storedIdentity(this.db, id)?.authority ?? (id === OPERATOR_ID ? "operator" : "worker");
            const token = storeNewToken(this.db, id, authority, this.now());
            writePrivateToken(this.home, tokenPath, token);
            this.event(actor.agentId, "token_rotated", "agent", id);
        });
        return { tokenPath };
    }
    /**
     * Store the policy that narrows `agentId`'s permissions (null clears it). The operator may
     * set any policy; an agent may set its own only when the new one allows nothing the stored
     * one forbids, so a supervisor can apply its project configuration without the operator
     * token but can never widen what the operator or an earlier policy allowed.
     */
    setAgentPolicy(actor, agentId, policy) {
        const id = assertSafeAgentId(agentId);
        if (actor.authority !== "operator" && actor.agentId !== id)
            throw new BusError("forbidden", `only the operator or ${id} itself may set ${id}'s policy`);
        const next = parsePolicy(policy ?? {});
        return this.write(() => {
            if (!storedIdentity(this.db, id))
                throw new BusError("not_found", `unknown agent: ${id}`);
            const stored = storedPermissionsJson(this.db, id);
            const current = parsePolicy(stored.policy);
            if (actor.authority !== "operator" && policyWidens(current, next)) {
                throw new BusError("forbidden", `${id} may only narrow its own policy; ask the operator to widen it`);
            }
            const value = { ...stored };
            if (policy === null || !Object.keys(next).length)
                delete value.policy;
            else
                value.policy = { ...next, updatedMs: this.now(), by: actor.agentId };
            prepared(this.db, "UPDATE identities SET permissions_json = ?, updated_ms = ? WHERE agent_id = ?").run(JSON.stringify(value), this.now(), id);
            this.event(actor.agentId, "agent_policy", "agent", id, { policy: value.policy ?? null });
            return currentPermissions(this.db, id);
        });
    }
    // ------------------------------------------------------------------ agents
    agentRow(id) {
        return prepared(this.db, "SELECT * FROM agents WHERE id = ?").get(id);
    }
    toAgent(row) {
        const now = this.now();
        const stored = String(row.status ?? "offline");
        const waitUntil = num(row.wait_until_ms);
        const lastSeen = num(row.last_seen_ms);
        let status;
        if (stored === "waiting")
            status = waitUntil !== null && waitUntil >= now ? "waiting" : "offline";
        else if (stored === "offline")
            status = "offline";
        else
            status = lastSeen !== null && now - lastSeen <= STALE_AGENT_MS ? stored : "offline";
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
            authority: row.authority ?? null,
            meta: json(row.meta_json, {}),
        };
    }
    getAgent(id) {
        const row = prepared(this.db, `
      SELECT a.*, i.authority FROM agents a LEFT JOIN identities i ON i.agent_id = a.id WHERE a.id = ?
    `).get(id);
        return row ? this.toAgent(row) : null;
    }
    listAgents() {
        const rows = prepared(this.db, `
      SELECT a.*, i.authority,
        (SELECT COUNT(*) FROM messages m
          WHERE m.seq > COALESCE(c.last_seq, 0) AND (m.recipient = a.id OR (m.recipient IS NULL AND m.sender <> a.id))) AS unread
      FROM agents a LEFT JOIN identities i ON i.agent_id = a.id LEFT JOIN cursors c ON c.agent_id = a.id
      ORDER BY a.id
    `).all();
        return rows.map((row) => ({ ...this.toAgent(row), unread: Number(row.unread) }));
    }
    /** Stored status columns for the given agent ids in one query (dashboard deltas). */
    agentSummaries(ids) {
        const wanted = [...new Set(ids.map(String))].filter((id) => id.length > 0).slice(0, 500);
        if (!wanted.length)
            return [];
        const rows = this.db.prepare(`SELECT id, status, wait_until_ms, last_seen_ms FROM agents WHERE id IN (${placeholders(wanted.length)}) ORDER BY id`)
            .all(...wanted);
        return rows.map((row) => ({
            id: String(row.id), storedStatus: String(row.status), waitUntilMs: num(row.wait_until_ms), lastSeenMs: num(row.last_seen_ms),
        }));
    }
    whoami(actor) {
        return { agent: this.getAgent(actor.agentId), authority: actor.authority, unread: this.unreadCount(actor.agentId), cursor: this.cursor(actor.agentId), dbPath: this.dbPath };
    }
    // ---------------------------------------------------------------- messages
    toMessage(row) {
        return {
            seq: Number(row.seq),
            id: String(row.id),
            tsMs: Number(row.ts_ms),
            sender: String(row.sender),
            recipient: row.recipient === null || row.recipient === undefined ? null : String(row.recipient),
            type: String(row.type),
            subject: String(row.subject ?? ""),
            body: String(row.body ?? ""),
            thread: String(row.thread ?? ""),
            taskId: num(row.task_id),
            refs: json(row.refs_json, []),
            requiresAck: Number(row.requires_ack) === 1,
            source: String(row.source ?? "v2"),
        };
    }
    /** Insert one message inside the current transaction and queue the recipient's signal file. */
    insertMessage(sender, recipient, fields) {
        const id = `msg_${randomUUID()}`;
        const ts = this.now();
        const result = prepared(this.db, `
      INSERT INTO messages(id, ts_ms, sender, recipient, type, subject, body, thread, task_id, refs_json, requires_ack)
      VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
    `).run(id, ts, sender, recipient, fields.type, fields.subject, fields.body, fields.thread, fields.taskId, JSON.stringify(fields.refs), fields.requiresAck ? 1 : 0);
        const seq = Number(result.lastInsertRowid);
        this.event(sender, "message", "message", seq, { recipient, type: fields.type, subject: fields.subject.slice(0, 200), taskId: fields.taskId });
        if (recipient) {
            this.pendingSignals.set(recipient, seq);
        }
        else {
            for (const row of prepared(this.db, "SELECT id FROM agents WHERE id <> ?").all(sender))
                this.pendingSignals.set(String(row.id), seq);
        }
        return { seq, id, tsMs: ts, sender, recipient, ...fields, source: "v2" };
    }
    /** Atomically rewrite inbox/<agent>.seq for harness hooks and shell loops that cannot open SQLite. */
    flushSignals() {
        if (!this.pendingSignals.size)
            return;
        const dir = join(this.home, "inbox");
        try {
            mkdirSync(dir, { recursive: true, mode: 0o700 });
            for (const [agentId, seq] of this.pendingSignals) {
                const path = join(dir, `${agentId}.seq`);
                const temporary = `${path}.${process.pid}.tmp`;
                writeFileSync(temporary, `${seq}\n`, { mode: 0o600 });
                renameSync(temporary, path);
            }
        }
        catch {
            // Signal files are a convenience; the database stays authoritative.
        }
        finally {
            this.pendingSignals.clear();
        }
    }
    send(actor, input) {
        const to = String(input.to ?? "").trim();
        if (!to)
            throw new BusError("invalid", "recipient is required (id, a,b or *)");
        const recipients = to === "*" ? [null] : [...new Set(to.split(",").map((part) => part.trim()).filter(Boolean))];
        if (!recipients.length)
            throw new BusError("invalid", "recipient is required (id, a,b or *)");
        if (recipients.length > 50)
            throw new BusError("invalid", "at most 50 recipients");
        for (const recipient of recipients)
            if (recipient !== null)
                assertSafeAgentId(recipient);
        const type = input.type ?? "info";
        if (!MESSAGE_TYPES.includes(type))
            throw new BusError("invalid", `invalid message type: ${type}`);
        const subject = boundedString(input.subject, "subject", LIMITS.subject);
        const body = boundedString(input.body, "body", LIMITS.body);
        if (!subject.trim() && !body.trim())
            throw new BusError("invalid", "subject or body is required");
        const thread = boundedString(input.thread, "thread", LIMITS.thread);
        const refs = contextReferences(input.refs);
        const taskId = input.taskId === undefined || input.taskId === null ? null : Number(input.taskId);
        return this.write(() => {
            for (const recipient of recipients) {
                if (recipient !== null && !this.agentRow(recipient))
                    throw new BusError("not_found", `unknown recipient: ${recipient}`);
            }
            if (taskId !== null && !this.taskRow(taskId))
                throw new BusError("not_found", `unknown task: ${taskId}`);
            const sent = recipients.map((recipient) => this.insertMessage(actor.agentId, recipient, {
                type, subject, body, thread: thread || (taskId !== null ? `task-${taskId}` : ""), taskId, refs, requiresAck: Boolean(input.requiresAck),
            }));
            this.touch(actor.agentId);
            return sent;
        });
    }
    cursor(agentId) {
        const row = prepared(this.db, "SELECT last_seq FROM cursors WHERE agent_id = ?").get(agentId);
        return Number(row?.last_seq ?? 0);
    }
    /** Unread rows plus the full unread count in one scan; `total` counts everything past the cursor, not just the fetched page. */
    unreadRows(agentId, cursor, limit) {
        const rows = prepared(this.db, `
      SELECT *, (SELECT COUNT(*) FROM messages WHERE seq > ? AND (recipient = ? OR (recipient IS NULL AND sender <> ?))) AS total
      FROM messages
      WHERE seq > ? AND (recipient = ? OR (recipient IS NULL AND sender <> ?))
      ORDER BY seq LIMIT ?
    `).all(cursor, agentId, agentId, cursor, agentId, agentId, limit);
        return { rows, total: rows.length ? Number(rows[0].total) : 0 };
    }
    unreadAfterSnapshot(agentId, afterSeq, limit) {
        const from = Math.max(this.cursor(agentId), afterSeq);
        const { rows, total } = this.unreadRows(agentId, from, Math.max(1, Math.min(LIMITS.inboxLimit, Math.floor(limit))));
        const messages = rows.map((row) => this.toMessage(row));
        let lastSeq = messages.length ? messages[messages.length - 1].seq : from;
        if (total > messages.length) {
            const row = prepared(this.db, `
        SELECT MAX(seq) AS seq FROM messages WHERE seq > ? AND (recipient = ? OR (recipient IS NULL AND sender <> ?))
      `).get(from, agentId, agentId);
            lastSeq = Number(row.seq);
        }
        return { messages, total, lastSeq };
    }
    unreadCount(agentId) {
        const row = prepared(this.db, `
      SELECT COUNT(*) AS n FROM messages WHERE seq > ? AND (recipient = ? OR (recipient IS NULL AND sender <> ?))
    `).get(this.cursor(agentId), agentId, agentId);
        return Number(row.n);
    }
    /**
     * Unread mail with a seq after `afterSeq` (never before the read cursor), oldest first, without
     * moving the cursor. `total` and `lastSeq` cover all of it, not just the returned page.
     */
    unreadAfter(agentId, afterSeq, limit = 50) {
        return this.write(() => this.unreadAfterSnapshot(agentId, afterSeq, limit));
    }
    ensureClaudeCodeHookState() {
        this.db.exec(`
      CREATE TABLE IF NOT EXISTS claude_code_hook_state (
        agent_id TEXT PRIMARY KEY,
        owner TEXT NOT NULL,
        announced_seq INTEGER NOT NULL DEFAULT 0
      )
    `);
    }
    registerClaudeCodeHookOwner(agentId, owner) {
        const id = agentId === OPERATOR_ID ? agentId : assertSafeAgentId(agentId);
        const value = boundedString(owner, "hook owner", 256, true);
        if (!value)
            throw new BusError("invalid", "hook owner is required");
        this.write(() => {
            this.ensureClaudeCodeHookState();
            prepared(this.db, `
        INSERT INTO claude_code_hook_state(agent_id, owner, announced_seq) VALUES(?, ?, 0)
        ON CONFLICT(agent_id) DO UPDATE SET owner = excluded.owner
      `).run(id, value);
        });
    }
    isClaudeCodeHookOwner(agentId, owner) {
        const id = agentId === OPERATOR_ID ? agentId : assertSafeAgentId(agentId);
        const table = prepared(this.db, "SELECT 1 AS present FROM sqlite_master WHERE type = 'table' AND name = 'claude_code_hook_state'").get();
        if (!table)
            return false;
        const row = prepared(this.db, "SELECT 1 AS owner FROM claude_code_hook_state WHERE agent_id = ? AND owner = ?").get(id, owner);
        return Boolean(row);
    }
    claimClaudeCodeHookMail(agentId, owner, limit = 50) {
        const id = agentId === OPERATOR_ID ? agentId : assertSafeAgentId(agentId);
        return this.write(() => {
            this.ensureClaudeCodeHookState();
            const state = prepared(this.db, "SELECT owner, announced_seq FROM claude_code_hook_state WHERE agent_id = ?").get(id);
            if (!state || state.owner !== owner)
                return { status: "superseded", messages: [], total: 0 };
            const fresh = this.unreadAfterSnapshot(id, Number(state.announced_seq), limit);
            if (!fresh.messages.length)
                return { status: "empty", messages: [], total: 0 };
            prepared(this.db, "UPDATE claude_code_hook_state SET announced_seq = ? WHERE agent_id = ? AND owner = ?")
                .run(fresh.lastSeq, id, owner);
            return { status: "mail", messages: fresh.messages, total: fresh.total };
        });
    }
    /**
     * Keep a long turn's claims alive: push the expiry of every unexpired task the caller holds
     * out by one claim TTL and mark it seen. Returns how many claims moved.
     */
    renewClaims(actor) {
        return this.write(() => {
            const now = this.now();
            const result = prepared(this.db, `
        UPDATE tasks SET claim_expires_ms = ?
        WHERE state = 'claimed' AND assignee = ? AND claim_expires_ms IS NOT NULL AND claim_expires_ms >= ?
      `).run(now + this.claimTtlMs, actor.agentId, now);
            this.touch(actor.agentId);
            return Number(result.changes);
        });
    }
    /** New mail since the cursor. Advances the cursor unless peek is set. */
    inbox(actor, options = {}) {
        const limit = Math.max(1, Math.min(LIMITS.inboxLimit, Math.floor(options.limit ?? 50)));
        const me = actor.agentId;
        if (options.peek) {
            const cursor = this.cursor(me);
            const { rows, total } = this.unreadRows(me, cursor, limit);
            const messages = rows.map((row) => this.toMessage(row));
            return { messages, cursor, remaining: total - messages.length };
        }
        return this.write(() => {
            const readAt = this.cursor(me);
            const { rows, total } = this.unreadRows(me, readAt, limit);
            const messages = rows.map((row) => this.toMessage(row));
            if (messages.length) {
                const last = messages[messages.length - 1].seq;
                prepared(this.db, `
          INSERT INTO cursors(agent_id, last_seq) VALUES(?, ?)
          ON CONFLICT(agent_id) DO UPDATE SET last_seq = MAX(cursors.last_seq, excluded.last_seq)
        `).run(me, last);
                this.event(me, "inbox_read", "agent", me, { cursor: last, count: messages.length });
                this.touch(me);
                return { messages, cursor: last, remaining: total - messages.length };
            }
            return { messages, cursor: readAt, remaining: 0 };
        });
    }
    ack(actor, seq) {
        return this.write(() => {
            const row = prepared(this.db, "SELECT * FROM messages WHERE seq = ?").get(seq);
            if (!row || !(row.recipient === actor.agentId || row.recipient === null))
                throw new BusError("not_found", `no message ${seq} for ${actor.agentId}`);
            const ackMs = this.now();
            prepared(this.db, "INSERT OR IGNORE INTO acks(seq, agent_id, ack_ms) VALUES(?, ?, ?)").run(seq, actor.agentId, ackMs);
            this.event(actor.agentId, "ack", "message", seq);
            this.touch(actor.agentId);
            return { seq, ackMs };
        });
    }
    getMessages(options = {}) {
        const limit = Math.max(1, Math.min(1000, options.limit ?? 100));
        let rows;
        if (options.taskId !== undefined)
            rows = prepared(this.db, "SELECT * FROM messages WHERE task_id = ? AND seq > ? ORDER BY seq LIMIT ?").all(options.taskId, options.sinceSeq ?? 0, limit);
        else if (options.thread)
            rows = prepared(this.db, "SELECT * FROM messages WHERE thread = ? AND seq > ? ORDER BY seq LIMIT ?").all(options.thread, options.sinceSeq ?? 0, limit);
        else if (options.sinceSeq !== undefined)
            rows = prepared(this.db, "SELECT * FROM messages WHERE seq > ? ORDER BY seq LIMIT ?").all(options.sinceSeq, limit);
        else
            rows = prepared(this.db, "SELECT * FROM messages ORDER BY seq DESC LIMIT ?").all(limit).reverse();
        return rows.map((row) => this.toMessage(row));
    }
    /** Subject/body columns for the given message seqs in one query (dashboard deltas). */
    messageSummaries(seqs) {
        const wanted = [...new Set(seqs.map(Number))].filter((seq) => Number.isInteger(seq) && seq > 0).slice(0, 500);
        if (!wanted.length)
            return [];
        const rows = this.db.prepare(`SELECT seq, ts_ms, sender, recipient, subject, body FROM messages WHERE seq IN (${placeholders(wanted.length)}) ORDER BY seq`)
            .all(...wanted);
        return rows.map((row) => ({
            seq: Number(row.seq), tsMs: Number(row.ts_ms), sender: String(row.sender),
            recipient: row.recipient === null || row.recipient === undefined ? null : String(row.recipient),
            subject: String(row.subject ?? ""), body: String(row.body ?? ""),
        }));
    }
    // ----------------------------------------------------------------- waiting
    /**
     * Block until the caller has unread mail or a task event that concerns it.
     * Writes status='waiting' once at the start and 'idle' once at the end;
     * nothing is written while idle.
     */
    async waitForMail(actor, options) {
        const me = actor.agentId;
        const checkedSeq = latestEventSeq(this.db);
        const pending = this.inbox(actor, { peek: true, limit: 50 }).messages;
        if (pending.length) {
            // A waiter killed earlier (kill -9) can leave 'waiting' stored; clear it without writing otherwise.
            if (String(this.agentRow(me)?.status ?? "") === "waiting") {
                this.write(() => {
                    prepared(this.db, "UPDATE agents SET status = 'idle', wait_until_ms = NULL, last_seen_ms = ? WHERE id = ? AND status = 'waiting'").run(this.now(), me);
                });
            }
            return { status: "mail", messages: pending, events: [], seq: latestEventSeq(this.db) };
        }
        const timeoutMs = Math.max(0, options.timeoutMs);
        const deadline = Date.now() + timeoutMs;
        let since = this.write(() => {
            prepared(this.db, "UPDATE agents SET status = 'waiting', wait_until_ms = ?, last_seen_ms = ? WHERE id = ?").run(this.now() + timeoutMs, this.now(), me);
            return this.event(me, "agent_waiting", "agent", me, { until: this.now() + timeoutMs });
        });
        // Waiters poll data_version only as a missed fs.watch fallback; a 1 s ceiling keeps an
        // idle wait near one read per second instead of the 100 ms default meant for short waits.
        const watcher = options.watcher ?? new ChangeWatcher(this.db, this.dbPath, { maxPollMs: 1000, ...options.watcherOptions });
        let result = { status: "timeout", messages: [], events: [], seq: since };
        try {
            // Mail or a task event written between the empty check above and the waiting event
            // sits at or below `since`, where the watcher never looks; pick it up here.
            const raced = this.inbox(actor, { peek: true, limit: 50 }).messages;
            const racedEvents = raced.length ? [] : this.taskEventsFor(me, checkedSeq, since);
            if (raced.length)
                result = { status: "mail", messages: raced, events: [], seq: since };
            else if (racedEvents.length)
                result = { status: "task", messages: [], events: racedEvents, seq: since };
            while (result.status === "timeout" && !options.signal?.aborted) {
                const remaining = deadline - Date.now();
                if (remaining <= 0)
                    break;
                const seq = await watcher.next(since, remaining, options.signal);
                if (seq <= since)
                    break;
                const messages = this.inbox(actor, { peek: true, limit: 50 }).messages;
                if (messages.length) {
                    result = { status: "mail", messages, events: [], seq };
                    break;
                }
                const events = this.taskEventsFor(me, since, seq);
                if (events.length) {
                    result = { status: "task", messages: [], events, seq };
                    break;
                }
                since = seq;
                result.seq = seq;
            }
        }
        finally {
            if (!options.watcher)
                watcher.close();
            if (this.db.isOpen) {
                const reason = result.status;
                this.write(() => {
                    prepared(this.db, "UPDATE agents SET status = 'idle', wait_until_ms = NULL, last_seen_ms = ? WHERE id = ? AND status = 'waiting'").run(this.now(), me);
                    this.event(me, "agent_idle", "agent", me, { reason });
                });
            }
        }
        return result;
    }
    /** Task events after `afterSeq` by someone else on a task this agent owns, reviews, or could claim. */
    taskEventsFor(agentId, afterSeq, uptoSeq = Number.MAX_SAFE_INTEGER) {
        const role = String(this.agentRow(agentId)?.role ?? "");
        const rows = prepared(this.db, `
      SELECT e.* FROM events e JOIN tasks t ON e.entity = 'task' AND t.id = CAST(e.entity_id AS INTEGER)
      WHERE e.seq > ? AND e.seq <= ? AND e.actor <> ? AND e.source = 'v2'
        AND (t.assignee = ? OR t.reviewer = ? OR (t.reviewer IS NULL AND t.creator = ?)
             OR (t.assignee IS NULL AND t.state = 'open' AND (t.role = '' OR t.role = ?)))
      ORDER BY e.seq
    `).all(afterSeq, uptoSeq, agentId, agentId, agentId, agentId, role);
        return rows.map((row) => this.toEvent(row));
    }
    // ------------------------------------------------------------------ events
    toEvent(row) {
        return {
            seq: Number(row.seq),
            tsMs: Number(row.ts_ms),
            actor: String(row.actor),
            kind: String(row.kind),
            entity: String(row.entity),
            entityId: String(row.entity_id),
            data: json(row.data_json, {}),
            source: String(row.source ?? "v2"),
        };
    }
    latestSeq() {
        return latestEventSeq(this.db);
    }
    events(sinceSeq = 0, limit = 200) {
        const rows = prepared(this.db, "SELECT * FROM events WHERE seq > ? ORDER BY seq LIMIT ?").all(sinceSeq, Math.max(1, Math.min(5000, limit)));
        return rows.map((row) => this.toEvent(row));
    }
    // ------------------------------------------------------------------- tasks
    taskRow(id) {
        return prepared(this.db, "SELECT * FROM tasks WHERE id = ?").get(id);
    }
    taskDeps(id) {
        return prepared(this.db, "SELECT depends_on FROM task_deps WHERE task_id = ? ORDER BY depends_on").all(id).map((row) => Number(row.depends_on));
    }
    /** All dependencies for `ids` in one query, keyed by task id. */
    dependenciesFor(ids) {
        const map = new Map();
        if (!ids.length)
            return map;
        const rows = this.db.prepare(`SELECT task_id, depends_on FROM task_deps WHERE task_id IN (${placeholders(ids.length)}) ORDER BY depends_on`).all(...ids);
        for (const row of rows) {
            const taskId = Number(row.task_id);
            const list = map.get(taskId) ?? [];
            list.push(Number(row.depends_on));
            map.set(taskId, list);
        }
        return map;
    }
    toTask(row, dependencies) {
        const id = Number(row.id);
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
            state: String(row.state),
            creator: String(row.creator),
            assignee: row.assignee ? String(row.assignee) : null,
            reviewer: row.reviewer ? String(row.reviewer) : null,
            pathScopes: json(row.path_scopes_json, []),
            refs: json(row.refs_json, []),
            result: json(row.result_json, null),
            review: json(row.review_json, null),
            round: Number(row.round),
            attempts: Number(row.attempts),
            maxRetries: Number(row.max_retries),
            claimExpiresMs: num(row.claim_expires_ms),
            createdMs: Number(row.created_ms),
            updatedMs: Number(row.updated_ms),
            dependencies: dependencies ?? this.taskDeps(id),
        };
    }
    /** Rows -> tasks with one shared dependency query instead of one per task. */
    toTasks(rows) {
        const dependencies = this.dependenciesFor(rows.map((row) => Number(row.id)));
        return rows.map((row) => this.toTask(row, dependencies.get(Number(row.id)) ?? []));
    }
    requireTask(id) {
        if (!Number.isInteger(id) || id <= 0)
            throw new BusError("invalid", `invalid task id: ${id}`);
        const row = this.taskRow(id);
        if (!row)
            throw new BusError("not_found", `unknown task: ${id}`);
        return this.toTask(row);
    }
    getTask(id) {
        const task = this.requireTask(id);
        const notes = prepared(this.db, "SELECT * FROM task_notes WHERE task_id = ? ORDER BY id").all(id).map((row) => ({
            id: Number(row.id), taskId: Number(row.task_id), author: String(row.author), tsMs: Number(row.ts_ms), body: String(row.body),
        }));
        const dependents = prepared(this.db, "SELECT task_id FROM task_deps WHERE depends_on = ? ORDER BY task_id").all(id).map((row) => Number(row.task_id));
        const leases = prepared(this.db, "SELECT path FROM leases WHERE task_id = ? ORDER BY path").all(id).map((row) => String(row.path));
        return { ...task, notes, dependents, messages: this.getMessages({ taskId: id, limit: 1000 }), leases };
    }
    /** Summary columns for the given task ids, without deps, notes or messages (dashboard deltas). */
    taskSummaries(ids) {
        const wanted = [...new Set(ids.map(Number))].filter((id) => Number.isInteger(id) && id > 0).slice(0, 500);
        if (!wanted.length)
            return [];
        const rows = this.db.prepare(`SELECT id, title, assignee, state, created_ms, updated_ms FROM tasks WHERE id IN (${placeholders(wanted.length)}) ORDER BY id`)
            .all(...wanted);
        return rows.map((row) => ({
            id: Number(row.id), title: String(row.title), assignee: row.assignee ? String(row.assignee) : null,
            state: String(row.state), createdMs: Number(row.created_ms), updatedMs: Number(row.updated_ms),
        }));
    }
    listTasks(input = {}) {
        const where = [];
        const args = [];
        if (input.states?.length) {
            for (const state of input.states)
                if (!TASK_STATES.includes(state))
                    throw new BusError("invalid", `invalid task state: ${state}`);
            where.push(`state IN (${placeholders(input.states.length)})`);
            args.push(...input.states);
        }
        else if (!input.includeClosed) {
            where.push(`state NOT IN (${placeholders(CLOSED_STATES.length)})`);
            args.push(...CLOSED_STATES);
        }
        if (input.mine) {
            where.push("(assignee = ? OR creator = ? OR reviewer = ?)");
            args.push(input.mine, input.mine, input.mine);
        }
        const limit = Math.max(1, Math.min(1000, input.limit ?? 200));
        const sql = `SELECT * FROM tasks ${where.length ? `WHERE ${where.join(" AND ")}` : ""} ORDER BY id LIMIT ?`;
        return this.toTasks(this.db.prepare(sql).all(...args, limit));
    }
    /**
     * Claimed tasks with no claim/note activity for `stallMs` — a probably-dead claim.
     * `updated_ms` moves on claim and on every note, so it is the last-activity clock.
     */
    stalledTasks(stallMs) {
        const cutoff = this.now() - Math.max(0, stallMs);
        return this.db.prepare("SELECT * FROM tasks WHERE state = 'claimed' AND updated_ms < ? ORDER BY id").all(cutoff).map((row) => this.toTask(row));
    }
    /**
     * Claims the bus can treat as dead: the lease expired, or the task has been idle
     * for stallMs AND the assignee has not touched the bus in that same window. An
     * active worker keeps refreshing last_seen_ms, so a live claim survives both tests.
     */
    deadClaims(stallMs) {
        const now = this.now();
        const cutoff = now - Math.max(0, stallMs);
        return this.db.prepare(`SELECT t.* FROM tasks t LEFT JOIN agents a ON a.id = t.assignee
       WHERE t.state = 'claimed' AND (
         (t.claim_expires_ms IS NOT NULL AND t.claim_expires_ms < ?)
         OR (t.updated_ms < ? AND (a.last_seen_ms IS NULL OR a.last_seen_ms < ?))
       ) ORDER BY t.id`).all(now, cutoff, cutoff).map((row) => this.toTask(row));
    }
    /**
     * The task's causal chain: its events, its notes, and the mail the bus sent about it,
     * merged into one chronological timeline — the bus is the trace.
     */
    traceTask(id) {
        const task = this.getTask(id);
        // Imported tasks' events are keyed by legacy_id, not the new numeric id.
        const ids = task.legacyId !== null ? [String(id), task.legacyId] : [String(id)];
        const events = this.db.prepare("SELECT * FROM events WHERE entity = 'task' AND entity_id IN (SELECT value FROM json_each(?)) ORDER BY seq").all(JSON.stringify(ids)).map((row) => this.toEvent(row));
        const timeline = [];
        for (const event of events) {
            timeline.push({ seq: event.seq, tsMs: event.tsMs, kind: event.kind, actor: event.actor, summary: event.kind.replaceAll("_", " "), data: event.data });
        }
        for (const note of task.notes) {
            timeline.push({ seq: note.id, tsMs: note.tsMs, kind: "note", actor: note.author, summary: note.body.split("\n", 1)[0].slice(0, 200), body: note.body });
        }
        // Uncapped: getTask() limits messages to 1000, a trace wants the whole chain.
        const mail = this.db.prepare("SELECT * FROM messages WHERE task_id = ? ORDER BY seq").all(id).map((row) => this.toMessage(row));
        for (const message of mail) {
            timeline.push({ seq: message.seq, tsMs: message.tsMs, kind: "mail", actor: message.sender, to: message.recipient, summary: message.subject, body: message.body });
        }
        timeline.sort((a, b) => a.tsMs - b.tsMs || a.seq - b.seq);
        return { task, dependencies: task.dependencies, dependents: task.dependents, timeline };
    }
    /** Reopen claims past their expiry. There is no sweeper process; every task write calls this first. */
    reopenExpiredClaims() {
        const now = this.now();
        const expired = prepared(this.db, "SELECT id, assignee FROM tasks WHERE state = 'claimed' AND claim_expires_ms IS NOT NULL AND claim_expires_ms < ?").all(now);
        for (const row of expired) {
            const id = Number(row.id);
            const created = prepared(this.db, "SELECT data_json FROM events WHERE entity = 'task' AND entity_id = ? AND kind = 'task_created' ORDER BY seq LIMIT 1").get(String(id));
            const preassigned = json(created?.data_json, {}).assignee ?? null;
            prepared(this.db, "UPDATE tasks SET state = 'open', assignee = ?, claim_expires_ms = NULL, updated_ms = ? WHERE id = ?").run(preassigned, now, id);
            prepared(this.db, "DELETE FROM leases WHERE task_id = ?").run(id);
            this.event("system", "claim_expired", "task", id, { previousAssignee: row.assignee ?? null });
        }
    }
    createTask(actor, input) {
        const title = boundedString(input.title, "title", LIMITS.title, true);
        const brief = boundedString(input.brief, "brief", LIMITS.brief);
        const acceptance = boundedString(input.acceptance, "acceptance", LIMITS.acceptance);
        const role = boundedString(input.role, "role", LIMITS.role);
        const priority = input.priority ?? "normal";
        if (!PRIORITIES.includes(priority))
            throw new BusError("invalid", `invalid priority: ${priority}`);
        const to = input.to ? assertSafeAgentId(input.to) : null;
        const reviewer = input.reviewer ? assertSafeAgentId(input.reviewer) : null;
        const refs = contextReferences(input.refs);
        const dependencies = [...new Set((input.dependencies ?? []).map(Number))];
        const project = input.project ? resolve(boundedString(input.project, "project", LIMITS.path)) : null;
        const rawScopes = (input.pathScopes ?? []).map((scope) => boundedString(scope, "path scope", LIMITS.path, true));
        if (rawScopes.length && !project)
            throw new BusError("invalid", "path scopes need a project directory");
        const pathScopes = project ? [...new Set(rawScopes.map((scope) => normalizeScope(project, scope)))].sort() : [];
        const maxRetries = input.maxRetries ?? 2;
        return this.write(() => {
            this.reopenExpiredClaims();
            const now = this.now();
            if (to && !this.agentRow(to))
                throw new BusError("not_found", `unknown assignee: ${to}`);
            if (reviewer && !this.agentRow(reviewer))
                throw new BusError("not_found", `unknown reviewer: ${reviewer}`);
            if (input.parentId !== undefined && input.parentId !== null)
                this.requireTask(Number(input.parentId));
            this.assertMayDelegate(actor, to, input.parentId ?? null);
            let blocked = false;
            for (const dep of dependencies)
                blocked = this.requireTask(dep).state !== "accepted" || blocked;
            const state = blocked ? "blocked" : "open";
            const result = prepared(this.db, `
        INSERT INTO tasks(project, parent_id, title, brief, acceptance, role, priority, state, creator, assignee, reviewer,
          path_scopes_json, refs_json, max_retries, created_ms, updated_ms)
        VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
      `).run(project, input.parentId ?? null, title, brief, acceptance, role, priority, state, actor.agentId, to, reviewer, JSON.stringify(pathScopes), JSON.stringify(refs), maxRetries, now, now);
            const id = Number(result.lastInsertRowid);
            const insertDep = prepared(this.db, "INSERT OR IGNORE INTO task_deps(task_id, depends_on) VALUES(?, ?)");
            for (const dep of dependencies)
                insertDep.run(id, dep);
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
     * Delegation rules, checked inside the creating transaction against the permissions stored
     * now (not those resolved when the caller identified itself). Creating work for someone
     * else, or for anyone to claim, needs canDelegate; a policy can also name the only agents
     * this one may assign and how deep under existing tasks it may create work.
     */
    assertMayDelegate(actor, to, parentId) {
        if (actor.authority === "operator")
            return;
        const permissions = currentPermissions(this.db, actor.agentId) ?? actor.permissions;
        const me = actor.agentId;
        if (to !== me && !permissions.canDelegate) {
            throw new BusError("forbidden", `${me} may not delegate: it can only create tasks assigned to itself (--to ${me})`);
        }
        if (to && to !== me && permissions.allowedChildAgentIds?.length && !permissions.allowedChildAgentIds.includes(to)) {
            throw new BusError("forbidden", `${me} may only assign work to ${permissions.allowedChildAgentIds.join(", ")}, not ${to}`);
        }
        if (to !== me && permissions.maxDelegationDepth !== undefined && parentId !== null) {
            let depth = 0;
            for (let id = Number(parentId); id !== null && depth <= permissions.maxDelegationDepth; depth += 1) {
                const parent = prepared(this.db, "SELECT parent_id FROM tasks WHERE id = ?").get(id);
                id = parent ? num(parent.parent_id) : null;
            }
            if (depth > permissions.maxDelegationDepth) {
                throw new BusError("forbidden", `${me} may create work at most ${permissions.maxDelegationDepth} level(s) below a top-level task`);
            }
        }
    }
    /**
     * Tasks `agentId` could claim right now: open or sent back, assigned to it or unassigned for
     * its role, with no lease conflict, and nothing at all once it holds its claim limit. Read
     * only; the supervisor uses it to schedule work that is already waiting, not just new mail.
     */
    claimableTasks(agentId, limit = 50) {
        const permissions = currentPermissions(this.db, agentId);
        if (permissions?.maxConcurrentTasks !== undefined && this.claimedCount(agentId) >= permissions.maxConcurrentTasks)
            return [];
        const role = String(this.agentRow(agentId)?.role ?? "");
        const rows = prepared(this.db, `
      SELECT * FROM tasks
      WHERE state IN ('open', 'changes_requested')
        AND (assignee = ? OR (assignee IS NULL AND (role = '' OR role = ?)))
      ORDER BY CASE WHEN assignee = ? THEN 0 ELSE 1 END,
               CASE priority WHEN 'urgent' THEN 0 WHEN 'high' THEN 1 WHEN 'normal' THEN 2 ELSE 3 END,
               id LIMIT 500
    `).all(agentId, role, agentId);
        const now = this.now();
        const free = rows.filter((row) => {
            // A claim past its expiry is reopened by the next write; its leases do not block here.
            const task = this.toLiteTask(row);
            return !this.leaseConflicts(task).some((conflict) => {
                const holder = this.taskRow(conflict.taskId);
                return !(holder && num(holder.claim_expires_ms) !== null && Number(holder.claim_expires_ms) < now);
            });
        });
        return this.toTasks(free.slice(0, Math.max(1, limit)));
    }
    /** How many tasks the agent holds claimed right now. */
    claimedCount(agentId) {
        return Number(prepared(this.db, "SELECT COUNT(*) AS n FROM tasks WHERE state = 'claimed' AND assignee = ?").get(agentId).n);
    }
    /**
     * Claim a task atomically. The claim is one UPDATE ... WHERE state IN ('open','changes_requested')
     * AND (assignee IS NULL OR assignee = me) RETURNING; it wins only if that row came back.
     * Without an id, the most urgent claimable task assigned to me, or unassigned for my role, is taken.
     */
    claimTask(actor, taskId) {
        const me = actor.agentId;
        const explicit = taskId !== undefined && taskId !== null;
        return this.write(() => {
            this.reopenExpiredClaims();
            const limit = (currentPermissions(this.db, me) ?? actor.permissions).maxConcurrentTasks;
            if (limit !== undefined) {
                const held = this.claimedCount(me);
                if (held >= limit)
                    throw new BusError("conflict", `${me} already holds ${held} claimed task(s), its limit; submit or release one first`);
            }
            const role = explicit ? "" : String(this.agentRow(me)?.role ?? "");
            const candidateStmt = prepared(this.db, `
        SELECT id, state, assignee, project, path_scopes_json FROM tasks
        WHERE state IN ('open', 'changes_requested')
          AND (assignee = ? OR (assignee IS NULL AND (role = '' OR role = ?)))
        ORDER BY CASE WHEN assignee = ? THEN 0 ELSE 1 END,
                 CASE priority WHEN 'urgent' THEN 0 WHEN 'high' THEN 1 WHEN 'normal' THEN 2 ELSE 3 END,
                 id LIMIT 100 OFFSET ?
      `);
            // Assigned-to-me first, then urgent before older ordinary work. Pages past the first
            // 100 too: a pile of lease-conflicted urgent tasks must not starve later claimable ones.
            let pageStart = 0;
            for (;;) {
                let candidates;
                if (explicit) {
                    const id = Number(taskId);
                    if (!Number.isInteger(id) || id <= 0)
                        throw new BusError("invalid", `invalid task id: ${taskId}`);
                    const row = this.taskRow(id);
                    if (!row)
                        throw new BusError("not_found", `unknown task: ${taskId}`);
                    candidates = [row];
                }
                else {
                    candidates = candidateStmt.all(me, role, me, pageStart);
                    pageStart += 100;
                    if (!candidates.length)
                        break;
                }
                for (const row of candidates) {
                    const current = this.toLiteTask(row);
                    const conflicts = this.leaseConflicts(current);
                    if (conflicts.length) {
                        if (explicit)
                            throw new BusError("conflict", `task ${current.id} path scopes overlap leases held by ${conflicts.map((c) => `#${c.taskId}:${c.path}`).join(", ")}`);
                        continue;
                    }
                    const now = this.now();
                    const claimed = prepared(this.db, `
            UPDATE tasks SET state = 'claimed', assignee = ?, claim_expires_ms = ?, updated_ms = ?
            WHERE id = ? AND state IN ('open', 'changes_requested') AND (assignee IS NULL OR assignee = ?)
            RETURNING *
          `).get(me, now + this.claimTtlMs, now, current.id, me);
                    if (!claimed) {
                        if (explicit) {
                            const reason = current.assignee && current.assignee !== me ? `is ${current.state} and assigned to ${current.assignee}` : `is ${current.state}`;
                            throw new BusError("conflict", `task ${current.id} cannot be claimed: it ${reason}`);
                        }
                        continue;
                    }
                    const task = this.toTask(claimed);
                    if (task.project) {
                        const insert = prepared(this.db, "INSERT OR REPLACE INTO leases(project, path, task_id, created_ms) VALUES(?, ?, ?, ?)");
                        for (const path of task.pathScopes)
                            insert.run(task.project, path, task.id, now);
                    }
                    this.event(me, "task_claimed", "task", task.id, { round: task.round, leases: task.pathScopes });
                    this.touch(me, "working");
                    return task;
                }
                if (explicit)
                    break;
            }
            throw new BusError("not_found", "no claimable task");
        });
    }
    /** The claim path's cheap view of a task row: no dependency or refs parsing. */
    toLiteTask(row) {
        return {
            id: Number(row.id),
            state: String(row.state),
            assignee: row.assignee ? String(row.assignee) : null,
            project: row.project ? String(row.project) : null,
            pathScopes: json(row.path_scopes_json, []),
        };
    }
    leaseConflicts(task) {
        if (!task.project || !task.pathScopes.length)
            return [];
        const active = prepared(this.db, "SELECT task_id, path FROM leases WHERE project = ? AND task_id <> ?").all(task.project, task.id);
        const conflicts = [];
        for (const wanted of task.pathScopes) {
            for (const lease of active)
                if (scopesOverlap(wanted, String(lease.path)))
                    conflicts.push({ taskId: Number(lease.task_id), path: String(lease.path) });
        }
        return conflicts;
    }
    noteTask(actor, taskId, text) {
        const body = boundedString(text, "note", LIMITS.note, true);
        return this.write(() => {
            this.reopenExpiredClaims();
            const task = this.requireTask(taskId);
            const now = this.now();
            const result = prepared(this.db, "INSERT INTO task_notes(task_id, author, ts_ms, body) VALUES(?, ?, ?, ?)").run(task.id, actor.agentId, now, body);
            if (task.state === "claimed" && task.assignee === actor.agentId) {
                prepared(this.db, "UPDATE tasks SET claim_expires_ms = ?, updated_ms = ? WHERE id = ?").run(now + this.claimTtlMs, now, task.id);
            }
            else {
                prepared(this.db, "UPDATE tasks SET updated_ms = ? WHERE id = ?").run(now, task.id);
            }
            const id = Number(result.lastInsertRowid);
            this.event(actor.agentId, "task_note", "task", task.id, { noteId: id });
            this.touch(actor.agentId);
            return { id, taskId: task.id, author: actor.agentId, tsMs: now, body };
        });
    }
    submitTask(actor, taskId, input) {
        const summary = boundedString(input.summary, "summary", LIMITS.summary, true);
        const details = boundedString(input.details, "details", LIMITS.details);
        const changedFiles = [...new Set((input.changedFiles ?? []).map((file) => boundedString(file, "changed file", LIMITS.refValue, true).trim()))];
        if (changedFiles.length > LIMITS.changedFiles)
            throw new BusError("invalid", `at most ${LIMITS.changedFiles} changed files`);
        const artifacts = contextReferences(input.artifacts);
        const validationInput = input.validation === undefined || input.validation === null ? [] : input.validation;
        if (!Array.isArray(validationInput))
            throw new BusError("invalid", "validation must be a list");
        if (validationInput.length > LIMITS.validation)
            throw new BusError("invalid", `at most ${LIMITS.validation} validation entries`);
        const validation = validationInput.map((item) => {
            const row = item && typeof item === "object" ? item : { summary: String(item) };
            const entry = { passed: Boolean(row.passed), summary: boundedString(row.summary, "validation summary", 4096, true) };
            if (row.command)
                entry.command = boundedString(row.command, "validation command", 8192);
            return entry;
        });
        return this.write(() => {
            this.reopenExpiredClaims();
            const task = this.requireTask(taskId);
            if (task.assignee !== actor.agentId)
                throw new BusError("forbidden", `task ${task.id} is assigned to ${task.assignee ?? "nobody"}, not ${actor.agentId}`);
            if (task.state !== "claimed" && task.state !== "changes_requested")
                throw new BusError("conflict", `task ${task.id} is ${task.state}; claim it before submitting`);
            const now = this.now();
            const result = { summary, details, changedFiles, artifacts, validation, completedMs: now };
            prepared(this.db, "UPDATE tasks SET state = 'submitted', result_json = ?, claim_expires_ms = NULL, updated_ms = ? WHERE id = ?")
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
    reviewTask(actor, taskId, input) {
        const feedback = boundedString(input.feedback, "feedback", LIMITS.feedback, true);
        return this.write(() => {
            this.reopenExpiredClaims();
            const task = this.requireTask(taskId);
            const reviewer = task.reviewer ?? task.creator;
            const operator = actor.authority === "operator";
            if (!operator && actor.agentId !== reviewer)
                throw new BusError("forbidden", `only ${reviewer} or the operator may review task ${task.id}`);
            if (!operator && actor.agentId === task.assignee)
                throw new BusError("forbidden", `${actor.agentId} cannot review its own work on task ${task.id}`);
            if (task.state !== "submitted")
                throw new BusError("conflict", `task ${task.id} is ${task.state}, not submitted`);
            const now = this.now();
            const review = { reviewer: actor.agentId, accepted: Boolean(input.accepted), feedback, reviewedMs: now };
            const thread = `task-${task.id}`;
            if (review.accepted) {
                prepared(this.db, "UPDATE tasks SET state = 'accepted', review_json = ?, updated_ms = ? WHERE id = ?").run(JSON.stringify(review), now, task.id);
                prepared(this.db, "DELETE FROM leases WHERE task_id = ?").run(task.id);
                this.event(actor.agentId, "task_accepted", "task", task.id, { round: task.round });
                if (task.assignee && task.assignee !== actor.agentId) {
                    this.insertMessage(actor.agentId, task.assignee, { type: "feedback", subject: `[ACCEPTED #${task.id}] ${task.title}`, body: `${feedback}\n\nNo further action is required on this task.`, thread, taskId: task.id, refs: [], requiresAck: false });
                }
                this.unblockDependents(task.id, actor.agentId);
            }
            else {
                const round = task.round + 1;
                if (round - 1 > task.maxRetries) {
                    prepared(this.db, "UPDATE tasks SET state = 'failed', round = ?, review_json = ?, claim_expires_ms = NULL, updated_ms = ? WHERE id = ?").run(round, JSON.stringify(review), now, task.id);
                    prepared(this.db, "DELETE FROM leases WHERE task_id = ?").run(task.id);
                    this.event(actor.agentId, "task_failed", "task", task.id, { round, reason: "review retry limit exceeded" });
                    if (task.creator !== actor.agentId) {
                        this.insertMessage(actor.agentId, task.creator, { type: "control", subject: `[ESCALATE #${task.id}] review retry limit exceeded`, body: feedback, thread, taskId: task.id, refs: [], requiresAck: false });
                    }
                }
                else {
                    prepared(this.db, "UPDATE tasks SET state = 'changes_requested', round = ?, review_json = ?, claim_expires_ms = NULL, updated_ms = ? WHERE id = ?").run(round, JSON.stringify(review), now, task.id);
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
    unblockDependents(taskId, actor) {
        const dependents = prepared(this.db, `
      SELECT t.id FROM task_deps d JOIN tasks t ON t.id = d.task_id WHERE d.depends_on = ? AND t.state = 'blocked'
    `).all(taskId);
        for (const row of dependents) {
            const id = Number(row.id);
            const open = prepared(this.db, `
        SELECT COUNT(*) AS n FROM task_deps d JOIN tasks t ON t.id = d.depends_on WHERE d.task_id = ? AND t.state <> 'accepted'
      `).get(id);
            if (Number(open.n) === 0) {
                prepared(this.db, "UPDATE tasks SET state = 'open', updated_ms = ? WHERE id = ?").run(this.now(), id);
                this.event(actor, "task_unblocked", "task", id, { releasedBy: taskId });
            }
        }
    }
    /**
     * Give a claimed task back without failing it: state 'open', the creator's original
     * assignee (or nobody) restored, leases released. The assignee or the operator may release.
     */
    releaseTask(actor, taskId, reason) {
        const text = boundedString(reason, "reason", LIMITS.reason) || "released";
        return this.write(() => {
            const before = this.requireTask(taskId);
            if (before.state !== "claimed")
                throw new BusError("conflict", `task ${before.id} is ${before.state}, not claimed`);
            if (actor.authority !== "operator" && actor.agentId !== before.assignee)
                throw new BusError("forbidden", `only ${before.assignee ?? "the assignee"} or the operator may release task ${before.id}`);
            // Snapshot first: an expired claim is reopened by this sweep, which must not
            // count as "not claimed" (and must not be rolled back by throwing after it).
            const claimExpired = before.claimExpiresMs !== null && before.claimExpiresMs < this.now();
            this.reopenExpiredClaims();
            const task = this.requireTask(taskId);
            if (!claimExpired && task.state !== "claimed")
                throw new BusError("conflict", `task ${task.id} is ${task.state}, not claimed`);
            const now = this.now();
            if (!claimExpired) {
                const created = prepared(this.db, "SELECT data_json FROM events WHERE entity = 'task' AND entity_id = ? AND kind = 'task_created' ORDER BY seq LIMIT 1").get(String(task.id));
                const preassigned = json(created?.data_json, {}).assignee ?? null;
                prepared(this.db, "UPDATE tasks SET state = 'open', assignee = ?, claim_expires_ms = NULL, updated_ms = ? WHERE id = ?").run(preassigned, now, task.id);
                prepared(this.db, "DELETE FROM leases WHERE task_id = ?").run(task.id);
            }
            this.event(actor.agentId, "task_released", "task", task.id, { reason: text.slice(0, 500), previousAssignee: before.assignee });
            this.touch(actor.agentId, "idle");
            return this.requireTask(task.id);
        });
    }
    /**
     * Return a task to the pool with no assignee — anyone may claim it. Works on a
     * claimed task and on an open one, so requeuing a batch of expired claims still
     * pools each of them even after the expiry sweep reopened them mid-batch.
     */
    requeueTask(actor, taskId, reason) {
        const text = boundedString(reason, "reason", LIMITS.reason) || "requeued";
        return this.write(() => {
            const before = this.requireTask(taskId);
            if (before.state !== "claimed" && before.state !== "open")
                throw new BusError("conflict", `task ${before.id} is ${before.state}, not claimed or open`);
            if (actor.authority !== "operator" && actor.agentId !== before.assignee)
                throw new BusError("forbidden", `only ${before.assignee ?? "the assignee"} or the operator may requeue task ${before.id}`);
            this.reopenExpiredClaims();
            const now = this.now();
            this.db.prepare("UPDATE tasks SET state = 'open', assignee = NULL, claim_expires_ms = NULL, updated_ms = ? WHERE id = ?").run(now, before.id);
            this.db.prepare("DELETE FROM leases WHERE task_id = ?").run(before.id);
            this.event(actor.agentId, "task_released", "task", before.id, { reason: text.slice(0, 500), previousAssignee: before.assignee, requeued: true });
            this.touch(actor.agentId, "idle");
            return this.requireTask(before.id);
        });
    }
    /**
     * Report that work on a claimed task failed (port of /task/failure, broker.ts:1214-1283, without
     * rerouting). The attempt count rises; within max_retries the task reopens for the same assignee
     * with a retry message, beyond it the task fails and the creator gets an escalation.
     */
    failTask(actor, taskId, error) {
        const note = boundedString(error, "failure", LIMITS.note, true);
        return this.write(() => {
            this.reopenExpiredClaims();
            const task = this.requireTask(taskId);
            if (actor.authority !== "operator" && actor.agentId !== task.assignee)
                throw new BusError("forbidden", `only ${task.assignee ?? "the assignee"} or the operator may report failure for task ${task.id}`);
            if (task.state !== "claimed")
                throw new BusError("conflict", `task ${task.id} is ${task.state}, not claimed`);
            const now = this.now();
            const attempts = task.attempts + 1;
            const thread = `task-${task.id}`;
            prepared(this.db, "INSERT INTO task_notes(task_id, author, ts_ms, body) VALUES(?, ?, ?, ?)").run(task.id, actor.agentId, now, `failure (attempt ${attempts}): ${note}`);
            prepared(this.db, "DELETE FROM leases WHERE task_id = ?").run(task.id);
            if (attempts <= task.maxRetries) {
                prepared(this.db, "UPDATE tasks SET state = 'open', attempts = ?, claim_expires_ms = NULL, updated_ms = ? WHERE id = ?").run(attempts, now, task.id);
                this.event(actor.agentId, "task_retry", "task", task.id, { attempts, maxRetries: task.maxRetries });
                if (task.assignee) {
                    this.insertMessage("system", task.assignee, { type: "task", subject: `[RETRY #${task.id}] attempt ${attempts + 1}: ${task.title}`, body: `${note}\n\nRetry the original scoped task. Do not broaden scope.`, thread, taskId: task.id, refs: task.refs, requiresAck: false });
                }
            }
            else {
                prepared(this.db, "UPDATE tasks SET state = 'failed', attempts = ?, claim_expires_ms = NULL, updated_ms = ? WHERE id = ?").run(attempts, now, task.id);
                this.event(actor.agentId, "task_failed", "task", task.id, { attempts, reason: "retry limit exceeded" });
                if (task.creator !== actor.agentId) {
                    this.insertMessage(actor.agentId, task.creator, { type: "control", subject: `[ESCALATE #${task.id}] attempts exhausted`, body: note, thread, taskId: task.id, refs: [], requiresAck: false });
                }
            }
            this.touch(actor.agentId, "idle");
            return this.requireTask(task.id);
        });
    }
    cancelTask(actor, taskId, reason) {
        const text = boundedString(reason, "reason", LIMITS.reason) || "cancelled";
        return this.write(() => {
            // The expired-claim sweep can clear this task's assignee; the former claimer still
            // deserves the cancelled notice, so remember who held it.
            const prior = this.taskRow(taskId);
            const priorAssignee = prior?.assignee ? String(prior.assignee) : null;
            this.reopenExpiredClaims();
            const task = this.requireTask(taskId);
            if (actor.authority !== "operator" && actor.agentId !== task.creator)
                throw new BusError("forbidden", `only ${task.creator} or the operator may cancel task ${task.id}`);
            if (CLOSED_STATES.includes(task.state))
                throw new BusError("conflict", `task ${task.id} is already ${task.state}`);
            const now = this.now();
            prepared(this.db, "UPDATE tasks SET state = 'cancelled', claim_expires_ms = NULL, updated_ms = ? WHERE id = ?").run(now, task.id);
            prepared(this.db, "DELETE FROM leases WHERE task_id = ?").run(task.id);
            this.event(actor.agentId, "task_cancelled", "task", task.id, { reason: text.slice(0, 500) });
            const notify = task.assignee ?? priorAssignee;
            if (notify && notify !== actor.agentId) {
                this.insertMessage(actor.agentId, notify, { type: "control", subject: `[CANCELLED #${task.id}] ${task.title}`, body: text, thread: `task-${task.id}`, taskId: task.id, refs: [], requiresAck: false });
            }
            this.touch(actor.agentId);
            return this.requireTask(task.id);
        });
    }
    // ------------------------------------------------------------------ status
    status() {
        const counts = {};
        for (const row of prepared(this.db, "SELECT state, COUNT(*) AS n FROM tasks GROUP BY state").all())
            counts[String(row.state)] = Number(row.n);
        return { dbPath: this.dbPath, seq: this.latestSeq(), agents: this.listAgents(), counts, openTasks: this.listTasks({ limit: 200 }) };
    }
}
//# sourceMappingURL=bus.js.map