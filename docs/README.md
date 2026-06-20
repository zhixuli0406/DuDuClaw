# DuDuClaw Documentation

> Public documentation for the DuDuClaw Multi-Runtime AI Agent Platform (v1.9.4).

---

## Feature Highlights

Detailed introductions to DuDuClaw's standout features, with metaphors and flow diagrams for developers.

| Document | Description |
|----------|-------------|
| [features/README.md](features/README.md) | Feature index + full inventory |
| [features/01-prediction-driven-evolution.md](features/01-prediction-driven-evolution.md) | Prediction-driven evolution — 90% zero-cost conversations |
| [features/02-gvu-self-play-loop.md](features/02-gvu-self-play-loop.md) | GVU self-play loop — agent self-improvement pipeline |
| [features/03-confidence-router.md](features/03-confidence-router.md) | Confidence router & local inference — smart model selection |
| [features/04-file-based-ipc.md](features/04-file-based-ipc.md) | File-based IPC — zero-dependency agent communication |
| [features/05-security-defense.md](features/05-security-defense.md) | Three-phase security defense — layered threat filtering |
| [features/06-soul-versioning.md](features/06-soul-versioning.md) | SOUL.md versioning — atomic updates with auto-rollback |
| [features/07-account-rotation.md](features/07-account-rotation.md) | Multi-account rotation — intelligent credential scheduling |
| [features/08-browser-automation.md](features/08-browser-automation.md) | 5-layer browser automation — progressive escalation |
| [features/09-behavioral-contracts.md](features/09-behavioral-contracts.md) | Behavioral contracts — machine-enforceable agent boundaries |
| [features/10-cognitive-memory.md](features/10-cognitive-memory.md) | Cognitive memory — human-inspired memory with forgetting |
| [features/11-token-compression.md](features/11-token-compression.md) | Token compression triad — lossless, lossy, and streaming |
| [features/12-industry-templates.md](features/12-industry-templates.md) | Industry templates & Odoo ERP bridge |
| [features/13-multi-runtime.md](features/13-multi-runtime.md) | Multi-runtime agent execution — Claude / Codex / Gemini / OpenAI |
| [features/14-voice-pipeline.md](features/14-voice-pipeline.md) | Voice pipeline — ASR / TTS / VAD / LiveKit |
| [features/15-skill-lifecycle.md](features/15-skill-lifecycle.md) | Skill lifecycle engine — 7-stage automated extraction |
| [features/16-session-memory-stack.md](features/16-session-memory-stack.md) | Session memory stack — pinned instructions + snowball recap + key facts |
| [features/17-wiki-knowledge-layer.md](features/17-wiki-knowledge-layer.md) | Wiki knowledge layer — L0-L3 trust-weighted auto-injection |
| [features/18-worktree-isolation.md](features/18-worktree-isolation.md) | Git worktree L0 isolation — lightweight per-task sandbox |
| [features/19-agent-client-protocol.md](features/19-agent-client-protocol.md) | ACP/A2A protocol server — Zed / JetBrains / Neovim integration |

---

## Format Specifications

Open standards that define the DuDuClaw agent ecosystem.

| Document | Description | Status |
|----------|-------------|--------|
| [spec/soul-md-spec.md](spec/soul-md-spec.md) | SOUL.md agent identity format v1.0 | Draft |
| [spec/contract-toml-spec.md](spec/contract-toml-spec.md) | CONTRACT.toml behavioral boundary format v1.0 | Draft |
| [spec/contract-toml-schema.json](spec/contract-toml-schema.json) | CONTRACT.toml JSON Schema | Draft |

## Architecture & Technical Reference

| Document | Description | Status |
|----------|-------------|--------|
| [architecture/overview.md](architecture/overview.md) | System architecture overview | Current |
| [architecture/evolution-engine.md](architecture/evolution-engine.md) | Evolution Engine v2 — Prediction + GVU² + Cognitive Memory | Current |

## Design Proposals (RFC / ADR)

| Document | Description |
|----------|-------------|
| [rfc/RFC-21-identity-credential-isolation.md](rfc/RFC-21-identity-credential-isolation.md) | Identity resolution & per-agent credential isolation |
| [rfc/RFC-21-operator-guide.md](rfc/RFC-21-operator-guide.md) | RFC-21 operator migration playbook |
| [rfc/RFC-22-multi-agent-coordination-principles.md](rfc/RFC-22-multi-agent-coordination-principles.md) | Multi-agent coordination principles |
| [rfc/RFC-26-deep-agents-alignment.md](rfc/RFC-26-deep-agents-alignment.md) | Deep-agents / live-forking alignment |
| [adr/ADR-002-x-duduclaw-capability-negotiation.md](adr/ADR-002-x-duduclaw-capability-negotiation.md) | ACP capability negotiation decision |

## Planning (TODO)

| Document | Description |
|----------|-------------|
| [todo/TODO-agent-honesty.md](todo/TODO-agent-honesty.md) | Agent honesty / anti-hallucination tasks |
| [todo/TODO-rfc26-live-forking.md](todo/TODO-rfc26-live-forking.md) | RFC-26 live-forking implementation tracking |

## User & Developer Guides

| Document | Description | Status |
|----------|-------------|--------|
| [guides/deployment-guide.md](guides/deployment-guide.md) | Production deployment (Tailscale/ngrok/Docker/systemd) | Current |
| [guides/development-guide.md](guides/development-guide.md) | Developer setup, agent development, browser automation | Current |
| [guides/custom-mcp-tool.md](guides/custom-mcp-tool.md) | Extending MCP tools — step-by-step guide | Current |
| [guides/docker.md](guides/docker.md) | Docker build & run | Current |

## API Reference

| Document | Description | Status |
|----------|-------------|--------|
| [api/README.md](api/README.md) | WebSocket RPC protocol, JSON-RPC 2.0 interface | Current |
| [api/openapi.yaml](api/openapi.yaml) | OpenAPI specification | Current |

---

## Directory Structure

```
docs/                                  # L1 PUBLIC — product & developer documentation
├── README.md                          # This index
├── architecture/                      # System architecture & engine design
│   ├── overview.md                    #   Architecture overview
│   └── evolution-engine.md            #   Evolution Engine v2 spec
├── rfc/                               # Request-for-Comments design proposals
│   ├── RFC-21-identity-credential-isolation.md
│   ├── RFC-21-operator-guide.md
│   ├── RFC-22-multi-agent-coordination-principles.md
│   └── RFC-26-deep-agents-alignment.md
├── adr/                               # Architecture Decision Records
│   └── ADR-002-x-duduclaw-capability-negotiation.md
├── todo/                              # Public planning / tracking docs
│   ├── TODO-agent-honesty.md
│   └── TODO-rfc26-live-forking.md
├── features/                          # Feature highlight articles (+ ja-JP, zh-TW)
│   ├── README.md
│   ├── feature-inventory.md
│   └── 01-…-19-…                      #   19 feature deep-dives
├── spec/                              # Open format specifications
│   ├── soul-md-spec.md                #   SOUL.md format v1.0
│   ├── contract-toml-spec.md          #   CONTRACT.toml format v1.0
│   └── contract-toml-schema.json
├── guides/                            # User & developer guides
│   ├── deployment-guide.md
│   ├── development-guide.md
│   ├── custom-mcp-tool.md
│   └── docker.md
└── api/
    ├── README.md                      # WebSocket RPC protocol
    └── openapi.yaml                   # OpenAPI spec
```

> **Confidentiality tiers** — `docs/` is **Public**. Internal operational reports (daily/sprint/eval) live under `wiki/` and `reports`-style trees; commercial plans, competitive analysis, and research notes are **Confidential** and kept in the gitignored `commercial/` and `research/` trees. See the project root `CLAUDE.md` → "Documentation Classification & Placement" for the full rule.
