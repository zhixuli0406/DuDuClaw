# DuDuClaw Documentation Index

> Version: v0.12.0 | Last updated: 2026-03-31 | Total: 34 files

---

## Format Specifications

Open standards that define the DuDuClaw agent ecosystem.

| Document | Description | Status |
|----------|-------------|--------|
| [spec/soul-md-spec.md](spec/soul-md-spec.md) | SOUL.md agent identity format v1.0 — sections, constraints, evolution integration | Draft |
| [spec/contract-toml-spec.md](spec/contract-toml-spec.md) | CONTRACT.toml behavioral boundary format v1.0 — schema, validation, red-team | Draft |

## Architecture & Technical Specs

Core system design and technical reference documents.

| Document | Description | Status |
|----------|-------------|--------|
| [CLAUDE.md](CLAUDE.md) | Architecture overview — 12 crates, 8 channels, 5 runtimes, 52+ MCP tools | Current (v0.10.0) |
| [evolution-engine.md](evolution-engine.md) | Evolution Engine v2 — Prediction + GVU + Cognitive Memory (197 tests) | Current |
| [odoo-integration-plan.md](odoo-integration-plan.md) | Odoo ERP bridge — JSON-RPC, 15 MCP tools, CE/EE edition gate | Implemented |
| [feasibility-kubernetes.md](feasibility-kubernetes.md) | K8s deployment feasibility study — architecture impact, effort, ROI | Research |

## User Guides

How to install, deploy, and operate DuDuClaw.

| Document | Description | Status |
|----------|-------------|--------|
| [deployment-guide.md](deployment-guide.md) | Production deployment — Tailscale/ngrok/Cloudflare/Docker/systemd | Current |
| [development-guide.md](development-guide.md) | Developer setup — local dev, browser automation, agent development | Current |
| [account-rotation-guide.md](account-rotation-guide.md) | OAuth/API key multi-account rotation configuration | Current (v0.12.0+) |

## Business & Commercialization

Revenue strategy, pricing, go-to-market, and intellectual property protection.

| Document | Description | Status |
|----------|-------------|--------|
| [business-plan.md](business-plan.md) | Commercial plan v2.0 — pricing tiers, 7-layer defense, evolution open-source strategy | Current |
| [TODO-commercialization.md](TODO-commercialization.md) | Commercialization work items — licensing, marketing, anti-copy defense (50+ items) | Active |
| [content-policy.md](content-policy.md) | Content tiering — public / semi-public / paid boundaries | Current |
| [security-patch-sop.md](security-patch-sop.md) | Security patch release SOP — severity matrix, timing by edition | Current |
| [implementation-methodology/](implementation-methodology/) | 5-phase SI delivery framework (discovery → PoC → build → pilot → handover) | Current |
| [implementation-methodology/templates/quotation-template.md](implementation-methodology/templates/quotation-template.md) | Client quotation template (licensing + services + support) | Current |

## Competitive Analysis

Market positioning, feature comparison, and ecosystem research.

| Document | Description | Status |
|----------|-------------|--------|
| [duduclaw-vs-openclaw.md](duduclaw-vs-openclaw.md) | Feature-by-feature comparison with OpenClaw | Current (v0.12.0) |
| [gap-analysis-vs-openclaw.md](gap-analysis-vs-openclaw.md) | Feature gap tracker — Phase 1-3 complete, P3 gaps remaining | Current |
| [claw-ecosystem-report.md](claw-ecosystem-report.md) | Top-10 Claude Code extension ecosystem survey | Reference |

## Active Roadmap & TODOs

In-progress or planned work items.

| Document | Scope | Progress |
|----------|-------|----------|
| [TODO-roadmap-v0.10-v0.12.md](TODO-roadmap-v0.10-v0.12.md) | Master roadmap v0.10.0 ~ v0.12.0 | **97%** (285/295) |
| [TODO-commercialization.md](TODO-commercialization.md) | Commercialization: licensing, marketing, defense | Active |
| [TODO-browser-automation.md](TODO-browser-automation.md) | 5-layer browser router + computer use | Active |
| [TODO-evolution-engine-v2.md](TODO-evolution-engine-v2.md) | Evolution Engine v2 detailed tasks | Active |
| [TODO-local-inference.md](TODO-local-inference.md) | llama.cpp integration (4-phase plan) | In-progress |
| [TODO-model-registry.md](TODO-model-registry.md) | Curated model registry + HuggingFace search | In-progress |
| [TODO-token-cost-defense.md](TODO-token-cost-defense.md) | Token cost defense against cache breakage | Active |
| [TODO-cli-streaming-keepalive.md](TODO-cli-streaming-keepalive.md) | CLI idle timeout fix with keepalive | Near-complete |

## Archive

Completed work items kept for historical reference.

### Completed TODOs

| Document | Scope | Completed |
|----------|-------|-----------|
| [archive/TODO-memory-collaboration.md](archive/TODO-memory-collaboration.md) | Long-term memory + cross-agent collaboration | 2026-03-30 |
| [archive/TODO-security-hooks.md](archive/TODO-security-hooks.md) | 3-phase Claude Code security hooks | 2026-03-27 |
| [archive/TODO-skill-lifecycle.md](archive/TODO-skill-lifecycle.md) | Skill injection + distillation pipeline (119 tests) | 2026-03-27 |
| [archive/TODO-dashboard-settings.md](archive/TODO-dashboard-settings.md) | Dashboard settings edit functionality | 2026-03-30 |

### Code Reviews

| Document | Scope | Verdict |
|----------|-------|---------|
| [archive/reviews/CODE-REVIEW-R1.md](archive/reviews/CODE-REVIEW-R1.md) | v0.10-v0.12 features (4,900 lines / 46 files) | ALL FIXED |
| [archive/reviews/REVIEW-memory-collaboration.md](archive/reviews/REVIEW-memory-collaboration.md) | Memory + cross-agent collab (6 rounds) | APPROVED |
| [archive/reviews/code-review-fixes-evolution-v2.md](archive/reviews/code-review-fixes-evolution-v2.md) | Evolution Engine v2 (2 rounds, 88 tests) | ALL FIXED |
| [archive/reviews/code-review-local-inference.md](archive/reviews/code-review-local-inference.md) | `duduclaw-inference` crate (4-agent review) | ALL FIXED |
| [archive/reviews/code-review-model-registry.md](archive/reviews/code-review-model-registry.md) | Model registry + routing (3-agent review) | ALL FIXED |
| [archive/reviews/code-review-security-hooks.md](archive/reviews/code-review-security-hooks.md) | Security hooks (4-agent review) | PASS |
| [archive/reviews/code-review-v0.6.0.md](archive/reviews/code-review-v0.6.0.md) | v0.6.0 full-stack review (64 findings) | Historical |

---

## Directory Structure

```
docs/
├── README.md                          # This index
├── spec/                              # Open format specifications
│   ├── soul-md-spec.md                #   SOUL.md format v1.0
│   └── contract-toml-spec.md          #   CONTRACT.toml format v1.0
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
