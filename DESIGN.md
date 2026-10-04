---
name: Qagent dashboard
description: Optional local page that shows the bus and sends one kind of message.
colors:
  bg: "#181818"
  text: "#dddddd"
  muted: "#8c8c8c"
  rule: "#2e2e2e"
  rule-row: "#242424"
  field-border: "#3a3a3a"
  accent: "#6aa0ff"
typography:
  body:
    fontFamily: "-apple-system, BlinkMacSystemFont, Segoe UI, Helvetica, Arial, sans-serif"
    fontSize: "14px"
    fontWeight: 400
    lineHeight: 1.5
  heading:
    fontFamily: "{typography.body.fontFamily}"
    fontSize: "14px"
    fontWeight: 600
  title:
    fontFamily: "{typography.body.fontFamily}"
    fontSize: "16px"
    fontWeight: 600
  mono:
    fontFamily: "ui-monospace, SFMono-Regular, Menlo, Consolas, monospace"
    fontSize: "13px"
rounded:
  none: "0"
spacing:
  gutter: "16px"
  column: "960px"
  section: "36px"
---

# Design: Qagent dashboard

## What it is

One server-rendered page from `qagent dashboard` (`src/dashboard/`), on 127.0.0.1 only. It is a tool the operator glances at, not a report and not an app. It reads the bus, leads with what needs the operator, and does one thing: send a message as the operator. Review, cancel, requeue, start and stop live in the CLI and `qagent supervise`; the page shows the exact command instead of a button.

## Look

- Flat `#181818` page. Nothing is raised: no panels, cards, shadows, gradients or rounded corners.
- One reading column, at most 960 px wide, with a 16 px gutter.
- System sans at 14 px. Monospace only for agent ids and task numbers.
- Structure comes from hairline rules (`#2e2e2e` under headings, `#242424` between rows) and spacing.
- Status is a plain word: waiting, working, idle, offline; needs review, failed, blocked, stalled, active, queued. Words that matter less (offline, unassigned, ages) are muted grey. There is no status colour.
- One accent, `#6aa0ff`, for links and the keyboard focus ring only.
- No badges, pills, rings, dots, icons, KPI tiles, charts or counters.

## Layout

From top to bottom:

1. **Title and status line.** "Qagent", then one muted line: database path, "N tasks need you" (or "nothing needs you"), "N of M agents online", "last change 2 min ago", and the stream state as a word (connecting, live, reconnecting, signed out).
2. **Needs you.** Tasks that wait on the operator, most urgent first: needs review, then failed or blocked, then stalled. Each row has the task number, a status word, the title, the reason in one sentence, the evidence (muted: ages, last seen, the latest summary, feedback or error), and the exact CLI command to run next. Empty: "Nothing needs you."
3. **Active and queued.** Work going on without the operator, active first, then queued: task number, title with its reason under it, assignee (or "unassigned"), status word, last change.
4. **Agents.** Agent, last seen, state.
5. **Recent messages.** Newest first, last 100: from, to ("everyone" for a broadcast), first line, age.
6. **Send a message.** "To" (agent id, `a,b` or `*`, with the known ids offered) and "Text", then a plain outlined "Send as operator" button with a one-line result beside it.

The attention rules live in `src/attention.ts` and are shared with `qagent doctor` (an "attention" block) and `qagent trace` (a "now:" line), so all three say the same thing about a task. A claim is stalled when its lease expired, or when neither the task nor its assignee has moved for 60 minutes (the `qagent task stalled` default). An open task assigned to an offline agent, or unassigned with no agent online, is stalled too. Failed tasks stay listed for a day.

Below 600 px the table header is hidden and each row becomes lines: the short fields on one wrapping line, the long field (title or message) on its own line under it. Long ids and paths wrap; the page never scrolls sideways.

## Behaviour

- The first paint is complete HTML from the server; the script only keeps it current.
- One EventSource per tab. The server pushes a `change` delta (events, the agent and message rows they name, and the whole task list when any task changed, since one task can block or unblock another) or `reset` (the page reloads). The only client timer is a 30 s local re-render so ages stay current and a claim that goes stale with no write still moves into "Needs you"; it makes no request.
- The page is inert without a session: it shows how to get a sign-in link (`qagent dashboard link`) and nothing from the bus.

## Security in the page

- CSS and script are inline under a per-response CSP nonce. `default-src 'none'`, `connect-src 'self'`, `frame-ancestors 'none'`. No external files, fonts or images.
- The session cookie is HttpOnly and SameSite=Strict; the operator token never reaches the browser. The sign-in ticket travels in the URL fragment, is posted once, and is removed from the address bar.

## Don't

- Don't add a second write, a settings page, or controls for agents or tasks.
- Don't add colour to status, or any decoration that restates what a word already says.
- Don't load webfonts, frameworks or a build step.
