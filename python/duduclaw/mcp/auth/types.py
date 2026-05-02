"""API Key context types for MCP Server auth middleware.

These types define the interface contract between:
  - Producer: eng-infra auth middleware (Strategy Pattern, APIKeyContext factory)
  - Consumer: eng-memory tool handlers (this module)

API Key format: ddc_<env>_<random_32hex>
  e.g. ddc_prod_a3f2c1e4b5d6e7f8a3f2c1e4b5d6e7f8

client_id derivation: SHA256(api_key)[:12]
  - Deterministic: same key always maps to same client_id
  - Non-reversible: cannot reconstruct key from client_id
  - Short: 12 hex chars = 6 bytes = sufficient collision resistance

TL Decision (2026-04-29):
  - Strategy Pattern for auth middleware (JWT/OAuth2 upgrade path)
  - Key rotation: every 30 days enforced
  - Per-client write quota: default 1000 records/day
"""

from __future__ import annotations

import hashlib
from dataclasses import dataclass, field
from typing import Literal


# Scope literals (from spec: mcp-server-spec-draft.md §4.1)
Scope = Literal[
    "memory:read",
    "memory:write",
    "wiki:read",
    "wiki:write",
    "messaging:send",
    "tasks:read",
    "tasks:write",
    "agents:read",
    "evolution:read",
]

TrustLevel = Literal[1, 2, 3]  # 1=external, 2=trusted_external, 3=internal


@dataclass(frozen=True)
class APIKeyContext:
    """Parsed and validated API Key context, produced by the auth middleware.

    This dataclass is the authoritative representation of an authenticated
    MCP client.  It is produced once per request by the auth middleware and
    passed through to all tool handlers.

    Attributes:
        api_key_prefix:  Safe log-friendly prefix (e.g. ``"ddc_prod"``).
                         **Never** contains the full key.
        client_id:       12-char hex derived via SHA256(api_key)[:12].
                         Used as the unique client identifier across all systems.
        scopes:          Frozenset of granted permission scopes.
        trust_level:     1=external, 2=trusted_external, 3=internal.
    """

    api_key_prefix: str  # e.g. "ddc_prod" — safe for logging, NEVER full key
    client_id: str       # SHA256(api_key)[:12], e.g. "a3f2c1e4b5d6"
    scopes: frozenset[str] = field(default_factory=frozenset)
    trust_level: TrustLevel = 1

    @property
    def namespace(self) -> str:
        """Return the forced namespace for this client: ``external/{client_id}``."""
        return f"external/{self.client_id}"

    def has_scope(self, scope: str) -> bool:
        """Return True iff the given scope is granted for this client."""
        return scope in self.scopes

    def has_any_scope(self, *scopes: str) -> bool:
        """Return True iff at least one of the given scopes is granted."""
        return bool(self.scopes.intersection(scopes))


def derive_client_id(api_key: str) -> str:
    """Derive a deterministic, non-reversible 12-char hex client_id from an API key.

    Uses SHA-256 to produce the client_id.  The first 12 hex characters are taken
    (= 6 bytes = 48 bits of entropy, sufficient for collision resistance at scale).

    Args:
        api_key: Full API key string (format: ``ddc_<env>_<random_32hex>``).

    Returns:
        12-character lowercase hex string, e.g. ``"a3f2c1e4b5d6"``.

    Example::

        client_id = derive_client_id("ddc_prod_a3f2c1e4b5d6e7f8a3f2c1e4b5d6e7f8")
        # → "5e3d2b1a0f9c"  (deterministic for the given input)
    """
    return hashlib.sha256(api_key.encode()).hexdigest()[:12]
