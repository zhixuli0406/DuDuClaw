"""Micro Reflection - triggered after each conversation"""
import logging
from datetime import datetime, timezone
from pathlib import Path

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

        # TODO: Use Claude to generate actual reflection
        # For now, create a structured daily note
        daily_entry = self._format_daily_entry(now, conversation_summary)

        # Append to daily note
        with open(daily_note_path, "a", encoding="utf-8") as f:
            f.write(daily_entry)

        logger.info(
            f"Micro reflection saved for {self.agent_id}: {daily_note_path}"
        )
        return reflection

    def _format_daily_entry(self, timestamp: datetime, summary: str) -> str:
        return f"\n## {timestamp.strftime('%H:%M:%S')}\n\n{summary}\n\n---\n"
