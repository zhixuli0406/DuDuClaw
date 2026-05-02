"""Capability-match routing engine for HandoffPacketV2 (W19-P0).

Public API
----------
Types::

    HandoffPacketV1          — legacy v1 packet
    HandoffPacketV2          — structured v2 packet with routing strategy
    CapabilityMatchStrategy  — parameters for capability-based routing
    FallbackStrategy         — fallback when no capable agent is found
    RoutingStrategy          — top-level strategy container
    LazyRef                  — deferred reference to an external resource
    ToolCall                 — immutable MCP tool invocation record
    normalize_packet         — v0.1 → v0.2 migration helper

Router::

    CapabilityMatchRouter    — core routing decision engine
    ScoredCandidate          — intermediate scored result
    NoCapableAgentError      — no agent satisfied requirements
    RoutingLoopError         — hop_count >= max_hops

Resolution framework::

    Resolver                 — abstract base; implement for each ref_type
    ResolutionPolicy         — resolves LazyRef sets via registered Resolvers
    ResolutionResult         — outcome of a single resolution attempt
    ResolutionError          — raised by Resolver on fetch failure

Memory Lazy Reference Resolver (OQ-01 HandoffPacket v0.2)::

    MemoryClient             — abstract memory backend interface
    MemoryRecordResolver     — resolves memory_record refs by ID
    MemorySearchResolver     — resolves memory_search refs via semantic search
    MemoryLazyRefResolver    — composite resolver for both memory ref types
    TtlCache                 — shared TTL cache used by memory resolvers

Usage
-----
::

    from duduclaw.agents.routing import (
        CapabilityMatchRouter,
        CapabilityMatchStrategy,
        HandoffPacketV2,
        RoutingStrategy,
        normalize_packet,
    )

    router = CapabilityMatchRouter()
    packet = normalize_packet(raw_packet)

    if packet.routing_strategy.type == "capability_match":
        strategy = packet.routing_strategy.capability_match
        agent_id = await router.route(strategy, packet)

    # Memory lazy ref resolution (OQ-01)::

    from duduclaw.agents.routing import MemoryLazyRefResolver

    resolver = MemoryLazyRefResolver(my_memory_client)
    results = await resolver.resolve_all(list(packet.memory_refs))
"""

from .memory_resolver import (
    MemoryClient,
    MemoryLazyRefResolver,
    MemoryRecordResolver,
    MemorySearchResolver,
    TtlCache,
)
from .resolution import ResolutionError, ResolutionPolicy, ResolutionResult, Resolver
from .router import (
    CapabilityMatchRouter,
    NoCapableAgentError,
    RoutingLoopError,
    ScoredCandidate,
)
from .types import (
    CapabilityMatchStrategy,
    FallbackStrategy,
    HandoffPacketV1,
    HandoffPacketV2,
    LazyRef,
    RoutingStrategy,
    ToolCall,
    normalize_packet,
)

__all__ = [
    # types
    "CapabilityMatchStrategy",
    "FallbackStrategy",
    "HandoffPacketV1",
    "HandoffPacketV2",
    "LazyRef",
    "RoutingStrategy",
    "ToolCall",
    "normalize_packet",
    # router
    "CapabilityMatchRouter",
    "NoCapableAgentError",
    "RoutingLoopError",
    "ScoredCandidate",
    # resolution
    "Resolver",
    "ResolutionError",
    "ResolutionPolicy",
    "ResolutionResult",
    # memory lazy ref resolver (OQ-01)
    "MemoryClient",
    "MemoryLazyRefResolver",
    "MemoryRecordResolver",
    "MemorySearchResolver",
    "TtlCache",
]
