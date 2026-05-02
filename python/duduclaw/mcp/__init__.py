"""DuDuClaw MCP Server — stdio transport + memory endpoints.

Phase 1 (W19-P0):
  - memory/search  → duduclaw/memory_search tool
  - memory/store   → duduclaw/memory_store tool
  - memory/read    → duduclaw/memory_read tool

Transport: stdio (local Claude Desktop / Claude Code integration)
Auth: API Key + Scope Strategy Pattern (eng-infra M1)
Namespace: external/{client_id} isolation (TL decision)
"""
