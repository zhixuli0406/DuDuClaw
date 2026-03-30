# TODO: Dashboard 設定功能改造

> Dashboard 從「幾乎全唯讀」升級為「可編輯所有熱重啟設定」

## Phase 1: Backend — Gateway RPC Handler

- [x] 1.1 提取共用 `update_agent_toml()` 工具函式（泛化 `update_agent_status()`）
- [x] 1.2 新增 `agents.update` handler（身份/模型/預算/Heartbeat/權限/演化）
- [x] 1.3 新增 `agents.remove` handler（移至 `_trash/`，拒絕刪除 main）
- [x] 1.4 新增 `system.update_config` handler（log_level, rotation_strategy 白名單）
- [x] 1.5 新增 `accounts.update_budget` handler（per-account 月預算）
- [x] 1.6 註冊新 handler 到 `dispatch()` + `tools.catalog`

## Phase 2: Frontend — API & Store 層

- [x] 2.1 新增 `AgentUpdateParams` 型別
- [x] 2.2 新增 `api.agents.update()`, `api.agents.remove()`
- [x] 2.3 新增 `api.system.updateConfig()`, `api.accounts.updateBudget()`
- [x] 2.4 擴展 `agents-store` — `updateAgent`, `removeAgent` action

## Phase 3: Frontend — Agent 編輯 UI

- [x] 3.1 建立 `EditAgentDialog`（4 tab: 基本資訊/模型預算/Heartbeat/權限演化）
- [x] 3.2 Agent card 加入 Edit (Pencil) + Remove (Trash2) 按鈕
- [x] 3.3 InspectDialog 加入「編輯」快捷按鈕
- [x] 3.4 Remove 確認對話框

## Phase 4: Frontend — Settings 頁面可編輯化

- [x] 4.1 GeneralTab — Log Level / Rotation Strategy 下拉選單 + Save
- [x] 4.2 HeartbeatTab — 每個 agent heartbeat 加 enabled toggle + manual trigger
- [x] 4.3 CronTab — 新增/暫停/刪除按鈕的 UI（API 已存在）

## Phase 5: i18n

- [x] 5.1 zh-TW.json 新增翻譯 key
- [x] 5.2 en.json 新增翻譯 key

## Phase 6: 深度補全（第二輪）

- [x] 6.1 Backend: `accounts.add` handler（持久化帳號到 config.toml）
- [x] 6.2 Backend: `agents.update` 補 container 欄位（sandbox, network, timeout, max_concurrent）
- [x] 6.3 Backend: `agents.update` 補進階演化欄位（max_gvu_generations, observation_period_hours, skill_token_budget）
- [x] 6.4 Frontend: AccountsPage 帳號預算 inline 編輯（Pencil icon → 數字輸入 → Save）
- [x] 6.5 Frontend: AddAccountDialog 改接 `accounts.add`（原本是空操作）
- [x] 6.6 Frontend: EditAgentDialog 新增 Container tab（sandbox, network, readonly, timeout, max_concurrent）
- [x] 6.7 Frontend: EditAgentDialog 補 api_mode 下拉（cli/direct/auto）
- [x] 6.8 Frontend: EditAgentDialog 補進階演化欄位（max_active_skills, max_silence_hours）
- [x] 6.9 Frontend: ChannelsPage 加入編輯按鈕（re-add 替換 token）
- [x] 6.10 Frontend: `api.accounts.add()` 方法
- [x] 6.11 Frontend: `AgentUpdateParams` 補 container + 進階演化欄位

## Bug 修復

- [x] Memory 頁面 DB 路徑不一致（state.db → memory.db）
- [x] Memory 頁面沒有 auto-load（加 agent 選擇器 + browse）
- [x] Memory 頁面 `memory.browse` API 未暴露
- [x] Dashboard 首頁載入 race condition（ws-client 等 authenticated 狀態）

## Build 驗證

- [x] Rust `cargo check` 通過（兩輪）
- [x] TypeScript `tsc --noEmit` 通過（兩輪）

## 風險控制

- `config.toml` 含加密 token → `system.update_config` 只接受白名單欄位
- 並發寫入 → atomic write（temp + rename）
- 寫入後即時 `registry.scan()` + 前端 `fetchAgents()` 確保 hot-reload
- 敏感資料（API key、token）絕不暴露到前端

## 可編輯欄位清單

### Agent (agent.toml)
| Section | Fields |
|---------|--------|
| Identity | display_name, role, trigger, icon, reports_to |
| Model | preferred, fallback, api_mode |
| Budget | monthly_limit_cents, warn_threshold_percent, hard_stop |
| Heartbeat | enabled, interval_seconds, cron |
| Permissions | can_create_agents, can_send_cross_agent, can_modify_own_skills, can_modify_own_soul, can_schedule_tasks |
| Evolution | skill_auto_activate, skill_security_scan, max_active_skills |

### System (config.toml) — 白名單
| Field | Valid Values |
|-------|-------------|
| log_level | trace, debug, info, warn, error |
| rotation_strategy | priority, round_robin, least_cost, failover |

### Account (config.toml [[accounts]])
| Field | Type |
|-------|------|
| monthly_budget_cents | u64 |
