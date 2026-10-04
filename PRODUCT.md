# Product

<!-- impeccable:product-schema 1 -->

## Platform

web

## Users

Primary user is a human operator of a local multi-agent coding setup. Other people may install the same product. Job: configure providers and a roster from scratch, then run work from the CLI or an operator MCP client, and see from the dashboard, `qagent doctor` or `qagent trace` what needs them.

## Product Purpose

Qagent is a local-first control plane for heterogeneous autonomous agents. Success is: a fresh install has no demo roster; the operator can autodetect installed CLIs, name agents, set who a manager may create, and hand tasks to named agents from the CLI or an operator MCP client.

## Positioning

Independent model CLIs (Anthropic, OpenAI, Cursor, xAI, Moonshot, and others) stay behind one durable local broker. Cursor is one provider among equals. The browser dashboard, CLI, and operator MCP are interfaces over the same SQLite state.

## Operating Context

Desk use on the operator's machine. Commands: `qagent init`, `qagent status`, `qagent doctor`, `qagent dashboard` (`agent-bus` is an alias). The dashboard requires a single-use sign-in link from `qagent dashboard link`. Default listen address is `127.0.0.1:11511`. Attach operator MCP with `qagent mcp-config` so a chat model can create and review tasks (`bus_task_create`, `bus_task_review`).

## Brand Commitments

Name: Qagent. Mark: "Q". The dashboard look is set in DESIGN.md: flat dark page, hairline rules, status as plain words, one blue accent. It has no settings page.

## Product Principles

1. Start empty. No fake agents and no stock opus/gpt roster in production config.
2. Autodetect installed CLIs; if a CLI is missing, show a login command and allow a manual binary path.
3. The operator configures hierarchy in config; managers get an explicit spawn list.
4. State on screen must match the broker contract.
5. Unrelated local processes on other ports are not this product.
