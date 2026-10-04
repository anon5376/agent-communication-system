# Submission pack

> **Superseded for launch (2026-10-04).** Use [launch-copy.md](launch-copy.md) for wording and [launch-checklist.md](launch-checklist.md) for steps. This file is kept as background research; its numbers and claims (binary size, speed, star counts, feature status) were not re-checked and some are wrong, for example the `aos` binary is about 5 MB, not 3 or 4 MB. Check any claim against the [claims table](launch-copy.md#claims-and-evidence) before reusing it.

Paste-ready copy for every directory and list in [promotion-playbook.md](promotion-playbook.md).
Fields marked ✅ were verified against the live form/schema on 2026-09-30; anything else is
noted inline.

## Shared copy

| Field | Value |
|---|---|
| Name | `qagent` — Agent Communication System |
| Repo URL | `https://github.com/anon5376/agent-communication-system` |
| Author | `anon5376` · `https://github.com/anon5376` · `anon5376@users.noreply.github.com` (swap in any monitored address for contact-email fields) |
| License | MIT |
| Tagline (~100 chars) | `Local-first mail + task queue for coding agents — one SQLite file, no daemon.` |
| One-liner (≤200 chars) | `Local-first coordination bus for coding agents: durable agent-to-agent mail, task queues with claims and review gates, shared state in one SQLite file — no daemon, no cloud. Stdio MCP server + CLI.` |
| Longer description | `Agent Communication System gives local coding agents durable mail, task handoffs, review gates, and shared state — all in one SQLite file with no broker or daemon. Ships a stdio MCP server (14 agent tools + 1 operator tool), a CLI, harness adapters for Claude Code/Codex/Gemini/Kimi/OpenCode/OpenAI-compatible CLIs, an optional supervisor, and a localhost dashboard. Requires Node.js ≥22.13.` |
| GitHub topics | `mcp-server`, `mcp`, `ai-agents`, `multi-agent`, `local-first`, `sqlite`, `claude-code`, `developer-tools` |

Install command — today (source build):

```bash
git clone https://github.com/anon5376/agent-communication-system.git
cd agent-communication-system && npm ci && npm run build && npm link
qagent init
```

Install command — only valid after `npm publish` (the package is not on npm yet; until then clone, `npm ci`, `npm run build`, `npm link`):

```bash
npm install -g agent-communication-system
qagent init
```

MCP run (stdio): `qagent mcp` with `QAGENT_AGENT_ID=<id>` (optional `QAGENT_BUS_DB=<path>`;
default `~/.agent-bus/bus.db`). Equivalent one-shot: `npx -y -p agent-communication-system qagent mcp`.
Client registration snippet generator: `qagent mcp-config --agent <id> --client claude|codex`.

## Prerequisite: npm publish (blocks the official registry, Smithery scan, Terminal Trove, and makes every listing's install command real)

`package.json` still has `"private": true`. Before publishing to npm:

```jsonc
{
  // remove: "private": true
  "license": "MIT",
  "mcpName": "io.github.anon5376/agent-communication-system", // required by the official registry for npm ownership verification
  "repository": { "type": "git", "url": "https://github.com/anon5376/agent-communication-system.git" }
}
```

Then `npm publish --access public`. Keep `version` in `server.json` equal to the published
package version (currently `0.2.0`).

---

## 1. Official MCP Registry — registry.modelcontextprotocol.io ✅

Manifest: `server.json` at repo root (validated against `server.schema.json` 2025-12-11).

- `name`: `io.github.anon5376/agent-communication-system`
- `version` / `description`: as in `server.json` (description is capped at 100 chars)
- `packages[0]`: npm `agent-communication-system` + positional arg `mcp` + env `QAGENT_AGENT_ID` (required), `QAGENT_BUS_DB` (optional)

Steps:

```bash
npm publish --access public          # after the package.json edits above
brew install mcp-publisher           # or the tarball from modelcontextprotocol/registry releases
mcp-publisher validate server.json   # optional sanity check
mcp-publisher login github           # device-flow GitHub auth, grants io.github.anon5376/*
mcp-publisher publish                # run from the repo root containing server.json
```

Verify: `curl "https://registry.modelcontextprotocol.io/v0/servers?search=agent-communication-system"`.

## 2. Glama — glama.ai ✅

Submit at `https://glama.ai/mcp/servers` → "Add MCP Server":

- GitHub repository URL: `https://github.com/anon5376/agent-communication-system`
- Display name: `qagent — Agent Communication System`
- Short description: the one-liner above

Claim ownership: `glama.json` (repo root, this PR) lists `maintainers: ["anon5376"]`.
After it merges, re-run the "Claim ownership" flow on the server page so Glama picks it up.
One submission also feeds `punkpeye/awesome-mcp-servers`.

## 3. Smithery — smithery.ai ✅/⚠️

Manifest: `smithery.yaml` at repo root (`startCommand.type: stdio`, `configSchema` with a
required `agentId`, `commandFunction` running `node dist/qagent.js mcp`).

Submit: `https://smithery.ai/new` → sign in with GitHub → paste the repo URL.

⚠️ Caveat: Smithery's current docs push two publish paths — remote servers via Streamable-HTTP
URL, and local stdio servers via an `.mcpb` bundle upload. The repo-`smithery.yaml` build flow
is still what in-the-wild files (e.g. `microsoft/mcp`) use, but confirm the repo-URL intake is
still offered at submit time; if not, the path is packaging a `qagent.mcpb` bundle instead.

## 4. mcpservers.org ✅ (also the intake for `wong2/awesome-mcp-servers`, which takes no PRs)

Form at `https://mcpservers.org/submit`:

- Server Name: `qagent — Agent Communication System`
- Short Description: the one-liner above
- Link: `https://github.com/anon5376/agent-communication-system`
- Category: `Development` (backup pick: `Communication`)
- Contact Email: a monitored address (`anon5376@users.noreply.github.com` placeholder — substitute the real contact at submit time)

Free listing, or $39 premium (faster review, badge, dofollow link).

## 5. mcp.so ✅

Form at `https://mcp.so/submit`:

- Repository URL: `https://github.com/anon5376/agent-communication-system`
- Name: `qagent — Agent Communication System`

Free review queue, or $39 paid (immediate publish, verified badge, dofollow link).

## 6. hesreallyhim/awesome-claude-code ✅ — issue form only, PRs rejected

File at `https://github.com/hesreallyhim/awesome-claude-code/issues/new?template=recommend-resource.yml`.
Form fields:

- Title: `[Resource]: qagent — Agent Communication System`
- Display Name: `qagent — Agent Communication System`
- Category: `Agent Orchestration`
- Link: `https://github.com/anon5376/agent-communication-system`
- Author Name: `anon5376`
- Author Link: `https://github.com/anon5376`
- Description (10–500 chars, 1–3 sentences, descriptive not promotional):

  > `An MCP server and CLI that gives Claude Code instances a durable inbox, a shared task queue with claims and review gates, and shared state — all in one local SQLite file, no daemon. Also supports Codex, Gemini, Kimi, and OpenAI-compatible CLIs.`

- Checklist: tick the five real boxes; leave the trap box ("Do not check the following box") **unchecked**.

Gate: the resource needs ≥14 days of active development OR ≥100 stars. The repo was created
2026-09-27 — do not file before **2026-10-11** unless it has 100★ by then.

## 7. e2b-dev/awesome-ai-agents — PR to README.md ✅

Entries are a flat alphabetical list; each is `## Name` / tagline / `### Category` /
`### Description` (bullets) / `### Links`. Insert under Q:

```markdown
## Qagent
Local-first coordination bus for autonomous coding agents

### Category
Multi-agent, Coding

### Description
- Durable agent-to-agent mail and a shared task queue with claims, leases, and review gates
- All coordination state in one local SQLite file — no broker, daemon, or cloud
- Stdio MCP server with 14 agent tools, plus CLI, optional supervisor, and localhost dashboard
- Harness adapters for Claude Code, Codex, Gemini, Kimi, OpenCode, and OpenAI-compatible CLIs

### Links
- GitHub: https://github.com/anon5376/agent-communication-system
```

## 8. CharlieLZ/awesome-agents — ⚠️ target does not exist

`github.com/CharlieLZ/awesome-agents` returns 404 (the account exists; no such repo). The
playbook's description — "has agent-communication/protocol categories" — best matches
`kyrolabs/awesome-agents` (~2.9k★), which takes PRs in single-line format. File a PR adding to
the `## Frameworks` section, alphabetically under Q:

```markdown
- [qagent](https://github.com/anon5376/agent-communication-system): Local-first coordination bus for autonomous coding agents — durable mail, task queues with claims and review gates, shared state in one SQLite file; stdio MCP server + CLI. ![GitHub Repo stars](https://img.shields.io/github/stars/anon5376/agent-communication-system?style=social)
```

## 9. adw0rd/awesome-mcp-servers — PR to README.md ✅

One line, alphabetical inside the `### 🤖 Coding Agents` section:

```markdown
- [anon5376/agent-communication-system](https://github.com/anon5376/agent-communication-system) 📇 🏠 🍎 🪟 🐧 - Local-first coordination bus for coding agents: durable mail, task queues with claims/leases and review gates, one SQLite file. Requires Node.js ≥22.13. Install: `npm i -g agent-communication-system`.
```

(📇 TypeScript, 🏠 local service, 🍎/🪟/🐧 macOS/Windows/Linux.) After Glama lists it, add the
score badge like neighboring entries:
`[![qagent MCP server](https://glama.ai/mcp/servers/anon5376/agent-communication-system/badges/score.svg)](https://glama.ai/mcp/servers/anon5376/agent-communication-system)`.
Their CONTRIBUTING requires reproducible install + connection instructions — the line above
covers it once the npm package exists.

## 10. Terminal Trove — terminaltrove.com/submit ✅

Fields (verified):

- `url`: `github.com/anon5376/agent-communication-system` (no `https://`)
- `tagline` (~100 chars): `Local-first mail + task queue for coding agents — one SQLite file, no daemon.`
- Checkbox: "I have checked, and this is a different tool."
- `describe your tool` (100–400 chars): the one-liner above + ` Requires Node.js ≥22.13.`
- `2-3 standout features` (60–400 chars): `Durable agent-to-agent mail and task queue with claims/leases/review gates in one SQLite file — no daemon, no cloud. Stdio MCP server + CLI; harness adapters for Claude Code, Codex, Gemini, Kimi, OpenCode.`
- `primary language` / `license`: auto-read from the repo (TypeScript, MIT).
- Preview image: PNG **required**, GIF recommended — record the 30–60s demo GIF first.
- Category tags: `ai`, `llm`, `coding-agents` (AI) + `linux`, `macos`, `windows`, `cross-platform` (Platforms).
- Install Instructions: per-platform package-manager commands — the tool must exist in package
  repositories, so submit only **after** `npm publish`: `npm install -g agent-communication-system`.

## 11. DevHunt — devhunt.org ✅

Submit at `https://devhunt.org/account/tools/new` (GitHub sign-in required). The form asks for:

- Tool name: `qagent — Agent Communication System`
- Website: `https://github.com/anon5376/agent-communication-system`
- Tagline + description: tagline and longer description above
- Logo: 220×220 or 210×210 PNG
- Screenshots: dashboard + terminal captures
- Category: developer tools / AI (their form explicitly accepts "AI agents and MCP servers")
- Launch week: free queue (multi-month wait reported) or $49 to pick a week

---

## Verification status / unknowns

- ✅ Verified against live docs/forms: official registry schema (validated locally against
  `server.schema.json@2025-12-11`), Glama `glama.json`, Smithery `smithery.yaml`, mcpservers.org
  form, mcp.so form, awesome-claude-code issue form, e2b-dev and adw0rd README formats,
  Terminal Trove form, DevHunt FAQ/submit path.
- ⚠️ `CharlieLZ/awesome-agents` does not exist (404) — substituted `kyrolabs/awesome-agents`.
- ⚠️ Smithery repo-URL + `smithery.yaml` intake is the established path but their current docs
  foreground URL/MCPB publishing — re-check `smithery.ai/new` at submit time.
- ⛔ npm publish is the hard prerequisite for the official registry (`mcpName` check),
  Terminal Trove (package-repo installs), and every "install command" field.
- ⛔ awesome-claude-code maturity gate: wait until 2026-10-11 or 100 stars.
