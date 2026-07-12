"""
tests/test_personamem_v2.py
PersonaMem-v2 M1 — loader + recall@k 指標 + 告警 單測

覆蓋率目標：≥ 80%（離線，無網路 / 無 DB）
"""
from __future__ import annotations

import json
from pathlib import Path

import pytest

from duduclaw.memory_eval.personamem_v2 import (
    PMResult,
    SAMPLE_PATH,
    compute_personamem,
    evaluate_pm_alerts,
    load_personamem,
)
from duduclaw.memory_eval.config import PersonaMemConfig
from duduclaw.memory_eval.client import MemoryClient, SearchResult
from duduclaw.memory_eval.fixture_client import InMemoryMemoryClient


def write_jsonl(tmp_path: Path, rows: list[dict]) -> Path:
    p = tmp_path / "pm.jsonl"
    with open(p, "w", encoding="utf-8") as f:
        for r in rows:
            f.write(json.dumps(r, ensure_ascii=False) + "\n")
    return p


class MapClient(MemoryClient):
    def __init__(self, results_map):
        self._m = results_map

    async def search(self, query, limit=5, namespace=None):
        return self._m.get(query, [])[:limit]

    async def store(self, content, tags=None, namespace=None):
        return "mem-new"

    async def list_important(self, agent_id, min_importance=0.7, limit=500):
        return []

    async def list_active(self, agent_id):
        return []

    async def get_by_ids(self, memory_ids):
        return []

    async def get_episodic_pressure(self, hours_ago=24):
        return 0.0


# ---------------------------------------------------------------------------
# loader
# ---------------------------------------------------------------------------

def test_load_sample_fixture_exists():
    questions = load_personamem(SAMPLE_PATH)
    assert 3 <= len(questions) <= 5
    for q in questions:
        assert q.scenario
        assert q.evidence_memory_ids
        assert q.interactions


def test_load_missing_file():
    with pytest.raises(FileNotFoundError, match="PersonaMem-v2 dataset not found"):
        load_personamem(Path("/nonexistent/pm.jsonl"))


def test_load_defaults_scenario(tmp_path):
    p = write_jsonl(tmp_path, [
        {"question_id": "a", "question": "q", "evidence_memory_ids": ["m1"], "interactions": []},
    ])
    qs = load_personamem(p)
    assert qs[0].scenario == "general"


# ---------------------------------------------------------------------------
# status / to_report
# ---------------------------------------------------------------------------

def test_status_thresholds():
    assert PMResult(0.80, 5, 10, "sample").status == "✅ OK"
    assert PMResult(0.72, 5, 10, "sample").status == "⚠️ WARNING"
    assert PMResult(0.50, 5, 10, "sample").status == "🔴 CRITICAL"
    assert PMResult(0.0, 5, 0, "sample").status == "⚠️ WARNING"


def test_to_report_shape():
    r = PMResult(0.5, 5, 4, "sample", per_scenario={"food_ordering": 0.5})
    rep = r.to_report()
    assert rep["benchmark"] == "personamem_v2"
    assert rep["recall_at_k"] == 0.5
    assert rep["dataset"] == "sample"
    assert "per_scenario" in rep and "status" in rep


# ---------------------------------------------------------------------------
# compute — deterministic
# ---------------------------------------------------------------------------

@pytest.mark.asyncio
async def test_compute_full_recall(tmp_path):
    p = write_jsonl(tmp_path, [{
        "question_id": "q1", "question": "Q", "scenario": "travel_planning",
        "evidence_memory_ids": ["m1"], "interactions": [],
    }])
    client = MapClient({"Q": [SearchResult("m1", "c", 0.9)]})
    res = await compute_personamem(client, PersonaMemConfig(k=5), p, ingest=False)
    assert res.question_count == 1
    assert abs(res.recall_at_k - 1.0) < 1e-6
    assert res.per_scenario["travel_planning"] == 1.0


@pytest.mark.asyncio
async def test_compute_no_recall(tmp_path):
    p = write_jsonl(tmp_path, [{
        "question_id": "q1", "question": "Q", "scenario": "gift_recommendation",
        "evidence_memory_ids": ["m9"], "interactions": [],
    }])
    client = MapClient({"Q": [SearchResult("m1", "c", 0.5)]})
    res = await compute_personamem(client, PersonaMemConfig(k=5), p, ingest=False)
    assert res.recall_at_k == 0.0


@pytest.mark.asyncio
async def test_compute_skips_empty_evidence(tmp_path):
    p = write_jsonl(tmp_path, [{
        "question_id": "q1", "question": "Q", "scenario": "general",
        "evidence_memory_ids": [], "interactions": [],
    }])
    res = await compute_personamem(MapClient({}), PersonaMemConfig(), p, ingest=False)
    assert res.question_count == 0


# ---------------------------------------------------------------------------
# compute — end-to-end offline via InMemory client
# ---------------------------------------------------------------------------

@pytest.mark.asyncio
async def test_sample_end_to_end_offline():
    res = await compute_personamem(
        InMemoryMemoryClient(), PersonaMemConfig(), SAMPLE_PATH, ingest=True
    )
    assert res.question_count >= 3
    assert res.recall_at_k > 0.0
    assert res.dataset == "sample"
    assert res.per_scenario  # 至少一個情境有分


# ---------------------------------------------------------------------------
# alerts
# ---------------------------------------------------------------------------

def test_alerts_zero_questions():
    alerts = evaluate_pm_alerts(PMResult(0.0, 5, 0, "sample"), PersonaMemConfig())
    assert len(alerts) == 1 and "WARNING" in alerts[0]


def test_alerts_critical():
    alerts = evaluate_pm_alerts(PMResult(0.50, 5, 10, "full"), PersonaMemConfig())
    assert len(alerts) == 1 and "CRITICAL" in alerts[0]


def test_alerts_none_when_ok():
    assert evaluate_pm_alerts(PMResult(0.80, 5, 10, "full"), PersonaMemConfig()) == []
