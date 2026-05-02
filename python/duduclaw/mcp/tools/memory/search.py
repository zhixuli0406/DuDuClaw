"""MCP Server memory_search tool handler.

Endpoint: ``duduclaw/memory_search``
Scope required: ``memory:read``
P95 target: < 200ms (ENG-MEMORY SLA)

Workflow:
  1. Validate input parameters (ValidationError → 422)
  2. Inject client namespace (caller cannot override)
  3. Call internal memory_search backend
  4. Return results with query timing

Rate limits (from spec):
  Level 1 (external):       60 req/min
  Level 2 (trusted external): 300 req/min
  (Rate limiting is applied at the transport layer, not here.)
"""

from __future__ import annotations

import logging
import time
from typing import Any, Callable, Coroutine

from ...auth.types import APIKeyContext
from ...logging_utils import APIKeyMaskingFilter
from .namespace import NamespaceInjectionMiddleware
from .validation import validate_memory_search_params

logger = logging.getLogger(__name__)
logger.addFilter(APIKeyMaskingFilter())

# Type alias: the internal memory search function signature
MemorySearchFn = Callable[..., Coroutine[Any, Any, dict[str, Any]]]


class MemorySearchTool:
    """Handle the ``duduclaw/memory_search`` MCP endpoint.

    Validates, scopes, and executes a memory search for an external MCP client.
    The client's namespace is forcibly set to ``external/{client_id}`` — the
    caller cannot read memories outside their own namespace.

    Args:
        memory_search_fn:    Async callable that executes the actual memory search.
                             Expected signature::

                               async def search(
                                   query: str,
                                   namespace: str,
                                   limit: int,
                                   layer: str,
                                   min_relevance: float,
                               ) -> dict:
                                   ...

                             The returned dict should contain ``"memories"`` (list)
                             and optionally ``"total"`` (int).

        namespace_middleware: :class:`~.namespace.NamespaceInjectionMiddleware`
                              instance (typically a shared singleton).
    """

    def __init__(
        self,
        memory_search_fn: MemorySearchFn,
        namespace_middleware: NamespaceInjectionMiddleware,
    ) -> None:
        self._search_fn = memory_search_fn
        self._ns = namespace_middleware

    async def execute(
        self,
        raw_params: dict[str, Any],
        ctx: APIKeyContext,
    ) -> dict[str, Any]:
        """Execute ``memory_search`` with full namespace isolation.

        Args:
            raw_params: Raw parameters from the MCP caller (before validation).
            ctx:        Authenticated API Key context (from auth middleware).

        Returns:
            Dict conforming to ``duduclaw/memory_search`` output schema::

                {
                    "memories": [...],       # list of memory records
                    "total": 5,              # total matched count
                    "namespace": "external/abc123",   # informational only
                    "query_time_ms": 42,     # P95 target < 200ms
                }

        Raises:
            :exc:`~...errors.ValidationError`: If any parameter is invalid.
        """
        # Step 1: Validate and sanitise parameters
        params = validate_memory_search_params(raw_params)

        # Step 2: Inject namespace — caller value is silently overridden
        scoped = self._ns.inject(params, ctx)

        # Step 3: Call internal backend
        start = time.monotonic()
        try:
            result = await self._search_fn(
                query=scoped["query"],
                namespace=scoped["namespace"],
                limit=scoped["limit"],
                layer=scoped["layer"],
                min_relevance=scoped["min_relevance"],
            )
        except Exception:
            logger.exception(
                "memory_search backend error (client_id=%s)", ctx.client_id
            )
            raise
        elapsed_ms = int((time.monotonic() - start) * 1000)

        memories: list[dict[str, Any]] = result.get("memories", [])

        return {
            "memories": memories,
            "total": result.get("total", len(memories)),
            "namespace": scoped["namespace"],
            "query_time_ms": elapsed_ms,
        }
