# DuDuClaw v0.6.0 深度程式碼審查報告

> 日期：2026-03-23
> 審查範圍：前端 / 後端 / CLI / MCP
> 審查方法：4 個專門 Agent 平行掃描 + 整合分析

---

## 總覽

| 嚴重程度 | 數量 | 說明 |
|----------|------|------|
| CRITICAL | 8 | 必須立即修復 |
| HIGH | 24 | 強烈建議修復 |
| MEDIUM | 22 | 應該改善 |
| LOW | 10 | 可選改善 |
| **合計** | **64** | 4 個 Agent 完整掃描 |

**整體裁決：BLOCK — 6 個 CRITICAL 問題必須修復後才能 release。**

---

## CRITICAL（3 項）

### CR-01. 頻道 Token 以明文寫入 config.toml
- **檔案**：`crates/duduclaw-cli/src/main.rs:566-572`
- **問題**：LINE token、Telegram token、Discord token 在 `cmd_onboard` 中以明文格式寫入 `config.toml`。Anthropic API key 有 AES-256-GCM 加密，但其他頻道憑證完全未加密。
- **風險**：config.toml 被備份外洩、版控提交或其他 process 讀取時，所有 Bot 憑證即刻洩漏。
- **修正**：使用相同的 `encrypt_api_key` 機制加密所有 token，或限制 config.toml 權限為 `0600`。

### CR-02. Dispatcher TOCTOU 競態條件導致訊息遺失
- **檔案**：`crates/duduclaw-gateway/src/dispatcher.rs:99-154`
- **問題**：`poll_and_dispatch` 流程為：讀取 → 移除待處理 → 寫回 → 並發追加回應。若另一個 MCP 工具在同時追加新訊息，或兩個 poll 重疊執行，`tokio::fs::write` 整檔覆蓋會導致訊息遺失。
- **修正**：使用 `tokio::sync::Mutex` 保護 `poll_and_dispatch`，或改用 SQLite。

### CR-03. 金鑰生成失敗時回退到全零金鑰
- **檔案**：`crates/duduclaw-cli/src/main.rs:26`
- **問題**：`CryptoEngine::generate_key().unwrap_or([0u8; 32])` — 若 OS 隨機數源失敗，靜默使用全零 AES-256 金鑰，使加密形同虛設。
- **修正**：傳播錯誤而非回退：`generate_key()?`

---

## HIGH（8 項）

### CR-04. TOML Injection — 使用者值直接插入模板字串
- **檔案**：`crates/duduclaw-cli/src/mcp.rs:739-785`
- **問題**：`handle_create_agent` 用 `format!()` 直接嵌入 `display_name`、`trigger` 等值到 TOML 模板。若值含 `"` 或換行，可注入任意 TOML 設定。
- **修正**：改用 `toml::to_string(&config)` 序列化。

### CR-05. Path Traversal — agent_name 未驗證
- **檔案**：`crates/duduclaw-cli/src/main.rs:1130,1265`
- **問題**：`cmd_test_agent` 的 `agent_name` 直接用於路徑建構，若為 `../../etc/passwd` 可遍歷任意路徑。
- **修正**：在入口驗證 `agent_name` 只含 `[a-z0-9-]`。

### CR-06. 沙箱 API Key 透過 env 傳遞（與文件不符）
- **檔案**：`crates/duduclaw-container/src/sandbox.rs:73`
- **問題**：Doc comment 聲稱 "API key injected via stdin"，但實際使用環境變數。env 可在 `/proc/[pid]/environ` 洩漏。
- **修正**：更新文件反映實際行為，或改用 stdin 注入。

### CR-07. Cron 表達式未驗證即持久化
- **檔案**：`crates/duduclaw-cli/src/mcp.rs:638` + `crates/duduclaw-gateway/src/cron_scheduler.rs:102`
- **問題**：`handle_schedule_task` 不驗證 cron 表達式即寫入磁碟。`* * * * *`（每分鐘）可觸發過量 API 呼叫。
- **修正**：寫入前 parse 驗證 + 最小間隔限制（5 分鐘）。

### CR-08. Python 子進程路徑不安全
- **檔案**：`crates/duduclaw-agent/src/heartbeat.rs:350`
- **問題**：`PYTHONPATH` 由使用者可控的 `home_dir` 上層目錄決定，攻擊者可注入惡意 Python 模組。
- **修正**：固定 Python 執行路徑，白名單驗證 `reflection_type`。

### CR-09. API Key 加密失敗靜默降級為明文
- **檔案**：`crates/duduclaw-gateway/src/claude_runner.rs:110-118`
- **問題**：加密金鑰解密失敗時，靜默回退讀取明文 `anthropic_api_key`。
- **修正**：解密失敗時回傳錯誤 + warn 日誌，不靜默降級。

### CR-10. Dispatcher JoinHandle 錯誤被靜默忽略
- **檔案**：`crates/duduclaw-gateway/src/dispatcher.rs:159`
- **問題**：`let _ = handle.await` 吞掉 task panic。
- **修正**：`handle.await.unwrap_or_else(|e| warn!("dispatch panic: {e}"));`

### CR-11. MCP 工具未驗證 agent_id 存在性
- **檔案**：`crates/duduclaw-cli/src/mcp.rs:639,486`
- **問題**：`handle_send_to_agent` 和 `handle_schedule_task` 未驗證 `agent_id` 是否存在即寫入 JSONL。
- **修正**：寫入前檢查 agents 目錄中是否存在對應資料夾。

---

## MEDIUM（7 項）

### CR-12. WebSocket 重連時 Promise resolve/reject 洩漏
- **檔案**：`web/src/lib/ws-client.ts:87-108`
- **問題**：重連時 `resolve`/`reject` 可能被雙重呼叫。
- **修正**：加入 `settled` 旗標防止雙重 settle。

### CR-13. Agent store 樂觀更新無回滾
- **檔案**：`web/src/stores/agents-store.ts:46,55`
- **問題**：`pauseAgent`/`resumeAgent` 在 API 失敗時不回滾 UI 狀態。
- **修正**：保存 prev state，catch 時 set 回 prev。

### CR-14. SkillMarket 類別按鈕搜尋用到舊 state
- **檔案**：`web/src/pages/SkillMarketPage.tsx:97-98`
- **問題**：`setQuery(cat)` 後立即 `handleSearch()`，但 state 尚未更新。
- **修正**：直接傳參數 `handleSearchQuery(cat)`。

### CR-15. OrgChart 循環依賴可能造成無限遞迴
- **檔案**：`web/src/components/OrgChart.tsx:33`
- **問題**：若 `reports_to` 形成循環（A→B→A），`toNode` 遞迴不會終止。
- **修正**：加入 `visited` Set 防止循環。

### CR-16. SOUL 備份同秒衝突
- **檔案**：`crates/duduclaw-security/src/soul_guard.rs:154`
- **問題**：備份檔名 `SOUL_YYYYMMDD_HHMMSS.md` 若同秒呼叫兩次會覆蓋。
- **修正**：附加毫秒或 UUID。

### CR-17. input_guard 單一高危規則不觸發封鎖
- **檔案**：`crates/duduclaw-security/src/input_guard.rs`
- **問題**：`tool_abuse`（weight=30）單獨命中不會封鎖（30 < threshold 60），但 `rm -rf /` 應直接封鎖。
- **修正**：高危規則加入「單一規則即封鎖」機制。

### CR-18. Heartbeat 寫鎖長期持有
- **檔案**：`crates/duduclaw-agent/src/heartbeat.rs:260`
- **問題**：`run()` 中 `tasks.write()` 在整個 for 迴圈期間持有，阻塞 `status()` 讀取。
- **修正**：先收集需要 spawn 的任務為 Vec，釋放鎖後再 spawn。

---

## LOW（2 項）

### CR-19. SettingsPage 錯誤被靜默吞掉
- **檔案**：`web/src/pages/SettingsPage.tsx`
- **問題**：HeartbeatTab、CronTab、DoctorTab 的 catch 區塊為空。
- **修正**：設定 error state 並在 UI 顯示。

### CR-20. 自製 HTML 解析器脆弱
- **檔案**：`crates/duduclaw-cli/src/mcp.rs:356-405`
- **問題**：`strip_html_tags` 字符級狀態機無法處理邊界情況。
- **修正**：改用 `scraper` crate。

---

---

## MCP Agent 補充發現（CRITICAL + HIGH）

### CR-21. [CRITICAL] send_photo Schema 參數名 `url_or_path` 與 handler 讀取的 `url` 不符
- **檔案**：`crates/duduclaw-cli/src/mcp.rs:46,535`
- **問題**：Schema 宣告 `url_or_path`（required），但 handler 讀取 `url`/`sticker_id`/`file_id`。Claude Code 傳入 `url_or_path` 但 handler 永遠讀不到，`send_photo` 功能完全失效。
- **修正**：handler 改為讀取 `params.get("url_or_path")`。

### CR-22. [CRITICAL] log_mood Schema 宣告 `score` 但 handler 讀取 `mood`
- **檔案**：`crates/duduclaw-cli/src/mcp.rs:76-79,607-608`
- **問題**：Schema 有 `score: "Mood score (1-10)"`（required），handler 讀取 `mood`（不存在於 schema），結果永遠是 `"neutral"`。功能完全失效。
- **修正**：統一 schema 和 handler 的參數名稱。

### CR-23. [CRITICAL] schedule_task Schema 宣告 `description` 但 handler 讀取 `task`/`prompt`
- **檔案**：`crates/duduclaw-cli/src/mcp.rs:85,641`
- **問題**：Claude Code 傳入 `description`，handler 讀 `task`/`prompt`，永遠空值，始終回傳 "Error: cron and task are required"。功能完全失效。
- **修正**：handler 加入 `.or_else(|| params.get("description"))`。

### CR-24. [HIGH] 所有 HTTP 回應未檢查 4xx/5xx 狀態碼
- **檔案**：`crates/duduclaw-cli/src/mcp.rs:258,283,309,567,586`
- **問題**：`Status: 401`/`429`/`403` 都被回報為「成功」。
- **修正**：檢查 `resp.status().is_success()`。

### CR-25. [HIGH] bus_queue.jsonl / cron_tasks.jsonl 無大小限制
- **檔案**：`crates/duduclaw-cli/src/mcp.rs:506,663,1027`
- **問題**：無限追加可造成 Disk DoS + OOM（`count_pending_tasks` 讀取整檔到記憶體）。
- **修正**：寫入前檢查檔案大小上限。

### CR-26. [HIGH] MCP 工具完全未引用 input_guard 掃描
- **檔案**：`crates/duduclaw-cli/src/mcp.rs:486,993,638`
- **問題**：`duduclaw-security` 有 `scan_input` 但 MCP 完全沒使用。`send_to_agent` 的 `prompt`、`spawn_agent` 的 `task` 不經過濾即執行。
- **修正**：在 dispatcher 或 MCP handler 中加入 `scan_input` 檢查。

### CR-27. [MEDIUM] send_media channel 預設為 telegram（靜默回退）
- **檔案**：`crates/duduclaw-cli/src/mcp.rs:533`
- **問題**：`channel` 未提供時預設 `"telegram"`，與 `send_message` 行為不一致。

### CR-28. [HIGH] keyfile 寫入失敗靜默忽略 — 後續解密永遠失敗
- **檔案**：`crates/duduclaw-cli/src/main.rs:27`
- **問題**：`let _ = std::fs::write(&keyfile, &key)` — 若寫入失敗（權限/磁碟滿），下次啟動會生成新 key，永遠無法解密已存的 API key。
- **修正**：回傳 `Result`，write 失敗即 fatal。

### CR-29. [HIGH] service logs `--lines` 參數被丟棄
- **檔案**：`crates/duduclaw-cli/src/main.rs:236`
- **問題**：`ServiceCommands::Logs { lines: _ }` 明確丟棄使用者指定的行數。CLI 廣告 `--lines` 但從不使用。
- **修正**：傳遞 `lines` 到 `ServiceAction::Logs(lines)`。

### CR-30. [HIGH] migrate.rs 不偵測已遷移 agent，重複執行會覆蓋手動編輯
- **檔案**：`crates/duduclaw-cli/src/migrate.rs:37,130`
- **問題**：`skipped` 計數器永遠是 0（從未遞增），第二次 `duduclaw migrate` 會覆蓋 `.claude/settings.local.json` 和 `CLAUDE.md`。
- **修正**：檢查目標檔案是否存在，存在即 skip。

### CR-31. [MEDIUM] migrate 硬編碼 `allow_bash: true`
- **檔案**：`crates/duduclaw-cli/src/migrate.rs:56-62`
- **問題**：每個遷移的 agent 都被授予無限制 bash 存取權，不讀取原始 `agent.toml` 的權限設定。

### CR-32. [MEDIUM] cmd_test_agent PASS/FAIL 語意反轉易混淆
- **檔案**：`crates/duduclaw-cli/src/main.rs:1188,1204`
- **問題**：`passed: scan.risk_score >= 25` 表示「掃描器成功偵測到攻擊」= PASS，但 JSON 報告中 `"passed": true` 會被誤解為「輸入是安全的」。
- **修正**：改名為 `detected: bool` 或使用 `Detected`/`Missed` 枚舉。

### CR-33. [LOW] 歡迎訊息版本號硬編碼 v0.1.0
- **檔案**：`crates/duduclaw-cli/src/main.rs:263`
- **問題**：`歡迎使用 DuDuClaw v0.1.0` 硬編碼，實際版本是 0.6.0。
- **修正**：改用 `env!("CARGO_PKG_VERSION")`。

### CR-34. [LOW] systemd unit 硬性依賴 docker.service
- **檔案**：`crates/duduclaw-cli/src/service/mod.rs:57-58`
- **問題**：`Requires=docker.service` — Docker 未安裝時 `systemctl start duduclaw` 直接失敗。
- **修正**：改用 `Wants=docker.service`。

### CR-35. [HIGH] AddAccountDialog 是假實作 — API Key 被丟棄但顯示成功
- **檔案**：`web/src/pages/AccountsPage.tsx:166-183`
- **問題**：使用者填入 API Key / 預算 / 優先級後按「新增」，handler 只呼叫 `api.accounts.health()` 然後關閉 Dialog。所有輸入被丟棄，使用者誤以為帳號已新增。
- **修正**：實作真正的 `accounts.add` API，或暫時禁用按鈕並顯示「尚未實作」。

### CR-36. [HIGH] skill.url 未驗證協定 — javascript: URI XSS 風險
- **檔案**：`web/src/pages/SkillMarketPage.tsx:121-129`
- **問題**：`<a href={skill.url}>` 的 `skill.url` 來自後端，若為 `javascript:alert(1)` 可在當前頁面執行。
- **修正**：渲染前檢查 `url.startsWith('https://')`。

### CR-37. [HIGH] 全應用程式無 Error Boundary
- **檔案**：`web/src/App.tsx`
- **問題**：任何 page component 未捕捉例外會導致整個 SPA 白屏。
- **修正**：在 `<Routes>` 外層加入 React Error Boundary。

### CR-38. [HIGH] 大量 Dialog/Card UI 字串硬編碼未走 i18n
- **檔案**：`AgentsPage.tsx:198-307`, `ChannelsPage.tsx:248-283`, `AccountsPage.tsx:186-250`, `SecurityPage.tsx:63-133`, `SettingsPage.tsx:81-109`, `OrgChart.tsx:341-366`
- **問題**：數十個使用者介面字串繞過 intl，包括表單標籤、按鈕文字、Card 標題。
- **修正**：逐一移入 i18n JSON。

### CR-39. [HIGH] AgentsPage sandbox_enabled 未加入 AgentDetail 型別
- **檔案**：`web/src/pages/AgentsPage.tsx:86`
- **問題**：`agent as unknown as { sandbox_enabled?: boolean }` 繞過型別系統。
- **修正**：在 `api.ts` 的 `AgentDetail` interface 加入 `sandbox_enabled?: boolean`。

### CR-40. [MEDIUM] LogsPage 無虛擬化 — 5000 筆全量渲染
- **檔案**：`web/src/pages/LogsPage.tsx:155-207`
- **問題**：entries 最多 5000 筆時 `.map()` 渲染所有 DOM。
- **修正**：引入 `@tanstack/react-virtual`（已在 package.json）。

### CR-41. [MEDIUM] OrgChart isDark stale closure
- **檔案**：`web/src/components/OrgChart.tsx:119`
- **問題**：`useCallback` 依賴不含 `isDark`，theme 切換但 agents 不變時顏色不更新。
- **修正**：將 isDark 加入 `useCallback` 依賴或改為 state。

### CR-42. [MEDIUM] logs-store subscribe 重複呼叫堆積 handler
- **檔案**：`web/src/stores/logs-store.ts:26-35`
- **問題**：React Strict Mode 雙重 effect 時 `subscribe()` 被呼叫兩次，舊 subscription 不清除。
- **修正**：subscribe 前先 unsubscribe 舊的。

### CR-43. [MEDIUM] api.ts 所有呼叫使用 `as Promise<T>` 無執行期驗證
- **檔案**：`web/src/lib/api.ts` 全域
- **問題**：後端格式不符時只在深層屬性存取才 TypeError。
- **修正**：加入 zod schema 或 guard function。

---

## Backend Agent 補充發現

### CR-44. [CRITICAL] Python 腳本注入 — handle_accounts_rotate
- **檔案**：`crates/duduclaw-gateway/src/handlers.rs:690-716`
- **問題**：`python_path` 和 `config_path` 直接插入 `format!()` 生成的 Python 原始碼，以 `python3 -c` 執行。若路徑含單引號可注入任意 Python 程式碼（RCE）。
- **修正**：改用環境變數傳遞路徑，不在 `-c` 中嵌入使用者可控值。

### CR-45. [CRITICAL] IPC target_agent 路徑穿越
- **檔案**：`crates/duduclaw-agent/src/ipc.rs:86`
- **問題**：`message.target_agent` 未驗證即用於 `ipc_dir.join()`。若為 `../../../etc` 可在任意目錄建立檔案。`message.id` 有 UUID 驗證但 `target_agent` 完全沒有。
- **修正**：加入 `validate_agent_id()` 白名單檢查。

### CR-46. [HIGH] 同步阻塞 I/O 在 async runtime — audit.rs, soul_guard.rs, skill_loader.rs
- **檔案**：`crates/duduclaw-security/src/audit.rs:54-79`, `soul_guard.rs` 全域, `skill_loader.rs` 全域
- **問題**：`std::fs::*` 同步呼叫阻塞 Tokio worker thread。
- **修正**：使用 `tokio::task::spawn_blocking` 包裝。

### CR-47. [HIGH] CronScheduler 無並發上限 — 可無限累積 Claude CLI 子行程
- **檔案**：`crates/duduclaw-gateway/src/cron_scheduler.rs:171`
- **問題**：每次 cron 觸發都 `tokio::spawn`，無 Semaphore 保護（heartbeat 有但 cron 沒有）。
- **修正**：加入 `Semaphore` 限制並發。

### CR-48. [HIGH] 巢狀 RwLock 取鎖順序不一致，潛在死鎖
- **檔案**：`crates/duduclaw-agent/src/heartbeat.rs:159-193`
- **問題**：`sync_from_registry` 同時持有 `registry.read()` 和 `agents.write()`。
- **修正**：先讀 registry 收集資料，drop reg lock，再取 agents write lock。

### CR-49. [HIGH] handlers.rs handle_agents_create 未驗證 agent name
- **檔案**：`crates/duduclaw-gateway/src/handlers.rs:258`
- **問題**：WebSocket RPC 路徑的 `agents.create` 未對 name 做路徑安全檢查。
- **修正**：同 mcp.rs 的 `create_agent` 加入字符白名單。

### CR-50. [HIGH] handle_channels_test 洩漏 token 前綴
- **檔案**：`crates/duduclaw-gateway/src/handlers.rs:603`
- **問題**：回傳 token 前 4 字元給 WebSocket 客戶端。
- **修正**：完全省略 token 資訊。

### CR-51. [MEDIUM] 可預測臨時檔案 .tmp_system_prompt.md 競爭條件
- **檔案**：`crates/duduclaw-gateway/src/channel_reply.rs:388`
- **問題**：固定檔名在並發請求時互相覆蓋。
- **修正**：使用 `tempfile` crate 或 UUID 檔名。

### CR-52. [MEDIUM] contract.rs extract_context 在 CJK 字串中按 byte 切片可能 panic
- **檔案**：`crates/duduclaw-agent/src/contract.rs:143-155`
- **問題**：`text[start..end]` 的 start/end 是 byte offset，可能切在多位元組 UTF-8 字元中間。
- **修正**：使用 `text.get(start..end).unwrap_or("")` 或 char boundary 對齊。

### CR-53. [MEDIUM] skill_loader install_skill 聲稱做安全掃描但未實作
- **檔案**：`crates/duduclaw-agent/src/skill_loader.rs:195-225`
- **問題**：docstring 聲稱呼叫 `vet_skill`，`quarantine_dir` 參數被接受但未使用。
- **修正**：實作安全掃描或更新文件。

### CR-54. [MEDIUM] audit.rs read_recent_events O(n) 全檔讀取，無 rotation
- **檔案**：`crates/duduclaw-security/src/audit.rs:82-97`
- **問題**：audit log 無限增長，每次查詢讀完整個檔案。
- **修正**：加入 size/time rotation 機制。

### CR-55. [LOW] 三個模組重複 find_python_path 實作
- **檔案**：`channel_reply.rs:334`, `evolution.rs:197`, `heartbeat.rs:414`
- **修正**：提取到 `duduclaw-core` 共用。

### CR-56. [LOW] handlers.rs 超過 800 行
- **修正**：按功能拆分為 agents/channels/memory/cron/system 子模組。

### CR-57. [LOW] 三處重複的 bus_queue append 邏輯
- **檔案**：`crates/duduclaw-cli/src/mcp.rs:506,663,1027`
- **問題**：`handle_send_to_agent`/`handle_schedule_task`/`handle_spawn_agent` 各自複製相同的 `spawn_blocking` + append。
- **修正**：抽取為 `async fn append_to_jsonl(path, entry) -> bool`。

---

## 修復優先順序

### Sprint A（CRITICAL — 立即修復，8 項）
1. **CR-44** Python 腳本注入 RCE → 改用環境變數傳遞
2. **CR-45** IPC target_agent 路徑穿越 → 白名單驗證
3. **CR-21** send_photo schema/handler 不符 → 修正參數名
4. **CR-22** log_mood schema/handler 不符 → 統一參數
5. **CR-23** schedule_task schema/handler 不符 → 加入 fallback 讀取
6. **CR-03** 全零金鑰回退 → 改為錯誤傳播
7. **CR-01** 頻道 Token 明文 → 加密或限制權限
8. **CR-02** Dispatcher TOCTOU → 加 Mutex

### Sprint B（HIGH — 安全修復）
7. **CR-04** TOML Injection → 改用 toml crate 序列化
8. **CR-05** Path Traversal → agent_name 驗證
9. **CR-06** Sandbox env 洩漏 → 更新文件
10. **CR-07** Cron 驗證 → 寫入前 parse
11. **CR-24** HTTP 狀態碼檢查
12. **CR-25** JSONL 大小限制
13. **CR-26** MCP 引用 input_guard
14. **CR-09** 加密降級 → 不靜默回退

### Sprint C（MEDIUM + LOW — 品質）
15-34. 其餘 MEDIUM/LOW issues
