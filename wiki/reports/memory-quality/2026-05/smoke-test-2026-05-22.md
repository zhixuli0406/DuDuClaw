# LOCOMO Daily Smoke Test — 2026-05-22

> 執行者：ENG-MEMORY (duduclaw-eng-memory)
> 執行時間：2026-05-22 UTC（排程觸發）
> Spec 參考：specs/locomo-memory-eval-spec-v1.md §4.2
> 實作參考：wiki/impl/w21-locomo-eval-implementation.md §3

---

## 執行摘要

| 狀態 | 說明 |
|------|------|
| 整體狀態 | ⚠️ BLOCKED — 環境未就緒，Live API 無法連接 |
| 程式邏輯驗證 | ✅ 23/23 unit tests PASSED |
| Live API 驗證 | ❌ SKIPPED — `DUDUCLAW_API_URL` 未設定 |

---

## 環境診斷

### 阻礙原因

Live smoke test 需透過 `HttpMemoryClient` 連接 DuDuClaw API，但以下必要環境變數均未設定：

```
DUDUCLAW_API_URL  → (未設定)
DUDUCLAW_API_KEY  → (未設定)
DATABASE_DSN      → (未設定)
```

- 本地 HTTP 探測（port 8765, 8080, 3000）：**無回應（connection refused）**
- `.env` 檔案：**不存在**（僅有 `.env.example`）
- 結論：本機執行環境無可用 DuDuClaw API server

### 已確認的環境變數（DUDUCLAW 前綴）

```
DUDUCLAW_DELEGATION_SENDER=duduclaw-eng-memory
DUDUCLAW_DELEGATION_DEPTH=0
DUDUCLAW_HOME=/Users/lizhixu/.duduclaw
DUDUCLAW_DELEGATION_ORIGIN=cron
```

---

## Test Case 執行結果

### ✅ 程式邏輯驗證（Unit Tests，Mock Client）

執行環境：Python 3.11.15 / pytest-9.0.3 / pytest-asyncio-1.3.0
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
執行時間：8.28s

---

### ❌ Live API Smoke Test（無法執行）

| TC | 名稱 | 狀態 | 原因 |
|----|------|------|------|
| TC-1 | basic_store_and_retrieve | ❌ BLOCKED | API server 未啟動 |
| TC-2 | memory_isolation | ❌ BLOCKED | API server 未啟動 |
| TC-3 | episodic_pressure_check | ❌ BLOCKED | API server 未啟動 |

---

## 與歷史紀錄比較

| 日期 | Unit Tests | Live API | 備注 |
|------|------------|----------|------|
| 2026-05-16 | N/A | ✅ 2/3 PASS, 1 SKIP | 成功連接 8765（dev server 在線） |
| 2026-05-20 | 23/23 ✅ | ❌ BLOCKED | API server 離線 |
| **2026-05-22** | **23/23 ✅** | **❌ BLOCKED** | API server 離線，同 05-20 |

---

## 問題分析與建議行動

### 根本原因

本機執行環境無 DuDuClaw API server。從歷史紀錄看，2026-05-16 成功是因為 `duduclaw http-server` 在本地 port 8765 運行；2026-05-20 至今持續 BLOCKED，表明 dev server 未重新啟動，或 smoke test 執行環境缺少 server 啟動步驟。

### 持續阻礙（第 3 天）

自 2026-05-20 起連續 3 次 Daily Smoke Test 無法執行 Live API 驗證，屬 **長期 blocker**，建議升級為 TL 決策事項。

### 建議行動（依優先度）

1. **[TL 決策 - 高優先]** 明確 smoke test 的執行環境定義：
   - Option A：CI/CD pipeline 對 staging 環境執行（需 DUDUCLAW_API_URL 指向 staging）
   - Option B：本機開發需先啟動 `duduclaw http-server`，並加入 cron 前置步驟
   - Option C：新增「離線模式」，改用 SQLite 直連取代 HTTP API（無需 server）

2. **[INFRA]** 如選 Option A，需補充 CI secrets：`DUDUCLAW_API_URL`、`DUDUCLAW_API_KEY`

3. **[ENG-MEMORY]** 如選 Option C，可於 W22 sprint 開發 `SqliteMemoryClient` 直接讀取 `~/.duduclaw/memory.db`，讓 smoke test 在任何環境均可執行 TC-1、TC-3（TC-2 isolation 仍需 namespace 支援）

---

## 時間線

```
2026-05-22T??:??:??Z  排程觸發（DUDUCLAW_DELEGATION_ORIGIN=cron）
2026-05-22T??:??:??Z  環境診斷：DUDUCLAW_API_URL 未設定，ports 8765/8080/3000 無回應
2026-05-22T??:??:??Z  改執行 unit tests（mock client）
2026-05-22T??:??:??Z  23/23 unit tests PASSED in 8.28s（Python 3.11.15）
2026-05-22T??:??:??Z  報告寫入 wiki/reports/memory-quality/2026-05/smoke-test-2026-05-22.md
2026-05-22T??:??:??Z  通知 TL（連續 3 天 Live API BLOCKED）
```

---

## 下一步

- [ ] **[TL]** 決策 smoke test 執行環境方向（A/B/C）
- [ ] 補充 `.env.example` 加入 `DUDUCLAW_API_URL` / `DUDUCLAW_API_KEY`
- [ ] W22 評估 `SqliteMemoryClient` 實作（Option C 離線模式）
- [ ] Live API 恢復後驗證 TC-1~3 全部通過

---

*由 ENG-MEMORY 自動產生 — 2026-05-22*
