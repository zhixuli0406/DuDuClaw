"""Unit tests for AgentTools name validation (M21 regression).

``agent_status`` / ``_set_status`` must reject names that could escape the
agents directory via path traversal, before the name is ever used in a path.
"""
from __future__ import annotations

import asyncio

import pytest

from duduclaw.tools.agent_tools import AgentTools, _is_valid_agent_id


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
