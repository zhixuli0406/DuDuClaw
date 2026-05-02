"""Input validation for MCP Server memory endpoints.

All user-supplied parameters are validated at the system boundary before
entering any business logic.  Invalid inputs raise :exc:`ValidationError`
immediately (fail-fast principle).

Validation rules per endpoint:
  memory_search:
    - query: required, non-empty, ≤ 500 chars, HTML-escaped
    - limit: optional int, 1–50, default 10
    - layer: optional enum {"episodic", "semantic", "all"}, default "all"
    - min_relevance: optional float 0.0–1.0, default 0.5

  memory_store:
    - content: required, non-empty, ≤ 4096 chars, HTML-escaped
    - layer: optional enum {"episodic", "semantic"}, default "episodic"
              NOTE: "all" is valid for search but NOT for store
    - tags: optional list[str], ≤ 10 items, each ≤ 50 chars
    - ttl_days: optional int, 1–365 (None = permanent)

  memory_read:
    - memory_id: required, UUID v4 format

References: mcp-memory-endpoints-design.md §3 and §4.3
"""

from __future__ import annotations

import html
import re
import uuid
from typing import Any, Optional

from ...errors import ValidationError

# ── Limits (from spec) ────────────────────────────────────────────────────────

MAX_QUERY_LENGTH: int = 500
MAX_CONTENT_LENGTH: int = 4096
MAX_TAG_LENGTH: int = 50
MAX_TAGS_COUNT: int = 10
MAX_SEARCH_LIMIT: int = 50
MIN_SEARCH_LIMIT: int = 1
MIN_TTL_DAYS: int = 1
MAX_TTL_DAYS: int = 365

# Valid memory layer values
VALID_SEARCH_LAYERS: frozenset[str] = frozenset({"episodic", "semantic", "all"})
VALID_STORE_LAYERS: frozenset[str] = frozenset({"episodic", "semantic"})

# UUID v4 pattern: xxxxxxxx-xxxx-4xxx-[89ab]xxx-xxxxxxxxxxxx
_UUID_V4_PATTERN = re.compile(
    r"^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$",
    re.IGNORECASE,
)


# ── Sanitisation helpers ──────────────────────────────────────────────────────


def _sanitize(text: str) -> str:
    """HTML-escape special characters to prevent injection attacks.

    Escapes: ``<``, ``>``, ``"``, ``'``, ``&``.
    """
    return html.escape(text, quote=True)


# ── Validators ────────────────────────────────────────────────────────────────


def validate_memory_search_params(params: dict[str, Any]) -> dict[str, Any]:
    """Validate and sanitise ``memory_search`` parameters.

    Args:
        params: Raw parameters dict from the MCP caller.

    Returns:
        Sanitised and normalised parameters dict.

    Raises:
        :exc:`ValidationError`: On any invalid or out-of-range parameter.
    """
    # ── query ──────────────────────────────────────────────────────────────────
    query = params.get("query")
    if not isinstance(query, str) or not query.strip():
        raise ValidationError("'query' is required and must be a non-empty string")
    if len(query) > MAX_QUERY_LENGTH:
        raise ValidationError(
            f"'query' must not exceed {MAX_QUERY_LENGTH} characters "
            f"(got {len(query)})"
        )

    # ── limit ──────────────────────────────────────────────────────────────────
    limit = params.get("limit", 10)
    if isinstance(limit, bool) or not isinstance(limit, int):
        raise ValidationError("'limit' must be an integer")
    if not (MIN_SEARCH_LIMIT <= limit <= MAX_SEARCH_LIMIT):
        raise ValidationError(
            f"'limit' must be between {MIN_SEARCH_LIMIT} and {MAX_SEARCH_LIMIT} "
            f"(got {limit})"
        )

    # ── layer ──────────────────────────────────────────────────────────────────
    layer = params.get("layer", "all")
    if layer not in VALID_SEARCH_LAYERS:
        raise ValidationError(
            f"'layer' must be one of {sorted(VALID_SEARCH_LAYERS)} (got {layer!r})"
        )

    # ── min_relevance ──────────────────────────────────────────────────────────
    min_relevance = params.get("min_relevance", 0.5)
    if isinstance(min_relevance, bool) or not isinstance(min_relevance, (int, float)):
        raise ValidationError("'min_relevance' must be a number")
    min_relevance_f = float(min_relevance)
    if not (0.0 <= min_relevance_f <= 1.0):
        raise ValidationError(
            f"'min_relevance' must be between 0.0 and 1.0 (got {min_relevance_f})"
        )

    return {
        "query": _sanitize(query),
        "limit": limit,
        "layer": layer,
        "min_relevance": min_relevance_f,
    }


def validate_memory_store_params(params: dict[str, Any]) -> dict[str, Any]:
    """Validate and sanitise ``memory_store`` parameters.

    Args:
        params: Raw parameters dict from the MCP caller.

    Returns:
        Sanitised and normalised parameters dict.

    Raises:
        :exc:`ValidationError`: On any invalid or out-of-range parameter.
    """
    # ── content ────────────────────────────────────────────────────────────────
    content = params.get("content")
    if not isinstance(content, str) or not content.strip():
        raise ValidationError("'content' is required and must be a non-empty string")
    if len(content) > MAX_CONTENT_LENGTH:
        raise ValidationError(
            f"'content' must not exceed {MAX_CONTENT_LENGTH} characters "
            f"(got {len(content)})"
        )

    # ── layer ──────────────────────────────────────────────────────────────────
    layer = params.get("layer", "episodic")
    if layer not in VALID_STORE_LAYERS:
        raise ValidationError(
            f"'layer' must be one of {sorted(VALID_STORE_LAYERS)} for store "
            f"(got {layer!r}). Note: 'all' is only valid for search."
        )

    # ── tags ───────────────────────────────────────────────────────────────────
    tags = params.get("tags", [])
    if not isinstance(tags, list):
        raise ValidationError("'tags' must be an array")
    if len(tags) > MAX_TAGS_COUNT:
        raise ValidationError(
            f"'tags' must have at most {MAX_TAGS_COUNT} items (got {len(tags)})"
        )
    validated_tags: list[str] = []
    for i, tag in enumerate(tags):
        if not isinstance(tag, str):
            raise ValidationError(f"tags[{i}] must be a string (got {type(tag).__name__!r})")
        if len(tag) > MAX_TAG_LENGTH:
            raise ValidationError(
                f"tags[{i}] must not exceed {MAX_TAG_LENGTH} characters "
                f"(got {len(tag)})"
            )
        validated_tags.append(tag)

    # ── ttl_days ───────────────────────────────────────────────────────────────
    ttl_days: Optional[int] = params.get("ttl_days")
    if ttl_days is not None:
        if isinstance(ttl_days, bool) or not isinstance(ttl_days, int):
            raise ValidationError("'ttl_days' must be an integer")
        if not (MIN_TTL_DAYS <= ttl_days <= MAX_TTL_DAYS):
            raise ValidationError(
                f"'ttl_days' must be between {MIN_TTL_DAYS} and {MAX_TTL_DAYS} "
                f"(got {ttl_days})"
            )

    return {
        "content": _sanitize(content),
        "layer": layer,
        "tags": validated_tags,
        "ttl_days": ttl_days,
    }


def validate_memory_read_params(params: dict[str, Any]) -> dict[str, Any]:
    """Validate ``memory_read`` parameters.

    Args:
        params: Raw parameters dict from the MCP caller.

    Returns:
        Validated parameters dict with ``memory_id`` normalised to lowercase.

    Raises:
        :exc:`ValidationError`: If ``memory_id`` is missing or not a valid UUID v4.
    """
    memory_id = params.get("memory_id")
    if not isinstance(memory_id, str) or not memory_id.strip():
        raise ValidationError(
            "'memory_id' is required and must be a non-empty string"
        )

    # Check format with regex first (fast)
    if not _UUID_V4_PATTERN.match(memory_id.strip()):
        raise ValidationError(
            f"'memory_id' must be a valid UUID v4 "
            f"(e.g. '550e8400-e29b-41d4-a716-446655440000'), got {memory_id!r}"
        )

    # Double-check using stdlib uuid (strict version validation)
    try:
        parsed = uuid.UUID(memory_id.strip(), version=4)
        # uuid.UUID normalises non-canonical forms; verify round-trip
        normalised = str(parsed)
    except ValueError:
        raise ValidationError(
            f"'memory_id' must be a valid UUID v4, got {memory_id!r}"
        )

    return {"memory_id": normalised}
