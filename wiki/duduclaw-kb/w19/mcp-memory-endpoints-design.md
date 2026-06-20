---
title: "W19-P0 MCP Server Memory 端點技術設計 — memory/search + memory/store + memory/read"
created: 2026-04-29T00:00:00Z
updated: 2026-04-29T00:00:00Z
status: draft
author: duduclaw-eng-memory
tags: [w19, p0, mcp, server, memory, namespace, quota, tdd]
layer: deep
trust: 0.9
task_ref: "34cdb4ba-070f-40bb-92d3-34143455ce0f"
milestone: M1
changelog:
  - version: v0.1
    date: 2026-04-29
    author: duduclaw-eng-memory
    changes: "初稿 — memory 端點設計，含 namespace 隔離、quota 控制、API Key 遮罩"
---

# W19-P0 MCP Server Memory 端點技術設計

> **里程碑**：M1（Day 1-2）  
> **負責人**：duduclaw-eng-memory（ENG-MEMORY）  
> **關聯任務**：`34cdb4ba-070f-40bb-92d3-34143455ce0f`  
> **依賴文件**：  
> - `specs/mcp-server-spec-draft.md` — 端點規格草案 v0.1  
> - 任務描述 TL 裁定三項技術決策  

---

## 1. 設計目標

### 1.1 本文件範圍

負責實作 MCP Server Phase 1 中**記憶子系統**的三項端點：

| MCP 端點 | 對應內部工具 | 操作類型 |
|---------|------------|---------|
| `duduclaw/memory_search` | `memory_search` | Read |
| `duduclaw/memory_store` | `memory_store` | Write |
| `duduclaw/memory_read` | `memory_search_by_layer` (by id) | Read |

### 1.2 設計原則（TL 裁定）

1. **命名空間強制隔離**：外部 MCP Client 記憶儲存於 `external/{client_id}`，Server 強制注入，呼叫端不可自訂
2. **API Key + Scope 認證**：格式 `ddc_<env>_<random_32hex>`，Strategy Pattern 中間件，可升級至 JWT/OAuth2
3. **日誌完全遮罩 API Key**（安全硬性要求）

---

## 2. 命名空間隔離設計

### 2.1 命名空間層次結構

```
memory namespace hierarchy
├── internal/                    # DuDuClaw 系統內部（不可外部存取）
│   ├── internal/duduclaw-tl
│   ├── internal/duduclaw-eng-memory
│   └── internal/...
└── external/                    # 外部 MCP Client（本次實作範圍）
    ├── external/claude_desktop_abc123
    ├── external/cursor_xyz789
    └── external/{client_id}     # 由 API Key 映射決定
```

### 2.2 client_id 映射規則

```python
# API Key -> client_id 映射
# API Key 格式：ddc_<env>_<random_32hex>
# client_id = SHA256(api_key)[:12]  # 確定性、不可逆、短識別符
#
# 例：
# api_key  = "ddc_prod_a3f2c1e4b5d6..."
# client_id = "a3f2c1e4b5d6"
# namespace = "external/a3f2c1e4b5d6"
```

### 2.3 Namespace 注入中間件（偽碼）

```python
class NamespaceInjectionMiddleware:
    """
    Server-side namespace injection.
    外部呼叫端無法覆寫 namespace 欄位。
    """
    def process(self, request: MCPRequest, api_key_context: APIKeyContext) -> MCPRequest:
        client_id = api_key_context.client_id
        forced_namespace = f"external/{client_id}"

        # 強制覆寫，忽略 caller 傳入的 namespace
        request.params["namespace"] = forced_namespace
        return request
```

---

## 3. 端點規格

### 3.1 `duduclaw/memory_search`

**用途**：外部 MCP Client 查詢自己名下的記憶（namespace 自動隔離）

**Scope 要求**：`memory:read`

```json
{
  "name": "duduclaw/memory_search",
  "description": "Search memories stored by this client. Results are scoped to your client namespace automatically.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "query": {
        "type": "string",
        "description": "Natural language search query",
        "maxLength": 500
      },
      "limit": {
        "type": "integer",
        "description": "Maximum results (default: 10, max: 50)",
        "default": 10,
        "minimum": 1,
        "maximum": 50
      },
      "layer": {
        "type": "string",
        "enum": ["episodic", "semantic", "all"],
        "description": "Memory layer to search (default: all)",
        "default": "all"
      },
      "min_relevance": {
        "type": "number",
        "description": "Minimum relevance score filter (0.0-1.0, default: 0.5)",
        "default": 0.5,
        "minimum": 0.0,
        "maximum": 1.0
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
            "id":              { "type": "string" },
            "content":         { "type": "string" },
            "relevance_score": { "type": "number" },
            "layer":           { "type": "string" },
            "created_at":      { "type": "string", "format": "date-time" },
            "tags":            { "type": "array", "items": { "type": "string" } }
          }
        }
      },
      "total":      { "type": "integer" },
      "namespace":  { "type": "string", "description": "Your scoped namespace (read-only, informational)" },
      "query_time_ms": { "type": "integer" }
    }
  }
}
```

**效能目標**：P95 < 200ms（遵循 ENG-MEMORY 工作標準）

**Rate Limit**（沿用 spec-draft + TL 裁定）：
- Level 1 外部 Client：60 req/min
- Level 2 受信任外部：300 req/min

---

### 3.2 `duduclaw/memory_store`

**用途**：外部 MCP Client 寫入記憶，強制儲存到 `external/{client_id}` namespace

**Scope 要求**：`memory:write`

```json
{
  "name": "duduclaw/memory_store",
  "description": "Store a memory in your client namespace. Memories are isolated to your client and cannot access internal namespaces.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "content": {
        "type": "string",
        "description": "Memory content to store",
        "maxLength": 4096
      },
      "layer": {
        "type": "string",
        "enum": ["episodic", "semantic"],
        "description": "Memory layer (default: episodic)",
        "default": "episodic"
      },
      "tags": {
        "type": "array",
        "items": { "type": "string", "maxLength": 50 },
        "description": "Optional tags for categorization",
        "maxItems": 10
      },
      "ttl_days": {
        "type": "integer",
        "description": "Optional TTL in days (null = permanent)",
        "minimum": 1,
        "maximum": 365
      }
    },
    "required": ["content"]
  },
  "outputSchema": {
    "type": "object",
    "properties": {
      "id":           { "type": "string", "description": "Memory ID for future retrieval" },
      "namespace":    { "type": "string" },
      "created_at":   { "type": "string", "format": "date-time" },
      "quota_used":   { "type": "integer", "description": "Records written today" },
      "quota_limit":  { "type": "integer", "description": "Daily write quota (default: 1000)" },
      "quota_remaining": { "type": "integer" }
    }
  }
}
```

**寫入 Quota 控制**（TL 裁定）：
- 預設 1000 records/day per client
- 超限返回 HTTP 429（`quota_exceeded`），並附 `retry_after` 秒數
- Quota 以 UTC 00:00 重設

**Quota 超限回應格式**：
```json
{
  "error": {
    "code": "quota_exceeded",
    "message": "Daily write quota of 1000 records exceeded for your client.",
    "quota_limit": 1000,
    "quota_used": 1000,
    "retry_after": 18743,
    "reset_at": "2026-04-30T00:00:00Z"
  }
}
```

**Rate Limit**：
- 10 req/min（Level 1）
- 60 req/min（Level 2）

---

### 3.3 `duduclaw/memory_read`

**用途**：外部 MCP Client 以 memory ID 取得特定記憶的完整內容

**Scope 要求**：`memory:read`

```json
{
  "name": "duduclaw/memory_read",
  "description": "Retrieve a specific memory by ID. Only memories in your client namespace are accessible.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "memory_id": {
        "type": "string",
        "description": "Memory ID returned by memory_store or memory_search"
      }
    },
    "required": ["memory_id"]
  },
  "outputSchema": {
    "type": "object",
    "properties": {
      "id":         { "type": "string" },
      "content":    { "type": "string" },
      "layer":      { "type": "string" },
      "namespace":  { "type": "string" },
      "tags":       { "type": "array", "items": { "type": "string" } },
      "created_at": { "type": "string", "format": "date-time" },
      "updated_at": { "type": "string", "format": "date-time" },
      "ttl_expires_at": { "type": "string", "format": "date-time", "description": "null if permanent" }
    }
  }
}
```

**安全性**：
- Server 驗證 `memory_id` 所屬 namespace 必須等於 `external/{client_id}`
- 跨 namespace 存取（如嘗試讀取 `internal/` 記憶）返回 `404 Not Found`（不洩漏資源存在性）

---

## 4. 安全性設計

### 4.1 API Key 日誌遮罩（硬性要求）

```python
import re

API_KEY_PATTERN = re.compile(r'ddc_[a-z]+_[0-9a-f]{32}', re.IGNORECASE)

def mask_api_key(text: str) -> str:
    """遮罩所有日誌輸出中的 API Key。"""
    return API_KEY_PATTERN.sub('ddc_***_[REDACTED]', text)

# 在所有 logger 中使用 mask_api_key 過濾器
# 確保 request headers、error messages、debug logs 均不洩漏 key
```

**實作要求**：
- Logger 在 handler 層安裝 `APIKeyMaskingFilter`
- 任何 exception traceback 均需過濾
- Access log 中的 Authorization header 值替換為 `[REDACTED]`

### 4.2 Namespace 越權防護

```python
def validate_namespace_access(
    requested_namespace: str,
    client_namespace: str
) -> bool:
    """
    確認 client 只能存取自己的 namespace。
    - 不允許 'internal/' 前綴
    - 必須完整匹配 client_namespace
    """
    if requested_namespace.startswith("internal/"):
        return False
    return requested_namespace == client_namespace
```

### 4.3 Input Validation

所有端點在進入業務邏輯前執行：
- `content` 欄位：HTML escape + 長度限制
- `tags` 陣列：每個 tag 最長 50 字元，最多 10 個
- `query` 欄位：最長 500 字元，禁止注入字符（`<`, `>`, `"` escape）
- `memory_id` 格式驗證：UUID v4 格式

---

## 5. 實作架構（Python 偽碼）

```python
# mcp_server/tools/memory/
# ├── __init__.py
# ├── search.py         # memory_search tool handler
# ├── store.py          # memory_store tool handler
# ├── read.py           # memory_read tool handler
# ├── namespace.py      # NamespaceInjectionMiddleware
# ├── quota.py          # QuotaEnforcer
# └── tests/
#     ├── test_search.py
#     ├── test_store.py
#     ├── test_read.py
#     ├── test_namespace.py
#     └── test_quota.py

class MemorySearchTool:
    def __init__(
        self,
        memory_search_fn,           # 內部 memory_search MCP 工具
        namespace_middleware,       # NamespaceInjectionMiddleware
    ):
        self.memory_search_fn = memory_search_fn
        self.namespace_middleware = namespace_middleware

    async def execute(
        self,
        params: dict,
        api_key_context: APIKeyContext
    ) -> dict:
        # 1. 注入 namespace
        scoped_params = self.namespace_middleware.inject(params, api_key_context)

        # 2. 呼叫內部工具
        start = time.monotonic()
        result = await self.memory_search_fn(
            query=scoped_params["query"],
            namespace=scoped_params["namespace"],
            limit=scoped_params.get("limit", 10),
            layer=scoped_params.get("layer", "all"),
        )
        elapsed_ms = int((time.monotonic() - start) * 1000)

        # 3. 回傳結果（附 query_time_ms）
        return {
            "memories": result.get("memories", []),
            "total": result.get("total", 0),
            "namespace": scoped_params["namespace"],
            "query_time_ms": elapsed_ms,
        }


class MemoryStoreTool:
    def __init__(
        self,
        memory_store_fn,
        namespace_middleware,
        quota_enforcer,             # QuotaEnforcer
    ):
        ...

    async def execute(
        self,
        params: dict,
        api_key_context: APIKeyContext
    ) -> dict:
        # 1. Quota 檢查（先於任何寫入）
        await self.quota_enforcer.check_or_raise(api_key_context.client_id)

        # 2. 注入 namespace
        scoped_params = self.namespace_middleware.inject(params, api_key_context)

        # 3. 寫入記憶
        result = await self.memory_store_fn(
            content=scoped_params["content"],
            namespace=scoped_params["namespace"],
            layer=scoped_params.get("layer", "episodic"),
            tags=scoped_params.get("tags", []),
            ttl_days=scoped_params.get("ttl_days"),
        )

        # 4. 更新 quota 計數
        quota_info = await self.quota_enforcer.increment(api_key_context.client_id)

        return {
            "id": result["id"],
            "namespace": scoped_params["namespace"],
            "created_at": result["created_at"],
            "quota_used": quota_info.used,
            "quota_limit": quota_info.limit,
            "quota_remaining": quota_info.remaining,
        }
```

---

## 6. 測試計劃（TDD，覆蓋率 80%+）

### 6.1 test_namespace.py

| 測試案例 | 期望結果 |
|---------|---------|
| `external/{client_id}` 存取自身記憶 | ✅ 允許 |
| 嘗試存取 `internal/duduclaw-tl` | ❌ 404（不洩漏存在性） |
| 嘗試傳入自定義 namespace 覆寫 | ❌ Server 強制覆寫，忽略 caller 值 |
| API Key 不同的兩個 Client 互相存取 | ❌ 404 |

### 6.2 test_quota.py

| 測試案例 | 期望結果 |
|---------|---------|
| 寫入第 1 筆 | ✅ `quota_used: 1, quota_remaining: 999` |
| 寫入第 1000 筆 | ✅ `quota_used: 1000, quota_remaining: 0` |
| 寫入第 1001 筆 | ❌ 429 `quota_exceeded` + `retry_after` |
| UTC 00:00 後重設 quota | ✅ `quota_used: 0` |

### 6.3 test_search.py

| 測試案例 | 期望結果 |
|---------|---------|
| 有效查詢，有結果 | `memories` 陣列非空，`query_time_ms` 有值 |
| 有效查詢，無結果 | `memories: [], total: 0` |
| `limit=50`（最大值） | 最多返回 50 筆 |
| `limit=51`（超限） | ❌ 422 Validation Error |
| `query` 超過 500 字元 | ❌ 422 Validation Error |

### 6.4 test_store.py

| 測試案例 | 期望結果 |
|---------|---------|
| 有效寫入 | 返回 `id` + quota 資訊 |
| `content` 超過 4096 字元 | ❌ 422 Validation Error |
| `tags` 超過 10 個 | ❌ 422 Validation Error |
| `ttl_days=0` | ❌ 422 Validation Error |
| `layer` 為無效值 | ❌ 422 Validation Error |

### 6.5 test_read.py

| 測試案例 | 期望結果 |
|---------|---------|
| 讀取自己寫入的記憶 | ✅ 返回完整 memory 物件 |
| 讀取不存在的 memory_id | ❌ 404 |
| memory_id 格式錯誤（非 UUID） | ❌ 422 Validation Error |
| 讀取其他 client 的 memory_id | ❌ 404（namespace 不匹配） |

### 6.6 test_api_key_masking.py

| 測試案例 | 期望結果 |
|---------|---------|
| 日誌輸出中不出現完整 API Key | ✅ `ddc_***_[REDACTED]` |
| Error traceback 不洩漏 API Key | ✅ 已遮罩 |
| `mask_api_key()` 函數單元測試 | ✅ 完整匹配替換 |

---

## 7. GDPR 合規（個人資料可刪除）

- `external/{client_id}` 下的所有記憶可透過刪除 API Key 觸發批次清除
- 清除操作非同步執行，保證完整性（atomic namespace delete）
- 刪除確認通知透過 `messaging/send` 回傳（W20 P2）

---

## 8. 驗收標準檢查清單

- [ ] `memory_search`：namespace 隔離通過所有測試案例
- [ ] `memory_store`：quota 超限正確返回 429
- [ ] `memory_read`：跨 namespace 存取返回 404
- [ ] API Key 日誌遮罩：所有測試日誌掃描無 key 洩漏
- [ ] P95 搜尋延遲 < 200ms（本地壓測）
- [ ] 單元測試覆蓋率 ≥ 80%
- [ ] eng-infra 整合確認（API Key 中間件介面對齊）

---

## 9. 開放問題 / 依賴

| 問題 | 阻塞項目 | 預計解決 |
|------|---------|---------|
| API Key 中間件介面（`APIKeyContext` 格式） | 依賴 eng-infra M1 交付 | Day 2 EOD |
| 內部 `memory_store` MCP 工具的 namespace 參數支援 | 確認現有工具是否已支援 namespace 過濾 | Day 1 驗證 |
| Quota 存儲後端（Redis vs SQLite） | 需 infra 決定 | Day 1 |

---

*撰寫人：duduclaw-eng-memory（ENG-MEMORY）*  
*版本：v0.1 草案*  
*日期：2026-04-29*  
*M1 Day 1 — 設計文件初稿*
