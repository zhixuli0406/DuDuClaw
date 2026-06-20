# 第二輪深度審查 — Dashboard 設定覆蓋率 Gap Analysis（2026-06-19）

> 目標:確保**所有**設定項都能在 dashboard 設定。本報告交叉比對「設定面總清單(~340 欄位)」與「dashboard 經 gateway 可寫入的設定面」,列出**無法在 dashboard 設定的缺口**,並給出補齊優先序。
> 方法:兩個並行 agent 分別盤點 (A) 所有 config struct + toml,(B) gateway write-surface(handlers.rs RPC + server.rs REST)+ web 表單;再以 grep 驗證關鍵「整段缺失」宣稱(`computer_use`/`pty_pool`/`runtime`/`killswitch`/`mcp_keys`/`ptc`/`cultural_context`/`worktree`/`external_factors` 在 handlers.rs 與 web 皆 0 命中)。

---

## 總覽

| 設定來源 | 總欄位(約) | 可在 dashboard 設定 | 覆蓋率 |
|---|---|---|---|
| 全域 `config.toml` | ~95 | channels(大部分)、accounts(部分)、odoo(全)、logging.level、rotation.strategy、gateway.auto_update | ~45% |
| 每-agent `agent.toml` | ~125 | identity/model(部分)/budget/heartbeat(部分)/proactive/permissions/container(部分)/evolution(部分)/channels/sticker | ~45% |
| `inference.toml` | ~70 | **僅 `[voice]`** | ~7% |
| `CONTRACT.toml` | 3 | 無 | 0% |
| `KILLSWITCH.toml` | ~20 | 無 | 0% |
| governance `policies/*.yaml` | ~22 | 無 | 0% |
| `.scope.toml`(wiki 命名空間) | ~3 | 無 | 0% |
| **合計** | **~340** | **~130** | **~38%** |

**結論:約 6 成的設定項目前無法在 dashboard 設定**,其中包含數個**安全關鍵**面向(capabilities、CONTRACT、KILLSWITCH、redaction、mcp_keys)。

---

## A. 每-agent `agent.toml` 缺口(最高價值 — 核心 agent 行為)

`agents.update` RPC 已涵蓋 identity / model(preferred,fallback,api_mode,local.*)/ budget / heartbeat(enabled,interval,cron)/ proactive / permissions / container(timeout,max_concurrent,sandbox,network,readonly)/ evolution(部分)/ channels / sticker。**未涵蓋:**

### A1. `[capabilities]` — ❌ 完全缺失(🔴 安全關鍵)
`computer_use`、`computer_use_mode`、`browser_via_bash`、`allowed_tools`、`denied_tools`、`wiki_visible_to`,以及 `[capabilities.computer_use_config]`(allowed_apps / blocked_actions / max_session_minutes / max_actions / display_width / display_height / auto_confirm_trusted)。
> 影響:操作者無法在 dashboard 限制 sub-agent 的工具權限(對應第一輪 HS12 修復的 `allowed_tools`)、無法開關 computer-use / browser。目前只能手改 agent.toml。

### A2. `[runtime]` — ❌ 完全缺失
`provider`(claude/codex/gemini/openai_compat)、`fallback`、`pty_pool_enabled`、`worker_managed`。
> 影響:多-runtime 切換、互動式 PTY pool、out-of-process worker 這些招牌功能完全無法在 dashboard 開啟。

### A3. `[container]` 進階 — ❌ 部分缺失
`additional_mounts`、`worktree_enabled`、`worktree_auto_merge`、`worktree_cleanup_on_exit`、`worktree_copy_files`、`cmd`、`env`。(只有 timeout/max_concurrent/sandbox/network/readonly 可設)

### A4. `[evolution]` 進階 — ❌ 部分缺失
`external_factors`(user_feedback/security_events/channel_metrics/business_context/peer_signals)、skill-synthesis(enabled/threshold/cooldown/ttl)、graduation(enabled/min_lift)、recommendation、curiosity(enabled/threshold/max_daily)、behavior monitor、`[evolution.stagnation_detection]`(RPC 支援但 UI 無輸入)。

### A5. 其餘整段缺失
- `[ptc]`(Programmatic Tool Calling):enabled / allowed_tools / max_output_tokens / timeout_seconds
- `[prompt]`:mode / minimal_core_kb / cli_bare_mode
- `[cultural_context]`:locale / high_context / short_reply_threshold / 權重
- `[odoo]` per-agent override:profile / allowed_models / allowed_actions / company_ids(只有全域 odoo.configure)

### A6. 散落欄位
`[model].account_pool`、`[model].utility`、`[heartbeat].max_concurrent_runs`、`[heartbeat].cron_timezone`、`[proactive].token_budget_per_check`/`timezone`/`max_turns`、`[agent].status`(RPC 支援、UI 無下拉)。

---

## B. 全域 `config.toml` 缺口

### B1. `[mcp_keys]` — ❌ 完全缺失(🔴 安全)
MCP API key 的建立/撤銷/scope 設定(client_id / is_external / created_at / scopes)。目前只能手改 config.toml,無法在 dashboard 發/撤 MCP 金鑰。

### B2. `[secret_manager]` — ❌ 完全缺失
backend / vault_addr / vault_token / vault_mount。

### B3. `[redaction]` — ❌ 完全缺失(🔴 安全/隱私)
enabled / vault_ttl_hours / purge_after_expire_days / profiles / `[redaction.sources]`(per-source on/off/selective)/ `[redaction.tool_egress.<tool>]`(restore/passthrough/deny + audit_reveal)/ inline rules。
> 影響:PII redaction 是隱私核心功能,卻完全無法在 dashboard 開關或調整。

### B4. 散落欄位
- `[gateway]`:`bind` / `port` / `auth_token`(❌,僅 auto_update 可設)
- `[rotation]`:`health_check_interval_seconds` / `cooldown_after_rate_limit_seconds`(❌,僅 strategy 可設)
- `[general]`:`default_agent` / `inference_mode`(❌)
- `[logging]`:`format`(❌,僅 level 可設)
- `[[accounts]]`:新增後只有 `monthly_budget_cents` 可改;`priority`/`tags`/`profile`/`email`/`subscription`/`label` 無法編輯
- `[channels]` 全域:`whatsapp_verify_token` / `whatsapp_app_secret` / `feishu_verification_token`(channels.add 未涵蓋這幾個)

---

## C. `inference.toml` — ❌ 幾乎完全缺失(僅 `[voice]` 可設)

整個本地推論 / 路由 / 壓縮設定(~65 欄位)無法在 dashboard 設定:
- root:enabled / backend / models_dir / default_model / auto_load / max_memory_mb
- `[generation]`、`[openai_compat]`、`[mistralrs]`(+speculative)、`[router]`(fast/strong threshold、keywords)、`[exo]`、`[llamafile]`、`[mlx]`、`[llmlingua]`、`[streaming_llm]`、`[embedding]`
> 註:有 MCP 工具(`model_load`/`llamafile_start`/`inference_mode`)可在 runtime 操作,但屬 agent-facing 且非持久化設定;dashboard 無對應持久化設定面。

---

## D. 獨立設定檔 — ❌ 完全缺失

- **`CONTRACT.toml`**(🔴 安全):`must_not` / `must_always` / `max_tool_calls_per_turn` — agent 行為邊界,無法在 dashboard 設定。
- **`KILLSWITCH.toml`**(🔴 安全):triggers / circuit_breaker / failsafe / safety_words / defensive_prompt / audit — 緊急停機設定全無 UI。
- **governance `policies/*.yaml`**:Rate / Permission / Quota / Lifecycle policy 無 UI(僅 dashboard RPC `autopilot.*` 與此不同;governance 完全沒有設定面)。
- **`.scope.toml`**:wiki 命名空間 SoT policy(agent_writable / read_only / operator_only)無 UI。

---

## 已存在但缺 UI 的後端寫入方法(low-hanging fruit)

這些 gateway RPC 已實作、`api.ts` 已接,但**沒有頁面呼叫**:
- `users.remove`、`users.unbind_agent`(只接了 offboard / bind)
- `partner.customer.update`、`partner.customer.delete`(只接了 add)
- `tasks.assign`(UI 改用 tasks.update)
- `agents.update` 內已支援但 UI 無輸入框:`status`、`max_gvu_generations`、`observation_period_hours`、`skill_token_budget`、`whatsapp_app_secret`、`feishu_verification_token`、`[evolution.stagnation_detection]`

---

## 順帶發現(一致性 / 安全)

1. **MCP OAuth token 明文存放** `~/.duduclaw/mcp-oauth-tokens.json`(access/refresh),與其他 channel/API token 的 AES-256-GCM 不一致。
2. 全域 `channels.add`/`remove`、`agents.create` 為**非原子寫入**(partial-write 風險),與 per-agent/accounts/odoo 的原子寫入不一致。
3. `whatsapp_phone_number_id` 在全域 channels.add 被加密、在 agents.update 卻明文(heuristic 不一致)。
4. `skills.adopt` 未驗證 `target_agent_id`(其他 agent-targeting handler 都有驗)。
5. `secret_manager.vault_token` 目前明文(加密變體保留未接線)。
6. PartnerPortalPage 的 license 產生表單是 client-side stub(`api.partner.generateLicense` 永遠 reject,無 gateway 方法)。

---

## 建議補齊優先序

**P0 — 安全關鍵(應優先補)**
1. 每-agent `[capabilities]` 編輯(allowed_tools/denied_tools/computer_use/browser_via_bash/wiki_visible_to + computer_use_config)→ 擴充 `agents.update` + AgentsPage 新增「Capabilities」分頁。
2. `CONTRACT.toml` 編輯(must_not/must_always/max_tool_calls_per_turn)→ 新 RPC `contract.get/update` + AgentsPage「Contract」分頁。
3. `[redaction]` 全域開關 + sources/tool_egress → 新 RPC `redaction.get/update` + SettingsPage「Privacy/Redaction」分頁。
4. `[mcp_keys]` 發/撤/scope → 新 RPC `mcp_keys.list/create/revoke` + 新頁。
5. `KILLSWITCH.toml` 編輯 → 新 RPC + SecurityPage(目前唯讀)加表單。

**P1 — 招牌功能可用性**
6. 每-agent `[runtime]`(provider / pty_pool_enabled / worker_managed)→ 擴 `agents.update` + AgentsPage「Runtime」分頁。
7. `inference.toml` 設定面(至少 backend/default_model/router thresholds/exo/llamafile)→ 新 RPC `inference.get/update` + 新「Inference」頁。
8. `[evolution]` 進階(external_factors / synthesis / curiosity)→ 擴 `agents.update` evolution 分頁。
9. `[container]` 進階(worktree_* / additional_mounts)→ 擴 `agents.update`。

**P2 — 完整度 / 一致性**
10. governance policies 編輯頁;`.scope.toml` 編輯;per-agent `[odoo]` override。
11. 散落全域欄位(rotation intervals / general.default_agent/inference_mode / logging.format / accounts 其餘欄位 / channels 缺的 3 個 token)。
12. 補缺 UI 的既有 RPC(users.remove/unbind、partner.customer.update/delete)+ agents.update 已支援但無輸入框的欄位。
13. 修一致性問題(#1 OAuth token 加密、#2 原子寫入、#3 phone_number_id 加密一致)。

---

## 附註:哪些「不在 dashboard」是合理設計?
- **License 啟用**:目前 CLI-only(`duduclaw` CLI),屬刻意設計;若要 dashboard 化需另開 REST。
- **`[memory]`(agent.toml)**:legacy、runtime 忽略,不需補。
- **環境變數覆寫**(RUST_LOG / DUDUCLAW_* / ANTHROPIC_API_KEY 等):屬部署層,通常不在 dashboard。
- 其餘(capabilities / contract / killswitch / redaction / runtime / inference / mcp_keys / governance)**應該**要能在 dashboard 設定,目前是真缺口。
