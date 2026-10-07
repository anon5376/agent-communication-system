# Provider and harness support

This document separates implemented code from live account verification. A configured entry is not a claim that a provider grants a particular model to the current user.

## Status vocabulary

- **Implemented and tested:** adapter/control-plane behaviour is covered by automated tests without consuming external usage.
- **Implemented, live unverified:** command construction and output normalization exist and are unit-tested against recorded command lines and output (`tests/adapters.test.ts`), but no real call to the provider is made in CI or recorded in this repository. Provider flags drift, so treat these as needing a local check.
- **Adapter-ready:** the normalized interface exists and configuration is present, but invocation needs local validation.
- **Researched path:** an official or realistic compatible route exists, but no first-class adapter is claimed.
- **Unsupported:** no defensible integration is enabled.

## Matrix

| Provider / route | Harness | Authentication source | Prototype status | Notes |
|---|---|---|---|---|
| Anthropic | Claude Code | Normal Claude Code login / subscription | Implemented, live unverified | Headless `-p`, exact `--resume`, model selection, JSON output, MCP injection, permission allow-list and usage parsing are normalized. |
| OpenAI | Codex CLI | Normal Codex login, including eligible ChatGPT plans, or explicit API auth | Implemented, live unverified | Pinned original chats use exact `queue --thread`; managed headless chats use exact `exec resume <id>`. MCP config, full-access execution, model/reasoning controls, JSON events and token parsing are normalized. |
| Google | Gemini CLI | Normal Google/Gemini CLI login | Implemented, live unverified | Headless JSON prompt, model selection, YOLO approval and `--resume <latest-or-index>` are normalized. Gemini's CLI does not currently expose arbitrary UUID resume through this flag. |
| Moonshot AI | Kimi Code CLI | Normal Kimi Code login/provider configuration | Implemented, live unverified | Prompt, exact `--session`, autonomous permissions, stream-JSON output and model selection are normalized. |
| OpenCode ecosystem | OpenCode | Provider accounts configured through OpenCode | Implemented, live unverified | Project-local MCP config, autonomous `run`, exact `--session`, model/variant selection, JSON events and `opencode models` discovery are supported. |
| Z.AI / GLM | OpenCode | GLM-capable provider configured through OpenCode | Implemented, live unverified | Uses the OpenCode harness and its exact-session resume path. The catalog seed matches the locally discovered `opencode-go/glm-5.3`; replace the exact selector when the authenticated catalog differs. |
| Local models | Codex `--oss` / OpenCode | Local Ollama or compatible endpoint | Researched path and adapter configuration | Local inference is intentionally reached through a real coding harness rather than pretending the broker itself is an agent. Exact model availability comes from the local runtime. |
| Ollama | Codex `--oss` or OpenCode | Local runtime or configured Ollama account | Researched path | Official Ollama documentation describes Codex `--oss`; no separate fake Ollama coding-agent adapter is added. |
| LM Studio | OpenCode or OpenAI-compatible endpoint | Local LM Studio runtime | Researched path | LM Studio exposes local model listing and OpenAI-compatible APIs. A dedicated `lms` discovery adapter is not yet wired. |
| Hermes | Hermes-compatible CLI | Local/provider profile | Implemented, live unverified | Profile selection, noninteractive chat query, exact `--resume`, quiet output and automatic approvals are normalized. |
| Cursor | Cursor CLI (`cursor-agent` / `agent`) | Cursor account / `agent login` / `CURSOR_API_KEY` | Implemented, live unverified | Print mode, exact `--resume`, model selection and MCP injection are normalized. Models available through Cursor are selected with `--model`. |
| xAI / Grok | Grok CLI | `grok login` | Implemented, live unverified | Headless `-p`, JSON output, exact `--resume`, automatic approvals and model selection are normalized. |
| Cognition / Devin | Devin CLI (`devin`) | `devin auth login` | Implemented, live unverified | Print mode `-p`, `--permission-mode dangerous`, exact `--resume` and `--model` are normalized. Devin has no per-run MCP flag, so the harness declares `mcp: false`: the supervisor claims the task and submits Devin's printed answer. The adapter reads no session id from print output, so a turn resumes only a pinned `resumeSessionId`. |
| Deterministic fake | Fake harness | None | Implemented and tested | Used for routing, task graph, retry, failure, cancellation and supervisor simulations without external calls. |
| Other providers | Generic command adapter | Subscription, API or local | Implemented, live unverified | Configure `args` plus `resumeArgs` with the `{session}` placeholder. Adding a model provider behind an existing harness does not require a new resume implementation. |

Every row marked "Implemented, live unverified" above was previously labelled "invocation tested". That meant the unit tests above, not a live run; the label was changed on 2026-10-04 so it cannot be read as a live test.

## Which CLIs can join an aos crew

In the `aos-v0.1.0` release only Claude Code, Codex CLI and Cursor CLI can join a crew, because only their adapters handed the agent the bus tools on every turn. On `rust-port` (not yet released), `aos connect <cli> --auto-approve` adds Gemini CLI, Kimi, OpenCode, Hermes, Grok and other CLIs, either with the bus tools or supervisor-managed; the full table is in [`rust/AOS.md`](https://github.com/anon5376/agent-communication-system/blob/rust-port/rust/AOS.md#connect-any-agent-cli). Their command lines were checked against each CLI's `--help`, not run live. `qagent supervise` can launch any adapter from `.qagent/config.json`, but an agent can only report back if its CLI is given the bus MCP tools or runs `qagent` itself from its shell.

## Model discovery

The prototype supports discovery only where a CLI safely exposes it:

- OpenCode: `opencode models` through adapter discovery.
- Other official CLIs: registry configuration unless a stable enumeration command is available.
- Ollama/LM Studio: documented external discovery paths, not silently scraped by the broker.

`qagent doctor` checks the bus and the operator token. `qagent doctor <agent> [project]` also checks that agent's identity, its entry in the supervisor configuration, and that the configured harness command is on PATH. It does not run the CLI, scan for other providers, or report login commands, and it does not infer login state, subscription entitlement, remaining quota or model access.

## Primary references inspected

- Claude Code CLI reference: https://docs.anthropic.com/en/docs/claude-code/cli-reference
- Claude Code MCP: https://docs.anthropic.com/en/docs/claude-code/mcp
- OpenAI Codex CLI: https://developers.openai.com/codex/cli/reference
- OpenAI Codex repository/authentication: https://github.com/openai/codex
- Gemini CLI: https://github.com/google-gemini/gemini-cli
- Kimi Code CLI: https://github.com/MoonshotAI/kimi-cli
- OpenCode CLI: https://opencode.ai/docs/cli/
- Ollama Codex integration: https://docs.ollama.com/integrations/codex
- LM Studio CLI and APIs: https://lmstudio.ai/docs/cli
- Node.js SQLite: https://nodejs.org/api/sqlite.html

Provider interfaces change. The adapter configuration is the source of truth for this prototype; claims above are intentionally narrow.
