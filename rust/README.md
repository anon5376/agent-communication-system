# acs — the Rust port of qagent

A from-scratch Rust implementation of the Agent Communication System bus and CLI.
It shares the SQLite schema, token files, inbox signal files, and event log
semantics with the TypeScript implementation in `../src`, so a `bus.db` written by
either binary is fully readable — and wakeable — by the other.

## Build and test

```sh
cargo build --release     # produces target/release/qagent
cargo test                # 37 tests ported from tests/core-*.test.ts + supervisor/adapters
```

## What is ported (v1)

- **Core bus** (`src/bus.rs`): init/adopt/rotate, agents, send/inbox/ack,
  broadcast, task lifecycle (add/claim/note/submit/review/release/fail/cancel),
  dependency blocking, path leases, claim expiry sweep, events, status.
- **Data layer** (`src/db.rs`): identical `SCHEMA_SQL` (meta, agents, identities,
  messages, cursors, acks, tasks, task_deps, task_notes, leases, events, usage),
  WAL + busy_timeout(5000) + foreign_keys, 0700/0600 permissions.
- **Identity** (`src/identity.rs`): sha256 token hashes, base64url bearer tokens,
  token files under `<home>/tokens/`, operator adoption and repair.
- **Change watcher** (`src/watcher.rs`): `PRAGMA data_version` polling with
  exponential backoff plus `notify` filesystem wake, same idle-cost profile as
  the TS `ChangeWatcher`.
- **Wait + signal files** (`src/wait.rs`): `waitForMail` semantics — peek fast
  path, `agent_waiting`/`agent_idle` events, `<home>/inbox/<agent>.seq` signal
  files, task-event wake — and Ctrl-C → exit 130, timeout → exit 2.
- **Full CLI** (`src/cli.rs`): `init`, `agent add|token rotate`, `send`, `inbox`,
  `ack`, `wait`, `status`, `whoami`, `log [--follow]`, `doctor`, `import`,
  `supervise`, `fake-harness`, and
  the whole `task` family — same flags, same exit codes, same `--json` output.
- **Config** (`src/config.rs`): full `loadConfig`/`resolveAgent`/
  `configPathFromProject`/`validateConfig` parity — providers, harnesses (with
  feature sets), models (with capabilities + exactModel), agents (permissions,
  resumeSessionId, harnessOptions), roles, routing, constraints.
- **Adapters** (`src/adapters.rs`): all 10 harness adapters verbatim — claude,
  codex, kimi, gemini, cursor, grok, opencode, hermes, fake, and the `command`
  escape hatch — arg building, MCP launch lines, prepare hooks, and output
  normalization.
- **Supervisor** (`src/supervisor.rs`): `qagent supervise` — provider-key env
  sanitization, pid-file lock, wait->claim->brief->run->fail/submit loop,
  process-group spawn with SIGTERM->SIGKILL timeout, head+tail output cap,
  session.json accounting, exponential backoff retry.
- **Fake harness** (`qagent fake-harness`): the `src/fake-harness.ts` test stand-in
  as a hidden subcommand — success/fail/fail-once/malformed/hang/bus-cli modes.
- **Import** (`src/import.rs`): the v1 migration — reads `bus.jsonl`, a
  `state.sqlite` legacy store, and a `prototype.db` coordinator into a v2 bus
  with per-source sha256 idempotency, `--dry-run` against an in-memory schema,
  and the same inserted/duplicates/invalid report accounting.

## Deliberately not ported (v1)

`mcp`, `mcp-config`, `dashboard` — "not implemented in the Rust port yet" stubs.
Keep using the Node `qagent` for those; both binaries share the same `bus.db`
safely. Note `supervise` already runs managed (non-MCP) harnesses end-to-end;
MCP-capable harnesses get the `mcp` stub error until that port lands.

## Interop

Verified live in this repo: TS `send` → Rust `inbox` (and reverse), shared
cursors, signal files written by either side, `qagent wait` on Rust woken by a
TS send and vice versa (~ms-scale wake latency through the WAL + signal file).

## Layout notes vs the TS source

- `Bus::write` = the `write()` transaction wrapper: BEGIN IMMEDIATE on the
  outermost call, nested calls join, pending inbox signal files flush after
  commit (never on rollback).
- `ChangeWatcher` owns its own `Connection` so same-process writes count as
  changes too (`data_version` only tracks *other* connections).
- `identity_for_token`, `claim_task`'s UPDATE...RETURNING race, and
  `reopen_expired_claims` mirror the improved semantics from the TypeScript
  efficiency pass (priority claim ordering, paginated candidate scan, batched
  dependency lookup, single-scan unread, cancel-notifies-expired-claimer).
