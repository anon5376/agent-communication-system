# acs — the Rust port of qagent

A from-scratch Rust implementation of the Agent Communication System bus and CLI.
It shares the SQLite schema, token files, inbox signal files, and event log
semantics with the TypeScript implementation in `../src`, so a `bus.db` written by
either binary is fully readable — and wakeable — by the other.

## Build and test

See the [source-build prerequisites](../README.md#install-the-tui-acs) before
continuing. Run the following commands from this `rust/` directory:

```sh
cargo build --release
cargo test
./install.sh
```

The install script builds `acs` and installs it globally in `/usr/local/bin`,
or falls back to `~/.local/bin` if needed. See the linked installation guide for
PATH setup and troubleshooting.

After `install.sh`, the TUI starts from anywhere with `acs` (or `acs --db /path/to/bus.db`).

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
  `supervise`, `fake-harness`, `mcp`, `mcp-config`, and
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
- **MCP server** (`src/mcp.rs`): `qagent mcp [--operator]` — newline-delimited
  JSON-RPC 2.0 stdio transport, all 14 agent tools + `bus_agent_add` under
  `--operator`, per-call identity re-check, `bus_wait` on a worker thread with
  its own connection so waits don't stall other calls, `notifications/cancelled`
  aborts a wait, drain-on-shutdown.
- **MCP config** (`src/mcp_config.rs`): `qagent mcp-config` — Claude/Codex
  registration snippets with the same TOML escaping and env pinning rules.
- **Dashboard** (`src/dashboard.rs` + `src/dashboard_page.rs`): `qagent
  dashboard [serve|link]` — hand-rolled HTTP/1.1 on 127.0.0.1 with the full
  guard set (loopback Host, same-origin writes, JSON-only bodies, CSP nonces),
  operator-token → single-use ticket → session cookie sign-in, and SSE fan-out:
  one `ChangeWatcher` loop publishes per-connection channel streams so a slow
  client never stalls the loop, with `event: reset` on delta overflow. The HTML
  and client JS ship verbatim — the browser re-renders rows with the same
  renderers the server used for first paint.
- **Terminal control app** (`src/app.rs` + the `acs`/`acs-app` bins): a ratatui
  TUI for non-technical operators — first run auto-creates the bus and shows a
  help card (`?` anytime); live agents / tasks / message panes fed by the same
  `ChangeWatcher`; send via a pick-a-recipient popup (`m`), create tasks (`t`),
  add agents (`a`), read any item (`enter`), cancel with confirmation (`x`),
  click to focus a pane, scroll to select. ~5 MB binary, no webview; install it
  globally with `./install.sh`.
- **AOS terminal** (`src/aos/` + the `aos` bin): the Acceleration Chamber
  console on the same bus: causal spine, gate strip for reviews and stalled
  claims, goal tree, evidence, retro, 80x24 down to 60x20, truecolor, 16-colour
  and NO_COLOR. `aos demo` opens a sample bus. Install with `./install-aos.sh`;
  details in [AOS.md](AOS.md).
- **OpenAI-compatible harness** (`src/openai_harness.rs` + the
  `qagent-openai-compatible` / `agent-bus-openai-compatible` bins): the generic
  chat/completions caller from `src/openai-compatible-harness.ts`, verbatim —
  same flag parser, same env-pinned auth header, same normalized result JSON.
- **Import** (`src/import.rs`): the v1 migration — reads `bus.jsonl`, a
  `state.sqlite` legacy store, and a `prototype.db` coordinator into a v2 bus
  with per-source sha256 idempotency, `--dry-run` against an in-memory schema,
  and the same inserted/duplicates/invalid report accounting.

## Deliberately not ported (v1)

Nothing left — every `qagent` subcommand now has a Rust implementation.
`supervise` runs MCP-capable harnesses end-to-end (the `mcp` subcommand it
launches is the Rust stdio server), and the dashboard serves the same HTML/JS
the Node build serves.

## Interop

Verified live in this repo: TS `send` → Rust `inbox` (and reverse), shared
cursors, signal files written by either side, `qagent wait` on Rust woken by a
TS send and vice versa (~ms-scale wake latency through the WAL + signal file).

Both bus cores enforce stored delegation, allowed-child, depth and concurrent
claim policies inside write transactions and preserve them across token rotation.
Both supervisors stop before running work if the configured policy cannot be
applied (for example, widening an existing restriction without an operator token).
Correct the config to retain the stored restriction, or have the operator
explicitly authorise the wider policy, then restart. A rejected mixed update
does not partially apply its tighter fields.

Both supervisors use the same atomic hard-link ownership protocol. Only one
supervisor can run an agent at a time, including across Rust and TypeScript.
An empty/corrupt ownership file or an abandoned `.pid.reap` cleanup lock fails
closed rather than risking removal of a new owner's lock. Stop all starters
for that agent and confirm no supervisor is running before removing the named
stale file and retrying. Ordinary dead-supervisor PID files recover automatically.

## Platform notes

- **Windows (`x86_64-pc-windows-msvc`)**: the crate builds and the full test
  suite passes. The OS seams live in `src/platform.rs`.
- File privacy (`0600`/`0700` on tokens, db and directories) is Unix-only.
  `platform::chmod_private` is a no-op on Windows; token and bus files rely on
  the per-user profile ACLs of `%USERPROFILE%`/`%LOCALAPPDATA%` — the same
  limitation the security doc spells out.
- Process liveness is `OpenProcess` + `GetExitCodeProcess` (`STILL_ACTIVE`);
  a foreign command line cannot be read, so a live pid counts as "the
  supervisor".
- Supervisor-spawned agent trees run detached
  (`CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW`, the `setsid` equivalent) and
  are registered in a Job Object — stop terminates the whole tree via
  `TerminateJobObject`. `KILL_ON_JOB_CLOSE` is deliberately not set: the tree
  must outlive the launcher.
- A detached supervisor has no console to receive a Ctrl event, so graceful
  stop is a `<agent>.stop` file it polls next to its pid file.
- `aos autostart` (launchd/systemd) returns "not supported on Windows" rather
  than faking it.
- Provider CLIs resolve through PATH + PATHEXT (`.exe`/`.cmd`/`.bat`), so
  npm-installed shims are found. `.cmd`/`.bat` run through
  `cmd.exe /d /s /c` with every argument quoted; an argument cmd.exe cannot
  represent (`%`, `"`, a newline) is refused rather than interpolated.

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
