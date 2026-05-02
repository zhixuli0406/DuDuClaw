"""
tests/test_consolidation.py
db/consolidation.py unit tests — mock asyncpg.Pool

目標：consolidation.py 覆蓋率從 0% 提升至 ≥ 70%

W21 Sprint — ENG-MEMORY
"""
from __future__ import annotations

import uuid
from datetime import datetime, timezone
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

# ---------------------------------------------------------------------------
# helpers
# ---------------------------------------------------------------------------

def _make_mock_pool(execute_return=None, fetch_return=None):
    """建立 asyncpg.Pool mock，支援 async with pool.acquire() as conn"""
    mock_conn = AsyncMock()
    mock_conn.execute = AsyncMock(return_value=execute_return)
    mock_conn.fetch   = AsyncMock(return_value=fetch_return or [])

    mock_ctx = AsyncMock()
    mock_ctx.__aenter__ = AsyncMock(return_value=mock_conn)
    mock_ctx.__aexit__  = AsyncMock(return_value=False)

    mock_pool = MagicMock()
    mock_pool.acquire = MagicMock(return_value=mock_ctx)
    return mock_pool, mock_conn


# ---------------------------------------------------------------------------
# log_consolidation_start
# ---------------------------------------------------------------------------

@pytest.mark.asyncio
async def test_log_consolidation_start_returns_uuid():
    """log_consolidation_start 回傳 UUID 字串"""
    from duduclaw.memory_eval.db.consolidation import log_consolidation_start

    mock_pool, mock_conn = _make_mock_pool()

    result = await log_consolidation_start(
        db_pool=mock_pool,
        agent_id="test-agent",
        trigger_reason="episodic_pressure_threshold",
        pressure_before=0.85,
        source_memory_ids=["mem-001", "mem-002"],
    )

    # 應為合法 UUID
    parsed = uuid.UUID(result)
    assert str(parsed) == result


@pytest.mark.asyncio
async def test_log_consolidation_start_executes_insert():
    """log_consolidation_start 應呼叫 conn.execute"""
    from duduclaw.memory_eval.db.consolidation import log_consolidation_start

    mock_pool, mock_conn = _make_mock_pool()

    await log_consolidation_start(
        db_pool=mock_pool,
        agent_id="test-agent",
        trigger_reason="manual",
        pressure_before=0.50,
        source_memory_ids=["mem-001"],
        algorithm_version="v1.1",
    )

    mock_conn.execute.assert_called_once()
    call_args = mock_conn.execute.call_args[0]
    sql = call_args[0]
    assert "INSERT INTO memory_consolidation_log" in sql
    assert "running" in sql


@pytest.mark.asyncio
async def test_log_consolidation_start_default_algorithm_version():
    """不傳 algorithm_version 時使用預設 'v1.0'"""
    from duduclaw.memory_eval.db.consolidation import log_consolidation_start

    mock_pool, mock_conn = _make_mock_pool()

    await log_consolidation_start(
        db_pool=mock_pool,
        agent_id="agent",
        trigger_reason="scheduled",
        pressure_before=0.10,
        source_memory_ids=[],
    )

    # 驗證 v1.0 被傳入（最後一個參數）
    call_args = mock_conn.execute.call_args[0]
    assert "v1.0" in call_args


@pytest.mark.asyncio
async def test_log_consolidation_start_unique_ids():
    """連續兩次呼叫應回傳不同 UUID"""
    from duduclaw.memory_eval.db.consolidation import log_consolidation_start

    mock_pool, _ = _make_mock_pool()

    id1 = await log_consolidation_start(mock_pool, "agent", "manual", 0.5, [])
    id2 = await log_consolidation_start(mock_pool, "agent", "manual", 0.5, [])

    assert id1 != id2


# ---------------------------------------------------------------------------
# log_consolidation_complete
# ---------------------------------------------------------------------------

@pytest.mark.asyncio
async def test_log_consolidation_complete_executes_update():
    """log_consolidation_complete 應執行 UPDATE"""
    from duduclaw.memory_eval.db.consolidation import log_consolidation_complete

    mock_pool, mock_conn = _make_mock_pool()
    cid = str(uuid.uuid4())

    await log_consolidation_complete(
        db_pool=mock_pool,
        consolidation_id=cid,
        result_memory_ids=["sem-001", "sem-002"],
        pressure_after=0.20,
        duration_ms=3500,
    )

    mock_conn.execute.assert_called_once()
    call_args = mock_conn.execute.call_args[0]
    sql = call_args[0]
    assert "UPDATE memory_consolidation_log" in sql
    assert "completed" in sql


@pytest.mark.asyncio
async def test_log_consolidation_complete_passes_correct_args():
    """驗證 duration_ms 與 result_memory_ids 正確傳入"""
    from duduclaw.memory_eval.db.consolidation import log_consolidation_complete

    mock_pool, mock_conn = _make_mock_pool()
    cid = str(uuid.uuid4())
    result_ids = ["sem-a", "sem-b", "sem-c"]

    await log_consolidation_complete(
        db_pool=mock_pool,
        consolidation_id=cid,
        result_memory_ids=result_ids,
        pressure_after=0.15,
        duration_ms=5000,
    )

    call_positional = mock_conn.execute.call_args[0]
    # args[1]=completed_at, args[2]=result_memory_ids, args[3]=pressure_after,
    # args[4]=duration_ms, args[5]=consolidation_id
    assert result_ids in call_positional
    assert 5000 in call_positional
    assert cid in call_positional


# ---------------------------------------------------------------------------
# log_consolidation_failed
# ---------------------------------------------------------------------------

@pytest.mark.asyncio
async def test_log_consolidation_failed_executes_update():
    """log_consolidation_failed 應執行 UPDATE"""
    from duduclaw.memory_eval.db.consolidation import log_consolidation_failed

    mock_pool, mock_conn = _make_mock_pool()
    cid = str(uuid.uuid4())

    await log_consolidation_failed(
        db_pool=mock_pool,
        consolidation_id=cid,
        error_message="asyncpg timeout",
        duration_ms=1000,
    )

    mock_conn.execute.assert_called_once()
    call_args = mock_conn.execute.call_args[0]
    sql = call_args[0]
    assert "UPDATE memory_consolidation_log" in sql
    assert "failed" in sql


@pytest.mark.asyncio
async def test_log_consolidation_failed_passes_error_message():
    """error_message 應正確傳入 SQL"""
    from duduclaw.memory_eval.db.consolidation import log_consolidation_failed

    mock_pool, mock_conn = _make_mock_pool()
    cid = str(uuid.uuid4())
    err_msg = "Connection refused by DB server"

    await log_consolidation_failed(
        db_pool=mock_pool,
        consolidation_id=cid,
        error_message=err_msg,
        duration_ms=200,
    )

    call_positional = mock_conn.execute.call_args[0]
    assert err_msg in call_positional


# ---------------------------------------------------------------------------
# get_recent_consolidation_events
# ---------------------------------------------------------------------------

@pytest.mark.asyncio
async def test_get_recent_events_empty():
    """無記錄時回傳空 list"""
    from duduclaw.memory_eval.db.consolidation import get_recent_consolidation_events

    mock_pool, _ = _make_mock_pool(fetch_return=[])

    result = await get_recent_consolidation_events(
        db_pool=mock_pool,
        agent_id="test-agent",
        lookback_days=7,
    )

    assert result == []


@pytest.mark.asyncio
async def test_get_recent_events_returns_dicts():
    """有記錄時回傳 list[dict]"""
    from duduclaw.memory_eval.db.consolidation import get_recent_consolidation_events

    # asyncpg Row mock：讓 dict(row) 可用
    mock_row = {
        "id": str(uuid.uuid4()),
        "triggered_at": datetime.now(timezone.utc),
        "trigger_reason": "episodic_pressure_threshold",
        "pressure_before": 0.90,
        "pressure_after": 0.30,
        "source_memory_ids": ["mem-001"],
        "result_memory_ids": ["sem-001"],
        "source_memory_count": 1,
        "result_memory_count": 1,
        "duration_ms": 2000,
        "epr_quality_score": None,
        "epr_information_retention": None,
        "epr_compression_ratio_score": None,
        "epr_novelty_penalty": None,
    }

    # asyncpg.Record 支援 dict() 轉換 — 用 MagicMock 模擬
    class FakeRecord(dict):
        pass

    fake_row = FakeRecord(mock_row)

    mock_pool, mock_conn = _make_mock_pool(fetch_return=[fake_row])

    result = await get_recent_consolidation_events(
        db_pool=mock_pool,
        agent_id="test-agent",
        lookback_days=7,
    )

    assert len(result) == 1
    assert isinstance(result[0], dict)
    assert result[0]["trigger_reason"] == "episodic_pressure_threshold"


@pytest.mark.asyncio
async def test_get_recent_events_default_status_completed():
    """預設只查 status='completed'"""
    from duduclaw.memory_eval.db.consolidation import get_recent_consolidation_events

    mock_pool, mock_conn = _make_mock_pool(fetch_return=[])

    await get_recent_consolidation_events(mock_pool, "agent")

    mock_conn.fetch.assert_called_once()
    call_args = mock_conn.fetch.call_args[0]
    # $3 = status param，預設值 "completed"
    assert "completed" in call_args


@pytest.mark.asyncio
async def test_get_recent_events_custom_lookback():
    """可設定不同 lookback_days"""
    from duduclaw.memory_eval.db.consolidation import get_recent_consolidation_events

    mock_pool, mock_conn = _make_mock_pool(fetch_return=[])

    await get_recent_consolidation_events(mock_pool, "agent", lookback_days=30)

    mock_conn.fetch.assert_called_once()


@pytest.mark.asyncio
async def test_get_recent_events_none_status():
    """status=None 表示不過濾 status"""
    from duduclaw.memory_eval.db.consolidation import get_recent_consolidation_events

    mock_pool, mock_conn = _make_mock_pool(fetch_return=[])

    await get_recent_consolidation_events(mock_pool, "agent", status=None)

    mock_conn.fetch.assert_called_once()
    call_args = mock_conn.fetch.call_args[0]
    # status 參數傳入 None
    assert None in call_args
