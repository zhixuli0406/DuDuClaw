"""
tests/test_smoke_test.py
Smoke Test P0 — unit tests

TDD RED 先寫測試，GREEN 再寫實作
覆蓋率目標：≥ 80%
"""
from __future__ import annotations

import pytest
from unittest.mock import AsyncMock

from duduclaw.memory_eval.smoke_test import (
    run_smoke_test,
    _tc_basic_store_and_retrieve,
    _tc_memory_isolation,
    _tc_episodic_pressure_check,
    SmokeTestReport,
    SmokeTestResult,
)
from duduclaw.memory_eval.client import MemoryClient, SearchResult, Memory


# ---------------------------------------------------------------------------
# Test doubles
# ---------------------------------------------------------------------------

class MockMemoryClient(MemoryClient):
    """測試用 MemoryClient Mock"""

    def __init__(
        self,
        search_results: list[SearchResult] | None = None,
        pressure: float = 0.50,
        raise_on_store: Exception | None = None,
        raise_on_search: Exception | None = None,
    ) -> None:
        self._store_calls: list[dict] = []
        self._search_results = search_results or []
        self._pressure = pressure
        self._raise_on_store = raise_on_store
        self._raise_on_search = raise_on_search

    async def store(
        self,
        content: str,
        tags: list[str] | None = None,
        namespace: str | None = None,
    ) -> str:
        if self._raise_on_store:
            raise self._raise_on_store
        self._store_calls.append({"content": content, "ns": namespace})
        return f"mem-{len(self._store_calls)}"

    async def search(
        self,
        query: str,
        limit: int = 5,
        namespace: str | None = None,
    ) -> list[SearchResult]:
        if self._raise_on_search:
            raise self._raise_on_search
        return self._search_results

    async def list_important(
        self, agent_id: str, min_importance: float = 0.7, limit: int = 500
    ) -> list[Memory]:
        return []

    async def list_active(self, agent_id: str) -> list[Memory]:
        return []

    async def get_by_ids(self, memory_ids: list[str]) -> list[Memory]:
        return []

    async def get_episodic_pressure(self, hours_ago: int = 24) -> float:
        return self._pressure


# ---------------------------------------------------------------------------
# TC-3: episodic_pressure_check
# ---------------------------------------------------------------------------

@pytest.mark.asyncio
async def test_tc3_episodic_pressure_pass_normal():
    """TC-3 正常回傳有效 float"""
    client = MockMemoryClient(pressure=0.50)
    result = await _tc_episodic_pressure_check(client)
    assert result.passed is True
    assert "TC-3 PASS" in result.detail
    assert "0.50" in result.detail


@pytest.mark.asyncio
async def test_tc3_episodic_pressure_high_value():
    """TC-3 壓力值高（9.5）仍應通過 — float 有效，Smoke Test 不判斷值大小"""
    client = MockMemoryClient(pressure=9.5)
    result = await _tc_episodic_pressure_check(client)
    assert result.passed is True
    assert result.error is None


@pytest.mark.asyncio
async def test_tc3_episodic_pressure_zero():
    """TC-3 壓力值 0.0 應通過"""
    client = MockMemoryClient(pressure=0.0)
    result = await _tc_episodic_pressure_check(client)
    assert result.passed is True


@pytest.mark.asyncio
async def test_tc3_episodic_pressure_negative():
    """TC-3 負數壓力值 → FAIL（業務規則：壓力不得為負）"""
    client = MockMemoryClient(pressure=-1.0)
    result = await _tc_episodic_pressure_check(client)
    assert result.passed is False
    assert result.error is not None


@pytest.mark.asyncio
async def test_tc3_episodic_pressure_exception():
    """TC-3 API 異常 → FAIL，error 欄位記錄例外"""
    class BrokenClient(MockMemoryClient):
        async def get_episodic_pressure(self, hours_ago: int = 24) -> float:
            raise ConnectionError("backend unavailable")

    client = BrokenClient()
    result = await _tc_episodic_pressure_check(client)
    assert result.passed is False
    assert "ConnectionError" in result.detail
    assert result.error == "backend unavailable"


@pytest.mark.asyncio
async def test_tc3_normalized_value_within_bounds():
    """TC-3 正規化值在 0~1 之間（即使 raw > 10）"""
    client = MockMemoryClient(pressure=15.0)
    result = await _tc_episodic_pressure_check(client)
    assert result.passed is True
    # normalized = min(15.0/10.0, 1.0) = 1.0
    assert "1.000" in result.detail


# ---------------------------------------------------------------------------
# TC-1: basic_store_and_retrieve
# ---------------------------------------------------------------------------

@pytest.mark.asyncio
async def test_tc1_store_and_retrieve_pass():
    """TC-1 store + 精確 retrieve → PASS"""
    class SmartMockClient(MockMemoryClient):
        async def search(self, query: str, limit: int = 5, namespace: str | None = None):
            # 模擬：含 SMOKETEST token 的查詢能找到結果
            if "SMOKETEST" in query or "smoke_test_user" in query:
                return [SearchResult(
                    memory_id="mem-001",
                    content=f"{query} smoke_test_user 最愛的食物是拉麵",
                    similarity=0.95,
                )]
            return []

    client = SmartMockClient(pressure=0.0)
    result = await _tc_basic_store_and_retrieve(client)
    assert result.passed is True
    assert "TC-1 PASS" in result.detail


@pytest.mark.asyncio
async def test_tc1_store_and_retrieve_fail_not_found():
    """TC-1 store 成功但 search 回傳空 → FAIL"""
    client = MockMemoryClient(search_results=[])  # 空搜尋結果
    result = await _tc_basic_store_and_retrieve(client)
    assert result.passed is False
    assert "TC-1 FAIL" in result.detail


@pytest.mark.asyncio
async def test_tc1_store_exception():
    """TC-1 store 拋出例外 → FAIL，error 記錄"""
    client = MockMemoryClient(raise_on_store=RuntimeError("quota exceeded"))
    result = await _tc_basic_store_and_retrieve(client)
    assert result.passed is False
    assert "RuntimeError" in result.detail
    assert "quota exceeded" in (result.error or "")


@pytest.mark.asyncio
async def test_tc1_duration_ms_recorded():
    """TC-1 duration_ms 應大於等於 0"""
    client = MockMemoryClient(search_results=[])
    result = await _tc_basic_store_and_retrieve(client)
    assert result.duration_ms >= 0


# ---------------------------------------------------------------------------
# TC-2: memory_isolation
# ---------------------------------------------------------------------------

@pytest.mark.asyncio
async def test_tc2_isolation_pass():
    """TC-2 跨 namespace 無洩漏 → PASS"""
    client = MockMemoryClient(search_results=[])  # NS-B 搜不到 NS-A 的內容
    result = await _tc_memory_isolation(client)
    assert result.passed is True
    assert "TC-2 PASS" in result.detail


@pytest.mark.asyncio
async def test_tc2_isolation_fail_leakage():
    """TC-2 NS-B 搜尋到 NS-A 的 token → FAIL"""
    class LeakyClient(MockMemoryClient):
        async def store(self, content, tags=None, namespace=None):
            self._stored_content = content
            return "mem-001"

        async def search(self, query, limit=5, namespace=None):
            # 模擬洩漏：任何 namespace 都能找到
            if hasattr(self, "_stored_content"):
                return [SearchResult("mem-001", self._stored_content, 0.99)]
            return []

    client = LeakyClient()
    result = await _tc_memory_isolation(client)
    assert result.passed is False
    assert "TC-2 FAIL" in result.detail


@pytest.mark.asyncio
async def test_tc2_not_implemented_skipped():
    """TC-2 client 不支援 namespace → SKIP（視為通過）"""
    class NoNamespaceClient(MockMemoryClient):
        async def store(self, content, tags=None, namespace=None):
            raise NotImplementedError("namespace not supported")

    client = NoNamespaceClient()
    result = await _tc_memory_isolation(client)
    assert result.passed is True
    assert "SKIP" in result.detail


# ---------------------------------------------------------------------------
# run_smoke_test integration
# ---------------------------------------------------------------------------

@pytest.mark.asyncio
async def test_smoke_test_all_pass():
    """整體 run_smoke_test：全部通過的情境"""
    class AllPassClient(MockMemoryClient):
        async def search(self, query, limit=5, namespace=None):
            # TC-1 SMOKETEST query → 命中（返回含 token 的結果）
            if "SMOKETEST" in query:
                return [SearchResult(
                    memory_id="mem-001",
                    content=query,  # content = query，token 必然在其中
                    similarity=0.95,
                )]
            # TC-2 ISOLATION query → 回傳空，模擬 namespace 正確隔離
            return []

    client = AllPassClient(pressure=0.50)
    report = await run_smoke_test(client, agent_id="test-agent")

    assert isinstance(report, SmokeTestReport)
    assert report.passed_count >= 1
    assert report.all_passed is True


@pytest.mark.asyncio
async def test_smoke_test_partial_fail():
    """部分測試失敗 → all_passed = False"""
    client = MockMemoryClient(pressure=-5.0)  # TC-3 會失敗
    client_with_empty = MockMemoryClient(
        search_results=[],
        pressure=-5.0,
    )
    report = await run_smoke_test(client_with_empty, agent_id="test-agent")

    assert isinstance(report, SmokeTestReport)
    assert report.failed_count >= 1
    assert report.all_passed is False


@pytest.mark.asyncio
async def test_smoke_test_report_summary_format():
    """report.summary 格式驗證"""
    client = MockMemoryClient(pressure=3.0)
    report = await run_smoke_test(client, agent_id="test-agent")
    assert "Smoke Test:" in report.summary
    assert "passed" in report.summary
    assert "ms" in report.summary


@pytest.mark.asyncio
async def test_smoke_test_report_has_3_results():
    """report 應包含恰好 3 個測試案例結果"""
    client = MockMemoryClient(pressure=0.5)
    report = await run_smoke_test(client, agent_id="test-agent")
    assert len(report.results) == 3


@pytest.mark.asyncio
async def test_smoke_test_report_run_id_is_uuid():
    """report.run_id 應為有效的 UUID 格式"""
    import uuid
    client = MockMemoryClient(pressure=0.5)
    report = await run_smoke_test(client, agent_id="test-agent")
    uuid.UUID(report.run_id)  # 若格式無效會拋出 ValueError


def test_smoke_test_result_dataclass():
    """SmokeTestResult dataclass 基本屬性驗證"""
    r = SmokeTestResult(
        test_name="basic_store_and_retrieve",
        passed=True,
        duration_ms=42,
        detail="✅ TC-1 PASS",
    )
    assert r.test_name == "basic_store_and_retrieve"
    assert r.passed is True
    assert r.duration_ms == 42
    assert r.error is None  # 預設值


def test_smoke_test_report_all_passed_property():
    """SmokeTestReport.all_passed 屬性邏輯"""
    report = SmokeTestReport()
    report.failed_count = 0
    assert report.all_passed is True

    report.failed_count = 1
    assert report.all_passed is False


# ---------------------------------------------------------------------------
# Line 87: skipped_count incremented when TC-2 returns SKIP
# ---------------------------------------------------------------------------

@pytest.mark.asyncio
async def test_run_smoke_test_skipped_count_incremented():
    """line 87: TC-2 回傳 SKIP → report.skipped_count >= 1"""
    class SkipOnStoreClient(MockMemoryClient):
        async def store(self, content, tags=None, namespace=None):
            raise NotImplementedError("namespace not supported")

        async def search(self, query, limit=5, namespace=None):
            # TC-1 需要能找到內容才能通過，確保此處讓 TC-1 pass 不影響計數
            if "SMOKETEST" in query:
                return [SearchResult(
                    memory_id="mem-001",
                    content=query,
                    similarity=0.95,
                )]
            return []

    client = SkipOnStoreClient(pressure=0.50)
    report = await run_smoke_test(client, agent_id="test-agent")

    assert report.skipped_count >= 1


# ---------------------------------------------------------------------------
# Lines 222-224: TC-2 general except Exception branch
# ---------------------------------------------------------------------------

@pytest.mark.asyncio
async def test_tc2_general_exception_returns_error_detail():
    """lines 222-224: search 拋出非 NotImplementedError → passed=False, detail 含 TC-2 ERROR"""
    class ConnectionErrorClient(MockMemoryClient):
        async def search(self, query, limit=5, namespace=None):
            raise ConnectionError("network unreachable")

    client = ConnectionErrorClient()
    result = await _tc_memory_isolation(client)

    assert result.passed is False
    assert "TC-2 ERROR" in result.detail


# ---------------------------------------------------------------------------
# Line 245: ValueError for non-numeric episodic pressure
# ---------------------------------------------------------------------------

@pytest.mark.asyncio
async def test_tc3_non_numeric_pressure_raises_value_error():
    """line 245: get_episodic_pressure 回傳非數值 → passed=False, detail 含 ValueError"""
    class StringPressureClient(MockMemoryClient):
        async def get_episodic_pressure(self, hours_ago: int = 24):
            return "not a number"

    client = StringPressureClient()
    result = await _tc_episodic_pressure_check(client)

    assert result.passed is False
    assert "ValueError" in result.detail
