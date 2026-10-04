#!/bin/sh
# Worker death -> recovery -> submission -> independent review, on a throwaway bus.
#
# No agent CLI, account or token is used: the "agents" are shell commands that call
# qagent with their own identity. The bus lives in a temporary directory and is
# deleted at the end; your real bus (~/.agent-bus) is never opened.
#
#   sh examples/worker-death-recovery.sh                  # uses `qagent` on PATH
#   QAGENT="aos" sh examples/worker-death-recovery.sh     # the aos binary passes qagent commands through
#   QAGENT="node dist/qagent.js" sh examples/worker-death-recovery.sh
set -eu

QAGENT="${QAGENT:-qagent}"
HOME_DIR="$(mktemp -d "${TMPDIR:-/tmp}/acs-recovery.XXXXXX")"
trap 'rm -rf "$HOME_DIR"' EXIT
unset QAGENT_BUS_DB AGENT_BUS_HOME QAGENT_AGENT_ID
export QAGENT_HOME="$HOME_DIR"
export NODE_NO_WARNINGS=1

q() { $QAGENT "$@"; }
step() { printf '\n== %s\n' "$*"; }

step "a fresh bus with two workers and a reviewer"
q init >/dev/null
q --as operator agent add worker-1 --role worker >/dev/null
q --as operator agent add worker-2 --role worker >/dev/null
q --as operator agent add reviewer --role reviewer >/dev/null
q --as operator agent list

step "the operator files one scoped task; reviewer is the independent reviewer"
q --as operator task add "Fix the date parser" --brief "Parse ISO dates with offsets" \
  --scope src/date.ts --acceptance "tests pass" --reviewer reviewer
TASK=1

step "worker-1 claims it, starts work, and is killed"
# worker-1 is a background process: it claims, leaves a note, then works (sleeps).
QAGENT="$QAGENT" TASK="$TASK" sh -c '
  $QAGENT --as worker-1 task claim "$TASK"
  $QAGENT --as worker-1 task note "$TASK" "reproduced the offset bug, starting the fix"
  exec sleep 600' &
WORKER_PID=$!
until q --as operator task show "$TASK" | grep -q "starting the fix"; do sleep 0.2; done
kill -9 "$WORKER_PID"
wait "$WORKER_PID" 2>/dev/null || true
echo "worker-1 (pid $WORKER_PID) killed with SIGKILL; its claim is still on the bus"
q --as operator task show "$TASK" | head -n 3

step "the claim goes quiet; the bus reports it as stalled"
sleep 4
q --as operator task stalled --stall-min 0.05

step "recovery: the operator requeues it"
# The TypeScript supervisor can do this unattended: qagent supervise <agent> --auto-requeue-min <n>.
# The Rust build (aos) has no automatic requeue yet; there this step is manual.
q --as operator task requeue "$TASK" --reason "worker-1 died mid-task"

step "worker-2 picks it up from the note worker-1 left, and submits"
q --as worker-2 task claim "$TASK"
q --as worker-2 task submit "$TASK" --summary "Offset parsing fixed; tests pass" --file src/date.ts

step "the worker cannot accept its own work"
if q --as worker-2 task review "$TASK" --accept --feedback "lgtm" 2>&1; then
  echo "UNEXPECTED: self-review was accepted" >&2
  exit 1
fi

step "the independent reviewer accepts it"
q --as reviewer task review "$TASK" --accept --feedback "Checked the offset cases"

step "the bus is the trace"
q --as operator trace "$TASK"
