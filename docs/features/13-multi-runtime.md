# Multi-Runtime Agent Execution

**Codex external configuration gate (since v1.80.0):** Employees, Explore, utility, Task Sandbox and Discovery may run when external layers are absent or define no MCP servers; detected MCP or unreadable/unparseable existing layers refuse launch with the source named. A refused explicit Codex judge fails acceptance without retrying the default judge. This checks a local/cache snapshot before launch, not complete isolation from subsequent cloud refresh. See [checked sources and limits](../architecture/extension-gates.md).

> One platform, eleven runtime ids: ten CLI backends (Claude, Codex, Grok, Qwen Code, Kimi Code, GitHub Copilot CLI, Kiro, Cursor, Mistral Vibe, OpenCode) plus `openai_compat`, which talks HTTP to any OpenAI-compatible endpoint.

Managed Claude spawns use only a temporary, strict MCP configuration, even when it contains only built-in tools. Managed Grok spawns refuse unmanaged project MCP entries and global MCP definitions under the actual `HOME` or `GROK_HOME`; unreadable configurations also refuse the spawn. The effective Grok configuration root must be absolute; correct a relative `HOME` or `GROK_HOME` before retrying. Existing files are preserved: move the server definitions into the employee MCP configuration before retrying. Standalone CLI runs and native tools remain outside this boundary.

## Failure provenance and explicit fallback

An absent or invalid `[runtime] fallback` does not silently try Claude. An explicitly configured runtime fallback still uses the existing health, policy and compatible-model checks. Account rotation, model fallback and configured local inference remain available. A non-Claude reply does not add an Anthropic Direct API hop after failure.

Diagnostics distinguish the primary backend, explicit fallback and unexecuted steps. GPT/Codex or API authentication errors do not instruct Claude login. Only an actual Claude CLI authentication failure opens its outage, and only an actual Claude CLI success clears it. An OpenAI-compatible endpoint may belong to another vendor. Administrators who relied on implicit Claude fallback must now configure `fallback` explicitly.

## When the fallback engine runs (v1.84.0)

One rule decides whether an explicitly configured `[runtime] fallback` runs after the primary engine fails (`failover/fallback_rule.rs`). Refusals are checked first, then a typed failure kind, then error prefixes that DuDuClaw itself writes; any other text keeps the v1.83.0 outcome. Typed failure kinds (`failover/failure_kind.rs`) currently come only from the OpenAI-compatible runtime; the CLI engines are judged by the prefixes and the v1.83.0 text rule.

| The fallback engine runs after | The fallback engine does not run after |
|---|---|
| A login or credential failure identified by a typed kind or a DuDuClaw-written prefix (an API engine's HTTP 401/403, the Grok authentication-failure message, "CLI is not logged in") | Work limits: step limit (`max_turns`), total time limit (`timeout_total`), no-progress limit (`timeout_no_progress`), now the same for Grok, generic CLIs and OpenAI-compatible engines as for Claude and Codex |
| A missing model reported with the typed provider code `model_not_found` | Budget, product-policy and autonomy-pause refusals |
| Rate limits and server errors (429, 5xx) | Content-policy refusals and billing hard limits |
| Start-up or execution errors, empty replies | The employee's own tool configuration failing the spawn check (`.mcp.json`), and a Codex project trust override |
| Network errors, including connection timeouts (an OpenAI-compatible read timeout during the reply counts as the total time limit and does not switch) | A request the engine rejected (HTTP 400/404/422) |
| The engine is not installed or is cooling down | Error text that is not typed and only mentions `401 `, `403 `, `invalid_api_key`, `400 `, `404 `, `422 `, a content-policy violation or `billing_hard_limit` (the v1.83.0 list; the message says it looked like a login error that could not be confirmed and asks to check the primary engine's login) |
| The engine's own setup is refused, reason code `engine_config_refused`: Codex external configuration, a Codex working folder that is or contains the data directory, Grok global MCP configuration, and context too large for a single-argument engine (that one only to Claude, Codex or an OpenAI-compatible engine) | |
| Any other error text not listed on the right, for example a refused connection | |

Only provider failures count against the primary engine's health; refusals and engine-setup refusals do not put it on cooldown. The engine-setup case is controlled by the build constant `ENGINE_LOCAL_CONFIG_REFUSAL_FALLS_BACK` (currently `true`; `false` restores the v1.83.0 behaviour of stopping there).

For signed-in members and operators, a failure message lists the engines tried in order with what happened on each, and when a configured fallback was not used it says why. These sentences are currently zh-TW only and are never added for website visitors, who still receive only the generic message and reference number. The audit record carries `fallback_skipped` with the runtime and reason.

**Conversation continuity.** The fallback engine receives the same conversation history as the primary. Before v1.84.0, Claude as a fallback engine and the Direct-API fallback of Claude employees received only the newest message.

**Model on fallback.** A fallback engine that declares its own model families keeps the current model only when it confidently belongs to that engine; otherwise it uses the engine's catalog default, including for unknown model names. Engines without declared families (`openai_compat`, multi-vendor shells) still receive the model as configured. The bare names `opus`, `sonnet`, `haiku` and `fable` are recognised as Claude models (exact, case-insensitive match; `vendor/opus` or `opus-foo` are not). An employee saved with such a name on another engine that declares model prefixes runs on that engine's default model and gets a notice on the employee page and in the system check: 「這位 AI 員工設定的模型（…）不屬於它的執行引擎（…），已改用該引擎的預設模型。請到員工設定重新選擇模型。」, where Codex is named 「ChatGPT」. Full `claude-*` names on another engine are still refused until an administrator saves the settings. Product policy judges a model by the pricing table and an explicit `provider/` prefix; only a catalog default that DuDuClaw substituted is also checked against its engine's vendor (Claude → Anthropic, Codex → OpenAI, Grok → xAI).

**Engines installed later.** An install from the dashboard refreshes the engine list at once. `runtime.detect` rebuilds it only when the detected engines differ from the ones the list was built from, at most once a minute per data directory; when an employee's primary or fallback engine is missing, the list is probed again at most once a minute. Calls already running keep the list they started with.

**Codex instructions and call directory.** Each Codex call gets a private directory under `<home>/run/codex/` (`call-<32 hex>/`, or `utility-<32 hex>/` for background work, mode 0700) holding its own `CODEX_HOME` with an owner-only `AGENTS.md` carrying the full employee instructions, and the output schema file `output-schema.json`. The conversation goes over standard input, and `-c project_doc_max_bytes=0` keeps working-directory and ancestor `AGENTS.md` files out. The directory is removed when the call ends; leftovers older than 15 minutes are removed at start-up and at most once an hour. The Codex permission profile marks `<home>/run`, `<home>/run/codex` and the host Codex credential store read-only for Codex's tools, and `-c shell_environment_policy.exclude=["CODEX_HOME"]` keeps `CODEX_HOME` out of the tool environment. Before v1.84.0, Codex cut anything beyond its 32 KiB project-document limit, which could drop conversation summaries and pinned instructions. DuDuClaw no longer writes `AGENTS.md` into the employee directory; an existing file is left alone. A working folder that is the data directory, one of its parents, or any folder containing it is refused (「Codex 不會啟動：這位員工的工作資料夾包含了 DuDuClaw 的系統資料夾。…」); on Windows this also catches drive roots such as `C:\` and differently spelled paths to the same folder, and the same refusal is shown if the folder changes between the check and the start. On Windows, Codex is also not started when the working folder is not a full path (for example `C:` or `\work`: 「為保護系統資料夾，這位員工的工作資料夾必須是完整路徑（例如 C:\Work\project），已停止啟動 Codex。」) or is a network folder or a drive Windows does not report as local, such as a UNC path or a mapped network drive (「網路資料夾或無法確認為本機的磁碟不能用作 Codex 的工作資料夾，已停止啟動 Codex。」). All of these are engine-setup refusals (`engine_config_refused`), so a configured fallback engine answers while `ENGINE_LOCAL_CONFIG_REFUSAL_FALLS_BACK` is `true`. To fix it, give the employee a working folder on a local disk, written as a full path, that does not contain the DuDuClaw data directory. An external Codex configuration refuses the launch (reason code `engine_config_refused`; the fallback engine may run) when any table in it, profiles included, defines a non-empty `mcp_servers` (an empty table is allowed), or when a file that exists cannot be inspected, read or parsed (a missing file is fine); the same applies to the profile files, the managed and cloud configuration caches and macOS forced preferences. The message names only the layer (主機層, 專案層 or 員工資料夾) and the file name: 「Codex 外部設定（{layer}）{file}：{reason}；拒絕啟動。請由管理者移除原生 MCP 設定，改用 DuDuClaw 的代理設定。」; the full path goes to the log. A project trust override passed to Codex (`-c projects…`, `--config=projects…`, or `[projects]` in the owned `.codex/config.toml`) is refused through the employee spawn check: it does not fall back, and its reason code is `unknown`. For project-layer `.codex/config.toml` files the external configuration check looks only from the working directory up to the nearest git root, which is where Codex itself looks; the system, profile, cloud-cache and MDM sources are checked as before. See [extension gates](../architecture/extension-gates.md).

**Grok and generic CLIs: prompt size.** These engines receive instructions and history in one command argument, limited to 120,000 bytes. The required context is the summaries, pinned instructions, historical note, knowledge rules, per-turn instruction, the newest three messages and the new message; when that alone is larger, the call is refused with 「這段對話必要的內容太大（…），{engine} 無法一次送出。請開始新的對話，或縮短固定指示後再試。」 (reason code `engine_config_refused`; it can fall back to Claude, Codex or an OpenAI-compatible engine, never to another single-argument engine). Otherwise the payload is reduced in this order: the oldest turns down to the newest three, then the lower-priority sections (key facts, past mistakes, playbook, recent actions, user profile, working state, open decisions, preset), then the remaining recent turns oldest first, and only then the base prompt from its end. A payload under the limit is unchanged.

## Model and runtime consistency

Agent creation, edited templates, model saves and the administrator `/model` command share the same model-family alignment rule. A confident mismatch is corrected when saving, subject to the existing administrator and product-policy checks. Explicit API-compatible backends, fallback settings and unknown custom models are preserved. Saving the same model can repair an older runtime mismatch; the response distinguishes a live repair from a failed registry reload.

An older unsaved profile with a known model/provider mismatch is refused before model execution. Ask an administrator to save its settings and retry. Reading or routing never silently rewrites the file, and a configuration mismatch is not a Claude login error. Model-name substrings do not determine a runtime.

**Editing `agent.toml` on disk (v1.84.0).** For a chat turn (channel and WebChat), the engine and model now come from one strict read of the employee's settings file (the preset-resolved profile when there is one, otherwise `agent.toml`), so changing both `[runtime] provider` and `[model] preferred` on disk takes effect on the next message without a restart or a registry reload. Since v1.86 background work (dispatch, cron, reminders, heartbeats) uses the same strict read and is refused instead of running with Claude defaults; see [Security Defense](05-security-defense.md#a-misconfigured-employee-is-refused-at-every-entry-v186). The turn is refused before any model call, before any session row is written, and with only a live notice on a brand-new WebChat conversation, in two cases:

- `model_provider_mismatch`: the model does not belong to the engine (after a bare Claude alias has been replaced by the engine's default). Notice: 「模型與執行引擎設定不相容，請管理者選擇這個引擎支援的模型後再試。」 (also in en, ja-JP and zh-CN).
- `agent_config_unreadable`: the settings file is missing or unreadable, cannot be parsed or has wrongly typed `[runtime]`/`[model]` fields, names an unknown engine, or gives no model for an engine that cannot run without one (OpenAI-compatible). Kiro takes its model from `kiro-cli settings` and runs normally when none is configured. Notice: 「無法讀取這位員工的設定，本次未執行。請聯絡管理者檢查這位員工的設定。」 The file path and parser message go to the gateway log only.

Website visitors see only the generic visitor message. In v1.83.0 a mismatch showed an English sentence without a reason code, and an unreadable or unknown setting silently ran with Claude defaults. Changing engines from the dashboard is still the recommended way: a save validates the pair and reloads the employee at once.

**Model shown for a turn.** The WebChat conversation information shows the model that actually ran: the engine's default when a bare Claude alias was replaced, and the fallback engine's model with 「（備援）」 after a fallback. A model name reported by a CLI is shown only when it is 1 to 128 ASCII characters from letters, digits and `. _ : / @ + - [ ]`; otherwise the requested model is shown.

## Discord employee selection

Ordinary messages, text commands such as `/model`, and `/ask` use the same explicit employee order: a per-employee bot binding, the guild or DM selection, then the global selection. An empty scoped selection inherits the global setting. A removed or renamed explicit target is refused rather than silently replaced by the main employee.

With no explicit selection, ordinary conversation retains the existing channel/thread bindings, allowed-channel and default routing, followed by sender-aware `@` calls. Text commands resolve their context through the existing fallback without interpreting the command as an employee call. `/agent` and Switch Agent still require Manage Server to change a guild binding. Reply labels identify the employee actually selected by the reply pipeline, including bound and `@`-called employees. Document delivery uses that employee's existing directory checks; an unresolved reply does not scan the main employee's files.

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

Every backend — Claude, Codex, or any OpenAI-compatible endpoint — implements this same interface. The rest of the system doesn't know or care which backend is handling a particular request.

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
| Grok Build | `grok` | `x.ai/cli/install.sh` (manual) | `-p <prompt>` | text | `grok login --device-code` | `~/.grok/auth.json` |
| Qwen Code | `qwen` | npm `@qwen-code/qwen-code` | `-p <prompt> --yolo --output-format json` | json | none (API key only) | `~/.qwen/.env` |
| Kimi Code | `kimi` | npm `@moonshot-ai/kimi-code` | `-p <prompt> --output-format stream-json` | jsonl | `kimi login` (device code) | `~/.kimi-code/credentials/` |
| GitHub Copilot CLI | `copilot` | npm `@github/copilot` | `-p <prompt> -s --no-ask-user --allow-all-tools` | text | `copilot login --device-code` | `~/.copilot/config.json` |
| Kiro CLI | `kiro-cli` | `cli.kiro.dev/install` (manual) | `chat --no-interactive --trust-all-tools <prompt>` | text | `kiro-cli login --use-device-flow` | `~/.kiro/settings/cli.json` |
| Cursor CLI | `cursor-agent` | `cursor.com/install` | `-p <prompt> --force --output-format json` | json | `cursor-agent login` (browser) | `~/.cursor/cli-config.json` |
| Mistral Vibe | `vibe` | PyPI `mistral-vibe` | `-p <prompt> --yolo --trust --output json` | json | none (API key only) | `~/.vibe/.env` |
| OpenCode | `opencode` | `opencode.ai/install` | `run <prompt> --auto --format json` | jsonl | `opencode auth login` | `~/.local/share/opencode/auth.json` |
| OpenAI-compatible | *(HTTP)* | — | — | json | none (API key only) | — |

**Dashboard one-click login** supports Claude, Codex, Grok, Kimi, Copilot, Kiro and OpenCode. Codex uses device authorization: open the verification page shown by the CLI, enter its code, and wait for the CLI to finish. Pasting the OAuth callback URL `code` into the dashboard does not complete login. Cursor remains a runtime, but its browser callback cannot reliably finish from a remote dashboard, so it is not in the one-click picker. Qwen and Mistral Vibe use API keys. The Accounts page API Token card can now delete a saved account after confirming its account id; this does not revoke the key at its provider.

Gemini CLI is no longer a DuDuClaw runtime or Docker package. Existing `runtime = "gemini"` settings require another runtime and compatible model; the separate Gemini API provider remains supported. See [removal and migration](../guides/deprecations.md).

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

Three CLI backends (`claude`, `codex`, `grok`;
`BESPOKE_RUNTIME_IDS` in `runtime/mod.rs`) have bespoke runtime modules, because each has real per-vendor
wiring that is not shareable — account rotation (Claude), MCP config injection
in the CLI's own format, capability→sandbox-flag translation. Everything else is driven by **one** generic print-mode
runtime (`runtime/generic_cli.rs`) built straight from the catalog entry: spawn
the binary with the templated argv, deliver the prompt as an argument or on
stdin, parse text / JSON / JSONL back to the final assistant text, and map a
non-zero exit or an auth-required marker to a typed failure the failover chain
understands. The seven other CLIs (Qwen Code, Kimi Code, GitHub Copilot CLI,
Kiro, Cursor, Mistral Vibe, OpenCode) go through that generic runtime.
`openai_compat` is not a CLI: it has its own HTTP module
(`runtime/openai_compat.rs`). That makes four hand-written modules in total.

### Main Backends

Claude, Codex and OpenAI-compatible endpoints are described here. Grok is covered in its own section further down.

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
fallback = "codex"     # If primary is unavailable
```

If `provider` is absent, the agent runs on Claude. An unrecognised value logs a warning and also falls back to Claude.

### Execution time limit and background models

`agent.toml [runtime] timeout_secs` sets the per-run limit for Codex, Grok and other CLI runtimes, and the per-request limit for OpenAI-compatible APIs (plain, streaming and tool-loop requests). This also applies to Claude. Without an employee or global override, every runtime defaults to **1800 seconds** (since v1.84.0; runtimes other than Claude used **300 seconds** before). Values start at **30 seconds** and are bounded by `[runtime] max_timeout_secs` (default **3600**, configurable from **60 to 14400**); `[runtime] default_timeout_secs` optionally replaces the runtime default. A manually edited integer outside that range is clamped with a warning; a wrong-typed value is ignored with a warning and uses the runtime/global default. The dashboard rejects invalid values when saving. This is separate from the container task timeout.

Increasing the effective `timeout_secs` requires an administrator. An Owner may lower it; clearing the setting restores the global default or 1800 seconds and needs an administrator only when that increases the current effective limit (for any employee, Claude included, whose value is below the default that would apply, clearing is an increase).


On Windows, managed CLI children start suspended, join a kill-on-close Job Object, and resume only after assignment succeeds. Cleanup terminates the job even if the leader already exited; assignment failure kills the child and returns an error. Native Windows execution remains unverified in this round.

Grok no longer retries an empty pipe-mode response under a PTY: that path cannot guarantee whole-process-tree cleanup after timeout. An empty response is returned as an error.

Background decision checks, instruction pinning, fact extraction and conversation compression use the employee's runtime for Claude, Codex and OpenAI-compatible APIs. A non-blank `[model] utility` wins; otherwise Claude uses `claude-haiku-4-5`, while Codex and OpenAI-compatible APIs use `[model] preferred`. If both are blank, Codex omits `-m` and uses its CLI default; OpenAI-compatible APIs refuse the call. Grok and generic CLI employees follow the rule below (v1.84.0). The dashboard warns when an explicit utility model belongs to a different vendor family.

Claude utility calls clear the inherited environment, apply the shared spawn allowlist, then inject only the Anthropic API key from the same credential source as the normal no-rotation Claude spawn (OAuth keeps its usual HOME/keychain/config directory). They use the lightweight lane and do not enter account rotation. Normal chat retains its existing account rotation.

Utility calls, including classifiers and judges, use a dedicated execution profile. Claude uses the tool-free, single-turn lightweight lane; OpenAI-compatible APIs send one completion without tools and never enter the tool loop. Codex uses a restricted profile: a read-only sandbox, no DuDuClaw MCP overrides, a fresh owner-only empty temporary working directory, ignored user config, and no persisted session. The temporary directory is removed after the call, including errors and timeouts; the system prompt travels in the prompt without writing `AGENTS.md`. This also applies to explicit Codex judge and team-verifier hints. Grok and generic CLIs remain unsupported for utility execution. Since v1.84.0 their employees' background work uses the employee's `agent.toml [runtime] utility_provider` (`claude`, `codex` or `openai_compat`, Admin only), else the global `[runtime] utility_provider` only when that key is present in `config.toml`, else it is skipped with the error prefix `[utility_unavailable]` (before v1.84.0 it silently used the built-in Claude / `claude-haiku-4-5` default). Skipped work: pinned-instruction and key-fact extraction, decision capture, history compression (the history is kept), session summaries and titles, conversation distillation and knowledge filing, the situation classifier (which then treats the turn as an anomaly) and the proactive-message gate (which holds the message). The system check lists affected employees. The global provider must be Claude, Codex or OpenAI-compatible; an unsupported global provider or explicit hint is refused. An explicit Codex judge refusal fails acceptance without retrying the default judge; other providers retain existing degradation rules. Codex may retain built-in reading tools; see [Security defense](05-security-defense.md) for the residual risk.

The scheduled session summarizer and auto-titler apply the same utility provider selection to the session owner. The stored owner must not begin with `_` or `.` and must name a non-reserved, existing employee with a matching `agent.toml`, in a non-symlink direct child of the canonical `<home>/agents` directory. A nonempty invalid owner skips the utility call with an error; only an empty owner uses the global utility settings. Protected summary sections remain excluded from the model prompt and are preserved verbatim with the existing cap. Explicit utility-model mismatches keep the saved value and emit one warning per agent/model. Agent-less work uses `[runtime] utility_provider` / `utility_model`, now editable in Settings → Advanced → System.

### Per-Agent Configuration

Different agents can use different backends simultaneously:

```
Agent "dudu" (customer support)  → Claude (best reasoning)
Agent "coder" (code generation)  → Codex (optimized for code)
Agent "analyst" (data analysis)  → Codex
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

Probed against the installed binaries on 2026-09-24 (recorded in internal probe notes, not published) — not inferred from docs:

| Runtime | Version probed | How effort is expressed | Values the CLI accepts |
|---|---|---|---|
| `claude` | 2.1.258 | `--effort <v>` | `low` `medium` `high` `xhigh` `max` |
| `codex` | 0.156.1 | `-c model_reasoning_effort=<v>` (config override, no dedicated flag) | `low` `medium` `high` `xhigh` |
| `grok` | 1.0.41 | `--reasoning-effort <v>` (alias `--effort`) | *not enumerated by `--help`* |
| `openai_compat` | — | `reasoning_effort` in the request body | `low` `medium` `high` |

### The clamp table

Because the accepted sets differ, your setting is clamped **down** to what the target runtime will take. It is never dropped silently, and never sent as a value the CLI would reject:

| You set | claude | codex | grok | openai_compat |
|---|---|---|---|---|
| `low` | `low` | `low` | `low` | `low` |
| `medium` | `medium` | `medium` | `medium` | `medium` |
| `high` | `high` | `high` | `high` | `high` |
| `xhigh` | `xhigh` | `xhigh` | **`high`** | **`high`** |
| `max` | `max` | **`xhigh`** | **`high`** | **`high`** |

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

The one place effort is deliberately *not* agent-driven is Claude's lightweight extraction path (session compression, GVU, wiki ingest), which is pinned to `medium`. Mechanical extraction should not get more expensive because a conversational agent was turned up to `max`.

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
  fallback = "codex"
     |
     v
Route to the compatible runtime
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
everything below) happens on every non-utility Codex spawn, ReadOnly included: at ReadOnly
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
  the old conversational spawn: no gate, no write. Codex utility execution skips this MCP setup entirely and uses the restricted
  profile described above.
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

### Grok and the tool loop: the employee's other MCP servers (2026-10-20)

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
connected directly. Grok reads more than that from the working directory
(measured with `grok inspect`, and in its bundled `07-mcp-servers.md` /
`05-configuration.md`): `<cwd>/.cursor/mcp.json` (Cursor compatibility, on by
default), and, when the directory is inside a git repository, the `.mcp.json`
and `.grok/config.toml` of every directory up to the repository root. It does
not read `<cwd>/.claude.json` or the `mcpServers` of `.claude/settings*.json`.
Every one of these ranks below the working directory's `.grok/config.toml`,
so the runtime now writes every name found in them into that file: gated
servers from the employee's own `.mcp.json` as spawn-scoped proxies (below),
everything else as a disabled entry that hides the original (DuDuClaw-named
extras such as `duduclaw-pro` included). A source that exists but cannot be
parsed refuses the spawn. An entry the runtime wrote for a server the employee
no longer lists is removed; entries an operator added by hand are left alone
and logged on every spawn. `.cursor/` in an employee directory is now frozen
for the employee like `.grok/`.


**Spawn-scoped proxies (Grok).** The file is persistent,
every spawn in the directory shares them, and the CLI takes no a per-run MCP
override, so the file holds nothing per spawn and no upstream credential.
Each server is registered as `duduclaw mcp-proxy --spawn-scoped --server
<name> -- …`. The spawn writes the upstream servers' env into a new
owner-only directory under `<home>/tmp/mcp-proxy-env/` and names it in
`DUDUCLAW_MCP_PROXY_ENV_DIR`: Grok expands `${DUDUCLAW_MCP_PROXY_ENV_DIR:-}` in the entry's
`env`. The directory is deleted when the CLI exits; the gateway removes
leftovers older than 15 minutes when it starts. A spawn-scoped proxy that
finds no directory, or no file for its server, starts nothing. An operator
running `grok` by hand in the directory therefore gets none of
these proxied servers. Utility execution on Grok is refused before
spawning; employee background calls follow the utility rule above (the employee's `utility_provider`, a present global `utility_provider`, or skipped). Two spawns of the same employee write the same file content and each
get their own directory. The Grok entry also holds the proxy's own identity
(agent id, home, and the MCP credential the `duduclaw` entry of the same file
already holds), never the agent token. The upstream server starts from the
agent-CLI allowlisted environment plus its declared env, so the CLI's own
vendor key (`XAI_API_KEY`) does not reach it.

Managed spawns refuse entries in `.grok/config.toml` that DuDuClaw did not write, preserving the file for operator migration. Known limits: in a borrowed work root two employees with a server of
the same name share one entry, and the spawn that runs uses its own env file
for it; a `.mcp.json` (or `.cursor/mcp.json`) changed between the spawn's read
and Grok's own read is not covered; two server names that differ only in
letter case refuse the spawn (they would share one env file on a
case-insensitive disk). Global MCP entries under the actual `HOME` or
`GROK_HOME` now refuse managed spawns instead of being started unguarded. Grok's Claude- and Cursor-compat MCP
scanning (`~/.claude.json`, `~/.cursor/mcp.json`, `<cwd>/.cursor/mcp.json`) is
switched off for every DuDuClaw spawn with `GROK_CLAUDE_MCPS_ENABLED=false` and
`GROK_CURSOR_MCPS_ENABLED=false` (owner decision 2026-10-20; `grok inspect`
then lists those servers as `[disabled]`). Grok's Claude- and Cursor-compat
hook scanning is switched off the same way (`GROK_CLAUDE_HOOKS_ENABLED=false`,
`GROK_CURSOR_HOOKS_ENABLED=false`), so the OS user's `~/.claude/settings.json`
and `~/.cursor/hooks.json` hooks never run in an employee's turn. That also
stops Grok reading the employee's `.claude/settings.json`, so DuDuClaw's
`agent-file-guard` and `data-file-guard` are written before every spawn as a
Grok project hook file, `<cwd>/.grok/hooks/duduclaw-guards-<agent>.json`, with the same
matchers and commands; a spawn whose file cannot be written is refused. Other
hooks an operator added to the employee's `.claude/settings.json` no longer run
for Grok, and Grok's own global hooks in `~/.grok/hooks/` still do. The
Claude- and Cursor-compat rules, skills and agents cells are off too
(`GROK_CLAUDE_RULES_ENABLED`, `GROK_CLAUDE_SKILLS_ENABLED`,
`GROK_CLAUDE_AGENTS_ENABLED` and the three `GROK_CURSOR_*` twins, all `false`),
so the OS user's `~/.claude/CLAUDE.md`, `~/.claude/rules/`, `~/.claude/skills/`
and `~/.cursor/skills/` do not reach an employee's turn. The working
directory's own `AGENTS.md` and `CLAUDE.md` are Grok-native and keep loading
(with no git repository above the employee directory, Grok reads no parent
directory's `AGENTS.md` or `CLAUDE.md`); rules and skills an operator put in
the employee's `.claude/rules/` or `.claude/skills/` no longer load for Grok
(DuDuClaw's own skills live in `SKILLS/` and are not affected). Known limits in
Grok 1.0.62: `grok inspect` still lists `~/.claude/agents/` subagent
definitions with the agents cell off; it lists no `.cursor/rules/` file whether
the Cursor rules cell is on or off, so that cell is set without a verified
effect; and Grok's native user sources (`~/.grok/`, `~/.agents/skills/`) are
not compat cells and still load.

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
missing shadow would let Grok start a server ungated). Grok utility calls
are refused before config inspection or spawning, regardless of whether
`.mcp.json` is valid. With no third-party server listed every one
of these spawns is the old one. Grok refuses the read-only
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

Supported CLI backends resolve the working root through a shared helper. It checks that the requested path is a real directory, otherwise warns and falls back to the employee directory. The native OS sandbox uses the same working root.

The override moves the **working directory only**. Agent identity — the MCP
server registration, the agent id the tools authenticate with, the agent's own
configuration — stays with the agent directory.

One backend-specific consequence, stated rather than left to be discovered:

- **Grok** resolves both its MCP registration (`.grok/config.toml`) and its
  sandbox profile names (`.grok/sandbox.toml`) from the working directory, not
  from the agent directory. An overridden root therefore gets its own copy of
  both, or the member would spawn with no tools and an unresolvable sandbox
  profile. Known limitation: those two files are keyed by directory, so two Grok
  role members sharing one workspace overwrite each other's declared env block.
  The command/args halves are identical between members and the per-process
  identity is what the MCP child actually authenticates with, so the blast
  radius is the declared block only.

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

**Judge and evaluator execution does not use conversational cross-family failover.** The utility execution entry point preserves explicit `[dispatch] judge_provider` / `judge_model` hints and refuses providers without a supported utility profile. Existing higher-level judge degradation can record a warning and retry without the hint; that retry still resolves only to a supported utility profile. Unhinted employee utility calls on unsupported employee runtimes resolve to the employee's `[runtime] utility_provider`, then a global `[runtime] utility_provider` that is present in the file, and are otherwise skipped (v1.84.0). A configured global provider that has no supported utility profile is refused.

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

**Per vendor.** The full notice above is what Claude logins and Claude accounts show. Codex, Grok logins (and Add account's OAuth path with a non-Anthropic provider) show that vendor's own text and terms link instead, with the same checkbox: for OpenAI, the Sign in with ChatGPT program summarised above (open-source and approved commercial partners, tokens kept local) with a link to its terms; for xAI, that its position is unclear and its terms apply. An unknown provider gets a vendor-neutral notice with no link. Every sentence points at a vendor page; the earlier "Google has blocked server-side since 2026-03" sentence had no primary source and was removed. The vendor table lives in `duduclaw_core::subscription_risk::vendor_policy` (gateway, CLI) and `web/src/lib/subscription-risk.ts` (dashboard).

**Where it shows.** The full disclosure with a checkbox opens before any credential step in the CLI login modal, the guided QR-code setup wizard, and the OAuth Token path of Add account. A one-line note with a link to the policy sits on the Welcome page's Claude subscription card (and on its "already signed in" card, the host-login adoption case), the runtime setup card's sign-in row, the Accounts page's subscription hero card, the one-click CLI picker, the CLI credentials list, the agent editor's API mode row when it is set to the subscription CLI, and the account pool picker when a subscription account is in the pool.

**Server-side gate.** `auth.cli_login.start` (every runtime that has a scriptable login), `accounts.setup_token_start`, `accounts.add` with `type = "oauth"`, and `config.raw.set` when the submitted `config.toml` adds an `[[accounts]]` entry with `type = "oauth"` that the stored file does not have (a new id, or an existing id switched to `oauth`) refuse unless the request carries `accept_subscription_risk: true`. This is a breaking change for custom WebSocket clients, shipping in v1.74.0 (see [Deprecations](../guides/deprecations.md#changed-without-a-deprecation-window-v1740)). The refusal is a structured error with code `subscription_risk_not_accepted` plus the `provider` and its `policy_url`, which the dashboard shows in the user's language; a refused raw edit writes nothing, and the raw editor then shows the vendor's notice and saves again once it is ticked. Each accepted call writes one `subscription_risk_accepted` row to `security_audit.jsonl` with the timestamp, method, provider, account id (when known), the acting dashboard user, the vendor's `policy_url` and `outcome = "acknowledged"`; its `note` says the row records the acknowledgment, not a successful login or save. A raw edit that adds several oauth accounts writes one row each. Internal reuse of `accounts.add` by `auth.cli_login.finalize` and `accounts.setup_token_submit` is not asked again; those finish a session that an acknowledged `start` opened. API-key accounts are not gated.

**Terminal and logs.** `duduclaw onboard` prints the risk line next to "偵測到 Claude … 登入". When the account rotator adopts the host's own Claude login (`[account_loading] inherit_host_credentials`, still on by default), the gateway logs one WARN per process with the same line. `duduclaw doctor` and the dashboard doctor add a "訂閱帳號政策風險" row whenever a Claude OAuth account (provider `anthropic` or unset) is configured in `config.toml`; other vendors' oauth accounts, such as a Copilot seat, are not counted because the row quotes Anthropic's policy. `duduclaw auth device --provider copilot`, the operator command that stores a GitHub Copilot seat, prints the notice with GitHub's terms link and stops unless `--accept-subscription-risk` is given; with the flag it writes the same audit row (actor `operator-cli`).

---

## The Takeaway

The AI market is multi-provider. Building on a single CLI is like writing software for a single operating system — it works until it doesn't. The `AgentRuntime` trait abstracts away the differences, letting DuDuClaw treat Claude, Codex, any OpenAI-compatible endpoint as interchangeable backends. Your agents get the best available brain, every time.

## Run step budgets

The step budget is configured separately: global `[runtime] default_max_turns` defaults to **50**, and `max_turns_limit` defaults to **200** (integer **1–10000**). An employee may set `[runtime] max_turns` from **1** up to that ceiling; leaving it unset uses the global default. Zero never means unlimited. Invalid or unreadable settings use bounded defaults, with warnings deduplicated per employee, configured value and applied cap; values above the ceiling are clamped during execution and rejected on dashboard save. Raising an employee's limit requires an administrator; owners may lower it, and clearing it requires an administrator if the inherited default raises the effective budget. Changing either global key also requires an administrator and is audited through the existing config commit path. The dashboard places these fields beside the time limits. More steps can consume more quota and extend runs. Claude chat, background work and terminal chat use this budget; protected mode remains fixed at 50 until native Linux protected mode supports it. OpenAI-compatible tool loops use an explicit employee `max_turns`, otherwise retain 10; exhaustion returns a distinct max-turns error. Headless Claude/Grok runs disable interactive question tools. Claude receives a system-prompt reminder to summarise progress and stop near its step budget. Proactive checks retain their separate `max_turns`, accepted from **1 to 64**.

Web chat waits for the target employee’s effective single-run timeout plus 30 seconds for delivery. Goal task `timeout_secs` may range from **30 seconds** to `min(max_timeout_secs, 3600)`; each round remains capped by that setting, with a one-hour ceiling.

## Event-silence limits and continuation

The global `[runtime] default_idle_timeout_secs` defaults to **900 seconds**. An employee may override it with `[runtime] idle_timeout_secs`; absent means inherit, **0 disables**, and other values must be **60..=max_timeout_secs**. With an omitted global value, a lower global maximum also narrows the default. Read or parse failures use the built-in 900 seconds. Raising or disabling an employee limit, including clearing it to a larger or disabled global default, requires administrator authority; lowering does not. The dashboard places both fields next to the execution time limits, in the employee editor and global run limits.

Claude chat and background runs, Codex JSONL runs, and OpenAI-compatible SSE responses stop when no event arrives within that window. Any parsed event counts, including tool events; the periodic 90-second keep-alive notice does not. The last unfinished tool is named when known. Grok and non-streaming OpenAI-compatible requests/tool loops retain their total timeout because these paths do not expose a live event stream here.

A step limit (`max_turns`), total timeout (`timeout_total`), or event-silence timeout (`timeout_no_progress`) ends the current turn without marking an account unhealthy, changing accounts, or retrying another model/runtime. A timeout reports that **no final answer was produced before the time limit**, even when tools ran. For signed-in channel users, captured assistant text is delivered before the notice and saved as an assistant turn; reply “continue” to resume with that context. Claude step-limit notices include the applied step count. Public visitors receive only the visitor-safe generic message and reference number, with no partial text, counts, reason code, or operator hints.

Failure notices end with a stable lowercase reason code and the first eight characters of the turn reference. The audit record retains `reason_code` and the full `reference_id`; background results use the dispatch run reference. Goal-loop work limits pause with `max_turns`, `timeout_total`, or `timeout_no_progress`, without accumulating infrastructure dispatch failures.
