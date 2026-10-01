/**
 * One SQLite file in WAL mode, opened directly by every process.
 * Pragmas and the transaction helper are ported from store.ts:43-138.
 */
import { DatabaseSync } from "node:sqlite";
import { chmodSync, existsSync, mkdirSync, readdirSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
export const BUSY_TIMEOUT_MS = 5000;
function envValue(env, ...names) {
    for (const name of names) {
        const value = env[name];
        if (typeof value === "string" && value.trim())
            return value.trim();
    }
    return undefined;
}
/**
 * Database path: --db flag, then QAGENT_BUS_DB, then <QAGENT_HOME or AGENT_BUS_HOME or ~/.agent-bus>/bus.db.
 * Token files and inbox signal files live next to the database, so one path fixes the whole home.
 */
export function resolveDbPath(flag, env = process.env) {
    if (flag && flag.trim())
        return resolve(flag.trim());
    const direct = envValue(env, "QAGENT_BUS_DB");
    if (direct)
        return resolve(direct);
    const home = envValue(env, "QAGENT_HOME", "AGENT_BUS_HOME") ?? join(homedir(), ".agent-bus");
    return resolve(home, "bus.db");
}
export function homeFor(dbPath) {
    return dirname(resolve(dbPath));
}
/** Raised when the database was written by a newer binary than this one; nothing is written. */
export class SchemaVersionError extends Error {
    found;
    known;
    constructor(found, known, path) {
        super(`bus database ${path} is at schema version ${found}, but this qagent only knows up to ${known}: upgrade qagent (it will not write to a newer schema)`);
        this.found = found;
        this.known = known;
        this.name = "SchemaVersionError";
    }
}
/**
 * Ordered `schema/NNN-name.sql` files at the repo root, shared with the Rust port. Migrations are
 * additive only: new tables, or nullable/defaulted columns. Never drop or rename.
 */
export function loadMigrations(dir = schemaDirectory()) {
    const migrations = readdirSync(dir)
        .filter((file) => /^\d{3}-[\w-]+\.sql$/.test(file))
        .sort()
        .map((file) => ({ version: Number(file.slice(0, 3)), name: file.slice(4, -4), sql: readFileSync(join(dir, file), "utf8") }));
    migrations.forEach((migration, index) => {
        if (migration.version !== index + 1)
            throw new Error(`schema migrations in ${dir} must be numbered 001, 002, ... without gaps (found ${migration.version} at position ${index + 1})`);
    });
    if (migrations.length === 0)
        throw new Error(`no schema migrations found in ${dir}`);
    return migrations;
}
/** dist/core, dist-test/src/core and src/core all sit below the directory that holds schema/. */
function schemaDirectory() {
    let dir = dirname(fileURLToPath(import.meta.url));
    for (;;) {
        const candidate = join(dir, "schema");
        if (existsSync(join(candidate, "001-baseline.sql")))
            return candidate;
        const parent = dirname(dir);
        if (parent === dir)
            throw new Error("schema/ directory not found next to the qagent installation");
        dir = parent;
    }
}
let defaultMigrations;
/** Highest schema version this binary knows. */
export function latestSchemaVersion() {
    defaultMigrations ??= loadMigrations();
    return defaultMigrations.length;
}
/** The version marker: 0 for an empty database, otherwise the highest applied migration. */
export function currentSchemaVersion(db) {
    const table = db.prepare("SELECT 1 AS ok FROM sqlite_master WHERE type = 'table' AND name = 'meta'").get();
    if (!table)
        return 0;
    const row = db.prepare("SELECT value FROM meta WHERE key = 'schema_version'").get();
    if (!row)
        return 0;
    const version = Number(row.value);
    if (!/^\d+$/.test(row.value) || !Number.isSafeInteger(version))
        throw new Error(`bus database has an unreadable schema_version: ${JSON.stringify(row.value)}`);
    return version;
}
/**
 * Bring the database up to the newest migration in one BEGIN IMMEDIATE transaction. The version is
 * re-read inside the transaction so concurrent first opens apply each migration once. A database
 * newer than the migrations is refused, never rewritten.
 */
export function migrate(db, path = "", migrations = (defaultMigrations ??= loadMigrations())) {
    const known = migrations.length;
    const found = currentSchemaVersion(db);
    if (found > known)
        throw new SchemaVersionError(found, known, path);
    if (found === known)
        return;
    transaction(db, () => {
        const start = currentSchemaVersion(db);
        if (start > known)
            throw new SchemaVersionError(start, known, path);
        for (const migration of migrations.slice(start)) {
            db.exec(migration.sql);
            setMeta(db, "schema_version", String(migration.version));
        }
    });
}
/**
 * Open the bus. The busy timeout is set before anything else so concurrent first
 * opens wait for each other. The schema is only written when it is missing or behind, so an
 * ordinary open performs no write at all. A newer schema throws SchemaVersionError.
 */
export function openDatabase(path, options = {}) {
    if (options.readOnly) {
        if (!existsSync(path))
            throw new Error(`bus database not found: ${path} (run \`qagent init\`)`);
        const db = new DatabaseSync(path, { readOnly: true, timeout: BUSY_TIMEOUT_MS });
        db.exec(`PRAGMA busy_timeout = ${BUSY_TIMEOUT_MS}`);
        return db;
    }
    const dir = dirname(path);
    if (!existsSync(dir)) {
        mkdirSync(dir, { recursive: true, mode: 0o700 });
        try {
            chmodSync(dir, 0o700);
        }
        catch { /* best effort */ }
    }
    const created = !existsSync(path);
    const db = new DatabaseSync(path, { timeout: BUSY_TIMEOUT_MS });
    if (created) {
        try {
            chmodSync(path, 0o600);
        }
        catch { /* best effort */ }
    }
    db.exec(`PRAGMA busy_timeout = ${BUSY_TIMEOUT_MS}`);
    const mode = db.prepare("PRAGMA journal_mode").get();
    if (String(mode.journal_mode).toLowerCase() !== "wal")
        db.exec("PRAGMA journal_mode = WAL");
    db.exec("PRAGMA foreign_keys = ON");
    migrate(db, path);
    return db;
}
/** One BEGIN IMMEDIATE transaction. Nested calls join the outer transaction. */
export function transaction(db, fn) {
    if (db.isTransaction)
        return fn();
    db.exec("BEGIN IMMEDIATE");
    try {
        const value = fn();
        db.exec("COMMIT");
        return value;
    }
    catch (error) {
        if (db.isTransaction)
            db.exec("ROLLBACK");
        throw error;
    }
}
/** Constant SQL prepared once per connection instead of on every call. Entries die with the connection. */
const statements = new WeakMap();
export function prepared(db, sql) {
    let perDb = statements.get(db);
    if (!perDb) {
        perDb = new Map();
        statements.set(db, perDb);
    }
    let statement = perDb.get(sql);
    if (!statement) {
        statement = db.prepare(sql);
        perDb.set(sql, statement);
    }
    return statement;
}
export function getMeta(db, key) {
    const row = prepared(db, "SELECT value FROM meta WHERE key = ?").get(key);
    return row?.value ?? null;
}
export function setMeta(db, key, value) {
    prepared(db, "INSERT INTO meta(key, value) VALUES(?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value").run(key, value);
}
export function appendEvent(db, event) {
    const result = prepared(db, `
    INSERT INTO events(ts_ms, actor, kind, entity, entity_id, data_json, source) VALUES(?, ?, ?, ?, ?, ?, ?)
  `).run(event.tsMs, event.actor, event.kind, event.entity, String(event.entityId), JSON.stringify(event.data ?? {}), event.source ?? "v2");
    return Number(result.lastInsertRowid);
}
export function latestEventSeq(db) {
    const row = prepared(db, "SELECT COALESCE(MAX(seq), 0) AS seq FROM events").get();
    return Number(row.seq);
}
//# sourceMappingURL=db.js.map