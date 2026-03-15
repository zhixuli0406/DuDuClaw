"""Agent management MCP tools"""
import logging
from typing import Optional

logger = logging.getLogger(__name__)


class AgentTools:
    """Tools exposed to agents for managing other agents."""

    async def agent_list(self) -> list[dict]:
        """List all registered agents and their status."""
        # TODO: Call into Rust via _native bridge
        return [
            {"name": "dudu", "role": "main", "status": "active"},
        ]

    async def agent_create(
        self,
        name: str,
        display_name: str,
        role: str = "specialist",
        trigger: str = "",
        soul: str = "",
        model: str = "claude-sonnet-4-6",
    ) -> dict:
        """Create a new agent dynamically."""
        logger.info("Creating agent: %s (%s)", name, display_name)
        # TODO: Write agent directory + agent.toml + SOUL.md
        return {
            "success": True,
            "agent": {
                "name": name,
                "display_name": display_name,
                "role": role,
                "status": "active",
            },
        }

    async def agent_delegate(
        self,
        target_agent: str,
        prompt: str,
        wait_for_response: bool = False,
    ) -> dict:
        """Delegate a task to another agent."""
        logger.info("Delegating to %s: %s...", target_agent, prompt[:100])
        # TODO: Send IPC message via broker
        return {
            "success": True,
            "message_id": "placeholder-id",
            "target_agent": target_agent,
        }

    async def agent_status(self, name: str) -> dict:
        """Get detailed status of a specific agent."""
        # TODO: Query from registry
        return {
            "name": name,
            "status": "active",
            "budget_used_cents": 0,
            "budget_limit_cents": 5000,
            "pending_messages": 0,
        }

    async def agent_pause(self, name: str) -> dict:
        """Pause an agent."""
        logger.info("Pausing agent: %s", name)
        return {"success": True, "name": name, "status": "paused"}

    async def agent_resume(self, name: str) -> dict:
        """Resume a paused agent."""
        logger.info("Resuming agent: %s", name)
        return {"success": True, "name": name, "status": "active"}
