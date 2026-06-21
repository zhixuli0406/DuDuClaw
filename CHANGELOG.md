# Changelog


## [1.22.0] - 2026-06-21 — RFC-26 Live Forking · skill-synthesis scheduler · Calm Glass dashboard

Inspired by [vstorm-co/pydantic-deepagents](https://github.com/vstorm-co/pydantic-deepagents),
this adds **Live Run Forking** — split an in-flight agent task into N competing
branches that explore different strategies in isolated copy-on-write workspaces,
then let an AI judge pick the winner. **Default off**; per-agent opt-in via
`agent.toml [fork] enabled = true`. See `docs/rfc/RFC-26-deep-agents-alignment.md` and
`docs/todo/TODO-rfc26-live-forking.md`.

### Added
- **New crate `duduclaw-fork`** — the forking engine: `Branch`/`BranchState`,
  copy-on-write `BranchOverlay` (reads fall through to parent, writes stay local,
  `promote()` merges the winner), per-branch + aggregate `budget::Pool`,
  `ForkController` over a decoupled `BranchExecutor` trait, a `JudgeAgent` with the
  deep-agents confidence formula (`quality·0.4 + test_pass·0.4 + consistency·0.2`),
  4 merge modes (`manual`/`auto`/`auto_with_fallback`/`vote`), and a `test_runner`
  that scores branches by their configured test command. (49 unit tests.)
- **6 MCP tools** gated by the new `Scope::ForkExecute` + the `[fork] enabled`
  toggle: `fork_run`, `inspect_branches`, `diff_branches`, `merge_or_select`,
  `terminate_branch`, `fork_cost` (`crates/duduclaw-cli/src/mcp_fork.rs`).
- **`RotatingBranchExecutor`** — runs each branch through the `AccountRotator`
  + a real `claude` spawner, enforcing per-branch and aggregate USD budgets; forks
  execute in a **background** task so the MCP stdio loop never blocks. Branch
  outcomes + spend recorded to `~/.duduclaw/fork_history.jsonl` (advisory-locked)
  with in-process `FORK_METRICS` counters (`crates/duduclaw-cli/src/mcp_fork_exec.rs`).
- **Checkpoint fork/rewind** (`duduclaw-durability`) — `fork(checkpoint_id, new_task)`
  copies state under a new lineage, `rewind(task, checkpoint_id)` restores an earlier
  snapshot, `Checkpoint.parent_checkpoint_id` tracks lineage; id-addressable archive.
- **Smoke harness** `scripts/smoke-fork.{sh,ps1}`.

### Added (round 2 — cross-process + parity follow-ups)
- **Shared SQLite fork store** (`duduclaw-fork::ForkStore`, WAL at `~/.duduclaw/fork_store.db`) — the
  cross-process source of truth. `mcp_fork`/`mcp_fork_exec` refactored onto it.
- **Gateway `/metrics`** emits `duduclaw_fork_*` lines (read from the store at scrape time).
- **Dashboard ForkPage** (`web/`) + `fork.list/inspect/resolve` WebSocket RPC — list forks, compare
  branches side by side, see the judge's winner, resolve manually. New `/forks` route + nav.
- **`memory_improve`** MCP tool — clusters memories by tag into a propose-not-apply reflection scaffold.
- **`plan_start`** MCP tool (Plan Mode) — clarify-first planning scaffold, `agent.toml [planner]` toggle.
- **Built-in skills** — `code-review`/`refactor`/`test-writer`/`git-workflow` seeded idempotently into
  every new agent's `SKILLS/` at creation.
- **Checkpoint durability** — `CheckpointManager::with_persistence` SQLite backend; fork/rewind/lineage
  survive restart. **Task Board** — `claim_task` (atomic CAS) + parent-cycle detection.
- **Fork executor hardening** — branches capped to distinct available accounts (logged); pre-spawn
  cancellation registry for `terminate_branch`.

### Added (round 3 — the last deferred items)
- **Native copy-on-write overlay** — `BranchOverlay` clones the parent workspace via `clonefile(2)`
  (`cp -c`, macOS/APFS) or `cp --reflink` (Linux btrfs/XFS); `detect_backend()` probes once and falls
  back to the snapshot copy if unavailable.
- **Streaming budget enforcement + external SIGKILL** — `ClaudeCliSpawner` streams stream-json, charges
  `total_cost_usd` incrementally, and kills the child mid-stream on per-branch overspend
  (`SpawnOutcome::BudgetExceeded`); a per-branch kill-switch registry lets `terminate_branch` SIGKILL an
  in-flight subprocess (`→ Terminated`).
- **Activity-Feed mirroring** — fork resolutions write a `fork_resolved` row into the gateway's
  cross-process `activity` table (`<home>/tasks.db`), surfacing on the dashboard Activity Feed.

### Added (round 4 — cross-branch aggregate pre-emption)
- **`duduclaw_fork::LiveAggregate`** — a streaming-time companion to `budget::Pool`, shared across a
  fork's concurrent branches. It tracks every in-flight branch's live `total_cost_usd`; the moment their
  combined spend crosses the aggregate cap it names the **most-expensive in-flight branch** (deterministic
  tie-break) so it can be pre-emptively killed — instead of waiting for each branch to hit its own
  per-branch cap. (5 unit tests.)
- **Spawner wiring** (`mcp_fork_exec.rs`) — each stream-json cost update runs the pure
  `stream_budget_decision` (per-branch cap → aggregate `observe`): the priciest over-budget branch
  self-kills if it is the observer, otherwise the observer `request_budget_kill`s the sibling. The
  aggregate kill is tagged so the woken branch maps to `BudgetExceeded` (→ `BudgetKilled`), distinct
  from an operator `terminate_branch` (`Cancelled` → `Terminated`); `LiveAggregate::finish` frees a
  branch's budget for survivors once it ends. (5 unit tests.) Completes RFC-26 §4.2.

### Added (skill synthesis — W19-P1)
- **Periodic auto-run scheduler** (`skill_synthesis_pipeline::scheduler`) — runs the
  rollout-to-skill pipeline on a fixed interval instead of waiting for a manual
  `skill_synthesis_run` MCP call. **Off by default**, **dry-run by default even when
  enabled**, hot-reloaded config (`config.toml [skill_synthesis] auto_run/dry_run/
  interval_hours/lookback_days/target_agent`), and non-blocking (pipeline errors are
  captured into the run summary, never abort the loop).
- **Dashboard config RPCs** — admin-gated `skill_synthesis.get` / `skill_synthesis.update`
  (validated writes onto `config.toml [skill_synthesis]`); `skill_synthesis_threshold`
  is a `u32` count (no longer a float — fixes registry scan rejecting `0.7`).
- **`fetch_episodic_evidence`** with path-traversal rejection + tests.

### Added (dashboard — Calm Glass redesign)
- **Calm Glass design system** — shared component library (`web/src/components/ui/`:
  Page / PageHeader / Section / Card / StatCard / Button / Badge / Field / Tabs /
  EmptyState) + a 6-group sidebar nav model (`layout/nav-model.ts`), applied across
  every dashboard page. Design spec in `web/DESIGN.md`; design tokens in `index.css`.
- i18n keys synchronized across `en` / `ja-JP` / `zh-TW`.

### Documentation
- **Feature docs reorg + trilingual coverage** — 10 new feature deep-dives (20–29:
  memory-intelligence, governance-layer, durability-framework, autopilot-engine,
  task-board, identity-resolution, mcp-http-sse, pty-pool-runtime, live-forking,
  evolution-events) in `en` / `ja-JP` / `zh-TW`; back-translated 16–19 to `ja-JP` /
  `zh-TW`; `feature-inventory` refreshed `v1.8.14 → v1.22.0`; README indexes updated.

### Housekeeping
- Removed residual local artifacts (test `.profraw`/coverage, stale 5.6 GB git
  worktrees) — gitignored cruft only, no repo content affected.


## [1.21.1] - 2026-06-18 — Channel routing: stop bot "identity mixing"

### Fixed
- **Agent-bound bot token now takes precedence over the global poller**
  (Telegram / Slack / Discord). These channels' long-poll (`getUpdates`) and
  gateway sessions are exclusive per bot token. When the same token was
  configured both globally (`config.toml`) and on a specific agent, the dedup
  kept the generic **global** poller and skipped the agent one — so two pollers
  fought over the same token (Telegram **409 Conflict**, dropped messages) and
  the surviving generic poller routed via `default_agent`, causing **identity
  mixing** (e.g. a CEO bot sometimes answered as COO). Precedence is reversed:
  agent tokens are collected first and the global poller is skipped (with a
  `WARN` naming the owner) for any token an agent already binds. Extracted the
  shared `find_global_token_owner` helper with unit tests.
- **`default_agent` validation at startup** — a dangling `default_agent`
  (pointing at a renamed/removed agent) silently fell back to an arbitrary main
  agent at routing time, the other root cause of identity mixing. The gateway
  now validates `default_agent` against the loaded registry at boot and `WARN`s
  loudly (listing available agents), and the per-turn fallback path warns too.



## [1.21.0] - 2026-06-17 — RFC-25 §5 Followups: non-Claude path fully functional

RFC-25 v1.20.0 compiled the multi-runtime abstraction but left the non-Claude
(Codex / Gemini / OpenAI-compat) path as a thin opt-in with documented gaps.
v1.21.0 closes all 11 of those gaps so non-Claude agents are first-class, and
hardens the release tooling so PyPI can no longer be silently skipped.

### Added
- **Multi-turn context for non-Claude runtimes** (A1): `conversation_history`
  threaded through the choke-point and consumed by Codex / Gemini / OpenAI-compat
  (OpenAI-compat uses native multi-turn `messages`); duplicate `ConversationTurn`
  consolidated onto `runtime::ConversationTurn`.
- **Non-Claude cost telemetry** (A3): `run_agent_prompt` records token usage to
  `CostTelemetry` (detached, classified by `request_type`) so Codex/Gemini/OpenAI
  usage is visible to cost summaries, the 200K price-cliff warning, and adaptive routing.
- **Non-Claude channel keepalive** (A4): periodic `ProgressEvent::Keepalive` during
  long non-Claude replies so channels don't look stalled / hit idle timeouts.
- **`scripts/release.sh` multi-platform sync**: `audit` (per-platform version + drift),
  synchronized bump across every manifest, a post-bump assertion that aborts if any
  platform is left behind, and `verify <version>` that queries PyPI + npm.

### Changed
- **Pending-tasks for non-Claude delegation** (A2): the Task-Board queue is inlined
  into the non-Claude sub-agent system prompt.
- **Routing decision centralized** (B1): `RuntimeSettings::non_claude_provider()`;
  "Claude into the registry" is an explicit non-goal (orphan `runtime/claude.rs`).
- **Single `agent.toml` parse per reply** (B2): `RuntimeSettings` + `load_runtime_settings`
  threaded via `AgentPrompt.runtime_settings` (3 → 1 reads/reply).
- **Per-(home,provider) failover health** (R1): choke-point routes through
  `FailoverManager` (3 failures → 60s cooldown → fallback), keyed per home to avoid
  cross-tenant bleed.
- **Per-home `RuntimeRegistry` cache** (R2): replaces the first-home-binding `OnceCell`.
- **A2A target resolution** (R3): `resolve_target_agent` (default → Main agent, else
  validated) + per-home `AgentRegistry` cache with agents-dir mtime invalidation.
- **Utility provider routing** (N2): summarizer / wiki-ingest / reflection / synthesis
  honour the agent's (or global `config.toml [runtime] utility_provider`) runtime.
- `DEFAULT_UTILITY_MODEL` is a single source in `duduclaw-core` (B3).

### Fixed
- **PyPI release miss**: `pyproject.toml` had drifted to 1.18.0 (while Cargo/READMEs
  were at 1.20.0), so the CI `pypi-publish` job built a stale wheel that
  `skip-existing` silently dropped. `release.sh` now syncs every manifest and
  asserts they all reach the new version; this release also heals the drift
  (pyproject + npm manifests → 1.21.0).
- `record_usage` no longer blocks the reply on a synchronous SQLite write, and skips
  empty-`agent_id` (agent-less utility) attribution.



## [1.20.0] - 2026-06-16 — RFC-25 Multi-Runtime Unlock + A2A

The "Multi-Runtime four-backend" abstraction was previously **orphan, uncompiled
source** — every execution path hardcoded Claude. RFC-25 compiles and wires it,
and routes the LLM-calling subsystems through a single provider-agnostic
choke-point. Existing agents are unaffected (provider defaults to Claude); agents
can now opt into Codex / Gemini / OpenAI-compat via `agent.toml [runtime] provider`.

### Added

- **`RuntimeType` (duduclaw-core)** — `{Claude, Codex, Gemini, OpenAiCompat}`,
  unblocking the never-compiled `runtime/` abstraction (`AgentRuntime` trait,
  `RuntimeRegistry`, four runtime impls) + `failover.rs` — now compiled for the
  first time.
- **`runtime_dispatch::run_agent_prompt` choke-point** — resolves the agent's
  `[runtime] provider` → selects from a lazily-built, auto-detecting
  `RuntimeRegistry` → executes, falling back to the configured fallback then Claude.
- **`runtime_config`** — reads `[runtime] provider`/`fallback` and `[model] utility`;
  `ModelConfig.utility` (default `claude-haiku-4-5`) centralizes the previously
  scattered hardcoded utility-model literals.
- **A2A real execution** — the ACP stdio server's `tasks/send` now runs the target
  agent (via the provider-aware gateway dispatch) instead of a placeholder; the
  responding agent's `[runtime] provider` is honoured.

### Changed

- **GVU evolution allowlist relaxed** — the hard `ALLOWED_EVOLUTION_MODELS` reject
  (which forced `claude-haiku-4-5` and blocked everything else) is now a warning.
- **Channel reply** routes non-Claude providers through the choke-point; Claude
  keeps the optimized OAuth-rotation/PTY path (zero regression).
- **GVU loop + sub-agent delegation** (`call_claude_for_agent_with_type`,
  plain + worktree paths) route through the choke-point, honouring per-agent provider.

### Fixed

- `failover::is_non_retryable` now matches free-form "content policy" (a never-run
  test in the previously-uncompiled module).

### Notes

- `a2a/1` HTTP capability stays gated (separate transport, not wired to A2A
  execution). Sandbox (container) delegation and home-only utility tasks remain on
  Claude — documented follow-ups in `commercial/docs/RFC-25-multi-runtime-unlock.md`.


## [1.19.0] - 2026-06-16 — Memory Intelligence: Temporal Memory + Reflexion Loop + Batch Fetch

Three W18/W19-designed memory features, implemented non-invasively on the live
Rust `SqliteMemoryEngine` (the original PostgreSQL/Python designs were updated to
the actual SQLite architecture). No new infrastructure.

### Added

- **F1 Temporal Memory** (`duduclaw-memory`) — `memories` gains temporal /
  knowledge-graph columns (`valid_from`, `valid_until`, `superseded_by`,
  `supersedes`, `subject`, `predicate`, `object`, `confidence`, `metadata`) via
  the existing idempotent migration loop, plus two indexes. New
  `store_temporal(entry, TemporalMeta)` performs automatic conflict resolution:
  writing the same `(agent, subject, predicate)` supersedes the prior fact
  (closes its `valid_until`, links the supersession chain). `search()` /
  `search_layer()` now return only currently-valid memories by default;
  `get_history()` / `get_at()` expose the chain and point-in-time lookups.
  `MemoryEntry` is **unchanged** (zero blast radius across 22+ construction sites).
- **F2a Reflexion recall** (`duduclaw-gateway/channel_reply`) — an agent's recent
  unresolved `MistakeNotebook` entries are now injected into the **answering
  prompt** (`## Past Mistakes to Avoid`), not just the GVU SOUL.md generator —
  delivering cross-task learning. CJK-safe (topic match with recency fallback).
- **F2b Reflexion consolidation** (`duduclaw-gateway/reflexion`) — when the same
  `MistakeCategory` accumulates ≥3 unresolved mistakes, they are distilled into a
  single **semantic** memory rule (via F1 supersession) and the source mistakes
  are marked resolved. Runs detached so it never delays replies. Deterministic
  synthesis (zero LLM cost).
- **F3 `memory_fetch_batch`** — new MCP tool + `SqliteMemoryEngine::get_by_ids`
  fetch up to 100 entries by ID in one call (`Scope::MemoryRead`, namespace /
  ownership enforced, partial hits return `missing_ids` without error).

### Notes

- Trigger signal for reflexion is the existing `ErrorCategory` (Significant /
  Critical, MetaCognition-adaptive) — **not** the GVU Verifier (which validates
  SOUL.md proposals, not task quality). No hard-coded score threshold.
- The standalone Python MCP memory track remains superseded by the Rust CLI
  endpoints (not revived).


## [1.18.0] - 2026-06-15 — Dashboard budget/usage accuracy + reliability fixes

Dashboard now reads real spend from the persistent `CostTelemetry` ledger
instead of the `AccountRotator`'s in-memory counter (which reset on every
5-minute rebuild / gateway restart and stayed 0 for OAuth-subscription
accounts), plus a sweep of dashboard runtime-bug fixes.

### Added

- **`marketplace.install`** handler — installs a catalog MCP server into a
  chosen agent's `.mcp.json` (was previously an error stub). Frontend gains
  a target-agent picker dialog.
- **`system.version` returns `edition`** so the dashboard can gate Pro-only
  UI (auto-update toggle).
- **Voice & Proactive settings persistence** — `system.update_config` writes
  `[voice]` to `inference.toml`; per-agent `[proactive]` saved via
  `agents.update` and surfaced through `agents.inspect`.
- Language switcher + theme store wired into the dashboard Header.
- 88 missing i18n keys across `zh-TW` / `en` / `ja-JP`.

### Fixed

- **Budget/usage shows real data**: `agents.list` / `agents.inspect` report
  per-agent month-to-date spend (was one all-account aggregate shown
  identically for every agent); `accounts.budget_summary` reports the real
  global month-to-date total.
- **Cost unit correction**: `cost_millicents` holds whole cents — removed the
  erroneous `/10` in analytics savings/cost which under-reported by 10x.
- **Schedule (cron) add**: sends required `name` + `task` (was failing).
- **MCP servers** consumed as the array shape the backend serializes.
- Division-by-zero guard in budget progress bars; silent account errors now
  surface as toasts; assorted async-state and timer-cleanup fixes.

## [1.17.0] - 2026-06-10 — RFC-24 License v2.0 (Open Core foundation)

Bootstrap of the DuDuClaw Open Core commercial layer. Apache 2.0 remains
**fully usable with no limits and no commercial modules** — paid
subscription tiers unlock value-add modules under `commercial/*`
(premium templates, evolution params, enterprise dashboard, priority
security patches). No license installed → OpenSource tier (current
behavior, zero regression).

### Added

- **`duduclaw-license` crate** — verification-only client (signing key
  stays in `commercial/duduclaw-license`).
  - 7 tiers with inheritance chain: `OpenSource` / `Hobby` / `Solo` /
    `Studio` / `Business` / `SelfHostPro` / `Oem`.
  - Ed25519 trust registry seeded from `DUDUCLAW_LICENSE_PUBKEY_<ID>`
    env vars; empty registry collapses to OpenSource (fail-safe).
  - CRL polling for emergency revocations.
  - Machine binding via hostname + MAC fingerprint.
  - `~/.duduclaw/license.json` (`0o600`) storage.
  - `features.toml` v2 subscription matrix: Solo NT$990 / Studio
    NT$2,990 / Business NT$8,900 / Self-Host Pro NT$1,490·mo or
    NT$14,900·yr / OEM (Year 2+).
- **Gateway `license_runtime`** — bootstraps on `start_gateway`,
  phone-home loop, CRL poll, process-global `LicenseRuntime`. Every
  failure mode (missing file / empty key registry / signature mismatch
  / expired / grace exceeded) collapses to OpenSource — gateway never
  crashes on license errors.
- **`license.status` RPC** (manager-only) — returns `LicenseSnapshot`
  that intentionally **omits the raw Ed25519 signature and customer
  email** so it is safe to render in the dashboard.
- **`duduclaw license` CLI** — subcommands: `activate` / `status` /
  `refresh` / `export` / `import` / `deactivate` / `fingerprint`.
- **Web Dashboard `LicensePage`** — tier status, expiry warnings,
  unlocked modules grid, CTAs (renew / pricing / docs). i18n: 39 keys
  per locale (zh-TW / en / ja-JP).
- **README Hire me block** — Fiverr / LinkedIn / Portfolio links above
  the badges (GitHub long-URL SEO).
- **`scripts/dev-replace-binary.sh`** — local build → npm-installed
  binary replacement loop (rebuild dashboard + cargo `--release` +
  backup + install + smoke). Does NOT auto-restart side-effectful
  processes.
- **`.cargo/config.toml`** — pin `PYO3_PYTHON=python3.13` (BLK-7
  workaround; PyO3 0.24 max supported Python is 3.13, system default
  is 3.14).
- **Operational docs** — `marketing/blog/`, `marketing/press-kit/`,
  `wiki/eval/`, `wiki/impl/`, `wiki/reports/`, `wiki/sprint-reports/`,
  `docs/wiki/reports/`, `reports/tl-daily/`.

### Backward compatibility

- No license installed → OpenSource tier (current behavior, zero
  regression).
- All existing CLI / RPC / config surface unchanged.



## [1.16.0] - 2026-06-01 — MCP Refresh Tokens + GVU Consolidate Op

Two operationally-driven additions surfaced by the 12-day post-v1.15.2
production soak.

**Symptom 1**: agnes' Claude Desktop MCP server stopped working on
2026-05-30 15:59 UTC with `API key expired (31 days old, max 30)` and
sat broken for 2 days before being investigated. The 30-day rotation
policy was sound, but the user-experience around it was not — Claude
Desktop quietly disconnected and never retried, and there was no CLI
to rotate cleanly.

**Symptom 2**: agnes' SOUL.md has been growing at ~5 lines/week under
healthy GVU operation and is now at 132 lines / 7909 bytes — within the
150-line / 8 KB cap, but ~3 weeks from cap-rejection on current
trajectory. The structured patch path can only grow SOUL.md; it has
no shrink primitive.

### Added — MCP refresh tokens (Phase A)

- **New module `duduclaw-cli::mcp_refresh`** — SQLite-backed refresh
  tokens that supersede the 30-day legacy API keys with 90-day
  lifetime, per-token revocation, and a hash-only store (the raw
  token is never persisted).
  - Format: `ddc_refresh_<env>_<64hex>` (twice the entropy of legacy).
  - Storage: `~/.duduclaw/mcp_tokens.db` table `refresh_tokens` with
    columns `jti, token_hash, client_id, scopes, is_external,
    issued_at, expires_at, revoked_at`.
  - Validation: `authenticate_with_refresh_token` mirrors
    `mcp_auth::authenticate_with_key`'s error model so the existing
    dispatcher just needs prefix-based routing.

- **`mcp_auth::authenticate_from_env`** now prefix-routes credentials:
  values starting with `ddc_refresh_` go through the refresh-token
  validator, everything else through the legacy `ddc_<env>_<32hex>`
  validator. Both paths return the same `Principal` type so downstream
  code is unchanged. Legacy keys keep working — no migration required
  before refresh tokens are adopted.

- **New CLI subcommand `duduclaw mcp …`**:
  - `issue-refresh-token --env <prod|staging|dev> --client-id <id>
    --scopes <csv> [--external]` — generates a fresh token, persists
    its hash, prints the raw token ONCE.
  - `revoke-token <jti>` — soft-deletes a refresh token by its
    16-hex jti prefix.
  - `list-tokens` — table view of all tokens with status / remaining
    TTL / scopes.

### Added — GVU `SoulPatchOp::Consolidate` (Phase B)

- **New variant** in `crate::gvu::proposal::SoulPatchOp`.
  Semantically equivalent to `Replace` but with a hard size-shrink
  invariant: the patch is rejected if
  `content.trim().len() >= existing_body.trim().len()`. Used when
  SOUL.md is approaching the line/byte caps and the LLM is asked to
  merge redundant bullets or tighten verbose phrasing without
  changing behavior.

- **`apply_patch_to_soul` enforces the shrink invariant** before
  swapping. A `Consolidate` whose content does not shrink the section
  is rejected with `"Consolidate must shrink the section — new
  content is N bytes but existing body is M bytes"`. Empty existing
  body is also rejected (cannot consolidate nothing).

- **Generator prompt updated** with the `consolidate` op semantics so
  the LLM can self-trigger it when it sees SOUL.md approaching cap.
  Prompt change is additive — existing flows that don't need to
  consolidate are unaffected.

### Fixed

- **`mcp_auth` test suite was a time-bomb**. Six test fixtures used
  the hardcoded `created_at = "2026-04-29T00:00:00Z"`. On 2026-06-01
  (33 days later) every test expecting `Ok(Principal)` started
  failing with `KeyExpired { days_old: 33 }`. Replaced with
  `Utc::now().to_rfc3339()` so the suite stays robust to time. Five
  similar fixtures in `mcp_auth_strategy` already had this issue
  (5/30 onward) and were similarly fixed.

### Test coverage

12 new unit tests, total workspace **1537 passing**:

- `mcp_refresh::tests` ×8 — roundtrip, format rejection, unknown,
  revoke, expired, list ordering, jti determinism, env-label
  rejection at issue.
- `soul_patch_tests::consolidate_*` ×4 — shrinks existing,
  rejects-when-grows, rejects unknown section, rejects empty body.

### Operator action items

**Migrating Claude Desktop to a refresh token**:

```
duduclaw mcp issue-refresh-token \
    --env dev \
    --client-id claude-desktop \
    --scopes memory:read,memory:write,wiki:read,wiki:write,messaging:send
```

Paste the printed token into `~/Library/Application Support/Claude/claude_desktop_config.json`
under `mcpServers.duduclaw.env.DUDUCLAW_MCP_API_KEY`, then Quit and
relaunch Claude Desktop. After verifying the new token works, revoke
the old legacy key by removing it from `~/.duduclaw/config.toml` (or
running `duduclaw mcp revoke-token <jti>` if it was already a
refresh token).

**Legacy keys remain supported indefinitely** — refresh tokens are
strictly opt-in. Operators who prefer the file-based registry can
continue using `[mcp_keys]` entries.


## [1.15.2] - 2026-05-20 — agent_update_soul Audit + Drift Detection

Follow-up to v1.15.1. Investigating an unexpected 11-line SOUL.md growth
during agnes' 24h observation period revealed three pre-existing security
gaps in the `agent_update_soul` MCP backdoor that long predated v1.15.1 but
became visible thanks to the structured-patch path making routine GVU
writes traceable by contrast.

### Fixed

- **`agent_update_soul` now refreshes the `soul_guard` integrity hash**
  via `accept_soul_change` on every successful write. Before this fix the
  stored fingerprint was never updated, so legitimate calls left permanent
  drift that `check_soul_integrity` would (eventually, if a human invoked
  it) flag as tampering. agnes' 2026-05-19 02:27Z self-modification was
  the canonical observation.

- **`agent_update_soul` now writes to `tool_calls.jsonl`** for every
  invocation — success path with hash prefix + size, and four distinct
  rejection paths (invalid agent_id / empty content / nonexistent agent /
  tmp-write / rename failures). The trusted MCP backdoor was previously
  invisible to post-hoc audit; `tool_calls.jsonl` had no `agent_update_soul`
  entries between 2026-04-22 and today despite the tool being exercised at
  least once on 2026-05-19.

- **Heartbeat now runs `soul_guard::check_soul_integrity` per agent per
  tick** via the new `check_soul_integrity_with_audit` helper.
  Out-of-band SOUL.md modifications (whether legitimate-but-unaudited or
  malicious) now produce a `WARN` log and a `_soul_integrity_drift`
  synthetic audit row within one heartbeat interval (default 1 h). Prior
  to this fix the integrity check had exactly one caller — the manual
  `duduclaw test <agent>` CLI red-team — so drift sat silently until an
  operator chose to investigate. Agents without a `SOUL.md` are silently
  skipped (stub-agent configuration is documented and supported).

### Why this is separate from v1.15.1

These three gaps existed before v1.15.1 — the bloat-fix surfaced them but
did not introduce them. They are filed as a separate patch release
because their root cause (the `agent_update_soul` MCP tool bypassing the
GVU safety stack) is structurally orthogonal to the GVU verifier path and
deserves its own audit narrative.

### Test coverage

9 new tests, total workspace 1525 unit tests passing:

- `mcp::wiki_namespace_tests::agent_update_soul_refreshes_soul_guard_hash`
- `mcp::wiki_namespace_tests::agent_update_soul_appends_audit_row`
- `mcp::wiki_namespace_tests::agent_update_soul_audits_validation_rejections`
- `heartbeat::tests::soul_integrity_check_skips_agent_without_soul`
- `heartbeat::tests::soul_integrity_check_clean_when_hash_matches`
- `heartbeat::tests::soul_integrity_check_emits_audit_on_drift`

### Operator action items

After upgrading to 1.15.2 you may see `_soul_integrity_drift` audit rows
in `tool_calls.jsonl` for agents whose SOUL.md was last modified by the
pre-1.15.2 `agent_update_soul` (which never updated the hash). The drift
is real — the stored hash genuinely doesn't match the file — but it's a
historical artefact, not active tampering.

To clear the false-positive baseline, delete the stored hash so the next
integrity check re-fingerprints the current file as the new baseline:

```
rm ~/.duduclaw/soul_hashes/<agent>.hash
```

`check_soul_integrity` treats a missing hash as "first run" and stores
the current SHA-256 automatically. Subsequent out-of-band modifications
will then flag genuine drift.


## [1.15.1] - 2026-05-18 — SOUL.md Bloat Containment + Structured Patch Path

Customer-reported regression: a production COO bot (`agnes`) had its
`SOUL.md` balloon from 61 to 592 lines over 5 GVU cycles, with 88% of the
file being accumulated proposal-meta narrative (`## 診斷` / `## rationale` /
`## expected_improvement` / `## wiki_proposals`). Each subsequent cycle saw
the bloated file, generated another correction, and the updater appended it
verbatim — an infinite feedback loop that bypassed every safety check by
progressively expanding the baseline so ASI's content-weighted threshold
stayed permanently satisfied.

This release fixes the failure mode in three layers and ships an unrelated
MCP server stdout-pollution bug fix.

### Fixed

- **`Updater::apply` no longer appends LLM proposal-meta narrative.**
  - New `strip_proposal_meta()` drops `## 診斷` / `## Analysis` /
    `## rationale` / `## expected_improvement` / `## wiki_proposals` /
    `## proposed_changes` headers and their bodies before the legacy append.
  - New `SOUL_MAX_LINES = 150` and `SOUL_MAX_BYTES = 8 KB` hard caps reject
    any proposal that would push SOUL.md beyond either limit, independent of
    ASI (which becomes permissive on a growing baseline because
    `for_baseline_size` weakens the threshold proportionally).
  - The legacy "always append, never replace" safety justification is
    preserved — the strip+cap layer makes append bounded instead of unbounded.

- **L1 verifier now simulates the real apply path.** When
  `proposal.patch.is_some()`, `verify_deterministic` calls
  `apply_patch_to_soul` to compute the true post-apply SOUL.md and runs
  must_always / must_not / size / sensitive-pattern checks against it,
  instead of the legacy `current + content` fake append. Without this fix
  the structured-patch path was DOA: `proposal.content` becomes a human
  summary like "Add refusal rule" and the must_always pattern (a chunk of
  contract text) was never found inside it — observed on agnes 2026-05-18
  where 3 generations rejected for the same phantom must_always failure
  despite the LLM's `soul_patch` JSON containing exactly the required text.

- **`Generator::parse_response` extracts JSON from markdown code fences.**
  LLMs commonly wrap structured output in ` ```json ... ``` ` fences with
  surrounding narrative (e.g. "根據分析... ```json\n{...}\n``` ...核心邏輯...").
  The parser previously failed pure-JSON parsing on the fence, fell back to
  section extraction, dropped the `soul_patch` field, and silently
  downgraded to the legacy strip+cap append. Reuses `verifier::strip_json_fences`
  for consistency with `parse_judge_response`.

- **MCP server stdout pollution.** `tracing_subscriber::fmt::layer()` now
  routes to stderr instead of stdout. Claude Desktop's MCP stdio transport
  parses stdout as JSON-RPC 2.0; the previous default tracing destination
  corrupted every session with `Unexpected token '', "[2m2026-0..." is not
  valid JSON` errors. The downstream `cmd_mcp_server` re-init via
  `try_init` silently no-opped once the global subscriber was already
  installed. Independent of GVU — affects every MCP client.

### Added

- **`SoulPatch { section, op, content }` structured edit type** with four
  ops — `Replace`, `AppendWithin`, `PrependWithin`, `AddSection`. Located in
  `crates/duduclaw-gateway/src/gvu/proposal.rs`. Optional `patch:
  Option<SoulPatch>` field on `EvolutionProposal` so on-disk proposals
  deserialize unchanged.

- **`apply_patch_to_soul(current, patch) -> Result<String, String>`** in
  `gvu/updater.rs` — locates the target `## <title>` header, edits the
  section body in place per the op, reassembles SOUL.md. Section names
  containing newlines or `##` tokens are rejected (prompt-injection
  defence); patch.content is capped at 4 KB per edit.

- **Generator prompt asks LLM to emit a `soul_patch` field.** Schema with
  op semantics, hard rules forbidding `[保留現有內容]` placeholders and
  whole-file rewrites, plus a concrete example. Prompt length grew from
  ~22 KB to ~24.5 KB (≈11% per GVU run).

### Production validation

Run on `agnes` 2026-05-18 09:53Z → 10:42Z, four iterations against the same
Round 1-4 conversation script (boundary probe → off-scope medical question
→ negative feedback). Final state: SOUL.md grew 61 → 85 lines with one
cleanly-added new section, zero meta-narrative residue, zero duplicate
headers, zero embedded JSON. GVU `outcome=applied`, generation 1, 60.2s
duration, ASI=0.679 (warning, not critical), L1+L3 verifier approved.

### Test coverage

106 GVU unit tests, +22 new:
- `proposal_meta_stripper_tests` — Chinese + English meta sections, blank-line
  collapsing, case-insensitive headers, cap sanity.
- `updater_apply_caps_tests` — meta-only rejection, line-cap overshoot
  rejection, clean-proposal application.
- `soul_patch_tests` — all four ops, section-not-found, prompt-injection
  guards, oversized content rejection, replace-then-replace idempotence.
- `soul_patch_apply_e2e_tests` — end-to-end through `Updater::apply` with
  `proposal.patch = Some(...)`.
- `generator_tests` — fence-stripped JSON parse, missing-patch fallback,
  prompt-includes-schema regression guard.
- Patch-aware L1 verifier — append-patch satisfies must_always check,
  rejection when pattern truly missing, invalid-section gives clear error,
  must_not check uses patch content not human summary.

### Backward compatibility

- `proposal.patch` is `Option`, `#[serde(default, skip_serializing_if =
  "Option::is_none")]` — proposals serialized before this release
  deserialize unchanged.
- `Updater::apply` falls back to strip+cap legacy append when `patch` is
  `None`. LLMs that haven't adopted the new schema continue to work, just
  with the bounded-growth safety net instead of unbounded append.
- No `CONTRACT.toml` or `agent.toml` schema changes.
- No migration required. Existing SOUL.md files already polluted by prior
  bloat are not auto-cleaned — operators should hand-truncate or wait for
  ObservationFinalizer's rollback path to fire on metric regression.


## [1.15.0] - 2026-05-17 — Cross-Platform PTY Pool + Worker

Anthropic blocked `claude -p` for OAuth-subscription accounts in mid-2026 and
recommended driving the real interactive `claude` REPL instead. v1.15.0 ships
the runtime to do exactly that: long-lived `claude` sessions driven through a
real PTY (ConPTY on Win 10 1809+, openpty on Unix) with a sentinel-framed
in-band response protocol — no scrollback scraping, no sidecar.

Default **OFF**; per-agent opt-in via `agent.toml [runtime] pty_pool_enabled =
true`. The existing fresh-spawn `claude -p` path is preserved for API-key
accounts and remains the global default. Every PTY path falls back to legacy
`tokio::process::Command + claude -p` on error, so a missing worker /
unhealthy pool / spawn failure is recoverable, not fatal.

### Added

- **New crate `duduclaw-cli-runtime`** — cross-platform PTY runtime built on
  [`portable-pty`](https://crates.io/crates/portable-pty).
  - `PtySession` lifecycle + `SpawnOpts` per-CLI configuration
    (`CliKind::Claude / Codex / Gemini`).
  - `PtyPool` with per-agent semaphore, idle eviction, supervisor + restart
    policy. Cache-hit / spawn / 3 eviction-reason counters surface to gateway.
  - `Envelope` / `Frame` / sentinel constants + ANSI-stripping
    `extract_payload_with_chrome_filter`.
  - `oneshot_pty_invoke` for the `claude -p` PTY-wrapped fallback (API-key
    accounts).
- **New crate `duduclaw-cli-worker`** — standalone worker binary wrapping the
  pool over a localhost HTTP+JSON-RPC API.
  - Bearer-token auth via `DUDUCLAW_WORKER_TOKEN` env var + on-disk
    `TokenStore` (`ring::SecureRandom`).
  - Endpoints: `POST /rpc` (`invoke` / `shutdown_session` / `stats`),
    `GET /healthz` (no auth).
  - Library re-export so the gateway shares the protocol types directly.
- **Gateway integration**:
  - `crates/duduclaw-gateway/src/pty_runtime.rs` — adapter owning the global
    `PtyPool`, `RuntimeMode::{FreshSpawn, PtyPool}` per-agent routing,
    `acquire_and_invoke` + `acquire_and_invoke_with` public surface,
    optional `MANAGED_WORKER` `WorkerClient` for Phase 7.
  - `crates/duduclaw-gateway/src/worker_supervisor.rs` — Phase 7 supervisor
    for the out-of-process worker. Resolves binary, spawns with loopback
    bind + token + `--home-dir`, polls `/healthz` until ready, runs a 30s
    health-check loop with N-strike restart, and **sequences
    SIGTERM/SIGKILL into the gateway graceful-shutdown future** (after
    prediction-engine flush, before axum drains) instead of racing it from
    a detached task (Round 2 review HIGH-4).
  - `crates/duduclaw-gateway/src/runtime_status.rs` — Phase 8.5 JSON status
    endpoint `GET /api/runtime/status` (loopback-only, no auth) with
    transport + kill-switch + session / invoke / worker stats.
  - `crates/duduclaw-gateway/src/channel_reply.rs` —
    `call_claude_cli_pty_rotated` PTY-routed mirror of
    `call_claude_cli_rotated`; OAuth → interactive REPL, API-key →
    `oneshot_pty_invoke + claude -p`. `parse_claude_stream_json_complete`
    is a buffer-based mirror of the streaming parser;
    `StreamDiagnostics` is embedded in error messages so
    `channel_failures.jsonl` post-mortem identifies what went wrong inside
    the PTY response (`exit / lines / events / assistant / text_blocks /
    thinking / tool_use / result_subtype / stop_reason / last_line /
    stderr_tail`).
  - `crates/duduclaw-gateway/src/claude_runner.rs` — dispatcher-side
    short-circuit: when `pty_pool_enabled = true`, sub-agent invocations
    skip local-offload + hybrid routing and go straight through the pool
    (channel reply + dispatcher consistent).
- **Phase 8 production observability** (`crates/duduclaw-gateway/src/metrics.rs`):
  - `pty_pool_acquires_total` + `pty_pool_acquires_cache_hit_total` +
    `pty_pool_acquires_spawn_total`.
  - `pty_pool_evicted_idle_total` + `pty_pool_evicted_unhealthy_total` +
    `pty_pool_evicted_shutdown_total`.
  - `pty_pool_invokes_ok_total` + `pty_pool_invokes_empty_total` +
    `pty_pool_invokes_error_total` + `pty_pool_invokes_timeout_total`.
  - `pty_pool_invoke_duration_buckets[8]` (shared bounds with the main
    request histogram) + `pty_pool_invoke_duration_sum_ms`.
  - `worker_health_misses_total` + `worker_restarts_total`.
  - `pty_pool_managed_worker_active` gauge (0 = in-process, 1 = managed).
- **Smoke harness**:
  - `scripts/smoke-pty-pool.sh` (Unix/macOS) — build cli-runtime + spike
    example + run cli-runtime / gateway `pty_runtime::` /
    `channel_reply::routing_helper_tests` / `stream_json_parser_tests`.
    `CLAUDE_SPIKE=1` runs the live interactive spike (consumes OAuth quota).
  - `scripts/smoke-pty-pool.ps1` — Windows equivalent.

### Operational notes

- **Kill switches**:
  - Per-agent: `agent.toml [runtime] pty_pool_enabled = false` (default).
  - Global: env-var kill switch disables PTY routing without rolling back.
- **Out-of-process mode** (`[runtime] worker_managed = true` in
  `<home>/config.toml`) promotes the in-process pool to the
  `duduclaw-cli-worker` subprocess. The supervisor is best-effort: spawn
  failure leaves the gateway in in-process mode with a warn log.
- **Cross-platform**:
  - Windows: `windows` crate Job Objects for child-process containment
    (Win10 1809+).
  - Unix: `nix` for signal + process-group control.
- **References**: [`dorkitude/maude`](https://github.com/dorkitude/maude)
  (Unix-only tmux shim that inspired the interactive driving idea) and
  [`runtorque/torque`](https://github.com/runtorque/torque) (Unix-only
  PTY + UDS frame protocol). `portable-pty` is what makes one code path
  span mac/Linux/Windows.

### Design docs

- `commercial/docs/runtime-pty-pool-design.md` — full architecture, kill
  switches, security stance.
- `commercial/docs/TODO-cli-pty-pool-worker.md` — phase-by-phase TODO with
  verify steps.


## [1.14.0] - 2026-05-14 — RFC-23 Redaction Pipeline

新增獨立 crate `duduclaw-redaction` 與 gateway 整合層，預設**未啟用**。

### Added

- **New crate `duduclaw-redaction`** — source-aware redaction +
  reversible restoration. Internal data (Odoo / shared wiki / file tools)
  is replaced with `<REDACT:CATEGORY:hash8>` tokens before the LLM sees
  it; tokens are restored at trusted boundaries (user channel reply,
  whitelisted tool egress).
- **Encrypted SQLite vault** at `~/.duduclaw/redaction/vault.db` using
  AES-256-GCM (reused from `duduclaw-security`), with per-agent 32-byte
  keys (`0o600` permission), TTL 7d default, two-stage GC (mark expired
  → purge after 30d).
- **Five built-in profiles** embedded in the binary: `general`,
  `taiwan_strict`, `taiwan_minimal`, `financial`, `developer`. Selected
  via `[redaction] profiles = [...]`.
- **Five-layer enable/disable resolver** (`compute_effective_enabled`):
  channel `force_on` (banked) → env + CLI flag emergency override → env
  alone → CLI flag → agent.toml → config.toml. Full truth-table coverage.
- **Channel `force_on` lock** with audited `--force-disable-redaction`
  emergency break-glass; persistent override-flag file
  (`~/.duduclaw/redaction/override.flag`) and CRITICAL audit per affected
  channel.
- **Tool egress whitelist** with default deny. Whitelisted tools can
  `restore_args = true` (real values), `passthrough` (keep tokens), or
  `deny`. Hallucinated tokens always result in deny.
- **JSONL audit sink** at `~/.duduclaw/redaction/audit.jsonl` with 10MB
  rotation; events: `redact / restore_ok / restore_denied / restore_miss
  / egress_allow / egress_deny / vault_gc / force_on_override`.
- **Background GC tokio task** running `mark_expired` every 6h and
  `purge_expired` every 24h, with graceful cancel.
- **Dashboard read-only RPCs**: `redaction.stats`,
  `redaction.recent_audit`, `redaction.override_status`,
  `redaction.policy_status`.
- **Gateway integration shim** at `crates/duduclaw-gateway/src/redaction_integration.rs`
  providing `build_manager_from_home()`,
  `compute_effective_for_channel()`, `cli_flag_from_env()`, and
  `force_disable_active()`.
- **Full gateway wiring**:
  - `MethodHandler` carries `Option<Arc<RedactionManager>>` + setter +
    4 `redaction.*` Dashboard RPC handlers (`stats`, `recent_audit`,
    `override_status`, `policy_status`).
  - `start_gateway()` parses `[redaction]` from `config.toml`, builds the
    manager, spawns the 6h-mark/24h-purge GC task, and injects the
    manager into `MethodHandler` and `ReplyContext`.
  - `build_reply_with_session` / `build_reply_for_agent` apply
    `restore` at the public-API exit so the user channel sees real
    values while LLM-bound text retains tokens.
- **MCP-layer integration** (`crates/duduclaw-cli/src/mcp_redaction.rs`):
  - `McpRedactionLayer` reads `DUDUCLAW_AGENT_ID` + `DUDUCLAW_SESSION_ID`
    env vars (set by gateway when spawning the Claude CLI subprocess).
  - On every `tools/call`: pre-check tool args for `<REDACT:...>` tokens
    and run the egress evaluator (whitelisted → restore; otherwise →
    JSON-RPC error). Post-process the tool result Value by walking every
    string leaf through `RedactionPipeline.redact` so the LLM never sees
    raw internal data.
- **CLI flags**: global `--redact=on/off` (overrides agent/global config
  but not channel `force_on`) and `--force-disable-redaction` (requires
  `DUDUCLAW_REDACTION=off`, writes a persistent override flag + CRITICAL
  audit + dashboard red banner).
- **RFC-23** at `commercial/docs/RFC-23-redaction-pipeline.md` + detailed
  per-phase TODO at `commercial/docs/TODO-redaction-pipeline.md` +
  operator guide at `commercial/docs/redaction-operator-guide.md`.

### Tests

- 98 unit tests + 11 end-to-end integration tests in
  `crates/duduclaw-redaction/`, covering: token format & HMAC salt
  derivation; rule compile + ReDoS-surface limits; vault round trip
  (encrypt blob never contains plaintext); cross-session and cross-agent
  isolation; per-rule cross-session-stable override; TTL → expired
  marker → 30-day purge; reveal counter bookkeeping; egress decisions
  (allow/passthrough/deny + nested JSON + hallucinated tokens); profile
  merge with id collision; five-layer toggle truth table with channel
  force_on priority; force-override flag persistence + banner; GC task
  mark+stop cycle.

### Default behaviour

`config.toml [redaction] enabled = false` — existing deployments are
unaffected unless operators explicitly opt in. See
[`commercial/docs/redaction-operator-guide.md`](commercial/docs/redaction-operator-guide.md)
for the five-step adoption recipe.


## [1.13.2] - 2026-05-12

Bug fix for fresh-install clients that have never run the CLI keyfile
init flow.

### Fixed

- **Dashboard credential save no longer fails with "Encryption
  unavailable" on a fresh install.** `encrypt_value()` now calls a new
  `load_or_create_keyfile()` helper that auto-generates the 32-byte
  AES-256 keyfile (`~/.duduclaw/.keyfile`, owner-only permissions) the
  first time the gateway is asked to encrypt a credential. Previously
  the helper was read-only and any client that hit the dashboard
  without first running `duduclaw init` would see the Odoo / channel
  token / API key save fail with a misleading "Ensure keyfile exists"
  message. The decrypt path stays read-only by design so a missing
  keyfile never silently destroys an existing ciphertext.
  (`crates/duduclaw-gateway/src/config_crypto.rs`)
- **Better error messages on the rare encryption failures that remain.**
  The Odoo configure handler now distinguishes the new failure modes
  (RNG / disk write) from the old "keyfile missing" case and points
  operators at the gateway log instead of telling them to fix a file
  the gateway is now able to create itself.
  (`crates/duduclaw-gateway/src/handlers.rs`)

### Tests

- 7 new unit tests covering: keyfile auto-creation, encrypt→decrypt
  round trip after auto-create, rejection of empty plaintext (does not
  pollute the home dir), keyfile stability across successive encrypts,
  decrypt-side read-only invariant, and `mkdir -p` of a fully absent
  home directory.


## [1.13.1] - 2026-05-12

Dashboard UX fix for the Odoo connection page.

### Changed

- **`odoo.test` RPC now accepts inline params** — when the dashboard
  sends `{ url, db, protocol, auth_method, username, api_key?, password? }`,
  the connector is built from those values without writing to
  `config.toml`, so users can verify credentials before persisting. When
  the credential field is empty in inline mode, the handler falls back
  to the stored encrypted secret so a small URL tweak does not require
  retyping the API key. Calling `odoo.test` with no params preserves the
  original "test the saved config" behaviour.
  (`crates/duduclaw-gateway/src/handlers.rs`)
- The Test Connection button on the Odoo page now uses the form's live
  values instead of requiring a prior save. The button is gated on
  url + db being present.
  (`web/src/pages/OdooPage.tsx`)
- `handleSave` / `handleTest` surface the real backend error string
  instead of swallowing it — the previous generic "save failed" /
  "Odoo not configured" messages were undiagnosable from the UI alone.

### Security

- Inline-mode params go through the same SSRF / HTTPS / db-name
  validators as `odoo.configure`. The test path cannot be used to
  bypass safety rules.
- New `scrub_odoo_error()` caps connector failure text at 240 chars
  before forwarding to the dashboard so HTML error pages or full URLs
  with query strings are not leaked.

### Tests

- 16 new unit tests covering happy path, every validation branch, the
  `fc00.*` hostname regression (not an IPv6 ULA), credential fallback,
  and the error-scrubber.


## [1.13.0] - 2026-05-12

Runtime-health overhaul covering 16 issues across two rounds. Round 1
restores GVU/SOUL self-evolution (was effectively dead since 5/3); Round 2
introduces architectural fixes for the cron-driven 200 K token cliff.

See `commercial/docs/TODO-runtime-health-fixes-202605.md` for the
issue-by-issue audit log with verification evidence.

### Added

- **`[prompt] mode = "minimal"` agent config** — opt-in Anthropic
  Skills-style system prompt: SOUL core (≤ 5 KB) + identity + contract +
  MCP tool index. Wiki / skill content fetched on demand instead of
  inlined upfront. Stable prefix → near-perfect prompt-cache hit.
  Expected cliff reduction: 75% on knowledge-rich agents.
  (`crates/duduclaw-gateway/src/prompt_minimal.rs`)
- **`[budget] max_input_tokens` enforcement** — when set, an agent's
  request goes through a compression pipeline (Hermes trim → drop oldest
  tool echoes → bisect-and-summarize) before send. `cost_pressure` flag
  from §6.3 tightens thresholds automatically. Non-fatal: falls back to
  full history on pipeline failure.
  (`crates/duduclaw-gateway/src/prompt_compression.rs`)
- **`[prompt] cli_bare_mode = true` agent config** — when set, the agent's
  Claude CLI subprocesses launch with `--bare`, suppressing the
  CLAUDE.md auto-discovery leak documented in the spike (see
  TODO #15). Requires an API-key account in the rotator; OAuth accounts
  are skipped with a warn.
  (`crates/duduclaw-gateway/src/claude_runner.rs` `BARE_MODE` task-local)
- **Async session summarizer** — background task (10-min cadence) folds
  older session turns into Haiku-generated bullet summaries. Stored in
  three new columns on `sessions` (`summary_of_prior`,
  `summarized_through_turn`, `last_summarized_at`). `channel_reply`
  prepends the summary as a synthetic assistant recap turn.
  (`crates/duduclaw-gateway/src/session_summarizer*.rs`)
- **TF-IDF wiki relevance ranking** — wiki injection now ranks L0/L1
  pages by user-message relevance (char-bigram TF-IDF, CJK-safe) before
  hitting the 6 KB cap. Auto-enabled, no config required; empty query
  preserves file order for back-compat.
  (`crates/duduclaw-gateway/src/relevance_ranker.rs`,
   `crates/duduclaw-gateway/src/ranked_wiki_injection.rs`)
- **`duduclaw lifecycle flush` CLI** — quarterly cold/hot separation of
  wiki pages. Uses file mtime as access proxy (real counter deferred).
  `--dry-run` by default; pass `--apply` to commit moves to
  `wiki/.archive/`.
  (`crates/duduclaw-gateway/src/lifecycle_flush.rs`)
- **GVU trigger module** — sub-agent dispatches now fire GVU via the
  same path as channel-facing root agents. Previously only `agnes` ever
  evolved; now `duduclaw-tl` etc. can too.
  (`crates/duduclaw-gateway/src/gvu/trigger.rs`)
- **`prompt_audit` observability** — per-section byte-count breakdown
  emitted as `INFO target=prompt_section_audit` when total exceeds 50 KB.
  Surfaces *which* section bloated, not just that total was high.

### Fixed

- **`log_level` config now resolves correctly** — three-tier
  `RUST_LOG → config.toml [general] log_level → "warn"` instead of the
  previous hard-coded `"warn"` fallback. Restores visibility of
  `Heartbeat firing`, `forced_reflection`, `SilenceBreaker consumer
  started`, and other INFO-level diagnostics that were silently dropped.
- **L1 generator `must_always` injection** — Generator now receives the
  contract's `must_always` patterns and emits a `<must_include>` block
  flagging any pattern absent from current SOUL. Unblocks the
  5/3-onwards deferred loop on agnes where every generation failed the
  same L1 check.
- **L1 `must_not` catch-22** — now checks `proposal.content` instead of
  `simulated_final`. Previously, agents that mirrored a `must_not` rule
  into SOUL.md as a self-reminder would have every subsequent proposal
  rejected because the rule statement was in `current_soul`.
- **Discord token-check backoff** — exponential 60 → 120 → 240 → 480 → 900
  seconds (capped 15 min) instead of flat 60 s; respects `Retry-After`
  header. Adds 24 h sliding-window storm detector that emits a
  `discord_invalid_session_storm` security audit event after 5 events.
- **GVU `Skipped` log level** — `debug!` → `info!` so trigger-fired-then-silent
  scenarios (e.g. agent in observation window) are debuggable without
  enabling debug logging.
- **`ObservationFinalizer` 72 h no-traffic cap** — sub-agents without
  channel traffic no longer sit in `observing` forever. After 72 h with
  conversations < 5, auto-confirm so the next GVU can proceed.
- **`skill_loader` recursive scan** — supports the official Anthropic
  Skills `<skill>/SKILL.md` layout (case-insensitive) alongside the
  legacy flat `<name>.md` form. Nested `references/*.md` correctly
  treated as supporting material, not separate skills. Symlink
  containment, hidden-entry skip, 8-level depth cap.
- **`skill_synthesis` pipeline tools** — added regression-guard tests
  ensuring all four pipeline tools (`memory_episodic_pressure`,
  `skill_synthesis_status`, `skill_synthesis_run`, `activity_post`) are
  visible to internal principals. Root cause of the 5/7 incident was a
  stale gateway binary, not missing implementation.

### Stats

- 1264 → 1390 tests green (+126 new unit tests)
- 9 new modules in `duduclaw-gateway`
- 31 files changed, +5790 / −164



## [1.12.3] - 2026-05-08

Hot-fix on top of v1.12.2 — Dashboard 編輯 agent 時 evolution 與 sticker
欄位顯示為預設值而非 agent.toml 真實值。

### Fixed

- **`agents.list` response 漏 `evolution` / `sticker` 區段**
  - Symptom: 在 Dashboard 把 agent 的 `skill_auto_activate` 從 false 改 true
    並儲存，response 回 `success: true` / `hot_reloaded: true`，`agent.toml`
    也確實寫入 `skill_auto_activate = true`；但重新打開 agent 編輯框仍
    顯示 false
  - Root cause: `handle_agents_list_filtered` 回傳 JSON 沒有 `evolution`
    與 `sticker` 兩個區段（只有 `agents.inspect` 有）。前端
    `EditAgentDialog` 從 list response 初始化表單，
    `agent.evolution?.skill_auto_activate ?? false` 因 `agent.evolution`
    為 `undefined` 永遠 fallback 到 `false`
  - 其他 3 個 evolution 欄位（`gvu_enabled` / `cognitive_memory` /
    `skill_security_scan`）剛好預設 `?? true` 對齊大多 agent.toml 真實值，
    使用者沒察覺；只有 `skill_auto_activate` 預設 `?? false` 與真實值衝突，
    才把這個顯示 bug 暴露出來。Sticker 區段也有同樣問題
  - Fix: 把 `evolution` + `sticker` 區段補進 `agents.list` response，與
    `agents.inspect` 對齊



## [1.12.2] - 2026-05-07

Dashboard 死局與假性「設定無反應」修復。使用者回報 Dashboard 設定幾乎無法
操作、任務無法操作；Telegram 與 Odoo 路徑正常。深入追查後發現 4 個獨立
問題交互疊加，本版一次解決。

### Fixed

- **JWT auto-refresh 缺失導致 WebSocket 死循環**（CRITICAL）
  - Symptom: gateway log 連續 4000+ 次 `WebSocket auth failed – closing connection`，
    最後一次成功認證 2026-05-06T02:17:52，之後 dashboard 全面失效
  - Root cause: access token TTL 30 分鐘，前端只在 `loadFromStorage` 啟動時
    呼叫一次 `/api/refresh`，過期後 WS 持續用過期 token 重連被拒
  - Fix: `auth-store` 加 25 分鐘 setInterval + `visibilitychange` listener；
    `ws-client` 加 `authRefreshHook`，handshake 失敗訊息含 `jwt`/`auth` 時
    下次 `doConnect` 前先 await refresh

- **重整頁面看不到資料、需切走再切回**（HIGH）
  - Symptom: 頁面 reload 後資料空白；切換頁面再切回才正常
  - Root cause: React effects 由葉子向根 commit，page useEffect 比 App
    `connectWithAuth` 早跑；`waitForReady` 在 state=disconnected & 無
    reconnectTimer 時 fast-reject `"Not connected"`
  - Fix: `AuthGuard` 多 gate 一層 `wsState === 'authenticated'`，protected
    route 在 WS 就緒後才 mount

- **agents.update 寫入後 registry 沒立刻 reload**（MEDIUM）
  - Symptom: 修改 agent 設定後使用者誤以為沒生效
  - Root cause: `update_agent_toml` 拿 registry write lock 用 500ms timeout
    但 timeout 後 silent fail，agent.toml 已寫入但記憶體 registry 沒重載
  - Fix: 改回傳 `Result<bool, String>`（bool = hot_reloaded），timeout / scan
    失敗時 `warn!` 一行；`agents.update` response 加 `"hot_reloaded": bool`
    與對應 message

- **per-agent channel token 變更不會 hot-restart bot**（MEDIUM）
  - Symptom: 修改 Discord/Telegram per-agent token 後，下次發訊息仍走舊
    token，需重啟 gateway
  - Root cause: bot 啟動時 capture token，registry rescan 不會觸發 bot 重啟；
    只有 `channels.add` / `channels.remove` RPC 走 hot-restart 路徑
  - Fix: 新增 `hot_restart_agent_channels(channel_types, agent_name)` helper；
    `handle_agents_update` 偵測到 `discord_bot_token` / `telegram_bot_token`
    入參時，寫檔成功後自動 hot-restart 對應 bot；response 加
    `"channels_restarted": [...]`。LINE 是 webhook 不需處理；Slack / WhatsApp
    / Feishu 仍需 gateway 重啟（缺 hot-restart helper）

### Notes

- 升版後第一次開啟 dashboard 仍需清除瀏覽器 localStorage 的
  `duduclaw-refresh-token` 重新登入，才能拿到走新 auth flow 的 fresh JWT。
- Telegram / Odoo / channel_reply 路徑本來就 OK，不受本版影響。



## [1.12.0] - 2026-05-06

W22 Sprint deliverables — two W22-P0 ADRs ship together with a multi-agent
coordination overhaul (RFC-22) driven by a 2026-05-04 → 2026-05-06 端到端
incident that exposed agnes silently fabricating sub-agent replies, autopilot
mass-firing on malformed events, and channel-path token usage going entirely
unrecorded.

### Added

#### W22-P0 ADR-002 — `x-duduclaw` capability negotiation

Every HTTP response from the MCP HTTP server now carries machine-readable
capability metadata, and clients can declare capability requirements that
trigger an early 422 rather than silent partial failures.

- **`mcp_headers.rs`** — `CAPABILITY_REGISTRY` static table (9 capabilities:
  `memory/3`, `mcp/2`, `audit/2`, `governance/1`, `skill/1`, `wiki/1` enabled;
  `a2a/1`, `secret-manager/1`, `signed-card/1` disabled/pending).
  `API_VERSION = "1.2"`. Builder/parser/negotiation functions. 23+ unit tests.
- **`mcp_capability.rs`** — `inject_capability_headers` outer middleware
  (appends `x-duduclaw-version` + `x-duduclaw-capabilities` to every
  response) and `negotiate_capabilities` inner middleware (returns 422
  Unprocessable Entity when client requirements unmet, with structured
  JSON body + `x-duduclaw-missing-capabilities` header). Permissive when
  header absent/empty/malformed. 11 Axum integration tests.
- **`mcp_http_server.rs`** — Both layers wired into `build_router()` with
  correct outer/inner ordering. Adds 11 integration tests for healthz,
  unauthorized 401, malformed JSON-RPC, and capability negotiation 422.
- **`docs/ADR-002-x-duduclaw-capability-negotiation.md`** — Full ADR.

#### W22-P0 ADR-004 — Secret Manager

Unified abstraction over three backends behind a `secret://<backend>/<name>`
URI scheme so MCP clients (Brave Search, Figma, Notion) can reference
credentials without embedding them in code or env vars.

- **`crates/duduclaw-security/src/secret_manager/`** — new module:
  - `mod.rs` — `SecretAdapter` async trait, `SecretUri` parser, config
    loader (`[secret_manager]` in `config.toml`), `Backend::Local|Vault|Env`.
  - `local.rs` — In-process AES-256-GCM encrypted store (dev/testing).
  - `vault.rs` — HashiCorp Vault KV v2 HTTP client (production), reads
    `vault_addr`, `vault_token`/`vault_token_enc`, `vault_mount`.
  - `env.rs` — Reads from process environment (CI/override).
- 26 unit tests covering URI parsing, config parsing, encrypted at-rest
  verification, error variants, cross-backend round-trips.

#### RFC-22 — Multi-agent coordination principles

- **`docs/RFC-22-multi-agent-coordination-principles.md`** — Four design
  decisions: (1-C) Two-tier Task/Wiki, (2-C) Hybrid spawn+bus fallback,
  (3-D) Channel mapping, (4-D) Hallucination forbidden + audit trail.
- **`crates/duduclaw-core/src/types.rs`** — `ChannelBinding { kind, id,
  description }` + `DiscordChannelConfig.bindings: Vec<ChannelBinding>` so
  per-thread routing can target sub-agents directly.
- **`crates/duduclaw-agent/src/resolver.rs`** — `AgentResolver` step-2
  channel/thread binding match between trigger word and coarse permission
  grant. 8 new unit tests.
- **`crates/duduclaw-security/src/audit.rs`** — `append_tool_call_with_extras`
  helper for attaching wiki authorship audit fields
  (`claimed_authors_in_content`, `matches_caller`, `actual_caller`).
- **`crates/duduclaw-cli/src/mcp.rs`** — `detect_claimed_authors_in_wiki`
  parses `## <agent> 的觀點`, `**回覆人**：<agent>`, signature, and
  frontmatter `claimed_authors:` patterns. Recorded on every
  `shared_wiki_write`. 6 new unit tests.

### Changed

- `x-duduclaw-version` bumped to `1.2` (second backward-compatible HTTP API change).
- **`crates/duduclaw-gateway/src/autopilot_engine.rs`** — `lookup_path_opt`
  returns `Option<Value>` so missing fields no longer match `eq null`,
  fixing the 5/5 mass-fire bug where 5 task_created events all triggered
  Rule A. `apply_op` short-circuits `None` to `false`. 4 regression tests
  (P1-9b).
- **`crates/duduclaw-gateway/src/channel_reply.rs`** — `build_system_prompt`
  now injects `CONTRACT.toml` boundaries via `contract_to_prompt`
  (P1-8 / P1-9a). `spawn_claude_cli_with_env` parses the result event's
  `usage` field and records via `cost_telemetry` against a
  `CHANNEL_REPLY_AGENT_ID` task_local set in `build_reply_with_session_inner`
  — channel replies now produce token usage rows (P1-7).
- **`crates/duduclaw-gateway/src/claude_runner.rs`** — adds
  `CHANNEL_REPLY_AGENT_ID` task_local for per-agent cost attribution.
- **`crates/duduclaw-cli/src/mcp.rs`** — MCP server boot log now logs
  `caller_agent` alongside `client_id` so observers can distinguish API
  key owner from actual sub-agent (P1-10). `handle_spawn_agent` surfaces
  underlying I/O error when `bus_queue.jsonl` write fails, with RFC-22
  reminder not to fabricate a reply (W1).
- **`crates/duduclaw-cli/Cargo.toml`** — `default = ["dashboard"]` so
  `cargo build -p duduclaw-cli --release` produces a binary whose
  dashboard SPA fallback is mounted (without this every HTTP path except
  `/health` and `/ws` returned 404).

### Tests

  duduclaw-gateway: 838 passed (incl. 4 new autopilot regression tests)
  duduclaw-agent:    39 passed (incl. 8 new resolver binding tests)
  duduclaw-cli:     365 passed (incl. 6 new wiki author + 13 HTTP transport)
  duduclaw-core:     80 passed
  duduclaw-security: 179 passed (incl. 26 new secret_manager tests)

  Total **1501 / 1501 green** across all crates.

### Hygiene

- **`.gitignore`** — adds `*.profraw` (cargo test residue),
  `docs/{tl,pm}/daily-report-*.md` (agent operational logs belong on
  shared wiki), `/research/` (researcher agent local notes), `/python/spikes/`
  (active spike workspaces, promoted to production on completion), `/uv.lock`.

---

## [1.11.0] - 2026-05-04

RFC-21 — Identity Resolution & Per-Agent Credential Isolation. Closes
[#21](https://github.com/zhixuli0406/DuDuClaw/issues/21) by addressing all
three architectural gaps the reporter identified: identity resolution
walked the shared wiki instead of an authoritative external source, Odoo
MCP credentials shared one global admin slot across every agent, and the
shared wiki had no source-of-truth boundary so an evolving agent could
silently overwrite externally-synced data. All three are now enforced at
the system layer (dispatcher / pool / namespace policy) instead of relying
on SOUL.md prompt-layer self-restraint.

### Added — `duduclaw-identity` crate (§1)

- **`IdentityProvider` async trait** + `ResolvedPerson` (`person_id`,
  `display_name`, `roles`, `project_ids`, `emails`, `channel_handles`,
  `source`, `fetched_at`) + `ChannelKind` enum (Discord / Line / Telegram
  / Slack / WhatsApp / Feishu / WebChat / Email + `Other(_)` catch-all
  with stable wire format) + `IdentityError` (Unreachable / Malformed /
  Unsupported / Io / Internal).
- **`WikiCacheIdentityProvider`** reads `<home>/shared/wiki/identity/people/*.md`
  per-person YAML frontmatter records; tolerates malformed files and
  missing optional fields; mtime-driven `fetched_at`.
- **`NotionIdentityProvider`** queries Notion `databases/query` with
  configurable `NotionFieldMap` (property names + `ProjectsKind`
  multi_select / relation). HTTP errors classify cleanly: 5xx /
  network ⇒ Unreachable (chained provider degrades), 4xx ⇒ Malformed.
- **`ChainedProvider`** combines cache + upstream — cache hit
  short-circuits; cache miss falls through; upstream unreachable
  degrades to `Ok(None)` rather than hard-erroring; project membership
  prefers upstream then falls back to cache.
- **`identity_resolve` MCP tool** + new `Scope::IdentityRead`
  ("identity:read") gates the tool. Audit row emitted per call.
- **`<sender>` XML block auto-injection** into channel reply system
  prompt (`crates/duduclaw-gateway/src/channel_reply.rs`). Sender is
  resolved once per turn; XML-escaped to keep the envelope intact;
  optional fields omitted when empty. Empty result ⇒ block omitted ⇒
  v1.10.1 behaviour preserved.

### Added — Per-agent Odoo credential isolation (§2)

- **`agent.toml [odoo]` override block** parsed via new
  `duduclaw-odoo::AgentOdooConfig`: `profile` / `username` /
  `api_key_enc` / `password_enc` / `allowed_models` /
  `allowed_actions` / `company_ids`. Empty / malformed block returns
  None; agent without override falls back to global config.
- **`OdooConfigResolver`** layers global + per-agent; `pool_key_for`
  produces stable `(agent_id, profile)` pool keys.
- **`OdooConnectorPool`** (new `crates/duduclaw-cli/src/odoo_pool.rs`)
  replaces the v1.10.1 global `Arc<RwLock<Option<OdooConnector>>>` with
  a `(agent_id, profile)`-keyed pool. Outer `RwLock<HashMap>` for
  membership reads + per-slot `tokio::sync::Mutex` for first-use
  connect serialisation. `get_or_connect(decrypt)` → cached
  `Arc<OdooConnector>` or cold-connect via merged credentials.
  `set_global` preserves per-agent overrides on hot-reload;
  `disconnect`/`disconnect_all`/`is_connected` complete the lifecycle.
- **`Scope::OdooRead` / `OdooWrite` / `OdooExecute`** added to
  `mcp_auth.rs`. All 14 `odoo_*` tools registered into
  `tool_requires_scope` — read class (status / connect / search /
  CRM leads / sale orders / inventory / invoice / payment), write
  class (create lead / update stage / create quotation), execute class
  (sale confirm / generic execute / report).
- **`allowed_models` / `allowed_actions` defence-in-depth filter** —
  `check_action_permission(verb, model)` runs before any HTTP request
  leaves the process; supports bare verbs (`"read"` → all models) and
  qualified verbs (`"write:crm.lead"` → only crm.lead). Policy denials
  audited as DENIED rows.
- **Audit attribution**: `tool_calls.jsonl` rows for Odoo calls now
  carry `params_summary = "profile=<profile>; tool=<name>; ok=<bool>"`
  so Odoo activity is traceable to the originating agent rather than
  the shared admin user inside Odoo's own audit log.
- **`handle_odoo_connect`** now reload-and-reconnect: re-reads
  `config.toml [odoo]` (set as global), re-reads
  `agents/<caller>/agent.toml [odoo]` (registers as override),
  forces `disconnect(caller)`, then `get_or_connect`. The connection
  report includes the resolved `(agent, profile)`.

### Added — Shared wiki SoT namespace policy (§3)

- **`~/.duduclaw/shared/wiki/.scope.toml`** declares which top-level
  namespaces are read-only / operator-only. Three modes:
  `agent_writable` (default — same as v1.10.1, no regression),
  `read_only { synced_from = "<capability>" }` (only the named internal
  capability or operator may write), `operator_only` (never writable
  via MCP).
- **Enforcement** in both `handle_shared_wiki_write` and
  `handle_shared_wiki_delete` — the namespace policy is the authority,
  not the per-page ACL. Read-only namespaces deny even the original
  page author from deleting.
- **`wiki_namespace_status` MCP tool** lets agents introspect the
  active policy before attempting a write.
- **Fail-safe**: absent file ⇒ empty policy ⇒ everything writable.
  Malformed TOML ⇒ logged warning + treated as no policy. Hot-reload
  is automatic — every write/delete re-reads the file (KB-sized; not
  on the hot path).
- **Reserved policy filename**: `.scope.toml` is implicitly rejected by
  the existing `.md` extension check in `validate_wiki_page_path`; no
  separate reserved-list entry needed.

### Added — Documentation

- **`docs/RFC-21-identity-credential-isolation.md`** — original design
  doc with three-section migration plan, acceptance criteria, risks,
  and rollout strategy.
- **`docs/RFC-21-operator-guide.md`** — step-by-step deployment
  playbook for all three sections, with verify commands, common
  pitfalls, and migration sequence from the v1.10.1 single-tenant
  deployment.
- **`docs/features/17-wiki-knowledge-layer.md`** updated with the
  namespace SoT policy section.
- **`CLAUDE.md`** Architecture Overview header bumped to v1.11.0; new
  bullets summarising RFC-21 §1 / §2 / §3 in the relevant sections.

### Tests

Cross four crates, **1193 unit + integration tests pass** with no
regression:

- `duduclaw-identity` 31/31 (15 wiki_cache + 7 chained + 9 notion) +
  1 doctest
- `duduclaw-odoo` 27/27 (15 new agent_config tests on top of existing
  12)
- `duduclaw-cli` 301/301 — 15 wiki_scope unit + 12 odoo_pool unit + 14
  odoo_pool_dispatch integration + 4 identity_resolve integration + 7
  new wiki_schema_tests for namespace policy enforcement
- `duduclaw-gateway` 834/834 (7 new sender_block tests)

### Backwards compat

Every section preserves v1.10.1 behaviour for deployments that don't
opt in:

- Absent `.scope.toml` ⇒ no namespace restrictions.
- Absent `[identity]` ⇒ no `<sender>` block; `shared_wiki_read` for
  identity continues to work.
- Absent `agent.toml [odoo]` ⇒ pool collapses to `(agent_id,
  "default")` slot using global config exactly as before.

No flag-day migration required.

### Commits

`867e719` (RFC) → `1a967f5` (§3) → `53e19a8` (§1 step 1-2) → `5c0b116`
(§1 step 4) → `a17ba5a` (§2) → `9a40c18` (§1 step 3) → `3269ca0`
(operator guide + status reflection) → `<this commit>` (v1.11.0 release).


## [1.10.1] - 2026-05-04

### Fixed — Release pipeline
- **PyPI publish 失敗修正**：`pyproject.toml` 仍停留在 `1.8.0`（自 v1.8.0 release 後未隨 workspace 同步），導致 v1.10.0 release workflow 嘗試重複上傳已存在的 `duduclaw-1.8.0-py3-none-any.whl`，被 PyPI 拒以 `400 File already exists`。本版同步將 Python SDK 版本提升至 `1.10.1`，與 Cargo workspace 對齊。
- **`pypa/gh-action-pypi-publish` 加上 `skip-existing: true`**：未來若同一版本被重新觸發（workflow_dispatch 重跑、tag 重推），PyPI 步驟會跳過而非整個 release job 失敗。Trusted Publisher 與 token fallback 兩條路徑都套用。

### 內容差異
- v1.10.0 的 GitHub Release 二進位、npm 套件已成功發佈；本 patch 主要是把 PyPI 的 `duduclaw` 套件補上來，並順帶 bump 一個 Cargo workspace patch 版本以走完整 release pipeline。Rust / web 程式碼相對 v1.10.0 無新增功能。


## [1.10.0] - 2026-05-03

### Added — Wiki RL Trust Feedback（核心新功能）
- **`duduclaw-memory` 新增** `trust_store.rs` / `feedback.rs` / `janitor.rs` — 預測誤差驅動的 wiki 信任反饋系統。
  - `WikiTrustStore`（SQLite，PK `(page_path, agent_id)` 每 agent 獨立 trust）
  - `CitationTracker` 用 turn_id 為 drain key、session_id 為 cap budget key（兩級 id），LRU + bounded-time 雙條件 eviction 防 keep-alive DoS
  - `WikiJanitor` 每日 pass：3 negatives in 30d 加 `corrected` tag、隔離 30d 後 archive 至 `wiki/_archive/`、frontmatter ↔ live trust 同步
  - 防禦：per-page daily cap (10/day)、per-conv Δ cap (0.10)、`VerifiedFact` ×0.5 抗性、`lock=true` 人工 override、0.10/0.20 archive hysteresis
- **`duduclaw-gateway` 新增** `prediction/feedback_bus.rs` / `wiki_trust_federation.rs` — `TrustFeedbackBus` 在每次 `PredictionError` 後 drain `CitationTracker` 並 dispatch 簽名 deltas（error < 0.20 → positive、≥ 0.55 → negative）；GVU 結果以 2× magnitude 經 `on_gvu_outcome` 進信任反饋。
- **Federation 同步**（Q3）：trust 信號可跨機 export/import，衝突取均值、`do_not_inject` 取 OR、`schema_version` 拒絕未來版本、5000 updates/push + 1 MiB body 上限 + `constant_time_eq` bearer。
- **MCP 工具**：`wiki_trust_audit` / `wiki_trust_history`；RPC `wiki.trust_audit / trust_history / trust_override`。
- **Search ranking** 改為 `score × (0.5 + live_trust) × source_type_factor`（verified_fact ×1.2，raw_dialogue ×0.6）。
- **Web** 新增 `WikiTrustPage.tsx` 儀表板（trust 列表、history、override、archive 操作）。
- 文件：[docs/wiki-trust-feedback.md](docs/wiki-trust-feedback.md) runbook + 架構說明。

### Added — v1.10 收尾
- **Sub-agent enqueue turn_id 完整貫通**：`DUDUCLAW_TURN_ID` / `DUDUCLAW_SESSION_ID` 兩個 env var 常數，gateway spawn Claude CLI 時 set，MCP `send_to_agent` 讀 env 並寫入 `message_queue.{turn_id, session_id}`，dispatcher 從 queue 讀回後重新 scope。channel → 頂層 agent → MCP send_to_agent → SQLite queue → dispatcher → 子 agent CLI 全鏈 turn_id/session_id 正確傳遞。
- **`flock` for `wiki_trust.db`**：advisory file lock 防多 process 共用 home_dir 造成 archive race / frontmatter 競爭，第二個 process fail-fast 並回明確錯誤。
- **Atomic batch upsert（真正單 Tx）**：`WikiTrustStore::upsert_signal_batch` 一次 `BEGIN IMMEDIATE` 處理整批；32 citations / 1 prediction error 從 32 fsync 收斂為 **1 fsync**；任何中途錯誤自動 rollback。原本延後到 v1.11 的計畫**提前在 v1.10 完成**。
- **ABS migration once-only**：`wiki_trust_meta` 標記 conv_cap ABS migration 已完成，避免每次 boot 全表掃描。

### Schema migration
- `message_queue.turn_id` / `message_queue.session_id` columns 自動新增（既有資料庫升級時 NULL，新訊息會帶值）
- `wiki_trust_meta(key, value)` 新表 + `conv_cap_abs_migration_done` 標記
- `wiki_trust_state` / `wiki_trust_history` / `wiki_trust_rate` / `wiki_trust_conv_cap`（PK rename `conversation_id` → `cap_budget_id`）/ `idx_wiki_trust_history_agent_kind_ts` / `idx_wiki_trust_history_ts`

### Tests
- Backend **126 tests pass**（duduclaw-memory），包含 5 個 v1.10 regression test：flock、batch order、batch cap-budget shared、batch single-Tx、migration once-only
- 5 輪深度審查（code / security / database / architecture）+ Round 5 SHIP-BLOCK 修復全數收斂


## [1.9.4] - 2026-05-02

### Added
- **`duduclaw-durability` crate** — five-pillar durability framework:
  `idempotency` (key 管理防止重複執行)、`retry`（指數退避 + jitter）、
  `circuit_breaker`（三態 Closed/Open/HalfOpen）、`checkpoint`（任務進度
  斷點續傳）、`dlq`（Dead Letter Queue 終態失敗訊息）。完整 unit +
  integration tests 涵蓋高並發場景。
- **`duduclaw-governance` crate**（W19-P1 M1-A）— PolicyRegistry +
  4 種 PolicyType（Rate / Permission / Quota / Lifecycle）+ YAML 載入 +
  熱重載 + Agent 優先序合併 + fail-safe（非法政策跳過、非法 YAML 不
  panic）+ 並發 upsert 安全。新增 `quota_manager.rs`（每 agent / 每
  policy 配額 soft/hard 強制）+ `error_codes.rs`（QUOTA_EXCEEDED /
  POLICY_DENIED 等標準化錯誤碼）+ `evaluator` / `violation` /
  `approval` / `audit` 完整 PolicyEngine。預設政策集 `policies/global.yaml`
  含 default-rate-mcp（200/min MCP 呼叫限制）等六項。
- **MCP HTTP/SSE Transport**（W20-P1/P2）— 新增 `duduclaw http-server
  --bind 127.0.0.1:8765` 子命令。`mcp_http_server.rs` 提供
  `POST /mcp/v1/call`（單次 JSON-RPC 2.0 工具呼叫）、
  `GET /mcp/v1/stream`（SSE 長連接事件流，Bearer / `?api_key=`）、
  `POST /mcp/v1/stream/call`（async + SSE 結果推送）、`GET /healthz`
  （無需認證）。`mcp_rate_limit.rs` 新增 `OpType::HttpRequest`（60
  req/min token bucket），`mcp_sse_store.rs` 連線管理與 broadcast
  channel 事件推送，`mcp_http_auth.rs` / `mcp_http_errors.rs` 處理
  認證 + JSON-RPC↔HTTP 錯誤映射。
- **`skill_synthesis_run` MCP tool**（W20-P0）— Internal principal 可見、
  external 隱藏。`pipeline.rs::graduate_trajectories()` 取代 Phase 2
  stub，串起 memory_search → skill_extract → security_scan →
  skill_graduate 完整流程。
- **`duduclaw-memory` 評測 batch query API** — 新增 `MemoryEngine`
  方法支援評測批次查詢，配合 LOCOMO 評測系統。
- **LOCOMO 記憶評測系統**（W21）— `python/duduclaw/memory_eval/`：
  `retrieval_accuracy` / `retention_rate` / `locomo_integrity_check`
  + `cron_runner`（每日 03:00 UTC 排程）+ 5 分鐘 `smoke_test` P0 +
  `build_golden_qa`（從 LOCOMO 資料集建構黃金 QA）+
  `data/golden_qa_set.jsonl`（首批 200 筆 golden QA）+ `client.py` /
  `config.py` / `db/consolidation.py`。
- **Python `agents/` + `mcp/` 模組** — `agents/capabilities/`
  （manifest 載入 + matcher）、`agents/routing/`（capability-based
  router + resolution + memory_resolver）；`mcp/auth/`（API Key 驗證
  含 key masking 防洩漏）、`mcp/tools/memory/`（store / read / search
  / namespace / quota 含 scope 強制驗證）。
- **LLM Fallback** — `claude_runner.rs` + `llm_fallback.rs`：主模型
  逾時 / 503 / 429 / overloaded 時自動切換 fallback 模型。新增
  `is_llm_fallback_error` / `should_attempt_model_fallback` 純函式
  + 完整 unit tests。
- **Evolution Events 系統擴充** — `schema.rs` 新增 30+ event schema
  定義（+483 行）、`emitter.rs` 非同步發送支援 batch + retry（+190
  行）、新增 `query.rs`（EvolutionEvent 查詢介面，1685 行）+
  `reliability.rs`（事件可靠性保證機制，324 行）。Gateway HTTP
  endpoints 暴露於 `handlers.rs`（+154 行）。
- **Web `ReliabilityPage`**（+328 行，`/reliability` 路由）— circuit
  breaker 狀態、retry 統計、DLQ 佇列深度即時儀表板。`api.ts` 新增
  `getEvolutionEvents` / `getReliabilityStats` / `getDlqItems`。
- **`duduclaw evolution finalize` CLI 子命令**（v1.9.1 引入，v1.9.4
  封版穩定）— `--dry-run` / `--agent <id>`，一次性回收逾期 SOUL.md
  觀察視窗。
- **`claude_desktop_config.example.json`** — Claude Desktop MCP Server
  整合設定範例。

### Fixed (W21 QA 4-round CRITICAL/HIGH 全清)
- **CRITICAL — 記憶 MCP scope 認證缺口**：`mcp/tools/memory/store.py`、
  `read.py`、`search.py` 在 `execute()` 進入點補上 `memory:write` /
  `memory:read` scope 強制檢查。修補先前任意有效 API Key 都能繞過
  scope 限制的認證缺口。
- **HIGH — XSS 儲存型注入**：`validation.py::validated_tags` 改用
  `_sanitize(tag)` 處理使用者輸入的 tag。
- **HIGH — SSRF 防護**：`client.py::build_client()` 新增 URL
  scheme/netloc 驗證，拒絕指向內網或私有位址的 URL。
- **HIGH — circuit breaker 幽靈探測**：`circuit_breaker.rs`
  OPEN→HALF_OPEN 轉換時補上 `probe_inflight.saturating_add(1)`。修復
  並發探測數比設計上限多 1 的 bug。
- **HIGH — `claude_runner.rs` hard deadline 邏輯**：移除 partial
  output 時 `break` 的分支，統一回傳含 "hard timeout" 字串的 `Err`，
  確保 `is_llm_fallback_error` 正確觸發 fallback。
- **HIGH — UTF-8 truncation panic**：`llm_fallback.rs` truncation 改用
  `char_indices` 安全 UTF-8 char boundary 切片。修復多位元組字元在
  byte 512 邊界處切割時的 runtime panic。
- **Web 高危依賴**：`vite` 8.0.0-8.0.4 → 8.0.5+（GHSA-4w7w-66w2-5vf9
  + GHSA-v2wj-q39q-566r + GHSA-p9ff-h696-f583：Path Traversal in
  Optimized Deps、`server.fs.deny` bypass、Arbitrary File Read via
  WebSocket）；`postcss` <8.5.10 → 8.5.10+（GHSA-qx2v-qp2m-jg93：XSS
  via Unescaped `</style>` in CSS Stringify Output）。npm audit 0
  vulnerabilities。
- **Inference 編譯**：`ProgressCallback` 補上 `Sync` trait bound，修復
  多執行緒共享場景編譯錯誤。

### Tests
- 549+ tests, 0 failures（包含 `duduclaw-durability`、
  `duduclaw-governance` 73 tests + integration 22 個 W19-P1 M1-A
  驗收項、MCP HTTP transport tests、LLM fallback unit tests、Python
  agents routing + memory MCP tools 含 api_key_masking 安全測試）。

### Build/Repo
- `.gitignore` 排除 Python coverage db (`.coverage` /
  `**/.coverage`)、`release artifacts/`、各平台 `npm/*/bin/` 預建
  binary（應透過 npm publish）。
- `pyproject.toml` 更新 Python 依賴版本（memory_eval / agents / mcp
  相關套件）。


## [1.9.3] - 2026-04-28

### Fixed
- **Heartbeat: task-board pull 對所有 agent 生效，無視 enabled flag**。
  `poll_assigned_tasks` 之前在 `execute_heartbeat` 內，僅當 agent 心跳
  config `enabled=true` 才會跑。生產環境 17 個 agent 中有 16 個預設
  `enabled=false`，於是新加的 task board pull 對最需要它的 agent 從
  未觸發 — 包括 2026-04-28 12:27 觀察到的 26 個未路由 backlog 任務。
  修正：將 pull 上移到 `HeartbeatScheduler::run` 的 tick body，每 30s
  掃描整個 agent registry。`poll_assigned_tasks` 原有的 1-hour LIKE
  marker cooldown 已防止 stampede。task board pull 概念上屬 scheduler
  層級而非 per-agent evolution，agent 不該為了被指派工作時被叫醒而
  opt-in。


## [1.9.2] - 2026-04-28

### Fixed
- **Discord Gateway: 真正實作 RESUME (op 6) + stall watchdog**
  （`discord.rs`）。
  - 持久化 `session_id` + `resume_gateway_url` + sequence 跨重連。
    先前每次重連都發新的 IDENTIFY，丟掉 Discord 在斷線期間緩衝的所有
    事件。
  - 第三個 `select!` arm 加入 stall watchdog：超過 2× heartbeat
    interval 沒有任何流量就 break。修復 2026-04-28 11:17Z 觀察到的
    silent zombie 狀態，gateway loop 卡住 18 分鐘無任何 log 輸出。
  - heartbeat channel capacity `1 → 16` + `try_send` 防止 `select!`
    消費慢時反向阻塞。
  - Op 9 Invalid Session 讀 `d.bool` 決定 RESUME vs IDENTIFY，依
    Discord docs 加 1-5s jitter。
  - close codes 4007/4009/4003 清掉 session state 觸發新 IDENTIFY。
  - backoff cap 300s → 60s；不要懲罰已經跑了好幾小時的 session。
  - 處理 `RESUMED` dispatch event。


## [1.9.1] - 2026-04-28

### Added
- **`duduclaw evolution finalize` CLI subcommand** with `--dry-run` and
  `--agent <id>` filters. One-shot recovery for SOUL.md observation
  windows that should already have transitioned but never did.

### Fixed (self-evolution pipeline — 5 audit gaps from 2026-04-28 health check)
- **SOUL.md observation windows now actually close.**
  `VersionStore::get_expired_observations` and `Updater::execute_confirm /
  execute_rollback` had no callers, so the very first applied SOUL change
  blocked all subsequent GVU proposals indefinitely. agnes was stuck for
  6 days locally. Adds a 30-min `ObservationFinalizer` background task
  that computes post-metrics from `prediction.db` + `feedback.jsonl`,
  runs the existing `judge_outcome` tolerance logic, and confirms /
  rolls back / extends accordingly.
- **EvolutionEvents audit log now writes to a stable absolute path.**
  Default base directory was `data/evolution/events` — relative to cwd.
  Gateway boot from `cwd=$HOME` silently dropped every audit event. Now
  resolves via layered fallback: `$EVOLUTION_EVENTS_DIR` →
  `$DUDUCLAW_HOME/evolution/events` → `$HOME/.duduclaw/evolution/events`
  → legacy. Boot also injects the env var before any emitter is
  constructed and runs a `.healthcheck` self-test that surfaces IO
  failures via `tracing::error!` instead of silent `eprintln!`.
- **Silence breaker now actually triggers a forced reflection.**
  `heartbeat.rs` previously only emitted `warn!` and reset its own timer
  — the system advertised "self-reflection on long silence" but never
  did anything. Adds a `SilenceBreakerEvent` mpsc channel; the gateway
  consumes it and writes a typed `silence_breaker` row to
  `prediction.db.evolution_events`, with a 4-hour per-agent cooldown to
  prevent loops.
- **MetaCognition rehydrates counters from `prediction.db` on startup.**
  `total_predictions` and `predictions_since_last_eval` were stuck at 0
  across restarts because `metacognition.json` only persisted at
  evaluation time. With `evaluation_interval=100` the threshold became
  unreachable and adaptive thresholds never recalibrated. Now takes
  `max(disk, in-memory)` and runs a one-shot `evaluate_and_adjust` if
  the in-memory counter is overdue. Also anchors
  `original_sig_improvement_rate` baseline on the first eval that has
  ≥5 Significant samples (was previously stuck at `null`).
- **Sub-agent dispatches now record prediction samples.**
  `prediction.db.user_models` had only the channel-facing root agent
  (1/19 in our deployment); 18 sub-agents accumulated nothing because
  the prediction hook only ran in `channel_reply`, not in
  `dispatcher.rs`. Adds a fire-and-forget `subagent_prediction` module
  that synthesises `user_id = "agent:<sender_or_origin>"`, builds a
  2-message `ConversationMetrics` snapshot from the dispatched payload
  + response, and runs the same `predict → calculate_error →
  log_evolution_event → update_model` cycle as the channel path. Hooks
  both the JSONL and SQLite dispatch loops; deliberately does NOT
  trigger the GVU loop from this path (preserves the channel-only
  invariant for SOUL evolution).

### Tests
- 23 new unit tests across `observation_finalizer`, `evolution_events::logger`,
  `prediction::forced_reflection`, `prediction::metacognition` (BUG-4 group),
  and `prediction::subagent_prediction`.
- Workspace tests after the change:
  duduclaw-gateway 730 ✓, duduclaw-agent 31 ✓, duduclaw-cli 80 ✓.

### Dashboard
- ActivityFeed no longer crashes when the gateway emits an unknown
  `ActivityType`. Adds explicit entries for `autopilot_triggered` and
  `autopilot_lag`, plus a neutral `FALLBACK_CONFIG` so future unknown
  types render as a generic row instead of throwing on `config.icon`.


## [1.8.34] - 2026-04-27

### Fixed
- **Local-fallback path silently failed for users running a remote
  OpenAI-compatible inference server (vLLM / SGLang / llamafile).**
  Reproducer: Linux gateway with no Claude CLI installed,
  `inference_mode = "local"` in `config.toml`, and `[openai_compat]`
  pointing at `http://192.168.168.244:8000/v1` in `inference.toml`.
  Sending a message via the dashboard webchat returned
  `DuDu 暫時無法回應：系統找不到 Claude Code CLI` even though the
  remote vLLM endpoint was reachable and the model id matched.

  Root cause: `InferenceEngine::load_model` unconditionally called
  `ModelManager::resolve_path`, which only finds GGUF files under
  `~/.duduclaw/models/`. For remote backends the model lives on a
  server, so `resolve_path` returned `ModelNotFound` and the engine
  errored before `OpenAiCompatBackend` ever saw the request — making
  the `channel_reply` local-fallback path silently fail with the
  misleading "Claude Code CLI not found" final message.

  Gateway log evidence:
  ```
  WARN duduclaw_inference::engine: Failed to auto-load model
    model="qwen3.6-35b-a3b" error=Model not found: qwen3.6-35b-a3b
  WARN duduclaw_gateway::channel_reply: Local inference unavailable:
    Local inference error: Model not found: qwen3.6-35b-a3b
  WARN duduclaw_gateway::channel_reply: Channel reply fallback —
    all providers failed agent=DuDu reason=BinaryMissing
    last_error=claude CLI not found in PATH
  ```

  Fix: add `InferenceBackend::requires_local_file` (default `true`,
  override `false` in `OpenAiCompatBackend`) and gate `resolve_path`
  on it. Remote backends now receive the raw model id, which matches
  what `OpenAiCompatBackend::load_model` already does (ignores the
  path arg and uses `[openai_compat].base_url + .model` from
  `inference.toml`).

  Adds two regression tests in `engine::tests` using a stub backend:
  - `load_model_skips_path_resolution_for_remote_backends`
  - `load_model_still_resolves_path_for_local_backends`

  Workaround for users on ≤ 1.8.33: `touch
  ~/.duduclaw/models/<model-id>.gguf` to satisfy the path check.
  Safe to delete after upgrading to 1.8.34.


## [1.8.33] - 2026-04-27

### Fixed
- **Windows: BatBadBut spawn error persisted on hosts where the
  `@anthropic-ai/claude-code` npm package ships a native `.exe`
  instead of a JS CLI.** The customer reproducer on 2026-04-27
  (after v1.8.32 still failed) revealed the `claude.cmd` shim
  contents:

  ```bat
  @ECHO off
  GOTO start
  :find_dp0
  SET dp0=%~dp0
  EXIT /b
  :start
  SETLOCAL
  CALL :find_dp0
  "%dp0%\node_modules\@anthropic-ai\claude-code\bin\claude.exe"   %*
  ```

  `@anthropic-ai/claude-code` ≥ 2.x ships a real `claude.exe` inside
  the npm package and the cmd shim is just a transfer wrapper. The
  v1.8.32 shim parser only matched `.js`/`.mjs`/`.cjs` references,
  returned `None` for the `.exe` line, fell through to known-layout
  probes (which also only checked for `cli.js` / `cli.mjs`), returned
  `None` there too, and the caller spawned the `.cmd` directly →
  BatBadBut. The diagnostic log added in v1.8.32 confirmed it:

  ```
  INFO Resolved claude binary
    path=C:\Users\USER\AppData\Roaming\npm\claude.cmd
    candidates=[..., "...\\claude.cmd"]   ← no .exe in pool
  WARN claude CLI spawn error: batch file arguments are invalid
  ```

  **Fix**: extend the shim parser and probe table to follow shims
  that point to a real `.exe` (not just JavaScript scripts). Three
  rule changes in [`platform::resolve_cmd_shim`](crates/duduclaw-core/src/platform.rs):

  1. `clean_shim_token` now matches `.exe` in addition to
     `.js`/`.mjs`/`.cjs`. The result is typed:
     `ShimTarget { kind: Exe | Script, rel: String }`.

  2. **Per-line target selection rule**:
     - Line has BOTH `.exe` AND a script → **Script wins** (the
       `.exe` is the runtime — `node.exe` / `bun.exe` — and the
       script is the actual target). Handles Bun / pnpm / yarn
       JS shims.
     - Line has ONLY `.exe` → **Exe wins** (new-style native shim;
       the `.exe` IS the target). Handles the customer's case.
     - Line has ONLY a script → **Script wins** (legacy npm shims).

  3. `known_cli_subpaths` → `known_target_subpaths` now contains 5
     native-`.exe` probes covering npm / yarn / Bun / pnpm globals —
     each terminating at `node_modules/@anthropic-ai/claude-code/bin/claude.exe`.
     Legacy `cli.js` / `cli.mjs` probes are retained for older
     installs.

  After this change, the customer's spawn path becomes:
  `Command::new("C:\\Users\\USER\\AppData\\Roaming\\npm\\node_modules\\@anthropic-ai\\claude-code\\bin\\claude.exe")` —
  a direct `.exe` invocation with zero `cmd.exe` involvement and
  zero BatBadBut hazard, regardless of prompt content.

### Changed
- `resolve_cmd_to_node` (private) renamed to `resolve_cmd_shim` and
  now returns `Option<(String, Vec<String>)>` — a real executable
  plus prefix args — so callers can spawn either a direct `.exe`
  (`vec![]`) or `node + cli.js` (`vec![cli.js]`) uniformly.
  `command_for` / `async_command_for` updated accordingly.

### Tests
- Shim parser tests overhauled around the new `parse_shim_target`
  API. 14 cross-platform unit tests now cover:
  - the new-style native-`.exe` shim (the customer's exact
    `claude.cmd` content reproduced verbatim),
  - legacy JS shims for npm v9 / Bun / pnpm / yarn classic,
  - the **Script-wins-over-Exe-when-both-present** priority rule,
  - the multi-token-per-line ordering for both `.exe` and `.js`,
  - the empty-shim, unquoted-hand-written, and `.cjs` extension
    edge cases,
  - a `known_target_subpaths_cover_native_and_legacy` assertion
    that the probe table contains ≥4 native-`.exe` probes and ≥4
    JS probes, all targeting `@anthropic-ai/claude-code`.


## [1.8.32] - 2026-04-27

### Fixed
- **Windows: BatBadBut spawn error persisted after v1.8.31 because
  `which_claude` short-circuited on `where.exe` results before
  HOME-rooted candidates were consulted.** v1.8.31 reordered the HOME
  candidate list so `.exe` came before `.cmd`, but missed the more
  fundamental bug: [`which_claude`](crates/duduclaw-core/src/lib.rs)
  ran `where.exe claude` first and **returned the first matching
  `.exe` OR `.cmd` line**, never reaching the HOME scan. On hosts
  with both a clean `~/.local/bin/claude.exe` install AND a leftover
  `%APPDATA%\npm\claude.cmd`, `where.exe` typically returned the
  `.cmd` first when PATH included `%APPDATA%\Roaming\npm` (which it
  often does for service / launchd / Explorer-launched processes
  even though the user's interactive shell shows it empty). The
  `.cmd` then triggered Rust 1.77+'s
  [BatBadBut][batbadbut] rejection (CVE-2024-24576) for any prompt
  containing newlines / quotes / `&` — i.e. essentially every prompt.

  [batbadbut]: https://blog.rust-lang.org/2024/04/09/cve-2024-24576/

  **Fix**: `which_claude` now **pools** results from PATH discovery
  AND the HOME-rooted scan (deduped), then applies a strict
  precedence regardless of source:

  1. any `.exe` in the pool wins (always safe to spawn)
  2. then any `.cmd` (parsed by `resolve_cmd_to_node` into
     `node.exe + cli.js` to avoid handing args to `cmd.exe`)
  3. then extensionless paths with `.exe`/`.cmd` appended via FS check
  4. last resort: first existing entry as-is

  On the customer machine that was failing in v1.8.31, this means
  `where.exe claude` returning `%APPDATA%\Roaming\npm\claude.cmd`
  AND the HOME scan finding `~/.local/bin/claude.exe` now resolves
  to the `.exe` — bypassing the BatBadBut hazard entirely.

### Added
- **One-shot `INFO` log of the resolved `claude` binary path on the
  first `which_claude` call.** The log line includes both the chosen
  path and the full discovery pool. This means future Windows /
  multi-installer issue reports arrive with the resolved path
  already in the logs:

      INFO duduclaw_core: Resolved claude binary
        path="C:\\Users\\X\\.local\\bin\\claude.exe"
        candidates=["C:\\Users\\X\\AppData\\Roaming\\npm\\claude.cmd",
                    "C:\\Users\\X\\.local\\bin\\claude.exe"]

  Subsequent `which_claude` calls (there are 11 call sites — channel
  reply, account rotation, heartbeat, etc.) are silent so this never
  becomes log spam.

### Tests
- 7 new cross-platform unit tests in `which_claude_tests` exercise
  the new precedence rules:
  `windows_pref_exe_beats_cmd_even_when_cmd_listed_first`,
  `windows_pref_picks_cmd_when_no_exe_exists`,
  `windows_pref_returns_none_for_empty_pool`,
  `windows_pref_first_exe_wins_among_multiple_exes`,
  `windows_pref_first_cmd_wins_among_multiple_cmds_when_no_exe`,
  `windows_pref_extension_check_is_case_insensitive` (handles
  uppercase `.EXE` / `.CMD` from PATHEXT-style discovery), and
  `windows_pref_falls_back_to_first_for_extensionless_when_no_fs_match`.

  Compile-gated with `#[cfg(any(windows, test))]` on the helper
  `pick_windows_preferred` so macOS / Linux CI runners can validate
  the Windows-only logic without needing a Windows host.


## [1.8.31] - 2026-04-27

### Fixed
- **Windows: `claude CLI spawn error: batch file arguments are
  invalid` blocking every channel reply.** Rust 1.77+ rejects spawning
  `.bat`/`.cmd` files when argv contains characters that could be
  reinterpreted by `cmd.exe` (newlines, quotes, `&`, `|`, …) — the
  [BatBadBut][batbadbut] mitigation for CVE-2024-24576. User prompts
  and system prompts routinely contain those characters, so `claude
  -p` subprocess calls failed at spawn time on every Windows host
  whose `which_claude` resolved to `%APPDATA%\npm\claude.cmd` (or any
  other npm/Bun/pnpm/yarn `.cmd` shim). The rotator interpreted the
  spawn failure as an account error, retried each account in turn, and
  surfaced the misleading `All accounts exhausted` to the user.

  [batbadbut]: https://blog.rust-lang.org/2024/04/09/cve-2024-24576/

  **Two-layer fix in `duduclaw-core`:**

  1. [`which_claude_in_home`](crates/duduclaw-core/src/lib.rs) on
     Windows now **prefers `.exe` over `.cmd`** in candidate ordering.
     A host with both a real `.exe` install (e.g. Claude Code native
     installer at `~/.local/bin/claude.exe`) and a leftover npm
     `.cmd` shim previously matched the `.cmd` first and tripped
     BatBadBut. Reordered so every `.exe` location is checked before
     any `.cmd`. Also added the **`~/.local/bin/claude.exe`** path
     (the official native installer's XDG-style location on Windows,
     previously missing) plus pnpm / Yarn-classic / Bun-`.cmd` /
     Volta-`.cmd` fallbacks.

  2. [`platform::resolve_cmd_to_node`](crates/duduclaw-core/src/platform.rs)
     — the npm-shim parser that converts a `.cmd` shim into a
     `node.exe + cli.js` invocation (so we never hand args to
     `cmd.exe`) — previously only matched paths containing
     `node_modules` ending in `.mjs`/`.js`. Bun (`..\packages\…`),
     pnpm (`..\global\5\node_modules\…`), and Yarn classic
     (`..\lib\node_modules\…`) all parsed as `None` and fell through
     to the BatBadBut path. New parser scans every quoted segment +
     every whitespace token, expands `%~dp0` / `%dp0%` / `%~dpn0` /
     `%~f0` / `%CD%` to empty, normalizes `\` to `/` for
     cross-platform path joining, accepts `.cjs`, and picks the
     *last* JS token per line so wrapper scripts don't shadow the
     real `cli.js`. When parsing still fails (binary wrappers, custom
     shims), a known-layout probe checks 6 well-known relative paths
     from the shim directory to `@anthropic-ai/claude-code/cli.js`
     for npm / Bun / yarn / pnpm.

  **Diagnostic note**: `where claude` returning empty on the customer
  machine was a red herring — `which_claude`'s HOME-rooted candidate
  scan still found `~/.local/bin/claude.exe`. The actual root cause
  was the `.cmd`-before-`.exe` ordering shadowing it.

### Tests
- 11 new cross-platform unit tests in `platform::shim_parser_tests`
  exercise npm v9 / Bun / pnpm / Yarn-classic shim formats, the
  pure-`.exe`-wrapper case, multi-`.js`-per-line ordering, `.cjs`
  extension handling, and unquoted-token fallback. Compile-gated with
  `#[cfg(any(windows, test))]` so they run on macOS/Linux CI hosts
  and validate the parser without needing a Windows runner.


## [1.8.30] - 2026-04-24

### Fixed
- **Native Claude Code tools (`WebSearch` / `WebFetch` / `Read` /
  `Write` / `Edit` / `Glob` / `Grep` / `Bash` / `TodoWrite`) were
  silently unavailable to `claude -p` subprocesses**, causing
  researcher cron tasks to receive 0 results and bail out even when
  the same tools worked in interactive Claude Code sessions.

  **Root cause**: [`claude_runner.rs`](crates/duduclaw-gateway/src/claude_runner.rs)
  passed `--allowedTools "mcp__duduclaw__*"` to `claude -p`. Claude
  Code treats `--allowedTools` as an **exclusive** auto-approve list,
  not an *additive* one: anything not matching would need interactive
  confirmation, which is impossible in subprocess mode. The built-in
  tools therefore returned empty / no-oped with no error signal.

  User-visible symptom (from the 2026-04-24 evening cron run): the
  `ai-papers-researcher` / `ai-repos-researcher` agents correctly
  followed their updated SOUL.md and cron prompts (which now direct
  them to use native `WebSearch` instead of the DDG-blocked MCP
  `web_search`), invoked `WebSearch`, got 0 results, and — per the
  hard-stop rule — aborted with "搜尋工具失效" inside six seconds.
  The equivalent query run interactively via Claude Code returned
  normal results immediately.

  **Fix**: expand the `--allowedTools` list to explicitly include the
  native tool names researchers actually need:

      mcp__duduclaw__*,WebSearch,WebFetch,Read,Write,Edit,
      Glob,Grep,Bash,TodoWrite

  This keeps the deny-by-default posture for anything not listed
  (e.g. no `KillBash` / `NotebookEdit` / etc.) while restoring the
  research capability that interactive Claude Code has had all along.
  `disallowed_tools` from `agent.toml [capabilities]` still layers on
  top via `--disallowedTools`, so explicit per-agent blocks are
  unchanged.


## [1.8.29] - 2026-04-24

### Fixed
- **Misleading "No auth token configured" startup banner.** The CLI
  always printed that message whenever `DUDUCLAW_AUTH_TOKEN` and
  `[gateway].auth_token` were both unset — but the WebSocket auth gate
  in `server::handle_socket` *also* requires JWT when `users.db`
  contains any rows (legacy `auth_token` and JWT are independent gates).
  Operators saw the message, assumed authentication was off, and then
  got spammed with `WebSocket auth failed – closing connection` once
  per second as the dashboard reconnected — with no hint that the real
  fix was to log in at `/login`.

### Changed
- [`duduclaw run`](crates/duduclaw-cli/src/lib.rs) now probes
  `~/.duduclaw/users.db` at startup (via `probe_users_db`). When any
  user exists the banner switches from "no auth token" to:

  ```
  🔐 JWT auth required: N user(s) in ~/.duduclaw/users.db
    Dashboard login: http://localhost:PORT/login
  ```

  so the correct next action is obvious.

- When `admin@local`'s stored password hash still verifies against the
  literal `"admin"` seeded by
  `duduclaw_auth::UserDb::ensure_default_admin`, an additional line
  warns: `⚠ Default admin still in use: admin@local / admin — change the
  password at /settings`. The verification uses the `argon2` crate
  directly (now a direct `duduclaw-cli` dep) rather than the full
  `duduclaw-auth` crate to keep the CLI's dependency surface narrow.

### Added
- 6 new unit tests in `startup_probe_tests` covering: missing
  `users.db`, empty users table, default-admin detection,
  non-default-password non-detection, admin@local absence, and
  garbage-PHC input handling.


## [1.8.28] - 2026-04-24

### Fixed
- **Cron notifications failed silently with Discord 401 Unauthorized
  in multi-bot setups.** When a cron-fired agent (e.g. `xianwen-pm`,
  `ai-papers-researcher`) had no per-agent
  `[channels.discord] bot_token` set in its `agent.toml`, the token
  resolver fell straight to the **global** `config.toml [channels]
  discord_bot_token_enc`. If that global token belongs to a different
  bot from the one that opened the notify target — and Discord threads
  are bot-scoped so only the opening bot can post into them — every
  delivery attempt returned `401 Unauthorized` even though the agent
  LLM call had already succeeded. User-visible symptom: cron
  `last_status = success` but nothing arrives in the Discord thread.

  **Fix**: new `resolve_agent_channel_token_via_reports_to` in
  [`config_crypto.rs`](crates/duduclaw-gateway/src/config_crypto.rs)
  walks the `reports_to` chain and returns the first ancestor's token.
  Cycle-safe (tracks visited ids) and bounded (`MAX_REPORTS_TO_HOPS =
  8`). Wired into both:

  1. [`cron_scheduler::resolve_channel_token`](crates/duduclaw-gateway/src/cron_scheduler.rs) — the cron
     `deliver_cron_result` path.
  2. [`dispatcher::resolve_forward_token`](crates/duduclaw-gateway/src/dispatcher.rs) — the
     `forward_delegation_response` path that relays sub-agent replies
     back to the originating channel.

  After this change, a cron-fired `xianwen-pm` with no Discord bot of
  its own inherits `xianwen-tl`'s token, or `agnes`'s if the TL also
  has none configured — matching the `reports_to` hierarchy the user
  already declared.

### Changed
- `resolve_forward_token` now does the `reports_to` cascade on **both**
  `callback_agent_id` AND `origin_agent` (the thread opener). The
  v1.8.20 behaviour of falling back to `origin_agent`'s direct token
  is preserved as step 3 in the cascade; steps 1-2 add the new walk so
  agents deeper in the hierarchy are covered without needing every TL
  / PM / researcher to have the same bot token pasted into their
  `agent.toml`.

- The stale single-purpose `get_agent_channel_token` helper in
  `dispatcher.rs` is removed — superseded by the shared cascade helper
  in `config_crypto.rs`.

### Added
- 8 new unit tests in `config_crypto::tests` covering the cascade:
  own-token wins, parent-token cascade, `None` when chain is empty,
  nearest-ancestor-not-farthest preference, cycle detection, missing
  agent.toml, `reports_to = ""` treated as root, and per-channel
  independence.


## [1.8.27] - 2026-04-23

### Added
- **Multica-inspired Agent integration layer** — agents are now
  first-class teammates on the task board, not just tools. Ships three
  coupled pieces:

  1. **12 new MCP tools** (`crates/duduclaw-cli/src/mcp.rs`) —
     `tasks_list`, `tasks_create`, `tasks_update`, `tasks_claim`,
     `tasks_complete`, `tasks_block`, `activity_post`, `activity_list`,
     `autopilot_list`, `shared_skill_list`, `shared_skill_share`,
     `shared_skill_adopt`. All mutating tools enforce
     `is_valid_agent_id` on the caller, and `tasks_list` defaults to
     the calling agent so noise stays low.
  2. **Pending task queue injection into the agent system prompt**
     (`crates/duduclaw-gateway/src/claude_runner.rs`) — every call to
     `call_claude_for_agent*` renders the top-5 open tasks (priority-
     ordered, `in_progress` → `todo` → `blocked`) into a
     `## Your Task Queue` block. Uses a shared `Arc<TaskStore>` via
     `OnceLock` so system-prompt composition doesn't open a fresh
     SQLite connection per turn. On the Direct API path the block is
     passed as an uncached second system block via
     `direct_api::call_direct_api_with_dynamic`, so the static 5–20k
     token prefix stays cacheable.
  3. **Autopilot trigger engine** (`autopilot_engine.rs`, new) —
     `tokio::broadcast::Sender<AutopilotEvent>` (capacity 8192) fed by
     both WebSocket handlers (in-process) and a SQLite event bus
     (out-of-process, see below). Typed variants: `TaskCreated`,
     `TaskUpdated`, `TaskStatusChanged`, `ActivityNew`, `ChannelMessage`,
     `AgentIdle`, `CronTick`. Condition DSL supports nested `all`/`any`
     + `eq`/`neq`/`in`/`not_in`/`gt`/`gte`/`lt`/`lte`/`contains`. Three
     action executors: `delegate` (MessageQueue enqueue), `notify`
     (Telegram/LINE/Discord/Slack via shared `reqwest::Client` from
     `OnceLock`), `run_skill` (reads the agent's `SKILLS/<name>.md`
     and delegates it as a prompt).

- **SQLite event bus** (`events_store.rs`, new) — `events.db` replaces
  the legacy `events.jsonl` file bus. WAL mode + `busy_timeout=5000` +
  monotonic auto-increment `id` give the tail reader a simple
  `WHERE id > ?` watermark; 7-day retention prune runs every 6 hours.
  Eliminates the file-bus hazard matrix in one swap (rotation race,
  partial-line reads, 0644 permissions, unbounded growth).

- **Dashboard Task Board preview widget** (`DashboardPage.tsx`) —
  `TasksPreviewCard` renders a mini 4-column Kanban with per-column
  task counts and links to `/tasks`. Loading skeleton, error banner,
  and empty-state tri-state so users can distinguish "never loaded"
  from "loaded empty".

- **Autopilot rule dashboard schema validation** (`handlers.rs`) —
  `autopilot.create` / `autopilot.update` reject unknown
  `trigger_event` values and `action` JSON missing required fields
  per type, so malformed rules fail immediately on the dashboard
  instead of silently during the first fire.

- **i18n keys** `tasks.preview.{title,viewAll,empty}` synced across
  `zh-TW`, `en`, `ja-JP`.

- **47 new unit tests** — 18 in `mcp::task_board_tests`, 18 in
  `autopilot_engine::tests` (including Closed/Open/HalfOpen state
  transitions), 7 in `handlers::autopilot_validation_tests`, 4 in
  `events_store::tests`. Full gateway lib suite: 611 tests passing.

### Changed
- **Task Board always renders four columns** (`TaskBoardPage.tsx`) —
  v1.4.29 hid the entire board behind an `tasks.length === 0`
  early-return, breaking the Kanban design intent that empty columns
  themselves *are* the affordance. Grid is now
  `grid-cols-1 md:grid-cols-2 lg:grid-cols-4` with each column keeping
  its own drop-hint placeholder.

- **Agent-facing MCP caller validation** is now consistent across
  `tasks_create` / `tasks_claim` / `tasks_complete` / `tasks_block` /
  `activity_post`. Wildcard (`*`) and path-traversal-like values are
  rejected at the boundary with a clear error message.

- **Autopilot circuit breaker is now a proper 3-state FSM** (Closed /
  Open / HalfOpen). 10 fires in 60s trip to Open (60s cooldown),
  HalfOpen allows one probe; retry within 30s re-trips, quiet window
  returns to Closed. All transitions are logged to `autopilot_history`
  and the Activity Feed so operators can see rule loops get contained
  and recover. Replaces the v1.8.27-dev sliding-window rate limiter.

- **Autopilot broadcast channel** capacity raised from 1024 → 8192 and
  the `RecvError::Lagged` branch escalated from `warn!` → `error!`
  with a detached `append_activity` task (so logging the lag no longer
  amplifies event drops).

### Fixed
- **Autopilot rule storage silently accepted malformed JSON**, so
  broken rules would only surface their error when first fired (and
  only in `autopilot_history`, invisible during rule authoring). Now
  rejected at write time.

- **`action_run_skill` had no path guard** — a crafted rule with
  `skill_name: "../../../etc/passwd"` could have escaped the
  SKILLS directory. Defense in depth: alphanumeric allowlist on both
  `target_agent` and `skill_name`, plus `canonicalize()` containment
  check against `<home>/agents/<agent>/SKILLS/`.

- **`events.jsonl` rotation race lost in-flight events** — writers
  holding an `O_APPEND` fd at the moment of `rename()` would land
  writes on the orphaned `.jsonl.1`, which the tail task ignored.
  Made moot by the SQLite event bus swap.

- **`build_pending_tasks_section` silently returned `None` when
  TaskStore open failed**, hiding a broken task board from operators.
  Now logs a warning at `warn!` level while still degrading gracefully
  (the agent just loses its task queue for that turn).

### Security
- **`events.db` is owned exclusively by the gateway/MCP process
  writing it** — SQLite handles file permissions (`0600` under default
  umask). Event payloads containing task descriptions / metadata are
  no longer world-readable on multi-user systems.


## [1.8.26] - 2026-04-22

### Added
- **`shared_wiki_lint` MCP tool** — audits `~/.duduclaw/shared/wiki/`
  for Karpathy LLM Wiki schema compliance. Reports: pages missing
  any of the six required frontmatter fields (`title`, `created`,
  `updated`, `tags`, `layer`, `trust`), pages containing fallback-
  content markers (e.g. "基於訓練資料", "web_search failed",
  "無法取得", "查無結果", "based on training data" …) that were not
  explicitly tagged `fallback-mode`, plus the existing graph-level
  checks (orphans / broken links / stale pages) delegated to
  `WikiStore::lint()`. Unlike per-agent `wiki_lint`, this tool
  takes no `agent_id` — shared wiki is a single global namespace.

### Fixed
- **Shared wiki accepted pages authored from stale LLM priors,
  polluting the cross-agent knowledge base.** When
  `ai-papers-researcher` / `ai-repos-researcher` cron tasks ran
  while `web_search` was failing, they silently fell back to
  recalling training data and wrote reports whose frontmatter
  looked legitimate but whose body was unanchored to any verifiable
  source (7/7 Hugging Face model URLs returned HTTP 200 + `<title>
  404` body in one case). These entered `shared/wiki/` unchallenged
  and drifted there indefinitely. Project rule: 「有 fallback 的資
  料不應該混入共用 wiki 中產生雜訊」.

  **Fix A** — `handle_shared_wiki_write` now enforces two gates
  before the write:

  1. **Frontmatter schema gate** (`validate_wiki_frontmatter`):
     page must open with a `---…---` block declaring *all* of
     `title, created, updated, tags, layer, trust`. `trust` must
     parse as a float in `[0.0, 1.0]`. Missing or malformed
     frontmatter → hard reject with a message pointing at the
     missing fields.
  2. **Fallback-content gate** (`detect_fallback_content`): body
     scanned for any of 14 CJK / English fallback markers. On
     match, reject unless the page explicitly opts in with
     `fallback-mode` in its `tags` (for post-mortem archives
     where a human deliberately wants the record preserved; those
     pages are still expected to carry `trust: 0.2` or lower).

  Per-agent `wiki_write` is intentionally left permissive — private
  wikis can hold speculative or fallback material; only the shared
  bus is strict.

- **Four research-pipeline cron prompts pushed fabricated content
  into `shared/wiki/` when search tools failed.**
  `ai-papers-morning`, `ai-papers-evening`, `ai-repos-morning`, and
  `ai-repos-evening` (rows in `~/.duduclaw/cron_tasks.db`) have been
  rewritten to:

  - **Abort on search failure** instead of falling through to
    training-data recall. The new prompts open with a hard
    precondition: if `web_search` returns 0 results, immediately
    notify `agnes` that "本日研究暫停：搜尋工具失效" and exit the
    task. Explicit ban on the 無法取得 / 基於訓練資料 / 查無結果
    narrative patterns (which now trip the shared-wiki fallback
    gate anyway).
  - **Two-layer URL verification** before any wiki write: a HEAD
    fetch must return HTTP 200 *and* the body must not contain
    `<title>404` (the Hugging Face gotcha where bad model URLs
    return 200 with a 404 page body). Items failing either check
    are dropped — the prompts are explicit that filling with
    unverified items is prohibited.
  - **Atomic-entity page layout per Karpathy LLM Wiki**: one
    entity page per paper/repo under `entities/YYYY-MM-DD-<slug>.
    md`, plus a daily digest under `research/ai-papers/YYYY-MM-DD-
    (08|20).md` whose `related:` points back to every entity.
    Frontmatter is spelled out explicitly inline (all six required
    fields, `layer: context`, `trust: 0.5` default, `sources:`
    list), and heading decoration emoji are banned.

  Backup of the pre-rewrite rows saved to
  `~/.duduclaw/cron_tasks.db.v1.8.25.bak` in case rollback is
  needed.

- **Two fabricated shared-wiki pages from 2026-04-22** were
  removed: `research/ai-repos/2026-04-22-08.md` (web_search
  fallback, 0 real URLs) and `research/ai-repos/2026-04-22-20.md`
  (7/7 HF model URLs were 404-in-body). `_index.md` cleaned and
  `_log.md` appended with `delete … by:operator (fabricated: …)`
  entries. Both surviving `research/ai-papers/*.md` pages were
  retrofitted with the full nine-field Karpathy frontmatter
  (`title`, `created`, `updated`, `author`, `tags`, `related`,
  `sources`, `layer: context`, `trust: 0.5`) so they pass the new
  `shared_wiki_lint` tool.

### Tests

**12 new** (all passing, all in `mcp::wiki_schema_tests`):

- `frontmatter_validator_accepts_full_schema`
- `frontmatter_validator_rejects_missing_frontmatter`
- `frontmatter_validator_rejects_missing_required_fields`
- `frontmatter_validator_rejects_out_of_range_trust`
- `frontmatter_validator_rejects_non_numeric_trust`
- `detect_fallback_catches_cjk_marker`
- `detect_fallback_catches_english_marker`
- `detect_fallback_ignores_clean_body`
- `shared_wiki_write_rejects_fallback_content`
- `shared_wiki_write_rejects_missing_frontmatter`
- `shared_wiki_write_allows_fallback_mode_opt_in`
- `shared_wiki_write_accepts_clean_karpathy_page`

Full workspace lib suite still green.


## [1.8.25] - 2026-04-22

### Fixed
- **Cron tasks scheduled `0 8 * * *` expecting 8 am local fired 8 am
  UTC instead**. Creating a task via MCP `schedule_task` without
  specifying `cron_timezone` fell through to UTC evaluation — so a
  Taipei user got their "morning" cron at 16:00 local and their
  "evening" cron at 04:00 the *next morning*. New
  `detect_local_timezone()` helper reads the host's IANA name
  (`iana_time_zone::get_timezone()` on Unix / Windows) and
  round-trips it through `duduclaw_core::parse_timezone` to guarantee
  `chrono-tz` acceptance. `handle_schedule_task` now auto-populates
  `cron_timezone` from the detected TZ when absent; explicit
  `cron_timezone='UTC'` still forces UTC (opt-out), any explicit IANA
  name still wins. Logs the detected zone at info level for
  observability. `cron_timezone` tool schema description updated to
  reflect the new auto-detect default. New direct dep
  `iana-time-zone = "0.1"` on `duduclaw-cli` (already a transitive
  dep of `chrono`, no new vendored C). New test
  `detect_local_timezone_returns_valid_iana_name` asserts
  parse_timezone round-trip and tolerates None on hosts with no
  discoverable TZ (minimal Docker images).
- **Cron agents' nested `send_to_agent` replies silently dropped
  (same class as v1.8.16 but for cron-initiated chains)**. The cron
  scheduler dispatched tasks via `call_claude_for_agent_with_type`
  wrapped only in `DELEGATION_ENV.scope` — never in
  `REPLY_CHANNEL.scope`. So when a daily-report agent called
  `send_to_agent("agnes", "here's my report")`, no
  `delegation_callbacks` row was ever registered (MCP's
  `send_to_agent` only inserts callbacks when
  `DUDUCLAW_REPLY_CHANNEL` env is set). Agnes's response landed in
  `message_queue.response` and was then dropped at
  `forward_delegation_response`'s no-callback silent-return branch.
  Fix: `run_task` now wraps the dispatch future in
  `REPLY_CHANNEL.scope(cron_reply_channel_string(task), …)` when
  the task has a `notify_channel` target. New helper
  `cron_reply_channel_string` builds the
  `<channel_type>:<chat_id>[:<thread_id>]` grammar that
  `mcp.rs::send_to_agent` parses; Discord threads stored as
  `chat_id=<thread_id>, thread_id=NULL` emit `discord:<thread_id>`
  (matching `deliver_cron_result`'s existing API-level "thread is
  a channel" semantics). Effect: nested cron delegations now
  register callbacks → forward through v1.8.20 token cascade →
  session-append via v1.8.24 chain-root cascade. The cron agent's
  own top-level response still goes through `deliver_cron_result`
  (direct POST) unchanged; this patch strictly closes the nested
  path. 5 new tests in `cron_scheduler::tests` covering None /
  Discord thread-as-chat-id / Discord parent+thread / Telegram
  without thread / Telegram forum topic thread.



## [1.8.24] - 2026-04-22

### Fixed
- **Sub-agent replies disappeared from the root agent's session on
  nested delegations (chain-root session-append gap)**. v1.8.17 Fix 2
  wrote an XML-delimited `<subagent_reply agent="X">` turn into the
  parent agent's session, but only when the session owner matched
  `callback.agent_id` — a deliberate cross-agent-bleed guard. The
  unintended side effect: sub-agents spawned by the dispatcher (TL,
  eng-agent, eng-infra, marketing, …) don't have their own sessions in
  `sessions.db` — only agnes does. So when eng-agent replied to TL's
  `send_to_agent` call, the owner-mismatch skip fired:
  `callback.agent_id=duduclaw-tl` vs `session owner=agnes` → warn +
  silent drop → agnes's next turn had no record of the engineer's
  output → root agent couldn't synthesise the chain's total work.
  Fix: same cascade pattern as v1.8.20 token resolution.
  `append_subagent_reply_to_parent_session` now takes
  `chain_root_agent: Option<&str>` and accepts an owner match at
  either tier. Tier 1 (parent direct) uses the existing
  `<subagent_reply agent="X">` grammar. Tier 2 (chain root)
  writes `<subagent_reply agent="X" via="Y">` where Y is the
  callback agent — the `via=` attribute lets the root LLM tell a
  direct reply apart from one relayed via a sub-agent. Tier 3
  (neither match) still skips, so the cross-agent-bleed guard
  holds. `forward_delegation_response` already computed the
  chain root for v1.8.20's token cascade; just wires it down.
  `safe_agent_tag` helper factored out so direct and relayed
  content share the same `[A-Za-z0-9_-]` sanitisation. 4 new
  regression tests in `dispatcher::tests`
  (`append_cascades_to_chain_root_when_parent_has_no_session`,
  `cascade_appends_via_annotation`,
  `cascade_does_not_override_direct_parent_match`,
  `cascade_skipped_when_neither_parent_nor_root_owns_session`).
  Sub-agents still don't get their own persistent sessions —
  session-per-agent-per-chain remains a separate, larger design
  decision.



## [1.8.23] - 2026-04-22

### Added
- **Timezone-aware cron evaluation (#16 Level 2)**. Both the heartbeat
  scheduler and the per-task cron scheduler now honour a new
  `cron_timezone` field. Setting it to an IANA name
  (e.g. `"Asia/Taipei"`) lets the user write cron expressions in their
  wall clock and have the scheduler do the UTC conversion —
  `"0 9 * * *"` with `cron_timezone = "Asia/Taipei"` now actually fires
  at 09:00 Taipei every day. Empty / absent `cron_timezone` preserves
  the pre-v1.8.23 UTC behaviour, so nothing moves for existing
  deployments. The field lives on `HeartbeatConfig` (agent.toml
  `[heartbeat]`) and on `cron_tasks` DB rows (accepted by MCP
  `schedule_task` and dashboard `cron_add` / `cron_update`). A shared
  `duduclaw_core::should_fire_in_tz` makes both schedulers use
  identical evaluation semantics. Typos are caught at call time in the
  MCP tool and dashboard handlers (IANA validation via `chrono-tz`),
  so a bad zone name surfaces as an error instead of silently firing
  in UTC. If a bad name does reach the scheduler somehow, it logs a
  single warn line at load time and falls back to UTC — the cron
  keeps firing instead of going silent. DB migration is idempotent
  `ALTER TABLE`: reopening a v1.8.22 database adds the column with all
  existing rows inheriting `NULL` (= UTC). Documented in all 5
  `templates/*/agent.toml` and in the dashboard cron-input hint.
  18 new tests across `duduclaw-core` (8: Taipei, New York EDT, UTC
  fallback, invalid names, `*/5` tz-invariance, trimming), agent
  heartbeat (5: tz set / empty / invalid / disabled, next_fire UTC
  instant), and cron_store (5 including a `cron_timezone` roundtrip
  + `update_cron_timezone` clearing, and migration idempotency across
  reopen).


## [1.8.22] - 2026-04-21

### Fixed
- **Proactive check could not use the agent's MCP tools (#14)**.
  `heartbeat.rs`'s proactive spawn hard-coded
  `--print --no-input --system-prompt --max-turns 3` without
  `--mcp-config`. Two breakages stacked: Claude CLI ≥2.1 removed
  `--no-input` (so the spawn hard-errored on the current CLI), and
  the missing `--mcp-config` meant any PROACTIVE.md that said "query
  Notion for open tasks" silently no-opped — the sub-agent could not
  see the tool. Rewritten to mirror `spawn_claude_cli_with_env`:
  system prompt via `--system-prompt-file` (no `/proc/PID/cmdline`
  exposure), auto-attach `<agent_dir>/.mcp.json` with
  `--strict-mcp-config` when present, and `--max-turns` now reads
  from a new `ProactiveConfig.max_turns` field (default 8, clamped
  1–64) so checks that chain multiple tool calls have headroom.
- **Cron task results never reached the chat channel (#15)**.
  `cron_scheduler::execute_cron_task` only called `record_run` +
  hallucination audit; the response text lived in the DB only, and
  any prompt asking the agent to "send to Discord via send_message"
  silently failed because `call_claude_for_agent_with_type` does not
  attach MCP. Users were wrapping cron jobs in external shell scripts
  that called Discord/Notion APIs directly. Fix adds row-level
  routing: three new columns on `cron_tasks`
  (`notify_channel` / `notify_chat_id` / `notify_thread_id`, all
  `TEXT NULL`, idempotent `ALTER TABLE` migration that tolerates
  "duplicate column name" so reopening a v1.8.21 DB is safe). New
  `deliver_cron_result` resolves the bot token through the same
  cascade the dispatcher uses (per-agent `agent.toml [channels.<ch>]`
  encrypted or plaintext → global `config.toml [channels]`), clamps
  the response to 3500 chars (Discord's 2000-char cap is the tightest;
  CJK-safe codepoint count), prefixes with a task-name header, and
  calls the unified `ChannelSender`. Discord thread routing uses
  `notify_thread_id` as the effective chat_id. Delivery failures log
  but never flip `record_run` — the agent did its work, only the
  postage failed. `CronTaskRow::has_notify_target()` gates delivery
  so legacy rows without notify columns stay completely silent. MCP
  `schedule_task` and dashboard `cron_add` / `cron_update` both
  accept the three new optional params with symmetric validation
  ("both or neither" for channel + chat_id). Two new tests cover
  round-trip + `update_notify` clearing, and the reopen-the-DB
  migration idempotency contract.

### Documented
- **`[heartbeat] cron` is UTC — was not documented (#16 Level 1)**.
  `heartbeat.rs:251` and `cron_scheduler.rs:151` both call
  `chrono::Utc::now()`, and `ProactiveConfig.timezone` only affects
  `quiet_hours_*` — not the cron evaluation. Taipei (UTC+8) users
  writing `"0 9 * * *"` expecting 09:00 local actually got 17:00.
  Added comments to all 5 `templates/*/agent.toml` heartbeat blocks
  with the Asia/Taipei mapping (`"0 1 * * *"` → local 09:00),
  expanded the `HeartbeatConfig` doc-comment, clarified on
  `ProactiveConfig.timezone` that it is quiet-hours-only, added the
  same UTC caveat to the MCP `schedule_task` tool description and
  the dashboard `SettingsPage` cron-input hint. Timezone-aware cron
  evaluation (Level 2 — reading `cron_timezone` on the task row) is
  planned for a later release; this change is documentation-only so
  no behaviour change for existing crons.


## [1.8.21] - 2026-04-21

### Added
- **`duduclaw reforward <message_id> [--dry-run]`** — manual unstuck
  lever for completed delegations whose forward failed and got retry-
  queued. Before v1.8.20, nested sub-agent forwards to Discord threads
  hit 401 Unauthorized because token lookup didn't cascade to the
  chain-root agent; v1.8.20 fixes that going forward, but
  already-completed messages were stuck — the dispatcher only retries
  when a new `agent_response` arrives for the same message_id, which
  never happens for a message that's already `done`. The callback row
  ages out to 24h cleanup and the user loses the reply. New command:
  reads `message_queue.db` by id (requires `status='done'` and
  non-empty response), uses the existing `delegation_callbacks` row
  if present, synthesizes one from the stored `reply_channel` column
  if missing (`INSERT OR REPLACE` for idempotency across re-runs),
  then delegates to `forward_delegation_response` which uses the
  v1.8.20 token cascade and v1.8.17 Fix 2 session append. Reports
  `Sent` / `DryRun` / `Failed` with friendly output; exit 1 on error.
  New `pub async fn reforward_message` + `pub enum ReforwardOutcome`
  in `duduclaw_gateway::dispatcher` for library reuse. 9 new
  regression tests covering dry-run paths, error cases
  (pending / missing / empty response / no channel context), and the
  `parse_reply_channel` grammar incl. the `discord:thread:<id>`
  collapse rule. Production-verified: recovered message
  `78fbcfc8-735b-4053-9ee0-a03543fd904f` (a marketing report that had
  been stuck since 12:35 UTC) delivered to its Discord thread.



## [1.8.20] - 2026-04-21

### Fixed
- **Nested sub-agent forwards to Discord threads got 401
  Unauthorized when only the chain-root agent had a per-agent bot
  token**. Production-observed on v1.8.19 (message
  `78fbcfc8-735b-4053-9ee0-a03543fd904f`, TL→marketing depth=2 — the
  marketing agent finished the report, response text in DB, but the
  HTTP POST to Discord thread `1496095418805780591` returned 401).
  `forward_to_channel`'s token lookup cascaded from `callback.agent_id`
  (the `send_to_agent` caller, e.g. `duduclaw-tl` — no per-agent bot
  configured) straight to the global `config.toml` token, skipping
  the chain-root agent (agnes) who actually owned the bot that
  opened the thread. Discord threads are scoped to the bot that
  opened them (v1.8.14 already documented this), so the 401 loop
  was inevitable for any nested delegation whose immediate caller
  lacked its own bot. New `resolve_forward_token` helper cascades
  three tiers: (1) callback agent's own token → (2) chain-root
  agent's token (looked up from `message_queue.origin_agent` via
  new `lookup_origin_agent`) → (3) global config token. The four
  channel arms (telegram/line/discord/slack) in `forward_to_channel`
  all route through the helper so the cascade applies uniformly,
  though only Discord's thread-bot scoping actually triggered the
  production failure. Handles the `origin_agent == callback_agent`
  self-loop, missing `message_queue.db`, and NULL
  `origin_agent` column cleanly. 7 new regression tests in
  `dispatcher::tests`, including
  `resolve_token_cascades_to_chain_root_when_callback_agent_has_none`
  that replays the production scenario.



## [1.8.19] - 2026-04-21

### Fixed
- **`Failed to initialize inference engine: Backend unavailable:
  llama.cpp` WARN flood**. When an agent's `[model.local]` had
  `use_router = true` but the gateway binary was built without
  `--features metal`/`cuda`/`vulkan` (the default for the
  npm-distributed binary to avoid pulling libclang + cmake into the
  release build), every single request ran the local-offload path,
  hit `InferenceEngine::init`, got `BackendUnavailable`, warned, fell
  back to SDK, and repeated next request. Functionally harmless — the
  fallback always worked — but drowned real warnings and wasted
  ~100ms per request on a doomed init attempt. Added a process-
  lifetime `AtomicBool` negative cache next to the existing
  `INFERENCE_ENGINE` singleton in `claude_runner.rs`: on the first
  failed `init` (or first successful init that still reports no
  available backend), the flag latches to `true` and every subsequent
  `get_inference_engine` short-circuits to `None` silently. The WARN
  is now one-shot per gateway process, with an actionable hint on how
  to enable a backend (rebuild with `--features metal/cuda/vulkan`, or
  configure `[openai_compat]` in `inference.toml` for a remote
  backend). A gateway restart resets the cache — which is also when
  operators would have rebuilt the binary, so the trade-off aligns.



## [1.8.18] - 2026-04-21

### Fixed
- **Dual-rail dispatch race silently defeated v1.8.16's reply_channel
  propagation**. Production-observed on a live v1.8.17 chain (agnes →
  TL → [eng-agent + eng-infra]): TL's outgoing delegations to the two
  eng-agents had `reply_channel=NULL` in `message_queue.db` even
  though `DUDUCLAW_REPLY_CHANNEL` was scoped correctly in the
  dispatcher. Effect: when eng-agent replied, no callback was
  registered, the forward lookup silently skipped, and the engineer's
  output never reached TL's session. `DUDUCLAW_DELEGATION_DEPTH`
  still propagated correctly in the same chain — the "half-propagated"
  pattern (correct depth + NULL reply_channel) was the telltale.
  Root cause: `mcp.rs::send_to_agent` was dual-writing every
  delegation to both `bus_queue.jsonl` (legacy) and
  `message_queue.db` (SQLite, authoritative since v1.8.1).
  The gateway's dispatcher polled both every 5 seconds:
  `poll_and_dispatch` (legacy) `tokio::spawn`'s a per-message
  dispatch task, which drops task-local `REPLY_CHANNEL` at the
  spawn boundary; `poll_and_dispatch_sqlite` (v1.8.16) scopes
  `REPLY_CHANNEL` correctly. Whichever side reached
  `prepare_claude_cmd` first determined whether
  `DUDUCLAW_REPLY_CHANNEL` was set on the target's Claude CLI
  subprocess. `DELEGATION_ENV.scope` nested INSIDE `dispatch_to_agent`
  applies to both paths equally, explaining why depth propagated but
  reply_channel didn't. Fix: removed the `bus_queue.jsonl` write from
  `send_to_agent`. SQLite has been the authoritative rail since
  v1.8.1 — the jsonl write was dead weight kept around for migration
  safety and, by causing the race, actively defeating the v1.8.16
  fix. `queued` flag now derives from the SQLite INSERT rowcount
  (v1.8.16 schema-downgrade fallback preserved). `poll_and_dispatch`
  (legacy) is left untouched; it still handles `task_created`
  signals and orphan-response recovery, both of which use separate
  writers not affected by this change. New
  `mcp::tests::send_to_agent_never_writes_bus_queue_jsonl`
  regression guard. Two existing E2E tests
  (`e2e_send_to_agent_increments_depth`,
  `e2e_depth_zero_defaults_origin_to_caller`) migrated from reading
  `bus_queue.jsonl` to `message_queue.db`.



## [1.8.17] - 2026-04-21

### Fixed
- **MCP server used the global `default_agent` as caller identity,
  silently breaking supervisor-relation authorization for every
  sub-agent**. `mcp.rs::get_default_agent` read `config.toml [general]
  default_agent` (typically the top-level `agnes`) regardless of which
  agent's Claude CLI actually spawned the MCP subprocess. When
  `duduclaw-tl` called `send_to_agent("duduclaw-eng-agent", …)`, the
  supervisor check asked "is agnes the parent of duduclaw-eng-agent?",
  saw `reports_to=duduclaw-tl`, and rejected the call as a pattern
  violation — even though the delegation was correct. The TL agent's
  own Discord message diagnosed this accurately ("MCP Server 在驗證
  委派權限時，仍以發起 Session 的身份（agnes）作為呼叫者") and
  proposed `方案 B: 由我代替產出` as a workaround — improvising around
  the bug instead of the system enforcing the correct chain. New
  `duduclaw_core::ENV_AGENT_ID = "DUDUCLAW_AGENT_ID"`;
  `mcp.rs::get_default_agent` preference order is now env var → config
  `default_agent` → `"dudu"`. `duduclaw-agent::mcp_template::
  ensure_duduclaw_absolute_path` (called from `server.rs:344` on
  gateway startup) injects `{ "DUDUCLAW_AGENT_ID": "<agent-dir-name>" }`
  into each agent's `.mcp.json` `env` block — preserving other env
  vars, preserving other `mcpServers` entries (playwright,
  browserbase), handling legacy `duduclaw-pro` key, idempotent on
  repeated calls. Empty string falls through to config to avoid
  lockout on botched migrations. After this: `agnes → duduclaw-tl`
  still allowed, `duduclaw-tl → duduclaw-eng-agent` now allowed,
  `agnes → duduclaw-eng-agent` correctly rejected.
- **Sub-agent replies never reached the parent agent's session,
  breaking conversation continuity across delegations**.
  `forward_delegation_response` delivered a sub-agent's reply to the
  originating channel (Discord/Telegram/LINE/Slack) and stopped.
  Parent agents had no record in their SQLite session of what the
  sub-agent said, so the next user turn replying to the parent
  referenced content the parent couldn't see. Production-observed
  symptom (Discord 2026-04-21 07:24): TL replied with "方案 A/B/C",
  user said "@Agnes 方案A", Agnes's next invocation had no trace of
  A/B/C and asked the user to disambiguate between Fabric / Besu /
  PoA (from an earlier unrelated branch). Fix: after
  `forward_to_channel(...)` returns `Ok(())`,
  `forward_delegation_response` appends a single assistant-role turn
  to the parent's session with XML-delimited content
  `<subagent_reply agent="X">...</subagent_reply>` (same grammar as
  `channel_reply::format_history_as_prompt`). Agent name sanitised
  to `[A-Za-z0-9_-]`. Token count uses the CJK-aware estimator.
  `sessions.total_tokens` + `last_active` updated in the same
  transaction. New `candidate_session_ids` tries both
  `discord:thread:<id>` and `discord:<id>` forms (the `thread:`
  marker was collapsed in `mcp.rs::send_to_agent` callback insert)
  and matches by `owner_agent` to prevent cross-agent bleed on
  shared channels. Session store errors are swallowed at warn level —
  Discord delivery already succeeded, dropping the session append is
  strictly better than losing the forward. Append happens only on
  HTTP success, so retry loops don't double-append.



## [1.8.16] - 2026-04-21

### Fixed
- **Nested sub-agent replies silently dropped at delegation depth ≥ 2**.
  A user-visible chain like `agnes → duduclaw-tl → [eng-agent +
  eng-infra] → synthesis` would deliver the first-level "dispatch
  confirmation" (depth=1, from `channel_reply`), complete all three
  sub-agent messages in `message_queue.db` with status=`done`, but
  never forward the status update (depth=2) nor the 16 KB final
  synthesis (depth=3) to the originating Discord channel — no WARN,
  no error, just silence. Root cause: MCP `send_to_agent` only
  registers a `delegation_callbacks` row when `DUDUCLAW_REPLY_CHANNEL`
  is set in env, which `channel_reply::REPLY_CHANNEL.scope()` does for
  inbound channel messages but `dispatcher::dispatch_to_agent` did
  NOT, so nested sub-agent processes had no channel context, their
  callback rows were never inserted, and `forward_delegation_response`
  took its no-callback silent-return branch. Fix propagates channel
  context through the chain: (1) `message_queue` gains a
  `reply_channel TEXT` column with idempotent `PRAGMA table_info` +
  `ALTER TABLE ADD COLUMN` migration; (2) MCP `send_to_agent` captures
  `DUDUCLAW_REPLY_CHANNEL` from env on INSERT, with a schema-downgrade
  fallback for the cross-process race on first v1.8.16 boot; (3)
  `dispatcher::dispatch_to_agent` now wraps the dispatch future in
  `claude_runner::REPLY_CHANNEL.scope(msg.reply_channel, ...)` when
  the row carries channel context, so the spawned Claude CLI
  subprocess inherits the env var and its own nested `send_to_agent`
  calls register callbacks correctly. Chain propagation is automatic:
  depth-1's row stores discord:..., depth-2 inherits via env during
  dispatch and writes it back to its own row, depth-3 does the same.
- **`forward_delegation_response` no-callback path was fully silent**,
  making the above bug invisible in logs. Added
  `tracing::debug!` so future drops surface under
  `RUST_LOG=duduclaw_gateway::dispatcher=debug` with the message-id +
  responder agent. Still expected-and-benign for cron / reminder /
  non-channel delegations; unexpected for user-facing sub-agent
  replies.



## [1.8.15] - 2026-04-21

### Fixed
- **Discord global `[discord]` 401 noise at gateway startup**. The
  global `config.toml [channels] discord_bot_token_enc` was eagerly
  validated on startup via `GET /users/@me`, printing a warn-level
  "token invalid (HTTP 401)" even when per-agent Discord tokens (the
  authoritative source since v1.8.14) were live and serving traffic.
  Users who migrated to per-agent tokens saw a scary warning that
  implied Discord was broken when it wasn't. `start_discord_bots` now
  collects per-agent tokens first and passes a `quiet_on_auth_failure`
  flag to `spawn_discord_bot`; a 401/403 on the global token when at
  least one per-agent token exists is logged at info level with an
  explicit note. A 401 with no per-agent fallback still warns.
- **GVU proposals on tiny SOUL.md baselines were always rejected as
  CRITICAL drift**. With a ~400-char baseline (e.g. the default agnes
  template), every evolution `append` made `compute_asi`'s 0.40-
  weighted char-bigram content similarity collapse to ~0.06 and trip
  the 0.50 critical threshold deterministically. Not a drift problem —
  a baseline-size problem. Added
  `duduclaw_security::stability_index::AsiConfig::bootstrap()`
  (content 0.40 → 0.20, semantic 0.30 → 0.45, critical 0.50 → 0.25)
  and `AsiConfig::for_baseline_size(bytes)` which dispatches to
  bootstrap when `bytes < 1024`, default otherwise. The updater now
  calls `for_baseline_size(current_content.len())` so agents with
  richer SOUL.md files still face the strict default threshold.
- **Claude CLI `--resume` was permanently unreachable — wasting 1
  extra CLI spawn per multi-turn conversation**. v1.8.1 introduced
  native multi-turn via `--resume <dd-{hex16}>` with a SHA-256 session
  ID. Claude CLI strictly requires either a canonical UUID or an
  exact session title match — `dd-5d8a35f9dba3408e` is neither, so
  the first `--resume` attempt was rejected 100% of the time before
  the `is_session_error`-guarded fallback retried with history-in-
  prompt (the only path that actually worked). Every multi-turn
  reply paid one wasted CLI spawn + startup latency + warn-level log
  line. `call_claude_cli_rotated` no longer attempts `--resume`:
  when conversation history exists, it is folded into the prompt via
  `format_history_as_prompt` and Claude CLI is spawned once. The
  `session_id` parameter is kept as `_session_id` for call-site
  compatibility. Removed dead `make_claude_session_id` and
  `is_session_error` helpers plus their 3 tests.



## [1.8.14] - 2026-04-21

### Fixed
- **Discord thread session id drifted across turns**. `auto_thread &&
  !is_thread` in the session-id formatter was only true on the first
  turn (when a thread was about to be created) — every follow-up turn
  the user typed inside the thread flipped `is_thread` to true and the
  session id silently switched from `discord:thread:{id}` to
  `discord:{id}`, loading a fresh empty session and losing all context.
  Condition is now `is_thread || created_thread` so a thread-scoped
  conversation keeps one session id for its entire lifetime. Also
  handles the edge case where `create_thread()` fails (returns
  `discord:{channel_id}` instead of a misleading `discord:thread:...`).
- **Sub-agent replies stuck in bus_queue.jsonl**. Three layered bugs
  prevented `send_to_agent` → sub-agent → user round-trips from ever
  completing:
  1. The `delegation_callbacks` parser split `<channel>:thread:<id>`
     by `:` and stored the literal string "thread" as `channel_id`;
     downstream `validate_channel_id` rejected it as non-numeric, so
     forwarding retry-looped forever. Parser now recognises the
     `<type>:thread:<id>` marker and stores `channel_id=<id>,
     thread_id=None`.
  2. `forward_to_channel` only ran immediately after a live dispatch;
     orphan `agent_response` entries left on disk after a crash /
     Ctrl+C / hotswap were never replayed. New
     `reconcile_orphan_responses` scans `bus_queue.jsonl` on
     dispatcher startup and atomically replays every callback whose
     row is still pending.
  3. Discord / Telegram / LINE / Slack arms read the global
     `[channels] <type>_bot_token` from config.toml. Discord threads
     are scoped to the bot that opened them — a different bot returns
     401 Unauthorized even in the same guild. New
     `get_agent_channel_token` reads the originating agent's per-agent
     token from `agents/<id>/agent.toml [channels.<type>] bot_token_enc`
     first, falling back to the global token only when the agent has
     none.
- **Long sub-agent replies silently truncated**. `forward_to_channel`
  capped responses at the channel byte limit and appended
  `_(回應過長，已截斷)_`, dropping most TL/PM report content. Rewritten
  to use the existing `channel_format::split_text` (paragraph/line
  aligned, UTF-8 safe) emitting chunks labelled
  `📨 **agent** 的回報 (1/N)` / `(續 2/N)`, each sized under the
  channel's byte budget (Discord 1900, Telegram 4000, LINE 4900, Slack
  3900) with a 250ms inter-chunk gap to stay within API rate limits.

### Changed
- **Default log level is now `warn`** when `RUST_LOG` is unset.
  Previous default (`EnvFilter::from_default_env()` with no fallback)
  dropped every log unless the user explicitly set `RUST_LOG`, which
  made issues like "401 on delegation forward" undiagnosable from the
  terminal and left `~/.duduclaw/logs/gateway.log` at 0 bytes. `warn`
  keeps the terminal quiet for end users while still surfacing real
  problems; run `RUST_LOG=info duduclaw run` for the verbose
  dispatcher / WebSocket / heartbeat trace when debugging.



## [1.8.13] - 2026-04-20

### Added
- **Memory page Key Insights tab**. The agent-local `memory.db` →
  `memories` table is populated by the prediction engine with
  satisfaction-error deltas ("Prediction deviation: expected 0.70,
  inferred 0.42 ..."), not conversational content — so the previous
  Memory tab looked empty / unhelpful on a running system. The real
  extracted insights live in the `key_facts` table (P2 Key-Fact
  Accumulator), which had zero dashboard exposure. New RPC
  `memory.key_facts(agent_id, limit)` queries that table directly
  and the Memory page now has a 4th tab "關鍵洞察 / Key Insights /
  主要インサイト" rendering each fact as a card with `access_count`
  badge, timestamp, and collapsible source metadata.
- **Unified multi-source audit log on the Logs page**. Previously
  `security.audit_log` read only `security_audit.jsonl` (rarely
  written), so the history panel showed "暫無審計事件" on systems
  with dozens of real tool calls. New RPC `audit.unified_log(params)`
  merges four JSONL sources (`security_audit.jsonl`,
  `tool_calls.jsonl`, `channel_failures.jsonl`, `feedback.jsonl`)
  into a common envelope — `timestamp` / `source` / `event_type` /
  `agent_id` / `severity` / `summary` / `details` — sorted
  newest-first, with per-source counts returned alongside. Severity
  rules: tool_call success=info, failure=warning,
  channel_failure=warning, feedback=info, security preserves its
  original severity. Missing files and malformed JSONL lines are
  tolerated silently. Summary truncation goes through
  `duduclaw_core::truncate_bytes` (CJK-safe).
- **Logs page history tab rewrite**. Source filter chips
  (全部 / 安全 / 工具呼叫 / 通道失敗 / 回饋) with live per-source
  counts, severity dropdown, severity-colored left borders
  (emerald / amber / rose), click-to-expand pretty-printed detail
  JSON. Realtime tab untouched. `handle_security_audit_log` is
  preserved intact for backward compatibility.



## [1.8.12] - 2026-04-20

### Fixed
- **Opaque `claude CLI stream error: Unknown stream-json error`** now
  carries the captured Claude CLI stderr tail (`| stderr: ...`, 500
  bytes max). When Claude CLI emits `is_error: true` on a `result`
  event with no `result` string, the caller previously got no
  actionable detail; now the real reason (stale `--resume` handle,
  internal CLI error, etc.) is surfaced in both the debug log and
  the rotator's error history.
- **Auto-fallback on generic `--resume` failures**. `is_session_error`
  now also matches "unknown stream-json error", so when Claude CLI
  can't spell out why `--resume` failed the caller retries once with
  the session history folded into the prompt. Worst case one extra
  turn of cost; best case the user gets a reply instead of an opaque
  error.
- **`schedule_task` MCP tool schema was missing `agent_id` and `name`**.
  The handler reads both (plus `task` / `prompt` / `description` as
  synonyms) but the declared `ParamDef` list exposed only `cron` and
  `description`. From the agent's point of view the tool looked half-
  built, so Agnes fell back to Claude Code's session-bound
  `/schedule` slash command (7-day auto-expiry) instead of DuDuClaw's
  persistent `CronScheduler`. Schema now lists `cron`, `task`, `name`
  (all required), and `agent_id` (optional, strongly recommended),
  and the description explicitly states the tool is persistent
  (`~/.duduclaw/cron_tasks.db`), survives restarts, and should be
  preferred over `/schedule`.



## [1.8.11] - 2026-04-20

### Fixed
- **Claude CLI `--bare` broke OAuth authentication** (Claude CLI
  2.1.110 regression). The flag was added to
  `spawn_claude_cli_with_env` for ~15-25% latency reduction by
  skipping hooks / LSP / plugin sync / CLAUDE.md auto-discovery, but
  also disabled OS-keychain credential lookup, causing every channel
  subprocess call to fail with "Not logged in · Please run /login"
  even when `claude auth status` confirmed a valid session. Removed
  from both `call_claude_cli_rotated` and `call_claude_cli_lightweight`
  paths.
- **CJK / emoji byte-index string slicing panicked tokio workers**.
  `s[..s.len().min(N)]` slices by byte, not by char, so any multi-byte
  codepoint straddling byte N (e.g. `學` = 3 bytes) triggered "byte
  index N is not a char boundary" panics that crashed reply dispatch
  silently. The pattern was copy-pasted across 31 sites in 16 files
  (Feishu, WhatsApp, LINE, Slack, Telegram, Discord, TTS, direct_api,
  handlers, dispatcher, tool_classifier, gvu/loop_, cli/mcp,
  cli/acp/handlers, runtime/openai_compat, computer_use, webchat,
  channel_reply).

### Added
- **`duduclaw_core::truncate_bytes` / `truncate_chars`** (new
  `duduclaw-core/src/text_utils.rs` module). `truncate_bytes` returns
  a `&str` sliced at the nearest UTF-8 char boundary ≤ the requested
  byte budget — a panic-safe drop-in for `&s[..N]`. `truncate_chars`
  counts codepoints. Six unit tests cover ASCII, mid-CJK, zero-budget,
  and emoji (4-byte) cases. Every unsafe byte-index slice on a
  user-text / LLM-text / HTTP-body string was migrated.



## [1.8.10] - 2026-04-20

### Added
- **`marketplace.list` RPC** serving the real built-in MCP catalog
  (Playwright, Browserbase, Filesystem, GitHub, Slack, Postgres,
  SQLite, Memory, Fetch, Brave Search) enriched with `author`,
  `tags`, and `featured` fields. Merges optional user entries from
  `~/.duduclaw/marketplace.json` without a rebuild.
- **Partner data model**: new SQLite-backed `PartnerStore`
  (`~/.duduclaw/partner.db`) with profile + customer CRUD and
  computed sales stats. Seven RPCs (`partner.profile`, `partner.stats`,
  `partner.customers`, `partner.profile.update`,
  `partner.customer.add`, `partner.customer.update`,
  `partner.customer.delete`) and 4 unit tests.
- **Toast notification system** (`web/src/components/Toast.tsx` +
  `web/src/lib/toast.ts`): module-scoped event bus, max-5 queue,
  auto-dismiss, warm stone/amber/emerald/rose variants,
  `prefers-reduced-motion` honored.
- **`cron.resume`** wired to a Resume button alongside Pause in the
  Settings cron task list.
- **SOUL.md evolution history UI** in Memory → Evolution tab with
  pre/post metric deltas (positive feedback, prediction error, user
  corrections) and status badges (Confirmed / RolledBack / Observing).

### Changed
- **`evolution.status`** returns real aggregate data
  (`enabled`/`mode`/`total_agents`/`gvu_enabled_count`/
  `total_versions`/`last_applied_at`) instead of hardcoded
  `{enabled: true, mode: "prediction_driven"}`.
- **`activity.subscribe`** returns honest metadata
  (`broadcast_mode: "all_events"` + note) — previously a bare stub.
  Per-topic filtering is not implemented; all authenticated WS
  clients receive all activity events.
- **ChannelsPage setup guides**: 42 hardcoded zh-TW strings extracted
  to i18n across Telegram / LINE / Discord / Slack / WhatsApp /
  Feishu in zh-TW / en / ja-JP.
- **MarketplacePage** loads from the real RPC; fake stars/prices and
  the 8-item `MOCK_SERVERS` constant removed. Category-based icon
  mapping (browser / data / communication).
- **PartnerPortalPage** rewired to real RPCs; mock constants
  (`PARTNER_STATUS`, `SALES_STATS`, `MOCK_CUSTOMERS`) and the
  preview banner removed. Added onboarding card (empty-profile
  state) and Add Customer modal.
- Inline error feedback added to MarketplacePage install,
  PartnerPortalPage license generation, and ApprovalModal WS
  response failures (previously all silently swallowed).

### Removed
- **`activity.unsubscribe`** RPC (backend dispatch arm and frontend
  method) — broadcasts cannot be stopped without closing the WS
  itself, so the RPC was dead.
- **`evolution.skills`** handler — fully redundant with
  `skills.list`, which returns richer per-agent + global structure.

### Fixed
- 23 silent `console.warn("[api]", e)` catches across DashboardPage,
  ReportPage, BillingPage, SkillMarketPage, SettingsPage, MemoryPage,
  AgentsPage, ChannelsPage, and KnowledgeHubPage now surface errors
  to users via toast while preserving devtools visibility.



## [1.8.9] - 2026-04-20

### Added
- **Wiki knowledge layer system** (Vault-for-LLM inspired): 4-layer
  architecture (L0 Identity / L1 Core / L2 Context / L3 Deep) with
  `layer` and `trust` (0.0-1.0) frontmatter fields. Search results
  ranked by trust-weighted score. Backward-compatible defaults for
  existing pages.
- **Wiki system prompt injection**: `build_system_prompt()` now
  auto-injects L0+L1 wiki pages into the WIKI_CONTEXT module.
  Agents automatically reference their accumulated knowledge without
  manual `wiki_search` calls.
- **FTS5 full-text index**: `WikiFts` SQLite-backed index with
  `unicode61` tokenizer for CJK support. Auto-syncs on every
  `write_page` / `delete_page`. Manual rebuild via `wiki_rebuild_fts`
  MCP tool.
- **Wiki dedup detection**: `wiki_dedup` MCP tool detects duplicate
  pages by title match and tag Jaccard similarity (>= 0.8).
- **Wiki knowledge graph**: `wiki_graph` MCP tool exports Mermaid
  diagrams with BFS-limited center+depth focused view. Node shapes
  vary by knowledge layer.
- **Wiki search filters**: `wiki_search` / `shared_wiki_search` now
  support `min_trust`, `layer`, and `expand` (1-hop related/backlink
  expansion) parameters.
- **Reverse backlink index**: `build_backlink_index()` scans
  `related` frontmatter + body markdown links for bidirectional
  mapping.
- **Layer-aware context injection**: `build_injection_context()` +
  `collect_by_layer()` for system prompt budget-aware injection.
- **CLAUDE_WIKI.md template**: Now included in agent CLAUDE.md on
  creation, providing wiki MCP tool usage guide to Claude Code.
- **A2A stdio JSON-RPC server** (`acp::server::run_acp_server`):
  `duduclaw acp-server` is now functional (previously a stub). Runs a
  line-delimited JSON-RPC 2.0 loop on stdin/stdout with
  `agent/discover`, `tasks/send`, `tasks/get`, `tasks/cancel`
  methods, backed by the `A2ATaskManager`. Enables Zed / JetBrains /
  Neovim IDE integration via the Agent Client Protocol.
- **Behavioral contract injection**: `AgentRegistry` now loads
  `CONTRACT.toml` into `LoadedAgent.contract`. `must_not` /
  `must_always` rules are rendered as a CONTRACT module in the
  system prompt, giving every runtime (Claude / Codex / Gemini)
  consistent behavioral boundaries.
- **Memory decay daily scheduler**: Gateway spawns a background
  task that runs `duduclaw_memory::decay::run_decay` every 24h,
  archiving low-importance entries older than 30 days and
  permanently deleting archived entries older than 90 days.
- **Dashboard WebSocket heartbeat**: Server sends a WebSocket
  `Ping` every 30s and closes idle sockets after 60s without a
  `Pong`. Client sends an application-level `ping` RPC every 25s
  (browsers can't issue control frames). New `ping` method on the
  gateway method handler returns `{pong:true}`.
- **`/metrics` Prometheus endpoint**: New `duduclaw_gateway::metrics`
  module exposed as `GET /metrics` on the gateway HTTP server for
  scraping runtime metrics.
- **RL trajectory collector + CLI**: New
  `duduclaw_gateway::rl::collector` module writes per-agent
  trajectories to `~/.duduclaw/rl_trajectories.jsonl` during
  channel interactions. `duduclaw rl export|stats|reward` is now
  functional (previously stub), including composite reward
  computation (outcome × 0.7 + efficiency × 0.2 + overlong × 0.1).
- **Cognitive memory MCP tools**: `memory_search_by_layer`
  (episodic/semantic filter), `memory_successful_conversations`
  (high-importance episodic recall by topic),
  `memory_episodic_pressure` (observation-density score for
  scheduling Meso reflections), `memory_consolidation_status`
  (count of un-consolidated high-importance episodes).
- **Streaming ASR providers**: `AsrRouter` now accepts
  `Box<dyn StreamingAsrProvider>` (e.g. Deepgram WebSocket) via
  `add_streaming_provider` / `streaming_provider()` for real-time
  transcription alongside existing batch providers.
- **Compression strategy selector**: `compress_text` MCP tool gains
  a `strategy` param — `meta_token` (lossless), `llmlingua` (lossy
  2-5×), `streaming_llm` (window management), or `auto`.
- **Marketplace + Partner Portal dashboard pages**: Wired into
  router and sidebar (manager+ gate for Partner Portal). New
  Browser Automation tab under Settings with ToolApproval,
  SessionReplay, and BrowserAudit panels. `ApprovalModal` mounted
  at app root for synchronous tool approval prompts.

### Changed
- **Cloud ingest prompt**: Now instructs Claude to include `layer`
  and `trust` in extracted wiki page frontmatter.
- **Auto-ingest defaults**: Source pages default to `layer: context,
  trust: 0.4`; entity pages to `layer: deep, trust: 0.3`.
- **Backlink logging**: `write_page()` logs info-level suggestions
  when referenced pages lack reciprocal backlinks.
- **`wiki_search` / `shared_wiki_search` response**: Hits now
  include `weighted_score`, `trust`, and `layer` fields alongside
  the existing `score`.
- **`duduclaw-agent` crate**: Now depends on `duduclaw-memory` to
  build the WIKI_CONTEXT injection module at prompt assembly time.

### Fixed
- **Wiki-to-LLM disconnect (all runtimes)**: Wiki system previously
  accumulated knowledge via channel ingest and GVU evolution but
  never fed it back into LLM system prompts. Now L0+L1 pages are
  auto-injected into ALL three system prompt assembly paths:
  - CLI interactive (`runner.rs` — `WIKI_CONTEXT` module)
  - Channel reply (`channel_reply.rs` — `## Wiki Knowledge` section,
    serves Telegram/LINE/Discord → Claude/Codex/Gemini/OpenAI)
  - Dispatcher/Cron (`claude_runner.rs` — `# Wiki Knowledge` section,
    serves agent-to-agent delegation and scheduled tasks)
- **FTS desync**: FTS index was completely disconnected from write
  operations. Now auto-syncs on every page write/delete.
- **CLAUDE_WIKI template unused**: Template existed but was never
  included in agent CLAUDE.md files.
- **`duduclaw rl` / `duduclaw acp-server` stubs**: Both commands
  previously printed a placeholder and returned; they now execute
  the real collector / JSON-RPC server.


## [1.8.8] - 2026-04-20

### Fixed
- **Lightweight CLI effort level**: Changed from `--effort low` to
  `--effort medium` for instruction/fact extraction tasks. Prevents
  quality degradation in extracted pinned instructions and key facts
  while maintaining cost savings from other lightweight flags.



## [1.8.7] - 2026-04-19

### Added
- **Claude CLI lightweight path**: New `call_claude_cli_lightweight()` for
  single-turn metadata tasks (compression, instruction/fact extraction). Uses
  `--bare --effort low --max-turns 1 --no-session-persistence --tools ""`.
  Estimated 25-40% cost reduction for metadata tasks.

### Changed
- **Claude CLI `--bare` mode**: Main channel reply path now uses `--bare` to
  skip hooks/LSP/plugins/CLAUDE.md discovery (15-25% latency reduction).
- **Claude CLI `--exclude-dynamic-system-prompt-sections`**: Stabilizes system
  prompt across turns for better prompt cache hit rate (10-15% token reduction).
- **Claude CLI `--strict-mcp-config`**: Explicit MCP isolation per agent.
- **Gemini CLI system prompt**: Fixed from non-existent `--system-instruction`
  flag to `GEMINI_SYSTEM_MD` env var (temp file). Added `--approval-mode yolo`
  and conversation history prefix.
- **Codex CLI system prompt**: Fixed from non-existent `--instructions` flag
  to `AGENTS.md` file write. Added conversation history prefix.

### Fixed
- **Gemini runtime**: `--system-instruction` flag doesn't exist in Gemini CLI.
- **Codex runtime**: `--instructions` flag doesn't exist in Codex exec.



## [1.8.6] - 2026-04-19

### Added
- **Instruction Pinning** (P0): First user message → async Haiku extraction of
  core task instructions → stored in `sessions.pinned_instructions` → injected
  at system prompt tail (high-attention position). Survives session compression.
- **Snowball Recap** (P0): Each turn prepends `<task_recap>` with pinned
  instructions to user message. Zero LLM cost, utilizes U-shaped attention tail.
- **Clarification Accumulation**: When agent asks a question and user answers,
  the answer is appended to pinned instructions (capped at 1000 chars).
- **P2 Key-Fact Accumulator**: Lightweight cross-session memory replacing
  MemGPT Core Memory. Extracts 2-4 key facts per substantive turn via Haiku,
  stores in `key_facts` table with FTS5 search, injects top 3 relevant facts
  into system prompt. ~100-150 tokens vs MemGPT's 6,500 (87% reduction).



## [1.8.5] - 2026-04-19

### Fixed
- **MCP tools unavailable in channel reply**: Claude CLI in `-p
  --dangerously-skip-permissions` mode does NOT read global
  `~/.claude/settings.json` MCP servers — only project-level `.mcp.json`.
  Reverted v1.8.4's global migration back to per-agent `.mcp.json` with
  gateway startup auto-creation/fixup for all agents.



## [1.8.4] - 2026-04-19

### Changed
- **Global MCP server registration**: DuDuClaw MCP server (platform tools:
  `send_to_agent`, `list_cron_tasks`, `create_agent`, etc.) is now registered
  in `~/.claude/settings.json` (global) instead of per-agent `.mcp.json`.
  Gateway startup auto-migrates existing per-agent entries to global.
  Agent-specific MCP servers (Playwright, Browserbase) stay per-agent.
  This eliminates the class of bugs where agents lacked MCP tool access.



## [1.8.3] - 2026-04-19

### Fixed
- **Cron jobs invisible to MCP**: `list_cron_tasks` filtered by `default_agent`,
  hiding sub-agent cron tasks (duduclaw-pm, xianwen-pm, etc.). Dashboard showed
  them but agents couldn't see or manage them. Now returns all tasks by default.
- **Missing `.mcp.json` for agents**: Agnes pointed to non-existent `duduclaw-pro`
  binary; other agents had no `.mcp.json` at all, causing "沒有 MCP 通訊工具".
  Gateway startup now auto-creates/fixes `.mcp.json` for all agents.



## [1.8.2] - 2026-04-19

### Added
- **Sub-agent team roster injection**: System prompt now automatically includes
  a "Your Team" section listing sub-agents (by `reports_to` hierarchy), enabling
  natural delegation like "請團隊檢查" without requiring SOUL.md changes.
- **Release workflow_dispatch**: Release CI can now be manually re-triggered
  with `gh workflow run release.yml -f tag=vX.Y.Z` when tag-push CI fails.

### Fixed
- **Agent team awareness**: Agnes didn't recognize "duduclaw團隊" as her
  sub-agents because organizational context was missing from system prompt.



## [1.8.1] - 2026-04-19

### Added
- **Native multi-turn session management**: Claude CLI `--resume` with SHA-256
  deterministic session ID mapping. Fallback to XML-delimited history-in-prompt
  when session not found (e.g., account rotation).
- **Hermes-inspired turn trimming**: Long conversation turns (>800 chars) are
  trimmed to head 300 + tail 200 chars with `[trimmed N chars]` placeholder.
  CJK-safe char-level slicing. Zero LLM cost.
- **Direct API prompt cache strategy**: "system_and_3" cache breakpoint placement
  inspired by Hermes Agent for ~75% cache hit rate on multi-turn conversations.
- **Session compression summary injection**: Post-compression summaries (role=system)
  are now injected into system prompt instead of conversation turns.

### Removed
- **MemGPT 3-layer memory system** (-1,985 LOC): Core Memory, Recall Memory,
  Archival Bridge, Budget Manager, Consolidation Pipeline.
  The system prompt injection approach caused 6,500 tokens of bloat per prompt
  and "lost in the middle" attention degradation.
- **6 MCP tools**: `core_memory_get`, `core_memory_append`, `core_memory_replace`,
  `recall_search`, `archival_search`, `archival_insert`.
- 3 SQLite databases (`core_memory.db`, `recall_memory.db`) are no longer populated.

### Fixed
- **Session chain breakage**: Agnes losing context between consecutive messages
  ("幫我全部開啟" → "你指的是什麼？"). Root cause: stateless CLI subprocess
  per message with history in system prompt. Now uses native multi-turn.



## [1.7.2] - 2026-04-17

### Fixed
- **Stream-JSON empty result overwrite**: When Claude uses tools, the final `result`
  event often has an empty `result` field. The parser unconditionally overwrote
  accumulated assistant text with this empty string, causing false "Empty response"
  errors. Fixed in all 4 stream-json parsers (channel_reply, claude_runner, agent
  runner, gemini runtime).
- **Python SDK fallback OAuth awareness**: The Python SDK fallback now skips entirely
  for OAuth-only setups (it requires API keys) instead of producing the misleading
  "未設定任何 API 帳號" error. When an API key is available, it is explicitly
  passed to the subprocess.



## [1.6.0] - 2026-04-17

### Added
- **Git Worktree L0 isolation layer** (`worktree.rs`): lightweight per-task filesystem
  isolation via git worktrees. Cheaper than container sandbox — creates isolated working
  directories so concurrent agents don't step on each other's files.
  - `WorktreeManager`: full lifecycle management (create / remove / list / cleanup_stale)
  - **Atomic merge** with dry-run pre-check: merge → check → abort → real merge if clean.
    Protected by global `Mutex` to prevent concurrent merge corruption.
  - **Snap workflow** (inspired by agent-worktree): create → execute → inspect → merge/cleanup,
    with pure-function decision logic separated from I/O for testability.
  - **Friendly branch names**: `wt/{agent_id}/{adjective}-{noun}` from 50×50 word lists.
  - **copy_env_files**: copies `.env` etc. into worktree with path traversal jail,
    symlink rejection, and 1MB size limit.
  - **Structured exit codes**: `AgentExitCode` enum (Success/Error/Retry/KeepAlive).
  - **Resource limits**: max 5 worktrees per agent, 20 total.
- `ContainerConfig` extended with `worktree_enabled`, `worktree_auto_merge`,
  `worktree_cleanup_on_exit`, `worktree_copy_files` fields.
- Three-tier isolation routing in dispatcher: L0 Worktree → L1 Container → Direct.
- `WORKTREE_PATH` task-local in `claude_runner` for working directory override.

### Security (3-round deep review)
- Path traversal defense: canonical jail + absolute path rejection + `..` blocking.
- Agent ID sanitization: `sanitize_agent_id()` restricts to `[a-z0-9-]`.
- Branch name validation: `validate_wt_branch()` rejects `..`, leading `-`, non-`wt/` prefixes.
- Git command hardening: `--` separators on all `git merge` commands.
- `restore_head` validates branch names and commit hashes before `git checkout`.
- Symlink checks before `canonicalize()` to prevent TOCTOU bypass.
- Destination file removal before copy to prevent symlink race.
- Global merge lock via `OnceLock<Mutex<()>>` (not per-instance).

## [1.5.0] - 2026-04-17

### Added
- **SOUL.md content scanner** (`soul_scanner`): defends against "Soul-Evil Attack" —
  detects hidden HTML comments, invisible Unicode, zero-width steganography, data URIs,
  and hidden HTML tags in SOUL.md files.
- **Agent Stability Index** (`stability_index`): quantifies identity drift between
  SOUL.md versions with configurable thresholds (Warning / Critical).
- **Template sanitizer** (`template_sanitizer`): sanitizes prompt templates for
  injection resistance.
- **SoulSpec v0.5 compatibility**: soul_partition now recognizes SoulSpec v0.5 headers
  (Core Identity, Personality, Learned Patterns, etc.), with validation and export.
- **Audit Logs page**: new History tab showing JSONL audit events with severity icons,
  agent/channel/user badges, and expandable JSON details. Existing real-time log stream
  moved to Realtime tab.
- **Billing usage API** (`billing.usage`): returns live session count, active agents,
  connected channels, and inference hours from actual data sources.

### Changed
- GVU updater now runs soul_scanner + ASI checks before applying SOUL.md proposals.
- Soul guard integrity check includes content scan on every run and ASI on drift.
- BillingPage simplified — removed stub plan card, payment method, invoice history,
  and upgrade sections (not applicable to community edition).
- Logs nav icon changed from ScrollText to FileText; label renamed to "Audit Logs".

### Fixed
- Clippy: `sort_by_key` with `Reverse` instead of `sort_by` closure (3 occurrences).
- Windows sandbox test split with `cfg(not(windows))` / `cfg(windows)`.
- `clippy::collapsible_match` allow in webchat.
- CI: ignore RUSTSEC-2026-0098 and RUSTSEC-2026-0099.


All notable changes to DuDuClaw are documented here. For the authoritative
version history and per-commit detail, see `git log`.

## [v1.4.31] — 2026-04-16

### Fixed

- **GVU JSON fence parsing.** Rewrote `strip_json_fences()` to handle LLM
  responses with trailing text after the closing ` ``` ` fence. Previous
  implementation used `strip_suffix` which failed when judges appended
  commentary, causing 22 consecutive GVU trigger failures since 4/07.
  Unified fast-path and preamble-path into a single `rfind`-based approach.

### Changed

- Dashboard live data, logs fix, analytics API (from v1.4.30)

---

## [v1.4.29] — 2026-04-16

### Added

- **Skill auto-synthesis (Phase 3-4).** Gap accumulator detects repeated
  domain gaps → synthesizes skills from episodic memory (Voyager-inspired)
  → sandbox trial with TTL management → cross-agent graduation to global
  scope. New MCP tools: `skill_security_scan`, `skill_graduate`,
  `skill_synthesis_status`.

- **Task Board.** SQLite-backed task management with status/priority/
  assignment tracking and real-time Activity Feed via WebSocket. MCP tools:
  `tasks.list`, `tasks.create`, `tasks.update`, `tasks.assign`,
  `activity.list`, `activity.subscribe`.

- **Shared Knowledge Base.** Cross-agent wiki at `~/.duduclaw/shared/wiki/`
  for organizational knowledge (SOPs, policies, product specs). Wiki target
  classification (agent/shared/both), visibility control via `wiki_visible_to`
  capability, full-text search with author attribution. MCP tools:
  `shared_wiki_ls`, `shared_wiki_read`, `shared_wiki_write`,
  `shared_wiki_search`, `shared_wiki_delete`, `shared_wiki_stats`, `wiki_share`.

- **Autopilot rule engine.** Event-driven automation — triggers: task_created,
  task_status_changed, channel_message, agent_idle, cron. Actions: task_delegate,
  notify, skill_execute. Dashboard Settings → Autopilot tab for rule management
  and execution history.

- **Skill Market three-tab UI.** Marketplace / Shared Skills / My Skills with
  skill adoption flow and usage statistics.

- **Security status endpoint.** Exposes credential proxy, mount guard, RBAC,
  rate limiter, and SOUL drift state via API.

- **Analytics endpoints.** Conversation summaries and cost savings tracking.

### Enhanced

- MCP Server expanded from 70+ to 80+ tools.
- Dashboard i18n keys expanded from 540+ to 600+ (zh-TW / en / ja-JP).
- Evolution config extensibility for skill synthesis thresholds, graduation
  criteria, and curiosity-driven exploration.
- `CapabilitiesConfig` now includes `wiki_visible_to` with explicit `Default`
  implementation and `sanitize()` for safe deserialization.

## [v1.4.28] — 2026-04-15

### Fixed

- **Cognitive memory not persisted to database.** `StoreEpisodic` action
  from the prediction router was only debug-logged but never written to
  the per-agent `memory.db`. Dashboard Memory & Skills page showed empty
  even with cognitive memory enabled. Now creates
  `agents/<id>/state/memory.db` and stores `MemoryEntry` via
  `SqliteMemoryEngine`, making episodic observations queryable from the
  dashboard and MCP `memory.search` / `memory.browse` tools.

## [v1.3.17] — 2026-04-12

### Added

- **Action-claim verifier wired into live reply path (shadow mode).**
  The existing `duduclaw_security::action_claim_verifier` module (420
  lines, 13 unit tests, pure regex + audit-log cross-reference, zero
  LLM cost) was built but **never called from production code**. It is
  now invoked at two critical points:

  1. **Channel replies** ([channel_reply.rs](crates/duduclaw-gateway/src/channel_reply.rs)):
     immediately after the Claude CLI subprocess returns and before the
     reply is saved to the session / shipped to Discord / Telegram / LINE.
  2. **Cron task execution** ([cron_scheduler.rs](crates/duduclaw-gateway/src/cron_scheduler.rs)):
     after the scheduled agent responds and before `record_run` marks
     the task as successful.

  On both paths, a `dispatch_start_time` is captured before the CLI
  call. After the reply arrives, `detect_hallucinations(home_dir,
  agent_id, &reply, &dispatch_start_time)` extracts action claims via
  regex (zh-TW + English patterns for AgentCreated / AgentDeleted /
  SoulUpdated / MessageSent / AgentSpawned), reads the MCP tool-call
  audit log (`tool_calls.jsonl`) filtered to this turn + this agent,
  and cross-references each claim against actual successful tool calls.

  **Shadow mode**: detections are logged at `warn!` level and written
  to `security_audit.jsonl` via `log_tool_hallucination()`, but the
  reply text is **not modified**. This lets us collect a baseline
  `ungrounded_claim_rate` before flipping to enforce mode.

- **Implementation plan document** at [docs/TODO-agent-honesty.md](docs/TODO-agent-honesty.md):
  3-phase defence-in-depth roadmap (Action-Claim Verifier → Proxy State
  Verifier + Abstain Actions → Tool Receipts / NabaOS), backed by 6
  verified arxiv papers (ToolBeHonest 2406.20015, Agent-as-a-Judge
  2410.10934, Relign 2412.04141, MCPVerse 2508.16260, Agent Hallucination
  Survey 2509.18970, Tool Receipts 2603.10060). Day-by-day schedule,
  success metrics, known limitations, and enforce-mode policy options.

---

## [v1.3.16] — 2026-04-12

### Fixed

- **`duduclaw agent create` now writes `.mcp.json`.** New agents created
  via the CLI (or the `wizard` subcommand) previously got every scaffold
  file *except* `.mcp.json`, which meant the duduclaw MCP server never
  attached to their Claude Code sessions and tools like `create_agent`,
  `spawn_agent`, `list_agents`, `send_to_agent` were silently unavailable.
  SOUL.md's "always call `create_agent`" rule became unenforceable
  because the tool literally didn't exist in the model's toolbelt — the
  model either fell back to raw Bash writes (blocked by agent-file-guard
  since v1.3.15) or fabricated agent creation in plain text. Both the
  CLI (`cmd_agent_create`) and the industry wizard now write a
  `.mcp.json` pointing at the currently-running duduclaw binary.

- **Hint message placeholder not expanded.** `duduclaw agent create`
  used to print `Run \`duduclaw agent run {agent_name}\` to start a
  session` literally with `{agent_name}` unexpanded (because the string
  was passed to `style()` instead of `format!()`). The hint now shows
  the real agent name.

### Added

- **`duduclaw agent create` flags.** The subcommand previously took
  only a positional `name`. It now accepts `--display-name`, `--role`,
  `--reports-to`, `--icon`, and `--trigger` so teams can be scripted
  without post-hoc `sed` on `agent.toml`:

  ```sh
  duduclaw agent create xianwen-tl \
    --display-name "Xianwen TL" \
    --role team-leader \
    --icon 🎯
  ```

- **`AgentRole` enum gained `TeamLeader` and `ProductManager`** so
  planner/coordinator agents can declare a more specific role. The enum
  serialisation switched from `rename_all = "lowercase"` to
  `rename_all = "kebab-case"`; single-word variants (`main`, `worker`,
  `qa`, `planner`, …) look identical to the old encoding so existing
  `agent.toml` files keep parsing unchanged. Multi-word variants use
  kebab-case (`team-leader`, `product-manager`).

- **Lenient role parsing.** `AgentRole::from_str` normalises spacing /
  case / underscore vs hyphen and accepts common aliases: `engineer`
  (→ Developer), `tl`/`lead`/`teamlead` (→ TeamLeader), `pm`
  (→ ProductManager), `quality`/`quality-assurance` (→ Qa). The same
  aliases are accepted by serde via `#[serde(alias = …)]`, so
  round-tripping natural-language role input through `agent.toml`
  resolves to the canonical form on the next read.

- **`AgentRole::as_str()` + `Display` impl + `valid_values_help()`**
  helpers for error messages. The MCP `agent_update` handler now uses
  `AgentRole::from_str` with a single shared help string instead of its
  own private match table.

### Tests

- 6 new unit tests in `duduclaw_core::types::tests` covering round-trip
  (`agent_role_roundtrip_via_serde_json`), wire format
  (`agent_role_kebab_case_wire_format`), serde aliases
  (`agent_role_serde_aliases_accepted`), lenient `FromStr` parsing
  (`agent_role_from_str_lenient_normalisation`), rejection of garbage
  (`agent_role_from_str_rejects_garbage`), and `Display` round-trip.

---

## [v1.3.15] — 2026-04-11

### Fixed

- **agent-file-guard now blocks Bash-based agent-structure writes.** The
  PreToolUse hook matcher was previously `Write|Edit|MultiEdit` only, so a
  sub-agent could silently bypass the guard by invoking
  `Bash mkdir -p /some/project/.claude/agents/foo` or
  `Bash cat > /some/project/.claude/agents/foo/agent.toml`. The guard now
  also matches `Bash`, and `cmd_hook_agent_file_guard` dispatches on
  `tool_name` so that Bash commands are inspected against the new
  [`duduclaw_core::check_bash_command`] helper.

  **Policy:** any Bash command whose text contains the substring
  `.claude/agents/` is blocked. Rationale — the canonical agent root is
  `~/.duduclaw/agents/<name>/` and never contains that path segment, and
  project trees that an agent *works on* should never have an in-tree
  `.claude/agents/` directory (Claude Code's own config lives at
  `~/.claude/`, not nested in project repos). The rule is intentionally
  conservative: even read-only listings that mention `.claude/agents/`
  are blocked, since the correct replacement is the `list_agents` MCP
  tool or a direct `Read` on a known canonical path.

  Existing agents get the updated matcher automatically on next invocation
  (the hook installer runs on every `call_claude_for_agent_with_type` and
  updates the tagged hook entry in place — no manual action required).

### Tests

- 8 new unit tests in `duduclaw_core::agent_guard::tests`
  (`bash_mkdir_in_foreign_project_is_blocked`,
  `bash_write_to_agent_toml_via_heredoc_is_blocked`,
  `bash_with_quoted_path_is_blocked`,
  `bash_ls_mentioning_sentinel_is_also_blocked`,
  `bash_git_status_is_allowed`,
  `bash_ls_canonical_agent_dotclaude_is_allowed`,
  `bash_touching_claude_hooks_subdir_is_allowed`,
  `bash_nested_agents_under_home_is_still_blocked`).

---

## [v1.3.14] — 2026-04-11

### Added

- **SQLite-backed cron task store with hot reload.** Replaced the legacy `cron_tasks.jsonl` file with a proper relational store at `~/.duduclaw/cron_tasks.db` (WAL mode). The new `CronStore` module ([crates/duduclaw-gateway/src/cron_store.rs](crates/duduclaw-gateway/src/cron_store.rs)) exposes full CRUD (`list_all`, `list_enabled`, `get`, `get_by_name`, `insert`, `update_fields`, `set_enabled`, `delete`, `record_run`) and tracks run history (`last_run_at`, `last_status`, `last_error`, `run_count`, `failure_count`) so the dashboard can surface per-task reliability metrics.

- **Hot-reload signal for `CronScheduler`.** The scheduler's run loop now uses `tokio::select!` to wake on **either** a 30-second baseline tick **or** an `Arc<Notify>` pulse fired by `CronScheduler::reload_now()`. Dashboard edits (`cron.add` / `cron.update` / `cron.pause` / `cron.resume` / `cron.remove`) now take effect immediately — no more 5-minute reload window. MCP subprocess writes are picked up on the next 30-second tick via shared WAL-mode SQLite (no inter-process signal needed).

- **New dashboard RPC methods:** `cron.update` (partial-field update) and `cron.resume` (re-enable paused task). All cron handlers now accept either `id` or `name` for identification, and `cron` or `schedule` for the expression (legacy alias).

- **One-shot JSONL → SQLite migration.** On first startup after upgrade, `CronStore::migrate_from_jsonl` imports any existing `cron_tasks.jsonl` entries into the DB, then renames the file to `cron_tasks.jsonl.migrated` to avoid re-running. Idempotent and safe to re-invoke.

### Changed

- **MCP `schedule_task` writes to SQLite directly** instead of appending JSONL. Both the gateway process and the MCP subprocess share the same WAL-mode DB — safe for concurrent access.

- **Last-run merge strategy on reload.** When the scheduler reloads (either via hot-reload signal or baseline tick), each task's `last_run` is merged as `max(in-memory, DB last_run_at)` to prevent same-minute re-fires after a mid-cycle reload.

### Tests

- 2 new unit tests for `CronStore`: CRUD roundtrip + JSONL migration idempotency.

---

## [v1.3.13] — 2026-04-11

### Added

- **Stream-json diagnostics on CLI failures.** The `channel_reply::spawn_claude_cli_with_env` now tracks stream-json event counts (`lines_seen`, `events_parsed`, `assistant_events`, `text_blocks`, `thinking_blocks`, `tool_use_blocks`, `result_events`) and captures the last raw stream line, `result.subtype`, the latest `message.stop_reason`, and a tail of stderr. All of these are embedded into the error message when `spawn_claude_cli_with_env` returns `Empty response from claude CLI` or non-zero exit. `channel_failures.jsonl` is now self-describing — no more needing to reproduce manually in a shell to figure out *why* a reply was empty.

- **`DUDUCLAW_STREAM_DEBUG=1` env var.** When set on the gateway process, every raw line from `claude`'s stdout is appended to `<home>/claude_stream.log`. Off by default (the log can be large and contains user prompts).

- **Stderr draining.** A background tokio task drains `claude` CLI's stderr pipe concurrently and keeps the last 2 KiB for error diagnostics. Without this, `claude` could block forever if stderr filled its pipe buffer (~64 KiB).

### Changed

- **Classifier substring matching still works on diagnostic-suffixed errors.** The error strings returned by `spawn_claude_cli_with_env` now look like:
  ```
  Empty response from claude CLI (exit=0 lines=42 events=30 assistant=2 text_blocks=0 thinking=1 ...)
  ```
  `classify_cli_failure` uses substring matches so the same reason (`EmptyResponse`, `SpawnError`, etc.) is still detected. Two new regression tests lock this invariant.

### Tests

- **415 tests passing** (core: 21, gateway: 377, agent: 17). Added 2 new classifier tests for diagnostic-suffixed error strings.

---

## [v1.3.12] — 2026-04-11

### Fixed

- **Rotator broke keychain auth by injecting `CLAUDE_CONFIG_DIR=~/.claude`**
  (regression from the multi-account rotation introduced in v1.3.11). When
  the auto-detected default OAuth session was selected, `select()` set
  `CLAUDE_CONFIG_DIR` to `~/.claude` even though that *is* the claude CLI
  default — and the `claude` CLI, when the env var is set explicitly, stops
  looking at the macOS keychain for credentials. Every channel reply call
  then hit "Not logged in · Please run /login".
  Fix: `account_rotator::select()` now skips the `CLAUDE_CONFIG_DIR`
  injection when `credentials_dir` equals the default `~/.claude`, so
  claude CLI picks up keychain auth normally. Non-default profile
  directories (`~/.claude/profiles/work`, etc.) still get the env var.
  Regression tests in `account_rotator::select_env_tests` lock this in.

- **Stream parser silently swallowed `is_error: true` results.** The
  `claude` CLI emits terminal errors (auth failure, synthetic responses)
  as `type="result"` stream-json events with `is_error: true`, with the
  error text in the `result` field. Both `channel_reply::spawn_claude_cli_with_env`
  and `claude_runner::call_claude_streaming` were capturing the error
  text as `result_text` and returning `Ok(...)`, so users saw
  "Not logged in · Please run /login" posted to Discord/LINE/Telegram as
  Agnes's actual reply. Now:
  - `is_error: true` on a `result` event → `return Err("claude CLI stream error: ...")`
  - `error` field on an `assistant` event → same
  - Post-loop: any non-zero exit code is a hard failure (previously we
    only errored when `result_text` was empty, which let partial output
    leak through).

- **`FailureReason::AuthFailed` classifier** — new branch in
  `classify_cli_failure` detects `"Not logged in"` / `"authentication_failed"` /
  `"please run /login"` and surfaces a zh-TW message that actually tells
  the user to run `claude /login` instead of the misleading
  "`claude auth status`" hint (which only checks state, doesn't fix auth).

### Tests

- 2 new regression tests in `duduclaw-agent::account_rotator::select_env_tests`
- 2 new classifier tests + 1 end-to-end pipeline test in `channel_reply::fallback_tests` / `rotation_tests`
- **413 tests total passing** (core: 21, gateway: 375, agent: 17)

---

## [v1.3.11] — 2026-04-11

### Added

- **Agent file-write guard (Option 3 hardening)** — `duduclaw hook
  agent-file-guard` PreToolUse hook is now automatically installed into
  `<agent_dir>/.claude/settings.json` on every agent creation (MCP
  `create_agent`, dashboard `agents.create`, CLI `wizard`, channel reply
  spawn, dispatcher spawn, and gateway startup). Blocks agents from using
  raw Write/Edit/MultiEdit to create `agent.toml` / `SOUL.md` / `CLAUDE.md`
  / `MEMORY.md` / `.mcp.json` / `CONTRACT.toml` outside the canonical
  `<home>/agents/<name>/` tree. Agents must use the `create_agent` MCP
  tool instead, so the registry and dashboard always see newly-created
  sub-agents. Pure Rust enforcement — no shell dependencies, cross-platform
  (macOS/Linux/Windows).
  Files: `crates/duduclaw-core/src/agent_guard.rs`,
  `crates/duduclaw-gateway/src/agent_hook_installer.rs`,
  `crates/duduclaw-cli/src/lib.rs` (new `Hook` subcommand).

### Fixed

- **Channel reply: intermittent "Claude Code not found" error (#fallback-fix)**
  Root cause: the channel reply path (`channel_reply::call_claude_cli`) was
  bypassing the `AccountRotator` entirely and spawning `claude -p` against
  the ambient environment. When the single default OAuth session was cooling
  down (rate-limit / token refresh / billing), every attempt failed and the
  user saw a hardcoded "please run `claude auth status`" message that
  misrepresented the actual cause. The sub-agent dispatcher path already
  rotated correctly, which explained the "有機率" symptom.

  This release routes the channel reply path through a new testable
  rotation primitive `rotate_cli_spawn`, so **both** the dispatcher and
  channel paths now use the same multi-OAuth / API-key rotation, cooldown
  tracking, and billing-exhaustion handling.
  Files: `crates/duduclaw-gateway/src/channel_reply.rs`.

- **Misleading fallback error message → category-specific diagnostics**
  Replaced the hardcoded `"{name} 收到你的訊息，但目前無法回覆。請確認 Claude
  Code 已安裝並登入"` message with a classifier (`FailureReason`) that
  distinguishes:
  - `BinaryMissing` — actually missing binary (keeps the `auth status` hint)
  - `RateLimited` — 忙線中，請稍後再試
  - `Billing` — 帳號額度已用完
  - `Timeout` — 30 分鐘處理超時
  - `SpawnError` — 子程序啟動失敗
  - `EmptyResponse` — 空回應
  - `NoAccounts` — 尚未設定帳號
  - `Unknown` — 通用錯誤提示

  Each fallback also appends a structured JSONL record to
  `~/.duduclaw/channel_failures.jsonl` for dashboard surfacing.

- **`which_claude()` now discovers launchd / Finder-launched installs**
  Added candidate paths for `/opt/homebrew/bin/claude` (Apple Silicon
  Homebrew), `$HOME/.bun/bin/claude`, `$HOME/.volta/bin/claude`,
  `$HOME/.asdf/shims/claude`, plus NVM version-directory scanning
  (`$HOME/.nvm/versions/node/*/bin/claude`). Previously, gateways launched
  from Finder / Dock / launchd without Homebrew on `PATH` would fail to
  find `claude` even when it was installed.

  Also extracted `which_claude_in_home(home: &Path)` as a pure, testable
  helper that doesn't touch `PATH` or environment state.
  Files: `crates/duduclaw-core/src/lib.rs`.

### Added

- **`AccountRotator::push_account_for_test`** — cross-crate test helper
  (marked `#[doc(hidden)]`) so rotation unit tests can inject synthetic
  accounts without writing a config file or shelling out to `claude auth
  status`. Files: `crates/duduclaw-agent/src/account_rotator.rs`.

### Tests

- 7 new unit tests in `duduclaw-core::which_claude_tests` covering Bun,
  Volta, asdf, npm-global, NVM, candidate ordering, and "no candidates"
  fallback.
- 10 new unit tests in `duduclaw-gateway::channel_reply::fallback_tests`
  covering `classify_cli_failure` (rate-limit / billing / timeout / binary /
  empty / spawn / unknown) and `format_fallback_message` (message content
  assertions for zh-TW, agent name substitution, correct vs. misleading
  hints).
- 6 new async tests in `duduclaw-gateway::channel_reply::rotation_tests`:
  - `single_account_success_is_first_try` — smoke-replacement for the
    single-OAuth regression path
  - `rotation_advances_past_rate_limited_account` — verifies 2-account
    cycling and rotator state after `on_rate_limited`
  - `rotation_all_fail_propagates_last_error` — all-fail aggregator
  - `rotation_billing_error_triggers_long_cooldown` — 24h cooldown
  - `rotation_empty_rotator_returns_empty_exhausted` — primitive contract
  - `end_to_end_rate_limit_yields_busy_message` — full pipeline from
    rotation failure → classification → user message; guards against
    future regressions where the message incorrectly says "please install"

### Developer Notes

- `is_billing_error` and `is_rate_limit_error` in `claude_runner.rs` are now
  `pub(crate)` so the channel reply path can reuse the shared classifiers.
- `spawn_claude_cli_with_env` carries `#[allow(clippy::too_many_arguments)]`
  (8 args, pure extraction from the pre-existing 7-arg `call_claude_cli`).
- The rotation loop is now decoupled from the subprocess spawn: see
  `rotate_cli_spawn<F, Fut>(rotator, spawn, input_size_hint)`. This enables
  deterministic testing and future reuse (e.g., for other LLM backends).

---

Earlier versions: see `git log --oneline` for commit-level history.
Recent highlights:

- **v1.3.10** — Discord cross-channel reply error, cognitive memory toggle reset
- **v1.3.9** — Discord auto-thread sends guide message in channel
- **v1.3.8** — service stop kills process, all-channel attachment forwarding
- **v1.3.7** — Homebrew formula version alignment
