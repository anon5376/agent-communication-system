export function fmtAgo(ts, now = Date.now()) {
    if (!ts)
        return "-";
    const seconds = Math.max(0, Math.round((now - ts) / 1000));
    if (seconds < 60)
        return `${seconds}s`;
    if (seconds < 3600)
        return `${Math.round(seconds / 60)}m`;
    if (seconds < 86_400)
        return `${Math.round(seconds / 3600)}h`;
    return `${Math.round(seconds / 86_400)}d`;
}
function clip(value, width) {
    const flat = value.replace(/\s+/g, " ").trim();
    return flat.length > width ? `${flat.slice(0, width - 1)}…` : flat;
}
function table(rows) {
    if (!rows.length)
        return "";
    const widths = rows[0].map((_, column) => Math.max(...rows.map((row) => (row[column] ?? "").length)));
    return rows.map((row) => row.map((cell, column) => column === row.length - 1 ? cell : cell.padEnd(widths[column])).join("  ").trimEnd()).join("\n");
}
export function renderAgents(agents) {
    if (!agents.length)
        return "(no agents)";
    return table([
        ["ID", "ROLE", "STATUS", "UNREAD", "SEEN", "MODEL", "HARNESS"],
        ...agents.map((agent) => [
            agent.id, clip(agent.role, 24), agent.status, String(agent.unread ?? ""), fmtAgo(agent.lastSeenMs), clip(agent.model, 24), agent.harness,
        ]),
    ]);
}
export function renderTasks(tasks) {
    if (!tasks.length)
        return "(no tasks)";
    return table([
        ["#", "STATE", "ROUND", "ASSIGNEE", "CREATOR", "UPDATED", "TITLE"],
        ...tasks.map((task) => [
            String(task.id), task.state, `r${task.round}`, task.assignee ?? "-", task.creator, fmtAgo(task.updatedMs), clip(task.title, 70),
        ]),
    ]);
}
export function renderMessage(message) {
    const to = message.recipient ?? "*";
    const head = `#${message.seq} ${new Date(message.tsMs).toISOString()} ${message.sender} -> ${to} [${message.type}]${message.taskId ? ` task ${message.taskId}` : ""}${message.requiresAck ? " (ack requested)" : ""}`;
    return [head, message.subject ? `  ${message.subject}` : "", message.body ? message.body.split("\n").map((line) => `    ${line}`).join("\n") : ""]
        .filter(Boolean).join("\n");
}
export function renderMessages(messages, empty = "(no new messages)") {
    return messages.length ? messages.map(renderMessage).join("\n\n") : empty;
}
export function renderTask(task) {
    const lines = [
        `Task #${task.id}: ${task.title}`,
        `  state      ${task.state} (round ${task.round}, max retries ${task.maxRetries})`,
        `  creator    ${task.creator}`,
        `  assignee   ${task.assignee ?? "-"}`,
        `  reviewer   ${task.reviewer ?? `${task.creator} (creator)`}`,
        `  priority   ${task.priority}${task.role ? `, role ${task.role}` : ""}`,
    ];
    if (task.legacyId)
        lines.push(`  legacy id  ${task.legacyId}`);
    if (task.project)
        lines.push(`  project    ${task.project}`);
    if (task.pathScopes.length)
        lines.push(`  scopes     ${task.pathScopes.join(", ")}${task.leases.length ? ` (leased: ${task.leases.join(", ")})` : ""}`);
    if (task.parentId)
        lines.push(`  parent     #${task.parentId}`);
    if (task.dependencies.length)
        lines.push(`  depends on ${task.dependencies.map((id) => `#${id}`).join(", ")}`);
    if (task.dependents.length)
        lines.push(`  blocks     ${task.dependents.map((id) => `#${id}`).join(", ")}`);
    if (task.claimExpiresMs)
        lines.push(`  claim ends ${new Date(task.claimExpiresMs).toISOString()}`);
    if (task.brief)
        lines.push("", "Brief:", ...task.brief.split("\n").map((line) => `  ${line}`));
    if (task.acceptance)
        lines.push("", "Acceptance:", ...task.acceptance.split("\n").map((line) => `  ${line}`));
    if (task.result) {
        lines.push("", `Result: ${task.result.summary}`);
        if (task.result.changedFiles.length)
            lines.push(`  files: ${task.result.changedFiles.join(", ")}`);
    }
    if (task.review)
        lines.push("", `Review by ${task.review.reviewer}: ${task.review.accepted ? "accepted" : "changes requested"} - ${task.review.feedback}`);
    if (task.notes.length) {
        lines.push("", "Notes:");
        for (const note of task.notes)
            lines.push(`  ${new Date(note.tsMs).toISOString()} ${note.author}: ${note.body}`);
    }
    if (task.messages.length) {
        lines.push("", "Thread:");
        for (const message of task.messages)
            lines.push(`  #${message.seq} ${message.sender} -> ${message.recipient ?? "*"}: ${clip(message.subject || message.body, 100)}`);
    }
    return lines.join("\n");
}
export function renderTrace(trace) {
    const task = trace.task;
    const lines = [
        `Trace #${task.id}: ${task.title}`,
        `  state ${task.state}  assignee ${task.assignee ?? "-"}  creator ${task.creator}  round ${task.round}`,
    ];
    if (task.parentId !== null)
        lines.push(`  parent #${task.parentId}`);
    if (trace.dependencies.length)
        lines.push(`  depends on ${trace.dependencies.map((id) => `#${id}`).join(", ")}`);
    if (trace.dependents.length)
        lines.push(`  unblocks ${trace.dependents.map((id) => `#${id}`).join(", ")}`);
    lines.push("");
    for (const item of trace.timeline) {
        const time = new Date(item.tsMs).toISOString().slice(5, 19).replace("T", " ");
        const route = item.to !== undefined ? ` -> ${item.to ?? "*"}` : "";
        lines.push(`  ${time} ${item.actor}${route} ${item.kind}${item.summary ? ` — ${clip(item.summary, 100)}` : ""}`);
    }
    if (!trace.timeline.length)
        lines.push("  (empty: the task exists but no events, notes, or mail yet)");
    return lines.join("\n");
}
function esc(value) {
    return value.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
}
export function renderTraceHtml(trace) {
    const task = trace.task;
    const items = trace.timeline.map((item) => {
        const time = new Date(item.tsMs).toISOString();
        const route = item.to !== undefined ? ` → ${esc(item.to ?? "broadcast")}` : "";
        const body = item.body ? `<pre>${esc(item.body)}</pre>` : "";
        return `    <li class="${esc(item.kind)}"><time>${time}</time><b>${esc(item.actor)}${route}</b> <span class="kind">${esc(item.kind)}</span><p>${esc(item.summary)}</p>${body}</li>`;
    }).join("\n");
    return `<!doctype html><html><head><meta charset="utf-8"><title>ACS trace #${task.id} — ${esc(task.title)}</title>
<style>body{font:14px/1.5 system-ui,sans-serif;max-width:860px;margin:32px auto;padding:0 16px;color:#1a1a1a}
h1{font-size:20px}.meta{color:#555;margin-bottom:20px}code{background:#f0f0f0;padding:1px 4px}
ol{list-style:none;padding:0;border-left:3px solid #ddd}li{position:relative;padding:8px 0 8px 20px;border-bottom:1px solid #eee}
li::before{content:"";position:absolute;left:-6.5px;top:14px;width:10px;height:10px;border-radius:50%;background:#999}
li.task_claimed::before,li.note::before{background:#2563eb}li.mail::before{background:#059669}li.task_accepted::before{background:#16a34a}li.task_failed::before,li.task_cancelled::before{background:#dc2626}
time{color:#888;font-size:12px;display:block}.kind{font-size:12px;color:#555;background:#f0f0f0;padding:1px 6px;border-radius:8px}p{margin:4px 0 0}pre{background:#f7f7f7;padding:8px;overflow-x:auto;font-size:12px;white-space:pre-wrap}</style>
</head><body>
<h1>Trace #${task.id}: ${esc(task.title)}</h1>
<div class="meta">state <code>${esc(task.state)}</code> · assignee <code>${esc(task.assignee ?? "-")}</code> · creator <code>${esc(task.creator)}</code> · round ${task.round}
${task.parentId !== null ? ` · parent #${task.parentId}` : ""}${trace.dependencies.length ? ` · depends on ${trace.dependencies.map((id) => `#${id}`).join(", ")}` : ""}${trace.dependents.length ? ` · unblocks ${trace.dependents.map((id) => `#${id}`).join(", ")}` : ""}
<br>${trace.timeline.length} items · exported ${new Date().toISOString()}</div>
<ol>
${items}
</ol>
</body></html>`;
}
export function renderEvent(event) {
    const data = Object.keys(event.data).length ? ` ${clip(JSON.stringify(event.data), 140)}` : "";
    const source = event.source === "v2" ? "" : ` (${event.source})`;
    return `${event.seq} ${new Date(event.tsMs).toISOString()} ${event.actor} ${event.kind} ${event.entity}:${event.entityId}${source}${data}`;
}
export function renderStatus(status) {
    const online = status.agents.filter((agent) => agent.status !== "offline").length;
    const counts = Object.entries(status.counts).map(([state, n]) => `${state} ${n}`).join(", ") || "none";
    return [
        `bus ${status.dbPath}  seq ${status.seq}  agents ${status.agents.length} (${online} online)  tasks: ${counts}`,
        "",
        "AGENTS",
        renderAgents(status.agents),
        "",
        "OPEN TASKS",
        renderTasks(status.openTasks),
    ].join("\n");
}
function counts(record) {
    const entries = Object.entries(record).filter(([, n]) => n > 0);
    return entries.length ? entries.map(([key, n]) => `${key} ${n}`).join(", ") : "none";
}
export function renderImport(report) {
    const lines = [`${report.dryRun ? "Dry run: nothing written. " : ""}Import into ${report.dbPath}`];
    if (!report.sources.length)
        lines.push("  (no sources found)");
    for (const source of report.sources) {
        lines.push("", `${source.kind}  ${source.path}`, `  sha256     ${source.sha256}${source.alreadyImported ? "  (already imported; use --force to recheck)" : ""}`, `  read       ${counts(source.read)}`, `  ${report.dryRun ? "would add " : "added     "} ${counts(source.inserted)}`, `  existing   ${counts(source.duplicates)}`, `  invalid    ${counts(source.invalid)}`);
    }
    lines.push("", report.cursorSeq ? `Cursors raised to message #${report.cursorSeq}.` : "Cursors unchanged.");
    return lines.join("\n");
}
//# sourceMappingURL=format.js.map