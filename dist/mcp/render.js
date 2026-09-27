import { renderAgents, renderEvent, renderMessages, renderTask, renderTasks } from "../cli/format.js";
export { renderAgents, renderTask, renderTasks };
export function renderWhoami(identity, who, roster) {
    const agent = who.agent;
    return [
        `You are ${identity.agentId} (${identity.authority}${agent?.role ? `, role ${agent.role}` : ""}${agent?.model ? `, model ${agent.model}` : ""}).`,
        `Unread: ${who.unread}. Read cursor: ${who.cursor}. Bus: ${who.dbPath}.`,
        `May delegate: ${identity.permissions.canDelegate ? "yes" : "no"}. May review: ${identity.permissions.canReview ? "yes" : "no"}.`,
        "",
        "Agents:",
        renderAgents(roster),
    ].join("\n");
}
export function renderSent(messages) {
    const to = messages.map((message) => `${message.recipient ?? "*"} (#${message.seq})`).join(", ");
    return `Sent to ${to}.`;
}
export function renderInbox(result, peek) {
    if (!result.messages.length)
        return "Inbox is empty.";
    const tail = result.remaining > 0 ? `\n\n${result.remaining} more unread; call bus_inbox again.` : "";
    const head = `${result.messages.length} message(s)${peek ? " (peek; not marked read)" : ""}:`;
    return `${head}\n\n${renderMessages(result.messages)}${tail}`;
}
export function renderWait(result, delivered, seconds, cancelled) {
    if (delivered && delivered.messages.length) {
        const tail = delivered.remaining > 0 ? `\n\n${delivered.remaining} more unread; call bus_inbox.` : "";
        return `${delivered.messages.length} new message(s):\n\n${renderMessages(delivered.messages)}${tail}`;
    }
    if (result.status === "task") {
        return `Task activity that concerns you:\n${result.events.map(renderEvent).join("\n")}\n\nUse bus_task_get for details.`;
    }
    if (cancelled)
        return "Wait cancelled.";
    return `Nothing arrived within ${seconds}s. This is normal; call bus_wait again while work is outstanding.`;
}
export function renderTaskLine(task, verb) {
    return `${verb} task #${task.id} [${task.state}, round ${task.round}] ${task.title}${task.assignee ? ` (assignee ${task.assignee})` : ""}`;
}
export function renderNote(note) {
    return `Noted on task #${note.taskId} (note ${note.id}).`;
}
export function renderError(error) {
    const code = error && typeof error === "object" && "code" in error ? String(error.code) : "error";
    const message = error instanceof Error ? error.message : String(error);
    return `qagent error (${code}): ${message}`;
}
//# sourceMappingURL=render.js.map