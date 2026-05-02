"""
tests/test_build_golden_qa.py
Golden QA Set Seed Run 工具測試

W21 Sprint — ENG-MEMORY
任務：0b5c478e-729d-46f7-9408-a2f281c605f4
"""
from __future__ import annotations

import json
import asyncio
import pytest
from pathlib import Path
from unittest.mock import AsyncMock, patch

from duduclaw.memory_eval.build_golden_qa import (
    GoldenQAPair,
    load_jsonl,
    save_jsonl,
    run_seed_run,
    main,
)
from duduclaw.memory_eval.client import SearchResult
from duduclaw.memory_eval.config import EvalConfig


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


def make_pair(
    id: str = "gqa-001",
    query: str = "用戶喜歡吃什麼",
    relevant_memory_ids: list[str] | None = None,
    source: str = "auto",
    created: str = "2026-05-01",
    category: str | None = "personal_preference",
) -> GoldenQAPair:
    return GoldenQAPair(
        id=id,
        query=query,
        relevant_memory_ids=relevant_memory_ids or [],
        source=source,
        created=created,
        category=category,
    )


# ---------------------------------------------------------------------------
# load_jsonl
# ---------------------------------------------------------------------------


def test_load_jsonl_basic(tmp_path: Path) -> None:
    """基本 JSONL 載入"""
    jsonl = tmp_path / "qa.jsonl"
    jsonl.write_text(
        json.dumps({
            "id": "gqa-001",
            "query": "test query",
            "relevant_memory_ids": [],
            "source": "auto",
            "created": "2026-05-01",
            "category": "personal_preference",
        }) + "\n",
        encoding="utf-8",
    )
    pairs = load_jsonl(jsonl)
    assert len(pairs) == 1
    assert pairs[0].id == "gqa-001"
    assert pairs[0].query == "test query"


def test_load_jsonl_skip_empty_lines(tmp_path: Path) -> None:
    """空行和 # 開頭行應被跳過"""
    jsonl = tmp_path / "qa.jsonl"
    jsonl.write_text(
        "# comment\n"
        "\n"
        + json.dumps({
            "id": "gqa-002",
            "query": "query 2",
            "relevant_memory_ids": ["mem-abc"],
            "source": "manual",
            "created": "2026-05-01",
        }) + "\n",
        encoding="utf-8",
    )
    pairs = load_jsonl(jsonl)
    assert len(pairs) == 1
    assert pairs[0].relevant_memory_ids == ["mem-abc"]


def test_load_jsonl_with_relevant_ids(tmp_path: Path) -> None:
    """already-seeded pair 應正確載入 relevant_memory_ids"""
    jsonl = tmp_path / "qa.jsonl"
    jsonl.write_text(
        json.dumps({
            "id": "gqa-003",
            "query": "q3",
            "relevant_memory_ids": ["mem-1", "mem-2"],
            "source": "auto",
            "created": "2026-05-01",
        }) + "\n",
        encoding="utf-8",
    )
    pairs = load_jsonl(jsonl)
    assert pairs[0].relevant_memory_ids == ["mem-1", "mem-2"]


def test_load_jsonl_malformed_line_skipped(tmp_path: Path, caplog) -> None:
    """格式錯誤的行應跳過並記錄 warning"""
    jsonl = tmp_path / "qa.jsonl"
    jsonl.write_text(
        "not valid json\n"
        + json.dumps({
            "id": "gqa-ok",
            "query": "q",
            "relevant_memory_ids": [],
            "source": "auto",
            "created": "2026-05-01",
        }) + "\n",
        encoding="utf-8",
    )
    with caplog.at_level("WARNING"):
        pairs = load_jsonl(jsonl)
    assert len(pairs) == 1
    assert pairs[0].id == "gqa-ok"


# ---------------------------------------------------------------------------
# save_jsonl
# ---------------------------------------------------------------------------


def test_save_jsonl_round_trip(tmp_path: Path) -> None:
    """存檔後重新載入應與原始資料相同"""
    pairs = [
        make_pair("gqa-001", "query A", ["mem-1"]),
        make_pair("gqa-002", "query B", []),
    ]
    output = tmp_path / "output.jsonl"
    save_jsonl(pairs, output)

    loaded = load_jsonl(output)
    assert len(loaded) == 2
    assert loaded[0].id == "gqa-001"
    assert loaded[0].relevant_memory_ids == ["mem-1"]
    assert loaded[1].relevant_memory_ids == []


def test_save_jsonl_creates_parent_dir(tmp_path: Path) -> None:
    """應自動建立不存在的父目錄"""
    output = tmp_path / "nested" / "deep" / "output.jsonl"
    pairs = [make_pair()]
    save_jsonl(pairs, output)
    assert output.exists()


def test_save_jsonl_needs_manual_review_flag(tmp_path: Path) -> None:
    """needs_manual_review=True 應寫入輸出"""
    pair = make_pair()
    pair.needs_manual_review = True
    output = tmp_path / "qa.jsonl"
    save_jsonl([pair], output)

    raw = output.read_text()
    data = json.loads(raw.strip())
    assert data.get("needs_manual_review") is True


def test_save_jsonl_no_needs_manual_review_when_false(tmp_path: Path) -> None:
    """needs_manual_review=False 時不應輸出此欄位"""
    pair = make_pair()
    pair.needs_manual_review = False
    output = tmp_path / "qa.jsonl"
    save_jsonl([pair], output)

    raw = output.read_text()
    data = json.loads(raw.strip())
    assert "needs_manual_review" not in data


# ---------------------------------------------------------------------------
# run_seed_run
# ---------------------------------------------------------------------------


class MockMemoryClient:
    """測試用 MemoryClient Mock"""

    def __init__(self, search_results: list[SearchResult] | None = None) -> None:
        self._results = search_results or []
        self.search_calls: list[dict] = []

    async def search(self, query: str, limit: int = 5, namespace=None) -> list[SearchResult]:
        self.search_calls.append({"query": query, "limit": limit})
        return self._results

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


@pytest.mark.asyncio
async def test_run_seed_run_fills_empty_pair() -> None:
    """空的 relevant_memory_ids 應被填入搜尋結果"""
    mock_client = MockMemoryClient(
        search_results=[
            SearchResult("mem-001", "content 1", 0.90),
            SearchResult("mem-002", "content 2", 0.80),
        ]
    )
    pairs = [make_pair("gqa-001", "用戶喜歡吃什麼")]
    config = EvalConfig(agent_id="test-agent")

    with patch("duduclaw.memory_eval.build_golden_qa.build_client", return_value=mock_client):
        updated, stats = await run_seed_run(pairs, config, similarity_threshold=0.75)

    assert len(updated) == 1
    assert updated[0].relevant_memory_ids == ["mem-001", "mem-002"]
    assert stats["seeded"] == 1
    assert stats["no_results"] == 0


@pytest.mark.asyncio
async def test_run_seed_run_below_threshold_flagged() -> None:
    """similarity 低於門檻的結果不應填入，且 needs_manual_review=True"""
    mock_client = MockMemoryClient(
        search_results=[
            SearchResult("mem-low", "content", 0.50),  # 低於門檻
        ]
    )
    pairs = [make_pair("gqa-002", "query low sim")]
    config = EvalConfig(agent_id="test-agent")

    with patch("duduclaw.memory_eval.build_golden_qa.build_client", return_value=mock_client):
        updated, stats = await run_seed_run(pairs, config, similarity_threshold=0.75)

    assert updated[0].relevant_memory_ids == []
    assert updated[0].needs_manual_review is True
    assert stats["no_results"] == 1


@pytest.mark.asyncio
async def test_run_seed_run_skip_already_seeded() -> None:
    """已有 relevant_memory_ids 的 pair 應被跳過（不重 seed）"""
    mock_client = MockMemoryClient(
        search_results=[SearchResult("mem-new", "content", 0.95)]
    )
    pairs = [make_pair("gqa-003", "query", relevant_memory_ids=["mem-existing"])]
    config = EvalConfig(agent_id="test-agent")

    with patch("duduclaw.memory_eval.build_golden_qa.build_client", return_value=mock_client):
        updated, stats = await run_seed_run(pairs, config, force_reseed=False)

    # 應保留原始 ID，未重 seed
    assert updated[0].relevant_memory_ids == ["mem-existing"]
    assert stats["already_seeded"] == 1
    assert stats["seeded"] == 0
    assert len(mock_client.search_calls) == 0


@pytest.mark.asyncio
async def test_run_seed_run_force_reseed() -> None:
    """force_reseed=True 應對已有結果的 pair 重新 seed"""
    mock_client = MockMemoryClient(
        search_results=[SearchResult("mem-new", "content", 0.90)]
    )
    pairs = [make_pair("gqa-004", "query", relevant_memory_ids=["mem-old"])]
    config = EvalConfig(agent_id="test-agent")

    with patch("duduclaw.memory_eval.build_golden_qa.build_client", return_value=mock_client):
        updated, stats = await run_seed_run(pairs, config, force_reseed=True)

    assert updated[0].relevant_memory_ids == ["mem-new"]
    assert stats["seeded"] == 1
    assert stats["already_seeded"] == 0


@pytest.mark.asyncio
async def test_run_seed_run_handles_exception() -> None:
    """搜尋拋出異常時應記錄錯誤並標記 needs_manual_review"""
    mock_client = MockMemoryClient()
    mock_client.search = AsyncMock(side_effect=RuntimeError("Connection failed"))

    pairs = [make_pair("gqa-005", "error query")]
    config = EvalConfig(agent_id="test-agent")

    with patch("duduclaw.memory_eval.build_golden_qa.build_client", return_value=mock_client):
        updated, stats = await run_seed_run(pairs, config)

    assert updated[0].needs_manual_review is True
    assert stats["errors"] == 1


@pytest.mark.asyncio
async def test_run_seed_run_mixed_pairs() -> None:
    """混合 pair：空的 seed、已有的跳過、低相似度的標記"""
    mock_client = MockMemoryClient(
        search_results=[SearchResult("mem-fresh", "content", 0.88)]
    )

    pairs = [
        make_pair("gqa-a", "empty query", relevant_memory_ids=[]),
        make_pair("gqa-b", "filled query", relevant_memory_ids=["mem-old"]),
    ]
    config = EvalConfig(agent_id="test-agent")

    with patch("duduclaw.memory_eval.build_golden_qa.build_client", return_value=mock_client):
        updated, stats = await run_seed_run(pairs, config, force_reseed=False)

    assert updated[0].relevant_memory_ids == ["mem-fresh"]
    assert updated[1].relevant_memory_ids == ["mem-old"]  # 跳過，保持原值
    assert stats["seeded"] == 1
    assert stats["already_seeded"] == 1


# ---------------------------------------------------------------------------
# main() CLI
# ---------------------------------------------------------------------------


def test_main_stats_only(tmp_path: Path) -> None:
    """--stats-only 模式應顯示統計並返回 0"""
    jsonl = tmp_path / "qa.jsonl"
    jsonl.write_text(
        json.dumps({
            "id": "gqa-001",
            "query": "q",
            "relevant_memory_ids": ["mem-1"],
            "source": "auto",
            "created": "2026-05-01",
        }) + "\n" +
        json.dumps({
            "id": "gqa-002",
            "query": "q2",
            "relevant_memory_ids": [],
            "source": "auto",
            "created": "2026-05-01",
        }) + "\n",
        encoding="utf-8",
    )

    exit_code = main([
        "--input", str(jsonl),
        "--output", str(tmp_path / "out.jsonl"),
        "--agent-id", "test-agent",
        "--stats-only",
    ])
    assert exit_code == 0


def test_main_input_not_found(tmp_path: Path) -> None:
    """輸入文件不存在時應返回 1"""
    exit_code = main([
        "--input", str(tmp_path / "nonexistent.jsonl"),
        "--output", str(tmp_path / "out.jsonl"),
        "--agent-id", "test-agent",
    ])
    assert exit_code == 1


def test_main_copy_without_seed_run(tmp_path: Path) -> None:
    """不加 --seed-run 時應直接複製輸入到輸出"""
    jsonl = tmp_path / "qa.jsonl"
    jsonl.write_text(
        json.dumps({
            "id": "gqa-001",
            "query": "q",
            "relevant_memory_ids": [],
            "source": "auto",
            "created": "2026-05-01",
        }) + "\n",
        encoding="utf-8",
    )
    output = tmp_path / "out.jsonl"

    exit_code = main([
        "--input", str(jsonl),
        "--output", str(output),
        "--agent-id", "test-agent",
    ])
    assert exit_code == 0
    assert output.exists()
    pairs = load_jsonl(output)
    assert len(pairs) == 1


# ---------------------------------------------------------------------------
# print_stats
# ---------------------------------------------------------------------------


def test_print_stats_outputs_report(tmp_path: Path, capsys) -> None:
    """print_stats 應輸出包含各統計數字的報告"""
    from duduclaw.memory_eval.build_golden_qa import print_stats

    stats = {
        "total": 100,
        "seeded": 70,
        "already_seeded": 20,
        "no_results": 8,
        "errors": 2,
    }
    output_path = tmp_path / "out.jsonl"
    print_stats(stats, output_path)

    captured = capsys.readouterr()
    assert "100" in captured.out
    assert "70" in captured.out
    assert "20" in captured.out
    assert "8" in captured.out
    assert "2" in captured.out
    assert "90.0%" in captured.out   # coverage = (70+20)/100
    assert str(output_path) in captured.out


def test_print_stats_zero_total(tmp_path: Path, capsys) -> None:
    """total=0 時 coverage 應為 0.0%，不應出現 ZeroDivisionError"""
    from duduclaw.memory_eval.build_golden_qa import print_stats

    stats = {
        "total": 0,
        "seeded": 0,
        "already_seeded": 0,
        "no_results": 0,
        "errors": 0,
    }
    print_stats(stats, tmp_path / "out.jsonl")
    captured = capsys.readouterr()
    assert "0.0%" in captured.out


# ---------------------------------------------------------------------------
# main() with --seed-run（完整路徑）
# ---------------------------------------------------------------------------


def test_main_seed_run_success(tmp_path: Path) -> None:
    """--seed-run 且無錯誤時應返回 0，並寫入輸出"""
    from duduclaw.memory_eval.build_golden_qa import GoldenQAPair, print_stats
    from unittest.mock import patch, AsyncMock

    jsonl = tmp_path / "input.jsonl"
    jsonl.write_text(
        json.dumps({
            "id": "gqa-001",
            "query": "test query",
            "relevant_memory_ids": [],
            "source": "auto",
            "created": "2026-05-01",
        }) + "\n",
        encoding="utf-8",
    )
    output = tmp_path / "output.jsonl"

    updated_pairs = [
        GoldenQAPair(
            id="gqa-001",
            query="test query",
            relevant_memory_ids=["mem-001"],
            source="auto",
            created="2026-05-01",
        )
    ]
    stats = {
        "total": 1,
        "seeded": 1,
        "already_seeded": 0,
        "no_results": 0,
        "errors": 0,
    }

    with patch(
        "duduclaw.memory_eval.build_golden_qa.asyncio.run",
        return_value=(updated_pairs, stats),
    ):
        exit_code = main([
            "--input", str(jsonl),
            "--output", str(output),
            "--agent-id", "test-agent",
            "--seed-run",
        ])

    assert exit_code == 0
    assert output.exists()
    result_pairs = load_jsonl(output)
    assert result_pairs[0].relevant_memory_ids == ["mem-001"]


def test_main_seed_run_with_errors_returns_2(tmp_path: Path) -> None:
    """--seed-run 且 stats['errors'] > 0 時應返回 2"""
    from duduclaw.memory_eval.build_golden_qa import GoldenQAPair

    jsonl = tmp_path / "input.jsonl"
    jsonl.write_text(
        json.dumps({
            "id": "gqa-001",
            "query": "test query",
            "relevant_memory_ids": [],
            "source": "auto",
            "created": "2026-05-01",
        }) + "\n",
        encoding="utf-8",
    )
    output = tmp_path / "output.jsonl"

    updated_pairs = [
        GoldenQAPair(
            id="gqa-001",
            query="test query",
            relevant_memory_ids=[],
            source="auto",
            created="2026-05-01",
            needs_manual_review=True,
        )
    ]
    stats = {
        "total": 1,
        "seeded": 0,
        "already_seeded": 0,
        "no_results": 0,
        "errors": 1,
    }

    with patch(
        "duduclaw.memory_eval.build_golden_qa.asyncio.run",
        return_value=(updated_pairs, stats),
    ):
        exit_code = main([
            "--input", str(jsonl),
            "--output", str(output),
            "--agent-id", "test-agent",
            "--seed-run",
        ])

    assert exit_code == 2
