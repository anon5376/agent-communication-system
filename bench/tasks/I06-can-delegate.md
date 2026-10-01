---
id: I06
kind: implementation
role: implementation
title: Enforce canDelegate=false when a worker creates a task
mini: false
scope: [src/core/, dist/, tests/]
validator: validate/I06.mjs
---

## Brief
Every identity carries `permissions.canDelegate` (workers: false, managers: true; see `src/core/identity.ts`), and `qagent whoami` and the MCP prompt tell a worker it may not delegate, yet `Bus.createTask` never checks it. A worker can create tasks for any agent.

Make `Bus.createTask` refuse a non-operator actor whose `permissions.canDelegate` is false. The error is a `BusError` with code `forbidden` and a message that names `canDelegate`, says which agent was refused, and says who may create tasks. Managers and the operator keep working. Update any existing test that relied on a worker creating tasks, and add a test for the refusal.

## Acceptance
- A worker running `qagent task add ... --to <other>` (or with no `--to`) fails with a message containing `canDelegate`, and no task is created.
- A manager and the operator still create tasks.
- The existing suite still passes.
