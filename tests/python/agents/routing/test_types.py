"""Unit tests for duduclaw.agents.routing.types"""

from __future__ import annotations

import pytest

from duduclaw.agents.routing.types import (
    CapabilityMatchStrategy,
    FallbackStrategy,
    HandoffPacketV1,
    HandoffPacketV2,
    LazyRef,
    RoutingStrategy,
    ToolCall,
    normalize_packet,
)


# ── Fixtures ──────────────────────────────────────────────────────────────────


def _make_v1(
    task_summary: str = "test task",
    sender: str = "agent-a",
    timestamp: str = "2026-04-29T00:00:00Z",
    memory_refs: tuple[str, ...] = (),
) -> HandoffPacketV1:
    return HandoffPacketV1(
        task_summary=task_summary,
        sender_agent_id=sender,
        timestamp=timestamp,
        memory_refs=memory_refs,
    )


def _make_v2(
    task_summary: str = "test task",
    sender: str = "agent-a",
    timestamp: str = "2026-04-29T00:00:00Z",
    routing_type: str = "broadcast",
) -> HandoffPacketV2:
    return HandoffPacketV2(
        task_summary=task_summary,
        sender_agent_id=sender,
        timestamp=timestamp,
        routing_strategy=RoutingStrategy(type=routing_type),
    )


# ── ToolCall ──────────────────────────────────────────────────────────────────


class TestToolCall:
    def test_frozen(self):
        tc = ToolCall(tool_name="wiki_read", input={}, output="result", called_at="2026-04-29")
        with pytest.raises((AttributeError, TypeError)):
            tc.tool_name = "mutated"  # type: ignore[misc]

    def test_fields_set_correctly(self):
        tc = ToolCall(
            tool_name="memory_search",
            input={"query": "foo"},
            output="bar",
            called_at="2026-04-29T01:00:00Z",
        )
        assert tc.tool_name == "memory_search"
        assert tc.input == {"query": "foo"}
        assert tc.output == "bar"
        assert tc.called_at == "2026-04-29T01:00:00Z"


# ── LazyRef ───────────────────────────────────────────────────────────────────


class TestLazyRef:
    def test_frozen(self):
        ref = LazyRef(ref_type="memory", ref_id="key-1")
        with pytest.raises((AttributeError, TypeError)):
            ref.ref_id = "mutated"  # type: ignore[misc]

    def test_default_resolution_policy(self):
        ref = LazyRef(ref_type="memory", ref_id="key-1")
        assert ref.resolution_policy == "lazy"

    def test_custom_resolution_policy(self):
        ref = LazyRef(ref_type="wiki", ref_id="page-1", resolution_policy="eager")
        assert ref.resolution_policy == "eager"

    def test_metadata_defaults_empty_dict(self):
        ref = LazyRef(ref_type="memory", ref_id="key-1")
        assert ref.metadata == {}

    def test_custom_metadata(self):
        ref = LazyRef(ref_type="artifact", ref_id="id-1", metadata={"size": 42})
        assert ref.metadata["size"] == 42


# ── FallbackStrategy ──────────────────────────────────────────────────────────


class TestFallbackStrategy:
    def test_default_type_is_error(self):
        fb = FallbackStrategy()
        assert fb.type == "error"

    def test_random_type(self):
        fb = FallbackStrategy(type="random")
        assert fb.type == "random"

    def test_all_valid_types(self):
        for t in ("none", "random", "round_robin", "least_loaded", "error"):
            fb = FallbackStrategy(type=t)
            assert fb.type == t

    def test_frozen(self):
        fb = FallbackStrategy()
        with pytest.raises((AttributeError, TypeError)):
            fb.type = "random"  # type: ignore[misc]


# ── CapabilityMatchStrategy ───────────────────────────────────────────────────


class TestCapabilityMatchStrategy:
    def test_required_capabilities_stored_as_tuple(self):
        s = CapabilityMatchStrategy(required_capabilities=("code_review", "testing"))
        assert isinstance(s.required_capabilities, tuple)
        assert "code_review" in s.required_capabilities

    def test_default_match_mode_is_all(self):
        s = CapabilityMatchStrategy(required_capabilities=("testing",))
        assert s.match_mode == "all"

    def test_default_load_factor_weight_zero(self):
        s = CapabilityMatchStrategy(required_capabilities=("testing",))
        assert s.load_factor_weight == 0.0

    def test_default_exclude_empty_tuple(self):
        s = CapabilityMatchStrategy(required_capabilities=("testing",))
        assert s.exclude_agent_ids == ()

    def test_default_fallback_is_error(self):
        s = CapabilityMatchStrategy(required_capabilities=("testing",))
        assert s.fallback.type == "error"

    def test_custom_match_mode(self):
        for mode in ("all", "any", "scored"):
            s = CapabilityMatchStrategy(
                required_capabilities=("testing",), match_mode=mode
            )
            assert s.match_mode == mode

    def test_frozen(self):
        s = CapabilityMatchStrategy(required_capabilities=("testing",))
        with pytest.raises((AttributeError, TypeError)):
            s.match_mode = "any"  # type: ignore[misc]

    def test_load_factor_weight_stored(self):
        s = CapabilityMatchStrategy(
            required_capabilities=("testing",),
            match_mode="scored",
            load_factor_weight=0.4,
        )
        assert s.load_factor_weight == pytest.approx(0.4)


# ── RoutingStrategy ───────────────────────────────────────────────────────────


class TestRoutingStrategy:
    def test_broadcast_type(self):
        rs = RoutingStrategy(type="broadcast")
        assert rs.type == "broadcast"
        assert rs.capability_match is None
        assert rs.direct_target is None

    def test_direct_type_with_target(self):
        rs = RoutingStrategy(type="direct", direct_target="qa-agent")
        assert rs.direct_target == "qa-agent"

    def test_capability_match_type(self):
        strategy = CapabilityMatchStrategy(required_capabilities=("testing",))
        rs = RoutingStrategy(type="capability_match", capability_match=strategy)
        assert rs.capability_match is strategy

    def test_frozen(self):
        rs = RoutingStrategy(type="broadcast")
        with pytest.raises((AttributeError, TypeError)):
            rs.type = "direct"  # type: ignore[misc]


# ── HandoffPacketV1 ───────────────────────────────────────────────────────────


class TestHandoffPacketV1:
    def test_required_fields(self):
        pkt = _make_v1()
        assert pkt.task_summary == "test task"
        assert pkt.sender_agent_id == "agent-a"
        assert pkt.timestamp == "2026-04-29T00:00:00Z"

    def test_default_memory_refs_empty(self):
        pkt = _make_v1()
        assert pkt.memory_refs == ()

    def test_default_partial_results_none(self):
        pkt = _make_v1()
        assert pkt.partial_results is None

    def test_frozen(self):
        pkt = _make_v1()
        with pytest.raises((AttributeError, TypeError)):
            pkt.task_summary = "mutated"  # type: ignore[misc]

    def test_memory_refs_as_tuple(self):
        pkt = HandoffPacketV1(
            task_summary="t",
            sender_agent_id="s",
            timestamp="ts",
            memory_refs=("key-1", "key-2"),
        )
        assert pkt.memory_refs == ("key-1", "key-2")


# ── HandoffPacketV2 ───────────────────────────────────────────────────────────


class TestHandoffPacketV2:
    def test_required_fields(self):
        pkt = _make_v2()
        assert pkt.task_summary == "test task"
        assert pkt.sender_agent_id == "agent-a"
        assert pkt.routing_strategy.type == "broadcast"

    def test_default_version_v2(self):
        assert _make_v2().version == "v2"

    def test_default_max_hops_5(self):
        assert _make_v2().max_hops == 5

    def test_default_hop_count_0(self):
        assert _make_v2().hop_count == 0

    def test_default_memory_refs_empty(self):
        assert _make_v2().memory_refs == ()

    def test_default_tool_history_empty(self):
        assert _make_v2().tool_history == ()

    def test_default_partial_results_none(self):
        assert _make_v2().partial_results is None

    def test_frozen(self):
        pkt = _make_v2()
        with pytest.raises((AttributeError, TypeError)):
            pkt.hop_count = 3  # type: ignore[misc]

    def test_capability_match_routing_strategy(self):
        strategy = CapabilityMatchStrategy(required_capabilities=("testing",))
        rs = RoutingStrategy(type="capability_match", capability_match=strategy)
        pkt = HandoffPacketV2(
            task_summary="t",
            sender_agent_id="s",
            timestamp="ts",
            routing_strategy=rs,
        )
        assert pkt.routing_strategy.type == "capability_match"
        assert pkt.routing_strategy.capability_match is strategy

    def test_lazy_ref_in_memory_refs(self):
        ref = LazyRef(ref_type="memory", ref_id="key-1")
        pkt = HandoffPacketV2(
            task_summary="t",
            sender_agent_id="s",
            timestamp="ts",
            routing_strategy=RoutingStrategy(type="broadcast"),
            memory_refs=(ref,),
        )
        assert pkt.memory_refs[0] == ref


# ── normalize_packet ──────────────────────────────────────────────────────────


class TestNormalizePacket:
    def test_v2_returned_unchanged(self):
        pkt = _make_v2()
        result = normalize_packet(pkt)
        assert result is pkt  # exact same object

    def test_v1_converted_to_v2(self):
        pkt = _make_v1()
        result = normalize_packet(pkt)
        assert isinstance(result, HandoffPacketV2)
        assert result.version == "v2"

    def test_v1_fields_preserved(self):
        pkt = _make_v1(task_summary="my task", sender="eng-agent")
        result = normalize_packet(pkt)
        assert result.task_summary == "my task"
        assert result.sender_agent_id == "eng-agent"
        assert result.timestamp == "2026-04-29T00:00:00Z"

    def test_v1_routing_strategy_defaults_broadcast(self):
        result = normalize_packet(_make_v1())
        assert result.routing_strategy.type == "broadcast"

    def test_v1_string_memory_refs_wrapped_as_lazy_refs(self):
        pkt = _make_v1(memory_refs=("mem-key-1", "mem-key-2"))
        result = normalize_packet(pkt)
        assert len(result.memory_refs) == 2
        for ref in result.memory_refs:
            assert isinstance(ref, LazyRef)
            assert ref.ref_type == "memory"
            assert ref.resolution_policy == "lazy"

    def test_v1_memory_ref_ids_preserved(self):
        pkt = _make_v1(memory_refs=("mem-key-1", "mem-key-2"))
        result = normalize_packet(pkt)
        ref_ids = [r.ref_id for r in result.memory_refs]
        assert ref_ids == ["mem-key-1", "mem-key-2"]

    def test_v1_empty_memory_refs_stays_empty(self):
        result = normalize_packet(_make_v1(memory_refs=()))
        assert result.memory_refs == ()

    def test_v1_partial_results_preserved(self):
        pkt = HandoffPacketV1(
            task_summary="t",
            sender_agent_id="s",
            timestamp="ts",
            partial_results={"key": "value"},
        )
        result = normalize_packet(pkt)
        assert result.partial_results == {"key": "value"}

    def test_v1_tool_history_preserved(self):
        tc = ToolCall(tool_name="wiki_read", input={}, output="x", called_at="ts")
        pkt = HandoffPacketV1(
            task_summary="t",
            sender_agent_id="s",
            timestamp="ts",
            tool_history=(tc,),
        )
        result = normalize_packet(pkt)
        assert result.tool_history == (tc,)

    def test_v1_defaults_max_hops_5(self):
        result = normalize_packet(_make_v1())
        assert result.max_hops == 5

    def test_v1_defaults_hop_count_0(self):
        result = normalize_packet(_make_v1())
        assert result.hop_count == 0
