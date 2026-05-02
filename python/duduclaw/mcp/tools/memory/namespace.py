"""Namespace injection middleware for MCP Server memory tools.

Security requirement (TL Decision 2026-04-29):
  - External MCP Clients CANNOT choose their own namespace.
  - Server MUST inject ``external/{client_id}`` and override any caller-supplied value.
  - Any access to ``internal/`` prefixed namespaces MUST be denied (→ 404).

Namespace hierarchy:
  internal/                    ← DuDuClaw internal agents only
  └── internal/duduclaw-tl
  └── internal/duduclaw-eng-*
  external/                    ← external MCP Clients (this module manages)
  └── external/{client_id}    ← isolated per API Key

Design:
  NamespaceInjectionMiddleware is a pure, stateless transformation.
  It does not perform I/O — inject() returns a new dict (immutable pattern).
"""

from __future__ import annotations

from typing import Any

from ...auth.types import APIKeyContext


class NamespaceInjectionMiddleware:
    """Enforce namespace isolation for external MCP Clients.

    All external client memory operations are scoped to ``external/{client_id}``.
    The client_id is derived from the authenticated API Key and cannot be
    influenced by the caller.

    This middleware is intentionally stateless — a single instance can be
    shared across all tool handlers and requests.
    """

    def inject(self, params: dict[str, Any], ctx: APIKeyContext) -> dict[str, Any]:
        """Return a copy of *params* with ``namespace`` forcibly set to the client's namespace.

        The caller's ``namespace`` field (if any) is silently overridden.
        Original *params* are never mutated (immutable pattern).

        Args:
            params: Raw or validated parameters from the MCP caller.
            ctx:    Authenticated API Key context.

        Returns:
            New dict with all original fields plus ``namespace`` set to
            ``external/{ctx.client_id}``.
        """
        return {**params, "namespace": ctx.namespace}

    def validate_namespace_access(
        self,
        requested_namespace: str,
        ctx: APIKeyContext,
    ) -> bool:
        """Verify that *ctx* is authorised to access *requested_namespace*.

        Rules:
          1. Any namespace starting with ``"internal/"`` is forbidden.
          2. The namespace must exactly match ``ctx.namespace`` (``external/{client_id}``).

        Args:
            requested_namespace: The namespace found on the stored resource.
            ctx:                 Authenticated API Key context for the calling client.

        Returns:
            ``True`` iff access is permitted; ``False`` otherwise.
        """
        if not requested_namespace:
            return False
        if requested_namespace.startswith("internal/"):
            return False
        return requested_namespace == ctx.namespace
