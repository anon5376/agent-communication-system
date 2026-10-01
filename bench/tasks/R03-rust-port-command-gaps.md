---
id: R03
kind: research
role: research
title: Find the qagent CLI commands and MCP tools that the Rust port is missing
mini: false
truth: truth/R03.json
---

## Brief
This repository has a TypeScript implementation (this checkout) and a Rust port on the git ref `origin/rust-port` (crate under `rust/`, binaries `qagent`, `acs` and `acs-app`). Determine which CLI commands and MCP tools exist in the TypeScript build but are missing from the Rust port.

Scope and units:
- CLI: the command forms listed in the `qagent` usage text in `src/cli/main.ts`. The unit is `command`, or `command subcommand` for the grouped commands `agent`, `token` and `task` (for example `task claim`). Flags and positional arguments are not units: a command counts as present in Rust if a Rust binary dispatches it, even if some of its flags differ. Different forms of one subcommand (for example with different positional arguments) are the same unit.
- MCP: every tool name registered in `src/mcp/server.ts`, including tools registered only in some server mode. A tool counts as present in Rust if a Rust MCP server offers a tool with the same name.
- Judge the Rust side by what the built binaries actually do, not only by their help text. Build the port (`git worktree add <dir> origin/rust-port`, then `cargo build` in `<dir>/rust`), run the binaries' help, try the commands against a scratch bus database, and list the tools of any Rust MCP server. If you cannot build, derive from source and say so.

Name each item `cli:<command>` or `cli:<command> <subcommand>` (for example `cli:task claim`), or `mcp:<tool name>`. If nothing in a category is missing, say so in a claim.

## Acceptance
- `items` lists exactly the CLI units and MCP tools that exist in TypeScript and are missing in Rust, named as above.
- Each item has a claim with `path:line` evidence for the TypeScript definition and for the Rust side (where dispatch falls through or the name is absent), plus what running the Rust binary showed.
- Claims state the cargo build outcome, whether the Rust port has an MCP server and how its tool list was obtained, and any flag-level differences noticed but not counted as items.
