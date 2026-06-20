---
title: "W18-P1 交付報告：Reflexion Loop 記憶系統需求評估"
created: 2026-04-29T00:00:00Z
updated: 2026-04-29T00:00:00Z
status: draft
author: duduclaw-eng-memory
tags: [w18-p1, memory, reflexion-loop, episodic, semantic, report]
layer: deep
trust: 0.9
---

# Reflexion Loop 記憶需求評估報告

> **任務**：[W18-P1] 評估 Reflexion Loop 對記憶系統 API 的新增需求  
> **截止**：W18 Day5（2026-05-02）  
> **狀態**：⚠️ **部分完成** — TL 設計文件 `architecture/reflexion-loop-design.md` 尚未收到，本報告基於現有技術背景進行預評估，待收到設計文件後補充最終確認

---

## 1. Reflexion Loop 技術背景

### 1.1 什麼是 Reflexion Loop？

Reflexion 是 Shinn et al.（2023）提出的技術框架，讓 LLM-Agent 透過**語言反思（verbal reflection）**提升任務效能，核心機制：

```
Task Attempt（任務執行）
        ↓
Evaluator / Critic（評估結果）
        ↓
Reflexion Engine（生成 reflection_note）
        ↓  ← 存入 Memory
Next Attempt（改良後重試）
        ↑
Load relevant reflection_notes（從 Memory 載入相關反思）
```

在 DuDuClaw 語境中，**`critic_recalibration`** 對應 Evaluator/Critic 角色：Agent 完成任務後，Critic 對輸出品質打分，並觸發 Reflexion Engine 生成 `reflection_note`。

### 1.2 `reflection_note` 的資訊結構

`reflection_note` 預計包含以下語義：

```json
{
  "reflection_id": "ref-20260429-001",
  "task_id": "task-abc-123",
  "task_type": "code_review",
  "critic_score": 0.62,
  "failure_reason": "遺漏安全性檢查項目，未覆蓋 XSS 注入場景",
  "corrective_insight": "下次執行 code_review 類任務時，需額外呼叫 security-reviewer 子 Agent",
  "applicable_conditions": ["task_type:code_review", "language:typescript"],
  "created_at": "2026-04-29T10:00:00Z",
  "validity_confidence": 0.85
}
```

---

## 2. `reflection_note` 應存入哪個記憶層？

### 2.1 三層記憶層語義對照

| 記憶層 | 語義 | 範例 |
|--------|------|------|
| **Episodic** | 具體事件、情境記憶（What happened） | 「今天 code_review 任務失敗了」|
| **Semantic** | 通用知識、規則、原則（What is true in general） | 「code_review 任務必須覆蓋安全性檢查」|
| **Procedural** | 技能、行動流程（How to do） | 「執行 code_review 的標準步驟」|

### 2.2 推薦架構：Episodic 優先 → 整合至 Semantic

**建議採用「雙層寫入」策略：**

```
critic_recalibration 完成
        ↓
Step 1：寫入 Episodic Memory（即時，原始記錄）
  content: "任務 task-abc-123 code_review 失敗，critic_score=0.62，
            原因：未覆蓋 XSS 場景。矯正洞察：需呼叫 security-reviewer"
  tags: [reflexion, task_type:code_review, critic_recalibration]
  layer: episodic
  ↓
Step 2（非同步，觸發 Consolidation）：
  若同類型 reflection_note 累積 ≥ 3 筆，觸發 Memory Consolidation
  → 提取通用規則 → 寫入 Semantic Memory
  → 通用化知識："code_review 類任務需強制覆蓋安全性子任務"
```

**理由分析：**

| 方案 | 優點 | 缺點 |
|------|------|------|
| 僅存 Episodic | 保留完整情境；可精確追蹤 | 積累後檢索效率下降；難以抽象化 |
| 僅存 Semantic | 通用性強；查詢快 | 失去具體情境；過早抽象化有噪聲 |
| **Episodic → Consolidation → Semantic** | 兼顧原始記錄與通用規則；與現有 Consolidation 機制整合 | 需實作觸發邏輯；有延遲 |

→ **推薦：Episodic 優先，達到 consolidation 閾值後提升至 Semantic**

---

## 3. Reflexion Loop 對記憶 API 的新增需求

### 3.1 新需求列表

| 需求 | 說明 | 現有支援？ |
|------|------|----------|
| **N1. 帶標籤的 Episodic 寫入** | `memory_store` 需支援 `task_id`、`task_type`、`critic_score` 等結構化 metadata | ⚠️ 部分（需確認現有 `tags` 欄位是否足夠）|
| **N2. 條件化 Consolidation 觸發** | 相同 `task_type` 的 reflection_note ≥ N 筆時，自動觸發 Consolidation | ❌ 目前 Consolidation 似為定期執行，非條件觸發 |
| **N3. 反思記憶召回** | 下次執行同類任務前，搜尋相關 `reflection_note` | ⚠️ `memory_search` 可語義搜尋，但無 `task_type` 精確過濾 |
| **N4. Reflexion 有效期管理** | 舊 reflection_note 可能因情境改變而失效，需 `valid_until` 支援 | ❌ 現有記憶無時態欄位（見 Part 3 Temporal KG）|
| **N5. Critic Score 關聯存儲** | 需能查詢「低 critic_score 的反思記憶」以優先學習 | ❌ 現有不支援數值欄位過濾 |

### 3.2 最小可行 API 擴充（MVP for Reflexion）

**對現有 `memory_store` 的最小擴充：**

```json
{
  "content": "任務 task-abc-123 失敗，矯正洞察：...",
  "tags": "reflexion,task_type:code_review,critic_recalibration",
  "metadata": {
    "task_id": "task-abc-123",
    "task_type": "code_review",
    "critic_score": 0.62,
    "reflexion_type": "corrective",
    "applicable_conditions": ["task_type:code_review"]
  }
}
```

**對 `memory_search` 的最小擴充：**

```
memory_search(
    query="code_review security check",
    filter_tags=["reflexion", "task_type:code_review"],
    min_recency_days=30  // 只查近期 reflexion，避免載入過舊策略
)
```

---

## 4. W18 工時評估

| 項目 | 工時估算 | 說明 |
|------|---------|------|
| Reflexion 寫入路徑（memory_store metadata 擴充） | 1d | 欄位擴充 + 測試 |
| 條件化 Consolidation 觸發邏輯 | 2d | 複雜度較高 |
| Reflexion 召回優化（tag filter） | 0.5d | memory_search 擴充 |
| **合計** | **3.5d** | |

**W18 工時評估：❌ W18 無法承接**

W18 剩餘工時不足以完成上述需求，且 TL 設計文件（`architecture/reflexion-loop-design.md`）尚未收到，無法確認完整需求範圍。

→ **建議推遲至 W19 實作，W19 估時 3.5d，建議列為 W19-P1**

---

## 5. 阻塞項與待確認事項

| 阻塞項 | 說明 | 需誰解決 |
|--------|------|---------|
| 🔴 **TL 設計文件未收到** | `architecture/reflexion-loop-design.md` 尚未提供，無法確認 critic_recalibration 的完整觸發條件與輸出格式 | TL |
| 🟡 **memory_store metadata 欄位規格** | 現有 `memory_store` 的 metadata 結構是否已支援結構化 KV 存儲，需確認後端 schema | ENG-MEMORY（自查）|
| 🟡 **Consolidation 觸發機制** | 現有 Consolidation 是定期執行還是條件觸發？若為定期，需評估增加條件觸發的工程量 | ENG-MEMORY |

---

## 6. 結論

| 問題 | 答案 |
|------|------|
| Reflexion Loop 需要哪個記憶層？ | **Episodic 優先**，達閾值後整合至 Semantic |
| `reflection_note` 存入路徑 | `memory_store` + episodic layer + 結構化 metadata |
| W18 能否承接 Reflexion 記憶寫入？ | ❌ **否**，推 W19-P1 |
| 是否阻塞其他 W18 工作？ | 否，阻塞的是 W19 Reflexion Loop 功能，不影響 W18 現有任務 |
| 主要阻塞項 | TL 設計文件未收到；memory_store metadata 格式待確認 |

---

## 參照

- `research/memory/zep-vs-current.md` — 記憶分層分析
- `specs/checkpoint-schema-v0.1-mvp.md` — memory_refs 設計
- `reports/w18-p1-memory-api-batch-fetch-report.md` — Part 1：Batch Fetch 確認
- `reports/w18-p1-temporal-kg-schema-draft.md` — Part 3：Temporal KG Schema 草案
- Shinn et al. (2023). "Reflexion: Language Agents with Verbal Reinforcement Learning"

---

*作者：ENG-MEMORY（duduclaw-eng-memory）*  
*日期：2026-04-29*  
*版本：v1.0（預評估版，待 TL 設計文件補充）*
