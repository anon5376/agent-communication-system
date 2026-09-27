/**
 * Shared v2 types. Ported from protocol.ts:76-253 (Message, Task, TaskResult,
 * ContextReference) and trimmed to what the single-file bus stores.
 */
export const TASK_STATES = [
    "open", "blocked", "claimed", "submitted", "changes_requested", "accepted", "failed", "cancelled",
];
export const CLOSED_STATES = ["accepted", "failed", "cancelled"];
export const MESSAGE_TYPES = ["info", "question", "answer", "task", "result", "feedback", "control"];
export const PRIORITIES = ["low", "normal", "high", "urgent"];
export const REFERENCE_TYPES = ["path", "artifact", "summary", "commit", "url"];
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
};
export class BusError extends Error {
    code;
    constructor(code, message) {
        super(message);
        this.code = code;
        this.name = "BusError";
    }
}
export function boundedString(value, label, max, required = false) {
    const text = value === undefined || value === null ? "" : String(value);
    if (required && !text.trim())
        throw new BusError("invalid", `${label} is required`);
    if (text.length > max)
        throw new BusError("invalid", `${label} exceeds ${max} characters`);
    return text;
}
export function contextReferences(value) {
    if (value === undefined || value === null)
        return [];
    if (!Array.isArray(value))
        throw new BusError("invalid", "refs must be a list");
    if (value.length > LIMITS.refCount)
        throw new BusError("invalid", `at most ${LIMITS.refCount} refs are allowed`);
    return value.map((item) => {
        const row = item && typeof item === "object" ? item : { value: String(item) };
        const type = String(row.type ?? "path");
        if (!REFERENCE_TYPES.includes(type))
            throw new BusError("invalid", `invalid reference type: ${type}`);
        const ref = { type, value: boundedString(row.value, "reference value", LIMITS.refValue, true) };
        if (row.description)
            ref.description = boundedString(row.description, "reference description", LIMITS.refDescription);
        if (row.digest)
            ref.digest = boundedString(row.digest, "reference digest", 256);
        return ref;
    });
}
//# sourceMappingURL=types.js.map