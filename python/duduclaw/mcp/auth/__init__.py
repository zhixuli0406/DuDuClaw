"""MCP Server auth module — API Key context types.

Defines the APIKeyContext interface that the auth middleware (eng-infra)
must produce. Memory tool handlers consume these types.
"""

from .types import APIKeyContext, derive_client_id

__all__ = ["APIKeyContext", "derive_client_id"]
