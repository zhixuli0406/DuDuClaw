---
title: "W18-P1 交付報告：Temporal Knowledge Graph Schema 草案"
created: 2026-04-29T00:00:00Z
updated: 2026-04-29T00:00:00Z
status: draft
author: duduclaw-eng-memory
tags: [w18-p1, w19-p0, temporal-knowledge-graph, schema, memory, graph-db, vector-db]
layer: deep
trust: 0.9
---

# Temporal Knowledge Graph Schema 草案

> **任務**：[W18-P1 + W19-P0] Temporal Knowledge Graph 技術調研  
> **截止**：W18 Day5（2026-05-02）  
> **輸出**：`valid_until` / `superseded_by` / `created_at` Schema 草案 + 現有 DB 評估

---

## 1. 調研背景

### 1.1 為什麼需要 Temporal Knowledge Graph？

DuDuClaw 現有記憶系統的核心弱點（來自 `research/memory/zep-vs-current.md`）：

```
時態記憶能力：████       4/10（需強化）
知識圖譜能力：████       4/10（需建設）
衝突解決能力：███        3/10（需建設）
```

**核心問題**：記憶是靜態的 — 一旦寫入，無法表達「這個事實在某個時間點已不再有效」。

**具體問題場景**：

| 情境 | 現有系統問題 | Temporal KG 解法 |
|------|------------|-----------------|
| 偏好改變 | Agent 記住「用戶偏好 Python」，後來改用 TypeScript，兩條記憶共存造成混淆 | `valid_until` 標記舊事實失效 |
| 知識過時 | 「Zep v2 是最新版」 → Zep v3 發布後，舊記憶仍存在 | `superseded_by` 指向新記憶 |
| 市場/競品狀態 | 「Claude 3.5 是最強模型」 → Claude 4 發布後失效 | `valid_until` + 自動衝突解決 |
| Reflexion 知識更新 | 新 reflection_note 應取代舊策略 | `superseded_by` 鏈式追蹤 |

### 1.2 Zep Temporal KG 的核心設計（借鑑來源）

Zep v2 雙時態模型：

| 時間軸 | 欄位 | 意義 |
|--------|------|------|
| **有效時間** | `valid_at` / `invalid_at` | 事實在現實世界中有效的時間窗口 |
| **交易時間** | `created_at` | 系統記錄此事實的時間 |

DuDuClaw 設計差異：
- Zep 以 User Session 為隔離單位；DuDuClaw 以 **Agent** 為隔離單位
- DuDuClaw 有**分層記憶**（episodic/semantic）；Zep 無此概念
- DuDuClaw 需額外支援 `superseded_by`（記憶取代鏈）以支援 Reflexion Loop

---

## 2. Temporal Memory Node Schema 設計

### 2.1 核心 Schema（JSON Schema Draft-07）

```json
{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "$id": "temporal-memory-node-v0.1",
  "title": "TemporalMemoryNode",
  "description": "DuDuClaw Temporal Knowledge Graph 節點 Schema",
  "type": "object",
  "required": [
    "node_id",
    "content",
    "layer",
    "created_at",
    "valid_from"
  ],
  "properties": {

    "node_id": {
      "type": "string",
      "format": "uuid",
      "description": "記憶節點全域唯一識別碼（UUIDv4）"
    },

    "agent_id": {
      "type": "string",
      "description": "擁有此記憶的 Agent 識別碼（隔離邊界）"
    },

    "content": {
      "type": "string",
      "description": "記憶的自然語言內容"
    },

    "layer": {
      "type": "string",
      "enum": ["episodic", "semantic", "procedural"],
      "description": "記憶層分類：episodic=情節記憶, semantic=語義記憶, procedural=技能記憶"
    },

    "created_at": {
      "type": "string",
      "format": "date-time",
      "description": "此記憶節點被系統建立的時間（交易時間 / Transaction Time）"
    },

    "valid_from": {
      "type": "string",
      "format": "date-time",
      "description": "此事實在現實世界中開始有效的時間（有效時間開始 / Valid Time Start）。通常等於 created_at，但可設定為過去時間（e.g. 記錄歷史事實）"
    },

    "valid_until": {
      "type": ["string", "null"],
      "format": "date-time",
      "default": null,
      "description": "此事實失效時間（有效時間結束 / Valid Time End）。null = 目前仍有效。當新事實取代此記憶時，系統自動設定此欄位"
    },

    "superseded_by": {
      "type": ["string", "null"],
      "format": "uuid",
      "default": null,
      "description": "若此記憶被新記憶取代，此欄位指向取代記憶的 node_id。形成可追蹤的取代鏈（supersession chain）"
    },

    "supersedes": {
      "type": ["string", "null"],
      "format": "uuid",
      "default": null,
      "description": "若此記憶是取代既有記憶的新記憶，此欄位指向被取代記憶的 node_id（反向參照）"
    },

    "subject": {
      "type": ["string", "null"],
      "description": "三元組主詞（e.g. 'user:alice', 'agent:duduclaw-eng-memory'）。Semantic 層記憶強烈建議填寫"
    },

    "predicate": {
      "type": ["string", "null"],
      "description": "三元組謂詞（e.g. 'prefers', 'works_on', 'has_skill'）"
    },

    "object": {
      "type": ["string", "null"],
      "description": "三元組賓詞（e.g. 'python', 'task:w18-p1', 'skill:memory-design'）"
    },

    "confidence": {
      "type": "number",
      "minimum": 0.0,
      "maximum": 1.0,
      "default": 1.0,
      "description": "此記憶的可信度分數（0.0-1.0）。自動提取的記憶通常 < 1.0；人工確認的記憶 = 1.0"
    },

    "tags": {
      "type": "array",
      "items": {"type": "string"},
      "default": [],
      "description": "標籤列表，用於過濾搜尋（e.g. ['reflexion', 'task_type:code_review']）"
    },

    "metadata": {
      "type": ["object", "null"],
      "default": null,
      "description": "擴充 metadata KV 存儲（結構化資料，如 critic_score、task_id 等）",
      "additionalProperties": true
    },

    "embedding_id": {
      "type": ["string", "null"],
      "description": "此記憶對應的向量嵌入 ID（存於 Vector DB，用於語義搜尋）"
    },

    "source": {
      "type": "string",
      "enum": ["manual", "llm_extract", "reflexion", "consolidation", "checkpoint_restore"],
      "default": "llm_extract",
      "description": "記憶來源：手動寫入、LLM 提取、Reflexion Loop、Consolidation 整合、Checkpoint 還原"
    }
  }
}
```

### 2.2 完整範例

**範例 A：使用者偏好記憶（初始）**

```json
{
  "node_id": "550e8400-e29b-41d4-a716-446655440001",
  "agent_id": "duduclaw-eng-memory",
  "content": "用戶偏好 Python 作為主要開發語言",
  "layer": "semantic",
  "created_at": "2026-01-01T10:00:00Z",
  "valid_from": "2026-01-01T10:00:00Z",
  "valid_until": "2026-04-01T00:00:00Z",
  "superseded_by": "550e8400-e29b-41d4-a716-446655440002",
  "supersedes": null,
  "subject": "user:main",
  "predicate": "prefers_language",
  "object": "python",
  "confidence": 0.9,
  "tags": ["preference", "language"],
  "source": "llm_extract"
}
```

**範例 B：取代記憶（用戶改用 TypeScript）**

```json
{
  "node_id": "550e8400-e29b-41d4-a716-446655440002",
  "agent_id": "duduclaw-eng-memory",
  "content": "用戶轉向 TypeScript 作為主要前端語言，Python 仍用於後端",
  "layer": "semantic",
  "created_at": "2026-04-01T10:00:00Z",
  "valid_from": "2026-04-01T00:00:00Z",
  "valid_until": null,
  "superseded_by": null,
  "supersedes": "550e8400-e29b-41d4-a716-446655440001",
  "subject": "user:main",
  "predicate": "prefers_language",
  "object": "typescript",
  "confidence": 0.95,
  "tags": ["preference", "language"],
  "source": "llm_extract"
}
```

**範例 C：Reflexion Note（來自 critic_recalibration）**

```json
{
  "node_id": "550e8400-e29b-41d4-a716-446655440003",
  "agent_id": "duduclaw-eng-memory",
  "content": "執行 code_review 任務時，必須強制覆蓋安全性檢查場景（XSS、CSRF、SQL Injection）",
  "layer": "episodic",
  "created_at": "2026-04-29T10:00:00Z",
  "valid_from": "2026-04-29T10:00:00Z",
  "valid_until": null,
  "superseded_by": null,
  "supersedes": null,
  "subject": "task_type:code_review",
  "predicate": "requires_check",
  "object": "security_coverage",
  "confidence": 0.78,
  "tags": ["reflexion", "task_type:code_review", "critic_recalibration"],
  "metadata": {
    "task_id": "task-abc-123",
    "critic_score": 0.62,
    "reflexion_type": "corrective"
  },
  "source": "reflexion"
}
```

---

## 3. 取代鏈（Supersession Chain）機制

### 3.1 自動衝突解決邏輯

```
新記憶寫入（相同 subject + predicate）
        ↓
查詢現有有效記憶（valid_until IS NULL AND same subject+predicate）
        ↓
若找到衝突記憶：
  1. 設定舊記憶 valid_until = NOW()
  2. 設定舊記憶 superseded_by = 新記憶 node_id
  3. 設定新記憶 supersedes = 舊記憶 node_id
        ↓
寫入新記憶（valid_from = NOW(), valid_until = null）
```

### 3.2 時態查詢 API 設計

```python
# 查詢某時間點的有效記憶（Point-in-Time Query）
memory_search(
    query="用戶偏好的程式語言",
    point_in_time="2026-02-01T00:00:00Z"  # 新增參數
    # 內部邏輯：valid_from <= point_in_time AND (valid_until IS NULL OR valid_until > point_in_time)
)

# 查詢某主詞的完整歷史（Supersession Chain）
memory_get_history(
    subject="user:main",
    predicate="prefers_language"
)
# 返回：取代鏈上的所有記憶節點，按 valid_from 排序
```

---

## 4. 現有 DB 評估：需替換或擴充？

### 4.1 向量 DB（語義搜尋層）

| 候選方案 | 時態支援 | 評估 |
|---------|---------|------|
| **現有向量 DB（推測為 Chroma/pgvector）** | ❌ 無原生時態索引 | 需在 metadata 欄位存儲時態資訊，查詢時 post-filter |
| **Pinecone** | ❌ 無原生時態索引 | 同上 |
| **pgvector（PostgreSQL）** | ✅ 可利用 PostgreSQL 的 timestamp 索引 + 向量搜尋組合查詢 | **推薦：保留 + 擴充 schema** |

**結論**：向量 DB **不需要替換**，但需在記憶表中加入時態欄位（`valid_from`, `valid_until`, `superseded_by`）並建立索引。

### 4.2 Graph DB（知識圖譜層）

| 候選方案 | 評估 | 建議 |
|---------|------|------|
| **無（現況）** | DuDuClaw 目前無 Graph DB | W19 Phase 1 先用 PostgreSQL adjacency list 模擬 |
| **Neo4j** | ✅ 原生圖遍歷；Zep 採用；時態屬性需自行實作 | W20+ 考慮引入（部署成本高）|
| **PostgreSQL（adjacency list）** | ✅ 零額外基礎設施；SQL 支援遞迴查詢（WITH RECURSIVE） | **推薦 W19 MVP 方案** |
| **Apache AGE（PostgreSQL extension）** | ✅ PostgreSQL + 圖查詢（openCypher）| 中期可評估 |

**W19 MVP 建議**：用 PostgreSQL 的 `supersedes` / `superseded_by` 外鍵模擬取代鏈，不引入獨立 Graph DB，降低複雜度。

### 4.3 MVP 資料表設計（PostgreSQL）

```sql
CREATE TABLE temporal_memory_nodes (
    node_id         UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    agent_id        TEXT NOT NULL,
    content         TEXT NOT NULL,
    layer           TEXT NOT NULL CHECK (layer IN ('episodic', 'semantic', 'procedural')),
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    valid_from      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    valid_until     TIMESTAMPTZ,                    -- NULL = 仍有效
    superseded_by   UUID REFERENCES temporal_memory_nodes(node_id),
    supersedes      UUID REFERENCES temporal_memory_nodes(node_id),
    subject         TEXT,
    predicate       TEXT,
    object          TEXT,
    confidence      FLOAT NOT NULL DEFAULT 1.0 CHECK (confidence BETWEEN 0 AND 1),
    tags            TEXT[] DEFAULT '{}',
    metadata        JSONB DEFAULT '{}',
    embedding_id    TEXT,
    source          TEXT NOT NULL DEFAULT 'llm_extract'
);

-- 時態查詢索引
CREATE INDEX idx_tmn_agent_valid
    ON temporal_memory_nodes(agent_id, valid_from, valid_until);

-- 三元組索引（衝突解決查詢）
CREATE INDEX idx_tmn_triple
    ON temporal_memory_nodes(agent_id, subject, predicate)
    WHERE valid_until IS NULL;  -- 只索引當前有效記憶

-- JSONB metadata 索引
CREATE INDEX idx_tmn_metadata
    ON temporal_memory_nodes USING gin(metadata);

-- Tags 索引
CREATE INDEX idx_tmn_tags
    ON temporal_memory_nodes USING gin(tags);
```

---

## 5. 與現有記憶系統的整合路徑

### 5.1 漸進式升級策略

```
Phase 1（W19 MVP）：
  - 現有 memory_store / memory_search 加入時態欄位（向後相容）
  - 新欄位可選，預設 valid_from=NOW(), valid_until=NULL
  - 實作自動衝突解決（相同 subject+predicate 時更新 valid_until）

Phase 2（W20）：
  - 時態查詢 API（point_in_time 參數）
  - Supersession Chain 查詢（memory_get_history）
  - Consolidation 機制整合時態語義

Phase 3（W21+）：
  - 評估引入 Apache AGE 或 Neo4j
  - Graph 遍歷查詢（e.g. 二跳關係推理）
  - Zep TKG 完整對標能力
```

### 5.2 向後相容保證

- 現有無時態欄位的記憶讀取時：`valid_from = created_at`，`valid_until = NULL`
- 現有 `memory_search` 預設只返回 `valid_until IS NULL` 的有效記憶
- 可透過 `include_historical=true` 參數查詢歷史記憶

---

## 6. 調研結論

| 問題 | 答案 |
|------|------|
| `valid_until` / `superseded_by` / `created_at` 適合哪種 Schema？ | 見第 2 節 JSON Schema v0.1 草案 |
| 現有 Graph DB 是否需替換？ | **不需要替換** — W19 用 PostgreSQL adjacency list 模擬 |
| 現有向量 DB 是否需替換？ | **不需要替換** — 擴充 schema 加入時態欄位即可 |
| W19-P0 最小可行方案？ | PostgreSQL 時態欄位擴充 + 自動衝突解決邏輯 |
| 需額外引入的基礎設施？ | **無**（W19 MVP 零額外基礎設施成本）|

---

## 參照

- `research/memory/zep-vs-current.md` — Zep Temporal KG 架構分析
- `specs/checkpoint-schema-v0.1-mvp.md` — memory_refs 時態需求
- `reports/w18-p1-memory-api-batch-fetch-report.md` — Part 1：Batch Fetch 確認
- `reports/w18-p1-reflexion-loop-report.md` — Part 2：Reflexion Loop 評估
- Zep v2 Temporal KG 官方文件：https://help.getzep.com

---

*作者：ENG-MEMORY（duduclaw-eng-memory）*  
*日期：2026-04-29*  
*版本：v0.1 草案（W19-P0 開發直接參照）*
