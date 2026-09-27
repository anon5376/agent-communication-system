# Provider and harness support

This document separates implemented code from live account verification. A configured entry is not a claim that a provider grants a particular model to the current user.

## Status vocabulary

- **Implemented and tested:** adapter/control-plane behaviour is covered by automated tests without consuming external usage.
- **Implemented, live unverified:** command construction and normalization exist, but this branch did not spend a real subscription call in CI.
- **Adapter-ready:** the normalized interface exists and configuration is present, but invocation needs local validation.
- **Researched path:** an official or realistic compatible route exists, but no first-class adapter is claimed.
- **Unsupported:** no defensible integration is enabled.

## Matrix

| Provider / route | Harness | Authentication source | Prototype status | Notes |
|---|---|---|---|---|
| Anthropic | Claude Code | Normal Claude Code login / subscription | Implemented; invocation tested | Headless `-p`, exact `--resume`, model selection, JSON output, MCP injection, permission allow-list and usage parsing are normalized. |
| OpenAI | Codex CLI | Normal Codex login, including eligible ChatGPT plans, or explicit API auth | Implemented; invocation tested | Pinned original chats use exact `queue --thread`; managed headless chats use exact `exec resume <id>`. MCP config, full-access execution, model/reasoning controls, JSON events and token parsing are normalized. |
| Google | Gemini CLI | Normal Google/Gemini CLI login | Implemented; invocation tested | Headless JSON prompt, model selection, YOLO approval and `--resume <latest-or-index>` are normalized. Gemini's CLI does not currently expose arbitrary UUID resume through this flag. |
| Moonshot AI | Kimi Code CLI | Normal Kimi Code login/provider configuration | Implemented; invocation tested | Prompt, exact `--session`, autonomous permissions, stream-JSON output and model selection are normalized. |
| OpenCode ecosystem | OpenCode | Provider accounts configured through OpenCode | Implemented; invocation tested | Project-local MCP config, autonomous `run`, exact `--session`, model/variant selection, JSON events and `opencode models` discovery are supported. |
| Z.AI / GLM | OpenCode | GLM-capable provider configured through OpenCode | Implemented; invocation tested | Uses the OpenCode harness and its exact-session resume path. The catalog seed matches the locally discovered `opencode-go/glm-5.3`; replace the exact selector when the authenticated catalog differs. |
| Local models | Codex `--oss` / OpenCode | Local Ollama or compatible endpoint | Researched path and adapter configuration | Local inference is intentionally reached through a real coding harness rather than pretending the broker itself is an agent. Exact model availability comes from the local runtime. |
| Ollama | Codex `--oss` or OpenCode | Local runtime or configured Ollama account | Researched path | Official Ollama documentation describes Codex `--oss`; no separate fake Ollama coding-agent adapter is added. |
| LM Studio | OpenCode or OpenAI-compatible endpoint | Local LM Studio runtime | Researched path | LM Studio exposes local model listing and OpenAI-compatible APIs. A dedicated `lms` discovery adapter is not yet wired. |
| Hermes | Hermes-compatible CLI | Local/provider profile | Implemented; invocation tested | Profile selection, noninteractive chat query, exact `--resume`, quiet output and automatic approvals are normalized. |
| Cursor | Cursor CLI (`cursor-agent` / `agent`) | Cursor account / `agent login` / `CURSOR_API_KEY` | Implemented; invocation tested | Print mode, exact `--resume`, model selection and MCP injection are normalized. Models available through Cursor are selected with `--model`. |
| xAI / Grok | Grok CLI | `grok login` | Implemented; invocation tested | Headless `-p`, JSON output, exact `--resume`, automatic approvals and model selection are normalized. |
| Deterministic fake | Fake harness | None | Implemented and tested | Used for routing, task graph, retry, failure, cancellation and supervisor simulations without external calls. |
| Other providers | Generic command adapter | Subscription, API or local | Implemented; invocation tested | Configure `args` plus `resumeArgs` with the `{session}` placeholder. Adding a model provider behind an existing harness does not require a new resume implementation. |

## Model discovery

The prototype supports discovery only where a CLI safely exposes it:

- OpenCode: `opencode models` through adapter discovery.
- Other official CLIs: registry configuration unless a stable enumeration command is available.
- Ollama/LM Studio: documented external discovery paths, not silently scraped by the broker.

`agent-bus doctor` scans the known provider catalog (PATH plus well-known install locations) and reports login commands for missing CLIs. It also probes enabled harnesses. It does not infer login state, subscription entitlement, remaining quota or model access from the presence of a binary.

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
