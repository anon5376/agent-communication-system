import { existsSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
function envValue(...names) {
    for (const name of names) {
        const value = process.env[name];
        if (typeof value === "string" && value.trim())
            return value;
    }
    return undefined;
}
const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
export const PROJECT_ROOT = ROOT;
export const DEFAULT_CONFIG_PATH = join(ROOT, "agent-bus.config.json");
function assertObject(value, label) {
    if (!value || typeof value !== "object" || Array.isArray(value)) {
        throw new Error(`${label} must be an object`);
    }
}
function validateCapabilities(profile, label) {
    const names = [
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
    for (const name of names) {
        const value = profile[name];
        if (!Number.isFinite(value) || value < 0 || value > 1) {
            throw new Error(`${label}.${name} must be between 0 and 1`);
        }
    }
    if (!Number.isFinite(profile.contextTokens) || profile.contextTokens < 1) {
        throw new Error(`${label}.contextTokens must be a positive number`);
    }
}
export function validateConfig(value) {
    assertObject(value, "configuration");
    if (value.version !== 1)
        throw new Error("configuration.version must be 1");
    for (const key of ["providers", "harnesses", "models", "agents", "roles", "routing", "constraints"]) {
        assertObject(value[key], `configuration.${key}`);
    }
    const config = value;
    for (const [id, provider] of Object.entries(config.providers)) {
        if (provider.id !== id)
            throw new Error(`provider key ${id} must match provider.id`);
    }
    for (const [id, harness] of Object.entries(config.harnesses)) {
        if (harness.id !== id)
            throw new Error(`harness key ${id} must match harness.id`);
        if (!harness.command)
            throw new Error(`harness ${id} has no command`);
        for (const providerId of harness.providers) {
            if (!config.providers[providerId])
                throw new Error(`harness ${id} references unknown provider ${providerId}`);
        }
    }
    for (const [id, model] of Object.entries(config.models)) {
        if (model.id !== id)
            throw new Error(`model key ${id} must match model.id`);
        if (!config.providers[model.provider])
            throw new Error(`model ${id} references unknown provider ${model.provider}`);
        if (!config.harnesses[model.harness])
            throw new Error(`model ${id} references unknown harness ${model.harness}`);
        validateCapabilities(model.capabilities, `models.${id}.capabilities`);
    }
    for (const [id, role] of Object.entries(config.roles)) {
        if (role.id !== id)
            throw new Error(`role key ${id} must match role.id`);
    }
    for (const [id, agent] of Object.entries(config.agents)) {
        if (agent.id !== id)
            throw new Error(`agent key ${id} must match agent.id`);
        // Nothing starts an agent unless the configuration says so explicitly.
        agent.autoStart = agent.autoStart === true;
        if (!config.models[agent.model])
            throw new Error(`agent ${id} references unknown model ${agent.model}`);
        if (!config.roles[agent.role])
            throw new Error(`agent ${id} references unknown role ${agent.role}`);
        if (agent.permissions.maxDelegationDepth < 0) {
            throw new Error(`agent ${id} maxDelegationDepth must be >= 0`);
        }
        if (agent.resumeSessionId !== undefined) {
            if (typeof agent.resumeSessionId !== "string" || !agent.resumeSessionId.trim()) {
                throw new Error(`agent ${id} resumeSessionId must be a non-empty string`);
            }
            const harness = config.harnesses[config.models[agent.model].harness];
            if (!harness.features.resume) {
                throw new Error(`agent ${id} pins session ${agent.resumeSessionId}, but harness ${harness.id} does not support resume`);
            }
            if (harness.adapter === "command") {
                const options = agent.harnessOptions ?? {};
                const rawArgs = Array.isArray(options.resumeArgs) ? options.resumeArgs : options.args;
                if (!Array.isArray(rawArgs) || !rawArgs.some((item) => String(item).includes("{session}"))) {
                    throw new Error(`agent ${id} uses the command adapter with resumeSessionId, but harnessOptions.resumeArgs/args has no {session} placeholder`);
                }
            }
        }
    }
    if (config.constraints.maxDelegationDepth < 0)
        throw new Error("maxDelegationDepth must be >= 0");
    if (config.constraints.maxConcurrentTasks < 1)
        throw new Error("maxConcurrentTasks must be >= 1");
    if (config.constraints.maxRetries < 0)
        throw new Error("maxRetries must be >= 0");
    return config;
}
export function loadConfig(path = envValue("QAGENT_CONFIG", "AGENT_BUS_CONFIG") ?? DEFAULT_CONFIG_PATH) {
    const absolute = resolve(path);
    if (!existsSync(absolute))
        throw new Error(`Qagent configuration not found: ${absolute}`);
    let parsed;
    try {
        parsed = JSON.parse(readFileSync(absolute, "utf8"));
    }
    catch (error) {
        throw new Error(`could not parse ${absolute}: ${error.message}`);
    }
    return validateConfig(parsed);
}
export function resolveAgent(config, id) {
    const agent = config.agents[id];
    if (!agent)
        throw new Error(`unknown agent "${id}" — configured agents: ${Object.keys(config.agents).join(", ")}`);
    const modelDefinition = config.models[agent.model];
    const providerDefinition = config.providers[modelDefinition.provider];
    const harnessDefinition = config.harnesses[modelDefinition.harness];
    return { ...agent, modelDefinition, providerDefinition, harnessDefinition };
}
export function enabledAgents(config) {
    return Object.keys(config.agents)
        .sort()
        .map((id) => resolveAgent(config, id))
        .filter((agent) => agent.enabled &&
        agent.modelDefinition.enabled &&
        agent.providerDefinition.enabled &&
        agent.harnessDefinition.enabled);
}
export function configPathFromProject(projectRoot) {
    const explicit = envValue("QAGENT_CONFIG", "AGENT_BUS_CONFIG");
    if (explicit)
        return explicit;
    const next = join(projectRoot, ".qagent", "config.json");
    if (existsSync(next))
        return next;
    const previous = join(projectRoot, ".agent-bus", "config.json");
    return existsSync(previous) ? previous : DEFAULT_CONFIG_PATH;
}
//# sourceMappingURL=config.js.map