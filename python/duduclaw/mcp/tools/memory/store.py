"""MCP Server memory_store tool handler.

Endpoint: ``duduclaw/memory_store``
Scope required: ``memory:write``

Workflow:
  1. Validate input parameters (ValidationError → 422)
  2. Check daily write quota — BEFORE write (QuotaExceededError → 429)
  3. Inject client namespace (caller cannot override)
  4. Call internal memory_store backend
  5. Increment quota counter AFTER successful write

Quota (TL Decision 2026-04-29):
  - Default: 1000 records/day per client
  - Reset: UTC 00:00
  - Exceeded: HTTP 429 + retry_after + reset_at

Rate limits:
  Level 1 (external):       10 req/min
  Level 2 (trusted external): 60 req/min
  (Applied at transport layer.)
"""

from __future__ import annotations

import logging
from typing import Any, Callable, Coroutine

from ...auth.types import APIKeyContext
from ...logging_utils import APIKeyMaskingFilter
from .namespace import NamespaceInjectionMiddleware
from .quota import QuotaEnforcer
from .validation import validate_memory_store_params

logger = logging.getLogger(__name__)
logger.addFilter(APIKeyMaskingFilter())

# Type alias: the internal memory store function signature
MemoryStoreFn = Callable[..., Coroutine[Any, Any, dict[str, Any]]]


class MemoryStoreTool:
    """Handle the ``duduclaw/memory_store`` MCP endpoint.

    Writes a memory record to the client's isolated namespace with daily quota
    enforcement.  The namespace is forcibly set by the server — the caller
    cannot write to any namespace other than ``external/{client_id}``.

    Args:
        memory_store_fn:     Async callable for the actual memory write.
                             Expected signature::

                               async def store(
                                   content: str,
                                   namespace: str,
                                   layer: str,
                                   tags: list[str],
                                   ttl_days: int | None,
                               ) -> dict:
                                   ...

                             The returned dict must contain ``"id"`` (str) and
                             ``"created_at"`` (ISO 8601 string).

        namespace_middleware: :class:`~.namespace.NamespaceInjectionMiddleware` instance.
        quota_enforcer:       :class:`~.quota.QuotaEnforcer` instance.
    """

    def __init__(
        self,
        memory_store_fn: MemoryStoreFn,
        namespace_middleware: NamespaceInjectionMiddleware,
        quota_enforcer: QuotaEnforcer,
    ) -> None:
        self._store_fn = memory_store_fn
        self._ns = namespace_middleware
        self._quota = quota_enforcer

    async def execute(
        self,
        raw_params: dict[str, Any],
        ctx: APIKeyContext,
    ) -> dict[str, Any]:
        """Execute ``memory_store`` with quota check and namespace isolation.

        Args:
            raw_params: Raw parameters from the MCP caller (before validation).
            ctx:        Authenticated API Key context.

        Returns:
            Dict conforming to ``duduclaw/memory_store`` output schema::

                {
                    "id": "550e8400-...",
                    "namespace": "external/abc123",
                    "created_at": "2026-04-29T00:00:00Z",
                    "quota_used": 1,
                    "quota_limit": 1000,
                    "quota_remaining": 999,
                }

        Raises:
            :exc:`~...errors.ValidationError`:    If any parameter is invalid.
            :exc:`~...errors.QuotaExceededError`: If daily write quota is exceeded.
        """
        # Step 1: Validate and sanitise parameters
        params = validate_memory_store_params(raw_params)

        # Step 2: Quota check BEFORE write — fail fast before any I/O
        self._quota.check_or_raise(ctx.client_id)

        # Step 3: Inject namespace
        scoped = self._ns.inject(params, ctx)

        # Step 4: Write memory (quota is NOT incremented until write succeeds)
        try:
            result = await self._store_fn(
                content=scoped["content"],
                namespace=scoped["namespace"],
                layer=scoped["layer"],
                tags=scoped["tags"],
                ttl_days=scoped.get("ttl_days"),
            )
        except Exception:
            logger.exception(
                "memory_store backend error (client_id=%s)", ctx.client_id
            )
            raise

        # Step 5: Increment quota AFTER successful write
        quota_info = self._quota.increment(ctx.client_id)

        return {
            "id": result["id"],
            "namespace": scoped["namespace"],
            "created_at": result["created_at"],
            "quota_used": quota_info.used,
            "quota_limit": quota_info.limit,
            "quota_remaining": quota_info.remaining,
        }
