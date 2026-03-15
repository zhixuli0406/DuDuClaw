"""Claude Code SDK chat interface with tool use (web search + curl).

Called by the Rust gateway as a subprocess:
    python -m duduclaw.sdk.chat --model MODEL --system-prompt-file PATH

Reads user message from stdin, writes AI response to stdout.
Uses AccountRotator for multi-account rotation and budget tracking.
Supports tools: web_search (via DuckDuckGo) and curl (fetch URL content).
"""

import argparse
import asyncio
import json
import logging
import os
import subprocess
import sys
import urllib.parse
import urllib.request
from pathlib import Path

logger = logging.getLogger(__name__)

# ── Tool definitions for Claude API ──────────────────────────

TOOLS = [
    {
        "name": "web_search",
        "description": "Search the web using DuckDuckGo. Returns search results with titles, URLs, and snippets. Use this when you need current information, facts, or to look up something you don't know.",
        "input_schema": {
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query",
                }
            },
            "required": ["query"],
        },
    },
    {
        "name": "curl",
        "description": "Fetch the content of a URL. Returns the text content of a web page. Use this to read articles, documentation, or any web content.",
        "input_schema": {
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The URL to fetch",
                },
                "max_length": {
                    "type": "integer",
                    "description": "Maximum number of characters to return (default 5000)",
                },
            },
            "required": ["url"],
        },
    },
]


# ── Tool implementations ─────────────────────────────────────

def tool_web_search(query: str) -> str:
    """Search using DuckDuckGo HTML and parse results."""
    try:
        encoded = urllib.parse.quote_plus(query)
        url = f"https://html.duckduckgo.com/html/?q={encoded}"
        req = urllib.request.Request(url, headers={
            "User-Agent": "DuDuClaw/0.2 (AI Assistant)"
        })
        with urllib.request.urlopen(req, timeout=10) as resp:
            html = resp.read().decode("utf-8", errors="replace")

        # Simple HTML parsing — extract result snippets
        results = []
        chunks = html.split('class="result__a"')
        for chunk in chunks[1:6]:  # top 5 results
            # Extract title
            title_end = chunk.find("</a>")
            title_text = chunk[:title_end] if title_end > 0 else ""
            title_text = _strip_html(title_text.split(">")[-1] if ">" in title_text else title_text)

            # Extract URL
            href = ""
            if 'href="' in chunk:
                href_start = chunk.index('href="') + 6
                href_end = chunk.index('"', href_start)
                href = chunk[href_start:href_end]
                # DuckDuckGo redirects
                if "uddg=" in href:
                    href = urllib.parse.unquote(href.split("uddg=")[-1].split("&")[0])

            # Extract snippet
            snippet = ""
            if 'class="result__snippet"' in chunk:
                snip_start = chunk.index('class="result__snippet"')
                snip_chunk = chunk[snip_start:]
                snip_start2 = snip_chunk.find(">") + 1
                snip_end = snip_chunk.find("</")
                if snip_end > snip_start2:
                    snippet = _strip_html(snip_chunk[snip_start2:snip_end])

            if title_text:
                results.append(f"**{title_text}**\n{href}\n{snippet}")

        if not results:
            return f"No results found for: {query}"

        return "\n\n---\n\n".join(results)

    except Exception as e:
        return f"Search error: {e}"


def tool_curl(url: str, max_length: int = 5000) -> str:
    """Fetch URL content using subprocess curl."""
    try:
        result = subprocess.run(
            ["curl", "-sL", "--max-time", "10", "-A", "DuDuClaw/0.2", url],
            capture_output=True,
            text=True,
            timeout=15,
        )
        content = result.stdout

        if not content:
            return f"Empty response from {url}"

        # Strip HTML tags for readability
        text = _strip_html(content)

        # Collapse whitespace
        lines = [line.strip() for line in text.splitlines() if line.strip()]
        text = "\n".join(lines)

        if len(text) > max_length:
            text = text[:max_length] + f"\n\n[... truncated, {len(content)} chars total]"

        return text

    except subprocess.TimeoutExpired:
        return f"Timeout fetching {url}"
    except Exception as e:
        return f"Curl error: {e}"


def _strip_html(html: str) -> str:
    """Remove HTML tags from a string."""
    import re
    text = re.sub(r"<[^>]+>", "", html)
    text = text.replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">")
    text = text.replace("&quot;", '"').replace("&#39;", "'").replace("&nbsp;", " ")
    return text.strip()


def execute_tool(name: str, input_data: dict) -> str:
    """Execute a tool and return the result."""
    if name == "web_search":
        return tool_web_search(input_data.get("query", ""))
    elif name == "curl":
        return tool_curl(
            input_data.get("url", ""),
            input_data.get("max_length", 5000),
        )
    return f"Unknown tool: {name}"


# ── Chat with tool use loop ──────────────────────────────────

MAX_TOOL_ROUNDS = 5

async def chat(
    model: str,
    system_prompt: str,
    user_message: str,
    api_key: str,
) -> str:
    """Send a message to Claude with tool use support."""
    try:
        import anthropic
    except ImportError:
        return "(錯誤：anthropic 套件未安裝，請執行 pip install anthropic)"

    client = anthropic.Anthropic(api_key=api_key)
    messages = [{"role": "user", "content": user_message}]

    try:
        for _round in range(MAX_TOOL_ROUNDS):
            response = client.messages.create(
                model=model,
                max_tokens=4096,
                system=system_prompt,
                messages=messages,
                tools=TOOLS,
            )

            # Check if Claude wants to use tools
            if response.stop_reason == "tool_use":
                # Collect all tool uses and results
                assistant_content = []
                tool_results = []

                for block in response.content:
                    if block.type == "tool_use":
                        logger.info("🔧 Tool call: %s(%s)", block.name, json.dumps(block.input, ensure_ascii=False)[:100])
                        result = execute_tool(block.name, block.input)
                        assistant_content.append({
                            "type": "tool_use",
                            "id": block.id,
                            "name": block.name,
                            "input": block.input,
                        })
                        tool_results.append({
                            "type": "tool_result",
                            "tool_use_id": block.id,
                            "content": result,
                        })
                    elif block.type == "text":
                        assistant_content.append({
                            "type": "text",
                            "text": block.text,
                        })

                # Add assistant message with tool uses
                messages.append({"role": "assistant", "content": assistant_content})
                # Add tool results
                messages.append({"role": "user", "content": tool_results})
                continue  # Let Claude process the tool results

            # No more tool calls — extract final text
            texts = []
            for block in response.content:
                if hasattr(block, "text"):
                    texts.append(block.text)

            return "\n".join(texts) if texts else "（AI 沒有產生回覆）"

        return "（工具呼叫次數超過上限）"

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
