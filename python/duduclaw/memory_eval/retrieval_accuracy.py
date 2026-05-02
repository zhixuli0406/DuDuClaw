"""
memory_eval/retrieval_accuracy.py
Retrieval Accuracy (RA) 評測

依賴：
  - data/golden_qa_set.jsonl（Golden QA Set）
  - MemoryClient.search()

W21 Sprint 實作 — ENG-MEMORY
"""
from __future__ import annotations

import json
import logging
import random
from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional

from .client import MemoryClient, SearchResult
from .config import EvalConfig

logger = logging.getLogger(__name__)

GOLDEN_QA_PATH = Path(__file__).parent / "data" / "golden_qa_set.jsonl"


@dataclass
class GoldenQAPair:
    id:                  str
    query:               str
    relevant_memory_ids: list[str]
    source:              str              # 'auto' | 'manual'
    created:             str
    category:            Optional[str]  = None


@dataclass
class RAQueryResult:
    qa_id:          str
    query:          str
    precision_at_k: float
    top_k_ids:      list[str]
    relevant_found: int
    k:              int


@dataclass
class RAResult:
    precision_at_k: float               # 平均 Precision@K
    k:              int
    query_count:    int
    query_results:  list[RAQueryResult] = field(default_factory=list)
    worst_queries:  list[RAQueryResult] = field(default_factory=list)  # 最差 5 筆

    @property
    def status(self) -> str:
        if self.precision_at_k >= 0.75:
            return "✅ OK"
        elif self.precision_at_k >= 0.70:
            return "⚠️ WARNING"
        else:
            return "🔴 CRITICAL"


def load_golden_qa_set(path: Path = GOLDEN_QA_PATH) -> list[GoldenQAPair]:
    """載入 Golden QA Set from JSONL"""
    if not path.exists():
        raise FileNotFoundError(
            f"Golden QA Set not found: {path}\n"
            "Run build_golden_qa_set() to initialize."
        )

    pairs: list[GoldenQAPair] = []
    with open(path) as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            data = json.loads(line)
            pairs.append(GoldenQAPair(**data))

    return pairs


async def compute_retrieval_accuracy(
    memory_client: MemoryClient,
    config: EvalConfig,
    golden_qa_path: Path = GOLDEN_QA_PATH,
) -> RAResult:
    """
    計算 Retrieval Accuracy（Precision@K）

    實作步驟（依規格 §3.3）：
    1. 載入 Golden QA Set
    2. 隨機抽取 query_sample_size 條（或全量）
    3. 每條 query 執行 memory_search，取 top-K 結果
    4. 判斷 relevant：top-K 中有幾條在 relevant_memory_ids 中
    5. Precision@K(q) = relevant_in_top_k / K
    6. RA = mean(Precision@K across all queries)

    Returns:
        RAResult
    """
    qa_pairs = load_golden_qa_set(golden_qa_path)
    logger.info("Loaded %d QA pairs from golden set", len(qa_pairs))

    # 抽樣
    if len(qa_pairs) > config.ra_query_sample_size:
        sampled = random.sample(qa_pairs, config.ra_query_sample_size)
    else:
        sampled = qa_pairs

    query_results: list[RAQueryResult] = []

    for qa in sampled:
        # 過濾：relevant_memory_ids 為空則跳過（Phase 1 初期可能有 TBD 條目）
        if not qa.relevant_memory_ids:
            logger.debug("Skipping QA %s: no relevant_memory_ids", qa.id)
            continue

        search_results: list[SearchResult] = await memory_client.search(
            query=qa.query,
            limit=config.ra_k,
        )

        top_k_ids     = [r.memory_id for r in search_results]
        relevant_set  = set(qa.relevant_memory_ids)
        relevant_found = sum(1 for mid in top_k_ids if mid in relevant_set)
        precision      = relevant_found / config.ra_k

        query_results.append(RAQueryResult(
            qa_id=qa.id,
            query=qa.query,
            precision_at_k=precision,
            top_k_ids=top_k_ids,
            relevant_found=relevant_found,
            k=config.ra_k,
        ))

    if not query_results:
        logger.warning(
            "No valid QA pairs to evaluate (all skipped due to empty relevant_memory_ids). "
            "Phase 1 Golden QA Set requires a seed run to populate relevant_memory_ids."
        )
        return RAResult(
            precision_at_k=0.0,
            k=config.ra_k,
            query_count=0,
        )

    mean_precision = sum(r.precision_at_k for r in query_results) / len(query_results)

    # 找出最差 5 筆（按 precision_at_k 升序）
    worst = sorted(query_results, key=lambda r: r.precision_at_k)[:5]

    result = RAResult(
        precision_at_k=mean_precision,
        k=config.ra_k,
        query_count=len(query_results),
        query_results=query_results,
        worst_queries=worst,
    )

    logger.info(
        "RA Precision@%d: %.1f%% (%d queries evaluated)",
        config.ra_k, mean_precision * 100, len(query_results),
    )
    return result


def evaluate_ra_alerts(ra_result: RAResult) -> list[str]:
    """
    生成 RA 告警訊息

    告警門檻（依規格 §3.3.4）：
    - query_count = 0: WARNING（需要 seed run）
    - RA < 60%: CRITICAL（觸發向量模型評估）
    - RA < 70%: WARNING

    Returns:
        告警列表（空 = 無告警）
    """
    alerts: list[str] = []

    if ra_result.query_count == 0:
        alerts.append(
            "⚠️ WARNING: RA 無法計算 — Golden QA Set 的 relevant_memory_ids 全部為空，"
            "需執行 seed run 填入實際記憶 ID"
        )
    elif ra_result.precision_at_k < 0.60:
        alerts.append(
            f"🔴 CRITICAL: RA = {ra_result.precision_at_k:.1%} < 60% — "
            f"觸發向量模型評估 ({ra_result.query_count} queries)"
        )
    elif ra_result.precision_at_k < 0.70:
        alerts.append(
            f"⚠️ WARNING: RA = {ra_result.precision_at_k:.1%} < 70% "
            f"({ra_result.query_count} queries)"
        )

    return alerts
