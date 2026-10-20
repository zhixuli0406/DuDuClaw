# Multi-Runtime Agent Execution

> One platform, twelve runtime ids: eleven CLI backends (Claude, Codex, Antigravity, Grok, Qwen Code, Kimi Code, GitHub Copilot CLI, Kiro, Cursor, Mistral Vibe, OpenCode) plus `openai_compat`, which talks HTTP to any OpenAI-compatible endpoint.

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

Every backend — Claude, Codex, Antigravity, or any OpenAI-compatible endpoint — implements this same interface. The rest of the system doesn't know or care which backend is handling a particular request.

### The Runtime Catalog

Every backend is described once, in a single compile-time table
(`crates/duduclaw-core/src/runtime_catalog.rs`). Detection, one-click install,
model discovery, CLI login and model↔provider inference all read that table —
so a runtime cannot be installable but undetectable, or configurable but
unloggable-into.

| Runtime | Binary | Install channel | Headless invocation | Output | Login | Credential store |
|---|---|---|---|---|---|---|
| Claude Code | `claude` | npm `@anthropic-ai/claude-code` | `-p <prompt> --output-format stream-json` | jsonl | `claude setup-token` (paste-back) | `~/.claude/.credentials.json` |
| OpenAI Codex | `codex` | npm `@openai/codex` | `exec --json <prompt>` | jsonl | `codex login --device-auth` (device code; Docker/remote safe) | `~/.codex/auth.json` |
| Google Antigravity | `agy` | `antigravity.google/cli/install.sh` | `-p <prompt>` | stream-json (v1.2.10) | Google sign-in via `agy` in a terminal (no `login` subcommand), or API-key mode | OS keyring |
| Grok Build | `grok` | `x.ai/cli/install.sh` (manual) | `-p <prompt>` | text | `grok login --device-code` | `~/.grok/auth.json` |
| Qwen Code | `qwen` | npm `@qwen-code/qwen-code` | `-p <prompt> --yolo --output-format json` | json | none (API key only) | `~/.qwen/.env` |
| Kimi Code | `kimi` | npm `@moonshot-ai/kimi-code` | `-p <prompt> --output-format stream-json` | jsonl | `kimi login` (device code) | `~/.kimi-code/credentials/` |
| GitHub Copilot CLI | `copilot` | npm `@github/copilot` | `-p <prompt> -s --no-ask-user --allow-all-tools` | text | `copilot login --device-code` | `~/.copilot/config.json` |
| Kiro CLI | `kiro-cli` | `cli.kiro.dev/install` (manual) | `chat --no-interactive --trust-all-tools <prompt>` | text | `kiro-cli login --use-device-flow` | `~/.kiro/settings/cli.json` |
| Cursor CLI | `cursor-agent` | `cursor.com/install` | `-p <prompt> --force --output-format json` | json | `cursor-agent login` (browser) | `~/.cursor/cli-config.json` |
| Mistral Vibe | `vibe` | PyPI `mistral-vibe` | `-p <prompt> --yolo --trust --output json` | json | none (API key only) | `~/.vibe/.env` |
| OpenCode | `opencode` | `opencode.ai/install` | `run <prompt> --auto --format json` | jsonl | `opencode auth login` | `~/.local/share/opencode/auth.json` |
| OpenAI-compatible | *(HTTP)* | — | — | json | none (API key only) | — |

**Dashboard one-click login** supports Claude, Codex, Grok, Kimi, Copilot, Kiro and OpenCode. Codex uses device authorization: open the verification page shown by the CLI, enter its code, and wait for the CLI to finish. Pasting the OAuth callback URL `code` into the dashboard does not complete login. Cursor remains a runtime, but its browser callback cannot reliably finish from a remote dashboard, so it is not in the one-click picker. Antigravity has no scripted login subcommand; Qwen and Mistral Vibe use API keys. The Accounts page API Token card can now delete a saved account after confirming its account id; this does not revoke the key at its provider.

Gemini CLI is no longer a DuDuClaw runtime or Docker package. Existing `runtime = "gemini"` settings need migration to Antigravity; the separate Gemini API provider remains supported. See [removal and migration](../guides/deprecations.md).

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
- **Anthropic** — its [legal and compliance page](https://code.claude.com/docs/en/legal-and-compliance) says third-party
  developers may not offer Claude.ai login in their own applications or route
  requests through Free, Pro or Max plan credentials on behalf of their users,
  and may not collect, store or intermediate Claude.ai credentials or session
  tokens. Signing in to the unmodified Claude Code binary with your own
  subscription is allowed; Pro and Max limits assume ordinary usage. Anthropic may enforce this without prior notice, and users have
  reported suspended accounts. An API key (Claude Console) is the documented
  path for products.
- **Google** — the [Gemini CLI terms](https://geminicli.com/docs/resources/tos-privacy/)
  say that directly accessing the services behind Gemini CLI with third-party
  software, tools or services through Gemini CLI OAuth violates its terms and
  may be grounds for suspension or termination of the account. The
  [Antigravity terms](https://antigravity.google/terms) (section 6) say the
  same of third-party software accessing Antigravity, and may suspend or
  terminate the Antigravity and/or Gemini CLI accounts; the same section also
  lists using the service in connection with products not provided by Google
  as abuse. DuDuClaw runs the
  unmodified Antigravity CLI; whether that falls under these clauses is Google's call.
  Google's [Gemini Code Assist for individuals deprecation notice](https://developers.google.com/gemini-code-assist/docs/deprecations/code-assist-individuals)
  (last updated 2026-10-09) adds that since 2026-06-18 Gemini CLI no longer
  accepts Login with Google for Gemini Code Assist for individuals, Google AI
  Pro and Google AI Ultra accounts; a paid API key (Gemini API or enterprise)
  still works. Use an API key.
- **Qwen** — the free OAuth tier was discontinued 2026-04-15. API key
  (ModelStudio / DashScope) only.
- **OpenCode** — MIT with no restriction of its own, but it removed its
  Anthropic subscription plugin in 1.3.0 for the reason above. Use provider API
  keys.
- **OpenAI** — OpenAI runs an official Sign in with ChatGPT program
  ([terms](https://openai.com/policies/sign-in-with-chatgpt-terms/), dated
  2026-09-29). The [quickstart](https://developers.openai.com/siwc/quickstart)
  says "Sign in with ChatGPT is currently available to selected commercial
  partners through a limited trial." and "ChatGPT plan usage is available to
  all open-source partners and selected private clients."; the
  [help article](https://help.openai.com/en/articles/20001542-using-your-chatgpt-plan-in-other-apps-and-sites)
  says supported open-source tools remain available and commercial partners
  apply through an interest form. A commercial product therefore needs
  OpenAI's approval. The terms also say users must be able to use their
  ChatGPT plan without paying the partner or upgrading, any persistent storage
  of auth tokens must be local and user-controlled (not in remote or managed
  environments), the login must not provide general API access for other tools
  or unrelated requests, and violations may lead to suspension of Sign in with
  ChatGPT access. OpenAI's Codex lead
  [posted](https://x.com/thsottiaux/status/2097131394199896166) on 2026-09-08,
  about a third-party app: "Sorry, but this is not an approved use of Sign in
  With ChatGPT. We support pure OSS clients and others we have partnerships
  with, but you need to reach out and talk to us for that." When DuDuClaw runs
  on a remote host or in a container, the Codex token (`~/.codex/auth.json`)
  is stored there. The [Terms of Use](https://openai.com/policies/terms-of-use/)
  still apply. An API key is the path to rely on.
- **xAI** — no published position on third-party products driving a
  SuperGrok / X Premium+ subscription was found; the
  [Terms of Service](https://x.ai/legal/terms-of-service) still apply.

The dashboard shows the relevant note and requires an explicit acknowledgement
before starting or saving a subscription login, and the gateway refuses the
call without it (see [Subscription-Login Risk Disclosure](#subscription-login-risk-disclosure)).

### How Backends Are Driven

Four CLI backends (`claude`, `codex`, `antigravity`, `grok`;
`BESPOKE_RUNTIME_IDS` in `runtime/mod.rs`) have bespoke runtime modules, because each has real per-vendor
wiring that is not shareable — account rotation (Claude), MCP config injection
in the CLI's own format, capability→sandbox-flag translation, PTY recovery for
empty-output failures. Everything else is driven by **one** generic print-mode
runtime (`runtime/generic_cli.rs`) built straight from the catalog entry: spawn
the binary with the templated argv, deliver the prompt as an argument or on
stdin, parse text / JSON / JSONL back to the final assistant text, and map a
non-zero exit or an auth-required marker to a typed failure the failover chain
understands. The seven other CLIs (Qwen Code, Kimi Code, GitHub Copilot CLI,
Kiro, Cursor, Mistral Vibe, OpenCode) go through that generic runtime.
`openai_compat` is not a CLI: it has its own HTTP module
(`runtime/openai_compat.rs`). That makes five hand-written modules in total.

### Main Backends

Claude, Codex and OpenAI-compatible endpoints are described here. Antigravity and
Grok are covered in their own sections further down.

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

Agents choose their runtime in `agent.toml`:

```toml
[runtime]
provider = "claude"          # Primary backend
fallback = "antigravity"     # If primary is unavailable
```

If `provider` is absent, the agent runs on Claude. An unrecognised value logs a warning and also falls back to Claude.

### Per-Agent Configuration

Different agents can use different backends simultaneously:

```
Agent "dudu" (customer support)  → Claude (best reasoning)
Agent "coder" (code generation)  → Codex (optimized for code)
Agent "analyst" (data analysis)  → Antigravity
Agent "local" (privacy-sensitive) → OpenAI-compat (local endpoint)
```

This means a single DuDuClaw installation can orchestrate agents across multiple AI providers, each using the backend best suited to their task.

---

## Effort

Modern reasoning models take a **depth** dial separate from the model choice: how hard to think on this one call. Every vendor spells it differently, and they do not accept the same values. DuDuClaw makes it one setting and translates.

Set it on the agent:

```toml
# <home>/agents/<id>/agent.toml
[model]
preferred = "claude-opus-5"
effort    = "high"          # low | medium | high | xhigh | max
```

Unset is the default, and it means *no flag is passed at all* — the spawn is byte-identical to a DuDuClaw without this feature, and the provider's own default depth applies.

### Per-runtime flag mapping

Probed against the installed binaries on 2026-09-24 (`research/multi-model-routing-2026-09/17-P0-cli-flag-probe.md` §4 + §6) — not inferred from docs:

| Runtime | Version probed | How effort is expressed | Values the CLI accepts |
|---|---|---|---|
| `claude` | 2.1.258 | `--effort <v>` | `low` `medium` `high` `xhigh` `max` |
| `codex` | 0.156.1 | `-c model_reasoning_effort=<v>` (config override, no dedicated flag) | `low` `medium` `high` `xhigh` |
| `antigravity` (`agy`) | 1.2.10 | `--effort <v>` | `low` `medium` `high` |
| `grok` | 1.0.41 | `--reasoning-effort <v>` (alias `--effort`) | *not enumerated by `--help`* |
| `openai_compat` | — | `reasoning_effort` in the request body | `low` `medium` `high` |

### The clamp table

Because the accepted sets differ, your setting is clamped **down** to what the target runtime will take. It is never dropped silently, and never sent as a value the CLI would reject:

| You set | claude | codex | antigravity | grok | openai_compat |
|---|---|---|---|---|---|---|
| `low` | `low` | `low` | `low` | `low` | `low` |
| `medium` | `medium` | `medium` | `medium` | `medium` | `medium` |
| `high` | `high` | `high` | `high` | `high` | `high` |
| `xhigh` | `xhigh` | `xhigh` | **`high`** | **`high`** | **`high`** |
| `max` | `max` | **`xhigh`** | **`high`** | **`high`** | **`high`** |

Grok is capped at `high` deliberately: its `--help` names the flag but does not list the values, so forwarding `xhigh`/`max` risks an "unexpected value" that takes the whole spawn down. `openai_compat` is capped for the same reason across its eight heterogeneous presets. Both ceilings live in one place (`duduclaw-core/src/effort.rs`) and can be raised once the values are confirmed against a live run.

### Direct-API mapping

The API-level path (`duduclaw-llm`) carries the same value into each vendor's native field:

| Protocol | Field |
|---|---|
| Anthropic Messages | `output_config.effort` (GA, no beta header) |
| OpenAI Responses | `reasoning.effort` |
| OpenAI-compat chat/completions | `reasoning_effort` (top-level) |
| Gemini `generateContent` | `generationConfig.thinkingConfig.thinkingLevel` — **unverified**, see below |

> The Gemini key is the one mapping that is **not** confirmed. `thinkingConfig` as the container is verified (it is what the existing `thinkingBudget` uses and ships today), but the `thinkingLevel` sibling key could not be confirmed: two fetches of ai.google.dev returned truncated `GenerationConfig` references that never named it, and the Interactions API spells it `generation_config.thinking_level`, so the camelCase `generateContent` twin is an inference. It is gated behind the field being set, so an unset effort can never send it. Re-verify before relying on it.

### Cost and cache

Two things to know before you turn this up:

- **Effort costs tokens.** It is the first quality-trading lever after the free wins (caching, prompt hygiene), and the top of the range earns its price only on genuinely hard work. Coding and long-horizon agentic tasks respond strongly; chat, classification, and high-volume routes often do fine at `low`.
- **Changing effort mid-conversation invalidates the prompt cache** on most models — effort participates in the cached prefix. Pick a value per agent and leave it; do not tune it turn by turn.

The one place effort is deliberately *not* agent-driven is the lightweight extraction path (session compression, GVU, wiki ingest), which is pinned to `medium`. Mechanical extraction should not get more expensive because a conversational agent was turned up to `max`.

### PTY pool

*(Removed 2026-09.)* Effort used to be part of the PTY pool's **session cache key**, so two calls wanting different efforts got two separate pooled sessions. The pool is gone; every spawn now carries its own `--effort` flag.

---

## Cross-Provider Failover

When a backend becomes unavailable (rate-limited, down, or erroring), the **FailoverManager** automatically switches to the next available backend:

```
Claude runtime: rate-limited (cooldown: 2min)
     |
     v
FailoverManager checks agent config:
  fallback = "antigravity"
     |
     v
Route to Antigravity runtime
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

### Codex non-interactive approvals (2026-09)

Codex 0.156.x gates every MCP tool call behind an approval request; with
`approval_policy=never` that request is auto-rejected, and neither
`mcp_servers.<id>.default_tools_approval_mode` nor `projects.<cwd>.trust_level`
changes it. `--approve-for-me` (automatic review) is the supported
non-interactive escape hatch, and it is mutually exclusive with `-s/--sandbox`.
Agent directories are not git repositories, so `--skip-git-repo-check` is
always passed and stdin is closed.

**One flag set per capability level** (changed 2026-09-28 — read the ReadOnly
row before restricting an agent):

| `[capabilities]` level | Codex flags | What the agent can do |
|---|---|---|
| ReadOnly (no write tools granted, or all denied) | `-s read-only -c approval_policy=never` | Reads and reasons. Writes are **genuinely blocked**. **Every MCP tool call is auto-rejected**, so the agent has no duduclaw tools for that run. One `warn!` per spawn says so. |
| WorkspaceWrite (the default) | `--approve-for-me -c approval_policy=never -c sandbox_mode="workspace-write"` | Writes inside the workspace; full duduclaw MCP tool surface. |
| FullAccess (explicit `computer_use = true`) | `--dangerously-bypass-approvals-and-sandbox` | No confinement. Explicit operator grant only. |

Until 2026-09-28 ReadOnly also used `--approve-for-me` plus
`-c sandbox_mode="read-only"`. That **failed open**: `--approve-for-me` runs its
automatic review in a workspace-write sandbox, so the read-only declaration was
advisory and a capability-restricted agent could still write files. It now
carries the enforcing flag instead, and the MCP tool surface is the price. If
you need an agent to keep its tools, grant it WorkspaceWrite — ReadOnly on Codex
means "may not change anything", tools included.

### Codex MCP credentials: `env_vars` on 0.157+, `argv` fallback below (2026-09-28)

A Codex spawn registers the duduclaw MCP server through per-invocation `-c`
config overrides, and two of the values that registration carries are secrets:
`DUDUCLAW_MCP_API_KEY` and `DUDUCLAW_AGENT_TOKEN`. The registration (and
everything below) happens on every Codex spawn, ReadOnly included: at ReadOnly
the server is registered, but Codex auto-rejects each call to it, as the table
above says.

**Why the credential cannot just live in the environment.** Live-probed
2026-09-28 and confirmed against the Codex source: Codex `env_clear()`s every
stdio MCP server child and re-adds only an 11-name default allowlist (`HOME`,
`PATH`, `SHELL`, `USER`, `LOGNAME`, `TERM`, `TMPDIR`, `TZ`, `LANG`, `LC_ALL`,
`__CF_USER_TEXT_ENCODING`) plus whatever the config declares. The gateway's own
process environment does not reach the MCP server, so `Command::env()` alone
delivers nothing — the config channel is the only channel.

**What DuDuClaw does now.** The config channel has two shapes, and the shape is
chosen per Codex binary from the version that binary reports:

| Codex version | Credential shape | Visible in `ps` |
|---|---|---|
| **≥ 0.157.0** | `-c mcp_servers.duduclaw.env_vars=["DUDUCLAW_MCP_API_KEY", "DUDUCLAW_AGENT_TOKEN"]`, with the values set on the Codex **process** environment, from which Codex copies them into the MCP child | variable **names** only |
| **< 0.157.0**, or version unreadable | `-c mcp_servers.duduclaw.env.<K>="<value>"` (the previous behavior) | the credential **values** |

Non-credential entries (`DUDUCLAW_HOME`, `DUDUCLAW_PORT`, `DUDUCLAW_AGENT_ID`,
`DUDUCLAW_INSTANCE`) keep the `env.<K>="<value>"` form on both paths — they are
not secrets, and keeping them in the config table means registration still works
if the process environment is ever scrubbed. "Credential" is decided by an exact
name suffix: `_API_KEY`, `_TOKEN`, `_SECRET`, `_PASSWORD` (ASCII
case-insensitive), the same shape convention `duduclaw-core`'s spawn-env
allowlist enforces.

**Why it is gated rather than unconditional.** `env_vars` is verified working on
`codex-cli 0.157.1`, but the minimum version that accepts the key is
unconfirmed, and `RawMcpServerConfig` carries `deny_unknown_fields` — on a Codex
old enough not to know it, the run would either die at config parse (every spawn
lost) or silently drop the credential (the agent loses every duduclaw tool with
no error). So the gateway runs `codex --version` once per binary path per
process, parses `codex-cli X.Y.Z`, and treats anything below `0.157.0`,
unparseable, or unprobeable (spawn failure, non-zero exit, 5s timeout) as "no
support" — falling back to the old `argv` shape with one `warn!`. A failed probe
can never fail a spawn.

**If you are on the fallback path** (older Codex, shared or multi-tenant host),
the exposure is real: command-line arguments are readable by any process on the
same host (`ps -ww`, `/proc/<pid>/cmdline`). Upgrade the Codex CLI to 0.157.1 or
later and the credentials leave `argv` with no configuration change.

**Mitigations on both paths.** The set of env keys that may reach `argv` is
locked by tests to the known `DUDUCLAW_*` block, so a new secret cannot join it
silently; every key is validated to be a bare TOML key before interpolation
(including the names inside the `env_vars` array); and every value is
TOML-quoted.

### Codex: the employee's other MCP servers (2026-10-19)

Codex never reads `.mcp.json`, so a Codex spawn registers the employee's
other servers the same way it registers duduclaw: as `-c mcp_servers.<name>.…`
overrides (and in `<agent_dir>/.codex/config.toml`). Before that the spawn
registered duduclaw alone, and a server such as a broker connection listed in
`.mcp.json` was missing on Codex. The rules:

- A `.mcp.json` that cannot be read or parsed stops the spawn (a behavior
  change: Codex did not read the file before). No file, or a file listing
  only DuDuClaw's own server (team role scaffolds write exactly that), keeps
  the old spawn: no gate, no write. Utility calls on the employee's runtime
  (summaries, classifiers, judges) never get the employee's servers, as on
  the Claude utility path.
- When third-party servers are listed, the `.mcp.json` spawn gate runs first,
  as for a Claude spawn; a refusal stops the spawn and writes nothing. When
  it passes it may rewrite the file's DuDuClaw entries (other entries are
  kept), the same repair every Claude spawn and gateway boot does.
- Each third-party stdio server is registered as `duduclaw mcp-proxy
  --server <name> -- <command> …`, always (not only with `action_rules` or
  in the read-only lane, as for Claude), because Codex runs with
  `approval_policy=never` and these servers are registered with
  `default_tools_approval_mode = "auto"`: the proxy's third-party tool gate is
  what decides. Remote bridges are registered as they are (they gate
  themselves); direct `url` / `type` entries are removed and audited
  `third_party_remote_entry_dropped`. An entry the rewrite cannot wrap, or
  whose name is not a bare TOML key, is not registered.
- A `.mcp.json` entry with a DuDuClaw name (`duduclaw`, `duduclaw-pro`,
  `duduclaw-<instance>`) never replaces the server the gateway builds.
- The upstream server's own environment (its credentials) goes to a per-spawn
  owner-only file in `<home>/tmp/mcp-proxy-env/` (a 0700 directory) named by
  `DUDUCLAW_MCP_PROXY_ENV_FILE`, not to `argv` and not to
  `.codex/config.toml`; the gateway removes files older than 15 minutes when
  it starts. A manual `codex` run from `.codex/config.toml` alone therefore
  starts these servers without their declared env. `.codex/config.toml` is
  written owner-only from creation (temp file + rename), and an existing one
  is reset to owner-only. `DUDUCLAW_MCP_API_KEY` follows the table above (`env_vars`
  on 0.157+).
- Codex clears an MCP child's environment, so the explore lane is passed to
  each third-party server as `env.DUDUCLAW_LANE`.

### Grok, Antigravity and the tool loop: the employee's other MCP servers (2026-10-20)

The other runtimes that register MCP servers themselves had the same gap, and
on Grok it was worse than a missing server. All of them now take the
employee's `.mcp.json` servers through the same gated rewrite as Codex: stdio
servers behind `duduclaw mcp-proxy`, remote bridges as they are, direct
`url` / `type` entries removed and audited, DuDuClaw-named entries never
replacing the gateway's own server, and a name made of anything other than
letters, digits, `_` and `-` (at most 64) not registered.

**Grok.** Grok 1.0.62 loads the `.mcp.json` of its working directory by
itself (`grok inspect` lists it as a source and its bundled docs say so). For
an employee that is `<agent_dir>/.mcp.json`, so its other servers were
already being started by Grok without the proxy, which means without
`action_rules`, approvals or the read-only lane, and `url` entries were
connected directly. Grok ranks `.grok/config.toml` above `.mcp.json`, so the
runtime now writes every name from `.mcp.json` into `.grok/config.toml`:
gated servers as spawn-scoped proxies (below), everything else as a disabled
entry that hides the `.mcp.json` one (DuDuClaw-named extras such as
`duduclaw-pro` included). An entry the runtime wrote for a server
the employee no longer lists is removed; entries an operator added by hand
are left alone.

**Antigravity.** agy reads `.agents/mcp_config.json` only (on 1.4.2 a
`.mcp.json` server is never started), so these servers were simply missing.
Each gated server is now registered there with `command` and `args` only,
and gets an `mcp(<server>/*)` rule plus a read rule on its schema directory
in `~/.gemini/antigravity-cli/settings.json`. Without those rules agy's
print mode refuses the calls. The proxy decides, as with Codex's
`default_tools_approval_mode = "auto"`.

**Spawn-scoped proxies (Grok and Antigravity).** Both files are persistent,
every spawn in the directory shares them, and neither CLI takes a per-run MCP
override, so the file holds nothing per spawn and no upstream credential.
Each server is registered as `duduclaw mcp-proxy --spawn-scoped --server
<name> -- …`. The spawn writes the upstream servers' env into a new
owner-only directory under `<home>/tmp/mcp-proxy-env/` and names it in
`DUDUCLAW_MCP_PROXY_ENV_DIR`: agy hands its environment to the MCP children
it starts, and Grok expands `${DUDUCLAW_MCP_PROXY_ENV_DIR:-}` in the entry's
`env`. The directory is deleted when the CLI exits; the gateway removes
leftovers older than 15 minutes when it starts. A spawn-scoped proxy that
finds no directory, or no file for its server, starts nothing. That is why a
utility call on these runtimes gets none of the employee's servers, and why
an operator running `grok` or `agy` by hand in the directory gets none
either. Two spawns of the same employee write the same file content and each
get their own directory. The Grok entry also holds the proxy's own identity
(agent id, home, and the MCP credential the `duduclaw` entry of the same file
already holds), never the agent token. The upstream server starts from the
agent-CLI allowlisted environment plus its declared env, so the CLI's own
vendor key (`XAI_API_KEY`, `GEMINI_API_KEY`) does not reach it.

Known limits: the agy permission rules are added to the OS user's settings and
never removed (a later server of the same name in a workspace an operator
uses by hand is auto-approved there too); an entry an operator wrote by hand
in `.grok/config.toml` under a `.mcp.json` name is kept as written and
logged, so Grok starts it without the proxy; in a borrowed work root two
employees with a server of the same name share one entry, and the spawn that
runs uses its own env file for it.

**openai-compat, Direct API and local inference.** These three share one
tool-loop builder (`claude_runner::build_mcp_tool_registry`), which mounted
DuDuClaw's server and `agent.toml [[mcp.external]]` only. It now also mounts
the `.mcp.json` servers, each started as the gated rewrite produced it and
from the agent-CLI allowlisted environment instead of the gateway's, so no
other vendor key in the gateway's environment reaches a third-party server.
Nothing is written to disk on this path. In the read-only lane they are
mounted with `DUDUCLAW_LANE=explore`, so only read-only tools pass the
proxy, as on a Claude spawn in the lane; `agent.toml` externals stay
unmounted in the lane because nothing gates them. A server declared in both
files (same name, or the same command and arguments) is mounted once, from
`agent.toml`, which carries the tool allowlist and `secret://` references.

**Failing closed.** When third-party servers are listed, a `.mcp.json` that
cannot be read or parsed, or a spawn-gate refusal, stops the run with the
spawn-gate error, which also stops failover. The openai-compat runtime
refuses the reply; the Direct-API and local-inference loops answer without
tools. On Grok a config that cannot be written also refuses the spawn (a
missing shadow would let Grok start a server ungated), and a malformed
`.mcp.json` now refuses Grok utility calls too, because Grok would read that
file itself. On Antigravity a failed registration refuses the spawn when
third-party servers are listed. With no third-party server listed every one
of these spawns is the old one. Grok and Antigravity refuse the read-only
lane before any of this runs.

**Not verified.** A real `grok -p` turn (it needs a signed-in Grok; the
config was checked with `grok mcp doctor` and `grok inspect` under a
temporary `HOME`); a real model on any of these runtimes calling a
third-party tool; an approval-gated third-party tool inside the CLIs' time
limits.

### The working-directory override reaches every CLI backend (2026-09-28)

A caller can ask for one spawn to run somewhere other than the agent's own
directory. The only caller today is the team composer, which puts a role member
in the employee's workspace so the files the member writes survive the
throwaway scaffold being garbage-collected the moment the member finishes.

Until 2026-09-28 only the Codex backend honoured that request. Gemini,
Antigravity and Grok spawned in the agent directory regardless, so a role member
on any of those three did its work in a directory that was deleted seconds
later. All four now resolve the working root through one shared helper, which
also checks the requested path is a real directory and otherwise warns and falls
back to the agent directory rather than spawning into nowhere. The native OS
sandbox is scoped to the same root, so an overridden root is the one that gets
write access.

The override moves the **working directory only**. Agent identity — the MCP
server registration, the agent id the tools authenticate with, the agent's own
configuration — stays with the agent directory.

Two backend-specific consequences, stated rather than left to be discovered:

- **Antigravity** pre-trusts the working root (`agy` shows an interactive
  "trust this workspace?" prompt that would hang a headless run) and passes it
  as `--add-dir`.
- **Grok** resolves both its MCP registration (`.grok/config.toml`) and its
  sandbox profile names (`.grok/sandbox.toml`) from the working directory, not
  from the agent directory. An overridden root therefore gets its own copy of
  both, or the member would spawn with no tools and an unresolvable sandbox
  profile. Known limitation: those two files are keyed by directory, so two Grok
  role members sharing one workspace overwrite each other's declared env block.
  The command/args halves are identical between members and the per-process
  identity is what the MCP child actually authenticates with, so the blast
  radius is the declared block only.

### Antigravity authentication and MCP tools (2026-10-01)

`agy` has no `login` subcommand, so the dashboard offers no one-click sign-in for it. Two ways to authenticate:

- **Google sign-in**: run `agy` in a terminal on the host running DuDuClaw and follow the prompts. The credentials live in the OS keyring, so this does not work in a container or on a remote host with no keyring or browser.
- **API-key mode**: set `[antigravity] auth = "api_key"` in `config.toml` and supply a Gemini API key, either as a `gemini` provider account or in the `GEMINI_API_KEY` environment variable. The gateway then writes `modelProvider` into agy's settings itself. `auth = "login"` switches back to Google sign-in and removes that `modelProvider` entry. If `auth` has never been set, the gateway leaves `modelProvider` alone and passes no Gemini key, so agy keeps whatever route it was already on.

There is no `ANTIGRAVITY_API_KEY` variable. The platform's MCP tools are registered per agent workspace in `<agent workspace>/.agents/mcp_config.json`.

Three things to know before switching to API-key mode:

- The setting applies to the whole OS user. agy keeps `modelProvider` in its user-level settings file, so your own interactive `agy` under the same account moves to the API-key route too.
- Two gateways running under one OS user with different `auth` values overwrite each other's value.
- To go back to Google sign-in after using `api_key`, set `auth = "login"` explicitly. Removing the `auth` line is not enough: with no setting the gateway does not touch `modelProvider`, so the `"gemini"` value written earlier stays in agy's settings, and the gateway only logs a reminder. In `login` mode the gateway does not pass `GEMINI_API_KEY` / `GOOGLE_API_KEY` to agy or the commands it runs; in `api_key` mode the agent's shell can read the key (agy has to receive it through the environment).

### Antigravity tool permissions (v1.69.1)

In print mode, `agy` 1.2.16 refuses every confirmation it cannot ask a human about, and calling an MCP tool needs one. Until this fix an Antigravity employee at the default capability level (which runs with `--sandbox`) could not use any platform tool; only the full-access level, which passes `--dangerously-skip-permissions`, could. The defect dates from v1.67.0 and was found on 2026-10-04 with a real Gemini API key.

Each time it runs an Antigravity turn, the gateway already adds the working root to `trustedWorkspaces` in agy's user-level `~/.gemini/antigravity-cli/settings.json`. The same locked write now also adds two rules to `permissions.allow`:

- `mcp(duduclaw/*)` allows every tool of the MCP server registered under the name `duduclaw`.
- `read_file(<HOME>/.gemini/antigravity-cli/mcp/duduclaw)` allows reading that server's tool description files. agy loads MCP tools lazily, and the model reads the description file before each call; in print mode that read is refused too. When `HOME` resolves to a different path (for example `/var` and `/private/var` on macOS), both spellings are written.

The gateway adds no rule for shell commands, file writes or URLs, and the command-line flags are unchanged, so the default level still runs with `--sandbox`. In a test with agy 1.2.16 and a real Gemini API key, an employee at the default level could call DuDuClaw tools, while a shell command and a write outside the workspace in the same turn were still refused, and so were a path-traversal read and a read through a symbolic link that points outside the directory. Combining `--sandbox` with an auto-approve-everything flag was not adopted, because it let the file-write tool write outside the workspace.

Operator rules are kept: existing `allow`, `deny` and `ask` entries stay as they are. If `permissions` is not an object, or `allow` is not an array, the gateway leaves it untouched and logs a warning; the workspace trust and `modelProvider` are still written, and a failed run says in its error that the rules could not be added. If the home path contains `(`, `)`, `,`, `*` or a line break, or is not valid UTF-8, the `read_file` rule is skipped with a warning and only `mcp(duduclaw/*)` is written.

**Read-only level.** Because the rules live in a user-level file, they cannot depend on an employee's capability level. A read-only Antigravity employee therefore gets the same two rules and can call platform tools too; the MCP server's own `allowed_tools`, `denied_tools` and approval lists decide what it may do. This matches the Claude runtime. It differs from Codex, where every MCP tool call is rejected at the read-only level (see the table above).

**Side effects to know about.**

- The rules sit in the settings file that every `agy` of the OS user shares. When you use `agy` interactively in a terminal, calls to tools of an MCP server named `duduclaw` and reads of that schema directory are also approved without asking.
- The rules are only ever added. After you remove an employee, uninstall DuDuClaw or stop using Antigravity, they stay in the file, as `trustedWorkspaces` entries already do. To remove them, edit `~/.gemini/antigravity-cli/settings.json` and delete the two entries from `permissions.allow`.
- Two gateways under one OS user write the same rules, so they do not overwrite each other with different content.

**Errors.** When agy refuses a tool, the gateway's error now names the refused tool and includes agy's own error text, with the key redacted before the text is truncated. If agy reports success but the reply is empty and a tool was refused, the run is an error; before, the raw result JSON became the employee's answer. A normal reply with a refused tool is kept and logged as a warning.

**Not yet verified.** The end-to-end test with a real key has not been rerun since the last change to this code. It has not been run on Linux, inside a Docker container or on Windows. Two Antigravity problems are known and not addressed: agy can report a failure after it retried a 503 successfully, and the gateway then treats a complete reply as failed; and after an Antigravity failure, cross-vendor failover may switch to Claude.

### Antigravity stream parsing degrades instead of failing (2026-09-28)

The `agy --output-format stream-json` decoder used to be strict in six
independent places: one unparseable line, a missing `result` event, a missing
`response` field, or a `usage` block short one integer turned the whole run into
an error — and an `agy` that had *already answered* was reported as a failed
spawn, losing the role member with it.

Shape mismatches now degrade; facts do not. An unparseable line is skipped. A
missing result falls back to the last non-empty stream line as the answer. A
missing or partial usage block yields unknown tokens rather than an invented
zero. Each degrade logs one `warn!` naming what was missing. The one case that
still fails hard is an explicit non-`SUCCESS` status — that is `agy` telling us
the run failed, not a shape we failed to recognise.

### Which model the fallback runtime receives (2026-09)

Failing over to another runtime never forwards the original model id blindly
(a Codex agent's `gpt-5.4` must not be handed to the Claude CLI). The
FailoverManager resolves the fallback model in four ordered branches and
refuses to spawn when none applies:

1. the first `agent.toml [model] fallbacks` entry whose family confidently
   belongs to the fallback runtime (qualified ids such as `openai/gpt-5.4` are
   unqualified with the same `split_model_id` dialect the Direct-API chain uses);
2. the original model, when it already belongs to the fallback runtime;
3. the runtime's catalog default (`fallback_models[0]`, the same list the
   dashboard offers when live discovery fails);
4. otherwise the attempt is recorded as a failure with
   `no model configured for fallback runtime <name>` — no spawn.

Every substitution logs `agent / from_runtime / to_runtime / from_model /
to_model` at `warn` level.

**Judge and evaluator calls opt out of cross-family failover entirely**
(corrected 2026-09-28). When an operator names a judge runtime or model
(`[dispatch] judge_provider` / `judge_model`), the point of the call is *which
family answers* — so a failed judge spawn is never rescued by substituting
another family's model; the caller degrades explicitly and audibly instead. The
opt-out used to trigger only when the judge hint *moved* the provider away from
the resolved default, which quietly re-enabled substitution whenever the judge
family and the default utility family happened to be the same (both `codex`, for
instance — exactly where a decorrelated-judge setup ends up once Codex is also
made the default utility runtime). Naming a family is now a request for that
family whether or not it is also the default. Un-hinted utility calls keep
failover unchanged.

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

Owner decision 2026-10-16: the subscription-login features stay as they are (setup-token capture, OAuth accounts, host-login adoption, rotation, one-click CLI login). What changed is the disclosure.

**What the notice says.** Anthropic's [legal and compliance page](https://code.claude.com/docs/en/legal-and-compliance) says third-party developers may not offer Claude.ai login in their own applications or route requests through Free, Pro or Max plan credentials on behalf of their users, and may not collect, store or intermediate Claude.ai credentials or session tokens. Signing in to the unmodified Claude Code binary with your own subscription is allowed, and Pro/Max usage limits assume ordinary usage. The Consumer Terms add that you may not make your account available to anyone else. Anthropic may enforce this without prior notice; account suspensions have been reported by press and users (reports, not an Anthropic statement). The notice names the DuDuClaw uses that conflict with the policy: one subscription serving other people (channel customers), rotating several subscriptions past the limits, and DuDuClaw storing the token. The notice never says the token is "blocked": `claude -p` with an OAuth token still works technically (v1.66.0).

**Per vendor.** The full notice above is what Claude logins and Claude accounts show. Codex, Antigravity and Grok logins (and Add account's OAuth path with a non-Anthropic provider) show that vendor's own text and terms link instead, with the same checkbox: Google's Antigravity clause quoted above; for OpenAI, the Sign in with ChatGPT program summarised above (open-source and approved commercial partners, tokens kept local) with a link to its terms; for xAI, that its position is unclear and its terms apply. An unknown provider gets a vendor-neutral notice with no link. Every sentence points at a vendor page; the earlier "Google has blocked server-side since 2026-03" sentence had no primary source and was removed. The vendor table lives in `duduclaw_core::subscription_risk::vendor_policy` (gateway, CLI) and `web/src/lib/subscription-risk.ts` (dashboard).

**Where it shows.** The full disclosure with a checkbox opens before any credential step in the CLI login modal, the guided QR-code setup wizard, and the OAuth Token path of Add account. A one-line note with a link to the policy sits on the Welcome page's Claude subscription card (and on its "already signed in" card, the host-login adoption case), the runtime setup card's sign-in row, the Accounts page's subscription hero card, the one-click CLI picker, the CLI credentials list, the agent editor's API mode row when it is set to the subscription CLI, and the account pool picker when a subscription account is in the pool.

**Server-side gate.** `auth.cli_login.start` (every runtime that has a scriptable login), `accounts.setup_token_start`, `accounts.add` with `type = "oauth"`, and `config.raw.set` when the submitted `config.toml` adds an `[[accounts]]` entry with `type = "oauth"` that the stored file does not have (a new id, or an existing id switched to `oauth`) refuse unless the request carries `accept_subscription_risk: true`. This is a breaking change for custom WebSocket clients, shipping in v1.74.0 (see [Deprecations](../guides/deprecations.md#changed-without-a-deprecation-window-v1740)). The refusal is a structured error with code `subscription_risk_not_accepted` plus the `provider` and its `policy_url`, which the dashboard shows in the user's language; a refused raw edit writes nothing, and the raw editor then shows the vendor's notice and saves again once it is ticked. Each accepted call writes one `subscription_risk_accepted` row to `security_audit.jsonl` with the timestamp, method, provider, account id (when known), the acting dashboard user, the vendor's `policy_url` and `outcome = "acknowledged"`; its `note` says the row records the acknowledgment, not a successful login or save. A raw edit that adds several oauth accounts writes one row each. Internal reuse of `accounts.add` by `auth.cli_login.finalize` and `accounts.setup_token_submit` is not asked again; those finish a session that an acknowledged `start` opened. API-key accounts are not gated.

**Terminal and logs.** `duduclaw onboard` prints the risk line next to "偵測到 Claude … 登入". When the account rotator adopts the host's own Claude login (`[account_loading] inherit_host_credentials`, still on by default), the gateway logs one WARN per process with the same line. `duduclaw doctor` and the dashboard doctor add a "訂閱帳號政策風險" row whenever a Claude OAuth account (provider `anthropic` or unset) is configured in `config.toml`; other vendors' oauth accounts, such as a Copilot seat, are not counted because the row quotes Anthropic's policy. `duduclaw auth device --provider copilot`, the operator command that stores a GitHub Copilot seat, prints the notice with GitHub's terms link and stops unless `--accept-subscription-risk` is given; with the flag it writes the same audit row (actor `operator-cli`).

---

## The Takeaway

The AI market is multi-provider. Building on a single CLI is like writing software for a single operating system — it works until it doesn't. The `AgentRuntime` trait abstracts away the differences, letting DuDuClaw treat Claude, Codex, Antigravity, and any OpenAI-compatible endpoint as interchangeable backends. Your agents get the best available brain, every time.
