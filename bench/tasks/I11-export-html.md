---
id: I11
kind: implementation
role: implementation
title: Add qagent export to write the whole bus as one self-contained HTML file
mini: false
scope: [src/, dist/, tests/]
validator: validate/I11.mjs
---

## Brief
`qagent trace <task> --export` can write one task's chain as a self-contained HTML page, but there is no way to export the whole bus. Add `qagent export --format html --out FILE`, which writes one HTML document containing every agent (id, role, authority, status), every task (shown as `#<id>` with title, state, assignee, reviewer, result, review, and its notes), every message (sender, recipient, subject, body) and the event log (sequence number, actor, kind). Reuse the existing trace HTML helper's styling where sensible.

The file must be fully self-contained: inline CSS only, no script that loads anything, no external URL of any kind. All text from the bus must be HTML-escaped. `--out` is required for html; an unknown `--format` fails with a clear error. Add a test.

## Acceptance
- `qagent export --format html --out FILE` writes an HTML document that contains every task as `#<id>` with its title (escaped), plus notes, results, messages, agents and events.
- The document contains no `http://` or `https://` text and loads no external resource. Text from the bus that itself contains a URL must not appear as a literal `scheme://` (for example encode the slashes).
