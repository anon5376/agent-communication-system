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

Anything missing or misconfigured is caught by `qagent doctor`, which prints the
exact install/login command per provider.

---

## Option 1 — Gemini CLI (recommended core)

The generous free tier needs only a personal Google account — **no card, no API
key setup**. 1,000 model requests/day, Gemini 3 models, 1M-token context.

```sh
npm install -g @google/gemini-cli
gemini            # first run: choose "Login with Google"
```

That's the whole setup. In your `agent-bus.config.json`:

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
    "coding": 0.7, "reasoning": 0.7, "toolUse": 0.7, "reliability": 0.65,
    "speed": 0.8, "tokenEfficiency": 0.9
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
ollama pull qwen2.5-coder:7b                      # ~4.7 GB; good small coder
```

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
  "exactModel": "ollama/qwen2.5-coder:7b",
  "enabled": true,
  "capabilities": {
    "contextTokens": 32768, "costClass": "local",
    "coding": 0.5, "reasoning": 0.45, "toolUse": 0.5, "reliability": 0.6,
    "speed": 0.5, "tokenEfficiency": 1.0
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

Agents are roles + a model. A sensible all-free team (edit `agent-bus.config.json`
in your bus dir, `~/.agent-bus/` by default, or the project config):

```json
"agents": {
  "planner":  { "id": "planner",  "model": "muse-spark-free", "role": "planner",        "authority": "worker",  "enabled": true },
  "lead":     { "id": "lead",     "model": "gemini-free",     "role": "manager",        "authority": "manager", "enabled": true },
  "coder-1":  { "id": "coder-1",  "model": "mimo-flash-free", "role": "implementation", "authority": "worker",  "enabled": true },
  "coder-2":  { "id": "coder-2",  "model": "mimo-flash-free", "role": "implementation", "authority": "worker",  "enabled": true },
  "scout":    { "id": "scout",    "model": "gemini-free",     "role": "research",       "authority": "worker",  "enabled": true },
  "reviewer": { "id": "reviewer", "model": "gemini-free",     "role": "reviewer",       "authority": "worker",  "enabled": true }
}
```

Rules of thumb: give **planner/manager/reviewer** the strongest free brain you
have (Gemini free tier or `muse-spark-1.3-contributor-free`), give
**implementation** your bulk workers (`mimo-*-free`), and let **cheap-worker**
eat Ollama or Groq calls. Mix `family` values across reviewers and workers —
independent review requires a different model family.

Then:

```sh
qagent init                       # once, creates ~/.agent-bus
qagent doctor                     # verifies every harness/login is reachable
qagent supervise planner .        # one supervisor per agent (a terminal/tab each)
# or, on newer versions: qagent supervise --roster .
```

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
