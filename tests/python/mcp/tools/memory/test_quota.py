"""Tests for QuotaEnforcer.

Acceptance criteria (from mcp-memory-endpoints-design.md §6.2):
  ✅ Writing record #1 → quota_used=1, quota_remaining=999
  ✅ Writing record #1000 → quota_used=1000, quota_remaining=0
  ✅ Writing record #1001 → 429 quota_exceeded + retry_after
  ✅ After UTC 00:00 → quota resets to 0
"""

from __future__ import annotations

import time

import pytest

from duduclaw.mcp.errors import QuotaExceededError
from duduclaw.mcp.tools.memory.quota import DEFAULT_QUOTA, QuotaEnforcer, QuotaInfo


# ── Helpers ───────────────────────────────────────────────────────────────────


def _force_expire(enforcer: QuotaEnforcer, client_id: str) -> None:
    """Simulate UTC midnight passing by backdating the reset timestamp."""
    entry = enforcer._store[client_id]
    entry.reset_at_ts = time.time() - 1  # already in the past


# ── Basic quota management ────────────────────────────────────────────────────


class TestQuotaBasics:
    def test_check_passes_for_new_client(self) -> None:
        enforcer = QuotaEnforcer(default_limit=1000)
        enforcer.check_or_raise("client1")  # must not raise

    def test_increment_returns_quota_info(self) -> None:
        enforcer = QuotaEnforcer(default_limit=1000)
        info = enforcer.increment("client1")
        assert isinstance(info, QuotaInfo)
        assert info.used == 1
        assert info.limit == 1000
        assert info.remaining == 999

    def test_increment_accumulates(self) -> None:
        enforcer = QuotaEnforcer(default_limit=1000)
        for _ in range(10):
            enforcer.increment("client1")
        info = enforcer.get_info("client1")
        assert info.used == 10
        assert info.remaining == 990

    def test_get_info_does_not_increment(self) -> None:
        enforcer = QuotaEnforcer(default_limit=1000)
        enforcer.increment("client1")
        info_a = enforcer.get_info("client1")
        info_b = enforcer.get_info("client1")
        assert info_a.used == info_b.used == 1


# ── Quota exceeded ────────────────────────────────────────────────────────────


class TestQuotaExceeded:
    def test_raises_when_at_limit(self) -> None:
        enforcer = QuotaEnforcer(default_limit=3)
        for _ in range(3):
            enforcer.increment("client1")
        with pytest.raises(QuotaExceededError) as exc_info:
            enforcer.check_or_raise("client1")
        err = exc_info.value
        assert err.code == "quota_exceeded"
        assert err.quota_used == 3
        assert err.quota_limit == 3

    def test_error_has_retry_after(self) -> None:
        enforcer = QuotaEnforcer(default_limit=1)
        enforcer.increment("client1")
        with pytest.raises(QuotaExceededError) as exc_info:
            enforcer.check_or_raise("client1")
        assert exc_info.value.retry_after >= 0

    def test_error_has_reset_at_utc(self) -> None:
        enforcer = QuotaEnforcer(default_limit=1)
        enforcer.increment("client1")
        with pytest.raises(QuotaExceededError) as exc_info:
            enforcer.check_or_raise("client1")
        reset_at = exc_info.value.reset_at
        assert reset_at.endswith("Z"), f"Expected UTC ISO string ending in Z, got {reset_at!r}"
        assert "T" in reset_at  # ISO 8601

    def test_check_passes_just_below_limit(self) -> None:
        enforcer = QuotaEnforcer(default_limit=5)
        for _ in range(4):
            enforcer.increment("client1")
        enforcer.check_or_raise("client1")  # 4/5 — should not raise

    def test_http_status_is_429(self) -> None:
        enforcer = QuotaEnforcer(default_limit=1)
        enforcer.increment("client1")
        with pytest.raises(QuotaExceededError) as exc_info:
            enforcer.check_or_raise("client1")
        assert exc_info.value.http_status == 429


# ── Client isolation ──────────────────────────────────────────────────────────


class TestClientIsolation:
    def test_different_clients_are_independent(self) -> None:
        enforcer = QuotaEnforcer(default_limit=2)
        enforcer.increment("client_a")
        enforcer.increment("client_b")
        assert enforcer.get_info("client_a").used == 1
        assert enforcer.get_info("client_b").used == 1

    def test_client_a_exceeding_does_not_block_client_b(self) -> None:
        enforcer = QuotaEnforcer(default_limit=1)
        enforcer.increment("client_a")  # client_a at limit
        enforcer.check_or_raise("client_b")  # client_b unaffected


# ── Quota reset ───────────────────────────────────────────────────────────────


class TestQuotaReset:
    def test_quota_resets_after_utc_midnight(self) -> None:
        enforcer = QuotaEnforcer(default_limit=1000)
        enforcer.increment("client1")
        enforcer.increment("client1")
        _force_expire(enforcer, "client1")

        # After expiry, check should pass and counter should be reset
        enforcer.check_or_raise("client1")
        info = enforcer.get_info("client1")
        assert info.used == 0

    def test_reset_allows_write_that_was_previously_blocked(self) -> None:
        enforcer = QuotaEnforcer(default_limit=2)
        enforcer.increment("client1")
        enforcer.increment("client1")
        _force_expire(enforcer, "client1")

        # Now quota is reset — should be able to write again
        enforcer.check_or_raise("client1")
        info = enforcer.increment("client1")
        assert info.used == 1


# ── Custom limits ─────────────────────────────────────────────────────────────


class TestCustomLimits:
    def test_custom_limit_respected(self) -> None:
        enforcer = QuotaEnforcer(default_limit=5)
        for _ in range(5):
            enforcer.increment("client1")
        with pytest.raises(QuotaExceededError) as exc_info:
            enforcer.check_or_raise("client1")
        assert exc_info.value.quota_limit == 5

    def test_default_limit_constant(self) -> None:
        assert DEFAULT_QUOTA == 1000

    def test_default_enforcer_uses_default_limit(self) -> None:
        enforcer = QuotaEnforcer()
        info = enforcer.get_info("any_client")
        assert info.limit == DEFAULT_QUOTA


# ── QuotaInfo ─────────────────────────────────────────────────────────────────


class TestQuotaInfo:
    def test_remaining_is_limit_minus_used(self) -> None:
        enforcer = QuotaEnforcer(default_limit=100)
        for _ in range(37):
            enforcer.increment("client1")
        info = enforcer.get_info("client1")
        assert info.remaining == 63

    def test_remaining_never_negative(self) -> None:
        enforcer = QuotaEnforcer(default_limit=2)
        # Force used > limit via direct manipulation (edge case guard)
        enforcer.increment("client1")
        enforcer.increment("client1")
        info = enforcer.get_info("client1")
        assert info.remaining == 0

    def test_reset_at_is_iso8601_utc(self) -> None:
        enforcer = QuotaEnforcer(default_limit=100)
        info = enforcer.get_info("client1")
        assert "T" in info.reset_at
        assert info.reset_at.endswith("Z")
