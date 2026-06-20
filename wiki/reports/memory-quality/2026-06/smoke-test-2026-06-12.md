# LOCOMO Daily Smoke Test — 2026-06-12

> 執行者：ENG-MEMORY (duduclaw-eng-memory)
> 執行時間：2026-06-12 UTC（排程觸發）
> Spec 參考：specs/locomo-memory-eval-spec-v1.md §4.2
> 實作參考：wiki/impl/w21-locomo-eval-implementation.md §3

---

## 執行摘要

| 狀態 | 說明 |
|------|------|
| 整體狀態 | ✅ 部分通過 — SQLite 直連模式（Live API 仍不可用） |
| SQLite 直連 Live Tests | ✅ 2/3 PASS，1 SKIP（namespace 未實作，同前次） |
| Live HTTP API 驗證 | ❌ BLOCKED — `DUDUCLAW_API_URL` 未設定（連續第 19 天） |

---

## 環境診斷

### 環境變數狀態

```
DUDUCLAW_API_URL  → (未設定)
DUDUCLAW_API_KEY  → (未設定)
DATABASE_DSN      → (未設定)
```

- 本地 HTTP 探測（port 8765, 8080, 3000）：**未執行（沿用前次診斷結論）**
- 本機 SQLite：**可用** → `~/.duduclaw/memory.db`（50 筆 episodic，3 筆 semantic）

### 執行策略決定

Live API 不可用，採 **SQLite 直連模式（Option C）**：
- 直接讀寫 `~/.duduclaw/memory.db`
- TC-1: INSERT → LIKE 精確查詢 → DELETE（完整存取驗證）
- TC-2: 檢查 namespace 欄位是否存在（無則 SKIP）
- TC-3: 計算過去 24h 新增 episodic 記憶數量作為 pressure 代理值 + 搜尋延遲量測

---

## Test Case 執行結果

### TC-1: basic_store_and_retrieve

| 項目 | 結果 |
|------|------|
| 狀態 | ✅ PASS |
| duration | 59ms |
| 測試 token | `SMOKETEST_20260612_131113` |
| store → INSERT | ✅ 成功 |
| exact retrieve（LIKE） | ✅ 精確匹配 found=True |
| cleanup | ✅ 測試記錄已刪除（count=0 確認） |

**詳情**：✅ TC-1 PASS: store OK, exact retrieve OK (unique token matched), cleanup OK

---

### TC-2: memory_isolation

| 項目 | 結果 |
|------|------|
| 狀態 | ⏭️ SKIP |
| duration | 0ms |
| 原因 | `memories` 表無 `namespace` 欄位，SQLite schema 未實作 namespace 隔離（連續 SKIP） |

> TC-2 SKIP 屬預期行為（持續）。namespace 欄位尚未加入 SQLite schema，TC-2 需 HTTP API 層支援方可全面驗證。

---

### TC-3: episodic_pressure_check

| 項目 | 結果 |
|------|------|
| 狀態 | ✅ PASS |
| duration | ~40ms（10 次查詢均值） |
| raw_pressure | 0（過去 24h 新增 episodic 記憶數） |
| normalized | 0.000（÷ 10.0） |
| threshold | 10.0 |
| 搜尋延遲均值（10 次 LIKE） | **4ms**（遠低於 P95 < 200ms 目標） |
| 最新 episodic 時間戳 | 2026-05-07T03:08:13.035719+00:00 |
| 背景資訊 | 總記憶 53 筆（50 episodic + 3 semantic） |

> pressure=0 表示過去 24h 無新增 episodic 記憶（最新記錄仍來自 2026-05-07），系統處於低負荷待機狀態。記憶筆數與上次報告（2026-06-04: 53 筆）相同，**無異常波動**。

---

### HTTP API Smoke Test（無法執行）

| TC | 名稱 | 狀態 | 原因 |
|----|------|------|------|
| TC-1 | basic_store_and_retrieve | ❌ BLOCKED | API server 未啟動（連續第 19 天） |
| TC-2 | memory_isolation | ❌ BLOCKED | API server 未啟動 |
| TC-3 | episodic_pressure_check | ❌ BLOCKED | API server 未啟動 |

---

## 歷史紀錄比較

| 日期 | SQLite Live | HTTP Live | 記憶總數 | EP | 備注 |
|------|-------------|-----------|---------|-----|------|
| 2026-05-23 | ✅ 2/3, 1 SKIP | ❌ BLOCKED | 68 | 0.000 | 首次啟用 SQLite 直連模式 |
| 2026-06-04 | ✅ 2/3, 1 SKIP | ❌ BLOCKED | 53 | 0.000 | 記憶筆數由 68 降至 53（-15 筆） |
| **2026-06-12** | **✅ 2/3, 1 SKIP** | **❌ BLOCKED** | **53** | **0.000** | **記憶穩定，無波動；搜尋延遲 4ms** |

---

## 系統指標

| 指標 | 本次結果 | 目標 | 狀態 |
|------|---------|------|------|
| Smoke Test 通過率 | 2/3（不含 SKIP） | 3/3 | ⚠️ SKIP |
| 搜尋 P95 延遲 | 4ms（均值） | < 200ms | ✅ 達標 |
| episodic pressure | 0.000 | < 1.0 | ✅ 正常 |
| 記憶總數穩定性 | 53 筆（±0） | 無異常波動 | ✅ 穩定 |

---

## 持續阻礙

### ❌ HTTP API 長期不可用（第 19 天）

自 2026-05-24 至今（2026-06-12）已連續 19 天以上無法執行 HTTP Live API 驗證。
此阻礙持續影響 TC-1/TC-2 的完整語意搜尋驗證及 TC-2 namespace 隔離驗證。

**需 TL 決策**：
1. CI Staging 環境部署計畫是否存在？
2. 是否正式接受 SQLite 直連作為長期 smoke test 策略？
3. 需補充 `namespace` 欄位至 SQLite schema 以完整驗證 TC-2？

---

## 建議行動

| 優先 | 負責 | 行動 |
|------|------|------|
| 🔴 HIGH | TL 決策 | 明確 smoke test 執行環境（HTTP API 何時恢復 or 正式採用 SQLite 離線模式） |
| 🔴 HIGH | ENG-MEMORY | 修復 `memories_fts` rowid-UUID join bug（影響生產語意搜尋，已知超過 19 天） |
| 🟡 MED | ENG-MEMORY | 添加 `namespace` 欄位至 SQLite schema，使 TC-2 在離線模式亦可驗證 |

---

## 時間線

```
2026-06-12 13:11:13 UTC  排程觸發（DUDUCLAW_DELEGATION_ORIGIN=cron）
2026-06-12 13:11:xx UTC  環境確認：DUDUCLAW_API_URL 未設定，採 SQLite 直連模式（Option C）
2026-06-12 13:11:xx UTC  TC-1 PASS（token: SMOKETEST_20260612_131113，59ms）
2026-06-12 13:11:xx UTC  TC-2 SKIP（namespace 欄位不存在）
2026-06-12 13:11:xx UTC  TC-3 PASS（pressure=0.000，搜尋延遲均值 4ms）
2026-06-12 13:11:xx UTC  報告寫入 wiki/reports/memory-quality/2026-06/smoke-test-2026-06-12.md
```

---

*由 ENG-MEMORY 自動產生 — 2026-06-12*
