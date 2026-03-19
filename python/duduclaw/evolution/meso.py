"""Meso Reflection - triggered by heartbeat scheduler"""
import asyncio
import logging
import os
import subprocess
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import List

logger = logging.getLogger(__name__)


def _call_claude(prompt: str, model: str = "claude-haiku-4-5") -> str:
    """Call the claude CLI subprocess."""
    api_key = os.environ.get("ANTHROPIC_API_KEY", "")
    if not api_key:
        return ""
    import shutil
    claude = shutil.which("claude") or ""
    if not claude:
        home = os.environ.get("HOME", "")
        for c in [f"{home}/.npm-global/bin/claude", "/usr/local/bin/claude",
                  f"{home}/.claude/bin/claude", f"{home}/.local/bin/claude"]:
            if os.path.exists(c):
                claude = c
                break
    if not claude:
        return ""
    try:
        result = subprocess.run(
            [claude, "-p", prompt, "--model", model, "--output-format", "text"],
            capture_output=True, text=True, timeout=90,
            env={**os.environ, "ANTHROPIC_API_KEY": api_key},
        )
        return result.stdout.strip() if result.returncode == 0 else ""
    except Exception as e:
        logger.debug("claude CLI call failed: %s", e)
        return ""


class MesoReflection:
    """Layer 2: Meso Reflection - runs on heartbeat (hourly)"""

    def __init__(self, agent_id: str, agent_dir: Path):
        self.agent_id = agent_id
        self.agent_dir = agent_dir
        self.memory_dir = agent_dir / "memory"
        self.skills_dir = agent_dir / "SKILLS"

    async def reflect(self) -> dict:
        """Review recent daily notes and extract patterns"""
        recent_notes = self._load_recent_notes(days=3)

        result = {
            "timestamp": datetime.now(tz=timezone.utc).isoformat(),
            "agent_id": self.agent_id,
            "notes_reviewed": len(recent_notes),
            "common_patterns": [],
            "memory_updates": [],
            "candidate_skills": [],
        }

        if not recent_notes:
            logger.info(
                f"No recent notes for meso reflection: {self.agent_id}"
            )
            return result

        # Use Claude to analyze patterns across the recent daily notes
        combined = "\n\n---\n\n".join(recent_notes)
        analysis = await asyncio.get_event_loop().run_in_executor(
            None,
            _call_claude,
            (
                f"You are a reflective AI agent reviewing your own daily logs.\n\n"
                f"Recent daily notes (last 3 days):\n{combined[:4000]}\n\n"
                f"Identify common patterns, frequent topics, and candidate skills to formalise.\n"
                f"Respond in JSON with keys: common_patterns (list of strings), "
                f"memory_updates (list of strings), candidate_skills (list of strings)."
            ),
        )
        if analysis:
            try:
                import json
                parsed = json.loads(analysis)
                result["common_patterns"] = parsed.get("common_patterns", [])
                result["memory_updates"] = parsed.get("memory_updates", [])
                result["candidate_skills"] = parsed.get("candidate_skills", [])
            except (json.JSONDecodeError, ValueError):
                pass

        logger.info(
            f"Meso reflection for {self.agent_id}: "
            f"reviewed {len(recent_notes)} notes"
        )
        return result

    def _load_recent_notes(self, days: int = 3) -> List[str]:
        """Load daily notes from the last N days"""
        notes = []
        now = datetime.now(tz=timezone.utc)

        for i in range(days):
            date = now - timedelta(days=i)
            note_path = (
                self.memory_dir
                / date.strftime("%Y%m")
                / f"{date.strftime('%Y%m%d')}.md"
            )
            if note_path.exists():
                notes.append(note_path.read_text(encoding="utf-8"))

        return notes

    async def update_memory(self, updates: List[str]) -> None:
        """Update MEMORY.md with extracted knowledge"""
        memory_path = self.agent_dir / "MEMORY.md"

        if not updates:
            return

        existing = ""
        if memory_path.exists():
            existing = memory_path.read_text(encoding="utf-8")

        new_section = (
            f"\n\n## Meso Reflection "
            f"({datetime.now(tz=timezone.utc).strftime('%Y-%m-%d %H:%M')})\n\n"
        )
        new_section += "\n".join(f"- {u}" for u in updates)

        memory_path.write_text(existing + new_section, encoding="utf-8")
        logger.info(f"Updated MEMORY.md for {self.agent_id}")
