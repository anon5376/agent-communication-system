#!/usr/bin/env node
/** qagent v2 bin entry. Coordination goes straight to the SQLite file; nothing listens on a port. */
// Node 22 emits "SQLite is an experimental feature" whenever node:sqlite loads, which put the same two
// lines of noise on every command. Drop that one warning before the bus loads node:sqlite (hence the
// dynamic import below); every other warning still prints.
const emitWarning = process.emitWarning.bind(process);
process.emitWarning = ((warning, ...rest) => {
    const message = typeof warning === "string" ? warning : warning.message;
    if (message.startsWith("SQLite is an experimental feature"))
        return;
    emitWarning(warning, ...rest);
});
const { main } = await import("./cli/main.js");
main(process.argv.slice(2)).then((code) => { process.exitCode = code; }, (error) => {
    process.stderr.write(`qagent: ${error instanceof Error ? error.stack ?? error.message : String(error)}\n`);
    process.exitCode = 1;
});
export {};
//# sourceMappingURL=qagent.js.map