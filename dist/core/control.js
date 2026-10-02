/**
 * Pausing agents and giving them budgets. Mirror of rust/src/control.rs.
 *
 * Both live in `agents.meta_json`, so the TypeScript and Rust implementations
 * share them on one bus.db without a schema change:
 *
 *   "paused": {"atMs": 1700000000000, "by": "operator", "reason": "lunch"}
 *   "budget": {"turns": 20, "minutes": 60, "usd": 2.5, "setMs": 1700000000000,
 *              "base": {"turns": 3, "minutes": 1.5, "usd": 0.1}}
 *
 * A budget counts what the agent's supervisor records in `sessions/<agent>.json`
 * since the budget was set (or the agent last resumed): turns, minutes the CLI
 * ran, and the cost the CLI itself reported. A CLI that reports no cost counts
 * as 0 dollars, so only turns and minutes are dependable for every CLI.
 */
import { readFileSync } from "node:fs";
import { join } from "node:path";
const num = (value) => (typeof value === "number" && Number.isFinite(value) ? value : 0);
const round = (x, places) => Math.round(x * 10 ** places) / 10 ** places;
export function usageJson(u) {
    return { turns: u.turns, minutes: round(u.minutes, 2), usd: round(u.usd, 4) };
}
/** The running totals in `<home>/sessions/<agent>.json` (zero when there is none yet). */
export function sessionUsage(home, agentId) {
    let v = {};
    try {
        v = JSON.parse(readFileSync(join(home, "sessions", `${agentId}.json`), "utf8"));
    }
    catch { /* none yet */ }
    return { turns: num(v.turns), minutes: num(v.latencyMs) / 60_000, usd: num(v.costUSD) };
}
export function limitsEmpty(l) {
    return l.turns === undefined && l.minutes === undefined && l.usd === undefined;
}
function obj(value) {
    return value && typeof value === "object" && !Array.isArray(value) ? value : null;
}
export function pausedOf(meta) {
    const p = obj(meta.paused);
    if (!p)
        return null;
    return { atMs: num(p.atMs), by: typeof p.by === "string" ? p.by : "", reason: typeof p.reason === "string" ? p.reason : "" };
}
/** A number to one decimal, without a trailing ".0". */
export function short(x) {
    const r = Math.round(x * 10) / 10;
    return Number.isInteger(r) ? String(r) : r.toFixed(1);
}
/** The agent's budget and what it has used since the budget was set, given its current totals. */
export function budgetOf(meta, now) {
    const b = obj(meta.budget);
    if (!b)
        return null;
    const limits = {};
    for (const key of ["turns", "minutes", "usd"])
        if (typeof b[key] === "number")
            limits[key] = b[key];
    if (limitsEmpty(limits))
        return null;
    const base = obj(b.base) ?? {};
    return {
        limits,
        used: {
            turns: Math.max(0, now.turns - num(base.turns)),
            minutes: Math.max(0, now.minutes - num(base.minutes)),
            usd: Math.max(0, now.usd - num(base.usd)),
        },
        setMs: num(b.setMs),
    };
}
/** The first limit reached, in words ("20 of 20 turns"), or null. */
export function budgetOver(b) {
    const checks = [
        [b.limits.turns, b.used.turns, "turns"],
        [b.limits.minutes, b.used.minutes, "minutes"],
        [b.limits.usd, b.used.usd, "dollars reported"],
    ];
    for (const [limit, used, unit] of checks)
        if (limit !== undefined && used >= limit)
            return `${short(used)} of ${short(limit)} ${unit}`;
    return null;
}
/** "4/20 turns  12/60 min  $0.30/$2.00", only the limits that are set. */
export function budgetLine(b) {
    const parts = [];
    if (b.limits.turns !== undefined)
        parts.push(`${short(b.used.turns)}/${short(b.limits.turns)} turns`);
    if (b.limits.minutes !== undefined)
        parts.push(`${short(b.used.minutes)}/${short(b.limits.minutes)} min`);
    if (b.limits.usd !== undefined)
        parts.push(`$${b.used.usd.toFixed(2)}/$${b.limits.usd.toFixed(2)}`);
    return parts.join("  ");
}
/** The JSON stored for a budget that starts counting from `base`. */
export function budgetJson(limits, base, nowMs) {
    return { setMs: nowMs, base: usageJson(base), ...limits };
}
//# sourceMappingURL=control.js.map