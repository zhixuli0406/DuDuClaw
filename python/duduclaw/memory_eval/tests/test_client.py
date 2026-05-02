"""
tests/test_client.py
client.py — MemoryClient 介面、HttpMemoryClient、build_client 的 unit tests

覆蓋目標：client.py ≥ 80%
使用 aioresponses 模擬 HTTP 呼叫（不發送真實網路請求）

W21 Sprint 實作 — ENG-MEMORY
"""
from __future__ import annotations

import os
import pytest
from unittest.mock import patch, AsyncMock, MagicMock

import aiohttp
from aioresponses import aioresponses as AioResponses

from duduclaw.memory_eval.client import (
    HttpMemoryClient,
    Memory,
    MemoryClient,
    SearchResult,
    build_client,
)
from duduclaw.memory_eval.config import EvalConfig


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------

@pytest.fixture
def http_client() -> HttpMemoryClient:
    """標準 HttpMemoryClient 實例（不需真實 API）"""
    return HttpMemoryClient(
        base_url="http://localhost:8080",
        api_key="test-api-key",
        agent_id="test-agent",
    )


@pytest.fixture
def eval_config() -> EvalConfig:
    return EvalConfig(agent_id="test-agent")


# ---------------------------------------------------------------------------
# MemoryClient 抽象介面驗證
# ---------------------------------------------------------------------------

def test_memory_client_is_abstract():
    """MemoryClient 為抽象類別，不可直接實例化"""
    with pytest.raises(TypeError, match="Can't instantiate abstract class"):
        MemoryClient()  # type: ignore


def test_memory_dataclass_fields():
    """Memory dataclass 欄位驗證"""
    mem = Memory(
        memory_id="mem-001",
        content="用戶喜歡拉麵",
        summary="飲食偏好",
        importance_score=0.85,
        layer="episodic",
    )
    assert mem.memory_id == "mem-001"
    assert mem.content == "用戶喜歡拉麵"
    assert mem.importance_score == 0.85
    assert mem.layer == "episodic"
    assert mem.entity_type is None
    assert mem.valid_until is None


def test_search_result_dataclass_fields():
    """SearchResult dataclass 欄位驗證"""
    result = SearchResult(
        memory_id="mem-001",
        content="用戶喜歡拉麵",
        similarity=0.92,
    )
    assert result.memory_id == "mem-001"
    assert result.similarity == 0.92


def test_memory_dataclass_optional_fields():
    """Memory dataclass 可選欄位有預設值"""
    mem = Memory(
        memory_id="m-1",
        content="test",
        summary=None,
        importance_score=0.5,
        layer="semantic",
        entity_type="person",
        entity_id="user-123",
        attribute="preference",
        valid_until="2027-01-01T00:00:00Z",
        created_at="2026-05-01T00:00:00Z",
    )
    assert mem.entity_type == "person"
    assert mem.entity_id == "user-123"
    assert mem.attribute == "preference"
    assert mem.valid_until == "2027-01-01T00:00:00Z"
    assert mem.created_at == "2026-05-01T00:00:00Z"


# ---------------------------------------------------------------------------
# HttpMemoryClient — search()
# ---------------------------------------------------------------------------

@pytest.mark.asyncio
async def test_http_client_search_success(http_client):
    """search() 正常呼叫 → 回傳 SearchResult 列表"""
    with AioResponses() as m:
        m.post(
            "http://localhost:8080/memory/search",
            payload={
                "memories": [
                    {"memory_id": "mem-001", "content": "用戶喜歡拉麵", "similarity": 0.93},
                    {"memory_id": "mem-002", "content": "用戶喜歡吃辣", "similarity": 0.87},
                ]
            },
            status=200,
        )
        results = await http_client.search("用戶喜歡吃什麼", limit=5)

    assert len(results) == 2
    assert results[0].memory_id == "mem-001"
    assert results[0].similarity == 0.93
    assert results[1].memory_id == "mem-002"


@pytest.mark.asyncio
async def test_http_client_search_empty_results(http_client):
    """search() 無結果 → 回傳空列表"""
    with AioResponses() as m:
        m.post(
            "http://localhost:8080/memory/search",
            payload={"memories": []},
            status=200,
        )
        results = await http_client.search("完全不相關的查詢", limit=5)

    assert results == []


@pytest.mark.asyncio
async def test_http_client_search_with_namespace(http_client):
    """search() 帶 namespace 參數 → payload 含 namespace"""
    with AioResponses() as m:
        m.post(
            "http://localhost:8080/memory/search",
            payload={"memories": [{"memory_id": "mem-ns-001", "content": "ns memory", "similarity": 0.80}]},
            status=200,
        )
        results = await http_client.search("test query", limit=3, namespace="test-ns")

    assert len(results) == 1
    assert results[0].memory_id == "mem-ns-001"


@pytest.mark.asyncio
async def test_http_client_search_missing_similarity(http_client):
    """search() 回應缺少 similarity 欄位 → 使用預設值 0.0"""
    with AioResponses() as m:
        m.post(
            "http://localhost:8080/memory/search",
            payload={"memories": [{"memory_id": "mem-001", "content": "no similarity"}]},
            status=200,
        )
        results = await http_client.search("test", limit=5)

    assert results[0].similarity == 0.0


@pytest.mark.asyncio
async def test_http_client_search_http_error(http_client):
    """search() HTTP 500 → 拋出 ClientResponseError"""
    with AioResponses() as m:
        m.post(
            "http://localhost:8080/memory/search",
            status=500,
            reason="Internal Server Error",
        )
        with pytest.raises(aiohttp.ClientResponseError):
            await http_client.search("test", limit=5)


@pytest.mark.asyncio
async def test_http_client_search_unauthorized(http_client):
    """search() HTTP 401 → 拋出 ClientResponseError"""
    with AioResponses() as m:
        m.post(
            "http://localhost:8080/memory/search",
            status=401,
            reason="Unauthorized",
        )
        with pytest.raises(aiohttp.ClientResponseError):
            await http_client.search("test", limit=5)


# ---------------------------------------------------------------------------
# HttpMemoryClient — store()
# ---------------------------------------------------------------------------

@pytest.mark.asyncio
async def test_http_client_store_success(http_client):
    """store() 正常呼叫 → 回傳 memory_id"""
    with AioResponses() as m:
        m.post(
            "http://localhost:8080/memory/store",
            payload={"id": "mem-new-001"},
            status=200,
        )
        memory_id = await http_client.store("用戶喜歡拉麵")

    assert memory_id == "mem-new-001"


@pytest.mark.asyncio
async def test_http_client_store_with_tags(http_client):
    """store() 帶 tags 參數 → 正常呼叫"""
    with AioResponses() as m:
        m.post(
            "http://localhost:8080/memory/store",
            payload={"id": "mem-tag-001"},
            status=200,
        )
        memory_id = await http_client.store(
            "用戶喜歡跑步",
            tags=["exercise", "habit"],
        )

    assert memory_id == "mem-tag-001"


@pytest.mark.asyncio
async def test_http_client_store_with_namespace(http_client):
    """store() 帶 namespace 參數 → 正常呼叫"""
    with AioResponses() as m:
        m.post(
            "http://localhost:8080/memory/store",
            payload={"id": "mem-ns-001"},
            status=200,
        )
        memory_id = await http_client.store(
            "test content",
            namespace="private-ns",
        )

    assert memory_id == "mem-ns-001"


@pytest.mark.asyncio
async def test_http_client_store_http_error(http_client):
    """store() HTTP 400 → 拋出 ClientResponseError"""
    with AioResponses() as m:
        m.post(
            "http://localhost:8080/memory/store",
            status=400,
            reason="Bad Request",
        )
        with pytest.raises(aiohttp.ClientResponseError):
            await http_client.store("bad content")


# ---------------------------------------------------------------------------
# HttpMemoryClient — get_episodic_pressure()
# ---------------------------------------------------------------------------

@pytest.mark.asyncio
async def test_http_client_episodic_pressure_success(http_client):
    """get_episodic_pressure() 正常呼叫 → 回傳 float"""
    with AioResponses() as m:
        m.get(
            "http://localhost:8080/memory/episodic-pressure?hours_ago=24",
            payload={"pressure": 3.5},
            status=200,
        )
        pressure = await http_client.get_episodic_pressure(hours_ago=24)

    assert pressure == 3.5
    assert isinstance(pressure, float)


@pytest.mark.asyncio
async def test_http_client_episodic_pressure_custom_hours(http_client):
    """get_episodic_pressure() 自訂 hours_ago → 正常呼叫"""
    with AioResponses() as m:
        m.get(
            "http://localhost:8080/memory/episodic-pressure?hours_ago=48",
            payload={"pressure": 7.2},
            status=200,
        )
        pressure = await http_client.get_episodic_pressure(hours_ago=48)

    assert abs(pressure - 7.2) < 0.001


@pytest.mark.asyncio
async def test_http_client_episodic_pressure_zero(http_client):
    """get_episodic_pressure() 回傳 0 → float(0)"""
    with AioResponses() as m:
        m.get(
            "http://localhost:8080/memory/episodic-pressure?hours_ago=24",
            payload={"pressure": 0},
            status=200,
        )
        pressure = await http_client.get_episodic_pressure()

    assert pressure == 0.0


@pytest.mark.asyncio
async def test_http_client_episodic_pressure_http_error(http_client):
    """get_episodic_pressure() HTTP 503 → 拋出 ClientResponseError"""
    with AioResponses() as m:
        m.get(
            "http://localhost:8080/memory/episodic-pressure?hours_ago=24",
            status=503,
            reason="Service Unavailable",
        )
        with pytest.raises(aiohttp.ClientResponseError):
            await http_client.get_episodic_pressure()


# ---------------------------------------------------------------------------
# HttpMemoryClient — NotImplemented 方法
# ---------------------------------------------------------------------------

@pytest.mark.asyncio
async def test_http_client_list_important_not_implemented(http_client):
    """list_important() 應拋出 NotImplementedError"""
    with pytest.raises(NotImplementedError, match="direct DB access"):
        await http_client.list_important("test-agent")


@pytest.mark.asyncio
async def test_http_client_list_active_not_implemented(http_client):
    """list_active() 應拋出 NotImplementedError"""
    with pytest.raises(NotImplementedError, match="direct DB access"):
        await http_client.list_active("test-agent")


@pytest.mark.asyncio
async def test_http_client_get_by_ids_not_implemented(http_client):
    """get_by_ids() 應拋出 NotImplementedError"""
    with pytest.raises(NotImplementedError, match="direct DB access"):
        await http_client.get_by_ids(["mem-001", "mem-002"])


# ---------------------------------------------------------------------------
# HttpMemoryClient — base_url 末尾斜線處理
# ---------------------------------------------------------------------------

@pytest.mark.asyncio
async def test_http_client_base_url_trailing_slash():
    """base_url 末尾有斜線 → 不會重複斜線"""
    client = HttpMemoryClient(
        base_url="http://localhost:8080/",
        api_key="key",
        agent_id="agent",
    )
    assert client._base_url == "http://localhost:8080"


# ---------------------------------------------------------------------------
# build_client()
# ---------------------------------------------------------------------------

def test_build_client_missing_api_url(eval_config):
    """DUDUCLAW_API_URL 未設定 → RuntimeError"""
    with patch.dict(os.environ, {}, clear=True):
        # Ensure env vars are not set
        os.environ.pop("DUDUCLAW_API_URL", None)
        os.environ.pop("DUDUCLAW_API_KEY", None)
        with pytest.raises(RuntimeError, match="DUDUCLAW_API_URL is not set"):
            build_client(eval_config)


def test_build_client_missing_api_key(eval_config):
    """DUDUCLAW_API_URL 有值但 DUDUCLAW_API_KEY 未設定 → RuntimeError"""
    with patch.dict(os.environ, {"DUDUCLAW_API_URL": "http://localhost:8080"}, clear=False):
        os.environ.pop("DUDUCLAW_API_KEY", None)
        with pytest.raises(RuntimeError, match="DUDUCLAW_API_KEY is not set"):
            build_client(eval_config)


def test_build_client_success(eval_config):
    """兩個環境變數都設定 → 回傳 HttpMemoryClient"""
    with patch.dict(
        os.environ,
        {
            "DUDUCLAW_API_URL": "http://localhost:8080",
            "DUDUCLAW_API_KEY": "test-key-123",
        },
    ):
        client = build_client(eval_config)

    assert isinstance(client, HttpMemoryClient)
    assert client._base_url == "http://localhost:8080"
    assert client._api_key == "test-key-123"
    assert client._agent_id == "test-agent"
