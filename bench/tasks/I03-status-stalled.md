---
id: I03
kind: implementation
role: implementation
title: Show stalled claims and the operator's unread count in qagent status
mini: true
scope: [src/, dist/, tests/]
validator: validate/I03.mjs
---

## Brief
`qagent status` does not show which claimed tasks have gone quiet, or how much mail is waiting for the operator, so an unattended run hides its own stalls. The bus already has `Bus.stalledTasks(stallMs)` and `qagent task stalled`.

Extend `qagent status` so that its `--json` output has two new top-level keys: `stalled`, an array of the tasks that `stalledTasks` reports with the default 60-minute window (each element is the task object, with its `id`), and `operatorUnread`, the integer count of messages addressed to the operator that it has not read. Make the text output of `qagent status` print a "stalled" line listing the task ids (and "none" when there are none) and the unread count. Add a test.

## Acceptance
- `qagent status --json` contains `stalled` (array, empty for a fresh claim, containing a claim whose last activity is three hours old) and `operatorUnread` (integer, equal to the number of unread messages for the operator).
- `qagent status` prints stalled task ids and the unread count.
- A test covers both keys.
