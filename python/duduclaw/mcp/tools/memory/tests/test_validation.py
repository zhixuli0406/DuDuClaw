"""Unit tests for memory validation (HC18 regression).

Memory content and search queries must be stored/searched as plain text — the
validation layer must NOT HTML-escape natural-language content, otherwise text
like ``if x < 3 && y > 1`` is permanently corrupted.
"""
from __future__ import annotations

import pytest

from duduclaw.mcp.errors import ValidationError
from duduclaw.mcp.tools.memory.validation import (
    validate_memory_search_params,
    validate_memory_store_params,
)


# ── HC18: no HTML-escaping of natural-language content ─────────────────────────


def test_store_content_is_not_html_escaped():
    """Special characters in memory content must survive verbatim."""
    raw = "if x < 3 && y > 1 then \"ok\" else 'no'"
    out = validate_memory_store_params({"content": raw})
    assert out["content"] == raw


def test_search_query_is_not_html_escaped():
    """Search query must not be escaped (escaping hurts recall)."""
    raw = "x < 3 && y > 1"
    out = validate_memory_search_params({"query": raw})
    assert out["query"] == raw


def test_store_tags_are_not_html_escaped():
    """Tags are plain text too — no escaping."""
    out = validate_memory_store_params({"content": "hi", "tags": ["a&b", "c<d"]})
    assert out["tags"] == ["a&b", "c<d"]


def test_store_preserves_cjk_and_ampersand():
    raw = "用戶偏好 A&B 方案，閾值 < 5"
    out = validate_memory_store_params({"content": raw})
    assert out["content"] == raw


# ── Validation that must remain intact ─────────────────────────────────────────


def test_store_rejects_empty_content():
    with pytest.raises(ValidationError):
        validate_memory_store_params({"content": "   "})


def test_store_rejects_oversized_content():
    with pytest.raises(ValidationError):
        validate_memory_store_params({"content": "x" * 5000})


def test_search_rejects_empty_query():
    with pytest.raises(ValidationError):
        validate_memory_search_params({"query": ""})


def test_store_rejects_invalid_layer():
    with pytest.raises(ValidationError):
        validate_memory_store_params({"content": "hi", "layer": "all"})
