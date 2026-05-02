"""MCP Server memory_read tool handler.

Endpoint: ``duduclaw/memory_read``
Scope required: ``memory:read``

Workflow:
  1. Validate memory_id format (UUID v4)
  2. Fetch memory from internal backend
  3. Verify namespace ownership — client can only read its own namespace
  4. Return full memory record

Security (§4.2 mcp-memory-endpoints-design.md):
  - Cross-namespace access → 404 (NOT 403)
  - internal/ namespace access → 404
  - Non-existent memory → 404
  All three cases return 404 to avoid confirming resource existence
  in other namespaces (prevents namespace enumeration attacks).
"""

from __future__ import annotations

import logging
from typing import Any, Callable, Coroutine, Optional

from ...auth.types import APIKeyContext
from ...errors import NotFoundError
from ...logging_utils import APIKeyMaskingFilter
from .namespace import NamespaceInjectionMiddleware
from .validation import validate_memory_read_params

logger = logging.getLogger(__name__)
logger.addFilter(APIKeyMaskingFilter())

# Type alias: the internal memory read function signature
# Returns None when the memory_id is not found.
MemoryReadFn = Callable[..., Coroutine[Any, Any, Optional[dict[str, Any]]]]


class MemoryReadTool:
    """Handle the ``duduclaw/memory_read`` MCP endpoint.

    Fetches a specific memory record by ID, enforcing strict namespace isolation.
    Any attempt to access memories belonging to other clients or the internal
    namespace returns HTTP 404 (not 403), to avoid confirming resource existence.

    Args:
        memory_read_fn:      Async callable that fetches a memory by ID.
                             Expected signature::

                               async def read(memory_id: str) -> dict | None:
                                   ...

                             Returns the full memory record dict, or ``None`` if
                             not found.  The dict must include a ``"namespace"``
                             field for ownership verification.

        namespace_middleware: :class:`~.namespace.NamespaceInjectionMiddleware` instance.
    """

    def __init__(
        self,
        memory_read_fn: MemoryReadFn,
        namespace_middleware: NamespaceInjectionMiddleware,
    ) -> None:
        self._read_fn = memory_read_fn
        self._ns = namespace_middleware

    async def execute(
        self,
        raw_params: dict[str, Any],
        ctx: APIKeyContext,
    ) -> dict[str, Any]:
        """Execute ``memory_read`` with namespace ownership validation.

        Args:
            raw_params: Raw parameters from the MCP caller (before validation).
            ctx:        Authenticated API Key context.

        Returns:
            Dict conforming to ``duduclaw/memory_read`` output schema::

                {
                    "id": "550e8400-...",
                    "content": "...",
                    "layer": "episodic",
                    "namespace": "external/abc123",
                    "tags": ["tag1"],
                    "created_at": "2026-04-29T00:00:00Z",
                    "updated_at": "2026-04-29T00:00:00Z",
                    "ttl_expires_at": null,
                }

        Raises:
            :exc:`~...errors.ValidationError`: If ``memory_id`` format is invalid.
            :exc:`~...errors.NotFoundError`:   If memory is not found OR belongs
                                               to a different namespace (404 in all cases).
        """
        # Step 1: Validate memory_id format
        params = validate_memory_read_params(raw_params)
        memory_id = params["memory_id"]

        # Step 2: Fetch from backend
        try:
            record = await self._read_fn(memory_id=memory_id)
        except Exception:
            logger.exception(
                "memory_read backend error (client_id=%s, memory_id=%s)",
                ctx.client_id,
                memory_id,
            )
            raise

        # Step 3: Not found → 404 (no info leakage)
        if record is None:
            raise NotFoundError("Memory not found")

        # Step 4: Namespace ownership check
        #   - Cross-namespace access → 404 (same as not-found; prevents enumeration)
        #   - internal/ prefix → 404
        record_namespace: str = record.get("namespace", "")
        if not self._ns.validate_namespace_access(record_namespace, ctx):
            # Deliberately NOT logging memory_id here (would leak cross-namespace info)
            logger.warning(
                "memory_read namespace mismatch (client=%s, record_ns=%s)",
                ctx.client_id,
                record_namespace,
            )
            raise NotFoundError("Memory not found")

        # Step 5: Return record (only expose documented fields)
        return {
            "id": record.get("id"),
            "content": record.get("content"),
            "layer": record.get("layer"),
            "namespace": record.get("namespace"),
            "tags": record.get("tags", []),
            "created_at": record.get("created_at"),
            "updated_at": record.get("updated_at"),
            "ttl_expires_at": record.get("ttl_expires_at"),
        }
