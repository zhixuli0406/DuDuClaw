"""
tests/test_retention_rate.py
Retention Rate P1 — unit tests

覆蓋率目標：≥ 80%
"""
from __future__ import annotations

from datetime import date, timedelta
from unittest.mock import AsyncMock, MagicMock

import pytest

from duduclaw.memory_eval.retention_rate import (
    RRResult,
    SnapshotRecord,
    compute_retention_rate,
    evaluate_rr_alerts,
)
from duduclaw.memory_eval.client import MemoryClient, Memory, SearchResult
from duduclaw.memory_eval.config import EvalConfig


# ---------------------------------------------------------------------------
# Helper factories
# ---------------------------------------------------------------------------

def make_rr_result(rr_value: float, days: int) -> RRResult:
    total = 100
    recalled = int(rr_value * total)
    return RRResult(
        observation_days=days,
        recalled_count=recalled,
        total_count=total,
        retention_rate=rr_value,
    )


def make_snapshot(
    memory_id: str = "mem-001",
    importance_score: float = 0.9,
    summary: str | None = "The user loves ramen",
    days_ago: int = 7,
) -> SnapshotRecord:
    return SnapshotRecord(
        memory_id=memory_id,
        content_hash="abc123",
        importance_score=importance_score,
        summary=summary,
        memory_layer="episodic",
        snapshot_date=date.today() - timedelta(days=days_ago),
    )


class MockMemoryClient(MemoryClient):
    def __init__(self, search_results_by_query: dict | None = None):
        self._results = search_results_by_query or {}

    async def search(self, query, limit=5, namespace=None):
        return self._results.get(query, [])

    async def store(self, content, tags=None, namespace=None):
        return "mem-new"

    async def list_important(self, agent_id, min_importance=0.7, limit=500):
        return []

    async def list_active(self, agent_id):
        return []

    async def get_by_ids(self, memory_ids):
        return []

    async def get_episodic_pressure(self, hours_ago=24):
        return 0.5


# ---------------------------------------------------------------------------
# RRResult unit tests
# ---------------------------------------------------------------------------

def test_rr_result_status_ok():
    """RR ≥ 85% → OK"""
    assert make_rr_result(0.92, 7).status == "✅ OK"


def test_rr_result_status_warning():
    """80% ≤ RR < 85% → WARNING"""
    assert make_rr_result(0.82, 7).status == "⚠️ WARNING"


def test_rr_result_status_critical():
    """RR < 80% → CRITICAL"""
    assert make_rr_result(0.68, 7).status == "🔴 CRITICAL"


def test_rr_result_zero_total():
    """total=0 邊界：retention_rate = 0.0"""
    rr = RRResult(
        observation_days=7,
        recalled_count=0,
        total_count=0,
        retention_rate=0.0,
    )
    assert rr.status == "🔴 CRITICAL"


# ---------------------------------------------------------------------------
# evaluate_rr_alerts unit tests
# ---------------------------------------------------------------------------

def test_evaluate_rr_alerts_no_alert():
    """RR 超過所有門檻 → 無告警"""
    results = {
        7:  make_rr_result(0.90, 7),
        30: make_rr_result(0.87, 30),
    }
    alerts = evaluate_rr_alerts(results)
    assert alerts == []


def test_evaluate_rr_alerts_warning_7d():
    """RR(7d) = 78% → WARNING"""
    results = {7: make_rr_result(0.78, 7)}
    alerts = evaluate_rr_alerts(results)
    assert len(alerts) == 1
    assert "WARNING" in alerts[0]
    assert "RR(7d)" in alerts[0]


def test_evaluate_rr_alerts_critical_7d():
    """RR(7d) = 65% → CRITICAL + 含整合診斷建議"""
    results = {7: make_rr_result(0.65, 7)}
    alerts = evaluate_rr_alerts(results)
    assert len(alerts) == 1
    assert "CRITICAL" in alerts[0]
    assert "整合診斷" in alerts[0]


def test_evaluate_rr_alerts_warning_30d():
    """RR(30d) = 72% → WARNING"""
    results = {30: make_rr_result(0.72, 30)}
    alerts = evaluate_rr_alerts(results)
    assert len(alerts) == 1
    assert "WARNING" in alerts[0]
    assert "RR(30d)" in alerts[0]


def test_evaluate_rr_alerts_both_7d_and_30d():
    """7d CRITICAL 和 30d WARNING 同時存在"""
    results = {
        7:  make_rr_result(0.60, 7),
        30: make_rr_result(0.72, 30),
    }
    alerts = evaluate_rr_alerts(results)
    assert len(alerts) == 2


def test_evaluate_rr_alerts_empty_results():
    """空結果 → 無告警（無資料不告警）"""
    alerts = evaluate_rr_alerts({})
    assert alerts == []


def test_evaluate_rr_alerts_boundary_70():
    """RR(7d) 恰好 70% → CRITICAL（< 70% 才警告，邊界值：70.0% 剛好低於 WARNING）"""
    # 70% = 邊界：0.70 < 0.80 → WARNING，但 0.70 >= 0.70 → 不 CRITICAL
    results = {7: make_rr_result(0.70, 7)}
    alerts = evaluate_rr_alerts(results)
    assert len(alerts) == 1
    assert "WARNING" in alerts[0]
    assert "CRITICAL" not in alerts[0]


def test_evaluate_rr_alerts_boundary_69():
    """RR(7d) = 69% → CRITICAL"""
    results = {7: make_rr_result(0.69, 7)}
    alerts = evaluate_rr_alerts(results)
    assert "CRITICAL" in alerts[0]


# ---------------------------------------------------------------------------
# compute_retention_rate integration tests (with mocked DB)
# ---------------------------------------------------------------------------

@pytest.mark.asyncio
async def test_compute_retention_rate_no_baseline():
    """記憶快照為空時，RR 回傳 0.0（無資料情境）"""
    mock_pool = MagicMock()

    # mock DB conn.fetch → 回傳空列表
    mock_conn = AsyncMock()
    mock_conn.fetch.return_value = []
    mock_pool.acquire.return_value.__aenter__ = AsyncMock(return_value=mock_conn)
    mock_pool.acquire.return_value.__aexit__ = AsyncMock(return_value=False)

    client = MockMemoryClient()
    config = EvalConfig(agent_id="test-agent", rr_observation_days=[7])

    results = await compute_retention_rate(mock_pool, client, config)

    assert 7 in results
    assert results[7].total_count == 0
    assert results[7].retention_rate == 0.0


@pytest.mark.asyncio
async def test_compute_retention_rate_all_recalled():
    """所有記憶被成功召回 → RR = 1.0"""
    snapshot = make_snapshot(memory_id="mem-001", summary="user loves ramen")
    search_result = SearchResult("mem-001", "user loves ramen", similarity=0.90)

    mock_pool = MagicMock()
    mock_conn = AsyncMock()
    mock_conn.fetch.return_value = [
        {
            "memory_id": snapshot.memory_id,
            "content_hash": snapshot.content_hash,
            "importance_score": snapshot.importance_score,
            "summary": snapshot.summary,
            "memory_layer": snapshot.memory_layer,
            "snapshot_date": snapshot.snapshot_date,
        }
    ]
    mock_pool.acquire.return_value.__aenter__ = AsyncMock(return_value=mock_conn)
    mock_pool.acquire.return_value.__aexit__ = AsyncMock(return_value=False)

    client = MockMemoryClient(
        search_results_by_query={"user loves ramen": [search_result]}
    )
    config = EvalConfig(
        agent_id="test-agent",
        rr_observation_days=[7],
        rr_recall_threshold=0.75,
        ra_k=5,
    )

    results = await compute_retention_rate(mock_pool, client, config)

    assert 7 in results
    assert results[7].total_count == 1
    assert results[7].recalled_count == 1
    assert results[7].retention_rate == 1.0


@pytest.mark.asyncio
async def test_compute_retention_rate_none_recalled():
    """所有記憶搜尋 similarity < threshold → RR = 0.0"""
    snapshot = make_snapshot(memory_id="mem-002", summary="user loves sushi")
    low_sim_result = SearchResult("mem-002", "user loves sushi", similarity=0.50)

    mock_pool = MagicMock()
    mock_conn = AsyncMock()
    mock_conn.fetch.return_value = [
        {
            "memory_id": snapshot.memory_id,
            "content_hash": snapshot.content_hash,
            "importance_score": snapshot.importance_score,
            "summary": snapshot.summary,
            "memory_layer": snapshot.memory_layer,
            "snapshot_date": snapshot.snapshot_date,
        }
    ]
    mock_pool.acquire.return_value.__aenter__ = AsyncMock(return_value=mock_conn)
    mock_pool.acquire.return_value.__aexit__ = AsyncMock(return_value=False)

    client = MockMemoryClient(
        search_results_by_query={"user loves sushi": [low_sim_result]}
    )
    config = EvalConfig(
        agent_id="test-agent",
        rr_observation_days=[7],
        rr_recall_threshold=0.75,
        ra_k=5,
    )

    results = await compute_retention_rate(mock_pool, client, config)

    assert results[7].retention_rate == 0.0
    assert results[7].recalled_count == 0


@pytest.mark.asyncio
async def test_compute_retention_rate_uses_memory_id_fallback():
    """summary 為 None 時，canonical_query 退回 memory:{id}"""
    snapshot = make_snapshot(memory_id="mem-xyz", summary=None)
    search_result = SearchResult("mem-xyz", "something", similarity=0.90)

    mock_pool = MagicMock()
    mock_conn = AsyncMock()
    mock_conn.fetch.return_value = [
        {
            "memory_id": snapshot.memory_id,
            "content_hash": snapshot.content_hash,
            "importance_score": snapshot.importance_score,
            "summary": None,
            "memory_layer": snapshot.memory_layer,
            "snapshot_date": snapshot.snapshot_date,
        }
    ]
    mock_pool.acquire.return_value.__aenter__ = AsyncMock(return_value=mock_conn)
    mock_pool.acquire.return_value.__aexit__ = AsyncMock(return_value=False)

    client = MockMemoryClient(
        search_results_by_query={"memory:mem-xyz": [search_result]}
    )
    config = EvalConfig(
        agent_id="test-agent",
        rr_observation_days=[7],
        rr_recall_threshold=0.75,
        ra_k=5,
    )

    results = await compute_retention_rate(mock_pool, client, config)
    assert results[7].recalled_count == 1


@pytest.mark.asyncio
async def test_compute_retention_rate_multiple_observation_days():
    """同時計算 7d 和 30d"""
    mock_pool = MagicMock()
    mock_conn = AsyncMock()
    mock_conn.fetch.return_value = []  # 空快照
    mock_pool.acquire.return_value.__aenter__ = AsyncMock(return_value=mock_conn)
    mock_pool.acquire.return_value.__aexit__ = AsyncMock(return_value=False)

    client = MockMemoryClient()
    config = EvalConfig(agent_id="test-agent", rr_observation_days=[7, 30])

    results = await compute_retention_rate(mock_pool, client, config)

    assert 7 in results
    assert 30 in results
