# Agent Communication System — native macOS and terminal apps

## ACS for macOS (SwiftUI preview)

`swiftui` adds **ACS.app**, a native Mac app for tasks, reviews, agents, and
messages on the existing ACS bus. It complements the terminals below; it is
not an embedded terminal or a replacement coordination backend.

Start with **Try the Sample** (simulated, no model calls), or open a project
and explicitly start coding CLIs you already have installed. Installation,
first-run guidance, safety limits, and the architecture-specific DMG build
are in [macos/README.md](macos/README.md).

Requires macOS 14+. Default builds are ad-hoc signed, **not notarized**;
Gatekeeper may block downloaded copies. No public signed release is implied.

## ACS for Windows (preview)

`windows/` adds **ACS for Windows**, the same app on Windows: a Tauri v2
shell (WebView2) over the same `acs-desktop` JSON bridge as the macOS app —
welcome, task board, review decisions, agents, and messages, against the same
bus. The terminal tools remain the primary interface; the app sits alongside
them, never instead of them.

Build the installer from the repository root:

```powershell
powershell -ExecutionPolicy Bypass -File windows\scripts\build-installer.ps1
```

This produces `ACS-<version>-x64-setup.exe` (NSIS, per-user install, Start
menu shortcut) plus a `.sha256` under
`windows/src-tauri/target/release/bundle/nsis/`. When `rust/` compiles on
Windows the script builds `acs-desktop.exe`, `qagent.exe`, and `aos.exe` from
source and bundles them; until the Windows core port lands it stages
`fake-acs-desktop` — a protocol-identical double that keeps a JSON bus instead
of SQLite — and the installer still works end to end (agent starts then
report "no bundled qagent", matching the real helper's own error path).

The installer is **unsigned**: SmartScreen will warn on downloaded copies.
Details, layout, and protocol notes are in
[windows/README.md](windows/README.md). Requires Windows 10+ and the
WebView2 runtime (present on Windows 11 and most Windows 10 installs).

## Rust terminal implementation

> Based on **`rust-port`** — a from-scratch Rust implementation of ACS plus
> `acs`, an ultra-lightweight terminal control app (~4 MB single binary, ~2 ms
> CLI calls). It shares the same `bus.db`, tokens, and signal files as the
> TypeScript version, so the two can drive the same bus interchangeably. Best
> choice for machines with ~8 GB RAM or anyone who wants a daemon-free,
> zero-dependency install. The `main` branch has the TypeScript implementation.

## Install the TUI (`acs`)

Building from source requires Git, the **Rust toolchain** (`cargo` and `rustc`),
and a C compiler/linker. Install Rust with [rustup](https://rustup.rs/), then
open a new terminal and check that both commands are available:

```sh
cargo --version
rustc --version
```

On macOS, install Apple's Command Line Tools if you do not already have them,
and finish the installer before continuing:

```sh
xcode-select --install
```

On Linux, install your distribution's C build tools (for example,
`build-essential` on Debian/Ubuntu).

Run these commands from the directory where you want to clone the repository.
If you already have a checkout, enter it and skip the first two commands:

```bash
git clone https://github.com/anon5376/agent-communication-system.git
cd agent-communication-system
git checkout rust-port
./rust/install.sh
```

The script builds `acs` and installs it in `/usr/local/bin`, using `sudo` if
needed. If that installation fails, it falls back to `~/.local/bin`. When using
the fallback directory, add it to your shell's PATH (and your shell startup
file to keep the change across terminals):

```sh
export PATH="$HOME/.local/bin:$PATH"
```

After the build and installation succeed, start the dashboard:

```sh
acs
```

You can run `acs` from anywhere. First launch creates your bus automatically and
offers a **team-setup wizard** — space toggles preset agents (planner,
orchestrator/lead, worker-hard, worker-easy), Enter creates them with starter
charters in their inboxes.

If installation reports `cargo: command not found`, install Rust and check
`cargo --version` before rerunning `./rust/install.sh`. If `acs` is still not
found after a successful installation, check that the reported install directory
is on your PATH. The command blocks above contain only commands; keep explanatory
comments and Markdown link formatting out of terminal pastes.

**Keys:** `tab`/`←→` or click — switch panes · `↑↓`/`jk`/wheel — select ·
`m` — send a message (pick the recipient from a list) · `t` — new task ·
`x` — cancel selected task · `a` — add an agent · `enter` — open the full
message/task · `?` — help · `q` — quit. All panes refresh live.

Need a specific database? `acs --db /path/to/bus.db`. The Rust `qagent` CLI is
built alongside (`cargo build --release --manifest-path rust/Cargo.toml`) and
takes the same commands as the TypeScript one — see
[rust/README.md](rust/README.md) for what's ported.

## The AOS terminal (`aos`)

`aos` is a second terminal console on the same bus, drawn to the AOS
Acceleration Chamber design: agents as one causal spine, readouts for goal,
progress, cost, evidence and stuck work, and a gate strip for reviews and
stalled claims. Install it with `./rust/install-aos.sh`, then try `aos demo`
for a sample bus or `aos` for yours. Keys, gates and what each readout reads
are in [rust/AOS.md](rust/AOS.md).

---

# Agent Communication System

Agent Communication System gives local coding agents durable mail, task handoffs, review gates, and optional automatic wake-ups. The command is `qagent`; `agent-bus` remains as a compatibility alias.

Coordination lives in one SQLite file. The CLI and MCP server open it directly, so ordinary messaging and task work need no broker or background daemon. An optional supervisor can wake agent CLIs, and an optional local dashboard shows activity.

## What it provides

- Verified agent identities with per-agent tokens.
- Direct, multi-recipient, and broadcast messages.
- Threads, acknowledgements, typed messages, and file or URL references.
- Tasks with assignment, dependencies, claims, path leases, progress notes, submission, and independent review.
- A stdio MCP server with 14 agent tools and one operator-only tool.
- Harness adapters for Claude Code, Codex, Gemini, Kimi, OpenCode, and OpenAI-compatible CLIs.
- A localhost-only dashboard and an optional supervisor.
- Import tools for earlier Qagent and Python prototype stores.

## Quick start

Requires Node.js 22.13 or newer.

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
