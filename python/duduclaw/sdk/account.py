from dataclasses import dataclass, field
from datetime import datetime, timezone
from enum import Enum
from typing import List, Optional


class AccountType(Enum):
    API_KEY = "api_key"
    OAUTH = "oauth"


@dataclass
class Account:
    id: str
    account_type: AccountType
    priority: int = 1
    monthly_budget_cents: int = 5000
    tags: List[str] = field(default_factory=list)
    api_key: str = ""  # Actual API key value (not persisted to disk)

    # Runtime state (not persisted)
    is_healthy: bool = True
    consecutive_errors: int = 0
    spent_this_month: int = 0
    cooldown_until: Optional[datetime] = None

    @property
    def budget_exceeded(self) -> bool:
        return self.spent_this_month >= self.monthly_budget_cents

    @property
    def is_available(self) -> bool:
        if not self.is_healthy or self.budget_exceeded:
            return False
        if self.cooldown_until and datetime.now(tz=timezone.utc) < self.cooldown_until:
            return False
        return True
