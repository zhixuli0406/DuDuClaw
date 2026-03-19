# DuDuClaw — 已知問題與待修復清單

> 最後更新：2026-03-16
> 版本：v0.4.0
> 來源：深度 code review + 全專案 TODO 掃描

---

## 嚴重程度定義

| 符號 | 等級 | 說明 |
|------|------|------|
| 🔴 | CRITICAL | 功能完全無法使用，影響核心流程 |
| 🟠 | HIGH | 功能部分損壞或回傳誤導性資料 |
| 🟡 | MEDIUM | 功能有但品質差、有 bug 或邏輯錯誤 |
| 🔵 | LOW | 技術債、程式碼品質問題 |

---

## 🔴 CRITICAL — 必須優先修復

### C-1｜PyO3 橋接是空殼

- **檔案**：`crates/duduclaw-bridge/src/lib.rs:12`
- **函式**：`send_message()`
- **問題**：唯一公開函式只回傳 TODO 格式字串，Python 與 Rust 完全無法互通
- **影響**：Python 層的 channels 插件、evolution engine、agent tools 全部無法與 Rust core 通訊
- **標記**：`// TODO: implement in Phase 1`

```rust
// 現況（假實作）
fn send_message(agent_id: &str, payload: &str) -> PyResult<String> {
    Ok(format!("TODO: send message to agent '{}' with payload '{}'", ...))
}
```

---

### C-2｜CLI 大量子命令是 `println!("TODO")`

- **檔案**：`crates/duduclaw-cli/src/main.rs:155–194`
- **問題**：以下命令執行後只印出 TODO 字串，完全無功能

| 行號 | 命令 |
|------|------|
| 155 | `duduclaw agent create <name>` |
| 160 | `duduclaw agent pause <agent>` |
| 164 | `duduclaw agent resume <agent>` |
| 170 | `duduclaw gateway` |
| 178 | `duduclaw service install` |
| 182 | `duduclaw service start` |
| 185 | `duduclaw service stop` |
| 188 | `duduclaw service status` |
| 191 | `duduclaw service logs` |
| 194 | `duduclaw service uninstall` |

---

### C-3｜`agents.pause` / `agents.resume` 不持久化狀態

- **檔案**：`crates/duduclaw-gateway/src/handlers.rs:299–309`
- **函式**：`handle_agents_pause()`, `handle_agents_resume()`
- **問題**：回傳 `"success": true` 但從不寫入 `agent.toml`，重啟後狀態完全消失
- **標記**：`// TODO: actually modify agent.toml status field`

---

### C-4｜MCP Server 多個核心工具是 placeholder

- **檔案**：`crates/duduclaw-cli/src/mcp.rs:598–599`
- **問題**：Claude Code 呼叫以下工具時得到假成功，什麼都不發生

| MCP 工具名 | 狀態 |
|------------|------|
| `send_to_agent` | placeholder |
| `schedule_task` | placeholder |
| `send_photo` | placeholder |
| `send_sticker` | placeholder |
| `log_mood` | placeholder |

```rust
"send_photo" | "send_sticker" | "send_to_agent" | "log_mood" | "schedule_task" => {
    placeholder_response(tool_name)
}
```

---

### C-5｜三層 Evolution Engine 沒有 AI 邏輯

| 層級 | 檔案 | 行號 | 標記 | 實際行為 |
|------|------|------|------|---------|
| Micro | `python/duduclaw/evolution/micro.py` | 39 | `# TODO: Use Claude to generate actual reflection` | 只把對話摘要 append 到 markdown |
| Meso | `python/duduclaw/evolution/meso.py` | 38 | `# TODO: Use Claude to analyze patterns across notes` | 只計算筆記數量 |
| Macro | `python/duduclaw/evolution/macro_.py` | 33 | `# TODO: Use Claude to evaluate skill quality` | 掃描技能檔案後 `pass` |

---

### C-6｜多帳號輪替邏輯根本性錯誤

- **檔案**：`python/duduclaw/sdk/chat.py:221–255`
- **問題**：
  1. `load_accounts` 用字串掃描 TOML，找到第一個 key 就 `break`，永遠只讀到一個帳號
  2. `get_account_key` 完全忽略 `account_id` 參數，永遠回傳 `ANTHROPIC_API_KEY` 環境變數
  3. `Account` 物件不儲存實際的 API key 值，只有 `id`
- **結果**：`AccountRotator` 的多帳號旋轉機制完全無效

---

### C-7｜`agents.delegate` 不觸發任何執行

- **檔案**：`crates/duduclaw-gateway/src/handlers.rs:291–296`
- **函式**：`handle_agents_delegate()`
- **問題**：只產生一個 UUID 然後回傳假成功，沒有呼叫目標 agent、觸發 Claude 或建立任何 IPC

---

## 🟠 HIGH — 造成誤導或主要功能損壞

### H-1｜`system.status` hardcode 回傳錯誤數值

- **檔案**：`crates/duduclaw-gateway/src/handlers.rs:726–734`
- **問題**：`uptime_seconds: 0`、`channels_connected: 0` 永遠是 hardcoded，即使通道已連線也顯示 0

---

### H-2｜`channels.status` 不代表真實連線狀態

- **檔案**：`crates/duduclaw-gateway/src/handlers.rs:348–373`
- **問題**：`connected: true` 只判斷 token 欄位是否不為空，與實際連線狀態無關；通道斷線後 dashboard 仍顯示已連線

---

### H-3｜`accounts.rotate` 完全是 placeholder

- **檔案**：`crates/duduclaw-gateway/src/handlers.rs:537–541`
- **函式**：`handle_accounts_rotate()`
- **問題**：回傳 `"Key rotation placeholder"`，帳號輪替功能完全未實作
- **影響**：Dashboard 的輪替按鈕無任何效果

---

### H-4｜Cron 系統全部是 placeholder

| 函式 | 檔案 | 行號 | 問題 |
|------|------|------|------|
| `handle_cron_list()` | `handlers.rs` | ~706 | 永遠回傳 `"tasks": []` |
| `handle_cron_add()` | `handlers.rs` | 709 | `"Cron add placeholder"` |
| `handle_cron_pause()` | `handlers.rs` | 715 | `"Cron pause placeholder"` |
| `handle_cron_remove()` | `handlers.rs` | 721 | `"Cron remove placeholder"` |
| `HeartbeatScheduler::run()` | `heartbeat.rs` | 84 | `// TODO: trigger agent heartbeat action (Phase 4)` |

---

### H-5｜`duduclaw agent` 互動模式沒有 AI 功能

- **檔案**：`crates/duduclaw-agent/src/runner.rs:65–74`
- **問題**：執行 `duduclaw agent` CLI 對話只是 echo 輸入，附上 "Phase 3 pending" 提示
- **標記**：`// Real Claude Code SDK integration will come in Phase 3.`

---

### H-6｜Logs WebSocket 推送不存在

- **檔案**：`crates/duduclaw-gateway/src/handlers.rs:796–805`
- **函式**：`handle_logs_subscribe()`
- **問題**：回傳 `"WebSocket push is future work"`，沒有任何把 tracing 事件推送到 WebSocket 的機制
- **影響**：Dashboard Logs 頁面永遠空白

---

### H-7｜`memory.browse` 永遠回傳空 entries

- **檔案**：`crates/duduclaw-gateway/src/handlers.rs:628–636`
- **問題**：`entries: []` hardcoded 空陣列，Dashboard Memory 頁面的 entry 列表永遠不顯示資料

---

### H-8｜Python channels 插件與 Rust bus 完全解耦

- **檔案**：`python/duduclaw/channels/base.py:20`
- **問題**：`on_message_received()` 方法體只有 `pass`，TODO 指出需要呼叫 `_native.send_to_bus()`
- **標記**：`# TODO: Phase 3 - call _native.send_to_bus()`
- **影響**：Python channels 模組是孤兒程式碼，接收到訊息後無法傳入 Rust 處理

---

### H-9｜`sdk/health.py` 的帳號健康檢查未實作

- **檔案**：`python/duduclaw/sdk/health.py:11`
- **函式**：`check_account_health()`
- **問題**：只檢查 `account.is_healthy` 屬性，不實際呼叫 Anthropic API 驗證
- **標記**：`# TODO: Phase 2 - implement actual API health check`

---

### H-10｜Agent Tools 全部未實作

- **檔案**：`python/duduclaw/tools/agent_tools.py`
- **問題**：所有公開方法都是 TODO

| 行號 | 方法 | 標記 |
|------|------|------|
| 12 | `agent_list()` | `# TODO: Call into Rust via _native bridge` |
| 28 | `agent_create()` | `# TODO: Write agent directory + agent.toml + SOUL.md` |
| 47 | `agent_delegate()` | `# TODO: Send IPC message via broker` |
| 56 | `agent_status()` | `# TODO: Query from registry` |

---

## 🟡 MEDIUM — 品質差或有 bug

### M-1｜Session 壓縮不呼叫 AI，直接丟棄 context

- **檔案**：`crates/duduclaw-gateway/src/channel_reply.rs:170–171`
- **問題**：超過 50k tokens 時，壓縮只插入固定字串 `"[Session compressed — previous conversation summary omitted for brevity]"`，不呼叫 Claude 產生摘要
- **影響**：下次對話時完全失去歷史 context

---

### M-2｜Token 計算極度不準確

- **檔案**：`crates/duduclaw-gateway/src/channel_reply.rs:91`
- **問題**：用 `text.len() / 4` 估算 tokens，對中文（每字 1-3 tokens）和程式碼完全不準
- **影響**：預算追蹤和壓縮觸發時機都是錯的

---

### M-3｜前後端 `memory.search` 回傳欄位名不符

- **前端**：`web/src/lib/api.ts:149–156`，期望 `{ entries: MemoryEntry[] }`
- **後端**：`crates/duduclaw-gateway/src/handlers.rs:589`，回傳 `{ results: [] }`
- **影響**：前端解構永遠拿不到資料，記憶搜尋頁面永遠空白

---

### M-4｜API Key 明文寫入 config.toml

- **檔案**：`crates/duduclaw-cli/src/main.rs:505`
- **問題**：`duduclaw onboard` 把 API key 寫成 TOML 明文，`CredentialProxy`（AES-256-GCM）有加密機制但從未被使用
- **安全影響**：config 檔案外洩等於 API key 外洩

---

### M-5｜`system.doctor` 的 container_runtime 永遠 pass

- **檔案**：`crates/duduclaw-gateway/src/handlers.rs:926–930`
- **問題**：WebSocket 版的 doctor 不實際呼叫 Docker daemon，`status: "pass"` hardcoded
- **注意**：CLI 版 `cmd_doctor()` 有正確實作，兩者行為不一致

---

### M-6｜Memory Engine 的 summarize 不使用 AI

- **檔案**：`crates/duduclaw-memory/src/engine.rs:200`
- **問題**：`SqliteMemoryEngine::summarize()` 只做字串串接，不呼叫 Claude
- **標記**：`// Simple concatenation summary (AI-powered summarisation is planned for Phase 4)`

---

### M-7｜Discord Heartbeat 可能在高頻訊息下漏掉

- **檔案**：`crates/duduclaw-gateway/src/discord.rs:163–282`
- **問題**：heartbeat 與 write 串流共用同一個 WebSocket 連線，只靠 `try_recv()` 非阻塞輪詢協調，高頻訊息情境可能漏掉 heartbeat 導致連線中斷

---

### M-8｜Auth Manager 整個未實作

- **檔案**：`crates/duduclaw-gateway/src/auth.rs:6–7`
- **問題**：`AuthManager` 結構上有 Ed25519 challenge-response 的設計，但實際邏輯標記為 Phase 5
- **現況**：Dashboard 對外無任何認證保護

---

## 修復優先順序

```
Phase 1（立即）— 讓基礎架構可通
  ├── C-1  PyO3 橋接實作 send_message
  ├── C-6  多帳號輪替 load_accounts 邏輯修正
  └── M-4  API Key 改用 CredentialProxy 加密存取

Phase 2（核心功能）— 讓主要流程可用
  ├── C-3  agents.pause/resume 寫入 agent.toml
  ├── C-7  agents.delegate 觸發真實 IPC
  ├── H-3  accounts.rotate 實作
  ├── H-8  Python channels base 接入 Rust bus
  └── H-9  health.py 呼叫 Anthropic API 驗證

Phase 3（Agent 對話）— 讓 AI 對話可用
  ├── C-2  CLI agent create/pause/resume 實作
  ├── H-5  duduclaw agent 互動接入 Claude Code SDK
  ├── H-10 agent_tools.py 全部實作
  └── C-4  MCP send_to_agent 實作

Phase 4（Evolution & 記憶）— 讓進化系統可用
  ├── C-5  三層反思加入 Claude 呼叫
  ├── H-7  memory.browse 從 SQLite 讀取
  ├── M-3  memory.search 欄位名統一（results → entries 或反向修正）
  ├── M-1  Session 壓縮呼叫 AI 摘要
  └── M-6  Memory summarize 呼叫 AI

Phase 5（Dashboard & 維運）— 讓監控可用
  ├── H-1  system.status 動態計算 uptime / channels_connected
  ├── H-2  channels.status 反映真實連線狀態
  ├── H-4  Cron 系統完整實作
  ├── H-6  Logs WebSocket 推送實作
  ├── C-2  service 子命令（install/start/stop）實作
  └── M-8  Auth Manager Ed25519 實作

Phase 6（品質提升）— 讓數字準確
  ├── M-2  Token 計算改用真實 tokenizer
  ├── M-5  WebSocket doctor 呼叫 Docker daemon
  └── M-7  Discord heartbeat 改用獨立 write channel
```

---

## 統計摘要

| 嚴重程度 | 數量 |
|----------|------|
| 🔴 CRITICAL | 7 |
| 🟠 HIGH | 10 |
| 🟡 MEDIUM | 8 |
| **總計** | **25** |

| 來源 | TODO 數量 |
|------|-----------|
| Rust（TODO 標記） | 15 |
| Rust（Placeholder 回應） | 5 |
| Python（TODO 標記） | 6 |
| TypeScript/前端 | 3 |
| **總計** | **29** |
