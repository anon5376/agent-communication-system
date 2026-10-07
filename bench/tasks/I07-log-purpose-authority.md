---
id: I07
kind: implementation
role: implementation
title: Record purpose and authority on task events and show them in the log
mini: false
scope: [src/, dist/, tests/]
validator: validate/I07.mjs
---

## Brief
The event log records who did something and what kind of event it was, but not why, nor with what authority. Add both for task events, without changing the database schema (use each event's `data_json`).

1. `qagent task claim|note|submit|review|cancel|requeue` accept `--why TEXT`: a short free-text purpose, bounded in length like other text fields, stored in the event the call writes.
2. Every task event records the actor's `authority` (`operator`, `manager`, `worker`, or `system` for events written by the bus itself).
3. `qagent log` gains `--task N`, which prints only events whose entity is task N. With `--json` it prints one JSON object per line as before, but each task event object has two new top-level properties: `authority` (string) and `purpose` (the `--why` text, or `null`). Text output shows the purpose when there is one.
4. Add a test.

## Acceptance
- After `task claim N --why "picked up first"`, `qagent log --task N --json` has a `task_claimed` line with `"purpose":"picked up first"` and `"authority":"worker"`.
- Events written without `--why` carry `purpose: null`; every task event has a non-empty `authority`.
- `log --task N` prints no event of another task.
