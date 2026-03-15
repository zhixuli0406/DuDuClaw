# DuDuClaw 🐾

> **Multi-Agent AI Assistant Platform** — 讓 AI 助理在安全容器中自主進化

[![CI](https://github.com/zhixuli0406/DuDuClaw/actions/workflows/ci.yml/badge.svg)](https://github.com/zhixuli0406/DuDuClaw/actions/workflows/ci.yml)
[![Rust](https://img.shields.io/badge/Rust-2024_edition-orange?logo=rust)](https://www.rust-lang.org/)
[![Python](https://img.shields.io/badge/Python-3.10+-blue?logo=python)](https://www.python.org/)
[![License: MIT](https://img.shields.io/badge/License-MIT-green.svg)](LICENSE)

---

## 什麼是 DuDuClaw？

DuDuClaw 是一個**個人 AI 助理平台**，讓你在本機或伺服器上運行多個 AI Agent，每個 Agent 都在安全隔離的容器中執行，並透過 LINE、Telegram、Discord 等通道與你互動。

核心特色：
- **多 Agent 架構** — 同時運行多個專精 Agent（主助理、程式專家、研究員…），各有獨立人格與技能
- **安全容器隔離** — 每個 Agent 在 Docker / Apple Container / WSL2 容器中執行，敏感資料永遠不進容器
- **自主進化** — Agent 透過三層反思系統自動學習與改進，產出的技能經安全掃描後才啟用
- **多帳號輪替** — 智慧管理多個 Claude API 帳號，自動切換、預算控管、故障轉移
- **OpenClaw 相容** — WebSocket RPC 協議相容 OpenClaw 生態，可直接使用現有客戶端
- **Web Dashboard** — 內建溫暖風格的管理介面，即時監控所有 Agent 與通道狀態

---

## 系統架構

```
┌─────────────────────────────────────────────────────────┐
│                    DuDuClaw Server                       │
│                                                         │
│  ┌────────────────── Rust Core ──────────────────────┐  │
│  │                                                   │  │
│  │  Gateway (Axum)    Message Bus    Agent Registry   │  │
│  │  ├ WebSocket RPC   ├ broadcast    ├ 資料夾掃描     │  │
│  │  ├ HTTP API        ├ mpsc         ├ 觸發詞路由     │  │
│  │  └ Web Dashboard   └ 事件分發     └ 權限檢查       │  │
│  │                                                   │  │
│  │  ┌─────────── Security Layer ──────────────────┐  │  │
│  │  │ Credential Proxy │ Mount Guard │ RBAC │ Rate │  │  │
│  │  └─────────────────────────────────────────────┘  │  │
│  │                                                   │  │
│  │  Memory Engine     Container Manager   Heartbeat  │  │
│  │  (SQLite+FTS5+Vec) (Docker/Apple/WSL2) Scheduler  │  │
│  └───────────────────────────────────────────────────┘  │
│                                                         │
│  ┌────────────── Python Extension (PyO3) ────────────┐  │
│  │  Channels          Evolution Engine   Claude SDK   │  │
│  │  ├ LINE            ├ Micro Reflect    Rotator      │  │
│  │  ├ Telegram        ├ Meso Reflect     ├ 多帳號     │  │
│  │  └ Discord         ├ Macro Reflect    ├ 預算追蹤   │  │
│  │                    └ Skill Vetter     └ 健康檢查   │  │
│  └───────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────┘
```

### Rust Crates（9 個）

| Crate | 職責 |
|-------|------|
| `duduclaw-core` | 共用型別、traits（Channel, ContainerRuntime, MemoryEngine）、錯誤定義 |
| `duduclaw-agent` | Agent 註冊、資料夾掃描、agent.toml 解析、IPC、心跳排程、預算控管 |
| `duduclaw-container` | Docker (bollard)、Apple Container、WSL2 Direct 容器管理 |
| `duduclaw-security` | AES-256-GCM 加密、Credential Proxy、Mount Guard、RBAC、滑動視窗限流 |
| `duduclaw-memory` | SQLite + FTS5 全文搜尋 + 向量嵌入 cosine similarity |
| `duduclaw-gateway` | Axum WebSocket RPC (OpenClaw 相容)、token 認證、HTTP API |
| `duduclaw-bus` | tokio broadcast + mpsc 訊息路由 |
| `duduclaw-bridge` | PyO3 Rust↔Python 橋接層 |
| `duduclaw-cli` | clap CLI (onboard, agent, gateway, doctor, status, service) |
| `duduclaw-dashboard` | rust-embed 嵌入 React SPA |

### Python 模組

| 模組 | 職責 |
|------|------|
| `channels/` | LINE、Telegram、Discord 通道插件 + 自動發現 |
| `sdk/` | Claude API 多帳號輪替（4 種策略）+ 預算追蹤 + 健康檢查 |
| `evolution/` | 三層反思系統（Micro/Meso/Macro）+ Skill Vetter 安全掃描 |
| `tools/` | Agent 動態管理工具（create/delegate/pause/resume） |

---

## 多 Agent 架構

DuDuClaw 的每個 Agent 都是一個**資料夾**，結構清晰直覺：

```
~/.duduclaw/agents/
├── dudu/                    # 主 Agent
│   ├── agent.toml           # 設定（模型、預算、權限、心跳）
│   ├── SOUL.md              # 人格定義
│   ├── IDENTITY.md          # 身份（名稱、觸發詞）
│   ├── MEMORY.md            # 長期記憶
│   ├── SKILLS/              # 技能集（可自動產生）
│   │   ├── research.md
│   │   └── coding.md
│   ├── memory/              # 每日筆記
│   └── state/               # 運行時狀態 (SQLite)
│
├── coder/                   # 編碼專家 Agent
│   ├── agent.toml
│   ├── SOUL.md
│   └── SKILLS/
│       └── rust-patterns.md
│
└── researcher/              # 研究員 Agent
    └── ...
```

### Agent 路由

```
訊息進入 → Trigger Router → "@DuDu 幫我..." → dudu agent
                          → "@Coder 寫個..." → coder agent
                          → "幫我研究..."    → dudu agent (預設)
```

### Agent 間通訊（IPC）

Agent 之間透過 JSON 檔案佇列通訊，確保完全隔離：

```
Agent A → IPC Broker → 權限驗證 → Agent B
          (JSON file)   ↓
                     主機 Watcher
```

---

## 安全系統

DuDuClaw 的安全設計遵循**零信任原則**：

| 機制 | 說明 |
|------|------|
| **Credential Proxy** | API 金鑰永遠不進容器，透過代理存取 |
| **Mount Guard** | 白名單掛載、自動阻擋 .ssh/.gnupg/.env、symlink 解析防穿越 |
| **RBAC Engine** | 權限繼承上限（子 Agent 不可超越建立者）、SOUL.md 防自我修改 |
| **Rate Limiter** | 每 Agent / 每通道獨立滑動視窗限流 |
| **Skill Vetter** | 5 大類安全掃描（命令注入、路徑穿越、prompt injection、資源濫用、敏感資料） |
| **Container Hardening** | 非 root (uid 1000)、唯讀掛載、一次性容器 (--rm) |

---

## 自主進化引擎

Agent 透過三層反思系統持續自我改進：

```
Layer 1: Micro Reflection    每次對話後 → 寫入每日筆記
Layer 2: Meso Reflection     每小時心跳 → 萃取共同模式 → 更新 MEMORY.md
Layer 3: Macro Reflection    每日排程   → 審計技能 → 產出進化報告

候選技能 → Skill Vetter 安全掃描
          ├ PASS → 自動啟用
          ├ WARN → 啟用但標記
          └ FAIL → 隔離至 quarantine/
```

---

## Web Dashboard

內建的 Web 管理介面，使用 React + TypeScript + Tailwind CSS 建構，透過 rust-embed 嵌入 Rust binary。

**8 個頁面**：首頁總覽 · Agent 管理 · 通道管理 · 帳號與預算 · 記憶與技能 · 安全設定 · 系統設定 · 即時日誌

**設計風格**：溫暖 amber 色系、Claude.ai + Raycast 風格、支援深淺色切換

```
duduclaw run → 瀏覽 http://localhost:18789
```

---

## 快速開始

### 前置需求

- [Rust](https://rustup.rs/) (1.82+)
- [Python](https://www.python.org/) (3.10+)
- [Docker](https://www.docker.com/) 或 [Apple Container](https://developer.apple.com/) (macOS 26+)
- [Node.js](https://nodejs.org/) (20+, 僅建構 Dashboard 時需要)

### 從原始碼安裝

```bash
git clone https://github.com/zhixuli0406/DuDuClaw.git
cd DuDuClaw

# 建構 Dashboard
cd web && npm ci && npm run build && cd ..

# 建構 Rust binary
cargo build --release --workspace --exclude duduclaw-bridge

# 安裝 Python 擴充
pip install -e .

# 首次設定
./target/release/duduclaw onboard

# 啟動
./target/release/duduclaw run
```

### Docker 一行部署

```bash
ANTHROPIC_API_KEY=sk-ant-... docker compose -f docker-compose.quickstart.yml up -d
```

### CLI 指令

```
duduclaw onboard         # 互動式首次設定
duduclaw run             # 一鍵啟動（onboard + doctor + gateway）
duduclaw agent           # CLI 互動式對話
duduclaw agent list      # 列出所有 Agent
duduclaw agent create    # 建立新 Agent
duduclaw status          # 系統健康快照
duduclaw doctor          # 9 項健康診斷
duduclaw service install # 安裝為系統服務
```

---

## 專案結構

```
DuDuClaw/
├── crates/                  # Rust crates (9 個)
│   ├── duduclaw-core/       # 共用型別、traits
│   ├── duduclaw-agent/      # Agent 管理 + IPC + 預算
│   ├── duduclaw-container/  # Docker / Apple / WSL2
│   ├── duduclaw-security/   # 加密、RBAC、限流
│   ├── duduclaw-memory/     # SQLite + FTS5 + 向量
│   ├── duduclaw-gateway/    # WebSocket RPC
│   ├── duduclaw-bus/        # 訊息路由
│   ├── duduclaw-bridge/     # PyO3 橋接
│   ├── duduclaw-cli/        # CLI 入口
│   └── duduclaw-dashboard/  # Web Dashboard 嵌入
│
├── python/duduclaw/         # Python 擴充層
│   ├── channels/            # LINE / Telegram / Discord
│   ├── sdk/                 # 多帳號輪替
│   ├── evolution/           # 自主進化引擎
│   └── tools/               # Agent MCP 工具
│
├── web/                     # React Dashboard
│   └── src/
│       ├── components/      # UI 元件
│       ├── pages/           # 8 個頁面
│       ├── stores/          # Zustand 狀態管理
│       ├── lib/             # WsClient + API
│       └── i18n/            # zh-TW / en
│
├── container/               # Dockerfile + entrypoints
├── config/                  # 設定範例
├── ARCHITECTURE.md          # 完整架構設計文件
└── CLAUDE.md                # AI 協作設計上下文
```

---

## 技術決策

| 項目 | 選擇 | 理由 |
|------|------|------|
| 核心語言 | **Rust** | 安全關鍵路徑需要記憶體安全 |
| 擴充語言 | **Python (PyO3)** | Claude Code SDK 官方支援 |
| 容器引擎 | **Docker + Apple Container + WSL2** | 跨平台支援 |
| 前端框架 | **React 19 + TypeScript** | 即時資料更新、生態成熟 |
| UI 風格 | **shadcn/ui + Tailwind CSS 4** | 溫暖可自訂、效能佳 |
| 資料庫 | **SQLite + FTS5** | 零依賴、嵌入式、全文搜尋 |
| RPC 協議 | **WebSocket (OpenClaw 相容)** | 即時雙向通訊 |
| 加密 | **AES-256-GCM (ring)** | 業界標準、Rust 原生 |

---

## 支援平台

| 平台 | 容器引擎 | 系統服務 |
|------|----------|----------|
| **macOS** | Docker Desktop / Apple Container | launchd |
| **Linux** | Docker / Podman | systemd |
| **Windows** | Docker Desktop / WSL2 Direct | Windows Service |

---

## 測試

```bash
# Rust 測試 (43 tests)
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

- [ARCHITECTURE.md](ARCHITECTURE.md) — 完整系統架構設計（含 13 章節）
- [CLAUDE.md](CLAUDE.md) — AI 協作設計上下文與原則

---

## 授權

MIT License

---

<p align="center">
  🐾 Built with love for AI companions
</p>
