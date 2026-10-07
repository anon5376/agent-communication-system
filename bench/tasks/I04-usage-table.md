---
id: I04
kind: implementation
role: implementation
title: Record supervised turn usage in the usage table
mini: false
scope: [src/, dist/, tests/]
validator: validate/I04.mjs
---

## Brief
The bus schema has a `usage` table (`agent_id, day, turns, input_tokens, output_tokens, cost_usd, latency_ms`, primary key `agent_id, day`) that nothing writes. The supervisor keeps only cumulative per-agent totals in a JSON file under `<home>/sessions/`, so spend cannot be read from the bus.

After each completed supervised turn, add the turn's usage to the row for that agent and the current UTC day (`YYYY-MM-DD`), creating the row if needed: increment `turns` by one and add the turn's input tokens, output tokens, cost and duration. Do it through the core library (a method on `Bus` that the supervisor calls), not by opening the database from the supervisor. Do not change the schema. Keep the existing session file working. Add a test using the fake harness.

## Acceptance
- After one supervised turn with the fake harness, `usage` has exactly one row for that agent with `turns = 1`, positive input and output tokens, and `day` in `YYYY-MM-DD` form.
- A second turn on the same day updates that row instead of adding one.
- The schema (`src/core/db.ts`) is unchanged.
