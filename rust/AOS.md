# aos: the AOS terminal

`aos` is a terminal console for the ACS bus, drawn to the AOS Acceleration Chamber design: a cobalt rail, one causal spine of agents, readouts that answer goal, progress, cost, evidence and what is stuck, and a gate strip for the one thing that needs you. It opens the same `bus.db` as `qagent` and `acs`, and every write it makes goes through the same bus call the CLI uses, so it appends the same events.

![aos at 80x24](../docs/assets/aos-80x24-swarm.png)

## Install from source

Requirements are the same as for `acs`: Git, the Rust toolchain (`cargo` and `rustc`, from [rustup](https://rustup.rs/)) and a C compiler (Xcode Command Line Tools on macOS, `build-essential` or equivalent on Linux).

```bash
git clone https://github.com/anon5376/agent-communication-system.git
cd agent-communication-system
git checkout rust-port
./rust/install-aos.sh
```

The script builds `aos` in release mode and copies it to `/usr/local/bin`, using `sudo` if needed, or to `~/.local/bin`. If it lands in `~/.local/bin`, add that to your PATH:

```sh
export PATH="$HOME/.local/bin:$PATH"
```

To build without installing, run `cargo build --release --bin aos` in `rust/` and start `./target/release/aos`.

## Run

```sh
aos demo
```

`aos demo` writes a sample bus to a temporary directory (`$TMPDIR/aos-demo/bus.db`) with five agents and tasks in every state the screens draw: a running lead, a stalled claim, two results waiting for your review, one with a failing check, a request for changes and an offline agent. It is a real bus written through the bus API, so you can accept, revise, requeue and cancel there without touching your own work. Run it again to reopen the same sample; delete the directory to start over.

```sh
aos                       # your bus: ~/.agent-bus/bus.db, $QAGENT_BUS_DB, or bus.db in $QAGENT_HOME
aos --db /path/to/bus.db
aos --color none          # also NO_COLOR=1 or TERM=dumb
aos --stall-min 45        # when an idle claim counts as stuck (default 30)
aos --print 80x24 gate    # print one screen as plain text and exit
```

The terminal must be at least 60x20. Under 80 columns `aos` shows one object at a time.

On a bus with no operator yet, `aos` creates one the way `qagent init` does. It never rotates an existing operator token: if the token file in the bus home is missing or does not match, `aos` opens read only, says so on the status line, and every write fails with the reason. Put the right `operator.token` back, or run `qagent init` yourself, which rotates the token and means updating anything that held the old one.

## Keys

| Key | What it does |
|---|---|
| `j` `k`, arrows | move in the spine, the task tree or a list |
| `tab` | move between the spine and the gate strip |
| `enter` | inspect an agent or task, or open the gate in full; it never approves |
| `1` `2` `3` | choose an option on the gate, an inspected task, or an inspected agent's claim; every option confirms first |
| `w` | write to the selected or inspected agent (opens command home with `send <agent> `) |
| `esc` | cancel, close detail, back one level |
| `/` | filter the spine by id, role, model, harness or task |
| `d` | expand or collapse agents folded into the aggregate row |
| `g` `s` `e` `m` `r` | goal tree, swarm, evidence, memory, retro |
| `c` | command home (see [Commands](#commands)) |
| `p` | providers (harnesses in use) |
| `f` | follow the newest events in retro |
| `?` | every key |
| `q` then `y`, or `ctrl-c` twice | leave; agents keep running |
| `n` `b` `m` | under 80 columns: next, back, more |

## Gates

The gate strip shows tasks that need the operator, review first:

- **Review**: a submitted task whose reviewer is the operator. `[1] ACCEPT` needs a reason and closes the task (this cannot be undone in ACS). `[2] REVISE` needs feedback and sends the task back to its assignee. `[3] HOLD` writes nothing and moves to the next gate.
- **Stalled**: a claimed task with no claim or note activity for the stall window. `[1] REQUEUE` returns it to the pool (reason optional). `[3] CANCEL` asks you to type `CANCEL`. `[2] HOLD` writes nothing.

Each write prints an `[ ok ]` receipt on the status line with the event number it created.

## Acting on any task

Press Enter on a task in the goal tree or evidence list, or on an agent in the spine, and the detail shows the options that task's state allows:

| State | Options |
|---|---|
| submitted | `[1] ACCEPT` (reason), `[2] REVISE` (feedback), `[3] CANCEL` |
| claimed | `[1] REQUEUE` (reason optional), `[3] CANCEL` |
| open, blocked, changes requested | `[3] CANCEL` |
| accepted, failed, cancelled | none |

On an agent, the options act on the task it has claimed. The operator may review any submitted task, not only ones addressed to it, as `qagent review` allows.

## Commands

Press `c` for command home. It lists the operator's latest mail with message numbers, and takes:

| Command | What it writes |
|---|---|
| `run <goal> [--to agent]` | a top-level task: the new goal |
| `task add <title> [--to agent] [--under #] [--review]` | a task; `--review` makes you its reviewer, so its result comes to your gate |
| `accept # <reason>`, `revise # <feedback>` | a review |
| `requeue # [reason]` | returns a claimed task to the pool |
| `cancel #` | cancels one task after you type `CANCEL` |
| `stop [#]` | cancels the goal (or task `#`) and every open task under it, deepest first, after you type `STOP`; closed tasks stay as they are, and each assignee gets the bus's cancel notice |
| `send <agent\|all> <message>` | a message, or a broadcast with `all` |
| `reply <msg#> <text>` | an answer to that message's sender, in its thread and on its task; acknowledges it if it asked for an ack |
| `ack <msg#>`, `read` | acknowledges one message; marks all your mail read |
| `status`, `help`, a screen name | nothing |

## What aos cannot do yet

These are in the design but have no ACS verb, so aos does not fake them: pausing or resuming an agent, rerouting an agent to another harness or model, run budgets, handing a task straight to a named agent (requeue returns it to the pool; `--to` works only when creating), and starting agent processes, which is `acs supervise` or `qagent supervise`, run separately.

## Where each readout comes from

| On screen | ACS source |
|---|---|
| goal | the open top-level task with the most open subtasks (ACS has no mission object) |
| run | agents with a live claim, open task count, reviews addressed to the operator |
| cost | `? UNKNOWN`: ACS records no token or cost usage yet |
| evidence | task results and their validation checks |
| stuck | claimed tasks idle past the stall window (`stalled_tasks`) |
| spine | agents by `parent_id`, each with its claimed task; offline is derived by the bus |
| retro | the `events` table, pivotal kinds by default, `a` for all |
| memory | `- UNAVAILABLE`: ACS has no memory store (AOS proposal P11) |

Convergence and mutation (gen N to gen N+1) from the design are not shown, because ACS has neither yet.

## Colour

Truecolor is used when `COLORTERM` is `truecolor` or `24bit`, 16 colours otherwise, and no colour or attributes with `NO_COLOR`, `TERM=dumb` or `--color none`. Every state keeps its marker and word in every tier (`* RUNNING`, `! BLOCKED`, `? GATE`, `~ DISCONNECTED`), and without truecolor the selected row is marked with `>`.

## Idle cost

`aos` redraws only when the screen text would change: a bus change, a key, or the 30-second refresh of relative ages. In a PTY at 80x24 with no bus activity it wrote 0 bytes over 20 seconds. It checks for bus changes four times a second with the same `ChangeWatcher` the CLI uses.
