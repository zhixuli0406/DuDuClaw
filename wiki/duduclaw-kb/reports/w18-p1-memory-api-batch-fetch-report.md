---
title: "W18-P1 交付報告：Memory API Batch Fetch 確認"
created: 2026-04-29T00:00:00Z
updated: 2026-04-29T00:00:00Z
status: draft
author: duduclaw-eng-memory
tags: [w18-p1, memory, batch-fetch, api, report]
layer: deep
trust: 0.9
---

# Memory API Batch Fetch 確認報告

> **任務**：[W18-P1] 確認現有 Memory API 是否支援 `GET /memory/batch?ids=...` 介面  
> **截止**：W18 Day5（2026-05-02）  
> **結論摘要**：❌ 現有 API **不支援** batch fetch，列入 W19 Backlog，估時 3 天

---

## 1. 現有 Memory API 端點盤點

透過工具清單與 MCP Server 規格草案（`specs/mcp-server-spec-draft.md`）梳理現有 Memory API：

| 工具名稱 | 功能 | 支援 Batch？ |
|---------|------|------------|
| `memory_store` | 寫入單筆記憶 | ❌ 單筆寫入 |
| `memory_search` | 語義搜尋記憶（自然語言 query） | ❌ 無 ID 查詢 |
| `memory_search_by_layer` | 按記憶層（episodic/semantic）搜尋 | ❌ 無 ID 查詢 |
| `memory_episodic_pressure` | 查詢情節記憶壓力 | N/A |
| `memory_consolidation_status` | 查詢整合狀態 | N/A |
| `memory_successful_conversations` | 查詢成功對話記憶 | N/A |

**結論：目前 Memory API 完全缺乏「按 ID 精確查詢」能力，更不支援批次 ID 查詢（batch fetch）。**

---

## 2. 為什麼需要 Batch Fetch？

### 2.1 Checkpoint 還原依賴項（HandoffPacket OQ-01）

檢視 `specs/checkpoint-schema-v0.1-mvp.md` 與 `designs/checkpoint-schema-v1.md`，發現 `memory_refs` 群組：

```json
"memory_refs": {
  "episodic_ids": ["ep_20260421_001", "ep_20260421_002"],  // 多個 ID
  "core_memory_hash": "sha256:a3f8c2d1...",
  "context_memory_snapshot_id": "ctx_snap_20260421_184200",
  "consolidation_version": 1
}
```

Checkpoint 還原流程需要：
1. 讀取 Checkpoint → 取得 `episodic_ids` 陣列
2. **批次載入這些 episodic memories** → 重建 Agent 上下文

若無 batch fetch，每次 Session 還原需 N 次獨立 `memory_search` 呼叫，造成：
- **效能問題**：P95 < 500ms 目標可能無法達成（每次呼叫有 overhead）
- **語義漂移風險**：語義搜尋不等於精確 ID 查詢，可能載入錯誤記憶

### 2.2 W19-P0 Lazy Reference Resolver 依賴項

`checkpoint-schema-v0.1-mvp.md` 中明確標注：

> `memory_layer: MemoryLayerRef[]` — 關聯的記憶層引用（lazy reference，W19-P0 解析）

W19-P0 Lazy Reference Resolver 的解析邏輯需要 batch fetch 作為底層 API。

### 2.3 Reflexion Loop 寫入後的批次驗證

Reflexion Loop 每輪產生多個 `reflection_note`，需要批次讀取以驗證寫入結果（詳見 Part 2 報告）。

---

## 3. Batch Fetch API 規格草案

### 3.1 端點設計

```
GET /memory/batch?ids=ep_001,ep_002,ep_003
```

**或 MCP Tool 形式**：

```json
{
  "name": "memory_fetch_batch",
  "description": "Fetch multiple memory entries by their IDs in a single call.",
  "inputSchema": {
    "type": "object",
    "required": ["ids"],
    "properties": {
      "ids": {
        "type": "array",
        "items": {"type": "string"},
        "description": "Array of memory IDs to fetch",
        "maxItems": 100
      },
      "include_metadata": {
        "type": "boolean",
        "default": false,
        "description": "Include full metadata (created_at, tags, layer, etc.)"
      }
    }
  },
  "outputSchema": {
    "type": "object",
    "properties": {
      "memories": {
        "type": "array",
        "items": {
          "type": "object",
          "properties": {
            "id":        {"type": "string"},
            "content":   {"type": "string"},
            "layer":     {"type": "string", "enum": ["episodic", "semantic", "procedural"]},
            "created_at":{"type": "string", "format": "date-time"},
            "tags":      {"type": "array", "items": {"type": "string"}},
            "found":     {"type": "boolean"}
          }
        }
      },
      "missing_ids":   {"type": "array", "items": {"type": "string"}},
      "total_found":   {"type": "integer"},
      "total_missing": {"type": "integer"}
    }
  }
}
```

### 3.2 效能目標

| 指標 | 目標 |
|------|------|
| P95 延遲（≤50 IDs） | < 80ms |
| P95 延遲（50-100 IDs） | < 200ms |
| 最大批次大小 | 100 IDs / 請求 |
| 部分命中行為 | 返回找到的記憶 + `missing_ids` 列表（不報錯） |

### 3.3 安全考量

- ID 需屬於呼叫 Agent 的記憶空間（Agent 隔離）
- 跨 Agent 的 ID 查詢需額外 `cross_agent:read` scope
- `missing_ids` 不透露「ID 存在但無權存取」vs「ID 不存在」的區別（防資訊洩漏）

---

## 4. 實作估時

| 工作項 | 估時 | 說明 |
|--------|------|------|
| DB 層：按 ID 批次查詢 SQL | 0.5d | `SELECT ... WHERE id IN (...)` |
| MCP Tool：`memory_fetch_batch` 實作 | 0.5d | 參數驗證 + 授權檢查 |
| 單元測試（80%+ 覆蓋率） | 0.5d | 含邊界 case：空列表、全 missing、partial hit |
| 整合測試（Checkpoint 還原流程） | 0.5d | Session 還原 P95 驗證 |
| **合計** | **2d** | **建議 W19 Sprint 第一週交付** |

---

## 5. 建議決策

| 決策項 | 建議 |
|--------|------|
| W18 實作 batch fetch？ | ❌ 否（W18 工時已滿，無法承接） |
| 列入 W19 Backlog？ | ✅ 是，建議 W19-P0（Lazy Ref Resolver 依賴項）|
| 優先級 | **High**（阻塞 W19-P0 Lazy Reference Resolver 實作）|
| 設計先行？ | ✅ 本文件即為 API 規格草案，W19 開發直接參照 |

---

## 參照

- `specs/checkpoint-schema-v0.1-mvp.md` — Checkpoint memory_refs 欄位設計
- `designs/checkpoint-schema-v1.md` — episodic_ids 批次載入需求
- `specs/mcp-server-spec-draft.md` — MCP Tool 規格格式參考
- `reports/w18-p1-reflexion-loop-report.md` — Part 2：Reflexion Loop 評估

---

*作者：ENG-MEMORY（duduclaw-eng-memory）*  
*日期：2026-04-29*  
*版本：v1.0*
