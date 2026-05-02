"""Unit tests for duduclaw.agents.routing.router"""

from __future__ import annotations

from typing import Any
from unittest.mock import MagicMock, patch

import pytest

from duduclaw.agents.capabilities.loader import AgentCapability, CapabilityRegistry
from duduclaw.agents.capabilities.matcher import CapabilityMatcher
from duduclaw.agents.routing.router import (
    CapabilityMatchRouter,
    NoCapableAgentError,
    RoutingLoopError,
    ScoredCandidate,
)
from duduclaw.agents.routing.types import (
    CapabilityMatchStrategy,
    FallbackStrategy,
    HandoffPacketV2,
    RoutingStrategy,
)


# ── Fixtures & helpers ────────────────────────────────────────────────────────


def _make_agent(
    agent_id: str,
    capabilities: list[str],
    max_concurrent: int = 2,
    tags: list[str] | None = None,
) -> AgentCapability:
    return AgentCapability(
        agent_id=agent_id,
        capabilities=tuple(capabilities),
        max_concurrent_tasks=max_concurrent,
        tags=tuple(tags or []),
    )


def _make_registry(*agents: AgentCapability) -> CapabilityRegistry:
    return CapabilityRegistry(
        schema_version="1.0",
        agents={a.agent_id: a for a in agents},
    )


def _make_loader(registry: CapabilityRegistry):
    """Build a mock CapabilityRegistryLoader that returns *registry*."""
    loader = MagicMock()
    loader.get.return_value = registry
    return loader


def _make_packet(
    sender: str = "caller-agent",
    hop_count: int = 0,
    max_hops: int = 5,
    routing_type: str = "capability_match",
) -> HandoffPacketV2:
    return HandoffPacketV2(
        task_summary="test task",
        sender_agent_id=sender,
        timestamp="2026-04-29T00:00:00Z",
        routing_strategy=RoutingStrategy(type=routing_type),
        hop_count=hop_count,
        max_hops=max_hops,
    )


def _make_strategy(
    required: tuple[str, ...] = ("testing",),
    mode: str = "all",
    load_weight: float = 0.0,
    exclude: tuple[str, ...] = (),
    fallback_type: str = "error",
) -> CapabilityMatchStrategy:
    return CapabilityMatchStrategy(
        required_capabilities=required,
        match_mode=mode,
        load_factor_weight=load_weight,
        exclude_agent_ids=exclude,
        fallback=FallbackStrategy(type=fallback_type),
    )


@pytest.fixture()
def registry() -> CapabilityRegistry:
    return _make_registry(
        _make_agent("qa-agent", ["code_review", "testing", "security_testing"], tags=["qa"]),
        _make_agent("backend-agent", ["backend_development", "api_design", "testing"]),
        _make_agent(
            "research-agent", ["research", "paper_analysis", "summarization"], max_concurrent=3
        ),
        _make_agent(
            "leader-agent",
            ["team_leadership", "task_management"],
            max_concurrent=5,
        ),
    )


@pytest.fixture()
def router(registry: CapabilityRegistry) -> CapabilityMatchRouter:
    return CapabilityMatchRouter(loader=_make_loader(registry))


# ── ScoredCandidate ───────────────────────────────────────────────────────────


class TestScoredCandidate:
    def test_frozen(self):
        sc = ScoredCandidate(
            agent_id="qa-agent",
            capability_score=1.0,
            load_score=1.0,
            final_score=1.0,
        )
        with pytest.raises((AttributeError, TypeError)):
            sc.agent_id = "mutated"  # type: ignore[misc]


# ── RoutingLoopError ──────────────────────────────────────────────────────────


class TestRoutingLoopError:
    def test_attributes(self):
        err = RoutingLoopError(hop_count=5, max_hops=5)
        assert err.hop_count == 5
        assert err.max_hops == 5
        assert "5" in str(err)


# ── NoCapableAgentError ───────────────────────────────────────────────────────


class TestNoCapableAgentError:
    def test_attributes(self):
        err = NoCapableAgentError(("code_review",), "all")
        assert err.required_capabilities == ("code_review",)
        assert err.match_mode == "all"
        assert "code_review" in str(err)


# ── CapabilityMatchRouter.route — hop guard ───────────────────────────────────


class TestRouteHopGuard:
    async def test_raises_at_max_hops(self, router: CapabilityMatchRouter):
        packet = _make_packet(hop_count=5, max_hops=5)
        strategy = _make_strategy()
        with pytest.raises(RoutingLoopError):
            await router.route(strategy, packet)

    async def test_raises_when_hop_count_exceeds_max(self, router: CapabilityMatchRouter):
        packet = _make_packet(hop_count=6, max_hops=5)
        strategy = _make_strategy()
        with pytest.raises(RoutingLoopError):
            await router.route(strategy, packet)

    async def test_system_cap_at_5(self, router: CapabilityMatchRouter):
        """max_hops > 5 is still capped at 5 by SYSTEM_MAX_HOPS."""
        packet = _make_packet(hop_count=5, max_hops=10)
        strategy = _make_strategy()
        with pytest.raises(RoutingLoopError):
            await router.route(strategy, packet)

    async def test_does_not_raise_below_max(self, router: CapabilityMatchRouter):
        packet = _make_packet(hop_count=4, max_hops=5)
        strategy = _make_strategy(required=("testing",))
        result = await router.route(strategy, packet)
        assert result in ("qa-agent", "backend-agent")


# ── CapabilityMatchRouter.route — mode "all" ─────────────────────────────────


class TestRouteModeAll:
    async def test_selects_agent_with_all_caps(self, router: CapabilityMatchRouter):
        strategy = _make_strategy(required=("code_review", "testing"), mode="all")
        packet = _make_packet()
        result = await router.route(strategy, packet)
        assert result == "qa-agent"

    async def test_excludes_sender_automatically(self, router: CapabilityMatchRouter):
        # qa-agent is the sender — should be auto-excluded
        strategy = _make_strategy(required=("testing",), mode="all")
        packet = _make_packet(sender="qa-agent")
        result = await router.route(strategy, packet)
        # backend-agent also has testing
        assert result == "backend-agent"
        assert result != "qa-agent"

    async def test_explicit_exclude_agent_ids(self, router: CapabilityMatchRouter):
        strategy = _make_strategy(
            required=("testing",), mode="all", exclude=("qa-agent",)
        )
        packet = _make_packet()
        result = await router.route(strategy, packet)
        assert result != "qa-agent"

    async def test_no_capable_agent_raises(self, router: CapabilityMatchRouter):
        strategy = _make_strategy(
            required=("quantum_computing",), mode="all", fallback_type="error"
        )
        packet = _make_packet()
        with pytest.raises(NoCapableAgentError):
            await router.route(strategy, packet)

    async def test_returns_single_string(self, router: CapabilityMatchRouter):
        strategy = _make_strategy(required=("testing",), mode="all")
        packet = _make_packet()
        result = await router.route(strategy, packet)
        assert isinstance(result, str)
        assert len(result) > 0


# ── CapabilityMatchRouter.route — mode "any" ─────────────────────────────────


class TestRouteModeAny:
    async def test_selects_agent_with_any_cap(self, router: CapabilityMatchRouter):
        strategy = _make_strategy(
            required=("research", "code_review"), mode="any"
        )
        packet = _make_packet()
        result = await router.route(strategy, packet)
        assert result in ("qa-agent", "research-agent")

    async def test_prefers_higher_match_count(self, router: CapabilityMatchRouter):
        """qa-agent covers both testing + code_review → higher any-score."""
        strategy = _make_strategy(
            required=("testing", "code_review"), mode="any"
        )
        packet = _make_packet()
        result = await router.route(strategy, packet)
        # qa-agent matches 2/2; backend-agent matches 1/2 → qa-agent wins
        assert result == "qa-agent"


# ── CapabilityMatchRouter.route — mode "scored" ───────────────────────────────


class TestRouteModeScored:
    async def test_scored_mode_returns_valid_agent(self, router: CapabilityMatchRouter):
        strategy = _make_strategy(
            required=("testing",), mode="scored", load_weight=0.0
        )
        packet = _make_packet()
        result = await router.route(strategy, packet)
        assert result in ("qa-agent", "backend-agent")

    async def test_scored_pure_capability_zero_load_weight(
        self, router: CapabilityMatchRouter
    ):
        """load_factor_weight=0 → pure capability score, same as 'all'."""
        strategy = _make_strategy(
            required=("code_review",), mode="scored", load_weight=0.0
        )
        packet = _make_packet()
        result = await router.route(strategy, packet)
        assert result == "qa-agent"

    async def test_load_factor_prefers_high_capacity_agent(self):
        """With high load_factor_weight, higher max_concurrent wins ties."""
        registry = _make_registry(
            _make_agent("low-cap", ["testing"], max_concurrent=1),
            _make_agent("high-cap", ["testing"], max_concurrent=10),
        )
        # Simulate low-cap being fully loaded (1/1 tasks)
        def _load(aid: str) -> int:
            return 1 if aid == "low-cap" else 0

        router = CapabilityMatchRouter(loader=_make_loader(registry), load_provider=_load)
        strategy = _make_strategy(
            required=("testing",), mode="scored", load_weight=1.0
        )
        packet = _make_packet()
        result = await router.route(strategy, packet)
        assert result == "high-cap"

    async def test_scored_mode_falls_back_to_any_match_when_all_match_fails(self):
        """scored mode uses any-match when no agent satisfies all caps."""
        registry = _make_registry(
            _make_agent("partial-agent", ["testing"], max_concurrent=2),
        )
        router = CapabilityMatchRouter(loader=_make_loader(registry))
        # Require testing+code_review; partial-agent only has testing → no all-match
        strategy = _make_strategy(
            required=("testing", "code_review"), mode="scored", load_weight=0.0
        )
        packet = _make_packet()
        result = await router.route(strategy, packet)
        # Falls back to any-match → partial-agent matches "testing"
        assert result == "partial-agent"

    async def test_scored_mode_zero_max_concurrent_handled(self):
        """_score_candidates should not crash when max_concurrent_tasks == 0."""
        registry = _make_registry(
            _make_agent("zero-cap-agent", ["testing"], max_concurrent=0),
        )
        router = CapabilityMatchRouter(loader=_make_loader(registry))
        strategy = _make_strategy(
            required=("testing",), mode="scored", load_weight=0.5
        )
        packet = _make_packet()
        # Should not raise despite zero max_concurrent_tasks
        result = await router.route(strategy, packet)
        assert result == "zero-cap-agent"


# ── CapabilityMatchRouter.score_agent ─────────────────────────────────────────


class TestScoreAgent:
    def _router(self) -> CapabilityMatchRouter:
        return CapabilityMatchRouter(
            loader=MagicMock(),
            load_provider=lambda _: 0,
        )

    def test_all_caps_matched_zero_load_weight(self):
        agent = _make_agent("qa", ["testing", "code_review"], max_concurrent=2)
        strategy = _make_strategy(
            required=("testing", "code_review"), mode="scored", load_weight=0.0
        )
        score = self._router().score_agent(agent, strategy)
        assert score == pytest.approx(1.0)

    def test_partial_caps_matched(self):
        agent = _make_agent("qa", ["testing"], max_concurrent=2)
        strategy = _make_strategy(
            required=("testing", "code_review"), mode="scored", load_weight=0.0
        )
        score = self._router().score_agent(agent, strategy)
        assert score == pytest.approx(0.5)

    def test_no_caps_matched(self):
        agent = _make_agent("qa", ["testing"], max_concurrent=2)
        strategy = _make_strategy(
            required=("code_review",), mode="scored", load_weight=0.0
        )
        score = self._router().score_agent(agent, strategy)
        assert score == pytest.approx(0.0)

    def test_empty_required_caps_returns_full_score(self):
        agent = _make_agent("qa", ["testing"], max_concurrent=2)
        strategy = _make_strategy(required=(), mode="scored", load_weight=0.0)
        score = self._router().score_agent(agent, strategy)
        assert score == pytest.approx(1.0)

    def test_pure_load_factor(self):
        """load_weight=1.0 → score based only on availability."""
        agent = _make_agent("qa", ["testing"], max_concurrent=4)
        router = CapabilityMatchRouter(
            loader=MagicMock(),
            load_provider=lambda _: 2,  # 2 of 4 tasks occupied
        )
        strategy = _make_strategy(
            required=("testing",), mode="scored", load_weight=1.0
        )
        score = router.score_agent(agent, strategy)
        assert score == pytest.approx(0.5)  # (4-2)/4 = 0.5

    def test_load_weight_clamped_above_1(self):
        agent = _make_agent("qa", ["testing"], max_concurrent=2)
        strategy = CapabilityMatchStrategy(
            required_capabilities=("testing",),
            match_mode="scored",
            load_factor_weight=1.5,  # beyond valid range
        )
        score = self._router().score_agent(agent, strategy)
        assert 0.0 <= score <= 1.0  # must not blow up

    def test_load_weight_clamped_below_0(self):
        agent = _make_agent("qa", ["testing"], max_concurrent=2)
        strategy = CapabilityMatchStrategy(
            required_capabilities=("testing",),
            match_mode="scored",
            load_factor_weight=-0.5,
        )
        score = self._router().score_agent(agent, strategy)
        assert 0.0 <= score <= 1.0

    def test_zero_max_concurrent_availability_zero(self):
        agent = _make_agent("qa", ["testing"], max_concurrent=0)
        strategy = _make_strategy(
            required=("testing",), mode="scored", load_weight=1.0
        )
        score = self._router().score_agent(agent, strategy)
        assert score == pytest.approx(0.0)

    def test_score_rounded_to_4_decimals(self):
        agent = _make_agent("qa", ["testing", "a", "b"], max_concurrent=3)
        strategy = _make_strategy(required=("testing",), mode="scored", load_weight=0.0)
        score = self._router().score_agent(agent, strategy)
        assert score == round(score, 4)


# ── CapabilityMatchRouter.execute_fallback ────────────────────────────────────


class TestExecuteFallback:
    @pytest.fixture()
    def simple_router(self, registry: CapabilityRegistry) -> CapabilityMatchRouter:
        return CapabilityMatchRouter(loader=_make_loader(registry))

    async def test_error_raises(self, simple_router: CapabilityMatchRouter):
        fb = FallbackStrategy(type="error")
        with pytest.raises(NoCapableAgentError):
            await simple_router.execute_fallback(fb, ["agent-a"])

    async def test_none_raises(self, simple_router: CapabilityMatchRouter):
        fb = FallbackStrategy(type="none")
        with pytest.raises(NoCapableAgentError):
            await simple_router.execute_fallback(fb, ["agent-a"])

    async def test_empty_candidates_raises(self, simple_router: CapabilityMatchRouter):
        fb = FallbackStrategy(type="random")
        with pytest.raises(NoCapableAgentError):
            await simple_router.execute_fallback(fb, [])

    async def test_random_returns_from_candidates(self, simple_router: CapabilityMatchRouter):
        candidates = ["agent-a", "agent-b", "agent-c"]
        fb = FallbackStrategy(type="random")
        result = await simple_router.execute_fallback(fb, candidates)
        assert result in candidates

    async def test_round_robin_cycles(self, simple_router: CapabilityMatchRouter):
        candidates = ["agent-b", "agent-a", "agent-c"]  # unsorted on purpose
        sorted_c = sorted(candidates)
        fb = FallbackStrategy(type="round_robin")
        results = [
            await simple_router.execute_fallback(fb, candidates)
            for _ in range(len(sorted_c) + 1)
        ]
        assert results[:3] == sorted_c  # first full cycle
        assert results[3] == sorted_c[0]  # wraps around

    async def test_least_loaded_picks_idle_agent(
        self, registry: CapabilityRegistry
    ):
        """least_loaded prefers the agent with the highest free capacity."""
        def _load(aid: str) -> int:
            return {"qa-agent": 2, "backend-agent": 0, "research-agent": 1}[aid]

        router = CapabilityMatchRouter(
            loader=_make_loader(registry), load_provider=_load
        )
        fb = FallbackStrategy(type="least_loaded")
        result = await router.execute_fallback(
            fb, ["qa-agent", "backend-agent", "research-agent"]
        )
        # backend-agent has 0/2 → ratio 0.0 (lowest load)
        assert result == "backend-agent"

    async def test_single_candidate_always_selected(self, simple_router: CapabilityMatchRouter):
        for fb_type in ("random", "round_robin", "least_loaded"):
            fb = FallbackStrategy(type=fb_type)
            result = await simple_router.execute_fallback(fb, ["only-agent"])
            assert result == "only-agent"


# ── Activity logger ───────────────────────────────────────────────────────────


class TestActivityLogger:
    async def test_logger_called_on_route(self, registry: CapabilityRegistry):
        log_entries: list[dict[str, Any]] = []
        router = CapabilityMatchRouter(
            loader=_make_loader(registry),
            activity_logger=log_entries.append,
        )
        strategy = _make_strategy(required=("testing",))
        packet = _make_packet()
        await router.route(strategy, packet)
        assert len(log_entries) == 1

    async def test_log_entry_contains_required_fields(
        self, registry: CapabilityRegistry
    ):
        log_entries: list[dict[str, Any]] = []
        router = CapabilityMatchRouter(
            loader=_make_loader(registry),
            activity_logger=log_entries.append,
        )
        strategy = _make_strategy(required=("testing",), mode="all")
        packet = _make_packet(sender="test-sender")
        await router.route(strategy, packet)

        entry = log_entries[0]
        assert entry["event"] == "capability_match_route"
        assert entry["sender_agent_id"] == "test-sender"
        assert "selected_agent_id" in entry
        assert "required_capabilities" in entry
        assert "match_mode" in entry
        assert "timestamp" in entry

    async def test_no_logger_does_not_raise(self, router: CapabilityMatchRouter):
        """Router without activity_logger should not crash."""
        strategy = _make_strategy(required=("testing",))
        packet = _make_packet()
        result = await router.route(strategy, packet)
        assert isinstance(result, str)


# ── Fallback integration ──────────────────────────────────────────────────────


class TestFallbackIntegration:
    async def test_random_fallback_when_no_primary_match(
        self, registry: CapabilityRegistry
    ):
        router = CapabilityMatchRouter(loader=_make_loader(registry))
        strategy = _make_strategy(
            required=("quantum_computing",), fallback_type="random"
        )
        packet = _make_packet()
        result = await router.route(strategy, packet)
        # fallback pool = all agents not excluded
        assert result in registry.agents

    async def test_error_fallback_when_no_primary_match(
        self, router: CapabilityMatchRouter
    ):
        strategy = _make_strategy(
            required=("nonexistent_cap",), fallback_type="error"
        )
        packet = _make_packet()
        with pytest.raises(NoCapableAgentError):
            await router.route(strategy, packet)

    async def test_round_robin_fallback(self, registry: CapabilityRegistry):
        router = CapabilityMatchRouter(loader=_make_loader(registry))
        strategy = _make_strategy(
            required=("nonexistent_cap",), fallback_type="round_robin"
        )
        packet = _make_packet()
        result = await router.route(strategy, packet)
        assert result in registry.agents


# ── Auto-exclude sender ───────────────────────────────────────────────────────


class TestAutoExcludeSender:
    async def test_sender_excluded_from_all_results(self, router: CapabilityMatchRouter):
        strategy = _make_strategy(required=("testing",))
        # Even if qa-agent is the best match, it should be excluded as sender
        packet = _make_packet(sender="qa-agent")
        result = await router.route(strategy, packet)
        assert result != "qa-agent"
