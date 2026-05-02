"""Unit tests for duduclaw.agents.capabilities.loader

Test strategy:
  - _parse_registry / _parse_agent_entry: pure functions → easy to test with raw dicts
  - CapabilityRegistryLoader: uses tmp_path to avoid real filesystem side-effects
  - Bundled registry.yaml: smoke-test that the shipped YAML is valid
"""

from __future__ import annotations

import textwrap
from pathlib import Path

import pytest

from duduclaw.agents.capabilities.loader import (
    SCHEMA_VERSION,
    AgentCapability,
    CapabilityRegistry,
    CapabilityRegistryLoader,
    _parse_agent_entry,
    _parse_registry,
)


# ── _parse_agent_entry ────────────────────────────────────────────────────────


class TestParseAgentEntry:
    """Tests for the low-level per-entry parser."""

    def _valid_entry(self, **overrides):
        base = {
            "agent_id": "test-agent",
            "capabilities": ["coding", "testing"],
            "max_concurrent_tasks": 2,
            "description": "A test agent",
            "tags": ["eng"],
        }
        base.update(overrides)
        return base

    def test_parses_all_fields(self):
        cap = _parse_agent_entry(self._valid_entry(), 0)
        assert cap.agent_id == "test-agent"
        assert cap.capabilities == ("coding", "testing")
        assert cap.max_concurrent_tasks == 2
        assert cap.description == "A test agent"
        assert cap.tags == ("eng",)

    def test_optional_fields_have_defaults(self):
        entry = {"agent_id": "minimal", "capabilities": ["x"], "max_concurrent_tasks": 1}
        cap = _parse_agent_entry(entry, 0)
        assert cap.description == ""
        assert cap.tags == ()

    def test_immutable(self):
        cap = _parse_agent_entry(self._valid_entry(), 0)
        with pytest.raises((AttributeError, TypeError)):
            cap.agent_id = "mutated"  # type: ignore[misc]

    def test_missing_agent_id_raises(self):
        with pytest.raises(ValueError, match="agent_id"):
            _parse_agent_entry({"capabilities": ["x"], "max_concurrent_tasks": 1}, 0)

    def test_empty_agent_id_raises(self):
        with pytest.raises(ValueError, match="agent_id"):
            _parse_agent_entry(
                {"agent_id": "  ", "capabilities": ["x"], "max_concurrent_tasks": 1}, 0
            )

    def test_missing_capabilities_raises(self):
        with pytest.raises(ValueError, match="capabilities"):
            _parse_agent_entry({"agent_id": "x", "max_concurrent_tasks": 1}, 0)

    def test_capabilities_not_list_raises(self):
        with pytest.raises(ValueError, match="capabilities"):
            _parse_agent_entry(
                {"agent_id": "x", "capabilities": "not-a-list", "max_concurrent_tasks": 1}, 0
            )

    def test_missing_max_concurrent_raises(self):
        with pytest.raises(ValueError, match="max_concurrent_tasks"):
            _parse_agent_entry({"agent_id": "x", "capabilities": ["y"]}, 0)

    def test_zero_max_concurrent_raises(self):
        with pytest.raises(ValueError, match="max_concurrent_tasks"):
            _parse_agent_entry(
                {"agent_id": "x", "capabilities": ["y"], "max_concurrent_tasks": 0}, 0
            )

    def test_negative_max_concurrent_raises(self):
        with pytest.raises(ValueError, match="max_concurrent_tasks"):
            _parse_agent_entry(
                {"agent_id": "x", "capabilities": ["y"], "max_concurrent_tasks": -1}, 0
            )

    def test_bool_max_concurrent_raises(self):
        # bool is a subclass of int; True == 1 should still be rejected
        with pytest.raises(ValueError, match="max_concurrent_tasks"):
            _parse_agent_entry(
                {"agent_id": "x", "capabilities": ["y"], "max_concurrent_tasks": True}, 0
            )

    def test_tags_not_list_raises(self):
        with pytest.raises(ValueError, match="tags"):
            _parse_agent_entry(
                {
                    "agent_id": "x",
                    "capabilities": ["y"],
                    "max_concurrent_tasks": 1,
                    "tags": "not-a-list",
                },
                0,
            )

    def test_capabilities_coerced_to_strings(self):
        entry = {"agent_id": "x", "capabilities": [1, 2.0, True], "max_concurrent_tasks": 1}
        cap = _parse_agent_entry(entry, 0)
        assert cap.capabilities == ("1", "2.0", "True")


# ── _parse_registry ───────────────────────────────────────────────────────────


class TestParseRegistry:
    """Tests for the top-level registry parser."""

    def _minimal_registry(self):
        return {
            "schema_version": "1.0",
            "agents": [
                {
                    "agent_id": "alpha",
                    "capabilities": ["cap_a"],
                    "max_concurrent_tasks": 1,
                },
                {
                    "agent_id": "beta",
                    "capabilities": ["cap_b", "cap_a"],
                    "max_concurrent_tasks": 3,
                },
            ],
        }

    def test_parses_correctly(self):
        reg = _parse_registry(self._minimal_registry())
        assert isinstance(reg, CapabilityRegistry)
        assert reg.schema_version == "1.0"
        assert len(reg.agents) == 2
        assert "alpha" in reg.agents
        assert "beta" in reg.agents

    def test_immutable(self):
        reg = _parse_registry(self._minimal_registry())
        with pytest.raises((AttributeError, TypeError)):
            reg.schema_version = "mutated"  # type: ignore[misc]

    def test_root_not_dict_raises(self):
        with pytest.raises(ValueError, match="mapping"):
            _parse_registry(["not", "a", "dict"])

    def test_agents_not_list_raises(self):
        with pytest.raises(ValueError, match="agents"):
            _parse_registry({"schema_version": "1.0", "agents": "not-a-list"})

    def test_missing_schema_version_defaults(self):
        raw = {
            "agents": [{"agent_id": "x", "capabilities": ["y"], "max_concurrent_tasks": 1}]
        }
        reg = _parse_registry(raw)
        assert reg.schema_version == SCHEMA_VERSION

    def test_empty_agents_list(self):
        reg = _parse_registry({"schema_version": "1.0", "agents": []})
        assert reg.agents == {}

    def test_duplicate_agent_id_last_wins(self):
        raw = {
            "agents": [
                {"agent_id": "dupe", "capabilities": ["first"], "max_concurrent_tasks": 1},
                {"agent_id": "dupe", "capabilities": ["second"], "max_concurrent_tasks": 2},
            ]
        }
        reg = _parse_registry(raw)
        assert reg.agents["dupe"].capabilities == ("second",)


# ── CapabilityRegistryLoader ──────────────────────────────────────────────────


VALID_YAML = textwrap.dedent(
    """\
    schema_version: "1.0"
    agents:
      - agent_id: "worker-a"
        description: "Worker A"
        capabilities:
          - task_execution
          - reporting
        max_concurrent_tasks: 4
        tags:
          - worker
      - agent_id: "worker-b"
        capabilities:
          - task_execution
        max_concurrent_tasks: 2
    """
)

INVALID_YAML = textwrap.dedent(
    """\
    schema_version: "1.0"
    agents:
      - agent_id: "bad"
        capabilities: not-a-list
        max_concurrent_tasks: 1
    """
)


class TestCapabilityRegistryLoader:
    """Integration tests for CapabilityRegistryLoader."""

    def test_load_from_file(self, tmp_path: Path):
        yaml_file = tmp_path / "registry.yaml"
        yaml_file.write_text(VALID_YAML)
        loader = CapabilityRegistryLoader(registry_path=yaml_file)
        reg = loader.load()
        assert len(reg.agents) == 2
        assert "worker-a" in reg.agents

    def test_get_triggers_load(self, tmp_path: Path):
        yaml_file = tmp_path / "registry.yaml"
        yaml_file.write_text(VALID_YAML)
        loader = CapabilityRegistryLoader(registry_path=yaml_file)
        reg = loader.get()
        assert len(reg.agents) == 2

    def test_get_returns_cached(self, tmp_path: Path):
        yaml_file = tmp_path / "registry.yaml"
        yaml_file.write_text(VALID_YAML)
        loader = CapabilityRegistryLoader(registry_path=yaml_file)
        reg1 = loader.get()
        reg2 = loader.get()
        assert reg1 is reg2  # same object returned

    def test_reload_updates_registry(self, tmp_path: Path):
        yaml_file = tmp_path / "registry.yaml"
        yaml_file.write_text(VALID_YAML)
        loader = CapabilityRegistryLoader(registry_path=yaml_file)
        loader.load()
        assert len(loader.get().agents) == 2

        # Overwrite with a single-agent file
        yaml_file.write_text(
            textwrap.dedent(
                """\
                schema_version: "1.0"
                agents:
                  - agent_id: "only-one"
                    capabilities: ["single_cap"]
                    max_concurrent_tasks: 1
                """
            )
        )
        loader.load()
        assert len(loader.get().agents) == 1

    def test_load_invalid_yaml_with_stale_keeps_stale(self, tmp_path: Path):
        yaml_file = tmp_path / "registry.yaml"
        yaml_file.write_text(VALID_YAML)
        loader = CapabilityRegistryLoader(registry_path=yaml_file)
        loader.load()  # populate stale

        yaml_file.write_text(INVALID_YAML)
        result = loader.load()  # should return stale, not raise
        assert len(result.agents) == 2  # stale copy retained

    def test_load_invalid_yaml_no_stale_raises(self, tmp_path: Path):
        yaml_file = tmp_path / "registry.yaml"
        yaml_file.write_text(INVALID_YAML)
        loader = CapabilityRegistryLoader(registry_path=yaml_file)
        with pytest.raises(ValueError):
            loader.load()

    def test_missing_file_uses_bundled_fallback(self, tmp_path: Path):
        missing = tmp_path / "does_not_exist.yaml"
        loader = CapabilityRegistryLoader(registry_path=missing)
        # Bundled registry.yaml should be present in the package
        reg = loader.load()
        assert len(reg.agents) > 0

    def test_registry_path_property(self, tmp_path: Path):
        yaml_file = tmp_path / "r.yaml"
        yaml_file.write_text(VALID_YAML)
        loader = CapabilityRegistryLoader(registry_path=yaml_file)
        assert loader.registry_path == yaml_file

    def test_env_var_overrides_constructor_path(self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
        env_yaml = tmp_path / "env_registry.yaml"
        env_yaml.write_text(VALID_YAML)
        constructor_yaml = tmp_path / "constructor_registry.yaml"
        constructor_yaml.write_text(
            "schema_version: '1.0'\nagents: []\n"
        )
        monkeypatch.setenv("DUDUCLAW_CAPABILITY_REGISTRY", str(env_yaml))
        loader = CapabilityRegistryLoader(registry_path=constructor_yaml)
        reg = loader.load()
        # Should load from env path (2 agents), not constructor path (0 agents)
        assert len(reg.agents) == 2

    def test_stop_hot_reload_is_safe_when_not_started(self, tmp_path: Path):
        yaml_file = tmp_path / "r.yaml"
        yaml_file.write_text(VALID_YAML)
        loader = CapabilityRegistryLoader(registry_path=yaml_file)
        loader.stop_hot_reload()  # should not raise

    def test_stop_hot_reload_stops_active_watcher(self, tmp_path: Path):
        """stop_hot_reload clears _watcher even when .stop() succeeds."""
        yaml_file = tmp_path / "r.yaml"
        yaml_file.write_text(VALID_YAML)
        loader = CapabilityRegistryLoader(registry_path=yaml_file)
        loader.load()

        # Inject a fake watcher with a stop() method
        class _FakeWatcher:
            stopped = False

            def stop(self):
                _FakeWatcher.stopped = True

        loader._watcher = _FakeWatcher()
        loader.stop_hot_reload()

        assert _FakeWatcher.stopped
        assert loader._watcher is None

    def test_stop_hot_reload_clears_watcher_even_on_exception(self, tmp_path: Path):
        """_watcher is cleared even if .stop() raises."""
        yaml_file = tmp_path / "r.yaml"
        yaml_file.write_text(VALID_YAML)
        loader = CapabilityRegistryLoader(registry_path=yaml_file)

        class _BadWatcher:
            def stop(self):
                raise RuntimeError("stop exploded")

        loader._watcher = _BadWatcher()
        loader.stop_hot_reload()  # must not raise
        assert loader._watcher is None

    def test_reload_safe_swallows_errors(self, tmp_path: Path):
        """_reload_safe logs errors without raising."""
        yaml_file = tmp_path / "r.yaml"
        yaml_file.write_text(VALID_YAML)
        loader = CapabilityRegistryLoader(registry_path=yaml_file)
        loader.load()

        # Corrupt the file after initial load → reload raises, but _reload_safe absorbs it
        yaml_file.write_text(INVALID_YAML)
        loader._reload_safe()  # should not raise; stale registry survives
        assert loader.get() is not None

    def test_start_hot_reload_fallback_to_polling(self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
        """start_hot_reload falls back to polling when watchdog raises ImportError."""
        yaml_file = tmp_path / "r.yaml"
        yaml_file.write_text(VALID_YAML)
        loader = CapabilityRegistryLoader(registry_path=yaml_file)
        loader.load()

        # Patch _start_watchdog_reload to simulate missing watchdog
        monkeypatch.setattr(
            loader, "_start_watchdog_reload", lambda: (_ for _ in ()).throw(ImportError("no watchdog"))
        )
        polling_started = []
        original_polling = loader._start_polling_reload

        def _track_polling(interval):
            polling_started.append(interval)
            # Don't actually start a thread to keep tests fast

        monkeypatch.setattr(loader, "_start_polling_reload", _track_polling)
        loader.start_hot_reload(poll_interval_seconds=60.0)
        assert polling_started == [60.0]

    def test_resolve_path_raises_when_both_missing(self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
        """FileNotFoundError when configured path AND bundled fallback are absent."""
        import duduclaw.agents.capabilities.loader as loader_mod

        missing_config = tmp_path / "no_such_file.yaml"
        loader = CapabilityRegistryLoader(registry_path=missing_config)

        # Temporarily point bundled registry to a non-existent path
        original_bundled = loader_mod._BUNDLED_REGISTRY
        monkeypatch.setattr(loader_mod, "_BUNDLED_REGISTRY", tmp_path / "no_bundled.yaml")
        try:
            with pytest.raises(FileNotFoundError, match="No registry file found"):
                loader.load()
        finally:
            loader_mod._BUNDLED_REGISTRY = original_bundled


# ── Bundled registry smoke test ───────────────────────────────────────────────


class TestBundledRegistry:
    """Verify the bundled registry.yaml is well-formed."""

    def test_bundled_registry_loads_all_agents(self):
        loader = CapabilityRegistryLoader()
        # Force using the bundled file by asking the loader with no explicit path
        # (the default path likely doesn't exist in CI)
        from duduclaw.agents.capabilities.loader import _BUNDLED_REGISTRY

        loader2 = CapabilityRegistryLoader(registry_path=_BUNDLED_REGISTRY)
        reg = loader2.load()
        assert len(reg.agents) == 18, (
            f"Expected 18 agents in bundled registry, got {len(reg.agents)}"
        )

    def test_bundled_registry_schema_version(self):
        from duduclaw.agents.capabilities.loader import _BUNDLED_REGISTRY

        loader = CapabilityRegistryLoader(registry_path=_BUNDLED_REGISTRY)
        reg = loader.load()
        assert reg.schema_version == "1.0"

    def test_bundled_registry_all_required_fields(self):
        from duduclaw.agents.capabilities.loader import _BUNDLED_REGISTRY

        loader = CapabilityRegistryLoader(registry_path=_BUNDLED_REGISTRY)
        reg = loader.load()
        for agent_id, cap in reg.agents.items():
            assert cap.agent_id == agent_id
            assert len(cap.capabilities) > 0, f"{agent_id} has no capabilities"
            assert cap.max_concurrent_tasks >= 1
