# Running ACS for free — zero-subscription setup guide

Everything below works with **no paid AI subscription and no credit card on the
best path**. You combine a free agent CLI (the "brains") with ACS (the
coordination layer — always free, always local).

> Good defaults if you just want it working: **Gemini CLI** for the smart roles
> (1,000 requests/day free with a personal Google account) + **OpenCode Zen's
> free models** for workers.

---

## Your options, ranked

| # | Path | Cost | What you need | Limits | Best for |
|---|------|------|---------------|--------|----------|
| 1 | **Gemini CLI** | Free | Personal Google account | ~60 req/min, ~1,000 req/day | Planner, reviewer, research — the strongest free option |
| 2 | **OpenCode Zen free models** | $0 models | Free OpenCode Zen account + API key | Free-tier models rotate; Zen asks for billing details at signup | Workers — many models to spread across agents |
| 3 | **Ollama (local models)** | Free forever | ~8+ GB RAM, no account at all | Slower; small models only on weak hardware | Fully-offline use, cheap workers |
| 4 | **OpenRouter `:free` models** | $0 | Free OpenRouter account + API key | 50 req/day (or 1,000/day if you ever bought $10 credit) | Backup workers |
| 5 | **Groq free tier** | $0 | Free Groq API key | ~30 req/min, ~1,000 req/day | Fast cheap workers (gpt-oss, qwen3, kimi-k2) |
| 6 | **GitHub Models** | Free | Free GitHub account | Per-model daily limits | Extra capacity via an OpenAI-compatible endpoint |

`qagent doctor <agent> <project>` checks an agent's identity, its configuration
entry, and that its CLI is on PATH. It does not check logins; each option below
gives its install and login commands.

---

## Option 1 — Gemini CLI (recommended core)

The generous free tier needs only a personal Google account — **no card, no API
key setup**. 1,000 model requests/day, Gemini 3 models, 1M-token context.

```sh
npm install -g @google/gemini-cli
gemini            # first run: choose "Login with Google"
```

That's the whole setup. In your supervisor config (`.qagent/config.json`, see
[Putting a free team on the bus](#putting-a-free-team-on-the-bus)):

```json
"models": {
  "gemini-free": {
    "id": "gemini-free",
    "provider": "google",
    "harness": "gemini",
    "family": "gemini",
    "enabled": true,
    "capabilities": {
      "contextTokens": 1000000,
      "costClass": "low",
      "reasoning": 0.85, "planning": 0.8, "coding": 0.8, "debugging": 0.78,
      "research": 0.9, "toolUse": 0.85, "reliability": 0.8, "autonomy": 0.8,
      "speed": 0.7, "tokenEfficiency": 0.9
    }
  }
}
```

(Leave out `exactModel` — the CLI picks the right free model. Set
`"exactModel": "gemini-3-flash"` style selectors only if you want to pin one.)

## Option 2 — OpenCode Zen free models (free workers)

Zen is OpenCode's curated model relay. Several models are **$0 promotional
"Free" variants**: `big-pickle`, `space-bunny-free`, `mimo-v2.6-flash-free`,
`mimo-v2.5-free`, `nemotron-3-ultra-free`, `nemotron-3.5-lightning-free`,
`muse-spark-1.3-contributor-free`, `jev-1.13-free`, `deepseek-v4-flash-free`,
`longcat-2.5-preview-free`, `ling-3.0-flash-fin-free` — the list rotates, check
https://opencode.ai/docs/zen/ or run `opencode models`.

Honest catch: Zen signup asks for billing details even though the free models
cost $0 — creating the account is free and you're never charged for `*-free`
models.

```sh
curl -fsSL https://opencode.ai/install | bash   # or: npm install -g opencode-ai
opencode auth login                             # pick OpenCode Zen, paste API key
opencode models                                 # see what *-free selectors exist
```

Then a model entry per free model you want on the bus:

```json
"mimo-flash-free": {
  "id": "mimo-flash-free",
  "provider": "opencode",
  "harness": "opencode",
  "family": "mimo",
  "exactModel": "opencode/mimo-v2.6-flash-free",
  "enabled": true,
  "capabilities": {
    "contextTokens": 128000, "costClass": "low",
    "coding": 0.7, "reasoning": 0.7, "planning": 0.65, "debugging": 0.7,
    "research": 0.6, "toolUse": 0.7, "speed": 0.8, "tokenEfficiency": 0.9,
    "reliability": 0.65, "autonomy": 0.7
  }
}
```

The `exactModel` selector is `provider/model` exactly as `opencode models`
prints it — if the catalog shows a different id for a free model, use that.

## Option 3 — Ollama (fully local, no accounts)

Zero accounts, zero network dependency, zero cost — you pay in RAM/CPU instead.
On an ~8 GB machine stick to ≤7–8B models.

```sh
curl -fsSL https://ollama.com/install.sh | sh     # macOS: brew install ollama
ollama pull qwen3:8b                              # ~5.2 GB; tool-calling coder
```

Pick a **tool-calling** model — the agent drives ACS's qagent tools over MCP,
so tool calls must be native. Verified: `qwen2.5-coder:7b` fails here (it
prints `{"name": "read", ...}` as plain text instead of calling the tool);
`qwen3`, `llama3.1`, and `mistral-nemo` support real tool calls in Ollama.

Two ways in:

- **Via OpenCode** (recommended): OpenCode has a native Ollama provider — once
  `ollama serve` is running, `opencode models` lists `ollama/qwen2.5-coder:7b`
  style selectors; use them as `exactModel` on an `opencode` harness entry.
- **Via Codex `--oss`**: the codex adapter supports local providers
  (`"harnessOptions": {"localProvider": "ollama"}`) — see
  `docs/provider-support.md`. Install Codex only if you take this route.

```json
"local-qwen": {
  "id": "local-qwen",
  "provider": "ollama",
  "harness": "opencode",
  "family": "qwen",
  "exactModel": "ollama/qwen3:8b",
  "enabled": true,
  "capabilities": {
    "contextTokens": 32768, "costClass": "local",
    "coding": 0.5, "reasoning": 0.45, "planning": 0.4, "debugging": 0.5,
    "research": 0.4, "toolUse": 0.5, "speed": 0.5, "tokenEfficiency": 1.0,
    "reliability": 0.6, "autonomy": 0.5
  }
}
```

## Option 4 — OpenRouter `:free` (backup capacity)

Free account + API key at https://openrouter.ai — models with a `:free` suffix
(deepseek, qwen, llama variants) are $0. Limit is 50 free requests/day unless
you've ever bought $10 of credit (then 1,000/day). Route through OpenCode's
built-in OpenRouter provider (`opencode auth login` → OpenRouter) and use
`openrouter/<model>:free` selectors, or point a generic/`openai-compatible`
endpoint at it directly.

## Options 5–6 — Groq and GitHub Models

- **Groq**: free API key, ~30 RPM / ~1,000 req/day on `openai/gpt-oss-120b`,
  `qwen3.x-27b`, `moonshotai/kimi-k2-instruct`, `llama-4-scout`. Fast inference —
  great for `cheap-worker`. Reach it through OpenCode's Groq provider or an
  OpenAI-compatible endpoint.
- **GitHub Models**: free with any GitHub account (github.com/marketplace/models)
  — an OpenAI-compatible endpoint with per-model daily caps.

---

## Putting a free team on the bus

Two layers to set up: **bus identities** (created with `qagent agent add`, which
writes each agent's token) and **config entries** in the project's
`.qagent/config.json` telling the supervisor which harness/model each identity
runs. Start that file from a copy of the shipped `agent-bus.config.json` at the
root of your ACS checkout; it holds the roles, routing and constraints every
config needs, and its `providers`, `harnesses`, `models` and `agents` start
empty.

### Recommended preset — all-free OpenCode team

```
planner (muse-spark, xhigh) → orchestrator (muse-spark, xhigh) → worker-1/2/3 (mimo-flash)
```

1. Register the identities:

```sh
qagent init                        # once, creates ~/.agent-bus
qagent agent add planner --role planner --authority manager   # manager => canDelegate
qagent agent add lead --role manager --authority manager
qagent agent add worker-1 --role implementation --authority worker
qagent agent add worker-2 --role implementation --authority worker
qagent agent add worker-3 --role implementation --authority worker
```

2. Create the config in your project and add the provider and harness each
   model refers to (`google`/`gemini` is only needed for the Option 1 model,
   `ollama` only for the Option 3 model):

```sh
mkdir -p .qagent
cp <acs-checkout>/agent-bus.config.json .qagent/config.json
```

```json
"providers": {
  "google": {
    "id": "google", "displayName": "Google", "authKind": "subscription",
    "authSource": "Gemini CLI login", "subscriptionBacked": true, "enabled": true
  },
  "opencode": {
    "id": "opencode", "displayName": "OpenCode", "authKind": "subscription",
    "authSource": "OpenCode provider account", "subscriptionBacked": true, "enabled": true
  },
  "ollama": {
    "id": "ollama", "displayName": "Ollama", "authKind": "local",
    "authSource": "Local Ollama runtime", "subscriptionBacked": false, "enabled": true
  }
},
"harnesses": {
  "gemini": {
    "id": "gemini", "adapter": "gemini", "command": "gemini", "providers": ["google"],
    "enabled": true,
    "features": {
      "headless": true, "resume": true, "mcp": true, "structuredOutput": true,
      "streaming": true, "cancellation": true, "modelSelection": true,
      "reasoningControl": false, "usageReporting": false
    }
  },
  "opencode": {
    "id": "opencode", "adapter": "opencode", "command": "opencode", "providers": ["opencode", "ollama"],
    "enabled": true,
    "features": {
      "headless": true, "resume": true, "mcp": true, "structuredOutput": true,
      "streaming": true, "cancellation": true, "modelSelection": true,
      "reasoningControl": true, "usageReporting": true
    }
  }
}
```

3. Add the model entries (capabilities are required — every score must be
   present, `0`–`1`):

```json
"models": {
  "muse-spark-free": {
    "id": "muse-spark-free",
    "provider": "opencode",
    "harness": "opencode",
    "family": "muse",
    "exactModel": "opencode/muse-spark-1.3-contributor-free",
    "enabled": true,
    "capabilities": {
      "contextTokens": 128000, "costClass": "low",
      "coding": 0.8, "reasoning": 0.85, "planning": 0.85, "debugging": 0.75,
      "research": 0.75, "toolUse": 0.8, "speed": 0.6, "tokenEfficiency": 0.8,
      "reliability": 0.75, "autonomy": 0.8
    }
  },
  "mimo-flash-free": {
    "id": "mimo-flash-free",
    "provider": "opencode",
    "harness": "opencode",
    "family": "mimo",
    "exactModel": "opencode/mimo-v2.6-flash-free",
    "enabled": true,
    "capabilities": {
      "contextTokens": 128000, "costClass": "low",
      "coding": 0.7, "reasoning": 0.7, "planning": 0.65, "debugging": 0.7,
      "research": 0.6, "toolUse": 0.7, "speed": 0.8, "tokenEfficiency": 0.9,
      "reliability": 0.65, "autonomy": 0.7
    }
  }
}
```

4. Add the agents — `harnessOptions.variant: "xhigh"` selects the high-effort
   reasoning variant on the muse-spark agents:

```json
"agents": {
  "planner": {
    "id": "planner", "model": "muse-spark-free", "role": "planner",
    "authority": "manager", "description": "Plans and decomposes objectives.",
    "enabled": true, "autoStart": true,
    "harnessOptions": { "variant": "xhigh" },
    "permissions": {
      "canDelegate": true, "canReview": false, "filesystem": "read",
      "shell": true, "network": true, "maxDelegationDepth": 2
    }
  },
  "lead": {
    "id": "lead", "model": "muse-spark-free", "role": "manager",
    "authority": "manager", "description": "Orchestrates the team: assigns work and gates reviews.",
    "enabled": true, "autoStart": true,
    "harnessOptions": { "variant": "xhigh" },
    "permissions": {
      "canDelegate": true, "canReview": true, "filesystem": "write",
      "shell": true, "network": true, "maxDelegationDepth": 4
    }
  },
  "worker-1": {
    "id": "worker-1", "model": "mimo-flash-free", "role": "implementation",
    "authority": "worker", "description": "Implementation worker.",
    "enabled": true, "autoStart": true,
    "permissions": {
      "canDelegate": false, "canReview": false, "filesystem": "write",
      "shell": true, "network": true, "maxDelegationDepth": 0
    }
  },
  "worker-2": {
    "id": "worker-2", "model": "mimo-flash-free", "role": "implementation",
    "authority": "worker", "description": "Implementation worker.",
    "enabled": true, "autoStart": true,
    "permissions": {
      "canDelegate": false, "canReview": false, "filesystem": "write",
      "shell": true, "network": true, "maxDelegationDepth": 0
    }
  },
  "worker-3": {
    "id": "worker-3", "model": "mimo-flash-free", "role": "implementation",
    "authority": "worker", "description": "Implementation worker.",
    "enabled": true, "autoStart": true,
    "permissions": {
      "canDelegate": false, "canReview": false, "filesystem": "write",
      "shell": true, "network": true, "maxDelegationDepth": 0
    }
  }
}
```

Flow: you send a goal to `planner` → it mails `lead` a task graph → `lead`
assigns tasks to the `worker-*` pool → workers claim, implement, and submit →
`lead` reviews and releases. If `variant: "xhigh"` is rejected by your OpenCode
version, drop it or run `opencode models --verbose` to see the variant names
your catalog exposes.

Optional upgrades: add a `reviewer` agent on a **different model family** than
the workers (a good practice, e.g. `gemini-free`; ACS does not enforce it yet), and a
`cheap-worker` on Ollama/Groq for lookups and summaries.

Then:

```sh
qagent doctor planner .           # identity, config entry, CLI on PATH (not login)
qagent supervise planner .        # one supervisor per agent (a terminal/tab each)
# or, on newer versions: qagent supervise --roster .
```

### Watching the team — T3 Code (recommended front-end)

Running `qagent supervise` per agent means juggling terminal tabs. For a
friendlier surface, use **[T3 Code](https://t3.codes)** (`pingdotgg/t3code`)
— a free, open-source control surface for the agent CLIs you already have:
every agent thread lives in one GUI (desktop app, web app, and iOS/Android),
so you can watch all the OpenCode conversations at a glance, switch models
mid-thread, and one-button open a PR from a thread's branch. It drives the
same OpenCode install — no extra subscription on top of the free models.

```sh
brew install --cask t3-code     # macOS
winget install T3Tools.T3Code   # Windows
yay -S t3code-bin               # Linux (AUR) — or grab a release from GitHub
```

Prerequisite per its docs: have at least one provider authenticated — for
this preset that's `opencode auth login` (the same Zen account from Option 2,
$0). Use it alongside ACS: the bus coordinates agents durably, T3 Code is
where you watch and steer the threads.

With the Rust build, `acs` opens the TUI — its first-run wizard can create this
team for you and drops per-agent charters into their inboxes.

## Realistic expectations

- **Free ≠ unlimited.** 1,000 req/day on Gemini is plenty for a small team;
  OpenRouter's 50/day runs out fast — keep it as backup.
- **Free models rotate.** Zen's `*-free` list is promotional and changes —
  if a model 404s, run `opencode models` and swap the `exactModel`.
- **Quality** is a step below paid frontier models. Compensate with more
  review gates: route through `reviewer` before accepting submissions.
- **Rate-limit errors are normal.** Agents just retry; the bus queues work
  durably so nothing is lost while a worker waits out a cap.
