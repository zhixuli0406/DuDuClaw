"""
tests/test_retrieval_accuracy.py
Retrieval Accuracy P1 — unit tests

覆蓋率目標：≥ 80%
"""
from __future__ import annotations

import json
import tempfile
from pathlib import Path
from unittest.mock import AsyncMock

import pytest

from duduclaw.memory_eval.retrieval_accuracy import (
    GoldenQAPair,
    RAResult,
    RAQueryResult,
    compute_retrieval_accuracy,
    evaluate_ra_alerts,
    load_golden_qa_set,
)
from duduclaw.memory_eval.client import MemoryClient, Memory, SearchResult
from duduclaw.memory_eval.config import EvalConfig


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def make_qa_pair(
    qa_id: str = "gqa-001",
    query: str = "What does the user love to eat?",
    relevant_ids: list[str] | None = None,
    source: str = "auto",
) -> GoldenQAPair:
    return GoldenQAPair(
        id=qa_id,
        query=query,
        relevant_memory_ids=relevant_ids or ["mem-001"],
        source=source,
        created="2026-05-01T00:00:00Z",
    )


def write_golden_qa_file(tmp_path: Path, pairs: list[dict]) -> Path:
    """將 QA pair list 寫入臨時 JSONL 檔案"""
    qa_file = tmp_path / "golden_qa_set.jsonl"
    with open(qa_file, "w") as f:
        for pair in pairs:
            f.write(json.dumps(pair, ensure_ascii=False) + "\n")
    return qa_file


class MockMemoryClient(MemoryClient):
    def __init__(self, results_map: dict[str, list[SearchResult]] | None = None):
        self._results_map = results_map or {}

    async def search(self, query, limit=5, namespace=None):
        return self._results_map.get(query, [])

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
# load_golden_qa_set tests
# ---------------------------------------------------------------------------

def test_load_golden_qa_set_success(tmp_path):
    """正常載入 JSONL → 解析成功"""
    qa_file = write_golden_qa_file(tmp_path, [
        {
            "id": "gqa-001",
            "query": "What does the user love?",
            "relevant_memory_ids": ["mem-001"],
            "source": "auto",
            "created": "2026-05-01T00:00:00Z",
        }
    ])
    pairs = load_golden_qa_set(qa_file)
    assert len(pairs) == 1
    assert pairs[0].id == "gqa-001"
    assert pairs[0].relevant_memory_ids == ["mem-001"]


def test_load_golden_qa_set_multiple(tmp_path):
    """多筆 QA pair 正常載入"""
    qa_data = [
        {
            "id": f"gqa-{i:03d}",
            "query": f"Question {i}",
            "relevant_memory_ids": [f"mem-{i:03d}"],
            "source": "auto",
            "created": "2026-05-01T00:00:00Z",
        }
        for i in range(1, 6)
    ]
    qa_file = write_golden_qa_file(tmp_path, qa_data)
    pairs = load_golden_qa_set(qa_file)
    assert len(pairs) == 5


def test_load_golden_qa_set_file_not_found():
    """檔案不存在 → FileNotFoundError"""
    with pytest.raises(FileNotFoundError, match="golden_qa_set.jsonl"):
        load_golden_qa_set(Path("/nonexistent/path/golden_qa_set.jsonl"))


def test_load_golden_qa_set_empty_file(tmp_path):
    """空 JSONL 檔案 → 回傳空列表"""
    qa_file = tmp_path / "golden_qa_set.jsonl"
    qa_file.write_text("")
    pairs = load_golden_qa_set(qa_file)
    assert pairs == []


def test_load_golden_qa_set_with_optional_category(tmp_path):
    """含 optional category 欄位的 QA pair 正常載入"""
    qa_file = write_golden_qa_file(tmp_path, [
        {
            "id": "gqa-001",
            "query": "Where does user work?",
            "relevant_memory_ids": ["mem-007"],
            "source": "manual",
            "created": "2026-05-01T00:00:00Z",
            "category": "single_hop",
        }
    ])
    pairs = load_golden_qa_set(qa_file)
    assert pairs[0].category == "single_hop"
    assert pairs[0].source == "manual"


# ---------------------------------------------------------------------------
# RAResult unit tests
# ---------------------------------------------------------------------------

def test_ra_result_status_ok():
    """Precision@K ≥ 75% → OK"""
    ra = RAResult(precision_at_k=0.80, k=5, query_count=100)
    assert ra.status == "✅ OK"


def test_ra_result_status_warning():
    """70% ≤ Precision@K < 75% → WARNING"""
    ra = RAResult(precision_at_k=0.72, k=5, query_count=100)
    assert ra.status == "⚠️ WARNING"


def test_ra_result_status_critical():
    """Precision@K < 70% → CRITICAL"""
    ra = RAResult(precision_at_k=0.60, k=5, query_count=100)
    assert ra.status == "🔴 CRITICAL"


# ---------------------------------------------------------------------------
# evaluate_ra_alerts unit tests
# ---------------------------------------------------------------------------

def test_evaluate_ra_alerts_no_alert():
    """RA ≥ 70% → 無告警"""
    ra = RAResult(precision_at_k=0.75, k=5, query_count=100)
    alerts = evaluate_ra_alerts(ra)
    assert alerts == []


def test_evaluate_ra_alerts_critical():
    """RA < 60% → CRITICAL 含向量模型評估建議"""
    ra = RAResult(precision_at_k=0.55, k=5, query_count=50)
    alerts = evaluate_ra_alerts(ra)
    assert len(alerts) == 1
    assert "CRITICAL" in alerts[0]
    assert "向量模型" in alerts[0]


def test_evaluate_ra_alerts_warning():
    """60% ≤ RA < 70% → WARNING"""
    ra = RAResult(precision_at_k=0.65, k=5, query_count=50)
    alerts = evaluate_ra_alerts(ra)
    assert len(alerts) == 1
    assert "WARNING" in alerts[0]


def test_evaluate_ra_alerts_zero_queries():
    """query_count = 0 → WARNING 含 seed run 說明"""
    ra = RAResult(precision_at_k=0.0, k=5, query_count=0)
    alerts = evaluate_ra_alerts(ra)
    assert len(alerts) == 1
    assert "WARNING" in alerts[0]
    assert "seed run" in alerts[0]


# ---------------------------------------------------------------------------
# compute_retrieval_accuracy integration tests
# ---------------------------------------------------------------------------

@pytest.mark.asyncio
async def test_compute_ra_perfect_recall(tmp_path):
    """所有 top-K 命中 relevant_memory_ids → Precision@K = 1.0"""
    qa_file = write_golden_qa_file(tmp_path, [
        {
            "id": "gqa-001",
            "query": "What does user love?",
            "relevant_memory_ids": ["mem-001"],
            "source": "auto",
            "created": "2026-05-01T00:00:00Z",
        }
    ])

    client = MockMemoryClient(
        results_map={
            "What does user love?": [
                SearchResult("mem-001", "user loves ramen", 0.95),
                SearchResult("mem-002", "user loves sushi", 0.80),
            ]
        }
    )
    config = EvalConfig(agent_id="test-agent", ra_k=5)
    result = await compute_retrieval_accuracy(client, config, qa_file)

    assert result.query_count == 1
    # relevant_found = 1, k = 5 → Precision@5 = 1/5 = 0.2
    assert abs(result.precision_at_k - 0.2) < 0.001


@pytest.mark.asyncio
async def test_compute_ra_no_recall(tmp_path):
    """top-K 完全不含 relevant → Precision@K = 0.0"""
    qa_file = write_golden_qa_file(tmp_path, [
        {
            "id": "gqa-001",
            "query": "Where does user work?",
            "relevant_memory_ids": ["mem-999"],
            "source": "auto",
            "created": "2026-05-01T00:00:00Z",
        }
    ])

    client = MockMemoryClient(
        results_map={
            "Where does user work?": [
                SearchResult("mem-001", "unrelated content", 0.50),
            ]
        }
    )
    config = EvalConfig(agent_id="test-agent", ra_k=5)
    result = await compute_retrieval_accuracy(client, config, qa_file)

    assert result.precision_at_k == 0.0


@pytest.mark.asyncio
async def test_compute_ra_skip_empty_relevant(tmp_path):
    """relevant_memory_ids 為空的 QA pair 應被跳過"""
    qa_file = write_golden_qa_file(tmp_path, [
        {
            "id": "gqa-001",
            "query": "Some question",
            "relevant_memory_ids": [],  # 空 → 跳過
            "source": "auto",
            "created": "2026-05-01T00:00:00Z",
        }
    ])
    client = MockMemoryClient()
    config = EvalConfig(agent_id="test-agent", ra_k=5)
    result = await compute_retrieval_accuracy(client, config, qa_file)

    assert result.query_count == 0
    assert result.precision_at_k == 0.0


@pytest.mark.asyncio
async def test_compute_ra_worst_queries_sorted(tmp_path):
    """worst_queries 按 precision_at_k 升序排列（最差在前）"""
    qa_data = [
        {
            "id": f"gqa-{i:03d}",
            "query": f"Question {i}",
            "relevant_memory_ids": [f"mem-{i:03d}"],
            "source": "auto",
            "created": "2026-05-01T00:00:00Z",
        }
        for i in range(1, 8)
    ]
    qa_file = write_golden_qa_file(tmp_path, qa_data)

    results_map = {
        f"Question {i}": [SearchResult(f"mem-{i:03d}", f"content {i}", 0.9)]
        for i in range(1, 8)
    }
    client = MockMemoryClient(results_map=results_map)
    config = EvalConfig(agent_id="test-agent", ra_k=5)
    result = await compute_retrieval_accuracy(client, config, qa_file)

    # worst_queries 應 ≤ 5 筆
    assert len(result.worst_queries) <= 5
    # 排序驗證：worst_queries[0].precision_at_k ≤ worst_queries[-1].precision_at_k
    if len(result.worst_queries) > 1:
        for i in range(len(result.worst_queries) - 1):
            assert (
                result.worst_queries[i].precision_at_k
                <= result.worst_queries[i + 1].precision_at_k
            )


@pytest.mark.asyncio
async def test_compute_ra_sampling(tmp_path):
    """qa_query_sample_size 限制最大評測筆數"""
    qa_data = [
        {
            "id": f"gqa-{i:03d}",
            "query": f"Question {i}",
            "relevant_memory_ids": [f"mem-{i:03d}"],
            "source": "auto",
            "created": "2026-05-01T00:00:00Z",
        }
        for i in range(1, 21)  # 20 筆
    ]
    qa_file = write_golden_qa_file(tmp_path, qa_data)
    client = MockMemoryClient()

    # 限制只評測 5 筆
    config = EvalConfig(agent_id="test-agent", ra_k=5, ra_query_sample_size=5)
    result = await compute_retrieval_accuracy(client, config, qa_file)

    assert result.query_count == 5


@pytest.mark.asyncio
async def test_compute_ra_mean_precision(tmp_path):
    """平均 Precision@K 計算正確性"""
    qa_file = write_golden_qa_file(tmp_path, [
        {
            "id": "gqa-001",
            "query": "Q1",
            "relevant_memory_ids": ["mem-001", "mem-002"],
            "source": "auto",
            "created": "2026-05-01T00:00:00Z",
        },
        {
            "id": "gqa-002",
            "query": "Q2",
            "relevant_memory_ids": ["mem-003"],
            "source": "auto",
            "created": "2026-05-01T00:00:00Z",
        },
    ])

    client = MockMemoryClient(
        results_map={
            "Q1": [
                SearchResult("mem-001", "content1", 0.9),
                SearchResult("mem-002", "content2", 0.8),
            ],
            "Q2": [],  # 沒有命中
        }
    )
    config = EvalConfig(agent_id="test-agent", ra_k=5)
    result = await compute_retrieval_accuracy(client, config, qa_file)

    # Q1: relevant_found=2, precision=2/5=0.4
    # Q2: relevant_found=0, precision=0.0
    # mean = (0.4 + 0.0) / 2 = 0.2
    assert result.query_count == 2
    assert abs(result.precision_at_k - 0.2) < 0.001
