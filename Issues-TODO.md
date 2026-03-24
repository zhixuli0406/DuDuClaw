# DuDuClaw — 任務清單

> 最後更新：2026-03-23
> 來源：[ISSUES.md](ISSUES.md)（已歸檔）
> 格式：`- [ ]` 待辦 ／ `- [x]` 完成

---

## Phase 1 — 基礎架構可通

> 目標：讓 Python ↔ Rust 互通、帳號管理可用、安全性基線

- [x] **[C-1]** 實作 PyO3 橋接 `send_message()` + `send_to_bus()`
  - 檔案：`crates/duduclaw-bridge/src/lib.rs`
  - 改為寫入 `~/.duduclaw/bus_queue.jsonl` 的 file-based IPC queue

- [x] **[C-6a]** 修正 `load_accounts` TOML 解析邏輯
  - 檔案：`python/duduclaw/sdk/chat.py`
  - 改用 `tomllib`（Python 3.11+ stdlib），支援 `[[accounts]]` 與 `[api]` 兩種格式

- [x] **[C-6b]** 修正 `Account` 物件儲存完整 API key
  - 檔案：`python/duduclaw/sdk/account.py`
  - `Account` 增加 `api_key: str = ""` 欄位；`get_account_key()` 依 `account_id` 查詢

- [x] **[C-6c]** `AccountRotator` 多帳號輪替
  - 4 種策略（round-robin、least-used、budget-aware、failover）均已實作

- [x] **[M-4]** API Key 改用 AES-256-GCM 加密存取
  - 檔案：`crates/duduclaw-cli/src/main.rs`
  - `duduclaw onboard` 寫入 `anthropic_api_key_enc`（base64 AES-256-GCM），讀取時解密

---

## Phase 2 — 核心流程可用

> 目標：agent 狀態持久化、IPC 通訊、帳號管理、通道接入

- [x] **[C-3]** `agents.pause` / `agents.resume` 寫入 agent.toml
  - 檔案：`crates/duduclaw-gateway/src/handlers.rs`
  - `update_agent_status()` 讀取並修改 `agent.toml` 中的 `status` 欄位

- [x] **[C-7]** `agents.delegate` 觸發真實 IPC 執行
  - 檔案：`crates/duduclaw-gateway/src/handlers.rs`
  - 透過 bus_queue.jsonl 或 subprocess 委派；支援 sync/async 兩種模式

- [x] **[H-3]** 實作 `accounts.rotate`
  - 檔案：`crates/duduclaw-gateway/src/handlers.rs`
  - 呼叫 Python subprocess 執行 `AccountRotator`

- [x] **[H-8]** Python channels 插件接入 Rust bus
  - 檔案：`python/duduclaw/channels/base.py`
  - `on_message_received()` 呼叫 `_native.send_to_bus()`

- [x] **[H-9]** `check_account_health()` 呼叫 Anthropic API 驗證
  - 檔案：`python/duduclaw/sdk/health.py`
  - 呼叫 `GET /v1/models`；401 → unhealthy，網路錯誤 → inconclusive

---

## Phase 3 — Agent 對話可用

> 目標：CLI 對話接入 Claude、跨 Agent 委派、MCP 工具可用

- [x] **[C-2a]** 實作 `duduclaw agent create <name>`
  - 建立完整 agent 目錄結構（`.claude/`、`SOUL.md`、`CLAUDE.md`、`.mcp.json`、`agent.toml`）

- [x] **[C-2b]** 實作 `duduclaw agent pause <agent>`
  - 修改 `agent.toml` 的 `status` 欄位為 `paused`

- [x] **[C-2c]** 實作 `duduclaw agent resume <agent>`
  - 修改 `agent.toml` 的 `status` 欄位為 `active`

- [x] **[H-5]** `duduclaw agent` 互動模式接入 Claude Code SDK
  - 檔案：`crates/duduclaw-agent/src/runner.rs`
  - 呼叫真實 `claude` CLI subprocess 執行對話

- [x] **[H-10a]** 實作 `agent_tools.agent_list()`
  - 掃描 `~/.duduclaw/agents/` 目錄，讀取 agent.toml 清單

- [x] **[H-10b]** 實作 `agent_tools.agent_create()`
  - 建立 agent 目錄 + `agent.toml` + `SOUL.md`

- [x] **[H-10c]** 實作 `agent_tools.agent_delegate()`
  - 透過 `_native.send_message()` bridge 傳送委派訊息

- [x] **[H-10d]** 實作 `agent_tools.agent_status()`
  - 讀取 `agent.toml` 回傳當前狀態

- [x] **[C-4a]** 實作 MCP 工具 `send_to_agent`
  - 寫入 `bus_queue.jsonl` 路由訊息到指定 agent

- [x] **[C-4b]** 實作 MCP 工具 `send_photo`
  - 透過 Telegram/Discord API 傳送圖片

- [x] **[C-4c]** 實作 MCP 工具 `send_sticker`
  - 傳送貼圖至 Telegram/Discord

- [x] **[C-4d]** 實作 MCP 工具 `log_mood`
  - 寫入 SQLite memory engine

- [x] **[C-4e]** 實作 MCP 工具 `schedule_task`
  - 寫入 `cron_tasks.jsonl`

---

## Phase 4 — Evolution & 記憶可用

> 目標：三層反思有 AI 推理、記憶系統可讀寫

- [x] **[C-5a]** Micro 反思加入 Claude 呼叫
  - 檔案：`python/duduclaw/evolution/micro.py`
  - 呼叫 `claude` CLI 產生 what_went_well / patterns_noticed / candidate_skills JSON

- [x] **[C-5b]** Meso 反思加入 Claude 呼叫
  - 檔案：`python/duduclaw/evolution/meso.py`
  - 分析最近 3 天的 daily notes，提取 common_patterns 和 candidate_skills

- [x] **[C-5c]** Macro 反思加入 Claude 呼叫
  - 檔案：`python/duduclaw/evolution/macro_.py`
  - 評估每個 skill 的品質，回傳 skills_to_improve / skills_to_archive / soul_alignment_score

- [x] **[H-7]** `memory.browse` 從 SQLite 讀取真實資料
  - 檔案：`crates/duduclaw-memory/src/engine.rs`
  - 新增 `list_recent()` 方法；`handle_memory_browse()` 回傳真實 entries

- [x] **[M-3]** 統一 `memory.search` 前後端欄位名
  - 後端 `handle_memory_search()` 回傳 `entries`（原為 `results`）

- [x] **[M-1]** Session 壓縮呼叫 Claude 產生摘要
  - 檔案：`crates/duduclaw-gateway/src/channel_reply.rs`
  - 收集歷史訊息後呼叫 `claude-haiku` 產生摘要；失敗時降級為固定字串

- [x] **[M-6]** `MemoryEngine::summarize()` 呼叫 Claude
  - 檔案：`crates/duduclaw-memory/src/engine.rs`
  - 先收集 entries（持鎖），鎖釋放後呼叫 Claude 產生語意摘要

---

## Phase 5 — Dashboard & 維運可用

> 目標：監控資料真實、Cron 可用、服務管理可用、有認證

- [x] **[H-1]** `system.status` 動態計算 uptime 與 channels_connected
  - 使用 `Instant::now()` 計算 uptime；查詢 `ChannelState` map 計算連線數

- [x] **[H-2]** `channels.status` 反映真實連線狀態
  - 各 channel bot 啟動/斷線時呼叫 `set_channel_state()` 更新 runtime map

- [x] **[H-4a]** 實作 `handle_cron_list()`
  - 從 `cron_tasks.jsonl` 讀取任務清單

- [x] **[H-4b]** 實作 `handle_cron_add()`
  - 寫入 `cron_tasks.jsonl`，支援 name / schedule / agent_id / action 欄位

- [x] **[H-4c]** 實作 `handle_cron_pause()` / `handle_cron_remove()`
  - 原地修改或刪除 `cron_tasks.jsonl` 中的任務

- [x] **[H-4d]** 實作 `HeartbeatScheduler::run()` 觸發 meso 反思
  - 檔案：`crates/duduclaw-agent/src/heartbeat.rs`
  - heartbeat 觸發時 spawn `trigger_meso_reflection()` 呼叫 Python evolution subprocess

- [x] **[H-6]** 實作 Logs WebSocket 推送
  - 新增 `crates/duduclaw-gateway/src/log.rs`：`BroadcastLayer` tracing layer
  - `logs.subscribe` → 訂閱廣播；`tokio::select!` 同時接收 WebSocket 訊息和 log 事件

- [x] **[C-2d]** 實作 `duduclaw service install`
  - macOS launchd plist；Linux systemd unit（已在 service/mod.rs 中實作）

- [x] **[C-2e]** 實作 `duduclaw service start / stop / status / logs / uninstall`
  - CLI dispatch 已連接 `service::handle_service()`

- [x] **[C-2f]** 實作 `duduclaw gateway`
  - `Commands::Gateway` → `cmd_run_server(true)`

- [x] **[M-8]** 實作 Auth Manager Ed25519 challenge-response
  - 檔案：`crates/duduclaw-gateway/src/auth.rs`
  - `issue_challenge()` 生成 32-byte 隨機挑戰；`verify_ed25519()` 使用 `ring` 驗簽
  - WebSocket 協定：`connect` → 回傳 challenge → `authenticate` with signature

---

## Phase 6 — 品質與精確度

> 目標：數字準確、監控可信、連線穩定

- [x] **[M-2]** Token 計算改為 CJK-aware heuristic
  - 檔案：`crates/duduclaw-gateway/src/channel_reply.rs`
  - `estimate_tokens()` 對 CJK 字元使用 ~1.5 chars/token；ASCII ~4 chars/token

- [x] **[M-5]** WebSocket doctor 實際 ping Docker
  - 檔案：`crates/duduclaw-gateway/src/handlers.rs`
  - `check_docker()` 執行 `docker info` 或 `podman info`；pass/warn 依據結果決定

- [x] **[M-7]** Discord heartbeat 改用 `tokio::select!`
  - 檔案：`crates/duduclaw-gateway/src/discord.rs`
  - heartbeat timer 事件與 incoming events 並行處理，不再依賴 `try_recv()`

---

---

## Phase 2（v0.6.0）— 競爭力強化

> 詳細規劃：[Phase2-TODO.md](Phase2-TODO.md)
> 參考：[docs/claw-ecosystem-report.md](docs/claw-ecosystem-report.md)

| 模組 | 任務數 | 核心價值 |
|------|--------|---------|
| A. 容器隔離 | 6 | NanoClaw/NemoClaw 安全沙箱 |
| B. Skill 市場相容 | 5 | 借用 5,400+ OpenClaw skills |
| C. 安全防護 | 6 | SOUL.md 漂移 + prompt injection 防禦 |
| D. 紅隊測試 | 4 | 部署前安全驗證 |

---

## Phase 3（v0.7.0）— Odoo ERP 整合

> 詳細規劃：[docs/odoo-integration-plan.md](docs/odoo-integration-plan.md)
> 架構：中間層 `duduclaw-odoo-bridge`（獨立 crate）

| 階段 | 任務數 | 核心價值 |
|------|--------|---------|
| 3A. 基礎連線 | 4 | XML-RPC/JSON-RPC 雙協議 + CE/EE 偵測 |
| 3B. CRM + 銷售 | 5 | 5 個 MCP 工具 + Agent 權限控制 |
| 3C. 庫存 + 會計 | 4 | 4 個 MCP 工具 |
| 3D. 事件橋接 + Dashboard | 5 | Polling + Webhook + 6 種事件 |
| 3E. 通用工具 + 報表 | 3 | 進階 search/execute + PDF |

---

## 進度總覽

| Phase | 任務數 | 完成 | 剩餘 |
|-------|--------|------|------|
| Phase 1 — 基礎架構 | 5 | 5 | 0 |
| Phase 2 — 核心流程 | 5 | 5 | 0 |
| Phase 3 — Agent 對話 | 14 | 14 | 0 |
| Phase 4 — Evolution & 記憶 | 7 | 7 | 0 |
| Phase 5 — Dashboard & 維運 | 12 | 12 | 0 |
| Phase 6 — 品質精確度 | 3 | 3 | 0 |
| **v0.1–v0.5.1 小計** | **46** | **46** | **0** |
| Phase 2 (v0.6.0) — 競爭力強化 | 21 | 21 | 0 |
| Phase 3 (v0.7.0) — Odoo ERP 整合 | 21 | 21 | 0 |
| **總計** | **88** | **88** | **0** |
