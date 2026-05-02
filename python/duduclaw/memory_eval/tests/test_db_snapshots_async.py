"""
tests/test_db_snapshots_async.py
db/snapshots.py — asyncpg 函式的 unit tests（使用 MagicMock 模擬 DB）

覆蓋目標：db/snapshots.py ≥ 80%
不需要真實 PostgreSQL 連線

W21 Sprint 實作 — ENG-MEMORY
"""
from __future__ import annotations

import hashlib
from datetime import date
from unittest.mock import AsyncMock, MagicMock, patch, call

import pytest

from duduclaw.memory_eval.client import Memory
from duduclaw.memory_eval.db.snapshots import (
    _sha256_hash,
    cleanup_old_snapshots,
    get_snapshot_stats,
    take_memory_snapshot,
)


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def make_memory(
    memory_id: str = "mem-001",
    content: str = "用戶喜歡拉麵",
    importance_score: float = 0.85,
    layer: str = "episodic",
    summary: str | None = None,
) -> Memory:
    return Memory(
        memory_id=memory_id,
        content=content,
        summary=summary,
        importance_score=importance_score,
        layer=layer,
    )


def build_mock_pool(execute_result: str = "INSERT 0 1") -> MagicMock:
    """建立完整的 asyncpg Pool mock（支援 async with conn.transaction()）"""
    mock_conn = AsyncMock()
    mock_conn.execute = AsyncMock(return_value=execute_result)

    # transaction context manager
    mock_txn = AsyncMock()
    mock_txn.__aenter__ = AsyncMock(return_value=None)
    mock_txn.__aexit__ = AsyncMock(return_value=False)
    mock_conn.transaction = MagicMock(return_value=mock_txn)

    # pool.acquire() context manager
    mock_acquire = AsyncMock()
    mock_acquire.__aenter__ = AsyncMock(return_value=mock_conn)
    mock_acquire.__aexit__ = AsyncMock(return_value=False)

    mock_pool = MagicMock()
    mock_pool.acquire = MagicMock(return_value=mock_acquire)

    return mock_pool, mock_conn


def build_mock_pool_with_fetchrow(row: dict | None) -> tuple:
    """建立支援 fetchrow 的 asyncpg Pool mock"""
    mock_conn = AsyncMock()

    if row is not None:
        mock_row = MagicMock()
        mock_row.__getitem__ = lambda self, key: row[key]
        mock_conn.fetchrow = AsyncMock(return_value=mock_row)
    else:
        mock_conn.fetchrow = AsyncMock(return_value=None)

    mock_acquire = AsyncMock()
    mock_acquire.__aenter__ = AsyncMock(return_value=mock_conn)
    mock_acquire.__aexit__ = AsyncMock(return_value=False)

    mock_pool = MagicMock()
    mock_pool.acquire = MagicMock(return_value=mock_acquire)

    return mock_pool, mock_conn


# ---------------------------------------------------------------------------
# take_memory_snapshot tests
# ---------------------------------------------------------------------------

@pytest.mark.asyncio
async def test_take_snapshot_inserts_memories():
    """正常情況：插入多筆記憶，回傳正確統計"""
    mock_pool, mock_conn = build_mock_pool(execute_result="INSERT 0 1")

    mock_client = AsyncMock()
    mock_client.list_important = AsyncMock(return_value=[
        make_memory("mem-001", "用戶喜歡拉麵"),
        make_memory("mem-002", "用戶在 Google 工作"),
        make_memory("mem-003", "用戶養了一隻狗"),
    ])

    result = await take_memory_snapshot(
        db_pool=mock_pool,
        memory_client=mock_client,
        agent_id="test-agent",
    )

    assert result["inserted"] == 3
    assert result["total_fetched"] == 3
    assert "batch_id" in result
    assert "snapshot_date" in result
    # execute 應被呼叫 3 次（每筆記憶一次）
    assert mock_conn.execute.call_count == 3


@pytest.mark.asyncio
async def test_take_snapshot_no_memories():
    """無高重要性記憶 → inserted=0，不執行 DB 寫入"""
    mock_pool, mock_conn = build_mock_pool()

    mock_client = AsyncMock()
    mock_client.list_important = AsyncMock(return_value=[])

    result = await take_memory_snapshot(
        db_pool=mock_pool,
        memory_client=mock_client,
        agent_id="empty-agent",
    )

    assert result["inserted"] == 0
    assert "batch_id" in result
    # 無記憶 → 不需要 acquire DB conn
    mock_conn.execute.assert_not_called()


@pytest.mark.asyncio
async def test_take_snapshot_conflict_returns_zero():
    """ON CONFLICT 衝突（回傳 INSERT 0 0）→ inserted 不增加"""
    mock_pool, mock_conn = build_mock_pool(execute_result="INSERT 0 0")

    mock_client = AsyncMock()
    mock_client.list_important = AsyncMock(return_value=[
        make_memory("mem-001"),
        make_memory("mem-002"),
    ])

    result = await take_memory_snapshot(
        db_pool=mock_pool,
        memory_client=mock_client,
        agent_id="test-agent",
    )

    # INSERT 0 0 表示衝突，不計入 inserted
    assert result["inserted"] == 0
    assert result["total_fetched"] == 2


@pytest.mark.asyncio
async def test_take_snapshot_uses_summary_if_present():
    """有 summary 的記憶 → 使用 summary（截斷至 200 字）"""
    mock_pool, mock_conn = build_mock_pool(execute_result="INSERT 0 1")

    long_summary = "A" * 250  # 超過 200 字
    mock_client = AsyncMock()
    mock_client.list_important = AsyncMock(return_value=[
        make_memory("mem-001", summary=long_summary),
    ])

    await take_memory_snapshot(
        db_pool=mock_pool,
        memory_client=mock_client,
        agent_id="test-agent",
    )

    # 確認 execute 被呼叫（summary 截斷由實作內部處理）
    assert mock_conn.execute.call_count == 1
    call_args = mock_conn.execute.call_args
    # 第 7 個參數（index 6）應是 summary（截斷後 ≤ 200）
    summary_arg = call_args.args[7]
    assert len(summary_arg) <= 200


@pytest.mark.asyncio
async def test_take_snapshot_custom_source():
    """自訂 snapshot_source 正確傳入 DB"""
    mock_pool, mock_conn = build_mock_pool(execute_result="INSERT 0 1")

    mock_client = AsyncMock()
    mock_client.list_important = AsyncMock(return_value=[
        make_memory("mem-001"),
    ])

    await take_memory_snapshot(
        db_pool=mock_pool,
        memory_client=mock_client,
        agent_id="test-agent",
        snapshot_source="pre_locomo_eval",
    )

    call_args = mock_conn.execute.call_args
    # 最後一個參數應是 snapshot_source
    snapshot_source_arg = call_args.args[-1]
    assert snapshot_source_arg == "pre_locomo_eval"


@pytest.mark.asyncio
async def test_take_snapshot_content_hash_generated():
    """快照應包含 content_hash（SHA256）"""
    mock_pool, mock_conn = build_mock_pool(execute_result="INSERT 0 1")

    content = "用戶喜歡拉麵"
    expected_hash = hashlib.sha256(content.encode("utf-8")).hexdigest()

    mock_client = AsyncMock()
    mock_client.list_important = AsyncMock(return_value=[
        make_memory("mem-001", content=content),
    ])

    await take_memory_snapshot(
        db_pool=mock_pool,
        memory_client=mock_client,
        agent_id="test-agent",
    )

    call_args = mock_conn.execute.call_args
    # content_hash 是第 5 個參數（index 4）
    content_hash_arg = call_args.args[5]
    assert content_hash_arg == expected_hash


@pytest.mark.asyncio
async def test_take_snapshot_uses_content_when_no_summary():
    """無 summary 時使用 content[:200] 作為 summary"""
    mock_pool, mock_conn = build_mock_pool(execute_result="INSERT 0 1")

    content = "B" * 300
    mock_client = AsyncMock()
    mock_client.list_important = AsyncMock(return_value=[
        make_memory("mem-001", content=content, summary=None),
    ])

    await take_memory_snapshot(
        db_pool=mock_pool,
        memory_client=mock_client,
        agent_id="test-agent",
    )

    call_args = mock_conn.execute.call_args
    summary_arg = call_args.args[7]
    assert len(summary_arg) <= 200


@pytest.mark.asyncio
async def test_take_snapshot_returns_batch_id_uuid_format():
    """batch_id 應為有效 UUID 格式"""
    import re
    UUID_RE = re.compile(
        r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$"
    )

    mock_pool, _ = build_mock_pool()
    mock_client = AsyncMock()
    mock_client.list_important = AsyncMock(return_value=[])

    result = await take_memory_snapshot(
        db_pool=mock_pool,
        memory_client=mock_client,
        agent_id="test-agent",
    )

    assert UUID_RE.match(result["batch_id"]), f"Invalid UUID: {result['batch_id']}"


# ---------------------------------------------------------------------------
# cleanup_old_snapshots tests
# ---------------------------------------------------------------------------

@pytest.mark.asyncio
async def test_cleanup_old_snapshots_returns_deleted_count():
    """cleanup_old_snapshots 正常執行 → 回傳刪除筆數"""
    mock_conn = AsyncMock()
    mock_conn.execute = AsyncMock(return_value="DELETE 42")

    mock_acquire = AsyncMock()
    mock_acquire.__aenter__ = AsyncMock(return_value=mock_conn)
    mock_acquire.__aexit__ = AsyncMock(return_value=False)

    mock_pool = MagicMock()
    mock_pool.acquire = MagicMock(return_value=mock_acquire)

    deleted = await cleanup_old_snapshots(mock_pool, retention_days=90)

    assert deleted == 42
    mock_conn.execute.assert_called_once()


@pytest.mark.asyncio
async def test_cleanup_old_snapshots_zero_deleted():
    """無符合條件的記錄 → 回傳 0"""
    mock_conn = AsyncMock()
    mock_conn.execute = AsyncMock(return_value="DELETE 0")

    mock_acquire = AsyncMock()
    mock_acquire.__aenter__ = AsyncMock(return_value=mock_conn)
    mock_acquire.__aexit__ = AsyncMock(return_value=False)

    mock_pool = MagicMock()
    mock_pool.acquire = MagicMock(return_value=mock_acquire)

    deleted = await cleanup_old_snapshots(mock_pool, retention_days=90)

    assert deleted == 0


@pytest.mark.asyncio
async def test_cleanup_old_snapshots_custom_retention():
    """自訂 retention_days → DB execute 被呼叫一次"""
    mock_conn = AsyncMock()
    mock_conn.execute = AsyncMock(return_value="DELETE 10")

    mock_acquire = AsyncMock()
    mock_acquire.__aenter__ = AsyncMock(return_value=mock_conn)
    mock_acquire.__aexit__ = AsyncMock(return_value=False)

    mock_pool = MagicMock()
    mock_pool.acquire = MagicMock(return_value=mock_acquire)

    deleted = await cleanup_old_snapshots(mock_pool, retention_days=30)

    assert deleted == 10
    mock_conn.execute.assert_called_once()


@pytest.mark.asyncio
async def test_cleanup_unexpected_result_format():
    """DB 回傳非標準格式 → 回傳 0（不崩潰）"""
    mock_conn = AsyncMock()
    mock_conn.execute = AsyncMock(return_value="UNEXPECTED")

    mock_acquire = AsyncMock()
    mock_acquire.__aenter__ = AsyncMock(return_value=mock_conn)
    mock_acquire.__aexit__ = AsyncMock(return_value=False)

    mock_pool = MagicMock()
    mock_pool.acquire = MagicMock(return_value=mock_acquire)

    deleted = await cleanup_old_snapshots(mock_pool, retention_days=90)

    assert deleted == 0


# ---------------------------------------------------------------------------
# get_snapshot_stats tests
# ---------------------------------------------------------------------------

@pytest.mark.asyncio
async def test_get_snapshot_stats_with_data():
    """get_snapshot_stats 正常返回統計"""
    mock_pool, mock_conn = build_mock_pool_with_fetchrow({
        "latest_date": date(2026, 5, 1),
        "total_records": 250,
        "distinct_dates": 4,
    })

    stats = await get_snapshot_stats(mock_pool, agent_id="test-agent")

    assert stats["latest_date"] == "2026-05-01"
    assert stats["total_records"] == 250
    assert stats["distinct_dates"] == 4


@pytest.mark.asyncio
async def test_get_snapshot_stats_no_data():
    """無快照記錄 → latest_date 為 None"""
    mock_pool, mock_conn = build_mock_pool_with_fetchrow({
        "latest_date": None,
        "total_records": 0,
        "distinct_dates": 0,
    })

    stats = await get_snapshot_stats(mock_pool, agent_id="empty-agent")

    assert stats["latest_date"] is None
    assert stats["total_records"] == 0
    assert stats["distinct_dates"] == 0


@pytest.mark.asyncio
async def test_get_snapshot_stats_calls_fetchrow_once():
    """get_snapshot_stats 只發出一次 DB 查詢"""
    mock_pool, mock_conn = build_mock_pool_with_fetchrow({
        "latest_date": date(2026, 5, 1),
        "total_records": 10,
        "distinct_dates": 1,
    })

    await get_snapshot_stats(mock_pool, agent_id="test-agent")

    mock_conn.fetchrow.assert_called_once()
    # 確認 agent_id 正確傳入
    call_args = mock_conn.fetchrow.call_args
    assert "test-agent" in call_args.args
