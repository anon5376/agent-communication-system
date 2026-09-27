import { enabledAgents, resolveAgent, } from "./config.js";
const CAPABILITY_NAMES = [
    "coding",
    "reasoning",
    "planning",
    "debugging",
    "research",
    "toolUse",
    "speed",
    "tokenEfficiency",
    "reliability",
    "autonomy",
];
const COST_SCORE = {
    local: 1,
    subscription: 0.9,
    low: 0.75,
    medium: 0.45,
    high: 0.15,
};
function clamp(value, min = 0, max = 1) {
    return Math.max(min, Math.min(max, value));
}
function normalizedComplexity(value) {
    if (!Number.isFinite(value))
        return 0.5;
    return clamp((value - 1) / 4);
}
function requirementWeights(config, role, complexity) {
    const policy = config.roles[role];
    if (!policy)
        throw new Error(`unknown role: ${role}`);
    const c = normalizedComplexity(complexity);
    const weights = Object.fromEntries(CAPABILITY_NAMES.map((name) => [name, 0]));
    for (const [name, weight] of Object.entries(policy.capabilityWeights)) {
        weights[name] = Number(weight ?? 0);
    }
    // Difficult work values reasoning/reliability/autonomy more. Cheap work values speed/token efficiency.
    weights.reasoning += c * 0.55;
    weights.reliability += c * 0.45;
    weights.autonomy += c * 0.35;
    weights.speed += (1 - c) * 0.35;
    weights.tokenEfficiency += (1 - c) * 0.45;
    return weights;
}
function telemetryFor(agentId, telemetry) {
    return telemetry.find((item) => item.agentId === agentId);
}
function availabilityFor(agentId, availability) {
    return availability.find((item) => item.agentId === agentId) ?? {
        agentId,
        status: "unregistered",
        openTasks: 0,
    };
}
function scoreCandidate(config, agent, task, role, telemetry, availability) {
    const policy = config.roles[role];
    const caps = agent.modelDefinition.capabilities;
    const reasons = [];
    const rejectedBy = [];
    const components = {};
    if (!agent.enabled || !agent.modelDefinition.enabled || !agent.providerDefinition.enabled || !agent.harnessDefinition.enabled) {
        rejectedBy.push("agent, provider, model, or harness is disabled");
    }
    if (task.exactAgent && task.exactAgent !== agent.id)
        rejectedBy.push(`exact agent ${task.exactAgent} required`);
    if (task.allowedAgentIds?.length && !task.allowedAgentIds.includes(agent.id)) {
        rejectedBy.push(`agent ${agent.id} is outside this manager's spawn list`);
    }
    if (task.exactModel && task.exactModel !== agent.modelDefinition.id && task.exactModel !== agent.modelDefinition.exactModel) {
        rejectedBy.push(`exact model ${task.exactModel} required`);
    }
    // Role identity is enforced on the requested role only. Fallback roles still
    // score any agent against the fallback policy so reroute/exhaustion can move
    // work across the configured graph instead of a hardcoded pair.
    if (!task.exactAgent && role === task.role) {
        if (role === "reviewer") {
            if (agent.role !== "reviewer" && !agent.permissions.canReview) {
                rejectedBy.push(`agent role ${agent.role} is not a reviewer`);
            }
        }
        else if (agent.role !== role) {
            rejectedBy.push(`configured role ${agent.role} does not match ${role}`);
        }
    }
    const allowedFamilies = task.families?.length ? task.families : policy.families;
    if (allowedFamilies?.length && !allowedFamilies.includes(agent.modelDefinition.family)) {
        rejectedBy.push(`family ${agent.modelDefinition.family} is outside [${allowedFamilies.join(", ")}]`);
    }
    if (task.excludedFamilies?.includes(agent.modelDefinition.family)) {
        rejectedBy.push(`family ${agent.modelDefinition.family} explicitly excluded`);
    }
    const allowedProviders = task.providers?.length ? task.providers : policy.providers;
    if (allowedProviders?.length && !allowedProviders.includes(agent.modelDefinition.provider)) {
        rejectedBy.push(`provider ${agent.modelDefinition.provider} is outside [${allowedProviders.join(", ")}]`);
    }
    if (task.harness && task.harness !== agent.harnessDefinition.id && task.harness !== agent.harnessDefinition.adapter) {
        rejectedBy.push(`harness ${agent.harnessDefinition.id} is not ${task.harness}`);
    }
    if (config.constraints.permittedProviders.length && !config.constraints.permittedProviders.includes(agent.modelDefinition.provider)) {
        rejectedBy.push(`provider ${agent.modelDefinition.provider} denied by global constraints`);
    }
    if (config.constraints.permittedFamilies.length && !config.constraints.permittedFamilies.includes(agent.modelDefinition.family)) {
        rejectedBy.push(`family ${agent.modelDefinition.family} denied by global constraints`);
    }
    if (task.contextTokens > caps.contextTokens) {
        rejectedBy.push(`context estimate ${task.contextTokens} exceeds model profile ${caps.contextTokens}`);
    }
    if ((task.writeAccess || policy.requireWrite) && agent.permissions.filesystem !== "write") {
        rejectedBy.push("task needs repository write permission");
    }
    if ((task.shell || policy.requireShell) && !agent.permissions.shell)
        rejectedBy.push("task needs shell permission");
    if ((task.network || policy.requireNetwork) && !agent.permissions.network)
        rejectedBy.push("task needs network permission");
    const required = policy.minimumCapability ?? 0;
    const weights = requirementWeights(config, role, task.complexity);
    let weightedFit = 0;
    let totalWeight = 0;
    let largestGap = 0;
    for (const name of CAPABILITY_NAMES) {
        const weight = weights[name];
        if (weight <= 0)
            continue;
        weightedFit += caps[name] * weight;
        totalWeight += weight;
        largestGap = Math.max(largestGap, required - caps[name]);
    }
    const capabilityFit = totalWeight ? weightedFit / totalWeight : 0.5;
    if (capabilityFit < required) {
        rejectedBy.push(`capability fit ${capabilityFit.toFixed(2)} below role minimum ${required.toFixed(2)}`);
    }
    components.capability = capabilityFit;
    const c = normalizedComplexity(task.complexity);
    components.speed = caps.speed * (1 - c * 0.7);
    components.tokenEfficiency = caps.tokenEfficiency * (1 - c * 0.35);
    components.reliability = caps.reliability * (0.65 + c * 0.35);
    components.subscription = agent.providerDefinition.subscriptionBacked ? 1 : 0;
    components.cost = COST_SCORE[caps.costClass] ?? 0.4;
    const observed = telemetryFor(agent.id, telemetry);
    if (observed && observed.taskCount > 0) {
        components.observedSuccess = clamp(observed.acceptedCount / observed.taskCount - observed.failedCount / Math.max(1, observed.taskCount) * 0.35);
        components.observedLatency = observed.averageLatencyMs > 0
            ? clamp(1 - Math.log10(Math.max(1000, observed.averageLatencyMs)) / 7)
            : 0.5;
        reasons.push(`observed ${observed.acceptedCount}/${observed.taskCount} accepted, ${observed.reviewRejectedCount} review rejection(s)`);
    }
    else {
        components.observedSuccess = 0.5;
        components.observedLatency = 0.5;
        reasons.push("no task-history evidence; using configured capability profile");
    }
    components.familyDiversity = task.implementationFamily && task.implementationFamily !== agent.modelDefinition.family ? 1 : 0;
    const live = availabilityFor(agent.id, availability);
    components.availability = live.status === "idle" || live.status === "waiting"
        ? 1
        : live.status === "working"
            ? 0.45 / Math.max(1, live.openTasks)
            : live.status === "unregistered"
                ? 0.25
                : 0;
    const w = config.routing.weights;
    let score = components.capability * w.capability +
        components.speed * w.speed +
        components.tokenEfficiency * w.tokenEfficiency +
        components.reliability * w.reliability +
        components.subscription * w.subscription +
        components.cost * w.cost +
        components.observedSuccess * w.observedSuccess +
        components.observedLatency * w.observedLatency +
        components.familyDiversity * w.familyDiversity +
        components.availability * w.availability;
    const total = Object.values(w).reduce((sum, value) => sum + value, 0);
    score = total ? score / total : score;
    score -= Math.max(0, largestGap) * 0.35;
    score -= Math.min(0.25, live.openTasks * 0.04);
    if (agent.role === role) {
        score += 0.12;
        reasons.push(`configured as ${role}`);
    }
    else if (role === "reviewer" && agent.permissions.canReview) {
        reasons.push("review permission allows filling reviewer");
    }
    score = clamp(score, -1, 1);
    if (policy.independentFamilyReview && task.implementationFamily) {
        if (agent.modelDefinition.family === task.implementationFamily) {
            rejectedBy.push(`review must use a family other than ${task.implementationFamily}`);
        }
        else {
            reasons.push(`independent ${agent.modelDefinition.family} review of ${task.implementationFamily} work`);
        }
    }
    const preferSubscription = task.preferSubscription ?? policy.preferSubscription ?? config.constraints.preferSubscription;
    if (preferSubscription && agent.providerDefinition.subscriptionBacked)
        reasons.push("uses an existing subscription-backed account");
    if (task.complexity <= 2 && caps.tokenEfficiency >= 0.75)
        reasons.push("high token efficiency fits low-complexity work");
    if (task.complexity >= 4 && capabilityFit >= 0.8)
        reasons.push("strong configured fit for high-complexity work");
    if (live.status === "idle" || live.status === "waiting")
        reasons.push("agent is immediately available");
    else
        reasons.push(`agent status is ${live.status}${live.openTasks ? ` with ${live.openTasks} open task(s)` : ""}`);
    return {
        agentId: agent.id,
        model: agent.modelDefinition.id,
        family: agent.modelDefinition.family,
        provider: agent.modelDefinition.provider,
        harness: agent.harnessDefinition.id,
        score,
        eligible: rejectedBy.length === 0 && score >= config.routing.minimumScore,
        reasons,
        rejectedBy,
        components,
    };
}
function rolesToTry(config, requestedRole) {
    const roles = [requestedRole, ...(config.routing.fallbackRoles[requestedRole] ?? [])];
    return [...new Set(roles)].filter((role) => Boolean(config.roles[role]));
}
export function routeTask(config, task, telemetry = [], availability = []) {
    const agents = enabledAgents(config);
    let finalCandidates = [];
    let usedRole = task.role;
    let selected;
    for (const role of rolesToTry(config, task.role)) {
        const candidates = agents
            .map((agent) => scoreCandidate(config, agent, task, role, telemetry, availability))
            .sort((a, b) => b.score - a.score || a.agentId.localeCompare(b.agentId));
        finalCandidates = candidates;
        selected = candidates.find((candidate) => candidate.eligible);
        usedRole = role;
        if (selected)
            break;
    }
    const topRejected = finalCandidates.slice(0, 3).map((candidate) => `${candidate.agentId}: ${candidate.rejectedBy.join("; ") || `score ${candidate.score.toFixed(3)}`}`);
    const reason = selected
        ? `${selected.agentId} selected for ${usedRole}: score ${selected.score.toFixed(3)}; ${selected.reasons.slice(0, 4).join("; ")}`
        : `no eligible agent for ${task.role}; ${topRejected.join(" | ") || "no enabled candidates"}`;
    return {
        selectedAgentId: selected?.agentId ?? null,
        selectedModel: selected?.model ?? null,
        selectedFamily: selected?.family ?? null,
        selectedProvider: selected?.provider ?? null,
        selectedHarness: selected?.harness ?? null,
        role: task.role,
        usedFallbackRole: usedRole === task.role ? null : usedRole,
        reason,
        candidates: finalCandidates,
        createdAt: Date.now(),
    };
}
export function routeExactAgent(config, agentId, task) {
    resolveAgent(config, agentId); // fail loudly for typos
    return routeTask(config, { ...task, exactAgent: agentId });
}
//# sourceMappingURL=router.js.map