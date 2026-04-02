# TODO: 主動性系統（Proactive Agent）

> 開始日期：2026-04-02 | 專案版本：v0.12.0+
> 架構：H（排程執行）+ E（合約驅動）+ D（需求預測）+ F（事件通知）
> 目標：零 LLM 成本主動決策 + 有事回報/無事靜默 + 合約邊界保護
> 狀態：✅ 全部完成 — H1+H2+H3+E1+E2+F1+F2+D1+D2+D3+H4

---

## 現有基礎

| 元件 | 狀態 | 缺什麼 |
|------|------|--------|
| HeartbeatScheduler | ✅ per-agent cron/interval | 只輪詢 bus，不面向用戶 |
| CronScheduler | ✅ cron_tasks.jsonl | 無靜默機制，結果不路由到頻道 |
| Prediction Engine | ✅ 預測滿意度/話題 | 不預測用戶需求 |
| MetaCognition | ✅ 自校準閾值 | 不管主動性閾值 |
| CONTRACT.toml | ✅ must_not / must_always | 未衍生主動規則 |
| Webhook | ✅ HMAC 簽章 + bus_queue | 無即時通知路徑 |
| Shared Memory | ✅ 跨 Agent 可見性 | 未用於主動觸發 |
| MCP Tools | ✅ 30+ 工具（web_fetch/Odoo/send_message） | heartbeat 未帶工具集 |

---

## Phase H：PROACTIVE.md 排程執行引擎

### H1. PROACTIVE.md 檔案格式 + 載入 [P0]

- [ ] 定義 `PROACTIVE.md` 規格（放在 agent 目錄，與 SOUL.md 同級）
  ```markdown
  # Proactive Checks

  ## Every 30 minutes
  - Check website https://example.com/price for price changes
  - If price dropped >5%, notify user immediately

  ## Every day at 09:00
  - Summarize yesterday's Odoo orders using odoo_search
  - Report total revenue and any anomalies

  ## Every hour (quiet: 23:00-08:00)
  - Run `system.doctor` and check for warnings
  - Only report if there are failures

  ## Rules
  - If nothing to report, respond with PROACTIVE_OK
  - Never send more than 3 proactive messages per hour
  - Respect user's timezone (Asia/Taipei)
  ```
- [ ] `crates/duduclaw-agent/src/proactive.rs` — 新增模組
  - [ ] `ProactiveConfig` struct（從 agent.toml `[proactive]` 讀取）
    - `enabled: bool`
    - `check_interval: String` (cron 表達式)
    - `quiet_hours: Option<(u8, u8)>` (開始, 結束)
    - `max_messages_per_hour: u32` (預設 3)
    - `token_budget_per_check: u32` (預設 2000)
  - [ ] `load_proactive_md(agent_dir: &Path) -> Option<String>`
  - [ ] `is_quiet_hour(quiet_hours, timezone) -> bool`
- [ ] `agent.toml` 範例更新（templates/ 各產業）

### H2. 執行引擎 — Heartbeat 整合 [P0]

- [ ] `crates/duduclaw-agent/src/heartbeat.rs` — 擴充 heartbeat 回呼
  - [ ] heartbeat 觸發時檢查 `PROACTIVE.md` 是否存在
  - [ ] 若存在 → 建構 system prompt = PROACTIVE.md 內容
  - [ ] 呼叫 Claude CLI / Direct API（帶完整 MCP 工具集）
  - [ ] 注入 `lightContext` 模式（不帶對話歷史，降低 token 消耗）
  - [ ] 靜默時段檢查（quiet hours → 跳過）
- [ ] `crates/duduclaw-gateway/src/handlers.rs` — 新增 RPC 方法
  - [ ] `proactive.status` — 查詢各 Agent 的主動性狀態（上次檢查、下次排程、訊息計數）
  - [ ] `proactive.trigger` — 手動觸發一次主動檢查（測試用）

### H3. 結果路由 — 有事回報/無事靜默 [P0]

- [ ] 結果解析邏輯
  - [ ] 偵測 `PROACTIVE_OK` 關鍵字 → 丟棄，不通知用戶
  - [ ] 非 `PROACTIVE_OK` → 視為需要通知
- [ ] 通知投遞
  - [ ] 從 agent.toml `[proactive].notify_channel` 讀取目標頻道（telegram/line/discord）
  - [ ] 從 agent.toml `[proactive].notify_chat_id` 讀取目標 chat/group ID
  - [ ] 呼叫對應頻道的 send API（復用現有 MCP `send_message`）
- [ ] 限流
  - [ ] 每小時最多 `max_messages_per_hour` 則主動訊息
  - [ ] 記錄 `last_proactive_messages: Vec<Instant>` 做滑動視窗計數
- [ ] Token 預算
  - [ ] 每次檢查前檢查 `token_budget_per_check`
  - [ ] 超預算 → 中止並記錄 warn

### H4. Dashboard UI [P1]

- [ ] `web/src/pages/AgentsPage.tsx` — Agent 卡片加入主動性狀態指示器
  - [ ] 圖示：主動性 ON/OFF
  - [ ] 上次檢查時間 + 下次排程
  - [ ] 今日已發送主動訊息數
- [ ] `web/src/i18n/en.json` + `zh-TW.json` — proactive.* i18n 鍵值

---

## Phase E：CONTRACT.toml must_always 驅動

### E1. Contract Analyzer [P1]

- [ ] `crates/duduclaw-agent/src/proactive.rs` — 擴充
  - [ ] `extract_proactive_rules(contract: &Contract) -> Vec<ProactiveRule>`
  - [ ] 從 `must_always` 規則中識別可主動化的模式：
    - "greet returning customers" → TimeBased（3 天未互動）
    - "escalate angry customers after 2" → PatternBased（偵測未解決升級）
    - "flag orders above $50K" → EventBased（Odoo webhook）
    - "confirm reservation before finalizing" → TimeBased（預約前 2 小時）
  - [ ] `ProactiveRule` struct:
    ```rust
    struct ProactiveRule {
        trigger: ProactiveTrigger,  // TimeBased / EventBased / PatternBased
        condition: String,
        action: ProactiveAction,    // SendMessage / NotifyManager / InternalAlert
        source_contract: String,
        cooldown: Duration,
    }
    ```

### E2. 規則評估引擎 [P1]

- [ ] heartbeat 週期中評估所有 ProactiveRules
- [ ] 確定性規則（零 LLM）：時間比對、閾值比對
- [ ] 需要 LLM 判斷的規則：丟入 Claude 評估（受 token_budget 限制）
- [ ] must_not 邊界保護：每條主動訊息在送出前通過 GVU Verifier L1 檢查

---

## Phase D：Prediction Engine 需求預測

### D1. UserModel 延伸 [P2]

- [ ] `crates/duduclaw-gateway/src/prediction/engine.rs` — 擴充 UserModel
  - [ ] `next_topic_prediction: Option<String>` — 預測下次話題
  - [ ] `predicted_return_hours: Option<f32>` — 預測回訪時間
  - [ ] `proactive_score: f32` — 主動介入適合度（0.0-1.0）
  - [ ] 每次對話結束時更新預測

### D2. 需求預測邏輯 [P2]

- [ ] 在 Prediction Engine 中新增：
  - [ ] `predict_next_need(user_id, agent_id) -> Option<ProactiveCandidate>`
  - [ ] 基於歷史對話模式統計（零 LLM）：
    - 用戶通常在週一早上問庫存 → 週一 8:50 主動提供
    - 用戶每次下單後 3 天會問物流 → 第 3 天主動告知
    - 用戶上次提到「下週要出差」→ 出差前提醒

### D3. MetaCognition 主動性自校準 [P2]

- [ ] 延伸 MetaCognition 追蹤主動訊息的接受/拒絕率
- [ ] 自動調整 `proactive_threshold`：
  - 用戶經常互動（接受率高）→ 降低閾值（更主動）
  - 用戶經常忽略（接受率低）→ 提高閾值（更安靜）
- [ ] 每 20 次主動行為自校準一次

---

## Phase F：Webhook 事件即時通知

### F1. 事件通知路由 [P1]

- [ ] `crates/duduclaw-gateway/src/webhook.rs` — 擴充
  - [ ] 新增 `[webhook.notify]` 設定：event_type → channel + chat_id 映射
  - [ ] webhook 收到事件後：
    - 先走 bus_queue（現有行為，不變）
    - 同時檢查 notify 映射 → 直接推送即時通知
- [ ] 通知模板
  - [ ] `~/.duduclaw/templates/notifications/` 目錄
  - [ ] Mustache/Handlebars 簡單模板（零 LLM）
  - [ ] 無模板時 fallback 到 LLM 生成

### F2. Odoo 事件橋接 [P2]

- [ ] `crates/duduclaw-odoo/` — 擴充事件輪詢
  - [ ] 庫存低於安全水位 → 主動通知
  - [ ] 新訂單 > 閾值 → 通知主管
  - [ ] 付款逾期 > N 天 → 提醒跟進
- [ ] 事件去重：相同事件 24h 內不重複通知

---

## 實作優先順序

| 順序 | Phase | 工項 | 預估時間 | 依賴 |
|------|-------|------|---------|------|
| 1 | H1 | PROACTIVE.md 格式 + 載入 | 2 天 | 無 |
| 2 | H2 | Heartbeat 整合執行引擎 | 3 天 | H1 |
| 3 | H3 | 結果路由 + 靜默 + 限流 | 2 天 | H2 |
| 4 | E1 | Contract Analyzer | 2 天 | H3 |
| 5 | F1 | Webhook 即時通知路由 | 2 天 | H3 |
| 6 | E2 | 規則評估引擎 | 2 天 | E1 |
| 7 | H4 | Dashboard UI | 2 天 | H3 |
| 8 | D1 | UserModel 延伸 | 2 天 | H3 |
| 9 | D2 | 需求預測邏輯 | 3 天 | D1 |
| 10 | D3 | MetaCognition 自校準 | 2 天 | D2 |
| 11 | F2 | Odoo 事件橋接 | 2 天 | F1 |
