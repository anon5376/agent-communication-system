---
id: I05
kind: implementation
role: implementation
title: Enforce maxConcurrentTasks when an agent claims a task
mini: true
scope: [src/core/, dist/, tests/]
validator: validate/I05.mjs
---

## Brief
`constraints.maxConcurrentTasks` (default 4) is declared in `src/config.ts` and validated, but nothing enforces it: one agent can hold any number of claimed tasks.

Enforce it in the core claim path (`Bus.claimTask`). `src/core/` must not import from outside `core/`, so the limit lives in core: use a default of 4 held tasks per agent, overridable by a `maxConcurrentTasks` number in the identity's `permissions_json` when present. The operator is exempt. An agent "holds" a task while it is in state `claimed`; submitted tasks no longer count. A claim over the limit must fail with a `BusError` of code `conflict` whose message contains the word `maxConcurrentTasks` and says what the limit is and how many tasks the agent holds. The refused task must stay as it was. Add tests.

## Acceptance
- An agent can claim 4 tasks; the 5th claim fails with a `conflict` naming `maxConcurrentTasks`, and that task remains open.
- After the agent submits one of its tasks, the 5th claim succeeds.
- Only the operator identity is exempt; managers using their own identity are limited like workers.
