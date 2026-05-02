"""AgentCapabilityRegistry — Static YAML version (W18).

Public API:
    CapabilityRegistryLoader  — loads/watches registry.yaml
    CapabilityMatcher         — queries capabilities for HandoffPacket routing
    AgentCapability           — immutable per-agent capability record
    CapabilityRegistry        — immutable snapshot of the full registry
    MatchResult               — result from a capability-match query
"""

from .loader import (
    AgentCapability,
    CapabilityRegistry,
    CapabilityRegistryLoader,
)
from .matcher import CapabilityMatcher, MatchResult

__all__ = [
    "AgentCapability",
    "CapabilityRegistry",
    "CapabilityRegistryLoader",
    "CapabilityMatcher",
    "MatchResult",
]
