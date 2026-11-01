"""Agent management MCP tools"""
import asyncio
import logging
import os
import re
from pathlib import Path

logger = logging.getLogger(__name__)

# Valid agent name: lowercase alphanumeric + hyphens, 1-64 chars
_VALID_AGENT_NAME = re.compile(r"^[a-z0-9][a-z0-9-]{0,62}[a-z0-9]?$")
_STATUS_TIMEOUT_SECONDS = 45
_TERMINATE_GRACE_SECONDS = 2
_STATUS_UNKNOWN = "Rust CLI result unknown; re-read agent status before retrying"


def _resolve_cli_path() -> str:
    """Require the operator's absolute CLI override; never search inherited PATH."""
    configured = os.environ.get("DUDUCLAW_BIN", "")
    error = "DUDUCLAW_BIN must name a trusted absolute path to an executable Rust CLI"
    if not configured or not Path(configured).is_absolute():
        raise ValueError(error)
    try:
        binary = Path(configured).resolve(strict=True)
        if not binary.is_file() or not os.access(binary, os.X_OK):
            raise ValueError(error)
    except (OSError, RuntimeError) as exc:
        raise ValueError(error) from exc
    return str(binary)


async def _terminate_status_child(process: asyncio.subprocess.Process) -> None:
    """Stop and reap a child while draining its pipes to avoid wait deadlocks."""
    if process.returncode is None:
        try:
            process.terminate()
        except ProcessLookupError:
            pass  # The child exited between the returncode check and the signal.
    try:
        await asyncio.wait_for(process.communicate(), _TERMINATE_GRACE_SECONDS)
    except (asyncio.TimeoutError, OSError):
        if process.returncode is None:
            try:
                process.kill()
            except ProcessLookupError:
                pass
        # Continue draining after kill; wait() alone can block on full pipes.
        # A failed pipe cannot be drained, but the child still must be reaped.
        try:
            await asyncio.wait_for(process.communicate(), _TERMINATE_GRACE_SECONDS)
        except (asyncio.TimeoutError, OSError):
            pass
    finally:
        await process.wait()


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
            return {"success": False, "error": "Invalid agent name: must be lowercase alphanumeric with hyphens, 1-64 chars"}

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
        # Validate name before using it in a path (prevents traversal, M21)
        if not _is_valid_agent_id(name):
            return {"name": name, "status": "invalid_name"}

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
        # Validate name before using it in a path (prevents traversal, M21)
        if not _is_valid_agent_id(name):
            return {"success": False, "error": f"Invalid agent name: {name!r}"}

        agents_dir = _get_agents_dir()
        toml_path = agents_dir / name / "agent.toml"
        if not toml_path.exists():
            return {"success": False, "error": f"Agent '{name}' not found"}

        # Delegate to the Rust profile writer: it re-reads under the file lock
        # and drains admission before a lifecycle change. Never write a snapshot.
        action = {"paused": "pause", "active": "resume"}.get(status)
        if action is None:
            return {"success": False, "error": "Unsupported agent status"}
        try:
            binary = _resolve_cli_path()
        except ValueError as exc:
            return {"success": False, "error": str(exc)}
        try:
            process = await asyncio.create_subprocess_exec(
                binary, "agent", action, name,
                env={**os.environ, "DUDUCLAW_HOME": str(agents_dir.parent)},
                stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE,
            )
        except OSError as exc:
            return {"success": False, "error": f"Rust CLI unavailable; status unchanged: {exc}"}
        try:
            _, stderr = await asyncio.wait_for(process.communicate(), _STATUS_TIMEOUT_SECONDS)
        except (asyncio.TimeoutError, asyncio.CancelledError, OSError) as exc:
            # Shield cleanup even from repeated cancellation. A killed child may
            # already have committed, so neither failure implies "unchanged".
            cancelled = isinstance(exc, asyncio.CancelledError)
            cleanup = asyncio.create_task(_terminate_status_child(process))
            while not cleanup.done():
                try:
                    await asyncio.shield(cleanup)
                except asyncio.CancelledError:
                    cancelled = True
            cleanup.result()
            if cancelled:
                raise asyncio.CancelledError(_STATUS_UNKNOWN) from exc
            reason = "timed out" if isinstance(exc, asyncio.TimeoutError) else str(exc)
            return {"success": False, "error": f"{_STATUS_UNKNOWN}: {reason}"}
        if process.returncode != 0:
            return {"success": False, "error": stderr.decode(errors="replace")[:2000] or "Rust CLI refused the status change"}
        return {"success": True, "name": name, "status": status}
