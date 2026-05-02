"""API Key masking utilities for MCP Server logging.

Security hard requirement (TL Decision 2026-04-29):
  API Keys MUST be fully masked in all log output.
  No full API Key may appear in:
    - Log messages
    - Exception tracebacks
    - Access logs (Authorization header values)
    - Debug dumps

Usage::

    from duduclaw.mcp.logging_utils import APIKeyMaskingFilter, mask_api_key
    import logging

    logger = logging.getLogger(__name__)
    logger.addFilter(APIKeyMaskingFilter())

    # All log output through this logger will have keys replaced
    logger.info("Request auth: %s", auth_header)  # → "ddc_***_[REDACTED]"

API Key format matched: ``ddc_<env>_<random_32hex>``
  e.g. ddc_prod_a3f2c1e4b5d6e7f8a3f2c1e4b5d6e7f8
       ddc_dev_00112233445566778899aabbccddeeff
"""

from __future__ import annotations

import logging
import re

# Pattern matches: ddc_ + one-or-more lowercase letters + _ + exactly 32 hex chars
# Case-insensitive to catch any casing variant
_API_KEY_PATTERN = re.compile(r"ddc_[a-z]+_[0-9a-f]{32}", re.IGNORECASE)
_REDACTED = "ddc_***_[REDACTED]"


def mask_api_key(text: str) -> str:
    """Replace all API Key patterns in *text* with a redacted placeholder.

    The replacement ``ddc_***_[REDACTED]`` retains the prefix structure so
    log readers know a key was present, without exposing the actual value.

    Args:
        text: Any string that may contain an API Key.

    Returns:
        The string with all matching keys replaced by ``ddc_***_[REDACTED]``.

    Example::

        mask_api_key("Authorization: ddc_prod_a3f2c1e4b5d6e7f8a3f2c1e4b5d6e7f8")
        # → "Authorization: ddc_***_[REDACTED]"
    """
    return _API_KEY_PATTERN.sub(_REDACTED, text)


class APIKeyMaskingFilter(logging.Filter):
    """Logging filter that masks API Keys in all log records.

    Install on any logger that may receive API Key values:

    ::

        logger = logging.getLogger("duduclaw.mcp")
        logger.addFilter(APIKeyMaskingFilter())

    The filter modifies ``record.msg`` and ``record.args`` in-place (strings
    only; non-string args are passed through unchanged).  Always returns
    ``True`` so the record is always emitted — the filter only sanitises,
    it does not suppress.
    """

    def filter(self, record: logging.LogRecord) -> bool:
        # Mask the message string
        if isinstance(record.msg, str):
            record.msg = mask_api_key(record.msg)

        # Mask string arguments (used in %-style formatting)
        if record.args:
            if isinstance(record.args, tuple):
                record.args = tuple(
                    mask_api_key(arg) if isinstance(arg, str) else arg
                    for arg in record.args
                )
            elif isinstance(record.args, dict):
                record.args = {
                    k: mask_api_key(v) if isinstance(v, str) else v
                    for k, v in record.args.items()
                }

        return True  # always emit the (sanitised) record
