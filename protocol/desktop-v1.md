# acs-desktop protocol, version 1 (frozen)

The bridge between a native desktop app (ACS.app) and a local agent bus.
`acs-desktop` is a Rust binary shipped inside the app bundle next to `qagent`
(and optionally `aos`). It speaks once per invocation, like a CGI:

- **stdin**: exactly one JSON object, whole input.
- **stdout**: exactly one JSON object, one line. Nothing else is ever written
  to stdout; diagnostics go to stderr.
- **exit code**: `0` on success, nonzero on any error.

All keys are camelCase. No token, credential, or agent `meta` value ever
appears in a response. The bridge never shells out through a shell and never
interpolates paths or input into command lines.

## Request

```json
{
  "version": 1,
  "dbPath": "/absolute/path/to/bus.db",
  "action": "snapshot",
  "payload": {}
}
```

- `version` — must be `1`; anything else is an `invalid` error.
- `dbPath` — absolute path to the bus database. Required, absolute only.
- `action` — one of the actions below; anything else is an `invalid` error.
- `payload` — an object; may be omitted (defaults to `{}`).

## Response

```json
{ "ok": true, "data": <value> }
{ "ok": false, "error": { "code": "invalid", "message": "…" } }
```

`error.code` is one of `unauthorized`, `forbidden`, `not_found`, `invalid`,
`conflict` — the bus error codes. After any mutation the client re-fetches
`snapshot` rather than guessing the result.

## Database existence

`init`, `demo` and `detect` run without an existing database. Every other
action requires `dbPath` to be an existing file and fails `not_found`
otherwise — a mistyped path can never grow a new database.

An existing file is checked read-only before it is opened: an empty file, a
non-database, or a SQLite file without the core `agents`/`tasks`/`messages`
tables is not an agent bus and fails `invalid` — opening a bus runs schema
migrations, so an unrelated file at a mistyped path is never written into.

`detect` does not touch the bus at all (it scans the machine for provider
CLIs), so it also works before `init` during onboarding.

## Actions

### `snapshot` → data object

```json
{
  "dbPath": "/abs/bus.db",
  "simulated": false,
  "canOperate": true,
  "agents": [AgentRecord],
  "tasks": [TaskRecord],
  "messages": [MessageRecord],
  "truncated": false
}
```

- `simulated` — the bus is an `aos demo` sample; nothing on it is real.
- `canOperate` — the operator identity resolves (token file present and
  registered). Read-only: a snapshot never creates an operator.
- `agents` — `AgentRecord`, sorted by id.
- `tasks` — `TaskRecord`, newest id first, capped at 1000 (closed states
  included). `truncated` is true exactly when older tasks exist beyond the
  cap.
- `messages` — the latest 100 `MessageRecord`s, oldest-to-newest.

`AgentRecord`:

```json
{ "id": "lead", "role": "manager", "model": "claude-opus-4",
  "harness": "claude", "status": "idle", "lastSeenMs": 1759650000000,
  "running": true, "paused": false, "runtimeNote": null }
```

`runtimeNote` is optional, additive diagnostic text for the last failed supervised turn:
exit code, consecutive failure count, backoff and next eligible retry timestamp, or
crashing/operator-action guidance. It clears after a successful turn. A retry timestamp
does not promise execution if the agent is stopped, paused or blocked by its budget.

`running` means a live supervisor owns the agent's pid file. `paused` is the
agent's pause flag. Private fields (meta, tokens, parent, authority) are never
emitted.

`TaskRecord` (also embedded in `task` replies):

```json
{ "id": 12, "title": "…", "brief": "…", "acceptance": "…",
  "state": "open", "priority": "normal",
  "assignee": "impl-a", "reviewer": "operator", "project": "/abs/dir",
  "pathScopes": ["src"], "dependencies": [3],
  "updatedMs": 1759650000000, "createdMs": 1759640000000,
  "result": { "summary": "…", "details": "…", "changedFiles": ["…"],
              "artifacts": [], "validation": [{ "command": "…",
              "passed": true, "summary": "…" }], "completedMs": 0 } ,
  "review": { "reviewer": "operator", "accepted": true,
              "feedback": "…", "reviewedMs": 0 } }
```

`result`/`review` are `null` when absent; `assignee`, `reviewer`, `project`
are `null` when unset.

`MessageRecord`:

```json
{ "seq": 41, "tsMs": 1759650000000, "sender": "impl-a",
  "recipient": "operator", "subject": "…", "body": "…" }
```

`recipient` is `null` for broadcasts.

### `task` → flattened task detail

Payload `{ "id": 12 }`. Response: the `TaskRecord` fields plus

```json
{ "notes": [{ "id": 1, "author": "impl-a", "tsMs": 0, "body": "…" }],
  "messages": [MessageRecord] }
```

Unknown or non-positive `id` → `not_found`/`invalid`.

### `init` → `{ "message": "…" }`

Creates the bus (and the operator identity/token) at `dbPath`. Called only
when the operator asks for a new workspace. Idempotent on an existing bus
(`unchanged`/`adopted`/`rotated` outcomes are reported in the message); it
never resets or deletes data. Fails `conflict` when `dbPath` already exists
but is not an agent bus — init never writes over an unrelated file.

### `demo` → `{ "message": "…" }`

Seeds the simulated sample bus at `dbPath` via `aos::demo::seed`. Fails
`conflict` when `dbPath` already exists. The destination is marked SIMULATED;
`start` and `setup` refuse it forever after.

### `detect` → `[ProviderRecord]`

```json
[{ "id": "claude", "name": "Claude Code", "path": "/usr/local/bin/claude",
   "signIn": "found: ANTHROPIC_API_KEY is set", "requiresApproval": false }]
```

- `path` — `null` when no binary was found. A found binary is **not**
  authenticated.
- `signIn` — `"not installed"`, `"missing"`, `"found: <evidence>"`, or
  `"unknown: <why>"`. Detection reads files and environment only; it never
  runs a provider login or any paid call.
- `requiresApproval` — true when the provider's adapter runs tools without
  prompting, so it joins a crew only after explicit operator approval.

### `setup` → `{ "message": "…" }`

Writes `aos/crew.json` from detected providers and registers the crew on the
bus — only when the operator asks. `crew.json` is never overwritten
(`force=false`); existing custom configs and preset edits are kept. Starts
nothing.

### `start` → `{ "message": "…" }`

Payload `{ "ids": ["lead"], "workdir": "/abs/project", "confirmed": true }`.

Refuses (`invalid`) unless **all** hold:

- `confirmed` is exactly `true`;
- `workdir` exists, is a directory, and passes the worktree safety check
  (not `/`, not the home folder);
- the bus is not the simulated demo;
- `crew.json` loads and every `ids` entry is a configured, enabled member;
- a bundled `qagent` sits next to `acs-desktop` (or `ACS_DESKTOP_QAGENT`
  names it).

On those, the workdir is recorded as trusted and one supervisor per id is
spawned via the bundled `qagent` (`qagent --db <dbPath> supervise <id>
<workdir> --config <crew.json>`), which applies the crew's enforcement policy
and **fails closed** if it cannot. If a bundled `aos` is present
(`ACS_DESKTOP_AOS` overrides), a crash watcher is ensured for the crew.

Per-agent outcomes are reported verbatim. When any agent fails, the response
is `ok: false` and the message still names the ones that did start — no
silent partial success.

### `stop` → `{ "message": "…" }`

Payload `{ "ids": ["lead"] }` — configured crew members only, nonempty. Stops
each supervisor (SIGINT; it stops the agent's process group first). Agents
that were not running are reported as such; failures make the reply an error
that still reports the rest honestly.

### `pause` / `resume` → `{ "message": "…" }`

Payload `{ "id": "lead" }` — the bus pause flag for that agent.

### `createTask` → `{ "message": "task #N created" }`

Payload `{ "title": "…", "brief": "…", "acceptance": "…",
"to": "impl-a"|null, "reviewer": "lead"|null, "priority": "normal",
"pathScopes": ["src"], "project": "/abs/dir"|null }`.

`title` is required. `reviewer` defaults to the operator. All fields map
straight onto the bus task contract (same validation, dependencies optional
and not exposed here). `pathScopes` requires `project`.

### `reviewTask` → `{ "message": "…" }`

Payload `{ "id": 12, "accept": true, "feedback": "…" }` — the bus review gate;
`accept: false` sends the task back for another round.

### `requeue` → `{ "message": "…" }`

Payload `{ "id": 12, "reason": "…" }` — the real bus requeue (claimed or open
task returns to open, unassigned).

### `cancel` → `{ "message": "…" }`

Payload `{ "id": 12, "reason": "…" }`.

### `send` → `{ "message": "sent" }`

Payload `{ "to": "lead", "subject": "…", "body": "…" }` — operator mail to an
agent id, comma-separated ids, or `"*"` for broadcast.

### `orchestration` → data object

The aos side, read from `<bus home>/aos/` and the bus:

```json
{ "simulated": false, "configured": true, "crewError": null,
  "crewDir": "~/…/aos", "workdir": "/repo", "goalOwner": "builder",
  "missions": [{ "name": "fix", "summary": "…", "brief": "…{goal}…",
                 "acceptance": "…", "text": "<file without its note>", "custom": false }],
  "roles":    [{ "name": "builder", "text": "…", "custom": false }],
  "crew":     [{ "id": "builder", "role": "implementation", "authority": "worker",
                 "cli": "claude", "description": "…", "enabled": true,
                 "instructions": "roles/builder.md", "running": false }],
  "goals":    [{ "id": 12, "title": "fix …", "state": "open",
                 "assignee": "builder", "reviewer": "operator", "updatedMs": 0 }] }
```

`custom` is true when the file differs from the built-in preset. `goals` are
the newest 20 top-level tasks the operator created. Before any crew file,
`configured` is false, `crew` is empty and the built-in presets are listed.

### `startGoal` → `{ "message": "goal #N queued / …", "taskId": N, "state": "queued", "taskState": "open", "nextAction": "…" }`

Creating a goal queues a task; it does not prove that an agent has claimed it.
`state` describes this receipt, while `taskState` is the persisted task state.
Clients display `message` and use subsequent snapshots to observe actual progress.
With no AOS crew, `nextAction` points to `aos setup` or an external worker.

Payload `{ "mission": "fix", "goal": "…", "to": null, "project": null }`.
Expands the mission like `aos` does (`{goal}` substitution, 120-char title),
hands it to `to` or the crew's goal owner, reviewer per `aos` rules (the
operator when there is no independent reviewer). Never starts an agent.

### `saveMission` / `saveRole` → `{ "message": "… saved" }`

Payload `{ "name": "ship", "text": "…" }`. `name` is 1-40 of `[a-z0-9_-]`
and becomes `missions/<name>.md` or `roles/<name>.md`; text is non-empty and
at most 64 KB. Missing presets are written first; no other file changes.

### `setAgent` → `{ "message": "…" }`

Payload `{ "id": "builder", "enabled": false, "description": "…" }` (either
field). Edits that crew member in `crew.json`, keeping all other fields; the
result is validated before it replaces the file. Errors when there is no crew
or the id is not in it.

## Environment

- `ACS_DESKTOP_QAGENT` — absolute path to the `qagent` supervisors run
  through; default is the file named `qagent` next to the `acs-desktop`
  binary. Required to be set or bundled for `start`.
- `ACS_DESKTOP_AOS` — same for the `aos` binary that runs the crash watcher;
  optional.
- `PATH` — the app sets it (GUI apps get a sparse one); `detect` and the
  supervisors inherit it.
