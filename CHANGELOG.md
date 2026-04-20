# Changelog


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
