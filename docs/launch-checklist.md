# Owner launch checklist

Only the owner can do the steps below: Claude cannot merge without your word, push tags, change repository settings (GitHub returns 403), or post anywhere. Do them in order; each gate says what must be true first.

## 1. Before anything is public

- [ ] Merge the open reliability and first-run PRs you are happy with, on their own branches (`rust-port` work first, then `main`). Check the PR list rather than this file for what is still open.
- [ ] Cut a new `aos` release from `rust-port` once those are merged: push a tag `aos-v0.1.1` (or run the "aos binaries" workflow with `tag` set to a tag you pushed). Do not create the release from GitHub's "Draft a new release" page; the workflow's publish step then fails.
- [ ] On a Mac: run the one-line install, `aos demo`, and `QAGENT=aos sh examples/worker-death-recovery.sh`. No one has run the macOS binaries yet.
- [ ] Run one real goal end to end with `aos` in a scratch repository, with a builder and a reviewer from two different CLIs. Note what it cost. Until this works on your machine, do not post.
- [ ] Re-read the [claims table](launch-copy.md#claims-and-evidence) and strike anything that changed.

## 2. Repository settings (by hand, in the browser or with `gh`)

- [ ] Description and topics, from [launch copy](launch-copy.md#repository-description-and-topics):

  ```bash
  gh repo edit anon5376/agent-communication-system \
    --description "Run different coding agents together without being their message bus: task claims, crash recovery and independent review for Claude Code, Codex and other CLIs, in one local SQLite file." \
    --add-topic mcp,mcp-server,model-context-protocol,ai-agents,multi-agent,agent-orchestration,coding-agents,claude-code,codex,sqlite,local-first,cli,tui,typescript,rust,developer-tools
  ```

- [ ] Social preview: Settings → General → Social preview → upload `docs/assets/social-preview.png`.
- [ ] Optional: turn on Discussions for "how do I…" questions.

## 3. Listings (optional, after section 1)

- [ ] npm: `npm publish` from `main` publishes the `agent-communication-system` package (commands `qagent` and `agent-bus`). Then set `ACS_CHECK_NPM=1` once and run `npm run audit:public` so the README install check knows the package exists.
- [ ] MCP registries: `server.json`, `glama.json` and `smithery.yaml` are in the repo; submission steps are in [submission-pack.md](submission-pack.md). Its numbers predate this checklist; check them against the claims table first.

## 4. Posting (after sections 1 and 2)

- [ ] Show HN, using the draft in [launch copy](launch-copy.md#show-hn). Be around for the first two hours to answer.
- [ ] One Reddit post, a day or more later, not cross-posted the same hour.
- [ ] One X or Bluesky post linking the repo.
- [ ] Anyone you ask to share it (for example a friend with an audience) should try `aos demo` first and post only if it worked for them.

Answer every issue opened in the first week. "This didn't work for me" reports are the most useful thing a launch produces.
