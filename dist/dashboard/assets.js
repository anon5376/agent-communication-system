/**
 * Static strings for the dashboard page: the stylesheet and the client logic.
 * Both are inlined into the one HTML response under a per-response CSP nonce,
 * so there is no build step and no second request.
 *
 * Look (DESIGN.md): flat #181818, one 960 px column, system sans, hairline
 * rules, status as plain words. No cards, badges, rings or tiles.
 */
export const CSS = `
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
.tasks th:nth-child(1), .attention th:nth-child(1) { width: 64px; }
.attention th:nth-child(2) { width: 120px; }
.why { display: block; }
code.next { display: block; margin-top: 4px; color: #dddddd; white-space: pre-wrap; overflow-wrap: anywhere; }
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
`;
/**
 * Client logic. Runs after the shared renderers from page.ts and attention.ts
 * (esc, ago, agentState, attention, agentRows, attentionRows, taskRows,
 * messageRows, statusLine) are defined. The 30 s re-render also re-applies the
 * attention rules, so a claim that goes stale with no write still moves up.
 * One EventSource per tab; the server pushes changes. The only timer is a
 * 30 s local re-render so ages stay current; it makes no request.
 */
export const CLIENT_JS = `
const boot = JSON.parse(document.getElementById("boot").textContent);
const staleMs = boot.staleMs;
const stallMs = boot.stallMs;
const tiers = boot.tiers;
const agents = new Map(boot.agents.map((a) => [a.id, a]));
const tasks = new Map(boot.tasks.map((t) => [t.id, t]));
const messages = new Map(boot.messages.map((m) => [m.seq, m]));
let lastChangeMs = boot.lastChangeMs;
const $ = (id) => document.getElementById(id);

function render() {
  const now = Date.now();
  const agentList = Array.from(agents.values()).sort((a, b) => a.id < b.id ? -1 : 1);
  $("agent-rows").innerHTML = agentRows(agentList, now, staleMs);
  const taskList = Array.from(tasks.values());
  $("attention-rows").innerHTML = attentionRows(taskList, agentList, now, staleMs, stallMs, tiers);
  $("task-rows").innerHTML = taskRows(taskList, agentList, now, staleMs, stallMs, tiers);
  $("message-rows").innerHTML = messageRows(Array.from(messages.values()).sort((a, b) => b.seq - a.seq), now);
  $("status").textContent = "";
  $("status").insertAdjacentHTML("afterbegin", statusLine({ dbPath: boot.dbPath, agents: agentList, tasks: taskList, lastChangeMs: lastChangeMs }, now, staleMs, stallMs, tiers));
  $("agent-ids").innerHTML = agentList.map((a) => '<option value="' + esc(a.id) + '">').join("");
}

function apply(delta) {
  for (const a of delta.agents || []) agents.set(a.id, a);
  if (delta.tasks) { tasks.clear(); for (const t of delta.tasks) tasks.set(t.id, t); }
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
`;
//# sourceMappingURL=assets.js.map