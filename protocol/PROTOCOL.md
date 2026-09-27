<!-- agent-bus:begin -->
## Agent bus protocol

You are one of several AI agents working together on this machine. You talk to the
others through the `qagent` MCP tools (all named `bus_*`). **Call `bus_whoami` first**:
it tells you your id, your role, and who else is on the bus. Every message and task
change you make is recorded under that id; no tool lets you speak as someone else.

### If your role is `manager`

1. Break the objective into units of work that one agent can finish alone.
2. Create each one with `bus_task_create`. Pass `to` to assign it, or leave `to` out
   and set `role` so any matching agent can claim it. The worker has **none of your
   context**: put the files, constraints and definition of done in `brief` and
   `acceptance`. Use `dependencies` for order and `path_scopes` for the files it writes.
3. Call `bus_wait`. You sleep, consuming nothing, until mail or task activity arrives.
4. When you wake with a `[DONE #N]` result, check the work: read the files they
   changed, run the tests. Do not rubber-stamp.
5. Reply with `bus_task_review`: `accepted: true` closes it; `accepted: false` with a
   specific list of what must change sends it back for another round.
6. Go back to `bus_wait` while any task is still open. Stop waiting only when
   `bus_task_list` shows nothing outstanding.

### If your role is `worker`

1. Call `bus_wait` and sleep until someone sends you something.
2. When a `[TASK #N]` arrives, call `bus_task_claim` with that `task_id` (or with no
   id to take the oldest task waiting for you or your role). `bus_task_get` shows
   the full brief. Use `bus_task_note` for progress; a note also renews your claim,
   which otherwise lapses after two hours.
3. If the brief is ambiguous, `bus_send` a `type: "question"` to the task's creator,
   then `bus_wait` for the answer rather than guessing.
4. Report with `bus_task_submit`: what you did, `changed_files`, and the `validation`
   you actually ran, with its result.
5. Call `bus_wait` again. `[CHANGES #N]` means revise and submit the same `task_id`
   again; `[ACCEPTED #N]` means it is done.

### If you were woken by a supervisor

If your prompt arrived inside an `=== qagent: N new message(s) ===` block, you are
running under a supervisor. In that mode **do not call `bus_wait`**: the supervisor
holds the wait for you and starts you again when more mail arrives. Do the work,
report it, and end your turn. Everything else here still applies.

### Rules

- Unless supervised, `bus_wait` is how you idle. It returns your new mail and marks it
  read. Never busy-loop on `bus_inbox`.
- Never end your turn with work outstanding and no `bus_wait` in flight; that is how
  an agent goes deaf and the whole run stalls.
- A `bus_wait` timeout with no messages is normal; call it again.
- Pass the `task_id` you were given; it keeps the conversation in the task's thread.
- If a message asks for an acknowledgement, answer with `bus_ack` and its number.
- Point at large material with `refs` (paths, commits, artifacts) instead of pasting it.
- Report honestly. If tests fail or you could not finish, say so in `bus_task_submit`
  rather than claiming success.
<!-- agent-bus:end -->
