# ACS benchmark runner

Runs the same task set against a roster of agents on a fresh bus and a fresh clone, then scores the result with frozen validators and truth files. The goal is to compare arms (for example ACS as shipped against ACS with a change) on acceptance, first-round acceptance, defect escape, cross-family review, stuck time, human touches, cost and coordination overhead.

Plain Node ESM, no dependencies. It reads the ACS bus read-only and starts `qagent supervise --roster` as an ordinary user would.

## Layout

| Path | Contents |
| --- | --- |
| `MANIFEST.json` | series name, `base_sha`, budgets, roster, arms |
| `tasks/` | 14 implementation tasks (`I01`–`I14`) and 6 research tasks (`R01`–`R06`), each with a `Brief` and `Acceptance` |
| `validate/` | frozen validators for the implementation tasks, run on the integrated result |
| `truth/` | frozen answer sets for the research tasks, and `derive/` scripts that produced them |
| `preamble/` | the text prepended to every task brief |
| `fixtures/` | scripted fake-harness verdicts and the golden metrics used by CI |
| `run.mjs`, `extract.mjs`, `lib/` | runner, metrics extraction, report |

The mini set is `I01 I03 I05 I10 I12 R01 R02 R05` (5 implementation, 3 research). The full set is the endurance run.

## Fake run (no credentials)

```sh
npm ci && npm run build
node bench/run.mjs baseline --fake --mini --out /tmp/bench-fake --script bench/fixtures/fake-mini.script.json
```

This uses the fake harness and a scratch repository, finishes in a few seconds, and exercises task creation, dispatch, review, integration, validators, scoring and the report. CI runs it (`tests/bench.test.ts`) and compares the result with `fixtures/fake-mini.metrics.json`. Without `--script` every task is accepted in round 1.

## Real run

Prerequisites:

- Node 22.13 or newer, `git`, network access to clone the repository.
- The `claude` and `codex` CLIs installed and logged in with subscription auth. API keys in the environment are stripped from the agents unless `QAGENT_ALLOW_API_KEY` is set, so a stray key does not bill silently.
- A built checkout of ACS (`npm ci && npm run build`); the runner uses its `dist/qagent.js`.
- Truth reconciliation (below).

Start:

```sh
node bench/run.mjs baseline --mini      # smoke run, caps 12 USD / 150 min
node bench/run.mjs baseline             # endurance run, caps 60 USD / 600 min
```

Useful flags:

- `--rep N` repetition number (default 1), `--out DIR` output root (default `bench-runs/`, git-ignored).
- `--source REPO` clone from a local path or another URL instead of `manifest.repo`.
- `--no-install` skip `npm ci` in the clone, `--no-validate` skip the post-run validators.
- `--allow-unreconciled-truth` see below.

The runner clones the repository at `base_sha` into `<out>/<series>/<arm>-<rep>-<stamp>/workdir`, creates the bus and every task in `home/`, starts the supervisors, and ends the run when every task is terminal, the run USD cap or wall cap is reached, or a single task exceeds 45 minutes. It refuses to start if `base_sha` already contains `bench/`, since agents would then be able to read the validators.

Afterwards it integrates each accepted task branch (`qagent/task-<id>-*`) into `bench/integrated` (conflicts and out-of-scope edits are recorded, `dist/`-only conflicts are resolved with the base side and rebuilt), builds with `tsc`, runs the validators, writes `metrics.json`, `audit-sheet.md` and `report.md`.

### Truth reconciliation

The research truth files were each derived once, by one author. The spec asks for a second, independent derivation before the first real run. A real run refuses to start until every research truth file used has `"reconciled": true`. Pass `--allow-unreconciled-truth` to run anyway; the report then says research precision and recall are provisional. Treat the flag as a deliberate decision, not a default.

### Dispatch

A supervisor only wakes a worker for tasks named in mail, so after its first task a worker would not learn about the rest. The runner therefore feeds work itself: from the manager identity it mails `[TASK #n]` to one idle worker of the right role at a time, in every arm. Reviews are done by the real manager agent; in fake mode a script supplies the verdicts.

### Between runs

```sh
node bench/run.mjs touch <run-dir> "restarted res-a by hand"   # log a manual action (human touches)
node bench/run.mjs invalidate <run-dir> "network outage"       # mark infrastructure faults; the run stays listed
node bench/run.mjs validate <run-dir>                          # redo integration, build and validators
node bench/extract.mjs <run-dir>... [--out report.md]          # metrics and report for one or more runs
```

Invalid runs are listed in the report and excluded from the comparison, never silently dropped. If `bench/validate` or `bench/truth` changes during a run (hash check), the run is marked invalid.

### Blind audit

`audit-sheet.md` holds a sample of tasks with the arm and reviewer removed. Grade each as `correct`, `partly` or `wrong` in `<run-dir>/grades.json`, for example `{ "I01": "correct" }`, then re-run `node bench/extract.mjs <run-dir>`. The report shows agreement between the grades and the validators.

### Comparing arms

Interleave arms (A, B, A, B) with `--rep`, so drift in vendor behaviour hits both. To add an arm, add an entry under `arms` in `MANIFEST.json`. An arm may set `qagent` (path to another build of the CLI), `env` and `configOverrides` (merged into the generated bus config). Pass several run directories to `extract.mjs` and the report groups them by arm.

## Known limits

- The runner has not been run against real vendors by its author. Everything but the harness is exercised in CI.
- The USD cap relies on costs the harnesses report in their session files, per agent rather than per task. "Queued minutes" is an upper bound.
- Validators and truth live in the same checkout the runner is started from, not in the agents' clone, but agents run as the same OS user and are not sandboxed from them.
- `I14` is not in the mini set although the spec table marks it: the spec asks for 5 implementation tasks and lists 6.
- Research reports are JSON in the task result `details`: `{"items": [...], "claims": [{"text", "evidence"}], "unresolved": [...]}`. Evidence that is a `path:line` is checked against the base commit; links are not fetched.
