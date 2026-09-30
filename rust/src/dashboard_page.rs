// Page rendering for the dashboard (port of page.ts + assets.ts).
// Included by `mod page` in dashboard.rs. The browser re-renders rows with
// the same renderer code — SHARED_JS carries the JS source of the row
// renderers verbatim so a Rust HTML page and a JS DOM update look identical.

use serde_json::{json, Value};

fn esc(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

fn ago(ms: Option<i64>, now: i64) -> String {
    let Some(ms) = ms else { return "never".to_string() };
    let s = ((now - ms) / 1000).max(0);
    if s < 10 {
        return "now".into();
    }
    if s < 60 {
        return format!("{s} s ago");
    }
    let m = s / 60;
    if m < 60 {
        return format!("{m} min ago");
    }
    let h = m / 60;
    if h < 48 {
        return format!("{h} h ago");
    }
    format!("{} d ago", h / 24)
}

/// Same rule as Bus::to_agent: a waiter past its deadline, or an agent unseen
/// for staleMs, is offline.
fn agent_state(agent: &Value, now: i64, stale_ms: i64) -> String {
    let status = agent["status"].as_str().unwrap_or("");
    if status == "waiting" {
        return match agent["waitUntilMs"].as_i64() {
            Some(until) if until >= now => "waiting".into(),
            _ => "offline".into(),
        };
    }
    if status == "offline" {
        return "offline".into();
    }
    match agent["lastSeenMs"].as_i64() {
        Some(seen) if now - seen <= stale_ms => status.to_string(),
        _ => "offline".into(),
    }
}

fn agent_rows(agents: &[Value], now: i64, stale_ms: i64) -> String {
    if agents.is_empty() {
        return r#"<tr><td class="none wide">No agents yet.</td></tr>"#.to_string();
    }
    agents
        .iter()
        .map(|agent| {
            let state = agent_state(agent, now, stale_ms);
            format!(
                r#"<tr><td class="id">{}</td><td class="m">{}</td><td class="{}">{}</td></tr>"#,
                esc(agent["id"].as_str().unwrap_or("")),
                esc(&ago(agent["lastSeenMs"].as_i64(), now)),
                if state == "offline" { "m" } else { "" },
                esc(&state),
            )
        })
        .collect::<Vec<_>>()
        .join("")
}

fn task_rows(tasks: &[Value], now: i64) -> String {
    if tasks.is_empty() {
        return r#"<tr><td class="none wide">No open tasks.</td></tr>"#.to_string();
    }
    tasks
        .iter()
        .map(|task| {
            let assignee = task["assignee"].as_str().unwrap_or("");
            format!(
                r#"<tr><td class="id">#{}</td><td class="wide">{}</td><td class="{}">{}</td><td>{}</td><td class="m">{}</td></tr>"#,
                task["id"].as_i64().unwrap_or(0),
                esc(task["title"].as_str().unwrap_or("")),
                if assignee.is_empty() { "m" } else { "" },
                esc(if assignee.is_empty() { "unassigned" } else { assignee }),
                esc(&task["state"].as_str().unwrap_or("").replace('_', " ")),
                esc(&ago(task["createdMs"].as_i64(), now)),
            )
        })
        .collect::<Vec<_>>()
        .join("")
}

fn message_rows(messages: &[Value], now: i64) -> String {
    if messages.is_empty() {
        return r#"<tr><td class="none wide">No messages yet.</td></tr>"#.to_string();
    }
    messages
        .iter()
        .map(|message| {
            format!(
                r#"<tr><td class="id">{}</td><td class="id">{}</td><td class="wide">{}</td><td class="m">{}</td></tr>"#,
                esc(message["sender"].as_str().unwrap_or("")),
                esc(message["recipient"].as_str().unwrap_or("everyone")),
                esc(message["line"].as_str().unwrap_or("")),
                esc(&ago(message["tsMs"].as_i64(), now)),
            )
        })
        .collect::<Vec<_>>()
        .join("")
}

fn status_line(state: &Value, now: i64, stale_ms: i64) -> String {
    let agents = state["agents"].as_array().cloned().unwrap_or_default();
    let online = agents
        .iter()
        .filter(|a| agent_state(a, now, stale_ms) != "offline")
        .count();
    format!(
        "{} · {} of {} agents online · last change {}",
        esc(state["dbPath"].as_str().unwrap_or("")),
        online,
        agents.len(),
        esc(&ago(state["lastChangeMs"].as_i64(), now)),
    )
}

/// JSON inside a <script> element: escape "<" so no value can close the element.
fn script_json(value: &Value) -> String {
    serde_json::to_string(value)
        .unwrap_or_default()
        .replace('<', "\\u003c")
}

fn shell(nonce: &str, body: &str, script: &str) -> String {
    format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n<meta name=\"referrer\" content=\"no-referrer\">\n<title>Qagent</title>\n<style nonce=\"{nonce}\">{CSS}</style>\n</head>\n<body>\n<main>\n{body}\n</main>\n<script nonce=\"{nonce}\">{script}</script>\n</body>\n</html>\n"
    )
}

pub fn render_page(state: &Value, nonce: &str, now: i64) -> String {
    let stale_ms = crate::types::STALE_AGENT_MS;
    let agents = state["agents"].as_array().cloned().unwrap_or_default();
    let tasks = state["tasks"].as_array().cloned().unwrap_or_default();
    let messages = state["messages"].as_array().cloned().unwrap_or_default();
    let ids = agents
        .iter()
        .map(|a| format!(r#"<option value="{}">"#, esc(a["id"].as_str().unwrap_or(""))))
        .collect::<Vec<_>>()
        .join("");
    let mut boot = state.clone();
    boot["staleMs"] = json!(stale_ms);
    let body = format!(
        "<header>\n<h1>Qagent</h1>\n<p class=\"status\"><span id=\"status\">{status}</span> · <span id=\"live\">connecting</span></p>\n</header>\n<section aria-labelledby=\"h-agents\">\n<h2 id=\"h-agents\">Agents</h2>\n<table class=\"agents\"><thead><tr><th>Agent</th><th>Last seen</th><th>State</th></tr></thead>\n<tbody id=\"agent-rows\">{agent_rows}</tbody></table>\n</section>\n<section aria-labelledby=\"h-tasks\">\n<h2 id=\"h-tasks\">Open tasks</h2>\n<table class=\"tasks\"><thead><tr><th>Task</th><th>Title</th><th>Assignee</th><th>Status</th><th>Age</th></tr></thead>\n<tbody id=\"task-rows\">{task_rows}</tbody></table>\n</section>\n<section aria-labelledby=\"h-messages\">\n<h2 id=\"h-messages\">Recent messages</h2>\n<table class=\"messages\"><thead><tr><th>From</th><th>To</th><th>Message</th><th>Age</th></tr></thead>\n<tbody id=\"message-rows\">{message_rows}</tbody></table>\n</section>\n<section aria-labelledby=\"h-send\">\n<h2 id=\"h-send\">Send a message</h2>\n<form id=\"send\" autocomplete=\"off\">\n<label for=\"send-to\">To</label>\n<input id=\"send-to\" name=\"to\" list=\"agent-ids\" required maxlength=\"2000\" spellcheck=\"false\" placeholder=\"agent id, a,b or *\">\n<datalist id=\"agent-ids\">{ids}</datalist>\n<label for=\"send-text\">Text</label>\n<textarea id=\"send-text\" name=\"text\" rows=\"4\" required></textarea>\n<p class=\"row\"><button type=\"submit\">Send as operator</button> <span id=\"send-status\" class=\"m\" role=\"status\"></span></p>\n</form>\n</section>\n<script type=\"application/json\" id=\"boot\">{boot_json}</script>",
        status = status_line(state, now, stale_ms),
        agent_rows = agent_rows(&agents, now, stale_ms),
        task_rows = task_rows(&tasks, now),
        message_rows = message_rows(&messages, now),
        ids = ids,
        boot_json = script_json(&boot),
    );
    shell(nonce, &body, &client_script())
}

pub fn render_signed_out(nonce: &str) -> String {
    let body = "<header>\n<h1>Qagent</h1>\n<p class=\"status\"><span id=\"live\">not signed in</span></p>\n</header>\n<section>\n<p>This dashboard needs a sign-in link. Run <code>qagent dashboard link</code> in a terminal and open the address it prints. Each link works once, for five minutes.</p>\n</section>";
    let script = r#"(() => {
  const match = /^#t=([A-Za-z0-9_-]+)$/.exec(location.hash);
  if (!match) return;
  history.replaceState(null, "", "/");
  const live = document.getElementById("live");
  live.textContent = "signing in";
  fetch("/session", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ ticket: match[1] }) })
    .then((response) => { if (response.ok) location.replace("/"); else live.textContent = "sign-in link invalid or expired"; })
    .catch(() => { live.textContent = "sign-in failed"; });
})();"#;
    shell(nonce, body, script)
}

fn client_script() -> String {
    format!("(() => {{\n\"use strict\";\n{SHARED_JS}\n{CLIENT_JS}\n}})();")
}

/// The shared row renderers, as JS — the browser applies SSE deltas with the
/// same code the server used for first paint (verbatim from page.ts).
const SHARED_JS: &str = r#"function esc(value) {
  return String(value ?? "").replace(/[&<>"']/g, (c) => c === "&" ? "&amp;" : c === "<" ? "&lt;" : c === ">" ? "&gt;" : c === '"' ? "&quot;" : "&#39;");
}
function ago(ms, now) {
  if (ms === null || ms === undefined) return "never";
  const s = Math.max(0, Math.round((now - ms) / 1000));
  if (s < 10) return "now";
  if (s < 60) return s + " s ago";
  const m = Math.floor(s / 60);
  if (m < 60) return m + " min ago";
  const h = Math.floor(m / 60);
  if (h < 48) return h + " h ago";
  return Math.floor(h / 24) + " d ago";
}
function agentState(agent, now, staleMs) {
  if (agent.status === "waiting") return agent.waitUntilMs !== null && agent.waitUntilMs >= now ? "waiting" : "offline";
  if (agent.status === "offline") return "offline";
  return agent.lastSeenMs !== null && now - agent.lastSeenMs <= staleMs ? agent.status : "offline";
}
function agentRows(agents, now, staleMs) {
  if (!agents.length) return '<tr><td class="none wide">No agents yet.</td></tr>';
  return agents.map((agent) => {
    const state = agentState(agent, now, staleMs);
    return '<tr><td class="id">' + esc(agent.id) + '</td><td class="m">' + esc(ago(agent.lastSeenMs, now)) +
      '</td><td class="' + (state === "offline" ? "m" : "") + '">' + esc(state) + "</td></tr>";
  }).join("");
}
function taskRows(tasks, now) {
  if (!tasks.length) return '<tr><td class="none wide">No open tasks.</td></tr>';
  return tasks.map((task) => '<tr><td class="id">#' + esc(task.id) + '</td><td class="wide">' + esc(task.title) +
    '</td><td class="' + (task.assignee ? "" : "m") + '">' + esc(task.assignee || "unassigned") + "</td><td>" + esc(String(task.state).replace(/_/g, " ")) +
    '</td><td class="m">' + esc(ago(task.createdMs, now)) + "</td></tr>").join("");
}
function messageRows(messages, now) {
  if (!messages.length) return '<tr><td class="none wide">No messages yet.</td></tr>';
  return messages.map((message) => '<tr><td class="id">' + esc(message.sender) + '</td><td class="id">' + esc(message.recipient || "everyone") +
    '</td><td class="wide">' + esc(message.line) + '</td><td class="m">' + esc(ago(message.tsMs, now)) + "</td></tr>").join("");
}
function statusLine(state, now, staleMs) {
  const online = state.agents.filter((agent) => agentState(agent, now, staleMs) !== "offline").length;
  return esc(state.dbPath) + " · " + online + " of " + state.agents.length + " agents online · last change " + esc(ago(state.lastChangeMs, now));
}
"#;

/// Client logic — verbatim from assets.ts.
const CLIENT_JS: &str = r#"
const boot = JSON.parse(document.getElementById("boot").textContent);
const staleMs = boot.staleMs;
const agents = new Map(boot.agents.map((a) => [a.id, a]));
const tasks = new Map(boot.tasks.map((t) => [t.id, t]));
const messages = new Map(boot.messages.map((m) => [m.seq, m]));
let lastChangeMs = boot.lastChangeMs;
const $ = (id) => document.getElementById(id);

function render() {
  const now = Date.now();
  const agentList = Array.from(agents.values()).sort((a, b) => a.id < b.id ? -1 : 1);
  $("agent-rows").innerHTML = agentRows(agentList, now, staleMs);
  $("task-rows").innerHTML = taskRows(Array.from(tasks.values()).sort((a, b) => a.id - b.id), now);
  $("message-rows").innerHTML = messageRows(Array.from(messages.values()).sort((a, b) => b.seq - a.seq), now);
  $("status").textContent = "";
  $("status").insertAdjacentHTML("afterbegin", statusLine({ dbPath: boot.dbPath, agents: agentList, lastChangeMs: lastChangeMs }, now, staleMs));
  $("agent-ids").innerHTML = agentList.map((a) => '<option value="' + esc(a.id) + '">').join("");
}

function apply(delta) {
  for (const a of delta.agents || []) agents.set(a.id, a);
  for (const t of delta.tasks || []) { if (t.closed) tasks.delete(t.id); else tasks.set(t.id, t); }
  for (const m of delta.messages || []) messages.set(m.seq, m);
  if (messages.size > 100) {
    const drop = Array.from(messages.keys()).sort((a, b) => a - b).slice(0, messages.size - 100);
    for (const seq of drop) messages.delete(seq);
  }
  if (delta.lastChangeMs) lastChangeMs = delta.lastChangeMs;
  render();
}

const live = $("live");
const stream = new EventSource("/api/events?since=" + encodeURIComponent(boot.seq));
stream.addEventListener("open", () => { live.textContent = "live"; });
stream.addEventListener("change", (event) => { apply(JSON.parse(event.data)); });
stream.addEventListener("reset", () => { location.reload(); });
stream.addEventListener("error", () => {
  live.textContent = stream.readyState === EventSource.CLOSED ? "signed out; run qagent dashboard link" : "reconnecting";
});
setInterval(render, 30000);

const form = $("send");
const sendStatus = $("send-status");
form.addEventListener("submit", async (event) => {
  event.preventDefault();
  const to = $("send-to").value.trim();
  const text = $("send-text").value;
  if (!to || !text.trim()) return;
  const button = form.querySelector("button");
  button.disabled = true;
  sendStatus.textContent = "sending";
  try {
    const response = await fetch("/api/send", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ to: to, text: text }),
    });
    const body = await response.json().catch(() => ({}));
    if (response.ok) {
      $("send-text").value = "";
      sendStatus.textContent = "sent to " + to;
    } else {
      sendStatus.textContent = "not sent: " + (body.error || response.status);
    }
  } catch (error) {
    sendStatus.textContent = "not sent: dashboard unreachable";
  } finally {
    button.disabled = false;
  }
});
"#;

/// Stylesheet — verbatim from assets.ts.
const CSS: &str = r#"
:root { color-scheme: dark; }
* { box-sizing: border-box; }
html, body { margin: 0; background: #181818; color: #dddddd; }
body { font: 14px/1.5 -apple-system, BlinkMacSystemFont, "Segoe UI", Helvetica, Arial, sans-serif; }
main { max-width: 960px; margin: 0 auto; padding: 28px 16px 64px; }
h1 { font-size: 16px; font-weight: 600; margin: 0 0 2px; }
h2 { font-size: 14px; font-weight: 600; margin: 36px 0 0; padding-bottom: 8px; border-bottom: 1px solid #2e2e2e; }
p { margin: 0 0 12px; }
.status { color: #8c8c8c; overflow-wrap: anywhere; }
table { width: 100%; border-collapse: collapse; table-layout: fixed; }
th { text-align: left; font-weight: 400; color: #8c8c8c; padding: 6px 12px 6px 0; border-bottom: 1px solid #2e2e2e; }
td { padding: 7px 12px 7px 0; border-bottom: 1px solid #242424; vertical-align: top; overflow-wrap: anywhere; }
.id, code { font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; font-size: 13px; }
.m, .none { color: #8c8c8c; }
.agents th:nth-child(1) { width: 40%; }
.agents th:nth-child(2) { width: 30%; }
.tasks th:nth-child(1) { width: 64px; }
.tasks th:nth-child(3) { width: 140px; }
.tasks th:nth-child(4) { width: 150px; }
.tasks th:nth-child(5), .messages th:nth-child(4) { width: 96px; }
.messages th:nth-child(1), .messages th:nth-child(2) { width: 120px; }
form { margin-top: 12px; }
label { display: block; color: #8c8c8c; margin: 10px 0 4px; }
input, textarea { display: block; width: 100%; font: inherit; color: #dddddd; background: #181818;
  border: 1px solid #3a3a3a; border-radius: 0; padding: 7px 8px; }
textarea { resize: vertical; }
button { font: inherit; color: #dddddd; background: #181818; border: 1px solid #5a5a5a; border-radius: 0; padding: 6px 14px; cursor: pointer; }
button:hover { border-color: #8c8c8c; }
button:disabled { color: #8c8c8c; cursor: default; }
.row { margin-top: 12px; }
a { color: #6aa0ff; }
:focus-visible { outline: 2px solid #6aa0ff; outline-offset: 1px; }
@media (max-width: 600px) {
  main { padding-top: 20px; }
  thead { display: none; }
  table, tbody { display: block; }
  tr { display: flex; flex-wrap: wrap; column-gap: 12px; padding: 8px 0; border-bottom: 1px solid #242424; }
  td { display: block; padding: 0; border: 0; }
  td.wide { order: 2; flex-basis: 100%; }
}
"#;
