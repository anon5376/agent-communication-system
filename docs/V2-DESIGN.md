# Qagent v2: one SQLite file, optional daemons

Historical design record, written on 2026-09-25 before the v2 implementation. Paths without a prefix refer to this repository. References to `prototype_0.2/` describe the earlier Python prototype used for comparison.

## Verdict

Build v2 in this repository as a small TypeScript library over one SQLite file (`~/.agent-bus/bus.db`, WAL mode), wrapped by a CLI and a stdio MCP server. Agents send, read, wait, claim and complete tasks with no broker, supervisor or dashboard running. From the Python Agent Coordinator (`prototype_0.2`) v2 keeps direct database access by every process, the read cursor with broadcast, task notes, and the events table as a change feed. From Qagent it keeps hashed per-agent tokens, the operator token, input limits, the submit-and-review cycle, path leases, and the harness adapters and supervisor, which becomes optional. It drops the HTTP broker, in-memory state, the 800 ms snapshot stream, the React dashboard, the Swift GUI, the Python dashboard plugin, the operator MCP, runs, and the router in core. The new dashboard is one dark page that reads the database and sends messages. About 4,100 lines of `src/` are deleted and 2,000 rewritten smaller; about 10,500 lines of web, GUI, plugin and browser-test code also go.

## Scope and limits

The design was based on the code and a local runtime inspection on 2026-09-25. File-watcher latency, sandbox write permissions, and MCP client timeouts were assumptions with tests in the build plan. The implementation and current behavior take precedence over this record.

## What is wrong today

1. **Agents cannot talk without a daemon.** Every worker MCP call is an HTTP request to the broker (`protocol.ts:888-914`). If the broker is missing, the MCP server tries to spawn one (`mcp-server.ts:40-51`). The supervisor refuses to start without it (`supervisor.ts:234`), and `qagent status` fails too (`cli.ts:220`). State lives in the broker's memory with SQLite as a copy (`broker.ts:181-185`, `:203-207`), so no other process can write. `~/.agent-bus/broker.log` ends with 13 `listen EPERM 127.0.0.1:7717` lines: spawned brokers that could not bind a port.
2. **Idle costs work.** The dashboard stream rebuilds a full snapshot every 800 ms per open tab (`product-server.ts:461-481`). Each snapshot covers every task and runs one pending-message query per agent (`broker.ts:374-419`). Every heartbeat writes the agent row and wakes all state waiters (`broker.ts:226-233`, `:327-335`), and the supervisor sends a heartbeat before every wait (`supervisor.ts:269`). The prototype's `await_message` polls every 750 ms and writes a heartbeat each time (`prototype_0.2/coordinator/store.py:338-366`); its dashboard stream checks every second per client (`prototype_0.2/coordinator/dashboard/app.py:1684-1693`).
3. **The prototype has no identity and no atomic claim.** Any caller can pass `sender`, `by` or `agent_id` and act as anyone (`prototype_0.2/coordinator/server.py:222-227`). `claim_task` reads and then writes without a transaction, and its lock is per process (`store.py:47`, `:443-451`), so two processes can both claim one task. Every lifecycle helper passes `force=True` (`store.py:439, 449, 539-554`), so the transition table in `models.py:45-54` rarely applies.
4. **Three systems overlap.** Brokers on 7717 (legacy) and 11511 both append to one 40 MB `bus.jsonl`, the prototype has its own database, and the Python plugin that reads both fails to start (see Migration).

## Requirements

- **R1. No daemon for coordination.** Any agent CLI can send, read, wait for, claim and complete tasks and exchange messages through one SQLite file in WAL mode. Access goes through `src/core/`, a small library shared by the `qagent` CLI and a stdio MCP server. Every write is one `BEGIN IMMEDIATE` transaction that also appends an `events` row.
- **R2. The supervisor is optional and separate.** `qagent supervise` spawns and watches agent CLIs using the same library. Nothing in core imports it, and `autoStart` defaults to false.
- **R3. The dashboard is optional and separate.** `qagent dashboard` is its own process. It only reads, except for one write: send a message as the operator. It learns about changes from one fs.watch per process plus the `events` sequence number, and pushes them to browsers over SSE. There are no per-client timers. A 10 s safety check catches missed file events.
- **R4. Localhost only, with the operator token.** The dashboard binds 127.0.0.1 only. The browser gets a one-time ticket that becomes an HttpOnly cookie, so the operator token never reaches it. Agent identity comes from a token file. No tool accepts a `from` argument.
- **R5. One-command import.** `qagent import` reads `bus.jsonl`, `state.sqlite` and `prototype.db`. It is idempotent and has a dry run.
- **R6. Plain dashboard design.** One page, dark, system fonts, text tables. No cards and no badges.
- **R7. Tests run under the existing npm script names.** `test:unit`, `test:lifecycle` and `test:browser` keep their names, and new tests live in `tests/*.test.ts`.

## Architecture

### Module map

Tags: **reuse** means imported unchanged, **port** means logic moved and trimmed, **new** means written from scratch, **drop** means deleted at cutover.

```text
src/core/types.ts        port  from protocol.ts:76-253 (Message, Task, TaskResult, ContextReference); wire parsers dropped
src/core/db.ts           port  from store.ts:43-138 (pragmas, transaction); new schema below
src/core/identity.ts     reuse security.ts:9-42 (hashToken, createBearerToken, 0600 files); home passed in, not global
src/core/bus.ts          port  task/review/lease rules from broker.ts:113-176, 566-777, 1095-1333 and store.ts:341-385;
                         port  cursor + broadcast + notes from prototype_0.2/coordinator/store.py:287-336, 477-483
src/core/changes.ts      new   change watcher (fs.watch on db dir + events.seq)
src/core/import.ts       new   bus.jsonl / state.sqlite / prototype.db importer
src/cli/main.ts          new   command dispatch; lazy-imports mcp, wait, supervise, dashboard
src/cli/format.ts        port  cli-view.ts:1-49
src/qagent.ts            new   bin entry (#!/usr/bin/env node)
src/mcp/server.ts        port  mcp-server.ts:65-383 (renderers, zod schemas); HTTP calls replaced by core calls
src/notify/wait.ts       new   `qagent wait` and bus_wait
src/supervisor/v2.ts     port  supervisor.ts:230-369 loop over core instead of HTTP
src/supervisor.ts, adapters.ts, config.ts, router.ts, discover.ts, provider-catalog.ts,
instance-processes.ts, fake-harness.ts, openai-compatible-harness.ts   reuse (supervisor side only)
src/dashboard/{server,page,assets,session}.ts   new; session.ts ports product-server.ts:65-133
broker.ts, product-server.ts, static-web.ts, product-runtime.ts, lifecycle.ts, process-management.ts,
operator-control.ts, operator-mcp.ts, config-transitions.ts, integrations.ts, appearance.ts,
resolve-target.ts, cli.ts, cli-view.ts, protocol.ts, mcp-server.ts, store.ts        drop
web/, gui/, plugins/agent-bus-dashboard/, scripts/{browser-smoke,installed-*,safari-*,verify-production-bundle}.mjs,
scripts/{build-gui.sh,daemonize.js}                                               drop
```

### Schema

```sql
PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000;  -- reuse store.ts:49-51

CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);            -- schema_version, import:<sha256>
CREATE TABLE agents (
  id TEXT PRIMARY KEY CHECK (id GLOB '[A-Za-z0-9._-]*'),                   -- same rule as security.ts:18
  role TEXT NOT NULL DEFAULT '', model TEXT NOT NULL DEFAULT '', harness TEXT NOT NULL DEFAULT '',
  parent_id TEXT REFERENCES agents(id) ON DELETE SET NULL,
  status TEXT NOT NULL DEFAULT 'offline',   -- idle|waiting|working|offline (derived when wait_until < now)
  wait_until_ms INTEGER, last_seen_ms INTEGER, created_ms INTEGER NOT NULL, meta_json TEXT NOT NULL DEFAULT '{}');
CREATE TABLE identities (                                                  -- reuse store.ts:57-64
  agent_id TEXT PRIMARY KEY, token_hash TEXT NOT NULL UNIQUE,
  authority TEXT NOT NULL CHECK (authority IN ('operator','manager','worker')),
  permissions_json TEXT NOT NULL, created_ms INTEGER NOT NULL, updated_ms INTEGER NOT NULL);
CREATE TABLE messages (
  seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT NOT NULL UNIQUE, ts_ms INTEGER NOT NULL,
  sender TEXT NOT NULL, recipient TEXT,                                    -- NULL = broadcast (prototype model)
  type TEXT NOT NULL DEFAULT 'info', subject TEXT NOT NULL DEFAULT '', body TEXT NOT NULL,
  thread TEXT NOT NULL DEFAULT '', task_id INTEGER, refs_json TEXT NOT NULL DEFAULT '[]',
  requires_ack INTEGER NOT NULL DEFAULT 0, source TEXT NOT NULL DEFAULT 'v2');   -- v2|bus.jsonl|qagent|prototype
CREATE INDEX messages_inbox ON messages(recipient, seq);
CREATE INDEX messages_thread ON messages(thread, seq);
CREATE INDEX messages_task ON messages(task_id, seq);
CREATE TABLE cursors (agent_id TEXT PRIMARY KEY, last_seq INTEGER NOT NULL DEFAULT 0);
CREATE TABLE acks (seq INTEGER NOT NULL, agent_id TEXT NOT NULL, ack_ms INTEGER NOT NULL, PRIMARY KEY (seq, agent_id));
CREATE TABLE tasks (
  id INTEGER PRIMARY KEY AUTOINCREMENT, legacy_id TEXT UNIQUE, project TEXT,  -- project root replaces runs
  parent_id INTEGER REFERENCES tasks(id) ON DELETE SET NULL,
  title TEXT NOT NULL, brief TEXT NOT NULL DEFAULT '', acceptance TEXT NOT NULL DEFAULT '',
  role TEXT NOT NULL DEFAULT '', priority TEXT NOT NULL DEFAULT 'normal',
  state TEXT NOT NULL CHECK (state IN ('open','blocked','claimed','submitted','changes_requested',
                                        'accepted','failed','cancelled')),
  creator TEXT NOT NULL, assignee TEXT, reviewer TEXT,
  path_scopes_json TEXT NOT NULL DEFAULT '[]', refs_json TEXT NOT NULL DEFAULT '[]',
  result_json TEXT, review_json TEXT, round INTEGER NOT NULL DEFAULT 1,
  attempts INTEGER NOT NULL DEFAULT 0, max_retries INTEGER NOT NULL DEFAULT 2,
  claim_expires_ms INTEGER, created_ms INTEGER NOT NULL, updated_ms INTEGER NOT NULL);
CREATE INDEX tasks_board ON tasks(state, assignee, updated_ms);
CREATE INDEX tasks_parent ON tasks(parent_id);
CREATE INDEX tasks_project ON tasks(project, state);
CREATE TABLE task_deps (task_id INTEGER NOT NULL, depends_on INTEGER NOT NULL, PRIMARY KEY (task_id, depends_on));
CREATE INDEX task_deps_rev ON task_deps(depends_on);
CREATE TABLE task_notes (id INTEGER PRIMARY KEY AUTOINCREMENT, task_id INTEGER NOT NULL, author TEXT NOT NULL,
  ts_ms INTEGER NOT NULL, body TEXT NOT NULL);
CREATE INDEX task_notes_task ON task_notes(task_id);
CREATE TABLE leases (project TEXT NOT NULL, path TEXT NOT NULL, task_id INTEGER NOT NULL,   -- port store.ts:113-120
  created_ms INTEGER NOT NULL, PRIMARY KEY (task_id, path));
CREATE INDEX leases_project ON leases(project, path);
CREATE TABLE events (seq INTEGER PRIMARY KEY AUTOINCREMENT, ts_ms INTEGER NOT NULL, actor TEXT NOT NULL,
  kind TEXT NOT NULL, entity TEXT NOT NULL, entity_id TEXT NOT NULL, data_json TEXT NOT NULL DEFAULT '{}',
  source TEXT NOT NULL DEFAULT 'v2');
CREATE INDEX events_entity ON events(entity, entity_id, seq);
CREATE TABLE usage (agent_id TEXT NOT NULL, day TEXT NOT NULL, turns INTEGER, input_tokens INTEGER,
  output_tokens INTEGER, cost_usd REAL, latency_ms INTEGER, PRIMARY KEY (agent_id, day));  -- written by supervisor only
```

`events.seq` is the change counter. It replaces the in-memory `stateRevision` (`broker.ts:190`) and the `bus.jsonl` audit file (`broker.ts:239-246`) **[drop]**. Dropped tables: `runs`, `routing_decisions` and `telemetry` (`store.ts:90-112`). A task's project path replaces the run, and routing moves to the supervisor.

**Task rules [port].** A claim is one transaction; it wins only if exactly one row changed, and it takes the path leases with the overlap rule from `store.ts:351-369`:

```sql
UPDATE tasks SET state='claimed', assignee=:me, claim_expires_ms=:t
 WHERE id=:id AND state IN ('open','changes_requested') AND (assignee IS NULL OR assignee=:me);
```

Submitting sends a `result` message to the reviewer, by default the creator (`broker.ts:1117-1128`). Accepting releases leases and unblocks dependents (`broker.ts:652-663`); requesting changes increments the round (`broker.ts:1178-1207`). Claims expire after two hours; with no background sweeper, the next board write reopens them. Input limits come from `broker.ts:113-176`.

**Identity [reuse/port].** A process names itself with `QAGENT_AGENT_ID` (old name `AGENT_ID` still works, `mcp-server.ts:30`). The library hashes `tokens/<id>.token` (`security.ts:17-20`) and matches it against `identities`; the sender is always that identity. Adding agents, rotating tokens, and cancelling or reviewing any task need `operator.token` (`broker.ts:255-272`, `:321-325`).

### CLI

```text
qagent init                                     create bus.db, operator token (rotate if file/hash mismatch)
qagent agent add <id> --role R [--model M --harness H --parent P --authority worker|manager]   operator
qagent agent list | qagent token rotate <id>    operator for rotate
qagent whoami | qagent status [--json]          status = agents, open tasks, unread counts; no daemon
qagent send <to|a,b|*> <subject> [body|-] [--thread T] [--task N] [--ack]
qagent inbox [--peek] [--limit N] [--json]      advances cursor unless --peek
qagent ack <seq>
qagent wait [--timeout SEC] [--json]            blocks until mail or a task event for me; exit 0 = got mail, 2 = timeout
qagent task add <title> [--brief -] [--to ID] [--parent N] [--dep N]... [--scope PATH]... [--project DIR]
qagent task list [--mine] [--state S]... [--all] | task show <N>
qagent task claim [<N>] | task note <N> <text> | task submit <N> --summary S [--file F]...
qagent task review <N> --accept|--revise --feedback F | task cancel <N> [--reason R]
qagent task stalled [--stall-min M] | task requeue <N> [--reason R]   idle claims back to open
qagent log [--follow] [--since SEQ]             events feed; --follow uses core/changes.ts
qagent trace <N> [--format text|json|html] [--out FILE]               the task's causal chain
qagent import [--jsonl P] [--qagent-state P] [--prototype P] [--dry-run] [--force]
qagent mcp [--operator]                         stdio MCP server (lane 2)
qagent mcp-config [--agent ID] [--client claude|codex]   prints registration snippet (lane 2)
qagent supervise <agent> [dir] [--auto-requeue-min M] | supervise --roster | doctor   optional supervisor (lane 3)
qagent dashboard [--port 11512] [--open]        optional dashboard (lane 4)
```

`agent-bus` stays as an alias. `--as <id>` or `--as operator` picks the token file.

### MCP tools

The `bus_` prefix stays; `protocol/PROTOCOL.md` already uses it.

```ts
bus_whoami      {}
bus_agents      {}
bus_send        { to: string /* id | "a,b" | "*" */, subject: string, body: string,
                  type?: "info"|"question"|"answer", thread?: string, task_id?: number,
                  refs?: {type:"path"|"artifact"|"summary"|"commit"|"url", value:string, description?:string}[],
                  requires_ack?: boolean }
bus_inbox       { peek?: boolean, limit?: number /* ≤200 */ }
bus_wait        { timeout_sec?: number /* 1..3600, default QAGENT_BLOCK_SEC or 240 */ }
bus_ack         { seq: number }
bus_task_create { title: string, brief: string, to?: string, parent_id?: number, dependencies?: number[],
                  path_scopes?: string[], acceptance?: string, role?: string, project?: string,
                  priority?: "low"|"normal"|"high"|"urgent" }
bus_task_list   { mine?: boolean /* default true */, state?: string[], include_closed?: boolean, limit?: number }
bus_task_get    { task_id: number }
bus_task_claim  { task_id?: number }   // omitted: most urgent open task assigned to me or unassigned for my role
bus_task_note   { task_id: number, note: string }
bus_task_submit { task_id: number, summary: string, details?: string, changed_files?: string[],
                  artifacts?: Ref[], validation?: {passed:boolean, summary:string, command?:string}[] }
bus_task_review { task_id: number, accepted: boolean, feedback: string }
bus_task_cancel { task_id: number, reason?: string }
// with --operator only:
bus_agent_add   { id: string, role: string, model?: string, harness?: string, parent?: string, authority?: "worker"|"manager" }
```

14 agent tools, against the prototype's 25 (`prototype_0.2/ARCHITECTURE.md`). `bus_route_task` (`mcp-server.ts:215-253`) and the 13 `qagent_*` operator tools (`operator-mcp.ts:34-163`) are **[drop]**.

### How an idle agent hears about new mail

`core/changes.ts` **[new]** runs `fs.watch` on the database directory. When `bus.db` or `bus.db-wal` changes it waits 25 ms, then reads `max(seq)` from `events`, a primary-key lookup. A 10 s safety check covers missed file events. API: `next(sinceSeq, timeoutMs): Promise<number>`.

- **`bus_wait` and `qagent wait` [new].** The waiter writes `status='waiting', wait_until_ms` once, blocks on `next()`, and returns when its inbox has mail. Nothing else is written while idle; today every wait writes (`broker.ts:981`, `store.py:360`). A dead waiter shows offline once `wait_until_ms` passes.
- **Signal file [new].** On delivery, `bus.ts` atomically rewrites `inbox/<agent>.seq` with the recipient's latest sequence number, for harness hooks or shell loops that cannot open SQLite.
- **Supervisor [port].** When running, it holds the wait for its agent and wakes the CLI with the existing prompt (`supervisor.ts:80-104`).

### Dashboard

A separate process, `src/dashboard/server.ts` **[new]**, on 127.0.0.1:11512. Port 11511 stays with the old broker until cutover.

```text
GET  /                 server-rendered page (no framework, no build step)
GET  /app.js /app.css  static strings from assets.ts; CSP default-src 'self'
POST /login            {operatorToken} -> one-time ticket     (port product-server.ts:665-669)
POST /session          {ticket} -> HttpOnly SameSite=Strict cookie  (port product-server.ts:423-430)
GET  /api/state        agents, open tasks, last 100 messages, seq
GET  /api/events       SSE: "change" {seq, events[], messages[], tasks[]} deltas; ": ping" every 25 s
GET  /api/task/:id     task with notes and thread
POST /api/send         {to, subject, body} as operator; session + same-origin required (port :126-133)
GET  /health
```

**Data flow.** One read connection and one change watcher per dashboard process. Each change triggers one delta query (`events WHERE seq > last` plus the rows they name), sent to every open stream, so cost does not grow with tabs. `POST /api/send` is the only write. Review and cancel move to the CLI, start and stop to `qagent supervise`; configuration editing, provider discovery and appearance settings (`product-server.ts:488-608`) are **[drop]**.

**Look.** `#111` background, `#ddd` text, one accent (`#6aa0ff`) for links and focus, system font at 14 px, monospace ids. Four stacked sections: a status line (database path, agents online, last change), Agents, Open tasks, and Messages with a filter and a compose form. Status is a plain word. Below 600 px, tables become lines with a 16 px gutter. `DESIGN.md` is rewritten; its card chrome (`DESIGN.md:77-107`) is **[drop]**.

## What gets removed

| Group | Lines | Why |
|---|---|---|
| Broker, product server, lifecycle, process ownership, operator MCP/control, config editing, integrations, appearance, delegate resolver (12 files) | 4,135 | They exist to serve or guard the HTTP broker and the configuration UI |
| `cli.ts`, `cli-view.ts`, `protocol.ts`, `mcp-server.ts`, `store.ts` | 1,976 rewritten as about 900 | Mostly HTTP response parsers (`protocol.ts:404-882`); `cli.ts:59-136` duplicates `lifecycle.ts:26-91` |
| `web/` (React and Vite) | 999 | Replaced by one server-rendered page |
| `gui/` (Swift) | 1,053 | HTTP client of the removed broker |
| `plugins/agent-bus-dashboard/` code | 6,354 | Third viewer; its launchers fail today |
| Browser, installed and GUI scripts | 1,061 | Tested the bundled SPA and release install |
| Tests for removed modules (12 files) | 1,842 | Replaced by core, MCP, dashboard and supervisor tests |

Kept supervisor-side code: 2,698 lines (the eleven reused files in the module map). New code: about 2,300 lines, an estimate. The dependencies `react`, `react-dom`, `vite` and `@types/react*` go. The runtime needs only `@modelcontextprotocol/sdk` and `zod`, plus `node:sqlite`. The engine requirement moves from Node 22.5 to 22.13, the first release where `node:sqlite` needs no flag; this machine runs 24.21.

## Migration

The first v2 migration combined data from an older JSONL broker, a SQLite broker, and the Python prototype. The commands below remain useful when those source formats are present.

1. Stop every process that writes to the legacy stores.
2. Back up `state.sqlite`, `prototype.db` and `bus.jsonl` before importing them.
3. Run `qagent init`, then `qagent import --dry-run`, then `qagent import`. The home directory stays `~/.agent-bus`, because `~/.qagent` does not exist (`protocol.ts:16-23`). The importer:
   - loads `bus.jsonl` messages, deduplicated by message id, since both brokers append to the same file; registrations become agent rows; task events become `events` rows only, because the file holds summaries, not task rows;
   - loads `state.sqlite` agents, messages, tasks (keeping the old id in `legacy_id`) and identities, so existing `tokens/*.token` files keep working;
   - loads `prototype.db` messages (keeping broadcasts), tasks (`todo→open`, `assigned→open` with the assignee kept, `in_progress→claimed`, `review→submitted`, `done→accepted`), notes and events;
   - sets every agent's cursor to the highest imported sequence number, so no agent wakes to months of history;
   - records each source's sha256 in `meta`, so a rerun changes nothing without `--force`;
   - does not import `dashboard-state.json` (pins, hidden projects, dashboard-only roles). It stays archived.
4. Replace legacy MCP registrations after adding the new identities. `qagent mcp-config --client claude|codex` prints the supported configuration. Tokens remain in files.
5. Disable any legacy dashboard or MCP plugin after the v2 clients work.
6. Keep the source databases read-only as an archive until the import has been checked.

## Build plan

The original implementation used an isolated worktree and separate lanes. Paths below are relative to the repository. At baseline, `npm run check` passed 84 unit tests. On macOS, `test:browser` can use `CHROME_BIN="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"`.

The lanes own separate files. No lane deletes or edits a file another lane owns, so the old code and its tests keep passing until integration. `test:dist` runs `git diff --exit-code -- dist` and sees only changed tracked files. Lanes 1, 2 and 4 add only new files, so `npm run check` must stay green for them. Lane 3 edits three tracked sources, so `test:dist` fails for it until the founder approves a commit that includes `dist/`. Lane 3 runs every other step of `check`. No lane commits.

Two new smoke scripts are run with `node` until integration wires them into `test:lifecycle` and `test:browser`.

```text
All lanes:  npm run build:core && npm run test:unit        (tsc, then node --test over dist-test/tests/*.test.js)
Lanes 1,2,4 also:  CHROME_BIN="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" npm run check
Lane 3 also:  npm run build && npm run test:bundle && npm run test:unit && CHROME_BIN=… npm run test:browser && npm run test:lifecycle
Lane 1 smoke:  node scripts/v2-cli-smoke.mjs
Lane 4 smoke:  CHROME_BIN=… node scripts/v2-dashboard-smoke.mjs
```

**Lane 1: core, schema, CLI, import.** Runs first. Lanes 2 to 4 start after it passes.

```text
create: src/core/{types,db,identity,bus,changes,import}.ts, src/cli/{main,format}.ts, src/qagent.ts,
        tests/core-bus.test.ts, tests/core-claim.test.ts, tests/core-identity.test.ts,
        tests/core-changes.test.ts, tests/core-import.test.ts, scripts/v2-cli-smoke.mjs
edit:   nothing existing
```

Acceptance tests:
- Two agents exchange messages and finish the whole cycle (create, claim, note, submit, request changes, submit again, accept) through `node dist/qagent.js` with no listener on any port.
- 20 processes claim one task at once, and exactly one wins.
- A token for agent A cannot act as agent B, and the operator commands reject agent tokens.
- `changes.next()` fires within 200 ms of a write from another process.
- The importer's dry-run counts on fixture copies match the source counts, and a second run changes nothing.
- An idle `qagent wait` leaves `bus.db-wal` unchanged for 60 s.

**Lane 2: MCP server, wait, notification.**

```text
create: src/mcp/{server,render,config}.ts, src/notify/wait.ts, tests/mcp-server.test.ts, tests/wait-notify.test.ts
edit:   protocol/PROTOCOL.md
```

Acceptance tests:
- The MCP SDK client lists exactly the 14 tools, plus `bus_agent_add` with `--operator`.
- `bus_send` has no sender field.
- `bus_wait` returns within 500 ms of a send from the CLI, and times out cleanly.
- The server starts and works with no broker present.
- Killing it with `kill -9` during a wait loses no mail.
- The signal file updates on delivery.

**Lane 3: optional supervisor.**

```text
create: src/supervisor/v2.ts, src/supervisor/entry.ts, tests/supervisor-v2.test.ts
edit:   src/supervisor.ts (export sanitizedEnvironment only), src/adapters.ts (additive optional mcpCommand in
        AdapterContext), src/config.ts (autoStart default false), tests/adapters.test.ts,
        tests/agent-hierarchy.test.ts (port to core)
owns, edit only if needed: src/router.ts, src/discover.ts, src/provider-catalog.ts, src/instance-processes.ts,
        src/fake-harness.ts, src/openai-compatible-harness.ts, src/supervisor-launch.ts
```

Acceptance tests:
- A fake-harness agent under `qagent supervise` claims and submits a CLI-created task, using `qagent mcp` with its own identity.
- After the supervisor stops, the agent is still reachable by `bus_wait`.
- API-key stripping still works (`supervisor.ts:106-118`), and so does the process-group kill (`:153-165`).

**Lane 4: dashboard.**

```text
create: src/dashboard/{server,page,assets,session,entry}.ts, tests/dashboard.test.ts, scripts/v2-dashboard-smoke.mjs
edit:   DESIGN.md
```

Acceptance tests:
- The server binds 127.0.0.1 only.
- `/api/*` returns 401 without a session and rejects a cross-origin send.
- A message sent by an agent reaches an open stream within 500 ms, and a dashboard send reaches that agent's `qagent wait`.
- With 3 open tabs and no writes for 5 minutes, the server logs at most one database query per 10 s safety check.
- The page has no horizontal scroll at 375 px.

**Integration (the coordinating session, after lanes 1 to 4).** Delete the files marked **[drop]**. Point the `qagent` and `agent-bus` bins at `dist/qagent.js`. Make `build` run `tsc` only, point `test:lifecycle` and `test:browser` at the two v2 smoke scripts, and remove `test:bundle`. Remove the React and Vite dependencies. Move `src/supervisor/v2.ts` into `src/supervisor.ts`. Rewrite `README.md`, `docs/architecture.md` and `docs/security.md`.

**Final check list.**
- In the worktree, `npm run build` then `npm run test:unit`, `npm run test:lifecycle` (v2 CLI smoke) and `CHROME_BIN=… npm run test:browser` (v2 dashboard smoke) pass. `npm run check` passes once `dist/` is committed with the founder's approval.
- `lsof -iTCP -sTCP:LISTEN` shows no Qagent listener while two real CLIs, one Claude and one Codex, exchange a message and finish a task.
- Import counts are recorded against the migration table above.
- A 10-minute idle soak with 10 waiters and 3 dashboard tabs leaves the WAL size unchanged and uses under 1% CPU.
- `grep` finds no `fetch(` to a bus URL in `src/core`, `src/mcp` or `src/notify`.
- The dashboard is not reachable on a non-loopback address.

## Risks and open decisions for the founder

1. **The security boundary is the Unix user.** Any process running as you can write `bus.db` directly, as it can already read token files (`docs/security.md:215`). Tokens stop accidental or prompt-induced impersonation, not a hostile local process. `bus.jsonl` shows an agent already sending as `opus` with `opus`'s token because the operator token was stale. Decision: accept (recommended) or keep a writer daemon, which R1 rules out.
2. **Sandboxed agents writing the database is unverified.** MCP servers are normally launched outside the harness sandbox, but a `qagent` call from a sandboxed shell may be refused writes to `~/.agent-bus`. Decision: keep one global database (recommended, after a live Codex check) or use one per project under `.qagent/`, which splits history.
3. **Tool names change.** Agents and prompts that call `mcp__agent-coordinator__*` stop working. Decision: rename cleanly (recommended; the lane 4 review found four such calls in Claude transcripts) or ship prototype-name aliases for 30 days.
4. **Supervisor scope.** Lane 3 keeps session-resume supervision as it is. `docs/orchestration-rework-review.md` proposes per-task spawning instead. Decision: build lane 3 minimal now, skip it and leave Qagent archived as the lane 4 review recommends, or plan the spawn model separately.
5. **How much history to import.** `bus.jsonl` holds 40 MB of message bodies. Decision: import everything, with cursors advanced (recommended), or import only metadata plus the last 30 days.
6. **Dashboard port.** Decision: 11512 during migration, 11511 once the old broker is gone (recommended).
