#!/usr/bin/env node
import { spawn, spawnSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
function arg(name) {
    const index = process.argv.indexOf(name);
    return index >= 0 ? String(process.argv[index + 1] ?? "") : "";
}
const mode = arg("--mode") || "success";
const agent = arg("--agent") || "fake";
const prompt = arg("--prompt");
const existingSession = arg("--session");
const stateFile = process.env.FAKE_HARNESS_STATE;
if (mode === "malformed") {
    process.stdout.write("this is deliberately malformed provider output\n");
    process.exit(0);
}
if (mode === "fail") {
    process.stderr.write("deterministic fake harness failure\n");
    process.exit(23);
}
if (mode === "fail-once") {
    let seen = false;
    if (stateFile) {
        try {
            seen = readFileSync(stateFile, "utf8").trim() === "failed";
        }
        catch { /* first attempt */ }
        if (!seen)
            writeFileSync(stateFile, "failed");
    }
    if (!seen) {
        process.stderr.write("deterministic first-attempt failure\n");
        process.exit(24);
    }
}
/**
 * bus-cli: act like an MCP-capable agent without speaking MCP. The v2 supervisor hands the
 * harness `qagent mcp` in QAGENT_MCP_COMMAND, with the agent's own identity in its env; this
 * mode drops the trailing `mcp` and runs `qagent task claim/note/submit` on that command line.
 */
function busCli() {
    const raw = process.env.QAGENT_MCP_COMMAND;
    if (!raw) {
        process.stderr.write("bus-cli mode needs QAGENT_MCP_COMMAND\n");
        process.exit(25);
    }
    const launch = JSON.parse(raw);
    const base = launch.args.at(-1) === "mcp" ? launch.args.slice(0, -1) : launch.args;
    const env = { ...process.env, ...(launch.env ?? {}) };
    const qagent = (args) => {
        const result = spawnSync(launch.command, [...base, ...args, "--json"], { env, encoding: "utf8" });
        if (result.status !== 0) {
            process.stderr.write(`qagent ${args.join(" ")} failed (${result.status}): ${result.stderr}\n`);
            process.exit(26);
        }
        return JSON.parse(result.stdout);
    };
    // Every task the brief names ([TASK #n] assignments, [CHANGES #n r2] review feedback), else any claimable one.
    // Also the task a worktree-isolating supervisor claimed for this turn ("Task #n is claimed for you").
    const wanted = [...new Set([...prompt.matchAll(/\[(?:TASK|CHANGES) #(\d+)|^Task #(\d+) is claimed for you/gm)].map((match) => match[1] ?? match[2]))];
    const reportDir = process.env.FAKE_HARNESS_REPORTS;
    const done = [];
    for (const target of wanted.length ? wanted : [undefined]) {
        // A supervisor that isolates tasks in worktrees claims before the turn; claim only what is not already ours.
        const shown = target ? qagent(["task", "show", target]) : null;
        const claimed = shown && shown.state === "claimed" && shown.assignee === agent ? shown : qagent(["task", "claim", ...(target ? [target] : [])]);
        const id = String(claimed.id);
        qagent(["task", "note", id, `fake ${agent} started task #${id}`]);
        const submit = ["task", "submit", id, "--summary", `fake ${agent} submitted task #${id} through qagent`];
        // With FAKE_HARNESS_REPORTS set, a canned report named after the task title's key (e.g. R01.txt) is submitted as details.
        const key = String(claimed.title ?? "").match(/^([A-Za-z]+\d+)/)?.[1];
        if (reportDir && key) {
            try {
                submit.push("--details", readFileSync(`${reportDir}/${key}.txt`, "utf8"));
            }
            catch { /* no canned report for this task */ }
        }
        qagent(submit);
        done.push(Number(id));
    }
    return done;
}
if (mode === "hang") {
    // A grandchild in the same process group, both pids recorded, and no exit:
    // only a process-group kill clears both.
    const grandchild = spawn(process.execPath, ["-e", "setInterval(() => {}, 1000)"], { stdio: "ignore" });
    if (stateFile)
        writeFileSync(stateFile, JSON.stringify({ pid: process.pid, grandchild: grandchild.pid }));
    setInterval(() => { }, 1000);
}
else {
    const busTasks = mode === "bus-cli" ? busCli() : null;
    const inputTokens = Math.max(1, Math.ceil(prompt.length / 4));
    const outputTokens = 24;
    process.stdout.write(JSON.stringify({
        sessionId: existingSession || `fake-${agent}-session`,
        result: `fake ${agent} completed the assigned work`,
        usage: { inputTokens, outputTokens, totalTokens: inputTokens + outputTokens, costUSD: 0 },
        changedFiles: [],
        validation: [{ passed: true, summary: "deterministic fake harness completed" }],
        ...(busTasks === null ? {} : { busTask: busTasks[0], busTasks }),
    }) + "\n");
}
//# sourceMappingURL=fake-harness.js.map