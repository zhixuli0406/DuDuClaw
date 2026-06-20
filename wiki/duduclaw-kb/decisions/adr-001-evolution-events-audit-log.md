---
title: "ADR-001: EvolutionEvents 審計日誌設計決策"
created: 2026-04-25T00:00:00Z
updated: 2026-04-25T00:00:00Z
status: accepted
date: 2026-04-25
decision_maker: Agnes (TL)
tags: [adr, sprint-n, p0, evolution-events, gep, architecture]
layer: decision
trust: 1.0
---

# ADR-001: EvolutionEvents 審計日誌設計決策

## 狀態

**已採用（Accepted）** — 2026-04-25，Agnes 批准

---

## 背景（Context）

DuDuClaw 的 Skill 系統支援動態啟用/停用、安全掃描、GVU（Generator-Verifier-Updater）演化循環等複雜行為。然而，這些演化行為目前缺乏可觀測性——工程師與 QA 無法回溯「為何某 Skill 被停用」、「GVU 循環失敗的診斷資訊」，更無法偵測潛在的 Repair Loop（修復迴圈無限循環）問題。

2026-04-24，研究團隊完成 GEP（Genome Evolution Protocol）競品分析報告（[詳見 research/ai-repos/entities/2026-04-24-evolver-gep.md](../research/ai-repos/entities/2026-04-24-evolver-gep.md)），確認業界存在以 EvolutionEvents 結構化記錄演化歷程的先例（Evolver 專案，6,800 ⭐，週增 +4,032）。

Sprint N P0 正式啟動，Agnes 決策採**增量借鑑**路線，而非整體移植 GEP。

---

## 問題陳述（Problem Statement）

1. **零可觀測性**：Agent 演化行為（Skill 啟停、GVU 循環、安全掃描）完全不留審計記錄
2. **Repair Loop 風險**：無停滯偵測機制，理論上 Agent 可對同一錯誤無限觸發修復
3. **跨 Sprint 設計債**：若 P0 不鎖定 Schema，後期新增欄位（如 `intent_category`）將破壞已有 JSONL 資料

---

## 決策驅動因素（Decision Drivers）

- **可審計性**：每次演化事件須可回溯、可匯出，支援 QA 與合規審查
- **Schema 穩定性**：P0 Schema 需能無破壞性地擴充至 P4
- **非阻塞合約**：寫入失敗不得影響主流程（降級至 stderr）
- **漸進實作**：P0 不引入不必要複雜度，保留 P1-P4 擴充空間
- **Rust 原生**：基礎設施須與 DuDuClaw 現有技術棧一致

---

## 方案評估（Considered Options）

### 方案 A：整體移植 GEP（已否決）

**描述**：直接採用 Evolver（EvoMap/evolver）的 GEP 協議，含 Genes、Capsules、EvolutionEvents 三層結構，以 JavaScript/JSON 格式儲存演化資產。

**優點**：
- 社群驗證方案，參考案例充足
- 四種演化策略預設（`balanced`、`innovate`、`harden`、`repair-only`）現成可用
- 信號去重與 Repair Loop 防護已實作

**否決理由**：

1. **語義降級（最核心理由）**：GEP Genes 是「Prompt 修復碎片」，DuDuClaw Skills 是「可執行能力單元」（含程式碼執行、安全掃描邊界、sandbox 試驗、GVU 循環）——兩者在語義層次上差了一個數量級。強行移植要求將 Skills 壓縮至 Genes 語義，是平台核心語義的退步。

2. **技術棧不相容**：GEP 為 JavaScript（Node.js ≥18），DuDuClaw 為 Rust（Tokio async）；跨語言橋接引入不必要的複雜度與 IPC 延遲。

3. **生命週期模型不符**：GEP Capsules 綁定 Genes 生命週期管理；DuDuClaw 的 GVU 循環（Generator-Verifier-Updater）有獨立的世代編號與結果分類，無 GEP 對應模型。

4. **P0 範圍過重**：完整 GEP 移植估計需 3–5 Sprint，遠超 P0 交付範圍，延誤可觀測性上線時程。

---

### 方案 B：自主設計 EvolutionEvents（採用）✅

**描述**：借鑑 GEP 的**可審計演化歷程**與**停滯偵測**兩個核心概念，自主設計符合 DuDuClaw 語義的 8 欄位 JSONL Schema，以 Rust 原生實作（Tokio + JSONL append-only）。

**優點**：
1. **語義精準**：EventType 直接對應 DuDuClaw 實際行為（`skill_activate`、`skill_deactivate`、`security_scan`、`gvu_generation`、`signal_suppressed`），無語義損失
2. **Rust 原生**：Tokio Mutex + JSONL append，天然支援並發安全與非阻塞寫入
3. **漸進式無破壞擴充**：P0 僅 8 欄位，`generation`、`intent_category` 以 `null` / Option<> 預留，後期擴充無需遷移已有資料
4. **輕量 P0 交付**：Sprint N 內可完整交付，35 tests 100% pass

**缺點**：
- 無現成社群生態，需自建工具鏈（可接受：此為 DuDuClaw 平台差異化優勢）

---

## 決策（Decision Outcome）

**採用方案 B：自主設計 EvolutionEvents**

### 核心理由

> **DuDuClaw Skill 語義比 GEP Genes 更豐富，整體移植 GEP 反而是退步。**

GEP 的 Genes 是「Prompt 修復碎片」，而 DuDuClaw 的 Skills 是「可執行的能力單元」，兩者在語義層次上差了一個數量級。強行移植等同於為了套用既有框架而犧牲平台的核心競爭力。

增量借鑑 GEP 的「審計歷程記錄」與「停滯偵測」兩個概念，以 DuDuClaw 原生語義重新實現，方為正確路線。

---

## 影響（Consequences）

### P0→P4 漸進路線圖

| Phase | 交付內容 | Schema 影響 | 依賴 |
|-------|---------|------------|------|
| **P0（當前）** | 8 欄位 Schema、5 種 EventType、JSONL logger、stagnation_detection 配置（`log_only`）、5 個發射點埋點 | Schema v1 鎖定 | — |
| **P1** | Anti-Repair-Loop 主動抑制（`signal_suppressed` 條件觸發，`action=suppress`） | 零破壞（僅邏輯層變更） | P0 Schema 穩定 |
| **P2** | `intent_category` 加入 JSONL（`repair`/`optimize`/`innovate`）、`generation` 欄位啟用 | 新增 Option<> 欄位（向後相容） | P0 Schema 穩定 |
| **P3** | `evolution_query` MCP tool、趨勢分析儀表板、JSONL 索引最佳化 | 唯讀查詢層，不修改 Schema | P1 + P2 |
| **P4** | 跨 Agent 演化協調、EvolutionEvents 跨 instance 同步 | 擴充同步欄位（TBD） | P3 |

### 非引入項目

見 [specs/evolution-events-spec-v1.md](../specs/evolution-events-spec-v1.md) §6「不引入項目清單」。

---

## 參照（References）

- GEP 競品研究：[research/ai-repos/entities/2026-04-24-evolver-gep.md](../research/ai-repos/entities/2026-04-24-evolver-gep.md)
- Schema + 基礎設施技術說明（T1+T3）：[sprint-n/evolution-events-schema-adr.md](../sprint-n/evolution-events-schema-adr.md)
- Agent 事件發射點盤點（T2）：[sprint-n/t2-emission-point-audit.md](../sprint-n/t2-emission-point-audit.md)
- 技術規格 v1：[specs/evolution-events-spec-v1.md](../specs/evolution-events-spec-v1.md)

---

*決策人：Agnes（TL）*
*起草：PM-DuDuClaw（duduclaw-pm）*
*日期：2026-04-25*
