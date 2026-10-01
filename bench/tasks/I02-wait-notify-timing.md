---
id: I02
kind: implementation
role: implementation
title: Make the wait-notify latency tests robust against slow machines
mini: false
scope: [tests/wait-notify.test.ts]
validator: validate/I02.mjs
---

## Brief
`tests/wait-notify.test.ts` contains hard wall-clock assertions such as `assert.ok(latency < 500, ...)`. On a loaded or single-core machine they fail without any regression in the code, so a red run proves nothing.

Keep what the tests are for (a waiter wakes promptly after a message rather than at the polling ceiling) but stop asserting bare numeric bounds that include process-spawn noise. Measure from the moment the message row is committed to the moment the waiter resolves where that is possible in-process; for cross-process cases take several samples and assert on the best one against a bound derived from the configured poll interval plus a generous constant, printing all samples with `t.diagnostic`. Add one deterministic test for the property itself: with `fsWatch: false` and a controlled clock or poll interval, a waiter wakes within one poll interval of a write, with no wall-clock sleeps.

Do not skip, disable, quarantine or delete any test.

## Acceptance
- No assertion in the file compares a latency to a bare number literal.
- Every test that existed before is still present and enabled, and the file passes repeatedly (20 runs in a row) on a single busy CPU core.
- The new deterministic wake-up test exists.
