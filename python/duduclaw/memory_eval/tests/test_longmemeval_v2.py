"""
tests/test_longmemeval_v2.py
LongMemEval-V2 M1 — loader + recall@k 指標 + 告警 單測

覆蓋率目標：≥ 80%（離線，無網路 / 無 DB）
"""
from __future__ import annotations

import json
from pathlib import Path

import pytest

from duduclaw.memory_eval.longmemeval_v2 import (
    ABILITIES,
    LMEResult,
    SAMPLE_PATH,
    compute_longmemeval,
    evaluate_lme_alerts,
    load_longmemeval,
)
from duduclaw.memory_eval.config import LongMemEvalConfig
from duduclaw.memory_eval.client import MemoryClient, SearchResult
from duduclaw.memory_eval.fixture_client import InMemoryMemoryClient


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def write_jsonl(tmp_path: Path, rows: list[dict]) -> Path:
    p = tmp_path / "lme.jsonl"
    with open(p, "w", encoding="utf-8") as f:
        for r in rows:
            f.write(json.dumps(r, ensure_ascii=False) + "\n")
    return p


class MapClient(MemoryClient):
    """靜態 map client（不 ingest），供確定性指標測試。"""
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
    """內建 sample fixture 可載入且題數 3-5、能力屬 5 種之一。"""
    questions = load_longmemeval(SAMPLE_PATH)
    assert 3 <= len(questions) <= 5
    for q in questions:
        assert q.ability in ABILITIES
        assert q.evidence_memory_ids
        assert q.haystack


def test_load_skips_comments_and_blanks(tmp_path):
    p = tmp_path / "lme.jsonl"
    p.write_text(
        "# comment\n\n"
        '{"question_id":"a","question":"q","ability":"temporal_reasoning",'
        '"evidence_memory_ids":["m1"],"haystack":[{"memory_id":"m1","content":"c"}]}\n',
        encoding="utf-8",
    )
    qs = load_longmemeval(p)
    assert len(qs) == 1
    assert qs[0].ability == "temporal_reasoning"


def test_load_missing_file():
    with pytest.raises(FileNotFoundError, match="LongMemEval-V2 dataset not found"):
        load_longmemeval(Path("/nonexistent/lme.jsonl"))


def test_load_defaults_ability(tmp_path):
    p = write_jsonl(tmp_path, [
        {"question_id": "a", "question": "q", "evidence_memory_ids": ["m1"], "haystack": []},
    ])
    qs = load_longmemeval(p)
    assert qs[0].ability == "information_extraction"


# ---------------------------------------------------------------------------
# status / to_report
# ---------------------------------------------------------------------------

def test_status_thresholds():
    assert LMEResult(0.80, 5, 10, "sample").status == "✅ OK"
    assert LMEResult(0.72, 5, 10, "sample").status == "⚠️ WARNING"
    assert LMEResult(0.50, 5, 10, "sample").status == "🔴 CRITICAL"
    assert LMEResult(0.0, 5, 0, "sample").status == "⚠️ WARNING"


def test_to_report_shape():
    r = LMEResult(0.5, 5, 4, "sample", per_ability={"temporal_reasoning": 0.5})
    rep = r.to_report()
    assert rep["benchmark"] == "longmemeval_v2"
    assert rep["recall_at_k"] == 0.5
    assert rep["k"] == 5
    assert rep["question_count"] == 4
    assert rep["dataset"] == "sample"
    assert "status" in rep and "per_ability" in rep


# ---------------------------------------------------------------------------
# compute — deterministic (MapClient, no ingest)
# ---------------------------------------------------------------------------

@pytest.mark.asyncio
async def test_compute_full_recall(tmp_path):
    """evidence 全落 top-K → recall=1.0。"""
    p = write_jsonl(tmp_path, [{
        "question_id": "q1", "question": "Q", "ability": "information_extraction",
        "evidence_memory_ids": ["m1", "m2"], "haystack": [],
    }])
    client = MapClient({"Q": [SearchResult("m1", "c", 0.9), SearchResult("m2", "c", 0.8)]})
    res = await compute_longmemeval(client, LongMemEvalConfig(k=5), p, ingest=False)
    assert res.question_count == 1
    assert abs(res.recall_at_k - 1.0) < 1e-6


@pytest.mark.asyncio
async def test_compute_partial_recall(tmp_path):
    """2 條 evidence 命中 1 → recall=0.5。"""
    p = write_jsonl(tmp_path, [{
        "question_id": "q1", "question": "Q", "ability": "knowledge_update",
        "evidence_memory_ids": ["m1", "m2"], "haystack": [],
    }])
    client = MapClient({"Q": [SearchResult("m1", "c", 0.9), SearchResult("mX", "c", 0.8)]})
    res = await compute_longmemeval(client, LongMemEvalConfig(k=5), p, ingest=False)
    assert abs(res.recall_at_k - 0.5) < 1e-6
    assert res.per_ability["knowledge_update"] == 0.5


@pytest.mark.asyncio
async def test_compute_skips_empty_evidence(tmp_path):
    p = write_jsonl(tmp_path, [{
        "question_id": "q1", "question": "Q", "ability": "abstention",
        "evidence_memory_ids": [], "haystack": [],
    }])
    res = await compute_longmemeval(MapClient({}), LongMemEvalConfig(), p, ingest=False)
    assert res.question_count == 0
    assert res.recall_at_k == 0.0


# ---------------------------------------------------------------------------
# compute — end-to-end offline via InMemory client (store→search→score)
# ---------------------------------------------------------------------------

@pytest.mark.asyncio
async def test_sample_end_to_end_offline():
    """對內建 sample fixture 跑真 ingest+search，recall 應 > 0（管線接線驗證）。"""
    res = await compute_longmemeval(
        InMemoryMemoryClient(), LongMemEvalConfig(), SAMPLE_PATH, ingest=True
    )
    assert res.question_count >= 3
    assert res.recall_at_k > 0.0
    assert res.dataset == "sample"
    # 每題能力都應出現在 per_ability
    assert set(res.per_ability.keys()).issubset(set(ABILITIES))


# ---------------------------------------------------------------------------
# alerts
# ---------------------------------------------------------------------------

def test_alerts_zero_questions():
    cfg = LongMemEvalConfig()
    alerts = evaluate_lme_alerts(LMEResult(0.0, 5, 0, "sample"), cfg)
    assert len(alerts) == 1 and "WARNING" in alerts[0]


def test_alerts_critical():
    cfg = LongMemEvalConfig()
    alerts = evaluate_lme_alerts(LMEResult(0.50, 5, 10, "full"), cfg)
    assert len(alerts) == 1 and "CRITICAL" in alerts[0]


def test_alerts_warning():
    cfg = LongMemEvalConfig()
    alerts = evaluate_lme_alerts(LMEResult(0.65, 5, 10, "full"), cfg)
    assert len(alerts) == 1 and "WARNING" in alerts[0]


def test_alerts_none_when_ok():
    cfg = LongMemEvalConfig()
    assert evaluate_lme_alerts(LMEResult(0.80, 5, 10, "full"), cfg) == []
