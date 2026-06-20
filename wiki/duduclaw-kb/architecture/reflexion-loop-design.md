---
title: "Reflexion Loop 架構設計文件 v0.1"
created: 2026-04-29T00:00:00Z
updated: 2026-04-29T00:00:00Z
status: approved
author: duduclaw-tl
tags: [architecture, reflexion-loop, memory, critic, w19-p1]
layer: deep
trust: 1.0
---

# Reflexion Loop 架構設計 v0.1

> **文件目的**：提供 ENG-MEMORY 完成 W18-P1 Part 2（Reflexion Loop 記憶需求評估）所需的 TL 設計意圖  
> **狀態**：v0.1 核心決策確定，細節實作規格於 W19-P1 啟動時補充  
> **關聯任務**：[W19-P1] 生產可靠性三柱強化 → Critic Recalibration 子系統

---

## 1. Reflexion Loop 在 DuDuClaw 中的定位

```
Task Execution（任務執行）
        ↓
critic_recalibration（品質評估，由 Critic Agent 執行）
        ↓ [score < 0.70 → 觸發]
Reflexion Engine（生成 reflection_note）
        ↓
memory_store → Episodic Layer（即時記錄）
        ↓ [累積 ≥ 3 筆同 task_type]
Memory Consolidation → Semantic Layer（抽象化規則）
        ↑
Next Task Attempt 開始前：memory_search 載入相關反思
```

**DuDuClaw 不採用「同任務強制重試」模型**（與 Shinn et al. 原始 Reflexion 不同）。  
我們的模型是「**跨任務跨 Session 的漸進式學習**」：reflection_note 在下一次執行同類任務時被召回，而不是立即重試。

---

## 2. 觸發條件（TL 裁定）

### 2.1 主觸發器

| 條件 | 說明 |
|------|------|
| `critic_score < 0.70` | Critic Agent 評分低於 70% 時自動觸發 |
| 任務明確失敗（error/exception） | 即使無 critic_score，也觸發 Reflexion |
| 人工標記（manual flag） | 主帥或 TL 可手動觸發特定任務的 Reflexion |

### 2.2 防無限循環保護

- 同一個 `task_id` 的 Reflexion Loop 最多執行 **3 輪**
- 超過 3 輪仍未改善 → 升級為人工介入（PushNotification → Agnes / TL）
- 此計數器存入 task metadata，不佔用 Memory 配額

---

## 3. `reflection_note` 輸出格式（TL 裁定）

```json
{
  "reflection_id": "ref-{uuid}",
  "task_id": "task-{id}",
  "task_type": "code_review | planning | memory_design | ...",
  "critic_score": 0.62,
  "failure_mode": "missing_step | quality_issue | security_gap | scope_drift | tool_misuse",
  "failure_summary": "遺漏安全性檢查項目，未覆蓋 XSS 注入場景",
  "corrective_insight": "下次執行 code_review 類任務時，需額外呼叫 security-reviewer 子 Agent",
  "applicable_conditions": [
    "task_type:code_review",
    "language:typescript"
  ],
  "confidence": 0.78,
  "valid_from": "2026-04-29T10:00:00Z",
  "valid_until": null
}
```

### 3.1 `failure_mode` 枚舉說明

| 值 | 語義 |
|----|------|
| `missing_step` | 任務流程中遺漏必要步驟 |
| `quality_issue` | 輸出品質不足（語言、格式、完整性）|
| `security_gap` | 安全性覆蓋不足 |
| `scope_drift` | 執行範圍偏離原始需求 |
| `tool_misuse` | 工具選用錯誤或參數錯誤 |

---

## 4. 記憶存儲路徑（TL 裁定）

### 4.1 寫入規格（對 ENG-MEMORY 的 API 需求確認）

**Step 1：即時寫入 Episodic Layer**

```python
memory_store(
    content="{failure_summary}。矯正洞察：{corrective_insight}",
    layer="episodic",
    tags=[
        "reflexion",
        f"task_type:{task_type}",
        "critic_recalibration",
        f"failure_mode:{failure_mode}"
    ],
    metadata={
        "task_id": task_id,
        "task_type": task_type,
        "critic_score": critic_score,
        "reflexion_type": "corrective",
        "applicable_conditions": applicable_conditions,
        "confidence": confidence,
        "reflection_id": reflection_id
    },
    source="reflexion"
)
```

**Step 2：Consolidation 觸發條件（非同步）**

- 當相同 `task_type` 的 `reflexion` tagged 記憶累積 **≥ 3 筆**時
- 觸發 Memory Consolidation → 提取通用規則 → 寫入 Semantic Layer
- Semantic 節點的 `supersedes` 指向被整合的 Episodic 節點群組

### 4.2 有效期設計

- 所有 `reflection_note` 預設有效期 **30 天**（`valid_until = created_at + 30d`）
- 被 Consolidation 整合後：`valid_until = consolidation_time`，`superseded_by = semantic_node_id`
- 理由：舊策略可能因環境變化而不再適用，避免過時反思影響後續決策

---

## 5. 召回（Retrieval）設計

**在每次 Task 執行前**，Orchestrator 應：

```python
reflexion_context = memory_search(
    query=f"{task_type} task execution guidance",
    filter_tags=["reflexion", f"task_type:{task_type}"],
    min_recency_days=30,    # 只召回近 30 天的 reflexion（防舊策略干擾）
    limit=3                  # 最多注入 3 條反思記憶（控制 context 開銷）
)
```

召回結果注入 Task Prompt 的 **System Context** 區塊，格式：

```
[Past Reflexion Insights for {task_type}]
1. {corrective_insight_1}（confidence: {conf}）
2. {corrective_insight_2}
...
```

---

## 6. 對 ENG-MEMORY N1-N5 需求的 TL 裁定

| ENG-MEMORY 需求 | TL 裁定 |
|----------------|---------|
| **N1. 帶 metadata 的 Episodic 寫入** | ✅ 確認需要。`memory_store` 的 `metadata` 欄位需支援結構化 JSON KV；W19-P1 Day 1 實作 |
| **N2. 條件化 Consolidation 觸發** | ✅ 確認需要。實作「同 task_type reflexion ≥ 3 觸發」邏輯；與現有 Consolidation 系統整合 |
| **N3. Tag-based 反思召回** | ✅ 確認需要。`memory_search` 加入 `filter_tags` + `min_recency_days` 參數 |
| **N4. `valid_until` 有效期管理** | ✅ 確認需要。納入 Part 3 Temporal KG Schema（30 天 TTL）|
| **N5. `critic_score` 數值過濾** | ⚠️ **降低優先級**。W19-P1 MVP 不實作；用 tag 代替（e.g. `score:low` for < 0.5）；W20 再評估數值過濾 |

---

## 7. Phase 計劃

| Phase | 內容 | 時間 |
|-------|------|------|
| **W19-P1 MVP**（ENG-MEMORY 主導） | N1 metadata 擴充、N2 條件觸發、N3 tag 召回 | W19 Week 2 |
| **W19-P1 整合** | Orchestrator 召回邏輯注入 Task Prompt | W19 Week 2 |
| **W20** | Temporal KG 完整整合（valid_until、supersession chain）| W20 |
| **W20+** | N5 數值過濾、Reflexion 效果指標追蹤 | W20+ |

---

## 參照

- `reports/w18-p1-reflexion-loop-report.md` — ENG-MEMORY 評估報告（本文件為其 unblock 文件）
- `reports/w18-p1-temporal-kg-schema-draft.md` — Temporal KG Schema（N4 依賴項）
- Shinn et al. (2023). "Reflexion: Language Agents with Verbal Reinforcement Learning"
- `specs/w19-p1-reliability-three-pillars.md` — W19-P1 整體規格（Critic Recalibration 子系統）

---

*作者：TL-DuDuClaw（duduclaw-tl）*  
*日期：2026-04-29*  
*版本：v0.1（核心決策確定；細節實作規格於 W19-P1 啟動時補充）*
