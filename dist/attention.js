/** A claim with no task activity for this long, from an assignee not seen for as long, is stalled. Matches `qagent task stalled`. */
export const STALL_MS = 60 * 60_000;
/** Failed tasks stay on the attention list for a day. */
export const RECENT_FAILURE_MS = 24 * 60 * 60_000;
export const TIERS = ["needs review", "failed or blocked", "stalled", "active", "queued"];
/** Tiers 0-2 need the operator; 3 and 4 are work going on without them. */
export const ATTENTION_TIERS = 3;
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
export function attention(task, agents, now, staleMs, stallMs) {
    const n = task.id;
    const agentOf = (id) => agents.find((agent) => agent.id === id) || null;
    const online = (id) => { const agent = agentOf(id); return agent !== null && agentState(agent, now, staleMs) !== "offline"; };
    const seen = (id) => { const agent = agentOf(id); return id + " last seen " + (agent ? ago(agent.lastSeenMs, now) : "never"); };
    const detail = task.detail ? ' "' + task.detail + '"' : "";
    const join = (...parts) => parts.filter(Boolean).join(" · ");
    const asOperator = " --as operator";
    const item = (tier, label, reason, evidence, next) => ({ tier, label, reason, evidence, next });
    if (task.state === "accepted" || task.state === "cancelled") {
        return item(5, task.state, "Closed; nothing left to do.", task.state + " " + ago(task.updatedMs, now), "");
    }
    if (task.state === "submitted") {
        const reviewer = task.reviewer || task.creator;
        const waiting = reviewer === "operator" ? "you" : reviewer + (online(reviewer) ? "" : ", who is offline");
        return item(0, "needs review", "Submitted by " + (task.assignee || "its worker") + "; waiting for review by " + waiting + ". Read it with qagent task show " + n + "; --revise sends it back.", join("submitted " + ago(task.updatedMs, now), task.detail ? "summary:" + detail : "", "round " + task.round), "qagent task review " + n + " --accept --feedback \"...\"" + asOperator);
    }
    if (task.state === "failed") {
        const rejected = task.attempts <= task.maxRetries;
        return item(1, "failed", rejected ? "Rejected in review on round " + task.round + "; no rounds left." : "Failed " + task.attempts + " time(s); retries are used up.", join("failed " + ago(task.updatedMs, now), task.detail ? (rejected ? "feedback:" : "last error:") + detail : ""), "qagent trace " + n);
    }
    if (task.state === "changes_requested") {
        const worker = task.assignee || "the worker";
        if (task.assignee && !online(task.assignee)) {
            return item(1, "blocked", "Changes requested, but " + worker + " is offline and cannot revise.", join(seen(task.assignee), task.detail ? "feedback:" + detail : ""), "qagent supervise " + task.assignee);
        }
        return item(3, "active", worker + " is revising after review (round " + task.round + ").", join("feedback " + ago(task.updatedMs, now), task.detail ? "feedback:" + detail : ""), "qagent trace " + n);
    }
    if (task.state === "blocked") {
        const dead = task.deps.filter((dep) => dep.state === "failed" || dep.state === "cancelled");
        const pending = task.deps.filter((dep) => dep.state !== "accepted");
        if (dead.length) {
            const list = dead.map((dep) => "#" + dep.id + " (" + dep.state + ")").join(", ");
            return item(1, "blocked", "Depends on " + list + ", so it can never start.", "dependencies: " + task.deps.map((dep) => "#" + dep.id + " " + dep.state.replace(/_/g, " ")).join(", "), "qagent task cancel " + n + " --reason \"dependency " + dead.map((dep) => "#" + dep.id).join(", ") + " did not finish\"" + asOperator);
        }
        return item(4, "queued", "Waiting on " + (pending.length ? pending.map((dep) => "#" + dep.id + " (" + dep.state.replace(/_/g, " ") + ")").join(", ") : "its dependencies") + ".", "created " + ago(task.createdMs, now), pending.length ? "qagent trace " + pending[0].id : "qagent trace " + n);
    }
    if (task.state === "claimed") {
        const worker = task.assignee || "the worker";
        const agent = agentOf(task.assignee);
        const expired = task.claimExpiresMs !== null && task.claimExpiresMs < now;
        const idle = now - task.updatedMs > stallMs && (!agent || agent.lastSeenMs === null || now - agent.lastSeenMs > stallMs);
        const evidence = join("last task activity " + ago(task.updatedMs, now), task.assignee ? seen(task.assignee) : "", expired ? "claim expired " + ago(task.claimExpiresMs, now) : "");
        if (expired || idle) {
            return item(2, "stalled", expired ? worker + "'s claim expired without a submission." : "Claimed by " + worker + ", but neither the task nor " + worker + " has moved since.", evidence, "qagent task requeue " + n + " --reason \"stalled\"" + asOperator);
        }
        return item(3, "active", worker + " is working on it.", evidence, "qagent trace " + n);
    }
    // open
    const retry = task.attempts > 0 ? "attempt " + (task.attempts + 1) + " of " + (task.maxRetries + 1) + (task.detail ? ", last error:" + detail : "") : "";
    if (task.assignee) {
        if (!online(task.assignee)) {
            return item(2, "stalled", "Assigned to " + task.assignee + ", who is offline, so nobody will claim it.", join(seen(task.assignee), "created " + ago(task.createdMs, now), retry), "qagent supervise " + task.assignee);
        }
        return item(4, "queued", "Waiting for " + task.assignee + " to claim it.", join("created " + ago(task.createdMs, now), retry), "qagent trace " + n);
    }
    if (!agents.some((agent) => agentState(agent, now, staleMs) !== "offline")) {
        return item(2, "stalled", "Unassigned, and no agent is online to claim it.", join("created " + ago(task.createdMs, now), retry), "qagent supervise --roster");
    }
    return item(4, "queued", "Unassigned; the next free agent can claim it.", join("created " + ago(task.createdMs, now), retry), "qagent trace " + n);
}
function firstLine(text) {
    const line = (text ?? "").trim().split("\n")[0]?.trim() ?? "";
    return line ? line.slice(0, 200) : null;
}
/**
 * Open tasks (up to 200) plus tasks that failed in the last day, with dependency states and
 * the line that explains each state. Four reads. `only` builds views for the given tasks instead.
 */
export function taskViews(bus, now, only) {
    let tasks;
    if (only)
        tasks = only;
    else {
        tasks = bus.listTasks({ limit: 200 });
        for (const task of bus.listTasks({ states: ["failed"], limit: 1000 })) {
            if (now - task.updatedMs <= RECENT_FAILURE_MS)
                tasks.push(task);
        }
    }
    const depIds = [...new Set(tasks.flatMap((task) => task.dependencies))];
    const depStates = new Map(depIds.length ? bus.taskSummaries(depIds).map((summary) => [summary.id, summary.state]) : []);
    const retried = tasks.filter((task) => task.attempts > 0).map((task) => task.id);
    const failures = new Map();
    if (retried.length) {
        const rows = bus.db.prepare(`SELECT task_id, body FROM task_notes WHERE task_id IN (${retried.map(() => "?").join(",")}) AND body LIKE 'failure (attempt %' ORDER BY id`).all(...retried);
        for (const row of rows)
            failures.set(Number(row.task_id), String(row.body).replace(/^failure \(attempt \d+\): /, ""));
    }
    return tasks.map((task) => {
        let detail = null;
        if (task.state === "submitted")
            detail = firstLine(task.result?.summary);
        else if (task.state === "changes_requested")
            detail = firstLine(task.review?.feedback);
        else if (task.state === "failed")
            detail = task.attempts > task.maxRetries ? firstLine(failures.get(task.id)) : firstLine(task.review?.feedback);
        else if (task.attempts > 0)
            detail = firstLine(failures.get(task.id));
        return {
            id: task.id, title: task.title, assignee: task.assignee, reviewer: task.reviewer, creator: task.creator, state: task.state,
            createdMs: task.createdMs, updatedMs: task.updatedMs, claimExpiresMs: task.claimExpiresMs, round: task.round,
            attempts: task.attempts, maxRetries: task.maxRetries,
            deps: task.dependencies.map((id) => ({ id, state: depStates.get(id) ?? "missing" })),
            detail, closed: task.state === "accepted" || task.state === "cancelled",
        };
    });
}
/** Agents as the attention rules see them, without the operator. */
export function agentViews(bus) {
    return bus.listAgents().filter((agent) => agent.id !== "operator").map((agent) => ({
        id: agent.id, status: agent.storedStatus, waitUntilMs: agent.waitUntilMs, lastSeenMs: agent.lastSeenMs,
    }));
}
/** Every task that needs the operator, most urgent first. */
export function attentionList(tasks, agents, now, staleMs, stallMs = STALL_MS) {
    return tasks.map((task) => ({ task, attention: attention(task, agents, now, staleMs, stallMs) }))
        .sort((a, b) => a.attention.tier - b.attention.tier || a.task.id - b.task.id);
}
/** The attention line for one task, open or closed. */
export function taskAttention(bus, task, staleMs, now = bus.now()) {
    return attention(taskViews(bus, now, [task])[0], agentViews(bus), now, staleMs, STALL_MS);
}
//# sourceMappingURL=attention.js.map