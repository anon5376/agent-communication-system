# Universal multi-model harness prototype: engineering report

## Result

The repository now contains a working local control-plane prototype rather than an architecture-only proposal. It can represent multiple providers, harnesses, exact models, model families, agents and roles; route graph tasks with explicit reasoning; preserve independent native CLI sessions; persist workflow state; prevent broker-created overlapping writes; exercise failure/review loops with fake agents; and expose why each task was assigned.

## Existing architecture retained

The original project got several difficult things right:

- independent agent processes instead of one giant conversation;
- blocking broker waits so idle agents consume no model turns;
- supervisor wakeup/retry behaviour;
- MCP integration for native coding agents;
- task submission and review cycles;
- local-first operation and process abort controls.

Those pieces remain the execution substrate. The prototype refactors hard-coded provider logic into adapters and replaces in-memory/flat-task assumptions.

## Implemented changes

### Configuration and model representation

`agent-bus.config.json` explicitly separates providers, authentication sources, harnesses, models, families, agents, roles, routing weights and global constraints. Capability profiles include source labels and remain distinct from observed telemetry.

### Normalized harnesses

`src/adapters.ts` provides a common invocation/result contract for Claude Code, Codex CLI, Gemini CLI, Kimi Code, OpenCode, Hermes, Grok and a deterministic fake harness. It isolates command syntax, MCP injection, resume behaviour, model selection, permissions, structured-output parsing, usage extraction, discovery and timeouts.

### Durable control plane

`src/store.ts` uses Node's built-in SQLite module for identities, agents, mail, runs, tasks, route history, usage, model telemetry, project knowledge and path leases. Broker restart reconstructs active state and preserves provisioned credentials.

### Secure identity

The unauthenticated registration/token-reissue flaw is removed. Provisioning is operator-only, tokens are private, only hashes persist, registration proves possession and authority is bound to the identity rather than prompt content.

### Task graph and scheduling

Tasks have parent/child relationships, dependencies, depth limits, roles, complexity, context estimates, references, validation requirements, retries, structured results, review state, usage and route history. The broker releases blocked tasks when dependencies/capacity/path scopes permit.

### Explicit routing

`src/router.ts` performs hard eligibility checks and deterministic scoring. It considers capability fit, complexity, context, permissions, provider/family constraints, subscription preference, cost class, load, family diversity and observed outcomes. Every candidate and rejection reason is inspectable.

### Concurrency control

Write tasks acquire normalized project path leases. Overlapping scopes are blocked; read-only tasks can proceed concurrently. This is the smallest maintainable race-prevention mechanism for the prototype.

### Token-efficient handoff

Tasks carry scoped briefs, references and validation contracts. Results carry summaries, changed paths, artifacts and observations. The manager need not consume raw worker transcripts.

### Operator CLI and MCP

The CLI now creates runs, provisions agents, starts supervisors, previews routes, probes/discovers models, renders graph state and reports usage. MCP tools cover routing, graph delegation, structured completion, review, cancellation and project knowledge.

## Automated coverage

The test suite uses temporary SQLite databases and deterministic fake harnesses. It covers:

- provisioning, registration and attempted identity theft;
- sender/assignee authorization;
- durable mail and message delivery;
- task creation, start, submission, rejection, revision and acceptance;
- dependencies and blocked-task release;
- overlapping write-scope prevention;
- cancellation and lease release;
- retry, fallback and escalation;
- restart recovery with the same credentials and graph;
- deterministic routing, exact-model/family policy and independent review;
- adapter command normalization and malformed output;
- fake harness execution and usage reporting;
- telemetry persistence and routing input.

Validation command:

```bash
npm test
```

GitHub Actions installs from the lockfile, compiles production and test TypeScript, and runs the complete suite without external model calls.

## Provider verification disposition

### Implemented and tested without external usage

- broker/control plane;
- SQLite persistence;
- authentication/authority;
- graph tasks, retries, cancellation and path leases;
- deterministic router;
- adapter normalization and fake harness.

### Implemented but not re-verified against a live subscription in this PR

- Claude Code;
- Codex CLI;
- Gemini CLI;
- Kimi Code;
- OpenCode.

The original repository had manually exercised several of these CLIs, but this prototype does not spend user subscription quota merely to make CI green.

### Adapter-ready or researched

- Hermes adapter;
- Ollama/local models through Codex `--oss` or OpenCode;
- LM Studio through a compatible endpoint/harness;
- additional providers through the same adapter contract.

### Unsupported by default

- Grok/xAI subscription CLI operation remains disabled until a current official path is locally validated.

## Tradeoffs

- SQLite JSON payloads were chosen over a complex ORM. The state model remains inspectable and migrations stay small.
- Cooperative path leases were chosen over immediate worktree automation. They prevent obvious races without creating a Git orchestration subsystem before routing is proven.
- Routing is deterministic and configurable. A learned black box would be premature without reliable task history.
- Native CLIs remain responsible for repository tools. The broker orchestrates them rather than replacing their strengths.
- Fake harnesses prove orchestration without spending paid usage or depending on flaky provider availability.

## Known limitations

- No OS-level per-task filesystem/shell/network confinement.
- No transactional worktree/patch merge path.
- Remaining provider quota is recorded only where a CLI exposes defensible telemetry.
- Live provider flags and output formats can drift and require adapter maintenance.
- The SwiftUI application does not yet expose every catalog/routing field available from the backend.
- Automatic decomposition quality still depends on the selected manager model and its use of graph-task tools.
- No long-running live heterogeneous subscription run was executed in CI.

## Next engineering sequence

1. Add per-task Git worktrees and patch-only integration for write tasks.
2. Add a GUI catalog editor for providers, accounts, models, roles and routing constraints.
3. Add installed-CLI-specific capability probes and safe model discovery.
4. Gather benchmark/task telemetry and calibrate routing priors.
5. Add explicit quota/budget adapters only where providers expose supported interfaces.
6. Add artifact hashing/signing and final integration provenance.
7. Run bounded live end-to-end validation across at least Claude Code, Codex and one independent reviewer family.

The prototype is deliberately a small control plane with strong interfaces, not a Kubernetes parody.
