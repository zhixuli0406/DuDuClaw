"""Unit tests for duduclaw.agents.routing.resolution"""

from __future__ import annotations

import pytest

from duduclaw.agents.routing.resolution import (
    ResolutionError,
    ResolutionPolicy,
    ResolutionResult,
    Resolver,
)
from duduclaw.agents.routing.types import LazyRef


# ── Test helpers ──────────────────────────────────────────────────────────────


class _StaticResolver(Resolver):
    """A resolver that always returns a fixed value."""

    def __init__(self, ref_type: str, value: str = "resolved_value") -> None:
        self._ref_type = ref_type
        self._value = value

    @property
    def ref_type(self) -> str:
        return self._ref_type

    async def resolve(self, ref: LazyRef) -> str:
        return self._value


class _ErrorResolver(Resolver):
    """A resolver that always raises ResolutionError."""

    def __init__(self, ref_type: str = "error_type") -> None:
        self._ref_type = ref_type

    @property
    def ref_type(self) -> str:
        return self._ref_type

    async def resolve(self, ref: LazyRef) -> str:
        raise ResolutionError(f"Cannot resolve {ref.ref_id!r}")


class _CrashResolver(Resolver):
    """A resolver that raises an unexpected exception."""

    @property
    def ref_type(self) -> str:
        return "crash_type"

    async def resolve(self, ref: LazyRef) -> str:
        raise RuntimeError("unexpected crash")


class _EchoResolver(Resolver):
    """Returns the ref_id as the resolved value."""

    def __init__(self, ref_type: str) -> None:
        self._ref_type = ref_type

    @property
    def ref_type(self) -> str:
        return self._ref_type

    async def resolve(self, ref: LazyRef) -> str:
        return f"resolved:{ref.ref_id}"


# ── Fixtures ──────────────────────────────────────────────────────────────────


@pytest.fixture()
def engine() -> ResolutionPolicy:
    return ResolutionPolicy()


def _make_ref(
    ref_type: str = "memory",
    ref_id: str = "key-1",
    policy: str = "lazy",
) -> LazyRef:
    return LazyRef(ref_type=ref_type, ref_id=ref_id, resolution_policy=policy)


# ── ResolutionError ───────────────────────────────────────────────────────────


class TestResolutionError:
    def test_is_exception(self):
        err = ResolutionError("oops")
        assert isinstance(err, Exception)

    def test_message_preserved(self):
        err = ResolutionError("cannot fetch key-1")
        assert "key-1" in str(err)


# ── ResolutionResult ──────────────────────────────────────────────────────────


class TestResolutionResult:
    def test_frozen(self):
        ref = _make_ref()
        result = ResolutionResult(ref=ref, value="x", resolved=True)
        with pytest.raises((AttributeError, TypeError)):
            result.resolved = False  # type: ignore[misc]

    def test_success_result(self):
        ref = _make_ref()
        result = ResolutionResult(ref=ref, value="hello", resolved=True)
        assert result.resolved is True
        assert result.value == "hello"
        assert result.error is None

    def test_failure_result(self):
        ref = _make_ref()
        result = ResolutionResult(ref=ref, value=None, resolved=False, error="not found")
        assert result.resolved is False
        assert result.value is None
        assert result.error == "not found"


# ── Resolver ABC ──────────────────────────────────────────────────────────────


class TestResolverABC:
    def test_cannot_instantiate_abstract_class(self):
        with pytest.raises(TypeError):
            Resolver()  # type: ignore[abstract]

    def test_concrete_resolver_works(self):
        r = _StaticResolver("memory")
        assert r.ref_type == "memory"

    def test_ref_type_property_required(self):
        class _Incomplete(Resolver):
            async def resolve(self, ref: LazyRef):
                return "ok"

        with pytest.raises(TypeError):
            _Incomplete()  # type: ignore[abstract]


# ── ResolutionPolicy.register ─────────────────────────────────────────────────


class TestResolutionPolicyRegister:
    def test_register_single_resolver(self, engine: ResolutionPolicy):
        engine.register(_StaticResolver("memory"))
        assert engine.get_resolver("memory") is not None

    def test_last_registration_wins(self, engine: ResolutionPolicy):
        first = _StaticResolver("memory", value="first")
        second = _StaticResolver("memory", value="second")
        engine.register(first)
        engine.register(second)
        assert engine.get_resolver("memory") is second

    def test_unknown_type_returns_none(self, engine: ResolutionPolicy):
        assert engine.get_resolver("nonexistent") is None

    def test_registered_types_sorted(self, engine: ResolutionPolicy):
        engine.register(_StaticResolver("wiki"))
        engine.register(_StaticResolver("memory"))
        assert engine.registered_types == ["memory", "wiki"]

    def test_empty_engine_registered_types(self, engine: ResolutionPolicy):
        assert engine.registered_types == []


# ── ResolutionPolicy.resolve ──────────────────────────────────────────────────


class TestResolutionPolicyResolve:
    async def test_success(self, engine: ResolutionPolicy):
        engine.register(_StaticResolver("memory", value="page_content"))
        ref = _make_ref(ref_type="memory", ref_id="key-1")
        result = await engine.resolve(ref)
        assert result.resolved is True
        assert result.value == "page_content"
        assert result.error is None

    async def test_no_resolver_returns_unresolved(self, engine: ResolutionPolicy):
        ref = _make_ref(ref_type="wiki")
        result = await engine.resolve(ref)
        assert result.resolved is False
        assert result.value is None
        assert "wiki" in (result.error or "")

    async def test_resolution_error_captured(self, engine: ResolutionPolicy):
        engine.register(_ErrorResolver("memory"))
        ref = _make_ref(ref_type="memory", ref_id="missing-key")
        result = await engine.resolve(ref)
        assert result.resolved is False
        assert result.value is None
        assert result.error is not None

    async def test_unexpected_exception_captured(self, engine: ResolutionPolicy):
        engine.register(_CrashResolver())
        ref = _make_ref(ref_type="crash_type")
        result = await engine.resolve(ref)
        assert result.resolved is False
        assert "unexpected" in (result.error or "").lower()

    async def test_ref_preserved_in_result(self, engine: ResolutionPolicy):
        engine.register(_StaticResolver("memory"))
        ref = _make_ref(ref_type="memory", ref_id="my-key")
        result = await engine.resolve(ref)
        assert result.ref is ref

    async def test_echo_resolver(self, engine: ResolutionPolicy):
        engine.register(_EchoResolver("memory"))
        ref = _make_ref(ref_type="memory", ref_id="abc-123")
        result = await engine.resolve(ref)
        assert result.value == "resolved:abc-123"


# ── ResolutionPolicy.resolve_all ──────────────────────────────────────────────


class TestResolutionPolicyResolveAll:
    async def test_empty_list_returns_empty(self, engine: ResolutionPolicy):
        results = await engine.resolve_all([])
        assert results == []

    async def test_all_succeed(self, engine: ResolutionPolicy):
        engine.register(_EchoResolver("memory"))
        refs = [_make_ref(ref_id=f"k-{i}") for i in range(3)]
        results = await engine.resolve_all(refs)
        assert len(results) == 3
        assert all(r.resolved for r in results)

    async def test_partial_failure_does_not_cancel_others(self, engine: ResolutionPolicy):
        engine.register(_StaticResolver("memory"))
        engine.register(_ErrorResolver("wiki"))
        refs = [
            _make_ref(ref_type="memory", ref_id="k-1"),
            _make_ref(ref_type="wiki", ref_id="p-1"),
            _make_ref(ref_type="memory", ref_id="k-2"),
        ]
        results = await engine.resolve_all(refs)
        assert len(results) == 3
        resolved_states = [r.resolved for r in results]
        assert resolved_states == [True, False, True]

    async def test_result_order_matches_input(self, engine: ResolutionPolicy):
        engine.register(_EchoResolver("memory"))
        refs = [_make_ref(ref_id=f"key-{i}") for i in range(5)]
        results = await engine.resolve_all(refs)
        for i, (ref, result) in enumerate(zip(refs, results)):
            assert result.ref is ref

    async def test_no_resolver_all_unresolved(self, engine: ResolutionPolicy):
        refs = [_make_ref(ref_type="unknown_type", ref_id=f"r-{i}") for i in range(3)]
        results = await engine.resolve_all(refs)
        assert all(not r.resolved for r in results)
