---
title: "HandoffPacket 規格 v0.2 — Progress Ledger 停滯偵測擴充"
created: 2026-04-27T00:00:00Z
updated: 2026-04-27T00:00:00Z
status: approved
approver: Agnes
tags: [w18-p0, spec, handoffpacket, progress-ledger, stagnation, a2a, multi-agent]
layer: deep
trust: 1.0
changelog:
  - version: v0.2
    date: 2026-04-27
    author: duduclaw-tl
    changes: "Agnes 指令（2026-04-27）：加入 stagnation_count + task_ledger_reset Progress Ledger 欄位，借鑑 MagenticOne AutoGen MAF 機制。W18-P0 必交付項目。"
  - version: v0.1
    date: 2026-04-25
    author: duduclaw-pm
    changes: "W17 完成：Agent 間數據傳遞協議草案，HandoffPacket 基礎欄位定義"
---

# HandoffPacket 規格 v0.2

> **審批狀態**：Agnes 指令核准 ✅（2026-04-27）
> **版本**：v0.2（Progress Ledger 擴充）
> **前版本**：v0.1（W17 完成，基礎 Agent 間傳遞協議草案）
> **W18-P0 優先級**：本文件為本週必交付，TL 負責今日完成
> **借鑑來源**：MagenticOne / AutoGen MAF — Progress Ledger 機制（2026-04-27 競品研究）

---

## 1. 設計背景

### 1.1 v0.1 → v0.2 升級動機

**問題**：HandoffPacket v0.1 提供了 Agent 間基本的數據傳遞協議，但缺乏**任務停滯偵測機制**：

- 當 Agent 在多輪循環中重複嘗試相同方向而未取得有意義進展時，系統無法感知
- Orchestrator 無法判斷何時應該重新規劃任務路徑（重新規劃 vs. 繼續等待）
- 任務死鎖（task deadlock）的偵測依賴 timeout，粒度過粗

**解決方案來源**：MagenticOne（AutoGen MAF）的 Progress Ledger 機制：
- 透過 `stagnation_count` 計數有意義進展的缺失次數
- 當 `stagnation_count >= stagnation_threshold` 時，標記 `task_ledger_reset=true`
- Orchestrator 接收 reset 信號後觸發重新規劃，而非繼續執行已停滯的路徑

### 1.2 Agnes 指令（2026-04-27）

> 在 HandoffPacket v0.2 schema 加入 `stagnation_count + task_ledger_reset`。
> 觸發邏輯：`stagnation_count >= threshold` → `task_ledger_reset=true` → 重規劃。

---

## 2. v0.2 完整 Schema

### 2.1 HandoffPacket 頂層結構

```json
{
  "packet_id": "hp-2026-04-27-001",
  "protocol_version": "0.2",
  "from_agent": "duduclaw-worker-1",
  "to_agent": "duduclaw-orchestrator",
  "task_id": "task-abc-123",
  "created_at": "2026-04-27T10:00:00Z",
  "payload": { ... },
  "progress_ledger": {
    "stagnation_count": 2,
    "stagnation_threshold": 3,
    "task_ledger_reset": false,
    "last_meaningful_progress": "2026-04-27T09:45:00Z",
    "progress_checkpoint": "Successfully fetched API response, parsing failed"
  },
  "metadata": {
    "confidence": 0.72,
    "attempt_number": 3,
    "execution_time_ms": 1850
  }
}
```

### 2.2 `progress_ledger` 欄位定義（v0.2 新增）

| 欄位 | 型別 | 必填 | 預設值 | 說明 |
|------|------|------|--------|------|
| `stagnation_count` | int（≥0） | ✅ | `0` | 連續「無有意義進展」的回合計數。每次 Orchestrator 判定 Worker 無進展時 +1；有意義進展時歸零 |
| `stagnation_threshold` | int（≥1） | ✅ | `3` | 停滯判定門檻。當 `stagnation_count >= stagnation_threshold` 時，自動設定 `task_ledger_reset=true` |
| `task_ledger_reset` | bool | ✅ | `false` | Orchestrator 重新規劃信號。`true` 時 Orchestrator **必須**放棄當前路徑並重新規劃任務 |
| `last_meaningful_progress` | ISO8601 string \| null | — | `null` | 最後一次有意義進展的 UTC 時間戳記。用於診斷停滯持續時間 |
| `progress_checkpoint` | string \| null | — | `null` | 最後有意義進展的人類可讀描述（建議 < 200 字元）。幫助重規劃時快速恢復任務上下文 |

### 2.3 `progress_ledger` 範例場景

**場景 A：正常執行（stagnation_count = 0）**
```json
{
  "progress_ledger": {
    "stagnation_count": 0,
    "stagnation_threshold": 3,
    "task_ledger_reset": false,
    "last_meaningful_progress": "2026-04-27T10:00:00Z",
    "progress_checkpoint": "Database query completed, 42 records retrieved"
  }
}
```

**場景 B：停滯累積中（stagnation_count = 2）**
```json
{
  "progress_ledger": {
    "stagnation_count": 2,
    "stagnation_threshold": 3,
    "task_ledger_reset": false,
    "last_meaningful_progress": "2026-04-27T09:30:00Z",
    "progress_checkpoint": "API fetch attempt failed, retry in progress"
  }
}
```

**場景 C：停滯觸發重規劃（task_ledger_reset = true）**
```json
{
  "progress_ledger": {
    "stagnation_count": 3,
    "stagnation_threshold": 3,
    "task_ledger_reset": true,
    "last_meaningful_progress": "2026-04-27T09:30:00Z",
    "progress_checkpoint": "Last success: API fetch. Three consecutive parse failures."
  }
}
```

---

## 3. 觸發邏輯與 Orchestrator 協議

### 3.1 stagnation_count 更新規則

**Worker 端（每次回傳 HandoffPacket 時執行）**：

```
PROCEDURE update_progress_ledger(prev_ledger, current_result):

  IF current_result.has_meaningful_progress():
    # 有意義進展：歸零 + 更新 checkpoint
    new_ledger.stagnation_count = 0
    new_ledger.task_ledger_reset = false
    new_ledger.last_meaningful_progress = current_result.timestamp
    new_ledger.progress_checkpoint = current_result.describe()

  ELSE:
    # 無有意義進展：累計
    new_ledger.stagnation_count = prev_ledger.stagnation_count + 1
    new_ledger.last_meaningful_progress = prev_ledger.last_meaningful_progress
    new_ledger.progress_checkpoint = prev_ledger.progress_checkpoint

  # 自動設定 reset flag
  IF new_ledger.stagnation_count >= prev_ledger.stagnation_threshold:
    new_ledger.task_ledger_reset = true

  RETURN new_ledger
```

### 3.2 「有意義進展」的判定標準

以下情形視為**有有意義進展**（stagnation_count 歸零）：

| 情形 | 範例 |
|------|------|
| 完成任一子任務 | 成功讀取檔案、API 呼叫成功、資料庫寫入完成 |
| 取得新資訊 | 搜尋到前一回合未知的資料 |
| 狀態轉換 | 任務從「分析」進入「執行」階段 |
| 錯誤診斷完成 | 雖然失敗，但已識別失敗原因（新資訊）|

以下情形視為**無有意義進展**（stagnation_count +1）：

| 情形 | 範例 |
|------|------|
| 重複相同失敗 | 第 N 次執行相同查詢且得到相同錯誤 |
| 工具呼叫無回應 | Timeout 或空回應 |
| 輸出與前輪相同 | 生成完全相同的內容 |
| 明確宣告卡住 | `"I'm not sure how to proceed"` |

### 3.3 Orchestrator 接收 `task_ledger_reset=true` 的強制行為

```
PROCEDURE orchestrator_handle_packet(packet):

  IF packet.progress_ledger.task_ledger_reset == true:
    # 強制重規劃流程
    LOG "Task reset triggered. stagnation_count={}, last_progress={}, checkpoint={}"
    
    EMIT evolution_event(
      event_type="signal_suppressed",
      trigger_signal="task_stagnation",
      metadata={
        "stagnation_count": packet.progress_ledger.stagnation_count,
        "last_checkpoint": packet.progress_ledger.progress_checkpoint,
        "task_id": packet.task_id
      }
    )
    
    CALL replan_task(
      task_id=packet.task_id,
      context=packet.progress_ledger.progress_checkpoint,
      failed_approach=packet.payload.last_approach
    )
    
    RETURN new_plan

  ELSE:
    # 正常繼續執行
    ROUTE packet TO next_worker
```

> ⚠️ **強制要求**：Orchestrator 收到 `task_ledger_reset=true` 時**不得**繼續沿用當前執行路徑。必須先觸發重規劃，再啟動新執行。

---

## 4. v0.1 → v0.2 向後相容性

### 4.1 向後相容保證

- v0.1 HandoffPacket（無 `progress_ledger` 欄位）被 v0.2 Orchestrator 接收時：
  - `progress_ledger` 反序列化為 `None` / 預設初始值
  - **不觸發** stagnation 計數或重規劃
  - v0.2 Orchestrator 以 `stagnation_count=0` 初始化新的 `progress_ledger`

- v0.2 HandoffPacket 被 v0.1 Orchestrator 接收時：
  - `progress_ledger` 欄位被忽略（不識別欄位向後相容）
  - 功能退化為 v0.1 行為（無停滯偵測），但不報錯

### 4.2 遷移路徑

```
Phase 1（今日）：HandoffPacket v0.2 Schema 設計鎖定（本文件）
Phase 2（W18-W19）：Worker Agent 實作 progress_ledger 更新邏輯
Phase 3（W19）：Orchestrator 實作 task_ledger_reset 處理流程
Phase 4（W19 QA）：整合測試，含停滯觸發場景的端對端驗證
```

---

## 5. 與 A2A 協議的對齊

（本節對應 Agnes 指令中 HandoffPacket 與 A2A 對齊章節）

### 5.1 對齊關係

| HandoffPacket v0.2 概念 | A2A 對應概念 |
|------------------------|-------------|
| `stagnation_count` | A2A Task Status 中的 retry/failure count |
| `task_ledger_reset` | A2A `TaskResubscriptionRequest`（重新訂閱觸發重規劃）|
| `progress_checkpoint` | A2A Streaming Artifacts（中間結果保存）|
| Orchestrator 重規劃流程 | A2A Task 重新分派（new TaskRequest）|

### 5.2 A2A Agent Card 展望（W19 P1）

- HandoffPacket `progress_ledger` 的語義可映射為 A2A Task State 的擴展欄位
- W19 A2A Agent Card 發布時，`stagnation_count` / `task_ledger_reset` 可作為 DuDuClaw 私有擴展欄位
- A2A 原生 `TaskStatus` 不包含此機制，我們的 Progress Ledger 是差異化設計

---

## 6. 配置（agent.toml）

```toml
[handoff.progress_ledger]
default_stagnation_threshold = 3    # 全域預設停滯門檻（可被 HandoffPacket 欄位覆蓋）
max_stagnation_threshold     = 10   # 安全上限，防止過大閾值造成長期停滯
replan_cooldown_seconds      = 60   # 重規劃後的冷卻期（防止重規劃風暴）
emit_evolution_event         = true # 停滯觸發時是否同步發射 EvolutionEvent
```

---

## 7. 測試要求（W19 QA 前置）

| 測試場景 | 期望行為 |
|---------|---------|
| Worker 連續 3 次無進展 | `stagnation_count=3`，`task_ledger_reset=true` |
| 第 2 次後有進展 | `stagnation_count` 歸零，`task_ledger_reset=false` |
| Orchestrator 接收 reset=true | 強制重規劃，不繼續原路徑 |
| v0.1 Packet 被 v0.2 Orchestrator 接收 | 不報錯，`progress_ledger` 以預設值初始化 |
| `stagnation_threshold` 被設定為 1 | 任何一次無進展即觸發重規劃（允許但記錄警告） |
| 重規劃後 `stagnation_count` 歸零 | 重規劃觸發後新任務路徑的 `progress_ledger` 必須重置 |

---

## 8. 開放問題（W18 追蹤）

| 問題 | 負責人 | 期望解決時間 |
|------|--------|------------|
| `has_meaningful_progress()` 的 ML 判斷實作（純規則 vs. 嵌入式模型評分） | ENG-AGENT | W19 設計評審 |
| 多層 Orchestrator 階層的 `stagnation_count` 應在哪層重置？ | TL | W18-W19 架構評審 |
| A2A 正式對齊時 `task_ledger_reset` 映射到哪個 A2A field？ | PM | W19 A2A 規劃時 |

---

## 參照

- MagenticOne Progress Ledger 競品分析：`shared/wiki/competitive/magentic-one-progress-ledger.md`（PM W18 任務）
- Agnes 指令：`research/daily/2026-04-27-agnes-response.md` §2
- HandoffPacket v0.1 草案：W17 PM 交付物（oral spec，待整理至 wiki）
- A2A 協議研究：`research/pm-daily/2026-04-26-daily-research.md`
- Evolution Events 停滯偵測：`specs/evolution-events-spec-v1.md` §4

---

*設計者：TL-DuDuClaw*
*審批人：Agnes（指令核准，2026-04-27）*
*版本：v0.2 | 日期：2026-04-27*
*前版本：v0.1（W17，PM 草案）*
*W18-P0 必交付項目 — 今日完成*
