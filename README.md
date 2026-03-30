# DuDuClaw 🐾

> **Claude Code Extension Layer** — 讓 Claude Code 成為你的多通道 AI 助理

[![CI](https://github.com/zhixuli0406/DuDuClaw/actions/workflows/ci.yml/badge.svg)](https://github.com/zhixuli0406/DuDuClaw/actions/workflows/ci.yml)
[![Rust](https://img.shields.io/badge/Rust-2024_edition-orange?logo=rust)](https://www.rust-lang.org/)
[![Python](https://img.shields.io/badge/Python-3.10+-blue?logo=python)](https://www.python.org/)
[![License: Elastic-2.0](https://img.shields.io/badge/License-Elastic--2.0-blue.svg)](LICENSE)
[![Version](https://img.shields.io/badge/version-0.9.6-blue)](https://github.com/zhixuli0406/DuDuClaw/releases)

---

## 什麼是 DuDuClaw？

DuDuClaw **不是**一個獨立的 AI 平台。它是 **Claude Code 的擴充層 (extension layer)**——替 Claude Code 接上通訊通道、記憶、進化與帳號管理能力。

核心概念：

- **Brain = Claude Code SDK** — AI 對話由 `claude` CLI 處理，享受內建的 bash、web search、file ops 等工具
- **Plumbing = DuDuClaw** — 負責通道路由、session 管理、記憶搜尋、帳號輪替等基礎設施
- **橋接 = MCP Protocol** — `duduclaw mcp-server` 作為 MCP Server，將通道與記憶工具暴露給 Claude Code

```
Claude Code SDK (brain)
  ↕ MCP Protocol (JSON-RPC 2.0, stdin/stdout)
DuDuClaw (plumbing)
  ├─ Channel Router — Telegram (polling) / LINE (webhook) / Discord (Gateway WebSocket)
  ├─ Session Manager — SQLite, 50k token 自動壓縮
  ├─ MCP Server — send_message, send_photo, web_search, memory_search, memory_store …
  ├─ Evolution Engine — 預測驅動進化 + GVU 自我博弈迴圈
  ├─ Account Rotator — 多 API key 輪替、預算追蹤、健康檢查
  └─ Web Dashboard — React SPA, 透過 rust-embed 嵌入 binary
```

---

## 核心特色

| 特色 | 說明 |
|------|------|
| **MCP Server 架構** | `duduclaw mcp-server` 提供 30+ 工具給 Claude Code，涵蓋通訊、記憶、Agent 管理、Skill 市場、Odoo ERP（CRM/銷售/庫存/會計）|
| **Sub-Agent 編排** | `create_agent` 建立持久化 sub-agent + `spawn_agent` 背景派發 + `reports_to` 組織層級 + D3.js 組織架構圖 |
| **三通道支援** | Telegram (long polling)、LINE (webhook)、Discord (Gateway WebSocket) |
| **容器沙箱** | Docker / Apple Container 隔離執行（`--network=none`、tmpfs workspace、read-only rootfs、512MB limit） |
| **Session 管理** | SQLite 持久化對話歷程，超過 50k token 自動壓縮摘要 |
| **自主進化引擎** | 預測驅動進化（~90% 對話零 LLM 成本）+ GVU 自我博弈迴圈（Generator→Verifier→Updater, 4 層驗證）+ SOUL.md 版本控制（24h 觀察期 + 自動回滾）+ 5 種外部因素介入 |
| **安全防護** | SOUL.md 漂移檢測（SHA-256）+ prompt injection 掃描（6 類規則）+ JSONL 審計日誌 + per-agent 密鑰隔離 + **Claude Code Security Hooks（三層漸進式防禦）** |
| **行為契約** | `CONTRACT.toml` 定義 `must_not` / `must_always` 邊界 + `duduclaw test` 紅隊測試（9 項內建場景） |
| **Odoo ERP 整合** | 中間層 `duduclaw-odoo` 支援 CE/EE — 15 個 MCP 工具（CRM/銷售/庫存/會計/通用搜尋報表）+ 事件橋接 |
| **Skill 市場** | GitHub Search API 即時索引真實 skill repo + 24h 本地快取 + Dashboard 市場頁面 |
| **雙模式帳號輪替** | OAuth 訂閱帳號 + API Key 混合輪替 — 4 種策略（LeastCost 優先 OAuth 零成本）、健康追蹤、Rate limit 冷卻、預算強制 |
| **Claude Code 原生目錄** | Agent 目錄包含 `.claude/`、`SOUL.md`、`CLAUDE.md`、`.mcp.json`，直接相容 Claude Code |
| **Claude Code Security Hooks** | 三層漸進式防禦 — Layer 1 黑名單（<50ms）→ Layer 2 混淆偵測（YELLOW+）→ Layer 3 Haiku AI 研判（RED only）；威脅等級自動升降級；敏感檔案 R/W 保護；Secret 洩漏掃描（20+ 模式）；async 審計日誌（相容 Rust AuditEvent） |
| **Web Dashboard** | React + TypeScript + Tailwind CSS，溫暖 amber 色系，支援深淺色切換，含組織架構圖、Skill 市場、安全審計頁 |

---

## Agent 目錄結構

每個 Agent 是一個資料夾，結構與 Claude Code 完全相容：

```
~/.duduclaw/agents/
├── dudu/                    # 主 Agent
│   ├── .claude/             # Claude Code 設定
│   │   └── settings.local.json
│   ├── .mcp.json            # MCP Server 設定（指向 duduclaw mcp-server）
│   ├── SOUL.md              # 人格定義
│   ├── CLAUDE.md            # Claude Code 指引
│   ├── agent.toml           # DuDuClaw 特定設定（模型、預算、心跳）
│   ├── SKILLS/              # 技能集（可由進化引擎自動產出）
│   ├── memory/              # 每日筆記
│   └── state/               # 運行時狀態 (SQLite)
│
└── coder/                   # 另一個 Agent
    └── ...
```

使用 `duduclaw migrate` 可將舊版 `agent.toml` 自動轉換為 Claude Code 相容格式（產生 `.claude/`、`CLAUDE.md`、`.mcp.json`）。

---

## Security Hooks 安全防禦系統

DuDuClaw 在 Claude Code 的 Hook 系統上建構了三層漸進式防禦，保護開發環境免受惡意命令、秘密洩漏和配置篡改：

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

### 保護範圍

- **30+ 危險命令模式**：rm -rf、fork bomb、curl pipe sh、DROP TABLE、chmod 777 等
- **20+ Secret 洩漏模式**：Anthropic/OpenAI/AWS/GitHub/Slack/Stripe/Vault/DB URL 等
- **敏感檔案保護**：`secret.key`、`.env*`、`.credentials.json`、`SOUL.md`、`CONTRACT.toml`、`threat_level`
- **Read tool 保護**：防止讀取敏感檔案洩漏至 context
- **AI 提示注入防護**：所有 Haiku 提示使用 XML 分隔標籤標記不受信任輸入
- **審計日誌**：每次工具呼叫 async 寫入 JSONL，格式相容 Rust `audit.rs` 的 `AuditEvent` schema

> 詳細設計見 [docs/TODO-security-hooks.md](docs/TODO-security-hooks.md)，
> Code Review 報告見 [docs/code-review-security-hooks.md](docs/code-review-security-hooks.md)。

---

## 安裝

### Homebrew（macOS / Linux）

```bash
brew install zhixuli0406/tap/duduclaw
```

### 一行安裝

```bash
curl -fsSL https://raw.githubusercontent.com/zhixuli0406/DuDuClaw/main/scripts/install.sh | sh
```

### 從原始碼建構

```bash
git clone https://github.com/zhixuli0406/DuDuClaw.git
cd DuDuClaw

# 建構 Dashboard
cd web && npm ci --legacy-peer-deps && npm run build && cd ..

# 建構 Rust binary（含 Dashboard）
cargo build --release -p duduclaw-cli -p duduclaw-gateway --features duduclaw-gateway/dashboard

# 首次設定
./target/release/duduclaw onboard

# 啟動
./target/release/duduclaw run
```

> **前置需求**：[Rust](https://rustup.rs/) 1.85+、[Python](https://www.python.org/) 3.10+、[Node.js](https://nodejs.org/) 20+、[Claude Code](https://docs.anthropic.com/en/docs/claude-code) (`claude` CLI)

---

## CLI 指令

```
duduclaw onboard         # 互動式首次設定
duduclaw run             # 一鍵啟動（gateway + channels + heartbeat + cron + dispatcher）
duduclaw migrate         # 將 agent.toml 轉換為 Claude Code 格式
duduclaw mcp-server      # 啟動 MCP Server（供 Claude Code 使用）
duduclaw agent           # CLI 互動式對話
duduclaw agent list      # 列出所有 Agent
duduclaw agent create    # 建立新 Agent
duduclaw agent inspect   # 檢視 Agent 詳情
duduclaw test <agent>    # 紅隊安全測試（9 項內建場景 + JSON 報告）
duduclaw status          # 系統健康快照
duduclaw doctor          # 健康診斷
duduclaw service install # 安裝為系統服務
```

---

## 專案結構

```
DuDuClaw/
├── crates/                     # Rust crates (10 個)
│   ├── duduclaw-core/          # 共用型別、traits (Channel, MemoryEngine)、錯誤定義
│   ├── duduclaw-agent/         # Agent 註冊、心跳、預算、契約、skill loader/registry
│   ├── duduclaw-security/      # AES-256-GCM、SOUL guard、input guard、audit、key vault
│   ├── duduclaw-container/     # Docker / Apple Container / WSL2 沙箱執行
│   ├── duduclaw-memory/        # SQLite + FTS5 全文搜尋 + 向量嵌入
│   ├── duduclaw-gateway/       # Axum 伺服器、通道、session、evolution、cron、dispatcher
│   ├── duduclaw-bus/           # tokio broadcast + mpsc 訊息路由
│   ├── duduclaw-bridge/        # PyO3 Rust↔Python 橋接層
│   ├── duduclaw-odoo/           # Odoo ERP 中間層 (JSON-RPC, CE/EE)
│   ├── duduclaw-cli/           # clap CLI 入口 + MCP server + migrate + test
│   └── duduclaw-dashboard/     # rust-embed 嵌入 React SPA
│
├── python/duduclaw/            # Python 擴充層
│   ├── channels/               # LINE / Telegram / Discord 通道插件
│   ├── sdk/                    # Claude Code SDK chat + 多帳號輪替
│   ├── evolution/              # Skill Vetter 安全掃描
│   └── tools/                  # Agent 動態管理工具
│
├── web/                        # React Dashboard
│   └── src/
│       ├── components/         # UI 元件 (OrgChart, Dialog, Layout)
│       ├── pages/              # 10 個頁面（Dashboard, Agents, OrgChart, SkillMarket, ...）
│       ├── stores/             # Zustand 狀態管理
│       ├── lib/                # API client (WebSocket JSON-RPC)
│       └── i18n/               # zh-TW / en
│
├── .claude/                    # Claude Code Hook 安全系統
│   ├── settings.local.json     # Hook 設定（6 事件 × 10 腳本）
│   └── hooks/
│       ├── lib/threat-level.sh # 共享函式庫（狀態機、timeout、TOML 解析）
│       ├── bash-gate.sh        # PreToolUse Bash — 三層防禦
│       ├── file-protect.sh     # PreToolUse Write|Edit|Read — 敏感檔案保護
│       ├── secret-scanner.sh   # PostToolUse — Secret 洩漏掃描（20+ 模式）
│       ├── inject-contract.sh  # UserPromptSubmit — CONTRACT.toml 注入
│       ├── session-init.sh     # SessionStart — 環境初始化 + 密鑰驗證
│       ├── audit-logger.sh     # PostToolUse — async JSONL 審計
│       ├── config-guard.sh     # ConfigChange — 配置篡改偵測
│       ├── threat-eval.sh      # Stop — 威脅等級重新評估
│       └── security-file-ai-review.sh  # PreToolUse — RED 模式 AI 審查
│
├── docs/                       # 研究文件
│   ├── TODO-security-hooks.md  # Security Hooks 設計 + 實作追蹤
│   ├── code-review-security-hooks.md # Security Hooks 深度 Code Review
│   ├── claw-ecosystem-report.md
│   └── ...
├── ARCHITECTURE.md             # 完整架構設計文件
├── Phase2-TODO.md              # Phase 2 任務追蹤（21/21 完成）
└── CLAUDE.md                   # AI 協作設計上下文
```

---

## 技術決策

| 項目 | 選擇 | 理由 |
|------|------|------|
| AI 對話 | **Claude Code SDK (`claude` CLI)** | 內建工具鏈、MCP 相容、官方支援 |
| 核心語言 | **Rust** | 記憶體安全、高效能、單 binary 部署 |
| 擴充語言 | **Python (PyO3)** | Claude Code SDK 整合、通道插件彈性 |
| 前端框架 | **React 19 + TypeScript** | 即時資料更新、生態成熟 |
| UI 風格 | **shadcn/ui + Tailwind CSS 4** | 溫暖可自訂、效能佳 |
| 資料庫 | **SQLite + FTS5** | 零依賴、嵌入式、全文搜尋 |
| 工具協議 | **MCP (Model Context Protocol)** | Claude Code 原生支援、stdin/stdout JSON-RPC |

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
- [docs/TODO-security-hooks.md](docs/TODO-security-hooks.md) — Security Hooks 設計文件與實作追蹤（Phase 1~4）
- [docs/code-review-security-hooks.md](docs/code-review-security-hooks.md) — Security Hooks 深度 Code Review（6 CRITICAL + 14 HIGH 全部修復）
- [docs/account-rotation-guide.md](docs/account-rotation-guide.md) — 帳號輪替使用教學（OAuth + API Key）
- [docs/odoo-integration-plan.md](docs/odoo-integration-plan.md) — Odoo ERP 整合規劃
- [docs/claw-ecosystem-report.md](docs/claw-ecosystem-report.md) — Claw 生態系統研究報告

---

## 授權

[Elastic License 2.0 (ELv2)](LICENSE) — 個人、教育、企業內部使用免費。詳見 [LICENSING.md](LICENSING.md)。

---

<p align="center">
  🐾 Built with louis.li
</p>
