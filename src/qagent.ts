#!/usr/bin/env node
/** qagent v2 bin entry. Coordination goes straight to the SQLite file; nothing listens on a port. */

// Node 22 emits "SQLite is an experimental feature" whenever node:sqlite loads, which put the same two
// lines of noise on every command. Drop that one warning before the bus loads node:sqlite (hence the
// dynamic import below); every other warning still prints.
const emitWarning = process.emitWarning.bind(process) as (warning: string | Error, ...rest: unknown[]) => void;
process.emitWarning = ((warning: string | Error, ...rest: unknown[]) => {
  const message = typeof warning === "string" ? warning : warning.message;
  if (message.startsWith("SQLite is an experimental feature")) return;
  emitWarning(warning, ...rest);
}) as typeof process.emitWarning;

const { main } = await import("./cli/main.js");

main(process.argv.slice(2)).then(
  (code) => { process.exitCode = code; },
  (error: unknown) => {
    process.stderr.write(`qagent: ${error instanceof Error ? error.stack ?? error.message : String(error)}\n`);
    process.exitCode = 1;
  },
);
