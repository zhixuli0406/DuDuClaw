# DuDuClaw v0.6.6 Deep Code Review

**Review Date**: 2026-03-24
**Reviewers**: 5 parallel Claude Code agents (Frontend / Backend / CLI / MCP / Middleware)
**Verdict**: **BLOCK** — 16 CRITICAL issues must be resolved before production deployment

---

## Executive Summary

| Layer | CRITICAL | HIGH | MEDIUM | LOW | Total |
|-------|----------|------|--------|-----|-------|
| Frontend (web/) | 2 | 7 | 9 | 7 | 25 |
| Backend (gateway/core/agent/bus/memory/container) | 5 | 7 | 8 | 5 | 25 |
| CLI (duduclaw-cli) | 3 | 7 | 8 | 5 | 23 |
| MCP Server (protocol/handlers) | 3 | 6 | 7 | 5 | 21 |
| Middleware (bridge/security/odoo/dashboard) | 3 | 7 | 8 | 4 | 22 |
| **Total** | **16** | **34** | **40** | **26** | **116** |

### Top 5 Cross-Cutting Themes

1. **Path Traversal** — `agent_id` / `name` validation is inconsistent; some handlers check, others don't
2. **TOML Injection** — `format!` string interpolation used to build agent.toml instead of `toml::to_string`
3. **Encryption Gap** — `CryptoEngine` exists but config reads often bypass decryption (明文 token)
4. **Bus Queue Safety** — `bus_queue.jsonl` file-based IPC lacks atomicity, size limits, and auth
5. **Missing Tests** — Security-critical modules (input_guard, odoo, bridge) have zero test coverage

---

## Part 1: Frontend (web/)

### CRITICAL

#### [FE-C1] WebSocket 連線無認證保護
**File**: `web/src/stores/connection-store.ts:19-27`, `web/src/lib/ws-client.ts:103-105`

`connect()` 不帶 token 時直接將狀態設為 `authenticated`。`App.tsx:20` 呼叫 `connect()` 時不帶任何 token。任何能存取 dashboard URL 的人都能直接獲得完整存取權限。

```typescript
// ws-client.ts:103-105
} else {
  // No auth required — go straight to authenticated
  this.setState('authenticated');
}
```

**Fix**: 生產環境應強制傳入 session token；若純本地工具，確保 dashboard 埠不對外暴露。

#### [FE-C2] 錯誤訊息直接洩漏系統內部資訊
**File**: `web/src/pages/ChannelsPage.tsx:65,82`

原始錯誤物件直接轉成字串顯示在 toast 中，可能洩漏內部路徑、服務名稱、堆疊軌跡。

**Fix**: 對錯誤訊息進行包裝，只顯示安全的用戶友善訊息。

### HIGH

#### [FE-H1] SecurityPage 顯示靜態硬編碼資料，非實際系統狀態
**File**: `web/src/pages/SecurityPage.tsx:61-133`

Credential Proxy、Mount Guard、RBAC、Rate Limiter 四個面板全部顯示靜態硬編碼數值，造成安全感的錯覺。

#### [FE-H2] AddAccountDialog 假實作 — API Key 被靜默丟棄
**File**: `web/src/pages/AccountsPage.tsx:170-183`

`handleSubmit` 只呼叫 `api.accounts.health()` 健康檢查，完全沒有提交帳號資料。用戶填寫 API key 後資料被丟棄但 UI 顯示成功。

#### [FE-H3] pauseAgent / resumeAgent 缺少錯誤處理
**File**: `web/src/stores/agents-store.ts:44-59`

API 呼叫失敗時用戶不會收到任何錯誤提示。

#### [FE-H4] 日誌列表 key 不穩定，5000 條日誌時嚴重卡頓
**File**: `web/src/pages/LogsPage.tsx:157-159`

使用 `${entry.timestamp}-${i}` 作為 key，ring buffer 截斷後所有 index 改變觸發全量重繪。`@tanstack/react-virtual` 已安裝但未使用。

#### [FE-H5] filteredEntries getter 每次 store 變動都全量重算
**File**: `web/src/stores/logs-store.ts:47-55`

**Fix**: 使用 `useMemo` 在 component 層計算。

#### [FE-H6] SkillMarketPage Install 按鈕無任何功能
**File**: `web/src/pages/SkillMarketPage.tsx:159`

沒有 `onClick` handler，也沒有「尚未實作」的提示。

#### [FE-H7] 多頁面重複輪詢模式，與 WebSocket 事件競爭
**File**: `web/src/pages/DashboardPage.tsx:57-62`, `web/src/pages/SettingsPage.tsx:135-141`

### MEDIUM

| ID | Issue | File |
|----|-------|------|
| FE-M1 | i18n 語系切換不會觸發頁面更新 | `web/src/i18n/index.ts:21-23` |
| FE-M2 | OrgChart 未處理空 agents 的初始渲染 | `web/src/components/OrgChart.tsx:123-125` |
| FE-M3 | CreateAgentDialog 靜默吃掉錯誤 | `web/src/pages/AgentsPage.tsx:190-193` |
| FE-M4 | SkillsTab key 使用 `skill.name` 可能不唯一 | `web/src/pages/MemoryPage.tsx:202` |
| FE-M5 | Header 主題切換不監聽 prefers-color-scheme 變化 | `web/src/components/layout/Header.tsx:22-32` |
| FE-M6 | SystemStore 錯誤狀態被靜默吃掉 | `web/src/stores/system-store.ts:29,39` |
| FE-M7 | ChannelsPage toast setTimeout 未 cleanup | `web/src/pages/ChannelsPage.tsx:54-57` |
| FE-M8 | DashboardPage 健康摘要字串未使用 i18n | `web/src/pages/DashboardPage.tsx:76-77` |
| FE-M9 | Sidebar 版本號硬編碼 v0.1.0（實際 v0.6.5） | `web/src/components/layout/Sidebar.tsx:74` |

### LOW

| ID | Issue | File |
|----|-------|------|
| FE-L1 | en.json 缺少約 40 個翻譯 key | `web/src/i18n/en.json` |
| FE-L2 | AgentsPage 對話框 UI 文字未使用 i18n | `web/src/pages/AgentsPage.tsx:198-223` |
| FE-L3 | Dialog 缺少焦點陷阱和 aria-label | `web/src/components/shared/Dialog.tsx` |
| FE-L4 | OrgChart legend/zoom hint 為硬編碼英文 | `web/src/components/OrgChart.tsx:346-372` |
| FE-L5 | recharts 和 class-variance-authority 已安裝但未使用 | `web/package.json` |
| FE-L6 | vite.config.ts dev proxy target 硬編碼 | `web/vite.config.ts:16` |
| FE-L7 | memory.browse API 已定義但從未使用 | `web/src/lib/api.ts:186-188` |

---

## Part 2: Backend (gateway/core/agent/bus/memory/container)

### CRITICAL

#### [BE-C1] System Prompt 注入 — 用戶輸入直接拼接到命令行參數
**File**: `crates/duduclaw-gateway/src/channel_reply.rs:260`, `crates/duduclaw-gateway/src/claude_runner.rs:187`

用戶消息和 system prompt 通過 `--system-prompt` / `-p` 參數直接傳遞給 `claude` CLI。Skill 文件來自文件系統，若被竄改可注入任意命令。prompt 內容也可見於 `/proc/PID/cmdline`。

**Fix**: 使用 stdin 傳遞 prompt；對 system_prompt 設長度上限。

#### [BE-C2] `.tmp_system_prompt.md` 臨時文件競爭條件
**File**: `crates/duduclaw-gateway/src/channel_reply.rs:388-429`

固定路徑 `.tmp_system_prompt.md` 在并發請求下互相覆蓋，A 請求的 system prompt 被 B 的 agent 使用。清理操作 best-effort，失敗時靜默忽略。

**Fix**: 使用 UUID 生成唯一臨時文件名 + RAII guard 確保清理。

#### [BE-C3] `bus_queue.jsonl` 讀取-清空-寫入缺乏原子性
**File**: `crates/duduclaw-gateway/src/dispatcher.rs:100-111`

進程在覆寫完成前崩潰會導致 pending 消息永久丟失。`collect_business_context` 同時讀取同一檔案，可能讀到空檔案。

**Fix**: 使用 write → rename 原子替換；或改用 SQLite 持久化隊列。

#### [BE-C4] WebSocket 認證無超時 — Slowloris 攻擊面
**File**: `crates/duduclaw-gateway/src/server.rs:159-254`

`handle_socket` 等待認證消息時沒有超時。攻擊者可建立大量連接但不發送認證消息，永久佔用資源。

**Fix**: `tokio::time::timeout(Duration::from_secs(10), socket.recv())`

#### [BE-C5] Ed25519 challenge 無過期 + Mutex 中毒風險
**File**: `crates/duduclaw-gateway/src/auth.rs:17, 59`

`std::sync::Mutex` lock 失敗時 challenge bytes 未存儲但仍回傳給客戶端。Challenge 沒有過期時間。

### HIGH

| ID | Issue | File |
|----|-------|------|
| BE-H1 | SessionManager 單 Mutex 包裝同步 rusqlite，全局瓶頸 | `crates/duduclaw-gateway/src/session.rs:33-34` |
| BE-H2 | FTS5 查詢直接接受用戶輸入，error 可能洩露 schema | `crates/duduclaw-memory/src/engine.rs:160-192` |
| BE-H3 | `agents.create` API 無 agent 名稱路徑穿越防護 | `crates/duduclaw-gateway/src/handlers.rs:247-330` |
| BE-H4 | AccountRotator 每次請求重建，狀態每次丟棄，預算追蹤失效 | `crates/duduclaw-gateway/src/claude_runner.rs:64-116` |
| BE-H5 | Evolution engine 子進程無全局數量上限 | `crates/duduclaw-agent/src/heartbeat.rs:289-296` |
| BE-H6 | `dispatch_to_agent` 未對 prompt 做注入清理或長度限制 | `crates/duduclaw-gateway/src/dispatcher.rs:127` |
| BE-H7 | `agents.inspect` 回傳完整 SOUL.md 和 IDENTITY.md 內容 | `crates/duduclaw-gateway/src/handlers.rs:464` |

### MEDIUM

| ID | Issue | File |
|----|-------|------|
| BE-M1 | 多處 `which` 命令使用同步 `std::process::Command` 阻塞 Tokio | `channel_reply.rs:302`, `engine.rs:299`, `runner.rs:191` |
| BE-M2 | cron_scheduler 在持有 write lock 期間 spawning | `crates/duduclaw-gateway/src/cron_scheduler.rs:142-183` |
| BE-M3 | feedback.jsonl 不限上限累積 | `crates/duduclaw-gateway/src/external_factors.rs:210-228` |
| BE-M4 | `AccountRotator::reset_monthly` 中 `now` 變量未使用 | `crates/duduclaw-gateway/src/account_rotator.rs:278-285` |
| BE-M5 | BroadcastLayer 可能廣播敏感資訊到 WebSocket | `crates/duduclaw-gateway/src/log.rs:52-70` |
| BE-M6 | dispatcher append_line 和讀取 bus_queue 存在 TOCTOU | `crates/duduclaw-gateway/src/dispatcher.rs:266-283` |
| BE-M7 | SessionManager compress() 無 Transaction 保護 | `crates/duduclaw-gateway/src/session.rs:197-223` |
| BE-M8 | LINE HMAC 驗證需確認是否 constant-time | `crates/duduclaw-gateway/src/line.rs` |

### LOW

| ID | Issue | File |
|----|-------|------|
| BE-L1 | 多處重複 `which_claude()` 實作 | channel_reply.rs, claude_runner.rs, runner.rs, engine.rs |
| BE-L2 | HeartbeatStatus active_runs 計算可能 u32 溢位 | `crates/duduclaw-agent/src/heartbeat.rs:208` |
| BE-L3 | contract.rs 偽 regex 實作不可靠 | `crates/duduclaw-agent/src/contract.rs:125-139` |
| BE-L4 | 多個後台任務 JoinHandle 被丟棄 | `crates/duduclaw-gateway/src/server.rs:78-103` |
| BE-L5 | handle_connect_challenge 與 Ed25519 認證流程無關（遺留代碼） | `crates/duduclaw-gateway/src/handlers.rs:126-129` |

---

## Part 3: CLI (duduclaw-cli)

### CRITICAL

#### [CLI-C1] `handle_agent_status` 與 `handle_spawn_agent` 缺少路徑穿越驗證
**File**: `crates/duduclaw-cli/src/mcp.rs:1055, 1160`

`agent_id` 未驗證直接拼入路徑。`handle_send_to_agent` (L653) 有呼叫 `is_valid_agent_id()` 但這兩個 handler 遺漏了。

**Fix**: 統一在所有接受 agent_id 的函式開頭呼叫 `is_valid_agent_id()`。

#### [CLI-C2] `handle_send_message` 讀取明文 token 欄位（應為加密欄位）
**File**: `crates/duduclaw-cli/src/mcp.rs:393, 421, 445`

`onboard` 寫入 `telegram_bot_token_enc`，但 `handle_send_message` 讀取 `telegram_bot_token`（無 `_enc`）。所有通道的 `send_message` 操作一律失敗並回傳 "not configured"。

**Fix**: 實作 `decrypt_channel_token(config, key_name, home_dir)` 函式。

#### [CLI-C3] Keyfile 寫入失敗後仍繼續加密 — 資料永久無法解密
**File**: `crates/duduclaw-cli/src/main.rs:33-36`

若 keyfile 寫入失敗，加密仍繼續執行。下次啟動產生新金鑰，所有已加密資料永久損毀。

**Fix**: 寫入失敗時 `std::process::exit(1)`。

### HIGH

| ID | Issue | File |
|----|-------|------|
| CLI-H1 | service start/stop/status/logs 只印出指令字串，不執行 | `crates/duduclaw-cli/src/service/mod.rs:84-111` |
| CLI-H2 | `cmd_run_server` 顯示 `0.0.0.0` 但實際綁定 `127.0.0.1`；忽略 config.toml | `crates/duduclaw-cli/src/main.rs:713-728` |
| CLI-H3 | `handle_create_agent` 的 display_name 未轉義直接嵌入 TOML | `crates/duduclaw-cli/src/mcp.rs:907-953` |
| CLI-H4 | `handle_spawn_agent` JSONL 寫入跳過 10MB size limit | `crates/duduclaw-cli/src/mcp.rs:1195-1208` |
| CLI-H5 | `web_search` 工具可阻塞單執行緒 MCP server 30 秒 | `crates/duduclaw-cli/src/mcp.rs:1726-1730` |
| CLI-H6 | migrate.rs 的 skipped 計數器永遠為零 | `crates/duduclaw-cli/src/migrate.rs:37, 130-131` |
| CLI-H7 | tracing 日誌輸出到 stdout 污染 MCP JSON-RPC 協議 | `crates/duduclaw-cli/src/main.rs:212` |

### MEDIUM

| ID | Issue | File |
|----|-------|------|
| CLI-M1 | `cmd_agent_set_status` 缺少路徑穿越驗證 | `crates/duduclaw-cli/src/main.rs:1086-1090` |
| CLI-M2 | `count_pending_tasks` 讀取整個 bus_queue.jsonl 到記憶體 | `crates/duduclaw-cli/src/mcp.rs:1233` |
| CLI-M3 | onboard OAuth token 與 API key 走同一欄位名，語意混淆 | `crates/duduclaw-cli/src/main.rs:314-323` |
| CLI-M4 | keyfile 讀取邏輯重複三次未封裝 | `crates/duduclaw-cli/src/mcp.rs:1531-1554` |
| CLI-M5 | `handle_web_search` HTML 解析極度脆弱 | `crates/duduclaw-cli/src/mcp.rs:511-560` |
| CLI-M6 | onboard 未驗證 port 值範圍 | `crates/duduclaw-cli/src/main.rs:474-478` |
| CLI-M7 | service/mod.rs 頂部 `#![allow(dead_code)]` 遮蔽警告 | `crates/duduclaw-cli/src/service/mod.rs:1` |
| CLI-M8 | `ServiceCommands::Logs { lines }` 參數被靜默丟棄 | `crates/duduclaw-cli/src/main.rs:245` |

### LOW

| ID | Issue | File |
|----|-------|------|
| CLI-L1 | launchd 日誌路徑使用 `/tmp`（macOS 會定期清除） | `crates/duduclaw-cli/src/service/mod.rs:143-144` |
| CLI-L2 | mcp.rs 1891 行，遠超 800 行規範 | `crates/duduclaw-cli/src/mcp.rs` |
| CLI-L3 | main.rs 約 1303 行，遠超 800 行規範 | `crates/duduclaw-cli/src/main.rs` |
| CLI-L4 | `duduclaw_home()` 在 home dir 不存在時靜默回退到 `.` | `crates/duduclaw-cli/src/main.rs:204-208` |
| CLI-L5 | migrate.rs 中 `allow_bash: true` 寫死，不讀原始權限設定 | `crates/duduclaw-cli/src/migrate.rs:56-62` |

---

## Part 4: MCP Server (protocol/handlers)

### CRITICAL

#### [MCP-C1] `handle_agents_create` (handlers.rs) 缺少輸入驗證 — 路徑穿越
**File**: `crates/duduclaw-gateway/src/handlers.rs:246-330`

`name` 參數直接 `join` 進路徑，無格式驗證。`mcp.rs` 的版本有驗證但 `handlers.rs` 沒有。

#### [MCP-C2] TOML 注入 — display_name 未逸出
**File**: `crates/duduclaw-gateway/src/handlers.rs:271-273`, `crates/duduclaw-cli/src/mcp.rs:908-915`

`format!` 字串插值建立 agent.toml，惡意 display_name 可覆寫任意 TOML 欄位。

**Fix**: 使用 `toml` crate 序列化 API。

#### [MCP-C3] WsFrame 回應 `id` 全部硬寫為空字串
**File**: `crates/duduclaw-gateway/src/handlers.rs` (全檔)

所有 `WsFrame::ok_response("", ...)` 的 id 為空。客戶端無法將回應對應到原始請求，多並發請求下完全失效。

**Fix**: `MethodHandler::handle` 需接收 `request_id` 並傳遞。

### HIGH

| ID | Issue | File |
|----|-------|------|
| MCP-H1 | `handle_agents_delegate` 的 prompt 無長度限制 | `crates/duduclaw-gateway/src/handlers.rs:332-394` |
| MCP-H2 | `memory.search` 的 limit 參數未設上限 | `crates/duduclaw-gateway/src/handlers.rs:738, 773` |
| MCP-H3 | `handle_cron_add` 未驗證 cron 表達式格式 | `crates/duduclaw-gateway/src/handlers.rs:955-983` |
| MCP-H4 | `handle_agent_status` / `handle_spawn_agent` 缺少 agent_id 驗證 | `crates/duduclaw-cli/src/mcp.rs:1055, 1173` |
| MCP-H5 | `handle_system_config` TOML 解析失敗時洩露原始設定（含 token） | `crates/duduclaw-gateway/src/handlers.rs:1084-1087` |
| MCP-H6 | `odoo_execute` 允許呼叫任意 Odoo 方法（僅黑名單 model） | `crates/duduclaw-cli/src/mcp.rs:1481-1491` |

### MEDIUM

| ID | Issue | File |
|----|-------|------|
| MCP-M1 | web_search HTML 解析脆弱且可被偽造 | `crates/duduclaw-cli/src/mcp.rs:512-560` |
| MCP-M2 | handle_skill_search 每次呼叫重新載入 SkillRegistry | `crates/duduclaw-gateway/src/handlers.rs:870-895` |
| MCP-M3 | handle_agents_list 每次觸發完整目錄掃描（持寫鎖） | `crates/duduclaw-gateway/src/handlers.rs:181-227` |
| MCP-M4 | bus_queue.jsonl 多處寫入無共享 mutex | `mcp.rs:670-682`, `handlers.rs:374-386` |
| MCP-M5 | send_message 從設定讀明文 token 未走加密路徑 | `crates/duduclaw-cli/src/mcp.rs:394-469` |
| MCP-M6 | skill_loader 自製 YAML 解析器 + install_skill 無安全掃描 | `crates/duduclaw-agent/src/skill_loader.rs:113-225` |
| MCP-M7 | mask_sensitive_fields 洩露 token 前四碼 | `crates/duduclaw-gateway/src/handlers.rs:1311-1329` |

### LOW

| ID | Issue | File |
|----|-------|------|
| MCP-L1 | 兩個重複的 handle_create_agent 實作（mcp.rs vs handlers.rs） | - |
| MCP-L2 | WsFrame error_response id 用空字串而非 null | `crates/duduclaw-gateway/src/protocol.rs:54-61` |
| MCP-L3 | handle_skill_search 本地 skill 無重複性篩選 | `crates/duduclaw-gateway/src/handlers.rs:851-865` |
| MCP-L4 | SkillRegistry::load 使用同步 std::fs::read_to_string | `crates/duduclaw-agent/src/skill_registry.rs:112-120` |
| MCP-L5 | handle_agents_list 的 spent_cents 永遠為 0 | `crates/duduclaw-gateway/src/handlers.rs:206` |

---

## Part 5: Middleware (bridge/security/odoo/dashboard)

### CRITICAL

#### [MW-C1] Webhook 密鑰使用明文字串比對 — 時序攻擊 + 空 secret 跳過驗證
**File**: `crates/duduclaw-odoo/src/events.rs:170-177`

`provided != expected_secret` 非常數時間比較。`expected_secret` 為空時驗證被完全跳過。

**Fix**: 使用 `ring::constant_time::verify_slices_are_equal`；空 secret 時拒絕所有 webhook。

#### [MW-C2] bus_queue.jsonl 寫入缺乏任何驗證 — Python bridge 繞過所有安全層
**File**: `crates/duduclaw-bridge/src/lib.rs:18-27`

`send_to_bus` 的所有參數無來源驗證、channel 白名單、agent_id 授權檢查、或大小限制。任何 Python 程序都可寫入任意指令。

**Fix**: 增加 channel 白名單驗證和 payload 大小限制；考慮 Unix socket + peer credential。

#### [MW-C3] key_vault 從 config.toml 讀明文 token，未呼叫 CryptoEngine 解密
**File**: `crates/duduclaw-security/src/key_vault.rs:86-98`

`resolve_agent_keys` 直接讀取明文 `api.anthropic_api_key`，不走 `CryptoEngine::decrypt_string`。加密層形同虛設。

### HIGH

| ID | Issue | File |
|----|-------|------|
| MW-H1 | RBAC `allowed_channels` 不處理萬用字元 "*" | `crates/duduclaw-security/src/rbac.rs:74-80` |
| MW-H2 | 審計日誌多程序寫入有資料競爭 + read_recent_events 讀整個檔案 | `crates/duduclaw-security/src/audit.rs:54-78` |
| MW-H3 | RateLimiter 記憶體無限增長（key 永不清理） | `crates/duduclaw-security/src/rate_limiter.rs:1-95` |
| MW-H4 | SOUL.md 雜湊存同一目錄，攻擊者可同時竄改 | `crates/duduclaw-security/src/soul_guard.rs:43-55` |
| MW-H5 | OdooConnector 無重試邏輯，單一錯誤導致永久失敗 | `crates/duduclaw-odoo/src/connector.rs:51-83` |
| MW-H6 | input_guard.rs 零測試 + Unicode/多空格繞過向量 | `crates/duduclaw-security/src/input_guard.rs` |
| MW-H7 | Dashboard 靜態資源缺少安全 HTTP 標頭 | `crates/duduclaw-dashboard/src/lib.rs:33-49` |

### MEDIUM

| ID | Issue | File |
|----|-------|------|
| MW-M1 | PollTracker 結果上限固定 100，可能遺漏事件 | `crates/duduclaw-odoo/src/events.rs:66` |
| MW-M2 | classify_event 字串比對判斷新建，時間精度不一致 | `crates/duduclaw-odoo/src/events.rs:88-95` |
| MW-M3 | count_events_since 字串前綴比對做時間過濾 | `crates/duduclaw-security/src/audit.rs:117` |
| MW-M4 | get_duduclaw_home 在 HOME 未設定時靜默回退 | `crates/duduclaw-bridge/src/lib.rs:14` |
| MW-M5 | MountGuard 路徑不存在時降級為不安全（TOCTOU） | `crates/duduclaw-security/src/mount_guard.rs:92-99` |
| MW-M6 | OdooConnector.http 為公開欄位，可被重用 | `crates/duduclaw-odoo/src/connector.rs:29-35` |
| MW-M7 | rpc.rs 固定 JSON-RPC id=1 | `crates/duduclaw-odoo/src/rpc.rs:42` |
| MW-M8 | duduclaw-odoo 和 duduclaw-bridge 完全沒有測試 | - |

### LOW

| ID | Issue | File |
|----|-------|------|
| MW-L1 | `constant_time_compare` 標記 dead_code 未被使用 | `crates/duduclaw-security/src/credential_proxy.rs:100` |
| MW-L2 | audit read_recent_events 結果順序邏輯過度複雜 | `crates/duduclaw-security/src/audit.rs:88-97` |
| MW-L3 | OdooConfig webhook_secret 未加密（與其他欄位不一致） | `crates/duduclaw-odoo/src/config.rs:22` |
| MW-L4 | soul_guard 版本歷史剪枝每次重新整理目錄 | `crates/duduclaw-security/src/soul_guard.rs:165-186` |

---

## Priority Fix Roadmap

### Phase 1: Security Blockers (Must fix immediately)

1. **Path Traversal** — Unify `is_valid_agent_id()` check across all handlers (BE-H3, CLI-C1, MCP-C1)
2. **TOML Injection** — Replace all `format!` TOML generation with `toml::to_string` (MCP-C2, CLI-H3)
3. **Token Encryption Gap** — Ensure all token reads go through `CryptoEngine::decrypt_string` (CLI-C2, MW-C3)
4. **Keyfile Fatal** — Exit on keyfile write failure (CLI-C3)
5. **WebSocket Auth Timeout** — Add 10s timeout to auth handshake (BE-C4)
6. **Webhook Secret** — Use constant-time compare; reject empty secret (MW-C1)

### Phase 2: Functional Correctness (Before next release)

7. **WsFrame Request ID** — Pass request_id through handler chain (MCP-C3)
8. **Temp File Race** — UUID-based temp files for system prompt (BE-C2)
9. **Bus Queue Atomicity** — write → rename pattern or SQLite (BE-C3)
10. **Python Bridge Auth** — Channel whitelist + size limits for bus_queue writes (MW-C2)
11. **AccountRotator Singleton** — Share instance in AppState (BE-H4)
12. **Tracing to stderr** — Redirect tracing in MCP server (CLI-H7)

### Phase 3: Robustness & Performance (Next sprint)

13. **SessionManager Connection Pool** — Replace single Mutex with `r2d2-sqlite` (BE-H1)
14. **Dashboard Security Headers** — Add CSP, X-Frame-Options, etc. (MW-H7)
15. **Rate Limiter Cleanup** — LRU eviction for sliding windows (MW-H3)
16. **RBAC Wildcard** — Handle `"*"` consistently (MW-H1)
17. **Odoo Retry** — Add exponential backoff (MW-H5)
18. **Test Coverage** — input_guard, odoo, bridge (MW-M8, MW-H6)

---

*Generated by 5 parallel Claude Code review agents on 2026-03-24*

---
---

# Round 2: Verification & Deep Dive

**Review Date**: 2026-03-25
**Focus**: Verify Round 1 CRITICALs, find cross-cutting issues, uncover blind spots
**Reviewers**: 5 parallel agents (Frontend R2 / Backend R2 / CLI+MCP R2 / Security R2 / Cross-Layer Integration)

---

## Round 1 CRITICAL Verification Results

### Summary: 11 of 16 Round 1 CRITICALs Already Fixed

| R1 ID | Issue | R2 Verdict |
|-------|-------|------------|
| BE-C1 | System prompt CLI arg injection | **Reclassified** — `Command::args()` is shell-safe; real risk is semantic prompt injection (see R2-NEW-C1) |
| BE-C2 | `.tmp_system_prompt.md` race condition | **FIXED** — Now uses UUID suffix |
| BE-C3 | `bus_queue.jsonl` non-atomic write | **FIXED** — Now uses write-then-rename |
| BE-C4 | WebSocket auth no timeout | **FIXED** — 10s timeout added |
| BE-C5 | Ed25519 challenge no expiry | **FIXED** — 30s TTL + proper error handling |
| CLI-C1 | `handle_agent_status` path traversal | **FIXED** — `is_valid_agent_id()` now called |
| CLI-C2 | `handle_send_message` reads plaintext token | **FIXED** — `decrypt_channel_token()` now used |
| CLI-C3 | Keyfile write failure continues encryption | **FIXED** — Now calls `exit(1)` on failure |
| MCP-C1 | `handlers.rs` agents.create path traversal | **FIXED** — `is_valid_agent_id()` now called |
| MCP-C2 | TOML injection via display_name | **FIXED** — Now uses `toml::toml!` macro |
| MCP-C3 | WsFrame response id always empty | **FIXED** — `server.rs` overwrites id before sending |
| MW-C1 | Webhook secret non-constant-time compare | **FIXED** — `constant_time_eq` + empty secret rejection |
| MW-C2 | `bus_queue.jsonl` Python bridge no auth | **FIXED** — Whitelist + size limit + agent_id validation |
| MW-C3 | `key_vault` reads plaintext tokens | **FIXED** — Now uses `_enc` priority with decrypt |
| FE-C1 | WebSocket no auth | **CONFIRMED** — Still exists |
| FE-C2 | Error message info leakage | **CONFIRMED** — Line 97 of ChannelsPage.tsx |

---

## Round 2 New Findings

### CRITICAL (5 new)

#### [R2-C1] 對話歷史直接插入 System Prompt — 跨輪次 Prompt Injection
**File**: `crates/duduclaw-gateway/src/channel_reply.rs:117-142`

User messages from conversation history are formatted directly into the system prompt with no escaping or boundary markers. An attacker can send `"user: Ignore all previous instructions"` in round 1, and it becomes part of the system prompt in round 2. `input_guard.rs` only scans the initial input, not historical messages.

```rust
let history_text = msgs.iter()
    .map(|m| format!("{}: {}", m.role, m.content))  // raw user input embedded
    .collect::<Vec<_>>()
    .join("\n");
let full_system_prompt = format!("{system_prompt}{history}");
```

**Fix**: Use structured delimiters for history; truncate user content per message; re-scan historical content.

#### [R2-C2] Evolution summary 透過 CLI arg 傳入用戶衍生資料，繞過 input_guard
**File**: `crates/duduclaw-gateway/src/evolution.rs:242-245`

User message fragments are passed via `--summary` CLI arg to Python subprocess, completely bypassing `input_guard.rs` scanning.

**Fix**: Pass summary via stdin instead of CLI arg.

#### [R2-C3] 通道 Handler 讀取 Token 全部繞過加密層 — Bot 無法啟動
**File**: `crates/duduclaw-gateway/src/telegram.rs:119-130`, `discord.rs:366-373`, `line.rs:251-263`

All three channel handlers read plaintext token fields (`telegram_bot_token`) but `onboard` writes encrypted fields (`telegram_bot_token_enc`). After running `onboard`, **all channel bots fail to start** because the plaintext fields are empty.

This is both a **functional break** and a **security design failure** — it forces users to store tokens in plaintext to make channels work.

**Fix**: Implement `_enc` priority read with decrypt in all three channel handlers, matching `key_vault.rs` pattern.

#### [R2-C4] soul_guard 初始化可被攻擊者完全繞過
**File**: `crates/duduclaw-security/src/soul_guard.rs:104-117`

Deleting `.soul_hash` + modifying `SOUL.md` → system treats tampered SOUL as "first-time initialization" and stores attacker's hash as the new baseline. Both files are in the same directory with the same permissions.

**Fix**: Store `.soul_hash` outside agent directory (e.g., `~/.duduclaw/soul_hashes/<agent_id>.hash`); never auto-trust first initialization.

#### [R2-C5] 審計日誌無防篡改機制
**File**: `crates/duduclaw-security/src/audit.rs:54-78`

`security_audit.jsonl` has no HMAC chain, no append-only OS flag, no access control. Any process can truncate and forge the audit log.

**Fix**: Add HMAC chain (each entry includes prev_hash + HMAC signature); or use OS-level append-only flag.

### HIGH (17 new)

| ID | Layer | Issue | File |
|----|-------|-------|------|
| R2-H1 | Frontend | agents-store/system-store WebSocket subscriptions never unsubscribed | `web/src/stores/agents-store.ts:18-27` |
| R2-H2 | Frontend | logs-store ring buffer uses `splice` mutation | `web/src/stores/logs-store.ts:33-36` |
| R2-H3 | Frontend | AddAccountDialog fake success — data not persisted | `web/src/pages/AccountsPage.tsx:177-192` |
| R2-H4 | Backend | cron_scheduler `sem.acquire()` Result silently ignored | `crates/duduclaw-gateway/src/cron_scheduler.rs:177-179` |
| R2-H5 | Backend | `write_cron_tasks` silently ignores I/O errors | `crates/duduclaw-gateway/src/handlers.rs:978` |
| R2-H6 | Backend | BroadcastLayer leaks user message fragments (first 80 chars) to all WS log subscribers | `crates/duduclaw-gateway/src/log.rs:51-70` |
| R2-H7 | Backend | No graceful shutdown — all in-flight requests force-killed on SIGTERM | `crates/duduclaw-gateway/src/server.rs:136-139` |
| R2-H8 | Backend | `handle_memory_browse` no limit cap (vs `memory.search` has `.min(200)`) | `crates/duduclaw-gateway/src/handlers.rs:804` |
| R2-H9 | CLI | `handle_channels_add` writes plaintext token (not `_enc`) | `crates/duduclaw-gateway/src/handlers.rs:589` |
| R2-H10 | CLI | `cmd_onboard` agent.toml uses `format!` string interpolation (TOML injection) | `crates/duduclaw-cli/src/main.rs:598-645` |
| R2-H11 | CLI | `handle_send_media` reads plaintext token, doesn't use `decrypt_channel_token()` | `crates/duduclaw-cli/src/mcp.rs:699-703` |
| R2-H12 | Security | `.keyfile` master key stored as plaintext 32 bytes on disk | `crates/duduclaw-gateway/src/claude_runner.rs:142-154` |
| R2-H13 | Security | `docker.rs` additional_mounts bypass MountGuard entirely | `crates/duduclaw-container/src/docker.rs:37-42` |
| R2-H14 | Security | Apple Container runtime has zero security isolation | `crates/duduclaw-container/src/apple.rs:68-74` |
| R2-H15 | Security | New agents default to `allowed_channels = ["*"]` (all channels) | `crates/duduclaw-gateway/src/handlers.rs:330-331` |
| R2-H16 | Security | WSL2 mount paths bypass MountGuard | `crates/duduclaw-container/src/wsl2.rs:104-117` |
| R2-H17 | Cross-Layer | `is_valid_agent_id` has 5 inconsistent implementations | See consistency table below |

### MEDIUM (14 new)

| ID | Issue | File |
|----|-------|------|
| R2-M1 | Frontend | SkillMarketPage external URL validation basic only | `web/src/pages/SkillMarketPage.tsx:123` |
| R2-M2 | Frontend | Header connectionError may leak internal URL | `web/src/components/layout/Header.tsx:57` |
| R2-M3 | Frontend | vite.config.ts missing explicit `sourcemap: false` | `web/vite.config.ts` |
| R2-M4 | Frontend | DashboardPage polling + WS events race condition (stale state overwrite) | `web/src/pages/DashboardPage.tsx:57-62` |
| R2-M5 | Backend | `read_cron_tasks`/`write_cron_tasks` are sync blocking in async handler | `crates/duduclaw-gateway/src/handlers.rs:961-979` |
| R2-M6 | Backend | `audit.rs` sync I/O in async context | `crates/duduclaw-security/src/audit.rs:65-78` |
| R2-M7 | Backend | `mask_sensitive_fields` shows first 4 chars (leaks token type) | `crates/duduclaw-gateway/src/handlers.rs:1353-1360` |
| R2-M8 | CLI | `handle_channels_test` reads plaintext token, always shows "not configured" | `crates/duduclaw-gateway/src/handlers.rs:618-625` |
| R2-M9 | CLI | MCP notification handling based on method name, not `id` presence | `crates/duduclaw-cli/src/mcp.rs:1825-1829` |
| R2-M10 | CLI | `is_valid_agent_id` in mcp.rs less strict than handlers.rs version | `crates/duduclaw-cli/src/mcp.rs:296-300` |
| R2-M11 | Security | CryptoEngine nonce has no counter protection (collision risk at high volume) | `crates/duduclaw-security/src/crypto.rs:42` |
| R2-M12 | Security | `constant_time_compare` in credential_proxy.rs marked dead_code, never used | `crates/duduclaw-security/src/credential_proxy.rs:99-110` |
| R2-M13 | Security | input_guard bypassable via Unicode homoglyphs, multi-space, zero-width chars | `crates/duduclaw-security/src/input_guard.rs` |
| R2-M14 | Cross-Layer | Channel status detection only checks plaintext fields | `crates/duduclaw-gateway/src/handlers.rs:526-530` |

### LOW (6 new)

| ID | Issue | File |
|----|-------|------|
| R2-L1 | Frontend | CreateAgentDialog error silently swallowed | `web/src/pages/AgentsPage.tsx:190-193` |
| R2-L2 | Backend | `auth.rs:60` uses `expect()` on RNG | `crates/duduclaw-gateway/src/auth.rs:60` |
| R2-L3 | Backend | `reqwest::Client::build().unwrap_or_default()` loses timeout config | `crates/duduclaw-gateway/src/channel_reply.rs:41` |
| R2-L4 | Security | RBAC role hierarchy not enforced (Specialist can create Main agent) | `crates/duduclaw-security/src/rbac.rs:89-138` |
| R2-L5 | Cross-Layer | `bus_queue.jsonl` message_id format inconsistent (UUID vs timestamp) | dispatcher.rs vs bridge/lib.rs |
| R2-L6 | Cross-Layer | `reqwest` not in workspace deps, declared separately in 4 crates | Cargo.toml |

---

## `is_valid_agent_id` Consistency Table

| Location | Allows `_` | Forbids leading/trailing `-` | Checks `..` |
|----------|-----------|------------------------------|-------------|
| `duduclaw-cli/src/main.rs:1087` | No | Yes | Yes |
| `duduclaw-cli/src/mcp.rs:296` | No | **No** | **No** |
| `duduclaw-gateway/src/handlers.rs:15` | No | Yes | Yes |
| `duduclaw-bridge/src/lib.rs:25` | No | Yes (leading only) | **No** |
| `duduclaw-agent/src/ipc.rs:67` | **Yes** | **No** | **No** |

**Fix**: Extract to `duduclaw-core::validate::is_valid_agent_id()` with the strictest ruleset.

---

## Encryption Layer Gap Analysis

The `_enc` encryption design is implemented in `onboard` and `key_vault`, but **7 read paths bypass it**:

| Reader | Reads `_enc`? | Status |
|--------|---------------|--------|
| `key_vault.rs` (resolve_agent_keys) | Yes | OK |
| `mcp.rs` (handle_send_message) | Yes | OK |
| `claude_runner.rs` (get_api_key) | Yes | OK |
| `telegram.rs` (read_telegram_token) | **No** | **BROKEN** |
| `discord.rs` (read_discord_token) | **No** | **BROKEN** |
| `line.rs` (read_line_config) | **No** | **BROKEN** |
| `channel_reply.rs` (get_api_key) | **No** | **BROKEN** |
| `runner.rs` (get_api_key) | **No** | **BROKEN** |
| `handlers.rs` (has_api_key) | **No** | **BROKEN** |
| `handlers.rs` (channels.add) | **Writes plaintext** | **BROKEN** |
| `handlers.rs` (channels.test) | **No** | **BROKEN** |
| `mcp.rs` (handle_send_media) | **No** | **BROKEN** |

**Fix**: Create `duduclaw-security::config_reader::read_encrypted_field(config, key, home_dir)` and use it everywhere.

---

## Revised Priority Fix Roadmap

### Phase 0: Functional Breaks (Immediate — channels don't work)

1. **[R2-C3] Channel token encryption gap** — telegram.rs, discord.rs, line.rs, channel_reply.rs, runner.rs, handlers.rs all need `_enc` decryption. Create shared `read_encrypted_field()` utility.
2. **[R2-H9/H11] channels.add + send_media** — Write encrypted tokens; read with decrypt.

### Phase 1: Active Security Vulnerabilities

3. **[R2-C1] Prompt injection via conversation history** — Add structured delimiters, re-scan history, truncate per-message.
4. **[R2-C2] Evolution summary injection** — Pass via stdin not CLI arg.
5. **[R2-C4] soul_guard bypass** — Store hashes outside agent directory; don't auto-trust initialization.
6. **[R2-C5] Audit log tampering** — HMAC chain or append-only flag.
7. **[R2-H12] Keyfile plaintext storage** — Use OS keychain or at minimum `chmod 0600`.
8. **[R2-H13/H16] MountGuard bypass** — Call `MountGuard::validate_mount()` in docker.rs and wsl2.rs.

### Phase 2: Consistency & Robustness

9. **[R2-H17] Unify `is_valid_agent_id`** — Single implementation in `duduclaw-core`.
10. **[R2-H7] Graceful shutdown** — `axum::serve().with_graceful_shutdown()`.
11. **[R2-H15] Default permissions** — `allowed_channels = []` not `["*"]`.
12. **[R2-H6] Log data isolation** — Filter user message content from BroadcastLayer.
13. **[FE-C1] WebSocket authentication** — Require token for dashboard access.

### Phase 3: Quality & Polish

14. Frontend subscription cleanup, error handling, i18n gaps.
15. Sync I/O in async context (`session.rs`, `cron tasks`, `audit.rs`).
16. `input_guard.rs` Unicode normalization + test coverage.

---

*Round 1: 2026-03-24 — 5 parallel agents, initial discovery*
*Round 2: 2026-03-25 — 5 parallel agents, verification + deep dive*

---
---

# Round 3: Verification, Attack Chains & Quantitative Analysis

**Review Date**: 2026-03-25
**Focus**: Verify R2 CRITICALs, validate R1 fixes completeness, attack chain analysis, test coverage metrics
**Reviewers**: 5 parallel agents (R2 CRITICAL Verification / Fix Completeness / Attack Chains / Metrics / Frontend Deep Dive)

---

## R2 CRITICAL Verification Results

### Key Correction: R2 Encryption Gap Table Was Largely Wrong

R2 claimed 8 read paths and 1 write path bypass encryption. **R3 found 5 of 9 claims were FALSE** — a `config_crypto.rs` module exists that handles `_enc` field priority with decrypt fallback.

| # | Path | R2 Claim | R3 Verdict | Evidence |
|---|------|----------|------------|----------|
| 1 | `telegram.rs read_telegram_token` | Reads plaintext | **FALSE** | Calls `config_crypto::read_encrypted_config_field` |
| 2 | `discord.rs read_discord_token` | Reads plaintext | **FALSE** | Calls `config_crypto::read_encrypted_config_field` |
| 3 | `line.rs read_line_config` | Reads plaintext | **FALSE** | Calls `config_crypto::read_encrypted_config_field` |
| 4 | `channel_reply.rs get_api_key` | Reads plaintext | **FALSE** | Calls `config_crypto::read_encrypted_config_field` |
| 5 | `runner.rs get_api_key` | Reads plaintext | **TRUE** | Direct `table.get("api").and_then(\|v\| v.get("anthropic_api_key"))` |
| 6 | `handlers.rs has_api_key` | Checks plaintext | **TRUE** | Direct `api.get("anthropic_api_key")` |
| 7 | `handlers.rs channels.add` | Writes plaintext | **TRUE** | `channels.insert(token_key, toml::Value::String(token))` |
| 8 | `handlers.rs channels.test` | Reads plaintext | **TRUE** | `ch.get(token_key)` reads plaintext only |
| 9 | `mcp.rs handle_send_media` | Reads plaintext | **FALSE** | Calls `decrypt_channel_token()` |

**R2-C3 downgraded from CRITICAL to HIGH** — scope reduced to 4 paths (items 5-8), not 9.

### R2-C1 Prompt Injection via History: CONFIRMED

`channel_reply.rs:117-142` — conversation history formatted directly into system prompt with zero sanitization. `input_guard::scan_input()` is **never called anywhere in duduclaw-gateway** (grep confirms zero imports). This is the most dangerous confirmed vulnerability.

### R2-C2 Evolution Summary Injection: CONFIRMED

`evolution.rs:242-245` — user message first 200 chars passed via `--summary` CLI arg to Python subprocess. Entire path bypasses `input_guard`.

### R2-C4 soul_guard Bypass: PARTIALLY CONFIRMED (Downgraded to MEDIUM)

Hash storage has been improved — now in `~/.duduclaw/soul_hashes/<agent_name>.hash` (separate from agent dir). But first-run trust issue remains: `intact: true` returned unconditionally on first check.

### R2-C5 Audit Log Tampering: CONFIRMED

`audit.rs:54-78` — pure `O_APPEND` with no HMAC, no chain hash, no integrity verification on read.

---

## R1 Fix Completeness Validation

| Fix | Verdict | Issue |
|-----|---------|-------|
| BE-C2 tmp_system_prompt UUID | **PARTIAL** | Error path (`?` return) skips cleanup; no RAII guard; sensitive content may persist on disk |
| BE-C3 bus_queue atomic write | **PARTIAL** | No startup cleanup of orphaned `.jsonl.tmp` files from pre-crash state |
| BE-C4 WebSocket auth timeout | **COMPLETE** | Both auth rounds covered with independent 10s timeouts |
| BE-C5 Ed25519 challenge TTL | **COMPLETE** | 30s TTL, `.take()` consumes challenge, constant-time compare |
| CLI-C1 agent_id validation | **PARTIAL** | `handle_schedule_task`, `handle_submit_feedback`, `handle_log_mood` still skip validation |
| CLI-C3 keyfile write exit | **COMPLETE** | `exit(1)` on failure, `chmod 0600` on unix |
| MW-C1 webhook constant-time | **COMPLETE** | XOR+OR accumulate, empty secret rejected |
| MW-C2 Python bridge validation | **COMPLETE** | Channel whitelist, 1MB size limit, agent_id validation |
| MCP-C2 TOML generation | **NEEDS INVESTIGATION** | `toml::toml!` macro may not support runtime variable interpolation — could be a functional bug where all agents get literal "name" instead of actual name |

---

## Attack Chain Analysis

### Chain 1: Unauthenticated WS → Full System Compromise (CRITICAL)

```
FE-C1 (no WS auth) → tools.catalog (discover methods) → system.config (leak all tokens)
  → agents.create (default allowed_channels=["*"]) → agents.delegate (arbitrary prompt)
    → Claude CLI Bash tool → read .keyfile → decrypt all tokens
      → cron.add (persistence) → audit.jsonl truncation (cover tracks)
```
**Break point**: Force `auth_token` to be required at startup.

### Chain 2: Prompt Injection → Agent Escape (CRITICAL)

```
R2-C1 (history injection) → bypass InputGuard (never called in gateway)
  → system prompt override → Claude CLI Bash tool
    → read other agents' memories (direct SQLite access)
    → modify SOUL.md + update soul_hash simultaneously
    → create sub-agents with elevated permissions
    → inject additional_mounts for container escape (R2-H13)
```
**Break point**: Scan conversation history with `input_guard` before system prompt assembly; use XML fencing.

### Chain 3: Rogue Agent → Lateral Movement (HIGH)

```
Compromised agent → write to bus_queue.jsonl (no source verification in dispatcher)
  → trigger arbitrary prompts on any agent → RBAC not enforced in dispatch path
    → read .keyfile + all agent dirs (same UID, no OS isolation)
    → tamper audit.jsonl → modify other agents' SOUL.md + soul_hash
```
**Break point**: Add source verification in `dispatcher.rs`; unify with `IpcBroker.send_verified()`.

### Chain 4: Supply Chain via Skill Market (HIGH)

```
Malicious GitHub repo indexed → skill_search returns it → install_skill (no security scan)
  → skill .md content injected into system prompt (build_system_prompt trusts all skills)
    → prompt injection via skill content → SOUL.md modification via evolution engine
    → self-replication to other agents' SKILLS/ directories
```
**Break point**: Scan skill content with `input_guard` at install time; implement quarantine.

### Core Architectural Issue

> Security boundaries are designed at the WS RPC layer, but the actual execution path (Claude CLI with Bash tool) completely bypasses all controls. RBAC, InputGuard, and SOUL Guard are semantic-layer controls, but Claude CLI has full host shell access. Any prompt reaching `dispatch_to_agent()` can trigger unrestricted system operations.

---

## Quantitative Analysis

### Test Coverage

| Crate | Source Files | Lines | Test Functions | Coverage |
|-------|-------------|-------|----------------|----------|
| duduclaw-core | 4 | 353 | **0** | 0% |
| duduclaw-gateway | 17 | 5,216 | 9 | ~5% |
| duduclaw-agent | 11 | 2,445 | **0** | 0% |
| duduclaw-bus | 3 | 196 | **0** | 0% |
| duduclaw-memory | 4 | 677 | 12 | ~20% |
| duduclaw-container | 6 | 890 | **0** | 0% |
| duduclaw-security | 10 | 1,579 | 13 | ~15% |
| duduclaw-bridge | 1 | 117 | **0** | 0% |
| duduclaw-odoo | 12 | 960 | **0** | 0% |
| duduclaw-cli | 4 | 3,691 | **0** | 0% |
| duduclaw-dashboard | 1 | 70 | **0** | 0% |
| **Rust Total** | **73** | **16,194** | **34** | **<5%** |
| **Frontend Total** | **26** | **4,118** | **0** | **0%** |

**Project requirement: 80% coverage. Actual: <5%. Gap: 75+ percentage points.**

### Zero-Test Security Modules (CRITICAL)

| Module | Lines | Function | Risk |
|--------|-------|----------|------|
| `soul_guard.rs` | 236 | SOUL.md drift detection | Any regression breaks integrity protection |
| `input_guard.rs` | 182 | Prompt injection scanning | Any regression opens injection vector |
| `rate_limiter.rs` | 134 | Rate limiting | Any regression enables DoS |
| `key_vault.rs` | 113 | AES-256-GCM key management | Any regression exposes all credentials |
| `audit.rs` | 184 | Security audit logging | Any regression loses audit trail |

### Files Exceeding 800-Line Limit

| File | Lines | Over by |
|------|-------|---------|
| `mcp.rs` | 1,910 | +139% |
| `handlers.rs` | 1,391 | +74% |
| `main.rs` | 1,326 | +66% |

### Dependency Audit

- **Total transitive dependencies**: 346 crates
- **`cargo audit` configured**: No (not installed, not in CI)
- **Multi-version duplicates**: `hashbrown` (4 versions), `getrandom` (3 versions), `windows-sys` (3 versions)
- **`reqwest`** declared inline in 4 crates instead of workspace
- **`unsafe` blocks**: 0 (positive)
- **`#[allow(dead_code)]`** in security module: `credential_proxy.rs:100`

---

## Frontend Round 3 Findings

### HIGH (7 new)

| ID | Issue | File |
|----|-------|------|
| R3-FE-H1 | WS `requestId` is predictable sequential integer (spoofable) | `web/src/lib/ws-client.ts:23,148` |
| R3-FE-H2 | LogsPage renders 5000 DOM nodes without virtualization (`@tanstack/react-virtual` installed but unused) | `web/src/pages/LogsPage.tsx:156` |
| R3-FE-H3 | All 10 pages eagerly imported — no code splitting (d3 loaded on every route) | `web/src/App.tsx:2-13` |
| R3-FE-H4 | Pause/Resume buttons have no loading guard — double-click sends duplicate requests | `web/src/pages/AgentsPage.tsx:113-127` |
| R3-FE-H5 | AddChannelDialog silently swallows errors | `web/src/pages/ChannelsPage.tsx:238-242` |
| R3-FE-H6 | All icon-only buttons missing `aria-label` (only 1 in entire codebase) | Multiple files |
| R3-FE-H7 | Health status uses color-only indicators — fails WCAG for color blindness | `web/src/pages/DashboardPage.tsx:132-134` |

### MEDIUM (6 new)

| ID | Issue | File |
|----|-------|------|
| R3-FE-M1 | WS reconnect doesn't re-subscribe to `logs.subscribe` | `web/src/stores/logs-store.ts:27-38` |
| R3-FE-M2 | WS disconnect doesn't rollback optimistic state / refresh on reconnect | `web/src/stores/connection-store.ts` |
| R3-FE-M3 | No incoming WS message size limit — oversized JSON blocks main thread | `web/src/lib/ws-client.ts:66-73` |
| R3-FE-M4 | Channel tokens transmitted over unencrypted `ws://` in dev | `web/src/stores/connection-store.ts:24` |
| R3-FE-M5 | Dialog missing `aria-labelledby` linking to title | `web/src/components/shared/Dialog.tsx:35-53` |
| R3-FE-M6 | Theme toggle button uses `title` instead of `aria-label` | `web/src/components/layout/Header.tsx:85-91` |

---

## Final Revised Priority Roadmap

### P0: Architectural Security (This Week)

1. **Force WS authentication** — `auth_token = None` must reject startup, not skip auth
2. **Scan conversation history** — Call `input_guard::scan_input()` on each history message before system prompt assembly; use `<history><turn role="user">...</turn></history>` XML fencing
3. **Scan skill content at install** — `install_skill()` must call `scan_input()` on full skill content
4. **Bus queue source verification** — `dispatcher.rs` must verify message origin; unify with `IpcBroker`

### P1: Security Hardening (This Sprint)

5. **Add `cargo audit` to CI** — 346 unscanned dependencies
6. **Test security modules** — `input_guard.rs`, `soul_guard.rs`, `key_vault.rs`, `rate_limiter.rs` need >80% coverage
7. **Fix `handlers.rs channels.add`** — Encrypt token before writing to config.toml
8. **Fix `runner.rs get_api_key`** — Use `config_crypto::read_encrypted_config_field`
9. **MountGuard enforcement** — Call `validate_mount()` in docker.rs and wsl2.rs

### P2: Robustness (Next Sprint)

10. **Split oversized files** — `mcp.rs` (1910L), `handlers.rs` (1391L), `main.rs` (1326L)
11. **Unify `is_valid_agent_id`** — Extract to `duduclaw-core`, eliminate 5 inconsistent copies
12. **Default `allowed_channels = []`** — Not `["*"]`
13. **Graceful shutdown** — `axum::serve().with_graceful_shutdown()`
14. **Frontend code splitting** — `React.lazy()` for OrgChartPage (d3), SkillMarketPage
15. **LogsPage virtualization** — Use already-installed `@tanstack/react-virtual`

### P3: Quality (Ongoing)

16. Frontend: `aria-label` audit, color-blind indicators, error handling
17. Frontend: WS reconnect data refresh, logs re-subscribe
18. Backend: tmp file RAII cleanup, `.jsonl.tmp` startup recovery
19. Dependency: `reqwest` to workspace deps, reduce multi-version duplicates

---

*Round 1: 2026-03-24 — 5 agents, initial discovery (16 CRITICAL found)*
*Round 2: 2026-03-25 — 5 agents, verification (11 R1 CRITICALs fixed, 5 new found)*
*Round 3: 2026-03-25 — 5 agents, deep verification + attack chains + metrics*

## Confirmed Active Vulnerabilities After 3 Rounds

| # | ID | Severity | Issue | Status |
|---|-----|----------|-------|--------|
| 1 | FE-C1 | CRITICAL | WebSocket no authentication when `auth_token = None` | Open |
| 2 | R2-C1 | CRITICAL | Conversation history prompt injection (input_guard never called in gateway) | Open |
| 3 | R2-C2 | CRITICAL | Evolution summary passes user content via CLI arg, bypasses input_guard | Open |
| 4 | R2-C5 | MEDIUM | Audit log has no tamper protection | Open |
| 5 | R2-H9 | HIGH | `channels.add` writes plaintext token | Open |
| 6 | R2-H10 | HIGH | `cmd_onboard` agent.toml uses `format!` (TOML injection) | Open |
| 7 | R2-H12 | HIGH | `.keyfile` master key stored as plaintext | Open |
| 8 | R2-H13/H16 | HIGH | Docker/WSL2 `additional_mounts` bypass MountGuard | Open |
| 9 | R2-H15 | HIGH | New agents default `allowed_channels = ["*"]` | Open |
| 10 | — | CRITICAL | Zero test coverage on 5 security-critical modules | Open |
| 11 | — | CRITICAL | No `cargo audit` — 346 unscanned dependencies | Open |

---
---

# Round 4: Precision Verification & Blind Spot Discovery

**Review Date**: 2026-03-25
**Focus**: Verify R3 corrections, audit config_crypto.rs, confirm toml macro behavior, deep dive top 2 vulns, find 3-round blind spots
**Reviewers**: 3 parallel agents (config_crypto + toml / Top 2 Deep Dive / Blind Spot Hunter)

---

## R4 Key Findings

### [R4-CRITICAL-1] `toml::toml!` Macro Confirmed as Functional Bug

**Files**: `handlers.rs:293`, `mcp.rs:896`

`toml::toml!` in `toml 0.8` does NOT support runtime variable interpolation. Variables like `name`, `display_name`, `role` in value position are parsed as TOML string literals `"name"`, `"display_name"`, `"role"` — not the Rust variable values.

**Impact**: ALL agents created via WebSocket dashboard or MCP `create_agent` tool get incorrect field values. Only `duduclaw onboard` and `duduclaw agent create` CLI commands (using `format!`) work correctly.

| Location | Method | Runtime vars? |
|----------|--------|--------------|
| `handlers.rs` handle_agents_create | `toml::toml!` | **NO — writes literal "name"** |
| `mcp.rs` handle_create_agent | `toml::toml!` | **NO — writes literal "name"** |
| `main.rs` cmd_onboard | `format!()` | Yes — correct |
| `main.rs` cmd_agent_create | `format!()` | Yes — correct |

**Fix**: Replace `toml::toml!` with `toml::Value` programmatic construction or `format!()`.

### [R4-CRITICAL-2] Python LINE Webhook: Signature Verification Never Called

**File**: `python/duduclaw/channels/line.py:106`

`verify_signature()` exists but is **never called** anywhere in the LINE webhook handling path. Anyone can forge LINE webhook events and inject arbitrary messages into the system. Additionally, `hmac.new()` parameter types may be incorrect depending on Python version.

### [R4-CRITICAL-3] `auth_token` Hardcoded to `None` — Never Reads Config or Env

**File**: `crates/duduclaw-cli/src/main.rs:739`

```rust
auth_token: None,   // <-- ALWAYS None, never reads config.toml or DUDUCLAW_AUTH_TOKEN env var
```

This means FE-C1 is not just "auth is optional" — **auth is impossible**. There is no way for users to enable authentication even if they want to. All 39 WS RPC methods are permanently unauthenticated.

### [R4-CRITICAL-4] `input_guard::scan_input` Only Called in Red-Team Test CLI

**File**: `crates/duduclaw-cli/src/main.rs:1245` (only call site)

`scan_input` is called ONLY in `cmd_test_agent()` — the `duduclaw test` CLI command for red-team testing. It is **never called in actual Telegram/LINE/Discord message processing**. The entire prompt injection defense system exists but is not wired into the production message flow.

### [R4-HIGH-1] `config_crypto.rs` — Decrypt-Only Design (No Encrypt Function)

**File**: `crates/duduclaw-gateway/src/config_crypto.rs`

The module only implements `read_encrypted_config_field` (decrypt+read). There is NO corresponding `write_encrypted_config_field` (encrypt+write). This architectural asymmetry means:
- Dashboard `channels.add` cannot encrypt tokens (no encrypt function available)
- Only `duduclaw onboard` CLI can write encrypted tokens
- Any runtime config modification writes plaintext

### [R4-HIGH-2] `config_crypto.rs` — Silent Decrypt Failure Fallback

When `_enc` field exists but decryption fails (corrupted keyfile, key rotation), the function silently falls back to reading the plaintext field with **zero logging**. If an attacker clears `.keyfile`, the system degrades to plaintext without any alert.

### [R4-HIGH-3] Python `await on_error` TypeError Crashes Account Rotation

**File**: `python/duduclaw/sdk/chat.py:194`

`await rotator.on_error(account, error)` calls a sync method with `await`, causing `TypeError` in Python. Account rotation logic crashes on the first account failure instead of falling back to the next account.

### [R4-HIGH-4] `MessageRouter` Trigger Uses Substring Match on agent_id

**File**: `crates/duduclaw-bus/src/router.rs:61`

```rust
if message.text.contains(agent_id.as_str())
```

Short agent IDs like `"ai"`, `"bot"`, `"dev"` will be triggered by almost any message. Attackers knowing agent names can force routing to specific agents.

### [R4-HIGH-5] CI Pipeline Silently Skips All Python Tests

**File**: `.github/workflows/ci.yml:70-75`

`tests/python/` directory is empty. CI checks if tests exist, outputs "No Python tests found, skipping" and exits 0 (green). All Python bugs (LINE hmac, await TypeError, evolution injection) are never caught by CI.

### [R4-MEDIUM] Python Discord Channel is Send-Only (No Message Receiving)

**File**: `python/duduclaw/channels/discord.py`

`DiscordChannel` implements `send_message()` but has no WebSocket Gateway connection for receiving messages. Despite CLAUDE.md describing "Gateway WebSocket with tokio::select! heartbeat", the Python implementation is incomplete — Discord is unidirectional.

---

## Final Confirmed Vulnerability Table (After 4 Rounds)

| # | ID | Severity | Issue | Root Cause | Fix Effort |
|---|-----|----------|-------|------------|------------|
| 1 | FE-C1 | **CRITICAL** | WS auth permanently disabled | `main.rs:739` hardcodes `auth_token: None` | 30 min |
| 2 | R2-C1 | **CRITICAL** | Prompt injection via conversation history | `input_guard` never called in gateway message flow | 2-3 hours |
| 3 | R4-C1 | **CRITICAL** | `toml::toml!` writes literal strings not variable values | Macro doesn't support runtime interpolation | 1 hour |
| 4 | R4-C2 | **CRITICAL** | LINE webhook signature verification never called | `verify_signature()` exists but unused | 30 min |
| 5 | R4-C4 | **CRITICAL** | `scan_input` only in test CLI, not production | Defence system built but not connected | 1 hour |
| 6 | R3-Metrics | **CRITICAL** | <5% test coverage (target: 80%) | Empty test dirs, security modules untested | Ongoing |
| 7 | R3-Metrics | **CRITICAL** | No `cargo audit` in CI | 346 unscanned transitive deps | 15 min |
| 8 | R2-H9 | HIGH | `channels.add` writes plaintext token | `config_crypto` has no encrypt function | 2 hours |
| 9 | R4-H2 | HIGH | Decrypt failure silent fallback, no logging | `config_crypto` returns None silently | 30 min |
| 10 | R4-H3 | HIGH | `await on_error` TypeError crashes rotation | Sync method called with await | 15 min |
| 11 | R4-H4 | HIGH | Router substring match on short agent IDs | `contains()` instead of word boundary | 1 hour |
| 12 | R2-H12 | HIGH | `.keyfile` master key plaintext on disk | No OS keychain integration | 4 hours |
| 13 | R2-H13/H16 | HIGH | Docker/WSL2 mounts bypass MountGuard | `validate_mount()` never called | 1 hour |
| 14 | R2-H15 | HIGH | New agents default `allowed_channels=["*"]` | Permissive default in handlers.rs | 15 min |

---

## Definitive Fix Roadmap

### Week 1: Stop the Bleeding (7 CRITICAL)

| Priority | Fix | File | Effort |
|----------|-----|------|--------|
| P0-1 | Read `auth_token` from env/config; refuse `0.0.0.0` bind without token | `main.rs:736-741` | 30 min |
| P0-2 | Call `scan_input()` on incoming messages in `build_reply_with_session` | `channel_reply.rs:108` | 30 min |
| P0-3 | XML-fence conversation history; scan historical user messages | `channel_reply.rs:117-142` | 2 hours |
| P0-4 | Replace `toml::toml!` with `toml::Value` construction | `handlers.rs:293`, `mcp.rs:896` | 1 hour |
| P0-5 | Call `verify_signature()` in LINE webhook handler | `line.py` webhook path | 30 min |
| P0-6 | Add `cargo audit` to CI | `.github/workflows/ci.yml` | 15 min |
| P0-7 | Fix `await on_error` TypeError | `chat.py:194` | 15 min |

### Week 2: Harden (8 HIGH)

| Priority | Fix | Effort |
|----------|-----|--------|
| P1-1 | Add `encrypt_config_field()` to config_crypto.rs; use in channels.add | 2 hours |
| P1-2 | Add decrypt failure logging in config_crypto.rs | 30 min |
| P1-3 | Fix router substring match → word boundary or use trigger field | 1 hour |
| P1-4 | Call MountGuard in docker.rs, wsl2.rs, apple.rs | 1 hour |
| P1-5 | Default `allowed_channels = []` | 15 min |
| P1-6 | Fix runner.rs get_api_key to try `_enc` field | 30 min |
| P1-7 | Pass evolution summary via stdin, not CLI arg | 1 hour |
| P1-8 | Make CI fail when test directories are empty | 15 min |

### Week 3-4: Test Coverage Sprint

| Priority | Scope | Target |
|----------|-------|--------|
| P2-1 | `input_guard.rs` — injection patterns, Unicode bypass, edge cases | 80% |
| P2-2 | `soul_guard.rs` — drift detection, first-run, hash storage | 80% |
| P2-3 | `key_vault.rs` — encrypt/decrypt roundtrip, missing keyfile | 80% |
| P2-4 | `rate_limiter.rs` — sliding window, cleanup, edge cases | 80% |
| P2-5 | `config_crypto.rs` — enc priority, fallback, missing fields | 80% |
| P2-6 | Frontend: vitest setup, store tests, ws-client tests | 50% |

---

*Round 1-4 history preserved above*

---
---

# Round 5: Final Verdict & Actionable Fixes

**Review Date**: 2026-03-25
**Focus**: Definitive verdict on all CRITICALs, complete Python audit, generate copy-paste fix patches
**Reviewers**: 3 parallel agents (Final Verdict / Python Full Audit / Fix Code Generator)

---

## Final Verdict: 7 Claimed CRITICALs → 2 Disputed, 5 Confirmed

| # | Issue | R4 Claim | **R5 Final Verdict** | Confidence | Reason |
|---|-------|----------|---------------------|------------|--------|
| C1 | `auth_token` hardcoded `None` | CRITICAL | **CONFIRMED** | HIGH | `main.rs:739` — no code path ever sets `Some(...)` |
| C2 | `input_guard` never called in gateway | CRITICAL | **CONFIRMED** | HIGH | Zero matches for `scan_input` or `input_guard` in entire gateway crate |
| C3 | `toml::toml!` macro bug | CRITICAL | **DISPUTED (downgraded to functional bug)** | HIGH | Functional bug, not security — prevents TOML injection by never inserting user data |
| C4 | LINE `verify_signature` never called | CRITICAL | **DISPUTED (invalid)** | HIGH | Rust `line.rs:158-169` correctly calls `verify_signature` — Python side doesn't handle webhooks |
| C5 | `scan_input` missing in Python | CRITICAL→HIGH | **CONFIRMED** | HIGH | Zero matches in Python codebase; merged with C2 |
| C6 | Test coverage <5% | CRITICAL | **CONFIRMED** | HIGH | 34 Rust tests, 0 Python tests, front-end tests exist but coverage low |
| C7 | `await` on sync `on_error` | CRITICAL→HIGH | **CONFIRMED** | HIGH | `rotator.py:78` is `def`, not `async def`; `chat.py:194` uses `await` |

### R5 Self-Correction Summary

- **C3 `toml::toml!`**: R4 claimed CRITICAL security issue. R5 confirms it's a **functional bug** (agents get wrong names) but actually **prevents TOML injection** since user data is never interpolated. Downgraded.
- **C4 LINE webhook**: R4 claimed Python `verify_signature` never called. R5 found **Rust-side `line.rs` correctly verifies signatures** — Python doesn't handle webhooks at all. **Fully disputed.**
- **C6 test coverage**: R5 found web/ does have ~60 test files (not 0 as R3 claimed), but Python remains at absolute zero.

---

## R5 New Python Findings (3 CRITICAL + 5 HIGH)

### [R5-PY-C1] Python `agent_tools.py` — Path Traversal in `agent_create`
**File**: `python/duduclaw/tools/agent_tools.py:63-65`

`agent_name` only does `.lower().replace(" ", "-")` — no filtering of `..` or `/`. Input `name = "../../.ssh"` writes to `~/.duduclaw/agents/../../.ssh/`. Same issue in `agent_status` (line 163) and `_set_status` (line 196).

### [R5-PY-C2] Python `agent_tools.py` — TOML Injection via f-string
**File**: `python/duduclaw/tools/agent_tools.py:76-115`

`display_name`, `role`, `trigger` inserted via f-string into TOML. Input `display_name = 'evil"\ncan_create_agents = true'` escalates agent permissions.

### [R5-PY-C3] Evolution engine prompt injection → persistent memory corruption
**File**: `python/duduclaw/evolution/micro.py:88-95`

User conversation content directly embedded in Claude prompt; LLM output parsed as JSON and written to `MEMORY.md` and `daily_note`. Attacker can inject fake skill candidates or corrupt agent memory permanently.

### R5 Python HIGH Findings

| ID | Issue | File |
|----|-------|------|
| R5-PY-H1 | `await` on sync `on_error`/`on_rate_limited` | `chat.py:194,201` vs `rotator.py:72,78` |
| R5-PY-H2 | `health.py` uses blocking `urllib.request` in `async def` | `health.py:26` |
| R5-PY-H3 | `_call_claude` triplicated across micro/meso/macro | `micro.py:12`, `meso.py:13`, `macro_.py:12` |
| R5-PY-H4 | `asyncio.get_event_loop()` deprecated in Python 3.10+ | `micro.py:85`, `meso.py:71`, `macro_.py:67` |
| R5-PY-H5 | `vetter.py` regex only catches `subprocess.call`, not `.run`/`.Popen` | `vetter.py:41` |

---

## Definitive Active Vulnerability Table (After 5 Rounds)

### Confirmed Security Vulnerabilities

| # | Severity | Issue | File | Fix Effort |
|---|----------|-------|------|------------|
| 1 | **CRITICAL** | WS `auth_token` hardcoded `None` — 39 RPC methods permanently unauthenticated | `main.rs:739` | 30 min |
| 2 | **CRITICAL** | `input_guard::scan_input` never called in production message flow | `channel_reply.rs` (entire gateway crate) | 1 hour |
| 3 | **CRITICAL** | Python `agent_create` path traversal — write to arbitrary filesystem path | `agent_tools.py:63-65` | 30 min |
| 4 | **CRITICAL** | Python `agent_create` TOML injection — escalate agent permissions | `agent_tools.py:76-115` | 30 min |
| 5 | **CRITICAL** | Evolution engine prompt injection → persistent memory corruption | `micro.py:88-95` | 2 hours |

### Confirmed Functional Bugs (Non-Security)

| # | Severity | Issue | File |
|---|----------|-------|------|
| 6 | HIGH (func) | `toml::toml!` writes literal "name" not variable values | `handlers.rs:293`, `mcp.rs:896` |
| 7 | HIGH (func) | `await` on sync `on_error` crashes account rotation | `chat.py:194` |
| 8 | HIGH (func) | `health.py` blocking I/O freezes event loop | `health.py:26` |

### Confirmed Hardening Gaps

| # | Severity | Issue | File |
|---|----------|-------|------|
| 9 | HIGH | `channels.add` writes plaintext token (no encrypt function exists) | `handlers.rs:589` |
| 10 | HIGH | Docker/WSL2 `additional_mounts` bypass MountGuard | `docker.rs:37`, `wsl2.rs:104` |
| 11 | HIGH | New agents default `allowed_channels=["*"]` | `handlers.rs:330` |
| 12 | HIGH | `config_crypto.rs` silent decrypt failure (no logging) | `config_crypto.rs:47-55` |
| 13 | HIGH | Router `contains()` substring match on short agent IDs | `router.rs:61` |
| 14 | HIGH | CI silently skips all Python tests (empty test dir) | `ci.yml:70-75` |
| 15 | CRITICAL | No `cargo audit` — 346 unscanned dependencies | `ci.yml` |
| 16 | CRITICAL | <5% test coverage; 5 security modules have 0 tests | Project-wide |

---

## Fix Patches Included

Round 5 generated **7 complete, copy-paste-ready code patches** for the highest-priority fixes:

1. **Fix 1**: `main.rs` — Read `auth_token` from `DUDUCLAW_AUTH_TOKEN` env / `config.toml [gateway]`; warn on non-loopback bind without token
2. **Fix 2**: `channel_reply.rs` — Call `scan_input()` before `append_message()`; return rejection message if blocked
3. **Fix 3**: `channel_reply.rs` — XML-fence conversation history with `<conversation_history>/<user>/<assistant>` tags, HTML-escape user content, 500-char truncation
4. **Fix 4**: `handlers.rs` — Replace `toml::toml!` with programmatic `toml::value::Table` construction
5. **Fix 5**: `line.py` — Add `handle_webhook_request()` method that calls `verify_signature()` before event processing
6. **Fix 6**: `ci.yml` — Add `cargo install cargo-audit && cargo audit` step before build
7. **Fix 7**: `chat.py` — Remove `await` from sync `on_error()` calls, or make `on_error` async

*Full patch code is available in the Round 5 agent output transcripts.*

---

## Five-Round Review Statistics

| Metric | Value |
|--------|-------|
| Total review rounds | 5 |
| Total agents deployed | 21 |
| Rust files reviewed | 73 |
| TypeScript files reviewed | 26 |
| Python files reviewed | 15+ |
| Total lines of code reviewed | ~20,000+ |
| Confirmed CRITICAL vulnerabilities | 5 security + 2 functional |
| Confirmed HIGH issues | 11 |
| Round-over-round self-corrections | 6 major (C3 downgraded, C4 disputed, encryption gap 5/9 false positive, LINE webhook verified in Rust, test count corrected, toml macro reclassified) |

---

*Rounds 1-5 history preserved above*

---
---

# Round 6: Fix Patch Audit & Red Team Validation

**Review Date**: 2026-03-25
**Focus**: Security review of the 7 proposed fix patches; red team attack simulation post-fix
**Reviewers**: 2 parallel agents (Fix Patch Auditor / Red Team Attacker)

---

## Fix Patch Audit Results

| Fix | Verdict | Critical Issue Found |
|-----|---------|---------------------|
| Fix 1: auth_token from env | **NEEDS CHANGES** | Non-loopback without token should **block startup**, not just warn. Add min 32-char token length. |
| Fix 2: scan_input before append | **NEEDS CHANGES** | Multi-message chunked injection bypasses per-message scanning. Document as known limitation; consider session-level cumulative risk score. |
| Fix 3: XML-fence history | **NEEDS CHANGES** | **Assistant messages NOT escaped** — compromised assistant response can inject `</conversation_history><system>` tags. Must escape ALL roles. Also: truncate BEFORE escape to avoid cutting `&lt;` back to `<`. |
| Fix 4: toml::Value construction | **APPROVED** | Safe. `toml::Value::String` handles escaping. Add `display_name` length limit (128 chars). `allowed_channels=["*"]` still present — track separately. |
| Fix 5: Python LINE verify_signature | **REJECTED** | Rust `line.rs:158-169` already verifies signatures correctly. Python side doesn't handle webhooks. Adding Python code increases attack surface with zero benefit. |
| Fix 6: cargo audit in CI | **NEEDS CHANGES** | Use `--deny unsound --deny yanked` (not `--deny warnings`). Use `taiki-e/install-action` for pre-built binary instead of compiling. |
| Fix 7: Remove await on sync on_error | **APPROVED** | Remove `await` (simplest). Reject the `hasattr(__await__)` defensive check — too complex, masks design issues. |

### Fix 3 Is the Most Dangerous If Applied Incorrectly

The proposed Fix 3 escapes `<user>` content but leaves `<assistant>` content **unescaped**. This creates a NEW vulnerability:

1. Attacker sends injection via prompt injection in round N
2. Claude's response (which may contain injected content) is stored as `assistant` role
3. Next round, the assistant message enters the XML fence **without escaping**
4. Injected `</conversation_history>` breaks out of the fence structure

**Fix 3 must escape ALL roles equally**, not just `user`.

---

## Red Team Post-Fix Residual Risk Assessment

| Attack Scenario | Rating | Key Finding |
|----------------|--------|-------------|
| Auth Bypass | **PARTIALLY MITIGATED** | Token in plaintext config.toml readable by agents; no brute-force protection; SSRF from local services |
| Prompt Injection | **STILL VULNERABLE** | `scan_input` rules too sparse (narrative wrapping, Unicode homoglyphs, multi-turn accumulation all bypass); **compression path is a persistent injection pipeline** — transcript fed to Claude for summary, summary stored permanently |
| Skill Market | **STILL VULNERABLE** | `skill_loader.rs install_skill` never calls `scan_input`; `skill_security_scan=true` config flag is a dead setting; skill name path traversal via `../../` |
| Lateral Movement | **STILL VULNERABLE** | Sandbox is opt-in (most agents run unsandboxed); `bus_queue.jsonl` has no source verification; `agents.delegate` prompt not scanned |
| Data Exfiltration | **PARTIALLY MITIGATED** | `memory.browse` has no agent authorization (any agent_id accepted); no `limit` cap; `logs.subscribe` may leak user message fragments |

### Most Critical Post-Fix Gap: Compression Path Injection

Even with Fix 2 (scan_input) and Fix 3 (XML fence), the **compression path** (`channel_reply.rs:186-205`) remains completely unprotected:

```
User sends injection → stored in session → session grows →
compress() reads all messages → formats as transcript →
sends to Claude (haiku) for summarization →
summary stored permanently →
future system prompts include the poisoned summary
```

No scanning, no fencing, no validation of the summary output. This is a **persistent prompt injection pipeline** that survives session compression.

### Second Most Critical: `agents.delegate` Prompt Not Scanned

Authenticated users can call `agents.delegate` with arbitrary prompts that go directly to Claude CLI without any `scan_input` call. This means the entire input scanning defense can be bypassed by using the WS RPC directly instead of going through a channel.

---

## Revised Fix Recommendations (Incorporating R6 Findings)

### Fix 1 (auth_token) — Updated

```rust
// BLOCK startup if non-loopback without token
if !is_loopback && auth_token.is_none() {
    eprintln!("FATAL: auth_token required for non-loopback bind. Set DUDUCLAW_AUTH_TOKEN.");
    std::process::exit(1);
}
// Enforce minimum token length
if let Some(ref t) = auth_token {
    if t.len() < 32 {
        eprintln!("FATAL: auth_token must be >= 32 characters.");
        std::process::exit(1);
    }
}
```

### Fix 3 (XML fence) — Updated

```rust
// MUST escape ALL roles, not just user
let content = {
    let truncated = m.content.chars().take(500).collect::<String>();
    html_escape(&truncated)  // Escape for ALL roles including assistant
};
```

### NEW Fix 8: Scan compression transcript

```rust
// In channel_reply.rs compress path (~line 186-205):
// Before sending transcript to Claude for summarization,
// scan each user message in the transcript
let safe_transcript = msgs.iter()
    .map(|m| {
        if m.role == "user" {
            let scan = scan_input(&m.content, DEFAULT_BLOCK_THRESHOLD);
            if scan.blocked { "[blocked content]".to_string() }
            else { m.content.clone() }
        } else { m.content.clone() }
    })
    .collect();
// Also validate summary output before storing
```

### NEW Fix 9: Scan agents.delegate prompt

```rust
// In handlers.rs handle_agents_delegate:
let scan = scan_input(&prompt, DEFAULT_BLOCK_THRESHOLD);
if scan.blocked {
    return WsFrame::error_response(id, "Prompt contains blocked content");
}
```

### NEW Fix 10: Scan skill content at install time

```rust
// In skill_loader.rs install_skill:
let scan = scan_input(&content, DEFAULT_BLOCK_THRESHOLD);
if scan.blocked {
    return Err(format!("Skill content blocked by security scan: {}", scan.summary));
}
// Also validate skill name: no path traversal
if name.contains("..") || name.contains('/') || name.contains('\\') {
    return Err("Invalid skill name".to_string());
}
```

---

*Rounds 1-6 history preserved above*

---
---

# Round 7: Dynamic Functional Verification

**Review Date**: 2026-03-25
**Focus**: Actually compile, test, and runtime-verify disputed claims
**Reviewers**: 2 parallel agents (Rust build+test+toml macro / Frontend build + Python verify + await test)

---

## Build Verification Results

### Rust: ALL CRATES COMPILE SUCCESSFULLY

| Crate | Build | Tests | Test Count |
|-------|-------|-------|------------|
| duduclaw-core | PASS | 0 tests | 0 |
| duduclaw-security | PASS | ALL PASS | 31 |
| duduclaw-memory | PASS | ALL PASS | 16 |
| duduclaw-gateway | PASS | ALL PASS | 9 |
| duduclaw-agent | PASS | 0 tests | 0 |
| duduclaw-bus | PASS | 0 tests | 0 |
| duduclaw-container | PASS | 0 tests | 0 |
| duduclaw-odoo | PASS | 0 tests | 0 |
| duduclaw-cli | PASS | 0 tests | 0 |
| duduclaw-dashboard | PASS | 0 tests | 0 |
| **Total** | **ALL PASS** | **56/56 PASS** | **56** |

### Frontend: BUILD SUCCESS (after 3 TS fixes)

Original build had 3 TypeScript errors:
1. `AccountsPage.tsx:166` — `error` state declared but never rendered (TS6133)
2. `LogsPage.tsx:158` — invalid type cast `LogEntry as Record<string, unknown>` (TS2352)
3. `MemoryPage.tsx:204` — same cast issue with `SkillInfo` (TS2352)

All 3 fixed. Final build: **465.70 kB JS + 38.58 kB CSS, built in 703ms**.

### Python: ALL 12 FILES SYNTAX OK

All core Python files pass `py_compile`. Package imports successfully (`duduclaw v0.1.0`).

---

## CRITICAL DISPUTE RESOLUTION

### `toml::toml!` Macro: VARIABLES ARE INTERPOLATED CORRECTLY

**This resolves a 3-round dispute (R3 raised concern → R4 CRITICAL → R5 DISPUTED → R7 DEFINITIVE).**

Actual runtime test with `toml 0.8`:

```rust
let name = "my-actual-agent";
let display_name = "My Agent Display";
let role = "specialist";

let config = toml::toml! {
    [agent]
    name = name
    display_name = display_name
    role = role
};
```

**Output:**
```toml
[agent]
display_name = "My Agent Display"
name = "my-actual-agent"
role = "specialist"
```

**Variables ARE correctly interpolated.** The `toml::toml!` macro in `toml 0.8` DOES support runtime variable references in value positions. R4's CRITICAL claim and R5's DISPUTED status were both wrong — the macro works correctly.

**Impact**: R4-C1 (`toml::toml!` functional bug) is **FULLY INVALID**. Agent creation via WS/MCP works correctly. This removes 1 claimed CRITICAL from the list.

### `await` on Sync Method TypeError: CONFIRMED

```python
class Rotator:
    def on_error(self, msg):
        return None

async def test():
    await Rotator().on_error("test")
# → TypeError: object NoneType can't be used in 'await' expression
```

**Confirmed at runtime.** `chat.py:194`'s `await rotator.on_error(...)` will crash if `on_error` is sync.

---

## cargo audit Results

**1 known vulnerability found:**

| Advisory | Crate | Issue | Fix |
|----------|-------|-------|-----|
| RUSTSEC-2025-0020 | `pyo3 v0.23.5` | Buffer overflow in `PyString::from_object` | Upgrade to `pyo3 >= 0.24.1` |

Only affects `duduclaw-bridge` (Python FFI crate). Low immediate risk since bridge is excluded from default build, but should be upgraded.

---

## Clippy Analysis: 46 Warnings, 0 Errors

| Category | Count | Severity |
|----------|-------|----------|
| `collapsible_if` (style) | 31 | Low |
| `should_implement_trait` | 2 | Medium |
| `ptr_arg` (`&PathBuf` → `&Path`) | 2 | Low |
| `too_many_arguments` (9 params) | 1 | Medium |
| Other style warnings | 10 | Low |

No blocking issues. The `too_many_arguments` warning on `rpc.rs:105` (`execute_kw` with 9 params) is worth refactoring.

---

## Final Corrected Vulnerability Table (After 7 Rounds)

### Confirmed Security Vulnerabilities (4, down from 5)

| # | Severity | Issue | File | Verified By |
|---|----------|-------|------|-------------|
| 1 | **CRITICAL** | WS `auth_token` hardcoded `None` — 39 RPC methods permanently unauthenticated | `main.rs:739` | R5 final verdict + R7 code confirmed |
| 2 | **CRITICAL** | `input_guard::scan_input` never called in production message flow | Gateway crate (zero imports) | R5 grep confirmed |
| 3 | **CRITICAL** | Python `agent_tools.py` path traversal + TOML injection | `agent_tools.py:63,76` | R5 Python audit |
| 4 | **CRITICAL** | Evolution engine prompt injection → persistent memory corruption | `micro.py:88` | R5 Python audit |

### ~~Removed: `toml::toml!` Functional Bug~~ — R7 DISPROVED

R7 runtime test proves `toml::toml!` in `toml 0.8` correctly interpolates runtime variables. This was a false positive across R4-R6.

### Confirmed Functional Bugs (2)

| # | Severity | Issue | Verified By |
|---|----------|-------|-------------|
| 1 | HIGH | `await` on sync `on_error` crashes account rotation | R7 runtime test confirmed TypeError |
| 2 | HIGH | 3 TypeScript errors prevent frontend build | R7 build test (fixed by agent) |

### Confirmed Hardening Gaps

| # | Severity | Issue |
|---|----------|-------|
| 1 | HIGH | `channels.add` writes plaintext token (no encrypt function) |
| 2 | HIGH | Docker/WSL2 `additional_mounts` bypass MountGuard |
| 3 | HIGH | New agents default `allowed_channels=["*"]` |
| 4 | HIGH | `config_crypto.rs` silent decrypt failure |
| 5 | HIGH | Router `contains()` substring match on short agent IDs |
| 6 | HIGH | CI silently skips all Python tests |
| 7 | CRITICAL | No `cargo audit` in CI — `pyo3` has known CVE (RUSTSEC-2025-0020) |
| 8 | CRITICAL | <5% test coverage; 5 security modules + all Python have 0 tests |

---

## Complete 7-Round Statistics

| Metric | Value |
|--------|-------|
| Total review rounds | **7** |
| Total agents deployed | **25** |
| Rust files reviewed | 73 |
| TypeScript files reviewed | 26 |
| Python files reviewed | 15+ |
| Total lines of code reviewed | ~20,000+ |
| Confirmed security CRITICALs | **4** (was 5, R7 disproved toml macro) |
| Confirmed functional bugs | **2** (await TypeError + TS build errors) |
| Confirmed HIGH issues | **14** |
| Known CVEs | **1** (pyo3 RUSTSEC-2025-0020) |
| Existing tests | **56 Rust** (all pass), **0 frontend**, **0 Python** |
| Clippy warnings | 46 (0 errors) |
| Fix patches: approved | 2 (Fix 4 toml::Value, Fix 7 await) |
| Fix patches: need changes | 4 (Fix 1 auth, Fix 2 scan, Fix 3 XML fence, Fix 6 cargo audit) |
| Fix patches: rejected | 2 (Fix 5 Python LINE — not needed; ~~Fix 4~~ toml macro — not needed, macro works correctly) |
| Round-over-round self-corrections | **8** major |

### Complete Self-Correction History

| Round | Correction |
|-------|-----------|
| R1→R2 | 16 CRITICALs → 11 already fixed |
| R2→R3 | Encryption gap table 5/9 false positive (`config_crypto.rs` exists) |
| R3→R4 | Python layer completely missed by first 3 rounds |
| R4→R5 | `toml::toml!` downgraded from security CRITICAL to functional bug |
| R4→R5 | LINE webhook — Rust side already verifies correctly |
| R3→R5 | Frontend test count corrected |
| R5→R6 | Fix 3 (XML fence) would introduce NEW vulnerability if assistant messages not escaped |
| **R4→R7** | **`toml::toml!` macro FULLY DISPROVED — works correctly, not a bug at all** |

---

*Rounds 1-7 history preserved above*

---
---

# Round 8: Fix Verification & Change Inventory

**Review Date**: 2026-03-25
**Focus**: Verify all code changes made during 7 rounds of review; ensure build integrity
**Reviewers**: 2 parallel agents (Frontend Fix Quality / Complete Change Inventory)

---

## Change Inventory

The 7 rounds of review resulted in **67 modified files** + **2 new files** (`CODE-REVIEW.md`, `config_crypto.rs`), totaling approximately **+2,076 / -811 lines**.

### Build Status After All Changes

| Component | Status |
|-----------|--------|
| `cargo build` (all crates excl. bridge) | **PASS** |
| `cargo test` (67 tests) | **67/67 PASS** (was 56 in R7 — 11 new tests added) |
| `npm run build` (frontend) | **PASS** |
| `npx tsc --noEmit` | **0 errors** |

### Confirmed Good Fixes (Verified Correct)

| Fix | Assessment |
|-----|-----------|
| `input_guard.rs` +13 tests | Correct, all pass |
| `audit.rs` flock() file locking | Correct, `#[cfg(unix)]` guarded |
| `audit.rs` DateTime parsing for time comparison | Correct |
| `soul_guard.rs` hash moved to `~/.duduclaw/soul_hashes/` | Correct direction (see caveat below) |
| `rbac.rs` wildcard `*` channel support | Correct |
| `dispatcher.rs` write+rename atomic pattern | Correct |
| `session.rs` compress() uses transaction | Correct |
| `auth.rs` challenge TTL (30s) | Correct |
| `server.rs` WS auth timeout (10s) | Correct |
| `log.rs` `scrub_sensitive()` filter | Correct |
| `line.rs` constant-time HMAC comparison | Correct |
| `odoo/events.rs` forced webhook secret verification | Correct |
| `config_crypto.rs` centralized encrypt/decrypt | Good design |
| `duduclaw-core` shared `which_claude()` | Correct, eliminates 4 duplicates |
| Python `agent_tools.py` TOML injection fix | Correct |
| `dashboard.rs` security HTTP headers | Correct |
| `Header.tsx` system dark mode listener | Correct |
| `SecurityPage.tsx` static data warning banner | Correct |
| `i18n/index.ts` Zustand store for locale | Correct |

---

## New Issues Found in Existing Fixes

### [R8-CRITICAL] `--system-prompt-file` Flag Does Not Exist in Claude CLI

**Files**: `channel_reply.rs:284`, `claude_runner.rs:180`

Both files write the system prompt to a temp file and pass it via `--system-prompt-file <path>`. **This flag does not exist in the Claude CLI** (only `--system-prompt` and `--append-system-prompt` exist). This causes ALL AI responses to fail when system prompt is non-empty — the `claude` subprocess exits with a non-zero code.

```rust
// CURRENT (broken)
cmd.args(["--system-prompt-file", &path.to_string_lossy()]);

// FIX: revert to --system-prompt or use stdin
cmd.args(["--system-prompt", system_prompt]);
```

### [R8-HIGH] Session pool creates 4 concurrent SQLite connections (unsafe)

**File**: `session.rs:43-62`

New connection pool creates 4 `rusqlite::Connection` instances to the same SQLite database. Even with WAL mode, concurrent writes from the same process can cause contention. The original single `Mutex<Connection>` was a bottleneck but was **correct** for SQLite. The pool introduces undefined behavior with `unchecked_transaction()`.

### [R8-HIGH] `get_agent_spent` ignores agent_name parameter

**File**: `handlers.rs:791-795`

`_agent_name` parameter prefixed with `_` (unused). Returns total spend across ALL accounts, making every agent show the same (incorrect) budget consumption.

### [R8-HIGH] `soul_guard.rs` `hash_path()` fallback defeats security purpose

**File**: `soul_guard.rs:50-63`

`.parent().parent()` path inference may fail if `agent_dir` isn't at expected depth. Fallback places hash in same directory as SOUL.md, defeating the separation goal.

### [R8-MEDIUM] MemoryPage.tsx double-cast is unnecessary

**File**: `web/src/pages/MemoryPage.tsx:204`

`(skill as unknown as Record<string, unknown>).agent_id` — but `agent_id` is already an optional field on `SkillInfo` interface. Should be simply `skill.agent_id`.

### [R8-MEDIUM] LogsPage.tsx double-cast should be a type fix

**File**: `web/src/pages/LogsPage.tsx:159`

`(entry as unknown as Record<string, unknown>)._id` — should add `_id?: number` to `LogEntry` interface instead of double-casting.

---

## Final Statistics (8 Rounds Complete)

| Metric | Value |
|--------|-------|
| Total review rounds | **8** |
| Total agents deployed | **27** |
| Files modified during review | 67 + 2 new |
| Lines changed | +2,076 / -811 |
| Rust tests | **67/67 pass** (56 original + 11 new) |
| Frontend build | **PASS** (after 3 TS fixes) |
| Python syntax | **12/12 pass** |
| Known CVEs | 1 (pyo3 RUSTSEC-2025-0020) |
| Confirmed security CRITICALs | **4** + 1 new from R8 |
| Round-over-round self-corrections | **9** major |

### Updated Self-Correction History

| Round | Correction |
|-------|-----------|
| R1→R2 | 16 CRITICALs → 11 already fixed |
| R2→R3 | Encryption gap table 5/9 false positive |
| R3→R4 | Python layer missed by first 3 rounds |
| R4→R5 | `toml::toml!` downgraded to functional bug |
| R4→R5 | LINE webhook — Rust side already verifies |
| R3→R5 | Frontend test count corrected |
| R5→R6 | Fix 3 XML fence would introduce new vuln if assistant not escaped |
| R4→R7 | `toml::toml!` FULLY DISPROVED — works correctly |
| **R7→R8** | **`--system-prompt-file` flag doesn't exist — breaks all AI responses** |

---

*Round 1: 2026-03-24 — 5 agents, initial broad discovery*
*Round 2: 2026-03-25 — 5 agents, verification + deep dive*
*Round 3: 2026-03-25 — 5 agents, attack chains + quantitative metrics*
*Round 4: 2026-03-25 — 3 agents, precision verification + blind spots*
*Round 5: 2026-03-25 — 3 agents, final verdict + Python audit + fix patches*
*Round 6: 2026-03-25 — 2 agents, fix patch audit + red team validation*
*Round 7: 2026-03-25 — 2 agents, dynamic build/test/runtime verification*
*Round 8: 2026-03-25 — 2 agents, fix verification + complete change inventory*
