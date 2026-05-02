"""
memory_eval/cron_runner.py
評測系統 Cron Job 主入口

對接 DuDuClaw schedule_task 系統
CLI 用法：python -m memory_eval.cron_runner [smoke_test|weekly_kpis|monthly_locomo]

Cron 排程（依規格 §4.1）：
  Daily Smoke Test    → 每日 03:00 UTC
  Memory Snapshot     → 每週日 03:30 UTC
  Weekly Native KPIs  → 每週日 04:00 UTC
  Monthly LOCOMO      → 每月 1 日 04:00 UTC（P3）

W21 Sprint 實作 — ENG-MEMORY
"""
from __future__ import annotations

import asyncio
import logging
import os
import sys
from datetime import datetime, timezone

import asyncpg

from .config import EvalConfig, LOCOMOConfig
from .smoke_test import run_smoke_test
from .retention_rate import compute_retention_rate, evaluate_rr_alerts
from .retrieval_accuracy import compute_retrieval_accuracy, evaluate_ra_alerts
from .db.snapshots import take_memory_snapshot

logger = logging.getLogger(__name__)


async def run_daily_smoke_test(agent_id: str, db_dsn: str) -> dict:
    """
    Daily Cron：03:00 UTC
    執行 3 個 Smoke Test 案例，回傳結果摘要

    Returns:
        {run_id, summary, passed, results, timestamp}
    """
    from .client import build_client

    config = EvalConfig(agent_id=agent_id, db_dsn=db_dsn)
    client = build_client(config)

    report = await run_smoke_test(client, agent_id)

    if not report.all_passed:
        logger.error("Smoke Test FAILED: %s", report.summary)

    return {
        "run_id":    report.run_id,
        "summary":   report.summary,
        "passed":    report.all_passed,
        "results":   [
            {
                "name":   r.test_name,
                "passed": r.passed,
                "detail": r.detail,
                "ms":     r.duration_ms,
            }
            for r in report.results
        ],
        "timestamp": datetime.now(timezone.utc).isoformat(),
    }


async def run_weekly_native_kpis(agent_id: str, db_dsn: str) -> dict:
    """
    Weekly Cron：每週日 03:30 UTC 先快照，04:00 UTC 評測
    執行 RR + RA（TC + EPR 為 P2，W22）

    Returns:
        {snapshot, retention_rate, retrieval_accuracy, alerts, timestamp}
    """
    from .client import build_client

    config = EvalConfig(agent_id=agent_id, db_dsn=db_dsn)
    client = build_client(config)
    db_pool = await asyncpg.create_pool(db_dsn)

    all_alerts: list[str] = []
    results: dict = {}

    try:
        # Phase 1: 記憶快照（RR 前置，每週日 03:30 UTC）
        snapshot_result = await take_memory_snapshot(
            db_pool=db_pool,
            memory_client=client,
            agent_id=agent_id,
            snapshot_source="weekly_cron",
        )
        results["snapshot"] = snapshot_result
        logger.info("Weekly snapshot: inserted=%d", snapshot_result.get("inserted", 0))

        # Phase 2: Retention Rate
        rr_results = await compute_retention_rate(db_pool, client, config)
        results["retention_rate"] = {
            n: {
                "rate":     r.retention_rate,
                "recalled": r.recalled_count,
                "total":    r.total_count,
                "status":   r.status,
            }
            for n, r in rr_results.items()
        }
        all_alerts.extend(evaluate_rr_alerts(rr_results))

        # Phase 3: Retrieval Accuracy
        ra_result = await compute_retrieval_accuracy(client, config)
        results["retrieval_accuracy"] = {
            "precision_at_k": ra_result.precision_at_k,
            "k":              ra_result.k,
            "query_count":    ra_result.query_count,
            "status":         ra_result.status,
        }
        all_alerts.extend(evaluate_ra_alerts(ra_result))

        # Phase 4-5: TC / EPR → P2, W22
        results["temporal_consistency"]   = {"status": "not_implemented", "note": "P2, W22"}
        results["episodic_pressure_resp"] = {"status": "not_implemented", "note": "P2, W22"}

        results["alerts"]    = all_alerts
        results["timestamp"] = datetime.now(timezone.utc).isoformat()

        if all_alerts:
            logger.warning("Weekly KPIs — %d alert(s):\n%s", len(all_alerts), "\n".join(all_alerts))
        else:
            logger.info("Weekly KPIs — All metrics within thresholds.")

    finally:
        await db_pool.close()

    return results


async def run_monthly_locomo(agent_id: str, db_dsn: str) -> dict:
    """
    Monthly Cron：每月 1 日 04:00 UTC
    執行 LOCOMO Full Benchmark（P3，需資料集就緒）
    """
    # P3 實作佔位：W21 週末驗收目標不含此項
    return {
        "status":    "not_implemented",
        "note":      "LOCOMO Full Benchmark is P3, scheduled for W22 after dataset download.",
        "timestamp": datetime.now(timezone.utc).isoformat(),
    }


def _get_required_env(key: str) -> str:
    """讀取必要環境變數，不存在則 raise"""
    value = os.environ.get(key, "")
    if not value:
        raise RuntimeError(
            f"Required environment variable '{key}' is not set. "
            "Check .env or deployment config."
        )
    return value


def main() -> None:
    """CLI 入口：python -m memory_eval.cron_runner <command>"""
    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s %(levelname)s %(name)s — %(message)s",
    )

    if len(sys.argv) < 2:
        print("Usage: python -m memory_eval.cron_runner <smoke_test|weekly_kpis|monthly_locomo>")
        sys.exit(1)

    command = sys.argv[1]
    agent_id = os.environ.get("EVAL_AGENT_ID", "duduclaw-eng-memory")
    db_dsn = _get_required_env("DATABASE_DSN")

    if command == "smoke_test":
        result = asyncio.run(run_daily_smoke_test(agent_id, db_dsn))
    elif command == "weekly_kpis":
        result = asyncio.run(run_weekly_native_kpis(agent_id, db_dsn))
    elif command == "monthly_locomo":
        result = asyncio.run(run_monthly_locomo(agent_id, db_dsn))
    else:
        print(f"Unknown command: {command}")
        sys.exit(1)

    import json
    print(json.dumps(result, indent=2, ensure_ascii=False))


if __name__ == "__main__":
    main()
