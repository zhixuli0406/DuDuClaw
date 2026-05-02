"""Capability matcher — query interface for HandoffPacket routing.

This module exposes CapabilityMatcher, consumed by the HandoffPacket router
to locate the most suitable target agent for a given task.

All methods return immutable MatchResult objects sorted by descending score.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Optional

from .loader import AgentCapability, CapabilityRegistry


# ── Result model (immutable) ──────────────────────────────────────────────────


@dataclass(frozen=True)
class MatchResult:
    """Immutable result from a capability match query.

    Attributes:
        agent_id:              The matched agent's identifier.
        capability:            Full AgentCapability record for this agent.
        matched_capabilities:  Capabilities from the query that this agent has.
        score:                 Relevance score in [0.0, 1.1].  Higher is better.
    """

    agent_id: str
    capability: AgentCapability
    matched_capabilities: tuple[str, ...]
    score: float


# ── Matcher ───────────────────────────────────────────────────────────────────


class CapabilityMatcher:
    """Query interface for capability-based agent lookup.

    Typical usage::

        loader = CapabilityRegistryLoader()
        registry = loader.get()
        matcher = CapabilityMatcher(registry)

        # Find agents that can handle code review AND security testing
        results = matcher.find_by_capability(
            ["code_review", "security_testing"]
        )
        if results:
            best_agent = results[0].agent_id
    """

    def __init__(self, registry: CapabilityRegistry) -> None:
        self._registry = registry

    # ── Primary query methods ─────────────────────────────────────────────────

    def find_by_capability(
        self,
        required_capabilities: list[str],
        *,
        exclude_agent_ids: Optional[list[str]] = None,
    ) -> list[MatchResult]:
        """Find agents that possess **all** of the required capabilities.

        Agents are ranked by *score*:
          - Base score 1.0 when all required capabilities are matched.
          - Small bonus (+≤0.1) for agents with broader skill sets.

        Args:
            required_capabilities: Every capability in this list must be present.
            exclude_agent_ids:     Agent IDs to skip (e.g. the calling agent).

        Returns:
            Sorted list of MatchResult (highest score first, then alphabetical).
            Empty list if no agent satisfies all requirements.
        """
        if not required_capabilities:
            return []

        required = frozenset(required_capabilities)
        excluded = frozenset(exclude_agent_ids or [])
        results: list[MatchResult] = []

        for agent_id, capability in self._registry.agents.items():
            if agent_id in excluded:
                continue

            agent_caps = frozenset(capability.capabilities)
            if not required.issubset(agent_caps):
                continue  # must match ALL required

            # Base score 1.0; small bonus proportional to extra capabilities
            extra = len(agent_caps - required)
            bonus = extra / max(len(agent_caps), 1) * 0.1
            score = round(1.0 + bonus, 4)

            results.append(
                MatchResult(
                    agent_id=agent_id,
                    capability=capability,
                    matched_capabilities=tuple(sorted(required)),
                    score=score,
                )
            )

        return sorted(results, key=lambda r: (-r.score, r.agent_id))

    def find_by_any_capability(
        self,
        any_capabilities: list[str],
        *,
        exclude_agent_ids: Optional[list[str]] = None,
    ) -> list[MatchResult]:
        """Find agents that have **at least one** of the given capabilities.

        Ranked by what fraction of the requested capabilities they cover.

        Args:
            any_capabilities:  At least one must be present.
            exclude_agent_ids: Agent IDs to skip.

        Returns:
            Sorted list of MatchResult (highest score first, then alphabetical).
            Empty list if no agent has any of the listed capabilities.
        """
        if not any_capabilities:
            return []

        requested = frozenset(any_capabilities)
        excluded = frozenset(exclude_agent_ids or [])
        results: list[MatchResult] = []

        for agent_id, capability in self._registry.agents.items():
            if agent_id in excluded:
                continue

            agent_caps = frozenset(capability.capabilities)
            matched = requested & agent_caps
            if not matched:
                continue

            score = round(len(matched) / len(requested), 4)
            results.append(
                MatchResult(
                    agent_id=agent_id,
                    capability=capability,
                    matched_capabilities=tuple(sorted(matched)),
                    score=score,
                )
            )

        return sorted(results, key=lambda r: (-r.score, r.agent_id))

    def find_by_tag(
        self,
        tags: list[str],
        *,
        exclude_agent_ids: Optional[list[str]] = None,
    ) -> list[MatchResult]:
        """Find agents that carry **at least one** of the given tags.

        Args:
            tags:              At least one must match.
            exclude_agent_ids: Agent IDs to skip.

        Returns:
            Sorted list of MatchResult, ``matched_capabilities`` is empty tuple.
        """
        if not tags:
            return []

        requested = frozenset(tags)
        excluded = frozenset(exclude_agent_ids or [])
        results: list[MatchResult] = []

        for agent_id, capability in self._registry.agents.items():
            if agent_id in excluded:
                continue

            agent_tags = frozenset(capability.tags)
            matched_tags = requested & agent_tags
            if not matched_tags:
                continue

            score = round(len(matched_tags) / len(requested), 4)
            results.append(
                MatchResult(
                    agent_id=agent_id,
                    capability=capability,
                    matched_capabilities=(),
                    score=score,
                )
            )

        return sorted(results, key=lambda r: (-r.score, r.agent_id))

    # ── Lookup helpers ────────────────────────────────────────────────────────

    def get_agent(self, agent_id: str) -> Optional[AgentCapability]:
        """Return the AgentCapability for *agent_id*, or ``None`` if not found."""
        return self._registry.agents.get(agent_id)

    def list_all_capabilities(self) -> list[str]:
        """Return a sorted list of every unique capability across all agents."""
        caps: set[str] = set()
        for capability in self._registry.agents.values():
            caps.update(capability.capabilities)
        return sorted(caps)

    def list_all_tags(self) -> list[str]:
        """Return a sorted list of every unique tag across all agents."""
        tags: set[str] = set()
        for capability in self._registry.agents.values():
            tags.update(capability.tags)
        return sorted(tags)

    def list_agents_by_capability(self, capability: str) -> list[str]:
        """Return sorted list of agent IDs that have the given capability."""
        return sorted(
            agent_id
            for agent_id, cap in self._registry.agents.items()
            if capability in cap.capabilities
        )
