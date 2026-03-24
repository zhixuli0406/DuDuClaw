# DuDuClaw 🐾

> **Claude Code Extension Layer** — 讓 Claude Code 成為你的多通道 AI 助理

[![CI](https://github.com/zhixuli0406/DuDuClaw/actions/workflows/ci.yml/badge.svg)](https://github.com/zhixuli0406/DuDuClaw/actions/workflows/ci.yml)
[![Rust](https://img.shields.io/badge/Rust-2024_edition-orange?logo=rust)](https://www.rust-lang.org/)
[![Python](https://img.shields.io/badge/Python-3.10+-blue?logo=python)](https://www.python.org/)
[![License: MIT](https://img.shields.io/badge/License-MIT-green.svg)](LICENSE)
[![Version](https://img.shields.io/badge/version-0.6.1-blue)](https://github.com/zhixuli0406/DuDuClaw/releases)

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
  ├─ Evolution Engine — Micro / Meso / Macro 三層反思
  ├─ Account Rotator — 多 API key 輪替、預算追蹤、健康檢查
  └─ Web Dashboard — React SPA, 透過 rust-embed 嵌入 binary
```

---

## 核心特色

| 特色 | 說明 |
|------|------|
| **MCP Server 架構** | `duduclaw mcp-server` 提供 15+ 工具給 Claude Code，包括 send_message、send_photo、send_sticker、web_search、send_to_agent、memory_search、memory_store、create_agent、spawn_agent、list_agents、agent_status、skill_search、skill_list、schedule_task 等 |
| **Sub-Agent 編排** | `create_agent` 建立持久化 sub-agent + `spawn_agent` 背景派發 + `reports_to` 組織層級 + D3.js 組織架構圖 |
| **三通道支援** | Telegram (long polling)、LINE (webhook)、Discord (Gateway WebSocket) |
| **容器沙箱** | Docker / Apple Container 隔離執行（`--network=none`、tmpfs workspace、read-only rootfs、512MB limit） |
| **Session 管理** | SQLite 持久化對話歷程，超過 50k token 自動壓縮摘要 |
| **自主進化引擎** | Micro（每次對話後）→ Meso（per-agent 心跳）→ Macro（每日排程）三層反思，統一由 heartbeat scheduler 驅動 |
| **安全防護** | SOUL.md 漂移檢測（SHA-256）+ prompt injection 掃描（6 類規則）+ JSONL 審計日誌 + per-agent 密鑰隔離 |
| **行為契約** | `CONTRACT.toml` 定義 `must_not` / `must_always` 邊界 + `duduclaw test` 紅隊測試（9 項內建場景） |
| **Skill 市場** | OpenClaw 相容的 skill 格式解析 + 本地 registry 索引 + MCP 工具自主安裝 + Dashboard 市場頁面 |
| **多帳號輪替** | Python SDK 管理多個 Claude API key，支援 4 種策略 + 預算追蹤 + 故障轉移 |
| **Claude Code 原生目錄** | Agent 目錄包含 `.claude/`、`SOUL.md`、`CLAUDE.md`、`.mcp.json`，直接相容 Claude Code |
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
│   ├── duduclaw-cli/           # clap CLI 入口 + MCP server + migrate + test
│   └── duduclaw-dashboard/     # rust-embed 嵌入 React SPA
│
├── python/duduclaw/            # Python 擴充層
│   ├── channels/               # LINE / Telegram / Discord 通道插件
│   ├── sdk/                    # Claude Code SDK chat + 多帳號輪替
│   ├── evolution/              # 三層反思系統 + Skill Vetter 安全掃描
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
├── docs/                       # 研究文件
│   └── claw-ecosystem-report.md
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

---

## 授權

MIT License

---

<p align="center">
  🐾 Built with louis.li
</p>
