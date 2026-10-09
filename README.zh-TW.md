# DuDuClaw 🐾

<div align="center">

[English](README.md) · **繁體中文** · [日本語](README.ja.md)

</div>

DuDuClaw 把 Claude Code、Codex、Antigravity 這類 AI 指令列工具，變成公司裡交得出東西的 AI 員工：常駐 Telegram、LINE、Discord 等 11 個通訊軟體，交件前有獨立判官驗收，花掉的每一塊錢都記在帳上。

你只需要一個 Rust binary。通道路由、對話記憶、多帳號輪替、行為安全邊界、本地推論、Web 管理後台全部內建;AI 大腦可以在十二種 CLI 後端(Claude Code、Codex、Antigravity、Grok 等;Gemini CLI 已棄用)與任何 OpenAI 相容 API 之間隨你換,設定和記憶都留在你自己的機器上。核心採 Apache 2.0 授權。

[![CI](https://github.com/zhixuli0406/DuDuClaw/actions/workflows/ci.yml/badge.svg)](https://github.com/zhixuli0406/DuDuClaw/actions/workflows/ci.yml)
[![Version](https://img.shields.io/badge/version-1.71.0-blue)](https://github.com/zhixuli0406/DuDuClaw/releases)
[![npm](https://img.shields.io/npm/v/duduclaw?logo=npm)](https://www.npmjs.com/package/duduclaw)
[![PyPI](https://img.shields.io/pypi/v/duduclaw?logo=pypi)](https://pypi.org/project/duduclaw/)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache--2.0-blue.svg)](LICENSE)

**快速開始**(需要 [Node.js](https://nodejs.org/) 20+;想用桌面版請看[安裝](#install)):

```bash
npm install -g duduclaw
duduclaw run                  # 接著打開 http://localhost:18789
```


## 目錄

- [為什麼需要 DuDuClaw?](#why)
- [架構一覽](#architecture)
- [環境準備](#prerequisites)
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
| 接上 Telegram / LINE / Discord | 只能在終端機用 | 11 個通道,per-agent bot token |
| 多 LLM 容錯切換 | 手動重啟 | 4 種輪替策略 + 跨供應商 failover |
| 換 LLM 時保留上下文 | 遺失 | 完整保留 |
| 對話記憶與知識庫 | 單次 session | SQLite 時態記憶 + 分層 wiki + 自動注入 |
| 工具跨 LLM 共用 | 每家重寫 | 247 個 MCP 工具寫一次,Claude、Codex、Gemini、Antigravity、Grok 與 OpenAI 相容 runtime 都能呼叫 |
| 安全邊界 / 稽核 / 密鑰管理 | 自己造 | 政策核心 + OS 沙箱 + AES-256-GCM 內建 |
| 交給客戶的整台值班機 | 自己裝 Linux,更新與防竄改自己管 | DuDuClaw OS 映像:A/B 更新回滾 + 唯讀 root,插電即用;人機共用桌面,不影響日常使用 |

<a id="architecture"></a>

## 架構一覽

AI 運行時是大腦,DuDuClaw 是水電管線,中間用 MCP(JSON-RPC 2.0)橋接。大腦可換,管線不動:

```
AI Runtime (brain) — Claude Code / Codex / Antigravity / Grok / … (12 CLIs) / OpenAI-compat
  ↕ MCP Protocol (JSON-RPC 2.0, stdin/stdout)
DuDuClaw (plumbing)
  ├─ Channel Router — Telegram / LINE / Discord / Slack / WhatsApp / Feishu
  │                    / Google Chat / Microsoft Teams / WeCom / DingTalk / WebChat
  ├─ Multi-Runtime — 13 個 runtime id(12 種 CLI + OpenAI-compat),自動偵測,per-agent 設定
  ├─ Session Memory — 原生 --resume + 時態記憶 + key-fact 累積 + 分層 wiki
  ├─ MCP Server — 247 個工具(通訊、記憶、Agent、Skill、任務、知識庫、ERP)
  ├─ Evolution Engine — 預測驅動 + AEE playbook 進化(v3 預設) + MistakeNotebook
  ├─ Security — PolicyKernel reference monitor + OS 沙箱 + redaction vault
  ├─ Inference Engine — OpenAI 相容本地伺服器(llama-server / Ollama / vLLM)/ llamafile
  ├─ Account Rotator — 多 OAuth + API Key 輪替、預算追蹤、健康檢查
  └─ Web Dashboard — React 19 SPA,rust-embed 嵌入 binary
```

Rust workspace 由 24 個 crate 組成:核心地基 `duduclaw-core`、服務層 `duduclaw-gateway`、統一 API 層 `duduclaw-llm`、本地推論 `duduclaw-inference`、認知記憶 `duduclaw-memory`、安全層 `duduclaw-security` 等。完整設計見 [ARCHITECTURE.md](ARCHITECTURE.md)。

本地模型的實驗性校準路由參考 Varun Kotte 的 [UCCI（arXiv:2605.18796）](https://arxiv.org/abs/2605.18796)；設定與資料準備見 [UCCI calibrated cascade](docs/features/57-ucci-calibrated-cascade.md)。

同一套 gateway + dashboard 還有一種出貨形態:整台機器。[DuDuClaw OS](https://github.com/zhixuli0406/DuDuClaw-OS) 是以 Yocto 建出的值班機映像,Yocto 層與映像產線放在獨立 repo,把本 repo 的 Rust workspace 以剪枝快照 vendor 進去;見下方安裝一節。

<a id="prerequisites"></a>

## 環境準備

DuDuClaw 本身不含 LLM,需要一個 AI 大腦。三選一(之後也能在瀏覽器引導中設定):

- 裝好一個支援的 AI CLI,例如 [Claude Code](https://docs.anthropic.com/en/docs/claude-code)、[Codex](https://github.com/openai/codex) 或 Antigravity(完整清單見 [multi-runtime](docs/features/zh-TW/13-multi-runtime.md);[Gemini CLI](https://github.com/google-gemini/gemini-cli) 已棄用,v1.73.0 移除),並給它一把 API key。Anthropic 與 Google 會封鎖第三方產品使用的消費者訂閱 token,也有帳號因此被停權,所以請用 API key
- 準備一把 API key,走任何 OpenAI 相容供應商
- 或把本地模型掛在 OpenAI 相容伺服器後面(llama-server、Ollama、vLLM、llamafile),不需要任何雲端帳號

<a id="install"></a>

## 安裝

### 桌面應用程式(個人使用推薦)

Tauri 原生桌面版,啟動應用程式時會自動拉起本機 Gateway,全程免碰終端機,與 CLI 共用 `~/.duduclaw`。到 [Releases](https://github.com/zhixuli0406/DuDuClaw/releases) 下載:

| 平台 | 檔案 | 備註 |
|------|------|------|
| macOS(Apple Silicon / Intel) | `DuDuClaw_*.dmg` | 已簽章 + Apple 公證,直接開 |
| Windows x64 | `DuDuClaw_*_x64_en-US.msi` | 未購買 Authenticode 憑證,SmartScreen 會示警;點「更多資訊」→「仍要執行」即可安裝,解鎖細節見 [docs/guides/desktop-unblock.md](docs/guides/desktop-unblock.md) |
| Linux | `*_amd64.AppImage` / `.deb` | 免簽 |

> macOS 桌面版自 v1.68.1 起恢復提供(已簽章並通過 Apple 公證)。v1.67.0 到 v1.68.0 沒有 macOS 版,這幾版的 Windows 與 Linux 版照常提供。

裝好打開就是完整體驗——啟動精靈會在應用程式內帶你設定 AI 後端與第一個 agent,不需要另外跑指令。

### npm(進階 / 伺服器用途,所有平台含 Windows)

想在伺服器上跑、寫腳本自動化,或單純習慣命令列,可以改用 npm。前置需求只有 [Node.js](https://nodejs.org/) 20+:

```bash
npm install -g duduclaw
```

會自動安裝對應平台的預編譯 binary(macOS ARM64/x64、Linux x64/ARM64、Windows x64),免編譯器、免 Rust。

> ⚠️ 如果安裝過程要求你裝 Rust / MSVC Build Tools 並編譯 1.5 小時,代表走錯路徑了。那是給貢獻者的「從原始碼建構」;一般使用請用上面的 npm 指令。

### DuDuClaw OS(值班機映像,pre-GA)

不想佔用一台電腦,想要一台插電就跑的 AI 員工值班機:[DuDuClaw OS](https://github.com/zhixuli0406/DuDuClaw-OS) 是以 Yocto 建出的 Linux 作業系統,AI agent 是原生住民:開機就是自家桌面(compositor / 殼、鎖定畫面、Cmd+K 交辦列),人和 AI 共用同一台 x86-64 主機,而且不影響日常使用:agent 的 GUI 工作預設在影子工作區跑,你一動鍵盤滑鼠,正在你桌面上操作的 agent 立刻讓位。桌面版內建 A/B 原子更新與回滾、唯讀 root,並預載 Chromium / LibreOffice / Steam 與注音輸入法;Secure Boot 簽章、dm-verity、TPM2 為建置期 overlay 選項,截至最新版 v0.2.0 仍未啟用,開機需先在 BIOS/UEFI 關閉 Secure Boot。

現行版本是 v0.2.0(內嵌平台 v1.63.0;上一版 v0.1.0 保留作為回滾目標)。到 [DuDuClaw-OS Releases](https://github.com/zhixuli0406/DuDuClaw-OS/releases) 下載整碟 `.wic.zst`(桌面版)、`installer-desktop` 安裝器 `.iso`(寫入桌面版)或 `installer` 安裝器 `.iso`(寫入基礎版,沒有應用層),每個檔案都附 `.sha256` 與 minisign 簽章,驗簽指令見[官方文件站的 OS README](https://os.duduclaw.dudustudio.monster/docs/os/readme/)。目前是 bring-up 版(0.x):QEMU 驗證通過,**尚未在真實硬體開機驗證**。硬體條件與相容機型見 [docs/guides/hardware-requirements.md](docs/guides/hardware-requirements.md),產品說明見 [docs/features/50-duduclaw-os-appliance.md](docs/features/50-duduclaw-os-appliance.md)。

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

- **桌面版**:直接開啟應用程式,Gateway 會自動啟動,應用程式內會直接顯示引導精靈。
- **npm / 原始碼安裝**:

  ```bash
  duduclaw run                  # 啟動(gateway + 通道 + 排程 + dispatcher 一次拉起)
  open http://localhost:18789   # 打開管理後台
  ```

無論走哪條路,第一次打開都會進入引導精靈,所有設定都在瀏覽器裡完成:選 AI 後端 → 建立第一個 agent → 直接在內建 WebChat 對話,不必先在終端機跑 `duduclaw onboard`。之後到 Channels 頁貼上 bot token,就能把同一個 agent 接上 Telegram、LINE、Discord 等平台,不用重啟。

常用的下一步:

```bash
duduclaw agent create      # 建立更多 agent
duduclaw wizard            # 產業模板互動式設定
duduclaw status            # 系統健康快照
duduclaw update            # 檢查並安裝新版本
duduclaw service install   # 開機自動啟動(launchd / systemd)
```

<a id="features"></a>

## 功能總覽

| 領域 | 內建能力 | 深入閱讀 |
|------|----------|----------|
| 通訊通道 | 11 通道(Telegram / LINE / Discord / Slack / WhatsApp / Feishu / Google Chat / Teams / WeCom / DingTalk / WebChat),per-agent bot、熱啟停、平台原生排版、輸入中指示、長任務進度看板;Telegram 語音訊息經 OpenAI Whisper API 轉文字。Discord 語音頻道是非預設的編譯選項,發行版 binary 不含 | [docs/features](docs/features/README.md) |
| Multi-Runtime | 13 個 runtime id:Claude Code / Codex / Antigravity / Grok / Qwen Code / Kimi Code / GitHub Copilot CLI / Kiro / Cursor / Mistral Vibe / OpenCode / Gemini CLI(已棄用,v1.73.0 移除)加上 OpenAI-compat;自動偵測、per-agent 設定、換後端保留上下文 | [docs/features/13](docs/features/zh-TW/13-multi-runtime.md) |
| 統一 LLM API 層 | `duduclaw-llm` 用一套正規化請求覆蓋 4 種原生協定(Anthropic Messages / OpenAI Responses / Gemini / OpenAI-compat),內建 8 個 OpenAI-compat preset(DeepSeek / MiniMax / Groq / Together / Mistral / OpenRouter / xAI / Qwen)+ 計價 registry + 跨供應商 fallback | [ARCHITECTURE.md](ARCHITECTURE.md) |
| MCP Server | 247 個工具:通訊、記憶、agent 編排、skill 市場、任務看板、共享 wiki、Odoo ERP、computer use、live forking;stdio 與 HTTP/SSE 雙 transport;外部客戶端的 key 預設只能用 7 個基本工具,操作者可另外授與記憶、wiki 或訊息類 scope,連接器、執行類與管理類工具一律不對外 | [docs/api](docs/api/README.md) |
| 記憶系統 | SQLite 時態記憶(事實取代鏈)、HippoRAG-lite 知識圖譜檢索(Personalized PageRank)、Ebbinghaus 遺忘曲線自動封存、跨 agent 共享 wiki | [docs/features](docs/features/README.md) |
| 自我進化 | 預測驅動(設計上多數對話不需呼叫 LLM)、AEE playbook 進化(SOUL.md 對 agent 唯讀;學到的是一條條連結 eval 案例的小規則,不輸目前的 playbook 才提交,24 小時後逐條結算,退步只撤那一條)、MistakeNotebook 跨回合記憶 | [evolution-engine.md](docs/architecture/evolution-engine.md) |
| 安全 | PolicyKernel reference monitor(零 LLM、fail-closed)、macOS Seatbelt / Linux Landlock 原生沙箱(逐 agent 開啟,預設關)、容器沙箱(任務沙箱只支援 Docker,預設關;腳本沙箱用 Docker,Windows 先試 WSL2)、secret redaction vault、CONTRACT.toml 行為契約 + 紅隊測試 | [SECURITY.md](SECURITY.md) |
| 帳號與成本 | 多 OAuth + API Key 輪替(4 策略)、rate-limit / 帳單冷卻、成本遙測與快取效率分析。每次呼叫都以新行程執行官方 CLI(PTY 連線池已於 2026-09 移除;需要終端機的 CLI,例如 Grok,改用一次性偽終端)。Anthropic 與 Google 會封鎖第三方產品使用的消費者訂閱 token,請用 API key([multi-runtime](docs/features/zh-TW/13-multi-runtime.md)) | [docs/features](docs/features/README.md) |
| 本地推論 | 指向任一 OpenAI 相容本地伺服器(llama-server / Ollama / vLLM / SGLang)或 llamafile,三層信心路由自動分流 | [docs/features](docs/features/README.md) |
| 微調與後訓練 | 從本機對話、任務結果與審批決定建構 SFT / DPO 資料集(ShareGPT / Alpaca),送到自有 GPU 主機(SSH + LLaMA-Factory)或 Together 雲端訓練,GGUF / LoRA 匯回本地模型目錄;本機不做訓練(內顯跑不動),資料離機需明確確認 | [docs/features/54](docs/features/54-finetune.md) |
| Live Forking | RFC-26:把進行中的任務分叉成 N 個競爭分支,各自 copy-on-write 隔離、AI judge 選勝者合併(預設關閉;v1.67.0 請勿在 Windows 開啟,見 CHANGELOG) | [docs/rfc](docs/rfc) |
| 自動更新 | Dashboard 一鍵更新或背景自動更新(`auto_update = true`),SHA-256 + Ed25519 雙重驗證後原地重啟,前台分頁自動重載 | [deployment-guide.md](docs/guides/deployment-guide.md) |
| Web Dashboard | React 19 + TypeScript SPA,嵌入 binary 零額外部署;zh-TW / en / ja 三語 | [docs/features](docs/features/README.md) |
| ERP 整合 | Odoo 中間層 17 個 MCP 工具(CRM / 銷售 / 庫存 / 會計),CE/EE 自動偵測、per-agent 認證隔離 | [docs/rfc](docs/rfc/RFC-21-operator-guide.md) |
| DuDuClaw OS | Yocto 值班機映像(現行 v0.2.0,內嵌平台 v1.63.0):自家 compositor / 殼與快捷鍵、人機共駕(agent 專屬 seat、影子工作區、人輸入即凍結、Super+Esc 急停,已編譯進映像但預設關閉)、A/B 原子更新與回滾、唯讀 root、首次開機自動 provision + 區網後台、app 相容層(Flatpak / Bottles / Waydroid);Secure Boot 簽章 / dm-verity / TPM2 為建置 overlay 選項,截至 v0.2.0 仍未啟用;獨立 repo 與版號,pre-GA | [docs/features/50](docs/features/50-duduclaw-os-appliance.md) · [52](docs/features/52-desktop-edition.md) |

完整功能清單見 [docs/features/feature-inventory.md](docs/features/feature-inventory.md),版本演進見 [CHANGELOG.md](CHANGELOG.md)。

<a id="cli"></a>

## CLI 指令

```
duduclaw onboard             # 首次設定;瀏覽器引導已涵蓋,此為無頭/腳本場景用(--yes 跳過互動)
duduclaw run                 # 一鍵啟動(gateway + channels + heartbeat + cron + dispatcher)
duduclaw agent               # CLI 互動式對話;子指令 create / list / inspect / pause / resume / run
duduclaw wizard              # 產業模板互動式設定
duduclaw status              # 系統健康快照
duduclaw doctor              # 健康診斷
duduclaw test <agent>        # 紅隊安全測試(9 項內建場景)
duduclaw eval                # 執行 agent 行為 eval 套件
duduclaw update              # 檢查並安裝更新
duduclaw service install     # 安裝為系統服務;另有 start / stop / status / logs / uninstall
duduclaw export / import     # 匯出 / 匯入 ~/.duduclaw(個人版資料可攜)
duduclaw migrate from openclaw   # 從 OpenClaw / Hermes / paperclip 無痛轉移(預設 dry-run,--apply 落地)
duduclaw mcp-server          # 啟動 MCP Server(stdio JSON-RPC 2.0)
duduclaw http-server         # 啟動 MCP HTTP/SSE Transport(Bearer 認證)
duduclaw acp                 # 啟動 Agent Client Protocol server(Zed / JetBrains / Neovim agent panel)
duduclaw acp server          # 啟動 A2A Server(agent 對 agent 互通)
duduclaw license             # 授權管理(activate / status / redeem / rebind / …)
```

完整指令與所有子指令用 `duduclaw --help` 查看,開發者相關見 [development-guide.md](docs/guides/development-guide.md)。

<a id="trust"></a>

## 信任與安全

你安裝的東西完全透明:

- **npm 套件內容**:一個小型 JS wrapper 加上平台 binary(`@duduclaw/<platform>` optionalDependencies)。`postinstall` 只檢查平台套件是否就位([`install.js`](npm/duduclaw/scripts/install.js)),沒有任何「從任意 URL 下載並執行」的行為
- **無遙測**:不會把使用資料或對話內容送給我們。Gateway 每 6 小時向 GitHub Releases 檢查更新;裝了付費授權時,另會向授權伺服器更新授權(依方案每 3 到 7 天一次)並每天抓取撤銷清單,沒有授權檔就沒有任何授權相關連線。所有密鑰以 AES-256-GCM 留在你的機器
- **不需特權**:完全在 user space 執行
- **維護者**:嘟嘟數位科技有限公司(台灣登記公司,統編 94139082)
- **頻道裡的高風險操作**:AI 員工在頻道裡請你核准高風險 Computer Use 步驟時,你要回覆該請求的完整 ID;執行結果不明的操作不會自動重送,由 Admin 在管理後台核對。見[操作指南](docs/guides/zh-TW/durable-channel-decisions.md)

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
| 通道 | 11 | 25+ | 8 | 0(API)|
| Multi-Runtime | 13 個 runtime id(12 種 CLI + OpenAI-compat) | 單一 | 單一 | 多 LLM |
| MCP Server | 247 個工具 | 無 | 無 | 無 |
| 自我進化引擎 | AEE playbook 規則(預測驅動) | 無 | 無 | 無 |
| 本地推論 | OpenAI 相容本地伺服器 / llamafile + 信心路由 | 無 | 無 | 無 |
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
- [docs/features/50-duduclaw-os-appliance.md](docs/features/50-duduclaw-os-appliance.md):DuDuClaw OS 值班機(產品說明);[52-desktop-edition.md](docs/features/52-desktop-edition.md):桌面版,人機共用一台機器;硬體需求見 [hardware-requirements.md](docs/guides/hardware-requirements.md);app 相容層、映像建置與發布都在 [DuDuClaw-OS](https://github.com/zhixuli0406/DuDuClaw-OS) repo(相容層說明見該 repo 的 `docs/guides/app-compat.md`)
- [CHANGELOG.md](CHANGELOG.md):版本變更紀錄

<a id="license"></a>

## 授權

Open Core 模式:核心程式碼採 [Apache License 2.0](LICENSE),自由使用、修改、分發。商業加值內容(`commercial/`,不在本 repo)為閉源付費,例如付費產業包,以授權金鑰解鎖;授權驗證用的用戶端(`crates/duduclaw-license`)屬於 Apache 2.0 核心。詳見 [LICENSING.md](LICENSING.md)。

商用授權、企業導入、培訓與支援由授權總經銷 [未來企業股份有限公司](https://www.futurecorp.tw/) 提供，原廠不直接販售。

<p align="center">
  🐾 Built with louis.li
</p>
