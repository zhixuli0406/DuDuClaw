"""
memory_eval/personamem_v2.py
PersonaMem-v2 Benchmark 檢索評測（arXiv:2512.06688）

PersonaMem-v2：HF 資料集 `bowen-upenn/PersonaMem-v2`，1000 組 user-chatbot
互動、300+ 情境、隱式偏好，測長上下文使用者理解。

本模組量測「記憶系統能否檢索到回答隱式偏好題所需的 persona 記憶」的
recall@k：把每組互動史當記憶注入，對偏好題 search()，看承載該偏好的
gold persona 記憶是否落在 top-K，並依情境（scenario）分組。

⚠️ 完整多選題 answer-correctness（模型選對隱式偏好選項與否）需 LLM/選項
   評分，屬 PENDING-LIVE；本模組不宣稱跑過完整 PersonaMem-v2 分數。

資料集取得見 fetch_benchmarks.py。sample fixture（data/personamem_v2/sample.jsonl）
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
from .config import PersonaMemConfig

logger = logging.getLogger(__name__)

DATA_DIR = Path(__file__).parent / "data" / "personamem_v2"
SAMPLE_PATH = DATA_DIR / "sample.jsonl"
FULL_PATH = DATA_DIR / "full.jsonl"


@dataclass
class PersonaMemQuestion:
    question_id:         str
    question:            str              # 隱式偏好探問
    scenario:            str              # 情境標籤（300+ 種之一）
    evidence_memory_ids: list[str]        # 承載該偏好的 gold persona 記憶
    interactions:        list[dict]       # [{memory_id, content}, ...] user-chatbot 互動史
    expected_answer:     Optional[str] = None


@dataclass
class PMQueryResult:
    question_id:  str
    scenario:     str
    question:     str
    recall_at_k:  float
    top_k_ids:    list[str]
    evidence_ids: list[str]
    k:            int


@dataclass
class PMResult:
    recall_at_k:    float
    k:              int
    question_count: int
    dataset:        str                                # 'sample' | 'full'
    per_scenario:   dict[str, float] = field(default_factory=dict)
    query_results:  list[PMQueryResult] = field(default_factory=list)
    worst_queries:  list[PMQueryResult] = field(default_factory=list)

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
            "benchmark":      "personamem_v2",
            "recall_at_k":    round(self.recall_at_k, 4),
            "k":              self.k,
            "question_count": self.question_count,
            "dataset":        self.dataset,
            "per_scenario":   {s: round(v, 4) for s, v in self.per_scenario.items()},
            "status":         self.status,
        }


def load_personamem(path: Path = SAMPLE_PATH) -> list[PersonaMemQuestion]:
    """載入 PersonaMem-v2 JSONL（sample 或 fetch 後的 full）。"""
    if not path.exists():
        raise FileNotFoundError(
            f"PersonaMem-v2 dataset not found: {path}\n"
            "Run `python -m duduclaw.memory_eval.fetch_benchmarks personamem` "
            "to download from HF (bowen-upenn/PersonaMem-v2), "
            "or use the bundled sample.jsonl."
        )

    questions: list[PersonaMemQuestion] = []
    with open(path, encoding="utf-8") as f:
        for lineno, line in enumerate(f, 1):
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            try:
                data = json.loads(line)
                questions.append(PersonaMemQuestion(
                    question_id=data["question_id"],
                    question=data["question"],
                    scenario=data.get("scenario", "general"),
                    evidence_memory_ids=data.get("evidence_memory_ids", []),
                    interactions=data.get("interactions", []),
                    expected_answer=data.get("expected_answer"),
                ))
            except (json.JSONDecodeError, KeyError) as e:
                logger.warning("%s line %d: parse error — %s", path.name, lineno, e)
    return questions


async def compute_personamem(
    memory_client: MemoryClient,
    config: PersonaMemConfig,
    path: Path = SAMPLE_PATH,
    ingest: bool = True,
) -> PMResult:
    """計算 PersonaMem-v2 檢索 recall@k（依情境分組）。

    步驟同 LongMemEval：注入互動史 → 對隱式偏好題檢索 → recall@k。
    """
    questions = load_personamem(path)
    dataset = "sample" if path == SAMPLE_PATH else "full"
    logger.info("Loaded %d PersonaMem-v2 questions (%s)", len(questions), dataset)

    if len(questions) > config.sample_size:
        questions = random.sample(questions, config.sample_size)

    query_results: list[PMQueryResult] = []
    scenario_scores: dict[str, list[float]] = defaultdict(list)

    for q in questions:
        if not q.evidence_memory_ids:
            logger.debug("Skip %s: no evidence_memory_ids", q.question_id)
            continue

        namespace = f"{config.memory_namespace_prefix}_{q.question_id}"
        evidence_ids = list(q.evidence_memory_ids)

        if ingest and q.interactions:
            id_map = await ingest_haystack(memory_client, q.interactions, namespace)
            evidence_ids = [id_map.get(e, e) for e in evidence_ids]

        results: list[SearchResult] = await memory_client.search(
            query=q.question,
            limit=config.k,
            namespace=namespace,
        )
        top_k_ids = [r.memory_id for r in results]
        score = recall_at_k(top_k_ids, evidence_ids)

        qr = PMQueryResult(
            question_id=q.question_id,
            scenario=q.scenario,
            question=q.question,
            recall_at_k=score,
            top_k_ids=top_k_ids,
            evidence_ids=evidence_ids,
            k=config.k,
        )
        query_results.append(qr)
        scenario_scores[q.scenario].append(score)

    if not query_results:
        logger.warning("No PersonaMem-v2 questions with evidence to evaluate.")
        return PMResult(recall_at_k=0.0, k=config.k, question_count=0, dataset=dataset)

    mean_recall = sum(r.recall_at_k for r in query_results) / len(query_results)
    per_scenario = {
        s: sum(v) / len(v) for s, v in scenario_scores.items() if v
    }
    worst = sorted(query_results, key=lambda r: r.recall_at_k)[:5]

    logger.info(
        "PersonaMem-v2 recall@%d: %.1f%% (%d questions, %s)",
        config.k, mean_recall * 100, len(query_results), dataset,
    )
    return PMResult(
        recall_at_k=mean_recall,
        k=config.k,
        question_count=len(query_results),
        dataset=dataset,
        per_scenario=per_scenario,
        query_results=query_results,
        worst_queries=worst,
    )


def evaluate_pm_alerts(result: PMResult, config: PersonaMemConfig) -> list[str]:
    """生成 PersonaMem-v2 告警。"""
    alerts: list[str] = []
    if result.question_count == 0:
        alerts.append(
            "⚠️ WARNING: PersonaMem-v2 無可評測題目 — evidence_memory_ids 全空，"
            "或資料集未就緒（見 fetch_benchmarks.py）"
        )
    elif result.recall_at_k < config.recall_crit_threshold:
        alerts.append(
            f"🔴 CRITICAL: PersonaMem-v2 recall@{result.k} = "
            f"{result.recall_at_k:.1%} < {config.recall_crit_threshold:.0%} "
            f"({result.question_count} questions, {result.dataset})"
        )
    elif result.recall_at_k < config.recall_warn_threshold:
        alerts.append(
            f"⚠️ WARNING: PersonaMem-v2 recall@{result.k} = "
            f"{result.recall_at_k:.1%} < {config.recall_warn_threshold:.0%} "
            f"({result.question_count} questions, {result.dataset})"
        )
    return alerts
