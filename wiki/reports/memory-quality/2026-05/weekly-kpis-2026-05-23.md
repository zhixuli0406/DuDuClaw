# W21 Weekly Native KPIs — 記憶系統週報 2026-05-23

> 執行者：ENG-MEMORY (duduclaw-eng-memory)
> 執行時間：2026-05-23 04:00 UTC（排程觸發 locomo-eval-weekly-native-kpis）
> 實作規格：wiki/impl/w21-locomo-eval-implementation.md
> Golden QA Set：wiki/eval/golden-qa-set.md
> Run ID：603aa92b-d657-48bc-96f4-3cd050b3976a

---

## 執行摘要

| KPI | 目標 | 本週結果 | 狀態 |
|-----|------|----------|------|
| RR(7) — 7 日保留率 | ≥ 85% | **100.0%** | ✅ 達標 |
| RR(30) — 30 日保留率 | ≥ 80% | **100.0%** | ✅ 達標 |
| Precision@5（RA） | ≥ 75% | **100.0%**（10/10） | ✅ 達標 |
| 搜尋平均延遲 | < 200ms | **0.41ms** | ✅ 優秀 |
| Memory Snapshot | 20 筆記憶 | 已寫入 memory_snapshots | ✅ 完成 |
| 整體狀態 | — | **全部 KPI 達標**（LIKE 搜尋模式） | ✅ PASS |

> ⚠️ **重要聲明**：本週為 W21 **首次**執行週報。RR 採 archive-based 方法（無歷史快照），
> Precision@5 採 LIKE AND 搜尋（FTS rowid-UUID bug 導致語意搜尋失效）。
> 數字偏樂觀，不代表真實語意召回能力，待 FTS 修復後重新校準。

---

## 環境診斷

```
執行模式：SQLite 直連（Option C）
HTTP API：❌ BLOCKED（DUDUCLAW_API_URL 未設定，連續第 5 天）
SQLite DB：✅ ~/.duduclaw/memory.db
Python：3.11 / sqlite3 內建
FTS 狀態：⚠️ rowid-UUID join bug（語意搜尋失效，使用 LIKE 代替）
```

### 記憶庫全域統計

| 項目 | 數值 |
|------|------|
| Active memories | 68 筆 |
| Archived (deleted) | 0 筆 |
| Episodic | 65 筆 |
| Semantic | 3 筆 |
| 最早記憶 | 2026-04-21 |
| 最新記憶 | 2026-05-21 |
| 平均 importance | 5.13 |

---

## Step 1 — Memory Snapshot（RR 前置）

**目的**：建立 W21 基準快照，供 W22 RR(7) 計算使用。

| 項目 | 結果 |
|------|------|
| Run ID | `603aa92b-d657-48bc-96f4-3cd050b3976a` |
| Snapshot 日期 | 2026-05-23 |
| 快照策略 | Top-20 by importance（排除 SMOKETEST） |
| 實際快照數 | 20 筆 |
| memory_snapshots 表 | ✅ 首次建立（W21 初始化） |

**索引建立**：
- `idx_snapshots_run_id` ✅
- `idx_snapshots_date` ✅
- `idx_snapshots_memory_id` ✅

> W22 Weekly KPI 執行時，可使用此快照計算 RR(7) 的「精確版本」（以快照 ID 列表 vs 當時 DB 存在與否）。

---

## Step 2 — Retention Rate (RR)

### RR(7) — 7 日保留率

| 項目 | 數值 |
|------|------|
| 參考視窗 | 2026-05-16（7 天前） |
| 原始記憶數 | 2 筆（DB: 2 + Archive: 0） |
| 仍存在 DB | 2 筆 |
| 已歸檔/刪除 | 0 筆 |
| **RR(7)** | **100.0%** ✅ 達標（≥85%） |

**精確召回驗證**（LIKE 搜尋）：

| 記憶 ID | 時間戳 | 搜尋狀態 |
|---------|--------|---------|
| `991ec4a1` | 2026-05-16 04:07:57 | ✅ Found（LIKE token 精確命中） |
| `9f2f5e9a` | 2026-05-16T03:07:19 | ✅ Found（LIKE token 精確命中） |

> 注意：2 筆均為 Daily Smoke Test 測試記憶（SMOKETEST token），內容語意有限。
> 7-day 視窗記憶數偏少（煙霧測試寫入），不足以代表真實業務記憶保留狀況。

### RR(30) — 30 日保留率

| 項目 | 數值 |
|------|------|
| 參考視窗 | 2026-04-21 ~ 2026-04-23（30 天前範圍） |
| 原始記憶數 | 21 筆（DB: 21 + Archive: 0） |
| 仍存在 DB | 21 筆 |
| 已歸檔/刪除 | 0 筆 |
| **RR(30)** | **100.0%** ✅ 達標（≥80%） |

> 30 天內無任何記憶被刪除或歸檔，`memories_archive` 表目前為空。
> 記憶系統尚未啟動自動衰退/壓縮機制，保留率自然為 100%。

---

## Step 3 — Retrieval Accuracy (RA) — Precision@5

### 評測配置

| 項目 | 配置 |
|------|------|
| Golden QA Set 版本 | v1.0（初始化） |
| 題目數 | 10 題 |
| 搜尋方法 | LIKE AND 多項關鍵字（代理語意搜尋） |
| FTS 狀態 | ⚠️ rowid-UUID bug，語意搜尋失效 |

### 各題結果

| QID | 分類 | 狀態 | Hit Rank | 搜尋延遲 | 問題摘要 |
|-----|------|------|----------|---------|---------|
| QA-001 | 競品分析 | ✅ Hit@1 | 1 | 3.62ms | LangGraph Stars 數量 |
| QA-002 | 架構設計 | ✅ Hit@1 | 1 | 0.06ms | Checkpoint TTL 等級 |
| QA-003 | 團隊報告 | ✅ Hit@1 | 1 | 0.04ms | W17 ENG-MEMORY 狀態 |
| QA-004 | 技術選型 | ✅ Hit@1 | 1 | 0.05ms | 仙問 TTS 選型 |
| QA-005 | 技術路線 | ✅ Hit@4 | 4 | 0.05ms | Web3 P0 項目 |
| QA-006 | 架構規格 | ✅ Hit@1 | 1 | 0.07ms | MCP 工具快取 TTL |
| QA-007 | 記憶架構 | ✅ Hit@1 | 1 | 0.05ms | Core Memory 衝突解析 |
| QA-008 | 系統評測 | ✅ Hit@1 | 1 | 0.05ms | Smoke Test 2026-05-10 結果 |
| QA-009 | 進度報告 | ✅ Hit@1 | 1 | 0.04ms | V2 重構進度 97% |
| QA-010 | 架構設計 | ✅ Hit@1 | 1 | 0.05ms | Checkpoint 儲存後端 |

### 匯總指標

| 指標 | 數值 |
|------|------|
| 總題數 | 10 |
| Hits in Top-5 | 10 / 10 |
| **Macro Precision@5** | **100.0%** ✅ 達標（≥75%） |
| 平均搜尋延遲 | 0.41ms（遠低於 P95 < 200ms 標準）|
| 最慢單題 | QA-001 = 3.62ms（較複雜的 LIKE 查詢） |

> **QA-005 命中 Rank=4**（非 Rank=1）：Web3+P0 共召回 5 筆，
> 含 SIWE 的記憶排在第 4（importance=5.0，有多筆 Web3 相關記憶並列）。
> 整體 Precision@5 不受影響。

---

## 搜尋效能統計

| 指標 | 數值 | 目標 | 狀態 |
|------|------|------|------|
| 平均搜尋延遲 | 0.41ms | < 200ms (P95) | ✅ 優秀 |
| 最大搜尋延遲 | 3.62ms | < 200ms (P95) | ✅ 優秀 |
| 最小搜尋延遲 | 0.04ms | — | — |
| 樣本搜尋（LIKE 'LangGraph'） | 0.01ms | — | ✅ 超快 |

> SQLite LIKE 搜尋在 68 筆記憶規模下效能極佳。
> 規模擴增至 10,000+ 筆後需重新評測（預期需加索引或切換向量搜尋）。

---

## 歷史趨勢

| 週次 | 日期 | RR(7) | RR(30) | Precision@5 | 搜尋延遲 | 備注 |
|------|------|-------|--------|-------------|---------|------|
| **W21** | **2026-05-23** | **100%** | **100%** | **100%** | **0.41ms** | **首次執行；LIKE 模式；無歷史快照** |

> 本週為基準週，後續週次將與本週對比觀察趨勢。

---

## 問題分析

### 🔴 高優先問題

#### P1: memories_fts rowid-UUID Join Bug（FTS 語意搜尋失效）

**現象**：FTS 搜尋永遠不返回結果（rowid 整數 vs UUID 字串不匹配）。
**影響**：
- Precision@5 評測只能用 LIKE 代替，結果不代表真實語意召回
- 生產環境語意搜尋可能已靜默失效
**修復優先級**：HIGH（影響核心功能）
**建議**：短期採 FTS content= 外部表重建；長期切換向量後端（pgvector/Chroma）

#### P2: HTTP API 持續不可用（連續第 5 天）

**現象**：`DUDUCLAW_API_URL` 未設定，所有 HTTP live test BLOCKED。
**影響**：無法測試 API 層行為（命名空間隔離、過濾邏輯等）
**需要**：TL 決策明確執行環境

### 🟡 中優先問題

#### P3: RR 計算缺乏精確歷史快照

**現象**：首次執行無 `memory_snapshots` 歷史記錄，RR 採 archive-based 估算。
**影響**：W21 RR 數字偏樂觀（archive 為空即 RR=100%）
**下一步**：W22 起可使用本次快照（run_id=603aa92b）作為精確基準

#### P4: 7-day 視窗記憶品質偏低

**現象**：7 天前（2026-05-16）只有 2 筆 SMOKETEST 記憶。
**影響**：RR(7) 無法代表業務記憶的真實保留狀況
**建議**：調整 RR 視窗策略，改為「最近 7 天內所有新增記憶」而非「特定日期範圍」

### 🟢 低優先問題

#### P5: 記憶衰退/壓縮機制尚未啟動

**現象**：`memories_archive` 為空，所有記憶永久保留。
**影響**：短期無影響；長期（10,000+ 筆）效能將下降
**建議**：W22 設計記憶衰退策略（TTL / importance 閾值 / 語意相似度合併）

---

## 建議行動

| 優先 | 負責 | 行動 | 期限 |
|------|------|------|------|
| 🔴 HIGH | ENG-MEMORY | 修復 `memories_fts` rowid-UUID join bug | W22 |
| 🔴 HIGH | TL 決策 | 確認 DUDUCLAW_API_URL 執行環境 | 即時 |
| 🟡 MED | ENG-MEMORY | W22 起使用 memory_snapshots 精確計算 RR | W22 |
| 🟡 MED | ENG-MEMORY | 調整 RR(7) 視窗策略（改為滾動 7 天新增記憶） | W22 |
| 🟢 LOW | ENG-MEMORY | 設計記憶衰退壓縮機制（TTL/importance 閾值） | W23 |
| 🟢 LOW | ENG-MEMORY | Golden QA Set 擴充至 50 題（含語意難題） | FTS 修復後 |

---

## 時間線

```
2026-05-23T04:00:00Z  排程觸發（locomo-eval-weekly-native-kpis）
2026-05-23T04:00:01Z  環境診斷：SQLite 直連可用，HTTP API BLOCKED（第 5 天）
2026-05-23T04:00:02Z  Step 1：建立 memory_snapshots 表（首次初始化）
2026-05-23T04:00:03Z  Step 1：Snapshot 20 筆高重要性記憶（run_id=603aa92b）
2026-05-23T04:00:04Z  Step 2：RR(7) 計算 → 100.0%（2/2 records，archive=0）
2026-05-23T04:00:05Z  Step 2：RR(30) 計算 → 100.0%（21/21 records，archive=0）
2026-05-23T04:00:06Z  Step 3：Golden QA Set v1.0 初始化（10 題）
2026-05-23T04:00:07Z  Step 3：Precision@5 評測 → 100.0%（10/10 hits）
2026-05-23T04:00:08Z  Step 4：週報寫入 wiki/reports/memory-quality/2026-05/weekly-kpis-2026-05-23.md
2026-05-23T04:00:09Z  Step 4：activity_post 回報 TL
```

---

## 新增發現摘要（本週）

🆕 **W21 Weekly KPI 系統首次啟動**：memory_snapshots 表建立，Golden QA Set v1.0 初始化，評測框架上線。

✅ **全部 KPI 達標**：RR(7)=100%、RR(30)=100%、Precision@5=100%、延遲 0.41ms。

⚠️ **LIKE 搜尋模式警告**：所有結果基於 LIKE AND 搜尋，FTS rowid-UUID bug 使真實語意召回能力無法評估。

📋 **兩份新文件產出**：`wiki/eval/golden-qa-set.md`（v1.0）+ `wiki/impl/w21-locomo-eval-implementation.md`（v1.0）。

---

*由 ENG-MEMORY 自動產生 — 2026-05-23（W21 Weekly Native KPIs）*
