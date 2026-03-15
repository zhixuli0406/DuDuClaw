"""Claude Code SDK chat interface.

Called by the Rust gateway as a subprocess:
    python -m duduclaw.sdk.chat --model MODEL --system-prompt-file PATH

Reads user message from stdin, writes AI response to stdout.
Uses AccountRotator for multi-account rotation and budget tracking.
"""

import argparse
import asyncio
import json
import logging
import os
import sys
from pathlib import Path

logger = logging.getLogger(__name__)


async def chat(
    model: str,
    system_prompt: str,
    user_message: str,
    api_key: str,
) -> str:
    """Send a message to Claude via the Anthropic SDK and return the response."""
    try:
        import anthropic
    except ImportError:
        return "(錯誤：anthropic 套件未安裝，請執行 pip install anthropic)"

    client = anthropic.Anthropic(api_key=api_key)

    try:
        response = client.messages.create(
            model=model,
            max_tokens=2048,
            system=system_prompt,
            messages=[{"role": "user", "content": user_message}],
        )

        # Extract text from response
        texts = []
        for block in response.content:
            if hasattr(block, "text"):
                texts.append(block.text)

        if not texts:
            return "（AI 沒有產生回覆）"

        return "\n".join(texts)

    except anthropic.AuthenticationError:
        return "（錯誤：API Key 無效，請檢查設定）"
    except anthropic.RateLimitError:
        return "（錯誤：API 速率限制，請稍後再試）"
    except anthropic.APIError as e:
        return f"（API 錯誤：{e.message}）"
    except Exception as e:
        return f"（錯誤：{e}）"


async def chat_with_rotation(
    model: str,
    system_prompt: str,
    user_message: str,
    config_path: str,
) -> str:
    """Chat with multi-account rotation support."""
    from .account import Account, AccountType
    from .rotator import AccountRotator, RequestContext, RotationStrategy

    # Load accounts from config
    accounts = load_accounts(config_path)

    if not accounts:
        # Fallback: try env var
        env_key = os.environ.get("ANTHROPIC_API_KEY", "")
        if env_key:
            return await chat(model, system_prompt, user_message, env_key)
        return "（錯誤：未設定任何 API 帳號）"

    # Create rotator
    rotator = AccountRotator(accounts, RotationStrategy.PRIORITY)
    ctx = RequestContext(model=model)

    # Try accounts with rotation
    last_error = ""
    for _attempt in range(len(accounts)):
        try:
            account = await rotator.select_account(ctx)
        except Exception:
            break

        # Get the actual API key for this account
        api_key = get_account_key(account.id, config_path)
        if not api_key:
            await rotator.on_error(account, Exception("No API key"))
            continue

        result = await chat(model, system_prompt, user_message, api_key)

        # Check if it was an error
        if result.startswith("（錯誤：API Key 無效"):
            await rotator.on_error(account, Exception("Invalid key"))
            last_error = result
            continue
        elif result.startswith("（錯誤：API 速率限制"):
            rotator.on_rate_limited(account, 60)
            last_error = result
            continue
        else:
            await rotator.on_success(account)
            # Estimate cost (rough: $3/M input, $15/M output for Sonnet)
            estimated_cost_cents = max(1, len(user_message) // 500 + len(result) // 200)
            rotator.record_usage(account, estimated_cost_cents)
            return result

    return last_error or "（錯誤：所有帳號均不可用）"


def load_accounts(config_path: str) -> list:
    """Load accounts from config.toml."""
    from .account import Account, AccountType

    accounts = []
    try:
        import tomllib
    except ImportError:
        try:
            import tomli as tomllib  # type: ignore[no-redef]
        except ImportError:
            # Fallback: parse manually
            return _load_accounts_fallback(config_path)

    try:
        with open(config_path, "rb") as f:
            config = tomllib.load(f)
    except Exception:
        return _load_accounts_fallback(config_path)

    # Check [api] section for single key
    api_section = config.get("api", {})
    api_key = api_section.get("anthropic_api_key", "")
    if api_key:
        accounts.append(
            Account(
                id="main",
                account_type=AccountType.API_KEY,
                priority=1,
                monthly_budget_cents=config.get("budget", {}).get(
                    "monthly_limit_cents", 10000
                ),
            )
        )

    # Check [[accounts]] array
    for acc in config.get("accounts", []):
        acc_type = AccountType.OAUTH if acc.get("type") == "oauth" else AccountType.API_KEY
        accounts.append(
            Account(
                id=acc.get("id", "unnamed"),
                account_type=acc_type,
                priority=acc.get("priority", 1),
                monthly_budget_cents=acc.get("monthly_budget_cents", 5000),
                tags=acc.get("tags", []),
            )
        )

    return accounts


def _load_accounts_fallback(config_path: str) -> list:
    """Minimal account loading without tomllib."""
    from .account import Account, AccountType

    env_key = os.environ.get("ANTHROPIC_API_KEY", "")
    if env_key:
        return [Account(id="env", account_type=AccountType.API_KEY, priority=1)]

    # Try reading config.toml as text
    try:
        content = Path(config_path).read_text()
        if "anthropic_api_key" in content:
            for line in content.splitlines():
                if "anthropic_api_key" in line and "=" in line:
                    val = line.split("=", 1)[1].strip().strip('"').strip("'")
                    if val:
                        return [
                            Account(
                                id="main", account_type=AccountType.API_KEY, priority=1
                            )
                        ]
    except Exception:
        pass

    return []


def get_account_key(account_id: str, config_path: str) -> str:
    """Get the actual API key for an account."""
    # For 'env' or 'main' with env var
    env_key = os.environ.get("ANTHROPIC_API_KEY", "")
    if env_key:
        return env_key

    # Read from config.toml
    try:
        content = Path(config_path).read_text()
        for line in content.splitlines():
            if "anthropic_api_key" in line and "=" in line:
                val = line.split("=", 1)[1].strip().strip('"').strip("'")
                if val:
                    return val
    except Exception:
        pass

    return ""


def main() -> None:
    parser = argparse.ArgumentParser(description="DuDuClaw Claude Chat")
    parser.add_argument("--model", default="claude-sonnet-4-20250514")
    parser.add_argument("--system-prompt", default="")
    parser.add_argument("--system-prompt-file", default="")
    parser.add_argument("--config", default="")
    args = parser.parse_args()

    # Read system prompt
    system_prompt = args.system_prompt
    if args.system_prompt_file and Path(args.system_prompt_file).exists():
        system_prompt = Path(args.system_prompt_file).read_text(encoding="utf-8")

    if not system_prompt:
        system_prompt = "You are a helpful AI assistant. Reply in the user's language."

    # Read user message from stdin
    user_message = sys.stdin.read().strip()
    if not user_message:
        print("（未收到訊息）")
        sys.exit(0)

    # Config path
    config_path = args.config
    if not config_path:
        home = os.environ.get("DUDUCLAW_HOME", "")
        if not home:
            home = str(Path.home() / ".duduclaw")
        config_path = str(Path(home) / "config.toml")

    # Run
    result = asyncio.run(chat_with_rotation(
        model=args.model,
        system_prompt=system_prompt,
        user_message=user_message,
        config_path=config_path,
    ))
    print(result)


if __name__ == "__main__":
    main()
