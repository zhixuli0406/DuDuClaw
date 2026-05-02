"""
memory_eval/db/snapshots.py
memory_snapshots 表 CRUD 操作

Schema 依據：specs/memory-snapshots-migration-v1.md
用途：為 Retention Rate 計算提供歷史記憶快照

W21 Sprint 實作 — ENG-MEMORY
"""
from __future__ import annotations

import hashlib
import logging
from datetime import date, datetime, timezone
from typing import Optional

import asyncpg

from ..client import MemoryClient, Memory

logger = logging.getLogger(__name__)


async def take_memory_snapshot(
    db_pool: asyncpg.Pool,
    memory_client: MemoryClient,
    agent_id: str,
    snapshot_source: str = "weekly_cron",
    min_importance: float = 0.7,
    max_records: int = 500,
) -> dict:
    """
    對 agent 的高重要性記憶執行快照，寫入 memory_snapshots 表。

    執行時機：
    - 每週日 03:30 UTC（weekly_cron）
    - Retention Rate 評測前（pre_locomo_eval）
    - 手動觸發（manual）

    Returns:
        {"inserted": int, "batch_id": str, "snapshot_date": str}
    """
    import uuid

    batch_id = str(uuid.uuid4())
    snapshot_date = date.today()

    # 取得高重要性記憶列表
    memories: list[Memory] = await memory_client.list_important(
        agent_id=agent_id,
        min_importance=min_importance,
        limit=max_records,
    )

    if not memories:
        logger.info(
            "take_memory_snapshot: no memories with importance >= %.1f for agent=%s",
            min_importance, agent_id,
        )
        return {"inserted": 0, "batch_id": batch_id, "snapshot_date": str(snapshot_date)}

    # 批次 upsert（ON CONFLICT DO NOTHING 避免重複快照）
    inserted_count = 0
    async with db_pool.acquire() as conn:
        async with conn.transaction():
            for mem in memories:
                content_hash = _sha256_hash(mem.content)
                summary = (mem.summary or mem.content)[:200]

                result = await conn.execute(
                    """
                    INSERT INTO memory_snapshots (
                        agent_id,
                        snapshot_date,
                        batch_id,
                        memory_id,
                        content_hash,
                        importance_score,
                        summary,
                        memory_layer,
                        snapshot_source
                    ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                    ON CONFLICT (agent_id, snapshot_date, memory_id) DO NOTHING
                    """,
                    agent_id,
                    snapshot_date,
                    batch_id,
                    mem.memory_id,
                    content_hash,
                    mem.importance_score,
                    summary,
                    mem.layer,
                    snapshot_source,
                )
                # asyncpg returns "INSERT 0 N" string; count successes
                if result == "INSERT 0 1":
                    inserted_count += 1

    logger.info(
        "take_memory_snapshot: inserted=%d, batch_id=%s, agent=%s",
        inserted_count, batch_id, agent_id,
    )
    return {
        "inserted":      inserted_count,
        "batch_id":      batch_id,
        "snapshot_date": str(snapshot_date),
        "total_fetched": len(memories),
    }


async def cleanup_old_snapshots(
    db_pool: asyncpg.Pool,
    retention_days: int = 90,
) -> int:
    """
    刪除超過 retention_days 天的舊快照（GDPR 合規保留政策）

    Returns:
        刪除筆數
    """
    from datetime import timedelta
    cutoff_date = date.today() - timedelta(days=retention_days)

    async with db_pool.acquire() as conn:
        result = await conn.execute(
            "DELETE FROM memory_snapshots WHERE snapshot_date < $1",
            cutoff_date,
        )

    # result = "DELETE N"
    deleted = int(result.split()[-1]) if result.startswith("DELETE") else 0
    logger.info("cleanup_old_snapshots: deleted=%d (older than %s)", deleted, cutoff_date)
    return deleted


async def get_snapshot_stats(
    db_pool: asyncpg.Pool,
    agent_id: str,
) -> dict:
    """
    取得快照統計資訊（最新快照日期、總筆數）

    Returns:
        {"latest_date": str | None, "total_records": int, "distinct_dates": int}
    """
    async with db_pool.acquire() as conn:
        row = await conn.fetchrow(
            """
            SELECT
                MAX(snapshot_date)  AS latest_date,
                COUNT(*)            AS total_records,
                COUNT(DISTINCT snapshot_date) AS distinct_dates
            FROM memory_snapshots
            WHERE agent_id = $1
            """,
            agent_id,
        )

    return {
        "latest_date":    str(row["latest_date"]) if row["latest_date"] else None,
        "total_records":  row["total_records"],
        "distinct_dates": row["distinct_dates"],
    }


def _sha256_hash(content: str) -> str:
    """計算內容的 SHA256 hash（用於 content_hash 欄位）"""
    return hashlib.sha256(content.encode("utf-8")).hexdigest()
