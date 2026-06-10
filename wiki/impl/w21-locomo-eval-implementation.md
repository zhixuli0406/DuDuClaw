# W21 LOCOMO Eval — 實作規格（Implementation Spec）

> 版本：v1.0
> 建立日期：2026-05-23
> 建立者：ENG-MEMORY
> 關聯排程：locomo-eval-weekly-native-kpis（每週日 04:00 UTC）
> 關聯日報：locomo-eval-daily-smoke-test（每日 UTC）

---

## 1. 概述

本規格定義 W21 LOCOMO 記憶評測系統的兩個排程工作：

| 排程 | 頻率 | 核心 KPI |
|------|------|----------|
| Daily Smoke Test | 每日 | TC-1/2/3 三項基本健康度 |
| **Weekly Native KPIs** | 每週日 04:00 UTC | RR(7)、RR(30)、Precision@5 |

本文件側重 Weekly Native KPIs 的實作細節。

---

## 2. 背景與限制

### 2.1 環境限制（截至 W21）

- **HTTP API**：`DUDUCLAW_API_URL` 未設定，所有 HTTP live test 均 BLOCKED（連續 5+ 天）
- **SQLite 直連**：可用，路徑 `~/.duduclaw/memory.db`
- **FTS bug**：`memories_fts` 使用 rowid 而非 UUID，語意搜尋失效（詳見 §5.1）
- **記憶規模**：68 筆（65 episodic + 3 semantic），時間範圍 2026-04-21 ~ 2026-05-21

### 2.2 評測策略選擇

因 HTTP API 不可用，採 **SQLite 直連模式（Option C）**：

```
Option A: CI Staging（需 DUDUCLAW_API_URL + DUDUCLAW_API_KEY）→ 目前不可用
Option B: 本機 dev server（需手動啟動）→ 目前不可用
Option C: SQLite 直連（~/.duduclaw/memory.db）→ 可用 ✅
```

---

## 3. Weekly Native KPIs 實作流程

### 3.1 Step 1 — Memory Snapshot（RR 前置）

**目的**：為下週 RR 評測建立快照基準。

**實作**：
1. 確認 `memory_snapshots` 表存在（首次執行時 CREATE TABLE）
2. 查詢 importance ≥ 5.0 且非 SMOKETEST 的前 20 筆記憶
3. 插入 `memory_snapshots`，帶 `run_id`（UUID）和 `snapshot_date`（YYYY-MM-DD）

**Schema**：
```sql
CREATE TABLE IF NOT EXISTS memory_snapshots (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL,
    snapshot_date TEXT NOT NULL,
    memory_id TEXT NOT NULL,
    agent_id TEXT NOT NULL,
    content TEXT NOT NULL,
    layer TEXT NOT NULL,
    importance REAL NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
```

**執行位置**：`python/duduclaw/memory_eval/weekly_kpis.py` §snapshot()

### 3.2 Step 2 — Retention Rate（RR）計算

**指標定義**：

```
RR(N) = memories_still_present / memories_originally_present
      = COUNT(in memories WHERE timestamp in [N_days_ago_window])
        / (COUNT(in memories) + COUNT(in memories_archive) for same window)
```

**視窗設計**：

| 指標 | 時間視窗 | 目標 |
|------|----------|------|
| RR(7) | 7 天前±0.5 天（以整天為邊界） | ≥ 85% |
| RR(30) | 30 天前起連續 3 天 | ≥ 80%（參考值） |

**W21 計算方法**（首次執行無歷史快照）：
- 使用 `memories_archive` 作為刪除記錄
- `original = COUNT(memories WHERE window) + COUNT(memories_archive WHERE window)`
- `present = COUNT(memories WHERE window)`
- `RR = present / original × 100`

**未來版本**（有歷史快照後）：
- 應使用 `memory_snapshots` 表作為快照基準
- `RR = COUNT(snapshots in window that still exist in memories) / COUNT(snapshots in window)`

### 3.3 Step 3 — Retrieval Accuracy（RA）計算

**指標定義**：
```
Precision@5 = hits_in_top5 / total_queries
```

**Golden QA Set**：`wiki/eval/golden-qa-set.md`（10 題，詳見各題定義）

**搜尋方法（W21）**：LIKE AND 多項關鍵字搜尋（代理語意搜尋）

```sql
SELECT id, content, importance
FROM memories
WHERE content LIKE '%term1%' AND content LIKE '%term2%'
ORDER BY importance DESC, timestamp DESC
LIMIT 5
```

**正式語意搜尋方法**（FTS bug 修復後）：
```sql
-- 使用 memories_fts 全文搜尋
SELECT m.id, m.content, m.importance
FROM memories m
JOIN memories_fts fts ON m.id = fts.rowid  -- 需修復此 join
WHERE memories_fts MATCH 'query_terms'
ORDER BY rank
LIMIT 5
```

### 3.4 Step 4 — 週報產出

**路徑**：`wiki/reports/memory-quality/YYYY-MM/weekly-kpis-YYYY-MM-DD.md`

**必含欄位**：
- 執行摘要表（RR-7、RR-30、Precision@5、搜尋延遲）
- Snapshot 統計
- 各 QA 題詳細結果
- 歷史趨勢表（多週對比）
- 問題分析與建議

---

## 4. KPI 目標門檻

| KPI | 目標 | 警戒線 | 評測頻率 |
|-----|------|--------|---------|
| RR(7) — 7日保留率 | ≥ 85% | < 70% 需即時告警 | 每週 |
| RR(30) — 30日保留率 | ≥ 80% | < 60% 需即時告警 | 每週 |
| Precision@5 | ≥ 75% | < 60% 需即時告警 | 每週 |
| 搜尋 P95 延遲 | < 200ms | > 500ms 需告警 | 每日（Smoke Test） |

---

## 5. 已知問題

### 5.1 memories_fts rowid-UUID Join Bug（HIGH）

**現象**：`memories_fts` FTS 虛擬表以 SQLite 內建整數 rowid 索引，但 `memories.id` 是 TEXT UUID（無 implicit rowid 對應），JOIN 條件永遠不匹配。

**影響**：
- SQLite 層語意搜尋完全失效
- Precision@5 評測只能用 LIKE 代替，結果不代表真實語意召回能力
- 若 HTTP API 層也依賴此 FTS 表，生產環境語意搜尋亦受損

**修復方案**（需 ENG-MEMORY 執行）：
```sql
-- Option A: 重建 FTS 表，使用 content= + 外部內容表，以 INTEGER rowid 做 docid
-- Option B: memories 表加 INTEGER 自增欄位 row_num，建立 UUID→rowid 映射
-- Option C: 放棄 SQLite FTS，改用 pgvector 或 Chroma 作為向量搜尋後端
```

建議優先採 Option C（符合長期 roadmap），Option A 作為短期修復。

### 5.2 HTTP API 長期不可用（HIGH）

自 2026-05-19 起連續 5 天 API server 未啟動，所有 HTTP live test 均 BLOCKED。
需 TL 決策明確執行環境（CI staging / 本機 server / SQLite 離線）。

### 5.3 Weekly KPI 首次執行無歷史快照（INFO）

W21 是首次執行週報，`memory_snapshots` 表為空，RR 計算採「archive-based」方法。
W22 起可使用本次 snapshot 作為 7-day 基準。

---

## 6. 檔案路徑索引

| 檔案 | 用途 |
|------|------|
| `wiki/eval/golden-qa-set.md` | Golden QA Set 定義（10 題） |
| `wiki/reports/memory-quality/2026-05/weekly-kpis-*.md` | 週報存檔 |
| `wiki/reports/memory-quality/2026-05/smoke-test-*.md` | 日報存檔 |
| `python/duduclaw/memory_eval/` | 評測程式碼目錄 |
| `specs/locomo-memory-eval-spec-v1.md` | 原始規格（上層） |

---

*由 ENG-MEMORY 自動產生 — 2026-05-23（W21 Weekly KPI 首次執行時初始化）*
