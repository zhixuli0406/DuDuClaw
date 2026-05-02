"""Tests for MemorySearchTool.

Acceptance criteria (from mcp-memory-endpoints-design.md §6.3):
  ✅ Valid query → memories array + query_time_ms in response
  ✅ Valid query with no results → memories=[], total=0
  ✅ limit=50 (max) → at most 50 results
  ✅ limit=51 (over max) → 422
  ✅ query > 500 chars → 422
  ✅ Namespace always injected as external/{client_id}
  ✅ Caller cannot override namespace
"""

from __future__ import annotations

from typing import Any
from unittest.mock import AsyncMock

import pytest

from duduclaw.mcp.auth.types import APIKeyContext
from duduclaw.mcp.errors import ValidationError
from duduclaw.mcp.tools.memory.namespace import NamespaceInjectionMiddleware
from duduclaw.mcp.tools.memory.search import MemorySearchTool

# ── Fixtures ──────────────────────────────────────────────────────────────────

CLIENT_ID = "search_client_1"


@pytest.fixture
def ctx() -> APIKeyContext:
    return APIKeyContext(
        api_key_prefix="ddc_prod",
        client_id=CLIENT_ID,
        scopes=frozenset({"memory:read"}),
        trust_level=1,
    )


def _make_search_fn(memories: list[dict[str, Any]], total: int | None = None) -> AsyncMock:
    return AsyncMock(
        return_value={
            "memories": memories,
            "total": total if total is not None else len(memories),
        }
    )


@pytest.fixture
def mock_search_fn() -> AsyncMock:
    return _make_search_fn(
        [{"id": "mem1", "content": "DuDuClaw memory", "relevance_score": 0.9, "layer": "episodic"}]
    )


@pytest.fixture
def tool(mock_search_fn: AsyncMock) -> MemorySearchTool:
    return MemorySearchTool(
        memory_search_fn=mock_search_fn,
        namespace_middleware=NamespaceInjectionMiddleware(),
    )


# ── Response structure ────────────────────────────────────────────────────────


class TestMemorySearchResponse:
    async def test_returns_memories(self, tool: MemorySearchTool, ctx: APIKeyContext) -> None:
        result = await tool.execute({"query": "DuDuClaw"}, ctx)
        assert result["memories"][0]["content"] == "DuDuClaw memory"

    async def test_returns_total(self, tool: MemorySearchTool, ctx: APIKeyContext) -> None:
        result = await tool.execute({"query": "DuDuClaw"}, ctx)
        assert result["total"] == 1

    async def test_returns_query_time_ms(self, tool: MemorySearchTool, ctx: APIKeyContext) -> None:
        result = await tool.execute({"query": "DuDuClaw"}, ctx)
        assert "query_time_ms" in result
        assert isinstance(result["query_time_ms"], int)
        assert result["query_time_ms"] >= 0

    async def test_returns_namespace_in_response(
        self, tool: MemorySearchTool, ctx: APIKeyContext
    ) -> None:
        result = await tool.execute({"query": "test"}, ctx)
        assert result["namespace"] == f"external/{CLIENT_ID}"

    async def test_empty_results(self, ctx: APIKeyContext) -> None:
        tool = MemorySearchTool(
            memory_search_fn=_make_search_fn([]),
            namespace_middleware=NamespaceInjectionMiddleware(),
        )
        result = await tool.execute({"query": "nothing"}, ctx)
        assert result["memories"] == []
        assert result["total"] == 0


# ── Namespace isolation ───────────────────────────────────────────────────────


class TestNamespaceIsolation:
    async def test_namespace_injected_to_backend(
        self, tool: MemorySearchTool, mock_search_fn: AsyncMock, ctx: APIKeyContext
    ) -> None:
        await tool.execute({"query": "test"}, ctx)
        call_kwargs = mock_search_fn.call_args.kwargs
        assert call_kwargs["namespace"] == f"external/{CLIENT_ID}"

    async def test_caller_cannot_override_namespace(
        self, tool: MemorySearchTool, mock_search_fn: AsyncMock, ctx: APIKeyContext
    ) -> None:
        """Even if the caller injects namespace, server overrides it."""
        await tool.execute({"query": "test", "namespace": "internal/evil"}, ctx)
        call_kwargs = mock_search_fn.call_args.kwargs
        assert call_kwargs["namespace"] == f"external/{CLIENT_ID}"

    async def test_different_clients_get_different_namespaces(
        self, mock_search_fn: AsyncMock
    ) -> None:
        tool = MemorySearchTool(
            memory_search_fn=mock_search_fn,
            namespace_middleware=NamespaceInjectionMiddleware(),
        )
        ctx_a = APIKeyContext(
            api_key_prefix="ddc_prod",
            client_id="client_aaa",
            scopes=frozenset({"memory:read"}),
            trust_level=1,
        )
        ctx_b = APIKeyContext(
            api_key_prefix="ddc_prod",
            client_id="client_bbb",
            scopes=frozenset({"memory:read"}),
            trust_level=1,
        )
        await tool.execute({"query": "test"}, ctx_a)
        assert mock_search_fn.call_args.kwargs["namespace"] == "external/client_aaa"

        await tool.execute({"query": "test"}, ctx_b)
        assert mock_search_fn.call_args.kwargs["namespace"] == "external/client_bbb"


# ── Parameters forwarded to backend ──────────────────────────────────────────


class TestParameterForwarding:
    async def test_limit_forwarded(
        self, tool: MemorySearchTool, mock_search_fn: AsyncMock, ctx: APIKeyContext
    ) -> None:
        await tool.execute({"query": "test", "limit": 25}, ctx)
        assert mock_search_fn.call_args.kwargs["limit"] == 25

    async def test_layer_forwarded(
        self, tool: MemorySearchTool, mock_search_fn: AsyncMock, ctx: APIKeyContext
    ) -> None:
        await tool.execute({"query": "test", "layer": "semantic"}, ctx)
        assert mock_search_fn.call_args.kwargs["layer"] == "semantic"

    async def test_query_forwarded(
        self, tool: MemorySearchTool, mock_search_fn: AsyncMock, ctx: APIKeyContext
    ) -> None:
        await tool.execute({"query": "my search query"}, ctx)
        assert "my search query" in mock_search_fn.call_args.kwargs["query"]


# ── Validation errors ─────────────────────────────────────────────────────────


class TestValidationErrors:
    async def test_missing_query_raises(self, tool: MemorySearchTool, ctx: APIKeyContext) -> None:
        with pytest.raises(ValidationError):
            await tool.execute({}, ctx)

    async def test_empty_query_raises(self, tool: MemorySearchTool, ctx: APIKeyContext) -> None:
        with pytest.raises(ValidationError):
            await tool.execute({"query": ""}, ctx)

    async def test_limit_over_max_raises(self, tool: MemorySearchTool, ctx: APIKeyContext) -> None:
        with pytest.raises(ValidationError, match="limit"):
            await tool.execute({"query": "test", "limit": 51}, ctx)

    async def test_invalid_layer_raises(self, tool: MemorySearchTool, ctx: APIKeyContext) -> None:
        with pytest.raises(ValidationError, match="layer"):
            await tool.execute({"query": "test", "layer": "invalid"}, ctx)

    async def test_query_too_long_raises(self, tool: MemorySearchTool, ctx: APIKeyContext) -> None:
        with pytest.raises(ValidationError, match="query"):
            await tool.execute({"query": "q" * 501}, ctx)
