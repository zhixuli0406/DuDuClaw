# Multi-Runtime Agent Execution

> One platform, twelve AI backends — Claude, Codex, Gemini, Antigravity, Grok, Qwen Code, Kimi Code, GitHub Copilot CLI, Kiro, Cursor, Mistral Vibe, OpenCode, and any OpenAI-compatible endpoint.

---

## The Metaphor: A Multilingual Office

Imagine an office that needs translators. Instead of hiring one translator who only speaks French, you build a translation desk that can assign work to a French speaker, a German speaker, a Japanese speaker, or any freelancer who speaks the client's language.

The desk doesn't care *which* translator handles the job — it cares that the translation is done well. If the French translator is busy, it routes to the next available one.

DuDuClaw's Multi-Runtime architecture is that translation desk — but for AI backends.

---

## How It Works

### The AgentRuntime Trait

At the core is a unified interface (`AgentRuntime`) that all backends implement:

```
AgentRuntime trait:
  fn execute(prompt, tools, context) → Response
  fn stream(prompt, tools, context) → Stream<Event>
  fn health_check() → Status
```

Every backend — Claude, Codex, Gemini, or any OpenAI-compatible endpoint — implements this same interface. The rest of the system doesn't know or care which backend is handling a particular request.

### The Runtime Catalog

Every backend is described once, in a single compile-time table
(`crates/duduclaw-core/src/runtime_catalog.rs`). Detection, one-click install,
model discovery, CLI login and model↔provider inference all read that table —
so a runtime cannot be installable but undetectable, or configurable but
unloggable-into.

| Runtime | Binary | Install channel | Headless invocation | Output | Login | Credential store |
|---|---|---|---|---|---|---|
| Claude Code | `claude` | npm `@anthropic-ai/claude-code` | `-p <prompt> --output-format stream-json` | jsonl | `claude setup-token` (paste-back) | `~/.claude/.credentials.json` |
| OpenAI Codex | `codex` | npm `@openai/codex` | `exec --json <prompt>` | jsonl | `codex login` (localhost callback) | `~/.codex/auth.json` |
| Gemini CLI | `gemini` | npm `@google/gemini-cli` | `-p --output-format stream-json <prompt>` | jsonl | `gemini auth login` (localhost callback) | `~/.gemini/oauth_creds.json` |
| Google Antigravity | `agy` | `antigravity.google/cli/install.sh` | `-p <prompt>` | text | `agy login` (localhost callback) | — |
| Grok Build | `grok` | `x.ai/cli/install.sh` (manual) | `-p <prompt>` | text | `grok login --device-code` | `~/.grok/auth.json` |
| Qwen Code | `qwen` | npm `@qwen-code/qwen-code` | `-p <prompt> --yolo --output-format json` | json | none (API key only) | `~/.qwen/.env` |
| Kimi Code | `kimi` | npm `@moonshot-ai/kimi-code` | `-p <prompt> --output-format stream-json` | jsonl | `kimi login` (device code) | `~/.kimi-code/credentials/` |
| GitHub Copilot CLI | `copilot` | npm `@github/copilot` | `-p <prompt> -s --no-ask-user --allow-all-tools` | text | `copilot login --device-code` | `~/.copilot/config.json` |
| Kiro CLI | `kiro-cli` | `cli.kiro.dev/install` (manual) | `chat --no-interactive --trust-all-tools <prompt>` | text | `kiro-cli login --use-device-flow` | `~/.kiro/settings/cli.json` |
| Cursor CLI | `cursor-agent` | `cursor.com/install` | `-p <prompt> --force --output-format json` | json | `cursor-agent login` (browser) | `~/.cursor/cli-config.json` |
| Mistral Vibe | `vibe` | PyPI `mistral-vibe` | `-p <prompt> --yolo --trust --output json` | json | none (API key only) | `~/.vibe/.env` |
| OpenCode | `opencode` | `opencode.ai/install` | `run <prompt> --auto --format json` | jsonl | `opencode auth login` | `~/.local/share/opencode/auth.json` |
| OpenAI-compatible | *(HTTP)* | — | — | json | none (API key only) | — |

Model selection differs per vendor and the catalog records which form each one
takes: a separate flag (`--model <id>`), the joined form Copilot documents
(`--model=<id>`), an environment variable for CLIs with no flag at all (Mistral
Vibe's `VIBE_ACTIVE_MODEL`), or nothing (Kiro selects its model through
`kiro-cli settings`, not per call).

**Vendor terms worth reading before you switch a runtime on:**

- **Kiro** — AWS's FAQ states that use through third-party automation harnesses
  that route requests outside Kiro's native interfaces is *not permitted*.
  Driving Kiro from DuDuClaw is exactly that. Calling `kiro-cli` directly from
  your own CI is allowed. This is why Kiro's install channel is manual: the
  decision is yours to make deliberately.
- **Anthropic / Google** — since 2026-03, consumer subscription tokens used by
  third-party products are blocked server-side, and accounts have been
  suspended. Use an API key.
- **Qwen** — the free OAuth tier was discontinued 2026-04-15. API key
  (ModelStudio / DashScope) only.
- **OpenCode** — MIT with no restriction of its own, but it removed its
  Anthropic subscription plugin in 1.3.0 for the reason above. Use provider API
  keys.
- **OpenAI** — policy on third-party products driving a ChatGPT subscription
  login is unclear. API key is the supported path.

The dashboard shows the relevant note and requires an explicit "I understand
the risk" acknowledgement before starting a subscription login.

### How Backends Are Driven

Five backends have bespoke runtime modules, because each has real per-vendor
wiring that is not shareable — account rotation (Claude), MCP config injection
in the CLI's own format, capability→sandbox-flag translation, PTY recovery for
empty-output failures. Everything else is driven by **one** generic print-mode
runtime (`runtime/generic_cli.rs`) built straight from the catalog entry: spawn
the binary with the templated argv, deliver the prompt as an argument or on
stdin, parse text / JSON / JSONL back to the final assistant text, and map a
non-zero exit or an auth-required marker to a typed failure the failover chain
understands.

### The Original Four Backends

**Claude Runtime** — Calls the Claude Code CLI (`claude`) with JSONL streaming output. This is the most feature-rich backend, with native MCP tool support, bash execution, web search, and file operations built in.

```
Agent config: runtime = "claude"
     |
     v
Spawn: claude --json --print ...
     |
     v
Parse JSONL streaming events
     |
     v
Extract response + tool calls
```

**Codex Runtime** — Calls the OpenAI Codex CLI with `--json` flag for structured streaming events.

```
Agent config: runtime = "codex"
     |
     v
Spawn: codex --json ...
     |
     v
Parse JSONL STDOUT events
     |
     v
Extract response
```

**Gemini Runtime** — Calls the Google Gemini CLI with `--output-format stream-json` for structured output.

```
Agent config: runtime = "gemini"
     |
     v
Spawn: gemini --output-format stream-json ...
     |
     v
Parse streaming JSON events
     |
     v
Extract response
```

**OpenAI-compatible Runtime** — Calls any HTTP endpoint that speaks the OpenAI chat completions API (MiniMax, DeepSeek, local servers, etc.).

```
Agent config: runtime = "openai-compat"
              api_url = "http://localhost:8080/v1"
     |
     v
HTTP POST /v1/chat/completions
     |
     v
Parse SSE stream
     |
     v
Extract response
```

### RuntimeRegistry: Auto-Detection

When DuDuClaw starts, the **RuntimeRegistry** scans the system for available CLI tools:

```
Startup scan (one loop over the runtime catalog):
     |
     v
  For each catalog entry with a binary:
     PATH → ~/.local/bin, Homebrew, bun/volta/npm-global/asdf shims,
     /opt/duduclaw/runtimes/bin, /usr/bin, /bin
       found? → register (bespoke module if it has one, else generic print-mode)
     |
     v
  Always: OpenAI-compat (an HTTP endpoint, gated on an API key, not a binary)
     |
     v
Registry knows which backends are available
```

`/opt/duduclaw/runtimes/bin` is where the DuDuClaw OS appliance image lands its
bundled CLIs, so a runtime shipped in the image is discovered even when the
gateway inherited no interactive `PATH`.

Agents can specify their preferred runtime in `agent.toml`:

```toml
[runtime]
preferred = "claude"    # Primary backend
fallback = "gemini"     # If primary is unavailable
```

If no preference is set, the registry uses the first available backend.

### Per-Agent Configuration

Different agents can use different backends simultaneously:

```
Agent "dudu" (customer support)  → Claude (best reasoning)
Agent "coder" (code generation)  → Codex (optimized for code)
Agent "analyst" (data analysis)  → Gemini (large context window)
Agent "local" (privacy-sensitive) → OpenAI-compat (local endpoint)
```

This means a single DuDuClaw installation can orchestrate agents across multiple AI providers, each using the backend best suited to their task.

---

## Cross-Provider Failover

When a backend becomes unavailable (rate-limited, down, or erroring), the **FailoverManager** automatically switches to the next available backend:

```
Claude runtime: rate-limited (cooldown: 2min)
     |
     v
FailoverManager checks agent config:
  fallback = "gemini"
     |
     v
Route to Gemini runtime
     |
     v
When Claude cools down → restore primary routing
```

The failover is transparent to the user — they see a response, regardless of which backend handled it. Health states are tracked independently per backend:

- **Healthy**: Normal operation
- **Rate-Limited**: Short cooldown (2 minutes)
- **Error**: Exponential backoff
- **Non-Retryable**: Manual intervention needed (auth failure, billing)

---

## Why This Matters

### No Vendor Lock-In

DuDuClaw doesn't bet on a single AI provider. If Claude raises prices, you can shift agents to Codex or Gemini. If Gemini adds a killer feature, you can adopt it without rebuilding your infrastructure.

### Best Tool for Each Job

Code generation might work better on Codex. Complex reasoning might work better on Claude. Data analysis might benefit from Gemini's large context window. Multi-Runtime lets you match the right brain to the right task.

### Resilience

If one provider goes down, the others keep your agents running. Combined with local inference fallback, DuDuClaw can survive any single-provider outage.

### Cost Optimization

Different providers have different pricing. The `LeastCost` rotation strategy can route to whichever provider offers the best price/performance for each query type.

---

## Interaction with Other Systems

- **Account Rotator**: Manages credentials across all providers, with cross-provider failover.
- **Confidence Router**: Sits below the runtime layer — decides local vs. cloud. The runtime layer decides *which* cloud.
- **CostTelemetry**: Tracks cost per provider, enabling informed routing decisions.
- **MCP Server**: Tools are exposed to all backends that support them (Claude via native MCP, others via tool injection).
- **Agent Config**: Each agent's `agent.toml` specifies its runtime preference and fallback chain.

---

## Provider-Aware Accounts (WP-A, 2026-09)

`accounts.add` — the gateway RPC behind both the dashboard's Accounts page and the OOBE "AI Runtime Authorization" step — accepts a `provider` id alongside the existing `type` (`api_key` | `oauth`). Accepted ids are the platform's canonical provider table (`duduclaw_core::provider_env::KNOWN_PROVIDER_IDS`): `anthropic`, `openai`, `gemini`/`google`, `deepseek`, `minimax`, `groq`, `together`, `mistral`, `openrouter`, `xai`, `qwen`. Omitting `provider` defaults to `"anthropic"`, so every caller written before this feature keeps working byte-for-byte; an unrecognized id is rejected instead of silently accepted.

The credential still lands in `config.toml`'s `[[accounts]]` array, now tagged with its provider:

```toml
[[accounts]]
id = "openai-prod"
type = "api_key"
provider = "openai"
api_key_enc = "..."          # anthropic keeps the legacy anthropic_api_key_enc field
```

`AccountRotator::select_for_provider` — the same selection logic both the Claude CLI path and the Direct-API `duduclaw-llm` provider path already used for cross-provider Direct-API rotation — filters strictly by this field, so an OpenAI/Gemini/xAI/DeepSeek/… key added this way is picked up by the exact same rotation, budget-tracking, and cooldown machinery Anthropic accounts get. No per-provider code path was needed on the read side. `accounts.list` and `accounts.budget_summary` both return `provider` on every account so the dashboard's Accounts page can show which vendor each credential belongs to; `AddAccountDialog` offers a provider picker with a per-provider key-format hint and a "get an API key" link to the vendor's own console.

## Subscription-Login Risk Disclosure

Every one-click "sign in with your subscription" flow — the CLI login modal, the guided QR-code setup wizard, and the OOBE runtime-setup card that opens one of the two — shows a risk notice before it starts: Anthropic and Google have blocked third-party products from using consumer-subscription tokens on the server side since March 2026 and have suspended accounts over it; OpenAI's policy on this is unspecified. An explicit "I understand the risk and accept it" checkbox must be ticked before the flow's own login step (CLI subprocess, browser callback, or device-code polling) begins. The API-key path is unaffected by this gate and remains the recommended default.

---

## The Takeaway

The AI market is multi-provider. Building on a single CLI is like writing software for a single operating system — it works until it doesn't. The `AgentRuntime` trait abstracts away the differences, letting DuDuClaw treat Claude, Codex, Gemini, and any OpenAI-compatible endpoint as interchangeable backends. Your agents get the best available brain, every time.
