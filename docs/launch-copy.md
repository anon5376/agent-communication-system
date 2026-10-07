# Launch copy

Drafts for when the owner decides to post. Nothing here has been posted. Every claim in these drafts is listed in the [claims table](#claims-and-evidence) with its evidence; if a claim stops being true, fix the table and the drafts together. Posting steps and gates are in the [owner checklist](launch-checklist.md).

Line to lead with everywhere: **Run different coding agents together without being their message bus.**

---

## Show HN

**Title:**

> Show HN: ACS – run Claude Code, Codex and others together on one task board

**Body:**

> I kept running two or three coding agents on the same repo and ended up as their message bus: pasting briefs between terminals, remembering who was editing what, and checking every result myself.
>
> ACS moves that into one SQLite file on your machine. Agents claim tasks from it (one owner at a time), leave notes and submit work for review. A worker cannot accept its own submission; a designated reviewer or the operator does. Cooperative path leases help agents avoid overlapping edits, but they are not a filesystem sandbox. Supervised failures are recorded; external claims need lease/stall inspection and explicit recovery. Stop the original worker before requeueing. `qagent trace` shows the history. No always-on server is required.
>
> Use terminal mission control (`aos`) or the native Mac and Windows apps. Communication shows messages, tasks and reviews; Orchestration configures goals, prompts and agents. `aos demo` and the desktop sample need no account and make no model calls. Queueing a desktop goal does not start an agent.
>
> Limits, plainly: adapter/control-plane tests do not certify every live provider. Unattended execution needs explicit approval in a trusted project. Same machine only; identity stops accidents, not a hostile process running as you, and messages are plaintext on disk. Desktop downloads are unsigned/not notarized; the Mac app requires Apple Silicon and macOS 14+. See the release notes for the exact tested versions and limitations.
>
> MIT. I would like to hear how you coordinate several agents today, and where this breaks for you.

---

## Reddit (r/ClaudeAI, r/ChatGPTCoding, r/LocalLLaMA)

**Title:** I stopped being the message bus between my coding agents

**Body:**

> I run Claude Code and Codex on the same projects and was spending more time relaying between them than reviewing their work. So I built ACS: a local task board in one SQLite file that the agents use directly.
>
> - A task has one owner at a time. Inspect failures/stalled claims, stop the original worker, then explicitly requeue when needed; the history stays with the task.
> - The agent that did the work cannot accept it. A reviewer agent (from a different CLI if you have two) or you does.
> - `qagent trace <task>` shows who claimed, noted, submitted and reviewed, and when.
>
> Try it without any account: `aos demo` after the one-line install, or `sh examples/worker-death-recovery.sh` from the repo.
>
> What it is not: a sandbox, a cloud service, or a replacement coding model. It coordinates your existing CLIs, with editable goal and role prompts. Rust is the primary product; the TypeScript `qagent` package remains compatible. Use the terminal or Mac/Windows apps; desktop artifacts are unsigned. Provider adapters are not blanket live-provider certification.
>
> Repo: github.com/anon5376/agent-communication-system. A roast of the design is welcome.

For r/LocalLLaMA, link the exact local-model release evidence. Local inference is reached through a coding harness such as OpenCode, not through a fake Ollama agent. Do not call the workflow validated unless the worker actually claims/submits and an independent reviewer checks/accepts the result.

---

## X / Bluesky (under 280 characters)

> Run different coding agents together without being their message bus. ACS: one SQLite file where Claude Code, Codex and Cursor agents claim tasks, recover when a worker dies, and can't approve their own work. Local, MIT. github.com/anon5376/agent-communication-system

---

## Repository description and topics

Description (for the GitHub "About" box):

> Run different coding agents together without being their message bus: task claims, crash recovery and independent review for Claude Code, Codex and other CLIs, in one local SQLite file.

Topics: `mcp` `mcp-server` `model-context-protocol` `ai-agents` `multi-agent` `agent-orchestration` `coding-agents` `claude-code` `codex` `sqlite` `local-first` `cli` `tui` `typescript` `rust` `developer-tools`

---

## Short answers to "why not X"

- **An agent framework (CrewAI, AutoGen, LangGraph).** Those define agents inside their runtime. ACS sits under CLIs you already run, each with its own tools, models and subscription.
- **A message queue (Redis, NATS).** A queue moves messages. ACS knows what a task is: one owner, leases on paths, a stalled state, a review that the author cannot pass. And there is no server to run.
- **tmux send-keys and a shared notes file.** A notes file cannot refuse a second claim, notice a dead worker, or stop an agent approving its own work.

---

## Claims and evidence

| Claim | Evidence | Checked |
|---|---|---|
| Worker cannot accept its own work | `src/core/bus.ts` review gate: only the named reviewer (or creator) or the operator may review, and never the assignee. Checked by hand with TS `qagent` and `aos` 0.1.0 with the worker as named reviewer and as creator; the example script shows the reviewer-only rule | 2026-10-04 |
| Dead worker's claim is reported and recoverable | `task stalled`, `task requeue` in both builds; the example above kills the worker with SIGKILL | 2026-10-04 |
| Unattended requeue | TS only: `qagent supervise --auto-requeue-min`. Not in the Rust build or `aos` | 2026-10-04 |
| Overlapping path claims refused | `leaseConflicts` in `src/core/bus.ts`; cooperative, not filesystem-enforced | 2026-10-04 |
| No daemon | CLI and MCP server open `bus.db` directly; the supervisor and dashboard are optional processes you start | 2026-10-04 |
| Linux and macOS binaries | Release `aos-v0.1.0` has 4 builds; installer run in a clean `$HOME` on Linux x86_64 only | 2026-10-04 |
| `aos demo` needs no account | Seeds a sample bus in `$TMPDIR/aos-demo`; starts no agent CLI | 2026-10-04 (code read) |
| Claude Code, Codex, Cursor can join a crew | `rust/AOS.md`; true in `aos-v0.1.0` | 2026-10-04 |
| Other CLIs join through `aos connect --auto-approve` | `rust/AOS.md` on `rust-port` (#34, #35); flags checked against each CLI's `--help`; not in `aos-v0.1.0`, so post only after the next release | 2026-10-05 |
| Other adapters exist | `ADAPTERS` in `src/adapters.ts`; unit tests only, no live run recorded | 2026-10-04 |
| TS and Rust share one bus | `scripts/v2-interop-smoke.mjs` in `rust-port` CI | 2026-10-04 |
| CLI call ~2 ms (Rust) vs ~77 ms (Node) | `inbox --peek`, mean of 20 calls, Linux container | 2026-10-04 |
| Doctor, trace and dashboard list what needs the operator with reason, evidence, next command | `src/attention.ts`, merged in #31; `qagent doctor` output checked on a sample bus | 2026-10-04 |
| `aos watch` restarts crashed agents; claims renewed during long turns | `rust/AOS.md` "Long unattended runs", #28 merged into `rust-port`; not in `aos-v0.1.0` | 2026-10-05 |
| Delegation and claim limits enforced in the core, both builds | TS `tests/reliability.test.ts` (#29, on `main`); Rust `rust/tests/reliability_tests.rs` (#32, on `rust-port`) | 2026-10-05 |
| Release binary size | `aos` x86_64 Linux: 5,242,048 bytes (earlier drafts said 3 or 4 MB, which was the older `acs` binary) | 2026-10-04 |

The table above is historical evidence for the old AOS-only release, not a certification of a new unified release. Native macOS and Windows candidate builds now exist; use the release PR's revision-specific tests, installer hashes and limitations rather than carrying forward the old "no Windows"/"Mac untested" statements. Historical timing and binary-size measurements are not current performance promises.

Do not claim "works with every agent", a hard dollar cap (budgets are checked between turns and only when a CLI reports cost), a passing local-model workflow without its acceptance evidence, clean-machine installation from a local reinstall, star/user counts without evidence, or "first"/"only" anything. Do not publish these drafts before the owner checklist passes.
