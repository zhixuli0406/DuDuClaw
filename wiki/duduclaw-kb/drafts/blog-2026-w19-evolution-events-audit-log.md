---
title: "當 AI Agent 開始進化，你怎麼知道它做了什麼？——DuDuClaw EvolutionEvents 架構設計"
created: 2026-04-25T00:00:00Z
updated: 2026-04-25T00:00:00Z
status: draft_v1
author: duduclaw-marketing
task_id: 625128db-d5c6-4b94-94b9-d5149d6cdf63
tags: [blog, draft, w19, evolution-events, gep, evolver, architecture, audit-log]
layer: draft
trust: 0.8
review_status: pending_agnes
target_audience: AI工程師、Agent開發者社群
word_count_estimate: ~2400
---

# 當 AI Agent 開始進化，你怎麼知道它做了什麼？

## DuDuClaw EvolutionEvents：可審計 AI 演化日誌的設計全紀錄

---

想像這個場景：你的 AI Agent 系統昨晚自動停用了某個 Skill，今天早上你打開日誌，卻一片空白。

它為什麼停用？是效能不足？安全掃描踢出？還是 GVU 修復循環中途放棄了？你完全不知道。

這就是我們在 DuDuClaw 剛交付的 Sprint N P0 之前，真實面臨的問題。

---

## 一、為什麼 AI Agent 需要可審計的演化歷史？

DuDuClaw 是一個多 Agent 協作系統，核心能力由「Skill」組成——每個 Skill 是一個可執行的能力單元，包含程式碼執行邊界、安全掃描規則，以及 GVU（Generator-Verifier-Updater）演化循環。

系統設計從一開始就支援動態行為：Skill 可以被啟用、停用、安全掃描，GVU 循環會自動嘗試修復失敗的 Skill，甚至可以「世代遞進」地演化出更好的版本。

問題在於：**這些行為原本是完全不留痕跡的。**

當一個 Skill 被停用，原因消失了。當 GVU 循環失敗三次，沒有任何記錄。當系統陷入 Repair Loop（對同一個錯誤無限觸發修復），我們甚至無從偵測。

對工程師來說，這是噩夢；對想要做合規審查的場景，這更是致命傷。

我們需要的不只是「logs」，而是一套**結構化的演化審計系統**——能夠回溯每一次演化決策的完整脈絡。

---

## 二、業界怎麼做？認識 GEP（Genome Evolution Protocol）

在決定自己設計之前，我們先研究了業界現有的方案。

這時候我注意到了 **Evolver**（[EvoMap/evolver](https://github.com/EvoMap/evolver)）——一個本週剛爆發的開源專案，週增 **+4,032 ⭐**，目前已累積 6,800+ Stars。它的核心協議叫做 **GEP（Genome Evolution Protocol）**，是一套專為 AI Agent 演化設計的結構化框架。

GEP 的設計非常有意思，它用了生物學隱喻：

- **Genes（基因）**：最小可複用的 Prompt 修復模式，對應特定錯誤信號的 Prompt 片段
- **Capsules（膠囊）**：封裝多個 Genes 的複合修復單元
- **EvolutionEvents**：記錄每次演化行為的審計事件

GEP 預設了四種全域演化策略（`balanced`、`innovate`、`harden`、`repair-only`），支援信號去重與 Repair Loop 防護，還有可選的 GEP Hub 進行跨實例同步。

從「可審計演化歷程」這個角度來看，GEP 提供了我們需要的核心概念驗證。**那，為什麼我們沒有直接移植它？**

---

## 三、為什麼不整體移植 GEP？

這是本篇文章的核心問題，也是我們在 ADR-001 架構決策中花最多篇幅論證的地方。

答案一句話：**DuDuClaw Skill 語義比 GEP Genes 更豐富，整體移植 GEP 反而是退步。**

讓我展開說明四個關鍵理由。

### 理由一：語義層次差了一個數量級

GEP 的 Genes 是「Prompt 修復碎片」——本質上是一段 Prompt 文字，對應特定錯誤信號的修復片段。

DuDuClaw 的 Skills 是「可執行的能力單元」——包含程式碼執行、安全掃描邊界、sandbox 試驗流程、GVU 世代循環……

這不只是「複雜程度」的差異，而是**語義本質的差異**。把 Skills 壓縮成 Genes 的語義，就像把一個能跑程式、做安全審查的系統，降格成一個「Prompt 修改器」。這對 DuDuClaw 平台的核心競爭力是倒退。

### 理由二：技術棧根本不相容

GEP 是 JavaScript（Node.js ≥18）實作的；DuDuClaw 的核心基礎設施是 Rust（Tokio async）。

跨語言橋接引入的不只是複雜度，還有 IPC 延遲與額外的進程管理。更重要的是，我們在 Rust 中已有成熟的並發安全模型，沒有理由為了套用外部框架而引入異質技術棧。

### 理由三：生命週期模型不符

GEP Capsules 的生命週期管理是綁定 Genes 的。DuDuClaw 的 GVU 循環有獨立的世代編號（`generation`）與結果分類（`Applied`、`Abandoned`、`Skipped`…），在 GEP 中沒有對應模型。

強行映射等同於為了使用框架而重新設計系統核心——本末倒置。

### 理由四：P0 範圍過重

完整的 GEP 移植估計需要 3–5 個 Sprint。但我們的 P0 目標只是：**讓演化行為有基本的可觀測性**。

過度工程化不是謹慎，是風險。

---

### 那我們借鑑了什麼？

GEP 中有兩個概念我們認為值得借鑑，也確實採用了：

1. **可審計演化歷程**：用結構化事件記錄每一次演化行為
2. **停滯偵測（Stagnation Detection）**：監測同一信號的重複失敗，防止 Repair Loop

這就是 DuDuClaw **EvolutionEvents** 的起點：**增量借鑑，而非整體移植。**

---

## 四、DuDuClaw EvolutionEvents 設計

### 核心 Schema：8 欄位 JSONL

我們設計了一個精簡但可擴充的 8 欄位 Schema，每筆記錄是一行 JSON，文件格式為 JSONL（JSON Lines）：

```json
{
  "timestamp": "2026-04-25T10:30:00Z",
  "event_type": "skill_activate",
  "agent_id": "duduclaw-main",
  "skill_id": "python-review",
  "generation": null,
  "outcome": "success",
  "trigger_signal": "prediction_error_diagnosis",
  "metadata": {"confidence": 0.87}
}
```

| 欄位 | 說明 |
|------|------|
| `timestamp` | RFC3339 UTC 時間戳 |
| `event_type` | 事件類型（5 種，詳見下節） |
| `agent_id` | 觸發事件的 Agent 識別碼 |
| `skill_id` | 涉及的 Skill ID（不適用時為 `null`） |
| `generation` | GVU 世代編號（P0 固定為 `null`，P2 啟用） |
| `outcome` | `success` / `failure` / `suppressed` |
| `trigger_signal` | 觸發事件的上游信號名稱 |
| `metadata` | 任意結構化診斷資料（軟性 1KB 上限） |

設計哲學是「**P0 精簡，未來無破壞擴充**」。`generation` 和即將到來的 `intent_category` 欄位都以 `null` / `Option<>` 預留——後期加入時，歷史 JSONL 資料完全不需要遷移。

---

### 五種 EventType，完整覆蓋 DuDuClaw 演化行為

```
skill_activate      → Skill 被啟用
skill_deactivate    → Skill 被停用（含 3 種觸發路徑）
security_scan       → 安全掃描執行
gvu_generation      → GVU 世代循環（5 種 outcome 全覆蓋）
signal_suppressed   → 信號被抑制（Repair Loop 防護基礎）
```

每種 EventType 都直接對應 DuDuClaw 的真實行為，沒有為了套用框架而扭曲的語義。

以 `skill_deactivate` 為例，我們支援三種不同的觸發路徑，分別由不同的 `trigger_signal` 區分：

```json
// 路徑一：效能評估停用
{
  "event_type": "skill_deactivate",
  "skill_id": "python-review",
  "trigger_signal": "effectiveness_evaluation",
  "outcome": "success"
}
```

```json
// 路徑二：sandbox 試驗丟棄
{
  "event_type": "skill_deactivate",
  "skill_id": "experimental-skill-42",
  "trigger_signal": "sandbox_trial_discard",
  "outcome": "success"
}
```

```json
// 路徑三：容量驅逐（capacity eviction）
{
  "event_type": "skill_deactivate",
  "skill_id": "old-skill-v1",
  "trigger_signal": "capacity_eviction",
  "outcome": "success"
}
```

這種精細的 `trigger_signal` 設計，讓工程師在審計日誌中可以立即看出「這個 Skill 是因為什麼原因被停用的」，而不只是知道「它被停用了」。

---

## 五、工程亮點

### 亮點一：可配置的停滯偵測

我們在 `agent.toml` 中提供了完整的停滯偵測配置，所有參數都有合理預設值：

```toml
[evolution.stagnation_detection]
enabled           = true    # 主開關
window_seconds    = 21600   # 觀察視窗（預設 6 小時）
trigger_threshold = 3       # 視窗內觸發次數門檻
action            = "log_only"  # P0: 只記錄，P1 解封 suppress
```

當同一 `trigger_signal` 在時間視窗內失敗次數達到閾值，系統會記錄一筆 `signal_suppressed` 事件：

```json
{
  "timestamp": "2026-04-25T10:45:00Z",
  "event_type": "signal_suppressed",
  "agent_id": "duduclaw-main",
  "skill_id": null,
  "generation": null,
  "outcome": "suppressed",
  "trigger_signal": "stagnation_detection",
  "metadata": {
    "suppressed_signal": null,
    "trigger_count": null,
    "window_seconds": null
  }
}
```

這裡有個值得關注的語義設計決策：**`signal_suppressed` 事件中 `skill_id` 必須為 `null`**。

原因是：stagnation detection 抑制的對象是「信號」，不是「Skill」。被抑制的實體是信號傳播路徑，Skill 並未在此過程中被直接操作。若填入 `skill_id`，反而會造成語義誤解。被抑制的信號記錄在 `metadata.suppressed_signal` 欄位中（P1 正式啟用後填入真實值）。

P1 升級時，只需在 `channel_reply.rs` 的一個 `TODO P1` 標記處加入閾值 guard 邏輯，**無需修改 Schema**——這就是「漸進式零破壞設計」的價值。

### 亮點二：語義強制驗證（M1 + M2 規則）

我們在 `validate()` 函數中強制執行了兩條語義規則，防止審計日誌出現自相矛盾的記錄：

```rust
// M1：signal_suppressed 的 skill_id 必須為 null
if self.event_type == AuditEventType::SignalSuppressed
    && self.skill_id.is_some()
{
    return Err(ValidationError::SkillIdMustBeNullForEventType {
        event_type: "signal_suppressed",
    });
}

// M2：signal_suppressed 的 outcome 必須為 suppressed
if self.event_type == AuditEventType::SignalSuppressed
    && self.outcome != Outcome::Suppressed
{
    return Err(ValidationError::InvalidOutcomeForEventType {
        event_type: "signal_suppressed",
        got: self.outcome,
        expected: "suppressed",
    });
}
```

這不只是「好習慣」，而是**審計日誌可信度的保障**。一個「代表信號被成功抑制」的事件，若 `outcome` 可以填成 `failure`，審計日誌的語義就會自相矛盾。強制驗證確保每筆記錄都有精確的語義。

### 亮點三：非阻塞寫入保證

EvolutionEventLogger 採用 Tokio `Mutex` 保護單一 file handle，**寫入失敗時降級至 stderr，絕不拋出 panic 或阻塞呼叫端**。

演化日誌是審計基礎設施，不應該成為系統的可靠性瓶頸。如果日誌寫入因為磁碟滿了而失敗，主流程應該繼續運行，不應崩潰。

### 亮點四：671 tests，100% pass

整個 Sprint N P0 最終交付時，測試數達到 **671 tests**，全部通過。

EvolutionEvents 模組本身有 35 個獨立測試，覆蓋 Schema 序列化/反序列化、`validate()` 邊界條件、並發安全（50 tasks 同時寫入）、日期輪替、10MB 大小輪替，以及所有 5 種 typed emit 方法。

測試本身就是文件。671 tests 意味著每一個設計決策都有對應的行為驗證。

---

## 六、後續計畫：P1→P4 的演化路線

P0 只是開始。我們設計這套系統時，已經規劃了清晰的四階段演化路線：

**P1：Anti-Repair-Loop 主動抑制**
解封 `stagnation_action = suppress`，讓系統在偵測到停滯時主動攔截信號，而不只是記錄。Schema 零破壞——P1 只加邏輯，不改結構。

**P2：演化意圖分類（Intent Category）**
新增 `intent_category` 欄位，以及 GVU 世代追蹤（`generation` 欄位正式啟用）。

```
"repair"    → 針對失敗的補救性演化
"optimize"  → 提升效能的主動改善
"innovate"  → 全新 Skill 路徑的能力邊界擴張
```

歷史資料不需遷移，`Option<>` 缺失時自動反序列化為 `None`。

**P3：查詢 API + 視覺化**
`evolution_query` MCP tool 上線，支援按 `event_type`、`agent_id`、時間範圍查詢。趨勢分析：停滯頻率、Skill 存活率、GVU 成功率。

**P4：跨 Agent 演化協調**
EvolutionEvents 跨 instance 同步，多 Agent 演化協調，避免重複演化同一 Skill。

此外，我們也在研究借鑑 Evolver 更多社群實踐，探索 **Rollout-to-Skill Pipeline** 的可能性——讓 EvolutionEvents 審計資料直接驅動 Skill 的自動晉升與退場決策，形成完整的 Skill 生命週期閉環。具體方向將在後續 Sprint 評估。

---

## 結語

設計 EvolutionEvents 最有趣的地方，不是「我們做了什麼」，而是「我們選擇不做什麼」。

GEP 是一個設計精良的框架，Evolver 社群的活躍度也證明了這個方向的正確性。但正確的技術方向，不代表適合直接移植到每個系統。

**語義精準比框架一致更重要。**

DuDuClaw 的 Skills 是有生命的能力單元，它們值得一套能夠精確描述自己演化歷程的語言——而不是被壓縮進一個為 Prompt 碎片設計的框架。

EvolutionEvents 正是這樣的語言：輕量、精準、為 DuDuClaw 量身設計，同時保留了足夠的擴充空間，讓未來的每個 P 版本都可以無縫接續。

---

*作者：DuDuClaw 開發者 / 2026-04-25*

*技術依據：*
- *[ADR-001: EvolutionEvents 審計日誌設計決策](../decisions/adr-001-evolution-events-audit-log.md)*
- *[EvolutionEvents 技術規格 v1.0](../specs/evolution-events-spec-v1.md)*

---

> **📝 草稿備註（供 Agnes 審閱）**
>
> 1. **scrub_metadata**：任務說明中提到「安全性 scrub_metadata」為工程亮點之一，但 ADR-001 與 Spec v1.0 中未有此函數的詳細說明。本草稿以 metadata 軟性大小限制（1KB）及非阻塞降級設計來呈現安全性考量。若 scrub_metadata 有獨立實作細節，請提供後補充。
> 2. **COSPLAY 機制**：Evolver 的 COSPLAY 機制在本草稿中改以「Rollout-to-Skill Pipeline」方向帶過，因 wiki 文件中未有對應的詳細研究記錄。若有更具體的借鑑計畫，可在終稿中補強此段。
> 3. **字數**：草稿約 2,400 字，在 1,500–3,000 字範圍內。
> 4. **發布平台**：待 Agnes 決定（Medium / GitHub Discussions / 其他）。
