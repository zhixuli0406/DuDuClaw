"""Claude Code SDK chat interface.

Called by the Rust gateway as a subprocess:
    python -m duduclaw.sdk.chat --model MODEL --system-prompt-file PATH

Reads user message from stdin, writes AI response to stdout.
Uses the `claude` CLI (Claude Code SDK) for conversation,
which provides built-in tools (bash, web search, file ops, etc.).
Supports multi-account rotation via AccountRotator.
"""

import argparse
import asyncio
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
    """Send a message via Claude Code SDK (claude CLI)."""

    # Build the prompt with system context
    full_prompt = user_message

    # Try Claude Code SDK first, then fallback to claude CLI
    result = await _call_claude_sdk(model, system_prompt, full_prompt, api_key)
    if result is not None:
        return result

    result = await _call_claude_cli(model, system_prompt, full_prompt, api_key)
    if result is not None:
        return result

    return "（錯誤：claude CLI 未安裝，請執行 npm install -g @anthropic-ai/claude-code）"


async def _call_claude_sdk(
    model: str,
    system_prompt: str,
    prompt: str,
    api_key: str,
) -> str | None:
    """Try using claude_code_sdk Python package."""
    try:
        from claude_code_sdk import Claude

        agent = Claude(
            model=model,
            system_prompt=system_prompt,
            api_key=api_key,
        )

        result_parts = []
        async for event in agent.process_query(prompt):
            if hasattr(event, "text"):
                result_parts.append(event.text)
            elif hasattr(event, "content") and isinstance(event.content, str):
                result_parts.append(event.content)

        if result_parts:
            return "".join(result_parts)
        return None

    except ImportError:
        logger.debug("claude_code_sdk not installed, trying CLI")
        return None
    except Exception as e:
        logger.warning("claude_code_sdk error: %s", e)
        return None


async def _call_claude_cli(
    model: str,
    system_prompt: str,
    prompt: str,
    api_key: str,
) -> str | None:
    """Fallback: use `claude` CLI tool as subprocess."""
    # Check if claude CLI exists
    claude_path = _find_claude_cli()
    if not claude_path:
        return None

    try:
        env = os.environ.copy()
        env["ANTHROPIC_API_KEY"] = api_key

        # claude -p "prompt" --model MODEL --output-format text
        cmd = [
            claude_path,
            "-p", prompt,
            "--model", model,
            "--output-format", "text",
        ]

        # Add system prompt via --system-prompt if supported
        if system_prompt:
            cmd.extend(["--system-prompt", system_prompt])

        proc = await asyncio.create_subprocess_exec(
            *cmd,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
            env=env,
        )

        stdout, stderr = await asyncio.wait_for(
            proc.communicate(),
            timeout=120,
        )

        if proc.returncode == 0:
            text = stdout.decode("utf-8", errors="replace").strip()
            if text:
                return text

        if stderr:
            logger.warning("claude CLI stderr: %s", stderr.decode("utf-8", errors="replace")[:200])

        return None

    except asyncio.TimeoutError:
        logger.warning("claude CLI timeout (120s)")
        return None
    except Exception as e:
        logger.warning("claude CLI error: %s", e)
        return None


def _find_claude_cli() -> str | None:
    """Find the claude CLI binary."""
    import shutil

    # Check PATH
    path = shutil.which("claude")
    if path:
        return path

    # Common install locations
    candidates = [
        os.path.expanduser("~/.npm-global/bin/claude"),
        "/usr/local/bin/claude",
        os.path.expanduser("~/.claude/bin/claude"),
    ]
    for p in candidates:
        if os.path.isfile(p) and os.access(p, os.X_OK):
            return p

    return None


async def chat_with_rotation(
    model: str,
    system_prompt: str,
    user_message: str,
    config_path: str,
) -> str:
    """Chat with multi-account rotation support."""
    from .rotator import AccountRotator, RequestContext, RotationStrategy

    # Load accounts from config
    accounts = load_accounts(config_path)

    if not accounts:
        # Fallback: try env var
        env_key = os.environ.get("ANTHROPIC_API_KEY", "")
        if env_key:
            return await chat(model, system_prompt, user_message, env_key)
        return "（錯誤：未設定任何 API 帳號，請設定 ANTHROPIC_API_KEY）"

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
        if "API Key 無效" in result:
            await rotator.on_error(account, Exception("Invalid key"))
            last_error = result
            continue
        elif "速率限制" in result:
            rotator.on_rate_limited(account, 60)
            last_error = result
            continue
        elif result.startswith("（錯誤："):
            last_error = result
            continue
        else:
            await rotator.on_success(account)
            # Estimate cost
            estimated_cost_cents = max(1, len(user_message) // 500 + len(result) // 200)
            rotator.record_usage(account, estimated_cost_cents)
            return result

    return last_error or "（錯誤：所有帳號均不可用）"


def _load_config_toml(config_path: str) -> dict:
    """Parse config.toml and return a dict. Returns {} on error."""
    try:
        raw = Path(config_path).read_bytes()
    except Exception:
        return {}
    # Python 3.11+ has tomllib in stdlib; older versions need tomli
    try:
        import tomllib  # type: ignore
        return tomllib.loads(raw.decode("utf-8", errors="replace"))
    except ImportError:
        pass
    try:
        import tomli  # type: ignore
        return tomli.loads(raw.decode("utf-8", errors="replace"))
    except ImportError:
        pass
    # Fallback: minimal line-based parser for simple key=value pairs
    result: dict = {}
    current_section: str = ""
    for line in raw.decode("utf-8", errors="replace").splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("[") and not line.startswith("[["):
            current_section = line.strip("[]").strip()
            result.setdefault(current_section, {})
        elif "=" in line:
            key, _, val = line.partition("=")
            key = key.strip()
            val = val.strip().strip('"').strip("'")
            if current_section:
                result.setdefault(current_section, {})[key] = val
            else:
                result[key] = val
    return result


def load_accounts(config_path: str) -> list:
    """Load all accounts from config.toml.

    Supports two config formats:
    1. New multi-account format:
       [[accounts]]
       id = "account1"
       anthropic_api_key = "sk-ant-..."
       priority = 1

    2. Legacy single-account format:
       [api]
       anthropic_api_key = "sk-ant-..."
    """
    from .account import Account, AccountType

    config = _load_config_toml(config_path)
    accounts = []

    # Format 1: [[accounts]] array
    if "accounts" in config and isinstance(config["accounts"], list):
        for idx, acc_data in enumerate(config["accounts"]):
            if not isinstance(acc_data, dict):
                continue
            api_key = acc_data.get("anthropic_api_key", "").strip()
            if not api_key:
                continue
            accounts.append(Account(
                id=acc_data.get("id", f"account_{idx}"),
                account_type=AccountType.API_KEY,
                priority=int(acc_data.get("priority", idx + 1)),
                monthly_budget_cents=int(acc_data.get("monthly_budget_cents", 5000)),
                tags=acc_data.get("tags", []),
                api_key=api_key,
            ))

    # Format 2: [api] section
    if not accounts:
        api_section = config.get("api", {})
        if isinstance(api_section, dict):
            key = api_section.get("anthropic_api_key", "").strip()
            if key:
                accounts.append(Account(
                    id="main",
                    account_type=AccountType.API_KEY,
                    priority=1,
                    api_key=key,
                ))

    # Fallback: environment variable
    if not accounts:
        env_key = os.environ.get("ANTHROPIC_API_KEY", "").strip()
        if env_key:
            accounts.append(Account(
                id="env",
                account_type=AccountType.API_KEY,
                priority=1,
                api_key=env_key,
            ))

    return accounts


def get_account_key(account_id: str, config_path: str) -> str:
    """Get the actual API key for an account by its ID."""
    accounts = load_accounts(config_path)

    # Find by ID
    for account in accounts:
        if account.id == account_id:
            return account.api_key

    # Fallback: return the first available key
    if accounts:
        return accounts[0].api_key

    # Last resort: environment variable
    return os.environ.get("ANTHROPIC_API_KEY", "")


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
    result = asyncio.run(
        chat_with_rotation(
            model=args.model,
            system_prompt=system_prompt,
            user_message=user_message,
            config_path=config_path,
        )
    )
    print(result)


if __name__ == "__main__":
    main()
