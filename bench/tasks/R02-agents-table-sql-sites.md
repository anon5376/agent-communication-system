---
id: R02
kind: research
role: research
title: Find every function in the TypeScript and Rust code that runs SQL against the agents table
mini: true
truth: truth/R02.json
---

## Brief
We plan to add a nullable `family` column to the `agents` table of the bus database. Before doing so, list every code path that reads or writes `agents` rows, in both implementations:
- TypeScript: everything under `src/` in this checkout.
- Rust: everything under `rust/src/` on the git ref `origin/rust-port` of this repository (read it with `git show origin/rust-port:<path>` or a worktree).

An item is one function or method that itself executes SQL touching the bus `agents` table: SELECT, INSERT, UPDATE or DELETE, including statements that read `agents` through a JOIN, a subquery or `INSERT ... SELECT`. Rules:
- The bus `agents` table is the one created by the schema in `src/core/db.ts` (TypeScript) and `rust/src/db.rs` (Rust). SQL that queries a table named `agents` in some other SQLite database does not count.
- The schema/DDL itself is not an item.
- A function that only receives or maps rows produced elsewhere is not an item; the function whose body contains (and runs) the SQL is.
- Several statements in one function make one item. Check for SQL assembled at runtime as well as literal SQL.
- Code under `tests/` or `rust/tests/` is out of scope.

Name each item `<path>:<function-or-method name>`, using the path as in the repository and the bare function or method name, without class or impl prefix, for example `src/core/bus.ts:someMethod` or `rust/src/bus.rs:some_method`.

## Acceptance
- `items` lists every qualifying TypeScript and Rust function, named as above, and nothing else.
- Each item has a claim citing `path:line` of the SQL statement inside that function, saying whether it reads, writes, or both.
- Any function judged borderline (for example SQL against a table named `agents` in a different database) is explained in a claim or listed in `unresolved`.
