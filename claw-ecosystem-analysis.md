# Claw 生態系深度分析報告

> 研究日期：2026-03-15
> 研究範圍：OpenClaw、NanoClaw、PicoClaw、ZeroClaw

---

## 目錄

1. [專案總覽與定位比較](#一專案總覽與定位比較)
2. [功能面深度分析](#二功能面深度分析)
3. [安全性分析](#三安全性分析)
4. [設定與部署](#四設定與部署)
5. [API 介面與規格](#五api-介面與規格)
6. [綜合比較矩陣](#六綜合比較矩陣)
7. [結論與建議](#七結論與建議)

---

## 一、專案總覽與定位比較

| 項目 | OpenClaw | NanoClaw | PicoClaw | ZeroClaw |
|------|----------|----------|----------|----------|
| **GitHub** | openclaw/openclaw | qwibitai/nanoclaw | sipeed/picoclaw | openagen/zeroclaw ⚠️ |
| **語言** | TypeScript (Node.js) | TypeScript (Node.js) | Go | Rust |
| **Stars** | ~313k | ~22.9k | ~24.7k | N/A (fork) |
| **授權** | MIT | MIT | MIT | MIT |
| **定位** | 全功能自託管 AI 助理 | OpenClaw 輕量替代方案 | 超低資源邊緣裝置 AI | Trait 驅動極簡 Agent 基礎設施 |
| **成熟度** | 最成熟，社群最大 | 早期但設計精良 | 早期開發（未達 v1.0） | 需使用官方 repo |
| **口號** | 在你自己的設備上運行 | 最少程式碼、最大功能 | $10 Hardware, 10MB RAM, 1s Boot | Deploy anywhere, swap anything |

> ⚠️ **重要警告**：`openagen/zeroclaw` 已被官方 ZeroClaw 團隊標記為**未經授權的 fork / 冒充專案**。官方儲存庫為 [zeroclaw-labs/zeroclaw](https://github.com/zeroclaw-labs/zeroclaw)。

---

## 二、功能面深度分析

### 2.1 OpenClaw — 功能最全面的旗艦專案

**核心架構**：以 WebSocket RPC Gateway 為中樞的分散式架構

- **25+ 通訊平台**：WhatsApp、Telegram、Slack、Discord、Google Chat、Signal、iMessage、IRC、Teams、Matrix、Feishu、LINE 等
- **多 Agent 路由**：各 Agent 擁有隔離工作區、獨立 Session 與認證設定
- **Canvas 系統**：Agent 驅動的視覺工作區，A2UI 即時協作
- **語音功能**：macOS/iOS 喚醒詞、Android 持續語音、ElevenLabs TTS
- **瀏覽器控制**：專屬 Chromium 實例，支援快照與自動化
- **設備整合**：配對 macOS/iOS/Android 節點，暴露相機、螢幕錄製、通知等本地能力
- **記憶體系統**：SQLite + sqlite-vec 向量嵌入，混合搜尋
- **自動化**：Cron 排程、Webhook、Gmail Pub/Sub、Skills 擴充平台

**技術棧**：Node.js >= 22、TypeScript (tsx)、React (Control UI)、Swift (macOS/iOS)、Kotlin (Android)

```
外部客戶端 → WebSocket RPC (ws://localhost:18789)
    → Gateway Server
        ├→ Session Manager
        ├→ Agent Router
        ├→ Config Manager (Zod + JSON5)
        └→ Tool Registry
    → Agent Runtime
        ├→ LLM Provider
        ├→ Tool Executor
        ├→ Skills Loader
        └→ Memory Index
    → 持久化 (JSONL + SQLite + JSON)
```

---

### 2.2 NanoClaw — 極簡但安全的替代方案

**核心架構**：單一 Node.js 主程序 + 容器化 Agent 執行

- **通道即插件**：不內建任何通道，透過 Claude Code Skill 安裝（`/add-whatsapp`、`/add-telegram`）
- **容器化隔離**：每次 Agent 呼叫在獨立 Docker / Apple Container 中執行
- **分群組記憶系統**：
  - 全域記憶（`groups/CLAUDE.md`）— 所有群組可讀
  - 群組記憶（`groups/{name}/CLAUDE.md`）— 群組專屬
  - 檔案記憶（`groups/{name}/*.md`）— Agent 產生的筆記
- **排程系統**：支援 cron / interval / once 三種排程
- **Session 持續性**：每群組獨立 Session（SQLite）
- **Agent Swarm**：支援平行 Agent 執行（`/add-parallel`）
- **遠端控制**：`claude remote-control` 產生遠端存取 URL

**技術棧**：Node.js 20+、TypeScript、@anthropic-ai/claude-agent-sdk、better-sqlite3、Docker

```
主程序
    ├→ Message Loop (每 2 秒輪詢 SQLite)
    ├→ Scheduler Loop (每 60 秒)
    ├→ IPC Watcher (檔案系統 JSON)
    ├→ Container Runner (最多 5 並行)
    └→ Channel Registry (動態)
```

---

### 2.3 PicoClaw — 邊緣裝置的極致輕量化

**核心架構**：AgentLoop 協調四大元件

- **10+ 聊天平台**：Telegram、Discord、Slack、WhatsApp、Matrix、QQ、DingTalk、LINE 等
- **10+ LLM 供應商**：OpenAI、Anthropic、Gemini、Groq、Deepseek、智譜、Ollama 等
- **工具系統**：檔案操作、Shell 執行、網頁搜尋、排程、子代理人
- **MCP 整合**：透過 Model Context Protocol 擴充工具
- **語音轉錄**：Groq Whisper 整合
- **負載平衡**：同名模型自動 round-robin
- **心跳服務**：定期讀取 HEARTBEAT.md 執行週期性任務
- **超低資源**：記憶體 < 10MB，單一靜態二進位檔

**技術棧**：Go 1.21+、CGO_ENABLED=0 靜態編譯

**支援架構**：x86_64、ARM64、ARMv7、MIPS LE、RISC-V、LoongArch

```
外部平台 → Channel → MessageBus → AgentLoop
    → ContextBuilder → LLM Provider
    → Tool Loop (最多 20 次迭代)
    → SessionManager → 回傳結果
```

---

### 2.4 ZeroClaw — Rust 驅動的 Trait 架構

**核心架構**：Trait 驅動的模組化設計

- **70+ LLM 整合**：OpenAI、Anthropic、OpenRouter 等
- **多通訊頻道**：CLI、Telegram、Discord、Slack、Mattermost、iMessage、Matrix、Signal、WhatsApp 等
- **極低資源消耗**：執行時記憶體 < 5MB，冷啟動 < 10ms
- **可替換架構**：所有核心系統皆以 Rust trait 實作
- **SQLite 混合搜尋**：向量嵌入 + FTS5 關鍵字索引
- **二進位大小**：約 8.8MB
- **研究優先**：先驗證事實再生成回應

**技術棧**：Rust、Axum (HTTP)、SQLite

```
核心 Trait 層
    ├→ Provider trait (AI 供應商)
    ├→ Channel trait (通訊頻道)
    ├→ Tool trait (工具呼叫)
    ├→ Memory trait (記憶體後端)
    └→ Tunnel trait (連線通道)
```

---

## 三、安全性分析

### 3.1 安全機制比較

| 安全特性 | OpenClaw | NanoClaw | PicoClaw | ZeroClaw |
|----------|----------|----------|----------|----------|
| **容器隔離** | 選用 Docker 沙箱 | ✅ 預設啟用 | ❌ 無 | ❌ 無 |
| **憑證隔離** | 設定檔管理 | ✅ 憑證代理（金鑰不進容器）| ❌ 明文 config.json | ✅ 加密金鑰 |
| **RBAC 角色控制** | ✅ Operator Scope | ✅ 主群組/非主群組權限分離 | ❌ 基本白名單 | ✅ 自主等級 |
| **路徑遍歷防護** | ⚠️ 曾有 CVE | ✅ Symlink 解析 + 阻擋清單 | ✅ 工作區限制 | ✅ 內建防護 |
| **指令注入防護** | ⚠️ 曾有 CVE | ✅ 容器隔離 | ✅ 危險指令封鎖清單 | ✅ 內建防護 |
| **速率限制** | ✅ | ✅ 發送者白名單 | ✅ 頻道級別 | ✅ 可配置 |
| **設備配對** | ✅ Ed25519 | ❌ | ❌ | ✅ 配對碼機制 |
| **安全測試覆蓋** | 未公開 | 未公開 | 未公開 | 129 個安全測試 |
| **漏洞回報政策** | ✅ | 未公開 | 未公開 | ✅ 48 小時回應 |

### 3.2 已知漏洞

#### OpenClaw — ⚠️ 高風險歷史

| CVE | 嚴重性 | 描述 |
|-----|--------|------|
| CVE-2026-25253 | CVSS 8.8 (高) | 一鍵 RCE，惡意網站可竊取 Token 並控制 Gateway |
| CVE-2026-25593 | 高 | 認證繞過 |
| CVE-2026-24763 | 高 | 命令注入 |
| CVE-2026-25157 | 高 | 命令注入 |
| CVE-2026-25475 | 中-高 | SSRF |
| CVE-2026-26319 | 中-高 | 路徑遍歷 |
| CVE-2026-26322 | 中 | 缺少認證 |
| CVE-2026-26329 | 中 | 路徑遍歷 |

此外，ClawHub（技能市集）發現 **341 個惡意技能**，其中 335 個追溯到「ClawHavoc」協調攻擊行動。

#### NanoClaw、PicoClaw、ZeroClaw

目前無已知 CVE。但 PicoClaw 官方明確警告**不建議在 v1.0 前用於生產環境**。

### 3.3 NanoClaw 安全亮點 — 憑證代理設計

NanoClaw 的憑證代理（Credential Proxy）是四個專案中最精巧的安全設計：

```
容器內 Agent
  → ANTHROPIC_BASE_URL=http://host.docker.internal:3001
  → ANTHROPIC_API_KEY=placeholder（假金鑰）
    ↓
主機憑證代理 (port 3001)
  → 移除假認證標頭
  → 注入真實 API 金鑰
  → 轉發至 api.anthropic.com
```

真正的 API 金鑰**永遠不會進入容器**，即使 Agent 被注入攻擊也無法洩漏憑證。

### 3.4 ZeroClaw 安全亮點 — 縱深防禦

三級自主控制：`ReadOnly` → `Supervised`（預設）→ `Full`（沙盒限制）

- 129 個自動化安全測試
- CIS Docker Benchmark 合規
- 非 root 運行（UID 65534）
- Distroless 最小基底映像

---

## 四、設定與部署

### 4.1 設定檔格式比較

| 項目 | 格式 | 主設定路徑 | 驗證機制 |
|------|------|-----------|----------|
| OpenClaw | JSON5 | `~/.openclaw/openclaw.json` | Zod Schema |
| NanoClaw | .env + JSON | `.env` + `~/.config/nanoclaw/` | TypeScript 型別 |
| PicoClaw | JSON | `~/.picoclaw/config.json` | Go struct |
| ZeroClaw | TOML | `~/.zeroclaw/config.toml` | Rust 型別 |

### 4.2 設定優先級

所有專案都支援三層優先級：**環境變數 > 設定檔 > 內建預設值**

### 4.3 環境變數比較

| 用途 | OpenClaw | NanoClaw | PicoClaw | ZeroClaw |
|------|----------|----------|----------|----------|
| API 金鑰 | `OPENCLAW_GATEWAY_TOKEN` | `ANTHROPIC_API_KEY` | 各 provider 獨立 | `ZEROCLAW_API_KEY` |
| 設定覆蓋 | 部分支援 | `CONTAINER_*` 系列 | `PICOCLAW_*` 前綴 | `ZEROCLAW_*` 前綴 |
| OAuth | 支援 | `CLAUDE_CODE_OAUTH_TOKEN` | `picoclaw auth login` | 不適用 |

### 4.4 工作區結構比較

**OpenClaw**：
```
~/.openclaw/
├── openclaw.json          # 主設定
├── agents/<id>/sessions/  # Session 逐字稿 (JSONL)
└── memory/<id>.sqlite     # 記憶體資料庫
```

**NanoClaw**：
```
groups/
├── CLAUDE.md              # 全域記憶
├── {group}/
│   ├── CLAUDE.md          # 群組記憶
│   └── *.md               # Agent 筆記
data/
├── ipc/{group}/           # IPC JSON 檔案
└── sessions/{group}/      # Session 資料
store/messages.db           # SQLite 資料庫
```

**PicoClaw**：
```
~/.picoclaw/
├── config.json            # 主設定
└── workspace/
    ├── sessions/          # 對話歷史
    ├── memory/MEMORY.md   # 長期記憶
    ├── cron/cron.json     # 排程任務
    ├── skills/            # 技能 markdown
    ├── AGENTS.md          # 行為指南
    ├── IDENTITY.md        # 身份定義
    └── SOUL.md            # 人格設定
```

**ZeroClaw**：
```
~/.zeroclaw/
├── config.toml            # 主設定
├── secret.key             # 加密金鑰
└── workspace/
    ├── AGENTS.md          # Agent 身份
    ├── SOUL.md            # 個性定義
    ├── MEMORY.md          # 長期記憶
    └── state/             # 執行期資料庫
        ├── memory.db
        └── models_cache.json
```

### 4.5 安裝方式

| 方式 | OpenClaw | NanoClaw | PicoClaw | ZeroClaw |
|------|----------|----------|----------|----------|
| npm/npx | ✅ `npm install -g openclaw` | ✅ npm | ❌ | ❌ |
| Homebrew | ❌ | ❌ | ❌ | ✅ |
| Shell 一鍵安裝 | ❌ | ❌ | ✅ | ✅ |
| Cargo | ❌ | ❌ | ❌ | ✅ |
| Docker | ✅ | ✅ | ✅ | ✅ |
| Git Clone | ✅ | ✅ | ✅ | ✅ |
| 二進位下載 | ❌ | ❌ | ✅ 靜態編譯 | ✅ 靜態編譯 |

---

## 五、API 介面與規格

### 5.1 API 介面類型比較

| 介面類型 | OpenClaw | NanoClaw | PicoClaw | ZeroClaw |
|----------|----------|----------|----------|----------|
| **WebSocket RPC** | ✅ 主要介面 | ❌ | ❌ | ❌ |
| **REST HTTP** | ✅ OpenAI 相容 | ❌ | ✅ Webhook | ✅ Axum Gateway |
| **CLI** | ✅ 完整 | ✅ 基本 | ✅ 完整 | ✅ 完整 |
| **IPC (檔案)** | ❌ | ✅ JSON 檔案 | ❌ | ❌ |
| **MCP** | ❌ | ✅ 動態 MCP Server | ✅ 客戶端 | ❌ |

### 5.2 OpenClaw — WebSocket RPC 協定

**訊息框架**：

```typescript
// 請求
{ type: "req", id: string, method: string, params: object }

// 回應
{ type: "res", id: string, ok: boolean, payload?: object, error?: object }

// 事件
{ type: "event", event: string, payload: object, seq?: number }
```

**認證握手**：
1. Gateway 發送 `connect.challenge`（nonce + timestamp）
2. 客戶端回應 `connect`（Ed25519 簽名 nonce）
3. Gateway 回傳 `hello-ok`（協議版本 + 設備 Token）

**關鍵 Method**：
- `tools.catalog` — 取得工具目錄
- `skills.bins` — 取得可執行允許清單
- `exec.approval.resolve` — 解決執行核准
- `device.token.rotate` / `revoke` — Token 管理

### 5.3 NanoClaw — IPC + MCP 介面

**IPC 訊息格式**（容器 → 主機，透過 JSON 檔案）：

```json
// 發送訊息
{ "type": "message", "chatJid": "...", "text": "..." }

// 排程任務
{ "type": "schedule_task", "prompt": "...", "schedule_type": "cron", "schedule_value": "0 9 * * *" }
```

**MCP 工具規格**：

| 工具 | 參數 | 說明 |
|------|------|------|
| `schedule_task` | prompt, schedule_type, schedule_value | 建立排程 |
| `send_message` | chatJid, text | 發送訊息 |
| `list_tasks` | — | 列出任務 |
| `pause_task` / `resume_task` / `cancel_task` | taskId | 任務控制 |

**通道介面**：
```typescript
interface Channel {
  name: string;
  connect(): Promise<void>;
  sendMessage(jid: string, text: string): Promise<void>;
  isConnected(): boolean;
  ownsJid(jid: string): boolean;
  disconnect(): Promise<void>;
}
```

### 5.4 PicoClaw — HTTP Gateway + 工具介面

**Gateway**：`127.0.0.1:18790`（預設）

**CLI 指令**：

| 指令 | 用途 |
|------|------|
| `picoclaw agent -m "msg"` | 單次查詢 |
| `picoclaw agent` | 互動式 REPL |
| `picoclaw gateway` | 啟動 HTTP 服務 |
| `picoclaw status` | 連線狀態 |
| `picoclaw cron list/add` | 排程管理 |

**LLM Provider 路由**：協定前綴格式 `[protocol/]model-identifier`

| 前綴 | 範例 |
|------|------|
| `openai/` | `openai/gpt-5.2` |
| `anthropic/` | `anthropic/claude-sonnet-4.6` |
| `gemini/` | `gemini/gemini-2.5-pro` |
| `ollama/` | `ollama/llama3` |
| `claude-cli/` | 本地 Claude 執行檔 |

**工具規格**（轉換為 OpenAI function_calling 格式）：

| 工具 | 功能 |
|------|------|
| `read_file` / `write_file` / `edit_file` | 檔案操作 |
| `exec` / `sh` | Shell 執行 |
| `web_search` | 網頁搜尋 |
| `cron` | 排程（at/every/cron） |
| `subagent` | 子代理人 |

### 5.5 ZeroClaw — RESTful Gateway

**Base URL**：`http://127.0.0.1:3000`（預設）

| 端點 | Method | 認證 | 用途 |
|------|--------|------|------|
| `/health` | GET | 無 | 健康檢查 |
| `/metrics` | GET | 無 | Prometheus 指標 |
| `/pair` | POST | `X-Pairing-Code` | 取得 Bearer Token |
| `/webhook` | POST | Bearer Token | LLM 處理訊息（核心 API）|
| `/whatsapp` | GET | Query 參數 | WhatsApp Webhook 驗證 |
| `/whatsapp` | POST | HMAC-SHA256 | WhatsApp 來訊處理 |

**Webhook 請求/回應**：

```json
// Request
POST /webhook
Authorization: Bearer <token>
{ "message": "string" }

// Response 200
{ "response": "...", "model": "..." }
```

**傳輸限制**：

| 項目 | 值 |
|------|-----|
| 最大請求體 | 64KB |
| 請求超時 | 30 秒 |
| 速率限制視窗 | 60 秒滑動視窗 |

---

## 六、綜合比較矩陣

### 6.1 功能完整度（5 分制）

| 維度 | OpenClaw | NanoClaw | PicoClaw | ZeroClaw |
|------|:--------:|:--------:|:--------:|:--------:|
| 通訊平台數量 | ★★★★★ (25+) | ★★★☆☆ (插件式) | ★★★★☆ (10+) | ★★★★☆ (10+) |
| LLM 供應商 | ★★★★☆ | ★★☆☆☆ (Claude 專屬) | ★★★★★ (10+) | ★★★★★ (70+) |
| 工具生態 | ★★★★★ | ★★★☆☆ | ★★★★☆ | ★★★☆☆ |
| 記憶系統 | ★★★★★ | ★★★★☆ | ★★★☆☆ | ★★★★☆ |
| 語音支援 | ★★★★★ | ★☆☆☆☆ | ★★★☆☆ | ★★☆☆☆ |
| 行動裝置 | ★★★★★ | ★☆☆☆☆ | ★☆☆☆☆ | ★☆☆☆☆ |
| 文件品質 | ★★★★★ | ★★★★☆ | ★★★★☆ | ★★★★☆ |

### 6.2 非功能性特質

| 維度 | OpenClaw | NanoClaw | PicoClaw | ZeroClaw |
|------|:--------:|:--------:|:--------:|:--------:|
| **資源佔用** | 高 (Node.js) | 中 (Node.js + Docker) | ★★★★★ 極低 (<10MB) | ★★★★★ 極低 (<5MB) |
| **啟動速度** | 慢 | 中 | ★★★★★ 快 (<1s) | ★★★★★ 極快 (<10ms) |
| **二進位大小** | 大 (npm 生態) | 大 (npm 生態) | 小 (靜態編譯) | ★★★★★ ~8.8MB |
| **安全性** | ⚠️ 多個 CVE | ★★★★★ 最佳設計 | ★★★☆☆ 基本防護 | ★★★★☆ 縱深防禦 |
| **社群規模** | ★★★★★ 最大 | ★★★☆☆ | ★★★☆☆ | ★★☆☆☆ |
| **生產就緒** | ⚠️ 需注意安全更新 | ★★★★☆ | ❌ 官方不建議 | ★★★☆☆ |
| **邊緣裝置** | ❌ | ❌ | ★★★★★ | ★★★★★ |

---

## 七、結論與建議

### 7.1 各專案適用場景

| 場景 | 推薦 | 原因 |
|------|------|------|
| **全功能個人助理** | OpenClaw | 功能最完整，語音、行動裝置、Canvas 等獨有特性 |
| **安全優先的 AI Agent** | NanoClaw | 憑證代理、容器隔離、掛載安全設計最佳 |
| **邊緣/IoT 裝置** | PicoClaw 或 ZeroClaw | 極低資源消耗，靜態編譯跨平台 |
| **Rust 生態整合** | ZeroClaw | Trait 驅動設計，Rust 原生 |
| **快速原型開發** | NanoClaw | 程式碼量少，Fork-and-customize 哲學 |
| **企業部署** | OpenClaw（謹慎）| 功能完整但需嚴格安全審查 |

### 7.2 風險提示

1. **OpenClaw**：曾有多個嚴重 CVE，ClawHub 惡意技能事件（341 個），務必保持最新版本、限制 Gateway 暴露、啟用 Docker 沙箱
2. **NanoClaw**：網路隔離預設「不限制」，容器可自由存取網路；依賴 Claude 內建安全訓練防範 prompt injection
3. **PicoClaw**：官方明確**不建議在 v1.0 前用於生產環境**；API 金鑰明文儲存；Shell 執行工具風險
4. **ZeroClaw**：`openagen/zeroclaw` 為冒充 fork，務必使用官方 `zeroclaw-labs/zeroclaw`

### 7.3 技術趨勢觀察

- **語言選擇分化**：TypeScript（OpenClaw/NanoClaw）重視開發速度與生態；Go/Rust（PicoClaw/ZeroClaw）重視效能與資源效率
- **安全模型演進**：從 OpenClaw 的事後修補，到 NanoClaw 的 security-by-design（憑證代理），反映社群對 AI Agent 安全性的認識在快速成長
- **邊緣計算趨勢**：PicoClaw 和 ZeroClaw 證明 AI Agent 可以在極低資源環境中運行，這對 IoT 和離線場景意義重大
- **模組化 vs 單體**：四個專案都朝向模組化發展，但策略不同 — OpenClaw 是大型模組化、NanoClaw 是插件式、PicoClaw 是接口抽象、ZeroClaw 是 Trait 驅動

---

> 本報告由四個平行 Agent 同時研究，資料來源包括 GitHub 儲存庫、官方文件、DeepWiki、安全公告、第三方安全研究報告等。
