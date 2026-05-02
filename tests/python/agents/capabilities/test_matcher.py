"""Unit tests for duduclaw.agents.capabilities.matcher"""

from __future__ import annotations

import pytest

from duduclaw.agents.capabilities.loader import AgentCapability, CapabilityRegistry
from duduclaw.agents.capabilities.matcher import CapabilityMatcher, MatchResult


# ── Fixtures ──────────────────────────────────────────────────────────────────


def _make_capability(
    agent_id: str,
    capabilities: list[str],
    max_concurrent: int = 2,
    tags: list[str] | None = None,
    description: str = "",
) -> AgentCapability:
    return AgentCapability(
        agent_id=agent_id,
        capabilities=tuple(capabilities),
        max_concurrent_tasks=max_concurrent,
        description=description,
        tags=tuple(tags or []),
    )


@pytest.fixture()
def registry() -> CapabilityRegistry:
    """Small registry used across most tests."""
    return CapabilityRegistry(
        schema_version="1.0",
        agents={
            "qa-agent": _make_capability(
                "qa-agent",
                ["code_review", "testing", "security_testing"],
                tags=["qa", "security"],
            ),
            "backend-agent": _make_capability(
                "backend-agent",
                ["backend_development", "api_design", "testing"],
                tags=["engineering", "backend"],
            ),
            "research-agent": _make_capability(
                "research-agent",
                ["research", "paper_analysis", "summarization"],
                tags=["research"],
                max_concurrent=3,
            ),
            "leader-agent": _make_capability(
                "leader-agent",
                ["team_leadership", "task_management", "agent_coordination"],
                tags=["leadership"],
                max_concurrent=5,
            ),
        },
    )


@pytest.fixture()
def matcher(registry: CapabilityRegistry) -> CapabilityMatcher:
    return CapabilityMatcher(registry)


# ── MatchResult ───────────────────────────────────────────────────────────────


class TestMatchResult:
    def test_immutable(self, registry: CapabilityRegistry):
        cap = registry.agents["qa-agent"]
        result = MatchResult(
            agent_id="qa-agent",
            capability=cap,
            matched_capabilities=("testing",),
            score=1.0,
        )
        with pytest.raises((AttributeError, TypeError)):
            result.agent_id = "mutated"  # type: ignore[misc]


# ── find_by_capability ────────────────────────────────────────────────────────


class TestFindByCapability:
    def test_returns_agent_with_all_caps(self, matcher: CapabilityMatcher):
        # qa-agent has both code_review AND testing; backend-agent has testing but NOT code_review
        results = matcher.find_by_capability(["code_review", "testing"])
        ids = [r.agent_id for r in results]
        assert "qa-agent" in ids
        assert "backend-agent" not in ids  # missing code_review → excluded

    def test_single_cap_matches_multiple_agents(self, matcher: CapabilityMatcher):
        # Both qa-agent and backend-agent have "testing"
        results = matcher.find_by_capability(["testing"])
        ids = [r.agent_id for r in results]
        assert "qa-agent" in ids
        assert "backend-agent" in ids

    def test_excludes_agents_missing_required_cap(self, matcher: CapabilityMatcher):
        results = matcher.find_by_capability(["security_testing"])
        ids = [r.agent_id for r in results]
        assert "qa-agent" in ids
        assert "backend-agent" not in ids

    def test_all_required_must_match(self, matcher: CapabilityMatcher):
        results = matcher.find_by_capability(["code_review", "backend_development"])
        assert results == []  # no agent has both

    def test_sorted_by_descending_score(self, matcher: CapabilityMatcher):
        results = matcher.find_by_capability(["testing"])
        scores = [r.score for r in results]
        assert scores == sorted(scores, reverse=True)

    def test_exclude_agent_ids(self, matcher: CapabilityMatcher):
        results = matcher.find_by_capability(["testing"], exclude_agent_ids=["qa-agent"])
        ids = [r.agent_id for r in results]
        assert "qa-agent" not in ids
        assert "backend-agent" in ids

    def test_empty_required_returns_empty(self, matcher: CapabilityMatcher):
        assert matcher.find_by_capability([]) == []

    def test_nonexistent_capability_returns_empty(self, matcher: CapabilityMatcher):
        assert matcher.find_by_capability(["does_not_exist"]) == []

    def test_matched_capabilities_reflect_query(self, matcher: CapabilityMatcher):
        results = matcher.find_by_capability(["code_review"])
        assert all("code_review" in r.matched_capabilities for r in results)

    def test_score_above_1_when_agent_has_extra_caps(self, matcher: CapabilityMatcher):
        # qa-agent has 3 caps; matching 1 should yield bonus
        results = matcher.find_by_capability(["testing"])
        qa_result = next(r for r in results if r.agent_id == "qa-agent")
        assert qa_result.score > 1.0

    def test_tiebreak_by_agent_id_alphabetically(self):
        """Agents with equal score are sorted alphabetically by agent_id."""
        # Create two agents with identical cap sets
        reg = CapabilityRegistry(
            schema_version="1.0",
            agents={
                "zzz-agent": _make_capability("zzz-agent", ["cap_x"], max_concurrent=1),
                "aaa-agent": _make_capability("aaa-agent", ["cap_x"], max_concurrent=1),
            },
        )
        m = CapabilityMatcher(reg)
        results = m.find_by_capability(["cap_x"])
        assert results[0].agent_id == "aaa-agent"
        assert results[1].agent_id == "zzz-agent"


# ── find_by_any_capability ────────────────────────────────────────────────────


class TestFindByAnyCapability:
    def test_matches_agents_with_any_cap(self, matcher: CapabilityMatcher):
        results = matcher.find_by_any_capability(["research", "code_review"])
        ids = {r.agent_id for r in results}
        assert "research-agent" in ids
        assert "qa-agent" in ids

    def test_score_proportional_to_matches(self, matcher: CapabilityMatcher):
        # qa-agent has both testing and code_review (requested 2)
        results = matcher.find_by_any_capability(["testing", "code_review"])
        qa_result = next(r for r in results if r.agent_id == "qa-agent")
        assert qa_result.score == pytest.approx(1.0)  # 2/2

        backend_result = next(r for r in results if r.agent_id == "backend-agent")
        assert backend_result.score == pytest.approx(0.5)  # 1/2

    def test_sorted_descending(self, matcher: CapabilityMatcher):
        results = matcher.find_by_any_capability(["testing", "code_review"])
        scores = [r.score for r in results]
        assert scores == sorted(scores, reverse=True)

    def test_exclude_agent_ids(self, matcher: CapabilityMatcher):
        results = matcher.find_by_any_capability(
            ["code_review"], exclude_agent_ids=["qa-agent"]
        )
        ids = [r.agent_id for r in results]
        assert "qa-agent" not in ids

    def test_empty_list_returns_empty(self, matcher: CapabilityMatcher):
        assert matcher.find_by_any_capability([]) == []

    def test_no_match_returns_empty(self, matcher: CapabilityMatcher):
        assert matcher.find_by_any_capability(["nonexistent_cap"]) == []


# ── find_by_tag ───────────────────────────────────────────────────────────────


class TestFindByTag:
    def test_matches_by_tag(self, matcher: CapabilityMatcher):
        results = matcher.find_by_tag(["qa"])
        ids = [r.agent_id for r in results]
        assert "qa-agent" in ids

    def test_multiple_tags_score(self, matcher: CapabilityMatcher):
        results = matcher.find_by_tag(["qa", "security"])
        qa_result = next(r for r in results if r.agent_id == "qa-agent")
        assert qa_result.score == pytest.approx(1.0)  # matches both

    def test_matched_capabilities_is_empty_tuple(self, matcher: CapabilityMatcher):
        results = matcher.find_by_tag(["research"])
        assert all(r.matched_capabilities == () for r in results)

    def test_exclude_agent_ids(self, matcher: CapabilityMatcher):
        results = matcher.find_by_tag(["qa"], exclude_agent_ids=["qa-agent"])
        ids = [r.agent_id for r in results]
        assert "qa-agent" not in ids

    def test_empty_tags_returns_empty(self, matcher: CapabilityMatcher):
        assert matcher.find_by_tag([]) == []

    def test_no_match_returns_empty(self, matcher: CapabilityMatcher):
        assert matcher.find_by_tag(["nonexistent_tag"]) == []


# ── get_agent ─────────────────────────────────────────────────────────────────


class TestGetAgent:
    def test_returns_capability_for_known_agent(self, matcher: CapabilityMatcher):
        cap = matcher.get_agent("qa-agent")
        assert cap is not None
        assert cap.agent_id == "qa-agent"

    def test_returns_none_for_unknown_agent(self, matcher: CapabilityMatcher):
        assert matcher.get_agent("ghost-agent") is None


# ── list_all_capabilities ─────────────────────────────────────────────────────


class TestListAllCapabilities:
    def test_returns_sorted_unique_list(self, matcher: CapabilityMatcher):
        caps = matcher.list_all_capabilities()
        assert caps == sorted(set(caps))

    def test_includes_known_capabilities(self, matcher: CapabilityMatcher):
        caps = matcher.list_all_capabilities()
        assert "code_review" in caps
        assert "research" in caps
        assert "team_leadership" in caps

    def test_no_duplicates(self, matcher: CapabilityMatcher):
        caps = matcher.list_all_capabilities()
        assert len(caps) == len(set(caps))


# ── list_all_tags ─────────────────────────────────────────────────────────────


class TestListAllTags:
    def test_returns_sorted_unique_list(self, matcher: CapabilityMatcher):
        tags = matcher.list_all_tags()
        assert tags == sorted(set(tags))

    def test_includes_known_tags(self, matcher: CapabilityMatcher):
        tags = matcher.list_all_tags()
        assert "qa" in tags
        assert "research" in tags

    def test_no_duplicates(self, matcher: CapabilityMatcher):
        tags = matcher.list_all_tags()
        assert len(tags) == len(set(tags))


# ── list_agents_by_capability ─────────────────────────────────────────────────


class TestListAgentsByCapability:
    def test_returns_agents_with_capability(self, matcher: CapabilityMatcher):
        agents = matcher.list_agents_by_capability("testing")
        assert "qa-agent" in agents
        assert "backend-agent" in agents
        assert "research-agent" not in agents

    def test_returns_sorted_list(self, matcher: CapabilityMatcher):
        agents = matcher.list_agents_by_capability("testing")
        assert agents == sorted(agents)

    def test_unknown_capability_returns_empty(self, matcher: CapabilityMatcher):
        assert matcher.list_agents_by_capability("nonexistent") == []


# ── Integration: full HandoffPacket routing scenario ─────────────────────────


class TestHandoffRoutingScenario:
    """Simulate how the HandoffPacket router selects a target agent."""

    def test_route_code_review_task(self, matcher: CapabilityMatcher):
        """Routing 'code_review' should prefer QA agents."""
        results = matcher.find_by_capability(["code_review"])
        assert len(results) > 0
        assert results[0].agent_id == "qa-agent"  # only agent with code_review

    def test_route_security_review_task(self, matcher: CapabilityMatcher):
        results = matcher.find_by_capability(["code_review", "security_testing"])
        assert results[0].agent_id == "qa-agent"

    def test_route_research_task(self, matcher: CapabilityMatcher):
        results = matcher.find_by_capability(["research", "summarization"])
        assert results[0].agent_id == "research-agent"

    def test_no_suitable_agent_returns_empty(self, matcher: CapabilityMatcher):
        results = matcher.find_by_capability(["quantum_computing"])
        assert results == []

    def test_exclude_self_from_routing(self, matcher: CapabilityMatcher):
        """Caller should exclude itself to avoid self-delegation."""
        results = matcher.find_by_capability(
            ["testing"], exclude_agent_ids=["backend-agent"]
        )
        ids = [r.agent_id for r in results]
        assert "backend-agent" not in ids
        assert "qa-agent" in ids
