"""Unit tests for the daily write quota enforcer (M56 regression).

The atomic ``reserve`` must prevent concurrent writers from both passing the
quota gate when only one slot remains.
"""
from __future__ import annotations

import threading

import pytest

from duduclaw.mcp.errors import QuotaExceededError
from duduclaw.mcp.tools.memory.quota import QuotaEnforcer


def test_reserve_increments_and_enforces_limit():
    enforcer = QuotaEnforcer(default_limit=2)
    assert enforcer.reserve("c").used == 1
    assert enforcer.reserve("c").used == 2
    with pytest.raises(QuotaExceededError):
        enforcer.reserve("c")


def test_release_rolls_back_one_slot():
    enforcer = QuotaEnforcer(default_limit=1)
    enforcer.reserve("c")
    enforcer.release("c")
    # After rollback the slot is free again.
    assert enforcer.reserve("c").used == 1


def test_release_never_goes_below_zero():
    enforcer = QuotaEnforcer(default_limit=5)
    enforcer.release("c")  # no prior reserve
    assert enforcer.get_info("c").used == 0


def test_concurrent_reserves_never_exceed_limit():
    """M56: many threads racing must not over-grant past the limit."""
    limit = 50
    workers = 200
    enforcer = QuotaEnforcer(default_limit=limit)
    granted = []
    grant_lock = threading.Lock()
    start = threading.Barrier(workers)

    def worker() -> None:
        start.wait()  # maximise contention
        try:
            enforcer.reserve("client")
            with grant_lock:
                granted.append(1)
        except QuotaExceededError:
            pass

    threads = [threading.Thread(target=worker) for _ in range(workers)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()

    # Exactly `limit` reservations must succeed — never more.
    assert sum(granted) == limit
    assert enforcer.get_info("client").used == limit
