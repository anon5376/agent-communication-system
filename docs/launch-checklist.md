# Owner launch checklist

Owner-controlled release procedure. Assistants may prepare and test a candidate, but merging, tagging, publishing, repository settings and posting require the owner's explicit approval. An installer attached to a PR is a candidate, not a published release.

## 1. Before anything is public

- [ ] Review the exact release PR and commit. Rust is the product; TypeScript `qagent` remains a compatible npm package. Do not release the old `rust-port` branch or reuse an `aos-v*` tag for the unified product.
- [ ] Run the release checks on that commit: `npm run check`, `npm run audit:public`, `npm run audit:public:history`, Rust formatting/Clippy/tests, frontend checks, native packaging and shared-bus interop. Record skipped or failed checks, not just the green jobs.
- [ ] Resolve the historical public-audit findings with the repository owner. A clean working-tree audit does not clear historical author/committer metadata. Do not disable the gate, silently allowlist findings or rewrite history to get green CI.
- [ ] Run a real local-model task in a disposable project: worker claims, fixes and submits; a distinct reviewer runs the acceptance tests and records review. Keep the model/version, endpoint, deadline, protected-file checks and failure evidence. A simulated soak or a successful model greeting does not satisfy this gate. Use local Ollama only for this rehearsal.
- [ ] Verify native candidate installers on macOS and Windows: real helper, both modes, agent setup, queue-only goals, failure diagnostics and stop. Create a disposable task/preset, reinstall, uninstall without deleting workspace data, then reconnect. Distinguish same-version reinstall from a genuine version upgrade.
- [ ] Review candidate evidence in the PR, including source revision and SHA-256 per artifact. A soak on an earlier binary is useful evidence, not a soak of final HEAD.
- [ ] Re-read the [claims table](launch-copy.md#claims-and-evidence). Adapter fixtures do not certify live providers.

## 2. Publish and rehearse the download path

- [ ] Approve the exact merge candidate(s) and order, then merge to `main`. Do not treat approval of this checklist as merge permission.
- [ ] Choose a new unified `v*` tag on the approved commit. A hyphenated tag is marked as a prerelease by **ACS release assets** (`.github/workflows/acs-release.yml`). Manual workflow dispatch builds candidate artifacts but does not publish a release.
- [ ] Confirm the workflow produced all four Linux/macOS CLI archives (each with `acs`, `aos`, `qagent`, `acs-desktop`), the Apple Silicon DMG, Windows x64 installer, source SBOM and `SHA256SUMS`. Old AOS-only releases do not satisfy this gate.
- [ ] Download through the public release URLs on clean machines and verify checksums. Test pinned `ACS_VERSION=<published-tag>` CLI installation, `aos demo`, native first launch and reconnecting an existing workspace. A local build/reinstall is not a public-download rehearsal.
- [ ] Keep unsigned warnings visible: macOS app requires macOS 14+ and Apple Silicon and is ad-hoc signed, not notarized; Windows setup is unsigned. Use only OS-provided Open Anyway / Run anyway options when offered. Do not disable platform security. Managed machines may block these apps entirely.
- [ ] Only promote a stable release once these gates pass. If a gate fails, stop promotion and document the actual limitation.

## 3. Repository settings (owner only)

- [ ] Description and topics, from [launch copy](launch-copy.md#repository-description-and-topics):

  ```bash
  gh repo edit anon5376/agent-communication-system \
    --description "Run different coding agents together without being their message bus: task claims, crash recovery and independent review for Claude Code, Codex and other CLIs, in one local SQLite file." \
    --add-topic mcp,mcp-server,model-context-protocol,ai-agents,multi-agent,agent-orchestration,coding-agents,claude-code,codex,sqlite,local-first,cli,tui,typescript,rust,developer-tools
  ```

- [ ] Social preview: Settings → General → Social preview → upload `docs/assets/social-preview.png`.
- [ ] Optional: turn on Discussions for "how do I…" questions.

## 4. Listings (optional, after the release gates)

- [ ] npm: `npm publish` from `main` publishes the `agent-communication-system` package (commands `qagent` and `agent-bus`). Then set `ACS_CHECK_NPM=1` once and run `npm run audit:public` so the README install check knows the package exists.
- [ ] MCP registries: `server.json`, `glama.json` and `smithery.yaml` are in the repo; submission steps are in [submission-pack.md](submission-pack.md). Its numbers predate this checklist; check them against the claims table first.

## 5. Posting (after the release and repository gates)

- [ ] Show HN, using the draft in [launch copy](launch-copy.md#show-hn). Be around for the first two hours to answer.
- [ ] One Reddit post, a day or more later, not cross-posted the same hour.
- [ ] One X or Bluesky post linking the repo.
- [ ] Anyone you ask to share it (for example a friend with an audience) should try `aos demo` first and post only if it worked for them.

Answer every issue opened in the first week. "This didn't work for me" reports are the most useful thing a launch produces.
