---
title: "DuDuClaw MCP Server 正式化規格草案 v0.1"
created: 2026-04-27T00:00:00Z
updated: 2026-04-27T00:00:00Z
status: draft
author: duduclaw-pm
tags: [w18-p0, spec, mcp, server, agnes-directive]
layer: deep
trust: 0.9
changelog:
  - version: v0.1
    date: 2026-04-27
    author: duduclaw-pm
    changes: "Agnes 指令（2026-04-27）：初稿 MCP Server 端點規格"
---

# DuDuClaw MCP Server 正式化規格草案 v0.1

> **審批狀態**：草案，待 TL/Agnes 審閱
> **優先級**：W18-P0
> **任務背景**：搭配 [W18-P0] MCP 支援可行性評估報告（wiki/reports/mcp-feasibility-2026-W18.md）

---

## 1. 背景與目的

### 1.1 為什麼要建立 DuDuClaw MCP Server？

MCP（Model Context Protocol）是 Anthropic 於 2024 年底發布的開放標準，已成為 AI Agent 工具互通的事實標準：

| 採用者 | MCP 整合狀態 |
|--------|------------|
| Claude Desktop / Claude Code | 原生支援（MCP Client） |
| Cursor | MCP Server 市場 |
| GitHub Copilot | 實驗性支援 |
| Zep | MCP Server（memory 端點） |
| n8n | MCP Node 整合 |

**核心價值主張**：若 DuDuClaw 暴露 MCP Server，任何 MCP Client（Claude、Cursor 等）無需自定義整合即可直接使用 DuDuClaw 的記憶、Wiki、Agent 通訊等能力。

### 1.2 DuDuClaw 的雙角色定位

```
┌──────────────────────────────────────────────────┐
│                 DuDuClaw 系統                     │
│                                                   │
│  ┌──────────────┐      ┌──────────────────────┐  │
│  │  MCP Server  │      │     MCP Client       │  │
│  │  （暴露工具）  │      │  （接入外部 MCP）     │  │
│  │              │      │                      │  │
│  │ memory_search│      │ Zep MCP Server       │  │
│  │ wiki_read    │◄────►│ n8n MCP Node         │  │
│  │ wiki_write   │      │ 外部資料庫 MCP        │  │
│  │ send_message │      │                      │  │
│  └──────────────┘      └──────────────────────┘  │
└──────────────────────────────────────────────────┘
```

**本文件聚焦：MCP Server 角色**（暴露 DuDuClaw 工具給外部 Client）

---

## 2. 暴露端點清單

### 2.1 優先級矩陣

| 優先級 | 工具名稱 | 分類 | 暴露理由 |
|--------|---------|------|---------|
| **P0** | `memory_search` | 記憶 | 核心差異化能力，允許外部 Agent 查詢 DuDuClaw 記憶庫 |
| **P0** | `wiki_read` | 知識庫 | 讓外部工具讀取共享知識庫 |
| **P0** | `wiki_write` | 知識庫 | 讓外部 Agent 貢獻知識（需嚴格授權） |
| **P0** | `send_message` | 通訊 | 讓外部 Client 向 DuDuClaw Agent 發送訊息 |
| **P1** | `memory_store` | 記憶 | 讓外部 Agent 寫入記憶（需授權隔離） |
| **P1** | `tasks_list` | 任務管理 | 讓外部 Client 查詢任務狀態 |
| **P1** | `tasks_create` | 任務管理 | 讓外部 Client 建立任務 |
| **P1** | `agent_status` | Agent 管理 | 查詢 Agent 健康狀態 |
| **P2** | `shared_wiki_read` | 知識庫 | 跨 Agent 共享知識庫讀取 |
| **P2** | `shared_wiki_write` | 知識庫 | 跨 Agent 共享知識庫寫入 |
| **P2** | `evolution_status` | 演化系統 | 查詢 Agent 演化狀態 |
| **P2** | `send_to_agent` | Agent 路由 | 直接向特定 Agent 發送任務 |

### 2.2 不暴露的工具（安全紅線）

以下工具**不應暴露**為 MCP Server 端點：

| 工具 | 不暴露理由 |
|------|----------|
| `agent_update_soul` | Soul 修改需最高授權，不允許外部調用 |
| `evolution_toggle` | 演化開關不允許外部控制 |
| `create_agent` / `agent_remove` | Agent 生命週期管理需內部控制 |
| `llamafile_start/stop` | 本地模型啟停需系統級權限 |
| `computer_*` 系列 | 計算機控制工具風險過高 |

---

## 3. 端點詳細規格（P0 端點）

### 3.1 `duduclaw/memory_search`

```json
{
  "name": "duduclaw/memory_search",
  "description": "Search DuDuClaw's episodic and semantic memory. Returns relevant memories ranked by relevance score.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "query": {
        "type": "string",
        "description": "Natural language search query"
      },
      "limit": {
        "type": "integer",
        "description": "Maximum results to return (default: 10, max: 50)",
        "default": 10
      },
      "memory_layer": {
        "type": "string",
        "enum": ["episodic", "semantic", "all"],
        "description": "Memory layer to search (default: all)",
        "default": "all"
      }
    },
    "required": ["query"]
  },
  "outputSchema": {
    "type": "object",
    "properties": {
      "memories": {
        "type": "array",
        "items": {
          "type": "object",
          "properties": {
            "content": {"type": "string"},
            "relevance_score": {"type": "number"},
            "created_at": {"type": "string", "format": "date-time"},
            "layer": {"type": "string"}
          }
        }
      },
      "total": {"type": "integer"}
    }
  }
}
```

**授權要求**：Read-only。任何具有 `memory:read` scope 的 Client 可調用。

---

### 3.2 `duduclaw/wiki_read`

```json
{
  "name": "duduclaw/wiki_read",
  "description": "Read a page from DuDuClaw's knowledge wiki. Supports both internal wiki and shared wiki.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "page_path": {
        "type": "string",
        "description": "Path relative to wiki root (e.g. 'specs/handoffpacket-spec-v0.2.md')"
      },
      "wiki_type": {
        "type": "string",
        "enum": ["internal", "shared"],
        "description": "Wiki namespace (default: shared)",
        "default": "shared"
      }
    },
    "required": ["page_path"]
  },
  "outputSchema": {
    "type": "object",
    "properties": {
      "content": {"type": "string"},
      "frontmatter": {"type": "object"},
      "last_updated": {"type": "string", "format": "date-time"}
    }
  }
}
```

**授權要求**：Read-only。`wiki:read` scope。

---

### 3.3 `duduclaw/wiki_write`

```json
{
  "name": "duduclaw/wiki_write",
  "description": "Create or update a page in DuDuClaw's shared knowledge wiki.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "page_path": {
        "type": "string",
        "description": "Path relative to shared wiki root"
      },
      "content": {
        "type": "string",
        "description": "Full page content including YAML frontmatter"
      }
    },
    "required": ["page_path", "content"]
  },
  "outputSchema": {
    "type": "object",
    "properties": {
      "success": {"type": "boolean"},
      "page_path": {"type": "string"},
      "version": {"type": "string"}
    }
  }
}
```

**授權要求**：Write。`wiki:write` scope。需額外 `trusted_writer` flag 才能寫入 `specs/` 和 `decisions/` 路徑。

---

### 3.4 `duduclaw/send_message`

```json
{
  "name": "duduclaw/send_message",
  "description": "Send a message to a DuDuClaw agent or channel. Used for agent-to-agent communication or notifications.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "recipient": {
        "type": "string",
        "description": "Target agent ID or channel name (e.g. 'duduclaw-tl', 'general')"
      },
      "message": {
        "type": "string",
        "description": "Message content (Markdown supported)"
      },
      "priority": {
        "type": "string",
        "enum": ["low", "normal", "high", "urgent"],
        "default": "normal"
      }
    },
    "required": ["recipient", "message"]
  },
  "outputSchema": {
    "type": "object",
    "properties": {
      "message_id": {"type": "string"},
      "delivered_at": {"type": "string", "format": "date-time"},
      "status": {"type": "string", "enum": ["delivered", "queued", "failed"]}
    }
  }
}
```

**授權要求**：`messaging:send` scope。外部 Client 僅可發送到 `public` 頻道和已明確授權的 Agent。

---

## 4. 授權與安全模型

### 4.1 OAuth 2.0 Scope 設計

```
mcp://duduclaw/
  ├── memory:read         → memory_search
  ├── memory:write        → memory_store
  ├── wiki:read           → wiki_read, shared_wiki_read
  ├── wiki:write          → wiki_write, shared_wiki_write
  ├── messaging:send      → send_message, send_to_agent
  ├── tasks:read          → tasks_list, task_status
  ├── tasks:write         → tasks_create
  ├── agents:read         → agent_status, list_agents
  └── evolution:read      → evolution_status
```

### 4.2 Client 信任等級

| 等級 | 範例 | 可用 Scope |
|------|------|-----------|
| **Level 0：匿名** | 未認證 | 無 |
| **Level 1：外部** | Claude Desktop | `memory:read`, `wiki:read` |
| **Level 2：受信任外部** | 合作 Agent | + `wiki:write`, `tasks:read` |
| **Level 3：內部 Agent** | DuDuClaw 自身 Agent | 全部 scope |

### 4.3 Rate Limiting

| 端點 | Level 1 限制 | Level 2 限制 |
|------|------------|------------|
| `memory_search` | 60 req/min | 300 req/min |
| `wiki_read` | 120 req/min | 600 req/min |
| `wiki_write` | 10 req/min | 60 req/min |
| `send_message` | 20 req/min | 100 req/min |

---

## 5. 技術架構

### 5.1 MCP Server 實作方案

```
┌─────────────────────────────────────────────────┐
│         DuDuClaw MCP Server Layer               │
│                                                  │
│  Transport: stdio (local) + HTTP/SSE (remote)   │
│                                                  │
│  ┌──────────────────────────────────────────┐   │
│  │           Tool Dispatcher               │   │
│  │  ┌──────────┐  ┌───────────┐            │   │
│  │  │ Auth     │  │ Rate      │            │   │
│  │  │ Middleware│  │ Limiter   │            │   │
│  │  └──────────┘  └───────────┘            │   │
│  └──────────────────────────────────────────┘   │
│                    │                             │
│  ┌─────────────────▼──────────────────────────┐ │
│  │         DuDuClaw Internal MCP Tools        │ │
│  │  memory_search  wiki_read  send_message    │ │
│  └────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────┘
```

### 5.2 Transport 選擇

| 模式 | 使用場景 | 優先級 |
|------|---------|--------|
| `stdio` | 本地 Claude Code / Claude Desktop 整合 | **W19 P0** |
| `HTTP + SSE` | 遠端 Client、n8n 整合 | **W20 P1** |
| `WebSocket` | 即時雙向通訊（未來） | **W21+ P2** |

---

## 6. 競品對比

### 6.1 Zep MCP 整合

- Zep 透過 MCP Server 暴露 `zep/search_memory`、`zep/add_memory`、`zep/get_user`
- 重點：以 **User Session** 為隔離單位，不同 User 的記憶完全隔離
- DuDuClaw 差異：以 **Agent** 為隔離單位，更符合 Multi-Agent 系統語義

### 6.2 n8n MCP 整合

- n8n 作為 MCP Client，透過 `MCP Client Node` 接入任意 MCP Server
- n8n 作為 MCP Server，透過 `MCP Trigger Node` 暴露 n8n Workflow 為工具
- **DuDuClaw 借鑑**：可考慮類似 n8n 的雙角色設計，任意 DuDuClaw Skill 均可透過配置暴露為 MCP 工具

---

## 7. 實作路徑（W19 計劃）

```
W18（本週）：規格草案 → TL/Agnes 審閱
W19 Week 1：
  - P0：實作 stdio transport + memory_search + wiki_read
  - 測試：Claude Desktop 本地整合驗證

W19 Week 2：
  - P0：wiki_write + send_message + 授權模型
  - 測試：E2E 整合測試

W20：
  - P1：HTTP/SSE transport
  - P1：tasks_* 端點
  - 文件：MCP Server 使用指南
```

---

## 8. 開放問題

| 問題 | 負責人 | 期望解決時間 |
|------|--------|------------|
| Wiki write 的 conflict resolution（並發寫入衝突）如何處理？ | ENG | W19 設計評審 |
| memory_store 的隔離邊界：外部 Client 寫入的記憶是否與 DuDuClaw 內部記憶混合？ | TL | W19 架構評審 |
| MCP Server 的身份認證：使用 API Key 還是 OAuth2？ | ENG | W19 設計評審 |

---

## 參照

- MCP 支援可行性評估報告：`wiki/reports/mcp-feasibility-2026-W18.md`
- MCP 官方規格：https://modelcontextprotocol.io/spec
- Zep MCP 整合參考：`wiki/research/memory/zep-vs-current.md`（PM W18 P1）
- Agnes 指令：2026-04-27

---

*起草人：PM-DuDuClaw*
*版本：v0.1 草案*
*日期：2026-04-27*
*W18-P0 — 今日完成*
