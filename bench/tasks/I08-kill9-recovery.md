---
id: I08
kind: implementation
role: implementation
title: Add a kill -9 recovery test for supervised work
mini: false
scope: [tests/]
validator: validate/I08.mjs
---

## Brief
Nothing proves that a supervisor killed with `kill -9` mid-turn leaves the bus in a state that recovers. Add one test file, `tests/lifecycle-kill.test.ts`, using the fake harness (see `tests/supervisor-v2.test.ts` for the fixtures: `bus-cli` mode and `hang` mode):

- start a supervisor for a fake agent on a task and let the turn begin;
- `SIGKILL` the supervisor and the harness process group while the turn is running;
- assert the task shows as stalled once the window passes (use a very short window), that `supervise --auto-requeue-min` (or `qagent task requeue`) returns it to the pool, that a second supervisor completes it, and that no message or note written before the kill is lost.

The test title must mention `kill -9`. Do not skip it, and keep it reliable: wait on conditions, not fixed sleeps.

## Acceptance
- The test file exists, compiles with `npm run test:compile`, and passes when run alone.
- It performs the steps above against real processes, not mocks.
