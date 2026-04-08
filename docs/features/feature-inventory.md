# DuDuClaw Complete Feature Inventory

> v1.1.0 | Last updated: 2026-04-07

---

## Core Architecture

| Feature | Description |
|---------|-------------|
| Claude Code Extension Layer | Not a standalone AI — provides plumbing for channel routing, session management, memory, evolution |
| MCP Server (JSON-RPC 2.0) | Exposes 52+ tools to Claude Code via stdin/stdout |
| Agent Directory Structure | Each agent contains `.claude/`, `SOUL.md`, `CLAUDE.md`, `.mcp.json` |
| Sub-agent Orchestration | `create_agent` / `spawn_agent` / `list_agents` with `reports_to` hierarchy |
| Session Manager | SQLite persistence, 50k token auto-compression (CJK-aware) |
| File-based IPC | `bus_queue.jsonl` for inter-agent delegation |

## Communication Channels (7)

| Channel | Protocol |
|---------|----------|
| Telegram | Long polling, file/photo/sticker support |
| LINE | Webhook, sticker support |
| Discord | Gateway WebSocket, slash commands, voice channels |
| Slack | Socket Mode |
| WhatsApp | Cloud API |
| Feishu | Open Platform v2 |
| WebChat | Embedded `/ws/chat` WebSocket + React frontend |
| Channel Hot-Start/Stop | Dashboard-driven dynamic launch/termination |

## Evolution System

| Feature | Description |
|---------|-------------|
| Prediction-Driven Engine | Active Inference, ~90% zero LLM cost |
| Dual Process Router | System 1 (rules) / System 2 (LLM reflection) |
| GVU Self-Play Loop | Generator-Verifier-Updater, TextGrad feedback, max 3 rounds |
| SOUL.md Versioning | 24h observation period, atomic rollback, SHA-256 fingerprint |
| MetaCognition | Self-calibrating error thresholds every 100 predictions |

## Skill Ecosystem

| Feature | Description |
|---------|-------------|
| 7-Stage Lifecycle | Activation, compression, extraction, reconstruction, distillation, diagnostics, gap analysis |
| GitHub Live Indexing | Search API with 24h local cache |
| Skill Marketplace | Web dashboard browsing and installation |

## Local Inference Engine

| Feature | Description |
|---------|-------------|
| llama.cpp | Metal/CUDA/Vulkan/CPU |
| mistral.rs | Rust-native, ISQ, PagedAttention, Speculative Decoding |
| OpenAI-compatible HTTP | Exo/llamafile/vLLM/SGLang |
| Confidence Router | LocalFast / LocalStrong / CloudAPI three-tier routing |
| InferenceManager | Multi-mode auto-switching state machine with health checks |
| llamafile Manager | Subprocess lifecycle, zero-install portable inference |
| Exo P2P Cluster | Distributed inference, 235B+ models across machines |
| MLX Bridge | Apple Silicon local reflections + LoRA |

## Compression Engine

| Feature | Description |
|---------|-------------|
| Meta-Token (LTSC) | Rust-native lossless BPE-like, 27-47% compression |
| LLMLingua-2 | Microsoft token-importance pruning, 2-5x lossy |
| StreamingLLM | Attention sink + sliding window KV-cache |

## Voice Pipeline

| Feature | Description |
|---------|-------------|
| ASR | Whisper.cpp / SenseVoice ONNX / Deepgram |
| TTS | Piper (local ONNX) / MiniMax (remote) |
| VAD | Silero (ONNX) |
| LiveKit Voice | WebRTC voice rooms |

## Security

| Feature | Description |
|---------|-------------|
| 3-Phase Defense | Deterministic blacklist / obfuscation detection / AI judgment |
| Ed25519 Auth | Challenge-response WebSocket authentication |
| AES-256-GCM | API key encryption at rest |
| Prompt Injection Scanner | 6 rule categories |
| SOUL.md Drift Detection | SHA-256 fingerprint comparison |
| CONTRACT.toml | Behavioral boundaries + `duduclaw test` red-team CLI |
| RBAC | Role-based access control |
| JSONL Audit Log | Full tool call recording |

## Memory System

| Feature | Description |
|---------|-------------|
| Episodic / Semantic Separation | Generative Agents 3D-weighted retrieval |
| Full-Text Search (FTS5) | SQLite built-in |
| Vector Index | Embedding-based semantic search |
| Memory Decay | Spaced-repetition forgetting curves |
| Federated Memory | Cross-agent knowledge sharing |
| Wiki Knowledge Base | Full-text search + knowledge graph visualization |

## Account & Cost Management

| Feature | Description |
|---------|-------------|
| Multi-Account Rotation | OAuth + API Key, 4 strategies |
| CostTelemetry | Token usage tracking + cache efficiency analytics |
| Budget Manager | Per-account monthly limits + cooldown |
| Direct API | Bypass CLI, 95%+ cache hit rate |

## Browser Automation

| Feature | Description |
|---------|-------------|
| 5-Layer Router | API Fetch / Static Scrape / Headless / Sandbox / Computer Use |
| Capability Gating | `agent.toml [capabilities]` deny-by-default |

## Container Sandbox

| Feature | Description |
|---------|-------------|
| Docker | Bollard API, all platforms |
| Apple Container | Native macOS 15+ |
| WSL2 | Windows Linux subsystem |

## Scheduling

| Feature | Description |
|---------|-------------|
| CronScheduler | `cron_tasks.jsonl` cron expression evaluation |
| ReminderScheduler | One-shot reminders (relative/absolute time) |
| HeartbeatScheduler | Per-agent unified scheduling |

## ERP Integration

| Feature | Description |
|---------|-------------|
| Odoo Bridge | 15 MCP tools (CRM/Sales/Inventory/Accounting) |
| Edition Gate | CE/EE auto-detection |

## Web Dashboard

| Feature | Description |
|---------|-------------|
| 23 Pages | Dashboard, agents, channels, memory, security, billing, etc. |
| Real-time Log Streaming | BroadcastLayer tracing to WebSocket |
| WikiGraph | Interactive knowledge graph |
| OrgChart | Agent hierarchy visualization |

## Commercial

| Feature | Description |
|---------|-------------|
| License Tiers | Free / Pro / Enterprise |
| Hardware Fingerprint | License binding |
| Industry Templates | Manufacturing / Restaurant / Trading |
| CLI Tools | 12 subcommands |
