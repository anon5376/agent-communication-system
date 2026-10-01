# ACS positioning research: what users complain about, and how a local-first bus wins

> **Status note (2026-10-01).** This is a positioning draft; some of its recommendations have since shipped in the unreleased `main` (see `CHANGELOG.md`, `[Unreleased]`): stalled-task detection and requeue, `qagent trace`, `supervise --roster`, registry manifests (`server.json`, `glama.json`, `smithery.yaml`), and per-task git worktrees. Not shipped: prebuilt `acs` binaries, standards interop (A2A/ACP), and a family-aware review rule. The other items were not re-checked. Quoted issue threads and blog posts were not re-checked. The Hermes Agent comparison is in `docs/competitive-analysis.md`.

## TL;DR

The loudest complaints about CrewAI / AutoGen / LangGraph are not about model quality — they're about **silent failures, debugging opacity, dependency hell, cloud-coupled observability, and duct-taped agent messaging**. ACS already owns the architectural answer to most of these (durable SQLite bus, token identity, claims/leases, signal-file wakeups). The biggest wins are surfacing failure visibility, turning the bus into a free trace store, and shipping the Rust single binary as the front door. Positioning line: **"Agents churn; the bus endures. ACS is not another framework — it's the durable, inspectable layer under whatever CLIs you already run."**

## Pain-point evidence summary

### 1. Silent failures are the norm, not the edge case
- CrewAI production issue: async task LLM failure **silently freezes the flow** — no exception, no log, task stuck "running", downstream agents wait forever. Reporter: "cost me a full day of debugging"; asks for retry/timeout policy. (github.com/crewAIInc/crewAI/issues/6380)
- LangChain's own observability blog admits: "There's no stack trace — because there's no code that failed. What failed was the agent's reasoning." A 200-step trajectory can't be debugged by reading logs.

### 2. Debugging opacity — you're debugging a graph, not a function
- "Why We Removed LangGraph From Our AI Platform" (dev.to): debugging became tracing state transitions across nodes/edges/reducers; "the framework became part of the problem"; most flows weren't actually graphs.
- AutoGen limitations writeups: when a multi-agent conversation goes wrong, "diagnosing the root cause is genuinely difficult" — could be any agent's system message, flow logic, or a tool call.
- "agents-from-scratch" PHILOSOPHY.md: frameworks "hide the mechanisms that make agents work… opaque chain-of-thought makes debugging nearly impossible."

### 3. Dependency weight and version churn
- CrewAI pins `openai>=1.83.0,<1.84.dev0` → unsatisfiable resolver errors against `langchain-openai`, `openlit`, LiteLLM stacks (issue #4300). History of crewai-tools/langchain conflicts; "pip is looking at multiple versions of crewai" loops.
- AutoGen: v0.2→v0.4 breaking API rewrite, then **maintenance mode** — users told to migrate to Microsoft Agent Framework. Tutorials obsolete overnight.

### 4. Cloud coupling / lock-in for observability
- LangSmith: self-hosting and SSO are Enterprise-only, $39/seat, 180-day SaaS retention, bulk export gated; storage moved to proprietary SmithDB. Langfuse's whole pitch is "open data, no lock-in" — the market explicitly buys on this axis.
- CrewAI AMP and managed observability push agents' coordination state into vendor SaaS.

### 5. Agent-to-agent messaging is still duct tape
- People literally coordinate Claude Code/Codex via `tmux send-keys` with `sleep 1` hacks so Enter registers (sderev.com notes; twaldin/tmux-orchestrator — "no daemons, no APIs, no polling — the window list is the registry"; appautomaton/automux uses file-based coordination). This is a *proven, unserved* demand ACS directly replaces.
- EngineersOfAI: early frameworks treat communication as string-passing — no message IDs, no timestamps, no sender info, no error metadata, no audit trail → "debugging takes hours."
- The "inter-agent protocol problem": AutoGen agents can't natively receive tasks from LangGraph; every framework is a silo (dzone). Google's A2A exists precisely because cross-framework agent messaging doesn't work.

### 6. Token cost scales quadratically with shared context
- AutoGen: every message enters shared context all agents process → quadratic token growth; no built-in summarization, context-window blowouts.
- clawRxiv paper: communication overhead in broadcast/P2P multi-agent systems grows quadratically (C(n)≈0.023n²), hitting 50% of tokens at n=7; "communication inflation" — outputs grow 34% when agents are aware of each other.

### 7. Onboarding assumes a Python engineer
- Every incumbent assumes you can wrangle pip/uv, an env, a persistence backend, and an observability SaaS before your first agent message. Nobody ships "download one file, answer three questions, your team is running."

---

## Ranked recommendations (impact ÷ effort)

### 1. Dead-agent detection & automatic requeue — kill the "stuck forever" state
**Why:** The single most damaging complaint in the wild is CrewAI's silent freeze — work looks "running" and never finishes. ACS already has claims, leases, and progress notes; it just doesn't yet *act* on their expiry loudly.
**Effort:** M (mostly surfacing existing state)
**How:** Lease TTL + heartbeat on claims; when a claim's lease expires or no progress note lands within N minutes, mark the task `stalled`, emit a bus event, post to the operator's dashboard banner and `qagent status`. Add `qagent requeue <task>` and an opt-in supervisor flag `--auto-requeue` that unclaims stalled tasks so another agent (or a restarted one) picks them up. Ship a demo: `kill -9` a worker mid-task, watch the task stall and requeue. This is the direct antithesis of issue #6380.

### 2. "The bus is the trace" — free, replayable observability
**Why:** Debugging opacity is complaint #2, and the only mainstream fix is LangSmith ($39/seat, Enterprise-gated self-host, 180-day retention). ACS's bus.db already records every message with sender, recipient, thread, timestamp, ack — i.e., the causal trace LangSmith sells, stored in a file the operator owns.
**Effort:** M
**How:** `qagent trace <task-id|thread>` renders the causal chain: who messaged whom, task claimed → progress → submitted → reviewed, with timestamps and payloads. `qagent export` writes the same as JSON or a self-contained HTML timeline. Dashboard gets a "task replay" scrubber. Because it's just SQLite, `sqlite3 bus.db 'select ...'` is itself a feature — document the tables as the public observability API. Zero new infrastructure, zero telemetry leaving the machine.

### 3. Ship the Rust TUI as the single-binary front door
**Why:** Dependency hell (CrewAI resolver failures, AutoGen churn) means the install *is* the churn filter. ACS's ~4MB `acs` binary with the team-setup wizard + CLI autodetect is the anti-CrewAI: no Node, no pip, no resolver. Right now the README leads with `npm ci && npm run build && npm link` — the hardest path, shown first.
**Effort:** M (packaging/release plumbing; product already exists)
**How:** Publish prebuilt `acs` binaries (GitHub Releases, `cargo install`, maybe a curl|sh). Wizard flow: detect installed agent CLIs → name agents → pick roles/spawn lists → write roster → first task in under 5 minutes. Make the binary the headline of README; demote the TS build-from-source to a contributor path. Non-technical onboarding is a category no incumbent even attempts.

### 4. Multi-agent supervisor + "stop driving agents with tmux send-keys" positioning
**Why:** tmux-orchestrator, automux, and "just use tmux" blog posts prove developers *want* multi-CLI coordination and are settling for send-keys with `sleep 1` so Enter lands. ACS's supervisor + signal-file wakeup is the same idea done correctly — but it currently "waits for one agent."
**Effort:** S–M
**How:** `qagent supervise --roster <file>` (or `--all`) runs N agent CLIs under one supervisor, each woken by its signal file when mail/tasks arrive — replacing the tmux window-list registry with the bus. Then write the comparison docs/landing section: "tmux send-keys → durable wakeup. Window list → verified roster. Pane scraping → inbox/ack/review." Target r/LocalLLaMA and Claude-Code power users explicitly; this is the highest-conversion audience.

### 5. Headline the review gates — human-in-the-loop without checkpoint YAML
**Why:** LangGraph sells HITL but it requires configuring a persistence backend and checkpointing; CrewAI bolts it on. ACS already has review built into the task lifecycle (the assignee cannot accept its own work; the operator can override), so most users don't know because it's buried in the guide.
**Effort:** S
**How:** Promote review gates to a top-level README feature with a 30-second demo (`qagent task submit` → `qagent review` → approve/reject with notes routed back to the worker). Dashboard: one-click approve/request-changes on submitted work, diff/task brief side by side. Position: "review is a first-class state, not a node you wire in."

### 6. Crash-resume as an advertised guarantee
**Why:** LangGraph's checkpointing is its best-loved feature and the reason reluctant converts stay — but it needs deliberate config. ACS gets equivalent durability *free* because all coordination state is committed to bus.db before agents are woken.
**Effort:** S
**How:** Write it as a tested guarantee: integration test + doc that SIGKILLs the supervisor and a worker mid-task, restarts, and shows messages/claims/progress intact. "Durability isn't a plugin — it's the storage model." Cheap credibility builder against every in-memory framework.

### 7. Bounded-context messaging — sell the token-sparing architecture
**Why:** Shared-context frameworks' quadratic token growth (50% overhead at n=7 agents, "communication inflation") is a real cost complaint. ACS's direct/threaded delivery means an agent only ever sees its own mail — a structural cost advantage nobody has marketed.
**Effort:** S–M
**How:** Document the model: no broadcast-all by default; an agent reads only its threads. Add `qagent catch-up [--since]` that synthesizes unread messages + open tasks into a compact briefing so a woken agent gets minimum-viable context instead of a firehose. Publish a small benchmark/table: tokens-to-deliver-context vs. shared-context fan-in at 4/8/16 agents.

### 8. Neutral interop: minimal task-submit endpoint (A2A-lite)
**Why:** The inter-agent protocol problem — frameworks can't talk to each other — is why Google built A2A. ACS is *already* the heterogeneity play (adapters for 10+ CLIs); a thin inbound bridge makes it the neutral meeting point, not another silo.
**Effort:** L (full A2A) — M for the pragmatic version
**How:** Start pragmatic: `qagent serve` exposes a localhost HTTP endpoint `POST /tasks` (+ `GET /tasks/:id`) mapping to the existing task queue — any framework, script, or CI job can drop work on the bus without learning MCP. Optional later: an A2A-conformant agent card + task endpoints. Positioning: "the SMTP of agent CLIs — one address every agent can reach." Rank last because it's the biggest lift and matters most only after adoption exists.

---

## Positioning notes (free, do regardless)

- **Lead with what frameworks can't claim:** "Every message is a row in a file you own. Kill the power mid-task; nothing is lost. No daemon, no cloud, no seat fee."
- **Use competitors' own bug trackers as the pitch:** silent freezes (CrewAI #6380), resolver failures (CrewAI #4300), abandoned versions (AutoGen) are public receipts — a comparison page citing them is more credible than adjectives.
- **Name the enemy correctly:** the alternative to ACS isn't CrewAI — it's tmux send-keys, pasted transcripts, and "which agent said that?"
