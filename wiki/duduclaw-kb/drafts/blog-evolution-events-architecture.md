---
title: "DuDuClaw 如何借鑑 Evolver/GEP 設計可審計的 AI Agent 演化日誌"
created: 2026-04-29T00:00:00Z
updated: 2026-04-29T00:00:00Z
status: draft
author: duduclaw-marketing
task_id: 625128db-d5c6-4b94-94b9-d5149d6cdf63
tags: [blog, draft, evolution-events, gep, evolver, architecture, w19, m1]
layer: context
trust: 0.9
target_audience: AI 工程師、Agent 開發者社群
word_count_estimate: 2800
deadline: 2026-05-02
references:
  - decisions/adr-001-evolution-events-audit-log.md
  - specs/evolution-events-spec-v1.md
---

# DuDuClaw 如何借鑑 Evolver/GEP 設計可審計的 AI Agent 演化日誌

> **草稿版本**：v0.1 — 待 Agnes 審閱後決定發布平台（Medium / GitHub Discussions）

---

## 你的 AI Agent 知道自己昨天做了什麼嗎？

上週，我在 debug 一個奇怪的問題：某個 Skill 被停用了，但沒有人知道為什麼。

日誌裡什麼都找不到。GVU 循環（Generator-Verifier-Updater）確實跑了，安全掃描也有執行，但整個演化過程就像黑盒子——能力在變化，卻沒有留下任何可以回溯的痕跡。

這讓我意識到一件事：**如果一個 AI Agent 系統的演化行為無法審計，它本質上是不可信任的。**

這篇文章想聊的，就是 DuDuClaw 如何在 Sprint N 解決這個問題——以及我們為什麼選擇「借鑑而非移植」Evolver/GEP 的設計思路。

---

## 開源社群的先例：Evolver 與 GEP

在開始動手設計之前，我們做了一輪競品研究。

Evolver 這週漲了 +4,032 ⭐，來到 6,800 ⭐ 附近——這個數字背後，是 AI Agent 演化管理這個賽道正在快速升溫的訊號。

Evolver 的核心設計是 **GEP（Genome Evolution Protocol）**，它把 Agent 的演化歷程結構化為三層：

- **Genes（基因）**：最小可複用的 Prompt 修復碎片，對應特定錯誤信號的 Prompt 片段
- **Capsules（膠囊）**：封裝多個 Genes 的複合修復單元，管理 Gene 生命週期
- **EvolutionEvents**：記錄演化事件的審計歷程

GEP 還內建了四種全域演化策略（`balanced`、`innovate`、`harden`、`repair-only`），以及信號去重與 Repair Loop 防護機制。

整個設計思路很清晰：把 Agent 的 Prompt 演化過程變成可追蹤、可審計的記錄。

那我們直接整體移植不就好了嗎？

---

## 為什麼我們沒有整體移植 GEP

這是整個設計過程中最重要的一個決策，值得認真解釋清楚。

**核心問題只有一個：語義不相容。**

GEP 的 Genes 是「Prompt 修復碎片」——它們是對應特定錯誤信號的 Prompt 片段，設計目的是讓 Agent 在遇到錯誤時，能夠從基因庫中調取合適的 Prompt 來修復問題。

DuDuClaw 的 Skills 則完全不同。一個 Skill 是**可執行的能力單元**，它包含：

- 可執行的程式碼邏輯與 sandbox 試驗機制
- 安全掃描邊界（透過 `skill_security_scan` 守衛）
- GVU 循環（Generator-Verifier-Updater）的獨立世代編號
- 能力評估機制（effectiveness evaluation）

如果我們把 Skills 壓縮到 Genes 的語義框架裡，等於是在主動降低平台的核心能力表達層次——這不是借鑑，這是退步。

> 「DuDuClaw Skill 語義比 GEP Genes 更豐富，整體移植 GEP 反而是退步。」  
> ——摘自 ADR-001，Agnes（TL）

除了語義問題，還有幾個工程層面的不相容：

**1. 技術棧衝突**

GEP 是 JavaScript（Node.js ≥18），DuDuClaw 是 Rust（Tokio async）。跨語言橋接引入了不必要的複雜度與 IPC 延遲，且與我們「Rust 原生、零依賴外部 runtime」的基礎設施原則相違。

**2. 生命週期模型不符**

GEP 的 Capsules 綁定 Genes 的生命週期管理。DuDuClaw 的 GVU 循環有獨立的世代編號（`generation`）與結果分類（`GvuOutcome`），兩者沒有對應的映射關係。

**3. P0 範圍過重**

完整的 GEP 移植估計需要 3–5 個 Sprint。DuDuClaw 的可觀測性問題是 P0，我們需要一個可以在單一 Sprint 內完整交付的方案。

**我們的選擇：增量借鑑。**

從 GEP 裡提取兩個真正有價值的核心概念：
- **可審計的演化歷程記錄**
- **停滯偵測（Stagnation Detection）**

然後以 DuDuClaw 原生語義重新實現，其餘的 GEP 概念明確排除。

---

## DuDuClaw EvolutionEvents 設計

### Schema：8 欄位，一次鎖定

每筆 EvolutionEvent 是單行 JSON，整個日誌以 JSONL 格式儲存（每行一筆記錄，`\n` 結尾）：

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

8 個欄位的設計原則是**漸進無破壞擴充**：

| 欄位 | 型別 | P0 狀態 | 說明 |
|------|------|---------|------|
| `timestamp` | ISO8601 string（RFC3339 UTC） | ✅ 啟用 | 事件記錄時的 UTC 時間 |
| `event_type` | enum（5 種） | ✅ 啟用 | 事件類型 |
| `agent_id` | string | ✅ 啟用 | 觸發 Agent 識別碼 |
| `skill_id` | string \| null | ✅ 啟用 | 涉及的 Skill（不適用時為 null） |
| `generation` | int \| null | P0 固定 null | GVU 世代編號，P2 啟用 |
| `outcome` | enum | ✅ 啟用 | `success` / `failure` / `suppressed` |
| `trigger_signal` | string \| null | ✅ 啟用 | 觸發事件的上游信號名稱 |
| `metadata` | JSON object | ✅ 啟用 | 結構化診斷資料（< 1 KB） |

`generation` 欄位 P0 固定為 `null`，P2 啟用；計畫中的 `intent_category` 欄位 P0 僅在文件預定義語義，P2 才加入 JSONL。**P0 Schema 在整個 P0→P4 路線圖中保持穩定，歷史資料永遠不需要遷移。**

### 5 種 EventType：覆蓋完整演化生命週期

| EventType | 說明 | 典型觸發信號 |
|-----------|------|------------|
| `skill_activate` | Skill 被啟用 | `prediction_error_diagnosis` |
| `skill_deactivate` | Skill 被停用（3 種觸發路徑） | `effectiveness_evaluation`、`sandbox_trial_discard`、`capacity_eviction` |
| `security_scan` | 安全掃描執行 | `skill_security_scan` |
| `gvu_generation` | GVU 世代循環（5 種 outcome 全覆蓋） | dynamic（按 GvuOutcome 變體） |
| `signal_suppressed` | 信號被抑制（Repair Loop 防護審計基礎）| `stagnation_detection` |

`signal_suppressed` 在 P0 是 stub 實作——Schema 型別已穩定鎖定，但主動抑制的觸發邏輯保留到 P1 啟用。這個設計確保了 P1 只需要在 `channel_reply.rs` 解封一個閾值 guard，**零 Schema 破壞，零歷史資料遷移**。

### 停滯偵測：Repair Loop 防護的審計基礎

`signal_suppressed` 事件是整個停滯偵測機制的審計基礎。核心概念借鑑自 GEP 的 Repair Loop 防護，但用 DuDuClaw 的語義重新表達：

```toml
[evolution.stagnation_detection]
enabled           = true
window_seconds    = 21600       # 觀察視窗：6 小時
trigger_threshold = 3           # 門檻：6 小時內同一信號失敗 ≥3 次 → 判定停滯
action            = "log_only"  # P0: 記錄不阻斷；P1 reserved: suppress
```

P0 的 `action = "log_only"` 是有意識的決策：**先讓審計資料流起來，觀察停滯模式，P1 再啟用主動抑制。** 這避免了在沒有足夠資料支撐的情況下，貿然上線可能影響正常演化流程的抑制機制。

`signal_suppressed` 事件有兩條被強制執行的語義規則：

- **M1**：`skill_id` **必須為 null**——被抑制的對象是「信號」，不是某個具體 Skill
- **M2**：`outcome` **必須為 `suppressed`**——此事件的存在本身就代表抑制成功

P1 填入真實資料後的完整記錄範例：

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
    "suppressed_signal": "prediction_error_diagnosis",
    "trigger_count": 3,
    "window_seconds": 21600
  }
}
```

---

## 工程實作亮點

### Rust 原生，非阻塞保證

整個 EvolutionEvents 基礎設施以 Rust 實作，模組結構清晰：

```
crates/duduclaw-gateway/src/evolution_events/
├── schema.rs    — AuditEvent struct + AuditEventType/Outcome enum + validate()
├── logger.rs    — EvolutionEventLogger（JSONL append + UTC 日期輪替 + 10 MB 上限）
├── emitter.rs   — EvolutionEventEmitter（typed fire-and-forget + global singleton）
└── mod.rs       — 模組 re-export + Quick-start 文件
```

**最重要的工程約束**：**寫入失敗絕不影響主流程。**

Logger 採用「錯誤降級至 stderr」策略——無論 JSONL 寫入遇到什麼問題（磁碟滿、權限錯誤），呼叫端永遠不會收到 panic 或 blocking。審計記錄不會成為系統的單點故障。

並發安全由 Tokio `Mutex` 保護單一 file handle；跨 process 的並發寫入依賴 `O_APPEND` 的原子性保證。

日誌寫入路徑：`data/evolution/events/YYYY-MM-DD.jsonl`（可透過 `$EVOLUTION_EVENTS_DIR` 環境變數覆蓋）。

### validate()：語義強制在寫入端執行

schema.rs 的 `validate()` 函式強制執行 signal_suppressed 的語義規則（M1 + M2）：

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

不符合規則的記錄直接被拒絕（返回 `Err`），記錄到 stderr，**不寫入 JSONL**。審計日誌的語義一致性在寫入端即被強制保證——這對長期的日誌可信度至關重要。

### P0 Stub 設計：為 P1 舖平道路

`signal_suppressed` 的 P0 stub metadata 採用「null placeholder」策略：

```rust
// P0 stub：欄位結構與 P1 完全一致，值先填 null
emitter.emit_signal_suppressed_stub(
    agent_id,
    serde_json::json!({
        "suppressed_signal": null,
        "trigger_count": null,
        "window_seconds": null
    }),
);

// P1 解封後：只需填入真實值，零欄位重命名
emitter.emit_signal_suppressed_stub(
    agent_id,
    serde_json::json!({
        "suppressed_signal": "prediction_error_diagnosis",
        "trigger_count": consecutive,
        "window_seconds": stagnation_cfg.window_seconds,
    }),
);
```

P1 工程師接手時，只需替換 null 為真實值，**欄位名稱完全不變**。

### 測試覆蓋：35 tests，100% pass

| 模組 | 測試數 | 關鍵涵蓋點 |
|------|--------|----------|
| `schema.rs` | 12 | 序列化/反序列化、`validate()` 全規則、null 欄位存在性、非法值拒絕 |
| `logger.rs` | 11 | JSONL 格式正確性、並發安全（50 concurrent tasks）、日期輪替、10 MB 大小輪替、錯誤降級 |
| `emitter.rs` | 12 | 5 種 event_type typed methods、並發發射（30 concurrent tasks）、P0 null generation 強制、global singleton 唯一性 |
| **合計** | **35** | **100% pass** |

這 35 個 tests 是全系統 671 個測試中的一部分，所有測試全數通過是 Sprint N P0 的驗收基準。

---

## 增量借鑑的實務心得

這次設計過程讓我對「如何借鑑開源方案」有了新的體會。

**不是所有的「不整體移植」都是懶，有時候恰恰是對自己系統語義的尊重。**

GEP 是個優秀的設計，但它的設計目標是解決「Prompt 修復」的問題。DuDuClaw 的 Skills 在語義層次上遠比 Genes 豐富，強行套用 GEP 框架，實際上是為了「使用現成方案」而犧牲平台的核心競爭力。

我逐漸形成的增量借鑑流程是：

1. **理解對方設計的核心問題**（而非具體方案）
2. **確認自己是否面臨同樣的問題**
3. **以自己的語義重新實現解決方案**
4. **明確記錄「不引入的項目」及原因**，避免未來工程師重蹈覆轍

從 GEP，我們借鑑了「可審計演化歷程」和「停滯偵測」兩個概念。Genes、Capsules、四種全域策略、JavaScript 技術棧——全部明確排除，並在 Spec 中寫明原因。

---

## 後續計畫：P1–P4 路線圖

EvolutionEvents 只是開始，整個演化系統還有四個 Phase 等待完成：

```
P1（Anti-Repair-Loop 主動抑制）
   → signal_suppressed 閾值 guard 解封
   → metadata 填入真實值（suppressed_signal / trigger_count / window_seconds）
   → stagnation_action = suppress 支援
   → 零 Schema 破壞

P2（演化意圖分類 + GVU 世代追蹤）
   → intent_category 加入 JSONL（repair / optimize / innovate）
   → generation 欄位啟用（GVU 世代編號 1-based）
   → 歷史資料向後相容（Option<> 欄位缺失為 None）

P3（查詢 API + 可視化）
   → evolution_query MCP tool
   → 停滯頻率、Skill 存活率、GVU 成功率趨勢分析
   → JSONL 索引最佳化

P4（跨 Agent 演化協調）
   → EvolutionEvents 跨 instance 同步
   → 多 Agent 演化協調，避免重複演化同一 Skill
```

P0 Schema 的設計確保了每個 Phase 都可以零破壞地往前推進。這種「預留擴充點、不提前實作」的策略，是我在這次 Sprint 中最滿意的工程決策之一。

---

## 結語

如果你正在設計 AI Agent 的演化系統，我想提一個問題：**你能回答「這個 Skill 為什麼在上週三被停用」嗎？**

如果答案是「查不到」，那可審計的演化日誌就是你的下一個 P0。

DuDuClaw 的 EvolutionEvents 是一個起點，也是我們對「AI Agent 可觀測性」這個問題的第一個正式回答。我希望把整個設計過程——ADR、Spec、測試策略、借鑑決策——都透明地記錄下來，讓社群可以參考，也讓未來的我可以回頭看看當初為什麼做了這些決定。

這就是為什麼我寫這篇文章。

---

*相關技術文件：*
- *ADR-001：EvolutionEvents 審計日誌設計決策（架構決策依據）*
- *EvolutionEvents 技術規格 v1.0（Schema + 實作細節）*

*#DuDuClaw #AIAgent #Evolver #GEP #RustLang #可審計AI #AgentEvolution*
