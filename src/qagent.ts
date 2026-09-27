#!/usr/bin/env node
/** qagent v2 bin entry. Coordination goes straight to the SQLite file; nothing listens on a port. */
import { main } from "./cli/main.js";

main(process.argv.slice(2)).then(
  (code) => { process.exitCode = code; },
  (error: unknown) => {
    process.stderr.write(`qagent: ${error instanceof Error ? error.stack ?? error.message : String(error)}\n`);
    process.exitCode = 1;
  },
);
