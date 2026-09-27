# Architecture

Qagent is a library over one SQLite file, wrapped by a CLI and a stdio MCP server. Every process that coordinates, whether an agent's CLI call, an MCP server or the dashboard, opens the database itself. There is no broker, so there is no process whose absence stops agents from talking. The supervisor and the dashboard are separate programs you start only when you want them. `docs/V2-DESIGN.md` is the design record, with the reasons and the migration plan.

## Layout

| Path | What it does |
|---|---|
| `src/core/` | The coordination library: schema, identity, messages, tasks, leases, change detection, import. Imports nothing outside `core/`. |
| `src/cli/`, `src/qagent.ts` | The `qagent` command. Supervisor, dashboard and MCP commands are loaded lazily, so plain commands never load them. |
| `src/mcp/` | `qagent mcp` (stdio MCP server with 14 `bus_*` tools, plus `bus_agent_add` with `--operator`) and `qagent mcp-config`. |
| `src/notify/wait.ts` | The shared wait used by `qagent wait` and `bus_wait`. |
| `src/supervisor.ts`, `src/supervisor/entry.ts` | Optional `qagent supervise` and `qagent doctor`. |
| `src/adapters.ts`, `config.ts`, `router.ts`, `discover.ts`, `provider-catalog.ts`, `instance-processes.ts`, `fake-harness.ts`, `openai-compatible-harness.ts`, `security.ts`, `supervisor-launch.ts` | Supervisor-side code kept from the previous version: harness adapters, harness configuration, and helpers. Core does not import them. |
| `src/dashboard/` | Optional `qagent dashboard`: one server-rendered page with live updates. |

## The database

`~/.agent-bus/bus.db` in WAL mode with a 5 s busy timeout. Tables: `agents`, `identities` (token hashes), `messages`, `cursors` and `acks`, `tasks`, `task_deps`, `task_notes`, `leases`, `events`, `usage` (reserved for the supervisor, not yet written) and `meta`. The full schema is in `src/core/db.ts`.

Every write is one `BEGIN IMMEDIATE` transaction that also appends a row to `events`. `events.seq` is the change counter every reader uses, and the `events` table is the only history; there is no separate audit file. Opening an existing database performs no write.

## Messages

A message goes to one agent, or to everyone when its recipient is empty. Each agent has a read cursor. `inbox` returns messages after the cursor and advances it; `--peek` leaves it alone. After each delivery the library rewrites `inbox/<agent>.seq` next to the database with the recipient's newest sequence number, for hooks and shell loops that cannot open SQLite.

## Tasks

States: `open`, `blocked`, `claimed`, `submitted`, `changes_requested`, `accepted`, `failed`, `cancelled`.

- A task is created assigned (`--to`) or unassigned with a role. An assigned task also sends a `[TASK #N]` message.
- A claim is one `UPDATE ... WHERE state IN ('open','changes_requested') AND (assignee IS NULL OR assignee = me)`. It wins only if a row changed, so twenty processes claiming one task produce exactly one owner. The claim takes path leases in the task's project and fails when they overlap another task's leases.
- Claims expire after two hours; a note renews them. No sweeper runs: the next task write reopens expired claims.
- Submitting sends a `[DONE #N]` result to the reviewer (the creator by default). Accepting releases leases and unblocks dependents. Requesting changes starts another round; past the retry limit the task fails and the creator gets an escalation.
- The assignee can release a claim, and the supervisor reports a failed turn, which reopens the task for a retry or fails it once attempts are used up.

## Waiting without a daemon

`core/changes.ts` watches for commits by other processes. It polls `PRAGMA data_version`, which costs no disk write, backing off from 10 ms to 100 ms, and an `fs.watch` on the database directory wakes it early. Only when the version moves does it read `max(events.seq)`. The wait adds a watch on the agent's signal file, so a delivery wakes it at once.

A waiter writes `status = 'waiting'` and a deadline once, blocks, and writes `idle` once when it returns. A waiter that is killed leaves `waiting` behind; readers show it as offline once the deadline passes, and the agent's next action resets it.

## Supervisor

`qagent supervise <agent> [dir]` holds the wait for one agent. When mail or task activity arrives it renders a brief and starts the agent's CLI through its harness adapter, with a `qagent mcp` entry running as that agent. For harnesses without MCP, the supervisor claims the task, and afterwards either submits the CLI's answer or reports the failure. One pid file per agent prevents two supervisors from driving the same session. Stopping the supervisor kills the CLI's process group. The agent stays reachable through `bus_wait` afterwards.

## Dashboard

`qagent dashboard` opens one read connection and one change watcher, whatever the number of browser tabs. Each change produces one delta query (the new events and the rows they name), which is pushed to every open stream over server-sent events. A 10 s safety check catches missed file events, and a comment ping every 25 s keeps streams open. The only write is `POST /api/send`, a message from the operator.

## What was removed

The HTTP broker and its in-memory state, the product server and React dashboard, the Swift GUI, the Python dashboard plugin, the operator MCP tools, runs, the router in the coordination path, configuration editing from the browser, and the release installer. `bus.jsonl`, `state.sqlite` and the prototype database can be imported with `qagent import`.
