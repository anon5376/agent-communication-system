---
id: I10
kind: implementation
role: implementation
title: Make qagent status report every open task instead of silently stopping at 200
mini: true
scope: [src/, dist/, tests/]
validator: validate/I10.mjs
---

## Brief
`Bus.status()` builds `openTasks` with `listTasks({ limit: 200 })`, so a bus with more than 200 unfinished tasks reports only the first 200 and does not say so. An operator looking at `qagent status` is told the queue is shorter than it is.

Make `status` report the truth. Add `openTaskCount`, the number of tasks not in a closed state (`accepted`, `failed`, `cancelled`), to the status result and to `qagent status --json`, and make `openTasks` list all of them, with no hidden cap. In the text output, print the true count; if the text list is shortened for readability, it must say how many more there are ("... and N more"). Add a test with more than 200 open tasks.

## Acceptance
- With 250 open tasks, `qagent status --json` has `openTaskCount: 250` and an `openTasks` array of length 250.
- The text output shows 250.
- The test exists and passes.
