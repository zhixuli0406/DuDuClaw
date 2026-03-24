# Claw 生態系統研究報告

> 更新日期：2026-03-23

## 概要

"Claw" 是圍繞 Claude Code / AI Agent 建構的開源生態系統，涵蓋通訊管道整合、Agent 編排、安全防護、skill 市場等面向。以下是 GitHub 上星星數最多的前 10 個 Claw 相關專案。

---

## Top 10 Claw 專案（按 GitHub Stars 排名）

| # | 專案 | Stars | 語言 | 定位 |
|---|-------|------:|------|------|
| 1 | **openclaw/openclaw** | 333k | TypeScript | 全功能 AI 助手（20+ 通訊管道） |
| 2 | **VoltAgent/awesome-openclaw-skills** | 41k | N/A | 5,400+ skills 索引合集 |
| 3 | **qwibitai/nanoclaw** | 25k | TypeScript | 輕量安全替代方案（容器隔離） |
| 4 | **NVIDIA/NemoClaw** | 16k | JavaScript | 企業級沙箱 + Nemotron 模型 |
| 5 | **FreedomIntelligence/OpenClaw-Medical-Skills** | 1.6k | Python | 醫療領域專業 skill 集 |
| 6 | **onecli/onecli** | 1.1k | TypeScript | AI Agent 憑證保險庫 |
| 7 | **prompt-security/clawsec** | 830 | JavaScript | Agent 安全 skill 套件 |
| 8 | **microclaw/microclaw** | 594 | Rust | Rust 單 runtime 跨平台 agent |
| 9 | **moazbuilds/claudeclaw** | 580 | TypeScript | Claude Code 內建輕量版 |
| 10 | **SuperagenticAI/superclaw** | 214 | Python | AI Agent 紅隊安全測試 |

---

## 各專案詳細分析

### 1. OpenClaw (openclaw/openclaw) — 333k stars

**定位**：生態系統核心，全功能 AI 個人助手
**網站**：https://openclaw.ai

**主要功能**：
- 20+ 通訊管道（WhatsApp, Telegram, Slack, Discord, LINE, iMessage...）
- 語音輸入/輸出
- Canvas 視覺化功能
- Skill 系統（可擴充外掛）
- 多模型支援（OpenAI, Claude, Gemini, 本地模型）
- 一鍵 onboard 設定精靈
- 超過 600k 行程式碼

**特色**：社群最大、功能最完整、是整個生態的基石。但體積龐大，配置複雜。

---

### 2. Awesome OpenClaw Skills (VoltAgent/awesome-openclaw-skills) — 41k stars

**定位**：策劃過的 skill 目錄

**主要功能**：
- 收錄 5,400+ 個 OpenClaw skills
- 按類別分類整理
- 涵蓋 clawdbot / moltbot 等多平台
- 持續更新

**特色**：類似 npm registry 的角色，讓使用者快速找到需要的 skill。

---

### 3. NanoClaw (qwibitai/nanoclaw) — 25k stars

**定位**：OpenClaw 的輕量安全替代方案
**網站**：https://nanoclaw.dev

**主要功能**：
- 容器隔離運行（Docker / Apple Container）
- 基於 Anthropic Agent SDK
- Claude Code 原生整合
- 支援 WhatsApp, Telegram, Slack, Discord, Gmail
- Skill 系統

**特色**：「小到能完全理解」的設計理念。刻意保持精簡，以安全性（OS 層級隔離）為核心賣點。Fork-and-customize 哲學。

---

### 4. NemoClaw (NVIDIA/NemoClaw) — 16k stars

**定位**：企業級安全沙箱 AI Agent

**主要功能**：
- NVIDIA OpenShell 沙箱環境
- 內建 Nemotron-3-Super-120B 開源模型
- 安全策略管理
- k3s 容器編排
- 相容 OpenClaw skill 格式

**特色**：NVIDIA 官方出品，2026-03-16 公開 Alpha。專注企業場景：安全合規、本地部署、GPU 加速推理。

---

### 5. OpenClaw Medical Skills (FreedomIntelligence) — 1.6k stars

**定位**：醫療垂直領域 skill 集

**主要功能**：
- 醫療專業 skill 函式庫
- 支援 OpenClaw + Claude Code + NanoClaw
- 涵蓋問診、藥物查詢、報告解讀等

**特色**：展示了 Claw 生態的垂直領域延伸能力。

---

### 6. OneCLI (onecli/onecli) — 1.1k stars

**定位**：AI Agent 憑證保險庫

**主要功能**：
- 密鑰託管，Agent 可存取服務但不暴露金鑰
- 與 OpenClaw / NanoClaw 整合
- MCP Server 支援
- Postgres 後端
- Rust + Node.js 混合架構

**特色**：解決 AI Agent 密鑰管理的痛點。Agent 不需要直接持有 API key。

---

### 7. ClawSec (prompt-security/clawsec) — 830 stars

**定位**：AI Agent 安全防護套件

**主要功能**：
- SOUL.md 漂移檢測（防止人格被竄改）
- NVD CVE 自動監控
- SHA256 完整性驗證
- Prompt injection 偵測
- 自動安全審計

**特色**：Prompt Security 公司出品，專門保護 Agent 認知架構安全。

---

### 8. MicroClaw (microclaw/microclaw) — 594 stars

**定位**：Rust 單 runtime 跨平台 Agent

**主要功能**：
- 支援 Telegram, Discord, Slack, Feishu, IRC, Web
- 多步驟工具調用
- 持久記憶
- 排程任務
- MCP Server 支援
- 本地 Web 控制面板

**特色**：唯一用 Rust 寫的 Claw runtime，強調高效能與跨平台。與 DuDuClaw 架構最接近。

---

### 9. ClaudeClaw (moazbuilds/claudeclaw) — 580 stars

**定位**：直接內建於 Claude Code 的輕量版

**主要功能**：
- `claude plugin` 形式安裝
- 5 分鐘設定
- 背景 daemon
- Telegram / Discord 整合
- Web Dashboard

**特色**：利用 Claude Code 訂閱而非額外 API 費用。最接近「Claude Code 延伸層」的定位。

---

### 10. SuperClaw (SuperagenticAI/superclaw) — 214 stars

**定位**：AI Agent 紅隊安全測試框架

**主要功能**：
- 情境驅動測試
- 行為契約驗證
- 證據導向報告（HTML / JSON / SARIF）
- CI/CD 整合

**特色**：「Red-Team AI Agents Before They Red-Team You」。填補生態中「部署前安全驗證」的缺口。

---

## 共通功能分析

以下是 Claw 生態中大多數專案都具備的共通功能：

| 功能 | OC | NC | NemoC | MicroC | CC | DuDuC |
|------|:--:|:--:|:-----:|:------:|:--:|:-----:|
| Telegram 整合 | v | v | v | v | v | v |
| Discord 整合 | v | v | v | v | v | v |
| LINE 整合 | v | - | - | - | - | v |
| Skill / Plugin 系統 | v | v | v | v | - | v |
| Claude Code SDK 整合 | v | v | v | v | v | v |
| 多模型支援 | v | v | v | - | - | v |
| Web Dashboard | v | v | - | v | v | v |
| 記憶系統 | v | v | - | v | - | v |
| MCP Server | - | v | - | v | - | v |
| 排程任務 | v | v | - | v | - | v |
| 容器隔離 | - | v | v | - | - | - |

**OC** = OpenClaw, **NC** = NanoClaw, **NemoC** = NemoClaw, **MicroC** = MicroClaw, **CC** = ClaudeClaw, **DuDuC** = DuDuClaw

### 核心共通功能

1. **通訊管道路由** — 接收 Telegram / Discord 等平台訊息，轉發給 AI 處理
2. **Claude Code SDK 整合** — 透過 `claude` CLI 或 Agent SDK 呼叫 AI
3. **Skill 外掛系統** — 可擴充的功能模組
4. **對話記憶** — 跨會話保持上下文
5. **Web 管理介面** — 視覺化管理與監控

---

## 各專案特色功能（差異化）

| 專案 | 特色功能 |
|------|---------|
| **OpenClaw** | 20+ 通訊管道、語音 I/O、Canvas、最大社群 |
| **NanoClaw** | OS 層級容器隔離、Apple Container 支援、極簡設計 |
| **NemoClaw** | NVIDIA 沙箱、Nemotron 本地模型、k3s 編排、企業級 |
| **MicroClaw** | Rust 高效能、Feishu/IRC 支援、單一 runtime |
| **ClaudeClaw** | Claude Code plugin 安裝、免額外 API 費用 |
| **ClawSec** | SOUL.md 漂移檢測、prompt injection 防禦 |
| **SuperClaw** | AI Agent 紅隊測試、SARIF 報告、CI/CD 整合 |
| **OneCLI** | 密鑰託管、Agent 不直接持有 API key |
| **DuDuClaw** | 三層演化系統、Ed25519 認證、MCP Server、Rust 單二進位 |

---

## DuDuClaw 競爭定位

### 最直接的競爭者

1. **ClaudeClaw** (580 stars) — 同為「Claude Code 延伸層」定位
2. **NanoClaw** (25k stars) — 輕量、安全隔離、多通道
3. **MicroClaw** (594 stars) — 同為 Rust、同為多平台 agent runtime

### DuDuClaw 的差異化優勢

| 功能 | DuDuClaw | 競品 |
|------|---------|------|
| 三層演化系統（Micro/Meso/Macro） | v | 無 |
| Ed25519 challenge-response 認證 | v | 無 |
| MCP Server（JSON-RPC 2.0 over stdio） | v | NanoClaw 有，但不同架構 |
| Rust 單一二進位 + 嵌入式 Web Dashboard | v | MicroClaw 類似 |
| LINE 整合 | v | 僅 OpenClaw |
| 組織層級委派（reports_to） | v | OpenClaw 類似 |
| CJK-aware token estimation | v | 無 |
| AES-256-GCM API key 加密 | v | OneCLI 有外部方案 |
| Per-agent heartbeat + cron | v | OpenClaw 有，但更粗粒度 |

### 建議發展方向

1. **容器隔離**：NanoClaw 和 NemoClaw 的安全沙箱是強賣點，DuDuClaw 可考慮 Phase 2 實作
2. **Skill 市場相容**：考慮相容 OpenClaw skill 格式以借用其 5,400+ skill 生態
3. **安全防護**：借鑒 ClawSec 的 SOUL.md 漂移檢測和 prompt injection 偵測
4. **紅隊測試**：整合 SuperClaw 作為部署前驗證
