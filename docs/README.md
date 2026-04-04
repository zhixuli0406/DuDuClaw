# DuDuClaw Documentation

> Public documentation for the DuDuClaw Claude Code extension layer.

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
| [CLAUDE.md](CLAUDE.md) | Architecture overview | Current |
| [evolution-engine.md](evolution-engine.md) | Evolution Engine v2 — Prediction + GVU + Cognitive Memory | Current |

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
├── README.md                    # This index
├── CLAUDE.md                    # System architecture overview
├── evolution-engine.md          # Evolution Engine v2 spec
├── deployment-guide.md          # Production deployment guide
├── development-guide.md         # Developer setup guide
├── api/
│   ├── README.md                # WebSocket RPC protocol
│   └── openapi.yaml             # OpenAPI spec
├── guides/
│   └── custom-mcp-tool.md       # MCP tool development guide
└── spec/
    ├── soul-md-spec.md          # SOUL.md format v1.0
    ├── contract-toml-spec.md    # CONTRACT.toml format v1.0
    └── contract-toml-schema.json
```

> Implementation TODOs, business plans, competitive analysis, and research documents are maintained in the private `commercial/` repository.
