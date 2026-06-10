# LOCOMO Daily Smoke Test — 2026-05-23

> 執行者：ENG-MEMORY (duduclaw-eng-memory)
> 執行時間：2026-05-23 UTC（排程觸發）
> Spec 參考：specs/locomo-memory-eval-spec-v1.md §4.2
> 實作參考：wiki/impl/w21-locomo-eval-implementation.md §3

---

## 執行摘要

| 狀態 | 說明 |
|------|------|
| 整體狀態 | ✅ 部分通過 — SQLite 直連模式（Live API 仍不可用） |
| 程式邏輯驗證（Unit Tests） | ✅ 23/23 PASSED |
| SQLite 直連 Live Tests | ✅ 2/3 PASS，1 SKIP（namespace 未實作） |
| Live HTTP API 驗證 | ❌ BLOCKED — `DUDUCLAW_API_URL` 未設定（連續第 4 天） |

---

## 環境診斷

### 環境變數狀態

```
DUDUCLAW_API_URL  → (未設定)
DUDUCLAW_API_KEY  → (未設定)
DATABASE_DSN      → (未設定)
```

- 本地 HTTP 探測（port 8765, 8080, 3000）：**無回應（curl exit code 7 = connection refused）**
- `.env` 檔案：**不存在**
- 本機 SQLite：**可用** → `~/.duduclaw/memory.db`（65 筆 episodic，3 筆 semantic）

### 執行策略決定

Live API 不可用，改採 **SQLite 直連模式**（Option C 預研路線）：
- 直接讀寫 `~/.duduclaw/memory.db`
- TC-1: INSERT → LIKE 精確查詢 → DELETE（完整存取驗證）
- TC-2: 檢查 namespace 欄位是否存在（無則 SKIP）
- TC-3: 計算過去 24h 新增 episodic 記憶數量作為 pressure 代理值

---

## Test Case 執行結果

### ✅ 程式邏輯驗證（Unit Tests，Mock Client）

執行指令：`uv run --with pytest --with pytest-asyncio pytest python/duduclaw/memory_eval/tests/test_smoke_test.py -v --tb=short`

| Test ID | 測試案例 | 狀態 |
|---------|----------|------|
| TC-1 | basic_store_and_retrieve — 正常通過 | ✅ PASS |
| TC-1 | basic_store_and_retrieve — search 回傳空 → FAIL 邏輯 | ✅ PASS |
| TC-1 | basic_store_and_retrieve — store 例外處理 | ✅ PASS |
| TC-1 | duration_ms 正確記錄 | ✅ PASS |
| TC-2 | memory_isolation — 無洩漏 → PASS 邏輯 | ✅ PASS |
| TC-2 | memory_isolation — 洩漏偵測 → FAIL 邏輯 | ✅ PASS |
| TC-2 | memory_isolation — NotImplementedError → SKIP 邏輯 | ✅ PASS |
| TC-2 | general exception → ERROR 邏輯 | ✅ PASS |
| TC-3 | episodic_pressure_check — 正常 float 0.50 | ✅ PASS |
| TC-3 | episodic_pressure_check — 高壓力值 9.5 | ✅ PASS |
| TC-3 | episodic_pressure_check — 零值 0.0 | ✅ PASS |
| TC-3 | episodic_pressure_check — 負數 → FAIL 邏輯 | ✅ PASS |
| TC-3 | episodic_pressure_check — 例外處理 | ✅ PASS |
| TC-3 | 正規化值 ≤ 1.0（raw=15.0 → 1.000） | ✅ PASS |
| TC-3 | 非數值回傳 → ValueError 邏輯 | ✅ PASS |
| Integration | run_smoke_test — 全部通過情境 | ✅ PASS |
| Integration | run_smoke_test — 部分失敗情境 | ✅ PASS |
| Integration | report.summary 格式驗證 | ✅ PASS |
| Integration | report 包含 3 個結果 | ✅ PASS |
| Integration | report.run_id 為 UUID 格式 | ✅ PASS |
| Integration | skipped_count 正確遞增 | ✅ PASS |
| Unit | SmokeTestResult dataclass 屬性 | ✅ PASS |
| Unit | SmokeTestReport.all_passed 屬性邏輯 | ✅ PASS |

**總計：23/23 PASSED（0 failed, 0 errors）**
執行時間：8.11s

---

### ✅ SQLite 直連 Live Tests

執行環境：Python 3.11 / sqlite3 / `~/.duduclaw/memory.db`
總執行時間：518ms

#### TC-1: basic_store_and_retrieve

| 項目 | 結果 |
|------|------|
| 狀態 | ✅ PASS |
| duration | 514ms（含 0.5s sleep） |
| 測試 token | `SMOKETEST_1748040xxx`（動態產生） |
| store → INSERT | ✅ 成功，id=UUID |
| exact retrieve（LIKE） | ✅ 精確匹配 found=True |
| FTS retrieve | ⚠️ found=False（見附注） |
| cleanup | ✅ 測試記錄已刪除 |

**附注 — FTS Join Bug 發現**：`memories_fts` 使用 `rowid` 與 `memories` 做 JOIN，但 `memories.id` 是 TEXT UUID（無 implicit rowid 對應），導致 FTS 查詢無法返回結果。此為 schema 設計問題，已記錄（見問題分析節）。

#### TC-2: memory_isolation

| 項目 | 結果 |
|------|------|
| 狀態 | ⏭️ SKIP |
| duration | 1ms |
| 原因 | `memories` 表無 `namespace` 欄位，SQLite schema 未實作 namespace 隔離 |

> TC-2 SKIP 屬預期行為。HTTP API 層可能在 `/memory/store` / `/memory/search` 的 payload 中支援 `namespace`，但 SQLite DB 層尚未實作對應欄位。

#### TC-3: episodic_pressure_check

| 項目 | 結果 |
|------|------|
| 狀態 | ✅ PASS |
| duration | 1ms |
| raw_pressure | 0.00（過去 24h 新增 episodic 記憶數） |
| normalized | 0.000（÷ 10.0） |
| threshold | 10.0 |
| 背景資訊 | 總 episodic 記憶 65 筆，avg importance=5.12，最新記憶時間戳 2026-05-21 |

> pressure=0.00 表示過去 24h 無新增 episodic 記憶（最新記錄來自 2026-05-21），系統處於低負荷待機狀態。

---

### ❌ Live HTTP API Smoke Test（無法執行）

| TC | 名稱 | 狀態 | 原因 |
|----|------|------|------|
| TC-1 | basic_store_and_retrieve | ❌ BLOCKED | API server 未啟動（連續第 4 天） |
| TC-2 | memory_isolation | ❌ BLOCKED | API server 未啟動 |
| TC-3 | episodic_pressure_check | ❌ BLOCKED | API server 未啟動 |

---

## 歷史紀錄比較

| 日期 | Unit Tests | SQLite Live | HTTP Live | 備注 |
|------|------------|-------------|-----------|------|
| 2026-05-16 | N/A | N/A | ✅ 2/3 PASS, 1 SKIP | dev server 在線（port 8765） |
| 2026-05-20 | 23/23 ✅ | N/A | ❌ BLOCKED | API server 離線 |
| 2026-05-22 | 23/23 ✅ | N/A | ❌ BLOCKED | API server 離線 |
| **2026-05-23** | **23/23 ✅** | **2/3 ✅, 1 SKIP** | **❌ BLOCKED** | **首次啟用 SQLite 直連模式** |

---

## 問題分析

### Bug: memories_fts rowid-UUID 不匹配

**現象**：`memories_fts` FTS 表使用 SQLite 內建 rowid，但 `memories.id` 是 TEXT UUID PRIMARY KEY。JOIN 條件 `memories.id = memories_fts.rowid` 永遠不匹配（型別不同），導致 FTS 查詢無效。

**影響**：
- SQLite 層 FTS 語意搜尋失效（只能用 LIKE exact match）
- 若 HTTP API 內部也依賴此 FTS 表做語意搜尋，召回率可能嚴重受損

**建議修正方案**：
```sql
-- Option A: 在 memories 表加 INTEGER rowid 對應欄位
-- Option B: 在 memories_fts 表使用 content="" + external content，以 id 作為 docid
-- Option C: 建立獨立 FTS trigger，自動同步 TEXT id → FTS rowid mapping
```

> **優先級建議：HIGH** — 影響核心語意召回功能

### 持續阻礙（第 4 天）：HTTP API Unavailable

自 2026-05-20 起連續 4 次 Daily Smoke Test 無法執行 HTTP Live API 驗證，已屬長期 blocker。

---

## 建議行動

| 優先 | 負責 | 行動 |
|------|------|------|
| 🔴 HIGH | ENG-MEMORY | 修復 `memories_fts` rowid-UUID join bug（影響生產語意搜尋） |
| 🔴 HIGH | TL 決策 | 明確 smoke test 執行環境（Option A:CI staging / B:本機需啟動 server / C:SQLite 離線模式） |
| 🟡 MED | INFRA | 如選 Option A，補充 CI secrets：`DUDUCLAW_API_URL`、`DUDUCLAW_API_KEY` |
| 🟡 MED | ENG-MEMORY | 正式化 `SqliteMemoryClient`（目前 SQLite 直連是 ad-hoc inline script，應封裝為正式 client） |
| 🟢 LOW | ENG-MEMORY | 添加 `namespace` 欄位至 SQLite schema，讓 TC-2 isolation 在離線模式亦可驗證 |

---

## 時間線

```
2026-05-23T??:??:??Z  排程觸發（DUDUCLAW_DELEGATION_ORIGIN=cron）
2026-05-23T??:??:??Z  環境診斷：DUDUCLAW_API_URL 未設定，ports 8765/8080/3000 無回應
2026-05-23T??:??:??Z  Unit Tests 23/23 PASSED（8.11s）
2026-05-23T??:??:??Z  改採 SQLite 直連模式（Option C 預研驗證）
2026-05-23T??:??:??Z  TC-1 PASS / TC-2 SKIP / TC-3 PASS（518ms total）
2026-05-23T??:??:??Z  發現 memories_fts rowid-UUID join bug（FTS 語意搜尋失效）
2026-05-23T??:??:??Z  報告寫入 wiki/reports/memory-quality/2026-05/smoke-test-2026-05-23.md
2026-05-23T??:??:??Z  通知 TL（連續 4 天 HTTP API BLOCKED + FTS bug 發現）
```

---

## 新增發現摘要（本日）

🆕 **首次啟用 SQLite 直連模式**：在 HTTP API 不可用時成功執行部分 live 驗證（TC-1 + TC-3），為 Option C 方案可行性提供實證。

🐛 **memories_fts rowid-UUID join bug**：FTS 全文搜尋在 SQLite 層完全失效，應優先修復。

---

*由 ENG-MEMORY 自動產生 — 2026-05-23*
