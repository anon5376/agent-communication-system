# Security model

Qagent's tokens stop an agent from accidentally, or because a prompt told it to, acting as another agent. They do not stop a hostile process running as your Unix user. That process can open `bus.db` and write to it directly, just as it can read the token files. If you need a boundary between agents, run them as different users or in separate VMs.

## Identity

- `qagent init` creates the operator identity and writes a random token to `~/.agent-bus/operator.token` (mode 0600; the directory is 0700).
- `qagent agent add <id>` (operator only) creates an agent and writes its token to `~/.agent-bus/tokens/<id>.token`.
- The database stores only the SHA-256 hash of each token, with the identity's authority: `operator`, `manager` or `worker`.
- A process names itself with `QAGENT_AGENT_ID` (old name `AGENT_ID`) or `--as <id>`. The library reads that identity's token file and checks it against the stored hash. A missing or mismatched token is refused.
- The sender of a message, the creator of a task and the author of a note are always the verified identity. No CLI command or MCP tool accepts a `from` argument.
- `qagent token rotate <id>` (operator only) replaces a token; the old one stops working at once.

## Authority

| Action | Who |
|---|---|
| Add agents, rotate tokens, import history | Operator |
| Review a task | Its reviewer (the creator by default) or the operator; never the assignee reviewing its own work |
| Cancel a task | Its creator or the operator |
| Submit, note on or release a claimed task | Its assignee (release and failure reports: also the operator) |
| Send, read own inbox, wait, create and claim tasks | Any identity |

The MCP server exposes `bus_agent_add` only when started with `--operator`, and `--operator` refuses to start unless the identity is the operator.

## Path leases

A task can declare path scopes inside a project directory. Claiming the task takes leases on those paths, and a claim whose scopes overlap another task's leases is refused. Leases prevent two agents from being handed the same files. They do not stop a process from writing outside its scope; for that, use per-task git worktrees (`qagent task claim --worktree`, or `"isolation": "worktree"` for the supervisor; TypeScript build only) or the harness's own sandbox. A worktree separates checkouts, not permissions: the agent process can still reach any path its user can.

## Input limits

Titles, briefs, message bodies, notes, reference lists and changed-file lists have size limits, enforced in `src/core/types.ts` before anything is written.

## Dashboard

- It binds `127.0.0.1` only. The address is fixed in code; there is no flag to change it.
- Requests whose `Host` header is not `127.0.0.1:<port>` or `localhost:<port>` are refused, which blocks DNS-rebinding pages.
- The browser never receives the operator token. `qagent dashboard` (or `qagent dashboard link`) proves it holds the token and gets a single-use ticket valid for 5 minutes. The page trades the ticket for an `HttpOnly`, `SameSite=Strict` session cookie. Sessions live in memory and end when the process stops.
- `/api/*` needs a session. The one write, `POST /api/send`, also needs a same-origin `Origin` header and refuses cross-site fetch metadata.
- Responses carry a strict content security policy: `default-src 'none'`, with the page's own script and style allowed by a per-response nonce, and `frame-ancestors 'none'`.

## Supervisor and harnesses

- The supervisor starts agent CLIs with the agent's own identity; they never see the operator token.
- For subscription-backed providers it removes provider API-key variables (for example `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`) from the child's environment unless `QAGENT_ALLOW_API_KEY=1` is set, so a CLI does not silently bill an API key instead of the subscription.
- Stopping the supervisor, or a turn timing out, kills the CLI's whole process group. File changes the CLI already made are not rolled back.
- What a CLI can do on disk and on the network is decided by that CLI's own permission and sandbox settings, which the adapters pass through. Some adapters need broad access for MCP to work under some CLI versions; that is visible configuration, not a guarantee. Do not point unsandboxed agents at untrusted directories.

## Data and residual risks

- `bus.db` and its WAL hold message bodies, task briefs and results in plain text. Logs under `~/.agent-bus/logs/` may contain project content. Protect the directory accordingly.
- A sandboxed agent may be unable to write `~/.agent-bus`. MCP servers usually run outside the harness sandbox, but a `qagent` call from inside one may be refused; check with the harness you use.
- No task artifact or commit is signed.
