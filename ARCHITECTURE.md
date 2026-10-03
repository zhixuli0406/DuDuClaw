# DuDuClaw 系統架構設計

> 版本：1.66.1（workspace `Cargo.toml`）
> 日期：2026-10-01
>
> 本文件是精簡總覽。各子系統的細節見
> [docs/architecture/overview.md](docs/architecture/overview.md)、
> [docs/architecture/evolution-engine.md](docs/architecture/evolution-engine.md)
> 與 [docs/features/](docs/features/README.md)；版本差異見 [CHANGELOG.md](CHANGELOG.md)。

---

## 目錄

1. [設計決策](#一設計決策)
2. [總覽：Plumbing 層](#二總覽plumbing-層)
3. [多 Agent 架構](#三多-agent-架構)
4. [Rust 核心層](#四rust-核心層)
5. [Python 套件](#五python-套件)
6. [安全系統](#六安全系統)
7. [自主進化引擎（Prediction-Driven + AEE）](#七自主進化引擎prediction-driven--aee)
8. [記憶系統](#八記憶系統)
9. [通訊通道](#九通訊通道)
10. [Web 管理介面](#十web-管理介面)
11. [專案結構](#十一專案結構)
12. [設定格式](#十二設定格式)
13. [其他子系統](#十三其他子系統)

---

## 一、設計決策

| 決策項目 | 選擇 | 理由 |
|----------|------|------|
| AI 對話 | **多 runtime CLI**（`AgentRuntime` trait）：Claude Code、Codex、Antigravity（`agy`）、Grok、OpenAI-compatible；Gemini CLI 於 v1.67.0 棄用、v1.70.0 移除（由 Antigravity 取代）；另有 `runtime_catalog.rs` 收錄的其他 CLI | 工具鏈、MCP 相容與 session 由各家 CLI 負責，per-agent 在 `agent.toml [runtime]` 選擇 |
| 核心語言 | **Rust** | 記憶體安全、高效能、單 binary 部署 |
| Python | **獨立 companion 套件**（`pip install duduclaw`），無 PyO3 綁定 | Rust binary 不呼叫它，見第五節 |
| Agent 隔離 | **資料夾 + SOUL.md**；選配 Docker 任務沙箱（預設關） | 預設無容器開銷；沙箱見 [docs/guides/task-sandbox.md](docs/guides/task-sandbox.md) |
| IPC 機制 | **File-based queue** (`bus_queue.jsonl`) | 零依賴，跨程序讀寫，天然持久化 |
| 通道 | 十一個：Telegram、LINE、Discord、Slack、WhatsApp、Feishu、Google Chat、Microsoft Teams、WeCom、DingTalk、WebChat | 全部在 Rust gateway 內實作 |
| Dashboard 認證 | **JWT 帳號登入**（`duduclaw-auth`：Argon2 + JWT，`users.db`）或 admin token（`[gateway] auth_token_enc`） | 早期的 Ed25519 challenge-response 認證路徑已移除（從來沒有設定能啟用它，儀表板也沒有實作 client 端） |
| API key 儲存 | **AES-256-GCM** | 金鑰檔 `~/.duduclaw/.keyfile`，密文以 base64 存於 config |
| 日誌推送 | **BroadcastLayer** tracing | 即時推播 log 到 WebSocket，零侵入 |
| Evolution | **預測驅動 + AEE playbook 進化**（整份改寫 SOUL.md 的舊路徑已於 2026-09-29 移除） | 設計上多數對話不需呼叫 LLM（未公布實測比例），Significant/Critical 誤差與少量 ε 探索才觸發；`SOUL.md` 對 agent 唯讀 |
| 任務驗收 | **判官前確定性防線**：grounding 證據預檢 + outcome schema 校驗；任務層 forward model（`[task_forward_model]` 自 v1.54 預設開） | 證據不落地就不燒判官 LLM；世界模型統計先行，冷啟動零 LLM（見 [docs/guides/goal-loop.md](docs/guides/goal-loop.md)） |
| Token 計算 | **CJK-aware heuristic** | CJK 字元 ~1.5 chars/token，ASCII ~4 chars/token |

---

## 二、總覽：Plumbing 層

DuDuClaw 本身不訓練、不提供模型。對話、工具使用與上下文管理交給 AI CLI；DuDuClaw 負責把一個或多個 CLI 變成長駐的 agent。

```
┌─────────────────────────────────────────────┐
│ AI Runtime（Claude Code / Codex / agy /     │
│ Grok / OpenAI-compat / Gemini CLI 棄用中）   │
└────────────────┬────────────────────────────┘
                 │ MCP（JSON-RPC 2.0，stdio；另有 HTTP/SSE）
┌────────────────▼────────────────────────────┐
│              DuDuClaw                       │
│  MCP Server   Session Manager               │
│  Channel Router   Memory Engine             │
│  Evolution (AEE)   Account Rotator          │
│  Task Board / Goal Loop   Autopilot         │
│  Web Dashboard (embedded)                   │
└─────────────────────────────────────────────┘
```

核心原則：
- **AI = CLI runtime**：對話邏輯、工具使用、上下文管理由各 runtime 負責
- **DuDuClaw = Plumbing**：通道路由、session 持久化、記憶搜尋、帳號輪替、排程與驗收
- **橋接 = MCP**：`duduclaw mcp-server` 以 JSON-RPC 2.0 暴露工具；`duduclaw http-server` 提供 HTTP/SSE transport

---

## 三、多 Agent 架構

### 3.1 目錄結構

每個 Agent 是一個資料夾，與 Claude Code 相容：

```
~/.duduclaw/
├── config.toml                     # 全域設定（含加密 API key）
├── .keyfile                        # AES-256-GCM 金鑰（32 bytes）
├── bus_queue.jsonl                 # 跨 Agent 訊息隊列（file-based IPC）
├── sessions.db / memory.db         # session 歷史、共用記憶庫（以 agent_id 區分）
├── tasks.db / cron_tasks.db        # Task Board、Cron 任務
├── approvals.db / events.db        # HITL 審批、Autopilot 事件
├── shared/wiki/                    # 跨 Agent 共享知識庫
│
└── agents/
    ├── dudu/
    │   ├── agent.toml              # Agent 設定（見第十二節）
    │   ├── SOUL.md                 # 人格定義（agent 唯讀）
    │   ├── CLAUDE.md               # Claude Code 指引
    │   ├── CONTRACT.toml           # 行為契約（選配）
    │   ├── .mcp.json               # MCP Server 設定（duduclaw 條目由 gateway 開機時補齊）
    │   ├── .claude/settings.json   # PreToolUse hooks（agent-file-guard 帶 --agent／--home 等）
    │   ├── SKILLS/                 # 技能集
    │   ├── wiki/                   # Agent 私有 wiki
    │   └── state/working_state.json # 跨喚醒工作狀態
    └── .ephemeral/                 # 一次性 agent（team role member 等）
```

### 3.2 跨 Agent 委派（File-based IPC）

```
Agent A → MCP tool: send_to_agent / spawn_agent
         │  （delegation_policy 授權檢查）
         ▼
bus_queue.jsonl  ← MCP server 追加 JSON 行（advisory lock）
         │
         ▼
AgentDispatcher（gateway）消費 → spawn Agent B 的 runtime CLI
```

格式（`duduclaw-cli/src/mcp/spawn.rs`）：
```json
{"type": "agent_message", "message_id": "uuid", "agent_id": "coder", "payload": "幫我 code review", "timestamp": "2026-03-19T10:00:00Z", "sender_agent": "dudu", "delegation_depth": 1, "hop_depth": 1}
```

---

## 四、Rust 核心層

### 4.1 Crate 架構

Workspace 共 24 個 crate，完整清單與一行說明見[第十一節](#十一專案結構)。主要依賴方向：`duduclaw-cli`（binary 入口）→ `duduclaw-gateway`（服務層）→ `duduclaw-agent` / `-memory` / `-security` / `-llm` / `-inference` 等 → `duduclaw-core`（共用型別）。

### 4.2 Gateway（`duduclaw-gateway`）

```
axum HTTP + WebSocket（/ws）
    │
    ├── auth.rs          — admin token 驗證輔助（JWT 由 duduclaw-auth 處理）
    ├── handlers/        — JSON-RPC dispatch（agents.*, memory.*, tasks.*, system.*, …）
    ├── channel_reply/   — 通道回覆、session 管理、prompt 組裝、token 壓縮
    ├── server.rs        — 路由、WebSocket 連線處理（tokio::select! 並行驅動）
    ├── log.rs           — BroadcastLayer：tracing → WebSocket push
    ├── runtime/         — claude / codex / gemini / antigravity / grok / openai_compat / generic_cli
    ├── dispatcher.rs    — bus_queue 消費與 sub-agent spawn
    └── telegram.rs / line.rs / discord.rs / slack.rs / whatsapp.rs / feishu.rs /
        googlechat.rs / msteams.rs / wecom.rs / dingtalk.rs / webchat.rs
```

### 4.3 WebSocket 認證

`connect` frame 接受兩種憑證（`server.rs` `handle_socket`）：

1. `{"method": "connect", "params": {"jwt": "..."}}`：`POST /api/login` 取得的 JWT
2. `{"method": "connect", "params": {"token": "..."}}`：admin token

沒有帶任何憑證的連線一律拒絕，只有一個例外：值班機（appliance）上來自本機 loopback 的連線，會得到只能操作鎖定畫面的受限 session。

早期的 Ed25519 challenge-response 路徑已從 gateway 移除：沒有任何設定讀取公鑰，只有測試建構過它，儀表板也從未實作 client 端，所以它從來無法啟用。Ed25519 仍用在授權簽章、更新驗證與 relay 裝置協定，這些不受影響。

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

### 4.5 Cron 任務管理

任務存於 SQLite `~/.duduclaw/cron_tasks.db`（`cron_store.rs`，WAL；舊的 `cron_tasks.jsonl` 開機時自動遷移並改名為 `.migrated`），由 `CronScheduler`（`cron_scheduler.rs`）依 cron 表達式觸發。主要欄位：`id`、`name`、`agent_id`、`cron`、`task`、`enabled`、`cron_timezone`、`notify_channel` / `notify_chat_id`，外加執行統計。

RPC：`cron.list` / `cron.add` / `cron.update` / `cron.pause` / `cron.resume` / `cron.remove`。

### 4.6 HeartbeatScheduler

`duduclaw-agent/src/heartbeat.rs`：

```
HeartbeatScheduler::run()  （每 30 秒 tick，每 5 分鐘從 registry 重新同步）
    │
    ├─ 每個 agent 的 cron/interval 心跳 → bus polling（max_concurrent_runs semaphore）
    ├─ Task Board 拉取：對所有 agent（不論 heartbeat.enabled），每 agent 每 60 秒最多一次
    └─ 靜默破壞器：超過 max_silence_hours 無進化觸發 → 發出 SilenceBreakerEvent，
       由 gateway 轉成 forced reflection
```

一般的進化反思由預測引擎在對話後事件驅動觸發。

---

## 五、Python 套件

`python/duduclaw/` 是發佈到 PyPI 的 companion 套件，與 Rust binary 並存，Rust 端不呼叫它，也沒有 PyO3 綁定：

```
python/duduclaw/
├── agents/        # capability-based agent routing（manifest loader、matcher、router）
├── mcp/           # MCP 輔助：API key 認證與 scope 檢查、記憶工具
├── evolution/     # Skill Vetter 安全掃描（vetter.py、run.py）
├── tools/         # agent_tools.py：agent_list / agent_create / agent_delegate / agent_status
└── memory_eval/   # 記憶評測（LOCOMO 等），僅 repo 內使用，不進 wheel
```

`agent_tools.py` 的 `agent_delegate` 會嘗試 `import _native`；repo 內沒有對應的原生模組，因此回傳 `"_native bridge not available"` 警告。通道、帳號輪替與健康檢查都在 Rust 端（第六、九節）。

---

## 六、安全系統

### 6.1 API Key 加密

```
duduclaw onboard / dashboard
    │  使用者輸入明文 API Key
    ▼
AES-256-GCM 加密（ring crate）
    │  key 存於 ~/.duduclaw/.keyfile
    ▼
api_key_enc / *_enc = "<base64 ciphertext>"
    │  寫入 ~/.duduclaw/config.toml
```

讀取時解密，只在 spawn 時顯式注入子行程環境；spawn env 以白名單建立（`duduclaw-core/src/spawn_env.rs`），其餘 `*_API_KEY` 等變數不會繼承。`secret://` 參照由 `duduclaw-security/src/secret_ref.rs` 解析。

### 6.2 帳號輪替與健康檢查

帳號池在 `duduclaw-agent/src/account_rotator/`，策略為 `priority`（預設）/ `least_cost` / `failover` / `round_robin`。健康檢查（`credential_probe.rs`）呼叫：

```
GET https://api.anthropic.com/v1/models
    ├─ 200      → 恢復可用
    ├─ 401/403  → auth_dead（指數退避 15 分鐘到 6 小時）
    └─ 429 / 網路錯誤 → 不改變狀態
```

### 6.3 其他防護

| 模組 | 位置 | 功能 |
|------|------|------|
| Soul Guard | `duduclaw-security/src/soul_guard.rs` | SHA-256 指紋（`~/.duduclaw/soul_hashes/`）+ `.soul_history/` 最多 10 版備份 |
| Input Guard | `duduclaw-security/src/input_guard.rs` | 7 類 prompt injection 規則，NFKC 正規化，分數 ≥ 60 阻擋 |
| Audit Log | `duduclaw-security/src/audit.rs` | `security_audit.jsonl` append-only 安全事件 |
| PreToolUse hooks | `duduclaw hook agent-file-guard` / `data-file-guard` | matcher `Write\|Edit\|MultiEdit\|NotebookEdit\|Bash`。擋 agent 寫自己的 SOUL.md 與 CONTRACT.toml、跨 agent 寫入、組織欄位修改；AI 員工在資料目錄只能寫自己的目錄與 `attachments/`（以符號連結解析後的落點判斷）；自己 `agent.toml` 只有可編輯清單裡的區段能改。Bash 部分是啟發式減速帶，規則與限制見 05 |
| 委派授權 | `duduclaw-core/src/delegation_policy.rs` | `reports_to` 樹 + 部門 + 白名單，fail-closed |

**Injection 規則類別與權重**：instruction_override (40)、role_hijack (35)、system_prompt_extraction (30)、tool_abuse (30)、termination_manipulation (30)、encoding_bypass (25)、data_exfiltration (25)。

詳見 [docs/features/05-security-defense.md](docs/features/05-security-defense.md)。

---

## 七、自主進化引擎（Prediction-Driven + AEE）

> 完整技術文件：[docs/architecture/evolution-engine.md](docs/architecture/evolution-engine.md)、[docs/features/38-aee-playbook-evolution.md](docs/features/38-aee-playbook-evolution.md)

進化引擎以**預測誤差**驅動，設計上多數對話不需呼叫 LLM（未公布實測比例）。`SOUL.md` 對 agent 唯讀，學習落地成 playbook 行為規則（獨立驗證、獨立回滾）。整份改寫 `SOUL.md` 的舊 GVU 路徑（含 24h 觀察期與 `legacy_soul_evolution` 開關）已於 2026-09-29 移除。

### 7.1 預測引擎

每次對話後執行（零 LLM），程式在 `duduclaw-gateway/src/prediction/`：

```
predict() → calculate_error() → route()
    │               │                │
    ▼               ▼                ▼
 UserModel     PredictionError    EvolutionAction
 統計預測       加權組合誤差        None / StoreEpisodic / TriggerReflection / TriggerEmergencyEvolution
```

**誤差分級**（預設閾值，`metacognition.rs`）：

| 等級 | 閾值 | 動作 | LLM 成本 |
|------|------|------|---------|
| Negligible | < 0.2 | 無 | 0 |
| Moderate | 0.2-0.5 | 存情節記憶 | 0 |
| Significant | 0.5-0.8 | 反思（AEE 回合） | 有 |
| Critical | ≥ 0.8 | 緊急進化 | 有 |

**MetaCognition**：每 100 次預測自適應調整閾值，雙向調整。

### 7.2 AEE 回合

`gvu/aee/`：`intent.rs` 依 `[evolution] strategy` 決定 repair / optimize / innovate → `inner_loop.rs` 最多 3 輪 generate / gate / shadow-apply / score → Gate（`verifier_gate.rs`，確定性、零 LLM、可否決）先跑，再由 Measure（`verifier_measure.rs`，評測案例通過率、判官分數等，無否決權）評分 → 與 champion 快照比較，matches-or-improves 才 commit → 逐條 entry 依自己的 eval case 在 `aee_settle_hours` 後結算，退步只回滾該條。`agent.toml [evolution] gvu_enabled` 為入口開關，`gvu_cooldown_minutes` 控制頻率。

### 7.3 安全機制

- **XML 隔離**：不受信任內容以 XML tag 包裹（`xml_fence.rs`）
- **合約強制**：`CONTRACT.toml` 的 `must_not` 進入 Gate 檢查（`must_always` 只寫進提示，Gate 不檢查）
- **SOUL.md 唯讀**：MCP `agent_update_soul` 與 file-guard hook 拒絕 agent 身分寫入
- **SHA-256 指紋**：soul_guard 偵測非法修改
- **Reward-hack 稽核**：`gvu/reward_hack.rs`

---

## 八、記憶系統

### 8.1 SQLite + FTS5

`duduclaw-memory/src/engine.rs` 實作 `SqliteMemoryEngine`，所有 agent 共用 `~/.duduclaw/memory.db`，以 `agent_id` 區分（舊的 per-agent `memory.db` 開機時由 `memory_migrate.rs` 合併）。`memories` 表除 `id` / `agent_id` / `content` / `timestamp` / `tags` 外，另有 `layer`（episodic / semantic）、`importance`、`access_count`，以及 temporal / 知識圖譜欄位（`valid_from`、`valid_until`、`superseded_by`、`subject` / `predicate` / `object` 等）。全文索引：

```sql
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
| `store(agent_id, entry)` | 寫入 `memories` + `memories_fts` |
| `search(agent_id, query, limit)` | FTS5 搜尋，預設只回傳目前有效的事實 |
| `store_temporal(entry, meta)` | 同 subject/predicate 自動取代舊事實並串接 supersession chain |
| `get_by_ids(...)` | 批次取回（MCP `memory_fetch_batch`） |
| `list_recent(agent_id, limit)` | 依時間排序（無 FTS） |
| `summarize(agent_id, window)` | 取時間區間 entries，呼叫 Claude 產生摘要 |

檢索排序與衰減（Ebbinghaus、HippoRAG-lite）見 [docs/features/10-cognitive-memory.md](docs/features/10-cognitive-memory.md) 與 [docs/features/20-memory-intelligence.md](docs/features/20-memory-intelligence.md)。

### 8.3 Token 估算（CJK-aware）

Session 壓縮前估算 token 數（`channel_reply/delivery.rs`）：

```rust
fn estimate_tokens(text: &str) -> u32 {
    // CJK: U+3000–U+9FFF、U+F900–U+FAFF 及兩段 supplementary 範圍
    let cjk_tokens = (cjk_chars as f32 / 1.5).ceil() as u32;
    let other_tokens = (other_chars as f32 / 4.0).ceil() as u32;
    cjk_tokens + other_tokens + 1
}
```

Session 存於 `~/.duduclaw/sessions.db`，超過 50k token（`session.rs` `COMPRESSION_THRESHOLD`）時產生摘要並壓縮。

---

## 九、通訊通道

### 9.1 架構

所有通道最終經過 `channel_reply/entry.rs` 的 `build_reply()`，統一走 session 管理與 runtime 呼叫：

```
用戶訊息（十一個通道之一）
    │
    ▼
build_reply(text, ctx)
    │
    ├─ 取得/建立 session (sessions.db)
    ├─ 估算 token，超限自動壓縮
    ├─ 組裝 prompt（SOUL.md、session 歷史、記憶、工作狀態等）
    ├─ 經帳號輪替呼叫 runtime CLI subprocess
    ├─ 儲存回覆至 session
    └─ 返回回覆文字（依平台轉換 markdown）
```

### 9.2 各通道傳輸方式

| 通道 | 傳輸 |
|------|------|
| Telegram | Long polling（`getUpdates`） |
| LINE | Webhook `POST /webhook/line`（或經 cloud relay） |
| Discord | Gateway WebSocket，`tokio::select!` 並行處理訊息與心跳，支援 op 6 RESUME |
| Slack | Socket Mode |
| WhatsApp | Cloud API webhook |
| Feishu、WeCom、DingTalk | Webhook（各自的簽章驗證） |
| Google Chat、Microsoft Teams | Webhook（JWT 驗證） |
| WebChat | WebSocket |

---

## 十、Web 管理介面

### 10.1 技術棧

- **Vite + React 19 + TypeScript + React Router 7**
- **Tailwind CSS 4** + 自有元件庫 `web/src/components/mds/`（設計規範見 `web/DESIGN.md`）
- **Zustand**：狀態管理
- **WebSocket**：JSON-RPC 與即時資料（系統狀態、日誌推播、Activity Feed）
- **rust-embed**：`duduclaw-dashboard` 把 build 產物嵌入 binary
- **i18n**：zh-TW / en / ja-JP（`web/src/i18n/`）

### 10.2 頁面

`web/src/pages/` 目前有 74 個 `*Page.tsx`，涵蓋首頁、AI 員工、通道、記憶與知識、Task Board / Goals、Autopilot、帳號與成本、安全、裝置與系統設定等。導覽分組見 `web/src/apps/registry.ts`。

---

## 十一、專案結構

```
DuDuClaw/
├── crates/                       # Rust workspace（24 個 crate）
│   ├── duduclaw-core/            # 共用型別、traits、設定解析、runtime/tool catalog、委派政策
│   ├── duduclaw-auth/            # 多用戶認證（Argon2、JWT、ACL、OTP）
│   ├── duduclaw-gateway/         # 服務層：axum 伺服器、通道、WebSocket RPC、runtime、進化、排程
│   ├── duduclaw-security/        # AES-256-GCM、soul guard、input guard、audit、secret 參照
│   ├── duduclaw-memory/          # SQLite + FTS5 記憶引擎、wiki、因果證據圖
│   ├── duduclaw-container/       # 腳本沙箱的容器後端（Docker；Windows 先試 WSL2）
│   ├── duduclaw-agent/           # Agent registry、心跳、預算、帳號輪替、skill 載入
│   ├── duduclaw-cli/             # `duduclaw` binary：clap CLI、MCP server（stdio / HTTP / SSE）
│   ├── duduclaw-dashboard/       # rust-embed 嵌入 React SPA
│   ├── duduclaw-odoo/            # Odoo ERP JSON-RPC 中間層
│   ├── duduclaw-inference/       # 本地 LLM 推論（OpenAI-compatible HTTP、llamafile、路由）
│   ├── duduclaw-desktop/         # 原生桌面控制（滑鼠、鍵盤、截圖）；gateway 已不再使用（native 電腦操作模式於 2026-10 移除）
│   ├── duduclaw-identity/        # Identity Resolution provider（RFC-21 §1）
│   ├── duduclaw-redaction/       # 敏感資料去識別化管線（RFC-23）
│   ├── duduclaw-cli-runtime/     # 跨平台 one-shot PTY 呼叫 AI CLI
│   ├── duduclaw-license/         # License client：解析、驗證、功能閘
│   ├── duduclaw-fork/            # Live Run Forking（RFC-26）：並行分支 + AI 判官選擇
│   ├── duduclaw-llm/             # Provider-agnostic API 層（Anthropic / OpenAI / Gemini / OpenAI-compat）
│   ├── duduclaw-sandbox/         # 原生行程隔離（macOS Seatbelt / Linux Landlock）
│   ├── duduclaw-os/              # OS 整合（檔案監看、原生通知、open），OS-native 線 Phase 1
│   ├── duduclaw-pets/            # 照片轉桌面寵物包 + 本地去背
│   ├── duduclaw-relay/           # Cloud Relay：webhook 轉發 + LAN 裝置探索（可獨立部署）
│   ├── duduclaw-sysd/            # 值班機 image 的權限分離 root 系統服務（Unix socket）
│   └── duduclaw-db/              # 唯讀 SQL 資料來源（PostgreSQL / MySQL / SQLite）
│
├── web/                          # React Dashboard（見第十節）
├── python/duduclaw/              # Python companion 套件（見第五節）
├── src-tauri/                    # 桌面 app 殼（Tauri 2，不在主 workspace）
├── npm/                          # npm 發佈包（各平台 binary + wrapper）
├── clients/                      # 外部用戶端（VS Code、Chrome、Obsidian、Stream Deck、WordPress）
├── distribution/                 # 散發素材（Claude marketplace、packs、registries、NAS、Railway 等）
├── container/                    # Dockerfile 與 compose 範例
├── config/                       # 設定範例（duduclaw.example.toml）
├── templates/                    # Agent / preset / redteam 等範本
├── evals/                        # Agent 行為評測案例
├── tests/                        # 跨 crate 測試（python/、rust/）
├── scripts/                      # 安裝與 release 腳本
├── docs/                         # 公開文件（architecture / features / guides / rfc / spec …）
├── wiki/                         # 內部知識庫與報告
├── ARCHITECTURE.md               # 本文件
└── CLAUDE.md                     # AI 協作設計上下文
```

`duduclaw-shell`、`duduclaw-comp`、`duduclaw-native-gui` 已移到 DuDuClaw-OS repo。

---

## 十二、設定格式

### 12.1 `agent.toml`（節錄自 `templates/evaluator/agent.toml`）

```toml
[agent]
name = "evaluator"
display_name = "QA Evaluator"
role = "specialist"
status = "active"
reports_to = ""

# [runtime]
# provider = "antigravity"   # claude（預設）| codex | antigravity | openai_compat | …
# fallback = "claude"

[model]
preferred = "claude-haiku-4-5"
fallback = "claude-haiku-4-5"
account_pool = []            # 空 = 全部帳號
api_mode = "cli"

[container]
sandbox_enabled = false      # 任務沙箱，見 docs/guides/task-sandbox.md
network_access = false

[heartbeat]
enabled = false
interval_seconds = 3600
max_concurrent_runs = 1
cron = ""

[budget]
monthly_limit_cents = 500
warn_threshold_percent = 80
hard_stop = false

[permissions]
can_create_agents = false
can_send_cross_agent = true
can_modify_own_soul = false

[evolution]
gvu_enabled = false          # AEE 入口開關；新 agent 由 onboarding 寫入 true
max_silence_hours = 168.0

[capabilities]
computer_use = false
allowed_tools = []
denied_tools = []
```

### 12.2 `config.toml`（全域，節錄自 `config/duduclaw.example.toml`）

```toml
[[accounts]]
id = "main"
type = "api_key"
# api_key_enc = "<AES-256-GCM ciphertext>"
monthly_budget_cents = 5000
priority = 1

[rotation]
strategy = "priority"        # round_robin | least_cost | failover | priority

[gateway]
bind = "127.0.0.1"
port = 18789

# [channels]                 # 扁平 `<platform>_<field>[_enc]` 鍵
# telegram_bot_token_enc = "<ciphertext>"
# line_channel_token_enc = "<ciphertext>"
# discord_bot_token_enc = "<ciphertext>"

# [api]                      # 帳號池為空時的最後備援
# anthropic_api_key_enc = "<ciphertext>"
```

### 12.3 `bus_queue.jsonl`（IPC）

格式見 3.2。

### 12.4 `CONTRACT.toml`（行為契約）

```toml
[boundaries]
must_not = ["reveal api keys", "execute rm -rf", "modify SOUL.md"]
must_always = ["respond in zh-TW", "refuse harmful requests"]
max_tool_calls_per_turn = 10
```

### 12.5 `security_audit.jsonl`

```json
{"timestamp": "2026-03-23T12:00:00Z", "event_type": "soul_drift", "agent_id": "dudu", "severity": "critical", "details": {"expected_hash": "abc...", "actual_hash": "def..."}}
```

---

## 十三、其他子系統

### 13.1 沙箱

兩條獨立路徑：

- **任務沙箱**（`agent.toml [container] sandbox_enabled`，預設關，僅支援 Docker，`duduclaw-gateway/src/task_sandbox.rs`）：唯讀 rootfs、非 root、drop 全部 capabilities、記憶體 4 GiB / pids 128 / `/tmp` 256 MiB 等限制、私有工作區、agent 目錄唯讀掛在 `/agent`。AI 必須連到 provider，所以需要 `network_access = true`。條件不足時 fail closed。進沙箱的是 bus／儀表板派的任務、heartbeat 看板喚醒、autopilot `delegate`／`run_skill`、goal 回合與多步驟計畫步驟；開了沙箱的員工 goal 回合一律 Solo（不組團隊），到信觸發直接跳過；通道回覆、cron、提醒、主動檢查、ephemeral、`duduclaw acp`、live `duduclaw eval` 仍在主機執行，各寫一次 `task_sandbox_not_applied` 稽核事件。見 [docs/guides/task-sandbox.md](docs/guides/task-sandbox.md)。
- **腳本沙箱**（`duduclaw-container`，PTC 與 `secaudit` PoC 使用）：macOS／Linux 用 Docker，Windows 先試 WSL2 再用 Docker（Apple Container 後端不會被選用），`--network=none`，tmpfs 工作區；無法使用時 PTC 預設拒絕執行（`[container.sandbox] script_when_unavailable`），PoC 一律不在主機執行。

### 13.2 Skill 生態系統

`duduclaw-agent` 的 `skill_loader.rs` 解析 SKILL.md frontmatter，`skill_registry.rs` 做本地加權搜尋（name +10、tag +7、description +5）。MCP：`skill_search`（`source` 參數涵蓋 hub 與 skill bank）、`skill_list`。

Skill 自動合成（Voyager-inspired）：Gap Accumulator → 從情節記憶合成 SKILL.md → 安全掃描 → 沙箱試用（TTL）→ 跨 Agent 畢業。由 `agent.toml [evolution] skill_synthesis_enabled` 控制，預設關。MCP：`skill_security_scan`、`skill_graduate`、`skill_synthesis_status`。

### 13.3 紅隊測試

`duduclaw test <agent>` 執行 9 項檢查：SOUL.md 完整性、行為契約存在性、六種 injection 偵測（instruction override、role hijack、system prompt extraction、tool abuse、data exfiltration、encoding bypass）、contract enforcement。`--bank` 可載入外部案例庫（範本 `templates/redteam/starter-bank.jsonl`）。輸出：終端機摘要 + `~/.duduclaw/test-report-<agent>.json`。

### 13.4 Sub-Agent 編排

```
Main Agent
  ├─ create_agent(name, role, soul, reports_to)  → 建立 agent 目錄
  ├─ spawn_agent / send_to_agent                 → 寫入 bus_queue.jsonl
  │     → AgentDispatcher 消費 → spawn runtime CLI → 結果回寫
  ├─ list_agents()                               → 依呼叫者可見範圍過濾
  └─ agent_status(agent_id)
```

授權規則見 [docs/features/37-delegation-isolation.md](docs/features/37-delegation-isolation.md)。

### 13.5 Task Board 與 Goal Loop

SQLite `tasks.db`。Dashboard RPC：`tasks.list` / `tasks.create` / `tasks.update` / `tasks.remove` / `tasks.assign`、`activity.list`。Agent 用 MCP：`tasks_list`、`tasks_create`、`tasks_update`、`tasks_claim`、`tasks_complete`、`tasks_block`、`activity_list`、`activity_post`。AI 員工改動或觸發別人的任務（以及 cron 管理工具、`create_reminder`）要與擁有者有委派關係（同部門、上下級或白名單），身分不明一律拒絕；未指派、未認領的任務要先認領（`tasks_update` 與 `activity_post` 的建立者例外）；goal 任務的標題、說明與驗收標準對 AI 員工凍結，`outcome:`／`grant:`／`auto-research` 控制用 tag 不能增刪或調換（規則在 `crates/duduclaw-cli/src/mcp/record_authz.rs`）。`/goal` 與 `tasks_create kind="goal"` 走自主 Goal Loop（MAV 判官驗收、卡住轉 `needs_human`），見 [docs/features/24-task-board.md](docs/features/24-task-board.md) 與 [docs/guides/goal-loop.md](docs/guides/goal-loop.md)。

### 13.6 共享知識庫（Shared Wiki）

儲存於 `~/.duduclaw/shared/wiki/`，可見性由 `wiki_visible_to` capability 控制，`.scope.toml` 定義命名空間政策。MCP：`wiki_ls` / `wiki_read` / `wiki_write` / `wiki_search` / `wiki_stats` / `wiki_lint` 以 `scope: "agent" | "shared"` 切換；六個 `shared_wiki_*` 別名已在 v1.69.0 移除，`shared_wiki_delete` 與 `wiki_share` 保留原名。見 [docs/features/17-wiki-knowledge-layer.md](docs/features/17-wiki-knowledge-layer.md)。

### 13.7 Autopilot 規則引擎

事件匯流排（`tokio::broadcast`）驅動：

- 觸發事件：`task_created` / `task_updated` / `task_status_changed` / `activity_new` / `channel_message` / `agent_idle` / `cron_tick` / `tick` / `odoo_event` / `os_file` 等
- 動作：`delegate` / `notify` / `run_skill`
- 每條規則三態斷路器，歷史記錄於 `autopilot_history`，MCP 端事件經 `events.db` 進入引擎

見 [docs/features/23-autopilot-engine.md](docs/features/23-autopilot-engine.md)。
