# DuDuClaw Project Guidelines

## Highest directive: every new feature and every fix gets its own branch

Owner instruction, 2026-10-03: 「每次有新功能與修復時都開分支」. It outranks every other workflow rule in this file.

- Before starting a feature or a fix, create a branch from the latest `origin/main` (`feat/…`, `fix/…`, `docs/…`, `chore/…`). Feature and fix commits never go on `main` directly.
- The change reaches `main` through a pull request. `main` requires five checks on a PR merge: Rust ubuntu-latest, Rust macos-latest, Dashboard, Python, Security Audit (`cargo audit`, required since 2026-10-08). The ECC Tools app comments on PRs (security evidence, risk taxonomy and other audits); its results are advisory and not required checks.
- When another session is using the main checkout, work in a separate `git worktree` instead of switching branches there.
- When several open PRs touch the same files, merge them in the order the owner gives; the later one rebases.
- Opening a branch is not permission to merge or release. Merging a PR and cutting a release stay the owner's decision.


## Where the detail lives

This file is loaded into every session, so it keeps only the map, the cross-cutting rules and the conventions. Detail lives elsewhere:

- `CHANGELOG.md`: what shipped in which version (zh-TW, user-facing). Source of truth for version numbers.
- `wiki/duduclaw-kb/architecture/subsystem-reference.md`: the long per-subsystem history (file paths, config keys, measured numbers, known gaps). Moved verbatim from this file on 2026-10-12; code comments that say "see CLAUDE.md's … note" (credentials consolidation, agent-id validator waves, harness-borrowing waves) now mean this file.
- `wiki/duduclaw-kb/architecture/release-implementation-notes.md`: developer notes for v1.67.0 → v1.72.0, one block per feature batch, including each batch's "not verified" list.
- Public docs: `docs/features/` and `docs/guides/` (three languages), `docs/features/05-security-defense.md` (canonical security description), `docs/guides/deprecations.md`, `docs/guides/dashboard-settings.md`, `docs/guides/mcp-tools.md`, `SECURITY.md`.

When a change lands: write the detail into `CHANGELOG.md` and the wiki reference; touch the map below only when one of its lines becomes wrong or a new subsystem appears. Keep this file short (target under 40 KB).

## Current state (v1.72.2, 2026-10-11)

- Workspace version 1.72.2. Nothing is under `[Unreleased]` in `CHANGELOG.md` as of 2026-10-12.
- Gemini CLI runtime (`gemini`) is deprecated, removal scheduled for v1.75.0; it waits for a real-key re-verification of Antigravity after v1.69.1. Version lives in `runtime_catalog.rs` `RuntimeSpec.deprecation`. The Gemini API provider is not deprecated.
- Legacy pack manifests (`expert.toml`, `team.toml`, industry pack directories) are deprecated with no removal version (a `pack.toml` team/template pack cannot be installed yet).
- Deprecation policy: a renamed or merged public name survives two minor versions. MCP tool: stays listed and callable with a `[deprecated → …]` description prefix; CLI: hidden clap alias; config value: still parses and warns once per process; dashboard: offers only the new name. Table: `docs/guides/deprecations.md`.

## Architecture map

DuDuClaw is a multi-runtime AI agent platform: it routes channel messages and dispatched work to AI CLIs or APIs and provides sessions, memory, evolution, approvals, account rotation, local inference and computer use around them. One Rust binary embeds the dashboard (`web/`).

### Runtimes and model calls
- **Runtimes** (`AgentRuntime`, `RuntimeRegistry`, per-agent `agent.toml [runtime]`): Claude, Codex, Gemini CLI (deprecated), Antigravity (`agy`), Grok, OpenAI-compat. Sandbox flags come from `CapabilitiesConfig::sandbox_level()`; each non-Claude CLI gets the DuDuClaw MCP server through its own native config.
- **Spawning**: every Claude call is a fresh `claude -p` (the PTY session pool was removed 2026-09; it had no conversation key). `duduclaw-cli-runtime` runs TTY-only CLIs (Grok) one-shot under a PTY. Spawn env is an allowlist (`duduclaw-core/src/spawn_env.rs`; vendor secrets injected explicitly; `[capabilities] git_credentials` opts into SSH/GPG). `[runtime] minimal_context` (default on) passes a curated `--tools` list and `--setting-sources project,local`.
- **Accounts and failover**: account rotator (`duduclaw-agent/src/account_rotator/`): OAuth + API keys, Priority/LeastCost/Failover/RoundRobin, cooldowns, `auth_dead` from real probes; `[model] account_pool` narrows candidates and fails open. Three failover layers in `duduclaw-gateway/src/failover.rs` (account, model, runtime). `direct_api.rs` is the paid fallback with `CACHE_SPLIT_MARKER` cache breakpoints.
- **`duduclaw-llm`**: one request/stream/usage shape over Anthropic Messages, OpenAI Responses, Gemini `generateContent` and OpenAI-compat presets; `ModelRegistry` prices; `run_tool_loop` + stdio MCP client give API and openai-compat agents the MCP tool surface; `ToolInterceptor` hook.
- **Local inference** (`duduclaw-inference`): only the OpenAI-compatible HTTP backend remains (in-process llama.cpp / mistral.rs / MLX removed 2026-09-29); confidence router; optional UCCI cascade; llamafile manager.
- **Team-as-Agent** (`[team]`): planner / executor / verifier / synthesizer roles per goal round, exchanging only `TaskPacket`s. Same-family roles are refused (decorrelation), so without a second vendor in `[team.roles]` work stays Solo. Gate `duduclaw-core/src/team_gate.rs`.

### MCP server and tool gates
- **Server**: `duduclaw mcp-server` (stdio JSON-RPC), registered per employee in `<agent_dir>/.mcp.json`, rewritten at boot (`ensure_mcp_absolute_paths_all`) and re-verified before every Claude CLI spawn (`prepare_mcp_config_for_spawn`; a refusal carries `SPAWN_GATE_ERROR_PREFIX` and stops rotation/fallbacks). Tool definitions: `crates/duduclaw-cli/src/mcp/tools_def/` (247 `ToolDef` entries on 2026-10-12; re-count with `grep -rc 'ToolDef {' crates/duduclaw-cli/src/mcp/tools_def/ | awk -F: '{s+=$2} END {print s}'`). Also `duduclaw http-server` (HTTP/SSE + OAuth AS with CIMD) and `duduclaw mcp init` (standalone profile, external keys in their own namespace).
- **Discoverable ⇔ callable**: `tools/list` is filtered with the same constants the dispatch gate uses (`mcp/tools_list.rs`, `mcp_dispatch.rs`), and `notifications/tools/list_changed` fires when the visible set changes. Hiding is never authorization; an unlisted call still hits the gate. A tool missing from the scope table needs Admin.
- **Gates at dispatch** (all fail closed, all only narrow): scopes; `denied_tools`/`allowed_tools`; `[permissions]` flags; `scoped_tools` task grants; `action_rules` over effect classes (`duduclaw-core/src/tool_effect.rs`, unknown tool = `admin`); explore lane (`DUDUCLAW_LANE=explore`, read/draft only; single flag `explore_lane::EXPLORE`; supported for Claude, Codex, OpenAI-compat); approval lists through `mcp/approval.rs` with single-use `ActionGrant` binding; action review (`[action_review] mode`, default off); record authorization for task/cron/reminder changes (`mcp/record_authz.rs`).
- **Third-party MCP**: other `.mcp.json` servers are classified from tool annotations and gated through `duduclaw mcp-proxy` (`third_party_tools.rs`). Remote servers: `remote_mcp/` (encrypted store, OAuth with PKCE/DCR/resource indicators, stdio⇄Streamable HTTP bridge `duduclaw mcp-remote-bridge`), MCP Registry search/install, MCP Events receiver and poller (`mcp_events/`).
- **Redaction** (RFC-23): rule engine incl. on-device NER, data-source registry, `duduclaw-db` read-only DB connector, local file tools, `data-file-guard` hook. See `docs/features/55-data-sources.md`.

### Identity, authority, approvals
- **Delegation**: `duduclaw-core/src/delegation_policy.rs` (`reports_to` tree both directions, same department, explicit pairs; `[delegation] policy`). Org authority `~/.duduclaw/org.toml` (`duduclaw org sync`). Employee identity is proven by `DUDUCLAW_AGENT_TOKEN` (HMAC); system-sender names (`dashboard`, `cron`, `heartbeat`, …) are never accepted as an MCP identity.
- **Hooks** (installed per employee by `agent_hook_installer`): `duduclaw hook agent-file-guard` refuses writes under `<home>` outside the own directory and `attachments/`, the own `SOUL.md`/`CONTRACT.toml`, any `.mcp.json` or CLI config, cross-agent writes, and frozen org/`[capabilities]` fields (`org_field_guard`). The Bash lane is a speed bump; real containment is not granting Bash or using the task sandbox. `duduclaw hook data-file-guard` is a filename heuristic.
- **Approvals**: `ApprovalBroker` (`approval/`, expiry = deny), one push path `notify_push::push`, dashboard-only kinds. Operator terminal commands that change state (`ops channel-ingress`, `responsibility`, `memory forget-source`, `ops computer-workspaces`) only file a dashboard Admin approval (`approval/operator_cli_gate.rs`, `OPERATOR_CLI_KINDS`) and refuse inside AI sessions (`duduclaw-cli/src/ai_session_guard.rs`).
- **Content screening**: `duduclaw_security::input_guard` (11 rule categories, en + zh-TW); SOUL.md is read-only for employees; `CONTRACT.toml` + `duduclaw test` red team; task audience lists narrow non-Admin and channel reads.

### Work execution
- **Task board** (`tasks.db`): `tasks_create` is the single entry (`kind = "goal"`, `schedule` cron or RFC3339); record authorization limits AI employees to their own tasks.
- **Dispatch and goal loop** (`[dispatch] enabled`, default on): two-stage judge (cheap evaluator, then three-aspect MAV panel), done only by verifier approval, strict reply parsing (`[dispatch] strict_reply_parsing`, default shadow), per-criterion ledger, `needs_human` with closed pause reasons, plan mode, `resume_on_restart = "pause"`. Bus: `bus_queue.jsonl` + `message_queue`, `dispatch_guard`, hop depth, `spawn_admission` queue.
- **Responsibilities** (`responsibility/`, `[responsibilities] enabled` default off), steering, task stop; **workflows** (`workflow/`, Admin-only activation, cost ledger); **autopilot** rules on the event bus (`events.db`, circuit breaker), resident sensing (`[tick]`), Odoo events; cron, reminders, heartbeat scheduler; employee digest and interim status.
- **Sandboxes**: task sandbox (`task_sandbox.rs`, `[container] sandbox_enabled`) and script sandbox (`duduclaw-container`); both fail closed unless `when_unavailable` / `script_when_unavailable = "run_unsandboxed"`.
- **Computer use**: eight `computer_*` MCP tools drive gateway-owned containers (`computer_use_sessions/`) over a signed loopback route; network only to pinned `allowed_domains`; keep-alive, VNC live view over a unix socket, takeover, injection pause; durable workspaces (`computer_workspaces/`). The chat-triggered loop and native mode are removed.
- **Discovery** (`discovery/`): operator-only exploration in disposable containers.
- **Single gateway**: maintenance loops run only in the process holding `<home>/locks/gateway.lock` (`gateway_instance::held`).

### Memory, knowledge, evolution
- **Memory** (`duduclaw-memory`): temporal SPO facts with supersession, Ebbinghaus decay, graph PPR, origin trust + supersession trust guard, novelty gate, source lineage and forget-by-source. Gateway auto paths build engines through `memory_factory::build_memory_engine`. Verified employees use their bare id as namespace; external keys get their own.
- **Wiki**: agent and shared wikis behind one tool set with `scope`; `.scope.toml` policies; auto-filed reference pages under `auto/`.
- **Every wake-up** gets working state (`working_state.rs`, MCP-write only) and recent own actions from `tool_calls.jsonl` (`recent_actions.rs`).
- **Evolution**: AEE is the only path (`gvu/aee/`): playbook entries with eval-case links and assertions, Gate (veto) / Measure (no veto), champion snapshot. Prediction engine, task forward model with calibration and held-out rule gate, belief loop (only verified settlements count, and nothing in production verifies one yet).

### Channels, dashboard, other lines
- **Channels**: eleven (Telegram, LINE, Discord, Slack, WhatsApp, Feishu, Google Chat, Teams, WeCom, DingTalk, WebChat); capability table `channel_capabilities.rs`; durable LINE ingress (`channel_ingress/`); chat decisions need verb + full request UUID; per-employee bot tokens only for Telegram/Discord/Slack (`reports_to` cascade).
- **Odoo website Live Chat** (`odoo_livechat/`, `[odoo_livechat]`, default off): the bot answers as an Odoo Live Chat operator kept online by the gateway; audience rules pick the employee per visitor; Odoo-native takeover. Public-visitor sessions (`odoolivechat:` prefix) never see operator-facing fallback text, notices or `/takeover`. Detail: `docs/features/61-odoo-livechat.md`.
- **Dashboard**: JWT account or admin token; config writes through `handlers/config_commit.rs` (lock, hash compare, format-preserving); raw editor; settings map `docs/guides/dashboard-settings.md`. New nav pages carry a `newIn` version badge.
- **Elsewhere**: OS-native line (`duduclaw-os`, `duduclaw-sysd`, `os_ops.rs` as the single authority for device `os_*` capabilities; gpui/smithay UI crates live in the DuDuClaw-OS repo); packs (`duduclaw pack`); skills; integrations gated by `[integrations]`; cost telemetry; OpenTelemetry (opt-in); A2A agent card; CCR, causal graph and decision twin (admin-only, gated).

## Cross-cutting rules

- A gate that cannot read its config, its file or its identity denies. Validate the artifact that takes effect, not the proposal.
- New settings and arguments may only narrow what an employee can do unless the owner decided otherwise; `allow` never removes a name-list gate.
- Anything an employee can list it can call, and anything it cannot call is not listed.
- Every spawn that hands an employee's `.mcp.json` to a CLI goes through the spawn gate; employees never write MCP or CLI config files.
- Facts that need the live gateway (`ApprovalBroker`, sessions, rotator) stay in the gateway; the separate `mcp-server` process reaches them through signed loopback routes or the shared databases.
- Write honest "not verified" notes. Live tests use an isolated home (`scripts/live-test/make-home.sh`, `mcp-probe.sh`) with a production-shaped employee (wildcard `mcp__duduclaw__*` allowlist).
- Windows runs in CI as an advisory leg (not one of the five required checks); before a release also run the local Windows cross-compile check. Discovery and computer-use workspaces are Unix-only by design.

## Design Context

### Users
DuDuClaw is a **Multi-Runtime AI Agent Platform** for individual developers and power users, primarily in Taiwan (zh-TW). It supports Claude Code / Codex / Gemini CLI (deprecated) as interchangeable AI backends. Users interact through a web dashboard to manage AI agents, monitor channels (LINE/Telegram/Discord/Slack/WhatsApp/Feishu), track API budgets, and observe agent self-evolution. They expect a tool that feels like a trusted companion — not a cold enterprise console.

### Brand Personality
**Professional · Efficient · Precise** — with a warm, approachable surface.

Like a skilled engineer who happens to be your close friend: reliable, sharp, but never cold. The paw print (🐾) icon reflects a pet-like companionship — the AI is loyal, attentive, and delightful to interact with.

### Aesthetic Direction
- **Primary references**: Claude.ai (warm sand/beige tones, generous whitespace, soft typography) + Raycast (macOS-native polish, frosted glass effects, refined dark theme)
- **Anti-references**: Grafana (too dense), Discord (too playful), enterprise dashboards (too cold)
- **Color palette**:
  - Primary: warm amber (`amber-500` / `#f59e0b`) — evokes warmth and trust
  - Accent: soft orange (`orange-400` / `#fb923c`) — for highlights and CTAs
  - Surface light: warm stone (`stone-50` / `#fafaf9`) with subtle warm undertones
  - Surface dark: deep stone (`stone-900` / `#1c1917`) — warm dark, not cold blue-black
  - Success: emerald, Warning: amber, Error: rose — standard semantic colors
- **Theme**: Follow system preference (auto dark/light), with manual toggle
- **Typography**: System font stack for performance; generous line-height; larger body text (16px base)
- **Border radius**: Rounded (0.75rem default) — soft, approachable
- **Spacing**: Generous padding — the interface should breathe
- **Motion**: Subtle fade-in/slide transitions (150-200ms); respect `prefers-reduced-motion`
- **Glass effects**: Subtle backdrop-blur on sidebars and overlays (Raycast influence)

### Design Principles
1. **Warmth over sterility** — Every surface should feel inviting. Prefer warm neutrals over cold grays. Use color strategically to create emotional connection.
2. **Clarity over density** — Show what matters, hide what doesn't. Progressive disclosure: summary first, details on demand. Never overwhelm.
3. **Real-time without anxiety** — Status indicators should inform, not alarm. Use gentle transitions for state changes. Green means "all is well" and should be the dominant state color.
4. **One binary, one experience** — The dashboard is embedded in the Rust binary. It should feel native and instant, like a local app, not a remote web service.
5. **Accessible by default** — WCAG 2.1 AA compliance. Semantic HTML. Keyboard navigation. Respect motion preferences. Sufficient color contrast in both themes.

## Coding Conventions (security-relevant, from 2026-06 deep review)

These conventions exist to prevent classes of bugs surfaced by the 2026-06 whole-project review. Shared helpers live in `duduclaw-core`.

1. **Never slice strings by raw byte index.** `&s[..n]` panics when `n` lands mid-char on multi-byte UTF-8 (any CJK/emoji input). Use `duduclaw_core::truncate_bytes(s, max_bytes)` (byte budget, walks back to a char boundary) or `truncate_chars(s, max_chars)` (codepoint count). Applies to every channel/log snippet and id prefix.
2. **No unanchored `contains` / `starts_with` for security or routing decisions.** Substring checks let `http://localhost.evil.com` pass a `localhost` allowlist and the keyword `hi` match inside `this`. Use `duduclaw_core::origin_host_matches(origin, &allowed)` for Origin/CORS allowlists and `word_contains_ci(haystack, needle)` for keyword matching; use exact/token equality for id routing.
3. **Cross-process file appends must hold an advisory lock.** Shared JSONL (`bus_queue.jsonl`, audit logs) is appended by both the Rust gateway and Python adapters; records over `PIPE_BUF` interleave and corrupt lines. Wrap the write in `duduclaw_core::with_file_lock(path, || …)`.
4. **Security gates fail closed.** Missing config, a detection error, or an unenumerated tool/method must DENY, never fall through to allow. MCP authorization is enumerated explicitly; any tool not in the scope table defaults to requiring Admin scope. Validate the *artifact that actually takes effect* (the written `patch.content`, the final screenshot), not just the proposal/narrative.

## Documentation Classification & Placement (from 2026-06-20 doc reorg)

Every document (`.md`, design notes, reports, specs) MUST be filed by **TYPE × CONFIDENTIALITY**. When creating or moving a doc, pick the matching cell below — do not drop loose files at a tree root, and never co-locate a higher-confidentiality doc inside a lower-confidentiality tree.

### Confidentiality tiers (3 levels)

| Tier | Meaning | Git state | Lives in |
|------|---------|-----------|----------|
| **L1 Public** | Product / developer docs intended for the public repo, website, npm/PyPI README | Committed, externally published | root `README*.md`, `CHANGELOG.md`, `ARCHITECTURE.md`, `SECURITY.md`, `LICENSING.md`, `docs/`, `templates/`, `marketing/press-kit/` |
| **L2 Internal** | Dev/ops material — committed but not product-facing | Committed (visible to repo collaborators), **not** polished docs | `wiki/` (knowledge base + reports), `team/`, `ISSUES.md`, `marketing/{blog,social-templates,threads-drafts}/` |
| **L3 Confidential** | Commercial plans, competitive analysis, pricing, research notes, secrets, build output | **gitignored — never committed to the public repo** | `commercial/` (own nested git), `research/`, `artifacts/`, `.env*`, anything under a secret manager |

**Hard rule:** L3 lives ONLY in gitignored trees. Before committing, confirm no `commercial/`, `research/`, pricing, or secret content is staged. `commercial/` and `research/` are in `.gitignore`; if `git ls-files <tree>` ever returns rows, untrack them with `git rm --cached -r <tree>` (files stay on disk).

### Type taxonomy → canonical home

**L1 Public — `docs/` only (one subdir per type, never loose at `docs/` root):**
- `docs/architecture/` — system architecture & engine design (`overview.md`, `evolution-engine.md`)
- `docs/rfc/` — Request-for-Comments proposals (`RFC-NN-*.md`, plus their operator guides)
- `docs/adr/` — Architecture Decision Records (`ADR-NNN-*.md`)
- `docs/todo/` — public planning / tracking (`TODO-*.md`)
- `docs/spec/` — open format specs (SOUL.md, CONTRACT.toml, JSON Schema)
- `docs/features/` — feature deep-dives (+ `ja-JP/`, `zh-TW/` translations)
- `docs/guides/` — user & developer how-tos (deployment, development, docker, MCP tools)
- `docs/api/` — protocol / OpenAPI reference

**L2 Internal — operational, under `wiki/` (single report home) + `team/`:**
- `wiki/reports/tl-daily/<YYYY-MM-DD>.md` — daily reports (date-only filename; **never** `daily-report-*` and **never** in `docs/`, `reports/`, or `artifacts/`)
- `wiki/reports/skill-pipeline/<YYYY-MM>/`, `wiki/reports/memory-quality/<YYYY-MM>/` — automated pipeline/eval outputs
- `wiki/sprint-reports/`, `wiki/pm/` — sprint logs & PM tracking
- `wiki/duduclaw-kb/{architecture,decisions,specs,drafts,w19,sprint-n}/` — internal knowledge base
- `wiki/{eval,impl,research}/` — evaluation sets, implementation notes, internal design feedback
- `team/` — team roster & role docs

**L3 Confidential — gitignored trees (do NOT add to `docs/` or `wiki/`):**
- `commercial/docs/` — business plans, pricing, competitive analysis, SI methodology, commercial design docs (e.g. PTY-pool design, runtime TODOs)
- `research/` — AI paper / repo research notes (auto-mirrored to the shared wiki at runtime)
- `artifacts/` — build output (tarballs, generated bundles)

### When in doubt
1. **Is it secret / commercial / competitive / a research note?** → L3 gitignored tree. Stop.
2. **Is it a daily/sprint/eval/PM report or internal KB?** → L2 under `wiki/` (reports → `wiki/reports/…`).
3. **Is it polished product/dev documentation safe to publish?** → L1 under the matching `docs/<type>/` subdir.
4. Update `docs/README.md` (the public index) whenever a `docs/` file is added, moved, or removed; update in-repo links (READMEs, `CLAUDE.md`, code comments, `CONTRACT.toml`) in the same commit.
