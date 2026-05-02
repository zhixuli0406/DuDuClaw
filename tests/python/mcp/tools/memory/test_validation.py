"""Tests for memory endpoint input validation.

Acceptance criteria (from mcp-memory-endpoints-design.md §6.3–6.5):
  memory_search:
    ✅ Valid query returns normalised params
    ✅ Missing/empty query → 422
    ✅ query > 500 chars → 422
    ✅ limit > 50 → 422
    ✅ Invalid layer → 422
    ✅ XSS chars in query → HTML-escaped

  memory_store:
    ✅ Missing/empty content → 422
    ✅ content > 4096 chars → 422
    ✅ layer="all" → 422 (only episodic/semantic for store)
    ✅ tags > 10 items → 422
    ✅ ttl_days=0 → 422

  memory_read:
    ✅ Valid UUID v4 → passes
    ✅ Missing memory_id → 422
    ✅ Non-UUID string → 422
    ✅ UUID v1 → 422 (only v4 accepted)
"""

from __future__ import annotations

import pytest

from duduclaw.mcp.errors import ValidationError
from duduclaw.mcp.tools.memory.validation import (
    MAX_CONTENT_LENGTH,
    MAX_QUERY_LENGTH,
    MAX_SEARCH_LIMIT,
    MAX_TAGS_COUNT,
    MAX_TAG_LENGTH,
    MAX_TTL_DAYS,
    MIN_TTL_DAYS,
    validate_memory_read_params,
    validate_memory_search_params,
    validate_memory_store_params,
)

_VALID_UUID_V4 = "550e8400-e29b-41d4-a716-446655440000"
_UUID_V1 = "6ba7b810-9dad-11d1-80b4-00c04fd430c8"  # version=1


# ── memory_search ─────────────────────────────────────────────────────────────


class TestSearchValidation:
    def test_valid_minimal_params(self) -> None:
        result = validate_memory_search_params({"query": "hello"})
        assert result["query"] == "hello"
        assert result["limit"] == 10       # default
        assert result["layer"] == "all"    # default
        assert result["min_relevance"] == 0.5  # default

    def test_valid_all_params(self) -> None:
        result = validate_memory_search_params(
            {"query": "hello", "limit": 25, "layer": "semantic", "min_relevance": 0.8}
        )
        assert result["limit"] == 25
        assert result["layer"] == "semantic"
        assert result["min_relevance"] == 0.8

    # ── query ──────────────────────────────────────────────────────────────────

    def test_query_required(self) -> None:
        with pytest.raises(ValidationError, match="query"):
            validate_memory_search_params({})

    def test_query_whitespace_only_rejected(self) -> None:
        with pytest.raises(ValidationError, match="query"):
            validate_memory_search_params({"query": "   "})

    def test_query_empty_string_rejected(self) -> None:
        with pytest.raises(ValidationError, match="query"):
            validate_memory_search_params({"query": ""})

    def test_query_at_max_length_accepted(self) -> None:
        result = validate_memory_search_params({"query": "q" * MAX_QUERY_LENGTH})
        assert len(result["query"]) <= MAX_QUERY_LENGTH * 3  # allow for HTML escaping

    def test_query_over_max_length_rejected(self) -> None:
        with pytest.raises(ValidationError, match="query"):
            validate_memory_search_params({"query": "q" * (MAX_QUERY_LENGTH + 1)})

    def test_query_non_string_rejected(self) -> None:
        with pytest.raises(ValidationError):
            validate_memory_search_params({"query": 123})

    def test_xss_chars_are_html_escaped(self) -> None:
        result = validate_memory_search_params(
            {"query": '<script>alert("xss")</script>'}
        )
        assert "<script>" not in result["query"]
        assert ">" not in result["query"]
        assert "script" in result["query"]  # content preserved, just escaped

    # ── limit ──────────────────────────────────────────────────────────────────

    def test_limit_default_is_10(self) -> None:
        result = validate_memory_search_params({"query": "test"})
        assert result["limit"] == 10

    def test_limit_max_accepted(self) -> None:
        result = validate_memory_search_params({"query": "test", "limit": MAX_SEARCH_LIMIT})
        assert result["limit"] == MAX_SEARCH_LIMIT

    def test_limit_over_max_rejected(self) -> None:
        with pytest.raises(ValidationError, match="limit"):
            validate_memory_search_params({"query": "test", "limit": MAX_SEARCH_LIMIT + 1})

    def test_limit_zero_rejected(self) -> None:
        with pytest.raises(ValidationError, match="limit"):
            validate_memory_search_params({"query": "test", "limit": 0})

    def test_limit_negative_rejected(self) -> None:
        with pytest.raises(ValidationError, match="limit"):
            validate_memory_search_params({"query": "test", "limit": -1})

    def test_limit_boolean_rejected(self) -> None:
        """True/False must not be accepted as integers."""
        with pytest.raises(ValidationError, match="limit"):
            validate_memory_search_params({"query": "test", "limit": True})

    # ── layer ──────────────────────────────────────────────────────────────────

    def test_layer_episodic_accepted(self) -> None:
        result = validate_memory_search_params({"query": "test", "layer": "episodic"})
        assert result["layer"] == "episodic"

    def test_layer_semantic_accepted(self) -> None:
        result = validate_memory_search_params({"query": "test", "layer": "semantic"})
        assert result["layer"] == "semantic"

    def test_layer_all_accepted(self) -> None:
        result = validate_memory_search_params({"query": "test", "layer": "all"})
        assert result["layer"] == "all"

    def test_invalid_layer_rejected(self) -> None:
        with pytest.raises(ValidationError, match="layer"):
            validate_memory_search_params({"query": "test", "layer": "invalid"})

    # ── min_relevance ──────────────────────────────────────────────────────────

    def test_min_relevance_float(self) -> None:
        result = validate_memory_search_params({"query": "test", "min_relevance": 0.75})
        assert result["min_relevance"] == 0.75

    def test_min_relevance_zero_accepted(self) -> None:
        result = validate_memory_search_params({"query": "test", "min_relevance": 0.0})
        assert result["min_relevance"] == 0.0

    def test_min_relevance_one_accepted(self) -> None:
        result = validate_memory_search_params({"query": "test", "min_relevance": 1.0})
        assert result["min_relevance"] == 1.0

    def test_min_relevance_over_one_rejected(self) -> None:
        with pytest.raises(ValidationError, match="min_relevance"):
            validate_memory_search_params({"query": "test", "min_relevance": 1.1})

    def test_min_relevance_negative_rejected(self) -> None:
        with pytest.raises(ValidationError, match="min_relevance"):
            validate_memory_search_params({"query": "test", "min_relevance": -0.1})


# ── memory_store ──────────────────────────────────────────────────────────────


class TestStoreValidation:
    def test_valid_minimal_params(self) -> None:
        result = validate_memory_store_params({"content": "hello"})
        assert result["content"] == "hello"
        assert result["layer"] == "episodic"   # default
        assert result["tags"] == []
        assert result["ttl_days"] is None

    # ── content ────────────────────────────────────────────────────────────────

    def test_content_required(self) -> None:
        with pytest.raises(ValidationError, match="content"):
            validate_memory_store_params({})

    def test_content_whitespace_only_rejected(self) -> None:
        with pytest.raises(ValidationError, match="content"):
            validate_memory_store_params({"content": "   "})

    def test_content_at_max_length_accepted(self) -> None:
        validate_memory_store_params({"content": "x" * MAX_CONTENT_LENGTH})

    def test_content_over_max_length_rejected(self) -> None:
        with pytest.raises(ValidationError, match="content"):
            validate_memory_store_params({"content": "x" * (MAX_CONTENT_LENGTH + 1)})

    def test_content_xss_escaped(self) -> None:
        result = validate_memory_store_params({"content": '<b>bold</b> & "quoted"'})
        assert "<b>" not in result["content"]

    # ── layer ──────────────────────────────────────────────────────────────────

    def test_layer_episodic_accepted(self) -> None:
        result = validate_memory_store_params({"content": "hi", "layer": "episodic"})
        assert result["layer"] == "episodic"

    def test_layer_semantic_accepted(self) -> None:
        result = validate_memory_store_params({"content": "hi", "layer": "semantic"})
        assert result["layer"] == "semantic"

    def test_layer_all_rejected_for_store(self) -> None:
        """'all' is valid for search but NOT for store."""
        with pytest.raises(ValidationError, match="layer"):
            validate_memory_store_params({"content": "hi", "layer": "all"})

    def test_invalid_layer_rejected(self) -> None:
        with pytest.raises(ValidationError, match="layer"):
            validate_memory_store_params({"content": "hi", "layer": "unknown"})

    # ── tags ───────────────────────────────────────────────────────────────────

    def test_tags_empty_list_accepted(self) -> None:
        result = validate_memory_store_params({"content": "hi", "tags": []})
        assert result["tags"] == []

    def test_tags_max_count_accepted(self) -> None:
        tags = [f"tag{i}" for i in range(MAX_TAGS_COUNT)]
        result = validate_memory_store_params({"content": "hi", "tags": tags})
        assert len(result["tags"]) == MAX_TAGS_COUNT

    def test_tags_over_max_count_rejected(self) -> None:
        tags = [f"tag{i}" for i in range(MAX_TAGS_COUNT + 1)]
        with pytest.raises(ValidationError, match="tags"):
            validate_memory_store_params({"content": "hi", "tags": tags})

    def test_tag_at_max_length_accepted(self) -> None:
        validate_memory_store_params(
            {"content": "hi", "tags": ["x" * MAX_TAG_LENGTH]}
        )

    def test_tag_over_max_length_rejected(self) -> None:
        with pytest.raises(ValidationError, match="tag"):
            validate_memory_store_params(
                {"content": "hi", "tags": ["x" * (MAX_TAG_LENGTH + 1)]}
            )

    def test_tags_not_list_rejected(self) -> None:
        with pytest.raises(ValidationError, match="tags"):
            validate_memory_store_params({"content": "hi", "tags": "not-a-list"})

    def test_tag_non_string_rejected(self) -> None:
        with pytest.raises(ValidationError):
            validate_memory_store_params({"content": "hi", "tags": [123]})

    # ── ttl_days ───────────────────────────────────────────────────────────────

    def test_ttl_days_none_accepted(self) -> None:
        result = validate_memory_store_params({"content": "hi", "ttl_days": None})
        assert result["ttl_days"] is None

    def test_ttl_days_min_accepted(self) -> None:
        result = validate_memory_store_params({"content": "hi", "ttl_days": MIN_TTL_DAYS})
        assert result["ttl_days"] == MIN_TTL_DAYS

    def test_ttl_days_max_accepted(self) -> None:
        result = validate_memory_store_params({"content": "hi", "ttl_days": MAX_TTL_DAYS})
        assert result["ttl_days"] == MAX_TTL_DAYS

    def test_ttl_days_zero_rejected(self) -> None:
        with pytest.raises(ValidationError, match="ttl_days"):
            validate_memory_store_params({"content": "hi", "ttl_days": 0})

    def test_ttl_days_over_max_rejected(self) -> None:
        with pytest.raises(ValidationError, match="ttl_days"):
            validate_memory_store_params({"content": "hi", "ttl_days": MAX_TTL_DAYS + 1})

    def test_ttl_days_boolean_rejected(self) -> None:
        with pytest.raises(ValidationError, match="ttl_days"):
            validate_memory_store_params({"content": "hi", "ttl_days": True})


# ── memory_read ───────────────────────────────────────────────────────────────


class TestReadValidation:
    def test_valid_uuid_v4_accepted(self) -> None:
        result = validate_memory_read_params({"memory_id": _VALID_UUID_V4})
        assert result["memory_id"] == _VALID_UUID_V4

    def test_uppercase_uuid_normalised_to_lowercase(self) -> None:
        result = validate_memory_read_params(
            {"memory_id": _VALID_UUID_V4.upper()}
        )
        assert result["memory_id"] == _VALID_UUID_V4

    def test_memory_id_required(self) -> None:
        with pytest.raises(ValidationError, match="memory_id"):
            validate_memory_read_params({})

    def test_empty_memory_id_rejected(self) -> None:
        with pytest.raises(ValidationError, match="memory_id"):
            validate_memory_read_params({"memory_id": ""})

    def test_non_uuid_string_rejected(self) -> None:
        with pytest.raises(ValidationError, match="UUID"):
            validate_memory_read_params({"memory_id": "not-a-uuid"})

    def test_uuid_v1_rejected(self) -> None:
        """Only UUID v4 is accepted."""
        with pytest.raises(ValidationError, match="UUID"):
            validate_memory_read_params({"memory_id": _UUID_V1})

    def test_partial_uuid_rejected(self) -> None:
        with pytest.raises(ValidationError):
            validate_memory_read_params({"memory_id": "550e8400-e29b-41d4"})

    def test_non_string_memory_id_rejected(self) -> None:
        with pytest.raises(ValidationError, match="memory_id"):
            validate_memory_read_params({"memory_id": 12345})
