"""
memory_eval/db/consolidation.py
memory_consolidation_log 表 CRUD 操作（Stub — P2 W22）

Schema 依據：specs/consolidation-log-schema-v1.md (v1.1，含 TL 審查修正)
用途：為 Episodic Pressure Response (EPR) 指標提供整合事件記錄

W21 Sprint：僅提供 schema 介面與 stub 實作
P2 完整實作：W22 Sprint

注意：v1.1 關鍵修正
  - Critical-1: source/result memory_count 改為 Generated Columns（DB-enforced）
  - Critical-2: status='completed'/'failed' 加入 CHECK constraints
"""
from __future__ import annotations

import logging
import uuid
from datetime import datetime, timezone
from typing import Optional

import asyncpg

logger = logging.getLogger(__name__)


async def log_consolidation_start(
    db_pool: asyncpg.Pool,
    agent_id: str,
    trigger_reason: str,
    pressure_before: float,
    source_memory_ids: list[str],
    algorithm_version: str = "v1.0",
) -> str:
    """
    記錄整合事件開始（status='running'）

    Args:
        trigger_reason: 'episodic_pressure_threshold' | 'manual' | 'scheduled'
        pressure_before: 整合前的 episodic pressure（0.0~1.0 已正規化）
        source_memory_ids: 來源 episodic memory IDs

    Returns:
        consolidation_id (UUID)
    """
    consolidation_id = str(uuid.uuid4())
    now = datetime.now(timezone.utc)

    async with db_pool.acquire() as conn:
        await conn.execute(
            """
            INSERT INTO memory_consolidation_log (
                id,
                agent_id,
                triggered_at,
                trigger_reason,
                pressure_before,
                source_memory_ids,
                status,
                consolidation_algorithm_version
            ) VALUES ($1, $2, $3, $4, $5, $6, 'running', $7)
            """,
            consolidation_id,
            agent_id,
            now,
            trigger_reason,
            pressure_before,
            source_memory_ids,
            algorithm_version,
        )

    logger.info(
        "log_consolidation_start: id=%s, agent=%s, trigger=%s",
        consolidation_id, agent_id, trigger_reason,
    )
    return consolidation_id


async def log_consolidation_complete(
    db_pool: asyncpg.Pool,
    consolidation_id: str,
    result_memory_ids: list[str],
    pressure_after: float,
    duration_ms: int,
) -> None:
    """
    更新整合事件完成（status='completed'）

    注意：CHECK constraint 要求 status='completed' 時
    completed_at 和 result_memory_ids 不得為 NULL（v1.1）
    """
    now = datetime.now(timezone.utc)

    async with db_pool.acquire() as conn:
        await conn.execute(
            """
            UPDATE memory_consolidation_log
            SET status             = 'completed',
                completed_at       = $1,
                result_memory_ids  = $2,
                pressure_after     = $3,
                duration_ms        = $4
            WHERE id = $5
            """,
            now,
            result_memory_ids,
            pressure_after,
            duration_ms,
            consolidation_id,
        )

    logger.info(
        "log_consolidation_complete: id=%s, result_count=%d, duration_ms=%d",
        consolidation_id, len(result_memory_ids), duration_ms,
    )


async def log_consolidation_failed(
    db_pool: asyncpg.Pool,
    consolidation_id: str,
    error_message: str,
    duration_ms: int,
) -> None:
    """
    更新整合事件失敗（status='failed'）

    注意：CHECK constraint 要求 status='failed' 時
    error_message 不得為 NULL（v1.1）
    """
    now = datetime.now(timezone.utc)

    async with db_pool.acquire() as conn:
        await conn.execute(
            """
            UPDATE memory_consolidation_log
            SET status        = 'failed',
                completed_at  = $1,
                error_message = $2,
                duration_ms   = $3
            WHERE id = $4
            """,
            now,
            error_message,
            duration_ms,
            consolidation_id,
        )

    logger.warning(
        "log_consolidation_failed: id=%s, error=%s",
        consolidation_id, error_message,
    )


async def get_recent_consolidation_events(
    db_pool: asyncpg.Pool,
    agent_id: str,
    lookback_days: int = 7,
    status: Optional[str] = "completed",
) -> list[dict]:
    """
    取得最近 N 天的整合事件（EPR 計算用）

    Returns:
        consolidation event dict list（含 epr_* cache columns）
    """
    from datetime import timedelta, date
    since = datetime.now(timezone.utc) - __import__("datetime").timedelta(days=lookback_days)

    async with db_pool.acquire() as conn:
        rows = await conn.fetch(
            """
            SELECT
                id,
                triggered_at,
                trigger_reason,
                pressure_before,
                pressure_after,
                source_memory_ids,
                result_memory_ids,
                source_memory_count,
                result_memory_count,
                duration_ms,
                epr_quality_score,
                epr_information_retention,
                epr_compression_ratio_score,
                epr_novelty_penalty
            FROM memory_consolidation_log
            WHERE agent_id     = $1
              AND triggered_at >= $2
              AND ($3::text IS NULL OR status = $3)
            ORDER BY triggered_at DESC
            """,
            agent_id,
            since,
            status,
        )

    return [dict(row) for row in rows]
