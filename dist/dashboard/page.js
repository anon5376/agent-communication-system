/**
 * The dashboard's one page, rendered on the server.
 *
 * The row renderers below are plain, self-contained functions. The server calls
 * them to render the first paint, and assets.ts inlines their source text into
 * the page script so the browser re-renders rows with the same code when an SSE
 * change arrives. Keep them free of imports and closures for that reason.
 */
import { STALE_AGENT_MS } from "../core/types.js";
import { agentState, ago, ATTENTION_TIERS, attention, STALL_MS } from "../attention.js";
import { CLIENT_JS, CSS } from "./assets.js";
export { agentState, ago } from "../attention.js";
export function esc(value) {
    return String(value ?? "").replace(/[&<>"']/g, (c) => c === "&" ? "&amp;" : c === "<" ? "&lt;" : c === ">" ? "&gt;" : c === '"' ? "&quot;" : "&#39;");
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
/** Tasks that need the operator, most urgent first: needs review, then failed or blocked, then stalled. */
export function attentionRows(tasks, agents, now, staleMs, stallMs, tiers) {
    const items = tasks.map((task) => ({ task, a: attention(task, agents, now, staleMs, stallMs) }))
        .filter((item) => item.a.tier < tiers)
        .sort((x, y) => x.a.tier - y.a.tier || x.task.id - y.task.id);
    if (!items.length)
        return '<tr><td class="none wide">Nothing needs you. Nothing is waiting for review, failed, blocked or stalled.</td></tr>';
    return items.map((item) => '<tr><td class="id">#' + esc(item.task.id) + '</td><td>' + esc(item.a.label) + '</td><td class="wide">' + esc(item.task.title) +
        '<span class="why">' + esc(item.a.reason) + '</span><span class="why m">' + esc(item.a.evidence) + '</span><code class="next">' + esc(item.a.next) + "</code></td></tr>").join("");
}
/** Work going on without the operator: active first, then the queue. */
export function taskRows(tasks, agents, now, staleMs, stallMs, tiers) {
    const items = tasks.map((task) => ({ task, a: attention(task, agents, now, staleMs, stallMs) }))
        .filter((item) => item.a.tier >= tiers)
        .sort((x, y) => x.a.tier - y.a.tier || x.task.id - y.task.id);
    if (!items.length)
        return '<tr><td class="none wide">No active or queued tasks.</td></tr>';
    return items.map((item) => '<tr><td class="id">#' + esc(item.task.id) + '</td><td class="wide">' + esc(item.task.title) +
        '<span class="why m">' + esc(item.a.reason) + '</span></td><td class="' + (item.task.assignee ? "" : "m") + '">' + esc(item.task.assignee || "unassigned") +
        "</td><td>" + esc(item.a.label) + '</td><td class="m">' + esc(ago(item.task.updatedMs, now)) + "</td></tr>").join("");
}
export function messageRows(messages, now) {
    if (!messages.length)
        return '<tr><td class="none wide">No messages yet.</td></tr>';
    return messages.map((message) => '<tr><td class="id">' + esc(message.sender) + '</td><td class="id">' + esc(message.recipient || "everyone") +
        '</td><td class="wide">' + esc(message.line) + '</td><td class="m">' + esc(ago(message.tsMs, now)) + "</td></tr>").join("");
}
export function statusLine(state, now, staleMs, stallMs, tiers) {
    const online = state.agents.filter((agent) => agentState(agent, now, staleMs) !== "offline").length;
    const need = state.tasks.filter((task) => attention(task, state.agents, now, staleMs, stallMs).tier < tiers).length;
    return esc(state.dbPath) + " · " + (need ? need + (need === 1 ? " task needs" : " tasks need") + " you" : "nothing needs you") + " · " +
        online + " of " + state.agents.length + " agents online · last change " + esc(ago(state.lastChangeMs, now));
}
/** The page script: the shared renderers' source text, then the client logic from assets.ts. */
function clientScript() {
    const shared = [esc, ago, agentState, attention, agentRows, attentionRows, taskRows, messageRows, statusLine].map((fn) => fn.toString()).join("\n");
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
    const stallMs = STALL_MS;
    const tiers = ATTENTION_TIERS;
    const ids = state.agents.map((agent) => `<option value="${esc(agent.id)}">`).join("");
    const body = `<header>
<h1>Qagent</h1>
<p class="status"><span id="status">${statusLine(state, now, staleMs, stallMs, tiers)}</span> · <span id="live">connecting</span></p>
</header>
<section aria-labelledby="h-attention">
<h2 id="h-attention">Needs you</h2>
<table class="attention"><thead><tr><th>Task</th><th>Status</th><th>Why, evidence and the next command</th></tr></thead>
<tbody id="attention-rows">${attentionRows(state.tasks, state.agents, now, staleMs, stallMs, tiers)}</tbody></table>
</section>
<section aria-labelledby="h-tasks">
<h2 id="h-tasks">Active and queued</h2>
<table class="tasks"><thead><tr><th>Task</th><th>Title</th><th>Assignee</th><th>Status</th><th>Changed</th></tr></thead>
<tbody id="task-rows">${taskRows(state.tasks, state.agents, now, staleMs, stallMs, tiers)}</tbody></table>
</section>
<section aria-labelledby="h-agents">
<h2 id="h-agents">Agents</h2>
<table class="agents"><thead><tr><th>Agent</th><th>Last seen</th><th>State</th></tr></thead>
<tbody id="agent-rows">${agentRows(state.agents, now, staleMs)}</tbody></table>
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
<script type="application/json" id="boot">${scriptJson({ ...state, staleMs, stallMs, tiers })}</script>`;
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