# DuDuClaw Complete Feature Inventory

> v1.8.14 | Last updated: 2026-04-21

---

## Core Architecture

| Feature | Description |
|---------|-------------|
| Multi-Runtime AI Agent Platform | Unified `AgentRuntime` trait — Claude / Codex / Gemini / OpenAI-compat four backends with auto-detection |
| MCP Server (JSON-RPC 2.0) | Exposes 80+ tools to AI Runtime via stdin/stdout; registered at `<agent>/.mcp.json` (v1.8.5 — Claude CLI `-p` only reads project-level), gateway auto-creates/repairs on startup |
| ACP/A2A Server | `duduclaw acp-server` — stdio JSON-RPC 2.0 with `agent/discover` / `tasks/send` / `tasks/get` / `tasks/cancel`; `.well-known/agent.json` AgentCard; IDE integration (Zed / JetBrains / Neovim) |
| Agent Directory Structure | `.claude/`, `.mcp.json`, `SOUL.md`, `CLAUDE.md`, `CONTRACT.toml`, `agent.toml`, `wiki/`, `SKILLS/`, `memory/`, `tasks/`, `state/` |
| Sub-agent Orchestration | `create_agent` / `spawn_agent` / `list_agents` with `reports_to` hierarchy + D3.js OrgChart + "## Your Team" auto-injection |
| DelegationEnvelope | Structured handoff protocol — context / constraints / task_chain / expected_output |
| TaskSpec Workflow | Multi-step task planning — dependency-aware scheduling, auto-retry (3x), replan (2x), persistence |
| Long-Response Splitting | Sub-agent replies > channel byte budget split via `channel_format::split_text` with paginated labels `📨 **agent** 的回報 (1/N)` |
| Orphan Response Recovery | `reconcile_orphan_responses` replays `bus_queue.jsonl` `agent_response` callbacks left by crash / Ctrl+C / hotswap |
| File-based IPC | `bus_queue.jsonl` for inter-agent delegation, max 5 hop tracking |
| Per-Agent Channel Token | `get_agent_channel_token` reads per-agent `bot_token_enc` first (fixes Discord thread cross-bot 401s) |

## Multi-Runtime

| Feature | Description |
|---------|-------------|
| Claude Runtime | Claude Code SDK (`claude` CLI) with JSONL streaming + `--resume` multi-turn |
| Codex Runtime | OpenAI Codex CLI with `--json` streaming events, `AGENTS.md` file for system prompt |
| Gemini Runtime | Google Gemini CLI with `--output-format stream-json`, `GEMINI_SYSTEM_MD` env var for system prompt, `--approval-mode yolo` |
| OpenAI-compat Runtime | HTTP endpoint (MiniMax / DeepSeek / etc.) via REST API |
| RuntimeRegistry | Auto-detection of installed CLIs, per-agent `[runtime]` config |
| Cross-Provider Failover | `FailoverManager` health tracking, cooldown, non-retryable error detection |

## Session Memory Stack (v1.8.1 + v1.8.6)

| Feature | Description |
|---------|-------------|
| Native Multi-Turn | Claude CLI `--resume` + SHA-256 deterministic session ID + history-in-prompt fallback (stale session, account rotation, unknown stream-json error) |
| Hermes Turn Trimming | >800 chars → head 300 + tail 200 + `[trimmed N chars]`, CJK-safe char-level slicing |
| Prompt Cache Strategy | Direct API "system_and_3" breakpoint placement, ~75% multi-turn hit rate |
| Compression Summary Injection | Post-compression summaries (role=system) injected into system prompt, not conversation turns |
| Instruction Pinning | First user message → async Haiku extraction → `sessions.pinned_instructions` → injected at system prompt tail |
| Snowball Recap | Each turn prepends `<task_recap>` to user message — zero LLM cost, U-shaped attention tail |
| Clarification Accumulation | Agent-question + user-answer appended to pinned instructions (≤1000 chars) |
| P2 Key-Fact Accumulator | 2-4 facts per substantive turn → `key_facts` FTS5 table → top-3 injected (~100-150 tokens vs MemGPT 6,500, −87%) |
| CLI Lightweight Path | `call_claude_cli_lightweight()` — `--effort medium --max-turns 1 --no-session-persistence --tools ""`, 25-40% cost reduction |
| Stabilization Flags | `--strict-mcp-config` + `--exclude-dynamic-system-prompt-sections` (10-15% token reduction); `--bare` removed v1.8.11 (broke OAuth keychain) |
| CJK-Safe String Slicing | `duduclaw_core::truncate_bytes` / `truncate_chars` replaced 31 unsafe byte-index sites |

## Communication Channels (7)

| Channel | Protocol |
|---------|----------|
| Telegram | Long polling, file/photo/sticker/voice, forums/topics, mention-only, voice transcription |
| LINE | Webhook, HMAC-SHA256 signature, sticker support, per-chat settings |
| Discord | Gateway WebSocket, slash commands (`/ask /status /config /session /agent`), voice channels (Songbird), auto-thread (session id stable across entire thread lifetime post-v1.8.14), embed replies |
| Slack | Socket Mode, mention-only, thread replies |
| WhatsApp | Cloud API |
| Feishu | Open Platform v2 |
| WebChat | Embedded `/ws/chat` WebSocket + React frontend (Zustand store) |
| Channel Hot-Start/Stop | Dashboard-driven dynamic launch/termination |
| Generic Webhook | `POST /webhook/{agent_id}` + HMAC-SHA256 signature verification |
| Media Pipeline | Auto-resize (max 1568px) + MIME detection + Vision integration |
| Sticker System | LINE sticker catalog + emotion detection + Discord emoji equivalents |
| Channel Failure Tracking | `channel_failures.jsonl` with `FailureReason` enum (RateLimited/Billing/Timeout/BinaryMissing/SpawnError/EmptyResponse/NoAccounts/Unknown) |

## Evolution System

| Feature | Description |
|---------|-------------|
| Prediction-Driven Engine | Active Inference + Dual Process Theory, ~90% zero LLM cost |
| Dual Process Router | System 1 (rules) / System 2 (LLM reflection) |
| GVU² Dual-Loop | Outer loop (Behavioral GVU — SOUL.md) + Inner loop (Task GVU — instant retry) |
| 4+2 Layer Verification | L1-Format / L2-Metrics / L2.5-MistakeRegression / L3-LLMJudge / L3.5-SandboxCanary / L4-Safety |
| MistakeNotebook | Cross-loop error memory — records failure patterns, prevents regression |
| SOUL.md Versioning | 24h observation period, atomic rollback, SHA-256 fingerprint |
| MetaCognition | Self-calibrating error thresholds every 100 predictions |
| Adaptive Depth | MetaCognition-driven GVU iteration count (3-7 rounds based on history) |
| Deferred GVU | Gradient accumulation + delayed retry (max 3 deferrals, 72h span, 9-21 effective rounds) |
| ConversationOutcome | Zero-LLM conversation result detection (TaskType / Satisfaction / Completion), zh-TW + en |
| Agent-as-Evaluator | Independent Evaluator Agent (Haiku cost control) for adversarial verification, structured JSON verdict |
| Orchestrator Template | 5-step planning (Analyze → Decompose → Delegate → Evaluate → Synthesize) + complexity routing |

## Wiki Knowledge Layer (v1.8.9)

| Feature | Description |
|---------|-------------|
| 4-Layer Architecture | L0 Identity / L1 Core / L2 Context / L3 Deep — Vault-for-LLM inspired |
| Trust Weighting | `trust` (0.0-1.0) frontmatter; search ranked by trust-weighted score |
| Auto-Injection | `build_system_prompt()` injects L0+L1 into WIKI_CONTEXT across CLI / channel / dispatcher paths |
| FTS5 Full-Text Index | SQLite `unicode61` tokenizer with CJK support, auto-syncs on write/delete, manual rebuild `wiki_rebuild_fts` |
| Knowledge Graph | `wiki_graph` MCP tool exports BFS-limited Mermaid diagrams; node shapes by layer |
| Dedup Detection | `wiki_dedup` — title match + tag Jaccard similarity (≥0.8) |
| Reverse Backlink Index | Scans `related` frontmatter + body markdown links for bidirectional mapping |
| Search Filters | `min_trust` / `layer` / `expand` (1-hop related/backlink expansion) |
| Shared Wiki | `~/.duduclaw/shared/wiki/` cross-agent SOPs + policies + specs; `wiki_visible_to` capability control |
| CLAUDE_WIKI Template | Included in agent CLAUDE.md on creation, provides wiki MCP tool usage guide |

## Skill Ecosystem

| Feature | Description |
|---------|-------------|
| 7-Stage Lifecycle | Activation → Compression → Extraction → Reconstruction → Distillation → Diagnostician → Gap Analysis |
| GitHub Live Indexing | Search API with 24h local cache, weighted search |
| Skill Marketplace | Web dashboard browsing, installation, security scanning |
| Skill Auto-Synthesis | Gap accumulator → synthesize from episodic memory (Voyager-inspired) → sandbox trial with TTL → cross-agent graduation |
| Python Skill Vetter | Subprocess-based security scanning for candidate skills |

## Local Inference Engine

| Feature | Description |
|---------|-------------|
| llama.cpp | Metal/CUDA/Vulkan/CPU via `llama-cpp-2` crate |
| mistral.rs | Rust-native, ISQ on-the-fly quantization, PagedAttention, Speculative Decoding |
| OpenAI-compatible HTTP | Exo/llamafile/vLLM/SGLang |
| Confidence Router | LocalFast / LocalStrong / CloudAPI three-tier routing, CJK-aware token estimation |
| InferenceManager | Multi-mode auto-switching: Exo P2P → llamafile → Direct → OpenAI-compat → Cloud API |
| llamafile Manager | Subprocess lifecycle, zero-install portable inference across 6 OS |
| Exo P2P Cluster | Distributed inference, 235B+ models across machines, cluster discovery, endpoint failover |
| MLX Bridge | Apple Silicon local reflections via `mlx_lm` + LoRA adapter support |
| Model Management | `model_search` (HuggingFace), `model_download` (resume + mirror), `model_recommend` (hardware-aware) |

## Compression Engine

| Feature | Description |
|---------|-------------|
| Meta-Token (LTSC) | Rust-native lossless BPE-like, 27-47% compression on structured input |
| LLMLingua-2 | Microsoft token-importance pruning, 2-5x lossy compression |
| StreamingLLM | Attention sink + sliding window KV-cache for infinite conversations |
| Strategy Selector | `compress_text` accepts `strategy` param — `meta_token` / `llmlingua` / `streaming_llm` / `auto` |

## Voice Pipeline

| Feature | Description |
|---------|-------------|
| ASR (Speech-to-Text) | Whisper.cpp (local) / SenseVoice ONNX (local) / OpenAI Whisper API / Deepgram (streaming) |
| TTS (Text-to-Speech) | Piper ONNX (local) / MiniMax T2A (auto-detect CJK/Latin) / Edge TTS / OpenAI TTS |
| VAD | Silero ONNX voice activity detection |
| Audio Decode | symphonia: OGG Opus, MP3, AAC, WAV, FLAC → PCM |
| Discord Voice | Songbird integration, voice channel participation |
| LiveKit Voice | WebRTC multi-agent voice rooms |
| ONNX Embedding | BERT WordPiece tokenizer + ONNX Runtime vector embedding |

## Security

| Feature | Description |
|---------|-------------|
| 3-Phase Defense | Deterministic blacklist (<50ms) / obfuscation detection (YELLOW+) / AI judgment (RED only) |
| Threat Level State Machine | GREEN → YELLOW → RED auto-escalation, 24h no-event auto-demotion |
| Ed25519 Auth | Challenge-response WebSocket authentication |
| AES-256-GCM | API key encryption at rest, per-agent key isolation |
| Prompt Injection Scanner | 6 rule categories + XML delimiter protection |
| SOUL.md Drift Detection | SHA-256 fingerprint comparison |
| CONTRACT.toml | Behavioral boundaries + `duduclaw test` red-team CLI (9 built-in scenarios); auto-injected into system prompt for all runtimes |
| RBAC | Role-based access control matrix |
| Unified Audit Log | `audit.unified_log` merges `security_audit.jsonl` / `tool_calls.jsonl` / `channel_failures.jsonl` / `feedback.jsonl` — Logs page source filter + severity dropdown |
| JSONL Audit Log | Full tool call recording, async write |
| Unicode Normalization | NFKC normalization to detect homograph attacks |
| Action Claim Verifier | Signature validation for tool execution claims |
| Container Sandbox | Docker (Bollard) / Apple Container / WSL2 — `--network=none`, tmpfs, read-only rootfs, 512MB limit |
| Secret Leak Scanner | 20+ patterns (Anthropic/OpenAI/AWS/GitHub/Slack/Stripe/DB URLs) |

## Memory System

| Feature | Description |
|---------|-------------|
| Episodic / Semantic Separation | Generative Agents 3D-weighted retrieval (Recency + Importance + Relevance) |
| Full-Text Search (FTS5) | SQLite built-in |
| Vector Index | Embedding-based semantic search (ONNX BERT / Qwen3-Embedding) |
| Memory Decay Scheduler | Daily background task — low-importance + 30d old → archived, archived + 90d → permanent delete |
| Cognitive Memory MCP Tools | `memory_search_by_layer`, `memory_successful_conversations`, `memory_episodic_pressure`, `memory_consolidation_status` |
| Federated Memory | Cross-agent knowledge sharing (Private / Team / Public levels) |
| Key-Fact Accumulator | `key_facts` table with FTS5 — cross-session lightweight memory (see Session Memory Stack) |

## Git Worktree Isolation (v1.6.0)

| Feature | Description |
|---------|-------------|
| L0 Isolation Layer | Per-task git worktree — cheaper than container sandbox, prevents concurrent agent file collisions |
| Atomic Merge | Dry-run pre-check → abort → real merge if clean; protected by global `Mutex` |
| Snap Workflow | create → execute → inspect → merge/cleanup; pure-function decision logic |
| Friendly Branch Names | `wt/{agent_id}/{adjective}-{noun}` from 50×50 word lists |
| copy_env_files | Path traversal jail, symlink rejection, 1MB size limit |
| AgentExitCode | Structured exit codes — Success / Error / Retry / KeepAlive |
| Resource Limits | Max 5 worktrees per agent, 20 total |

## Account & Cost Management

| Feature | Description |
|---------|-------------|
| Multi-Account Rotation | OAuth + API Key, 4 strategies (Priority/LeastCost/RoundRobin/Failover) |
| Dual Dispatch Path | Both sub-agent dispatcher (`claude_runner::call_with_rotation`) and channel reply (`channel_reply::call_claude_cli_rotated`) go through rotator |
| CostTelemetry | SQLite token tracking + cache efficiency analytics + 200K price cliff warning |
| Budget Manager | Per-account monthly limits + cooldown + adaptive routing (cache_eff <30% → local) |
| Direct API | Bypass CLI, `cache_control: ephemeral`, 95%+ cache hit rate |
| Channel Failure Tracking | `channel_failures.jsonl` with category-specific zh-TW messages |
| Binary Discovery | `which_claude()` / `which_claude_in_home()` probe Homebrew (Intel + Apple Silicon) / Bun / Volta / npm-global / `.claude/bin` / `.local/bin` / asdf / NVM |

## Browser Automation

| Feature | Description |
|---------|-------------|
| 5-Layer Router | API Fetch / Static Scrape / Headless Playwright / Sandbox Container / Computer Use |
| Capability Gating | `agent.toml [capabilities]` deny-by-default |
| Browserbase | Cloud browser alternative for L5 |
| bash-gate.sh | Layer 1.5 allowlist for Playwright/Puppeteer (requires `DUDUCLAW_BROWSER_VIA_BASH=1`) |

## Container Sandbox

| Feature | Description |
|---------|-------------|
| Docker | Bollard API, all platforms |
| Apple Container | Native macOS 15+ |
| WSL2 | Windows Linux subsystem |

## Scheduling

| Feature | Description |
|---------|-------------|
| CronScheduler | `cron_tasks.jsonl` + `cron_tasks.db` persistent (v1.8.12), `schedule_task` MCP tool with corrected schema including `agent_id` + `name` |
| ReminderScheduler | One-shot reminders (relative `5m`/`2h`/`1d` or ISO 8601), `direct` or `agent_callback` mode |
| HeartbeatScheduler | Per-agent unified scheduling — bus polling + GVU silence breaker + cron |

## ERP Integration

| Feature | Description |
|---------|-------------|
| Odoo Bridge | 15 MCP tools (CRM/Sales/Inventory/Accounting), JSON-RPC middleware |
| Edition Gate | CE/EE auto-detection, feature gating |
| Event Polling | Proactive agent notifications on Odoo state changes |
| Per-Agent Credential Isolation | `OdooConnectorPool` keyed by `(agent_id, profile)`; audit log carries `profile` + `ok=bool` (v1.11.0 / RFC-21 §2) |
| Dashboard Test-Before-Save | `odoo.test` accepts inline params; missing credential falls back to stored secret; inline mode reuses the same SSRF / HTTPS / db-name validators (v1.13.1) |

## RL & Observability

| Feature | Description |
|---------|-------------|
| RL Trajectory Collector | Writes `~/.duduclaw/rl_trajectories.jsonl` during channel interactions |
| `duduclaw rl` CLI | `export` / `stats` / `reward` — composite reward (outcome × 0.7 + efficiency × 0.2 + overlong × 0.1) |
| Prometheus Metrics | `GET /metrics` — requests, tokens, duration histogram, channel status |
| Dashboard WebSocket Heartbeat | Server Ping 30s + 60s idle close; client `ping` RPC 25s |
| BroadcastLayer | Tracing layer streams real-time logs to WebSocket subscribers |

## Web Dashboard

| Feature | Description |
|---------|-------------|
| 23 Pages | Dashboard / Agents / Channels / Accounts / Memory / Security / Settings / OrgChart / SkillMarket / Logs / WebChat / OnboardWizard / Billing / License / Report / PartnerPortal / Marketplace / KnowledgeHub / Odoo / Login / Users / Analytics / Export |
| Tech Stack | React 19 + TypeScript + Tailwind CSS 4 + shadcn/ui, warm amber theme |
| Real-time Log Streaming | BroadcastLayer tracing → WebSocket push |
| Memory → Key Insights Tab | `key_facts` cards with access_count badge + timestamp + collapsible source metadata |
| Memory → Evolution Tab | SOUL.md version history with pre/post metric deltas + status badges |
| Logs → History Tab Rewrite | Source filter chips + per-source counts + severity dropdown + severity-colored left borders + JSON detail expansion |
| Toast Notifications | Module-scoped event bus, max-5 queue, warm stone/amber/emerald/rose variants, respects `prefers-reduced-motion` |
| OrgChart | D3.js interactive agent hierarchy visualization |
| Session Replay | Conversation playback with timeline |
| WikiGraph | Interactive knowledge graph |
| Internationalization | zh-TW / en / ja-JP (600+ translation keys) |
| Dark/Light Theme | System preference + manual toggle |
| Experiment Logger | Trajectory recording for RL/RLHF offline analysis |
| Marketplace RPC | `marketplace.list` serves real MCP catalog (Playwright / Browserbase / Filesystem / GitHub / Slack / Postgres / SQLite / Memory / Fetch / Brave Search) |
| Partner Portal | SQLite `PartnerStore` + profile/stats/customers CRUD + 7 RPCs |

## Commercial

| Feature | Description |
|---------|-------------|
| License Tiers | Free / Pro / Enterprise |
| Hardware Fingerprint | License binding |
| Industry Templates | Manufacturing / Restaurant / Trading |
| CLI Tools | 12+ subcommands |
| Partner Portal | Multi-tenant reseller interface |
