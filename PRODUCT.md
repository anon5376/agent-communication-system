# Product

<!-- impeccable:product-schema 1 -->

## Platform

cli (with an optional localhost web page and, on the `rust-port` branch, the `aos` terminal console)

## Users

A developer who already uses one or more coding-agent CLIs (Claude Code, Codex, Cursor and others) on their own machine and wants several of them to work on one project without relaying every brief and result by hand. Job: give agents scoped tasks, let them claim, hand off and submit work, have someone other than the author review it, and see what is stuck.

## Product Purpose

Run different coding agents together without being their message bus. Success is: an agent dies mid-task and the work is recovered rather than lost; every task has one owner at a time; no agent accepts its own work; and the operator can tell from the bus alone what is waiting on them, what is running and why something stalled.

## Positioning

ACS is the coordination layer under standalone agent CLIs, not an agent framework. It does not write prompts or choose models. One SQLite file holds identities, mail, tasks, claims, path leases, reviews and the event log; the CLI, the MCP server, the supervisor, the dashboard and `aos` are interfaces over that same file. No daemon, no cloud, no broker.

## Operating Context

Desk use on the operator's machine, per user, per machine. Commands on `main`: `qagent init`, `qagent status`, `qagent task …`, `qagent trace <task>`, `qagent supervise <agent> [dir]`, `qagent doctor`, `qagent dashboard` (`agent-bus` is an alias). The dashboard listens on `127.0.0.1:11511`, needs a single-use sign-in link from `qagent dashboard link`, reads the bus, and can only send a message as the operator. `aos` (from the `aos-v0.1.0` release, built from `rust-port`) is the guided front door: it sets up a crew from detected CLIs, takes goals as sentences, and passes every `qagent` command through.

## Brand Commitments

Names: ACS for the project, `qagent` for the TypeScript CLI, `aos` for the terminal console. The dashboard follows `DESIGN.md` (flat, one accent, no status colour). `aos` follows the Accelerate / Acceleration Chamber design record in the AOS repository.

## Product Principles

1. Start empty. No demo agents in a real setup; samples live only in `aos demo` and in throwaway buses.
2. Detect installed CLIs and say plainly whether each is installed, signed in and able to join a crew; never infer sign-in, quota or model access from a binary on PATH.
3. Claims, reviews and permissions are enforced in the bus core, not by the interface that happens to be used.
4. State on screen must match the bus. Unknown is shown as unknown (for example cost when a CLI reports none).
5. Guardrails are not a sandbox. Say what the bus protects against (accidental impersonation, double claims, self-review) and what it does not (a hostile process running as the same OS user).
