---
title: "Checkpoint Schema 設計初稿 — MVP v0.1"
created: 2026-04-29T00:00:00Z
updated: 2026-04-29T00:00:00Z
status: draft
author: duduclaw-eng-memory
tags: [w17, memory, checkpoint, schema, mvp, agent-state, persistence]
layer: deep
trust: 0.9
sprint: W17
changelog:
  - version: v0.1
    date: 2026-04-29
    author: duduclaw-eng-memory
    changes: "W17-CRITICAL 交付：Checkpoint Schema MVP 初稿，選項 A（Schema 定義 + 設計說明）"
---

# Checkpoint Schema 設計初稿 — MVP v0.1

> **任務來源**：W17-CRITICAL（Task ID: 7fba81f8-0758-41e8-b3d4-ff649ed4ce53）
> **交付形式**：選項 A — Schema 定義 + 設計考量說明
> **撰寫者**：ENG-MEMORY（duduclaw-eng-memory）
> **狀態**：Draft，等待 TL 審查

---

## 1. 核心 Schema 定義（TypeScript）

### 1.1 MVP 最小 Schema（與任務規格一致）

```typescript
interface CheckpointSchema {
  checkpoint_id: string;        // UUID v4，全系統唯一識別碼
  agent_id: string;             // 建立此 Checkpoint 的 Agent 識別碼
  task_id: string;              // 關聯任務 ID（對應 Kanban task）
  state_snapshot: object;       // Agent 當前狀態快照（見 §1.3）
  created_at: string;           // ISO8601 UTC（e.g. "2026-04-29T10:00:00Z"）
  metadata: CheckpointMetadata;
}

interface CheckpointMetadata {
  sprint?: string;              // Sprint 標識（e.g. "W17", "W19"）
  trigger?: 'manual' | 'auto' | 'error';  // 建立觸發來源
  [key: string]: unknown;       // 擴展欄位（向前相容）
}
```

### 1.2 DuDuClaw 擴充 Schema（v0.1 完整版）

結合 HandoffPacket v0.2、EvolutionEvents v1.0 及記憶分層架構，MVP 建議採用以下擴充版本：

```typescript
interface DuDuClawCheckpoint extends CheckpointSchema {
  // === 版本與生命週期 ===
  schema_version: '0.1';              // Schema 版本，用於向後相容解析
  status: CheckpointStatus;           // 當前狀態
  parent_checkpoint_id: string | null; // 父 Checkpoint ID，構成復原鏈

  // === 系統整合欄位 ===
  session_id: string | null;          // 關聯 Session ID（跨 session 持久化）
  handoff_packet_id: string | null;   // 關聯 HandoffPacket ID（v0.2）
  evolution_generation: number | null; // GVU 世代編號（對應 EvolutionEvents generation，P0 為 null）

  // === 記憶系統整合 ===
  memory_layer: MemoryLayerRef[];     // 關聯的記憶層引用（lazy reference，W19-P0 解析）

  // === 安全與隱私 ===
  encryption_key_id: string | null;   // 加密金鑰 ID（state_snapshot 加密時使用）
  gdpr_deletable: boolean;            // 是否納入 GDPR 刪除範圍（預設 true）

  // === 壓縮與效能 ===
  snapshot_size_bytes: number;        // state_snapshot 序列化後的位元組大小
  compressed: boolean;                // state_snapshot 是否已壓縮（gzip）
  ttl_seconds: number | null;         // Checkpoint 存活秒數（null = 永久保留）

  // === 擴充 metadata ===
  metadata: DuDuClawCheckpointMetadata;
}

type CheckpointStatus =
  | 'active'       // 當前有效的 Checkpoint
  | 'superseded'   // 已被更新的 Checkpoint 取代（仍可用於復原）
  | 'archived'     // 已歸檔，壓縮存儲，不常訪問
  | 'corrupted'    // 驗證失敗，不可用於復原（保留以供診斷）
  | 'deleted';     // GDPR 刪除後的邏輯標記（state_snapshot 已清除）

interface MemoryLayerRef {
  ref_type: 'memory_record' | 'memory_search';
  layer: 'episodic' | 'semantic' | 'procedural';
  source_id?: string;     // memory_record 模式：直接記憶 ID
  query_hint?: string;    // memory_search 模式：語意查詢 hint
  ttl_seconds?: number;   // 快取 TTL（對應 W19-P0 Lazy Reference 規格）
}

interface DuDuClawCheckpointMetadata {
  sprint?: string;
  trigger?: 'manual' | 'auto' | 'error';
  // 觸發錯誤時的診斷資訊
  error_context?: {
    error_type: string;
    error_message: string;
    stack_trace?: string;
  };
  // 對應 HandoffPacket v0.2 progress_ledger 快照
  progress_checkpoint_snapshot?: string;  // 人類可讀的進度描述（< 200 字元）
  stagnation_count_at_checkpoint?: number;
  // 效能計量
  agent_execution_time_ms?: number;
  // 品質評分（對應 W19-P1 軌跡品質評分）
  quality_score?: number;             // 0.0–1.0
  intent_category?: 'repair' | 'optimize' | 'innovate' | null;
  [key: string]: unknown;
}
```

### 1.3 `state_snapshot` 建議結構

`state_snapshot` 為非結構化 JSON object，以下為建議的 DuDuClaw Agent 狀態快照格式：

```typescript
interface AgentStateSnapshot {
  // 任務執行上下文
  current_task_step: string;          // 當前執行步驟描述
  completed_steps: string[];          // 已完成步驟列表
  pending_steps: string[];            // 待完成步驟列表
  tool_call_history: ToolCallEntry[]; // 最近 N 次工具呼叫記錄（建議 ≤ 20 條）

  // 關鍵決策記錄
  key_decisions: string[];            // 重要決策點的人類可讀描述

  // 外部依賴狀態
  external_resource_refs: {
    type: string;
    id: string;
    status: 'fetched' | 'pending' | 'failed';
  }[];
}

interface ToolCallEntry {
  tool_name: string;
  called_at: string;             // ISO8601
  outcome: 'success' | 'failure' | 'timeout';
  result_summary?: string;       // 結果摘要（< 100 字元，不含原始資料）
}
```

---

## 2. 實例範例

### 2.1 正常執行中的 Checkpoint（trigger: 'auto'）

```json
{
  "checkpoint_id": "cp-7a3f2e1b-4c5d-6e7f-8a9b-0c1d2e3f4a5b",
  "schema_version": "0.1",
  "agent_id": "duduclaw-eng-memory",
  "task_id": "7fba81f8-0758-41e8-b3d4-ff649ed4ce53",
  "session_id": "sess-2026-04-29-001",
  "status": "active",
  "parent_checkpoint_id": null,
  "handoff_packet_id": null,
  "evolution_generation": null,
  "created_at": "2026-04-29T10:30:00Z",
  "state_snapshot": {
    "current_task_step": "撰寫 Schema 設計說明",
    "completed_steps": ["閱讀 HandoffPacket v0.2 規格", "閱讀 EvolutionEvents v1.0 規格"],
    "pending_steps": ["寫入 wiki", "通知 TL 審查"],
    "tool_call_history": [],
    "key_decisions": ["採用選項 A（Schema + 說明），不做選項 B（現狀盤點）"]
  },
  "memory_layer": [
    {
      "ref_type": "memory_search",
      "layer": "episodic",
      "query_hint": "checkpoint schema design W17",
      "ttl_seconds": 3600
    }
  ],
  "encryption_key_id": null,
  "gdpr_deletable": true,
  "snapshot_size_bytes": 512,
  "compressed": false,
  "ttl_seconds": 604800,
  "metadata": {
    "sprint": "W17",
    "trigger": "auto",
    "progress_checkpoint_snapshot": "Completed spec review, drafting schema definitions",
    "stagnation_count_at_checkpoint": 0,
    "agent_execution_time_ms": 15200
  }
}
```

### 2.2 錯誤恢復 Checkpoint（trigger: 'error'）

```json
{
  "checkpoint_id": "cp-error-9b8a7c6d-...",
  "schema_version": "0.1",
  "agent_id": "duduclaw-eng-memory",
  "task_id": "7fba81f8-...",
  "status": "active",
  "parent_checkpoint_id": "cp-7a3f2e1b-...",
  "metadata": {
    "sprint": "W17",
    "trigger": "error",
    "error_context": {
      "error_type": "ToolCallTimeout",
      "error_message": "shared_wiki_write timed out after 30s"
    },
    "progress_checkpoint_snapshot": "Schema draft complete, wiki write failed"
  }
}
```

---

## 3. 設計考量

### 3.1 版本化策略

**問題**：Agent 狀態結構隨 Sprint 快速演進，任何欄位變動都可能破壞歷史 Checkpoint 的可讀性。

**策略：顯式版本號 + Optional 欄位優先**

- `schema_version` 欄位採用語意化版本（`'0.1'`、`'0.2'` 等），明確區分不同 Schema 世代
- 新欄位**必須**使用 `?`（Optional / `| null`）設計，確保舊版 Checkpoint 在新版 Resolver 中可安全反序列化
- 借鑑 HandoffPacket v0.2 的向後相容原則：新版 Reader 遇到舊版 Checkpoint 時，缺失欄位以預設值處理，**不拋出錯誤**
- 破壞性變更（欄位重命名、型別變更）需升級 major version（如 `'1.0'`），並提供遷移工具
- 建議在 `data/checkpoints/schema/` 目錄維護各版本的 JSON Schema 檔案，供 Validator 動態載入

### 3.2 儲存位置建議

**建議架構（三層分離）**：

```
data/
├── checkpoints/
│   ├── active/
│   │   └── {agent_id}/
│   │       └── {task_id}/
│   │           └── {checkpoint_id}.json    ← 當前活躍 Checkpoint（熱儲存）
│   ├── archive/
│   │   └── YYYY-MM/
│   │       └── {checkpoint_id}.json.gz    ← 歸檔壓縮（冷儲存）
│   └── schema/
│       └── v0.1.json                       ← JSON Schema Validator
```

**選型評估**：

| 儲存層 | 建議方案 | 理由 |
|--------|---------|------|
| 熱儲存（active） | 本地 JSON 檔案 + SQLite 索引 | P0 簡單實作；SQLite 支援快速 task_id / agent_id 查詢 |
| 冷儲存（archive） | gzip 壓縮 JSON 檔案 | state_snapshot 通常大，壓縮率可達 70%+ |
| 未來擴充 | pgvector / Chroma（state_snapshot 向量化） | 支援語意搜尋「哪個 Checkpoint 最接近當前狀態」|

**理由**：EvolutionEvents 採用 JSONL append-only 設計（審計日誌語義），Checkpoint 則具備可更新性（status 字段變更、supersede 操作），兩者儲存模式根本不同，**不應合併**至同一 JSONL 檔案。

### 3.3 與現有系統整合

**與 HandoffPacket v0.2 整合**：
- Checkpoint 的 `metadata.progress_checkpoint_snapshot` 對應 HandoffPacket `progress_ledger.progress_checkpoint`，兩者語義一致
- 當 `task_ledger_reset=true` 觸發重規劃時，系統應自動建立一個 `trigger='auto'` 的 Checkpoint，記錄重規劃前的最後狀態
- `stagnation_count_at_checkpoint` 欄位確保 Checkpoint 可重建停滯發生時的完整上下文

**與 EvolutionEvents 整合**：
- `evolution_generation` 欄位與 EvolutionEvents `generation`（P2 啟用）對齊，未來可在 JSONL 審計日誌中交叉查詢
- 建議在 `skill_activate` / `gvu_generation` 事件前後自動觸發 `trigger='auto'` Checkpoint，提供演化前後的狀態對比能力
- `intent_category` 欄位複用 EvolutionEvents P2 定義（`repair` / `optimize` / `innovate`），保持語義統一

**與 Memory 系統整合**：
- `memory_layer` 陣列採用 Lazy Reference 設計，與 W19-P0 Memory Lazy Reference Resolver 規格對齊
- Checkpoint 建立時**不主動拉取**記憶內容，僅記錄引用；Resolver 在需要恢復狀態時按需解析
- 支援三種記憶層（episodic / semantic / procedural）的引用，確保完整的記憶上下文可被還原

### 3.4 GDPR 合規設計

DuDuClaw 的 Checkpoint 可能包含使用者相關的任務內容（`state_snapshot`），依 GDPR 需支援可刪除性：

- `gdpr_deletable: true`（預設）的 Checkpoint，在收到刪除請求後須在 30 日內清除 `state_snapshot`
- 刪除後將 `status` 設為 `'deleted'`，保留 `checkpoint_id`、`agent_id`、`task_id`、`created_at` 等非個人資料欄位（用於審計追蹤）
- `encryption_key_id` 支援 Envelope Encryption：`state_snapshot` 以 AES-256-GCM 加密，金鑰由 KMS 管理。金鑰撤銷即等同刪除（Crypto-Shredding 模式）
- 系統敏感 Checkpoint（如 evolution_generation 相關）可設 `gdpr_deletable: false`，需 TL 審批

### 3.5 壓縮與效能考量

- `state_snapshot` 在 `snapshot_size_bytes > 10KB` 時自動啟用 gzip 壓縮（`compressed: true`）
- 搜尋 P95 < 200ms 目標：SQLite 索引欄位為 `(agent_id, task_id, status, created_at)`，`state_snapshot` 不參與索引
- `ttl_seconds`：建議活躍任務 Checkpoint 設 604800（7 天），長期歸檔設 `null`（永久），錯誤 Checkpoint 設 86400（1 天）

---

## 4. 核心缺口識別（對應選項 B 現狀盤點）

基於對現有系統的評估，記憶系統目前不原生支援 Checkpoint 概念，核心缺口如下：

| # | 缺口 | 影響 | 建議優先級 |
|---|------|------|----------|
| G1 | 無 Checkpoint 建立 / 讀取 / 刪除 API | Agent 崩潰後無法從中間狀態恢復，必須重新執行整個任務 | P0 |
| G2 | `state_snapshot` 未定義結構化格式 | 無法跨 Agent 版本重現任務狀態，復原可靠性低 | P0 |
| G3 | 缺乏與 HandoffPacket `progress_ledger` 的雙向綁定 | 停滯偵測觸發重規劃時無法保存恢復點，重規劃後無法比對前後狀態差異 | P1 |

---

## 5. 待解決開放問題（W18 追蹤）

| # | 問題 | 負責人 | 期望解決時間 |
|---|------|--------|------------|
| Q1 | `state_snapshot` 是否需要 JSON Schema 強制驗證，或保持完全非結構化以保留彈性？ | ENG-MEMORY + TL | W18 架構評審 |
| Q2 | Checkpoint 觸發頻率策略：每 N 個工具呼叫？每個重要決策點？ | ENG-AGENT + ENG-MEMORY | W18 設計評審 |
| Q3 | 多 Agent 協作場景下，同一 task_id 下的多個 Agent Checkpoint 如何合併成統一的「任務快照」？ | TL | W19 架構評審 |
| Q4 | SQLite vs. 直接 JSON 檔案的 P0 實作選擇（效能 benchmark 待補充）？ | ENG-MEMORY | W18 Day2 |

---

## 6. 實作路線圖（MVP → Production）

```
P0（W18 — MVP 實作）
├── CheckpointSchema v0.1 TypeScript/Rust struct 定義
├── 基本 CRUD：create / read / update_status
├── 本地 JSON 儲存（data/checkpoints/active/）
├── SQLite 索引（agent_id, task_id, status, created_at）
└── 單元測試覆蓋率 ≥ 80%

P1（W19 — 系統整合）
├── 與 HandoffPacket progress_ledger 雙向綁定
├── 與 EvolutionEvents skill_activate / gvu_generation 自動觸發
├── Memory Lazy Reference Resolver 整合（對應 W19-P0）
└── GDPR 刪除 API（state_snapshot 清除 + Crypto-Shredding）

P2（W20+ — 進階功能）
├── state_snapshot 向量化索引（語意搜尋「最相近狀態」）
├── Checkpoint 差異比較（before/after 演化對比）
├── 自動壓縮歸檔（超過 TTL 的 Checkpoint 移至 archive/）
└── 跨 Agent Checkpoint 聚合視圖
```

---

## 7. 參照文件

- HandoffPacket 規格 v0.2：`specs/handoffpacket-spec-v0.2.md`
- EvolutionEvents 技術規格 v1.0：`specs/evolution-events-spec-v1.md`
- Memory 系統評估（Zep vs Mem0）：`research/memory/zep-vs-current.md`
- W19-P0 Memory Lazy Reference Resolver 任務：Task ID `9e6f71e0-97b5-4237-8e12-aae72c6ccb12`
- W19-P1 軌跡品質評分任務：Task ID `4f8f71ab-37b1-4c8b-9e4e-bfb42bc32b96`

---

*設計者：ENG-MEMORY（duduclaw-eng-memory）*
*版本：v0.1 Draft | 日期：2026-04-29*
*待審查人：TL-DuDuClaw（duduclaw-tl）*
*W17-CRITICAL 交付物 — Task 7fba81f8-0758-41e8-b3d4-ff649ed4ce53*
