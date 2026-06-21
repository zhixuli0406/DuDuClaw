"""DuDuClaw Python SDK — evolution vetter, channel bridges, and Claude Code SDK integration"""

from importlib.metadata import PackageNotFoundError, version as _pkg_version

try:
    # Source of truth: the installed distribution's metadata (pyproject.toml).
    __version__ = _pkg_version("duduclaw")
except PackageNotFoundError:
    # Fallback for running from a source checkout that isn't pip-installed.
    # Kept in sync with pyproject.toml by scripts/release.sh (pyinit manifest).
    __version__ = "1.22.1"
