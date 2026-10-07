import { spawn } from "node:child_process";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
const EMPTY_USAGE = { inputTokens: 0, outputTokens: 0, totalTokens: 0, costUSD: 0 };
function stripAnsi(value) {
    return value.replace(/\x1b\[[0-9;?]*[A-Za-z]/g, "");
}
function jsonLines(stdout) {
    const rows = [];
    for (const line of stdout.split("\n")) {
        const trimmed = line.trim();
        if (!trimmed.startsWith("{"))
            continue; // mixed prose lines are common and never parse
        try {
            const parsed = JSON.parse(trimmed);
            if (parsed && typeof parsed === "object" && !Array.isArray(parsed))
                rows.push(parsed);
        }
        catch {
            // Mixed prose/JSON output is normal for several CLIs.
        }
    }
    return rows;
}
function defaultResult(stdout, exitCode) {
    const text = stripAnsi(stdout).trim();
    return {
        text: text || "(no textual output captured)",
        sessionId: null,
        usage: { ...EMPTY_USAGE },
        structured: null,
        malformed: exitCode === 0 && text.length === 0,
    };
}
function asNumber(value) {
    const n = Number(value ?? 0);
    return Number.isFinite(n) ? n : 0;
}
function commonEnvironment(context) {
    return {
        ...context.busEnvironment,
        AGENT_ID: context.agent.id,
        AGENT_ROLE: context.agent.role,
        AGENT_MODEL: context.agent.modelDefinition.id,
        AGENT_FAMILY: context.agent.modelDefinition.family,
        AGENT_PROVIDER: context.agent.modelDefinition.provider,
        AGENT_HARNESS: context.agent.harnessDefinition.id,
    };
}
/** The MCP server launch line: the v2 mcpCommand when given, otherwise the legacy `node <mcpServerPath>`. */
function mcpLaunch(context, env) {
    if (!context.mcpCommand)
        return { command: process.execPath, args: [context.mcpServerPath], env };
    return { command: context.mcpCommand.command, args: [...context.mcpCommand.args], env: { ...env, ...(context.mcpCommand.env ?? {}) } };
}
function commandTemplateValue(value, context) {
    const replacements = {
        "{prompt}": context.prompt,
        "{model}": context.agent.modelDefinition.exactModel ?? context.agent.modelDefinition.id,
        "{modelId}": context.agent.modelDefinition.id,
        "{family}": context.agent.modelDefinition.family,
        "{provider}": context.agent.modelDefinition.provider,
        "{agentId}": context.agent.id,
        "{role}": context.agent.role,
        "{session}": context.sessionId ?? "",
        "{workdir}": context.workdir,
        "{mcpServer}": context.mcpServerPath,
    };
    let output = value;
    for (const [token, replacement] of Object.entries(replacements)) {
        output = output.split(token).join(replacement);
    }
    return output;
}
function genericCommandResult(stdout, exitCode) {
    for (const row of jsonLines(stdout).reverse()) {
        const text = row.result ?? row.text ?? row.content ?? row.message;
        if (typeof text !== "string")
            continue;
        const usage = (row.usage ?? {});
        const input = asNumber(usage.input_tokens ?? usage.inputTokens ?? row.inputTokens);
        const output = asNumber(usage.output_tokens ?? usage.outputTokens ?? row.outputTokens);
        const total = asNumber(usage.total_tokens ?? usage.totalTokens ?? row.totalTokens) || input + output;
        return {
            text,
            sessionId: typeof row.session_id === "string" ? row.session_id :
                typeof row.sessionId === "string" ? row.sessionId : null,
            usage: {
                inputTokens: input,
                outputTokens: output,
                totalTokens: total,
                costUSD: asNumber(usage.cost_usd ?? usage.costUSD ?? row.costUSD),
            },
            structured: row,
            malformed: false,
        };
    }
    return defaultResult(stdout, exitCode);
}
/**
 * Escape hatch for models/harnesses Qagent does not know about yet.
 *
 * Configure a harness with `adapter: "command"`, then set per-agent harnessOptions:
 *   args: ["run", "--model", "{model}", "--prompt", "{prompt}"]
 *   resumeArgs: ["run", "--resume", "{session}", "--prompt", "{prompt}"]
 *   env: { SOME_PROFILE: "work" }
 *   autoReport: true
 *   timeoutMs: 3600000
 *
 * Supported placeholders: {prompt}, {model}, {modelId}, {family}, {provider},
 * {agentId}, {role}, {session}, {workdir}, {mcpServer}.
 *
 * If the custom CLI has its own Qagent MCP integration, set autoReport=false.
 * If it is a plain one-shot/model CLI, leave autoReport=true and the supervisor
 * submits its textual result for ordinary worker tasks.
 */
const commandAdapter = {
    id: "command",
    build(context) {
        const options = context.agent.harnessOptions ?? {};
        const configuredArgs = context.sessionId && Array.isArray(options.resumeArgs) ? options.resumeArgs : options.args;
        const rawArgs = Array.isArray(configuredArgs) ? configuredArgs.map(String) : ["{prompt}"];
        const args = rawArgs
            .map((item) => commandTemplateValue(item, context))
            .filter((item, index) => item.length > 0 || rawArgs[index] === "");
        const rawEnv = options.env && typeof options.env === "object" && !Array.isArray(options.env)
            ? options.env
            : {};
        const environment = {
            ...commonEnvironment(context),
            ...Object.fromEntries(Object.entries(rawEnv).map(([key, value]) => [key, commandTemplateValue(String(value), context)])),
        };
        const timeoutMs = Math.max(1_000, Math.min(24 * 60 * 60_000, Number(options.timeoutMs ?? 60 * 60_000) || 60 * 60_000));
        const autoReport = options.autoReport === undefined
            ? !context.agent.harnessDefinition.features.mcp
            : Boolean(options.autoReport);
        return {
            command: context.agent.harnessDefinition.command,
            args,
            environment,
            autoReport,
            timeoutMs,
        };
    },
    parse: genericCommandResult,
};
const claudeAdapter = {
    id: "claude",
    build(context) {
        const env = commonEnvironment(context);
        const mcp = JSON.stringify({
            mcpServers: {
                "qagent": mcpLaunch(context, env),
            },
        });
        const args = [
            "-p",
            context.prompt,
            "--mcp-config",
            mcp,
            "--output-format",
            "json",
            "--permission-mode",
            "acceptEdits",
            "--allowedTools",
            "mcp__qagent,Bash,Read,Write,Edit,Glob,Grep",
        ];
        if (context.sessionId)
            args.push("--resume", context.sessionId);
        if (context.agent.modelDefinition.exactModel)
            args.push("--model", context.agent.modelDefinition.exactModel);
        const effort = String(context.agent.harnessOptions?.effort ?? "");
        if (effort)
            args.push("--effort", effort);
        return {
            command: context.agent.harnessDefinition.command,
            args,
            environment: { ...env, MCP_TOOL_TIMEOUT: "3600000", QAGENT_BLOCK_SEC: "900", AGENT_BUS_BLOCK_SEC: "900" },
            autoReport: false,
            timeoutMs: 60 * 60_000,
        };
    },
    parse(stdout, exitCode) {
        for (const row of jsonLines(stdout).reverse()) {
            const result = row.result;
            if (typeof result !== "string")
                continue;
            const usage = row.usage;
            const input = asNumber(usage?.input_tokens) + asNumber(usage?.cache_read_input_tokens) + asNumber(usage?.cache_creation_input_tokens);
            const output = asNumber(usage?.output_tokens);
            return {
                text: result,
                sessionId: typeof row.session_id === "string" ? row.session_id : null,
                usage: { inputTokens: input, outputTokens: output, totalTokens: input + output, costUSD: asNumber(row.total_cost_usd) },
                structured: row,
                malformed: false,
            };
        }
        return defaultResult(stdout, exitCode);
    },
};
const codexAdapter = {
    id: "codex",
    build(context) {
        const env = commonEnvironment(context);
        const launch = mcpLaunch(context, env);
        const cfg = [
            `mcp_servers.agent_bus.command="${launch.command}"`,
            context.mcpCommand ? `mcp_servers.agent_bus.args=${JSON.stringify(launch.args)}` : `mcp_servers.agent_bus.args=["${context.mcpServerPath}"]`,
            "mcp_servers.agent_bus.startup_timeout_sec=30",
            "mcp_servers.agent_bus.tool_timeout_sec=300",
            `mcp_servers.agent_bus.env=${JSON.stringify(launch.env)}`,
        ].flatMap((item) => ["-c", item]);
        const reasoning = String(context.agent.harnessOptions?.reasoning ?? "");
        if (reasoning)
            cfg.push("-c", `model_reasoning_effort="${reasoning}"`);
        const localProvider = String(context.agent.harnessOptions?.localProvider ?? "");
        if (localProvider)
            cfg.push("--oss", "--local-provider", localProvider);
        // Current Codex builds cancel stdio MCP calls in every sandboxed mode.
        // Bus permissions remain enforced by the broker; the CLI must run with full
        // access for its MCP calls to reach that broker at all.
        const fullAccess = ["--dangerously-bypass-approvals-and-sandbox"];
        let args;
        if (context.pinnedSessionId) {
            // Explicit bindings target the original Desktop/TUI thread. `queue` is
            // the exact-thread API and lets that original chat visibly process mail.
            args = ["queue", ...cfg, ...fullAccess];
            if (context.agent.modelDefinition.exactModel)
                args.push("-m", context.agent.modelDefinition.exactModel);
            args.push("--thread", context.pinnedSessionId, "--message", context.prompt);
        }
        else {
            args = context.sessionId
                ? ["exec", "resume", ...cfg, ...fullAccess, "--skip-git-repo-check", "--json"]
                : ["exec", ...cfg, ...fullAccess, "--skip-git-repo-check", "--json"];
            if (context.agent.modelDefinition.exactModel)
                args.push("-m", context.agent.modelDefinition.exactModel);
            if (context.sessionId)
                args.push(context.sessionId);
            args.push(context.prompt);
        }
        return {
            command: context.agent.harnessDefinition.command,
            args,
            environment: { ...env, QAGENT_BLOCK_SEC: "240", AGENT_BUS_BLOCK_SEC: "240" },
            autoReport: false,
            timeoutMs: 60 * 60_000,
        };
    },
    parse(stdout, exitCode) {
        const rows = jsonLines(stdout);
        let sessionId = null;
        let inputTokens = 0;
        let outputTokens = 0;
        const text = [];
        for (const row of rows) {
            if (row.type === "thread.started" && typeof row.thread_id === "string")
                sessionId = row.thread_id;
            const item = row.item && typeof row.item === "object" ? row.item : null;
            if (item?.type === "agent_message" && typeof item.text === "string")
                text.push(item.text);
            const usage = row.usage && typeof row.usage === "object" ? row.usage : null;
            inputTokens = Math.max(inputTokens, asNumber(usage?.input_tokens ?? usage?.inputTokens));
            outputTokens = Math.max(outputTokens, asNumber(usage?.output_tokens ?? usage?.outputTokens));
        }
        if (text.length) {
            return {
                text: text.join("\n"),
                sessionId,
                usage: { inputTokens, outputTokens, totalTokens: inputTokens + outputTokens, costUSD: 0 },
                structured: { events: rows.length },
                malformed: false,
            };
        }
        const result = defaultResult(stdout, exitCode);
        const clean = stripAnsi(stdout);
        const match = clean.match(/tokens used\s*\n\s*([\d,]+)/i);
        if (match)
            result.usage.totalTokens = Number(match[1].replace(/,/g, "")) || 0;
        result.sessionId = sessionId;
        return result;
    },
};
const kimiAdapter = {
    id: "kimi",
    build(context) {
        const env = commonEnvironment(context);
        const args = context.sessionId ? ["--session", context.sessionId] : [];
        args.push("--prompt", context.prompt, "--auto", "--output-format", "stream-json");
        if (context.agent.modelDefinition.exactModel)
            args.push("-m", context.agent.modelDefinition.exactModel);
        return { command: context.agent.harnessDefinition.command, args, environment: env, autoReport: false, timeoutMs: 60 * 60_000 };
    },
    parse(stdout, exitCode) {
        const result = defaultResult(stdout, exitCode);
        const session = stdout.match(/kimi (?:-S|--session) (session_[\w-]+)/);
        if (session)
            result.sessionId = session[1];
        for (const row of jsonLines(stdout)) {
            const id = row.session_id ?? row.sessionId ?? row.sessionID;
            if (typeof id === "string")
                result.sessionId = id;
        }
        for (const match of stdout.matchAll(/"tokens":\{"total":(\d+)/g)) {
            result.usage.totalTokens = Math.max(result.usage.totalTokens, Number(match[1]) || 0);
        }
        return result;
    },
};
const geminiAdapter = {
    id: "gemini",
    build(context) {
        const env = commonEnvironment(context);
        const args = ["-p", context.prompt, "--output-format", "json", "--approval-mode", "yolo"];
        if (context.sessionId)
            args.push("--resume", context.sessionId);
        if (context.agent.modelDefinition.exactModel)
            args.push("-m", context.agent.modelDefinition.exactModel);
        return { command: context.agent.harnessDefinition.command, args, environment: env, autoReport: false, timeoutMs: 60 * 60_000 };
    },
    parse: defaultResult,
};
const cursorAdapter = {
    id: "cursor",
    prepare(context) {
        mkdirSync(join(context.workdir, ".cursor"), { recursive: true });
        const cfgPath = join(context.workdir, ".cursor", "mcp.json");
        let cfg = {};
        try {
            cfg = JSON.parse(readFileSync(cfgPath, "utf8"));
        }
        catch { /* new project-local MCP config */ }
        const mcp = (cfg.mcpServers && typeof cfg.mcpServers === "object" ? cfg.mcpServers : {});
        mcp["qagent"] = mcpLaunch(context, commonEnvironment(context));
        cfg.mcpServers = mcp;
        writeFileSync(cfgPath, JSON.stringify(cfg, null, 2));
    },
    build(context) {
        const env = commonEnvironment(context);
        const args = ["-p", context.prompt, "--output-format", "json", "--force", "--trust", "--approve-mcps"];
        if (context.sessionId)
            args.push("--resume", context.sessionId);
        if (context.agent.modelDefinition.exactModel)
            args.push("--model", context.agent.modelDefinition.exactModel);
        return {
            command: context.agent.harnessDefinition.command,
            args,
            environment: { ...env, QAGENT_BLOCK_SEC: "900", AGENT_BUS_BLOCK_SEC: "900" },
            autoReport: false,
            timeoutMs: 60 * 60_000,
        };
    },
    parse(stdout, exitCode) {
        for (const row of jsonLines(stdout).reverse()) {
            const text = row.result ?? row.text ?? row.content ?? row.message;
            if (typeof text !== "string")
                continue;
            const usage = (row.usage ?? {});
            const input = asNumber(usage.input_tokens ?? usage.inputTokens);
            const output = asNumber(usage.output_tokens ?? usage.outputTokens);
            return {
                text,
                sessionId: typeof row.session_id === "string" ? row.session_id : typeof row.sessionId === "string" ? row.sessionId : typeof row.chatId === "string" ? row.chatId : null,
                usage: {
                    inputTokens: input,
                    outputTokens: output,
                    totalTokens: asNumber(usage.total_tokens ?? usage.totalTokens) || input + output,
                    costUSD: asNumber(usage.cost_usd ?? usage.costUSD),
                },
                structured: row,
                malformed: false,
            };
        }
        return defaultResult(stdout, exitCode);
    },
};
const grokAdapter = {
    id: "grok",
    build(context) {
        const env = commonEnvironment(context);
        const args = ["-p", context.prompt, "--output-format", "json", "--always-approve"];
        if (context.sessionId)
            args.push("-r", context.sessionId);
        if (context.agent.modelDefinition.exactModel)
            args.push("-m", context.agent.modelDefinition.exactModel);
        return { command: context.agent.harnessDefinition.command, args, environment: env, autoReport: false, timeoutMs: 60 * 60_000 };
    },
    parse(stdout, exitCode) {
        for (const row of jsonLines(stdout).reverse()) {
            const text = row.result ?? row.text ?? row.content;
            if (typeof text !== "string")
                continue;
            const usage = (row.usage ?? {});
            const input = asNumber(usage.input_tokens ?? usage.inputTokens);
            const output = asNumber(usage.output_tokens ?? usage.outputTokens);
            return {
                text,
                sessionId: typeof row.session_id === "string" ? row.session_id : typeof row.sessionId === "string" ? row.sessionId : null,
                usage: { inputTokens: input, outputTokens: output, totalTokens: asNumber(usage.total_tokens ?? usage.totalTokens) || input + output, costUSD: asNumber(usage.cost_usd) },
                structured: row,
                malformed: false,
            };
        }
        return defaultResult(stdout, exitCode);
    },
};
const opencodeAdapter = {
    id: "opencode",
    prepare(context) {
        const cfgPath = join(context.workdir, "opencode.json");
        let cfg = {};
        try {
            cfg = JSON.parse(readFileSync(cfgPath, "utf8"));
        }
        catch { /* new project-local configuration */ }
        const mcp = (cfg.mcp && typeof cfg.mcp === "object" ? cfg.mcp : {});
        const launch = mcpLaunch(context, commonEnvironment(context));
        mcp["qagent"] = {
            type: "local",
            command: [launch.command, ...launch.args],
            environment: launch.env,
            enabled: true,
        };
        cfg.$schema = "https://opencode.ai/config.json";
        cfg.mcp = mcp;
        writeFileSync(cfgPath, JSON.stringify(cfg, null, 2));
    },
    build(context) {
        const env = commonEnvironment(context);
        const args = ["run", "--auto", "--format", "json"];
        if (context.agent.modelDefinition.exactModel)
            args.push("-m", context.agent.modelDefinition.exactModel);
        const variant = String(context.agent.harnessOptions?.variant ?? "");
        if (variant)
            args.push("--variant", variant);
        if (context.sessionId)
            args.push("-s", context.sessionId);
        args.push(context.prompt);
        return { command: context.agent.harnessDefinition.command, args, environment: env, autoReport: false, timeoutMs: 60 * 60_000 };
    },
    parse(stdout, exitCode) {
        const rows = jsonLines(stdout);
        const parts = [];
        let sessionId = null;
        let totalTokens = 0;
        for (const row of rows) {
            const part = row.part;
            if (part?.type === "text" && typeof part.text === "string")
                parts.push(part.text);
            if (typeof row.sessionID === "string")
                sessionId = row.sessionID;
            const tokens = row.tokens;
            totalTokens = Math.max(totalTokens, asNumber(tokens?.total));
        }
        if (!parts.length)
            return defaultResult(stdout, exitCode);
        return {
            text: parts.join(""),
            sessionId,
            usage: { ...EMPTY_USAGE, totalTokens },
            structured: { events: rows.length },
            malformed: false,
        };
    },
};
const hermesAdapter = {
    id: "hermes",
    build(context) {
        const env = { ...commonEnvironment(context), HERMES_DISABLE_STREAMING: "1" };
        const profile = String(context.agent.harnessOptions?.profile ?? "default");
        const args = ["--profile", profile, "chat", "-q", context.prompt, "-Q", "--yolo", "--pass-session-id"];
        if (context.sessionId)
            args.push("--resume", context.sessionId);
        return { command: context.agent.harnessDefinition.command, args, environment: env, autoReport: false, timeoutMs: 60 * 60_000 };
    },
    parse(stdout, exitCode) {
        const clean = stripAnsi(stdout);
        const blocks = clean.split(/\n─{20,}\n/);
        const text = blocks.length >= 2 ? blocks[blocks.length - 2].trim() : clean.trim();
        const session = clean.match(/--resume\s+(\S+)/);
        const tokens = clean.match(/~?([\d,]+)\s*tokens/i);
        return {
            text: text || "(no textual output captured)",
            sessionId: session?.[1] ?? null,
            usage: { ...EMPTY_USAGE, totalTokens: tokens ? Number(tokens[1].replace(/,/g, "")) || 0 : 0 },
            structured: null,
            malformed: exitCode === 0 && !text,
        };
    },
};
/**
 * Devin CLI (Cognition): `devin -p <prompt>` runs one turn and prints the answer. It has no per-run
 * MCP flag (servers come from `devin mcp add` or .devin/mcp_config*.json), so its harness declares
 * mcp: false and the supervisor claims the task and submits the printed answer.
 */
const devinAdapter = {
    id: "devin",
    build(context) {
        const env = commonEnvironment(context);
        const args = ["-p", context.prompt, "--permission-mode", "dangerous"];
        if (context.sessionId)
            args.push("--resume", context.sessionId);
        if (context.agent.modelDefinition.exactModel)
            args.push("--model", context.agent.modelDefinition.exactModel);
        const autoReport = !context.agent.harnessDefinition.features.mcp;
        return { command: context.agent.harnessDefinition.command, args, environment: env, autoReport, timeoutMs: 60 * 60_000 };
    },
    parse: defaultResult,
};
const fakeAdapter = {
    id: "fake",
    prepare(context) { mkdirSync(join(context.workdir, ".agent-bus"), { recursive: true }); },
    build(context) {
        const env = commonEnvironment(context);
        // The fake harness cannot speak MCP; its bus-cli mode runs the same `qagent` command line instead.
        if (context.mcpCommand)
            env.QAGENT_MCP_COMMAND = JSON.stringify(mcpLaunch(context, {}));
        const mode = String(context.agent.harnessOptions?.mode ?? "success");
        const args = [context.fakeHarnessPath, "--mode", mode, "--agent", context.agent.id, "--prompt", context.prompt];
        if (context.sessionId)
            args.push("--session", context.sessionId);
        return { command: process.execPath, args, environment: env, autoReport: true, timeoutMs: 30_000 };
    },
    parse(stdout, exitCode) {
        const rows = jsonLines(stdout);
        const row = rows.at(-1);
        if (!row || typeof row.result !== "string") {
            const fallback = defaultResult(stdout, exitCode);
            fallback.malformed = exitCode === 0;
            return fallback;
        }
        const usage = (row.usage ?? {});
        return {
            text: row.result,
            sessionId: typeof row.sessionId === "string" ? row.sessionId : null,
            usage: {
                inputTokens: asNumber(usage.inputTokens),
                outputTokens: asNumber(usage.outputTokens),
                totalTokens: asNumber(usage.totalTokens),
                costUSD: asNumber(usage.costUSD),
            },
            structured: row,
            malformed: false,
        };
    },
};
const ADAPTERS = {
    claude: claudeAdapter,
    codex: codexAdapter,
    kimi: kimiAdapter,
    gemini: geminiAdapter,
    cursor: cursorAdapter,
    grok: grokAdapter,
    opencode: opencodeAdapter,
    hermes: hermesAdapter,
    devin: devinAdapter,
    fake: fakeAdapter,
    command: commandAdapter,
};
export function getHarnessAdapter(id) {
    const adapter = ADAPTERS[id];
    if (!adapter)
        throw new Error(`unknown harness adapter: ${id}. Use adapter=command for configurable custom CLIs.`);
    return adapter;
}
function runCommand(command, args, timeoutMs = 10_000) {
    return new Promise((resolve) => {
        const child = spawn(command, args, { stdio: ["ignore", "pipe", "pipe"] });
        let output = "";
        const timer = setTimeout(() => child.kill("SIGTERM"), timeoutMs);
        child.stdout.on("data", (data) => (output += data.toString()));
        child.stderr.on("data", (data) => (output += data.toString()));
        child.on("error", (error) => {
            clearTimeout(timer);
            resolve({ code: -1, output: error.message });
        });
        child.on("close", (code) => {
            clearTimeout(timer);
            resolve({ code: code ?? -1, output: stripAnsi(output).trim() });
        });
    });
}
export async function probeCommand(command, probeArgs = ["--version"]) {
    const result = await runCommand(command, probeArgs);
    return {
        available: result.code === 0,
        version: result.code === 0 ? result.output.split("\n")[0] || null : null,
        error: result.code === 0 ? null : result.output || `exit ${result.code}`,
    };
}
export async function probeHarness(agent) {
    const harness = agent.harnessDefinition;
    const probe = await probeCommand(harness.command, harness.probeArgs ?? ["--version"]);
    return {
        harness: harness.id,
        command: harness.command,
        available: probe.available,
        version: probe.version,
        error: probe.error,
    };
}
export async function discoverHarnessModels(agent) {
    const discovery = agent.harnessDefinition.modelDiscovery;
    if (!discovery)
        return { harness: agent.harnessDefinition.id, models: [], error: "model discovery is registry-only for this harness" };
    const result = await runCommand(agent.harnessDefinition.command, discovery.args, 30_000);
    if (result.code !== 0)
        return { harness: agent.harnessDefinition.id, models: [], error: result.output || `exit ${result.code}` };
    try {
        const models = discovery.format === "json"
            ? JSON.parse(result.output).map(String)
            : result.output.split("\n").map((line) => line.trim()).filter(Boolean);
        return { harness: agent.harnessDefinition.id, models: [...new Set(models)], error: null };
    }
    catch (error) {
        return { harness: agent.harnessDefinition.id, models: [], error: `could not parse model discovery output: ${error.message}` };
    }
}
//# sourceMappingURL=adapters.js.map