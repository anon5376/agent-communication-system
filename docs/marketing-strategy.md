# ACS Marketing Strategy

The complete go-to-market plan for `qagent` / the `acs` TUI. Builds on
`competitive-analysis.md` (where the market gap is), `promotion-playbook.md`
(channel mechanics), `standout-features.md` (what to lead with), and
`launch-copy.md` (ready-to-post drafts). Goal: **1,000+ GitHub stars in 30 days**,
with durable discovery after the launch spike decays.

---

## 1. Positioning

**Category:** ACS is not an agent framework and not a parallel-run UI. It is the
**coordination substrate** underneath standalone agent CLIs — the mailbox, task
board, and shared state that lets heterogeneous agents act like a team.

**One-liner (everywhere):**

> One SQLite file to coordinate every coding agent on your machine — no daemon,
> no cloud.

**The empty cell we own** (from `competitive-analysis.md`): frameworks
(CrewAI/AutoGen/LangGraph) orchestrate LLM calls *inside one program*; control
surfaces (Vibe Kanban, claude-squad, OpenHands) manage agent *processes* but make
the human the router. A vendor-neutral, daemonless message-and-task bus between
real CLI agents is unoccupied — the only neighbors (agent-mail, Beads) don't
spawn or supervise agents.

**Three proof pillars — every asset leads with one of these:**

1. **Zero infrastructure.** The SQLite file *is* the broker. CLI and stdio MCP
   server open `bus.db` directly; delete the file and it's gone. Competitors all
   need a server, runtime, or vendor cloud.
2. **Real agents, real lifecycle.** Token identities, atomic claims, path leases,
   review gates, claim expiry — agent-shaped primitives, not a byte pipe. The
   supervisor *launches* heterogeneous CLIs (Claude, Codex, Gemini, Kimi,
   OpenCode, any OpenAI-compatible), it doesn't just watch running ones.
3. **Absurdly light.** ~4MB dependency-free Rust binary + `acs` TUI with a
   first-run team wizard — usable by non-technical operators; byte-compatible
   with the TypeScript implementation. ~2ms CLI calls vs ~78ms on Node.

**Wedge narrative (the emotional hook):** people are literally driving parallel
agents with tmux send-keys and shared docs. A doc can't push, expire a claim, or
wake an agent. ACS is what that duct tape wants to be.

## 2. Audience segments & where they live

| Segment | Pain we speak to | Primary channels | Message angle |
|---|---|---|---|
| Power users running 2+ coding agents | Coordination by hand: tmux, shared docs, copy-paste between terminals | r/claudecode, Show HN, X | "Stop being the message bus. Let the agents talk." |
| OSS/agent builders | Need durable agent-to-agent plumbing without standing up infra | Show HN, MCP registries, awesome lists, dev.to | "Coordination substrate, stdio MCP, agent-shaped primitives." |
| Local-first / self-host crowd | Everything wants a cloud account | r/LocalLLaMA, Lobsters, HN comments | "No daemon, no cloud, one file, delete it and it's gone." |
| Non-technical operators | Agent tools assume you script everything | `acs` TUI demo, Terminal Trove, README GIF | "Install, answer four questions, your team exists." |

## 3. Funnel

Visitor → star → install → operator. Stars come from reach; installs come from
friction removal. Every touchpoint must do both:

- **Reach:** Show HN (one shot, highest ceiling) → Reddit (3 subs, staggered) →
  X clip → evergreen (registries, awesome lists, dev.to SEO).
- **Convert:** README answers "what / why not X / how do I run it" in the first
  screen — tagline, GIF, `npx` install. Every second of install friction loses a
  fraction of the audience.
- **Retain/expand:** agents users already run get an inbox; `acs` wizard gives
  them a preset team. Success = they stop scripting coordination.

## 4. Prerequisites (launch blockers, owner: repo owner unless noted)

- [x] LICENSE file (in PR #3)
- [x] README hook: tagline + GIF + "why not" + `npx` lead (PRs #4, #6)
- [ ] **`npm publish`** — `npx qagent` is the biggest single conversion lever
      and gates the official MCP Registry + Smithery
- [ ] **GitHub topics** — `mcp-server, ai-agents, multi-agent, local-first,
      sqlite, claude-code, developer-tools` (repo settings → About)
- [ ] **Merge PRs #3→#4→#5/#6→#7→#8→#9** — the demo's claims (requeue, trace,
      roster) must be true on main when traffic lands
- [ ] Registry submission (PR #5 has manifests + `docs/submission-pack.md` with
      copy-paste submission text for all 11 targets)
- [ ] **Benchmark number for the README headline** — "what does ACS do that a
      markdown file can't" answered with a demo, not adjectives

## 5. Channel plan (ranked by expected stars/effort)

### Tier 1 — launch events

1. **Show HN** (Tue–Thu, ~9–11am ET / 14:00–16:00 UTC). Title:
   *"Show HN: qagent – a local-first control plane and message bus for coding
   agents"*. First comment ready (in `launch-copy.md`): how it works, why built,
   what feedback wanted. Sit in the thread 4–6h, answer everything. One shot.
   Lead with pillar 1; the GIF does pillar 3.
2. **r/claudecode** (same day, after HN is up). First-person build post with the
   required disclosure (what it does, who benefits, cost, relationship). Claude
   harness + MCP is the fit proof.
3. **r/mcp** (same day or next). Server announcement; technical tone: 14 agent
   tools, 1 operator tool, stdio, token model.
4. **X/Twitter** (launch day). The drafted post + the `acs` GIF clip. Reply under
   relevant threads the same week — aggregators amplify demos, not announcements.

### Tier 2 — compounding evergreen (week 1)

5. **MCP registry sweep, one afternoon:** official registry (needs npm publish;
   `mcp-publisher` + `server.json`) → Glama (`glama.json` claims ownership; feeds
   punkpeye's ~92k★ awesome-mcp-servers) → Smithery (`smithery.yaml` + GitHub
   OAuth) → mcpservers.org (feeds wong2's list; free or $39) → mcp.so.
6. **awesome-* submissions:** hesreallyhim/awesome-claude-code (needs 14 days or
   100★ — file after launch), e2b/awesome-ai-agents, kyrolabs/awesome-agents,
   adw0rd/awesome-mcp-servers. Submission text is in `submission-pack.md`.
7. **DevHunt** (weekly queue) + **Terminal Trove** (`acs` TUI is a perfect fit;
   needs a clean install line — `cargo install` or the npm-shipped binary) +
   **Console.dev pitch** (~30k curated devtool subs; short pitch + demo link).

### Tier 3 — owned content (weeks 2–4)

8. **dev.to long-form:** *"SQLite as a message bus for autonomous coding
   agents"* — WAL, polling vs fs events, stdio MCP, claim leases. Technical
   meat, tool mention as context. This is the citable asset for every later
   thread.
9. **r/LocalLLaMA** — *only after* ~2 weeks of genuine comments in the sub
   (self-promo ratio rule). Build-notes framing: "I run 4 local agents; here's
   what worked — one SQLite file, WAL as the bus." "No daemon, no cloud" is the
   entire pitch there.
10. **Daily thread-answering (20 min/day):** "how do I coordinate multiple
    Claude Code/agent sessions?" in r/claudecode, r/AI_Agents, r/mcp, HN
    comments. Helpful answer + disclosure + link. Replies out-convert posts.
11. **Failure-demo posts:** the features in PRs #7–#9 are the marketing —
    "your agents' claims expire, dead workers lose work to the pool, `qagent
    trace` is a free LangSmith for the bus, one `supervise --roster` runs the
    team." Each is a short clip/thread, ~1/week.

## 6. 30-day calendar

| Days | Actions |
|---|---|
| 1–3 | Merge the PR stack; `npm publish`; GitHub topics; verify `npx qagent init` cold-install works |
| 4 | **Show HN** + first comment; r/claudecode post; monitor both threads all day |
| 5 | r/mcp announcement; X post + clip; registry sweep (all 5 registries) |
| 6–7 | DevHunt + Terminal Trove + Console.dev pitch; dev.to article drafted |
| 8–14 | Daily thread-answering; LocalLLaMA comment warmup starts; awesome-list submissions; dev.to publish ~day 10 |
| 15–21 | Failure-demo clip #1 (requeue); LocalLLaMA build-notes post (if warmed up); awesome-claude-code submission at 100★ or day 14 |
| 22–30 | Failure-demo clips #2–3 (trace, roster); second dev.to piece if #1 performed; keep daily replies; report & double down on whichever channel converted |

## 7. Messaging matrix

| Claim | Where | Proof |
|---|---|---|
| No daemon, no cloud — one SQLite file | Everywhere (headline) | `qagent init` → `bus.db`; delete it and it's gone |
| Agents get real primitives, not a byte pipe | HN, dev.to, README | claims, leases, review gates, claim expiry, `task trace` |
| It launches your agents, not just watches them | r/claudecode, Show HN | `qagent supervise --roster`, 10 harness adapters |
| 4MB binary, 2ms CLI calls, TUI for non-experts | X, Terminal Trove, README | `acs` GIF, rust-port numbers |
| CrewAI's silent freezes can't happen here | Reddit replies, demos | dead-claim sweep → `--auto-requeue-min`, `task requeue` |

## 8. KPIs & decision points

- **Launch 48h:** HN front page? Stars + comments. If <50★, diagnose: title vs
  hook vs demo — the GIF/README is suspect #1.
- **Week 1:** ≥200★ target; registry listings live (Glama auto-verifies indexing
  — check tools render).
- **Week 2:** referral mix — if registries/awesome-lists >30% of traffic, lean
  into evergreen; if HN/Reddit dominated, more demo clips.
- **Day 30:** 1k★ needs ~150–350★/week compounding after the spike. If trending
  short, the failure-demo clips and a second Show-HN-adjacent launch (post the
  `acs` TUI itself — "Show HN: a 4MB TUI to run your agent team") is the
  reserve play.

## 9. Risks & honest constraints

- **HN is one shot** — a flat Show HN can't be re-run next week; the reserve is
  a second, distinct launch artifact (the TUI), not a repost.
- **Everything routes through the owner's accounts** (HN, Reddit, X, npm).
  Self-promo rules are real: r/LocalLLaMA needs comment history first; HN
  forbids soliciting upvotes; Reddit wants disclosure. Copy is drafted; posting
  is yours.
- **Claims must be true on main.** Merging #3–#9 before launch is load-bearing —
  the pitch demos stall-requeue, trace, and roster.
- **No paid promotion assumed.** Optional spends exist (DevHunt featured ~$49,
  mcpservers.org fast lane ~$39) — minor and optional.

## 10. What AI/Devin owns vs. what needs you

| Devin (already done or ready) | You |
|---|---|
| Research docs, strategy, launch copy, submission pack, manifests, GIF, README, features | Merge PRs; `npm publish`; GitHub topics |
| Monitoring: CI green, review triage done | Post: Show HN, Reddit, X (your accounts) |
| Follow-up PRs, fixes, docs updates | Registry submissions needing OAuth (Smithery) |
