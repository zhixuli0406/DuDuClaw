import logging
from typing import List

from .account import Account

logger = logging.getLogger(__name__)

ANTHROPIC_MODELS_ENDPOINT = "https://api.anthropic.com/v1/models"


async def check_account_health(account: Account) -> bool:
    """Check if an account's API key is valid by calling the Anthropic API."""
    if not account.api_key:
        account.is_healthy = False
        return False

    try:
        import urllib.request
        req = urllib.request.Request(
            ANTHROPIC_MODELS_ENDPOINT,
            headers={
                "x-api-key": account.api_key,
                "anthropic-version": "2023-06-01",
            },
        )
        with urllib.request.urlopen(req, timeout=10) as resp:
            healthy = resp.status == 200
            account.is_healthy = healthy
            return healthy
    except Exception as e:
        logger.debug("Health check failed for account %s: %s", account.id, e)
        # 401 = invalid key; other errors (network) treated as inconclusive
        if hasattr(e, "code") and e.code == 401:  # type: ignore[attr-defined]
            account.is_healthy = False
            return False
        # For network errors, trust the current state rather than marking unhealthy
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
