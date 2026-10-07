# Multi-Runtime Agent Execution

> One platform, thirteen runtime ids: twelve CLI backends (Claude, Codex, Gemini, Antigravity, Grok, Qwen Code, Kimi Code, GitHub Copilot CLI, Kiro, Cursor, Mistral Vibe, OpenCode) plus `openai_compat`, which talks HTTP to any OpenAI-compatible endpoint.

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
| Gemini CLI (deprecated in v1.67.0, removed in v1.72.0) | `gemini` | npm `@google/gemini-cli` | `-p --output-format stream-json <prompt>` | jsonl | `gemini auth login` (localhost callback) | `~/.gemini/oauth_creds.json` |
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

Five CLI backends (`claude`, `codex`, `gemini`, `antigravity`, `grok`;
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
(`runtime/openai_compat.rs`). That makes six hand-written modules in total.

### The Original Four Backends

These are the four backends DuDuClaw shipped first. Antigravity and Grok, the
other two bespoke CLI modules, are covered in their own sections further down.

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

> **Deprecated in v1.67.0, removed in v1.72.0.** Google stopped serving individual (free, AI Pro, AI Ultra) accounts through Gemini CLI on 2026-06-18; use the Antigravity runtime instead. It keeps working until removal. The Gemini API provider is not affected. Migration steps: [Deprecations](../guides/deprecations.md#gemini-cli-runtime).

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
| `gemini` | — | **no flag exists** — logged at debug and ignored | — |
| `openai_compat` | — | `reasoning_effort` in the request body | `low` `medium` `high` |

### The clamp table

Because the accepted sets differ, your setting is clamped **down** to what the target runtime will take. It is never dropped silently, and never sent as a value the CLI would reject:

| You set | claude | codex | antigravity | grok | openai_compat | gemini |
|---|---|---|---|---|---|---|
| `low` | `low` | `low` | `low` | `low` | `low` | — |
| `medium` | `medium` | `medium` | `medium` | `medium` | `medium` | — |
| `high` | `high` | `high` | `high` | `high` | `high` | — |
| `xhigh` | `xhigh` | `xhigh` | **`high`** | **`high`** | **`high`** | — |
| `max` | `max` | **`xhigh`** | **`high`** | **`high`** | **`high`** | — |

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

Every one-click "sign in with your subscription" flow — the CLI login modal, the guided QR-code setup wizard, and the OOBE runtime-setup card that opens one of the two — shows a risk notice before it starts: Anthropic and Google have blocked third-party products from using consumer-subscription tokens on the server side since March 2026 and have suspended accounts over it; OpenAI's policy on this is unspecified. An explicit "I understand the risk and accept it" checkbox must be ticked before the flow's own login step (CLI subprocess, browser callback, or device-code polling) begins. The API-key path is unaffected by this gate and remains the recommended default.

---

## The Takeaway

The AI market is multi-provider. Building on a single CLI is like writing software for a single operating system — it works until it doesn't. The `AgentRuntime` trait abstracts away the differences, letting DuDuClaw treat Claude, Codex, Gemini, and any OpenAI-compatible endpoint as interchangeable backends. Your agents get the best available brain, every time.
