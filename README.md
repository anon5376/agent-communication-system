# ACS — Agent Communication System

**Run different coding agents together without being their message bus.**

ACS keeps tasks, messages, ownership and reviews in one local SQLite database.
Your existing coding CLIs do the work; ACS coordinates it and shows what needs you.
Use a terminal, the native macOS app, or the Windows desktop app over the same bus.

![AOS terminal demo with simulated agents](docs/assets/aos-demo.gif)

## Try it in 20 seconds

After installing, run `aos demo`, or open ACS and choose **Try the Sample**.
The sample is simulated: no credentials, agent processes, model calls or charges.
In the terminal, press `?` for keys and `q` to quit. For a noninteractive preview:

```sh
aos demo --print 100x30
```

## Install

Use [tagged releases](https://github.com/anon5376/agent-communication-system/releases).
The unified `v*` release workflow produces the assets below. Older `aos-v*` releases
contain only the AOS command, not this complete product. Until a unified release is
published, use the explicit source build below; the installer never silently falls
back to a branch or a local build.

### Linux and macOS commands

```sh
curl -fsSL https://raw.githubusercontent.com/anon5376/agent-communication-system/main/install.sh -o acs-install.sh
sh acs-install.sh
export PATH="$HOME/.local/bin:$PATH"
```

Installs **acs, aos, qagent and acs-desktop** into `~/.local/bin` from one tagged
release, prints the selected tag and URL, and requires a matching SHA-256 checksum.
Linux: x86_64/ARM64. macOS: Apple Silicon/Intel. No Rust or Node needed.
Inspect the downloaded script before running it. To pin a release or install path:

```sh
ACS_VERSION=v1.0.0 ACS_INSTALL_DIR="$HOME/.local/bin" sh acs-install.sh
```

Use an actually published tag; this example is not a claim that v1.0.0 exists yet.
The macOS CLI archive and native app are separate installs.

### macOS app

Download **ACS-arm64.dmg** from the same release, open it, and drag ACS to
Applications. Requires macOS 14+ and Apple Silicon; the DMG is not universal.

**Ad-hoc signed, not notarized.** After a blocked first launch, open System Settings
→ Privacy & Security → **Open Anyway**, if offered, and confirm the app you downloaded.
Managed-device policy may prohibit this. Do not disable Gatekeeper globally.
[Build and installation details](macos/README.md).

### Windows app

Download **ACS_*_x64-setup.exe** from the same release and run it. Requires Windows
10+ x64 and WebView2; the installer bundles the real Rust helper, not a preview stub.

**Unsigned.** If SmartScreen offers **More info → Run anyway**, use it only after
checking the source and checksum. Some managed devices disallow this override.
Do not disable Defender or system security. [Details](windows/README.md).

### Verify a download

Download `SHA256SUMS` from the release and compare the matching file entry:

```sh
sha256sum ACS-arm64.dmg       # Linux
shasum -a 256 ACS-arm64.dmg  # macOS
```

Windows PowerShell: `Get-FileHash .\ACS_1.0.0_x64-setup.exe -Algorithm SHA256`
(substitute the downloaded filename). Checksums detect corruption; they do not
replace publisher signing. Releases also include an SPDX source-dependency SBOM.

## Your first real task

Real agent turns use the CLI's own account and can cost money. Installing ACS,
detecting tools and queuing a task do not themselves run a model.

1. Install and sign in to the coding CLIs you want to use, or configure local Ollama.
2. In a trusted project, run:

   ```sh
   aos setup
   aos doctor
   ```

3. Review the generated crew, provider permissions and prompt files. CLI detection
   is not proof of authentication. Use two distinct identities for worker/reviewer;
   two identities do not guarantee two independent models.
4. Queue a small, scoped goal:

   ```sh
   aos "Add a regression test for the empty input case"
   ```

   This queues work; it does not prove any agent has claimed it. If no eligible crew
   exists, configure one first. In the app: **Orchestration → Goals → Queue goal**.
5. Explicitly start the configured agents with `aos start`, or use **Orchestration
   → Agents** and approve the project. Some integrations need explicit unattended
   execution approval. Read that prompt before proceeding.
6. Review the submission against its acceptance criteria before accepting it.
   Use the AOS review gate, **Communication → Needs review**, or `qagent task show N`.
   A successful process exit and plain CLI output are not verification of the work.

## Communication and orchestration

| Surface | What it does |
| --- | --- |
| **Communication / ACS** | Messages, tasks, review inbox and current agent activity |
| **Orchestration / AOS** | Goals, reusable prompt presets, crew configuration and explicit start/stop |
| `acs` | Compact terminal control panel |
| `aos` | Terminal mission control, setup, goals and review gates |
| `qagent` | Low-level CLI, bus operations, supervision and MCP |
| `acs-desktop` | Versioned JSON bridge used by both desktop apps |

Desktop mode shortcuts: Cmd+1/2 on macOS, Ctrl+1/2 on Windows. Agent configuration
is under **Orchestration → Agents**, not hidden in the message dashboard.

## Ownership, review and recovery

- Claims are transactional; two workers cannot own the same task concurrently.
- Delegation/depth/concurrent-task policies are enforced by both implementations
  once configured in the bus. Restrictive supervisor setup fails closed.
- Workers cannot accept their own submissions. The designated reviewer or operator
  decides; acceptance remains a human/agent review decision, not a proof of correctness.
- A supervised failed turn is recorded and its still-held work is failed back through
  the retry/review policy. Repeated failures pause the agent for operator attention.
- An arbitrary external CLI claim has no worker PID to monitor. Recovery uses lease
  expiry/stall inspection and an explicit `qagent task requeue N --reason "..."`.
  Stop the original worker before reassignment to avoid duplicate execution.
- Requeue preserves the task's brief, acceptance criteria, file scope and reviewer.

[Worker-death recovery example](examples/worker-death-recovery.sh) uses fake workers
and a throwaway bus. [Security model](docs/security.md) explains the boundaries.

## Rust product and TypeScript compatibility

Rust is the primary implementation, including both terminal and desktop apps.
The TypeScript `qagent` remains a compatible npm package, with the same bus schema
and shared-bus policy checks. Source builds of the compatibility package need Node
22.13+ (`npm ci && npm run build`; run `node dist/qagent.js`). Do not install both
implementations under the same `qagent` path without choosing which one should run.

- Harness adapters: `claude`, `codex`, `gemini`, `kimi`, `cursor`, `grok`, `opencode`, `hermes`, `devin`, plus generic CLIs through `command`.
- **TypeScript-specific features:** per-task git worktrees, automatic stale-claim
  requeue, the browser attention view and
  Claude wake hook. These are not advertised as Rust features; Rust refuses requested
  worktree isolation rather than silently using a shared checkout.
- **Both implementations:** cooperative path leases, versioned schema migration,
  pause/resume, between-turn budgets, token-preserving policy enforcement and explicit
  task requeue. Rust's migration list is compiled in; schema files alone do not add a migration to it.
- **Rust-specific surfaces:** ACS/AOS terminal UI and the desktop bridge.

See [implementation differences](docs/FULL-GUIDE.md#implementation-differences) and
[provider support](docs/provider-support.md). Adapter fixtures are not live-provider
certification. No paid provider is called by CI.

## Limits worth reading

File scopes and bus path leases are cooperative coordination, **not OS sandboxing**.
A coding CLI can still modify files its OS user can access. The bus is not a security
boundary against another hostile process running as the same user.

Budgets are checked between turns. Turns/minutes are locally measured; dollars and
tokens depend on what the provider reports. One turn can overshoot, and a dollar
budget is not a hard spending cap. Credentials remaining on disk do not prove sign-in.

Messages and task data are local plaintext. Protect the bus directory and backups.
Report vulnerabilities through [private security advisories](https://github.com/anon5376/agent-communication-system/security/advisories/new).

## Why not just tmux or worktrees?

Keep them. tmux gives processes a place to run; git worktrees separate checkouts.
ACS adds durable task ownership, routed messages, review gates and recovery history
across different agent CLIs. It complements those tools rather than replacing your
editor, terminal or source-control workflow.

## Build, test and uninstall

Explicit source build (Git, Rust and a C compiler required):

```sh
git clone https://github.com/anon5376/agent-communication-system.git
cd agent-communication-system
cargo build --release --locked --manifest-path rust/Cargo.toml --bin acs --bin aos --bin qagent --bin acs-desktop
./rust/target/release/aos demo
```

The installer also supports `ACS_FROM_SOURCE=1 ACS_VERSION=<tag>`; it requires an
explicit existing tag and never selects a branch for you.

To uninstall CLI commands, stop agents first and remove the four binaries from your
chosen install directory. Remove ACS from Applications on macOS or use Windows
Installed apps. Uninstalling does not intentionally delete bus history or project
files; back those up and remove them separately only if wanted.

[Contributing and tests](CONTRIBUTING.md) · [Full guide](docs/FULL-GUIDE.md) ·
[Agent protocol](protocol/PROTOCOL.md) · [Desktop protocol](protocol/desktop-v1.md) ·
[Architecture](docs/architecture.md) · [Issues](https://github.com/anon5376/agent-communication-system/issues)

[MIT](LICENSE)
