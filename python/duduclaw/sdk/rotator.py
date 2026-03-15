import asyncio
import logging
from datetime import datetime, timedelta, timezone
from enum import Enum
from typing import List, Optional

from .account import Account

logger = logging.getLogger(__name__)


class RotationStrategy(Enum):
    ROUND_ROBIN = "round_robin"
    LEAST_COST = "least_cost"
    FAILOVER = "failover"
    PRIORITY = "priority"


class AllAccountsExhausted(Exception):
    """Raised when no accounts are available"""

    pass


class RequestContext:
    """Context for selecting the right account"""

    def __init__(self, model: str = "", tags: Optional[List[str]] = None):
        self.model = model
        self.tags = tags or []

    def matches_tags(self, account_tags: List[str]) -> bool:
        if not account_tags:
            return True
        return any(t in account_tags for t in self.tags)


class AccountRotator:
    def __init__(
        self,
        accounts: List[Account],
        strategy: RotationStrategy = RotationStrategy.PRIORITY,
    ):
        self.accounts = accounts
        self.strategy = strategy
        self._round_robin_index = 0
        self._lock = asyncio.Lock()

    async def select_account(self, context: Optional[RequestContext] = None) -> Account:
        async with self._lock:
            ctx = context or RequestContext()
            available = [a for a in self.accounts if a.is_available]

            if not available:
                raise AllAccountsExhausted("No accounts available for rotation")

            if self.strategy == RotationStrategy.ROUND_ROBIN:
                account = available[self._round_robin_index % len(available)]
                self._round_robin_index += 1
                return account
            elif self.strategy == RotationStrategy.LEAST_COST:
                return min(available, key=lambda a: a.spent_this_month)
            elif self.strategy == RotationStrategy.FAILOVER:
                return sorted(available, key=lambda a: a.priority)[0]
            elif self.strategy == RotationStrategy.PRIORITY:
                tagged = [a for a in available if ctx.matches_tags(a.tags)]
                pool = tagged if tagged else available
                return sorted(pool, key=lambda a: a.priority)[0]

            raise ValueError(f"Unknown rotation strategy: {self.strategy}")

    def on_rate_limited(self, account: Account, retry_after: int) -> None:
        account.cooldown_until = datetime.now(tz=timezone.utc) + timedelta(seconds=retry_after)
        logger.warning(
            f"Account {account.id} rate limited, cooldown {retry_after}s"
        )

    def on_error(self, account: Account, error: Exception) -> None:
        account.consecutive_errors += 1
        if account.consecutive_errors >= 3:
            account.is_healthy = False
            logger.error(
                f"Account {account.id} marked unhealthy after 3 consecutive errors"
            )

    async def on_success(self, account: Account) -> None:
        async with self._lock:
            account.consecutive_errors = 0

    def record_usage(self, account: Account, cost_cents: int) -> None:
        """Record API usage cost"""
        account.spent_this_month += cost_cents
        if account.spent_this_month >= account.monthly_budget_cents * 0.8:
            logger.warning(
                f"Account {account.id} at {account.spent_this_month}/{account.monthly_budget_cents} cents "
                f"({account.spent_this_month * 100 // account.monthly_budget_cents}%)"
            )

    def reset_monthly_usage(self) -> None:
        """Reset all accounts' monthly spending (call at month start)"""
        for account in self.accounts:
            account.spent_this_month = 0
            account.consecutive_errors = 0
            account.is_healthy = True
            account.cooldown_until = None
        logger.info("Monthly usage reset for all accounts")

    @property
    def total_budget(self) -> int:
        return sum(a.monthly_budget_cents for a in self.accounts)

    @property
    def total_spent(self) -> int:
        return sum(a.spent_this_month for a in self.accounts)

    @property
    def available_accounts(self) -> List[Account]:
        return [a for a in self.accounts if a.is_available]

    def status_summary(self) -> dict:
        return {
            "total_accounts": len(self.accounts),
            "available": len(self.available_accounts),
            "strategy": self.strategy.value,
            "total_budget_cents": self.total_budget,
            "total_spent_cents": self.total_spent,
            "accounts": [
                {
                    "id": a.id,
                    "healthy": a.is_healthy,
                    "available": a.is_available,
                    "spent": a.spent_this_month,
                    "budget": a.monthly_budget_cents,
                }
                for a in self.accounts
            ],
        }
