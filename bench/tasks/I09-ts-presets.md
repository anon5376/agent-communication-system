---
id: I09
kind: implementation
role: implementation
title: Port the Rust TUI preset charters to TypeScript with a preset list command
mini: false
scope: [src/, dist/, tests/, presets/]
validator: validate/I09.mjs
---

## Brief
The Rust TUI (`rust-port` branch, `rust/src/app.rs`) hard-codes four role presets in a `PRESETS` table with charters (`planner`, `lead`, `worker-hard`, `worker-easy`: id, role, authority, blurb, charter). The TypeScript build has no equivalent.

Fetch the branch read-only (`git fetch origin rust-port`, then `git show origin/rust-port:rust/src/app.rs`); do not change anything under `rust/`. Port the four presets to TypeScript as data: a `src/presets.ts` (or JSON files loaded by it) holding id, role, authority, blurb and charter, with a loader. Keep the charters' text as in the Rust source apart from line wrapping. Add `qagent preset list` (text: one line per preset with id, role, authority and blurb) and `qagent preset list --json` (an array of objects with `id`, `role`, `authority`, `blurb`, `charter`). Add `qagent preset show <id>` printing the charter. Add a test.

## Acceptance
- `qagent preset list --json` returns four presets with the ids, roles and authorities of the Rust table and charters whose text matches the Rust ones.
- `qagent preset show <id>` prints one charter; an unknown id fails with a clear error.
- `rust/` is untouched.
