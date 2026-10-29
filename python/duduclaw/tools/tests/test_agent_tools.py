"""Unit tests for AgentTools name validation (M21 regression).

``agent_status`` / ``_set_status`` must reject names that could escape the
agents directory via path traversal, before the name is ever used in a path.
"""
from __future__ import annotations

import asyncio

import pytest

from duduclaw.tools import agent_tools
from duduclaw.tools.agent_tools import AgentTools, _is_valid_agent_id


@pytest.fixture(autouse=True)
def configured_cli(tmp_path, monkeypatch):
    binary = tmp_path / "trusted bin" / "duduclaw"
    binary.parent.mkdir()
    binary.write_text("fake executable; never run")
    binary.chmod(0o700)
    monkeypatch.setenv("DUDUCLAW_BIN", str(binary))
    return binary


@pytest.mark.parametrize(
    "name",
    [
        "../../etc/passwd",
        "..",
        "foo/../bar",
        "a/b",
        "UPPER",          # uppercase not allowed by the strict allowlist
        "with space",
        "",
        "name\x00null",
    ],
)
def test_invalid_agent_ids_rejected(name):
    assert _is_valid_agent_id(name) is False


@pytest.mark.parametrize("name", ["alpha", "agent-1", "a", "my-agent-42"])
def test_valid_agent_ids_accepted(name):
    assert _is_valid_agent_id(name) is True


def test_agent_status_rejects_traversal(tmp_path, monkeypatch):
    monkeypatch.setenv("DUDUCLAW_HOME", str(tmp_path))
    tools = AgentTools()
    result = asyncio.run(tools.agent_status("../../../../etc/passwd"))
    assert result["status"] == "invalid_name"


def test_set_status_rejects_traversal(tmp_path, monkeypatch):
    monkeypatch.setenv("DUDUCLAW_HOME", str(tmp_path))
    tools = AgentTools()
    result = asyncio.run(tools.agent_pause("../../secret"))
    assert result["success"] is False
    assert "Invalid agent name" in result["error"]


@pytest.mark.parametrize("status,action", [("paused", "pause"), ("active", "resume")])
@pytest.mark.parametrize("returncode", [0, 1])
def test_status_delegates_to_locked_rust_writer(
    tmp_path, monkeypatch, configured_cli, status, action, returncode,
):
    monkeypatch.setenv("DUDUCLAW_HOME", str(tmp_path))
    path = tmp_path / "agents" / "a1" / "agent.toml"
    path.parent.mkdir(parents=True)
    original = "[agent]\nname='a1'\nstatus='archived'\n"
    path.write_text(original)
    calls = []

    class Process:
        async def communicate(self):
            return b"", b"barrier refused" if returncode else b""

    async def spawn(*args, **kwargs):
        calls.append((args, kwargs))
        proc = Process()
        proc.returncode = returncode
        return proc

    monkeypatch.setattr(asyncio, "create_subprocess_exec", spawn)
    result = asyncio.run(AgentTools()._set_status("a1", status))
    assert result["success"] is (returncode == 0)
    assert calls[0][0] == (str(configured_cli), "agent", action, "a1")
    assert calls[0][1]["env"]["DUDUCLAW_HOME"] == str(tmp_path)
    # Only the Rust process may write; the Python adapter never rewrites TOML.
    assert path.read_text() == original


def test_status_missing_rust_cli_leaves_profile_unchanged(tmp_path, monkeypatch):
    monkeypatch.setenv("DUDUCLAW_HOME", str(tmp_path))
    path = tmp_path / "agents" / "a1" / "agent.toml"
    path.parent.mkdir(parents=True)
    original = "[agent]\nname='a1'\nstatus='active'\n"
    path.write_text(original)

    async def spawn(*args, **kwargs):
        raise FileNotFoundError("duduclaw")

    monkeypatch.setattr(asyncio, "create_subprocess_exec", spawn)
    result = asyncio.run(AgentTools().agent_pause("a1"))
    assert result["success"] is False
    assert "Rust CLI unavailable" in result["error"]
    assert path.read_text() == original


def test_status_wait_failure_reports_unknown_result(tmp_path, monkeypatch):
    monkeypatch.setenv("DUDUCLAW_HOME", str(tmp_path))
    path = tmp_path / "agents" / "a1" / "agent.toml"
    path.parent.mkdir(parents=True)
    path.write_text("[agent]\nname='a1'\nstatus='active'\n")

    class Process:
        returncode = None

        async def communicate(self):
            # The child may commit before the pipe fails.
            path.write_text("[agent]\nname='a1'\nstatus='paused'\n")
            raise OSError("pipe failed")

        def terminate(self):
            self.returncode = -15

        async def wait(self):
            return self.returncode

    async def spawn(*args, **kwargs):
        return Process()

    monkeypatch.setattr(asyncio, "create_subprocess_exec", spawn)
    result = asyncio.run(AgentTools().agent_pause("a1"))
    assert result["success"] is False
    assert "result unknown" in result["error"]
    assert "unchanged" not in result["error"]


@pytest.mark.parametrize(
    "configured",
    [None, "", "duduclaw", "./duduclaw", "~/duduclaw", "missing", "directory", "not-executable"],
)
def test_status_requires_trusted_absolute_cli(tmp_path, monkeypatch, configured):
    monkeypatch.setenv("DUDUCLAW_HOME", str(tmp_path))
    profile = tmp_path / "agents" / "a1" / "agent.toml"
    profile.parent.mkdir(parents=True)
    profile.write_text("[agent]\nstatus='active'\n")
    if configured is None:
        monkeypatch.delenv("DUDUCLAW_BIN")
    else:
        if configured == "missing":
            configured = str(tmp_path / "absent")
        elif configured == "directory":
            configured = str(tmp_path)
        elif configured == "not-executable":
            path = tmp_path / "not-executable"
            path.write_text("not executable")
            path.chmod(0o600)
            configured = str(path)
        monkeypatch.setenv("DUDUCLAW_BIN", configured)
    # Even a PATH entry must never be used as an implicit fallback.
    monkeypatch.setenv("PATH", str(tmp_path / "trusted bin"))

    async def spawn(*args, **kwargs):
        pytest.fail("Invalid CLI configuration must fail before spawning")

    monkeypatch.setattr(asyncio, "create_subprocess_exec", spawn)
    result = asyncio.run(AgentTools().agent_pause("a1"))
    assert result["success"] is False
    assert "DUDUCLAW_BIN" in result["error"]
    assert "absolute" in result["error"]


def test_cli_path_resolves_configured_symlink(tmp_path, monkeypatch, configured_cli):
    link = tmp_path / "cli-link"
    link.symlink_to(configured_cli)
    monkeypatch.setenv("DUDUCLAW_BIN", str(link))
    assert agent_tools._resolve_cli_path() == str(configured_cli)


class StuckProcess:
    def __init__(self, ignores_terminate):
        self.returncode = None
        self.ignores_terminate = ignores_terminate
        self.started = asyncio.Event()
        self.terminated = asyncio.Event()
        self.exited = asyncio.Event()
        self.signals = []
        self.reaped = False

    async def communicate(self):
        self.started.set()
        await self.exited.wait()
        await self.wait()
        return b"", b""

    def terminate(self):
        self.signals.append("terminate")
        self.terminated.set()
        if not self.ignores_terminate:
            self.returncode = -15
            self.exited.set()

    def kill(self):
        self.signals.append("kill")
        self.returncode = -9
        self.exited.set()

    async def wait(self):
        await self.exited.wait()
        self.reaped = True
        return self.returncode


@pytest.mark.parametrize("cancel", ["timeout", "once", "twice"])
@pytest.mark.parametrize("ignores_terminate", [False, True])
def test_status_timeout_or_cancellation_reaps_child(
    tmp_path, monkeypatch, cancel, ignores_terminate,
):
    monkeypatch.setenv("DUDUCLAW_HOME", str(tmp_path))
    path = tmp_path / "agents" / "a1" / "agent.toml"
    path.parent.mkdir(parents=True)
    path.write_text("[agent]\nstatus='active'\n")
    monkeypatch.setattr(agent_tools, "_STATUS_TIMEOUT_SECONDS", 0.01)
    monkeypatch.setattr(agent_tools, "_TERMINATE_GRACE_SECONDS", 0.01)

    async def scenario():
        child = StuckProcess(ignores_terminate)

        async def spawn(*args, **kwargs):
            return child

        monkeypatch.setattr(asyncio, "create_subprocess_exec", spawn)
        task = asyncio.create_task(AgentTools().agent_pause("a1"))
        await child.started.wait()
        if cancel != "timeout":
            task.cancel()
            if cancel == "twice":
                await child.terminated.wait()
                task.cancel()
            with pytest.raises(asyncio.CancelledError, match="unknown"):
                await task
        else:
            result = await task
            assert result["success"] is False
            assert "unknown" in result["error"]
            assert "re-read" in result["error"]
            assert "unchanged" not in result["error"]
        assert child.signals == (["terminate", "kill"] if ignores_terminate else ["terminate"])
        assert child.reaped

    asyncio.run(asyncio.wait_for(scenario(), 0.5))
