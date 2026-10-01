---
id: I13
kind: implementation
role: implementation
title: Add qagent trust to count accepted review verdicts per model and task role
mini: false
scope: [src/, dist/, tests/]
validator: validate/I13.mjs
---

## Brief
Review verdicts are on the bus but nowhere summarised. Add `qagent trust`, which reads the events and tasks and prints, for each pair of (the assignee's `model` as stored on the agent, the task's `role`), how many review verdicts were accepted out of how many were given.

A verdict is one review decision: each `task_accepted` event, each `task_changes_requested` event, and each `task_failed` event whose data reason is `review retry limit exceeded` (the third rejection of a task). Count a verdict against the agent that was the task's assignee when the verdict was given, in the task's role. `qagent trust --json` prints an array of objects `{ "model": "...", "role": "...", "accepted": N, "total": M }`, sorted by model then role; the text form prints a table with the same figures as `accepted/total`. Agents with no model or tasks with no role use an empty string. Add a test with a hand-computed bus.

## Acceptance
- On a bus where agent a1 (model m1) had an impl task accepted first time, an impl task revised once then accepted, and a research task accepted, and agent a2 (model m2) had an impl task accepted and a research task rejected three times, the output is m1/impl 2/3, m1/research 1/1, m2/impl 1/1, m2/research 0/3.
- Tasks with no verdict yet are not counted.
