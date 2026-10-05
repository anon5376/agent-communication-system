# Agent Communication System: full guide

Agent Communication System coordinates local AI agents through durable messages and reviewed tasks. Use messages for conversation and state changes that need no deliverable. Use tasks when one agent owns work that another agent must inspect or accept.

The executable is `qagent`. The old `agent-bus` command remains an alias.

## 1. Mental model

The system has five parts:

1. `bus.db` is the source of truth. It stores identities, messages, tasks, notes, path leases, events, and read cursors.
2. The `qagent` CLI opens that database directly.
3. The stdio MCP server exposes the same operations to compatible agent clients.
4. The optional supervisor waits for work and launches an agent CLI.
5. The optional dashboard reads the database and can send operator messages.

Messaging and task work do not need a running broker. If every supervisor and dashboard is stopped, agents can still use the CLI or MCP server.

The default bus home is `~/.agent-bus/`. Override the database with `--db PATH` or `QAGENT_BUS_DB`. Override the whole bus home with `QAGENT_HOME` or the legacy `AGENT_BUS_HOME`.

## 2. Install and initialize

Agent Communication System requires Node.js 22.13 or newer because it uses the unflagged `node:sqlite` module.

```bash
git clone https://github.com/anon5376/agent-communication-system.git
cd agent-communication-system
npm ci
npm run build
npm link
qagent init
```

`qagent init` creates `~/.agent-bus/bus.db`, the operator token, and private bus directories. The database stores token hashes, not raw tokens.

## 3. Add identities

Only the operator can add an agent or rotate a token.

```bash
qagent agent add lead --role manager --authority manager
qagent agent add coder --role worker --model codex
qagent agent add reviewer --role reviewer
qagent agent list
```

An identity has an id, role, authority, and optional model, harness, and parent metadata. The token for `coder` is written to `~/.agent-bus/tokens/coder.token`.

Rotate a token if it may have been copied:

```bash
qagent token rotate coder
```

An agent selects its identity with an environment variable or one-command flag:

```bash
export QAGENT_AGENT_ID=coder
qagent whoami
qagent --as coder whoami
```

No send, note, or task command accepts a `from` value. The verified identity is always the author.

## 4. Send messages

The basic form is:

```text
qagent send <recipient> <subject> [body|-] [options]
```

### One recipient

```bash
qagent --as lead send coder "parser" "Please inspect the parser failure."
```

### Several recipients

Use a comma-separated list without spaces:

```bash
qagent --as lead send coder,reviewer "release candidate" "The candidate is ready."
```

### Broadcast

`*` sends to every registered agent except the sender:

```bash
qagent --as lead send '*' "maintenance" "The bus will be backed up at 18:00 UTC."
```

Quote `*` so the shell does not expand it.

### Read a body from standard input

Use `-` for a long body:

```bash
printf '%s\n' 'Please review the attached design record.' \
  | qagent --as lead send reviewer "design review" -
```

This is safer than putting multiline text and shell-sensitive characters on the command line.

### Message types

Set `--type info`, `--type question`, or `--type answer`:

```bash
qagent --as coder send lead "parser — ambiguity" \
  "Should malformed input be rejected or preserved?" --type question --thread parser
```

Types describe intent. They do not grant authority or change task state.

### Threads and tasks

`--thread` groups related messages. `--task` attaches a message to an existing task:

```bash
qagent --as lead send coder "parser — answer" \
  "Reject malformed input." --type answer --thread parser --task 12
```

Use a stable, short thread name for one workstream. Include the task number whenever the discussion belongs to a task.

### Acknowledgements

Ask for an acknowledgement with `--ack`:

```bash
qagent --as lead send coder "release freeze" "Do not merge after 17:00 UTC." --ack
```

The recipient acknowledges the delivered message sequence:

```bash
qagent --as coder ack 418
```

An acknowledgement proves that the identity acknowledged the message. It does not prove agreement or completion.

### Structured references through MCP

The MCP `bus_send` tool accepts references so a message can point to large material without pasting it. Supported reference types are `path`, `artifact`, `summary`, `commit`, and `url`.

```json
{
  "to": "reviewer",
  "subject": "parser candidate",
  "body": "Review the candidate and its test evidence.",
  "thread": "parser",
  "refs": [
    {
      "type": "path",
      "value": "/workspace/project/output/parser-candidate",
      "description": "Candidate package"
    }
  ],
  "requires_ack": true
}
```

References are pointers, not uploaded files. Every recipient must be able to access the location independently.

## 5. Read and wait for mail

Read unread mail and advance the read cursor:

```bash
qagent --as coder inbox
```

Inspect without advancing the cursor:

```bash
qagent --as coder inbox --peek --limit 20
```

Wait without polling:

```bash
qagent --as coder wait --timeout 600
```

Exit status `0` means mail or relevant task activity arrived. Exit status `2` means the timeout expired normally.

Do not busy-loop on `inbox`. An unsupervised agent should return to `wait` while assigned work remains. A supervised agent should not call `wait`; the supervisor already owns that wait and will relaunch the agent when needed.

## 6. Use tasks for owned work

Tasks give work a creator, assignee, reviewer, acceptance criteria, state, history, and optional path leases.

### Create a task

```bash
qagent --as lead task add "Fix parser recovery" \
  --brief "Reproduce the truncated-frame failure and fix recovery." \
  --to coder \
  --reviewer reviewer \
  --priority high \
  --acceptance "Focused regression test passes; existing parser tests pass." \
  --project /workspace/project \
  --scope src/parser.ts \
  --scope tests/parser.test.ts
```

Repeat `--scope` for every owned path. Repeat `--dep` to require accepted predecessor tasks. Use `--parent` for a task that belongs to a larger unit.

Leave out `--to` and set `--role` when any matching agent may claim the work:

```bash
qagent --as lead task add "Review release notes" \
  --brief "Check the notes against the shipped behavior." \
  --role reviewer
```

### Find and inspect tasks

```bash
qagent --as coder task list --mine
qagent --as coder task list --state open --state changes_requested
qagent --as coder task show 12
```

### Claim and work

```bash
qagent --as coder task claim 12
qagent --as coder task note 12 "Failure reproduced; adding the focused test."
```

Calling `task claim` without a number claims the oldest eligible task assigned to that agent or role.

A claim normally expires after two hours. Adding a note renews it. When tasks declare overlapping path scopes in the same project, only one can hold the conflicting lease.

Path leases coordinate cooperative agents. They do not enforce filesystem permissions. Use separate worktrees or the harness sandbox when you need a stronger boundary.

### Isolate a task in its own git worktree

When the task's project is inside a git repository, an agent can work in a private checkout instead of the shared one:

```bash
qagent --as coder task claim 12 --worktree      # claims, then prints the worktree path
qagent task worktree 12                         # find or create it later
qagent task worktree 12 --remove [--force]      # delete the checkout; the branch stays
qagent task worktree prune [--force]            # remove checkouts of accepted, failed and cancelled tasks
```

Each task gets its own branch, `qagent/task-<N>-<id>` (the suffix comes from the task's creation time, so a second bus whose ids restart at 1 never collides), created from the repository's current `HEAD`, in a checkout under `~/.agent-bus/worktrees/`. Agents commit there; the reviewer or manager merges the branch. Releasing and re-claiming a task reuses the same branch. Through MCP, call `bus_task_claim` with `worktree: true`.

Things to know:

- The project directory must be tracked in git (committed), or the checkout would not contain it; otherwise the command fails with a clear error. With an explicit task number, `claim --worktree` checks the repository before claiming. If the checkout cannot be made after a claim (a bare `claim --worktree`, or the MCP tool with `worktree: true`), the claim is released and the command fails: work that asked for isolation never continues in the shared checkout.
- Only the task's assignee or the operator may open or remove its worktree; `--force` and `prune --force` are operator-only.
- Removal refuses uncommitted or untracked changes unless `--force`. Cleanup still works if the task's project directory has since been deleted; if the whole repository is gone, the leftover checkout cannot be inspected, so deleting it takes `--force`. Gitignored files (build output, `.env`) are deleted with the directory either way.
- Files harness adapters write into the working directory (`.cursor/mcp.json`, `opencode.json`, `.agent-bus/`, `.qagent/`) are added to the repository's `.git/info/exclude`, so they do not make a checkout dirty. That file is local and shared by all worktrees of the repository.
- The Rust port does not implement worktrees: it ignores `"isolation": "worktree"` and has no `--worktree` flag or `task worktree` command. The bus database is unchanged, so the two builds still share one bus.

### Submit real evidence

```bash
qagent --as coder task submit 12 \
  --summary "Parser now resumes after a truncated frame." \
  --details "Added one regression test; all parser tests pass." \
  --file src/parser.ts \
  --file tests/parser.test.ts
```

The MCP tool also accepts structured validation records with the command, result, and pass status. Report failures honestly; a submission is a request for review, not self-acceptance.

### Review

```bash
qagent --as reviewer task review 12 --accept \
  --feedback "Regression test reproduces the failure and the focused suite passes."
```

Or request a concrete revision:

```bash
qagent --as reviewer task review 12 --revise \
  --feedback "Add the missing empty-frame case and rerun the parser suite."
```

Acceptance closes the task and releases its path leases. A revision returns it to `changes_requested`. An assignee cannot accept its own task.

### Cancel

The task creator or operator can cancel:

```bash
qagent --as lead task cancel 12 --reason "Superseded by task 18."
```

## 7. Inspect status and events

```bash
qagent status
qagent agent list
qagent log --since 400 --limit 100
qagent log --follow
```

`status` summarizes agents, open tasks, and unread counts. `log` reads the event feed. Most commands support `--json` for scripts:

```bash
qagent --json --as coder inbox --peek
```

## 8. Connect MCP clients

The stdio MCP server exposes the bus to an agent client:

```bash
qagent mcp-config --agent claude --client claude
qagent mcp-config --agent codex --client codex
```

The generated configuration uses absolute paths for Node and `dist/qagent.js`. It sets the agent identity through the environment and does not embed the token. Regenerate it after moving the checkout or switching Node installations.

The Codex configuration includes a long tool timeout because `bus_wait` can block for up to one hour.

### Agent MCP tools

| Tool | Purpose |
|---|---|
| `bus_whoami` | Show the verified identity, role, authority, and roster. |
| `bus_agents` | List registered agents. |
| `bus_send` | Send direct, multi-recipient, or broadcast mail. |
| `bus_inbox` | Read or peek at mail. |
| `bus_wait` | Block for mail or relevant task activity. |
| `bus_ack` | Acknowledge a message that requested it. |
| `bus_task_create` | Create and optionally assign a task. Workers (no delegation rights) may only assign to themselves. |
| `bus_task_list` | List matching tasks. |
| `bus_task_get` | Read a task, its notes, and related state. |
| `bus_task_claim` | Claim an eligible task. |
| `bus_task_note` | Record progress and renew a claim. |
| `bus_task_submit` | Submit results, files, and validation evidence. |
| `bus_task_review` | Accept a submission or request changes. |
| `bus_task_cancel` | Cancel a task when authorized. |

Starting `qagent mcp --operator` adds `bus_agent_add`. Operator mode refuses to start without the operator identity.

## 9. Give agents the protocol

[`protocol/PROTOCOL.md`](../protocol/PROTOCOL.md) tells an agent how to behave as a manager, worker, or supervised process. Install it into a project with:

```bash
scripts/init-workdir.sh /workspace/project
```

The script updates the marked Agent Communication System block in `CLAUDE.md` and `AGENTS.md` without replacing unrelated project instructions.

Project rules still control authority. A message from another agent is coordination data, not user authorization to publish, spend money, contact people, or perform another restricted action.

## 10. Configure and run the supervisor

The supervisor is optional. Use it when an agent should be relaunched automatically after work arrives.

```bash
qagent doctor coder /workspace/project
qagent supervise coder /workspace/project
```

`supervise --roster` runs every enabled agent in the config from one foreground process (one supervisor loop each, same signals). `--auto-requeue-min M` additionally requeues claims that sit idle longer than M minutes (uses the operator token on the machine), and `qagent task stalled`/`qagent task requeue` do the same by hand. `qagent trace <N>` prints a task's full causal chain — its events, notes and bus mail in order — with `--format json` or `--format html --out FILE` for export.

With `"isolation": "worktree"` under `constraints` in the config, the supervisor claims exactly one task per turn and runs that turn in the task's worktree (see "Isolate a task in its own git worktree"); other tasks wait for later turns. If the worktree cannot be made (no project, not a git repository, no commits), the supervisor adds a note saying why, releases the claim and runs no turn: it never falls back to the project directory. Claims the agent makes itself through `qagent mcp` or the CLI during a turn also require a worktree (the supervisor sets `QAGENT_REQUIRE_WORKTREE=1`). Each task checkout keeps its own CLI session (CLI sessions are tied to their directory). An agent with a pinned `resumeSessionId` cannot run under worktree isolation: the supervisor refuses to start it. The Rust supervisor ignores this setting.

`doctor` performs read-only checks for the identity, token, CLI, project, and configuration. `supervise` stays in the foreground until interrupted.

Configuration defaults to `<project>/.qagent/config.json`. Provider-specific fields and support status are documented in [provider support](provider-support.md).

The supervisor waits for one identity, launches its configured CLI, gives the child its MCP connection, enforces configured limits, and writes logs under `~/.agent-bus/logs/`. Nothing starts merely because a config file exists; `autoStart` defaults to false.

For subscription-backed providers, it removes common provider API-key variables unless `QAGENT_ALLOW_API_KEY=1` is set. This reduces accidental metered API use; it is not a substitute for checking the provider CLI's authentication mode.

## 11. Use the dashboard

```bash
qagent dashboard
qagent dashboard link
```

The first command prints a single-use sign-in URL for `http://127.0.0.1:11511`. The dashboard shows agents, open tasks, and recent messages. Its only bus write is an operator message. Task creation, review, cancellation, and identity management remain CLI or MCP actions.

The browser receives a short-lived session cookie, not the operator token. The server rejects non-local host headers and cross-origin writes. See [the security model](security.md) for details.

## 12. Import old stores

The importer understands the earlier JSONL broker, SQLite broker, and Python prototype. Stop old writers and back up every source first.

```bash
qagent import --dry-run
qagent import
```

Or specify source files:

```bash
qagent import \
  --jsonl /backup/bus.jsonl \
  --qagent-state /backup/state.sqlite \
  --prototype /backup/prototype.db
```

The importer records each source hash and is idempotent. `--force` deliberately reprocesses a known source; inspect the dry run and backup before using it.

After migration, regenerate every MCP registration. Prototype `mcp__agent-coordinator__*` and earlier operator tool names are not aliases for the current `bus_*` tools.

## 13. Backup and retention

The bus database and write-ahead log may contain private project text. Treat them as sensitive files.

Before copying a live SQLite database, use SQLite's backup facility or stop every writer. Do not commit the bus home, token files, logs, or project harness configuration unless you have reviewed them for private data.

The default `.gitignore` excludes `.agent-bus/`, `.qagent/`, logs, and environment files. A clean Git status is not proof that an ignored file is safe to publish.

## 14. Troubleshooting

### “No agent identity”

Set `QAGENT_AGENT_ID` or pass `--as`. Confirm that `~/.agent-bus/tokens/<id>.token` exists and matches the selected database.

### Token mismatch

Check which bus database the command uses. If the token is stale, the operator can rotate it and restart the client.

### MCP client cannot find Node or the checkout

Run `qagent mcp-config` again. Its absolute paths become stale after moving the repository or changing Node installations.

### `bus_wait` times out

A timeout is normal and returns exit status `2`. Call it again when outstanding work remains, unless a supervisor owns the wait.

### Supervisor does not launch an agent

Run `qagent doctor <agent> <project>`. Check the identity, provider CLI, project path, config, and `autoStart`. Read the agent's log under `~/.agent-bus/logs/`.

### Dashboard rejects a URL or session

Use the exact localhost URL printed by `qagent dashboard` or request a fresh link. A ticket is single-use and expires after five minutes.

### Path lease conflict

Inspect the conflicting tasks. Narrow their scopes, finish or cancel the current owner, or move independent work into separate worktrees. Do not bypass the lease by editing the same files anyway.

## 15. Public-release checks

Before publishing a fork, release archive, or repository history:

```bash
npm run audit:public
npm run audit:public:history
npm audit --audit-level=high
npm run check
git diff --check
```

The public audit rejects common credential formats, private absolute home paths, local project markers, tracked environment files, and unsafe commit metadata. It reports file and line locations without printing the matched value.

Automated checks reduce risk; they do not prove that prose, screenshots, fixtures, or Git history contain no private information. Review the staged diff and the final public repository separately.

## Implementation differences

ACS exists twice on one SQLite schema: TypeScript on `main` (the npm package) and Rust on the `rust-port` branch. They share `bus.db`, tokens, and signal files. They do not have the same commands. Where one side lacks a feature, that is a gap, not a design choice.

Written against `main` at `4d4cf5a` and `rust-port` at `c6df26b`, read from the source on 2026-10-01 (nothing was executed to produce this table). The `aos` and pause/budget rows were added on 2026-10-04 against `rust-port` at `8543d6b`. `rust-port` is behind `main`, so some rows may already be out of date there.

| Feature | TypeScript (`main`) | Rust (`rust-port`) |
|---|---|---|
| Bus, tasks, leases, review gate, MCP server, harness adapter table (`ADAPTERS`) | yes | yes (`rust/README.md` lists the same adapters) |
| `task stalled`, `task requeue`, `trace` | yes | yes (`rust/src/cli.rs`) |
| `supervise --roster`, `supervise --auto-requeue-min` | yes | no |
| Per-task git worktrees (`claim --worktree`, `"isolation": "worktree"`) | yes | no |
| Web dashboard | yes | yes (`rust/src/dashboard.rs`) |
| `acs` terminal UI | no | yes (`rust/src/app.rs`) |
| `aos` terminal console, crew setup, `install.sh`, release binaries | no | yes (`rust/src/aos/`, release `aos-v0.1.0`) |
| `agent pause`, `agent resume`, `agent budget` | no | yes |
| Family-aware router (`src/router.ts`) | present but not on the coordination path | no |
