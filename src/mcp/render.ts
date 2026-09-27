/**
 * Plain-text replies for the MCP tools. Tables and task detail reuse the CLI
 * formatters; this file adds the wording an agent needs to decide its next call
 * (ported in spirit from mcp-server.ts:65-100).
 */
import type { WaitResult } from "../core/bus.js";
import type { Identity } from "../core/identity.js";
import type { Agent, Message, Task, TaskNote } from "../core/types.js";
import { renderAgents, renderEvent, renderMessages, renderTask, renderTasks } from "../cli/format.js";

export { renderAgents, renderTask, renderTasks };

export function renderWhoami(identity: Identity, who: { agent: Agent | null; unread: number; cursor: number; dbPath: string }, roster: (Agent & { unread: number })[]): string {
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

export function renderSent(messages: Message[]): string {
  const to = messages.map((message) => `${message.recipient ?? "*"} (#${message.seq})`).join(", ");
  return `Sent to ${to}.`;
}

export function renderInbox(result: { messages: Message[]; cursor: number; remaining: number }, peek: boolean): string {
  if (!result.messages.length) return "Inbox is empty.";
  const tail = result.remaining > 0 ? `\n\n${result.remaining} more unread; call bus_inbox again.` : "";
  const head = `${result.messages.length} message(s)${peek ? " (peek; not marked read)" : ""}:`;
  return `${head}\n\n${renderMessages(result.messages)}${tail}`;
}

export function renderWait(result: WaitResult, delivered: { messages: Message[]; remaining: number } | null, seconds: number, cancelled: boolean): string {
  if (delivered && delivered.messages.length) {
    const tail = delivered.remaining > 0 ? `\n\n${delivered.remaining} more unread; call bus_inbox.` : "";
    return `${delivered.messages.length} new message(s):\n\n${renderMessages(delivered.messages)}${tail}`;
  }
  if (result.status === "task") {
    return `Task activity that concerns you:\n${result.events.map(renderEvent).join("\n")}\n\nUse bus_task_get for details.`;
  }
  if (cancelled) return "Wait cancelled.";
  return `Nothing arrived within ${seconds}s. This is normal; call bus_wait again while work is outstanding.`;
}

export function renderTaskLine(task: Task, verb: string): string {
  return `${verb} task #${task.id} [${task.state}, round ${task.round}] ${task.title}${task.assignee ? ` (assignee ${task.assignee})` : ""}`;
}

export function renderNote(note: TaskNote): string {
  return `Noted on task #${note.taskId} (note ${note.id}).`;
}

export function renderError(error: unknown): string {
  const code = error && typeof error === "object" && "code" in error ? String((error as { code: unknown }).code) : "error";
  const message = error instanceof Error ? error.message : String(error);
  return `qagent error (${code}): ${message}`;
}
