# DuDuClaw 系統架構設計

> 版本：0.6.2
> 日期：2026-03-23

---

## 目錄

1. [設計決策](#一設計決策)
2. [總覽：Extension Layer 模式](#二總覽extension-layer-模式)
3. [多 Agent 架構](#三多-agent-架構)
4. [Rust 核心層](#四rust-核心層)
5. [Python 擴充層](#五python-擴充層)
6. [安全系統](#六安全系統)
7. [Evolution 三層反思引擎](#七evolution-三層反思引擎)
8. [記憶系統](#八記憶系統)
9. [通訊通道](#九通訊通道)
10. [Web 管理介面](#十web-管理介面)
11. [專案結構](#十一專案結構)
12. [設定格式](#十二設定格式)

---

## 一、設計決策

| 決策項目 | 選擇 | 理由 |
|----------|------|------|
| AI 對話 | **Claude Code SDK (`claude` CLI)** | 內建工具鏈（bash、web search、file ops）、MCP 相容、官方維護 |
| 核心語言 | **Rust** | 記憶體安全、高效能、單 binary 部署 |
| 擴充語言 | **Python (PyO3)** | Claude Code SDK 整合、通道插件彈性 |
| Agent 隔離 | **資料夾 + SOUL.md** | 簡單直覺，無容器開銷；IPC 透過 `bus_queue.jsonl` |
| IPC 機制 | **File-based queue** (`bus_queue.jsonl`) | 零依賴，跨程序讀寫，天然持久化 |
| 首批通道 | LINE + Telegram + Discord | 台灣市場 + 國際社群 |
| 安全認證 | **Ed25519 challenge-response** | 非對稱簽章，無密碼傳輸 |
| API key 儲存 | **AES-256-GCM** | 對稱加密，base64 存於 config.toml |
| 日誌推送 | **BroadcastLayer** tracing | 即時推播 log 到 WebSocket，零侵入 |
| Evolution | **Claude subprocess 呼叫** | 三層反思均使用真實 LLM 推理，非模板產出 |
| Token 計算 | **CJK-aware heuristic** | CJK 字元 ~1.5 chars/token，ASCII ~4 chars/token |

---

## 二、總覽：Extension Layer 模式

DuDuClaw **不是**一個獨立的 AI 平台。它是 **Claude Code 的擴充層 (extension layer)**。

```
┌─────────────────────────────────────────┐
│           Claude Code SDK               │
│    (brain: bash / web / file / MCP)     │
└────────────────┬────────────────────────┘
                 │ MCP Protocol
                 │ JSON-RPC 2.0 over stdin/stdout
┌────────────────▼────────────────────────┐
│              DuDuClaw                   │
│  ┌──────────┐  ┌──────────┐            │
│  │ MCP      │  │ Session  │            │
│  │ Server   │  │ Manager  │            │
│  └──────────┘  └──────────┘            │
│  ┌──────────┐  ┌──────────┐            │
│  │ Channel  │  │ Memory   │            │
│  │ Router   │  │ Engine   │            │
│  └──────────┘  └──────────┘            │
│  ┌──────────┐  ┌──────────┐            │
│  │Evolution │  │ Account  │            │
│  │ Engine   │  │ Rotator  │            │
│  └──────────┘  └──────────┘            │
│  ┌──────────────────────────────┐      │
│  │    Web Dashboard (embedded)  │      │
│  └──────────────────────────────┘      │
└─────────────────────────────────────────┘
```

核心原則：
- **AI = Claude Code SDK** — 對話邏輯、工具使用、上下文管理全由 `claude` CLI 負責
- **DuDuClaw = Plumbing** — 通道路由、session 持久化、記憶搜尋、帳號輪替
- **橋接 = MCP Protocol** — `duduclaw mcp-server` 作為 MCP Server，工具以 JSON-RPC 2.0 暴露

---

## 三、多 Agent 架構

### 3.1 Agent 目錄結構

每個 Agent 是一個資料夾，與 Claude Code 完全相容：

```
~/.duduclaw/
├── config.toml                     # 全域設定（含加密 API key）
├── secret.key                      # AES-256-GCM 主加密金鑰
├── bus_queue.jsonl                 # 跨 Agent 訊息隊列（file-based IPC）
├── cron_tasks.jsonl                # Cron 任務持久化（JSONL 格式）
│
└── agents/
    ├── dudu/                       # 主 Agent
    │   ├── agent.toml              # Agent 設定（見第十二節）
    │   ├── SOUL.md                 # 人格定義
    │   ├── CLAUDE.md               # Claude Code 指引
    │   ├── .mcp.json               # MCP Server 設定（指向 duduclaw mcp-server）
    │   ├── .claude/
    │   │   └── settings.local.json
    │   ├── SKILLS/                 # 技能集（可由 Macro 反思自動產出）
    │   ├── memory/                 # 每日筆記
    │   │   └── 202603/
    │   │       └── 20260319.md
    │   └── state/
    │       └── state.db            # SQLite（記憶 + session 歷史）
    │
    └── coder/                      # 另一個 Agent
        └── ...
```

### 3.2 跨 Agent 委派（File-based IPC）

Agent 之間透過 `bus_queue.jsonl` 傳遞訊息，無需容器或網路：

```
Agent A (Claude Code) → MCP tool: send_to_agent
         │
         ▼
bus_queue.jsonl  ← Rust bridge 寫入 JSON 行
         │
         ▼
Agent B runner (subprocess) 讀取並執行
```

格式：
```json
{"to": "coder", "from": "dudu", "task": "幫我 code review", "ts": "2026-03-19T10:00:00Z"}
```

---

## 四、Rust 核心層

### 4.1 Crate 架構

```
crates/
├── duduclaw-core/      # 共用型別、traits、錯誤定義
├── duduclaw-agent/     # Agent 掃描、agent.toml 解析、心跳、預算
├── duduclaw-security/  # AES-256-GCM 加密、Ed25519 驗證
├── duduclaw-memory/    # SQLite + FTS5 全文搜尋記憶引擎
├── duduclaw-gateway/   # Axum 伺服器、通道整合、WebSocket、MCP tools
├── duduclaw-bus/       # tokio broadcast + mpsc 訊息路由
├── duduclaw-bridge/    # PyO3 Rust↔Python 橋接（bus_queue 寫入）
├── duduclaw-cli/       # clap CLI 入口、mcp-server、migrate 指令
└── duduclaw-dashboard/ # rust-embed 嵌入 React SPA
```

### 4.2 Gateway（`duduclaw-gateway`）

核心服務，負責所有 WebSocket 請求分發：

```
axum WebSocket
    │
    ├── auth.rs       — Ed25519 challenge-response 認證
    ├── handlers.rs   — JSON-RPC dispatch（agents.*, memory.*, system.*, …）
    ├── channel_reply.rs — 通道回覆、session 管理、token 壓縮
    ├── server.rs     — socket.split() + tokio::select! 並行驅動
    ├── log.rs        — BroadcastLayer：tracing → WebSocket push
    ├── discord.rs    — Discord Gateway WebSocket bot
    └── telegram.rs   — Telegram long polling bot
```

### 4.3 WebSocket 認證流程（Ed25519）

```
Client                          Server
  │                               │
  │── { "method": "connect" } ──► │
  │                               │  issue_challenge(): 32-byte random
  │◄── { "challenge": "<b64>" } ──│
  │                               │
  │  sign(challenge, privkey)     │
  │── { "method": "authenticate"  │
  │    "signature": "<b64>" } ──► │  verify_ed25519(sig, pubkey, challenge)
  │                               │
  │◄── { "ok": true } ────────────│
```

實作於 `auth.rs`，使用 `ring` crate 的 `ED25519` 算法。

### 4.4 日誌廣播（BroadcastLayer）

`log.rs` 實作自訂 `tracing_subscriber::Layer<S>`：

```
tracing 事件
    │
    ▼
BroadcastLayer::on_event()
    │  提取 level + target + message
    ▼
broadcast::Sender<String>
    │  JSON: { "level": "INFO", "target": "...", "message": "..." }
    ├──► WebSocket client A (logs.subscribe)
    └──► WebSocket client B
```

Server 端用 `tokio::select!` 並行處理收訊與推播：

```rust
loop {
    tokio::select! {
        msg_opt = stream.next() => { /* handle RPC */ }
        log_line = log_rx.recv(), if logs_subscribed => {
            sink.send(Message::Text(log_line)).await;
        }
    }
}
```

### 4.5 Cron 任務管理

任務儲存於 `~/.duduclaw/cron_tasks.jsonl`，每行一個 JSON：

```json
{"id": "uuid", "name": "daily-reflect", "schedule": "0 0 * * *", "agent_id": "dudu", "action": "meso_reflect", "enabled": true}
```

操作：
- `cron.list` → 讀取全部行
- `cron.add` → 追加新行
- `cron.pause` → 原地修改 `enabled: false`
- `cron.remove` → 刪除對應行

### 4.6 HeartbeatScheduler

`duduclaw-agent/src/heartbeat.rs` 定期觸發 Meso 反思：

```
HeartbeatScheduler::run()
    │
    ├─ 每次心跳 →  trigger_meso_reflection()
    │                   │
    │                   └─ spawn: python3 -m duduclaw.evolution.run meso
    │
    └─ budget 檢查（token 用量 vs 上限）
```

---

## 五、Python 擴充層

### 5.1 模組結構

```
python/duduclaw/
├── channels/           # 通道插件
│   ├── base.py         # BaseChannel，on_message_received() 呼叫 _native.send_to_bus()
│   ├── telegram.py     # Telegram long polling
│   ├── line_.py        # LINE webhook
│   └── discord_.py     # Discord（備用，主要由 Rust 處理）
│
├── sdk/                # Claude Code SDK 橋接
│   ├── chat.py         # 呼叫 claude CLI subprocess 執行對話
│   ├── account.py      # Account 物件（含完整 api_key 欄位）
│   ├── rotator.py      # AccountRotator：4 種輪替策略
│   └── health.py       # check_account_health()：呼叫 GET /v1/models 驗證
│
├── evolution/          # 三層反思系統
│   ├── micro.py        # 每次對話後的即時反思
│   ├── meso.py         # 每小時心跳，分析最近 3 天 daily notes
│   ├── macro_.py       # 每日排程，評估技能品質
│   └── run.py          # CLI 入口：python3 -m duduclaw.evolution.run <level>
│
└── tools/              # Agent 動態管理工具
    └── agent_tools.py  # agent_list / agent_create / agent_delegate / agent_status
```

### 5.2 帳號輪替（AccountRotator）

支援 4 種策略：

| 策略 | 說明 |
|------|------|
| `round-robin` | 依序使用每個帳號 |
| `least-used` | 優先選用使用次數最少的帳號 |
| `budget-aware` | 依預算剩餘比例加權選擇 |
| `failover` | 主帳號優先，失敗時依序切換備用 |

### 5.3 PyO3 橋接層

`duduclaw-bridge` crate 提供 Python native module `_native`：

```python
import _native
_native.send_message(agent_id, content)   # 寫訊息到 bus_queue.jsonl
_native.send_to_bus(payload_json)         # 通用 bus 寫入
```

---

## 六、安全系統

### 6.1 API Key 加密

```
duduclaw onboard
    │
    │  使用者輸入明文 API Key
    ▼
AES-256-GCM 加密（ring crate）
    │  key 存於 ~/.duduclaw/secret.key
    ▼
anthropic_api_key_enc = "<base64 ciphertext+nonce+tag>"
    │  寫入 ~/.duduclaw/config.toml
```

讀取時：解密 → 注入 `ANTHROPIC_API_KEY` 環境變數，不落地。

### 6.2 帳號健康檢查

`health.py` 呼叫 Anthropic REST API 驗證 key 有效性：

```
GET https://api.anthropic.com/v1/models
    ├─ 200 OK → healthy
    ├─ 401   → unhealthy（key 無效或過期）
    └─ 網路錯誤 → inconclusive（不計入輪替降級）
```

---

## 七、Evolution 三層反思引擎

每層均呼叫真實 `claude` CLI subprocess，輸出結構化 JSON。

### 7.1 Micro 反思（每次對話後）

觸發時機：對話結束後立即執行

Claude 提示產出：
```json
{
  "what_went_well": ["...", "..."],
  "what_could_improve": ["...", "..."],
  "patterns_noticed": ["...", "..."],
  "candidate_skills": [{"name": "...", "description": "..."}]
}
```

結果寫入當日 `memory/YYYYMM/YYYYMMDD.md`。

### 7.2 Meso 反思（每小時心跳）

觸發時機：`HeartbeatScheduler` 定時觸發 → `python3 -m duduclaw.evolution.run meso`

Claude 分析最近 3 天的 daily notes，產出：
```json
{
  "common_patterns": ["...", "..."],
  "candidate_skills": [{"name": "...", "description": "..."}],
  "period": "2026-03-17 to 2026-03-19"
}
```

### 7.3 Macro 反思（每日排程）

觸發時機：Cron 任務排程（`0 0 * * *`）

Claude 評估 `SKILLS/` 下每個技能的品質：
```json
{
  "skills_to_improve": ["skill-name"],
  "skills_to_archive": ["old-skill"],
  "soul_alignment_score": 0.87,
  "recommendations": ["...", "..."]
}
```

---

## 八、記憶系統

### 8.1 SQLite + FTS5

`duduclaw-memory/src/engine.rs` 實作 `SqliteMemoryEngine`：

```sql
CREATE TABLE memories (
    id        TEXT PRIMARY KEY,
    agent_id  TEXT NOT NULL,
    content   TEXT NOT NULL,
    timestamp TEXT NOT NULL,
    tags      TEXT NOT NULL DEFAULT '[]'
);

CREATE VIRTUAL TABLE memories_fts USING fts5(
    content,
    agent_id UNINDEXED,
    memory_id UNINDEXED,
    tokenize='unicode61'
);
```

### 8.2 操作方法

| 方法 | 說明 |
|------|------|
| `store(agent_id, entry)` | 同時寫入 `memories` + `memories_fts` |
| `search(agent_id, query, limit)` | FTS5 MATCH 搜尋，按相關性排序 |
| `list_recent(agent_id, limit)` | ORDER BY timestamp DESC（無 FTS） |
| `summarize(agent_id, window)` | 取時間區間 entries，呼叫 Claude 產生語意摘要 |

### 8.3 Token 估算（CJK-aware）

Session 壓縮前估算 token 數：

```rust
fn estimate_tokens(text: &str) -> u32 {
    // CJK: U+3000–U+9FFF 及相關範圍
    // CJK 字元 ~1.5 chars/token
    // ASCII 字元 ~4 chars/token
    let cjk_tokens = (cjk_chars as f32 / 1.5).ceil() as u32;
    let other_tokens = (other_chars as f32 / 4.0).ceil() as u32;
    cjk_tokens + other_tokens + 1
}
```

超過 50k token 時，呼叫 `claude-haiku` 產生摘要並壓縮 session。

---

## 九、通訊通道

### 9.1 架構

所有通道最終都經過 `channel_reply.rs` 的 `build_reply()` 處理，統一通過 session 管理與 Claude 呼叫：

```
用戶訊息 (LINE/Telegram/Discord)
    │
    ▼
build_reply(content, ctx)
    │
    ├─ 取得/建立 session (SQLite)
    ├─ 估算 token，超限自動壓縮
    ├─ 組裝 Claude 提示 (含 SOUL.md + session 歷史)
    ├─ 呼叫 claude CLI subprocess
    ├─ 儲存回覆至 session
    └─ 返回回覆文字
```

### 9.2 Discord（Gateway WebSocket）

`discord.rs` 使用 `tokio::select!` 並行處理訊息與心跳：

```rust
loop {
    tokio::select! {
        msg_opt = read.next() => {
            // 處理 HELLO / DISPATCH / RECONNECT
        }
        Some(seq) = heartbeat_rx.recv() => {
            // 獨立計時器觸發，不依賴訊息到達
            write.send(heartbeat_payload).await;
        }
    }
}
```

### 9.3 Telegram

Long polling 模式，定期呼叫 `getUpdates`，無需公開 webhook endpoint。

### 9.4 LINE

Webhook 模式，需公開 HTTPS endpoint（由 `duduclaw gateway` 提供）。

---

## 十、Web 管理介面

### 10.1 技術棧

- **React 19 + TypeScript** — 前端框架
- **shadcn/ui + Tailwind CSS 4** — UI 元件，溫暖 amber 色系
- **Zustand** — 狀態管理
- **WebSocket** — 即時資料（系統狀態、日誌推播）
- **rust-embed** — React SPA 嵌入 Rust binary（零額外部署）

### 10.2 頁面

| 頁面 | 功能 |
|------|------|
| Dashboard | 系統狀態、uptime、連線數 |
| Agents | Agent 清單、狀態、建立/暫停/恢復 |
| Memory | 記憶搜尋、瀏覽 |
| Channels | Telegram/LINE/Discord 連線狀態 |
| Logs | 即時 log 推播（BroadcastLayer） |
| Cron | 排程任務管理（CRUD） |
| Settings | API key 管理、帳號輪替設定 |

---

## 十一、專案結構

```
DuDuClaw/
├── crates/                     # Rust crates（9 個）
│   ├── duduclaw-core/          # 共用型別 + traits
│   ├── duduclaw-agent/         # Agent 管理 + 心跳 + 預算
│   ├── duduclaw-security/      # AES-256-GCM + Ed25519
│   ├── duduclaw-memory/        # SQLite + FTS5 記憶引擎
│   ├── duduclaw-gateway/       # Axum + 通道 + WebSocket + log
│   ├── duduclaw-bus/           # tokio broadcast/mpsc 路由
│   ├── duduclaw-bridge/        # PyO3 Rust↔Python
│   ├── duduclaw-cli/           # CLI 入口 + MCP server
│   └── duduclaw-dashboard/     # rust-embed React SPA
│
├── python/duduclaw/            # Python 擴充層
│   ├── channels/               # 通道插件
│   ├── sdk/                    # Claude Code SDK 整合
│   ├── evolution/              # 三層反思系統
│   └── tools/                  # Agent 動態管理工具
│
├── web/                        # React Dashboard
│   └── src/
│       ├── components/         # UI 元件 (shadcn/ui)
│       ├── pages/              # 頁面
│       ├── stores/             # Zustand 狀態管理
│       ├── lib/                # WebSocket API client
│       └── i18n/               # zh-TW / en
│
├── config/                     # 設定範例
├── scripts/                    # 安裝腳本
├── ARCHITECTURE.md             # 本文件
└── CLAUDE.md                   # AI 協作設計上下文
```

---

## 十二、設定格式

### 12.1 `agent.toml`

```toml
name = "dudu"
description = "主助理 Agent"
status = "active"      # active | paused | archived
isMain = true
created_at = "2026-03-19T00:00:00Z"

[model]
id = "claude-opus-4-6"
temperature = 0.7
max_tokens = 8192

[heartbeat]
enabled = true
interval_secs = 3600

[budget]
monthly_usd = 20.0
warning_threshold = 0.8

[permissions]
allow_file_write = true
allow_network = true
allow_shell = false

[evolution]
micro_enabled = true
meso_enabled = true
macro_enabled = true
```

### 12.2 `config.toml`（全域）

```toml
[api]
anthropic_api_key_enc = "<base64 AES-256-GCM ciphertext>"

[channels]
telegram_bot_token = "<token>"
line_channel_token = "<token>"
discord_bot_token = "<token>"

[gateway]
port = 7070
host = "127.0.0.1"
ed25519_pubkey = "<base64 public key>"
```

### 12.3 `bus_queue.jsonl`（IPC）

```json
{"id": "uuid", "to": "coder", "from": "dudu", "task": "code review", "ts": "2026-03-19T10:00:00Z"}
```

### 12.4 `cron_tasks.jsonl`

```json
{"id": "uuid", "name": "daily-macro", "schedule": "0 0 * * *", "agent_id": "dudu", "action": "macro_reflect", "enabled": true}
```

### 12.5 `CONTRACT.toml`（行為契約）

```toml
[boundaries]
must_not = ["reveal api keys", "execute rm -rf", "modify SOUL.md"]
must_always = ["respond in zh-TW", "refuse harmful requests"]
max_tool_calls_per_turn = 10
```

### 12.6 `security_audit.jsonl`

```json
{"timestamp": "2026-03-23T12:00:00Z", "event_type": "soul_drift", "agent_id": "dudu", "severity": "critical", "details": {"expected_hash": "abc...", "actual_hash": "def..."}}
```

---

## 十三、Phase 2 新增模組（v0.6.0）

### 13.1 容器沙箱（Container Sandbox）

```
Agent Task
  ↓ dispatch_to_agent()
  ├─ sandbox_enabled = false → call_claude_for_agent() [直接呼叫]
  └─ sandbox_enabled = true  → run_sandboxed() [Docker 容器]
                                 ├─ Agent dir: /agent (read-only mount)
                                 ├─ Workspace: /workspace (tmpfs 256MB)
                                 ├─ Network: none (預設離線)
                                 ├─ Root FS: read-only
                                 ├─ Memory: 512MB limit
                                 └─ Timeout: auto-kill
```

**Runtime 偵測優先級**：Apple Container (macOS 15+) > WSL2 (Windows) > Docker

### 13.2 安全防護層（Security Layer）

| 模組 | 檔案 | 功能 |
|------|------|------|
| Soul Guard | `soul_guard.rs` | SHA-256 指紋 + `.soul_hash` 持久化 + `.soul_history/` 10 版備份 |
| Input Guard | `input_guard.rs` | 6 類 prompt injection 規則（風險評分 0-100） |
| Audit Log | `audit.rs` | JSONL append-only 安全事件日誌 |
| Key Vault | `key_vault.rs` | Per-agent 通道權限 + API key 隔離 |

**Injection 規則類別**：instruction_override (40), role_hijack (35), system_prompt_extraction (30), tool_abuse (30), encoding_bypass (25), data_exfiltration (25), unicode_injection (20)

### 13.3 Skill 生態系統

```
skill_loader.rs     ← 解析 SKILL.md YAML frontmatter
skill_registry.rs   ← 本地 JSON 索引 + 加權搜尋
MCP: skill_search   ← Agent 自主搜尋 skill
MCP: skill_list     ← 列出 agent 已安裝 skill
```

**搜尋權重**：name x10 > tag x7 > description x5

### 13.4 紅隊測試（Red Team）

`duduclaw test <agent>` 內建 9 項測試：

1. SOUL.md 完整性
2. 行為契約存在性
3. Instruction override 偵測
4. Role hijack 偵測
5. System prompt extraction 偵測
6. Tool abuse 偵測
7. Data exfiltration 偵測
8. Encoding bypass 偵測
9. Contract enforcement 驗證

輸出：彩色終端機摘要 + `~/.duduclaw/test-report-<agent>.json`

### 13.5 Sub-Agent 編排

```
Main Agent (Claude Code)
  ├─ create_agent(name, role, soul, reports_to)  → 建立持久化 agent 目錄
  ├─ spawn_agent(agent_id, task)                 → 寫入 bus_queue.jsonl
  │     → AgentDispatcher 消費 → spawn claude CLI → 結果回寫
  ├─ list_agents()                               → 掃描 agents/ 目錄
  └─ agent_status(agent_id)                      → 配置 + 待處理任務數
```

### 13.6 統一 Heartbeat Scheduler

取代舊版 `start_evolution_timers`（全局硬編碼），改為 per-agent 排程：

- 每個 agent 獨立的 cron/interval 設定
- `max_concurrent_runs` Semaphore 並行控制
- 每 30 秒 tick，每 5 分鐘從 registry 重新同步
- Meso reflection：每次 heartbeat 觸發
- Macro reflection：每 24 小時觸發
- RPC 方法：`heartbeat.status` + `heartbeat.trigger`
