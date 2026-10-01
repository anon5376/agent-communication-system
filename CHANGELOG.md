# Changelog

All notable changes to the Agent Communication System. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); this project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- **Per-task git worktrees** — `qagent task claim --worktree` (and
  `bus_task_claim` with `worktree: true`) gives the task its own checkout on
  branch `qagent/task-<N>-<id>` under `~/.agent-bus/worktrees/`;
  `qagent task worktree <N> [--remove]` and `task worktree prune` manage them.
  With `"isolation": "worktree"` the supervisor runs single-task turns for a
  claimed task inside that checkout and resumes a per-task CLI session.
  TypeScript build only: the Rust port ignores `"isolation": "worktree"` and
  has no `--worktree` flag or `task worktree` command; the bus schema is
  unchanged, so both builds still share one bus.
- **Stalled-task detection** — claims whose assignee went quiet are listed by
  `qagent task stalled`; `qagent task requeue` returns a task to the pool;
  `qagent supervise --auto-requeue-min <n>` requeues dead claims automatically.
- **`qagent trace <id>`** — a task's full causal chain (events, notes, task
  mail) in one timeline, as text, `--format json`, or a self-contained HTML
  export via `--export`.
- **`qagent supervise --roster`** — runs every roster agent from one
  supervisor process; a dead sibling no longer orphans the rest.
- **Registry manifests** — `server.json`, `glama.json`, `smithery.yaml` for
  MCP-registry submission, plus `docs/submission-pack.md`.
- **Demo + docs** — `acs` TUI demo GIF in the README, competitive analysis,
  promotion playbook, standout-features list, launch copy, and a full
  marketing strategy with a 30-day calendar.

### Changed

- **Efficiency pass** — cached prepared statements on hot paths, batched
  dashboard delta reads (`agentSummaries`, `messageSummaries`), indexed
  router lookups, cheaper idle waits, narrower auto-claim candidate reads.
- **npm packaging** — `private` removed, `files` whitelist (~129 KB tarball),
  `publishConfig`, `prepack` build, `repository` and `mcpName` metadata;
  the README install section is written for the post-publish state; until the
  first publish it leads with clone, `npm ci`, `npm run build`, `npm link`.

### Fixed

- Worktree creation no longer blocks on a stale lock: a lock whose holder
  died, never wrote its owner file, or stopped heartbeating (pid reuse) is
  swept. Worktree cleanup (`task worktree --remove`, `prune`) now works after
  the task's project directory is deleted; if the whole repository is gone the
  orphaned checkout is deleted with `--force`.
- README and docs no longer claim what the code does not back: the install
  section leads with clone-and-build (the package is not on npm yet), review is
  described as "by someone other than the assignee" (the gate does not check
  model family), the `acs` TUI is labelled as the `rust-port` branch's, the
  adapter list matches `ADAPTERS`, and the competitive analysis now includes
  Hermes Agent. `npm run audit:public` also runs `scripts/check-readme-claims.mjs`.
- `tests/wait-notify.test.ts` no longer fails on one slow wake-up under load:
  the `bus_wait` test holds the median of five rounds to the bound, the signal-file test
  asserts against the poll interval, and a fake-clock test pins the poll bound.
- Expired claims can be released and requeued; a batch of expired claims
  requeues correctly after the mid-batch expiry sweep; the auto-requeue
  sweep no longer eats live claims.
- `public-release-audit` accepts an exact-address `allowedEmails` whitelist
  (the published SECURITY.md contact) — main's CI is green again.
- Claim-race test no longer crashes the suite on an expected child-stdin
  EPIPE.

## [0.2.0] — 2026-09-30

Tagged on GitHub as `v0.2.0`; not yet published to npm.

### Added

- **Bus core** — durable agent identities, addressed mail, task lifecycle
  (create → assign → claim → submit → review → release), atomic claims,
  path leases, claim expiry, and a full event log — all in one SQLite
  file with no daemon.
- **`qagent` CLI** — init, agent/identity management, send/inbox/ack/wait,
  task add/list/show/claim/note/submit/review/release/cancel, deps, leases,
  status, log, doctor, import, mcp-config.
- **MCP stdio server** — the bus as agent tools (send, inbox, wait, task
  ops) plus operator tools; `qagent mcp-config` writes provider configs.
- **Supervisor + harness adapters** — launches real agent CLIs (Claude Code,
  Codex, Gemini, Kimi, OpenCode, Grok, Hermes, Cursor, generic command
  adapter), routes tasks by role/capability, brief injection, progress
  tracking, API-key sanitization in child environments.
- **Dashboard** — localhost operator console (agents, tasks, message stream)
  with single-use sign-in tickets and SSE live updates.
- **Docs** — README, FULL-GUIDE, V2-DESIGN, architecture, provider-support,
  security, and `free-ai-setup.md` (zero-cost team on Gemini free tier /
  OpenCode Zen / Ollama / OpenRouter, with a recommended all-free preset).

### Fixed

- Change polling stays responsive after early file events.
- CI test scheduling stabilized.
