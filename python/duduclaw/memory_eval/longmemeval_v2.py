"""
memory_eval/longmemeval_v2.py
LongMemEval-V2 Benchmark 檢索評測（arXiv:2605.12493）

LongMemEval-V2：451 題、5 種核心記憶能力、context 由 web-agent 歷史
trajectory 構成，屬 agentic 經驗記憶評測。

本模組量測「記憶系統檢索層級」的 recall@k：把每題的 haystack 當記憶注入，
對問題 search()，看 gold evidence 是否落在 top-K，並依 5 種能力分組。

⚠️ 完整 QA answer-correctness（模型讀檢索結果後答對與否）需 LLM judge，
   屬 PENDING-LIVE；本模組不宣稱跑過完整 LongMemEval-V2 分數。

資料集取得見 fetch_benchmarks.py。sample fixture（data/longmemeval_v2/sample.jsonl）
為手造 3-5 題、供離線 smoke / 單測。

M1 記憶評測接軌
"""
from __future__ import annotations

import json
import logging
import random
from collections import defaultdict
from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional

from .benchmark_common import ingest_haystack, recall_at_k
from .client import MemoryClient, SearchResult
from .config import LongMemEvalConfig

logger = logging.getLogger(__name__)

DATA_DIR = Path(__file__).parent / "data" / "longmemeval_v2"
SAMPLE_PATH = DATA_DIR / "sample.jsonl"
FULL_PATH = DATA_DIR / "full.jsonl"

# 官方 5 種核心記憶能力（arXiv:2605.12493）
ABILITIES = (
    "information_extraction",
    "multi_session_reasoning",
    "temporal_reasoning",
    "knowledge_update",
    "abstention",
)


@dataclass
class LongMemEvalQuestion:
    question_id:         str
    question:            str
    ability:             str              # 5 種能力之一
    evidence_memory_ids: list[str]        # 含答案的 gold 記憶 id
    haystack:            list[dict]       # [{memory_id, content}, ...]
    answer:              Optional[str] = None


@dataclass
class LMEQueryResult:
    question_id:  str
    ability:      str
    question:     str
    recall_at_k:  float
    top_k_ids:    list[str]
    evidence_ids: list[str]
    k:            int


@dataclass
class LMEResult:
    recall_at_k:    float                              # 平均 recall@k
    k:              int
    question_count: int
    dataset:        str                                # 'sample' | 'full'
    per_ability:    dict[str, float] = field(default_factory=dict)
    query_results:  list[LMEQueryResult] = field(default_factory=list)
    worst_queries:  list[LMEQueryResult] = field(default_factory=list)

    @property
    def status(self) -> str:
        if self.question_count == 0:
            return "⚠️ WARNING"
        if self.recall_at_k >= 0.75:
            return "✅ OK"
        if self.recall_at_k >= 0.70:
            return "⚠️ WARNING"
        return "🔴 CRITICAL"

    def to_report(self) -> dict:
        """對齊 LOCOMO/RA 的 report 輸出格式（供 cron/dashboard 顯示）。"""
        return {
            "benchmark":      "longmemeval_v2",
            "recall_at_k":    round(self.recall_at_k, 4),
            "k":              self.k,
            "question_count": self.question_count,
            "dataset":        self.dataset,
            "per_ability":    {a: round(v, 4) for a, v in self.per_ability.items()},
            "status":         self.status,
        }


def load_longmemeval(path: Path = SAMPLE_PATH) -> list[LongMemEvalQuestion]:
    """載入 LongMemEval-V2 JSONL（sample 或 fetch 後的 full）。"""
    if not path.exists():
        raise FileNotFoundError(
            f"LongMemEval-V2 dataset not found: {path}\n"
            "Run `python -m duduclaw.memory_eval.fetch_benchmarks longmemeval` "
            "to download the full set, or use the bundled sample.jsonl."
        )

    questions: list[LongMemEvalQuestion] = []
    with open(path, encoding="utf-8") as f:
        for lineno, line in enumerate(f, 1):
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            try:
                data = json.loads(line)
                questions.append(LongMemEvalQuestion(
                    question_id=data["question_id"],
                    question=data["question"],
                    ability=data.get("ability", "information_extraction"),
                    evidence_memory_ids=data.get("evidence_memory_ids", []),
                    haystack=data.get("haystack", []),
                    answer=data.get("answer"),
                ))
            except (json.JSONDecodeError, KeyError) as e:
                logger.warning("%s line %d: parse error — %s", path.name, lineno, e)
    return questions


async def compute_longmemeval(
    memory_client: MemoryClient,
    config: LongMemEvalConfig,
    path: Path = SAMPLE_PATH,
    ingest: bool = True,
) -> LMEResult:
    """計算 LongMemEval-V2 檢索 recall@k（依 5 種能力分組）。

    步驟：
    1. 載入題目（sample 或 full）
    2. 抽樣至 config.sample_size
    3. （選）把每題 haystack 注入記憶系統
    4. 對每題 search，取 top-K，算 recall@k = |命中 evidence| / |evidence|
    5. 總平均 + 各能力平均

    Args:
        ingest: True = 先注入 haystack（離線 InMemory 或真實引擎皆需）；
                False = 假設記憶已預先注入，只跑檢索。
    """
    questions = load_longmemeval(path)
    dataset = "sample" if path == SAMPLE_PATH else "full"
    logger.info("Loaded %d LongMemEval-V2 questions (%s)", len(questions), dataset)

    if len(questions) > config.sample_size:
        questions = random.sample(questions, config.sample_size)

    query_results: list[LMEQueryResult] = []
    ability_scores: dict[str, list[float]] = defaultdict(list)

    for q in questions:
        if not q.evidence_memory_ids:
            logger.debug("Skip %s: no evidence_memory_ids", q.question_id)
            continue

        namespace = f"{config.memory_namespace_prefix}_{q.question_id}"
        evidence_ids = list(q.evidence_memory_ids)

        if ingest and q.haystack:
            id_map = await ingest_haystack(memory_client, q.haystack, namespace)
            # evidence id 若被 store 改寫，映射到實際寫入 id
            evidence_ids = [id_map.get(e, e) for e in evidence_ids]

        results: list[SearchResult] = await memory_client.search(
            query=q.question,
            limit=config.k,
            namespace=namespace,
        )
        top_k_ids = [r.memory_id for r in results]
        score = recall_at_k(top_k_ids, evidence_ids)

        qr = LMEQueryResult(
            question_id=q.question_id,
            ability=q.ability,
            question=q.question,
            recall_at_k=score,
            top_k_ids=top_k_ids,
            evidence_ids=evidence_ids,
            k=config.k,
        )
        query_results.append(qr)
        ability_scores[q.ability].append(score)

    if not query_results:
        logger.warning("No LongMemEval-V2 questions with evidence to evaluate.")
        return LMEResult(recall_at_k=0.0, k=config.k, question_count=0, dataset=dataset)

    mean_recall = sum(r.recall_at_k for r in query_results) / len(query_results)
    per_ability = {
        a: sum(s) / len(s) for a, s in ability_scores.items() if s
    }
    worst = sorted(query_results, key=lambda r: r.recall_at_k)[:5]

    logger.info(
        "LongMemEval-V2 recall@%d: %.1f%% (%d questions, %s)",
        config.k, mean_recall * 100, len(query_results), dataset,
    )
    return LMEResult(
        recall_at_k=mean_recall,
        k=config.k,
        question_count=len(query_results),
        dataset=dataset,
        per_ability=per_ability,
        query_results=query_results,
        worst_queries=worst,
    )


def evaluate_lme_alerts(result: LMEResult, config: LongMemEvalConfig) -> list[str]:
    """生成 LongMemEval-V2 告警。"""
    alerts: list[str] = []
    if result.question_count == 0:
        alerts.append(
            "⚠️ WARNING: LongMemEval-V2 無可評測題目 — evidence_memory_ids 全空，"
            "或資料集未就緒（見 fetch_benchmarks.py）"
        )
    elif result.recall_at_k < config.recall_crit_threshold:
        alerts.append(
            f"🔴 CRITICAL: LongMemEval-V2 recall@{result.k} = "
            f"{result.recall_at_k:.1%} < {config.recall_crit_threshold:.0%} "
            f"({result.question_count} questions, {result.dataset})"
        )
    elif result.recall_at_k < config.recall_warn_threshold:
        alerts.append(
            f"⚠️ WARNING: LongMemEval-V2 recall@{result.k} = "
            f"{result.recall_at_k:.1%} < {config.recall_warn_threshold:.0%} "
            f"({result.question_count} questions, {result.dataset})"
        )
    return alerts
