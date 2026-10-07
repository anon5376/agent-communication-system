---
id: R04
kind: research
role: research
title: Find where the dashboard re-reads the bus database while the bus is idle, and measure how often
mini: false
truth: truth/R04.json
---

## Brief
The repository is a local coordination bus over one SQLite file, with an optional web dashboard (`qagent dashboard`, code in `src/dashboard/`). Answer two questions about the code at this commit.

1. Where does the dashboard process read the bus database while the bus is idle? "Idle" means: the dashboard is running, one signed-in browser tab has the live update stream open, and no process writes to the database. Consider `src/dashboard/` and the change-watching code it uses (`src/core/changes.ts`, `src/notify/`). Include only code that actually reads the database repeatedly in that idle state. Do not include code that runs only on page load, at startup, on sign-in, after a write, or that never touches the database. If a module in that scope is not used by the dashboard, say so in a claim and leave it out of the items.
   - One item per file and function (or named timer) that drives the repeated reads. Name the function that holds the loop or timer, not a helper that runs a single statement.
   - Name each item `<path>:<Class.method>` or `<path>:<function>`, with the path relative to the repository root (for example `src/foo/bar.ts:Widget.refresh`).
2. How often? Measure it. Start the dashboard on a throwaway bus with its default settings (see how `tests/dashboard.test.ts` starts it with `startDashboard({ dbPath, port: 0, ... })` from the committed `dist/`), sign in and open one live stream the way that test does, then wait 60 seconds with no writes. Count the database statement executions the dashboard makes in that window. Report the total, which statements they were, the steady-state interval once the rate settles, and how long it takes to settle. Use a method you can show: for example wrap the `node:sqlite` statement methods to count executions, or read the dashboard's own counters, and state which you used.

The `dist/` directory is committed, so no build is needed. Do not run `npm install`.

## Acceptance
- `items` lists every function or timer in the stated scope that drives repeated idle database reads, named `<path>:<Class.method>` or `<path>:<function>`, and nothing that only runs on load, startup or writes.
- Each item has a claim citing `path:line` for the loop or timer and the read it triggers.
- A claim gives the measured count of statement executions in 60 s with one open stream, broken down by statement, with the command that was run and its real output.
- A claim gives the steady-state read interval and the time it takes to reach it, backed by the measurement (timestamps) and the code that sets the interval.
- Anything not measured is listed in `unresolved`.
