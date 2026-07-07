# DuDuClaw 🐾

<div align="center">

**繁體中文** · [English](README.en.md) · [日本語](README.ja.md)

</div>

DuDuClaw 把 Claude Code、Codex、Gemini 這些 AI 指令列工具,接上 Telegram、LINE、Discord 等九個通訊平台,變成一個 24 小時值班、會記得你、會自我改進的 AI 助理。

你只需要一個 Rust binary。通道路由、對話記憶、多帳號輪替、行為安全邊界、本地推論、Web 管理後台全部內建;AI 大腦用哪家隨你換,設定和記憶都留在你自己的機器上。核心採 Apache 2.0 授權。

[![CI](https://github.com/zhixuli0406/DuDuClaw/actions/workflows/ci.yml/badge.svg)](https://github.com/zhixuli0406/DuDuClaw/actions/workflows/ci.yml)
[![Version](https://img.shields.io/badge/version-1.34.0-blue)](https://github.com/zhixuli0406/DuDuClaw/releases)
[![npm](https://img.shields.io/npm/v/duduclaw?logo=npm)](https://www.npmjs.com/package/duduclaw)
[![PyPI](https://img.shields.io/pypi/v/duduclaw?logo=pypi)](https://pypi.org/project/duduclaw/)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache--2.0-blue.svg)](LICENSE)

https://github.com/user-attachments/assets/9f18408a-cf46-4db2-9ab0-dcc8db2486fc

## 目錄

- [為什麼需要 DuDuClaw?](#why)
- [架構一覽](#architecture)
- [安裝](#install)
- [快速開始](#quickstart)
- [功能總覽](#features)
- [CLI 指令](#cli)
- [信任與安全](#trust)
- [競品對比](#comparison)
- [文件](#docs)
- [授權](#license)

<a id="why"></a>

## 為什麼需要 DuDuClaw?

偶爾在終端機裡跑 `claude` 或 `gemini`,原生 CLI 就夠了。想讓 AI 進駐你的 LINE 官方帳號、幫團隊在 Discord 值班、或是同時管理多個各有分工的 agent,你就得自己蓋一整層基礎設施。DuDuClaw 把這層蓋好了:

| 需求 | 原生 CLI | DuDuClaw |
|---|---|---|
| 接上 Telegram / LINE / Discord | 只能在終端機用 | 9 個通道,per-agent bot token |
| 多 LLM 容錯切換 | 手動重啟 | 內建 4 種輪替策略 + 跨供應商 failover |
| 換 LLM 時保留上下文 | 遺失 | 完整保留 |
| 對話記憶與知識庫 | 單次 session | SQLite 時態記憶 + 分層 wiki + 自動注入 |
| 工具跨 LLM 共用 | 每家重寫 | MCP 工具寫一次,五種後端共用 |
| 安全邊界 / 稽核 / 密鑰管理 | 自己造 | 政策核心 + OS 沙箱 + AES-256-GCM 內建 |

<a id="architecture"></a>

## 架構一覽

AI Runtime 是大腦,DuDuClaw 是水電管線,中間用 MCP(JSON-RPC 2.0)橋接:

```
AI Runtime (brain) — Claude Code / Codex / Gemini / Antigravity / OpenAI-compat
  ↕ MCP Protocol (JSON-RPC 2.0, stdin/stdout)
DuDuClaw (plumbing)
  ├─ Channel Router — Telegram / LINE / Discord / Slack / WhatsApp / Feishu
  │                    / Google Chat / Microsoft Teams / WebChat
  ├─ Multi-Runtime — 5 種後端自動偵測,per-agent 設定
  ├─ Session Memory — 原生 --resume + 時態記憶 + key-fact 累積 + 分層 wiki
  ├─ MCP Server — 80+ 工具(通訊、記憶、Agent、Skill、任務、知識庫、ERP)
  ├─ Evolution Engine — GVU² 雙迴圈進化 + 預測驅動 + MistakeNotebook
  ├─ Security — PolicyKernel reference monitor + OS 沙箱 + redaction vault
  ├─ Inference Engine — llama.cpp / mistral.rs / Exo P2P / llamafile / MLX
  ├─ Account Rotator — 多 OAuth + API Key 輪替、預算追蹤、健康檢查
  └─ Web Dashboard — React 19 SPA(24 頁),rust-embed 嵌入 binary
```

完整設計見 [ARCHITECTURE.md](ARCHITECTURE.md)。

<a id="install"></a>

## 安裝

### npm(推薦,所有平台含 Windows)

前置需求只有 [Node.js](https://nodejs.org/) 20+:

```bash
npm install -g duduclaw
```

會自動安裝對應平台的預編譯 binary(macOS ARM64/x64、Linux x64/ARM64、Windows x64),免編譯器、免 Rust。

> ⚠️ 如果安裝過程要求你裝 Rust / MSVC Build Tools 並編譯 1.5 小時,代表走錯路徑了。那是給貢獻者的「從原始碼建構」;一般使用請用上面的 npm 指令。

### Homebrew(macOS / Linux)

```bash
brew install zhixuli0406/tap/duduclaw
```

### 一行安裝

```bash
# macOS / Linux
curl -fsSL https://raw.githubusercontent.com/zhixuli0406/DuDuClaw/main/scripts/install.sh | sh
```

```powershell
# Windows (PowerShell)
irm https://raw.githubusercontent.com/zhixuli0406/DuDuClaw/main/scripts/install.ps1 | iex
```

### 桌面應用程式

Tauri 原生桌面版,啟動時自動拉起本機 gateway,與 CLI 共用 `~/.duduclaw`。到 [Releases](https://github.com/zhixuli0406/DuDuClaw/releases) 下載:

| 平台 | 檔案 | 備註 |
|------|------|------|
| macOS(Apple Silicon / Intel) | `DuDuClaw_*.dmg` | 已簽章 + Apple 公證,直接開 |
| Windows x64 | `DuDuClaw_*_x64_en-US.msi` | 未購買 Authenticode 憑證,SmartScreen 會示警;點「更多資訊」→「仍要執行」。介意的話請改用 CLI 版 |
| Linux | `*_amd64.AppImage` / `.deb` | 免簽 |

### 從原始碼建構

前置需求:[Rust](https://rustup.rs/) 1.85+、[Node.js](https://nodejs.org/) 20+。

```bash
git clone https://github.com/zhixuli0406/DuDuClaw.git
cd DuDuClaw
cd web && npm ci --legacy-peer-deps && npm run build && cd ..
cargo build --release -p duduclaw-cli -p duduclaw-gateway --features duduclaw-gateway/dashboard
./target/release/duduclaw run
```

### Python SDK(選用函式庫)

核心 gateway / CLI 是 Rust binary,不需要 Python。PyPI 上的 `duduclaw` 是給 `import duduclaw` 用的純函式庫(agents / channels / mcp / memory_eval 模組),沒有命令列工具,所以 `pipx install duduclaw` 會失敗是預期行為。需要時:

```bash
pip install duduclaw
```

<a id="quickstart"></a>

## 快速開始

還需要一個 AI 大腦,四選一(可以之後在瀏覽器引導中設定):裝好 [Claude Code](https://docs.anthropic.com/en/docs/claude-code)、[Codex](https://github.com/openai/codex) 或 [Gemini CLI](https://github.com/google-gemini/gemini-cli) 其中之一並登入;或準備一把 API key;或用本地 GGUF 模型。

```bash
# 1. 啟動(gateway + 通道 + 排程 + dispatcher 一次拉起)
duduclaw run

# 2. 打開管理後台
open http://localhost:18789
```

第一次打開會進入三步驟引導:選 AI 後端 → 建立第一個 agent → 直接在內建 WebChat 對話。之後到 Channels 頁貼上 bot token,就能把同一個 agent 接上 Telegram、LINE、Discord 等平台,不用重啟。

常用的下一步:

```bash
duduclaw agent create      # 建立更多 agent(可指定產業模板)
duduclaw status            # 系統健康快照
duduclaw update            # 檢查並安裝新版本
duduclaw service install   # 開機自動啟動(launchd / systemd)
```

<a id="features"></a>

## 功能總覽

| 領域 | 內建能力 | 深入閱讀 |
|------|----------|----------|
| 通訊通道 | 9 通道(Telegram / LINE / Discord / Slack / WhatsApp / Feishu / Google Chat / Teams / WebChat),per-agent bot、熱啟停、平台原生排版、輸入中指示、長任務進度看板 | [docs/features](docs/features/README.md) |
| Multi-Runtime | Claude / Codex / Gemini / Antigravity / OpenAI-compat 五後端,自動偵測、per-agent 設定、換後端保留上下文 | [ARCHITECTURE.md](ARCHITECTURE.md) |
| MCP Server | 80+ 工具:通訊、記憶、agent 編排、skill 市場、任務看板、共享 wiki、Odoo ERP;stdio 與 HTTP/SSE 雙 transport | [docs/api](docs/api/README.md) |
| 記憶系統 | SQLite 時態記憶(事實取代鏈)、Ebbinghaus 遺忘曲線、知識圖譜檢索、跨 agent 共享 wiki | [docs/features](docs/features/README.md) |
| 自我進化 | GVU² 雙迴圈 + 預測驅動(約 90% 對話零 LLM 成本)、SOUL.md 版控 + 24h 觀察期自動回滾 | [evolution-engine.md](docs/architecture/evolution-engine.md) |
| 安全 | PolicyKernel reference monitor(零 LLM、fail-closed)、macOS Seatbelt / Linux Landlock 沙箱、secret redaction vault、CONTRACT.toml 行為契約 + 紅隊測試 | [SECURITY.md](SECURITY.md) |
| 帳號與成本 | 多 OAuth + API Key 輪替(4 策略)、rate-limit / 帳單冷卻、成本遙測與快取效率分析 | [docs/features](docs/features/README.md) |
| 本地推論 | llama.cpp(Metal/CUDA/Vulkan)/ mistral.rs / Exo P2P / llamafile / MLX,三層信心路由自動分流 | [docs/features](docs/features/README.md) |
| 語音 | ASR(SenseVoice / Whisper)+ TTS(Piper / MiniMax)+ VAD,Discord 語音頻道與 LiveKit 會議 | [docs/features](docs/features/README.md) |
| 自動更新 | Dashboard 一鍵更新或背景自動更新(`auto_update = true`),SHA-256 + Ed25519 雙重驗證後原地重啟,前台分頁自動重載 | [deployment-guide.md](docs/guides/deployment-guide.md) |
| Web Dashboard | React 19 SPA 24 頁,嵌入 binary 零額外部署;zh-TW / en / ja 三語 | [docs/features](docs/features/README.md) |
| ERP 整合 | Odoo 中間層 15 個 MCP 工具(CRM / 銷售 / 庫存 / 會計),per-agent 認證隔離 | [docs/rfc](docs/rfc/RFC-21-operator-guide.md) |

完整功能清單見 [docs/features/feature-inventory.md](docs/features/feature-inventory.md),版本演進見 [CHANGELOG.md](CHANGELOG.md)。

<a id="cli"></a>

## CLI 指令

```
duduclaw run                 # 一鍵啟動(gateway + channels + heartbeat + cron + dispatcher)
duduclaw agent               # CLI 互動式對話
duduclaw agent create        # 建立 agent(可指定產業模板)
duduclaw agent list          # 列出所有 agent
duduclaw status              # 系統健康快照
duduclaw doctor              # 健康診斷
duduclaw test <agent>        # 紅隊安全測試(9 項內建場景)
duduclaw update              # 檢查並安裝更新
duduclaw service install     # 安裝為系統服務(launchd / systemd)
duduclaw mcp-server          # 啟動 MCP Server(stdio JSON-RPC 2.0)
duduclaw acp-server          # 啟動 ACP/A2A Server(Zed / JetBrains / Neovim 整合)
```

完整指令用 `duduclaw --help` 查看,開發者相關見 [development-guide.md](docs/guides/development-guide.md)。

<a id="trust"></a>

## 信任與安全

你安裝的東西完全透明:

- **npm 套件內容**:一個小型 JS wrapper 加上平台 binary(`@duduclaw/<platform>` optionalDependencies)。`postinstall` 只檢查平台套件是否就位([`install.js`](npm/duduclaw/scripts/install.js)),沒有任何「從任意 URL 下載並執行」的行為
- **無遙測**:零 phone-home 連線;所有密鑰以 AES-256-GCM 留在你的機器
- **不需特權**:完全在 user space 執行
- **維護者**:嘟嘟數位科技有限公司(台灣登記公司,統編 94139082)

每個 Release 資產都附三種驗證:SHA-256 checksum、[cosign](https://github.com/sigstore/cosign) keyless 簽章、minisign Ed25519 簽章(內建自動更新器會強制驗證,拒絕未簽章或被竄改的版本):

```bash
# SHA-256
shasum -a 256 -c duduclaw-darwin-arm64.tar.gz.sha256

# minisign(公鑰同時內建於 binary)
minisign -Vm duduclaw-darwin-arm64.tar.gz \
  -P RWTh5pOpk0YmdBgm3VyB2bzxFtajNLXr7zFDhbcc75TgM8YfeV+NSzXh
```

不信任預編譯 binary?[從原始碼建構](#install)三行指令就好。漏洞回報見 [SECURITY.md](SECURITY.md)。

> 為什麼「新」套件版本號已經 1.3x?DuDuClaw 公開前在私有 repo 開發了數月(400+ commits),完整歷史都在 [git log](https://github.com/zhixuli0406/DuDuClaw/commits/main)。

<a id="comparison"></a>

## 競品對比

| | DuDuClaw | OpenClaw | IronClaw | Dify |
|---|---|---|---|---|
| 語言 | Rust | TypeScript | Rust | Python |
| 通道 | 9 | 25+ | 8 | 0(API)|
| Multi-Runtime | 5 後端 | 單一 | 單一 | 多 LLM |
| MCP Server | 80+ 工具 | 無 | 無 | 無 |
| 自我進化引擎 | GVU² 雙迴圈 | 無 | 無 | 無 |
| 本地推論 | 6 後端 + 信心路由 | 無 | 無 | 無 |
| 行為契約 | CONTRACT.toml + 紅隊 | 無 | WASM 沙箱 | 無 |
| 授權 | Apache 2.0(Open Core)| MIT | 開源 | $59+/月 |

<a id="docs"></a>

## 文件

- [ARCHITECTURE.md](ARCHITECTURE.md):完整系統架構
- [docs/README.md](docs/README.md):公開文件索引(架構 / RFC / ADR / 規格 / 指南)
- [docs/guides/deployment-guide.md](docs/guides/deployment-guide.md):生產部署(Tailscale / Docker / systemd / 自動更新 / 監控)
- [docs/guides/development-guide.md](docs/guides/development-guide.md):開發環境與 agent 開發
- [docs/guides/custom-mcp-tool.md](docs/guides/custom-mcp-tool.md):自訂 MCP 工具教學
- [docs/spec](docs/spec/soul-md-spec.md):SOUL.md 與 CONTRACT.toml 格式規範
- [CHANGELOG.md](CHANGELOG.md):版本變更紀錄

<a id="license"></a>

## 授權

Open Core 模式:核心程式碼採 [Apache License 2.0](LICENSE),自由使用、修改、分發。商業加值模組(`commercial/`)為閉源付費,含產業模板、企業儀表板與授權驗證,詳見 [LICENSING.md](LICENSING.md)。

<p align="center">
  🐾 Built with louis.li
</p>
