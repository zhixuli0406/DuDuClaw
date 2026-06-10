# LOCOMO Daily Smoke Test — 2026-05-20

> 執行者：ENG-MEMORY (duduclaw-eng-memory)
> 執行時間：2026-05-20 UTC（排程觸發）
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

- 本地 HTTP 探測（port 8080, 3000）：**無回應**
- `.env` 檔案：**不存在**（僅有 `.env.example`）
- 結論：本機開發環境尚未對接任何 DuDuClaw API server

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

使用 `MockMemoryClient` 以隔離方式執行 23 個 unit test，模擬三個 TC 的各種情境：

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
執行時間：8.14s（Python 3.12, pytest-9.0.2）

---

### ❌ Live API Smoke Test（無法執行）

| TC | 名稱 | 狀態 | 原因 |
|----|------|------|------|
| TC-1 | basic_store_and_retrieve | ❌ BLOCKED | API server 未啟動 |
| TC-2 | memory_isolation | ❌ BLOCKED | API server 未啟動 |
| TC-3 | episodic_pressure_check | ❌ BLOCKED | API server 未啟動 |

---

## 問題分析與建議行動

### 根本原因

本地開發環境沒有設定 DuDuClaw API server。Smoke test 的設計目的是驗證**生產或 staging 環境**的記憶系統健康狀態，但目前沒有任何環境變數指向可用的 API endpoint。

### 建議行動（優先度：高）

1. **[TL 確認]** smoke test 的執行環境定義：
   - 應在 CI/CD pipeline 中對 staging 環境執行？
   - 還是在 local dev 環境對本地 docker-compose 執行？

2. **[INFRA]** 確認 `DUDUCLAW_API_URL` 和 `DUDUCLAW_API_KEY` 應從哪裡取得，並補充到 `.env.example`

3. **[ENG-MEMORY]** 如果 smoke test 不打算對 live API 執行，考慮新增一個「離線模式」，改用 in-memory mock client 執行 TC-1~3，讓排程任務在沒有 API 的環境也能回報有意義的結果

---

## 時間線

```
2026-05-20T??:??:??Z  排程觸發（DUDUCLAW_DELEGATION_ORIGIN=cron）
2026-05-20T??:??:??Z  環境診斷：DUDUCLAW_API_URL 未設定
2026-05-20T??:??:??Z  改執行 unit tests（mock client）
2026-05-20T??:??:??Z  23/23 unit tests PASSED in 8.14s
2026-05-20T??:??:??Z  報告寫入 wiki/reports/memory-quality/2026-05/smoke-test-2026-05-20.md
2026-05-20T??:??:??Z  通知 TL
```

---

## 下一步

- [ ] TL 確認 smoke test 的執行環境設定方式
- [ ] 補充 `.env.example` 加入 `DUDUCLAW_API_URL` / `DUDUCLAW_API_KEY` / `DATABASE_DSN`
- [ ] 決定是否為無 API 環境新增離線 smoke test 模式
- [ ] 下次排程執行時對 live API 驗證

---

*由 ENG-MEMORY 自動產生*
