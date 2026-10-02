<h1 align="center">
  <img src="docs/assets/banner.png" alt="Agent Communication System: one SQLite file to coordinate every coding agent on your machine" width="100%">
</h1>

<p align="center">
  <a href="https://github.com/anon5376/agent-communication-system/actions/workflows/universal-harness-ci.yml"><img src="https://github.com/anon5376/agent-communication-system/actions/workflows/universal-harness-ci.yml/badge.svg?branch=main" alt="CI"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-6aa0ff" alt="MIT license"></a>
  <img src="https://img.shields.io/badge/node-%E2%89%A522.13-6aa0ff" alt="Node.js 22.13 or newer">
  <img src="https://img.shields.io/badge/MCP-stdio%20server-6aa0ff" alt="MCP stdio server">
  <a href="https://github.com/anon5376/agent-communication-system/stargazers"><img src="https://img.shields.io/github/stars/anon5376/agent-communication-system?style=flat&color=6aa0ff" alt="GitHub stars"></a>
</p>

<p align="center">
  <b>Durable mail, task handoffs and review gates for Claude Code, Codex, Gemini, Hermes and any other agent CLI.<br>No daemon, no cloud, no broker.</b>
</p>

<p align="center">
  <a href="#quick-start">Quick start</a> ·
  <a href="docs/FULL-GUIDE.md">Full guide</a> ·
  <a href="#terminal-ui">Terminal UI</a> ·
  <a href="docs/security.md">Security</a>
</p>

The command is `qagent` (`agent-bus` remains as a compatibility alias). The CLI and MCP server open one SQLite file directly, so messaging and task work need no background process. An optional supervisor wakes agent CLIs when work arrives, and an optional local dashboard shows activity.

![aos terminal demo: accepting a review, sending one back, a message and a new task from command home, then the goal tree and swarm](docs/assets/aos-demo.gif)

*`aos demo`, the terminal console from the Rust implementation on the `rust-port` branch. To try it: `git checkout rust-port && ./rust/install-aos.sh && aos demo`. See [Terminal UI](#terminal-ui).*

**Why not…**

- *…CrewAI/AutoGen/LangGraph?* Different layer. Those define agents inside a runtime; ACS is the mailbox and task board underneath the standalone CLIs you already run — mix Claude Code, Codex, and a local model on one bus.
- *…a message queue?* A broker moves bytes; it doesn't know what an agent task is. ACS ships agent-shaped primitives — token identities, atomic claims, path leases, review gates — with no server to run.
- *…tmux send-keys and a shared doc?* A doc can't push, can't expire stale claims, and can't wake an agent. ACS delivers addressed durable mail, claims with leases, and signal-file wake-ups.

## What it provides

- Verified agent identities with per-agent tokens.
- Direct, multi-recipient, and broadcast messages.
- Threads, acknowledgements, typed messages, and file or URL references.
- Tasks with assignment, dependencies, claims, path leases, progress notes, submission, and review by someone other than the assignee.
- A stdio MCP server with 14 agent tools and one operator-only tool.
- Harness adapters: `claude`, `codex`, `gemini`, `kimi`, `cursor`, `grok`, `opencode`, `hermes`, plus any other CLI through the `command` adapter.
- A localhost-only dashboard and an optional supervisor.
- Import tools for earlier Qagent and Python prototype stores.

## Quick start

Requires Node.js 22.13 or newer. The package is not on npm yet, so build from source:

```bash
git clone https://github.com/anon5376/agent-communication-system.git
cd agent-communication-system
npm ci
npm run build
npm link

qagent init
qagent agent add claude --role manager --authority manager
qagent agent add codex --role worker
```

After the first npm release (not published yet), `npm install -g agent-communication-system` or `npx -p agent-communication-system qagent <command>` will replace the clone-and-build steps.

Every agent names itself with `QAGENT_AGENT_ID` or `--as <id>`:

```bash
qagent --as claude send codex "parser" "Please take the parser task."
qagent --as codex inbox
qagent --as codex wait --timeout 600
```

Generate MCP client configuration without copying tokens into configuration files:

```bash
qagent mcp-config --agent claude --client claude
qagent mcp-config --agent codex --client codex
```

## Terminal UI

The terminals live in the Rust implementation on the `rust-port` branch and are not built from this branch: `aos`, the console in the demo, and `acs`, the earlier terminal UI. Both open the same `bus.db`, tokens and signal files as `qagent`, so all three can drive one bus. To install `aos`, check out `rust-port` and run `./rust/install-aos.sh` (needs the Rust toolchain and a C compiler); `aos demo` opens a sample bus in a temporary directory, and plain `aos` opens yours. Keys, commands and limits are in [`rust/AOS.md`](https://github.com/anon5376/agent-communication-system/blob/rust-port/rust/AOS.md); `acs` install steps are in the [`rust-port` README](https://github.com/anon5376/agent-communication-system/blob/rust-port/README.md), and command differences in [Implementation differences](docs/FULL-GUIDE.md#implementation-differences).

## Documentation

- [Full guide](docs/FULL-GUIDE.md) — setup, every messaging mode, task workflow, MCP tools, supervision, dashboard, migration, and troubleshooting.
- [Agent protocol](protocol/PROTOCOL.md) — the instruction block managers and workers use.
- [Security model](docs/security.md) — trust boundaries, identity, storage, and residual risks.
- [Architecture](docs/architecture.md) — components and data flow.
- [Provider support](docs/provider-support.md) — supported harnesses and their limits.
- [v2 design record](docs/V2-DESIGN.md) — historical design decisions behind the current implementation.
- [Contributing](CONTRIBUTING.md) — development checks and public-release hygiene.

## Optional supervisor

The supervisor waits for one agent and launches its configured CLI when work arrives. It exists only while you run it.

```bash
qagent doctor codex /workspace/project
qagent supervise codex /workspace/project
```

Project harness configuration lives at `<project>/.qagent/config.json`. Logs go to `~/.agent-bus/logs/`.

## Optional dashboard

```bash
qagent dashboard
qagent dashboard link
```

The dashboard binds only to `127.0.0.1:11511`. It prints a single-use sign-in link and never exposes the operator token to the browser.

## Development

Build, test and public-release checks are in [CONTRIBUTING.md](CONTRIBUTING.md).

## Security

The bus protects identities from accidental impersonation by another agent. It is not a security boundary against a hostile process running as the same operating-system user. Messages, task briefs, and results are stored in plaintext in `bus.db`; protect the bus directory accordingly.

Report vulnerabilities through [GitHub Security Advisories](https://github.com/anon5376/agent-communication-system/security/advisories/new), not a public issue.

## Feedback

Bug reports, harness requests and "this didn't work for me" stories go in [issues](https://github.com/anon5376/agent-communication-system/issues). If ACS saves you from copy-pasting briefs between terminals, a star helps other people find it.

## License

[MIT](LICENSE)
