# DuDuClaw Feature Highlights

> DuDuClaw v1.21.1 | Last updated: 2026-06-21

This directory contains detailed introductions to DuDuClaw's standout features. Each article explains the design rationale, system behavior, and operational flow — aimed at developers who want to understand *how things work* without diving into source code.

---

## Feature Index

| # | Article | One-liner |
|---|---------|-----------|
| 1 | [Prediction-Driven Evolution](01-prediction-driven-evolution.md) | 90% of conversations evolve at zero LLM cost |
| 2 | [GVU² Self-Play Loop](02-gvu-self-play-loop.md) | Dual-loop evolution with 4+2 layer verification |
| 3 | [Confidence Router & Local Inference](03-confidence-router.md) | Smart model selection that saves 80%+ on API bills |
| 4 | [File-Based IPC Message Bus](04-file-based-ipc.md) | Structured inter-agent delegation with TaskSpec workflows |
| 5 | [Three-Phase Security Defense](05-security-defense.md) | Layered threat filtering at minimal cost |
| 6 | [SOUL.md Versioning & Rollback](06-soul-versioning.md) | Atomic personality updates with auto-rollback |
| 7 | [Multi-Account Rotation](07-account-rotation.md) | Cross-provider credential scheduling with failover |
| 8 | [5-Layer Browser Automation](08-browser-automation.md) | Progressive resource escalation for web tasks |
| 9 | [Behavioral Contracts & Red-Team Testing](09-behavioral-contracts.md) | Machine-enforceable agent boundaries |
| 10 | [Cognitive Memory System](10-cognitive-memory.md) | Human-inspired memory with forgetting curves |
| 11 | [Token Compression Triad](11-token-compression.md) | Three strategies to fit more into less |
| 12 | [Industry Templates & Odoo ERP Bridge](12-industry-templates.md) | Out-of-the-box business intelligence |
| 13 | [Multi-Runtime Agent Execution](13-multi-runtime.md) | Claude / Codex / Gemini / OpenAI-compat unified backend |
| 14 | [Voice Pipeline](14-voice-pipeline.md) | ASR / TTS / VAD / LiveKit — local-first voice intelligence |
| 15 | [Skill Lifecycle Engine](15-skill-lifecycle.md) | 7-stage automated skill extraction and management |
| 16 | [Session Memory Stack](16-session-memory-stack.md) | Pinned instructions + snowball recap + key-fact accumulator |
| 17 | [Wiki Knowledge Layer](17-wiki-knowledge-layer.md) | L0-L3 trust-weighted knowledge with auto-injection |
| 18 | [Git Worktree L0 Isolation](18-worktree-isolation.md) | Lightweight per-task sandbox with atomic merge |
| 19 | [Agent Client Protocol (ACP/A2A)](19-agent-client-protocol.md) | IDE-grade stdio JSON-RPC for Zed/JetBrains/Neovim |
| 20 | [Memory Intelligence](20-memory-intelligence.md) | Temporal facts + reflexion loop + batch fetch |
| 21 | [Governance Layer](21-governance-layer.md) | Policy registry + per-agent quotas (duduclaw-governance) |
| 22 | [Durability Framework](22-durability-framework.md) | Idempotency / retry / circuit breaker / checkpoint / DLQ |
| 23 | [Autopilot Rule Engine](23-autopilot-engine.md) | Event-driven automation + circuit breaker |
| 24 | [Task Board & Activity Feed](24-task-board.md) | Agent-as-teammate task management |
| 25 | [Identity Resolution](25-identity-resolution.md) | WikiCache / Notion / Chained providers (RFC-21 §1) |
| 26 | [MCP HTTP/SSE Transport](26-mcp-http-sse.md) | Bearer-authed REST + SSE endpoints (W20) |
| 27 | [Cross-Platform PTY Pool + Worker](27-pty-pool-runtime.md) | Drive the interactive `claude` REPL (v1.15.0) |
| 28 | [Live Run Forking](28-live-forking.md) | Parallel branches + AI judge (duduclaw-fork, RFC-26) |
| 29 | [Evolution Events](29-evolution-events.md) | Black-box recorder with batch+retry delivery |

---

## Translations

- [繁體中文版 (zh-TW)](zh-TW/README.md)
- [日本語版 (ja-JP)](ja-JP/README.md)

---

## Full Feature Inventory

For a complete list of all features (not just highlights), see [feature-inventory.md](feature-inventory.md).
