# Golden QA Set — LOCOMO Memory Retrieval Accuracy

> 版本：v1.0
> 建立日期：2026-05-23
> 建立者：ENG-MEMORY（首次 W21 Weekly KPI 執行時初始化）
> 用途：Retrieval Accuracy（RA）評測 — 計算 Precision@5
> 評測目標：Macro Precision@5 ≥ 75%

---

## 概述

Golden QA Set 包含 10 道問答題，基於 DuDuClaw 記憶系統中實際儲存的記憶內容設計。
每道問題需使用關鍵字搜尋（LIKE / 語意搜尋），在 Top-5 結果中找到包含 `expected_keyword` 的記憶。

**評測方法（W21 基準）**：由於 `memories_fts` FTS 表存在 rowid-UUID join bug，
語意搜尋失效，本輪採 **LIKE 關鍵字多項 AND 搜尋**作為代理評測方法。
修復 FTS bug 後，應切換為向量語意搜尋（embedding cosine similarity）。

---

## 題目列表

| QID | 分類 | 問題 | 搜尋關鍵詞 | 期望包含字 |
|-----|------|------|-----------|-----------|
| QA-001 | 競品分析 | LangGraph v1.1.6 的 GitHub Stars 數量是多少？ | LangGraph, Stars | 126,000 |
| QA-002 | 架構設計 | DuDuClaw Checkpoint Schema 的 TTL 策略有哪些等級？ | Checkpoint, TTL | ephemeral |
| QA-003 | 團隊報告 | W17 TL Daily Report 中 ENG-MEMORY 的狀態為何？ | W17, ENG-MEMORY | missing |
| QA-004 | 技術選型 | 仙問 TTS 選型裁決選擇了哪個 TTS 服務？ | TTS, 選型 | 騰訊雲 |
| QA-005 | 技術路線 | DuDuClaw 與 Web3 整合的 P0 優先項目有哪些？ | Web3, P0 | SIWE |
| QA-006 | 架構規格 | MCP A2A Bridge 規格中 MCP Client 工具快取 TTL 為多少秒？ | MCP, TTL, 快取 | 300 |
| QA-007 | 記憶架構 | 記憶系統整合評估報告中 Core Memory 的衝突解析策略為何？ | Core Memory, 衝突 | NEWER_WINS |
| QA-008 | 系統評測 | LOCOMO Daily Smoke Test W21 在 2026-05-10 的執行結果？ | LOCOMO, Smoke Test, 2026-05-10 | 全部通過 |
| QA-009 | 進度報告 | 仙問 V2 重構進度於 2026-04-24 的完成百分比？ | V2, 重構 | 97% |
| QA-010 | 架構設計 | Checkpoint Schema 儲存後端採用什麼雙軌策略？ | Checkpoint, SQLite, Redis | Redis |

---

## 詳細題目定義

### QA-001：LangGraph Stars 數量

- **分類**：競品分析
- **問題**：LangGraph v1.1.6 的 GitHub Stars 數量是多少？
- **搜尋關鍵詞**：`["LangGraph", "Stars"]`
- **期望包含字**：`126,000`
- **參考記憶 ID**：`e153ee03-ee4a-410d-885d-9755de6413fe`
- **參考記憶來源**：duduclaw-tl，2026-04-21，層級=episodic，importance=7.0

### QA-002：Checkpoint TTL 等級

- **分類**：架構設計
- **問題**：DuDuClaw Checkpoint Schema 的 TTL 策略有哪些等級？
- **搜尋關鍵詞**：`["Checkpoint", "TTL"]`
- **期望包含字**：`ephemeral`（代表四個等級之一）
- **參考記憶 ID**：`a26c0483-0d40-4035-91de-...`（ENG-MEMORY 2026-04-21）
- **完整答案**：ephemeral(1h) / session(24h) / persistent(30d) / permanent

### QA-003：ENG-MEMORY W17 狀態

- **分類**：團隊報告
- **問題**：W17 TL Daily Report 中 ENG-MEMORY 的狀態為何？
- **搜尋關鍵詞**：`["W17", "ENG-MEMORY"]`
- **期望包含字**：`missing`
- **參考記憶 ID**：`0e6261e3-f98a-421c-9fa7-c956d889d9db`
- **完整答案**：CRITICAL — ENG-MEMORY missing for 3 consecutive days

### QA-004：仙問 TTS 選型

- **分類**：技術選型
- **問題**：仙問 TTS 選型裁決選擇了哪個 TTS 服務？
- **搜尋關鍵詞**：`["TTS", "選型"]`
- **期望包含字**：`騰訊雲`
- **參考記憶 ID**：`499d5a46-...`（xianwen-pm 2026-04-29）
- **完整答案**：騰訊雲 TTS 精品版（有條件通過，需執行 Audio POC）

### QA-005：Web3 整合 P0

- **分類**：技術路線
- **問題**：DuDuClaw 與 Web3 整合的 P0 優先項目有哪些？
- **搜尋關鍵詞**：`["Web3", "P0"]`
- **期望包含字**：`SIWE`
- **參考記憶 ID**：`57682bdb-...`（duduclaw-tl 2026-04-21）
- **完整答案**：Token-gated Access（ERC-1155）+ SIWE 身份驗證（EIP-4361）

### QA-006：MCP 工具快取 TTL

- **分類**：架構規格
- **問題**：MCP A2A Bridge 規格中 MCP Client 工具快取 TTL 為多少秒？
- **搜尋關鍵詞**：`["MCP", "TTL", "快取"]`
- **期望包含字**：`300`（秒）
- **參考記憶 ID**：`61d553b4-...`（duduclaw-pm 2026-05-01）
- **完整答案**：5 分鐘（300 秒）

### QA-007：Core Memory 衝突解析

- **分類**：記憶架構
- **問題**：記憶系統整合評估報告中 Core Memory 的衝突解析策略為何？
- **搜尋關鍵詞**：`["Core Memory", "衝突"]`
- **期望包含字**：`NEWER_WINS`
- **參考記憶 ID**：`9409d2d3-...`（duduclaw-eng-memory 2026-04-21）

### QA-008：Smoke Test 2026-05-10 結果

- **分類**：系統評測
- **問題**：LOCOMO Daily Smoke Test W21 在 2026-05-10 的執行結果？
- **搜尋關鍵詞**：`["LOCOMO", "Smoke Test", "2026-05-10"]`
- **期望包含字**：`全部通過`
- **參考記憶 ID**：`920a959e-...`（duduclaw-eng-memory 2026-05-10）

### QA-009：仙問 V2 重構進度

- **分類**：進度報告
- **問題**：仙問 V2 重構進度於 2026-04-24 的完成百分比？
- **搜尋關鍵詞**：`["V2", "重構"]`
- **期望包含字**：`97%`
- **參考記憶 ID**：`70aa14fc-...`（agnes 2026-04-24）

### QA-010：Checkpoint 儲存後端

- **分類**：架構設計
- **問題**：Checkpoint Schema 儲存後端採用什麼雙軌策略？
- **搜尋關鍵詞**：`["Checkpoint", "SQLite", "Redis"]`
- **期望包含字**：`Redis`
- **參考記憶 ID**：`a26c0483-...`（duduclaw-eng-memory 2026-04-21）
- **完整答案**：SQLite 主 + Redis 熱快照雙軌架構

---

## 評測歷史

| 週次 | 日期 | 方法 | Precision@5 | 狀態 |
|------|------|------|------------|------|
| W21 | 2026-05-23 | LIKE AND 多項搜尋 | 100.0% (10/10) | ✅ 達標（≥75%） |

> **注意**：W21 評測採 LIKE 搜尋（FTS rowid-UUID bug 未修復），結果偏樂觀。
> 修復 FTS 後重新使用語意搜尋評測，預期 Precision@5 會有所下降（基準待校準）。

---

## 版本演進計劃

| 版本 | 計劃 |
|------|------|
| v1.0 (現在) | 10 題 LIKE 搜尋基準；FTS bug 導致語意搜尋代替 |
| v1.1 (FTS 修復後) | 改用語意向量搜尋；重新校準基準值 |
| v2.0 (Q3 2026) | 擴充至 50 題；加入跨層（episodic/semantic）混合查詢 |

---

*由 ENG-MEMORY 自動產生 — 2026-05-23（W21 Weekly KPI 首次執行）*
