# DuDuClaw Documentation

> Public documentation for the DuDuClaw Multi-Runtime AI Agent Platform (v1.8.14).

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
| [CLAUDE.md](CLAUDE.md) | Architecture overview (v1.8.14) | Current |
| [evolution-engine.md](evolution-engine.md) | Evolution Engine v2 — Prediction + GVU² + Cognitive Memory | Current |

## User & Developer Guides

| Document | Description | Status |
|----------|-------------|--------|
| [deployment-guide.md](deployment-guide.md) | Production deployment (Tailscale/ngrok/Docker/systemd) | Current |
| [development-guide.md](development-guide.md) | Developer setup, agent development, browser automation | Current |
| [guides/custom-mcp-tool.md](guides/custom-mcp-tool.md) | Extending MCP tools — step-by-step guide | Current |

## API Reference

| Document | Description | Status |
|----------|-------------|--------|
| [api/README.md](api/README.md) | WebSocket RPC protocol, JSON-RPC 2.0 interface | Current |
| [api/openapi.yaml](api/openapi.yaml) | OpenAPI specification | Current |

---

## Directory Structure

```
docs/
├── README.md                          # This index
├── features/                          # Feature highlight articles
│   ├── README.md                      #   Feature index
│   ├── feature-inventory.md           #   Complete feature inventory
│   ├── 01-prediction-driven-evolution.md
│   ├── 02-gvu-self-play-loop.md
│   ├── 03-confidence-router.md
│   ├── 04-file-based-ipc.md
│   ├── 05-security-defense.md
│   ├── 06-soul-versioning.md
│   ├── 07-account-rotation.md
│   ├── 08-browser-automation.md
│   ├── 09-behavioral-contracts.md
│   ├── 10-cognitive-memory.md
│   ├── 11-token-compression.md
│   ├── 12-industry-templates.md
│   ├── 13-multi-runtime.md
│   ├── 14-voice-pipeline.md
│   ├── 15-skill-lifecycle.md
│   ├── 16-session-memory-stack.md
│   ├── 17-wiki-knowledge-layer.md
│   ├── 18-worktree-isolation.md
│   └── 19-agent-client-protocol.md
├── spec/                              # Open format specifications
│   ├── soul-md-spec.md                #   SOUL.md format v1.0
│   ├── contract-toml-spec.md          #   CONTRACT.toml format v1.0
│   └── contract-toml-schema.json
├── api/
│   ├── README.md                      # WebSocket RPC protocol
│   └── openapi.yaml                   # OpenAPI spec
├── guides/
│   └── custom-mcp-tool.md             # MCP tool development guide
│
├── # Architecture
├── CLAUDE.md                          # System architecture overview
├── evolution-engine.md                # Evolution Engine v2 spec
├── odoo-integration-plan.md           # Odoo ERP bridge design
├── feasibility-kubernetes.md          # K8s feasibility study
│
├── # Guides
├── deployment-guide.md                # Production deployment
├── development-guide.md               # Developer setup
├── account-rotation-guide.md          # Multi-account rotation
│
├── # Business
├── business-plan.md                   # Commercial plan v2.0
├── TODO-commercialization.md          # Commercialization tasks
├── content-policy.md                  # Content tiering rules
├── security-patch-sop.md              # Patch release SOP
├── implementation-methodology/        # 5-phase SI delivery
│   ├── 01-discovery.md
│   ├── 02-poc.md
│   ├── 03-build.md
│   ├── 04-pilot.md
│   ├── 05-handover.md
│   └── templates/quotation-template.md
│
├── # Competitive
├── duduclaw-vs-openclaw.md            # Feature comparison
├── gap-analysis-vs-openclaw.md        # Gap tracker
├── claw-ecosystem-report.md           # Ecosystem survey
│
├── # Active TODOs
├── TODO-roadmap-v0.10-v0.12.md        # Master roadmap (97%)
├── TODO-browser-automation.md
├── TODO-evolution-engine-v2.md
├── TODO-local-inference.md
├── TODO-model-registry.md
├── TODO-token-cost-defense.md
├── TODO-cli-streaming-keepalive.md
│
└── archive/                           # Completed work
    ├── TODO-memory-collaboration.md
    ├── TODO-security-hooks.md
    ├── TODO-skill-lifecycle.md
    ├── TODO-dashboard-settings.md
    └── reviews/
        ├── CODE-REVIEW-R1.md
        ├── REVIEW-memory-collaboration.md
        ├── code-review-fixes-evolution-v2.md
        ├── code-review-local-inference.md
        ├── code-review-model-registry.md
        ├── code-review-security-hooks.md
        └── code-review-v0.6.0.md
```

> Implementation TODOs, business plans, competitive analysis, and research documents are maintained in the private `commercial/` repository.
