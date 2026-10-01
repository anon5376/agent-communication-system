---
id: I14
kind: implementation
role: implementation
title: Add a task tree endpoint to the dashboard
mini: false
scope: [src/dashboard/, dist/, tests/]
validator: validate/I14.mjs
---

## Brief
The dashboard shows tasks as flat lists although tasks have a `parent_id`. Add `GET /api/tasks/tree` to the dashboard server (`src/dashboard/server.ts`), behind the same session check as the other `/api/` routes. It returns JSON `{ "seq": <latest event seq>, "roots": [node, ...] }` where each node is `{ "id", "title", "state", "assignee", "reviewer", "role", "children": [node, ...] }`: tasks without a parent are the roots, ordered by id, and each task's children are the tasks whose `parent_id` is that task, ordered by id, nested to any depth. Include tasks in every state. Build it from the same core queries the rest of the server uses (at most 1000 tasks are returned by them; a task whose parent is outside the set becomes a root); add no dependency and no schema change.

Add a test in `tests/dashboard.test.ts` or a new test file that exercises the endpoint, including that it needs a signed-in session, and a plain-text rendering helper (indented lines `#id [state] title`) with its own unit test.

## Acceptance
- An unauthenticated request is refused; an authenticated one returns the tree described, with a three-level example nested correctly and node states equal to the task states.
- A test mentioning `/api/tasks/tree` exists and passes.
