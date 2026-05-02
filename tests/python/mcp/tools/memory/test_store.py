"""Tests for MemoryStoreTool.

Acceptance criteria (from mcp-memory-endpoints-design.md §6.4):
  ✅ Valid write → returns id + quota info
  ✅ content > 4096 → 422
  ✅ tags > 10 → 422
  ✅ ttl_days=0 → 422
  ✅ layer="invalid" → 422
  ✅ Quota exceeded → 429
  ✅ Quota NOT incremented when backend write fails
  ✅ Namespace injected as external/{client_id}
"""

from __future__ import annotations

from unittest.mock import AsyncMock

import pytest

from duduclaw.mcp.auth.types import APIKeyContext
from duduclaw.mcp.errors import QuotaExceededError, ValidationError
from duduclaw.mcp.tools.memory.namespace import NamespaceInjectionMiddleware
from duduclaw.mcp.tools.memory.quota import QuotaEnforcer
from duduclaw.mcp.tools.memory.store import MemoryStoreTool

# ── Constants ─────────────────────────────────────────────────────────────────

CLIENT_ID = "store_client_1"
MOCK_MEMORY_ID = "550e8400-e29b-41d4-a716-446655440000"

# ── Fixtures ──────────────────────────────────────────────────────────────────


@pytest.fixture
def ctx() -> APIKeyContext:
    return APIKeyContext(
        api_key_prefix="ddc_prod",
        client_id=CLIENT_ID,
        scopes=frozenset({"memory:write"}),
        trust_level=1,
    )


@pytest.fixture
def mock_store_fn() -> AsyncMock:
    return AsyncMock(
        return_value={
            "id": MOCK_MEMORY_ID,
            "created_at": "2026-04-29T00:00:00Z",
        }
    )


@pytest.fixture
def quota() -> QuotaEnforcer:
    return QuotaEnforcer(default_limit=1000)


@pytest.fixture
def tool(mock_store_fn: AsyncMock, quota: QuotaEnforcer) -> MemoryStoreTool:
    return MemoryStoreTool(
        memory_store_fn=mock_store_fn,
        namespace_middleware=NamespaceInjectionMiddleware(),
        quota_enforcer=quota,
    )


# ── Successful writes ─────────────────────────────────────────────────────────


class TestSuccessfulWrite:
    async def test_returns_id(self, tool: MemoryStoreTool, ctx: APIKeyContext) -> None:
        result = await tool.execute({"content": "hello"}, ctx)
        assert result["id"] == MOCK_MEMORY_ID

    async def test_returns_created_at(self, tool: MemoryStoreTool, ctx: APIKeyContext) -> None:
        result = await tool.execute({"content": "hello"}, ctx)
        assert result["created_at"] == "2026-04-29T00:00:00Z"

    async def test_returns_namespace(self, tool: MemoryStoreTool, ctx: APIKeyContext) -> None:
        result = await tool.execute({"content": "hello"}, ctx)
        assert result["namespace"] == f"external/{CLIENT_ID}"

    async def test_returns_quota_info(self, tool: MemoryStoreTool, ctx: APIKeyContext) -> None:
        result = await tool.execute({"content": "hello"}, ctx)
        assert result["quota_used"] == 1
        assert result["quota_limit"] == 1000
        assert result["quota_remaining"] == 999

    async def test_quota_increments_per_write(
        self, tool: MemoryStoreTool, ctx: APIKeyContext
    ) -> None:
        for i in range(1, 4):
            result = await tool.execute({"content": f"write {i}"}, ctx)
            assert result["quota_used"] == i

    async def test_namespace_injected_to_backend(
        self, tool: MemoryStoreTool, mock_store_fn: AsyncMock, ctx: APIKeyContext
    ) -> None:
        await tool.execute({"content": "hello"}, ctx)
        assert mock_store_fn.call_args.kwargs["namespace"] == f"external/{CLIENT_ID}"

    async def test_layer_default_episodic(
        self, tool: MemoryStoreTool, mock_store_fn: AsyncMock, ctx: APIKeyContext
    ) -> None:
        await tool.execute({"content": "hello"}, ctx)
        assert mock_store_fn.call_args.kwargs["layer"] == "episodic"

    async def test_layer_semantic_forwarded(
        self, tool: MemoryStoreTool, mock_store_fn: AsyncMock, ctx: APIKeyContext
    ) -> None:
        await tool.execute({"content": "hello", "layer": "semantic"}, ctx)
        assert mock_store_fn.call_args.kwargs["layer"] == "semantic"

    async def test_tags_forwarded(
        self, tool: MemoryStoreTool, mock_store_fn: AsyncMock, ctx: APIKeyContext
    ) -> None:
        await tool.execute({"content": "hello", "tags": ["a", "b"]}, ctx)
        assert mock_store_fn.call_args.kwargs["tags"] == ["a", "b"]

    async def test_ttl_days_forwarded(
        self, tool: MemoryStoreTool, mock_store_fn: AsyncMock, ctx: APIKeyContext
    ) -> None:
        await tool.execute({"content": "hello", "ttl_days": 30}, ctx)
        assert mock_store_fn.call_args.kwargs["ttl_days"] == 30


# ── Quota enforcement ─────────────────────────────────────────────────────────


class TestQuotaEnforcement:
    async def test_quota_exceeded_raises_429(
        self, mock_store_fn: AsyncMock, ctx: APIKeyContext
    ) -> None:
        enforcer = QuotaEnforcer(default_limit=2)
        enforcer.increment(CLIENT_ID)
        enforcer.increment(CLIENT_ID)  # at limit
        tool = MemoryStoreTool(
            memory_store_fn=mock_store_fn,
            namespace_middleware=NamespaceInjectionMiddleware(),
            quota_enforcer=enforcer,
        )
        with pytest.raises(QuotaExceededError) as exc_info:
            await tool.execute({"content": "blocked"}, ctx)
        assert exc_info.value.http_status == 429
        assert exc_info.value.quota_used == 2
        assert exc_info.value.quota_limit == 2

    async def test_quota_not_incremented_when_backend_fails(
        self, ctx: APIKeyContext
    ) -> None:
        """Quota counter must NOT be incremented if the backend write fails."""
        failing_store = AsyncMock(side_effect=RuntimeError("backend down"))
        enforcer = QuotaEnforcer(default_limit=1000)
        tool = MemoryStoreTool(
            memory_store_fn=failing_store,
            namespace_middleware=NamespaceInjectionMiddleware(),
            quota_enforcer=enforcer,
        )
        with pytest.raises(RuntimeError):
            await tool.execute({"content": "hello"}, ctx)
        # Quota should remain at 0 — write never succeeded
        assert enforcer.get_info(CLIENT_ID).used == 0

    async def test_quota_check_before_write(
        self, ctx: APIKeyContext
    ) -> None:
        """Backend should never be called when quota is already exceeded."""
        never_called = AsyncMock()
        enforcer = QuotaEnforcer(default_limit=1)
        enforcer.increment(CLIENT_ID)  # at limit
        tool = MemoryStoreTool(
            memory_store_fn=never_called,
            namespace_middleware=NamespaceInjectionMiddleware(),
            quota_enforcer=enforcer,
        )
        with pytest.raises(QuotaExceededError):
            await tool.execute({"content": "blocked"}, ctx)
        never_called.assert_not_called()


# ── Validation errors ─────────────────────────────────────────────────────────


class TestValidationErrors:
    async def test_missing_content_raises(self, tool: MemoryStoreTool, ctx: APIKeyContext) -> None:
        with pytest.raises(ValidationError, match="content"):
            await tool.execute({}, ctx)

    async def test_empty_content_raises(self, tool: MemoryStoreTool, ctx: APIKeyContext) -> None:
        with pytest.raises(ValidationError, match="content"):
            await tool.execute({"content": ""}, ctx)

    async def test_layer_all_raises(self, tool: MemoryStoreTool, ctx: APIKeyContext) -> None:
        with pytest.raises(ValidationError, match="layer"):
            await tool.execute({"content": "hi", "layer": "all"}, ctx)

    async def test_too_many_tags_raises(self, tool: MemoryStoreTool, ctx: APIKeyContext) -> None:
        tags = [f"tag{i}" for i in range(11)]
        with pytest.raises(ValidationError, match="tags"):
            await tool.execute({"content": "hi", "tags": tags}, ctx)

    async def test_ttl_days_zero_raises(self, tool: MemoryStoreTool, ctx: APIKeyContext) -> None:
        with pytest.raises(ValidationError, match="ttl_days"):
            await tool.execute({"content": "hi", "ttl_days": 0}, ctx)

    async def test_content_too_long_raises(
        self, tool: MemoryStoreTool, ctx: APIKeyContext
    ) -> None:
        with pytest.raises(ValidationError, match="content"):
            await tool.execute({"content": "x" * 4097}, ctx)
