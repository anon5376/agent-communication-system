/**
 * `qagent dashboard` entry, loaded lazily by src/cli/main.ts.
 *
 *   qagent dashboard [--port N]        serve the page on 127.0.0.1 (default 11511)
 *   qagent dashboard link [--port N]   print a fresh sign-in address for a running dashboard
 *
 * Both read the operator token file next to the database. The token is checked
 * here and by POST /login; the browser only ever sees a single-use ticket.
 */
import { homeFor, openDatabase } from "../core/db.js";
import { identityForToken, operatorTokenPath, readTokenFile } from "../core/identity.js";
import { OPERATOR_ID } from "../core/types.js";
import { DEFAULT_PORT, HOST, startDashboard } from "./server.js";
const USAGE = `usage: qagent dashboard [--port N]        serve on ${HOST} (default ${DEFAULT_PORT}, or QAGENT_DASHBOARD_PORT)
       qagent dashboard link [--port N]   print a fresh single-use sign-in address for a running dashboard
`;
function parse(argv, env) {
    let command = "serve";
    let portText = env.QAGENT_DASHBOARD_PORT ?? "";
    for (let index = 0; index < argv.length; index += 1) {
        const arg = argv[index];
        if (arg === "--db") {
            index += 1;
            continue;
        }
        if (arg.startsWith("--db="))
            continue;
        if (arg === "--help" || arg === "-h" || arg === "help") {
            command = "help";
            continue;
        }
        if (arg === "--port") {
            portText = argv[index += 1] ?? "";
            continue;
        }
        if (arg.startsWith("--port=")) {
            portText = arg.slice("--port=".length);
            continue;
        }
        if (arg === "link") {
            command = "link";
            continue;
        }
        throw new Error(`unknown argument: ${arg}`);
    }
    const port = portText ? Number(portText) : DEFAULT_PORT;
    if (!Number.isInteger(port) || port < 1 || port > 65535)
        throw new Error(`invalid port: ${portText}`);
    return { command, port };
}
function operatorToken(dbPath) {
    const path = operatorTokenPath(homeFor(dbPath));
    const token = readTokenFile(path);
    if (!token)
        throw new Error(`no operator token at ${path} (run \`qagent init\`)`);
    return token;
}
async function link(dbPath, port) {
    const token = operatorToken(dbPath);
    let response;
    try {
        response = await fetch(`http://${HOST}:${port}/login`, {
            method: "POST",
            headers: { "content-type": "application/json" },
            body: JSON.stringify({ operatorToken: token }),
        });
    }
    catch {
        process.stderr.write(`qagent: no dashboard on ${HOST}:${port}; start it with \`qagent dashboard\`\n`);
        return 1;
    }
    const body = await response.json().catch(() => ({}));
    if (!response.ok || !body.url) {
        process.stderr.write(`qagent: dashboard refused the operator token: ${body.error ?? response.status}\n`);
        return 3;
    }
    process.stdout.write(`${body.url}\n`);
    return 0;
}
async function serve(dbPath, port) {
    const token = operatorToken(dbPath);
    const db = openDatabase(dbPath, { readOnly: true });
    try {
        identityForToken(db, OPERATOR_ID, token);
    }
    finally {
        db.close();
    }
    const dashboard = await startDashboard({ dbPath, port });
    process.stdout.write(`qagent dashboard on ${dashboard.url} (database ${dbPath})\n`);
    process.stdout.write(`sign in (single use, 5 minutes): ${dashboard.signInUrl()}\n`);
    process.stdout.write("later links: qagent dashboard link" + (port === DEFAULT_PORT ? "" : ` --port ${port}`) + "\n");
    await new Promise((resolve) => {
        process.once("SIGINT", () => resolve());
        process.once("SIGTERM", () => resolve());
    });
    await dashboard.close();
    return 0;
}
export async function main(argv, context) {
    let args;
    try {
        args = parse(argv, process.env);
    }
    catch (error) {
        process.stderr.write(`qagent: ${error.message}\n${USAGE}`);
        return 1;
    }
    if (args.command === "help") {
        process.stdout.write(USAGE);
        return 0;
    }
    try {
        return args.command === "link" ? await link(context.dbPath, args.port) : await serve(context.dbPath, args.port);
    }
    catch (error) {
        const message = error.message;
        process.stderr.write(`qagent: ${message}\n`);
        return /token/.test(message) ? 3 : 1;
    }
}
//# sourceMappingURL=entry.js.map