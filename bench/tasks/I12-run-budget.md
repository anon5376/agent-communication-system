---
id: I12
kind: implementation
role: implementation
title: Stop starting turns once the run cost cap is reached
mini: true
scope: [src/, dist/, tests/]
validator: validate/I12.mjs
---

## Brief
`constraints.optionalApiCostBudgetUSD` is declared in `src/config.ts` (default `null`) and nothing uses it. Make it a real run budget enforced by the supervisor (`src/supervisor.ts`).

Before the supervisor starts a model turn, compute the cost reported by all supervised turns so far in this bus home (sum `costUSD` over the per-agent session files in `<home>/sessions/`). If the cap is set and the total is at or above it, do not start the turn: write a `budget_exceeded` event to the bus (through the core library; include the cap and the amount spent in its data), log the reason, and keep waiting without running the harness (do not consume the mail or claim the task; back off for one wait interval before checking again) (so work that is already in flight finishes, and no new turn begins). Write the event once per supervisor, not once per wake. A task whose turn was not started stays as it was. With the cap `null`, behaviour is unchanged. Add a test using the command adapter or the fake harness with a reported cost.

## Acceptance
- With a cap of 0.01 and a harness reporting 0.02 per turn, the first task is processed, a `budget_exceeded` event appears, and a second task is not started.
- With no cap, nothing changes.
