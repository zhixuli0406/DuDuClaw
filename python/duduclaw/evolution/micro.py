"""Micro Reflection - triggered after each conversation"""
import asyncio
import logging
import os
import subprocess
from datetime import datetime, timezone
from pathlib import Path

logger = logging.getLogger(__name__)


def _call_claude(prompt: str, model: str = "claude-haiku-4-5") -> str:
    """Call the claude CLI subprocess for a quick reflection prompt."""
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
            timeout=60,
            env={**os.environ, "ANTHROPIC_API_KEY": api_key},
        )
        return result.stdout.strip() if result.returncode == 0 else ""
    except Exception as e:
        logger.debug("claude CLI call failed: %s", e)
        return ""


def _find_claude() -> str:
    """Find the claude CLI binary."""
    import shutil
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


class MicroReflection:
    """Layer 1: Micro Reflection - runs after each conversation ends"""

    def __init__(self, agent_id: str, memory_dir: Path):
        self.agent_id = agent_id
        self.memory_dir = memory_dir

    async def reflect(self, conversation_summary: str) -> dict:
        """Reflect on a completed conversation

        Returns:
            dict with keys: insights, candidate_skills, daily_note
        """
        now = datetime.now(tz=timezone.utc)
        daily_dir = self.memory_dir / now.strftime("%Y%m")
        daily_dir.mkdir(parents=True, exist_ok=True)

        daily_note_path = daily_dir / f"{now.strftime('%Y%m%d')}.md"

        # Generate reflection
        reflection = {
            "timestamp": now.isoformat(),
            "agent_id": self.agent_id,
            "summary": conversation_summary,
            "what_went_well": [],
            "what_could_improve": [],
            "patterns_noticed": [],
            "candidate_skills": [],
        }

        # Sanitize conversation summary to prevent prompt injection (C5)
        # Truncate and strip instruction-like patterns
        safe_summary = conversation_summary[:5000] if conversation_summary else ""
        safe_summary = safe_summary.replace("ignore previous", "[REDACTED]")
        safe_summary = safe_summary.replace("system prompt", "[REDACTED]")

        # Use Claude to generate structured reflection insights
        claude_insights = await asyncio.get_event_loop().run_in_executor(
            None,
            _call_claude,
            (
                f"You are a reflective AI agent. A conversation just ended.\n\n"
                f"<conversation_summary>\n{safe_summary}\n</conversation_summary>\n\n"
                f"Briefly answer in JSON with keys: what_went_well (list), "
                f"what_could_improve (list), patterns_noticed (list), "
                f"candidate_skills (list of skill names worth creating).\n"
                f"Respond ONLY with the JSON object, no other text."
            ),
        )
        if claude_insights:
            try:
                import json
                parsed = json.loads(claude_insights)
                reflection["what_went_well"] = parsed.get("what_went_well", [])
                reflection["what_could_improve"] = parsed.get("what_could_improve", [])
                reflection["patterns_noticed"] = parsed.get("patterns_noticed", [])
                reflection["candidate_skills"] = parsed.get("candidate_skills", [])
            except (json.JSONDecodeError, ValueError):
                pass  # Ignore malformed JSON; fall back to empty lists

        daily_entry = self._format_daily_entry(now, conversation_summary, reflection)

        # Append to daily note
        with open(daily_note_path, "a", encoding="utf-8") as f:
            f.write(daily_entry)

        logger.info(
            f"Micro reflection saved for {self.agent_id}: {daily_note_path}"
        )
        return reflection

    def _format_daily_entry(self, timestamp: datetime, summary: str, reflection: dict | None = None) -> str:
        entry = f"\n## {timestamp.strftime('%H:%M:%S')}\n\n{summary}\n"
        if reflection:
            if reflection.get("what_went_well"):
                entry += "\n**Went well:** " + "; ".join(reflection["what_went_well"]) + "\n"
            if reflection.get("what_could_improve"):
                entry += "**Improve:** " + "; ".join(reflection["what_could_improve"]) + "\n"
            if reflection.get("candidate_skills"):
                entry += "**Candidate skills:** " + ", ".join(reflection["candidate_skills"]) + "\n"
        entry += "\n---\n"
        return entry
