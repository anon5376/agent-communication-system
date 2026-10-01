import assert from "node:assert/strict";
import { mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { DatabaseSync } from "node:sqlite";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import {
  currentSchemaVersion, latestSchemaVersion, loadMigrations, migrate, openDatabase, SchemaVersionError, type Migration,
} from "../src/core/db.js";

const real = loadMigrations();
const baseline = real[0];

/** Synthetic later versions: an additive column and an additive table, as P03/P07 would add. */
const v2: Migration = { version: 2, name: "task-cost", sql: "ALTER TABLE tasks ADD COLUMN cost_usd REAL NOT NULL DEFAULT 0;" };
const v3: Migration = { version: 3, name: "verdicts", sql: "CREATE TABLE IF NOT EXISTS verdicts (task_id INTEGER NOT NULL, verdict TEXT NOT NULL);" };
const chain = [baseline, v2, v3];

function scratch(): { dir: string; path: string } {
  const dir = mkdtempSync(join(tmpdir(), "acs-schema-"));
  return { dir, path: join(dir, "bus.db") };
}

function populate(db: DatabaseSync): void {
  db.prepare("INSERT INTO agents(id, role, created_ms) VALUES('alice', 'worker', 1)").run();
  db.prepare("INSERT INTO tasks(title, state, creator, created_ms, updated_ms) VALUES('t1', 'open', 'alice', 1, 1)").run();
  db.prepare("INSERT INTO messages(id, ts_ms, sender, body) VALUES('m1', 1, 'alice', 'hello')").run();
  db.prepare("INSERT INTO events(ts_ms, actor, kind, entity, entity_id) VALUES(1, 'alice', 'x', 'task', '1')").run();
}

function snapshot(db: DatabaseSync): unknown {
  return {
    agents: db.prepare("SELECT * FROM agents").all().map((r) => ({ ...r })),
    tasks: db.prepare("SELECT id, title, state FROM tasks").all().map((r) => ({ ...r })),
    messages: db.prepare("SELECT * FROM messages").all().map((r) => ({ ...r })),
    events: db.prepare("SELECT * FROM events").all().map((r) => ({ ...r })),
  };
}

function tables(db: DatabaseSync): string[] {
  return (db.prepare("SELECT name, sql FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' ORDER BY name").all() as { name: string; sql: string | null }[])
    .map((row) => `${row.name}|${row.sql ?? ""}`);
}

test("fresh database equals the baseline file and carries the latest version marker", () => {
  const { dir, path } = scratch();
  try {
    const db = openDatabase(path);
    assert.equal(currentSchemaVersion(db), latestSchemaVersion());
    const reference = new DatabaseSync(":memory:");
    reference.exec(baseline.sql);
    reference.exec("INSERT INTO meta(key, value) VALUES('schema_version', '1')");
    assert.deepEqual(tables(db), tables(reference));
    db.close();
  } finally { rmSync(dir, { recursive: true, force: true }); }
});

test("a database created by the pre-migration binary (version '1') opens with no data loss and no write", () => {
  const { dir, path } = scratch();
  try {
    const legacy = new DatabaseSync(path);
    legacy.exec(baseline.sql);
    legacy.exec("INSERT INTO meta(key, value) VALUES('schema_version', '1')");
    populate(legacy);
    const before = snapshot(legacy);
    const schemaBefore = tables(legacy);
    legacy.close();

    const db = openDatabase(path);
    assert.equal(currentSchemaVersion(db), 1);
    assert.deepEqual(snapshot(db), before);
    assert.deepEqual(tables(db), schemaBefore);
    db.close();
  } finally { rmSync(dir, { recursive: true, force: true }); }
});

test("a database with tables but no version marker is adopted as the baseline without losing rows", () => {
  const { dir, path } = scratch();
  try {
    const legacy = new DatabaseSync(path);
    legacy.exec(baseline.sql);
    populate(legacy);
    const before = snapshot(legacy);
    legacy.close();
    const db = openDatabase(path);
    assert.equal(currentSchemaVersion(db), latestSchemaVersion());
    assert.deepEqual(snapshot(db), before);
    db.close();
  } finally { rmSync(dir, { recursive: true, force: true }); }
});

test("a database at every prior version migrates forward keeping every row", () => {
  for (let from = 1; from < chain.length; from += 1) {
    const { dir, path } = scratch();
    try {
      const old = new DatabaseSync(path);
      migrate(old, path, chain.slice(0, from));
      assert.equal(currentSchemaVersion(old), from);
      populate(old);
      const before = snapshot(old);
      old.close();

      const db = new DatabaseSync(path);
      migrate(db, path, chain);
      assert.equal(currentSchemaVersion(db), chain.length, `from v${from}`);
      assert.deepEqual(snapshot(db), before);
      assert.equal((db.prepare("SELECT cost_usd FROM tasks").get() as { cost_usd: number }).cost_usd, 0);
      assert.equal((db.prepare("SELECT COUNT(*) AS n FROM verdicts").get() as { n: number }).n, 0);
      migrate(db, path, chain);
      assert.equal(currentSchemaVersion(db), chain.length, "migrating again is a no-op");
      db.close();
    } finally { rmSync(dir, { recursive: true, force: true }); }
  }
});

test("a newer database is refused: nothing is written and the marker is not overwritten", () => {
  const { dir, path } = scratch();
  try {
    const future = new DatabaseSync(path);
    migrate(future, path, chain);
    populate(future);
    const before = snapshot(future);
    const schemaBefore = tables(future);
    future.close();

    const known = [baseline, v2]; // a binary that predates v3
    const stale = new DatabaseSync(path);
    assert.throws(() => migrate(stale, path, known), (error: unknown) => error instanceof SchemaVersionError && error.found === 3 && error.known === 2 && /upgrade qagent/.test(error.message));
    assert.equal(currentSchemaVersion(stale), 3);
    stale.close();

    // the real entry point, against a marker beyond anything this binary knows
    const marked = new DatabaseSync(path);
    marked.prepare("UPDATE meta SET value = '999' WHERE key = 'schema_version'").run();
    marked.close();
    assert.throws(() => openDatabase(path), SchemaVersionError);

    const check = openDatabase(path, { readOnly: true });
    assert.equal(currentSchemaVersion(check), 999);
    assert.deepEqual(snapshot(check), before);
    check.close();
    const after = new DatabaseSync(path);
    assert.deepEqual(tables(after).filter((row) => !row.startsWith("meta|")), schemaBefore.filter((row) => !row.startsWith("meta|")));
    after.close();
  } finally { rmSync(dir, { recursive: true, force: true }); }
});

test("an unreadable version marker is refused rather than replaced", () => {
  const { dir, path } = scratch();
  try {
    const db = new DatabaseSync(path);
    db.exec(baseline.sql);
    db.exec("INSERT INTO meta(key, value) VALUES('schema_version', 'banana')");
    db.close();
    assert.throws(() => openDatabase(path), /unreadable schema_version/);
    const after = new DatabaseSync(path);
    assert.equal((after.prepare("SELECT value FROM meta WHERE key = 'schema_version'").get() as { value: string }).value, "banana");
    after.close();
  } finally { rmSync(dir, { recursive: true, force: true }); }
});

test("a failing migration rolls back completely and leaves the previous version in place", () => {
  const { dir, path } = scratch();
  try {
    const db = new DatabaseSync(path);
    migrate(db, path, [baseline]);
    populate(db);
    const broken: Migration = { version: 2, name: "broken", sql: "ALTER TABLE tasks ADD COLUMN ok TEXT; ALTER TABLE no_such_table ADD COLUMN x TEXT;" };
    assert.throws(() => migrate(db, path, [baseline, broken]));
    assert.equal(currentSchemaVersion(db), 1);
    assert.equal(db.isTransaction, false);
    assert.throws(() => db.prepare("SELECT ok FROM tasks").all(), /no such column/);
    db.close();
  } finally { rmSync(dir, { recursive: true, force: true }); }
});

test("shipped schema files are numbered without gaps, and later ones are additive only", () => {
  assert.equal(real[0].version, 1);
  for (const migration of real.slice(1)) {
    assert.doesNotMatch(migration.sql, /\b(DROP|RENAME)\b/i, `schema/${String(migration.version).padStart(3, "0")}-${migration.name}.sql must be additive`);
  }
  const { dir } = scratch();
  try {
    writeFileSync(join(dir, "001-baseline.sql"), "SELECT 1;");
    writeFileSync(join(dir, "003-skipped.sql"), "SELECT 1;");
    assert.throws(() => loadMigrations(dir), /without gaps/);
  } finally { rmSync(dir, { recursive: true, force: true }); }
  const files = readdirSync(new URL("../../schema", import.meta.url)).filter((f) => f.endsWith(".sql"));
  assert.equal(files.length, real.length);
  assert.equal(readFileSync(new URL("../../schema/001-baseline.sql", import.meta.url), "utf8"), baseline.sql);
});
