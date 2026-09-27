/**
 * The dashboard's one page, rendered on the server.
 *
 * The row renderers below are plain, self-contained functions. The server calls
 * them to render the first paint, and assets.ts inlines their source text into
 * the page script so the browser re-renders rows with the same code when an SSE
 * change arrives. Keep them free of imports and closures for that reason.
 */
import { STALE_AGENT_MS } from "../core/types.js";
import { CLIENT_JS, CSS } from "./assets.js";
export function esc(value) {
    return String(value ?? "").replace(/[&<>"']/g, (c) => c === "&" ? "&amp;" : c === "<" ? "&lt;" : c === ">" ? "&gt;" : c === '"' ? "&quot;" : "&#39;");
}
export function ago(ms, now) {
    if (ms === null || ms === undefined)
        return "never";
    const s = Math.max(0, Math.round((now - ms) / 1000));
    if (s < 10)
        return "now";
    if (s < 60)
        return s + " s ago";
    const m = Math.floor(s / 60);
    if (m < 60)
        return m + " min ago";
    const h = Math.floor(m / 60);
    if (h < 48)
        return h + " h ago";
    return Math.floor(h / 24) + " d ago";
}
/** Same rule as Bus.toAgent: a waiter past its deadline, or an agent unseen for staleMs, is offline. */
export function agentState(agent, now, staleMs) {
    if (agent.status === "waiting")
        return agent.waitUntilMs !== null && agent.waitUntilMs >= now ? "waiting" : "offline";
    if (agent.status === "offline")
        return "offline";
    return agent.lastSeenMs !== null && now - agent.lastSeenMs <= staleMs ? agent.status : "offline";
}
export function agentRows(agents, now, staleMs) {
    if (!agents.length)
        return '<tr><td class="none wide">No agents yet.</td></tr>';
    return agents.map((agent) => {
        const state = agentState(agent, now, staleMs);
        return '<tr><td class="id">' + esc(agent.id) + '</td><td class="m">' + esc(ago(agent.lastSeenMs, now)) +
            '</td><td class="' + (state === "offline" ? "m" : "") + '">' + esc(state) + "</td></tr>";
    }).join("");
}
export function taskRows(tasks, now) {
    if (!tasks.length)
        return '<tr><td class="none wide">No open tasks.</td></tr>';
    return tasks.map((task) => '<tr><td class="id">#' + esc(task.id) + '</td><td class="wide">' + esc(task.title) +
        '</td><td class="' + (task.assignee ? "" : "m") + '">' + esc(task.assignee || "unassigned") + "</td><td>" + esc(String(task.state).replace(/_/g, " ")) +
        '</td><td class="m">' + esc(ago(task.createdMs, now)) + "</td></tr>").join("");
}
export function messageRows(messages, now) {
    if (!messages.length)
        return '<tr><td class="none wide">No messages yet.</td></tr>';
    return messages.map((message) => '<tr><td class="id">' + esc(message.sender) + '</td><td class="id">' + esc(message.recipient || "everyone") +
        '</td><td class="wide">' + esc(message.line) + '</td><td class="m">' + esc(ago(message.tsMs, now)) + "</td></tr>").join("");
}
export function statusLine(state, now, staleMs) {
    const online = state.agents.filter((agent) => agentState(agent, now, staleMs) !== "offline").length;
    return esc(state.dbPath) + " · " + online + " of " + state.agents.length + " agents online · last change " + esc(ago(state.lastChangeMs, now));
}
/** The page script: the shared renderers' source text, then the client logic from assets.ts. */
function clientScript() {
    const shared = [esc, ago, agentState, agentRows, taskRows, messageRows, statusLine].map((fn) => fn.toString()).join("\n");
    return `(() => {\n"use strict";\n${shared}\n${CLIENT_JS}\n})();`;
}
/** JSON inside a <script> element: escape "<" so no value can close the element. */
function scriptJson(value) {
    return JSON.stringify(value).replace(/</g, "\\u003c");
}
function shell(nonce, body, script) {
    return `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="referrer" content="no-referrer">
<title>Qagent</title>
<style nonce="${nonce}">${CSS}</style>
</head>
<body>
<main>
${body}
</main>
<script nonce="${nonce}">${script}</script>
</body>
</html>
`;
}
export function renderPage(state, nonce, now = Date.now()) {
    const staleMs = STALE_AGENT_MS;
    const ids = state.agents.map((agent) => `<option value="${esc(agent.id)}">`).join("");
    const body = `<header>
<h1>Qagent</h1>
<p class="status"><span id="status">${statusLine(state, now, staleMs)}</span> · <span id="live">connecting</span></p>
</header>
<section aria-labelledby="h-agents">
<h2 id="h-agents">Agents</h2>
<table class="agents"><thead><tr><th>Agent</th><th>Last seen</th><th>State</th></tr></thead>
<tbody id="agent-rows">${agentRows(state.agents, now, staleMs)}</tbody></table>
</section>
<section aria-labelledby="h-tasks">
<h2 id="h-tasks">Open tasks</h2>
<table class="tasks"><thead><tr><th>Task</th><th>Title</th><th>Assignee</th><th>Status</th><th>Age</th></tr></thead>
<tbody id="task-rows">${taskRows(state.tasks, now)}</tbody></table>
</section>
<section aria-labelledby="h-messages">
<h2 id="h-messages">Recent messages</h2>
<table class="messages"><thead><tr><th>From</th><th>To</th><th>Message</th><th>Age</th></tr></thead>
<tbody id="message-rows">${messageRows(state.messages, now)}</tbody></table>
</section>
<section aria-labelledby="h-send">
<h2 id="h-send">Send a message</h2>
<form id="send" autocomplete="off">
<label for="send-to">To</label>
<input id="send-to" name="to" list="agent-ids" required maxlength="2000" spellcheck="false" placeholder="agent id, a,b or *">
<datalist id="agent-ids">${ids}</datalist>
<label for="send-text">Text</label>
<textarea id="send-text" name="text" rows="4" required></textarea>
<p class="row"><button type="submit">Send as operator</button> <span id="send-status" class="m" role="status"></span></p>
</form>
</section>
<script type="application/json" id="boot">${scriptJson({ ...state, staleMs })}</script>`;
    return shell(nonce, body, clientScript());
}
export function renderSignedOut(nonce) {
    const body = `<header>
<h1>Qagent</h1>
<p class="status"><span id="live">not signed in</span></p>
</header>
<section>
<p>This dashboard needs a sign-in link. Run <code>qagent dashboard link</code> in a terminal and open the address it prints. Each link works once, for five minutes.</p>
</section>`;
    const script = `(() => {
  const match = /^#t=([A-Za-z0-9_-]+)$/.exec(location.hash);
  if (!match) return;
  history.replaceState(null, "", "/");
  const live = document.getElementById("live");
  live.textContent = "signing in";
  fetch("/session", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ ticket: match[1] }) })
    .then((response) => { if (response.ok) location.replace("/"); else live.textContent = "sign-in link invalid or expired"; })
    .catch(() => { live.textContent = "sign-in failed"; });
})();`;
    return shell(nonce, body, script);
}
//# sourceMappingURL=page.js.map