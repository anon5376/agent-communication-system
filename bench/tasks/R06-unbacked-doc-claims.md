---
id: R06
kind: research
role: research
title: Find public documentation claims that the code at this commit does not back
mini: false
truth: truth/R06.json
---

## Brief
The repository ships user-facing documentation alongside its code. Find the statements in that documentation that the code at this commit does not back.

Scope: `README.md`, `CHANGELOG.md`, and every `docs/*.md` file (not `docs/assets/`). Other files are evidence, not scope.

What counts as a claim: a statement presented as true now (or as shipped, in the changelog) about this project, such as a command or flag that is said to work, a feature, adapter or capability that is said to exist, a stated number, a statement about the package or its publication, or a description of the project's own current state or files. Proposals, plans and to-do items about future work do not count unless they also state the present state.

What counts as unbacked: the claim is contradicted by the code, by a config file in the checkout (for example `package.json`), or by a command you run in the checkout (for example the CLI in the committed `dist/`, a git command, or an npm command that needs no network). A claim you merely find unsourced or cannot check offline is not an item; put it in `unresolved` with the reason. Work offline.

How to name an item: `<file>:<short-slug>`, where `<file>` is the path from the repository root (`README.md`, `CHANGELOG.md`, `docs/<name>.md`) and the slug is a few lowercase words joined by hyphens that identify the claim (for example `docs/example.md:sync-flag`). One item per distinct claim per file: if the same false statement appears in two files, report it once for each file.

Run the CLI against a throwaway bus (`QAGENT_HOME` and `QAGENT_BUS_DB` pointing into a temp dir), never against `~/.agent-bus`. The `dist/` directory is committed, so no build is needed. Do not run `npm install`. Do not modify the repository.

## Acceptance
- `items` lists each unbacked claim once, named `<file>:<short-slug>`.
- Each item has a claim quoting the documentation text with its `path:line`, and evidence that contradicts it: a `path:line` in code or config, or a command and its real output.
- Claims that could not be settled offline or from the checkout are listed in `unresolved`, each with the reason.
