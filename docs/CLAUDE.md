# DuDuClaw Architecture Overview

## Architecture Overview (v1.9.4)

DuDuClaw is a **Multi-Runtime AI Agent Platform** — supporting **Claude Code / Codex / Gemini** CLI as AI backends via a unified `AgentRuntime` trait with auto-detection and per-agent configuration. DuDuClaw is not a standalone LLM product; it is the plumbing layer that turns one (or many) AI CLIs into long-running agents with channel routing, session memory, self-evolution, multi-account rotation, local LLM inference, browser automation, and IDE integration.

## Key Architectural Decisions

### Runtime & Transport
- **Multi-Runtime** (`AgentRuntime` trait) — Claude / Codex / Gemini / OpenAI-compat four backends, `RuntimeRegistry` auto-detection, per-agent config in `agent.toml [runtime]`.
- **MCP Server (stdio)** (`duduclaw mcp-server`) exposes channel, memory, agent, skill, task, shared wiki, and autopilot tools to AI Runtime via JSON-RPC 2.0 over stdin/stdout. Registered at the agent level in `<agent>/.mcp.json` (v1.8.5 reverted v1.8.4's global registration because Claude CLI `-p --dangerously-skip-permissions` only reads project-level `.mcp.json`). Gateway startup auto-creates/repairs `.mcp.json` for all agents.
- **MCP Server (HTTP/SSE)** (`duduclaw http-server --bind 127.0.0.1:8765`, v1.9.4) — Bearer-authenticated `POST /mcp/v1/call` (single JSON-RPC tool call), `GET /mcp/v1/stream` (SSE long-lived event stream, Bearer / `?api_key=`), `POST /mcp/v1/stream/call` (async + SSE result push), `GET /healthz` (no auth). Token bucket rate limit (60 req/min). `mcp_sse_store.rs` manages SSE connections with broadcast channels. Complements stdio for external HTTP clients.
- **ACP/A2A Server** (`duduclaw acp-server`) — stdio JSON-RPC 2.0 loop with `agent/discover`, `tasks/send`, `tasks/get`, `tasks/cancel` methods plus `.well-known/agent.json` AgentCard output. Enables Zed / JetBrains / Neovim IDE integration via Agent Client Protocol.
- **Agent directories** are Claude Code compatible: each contains `.claude/`, `.mcp.json`, `SOUL.md`, `CLAUDE.md`, `CONTRACT.toml`, `agent.toml`, `wiki/`, `SKILLS/`, `memory/`, `tasks/`, `state/`.

### Channels (7 + Generic Webhook)
- **Telegram** (long polling) — file/photo/sticker/voice, forums/topics, mention-only, Whisper transcription.
- **LINE** (webhook) — HMAC-SHA256 signature, sticker catalog, per-chat settings.
- **Discord** (Gateway WebSocket) — `tokio::select!` heartbeat, slash commands, auto-thread, voice channels (Songbird). v1.9.2 hardened: real op 6 RESUME (persists `session_id` + `resume_gateway_url` + sequence), stall watchdog (break if no traffic for 2× heartbeat interval), heartbeat capacity 1→16 with `try_send`, op 9 jitter 1-5s, RESUMED dispatch handling, backoff cap 60s.
- **Slack** (Socket Mode), **WhatsApp** (Cloud API), **Feishu** (Open Platform v2), **WebChat** (`/ws/chat` + React frontend).
- **Generic Webhook**: `POST /webhook/{agent_id}` + HMAC-SHA256.
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
- **Hermes-inspired turn trimming** (>800 chars → head 300 + tail 200 + `[trimmed N chars]`, CJK-safe).
- **Direct API prompt cache** ("system_and_3" breakpoint strategy, ~75% cache hit on multi-turn; 95%+ on pure system-prompt cache).
- **Compression summaries** injected into system prompt (not conversation turns) at 50k token threshold.
- **Instruction Pinning** (v1.8.6 P0) — first user turn → async Haiku extraction of core task → stored in `sessions.pinned_instructions` → injected at system prompt tail (U-shaped attention). Clarification answers accumulate (≤1000 chars).
- **Snowball Recap** (v1.8.6 P0) — every turn prepends `<task_recap>` to the user message. Zero LLM cost.
- **P2 Key-Fact Accumulator** (v1.8.6) — per substantive turn, Haiku extracts 2-4 key facts → `key_facts` table with FTS5 → top-3 relevant facts injected into system prompt. ~100-150 tokens vs MemGPT's 6,500 (−87%).
- **CLI lightweight path** — `call_claude_cli_lightweight()` with `--effort medium --max-turns 1 --no-session-persistence --tools ""` for metadata tasks. 25-40% cost reduction.
- **Stabilization flags** — `--strict-mcp-config` (MCP isolation) + `--exclude-dynamic-system-prompt-sections` (cross-turn prompt stability, 10-15% token reduction). `--bare` was removed in v1.8.11 (broke OS keychain credential lookup).

### Evolution
- **Prediction-driven engine**: Active Inference + Dual Process Theory, ~90% zero LLM cost. Negligible/Moderate errors → zero cost; Significant → GVU reflection; Critical → emergency GVU loop.
- **MetaCognition**: self-calibrating error thresholds every 100 predictions; drives Adaptive Depth (3-7 GVU rounds).
- **GVU² self-play loop** (Generator→Verifier→Updater): TextGrad feedback, 4+2 layer verification (L1-Format / L2-Metrics / L2.5-MistakeRegression / L3-LLMJudge / L3.5-SandboxCanary / L4-Safety).
- **Deferred GVU**: gradient accumulation + delayed retry (max 3 deferrals, 72h span, 9-21 effective rounds).
- **MistakeNotebook**: cross-loop error memory prevents regression.
- **SOUL.md versioning**: 24h observation period + auto-rollback, atomic write (temp + rename) with SHA-256 fingerprint.
- **Agent-as-Evaluator**: independent Evaluator Agent (Haiku cost control) for adversarial verification with structured JSON verdicts.
- **ConversationOutcome**: zero-LLM conversation result detection (TaskType / Satisfaction / Completion) in zh-TW + en.
- **External factors**: user feedback, security events, channel metrics, Odoo business context, peer agent signals feed into prediction engine and GVU reflections.

### Wiki Knowledge Layer (v1.8.9)
- **4-layer architecture** (Vault-for-LLM inspired): L0 Identity / L1 Core / L2 Context / L3 Deep.
- **Trust weighting** (`trust` 0.0-1.0 frontmatter) — search results ranked by trust-weighted score.
- **Auto-injection**: `build_system_prompt()` auto-injects L0+L1 pages into WIKI_CONTEXT across CLI / channel reply / dispatcher paths — unified across Claude / Codex / Gemini / OpenAI runtimes.
- **FTS5 index** (`unicode61` tokenizer) — auto-syncs on every write/delete, manual rebuild via `wiki_rebuild_fts`.
- **Knowledge graph**: `wiki_graph` MCP tool exports BFS-limited Mermaid diagrams; node shapes by layer.
- **Dedup detection**: `wiki_dedup` detects duplicate pages by title match + tag Jaccard similarity (≥0.8).
- **Reverse backlink index**: scans `related` frontmatter + body markdown links for bidirectional mapping.
- **Search filters**: `wiki_search` / `shared_wiki_search` support `min_trust`, `layer`, `expand` (1-hop backlink expansion).
- **Shared Wiki**: `~/.duduclaw/shared/wiki/` for cross-agent SOPs, policies, product specs. Visibility controlled via `wiki_visible_to` capability.

### Memory System
- **Cognitive memory** (optional): `SqliteMemoryEngine` with episodic/semantic separation and Generative Agents 3D-weighted retrieval (Recency × Importance × Relevance).
- **Memory decay daily scheduler**: background task runs `duduclaw_memory::decay::run_decay` every 24h. Low-importance + 30 days old → archived. Archived + 90 days → permanent delete.
- **Cognitive memory MCP tools**: `memory_search_by_layer` (episodic/semantic filter), `memory_successful_conversations`, `memory_episodic_pressure`, `memory_consolidation_status`.
- **MemGPT 3-layer system** (Core Memory, Recall Memory, Archival Bridge, Budget Manager, Consolidation Pipeline, 6 MCP tools) was **removed in v1.8.1** (−1,985 LOC) — the prompt injection caused 6,500 token bloat per prompt and "lost in the middle" attention degradation.

### Worktree Isolation (v1.6.0)
- **Git worktree L0 isolation layer** — per-task filesystem isolation cheaper than container sandbox.
- **WorktreeManager**: create / remove / list / cleanup_stale lifecycle.
- **Atomic merge**: dry-run pre-check → abort → real merge if clean. Protected by global `Mutex`.
- **Snap workflow**: create → execute → inspect → merge/cleanup (pure-function decision logic for testability).
- **Branch naming**: `wt/{agent_id}/{adjective}-{noun}` from 50×50 word lists.
- **copy_env_files**: path traversal jail + symlink rejection + 1MB size limit.
- **Resource limits**: max 5 worktrees per agent, 20 total.

### Local Inference
- **Unified `InferenceBackend` trait** (`duduclaw-inference` crate): llama.cpp (Metal/CUDA/Vulkan/CPU), mistral.rs (ISQ + PagedAttention + Speculative Decoding), OpenAI-compatible HTTP (Exo/llamafile/vLLM/SGLang).
- **Confidence Router**: three-tier LocalFast / LocalStrong / CloudAPI routing, CJK-aware token estimation.
- **InferenceManager**: auto-switching state machine — Exo P2P → llamafile → Direct backend → OpenAI-compat → Cloud API.
- **Exo P2P cluster** (`exo_cluster.rs`): distributed inference, 235B+ models across machines.
- **llamafile manager**: subprocess lifecycle, health monitoring, OpenAI-compatible API on localhost.
- **MLX bridge**: Python subprocess calling `mlx_lm` on Apple Silicon for local reflections + LoRA.
- **MCP tools**: `model_list`, `model_load`, `model_unload`, `inference_status`, `hardware_info`, `route_query`, `inference_mode`, `llamafile_start/stop/list`, `compress_text`, `decompress_text`.

### Token Compression
- **Meta-Token (LTSC)** — Rust-native lossless BPE-like, 27-47% compression on structured input.
- **LLMLingua-2** — Microsoft token-importance pruning, 2-5× lossy compression.
- **StreamingLLM** — attention sink + sliding window KV-cache.
- **Strategy selector**: `compress_text` accepts `strategy` param (meta_token / llmlingua / streaming_llm / auto).

### Voice Pipeline
- **ASR**: Whisper.cpp (local) / SenseVoice ONNX (local) / OpenAI Whisper API / Deepgram (streaming).
- **TTS**: Piper ONNX (local) / MiniMax T2A / Edge TTS / OpenAI TTS.
- **VAD**: Silero ONNX.
- **Audio decode**: symphonia (OGG Opus, MP3, AAC, WAV, FLAC → PCM).
- **Discord Voice** (Songbird) + **LiveKit** multi-agent voice rooms.
- **ONNX Embedding**: BERT WordPiece tokenizer + ONNX Runtime vector embedding.

### Security
- **Claude Code security hooks** (`.claude/hooks/`): 3-phase progressive defense — Layer 1 deterministic blacklist (<50ms), Layer 2 obfuscation/exfiltration detection (YELLOW+), Layer 3 Haiku AI judgment (RED only).
- **Threat level state machine**: GREEN → YELLOW → RED with auto-escalation/demotion (24h no-event → −1 level).
- **SOUL.md drift detection** (SHA-256 fingerprint).
- **Prompt injection scanner** (6 rule categories + XML delimiter protection).
- **Secret leak scanner** — 20+ patterns (Anthropic/OpenAI/AWS/GitHub/Slack/Stripe/DB URLs).
- **CONTRACT.toml** — `must_not` / `must_always` boundaries, auto-injected into system prompt; `duduclaw test` red-team CLI (9 built-in scenarios).
- **Unified multi-source audit log**: `audit.unified_log` merges `security_audit.jsonl` / `tool_calls.jsonl` / `channel_failures.jsonl` / `feedback.jsonl` into common envelope (timestamp / source / event_type / agent_id / severity / summary / details) with Logs page filter chips.
- **AES-256-GCM** at rest — per-agent key isolation.
- **Ed25519 challenge-response** WebSocket auth.
- **Container sandbox** (Docker / Apple Container / WSL2) — `--network=none`, tmpfs, read-only rootfs, 512MB limit.
- **Browser automation** (5-layer auto-routing): L1 API Fetch → L2 Static Scrape → L3 Headless → L4 Sandbox Container → L5 Computer Use. Deny-by-default via `CapabilitiesConfig`; `bash-gate.sh` Layer 1.5 allowlist for Playwright/Puppeteer.
- **CJK-safe byte slicing**: `duduclaw_core::truncate_bytes` / `truncate_chars` replaced 31 unsafe `s[..s.len().min(N)]` sites (fixed v1.8.11 multi-byte codepoint panics).

### Accounts & Cost
- **Per-agent model routing** (SDK-first): `agent.toml [model]` — `preferred` (Claude SDK model), `local.model`, `local.use_router`, `api_mode` (cli/direct/auto).
- **Multi-OAuth account rotation**: OAuth sessions (Claude Pro/Team/Max via `claude auth status` + `CLAUDE_CODE_OAUTH_TOKEN` for `setup-token` accounts) + API keys. 4 strategies (Priority/LeastCost/Failover/RoundRobin). Rate-limit cooldown (2min), billing-exhaustion cooldown (24h), budget enforcement, token expiry tracking (30d/7d warnings).
- **Dual dispatch path**: both sub-agent dispatcher (`claude_runner::call_with_rotation`) and user-facing channel reply (`channel_reply::call_claude_cli_rotated` → `rotate_cli_spawn`) go through the rotator.
- **`FailureReason` classification** — RateLimited / Billing / Timeout / BinaryMissing / SpawnError / EmptyResponse / NoAccounts / Unknown — with category-specific zh-TW user messages and `channel_failures.jsonl` audit records.
- **Binary discovery**: `which_claude()` / `which_claude_in_home()` probe Homebrew (Intel + Apple Silicon), Bun, Volta, npm-global, `.claude/bin`, `.local/bin`, asdf shims, NVM version directories — fixes launchd-launched gateway binary discovery when `PATH` is empty.
- **CostTelemetry**: SQLite-backed token usage tracking with cache efficiency analytics (`cache_read / (input + cache_read + cache_creation)`), 200K price cliff warning, adaptive routing (cache_eff <30% → local). MCP tools: `cost_summary`, `cost_agents`, `cost_recent`.
- **Direct API client** (`direct_api.rs`): bypasses Claude CLI for pure chat, `cache_control: ephemeral` on system prompt → 95%+ cache hit rate. Singleton `reqwest::Client` with 120s timeout; used as fallback when all OAuth accounts are cooling.

### Scheduling
- **HeartbeatScheduler**: per-agent unified scheduling — bus polling + GVU silence breaker + cron, `max_concurrent_runs` semaphore.
- **CronScheduler**: reads `cron_tasks.jsonl` (+ `cron_tasks.db` since v1.8.12), fires tasks on cron expression. `list_cron_tasks` returns all tasks (no longer filters by default_agent, v1.8.3). `schedule_task` MCP tool schema corrected (v1.8.12) to include `agent_id` and `name` fields.
- **ReminderScheduler**: one-shot reminders (relative `5m`/`2h`/`1d` or ISO 8601), `direct` static message or `agent_callback` wake-up mode.

### Skill Ecosystem
- **7-stage lifecycle**: Activation → Compression → Extraction → Reconstruction → Distillation → Diagnostician → Gap Analysis.
- **GitHub live indexing** — Search API with 24h local cache, weighted search.
- **Skill auto-synthesis** (Phase 3-4): gap accumulator detects repeated domain gaps → synthesizes skills from episodic memory (Voyager-inspired) → sandbox trial with TTL → cross-agent graduation. MCP tools: `skill_security_scan`, `skill_graduate`, `skill_synthesis_status`.
- **Python Skill Vetter** subprocess for security scanning.

### Task & Knowledge
- **Task Board**: SQLite-backed task management with status/priority/assignment tracking + real-time Activity Feed WebSocket. MCP tools: `tasks.list/create/update/assign`, `activity.list/subscribe`.
- **Shared Knowledge Base**: `~/.duduclaw/shared/wiki/` with Wiki target classification (agent/shared/both). MCP tools: `shared_wiki_ls/read/write/search/delete/stats`, `wiki_share`.
- **Autopilot rule engine**: automated delegation/notifications/skill execution. Triggers: task creation, status change, channel message, idle detection, cron schedule.

### Integrations
- **Odoo ERP bridge** (`duduclaw-odoo` crate): JSON-RPC middleware supporting CE/EE, 15 MCP tools (CRM/Sales/Inventory/Accounting), EditionGate auto-detection, event polling + webhook.
- **Prometheus metrics**: `GET /metrics` on gateway HTTP — requests, tokens, duration histogram, channel status.
- **RL trajectory collector**: writes per-agent trajectories to `~/.duduclaw/rl_trajectories.jsonl` during channel interactions. `duduclaw rl export|stats|reward` CLI (composite reward: outcome × 0.7 + efficiency × 0.2 + overlong × 0.1).
- **BroadcastLayer** tracing layer streams real-time logs to WebSocket subscribers.
- **Dashboard WebSocket heartbeat**: server Ping every 30s, close idle sockets after 60s without Pong. Client `ping` application-level RPC every 25s (browsers can't issue control frames).

### Reliability & Governance (v1.9.4)
- **`duduclaw-durability` crate** — five-pillar durability:
  - `idempotency.rs`: key-based dedup preventing duplicate execution.
  - `retry.rs`: exponential backoff with jitter strategy.
  - `circuit_breaker.rs`: three-state Closed / Open / HalfOpen with `probe_inflight` accounting (v1.9.4 fix: OPEN→HALF_OPEN transition increments `probe_inflight` to prevent ghost-probe overage).
  - `checkpoint.rs`: resumable task progress.
  - `dlq.rs`: Dead Letter Queue for terminally failed messages.
- **`duduclaw-governance` crate** (W19-P1 M1-A) — `PolicyRegistry` (YAML + hot reload + agent-priority merge + fail-safe + concurrent upsert safety), four `PolicyType`s (Rate / Permission / Quota / Lifecycle), `quota_manager.rs` (per-agent / per-policy soft + hard quotas), `error_codes.rs` (QUOTA_EXCEEDED / POLICY_DENIED / ...), approval workflow + audit log. Default policy set in `policies/global.yaml`.
- **LLM fallback chain** (`gateway/llm_fallback.rs`) — primary timeout / 503 / 429 / overloaded auto-switches to fallback model. `is_llm_fallback_error` / `should_attempt_model_fallback` are pure functions with unit tests. UTF-8-safe truncation via `char_indices`.
- **Evolution Events system** (`gateway/evolution_events/`) — 30+ event schema, async batch+retry emitter, query interface, reliability guarantees. HTTP endpoints exposed on gateway and surfaced in Web `ReliabilityPage`.

### Memory Evaluation (v1.9.4 / W21)
- **LOCOMO evaluation** (`python/duduclaw/memory_eval/`) — `retrieval_accuracy`, `retention_rate`, `locomo_integrity_check`. `cron_runner` triggered daily at 03:00 UTC. 5-minute `smoke_test` P0 verifies basic memory functions. `build_golden_qa.py` builds the gold QA set; `data/golden_qa_set.jsonl` carries the first 200 entries. `duduclaw-memory` engine adds batch query API for evaluation.
- **Python `agents/` + `mcp/` modules** — `agents/capabilities/` (manifest + matcher), `agents/routing/` (router + resolution + memory_resolver). `mcp/auth/` (API Key with key masking), `mcp/tools/memory/` (store / read / search / namespace / quota with strict scope enforcement at `execute()` entry — patches a v1.9.3 auth gap where any valid API key bypassed scope limits).

### Web Dashboard (24 pages)
- Tech stack: React 19 + TypeScript + Tailwind CSS 4 + shadcn/ui, warm amber theme.
- Real-time log streaming (BroadcastLayer → WebSocket).
- OrgChart (D3.js interactive agent hierarchy).
- Memory page with Key Insights tab (`key_facts` cards with access_count badges) + Evolution tab (SOUL.md version history with pre/post deltas).
- Logs page with source filter chips + severity dropdown + severity-colored left borders + JSON detail expansion.
- Toast notification system (module-scoped event bus, max-5 queue, warm variants).
- Skill Market 3-tab (Marketplace / Shared Skills / My Skills).
- Autopilot settings + Session Replay + WikiGraph.
- **ReliabilityPage** (v1.9.4, `/reliability` route) — circuit breaker state, retry stats, DLQ depth dashboard. Fetches `getEvolutionEvents` / `getReliabilityStats` / `getDlqItems`.
- i18n: zh-TW / en / ja-JP (600+ translation keys).
- Dark/Light theme (system + manual toggle).
