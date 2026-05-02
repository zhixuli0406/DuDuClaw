"""
memory_eval/retention_rate.py
Retention Rate (RR) 計算器

依賴：
  - memory_snapshots 表（specs/memory-snapshots-migration-v1.md）
  - MemoryClient.search()

W21 Sprint 實作 — ENG-MEMORY
"""
from __future__ import annotations

import logging
from dataclasses import dataclass, field
from datetime import date, timedelta
from typing import Optional

import asyncpg

from .client import MemoryClient, SearchResult
from .config import EvalConfig

logger = logging.getLogger(__name__)


@dataclass
class SnapshotRecord:
    memory_id:        str
    content_hash:     str
    importance_score: float
    summary:          Optional[str]
    memory_layer:     Optional[str]
    snapshot_date:    date


@dataclass
class RRResult:
    observation_days: int
    recalled_count:   int
    total_count:      int
    retention_rate:   float              # 0.0 ~ 1.0
    details:          list[dict] = field(default_factory=list)

    @property
    def status(self) -> str:
        if self.retention_rate >= 0.85:
            return "✅ OK"
        elif self.retention_rate >= 0.80:
            return "⚠️ WARNING"
        else:
            return "🔴 CRITICAL"


async def compute_retention_rate(
    db_pool: asyncpg.Pool,
    memory_client: MemoryClient,
    config: EvalConfig,
) -> dict[int, RRResult]:
    """
    計算各天數的記憶留存率

    實作步驟（依規格 §3.2）：
    1. 從 memory_snapshots 取得 N 天前的高重要性記憶
    2. 對每條記憶以 summary 為 canonical query 執行 memory_search
    3. 取 top-5 cosine similarity 最高值
    4. 計算 RR(N)

    Returns:
        {7: RRResult, 30: RRResult}
    """
    results: dict[int, RRResult] = {}

    for n_days in config.rr_observation_days:
        baseline = await _get_historical_memories(
            db_pool=db_pool,
            agent_id=config.agent_id,
            days_ago=n_days,
            importance_threshold=config.rr_importance_threshold,
            max_records=config.rr_baseline_sample_size,
        )

        if not baseline:
            logger.warning(
                "No baseline memories for agent=%s, days_ago=%d. "
                "Ensure weekly snapshot has run.",
                config.agent_id, n_days,
            )
            results[n_days] = RRResult(
                observation_days=n_days,
                recalled_count=0,
                total_count=0,
                retention_rate=0.0,
                details=[],
            )
            continue

        recalled_count = 0
        detail_records: list[dict] = []

        for record in baseline:
            # canonical query：優先用 summary，否則用 memory:{id}
            canonical_query = record.summary or f"memory:{record.memory_id}"

            search_results: list[SearchResult] = await memory_client.search(
                query=canonical_query,
                limit=config.ra_k,  # top-5
            )

            max_similarity = max(
                (r.similarity for r in search_results),
                default=0.0,
            )
            recalled = max_similarity >= config.rr_recall_threshold

            if recalled:
                recalled_count += 1

            detail_records.append({
                "memory_id":       record.memory_id,
                "canonical_query": canonical_query[:50],
                "max_similarity":  round(max_similarity, 4),
                "recalled":        recalled,
                "layer":           record.memory_layer,
            })

        total = len(baseline)
        rr_value = recalled_count / total if total > 0 else 0.0

        results[n_days] = RRResult(
            observation_days=n_days,
            recalled_count=recalled_count,
            total_count=total,
            retention_rate=rr_value,
            details=detail_records,
        )

        logger.info(
            "RR(%dd): %.1f%% (%d/%d recalled)",
            n_days, rr_value * 100, recalled_count, total,
        )

    return results


async def _get_historical_memories(
    db_pool: asyncpg.Pool,
    agent_id: str,
    days_ago: int,
    importance_threshold: float = 0.7,
    max_records: int = 100,
) -> list[SnapshotRecord]:
    """
    從 memory_snapshots 取得 N 天前的高重要性記憶快照

    SQL 使用 idx_ms_agent_date_score（見 specs/memory-snapshots-migration-v1.md §2）
    """
    target_date = date.today() - timedelta(days=days_ago)

    async with db_pool.acquire() as conn:
        rows = await conn.fetch(
            """
            SELECT
                memory_id,
                content_hash,
                importance_score,
                summary,
                memory_layer,
                snapshot_date
            FROM memory_snapshots
            WHERE agent_id         = $1
              AND snapshot_date    = $2
              AND importance_score >= $3
            ORDER BY importance_score DESC
            LIMIT $4
            """,
            agent_id,
            target_date,
            importance_threshold,
            max_records,
        )

    return [
        SnapshotRecord(
            memory_id=row["memory_id"],
            content_hash=row["content_hash"],
            importance_score=row["importance_score"],
            summary=row["summary"],
            memory_layer=row["memory_layer"],
            snapshot_date=row["snapshot_date"],
        )
        for row in rows
    ]


def evaluate_rr_alerts(rr_results: dict[int, RRResult]) -> list[str]:
    """
    根據 RR 結果生成告警訊息

    告警門檻（依規格 §3.2.3）：
    - RR(7d) < 70%: CRITICAL
    - RR(7d) < 80%: WARNING
    - RR(30d) < 75%: WARNING

    Returns:
        告警列表（空 = 無告警）
    """
    alerts: list[str] = []
    rr_7  = rr_results.get(7)
    rr_30 = rr_results.get(30)

    if rr_7:
        if rr_7.retention_rate < 0.70:
            alerts.append(
                f"🔴 CRITICAL: RR(7d) = {rr_7.retention_rate:.1%} < 70% "
                f"(recalled {rr_7.recalled_count}/{rr_7.total_count}) — "
                f"觸發記憶整合診斷"
            )
        elif rr_7.retention_rate < 0.80:
            alerts.append(
                f"⚠️ WARNING: RR(7d) = {rr_7.retention_rate:.1%} < 80% "
                f"(recalled {rr_7.recalled_count}/{rr_7.total_count})"
            )

    if rr_30:
        if rr_30.retention_rate < 0.75:
            alerts.append(
                f"⚠️ WARNING: RR(30d) = {rr_30.retention_rate:.1%} < 75% "
                f"(recalled {rr_30.recalled_count}/{rr_30.total_count})"
            )

    return alerts
