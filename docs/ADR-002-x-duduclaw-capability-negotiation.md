# ADR-002 — x-duduclaw Header Versioning & Capability Negotiation

**Status:** Implemented (2026-05-06)  
**Sprint:** W22-P0  
**Owner:** TL-DuDuClaw  
**Implemented in:** `crates/duduclaw-cli/src/mcp_headers.rs` + `mcp_capability.rs`  
**Last updated:** 2026-05-06

---

## Context

DuDuClaw exposes an HTTP/SSE MCP endpoint (`/mcp/v1/call`, `/mcp/v1/stream`) consumed by external
clients (Claude Desktop, CI pipelines, third-party integrations). As the platform ships new
capabilities (A2A Bridge, Secret Manager, signed agent cards) and introduces breaking changes, clients
need a reliable, machine-readable way to:

1. **Discover** which capabilities are available on the server they are talking to.
2. **Declare** which capabilities they require, so the server can reject incompatible requests early
   (before wasting tokens or performing partial work).
3. **Understand** the server's HTTP API compatibility level, independent of DuDuClaw's SemVer release
   number.

Without a formal header protocol, capability discovery degrades into documentation drift and runtime
surprises that are hard to diagnose across client/server version mismatches.

---

## Decision

Introduce three HTTP response headers and one HTTP request header, collectively called the
**x-duduclaw header protocol**, applied to every request/response on the HTTP server.

### §1 — Header Definitions

| Header | Direction | Description |
|--------|-----------|-------------|
| `x-duduclaw-version` | Response | HTTP API compatibility version (see §4.3) |
| `x-duduclaw-capabilities` | Response | Comma-separated list of enabled server capabilities (see §4.1) |
| `x-duduclaw-capabilities` | Request (optional) | Client-declared capability requirements for this request |
| `x-duduclaw-missing-capabilities` | Response (422 only) | Subset of requested capabilities the server cannot satisfy |

### §2 — Invariant: Headers on Every Response

Both `x-duduclaw-version` and `x-duduclaw-capabilities` MUST appear on **every** HTTP response,
including error responses (4xx, 5xx). This is enforced by the `inject_capability_headers` Axum
middleware, which wraps all routes including the 422 produced by `negotiate_capabilities`.

### §3 — Capability Negotiation Protocol

#### §3.1 — Client-side (request)
Clients MAY include `x-duduclaw-capabilities` in a request to declare which capabilities they require:

```
x-duduclaw-capabilities: memory/3,mcp/2
```

#### §3.2 — Permissive Mode
If the client omits the header (or sends an empty/malformed value), the server treats the request
as having no capability requirements and passes it through. **Absence = permissive, not rejection.**

#### §3.3 — 422 Unprocessable Entity
If the client includes the header and any stated requirement cannot be met:

| Failure mode | Server response |
|---|---|
| Capability absent or `enabled: false` | 422 — `server_version: null` |
| Capability present but major version too low | 422 — `server_version: <current_v>` |

The 422 body:
```json
{
  "error": "capability_mismatch",
  "message": "Required capabilities not available on this server",
  "missing": [
    { "capability": "a2a", "required_version": 1, "server_version": null },
    { "capability": "mcp", "required_version": 5, "server_version": 2 }
  ]
}
```

The 422 response also carries `x-duduclaw-missing-capabilities: a2a/1,mcp/5` for easy machine
parsing without body deserialization.

### §4 — Format Specifications

#### §4.1 — x-duduclaw-capabilities format
```
memory/<major>,<other-cap-alpha>/<major>,...
```

Rules:
1. Only `enabled: true` entries in `CAPABILITY_REGISTRY` appear.
2. `memory` is **always first** — it is the DuDuClaw core differentiator.
3. All other enabled capabilities follow in **lexicographic (ASCII) order**.
4. Each entry is `<name>/<major_version>` with no spaces.
5. Entries are comma-separated with no spaces.

Example: `memory/3,audit/2,governance/1,mcp/2,skill/1,wiki/1`

#### §4.2 — Major version semantics
The `major_version` in the capability registry increments **only on breaking protocol changes** for
that specific capability. Adding new optional fields or new tools within a capability is NOT a major
bump. Clients pinned to `mcp/2` can rely on all `mcp/2.x` behaviour remaining stable.

#### §4.3 — x-duduclaw-version semantics
This version tracks HTTP API compatibility — independent from DuDuClaw's SemVer release (`v1.11.x`
etc.). It changes only when the HTTP API itself has a compatibility change (new required header,
changed status code semantics, etc.).

Current value: `1.2`
- `1`: HTTP API stable (past beta, introduced W20)
- `2`: Second backward-compatible HTTP change (W22 — this ADR, capability negotiation added)

### §5 — Capability Registry

The canonical registry lives in `crates/duduclaw-cli/src/mcp_headers.rs::CAPABILITY_REGISTRY`.

| Capability | Major Version | Status | Sprint |
|---|---|---|---|
| `memory` | 3 | ✅ Enabled | core |
| `audit` | 2 | ✅ Enabled | W20-P1 |
| `governance` | 1 | ✅ Enabled | W19-P1 |
| `mcp` | 2 | ✅ Enabled | W20 HTTP/SSE Phase 2 |
| `skill` | 1 | ✅ Enabled | — |
| `wiki` | 1 | ✅ Enabled | — |
| `a2a` | 1 | 🔒 Disabled | W21 (pending enablement) |
| `secret-manager` | 1 | 🔒 Disabled | W22 P0 (pending) |
| `signed-card` | 1 | 🔒 Disabled | W22 P1 (pending) |

Disabled capabilities are **never emitted** in outbound headers. When a client requests a disabled
capability, the server returns 422 with `server_version: null`.

### §6 — Axum Middleware Layer Order

```
router
    .layer(middleware::from_fn(negotiate_capabilities))    // INNER (checked first on request)
    .layer(middleware::from_fn(inject_capability_headers)) // OUTER (headers added on all responses)
```

Axum evaluates layers in reverse registration order (last `.layer()` = outermost). This ordering
guarantees `inject_capability_headers` runs on **all** responses — including 422s produced by the
inner `negotiate_capabilities` middleware.

### §7 — Disabled Capability Policy

Capabilities with `enabled: false` behave identically to unknown capabilities from the client's
perspective: `server_version: null` in the 422 body. This avoids leaking implementation roadmap
information through the header protocol.

Disabled capabilities are omitted from the outbound `x-duduclaw-capabilities` header, so
opportunistic discovery (client reads the response header and adjusts) also works correctly.

### §8 — SDK and Documentation Sync Requirements

When the capability registry changes (capability added, enabled, or major-bumped):

1. Update `CAPABILITY_REGISTRY` in `mcp_headers.rs`
2. Update the snapshot test `header_snapshot_matches_expected` — this acts as a forced-pause
   before capability changes ship silently
3. Update §5 table in this ADR
4. Update CHANGELOG.md
5. Notify SDK maintainers if a new capability requires client-side feature flags

---

## Consequences

### Positive

- **Zero-cost discovery**: every response already carries capability metadata — no extra round-trip.
- **Explicit contract**: clients that declare requirements get 422 + diagnostic info instead of
  silent partial failures.
- **Additive by default**: clients that don't send the request header are never broken by new
  capabilities shipping.
- **Test-locked registry**: the snapshot test prevents accidental registry changes from silently
  reaching production.

### Negative / Trade-offs

- **Per-request overhead**: `build_capabilities_header()` iterates `CAPABILITY_REGISTRY` (9 entries
  today) on every response. Acceptable at current scale; if registry grows >100 entries, consider
  pre-computing a static `OnceLock<String>`.
- **Major-version-only negotiation**: clients cannot express fine-grained minor/patch requirements.
  This is intentional — minor/patch changes are always backward-compatible by definition.

---

## Implementation

| File | Role |
|------|------|
| `crates/duduclaw-cli/src/mcp_headers.rs` | Registry, header builder, parser, negotiation logic — 23 unit tests |
| `crates/duduclaw-cli/src/mcp_capability.rs` | Axum middleware (`inject_capability_headers`, `negotiate_capabilities`) — 11 integration tests |
| `crates/duduclaw-cli/src/mcp_http_server.rs` | Wires both middleware layers into `build_router()` |
| `crates/duduclaw-cli/src/lib.rs` | Exports `pub mod mcp_headers` + `pub mod mcp_capability` |

**Test coverage:** 34 tests (unit + Axum integration via `tower::ServiceExt::oneshot`), all passing.

---

## Alternatives Considered

### Alt A — Versioned URL paths only (`/mcp/v2/call`)
Rejected: URL versioning handles major API revisions but cannot express fine-grained capability
presence. A client connecting to `/mcp/v2/call` still cannot know whether `a2a` or `secret-manager`
are available on this particular deployment.

### Alt B — Capability discovery endpoint (`GET /mcp/capabilities`)
Rejected: requires an extra round-trip before every session. Header-based discovery is zero-cost
because the information piggybacks on every existing response.

### Alt C — Capabilities in JSON-RPC `initialize` result
Rejected: the HTTP layer sits below the JSON-RPC layer. Some routes (e.g. `/healthz`) never process
a JSON-RPC envelope. Headers are the correct layer for HTTP-level protocol negotiation.

---

*ADR written by TL-DuDuClaw | 2026-05-06*
