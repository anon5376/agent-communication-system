# ACS Promotion Plan

**Positioning wedge.** The crowded corner of this space is "run agents in parallel" UIs (Vibe Kanban, Claude Squad). ACS is different: it's the coordination *substrate* — durable agent-to-agent mail, task queues with claims/leases, review gates, shared state in one SQLite file. Lead everywhere with: **"your coding agents get an inbox, a task queue, and shared state — in one SQLite file, no daemon, no cloud."**

## Fix before promoting (gates several channels)

- ~~**No LICENSE file and no `license` field in package.json.**~~ LICENSE added in this PR (README already claimed MIT). Still needed: `"license": "MIT"` in package.json.
- **`"private": true`, not on npm.** Install is clone → `npm ci` → `npm run build` → `npm link`. Publishing so `npx qagent init` works is a hard prerequisite for the official MCP Registry and Smithery, and removes the biggest "try it now" friction for Show HN.
- **Record a 30–60s demo GIF**: two agents passing a task through the bus + the dashboard. Needed for Terminal Trove, Reddit posts, X, README.
- **Set GitHub topics**: `mcp-server`, `mcp`, `ai-agents`, `multi-agent`, `local-first`, `sqlite`, `claude-code`, `developer-tools`. Free; feeds awesome-list crawlers and GitHub search.

## Ranked channels

| # | Channel | Audience fit | Effort | Expected impact |
|---|---------|-------------|--------|-----------------|
| 1 | **Show HN** | Dev-infra/OSS readers; direct comp Vibe Kanban front-paged with a weaker moat | Medium | Highest ceiling — front page = thousands of stars/visitors |
| 2 | **r/claudecode** | Largest concentration of people literally running multiple coding agents; ACS has a Claude harness + MCP | Low | High — exact ICP |
| 3 | **MCP registries** (official, Glama, Smithery, mcpservers.org) | People searching for MCP servers right now | Low | Evergreen, compounding; feeds awesome lists automatically |
| 4 | **r/LocalLLaMA** | Huge (~local-first ethos); "no cloud, one file, runs on my machine" is catnip | Medium | High if framed as build-notes, not launch |
| 5 | **r/mcp** | Small but perfectly on-topic; server announcements are the norm | Low | Medium — qualified clicks |
| 6 | **r/AI_Agents** (+ r/AgentsOfAI) | Agent builders; self-promo tolerated | Low | Medium |
| 7 | **awesome-* lists** | Passive discovery + SEO | Low | Evergreen referral traffic |
| 8 | **X/Twitter AI-dev** | Demo clips get amplified by aggregator accounts | Medium | Spiky; feeds all other channels |
| 9 | **dev.to long-form** | Devs searching "coordinate multiple agents" | Medium | SEO + credibility asset to cite everywhere |
| 10 | **Console.dev** | Curated devtools newsletter, ~30k+ subs | Low | High-quality if accepted |
| 11 | **DevHunt** | Dev-only Product Hunt alternative; weekly queue | Low | Modest but targeted |
| 12 | **Terminal Trove** | TUI/CLI directory; the ~4MB Rust control app is a perfect fit | Low | Small, evergreen |

## Per-channel notes

### 1. Show HN — post Tue–Thu, ~9–11am ET (14:00–16:00 UTC)
- Title must start `Show HN:` and be neutral, no hype. E.g. *"Show HN: qagent – local-first control plane for coordinating multiple coding agents"*.
- Product must be **tryable without signup** — the clone-build-install is acceptable to HN but `npx` install converts much better. Publish to npm first.
- Post a first comment right after submitting: how it works (SQLite WAL as the bus, stdio MCP, harness adapters), why you built it, what you'd like feedback on.
- Stay in the thread for hours; reply to every comment like a human. **Never ask anyone to upvote** — that's the fastest way to get flagged.
- Timing is marginal vs. title/topic fit, but weekday US mornings are the sane default. One shot — don't repost quickly.

### 2. r/claudecode — disclosure rule
- Rule: you may share tools if you "clearly state what it does, who benefits, any costs, and your relationship to it. No clickbait, referral spam, or repeated promotion."
- Format: first-person build post — *"I built an MCP server that gives Claude Code instances an inbox + task queue (one SQLite file, no daemon). Free, [license]. I made it."* Lead with what it does technically; link at the bottom.
- Also monitor the sub for "how do I coordinate parallel Claude Code sessions" questions and answer them — replies convert better than posts.

### 3. MCP registries — do the sweep in one afternoon
- **Official MCP Registry** (registry.modelcontextprotocol.io): publish via `mcp-publisher` CLI + `server.json`; requires a published package (npm) for a stdio server, plus namespace verification (GitHub login for `io.github.*` works). Other directories copy from this — do it first.
- **Glama** (glama.ai): submit the GitHub repo URL; it auto-indexes tools/schemas and feeds **punkpeye/awesome-mcp-servers (~92k★)**. Add `glama.json` to repo root to claim ownership.
- **Smithery** (smithery.ai): add `smithery.yaml` to the repo, submit repo URL via GitHub OAuth.
- **mcpservers.org**: web form — this is also the intake for wong2/awesome-mcp-servers (that list no longer accepts PRs). Free tier or $39 fast lane.
- **mcp.so**: form; free or $39.

### 4. r/LocalLLaMA — contribute first, lead with the local-first angle
- 1/10th self-promo rule, affiliation must be disclosed; a fresh account posting a launch is the classic removal trigger. Comment for a week or two first.
- Frame as build notes, not a launch: *"I run 4 local coding agents; I built a message bus for them on one SQLite file — here's what worked (WAL, polling vs fs events, stdio MCP)."* Specs + lessons, tool link as context. Closed/cloud tooling gets pushback there — "no daemon, no cloud, single file" is the entire pitch.
- Best window: ~14:00–18:00 UTC weekdays.

### 5. r/mcp
- Server announcements are the community's bread and butter; keep it technical (14 tools, stdio, operator-only tool, token model). Low risk, qualified audience.

### 6. r/AI_Agents (+ r/AgentsOfAI)
- "We built X, looking for testers" is a recognized format. Use the showcase flair if present. This crowd distinguishes real orchestration (task queues, tool use) from chatbots — lead with the task-claim/review-gate mechanics, not "agents talk to each other."

### 7. awesome-* lists (see directory section below)

### 8. X/Twitter
- Hook tweet + demo clip/GIF (dashboard + terminal app), then a short thread: problem → design (one SQLite file) → install command. No hashtag stuffing; #buildinpublic is fine. Post during US hours, engage in replies. Aggregator accounts (AI dev tools, MCP roundups) pick up good demos — also tag/reply under relevant MCP and Claude Code community threads.

### 9. dev.to
- It's an article channel, not a launchpad — "we just launched X" dies. Write the engineering story instead: *"SQLite as a message bus for autonomous coding agents"* — WAL concurrency, claim/lease design, why no broker. Tags: `#showdev #ai #agents #sqlite`. If you also blog it, use `canonical_url` so your site keeps the SEO.

### 10. Console.dev
- Free weekly devtools newsletter (Thu), editorially reviewed — pitch them via console.dev's contact inbox with a 3-sentence blurb emphasizing DX and time-to-first-agent. Only pitch once docs/onboarding are polished.

### 11. DevHunt
- devhunt.org — dev-tools-only launchpad, weekly batches, GitHub-verified voters. Free queue or $49 to pick a week. Explicitly accepts "AI agents and MCP servers."

### 12. Terminal Trove
- terminaltrove.com/post — submission form for the ~4MB Rust TUI. Requires: preview image (GIF recommended) **and install commands via package repositories** — so ship `cargo install`/brew/npm availability first.

## Directories & lists that accept submissions

| Destination | How to submit | Notes |
|---|---|---|
| Official MCP Registry | `mcp-publisher` CLI + `server.json` | Needs published npm package + namespace verify |
| Glama → punkpeye/awesome-mcp-servers (~92k★) | Submit repo URL on glama.ai; `glama.json` to claim | One submission feeds both |
| Smithery | `smithery.yaml` + repo URL via GitHub OAuth | |
| mcpservers.org / wong2 awesome-mcp-servers | mcpservers.org/submit form | List takes no PRs; form is the only path |
| hesreallyhim/awesome-claude-code (~50k★) | **Issue form only** (issues/new?template=recommend-resource.yml) — PRs rejected, must be human-submitted, one-line non-sales description | Has an "Orchestrators" category — perfect fit. Note: recommendations were temporarily paused during a redesign; check before filing |
| e2b-dev/awesome-ai-agents (~30k★) | PR | Multi-agent category |
| CharlieLZ/awesome-agents | PR | Has agent-communication/protocol categories |
| adw0rd/awesome-mcp-servers | PR (README format) | |
| mcp.so | Form | Free or $39 |
| Terminal Trove | Web form | Needs package-manager install + preview GIF |
| DevHunt | Account → submit tool | Free queue or $49 scheduled |
| GitHub topics | Repo settings | `mcp-server`, `ai-agents`, `local-first`, `sqlite`, `claude-code` |

## Worth it only opportunistically

- **Lobste.rs** — high signal, but posting requires an invite and self-promo must stay under ~25% of your submissions/comments. Do it only if you already have (or can get) an account.
- **Discords** — official Anthropic/Claude Discord, the MCP Discord, LocalLLaMA Discord: share in showcase channels after being present; ambient reach, don't drive-by.
- **Product Hunt proper** — 12:01am PT Tue–Thu launch, gallery assets, comment desk all day; real production for modest OSS return. DevHunt is the cheaper equivalent.
- **r/ClaudeAI** — Rule 7 allows showcases, but only if the project was built with Claude/Claude Code and you describe concretely how; ~50 karma minimum; modmail first is the safe path. Worth it only if the "built with Claude Code" story is genuinely true and substantial.

## Skip

- **r/programming, r/coding** — self-promo removed on sight; only a standalone technical article could survive, and even that is thin ROI.
- **Paid newsletters** (TLDR AI ~$ placement, Ben's Bites $200–$2k slots, The Sequence) — poor ROI for a free OSS tool with no budget; Console.dev is the free editorial route.

## Suggested sequence

- **Week 0 (prep):** LICENSE + npm publish + demo GIF + GitHub topics + `smithery.yaml`/`glama.json`/`server.json` in-repo.
- **Launch day (Tue–Thu):** Show HN at ~9am ET → r/claudecode + r/mcp → X clip → be in the HN thread all day.
- **Week 1 (evergreen):** registry sweep (official → Glama → Smithery → mcpservers.org → mcp.so), awesome-list issues/PRs, DevHunt submission, Terminal Trove, Console.dev pitch.
- **Weeks 2–4 (compound):** dev.to deep-dive, answer "how do I coordinate parallel agents" threads across claudecode/LocalLLaMA/AI_Agents, build karma before any LocalLLaMA post.
