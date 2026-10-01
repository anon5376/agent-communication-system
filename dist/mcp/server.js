/**
 * `qagent mcp [--operator]`: a stdio MCP server over the core library.
 *
 * Ported from mcp-server.ts:65-383 (renderers, zod schemas) with every broker
 * HTTP call replaced by a direct core call on the SQLite file. Nothing here opens
 * a socket: the only transport is stdin/stdout.
 *
 * Identity: QAGENT_AGENT_ID (old name AGENT_ID) names the agent; the token comes
 * from QAGENT_TOKEN (old name AGENT_TOKEN) or, when unset, from the token file
 * next to the database (tokens/<id>.token, operator.token for the operator). The
 * identity is re-checked on every call, so a rotated token takes effect at once.
 * No tool accepts a sender: every write is made as that identity.
 */
import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { z } from "zod";
import { Bus } from "../core/bus.js";
import { agentIdFromEnv, identityForToken, resolveIdentity } from "../core/identity.js";
import { BusError, LIMITS, MAX_WAIT_SEC, OPERATOR_ID } from "../core/types.js";
import { waitForMail, waitSeconds } from "../notify/wait.js";
import { ensureTaskWorktree } from "../worktree.js";
import { renderAgents, renderError, renderInbox, renderNote, renderSent, renderTask, renderTaskLine, renderTasks, renderWait, renderWhoami, } from "./render.js";
export const AGENT_TOOLS = [
    "bus_whoami", "bus_agents", "bus_send", "bus_inbox", "bus_wait", "bus_ack",
    "bus_task_create", "bus_task_list", "bus_task_get", "bus_task_claim", "bus_task_note",
    "bus_task_submit", "bus_task_review", "bus_task_cancel",
];
export const OPERATOR_TOOLS = ["bus_agent_add"];
const USAGE = `usage: qagent mcp [--operator]

Stdio MCP server for one agent. Set QAGENT_AGENT_ID; the token is read from
QAGENT_TOKEN or from the token file next to the bus database. --operator adds
bus_agent_add and requires the operator identity (operator.token).
`;
const refSchema = z.object({
    type: z.enum(["path", "artifact", "summary", "commit", "url"]),
    value: z.string().max(LIMITS.refValue),
    description: z.string().max(LIMITS.refDescription).optional(),
});
const validationSchema = z.object({
    passed: z.boolean(),
    summary: z.string(),
    command: z.string().optional(),
});
const taskState = z.enum(["open", "blocked", "claimed", "submitted", "changes_requested", "accepted", "failed", "cancelled"]);
export function identify(bus, agentId, token) {
    return token ? identityForToken(bus.db, agentId, token) : resolveIdentity(bus.db, bus.home, agentId);
}
export function createBusServer(bus, options) {
    const server = new McpServer({ name: "qagent", version: "2.0.0" });
    const waits = new Set();
    const me = () => {
        const identity = identify(bus, options.agentId, options.token);
        if (options.operator && identity.authority !== "operator")
            throw new BusError("forbidden", "qagent mcp --operator needs the operator identity");
        return identity;
    };
    const run = async (fn) => {
        try {
            return { content: [{ type: "text", text: await fn(me()) }] };
        }
        catch (error) {
            return { content: [{ type: "text", text: renderError(error) }], isError: true };
        }
    };
    server.registerTool("bus_whoami", {
        description: "Show your identity, authority, unread count and the agent roster. Call this first.",
        inputSchema: {},
    }, async () => run((identity) => renderWhoami(identity, bus.whoami(identity), bus.listAgents())));
    server.registerTool("bus_agents", {
        description: "List every agent with role, status (idle, waiting, working, offline), unread count and model.",
        inputSchema: {},
    }, async () => run(() => renderAgents(bus.listAgents())));
    server.registerTool("bus_send", {
        description: "Send a message as yourself. `to` is an agent id, a comma-separated list, or \"*\" for everyone. Point at large material with refs instead of pasting it.",
        inputSchema: {
            to: z.string().describe("Recipient id, \"a,b\", or \"*\""),
            subject: z.string().max(LIMITS.subject),
            body: z.string().max(LIMITS.body),
            type: z.enum(["info", "question", "answer"]).optional(),
            thread: z.string().max(LIMITS.thread).optional(),
            task_id: z.number().int().positive().optional(),
            refs: z.array(refSchema).max(LIMITS.refCount).optional(),
            requires_ack: z.boolean().optional(),
        },
    }, async (input) => run((identity) => renderSent(bus.send(identity, {
        to: input.to, subject: input.subject, body: input.body, type: input.type, thread: input.thread,
        taskId: input.task_id ?? null, refs: input.refs, requiresAck: input.requires_ack,
    }))));
    server.registerTool("bus_inbox", {
        description: "Read new messages without blocking. Marks them read unless peek is true.",
        inputSchema: {
            peek: z.boolean().optional(),
            limit: z.number().int().min(1).max(LIMITS.inboxLimit).optional(),
        },
    }, async (input) => run((identity) => renderInbox(bus.inbox(identity, { peek: input.peek, limit: input.limit }), Boolean(input.peek))));
    server.registerTool("bus_wait", {
        description: "Sleep without using tokens until mail or task activity for you arrives, then return it (mail is marked read). A timeout with nothing is normal; call again. Do not call this when a supervisor woke you.",
        inputSchema: {
            timeout_sec: z.number().int().min(1).max(MAX_WAIT_SEC).optional().describe("Default QAGENT_BLOCK_SEC or 240"),
        },
    }, async (input, extra) => run(async (identity) => {
        const seconds = waitSeconds(input.timeout_sec, options.env);
        const controller = new AbortController();
        const cancel = () => controller.abort();
        extra.signal.addEventListener("abort", cancel, { once: true });
        if (extra.signal.aborted)
            controller.abort();
        const done = waitForMail(bus, identity, { timeoutMs: seconds * 1000, signal: controller.signal });
        const entry = { controller, done: done.catch(() => undefined) };
        waits.add(entry);
        try {
            const result = await done;
            const cancelled = controller.signal.aborted;
            // A cancelled wait leaves the mail unread: nobody is listening for the reply.
            const delivered = result.status === "mail" && !cancelled && bus.db.isOpen ? bus.inbox(identity, { limit: 50 }) : null;
            return renderWait(result, delivered, seconds, cancelled);
        }
        finally {
            waits.delete(entry);
            extra.signal.removeEventListener("abort", cancel);
        }
    }));
    server.registerTool("bus_ack", {
        description: "Acknowledge a message that asked for an acknowledgement, by its sequence number.",
        inputSchema: { seq: z.number().int().positive() },
    }, async (input) => run((identity) => `Acknowledged #${bus.ack(identity, input.seq).seq}.`));
    server.registerTool("bus_task_create", {
        description: "Create a task. With `to` it is assigned and the assignee is sent the brief; without it any agent of the matching role may claim it. The brief must stand alone: the worker has none of your context. You review the result unless the operator does.",
        inputSchema: {
            title: z.string().max(LIMITS.title),
            brief: z.string().max(LIMITS.brief),
            to: z.string().optional(),
            parent_id: z.number().int().positive().optional(),
            dependencies: z.array(z.number().int().positive()).optional(),
            path_scopes: z.array(z.string()).optional().describe("Paths the task will write, relative to project; leased while claimed"),
            acceptance: z.string().max(LIMITS.acceptance).optional(),
            role: z.string().max(LIMITS.role).optional(),
            project: z.string().optional().describe("Project directory; defaults to the server's working directory when path_scopes are given"),
            priority: z.enum(["low", "normal", "high", "urgent"]).optional(),
        },
    }, async (input) => run((identity) => {
        const scopes = input.path_scopes ?? [];
        const project = input.project ?? (scopes.length ? options.cwd : null);
        const task = bus.createTask(identity, {
            title: input.title, brief: input.brief, to: input.to ?? null, parentId: input.parent_id ?? null,
            dependencies: input.dependencies, pathScopes: scopes, acceptance: input.acceptance, role: input.role,
            project, priority: input.priority,
        });
        return renderTaskLine(task, "Created");
    }));
    server.registerTool("bus_task_list", {
        description: "List tasks. By default only open tasks you created, hold, or review.",
        inputSchema: {
            mine: z.boolean().optional().describe("Default true"),
            state: z.array(taskState).optional(),
            include_closed: z.boolean().optional(),
            limit: z.number().int().min(1).max(1000).optional(),
        },
    }, async (input) => run((identity) => renderTasks(bus.listTasks({
        mine: input.mine === false ? null : identity.agentId, states: input.state,
        includeClosed: input.include_closed, limit: input.limit,
    }))));
    server.registerTool("bus_task_get", {
        description: "Show one task: brief, acceptance, state, result, review, notes and its message thread.",
        inputSchema: { task_id: z.number().int().positive() },
    }, async (input) => run(() => renderTask(bus.getTask(input.task_id))));
    server.registerTool("bus_task_claim", {
        description: "Claim a task. Without task_id, takes the most urgent open task assigned to you, or unassigned for your role. Claims expire after two hours without a note or submit. With worktree: true, also returns a private git worktree for the task (branch qagent/task-<id>); make your edits and commits there.",
        inputSchema: { task_id: z.number().int().positive().optional(), worktree: z.boolean().optional() },
    }, async (input) => run((identity) => {
        const task = bus.claimTask(identity, input.task_id ?? null);
        const text = `${renderTaskLine(task, "Claimed")}\n\n${renderTask(bus.getTask(task.id))}`;
        if (!input.worktree)
            return text;
        try {
            const worktree = ensureTaskWorktree(task, bus.home);
            return `${text}\n\nWorktree: ${worktree.workdir} (branch ${worktree.branch}). Edit and commit there, not in ${task.project}.`;
        }
        catch (error) {
            // The claim stands; only the isolation step failed.
            return `${text}\n\nNo worktree: ${error.message}`;
        }
    }));
    server.registerTool("bus_task_note", {
        description: "Add a progress note to a task. A note from the assignee also renews the claim.",
        inputSchema: { task_id: z.number().int().positive(), note: z.string().max(LIMITS.note) },
    }, async (input) => run((identity) => renderNote(bus.noteTask(identity, input.task_id, input.note))));
    server.registerTool("bus_task_submit", {
        description: "Submit your work on a claimed task for review: what you did, files changed, and validation you actually ran. Report failures honestly.",
        inputSchema: {
            task_id: z.number().int().positive(),
            summary: z.string().max(LIMITS.summary),
            details: z.string().max(LIMITS.details).optional(),
            changed_files: z.array(z.string()).max(LIMITS.changedFiles).optional(),
            artifacts: z.array(refSchema).max(LIMITS.refCount).optional(),
            validation: z.array(validationSchema).max(LIMITS.validation).optional(),
        },
    }, async (input) => run((identity) => {
        const task = bus.submitTask(identity, input.task_id, {
            summary: input.summary, details: input.details, changedFiles: input.changed_files, artifacts: input.artifacts, validation: input.validation,
        });
        return `${renderTaskLine(task, "Submitted")}. Reviewer: ${task.reviewer ?? task.creator}.`;
    }));
    server.registerTool("bus_task_review", {
        description: "Review submitted work: accepted true closes the task; false sends your feedback back for another round. Check the work before accepting.",
        inputSchema: { task_id: z.number().int().positive(), accepted: z.boolean(), feedback: z.string().max(LIMITS.feedback) },
    }, async (input) => run((identity) => {
        const task = bus.reviewTask(identity, input.task_id, { accepted: input.accepted, feedback: input.feedback });
        return renderTaskLine(task, task.state === "accepted" ? "Accepted" : task.state === "failed" ? "Failed (retry limit)" : "Requested changes on");
    }));
    server.registerTool("bus_task_cancel", {
        description: "Cancel a task you created (the operator may cancel any task).",
        inputSchema: { task_id: z.number().int().positive(), reason: z.string().max(LIMITS.reason).optional() },
    }, async (input) => run((identity) => renderTaskLine(bus.cancelTask(identity, input.task_id, input.reason), "Cancelled")));
    if (options.operator) {
        server.registerTool("bus_agent_add", {
            description: "Operator only: register an agent and write its token file. The token itself is never returned.",
            inputSchema: {
                id: z.string(),
                role: z.string().max(LIMITS.role),
                model: z.string().max(LIMITS.model).optional(),
                harness: z.string().max(LIMITS.model).optional(),
                parent: z.string().optional(),
                authority: z.enum(["worker", "manager"]).optional(),
            },
        }, async (input) => run((identity) => {
            const result = bus.addAgent(identity, {
                id: input.id, role: input.role, model: input.model, harness: input.harness, parent: input.parent ?? null, authority: input.authority,
            });
            return `Added ${result.agent.id} (${input.authority ?? "worker"}). Token file: ${result.tokenPath}. Start its MCP server with QAGENT_AGENT_ID=${result.agent.id}.`;
        }));
    }
    const drain = async () => {
        const pending = [...waits];
        for (const wait of pending)
            wait.controller.abort();
        await Promise.all(pending.map((wait) => wait.done));
    };
    return { server, drain };
}
function parseArgs(argv) {
    let operator = false;
    let help = false;
    for (let index = 0; index < argv.length; index += 1) {
        const arg = argv[index];
        if (arg === "--operator")
            operator = true;
        else if (arg === "--help" || arg === "-h")
            help = true;
        else if (arg === "--db")
            index += 1; // consumed by the CLI dispatcher
        else if (arg.startsWith("--db="))
            continue;
        else
            throw new BusError("invalid", `unknown argument: ${arg}\n\n${USAGE}`);
    }
    return { operator, help };
}
/** Entry point loaded lazily by src/cli/main.ts. Resolves when stdin closes or on SIGINT/SIGTERM. */
export async function main(argv, context) {
    const env = process.env;
    let args;
    try {
        args = parseArgs(argv);
    }
    catch (error) {
        process.stderr.write(`qagent mcp: ${renderError(error)}\n`);
        return 1;
    }
    if (args.help) {
        process.stderr.write(USAGE);
        return 0;
    }
    const agentId = agentIdFromEnv(env) ?? (args.operator ? OPERATOR_ID : null);
    if (!agentId) {
        process.stderr.write("qagent mcp: set QAGENT_AGENT_ID to the agent this server speaks for.\n");
        return 1;
    }
    const token = (env.QAGENT_TOKEN ?? env.AGENT_TOKEN ?? "").trim() || null;
    const bus = Bus.open({ dbPath: context.dbPath });
    const options = { agentId, token, operator: args.operator, env, cwd: process.cwd() };
    try {
        const identity = identify(bus, agentId, token);
        if (args.operator && identity.authority !== "operator")
            throw new BusError("forbidden", "--operator needs the operator identity (QAGENT_AGENT_ID=operator or unset)");
    }
    catch (error) {
        process.stderr.write(`qagent mcp: ${renderError(error)}\n`);
        bus.close();
        return error instanceof BusError && (error.code === "unauthorized" || error.code === "forbidden") ? 3 : 1;
    }
    const { server, drain } = createBusServer(bus, options);
    const transport = new StdioServerTransport();
    await server.connect(transport);
    process.stderr.write(`qagent mcp: serving ${agentId} over stdio (${bus.dbPath})\n`);
    return new Promise((resolve) => {
        let stopping = false;
        const stop = () => {
            if (stopping)
                return;
            stopping = true;
            process.off("SIGINT", stop);
            process.off("SIGTERM", stop);
            process.stdin.off("end", stop);
            process.stdin.off("close", stop);
            void drain()
                .catch(() => undefined)
                .then(() => server.close().catch(() => undefined))
                .finally(() => {
                bus.close();
                resolve(0);
            });
        };
        process.once("SIGINT", stop);
        process.once("SIGTERM", stop);
        process.stdin.once("end", stop);
        process.stdin.once("close", stop);
    });
}
//# sourceMappingURL=server.js.map