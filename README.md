# DuDuClaw 🐾

> **Multi-Runtime AI Agent Platform** — 統一 Claude / Codex / Gemini 三大 CLI，打造你的多通道 AI 助理

[![CI](https://github.com/zhixuli0406/DuDuClaw/actions/workflows/ci.yml/badge.svg)](https://github.com/zhixuli0406/DuDuClaw/actions/workflows/ci.yml)
[![Rust](https://img.shields.io/badge/Rust-2024_edition-orange?logo=rust)](https://www.rust-lang.org/)
[![Python](https://img.shields.io/badge/Python-3.9+-blue?logo=python)](https://www.python.org/)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache--2.0-blue.svg)](LICENSE)
[![Version](https://img.shields.io/badge/version-1.4.27-blue)](https://github.com/zhixuli0406/DuDuClaw/releases)
[![npm](https://img.shields.io/npm/v/duduclaw?logo=npm)](https://www.npmjs.com/package/duduclaw)
[![PyPI](https://img.shields.io/pypi/v/duduclaw?logo=pypi)](https://pypi.org/project/duduclaw/)

---

## 什麼是 DuDuClaw？

DuDuClaw 是一個 **Multi-Runtime AI Agent 平台**——同時支援 **Claude Code / Codex / Gemini** 三大 CLI 作為 AI 後端，並透過統一的 `AgentRuntime` trait 實現無縫切換與自動偵測。

它不綁定任何單一 AI 供應商，而是為你的 AI Agent 接上通訊通道、記憶、自我進化、本地推論與帳號管理等完整基礎設施。

核心概念：

- **Multi-Runtime** — `AgentRuntime` trait 統一 Claude / Codex / Gemini / OpenAI-compat 四種後端，`RuntimeRegistry` 自動偵測，per-agent 設定
- **Plumbing = DuDuClaw** — 負責通道路由、session 管理、記憶搜尋、帳號輪替、本地推論等基礎設施
- **橋接 = MCP Protocol** — `duduclaw mcp-server` 作為 MCP Server，將通道與記憶工具暴露給 AI Runtime

```
AI Runtime (brain) — Claude CLI / Codex CLI / Gemini CLI / OpenAI-compat
  ↕ MCP Protocol (JSON-RPC 2.0, stdin/stdout)
DuDuClaw (plumbing)
  ├─ Channel Router — Telegram / LINE / Discord / Slack / WhatsApp / Feishu / WebChat
  ├─ Multi-Runtime — Claude / Codex / Gemini / OpenAI-compat 自動偵測 + per-agent 設定
  ├─ Session Manager — SQLite, 50k token 自動壓縮（CJK-aware）
  ├─ MCP Server — 70+ 工具（通訊、記憶、Agent、Skill、推論、ERP）
  ├─ Evolution Engine — GVU² 雙迴圈進化 + 預測驅動 + MistakeNotebook
  ├─ Inference Engine — llama.cpp / mistral.rs / Exo P2P / llamafile / MLX / ONNX
  ├─ Voice Pipeline — ASR (SenseVoice / Whisper) + TTS (Piper / MiniMax) + VAD (Silero)
  ├─ Account Rotator — 多 OAuth + API Key 輪替、預算追蹤、健康檢查、Cross-Provider Failover
  ├─ Browser Automation — 5 層自動路由（API Fetch → Scrape → Headless → Sandbox → Computer Use）
  └─ Web Dashboard — React 19 SPA（23 頁面），透過 rust-embed 嵌入 binary
```

---

## 核心特色

### 通訊與通道

| 特色 | 說明 |
|------|------|
| **七通道支援** | Telegram（long polling）、LINE（webhook）、Discord（Gateway WebSocket）、Slack（Socket Mode）、WhatsApp（Cloud API）、Feishu（Open Platform v2）、WebChat（WebSocket）|
| **Per-Agent Bot** | 每個 Agent 可擁有獨立的 Bot Token，同平台多 Agent 並行 |
| **通道熱啟停** | Dashboard 新增/移除通道即時生效，無需重啟 gateway |
| **WebChat** | 內建 `/ws/chat` WebSocket 端點，React 前端即時對話 |
| **Generic Webhook** | `POST /webhook/{agent_id}` + HMAC-SHA256 簽章驗證 |
| **Media Pipeline** | 圖片自動縮放（max 1568px）+ MIME 偵測 + Vision 整合 |
| **Sticker 系統** | LINE 貼圖目錄 + 情緒偵測 + Discord emoji 等價映射 |

### AI 執行與推論

| 特色 | 說明 |
|------|------|
| **MCP Server 架構** | `duduclaw mcp-server` 提供 70+ 工具，涵蓋通訊、記憶、Agent 管理、推論、排程、Skill 市場、Odoo ERP |
| **Multi-Runtime** | `AgentRuntime` trait — Claude / Codex / Gemini / OpenAI-compat 四種後端，`RuntimeRegistry` 自動偵測，per-agent 設定 |
| **本地推論引擎** | 統一 `InferenceBackend` trait — llama.cpp（Metal/CUDA/Vulkan）/ mistral.rs（ISQ + PagedAttention）/ Exo P2P 叢集 / llamafile / MLX（Apple Silicon）/ OpenAI-compat HTTP |
| **三層信心路由** | LocalFast → LocalStrong → CloudAPI，基於啟發式信心評分自動分流，CJK-aware token estimation |
| **InferenceManager** | 多模式自動切換：Exo P2P → llamafile → Direct backend → OpenAI-compat → Cloud API，週期性健康檢查 + 自動 failover |
| **Direct API** | 繞過 CLI 直接呼叫 Anthropic Messages API，`cache_control: ephemeral` 達 95%+ 快取命中率 |
| **Token 壓縮** | Meta-Token（BPE-like 27-47%）、LLMLingua-2（2-5x 有損）、StreamingLLM（無限長對話）|
| **Cross-Provider Failover** | `FailoverManager` 健康追蹤、冷卻、不可重試錯誤偵測 |
| **Browser 自動化** | 5 層路由（API Fetch → Static Scrape → Headless Playwright → Sandbox Container → Computer Use），deny-by-default |

### 語音與多媒體

| 特色 | 說明 |
|------|------|
| **ASR 語音辨識** | ONNX SenseVoice（本地）+ Whisper.cpp（本地）+ OpenAI Whisper API |
| **TTS 語音合成** | ONNX Piper（本地）+ MiniMax T2A |
| **VAD 語音活動偵測** | ONNX Silero VAD |
| **Discord 語音頻道** | Songbird 整合，Discord 語音對話 |
| **LiveKit 語音房** | WebRTC 多 Agent 語音會議 |
| **ONNX 嵌入** | BERT WordPiece tokenizer + ONNX Runtime 向量嵌入 |

### Agent 編排與進化

| 特色 | 說明 |
|------|------|
| **Sub-Agent 編排** | `create_agent` / `spawn_agent` / `list_agents` MCP 工具 + `reports_to` 組織層級 + D3.js 架構圖 |
| **GVU² 雙迴圈進化** | 外迴圈（Behavioral GVU — SOUL.md 進化）+ 內迴圈（Task GVU — 即時任務重試），MistakeNotebook 跨迴圈記憶 |
| **預測驅動進化** | Active Inference + Dual Process Theory，~90% 對話零 LLM 成本；MetaCognition 每 100 預測自校準閾值 |
| **4+2 層驗證** | L1-Format / L2-Metrics / **L2.5-MistakeRegression** / L3-LLMJudge / **L3.5-SandboxCanary** / L4-Safety，前 4 層零成本 |
| **Adaptive Depth** | MetaCognition 驅動 GVU 迭代深度（3-7 輪），根據歷史成功率自動調整 |
| **Deferred GVU** | gradient 累積 + 延遲重試（最多 3 次 deferral，72h 跨度 9-21 輪有效迭代）|
| **ConversationOutcome** | 零 LLM 對話結果偵測（TaskType / Satisfaction / Completion），zh-TW + en 雙語 |
| **SOUL.md 版控** | 24h 觀察期 + 自動回滾，atomic write（SHA-256 fingerprint）|
| **Agent-as-Evaluator** | 獨立 Evaluator Agent（Haiku 成本控制）進行對抗式驗證，結構化 JSON verdict |
| **DelegationEnvelope** | 結構化交接協議 — context / constraints / task_chain / expected_output，向後相容 Raw payload |
| **TaskSpec 工作流** | 多步驟任務規劃 — dependency-aware scheduling / auto-retry（3x）/ replan（最多 2 次）/ persistence |
| **Orchestrator 模板** | 5 步規劃策略（Analyze → Decompose → Delegate → Evaluate → Synthesize）+ 複雜度路由 |
| **Skill 生命週期** | 7 階段管理 — Activation → Compression → Extraction → Reconstruction → Distillation → Diagnostician → Gap Analysis |
| **Reminder 排程** | 一次性提醒（相對時間 `5m`/`2h`/`1d` 或 ISO 8601 絕對時間），`direct` 靜態訊息或 `agent_callback` 喚醒模式 |

### 安全防護

| 特色 | 說明 |
|------|------|
| **Claude Code Security Hooks** | 三層漸進式防禦 — Layer 1 黑名單（<50ms）→ Layer 2 混淆偵測（YELLOW+）→ Layer 3 Haiku AI 研判（RED only）|
| **威脅等級狀態機** | GREEN → YELLOW → RED 自動升降級，24h 無事件降一級 |
| **SOUL.md 漂移檢測** | SHA-256 fingerprint 即時比對 |
| **Prompt Injection 掃描** | 6 類規則，XML 分隔標籤防注入 |
| **Secret 洩漏掃描** | 20+ 模式（Anthropic/OpenAI/AWS/GitHub/Slack/Stripe/DB URL 等）|
| **敏感檔案保護** | Read/Write/Edit 三方向保護 `secret.key`、`.env*`、`SOUL.md`、`CONTRACT.toml` |
| **行為契約** | `CONTRACT.toml` 定義 `must_not` / `must_always` 邊界 + `duduclaw test` 紅隊測試（9 項場景）|
| **JSONL 審計日誌** | async 寫入，格式相容 Rust `AuditEvent` schema |
| **Per-Agent 密鑰隔離** | AES-256-GCM 加密儲存，agent 間密鑰互不可見 |
| **容器沙箱** | Docker / Apple Container（`--network=none`、tmpfs、read-only rootfs、512MB limit）|
| **Browser 自動化** | 5 層路由（API Fetch → Static Scrape → Headless → Sandbox → Computer Use），deny-by-default |

### 帳號與成本

| 特色 | 說明 |
|------|------|
| **雙模式帳號輪替** | OAuth 訂閱（Pro/Team/Max）+ API Key 混合 — 4 策略（Priority/LeastCost/Failover/RoundRobin）|
| **健康追蹤** | Rate limit 冷卻（2min）、帳單耗盡冷卻（24h）、Token 過期追蹤（30d/7d 預警）|
| **成本遙測** | SQLite token 追蹤、快取效率分析、200K 價格懸崖警告、自適應路由（快取效率 <30% 自動切本地）|

### 整合與擴充

| 特色 | 說明 |
|------|------|
| **Odoo ERP 整合** | `duduclaw-odoo` 中間層 — 15 個 MCP 工具（CRM/銷售/庫存/會計/通用搜尋報表），支援 CE/EE，EditionGate 自動偵測 |
| **Skill 市場** | GitHub Search API 即時索引 + 24h 本地快取 + 安全掃描 + Dashboard 市場頁面 |
| **Prometheus 指標** | `GET /metrics` — requests、tokens、duration histogram、channel status |
| **CronScheduler** | `cron_tasks.jsonl` + cron 表達式，定時任務自動觸發 |
| **ONNX 嵌入** | BERT WordPiece tokenizer + ONNX Runtime 向量嵌入，語意搜尋支援 |
| **Experiment Logger** | Trajectory recording，支援 RL/RLHF 離線分析 |

### Web Dashboard

| 特色 | 說明 |
|------|------|
| **技術棧** | React 19 + TypeScript + Tailwind CSS 4 + shadcn/ui，溫暖 amber 色系 |
| **23 個頁面** | Dashboard / Agents / Channels / Accounts / Memory / Security / Settings / OrgChart / SkillMarket / Logs / WebChat / OnboardWizard / Billing / License / Report / PartnerPortal / Marketplace / KnowledgeHub / Odoo / Login / Users / Analytics / Export |
| **即時日誌** | BroadcastLayer tracing → WebSocket 推播 |
| **組織架構圖** | D3.js 互動式 Agent 層級視覺化 |
| **深淺色切換** | 跟隨系統偏好，支援手動切換 |
| **國際化** | zh-TW / en / ja-JP 三語支援（540+ 翻譯鍵）|
| **Session Replay** | 對話回放元件，支援時間軸檢視 |

---

## 競品對比

| | **DuDuClaw** | **OpenClaw** | **IronClaw** | **Moltis** | **Dify** |
|---|---|---|---|---|---|
| 語言 | Rust | TypeScript | Rust | Rust | Python |
| 頻道 | 7 | 25+ | 8 | 5 | 0 (API) |
| Multi-Runtime | **4 後端（Claude/Codex/Gemini/OpenAI）** | - | - | - | 多 LLM |
| MCP Server | **70+ 工具** | - | - | - | - |
| 自我演化引擎 | **GVU² 雙迴圈** | - | - | - | - |
| 本地推論 | **6 後端 + 三層信心路由** | - | - | - | - |
| 語音 (ASR/TTS) | **4 ASR + 4 TTS provider** | - | - | - | - |
| Token 壓縮 | **3 種策略** | - | - | - | - |
| Browser 自動化 | **5 層路由** | - | - | - | - |
| 成本遙測 | **快取效率分析** | - | 基礎 | 基礎 | 基礎 |
| 行為合約 | **CONTRACT.toml + 紅隊** | - | WASM 沙箱 | - | - |
| ERP 整合 | **Odoo 15 工具** | - | - | - | - |
| 安全稽核 | **三層防禦 + Hooks** | CVE-2026-25253 | WASM | 基礎 | 中等 |
| 授權 | **Apache 2.0 (Open Core)** | MIT | 開源 | 開源 | $59+/月 |

---

## Agent 目錄結構

每個 Agent 是一個資料夾，結構與 Claude Code 完全相容：

```
~/.duduclaw/agents/
├── dudu/                    # 主 Agent
│   ├── .claude/             # Claude Code 設定
│   │   └── settings.local.json
│   ├── .mcp.json            # MCP Server 設定（指向 duduclaw mcp-server）
│   ├── SOUL.md              # 人格定義（SHA-256 保護）
│   ├── CLAUDE.md            # Claude Code 指引
│   ├── CONTRACT.toml        # 行為契約（must_not / must_always）
│   ├── agent.toml           # DuDuClaw 設定（模型、預算、心跳、runtime）
│   ├── SKILLS/              # 技能集（可由進化引擎自動產出）
│   ├── memory/              # 每日筆記
│   ├── tasks/               # TaskSpec 工作流持久化（JSON）
│   └── state/               # 運行時狀態 (SQLite)
│
└── coder/                   # 另一個 Agent
    └── ...
```

使用 `duduclaw migrate` 可將舊版 `agent.toml` 自動轉換為 Claude Code 相容格式。

---

## Security Hooks 安全防禦系統

DuDuClaw 在 Claude Code 的 Hook 系統上建構了三層漸進式防禦：

```
                    ┌─────────────────────────────────────┐
  SessionStart ──→  │ session-init.sh                     │  密鑰權限驗證 + 環境初始化
                    └─────────────────────────────────────┘
                    ┌─────────────────────────────────────┐
  UserPrompt   ──→  │ inject-contract.sh                  │  CONTRACT.toml 規則注入
                    └─────────────────────────────────────┘
                    ┌─────────────────────────────────────┐
  PreToolUse   ──→  │ bash-gate.sh (Bash)                 │  Layer 1: 黑名單 (<50ms)
     (Bash)         │   ├─ Layer 2: 混淆偵測 (YELLOW+)    │  Layer 2: base64/eval/外滲
                    │   └─ Layer 3: Haiku AI (RED only)   │  Layer 3: AI 安全研判
                    └─────────────────────────────────────┘
                    ┌─────────────────────────────────────┐
  PreToolUse   ──→  │ file-protect.sh → ai-review.sh     │  敏感檔案保護 + AI 審查
  (Write|Edit|Read) └─────────────────────────────────────┘
                    ┌─────────────────────────────────────┐
  PostToolUse  ──→  │ secret-scanner.sh → audit-logger.sh │  Secret 掃描 → async 審計
                    └─────────────────────────────────────┘
                    ┌─────────────────────────────────────┐
  Stop         ──→  │ threat-eval.sh                      │  威脅等級重新評估
                    └─────────────────────────────────────┘
                    ┌─────────────────────────────────────┐
  ConfigChange ──→  │ config-guard.sh                     │  配置篡改偵測
                    └─────────────────────────────────────┘
```

### 威脅等級狀態機

| 等級 | 觸發條件 | 防禦行為 |
|------|---------|---------|
| **GREEN** (預設) | 正常操作 | Layer 1 黑名單 + 檔案保護 + Secret 掃描 |
| **YELLOW** | 1 小時內 ≥ 2 次攔截 | +Layer 2 混淆偵測 + 外部網路限制 |
| **RED** | 偵測到注入/eval 攻擊 | +Layer 3 Haiku AI 研判所有命令 + AI 檔案審查 |

降級：24 小時無事件自動降一級（RED→YELLOW→GREEN）。

---

## 安裝

### npm（推薦）

```bash
npm install -g duduclaw
```

安裝完成後會自動下載對應平台的預編譯 binary（支援 macOS ARM64/x64、Linux x64/ARM64、Windows x64）。

### Homebrew（macOS / Linux）

```bash
brew install zhixuli0406/tap/duduclaw
```

### 一行安裝

```bash
curl -fsSL https://raw.githubusercontent.com/zhixuli0406/DuDuClaw/main/scripts/install.sh | sh
```

### Python SDK（必要依賴）

DuDuClaw 的進化引擎（Skill Vetter）與部分通道橋接需要 Python 環境：

```bash
pip install duduclaw
```

此命令會安裝以下必要依賴：

| 套件 | 最低版本 | 用途 |
|------|---------|------|
| `anthropic` | ≥ 0.40 | Claude API 直接呼叫、Skill 安全掃描 |
| `httpx` | ≥ 0.27 | 非同步 HTTP 客戶端（帳號輪替、健康檢查）|

開發環境額外安裝：

```bash
pip install duduclaw[dev]
# 包含：pytest>=8, pytest-asyncio>=0.24, ruff>=0.8
```

### 從原始碼建構

```bash
git clone https://github.com/zhixuli0406/DuDuClaw.git
cd DuDuClaw

# 安裝 Python 依賴
pip install duduclaw

# 建構 Dashboard
cd web && npm ci --legacy-peer-deps && npm run build && cd ..

# 建構 Rust binary（含 Dashboard）
cargo build --release -p duduclaw-cli -p duduclaw-gateway --features duduclaw-gateway/dashboard

# 首次設定
./target/release/duduclaw onboard

# 啟動
./target/release/duduclaw run
```

> **前置需求**：[Rust](https://rustup.rs/) 1.85+、[Python](https://www.python.org/) 3.9+、[Node.js](https://nodejs.org/) 20+，以及至少一個 AI CLI：[Claude Code](https://docs.anthropic.com/en/docs/claude-code)、[Codex](https://github.com/openai/codex)、[Gemini CLI](https://github.com/google-gemini/gemini-cli)（擇一或多個）

---

## CLI 指令

```
duduclaw onboard             # 互動式首次設定
duduclaw run                 # 一鍵啟動（gateway + channels + heartbeat + cron + dispatcher）
duduclaw migrate             # 將 agent.toml 轉換為 Claude Code 格式
duduclaw mcp-server          # 啟動 MCP Server（供 Claude Code 使用）
duduclaw gateway             # 僅啟動 WebSocket gateway server

duduclaw agent               # CLI 互動式對話
duduclaw agent list          # 列出所有 Agent
duduclaw agent create        # 建立新 Agent（可指定產業模板）
duduclaw agent inspect       # 檢視 Agent 詳情
duduclaw agent pause         # 暫停 Agent
duduclaw agent resume        # 恢復 Agent
duduclaw agent edit          # 編輯 Agent 設定
duduclaw agent remove        # 移除 Agent

duduclaw test <agent>        # 紅隊安全測試（9 項內建場景 + JSON 報告）
duduclaw status              # 系統健康快照
duduclaw doctor              # 健康診斷
duduclaw wizard              # 產業模板互動式設定

duduclaw service install     # 安裝為系統服務
duduclaw service start/stop  # 啟停系統服務
duduclaw service status      # 服務狀態
duduclaw service logs        # 服務日誌
duduclaw service uninstall   # 移除系統服務

duduclaw license activate    # 啟用授權
duduclaw license status      # 授權狀態
duduclaw license verify      # 驗證授權
duduclaw update              # 檢查並安裝更新
duduclaw version             # 版本資訊
```

---

## 專案結構

```
DuDuClaw/
├── crates/                         # Rust crates (13 個)
│   ├── duduclaw-core/              # 共用型別、traits (Channel, MemoryEngine)、錯誤定義
│   ├── duduclaw-agent/             # Agent 註冊、心跳、預算、契約、skill loader/registry
│   ├── duduclaw-auth/              # 多用戶認證（Argon2 密碼、JWT、ACL 角色權限）
│   ├── duduclaw-security/          # AES-256-GCM、SOUL guard、input guard、audit、key vault
│   ├── duduclaw-container/         # Docker / Apple Container / WSL2 沙箱執行
│   ├── duduclaw-memory/            # SQLite + FTS5 全文搜尋 + 向量嵌入
│   ├── duduclaw-inference/         # 本地推論引擎（llama.cpp / mistral.rs / ONNX / Exo / llamafile）
│   ├── duduclaw-gateway/           # Axum 伺服器、6 通道、session、GVU²、prediction、cron、dispatcher
│   ├── duduclaw-bus/               # tokio broadcast + mpsc 訊息路由
│   ├── duduclaw-bridge/            # PyO3 Rust↔Python 橋接層
│   ├── duduclaw-odoo/              # Odoo ERP 中間層 (JSON-RPC, CE/EE, 15 MCP tools)
│   ├── duduclaw-cli/               # clap CLI 入口 + MCP server + migrate + test
│   └── duduclaw-dashboard/         # rust-embed 嵌入 React SPA
│
├── python/duduclaw/                # Python 擴充層
│   ├── channels/                   # LINE / Telegram / Discord 通道插件
│   ├── sdk/                        # Claude Code SDK chat + 多帳號輪替
│   ├── evolution/                  # Skill Vetter 安全掃描
│   └── tools/                      # Agent 動態管理工具
│
├── npm/                            # npm 發布套件
│   ├── duduclaw/                   # 主套件（平台無關 wrapper + postinstall binary 下載）
│   ├── darwin-arm64/               # macOS Apple Silicon 預編譯 binary
│   ├── darwin-x64/                 # macOS Intel 預編譯 binary
│   ├── linux-x64/                  # Linux x86-64 預編譯 binary
│   ├── linux-arm64/                # Linux ARM64 預編譯 binary
│   └── win32-x64/                  # Windows x64 預編譯 binary
│
├── web/                            # React Dashboard
│   └── src/
│       ├── components/             # UI 元件 (OrgChart, ApprovalModal, SessionReplay)
│       ├── pages/                  # 23 個頁面
│       ├── stores/                 # Zustand 狀態管理 (8 stores)
│       ├── lib/                    # API client (WebSocket JSON-RPC)
│       └── i18n/                   # zh-TW / en / ja-JP
│
├── templates/                      # 產業模板 + Agent 角色模板
│   ├── restaurant/                 # 餐飲業（客服、訂位、FAQ、主動推播）
│   ├── manufacturing/              # 製造業（設備監控、SOP、異常告警）
│   ├── trading/                    # 貿易業（報價、訂單、庫存、價目表）
│   ├── evaluator/                  # Evaluator Agent（對抗式驗證）
│   ├── orchestrator/               # Orchestrator Agent（任務編排）
│   └── wiki/                       # Wiki 知識庫模板
│
├── .claude/                        # Claude Code Hook 安全系統
│   ├── settings.local.json         # Hook 設定（6 事件 × 10 腳本）
│   └── hooks/                      # 三層漸進式防禦腳本
│
├── docs/                           # 公開文件
│   ├── spec/                       # 格式規範（SOUL.md / CONTRACT.toml）
│   ├── api/                        # WebSocket RPC + OpenAPI spec
│   ├── guides/                     # 開發指南（自訂 MCP 工具等）
│   └── *.md                        # 架構、部署、進化引擎等
│
├── ARCHITECTURE.md                 # 完整架構設計文件
└── CLAUDE.md                       # AI 協作設計上下文
```

---

## 技術決策

| 項目 | 選擇 | 理由 |
|------|------|------|
| AI 對話 | **Multi-Runtime（Claude / Codex / Gemini CLI）** | 不綁定單一供應商、自動偵測 + per-agent 設定 |
| 核心語言 | **Rust** | 記憶體安全、高效能、單 binary 部署 |
| 擴充語言 | **Python (PyO3)** | Claude Code SDK 整合、通道插件彈性 |
| 前端框架 | **React 19 + TypeScript** | 即時資料更新、生態成熟 |
| UI 風格 | **shadcn/ui + Tailwind CSS 4** | 溫暖可自訂、效能佳 |
| 資料庫 | **SQLite + FTS5** | 零依賴、嵌入式、全文搜尋 |
| 工具協議 | **MCP (Model Context Protocol)** | Claude Code 原生支援、stdin/stdout JSON-RPC |
| 本地推論 | **ONNX Runtime + llama.cpp** | 跨平台、Metal/CUDA/Vulkan GPU 加速 |
| 語音辨識 | **SenseVoice + Whisper.cpp** | 多語言、本地離線、零 API 成本 |
| 即時通訊 | **WebRTC (LiveKit)** | 低延遲語音、多人會議 |

---

## 測試

```bash
# Rust 測試
cargo test --workspace --exclude duduclaw-bridge

# Python 測試
pip install pytest pytest-asyncio ruff
ruff check python/
pytest tests/python/ -v

# 前端型別檢查
cd web && npx tsc --noEmit
```

---

## 文件

- [ARCHITECTURE.md](ARCHITECTURE.md) — 完整系統架構設計
- [CLAUDE.md](CLAUDE.md) — AI 協作設計上下文與原則
- [docs/spec/soul-md-spec.md](docs/spec/soul-md-spec.md) — SOUL.md 格式規範 v1.0
- [docs/spec/contract-toml-spec.md](docs/spec/contract-toml-spec.md) — CONTRACT.toml 格式規範 v1.0
- [docs/api/README.md](docs/api/README.md) — WebSocket RPC 協議 + JSON-RPC 2.0 介面
- [docs/evolution-engine.md](docs/evolution-engine.md) — Evolution Engine v2 設計文件
- [docs/deployment-guide.md](docs/deployment-guide.md) — 生產環境部署指南
- [docs/development-guide.md](docs/development-guide.md) — 開發者設定與 Agent 開發
- [docs/guides/custom-mcp-tool.md](docs/guides/custom-mcp-tool.md) — 自訂 MCP 工具教學

---

## 授權

**Open Core 模式** — 核心程式碼採用 [Apache License 2.0](LICENSE)，完全自由使用、修改、分發。

商業加值模組（`commercial/` 目錄）為閉源付費，包含：產業模板、演化參數集、企業儀表板、授權驗證。

詳見 [LICENSING.md](LICENSING.md)。

---

<p align="center">
  🐾 Built with louis.li
</p>
