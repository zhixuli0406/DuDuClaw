# DuDuClaw Complete Feature Inventory

> v1.4.27 | Last updated: 2026-04-15

---

## Core Architecture

| Feature | Description |
|---------|-------------|
| Multi-Runtime AI Agent Platform | Unified `AgentRuntime` trait — Claude / Codex / Gemini / OpenAI-compat four backends with auto-detection |
| MCP Server (JSON-RPC 2.0) | Exposes 70+ tools to AI Runtime via stdin/stdout |
| Agent Directory Structure | Each agent contains `.claude/`, `SOUL.md`, `CLAUDE.md`, `.mcp.json`, `agent.toml` |
| Sub-agent Orchestration | `create_agent` / `spawn_agent` / `list_agents` with `reports_to` hierarchy + D3.js OrgChart |
| DelegationEnvelope | Structured handoff protocol — context / constraints / task_chain / expected_output |
| TaskSpec Workflow | Multi-step task planning — dependency-aware scheduling, auto-retry (3x), replan (2x), persistence |
| Session Manager | SQLite persistence, 50k token auto-compression (CJK-aware token estimation) |
| File-based IPC | `bus_queue.jsonl` for inter-agent delegation, max 5 hop tracking |

## Multi-Runtime

| Feature | Description |
|---------|-------------|
| Claude Runtime | Claude Code SDK (`claude` CLI) with JSONL streaming |
| Codex Runtime | OpenAI Codex CLI with `--json` streaming events |
| Gemini Runtime | Google Gemini CLI with `--output-format stream-json` |
| OpenAI-compat Runtime | HTTP endpoint (MiniMax / DeepSeek / etc.) via REST API |
| RuntimeRegistry | Auto-detection of installed CLIs, per-agent `[runtime]` config |
| Cross-Provider Failover | `FailoverManager` health tracking, cooldown, non-retryable error detection |

## Communication Channels (7)

| Channel | Protocol |
|---------|----------|
| Telegram | Long polling, file/photo/sticker/voice, forums/topics, mention-only, voice transcription |
| LINE | Webhook, HMAC-SHA256 signature, sticker support, per-chat settings |
| Discord | Gateway WebSocket, slash commands (`/ask /status /config /session /agent`), voice channels (Songbird), auto-thread, embed replies |
| Slack | Socket Mode, mention-only, thread replies |
| WhatsApp | Cloud API |
| Feishu | Open Platform v2 |
| WebChat | Embedded `/ws/chat` WebSocket + React frontend (Zustand store) |
| Channel Hot-Start/Stop | Dashboard-driven dynamic launch/termination |
| Generic Webhook | `POST /webhook/{agent_id}` + HMAC-SHA256 signature verification |
| Media Pipeline | Auto-resize (max 1568px) + MIME detection + Vision integration |
| Sticker System | LINE sticker catalog + emotion detection + Discord emoji equivalents |
| Channel Failure Tracking | `channel_failures.jsonl` with categorized reasons (RateLimited/Billing/Timeout/BinaryMissing/SpawnError) |

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

## Skill Ecosystem

| Feature | Description |
|---------|-------------|
| 7-Stage Lifecycle | Activation → Compression → Extraction → Reconstruction → Distillation → Diagnostician → Gap Analysis |
| GitHub Live Indexing | Search API with 24h local cache, weighted search |
| Skill Marketplace | Web dashboard browsing, installation, and security scanning |

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
| MCP Tools | `compress_text` / `decompress_text` for Meta-Token |

## Voice Pipeline

| Feature | Description |
|---------|-------------|
| ASR (Speech-to-Text) | Whisper.cpp (local) / SenseVoice ONNX (local) / OpenAI Whisper API / Deepgram |
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
| CONTRACT.toml | Behavioral boundaries + `duduclaw test` red-team CLI (9 built-in scenarios) |
| RBAC | Role-based access control matrix |
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
| Memory Decay | Spaced-repetition forgetting curves |
| Federated Memory | Cross-agent knowledge sharing (Private / Team / Public levels) |
| Wiki Knowledge Base | Full-text search + knowledge graph visualization |

## Account & Cost Management

| Feature | Description |
|---------|-------------|
| Multi-Account Rotation | OAuth + API Key, 4 strategies (Priority/LeastCost/RoundRobin/Failover) |
| CostTelemetry | SQLite token tracking + cache efficiency analytics + 200K price cliff warning |
| Budget Manager | Per-account monthly limits + cooldown + adaptive routing (cache_eff <30% → local) |
| Direct API | Bypass CLI, `cache_control: ephemeral`, 95%+ cache hit rate |
| Channel Failure Tracking | `channel_failures.jsonl` with category-specific zh-TW messages |
| Binary Discovery | `which_claude()` probes Homebrew/Bun/Volta/npm-global/asdf/NVM directories |

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
| CronScheduler | `cron_tasks.jsonl` cron expression evaluation + MCP tools (CRUD + pause) |
| ReminderScheduler | One-shot reminders (relative `5m`/`2h`/`1d` or ISO 8601), `direct` or `agent_callback` mode |
| HeartbeatScheduler | Per-agent unified scheduling — bus polling + GVU silence breaker + cron |

## ERP Integration

| Feature | Description |
|---------|-------------|
| Odoo Bridge | 15 MCP tools (CRM/Sales/Inventory/Accounting), JSON-RPC middleware |
| Edition Gate | CE/EE auto-detection, feature gating |
| Event Polling | Proactive agent notifications on Odoo state changes |

## Web Dashboard

| Feature | Description |
|---------|-------------|
| 23 Pages | Dashboard / Agents / Channels / Accounts / Memory / Security / Settings / OrgChart / SkillMarket / Logs / WebChat / OnboardWizard / Billing / License / Report / PartnerPortal / Marketplace / KnowledgeHub / Odoo / Login / Users / Analytics / Export |
| Tech Stack | React 19 + TypeScript + Tailwind CSS 4 + shadcn/ui, warm amber theme |
| Real-time Log Streaming | BroadcastLayer tracing → WebSocket push |
| OrgChart | D3.js interactive agent hierarchy visualization |
| Session Replay | Conversation playback with timeline |
| WikiGraph | Interactive knowledge graph |
| Internationalization | zh-TW / en / ja-JP (540+ translation keys) |
| Dark/Light Theme | System preference + manual toggle |
| Prometheus Metrics | `GET /metrics` — requests, tokens, duration histogram, channel status |
| Experiment Logger | Trajectory recording for RL/RLHF offline analysis |

## Commercial

| Feature | Description |
|---------|-------------|
| License Tiers | Free / Pro / Enterprise |
| Hardware Fingerprint | License binding |
| Industry Templates | Manufacturing / Restaurant / Trading |
| CLI Tools | 12+ subcommands |
| Partner Portal | Multi-tenant reseller interface |
