/**
 * Shared v2 types. Ported from protocol.ts:76-253 (Message, Task, TaskResult,
 * ContextReference) and trimmed to what the single-file bus stores.
 */

export type Authority = "operator" | "manager" | "worker";
export type AgentStatus = "idle" | "waiting" | "working" | "offline";
export type TaskState =
  | "open"
  | "blocked"
  | "claimed"
  | "submitted"
  | "changes_requested"
  | "accepted"
  | "failed"
  | "cancelled";
export type MessageType = "info" | "question" | "answer" | "task" | "result" | "feedback" | "control";
export type Priority = "low" | "normal" | "high" | "urgent";
export type ContextReferenceType = "path" | "artifact" | "summary" | "commit" | "url";

export const TASK_STATES: readonly TaskState[] = [
  "open", "blocked", "claimed", "submitted", "changes_requested", "accepted", "failed", "cancelled",
];
export const CLOSED_STATES: readonly TaskState[] = ["accepted", "failed", "cancelled"];
export const MESSAGE_TYPES: readonly MessageType[] = ["info", "question", "answer", "task", "result", "feedback", "control"];
export const PRIORITIES: readonly Priority[] = ["low", "normal", "high", "urgent"];
export const REFERENCE_TYPES: readonly ContextReferenceType[] = ["path", "artifact", "summary", "commit", "url"];

export const OPERATOR_ID = "operator";
/** Claims expire after two hours; the next task write reopens them. */
export const CLAIM_TTL_MS = 2 * 60 * 60_000;
/** An agent not seen for this long shows offline. */
export const STALE_AGENT_MS = 15 * 60_000;
export const DEFAULT_WAIT_SEC = 240;
export const MAX_WAIT_SEC = 3600;

/** Input limits ported from broker.ts:113-176, 735-737, 969-970, 1144. */
export const LIMITS = {
  subject: 1000,
  body: 200_000,
  title: 500,
  brief: 50_000,
  acceptance: 20_000,
  summary: 20_000,
  details: 100_000,
  feedback: 50_000,
  note: 20_000,
  reason: 20_000,
  refValue: 4096,
  refDescription: 2048,
  refCount: 100,
  changedFiles: 500,
  validation: 100,
  thread: 200,
  role: 200,
  model: 200,
  path: 8192,
  inboxLimit: 200,
} as const;

export interface ContextReference {
  type: ContextReferenceType;
  value: string;
  description?: string;
  digest?: string;
}

export interface Agent {
  id: string;
  role: string;
  model: string;
  harness: string;
  parentId: string | null;
  /** Derived: a waiter whose wait_until has passed, or an agent unseen for 15 minutes, is offline. */
  status: AgentStatus;
  storedStatus: string;
  waitUntilMs: number | null;
  lastSeenMs: number | null;
  createdMs: number;
  authority: Authority | null;
  meta: Record<string, unknown>;
}

export interface Message {
  seq: number;
  id: string;
  tsMs: number;
  sender: string;
  /** null means broadcast. */
  recipient: string | null;
  type: MessageType;
  subject: string;
  body: string;
  thread: string;
  taskId: number | null;
  refs: ContextReference[];
  requiresAck: boolean;
  source: string;
}

export interface ValidationObservation {
  command?: string;
  passed: boolean;
  summary: string;
}

export interface TaskResult {
  summary: string;
  details: string;
  changedFiles: string[];
  artifacts: ContextReference[];
  validation: ValidationObservation[];
  completedMs: number;
}

export interface TaskReview {
  reviewer: string;
  accepted: boolean;
  feedback: string;
  reviewedMs: number;
}

/** The columns a board needs, without deps, notes or messages. */
export interface TaskSummary {
  id: number;
  title: string;
  assignee: string | null;
  state: TaskState;
  createdMs: number;
  updatedMs: number;
}

/** The columns a status view needs, without role, authority or meta. */
export interface AgentSummary {
  id: string;
  storedStatus: string;
  waitUntilMs: number | null;
  lastSeenMs: number | null;
}

/** The columns a message-line view needs, without refs, thread or ack flags. */
export interface MessageSummary {
  seq: number;
  tsMs: number;
  sender: string;
  recipient: string | null;
  subject: string;
  body: string;
}

export interface Task {
  id: number;
  legacyId: string | null;
  project: string | null;
  parentId: number | null;
  title: string;
  brief: string;
  acceptance: string;
  role: string;
  priority: string;
  state: TaskState;
  creator: string;
  assignee: string | null;
  reviewer: string | null;
  pathScopes: string[];
  refs: ContextReference[];
  result: TaskResult | null;
  review: TaskReview | null;
  round: number;
  attempts: number;
  maxRetries: number;
  claimExpiresMs: number | null;
  createdMs: number;
  updatedMs: number;
  dependencies: number[];
}

export interface TaskNote {
  id: number;
  taskId: number;
  author: string;
  tsMs: number;
  body: string;
}

export interface TaskDetail extends Task {
  notes: TaskNote[];
  dependents: number[];
  messages: Message[];
  leases: string[];
}

/** One line in a task's causal timeline: an event, a note, or task-bound mail, ordered by time. */
export interface TraceItem {
  seq: number;
  tsMs: number;
  /** Event kind, 'note', or 'mail'. */
  kind: string;
  actor: string;
  /** One-line description for text output. */
  summary: string;
  /** Note/mail body, omitted when empty. */
  body?: string;
  /** For mail: the recipient (null = broadcast). */
  to?: string | null;
  data?: Record<string, unknown>;
}

export interface TaskTrace {
  task: TaskDetail;
  dependencies: number[];
  dependents: number[];
  timeline: TraceItem[];
}

export interface BusEvent {
  seq: number;
  tsMs: number;
  actor: string;
  kind: string;
  entity: string;
  entityId: string;
  data: Record<string, unknown>;
  source: string;
}

export type BusErrorCode = "unauthorized" | "forbidden" | "not_found" | "invalid" | "conflict";

export class BusError extends Error {
  constructor(readonly code: BusErrorCode, message: string) {
    super(message);
    this.name = "BusError";
  }
}

export function boundedString(value: unknown, label: string, max: number, required = false): string {
  const text = value === undefined || value === null ? "" : String(value);
  if (required && !text.trim()) throw new BusError("invalid", `${label} is required`);
  if (text.length > max) throw new BusError("invalid", `${label} exceeds ${max} characters`);
  return text;
}

export function contextReferences(value: unknown): ContextReference[] {
  if (value === undefined || value === null) return [];
  if (!Array.isArray(value)) throw new BusError("invalid", "refs must be a list");
  if (value.length > LIMITS.refCount) throw new BusError("invalid", `at most ${LIMITS.refCount} refs are allowed`);
  return value.map((item) => {
    const row = item && typeof item === "object" ? item as Record<string, unknown> : { value: String(item) };
    const type = String(row.type ?? "path") as ContextReferenceType;
    if (!REFERENCE_TYPES.includes(type)) throw new BusError("invalid", `invalid reference type: ${type}`);
    const ref: ContextReference = { type, value: boundedString(row.value, "reference value", LIMITS.refValue, true) };
    if (row.description) ref.description = boundedString(row.description, "reference description", LIMITS.refDescription);
    if (row.digest) ref.digest = boundedString(row.digest, "reference digest", 256);
    return ref;
  });
}
