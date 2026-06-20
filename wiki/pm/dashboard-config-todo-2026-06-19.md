# Dashboard 設定覆蓋 — 最細工項 TODO（第二輪審查）

> **狀態(2026-06-19):✅ P0~P2 全數完成(75/75)。** dashboard 現可設定先前缺漏的全部設定域。
> 執行:後端依 Phase 序列化(`handlers.rs` 單檔,避免衝突)、前端在對應後端綠燈後進行、web-PX 與 gateway-P(X+1) 並行。各 Phase 完成即更新本文件。
> 驗證:gateway lib `handlers` 測試(P0 26 → P1 47 → P2 64 pass,逐步累加)、`mcp_oauth` 2 pass;web `npm run build`(tsc+vite)綠燈、`vitest` 28 pass。最終整合 build:gateway + web 皆綠。
> 新增 dashboard 設定面:`[capabilities]`/`CONTRACT.toml`/`[redaction]`/`[mcp_keys]`/`KILLSWITCH.toml`(P0)、`[runtime]`/`inference.toml`/`[evolution]` 進階/`[container]` 進階(P1)、governance policies/`.scope.toml`/per-agent `[odoo]`/全域散落欄位/補缺 UI/一致性修正(含 MCP OAuth token 加密、原子寫、skills.adopt 驗證)(P2)。
> ~~已知後續 TODO~~ **已補齊(2026-06-19)**:讀取端解密已實作 — `duduclaw-security` 新增 `keyfile::decrypt_keyfile_value(enc, home_dir)` + `SecretManagerConfig::resolved_vault_token(home_dir)`;`duduclaw-inference` 的 `OpenAiCompatConfig` 新增 `api_key_enc` 欄位 + `resolved_api_key(home_dir)`,並接進 `openai_compat` backend(home_dir 經 `InferenceEngine` 串接)。security 202 tests / inference 57 tests / gateway build 皆綠。
> (更新:Vault adapter 已**接進機密讀取主路徑** — `SecretManagerConfig::build_manager`/`build_manager_for(backend, home_dir)` factory 建構 Local/Env/Vault adapter(`vault` 用 `resolved_vault_token` 解密 `vault_token_enc` 認證)。gateway 的 async chokepoint `config_crypto::read_encrypted_config_field` 新增 `secret://<backend>/<name>` 間接參照解析:欄位值若是 `secret://vault/<name>` 即透過 SecretManager 從 Vault 取(參照自帶 backend,優先於預設);否則照舊解 `_enc`/plaintext(零破壞、opt-in 逐欄位)。此 chokepoint 涵蓋**全部 channel token + `api.anthropic_api_key`**。fail-soft:參照無效/Vault 取失敗 → 記 warn 並視為未設定。security 36 tests / gateway build 皆綠。account_rotator / Odoo 的獨立解密路徑留待後續以統一 resolver 收斂。)
> **統一收斂(2026-06-19 完成)**:全 codebase 的 keyfile-AES 解密已收斂到單一 primitive `duduclaw_security::keyfile::decrypt_keyfile_value`(原本 4 份重複:gateway config_crypto / gateway reminder_scheduler / agent account_rotator / cli odoo,現皆 delegate;重複的 `decrypt_with_keyfile`、`load_crypto_engine` 已刪)。`secret://` 解析收斂到單一 async resolver `duduclaw_security::secret_manager::resolve_secret_reference(value, &sm_cfg, home_dir)`,gateway(channels + anthropic key)+ account_rotator(api_key/oauth)皆走它。~~Odoo 與 reminder channel token 暫以 `// TODO`~~ **已接上(2026-06-20)**:(a) reminder `decrypt_channel_token` 改 async,plaintext 為 `secret://` 時走 `resolve_secret_reference`(3 個呼叫點 await);(b) Odoo `OdooConnectorPool::{get_or_connect, merge_credentials}` 的 sync decrypt closure 改 async(`Fn(String)->Future`),`api_key_enc`/`password_enc` 重新詮釋為「憑證來源」(AES 密文 *或* `secret://` 參照),`handle_odoo_connect` 傳入 async resolver(`secret://`→SecretManager,否則 keyfile 解密),probe site 維持「未快取即錯」語意;(c) gateway `apply_odoo_to_table` 偵測 `secret://` 即原樣存入 `*_enc`(不加密)。驗證:odoo_pool 29 / reminder 5 / odo_apply 3 tests 綠,gateway+cli build rc=0。
> **cli `send_message` handler 也已接上(2026-06-20)**:`mcp.rs` 的 `decrypt_channel_token` 改 async + `secret://` 解析,5 個呼叫點(send_message telegram/line/discord + send_photo/sticker 的 telegram/discord)全部更新(`.map(...)` 改 async `match`)。cli build rc=0。
> **最終覆蓋**:所有 channel token + 憑證讀取路徑(gateway channel 啟動 / Anthropic key / account_rotator OAuth+API key / reminder / Odoo / cli send_message)皆支援 `secret://<backend>/<name>`,共用單一 async resolver `resolve_secret_reference` + 單一 keyfile primitive `decrypt_keyfile_value`。
>
> 來源:[dashboard-config-coverage-2026-06-19.md](../reports/dashboard-config-coverage-2026-06-19.md)。目標:讓**所有**設定項都能在 dashboard 設定。
> 編號:`CAP`=capabilities、`CON`=contract、`RED`=redaction、`MK`=mcp_keys、`KS`=killswitch、`RT`=runtime、`INF`=inference、`EVO`=evolution 進階、`CT`=container 進階、`ODO`=per-agent odoo、`GOV`=governance、`SCP`=.scope.toml、`G`=全域散落欄位、`UI`=既有後端缺 UI、`XC`=一致性/安全修正。
> 每個功能的標準切法(原子工項):**①gateway RPC handler(讀+寫,secret 加密、原子寫、role/scope gate)→ ②`web/src/lib/api.ts` 方法 + store → ③web 表單/頁面 → ④i18n(zh-TW/en/ja)→ ⑤測試**。
> 慣例:寫 config.toml/agent.toml 要走既有原子寫(`update_agent_toml` / 原子 temp+rename);secret 欄位用 `config_crypto::encrypt_value`,且**永不**回傳明文(沿用 `_enc` + skip_serializing 模式);新 RPC 一律加 role gate(比照 `require_role!`/`check_agent!`)。

---

## Phase P0 — 安全關鍵(最優先)

### CAP — 每-agent `[capabilities]` 編輯 🔴
擴充 `agents.update`(handlers.rs ~1261 的 field map)+ AgentsPage 新「Capabilities」分頁。
- [x] CAP.1 gateway:`update_agent_toml` 支援寫 `[capabilities]` 純量:`computer_use`(bool)、`computer_use_mode`(container/native/auto)、`browser_via_bash`(bool)
- [x] CAP.2 gateway:支援陣列欄位 `allowed_tools`、`denied_tools`、`wiki_visible_to`(TOML array)
- [x] CAP.3 gateway:支援 `[capabilities.computer_use_config]` 子表(allowed_apps/blocked_actions/max_session_minutes/max_actions/display_width/display_height/auto_confirm_trusted)
- [x] CAP.4 gateway:寫入前驗證(mode enum、display 尺寸範圍、tool 名稱非空)+ role gate(Admin/Manager)
- [x] CAP.5 web:`api.ts` 在 `agents.update` params 增 capabilities 欄位;agents-store 帶入
- [x] CAP.6 web:AgentsPage 新增「Capabilities」分頁(toggle + tool chips 編輯器 + computer_use_config 表單)
- [x] CAP.7 i18n:capabilities 相關 key(zh-TW/en/ja)
- [x] CAP.8 測試:gateway round-trip 寫回 agent.toml 正確;allowed_tools 設定後 spawn 帶 `--allowedTools`(對齊第一輪 HS12)

### CON — `CONTRACT.toml` 編輯 🔴
新 RPC + AgentsPage「Contract」分頁。
- [x] CON.1 gateway:新 RPC `contract.get`(讀 `agents/<id>/CONTRACT.toml` → must_not/must_always/max_tool_calls_per_turn)
- [x] CON.2 gateway:新 RPC `contract.update`(原子寫 CONTRACT.toml;陣列項逐行;max_tool_calls_per_turn 範圍驗證)+ role gate
- [x] CON.3 gateway:寫入後觸發 contract 重新載入(若 runtime 有快取)或註記下次載入生效
- [x] CON.4 web:`api.ts` `contract.get/update` + store
- [x] CON.5 web:AgentsPage「Contract」分頁(must_not / must_always 多行清單編輯 + 數字輸入)
- [x] CON.6 i18n + [x] CON.7 測試(空/多行/非法數字)

### RED — 全域 `[redaction]` 設定 🔴
新 RPC + SettingsPage「Privacy / Redaction」分頁。
- [x] RED.1 gateway:新 RPC `redaction.get`(讀 config.toml `[redaction]`:enabled/vault_ttl_hours/purge_after_expire_days/profiles)
- [x] RED.2 gateway:`redaction.update`(原子寫 enabled/ttl/purge/profiles 陣列)+ role gate
- [x] RED.3 gateway:`[redaction.sources]` per-source 模式(user_input/tool_results/system_prompt/sub_agent/cron_context = on/off/selective/inherit)讀寫
- [x] RED.4 gateway:`[redaction.tool_egress.<tool>]`(restore/passthrough/deny + audit_reveal)增刪改
- [x] RED.5 web:`api.ts` redaction.* + store;SettingsPage 新「Privacy/Redaction」分頁(master toggle + sources 矩陣 + egress 規則表)
- [x] RED.6 i18n + [x] RED.7 測試(toggle round-trip、egress 規則寫回 profile 不破壞)

### MK — `[mcp_keys]` 發/撤/scope 🔴
新 RPC + 新「MCP Keys」頁(或併入現有 McpPage)。
- [x] MK.1 gateway:`mcp_keys.list`(回傳 client_id/is_external/created_at/scopes —**不回傳金鑰明文**,只回 masked/前綴)
- [x] MK.2 gateway:`mcp_keys.create`(產生 `ddc_(prod|staging|dev)_<32hex>` 格式金鑰、寫 config.toml `[mcp_keys.<key>]`、回傳一次性明文)+ role gate(Admin)
- [x] MK.3 gateway:`mcp_keys.revoke`(刪除該 key 表項)
- [x] MK.4 gateway:scope 驗證(只允許已知 scope 字串);30 天輪替提示
- [x] MK.5 web:`api.ts` mcp_keys.* + 頁面(列表 + 建立對話框「一次性顯示金鑰」+ 撤銷 + scope 多選)
- [x] MK.6 i18n + [x] MK.7 測試(建立→list 出現 masked、revoke 後消失、scope 驗證)

### KS — `KILLSWITCH.toml` 編輯 🔴
新 RPC + SecurityPage(目前唯讀)加表單。
- [x] KS.1 gateway:`killswitch.get`(讀 `agents/<id>/KILLSWITCH.toml` 或全域:triggers/circuit_breaker/failsafe/safety_words/defensive_prompt/audit)
- [x] KS.2 gateway:`killswitch.update`(原子寫;數值範圍驗證 — rate/threshold/cooldown)+ role gate(Admin)
- [x] KS.3 web:`api.ts` killswitch.* + SecurityPage 新增「Killswitch」設定區(triggers/circuit_breaker 數值 + safety_words 清單 + defensive_prompt 開關)
- [x] KS.4 i18n + [x] KS.5 測試(範圍驗證、safety_words round-trip)

---

## Phase P1 — 招牌功能可用性

### RT — 每-agent `[runtime]` 🟠
擴 `agents.update` + AgentsPage「Runtime」分頁。
- [x] RT.1 gateway:`update_agent_toml` 支援 `[runtime]`:provider(claude/codex/gemini/openai_compat enum 驗證)、fallback、pty_pool_enabled(bool)、worker_managed(bool)
- [x] RT.2 web:AgentsPage「Runtime」分頁(provider 下拉 + 兩個 toggle + fallback 輸入)
- [x] RT.3 i18n + [x] RT.4 測試(provider enum 寫回、未知值拒絕)

### INF — `inference.toml` 設定面 🟠
新 RPC `inference.get/update` + 新「Inference」頁(分節)。
- [x] INF.1 gateway:`inference.get`(讀整個 inference.toml,secret 如 openai_compat.api_key 以 masked 回)
- [x] INF.2 gateway:`inference.update` root(enabled/backend/models_dir/default_model/auto_load/max_memory_mb)+ 原子寫 + role gate
- [x] INF.3 gateway:`[generation]`(max_tokens/temperature/top_p/stop/gpu_layers/context_size)
- [x] INF.4 gateway:`[router]`(enabled/fast_threshold/strong_threshold<fast/fast_model/strong_model/max_fast_prompt_tokens/cloud_keywords/fast_keywords)+ 跨欄位驗證(strong<fast)
- [x] INF.5 gateway:`[openai_compat]`(base_url/api_key→`_enc`/model)、`[exo]`、`[llamafile]`、`[mlx]`、`[mistralrs]`、`[llmlingua]`、`[streaming_llm]`、`[embedding]`
- [x] INF.6 web:新 InferencePage(分節 form:Backend / Generation / Router / Local backends / Compression / Embedding);`api.ts` inference.*
- [x] INF.7 i18n + [x] INF.8 測試(router threshold 驗證、api_key 加密不回明文)

### EVO — 每-agent `[evolution]` 進階 🟠
擴 `agents.update` evolution 區。
- [x] EVO.1 gateway:`[evolution.external_factors]`(user_feedback/security_events/channel_metrics/business_context/peer_signals bool)
- [x] EVO.2 gateway:skill-synthesis(enabled/threshold/cooldown_hours/trial_ttl)、graduation(enabled/min_lift)、recommendation(enabled/threshold)
- [x] EVO.3 gateway:curiosity(enabled/threshold/max_daily)、behavior monitor(enabled/drift_threshold)、`[evolution.stagnation_detection]`(已支援 RPC,補 UI)
- [x] EVO.4 web:AgentsPage evolution 分頁擴充(分組 toggle + 數值)
- [x] EVO.5 i18n + [x] EVO.6 測試

### CT — 每-agent `[container]` 進階 🟠
擴 `agents.update` container 區。
- [x] CT.1 gateway:worktree_enabled / worktree_auto_merge / worktree_cleanup_on_exit / worktree_copy_files(陣列)
- [x] CT.2 gateway:additional_mounts(host/container/readonly 陣列,經 mount-allowlist 驗證)、cmd、env
- [x] CT.3 web:AgentsPage container 分頁擴充(worktree toggles + mounts 表格)
- [x] CT.4 i18n + [x] CT.5 測試(mount 路徑驗證)

---

## Phase P2 — 完整度 / 一致性

### GOV — governance `policies/*.yaml` 編輯 🟡
- [x] GOV.1 gateway:`governance.list`(讀 global + per-agent policy)
- [x] GOV.2 gateway:`governance.upsert`/`governance.remove`(寫 policies/*.yaml;Rate/Permission/Quota/Lifecycle 結構驗證)+ role gate
- [x] GOV.3 web:新 GovernancePage(policy 列表 + 依 type 的 form)+ api.ts
- [x] GOV.4 i18n + [x] GOV.5 測試

### SCP — `.scope.toml`(wiki 命名空間 SoT)🟡
- [x] SCP.1 gateway:`wiki_scope.get/update`(per-namespace mode = agent_writable/read_only{synced_from}/operator_only)
- [x] SCP.2 web:SharedWikiPage 加「Namespace Policy」區 + api.ts
- [x] SCP.3 i18n + [x] SCP.4 測試

### ODO — per-agent `[odoo]` override 🟡
- [x] ODO.1 gateway:`agents.update` 支援 `[odoo]`:profile/allowed_models/allowed_actions/company_ids(api_key/password 加密)
- [x] ODO.2 web:AgentsPage 加「Odoo」分頁 + i18n + 測試

### G — 全域散落欄位 🟡
- [x] G.1 `[gateway]` bind/port/auth_token → `system.update_config` 擴充 + SettingsPage(⚠ 改 bind/port 需提示重啟)
- [x] G.2 `[rotation]` health_check_interval_seconds / cooldown_after_rate_limit_seconds → SettingsPage rotation 區
- [x] G.3 `[general]` default_agent / inference_mode → SettingsPage(inference_mode: local/claude/hybrid 下拉)
- [x] G.4 `[logging]` format(pretty/json)→ SettingsPage logging 區
- [x] G.5 `[[accounts]]` 完整編輯(priority/tags/profile/email/subscription/label),非僅 budget → AccountsPage 編輯表單擴充
- [x] G.6 全域 channels 補 `whatsapp_verify_token` / `whatsapp_app_secret` / `feishu_verification_token`(channels.add 欄位 + ChannelsPage 表單)
- [x] G.7 `[secret_manager]`(backend/vault_addr/vault_token→`_enc`/vault_mount)→ SettingsPage「Secrets」區
- [x] G.8 每-agent 散落:`[model].account_pool`/`utility`、`[heartbeat].max_concurrent_runs`/`cron_timezone`、`[proactive].token_budget_per_check`/`timezone`/`max_turns`、`[ptc]`、`[prompt]`、`[cultural_context]` → AgentsPage 對應分頁

### UI — 既有後端但缺 UI(low-hanging)🟢
- [x] UI.1 UsersPage 接 `users.remove`(刪除)+ `users.unbind_agent`(解綁)
- [x] UI.2 PartnerPortalPage 接 `partner.customer.update` / `partner.customer.delete`
- [x] UI.3 AgentsPage 補 `agents.update` 已支援但無輸入框的欄位:`status` 下拉、`max_gvu_generations`、`observation_period_hours`、`skill_token_budget`、`whatsapp_app_secret`、`feishu_verification_token`、`[evolution.stagnation_detection]`
- [x] UI.4 PartnerPortalPage license 產生表單:移除 client-side stub,或接真實 gateway 方法(若 license 仍 CLI-only,改為明確「請用 CLI」提示)

### XC — 一致性 / 安全修正 🟠
- [x] XC.1 MCP OAuth token 改 AES-256-GCM 加密存放(對齊 channel/API token);`mcp-oauth-tokens.json` 寫入處 + callback(server.rs:1973)
- [x] XC.2 全域 `channels.add`/`channels.remove`、`agents.create` 改原子寫(temp+rename),比照 per-agent/accounts/odoo
- [x] XC.3 `whatsapp_phone_number_id` 加密一致化(全域 channels.add 與 agents.update 行為統一)
- [x] XC.4 `skills.adopt` 補 `target_agent_id` 驗證(比照其他 agent-targeting handler)
- [x] XC.5 `secret_manager.vault_token` 接線加密變體(`vault_token_enc`)或文件化現況

---

## 統計

| Phase | 內容 | 功能組 | 細項(約) |
|---|---|---|---|
| P0 | 安全關鍵(CAP/CON/RED/MK/KS) | 5 | ~33 |
| P1 | 招牌功能(RT/INF/EVO/CT) | 4 | ~25 |
| P2 | 完整度/一致性(GOV/SCP/ODO/G/UI/XC) | 6 | ~30 |
| **合計** | | **15 功能組** | **~88 細項** |

> 建議執行序:P0 → P1 → P2。每個功能組可獨立交付(gateway + web + i18n + test 一條龍);跨功能組彼此檔案多為不同,適合多代理並行(每組一代理)。
> 注意:web 改動後需重建 binary(見記憶 [binary 測試流程]);多數 gateway RPC 集中於 `handlers.rs` 的 `match method`,並行編輯需小心同檔衝突(建議 gateway 端依功能組序列、web 端可並行)。
