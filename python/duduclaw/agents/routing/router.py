"""CapabilityMatchRouter — routes HandoffPacketV2 to the best capable agent.

Architecture
------------
The router consumes the existing :mod:`~duduclaw.agents.capabilities`
infrastructure (``CapabilityRegistryLoader`` + ``CapabilityMatcher``) and
wraps it with:

* **Three match modes**: ``all`` / ``any`` / ``scored``
* **Four fallback strategies**: ``error`` / ``random`` / ``round_robin`` /
  ``least_loaded``
* **Circular-routing protection**: ``hop_count >= max_hops`` raises
  :exc:`RoutingLoopError`
* **Load-factor scoring** (``scored`` mode): weighted combination of
  capability score and agent availability
* **Activity audit trail**: optional callback for observability

Integration with ``route_query``
---------------------------------
::

    from duduclaw.agents.routing.router import CapabilityMatchRouter
    from duduclaw.agents.routing.types import (
        CapabilityMatchStrategy, HandoffPacketV2, RoutingStrategy, normalize_packet,
    )

    router = CapabilityMatchRouter()

    async def route_query(raw_packet):
        packet = normalize_packet(raw_packet)
        if packet.routing_strategy.type == "capability_match":
            strategy = packet.routing_strategy.capability_match
            return await router.route(strategy, packet)
        # … other routing types …
"""

from __future__ import annotations

import logging
import random
import time
from dataclasses import dataclass
from typing import Any, Callable, Optional

from ..capabilities import AgentCapability, CapabilityMatcher, CapabilityRegistryLoader
from .types import CapabilityMatchStrategy, FallbackStrategy, HandoffPacketV2

logger = logging.getLogger(__name__)

# Hard cap on delegation depth (defence-in-depth; system also enforces this).
_SYSTEM_MAX_HOPS: int = 5

# Module-level default loader — created lazily, replaced in tests.
_default_loader: Optional[CapabilityRegistryLoader] = None


def _get_default_loader() -> CapabilityRegistryLoader:
    global _default_loader
    if _default_loader is None:
        _default_loader = CapabilityRegistryLoader()
    return _default_loader


# ── Custom exceptions ─────────────────────────────────────────────────────────


class NoCapableAgentError(Exception):
    """Raised when no agent satisfies the capability requirements.

    Attributes:
        required_capabilities: Capabilities that could not be satisfied.
        match_mode:            The match mode that was attempted.
    """

    def __init__(
        self,
        required_capabilities: tuple[str, ...],
        match_mode: str,
    ) -> None:
        caps = list(required_capabilities)
        super().__init__(
            f"No capable agent found for capabilities={caps}, mode={match_mode!r}"
        )
        self.required_capabilities = required_capabilities
        self.match_mode = match_mode


class RoutingLoopError(Exception):
    """Raised when ``packet.hop_count >= packet.max_hops``.

    Prevents unbounded delegation chains.

    Attributes:
        hop_count: Current hop depth at the time of detection.
        max_hops:  Configured maximum depth.
    """

    def __init__(self, hop_count: int, max_hops: int) -> None:
        super().__init__(
            f"Routing loop detected: hop_count={hop_count} >= max_hops={max_hops}"
        )
        self.hop_count = hop_count
        self.max_hops = max_hops


# ── Scored candidate ──────────────────────────────────────────────────────────


@dataclass(frozen=True)
class ScoredCandidate:
    """Intermediate result from ``scored`` mode ranking.

    Attributes:
        agent_id:          Agent identifier.
        capability_score:  Score from capability matching (0–1.1).
        load_score:        Availability score (0–1.0).
        final_score:       Weighted combination of both scores (0–1.0).
    """

    agent_id: str
    capability_score: float
    load_score: float
    final_score: float


# ── Router ────────────────────────────────────────────────────────────────────


class CapabilityMatchRouter:
    """Routes :class:`~duduclaw.agents.routing.types.HandoffPacketV2` to the
    best matching agent using capability matching.

    Match modes
    -----------
    * ``"all"``    — agent must have **all** required capabilities.
    * ``"any"``    — agent must have **at least one** required capability.
    * ``"scored"`` — agents ranked by a weighted score:

      .. code-block:: text

          final_score = capability_score × (1 − w) + availability_score × w

          capability_score   = matched_caps / required_caps  (0–1.0)
          availability_score = (max_concurrent − current_tasks) / max_concurrent
          w                  = load_factor_weight  (0–1.0)

    Fallback strategies (when no primary match is found)
    -----------------------------------------------------
    * ``"error"`` / ``"none"`` — raise :exc:`NoCapableAgentError`
    * ``"random"``             — :func:`random.choice` from remaining agents
    * ``"round_robin"``        — deterministic cycling (sorted agent IDs)
    * ``"least_loaded"``       — agent with highest available capacity

    Usage
    -----
    ::

        router = CapabilityMatchRouter()
        agent_id = await router.route(strategy, packet)
    """

    def __init__(
        self,
        loader: Optional[CapabilityRegistryLoader] = None,
        load_provider: Optional[Callable[[str], int]] = None,
        activity_logger: Optional[Callable[[dict[str, Any]], None]] = None,
    ) -> None:
        """
        Args:
            loader:          Registry loader to use.  Defaults to the
                             module-level singleton backed by ``registry.yaml``.
            load_provider:   ``(agent_id) → current_task_count``.  When
                             ``None``, all agents are treated as idle (0 tasks).
            activity_logger: Optional callback invoked with a routing-decision
                             dict for each :meth:`route` call (audit trail).
        """
        self._loader = loader or _get_default_loader()
        self._load_provider: Callable[[str], int] = load_provider or (lambda _: 0)
        self._activity_logger = activity_logger
        self._round_robin_index: int = 0

    # ── Public API ────────────────────────────────────────────────────────────

    async def route(
        self,
        strategy: CapabilityMatchStrategy,
        packet: HandoffPacketV2,
    ) -> str:
        """Select the best agent for *packet* using *strategy*.

        Steps:

        1. Guard against routing loops (``hop_count >= max_hops``).
        2. Load the latest capability registry.
        3. Build the exclusion set (``sender_agent_id`` + explicit excludes).
        4. Find candidates according to ``match_mode``.
        5. If ``scored`` mode, rerank by weighted capability+load score.
        6. Select the top candidate, or run the fallback strategy.
        7. Log the decision to the activity logger and Python logger.

        Args:
            strategy: Routing parameters.
            packet:   The packet being routed.

        Returns:
            ``agent_id`` of the selected agent.

        Raises:
            RoutingLoopError:    ``hop_count >= max_hops`` (also enforced at
                                 system level, max 5).
            NoCapableAgentError: No agent found and fallback is ``"error"`` /
                                 ``"none"``.
        """
        # 1. Circular routing guard
        effective_max = min(packet.max_hops, _SYSTEM_MAX_HOPS)
        if packet.hop_count >= effective_max:
            raise RoutingLoopError(packet.hop_count, effective_max)

        # 2. Registry snapshot
        registry = self._loader.get()
        matcher = CapabilityMatcher(registry)

        # 3. Exclusion set: always exclude the sender to prevent self-loops
        excluded = set(strategy.exclude_agent_ids) | {packet.sender_agent_id}
        excluded_list = sorted(excluded)

        # 4. Find primary candidates
        candidates = self._find_candidates(matcher, strategy, excluded_list)

        # 5. Score and rank (scored mode recomputes with load factor)
        if strategy.match_mode == "scored" and candidates:
            scored = self._score_candidates(candidates, strategy, registry)
            sorted_ids = [
                s.agent_id
                for s in sorted(scored, key=lambda x: (-x.final_score, x.agent_id))
            ]
        else:
            sorted_ids = [r.agent_id for r in candidates]

        # 6. Select or fallback
        if sorted_ids:
            selected = sorted_ids[0]
        else:
            # Fallback pool: all agents except excluded ones
            fallback_pool = sorted(
                aid for aid in registry.agents if aid not in excluded
            )
            selected = await self.execute_fallback(strategy.fallback, fallback_pool)

        # 7. Audit trail
        self._log_decision(strategy, packet, sorted_ids, selected)

        return selected

    def score_agent(
        self,
        agent: AgentCapability,
        strategy: CapabilityMatchStrategy,
    ) -> float:
        """Compute the weighted routing score for a single agent.

        Formula::

            final_score = capability_score × (1 − w) + availability_score × w

        where::

            capability_score   = |matched ∩ required| / |required|
            availability_score = max(0, max_concurrent − current_tasks)
                                 / max_concurrent
            w                  = strategy.load_factor_weight  (clamped to [0,1])

        Args:
            agent:    Agent capability record.
            strategy: Routing strategy carrying scoring parameters.

        Returns:
            Final score in ``[0.0, 1.0]`` (rounded to 4 decimal places).
        """
        required = frozenset(strategy.required_capabilities)
        agent_caps = frozenset(agent.capabilities)

        # Capability score
        if not required:
            capability_score = 1.0
        else:
            matched_count = len(required & agent_caps)
            capability_score = matched_count / len(required)

        # Availability score (current load vs maximum capacity)
        current_tasks = self._load_provider(agent.agent_id)
        if agent.max_concurrent_tasks > 0:
            free = max(0.0, agent.max_concurrent_tasks - current_tasks)
            availability_score = free / agent.max_concurrent_tasks
        else:
            availability_score = 0.0

        w = max(0.0, min(1.0, strategy.load_factor_weight))
        return round(
            capability_score * (1.0 - w) + availability_score * w,
            4,
        )

    async def execute_fallback(
        self,
        strategy: FallbackStrategy,
        candidates: list[str],
    ) -> str:
        """Run the configured fallback strategy.

        Args:
            strategy:   Fallback configuration.
            candidates: Eligible agent IDs (already sorted, excludes excluded).

        Returns:
            ``agent_id`` of the chosen fallback agent.

        Raises:
            NoCapableAgentError: When strategy is ``"none"`` / ``"error"``,
                                 or when *candidates* is empty for any strategy.
        """
        if strategy.type in ("none", "error"):
            raise NoCapableAgentError((), strategy.type)

        if not candidates:
            raise NoCapableAgentError((), f"fallback:{strategy.type}:no_candidates")

        if strategy.type == "random":
            return random.choice(candidates)

        if strategy.type == "round_robin":
            # Deterministic: always operate on sorted candidates
            sorted_candidates = sorted(candidates)
            idx = self._round_robin_index % len(sorted_candidates)
            self._round_robin_index += 1
            return sorted_candidates[idx]

        if strategy.type == "least_loaded":
            registry = self._loader.get()

            def _load_ratio(aid: str) -> float:
                cap = registry.agents.get(aid)
                if cap is None or cap.max_concurrent_tasks <= 0:
                    return 1.0  # unknown agents are treated as fully loaded
                current = self._load_provider(aid)
                return current / cap.max_concurrent_tasks

            return min(candidates, key=_load_ratio)

        raise NoCapableAgentError((), f"unknown_fallback:{strategy.type}")

    # ── Private helpers ───────────────────────────────────────────────────────

    def _find_candidates(
        self,
        matcher: CapabilityMatcher,
        strategy: CapabilityMatchStrategy,
        excluded: list[str],
    ):  # → list[MatchResult]
        """Dispatch to the correct ``CapabilityMatcher`` method by match mode."""
        caps = list(strategy.required_capabilities)

        if strategy.match_mode == "all":
            return matcher.find_by_capability(caps, exclude_agent_ids=excluded)

        if strategy.match_mode == "any":
            return matcher.find_by_any_capability(caps, exclude_agent_ids=excluded)

        # "scored": prefer all-match but fall back to any-match for broader pool
        results = matcher.find_by_capability(caps, exclude_agent_ids=excluded)
        if not results:
            results = matcher.find_by_any_capability(caps, exclude_agent_ids=excluded)
        return results

    def _score_candidates(
        self,
        candidates,  # list[MatchResult]
        strategy: CapabilityMatchStrategy,
        registry,  # CapabilityRegistry
    ) -> list[ScoredCandidate]:
        """Recompute scores with the load factor applied (``scored`` mode only)."""
        scored: list[ScoredCandidate] = []

        for result in candidates:
            agent = registry.agents.get(result.agent_id)
            if agent is None:
                continue  # stale result, skip

            final_score = self.score_agent(agent, strategy)

            # Separate load score for observability
            current_tasks = self._load_provider(agent.agent_id)
            if agent.max_concurrent_tasks > 0:
                free = max(0.0, agent.max_concurrent_tasks - current_tasks)
                load_score = round(free / agent.max_concurrent_tasks, 4)
            else:
                load_score = 0.0

            scored.append(
                ScoredCandidate(
                    agent_id=agent.agent_id,
                    capability_score=result.score,
                    load_score=load_score,
                    final_score=final_score,
                )
            )

        return scored

    def _log_decision(
        self,
        strategy: CapabilityMatchStrategy,
        packet: HandoffPacketV2,
        candidates: list[str],
        selected: str,
    ) -> None:
        """Emit a structured routing-decision log entry."""
        log_entry: dict[str, Any] = {
            "event": "capability_match_route",
            "timestamp": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            "sender_agent_id": packet.sender_agent_id,
            "selected_agent_id": selected,
            "required_capabilities": list(strategy.required_capabilities),
            "match_mode": strategy.match_mode,
            "load_factor_weight": strategy.load_factor_weight,
            "hop": packet.hop_count,
            "max_hops": packet.max_hops,
            "candidate_count": len(candidates),
            "top_candidates": candidates[:5],
        }

        logger.info(
            "capability_match route: sender=%s → selected=%s "
            "(mode=%s, hops=%d/%d, candidates=%d)",
            packet.sender_agent_id,
            selected,
            strategy.match_mode,
            packet.hop_count,
            packet.max_hops,
            len(candidates),
        )

        if self._activity_logger is not None:
            self._activity_logger(log_entry)
