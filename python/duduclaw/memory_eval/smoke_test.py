"""
memory_eval/smoke_test.py
Daily Smoke Test P0 — 驗證記憶系統基本功能正常

執行頻率：每日 03:00 UTC
執行時限：5 分鐘

W21 Sprint 實作 v1.1（含 TL review 修正）
作者：ENG-MEMORY (duduclaw-eng-memory)
任務：0b5c478e-729d-46f7-9408-a2f281c605f4
"""
from __future__ import annotations

import asyncio
import logging
import time
import uuid
from dataclasses import dataclass, field
from typing import Optional

from .client import MemoryClient
from .config import EvalConfig

logger = logging.getLogger(__name__)


@dataclass
class SmokeTestResult:
    test_name:   str
    passed:      bool
    duration_ms: int
    detail:      str
    error:       Optional[str] = None


@dataclass
class SmokeTestReport:
    run_id:        str                  = field(default_factory=lambda: str(uuid.uuid4()))
    results:       list[SmokeTestResult] = field(default_factory=list)
    total_ms:      int                  = 0
    passed_count:  int                  = 0
    failed_count:  int                  = 0
    skipped_count: int                  = 0

    @property
    def all_passed(self) -> bool:
        return self.failed_count == 0

    @property
    def summary(self) -> str:
        total = self.passed_count + self.failed_count + self.skipped_count
        return (
            f"Smoke Test: {self.passed_count}/{total} passed "
            f"({self.skipped_count} skipped) in {self.total_ms}ms"
        )


async def run_smoke_test(
    memory_client: MemoryClient,
    agent_id: str,
) -> SmokeTestReport:
    """
    執行所有 Smoke Test 案例

    Returns:
        SmokeTestReport — 含所有測試結果的完整報告
    """
    report = SmokeTestReport()
    start_time = time.time()

    # TC-1: basic_store_and_retrieve
    result = await _tc_basic_store_and_retrieve(memory_client)
    report.results.append(result)

    # TC-2: memory_isolation
    result = await _tc_memory_isolation(memory_client)
    report.results.append(result)

    # TC-3: episodic_pressure_check
    result = await _tc_episodic_pressure_check(memory_client)
    report.results.append(result)

    # 統計
    for r in report.results:
        if r.passed and r.error is None:
            if "SKIP" in (r.detail or ""):
                report.skipped_count += 1
            else:
                report.passed_count += 1
        else:
            report.failed_count += 1

    report.total_ms = int((time.time() - start_time) * 1000)
    return report


async def _tc_basic_store_and_retrieve(client: MemoryClient) -> SmokeTestResult:
    """
    TC-1：store_and_retrieve
    v1.1 修正：使用純淨短句 + 含關鍵字重疊的查詢
    """
    test_name = "basic_store_and_retrieve"
    start_time = time.time()

    # 生成唯一測試 ID，確保可精確召回
    test_token = f"SMOKETEST_{int(time.time())}"
    store_content = f"{test_token} smoke_test_user 最愛的食物是拉麵"

    try:
        # Step 1: Store
        mem_id = await client.store(
            content=store_content,
            tags=["smoke-test", "temp", "locomo-eval"],
            namespace="smoke_test_ns",
        )

        # 短暫等待索引更新
        await asyncio.sleep(0.5)

        # Step 2a: Retrieve with unique token（精確召回，驗證 store 成功）
        results_exact = await client.search(
            query=test_token,
            limit=5,
            namespace="smoke_test_ns",
        )
        exact_found = any(test_token in r.content for r in results_exact)

        # Step 2b: Retrieve with keyword-overlap query（語意 + 關鍵字重疊）
        results_semantic = await client.search(
            query="smoke_test_user 愛吃拉麵嗎",
            limit=5,
            namespace="smoke_test_ns",
        )
        semantic_found = any(test_token in r.content for r in results_semantic)

        duration_ms = int((time.time() - start_time) * 1000)

        if exact_found:
            detail = (
                f"✅ TC-1 PASS: store OK, exact retrieve OK"
                f"{' / semantic retrieve PARTIAL' if not semantic_found else ''}"
            )
            return SmokeTestResult(
                test_name=test_name,
                passed=True,
                duration_ms=duration_ms,
                detail=detail,
            )
        else:
            return SmokeTestResult(
                test_name=test_name,
                passed=False,
                duration_ms=duration_ms,
                detail="❌ TC-1 FAIL: memory stored but cannot be retrieved",
                error=f"exact_found={exact_found}, semantic_found={semantic_found}",
            )

    except Exception as e:
        duration_ms = int((time.time() - start_time) * 1000)
        return SmokeTestResult(
            test_name=test_name,
            passed=False,
            duration_ms=duration_ms,
            detail=f"❌ TC-1 ERROR: {type(e).__name__}",
            error=str(e),
        )


async def _tc_memory_isolation(client: MemoryClient) -> SmokeTestResult:
    """
    TC-2：memory_isolation
    v1.1：在整合測試層驗證 namespace 隔離；不支援時 SKIP
    """
    test_name = "memory_isolation"
    start_time = time.time()

    try:
        # 在 NS-A 存入記憶
        token_a = f"ISOLATION_A_{int(time.time())}"
        await client.store(
            content=f"{token_a} this memory belongs to namespace A",
            tags=["smoke-test", "temp"],
            namespace="isolation_ns_a",
        )

        await asyncio.sleep(0.5)

        # 在 NS-B 查詢 NS-A 的 token，期望 0 結果
        results = await client.search(
            query=token_a,
            limit=5,
            namespace="isolation_ns_b",  # 不同 namespace
        )

        isolated = not any(token_a in r.content for r in results)
        duration_ms = int((time.time() - start_time) * 1000)

        if isolated:
            return SmokeTestResult(
                test_name=test_name,
                passed=True,
                duration_ms=duration_ms,
                detail="✅ TC-2 PASS: cross-namespace isolation verified",
            )
        else:
            return SmokeTestResult(
                test_name=test_name,
                passed=False,
                duration_ms=duration_ms,
                detail="❌ TC-2 FAIL: cross-namespace leakage detected",
                error=f"Found {len(results)} results in NS-B that should be isolated",
            )

    except NotImplementedError:
        duration_ms = int((time.time() - start_time) * 1000)
        return SmokeTestResult(
            test_name=test_name,
            passed=True,
            duration_ms=duration_ms,
            detail="⏭️ TC-2 SKIP: namespace not supported by current client",
        )
    except Exception as e:
        duration_ms = int((time.time() - start_time) * 1000)
        return SmokeTestResult(
            test_name=test_name,
            passed=False,
            duration_ms=duration_ms,
            detail=f"❌ TC-2 ERROR: {type(e).__name__}",
            error=str(e),
        )


async def _tc_episodic_pressure_check(client: MemoryClient) -> SmokeTestResult:
    """
    TC-3：episodic_pressure_check
    v1.1：回傳值域為 0~10+，正規化至 0~1 後記錄
    """
    test_name = "episodic_pressure_check"
    start_time = time.time()

    try:
        raw_pressure = await client.get_episodic_pressure(hours_ago=24)

        if not isinstance(raw_pressure, (int, float)):
            raise ValueError(f"Expected numeric, got {type(raw_pressure)}")

        if raw_pressure < 0:
            raise ValueError(f"Pressure must be non-negative, got {raw_pressure}")

        # 正規化至 0~1（閾值 10.0）
        normalized = min(raw_pressure / 10.0, 1.0)
        duration_ms = int((time.time() - start_time) * 1000)

        return SmokeTestResult(
            test_name=test_name,
            passed=True,
            duration_ms=duration_ms,
            detail=(
                f"✅ TC-3 PASS: pressure={raw_pressure:.2f} (raw), "
                f"{normalized:.3f} (normalized), threshold=10.0"
            ),
        )

    except Exception as e:
        duration_ms = int((time.time() - start_time) * 1000)
        return SmokeTestResult(
            test_name=test_name,
            passed=False,
            duration_ms=duration_ms,
            detail=f"❌ TC-3 ERROR: {type(e).__name__}",
            error=str(e),
        )
