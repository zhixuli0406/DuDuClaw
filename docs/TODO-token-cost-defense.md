# TODO: Token Cost Defense — 六合一最佳組合方案

> Claude Agent SDK 隱藏 Token 成本防禦方案
> Created: 2026-03-28
> Reference: [CabLate 逆向分析](https://cablate.com/articles/reverse-engineer-claude-agent-sdk-hidden-token-cost/)

---

## 背景

Claude Agent SDK V1 (`query()`) 每次 spawn 新子程序，runtime 注入約 45K tokens 的
system-reminder（git status、CLAUDE.md、skill 列表等），因時間戳/mtime 差異導致
prompt cache prefix 斷裂，每次以 125% cache write 費率計費。

- 每則訊息消耗 5h 配額的 2-3%
- Cache 效率上限僅 25%（V2 可達 84%）
- 子 Agent 每 turn 浪費 ~50K tokens
- Input > 200K tokens 時 Anthropic 費率翻倍

DuDuClaw 的 `claude_runner.rs` 使用 `claude -p` 每次 spawn，完全受此問題影響。

---

## 架構：三層防禦 + 觀測層

```
╔══════════════════════════════════════════════════════════════╗
║  觀測層 (Phase 0) — CostTelemetry                           ║
║  • cache_efficiency 即時監控                                 ║
║  • 200K 價格懸崖預警                                         ║
║  • per-agent cost breakdown → Dashboard / MCP                ║
║  • 驅動下方三層的動態切換                                     ║
╠══════════════════════════════════════════════════════════════╣
║  第一層：迴避 (Phase 1) — 能不打 API 就不打                   ║
║  • Confidence Router 閾值調低                                ║
║  • 演化反思全部 MLX 本地化                                    ║
║  • Request Coalescing (bus_queue 合併)                        ║
╠══════════════════════════════════════════════════════════════╣
║  第二層：最佳化 (Phase 2) — 打 API 時最大化 cache hit         ║
║  • [純對話] Direct API (duduclaw-api crate)                   ║
║  • [工具路徑] CLI 瘦身 (Skill 漸進注入 + CLAUDE.md 摘要)      ║
║  • Runtime 注入正規化 (確定性序列化)                          ║
╠══════════════════════════════════════════════════════════════╣
║  第三層：復用 (Phase 3→4) — 多輪對話 cache 最大化             ║
║  • [現階段] Subprocess Pool + Session Pinning                 ║
║  • [V2 穩定後] 遷移至 createSession()                        ║
╚══════════════════════════════════════════════════════════════╝
```

### 路由決策流程

```
收到請求
  │
  ├─ CostTelemetry: 估算 input tokens
  │   └─ > 180K? → 強制壓縮 (防 200K 懸崖)
  │
  ├─ ConfidenceRouter: 評估複雜度
  │   └─ 低/中複雜度? → Local inference → done
  │
  ├─ 需要 Cloud API
  │   ├─ 純對話（無工具）? → Direct API → done
  │   └─ 需要工具?
  │       ├─ Agent 有 warm proc? → 復用 pool → done
  │       └─ Cold start → 瘦身 CLI + spawn into pool → done
  │
  └─ CostTelemetry: 記錄 usage
      └─ cache_eff < 30%? → 下次自動切換路徑
```

---

## Phase 0: CostTelemetry 觀測層 (Week 1)

### 0.1 定義型別與 struct

- [ ] `crates/duduclaw-gateway/src/cost_telemetry.rs` — `CostTelemetry` struct
- [ ] `TokenUsage` 型別: `input_tokens`, `cache_read_tokens`, `cache_creation_tokens`, `output_tokens`
- [ ] `CostRecord` 型別: `agent_id`, `request_type` (chat/evolution/cron), `model`, `usage`, `timestamp`, `cache_efficiency`
- [ ] `CostSummary` 型別: 滑動窗口統計 (1h/24h/7d)
- [ ] `cache_efficiency()` 公式: `cache_read / (input + cache_read + cache_creation)`
- [ ] `is_near_price_cliff()`: input > 180K → true

### 0.2 SQLite token_usage 表

- [ ] `crates/duduclaw-gateway/src/session.rs` 加入 `token_usage` 表 migration
- [ ] Schema: `id INTEGER PK, agent_id TEXT, request_type TEXT, model TEXT, input_tokens INT, cache_read_tokens INT, cache_creation_tokens INT, output_tokens INT, cache_efficiency REAL, cost_cents INT, created_at TEXT`
- [ ] `CostTelemetry::record()` 寫入
- [ ] `CostTelemetry::summary_by_agent()` 查詢 (時間範圍)
- [ ] `CostTelemetry::summary_global()` 全域統計
- [ ] Index: `agent_id + created_at`

### 0.3 修改 stream-json 解析提取 usage

- [ ] `call_claude_streaming()` 解析 `result` 事件中的 `usage` 欄位
- [ ] 提取: `input_tokens`, `cache_read_input_tokens`, `cache_creation_input_tokens`, `output_tokens`
- [ ] 回傳型別從 `Result<String, String>` 改為 `Result<(String, Option<TokenUsage>), String>`
- [ ] 上游 `call_claude_with_env()` / `call_claude()` 適配新簽名

### 0.4 整合到 call_with_rotation()

- [ ] `call_with_rotation()` 接收 `TokenUsage` 回傳值
- [ ] 初始化全域 `CostTelemetry` singleton (類似 ROTATOR_CACHE 模式)
- [ ] 每次成功呼叫後 `telemetry.record(agent_id, request_type, usage)`
- [ ] `call_claude_for_agent()` 傳遞 `agent_id` 和 `request_type` 到 rotation

### 0.5 Cache 效率計算 + 200K 預警

- [ ] `CostTelemetry::agent_cache_efficiency()` — 滑動 1h 平均
- [ ] 低於 30% 時 `warn!()` 日誌 + 建議切換路徑
- [ ] 200K 價格懸崖預警: 在 `call_claude_for_agent()` 入口估算 system_prompt + prompt tokens
- [ ] 超過 180K 時 `warn!()` + 返回建議壓縮
- [ ] CJK token 估算: 複用現有 `estimate_tokens()` 函數

### 0.6 MCP tools

- [ ] `cost_summary` tool: 全域 / per-agent 成本摘要 (1h/24h/7d)
- [ ] `cost_by_agent` tool: 指定 agent 的詳細 cost breakdown
- [ ] 註冊到 MCP server tool list

### 0.7 驗證

- [ ] `cargo build` 全 workspace 編譯通過
- [ ] 單元測試: `TokenUsage::cache_efficiency()` 計算
- [ ] 單元測試: `is_near_price_cliff()` 閾值
- [ ] 單元測試: SQLite CRUD
- [ ] 整合測試: mock stream-json output → 解析 usage

---

## Phase 1: 迴避層 — Confidence Router 強化 (Week 2) ✅ DONE

### 1.1 調低 Cloud 路由閾值

- [x] `inference.toml [router]` — `strong_threshold` 從 0.5 降至 0.35
- [x] `cloud_keywords` 精簡: 僅保留 refactor/architect/security audit/multi-step
- [x] `fast_keywords` 擴充: +13 個 (含 zh-TW: 翻譯/摘要/分類/格式/解釋 等)
- [x] `max_fast_prompt_tokens` 從 500 提高至 1000
- [x] `fast_threshold` 從 0.8 降至 0.7 (更容易命中本地路由)

### 1.2 演化反思本地化

- [x] 建立 `llm_call.py` 共用模組: 先嘗試本地 OpenAI-compat endpoint, 再 fallback CLI
- [x] Micro evolution: 改用 `call_llm()` (prefer_local=True)
- [x] Meso evolution: 改用 `call_llm()` (prefer_local=True)
- [x] Macro evolution: 改用 `call_llm()` (prefer_local=True)
- [x] 消除三個檔案的重複 `_call_claude()` / `_find_claude()`

### 1.3 Request Coalescing

- [x] `dispatcher.rs` — `coalesce_messages()` 合併同 agent 連續訊息
- [x] 合併窗口: 每個 poll cycle (5s) 內的所有訊息
- [x] 合併策略: `[Message N] payload` 以 `---` 分隔連接
- [x] 上限: MAX_COALESCE = 5 條/組, 超過分批
- [x] 保留第一條的 message_id 作為 coalesced ID

### 1.4 CostTelemetry 驅動的自適應降級

- [x] `adaptive_routing_check()` 每小時執行
- [x] `should_prefer_local()` 查詢 runtime override
- [x] cache_eff < 30% (≥3 requests) → 自動啟用 prefer_local
- [x] cache_eff > 70% → 移除 override (cache 恢復)
- [x] `call_claude_for_agent_with_type()` 整合 adaptive override
- [x] 每 30 天自動清理舊 telemetry 記錄

---

## Phase 2: 最佳化層 — Direct API + CLI 瘦身 (Week 3-4) ✅ DONE

### 2.0 演化機制清理 (附帶修復)

- [x] `evolution.rs` — `run_reflections_for_all_agents()` 檢查 `prediction_driven` flag
- [x] `prediction_driven=true` 的 agent 跳過舊 Python meso/macro 計時器
- [x] 避免新舊演化系統同時執行浪費 API token

### 2.1 Direct API client (精簡方案: 不建新 crate, 直接加入 gateway)

- [x] `direct_api.rs` — Direct Anthropic Messages API client (~250 行)
- [x] 完整 API 型別: MessagesRequest, SystemBlock, CacheControl, MessagesResponse, ApiUsage
- [x] `call_direct_api()` — 帶 `cache_control: ephemeral` 的 system prompt
- [x] `normalize_system_prompt()` — 確定性序列化 (移除尾部空白, 合併空行)
- [x] `ApiUsage → TokenUsage` 轉換 + cache_efficiency 計算
- [x] `prompt-caching-2024-07-31` beta header 啟用 cache

### 2.2 cache_control 精確放置

- [x] System prompt → 單一 block + `cache_control: ephemeral`
- [x] 位元組確定性: `normalize_system_prompt()` 消除格式差異
- [x] User message → 無 cache_control (每次變化)
- [x] 預期 cache hit: 95%+ (system prompt 完全由 DuDuClaw 控制)

### 2.3 整合到路由流程

- [x] `ModelConfig.api_mode` 新欄位: "cli" (default) | "direct" | "auto"
- [x] `try_direct_api()` — 取 API key → 呼叫 Direct API → 記錄 telemetry
- [x] "direct" 模式: 僅用 Direct API, 失敗即報錯
- [x] "auto" 模式: 先試 Direct API → 失敗 fallback CLI
- [x] "cli" 模式: 維持現狀 (預設)
- [x] OAuth 帳號自動 fallback CLI (Direct API 需要 API key)

### 2.4 CLI 路徑瘦身

- [x] `build_system_prompt()` — Skills 按名稱排序 (確定性)
- [x] `trim_end()` 所有 section 尾部空白 (減少位元組差異)
- [x] 為 Direct API 路徑複用 `normalize_system_prompt()` 正規化

---

## Phase 2.5: Multi-OAuth 輪替 + Auto 模式修正

### 2.5.0 Auto 模式修正 — CLI 優先

- [ ] `claude_runner.rs` — Auto 模式改為: CLI(OAuth) 優先 → rate limit 才 fallback Direct API(API Key)
- [ ] `is_billing_error()` 和 `is_rate_limit_error()` 分離: rate limit = 切帳號, billing = 長 cooldown
- [ ] Direct API 路徑僅在 api_mode="direct" 或所有 OAuth 帳號 rate limited 時啟用

### 2.5.1 OAuth Token 注入

- [ ] `account_rotator.rs` — Account struct 新增 `oauth_token: Option<String>` 欄位
- [ ] OAuth 帳號選中時, env vars 加入 `CLAUDE_CODE_OAUTH_TOKEN`
- [ ] 預設帳號 (OS keychain) 不設 `CLAUDE_CODE_OAUTH_TOKEN` (讓 CLI 自己讀 keychain)
- [ ] `setup-token` 產生的帳號透過 env var 注入

### 2.5.2 config.toml `[[accounts]]` 擴充

- [ ] `oauth_token_enc` 欄位: AES-256-GCM 加密的 setup-token JSON
- [ ] `expires_at` 欄位: token 過期時間 (ISO 8601)
- [ ] `label` 欄位: 使用者自訂標籤 (e.g. "工作帳號")
- [ ] `load_from_config()` 解析新欄位, 解密 token

### 2.5.3 Token 過期檢查 + 提醒

- [ ] `Account::is_available()` 檢查 `expires_at`, 過期 → `is_healthy = false`
- [ ] Gateway 啟動時掃描所有帳號, <30 天 → `warn!()`, <7 天 → `warn!()` 醒目
- [ ] `accounts.list` 回傳 `expires_at` + `days_until_expiry` 欄位
- [ ] Dashboard 帳號卡片顯示到期倒數

### 2.5.4 Dashboard 帳號管理 RPC

- [ ] `accounts.add_oauth` — 接收 token 字串, 驗證有效性, 加密儲存
- [ ] `accounts.remove` — 移除指定帳號
- [ ] `accounts.verify` — 用 token 呼叫 `claude auth status` 確認有效
- [ ] `accounts.list` — 回傳完整帳號清單 (含 OAuth 到期時間)

### 2.5.5 Onboard 更新

- [ ] 偵測到本機 OAuth 時, 提示可用 `setup-token` 新增更多帳號
- [ ] 移除對 `.credentials.json` 的依賴 (已在 v0.8.6 完成)

### 2.5.6 驗證

- [ ] 編譯通過
- [ ] 單元測試: OAuth token 注入 env var
- [ ] 單元測試: token 過期檢查
- [ ] 整合測試: rate limit → 自動切換帳號

---

## Phase 3: 復用層 — Subprocess Pool (Week 5-6)

### 3.1 SubprocessPool struct

- [ ] `crates/duduclaw-gateway/src/subprocess_pool.rs` — Pool 管理器
- [ ] `PoolEntry`: `child: Child, agent_id: String, created_at: Instant, last_used: Instant`
- [ ] `acquire(agent_id)` → 復用 warm proc 或 spawn 新的
- [ ] `release(agent_id)` → 放回 pool
- [ ] `max_pool_size` 可配置 (config.toml `[pool]`)

### 3.2 Interactive mode 支援

- [ ] `claude` CLI interactive mode 探索 (stdin/stdout 持久連線)
- [ ] JSON-RPC 或 line-delimited JSON 協議研究
- [ ] 替代方案: `claude --session-id` 復用 session

### 3.3 Session Pinning

- [ ] per-agent 綁定專屬進程
- [ ] Agent config 變更 → graceful restart 對應進程
- [ ] Idle 回收: 5 分鐘無請求 → kill proc
- [ ] Health check: 30 秒 ping，偵測僵死 proc

### 3.4 整合到現有路由

- [ ] `call_claude_for_agent()` → `pool.acquire()` 替代 `prepare_claude_cmd()`
- [ ] 流式解析邏輯不變
- [ ] CostTelemetry 追蹤 pool hit/miss rate

---

## Phase 4: V2 Session 遷移 (V2 API 穩定後)

### 4.1 Python bridge 改造

- [ ] `chat.py` 從每次 spawn 改為長駐 session daemon
- [ ] `createSession()` 持久化
- [ ] stdin/stdout JSON-RPC 通訊

### 4.2 CabLate 五點補丁

- [ ] `settingSources` 載入 CLAUDE.md
- [ ] `cwd` 指定 agent 工作目錄
- [ ] 提取 `thinkingConfig`, `maxTurns`, `maxBudgetUsd`
- [ ] 透傳 `extraArgs`
- [ ] MCP server 程序內路由

### 4.3 Pool 底層替換

- [ ] SubprocessPool 底層從 CLI proc 換成 V2 session
- [ ] 上層 API 不變
- [ ] 測試: cache 效率 > 80%

---

## 預期效果

| 指標                    | 現狀    | Phase 0 | Phase 1  | Phase 2    | Phase 3   |
|-------------------------|---------|---------|----------|------------|-----------|
| Cloud API 呼叫量        | 100%    | 100%    | 30-50%   | 30-50%     | 30-50%    |
| Cache 效率 (CLI)        | 25%     | 25% (可觀測) | 25%  | 25-40%     | 60-84%    |
| Cache 效率 (Direct API) | N/A     | N/A     | N/A      | 95%+       | 95%+      |
| 每則訊息配額消耗        | 2-3%    | 2-3%    | 1-1.5%   | 0.3-0.5%   | < 0.3%    |
| 200K 價格懸崖           | 無預警  | 自動偵測 | 自動偵測 | 自動偵測   | 自動偵測  |
| 成本可觀測性            | 僅總額  | per-agent 細分 | + 自適應 | + 路徑細分 | + pool 統計 |
