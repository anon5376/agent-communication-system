# Product

<!-- impeccable:product-schema 1 -->

## Platform

web

## Users

Primary user is a human operator of a local multi-agent coding setup. Other people may install the same product. Job: configure providers and a roster from scratch, then run work from the dashboard, CLI, or chat (`@qagent`).

## Product Purpose

Qagent is a local-first control plane for heterogeneous autonomous agents. Success is: a fresh install has no demo roster; the operator can autodetect installed CLIs, name agents, set who a manager may create, and delegate exact model+provider targets from chat.

## Positioning

Independent model CLIs (Anthropic, OpenAI, Cursor, xAI, Moonshot, and others) stay behind one durable local broker. Cursor is one provider among equals. The browser dashboard, CLI, and operator MCP are interfaces over the same SQLite state.

## Operating Context

Desk use on the operator's machine. Commands: `qagent start`, `qagent open`, `qagent status` (`agent-bus` is an alias). Dashboard requires a one-time CLI ticket. Default listen address is `127.0.0.1:11511`. Attach operator MCP with `qagent mcp-config` so a chat model can call `qagent_delegate`.

## Brand Commitments

Name: Qagent. Mark: "Q". Visual language follows Cursor-style dashboard chrome (dark panels, rounded rows, blue accent) without Cursor trademarks. Operators can change the dashboard colors in Settings.

## Product Principles

1. Start empty. No fake agents and no stock opus/gpt roster in production config.
2. Autodetect installed CLIs; if a CLI is missing, show a login command and allow a manual binary path.
3. The operator configures hierarchy; managers get an explicit spawn list via drag-and-drop.
4. State on screen must match the broker contract.
5. Unrelated local processes on other ports are not this product.
