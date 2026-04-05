# DuDuClaw Architecture Overview

## Architecture Overview (v1.2.0)

DuDuClaw is a **Claude Code extension layer** — not a standalone AI platform. The AI brain is Claude Code SDK (`claude` CLI); DuDuClaw provides the plumbing: channel routing, session management, memory, evolution, multi-account rotation, and **local LLM inference**.

Key architectural decisions:
- **MCP Server** (`duduclaw mcp-server`) exposes channel, memory, agent, and skill tools to Claude Code via JSON-RPC 2.0 over stdin/stdout
- **Agent directories** are Claude Code compatible: each contains `.claude/`, `SOUL.md`, `CLAUDE.md`, `.mcp.json`
- **Sub-agent orchestration** via `create_agent` / `spawn_agent` / `list_agents` MCP tools with `reports_to` hierarchy
- **Session Manager** persists conversations in SQLite with 50k token auto-compression (CJK-aware token estimation)
- **File-based IPC** (`bus_queue.jsonl`) for inter-agent delegation; **AgentDispatcher** consumes and spawns Claude CLI subprocesses
- **Container sandbox** (Docker / Apple Container) for agent task isolation with `--network=none`, tmpfs, read-only rootfs
- **Python subprocess** bridge for skill vetting
- **Six channels**: Telegram (long polling), LINE (webhook), Discord (Gateway WebSocket with tokio::select! heartbeat), Slack (Socket Mode), WhatsApp (Cloud API), Feishu (Open Platform v2)
- **Multi-runtime agent execution**: `AgentRuntime` trait with Claude/Codex/Gemini/OpenAI-compat backends, `RuntimeRegistry` auto-detection, per-agent `[model] runtime` config
- **WebChat**: Embedded chat via `/ws/chat` WebSocket endpoint, React frontend with Zustand store
- **Channel expansion**: Slack (Socket Mode), WhatsApp (Cloud API), Feishu (Open Platform v2) in addition to Telegram/LINE/Discord
- **MiniMax integration**: OpenAI-compatible chat API + T2A voice synthesis
- **Prometheus metrics**: `GET /metrics` endpoint with requests, tokens, duration histogram, channel status
- **Cross-provider failover**: `FailoverManager` with health tracking, cooldown, non-retryable error detection
- **Generic webhook**: `POST /webhook/{agent_id}` with HMAC-SHA256 signature verification
- **Media pipeline**: Image resize (max 1568px) + MIME detection + Claude Vision integration
- **Whisper transcription**: OpenAI Whisper API for Telegram voice messages
- **BroadcastLayer** tracing layer streams real-time logs to WebSocket subscribers
- **Ed25519 challenge-response** auth for secure WebSocket connections
- **Unified heartbeat scheduler** — per-agent cron/interval for bus polling + GVU silence breaker, `max_concurrent_runs` semaphore
- **CronScheduler** reads `cron_tasks.jsonl`, evaluates cron expressions, fires tasks on schedule
- **Prediction-driven evolution engine**: Prediction-error-driven evolution (Active Inference / Dual Process Theory) — zero LLM cost for ~90% of conversations. Dual Process Router: Negligible/Moderate errors -> zero cost, Significant -> GVU reflection, Critical -> emergency GVU loop. MetaCognition self-calibrates error thresholds every 100 predictions.
- **GVU self-play loop** (Generator->Verifier->Updater): TextGrad feedback, max 3 rounds, 4-layer verification (L1-L2-L4 deterministic zero-cost + L3 LLM judge). SOUL.md versioning with 24h observation period + auto-rollback. Atomic write (temp + rename) with SHA-256 fingerprint.
- **Cognitive memory** (optional): episodic/semantic separation with Generative Agents 3D-weighted retrieval
- **Security layer**: SOUL.md drift detection (SHA-256), prompt injection scanner (6 rule categories), JSONL audit log, per-agent key isolation
- **Claude Code security hooks** (`.claude/hooks/`): 3-phase progressive defense
- **Browser automation & computer use** (5-layer auto-routing): L1 API Fetch -> L2 Static Scrape -> L3 Headless Browser -> L4 Sandbox Browser -> L5 Computer Use
- **Behavioral contracts** (`CONTRACT.toml`) with `must_not` / `must_always` boundaries + `duduclaw test` red-team CLI
- **Skill ecosystem**: GitHub Search API live indexing of real skill repos, 24h local cache, weighted search, MCP `skill_search` / `skill_list` tools
- **Odoo ERP bridge** (`duduclaw-odoo` crate): JSON-RPC middleware supporting CE/EE, 15 MCP tools (CRM/Sales/Inventory/Accounting)
- **Per-agent model routing** (SDK-first design): `agent.toml [model]` routing with hybrid cloud+local
- **Multi-OAuth account rotation**: OAuth sessions + API keys, with 4 strategies (Priority/LeastCost/Failover/RoundRobin)
- **CostTelemetry**: SQLite-backed token usage tracking with cache efficiency analytics
- **Direct API client** (`direct_api.rs`): Bypasses Claude CLI for pure chat, 95%+ cache hit rate
- **Channel hot-start/stop**: Dashboard `channels.add` immediately launches the channel bot
- **Local inference engine** (`duduclaw-inference` crate): Unified `InferenceBackend` trait with pluggable backends
- **Confidence Router**: Three-tier query routing (LocalFast -> LocalStrong -> CloudAPI)
- **Token/prompt compression** (`compression/`): Three strategies (Meta-Token, LLMLingua-2, StreamingLLM)
- **API key encryption**: AES-256-GCM stored as base64 in config (all tokens including channel tokens)
