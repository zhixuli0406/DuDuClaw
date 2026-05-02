"""Type definitions for HandoffPacketV2 and capability-match routing.

This module is the single source of truth for:
  - HandoffPacketV1  — legacy v1 packet (backward compat)
  - HandoffPacketV2  — structured v2 packet with routing strategy & LazyRef
  - CapabilityMatchStrategy — parameters for capability-based routing
  - FallbackStrategy — what to do when no capable agent is found
  - RoutingStrategy  — top-level strategy container
  - LazyRef          — deferred reference to an external resource
  - ToolCall         — immutable record of a single MCP tool invocation
  - normalize_packet — v0.1 → v0.2 migration helper
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, Literal, Optional


# ── Tool history ──────────────────────────────────────────────────────────────


@dataclass(frozen=True)
class ToolCall:
    """Immutable record of a single MCP tool invocation.

    Attributes:
        tool_name: Name of the MCP tool called.
        input:     Input arguments as passed to the tool.
        output:    Serialised output string returned by the tool.
        called_at: ISO 8601 timestamp of the invocation.
    """

    tool_name: str
    input: dict[str, Any]
    output: str
    called_at: str  # ISO 8601


# ── LazyRef ───────────────────────────────────────────────────────────────────


@dataclass(frozen=True)
class LazyRef:
    """Deferred reference to an external resource resolved at use-time.

    A LazyRef carries just enough metadata to fetch a resource later,
    without embedding its content in the packet itself.  The receiving
    agent (or the ResolutionPolicy engine) decides *when* to resolve it.

    Attributes:
        ref_type:          Resource category (e.g. ``"memory"``, ``"wiki"``).
        ref_id:            Opaque resource identifier (key, UUID, path…).
        resolution_policy: How and when to resolve (``"eager"`` | ``"lazy"``
                           | ``"on_demand"``).
        metadata:          Optional extra context for the resolver.
    """

    ref_type: str
    ref_id: str
    resolution_policy: str = "lazy"
    metadata: dict[str, Any] = field(default_factory=dict)


# ── Fallback strategy ─────────────────────────────────────────────────────────


@dataclass(frozen=True)
class FallbackStrategy:
    """Describes what to do when capability matching yields no candidates.

    Attributes:
        type: One of:
            - ``"error"`` / ``"none"`` — raise :exc:`NoCapableAgentError`
            - ``"random"``             — choose randomly from remaining agents
            - ``"round_robin"``        — deterministic cycling through agents
            - ``"least_loaded"``       — pick the agent with most free capacity
    """

    type: Literal["none", "random", "round_robin", "least_loaded", "error"] = "error"


# ── Capability-match strategy ─────────────────────────────────────────────────


@dataclass(frozen=True)
class CapabilityMatchStrategy:
    """Parameters for capability-based agent routing.

    Attributes:
        required_capabilities: Capability identifiers to match against.
        match_mode:            One of ``"all"`` | ``"any"`` | ``"scored"``:
                               - ``"all"``    — agent must have ALL caps
                               - ``"any"``    — agent must have at least one
                               - ``"scored"`` — agents ranked by weighted score
        load_factor_weight:    In ``"scored"`` mode, weight (0–1) given to the
                               agent's current availability vs capability score.
                               0.0 = pure capability, 1.0 = pure availability.
        exclude_agent_ids:     Agent IDs to skip (e.g. the calling agent).
        fallback:              What to do when no candidate passes the filter.
    """

    required_capabilities: tuple[str, ...]
    match_mode: Literal["all", "any", "scored"] = "all"
    load_factor_weight: float = 0.0
    exclude_agent_ids: tuple[str, ...] = field(default_factory=tuple)
    fallback: FallbackStrategy = field(
        default_factory=lambda: FallbackStrategy(type="error")
    )


# ── Routing strategy ──────────────────────────────────────────────────────────


@dataclass(frozen=True)
class RoutingStrategy:
    """Top-level routing strategy container embedded in HandoffPacketV2.

    Attributes:
        type:              Strategy type: ``"capability_match"`` | ``"direct"``
                           | ``"broadcast"``.
        capability_match:  Populated when ``type == "capability_match"``.
        direct_target:     Target agent ID when ``type == "direct"``.
    """

    type: Literal["capability_match", "direct", "broadcast"]
    capability_match: Optional[CapabilityMatchStrategy] = None
    direct_target: Optional[str] = None  # only for type=="direct"


# ── HandoffPacketV1 (legacy) ──────────────────────────────────────────────────


@dataclass(frozen=True)
class HandoffPacketV1:
    """Legacy v1 handoff packet.

    All fields are strings / plain collections — no structured routing.
    Kept for backward compatibility; use :func:`normalize_packet` to
    upgrade to v2.

    Attributes:
        task_summary:     Human-readable summary of the task.
        sender_agent_id:  ID of the agent that created this packet.
        timestamp:        ISO 8601 creation timestamp.
        memory_refs:      Plain memory-key strings (no LazyRef wrapping).
        tool_history:     Tool calls made so far in this task chain.
        partial_results:  Any partial work product the receiver can reuse.
    """

    task_summary: str
    sender_agent_id: str
    timestamp: str
    memory_refs: tuple[str, ...] = field(default_factory=tuple)
    tool_history: tuple[ToolCall, ...] = field(default_factory=tuple)
    partial_results: Optional[dict[str, Any]] = None


# ── HandoffPacketV2 ───────────────────────────────────────────────────────────


@dataclass(frozen=True)
class HandoffPacketV2:
    """Structured task handoff packet (v2) for inter-agent communication.

    Carries full context for the receiving agent:

    * **task_summary / sender_agent_id / timestamp** — provenance
    * **routing_strategy** — structured instructions for the router
    * **memory_refs** — lazy references to memory / wiki / artifacts
    * **tool_history** — prior tool calls, prevents redundant re-execution
    * **partial_results** — work done so far
    * **hop_count / max_hops** — circular routing protection (max 5 hops)

    Attributes:
        task_summary:      Human-readable task summary (required).
        sender_agent_id:   ID of the originating agent (required).
        timestamp:         ISO 8601 creation timestamp (required).
        routing_strategy:  How this packet should be routed (required).
        memory_refs:       Lazy references to external resources.
        tool_history:      MCP tool calls made in prior hops.
        partial_results:   Any partial work product.
        max_hops:          Maximum allowed hop depth (default 5).
        hop_count:         Current hop depth (incremented at each handoff).
        version:           Packet schema version identifier (always ``"v2"``).
    """

    task_summary: str
    sender_agent_id: str
    timestamp: str
    routing_strategy: RoutingStrategy
    memory_refs: tuple[LazyRef | str, ...] = field(default_factory=tuple)
    tool_history: tuple[ToolCall, ...] = field(default_factory=tuple)
    partial_results: Optional[dict[str, Any]] = None
    max_hops: int = 5
    hop_count: int = 0
    version: Literal["v2"] = "v2"


# ── normalize_packet ──────────────────────────────────────────────────────────


def normalize_packet(packet: HandoffPacketV1 | HandoffPacketV2) -> HandoffPacketV2:
    """Upgrade a v1 packet to v2, or return a v2 packet unchanged.

    v1 → v2 migration rules:

    * Each plain ``str`` in ``memory_refs`` is wrapped in a
      :class:`LazyRef` (``ref_type="memory"``, ``resolution_policy="lazy"``).
    * ``routing_strategy`` defaults to ``"broadcast"`` (v1 had no routing).
    * All other fields are copied verbatim.

    Args:
        packet: Either a :class:`HandoffPacketV1` or :class:`HandoffPacketV2`.

    Returns:
        A :class:`HandoffPacketV2` equivalent.
    """
    if isinstance(packet, HandoffPacketV2):
        return packet

    # v1 → v2: wrap plain string memory refs into LazyRef
    lazy_refs: tuple[LazyRef | str, ...] = tuple(
        LazyRef(ref_type="memory", ref_id=ref, resolution_policy="lazy")
        if isinstance(ref, str)
        else ref
        for ref in packet.memory_refs
    )

    return HandoffPacketV2(
        task_summary=packet.task_summary,
        sender_agent_id=packet.sender_agent_id,
        timestamp=packet.timestamp,
        routing_strategy=RoutingStrategy(type="broadcast"),
        memory_refs=lazy_refs,
        tool_history=packet.tool_history,
        partial_results=packet.partial_results,
        max_hops=5,
        hop_count=0,
        version="v2",
    )
