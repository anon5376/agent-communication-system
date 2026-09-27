/**
 * One-command import of the three earlier stores into bus.db:
 *   bus.jsonl     broker audit log: messages (deduplicated by id), registrations -> agents,
 *                 task and system entries -> events rows only
 *   state.sqlite  Qagent broker state: agents, identities (so token files keep working),
 *                 messages, tasks (old id kept in legacy_id), task history -> events
 *   prototype.db  Python coordinator: agents, messages (broadcasts kept), acks, tasks
 *                 (todo/assigned -> open, in_progress -> claimed, review -> submitted,
 *                 done -> accepted), dependencies, notes, events
 *
 * Idempotent: messages dedupe on id, tasks on legacy_id, agents and identities on
 * their keys, notes and events on their full content. Each source's sha256 is kept
 * in meta, so a rerun skips it without --force; with --force a rerun still inserts
 * nothing that is already there. A dry run performs the same import inside a
 * transaction and rolls it back, so its counts are exact.
 */
import { createHash } from "node:crypto";
import { existsSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
import { basename, join, resolve } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { appendEvent, getMeta, openDatabase, SCHEMA_SQL, setMeta } from "./db.js";
import { isSafeAgentId } from "./identity.js";
import { CLAIM_TTL_MS, OPERATOR_ID, PRIORITIES, REFERENCE_TYPES } from "./types.js";
const MESSAGE_TYPES = new Set(["info", "question", "answer", "task", "result", "feedback", "control"]);
const QAGENT_STATES = {
    blocked: "blocked", ready: "open", assigned: "open", in_progress: "claimed", submitted: "submitted",
    changes_requested: "changes_requested", accepted: "accepted", failed: "failed", cancelled: "cancelled",
};
const PROTOTYPE_STATES = {
    todo: "open", assigned: "open", in_progress: "claimed", blocked: "blocked", review: "submitted",
    done: "accepted", failed: "failed", cancelled: "cancelled",
};
/** The default sources: bus.jsonl and state.sqlite next to the database, and ~/prototype_0.2/prototype.db. */
export function defaultImportSources(home) {
    const candidates = {
        jsonl: join(home, "bus.jsonl"),
        qagentState: join(home, "state.sqlite"),
        prototype: join(homedir(), "prototype_0.2", "prototype.db"),
    };
    return {
        jsonl: existsSync(candidates.jsonl) ? candidates.jsonl : null,
        qagentState: existsSync(candidates.qagentState) ? candidates.qagentState : null,
        prototype: existsSync(candidates.prototype) ? candidates.prototype : null,
    };
}
function bump(counter, key, by = 1) {
    counter[key] = (counter[key] ?? 0) + by;
}
function text(value) {
    return value === null || value === undefined ? "" : String(value);
}
function parseJson(value, fallback) {
    if (typeof value !== "string" || !value)
        return fallback;
    try {
        return JSON.parse(value);
    }
    catch {
        return fallback;
    }
}
function isoMs(value, fallback) {
    if (typeof value === "number" && Number.isFinite(value))
        return value;
    const parsed = Date.parse(text(value));
    return Number.isFinite(parsed) ? parsed : fallback;
}
function fileSha256(path) {
    const hash = createHash("sha256");
    hash.update(readFileSync(path));
    // A live SQLite source may hold committed pages in its WAL.
    if (existsSync(`${path}-wal`))
        hash.update(readFileSync(`${path}-wal`));
    return hash.digest("hex");
}
function refsFrom(value) {
    if (!Array.isArray(value))
        return [];
    return value.slice(0, 100).flatMap((item) => {
        if (item && typeof item === "object") {
            const row = item;
            const type = REFERENCE_TYPES.includes(text(row.type)) ? text(row.type) : "artifact";
            const value = text(row.value ?? row.path ?? row.url);
            if (!value)
                return [];
            const ref = { type, value: value.slice(0, 4096) };
            if (row.description)
                ref.description = text(row.description).slice(0, 2048);
            return [ref];
        }
        const value = text(item);
        return value ? [{ type: "artifact", value: value.slice(0, 4096) }] : [];
    });
}
function emptyReport(kind, path, sha256) {
    return { kind, path, sha256, alreadyImported: false, read: {}, inserted: {}, duplicates: {}, invalid: {} };
}
function emptyLoaded(report) {
    return { report, agents: [], identities: [], tasks: [], deps: [], messages: [], acks: [], notes: [], events: [] };
}
// ------------------------------------------------------------------- readers
function loadJsonl(path, now) {
    const loaded = emptyLoaded(emptyReport("bus.jsonl", path, fileSha256(path)));
    const { read, invalid } = loaded.report;
    const registrations = new Map();
    const seenMessages = new Set();
    for (const line of readFileSync(path, "utf8").split("\n")) {
        if (!line.trim())
            continue;
        bump(read, "lines");
        let entry;
        try {
            entry = JSON.parse(line);
        }
        catch {
            bump(invalid, "lines");
            continue;
        }
        const kind = text(entry.kind);
        const data = (entry.data && typeof entry.data === "object" ? entry.data : {});
        const ts = isoMs(entry.ts, now);
        if (kind === "message") {
            bump(read, "messages");
            const id = text(data.id);
            if (!id) {
                bump(invalid, "messages");
                continue;
            }
            if (seenMessages.has(id)) {
                bump(read, "duplicateLines");
                continue;
            }
            seenMessages.add(id);
            const legacyTaskId = data.taskId ? text(data.taskId) : null;
            loaded.messages.push({
                id, tsMs: isoMs(data.ts, ts), sender: text(data.from) || "unknown", recipient: data.to === "*" ? null : (text(data.to) || null),
                type: MESSAGE_TYPES.has(text(data.type)) ? text(data.type) : "info", subject: text(data.subject), body: text(data.body),
                thread: legacyTaskId ?? "", legacyTaskId, refs: refsFrom(data.refs), requiresAck: false,
            });
        }
        else if (kind === "register") {
            bump(read, "registrations");
            const id = text(data.id);
            if (!isSafeAgentId(id)) {
                bump(invalid, "registrations");
                continue;
            }
            const previous = registrations.get(id);
            registrations.set(id, {
                id, role: text(data.role) || previous?.role || "", model: text(data.model) || previous?.model || "",
                harness: text(data.harness) || previous?.harness || "", parent: null,
                createdMs: previous?.createdMs ?? ts, lastSeenMs: ts, meta: { importedFrom: "bus.jsonl" },
            });
        }
        else {
            bump(read, "events");
            const taskId = text(data.taskId ?? data.id);
            const entity = kind.startsWith("task_") ? "task" : (data.target || data.id || kind.startsWith("operator_")) ? "agent" : "system";
            const entityId = entity === "task" ? taskId : text(data.target ?? data.id) || (kind.startsWith("operator_") ? OPERATOR_ID : "");
            const summary = kind === "task_create"
                ? { taskId: text(data.id ?? data.taskId), title: text(data.title).slice(0, 200), assigner: data.assigner ?? null, assignee: data.assignee ?? null, state: data.state ?? null }
                : data;
            loaded.events.push({ tsMs: ts, actor: text(data.actor ?? data.by ?? data.assigner) || "system", kind, entity, entityId, data: summary });
        }
    }
    loaded.agents = [...registrations.values()];
    read.agents = loaded.agents.length;
    read.messages = read.messages ?? 0;
    return loaded;
}
function openSource(path) {
    return new DatabaseSync(path, { readOnly: true });
}
function loadQagentState(path, now) {
    const loaded = emptyLoaded(emptyReport("qagent", path, fileSha256(path)));
    const { read, invalid } = loaded.report;
    const db = openSource(path);
    try {
        for (const row of db.prepare("SELECT id, json, updated_at FROM agents ORDER BY id").all()) {
            bump(read, "agents");
            const agent = parseJson(row.json, {});
            const id = text(row.id);
            if (!isSafeAgentId(id)) {
                bump(invalid, "agents");
                continue;
            }
            loaded.agents.push({
                id, role: text(agent.role), model: text(agent.model), harness: text(agent.harness), parent: null,
                createdMs: Number(agent.registeredAt) || Number(row.updated_at) || now, lastSeenMs: Number(agent.lastSeen) || null,
                meta: { importedFrom: "qagent", family: agent.family ?? null, provider: agent.provider ?? null, description: agent.description ?? null },
            });
        }
        for (const row of db.prepare("SELECT * FROM identities ORDER BY id").all()) {
            bump(read, "identities");
            const agentId = text(row.id);
            if (!isSafeAgentId(agentId) || !text(row.token_hash)) {
                bump(invalid, "identities");
                continue;
            }
            const authority = ["operator", "manager", "worker"].includes(text(row.authority)) ? text(row.authority) : "worker";
            loaded.identities.push({
                agentId, tokenHash: text(row.token_hash), authority, permissionsJson: text(row.permissions_json) || "{}",
                createdMs: Number(row.created_at) || now, updatedMs: Number(row.updated_at) || now,
            });
        }
        for (const row of db.prepare("SELECT seq, id, to_agent, json, created_at FROM messages ORDER BY seq").all()) {
            bump(read, "messages");
            const message = parseJson(row.json, {});
            const id = text(row.id);
            if (!id) {
                bump(invalid, "messages");
                continue;
            }
            const legacyTaskId = message.taskId ? text(message.taskId) : null;
            loaded.messages.push({
                id, tsMs: Number(message.ts) || Number(row.created_at) || now, sender: text(message.from) || "unknown",
                recipient: text(row.to_agent) === "*" ? null : text(row.to_agent) || null,
                type: MESSAGE_TYPES.has(text(message.type)) ? text(message.type) : "info", subject: text(message.subject), body: text(message.body),
                thread: legacyTaskId ?? "", legacyTaskId, refs: refsFrom(message.refs), requiresAck: false,
            });
        }
        for (const row of db.prepare("SELECT id, json FROM tasks ORDER BY id").all()) {
            bump(read, "tasks");
            const task = parseJson(row.json, {});
            const legacyId = text(row.id);
            const state = QAGENT_STATES[text(task.state)];
            if (!legacyId || !state || !text(task.title)) {
                bump(invalid, "tasks");
                continue;
            }
            const updatedMs = Number(task.updatedAt) || now;
            const result = task.result && typeof task.result === "object" ? task.result : null;
            const review = task.review && typeof task.review === "object" ? task.review : null;
            const requirements = Array.isArray(task.validationRequirements) ? task.validationRequirements : [];
            loaded.tasks.push({
                legacyId, parentLegacy: task.parentTaskId ? text(task.parentTaskId) : null, title: text(task.title).slice(0, 500),
                brief: text(task.brief), acceptance: requirements.map((req) => text(req.description)).filter(Boolean).join("\n"),
                role: text(task.role), priority: "normal", state, creator: text(task.assigner) || OPERATOR_ID,
                assignee: text(task.assignee) || null, reviewer: text(task.reviewerId) || null,
                pathScopes: Array.isArray(task.pathScopes) ? task.pathScopes.map(text) : [], refs: refsFrom(task.contextRefs),
                result: result ? { summary: text(result.summary), details: text(result.details), changedFiles: result.changedFiles ?? [], artifacts: result.artifacts ?? [], validation: result.validation ?? [], completedMs: Number(result.completedAt) || updatedMs } : null,
                review: review ? { reviewer: text(review.reviewer), accepted: Boolean(review.accepted), feedback: text(review.feedback), reviewedMs: Number(review.reviewedAt) || updatedMs } : null,
                round: Number(task.round) || 1, attempts: Number(task.attempts) || 0, maxRetries: Number(task.maxRetries ?? 2),
                claimExpiresMs: state === "claimed" ? updatedMs + CLAIM_TTL_MS : null, createdMs: Number(task.createdAt) || updatedMs, updatedMs,
            });
            for (const dep of Array.isArray(task.dependencyIds) ? task.dependencyIds : []) {
                bump(read, "dependencies");
                loaded.deps.push({ legacyTaskId: legacyId, legacyDependsOn: text(dep) });
            }
            for (const entry of Array.isArray(task.history) ? task.history : []) {
                bump(read, "history");
                loaded.events.push({
                    tsMs: Number(entry.ts) || updatedMs, actor: text(entry.actor) || "system", kind: `task_${text(entry.kind) || "event"}`,
                    entity: "task", entityId: legacyId, data: { state: entry.state ?? null, note: text(entry.note).slice(0, 500) },
                });
            }
        }
    }
    finally {
        db.close();
    }
    return loaded;
}
function loadPrototype(path, now) {
    const loaded = emptyLoaded(emptyReport("prototype", path, fileSha256(path)));
    const { read, invalid } = loaded.report;
    const db = openSource(path);
    const legacy = (id) => `prototype:${text(id)}`;
    try {
        for (const row of db.prepare("SELECT * FROM agents ORDER BY id").all()) {
            bump(read, "agents");
            const id = text(row.id);
            if (!isSafeAgentId(id)) {
                bump(invalid, "agents");
                continue;
            }
            loaded.agents.push({
                id, role: text(row.role), model: text(row.model), harness: "", parent: row.parent_id ? text(row.parent_id) : null,
                createdMs: isoMs(row.created_ts, now), lastSeenMs: row.heartbeat_ts ? isoMs(row.heartbeat_ts, now) : null,
                meta: { importedFrom: "prototype", displayName: text(row.display_name), capabilities: parseJson(row.capabilities, []), permissions: parseJson(row.permissions, []), meta: parseJson(row.meta, {}) },
            });
        }
        for (const row of db.prepare("SELECT * FROM messages ORDER BY id").all()) {
            bump(read, "messages");
            loaded.messages.push({
                id: legacy(row.id), tsMs: isoMs(row.ts, now), sender: text(row.sender) || "unknown", recipient: row.recipient === null ? null : text(row.recipient),
                type: "info", subject: text(row.subject), body: text(row.body), thread: text(row.thread), legacyTaskId: null, refs: [],
                requiresAck: Number(row.requires_ack) === 1,
            });
        }
        for (const row of db.prepare("SELECT * FROM message_receipts ORDER BY message_id, agent_id").all()) {
            bump(read, "receipts");
            if (!row.ack_ts)
                continue;
            bump(read, "acks");
            loaded.acks.push({ messageId: legacy(row.message_id), agentId: text(row.agent_id), ackMs: isoMs(row.ack_ts, now) });
        }
        for (const row of db.prepare("SELECT * FROM tasks ORDER BY id").all()) {
            bump(read, "tasks");
            const state = PROTOTYPE_STATES[text(row.status)];
            if (!state || !text(row.title)) {
                bump(invalid, "tasks");
                continue;
            }
            const updatedMs = isoMs(row.updated_ts, now);
            const priority = PRIORITIES.includes(text(row.priority)) ? text(row.priority) : "normal";
            loaded.tasks.push({
                legacyId: legacy(row.id), parentLegacy: row.parent_id === null || row.parent_id === undefined ? null : legacy(row.parent_id),
                title: text(row.title).slice(0, 500), brief: text(row.description), acceptance: text(row.acceptance), role: "", priority, state,
                creator: text(row.creator) || "unknown", assignee: text(row.assignee) || null, reviewer: null, pathScopes: [],
                refs: refsFrom(parseJson(row.artifacts, [])), result: null, review: null, round: 1, attempts: 0, maxRetries: 2,
                claimExpiresMs: state === "claimed" ? updatedMs + CLAIM_TTL_MS : null, createdMs: isoMs(row.created_ts, updatedMs), updatedMs,
            });
        }
        for (const row of db.prepare("SELECT * FROM task_dependencies").all()) {
            bump(read, "dependencies");
            loaded.deps.push({ legacyTaskId: legacy(row.task_id), legacyDependsOn: legacy(row.depends_on_task_id) });
        }
        for (const row of db.prepare("SELECT * FROM task_notes ORDER BY id").all()) {
            bump(read, "notes");
            loaded.notes.push({ legacyTaskId: legacy(row.task_id), author: text(row.author) || "unknown", tsMs: isoMs(row.ts, now), body: text(row.note) });
        }
        for (const row of db.prepare("SELECT * FROM events ORDER BY id").all()) {
            bump(read, "events");
            const entity = text(row.entity_type) || "system";
            const entityId = entity === "task" || entity === "message" ? legacy(row.entity_id) : text(row.entity_id);
            loaded.events.push({ tsMs: isoMs(row.ts, now), actor: text(row.actor) || "system", kind: text(row.kind), entity, entityId, data: parseJson(row.data, {}) });
        }
    }
    finally {
        db.close();
    }
    return loaded;
}
// ------------------------------------------------------------------- writer
function metaKey(sha256) {
    return `import:${sha256}`;
}
function openTarget(dbPath, dryRun) {
    if (dryRun && !existsSync(dbPath)) {
        const db = new DatabaseSync(":memory:");
        db.exec(SCHEMA_SQL);
        return db;
    }
    return openDatabase(dbPath);
}
export function runImport(dbPath, sources, options = {}) {
    const clock = options.now ?? Date.now;
    const now = clock();
    const dryRun = Boolean(options.dryRun);
    const actor = options.actor ?? OPERATOR_ID;
    const loaded = [];
    // Order matters for which copy of an agent wins: the richest source first.
    if (sources.qagentState)
        loaded.push(loadQagentState(resolve(sources.qagentState), now));
    if (sources.prototype)
        loaded.push(loadPrototype(resolve(sources.prototype), now));
    if (sources.jsonl)
        loaded.push(loadJsonl(resolve(sources.jsonl), now));
    const db = openTarget(resolve(dbPath), dryRun);
    let cursorSeq = 0;
    try {
        // Dry runs keep every page in memory so the rolled-back transaction never spills to the WAL.
        if (dryRun)
            db.exec("PRAGMA cache_size = -1048576");
        db.exec("BEGIN IMMEDIATE");
        try {
            for (const source of loaded)
                source.report.alreadyImported = !options.force && getMeta(db, metaKey(source.report.sha256)) !== null;
            const active = loaded.filter((source) => !source.report.alreadyImported);
            // Agents: insert with no parent first, then link parents that exist.
            const insertAgent = db.prepare(`
        INSERT OR IGNORE INTO agents(id, role, model, harness, status, last_seen_ms, created_ms, meta_json)
        VALUES(?, ?, ?, ?, 'offline', ?, ?, ?)
      `);
            for (const source of active) {
                for (const agent of source.agents) {
                    const changes = Number(insertAgent.run(agent.id, agent.role, agent.model, agent.harness, agent.lastSeenMs, agent.createdMs, JSON.stringify(agent.meta)).changes);
                    bump(changes ? source.report.inserted : source.report.duplicates, "agents");
                }
            }
            const linkParent = db.prepare("UPDATE agents SET parent_id = ? WHERE id = ? AND parent_id IS NULL AND EXISTS (SELECT 1 FROM agents WHERE id = ?)");
            for (const source of active)
                for (const agent of source.agents)
                    if (agent.parent && agent.parent !== agent.id)
                        linkParent.run(agent.parent, agent.id, agent.parent);
            const insertIdentity = db.prepare(`
        INSERT OR IGNORE INTO identities(agent_id, token_hash, authority, permissions_json, created_ms, updated_ms) VALUES(?, ?, ?, ?, ?, ?)
      `);
            const ensureAgent = db.prepare("INSERT OR IGNORE INTO agents(id, role, status, created_ms, meta_json) VALUES(?, ?, 'offline', ?, '{}')");
            for (const source of active) {
                for (const identity of source.identities) {
                    ensureAgent.run(identity.agentId, identity.authority === "operator" ? "operator" : "", identity.createdMs);
                    const changes = Number(insertIdentity.run(identity.agentId, identity.tokenHash, identity.authority, identity.permissionsJson, identity.createdMs, identity.updatedMs).changes);
                    bump(changes ? source.report.inserted : source.report.duplicates, "identities");
                }
            }
            // Tasks, then parents and dependencies by legacy id.
            // Existence is checked first: an ignored INSERT would still advance the AUTOINCREMENT counter.
            const taskExists = db.prepare("SELECT 1 AS ok FROM tasks WHERE legacy_id = ?");
            const insertTask = db.prepare(`
        INSERT INTO tasks(legacy_id, title, brief, acceptance, role, priority, state, creator, assignee, reviewer,
          path_scopes_json, refs_json, result_json, review_json, round, attempts, max_retries, claim_expires_ms, created_ms, updated_ms)
        VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
      `);
            for (const source of active) {
                for (const task of source.tasks) {
                    if (taskExists.get(task.legacyId)) {
                        bump(source.report.duplicates, "tasks");
                        continue;
                    }
                    const changes = Number(insertTask.run(task.legacyId, task.title, task.brief, task.acceptance, task.role, task.priority, task.state, task.creator, task.assignee, task.reviewer, JSON.stringify(task.pathScopes), JSON.stringify(task.refs), task.result ? JSON.stringify(task.result) : null, task.review ? JSON.stringify(task.review) : null, task.round, task.attempts, task.maxRetries, task.claimExpiresMs, task.createdMs, task.updatedMs).changes);
                    bump(changes ? source.report.inserted : source.report.duplicates, "tasks");
                }
            }
            const taskIdFor = db.prepare("SELECT id FROM tasks WHERE legacy_id = ?");
            const lookupTask = (legacyId) => {
                if (!legacyId)
                    return null;
                const row = taskIdFor.get(legacyId);
                return row ? Number(row.id) : null;
            };
            const linkTask = db.prepare("UPDATE tasks SET parent_id = ? WHERE id = ? AND parent_id IS NULL");
            const insertDep = db.prepare("INSERT OR IGNORE INTO task_deps(task_id, depends_on) VALUES(?, ?)");
            for (const source of active) {
                for (const task of source.tasks) {
                    const id = lookupTask(task.legacyId);
                    const parent = lookupTask(task.parentLegacy);
                    if (id && parent && id !== parent)
                        linkTask.run(parent, id);
                }
                for (const dep of source.deps) {
                    const id = lookupTask(dep.legacyTaskId);
                    const on = lookupTask(dep.legacyDependsOn);
                    if (!id || !on) {
                        bump(source.report.invalid, "dependencies");
                        continue;
                    }
                    bump(Number(insertDep.run(id, on).changes) ? source.report.inserted : source.report.duplicates, "dependencies");
                }
            }
            // Messages from every source in time order, deduplicated by id.
            const messageExists = db.prepare("SELECT 1 AS ok FROM messages WHERE id = ?");
            const insertMessage = db.prepare(`
        INSERT INTO messages(id, ts_ms, sender, recipient, type, subject, body, thread, task_id, refs_json, requires_ack, source)
        VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
      `);
            const queue = active.flatMap((source, rank) => source.messages.map((message, order) => ({ source, message, rank, order })));
            queue.sort((a, b) => a.message.tsMs - b.message.tsMs || a.rank - b.rank || a.order - b.order);
            for (const { source, message } of queue) {
                if (messageExists.get(message.id)) {
                    bump(source.report.duplicates, "messages");
                    continue;
                }
                const result = insertMessage.run(message.id, message.tsMs, message.sender, message.recipient, message.type, message.subject, message.body, message.thread, lookupTask(message.legacyTaskId), JSON.stringify(message.refs), message.requiresAck ? 1 : 0, source.report.kind);
                if (Number(result.changes)) {
                    bump(source.report.inserted, "messages");
                    cursorSeq = Math.max(cursorSeq, Number(result.lastInsertRowid));
                }
                else {
                    bump(source.report.duplicates, "messages");
                }
            }
            const seqFor = db.prepare("SELECT seq FROM messages WHERE id = ?");
            const insertAck = db.prepare("INSERT OR IGNORE INTO acks(seq, agent_id, ack_ms) VALUES(?, ?, ?)");
            const insertNote = db.prepare(`
        INSERT INTO task_notes(task_id, author, ts_ms, body) SELECT ?, ?, ?, ?
        WHERE NOT EXISTS (SELECT 1 FROM task_notes WHERE task_id = ? AND author = ? AND ts_ms = ? AND body = ?)
      `);
            const insertEvent = db.prepare(`
        INSERT INTO events(ts_ms, actor, kind, entity, entity_id, data_json, source) SELECT ?, ?, ?, ?, ?, ?, ?
        WHERE NOT EXISTS (SELECT 1 FROM events WHERE entity = ? AND entity_id = ? AND ts_ms = ? AND kind = ? AND actor = ? AND source = ? AND data_json = ?)
      `);
            for (const source of active) {
                for (const ack of source.acks) {
                    const row = seqFor.get(ack.messageId);
                    if (!row) {
                        bump(source.report.invalid, "acks");
                        continue;
                    }
                    bump(Number(insertAck.run(Number(row.seq), ack.agentId, ack.ackMs).changes) ? source.report.inserted : source.report.duplicates, "acks");
                }
                for (const note of source.notes) {
                    const taskId = lookupTask(note.legacyTaskId);
                    if (!taskId) {
                        bump(source.report.invalid, "notes");
                        continue;
                    }
                    const changes = Number(insertNote.run(taskId, note.author, note.tsMs, note.body, taskId, note.author, note.tsMs, note.body).changes);
                    bump(changes ? source.report.inserted : source.report.duplicates, "notes");
                }
                for (const event of source.events) {
                    const data = JSON.stringify(event.data ?? {});
                    const kind = source.report.kind;
                    const changes = Number(insertEvent.run(event.tsMs, event.actor, event.kind, event.entity, event.entityId, data, kind, event.entity, event.entityId, event.tsMs, event.kind, event.actor, kind, data).changes);
                    bump(changes ? source.report.inserted : source.report.duplicates, "events");
                }
            }
            // No agent wakes to months of history: raise every cursor past the imported mail.
            if (cursorSeq > 0) {
                db.prepare(`
          INSERT INTO cursors(agent_id, last_seq) SELECT id, ? FROM agents WHERE true
          ON CONFLICT(agent_id) DO UPDATE SET last_seq = MAX(cursors.last_seq, excluded.last_seq)
        `).run(cursorSeq);
            }
            for (const source of active) {
                const inserted = Object.values(source.report.inserted).reduce((sum, n) => sum + n, 0);
                const key = metaKey(source.report.sha256);
                if (getMeta(db, key) === null) {
                    setMeta(db, key, JSON.stringify({ kind: source.report.kind, path: source.report.path, file: basename(source.report.path), importedMs: now, read: source.report.read, inserted: source.report.inserted }));
                }
                if (inserted > 0 || getMeta(db, key) === null) {
                    appendEvent(db, { tsMs: now, actor, kind: "import", entity: "system", entityId: source.report.kind, data: { path: source.report.path, sha256: source.report.sha256, inserted: source.report.inserted } });
                }
            }
            db.exec(dryRun ? "ROLLBACK" : "COMMIT");
        }
        catch (error) {
            if (db.isTransaction)
                db.exec("ROLLBACK");
            throw error;
        }
    }
    finally {
        db.close();
    }
    return { dbPath: resolve(dbPath), dryRun, sources: loaded.map((source) => source.report), cursorSeq };
}
//# sourceMappingURL=import.js.map