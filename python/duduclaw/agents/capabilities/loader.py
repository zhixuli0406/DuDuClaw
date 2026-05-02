"""AgentCapabilityRegistry loader — YAML parsing and hot reload.

Loading priority:
  1. DUDUCLAW_CAPABILITY_REGISTRY env var (absolute path)
  2. registry_path constructor argument
  3. Default: ~/.duduclaw/agents/capabilities/registry.yaml
  4. Fallback: bundled registry.yaml shipped with this package

Hot reload:
  - Uses watchdog library when available (inotify/kqueue/FSEvents)
  - Falls back to background polling thread (default 30 s interval)
  - Call start_hot_reload() after initial load() to activate

Thread safety:
  - All public methods are thread-safe via threading.RLock
  - Stale registry is retained on reload failure

Immutability:
  - AgentCapability and CapabilityRegistry are frozen dataclasses
"""

from __future__ import annotations

import logging
import os
import threading
from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional

logger = logging.getLogger(__name__)

# Schema version this module expects
SCHEMA_VERSION = "1.0"

# Path to bundled registry shipped alongside this module
_BUNDLED_REGISTRY = Path(__file__).parent / "registry.yaml"

# Default runtime registry location
_DEFAULT_REGISTRY = Path.home() / ".duduclaw" / "agents" / "capabilities" / "registry.yaml"


# ── Data model (immutable) ────────────────────────────────────────────────────


@dataclass(frozen=True)
class AgentCapability:
    """Immutable capability record for a single agent.

    Attributes:
        agent_id:              Unique agent identifier (matches agent.toml name).
        capabilities:          Tuple of capability identifiers (snake_case).
        max_concurrent_tasks:  Maximum tasks this agent can handle in parallel.
        description:           Human-readable description (optional).
        tags:                  Free-form grouping labels (optional).
    """

    agent_id: str
    capabilities: tuple[str, ...]
    max_concurrent_tasks: int
    description: str = ""
    tags: tuple[str, ...] = field(default_factory=tuple)


@dataclass(frozen=True)
class CapabilityRegistry:
    """Immutable snapshot of the full capability registry.

    Attributes:
        schema_version: Version string parsed from the YAML file.
        agents:         Mapping of agent_id → AgentCapability.
    """

    schema_version: str
    agents: dict[str, AgentCapability]


# ── Loader ────────────────────────────────────────────────────────────────────


class CapabilityRegistryLoader:
    """Loads and caches the AgentCapabilityRegistry from a YAML file.

    Usage::

        loader = CapabilityRegistryLoader()
        registry = loader.load()      # first load
        loader.start_hot_reload()     # enable live updates

        registry = loader.get()       # subsequent fast reads
    """

    def __init__(self, registry_path: Optional[Path] = None) -> None:
        env_path = os.environ.get("DUDUCLAW_CAPABILITY_REGISTRY")
        if env_path:
            self._path = Path(env_path)
        elif registry_path is not None:
            self._path = registry_path
        else:
            self._path = _DEFAULT_REGISTRY

        self._lock: threading.RLock = threading.RLock()
        self._registry: Optional[CapabilityRegistry] = None
        self._watcher: Optional[object] = None  # watchdog Observer or None

    # ── Public API ────────────────────────────────────────────────────────────

    @property
    def registry_path(self) -> Path:
        """Resolved path of the registry file being monitored."""
        return self._path

    def load(self) -> CapabilityRegistry:
        """(Re-)load the registry from disk.  Thread-safe.

        Falls back to the bundled ``registry.yaml`` when the configured path
        does not exist (useful in test / CI environments).

        Returns:
            Newly loaded CapabilityRegistry (also stored internally).

        Raises:
            ValueError:   YAML structure is invalid and no stale registry exists.
            RuntimeError: PyYAML is not installed.
        """
        try:
            import yaml  # type: ignore[import-untyped]
        except ImportError as exc:
            raise RuntimeError(
                "PyYAML is required for CapabilityRegistryLoader. "
                "Install it with: pip install pyyaml"
            ) from exc

        with self._lock:
            resolved = self._resolve_path()
            try:
                raw = yaml.safe_load(resolved.read_text(encoding="utf-8"))
                registry = _parse_registry(raw)
                self._registry = registry
                logger.info(
                    "Loaded capability registry: %d agents from %s",
                    len(registry.agents),
                    resolved,
                )
                return registry
            except Exception as exc:  # noqa: BLE001
                if self._registry is not None:
                    logger.warning(
                        "Failed to reload registry (%s); keeping stale copy", exc
                    )
                    return self._registry
                raise

    def get(self) -> CapabilityRegistry:
        """Return the current registry, loading it if necessary.  Thread-safe."""
        with self._lock:
            if self._registry is None:
                return self.load()
            return self._registry

    def start_hot_reload(self, poll_interval_seconds: float = 30.0) -> None:
        """Start a filesystem watcher so the registry reloads automatically.

        Prefers *watchdog* (inotify/FSEvents/kqueue) when installed; falls back
        to a background polling thread otherwise.

        Args:
            poll_interval_seconds: Polling interval used when watchdog is absent.
        """
        try:
            self._start_watchdog_reload()
        except ImportError:
            logger.info(
                "watchdog not available; starting polling hot reload "
                "(interval=%ss)", poll_interval_seconds
            )
            self._start_polling_reload(poll_interval_seconds)

    def stop_hot_reload(self) -> None:
        """Stop the hot reload watcher (no-op if not started)."""
        if self._watcher is not None:
            try:
                self._watcher.stop()  # type: ignore[attr-defined]
            except Exception:  # noqa: BLE001
                pass
            self._watcher = None
            logger.info("Hot reload watcher stopped")

    # ── Private helpers ───────────────────────────────────────────────────────

    def _resolve_path(self) -> Path:
        """Return the configured path, or the bundled fallback."""
        if self._path.exists():
            return self._path
        if _BUNDLED_REGISTRY.exists():
            logger.debug(
                "Registry not found at %s; using bundled fallback", self._path
            )
            return _BUNDLED_REGISTRY
        raise FileNotFoundError(
            f"No registry file found at {self._path} "
            f"and bundled fallback is missing ({_BUNDLED_REGISTRY})"
        )

    def _reload_safe(self) -> None:
        """Trigger a reload, logging errors without propagating them."""
        try:
            self.load()
        except Exception as exc:  # noqa: BLE001
            logger.error("Hot reload failed: %s", exc)

    def _start_watchdog_reload(self) -> None:
        """Set up watchdog observer (raises ImportError when not installed)."""
        from watchdog.events import FileSystemEventHandler  # type: ignore[import-untyped]
        from watchdog.observers import Observer  # type: ignore[import-untyped]

        loader = self

        class _Handler(FileSystemEventHandler):
            def on_modified(self, event: object) -> None:  # type: ignore[override]
                src = getattr(event, "src_path", "")
                if Path(src) == loader._path:
                    logger.info("Registry file changed; hot-reloading...")
                    loader._reload_safe()

        observer = Observer()
        observer.schedule(_Handler(), str(self._path.parent), recursive=False)
        observer.daemon = True  # type: ignore[attr-defined]
        observer.start()
        self._watcher = observer
        logger.info("watchdog hot reload active for %s", self._path)

    def _start_polling_reload(self, interval_seconds: float) -> None:
        """Fallback: poll mtime in a daemon thread."""
        loader = self

        def _poll() -> None:
            import time

            last_mtime = loader._path.stat().st_mtime if loader._path.exists() else 0.0
            while True:
                time.sleep(interval_seconds)
                try:
                    current_mtime = (
                        loader._path.stat().st_mtime if loader._path.exists() else 0.0
                    )
                    if current_mtime != last_mtime:
                        last_mtime = current_mtime
                        logger.info(
                            "Registry file changed (polling); hot-reloading..."
                        )
                        loader._reload_safe()
                except Exception as exc:  # noqa: BLE001
                    logger.error("Polling hot reload error: %s", exc)

        thread = threading.Thread(
            target=_poll, daemon=True, name="capability-registry-poller"
        )
        thread.start()
        self._watcher = thread


# ── Parsing ───────────────────────────────────────────────────────────────────


def _parse_registry(raw: object) -> CapabilityRegistry:
    """Convert raw YAML data into an immutable CapabilityRegistry.

    Args:
        raw: Top-level object from ``yaml.safe_load``.

    Returns:
        Validated, immutable CapabilityRegistry.

    Raises:
        ValueError: Missing required fields or wrong types.
    """
    if not isinstance(raw, dict):
        raise ValueError(
            f"Registry YAML root must be a mapping, got {type(raw).__name__}"
        )

    schema_version = str(raw.get("schema_version", SCHEMA_VERSION))

    agents_raw = raw.get("agents", [])
    if not isinstance(agents_raw, list):
        raise ValueError("'agents' key must be a list")

    agents: dict[str, AgentCapability] = {}
    for idx, entry in enumerate(agents_raw):
        if not isinstance(entry, dict):
            raise ValueError(
                f"agents[{idx}] must be a mapping, got {type(entry).__name__}"
            )
        capability = _parse_agent_entry(entry, idx)
        agents[capability.agent_id] = capability

    return CapabilityRegistry(schema_version=schema_version, agents=agents)


def _parse_agent_entry(entry: dict, idx: int) -> AgentCapability:
    """Parse and validate a single agent entry from the YAML list.

    Args:
        entry: Raw dict for one agent.
        idx:   Zero-based position in the agents list (used in error messages).

    Returns:
        Immutable AgentCapability.

    Raises:
        ValueError: Missing or invalid required fields.
    """
    agent_id = entry.get("agent_id")
    if not agent_id or not isinstance(agent_id, str) or not agent_id.strip():
        raise ValueError(f"agents[{idx}]: 'agent_id' is required and must be a non-empty string")
    agent_id = agent_id.strip()

    capabilities_raw = entry.get("capabilities")
    if capabilities_raw is None:
        raise ValueError(f"agents[{idx}] ({agent_id}): 'capabilities' is required")
    if not isinstance(capabilities_raw, list):
        raise ValueError(
            f"agents[{idx}] ({agent_id}): 'capabilities' must be a list"
        )

    max_concurrent = entry.get("max_concurrent_tasks")
    if max_concurrent is None:
        raise ValueError(
            f"agents[{idx}] ({agent_id}): 'max_concurrent_tasks' is required"
        )
    if not isinstance(max_concurrent, int) or isinstance(max_concurrent, bool):
        raise ValueError(
            f"agents[{idx}] ({agent_id}): 'max_concurrent_tasks' must be an integer"
        )
    if max_concurrent < 1:
        raise ValueError(
            f"agents[{idx}] ({agent_id}): 'max_concurrent_tasks' must be >= 1"
        )

    tags_raw = entry.get("tags", [])
    if not isinstance(tags_raw, list):
        raise ValueError(f"agents[{idx}] ({agent_id}): 'tags' must be a list")

    return AgentCapability(
        agent_id=agent_id,
        capabilities=tuple(str(c) for c in capabilities_raw),
        max_concurrent_tasks=max_concurrent,
        description=str(entry.get("description", "")),
        tags=tuple(str(t) for t in tags_raw),
    )
