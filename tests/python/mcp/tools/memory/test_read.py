"""Tests for MemoryReadTool.

Acceptance criteria (from mcp-memory-endpoints-design.md §6.5):
  ✅ Read own memory → returns full record
  ✅ Non-existent memory_id → 404
  ✅ Invalid memory_id format (non-UUID) → 422
  ✅ Cross-namespace access → 404 (not 403, no info leakage)
  ✅ internal/ namespace access → 404
"""

from __future__ import annotations

from unittest.mock import AsyncMock

import pytest

from duduclaw.mcp.auth.types import APIKeyContext
from duduclaw.mcp.errors import NotFoundError, ValidationError
from duduclaw.mcp.tools.memory.namespace import NamespaceInjectionMiddleware
from duduclaw.mcp.tools.memory.read import MemoryReadTool

# ── Constants ─────────────────────────────────────────────────────────────────

CLIENT_ID = "readclient_1"
VALID_UUID = "550e8400-e29b-41d4-a716-446655440000"


# ── Fixtures ──────────────────────────────────────────────────────────────────


@pytest.fixture
def ctx() -> APIKeyContext:
    return APIKeyContext(
        api_key_prefix="ddc_prod",
        client_id=CLIENT_ID,
        scopes=frozenset({"memory:read"}),
        trust_level=1,
    )


def _make_own_record(client_id: str = CLIENT_ID) -> dict:
    return {
        "id": VALID_UUID,
        "content": "my memory content",
        "layer": "episodic",
        "namespace": f"external/{client_id}",
        "tags": ["test", "w19"],
        "created_at": "2026-04-29T00:00:00Z",
        "updated_at": "2026-04-29T00:00:00Z",
        "ttl_expires_at": None,
    }


@pytest.fixture
def tool_with_record() -> MemoryReadTool:
    mock_fn = AsyncMock(return_value=_make_own_record())
    return MemoryReadTool(
        memory_read_fn=mock_fn,
        namespace_middleware=NamespaceInjectionMiddleware(),
    )


# ── Successful reads ──────────────────────────────────────────────────────────


class TestSuccessfulRead:
    async def test_returns_memory_id(
        self, tool_with_record: MemoryReadTool, ctx: APIKeyContext
    ) -> None:
        result = await tool_with_record.execute({"memory_id": VALID_UUID}, ctx)
        assert result["id"] == VALID_UUID

    async def test_returns_content(
        self, tool_with_record: MemoryReadTool, ctx: APIKeyContext
    ) -> None:
        result = await tool_with_record.execute({"memory_id": VALID_UUID}, ctx)
        assert result["content"] == "my memory content"

    async def test_returns_layer(
        self, tool_with_record: MemoryReadTool, ctx: APIKeyContext
    ) -> None:
        result = await tool_with_record.execute({"memory_id": VALID_UUID}, ctx)
        assert result["layer"] == "episodic"

    async def test_returns_namespace(
        self, tool_with_record: MemoryReadTool, ctx: APIKeyContext
    ) -> None:
        result = await tool_with_record.execute({"memory_id": VALID_UUID}, ctx)
        assert result["namespace"] == f"external/{CLIENT_ID}"

    async def test_returns_tags(
        self, tool_with_record: MemoryReadTool, ctx: APIKeyContext
    ) -> None:
        result = await tool_with_record.execute({"memory_id": VALID_UUID}, ctx)
        assert result["tags"] == ["test", "w19"]

    async def test_returns_timestamps(
        self, tool_with_record: MemoryReadTool, ctx: APIKeyContext
    ) -> None:
        result = await tool_with_record.execute({"memory_id": VALID_UUID}, ctx)
        assert result["created_at"] == "2026-04-29T00:00:00Z"
        assert result["updated_at"] == "2026-04-29T00:00:00Z"

    async def test_returns_ttl_expires_at_none(
        self, tool_with_record: MemoryReadTool, ctx: APIKeyContext
    ) -> None:
        result = await tool_with_record.execute({"memory_id": VALID_UUID}, ctx)
        assert result["ttl_expires_at"] is None

    async def test_memory_id_passed_to_backend(
        self, ctx: APIKeyContext
    ) -> None:
        mock_fn = AsyncMock(return_value=_make_own_record())
        tool = MemoryReadTool(
            memory_read_fn=mock_fn,
            namespace_middleware=NamespaceInjectionMiddleware(),
        )
        await tool.execute({"memory_id": VALID_UUID}, ctx)
        mock_fn.assert_called_once_with(memory_id=VALID_UUID)


# ── Not found ─────────────────────────────────────────────────────────────────


class TestNotFound:
    async def test_not_found_raises_404(self, ctx: APIKeyContext) -> None:
        tool = MemoryReadTool(
            memory_read_fn=AsyncMock(return_value=None),
            namespace_middleware=NamespaceInjectionMiddleware(),
        )
        with pytest.raises(NotFoundError) as exc_info:
            await tool.execute({"memory_id": VALID_UUID}, ctx)
        assert exc_info.value.http_status == 404


# ── Namespace security ────────────────────────────────────────────────────────


class TestNamespaceSecurity:
    async def test_cross_namespace_returns_404_not_403(self, ctx: APIKeyContext) -> None:
        """Cross-namespace access must return 404 to avoid info leakage (not 403)."""
        other_record = {
            **_make_own_record(),
            "namespace": "external/other-client-xyz",
        }
        tool = MemoryReadTool(
            memory_read_fn=AsyncMock(return_value=other_record),
            namespace_middleware=NamespaceInjectionMiddleware(),
        )
        with pytest.raises(NotFoundError) as exc_info:
            await tool.execute({"memory_id": VALID_UUID}, ctx)
        assert exc_info.value.http_status == 404
        assert exc_info.value.code == "not_found"

    async def test_internal_namespace_returns_404(self, ctx: APIKeyContext) -> None:
        """Attempting to read internal/ memories returns 404."""
        internal_record = {
            **_make_own_record(),
            "namespace": "internal/duduclaw-tl",
        }
        tool = MemoryReadTool(
            memory_read_fn=AsyncMock(return_value=internal_record),
            namespace_middleware=NamespaceInjectionMiddleware(),
        )
        with pytest.raises(NotFoundError):
            await tool.execute({"memory_id": VALID_UUID}, ctx)

    async def test_empty_namespace_returns_404(self, ctx: APIKeyContext) -> None:
        """Records with empty namespace return 404."""
        bad_record = {**_make_own_record(), "namespace": ""}
        tool = MemoryReadTool(
            memory_read_fn=AsyncMock(return_value=bad_record),
            namespace_middleware=NamespaceInjectionMiddleware(),
        )
        with pytest.raises(NotFoundError):
            await tool.execute({"memory_id": VALID_UUID}, ctx)

    async def test_different_clients_cannot_share_reads(self) -> None:
        """Two clients with different IDs cannot read each other's memories."""
        ctx_a = APIKeyContext(
            api_key_prefix="ddc_prod",
            client_id="client_aaa",
            scopes=frozenset({"memory:read"}),
            trust_level=1,
        )
        # Record belongs to client_bbb
        record_for_b = {**_make_own_record("client_bbb")}
        tool = MemoryReadTool(
            memory_read_fn=AsyncMock(return_value=record_for_b),
            namespace_middleware=NamespaceInjectionMiddleware(),
        )
        # ctx_a attempts to read client_bbb's record
        with pytest.raises(NotFoundError):
            await tool.execute({"memory_id": VALID_UUID}, ctx_a)


# ── Validation errors ─────────────────────────────────────────────────────────


class TestValidationErrors:
    async def test_missing_memory_id_raises(
        self, tool_with_record: MemoryReadTool, ctx: APIKeyContext
    ) -> None:
        with pytest.raises(ValidationError, match="memory_id"):
            await tool_with_record.execute({}, ctx)

    async def test_invalid_uuid_raises(
        self, tool_with_record: MemoryReadTool, ctx: APIKeyContext
    ) -> None:
        with pytest.raises(ValidationError, match="UUID"):
            await tool_with_record.execute({"memory_id": "not-a-uuid"}, ctx)

    async def test_empty_memory_id_raises(
        self, tool_with_record: MemoryReadTool, ctx: APIKeyContext
    ) -> None:
        with pytest.raises(ValidationError, match="memory_id"):
            await tool_with_record.execute({"memory_id": ""}, ctx)

    async def test_uuid_v1_raises(
        self, tool_with_record: MemoryReadTool, ctx: APIKeyContext
    ) -> None:
        uuid_v1 = "6ba7b810-9dad-11d1-80b4-00c04fd430c8"
        with pytest.raises(ValidationError, match="UUID"):
            await tool_with_record.execute({"memory_id": uuid_v1}, ctx)
