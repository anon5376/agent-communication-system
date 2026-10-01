# ACS Launch Copy

Draft launch assets for **ACS — Agent Communication System** (github.com/anon5376/agent-communication-system). Tone follows the repo's README: terse, factual, honest about limits.

---

## 1. Show HN

**Title** (76 chars):

> Show HN: ACS – a local-first control plane and message bus for coding agents

**Body** (192 words):

> I run several coding agents at once — Claude Code, Codex, and a couple of local-model CLIs — and found myself copy-pasting briefs between terminals and merging their edits by hand. ACS is what I wanted instead: a local control plane and message bus that lives in one SQLite file. No daemon, no cloud, no broker — the CLI and its MCP stdio server open the file directly (WAL + busy_timeout).
>
> Agents get token-file identities, send each other threaded messages, and claim work from a shared task queue with roles and path leases, so two agents can't grab the same files. Humans get a localhost dashboard (single-use sign-in tickets; the operator token never reaches the browser) and `acs`, a ~4 MB Rust TUI with a first-run wizard that sets up planner/lead/worker presets.
>
> There's a TypeScript reference implementation plus a byte-compatible Rust port — same schema, same token files, interop verified in both directions — which took CLI calls from ~78 ms to ~2 ms.
>
> Honest limits: it's per-machine, identity protects against accidents rather than hostile processes, and messages sit in plaintext on disk. MIT licensed. Curious how others coordinate agent fleets locally.

---

## 2. r/LocalLLaMA-style Reddit post

**Title**: Tired of copy-pasting briefs between agent terminals? I built a tiny local message bus for them

**Body**:

> So I've been running 3-4 coding agents on the same projects (Claude Code, Codex, plus a local model through an OpenAI-compatible harness) and realized I'd become the coordination layer. Copy task brief into terminal A, paste the result into terminal B, manually keep them from editing the same files. Felt dumb.
>
> I ended up building ACS — the whole thing is ONE SQLite file on your box. No daemon, no cloud, no broker process. Agents get their own identities (token files), real inboxes with threads and acks, and a shared task queue where they claim work with roles + leases so two agents can't grab the same task or stomp the same paths. Stale claims expire on their own.
>
> Anything plugs in: there's an MCP stdio server (14 tools) for MCP-aware CLIs, and harness adapters for claude/codex/gemini/kimi/opencode/etc, plus a generic OpenAI-compatible caller. A supervisor process can wake an agent's CLI when work lands in its inbox.
>
> For the human there's a localhost dashboard and `acs`, a ~4MB Rust TUI. First run walks you through a team-setup wizard with planner/lead/worker presets — charters land in the agents' inboxes so they know their jobs.
>
> It's not an agent framework — it doesn't write prompts or pick models. It's just the mailbox and task board underneath whatever CLIs you already run. TypeScript reference + a byte-compatible Rust port (interops with the same bus.db both directions); Rust CLI calls measure ~2ms vs ~78ms for the Node version.
>
> Fully local-first: delete bus.db and it's gone. Plaintext storage, same-machine only — it's coordination, not a security boundary. MIT. Would love a roast of the design.

---

## 3. Taglines

**Primary (README one-liner):**

> One SQLite file to coordinate every coding agent on your machine.

**Alternatives:**

1. Durable mail, task claims, and review gates for local agent teams — no daemon, no cloud.
2. The local post office and task board for heterogeneous coding agents.
3. Identities, inboxes, and a shared task queue for agent CLIs — all in one file.

---

## 4. X/Twitter post (270 chars)

> I built ACS: a local-first control plane + message bus for coding agents. One SQLite file — no daemon, no cloud. Agents get identities, mail, and a task queue with roles, leases, and review gates. Rust port: ~2ms CLI calls. github.com/anon5376/agent-communication-system

---

## 5. "Why not just use X"

- **vs CrewAI / agent frameworks** — different layer. CrewAI defines agents, prompts, and workflows inside its own runtime. ACS is the infrastructure underneath: mailboxes, a task queue, and identities for agents that already exist as standalone CLIs. You could run a crew *on* ACS, or mix Claude Code, Codex, and a local model on the same bus — each keeps its own tools, models, and subscriptions.
- **vs a message queue (Redis / NATS / RabbitMQ)** — a generic broker moves bytes; it doesn't know what an agent task is. ACS gives agent-shaped primitives out of the box — token identities, atomic task claims, path leases so agents don't collide on files, lease expiry, review gates — with zero ops: no server to run, the SQLite file *is* the broker.
- **vs a shared doc / markdown file** — a doc can't push. Agents have to poll it, two can claim the same item, nothing expires a stale claim, and nobody wakes up when work lands. ACS gives durable addressed delivery, atomic claims, lease expiry, and signal-file wake-ups — plus an event log of who did what.

---

*Fact sources: repo README + PRODUCT.md on main, rust/README.md and rust/src/app.rs on the rust-port branch (TUI, wizard presets, adapter list, interop), prompt-provided perf numbers (~78ms → ~2ms).*
