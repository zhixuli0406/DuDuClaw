import importlib
import pkgutil
from typing import Dict, Type

from .base import ChannelPlugin


def discover_channels() -> Dict[str, Type[ChannelPlugin]]:
    """Auto-scan channels/ directory to discover all plugins"""
    channels: Dict[str, Type[ChannelPlugin]] = {}
    package = importlib.import_module("duduclaw.channels")
    for _, module_name, _ in pkgutil.iter_modules(package.__path__):
        if module_name in ("base", "registry"):
            continue
        module = importlib.import_module(f"duduclaw.channels.{module_name}")
        for attr in dir(module):
            cls = getattr(module, attr)
            if (
                isinstance(cls, type)
                and issubclass(cls, ChannelPlugin)
                and cls is not ChannelPlugin
            ):
                channels[cls.name] = cls
    return channels
