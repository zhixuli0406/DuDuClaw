"""MCP Server error types for DuDuClaw memory endpoints.

All errors carry an HTTP status code for transport-layer mapping and a
machine-readable ``code`` string for client-side error handling.

Error hierarchy:
  MCPError                    (base)
  ├── ValidationError         422 — invalid input parameters
  ├── AuthError               401 — missing or invalid API Key
  ├── ForbiddenError          403 — valid key but insufficient scope
  ├── NotFoundError           404 — resource not found (also used for namespace violations)
  ├── QuotaExceededError      429 — daily write quota exceeded
  └── InternalError           500 — unexpected backend error

Design note on NotFoundError:
  Cross-namespace access and internal/ namespace access BOTH return 404 (not 403).
  This avoids leaking information about the existence of resources in other
  namespaces.  See §4.2 of mcp-memory-endpoints-design.md.
"""

from __future__ import annotations

from typing import Any, Optional


class MCPError(Exception):
    """Base class for all MCP Server errors.

    Attributes:
        code:        Machine-readable error code string.
        message:     Human-readable error message.
        http_status: Corresponding HTTP status code.
    """

    def __init__(self, code: str, message: str, http_status: int = 400) -> None:
        super().__init__(message)
        self.code = code
        self.message = message
        self.http_status = http_status

    def to_dict(self) -> dict[str, Any]:
        """Serialise to MCP error response format."""
        return {"error": {"code": self.code, "message": self.message}}


class ValidationError(MCPError):
    """Raised when input parameters fail validation (HTTP 422)."""

    def __init__(self, message: str) -> None:
        super().__init__(code="validation_error", message=message, http_status=422)


class AuthError(MCPError):
    """Raised when the API Key is missing or malformed (HTTP 401)."""

    def __init__(self, message: str = "Authentication required") -> None:
        super().__init__(code="auth_error", message=message, http_status=401)


class ForbiddenError(MCPError):
    """Raised when the API Key is valid but lacks a required scope (HTTP 403)."""

    def __init__(self, message: str = "Insufficient scope") -> None:
        super().__init__(code="forbidden", message=message, http_status=403)


class NotFoundError(MCPError):
    """Raised when a resource is not found or is in a forbidden namespace (HTTP 404).

    Both "not found" and "cross-namespace access" return this error to avoid
    leaking information about resources in other namespaces.
    """

    def __init__(self, message: str = "Resource not found") -> None:
        super().__init__(code="not_found", message=message, http_status=404)


class QuotaExceededError(MCPError):
    """Raised when the daily write quota is exceeded (HTTP 429).

    Attributes:
        quota_limit:   Maximum allowed writes per day.
        quota_used:    Writes performed today.
        retry_after:   Seconds until quota resets.
        reset_at:      ISO 8601 UTC timestamp of next quota reset.
    """

    def __init__(
        self,
        message: str,
        quota_limit: int,
        quota_used: int,
        retry_after: int,
        reset_at: str,
    ) -> None:
        super().__init__(code="quota_exceeded", message=message, http_status=429)
        self.quota_limit = quota_limit
        self.quota_used = quota_used
        self.retry_after = retry_after
        self.reset_at = reset_at

    def to_dict(self) -> dict[str, Any]:
        """Serialise to MCP error response format with quota details."""
        return {
            "error": {
                "code": self.code,
                "message": self.message,
                "quota_limit": self.quota_limit,
                "quota_used": self.quota_used,
                "retry_after": self.retry_after,
                "reset_at": self.reset_at,
            }
        }


class InternalError(MCPError):
    """Raised for unexpected backend errors (HTTP 500)."""

    def __init__(self, message: str = "Internal server error") -> None:
        super().__init__(code="internal_error", message=message, http_status=500)
