# LOCOMO Daily Smoke Test — 2026-06-04

> 執行者：ENG-MEMORY (duduclaw-eng-memory)
> 執行時間：2026-06-04 UTC（排程觸發）
> Spec 參考：specs/locomo-memory-eval-spec-v1.md §4.2
> 實作參考：wiki/impl/w21-locomo-eval-implementation.md §3

---

## 執行摘要

| 狀態 | 說明 |
|------|------|
| 整體狀態 | ✅ 部分通過 — SQLite 直連模式（Live API 仍不可用） |
| SQLite 直連 Live Tests | ✅ 2/3 PASS，1 SKIP（namespace 未實作） |
| Live HTTP API 驗證 | ❌ BLOCKED — `DUDUCLAW_API_URL` 未設定（連續第 12 天） |

---

## 環境診斷

### 環境變數狀態

```
DUDUCLAW_API_URL  → (未設定)
DUDUCLAW_API_KEY  → (未設定)
DATABASE_DSN      → (未設定)
```

- 本地 HTTP 探測（port 8765, 8080, 3000）：**無回應（connection refused）**
- 本機 SQLite：**可用** → `~/.duduclaw/memory.db`（50 筆 episodic，3 筆 semantic）

### 執行策略決定

Live API 不可用，採 **SQLite 直連模式**（Option C）：
- 直接讀寫 `~/.duduclaw/memory.db`
- TC-1: INSERT → LIKE 精確查詢 → DELETE（完整存取驗證）
- TC-2: 檢查 namespace 欄位是否存在（無則 SKIP）
- TC-3: 計算過去 24h 新增 episodic 記憶數量作為 pressure 代理值

---

## Test Case 執行結果

### TC-1: basic_store_and_retrieve

| 項目 | 結果 |
|------|------|
| 狀態 | ✅ PASS |
| duration | 5ms |
| 測試 token | `SMOKETEST_1748995xxx`（動態產生） |
| store → INSERT | ✅ 成功，id=UUID |
| exact retrieve（LIKE） | ✅ 精確匹配 found=True |
| cleanup | ✅ 測試記錄已刪除 |

**詳情**：✅ TC-1 PASS: store OK, exact retrieve OK (unique token matched)

---

### TC-2: memory_isolation

| 項目 | 結果 |
|------|------|
| 狀態 | ⏭️ SKIP |
| duration | 0ms |
| 原因 | `memories` 表無 `namespace` 欄位，SQLite schema 未實作 namespace 隔離 |

> TC-2 SKIP 屬預期行為（連續）。namespace 欄位尚未加入 SQLite schema，TC-2 需 HTTP API 層支援方可全面驗證。

---

### TC-3: episodic_pressure_check

| 項目 | 結果 |
|------|------|
| 狀態 | ✅ PASS |
| duration | 0ms |
| raw_pressure | 0.00（過去 24h 新增 episodic 記憶數） |
| normalized | 0.000（÷ 10.0） |
| threshold | 10.0 |
| 背景資訊 | 總 episodic 記憶 50 筆，最新記憶時間戳 2026-05-07 03:08:13 |

> pressure=0.00 表示過去 24h 無新增 episodic 記憶（最新記錄來自 2026-05-07），系統處於低負荷待機狀態。記憶筆數自上次報告（65 筆）下降至 50 筆（-15 筆，可能有記憶清理/歸檔操作）。

---

### HTTP API Smoke Test（無法執行）

| TC | 名稱 | 狀態 | 原因 |
|----|------|------|------|
| TC-1 | basic_store_and_retrieve | ❌ BLOCKED | API server 未啟動（連續第 12 天） |
| TC-2 | memory_isolation | ❌ BLOCKED | API server 未啟動 |
| TC-3 | episodic_pressure_check | ❌ BLOCKED | API server 未啟動 |

---

## 歷史紀錄比較

| 日期 | Unit Tests | SQLite Live | HTTP Live | 備注 |
|------|------------|-------------|-----------|------|
| 2026-05-16 | N/A | N/A | ✅ 2/3 PASS, 1 SKIP | dev server 在線（port 8765） |
| 2026-05-20 | 23/23 ✅ | N/A | ❌ BLOCKED | API server 離線 |
| 2026-05-22 | 23/23 ✅ | N/A | ❌ BLOCKED | API server 離線 |
| 2026-05-23 | 23/23 ✅ | ✅ 2/3, 1 SKIP | ❌ BLOCKED | 首次啟用 SQLite 直連模式 |
| **2026-06-04** | — | **✅ 2/3, 1 SKIP** | **❌ BLOCKED** | **記憶筆數由 68 降至 53（50 episodic + 3 semantic）** |

---

## 異常觀察

### ⚠️ 記憶筆數下降

上次報告（2026-05-23）：65 episodic + 3 semantic = 68 筆
本次報告（2026-06-04）：50 episodic + 3 semantic = 53 筆

差異：-15 筆 episodic（約 23% 減少）

可能原因：
1. 記憶壓縮/整合（memory consolidation）流程執行
2. TTL 自動清理
3. 手動刪除操作

建議：查閱 `memories_archive` 表確認是否有 archived 記憶，釐清原因。

---

## 持續阻礙

### ❌ HTTP API 長期不可用（第 12 天）

自 2026-05-23 至今已連續 12 天以上無法執行 HTTP Live API 驗證。
此阻礙持續影響 TC-1/TC-2 的完整語意搜尋驗證及 TC-2 namespace 隔離驗證。

**需 TL 決策**：
1. CI Staging 環境部署計畫是否存在？
2. 是否接受 SQLite 直連作為長期 smoke test 策略？
3. 需補充 `namespace` 欄位至 SQLite schema 以完整驗證 TC-2？

---

## 建議行動

| 優先 | 負責 | 行動 |
|------|------|------|
| 🔴 HIGH | TL 決策 | 明確 smoke test 執行環境（HTTP API 何時恢復 or 正式採用 SQLite 離線模式） |
| 🔴 HIGH | ENG-MEMORY | 調查 episodic 記憶筆數由 65 降至 50 的原因（`memories_archive` 查詢） |
| 🔴 HIGH | ENG-MEMORY | 修復 `memories_fts` rowid-UUID join bug（影響生產語意搜尋，已知超過 12 天） |
| 🟡 MED | ENG-MEMORY | 添加 `namespace` 欄位至 SQLite schema，使 TC-2 在離線模式亦可驗證 |

---

## 時間線

```
2026-06-04T??:??:??Z  排程觸發（DUDUCLAW_DELEGATION_ORIGIN=cron）
2026-06-04T??:??:??Z  環境診斷：DUDUCLAW_API_URL 未設定，ports 8765/8080/3000 無回應
2026-06-04T??:??:??Z  改採 SQLite 直連模式（Option C）
2026-06-04T??:??:??Z  TC-1 PASS / TC-2 SKIP / TC-3 PASS（5ms total）
2026-06-04T??:??:??Z  發現記憶筆數下降（65→50 episodic）
2026-06-04T??:??:??Z  報告寫入 wiki/reports/memory-quality/2026-06/smoke-test-2026-06-04.md
2026-06-04T??:??:??Z  通知 TL（HTTP API 連續第 12 天 BLOCKED + 記憶筆數異常下降）
```

---

*由 ENG-MEMORY 自動產生 — 2026-06-04*
