"""
tests/test_cron_runner.py
cron_runner.py unit tests — mock 所有外部依賴（asyncpg、build_client）

目標：cron_runner.py 覆蓋率從 0% 提升至 ≥ 70%

W21 Sprint — ENG-MEMORY
"""
from __future__ import annotations

import asyncio
import os
import sys
from datetime import datetime, timezone
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

# ---------------------------------------------------------------------------
# run_monthly_locomo（最簡單，無外部依賴）
# ---------------------------------------------------------------------------

@pytest.mark.asyncio
async def test_run_monthly_locomo_returns_not_implemented():
    """run_monthly_locomo 應回傳 not_implemented 佔位結果"""
    from duduclaw.memory_eval.cron_runner import run_monthly_locomo

    result = await run_monthly_locomo("test-agent", "postgresql://localhost/test")
    assert result["status"] == "not_implemented"
    assert "P3" in result["note"]
    assert "timestamp" in result


@pytest.mark.asyncio
async def test_run_monthly_locomo_timestamp_format():
    """timestamp 應為 ISO8601 格式"""
    from duduclaw.memory_eval.cron_runner import run_monthly_locomo

    result = await run_monthly_locomo("test-agent", "dsn")
    ts = result["timestamp"]
    # 驗證可被解析
    parsed = datetime.fromisoformat(ts.replace("Z", "+00:00"))
    assert parsed.tzinfo is not None


# ---------------------------------------------------------------------------
# _get_required_env
# ---------------------------------------------------------------------------

def test_get_required_env_success():
    """環境變數存在時回傳值"""
    from duduclaw.memory_eval.cron_runner import _get_required_env

    with patch.dict(os.environ, {"TEST_KEY_ABC": "test-value-xyz"}):
        assert _get_required_env("TEST_KEY_ABC") == "test-value-xyz"


def test_get_required_env_missing_raises():
    """環境變數不存在時 raise RuntimeError"""
    from duduclaw.memory_eval.cron_runner import _get_required_env

    env_without_key = {k: v for k, v in os.environ.items() if k != "MISSING_XYZ_KEY"}
    with patch.dict(os.environ, env_without_key, clear=True):
        with pytest.raises(RuntimeError, match="MISSING_XYZ_KEY"):
            _get_required_env("MISSING_XYZ_KEY")


def test_get_required_env_empty_string_raises():
    """環境變數為空字串時 raise RuntimeError"""
    from duduclaw.memory_eval.cron_runner import _get_required_env

    with patch.dict(os.environ, {"EMPTY_KEY": ""}):
        with pytest.raises(RuntimeError):
            _get_required_env("EMPTY_KEY")


# ---------------------------------------------------------------------------
# run_daily_smoke_test（mock build_client + run_smoke_test）
# ---------------------------------------------------------------------------

def _make_smoke_report(all_passed: bool = True, run_id: str = "test-uuid"):
    """建立 mock SmokeTestReport"""
    from duduclaw.memory_eval.smoke_test import SmokeTestReport, SmokeTestResult

    report = SmokeTestReport(run_id=run_id)
    report.results = [
        SmokeTestResult(
            test_name="basic_store_and_retrieve",
            passed=all_passed,
            duration_ms=120,
            detail="TC-1 PASS" if all_passed else "TC-1 FAIL",
        )
    ]
    report.passed_count = 1 if all_passed else 0
    report.failed_count = 0 if all_passed else 1
    report.total_ms = 500
    return report


@pytest.mark.asyncio
async def test_run_daily_smoke_test_all_passed():
    """全部通過 → result['passed'] = True"""
    from duduclaw.memory_eval.cron_runner import run_daily_smoke_test

    mock_report = _make_smoke_report(all_passed=True, run_id="uuid-pass")
    mock_client = MagicMock()

    with patch("duduclaw.memory_eval.client.build_client", return_value=mock_client), \
         patch("duduclaw.memory_eval.cron_runner.run_smoke_test", new=AsyncMock(return_value=mock_report)):
        result = await run_daily_smoke_test("test-agent", "postgresql://localhost/test")

    assert result["passed"] is True
    assert result["run_id"] == "uuid-pass"
    assert isinstance(result["results"], list)
    assert len(result["results"]) == 1
    assert "timestamp" in result


@pytest.mark.asyncio
async def test_run_daily_smoke_test_partial_fail():
    """部分失敗 → result['passed'] = False"""
    from duduclaw.memory_eval.cron_runner import run_daily_smoke_test

    mock_report = _make_smoke_report(all_passed=False, run_id="uuid-fail")
    mock_client = MagicMock()

    with patch("duduclaw.memory_eval.client.build_client", return_value=mock_client), \
         patch("duduclaw.memory_eval.cron_runner.run_smoke_test", new=AsyncMock(return_value=mock_report)):
        result = await run_daily_smoke_test("test-agent", "dsn")

    assert result["passed"] is False
    assert result["run_id"] == "uuid-fail"


@pytest.mark.asyncio
async def test_run_daily_smoke_test_result_shape():
    """result['results'] 每筆含 name / passed / detail / ms"""
    from duduclaw.memory_eval.cron_runner import run_daily_smoke_test

    mock_report = _make_smoke_report(all_passed=True, run_id="uuid-shape")
    mock_client = MagicMock()

    with patch("duduclaw.memory_eval.client.build_client", return_value=mock_client), \
         patch("duduclaw.memory_eval.cron_runner.run_smoke_test", new=AsyncMock(return_value=mock_report)):
        result = await run_daily_smoke_test("agent", "dsn")

    item = result["results"][0]
    assert "name"   in item
    assert "passed" in item
    assert "detail" in item
    assert "ms"     in item


# ---------------------------------------------------------------------------
# run_weekly_native_kpis（mock asyncpg.create_pool + build_client + sub-functions）
# ---------------------------------------------------------------------------

def _make_rr_results():
    """建立 mock RR 結果"""
    from duduclaw.memory_eval.retention_rate import RRResult

    return {
        7:  RRResult(observation_days=7,  recalled_count=90, total_count=100, retention_rate=0.90),
        30: RRResult(observation_days=30, recalled_count=85, total_count=100, retention_rate=0.85),
    }


def _make_ra_result():
    """建立 mock RA 結果"""
    from duduclaw.memory_eval.retrieval_accuracy import RAResult

    return RAResult(
        precision_at_k=0.80,
        k=5,
        query_count=200,
    )


@pytest.mark.asyncio
async def test_run_weekly_native_kpis_success():
    """週報 KPI 成功回傳，含 retention_rate / retrieval_accuracy / alerts"""
    from duduclaw.memory_eval.cron_runner import run_weekly_native_kpis

    mock_pool = AsyncMock()
    mock_pool.close = AsyncMock()
    mock_client = MagicMock()

    rr_results = _make_rr_results()
    ra_result   = _make_ra_result()

    with patch("duduclaw.memory_eval.cron_runner.asyncpg.create_pool", new=AsyncMock(return_value=mock_pool)), \
         patch("duduclaw.memory_eval.client.build_client", return_value=mock_client), \
         patch("duduclaw.memory_eval.cron_runner.take_memory_snapshot", new=AsyncMock(return_value={"inserted": 42})), \
         patch("duduclaw.memory_eval.cron_runner.compute_retention_rate", new=AsyncMock(return_value=rr_results)), \
         patch("duduclaw.memory_eval.cron_runner.compute_retrieval_accuracy", new=AsyncMock(return_value=ra_result)):

        result = await run_weekly_native_kpis("test-agent", "dsn")

    assert "retention_rate" in result
    assert "retrieval_accuracy" in result
    assert "alerts" in result
    assert "timestamp" in result
    # 指標值正確
    assert result["retention_rate"][7]["rate"] == pytest.approx(0.90)
    assert result["retrieval_accuracy"]["precision_at_k"] == pytest.approx(0.80)
    # 正常值域無告警
    assert len(result["alerts"]) == 0


@pytest.mark.asyncio
async def test_run_weekly_native_kpis_generates_alerts():
    """RR 低於門檻時 alerts 非空"""
    from duduclaw.memory_eval.cron_runner import run_weekly_native_kpis
    from duduclaw.memory_eval.retention_rate import RRResult
    from duduclaw.memory_eval.retrieval_accuracy import RAResult

    mock_pool = AsyncMock()
    mock_pool.close = AsyncMock()
    mock_client = MagicMock()

    # RR(7d) = 65% → CRITICAL 告警
    low_rr = {
        7: RRResult(observation_days=7, recalled_count=65, total_count=100, retention_rate=0.65),
    }
    ok_ra = RAResult(precision_at_k=0.80, k=5, query_count=200)

    with patch("duduclaw.memory_eval.cron_runner.asyncpg.create_pool", new=AsyncMock(return_value=mock_pool)), \
         patch("duduclaw.memory_eval.client.build_client", return_value=mock_client), \
         patch("duduclaw.memory_eval.cron_runner.take_memory_snapshot", new=AsyncMock(return_value={"inserted": 10})), \
         patch("duduclaw.memory_eval.cron_runner.compute_retention_rate", new=AsyncMock(return_value=low_rr)), \
         patch("duduclaw.memory_eval.cron_runner.compute_retrieval_accuracy", new=AsyncMock(return_value=ok_ra)):

        result = await run_weekly_native_kpis("test-agent", "dsn")

    assert len(result["alerts"]) >= 1
    assert any("CRITICAL" in a or "WARNING" in a for a in result["alerts"])


@pytest.mark.asyncio
async def test_run_weekly_native_kpis_pool_closed_on_exception():
    """即使中途出現例外，db_pool.close() 仍被呼叫（finally 保證）"""
    from duduclaw.memory_eval.cron_runner import run_weekly_native_kpis

    mock_pool = AsyncMock()
    mock_pool.close = AsyncMock()
    mock_client = MagicMock()

    with patch("duduclaw.memory_eval.cron_runner.asyncpg.create_pool", new=AsyncMock(return_value=mock_pool)), \
         patch("duduclaw.memory_eval.client.build_client", return_value=mock_client), \
         patch("duduclaw.memory_eval.cron_runner.take_memory_snapshot", new=AsyncMock(side_effect=RuntimeError("DB error"))):

        with pytest.raises(RuntimeError, match="DB error"):
            await run_weekly_native_kpis("test-agent", "dsn")

    mock_pool.close.assert_called_once()


@pytest.mark.asyncio
async def test_run_weekly_native_kpis_tc_epr_not_implemented():
    """TC / EPR 回傳 not_implemented（P2 佔位）"""
    from duduclaw.memory_eval.cron_runner import run_weekly_native_kpis

    mock_pool = AsyncMock()
    mock_pool.close = AsyncMock()
    mock_client = MagicMock()

    with patch("duduclaw.memory_eval.cron_runner.asyncpg.create_pool", new=AsyncMock(return_value=mock_pool)), \
         patch("duduclaw.memory_eval.client.build_client", return_value=mock_client), \
         patch("duduclaw.memory_eval.cron_runner.take_memory_snapshot", new=AsyncMock(return_value={"inserted": 0})), \
         patch("duduclaw.memory_eval.cron_runner.compute_retention_rate", new=AsyncMock(return_value={})), \
         patch("duduclaw.memory_eval.cron_runner.compute_retrieval_accuracy",
               new=AsyncMock(return_value=_make_ra_result())):

        result = await run_weekly_native_kpis("test-agent", "dsn")

    assert result["temporal_consistency"]["status"] == "not_implemented"
    assert result["episodic_pressure_resp"]["status"] == "not_implemented"


# ---------------------------------------------------------------------------
# main() CLI 入口
# ---------------------------------------------------------------------------

def test_main_no_args_exits():
    """無 subcommand → sys.exit(1)"""
    from duduclaw.memory_eval.cron_runner import main

    with patch.object(sys, "argv", ["cron_runner"]), \
         pytest.raises(SystemExit) as exc:
        main()

    assert exc.value.code == 1


def test_main_unknown_command_exits():
    """未知 subcommand → sys.exit(1)"""
    from duduclaw.memory_eval.cron_runner import main

    with patch.object(sys, "argv", ["cron_runner", "unknown_cmd"]), \
         patch.dict(os.environ, {"DATABASE_DSN": "postgresql://test", "EVAL_AGENT_ID": "agent"}), \
         pytest.raises(SystemExit) as exc:
        main()

    assert exc.value.code == 1


def test_main_smoke_test_command():
    """smoke_test subcommand 正常執行並輸出 JSON"""
    from duduclaw.memory_eval.cron_runner import main
    from duduclaw.memory_eval.smoke_test import SmokeTestReport

    mock_report = SmokeTestReport(run_id="cli-uuid")
    mock_report.passed_count = 1
    mock_report.failed_count = 0
    mock_report.total_ms = 100

    with patch.object(sys, "argv", ["cron_runner", "smoke_test"]), \
         patch.dict(os.environ, {"DATABASE_DSN": "postgresql://test", "EVAL_AGENT_ID": "agent"}), \
         patch("duduclaw.memory_eval.cron_runner.asyncio.run") as mock_run:

        # asyncio.run 回傳 mock 結果（避免實際執行 async）
        mock_run.return_value = {
            "run_id": "cli-uuid", "summary": "Smoke Test: 1/1 passed",
            "passed": True, "results": [], "timestamp": "2026-05-01T00:00:00+00:00",
        }
        main()

    mock_run.assert_called_once()


def test_main_monthly_locomo_command():
    """monthly_locomo subcommand 正常執行"""
    from duduclaw.memory_eval.cron_runner import main

    with patch.object(sys, "argv", ["cron_runner", "monthly_locomo"]), \
         patch.dict(os.environ, {"DATABASE_DSN": "postgresql://test"}), \
         patch("duduclaw.memory_eval.cron_runner.asyncio.run") as mock_run:

        mock_run.return_value = {
            "status": "not_implemented", "note": "P3",
            "timestamp": "2026-05-01T00:00:00+00:00",
        }
        main()

    mock_run.assert_called_once()


def test_main_missing_database_dsn_raises():
    """DATABASE_DSN 未設定 → RuntimeError"""
    from duduclaw.memory_eval.cron_runner import main

    env_without_dsn = {k: v for k, v in os.environ.items() if k != "DATABASE_DSN"}
    with patch.object(sys, "argv", ["cron_runner", "smoke_test"]), \
         patch.dict(os.environ, env_without_dsn, clear=True):
        with pytest.raises(RuntimeError, match="DATABASE_DSN"):
            main()


def test_main_weekly_kpis_command():
    """weekly_kpis subcommand 應呼叫 asyncio.run 一次"""
    from duduclaw.memory_eval.cron_runner import main

    with patch.object(sys, "argv", ["cron_runner", "weekly_kpis"]), \
         patch.dict(os.environ, {"DATABASE_DSN": "postgresql://test", "EVAL_AGENT_ID": "agent"}), \
         patch("duduclaw.memory_eval.cron_runner.asyncio.run") as mock_run:

        mock_run.return_value = {
            "retention_rate": {},
            "retrieval_accuracy": {"precision_at_k": 0.85, "k": 5, "query_count": 50, "status": "✅ OK"},
            "alerts": [],
            "timestamp": "2026-05-01T00:00:00+00:00",
        }
        main()

    mock_run.assert_called_once()


def test_main_module_entry_calls_main():
    """__main__ 模組入口應能執行 main() 並被 asyncio.run 呼叫"""
    from duduclaw.memory_eval.cron_runner import main

    # 驗證 weekly_kpis 的 asyncio.run 分支（line 181）確實執行
    with patch.object(sys, "argv", ["cron_runner", "weekly_kpis"]), \
         patch.dict(os.environ, {"DATABASE_DSN": "postgresql://test"}), \
         patch("duduclaw.memory_eval.cron_runner.asyncio.run") as mock_run:

        mock_run.return_value = {
            "alerts": [], "timestamp": "2026-05-01T00:00:00+00:00",
        }
        main()

    # 確認 run_weekly_native_kpis 協程被作為 asyncio.run 的參數
    assert mock_run.call_count == 1
    call_arg = mock_run.call_args[0][0]
    # asyncio.run 的第一個參數應為 coroutine（run_weekly_native_kpis 的回傳值）
    import inspect
    assert inspect.iscoroutine(call_arg)
    call_arg.close()  # 清理未被 await 的協程
