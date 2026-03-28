"""Shared LLM call helper for evolution reflections.

Tries local inference first (via DuDuClaw's OpenAI-compatible endpoint),
falls back to Claude CLI. This avoids burning cloud API tokens on
reflections that a local model can handle.

Environment variables:
    DUDUCLAW_LOCAL_URL: Local inference endpoint (default: http://127.0.0.1:18789/v1)
    ANTHROPIC_API_KEY: Required for Claude CLI fallback
"""

import logging
import os
import shutil
import subprocess
from typing import Optional

logger = logging.getLogger(__name__)

# Default local inference endpoint (DuDuClaw gateway exposes OpenAI-compat)
_DEFAULT_LOCAL_URL = "http://127.0.0.1:18789/v1"


def call_llm(
    prompt: str,
    model: str = "claude-haiku-4-5",
    timeout: int = 60,
    prefer_local: bool = True,
) -> str:
    """Call an LLM for evolution reflections — tries local first, then cloud.

    Args:
        prompt: The prompt text.
        model: Cloud model name (used only for Claude CLI fallback).
        timeout: Timeout in seconds.
        prefer_local: If True, try local inference before Claude CLI.

    Returns:
        Response text, or empty string on failure.
    """
    if prefer_local:
        result = _call_local(prompt, timeout)
        if result:
            return result
        logger.debug("Local inference unavailable or empty, falling back to Claude CLI")

    return _call_claude(prompt, model, timeout)


def _call_local(prompt: str, timeout: int = 60) -> str:
    """Try calling the local inference engine via OpenAI-compatible HTTP API."""
    base_url = os.environ.get("DUDUCLAW_LOCAL_URL", _DEFAULT_LOCAL_URL)

    # Validate URL points to localhost only — prevent SSRF via env var injection
    import urllib.parse
    parsed = urllib.parse.urlparse(base_url)
    if parsed.hostname not in ("127.0.0.1", "localhost", "::1"):
        logger.warning(
            "DUDUCLAW_LOCAL_URL must point to localhost (got %s), ignoring",
            parsed.hostname,
        )
        base_url = _DEFAULT_LOCAL_URL

    try:
        import urllib.request
        import json

        url = f"{base_url}/chat/completions"
        payload = json.dumps({
            "model": "default",
            "messages": [{"role": "user", "content": prompt}],
            "max_tokens": 2048,
            "temperature": 0.3,
        }).encode("utf-8")

        req = urllib.request.Request(
            url,
            data=payload,
            headers={"Content-Type": "application/json"},
            method="POST",
        )

        with urllib.request.urlopen(req, timeout=timeout) as resp:
            if resp.status != 200:
                return ""
            data = json.loads(resp.read().decode("utf-8"))
            choices = data.get("choices", [])
            if choices:
                return choices[0].get("message", {}).get("content", "").strip()
            return ""
    except Exception as e:
        logger.debug("Local inference call failed: %s", e)
        return ""


def _call_claude(prompt: str, model: str = "claude-haiku-4-5", timeout: int = 60) -> str:
    """Call the Claude CLI subprocess."""
    api_key = os.environ.get("ANTHROPIC_API_KEY", "")
    if not api_key:
        return ""

    claude = _find_claude()
    if not claude:
        return ""

    try:
        result = subprocess.run(
            [claude, "-p", prompt, "--model", model, "--output-format", "text"],
            capture_output=True,
            text=True,
            timeout=timeout,
            env={**os.environ, "ANTHROPIC_API_KEY": api_key},
        )
        return result.stdout.strip() if result.returncode == 0 else ""
    except Exception as e:
        logger.debug("Claude CLI call failed: %s", e)
        return ""


def _find_claude() -> str:
    """Find the claude CLI binary."""
    path = shutil.which("claude")
    if path:
        return path
    home = os.environ.get("HOME", "")
    for candidate in [
        f"{home}/.npm-global/bin/claude",
        "/usr/local/bin/claude",
        f"{home}/.claude/bin/claude",
        f"{home}/.local/bin/claude",
    ]:
        if os.path.exists(candidate):
            return candidate
    return ""
