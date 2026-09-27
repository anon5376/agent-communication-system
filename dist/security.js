import { createHash, randomBytes } from "node:crypto";
import { chmodSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { homeFor, resolveDbPath } from "./core/db.js";
/** The bus home: the directory holding bus.db (QAGENT_BUS_DB, QAGENT_HOME, or ~/.agent-bus). */
const BUS_HOME = homeFor(resolveDbPath());
export const TOKEN_DIR = join(BUS_HOME, "tokens");
export const OPERATOR_TOKEN_PATH = join(BUS_HOME, "operator.token");
export function hashToken(token) {
    return createHash("sha256").update(token, "utf8").digest("hex");
}
export function createBearerToken() {
    return randomBytes(32).toString("base64url");
}
export function agentTokenPath(agentId) {
    if (!/^[a-zA-Z0-9._-]+$/.test(agentId))
        throw new Error(`unsafe agent id: ${agentId}`);
    return join(TOKEN_DIR, `${agentId}.token`);
}
export function ensurePrivateDirectories() {
    mkdirSync(BUS_HOME, { recursive: true, mode: 0o700 });
    mkdirSync(TOKEN_DIR, { recursive: true, mode: 0o700 });
    try {
        chmodSync(BUS_HOME, 0o700);
    }
    catch { /* best effort on non-POSIX filesystems */ }
    try {
        chmodSync(TOKEN_DIR, 0o700);
    }
    catch { /* best effort */ }
}
export function writePrivateToken(path, token) {
    ensurePrivateDirectories();
    writeFileSync(path, `${token}\n`, { mode: 0o600 });
    try {
        chmodSync(path, 0o600);
    }
    catch { /* best effort */ }
}
export function readTokenFile(path) {
    try {
        const token = readFileSync(path, "utf8").trim();
        return token || null;
    }
    catch {
        return null;
    }
}
//# sourceMappingURL=security.js.map