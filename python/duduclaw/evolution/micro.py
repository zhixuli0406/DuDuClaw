"""Micro Reflection - triggered after each conversation"""
import asyncio
import functools
import logging
import re
from datetime import datetime, timezone
from pathlib import Path

from .llm_call import call_llm

logger = logging.getLogger(__name__)


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
        # Truncate, escape XML-like tags, strip instruction-like patterns
        safe_summary = conversation_summary[:5000] if conversation_summary else ""
        safe_summary = safe_summary.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")
        safe_summary = re.sub(r"(?i)ignore\s+previous", "[REDACTED]", safe_summary)
        safe_summary = re.sub(r"(?i)system\s+prompt", "[REDACTED]", safe_summary)

        # Use local LLM (preferred) or Claude to generate reflection insights
        prompt = (
            f"You are a reflective AI agent. A conversation just ended.\n\n"
            f"<conversation_summary>\n{safe_summary}\n</conversation_summary>\n\n"
            f"Briefly answer in JSON with keys: what_went_well (list), "
            f"what_could_improve (list), patterns_noticed (list), "
            f"candidate_skills (list of skill names worth creating).\n"
            f"Respond ONLY with the JSON object, no other text."
        )
        claude_insights = await asyncio.get_running_loop().run_in_executor(
            None, functools.partial(call_llm, prompt, timeout=60),
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
