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

One server-rendered page from `qagent dashboard` (`src/dashboard/`), on 127.0.0.1 only. It is a tool the operator glances at, not a report and not an app. It reads the bus and does one thing: send a message as the operator. Review, cancel, start and stop live in the CLI and `qagent supervise`.

## Look

- Flat `#181818` page. Nothing is raised: no panels, cards, shadows, gradients or rounded corners.
- One reading column, at most 960 px wide, with a 16 px gutter.
- System sans at 14 px. Monospace only for agent ids and task numbers.
- Structure comes from hairline rules (`#2e2e2e` under headings, `#242424` between rows) and spacing.
- Status is a plain word: waiting, working, idle, offline; open, claimed, submitted, changes requested. Words that matter less (offline, unassigned, ages) are muted grey. There is no status colour.
- One accent, `#6aa0ff`, for links and the keyboard focus ring only.
- No badges, pills, rings, dots, icons, KPI tiles, charts or counters.

## Layout

From top to bottom:

1. **Title and status line.** "Qagent", then one muted line: database path, "N of M agents online", "last change 2 min ago", and the stream state as a word (connecting, live, reconnecting, signed out).
2. **Agents.** Agent, last seen, state.
3. **Open tasks.** Task number, title, assignee (or "unassigned"), status, age.
4. **Recent messages.** Newest first, last 100: from, to ("everyone" for a broadcast), first line, age.
5. **Send a message.** "To" (agent id, `a,b` or `*`, with the known ids offered) and "Text", then a plain outlined "Send as operator" button with a one-line result beside it.

Below 600 px the table header is hidden and each row becomes lines: the short fields on one wrapping line, the long field (title or message) on its own line under it. Long ids and paths wrap; the page never scrolls sideways.

## Behaviour

- The first paint is complete HTML from the server; the script only keeps it current.
- One EventSource per tab. The server pushes a `change` delta (events, and the agent, task and message rows they name) or `reset` (the page reloads). The only client timer is a 30 s local re-render so ages stay current; it makes no request.
- The page is inert without a session: it shows how to get a sign-in link (`qagent dashboard link`) and nothing from the bus.

## Security in the page

- CSS and script are inline under a per-response CSP nonce. `default-src 'none'`, `connect-src 'self'`, `frame-ancestors 'none'`. No external files, fonts or images.
- The session cookie is HttpOnly and SameSite=Strict; the operator token never reaches the browser. The sign-in ticket travels in the URL fragment, is posted once, and is removed from the address bar.

## Don't

- Don't add a second write, a settings page, or controls for agents or tasks.
- Don't add colour to status, or any decoration that restates what a word already says.
- Don't load webfonts, frameworks or a build step.
