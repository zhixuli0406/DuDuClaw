"""Tests for NamespaceInjectionMiddleware.

Acceptance criteria (from mcp-memory-endpoints-design.md §6.1):
  ✅ external/{client_id} can access own memories
  ✅ internal/ namespace → forbidden (404)
  ✅ Caller-supplied namespace is ignored (server forces its own)
  ✅ Different client IDs cannot access each other's namespace
"""

from __future__ import annotations

import pytest

from duduclaw.mcp.auth.types import APIKeyContext
from duduclaw.mcp.tools.memory.namespace import NamespaceInjectionMiddleware


# ── Fixtures ──────────────────────────────────────────────────────────────────


@pytest.fixture
def ctx() -> APIKeyContext:
    return APIKeyContext(
        api_key_prefix="ddc_prod",
        client_id="a3f2c1e4b5d6",
        scopes=frozenset({"memory:read", "memory:write"}),
        trust_level=1,
    )


@pytest.fixture
def middleware() -> NamespaceInjectionMiddleware:
    return NamespaceInjectionMiddleware()


# ── Namespace injection ────────────────────────────────────────────────────────


class TestNamespaceInjection:
    def test_inject_adds_correct_namespace(
        self, middleware: NamespaceInjectionMiddleware, ctx: APIKeyContext
    ) -> None:
        result = middleware.inject({"query": "test"}, ctx)
        assert result["namespace"] == "external/a3f2c1e4b5d6"

    def test_inject_overrides_caller_namespace(
        self, middleware: NamespaceInjectionMiddleware, ctx: APIKeyContext
    ) -> None:
        """Caller-supplied namespace is silently overridden — security requirement."""
        result = middleware.inject(
            {"query": "test", "namespace": "internal/evil-override"}, ctx
        )
        assert result["namespace"] == "external/a3f2c1e4b5d6"

    def test_inject_overrides_external_other_client_namespace(
        self, middleware: NamespaceInjectionMiddleware, ctx: APIKeyContext
    ) -> None:
        """Cannot inject another client's external namespace either."""
        result = middleware.inject(
            {"query": "test", "namespace": "external/other-client"}, ctx
        )
        assert result["namespace"] == "external/a3f2c1e4b5d6"

    def test_inject_preserves_all_other_params(
        self, middleware: NamespaceInjectionMiddleware, ctx: APIKeyContext
    ) -> None:
        params = {"query": "test", "limit": 5, "layer": "episodic", "extra": True}
        result = middleware.inject(params, ctx)
        assert result["query"] == "test"
        assert result["limit"] == 5
        assert result["layer"] == "episodic"
        assert result["extra"] is True

    def test_inject_does_not_mutate_original_params(
        self, middleware: NamespaceInjectionMiddleware, ctx: APIKeyContext
    ) -> None:
        """Immutability: original params dict must not be modified."""
        params: dict = {"query": "test"}
        middleware.inject(params, ctx)
        assert "namespace" not in params

    def test_inject_empty_params(
        self, middleware: NamespaceInjectionMiddleware, ctx: APIKeyContext
    ) -> None:
        """Even an empty dict gets the namespace injected."""
        result = middleware.inject({}, ctx)
        assert result == {"namespace": "external/a3f2c1e4b5d6"}


# ── Namespace access validation ────────────────────────────────────────────────


class TestNamespaceAccessValidation:
    def test_own_external_namespace_is_allowed(
        self, middleware: NamespaceInjectionMiddleware, ctx: APIKeyContext
    ) -> None:
        assert middleware.validate_namespace_access("external/a3f2c1e4b5d6", ctx) is True

    def test_internal_namespace_is_forbidden(
        self, middleware: NamespaceInjectionMiddleware, ctx: APIKeyContext
    ) -> None:
        assert (
            middleware.validate_namespace_access("internal/duduclaw-tl", ctx) is False
        )

    def test_internal_root_is_forbidden(
        self, middleware: NamespaceInjectionMiddleware, ctx: APIKeyContext
    ) -> None:
        assert middleware.validate_namespace_access("internal/", ctx) is False

    def test_other_client_external_namespace_is_forbidden(
        self, middleware: NamespaceInjectionMiddleware, ctx: APIKeyContext
    ) -> None:
        assert (
            middleware.validate_namespace_access("external/other-client-789", ctx)
            is False
        )

    def test_empty_namespace_is_forbidden(
        self, middleware: NamespaceInjectionMiddleware, ctx: APIKeyContext
    ) -> None:
        assert middleware.validate_namespace_access("", ctx) is False

    def test_partial_match_is_forbidden(
        self, middleware: NamespaceInjectionMiddleware, ctx: APIKeyContext
    ) -> None:
        """Substring of client_id must not match (exact match required)."""
        assert (
            middleware.validate_namespace_access("external/a3f2c1e4b5d", ctx) is False
        )

    def test_different_clients_cannot_cross_access(self) -> None:
        middleware = NamespaceInjectionMiddleware()
        ctx_a = APIKeyContext(
            api_key_prefix="ddc_prod",
            client_id="client_aaa",
            scopes=frozenset({"memory:read"}),
            trust_level=1,
        )
        ctx_b = APIKeyContext(
            api_key_prefix="ddc_prod",
            client_id="client_bbb",
            scopes=frozenset({"memory:read"}),
            trust_level=1,
        )
        # ctx_a cannot access ctx_b's namespace
        assert middleware.validate_namespace_access("external/client_bbb", ctx_a) is False
        # ctx_b cannot access ctx_a's namespace
        assert middleware.validate_namespace_access("external/client_aaa", ctx_b) is False
        # Each can access their own
        assert middleware.validate_namespace_access("external/client_aaa", ctx_a) is True
        assert middleware.validate_namespace_access("external/client_bbb", ctx_b) is True
