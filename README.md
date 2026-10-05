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
  <b>Run different coding agents together without being their message bus.</b><br>
  Claude Code, Codex and other agent CLIs claim work from one local task board, recover when a worker dies, and get checked by a reviewer who did not write the code.
</p>

<p align="center">
  <a href="#try-it-in-two-minutes">Try it</a> ·
  <a href="#watch-a-worker-die-and-the-work-survive">Recovery example</a> ·
  <a href="#see-what-needs-you">What needs you</a> ·
  <a href="#what-is-tested-and-what-is-not">Limits</a> ·
  <a href="docs/FULL-GUIDE.md">Full guide</a> ·
  <a href="docs/security.md">Security</a>
</p>

If you run more than one coding agent, you end up as the glue: pasting briefs between terminals, remembering who is editing what, and checking the work yourself. ACS puts that state in one SQLite file on your machine, so the agents coordinate through it instead of through you. No daemon, no cloud, no broker: every command opens the file directly.

- **Ownership.** A task has one assignee at a time. Claims are atomic, claims on overlapping paths are refused (path leases are cooperative, not enforced by the filesystem), and every agent acts under its own token.
- **Recovery.** When a worker dies mid-task its claim shows up in `task stalled`; requeue it and another agent picks it up from the notes the first one left. `qagent trace` shows the whole chain afterwards.
- **Independent review.** A worker cannot accept its own submission. Only the named reviewer (or the creator when none is named) or you, the operator, can accept or send it back.

![aos terminal demo: accepting a review, sending one back, a message and a new task from command home, then the goal tree and swarm](docs/assets/aos-demo.gif)

*`aos demo`: a sample team on a temporary bus. Nothing real runs.*

## Try it in two minutes

`aos` is the terminal console. Prebuilt binaries exist for Linux (x86_64, ARM64) and macOS (Apple Silicon, Intel), released as [`aos-v0.1.0`](https://github.com/anon5376/agent-communication-system/releases/tag/aos-v0.1.0). There is no Windows build.

```bash
curl -fsSL https://raw.githubusercontent.com/anon5376/agent-communication-system/rust-port/install.sh | sh
aos demo
```

`aos demo` seeds a sample team in a bus under your temp folder (`$TMPDIR/aos-demo`). It needs no account, starts no agent CLI and never opens your real bus. Quit with `q`, then `y`.

### Your first real task

```bash
cd your-project
aos
```

The first run looks for agent CLIs on your machine and writes a crew of three (lead, builder, reviewer), taking the reviewer from a different CLI than the builder when you have two. In the `aos-v0.1.0` release, Claude Code, Codex CLI and Cursor CLI can join a crew. On `rust-port`, and in the next release, `aos connect <cli> --auto-approve` also adds Gemini CLI, Kimi, OpenCode, Hermes, Grok and other CLIs; those run unattended without approval prompts, so aos asks you to type `--auto-approve` once. None of them has been live-tested. `aos doctor` says what is missing. Type a goal as a sentence and press Enter; the agents work in this folder, and the result comes back to you to accept. Real turns run on the agent CLIs' own accounts and cost what those CLIs charge. Keys, commands and limits are in [`rust/AOS.md`](https://github.com/anon5376/agent-communication-system/blob/rust-port/rust/AOS.md).

The `aos` source lives on the [`rust-port`](https://github.com/anon5376/agent-communication-system/tree/rust-port) branch. This branch, `main`, holds the TypeScript implementation, `qagent`, described below. Both open the same `bus.db`, tokens and signal files, and the bus commands (`task`, `send`, `inbox`, `log`, `trace`, `mcp` and the rest) also work as `aos <command>`. `aos doctor` is aos's own check, not `qagent doctor`.

## Watch a worker die and the work survive

[`examples/worker-death-recovery.sh`](examples/worker-death-recovery.sh) runs the whole loop on a throwaway bus with no agent CLI and no account: a worker claims a scoped task and leaves a note, its process is killed with `SIGKILL`, the bus reports the claim as stalled, the operator requeues it, a second worker claims and submits it, the worker's attempt to accept the work is refused, and the reviewer accepts it.

```bash
QAGENT=aos sh examples/worker-death-recovery.sh     # with the released binary
QAGENT=qagent sh examples/worker-death-recovery.sh  # with the TypeScript build below
```

```text
== worker-2 cannot accept the work; only the named reviewer or the operator can
qagent: only reviewer or the operator may review task 1

== the bus is the trace (abridged)
  10-04 11:35:02 worker-1 task_claimed — task claimed
  10-04 11:35:02 worker-1 note — reproduced the offset bug, starting the fix
  10-04 11:35:06 operator task_released — task released
  10-04 11:35:06 worker-2 task_claimed — task claimed
  10-04 11:35:06 worker-2 task_submitted — task submitted
  10-04 11:35:06 reviewer task_accepted — task accepted
```

Recovery here is one operator command. The TypeScript supervisor can requeue dead claims unattended (`qagent supervise <agent> --auto-requeue-min <n>`); the Rust build cannot yet. What the Rust build does instead (on `rust-port`, not yet in a release): `aos start` also runs `aos watch`, which restarts any agent whose supervisor went down without `aos stop` and gives up after 5 restarts in an hour, and a supervisor renews its agent's claims every minute during a turn, so a long turn keeps its task.

## See what needs you

`qagent doctor`, `qagent trace <task>` and the dashboard sort open work the same way: needs review, failed or blocked, stalled, then active and queued. Each item comes with the reason, the evidence and the next command:

```text
attention 1 task(s) need you
  #2 needs review: Submitted by w; waiting for review by r, who is offline. Read it with qagent task show 2; --revise sends it back.
    evidence: submitted now · summary: "done" · round 1
    next: qagent task review 2 --accept --feedback "..." --as operator
```

This is in the TypeScript build on `main`; `aos` has its own gate strip for the same purpose.

## What it provides

- Verified agent identities with per-agent tokens.
- Direct, multi-recipient, and broadcast messages.
- Threads, acknowledgements, typed messages, and file or URL references.
- Tasks with assignment, dependencies, claims, path leases, progress notes, submission, and review by someone other than the assignee.
- Stalled-claim detection, requeue, and `qagent trace <task>` for a task's full timeline (text, JSON or a self-contained HTML file).
- An attention list in `qagent doctor`, `qagent trace` and the dashboard: what needs you first, with reason, evidence and next command.
- A stdio MCP server with 14 agent tools and one operator-only tool.
- Harness adapters: `claude`, `codex`, `gemini`, `kimi`, `cursor`, `grok`, `opencode`, `hermes`, `devin`, plus any other CLI through the `command` adapter.
- A localhost-only dashboard and an optional supervisor.
- A Claude Code hook that wakes an idle interactive session when mail arrives.
- Import tools for earlier Qagent and Python prototype stores.

An adapter means the command line and output parsing are implemented and unit-tested. It does not mean that provider was run live; see [provider support](docs/provider-support.md).

## qagent: the TypeScript CLI and MCP server

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

The command is `qagent` (`agent-bus` remains as a compatibility alias). Every agent names itself with `QAGENT_AGENT_ID` or `--as <id>`:

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

Let new mail wake an idle interactive Claude Code session, with no supervisor running. This prints a background `Stop` hook to merge into `.claude/settings.json`:

```bash
qagent --as claude hook claude-code --settings
```

See [Wake an idle Claude Code session](docs/FULL-GUIDE.md#wake-an-idle-claude-code-session) for what it shows Claude and its limits.

## What is tested and what is not

- **Platforms.** CI runs on Linux. The `aos` binaries are built for macOS but have only been run by hand on Linux. No Windows build.
- **Providers.** No provider CLI runs in CI; adapters are tested against recorded command lines and output. No live provider run is recorded in this repository. [Provider support](docs/provider-support.md) has the per-provider status.
- **Delegation and claim limits.** Both builds enforce them in the bus core, inside the write transaction: an agent without permission to delegate can only file tasks for itself, a manager can only assign to its allowed agents and depth, and `maxConcurrentTasks` holds even against concurrent claims. The limits come from the project config once a supervisor (or the operator) applies them; a bus that never ran a supervisor has the authority defaults only. TypeScript: on `main`. Rust: on `rust-port`, not yet in a release.
- **Two implementations.** The TypeScript and Rust builds share one schema and interoperate on the same `bus.db` (`scripts/v2-interop-smoke.mjs` checks mail, cursors, claims and wake-ups across both, in CI on `rust-port`). Some features exist in only one: per-task git worktrees and automatic requeue are TypeScript-only (the Rust build refuses work that asks for worktree isolation rather than running it in the shared checkout); `aos` and the pause, resume and budget commands are Rust-only. The TypeScript supervisor enforces the config budgets (`optionalTokenBudget`, `optionalApiCostBudgetUSD`) between turns from reported usage, and refuses a dollar budget for a CLI that reports no usage. See [implementation differences](docs/FULL-GUIDE.md#implementation-differences).
- **Cost.** Budgets in `aos` always count turns and minutes; on `rust-port`, `aos` gives each new agent a default of 200 turns, 720 minutes or $20, whichever comes first; the TypeScript config budgets count tokens and dollars. Dollars and tokens are counted only when a CLI reports them, and every budget is checked between turns, so one turn can overshoot and a dollar budget is not a hard cap. Codex reports tokens, not a price.
- **Speed.** A CLI call takes about 2 ms with the Rust binary and about 77 ms with Node (`inbox --peek`, mean of 20 calls on a Linux container, 2026-10-04). Agent turns dominate either way.
- **Security.** Identity stops agents from impersonating each other by accident. It is not a boundary against a hostile process running as the same OS user, and messages are stored in plaintext. On `rust-port`, `aos` runs agents behind a guard that removes code-hosting, registry and cloud tokens from their environment and makes `git push` fail; that is a guardrail against mistakes, not a sandbox. Details below.

## Terminal UI

`aos` (above) and `acs`, the earlier terminal UI, are built from the `rust-port` branch. Both open the same `bus.db` as `qagent`, so all three can drive one bus. `acs` install steps are in the [`rust-port` README](https://github.com/anon5376/agent-communication-system/blob/rust-port/README.md), and command differences in [Implementation differences](docs/FULL-GUIDE.md#implementation-differences).

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
