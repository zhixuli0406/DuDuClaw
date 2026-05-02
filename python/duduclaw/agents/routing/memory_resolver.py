"""Memory Lazy Reference Resolver — OQ-01 HandoffPacket v0.2.

Resolves two memory-specific ``LazyRef`` types for the DuDuClaw Lazy Reference
Pattern (wiki/specs/oq-01-lazy-reference-pattern-v0.2.md):

memory_record
    Fetch a single memory entry by its UUID.
    ``LazyRef.ref_id``      = memory UUID (source_id)
    ``LazyRef.metadata``    = optional: ``ttl_seconds`` (float, default 300),
                              ``fields`` (list[str], field projection)

memory_search
    Semantic search using a query hint.
    ``LazyRef.ref_id``      = stable opaque key used *only* for cache keying
    ``LazyRef.metadata``    = required: ``query_hint`` (str);
                              optional: ``layer`` (str), ``limit`` (int, default 10),
                              ``ttl_seconds`` (float, default 60),
                              ``fields`` (list[str], field projection)

TTL caching is shared across both resolver types to avoid redundant backend calls.
All failures are captured in :class:`ResolutionResult` — no exceptions are propagated
to callers (graceful degradation).

Acceptance criteria (W19-P0)
-----------------------------
- memory_record / memory_search 兩種 ref 類型均可解析
- TTL 快取機制實作（避免重複拉取）
- 解析失敗優雅降級（ResolutionResult.error 填充，不崩潰）
- 與 v0.1 封包兼容（空 lazy_refs 時正常運作）
"""

from __future__ import annotations

import asyncio
import time
from dataclasses import dataclass, field
from typing import Any, Optional

from .resolution import ResolutionError, ResolutionResult, Resolver
from .types import LazyRef


# ── Supported ref types ───────────────────────────────────────────────────────

SUPPORTED_REF_TYPES: frozenset[str] = frozenset({"memory_record", "memory_search"})

# Default TTL values (seconds)
_DEFAULT_RECORD_TTL: float = 300.0   # 5 min — records rarely change mid-session
_DEFAULT_SEARCH_TTL: float = 60.0    # 1 min — search results change more often
_DEFAULT_SEARCH_LIMIT: int = 10


# ── Memory client protocol ────────────────────────────────────────────────────


class MemoryClient:
    """Abstract memory backend interface.

    Concrete implementations call the DuDuClaw MCP server (``memory_read``,
    ``memory_search_by_layer``) or a direct SQLite engine.  This class is a
    stub meant to be subclassed — or replaced by a :class:`typing.Protocol`
    compatible duck-typed implementation — in production code.

    Both methods **must** be coroutines (``async def``).
    """

    async def get_by_id(self, memory_id: str) -> Optional[dict[str, Any]]:
        """Return a memory entry dict, or ``None`` if not found."""
        raise NotImplementedError  # pragma: no cover

    async def search(
        self,
        query: str,
        layer: Optional[str] = None,
        limit: int = _DEFAULT_SEARCH_LIMIT,
    ) -> list[dict[str, Any]]:
        """Return a list of memory entry dicts matching *query*."""
        raise NotImplementedError  # pragma: no cover


# ── TTL cache ─────────────────────────────────────────────────────────────────


@dataclass
class _CacheEntry:
    value: Any
    expires_at: float  # monotonic timestamp


class TtlCache:
    """Simple in-process TTL cache backed by a plain dict.

    Thread-safety: relies on asyncio's single-threaded event loop model.
    Not safe for use across OS threads without an external lock.
    """

    def __init__(self) -> None:
        self._store: dict[str, _CacheEntry] = {}

    def get(self, key: str) -> tuple[bool, Any]:
        """Return ``(True, value)`` on hit; ``(False, None)`` on miss or expiry."""
        entry = self._store.get(key)
        if entry is None:
            return False, None
        if time.monotonic() >= entry.expires_at:
            del self._store[key]
            return False, None
        return True, entry.value

    def set(self, key: str, value: Any, ttl_seconds: float) -> None:
        """Store *value* under *key* for *ttl_seconds* seconds."""
        self._store[key] = _CacheEntry(
            value=value,
            expires_at=time.monotonic() + ttl_seconds,
        )

    def invalidate(self, key: str) -> None:
        """Remove *key* from the cache (no-op if absent)."""
        self._store.pop(key, None)

    def clear(self) -> None:
        """Remove all entries."""
        self._store.clear()

    def size(self) -> int:
        """Return the number of entries currently in the cache."""
        return len(self._store)


# ── Helpers ───────────────────────────────────────────────────────────────────


def _project_fields(
    data: Optional[dict[str, Any]],
    fields: Optional[list[str]],
) -> Optional[dict[str, Any]]:
    """Return only the requested *fields* from *data*.

    When *fields* is ``None`` or empty, the original dict is returned unchanged.
    When *data* is ``None``, ``None`` is returned (safe to call unconditionally).
    """
    if data is None or not fields:
        return data
    return {k: v for k, v in data.items() if k in fields}


# ── memory_record resolver ────────────────────────────────────────────────────


class MemoryRecordResolver(Resolver):
    """Fetch a single memory entry by its UUID (``ref_id`` = memory UUID).

    Delegates to :meth:`MemoryClient.get_by_id`.  Results are cached with a
    configurable TTL to avoid repeated round-trips for the same ID.

    Metadata keys (all optional)
    -----------------------------
    ``ttl_seconds`` : float
        Cache TTL.  Defaults to 300 s.
    ``fields`` : list[str]
        Field projection applied *after* caching — reduces token usage without
        degrading cache hit rate.

    Raises
    ------
    ResolutionError
        When the memory entry does not exist or the backend returns ``None``.
    """

    def __init__(
        self,
        client: MemoryClient,
        cache: Optional[TtlCache] = None,
    ) -> None:
        self._client = client
        self._cache = cache if cache is not None else TtlCache()

    @property
    def ref_type(self) -> str:
        return "memory_record"

    async def resolve(self, ref: LazyRef) -> Optional[dict[str, Any]]:  # type: ignore[override]
        memory_id = ref.ref_id
        ttl = float(ref.metadata.get("ttl_seconds", _DEFAULT_RECORD_TTL))
        fields: Optional[list[str]] = ref.metadata.get("fields")

        cache_key = f"record:{memory_id}"
        hit, cached = self._cache.get(cache_key)
        if hit:
            return _project_fields(cached, fields)

        entry = await self._client.get_by_id(memory_id)
        if entry is None:
            raise ResolutionError(f"memory_record not found: {memory_id!r}")

        # Cache the full entry; projection is applied on read to preserve hit rate.
        self._cache.set(cache_key, entry, ttl)
        return _project_fields(entry, fields)


# ── memory_search resolver ────────────────────────────────────────────────────


class MemorySearchResolver(Resolver):
    """Execute a semantic memory search using ``query_hint`` from metadata.

    Delegates to :meth:`MemoryClient.search`.  Results are cached per
    ``(query_hint, layer, limit)`` triple with a configurable TTL.

    Metadata keys
    -------------
    ``query_hint`` : str (required)
        Natural-language search query sent to the memory backend.
    ``layer`` : str | None (optional)
        Cognitive layer filter: ``"episodic"``, ``"semantic"``, or omit for all.
    ``limit`` : int (optional)
        Maximum number of results.  Defaults to 10.
    ``ttl_seconds`` : float (optional)
        Cache TTL.  Defaults to 60 s.
    ``fields`` : list[str] (optional)
        Field projection applied after caching.

    Raises
    ------
    ResolutionError
        When ``query_hint`` is absent or blank.
    """

    def __init__(
        self,
        client: MemoryClient,
        cache: Optional[TtlCache] = None,
    ) -> None:
        self._client = client
        self._cache = cache if cache is not None else TtlCache()

    @property
    def ref_type(self) -> str:
        return "memory_search"

    async def resolve(self, ref: LazyRef) -> list[dict[str, Any]]:  # type: ignore[override]
        query_hint: Optional[str] = ref.metadata.get("query_hint")
        if not query_hint or not query_hint.strip():
            raise ResolutionError(
                f"memory_search ref {ref.ref_id!r} is missing required "
                "metadata.query_hint (must be a non-empty string)"
            )

        layer: Optional[str] = ref.metadata.get("layer")
        limit = int(ref.metadata.get("limit", _DEFAULT_SEARCH_LIMIT))
        ttl = float(ref.metadata.get("ttl_seconds", _DEFAULT_SEARCH_TTL))
        fields: Optional[list[str]] = ref.metadata.get("fields")

        # Cache key encodes all parameters that affect result content.
        cache_key = f"search:{query_hint}:{layer}:{limit}"
        hit, cached = self._cache.get(cache_key)
        if hit:
            return [_project_fields(e, fields) for e in cached]  # type: ignore[misc]

        results = await self._client.search(
            query=query_hint, layer=layer, limit=limit
        )

        # Cache raw (unprojected) results so field projection can vary per caller.
        self._cache.set(cache_key, results, ttl)
        return [_project_fields(e, fields) for e in results]  # type: ignore[misc]


# ── Composite resolver ────────────────────────────────────────────────────────


class MemoryLazyRefResolver:
    """Composite resolver that handles both ``memory_record`` and ``memory_search``.

    A single instance can be registered with a :class:`ResolutionPolicy` by
    using the two underlying :class:`Resolver` subclasses, *or* used directly
    via :meth:`resolve` / :meth:`resolve_all`.

    The internal :class:`TtlCache` is **shared** between both resolver types so
    a single ``memory_record`` fetch populates the cache for both resolvers when
    the same ID appears in a subsequent search result.

    Example::

        client = MyMemoryClient(mcp_server_url="...")
        resolver = MemoryLazyRefResolver(client)

        # Check support
        assert resolver.can_resolve("memory_record")
        assert not resolver.can_resolve("wiki")

        # Resolve one ref
        result = await resolver.resolve(
            LazyRef(ref_type="memory_record", ref_id="uuid-123")
        )
        if result.resolved:
            print(result.value["content"])

        # Resolve many refs concurrently
        results = await resolver.resolve_all(packet.memory_refs)

    Backward compatibility
    ----------------------
    When ``refs`` is empty (v0.1 packets have no ``lazy_refs`` field), both
    :meth:`resolve_all` returns an empty list immediately — no backend calls.
    """

    def __init__(self, client: MemoryClient) -> None:
        shared_cache = TtlCache()
        self._record_resolver = MemoryRecordResolver(client, cache=shared_cache)
        self._search_resolver = MemorySearchResolver(client, cache=shared_cache)
        self._by_type: dict[str, Resolver] = {
            "memory_record": self._record_resolver,
            "memory_search": self._search_resolver,
        }

    # ── Query ─────────────────────────────────────────────────────────────────

    def can_resolve(self, ref_type: str) -> bool:
        """Return ``True`` iff this resolver handles *ref_type*."""
        return ref_type in SUPPORTED_REF_TYPES

    # ── Resolution ────────────────────────────────────────────────────────────

    async def resolve(self, ref: LazyRef) -> ResolutionResult:
        """Resolve *ref*, capturing any error in :attr:`ResolutionResult.error`.

        Never raises; callers receive a :class:`ResolutionResult` in all cases.
        """
        resolver = self._by_type.get(ref.ref_type)
        if resolver is None:
            return ResolutionResult(
                ref=ref,
                value=None,
                resolved=False,
                error=(
                    f"MemoryLazyRefResolver: unsupported ref_type='{ref.ref_type}'. "
                    f"Supported types: {sorted(SUPPORTED_REF_TYPES)}"
                ),
            )

        try:
            value = await resolver.resolve(ref)
            return ResolutionResult(ref=ref, value=value, resolved=True)
        except ResolutionError as exc:
            return ResolutionResult(
                ref=ref, value=None, resolved=False, error=str(exc)
            )
        except Exception as exc:  # noqa: BLE001 — catch-all for graceful degradation
            return ResolutionResult(
                ref=ref,
                value=None,
                resolved=False,
                error=(
                    f"Unexpected error resolving "
                    f"{ref.ref_type}/{ref.ref_id!r}: {exc}"
                ),
            )

    async def resolve_all(self, refs: list[LazyRef]) -> list[ResolutionResult]:
        """Resolve *refs* concurrently.

        Failures in individual refs **do not cancel** others — every ref gets a
        :class:`ResolutionResult` regardless.  Order is preserved.

        Empty input returns an empty list immediately (v0.1 packet compat).
        """
        if not refs:
            return []
        return list(await asyncio.gather(*[self.resolve(ref) for ref in refs]))

    # ── Resolver access (for ResolutionPolicy integration) ────────────────────

    @property
    def record_resolver(self) -> MemoryRecordResolver:
        """The underlying :class:`MemoryRecordResolver` (for direct registration)."""
        return self._record_resolver

    @property
    def search_resolver(self) -> MemorySearchResolver:
        """The underlying :class:`MemorySearchResolver` (for direct registration)."""
        return self._search_resolver
