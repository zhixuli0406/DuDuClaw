import logging
from typing import List

from .account import Account

logger = logging.getLogger(__name__)


async def check_account_health(account: Account) -> bool:
    """Check if an account's API key/token is valid"""
    # TODO: Phase 2 - implement actual API health check
    return account.is_healthy


async def check_all_accounts(accounts: List[Account]) -> dict:
    """Check health of all accounts and return summary"""
    results = {}
    for account in accounts:
        healthy = await check_account_health(account)
        results[account.id] = {
            "healthy": healthy,
            "budget_remaining": account.monthly_budget_cents - account.spent_this_month,
            "consecutive_errors": account.consecutive_errors,
        }
    return results
