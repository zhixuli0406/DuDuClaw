"""Unit tests for duduclaw.agents.routing.memory_resolver.

Test-Driven Development (W19-P0) — Memory Lazy Reference Resolver.

Coverage targets (≥80%):
  TtlCache                — get/set/expiry/invalidate/clear/size
  MemoryRecordResolver    — success, cache hit, not-found, field projection, TTL
  MemorySearchResolver    — success, missing query_hint, cache hit, field projection
  MemoryLazyRefResolver   — can_resolve, resolve, resolve_all, backward compat
"""

from __future__ import annotations

import asyncio
import time
from typing import Any, Optional
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from duduclaw.agents.routing.memory_resolver import (
    SUPPORTED_REF_TYPES,
    MemoryClient,
    MemoryLazyRefResolver,
    MemoryRecordResolver,
    MemorySearchResolver,
    TtlCache,
    _project_fields,
)
from duduclaw.agents.routing.resolution import ResolutionError, ResolutionResult
from duduclaw.agents.routing.types import LazyRef


# ── Shared test data ──────────────────────────────────────────────────────────

_ENTRY_1: dict[str, Any] = {
    "id": "uuid-001",
    "content": "DuDuClaw uses Rust for the MCP server.",
    "layer": "episodic",
    "importance": 7.5,
    "tags": ["mcp", "rust"],
    "created_at": "2026-04-29T00:00:00Z",
}

_ENTRY_2: dict[str, Any] = {
    "id": "uuid-002",
    "content": "Memory consolidation runs nightly.",
    "layer": "semantic",
    "importance": 6.0,
    "tags": ["memory", "consolidation"],
    "created_at": "2026-04-29T01:00:00Z",
}


# ── Mock memory client ────────────────────────────────────────────────────────


class _MockMemoryClient(MemoryClient):
    """Configurable in-memory stub for testing."""

    def __init__(
        self,
        entries: Optional[dict[str, dict[str, Any]]] = None,
        search_results: Optional[list[dict[str, Any]]] = None,
    ) -> None:
        self._entries: dict[str, dict[str, Any]] = entries or {}
        self._search_results: list[dict[str, Any]] = search_results or []
        self.get_by_id_calls: list[str] = []
        self.search_calls: list[tuple] = []

    async def get_by_id(self, memory_id: str) -> Optional[dict[str, Any]]:
        self.get_by_id_calls.append(memory_id)
        return self._entries.get(memory_id)

    async def search(
        self,
        query: str,
        layer: Optional[str] = None,
        limit: int = 10,
    ) -> list[dict[str, Any]]:
        self.search_calls.append((query, layer, limit))
        return self._search_results[:limit]


class _ErrorMemoryClient(MemoryClient):
    """Always raises RuntimeError — simulates backend failure."""

    async def get_by_id(self, memory_id: str) -> Optional[dict[str, Any]]:
        raise RuntimeError(f"backend unavailable: {memory_id}")

    async def search(
        self,
        query: str,
        layer: Optional[str] = None,
        limit: int = 10,
    ) -> list[dict[str, Any]]:
        raise RuntimeError("search backend unavailable")


# ── Helpers ───────────────────────────────────────────────────────────────────


def _record_ref(
    ref_id: str = "uuid-001",
    ttl_seconds: Optional[float] = None,
    fields: Optional[list[str]] = None,
) -> LazyRef:
    meta: dict[str, Any] = {}
    if ttl_seconds is not None:
        meta["ttl_seconds"] = ttl_seconds
    if fields is not None:
        meta["fields"] = fields
    return LazyRef(ref_type="memory_record", ref_id=ref_id, metadata=meta)


def _search_ref(
    ref_id: str = "q-rust",
    query_hint: Optional[str] = "rust",
    layer: Optional[str] = None,
    limit: Optional[int] = None,
    ttl_seconds: Optional[float] = None,
    fields: Optional[list[str]] = None,
) -> LazyRef:
    meta: dict[str, Any] = {}
    if query_hint is not None:
        meta["query_hint"] = query_hint
    if layer is not None:
        meta["layer"] = layer
    if limit is not None:
        meta["limit"] = limit
    if ttl_seconds is not None:
        meta["ttl_seconds"] = ttl_seconds
    if fields is not None:
        meta["fields"] = fields
    return LazyRef(ref_type="memory_search", ref_id=ref_id, metadata=meta)


# ── _project_fields ───────────────────────────────────────────────────────────


class TestProjectFields:
    def test_none_data_returns_none(self):
        assert _project_fields(None, ["id"]) is None

    def test_no_fields_returns_full_dict(self):
        data = {"id": "1", "content": "x", "layer": "episodic"}
        assert _project_fields(data, None) is data  # same object

    def test_empty_fields_returns_full_dict(self):
        data = {"id": "1", "content": "x"}
        assert _project_fields(data, []) is data

    def test_projects_requested_fields(self):
        data = {"id": "1", "content": "hello", "layer": "semantic", "importance": 5.0}
        result = _project_fields(data, ["id", "content"])
        assert result == {"id": "1", "content": "hello"}
        assert "layer" not in result  # type: ignore[operator]
        assert "importance" not in result  # type: ignore[operator]

    def test_missing_field_silently_excluded(self):
        data = {"id": "1", "content": "x"}
        result = _project_fields(data, ["id", "nonexistent"])
        assert result == {"id": "1"}


# ── TtlCache ──────────────────────────────────────────────────────────────────


class TestTtlCache:
    def test_miss_on_empty_cache(self):
        cache = TtlCache()
        hit, val = cache.get("k1")
        assert not hit
        assert val is None

    def test_hit_returns_stored_value(self):
        cache = TtlCache()
        cache.set("k1", {"content": "hello"}, ttl_seconds=60)
        hit, val = cache.get("k1")
        assert hit
        assert val == {"content": "hello"}

    def test_expired_entry_returns_miss(self):
        cache = TtlCache()
        # Set with effectively-expired TTL by patching monotonic
        cache.set("k1", "value", ttl_seconds=60)
        # Simulate expiry by manipulating the entry directly
        cache._store["k1"].expires_at = time.monotonic() - 1.0
        hit, val = cache.get("k1")
        assert not hit
        assert val is None

    def test_expired_entry_is_evicted_from_store(self):
        cache = TtlCache()
        cache.set("k1", "value", ttl_seconds=60)
        cache._store["k1"].expires_at = time.monotonic() - 1.0
        cache.get("k1")  # triggers eviction
        assert "k1" not in cache._store

    def test_size_reflects_active_entries(self):
        cache = TtlCache()
        assert cache.size() == 0
        cache.set("k1", "a", 60)
        cache.set("k2", "b", 60)
        assert cache.size() == 2

    def test_invalidate_removes_entry(self):
        cache = TtlCache()
        cache.set("k1", "val", 60)
        cache.invalidate("k1")
        hit, _ = cache.get("k1")
        assert not hit

    def test_invalidate_nonexistent_key_is_noop(self):
        cache = TtlCache()
        cache.invalidate("does-not-exist")  # should not raise

    def test_clear_empties_cache(self):
        cache = TtlCache()
        cache.set("k1", "a", 60)
        cache.set("k2", "b", 60)
        cache.clear()
        assert cache.size() == 0

    def test_none_value_can_be_stored_and_retrieved(self):
        """Cache must be able to store None as a legitimate value."""
        cache = TtlCache()
        cache.set("k1", None, 60)
        hit, val = cache.get("k1")
        assert hit
        assert val is None


# ── MemoryRecordResolver ──────────────────────────────────────────────────────


class TestMemoryRecordResolver:
    def _client_with(self, *entries: dict[str, Any]) -> _MockMemoryClient:
        return _MockMemoryClient(entries={e["id"]: e for e in entries})

    async def test_success_returns_entry(self):
        client = self._client_with(_ENTRY_1)
        resolver = MemoryRecordResolver(client)
        ref = _record_ref(ref_id=_ENTRY_1["id"])
        result = await resolver.resolve(ref)
        assert result == _ENTRY_1

    async def test_not_found_raises_resolution_error(self):
        client = _MockMemoryClient()  # empty
        resolver = MemoryRecordResolver(client)
        ref = _record_ref(ref_id="nonexistent-uuid")
        with pytest.raises(ResolutionError, match="memory_record not found"):
            await resolver.resolve(ref)

    async def test_cache_hit_avoids_second_backend_call(self):
        client = self._client_with(_ENTRY_1)
        resolver = MemoryRecordResolver(client)
        ref = _record_ref(ref_id=_ENTRY_1["id"])

        await resolver.resolve(ref)  # first call — miss
        await resolver.resolve(ref)  # second call — should hit cache

        assert len(client.get_by_id_calls) == 1  # backend called exactly once

    async def test_field_projection_applied(self):
        client = self._client_with(_ENTRY_1)
        resolver = MemoryRecordResolver(client)
        ref = _record_ref(ref_id=_ENTRY_1["id"], fields=["id", "content"])
        result = await resolver.resolve(ref)
        assert result is not None
        assert set(result.keys()) == {"id", "content"}

    async def test_field_projection_does_not_corrupt_cache(self):
        """Projecting on the second call should still see all fields from cache."""
        client = self._client_with(_ENTRY_1)
        resolver = MemoryRecordResolver(client)

        ref_full = _record_ref(ref_id=_ENTRY_1["id"])
        ref_proj = _record_ref(ref_id=_ENTRY_1["id"], fields=["id"])

        full = await resolver.resolve(ref_full)
        proj = await resolver.resolve(ref_proj)

        # Full result has all keys
        assert "content" in full  # type: ignore[operator]
        # Projected result has only "id"
        assert set(proj.keys()) == {"id"}  # type: ignore[union-attr]
        # Backend called once (cache hit for projected call)
        assert len(client.get_by_id_calls) == 1

    async def test_custom_ttl_is_respected(self):
        """A TTL of 0 seconds causes immediate expiry — next get is a miss."""
        client = self._client_with(_ENTRY_1)
        resolver = MemoryRecordResolver(client)
        ref = _record_ref(ref_id=_ENTRY_1["id"], ttl_seconds=0)

        await resolver.resolve(ref)  # stores with TTL=0 (already expired)

        # Manually confirm cache entry expires immediately
        cache_key = f"record:{_ENTRY_1['id']}"
        hit, _ = resolver._cache.get(cache_key)
        assert not hit  # already expired

    async def test_ref_type_property(self):
        client = _MockMemoryClient()
        resolver = MemoryRecordResolver(client)
        assert resolver.ref_type == "memory_record"


# ── MemorySearchResolver ──────────────────────────────────────────────────────


class TestMemorySearchResolver:
    async def test_success_returns_list(self):
        client = _MockMemoryClient(search_results=[_ENTRY_1, _ENTRY_2])
        resolver = MemorySearchResolver(client)
        ref = _search_ref(query_hint="rust")
        result = await resolver.resolve(ref)
        assert isinstance(result, list)
        assert len(result) == 2
        assert result[0]["id"] == _ENTRY_1["id"]

    async def test_missing_query_hint_raises_resolution_error(self):
        client = _MockMemoryClient()
        resolver = MemorySearchResolver(client)
        ref = LazyRef(ref_type="memory_search", ref_id="q-1", metadata={})
        with pytest.raises(ResolutionError, match="query_hint"):
            await resolver.resolve(ref)

    async def test_blank_query_hint_raises_resolution_error(self):
        client = _MockMemoryClient()
        resolver = MemorySearchResolver(client)
        ref = _search_ref(query_hint="   ")  # whitespace only
        with pytest.raises(ResolutionError, match="query_hint"):
            await resolver.resolve(ref)

    async def test_cache_hit_avoids_second_search_call(self):
        client = _MockMemoryClient(search_results=[_ENTRY_1])
        resolver = MemorySearchResolver(client)
        ref = _search_ref(query_hint="rust")

        await resolver.resolve(ref)  # miss
        await resolver.resolve(ref)  # should hit cache

        assert len(client.search_calls) == 1

    async def test_field_projection_applied_to_results(self):
        client = _MockMemoryClient(search_results=[_ENTRY_1, _ENTRY_2])
        resolver = MemorySearchResolver(client)
        ref = _search_ref(query_hint="memory", fields=["id", "layer"])
        results = await resolver.resolve(ref)
        for entry in results:
            assert set(entry.keys()) == {"id", "layer"}  # type: ignore[arg-type]

    async def test_layer_filter_forwarded_to_backend(self):
        client = _MockMemoryClient(search_results=[_ENTRY_1])
        resolver = MemorySearchResolver(client)
        ref = _search_ref(query_hint="rust", layer="episodic")
        await resolver.resolve(ref)
        assert client.search_calls[0][1] == "episodic"

    async def test_limit_forwarded_to_backend(self):
        client = _MockMemoryClient(search_results=[_ENTRY_1, _ENTRY_2])
        resolver = MemorySearchResolver(client)
        ref = _search_ref(query_hint="info", limit=1)
        results = await resolver.resolve(ref)
        # Backend was called with limit=1
        assert client.search_calls[0][2] == 1
        assert len(results) == 1

    async def test_default_limit_is_10(self):
        client = _MockMemoryClient(search_results=[])
        resolver = MemorySearchResolver(client)
        ref = _search_ref(query_hint="x")
        await resolver.resolve(ref)
        _, _, limit = client.search_calls[0]
        assert limit == 10

    async def test_empty_search_results(self):
        client = _MockMemoryClient(search_results=[])
        resolver = MemorySearchResolver(client)
        ref = _search_ref(query_hint="no results here")
        result = await resolver.resolve(ref)
        assert result == []

    async def test_ref_type_property(self):
        client = _MockMemoryClient()
        resolver = MemorySearchResolver(client)
        assert resolver.ref_type == "memory_search"


# ── MemoryLazyRefResolver ─────────────────────────────────────────────────────


class TestMemoryLazyRefResolverCanResolve:
    def test_can_resolve_memory_record(self):
        r = MemoryLazyRefResolver(_MockMemoryClient())
        assert r.can_resolve("memory_record") is True

    def test_can_resolve_memory_search(self):
        r = MemoryLazyRefResolver(_MockMemoryClient())
        assert r.can_resolve("memory_search") is True

    def test_cannot_resolve_wiki(self):
        r = MemoryLazyRefResolver(_MockMemoryClient())
        assert r.can_resolve("wiki") is False

    def test_cannot_resolve_artifact(self):
        r = MemoryLazyRefResolver(_MockMemoryClient())
        assert r.can_resolve("artifact") is False

    def test_cannot_resolve_empty_string(self):
        r = MemoryLazyRefResolver(_MockMemoryClient())
        assert r.can_resolve("") is False

    def test_supported_ref_types_constant(self):
        assert "memory_record" in SUPPORTED_REF_TYPES
        assert "memory_search" in SUPPORTED_REF_TYPES


class TestMemoryLazyRefResolverResolve:
    async def test_resolve_memory_record_success(self):
        client = _MockMemoryClient(entries={_ENTRY_1["id"]: _ENTRY_1})
        resolver = MemoryLazyRefResolver(client)
        ref = _record_ref(ref_id=_ENTRY_1["id"])

        result = await resolver.resolve(ref)

        assert isinstance(result, ResolutionResult)
        assert result.resolved is True
        assert result.value == _ENTRY_1
        assert result.error is None
        assert result.ref is ref

    async def test_resolve_memory_search_success(self):
        client = _MockMemoryClient(search_results=[_ENTRY_1])
        resolver = MemoryLazyRefResolver(client)
        ref = _search_ref(query_hint="rust")

        result = await resolver.resolve(ref)

        assert result.resolved is True
        assert isinstance(result.value, list)
        assert result.value[0]["id"] == _ENTRY_1["id"]

    async def test_resolve_unsupported_type_returns_unresolved(self):
        resolver = MemoryLazyRefResolver(_MockMemoryClient())
        ref = LazyRef(ref_type="wiki", ref_id="some-page")

        result = await resolver.resolve(ref)

        assert result.resolved is False
        assert result.value is None
        assert result.error is not None
        assert "wiki" in result.error

    async def test_resolve_memory_record_not_found_returns_unresolved(self):
        resolver = MemoryLazyRefResolver(_MockMemoryClient())
        ref = _record_ref(ref_id="ghost-uuid")

        result = await resolver.resolve(ref)

        assert result.resolved is False
        assert result.error is not None
        assert "ghost-uuid" in result.error

    async def test_resolve_memory_search_missing_query_hint_returns_unresolved(self):
        resolver = MemoryLazyRefResolver(_MockMemoryClient())
        ref = LazyRef(ref_type="memory_search", ref_id="q-1", metadata={})

        result = await resolver.resolve(ref)

        assert result.resolved is False
        assert result.error is not None

    async def test_resolve_backend_exception_captured(self):
        """Unexpected backend errors must not propagate — captured in .error."""
        resolver = MemoryLazyRefResolver(_ErrorMemoryClient())
        ref = _record_ref(ref_id="any-id")

        result = await resolver.resolve(ref)

        assert result.resolved is False
        assert result.error is not None
        # Error message should mention the ref context
        assert "any-id" in result.error or "backend" in result.error.lower()

    async def test_resolve_ref_preserved_in_result(self):
        resolver = MemoryLazyRefResolver(_MockMemoryClient())
        ref = _record_ref(ref_id="x")

        result = await resolver.resolve(ref)

        assert result.ref is ref


class TestMemoryLazyRefResolverResolveAll:
    async def test_empty_list_returns_empty(self):
        resolver = MemoryLazyRefResolver(_MockMemoryClient())
        results = await resolver.resolve_all([])
        assert results == []

    async def test_all_succeed(self):
        client = _MockMemoryClient(
            entries={
                _ENTRY_1["id"]: _ENTRY_1,
                _ENTRY_2["id"]: _ENTRY_2,
            }
        )
        resolver = MemoryLazyRefResolver(client)
        refs = [_record_ref(_ENTRY_1["id"]), _record_ref(_ENTRY_2["id"])]

        results = await resolver.resolve_all(refs)

        assert len(results) == 2
        assert all(r.resolved for r in results)

    async def test_partial_failure_does_not_cancel_others(self):
        """One failing ref must not prevent others from resolving."""
        client = _MockMemoryClient(
            entries={_ENTRY_1["id"]: _ENTRY_1},
        )
        resolver = MemoryLazyRefResolver(client)
        refs = [
            _record_ref(_ENTRY_1["id"]),    # ← will succeed
            _record_ref("ghost-uuid"),       # ← will fail (not found)
            _record_ref(_ENTRY_1["id"]),    # ← will succeed (cache hit)
        ]

        results = await resolver.resolve_all(refs)

        assert len(results) == 3
        assert results[0].resolved is True
        assert results[1].resolved is False
        assert results[2].resolved is True

    async def test_result_order_matches_input(self):
        """resolve_all must preserve the input order."""
        client = _MockMemoryClient(
            entries={
                _ENTRY_1["id"]: _ENTRY_1,
                _ENTRY_2["id"]: _ENTRY_2,
            }
        )
        resolver = MemoryLazyRefResolver(client)
        refs = [_record_ref(_ENTRY_2["id"]), _record_ref(_ENTRY_1["id"])]

        results = await resolver.resolve_all(refs)

        assert results[0].ref is refs[0]
        assert results[1].ref is refs[1]
        assert results[0].value["id"] == _ENTRY_2["id"]  # type: ignore[index]
        assert results[1].value["id"] == _ENTRY_1["id"]  # type: ignore[index]

    async def test_mixed_ref_types(self):
        """resolve_all handles memory_record and memory_search in one call."""
        client = _MockMemoryClient(
            entries={_ENTRY_1["id"]: _ENTRY_1},
            search_results=[_ENTRY_2],
        )
        resolver = MemoryLazyRefResolver(client)
        refs = [
            _record_ref(_ENTRY_1["id"]),
            _search_ref(query_hint="consolidation"),
        ]

        results = await resolver.resolve_all(refs)

        assert len(results) == 2
        assert results[0].resolved is True
        assert results[1].resolved is True
        assert isinstance(results[1].value, list)

    async def test_unsupported_type_in_batch_captured(self):
        resolver = MemoryLazyRefResolver(_MockMemoryClient())
        refs = [
            _record_ref("nonexistent"),
            LazyRef(ref_type="wiki", ref_id="page-1"),
        ]

        results = await resolver.resolve_all(refs)

        assert results[1].resolved is False
        assert "wiki" in (results[1].error or "")


class TestMemoryLazyRefResolverBackwardCompat:
    """Verify that v0.1 packets (no lazy_refs) work without errors."""

    async def test_empty_memory_refs_in_v01_packet(self):
        """Simulates a HandoffPacketV1 with no memory refs — must return []."""
        resolver = MemoryLazyRefResolver(_MockMemoryClient())
        # v0.1 packets normalise to empty memory_refs tuple
        results = await resolver.resolve_all([])
        assert results == []

    async def test_resolver_exposed_sub_resolvers(self):
        """record_resolver and search_resolver properties are accessible."""
        resolver = MemoryLazyRefResolver(_MockMemoryClient())
        assert isinstance(resolver.record_resolver, MemoryRecordResolver)
        assert isinstance(resolver.search_resolver, MemorySearchResolver)


class TestMemoryLazyRefResolverSharedCache:
    """Verify that the shared TtlCache reduces redundant backend calls."""

    async def test_shared_cache_between_record_resolver_and_composite(self):
        """Both sub-resolvers share the same TtlCache — no double fetch."""
        client = _MockMemoryClient(entries={_ENTRY_1["id"]: _ENTRY_1})
        resolver = MemoryLazyRefResolver(client)
        ref = _record_ref(_ENTRY_1["id"])

        # Resolve via composite (populates shared cache)
        r1 = await resolver.resolve(ref)
        # Resolve again — must hit cache, not call backend again
        r2 = await resolver.resolve(ref)

        assert r1.resolved is True
        assert r2.resolved is True
        assert len(client.get_by_id_calls) == 1  # backend called once only
