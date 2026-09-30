# Changelog

All notable changes to the Agent Communication System. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); this project uses
[Semantic Versioning](https://semver.org/).

## [0.2.0] — 2026-09-30

First public release.

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
