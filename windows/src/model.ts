// Model layer for the ACS Windows app. Mirrors the Swift record types in
// macos/Sources/ACSCore/Models.swift: the helper may send more fields, which
// are ignored, and integer fields keep i64 precision end to end by travelling
// as lossless-json LosslessNumbers (never through JS doubles).

import { LosslessNumber, isLosslessNumber } from "lossless-json";

export type { LosslessNumber };

/** An i64 exactly as the helper sent it. String(v) yields its digits. */
export type Id = LosslessNumber;

export function idKey(id: Id | string | number): string {
  return String(id);
}

export function idEq(a: Id | string | number, b: Id | string | number): boolean {
  return String(a) === String(b);
}

export function msToDate(ms: Id | number): Date {
  return new Date(Number(ms));
}

export function timestamp(ms: Id | number): string {
  const date = msToDate(ms);
  return date.toLocaleString(undefined, {
    month: "short",
    day: "numeric",
    year: "numeric",
    hour: "numeric",
    minute: "2-digit",
  });
}

export function timestampTime(ms: Id | number): string {
  return msToDate(ms).toLocaleTimeString(undefined, {
    hour: "numeric",
    minute: "2-digit",
    second: "2-digit",
  });
}

export interface Snapshot {
  dbPath: string;
  simulated: boolean;
  canOperate: boolean;
  agents: AgentRecord[];
  tasks: TaskRecord[];
  messages: MessageRecord[];
  truncated: boolean;
}

export interface AgentRecord {
  id: string;
  role: string;
  model: string;
  harness: string;
  status: string;
  lastSeenMs?: Id | null;
  running: boolean;
  paused: boolean;
}

export interface ValidationObservation {
  command?: string;
  passed?: boolean | null;
  summary: string;
}

/// A file, link, or other reference a worker attached to its submission.
export interface ContextReference {
  type: string;
  value: string;
  description?: string;
  digest?: string;
}

export interface TaskResult {
  summary: string;
  details: string;
  changedFiles: string[];
  artifacts: ContextReference[];
  validation: ValidationObservation[];
}

export interface TaskReview {
  reviewer: string;
  accepted: boolean;
  feedback: string;
  reviewedMs: Id;
}

export interface TaskRecord {
  id: Id;
  title: string;
  brief: string;
  acceptance: string;
  state: string;
  priority: string;
  assignee?: string | null;
  reviewer?: string | null;
  project?: string | null;
  pathScopes: string[];
  dependencies: Id[];
  updatedMs: Id;
  createdMs: Id;
  result?: TaskResult | null;
  review?: TaskReview | null;
}

export interface MessageRecord {
  seq: Id;
  tsMs: Id;
  sender: string;
  recipient?: string | null;
  subject: string;
  body: string;
}

export interface TaskNote {
  id: Id;
  author: string;
  tsMs: Id;
  body: string;
}

/** Helper `task` reply: TaskRecord fields flattened alongside notes/messages. */
export interface TaskDetail extends TaskRecord {
  notes: TaskNote[];
  messages: MessageRecord[];
}

export interface ProviderRecord {
  id: string;
  name: string;
  path?: string | null;
  signIn: string;
  requiresApproval: boolean;
}

export interface Acknowledgement {
  message: string;
}

export const OPERATOR_ID = "operator";

// ---------------------------------------------------------------------------
// Parsing: trust the required security-signalling fields, tolerate the rest.
// A snapshot that omits simulated/canOperate/truncated must not silently
// decode as a normal workspace (same rule as the Swift decoder).
// ---------------------------------------------------------------------------

function asString(value: unknown, fallback = ""): string {
  return typeof value === "string" ? value : fallback;
}

function asOptString(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}

function asBool(value: unknown, fallback = false): boolean {
  return typeof value === "boolean" ? value : fallback;
}

function asId(value: unknown): Id {
  if (isLosslessNumber(value)) return value;
  if (typeof value === "number" && Number.isSafeInteger(value)) {
    return new LosslessNumber(String(value));
  }
  // Keep exact digits when the wire sent a quoted integer, and fall back to 0
  // rather than throwing away the whole record on an unexpected shape.
  if (typeof value === "string" && /^-?\d+$/.test(value)) {
    return new LosslessNumber(value);
  }
  return new LosslessNumber("0");
}

function asIdList(value: unknown): Id[] {
  return Array.isArray(value) ? value.map(asId) : [];
}

function asStringList(value: unknown): string[] {
  return Array.isArray(value) ? value.filter((v): v is string => typeof v === "string") : [];
}

function requiredBool(obj: Record<string, unknown>, key: string): boolean {
  const value = obj[key];
  if (typeof value !== "boolean") {
    throw new Error(`snapshot is missing required field "${key}"`);
  }
  return value;
}

export function parseSnapshot(value: unknown): Snapshot {
  const obj = value as Record<string, unknown> | null;
  if (!obj || typeof obj !== "object") throw new Error("snapshot was not an object");
  return {
    dbPath: asString(obj.dbPath),
    simulated: requiredBool(obj, "simulated"),
    canOperate: requiredBool(obj, "canOperate"),
    agents: Array.isArray(obj.agents) ? obj.agents.map(parseAgent) : [],
    tasks: Array.isArray(obj.tasks) ? obj.tasks.map(parseTask) : [],
    messages: Array.isArray(obj.messages) ? obj.messages.map(parseMessage) : [],
    truncated: requiredBool(obj, "truncated"),
  };
}

export function parseAgent(value: unknown): AgentRecord {
  const obj = (value ?? {}) as Record<string, unknown>;
  return {
    id: asString(obj.id),
    role: asString(obj.role),
    model: asString(obj.model),
    harness: asString(obj.harness),
    status: asString(obj.status, "unknown"),
    lastSeenMs: obj.lastSeenMs == null ? null : asId(obj.lastSeenMs),
    running: asBool(obj.running),
    paused: asBool(obj.paused),
  };
}

export function parseTask(value: unknown): TaskRecord {
  const obj = (value ?? {}) as Record<string, unknown>;
  return {
    id: asId(obj.id),
    title: asString(obj.title),
    brief: asString(obj.brief),
    acceptance: asString(obj.acceptance),
    state: asString(obj.state),
    priority: asString(obj.priority, "normal"),
    assignee: asOptString(obj.assignee),
    reviewer: asOptString(obj.reviewer),
    project: asOptString(obj.project),
    pathScopes: asStringList(obj.pathScopes),
    dependencies: asIdList(obj.dependencies),
    updatedMs: asId(obj.updatedMs),
    createdMs: obj.createdMs == null ? asId(obj.updatedMs) : asId(obj.createdMs),
    result: parseResult(obj.result),
    review: parseReview(obj.review),
  };
}

function parseResult(value: unknown): TaskResult | null {
  if (!value || typeof value !== "object") return null;
  const obj = value as Record<string, unknown>;
  return {
    summary: asString(obj.summary),
    details: asString(obj.details),
    changedFiles: asStringList(obj.changedFiles),
    artifacts: Array.isArray(obj.artifacts)
      ? obj.artifacts.map((item) => {
          const a = (item ?? {}) as Record<string, unknown>;
          return {
            type: asString(a.type),
            value: asString(a.value),
            description: asOptString(a.description) ?? undefined,
            digest: asOptString(a.digest) ?? undefined,
          };
        })
      : [],
    validation: Array.isArray(obj.validation)
      ? obj.validation.map((check) => {
          const c = (check ?? {}) as Record<string, unknown>;
          return {
            command: asOptString(c.command) ?? undefined,
            passed: typeof c.passed === "boolean" ? c.passed : null,
            summary: asString(c.summary),
          };
        })
      : [],
  };
}

function parseReview(value: unknown): TaskReview | null {
  if (!value || typeof value !== "object") return null;
  const obj = value as Record<string, unknown>;
  return {
    reviewer: asString(obj.reviewer),
    accepted: asBool(obj.accepted),
    feedback: asString(obj.feedback),
    reviewedMs: asId(obj.reviewedMs),
  };
}

export function parseMessage(value: unknown): MessageRecord {
  const obj = (value ?? {}) as Record<string, unknown>;
  return {
    seq: asId(obj.seq),
    tsMs: asId(obj.tsMs),
    sender: asString(obj.sender),
    recipient: asOptString(obj.recipient),
    subject: asString(obj.subject),
    body: asString(obj.body),
  };
}

export function parseTaskDetail(value: unknown): TaskDetail {
  const task = parseTask(value);
  const obj = (value ?? {}) as Record<string, unknown>;
  const notes: TaskNote[] = Array.isArray(obj.notes)
    ? obj.notes.map((note) => {
        const n = (note ?? {}) as Record<string, unknown>;
        return {
          id: asId(n.id),
          author: asString(n.author),
          tsMs: asId(n.tsMs),
          body: asString(n.body),
        };
      })
    : [];
  const messages = Array.isArray(obj.messages) ? obj.messages.map(parseMessage) : [];
  return { ...task, notes, messages };
}

export function parseProviders(value: unknown): ProviderRecord[] {
  if (!Array.isArray(value)) return [];
  return value.map((item) => {
    const obj = (item ?? {}) as Record<string, unknown>;
    return {
      id: asString(obj.id),
      name: asString(obj.name),
      path: asOptString(obj.path),
      signIn: asString(obj.signIn),
      requiresApproval: asBool(obj.requiresApproval),
    };
  });
}

// ---------------------------------------------------------------------------
// Task display semantics — the same plain-language mapping as ACSApp.swift.
// ---------------------------------------------------------------------------

export function stateTitle(state: string): string {
  switch (state) {
    case "open": return "Queued";
    case "claimed": return "Claimed";
    case "submitted": return "Needs review";
    case "changes_requested": return "Changes requested";
    case "blocked": return "Blocked";
    case "accepted": return "Accepted";
    case "cancelled": return "Cancelled";
    case "failed": return "Failed";
    default: {
      const titled = state.replace(/_/g, " ");
      return titled.charAt(0).toUpperCase() + titled.slice(1);
    }
  }
}

export type StateTone = "ok" | "bad" | "warn" | "accent" | "muted";

export function stateTone(state: string): StateTone {
  switch (state) {
    case "accepted": return "ok";
    case "failed": return "bad";
    case "submitted":
    case "blocked":
    case "changes_requested":
      return "warn";
    case "claimed": return "accent";
    default: return "muted";
  }
}

export function stateIcon(state: string): string {
  switch (state) {
    case "claimed": return "circle-dotted";
    case "submitted": return "tray-full";
    case "accepted": return "checkmark-circle";
    case "failed":
    case "blocked":
      return "exclamation-circle";
    case "changes_requested": return "arrow-uturn";
    case "cancelled": return "slash-circle";
    default: return "circle";
  }
}

export function taskExplanation(task: TaskRecord): string {
  switch (task.state) {
    case "open":
      return `Waiting for ${task.assignee ?? "an eligible worker"} to claim this task.`;
    case "claimed":
      return `Claimed by ${task.assignee ?? "a worker"}. Check recent messages for progress.`;
    case "submitted":
      return `Work submitted. Waiting for ${task.reviewer ?? "the task creator or operator"} to review it.`;
    case "changes_requested":
      return "The reviewer requested changes. The task is not accepted yet.";
    case "blocked":
      return task.dependencies.length === 0
        ? "This task is blocked. Check its notes and messages."
        : `Waiting on dependencies: ${task.dependencies.map((d) => `#${String(d)}`).join(", ")}.`;
    case "failed":
      return "This task failed. Read its submission and messages before deciding what to do next.";
    case "accepted":
      return "The submitted work was accepted by its reviewer.";
    case "cancelled":
      return "This task was cancelled. Its history is preserved.";
    default:
      return `State reported by the ACS bus: ${task.state}.`;
  }
}

export function isClosed(state: string): boolean {
  return state === "accepted" || state === "failed" || state === "cancelled";
}

/** TaskBrowser's filter + sort: pending review first, then newest updated. */
export function filterTasks(
  tasks: TaskRecord[],
  opts: { reviewOnly: boolean; includeClosed: boolean; query: string },
): TaskRecord[] {
  const q = opts.query.trim().toLowerCase();
  return tasks
    .filter((task) => {
      if (opts.reviewOnly ? task.state !== "submitted" : !opts.includeClosed && isClosed(task.state)) {
        return false;
      }
      if (!q) return true;
      const hay = `${String(task.id)} ${task.title} ${task.assignee ?? ""}`.toLowerCase();
      return hay.includes(q);
    })
    .sort((a, b) => {
      const aSubmitted = a.state === "submitted";
      const bSubmitted = b.state === "submitted";
      if (aSubmitted !== bSubmitted) return aSubmitted ? -1 : 1;
      return Number(b.updatedMs) - Number(a.updatedMs);
    });
}

// ---------------------------------------------------------------------------
// Grouping, progress and message tags for the task list and inspector.
// ---------------------------------------------------------------------------

export type TaskGroup = "review" | "stuck" | "active" | "queued" | "done";

export const TASK_GROUPS: { id: TaskGroup; title: string; hint: string }[] = [
  { id: "review", title: "Needs review", hint: "Submitted work waiting for a decision" },
  { id: "stuck", title: "Stuck", hint: "Blocked or failed" },
  { id: "active", title: "In progress", hint: "An agent is working on it" },
  { id: "queued", title: "Queued", hint: "Waiting for a worker" },
  { id: "done", title: "Finished", hint: "Accepted or cancelled" },
];

export function taskGroup(state: string): TaskGroup {
  switch (state) {
    case "submitted": return "review";
    case "blocked":
    case "failed":
      return "stuck";
    case "claimed":
    case "changes_requested":
      return "active";
    case "accepted":
    case "cancelled":
      return "done";
    default: return "queued";
  }
}

export function groupTasks(tasks: TaskRecord[]): { group: (typeof TASK_GROUPS)[number]; tasks: TaskRecord[] }[] {
  return TASK_GROUPS.map((group) => ({ group, tasks: tasks.filter((t) => taskGroup(t.state) === group.id) })).filter(
    (g) => g.tasks.length > 0,
  );
}

/** The state label a row needs only when its group header doesn't already say it. */
export function rowStateLabel(state: string): string | null {
  switch (state) {
    case "changes_requested":
    case "failed":
    case "cancelled":
      return stateTitle(state);
    default:
      return null;
  }
}

export const TASK_STAGES = ["Created", "Claimed", "Submitted", "Reviewed"] as const;

/** Index of the last reached stage, and whether the task ended off the happy path. */
export function taskStage(task: TaskRecord): { reached: number; outcome: "ok" | "warn" | "bad" | null } {
  switch (task.state) {
    case "open": return { reached: 0, outcome: null };
    case "blocked": return { reached: task.assignee ? 1 : 0, outcome: "warn" };
    case "claimed": return { reached: 1, outcome: null };
    case "changes_requested": return { reached: 1, outcome: "warn" };
    case "submitted": return { reached: 2, outcome: null };
    case "accepted": return { reached: 3, outcome: "ok" };
    case "failed": return { reached: task.result ? 2 : 1, outcome: "bad" };
    case "cancelled": return { reached: task.result ? 2 : task.assignee ? 1 : 0, outcome: "bad" };
    default: return { reached: 0, outcome: null };
  }
}

export function relativeTime(ms: Id | number, now: number = Date.now()): string {
  const diff = Math.max(0, now - Number(ms));
  const min = Math.floor(diff / 60_000);
  if (min < 1) return "just now";
  if (min < 60) return `${min} min ago`;
  const hours = Math.floor(min / 60);
  if (hours < 24) return `${hours} h ago`;
  const days = Math.floor(hours / 24);
  if (days < 7) return days === 1 ? "yesterday" : `${days} days ago`;
  return msToDate(ms).toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

const TAG_LABELS: Record<string, { label: string; tone: StateTone }> = {
  DONE: { label: "Submitted", tone: "warn" },
  CHANGES: { label: "Changes requested", tone: "warn" },
  ACCEPTED: { label: "Accepted", tone: "ok" },
  FAILED: { label: "Failed", tone: "bad" },
  BLOCKED: { label: "Blocked", tone: "warn" },
  TASK: { label: "New task", tone: "accent" },
};

/** "[DONE #7 r1] title" -> { label: "Submitted", taskId: "7", round: 1, rest: "title" }. */
export function parseSubjectTag(
  subject: string,
): { label: string; tone: StateTone; taskId: string; round: number | null; rest: string } | null {
  const m = /^\[([A-Z]+) #(\d+)(?: r(\d+))?\]\s*(.*)$/s.exec(subject);
  if (!m) return null;
  const [, kind = "", taskId = "", round, rest = ""] = m;
  const known = TAG_LABELS[kind];
  const label = known?.label ?? kind.charAt(0) + kind.slice(1).toLowerCase();
  return { label, tone: known?.tone ?? "muted", taskId, round: round ? Number(round) : null, rest };
}

// ---------------------------------------------------------------------------
// Orchestration (aos): missions, role prompts, crew and goals.
// ---------------------------------------------------------------------------

export interface MissionRecord {
  name: string;
  summary: string;
  brief: string;
  acceptance: string;
  text: string;
  custom: boolean;
}

export interface RolePrompt {
  name: string;
  text: string;
  custom: boolean;
}

export interface CrewMember {
  id: string;
  role: string;
  authority: string;
  cli: string;
  description: string;
  enabled: boolean;
  instructions: string | null;
  running: boolean;
}

export interface GoalRecord {
  id: Id;
  title: string;
  state: string;
  assignee: string | null;
  reviewer: string | null;
  updatedMs: Id;
}

export interface Orchestration {
  simulated: boolean;
  configured: boolean;
  crewError: string | null;
  crewDir: string;
  workdir: string | null;
  goalOwner: string | null;
  missions: MissionRecord[];
  roles: RolePrompt[];
  crew: CrewMember[];
  goals: GoalRecord[];
}

function asObj(value: unknown): Record<string, unknown> {
  return (value && typeof value === "object" ? value : {}) as Record<string, unknown>;
}

export function parseOrchestration(value: unknown): Orchestration {
  const obj = asObj(value);
  const list = (v: unknown) => (Array.isArray(v) ? v.map(asObj) : []);
  return {
    simulated: asBool(obj.simulated),
    configured: asBool(obj.configured),
    crewError: asOptString(obj.crewError),
    crewDir: asString(obj.crewDir),
    workdir: asOptString(obj.workdir),
    goalOwner: asOptString(obj.goalOwner),
    missions: list(obj.missions).map((m) => ({
      name: asString(m.name),
      summary: asString(m.summary),
      brief: asString(m.brief),
      acceptance: asString(m.acceptance),
      text: asString(m.text),
      custom: asBool(m.custom),
    })),
    roles: list(obj.roles).map((r) => ({
      name: asString(r.name),
      text: asString(r.text),
      custom: asBool(r.custom),
    })),
    crew: list(obj.crew).map((c) => ({
      id: asString(c.id),
      role: asString(c.role),
      authority: asString(c.authority),
      cli: asString(c.cli),
      description: asString(c.description),
      enabled: asBool(c.enabled),
      instructions: asOptString(c.instructions),
      running: asBool(c.running),
    })),
    goals: list(obj.goals).map((g) => ({
      id: asId(g.id),
      title: asString(g.title),
      state: asString(g.state),
      assignee: asOptString(g.assignee),
      reviewer: asOptString(g.reviewer),
      updatedMs: asId(g.updatedMs),
    })),
  };
}

/** Mirror of crew::expand: the title, brief and acceptance a goal becomes. */
export function expandMission(m: MissionRecord, goal: string): { title: string; brief: string; acceptance: string } {
  let title = m.name === "run" ? goal : `${m.name} ${goal}`;
  const chars = [...title];
  if (chars.length > 120) title = `${chars.slice(0, 117).join("")}...`;
  return {
    title,
    brief: m.brief.split("{goal}").join(goal),
    acceptance: m.acceptance.split("{goal}").join(goal),
  };
}
