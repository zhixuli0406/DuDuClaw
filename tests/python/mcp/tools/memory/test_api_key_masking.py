"""Tests for API Key masking utilities.

Acceptance criteria (from mcp-memory-endpoints-design.md §6.6):
  ✅ Log output must not contain full API Key
  ✅ Error tracebacks must not leak API Key
  ✅ mask_api_key() replaces all matching patterns

Security requirement: ALL API Keys of format ddc_<env>_<32hex> must be masked.
"""

from __future__ import annotations

import logging

import pytest

from duduclaw.mcp.logging_utils import APIKeyMaskingFilter, mask_api_key

# ── Test data ─────────────────────────────────────────────────────────────────

PROD_KEY = "ddc_prod_a3f2c1e4b5d6e7f8a3f2c1e4b5d6e7f8"
DEV_KEY = "ddc_dev_00112233445566778899aabbccddeeff"
STAGING_KEY = "ddc_staging_ffffffffffffffffffffffffffffffff"
REDACTED = "ddc_***_[REDACTED]"


# ── mask_api_key() ────────────────────────────────────────────────────────────


class TestMaskApiKey:
    def test_masks_prod_key(self) -> None:
        result = mask_api_key(f"Authorization: {PROD_KEY}")
        assert PROD_KEY not in result
        assert REDACTED in result

    def test_masks_dev_key(self) -> None:
        result = mask_api_key(DEV_KEY)
        assert DEV_KEY not in result
        assert REDACTED in result

    def test_masks_staging_key(self) -> None:
        result = mask_api_key(f"Using key: {STAGING_KEY}")
        assert STAGING_KEY not in result
        assert REDACTED in result

    def test_masks_uppercase_key(self) -> None:
        upper_key = PROD_KEY.upper()
        result = mask_api_key(upper_key)
        assert upper_key not in result
        assert REDACTED in result

    def test_masks_multiple_keys_in_one_string(self) -> None:
        text = f"key1={PROD_KEY}, key2={DEV_KEY}"
        result = mask_api_key(text)
        assert PROD_KEY not in result
        assert DEV_KEY not in result
        assert result.count(REDACTED) == 2

    def test_no_key_string_unchanged(self) -> None:
        text = "Normal log message without any sensitive data"
        assert mask_api_key(text) == text

    def test_empty_string_unchanged(self) -> None:
        assert mask_api_key("") == ""

    def test_partial_key_not_masked_prefix_only(self) -> None:
        """Only the prefix 'ddc_prod' — should not be masked."""
        text = "prefix: ddc_prod"
        assert mask_api_key(text) == text

    def test_partial_key_not_masked_short_hex(self) -> None:
        """Short hex suffix (< 32 chars) — should not be masked."""
        text = "ddc_prod_a3f2c1e4b5d6"  # only 12 hex chars
        assert mask_api_key(text) == text

    def test_key_in_json_masked(self) -> None:
        json_str = f'{{"api_key": "{PROD_KEY}", "user": "test"}}'
        result = mask_api_key(json_str)
        assert PROD_KEY not in result
        assert REDACTED in result
        assert '"user": "test"' in result  # non-key data preserved

    def test_key_at_start_of_string(self) -> None:
        result = mask_api_key(PROD_KEY + " is my key")
        assert PROD_KEY not in result

    def test_key_at_end_of_string(self) -> None:
        result = mask_api_key("My key is " + PROD_KEY)
        assert PROD_KEY not in result


# ── APIKeyMaskingFilter ───────────────────────────────────────────────────────


class TestAPIKeyMaskingFilter:
    def _make_record(self, msg: str, args=()) -> logging.LogRecord:
        record = logging.LogRecord(
            name="duduclaw.mcp.test",
            level=logging.INFO,
            pathname="",
            lineno=0,
            msg=msg,
            args=args,
            exc_info=None,
        )
        return record

    def test_filter_returns_true(self) -> None:
        """Filter must return True — it sanitises, not suppresses."""
        record = self._make_record("normal message")
        f = APIKeyMaskingFilter()
        assert f.filter(record) is True

    def test_filter_masks_msg(self) -> None:
        record = self._make_record(f"API key is {PROD_KEY}")
        f = APIKeyMaskingFilter()
        f.filter(record)
        assert PROD_KEY not in record.msg
        assert REDACTED in record.msg

    def test_filter_non_string_msg_unchanged(self) -> None:
        record = self._make_record(42)  # type: ignore[arg-type]
        f = APIKeyMaskingFilter()
        f.filter(record)
        assert record.msg == 42  # untouched

    def test_filter_masks_tuple_args(self) -> None:
        record = self._make_record("key: %s", args=(PROD_KEY,))
        f = APIKeyMaskingFilter()
        f.filter(record)
        assert PROD_KEY not in record.args
        assert REDACTED in record.args

    def test_filter_preserves_non_string_tuple_args(self) -> None:
        record = self._make_record("values: %s %s", args=(42, 3.14))
        f = APIKeyMaskingFilter()
        f.filter(record)
        assert record.args == (42, 3.14)

    def test_filter_masks_dict_args(self) -> None:
        # Create record then set args manually to avoid Python 3.12 init quirk
        record = self._make_record("%(key)s")
        record.args = {"key": PROD_KEY}  # type: ignore[assignment]
        f = APIKeyMaskingFilter()
        f.filter(record)
        assert PROD_KEY not in record.args["key"]
        assert REDACTED in record.args["key"]

    def test_filter_preserves_non_string_dict_args(self) -> None:
        # Create record then set args manually to avoid Python 3.12 init quirk
        record = self._make_record("%(n)s")
        record.args = {"n": 42}  # type: ignore[assignment]
        f = APIKeyMaskingFilter()
        f.filter(record)
        assert record.args["n"] == 42

    def test_filter_handles_none_args(self) -> None:
        record = self._make_record("no args")
        record.args = None  # type: ignore[assignment]
        f = APIKeyMaskingFilter()
        f.filter(record)  # must not raise

    def test_installed_on_logger(self) -> None:
        """End-to-end: log record with API key must not expose it in formatted output."""
        handler = logging.handlers_buffer = []

        class BufferHandler(logging.Handler):
            def emit(self, record: logging.LogRecord) -> None:
                handler.append(self.format(record))

        buf_handler = BufferHandler()
        logger = logging.getLogger("duduclaw.mcp.masking_test")
        logger.setLevel(logging.DEBUG)
        logger.addFilter(APIKeyMaskingFilter())
        logger.addHandler(buf_handler)

        logger.info("Processing request with key: %s", PROD_KEY)
        logger.removeHandler(buf_handler)

        assert len(handler) == 1
        assert PROD_KEY not in handler[0]
        assert REDACTED in handler[0]
