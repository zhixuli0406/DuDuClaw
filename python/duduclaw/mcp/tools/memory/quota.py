"""Daily write quota enforcement for MCP Server memory_store.

Quota rules (TL Decision 2026-04-29):
  - Default: 1000 records/day per client (configurable)
  - Quota window: UTC 00:00 – 23:59:59
  - On exceed: HTTP 429 with ``retry_after`` seconds and ``reset_at`` ISO 8601 UTC
  - Quota is checked BEFORE write; incremented AFTER successful write

Current backend: in-memory dict (thread-safe for asyncio single-thread model).
Production upgrade path: replace _store with Redis INCR + EXPIREAT.

Per-client state:
  QuotaEnforcer._store: dict[client_id → _QuotaEntry]
  _QuotaEntry.reset_at_ts: monotonic float of next UTC midnight
"""

from __future__ import annotations

import time
from dataclasses import dataclass
from datetime import datetime, timedelta, timezone
from typing import Optional

from ...errors import QuotaExceededError

# ── Constants ─────────────────────────────────────────────────────────────────

DEFAULT_QUOTA: int = 1000  # records per UTC day


# ── Internal state ────────────────────────────────────────────────────────────


class _QuotaEntry:
    """Per-client quota state (not exposed publicly)."""

    __slots__ = ("used", "limit", "reset_at_ts")

    def __init__(self, limit: int) -> None:
        self.used: int = 0
        self.limit: int = limit
        self.reset_at_ts: float = _next_utc_midnight_ts()

    def is_expired(self) -> bool:
        """Return True iff the UTC day has rolled over."""
        return time.time() >= self.reset_at_ts

    def reset(self) -> None:
        """Reset the quota counter for the new UTC day."""
        self.used = 0
        self.reset_at_ts = _next_utc_midnight_ts()


# ── Result type ───────────────────────────────────────────────────────────────


@dataclass(frozen=True)
class QuotaInfo:
    """Snapshot of a client's quota state.

    Attributes:
        client_id:  The client identifier.
        used:       Records written today.
        limit:      Maximum allowed records per day.
        reset_at:   ISO 8601 UTC timestamp when quota resets.
    """

    client_id: str
    used: int
    limit: int
    reset_at: str  # ISO 8601 UTC, e.g. "2026-04-30T00:00:00Z"

    @property
    def remaining(self) -> int:
        """Records remaining before quota is exceeded."""
        return max(0, self.limit - self.used)


# ── Helpers ───────────────────────────────────────────────────────────────────


def _next_utc_midnight_ts() -> float:
    """Return Unix timestamp of the next UTC midnight (00:00:00.000)."""
    now = datetime.now(timezone.utc)
    next_midnight = (now + timedelta(days=1)).replace(
        hour=0, minute=0, second=0, microsecond=0
    )
    return next_midnight.timestamp()


def _format_reset_at(ts: float) -> str:
    """Format a Unix timestamp as ISO 8601 UTC string ending in ``Z``."""
    dt = datetime.fromtimestamp(ts, tz=timezone.utc)
    return dt.strftime("%Y-%m-%dT%H:%M:%SZ")


# ── Enforcer ──────────────────────────────────────────────────────────────────


class QuotaEnforcer:
    """Per-client daily write quota enforcer.

    Checks whether a client has remaining quota before a write, and increments
    the counter after a successful write.

    Thread-safety:
        Uses a plain dict — safe under asyncio's single-threaded event loop.
        Not safe across OS threads; wrap with a Lock if threading is required.

    Production upgrade:
        Replace ``_store`` with Redis ``INCR`` + ``EXPIREAT`` for distributed
        multi-process deployments.

    Example::

        enforcer = QuotaEnforcer(default_limit=1000)

        # Before write:
        enforcer.check_or_raise(ctx.client_id)     # raises QuotaExceededError if over limit

        # After successful write:
        quota_info = enforcer.increment(ctx.client_id)
        print(quota_info.remaining)                 # 999
    """

    def __init__(self, default_limit: int = DEFAULT_QUOTA) -> None:
        self._default_limit = default_limit
        self._store: dict[str, _QuotaEntry] = {}

    def _get_or_create(self, client_id: str) -> _QuotaEntry:
        """Return existing entry (auto-resetting if expired) or create a new one."""
        entry = self._store.get(client_id)
        if entry is None:
            entry = _QuotaEntry(self._default_limit)
            self._store[client_id] = entry
        elif entry.is_expired():
            entry.reset()
        return entry

    def check_or_raise(self, client_id: str) -> None:
        """Raise :exc:`QuotaExceededError` if the client has hit their daily limit.

        This should be called **before** the write operation.  The counter is
        not incremented here — call :meth:`increment` after a successful write.

        Args:
            client_id: The client identifier (from ``APIKeyContext.client_id``).

        Raises:
            QuotaExceededError: When ``used >= limit`` for today.
        """
        entry = self._get_or_create(client_id)
        if entry.used >= entry.limit:
            retry_after = max(0, int(entry.reset_at_ts - time.time()))
            raise QuotaExceededError(
                message=(
                    f"Daily write quota of {entry.limit} records exceeded for your client."
                ),
                quota_limit=entry.limit,
                quota_used=entry.used,
                retry_after=retry_after,
                reset_at=_format_reset_at(entry.reset_at_ts),
            )

    def increment(self, client_id: str) -> QuotaInfo:
        """Increment the write counter and return the updated quota snapshot.

        Call this **after** a successful write operation.

        Args:
            client_id: The client identifier.

        Returns:
            :class:`QuotaInfo` with the updated ``used`` and ``remaining`` counts.
        """
        entry = self._get_or_create(client_id)
        entry.used += 1
        return QuotaInfo(
            client_id=client_id,
            used=entry.used,
            limit=entry.limit,
            reset_at=_format_reset_at(entry.reset_at_ts),
        )

    def get_info(self, client_id: str) -> QuotaInfo:
        """Return the current quota snapshot without incrementing the counter.

        Args:
            client_id: The client identifier.

        Returns:
            :class:`QuotaInfo` snapshot.
        """
        entry = self._get_or_create(client_id)
        return QuotaInfo(
            client_id=client_id,
            used=entry.used,
            limit=entry.limit,
            reset_at=_format_reset_at(entry.reset_at_ts),
        )
