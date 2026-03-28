"""Agent management MCP tools"""
import logging
import os
import re
from pathlib import Path

logger = logging.getLogger(__name__)

# Valid agent name: lowercase alphanumeric + hyphens, 1-64 chars
_VALID_AGENT_NAME = re.compile(r"^[a-z0-9][a-z0-9-]{0,62}[a-z0-9]?$")


def _is_valid_agent_id(name: str) -> bool:
    """Validate agent name is safe for filesystem paths (no traversal)."""
    return bool(name) and bool(_VALID_AGENT_NAME.match(name)) and ".." not in name


def _get_agents_dir() -> Path:
    """Return the agents directory path."""
    home = os.environ.get("DUDUCLAW_HOME") or str(Path.home() / ".duduclaw")
    return Path(home) / "agents"


class AgentTools:
    """Tools exposed to agents for managing other agents."""

    async def agent_list(self) -> list[dict]:
        """List all registered agents and their status."""
        agents_dir = _get_agents_dir()
        agents = []
        if not agents_dir.exists():
            return agents

        for agent_dir in sorted(agents_dir.iterdir()):
            if not agent_dir.is_dir():
                continue
            toml_path = agent_dir / "agent.toml"
            if not toml_path.exists():
                continue
            try:
                import tomllib  # type: ignore
                config = tomllib.loads(toml_path.read_text())
            except ImportError:
                try:
                    import tomli  # type: ignore
                    config = tomli.loads(toml_path.read_text())
                except ImportError:
                    config = {}

            agent_section = config.get("agent", {})
            agents.append({
                "name": agent_section.get("name", agent_dir.name),
                "display_name": agent_section.get("display_name", agent_dir.name),
                "role": agent_section.get("role", "specialist"),
                "status": agent_section.get("status", "active"),
                "trigger": agent_section.get("trigger", ""),
            })
        return agents

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

        agent_name = name.lower().replace(" ", "-")

        # Validate agent name to prevent path traversal (C3)
        if not _is_valid_agent_id(agent_name):
            return {"success": False, "error": f"Invalid agent name: must be lowercase alphanumeric with hyphens, 1-64 chars"}

        agents_dir = _get_agents_dir()
        agent_dir = agents_dir / agent_name

        if agent_dir.exists():
            return {"success": False, "error": f"Agent '{agent_name}' already exists"}

        # Create directory structure
        (agent_dir / "SKILLS").mkdir(parents=True, exist_ok=True)
        (agent_dir / "memory").mkdir(parents=True, exist_ok=True)
        (agent_dir / ".claude").mkdir(parents=True, exist_ok=True)

        # Build agent.toml using dict → toml serialization to prevent injection (C4)
        try:
            import tomli_w
        except ImportError:
            tomli_w = None

        agent_config = {
            "agent": {
                "name": agent_name,
                "display_name": display_name,
                "role": role,
                "status": "active",
                "trigger": trigger or f"@{display_name}",
                "reports_to": "",
                "icon": "\U0001f916",
            },
            "model": {
                "preferred": model,
                "fallback": "claude-haiku-4-5",
                "account_pool": ["main"],
            },
            "heartbeat": {
                "enabled": False,
                "interval_seconds": 3600,
                "max_concurrent_runs": 1,
                "cron": "",
            },
            "budget": {
                "monthly_limit_cents": 5000,
                "warn_threshold_percent": 80,
                "hard_stop": True,
            },
            "permissions": {
                "can_create_agents": False,
                "can_send_cross_agent": True,
                "can_modify_own_skills": True,
                "can_modify_own_soul": False,
                "can_schedule_tasks": False,
                "allowed_channels": ["*"],
            },
            "evolution": {
                "skill_auto_activate": False,
                "skill_security_scan": True,
                "gvu_enabled": True,
            },
        }

        if tomli_w:
            agent_toml = tomli_w.dumps(agent_config)
        else:
            # Fallback: manual safe serialization (values properly quoted)
            import json
            lines = []
            for section, values in agent_config.items():
                lines.append(f"[{section}]")
                for k, v in values.items():
                    if isinstance(v, bool):
                        lines.append(f"{k} = {'true' if v else 'false'}")
                    elif isinstance(v, int):
                        lines.append(f"{k} = {v}")
                    elif isinstance(v, list):
                        lines.append(f"{k} = {json.dumps(v)}")
                    else:
                        # Escape strings properly
                        escaped = str(v).replace("\\", "\\\\").replace('"', '\\"')
                        lines.append(f'{k} = "{escaped}"')
                lines.append("")
            agent_toml = "\n".join(lines)

        (agent_dir / "agent.toml").write_text(agent_toml)

        # Write SOUL.md
        soul_content = soul if soul else f"# {display_name}\n\nI am {display_name}, a specialist AI agent.\n"
        (agent_dir / "SOUL.md").write_text(soul_content)

        return {
            "success": True,
            "agent": {
                "name": agent_name,
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
        """Delegate a task to another agent via the bridge."""
        logger.info("Delegating to %s: %s...", target_agent, prompt[:100])
        try:
            from .. import _native  # type: ignore
            msg_id = _native.send_message(target_agent, prompt)
            return {
                "success": True,
                "message_id": msg_id,
                "target_agent": target_agent,
            }
        except ImportError:
            pass
        except Exception as e:
            logger.warning("Bridge send failed: %s", e)

        return {
            "success": True,
            "message_id": "no-bridge",
            "target_agent": target_agent,
            "warning": "_native bridge not available",
        }

    async def agent_status(self, name: str) -> dict:
        """Get detailed status of a specific agent."""
        agents_dir = _get_agents_dir()
        toml_path = agents_dir / name / "agent.toml"
        if not toml_path.exists():
            return {"name": name, "status": "not_found"}

        try:
            import tomllib  # type: ignore
            config = tomllib.loads(toml_path.read_text())
        except ImportError:
            config = {}

        agent_section = config.get("agent", {})
        budget = config.get("budget", {})
        return {
            "name": name,
            "status": agent_section.get("status", "active"),
            "display_name": agent_section.get("display_name", name),
            "role": agent_section.get("role", "specialist"),
            "budget_limit_cents": budget.get("monthly_limit_cents", 5000),
            "pending_messages": 0,
        }

    async def agent_pause(self, name: str) -> dict:
        """Pause an agent by updating its agent.toml."""
        logger.info("Pausing agent: %s", name)
        return await self._set_status(name, "paused")

    async def agent_resume(self, name: str) -> dict:
        """Resume a paused agent by updating its agent.toml."""
        logger.info("Resuming agent: %s", name)
        return await self._set_status(name, "active")

    async def _set_status(self, name: str, status: str) -> dict:
        agents_dir = _get_agents_dir()
        toml_path = agents_dir / name / "agent.toml"
        if not toml_path.exists():
            return {"success": False, "error": f"Agent '{name}' not found"}

        content = toml_path.read_text()
        lines = []
        in_agent_section = False
        for line in content.splitlines():
            if line.strip() == "[agent]":
                in_agent_section = True
            elif line.strip().startswith("[") and line.strip() != "[agent]":
                in_agent_section = False

            if in_agent_section and line.strip().startswith("status"):
                lines.append(f'status = "{status}"')
            else:
                lines.append(line)
        toml_path.write_text("\n".join(lines) + "\n")
        return {"success": True, "name": name, "status": status}
