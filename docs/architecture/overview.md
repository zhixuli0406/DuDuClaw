# DuDuClaw Architecture Overview

## Architecture Overview (reviewed against v1.67.0)

DuDuClaw is a **Multi-Runtime AI Agent Platform** — supporting **Claude Code / Codex / Antigravity / Grok** CLI (plus OpenAI-compatible APIs) as AI backends via a unified `AgentRuntime` trait with auto-detection and per-agent configuration (the Gemini CLI backend is deprecated in v1.67.0, removed in v1.70.0, replaced by Antigravity). DuDuClaw is not a standalone LLM product; it is the plumbing layer that turns one (or many) AI CLIs into long-running agents with channel routing, session memory, self-evolution, multi-account rotation, local LLM inference, browser automation, and IDE integration.

## Key Architectural Decisions

### Runtime & Transport
- **Multi-Runtime** (`AgentRuntime` trait) — 13 runtime ids in `runtime_catalog.rs`: twelve CLI backends (Claude, Codex, Antigravity (`agy`), Grok, Qwen Code, Kimi Code, GitHub Copilot CLI, Kiro, Cursor, Mistral Vibe, OpenCode, and Gemini CLI, deprecated in v1.67.0 and removed in v1.70.0) plus OpenAI-compat HTTP. Five CLIs and OpenAI-compat have their own modules; the other seven CLIs share one generic print-mode runtime. `RuntimeRegistry` auto-detection, per-agent config in `agent.toml [runtime] provider`.
- **Cross-runtime failover model substitution** (`failover.rs`): when `[runtime] fallback` sends a call to a *different* provider, the fallback runtime no longer inherits the primary's model id (a codex agent's `gpt-5.4` used to be spawned against the Claude runtime). The model is resolved in four ordered branches — ① the first `agent.toml [model] fallbacks` entry whose family confidently belongs to the fallback runtime (qualified `provider/model` entries are unqualified), ② keep the requested model when that runtime already serves it (this is how `openai_compat`, which declares no model family, keeps proxying arbitrary ids), ③ the runtime catalog's own first model for that backend, ④ otherwise **refuse to spawn** and report `no model configured for fallback runtime <P>`, recorded as a failed attempt. Every substitution emits a `warn!` carrying `agent` / `from_runtime` / `to_runtime` / `from_model` / `to_model`.
- **MCP Server (stdio)** (`duduclaw mcp-server`) exposes channel, memory, agent, skill, task, shared wiki, and autopilot tools to AI Runtime via JSON-RPC 2.0 over stdin/stdout. Registered at the agent level in `<agent>/.mcp.json` (v1.8.5 reverted v1.8.4's global registration because Claude CLI `-p --dangerously-skip-permissions` only reads project-level `.mcp.json`). Gateway startup auto-creates/repairs `.mcp.json` for all agents.
- **MCP Server (HTTP/SSE)** (`duduclaw http-server --bind 127.0.0.1:8765`, v1.9.4) — Bearer-authenticated `POST /mcp/v1/call` (single JSON-RPC tool call), `GET /mcp/v1/stream` (SSE long-lived event stream, Bearer / `?api_key=`), `POST /mcp/v1/stream/call` (async + SSE result push), `GET /healthz` (no auth). Token bucket rate limit (60 req/min). `mcp_sse_store.rs` manages SSE connections with broadcast channels. Complements stdio for external HTTP clients.
- **ACP and A2A** — `duduclaw acp` (= `duduclaw acp client`) implements Agent Client Protocol v1 over stdio for Zed / JetBrains / Neovim agent panels. `duduclaw acp server` (formerly `acp-server`, removed in v1.69.0) serves A2A: `message/send` appends to `bus_queue.jsonl`, `tasks/get` maps bus observations back to A2A states, and the Agent Card is at `/.well-known/agent-card.json` (legacy `/agent.json` alias).
- **Agent directories** are Claude Code compatible: each contains `.claude/`, `.mcp.json`, `SOUL.md`, `CLAUDE.md`, `CONTRACT.toml`, `agent.toml`, `wiki/`, `SKILLS/`, `memory/`, `tasks/`, `state/`.

### Channels (11)
- **Telegram** (long polling) — file/photo/sticker/voice, forums/topics, mention-only, voice transcription through the OpenAI Whisper API.
- **LINE** (webhook) — HMAC-SHA256 signature, sticker catalog, per-chat settings.
- **Discord** (Gateway WebSocket) — `tokio::select!` heartbeat, slash commands, auto-thread; voice channels (Songbird) only in builds with the non-default `discord-voice` feature, which release binaries do not include. v1.9.2 hardened: real op 6 RESUME (persists `session_id` + `resume_gateway_url` + sequence), stall watchdog (break if no traffic for 2× heartbeat interval), heartbeat capacity 1→16 with `try_send`, op 9 jitter 1-5s, RESUMED dispatch handling, backoff cap 60s.
- **Slack** (Socket Mode), **WhatsApp** (Cloud API webhook, signature fail-closed), **Feishu** (Open Platform v2), **Google Chat** (webhook, JWT-verified), **Microsoft Teams** (Azure Bot / Connector v3, JWT-verified), **WeCom** (HMAC-SHA1 + AES-256-CBC), **DingTalk** (HMAC-SHA256 + time window), **WebChat** (`/ws/chat` + React frontend).
- The generic `POST /webhook/{agent_id}` endpoint was never mounted and was removed in v1.66; inbound webhooks belong to individual channels and to Odoo (`POST /webhook/odoo`).
- **Channel hot-start/stop**: Dashboard `channels.add` / `channels.remove` launches/aborts the channel task without gateway restart.
- **Media pipeline**: image auto-resize (max 1568px) + MIME detection + Vision integration.

### Sub-Agent Orchestration
- `create_agent` / `spawn_agent` / `list_agents` MCP tools with `reports_to` hierarchy.
- System prompt auto-injects "## Your Team" sub-agent roster.
- **Structured handoff**: `DelegationEnvelope` (context / constraints / task_chain / expected_output) with Raw fallback.
- **TaskSpec workflow**: multi-step task planning with dependency-aware scheduling, auto-retry (3x), replan (2x), persistence.
- **Long-response splitting**: Sub-agent replies wider than the channel byte budget are split via `channel_format::split_text` with `📨 **agent** 的回報 (1/N)` / `(續 2/N)` labels (Discord 1900 / Telegram 4000 / LINE 4900 / Slack 3900).
- **Orphan response recovery**: `reconcile_orphan_responses` atomically replays `bus_queue.jsonl` entries left behind by crash / Ctrl+C / hotswap.

### Session Memory Stack
- **Native multi-turn**: Claude CLI `--resume <session-id>` with SHA-256 deterministic session ID; auto-fallback to history-in-prompt when `--resume` fails (stale handle, account rotation, unknown stream-json error).
- **Turn trimming** (>800 chars → head 300 + tail 200 + `[trimmed N chars]`, CJK-safe).
- **Direct API prompt cache** ("system_and_3" breakpoint strategy; no measured hit rate is published).
- **Compression summaries** injected into system prompt (not conversation turns) at 50k token threshold.
- **Instruction Pinning** (v1.8.6 P0) — first user turn → async Haiku extraction of core task → stored in `sessions.pinned_instructions` → injected at system prompt tail (U-shaped attention). Clarification answers accumulate (≤1000 chars).
- **Snowball Recap** (v1.8.6 P0) — every turn prepends `<task_recap>` to the user message. Zero LLM cost.
- **P2 Key-Fact Accumulator** (v1.8.6) — per substantive turn, Haiku extracts 2-4 key facts → `key_facts` table with FTS5 → top-3 relevant facts injected into system prompt. ~100-150 tokens vs MemGPT's 6,500 (−87%).
- **CLI lightweight path** — `call_claude_cli_lightweight()` with `--effort medium --max-turns 1 --no-session-persistence --tools ""` for metadata tasks. 25-40% cost reduction.
- **Stabilization flags** — `--strict-mcp-config` (MCP isolation) + `--exclude-dynamic-system-prompt-sections` (cross-turn prompt stability, 10-15% token reduction). `--bare` was removed in v1.8.11 (broke OS keychain credential lookup).

### Evolution
- **Prediction-driven engine**: Active Inference + Dual Process Theory; by design most conversations end without an LLM call (no measured share is published). Moderate errors are stored as episodic memory; Significant errors start an AEE evolution round; Critical errors (or three Significant in a row) start an emergency round; a small exploration share of Negligible errors (ε, hard floor 5%) also starts one.
- **MetaCognition**: self-calibrating error thresholds every 100 predictions.
- **MistakeNotebook**: cross-loop error memory prevents regression; entries now carry deterministic `TrajectoryEvidence` (which tool/assertion failed) so reflection consolidation stops trusting unverified self-reported diagnosis (Evolution v3).
- **`SOUL.md` is read-only for agents** (Evolution v3, WP1.1 — operator/dashboard writes only). The legacy Generator→Verifier→Updater rewrite path, `SOUL.md` versioning, the 24h observation window and automatic rollback were **removed on 2026-09-29 (S11)**: with nothing able to write the file on an agent's behalf, they guarded a path that no longer existed.
- **AEE (Agentic Evolution Engine, the only evolution engine)**: the evolution target is not `SOUL.md` but a **playbook** of small, independently-retirable gene-shaped entries (category/signals/eval-case-linked), evolved through a Gate (deterministic, veto-keeping) / Measure (scored, no veto) split, a champion + matches-or-improves commit gate, and entry-level (not whole-file) observation windows. See `evolution-engine.md` ch.12 and `docs/features/38-aee-playbook-evolution.md`.
- **Agent-as-Evaluator**: independent Evaluator Agent (Haiku cost control) for adversarial verification with structured JSON verdicts.
- **ConversationOutcome**: zero-LLM conversation result detection (TaskType / Satisfaction / Completion) in zh-TW + en.
- **External factors**: user feedback, security events, channel metrics, Odoo business context, peer agent signals feed into the prediction engine and evolution rounds.

### Wiki Knowledge Layer (v1.8.9)
- **4-layer architecture** (Vault-for-LLM inspired): L0 Identity / L1 Core / L2 Context / L3 Deep.
- **Trust weighting** (`trust` 0.0-1.0 frontmatter) — search results ranked by trust-weighted score.
- **Auto-injection**: `build_system_prompt()` auto-injects L0+L1 pages into WIKI_CONTEXT across CLI / channel reply / dispatcher paths — unified across the Claude / Codex / Antigravity / Grok / OpenAI-compat runtimes (and the deprecated Gemini runtime).
- **FTS5 index** (`unicode61` tokenizer) — auto-syncs on every write/delete, manual rebuild via `wiki_rebuild_fts`.
- **Knowledge graph**: `wiki_graph` MCP tool exports BFS-limited Mermaid diagrams; node shapes by layer.
- **Dedup detection**: `wiki_dedup` detects duplicate pages by title match + tag Jaccard similarity (≥0.8).
- **Reverse backlink index**: scans `related` frontmatter + body markdown links for bidirectional mapping.
- **Search filters**: `wiki_search` (either scope) supports `min_trust`, `layer`, `expand` (1-hop backlink expansion).
- **Shared Wiki**: `~/.duduclaw/shared/wiki/` for cross-agent SOPs, policies, product specs. Visibility controlled via `wiki_visible_to` capability.

### Memory System
- **Cognitive memory** (optional): `SqliteMemoryEngine` with episodic/semantic separation and Generative Agents 3D-weighted retrieval (Recency × Importance × Relevance).
- **Memory decay daily scheduler**: background task runs `duduclaw_memory::decay::run_decay` every 24h. Low-importance + 30 days old → archived. Archived + 90 days → permanent delete.
- **Cognitive memory MCP tools**: `memory_search_by_layer` (episodic/semantic filter), `memory_successful_conversations`, `memory_episodic_pressure`, `memory_consolidation_status`.
- **MemGPT 3-layer system** (Core Memory, Recall Memory, Archival Bridge, Budget Manager, Consolidation Pipeline, 6 MCP tools) was **removed in v1.8.1** (−1,985 LOC) — the prompt injection caused 6,500 token bloat per prompt and "lost in the middle" attention degradation.

### Local Inference
- **Unified `InferenceBackend` trait** (`duduclaw-inference` crate): OpenAI-compatible HTTP (llama-server/Ollama/vLLM/SGLang/llamafile). The in-process llama.cpp, mistral.rs and MLX backends were removed in 2026-09 — no release binary ever compiled them; run a local OpenAI-compatible server instead.
- **Confidence Router**: three-tier LocalFast / LocalStrong / CloudAPI routing, CJK-aware token estimation.
- **InferenceManager**: auto-switching state machine — llamafile → Direct backend → OpenAI-compat → Cloud API.
- **llamafile manager**: subprocess lifecycle, health monitoring, OpenAI-compatible API on localhost.
- **MCP tools**: `model_list`, `model_load`, `model_unload`, `inference_status`, `hardware_info`, `route_query`, `inference_mode`, `llamafile_start/stop/list`.

### Token Compression
- **Reply-path budget pipeline** (`gateway/prompt_compression.rs`): TurnTrim → DropOldestToolEchoes → BisectAndSummarize, cost-pressure aware, CJK-safe token estimation; skipped when recent cache efficiency is above 50% and the budget overshoot is below 15%.
- The earlier Meta-Token (LTSC) / LLMLingua-2 / StreamingLLM compressor and its `compress_text` / `decompress_text` tools were removed in v1.33.

### Voice Pipeline
- **HTTP endpoints**: `POST /api/stt` (an OpenAI-compatible transcription API or a local command template; 501 when no STT provider is configured), `POST /api/tts` over Piper (local) / Edge TTS / MiniMax T2A / OpenAI TTS.
- **Telegram voice**: transcription through the OpenAI Whisper API and Edge TTS replies, both hardcoded; the dashboard voice settings do not reach this path.
- **Not in release binaries**: in-process Whisper (`whisper` feature), ONNX embedding (`onnx` feature) and Discord voice (`discord-voice` feature).
- SenseVoice, Deepgram, Silero VAD, `symphonia` decoding and LiveKit voice rooms were listed here before; none of them has code. See [`docs/features/14-voice-pipeline.md`](../features/14-voice-pipeline.md).

### Security
- **Claude Code PreToolUse hooks** (installed per agent by `agent_hook_installer` into `<agent_dir>/.claude/settings.json`): `duduclaw hook agent-file-guard` (Rust subcommand, matcher `Write|Edit|MultiEdit|NotebookEdit|Bash`, command carries `--agent` and `--home` — agent-structure files outside the canonical tree, own-SOUL.md / own-CONTRACT.toml writes, cross-agent writes, and for an employee caller any write under the DuDuClaw home outside its own agent directory and `attachments/`, judged on the real path after symbolic links; plus the `org_field_guard` field-level freeze on `reports_to` / `department` / `name` / `[capabilities]` / `[delegation]` / `[acp]` and, for employee callers, every section of their own `agent.toml` outside the editable list; the Bash lane is a speed bump and `Read` is not covered — rules and limits in [`05-security-defense.md`](../features/05-security-defense.md)) and `duduclaw hook data-file-guard` (RFC-23 §14.4, matcher `Read|Bash`, armed only when redaction is active; a `Bash` filename heuristic, not a sandbox — H10 2026-09 replaced the shell script that was inert on Windows). The 2026-04 three-phase shell-script defense and its GREEN/YELLOW/RED threat-level state machine were removed in `ba015a48` — see [`docs/features/05-security-defense.md`](../features/05-security-defense.md).
- **SOUL.md drift detection** (SHA-256 fingerprint, ≤10 versioned backups in `.soul_history/`).
- **Prompt injection scanner** (`input_guard`, 11 rule categories, block threshold 60, NFKC-normalized, en + zh-TW patterns, XML delimiter protection).
- **Secret leak scanner** — 19 secret patterns (Anthropic / OpenAI / AWS / GitHub / GitLab / Slack / Stripe / Google / SendGrid / JWT / PEM keys and key or password assignments) plus a high-entropy check, used by the skill security scanner.
- **CONTRACT.toml** — `must_not` / `must_always` boundaries, auto-injected into system prompt; `duduclaw test` red-team CLI (9 built-in scenarios).
- **Unified multi-source audit log**: `audit.unified_log` merges `security_audit.jsonl` / `tool_calls.jsonl` / `channel_failures.jsonl` / `feedback.jsonl` into common envelope (timestamp / source / event_type / agent_id / severity / summary / details) with Logs page filter chips.
- **AES-256-GCM** at rest — per-agent key isolation.
- **Dashboard / WebSocket auth**: JWT account login (Argon2id-hashed passwords in `users.db`) or the gateway admin token. An earlier Ed25519 challenge-response path has been removed from the gateway; no configuration could ever enable it. Ed25519 is still used for licence signatures, update verification and the relay device protocol.
- **Container sandbox** — two separate paths. The per-agent *task sandbox* (`agent.toml [container] sandbox_enabled`) runs a delegated task's AI CLI in a read-only, non-root, resource-limited Docker container (Docker only, needs `network_access = true`, fails closed when it cannot run; see [Task sandbox guide](../guides/task-sandbox.md)). The *script sandbox* used by PTC `execute_program` and the `secaudit` PoC step runs on Docker (WSL2 first on Windows) with `--network=none`, a read-only root and only a private read-only script directory mounted; when it cannot run, PTC refuses the script unless `[container.sandbox] script_when_unavailable = "run_unsandboxed"`, and the PoC never runs on the host.
- **Browser automation & computer use** — two fetch tools and an optional browser server the agent chooses between, no auto-router: L1 `web_fetch_cached` (SSRF-gated, cached HTTP), L2 `web_extract` (CSS selector scrape). L5 computer use runs in a container started by `computer_use_orchestrator` (image `ghcr.io/zhixuli0406/duduclaw-computer-use:v<version>`, never pulled automatically, actions via `xdotool`) and is driven by the agent through eight `computer_*` MCP tools, which the `duduclaw mcp-server` process forwards to gateway-owned sessions over a signed loopback route (`POST /api/internal/computer-use`, `computer_use_sessions/`; network only to the per-agent `allowed_domains` hosts, pinned at session start). The chat-triggered loop and the `native` host-desktop mode were removed. L3 headless is an optional per-agent Playwright/Browserbase MCP server (`.mcp.json`). Deny-by-default via `CapabilitiesConfig` (`computer_use` / `browser_via_bash` / `allowed_tools` / `denied_tools`). The dead `browser_router.rs` 5-layer router and its "L4 Sandbox Browser" tier were removed in 2026-09.
- **CJK-safe byte slicing**: `duduclaw_core::truncate_bytes` / `truncate_chars` replaced 31 unsafe `s[..s.len().min(N)]` sites (fixed v1.8.11 multi-byte codepoint panics).

### Accounts & Cost
- **Per-agent model routing** (SDK-first): `agent.toml [model]` — `preferred` (Claude SDK model), `local.model`, `local.use_router`, `api_mode` (cli/direct/auto), `account_pool` (see below).
- **Multi-OAuth account rotation**: OAuth sessions (Claude Pro/Team/Max via `claude auth status` + `CLAUDE_CODE_OAUTH_TOKEN` for `setup-token` accounts) + API keys. 4 strategies (Priority/LeastCost/Failover/RoundRobin). Rate-limit cooldown (2min), billing-exhaustion cooldown (24h), budget enforcement, token expiry tracking (30d/7d warnings).
- **Per-agent account pool** (`agent.toml [model] account_pool`): restricts which rotator accounts an agent may use. Applied to the **candidate set** — after the provider/health/cooldown/budget filters and before the strategy runs — so all four strategies keep their exact semantics over a narrower set. Entries match an account `id` **or** its dashboard `label` (exact, trimmed, ASCII-case-insensitive; never a substring). **Fail-open**: a pool matching no *available* account (stale ids, everything cooling down) logs a `warn` and falls back to the full set — a stale pool must never leave an agent with no account. Unset/empty ⇒ rotation unchanged. Entry points: `AccountRotator::select_with_pool` / `select_for_provider_with_pool`.
- **Dual dispatch path**: both sub-agent dispatcher (`claude_runner::call_with_rotation`) and user-facing channel reply (`channel_reply::call_claude_cli_rotated` → `rotate_cli_spawn_with_pool`) go through the rotator, each threading the answering agent's `account_pool`.
- **`FailureReason` classification** — RateLimited / Billing / Timeout / BinaryMissing / SpawnError / EmptyResponse / NoAccounts / Unknown — with category-specific zh-TW user messages and `channel_failures.jsonl` audit records.
- **Binary discovery**: `which_claude()` / `which_claude_in_home()` probe Homebrew (Intel + Apple Silicon), Bun, Volta, npm-global, `.claude/bin`, `.local/bin`, asdf shims, NVM version directories — fixes launchd-launched gateway binary discovery when `PATH` is empty.
- **CostTelemetry**: SQLite-backed token usage tracking with cache efficiency analytics (`cache_read / (input + cache_read + cache_creation)`), 200K price cliff warning, adaptive routing (cache_eff <30% → local). MCP tools: `cost_summary`, `cost_agents`, `cost_recent`.
- **Per-model cost rollup** (`CostTelemetry::summary_by_model`): `token_usage` has carried a `model` column since the first schema, but every rollup grouped by agent / user / day — "which model is the money going to?" was unanswerable. `summary_by_model(agent_id: Option<&str>, since_unix)` groups by model (costliest first; rows with no recorded model id bucket under `"(unknown)"`, never guessed) and reports `requests` / `input_tokens` / `output_tokens` / `cache_read_tokens` / `cache_creation_tokens` / `cost_millicents` + `cost_usd` / `cache_efficiency`. Cost is the summed stored per-row `cost_millicents` — the same single pricing path (`cost_for`, applied once at record time) as every sibling rollup, never re-derived; `cost_usd` is a pure unit conversion of it. Surfaced additively: the MCP `cost_summary` and `cost_agents` responses gain a `by_model` array for the same window (`cost_agents`' agent rows move under an `agents` key, since a top-level JSON array cannot carry a named sibling), and the dashboard RPC `cost.by_model` (params `agent_id?`, `days?` — default 7, clamped 1–365) returns the rollup under the same admin gate as its `cost.*` siblings. A rollup failure inside `cost_summary`/`cost_agents` degrades to an empty `by_model` rather than failing the call the caller actually made.
- **Direct API client** (`direct_api.rs`): bypasses Claude CLI for pure chat, `cache_control: ephemeral` on system prompt (no measured hit rate is published). Singleton `reqwest::Client` with 120s timeout; used as fallback when all OAuth accounts are cooling.

### Scheduling
- **HeartbeatScheduler**: per-agent unified scheduling — bus polling + GVU silence breaker + cron, `max_concurrent_runs` semaphore.
- **CronScheduler**: reads `cron_tasks.jsonl` (+ `cron_tasks.db` since v1.8.12), fires tasks on cron expression. `list_cron_tasks` returns all tasks (no longer filters by default_agent, v1.8.3). Schedules are created with `tasks_create` + `schedule`; the older `schedule_task` MCP tool was removed in v1.69.0.
- **ReminderScheduler**: one-shot reminders (relative `5m`/`2h`/`1d` or ISO 8601), `direct` static message or `agent_callback` wake-up mode.

### Skill Ecosystem
- **6-stage lifecycle**: Activation → Compression (three-layer progressive loading) → Extraction → Distillation → Diagnosis → Gap Analysis. The former Reconstruction stage had no caller and was removed in 2026-09.
- **GitHub live indexing** — Search API with 24h local cache, weighted search.
- **Skill auto-synthesis** (Phase 3-4): gap accumulator detects repeated domain gaps → synthesizes skills from episodic memory (Voyager-inspired) → sandbox trial with TTL → cross-agent graduation; off by default (`agent.toml [evolution] skill_synthesis_enabled`). MCP tools: `skill_security_scan`, `skill_graduate`, `skill_synthesis_status`.
- **Rust-native Skill security scanner** (`skill_lifecycle::security_scanner`) — no Python subprocess; backs the dashboard vet, the MCP `skill_security_scan` tool, and the sandbox-trial gate.

### Task & Knowledge
- **Task Board**: SQLite-backed task management with status/priority/assignment tracking + real-time Activity Feed WebSocket. Dashboard RPC `tasks.list/create/update/remove/assign`, `activity.list`; agent MCP tools `tasks_list`, `tasks_create`, `tasks_update`, `tasks_claim`, `tasks_complete`, `tasks_block`, `activity_list`, `activity_post`. An AI employee changing a task that is not its own (not assignee, claimer or creator; for complete/block, not assignee or claimer) needs the delegation relationship with the assignee, and cannot add, remove or reorder the control tags `outcome:…`, `grant:…`, `auto-research`; on a goal task `title`, `description` and `acceptance_criteria` are frozen for AI employees.
- **Shared Knowledge Base**: `~/.duduclaw/shared/wiki/` with Wiki target classification (agent/shared/both). MCP tools: `wiki_ls/read/write/search/stats/lint` with `scope="shared"` (the six `shared_wiki_*` spellings were removed in v1.69.0), plus `shared_wiki_delete` and `wiki_share`.
- **Autopilot rule engine**: automated delegation/notifications/skill execution. Triggers a new rule may use (12): `task_created`, `task_updated`, `task_status_changed`, `activity_new`, `channel_message`, `agent_idle`, `run_at_risk`, `os_file`, `os_frontmost`, `tick`, `security_event`, `odoo_event`; `cron_tick` is never emitted and refused on create since v1.67.1 ([23-autopilot-engine](../features/23-autopilot-engine.md)).

### Integrations
- **Odoo ERP bridge** (`duduclaw-odoo` crate): JSON-RPC middleware supporting CE/EE, 17 MCP tools (CRM/Sales/Inventory/Accounting), EditionGate auto-detection, event polling + `POST /webhook/odoo` (both off by default) feeding `odoo_event` autopilot rules. Per-agent credential isolation via `OdooConnectorPool` (RFC-21 §2, v1.11.0). Dashboard test-before-save: `odoo.test` RPC accepts inline params (v1.13.1) — credential omitted falls back to stored secret; same SSRF/HTTPS/db-name validators as `odoo.configure`; `scrub_odoo_error()` caps connector errors at 240 chars to prevent HTML / URL leakage.
- **Prometheus metrics**: `GET /metrics` on gateway HTTP — failover, wiki trust, decision continuity, prompt compression, resident sensing (`tick_*`), goal-loop and live-fork counters. The request/token/duration/session/channel/budget series were never incremented and were removed in v1.66.
- **RL trajectory collector**: writes per-agent trajectories to `~/.duduclaw/rl_trajectories.jsonl` during channel interactions. The `duduclaw rl` CLI that exported them was removed in 2026-09.
- **BroadcastLayer** tracing layer streams real-time logs to WebSocket subscribers.
- **Dashboard WebSocket heartbeat**: server Ping every 30s, close idle sockets after 60s without Pong. Client `ping` application-level RPC every 25s (browsers can't issue control frames).

### Reliability & Governance (v1.9.4)
- **`duduclaw-durability` crate** (🗑️ **Removed in commit `b0639b96` on 2026-07-04**) — This crate contained a five-pillar durability framework (`idempotency`, `retry`, `circuit_breaker`, `checkpoint`, `dlq`). It was removed after determining it had zero call sites in the production codebase. The gateway's LLM fallback chain relies on other mechanisms (see `gateway/failover.rs`). Historical references to checkpoint save/rewind/fork are not available in the current codebase.
- **`duduclaw-governance` crate** (🗑️ **removed in `b0639b96`**) — its rate / permission / quota / lifecycle policies had no enforcer, and the dashboard Governance page and its RPCs were removed in v1.66. Rate limits, delegation policy + MCP scopes, and license quotas are enforced separately.
- **LLM fallback chain** (`gateway/failover.rs`, module `failover::model`) — layer 2 of the three-layer failover stack (account → model → runtime, all under one module tree since 2026-09-29): primary timeout / 503 / 429 / overloaded auto-switches to the lighter fallback model, never on a billing error. `is_llm_fallback_error` / `should_attempt_model_fallback` are pure functions with unit tests; `FailoverManager::model_fallback_for` is the combined decision every dispatch path calls. UTF-8-safe truncation via `char_indices`.
- **Evolution Events system** (`gateway/evolution_events/`) — 30+ event schema, async batch+retry emitter, query interface, reliability guarantees. HTTP endpoints exposed on gateway and surfaced in Web `ReliabilityPage`.

### Memory Evaluation (v1.9.4 / W21)
- **LOCOMO evaluation** (`python/duduclaw/memory_eval/`) — `retrieval_accuracy`, `retention_rate`, `locomo_integrity_check`. `cron_runner` is a manual CLI entry point (`python -m memory_eval.cron_runner smoke_test|weekly_kpis|monthly_locomo`); nothing in the repo schedules it. 5-minute `smoke_test` P0 verifies basic memory functions. `build_golden_qa.py` builds the gold QA set; `data/golden_qa_set.jsonl` carries the first 200 entries. `duduclaw-memory` engine adds batch query API for evaluation.
- **Python `agents/` + `mcp/` modules** — `agents/capabilities/` (manifest + matcher), `agents/routing/` (router + resolution + memory_resolver). `mcp/auth/` (API Key with key masking), `mcp/tools/memory/` (store / read / search / namespace / quota with strict scope enforcement at `execute()` entry — patches a v1.9.3 auth gap where any valid API key bypassed scope limits).

### Web Dashboard
- Tech stack: React 19 + TypeScript + Tailwind CSS 4 + Base UI + the shared `mds` component library.
- Real-time log streaming (BroadcastLayer → WebSocket).
- OrgChart (D3.js interactive agent hierarchy).
- Memory page with Key Insights tab (`key_facts` cards with access_count badges) + Self-Improvement tab (stagnation warnings, rejection statistics, playbook rule cards).
- Logs page with source filter chips + severity dropdown + severity-colored left borders + JSON detail expansion.
- Toast notification system (module-scoped event bus, max-5 queue, warm variants).
- Skill Market 3-tab (Marketplace / Shared Skills / My Skills).
- Autopilot settings + Session Replay + WikiGraph.
- **Reliability page** — evolution-event query and per-agent reliability summary (`audit.evolutionQuery`, `audit.reliabilitySummary`).
- i18n: zh-TW / en / ja-JP (600+ translation keys).
- Dark/Light theme (system + manual toggle).
