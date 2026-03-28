# DuDuClaw Project Guidelines

## Architecture Overview (v0.7.0)

DuDuClaw is a **Claude Code extension layer** — not a standalone AI platform. The AI brain is Claude Code SDK (`claude` CLI); DuDuClaw provides the plumbing: channel routing, session management, memory, evolution, multi-account rotation, and **local LLM inference**.

Key architectural decisions:
- **MCP Server** (`duduclaw mcp-server`) exposes channel, memory, agent, and skill tools to Claude Code via JSON-RPC 2.0 over stdin/stdout
- **Agent directories** are Claude Code compatible: each contains `.claude/`, `SOUL.md`, `CLAUDE.md`, `.mcp.json`
- **Sub-agent orchestration** via `create_agent` / `spawn_agent` / `list_agents` MCP tools with `reports_to` hierarchy
- **Session Manager** persists conversations in SQLite with 50k token auto-compression (CJK-aware token estimation)
- **File-based IPC** (`bus_queue.jsonl`) for inter-agent delegation; **AgentDispatcher** consumes and spawns Claude CLI subprocesses
- **Container sandbox** (Docker / Apple Container) for agent task isolation with `--network=none`, tmpfs, read-only rootfs
- **Python subprocess** bridge for Claude Code SDK chat and evolution engine
- **Three channels**: Telegram (long polling), LINE (webhook), Discord (Gateway WebSocket with tokio::select! heartbeat)
- **BroadcastLayer** tracing layer streams real-time logs to WebSocket subscribers
- **Ed25519 challenge-response** auth for secure WebSocket connections
- **Unified heartbeat scheduler** — per-agent cron/interval for bus polling + silence breaker, `max_concurrent_runs` semaphore
- **CronScheduler** reads `cron_tasks.jsonl`, evaluates cron expressions, fires tasks on schedule
- **Prediction-driven evolution engine**: Prediction-error-driven evolution (Active Inference / Dual Process Theory) — zero LLM cost for ~90% of conversations. Dual Process Router: Negligible/Moderate errors → zero cost, Significant → GVU reflection, Critical → emergency GVU loop. MetaCognition self-calibrates error thresholds every 100 predictions.
- **GVU self-play loop** (Generator→Verifier→Updater): TextGrad feedback, max 3 rounds, 4-layer verification (L1-L2-L4 deterministic zero-cost + L3 LLM judge). SOUL.md versioning with 24h observation period + auto-rollback. Atomic write (temp + rename) with SHA-256 fingerprint.
- **Cognitive memory** (optional): episodic/semantic separation with Generative Agents 3D-weighted retrieval
- **Security layer**: SOUL.md drift detection (SHA-256), prompt injection scanner (6 rule categories), JSONL audit log, per-agent key isolation
- **Claude Code security hooks** (`.claude/hooks/`): 3-phase progressive defense — Layer 1 deterministic blacklist, Layer 2 obfuscation/exfiltration detection (YELLOW+), Layer 3 Haiku AI judgment (RED only). Threat level state machine (GREEN→YELLOW→RED) with auto-escalation/degradation. Protects Write/Edit/Read of sensitive files, scans for secret leaks, audits all tool calls (async JSONL compatible with Rust `audit.rs`), validates `.env.claude`, detects config tampering. All prompts use XML delimiters for injection resistance. See `docs/TODO-security-hooks.md` and `docs/code-review-security-hooks.md`.
- **Behavioral contracts** (`CONTRACT.toml`) with `must_not` / `must_always` boundaries + `duduclaw test` red-team CLI
- **Skill ecosystem**: GitHub Search API live indexing of real skill repos, 24h local cache, weighted search, MCP `skill_search` / `skill_list` tools
- **Odoo ERP bridge** (`duduclaw-odoo` crate): JSON-RPC middleware supporting CE/EE, 15 MCP tools (CRM/Sales/Inventory/Accounting), EditionGate auto-detection, event polling + webhook
- **Per-agent model routing**: Each agent independently configures its model via `agent.toml [model]` — `preferred` (Claude SDK model), `local.model` (local GGUF), `local.prefer_local` (try local first), `local.use_router` (confidence router). Routing: if `prefer_local` → try local inference → fallback to Claude SDK. Local inference and account rotation are completely separate paths.
- **Dual-mode account rotation** (Claude SDK only): OAuth sessions (Claude Pro/Team/Max via `~/.claude/.credentials.json`) + API keys, with 4 strategies (Priority/LeastCost/Failover/RoundRobin), health tracking, rate-limit cooldown, budget enforcement. `LeastCost` prefers OAuth → API. Local LLM is NOT part of rotation — it's a separate per-agent routing decision.
- **Local inference engine** (`duduclaw-inference` crate): Unified `InferenceBackend` trait with pluggable backends — llama.cpp (Metal/CUDA/Vulkan/CPU via `llama-cpp-2`), mistral.rs (Rust-native via `mistralrs-core` with ISQ on-the-fly quantization, PagedAttention, Speculative Decoding), OpenAI-compatible HTTP (Exo/llamafile/vLLM/SGLang). Hardware auto-detection, GGUF model management (`~/.duduclaw/models/`), configured via `inference.toml`. MCP tools: `model_list`, `model_load`, `model_unload`, `inference_status`, `hardware_info`, `route_query`.
- **Confidence Router**: Three-tier query routing (LocalFast → LocalStrong → CloudAPI) based on heuristic confidence scoring — token count, keyword complexity detection, CJK-aware token estimation. Configurable thresholds and keyword lists in `inference.toml [router]`. Router escalation: when confidence is low, automatically falls back to Claude API through the AccountRotator.
- **InferenceManager**: Multi-mode auto-switching state machine with priority: Exo P2P cluster → llamafile → Direct backend → OpenAI-compat → Cloud API. Periodic health checks with automatic failover between modes.
- **Exo P2P cluster** client (`exo_cluster.rs`): HTTP client for Exo distributed inference, cluster discovery, health monitoring, automatic endpoint failover. Enables 235B+ models across multiple machines.
- **llamafile manager** (`llamafile.rs`): Subprocess lifecycle management for Mozilla's single-binary LLM inference — auto-start/stop, health monitoring, ready-wait polling, OpenAI-compatible API on localhost. Zero-install portable inference across 6 OS.
- **MLX bridge** (`mlx_bridge.rs`): Python subprocess calling `mlx_lm` on Apple Silicon for local GVU reflections, LoRA adapter support for agent personality. Saves API tokens by running reflections locally.
- **Token/prompt compression** (`compression/`): Three strategies — (1) **Meta-Token (LTSC)**: Rust-native lossless BPE-like compression replacing repeated subsequences with meta-tokens, 27-47% reduction on structured input (JSON, code, templates); (2) **LLMLingua-2**: Python subprocess bridge to Microsoft's token-importance pruning, 2-5x lossy compression for session history; (3) **StreamingLLM**: attention sink + sliding window KV-cache management for infinite-length conversation.
- **MCP tools (inference)**: `model_list`, `model_load`, `model_unload`, `inference_status`, `hardware_info`, `route_query`, `inference_mode`, `llamafile_start`, `llamafile_stop`, `llamafile_list`, `compress_text`, `decompress_text`.
- **Evolution external factors**: User feedback, security events, channel metrics, Odoo business context, peer agent signals feed into prediction engine and GVU reflections
- **API key encryption**: AES-256-GCM stored as base64 in config (all tokens including channel tokens)

## Design Context

### Users
DuDuClaw is a **Claude Code extension layer** for individual developers and power users, primarily in Taiwan (zh-TW). Users interact through a web dashboard to manage AI agents, monitor channels (LINE/Telegram/Discord), track API budgets, and observe agent self-evolution. They expect a tool that feels like a trusted companion — not a cold enterprise console.

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
