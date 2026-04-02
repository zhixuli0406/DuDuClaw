# DuDuClaw WebSocket RPC API

DuDuClaw exposes a JSON-RPC 2.0 API over WebSocket at `ws://<host>:<port>/ws`.

## Quick Start

```bash
# Start the gateway
duduclaw run

# Connect via WebSocket (default: ws://localhost:18789/ws)
```

## Protocol

All communication uses JSON-RPC 2.0 frames:

```json
// Request
{ "jsonrpc": "2.0", "method": "agents.list", "params": {}, "id": "1" }

// Success response
{ "jsonrpc": "2.0", "result": { "agents": [...] }, "id": "1" }

// Error response
{ "jsonrpc": "2.0", "error": { "code": -1, "message": "..." }, "id": "1" }
```

## Authentication

1. Send `connect.challenge` to receive a challenge UUID.
2. Sign with your Ed25519 private key.
3. Send `connect` with the signed response.

If `auth_token` is not configured, authentication is disabled.

## Method Categories

| Prefix | Description | Count |
|--------|-------------|-------|
| `agents.*` | Agent lifecycle and management | 12 |
| `channels.*` | Telegram/LINE/Discord management | 5 |
| `accounts.*` | OAuth/API account rotation and budgets | 6 |
| `memory.*` | Cognitive memory search and browsing | 2 |
| `skills.*` | Skill ecosystem discovery | 3 |
| `cron.*` | Scheduled task management | 4 |
| `system.*` | Status, config, diagnostics | 8 |
| `license.*` | License activation and status | 3 |
| `analytics.*` | Usage analytics and cost savings | 3 |
| `referral.*` | Referral code system | 3 |
| `security.*` | Audit logs, credentials | 1 |
| `models.*` | Cloud + local model listing | 1 |
| `logs.*` | Real-time log streaming | 2 |
| `heartbeat.*` | Agent heartbeat scheduling | 2 |
| `evolution.*` | Evolution engine status | 2 |
| `browser.*` | Browser automation approval | 2 |
| `billing.*` | Usage and billing history | 2 |
| `marketplace.*` | Skill marketplace | 2 |
| `partner.*` | Partner/reseller management | 4 |

## OpenAPI Spec

The full API specification is documented in [openapi.yaml](./openapi.yaml).

While this is a WebSocket JSON-RPC API (not REST), the spec uses OpenAPI 3.1 format with `/rpc/<method>` pseudo-paths for tooling compatibility and discoverability.

## Referral System

Generate and share referral codes to earn rewards:

| Referrals | Reward |
|-----------|--------|
| 1 | +500 bonus conversations |
| 3 | 1 month Pro tier |
| 10 | Permanent Pro tier |

```bash
# CLI usage
duduclaw refer generate        # Generate your code
duduclaw refer status          # Check progress
duduclaw refer redeem DDCL-XXXX  # Redeem a code
```
