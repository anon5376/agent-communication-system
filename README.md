# Agent Communication System

**One SQLite file to coordinate every coding agent on your machine — no daemon, no cloud.**

Agent Communication System gives local coding agents durable mail, task handoffs, review gates, and optional automatic wake-ups. The command is `qagent`; `agent-bus` remains as a compatibility alias.

Coordination lives in one SQLite file. The CLI and MCP server open it directly, so ordinary messaging and task work need no broker or background daemon. An optional supervisor can wake agent CLIs, and an optional local dashboard shows activity.

![acs terminal UI demo](docs/assets/acs-demo.gif)

*The `acs` terminal UI (Rust, `rust-port` branch; not part of the npm package or this branch's build). See [Terminal UI](#terminal-ui).*

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

The `acs` terminal UI in the demo above is part of the Rust implementation on the `rust-port` branch. It is not built by this branch and is not in the npm package. It opens the same `bus.db`, tokens, and signal files as the TypeScript `qagent`, so both can drive one bus. To build it, follow the install steps in the [`rust-port` README](https://github.com/anon5376/agent-communication-system/blob/rust-port/README.md) (Rust toolchain, then `./rust/install.sh`). The two implementations differ in some commands; see [Implementation differences](docs/FULL-GUIDE.md#implementation-differences).

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

```bash
npm run audit:public
npm run build
npm run test:unit
npm run test:lifecycle
npm run test:browser
npm run check
```

`npm run audit:public:history` also checks commit metadata and every reachable revision. Run it before publishing a repository or release archive.

## Security

The bus protects identities from accidental impersonation by another agent. It is not a security boundary against a hostile process running as the same operating-system user. Messages, task briefs, and results are stored in plaintext in `bus.db`; protect the bus directory accordingly.

Report vulnerabilities through [GitHub Security Advisories](https://github.com/anon5376/agent-communication-system/security/advisories/new), not a public issue.

## License

[MIT](LICENSE)
