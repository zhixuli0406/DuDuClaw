"""Memory tool handlers — search, store, read.

Public API::

    from duduclaw.mcp.tools.memory import MemorySearchTool, MemoryStoreTool, MemoryReadTool
    from duduclaw.mcp.tools.memory.namespace import NamespaceInjectionMiddleware
    from duduclaw.mcp.tools.memory.quota import QuotaEnforcer

Each tool handler is constructed with its dependencies injected:

    ns = NamespaceInjectionMiddleware()
    quota = QuotaEnforcer(default_limit=1000)

    search_tool = MemorySearchTool(memory_search_fn=backend.search, namespace_middleware=ns)
    store_tool  = MemoryStoreTool(memory_store_fn=backend.store,   namespace_middleware=ns, quota_enforcer=quota)
    read_tool   = MemoryReadTool( memory_read_fn=backend.read,     namespace_middleware=ns)
"""

from .read import MemoryReadTool
from .search import MemorySearchTool
from .store import MemoryStoreTool

__all__ = ["MemorySearchTool", "MemoryStoreTool", "MemoryReadTool"]
