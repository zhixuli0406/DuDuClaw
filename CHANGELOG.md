# Changelog

## [Unreleased]

### Added
- **成本／快取效率遙測 dashboard RPC（#3，2026-07-12）。** 為既有的
  `cost_summary` / `cost_agents` / `cost_recent` MCP 工具補上三個 admin-gated
  （`require_admin!` fail-closed）dashboard RPC，位於
  `crates/duduclaw-gateway/src/handlers.rs`，**復用同一條 `CostTelemetry` 查詢
  邏輯**（`summary_global` / `all_agents_summary` / `recent_records`），未另寫成本
  或快取效率公式：**`cost.summary {hours?=24}`**（總請求／各類 token／
  `avg_cache_efficiency`（＝`cache_hit_rate`）／成本 millicents／快取節省 ＋ 由
  `near_price_cliff_for` 衍生的 200K price-cliff 狀態 block）；**`cost.agents {hours?}`**
  （per-agent 明細 ＋ `cache_health`）；**`cost.recent {limit?=20,≤500}`**（近期
  per-request 記錄）。遙測未初始化時回良構的空／零 payload（`available:false`），
  不報錯。僅後端 RPC，前端下一波接線。
- **記憶時序／取代鏈 dashboard RPC（F1 Temporal Memory v1.19.0，#5，2026-07-12）。**
  為 `SqliteMemoryEngine` 既有的 `get_history` / `get_at` 補上 dashboard 操作面，
  authz 對齊既有 `memory.*`（`check_agent!(Viewer)` per-agent 可見性），
  `crates/duduclaw-gateway/src/handlers.rs`：**`memory.history {agent_id, subject,
  predicate | memory_id}`**（回該事實的完整取代鏈，各版本
  `valid_from`/`valid_until`/`superseded_by`/`supersedes`/`confidence` ＋
  `is_current` 旗標 ＋ `current_id`；給 memory_id 時先以新增的
  `SqliteMemoryEngine::triple_for_id` 解析出 triple，非 triple／非本 agent 回空鏈
  不報錯）；**`memory.at {agent_id, subject, predicate, at(RFC-3339)}`**（point-in-time，
  回當時有效的事實；查無回 `found:false`）。復用引擎方法，未重寫時序邏輯。
- **per-agent Odoo 憑證隔離 dashboard RPC（RFC-21 §2，#8，2026-07-12）。**
  三個 admin-gated RPC 讓 `agent.toml [odoo]` override 可讀寫測試，
  `crates/duduclaw-gateway/src/handlers.rs`：**`odoo.agent_config_get {agent_id}`**
  （回 profile／url／db／username／allowed_models／allowed_actions／company_ids ＋
  `api_key_set`／`password_set` 布林 ＋ 遮罩 `***set***`，**永不回傳明文或密文**）；
  **`odoo.agent_config_set {agent_id, url, db, user|username, api_key, password,
  profile, allowed_models, allowed_actions, company_ids}`**（api_key/password 走
  AES-256-GCM 加密存 `*_enc`，遮罩佔位符被拒收不覆蓋既有密鑰；url/db 沿用與全域
  相同的 SSRF/HTTPS/db-name 驗證器——復用 `apply_odoo_to_table`）；
  **`odoo.agent_test {agent_id}`**（以「全域 config.toml [odoo] ＋ 該 agent 覆寫」
  的有效設定測連線，憑證優先取 agent、否則全域；對實際撥號的 url 再做一次
  fail-closed SSRF 檢查；不寫入磁碟）。
- **`native_sandbox` ＋ Progent `policy` 經 dashboard 完整 round-trip（#6，2026-07-12）。**
  `agents.update` 的 `apply_capabilities_to_table` 補上 `capabilities.native_sandbox`
  （bool，Seatbelt/Landlock）與 `capabilities.policy[]`（Progent 參數級 tool policy：
  `{tool, effect: allow|forbid|ask, when: [{arg, op: equals|contains|starts_with,
  value}]}`）的寫入與嚴格驗證（非法 effect/op/缺 tool → fail-closed 拒絕整包）；
  `agents.inspect` 現回傳完整 `capabilities`（serde 直出 `CapabilitiesConfig`，含
  native_sandbox ＋ policy 的精確 ToolPolicy 形狀）供前端編輯器 round-trip。
- **Identity Resolution dashboard surface（RFC-21 §1，2026-07-12）。** 為既有的
  `duduclaw-identity` crate ＋ `identity_resolve` MCP 工具補上 dashboard 操作面，
  客戶可當場示範「AI 怎麼知道發訊的人是誰、怎麼拒絕非專案成員」。新增三個
  admin-gated（`require_admin!` 全數 fail-closed）dashboard RPC，位於
  `crates/duduclaw-gateway/src/handlers.rs`：**`identity.resolve`**（輸入 email／帳號
  ＋ channel，走與 MCP `identity_resolve` 同一條 `IdentityProvider` trait 解析路徑，回
  `ResolvedPerson` ＋ `is_project_member`；查無回 `found:false` 不是錯誤）；
  **`identity.config_get` / `identity.config_set`**（讀寫 config.toml `[identity]`：
  provider 選擇 wiki_cache／notion／chained、Notion database id ＋ `field_map`、
  `refresh_seconds`；Notion 整合金鑰 write-only、AES-256-GCM 加密存 `api_key_enc`、
  讀回遮罩 `***set***`，對齊既有 channel token 加密慣例）。provider 依設定建構
  （`build_identity_provider`）：Notion 未設定完整時 fail-safe 降級回 wiki_cache。
  前端：Integrations 頁（`web/src/pages/IntegrationsPage.tsx`）新增「身分解析」分頁
  `web/src/pages/IdentityPage.tsx`——provider 選擇 ＋ Notion 設定表單 ＋「測試解析」輸入框
  （即時查出姓名／角色／專案／是否專案成員）；`web/src/lib/api.ts` 補型別與
  `api.identity.*` 呼叫；i18n 三語（zh-TW／en／ja-JP）。
- **R4 Grok CLI 成為第六個 `AgentRuntime`（xAI「Grok Build」，2026-07-12）。**
  `RuntimeType::Grok`（`crates/duduclaw-core/src/types.rs`）＋ `which_grok` /
  `which_grok_in_home`（`crates/duduclaw-core/src/lib.rs`，官方 `grok` 優先、第三方
  `grok-cli` fallback）。新 `GrokRuntime`（`crates/duduclaw-gateway/src/runtime/grok.rs`，
  仿 antigravity）：偵測到 binary 才由 `RuntimeRegistry` 註冊，`execute()` 走 oneshot
  `grok -p`，system prompt＋history 內嵌進 prompt（無已驗證 `--system` flag、CJK-safe），
  duduclaw MCP server 寫入 `<agent_dir>/.grok/settings.json`，token 用量估算。接線：
  `runtime_config::model_matches_provider`（grok-* ↔ provider grok）、
  `model_capabilities::supports_vision`（Grok 保守 fail-closed，僅顯式 vision id）、
  `cli/lib.rs` `agent create --runtime grok`（scaffold `AGENTS.md`＋`CLAUDE.md`、typo 拒絕）、
  container `sandbox.rs`（`XAI_API_KEY` 注入＋`grok -p` 指令組裝）、dashboard
  `runtime_detect` 與 `runtime_models` 探測。**僅 CLI 偵測＋headless spawn**；
  SuperGrok OAuth（accounts.x.ai device-flow）與 Grok CLI 實際 flag / MCP config 路徑 /
  context 檔名皆標記 UNVERIFIED，列為 follow-up（見 `runtime/grok.rs` 檔頭）。
- **WP4 AI 員工離職生命週期（後端，2026-07-12）。** 新增 admin-gated（`require_admin!`
  全數 fail-closed）員工離職流程：**封存** `agents.archive` / **解封** `agents.unarchive`
  （`status=archived` + 復用 freeze kill-switch 停 heartbeat/evolution；`agents.list`
  預設排除、帶 `include_archived=true` 才回，回傳物件標 `archived` 旗標，零刪除可復原）；
  **交接** `agents.handoff`（memory/wiki/tasks 三開關預設全開：memory 走
  `duduclaw-memory::reassign_agent`（同庫 agent_id re-key，含 FTS 一致性、temporal
  supersession 鏈保留、交易內完成）或 `reassign_agent_cross_db`（per-agent memory.db
  ATTACH 跨庫搬移）；wiki 合併 `agents/<from>/wiki`→`agents/<to>/wiki`（衝突加尾綴不覆蓋）；
  tasks 走 `TaskStore::reassign_open_tasks`（未完成任務改指派）；完成後可選 `auto_archive`
  預設 true；全程冪等，任一子項失敗如實回報 `status:"PARTIAL"` 不吞錯）；**大頭貼上傳**
  `agents.set_avatar` / `agents.clear_avatar`（PNG/JPEG/WebP data URI，magic-byte 驗證＋
  512KB 上限，原子寫 `agents/<id>/avatar.<ext>`，`agents.get`/`inspect` 回傳 data URI、
  `agents.list` 回傳 `has_avatar`）。
- **WP4 移除語意改為軟刪除。** `agents.remove` 由硬刪（移入 `_trash/`）改為 `status=deleted`
  + freeze kill-switch，agent 目錄與 memory.db **資料保留不刪**、從所有清單/路由隱藏，
  拒絕對 main agent 執行（RPC 名維持相容）。archive/remove/handoff 皆寫入 audit log。
- **WP3 WebChat 歷史對話續聊（2026-07-12）。** 新增 `chat.sessions.list`（per-agent 歷史
  session 清單：首句摘要 CJK-safe 截斷、最後更新、輪數；非 admin 依 agent 可見性 fail-closed）
  與 `chat.sessions.history`（載入指定 session 對話輪，取最新 window 維持時序）RPC；`/ws/chat`
  UserMessage 幀帶 session_id 可 resume 既有 session（回送 session_info 確認；不帶維持原行為
  byte-compatible），前端 header `SessionHistoryMenu` 選歷史對話續聊。
- **WP5 agent 自助裝工具審批閘（2026-07-12，安全）。** MCP install-class 工具
  （`skill_hub_install`）一律經 `ApprovalBroker` 送收件匣人工拍板：安全掃描在前（High risk
  直接 DENY 不進審批），broker 不可用/逾時/拒絕皆 fail-closed DENY；唯一豁免為 operator 顯式
  `agent.toml [capabilities] auto_approve_install = true`（預設 false）。反向強制清單
  `approval_required_tools` 覆蓋豁免。補上會議指出的「agent 自主裝工具無授權」缺口。
- **WP6 Grok（xAI）direct-API 支援（2026-07-12）。** `ModelRegistry` 補 `grok-4.3`（1M ctx）
  /`grok-4.5`（500K ctx）＋既有 `grok-4.1-fast`；gateway `runtime` PROVIDERS 表補 xai entry
  對齊 duduclaw-llm preset（OpenAI-compat，tool calling/streaming 齊全）。使用者可經
  `~/.duduclaw/models.toml` override。
- **WP7 部門層知識庫/skill（公司→部門→個人，2026-07-12）。** `agent.toml [agent] department`
  欄位（選填，向後相容）；shared wiki `departments/<dept>/` 命名空間（agent 只讀寫自己部門＋公司層，
  跨部門 fail-closed）；skill 三層查找 per-agent > 部門 > global（近者優先），install scope 文法
  擴充 `department:<dept>`（admin/approval gate 沿用）；`wiki_namespace_status` 回報部門與讀隔離狀態。
- **WP8 白牌欄位級編輯分層（2026-07-12，安全）。** OEM license 攜帶簽章 `branding_editable` claim：
  系統方簽發的經銷商 token 與經銷商簽發的客戶 token 授予不同可編輯品牌欄位範圍。`branding.get`
  回 `editable_fields` 供前端遮罩；`branding.set`/`reset` 依範圍過濾（違規欄位整請求拒絕、部分範圍
  writer 不能清掉經銷商其他欄位）；`distributor.issue` 收選填 `branding_editable`。未顯式升級為 vendor
  級的欄位預設 system-only（fail-closed），System 範圍需真實簽章驗證的 issuer 私鑰、config 存在
  不足以升權。向後相容：無 claim 的 license 解析為完整 vendor 集。
- **WP9 Telegram 共用 bot + 員工綁定連結/QR（2026-07-12）。** 一間公司共用一個 Telegram bot，
  員工掃 QR／開連結即綁定到自己的 AI 員工，取代「一員工一 bot token」（規避 Telegram 多 bot
  帳號鎖定風險）。新增 `crates/duduclaw-gateway/src/agent_binding.rs`：`AgentBindingStore` 持久化
  `~/.duduclaw/agent_bindings.json`（`with_file_lock` 冪等原子寫，fail-closed）—— 存
  `(channel, external_user_id) → agent_id` 綁定表＋一次性綁定 token（SHA-256 digest 存儲、
  預設 TTL 15 分、用過即失效、`max_uses` 上限、過期自動修剪；plaintext 僅存在於 deep-link）。
  admin-gated RPC `channels.telegram_bind_token`（輸入 target agent，getMe 即時取 bot username
  不硬編，回 `token`/`deep_link`(`https://t.me/<bot>?start=<token>`)/`bot_username`/`expires_in_minutes`/
  `max_uses`）。全域 bot 收 `/start <token>` → 驗 token → 綁定該 Telegram user → target agent
  （成功同時 `approve_user` 過存取閘），無效/過期明確友善拒絕不靜默；每則訊息先 `resolve_bound_agent`
  路由到綁定 agent（agent 已刪則 fail-closed 不誤路由），未綁定且開啟 `shared_bot_binding` 設定時
  回引導訊息，否則維持既有 default-agent 行為。per-agent token 多 bot 模式不受影響（維持原路由）。
  external_user_id exact 比對、跨 channel token 不混用。前端通道設定頁「員工綁定連結」對話框：選
  AI 員工 → 產 token → 顯示 deep-link＋QR（純前端 `qrcode-generator` 產 SVG，無外部 CDN/服務）＋複製鈕；
  i18n 三語同步。

### Changed
- **排程任務設定統一到「例行工作」頁,移出系統設定→進階（2026-07-13）。** 原本排程管理是
  分裂的:`/routines`(例行工作)頁只能列出+暫停/恢復/刪除,而新增/編輯藏在「設定→進階→排程任務」
  的 `CronTab`。改為把新增/編輯(共用同一個 `ScheduleBuilder`)直接併入 `RoutinesPage`——頁首
  「新增例行工作」按鈕 + 每列「編輯」鈕 + 共用 create/edit 對話框;`SettingsPage` 移除 `cron`
  分頁(TabId/VALID_TABS/TAB_META/ADVANCED/render 一併清掉),舊 `?tab=cron` deep-link 自動
  導向 `/routines`;刪除已無引用的 `CronTab.tsx`。同一批 `cron.*` RPC,無後端變更;i18n 三語同步。
- **Grok Build runtime 依 docs.x.ai 官方文件重寫,去除 R4 的 UNVERIFIED 假設（2026-07-13）。**
  以 docs.x.ai 一手來源核對後重寫 `runtime/grok.rs`:headless `-p/--single`(確認)、模型
  `--model`(確認)、工具限制 `--tools`/`--disallowed-tools`(確認,疊在 `native_sandbox` 硬閘上)、
  auth `XAI_API_KEY` 環境變數(非先前誤設的 `GROK_API_KEY`)、`AGENTS.md` 指令檔家族。**最大修正**:
  MCP 設定改寫為 `[mcp_servers.duduclaw]` **TOML**(寫入 per-agent `<agent_dir>/.grok/config.toml`,
  merge 保留其他表)——先前誤仿 Gemini 寫成 `.grok/settings.json` JSON。agent 身分另經 spawn env
  轉發作為 fallback;`--version` 探測加逾時(裸 `grok` 會開 TUI,不能讓探測卡住)。殘餘(需 live CLI):
  `--tools` 清單分隔符、專案本地 config 探索、`--output-format json` schema、完整 `--model` 名冊
  (`grok models`;目前僅 `grok-4.5`/`grok-build-0.1` 經文件確認)。`docs/features/feature-inventory.md`
  同步更新。
- **去識別化設定 UI 改用白話，外部系統（ERP/CRM）成一等公民（2026-07-12）。** 客戶回饋
  「設定太難懂，連開發者都看不懂」——`RedactionTab`（`web/src/components/settings/sections/`）
  重寫：主開關配一句白話說明（明確點名檔案／Wiki／記憶／Odoo／鼎新 ERP／CRM 皆涵蓋）；
  「資料來源保護」每列加白話標題＋一行說明，主保護點「AI 讀取的外部資料」置頂，遮蔽模式
  改稱一律遮蔽／不遮蔽／智慧遮蔽／沿用上游；工程術語的「工具出口規則＋glob」改成
  **「外部系統（ERP／CRM／資料庫）」**區塊，用 Odoo／鼎新 ERP／Salesforce／HubSpot／自訂
  一鍵加入（底層仍是既有 `tool_egress`，無後端 schema 變更），外送政策白話化為
  完全不外送（最安全，預設）／需要時還原真實值／原樣傳遞代號。保管期限／清除／設定檔收進
  「進階設定」摺疊區。誠實標註：Odoo 為內建連接器，鼎新／Salesforce／HubSpot 標
  「範本·連接器規劃中」（規則可預先武裝，不假裝已有 live 整合）。i18n 三語同步、新增
  `common.remove`。純前端＋文案，引擎不動（所有 MCP 工具結果早已在 `mcp_dispatch.rs`
  單一節流點以 `Source::ToolResult` 去識別化，外部系統本就在保護範圍內）。
- **首屏/清單效能三修（2026-07-12）。** (E1) 新增輕量 `agents.avatar` RPC（輸入 `agent_id`，
  只讀 `agents/<id>/avatar.<ext>` 回 data URI 或 null，**不**跑 telemetry 月度聚合、**不**序列化
  SOUL/identity/skills/model config；authz 沿用 `check_agent!(Viewer)` fail-closed）；前端 avatar
  store 改呼叫它取代 `agents.inspect`，首屏 N 個有頭貼員工不再各發一次重 RPC（`agents.inspect`
  的 `avatar` 欄位保留相容）。(E2) `chat.sessions.list` 改為先在內層 `SELECT id ... ORDER BY
  last_active DESC LIMIT` 縮小列集，外層才對這批 id 算 title/turn 相關子查詢，並加
  `idx_sessions_last_active(agent_id, last_active)` 索引（冪等建於建表路徑），大量 session 時
  popover 不再全表掃描；清單內容與排序不變。(E3) `agents.list` 的 `agent_has_avatar` 由每員工最多
  3 次 `Path::exists` stat 改為單次 `read_dir`。 `AgentStatus` 新增 `is_operational()` /
  `is_listable(include_archived)`（fail-closed，非 Active 皆非 operational），取代散落的 ad-hoc
  match；掛到 MCP `list_agents`（恆隱藏 Deleted）、`spawn_agent`/`agents.delegate`（拒絕
  non-operational 目標不 enqueue）、「Your Team」名冊、dashboard `agents.list`。修正軟刪/封存 agent
  仍可被 spawn/委派/列名的漏洞。
- **部門 wiki 讀隔離與 `.scope.toml` 寫政策正交（2026-07-12，安全）。** 部門讀隔離永遠生效，不再被
  `.scope.toml` 對 `departments` 命名空間的宣告反向關閉；`shared_wiki_stats`/`lint` 亦依部門過濾。
- **agent 封存/解封保留原演化旗標。** archive 快照 `evolution.enabled`/`heartbeat.enabled` 原值，
  unarchive 還原（無快照保守維持 false），不再無條件開啟自我演化。

### Security
- **WP5 安裝審批閘上收 dispatch 層 — 補 `approval_required_tools` fail-open（2026-07-13）。**
  審批閘原先只嵌在 `handle_skill_hub_install`,導致 operator 在 `agent.toml [capabilities]
  approval_required_tools` 列出**其他**工具時被靜默忽略(死設定 = fail-open)。新增
  `mcp::gate_tool_approval_dispatch` 並在 `mcp_dispatch::dispatch_tool_call` 的統一節流點
  (complete mediation I3,涵蓋 stdio/HTTP/SSE)於派工前執行:任何 `install_approval_required`
  為真的工具都先過審批,fail-closed(拒絕/逾時/broker 不可用皆不派工)。`skill_hub_install`
  維持自身「掃描後才審批」的較佳流程並在 helper 內排除,避免重複提示。
- **avatar/logo 影像驗證器合併為單一 fail-closed 來源,logo 補預解碼 DoS 防護（2026-07-13）。**
  `branding::validate_image_data_uri` 成為 PNG/JPEG/WebP data URI 的唯一驗證器(SVG 拒收、
  magic-byte 比對、解碼上限),avatar (`handlers.rs`) 與 logo (`branding.rs`) 都改呼叫它。
  合併時把 avatar 既有的「解碼前先擋 encoded 長度」(F8)提升為共用行為——**logo 路徑原本缺這道
  防護**,惡意超大 base64 會先被完整解碼進記憶體才檢查大小,現已在解碼前擋下。
- **WebChat resume 擁有權閘（2026-07-12）。** `/ws/chat` resume 強制 resumed session 屬本連線
  （id 相符或 `{session_id}#` 前綴），跨 channel / 他連線一律拒絕（fail-closed）；關閉「送他人
  session_id 讀寫其對話」的越權。殘餘限制：webchat session 無 user 維度，前一連線的 session 無法
  跨連線 resume（誠實 DEGRADED，待 webchat 導入真實使用者驗證）。
- **收尾波（2026-07-12）— P2 兩項＋G12 落盤＋四項欠帳接線。** G13 Talk Mode（dashboard
  對話模式：WebAudio RMS VAD-lite＋狀態機＋Talk 切換鈕，疊在既有 PTT/STT/TTS 上；
  不做 wake word／barge-in；mic 迴圈 PENDING-LIVE）；G15 Live Canvas（agent
  `canvas_push`/`canvas_clear` 推 HTML 視覺工作區，寫入時 ammonia allowlist＋渲染時
  `<iframe srcdoc sandbox="">`＋自包含 CSP 三層防護，`canvas.get` RPC＋`/畫布` 頁）；
  G12 step 事件落盤（`run_steps.db` 持久化 tool_step/todo_update，runs.get 合併真步驟，
  祕密 mask-before-truncate）；`user_code_profile` MCP 工具；`[fork] judge = "llm"` 生產
  LlmJudge 建構點（FallbackJudge 降級）；ephemeral 成本父歸因（`ephemeral_parents` JOIN）。
- **P1 尾輪十五項（2026-07-12）。** G4 session 可攜三件套（`/handoff` 歧義即拒防跨使用者外洩、
  `/undo` tombstone 軟刪、`/rollback` 對話水位、世系 `#N`；chat 指令派發補齊 TG/DC/LINE）；
  G5 skill hub taps（clawhub/lobehub 一手驗證、**裝前必過安檢掃描 fail-closed**）＋curator
  生命週期（30d stale／90d 封存／pin 豁免，無使用訊號絕不自動封存）；G7 MoA 虛擬模型
  （`[moa.<name>]`＋`moa:` 解析、提案 `<data>` 降格、usage 誠實加總、gateway direct-API
  路由＋CLI 路徑明確拒絕）；G9 agentcompanies 雙向互通（export 確定性＋secret 全 scrub＋
  PARTIAL 誠實標注、npm `@duduclaw/paperclip-adapter` 建置測試綠未發布）；G12 執行紀錄
  `/runs`（sessions.db＋tool_calls.jsonl 誠實重建，未持久事件明示不偽造）；M2 User-as-Code
  read-only 實驗（typed 規則＋四階確定性衝突解決）；M3 JitRL 零梯度學習（OpenAI-compat
  `logit_bias` Tier B 上線、llama.cpp seam、預設關、僅顯式回饋 `jitrl_feedback` MCP）；
  S3 紅隊外部靶場（`duduclaw test --bank`＋over-defense 追蹤＋25 案例起始 bank）；
  R2 Foresight 前綴預警（確定性零 LLM、`run.at_risk` autopilot 事件）；R3 MAST 14 模式
  失敗分類（channel_failures＋eval 報告）；R4 audit 輸入捕捉（先遮罩後截斷、白名單擴充
  20 項高危工具）；O2 `spawn_ephemeral` 動態子代理（四元組、特權升級 fail-closed、GC
  圍堵）；O3 FineVerify 細粒度裁判（確定性聚合）；O4 誠實成本守門（委派 vs 直接回覆報表
  ＋spawn 成本提示，粒度限制如實輸出）；U4 共同計畫（`plans` 表＋`/plans` 頁＋agent
  holder-guard 步驟更新＋prompt 注入）。
- **白牌 P2 後段 — HTML 區塊、簽章散發包、主題色、金鑰自帶回家地址、channel 白牌。**
  ① **About HTML 區塊**（`about_html`）：新增 `ammonia` 依賴，保守 allowlist 消毒
  （`<a>` 強制 `rel="nofollow noopener noreferrer" target="_blank"`、`<img>` 僅
  `data:image/png|jpeg|webp` 且沿用 logo 的 magic-bytes/512KB 驗證、剝除
  `style/class/id/on*/<script>`、>64KB 直接拒絕）；消毒時機為 `branding.set` 儲存前＋
  `branding::load` 讀出後（防手改檔）。新 RPC `branding.preview`（所見即所存）。
  ② **主題色**（`accent_color`，驗證 `#rrggbb`）隨品牌散發。
  ③ **簽章散發包**（`branding.bundle.json`）：由 owner issuer 金鑰簽章，任何 instance
  只要驗簽通過即自動套用品牌，**無需 white_label license** 即可*顯示*（*編輯*仍受 gate）；
  gateway 品牌解析順序 local `branding.json` > 驗簽通過的 bundle > 預設，回應新增頂層
  `source` 欄位；owner gateway 新增 `POST /v1/branding/sign`（issuer-gate、per-IP 10/min、
  複用 refresh 的 subscription+fingerprint 閘）；RPC `branding.bundle.create`（經銷商自助）
  與 `distributor.bundle.sign`（owner 離線代簽）。vendor 名永遠疊加在最上層，散發包蓋不掉。
  ④ **金鑰自帶回家地址**：`config.toml [distributor] public_url` 設定後，`distributor.issue`
  將 owner URL 嵌入金鑰 `control_url`，客戶 instance 無需設 `DUDUCLAW_CONTROL_URL` 即自動
  續期（根治 60 天離線降級）。⑤ **Channel 白牌**：Telegram/Discord/Slack/Google Chat/
  WebChat 使用者可見的 "DuDuClaw" 字樣改吃品牌生效名稱（`effective_product_name`，短 TTL
  快取；log/內部識別不動）。詳見 `docs/guides/white-label.md`「Shipping your branding to
  customers」節。
- **G1 派工引擎收尾 — lease 續租＋Goal 鏈（G8）。** 續租三面向接上：dispatch engine 的
  `LeaseRenewalGuard` RAII ticker（lease/3 週期）、外部認領者的 `tasks_renew` MCP 工具
  （holder-guarded）、系統提示 Task Queue 段與 heartbeat 喚醒訊息的續租教學＋
  `tasks_claim` 回應附 `lease_note`。依賴閘控改為真強制：`atomic_claim` 在單一
  IMMEDIATE transaction 內驗證每個 `depends_on` 皆存在且 `done`（fail-closed，回傳
  `BlockedByDeps`）。新 `goals` 表（parent 鏈、循環拒絕、TOCTOU 防護）＋
  `goals_create`/`goals_list` MCP＋`tasks_create goal_id`；pending-task 注入攜帶
  root-first goal ancestry（byte-stable、CJK-safe）。`pending` 任務納入 heartbeat 拉取
  與提示注入。背景引擎預設仍關（`[dispatch] enabled = true` 開啟即安全）。
- **G10 — 企業微信（WeCom）＋釘釘（DingTalk）通道。** WeCom 自建應用：回調簽章
  known-answer 驗證＋AES-256-CBC 加解密（panic-safe 邊界檢查）＋gettoken 快取＋
  text/markdown 發送＋圖片上傳＋±1h 重放窗。DingTalk 企業內部機器人：HMAC-SHA256
  驗簽 fail-closed（1h 時鐘窗）＋sessionWebhook 回覆（conversation 持久化 90 分鐘窗，
  超窗主動發送誠實回錯）＋`*.dingtalk.com` 錨定允許清單防 SSRF。接線鏡射 feishu 全部
  註冊點（含 dashboard ChannelsPage、sender factory、GATED_CHANNELS、委派轉發）。
  活體驗證需真租戶憑證（PENDING-LIVE）。
- **G11 — Work Timeline（公司級時間軸）。** 新 `timeline.list` RPC（authz 同
  `activity.list`、非 admin fail-closed agent filter）＋純 SVG Gantt 頁 `/timeline`
  （每 AI 員工一 lane、重疊自動堆疊、1h/6h/24h/7d、now 線、雙主題、三語 i18n、30s
  靜默刷新）。誠實呈現：任務板有真起迄 ⇒ bar；活動/心跳只有單點 ⇒ dot，不捏造時長。
- **N1/N2 — 夜間引擎真 LLM adapter（活體驗證）。** `night_llm::RotatedNightLlm` 走既有
  帳號輪替路徑（CLI → Direct API fallback、haiku 級 utility model），每次呼叫前過
  DailyCircuitBreaker（rolling-24h 花費上限，狀態原子落盤 `night_breaker.json`，重啟
  不歸零）。`config.toml [night] llm_enabled` 預設關（fail-safe：關閉時與 scaffold
  byte-identical）。夜間 spawn 鎖零工具 capabilities、注入記憶一律 `<data>` 降格。
- **G2 收尾 — 訂閱 seat device-code 登入＋proxy 轉發。** `duduclaw auth device
  --provider copilot|qwen`（RFC 8628 device flow、長效 GitHub token AES-256-GCM 加密
  落庫、短效 Copilot token 按需鑄造 <5min 刷新、公開 client id 可 config 覆寫）；
  `duduclaw proxy` seat 轉發（SSE passthrough、無 seat ⇒ models 不列出 fail-closed）；
  `/v1/models` 補 60 req/min 限流（pre-auth）。Qwen 官方已停免費 OAuth ⇒ PENDING-LIVE。
- **S2 — 引數級 provenance v1（PACT，arXiv:2605.11039，library 層）。**
  `duduclaw-llm::provenance` 污染 span ledger（window-hash ≥12 字元比對、CJK-safe、
  512 span 上限鎖存）＋`run_tool_loop` 政策 `Off/Warn/Enforce`：Enforce 只擋「敏感工具
  ×污染引數」組合並以 `is_error` 回饋讓模型重規劃，非敏感工具照跑。預設 Off，既有
  呼叫者零行為變更；v1 限制（子字串比對非資料流追蹤）明載模組 doc。
- **O1 — 信心感知派工路由（OI-MAS，arXiv:2601.04861）。** `delegation_router` 三層
  tier 純函式（機械動詞＋短 ⇒ utility 模型；架構/安全/除錯訊號 ⇒ Preferred；模糊不
  降級），僅作用於 Dispatch 派工路徑（channel reply 不可能被改道）、非 Claude runtime
  不注入 Claude model id。`[delegation] confidence_routing` 預設關，關閉時 byte-identical。
- **U1 — 主動訊息時機引擎。** `proactive_timing::TimingGate` 純函式：24 槽日節律直方圖
  判 quiet hours＋10 分鐘 mid-flow 偵測（sessions.db 唯讀隨查隨算）。只延遲不丟訊息
  （6h 硬上限）、cold start 隱形、使用者排程 reminder 結構豁免；silence breaker 與
  PROACTIVE.md 兩條 send path 皆過 gate。預設開，`[proactive] natural_timing = false`
  kill switch 回復原行為。

- **Distributor white-label portal (backend).** Resellers whose license carries the
  `white_label` feature (tier = Oem) can rebrand the dashboard while the upstream
  vendor credit ("嘟嘟數位科技有限公司 / DuDu Digital Technology Co., Ltd.") stays
  const-assembled into every response — never config-sourced, never writable. New
  `crates/duduclaw-gateway/src/branding.rs` (`branding.json` atomic persistence;
  fail-closed validation: SVG rejected, logo magic-byte + 512 KB + data-URI prefix
  whitelist, CJK-safe codepoint length caps) and
  `crates/duduclaw-gateway/src/distributor_store.rs` (SQLite `distributor.db`,
  WAL/0600, `distributors` + `issued_licenses`). New dashboard RPCs `branding.get`
  / `about.get` (any logged-in user), `branding.set` / `branding.reset`
  (`require_admin!` **+** fail-closed white_label gate), and `distributor.status /
  list / add / update / remove / issue / revoke` (`require_admin!`). Issuance reuses
  the License v2 format (tier = Oem, `public_key_id = v2`, machine-bound, default
  365-day term), self-verifies against the binary's baked v2 public key before
  booking, and never logs the issuer private key; issue/revoke write
  `security_audit.jsonl`. Issuer key path comes only from `[distributor]
  issuer_key_path` (unset = explicit zh-TW error, no path guessing). **Known limit
  (§7):** a self-signed OEM license downgrades to OpenSource after 60 days without a
  phone-home — surfaced as an issue-time warning; a lightweight refresh/CRL endpoint
  is deferred to P2. 17 new unit tests.
- **Distributor white-label control-plane (P2 — refresh & revocation).** The owner
  gateway now acts as a lightweight control-plane for the OEM keys it signs, so they
  no longer trip the 60-day offline downgrade and revocations propagate. New
  `crates/duduclaw-gateway/src/license_serve.rs` mounts two public, issuer-gated
  endpoints (unset `[distributor] issuer_key_path` ⇒ `404` — a plain gateway exposes
  nothing): `POST /v1/license/refresh` re-signs the caller's license with
  `last_phone_home = now` (**never extends the term**; returns `revoked` for a
  revoked key, `403` for a fingerprint mismatch or expired key, echoes the request
  `nonce` verbatim for the anti-replay client) and `GET /v1/license/crl` serves an
  Ed25519-signed CRL (7-day TTL, canonical payload byte-aligned to the client
  verifier). Trust is proven by `subscription_id` + `machine_fingerprint` (no
  bearer, matching the cloud plane); per-IP rate limits (refresh 30/min, crl
  60/min); the issuer private key is read per request, never logged, never echoed.
  `distributor_store` gains `get_license_by_subscription_id`, an idempotent
  `last_refresh_at` column migration + `touch_refresh`, and a pure
  `resign_license_for_refresh` (shares the sign + self-verify kernel with issuance).
  Zero client code change — a reseller sets `DUDUCLAW_CONTROL_URL` to the owner
  gateway. `distributor.status` reports `refresh_endpoint_active`; the console shows
  an endpoint badge, the `DUDUCLAW_CONTROL_URL` setup snippet, and per-key last
  refresh time; the issue-time warning now guides distributors to point at the
  gateway instead of only warning about the 60-day downgrade. 10 new unit tests + 6
  integration tests.
- **Distributor white-label portal (dashboard).** New `useBrandingStore`
  (`web/src/lib/branding.ts`) hydrates from a localStorage cache pre-auth and from
  `branding.get` post-auth, driving the sidebar mark, login page, document title,
  favicon, and agent-name fallbacks (defaults unchanged: DuDuClaw / 🐾). New
  `/about` page shows the reseller's own branding above a fixed vendor block
  ("嘟嘟數位科技有限公司 / DuDu Digital Technology Co., Ltd.") sourced from the
  backend const with a hard-coded front-end fallback. New "品牌設定" settings tab
  (logo upload via base64, PNG/JPEG/WebP only, 512 KB pre-check; read-only with an
  upgrade notice when `white_label` is not licensed) and an admin-only
  `/manage/distributors` console (add resellers, issue machine-bound OEM keys with
  a copyable activation blob, revoke with an honest CRL note; empty state explains
  `[distributor] issuer_key_path`). i18n keys added across zh-TW / en / ja-JP.
- **G6 — A2A v1.0 signed agent cards.** `/.well-known/agent-card.json` now carries
  an EdDSA (Ed25519) detached-JWS `signatures` array so a recipient can verify the
  card's authenticity; `GET /.well-known/jwks.json` serves the public key (OKP /
  Ed25519, RFC 8037). The signing key is auto-generated at
  `~/.duduclaw/keys/a2a-signing.ed25519` (chmod 600) on first start. Fail-closed:
  a key error yields the unsigned card + a warning, never a 500. New
  `crates/duduclaw-gateway/src/a2a_signing.rs` (14 unit tests, sign→verify
  roundtrip). Existing card fields are unchanged — signatures are additive.
- **G2 — Subscription-OAuth breadth + local `duduclaw proxy`.** The `AccountRotator`
  OAuth path is generalized beyond hardcoded Anthropic: OAuth seats carry a
  `provider` field (Claude / ChatGPT-Codex / Copilot / Qwen Portal are catalogued),
  and non-Anthropic seats never fabricate env-var names or leak a seat token as an
  API key. `duduclaw proxy --bind 127.0.0.1:PORT` exposes the account pool as a
  local OpenAI-compatible endpoint (`POST /v1/chat/completions` with SSE,
  `GET /v1/models`, `GET /healthz`) so external tools (Aider / Cline / Codex) can
  borrow subscription quota; Bearer-guarded, loopback by default, 503 fail-closed
  on no accounts. Copilot/Qwen device-code token acquisition and OAuth-seat
  forwarding through the proxy are **PENDING-LIVE** (need per-vendor credentials /
  a CLI-runtime bridge).
- **U2 — Evidence-based approval UX (CHI/FAccT 2026).** Approval cards in the Inbox
  are redesigned around three empirical findings: plan-first (arXiv:2604.04918 —
  "what this AI employee intends to do" summary leads, approve/deny follows),
  heuristic verification (arXiv:2606.05391 — risk badge + opt-in one-tap spot-check
  instead of forcing a full read), and reviewer-fatigue protection
  (arXiv:2606.08919 — daily approval count + same-kind batch hint, never
  auto-approve). Confidence/risk is shown only at the whole-action level, never
  token-level (arXiv:2605.28571) — codified in `web/DESIGN.md` §13. High-risk
  actions gate through the shared `ConfirmDialog`. Pure risk-tiering in
  `web/src/lib/approval-risk.ts` (30 tests); backend approval RPCs unchanged.
- **G1 — Durable multi-agent dispatch engine.** Upgrades cross-agent delegation
  from the fragile file-based IPC (`bus_queue.jsonl`) to a durable SQLite task
  lifecycle, closing the gap against Hermes Kanban swarm / paperclip wakeup queue.
  `crates/duduclaw-gateway/src/task_store.rs` gains an idempotent column
  migration (`claimed_by` / `claimed_at` / `lease_expires_at` / `depends_on` /
  `retry_count` / `max_retries` / `goal_mode` / `acceptance_criteria` /
  `result_summary` / `judge_feedback`) plus four durability primitives: **atomic
  claim** (`atomic_claim` — conditional `UPDATE ... WHERE status='pending' AND
  claimed_by IS NULL`, exactly one worker wins), **zombie reclaim**
  (`reclaim_zombies` — a leased task whose worker died is requeued with a retry
  count, or marked `failed` once the budget is spent; NULL-lease board tasks are
  never touched), **dependency unlock** (`claimable_tasks` gates a task until
  every `depends_on` id is `done`), and **goal mode** (`complete_task` routes a
  `goal_mode` task to a `review` state; acceptance is decided by a judge). A new
  background loop `crates/duduclaw-gateway/src/dispatch_engine.rs`
  (`DispatchEngine`, heartbeat-cadence tick, spawned in `server.rs`) drives
  zombie reclaim every tick and runs goal-mode review through a pluggable
  `AcceptanceJudge` (`LlmAcceptanceJudge` reuses the fork crate's `LlmCaller`
  abstraction). **Fail-safe:** a judge error parks the task as `needs_human` —
  never auto-accepted, never looped. The MCP `tasks_claim` tool now uses the
  atomic primitive (falling back to the legacy claim for pre-G1 board tasks),
  `tasks_complete` routes goal-mode tasks to review, and `tasks_create` accepts
  `depends_on` / `goal_mode` / `acceptance_criteria` / `max_retries` / `durable`.
  The `bus_queue.jsonl` rail is retained as a compatibility path (unchanged). 19
  new unit tests (12 task_store + 7 dispatch_engine) cover atomic-claim
  concurrency, zombie requeue/fail at the retry cap, dependency gating, and
  goal-mode accept/reject/judge-failure, plus a guard so `complete_task` cannot
  overwrite a task already in a terminal (`done`/`cancelled`) state. **The
  synchronous primitives (atomic claim, dependency gating, complete) are live
  via the MCP task tools; the background `DispatchEngine` loop is default-OFF**
  (`config.toml [dispatch] enabled = true` / `DUDUCLAW_DISPATCH_ENGINE=1`):
  its zombie-reclaim would falsely requeue any task running longer than the
  fixed 300s lease because there is no mid-task lease-renewal signal yet
  (`renew_lease` has no caller). It stays off until renewal is wired. The
  acceptance judge is likewise spawned with `None`, so goal-mode `review` tasks
  sit safely in `review`.
- **Night Engine — idle-time compute suite (N1–N4).** "The AI employee tidies
  its memory and pre-reads tomorrow's work while it sleeps": four paper-grounded
  capabilities layered on the existing heartbeat scheduler + evolution engine,
  default OFF per agent (`agent.toml [night_engine] enabled = true`; new
  `NightEngineConfig` in `duduclaw-core`). A gateway-side idle-aware scheduler
  (`crates/duduclaw-gateway/src/night_engine.rs`, `spawn_night_engine`) reuses the
  heartbeat agent registry, reads `sessions.last_active` to detect idle agents,
  and fires a night pass bounded by a **per-pass cost cap** (`PassBudget`) and a
  **per-agent daily circuit breaker** (`DailyCircuitBreaker`) so idle compute can
  never run away. **N3 Schema induction** (DCPM arXiv:2606.09483) and **N4
  Recurrence-gated consolidation + deterministic trust verification** (RecMem
  arXiv:2605.16045 + TRUSTMEM arXiv:2606.25161) are live and fully deterministic
  (zero LLM), implemented in `crates/duduclaw-memory/src/night.rs`: N3 scans
  episodic memory for token themes recurring across ≥ `schema_min_support`
  memories and promotes each into a `night-schema` semantic entry (superseded by
  theme via the temporal chain); N4 only consolidates a theme that recurs ≥
  `recurrence_threshold` times, then gates the merge on a coverage / preservation
  / faithfulness check and **rolls back** (never stores) a merge that fails, so
  the store can't degrade the more it tidies. **N1 Sleep-time compute**
  (arXiv:2504.13171) and **N2 Proactive prefetch** (ProAct arXiv:2605.25971) are
  scaffolded behind a `NightLlm` trait — orchestration, budget gating, prompt
  building and `night_cache.jsonl` write are implemented and unit-tested with a
  mock LLM, but the live LLM adapter is not yet wired into the running scheduler
  (passes `None`), so N1/N2 are PENDING-LIVE. 28 new unit tests (12 memory + 16
  gateway) cover the tokenizer, theme detection, recurrence gate, three-axis
  verification, idle detection, circuit breaker, budget, prompt/cache, and the
  end-to-end deterministic + mock-LLM passes.
- **Event-triggered cron (G3, parity with OpenClaw 2026.7).** Cron tasks gain
  two event-driven trigger kinds on top of the existing time schedule. New
  `crates/duduclaw-gateway/src/condition_eval.rs` holds the fail-closed decision
  logic (pure, unit-tested): `TriggerKind` state machine (`time` | `condition` |
  `on_exit`), condition-output parsing (`{fire, message?, state?}` with a 16 KiB
  `state` cap and logs-then-JSON tolerance), and sandboxed execution via the
  existing `duduclaw-sandbox` native OS primitive (macOS Seatbelt / Linux
  Landlock). A **`condition`** task runs its script at each due cron slot and
  fires only on `fire:true`, persisting `state` (≤16 KiB, oversize rejected
  fail-closed) across evaluations and injecting the script's `message` into the
  fired prompt; an **`on_exit`** task fires only when its watch command exits 0.
  `CronStore` gains four idempotently-migrated columns (`trigger_kind`,
  `condition_script`, `condition_state`, `watch_command`) plus `update_trigger` /
  `update_condition_state` setters; `cron.add` / `cron.update` RPCs read and
  strictly validate the new fields (unknown `trigger_kind`, or a
  condition/on_exit task missing its script/command, is rejected at write time).
  Every failure mode — sandbox refusal, spawn error, 30 s timeout, non-JSON
  output, missing `fire`, oversized state — resolves to *do not fire*. The cron
  schedule acts as the evaluation cadence; `last_run` advances at each due slot
  regardless of the gate outcome, so a condition is re-checked only on schedule
  (no per-tick runaway).
- **Lightweight trajectory anomaly detection (R1, Trajectory Guard arXiv:2601.00516).**
  New `crates/duduclaw-gateway/src/trajectory_guard.rs` — a **deterministic,
  zero-LLM-cost** heuristic guard over the structured tool-step stream
  (`StepEvent`/`StepTracker`) plus a cost-slope signal. Four pure, unit-tested
  rules emit `AnomalySignal { kind, severity, evidence }`: **repeated-tool loop**
  (same tool + normalized input ≥N in a window — the runaway-loop fingerprint),
  **excessive depth** (outstanding tool-call nesting past a threshold),
  **cost-slope spike** (cumulative spend rate past `baseline × multiplier`), and
  **trajectory stall** (many steps, overwhelmingly read-only, no productive
  output). Severity grades Low/Medium/High with per-kind de-dup + escalation.
  Wired into the `channel_reply` stream loop: every parsed step feeds the guard;
  a High-severity signal is appended to `channel_failures.jsonl` (with an
  `anomaly` classification, under an advisory file lock) and logged in zh-TW.
  **Fail-safe, not fail-closed** — the guard NEVER kills a task; tripping an
  existing circuit breaker is an explicit operator opt-in (`intervene = true`)
  surfaced as a pure `should_intervene` decision, default report-only. Tunable
  via `config.toml [trajectory_guard]` (conservative defaults, `enabled = true`).
  36 new unit tests (each rule normal/anomaly + window boundaries + cost-slope
  math + severity grading + config parse/clamp + stateful de-dup/escalation).
- **Memory benchmark integration (M1): LongMemEval-V2 + PersonaMem-v2.**
  Two 2026 memory benchmarks wired into the existing `python/duduclaw/memory_eval`
  pipeline alongside LOCOMO/RR/RA. New modules `longmemeval_v2.py`
  (arXiv:2605.12493 — 451 questions, 5 memory abilities, agentic-trajectory
  context) and `personamem_v2.py` (arXiv:2512.06688 — HF `bowen-upenn/PersonaMem-v2`,
  1000 user-chatbot interactions, 300+ implicit-preference scenarios), both
  mirroring `retrieval_accuracy.py`'s loader + metric + alert shape. They measure
  a **retrieval-level `recall@k`** (does the gold evidence memory land in the
  top-K search over the SqliteMemoryEngine batch-query surface), broken down per
  ability / per scenario — a memory-system proxy; full QA answer-correctness needs
  an LLM judge and is left explicitly **PENDING-LIVE**. `to_report()` emits the
  same report shape as LOCOMO so the dashboard shows all three benchmarks together.
  Dataset acquisition via new `fetch_benchmarks.py` (downloads from HuggingFace,
  converts to local jsonl; on no-network / no-`datasets` / unknown-repo it
  **fails honestly** with the exact fetch commands and never fabricates data —
  LongMemEval-V2's repo id must be operator-supplied since it wasn't verified).
  Each benchmark ships a hand-crafted 4-5 question **sample fixture**
  (`data/<bench>/sample.jsonl`) plus a zero-dependency `InMemoryMemoryClient`
  (`fixture_client.py`) so smoke + unit tests run fully offline. Wired into the
  cron runner: the daily 03:00 UTC smoke test gains `longmemeval_v2_sample` /
  `personamem_v2_sample` cases (TC-4/TC-5, sample fixtures); weekly KPIs include
  the benchmark suite (full dataset if fetched, else sample fallback marked
  `dataset: "sample"`); new `monthly_benchmarks` command runs the full sets and
  reports `pending_live` when they aren't downloaded. 30 new unit tests
  (loader / recall metric / alerts / offline end-to-end / honest-fail); downloaded
  `full.jsonl`/`*.raw.jsonl` are gitignored, only sample fixtures tracked.
- **Revocable, epoch-bound capability lifecycle (PORTICO, arXiv:2606.22504).**
  New `crates/duduclaw-gateway/src/capability.rs` upgrades HITL from a *one-shot*
  `request → decide` (where a human approval was a **permanent** grant that was
  never taken back) to `request → grant → invoke`. Approving now mints a
  **`CapabilityGrant`** — an epoch-bound handle tied to a task/session subgoal
  (`scope_epoch`) via `CapabilityBroker::grant` / `grant_from_approval`. A tool
  runs `invoke(handle)` before acting; when the subgoal closes,
  `close_scope(epoch)` **auto-revokes** every handle bound to it, so any later
  `invoke` of the same handle is denied (PORTICO's post-closure "N/N blocked").
  Fully **fail-closed**: unknown handle → `NotFound`, revoked → `Revoked`,
  past-TTL or unparseable expiry → `Expired`, closed scope (even if the row's
  `revoked_at` wasn't yet stamped) → `ScopeClosed`, store error → `Store` — never
  a silent allow; granting *into* an already-closed scope is rejected up front
  (stale-write guard). Storage shares the `approvals.db` file with the approval
  store but owns two tables (`capability_grants`, `capability_closed_scopes`;
  WAL, parameterized SQL only). **Wired:** `approvals.decide` mints a grant on
  approve when the approval payload carries a `scope_epoch` (optional
  `capability_ttl_seconds`); `tasks.update → status=done` calls `close_scope(task_id)`
  to auto-revoke that subgoal's capabilities. Session-end auto-close and the
  MCP-tool `invoke` gate expose the API (`close_scope` / `invoke`) but their
  call-sites are follow-up wiring. 12 unit tests cover grant→invoke, post-closure
  reuse denial, expiry, unknown/revoked handles, and the stale-write guard.
- **Painless migration `duduclaw migrate-from <openclaw|hermes|paperclip>`.**
  One command to move from the three big competitor platforms into DuDuClaw.
  New `crates/duduclaw-cli/src/migrate_from/` module (`openclaw.rs` / `hermes.rs`
  / `paperclip.rs` / `report.rs` + shared `mod.rs`). **Default is a dry-run** that
  prints the migration plan; `--apply` performs the writes; `--rename` imports
  under a `-imported` suffix on a name clash instead of skipping. Every item is
  reported honestly — `IMPORTED` / `PARTIAL` / `SKIPPED(reason)` /
  `CONFLICT(reason)` — rolling up to `COMPLETE` / `DEGRADED` / `PARTIAL`; the
  report is also written to `~/.duduclaw/imported/<platform>/migration-report.md`
  and all token values are masked (first 4 + last 4). Maps source agents
  (workspace `SOUL.md` → `SOUL.md`, `MEMORY.md`/`USER.md` bullets → Semantic
  memory tagged `imported-from-<platform>`), channel tokens (telegram/discord/slack
  → AES-256-GCM-encrypted config.toml `[channels]`, **never overwriting** an
  existing token → `CONFLICT`), the Anthropic API key, `[model] preferred`
  (strips the `anthropic/` prefix; non-Claude models flagged `PARTIAL` for manual
  review), legacy cron `jobs.json` (defensive parse → SQLite cron store), and
  skills (each `SKILL.md` runs through the prompt-injection scanner **before**
  install — a flagged skill is `SKIPPED(security)`, fail-closed). paperclip goes
  via the official `paperclipai company export` directory (`--source` required):
  `reportsTo` → `reports_to` with topological creation order + cycle detection,
  `TASK.md` → Task Board, `recurring` → cron, `COMPANY.md` → shared wiki. Original
  session/conversation files are archived verbatim to
  `~/.duduclaw/imported/<platform>/raw/` (v1 does not parse them into
  `sessions.db`). The `agent create` scaffold logic was extracted into a shared
  `scaffold_agent_dir` helper so imported agents and hand-created agents stay
  byte-compatible. See `docs/guides/migrate-from.md`.
- **Dynamic runtime model discovery (`runtime_models.rs`).** The dashboard model
  picker was backed by a hard-coded, hand-edited cloud model list in
  `handle_models_list` that drifted stale (Claude listed "opus-4-6" with no Fable;
  codex/gemini hard-coded too). Replaced with a per-provider discovery chain that
  probes the *real* installed CLIs / APIs and caches to
  `~/.duduclaw/runtime_models.json` (12h background refresh + startup probe).
  Discovery chain per provider returns `{models, source, fetched_at}` where
  `source ∈ live_api / cli_probe / help_parse / pty_probe / fallback`:
  **claude** → Anthropic `GET /v1/models` when an API key is configured (10s
  timeout) → parse `claude --help` `--model` aliases (5s timeout, stdin closed)
  → static fallback (marked); **codex / gemini / agy** → best-effort `--help`
  `models` subcommand probe (≤5s, stdin closed) → static fallback. All CLI probes
  close stdin + hard-timeout + `kill_on_drop` so a probe can never drop into the
  interactive REPL or hang. Optional `pty_probe` source (drives the interactive
  `/model` menu) is **default OFF**, opt-in via `config.toml [models] pty_probe`.
  New `models.refresh` RPC (login-readable) forces a live re-probe. Each
  `models.list` entry now carries `provider` / `source` / `fetched_at`; the
  picker shows "updated N ago" + a 🔄 refresh button and flags fallback groups as
  "（預設清單，未能即時取得）". Live discovery failures never fabricate a
  live-looking list — they surface the static fallback, clearly marked.

- **Gamification growth persistence (V10-T10.0).** New `growth.rs` — SQLite
  `~/.duduclaw/growth.db` (WAL) storing only facts (achievement unlock
  timestamps, an XP-snapshot audit log, a per-day daily-report cache). A **pure**
  judging engine (`compute_snapshot`, fully unit-tested, byte-identical on
  recompute) scores real internal surfaces (tasks/skills/wiki/cron/custom-skills)
  into XP (task +12 / skill +25 / knowledge page +8 / routine run +5 + one-time
  achievement bonuses) and `Lv = floor(sqrt(XP/100))`. Declarative achievement
  table; sources we cannot read honestly (`inbox_zero_streak_7`,
  `custom_skill_saved_100h`) surface as `available: false` with a documented
  reason instead of a fabricated estimate. New RPCs `growth.snapshot` /
  `growth.daily_report` (login-readable, non-admin).
- **Human × agent custom skills backend (V13-T13.0).** New `custom_skills.rs` —
  SQLite `custom_skill_registry` with a `draft → generating → pending_approval →
  approved / rejected / retired` state machine (illegal transitions refused).
  Pre-approval `SKILL.md` bodies are quarantined in `~/.duduclaw/skills-drafts/`,
  which is **never** a skill-loader scan root (isolation asserted by test). Six
  RPCs `skills.custom_create / custom_generate / custom_update / custom_submit /
  custom_list / custom_retire`; generation reuses the existing `bus_queue`
  delegation channel. Submit runs the mandatory `scan_skill` safety pass
  (includes prompt-injection) — **high/critical risk is refused (fail-closed)**;
  a pass routes to the shared `ApprovalBroker` (`action_kind = "skill_create"`,
  7-day TTL = DENY on expiry). Approval side-effect installs the draft into the
  real global skills dir; deny marks the row rejected with a reason. Single-admin
  self-approval is audited (`self_approved`). Fail-closed unit tests cover all
  three invariants (TTL-expiry = deny, high-risk cannot submit, drafts unscanned).
- **Single-binary commercial upgrade.** A stock `duduclaw` verifies a signed
  `~/.duduclaw/license.json` out of the box — no separate `duduclaw-pro` binary.
  `license_runtime::production_registry()` bakes the production issuer public key
  (env `DUDUCLAW_LICENSE_PUBKEY_*` still overrides; empty/malformed baked key
  fails safe to OpenSource). Upgrade path: drop in `license.json` → restart.
- **Perpetual (buy-out / OEM) licenses.** `license-keygen issue --perpetual`
  issues a no-expiry license (100-year term); mutually exclusive with `--days`
  (clap-enforced). Term licenses (`--days N`) unchanged.
- **Proactive license-expiry warnings.** Gateway logs a warning in the 30/7-day
  pre-expiry window (at boot + daily via the CRL loop); the dashboard shows a
  cross-page `LicenseExpiryBanner` (warning ≤30d / critical ≤7d / expired),
  complementing the passive LicensePage countdown. Thresholds shared with the
  gateway (`classify_expiry_urgency`). Unit-tested.
- **Dashboard: per-AI-staff live status glyph (WP10-T10.2).** CSS-animated
  presence dots on the roster (`AgentStatusGlyph` + `agent-activity-store`):
  idle / 回覆中 / 工具執行中 / 背景固化 / 等待審批, derived non-invasively from the
  existing `activity.new` + `browser.approval_request` WS events (transient TTL
  decay, no new backend truth source). Reduced-motion safe. Unit-tested.
- **Dashboard: owner home incident banner (WP14-T14.2, partial).** A red
  "需要你關注" strip that stays fully silent when all is well and deep-links each
  incident chip to its page. Ships with paused-AI-staff + offline-channel
  sources (existing read paths); budget/approval sources await their read RPCs.
- **Dashboard: activity feed three-tier denoising (WP14-T14.3).** Headline vs
  secondary vs routine tiers — routine per-message chatter is hidden behind a
  "顯示全部細節" toggle, and ≥3 consecutive same-AI-staff updates fold into one
  "N 筆連續更新" row. Unit-tested.
- **Suspected-private-use detection guards (WP6-T6.4b, core).** Labour-relations
  sensitive, so the false-positive guards ship first as `workforce_private.rs`
  (opt-in, off by default): fail-closed with no operator business-scope baseline;
  only high-confidence "suspected private" is flagged ("undetermined" never is);
  exempt list; flag TTL auto-expiry (default 30 days, unparseable timestamp =
  expired). Advisory only — never grounds for discipline, never employee-visible.
  Unit-tested. Haiku classification batch + operator-only UI are the follow-up.
- **CEO/Board governance mode (WP17, core + ADR).** Opt-in `[governance] board_mode`
  (default off — solo deployments unchanged). New `governance.rs`: a typed
  `ApprovalKind` (serde-compatible with the stored `action_kind` strings; also used
  by WP8/WP16) plus fail-closed invariants — `StrategicPlan`/`AgentHire` are
  Board-human-only (an agent identity is refused, "Board = human"), Initiatives are
  board-human-created only, and in board_mode no agent may edit `[budget]` via MCP
  (anti self-promotion). Unit-tested. Design: `docs/adr/ADR-007-board-governance-mode.md`.
  Strategic-proposal flow + Board panel + cascade budget are follow-up integration.
- **LINE OA B2C credit metering (WP7, core).** `LineChannelConfig` gains an
  additive `[[channels.line.accounts]]` array (multi Official Account, each bound
  to an agent with a `credit_rate`); legacy single-OA config still works via
  `resolve_accounts()`. New `credit.rs` ledger (`credits.db`): per-`(oa, user)`
  points balance + append-only events, atomic deduct, fail-closed gate (balance ≤0
  ⇒ no LLM call), `tokens_to_points`. `duduclaw credit grant|balance|history` CLI
  for operator top-ups (PayUni settlement is separate). Webhook `destination`
  routing + per-account signature verify is the follow-up integration.
- **Channel-side approval buttons (WP16, core).** `RichComponent::Buttons` +
  `ActionButton`/`ButtonStyle` model for cross-platform action buttons, plus
  `channel_approval.rs`: the `approval:<id>:<approve|deny>:<nonce>` action-id codec
  (fits Telegram's 64-byte callback cap), fail-closed exact-match approver
  authorization (a forwarded button can't be actioned by the recipient), and a
  one-time nonce against replay. Pure + unit-tested; per-platform native render
  (TG inline keyboard / Slack Block Kit / Discord components / LINE quick reply)
  and the four click-event routes are the follow-up integration.
- **Delegation permission decay — "narrower wins" (WP4, core).** New
  `delegation_scope.rs` defines the permission-snapshot shape carried across a
  delegation hop and the intersection rule: allow-lists intersect (empty = no
  restriction, so the restrictive party wins), deny-lists union, Odoo model/action
  allow-lists intersect — so agent A delegating to a wider-privileged agent B can
  never widen what A could reach. Depth cap prevents unbounded hops. Pure,
  fully unit-tested; dispatcher/Odoo runtime wiring is the follow-up integration.
- **Per-user cost attribution (WP6) — "which employee is spending?".** `token_usage`
  gains additive `user_id` + `channel` columns (idempotent migration); the channel-reply
  path attributes spend to the end user via new `record_attributed` + `CHANNEL_REPLY_USER_ID`
  task-local. New `summary_by_user` query + admin-scoped `cost_users` MCP tool rank users by
  cost; non-human traffic buckets under `(system)`. Guide: `docs/guides/workforce-analytics.md`.
- **Skills speak the employee's language (WP8).** `SkillMeta` gains a `display`
  map (`zh-TW`/`en`/`ja-JP` → localised name+description) with a
  `locale → zh-TW → original` fallback chain, so non-English-reading employees
  see what a skill does. Presentation (`skill_list`, spec) uses it; skills
  predating the field render unchanged. Plus a skill-activation approval helper
  (`skill_approval.rs`, `action_kind = skill_activation`) that carries the
  "省多少分鐘?" estimate (`estimated_minutes_saved`) into the manager's Approval
  Inbox — the data source for the WP10 leaderboard. Spec: `docs/spec/skill-md-spec.md`.
- **Shared-wiki `agent_allowlist` namespace mode — "who may write" control.** `.scope.toml`
  gains a fourth mode: `mode = "agent_allowlist"` + `agents = ["agnes", "boss"]` restricts a
  namespace's writes to those exact agent ids (exact-equality, no substring), operator always
  allowed. Empty list is fail-closed (denies every agent). Honoured by both `shared_wiki_write`
  and `shared_wiki_delete`; surfaced in `wiki_namespace_status`.
  `crates/duduclaw-cli/src/wiki_scope.rs`.
- **`duduclaw redaction verify` — prove de-identification works, don't just claim it.**
  Runs a CSV/text file through the REAL redaction pipeline (vault writes included)
  and emits a Markdown evidence report: every hit (masked original `王**` × rule id
  × token × category), `PASS-THROUGH` lines, and a reversibility check that restores
  each token and asserts it round-trips (`restore OK n/n`). Ships a demo dataset at
  `docs/examples/redaction-sample.csv`. `crates/duduclaw-cli/src/redaction_verify.rs`.
- **Keyword redaction rules (no regex needed).** The `keyword` rule kind is now
  compiled by the engine — operators add a customer name (`Amazon`, `台積電`) via
  `type = "keyword"` and it is redacted with whole-word, CJK-safe matching
  (ASCII terms respect word boundaries; CJK terms match as substrings).
  `crates/duduclaw-redaction/src/rules/keyword.rs`.
- **`code_map` MCP tool — Aider-style repository symbol graph.** tree-sitter
  symbol extraction (Rust/Python/JS/TS/TSX) over the existing HippoRAG-lite
  Personalized-PageRank engine (`graph_rank.rs`): ranks a repo's source files by
  relevance to a query, with cross-file reference edges (def weight 4, ref weight
  1) and `chat_files` personalization. `crates/duduclaw-memory/src/code_map.rs`;
  MCP tool gated by `MemoryRead`, excluded from the external whitelist.
- **Semantic vector memory retrieval (`w_vec`).** Third re-rank signal alongside
  `w_fts`/`w_graph`: pluggable `EmbeddingProvider` with a zero-dependency,
  CJK-safe, stable char-n-gram default (`NgramHashEmbedder`); embeddings stored
  as additive `embedding` BLOB columns; brute-force cosine KNN respecting
  agent/temporal isolation and embedder-identity binding. Opt-in via
  `DUDUCLAW_SEMANTIC_VECTORS=1`. No signal ⇒ ranking byte-identical.
  `crates/duduclaw-memory/src/vector.rs`.
- **Budget circuit breaker — cost enforcement, not just observation.** Hard
  per-agent rolling-window spend caps (`[budget] daily_cap_cents` +
  `monthly_limit_cents`) that block new LLM calls at the dispatch choke-point and
  reply with a zh-TW notice; writes `budget_events.jsonl`. Fail-open on telemetry
  outage. `crates/duduclaw-gateway/src/budget.rs`.
- **MCP Bridge — mount external third-party MCP servers.** `[[mcp.external]]` in
  `agent.toml` spawns external MCP servers (Plane/Chatwoot/Gmail/…) alongside the
  internal server, with a deny-by-default per-server tool allow/deny filter
  (`duduclaw_llm::ToolFilter`), `env://` credential resolution, and fail-safe
  skip/degrade. `crates/duduclaw-gateway/src/mcp_external.rs`; guide at
  `docs/guides/mcp-bridge.md`.
- **Audit export + SIEM sink.** `duduclaw audit` aggregates the existing JSONL
  audit trails (security / tool-calls / channel-failures / budget) into a
  normalized, absolute-time-sorted NDJSON stream with `--since` filtering,
  writable to a file and/or POSTable to a SIEM/webhook.
  `crates/duduclaw-gateway/src/audit_export.rs`.
- **Output guardrail hook** (opt-in `[guardrails]`): scans the outbound reply
  for leaked secrets, prompt-injection echoes, and operator deny-phrases, and
  redacts PII — blocking/redacting before send. Deterministic default (Llama
  Guard is the documented upgrade). `crates/duduclaw-gateway/src/guardrail.rs`.
- **Burn-rate cost anomaly detection**: rolling mean+stddev over an agent's
  per-day spend flags statistical outliers (relative baseline, not a fixed
  threshold). `crates/duduclaw-gateway/src/cost_anomaly.rs` +
  `CostTelemetry::daily_cost_millicents`.
- **Security posture report**: `duduclaw security` scores active protections
  (fail-closed MCP auth, signed updates, injection scanning, HITL, hooks,
  no-plaintext-secrets, budget caps) as a checklist + weighted score.
  `crates/duduclaw-gateway/src/security_posture.rs`.
- **CI red-team scan**: `duduclaw redteam` synthesizes jailbreak prompt variants
  from an agent's `CONTRACT.toml` `must_not` rules and reports which the
  deterministic input-guard catches. `crates/duduclaw-gateway/src/redteam.rs`.
- **`duduclaw backup` / `duduclaw restore`**: timestamped home archive with a
  SHA-256 sidecar verified on restore (fail-closed on mismatch).
- **`duduclaw session replay <id>`**: print a stored session's turns in order
  (with `--tools` to interleave tool-call audit lines).
- **ADR-003**: records the decision to exclude Signal / personal WeChat / Viber
  channels (`docs/adr/ADR-003-excluded-channels.md`).
- **`duduclaw gdpr export|erase <contact>`**: data-subject requests over the
  memory store. Export = a JSON bundle of every row referencing the contact (as a
  triple subject/object or a free-text mention, LIKE-escaped). Erase = a single
  transactional hard delete across `memories` + `memories_fts` + `key_facts` +
  `key_facts_fts` (no FTS orphan), recording a SHA-256-pseudonymised erasure
  tombstone. `--confirm` gates deletion (dry-run preview otherwise).
  `crates/duduclaw-memory/src/gdpr.rs`.
- **`duduclaw memory bench`**: times HippoRAG-lite Personalized-PageRank over the
  live triple count and prints P50/P95 + a partition recommendation (the LightRAG
  subgraph-partition gate — measure before building). Thresholds: ≥10k triples or
  P95 ≥50 ms. `crates/duduclaw-memory/src/bench.rs`.
- **Cross-session user profile.** Per-user preference traits stored via temporal
  supersession (`subject = "user:<id>"`), a deterministic (prompt-cache-stable)
  `## About This User` render, and reflexion-style consolidation into one durable
  `profile_summary`. `crates/duduclaw-memory/src/user_profile.rs`.
- **MCP/skill trust tiering.** `classify_trust_tier` derives an official / active
  / orphan tier from a repo's last-push age + owner type + stars; `SkillIndexEntry`
  now carries `pushed_at` / `owner_type` / `stars` / `trust_tier` so users are
  steered away from abandoned MCP servers. `crates/duduclaw-agent/src/trust_tier.rs`.
- **Secret manager: 1Password Connect + Infisical backends**, and `secret://`
  resolution wired into the MCP Bridge. `[[mcp.external]]` credentials may now be
  `secret://<backend>/<name>`, resolved at spawn time against the configured
  secret manager; an unresolvable ref drops the server fail-safe (mirrors
  `env://`). New read-only adapters (`onepassword.rs`, `infisical.rs`) with
  fail-closed `put`/`delete`.
- **`## About This User` reply injection + `user_profile_record` /
  `user_profile_get` MCP tools.** The cross-session user profile is now
  end-to-end: an agent records preference traits, they render into a
  session-stable `## About This User` block injected into that user's future
  replies (next to Past Mistakes / Learned Rules), and are readable back. Agent
  scope is the server-injected namespace, never a client param.
- **GDPR erase/export now also covers the session store.** A `duduclaw gdpr`
  request matches sessions by the `<channel>:<chat_id>` session-id prefix (and its
  threads) and hard-deletes the matching `sessions` + `session_messages`
  transactionally; export includes the session turns.
  `SessionManager::sessions_for_contact` / `erase_sessions_for_contact`.
- **Email channel (SMTP send + inbound parse).**
  `crates/duduclaw-gateway/src/email.rs`: async SMTP send via `lettre` (rustls;
  STARTTLS / implicit-TLS / plaintext), a dependency-free RFC822 inbound parser
  (`parse_inbound`, header-unfolding), and `[channels.email]` config. Doubles as a
  fail-safe alert sink. Send path is loopback-verified; the IMAP poll transport +
  gateway channel-lifecycle wiring are the documented PENDING-LIVE remainder.
- **MCP Bridge SaaS recipes** in `docs/guides/mcp-bridge.md`: per-service
  `[[mcp.external]]` config for Gmail/Calendar, Plane, Invoice Ninja, Chatwoot,
  WooCommerce (+ DocuSeal / Monica notes), each with credential provisioning and
  `approval_required_tools` guidance for write tools.

### Changed
- **`License` 新增可選 `control_url` 欄位（相容性：向後相容）。** 白牌 §10.5 讓 issuer
  金鑰自帶續期端點。此欄位 **不進 canonical_payload**（與 `signature` 同級排除，不影響簽章），
  且 `#[serde(default, skip_serializing_if="Option::is_none")]`：舊 `license.json`（無此欄位）
  讀出即 `None`，行為不變；新舊 binary 互讀相容（License 無 `deny_unknown_fields`）。控制面
  URL 解析順序改為 `DUDUCLAW_CONTROL_URL` env > `license.control_url` > 內建預設（gateway
  `license_runtime` 與 CLI `license refresh` 同步）。`resign_license_for_refresh` 保留原
  `control_url`。
- **World stage is a PixiJS 2D isometric scene, now with an immersive full-width
  `/world` page.** After live testing, the interim three.js real-3D renderer was
  dropped in favour of a 2:1 isometric PixiJS renderer (`stage-scene.ts`): a
  batched floor/walls ground layer (town bakes it via `cacheAsTexture`), a
  depth-sorted actors layer for agents + ambient cars, per-agent characters with
  always-on nameplates, head-top emotes and fading CJK-safe speech bubbles, and
  eight-colour town buildings with lit windows. The camera is 2D (no rotation):
  cursor-anchored wheel / pinch zoom (0.5×–2.5×), bounds-clamped drag pan, and a ⟲
  recenter that snaps back to the contain-fit framing. Beyond the compact 38vh
  Home band (which gains a ⤢ 展開 link), the world now has a dedicated full-bleed
  `/world` page (openhuman Tiny Place style — edge-to-edge canvas, floating ROOM
  scene panel top-right, info card top-left) reachable from the sidebar (員工/公司
  → 世界) and the Org "世界" tab (now a link so the heavy scene mounts in one
  place). CSP note: PixiJS's WebGLRenderer uses `new Function` for uniform sync,
  so the renderer imports `pixi.js/unsafe-eval` eagerly alongside `pixi.js` to run
  under the dashboard's `script-src 'self'` CSP; WebGPU is forced off
  (`preference:'webgl'`) with a 10s init timeout. pixi.js loads in a lazy chunk
  (dynamic `import()`) — the main bundle is unchanged. The state / behaviour layer
  (`useWorldState`, `SCENES`, `traffic`, degradation chain, click→route map, scene
  persistence) is unchanged.
- **Dashboard: single Home spine (workspace/dashboard shell modes removed).**
  The `ui-mode-store` toggle, `WorkspacePage`, and `ModeToggle` were dropped;
  Home is the only index and carries the one-line launcher (`PromptBar`) hero at
  the top, which hands off to `/webchat` (shared chat session). `/workspace`
  aliases to Home so old bookmarks keep working.
- **Licensing: issuer key rotated v1 → v2.** v1's private key was unaccounted-for
  on the issuing side, so v1 trust was removed from the binary and a fresh v2
  issuer keypair (private key held offline) replaces it (`PROD_ISSUER_KEY_ID = "v2"`,
  v2 public key baked). No customer impact — no v1 licenses were issued.
- **Dashboard navigation regrouped into four owner-oriented groups
  (WP14-T14.1).** 總覽 / 工作 / 團隊 / 公司, replacing the previous six groups
  (代理→團隊, 知識 folded into 工作, 整合+營運+系統 folded into 公司). **All routes
  unchanged** — only grouping and labels moved, so bookmarks keep working.
- **Dashboard terminology pass toward "AI 員工" (WP14-T14.9, partial).** zh-TW UI
  copy on the primary owner surfaces (nav labels + descriptions, AI-staff roster,
  org chart, marketplace/reliability/wiki-trust/skill-synthesis) now says
  「AI 員工」instead of Agent/代理; measure table recorded in `web/DESIGN.md` §5.
  Deeper admin/edit-dialog strings remain on the follow-up list.
- **Sessions are soft-deleted, not destroyed (WP5).** `delete_session` now
  ARCHIVES (sets `archived_at`, keeps messages, hides from normal listings) — the
  conversation stays replayable and searchable. Real, irreversible deletion is the
  new `purge_session`. `cleanup_inactive` archives inactive sessions and only
  purges them after a 90-day retention window. New `record_message_session` /
  `session_for_reply` back per-task session resume: replying to a specific bot
  message resumes that task's session (`message_session_map` table, additive).
  **Behaviour change**: anything that called `delete_session` expecting a hard
  delete now archives instead — use `purge_session` for the old behaviour.
- **LINE inbound voice messages are now transcribed** to text (mirrors the
  existing Telegram path) via `duduclaw_inference::whisper::transcribe`, folded
  into the agent's input; the saved attachment reference is retained so a
  keyless/failed transcription degrades gracefully.
- `BudgetConfig` gains `daily_cap_cents` (`#[serde(default)]`, backward-compatible).
- MCP Bridge `env` values now accept `secret://` refs in addition to `env://`
  (see Added). `SecretBackend` gains `OnePassword` / `Infisical` variants.

### Security
- **收尾波複查修正（2026-07-12）。** Live Canvas 補自包含 srcDoc CSP（`default-src 'none';
  img-src data:`）——關掉 CSS `background:url()` 的外連信標通道（沙箱已擋 script，此層擋
  網路 egress，使 Canvas 成為全離線視覺面）。
- **P1 尾輪雙鏡頭複查修正（2026-07-12）。** skill 安裝 `meta.name` 路徑穿越（掃描乾淨的
  skill 可覆寫其他 agent 的 SOUL.md——全安裝路徑單點消毒，CRITICAL）；全通道
  `handle_command` 的 `is_admin=true` hardcode（任何群組成員可 `!STOP ALL` 全域停機——改
  `admin_users` 頻道設定，fail-closed 無名單即無管理員，CRITICAL）；指令攔截點前置中央
  存取閘（pairing/allowlist 不再被繞過）；export 不再跟隨 symlink＋contract/skill 目錄
  全檔 scrub＋token 形狀擴充；LobeHub manifest SSRF 允許清單；hub 安裝暫存檔移出共享
  /tmp；agents.update 通道密鑰不再以明文複活；全通道 config 密鑰 enc-only（presence
  檢查改 `_enc`-aware，移除 TG/DC/LINE 最後三個明文例外）；`spawn_ephemeral`/
  `spawn_agent` bus 入列補跨程序檔鎖；ephemeral 32 上限防 TOCTOU；`/handoff` 跨使用者
  對話外洩（歧義即拒）。
- **雙鏡頭複查修正（2026-07-11 第二輪）。** WeCom `corpsecret`/access_token 與 DingTalk
  sessionWebhook token 不再經 reqwest 錯誤 URL 洩漏到 log/dashboard（`scrub_reqwest_err`
  全站套用）；wecom/dingtalk 密鑰不再以明文與 `_enc` 並存於 config；
  `dingtalk_sessions.json` 與 `teams_conversations.json` 改 0600 原子建檔；DingTalk
  sessionWebhook 加 `*.dingtalk.com` 錨定允許清單（sign 不覆蓋 body 的 SSRF 面）；
  WeCom 回調加 ±1h 重放窗；夜間 LLM spawn 由預設 capabilities（skip-permissions 全工具
  面）收緊為零工具 allowlist＋注入記憶 `<data>` 降格；N3/N4 夜間升格記憶 importance
  封頂 5.0＋來源 tag（重複注入內容無法壓過 curated 條目）；`complete_task`/`block` 補
  claim-holder guard（殭屍工人不能蓋掉新持有者結果）。

### Fixed
- **去識別化外部系統移除後不再靜默復活（2026-07-12）。** `redaction.update` 的
  `tool_egress` 是 upsert-merge（僅 `null` 值移除，缺席 key 不動）。`RedactionTab` 存檔原本
  只送當前 map,移除某外部系統/工具規則後該 key 只是缺席→後端不刪除→重載復活。改為存檔時
  對「先前存在但現已移除」的 key 明確送 `null`,並在載入/存檔後更新基準 key 集。
- **P1 尾輪雙鏡頭複查修正（2026-07-12）。** MoA 在派工/cron 路徑原會毒化共享帳號池
  （rotator 之前即攔截＋direct-API 正確路由＋成本入 telemetry＋對話 history 接通）；
  JitRL 回饋零 caller＋record/generate model key 不一致（已接 MCP 工具＋統一 key）；
  audit 輸入捕捉零 caller 死碼（已接中央稽核站）；curator 會封存從未被 stamp 的活躍
  skill（無使用訊號改 stale-only）；`run_at_risk` 缺席 autopilot trigger 白名單；Slack
  指令 session key 錯位（幽靈 session）；handoff 佈告 marker 順序（`/rollback` 不再能
  整段吃掉匯入歷史）；runs.get／儀表板計數補 tombstone 過濾；proactive_timing 排除已
  撤銷輪；channel_reply 兩處 CJK 不安全位元組切片；curator 巢狀配置不再每日誤報。
- **雙鏡頭複查修正（2026-07-11 第二輪）。** 依賴閘控死碼（`claimable_tasks` 零呼叫者
  ⇒ 已 fold 進 `atomic_claim` 單一交易）；外部 provider seat 不再壓掉 anthropic OAuth
  自動偵測（channel reply 全斷回歸）；API-key qwen 帳號不再被 seat 分支劫持成 503；
  wecom/dingtalk 補 `create_sender` factory arm（cron/OTP/computer-use 不再靜默丟訊息）；
  goals/depends_on 循環檢查包 IMMEDIATE txn（TOCTOU）；zombie reclaim 加
  `lease_expires_at` CAS；`tasks_claim` legacy fallback 不能偷走已指派任務；timeline
  `truncated` 旗標修正＋`taskStatus.failed` 三語補齊；cli MCP `VALID_CHANNELS` 補
  wecom/dingtalk。
- **Evolution "off" now really stops evolution (master kill-switch).** Turning
  off self-evolution previously left two bypass paths — the heartbeat
  silence-breaker and the channel prediction path never checked the toggle, so a
  "disabled" agent kept reflecting and rewriting `SOUL.md`. `[evolution] enabled`
  (default `true`, backward-compatible) is now a single master switch enforced at
  every trigger point: GVU trigger, heartbeat silence-breaker, channel prediction
  path (skill synthesis/graduation + GVU), sub-agent dispatch, and the
  skill-synthesis auto-run scheduler. Prediction-error logging (passive telemetry)
  still runs. New `duduclaw agent freeze|unfreeze <id>` one-shot enterprise
  escape hatch (also disables heartbeat, writes an audit record). Guide:
  `docs/guides/evolution-switches.md`.
- **Dashboard secret-manager backend validation drift**: the settings handler
  accepted `config` / `keychain` (backends that do not exist) and *rejected*
  `local` (the default), so any valid backend selection failed. Now validates the
  real enum: `local` / `vault` / `env` / `onepassword` / `infisical`
  (`handlers.rs`).
- **GDPR erase closes an FTS-orphan leak**: the delete cascades `key_facts_fts`
  alongside `key_facts` (the pre-existing `purge_stale_facts` path leaves the FTS
  row behind); documented as a follow-up sweep target.
- **Infisical `exists()`** no longer folds auth/network failures into
  `Ok(false)` — only a genuine 404 is `false`; other errors propagate
  (fail-closed, matching the 1Password adapter).

## [1.35.0] - 2026-07-07 — True auto-update, Ed25519-signed releases, channel UX

### Added
- **Per-channel rich rendering & live progress**: platform-native markdown
  rendering (Telegram HTML / Slack blocks / WhatsApp markup / Feishu Card 2.0 /
  Google Chat / Teams / LINE plain text, CJK-width-aware monospace tables),
  typing indicators on all channels, and a live 📋 task-progress board for
  long-running jobs (parsed from Claude `TodoWrite` events, edited in place).
- **Google Chat and Microsoft Teams channels** (JWT-verified webhooks,
  service-account / Bot Framework proactive sending) — nine channels total.
- `web_fetch` (L1) / `web_extract` (L2) MCP tools wired to the browser
  auto-routing ladder; `slack` / `access_control` modules reconnected
  (previously dead code never declared in `lib.rs`).
- **True auto-update with in-process restart**: after a self-update installs
  (dashboard "Install update" button or the 6-hourly checker with
  `[gateway] auto_update = true`), the gateway now re-execs the new binary
  after graceful shutdown (`platform::self_restart()` — `execv` on Unix keeps
  the same PID so launchd/systemd supervision is undisturbed; Windows spawns a
  detached replacement). Works for unsupervised foreground runs (npm wrapper,
  `duduclaw run`) too — previously the process just exited and stayed down.
- **minisign Ed25519 release signatures**: every release asset is signed in CI
  (`MINISIGN_SECRET_KEY`) and the updater verifies the `.minisig` against a
  public key pinned in the binary — hard fail-closed; a compromised GitHub
  release without a valid signature can no longer install. SHA-256 sidecar
  check retained as defense in depth.
- Dashboard: `system.update_installed` is now broadcast on manual installs
  too; all tabs show a "restarting" banner and auto-reload once the updated
  gateway is back, so the new embedded dashboard assets load automatically.
- `InstallMethod::Npm` detection (binary under `node_modules/`) — self-update
  supported; npm registry metadata goes stale until the next `npm i -g`.

### Changed
- README rewritten in all three languages (zh-TW / en / ja): plain-language
  opening, 5 badges, one feature-overview table with links into `docs/`,
  refreshed facts (9 channels, 5 runtimes, signature verification).

### Removed
- Dead modules `analytics.rs` / `browserbase.rs` / `input_guard.rs`
  (superseded by the v1.34 security layer; no call sites).

### Fixed
- Updater never matched real release assets: `platform_asset_suffix()` looked
  for `arm64-apple-darwin.tar.gz`-style names while CI publishes
  `duduclaw-darwin-arm64.tar.gz` — update checks always reported "no download
  for this platform".
- Windows `.sha256` sidecar was written in PowerShell `Format-List` layout the
  updater could not parse; CI now writes standard `hash  filename` and the
  parser tolerates both.
- systemd unit template: `Restart=on-failure` → `Restart=always` (self-update
  exits 0 after graceful shutdown, which `on-failure` would not relaunch).
- Linux: re-exec path is pinned before the binary is replaced
  (`/proc/self/exe` reads `… (deleted)` after the update swaps the file).
- Pairing flow lock-in bug (stale pairing lock blocked re-pairing).

## [1.34.0] - 2026-07-06 — Runtime-agnostic security reference monitor

Moves the security boundary off prompts/hooks/config and onto deterministic
choke points and OS primitives. The MCP dispatch path becomes a true reference
monitor (complete mediation, tamper-proof, verifiable) so every runtime —
Claude / Codex / Gemini / Antigravity, plus the direct-API and local-inference
tool loops — is governed by the same zero-LLM policy. Every new control is
fail-closed and (where opt-in) backward compatible. New `duduclaw-sandbox`
crate; ~90 new tests, zero workspace warnings.

### Added
- **PolicyKernel reference monitor** (`duduclaw-security`): deterministic,
  zero-LLM `evaluate()` over a parameter-level static tool policy
  (`agent.toml [capabilities] policy`, Progent-style tool+arg matcher). Canonical
  `fs_write`/`shell_exec`/`mcp_call` families give one rule uniform reach across
  runtimes; precedence Forbid > Ask > Allow > default-deny; empty policy abstains
  (backward compatible). Wired into MCP dispatch (Ask → ApprovalBroker,
  TTL-expiry = deny) and the direct-API/local tool loop (`PolicyExecutor`).
- **Egress "secret in-use"** at the shared MCP choke point: `<REDACT:…>` tokens
  restored only for whitelisted tools, everything else denied (`-32007`), results
  re-redacted — now covering stdio **and** HTTP/SSE transports uniformly.
- **Native OS process sandbox** (`duduclaw-sandbox`, opt-in
  `[capabilities] native_sandbox`): confines the spawned agent CLI via macOS
  Seatbelt (live-verified) / Linux Landlock, derived from `SandboxLevel`;
  fail-closed when required but unavailable.
- **Origin-bound memory trust** (`duduclaw-memory`): temporal memories gain
  `origin`/`origin_trust`/`derived_from`; a derived fact's trust is clamped to
  ≤ min(source trusts) — non-malleable, can't be laundered upward. Distilled
  conversational facts are marked lowest-trust; search down-weights accordingly.
- **CONTRACT.toml runtime enforcement**: `must_not` boundaries validated on the
  final user-facing reply bytes (after secret restoration); violations blocked
  and audited.
- **SecurityPosture** state machine ({Green,Yellow,Red}, escalate-fast /
  decay-slow) + **OS ground-truth reconciliation** (`os_reconcile`: pure
  two-way diff of tool-call claims vs observed OS effects; macOS `eslogger`
  parser; Linux eBPF staged).

### Changed
- MCP dispatch pipeline now runs injection scan → PolicyKernel → egress at one
  shared choke point, so HTTP/SSE get the same enforcement as stdio.
- Antigravity spawn derives `--sandbox` / `--dangerously-skip-permissions` from
  `SandboxLevel` (never both) instead of unconditional skip-permissions.

### Fixed
- Egress domain filtering is fail-closed: an empty/invalid allowlist now denies
  all egress (`--network=none` / deny-all) instead of leaving the network
  unfiltered; allowlist entries are canonicalized (reject control bytes, `%`,
  CRLF, IP-literals).
- Inbound prompt-injection blocks on the channel reply path are now audited.


## [1.33.0] - 2026-07-05 — Model-agnostic core + AI harness infrastructure

The largest structural release to date, in three movements: a deep prune
(−19k lines of provably-orphaned code), a research-driven upgrade pass over
memory/routing, and the promotion of "Multi-Runtime" from a text shell into a
genuinely model-agnostic platform with 2026 harness table stakes. Net diff vs
v1.32.0 is roughly line-neutral (+20k/−21k) — redundancy traded for
infrastructure. 53 test suites green, zero workspace warnings.

### Added
- **`duduclaw-llm` crate — unified provider layer**: one normalized
  `ChatRequest`/`ContentPart`/`StreamEvent`/`NormalizedUsage` shape over four
  native protocols — Anthropic Messages (layered `cache_control`, thinking
  replay), OpenAI **Responses API**, Gemini `generateContent`
  (`thoughtSignature` echoed verbatim), OpenAI-compat chat/completions
  (8 presets + local llamafile/vLLM/Ollama). Real SSE on all four. Ten-way
  `LlmError` classification, `ModelRegistry` with vendored 2026 prices
  (millicents/MTok, price cliffs, cache rates, `~/.duduclaw/models.toml`
  override), `FallbackRouter` with per-(provider,model) cooldowns and
  context-window-aware candidate filtering.
- **MCP client + agentic tool loop** (`duduclaw-llm`): stdio JSON-RPC MCP
  client + provider-agnostic `run_tool_loop`, so the direct-API and
  local-inference paths finally get the full MCP tool surface (previously
  CLI-backends-only). Local models (llamafile/Exo/vLLM) become first-class
  tooled backends via a gateway `LocalChatProvider` adapter;
  `inference_mode = "local"` is now honored on the channel-reply path
  (local-first with CLI fallback).
- **Cross-provider fallback + rotation on the hot path**: `agent.toml
  [model] fallbacks = ["openai/gpt-5.4", "compat:deepseek/…"]` failover
  chain; `AccountRotator` generalized with per-account `provider` —
  multi-account, budget and cooldown machinery now applies to
  openai/gemini/deepseek/… (env-var fallback when unconfigured); optional
  OS-keychain master-key storage (`keychain` feature).
- **Harness infrastructure**: OpenTelemetry GenAI tracing
  (`invoke_agent`/`chat`/`execute_tool` spans per `gen_ai.*` semconv,
  OTLP/gRPC export with auth headers, opt-in `otel` feature, default OFF);
  `duduclaw eval` behavioral regression suite (golden-task TOML, live +
  replay modes, deterministic tool/regex assertions + optional LLM judge,
  CI-gateable); universal HITL `ApprovalBroker` (SQLite, TTL expiry = deny,
  wired into autopilot `require_approval`); A2A v1.0 Agent Card
  (`/.well-known/agent-card.json`) with a real `message/send` that enqueues
  onto the dispatcher bus and honest `submitted`/`working`/`completed`
  states.
- **Memory/routing research upgrades** (2024–2026 literature pass):
  Ebbinghaus retrievability (`R = exp(-t/S)`) for retrieval ranking and
  decay; HippoRAG-lite Personalized PageRank over the v1.19 SPO triple
  graph (multi-hop recall, supersession-aware); ACE/ExpeL rule lifecycle
  (helpful/harmful counters, net-zero rules retired); calibrated cascade
  routing (post-hoc logprob acceptance, opt-in); summarized-failure retry
  (context decontamination on Timeout/EmptyResponse); layered Direct-API
  cache breakpoints (`CACHE_SPLIT_MARKER`) + per-block invalidation
  attribution.
- **Non-Claude runtime parity**: `RuntimeContext` carries
  `CapabilitiesConfig`; codex/gemini spawn with capability-derived sandbox
  flags (no more blanket `--full-auto`/`yolo`); PTY-pooled sessions inject
  per-agent `--allowedTools`/`--disallowedTools` (previously zero
  restrictions reached the pool); codex/gemini/agy auto-register the
  duduclaw MCP server in their native configs; `agent create --runtime`
  scaffolds AGENTS.md/GEMINI.md and rejects typo'd providers.
- **Wiki ↔ memory boundary**: conversation distillation now persists to
  temporal memory (supersession) instead of wiki pages; session-stable wiki
  injection (15-min pinned selection, prompt-cache friendly); wiki/memory
  injection dedup (wiki wins); `.scope.toml` `knowledge_owner =
  "wiki"|"memory"` deterministic conflict arbitration.

### Changed
- CostTelemetry prices per model via the registry — non-Anthropic usage was
  previously billed at hardcoded Claude Sonnet rates (DeepSeek overbilled
  ~30×); price-cliff warnings now model-aware.
- `try_direct_api` routes by registry-resolved provider (OpenAI → Responses,
  Gemini → native, compat presets); Anthropic path byte-identical.
- Dashboard model suggestions follow installed runtimes; provider↔model
  mismatch (`preferred = "gpt-5"` on the Claude path) warns once with
  guidance.

### Removed (deep prune, −19k lines)
- Orphaned crates never linked by any binary: `duduclaw-governance`,
  `duduclaw-durability`, `duduclaw-bus`, `duduclaw-bridge`.
- Unwired gateway modules (AFM compression, stale `tool_classifier`,
  `experiment/`, `sticker/`), the three-strategy inference compressor
  (Meta-Token/LLMLingua-2/StreamingLLM — duplicated the live
  `prompt_compression.rs` pipeline; `compress_text`/`decompress_text` MCP
  tools retired), and the unwired voice subsystem (asr/vad/deepgram/
  sensevoice/livekit; `whisper.rs` + `embedding.rs` kept) — drops
  symphonia/livekit/tokio-tungstenite deps.
- Both crate-wide `#![allow(dead_code)]` suppressions; everything the
  compiler then surfaced (−1.3k lines) plus the 8 remaining pre-existing
  warnings — the workspace now builds with **zero warnings** including
  `--all-targets`.

### Security
- Fixed: non-Claude runtimes ran with zero tool-capability enforcement
  (blanket permission-bypass flags + PTY pool passing no restrictions).
  Capability enforcement is now fail-closed across every runtime path, with
  structured warnings where a CLI offers no enforcement mechanism
  (antigravity).


## [1.32.0] - 2026-07-03 — Dashboard UX: command palette, self-explanatory nav, mobile shell

A deep UX pass on the Calm Glass dashboard, driven by a full-page UX audit. The
headline is a **command palette (⌘K / Ctrl+K)** — the Raycast-aligned answer to
moving through a 37-page console without scrolling-and-hunting — plus a nav that
explains itself and a shell that finally works on a phone. Frontend-only; no
backend RPC / WS protocol change.

### Added
- **Command palette (⌘K / Ctrl+K)** — `components/CommandPalette.tsx`, mounted
  once in `MainLayout`. Dependency-free fuzzy search (`lib/fuzzy.ts`, CJK-safe,
  Latin aliases derived from each `nav.*` id + the localized description) across
  every role/edition-gated nav route plus quick actions (switch theme, language,
  workspace⇄dashboard shell, logout). Empty query surfaces recently-visited
  routes (`stores/command-palette-store.ts`, persisted MRU). ARIA
  combobox+listbox, arrow/Enter/Esc keyboard nav, match highlighting. The Header
  gains a discoverable `Search… ⌘K` trigger (⌘ on macOS, Ctrl elsewhere).
- **Self-explanatory sidebar** — every nav item now shows a one-line description
  under its label (and as a searchable subtitle in the palette) so functions are
  clear without guessing from the icon. `NavItem.desc` added to `nav-model.ts`
  for all 27 items, localized in zh-TW / en / ja-JP.
- **Shared loading primitives** — `ui/Skeleton.tsx` (`Skeleton` / `SkeletonList`,
  `role="status" aria-busy`, reduced-motion safe), applied to the Dashboard task
  columns in place of an ad-hoc pulse; and a `Button` `pending` prop (swaps the
  leading icon for a spinner, disables, sets `aria-busy`) for async submits.

### Changed
- **Mobile shell** — below `md` the dashboard sidebar is now an off-canvas drawer
  (`stores/sidebar-store.ts`) toggled by a Header hamburger and dismissed on
  navigation or backdrop tap; at `md`+ it stays a static column (no change on
  desktop). Header padding tightens to `px-4` on small screens.
- **DESIGN.md** documents the command palette, mobile drawer, `Skeleton`, and
  `Button.pending` as first-class patterns of the Calm Glass system.

### Testing
- `tsc -b` clean, `vite build` green, **98 web unit tests pass** (+18 new: 13 for
  the fuzzy matcher, 5 for the palette store's MRU de-dup/cap/persist).



## [1.31.0] - 2026-06-30 — Workspace shell + desktop lifecycle hardening

Ships the **Genspark-style 工作空間 (Workspace) 外殼** — a consumer-grade landing
layer (central prompt bar + capability launcher grid + "Claw, your first AI
employee" entry) layered on top of the existing Calm Glass power-user dashboard,
with a simple ⇄ advanced mode toggle. The full power dashboard is untouched and
remains the default for enterprise / existing users. No backend RPC / WS protocol
change — the workspace is purely a new frontend assembly over the existing
`/ws/chat` + stores. See `docs/todo/TODO-genspark-workspace-shell.md`.

Also lands the **Tauri 2 desktop scaffold** (Phase D) — a native window that wraps
the `duduclaw` gateway as a sidecar — and this release hardens its lifecycle.

### Added
- **Workspace shell (web)**: `WorkspacePage` (Hero + PromptBar + LauncherGrid),
  reusable `chat/` components (`MessageBubble` / `TypingIndicator`) shared with
  WebChat, `AgentModelPicker` / `ConnectorChips`, the Claw value-prop section, a
  `ui-mode-store` (workspace/dashboard, persisted, personal-edition default), and
  a Header `ModeToggle`. Full zh-TW / en / ja-JP i18n, a11y, and unit tests.
- **Desktop mode override (§D1)**: `DUDUCLAW_DESKTOP_MODE=auto|attach|spawn`
  controls whether the desktop shell attaches to an externally-managed gateway
  (launchd / CLI), always spawns its own sidecar, or auto-decides (default).
  Replaces the unbuilt settings-panel toggle with a testable env override.
- **`scripts/desktop/gen-icons.sh`**: generates the app icon set via
  `cargo tauri icon`, with a `sips` / `iconutil` fallback on macOS (PNGs + .icns;
  warns that the Windows `.ico` still needs the Tauri CLI / ImageMagick).

### Fixed / Changed
- **config.toml port priority (§D2.2)**: the desktop sidecar now resolves the
  gateway port as `DUDUCLAW_PORT` env > `~/.duduclaw/config.toml [gateway] port` >
  default `18789`, respecting the operator's persisted choice when the env var is
  absent.
- **Double-instance avoidance (§D1/§D2.2)**: attach-detection now probes *every*
  known port (env / config.toml / default), so a non-default `config.toml` port
  can no longer make the desktop app miss — and double-spawn over — a gateway
  already running on the default port. The whole attach-vs-spawn decision matrix
  is unit-tested via an injectable liveness probe (`decide_plan`).


## [1.30.1] - 2026-06-30 — LINE replies actually send

### Fixed
- **LINE webhook reply delivery**: `line_webhook_handler` processed each event
  inline and only returned 200 after the model reply was generated and sent.
  LINE times out a slow webhook response and the `reply_token` is short-lived, so
  when LINE disconnected the handler future (and the in-flight reply) was
  cancelled — the bot was read but never replied ("已讀沒回應"). The reply now runs
  in a detached task and the webhook returns 200 immediately.

> Operational note: the interactive-REPL PTY path (`[runtime] pty_pool_enabled =
> true`) can hang on its boot/sentinel dance in a headless container; OAuth
> **setup-token** accounts are reliably served by the legacy `claude -p` path
> (`pty_pool_enabled = false`), which is the recommended setting for them.

## [1.30.0] - 2026-06-30 — One-click login → working agent, end to end

Makes the dashboard **Claude 一鍵登入** flow actually produce a usable account,
and fixes the LINE channel + per-account PTY auth so an agent can reply.

### Fixed
- **One-click login UX** (`cli_auth`): the OAuth authorize URL is surfaced as a
  clickable button (PTY widened to 600 cols so it stays on one line); the ANSI
  console is de-garbled; the pasted code is submitted by sending Enter as a
  **separate** keystroke after the paste (Ink swallowed a CR that arrived in the
  same write, so the code never submitted); success/failure are detected through
  the Ink TUI's escape-separated words.
- **OAuth token capture**: `claude setup-token` only prints its long-lived token
  once — it's now scraped and registered as an account. The ANSI parser was
  rewritten to a correct CSI/OSC state machine (CSI ends on a 0x40–0x7E byte, not
  "the first letter"); the old one dropped a character from the token
  (`sk-ant-oat01-…` → `sk-ant-at01-…`), producing a 401 on every reply.
- **PTY binary resolution**: the PTY/OAuth reply path now falls back to a PATH
  lookup (`which_claude`), not just the HOME candidate list — the Docker image
  installs the CLI in `/usr/bin`, which the curated list omitted ("binary not
  found").
- **PTY pool per-account auth (HIGH-2, was deferred)**: the in-process pool now
  injects the rotator-resolved per-account credential env
  (`CLAUDE_CODE_OAUTH_TOKEN` / `CLAUDE_CONFIG_DIR` / `ANTHROPIC_API_KEY`) at spawn
  time via an account-keyed side-channel. Previously the spawned CLI used
  whatever ambient OAuth lived in `~/.claude/`, so a registered account never
  authenticated.
- **LINE channel save**: choosing an agent no longer makes the save fail —
  LINE/WhatsApp/Feishu are single global webhook endpoints, so they persist to
  the global `[channels]` and bind the selected agent as `[general] default_agent`
  instead of erroring "Per-agent channels not supported for: line".
- **LINE webhook live**: `/webhook/line` is now **always mounted** and the handler
  reads the token/secret per request, so configuring LINE in the dashboard takes
  effect with no gateway restart (previously: 405 on Verify + status stuck on
  "連線中"). Status refreshes live on save.
- **Account list refresh**: a one-click login invalidates the rotator cache so the
  new account shows immediately instead of after the 5-minute TTL.
- **Self-service password change**: a new "帳號安全" Settings tab lets the
  single-owner edition rotate the dashboard admin password from the UI.

### Ops
- `commercial/gateway-vm`: `CLOUD_BUILD=1 ./deploy.sh` builds the image remotely
  (no local Docker); the VM deploy prunes old images afterwards to stop the 30GB
  boot disk filling (which crash-looped the gateway with SQLite "disk I/O error").
- Repo-root `.gcloudignore` keeps the Cloud Build context to source only.

## [1.29.1] - 2026-06-28 — Fix placeholder domain

Replaces the never-registered placeholder `duduclaw.tw` with the real deployed
domains.

### Fixed
- **`DEFAULT_CONTROL_URL`** (CLI + gateway) → `https://api.duduclaw.dudustudio.monster`.
  `duduclaw license refresh / redeem / rebind` and the gateway phone-home now
  reach the real control-plane by default (still overridable via
  `DUDUCLAW_CONTROL_URL`).
- Dashboard pricing links + upsell strings (premium templates / wizard /
  tier-limit message) → `https://duduclaw.dudustudio.monster#pricing`.
- Contacts: security SOP → `louis.li@dudustudio.monster`; support / refund →
  `info@dudustudio.monster`. Marketing drafts point at the real domain.

## [1.29.0] - 2026-06-27 — Cloud-tier agent/channel caps

Enforces the per-tier `max_agents` / `max_channels` from `features.toml` that
were declared but never actually applied — so the free/entry tiers (Hobby
1 agent/1 channel, Solo 1/2, Studio 3/5) now hold. **Self-host is never
capped** (Apache 2.0 promise); the limit only applies to managed Cloud tenants.

### Added
- **Cloud-tier resource caps** — `agents.create` and `channels.add` reject once
  the active tier's cap is reached, with an upgrade message. Gated on
  `DUDUCLAW_DEPLOYMENT=cloud` (set only inside managed tenant containers); a
  self-hosted deployment (the default) is never limited, and `max_* = 0` in
  `features.toml` also means unlimited.
- **Soft-limit banner** now shows concrete usage (`Agents X/Y · Channels A/B`)
  and an upgrade CTA when a personal cloud tenant reaches its plan limit —
  non-blocking, dismissible.
- `license_runtime::cap_exceeded()` pure helper + `is_self_host_deployment()`
  exposed for the gateway to query.

### Notes
- The free-tier limit text mirrors `features.toml`; the gate is enforced
  server-side regardless of the dashboard hint.

## [1.28.0] - 2026-06-27 — Partner (NFR) licenses + license self-service

Adds a free **Partner (NFR — Not For Resale)** license path and closes the
remaining license-acquisition gaps: emailed keys for every issuance, machine
re-binding, remote subscription status, and deployment-mode enforcement. The
managed/Cloud purchase flow was already end-to-end; this release makes the
self-host and partner paths first-class.

### Added
- **Partner (NFR) tier** (`LicenseTier::Partner`, `[partner]` in
  `features.toml`) — a free, self-host, non-resellable grant for integration /
  channel partners. Unlocks the same commercial modules as Self-Host Pro
  **except** white-label / redistribution. Independently revocable; never sold
  through checkout (price 0).
- **Partner code redemption** (free path) — `POST /v1/partner/redeem` exchanges
  a code + machine fingerprint for a signed partner license (atomic one-use
  reservation, `max_uses` enforced, best-effort email); `POST /v1/partner/codes`
  (admin) mints codes. CLI: `duduclaw license redeem <code>`.
- **CLI self-service** — `duduclaw license redeem` / `rebind` / `subscriptions`
  (redeem a free partner code, move a license to this machine, check remote
  renewal status).
- **License key email on every issuance** (Gap) — `POST /v1/license/issue` now
  emails the key when an `email` is supplied (previously only the PayUni
  webhook did), so admin / self-host issuance no longer needs a manual send.
- **Self-service machine rebind** (Gap) — `POST /v1/license/rebind` re-signs a
  license for a new fingerprint, ownership proven by the current fingerprint
  (atomic, no operator round-trip).
- **Remote subscription status** (Gap) — `POST /v1/license/status` (self, by
  fingerprint) + `GET /v1/license/subscriptions` (admin, by customer).
- **Deployment-mode binding** (M51) — the gateway now enforces tier ↔
  deployment via `DUDUCLAW_DEPLOYMENT` (`cloud` vs self-host, default
  self-host): cloud-only tiers are refused on self-host and vice-versa,
  fail-closed to OpenSource.

### Notes
- Self-host paid checkout (PersonalProSelfHost / SelfHostPro) and the PayUni
  **sandbox toggle** were already wired; only live sandbox e2e remains, gated
  on a PayUni merchant account.
- `keygen` learns `--tier partner` / `--tier personal_pro_self_host`.

## [1.27.0] - 2026-06-27 — Industry templates (Pro) + license-gated wizard unlock

Ships four research-backed **premium industry templates** for Taiwan SMB
verticals and wires the previously-missing **"unlock" path** so the
`premium_templates` license feature actually surfaces them in the setup wizard
— fail-closed, so the public OSS binary and unlicensed users never receive the
closed content.

### Added
- **Premium industry templates** (Pro / Studio / SelfHostPro /
  PersonalProSelfHost / OEM) — `ecommerce` / `clinic` / `realestate` /
  `education`, each a full kit (SOUL.md + compliance-hardened CONTRACT.toml +
  vertical-tuned agent.toml + FAQ.json + glossary / SOP / compliance wiki) with
  cited Taiwan statutes (消保法 / 醫療法 / 不動產經紀業管理條例 /
  補習及進修教育法). Closed-source; shipped only in licensed builds.
- **License-gated template unlock** (`duduclaw-cli` `premium_templates` module)
  — `premium_unlocked()` / `find_premium_templates_dir()` /
  `available_premium_industries()` / `resolve_premium_template()`. Fail-closed:
  a missing / expired license, the OpenSource tier, an absent template tree, or
  an unsafe slug all resolve to *locked*; the slug is validated against path
  traversal before any filesystem access.
- **Wizard premium industries** — `duduclaw wizard` appends unlocked premium
  verticals to the industry menu, and shows a one-line upsell hint
  (`🔒 … 需 Pro 授權`) when the templates are present on disk but the license is
  locked.

### Notes
- `priority_security_patch` remains a support-SLA value-add (tier display
  only), not a code gate, by design.

## [1.26.0] - 2026-06-27 — Personal / Enterprise editions + one-click CLI login

Introduces an explicit **product form-factor** dimension (Personal vs
Enterprise) that is orthogonal to the license tier and **never gates a core
feature** — it only changes defaults and which management surfaces the
dashboard shows. Adds a **Dashboard one-click login** for every AI CLI, bundles
the Antigravity CLI in the server image, and ships personal-edition data
portability.

### Added
- **Personal / Enterprise editions** (`EditionProfile` in `duduclaw-core`):
  - `Personal` (default) = single-owner, zero-config; `Enterprise` = multi-seat
    / compliance management surfaces. Resolution precedence:
    `DUDUCLAW_EDITION` env > `agent.toml [edition]` > license tier > `Personal`.
  - Gateway resolves it per request and returns `edition_profile` on
    `system.status` / `system.version`.
  - Dashboard reads it to hide enterprise nav (org / users / governance /
    partner / wiki-trust) on Personal, shows an **EditionBadge**, and a
    non-blocking **soft-limit banner** near a plan's agent/channel limit.
- **`PersonalProSelfHost` license tier** — the individual-developer self-host
  tier (NT$490/mo or NT$4,900/yr): unlocks premium templates + priority patches
  without the enterprise modules.
- **Dashboard one-click CLI login** (`auth.cli_login.*`): drives each CLI's
  native login (Claude / Codex / Gemini / Antigravity) in a PTY on the gateway,
  streams the output to a dashboard terminal, and relays the user's pasted code
  back. Flags `remote_safe` per CLI (paste-back vs localhost-callback). Reachable
  from the Accounts page. Claude `setup-token` flow verified end-to-end.
- **Antigravity CLI (`agy`) bundled** in `container/Dockerfile.server` alongside
  claude / codex / gemini (Google's official installer), so the Antigravity
  runtime works out of the box.
- **Personal-edition data portability**: `duduclaw export` / `duduclaw import`
  package `~/.duduclaw/` as a portable `.tar.gz` (agents / memory / config /
  license; skips models / logs / backups) to move between machines or switch
  self-host ↔ managed. Guide: `docs/guides/personal-edition-portability.md`.

### Changed
- `.dockerignore` added at the repo root (keeps the build context small).

### Tests
- `duduclaw-core` EditionProfile (7), `duduclaw-license` tier (83),
  `duduclaw-gateway` cli_auth (6) + full suite, `duduclaw-cli` portability (3);
  web `tsc` + `vitest` (44). Live `docker run` smoke confirms
  `edition_profile` resolves from `DUDUCLAW_EDITION` in a real container.


## [1.25.0] - 2026-06-26 — Browser-first onboarding + guided product tour

First-run setup moves out of the terminal and into the dashboard. A fresh
install now boots straight into a warm, friendly setup flow ("👋 開始建立第一個
Agent 吧") and, after the first agent is created, offers a skippable guided
tour of the key pages. The `duduclaw onboard` CLI wizard is kept but
soft-deprecated.

### Added
- **Dashboard first-run wizard** (`WelcomePage`, `/welcome`): a 3-step flow —
  welcome → choose AI backend → name the agent. The AI-backend step covers
  five paths, each mapped to the right config via existing RPCs:
  - **Claude subscription** (OAuth) — `inference_mode=hybrid`, shows detected
    login status.
  - **Claude API key** — `accounts.add` + `api_mode=direct`.
  - **Generic API (OpenAI-compatible)** — any OpenAI-compatible endpoint
    (OpenAI / vLLM / Ollama / llamafile / Exo …) via `runtime=openai_compat` +
    `inference.update`.
  - **Local model (offline)** — `inference_mode=local` + `[model.local]`.
  - **Other CLI** — Codex / Gemini / Antigravity runtime.
- **`FirstRunGate`**: installs with zero agents are routed to `/welcome`
  automatically (loop-safe; a new agents-store `loaded` flag prevents a
  redirect flash before the first agent list resolves).
- **Guided product tour** (`GuidedTour`, lightweight self-built spotlight, no
  new deps): offered after the first agent is created, walks the user through
  the important pages, skippable any time (Esc), shown once per user
  (localStorage). Replayable from Settings → General. Sidebar links carry
  `data-tour` anchors.
- **`runtime.detect` RPC**: reports which AI runtime CLIs are installed
  (claude / codex / gemini / antigravity) plus Claude OAuth status — presence
  only, no secrets — driving the backend picker's "detected / not installed"
  badges.
- **`duduclaw_core::write_minimal_config`**: writes a bootable minimal
  `config.toml` (`[general]` + `[gateway]`) atomically.
- Empty-state CTA on the Agents page ("create your first agent").
- `welcome.*` / `tour.*` i18n strings across zh-TW / en / ja-JP.

### Changed
- **Gateway boots without a config**: `duduclaw run` on a fresh install now
  auto-writes a minimal config and starts straight into the dashboard instead
  of hard-stopping with "run `duduclaw onboard` first". The CLI `onboard`
  wizard remains for headless/advanced use but prints a soft-deprecation hint.

### Fixed
- **`agents.create` now honors the `soul` parameter** (it was silently
  dropped) and writes the `[runtime]` section at create time, so the dashboard
  can set an agent's persona and backend in one call.



## [1.24.0] - 2026-06-25 — Antigravity CLI (`agy`) runtime; PtyPool unbound from Claude

Google retired the personal-tier Gemini CLI on 2026-06-18 in favour of the
**Antigravity CLI** (`agy`). This release adds `agy` as a first-class multi-runtime
backend and unbinds the PtyPool / cli-worker layer from a hardcoded Claude so all
CLI kinds have real call points.

### Added
- **Antigravity (`agy`) runtime** (`RuntimeType::Antigravity`, `runtime/antigravity.rs`).
  Driven via oneshot `agy -p --dangerously-skip-permissions --print-timeout 300s`
  (`--model` / `--add-dir` when set), verified end-to-end against the real binary.
  - Binary auto-resolve (PATH → `~/.local/bin/agy`); system prompt + conversation
    history embedded in the prompt argument (agy has no `--system` flag); CJK-safe
    truncation; token usage estimated via the shared heuristic (print mode exposes
    no stats). Auth via `ANTIGRAVITY_API_KEY`; MCP config at
    `~/.gemini/antigravity-cli/settings.json`.
  - Pre-seeds the agent dir into agy's `trustedWorkspaces` (under a cross-process
    lock) so the interactive trust prompt never hangs a headless subprocess.
  - Registry auto-detection, vision-capability gating, and `[runtime] provider`
    validation all recognise `antigravity` (alias `agy`).
- **Per-CLI binary discovery** in `duduclaw-core`: generic `which_cli` /
  `which_cli_in_home` plus `which_codex` / `which_gemini` / `which_agy`.
- Docs: `docs/todo/TODO-antigravity-cli-migration.md`, development-guide §1.4
  (Multi-Runtime), and `[runtime]` examples in all agent.toml templates.

### Changed
- **PtyPool / cli-worker unbound from Claude.** `CliKind::Antigravity` added;
  `resolve_program` and the worker's `spawn_session_default` now resolve all four
  CliKinds (Codex/Gemini/Antigravity no longer return `None`/reject). New
  `cli_kind_for_provider()` derives the PtyPool kind from the agent's
  `[runtime] provider`, replacing the two hardcoded `CliKind::Claude` acquire sites
  (`claude_runner`, `channel_reply`).
- The interactive PtyPool REPL remains Claude-only by design: non-Claude providers
  route through the oneshot `runtime_dispatch` path. Reconnaissance showed agy's
  full-screen alt-screen TUI plus the missing system-prompt flag make the sentinel
  protocol a poor and unnecessary fit (`agy -p` already works); decision recorded
  in the migration TODO.

### Notes
- The legacy `gemini` CLI backend is retained for paid `GEMINI_API_KEY` / enterprise
  users, whose access continues past the 2026-06-18 personal-tier shutdown.



## [1.23.0] - 2026-06-22 — Decision Continuity (RFC-24): durable cross-session decisions

### Added
- **Decision Continuity (RFC-24).** When an agent offers the user an enumerated
  choice ("方案 A/B/C", "Option 1/2"), each option is now persisted into the
  Temporal Memory **semantic** layer — independent of session turns and untouched
  by `compress()` — and still-open decisions are re-injected into the next turn's
  prompt. A later "用方案 C" (new turn / session / process) resolves from durable
  state instead of being guessed from unrelated history. Opt-in per agent via
  `agent.toml [memory] decision_continuity = true` (default off).
  - **Detection** is deterministic and zero-LLM on the main path (方案/選項/Option
    /bare letter·digit/emoji keycap markers, conservative homogeneity + keyword
    gates). A suspected-but-unparsable choice (e.g. 甲/乙/丙, ①②) triggers a
    single background Haiku second-pass; plain prose never does.
  - **Resolution**: `decision_resolve` / `decision_list` MCP tools, plus
    auto-resolution when the user references an open option. Resolving supersedes
    the decision's status, records the choice as a long-lived semantic fact, and
    expires the option artifacts (fail-closed on unknown id / key / owner).
  - **Anti-guessing**: referencing a decision with no durable record records an
    F2 Reflexion learning signal so the agent learns to acknowledge the gap and
    query rather than fabricate.
  - **Lifecycle & ops**: per-agent TTL (`[memory] decision_ttl_days`, default 7)
    self-prunes stale open decisions; Dashboard "待決事項" panel with
    `decisions.list` / `decisions.dismiss` RPC (dismiss marks a false positive);
    Prometheus `decision_captured/resolved/expired/false_positive` counters;
    `scripts/smoke-decision-continuity.sh`.
  - Design: `docs/rfc/RFC-24-decision-continuity.md`; tracking:
    `docs/todo/TODO-rfc24-decision-continuity.md`.



## [1.22.1] - 2026-06-21 — Core gateway drops the Python runtime dependency

### Changed
- **Skill vetting is now Rust-native.** The dashboard `skills.vet` path no longer
  shells out to `python3 -m duduclaw.evolution.run`; it uses
  `skill_lifecycle::security_scanner::scan_skill` — the same scanner already
  backing the MCP `skill_security_scan` tool and the sandbox-trial gate, so the
  dashboard, agents, and lifecycle pipeline share one verdict.
- **Channel delegate / fallback is now Rust-native.** The `channel_reply`
  3rd-tier fallback and `agents.delegate` (wait=true) call
  `direct_api::call_direct_api` (new `call_direct_api_delegate`) instead of the
  `duduclaw.sdk.chat` Python subprocess.
- **The core gateway/CLI installed via npm/Homebrew now has no Python runtime
  dependency.** `pip install duduclaw` is optional — the `duduclaw` PyPI package
  is a standalone importable library only. Advanced local inference
  (MLX / LLMLingua-2) still depends on the separate `mlx_lm` / `llmlingua` ML
  packages, not on `duduclaw`. Docs updated across README (zh/en/ja),
  ARCHITECTURE, overview, evolution-engine, docker, and feature docs.

### Removed
- Deleted `gateway/src/evolution.rs` (its sole content was the Python vet
  bridge), the dead `vet_skill_native` fallback (which used non-compliant
  unanchored `contains`), and the `call_python_sdk_v2` / `find_python_path`
  helpers in `channel_reply`.



## [1.22.0] - 2026-06-21 — RFC-26 Live Forking · skill-synthesis scheduler · Calm Glass dashboard

Inspired by [vstorm-co/pydantic-deepagents](https://github.com/vstorm-co/pydantic-deepagents),
this adds **Live Run Forking** — split an in-flight agent task into N competing
branches that explore different strategies in isolated copy-on-write workspaces,
then let an AI judge pick the winner. **Default off**; per-agent opt-in via
`agent.toml [fork] enabled = true`. See `docs/rfc/RFC-26-deep-agents-alignment.md` and
`docs/todo/TODO-rfc26-live-forking.md`.

### Added
- **New crate `duduclaw-fork`** — the forking engine: `Branch`/`BranchState`,
  copy-on-write `BranchOverlay` (reads fall through to parent, writes stay local,
  `promote()` merges the winner), per-branch + aggregate `budget::Pool`,
  `ForkController` over a decoupled `BranchExecutor` trait, a `JudgeAgent` with the
  deep-agents confidence formula (`quality·0.4 + test_pass·0.4 + consistency·0.2`),
  4 merge modes (`manual`/`auto`/`auto_with_fallback`/`vote`), and a `test_runner`
  that scores branches by their configured test command. (49 unit tests.)
- **6 MCP tools** gated by the new `Scope::ForkExecute` + the `[fork] enabled`
  toggle: `fork_run`, `inspect_branches`, `diff_branches`, `merge_or_select`,
  `terminate_branch`, `fork_cost` (`crates/duduclaw-cli/src/mcp_fork.rs`).
- **`RotatingBranchExecutor`** — runs each branch through the `AccountRotator`
  + a real `claude` spawner, enforcing per-branch and aggregate USD budgets; forks
  execute in a **background** task so the MCP stdio loop never blocks. Branch
  outcomes + spend recorded to `~/.duduclaw/fork_history.jsonl` (advisory-locked)
  with in-process `FORK_METRICS` counters (`crates/duduclaw-cli/src/mcp_fork_exec.rs`).
- **Checkpoint fork/rewind** (`duduclaw-durability`) — `fork(checkpoint_id, new_task)`
  copies state under a new lineage, `rewind(task, checkpoint_id)` restores an earlier
  snapshot, `Checkpoint.parent_checkpoint_id` tracks lineage; id-addressable archive.
- **Smoke harness** `scripts/smoke-fork.{sh,ps1}`.

### Added (round 2 — cross-process + parity follow-ups)
- **Shared SQLite fork store** (`duduclaw-fork::ForkStore`, WAL at `~/.duduclaw/fork_store.db`) — the
  cross-process source of truth. `mcp_fork`/`mcp_fork_exec` refactored onto it.
- **Gateway `/metrics`** emits `duduclaw_fork_*` lines (read from the store at scrape time).
- **Dashboard ForkPage** (`web/`) + `fork.list/inspect/resolve` WebSocket RPC — list forks, compare
  branches side by side, see the judge's winner, resolve manually. New `/forks` route + nav.
- **`memory_improve`** MCP tool — clusters memories by tag into a propose-not-apply reflection scaffold.
- **`plan_start`** MCP tool (Plan Mode) — clarify-first planning scaffold, `agent.toml [planner]` toggle.
- **Built-in skills** — `code-review`/`refactor`/`test-writer`/`git-workflow` seeded idempotently into
  every new agent's `SKILLS/` at creation.
- **Checkpoint durability** — `CheckpointManager::with_persistence` SQLite backend; fork/rewind/lineage
  survive restart. **Task Board** — `claim_task` (atomic CAS) + parent-cycle detection.
- **Fork executor hardening** — branches capped to distinct available accounts (logged); pre-spawn
  cancellation registry for `terminate_branch`.

### Added (round 3 — the last deferred items)
- **Native copy-on-write overlay** — `BranchOverlay` clones the parent workspace via `clonefile(2)`
  (`cp -c`, macOS/APFS) or `cp --reflink` (Linux btrfs/XFS); `detect_backend()` probes once and falls
  back to the snapshot copy if unavailable.
- **Streaming budget enforcement + external SIGKILL** — `ClaudeCliSpawner` streams stream-json, charges
  `total_cost_usd` incrementally, and kills the child mid-stream on per-branch overspend
  (`SpawnOutcome::BudgetExceeded`); a per-branch kill-switch registry lets `terminate_branch` SIGKILL an
  in-flight subprocess (`→ Terminated`).
- **Activity-Feed mirroring** — fork resolutions write a `fork_resolved` row into the gateway's
  cross-process `activity` table (`<home>/tasks.db`), surfacing on the dashboard Activity Feed.

### Added (round 4 — cross-branch aggregate pre-emption)
- **`duduclaw_fork::LiveAggregate`** — a streaming-time companion to `budget::Pool`, shared across a
  fork's concurrent branches. It tracks every in-flight branch's live `total_cost_usd`; the moment their
  combined spend crosses the aggregate cap it names the **most-expensive in-flight branch** (deterministic
  tie-break) so it can be pre-emptively killed — instead of waiting for each branch to hit its own
  per-branch cap. (5 unit tests.)
- **Spawner wiring** (`mcp_fork_exec.rs`) — each stream-json cost update runs the pure
  `stream_budget_decision` (per-branch cap → aggregate `observe`): the priciest over-budget branch
  self-kills if it is the observer, otherwise the observer `request_budget_kill`s the sibling. The
  aggregate kill is tagged so the woken branch maps to `BudgetExceeded` (→ `BudgetKilled`), distinct
  from an operator `terminate_branch` (`Cancelled` → `Terminated`); `LiveAggregate::finish` frees a
  branch's budget for survivors once it ends. (5 unit tests.) Completes RFC-26 §4.2.

### Added (skill synthesis — W19-P1)
- **Periodic auto-run scheduler** (`skill_synthesis_pipeline::scheduler`) — runs the
  rollout-to-skill pipeline on a fixed interval instead of waiting for a manual
  `skill_synthesis_run` MCP call. **Off by default**, **dry-run by default even when
  enabled**, hot-reloaded config (`config.toml [skill_synthesis] auto_run/dry_run/
  interval_hours/lookback_days/target_agent`), and non-blocking (pipeline errors are
  captured into the run summary, never abort the loop).
- **Dashboard config RPCs** — admin-gated `skill_synthesis.get` / `skill_synthesis.update`
  (validated writes onto `config.toml [skill_synthesis]`); `skill_synthesis_threshold`
  is a `u32` count (no longer a float — fixes registry scan rejecting `0.7`).
- **`fetch_episodic_evidence`** with path-traversal rejection + tests.

### Added (dashboard — Calm Glass redesign)
- **Calm Glass design system** — shared component library (`web/src/components/ui/`:
  Page / PageHeader / Section / Card / StatCard / Button / Badge / Field / Tabs /
  EmptyState) + a 6-group sidebar nav model (`layout/nav-model.ts`), applied across
  every dashboard page. Design spec in `web/DESIGN.md`; design tokens in `index.css`.
- i18n keys synchronized across `en` / `ja-JP` / `zh-TW`.

### Documentation
- **Feature docs reorg + trilingual coverage** — 10 new feature deep-dives (20–29:
  memory-intelligence, governance-layer, durability-framework, autopilot-engine,
  task-board, identity-resolution, mcp-http-sse, pty-pool-runtime, live-forking,
  evolution-events) in `en` / `ja-JP` / `zh-TW`; back-translated 16–19 to `ja-JP` /
  `zh-TW`; `feature-inventory` refreshed `v1.8.14 → v1.22.0`; README indexes updated.

### Housekeeping
- Removed residual local artifacts (test `.profraw`/coverage, stale 5.6 GB git
  worktrees) — gitignored cruft only, no repo content affected.


## [1.21.1] - 2026-06-18 — Channel routing: stop bot "identity mixing"

### Fixed
- **Agent-bound bot token now takes precedence over the global poller**
  (Telegram / Slack / Discord). These channels' long-poll (`getUpdates`) and
  gateway sessions are exclusive per bot token. When the same token was
  configured both globally (`config.toml`) and on a specific agent, the dedup
  kept the generic **global** poller and skipped the agent one — so two pollers
  fought over the same token (Telegram **409 Conflict**, dropped messages) and
  the surviving generic poller routed via `default_agent`, causing **identity
  mixing** (e.g. a CEO bot sometimes answered as COO). Precedence is reversed:
  agent tokens are collected first and the global poller is skipped (with a
  `WARN` naming the owner) for any token an agent already binds. Extracted the
  shared `find_global_token_owner` helper with unit tests.
- **`default_agent` validation at startup** — a dangling `default_agent`
  (pointing at a renamed/removed agent) silently fell back to an arbitrary main
  agent at routing time, the other root cause of identity mixing. The gateway
  now validates `default_agent` against the loaded registry at boot and `WARN`s
  loudly (listing available agents), and the per-turn fallback path warns too.



## [1.21.0] - 2026-06-17 — RFC-25 §5 Followups: non-Claude path fully functional

RFC-25 v1.20.0 compiled the multi-runtime abstraction but left the non-Claude
(Codex / Gemini / OpenAI-compat) path as a thin opt-in with documented gaps.
v1.21.0 closes all 11 of those gaps so non-Claude agents are first-class, and
hardens the release tooling so PyPI can no longer be silently skipped.

### Added
- **Multi-turn context for non-Claude runtimes** (A1): `conversation_history`
  threaded through the choke-point and consumed by Codex / Gemini / OpenAI-compat
  (OpenAI-compat uses native multi-turn `messages`); duplicate `ConversationTurn`
  consolidated onto `runtime::ConversationTurn`.
- **Non-Claude cost telemetry** (A3): `run_agent_prompt` records token usage to
  `CostTelemetry` (detached, classified by `request_type`) so Codex/Gemini/OpenAI
  usage is visible to cost summaries, the 200K price-cliff warning, and adaptive routing.
- **Non-Claude channel keepalive** (A4): periodic `ProgressEvent::Keepalive` during
  long non-Claude replies so channels don't look stalled / hit idle timeouts.
- **`scripts/release.sh` multi-platform sync**: `audit` (per-platform version + drift),
  synchronized bump across every manifest, a post-bump assertion that aborts if any
  platform is left behind, and `verify <version>` that queries PyPI + npm.

### Changed
- **Pending-tasks for non-Claude delegation** (A2): the Task-Board queue is inlined
  into the non-Claude sub-agent system prompt.
- **Routing decision centralized** (B1): `RuntimeSettings::non_claude_provider()`;
  "Claude into the registry" is an explicit non-goal (orphan `runtime/claude.rs`).
- **Single `agent.toml` parse per reply** (B2): `RuntimeSettings` + `load_runtime_settings`
  threaded via `AgentPrompt.runtime_settings` (3 → 1 reads/reply).
- **Per-(home,provider) failover health** (R1): choke-point routes through
  `FailoverManager` (3 failures → 60s cooldown → fallback), keyed per home to avoid
  cross-tenant bleed.
- **Per-home `RuntimeRegistry` cache** (R2): replaces the first-home-binding `OnceCell`.
- **A2A target resolution** (R3): `resolve_target_agent` (default → Main agent, else
  validated) + per-home `AgentRegistry` cache with agents-dir mtime invalidation.
- **Utility provider routing** (N2): summarizer / wiki-ingest / reflection / synthesis
  honour the agent's (or global `config.toml [runtime] utility_provider`) runtime.
- `DEFAULT_UTILITY_MODEL` is a single source in `duduclaw-core` (B3).

### Fixed
- **PyPI release miss**: `pyproject.toml` had drifted to 1.18.0 (while Cargo/READMEs
  were at 1.20.0), so the CI `pypi-publish` job built a stale wheel that
  `skip-existing` silently dropped. `release.sh` now syncs every manifest and
  asserts they all reach the new version; this release also heals the drift
  (pyproject + npm manifests → 1.21.0).
- `record_usage` no longer blocks the reply on a synchronous SQLite write, and skips
  empty-`agent_id` (agent-less utility) attribution.



## [1.20.0] - 2026-06-16 — RFC-25 Multi-Runtime Unlock + A2A

The "Multi-Runtime four-backend" abstraction was previously **orphan, uncompiled
source** — every execution path hardcoded Claude. RFC-25 compiles and wires it,
and routes the LLM-calling subsystems through a single provider-agnostic
choke-point. Existing agents are unaffected (provider defaults to Claude); agents
can now opt into Codex / Gemini / OpenAI-compat via `agent.toml [runtime] provider`.

### Added

- **`RuntimeType` (duduclaw-core)** — `{Claude, Codex, Gemini, OpenAiCompat}`,
  unblocking the never-compiled `runtime/` abstraction (`AgentRuntime` trait,
  `RuntimeRegistry`, four runtime impls) + `failover.rs` — now compiled for the
  first time.
- **`runtime_dispatch::run_agent_prompt` choke-point** — resolves the agent's
  `[runtime] provider` → selects from a lazily-built, auto-detecting
  `RuntimeRegistry` → executes, falling back to the configured fallback then Claude.
- **`runtime_config`** — reads `[runtime] provider`/`fallback` and `[model] utility`;
  `ModelConfig.utility` (default `claude-haiku-4-5`) centralizes the previously
  scattered hardcoded utility-model literals.
- **A2A real execution** — the ACP stdio server's `tasks/send` now runs the target
  agent (via the provider-aware gateway dispatch) instead of a placeholder; the
  responding agent's `[runtime] provider` is honoured.

### Changed

- **GVU evolution allowlist relaxed** — the hard `ALLOWED_EVOLUTION_MODELS` reject
  (which forced `claude-haiku-4-5` and blocked everything else) is now a warning.
- **Channel reply** routes non-Claude providers through the choke-point; Claude
  keeps the optimized OAuth-rotation/PTY path (zero regression).
- **GVU loop + sub-agent delegation** (`call_claude_for_agent_with_type`,
  plain + worktree paths) route through the choke-point, honouring per-agent provider.

### Fixed

- `failover::is_non_retryable` now matches free-form "content policy" (a never-run
  test in the previously-uncompiled module).

### Notes

- `a2a/1` HTTP capability stays gated (separate transport, not wired to A2A
  execution). Sandbox (container) delegation and home-only utility tasks remain on
  Claude — documented follow-ups in `commercial/docs/RFC-25-multi-runtime-unlock.md`.


## [1.19.0] - 2026-06-16 — Memory Intelligence: Temporal Memory + Reflexion Loop + Batch Fetch

Three W18/W19-designed memory features, implemented non-invasively on the live
Rust `SqliteMemoryEngine` (the original PostgreSQL/Python designs were updated to
the actual SQLite architecture). No new infrastructure.

### Added

- **F1 Temporal Memory** (`duduclaw-memory`) — `memories` gains temporal /
  knowledge-graph columns (`valid_from`, `valid_until`, `superseded_by`,
  `supersedes`, `subject`, `predicate`, `object`, `confidence`, `metadata`) via
  the existing idempotent migration loop, plus two indexes. New
  `store_temporal(entry, TemporalMeta)` performs automatic conflict resolution:
  writing the same `(agent, subject, predicate)` supersedes the prior fact
  (closes its `valid_until`, links the supersession chain). `search()` /
  `search_layer()` now return only currently-valid memories by default;
  `get_history()` / `get_at()` expose the chain and point-in-time lookups.
  `MemoryEntry` is **unchanged** (zero blast radius across 22+ construction sites).
- **F2a Reflexion recall** (`duduclaw-gateway/channel_reply`) — an agent's recent
  unresolved `MistakeNotebook` entries are now injected into the **answering
  prompt** (`## Past Mistakes to Avoid`), not just the GVU SOUL.md generator —
  delivering cross-task learning. CJK-safe (topic match with recency fallback).
- **F2b Reflexion consolidation** (`duduclaw-gateway/reflexion`) — when the same
  `MistakeCategory` accumulates ≥3 unresolved mistakes, they are distilled into a
  single **semantic** memory rule (via F1 supersession) and the source mistakes
  are marked resolved. Runs detached so it never delays replies. Deterministic
  synthesis (zero LLM cost).
- **F3 `memory_fetch_batch`** — new MCP tool + `SqliteMemoryEngine::get_by_ids`
  fetch up to 100 entries by ID in one call (`Scope::MemoryRead`, namespace /
  ownership enforced, partial hits return `missing_ids` without error).

### Notes

- Trigger signal for reflexion is the existing `ErrorCategory` (Significant /
  Critical, MetaCognition-adaptive) — **not** the GVU Verifier (which validates
  SOUL.md proposals, not task quality). No hard-coded score threshold.
- The standalone Python MCP memory track remains superseded by the Rust CLI
  endpoints (not revived).


## [1.18.0] - 2026-06-15 — Dashboard budget/usage accuracy + reliability fixes

Dashboard now reads real spend from the persistent `CostTelemetry` ledger
instead of the `AccountRotator`'s in-memory counter (which reset on every
5-minute rebuild / gateway restart and stayed 0 for OAuth-subscription
accounts), plus a sweep of dashboard runtime-bug fixes.

### Added

- **`marketplace.install`** handler — installs a catalog MCP server into a
  chosen agent's `.mcp.json` (was previously an error stub). Frontend gains
  a target-agent picker dialog.
- **`system.version` returns `edition`** so the dashboard can gate Pro-only
  UI (auto-update toggle).
- **Voice & Proactive settings persistence** — `system.update_config` writes
  `[voice]` to `inference.toml`; per-agent `[proactive]` saved via
  `agents.update` and surfaced through `agents.inspect`.
- Language switcher + theme store wired into the dashboard Header.
- 88 missing i18n keys across `zh-TW` / `en` / `ja-JP`.

### Fixed

- **Budget/usage shows real data**: `agents.list` / `agents.inspect` report
  per-agent month-to-date spend (was one all-account aggregate shown
  identically for every agent); `accounts.budget_summary` reports the real
  global month-to-date total.
- **Cost unit correction**: `cost_millicents` holds whole cents — removed the
  erroneous `/10` in analytics savings/cost which under-reported by 10x.
- **Schedule (cron) add**: sends required `name` + `task` (was failing).
- **MCP servers** consumed as the array shape the backend serializes.
- Division-by-zero guard in budget progress bars; silent account errors now
  surface as toasts; assorted async-state and timer-cleanup fixes.

## [1.17.0] - 2026-06-10 — RFC-24 License v2.0 (Open Core foundation)

Bootstrap of the DuDuClaw Open Core commercial layer. Apache 2.0 remains
**fully usable with no limits and no commercial modules** — paid
subscription tiers unlock value-add modules under `commercial/*`
(premium templates, evolution params, enterprise dashboard, priority
security patches). No license installed → OpenSource tier (current
behavior, zero regression).

### Added

- **`duduclaw-license` crate** — verification-only client (signing key
  stays in `commercial/duduclaw-license`).
  - 7 tiers with inheritance chain: `OpenSource` / `Hobby` / `Solo` /
    `Studio` / `Business` / `SelfHostPro` / `Oem`.
  - Ed25519 trust registry seeded from `DUDUCLAW_LICENSE_PUBKEY_<ID>`
    env vars; empty registry collapses to OpenSource (fail-safe).
  - CRL polling for emergency revocations.
  - Machine binding via hostname + MAC fingerprint.
  - `~/.duduclaw/license.json` (`0o600`) storage.
  - `features.toml` v2 subscription matrix: Solo NT$990 / Studio
    NT$2,990 / Business NT$8,900 / Self-Host Pro NT$1,490·mo or
    NT$14,900·yr / OEM (Year 2+).
- **Gateway `license_runtime`** — bootstraps on `start_gateway`,
  phone-home loop, CRL poll, process-global `LicenseRuntime`. Every
  failure mode (missing file / empty key registry / signature mismatch
  / expired / grace exceeded) collapses to OpenSource — gateway never
  crashes on license errors.
- **`license.status` RPC** (manager-only) — returns `LicenseSnapshot`
  that intentionally **omits the raw Ed25519 signature and customer
  email** so it is safe to render in the dashboard.
- **`duduclaw license` CLI** — subcommands: `activate` / `status` /
  `refresh` / `export` / `import` / `deactivate` / `fingerprint`.
- **Web Dashboard `LicensePage`** — tier status, expiry warnings,
  unlocked modules grid, CTAs (renew / pricing / docs). i18n: 39 keys
  per locale (zh-TW / en / ja-JP).
- **README Hire me block** — Fiverr / LinkedIn / Portfolio links above
  the badges (GitHub long-URL SEO).
- **`scripts/dev-replace-binary.sh`** — local build → npm-installed
  binary replacement loop (rebuild dashboard + cargo `--release` +
  backup + install + smoke). Does NOT auto-restart side-effectful
  processes.
- **`.cargo/config.toml`** — pin `PYO3_PYTHON=python3.13` (BLK-7
  workaround; PyO3 0.24 max supported Python is 3.13, system default
  is 3.14).
- **Operational docs** — `marketing/blog/`, `marketing/press-kit/`,
  `wiki/eval/`, `wiki/impl/`, `wiki/reports/`, `wiki/sprint-reports/`,
  `docs/wiki/reports/`, `reports/tl-daily/`.

### Backward compatibility

- No license installed → OpenSource tier (current behavior, zero
  regression).
- All existing CLI / RPC / config surface unchanged.



## [1.16.0] - 2026-06-01 — MCP Refresh Tokens + GVU Consolidate Op

Two operationally-driven additions surfaced by the 12-day post-v1.15.2
production soak.

**Symptom 1**: agnes' Claude Desktop MCP server stopped working on
2026-05-30 15:59 UTC with `API key expired (31 days old, max 30)` and
sat broken for 2 days before being investigated. The 30-day rotation
policy was sound, but the user-experience around it was not — Claude
Desktop quietly disconnected and never retried, and there was no CLI
to rotate cleanly.

**Symptom 2**: agnes' SOUL.md has been growing at ~5 lines/week under
healthy GVU operation and is now at 132 lines / 7909 bytes — within the
150-line / 8 KB cap, but ~3 weeks from cap-rejection on current
trajectory. The structured patch path can only grow SOUL.md; it has
no shrink primitive.

### Added — MCP refresh tokens (Phase A)

- **New module `duduclaw-cli::mcp_refresh`** — SQLite-backed refresh
  tokens that supersede the 30-day legacy API keys with 90-day
  lifetime, per-token revocation, and a hash-only store (the raw
  token is never persisted).
  - Format: `ddc_refresh_<env>_<64hex>` (twice the entropy of legacy).
  - Storage: `~/.duduclaw/mcp_tokens.db` table `refresh_tokens` with
    columns `jti, token_hash, client_id, scopes, is_external,
    issued_at, expires_at, revoked_at`.
  - Validation: `authenticate_with_refresh_token` mirrors
    `mcp_auth::authenticate_with_key`'s error model so the existing
    dispatcher just needs prefix-based routing.

- **`mcp_auth::authenticate_from_env`** now prefix-routes credentials:
  values starting with `ddc_refresh_` go through the refresh-token
  validator, everything else through the legacy `ddc_<env>_<32hex>`
  validator. Both paths return the same `Principal` type so downstream
  code is unchanged. Legacy keys keep working — no migration required
  before refresh tokens are adopted.

- **New CLI subcommand `duduclaw mcp …`**:
  - `issue-refresh-token --env <prod|staging|dev> --client-id <id>
    --scopes <csv> [--external]` — generates a fresh token, persists
    its hash, prints the raw token ONCE.
  - `revoke-token <jti>` — soft-deletes a refresh token by its
    16-hex jti prefix.
  - `list-tokens` — table view of all tokens with status / remaining
    TTL / scopes.

### Added — GVU `SoulPatchOp::Consolidate` (Phase B)

- **New variant** in `crate::gvu::proposal::SoulPatchOp`.
  Semantically equivalent to `Replace` but with a hard size-shrink
  invariant: the patch is rejected if
  `content.trim().len() >= existing_body.trim().len()`. Used when
  SOUL.md is approaching the line/byte caps and the LLM is asked to
  merge redundant bullets or tighten verbose phrasing without
  changing behavior.

- **`apply_patch_to_soul` enforces the shrink invariant** before
  swapping. A `Consolidate` whose content does not shrink the section
  is rejected with `"Consolidate must shrink the section — new
  content is N bytes but existing body is M bytes"`. Empty existing
  body is also rejected (cannot consolidate nothing).

- **Generator prompt updated** with the `consolidate` op semantics so
  the LLM can self-trigger it when it sees SOUL.md approaching cap.
  Prompt change is additive — existing flows that don't need to
  consolidate are unaffected.

### Fixed

- **`mcp_auth` test suite was a time-bomb**. Six test fixtures used
  the hardcoded `created_at = "2026-04-29T00:00:00Z"`. On 2026-06-01
  (33 days later) every test expecting `Ok(Principal)` started
  failing with `KeyExpired { days_old: 33 }`. Replaced with
  `Utc::now().to_rfc3339()` so the suite stays robust to time. Five
  similar fixtures in `mcp_auth_strategy` already had this issue
  (5/30 onward) and were similarly fixed.

### Test coverage

12 new unit tests, total workspace **1537 passing**:

- `mcp_refresh::tests` ×8 — roundtrip, format rejection, unknown,
  revoke, expired, list ordering, jti determinism, env-label
  rejection at issue.
- `soul_patch_tests::consolidate_*` ×4 — shrinks existing,
  rejects-when-grows, rejects unknown section, rejects empty body.

### Operator action items

**Migrating Claude Desktop to a refresh token**:

```
duduclaw mcp issue-refresh-token \
    --env dev \
    --client-id claude-desktop \
    --scopes memory:read,memory:write,wiki:read,wiki:write,messaging:send
```

Paste the printed token into `~/Library/Application Support/Claude/claude_desktop_config.json`
under `mcpServers.duduclaw.env.DUDUCLAW_MCP_API_KEY`, then Quit and
relaunch Claude Desktop. After verifying the new token works, revoke
the old legacy key by removing it from `~/.duduclaw/config.toml` (or
running `duduclaw mcp revoke-token <jti>` if it was already a
refresh token).

**Legacy keys remain supported indefinitely** — refresh tokens are
strictly opt-in. Operators who prefer the file-based registry can
continue using `[mcp_keys]` entries.


## [1.15.2] - 2026-05-20 — agent_update_soul Audit + Drift Detection

Follow-up to v1.15.1. Investigating an unexpected 11-line SOUL.md growth
during agnes' 24h observation period revealed three pre-existing security
gaps in the `agent_update_soul` MCP backdoor that long predated v1.15.1 but
became visible thanks to the structured-patch path making routine GVU
writes traceable by contrast.

### Fixed

- **`agent_update_soul` now refreshes the `soul_guard` integrity hash**
  via `accept_soul_change` on every successful write. Before this fix the
  stored fingerprint was never updated, so legitimate calls left permanent
  drift that `check_soul_integrity` would (eventually, if a human invoked
  it) flag as tampering. agnes' 2026-05-19 02:27Z self-modification was
  the canonical observation.

- **`agent_update_soul` now writes to `tool_calls.jsonl`** for every
  invocation — success path with hash prefix + size, and four distinct
  rejection paths (invalid agent_id / empty content / nonexistent agent /
  tmp-write / rename failures). The trusted MCP backdoor was previously
  invisible to post-hoc audit; `tool_calls.jsonl` had no `agent_update_soul`
  entries between 2026-04-22 and today despite the tool being exercised at
  least once on 2026-05-19.

- **Heartbeat now runs `soul_guard::check_soul_integrity` per agent per
  tick** via the new `check_soul_integrity_with_audit` helper.
  Out-of-band SOUL.md modifications (whether legitimate-but-unaudited or
  malicious) now produce a `WARN` log and a `_soul_integrity_drift`
  synthetic audit row within one heartbeat interval (default 1 h). Prior
  to this fix the integrity check had exactly one caller — the manual
  `duduclaw test <agent>` CLI red-team — so drift sat silently until an
  operator chose to investigate. Agents without a `SOUL.md` are silently
  skipped (stub-agent configuration is documented and supported).

### Why this is separate from v1.15.1

These three gaps existed before v1.15.1 — the bloat-fix surfaced them but
did not introduce them. They are filed as a separate patch release
because their root cause (the `agent_update_soul` MCP tool bypassing the
GVU safety stack) is structurally orthogonal to the GVU verifier path and
deserves its own audit narrative.

### Test coverage

9 new tests, total workspace 1525 unit tests passing:

- `mcp::wiki_namespace_tests::agent_update_soul_refreshes_soul_guard_hash`
- `mcp::wiki_namespace_tests::agent_update_soul_appends_audit_row`
- `mcp::wiki_namespace_tests::agent_update_soul_audits_validation_rejections`
- `heartbeat::tests::soul_integrity_check_skips_agent_without_soul`
- `heartbeat::tests::soul_integrity_check_clean_when_hash_matches`
- `heartbeat::tests::soul_integrity_check_emits_audit_on_drift`

### Operator action items

After upgrading to 1.15.2 you may see `_soul_integrity_drift` audit rows
in `tool_calls.jsonl` for agents whose SOUL.md was last modified by the
pre-1.15.2 `agent_update_soul` (which never updated the hash). The drift
is real — the stored hash genuinely doesn't match the file — but it's a
historical artefact, not active tampering.

To clear the false-positive baseline, delete the stored hash so the next
integrity check re-fingerprints the current file as the new baseline:

```
rm ~/.duduclaw/soul_hashes/<agent>.hash
```

`check_soul_integrity` treats a missing hash as "first run" and stores
the current SHA-256 automatically. Subsequent out-of-band modifications
will then flag genuine drift.


## [1.15.1] - 2026-05-18 — SOUL.md Bloat Containment + Structured Patch Path

Customer-reported regression: a production COO bot (`agnes`) had its
`SOUL.md` balloon from 61 to 592 lines over 5 GVU cycles, with 88% of the
file being accumulated proposal-meta narrative (`## 診斷` / `## rationale` /
`## expected_improvement` / `## wiki_proposals`). Each subsequent cycle saw
the bloated file, generated another correction, and the updater appended it
verbatim — an infinite feedback loop that bypassed every safety check by
progressively expanding the baseline so ASI's content-weighted threshold
stayed permanently satisfied.

This release fixes the failure mode in three layers and ships an unrelated
MCP server stdout-pollution bug fix.

### Fixed

- **`Updater::apply` no longer appends LLM proposal-meta narrative.**
  - New `strip_proposal_meta()` drops `## 診斷` / `## Analysis` /
    `## rationale` / `## expected_improvement` / `## wiki_proposals` /
    `## proposed_changes` headers and their bodies before the legacy append.
  - New `SOUL_MAX_LINES = 150` and `SOUL_MAX_BYTES = 8 KB` hard caps reject
    any proposal that would push SOUL.md beyond either limit, independent of
    ASI (which becomes permissive on a growing baseline because
    `for_baseline_size` weakens the threshold proportionally).
  - The legacy "always append, never replace" safety justification is
    preserved — the strip+cap layer makes append bounded instead of unbounded.

- **L1 verifier now simulates the real apply path.** When
  `proposal.patch.is_some()`, `verify_deterministic` calls
  `apply_patch_to_soul` to compute the true post-apply SOUL.md and runs
  must_always / must_not / size / sensitive-pattern checks against it,
  instead of the legacy `current + content` fake append. Without this fix
  the structured-patch path was DOA: `proposal.content` becomes a human
  summary like "Add refusal rule" and the must_always pattern (a chunk of
  contract text) was never found inside it — observed on agnes 2026-05-18
  where 3 generations rejected for the same phantom must_always failure
  despite the LLM's `soul_patch` JSON containing exactly the required text.

- **`Generator::parse_response` extracts JSON from markdown code fences.**
  LLMs commonly wrap structured output in ` ```json ... ``` ` fences with
  surrounding narrative (e.g. "根據分析... ```json\n{...}\n``` ...核心邏輯...").
  The parser previously failed pure-JSON parsing on the fence, fell back to
  section extraction, dropped the `soul_patch` field, and silently
  downgraded to the legacy strip+cap append. Reuses `verifier::strip_json_fences`
  for consistency with `parse_judge_response`.

- **MCP server stdout pollution.** `tracing_subscriber::fmt::layer()` now
  routes to stderr instead of stdout. Claude Desktop's MCP stdio transport
  parses stdout as JSON-RPC 2.0; the previous default tracing destination
  corrupted every session with `Unexpected token '', "[2m2026-0..." is not
  valid JSON` errors. The downstream `cmd_mcp_server` re-init via
  `try_init` silently no-opped once the global subscriber was already
  installed. Independent of GVU — affects every MCP client.

### Added

- **`SoulPatch { section, op, content }` structured edit type** with four
  ops — `Replace`, `AppendWithin`, `PrependWithin`, `AddSection`. Located in
  `crates/duduclaw-gateway/src/gvu/proposal.rs`. Optional `patch:
  Option<SoulPatch>` field on `EvolutionProposal` so on-disk proposals
  deserialize unchanged.

- **`apply_patch_to_soul(current, patch) -> Result<String, String>`** in
  `gvu/updater.rs` — locates the target `## <title>` header, edits the
  section body in place per the op, reassembles SOUL.md. Section names
  containing newlines or `##` tokens are rejected (prompt-injection
  defence); patch.content is capped at 4 KB per edit.

- **Generator prompt asks LLM to emit a `soul_patch` field.** Schema with
  op semantics, hard rules forbidding `[保留現有內容]` placeholders and
  whole-file rewrites, plus a concrete example. Prompt length grew from
  ~22 KB to ~24.5 KB (≈11% per GVU run).

### Production validation

Run on `agnes` 2026-05-18 09:53Z → 10:42Z, four iterations against the same
Round 1-4 conversation script (boundary probe → off-scope medical question
→ negative feedback). Final state: SOUL.md grew 61 → 85 lines with one
cleanly-added new section, zero meta-narrative residue, zero duplicate
headers, zero embedded JSON. GVU `outcome=applied`, generation 1, 60.2s
duration, ASI=0.679 (warning, not critical), L1+L3 verifier approved.

### Test coverage

106 GVU unit tests, +22 new:
- `proposal_meta_stripper_tests` — Chinese + English meta sections, blank-line
  collapsing, case-insensitive headers, cap sanity.
- `updater_apply_caps_tests` — meta-only rejection, line-cap overshoot
  rejection, clean-proposal application.
- `soul_patch_tests` — all four ops, section-not-found, prompt-injection
  guards, oversized content rejection, replace-then-replace idempotence.
- `soul_patch_apply_e2e_tests` — end-to-end through `Updater::apply` with
  `proposal.patch = Some(...)`.
- `generator_tests` — fence-stripped JSON parse, missing-patch fallback,
  prompt-includes-schema regression guard.
- Patch-aware L1 verifier — append-patch satisfies must_always check,
  rejection when pattern truly missing, invalid-section gives clear error,
  must_not check uses patch content not human summary.

### Backward compatibility

- `proposal.patch` is `Option`, `#[serde(default, skip_serializing_if =
  "Option::is_none")]` — proposals serialized before this release
  deserialize unchanged.
- `Updater::apply` falls back to strip+cap legacy append when `patch` is
  `None`. LLMs that haven't adopted the new schema continue to work, just
  with the bounded-growth safety net instead of unbounded append.
- No `CONTRACT.toml` or `agent.toml` schema changes.
- No migration required. Existing SOUL.md files already polluted by prior
  bloat are not auto-cleaned — operators should hand-truncate or wait for
  ObservationFinalizer's rollback path to fire on metric regression.


## [1.15.0] - 2026-05-17 — Cross-Platform PTY Pool + Worker

Anthropic blocked `claude -p` for OAuth-subscription accounts in mid-2026 and
recommended driving the real interactive `claude` REPL instead. v1.15.0 ships
the runtime to do exactly that: long-lived `claude` sessions driven through a
real PTY (ConPTY on Win 10 1809+, openpty on Unix) with a sentinel-framed
in-band response protocol — no scrollback scraping, no sidecar.

Default **OFF**; per-agent opt-in via `agent.toml [runtime] pty_pool_enabled =
true`. The existing fresh-spawn `claude -p` path is preserved for API-key
accounts and remains the global default. Every PTY path falls back to legacy
`tokio::process::Command + claude -p` on error, so a missing worker /
unhealthy pool / spawn failure is recoverable, not fatal.

### Added

- **New crate `duduclaw-cli-runtime`** — cross-platform PTY runtime built on
  [`portable-pty`](https://crates.io/crates/portable-pty).
  - `PtySession` lifecycle + `SpawnOpts` per-CLI configuration
    (`CliKind::Claude / Codex / Gemini`).
  - `PtyPool` with per-agent semaphore, idle eviction, supervisor + restart
    policy. Cache-hit / spawn / 3 eviction-reason counters surface to gateway.
  - `Envelope` / `Frame` / sentinel constants + ANSI-stripping
    `extract_payload_with_chrome_filter`.
  - `oneshot_pty_invoke` for the `claude -p` PTY-wrapped fallback (API-key
    accounts).
- **New crate `duduclaw-cli-worker`** — standalone worker binary wrapping the
  pool over a localhost HTTP+JSON-RPC API.
  - Bearer-token auth via `DUDUCLAW_WORKER_TOKEN` env var + on-disk
    `TokenStore` (`ring::SecureRandom`).
  - Endpoints: `POST /rpc` (`invoke` / `shutdown_session` / `stats`),
    `GET /healthz` (no auth).
  - Library re-export so the gateway shares the protocol types directly.
- **Gateway integration**:
  - `crates/duduclaw-gateway/src/pty_runtime.rs` — adapter owning the global
    `PtyPool`, `RuntimeMode::{FreshSpawn, PtyPool}` per-agent routing,
    `acquire_and_invoke` + `acquire_and_invoke_with` public surface,
    optional `MANAGED_WORKER` `WorkerClient` for Phase 7.
  - `crates/duduclaw-gateway/src/worker_supervisor.rs` — Phase 7 supervisor
    for the out-of-process worker. Resolves binary, spawns with loopback
    bind + token + `--home-dir`, polls `/healthz` until ready, runs a 30s
    health-check loop with N-strike restart, and **sequences
    SIGTERM/SIGKILL into the gateway graceful-shutdown future** (after
    prediction-engine flush, before axum drains) instead of racing it from
    a detached task (Round 2 review HIGH-4).
  - `crates/duduclaw-gateway/src/runtime_status.rs` — Phase 8.5 JSON status
    endpoint `GET /api/runtime/status` (loopback-only, no auth) with
    transport + kill-switch + session / invoke / worker stats.
  - `crates/duduclaw-gateway/src/channel_reply.rs` —
    `call_claude_cli_pty_rotated` PTY-routed mirror of
    `call_claude_cli_rotated`; OAuth → interactive REPL, API-key →
    `oneshot_pty_invoke + claude -p`. `parse_claude_stream_json_complete`
    is a buffer-based mirror of the streaming parser;
    `StreamDiagnostics` is embedded in error messages so
    `channel_failures.jsonl` post-mortem identifies what went wrong inside
    the PTY response (`exit / lines / events / assistant / text_blocks /
    thinking / tool_use / result_subtype / stop_reason / last_line /
    stderr_tail`).
  - `crates/duduclaw-gateway/src/claude_runner.rs` — dispatcher-side
    short-circuit: when `pty_pool_enabled = true`, sub-agent invocations
    skip local-offload + hybrid routing and go straight through the pool
    (channel reply + dispatcher consistent).
- **Phase 8 production observability** (`crates/duduclaw-gateway/src/metrics.rs`):
  - `pty_pool_acquires_total` + `pty_pool_acquires_cache_hit_total` +
    `pty_pool_acquires_spawn_total`.
  - `pty_pool_evicted_idle_total` + `pty_pool_evicted_unhealthy_total` +
    `pty_pool_evicted_shutdown_total`.
  - `pty_pool_invokes_ok_total` + `pty_pool_invokes_empty_total` +
    `pty_pool_invokes_error_total` + `pty_pool_invokes_timeout_total`.
  - `pty_pool_invoke_duration_buckets[8]` (shared bounds with the main
    request histogram) + `pty_pool_invoke_duration_sum_ms`.
  - `worker_health_misses_total` + `worker_restarts_total`.
  - `pty_pool_managed_worker_active` gauge (0 = in-process, 1 = managed).
- **Smoke harness**:
  - `scripts/smoke-pty-pool.sh` (Unix/macOS) — build cli-runtime + spike
    example + run cli-runtime / gateway `pty_runtime::` /
    `channel_reply::routing_helper_tests` / `stream_json_parser_tests`.
    `CLAUDE_SPIKE=1` runs the live interactive spike (consumes OAuth quota).
  - `scripts/smoke-pty-pool.ps1` — Windows equivalent.

### Operational notes

- **Kill switches**:
  - Per-agent: `agent.toml [runtime] pty_pool_enabled = false` (default).
  - Global: env-var kill switch disables PTY routing without rolling back.
- **Out-of-process mode** (`[runtime] worker_managed = true` in
  `<home>/config.toml`) promotes the in-process pool to the
  `duduclaw-cli-worker` subprocess. The supervisor is best-effort: spawn
  failure leaves the gateway in in-process mode with a warn log.
- **Cross-platform**:
  - Windows: `windows` crate Job Objects for child-process containment
    (Win10 1809+).
  - Unix: `nix` for signal + process-group control.
- **References**: [`dorkitude/maude`](https://github.com/dorkitude/maude)
  (Unix-only tmux shim that inspired the interactive driving idea) and
  [`runtorque/torque`](https://github.com/runtorque/torque) (Unix-only
  PTY + UDS frame protocol). `portable-pty` is what makes one code path
  span mac/Linux/Windows.

### Design docs

- `commercial/docs/runtime-pty-pool-design.md` — full architecture, kill
  switches, security stance.
- `commercial/docs/TODO-cli-pty-pool-worker.md` — phase-by-phase TODO with
  verify steps.


## [1.14.0] - 2026-05-14 — RFC-23 Redaction Pipeline

新增獨立 crate `duduclaw-redaction` 與 gateway 整合層，預設**未啟用**。

### Added

- **New crate `duduclaw-redaction`** — source-aware redaction +
  reversible restoration. Internal data (Odoo / shared wiki / file tools)
  is replaced with `<REDACT:CATEGORY:hash8>` tokens before the LLM sees
  it; tokens are restored at trusted boundaries (user channel reply,
  whitelisted tool egress).
- **Encrypted SQLite vault** at `~/.duduclaw/redaction/vault.db` using
  AES-256-GCM (reused from `duduclaw-security`), with per-agent 32-byte
  keys (`0o600` permission), TTL 7d default, two-stage GC (mark expired
  → purge after 30d).
- **Five built-in profiles** embedded in the binary: `general`,
  `taiwan_strict`, `taiwan_minimal`, `financial`, `developer`. Selected
  via `[redaction] profiles = [...]`.
- **Five-layer enable/disable resolver** (`compute_effective_enabled`):
  channel `force_on` (banked) → env + CLI flag emergency override → env
  alone → CLI flag → agent.toml → config.toml. Full truth-table coverage.
- **Channel `force_on` lock** with audited `--force-disable-redaction`
  emergency break-glass; persistent override-flag file
  (`~/.duduclaw/redaction/override.flag`) and CRITICAL audit per affected
  channel.
- **Tool egress whitelist** with default deny. Whitelisted tools can
  `restore_args = true` (real values), `passthrough` (keep tokens), or
  `deny`. Hallucinated tokens always result in deny.
- **JSONL audit sink** at `~/.duduclaw/redaction/audit.jsonl` with 10MB
  rotation; events: `redact / restore_ok / restore_denied / restore_miss
  / egress_allow / egress_deny / vault_gc / force_on_override`.
- **Background GC tokio task** running `mark_expired` every 6h and
  `purge_expired` every 24h, with graceful cancel.
- **Dashboard read-only RPCs**: `redaction.stats`,
  `redaction.recent_audit`, `redaction.override_status`,
  `redaction.policy_status`.
- **Gateway integration shim** at `crates/duduclaw-gateway/src/redaction_integration.rs`
  providing `build_manager_from_home()`,
  `compute_effective_for_channel()`, `cli_flag_from_env()`, and
  `force_disable_active()`.
- **Full gateway wiring**:
  - `MethodHandler` carries `Option<Arc<RedactionManager>>` + setter +
    4 `redaction.*` Dashboard RPC handlers (`stats`, `recent_audit`,
    `override_status`, `policy_status`).
  - `start_gateway()` parses `[redaction]` from `config.toml`, builds the
    manager, spawns the 6h-mark/24h-purge GC task, and injects the
    manager into `MethodHandler` and `ReplyContext`.
  - `build_reply_with_session` / `build_reply_for_agent` apply
    `restore` at the public-API exit so the user channel sees real
    values while LLM-bound text retains tokens.
- **MCP-layer integration** (`crates/duduclaw-cli/src/mcp_redaction.rs`):
  - `McpRedactionLayer` reads `DUDUCLAW_AGENT_ID` + `DUDUCLAW_SESSION_ID`
    env vars (set by gateway when spawning the Claude CLI subprocess).
  - On every `tools/call`: pre-check tool args for `<REDACT:...>` tokens
    and run the egress evaluator (whitelisted → restore; otherwise →
    JSON-RPC error). Post-process the tool result Value by walking every
    string leaf through `RedactionPipeline.redact` so the LLM never sees
    raw internal data.
- **CLI flags**: global `--redact=on/off` (overrides agent/global config
  but not channel `force_on`) and `--force-disable-redaction` (requires
  `DUDUCLAW_REDACTION=off`, writes a persistent override flag + CRITICAL
  audit + dashboard red banner).
- **RFC-23** at `commercial/docs/RFC-23-redaction-pipeline.md` + detailed
  per-phase TODO at `commercial/docs/TODO-redaction-pipeline.md` +
  operator guide at `commercial/docs/redaction-operator-guide.md`.

### Tests

- 98 unit tests + 11 end-to-end integration tests in
  `crates/duduclaw-redaction/`, covering: token format & HMAC salt
  derivation; rule compile + ReDoS-surface limits; vault round trip
  (encrypt blob never contains plaintext); cross-session and cross-agent
  isolation; per-rule cross-session-stable override; TTL → expired
  marker → 30-day purge; reveal counter bookkeeping; egress decisions
  (allow/passthrough/deny + nested JSON + hallucinated tokens); profile
  merge with id collision; five-layer toggle truth table with channel
  force_on priority; force-override flag persistence + banner; GC task
  mark+stop cycle.

### Default behaviour

`config.toml [redaction] enabled = false` — existing deployments are
unaffected unless operators explicitly opt in. See
[`commercial/docs/redaction-operator-guide.md`](commercial/docs/redaction-operator-guide.md)
for the five-step adoption recipe.


## [1.13.2] - 2026-05-12

Bug fix for fresh-install clients that have never run the CLI keyfile
init flow.

### Fixed

- **Dashboard credential save no longer fails with "Encryption
  unavailable" on a fresh install.** `encrypt_value()` now calls a new
  `load_or_create_keyfile()` helper that auto-generates the 32-byte
  AES-256 keyfile (`~/.duduclaw/.keyfile`, owner-only permissions) the
  first time the gateway is asked to encrypt a credential. Previously
  the helper was read-only and any client that hit the dashboard
  without first running `duduclaw init` would see the Odoo / channel
  token / API key save fail with a misleading "Ensure keyfile exists"
  message. The decrypt path stays read-only by design so a missing
  keyfile never silently destroys an existing ciphertext.
  (`crates/duduclaw-gateway/src/config_crypto.rs`)
- **Better error messages on the rare encryption failures that remain.**
  The Odoo configure handler now distinguishes the new failure modes
  (RNG / disk write) from the old "keyfile missing" case and points
  operators at the gateway log instead of telling them to fix a file
  the gateway is now able to create itself.
  (`crates/duduclaw-gateway/src/handlers.rs`)

### Tests

- 7 new unit tests covering: keyfile auto-creation, encrypt→decrypt
  round trip after auto-create, rejection of empty plaintext (does not
  pollute the home dir), keyfile stability across successive encrypts,
  decrypt-side read-only invariant, and `mkdir -p` of a fully absent
  home directory.


## [1.13.1] - 2026-05-12

Dashboard UX fix for the Odoo connection page.

### Changed

- **`odoo.test` RPC now accepts inline params** — when the dashboard
  sends `{ url, db, protocol, auth_method, username, api_key?, password? }`,
  the connector is built from those values without writing to
  `config.toml`, so users can verify credentials before persisting. When
  the credential field is empty in inline mode, the handler falls back
  to the stored encrypted secret so a small URL tweak does not require
  retyping the API key. Calling `odoo.test` with no params preserves the
  original "test the saved config" behaviour.
  (`crates/duduclaw-gateway/src/handlers.rs`)
- The Test Connection button on the Odoo page now uses the form's live
  values instead of requiring a prior save. The button is gated on
  url + db being present.
  (`web/src/pages/OdooPage.tsx`)
- `handleSave` / `handleTest` surface the real backend error string
  instead of swallowing it — the previous generic "save failed" /
  "Odoo not configured" messages were undiagnosable from the UI alone.

### Security

- Inline-mode params go through the same SSRF / HTTPS / db-name
  validators as `odoo.configure`. The test path cannot be used to
  bypass safety rules.
- New `scrub_odoo_error()` caps connector failure text at 240 chars
  before forwarding to the dashboard so HTML error pages or full URLs
  with query strings are not leaked.

### Tests

- 16 new unit tests covering happy path, every validation branch, the
  `fc00.*` hostname regression (not an IPv6 ULA), credential fallback,
  and the error-scrubber.


## [1.13.0] - 2026-05-12

Runtime-health overhaul covering 16 issues across two rounds. Round 1
restores GVU/SOUL self-evolution (was effectively dead since 5/3); Round 2
introduces architectural fixes for the cron-driven 200 K token cliff.

See `commercial/docs/TODO-runtime-health-fixes-202605.md` for the
issue-by-issue audit log with verification evidence.

### Added

- **`[prompt] mode = "minimal"` agent config** — opt-in Anthropic
  Skills-style system prompt: SOUL core (≤ 5 KB) + identity + contract +
  MCP tool index. Wiki / skill content fetched on demand instead of
  inlined upfront. Stable prefix → near-perfect prompt-cache hit.
  Expected cliff reduction: 75% on knowledge-rich agents.
  (`crates/duduclaw-gateway/src/prompt_minimal.rs`)
- **`[budget] max_input_tokens` enforcement** — when set, an agent's
  request goes through a compression pipeline (Hermes trim → drop oldest
  tool echoes → bisect-and-summarize) before send. `cost_pressure` flag
  from §6.3 tightens thresholds automatically. Non-fatal: falls back to
  full history on pipeline failure.
  (`crates/duduclaw-gateway/src/prompt_compression.rs`)
- **`[prompt] cli_bare_mode = true` agent config** — when set, the agent's
  Claude CLI subprocesses launch with `--bare`, suppressing the
  CLAUDE.md auto-discovery leak documented in the spike (see
  TODO #15). Requires an API-key account in the rotator; OAuth accounts
  are skipped with a warn.
  (`crates/duduclaw-gateway/src/claude_runner.rs` `BARE_MODE` task-local)
- **Async session summarizer** — background task (10-min cadence) folds
  older session turns into Haiku-generated bullet summaries. Stored in
  three new columns on `sessions` (`summary_of_prior`,
  `summarized_through_turn`, `last_summarized_at`). `channel_reply`
  prepends the summary as a synthetic assistant recap turn.
  (`crates/duduclaw-gateway/src/session_summarizer*.rs`)
- **TF-IDF wiki relevance ranking** — wiki injection now ranks L0/L1
  pages by user-message relevance (char-bigram TF-IDF, CJK-safe) before
  hitting the 6 KB cap. Auto-enabled, no config required; empty query
  preserves file order for back-compat.
  (`crates/duduclaw-gateway/src/relevance_ranker.rs`,
   `crates/duduclaw-gateway/src/ranked_wiki_injection.rs`)
- **`duduclaw lifecycle flush` CLI** — quarterly cold/hot separation of
  wiki pages. Uses file mtime as access proxy (real counter deferred).
  `--dry-run` by default; pass `--apply` to commit moves to
  `wiki/.archive/`.
  (`crates/duduclaw-gateway/src/lifecycle_flush.rs`)
- **GVU trigger module** — sub-agent dispatches now fire GVU via the
  same path as channel-facing root agents. Previously only `agnes` ever
  evolved; now `duduclaw-tl` etc. can too.
  (`crates/duduclaw-gateway/src/gvu/trigger.rs`)
- **`prompt_audit` observability** — per-section byte-count breakdown
  emitted as `INFO target=prompt_section_audit` when total exceeds 50 KB.
  Surfaces *which* section bloated, not just that total was high.

### Fixed

- **`log_level` config now resolves correctly** — three-tier
  `RUST_LOG → config.toml [general] log_level → "warn"` instead of the
  previous hard-coded `"warn"` fallback. Restores visibility of
  `Heartbeat firing`, `forced_reflection`, `SilenceBreaker consumer
  started`, and other INFO-level diagnostics that were silently dropped.
- **L1 generator `must_always` injection** — Generator now receives the
  contract's `must_always` patterns and emits a `<must_include>` block
  flagging any pattern absent from current SOUL. Unblocks the
  5/3-onwards deferred loop on agnes where every generation failed the
  same L1 check.
- **L1 `must_not` catch-22** — now checks `proposal.content` instead of
  `simulated_final`. Previously, agents that mirrored a `must_not` rule
  into SOUL.md as a self-reminder would have every subsequent proposal
  rejected because the rule statement was in `current_soul`.
- **Discord token-check backoff** — exponential 60 → 120 → 240 → 480 → 900
  seconds (capped 15 min) instead of flat 60 s; respects `Retry-After`
  header. Adds 24 h sliding-window storm detector that emits a
  `discord_invalid_session_storm` security audit event after 5 events.
- **GVU `Skipped` log level** — `debug!` → `info!` so trigger-fired-then-silent
  scenarios (e.g. agent in observation window) are debuggable without
  enabling debug logging.
- **`ObservationFinalizer` 72 h no-traffic cap** — sub-agents without
  channel traffic no longer sit in `observing` forever. After 72 h with
  conversations < 5, auto-confirm so the next GVU can proceed.
- **`skill_loader` recursive scan** — supports the official Anthropic
  Skills `<skill>/SKILL.md` layout (case-insensitive) alongside the
  legacy flat `<name>.md` form. Nested `references/*.md` correctly
  treated as supporting material, not separate skills. Symlink
  containment, hidden-entry skip, 8-level depth cap.
- **`skill_synthesis` pipeline tools** — added regression-guard tests
  ensuring all four pipeline tools (`memory_episodic_pressure`,
  `skill_synthesis_status`, `skill_synthesis_run`, `activity_post`) are
  visible to internal principals. Root cause of the 5/7 incident was a
  stale gateway binary, not missing implementation.

### Stats

- 1264 → 1390 tests green (+126 new unit tests)
- 9 new modules in `duduclaw-gateway`
- 31 files changed, +5790 / −164



## [1.12.3] - 2026-05-08

Hot-fix on top of v1.12.2 — Dashboard 編輯 agent 時 evolution 與 sticker
欄位顯示為預設值而非 agent.toml 真實值。

### Fixed

- **`agents.list` response 漏 `evolution` / `sticker` 區段**
  - Symptom: 在 Dashboard 把 agent 的 `skill_auto_activate` 從 false 改 true
    並儲存，response 回 `success: true` / `hot_reloaded: true`，`agent.toml`
    也確實寫入 `skill_auto_activate = true`；但重新打開 agent 編輯框仍
    顯示 false
  - Root cause: `handle_agents_list_filtered` 回傳 JSON 沒有 `evolution`
    與 `sticker` 兩個區段（只有 `agents.inspect` 有）。前端
    `EditAgentDialog` 從 list response 初始化表單，
    `agent.evolution?.skill_auto_activate ?? false` 因 `agent.evolution`
    為 `undefined` 永遠 fallback 到 `false`
  - 其他 3 個 evolution 欄位（`gvu_enabled` / `cognitive_memory` /
    `skill_security_scan`）剛好預設 `?? true` 對齊大多 agent.toml 真實值，
    使用者沒察覺；只有 `skill_auto_activate` 預設 `?? false` 與真實值衝突，
    才把這個顯示 bug 暴露出來。Sticker 區段也有同樣問題
  - Fix: 把 `evolution` + `sticker` 區段補進 `agents.list` response，與
    `agents.inspect` 對齊



## [1.12.2] - 2026-05-07

Dashboard 死局與假性「設定無反應」修復。使用者回報 Dashboard 設定幾乎無法
操作、任務無法操作；Telegram 與 Odoo 路徑正常。深入追查後發現 4 個獨立
問題交互疊加，本版一次解決。

### Fixed

- **JWT auto-refresh 缺失導致 WebSocket 死循環**（CRITICAL）
  - Symptom: gateway log 連續 4000+ 次 `WebSocket auth failed – closing connection`，
    最後一次成功認證 2026-05-06T02:17:52，之後 dashboard 全面失效
  - Root cause: access token TTL 30 分鐘，前端只在 `loadFromStorage` 啟動時
    呼叫一次 `/api/refresh`，過期後 WS 持續用過期 token 重連被拒
  - Fix: `auth-store` 加 25 分鐘 setInterval + `visibilitychange` listener；
    `ws-client` 加 `authRefreshHook`，handshake 失敗訊息含 `jwt`/`auth` 時
    下次 `doConnect` 前先 await refresh

- **重整頁面看不到資料、需切走再切回**（HIGH）
  - Symptom: 頁面 reload 後資料空白；切換頁面再切回才正常
  - Root cause: React effects 由葉子向根 commit，page useEffect 比 App
    `connectWithAuth` 早跑；`waitForReady` 在 state=disconnected & 無
    reconnectTimer 時 fast-reject `"Not connected"`
  - Fix: `AuthGuard` 多 gate 一層 `wsState === 'authenticated'`，protected
    route 在 WS 就緒後才 mount

- **agents.update 寫入後 registry 沒立刻 reload**（MEDIUM）
  - Symptom: 修改 agent 設定後使用者誤以為沒生效
  - Root cause: `update_agent_toml` 拿 registry write lock 用 500ms timeout
    但 timeout 後 silent fail，agent.toml 已寫入但記憶體 registry 沒重載
  - Fix: 改回傳 `Result<bool, String>`（bool = hot_reloaded），timeout / scan
    失敗時 `warn!` 一行；`agents.update` response 加 `"hot_reloaded": bool`
    與對應 message

- **per-agent channel token 變更不會 hot-restart bot**（MEDIUM）
  - Symptom: 修改 Discord/Telegram per-agent token 後，下次發訊息仍走舊
    token，需重啟 gateway
  - Root cause: bot 啟動時 capture token，registry rescan 不會觸發 bot 重啟；
    只有 `channels.add` / `channels.remove` RPC 走 hot-restart 路徑
  - Fix: 新增 `hot_restart_agent_channels(channel_types, agent_name)` helper；
    `handle_agents_update` 偵測到 `discord_bot_token` / `telegram_bot_token`
    入參時，寫檔成功後自動 hot-restart 對應 bot；response 加
    `"channels_restarted": [...]`。LINE 是 webhook 不需處理；Slack / WhatsApp
    / Feishu 仍需 gateway 重啟（缺 hot-restart helper）

### Notes

- 升版後第一次開啟 dashboard 仍需清除瀏覽器 localStorage 的
  `duduclaw-refresh-token` 重新登入，才能拿到走新 auth flow 的 fresh JWT。
- Telegram / Odoo / channel_reply 路徑本來就 OK，不受本版影響。



## [1.12.0] - 2026-05-06

W22 Sprint deliverables — two W22-P0 ADRs ship together with a multi-agent
coordination overhaul (RFC-22) driven by a 2026-05-04 → 2026-05-06 端到端
incident that exposed agnes silently fabricating sub-agent replies, autopilot
mass-firing on malformed events, and channel-path token usage going entirely
unrecorded.

### Added

#### W22-P0 ADR-002 — `x-duduclaw` capability negotiation

Every HTTP response from the MCP HTTP server now carries machine-readable
capability metadata, and clients can declare capability requirements that
trigger an early 422 rather than silent partial failures.

- **`mcp_headers.rs`** — `CAPABILITY_REGISTRY` static table (9 capabilities:
  `memory/3`, `mcp/2`, `audit/2`, `governance/1`, `skill/1`, `wiki/1` enabled;
  `a2a/1`, `secret-manager/1`, `signed-card/1` disabled/pending).
  `API_VERSION = "1.2"`. Builder/parser/negotiation functions. 23+ unit tests.
- **`mcp_capability.rs`** — `inject_capability_headers` outer middleware
  (appends `x-duduclaw-version` + `x-duduclaw-capabilities` to every
  response) and `negotiate_capabilities` inner middleware (returns 422
  Unprocessable Entity when client requirements unmet, with structured
  JSON body + `x-duduclaw-missing-capabilities` header). Permissive when
  header absent/empty/malformed. 11 Axum integration tests.
- **`mcp_http_server.rs`** — Both layers wired into `build_router()` with
  correct outer/inner ordering. Adds 11 integration tests for healthz,
  unauthorized 401, malformed JSON-RPC, and capability negotiation 422.
- **`docs/ADR-002-x-duduclaw-capability-negotiation.md`** — Full ADR.

#### W22-P0 ADR-004 — Secret Manager

Unified abstraction over three backends behind a `secret://<backend>/<name>`
URI scheme so MCP clients (Brave Search, Figma, Notion) can reference
credentials without embedding them in code or env vars.

- **`crates/duduclaw-security/src/secret_manager/`** — new module:
  - `mod.rs` — `SecretAdapter` async trait, `SecretUri` parser, config
    loader (`[secret_manager]` in `config.toml`), `Backend::Local|Vault|Env`.
  - `local.rs` — In-process AES-256-GCM encrypted store (dev/testing).
  - `vault.rs` — HashiCorp Vault KV v2 HTTP client (production), reads
    `vault_addr`, `vault_token`/`vault_token_enc`, `vault_mount`.
  - `env.rs` — Reads from process environment (CI/override).
- 26 unit tests covering URI parsing, config parsing, encrypted at-rest
  verification, error variants, cross-backend round-trips.

#### RFC-22 — Multi-agent coordination principles

- **`docs/RFC-22-multi-agent-coordination-principles.md`** — Four design
  decisions: (1-C) Two-tier Task/Wiki, (2-C) Hybrid spawn+bus fallback,
  (3-D) Channel mapping, (4-D) Hallucination forbidden + audit trail.
- **`crates/duduclaw-core/src/types.rs`** — `ChannelBinding { kind, id,
  description }` + `DiscordChannelConfig.bindings: Vec<ChannelBinding>` so
  per-thread routing can target sub-agents directly.
- **`crates/duduclaw-agent/src/resolver.rs`** — `AgentResolver` step-2
  channel/thread binding match between trigger word and coarse permission
  grant. 8 new unit tests.
- **`crates/duduclaw-security/src/audit.rs`** — `append_tool_call_with_extras`
  helper for attaching wiki authorship audit fields
  (`claimed_authors_in_content`, `matches_caller`, `actual_caller`).
- **`crates/duduclaw-cli/src/mcp.rs`** — `detect_claimed_authors_in_wiki`
  parses `## <agent> 的觀點`, `**回覆人**：<agent>`, signature, and
  frontmatter `claimed_authors:` patterns. Recorded on every
  `shared_wiki_write`. 6 new unit tests.

### Changed

- `x-duduclaw-version` bumped to `1.2` (second backward-compatible HTTP API change).
- **`crates/duduclaw-gateway/src/autopilot_engine.rs`** — `lookup_path_opt`
  returns `Option<Value>` so missing fields no longer match `eq null`,
  fixing the 5/5 mass-fire bug where 5 task_created events all triggered
  Rule A. `apply_op` short-circuits `None` to `false`. 4 regression tests
  (P1-9b).
- **`crates/duduclaw-gateway/src/channel_reply.rs`** — `build_system_prompt`
  now injects `CONTRACT.toml` boundaries via `contract_to_prompt`
  (P1-8 / P1-9a). `spawn_claude_cli_with_env` parses the result event's
  `usage` field and records via `cost_telemetry` against a
  `CHANNEL_REPLY_AGENT_ID` task_local set in `build_reply_with_session_inner`
  — channel replies now produce token usage rows (P1-7).
- **`crates/duduclaw-gateway/src/claude_runner.rs`** — adds
  `CHANNEL_REPLY_AGENT_ID` task_local for per-agent cost attribution.
- **`crates/duduclaw-cli/src/mcp.rs`** — MCP server boot log now logs
  `caller_agent` alongside `client_id` so observers can distinguish API
  key owner from actual sub-agent (P1-10). `handle_spawn_agent` surfaces
  underlying I/O error when `bus_queue.jsonl` write fails, with RFC-22
  reminder not to fabricate a reply (W1).
- **`crates/duduclaw-cli/Cargo.toml`** — `default = ["dashboard"]` so
  `cargo build -p duduclaw-cli --release` produces a binary whose
  dashboard SPA fallback is mounted (without this every HTTP path except
  `/health` and `/ws` returned 404).

### Tests

  duduclaw-gateway: 838 passed (incl. 4 new autopilot regression tests)
  duduclaw-agent:    39 passed (incl. 8 new resolver binding tests)
  duduclaw-cli:     365 passed (incl. 6 new wiki author + 13 HTTP transport)
  duduclaw-core:     80 passed
  duduclaw-security: 179 passed (incl. 26 new secret_manager tests)

  Total **1501 / 1501 green** across all crates.

### Hygiene

- **`.gitignore`** — adds `*.profraw` (cargo test residue),
  `docs/{tl,pm}/daily-report-*.md` (agent operational logs belong on
  shared wiki), `/research/` (researcher agent local notes), `/python/spikes/`
  (active spike workspaces, promoted to production on completion), `/uv.lock`.

---

## [1.11.0] - 2026-05-04

RFC-21 — Identity Resolution & Per-Agent Credential Isolation. Closes
[#21](https://github.com/zhixuli0406/DuDuClaw/issues/21) by addressing all
three architectural gaps the reporter identified: identity resolution
walked the shared wiki instead of an authoritative external source, Odoo
MCP credentials shared one global admin slot across every agent, and the
shared wiki had no source-of-truth boundary so an evolving agent could
silently overwrite externally-synced data. All three are now enforced at
the system layer (dispatcher / pool / namespace policy) instead of relying
on SOUL.md prompt-layer self-restraint.

### Added — `duduclaw-identity` crate (§1)

- **`IdentityProvider` async trait** + `ResolvedPerson` (`person_id`,
  `display_name`, `roles`, `project_ids`, `emails`, `channel_handles`,
  `source`, `fetched_at`) + `ChannelKind` enum (Discord / Line / Telegram
  / Slack / WhatsApp / Feishu / WebChat / Email + `Other(_)` catch-all
  with stable wire format) + `IdentityError` (Unreachable / Malformed /
  Unsupported / Io / Internal).
- **`WikiCacheIdentityProvider`** reads `<home>/shared/wiki/identity/people/*.md`
  per-person YAML frontmatter records; tolerates malformed files and
  missing optional fields; mtime-driven `fetched_at`.
- **`NotionIdentityProvider`** queries Notion `databases/query` with
  configurable `NotionFieldMap` (property names + `ProjectsKind`
  multi_select / relation). HTTP errors classify cleanly: 5xx /
  network ⇒ Unreachable (chained provider degrades), 4xx ⇒ Malformed.
- **`ChainedProvider`** combines cache + upstream — cache hit
  short-circuits; cache miss falls through; upstream unreachable
  degrades to `Ok(None)` rather than hard-erroring; project membership
  prefers upstream then falls back to cache.
- **`identity_resolve` MCP tool** + new `Scope::IdentityRead`
  ("identity:read") gates the tool. Audit row emitted per call.
- **`<sender>` XML block auto-injection** into channel reply system
  prompt (`crates/duduclaw-gateway/src/channel_reply.rs`). Sender is
  resolved once per turn; XML-escaped to keep the envelope intact;
  optional fields omitted when empty. Empty result ⇒ block omitted ⇒
  v1.10.1 behaviour preserved.

### Added — Per-agent Odoo credential isolation (§2)

- **`agent.toml [odoo]` override block** parsed via new
  `duduclaw-odoo::AgentOdooConfig`: `profile` / `username` /
  `api_key_enc` / `password_enc` / `allowed_models` /
  `allowed_actions` / `company_ids`. Empty / malformed block returns
  None; agent without override falls back to global config.
- **`OdooConfigResolver`** layers global + per-agent; `pool_key_for`
  produces stable `(agent_id, profile)` pool keys.
- **`OdooConnectorPool`** (new `crates/duduclaw-cli/src/odoo_pool.rs`)
  replaces the v1.10.1 global `Arc<RwLock<Option<OdooConnector>>>` with
  a `(agent_id, profile)`-keyed pool. Outer `RwLock<HashMap>` for
  membership reads + per-slot `tokio::sync::Mutex` for first-use
  connect serialisation. `get_or_connect(decrypt)` → cached
  `Arc<OdooConnector>` or cold-connect via merged credentials.
  `set_global` preserves per-agent overrides on hot-reload;
  `disconnect`/`disconnect_all`/`is_connected` complete the lifecycle.
- **`Scope::OdooRead` / `OdooWrite` / `OdooExecute`** added to
  `mcp_auth.rs`. All 14 `odoo_*` tools registered into
  `tool_requires_scope` — read class (status / connect / search /
  CRM leads / sale orders / inventory / invoice / payment), write
  class (create lead / update stage / create quotation), execute class
  (sale confirm / generic execute / report).
- **`allowed_models` / `allowed_actions` defence-in-depth filter** —
  `check_action_permission(verb, model)` runs before any HTTP request
  leaves the process; supports bare verbs (`"read"` → all models) and
  qualified verbs (`"write:crm.lead"` → only crm.lead). Policy denials
  audited as DENIED rows.
- **Audit attribution**: `tool_calls.jsonl` rows for Odoo calls now
  carry `params_summary = "profile=<profile>; tool=<name>; ok=<bool>"`
  so Odoo activity is traceable to the originating agent rather than
  the shared admin user inside Odoo's own audit log.
- **`handle_odoo_connect`** now reload-and-reconnect: re-reads
  `config.toml [odoo]` (set as global), re-reads
  `agents/<caller>/agent.toml [odoo]` (registers as override),
  forces `disconnect(caller)`, then `get_or_connect`. The connection
  report includes the resolved `(agent, profile)`.

### Added — Shared wiki SoT namespace policy (§3)

- **`~/.duduclaw/shared/wiki/.scope.toml`** declares which top-level
  namespaces are read-only / operator-only. Three modes:
  `agent_writable` (default — same as v1.10.1, no regression),
  `read_only { synced_from = "<capability>" }` (only the named internal
  capability or operator may write), `operator_only` (never writable
  via MCP).
- **Enforcement** in both `handle_shared_wiki_write` and
  `handle_shared_wiki_delete` — the namespace policy is the authority,
  not the per-page ACL. Read-only namespaces deny even the original
  page author from deleting.
- **`wiki_namespace_status` MCP tool** lets agents introspect the
  active policy before attempting a write.
- **Fail-safe**: absent file ⇒ empty policy ⇒ everything writable.
  Malformed TOML ⇒ logged warning + treated as no policy. Hot-reload
  is automatic — every write/delete re-reads the file (KB-sized; not
  on the hot path).
- **Reserved policy filename**: `.scope.toml` is implicitly rejected by
  the existing `.md` extension check in `validate_wiki_page_path`; no
  separate reserved-list entry needed.

### Added — Documentation

- **`docs/RFC-21-identity-credential-isolation.md`** — original design
  doc with three-section migration plan, acceptance criteria, risks,
  and rollout strategy.
- **`docs/RFC-21-operator-guide.md`** — step-by-step deployment
  playbook for all three sections, with verify commands, common
  pitfalls, and migration sequence from the v1.10.1 single-tenant
  deployment.
- **`docs/features/17-wiki-knowledge-layer.md`** updated with the
  namespace SoT policy section.
- **`CLAUDE.md`** Architecture Overview header bumped to v1.11.0; new
  bullets summarising RFC-21 §1 / §2 / §3 in the relevant sections.

### Tests

Cross four crates, **1193 unit + integration tests pass** with no
regression:

- `duduclaw-identity` 31/31 (15 wiki_cache + 7 chained + 9 notion) +
  1 doctest
- `duduclaw-odoo` 27/27 (15 new agent_config tests on top of existing
  12)
- `duduclaw-cli` 301/301 — 15 wiki_scope unit + 12 odoo_pool unit + 14
  odoo_pool_dispatch integration + 4 identity_resolve integration + 7
  new wiki_schema_tests for namespace policy enforcement
- `duduclaw-gateway` 834/834 (7 new sender_block tests)

### Backwards compat

Every section preserves v1.10.1 behaviour for deployments that don't
opt in:

- Absent `.scope.toml` ⇒ no namespace restrictions.
- Absent `[identity]` ⇒ no `<sender>` block; `shared_wiki_read` for
  identity continues to work.
- Absent `agent.toml [odoo]` ⇒ pool collapses to `(agent_id,
  "default")` slot using global config exactly as before.

No flag-day migration required.

### Commits

`867e719` (RFC) → `1a967f5` (§3) → `53e19a8` (§1 step 1-2) → `5c0b116`
(§1 step 4) → `a17ba5a` (§2) → `9a40c18` (§1 step 3) → `3269ca0`
(operator guide + status reflection) → `<this commit>` (v1.11.0 release).


## [1.10.1] - 2026-05-04

### Fixed — Release pipeline
- **PyPI publish 失敗修正**：`pyproject.toml` 仍停留在 `1.8.0`（自 v1.8.0 release 後未隨 workspace 同步），導致 v1.10.0 release workflow 嘗試重複上傳已存在的 `duduclaw-1.8.0-py3-none-any.whl`，被 PyPI 拒以 `400 File already exists`。本版同步將 Python SDK 版本提升至 `1.10.1`，與 Cargo workspace 對齊。
- **`pypa/gh-action-pypi-publish` 加上 `skip-existing: true`**：未來若同一版本被重新觸發（workflow_dispatch 重跑、tag 重推），PyPI 步驟會跳過而非整個 release job 失敗。Trusted Publisher 與 token fallback 兩條路徑都套用。

### 內容差異
- v1.10.0 的 GitHub Release 二進位、npm 套件已成功發佈；本 patch 主要是把 PyPI 的 `duduclaw` 套件補上來，並順帶 bump 一個 Cargo workspace patch 版本以走完整 release pipeline。Rust / web 程式碼相對 v1.10.0 無新增功能。


## [1.10.0] - 2026-05-03

### Added — Wiki RL Trust Feedback（核心新功能）
- **`duduclaw-memory` 新增** `trust_store.rs` / `feedback.rs` / `janitor.rs` — 預測誤差驅動的 wiki 信任反饋系統。
  - `WikiTrustStore`（SQLite，PK `(page_path, agent_id)` 每 agent 獨立 trust）
  - `CitationTracker` 用 turn_id 為 drain key、session_id 為 cap budget key（兩級 id），LRU + bounded-time 雙條件 eviction 防 keep-alive DoS
  - `WikiJanitor` 每日 pass：3 negatives in 30d 加 `corrected` tag、隔離 30d 後 archive 至 `wiki/_archive/`、frontmatter ↔ live trust 同步
  - 防禦：per-page daily cap (10/day)、per-conv Δ cap (0.10)、`VerifiedFact` ×0.5 抗性、`lock=true` 人工 override、0.10/0.20 archive hysteresis
- **`duduclaw-gateway` 新增** `prediction/feedback_bus.rs` / `wiki_trust_federation.rs` — `TrustFeedbackBus` 在每次 `PredictionError` 後 drain `CitationTracker` 並 dispatch 簽名 deltas（error < 0.20 → positive、≥ 0.55 → negative）；GVU 結果以 2× magnitude 經 `on_gvu_outcome` 進信任反饋。
- **Federation 同步**（Q3）：trust 信號可跨機 export/import，衝突取均值、`do_not_inject` 取 OR、`schema_version` 拒絕未來版本、5000 updates/push + 1 MiB body 上限 + `constant_time_eq` bearer。
- **MCP 工具**：`wiki_trust_audit` / `wiki_trust_history`；RPC `wiki.trust_audit / trust_history / trust_override`。
- **Search ranking** 改為 `score × (0.5 + live_trust) × source_type_factor`（verified_fact ×1.2，raw_dialogue ×0.6）。
- **Web** 新增 `WikiTrustPage.tsx` 儀表板（trust 列表、history、override、archive 操作）。
- 文件：[docs/wiki-trust-feedback.md](docs/wiki-trust-feedback.md) runbook + 架構說明。

### Added — v1.10 收尾
- **Sub-agent enqueue turn_id 完整貫通**：`DUDUCLAW_TURN_ID` / `DUDUCLAW_SESSION_ID` 兩個 env var 常數，gateway spawn Claude CLI 時 set，MCP `send_to_agent` 讀 env 並寫入 `message_queue.{turn_id, session_id}`，dispatcher 從 queue 讀回後重新 scope。channel → 頂層 agent → MCP send_to_agent → SQLite queue → dispatcher → 子 agent CLI 全鏈 turn_id/session_id 正確傳遞。
- **`flock` for `wiki_trust.db`**：advisory file lock 防多 process 共用 home_dir 造成 archive race / frontmatter 競爭，第二個 process fail-fast 並回明確錯誤。
- **Atomic batch upsert（真正單 Tx）**：`WikiTrustStore::upsert_signal_batch` 一次 `BEGIN IMMEDIATE` 處理整批；32 citations / 1 prediction error 從 32 fsync 收斂為 **1 fsync**；任何中途錯誤自動 rollback。原本延後到 v1.11 的計畫**提前在 v1.10 完成**。
- **ABS migration once-only**：`wiki_trust_meta` 標記 conv_cap ABS migration 已完成，避免每次 boot 全表掃描。

### Schema migration
- `message_queue.turn_id` / `message_queue.session_id` columns 自動新增（既有資料庫升級時 NULL，新訊息會帶值）
- `wiki_trust_meta(key, value)` 新表 + `conv_cap_abs_migration_done` 標記
- `wiki_trust_state` / `wiki_trust_history` / `wiki_trust_rate` / `wiki_trust_conv_cap`（PK rename `conversation_id` → `cap_budget_id`）/ `idx_wiki_trust_history_agent_kind_ts` / `idx_wiki_trust_history_ts`

### Tests
- Backend **126 tests pass**（duduclaw-memory），包含 5 個 v1.10 regression test：flock、batch order、batch cap-budget shared、batch single-Tx、migration once-only
- 5 輪深度審查（code / security / database / architecture）+ Round 5 SHIP-BLOCK 修復全數收斂


## [1.9.4] - 2026-05-02

### Added
- **`duduclaw-durability` crate** — five-pillar durability framework:
  `idempotency` (key 管理防止重複執行)、`retry`（指數退避 + jitter）、
  `circuit_breaker`（三態 Closed/Open/HalfOpen）、`checkpoint`（任務進度
  斷點續傳）、`dlq`（Dead Letter Queue 終態失敗訊息）。完整 unit +
  integration tests 涵蓋高並發場景。
- **`duduclaw-governance` crate**（W19-P1 M1-A）— PolicyRegistry +
  4 種 PolicyType（Rate / Permission / Quota / Lifecycle）+ YAML 載入 +
  熱重載 + Agent 優先序合併 + fail-safe（非法政策跳過、非法 YAML 不
  panic）+ 並發 upsert 安全。新增 `quota_manager.rs`（每 agent / 每
  policy 配額 soft/hard 強制）+ `error_codes.rs`（QUOTA_EXCEEDED /
  POLICY_DENIED 等標準化錯誤碼）+ `evaluator` / `violation` /
  `approval` / `audit` 完整 PolicyEngine。預設政策集 `policies/global.yaml`
  含 default-rate-mcp（200/min MCP 呼叫限制）等六項。
- **MCP HTTP/SSE Transport**（W20-P1/P2）— 新增 `duduclaw http-server
  --bind 127.0.0.1:8765` 子命令。`mcp_http_server.rs` 提供
  `POST /mcp/v1/call`（單次 JSON-RPC 2.0 工具呼叫）、
  `GET /mcp/v1/stream`（SSE 長連接事件流，Bearer / `?api_key=`）、
  `POST /mcp/v1/stream/call`（async + SSE 結果推送）、`GET /healthz`
  （無需認證）。`mcp_rate_limit.rs` 新增 `OpType::HttpRequest`（60
  req/min token bucket），`mcp_sse_store.rs` 連線管理與 broadcast
  channel 事件推送，`mcp_http_auth.rs` / `mcp_http_errors.rs` 處理
  認證 + JSON-RPC↔HTTP 錯誤映射。
- **`skill_synthesis_run` MCP tool**（W20-P0）— Internal principal 可見、
  external 隱藏。`pipeline.rs::graduate_trajectories()` 取代 Phase 2
  stub，串起 memory_search → skill_extract → security_scan →
  skill_graduate 完整流程。
- **`duduclaw-memory` 評測 batch query API** — 新增 `MemoryEngine`
  方法支援評測批次查詢，配合 LOCOMO 評測系統。
- **LOCOMO 記憶評測系統**（W21）— `python/duduclaw/memory_eval/`：
  `retrieval_accuracy` / `retention_rate` / `locomo_integrity_check`
  + `cron_runner`（每日 03:00 UTC 排程）+ 5 分鐘 `smoke_test` P0 +
  `build_golden_qa`（從 LOCOMO 資料集建構黃金 QA）+
  `data/golden_qa_set.jsonl`（首批 200 筆 golden QA）+ `client.py` /
  `config.py` / `db/consolidation.py`。
- **Python `agents/` + `mcp/` 模組** — `agents/capabilities/`
  （manifest 載入 + matcher）、`agents/routing/`（capability-based
  router + resolution + memory_resolver）；`mcp/auth/`（API Key 驗證
  含 key masking 防洩漏）、`mcp/tools/memory/`（store / read / search
  / namespace / quota 含 scope 強制驗證）。
- **LLM Fallback** — `claude_runner.rs` + `llm_fallback.rs`：主模型
  逾時 / 503 / 429 / overloaded 時自動切換 fallback 模型。新增
  `is_llm_fallback_error` / `should_attempt_model_fallback` 純函式
  + 完整 unit tests。
- **Evolution Events 系統擴充** — `schema.rs` 新增 30+ event schema
  定義（+483 行）、`emitter.rs` 非同步發送支援 batch + retry（+190
  行）、新增 `query.rs`（EvolutionEvent 查詢介面，1685 行）+
  `reliability.rs`（事件可靠性保證機制，324 行）。Gateway HTTP
  endpoints 暴露於 `handlers.rs`（+154 行）。
- **Web `ReliabilityPage`**（+328 行，`/reliability` 路由）— circuit
  breaker 狀態、retry 統計、DLQ 佇列深度即時儀表板。`api.ts` 新增
  `getEvolutionEvents` / `getReliabilityStats` / `getDlqItems`。
- **`duduclaw evolution finalize` CLI 子命令**（v1.9.1 引入，v1.9.4
  封版穩定）— `--dry-run` / `--agent <id>`，一次性回收逾期 SOUL.md
  觀察視窗。
- **`claude_desktop_config.example.json`** — Claude Desktop MCP Server
  整合設定範例。

### Fixed (W21 QA 4-round CRITICAL/HIGH 全清)
- **CRITICAL — 記憶 MCP scope 認證缺口**：`mcp/tools/memory/store.py`、
  `read.py`、`search.py` 在 `execute()` 進入點補上 `memory:write` /
  `memory:read` scope 強制檢查。修補先前任意有效 API Key 都能繞過
  scope 限制的認證缺口。
- **HIGH — XSS 儲存型注入**：`validation.py::validated_tags` 改用
  `_sanitize(tag)` 處理使用者輸入的 tag。
- **HIGH — SSRF 防護**：`client.py::build_client()` 新增 URL
  scheme/netloc 驗證，拒絕指向內網或私有位址的 URL。
- **HIGH — circuit breaker 幽靈探測**：`circuit_breaker.rs`
  OPEN→HALF_OPEN 轉換時補上 `probe_inflight.saturating_add(1)`。修復
  並發探測數比設計上限多 1 的 bug。
- **HIGH — `claude_runner.rs` hard deadline 邏輯**：移除 partial
  output 時 `break` 的分支，統一回傳含 "hard timeout" 字串的 `Err`，
  確保 `is_llm_fallback_error` 正確觸發 fallback。
- **HIGH — UTF-8 truncation panic**：`llm_fallback.rs` truncation 改用
  `char_indices` 安全 UTF-8 char boundary 切片。修復多位元組字元在
  byte 512 邊界處切割時的 runtime panic。
- **Web 高危依賴**：`vite` 8.0.0-8.0.4 → 8.0.5+（GHSA-4w7w-66w2-5vf9
  + GHSA-v2wj-q39q-566r + GHSA-p9ff-h696-f583：Path Traversal in
  Optimized Deps、`server.fs.deny` bypass、Arbitrary File Read via
  WebSocket）；`postcss` <8.5.10 → 8.5.10+（GHSA-qx2v-qp2m-jg93：XSS
  via Unescaped `</style>` in CSS Stringify Output）。npm audit 0
  vulnerabilities。
- **Inference 編譯**：`ProgressCallback` 補上 `Sync` trait bound，修復
  多執行緒共享場景編譯錯誤。

### Tests
- 549+ tests, 0 failures（包含 `duduclaw-durability`、
  `duduclaw-governance` 73 tests + integration 22 個 W19-P1 M1-A
  驗收項、MCP HTTP transport tests、LLM fallback unit tests、Python
  agents routing + memory MCP tools 含 api_key_masking 安全測試）。

### Build/Repo
- `.gitignore` 排除 Python coverage db (`.coverage` /
  `**/.coverage`)、`release artifacts/`、各平台 `npm/*/bin/` 預建
  binary（應透過 npm publish）。
- `pyproject.toml` 更新 Python 依賴版本（memory_eval / agents / mcp
  相關套件）。


## [1.9.3] - 2026-04-28

### Fixed
- **Heartbeat: task-board pull 對所有 agent 生效，無視 enabled flag**。
  `poll_assigned_tasks` 之前在 `execute_heartbeat` 內，僅當 agent 心跳
  config `enabled=true` 才會跑。生產環境 17 個 agent 中有 16 個預設
  `enabled=false`，於是新加的 task board pull 對最需要它的 agent 從
  未觸發 — 包括 2026-04-28 12:27 觀察到的 26 個未路由 backlog 任務。
  修正：將 pull 上移到 `HeartbeatScheduler::run` 的 tick body，每 30s
  掃描整個 agent registry。`poll_assigned_tasks` 原有的 1-hour LIKE
  marker cooldown 已防止 stampede。task board pull 概念上屬 scheduler
  層級而非 per-agent evolution，agent 不該為了被指派工作時被叫醒而
  opt-in。


## [1.9.2] - 2026-04-28

### Fixed
- **Discord Gateway: 真正實作 RESUME (op 6) + stall watchdog**
  （`discord.rs`）。
  - 持久化 `session_id` + `resume_gateway_url` + sequence 跨重連。
    先前每次重連都發新的 IDENTIFY，丟掉 Discord 在斷線期間緩衝的所有
    事件。
  - 第三個 `select!` arm 加入 stall watchdog：超過 2× heartbeat
    interval 沒有任何流量就 break。修復 2026-04-28 11:17Z 觀察到的
    silent zombie 狀態，gateway loop 卡住 18 分鐘無任何 log 輸出。
  - heartbeat channel capacity `1 → 16` + `try_send` 防止 `select!`
    消費慢時反向阻塞。
  - Op 9 Invalid Session 讀 `d.bool` 決定 RESUME vs IDENTIFY，依
    Discord docs 加 1-5s jitter。
  - close codes 4007/4009/4003 清掉 session state 觸發新 IDENTIFY。
  - backoff cap 300s → 60s；不要懲罰已經跑了好幾小時的 session。
  - 處理 `RESUMED` dispatch event。


## [1.9.1] - 2026-04-28

### Added
- **`duduclaw evolution finalize` CLI subcommand** with `--dry-run` and
  `--agent <id>` filters. One-shot recovery for SOUL.md observation
  windows that should already have transitioned but never did.

### Fixed (self-evolution pipeline — 5 audit gaps from 2026-04-28 health check)
- **SOUL.md observation windows now actually close.**
  `VersionStore::get_expired_observations` and `Updater::execute_confirm /
  execute_rollback` had no callers, so the very first applied SOUL change
  blocked all subsequent GVU proposals indefinitely. agnes was stuck for
  6 days locally. Adds a 30-min `ObservationFinalizer` background task
  that computes post-metrics from `prediction.db` + `feedback.jsonl`,
  runs the existing `judge_outcome` tolerance logic, and confirms /
  rolls back / extends accordingly.
- **EvolutionEvents audit log now writes to a stable absolute path.**
  Default base directory was `data/evolution/events` — relative to cwd.
  Gateway boot from `cwd=$HOME` silently dropped every audit event. Now
  resolves via layered fallback: `$EVOLUTION_EVENTS_DIR` →
  `$DUDUCLAW_HOME/evolution/events` → `$HOME/.duduclaw/evolution/events`
  → legacy. Boot also injects the env var before any emitter is
  constructed and runs a `.healthcheck` self-test that surfaces IO
  failures via `tracing::error!` instead of silent `eprintln!`.
- **Silence breaker now actually triggers a forced reflection.**
  `heartbeat.rs` previously only emitted `warn!` and reset its own timer
  — the system advertised "self-reflection on long silence" but never
  did anything. Adds a `SilenceBreakerEvent` mpsc channel; the gateway
  consumes it and writes a typed `silence_breaker` row to
  `prediction.db.evolution_events`, with a 4-hour per-agent cooldown to
  prevent loops.
- **MetaCognition rehydrates counters from `prediction.db` on startup.**
  `total_predictions` and `predictions_since_last_eval` were stuck at 0
  across restarts because `metacognition.json` only persisted at
  evaluation time. With `evaluation_interval=100` the threshold became
  unreachable and adaptive thresholds never recalibrated. Now takes
  `max(disk, in-memory)` and runs a one-shot `evaluate_and_adjust` if
  the in-memory counter is overdue. Also anchors
  `original_sig_improvement_rate` baseline on the first eval that has
  ≥5 Significant samples (was previously stuck at `null`).
- **Sub-agent dispatches now record prediction samples.**
  `prediction.db.user_models` had only the channel-facing root agent
  (1/19 in our deployment); 18 sub-agents accumulated nothing because
  the prediction hook only ran in `channel_reply`, not in
  `dispatcher.rs`. Adds a fire-and-forget `subagent_prediction` module
  that synthesises `user_id = "agent:<sender_or_origin>"`, builds a
  2-message `ConversationMetrics` snapshot from the dispatched payload
  + response, and runs the same `predict → calculate_error →
  log_evolution_event → update_model` cycle as the channel path. Hooks
  both the JSONL and SQLite dispatch loops; deliberately does NOT
  trigger the GVU loop from this path (preserves the channel-only
  invariant for SOUL evolution).

### Tests
- 23 new unit tests across `observation_finalizer`, `evolution_events::logger`,
  `prediction::forced_reflection`, `prediction::metacognition` (BUG-4 group),
  and `prediction::subagent_prediction`.
- Workspace tests after the change:
  duduclaw-gateway 730 ✓, duduclaw-agent 31 ✓, duduclaw-cli 80 ✓.

### Dashboard
- ActivityFeed no longer crashes when the gateway emits an unknown
  `ActivityType`. Adds explicit entries for `autopilot_triggered` and
  `autopilot_lag`, plus a neutral `FALLBACK_CONFIG` so future unknown
  types render as a generic row instead of throwing on `config.icon`.


## [1.8.34] - 2026-04-27

### Fixed
- **Local-fallback path silently failed for users running a remote
  OpenAI-compatible inference server (vLLM / SGLang / llamafile).**
  Reproducer: Linux gateway with no Claude CLI installed,
  `inference_mode = "local"` in `config.toml`, and `[openai_compat]`
  pointing at `http://192.168.168.244:8000/v1` in `inference.toml`.
  Sending a message via the dashboard webchat returned
  `DuDu 暫時無法回應：系統找不到 Claude Code CLI` even though the
  remote vLLM endpoint was reachable and the model id matched.

  Root cause: `InferenceEngine::load_model` unconditionally called
  `ModelManager::resolve_path`, which only finds GGUF files under
  `~/.duduclaw/models/`. For remote backends the model lives on a
  server, so `resolve_path` returned `ModelNotFound` and the engine
  errored before `OpenAiCompatBackend` ever saw the request — making
  the `channel_reply` local-fallback path silently fail with the
  misleading "Claude Code CLI not found" final message.

  Gateway log evidence:
  ```
  WARN duduclaw_inference::engine: Failed to auto-load model
    model="qwen3.6-35b-a3b" error=Model not found: qwen3.6-35b-a3b
  WARN duduclaw_gateway::channel_reply: Local inference unavailable:
    Local inference error: Model not found: qwen3.6-35b-a3b
  WARN duduclaw_gateway::channel_reply: Channel reply fallback —
    all providers failed agent=DuDu reason=BinaryMissing
    last_error=claude CLI not found in PATH
  ```

  Fix: add `InferenceBackend::requires_local_file` (default `true`,
  override `false` in `OpenAiCompatBackend`) and gate `resolve_path`
  on it. Remote backends now receive the raw model id, which matches
  what `OpenAiCompatBackend::load_model` already does (ignores the
  path arg and uses `[openai_compat].base_url + .model` from
  `inference.toml`).

  Adds two regression tests in `engine::tests` using a stub backend:
  - `load_model_skips_path_resolution_for_remote_backends`
  - `load_model_still_resolves_path_for_local_backends`

  Workaround for users on ≤ 1.8.33: `touch
  ~/.duduclaw/models/<model-id>.gguf` to satisfy the path check.
  Safe to delete after upgrading to 1.8.34.


## [1.8.33] - 2026-04-27

### Fixed
- **Windows: BatBadBut spawn error persisted on hosts where the
  `@anthropic-ai/claude-code` npm package ships a native `.exe`
  instead of a JS CLI.** The customer reproducer on 2026-04-27
  (after v1.8.32 still failed) revealed the `claude.cmd` shim
  contents:

  ```bat
  @ECHO off
  GOTO start
  :find_dp0
  SET dp0=%~dp0
  EXIT /b
  :start
  SETLOCAL
  CALL :find_dp0
  "%dp0%\node_modules\@anthropic-ai\claude-code\bin\claude.exe"   %*
  ```

  `@anthropic-ai/claude-code` ≥ 2.x ships a real `claude.exe` inside
  the npm package and the cmd shim is just a transfer wrapper. The
  v1.8.32 shim parser only matched `.js`/`.mjs`/`.cjs` references,
  returned `None` for the `.exe` line, fell through to known-layout
  probes (which also only checked for `cli.js` / `cli.mjs`), returned
  `None` there too, and the caller spawned the `.cmd` directly →
  BatBadBut. The diagnostic log added in v1.8.32 confirmed it:

  ```
  INFO Resolved claude binary
    path=C:\Users\USER\AppData\Roaming\npm\claude.cmd
    candidates=[..., "...\\claude.cmd"]   ← no .exe in pool
  WARN claude CLI spawn error: batch file arguments are invalid
  ```

  **Fix**: extend the shim parser and probe table to follow shims
  that point to a real `.exe` (not just JavaScript scripts). Three
  rule changes in [`platform::resolve_cmd_shim`](crates/duduclaw-core/src/platform.rs):

  1. `clean_shim_token` now matches `.exe` in addition to
     `.js`/`.mjs`/`.cjs`. The result is typed:
     `ShimTarget { kind: Exe | Script, rel: String }`.

  2. **Per-line target selection rule**:
     - Line has BOTH `.exe` AND a script → **Script wins** (the
       `.exe` is the runtime — `node.exe` / `bun.exe` — and the
       script is the actual target). Handles Bun / pnpm / yarn
       JS shims.
     - Line has ONLY `.exe` → **Exe wins** (new-style native shim;
       the `.exe` IS the target). Handles the customer's case.
     - Line has ONLY a script → **Script wins** (legacy npm shims).

  3. `known_cli_subpaths` → `known_target_subpaths` now contains 5
     native-`.exe` probes covering npm / yarn / Bun / pnpm globals —
     each terminating at `node_modules/@anthropic-ai/claude-code/bin/claude.exe`.
     Legacy `cli.js` / `cli.mjs` probes are retained for older
     installs.

  After this change, the customer's spawn path becomes:
  `Command::new("C:\\Users\\USER\\AppData\\Roaming\\npm\\node_modules\\@anthropic-ai\\claude-code\\bin\\claude.exe")` —
  a direct `.exe` invocation with zero `cmd.exe` involvement and
  zero BatBadBut hazard, regardless of prompt content.

### Changed
- `resolve_cmd_to_node` (private) renamed to `resolve_cmd_shim` and
  now returns `Option<(String, Vec<String>)>` — a real executable
  plus prefix args — so callers can spawn either a direct `.exe`
  (`vec![]`) or `node + cli.js` (`vec![cli.js]`) uniformly.
  `command_for` / `async_command_for` updated accordingly.

### Tests
- Shim parser tests overhauled around the new `parse_shim_target`
  API. 14 cross-platform unit tests now cover:
  - the new-style native-`.exe` shim (the customer's exact
    `claude.cmd` content reproduced verbatim),
  - legacy JS shims for npm v9 / Bun / pnpm / yarn classic,
  - the **Script-wins-over-Exe-when-both-present** priority rule,
  - the multi-token-per-line ordering for both `.exe` and `.js`,
  - the empty-shim, unquoted-hand-written, and `.cjs` extension
    edge cases,
  - a `known_target_subpaths_cover_native_and_legacy` assertion
    that the probe table contains ≥4 native-`.exe` probes and ≥4
    JS probes, all targeting `@anthropic-ai/claude-code`.


## [1.8.32] - 2026-04-27

### Fixed
- **Windows: BatBadBut spawn error persisted after v1.8.31 because
  `which_claude` short-circuited on `where.exe` results before
  HOME-rooted candidates were consulted.** v1.8.31 reordered the HOME
  candidate list so `.exe` came before `.cmd`, but missed the more
  fundamental bug: [`which_claude`](crates/duduclaw-core/src/lib.rs)
  ran `where.exe claude` first and **returned the first matching
  `.exe` OR `.cmd` line**, never reaching the HOME scan. On hosts
  with both a clean `~/.local/bin/claude.exe` install AND a leftover
  `%APPDATA%\npm\claude.cmd`, `where.exe` typically returned the
  `.cmd` first when PATH included `%APPDATA%\Roaming\npm` (which it
  often does for service / launchd / Explorer-launched processes
  even though the user's interactive shell shows it empty). The
  `.cmd` then triggered Rust 1.77+'s
  [BatBadBut][batbadbut] rejection (CVE-2024-24576) for any prompt
  containing newlines / quotes / `&` — i.e. essentially every prompt.

  [batbadbut]: https://blog.rust-lang.org/2024/04/09/cve-2024-24576/

  **Fix**: `which_claude` now **pools** results from PATH discovery
  AND the HOME-rooted scan (deduped), then applies a strict
  precedence regardless of source:

  1. any `.exe` in the pool wins (always safe to spawn)
  2. then any `.cmd` (parsed by `resolve_cmd_to_node` into
     `node.exe + cli.js` to avoid handing args to `cmd.exe`)
  3. then extensionless paths with `.exe`/`.cmd` appended via FS check
  4. last resort: first existing entry as-is

  On the customer machine that was failing in v1.8.31, this means
  `where.exe claude` returning `%APPDATA%\Roaming\npm\claude.cmd`
  AND the HOME scan finding `~/.local/bin/claude.exe` now resolves
  to the `.exe` — bypassing the BatBadBut hazard entirely.

### Added
- **One-shot `INFO` log of the resolved `claude` binary path on the
  first `which_claude` call.** The log line includes both the chosen
  path and the full discovery pool. This means future Windows /
  multi-installer issue reports arrive with the resolved path
  already in the logs:

      INFO duduclaw_core: Resolved claude binary
        path="C:\\Users\\X\\.local\\bin\\claude.exe"
        candidates=["C:\\Users\\X\\AppData\\Roaming\\npm\\claude.cmd",
                    "C:\\Users\\X\\.local\\bin\\claude.exe"]

  Subsequent `which_claude` calls (there are 11 call sites — channel
  reply, account rotation, heartbeat, etc.) are silent so this never
  becomes log spam.

### Tests
- 7 new cross-platform unit tests in `which_claude_tests` exercise
  the new precedence rules:
  `windows_pref_exe_beats_cmd_even_when_cmd_listed_first`,
  `windows_pref_picks_cmd_when_no_exe_exists`,
  `windows_pref_returns_none_for_empty_pool`,
  `windows_pref_first_exe_wins_among_multiple_exes`,
  `windows_pref_first_cmd_wins_among_multiple_cmds_when_no_exe`,
  `windows_pref_extension_check_is_case_insensitive` (handles
  uppercase `.EXE` / `.CMD` from PATHEXT-style discovery), and
  `windows_pref_falls_back_to_first_for_extensionless_when_no_fs_match`.

  Compile-gated with `#[cfg(any(windows, test))]` on the helper
  `pick_windows_preferred` so macOS / Linux CI runners can validate
  the Windows-only logic without needing a Windows host.


## [1.8.31] - 2026-04-27

### Fixed
- **Windows: `claude CLI spawn error: batch file arguments are
  invalid` blocking every channel reply.** Rust 1.77+ rejects spawning
  `.bat`/`.cmd` files when argv contains characters that could be
  reinterpreted by `cmd.exe` (newlines, quotes, `&`, `|`, …) — the
  [BatBadBut][batbadbut] mitigation for CVE-2024-24576. User prompts
  and system prompts routinely contain those characters, so `claude
  -p` subprocess calls failed at spawn time on every Windows host
  whose `which_claude` resolved to `%APPDATA%\npm\claude.cmd` (or any
  other npm/Bun/pnpm/yarn `.cmd` shim). The rotator interpreted the
  spawn failure as an account error, retried each account in turn, and
  surfaced the misleading `All accounts exhausted` to the user.

  [batbadbut]: https://blog.rust-lang.org/2024/04/09/cve-2024-24576/

  **Two-layer fix in `duduclaw-core`:**

  1. [`which_claude_in_home`](crates/duduclaw-core/src/lib.rs) on
     Windows now **prefers `.exe` over `.cmd`** in candidate ordering.
     A host with both a real `.exe` install (e.g. Claude Code native
     installer at `~/.local/bin/claude.exe`) and a leftover npm
     `.cmd` shim previously matched the `.cmd` first and tripped
     BatBadBut. Reordered so every `.exe` location is checked before
     any `.cmd`. Also added the **`~/.local/bin/claude.exe`** path
     (the official native installer's XDG-style location on Windows,
     previously missing) plus pnpm / Yarn-classic / Bun-`.cmd` /
     Volta-`.cmd` fallbacks.

  2. [`platform::resolve_cmd_to_node`](crates/duduclaw-core/src/platform.rs)
     — the npm-shim parser that converts a `.cmd` shim into a
     `node.exe + cli.js` invocation (so we never hand args to
     `cmd.exe`) — previously only matched paths containing
     `node_modules` ending in `.mjs`/`.js`. Bun (`..\packages\…`),
     pnpm (`..\global\5\node_modules\…`), and Yarn classic
     (`..\lib\node_modules\…`) all parsed as `None` and fell through
     to the BatBadBut path. New parser scans every quoted segment +
     every whitespace token, expands `%~dp0` / `%dp0%` / `%~dpn0` /
     `%~f0` / `%CD%` to empty, normalizes `\` to `/` for
     cross-platform path joining, accepts `.cjs`, and picks the
     *last* JS token per line so wrapper scripts don't shadow the
     real `cli.js`. When parsing still fails (binary wrappers, custom
     shims), a known-layout probe checks 6 well-known relative paths
     from the shim directory to `@anthropic-ai/claude-code/cli.js`
     for npm / Bun / yarn / pnpm.

  **Diagnostic note**: `where claude` returning empty on the customer
  machine was a red herring — `which_claude`'s HOME-rooted candidate
  scan still found `~/.local/bin/claude.exe`. The actual root cause
  was the `.cmd`-before-`.exe` ordering shadowing it.

### Tests
- 11 new cross-platform unit tests in `platform::shim_parser_tests`
  exercise npm v9 / Bun / pnpm / Yarn-classic shim formats, the
  pure-`.exe`-wrapper case, multi-`.js`-per-line ordering, `.cjs`
  extension handling, and unquoted-token fallback. Compile-gated with
  `#[cfg(any(windows, test))]` so they run on macOS/Linux CI hosts
  and validate the parser without needing a Windows runner.


## [1.8.30] - 2026-04-24

### Fixed
- **Native Claude Code tools (`WebSearch` / `WebFetch` / `Read` /
  `Write` / `Edit` / `Glob` / `Grep` / `Bash` / `TodoWrite`) were
  silently unavailable to `claude -p` subprocesses**, causing
  researcher cron tasks to receive 0 results and bail out even when
  the same tools worked in interactive Claude Code sessions.

  **Root cause**: [`claude_runner.rs`](crates/duduclaw-gateway/src/claude_runner.rs)
  passed `--allowedTools "mcp__duduclaw__*"` to `claude -p`. Claude
  Code treats `--allowedTools` as an **exclusive** auto-approve list,
  not an *additive* one: anything not matching would need interactive
  confirmation, which is impossible in subprocess mode. The built-in
  tools therefore returned empty / no-oped with no error signal.

  User-visible symptom (from the 2026-04-24 evening cron run): the
  `ai-papers-researcher` / `ai-repos-researcher` agents correctly
  followed their updated SOUL.md and cron prompts (which now direct
  them to use native `WebSearch` instead of the DDG-blocked MCP
  `web_search`), invoked `WebSearch`, got 0 results, and — per the
  hard-stop rule — aborted with "搜尋工具失效" inside six seconds.
  The equivalent query run interactively via Claude Code returned
  normal results immediately.

  **Fix**: expand the `--allowedTools` list to explicitly include the
  native tool names researchers actually need:

      mcp__duduclaw__*,WebSearch,WebFetch,Read,Write,Edit,
      Glob,Grep,Bash,TodoWrite

  This keeps the deny-by-default posture for anything not listed
  (e.g. no `KillBash` / `NotebookEdit` / etc.) while restoring the
  research capability that interactive Claude Code has had all along.
  `disallowed_tools` from `agent.toml [capabilities]` still layers on
  top via `--disallowedTools`, so explicit per-agent blocks are
  unchanged.


## [1.8.29] - 2026-04-24

### Fixed
- **Misleading "No auth token configured" startup banner.** The CLI
  always printed that message whenever `DUDUCLAW_AUTH_TOKEN` and
  `[gateway].auth_token` were both unset — but the WebSocket auth gate
  in `server::handle_socket` *also* requires JWT when `users.db`
  contains any rows (legacy `auth_token` and JWT are independent gates).
  Operators saw the message, assumed authentication was off, and then
  got spammed with `WebSocket auth failed – closing connection` once
  per second as the dashboard reconnected — with no hint that the real
  fix was to log in at `/login`.

### Changed
- [`duduclaw run`](crates/duduclaw-cli/src/lib.rs) now probes
  `~/.duduclaw/users.db` at startup (via `probe_users_db`). When any
  user exists the banner switches from "no auth token" to:

  ```
  🔐 JWT auth required: N user(s) in ~/.duduclaw/users.db
    Dashboard login: http://localhost:PORT/login
  ```

  so the correct next action is obvious.

- When `admin@local`'s stored password hash still verifies against the
  literal `"admin"` seeded by
  `duduclaw_auth::UserDb::ensure_default_admin`, an additional line
  warns: `⚠ Default admin still in use: admin@local / admin — change the
  password at /settings`. The verification uses the `argon2` crate
  directly (now a direct `duduclaw-cli` dep) rather than the full
  `duduclaw-auth` crate to keep the CLI's dependency surface narrow.

### Added
- 6 new unit tests in `startup_probe_tests` covering: missing
  `users.db`, empty users table, default-admin detection,
  non-default-password non-detection, admin@local absence, and
  garbage-PHC input handling.


## [1.8.28] - 2026-04-24

### Fixed
- **Cron notifications failed silently with Discord 401 Unauthorized
  in multi-bot setups.** When a cron-fired agent (e.g. `xianwen-pm`,
  `ai-papers-researcher`) had no per-agent
  `[channels.discord] bot_token` set in its `agent.toml`, the token
  resolver fell straight to the **global** `config.toml [channels]
  discord_bot_token_enc`. If that global token belongs to a different
  bot from the one that opened the notify target — and Discord threads
  are bot-scoped so only the opening bot can post into them — every
  delivery attempt returned `401 Unauthorized` even though the agent
  LLM call had already succeeded. User-visible symptom: cron
  `last_status = success` but nothing arrives in the Discord thread.

  **Fix**: new `resolve_agent_channel_token_via_reports_to` in
  [`config_crypto.rs`](crates/duduclaw-gateway/src/config_crypto.rs)
  walks the `reports_to` chain and returns the first ancestor's token.
  Cycle-safe (tracks visited ids) and bounded (`MAX_REPORTS_TO_HOPS =
  8`). Wired into both:

  1. [`cron_scheduler::resolve_channel_token`](crates/duduclaw-gateway/src/cron_scheduler.rs) — the cron
     `deliver_cron_result` path.
  2. [`dispatcher::resolve_forward_token`](crates/duduclaw-gateway/src/dispatcher.rs) — the
     `forward_delegation_response` path that relays sub-agent replies
     back to the originating channel.

  After this change, a cron-fired `xianwen-pm` with no Discord bot of
  its own inherits `xianwen-tl`'s token, or `agnes`'s if the TL also
  has none configured — matching the `reports_to` hierarchy the user
  already declared.

### Changed
- `resolve_forward_token` now does the `reports_to` cascade on **both**
  `callback_agent_id` AND `origin_agent` (the thread opener). The
  v1.8.20 behaviour of falling back to `origin_agent`'s direct token
  is preserved as step 3 in the cascade; steps 1-2 add the new walk so
  agents deeper in the hierarchy are covered without needing every TL
  / PM / researcher to have the same bot token pasted into their
  `agent.toml`.

- The stale single-purpose `get_agent_channel_token` helper in
  `dispatcher.rs` is removed — superseded by the shared cascade helper
  in `config_crypto.rs`.

### Added
- 8 new unit tests in `config_crypto::tests` covering the cascade:
  own-token wins, parent-token cascade, `None` when chain is empty,
  nearest-ancestor-not-farthest preference, cycle detection, missing
  agent.toml, `reports_to = ""` treated as root, and per-channel
  independence.


## [1.8.27] - 2026-04-23

### Added
- **Multica-inspired Agent integration layer** — agents are now
  first-class teammates on the task board, not just tools. Ships three
  coupled pieces:

  1. **12 new MCP tools** (`crates/duduclaw-cli/src/mcp.rs`) —
     `tasks_list`, `tasks_create`, `tasks_update`, `tasks_claim`,
     `tasks_complete`, `tasks_block`, `activity_post`, `activity_list`,
     `autopilot_list`, `shared_skill_list`, `shared_skill_share`,
     `shared_skill_adopt`. All mutating tools enforce
     `is_valid_agent_id` on the caller, and `tasks_list` defaults to
     the calling agent so noise stays low.
  2. **Pending task queue injection into the agent system prompt**
     (`crates/duduclaw-gateway/src/claude_runner.rs`) — every call to
     `call_claude_for_agent*` renders the top-5 open tasks (priority-
     ordered, `in_progress` → `todo` → `blocked`) into a
     `## Your Task Queue` block. Uses a shared `Arc<TaskStore>` via
     `OnceLock` so system-prompt composition doesn't open a fresh
     SQLite connection per turn. On the Direct API path the block is
     passed as an uncached second system block via
     `direct_api::call_direct_api_with_dynamic`, so the static 5–20k
     token prefix stays cacheable.
  3. **Autopilot trigger engine** (`autopilot_engine.rs`, new) —
     `tokio::broadcast::Sender<AutopilotEvent>` (capacity 8192) fed by
     both WebSocket handlers (in-process) and a SQLite event bus
     (out-of-process, see below). Typed variants: `TaskCreated`,
     `TaskUpdated`, `TaskStatusChanged`, `ActivityNew`, `ChannelMessage`,
     `AgentIdle`, `CronTick`. Condition DSL supports nested `all`/`any`
     + `eq`/`neq`/`in`/`not_in`/`gt`/`gte`/`lt`/`lte`/`contains`. Three
     action executors: `delegate` (MessageQueue enqueue), `notify`
     (Telegram/LINE/Discord/Slack via shared `reqwest::Client` from
     `OnceLock`), `run_skill` (reads the agent's `SKILLS/<name>.md`
     and delegates it as a prompt).

- **SQLite event bus** (`events_store.rs`, new) — `events.db` replaces
  the legacy `events.jsonl` file bus. WAL mode + `busy_timeout=5000` +
  monotonic auto-increment `id` give the tail reader a simple
  `WHERE id > ?` watermark; 7-day retention prune runs every 6 hours.
  Eliminates the file-bus hazard matrix in one swap (rotation race,
  partial-line reads, 0644 permissions, unbounded growth).

- **Dashboard Task Board preview widget** (`DashboardPage.tsx`) —
  `TasksPreviewCard` renders a mini 4-column Kanban with per-column
  task counts and links to `/tasks`. Loading skeleton, error banner,
  and empty-state tri-state so users can distinguish "never loaded"
  from "loaded empty".

- **Autopilot rule dashboard schema validation** (`handlers.rs`) —
  `autopilot.create` / `autopilot.update` reject unknown
  `trigger_event` values and `action` JSON missing required fields
  per type, so malformed rules fail immediately on the dashboard
  instead of silently during the first fire.

- **i18n keys** `tasks.preview.{title,viewAll,empty}` synced across
  `zh-TW`, `en`, `ja-JP`.

- **47 new unit tests** — 18 in `mcp::task_board_tests`, 18 in
  `autopilot_engine::tests` (including Closed/Open/HalfOpen state
  transitions), 7 in `handlers::autopilot_validation_tests`, 4 in
  `events_store::tests`. Full gateway lib suite: 611 tests passing.

### Changed
- **Task Board always renders four columns** (`TaskBoardPage.tsx`) —
  v1.4.29 hid the entire board behind an `tasks.length === 0`
  early-return, breaking the Kanban design intent that empty columns
  themselves *are* the affordance. Grid is now
  `grid-cols-1 md:grid-cols-2 lg:grid-cols-4` with each column keeping
  its own drop-hint placeholder.

- **Agent-facing MCP caller validation** is now consistent across
  `tasks_create` / `tasks_claim` / `tasks_complete` / `tasks_block` /
  `activity_post`. Wildcard (`*`) and path-traversal-like values are
  rejected at the boundary with a clear error message.

- **Autopilot circuit breaker is now a proper 3-state FSM** (Closed /
  Open / HalfOpen). 10 fires in 60s trip to Open (60s cooldown),
  HalfOpen allows one probe; retry within 30s re-trips, quiet window
  returns to Closed. All transitions are logged to `autopilot_history`
  and the Activity Feed so operators can see rule loops get contained
  and recover. Replaces the v1.8.27-dev sliding-window rate limiter.

- **Autopilot broadcast channel** capacity raised from 1024 → 8192 and
  the `RecvError::Lagged` branch escalated from `warn!` → `error!`
  with a detached `append_activity` task (so logging the lag no longer
  amplifies event drops).

### Fixed
- **Autopilot rule storage silently accepted malformed JSON**, so
  broken rules would only surface their error when first fired (and
  only in `autopilot_history`, invisible during rule authoring). Now
  rejected at write time.

- **`action_run_skill` had no path guard** — a crafted rule with
  `skill_name: "../../../etc/passwd"` could have escaped the
  SKILLS directory. Defense in depth: alphanumeric allowlist on both
  `target_agent` and `skill_name`, plus `canonicalize()` containment
  check against `<home>/agents/<agent>/SKILLS/`.

- **`events.jsonl` rotation race lost in-flight events** — writers
  holding an `O_APPEND` fd at the moment of `rename()` would land
  writes on the orphaned `.jsonl.1`, which the tail task ignored.
  Made moot by the SQLite event bus swap.

- **`build_pending_tasks_section` silently returned `None` when
  TaskStore open failed**, hiding a broken task board from operators.
  Now logs a warning at `warn!` level while still degrading gracefully
  (the agent just loses its task queue for that turn).

### Security
- **`events.db` is owned exclusively by the gateway/MCP process
  writing it** — SQLite handles file permissions (`0600` under default
  umask). Event payloads containing task descriptions / metadata are
  no longer world-readable on multi-user systems.


## [1.8.26] - 2026-04-22

### Added
- **`shared_wiki_lint` MCP tool** — audits `~/.duduclaw/shared/wiki/`
  for Karpathy LLM Wiki schema compliance. Reports: pages missing
  any of the six required frontmatter fields (`title`, `created`,
  `updated`, `tags`, `layer`, `trust`), pages containing fallback-
  content markers (e.g. "基於訓練資料", "web_search failed",
  "無法取得", "查無結果", "based on training data" …) that were not
  explicitly tagged `fallback-mode`, plus the existing graph-level
  checks (orphans / broken links / stale pages) delegated to
  `WikiStore::lint()`. Unlike per-agent `wiki_lint`, this tool
  takes no `agent_id` — shared wiki is a single global namespace.

### Fixed
- **Shared wiki accepted pages authored from stale LLM priors,
  polluting the cross-agent knowledge base.** When
  `ai-papers-researcher` / `ai-repos-researcher` cron tasks ran
  while `web_search` was failing, they silently fell back to
  recalling training data and wrote reports whose frontmatter
  looked legitimate but whose body was unanchored to any verifiable
  source (7/7 Hugging Face model URLs returned HTTP 200 + `<title>
  404` body in one case). These entered `shared/wiki/` unchallenged
  and drifted there indefinitely. Project rule: 「有 fallback 的資
  料不應該混入共用 wiki 中產生雜訊」.

  **Fix A** — `handle_shared_wiki_write` now enforces two gates
  before the write:

  1. **Frontmatter schema gate** (`validate_wiki_frontmatter`):
     page must open with a `---…---` block declaring *all* of
     `title, created, updated, tags, layer, trust`. `trust` must
     parse as a float in `[0.0, 1.0]`. Missing or malformed
     frontmatter → hard reject with a message pointing at the
     missing fields.
  2. **Fallback-content gate** (`detect_fallback_content`): body
     scanned for any of 14 CJK / English fallback markers. On
     match, reject unless the page explicitly opts in with
     `fallback-mode` in its `tags` (for post-mortem archives
     where a human deliberately wants the record preserved; those
     pages are still expected to carry `trust: 0.2` or lower).

  Per-agent `wiki_write` is intentionally left permissive — private
  wikis can hold speculative or fallback material; only the shared
  bus is strict.

- **Four research-pipeline cron prompts pushed fabricated content
  into `shared/wiki/` when search tools failed.**
  `ai-papers-morning`, `ai-papers-evening`, `ai-repos-morning`, and
  `ai-repos-evening` (rows in `~/.duduclaw/cron_tasks.db`) have been
  rewritten to:

  - **Abort on search failure** instead of falling through to
    training-data recall. The new prompts open with a hard
    precondition: if `web_search` returns 0 results, immediately
    notify `agnes` that "本日研究暫停：搜尋工具失效" and exit the
    task. Explicit ban on the 無法取得 / 基於訓練資料 / 查無結果
    narrative patterns (which now trip the shared-wiki fallback
    gate anyway).
  - **Two-layer URL verification** before any wiki write: a HEAD
    fetch must return HTTP 200 *and* the body must not contain
    `<title>404` (the Hugging Face gotcha where bad model URLs
    return 200 with a 404 page body). Items failing either check
    are dropped — the prompts are explicit that filling with
    unverified items is prohibited.
  - **Atomic-entity page layout per Karpathy LLM Wiki**: one
    entity page per paper/repo under `entities/YYYY-MM-DD-<slug>.
    md`, plus a daily digest under `research/ai-papers/YYYY-MM-DD-
    (08|20).md` whose `related:` points back to every entity.
    Frontmatter is spelled out explicitly inline (all six required
    fields, `layer: context`, `trust: 0.5` default, `sources:`
    list), and heading decoration emoji are banned.

  Backup of the pre-rewrite rows saved to
  `~/.duduclaw/cron_tasks.db.v1.8.25.bak` in case rollback is
  needed.

- **Two fabricated shared-wiki pages from 2026-04-22** were
  removed: `research/ai-repos/2026-04-22-08.md` (web_search
  fallback, 0 real URLs) and `research/ai-repos/2026-04-22-20.md`
  (7/7 HF model URLs were 404-in-body). `_index.md` cleaned and
  `_log.md` appended with `delete … by:operator (fabricated: …)`
  entries. Both surviving `research/ai-papers/*.md` pages were
  retrofitted with the full nine-field Karpathy frontmatter
  (`title`, `created`, `updated`, `author`, `tags`, `related`,
  `sources`, `layer: context`, `trust: 0.5`) so they pass the new
  `shared_wiki_lint` tool.

### Tests

**12 new** (all passing, all in `mcp::wiki_schema_tests`):

- `frontmatter_validator_accepts_full_schema`
- `frontmatter_validator_rejects_missing_frontmatter`
- `frontmatter_validator_rejects_missing_required_fields`
- `frontmatter_validator_rejects_out_of_range_trust`
- `frontmatter_validator_rejects_non_numeric_trust`
- `detect_fallback_catches_cjk_marker`
- `detect_fallback_catches_english_marker`
- `detect_fallback_ignores_clean_body`
- `shared_wiki_write_rejects_fallback_content`
- `shared_wiki_write_rejects_missing_frontmatter`
- `shared_wiki_write_allows_fallback_mode_opt_in`
- `shared_wiki_write_accepts_clean_karpathy_page`

Full workspace lib suite still green.


## [1.8.25] - 2026-04-22

### Fixed
- **Cron tasks scheduled `0 8 * * *` expecting 8 am local fired 8 am
  UTC instead**. Creating a task via MCP `schedule_task` without
  specifying `cron_timezone` fell through to UTC evaluation — so a
  Taipei user got their "morning" cron at 16:00 local and their
  "evening" cron at 04:00 the *next morning*. New
  `detect_local_timezone()` helper reads the host's IANA name
  (`iana_time_zone::get_timezone()` on Unix / Windows) and
  round-trips it through `duduclaw_core::parse_timezone` to guarantee
  `chrono-tz` acceptance. `handle_schedule_task` now auto-populates
  `cron_timezone` from the detected TZ when absent; explicit
  `cron_timezone='UTC'` still forces UTC (opt-out), any explicit IANA
  name still wins. Logs the detected zone at info level for
  observability. `cron_timezone` tool schema description updated to
  reflect the new auto-detect default. New direct dep
  `iana-time-zone = "0.1"` on `duduclaw-cli` (already a transitive
  dep of `chrono`, no new vendored C). New test
  `detect_local_timezone_returns_valid_iana_name` asserts
  parse_timezone round-trip and tolerates None on hosts with no
  discoverable TZ (minimal Docker images).
- **Cron agents' nested `send_to_agent` replies silently dropped
  (same class as v1.8.16 but for cron-initiated chains)**. The cron
  scheduler dispatched tasks via `call_claude_for_agent_with_type`
  wrapped only in `DELEGATION_ENV.scope` — never in
  `REPLY_CHANNEL.scope`. So when a daily-report agent called
  `send_to_agent("agnes", "here's my report")`, no
  `delegation_callbacks` row was ever registered (MCP's
  `send_to_agent` only inserts callbacks when
  `DUDUCLAW_REPLY_CHANNEL` env is set). Agnes's response landed in
  `message_queue.response` and was then dropped at
  `forward_delegation_response`'s no-callback silent-return branch.
  Fix: `run_task` now wraps the dispatch future in
  `REPLY_CHANNEL.scope(cron_reply_channel_string(task), …)` when
  the task has a `notify_channel` target. New helper
  `cron_reply_channel_string` builds the
  `<channel_type>:<chat_id>[:<thread_id>]` grammar that
  `mcp.rs::send_to_agent` parses; Discord threads stored as
  `chat_id=<thread_id>, thread_id=NULL` emit `discord:<thread_id>`
  (matching `deliver_cron_result`'s existing API-level "thread is
  a channel" semantics). Effect: nested cron delegations now
  register callbacks → forward through v1.8.20 token cascade →
  session-append via v1.8.24 chain-root cascade. The cron agent's
  own top-level response still goes through `deliver_cron_result`
  (direct POST) unchanged; this patch strictly closes the nested
  path. 5 new tests in `cron_scheduler::tests` covering None /
  Discord thread-as-chat-id / Discord parent+thread / Telegram
  without thread / Telegram forum topic thread.



## [1.8.24] - 2026-04-22

### Fixed
- **Sub-agent replies disappeared from the root agent's session on
  nested delegations (chain-root session-append gap)**. v1.8.17 Fix 2
  wrote an XML-delimited `<subagent_reply agent="X">` turn into the
  parent agent's session, but only when the session owner matched
  `callback.agent_id` — a deliberate cross-agent-bleed guard. The
  unintended side effect: sub-agents spawned by the dispatcher (TL,
  eng-agent, eng-infra, marketing, …) don't have their own sessions in
  `sessions.db` — only agnes does. So when eng-agent replied to TL's
  `send_to_agent` call, the owner-mismatch skip fired:
  `callback.agent_id=duduclaw-tl` vs `session owner=agnes` → warn +
  silent drop → agnes's next turn had no record of the engineer's
  output → root agent couldn't synthesise the chain's total work.
  Fix: same cascade pattern as v1.8.20 token resolution.
  `append_subagent_reply_to_parent_session` now takes
  `chain_root_agent: Option<&str>` and accepts an owner match at
  either tier. Tier 1 (parent direct) uses the existing
  `<subagent_reply agent="X">` grammar. Tier 2 (chain root)
  writes `<subagent_reply agent="X" via="Y">` where Y is the
  callback agent — the `via=` attribute lets the root LLM tell a
  direct reply apart from one relayed via a sub-agent. Tier 3
  (neither match) still skips, so the cross-agent-bleed guard
  holds. `forward_delegation_response` already computed the
  chain root for v1.8.20's token cascade; just wires it down.
  `safe_agent_tag` helper factored out so direct and relayed
  content share the same `[A-Za-z0-9_-]` sanitisation. 4 new
  regression tests in `dispatcher::tests`
  (`append_cascades_to_chain_root_when_parent_has_no_session`,
  `cascade_appends_via_annotation`,
  `cascade_does_not_override_direct_parent_match`,
  `cascade_skipped_when_neither_parent_nor_root_owns_session`).
  Sub-agents still don't get their own persistent sessions —
  session-per-agent-per-chain remains a separate, larger design
  decision.



## [1.8.23] - 2026-04-22

### Added
- **Timezone-aware cron evaluation (#16 Level 2)**. Both the heartbeat
  scheduler and the per-task cron scheduler now honour a new
  `cron_timezone` field. Setting it to an IANA name
  (e.g. `"Asia/Taipei"`) lets the user write cron expressions in their
  wall clock and have the scheduler do the UTC conversion —
  `"0 9 * * *"` with `cron_timezone = "Asia/Taipei"` now actually fires
  at 09:00 Taipei every day. Empty / absent `cron_timezone` preserves
  the pre-v1.8.23 UTC behaviour, so nothing moves for existing
  deployments. The field lives on `HeartbeatConfig` (agent.toml
  `[heartbeat]`) and on `cron_tasks` DB rows (accepted by MCP
  `schedule_task` and dashboard `cron_add` / `cron_update`). A shared
  `duduclaw_core::should_fire_in_tz` makes both schedulers use
  identical evaluation semantics. Typos are caught at call time in the
  MCP tool and dashboard handlers (IANA validation via `chrono-tz`),
  so a bad zone name surfaces as an error instead of silently firing
  in UTC. If a bad name does reach the scheduler somehow, it logs a
  single warn line at load time and falls back to UTC — the cron
  keeps firing instead of going silent. DB migration is idempotent
  `ALTER TABLE`: reopening a v1.8.22 database adds the column with all
  existing rows inheriting `NULL` (= UTC). Documented in all 5
  `templates/*/agent.toml` and in the dashboard cron-input hint.
  18 new tests across `duduclaw-core` (8: Taipei, New York EDT, UTC
  fallback, invalid names, `*/5` tz-invariance, trimming), agent
  heartbeat (5: tz set / empty / invalid / disabled, next_fire UTC
  instant), and cron_store (5 including a `cron_timezone` roundtrip
  + `update_cron_timezone` clearing, and migration idempotency across
  reopen).


## [1.8.22] - 2026-04-21

### Fixed
- **Proactive check could not use the agent's MCP tools (#14)**.
  `heartbeat.rs`'s proactive spawn hard-coded
  `--print --no-input --system-prompt --max-turns 3` without
  `--mcp-config`. Two breakages stacked: Claude CLI ≥2.1 removed
  `--no-input` (so the spawn hard-errored on the current CLI), and
  the missing `--mcp-config` meant any PROACTIVE.md that said "query
  Notion for open tasks" silently no-opped — the sub-agent could not
  see the tool. Rewritten to mirror `spawn_claude_cli_with_env`:
  system prompt via `--system-prompt-file` (no `/proc/PID/cmdline`
  exposure), auto-attach `<agent_dir>/.mcp.json` with
  `--strict-mcp-config` when present, and `--max-turns` now reads
  from a new `ProactiveConfig.max_turns` field (default 8, clamped
  1–64) so checks that chain multiple tool calls have headroom.
- **Cron task results never reached the chat channel (#15)**.
  `cron_scheduler::execute_cron_task` only called `record_run` +
  hallucination audit; the response text lived in the DB only, and
  any prompt asking the agent to "send to Discord via send_message"
  silently failed because `call_claude_for_agent_with_type` does not
  attach MCP. Users were wrapping cron jobs in external shell scripts
  that called Discord/Notion APIs directly. Fix adds row-level
  routing: three new columns on `cron_tasks`
  (`notify_channel` / `notify_chat_id` / `notify_thread_id`, all
  `TEXT NULL`, idempotent `ALTER TABLE` migration that tolerates
  "duplicate column name" so reopening a v1.8.21 DB is safe). New
  `deliver_cron_result` resolves the bot token through the same
  cascade the dispatcher uses (per-agent `agent.toml [channels.<ch>]`
  encrypted or plaintext → global `config.toml [channels]`), clamps
  the response to 3500 chars (Discord's 2000-char cap is the tightest;
  CJK-safe codepoint count), prefixes with a task-name header, and
  calls the unified `ChannelSender`. Discord thread routing uses
  `notify_thread_id` as the effective chat_id. Delivery failures log
  but never flip `record_run` — the agent did its work, only the
  postage failed. `CronTaskRow::has_notify_target()` gates delivery
  so legacy rows without notify columns stay completely silent. MCP
  `schedule_task` and dashboard `cron_add` / `cron_update` both
  accept the three new optional params with symmetric validation
  ("both or neither" for channel + chat_id). Two new tests cover
  round-trip + `update_notify` clearing, and the reopen-the-DB
  migration idempotency contract.

### Documented
- **`[heartbeat] cron` is UTC — was not documented (#16 Level 1)**.
  `heartbeat.rs:251` and `cron_scheduler.rs:151` both call
  `chrono::Utc::now()`, and `ProactiveConfig.timezone` only affects
  `quiet_hours_*` — not the cron evaluation. Taipei (UTC+8) users
  writing `"0 9 * * *"` expecting 09:00 local actually got 17:00.
  Added comments to all 5 `templates/*/agent.toml` heartbeat blocks
  with the Asia/Taipei mapping (`"0 1 * * *"` → local 09:00),
  expanded the `HeartbeatConfig` doc-comment, clarified on
  `ProactiveConfig.timezone` that it is quiet-hours-only, added the
  same UTC caveat to the MCP `schedule_task` tool description and
  the dashboard `SettingsPage` cron-input hint. Timezone-aware cron
  evaluation (Level 2 — reading `cron_timezone` on the task row) is
  planned for a later release; this change is documentation-only so
  no behaviour change for existing crons.


## [1.8.21] - 2026-04-21

### Added
- **`duduclaw reforward <message_id> [--dry-run]`** — manual unstuck
  lever for completed delegations whose forward failed and got retry-
  queued. Before v1.8.20, nested sub-agent forwards to Discord threads
  hit 401 Unauthorized because token lookup didn't cascade to the
  chain-root agent; v1.8.20 fixes that going forward, but
  already-completed messages were stuck — the dispatcher only retries
  when a new `agent_response` arrives for the same message_id, which
  never happens for a message that's already `done`. The callback row
  ages out to 24h cleanup and the user loses the reply. New command:
  reads `message_queue.db` by id (requires `status='done'` and
  non-empty response), uses the existing `delegation_callbacks` row
  if present, synthesizes one from the stored `reply_channel` column
  if missing (`INSERT OR REPLACE` for idempotency across re-runs),
  then delegates to `forward_delegation_response` which uses the
  v1.8.20 token cascade and v1.8.17 Fix 2 session append. Reports
  `Sent` / `DryRun` / `Failed` with friendly output; exit 1 on error.
  New `pub async fn reforward_message` + `pub enum ReforwardOutcome`
  in `duduclaw_gateway::dispatcher` for library reuse. 9 new
  regression tests covering dry-run paths, error cases
  (pending / missing / empty response / no channel context), and the
  `parse_reply_channel` grammar incl. the `discord:thread:<id>`
  collapse rule. Production-verified: recovered message
  `78fbcfc8-735b-4053-9ee0-a03543fd904f` (a marketing report that had
  been stuck since 12:35 UTC) delivered to its Discord thread.



## [1.8.20] - 2026-04-21

### Fixed
- **Nested sub-agent forwards to Discord threads got 401
  Unauthorized when only the chain-root agent had a per-agent bot
  token**. Production-observed on v1.8.19 (message
  `78fbcfc8-735b-4053-9ee0-a03543fd904f`, TL→marketing depth=2 — the
  marketing agent finished the report, response text in DB, but the
  HTTP POST to Discord thread `1496095418805780591` returned 401).
  `forward_to_channel`'s token lookup cascaded from `callback.agent_id`
  (the `send_to_agent` caller, e.g. `duduclaw-tl` — no per-agent bot
  configured) straight to the global `config.toml` token, skipping
  the chain-root agent (agnes) who actually owned the bot that
  opened the thread. Discord threads are scoped to the bot that
  opened them (v1.8.14 already documented this), so the 401 loop
  was inevitable for any nested delegation whose immediate caller
  lacked its own bot. New `resolve_forward_token` helper cascades
  three tiers: (1) callback agent's own token → (2) chain-root
  agent's token (looked up from `message_queue.origin_agent` via
  new `lookup_origin_agent`) → (3) global config token. The four
  channel arms (telegram/line/discord/slack) in `forward_to_channel`
  all route through the helper so the cascade applies uniformly,
  though only Discord's thread-bot scoping actually triggered the
  production failure. Handles the `origin_agent == callback_agent`
  self-loop, missing `message_queue.db`, and NULL
  `origin_agent` column cleanly. 7 new regression tests in
  `dispatcher::tests`, including
  `resolve_token_cascades_to_chain_root_when_callback_agent_has_none`
  that replays the production scenario.



## [1.8.19] - 2026-04-21

### Fixed
- **`Failed to initialize inference engine: Backend unavailable:
  llama.cpp` WARN flood**. When an agent's `[model.local]` had
  `use_router = true` but the gateway binary was built without
  `--features metal`/`cuda`/`vulkan` (the default for the
  npm-distributed binary to avoid pulling libclang + cmake into the
  release build), every single request ran the local-offload path,
  hit `InferenceEngine::init`, got `BackendUnavailable`, warned, fell
  back to SDK, and repeated next request. Functionally harmless — the
  fallback always worked — but drowned real warnings and wasted
  ~100ms per request on a doomed init attempt. Added a process-
  lifetime `AtomicBool` negative cache next to the existing
  `INFERENCE_ENGINE` singleton in `claude_runner.rs`: on the first
  failed `init` (or first successful init that still reports no
  available backend), the flag latches to `true` and every subsequent
  `get_inference_engine` short-circuits to `None` silently. The WARN
  is now one-shot per gateway process, with an actionable hint on how
  to enable a backend (rebuild with `--features metal/cuda/vulkan`, or
  configure `[openai_compat]` in `inference.toml` for a remote
  backend). A gateway restart resets the cache — which is also when
  operators would have rebuilt the binary, so the trade-off aligns.



## [1.8.18] - 2026-04-21

### Fixed
- **Dual-rail dispatch race silently defeated v1.8.16's reply_channel
  propagation**. Production-observed on a live v1.8.17 chain (agnes →
  TL → [eng-agent + eng-infra]): TL's outgoing delegations to the two
  eng-agents had `reply_channel=NULL` in `message_queue.db` even
  though `DUDUCLAW_REPLY_CHANNEL` was scoped correctly in the
  dispatcher. Effect: when eng-agent replied, no callback was
  registered, the forward lookup silently skipped, and the engineer's
  output never reached TL's session. `DUDUCLAW_DELEGATION_DEPTH`
  still propagated correctly in the same chain — the "half-propagated"
  pattern (correct depth + NULL reply_channel) was the telltale.
  Root cause: `mcp.rs::send_to_agent` was dual-writing every
  delegation to both `bus_queue.jsonl` (legacy) and
  `message_queue.db` (SQLite, authoritative since v1.8.1).
  The gateway's dispatcher polled both every 5 seconds:
  `poll_and_dispatch` (legacy) `tokio::spawn`'s a per-message
  dispatch task, which drops task-local `REPLY_CHANNEL` at the
  spawn boundary; `poll_and_dispatch_sqlite` (v1.8.16) scopes
  `REPLY_CHANNEL` correctly. Whichever side reached
  `prepare_claude_cmd` first determined whether
  `DUDUCLAW_REPLY_CHANNEL` was set on the target's Claude CLI
  subprocess. `DELEGATION_ENV.scope` nested INSIDE `dispatch_to_agent`
  applies to both paths equally, explaining why depth propagated but
  reply_channel didn't. Fix: removed the `bus_queue.jsonl` write from
  `send_to_agent`. SQLite has been the authoritative rail since
  v1.8.1 — the jsonl write was dead weight kept around for migration
  safety and, by causing the race, actively defeating the v1.8.16
  fix. `queued` flag now derives from the SQLite INSERT rowcount
  (v1.8.16 schema-downgrade fallback preserved). `poll_and_dispatch`
  (legacy) is left untouched; it still handles `task_created`
  signals and orphan-response recovery, both of which use separate
  writers not affected by this change. New
  `mcp::tests::send_to_agent_never_writes_bus_queue_jsonl`
  regression guard. Two existing E2E tests
  (`e2e_send_to_agent_increments_depth`,
  `e2e_depth_zero_defaults_origin_to_caller`) migrated from reading
  `bus_queue.jsonl` to `message_queue.db`.



## [1.8.17] - 2026-04-21

### Fixed
- **MCP server used the global `default_agent` as caller identity,
  silently breaking supervisor-relation authorization for every
  sub-agent**. `mcp.rs::get_default_agent` read `config.toml [general]
  default_agent` (typically the top-level `agnes`) regardless of which
  agent's Claude CLI actually spawned the MCP subprocess. When
  `duduclaw-tl` called `send_to_agent("duduclaw-eng-agent", …)`, the
  supervisor check asked "is agnes the parent of duduclaw-eng-agent?",
  saw `reports_to=duduclaw-tl`, and rejected the call as a pattern
  violation — even though the delegation was correct. The TL agent's
  own Discord message diagnosed this accurately ("MCP Server 在驗證
  委派權限時，仍以發起 Session 的身份（agnes）作為呼叫者") and
  proposed `方案 B: 由我代替產出` as a workaround — improvising around
  the bug instead of the system enforcing the correct chain. New
  `duduclaw_core::ENV_AGENT_ID = "DUDUCLAW_AGENT_ID"`;
  `mcp.rs::get_default_agent` preference order is now env var → config
  `default_agent` → `"dudu"`. `duduclaw-agent::mcp_template::
  ensure_duduclaw_absolute_path` (called from `server.rs:344` on
  gateway startup) injects `{ "DUDUCLAW_AGENT_ID": "<agent-dir-name>" }`
  into each agent's `.mcp.json` `env` block — preserving other env
  vars, preserving other `mcpServers` entries (playwright,
  browserbase), handling legacy `duduclaw-pro` key, idempotent on
  repeated calls. Empty string falls through to config to avoid
  lockout on botched migrations. After this: `agnes → duduclaw-tl`
  still allowed, `duduclaw-tl → duduclaw-eng-agent` now allowed,
  `agnes → duduclaw-eng-agent` correctly rejected.
- **Sub-agent replies never reached the parent agent's session,
  breaking conversation continuity across delegations**.
  `forward_delegation_response` delivered a sub-agent's reply to the
  originating channel (Discord/Telegram/LINE/Slack) and stopped.
  Parent agents had no record in their SQLite session of what the
  sub-agent said, so the next user turn replying to the parent
  referenced content the parent couldn't see. Production-observed
  symptom (Discord 2026-04-21 07:24): TL replied with "方案 A/B/C",
  user said "@Agnes 方案A", Agnes's next invocation had no trace of
  A/B/C and asked the user to disambiguate between Fabric / Besu /
  PoA (from an earlier unrelated branch). Fix: after
  `forward_to_channel(...)` returns `Ok(())`,
  `forward_delegation_response` appends a single assistant-role turn
  to the parent's session with XML-delimited content
  `<subagent_reply agent="X">...</subagent_reply>` (same grammar as
  `channel_reply::format_history_as_prompt`). Agent name sanitised
  to `[A-Za-z0-9_-]`. Token count uses the CJK-aware estimator.
  `sessions.total_tokens` + `last_active` updated in the same
  transaction. New `candidate_session_ids` tries both
  `discord:thread:<id>` and `discord:<id>` forms (the `thread:`
  marker was collapsed in `mcp.rs::send_to_agent` callback insert)
  and matches by `owner_agent` to prevent cross-agent bleed on
  shared channels. Session store errors are swallowed at warn level —
  Discord delivery already succeeded, dropping the session append is
  strictly better than losing the forward. Append happens only on
  HTTP success, so retry loops don't double-append.



## [1.8.16] - 2026-04-21

### Fixed
- **Nested sub-agent replies silently dropped at delegation depth ≥ 2**.
  A user-visible chain like `agnes → duduclaw-tl → [eng-agent +
  eng-infra] → synthesis` would deliver the first-level "dispatch
  confirmation" (depth=1, from `channel_reply`), complete all three
  sub-agent messages in `message_queue.db` with status=`done`, but
  never forward the status update (depth=2) nor the 16 KB final
  synthesis (depth=3) to the originating Discord channel — no WARN,
  no error, just silence. Root cause: MCP `send_to_agent` only
  registers a `delegation_callbacks` row when `DUDUCLAW_REPLY_CHANNEL`
  is set in env, which `channel_reply::REPLY_CHANNEL.scope()` does for
  inbound channel messages but `dispatcher::dispatch_to_agent` did
  NOT, so nested sub-agent processes had no channel context, their
  callback rows were never inserted, and `forward_delegation_response`
  took its no-callback silent-return branch. Fix propagates channel
  context through the chain: (1) `message_queue` gains a
  `reply_channel TEXT` column with idempotent `PRAGMA table_info` +
  `ALTER TABLE ADD COLUMN` migration; (2) MCP `send_to_agent` captures
  `DUDUCLAW_REPLY_CHANNEL` from env on INSERT, with a schema-downgrade
  fallback for the cross-process race on first v1.8.16 boot; (3)
  `dispatcher::dispatch_to_agent` now wraps the dispatch future in
  `claude_runner::REPLY_CHANNEL.scope(msg.reply_channel, ...)` when
  the row carries channel context, so the spawned Claude CLI
  subprocess inherits the env var and its own nested `send_to_agent`
  calls register callbacks correctly. Chain propagation is automatic:
  depth-1's row stores discord:..., depth-2 inherits via env during
  dispatch and writes it back to its own row, depth-3 does the same.
- **`forward_delegation_response` no-callback path was fully silent**,
  making the above bug invisible in logs. Added
  `tracing::debug!` so future drops surface under
  `RUST_LOG=duduclaw_gateway::dispatcher=debug` with the message-id +
  responder agent. Still expected-and-benign for cron / reminder /
  non-channel delegations; unexpected for user-facing sub-agent
  replies.



## [1.8.15] - 2026-04-21

### Fixed
- **Discord global `[discord]` 401 noise at gateway startup**. The
  global `config.toml [channels] discord_bot_token_enc` was eagerly
  validated on startup via `GET /users/@me`, printing a warn-level
  "token invalid (HTTP 401)" even when per-agent Discord tokens (the
  authoritative source since v1.8.14) were live and serving traffic.
  Users who migrated to per-agent tokens saw a scary warning that
  implied Discord was broken when it wasn't. `start_discord_bots` now
  collects per-agent tokens first and passes a `quiet_on_auth_failure`
  flag to `spawn_discord_bot`; a 401/403 on the global token when at
  least one per-agent token exists is logged at info level with an
  explicit note. A 401 with no per-agent fallback still warns.
- **GVU proposals on tiny SOUL.md baselines were always rejected as
  CRITICAL drift**. With a ~400-char baseline (e.g. the default agnes
  template), every evolution `append` made `compute_asi`'s 0.40-
  weighted char-bigram content similarity collapse to ~0.06 and trip
  the 0.50 critical threshold deterministically. Not a drift problem —
  a baseline-size problem. Added
  `duduclaw_security::stability_index::AsiConfig::bootstrap()`
  (content 0.40 → 0.20, semantic 0.30 → 0.45, critical 0.50 → 0.25)
  and `AsiConfig::for_baseline_size(bytes)` which dispatches to
  bootstrap when `bytes < 1024`, default otherwise. The updater now
  calls `for_baseline_size(current_content.len())` so agents with
  richer SOUL.md files still face the strict default threshold.
- **Claude CLI `--resume` was permanently unreachable — wasting 1
  extra CLI spawn per multi-turn conversation**. v1.8.1 introduced
  native multi-turn via `--resume <dd-{hex16}>` with a SHA-256 session
  ID. Claude CLI strictly requires either a canonical UUID or an
  exact session title match — `dd-5d8a35f9dba3408e` is neither, so
  the first `--resume` attempt was rejected 100% of the time before
  the `is_session_error`-guarded fallback retried with history-in-
  prompt (the only path that actually worked). Every multi-turn
  reply paid one wasted CLI spawn + startup latency + warn-level log
  line. `call_claude_cli_rotated` no longer attempts `--resume`:
  when conversation history exists, it is folded into the prompt via
  `format_history_as_prompt` and Claude CLI is spawned once. The
  `session_id` parameter is kept as `_session_id` for call-site
  compatibility. Removed dead `make_claude_session_id` and
  `is_session_error` helpers plus their 3 tests.



## [1.8.14] - 2026-04-21

### Fixed
- **Discord thread session id drifted across turns**. `auto_thread &&
  !is_thread` in the session-id formatter was only true on the first
  turn (when a thread was about to be created) — every follow-up turn
  the user typed inside the thread flipped `is_thread` to true and the
  session id silently switched from `discord:thread:{id}` to
  `discord:{id}`, loading a fresh empty session and losing all context.
  Condition is now `is_thread || created_thread` so a thread-scoped
  conversation keeps one session id for its entire lifetime. Also
  handles the edge case where `create_thread()` fails (returns
  `discord:{channel_id}` instead of a misleading `discord:thread:...`).
- **Sub-agent replies stuck in bus_queue.jsonl**. Three layered bugs
  prevented `send_to_agent` → sub-agent → user round-trips from ever
  completing:
  1. The `delegation_callbacks` parser split `<channel>:thread:<id>`
     by `:` and stored the literal string "thread" as `channel_id`;
     downstream `validate_channel_id` rejected it as non-numeric, so
     forwarding retry-looped forever. Parser now recognises the
     `<type>:thread:<id>` marker and stores `channel_id=<id>,
     thread_id=None`.
  2. `forward_to_channel` only ran immediately after a live dispatch;
     orphan `agent_response` entries left on disk after a crash /
     Ctrl+C / hotswap were never replayed. New
     `reconcile_orphan_responses` scans `bus_queue.jsonl` on
     dispatcher startup and atomically replays every callback whose
     row is still pending.
  3. Discord / Telegram / LINE / Slack arms read the global
     `[channels] <type>_bot_token` from config.toml. Discord threads
     are scoped to the bot that opened them — a different bot returns
     401 Unauthorized even in the same guild. New
     `get_agent_channel_token` reads the originating agent's per-agent
     token from `agents/<id>/agent.toml [channels.<type>] bot_token_enc`
     first, falling back to the global token only when the agent has
     none.
- **Long sub-agent replies silently truncated**. `forward_to_channel`
  capped responses at the channel byte limit and appended
  `_(回應過長，已截斷)_`, dropping most TL/PM report content. Rewritten
  to use the existing `channel_format::split_text` (paragraph/line
  aligned, UTF-8 safe) emitting chunks labelled
  `📨 **agent** 的回報 (1/N)` / `(續 2/N)`, each sized under the
  channel's byte budget (Discord 1900, Telegram 4000, LINE 4900, Slack
  3900) with a 250ms inter-chunk gap to stay within API rate limits.

### Changed
- **Default log level is now `warn`** when `RUST_LOG` is unset.
  Previous default (`EnvFilter::from_default_env()` with no fallback)
  dropped every log unless the user explicitly set `RUST_LOG`, which
  made issues like "401 on delegation forward" undiagnosable from the
  terminal and left `~/.duduclaw/logs/gateway.log` at 0 bytes. `warn`
  keeps the terminal quiet for end users while still surfacing real
  problems; run `RUST_LOG=info duduclaw run` for the verbose
  dispatcher / WebSocket / heartbeat trace when debugging.



## [1.8.13] - 2026-04-20

### Added
- **Memory page Key Insights tab**. The agent-local `memory.db` →
  `memories` table is populated by the prediction engine with
  satisfaction-error deltas ("Prediction deviation: expected 0.70,
  inferred 0.42 ..."), not conversational content — so the previous
  Memory tab looked empty / unhelpful on a running system. The real
  extracted insights live in the `key_facts` table (P2 Key-Fact
  Accumulator), which had zero dashboard exposure. New RPC
  `memory.key_facts(agent_id, limit)` queries that table directly
  and the Memory page now has a 4th tab "關鍵洞察 / Key Insights /
  主要インサイト" rendering each fact as a card with `access_count`
  badge, timestamp, and collapsible source metadata.
- **Unified multi-source audit log on the Logs page**. Previously
  `security.audit_log` read only `security_audit.jsonl` (rarely
  written), so the history panel showed "暫無審計事件" on systems
  with dozens of real tool calls. New RPC `audit.unified_log(params)`
  merges four JSONL sources (`security_audit.jsonl`,
  `tool_calls.jsonl`, `channel_failures.jsonl`, `feedback.jsonl`)
  into a common envelope — `timestamp` / `source` / `event_type` /
  `agent_id` / `severity` / `summary` / `details` — sorted
  newest-first, with per-source counts returned alongside. Severity
  rules: tool_call success=info, failure=warning,
  channel_failure=warning, feedback=info, security preserves its
  original severity. Missing files and malformed JSONL lines are
  tolerated silently. Summary truncation goes through
  `duduclaw_core::truncate_bytes` (CJK-safe).
- **Logs page history tab rewrite**. Source filter chips
  (全部 / 安全 / 工具呼叫 / 通道失敗 / 回饋) with live per-source
  counts, severity dropdown, severity-colored left borders
  (emerald / amber / rose), click-to-expand pretty-printed detail
  JSON. Realtime tab untouched. `handle_security_audit_log` is
  preserved intact for backward compatibility.



## [1.8.12] - 2026-04-20

### Fixed
- **Opaque `claude CLI stream error: Unknown stream-json error`** now
  carries the captured Claude CLI stderr tail (`| stderr: ...`, 500
  bytes max). When Claude CLI emits `is_error: true` on a `result`
  event with no `result` string, the caller previously got no
  actionable detail; now the real reason (stale `--resume` handle,
  internal CLI error, etc.) is surfaced in both the debug log and
  the rotator's error history.
- **Auto-fallback on generic `--resume` failures**. `is_session_error`
  now also matches "unknown stream-json error", so when Claude CLI
  can't spell out why `--resume` failed the caller retries once with
  the session history folded into the prompt. Worst case one extra
  turn of cost; best case the user gets a reply instead of an opaque
  error.
- **`schedule_task` MCP tool schema was missing `agent_id` and `name`**.
  The handler reads both (plus `task` / `prompt` / `description` as
  synonyms) but the declared `ParamDef` list exposed only `cron` and
  `description`. From the agent's point of view the tool looked half-
  built, so Agnes fell back to Claude Code's session-bound
  `/schedule` slash command (7-day auto-expiry) instead of DuDuClaw's
  persistent `CronScheduler`. Schema now lists `cron`, `task`, `name`
  (all required), and `agent_id` (optional, strongly recommended),
  and the description explicitly states the tool is persistent
  (`~/.duduclaw/cron_tasks.db`), survives restarts, and should be
  preferred over `/schedule`.



## [1.8.11] - 2026-04-20

### Fixed
- **Claude CLI `--bare` broke OAuth authentication** (Claude CLI
  2.1.110 regression). The flag was added to
  `spawn_claude_cli_with_env` for ~15-25% latency reduction by
  skipping hooks / LSP / plugin sync / CLAUDE.md auto-discovery, but
  also disabled OS-keychain credential lookup, causing every channel
  subprocess call to fail with "Not logged in · Please run /login"
  even when `claude auth status` confirmed a valid session. Removed
  from both `call_claude_cli_rotated` and `call_claude_cli_lightweight`
  paths.
- **CJK / emoji byte-index string slicing panicked tokio workers**.
  `s[..s.len().min(N)]` slices by byte, not by char, so any multi-byte
  codepoint straddling byte N (e.g. `學` = 3 bytes) triggered "byte
  index N is not a char boundary" panics that crashed reply dispatch
  silently. The pattern was copy-pasted across 31 sites in 16 files
  (Feishu, WhatsApp, LINE, Slack, Telegram, Discord, TTS, direct_api,
  handlers, dispatcher, tool_classifier, gvu/loop_, cli/mcp,
  cli/acp/handlers, runtime/openai_compat, computer_use, webchat,
  channel_reply).

### Added
- **`duduclaw_core::truncate_bytes` / `truncate_chars`** (new
  `duduclaw-core/src/text_utils.rs` module). `truncate_bytes` returns
  a `&str` sliced at the nearest UTF-8 char boundary ≤ the requested
  byte budget — a panic-safe drop-in for `&s[..N]`. `truncate_chars`
  counts codepoints. Six unit tests cover ASCII, mid-CJK, zero-budget,
  and emoji (4-byte) cases. Every unsafe byte-index slice on a
  user-text / LLM-text / HTTP-body string was migrated.



## [1.8.10] - 2026-04-20

### Added
- **`marketplace.list` RPC** serving the real built-in MCP catalog
  (Playwright, Browserbase, Filesystem, GitHub, Slack, Postgres,
  SQLite, Memory, Fetch, Brave Search) enriched with `author`,
  `tags`, and `featured` fields. Merges optional user entries from
  `~/.duduclaw/marketplace.json` without a rebuild.
- **Partner data model**: new SQLite-backed `PartnerStore`
  (`~/.duduclaw/partner.db`) with profile + customer CRUD and
  computed sales stats. Seven RPCs (`partner.profile`, `partner.stats`,
  `partner.customers`, `partner.profile.update`,
  `partner.customer.add`, `partner.customer.update`,
  `partner.customer.delete`) and 4 unit tests.
- **Toast notification system** (`web/src/components/Toast.tsx` +
  `web/src/lib/toast.ts`): module-scoped event bus, max-5 queue,
  auto-dismiss, warm stone/amber/emerald/rose variants,
  `prefers-reduced-motion` honored.
- **`cron.resume`** wired to a Resume button alongside Pause in the
  Settings cron task list.
- **SOUL.md evolution history UI** in Memory → Evolution tab with
  pre/post metric deltas (positive feedback, prediction error, user
  corrections) and status badges (Confirmed / RolledBack / Observing).

### Changed
- **`evolution.status`** returns real aggregate data
  (`enabled`/`mode`/`total_agents`/`gvu_enabled_count`/
  `total_versions`/`last_applied_at`) instead of hardcoded
  `{enabled: true, mode: "prediction_driven"}`.
- **`activity.subscribe`** returns honest metadata
  (`broadcast_mode: "all_events"` + note) — previously a bare stub.
  Per-topic filtering is not implemented; all authenticated WS
  clients receive all activity events.
- **ChannelsPage setup guides**: 42 hardcoded zh-TW strings extracted
  to i18n across Telegram / LINE / Discord / Slack / WhatsApp /
  Feishu in zh-TW / en / ja-JP.
- **MarketplacePage** loads from the real RPC; fake stars/prices and
  the 8-item `MOCK_SERVERS` constant removed. Category-based icon
  mapping (browser / data / communication).
- **PartnerPortalPage** rewired to real RPCs; mock constants
  (`PARTNER_STATUS`, `SALES_STATS`, `MOCK_CUSTOMERS`) and the
  preview banner removed. Added onboarding card (empty-profile
  state) and Add Customer modal.
- Inline error feedback added to MarketplacePage install,
  PartnerPortalPage license generation, and ApprovalModal WS
  response failures (previously all silently swallowed).

### Removed
- **`activity.unsubscribe`** RPC (backend dispatch arm and frontend
  method) — broadcasts cannot be stopped without closing the WS
  itself, so the RPC was dead.
- **`evolution.skills`** handler — fully redundant with
  `skills.list`, which returns richer per-agent + global structure.

### Fixed
- 23 silent `console.warn("[api]", e)` catches across DashboardPage,
  ReportPage, BillingPage, SkillMarketPage, SettingsPage, MemoryPage,
  AgentsPage, ChannelsPage, and KnowledgeHubPage now surface errors
  to users via toast while preserving devtools visibility.



## [1.8.9] - 2026-04-20

### Added
- **Wiki knowledge layer system** (Vault-for-LLM inspired): 4-layer
  architecture (L0 Identity / L1 Core / L2 Context / L3 Deep) with
  `layer` and `trust` (0.0-1.0) frontmatter fields. Search results
  ranked by trust-weighted score. Backward-compatible defaults for
  existing pages.
- **Wiki system prompt injection**: `build_system_prompt()` now
  auto-injects L0+L1 wiki pages into the WIKI_CONTEXT module.
  Agents automatically reference their accumulated knowledge without
  manual `wiki_search` calls.
- **FTS5 full-text index**: `WikiFts` SQLite-backed index with
  `unicode61` tokenizer for CJK support. Auto-syncs on every
  `write_page` / `delete_page`. Manual rebuild via `wiki_rebuild_fts`
  MCP tool.
- **Wiki dedup detection**: `wiki_dedup` MCP tool detects duplicate
  pages by title match and tag Jaccard similarity (>= 0.8).
- **Wiki knowledge graph**: `wiki_graph` MCP tool exports Mermaid
  diagrams with BFS-limited center+depth focused view. Node shapes
  vary by knowledge layer.
- **Wiki search filters**: `wiki_search` / `shared_wiki_search` now
  support `min_trust`, `layer`, and `expand` (1-hop related/backlink
  expansion) parameters.
- **Reverse backlink index**: `build_backlink_index()` scans
  `related` frontmatter + body markdown links for bidirectional
  mapping.
- **Layer-aware context injection**: `build_injection_context()` +
  `collect_by_layer()` for system prompt budget-aware injection.
- **CLAUDE_WIKI.md template**: Now included in agent CLAUDE.md on
  creation, providing wiki MCP tool usage guide to Claude Code.
- **A2A stdio JSON-RPC server** (`acp::server::run_acp_server`):
  `duduclaw acp-server` is now functional (previously a stub). Runs a
  line-delimited JSON-RPC 2.0 loop on stdin/stdout with
  `agent/discover`, `tasks/send`, `tasks/get`, `tasks/cancel`
  methods, backed by the `A2ATaskManager`. Enables Zed / JetBrains /
  Neovim IDE integration via the Agent Client Protocol.
- **Behavioral contract injection**: `AgentRegistry` now loads
  `CONTRACT.toml` into `LoadedAgent.contract`. `must_not` /
  `must_always` rules are rendered as a CONTRACT module in the
  system prompt, giving every runtime (Claude / Codex / Gemini)
  consistent behavioral boundaries.
- **Memory decay daily scheduler**: Gateway spawns a background
  task that runs `duduclaw_memory::decay::run_decay` every 24h,
  archiving low-importance entries older than 30 days and
  permanently deleting archived entries older than 90 days.
- **Dashboard WebSocket heartbeat**: Server sends a WebSocket
  `Ping` every 30s and closes idle sockets after 60s without a
  `Pong`. Client sends an application-level `ping` RPC every 25s
  (browsers can't issue control frames). New `ping` method on the
  gateway method handler returns `{pong:true}`.
- **`/metrics` Prometheus endpoint**: New `duduclaw_gateway::metrics`
  module exposed as `GET /metrics` on the gateway HTTP server for
  scraping runtime metrics.
- **RL trajectory collector + CLI**: New
  `duduclaw_gateway::rl::collector` module writes per-agent
  trajectories to `~/.duduclaw/rl_trajectories.jsonl` during
  channel interactions. `duduclaw rl export|stats|reward` is now
  functional (previously stub), including composite reward
  computation (outcome × 0.7 + efficiency × 0.2 + overlong × 0.1).
- **Cognitive memory MCP tools**: `memory_search_by_layer`
  (episodic/semantic filter), `memory_successful_conversations`
  (high-importance episodic recall by topic),
  `memory_episodic_pressure` (observation-density score for
  scheduling Meso reflections), `memory_consolidation_status`
  (count of un-consolidated high-importance episodes).
- **Streaming ASR providers**: `AsrRouter` now accepts
  `Box<dyn StreamingAsrProvider>` (e.g. Deepgram WebSocket) via
  `add_streaming_provider` / `streaming_provider()` for real-time
  transcription alongside existing batch providers.
- **Compression strategy selector**: `compress_text` MCP tool gains
  a `strategy` param — `meta_token` (lossless), `llmlingua` (lossy
  2-5×), `streaming_llm` (window management), or `auto`.
- **Marketplace + Partner Portal dashboard pages**: Wired into
  router and sidebar (manager+ gate for Partner Portal). New
  Browser Automation tab under Settings with ToolApproval,
  SessionReplay, and BrowserAudit panels. `ApprovalModal` mounted
  at app root for synchronous tool approval prompts.

### Changed
- **Cloud ingest prompt**: Now instructs Claude to include `layer`
  and `trust` in extracted wiki page frontmatter.
- **Auto-ingest defaults**: Source pages default to `layer: context,
  trust: 0.4`; entity pages to `layer: deep, trust: 0.3`.
- **Backlink logging**: `write_page()` logs info-level suggestions
  when referenced pages lack reciprocal backlinks.
- **`wiki_search` / `shared_wiki_search` response**: Hits now
  include `weighted_score`, `trust`, and `layer` fields alongside
  the existing `score`.
- **`duduclaw-agent` crate**: Now depends on `duduclaw-memory` to
  build the WIKI_CONTEXT injection module at prompt assembly time.

### Fixed
- **Wiki-to-LLM disconnect (all runtimes)**: Wiki system previously
  accumulated knowledge via channel ingest and GVU evolution but
  never fed it back into LLM system prompts. Now L0+L1 pages are
  auto-injected into ALL three system prompt assembly paths:
  - CLI interactive (`runner.rs` — `WIKI_CONTEXT` module)
  - Channel reply (`channel_reply.rs` — `## Wiki Knowledge` section,
    serves Telegram/LINE/Discord → Claude/Codex/Gemini/OpenAI)
  - Dispatcher/Cron (`claude_runner.rs` — `# Wiki Knowledge` section,
    serves agent-to-agent delegation and scheduled tasks)
- **FTS desync**: FTS index was completely disconnected from write
  operations. Now auto-syncs on every page write/delete.
- **CLAUDE_WIKI template unused**: Template existed but was never
  included in agent CLAUDE.md files.
- **`duduclaw rl` / `duduclaw acp-server` stubs**: Both commands
  previously printed a placeholder and returned; they now execute
  the real collector / JSON-RPC server.


## [1.8.8] - 2026-04-20

### Fixed
- **Lightweight CLI effort level**: Changed from `--effort low` to
  `--effort medium` for instruction/fact extraction tasks. Prevents
  quality degradation in extracted pinned instructions and key facts
  while maintaining cost savings from other lightweight flags.



## [1.8.7] - 2026-04-19

### Added
- **Claude CLI lightweight path**: New `call_claude_cli_lightweight()` for
  single-turn metadata tasks (compression, instruction/fact extraction). Uses
  `--bare --effort low --max-turns 1 --no-session-persistence --tools ""`.
  Estimated 25-40% cost reduction for metadata tasks.

### Changed
- **Claude CLI `--bare` mode**: Main channel reply path now uses `--bare` to
  skip hooks/LSP/plugins/CLAUDE.md discovery (15-25% latency reduction).
- **Claude CLI `--exclude-dynamic-system-prompt-sections`**: Stabilizes system
  prompt across turns for better prompt cache hit rate (10-15% token reduction).
- **Claude CLI `--strict-mcp-config`**: Explicit MCP isolation per agent.
- **Gemini CLI system prompt**: Fixed from non-existent `--system-instruction`
  flag to `GEMINI_SYSTEM_MD` env var (temp file). Added `--approval-mode yolo`
  and conversation history prefix.
- **Codex CLI system prompt**: Fixed from non-existent `--instructions` flag
  to `AGENTS.md` file write. Added conversation history prefix.

### Fixed
- **Gemini runtime**: `--system-instruction` flag doesn't exist in Gemini CLI.
- **Codex runtime**: `--instructions` flag doesn't exist in Codex exec.



## [1.8.6] - 2026-04-19

### Added
- **Instruction Pinning** (P0): First user message → async Haiku extraction of
  core task instructions → stored in `sessions.pinned_instructions` → injected
  at system prompt tail (high-attention position). Survives session compression.
- **Snowball Recap** (P0): Each turn prepends `<task_recap>` with pinned
  instructions to user message. Zero LLM cost, utilizes U-shaped attention tail.
- **Clarification Accumulation**: When agent asks a question and user answers,
  the answer is appended to pinned instructions (capped at 1000 chars).
- **P2 Key-Fact Accumulator**: Lightweight cross-session memory replacing
  MemGPT Core Memory. Extracts 2-4 key facts per substantive turn via Haiku,
  stores in `key_facts` table with FTS5 search, injects top 3 relevant facts
  into system prompt. ~100-150 tokens vs MemGPT's 6,500 (87% reduction).



## [1.8.5] - 2026-04-19

### Fixed
- **MCP tools unavailable in channel reply**: Claude CLI in `-p
  --dangerously-skip-permissions` mode does NOT read global
  `~/.claude/settings.json` MCP servers — only project-level `.mcp.json`.
  Reverted v1.8.4's global migration back to per-agent `.mcp.json` with
  gateway startup auto-creation/fixup for all agents.



## [1.8.4] - 2026-04-19

### Changed
- **Global MCP server registration**: DuDuClaw MCP server (platform tools:
  `send_to_agent`, `list_cron_tasks`, `create_agent`, etc.) is now registered
  in `~/.claude/settings.json` (global) instead of per-agent `.mcp.json`.
  Gateway startup auto-migrates existing per-agent entries to global.
  Agent-specific MCP servers (Playwright, Browserbase) stay per-agent.
  This eliminates the class of bugs where agents lacked MCP tool access.



## [1.8.3] - 2026-04-19

### Fixed
- **Cron jobs invisible to MCP**: `list_cron_tasks` filtered by `default_agent`,
  hiding sub-agent cron tasks (duduclaw-pm, xianwen-pm, etc.). Dashboard showed
  them but agents couldn't see or manage them. Now returns all tasks by default.
- **Missing `.mcp.json` for agents**: Agnes pointed to non-existent `duduclaw-pro`
  binary; other agents had no `.mcp.json` at all, causing "沒有 MCP 通訊工具".
  Gateway startup now auto-creates/fixes `.mcp.json` for all agents.



## [1.8.2] - 2026-04-19

### Added
- **Sub-agent team roster injection**: System prompt now automatically includes
  a "Your Team" section listing sub-agents (by `reports_to` hierarchy), enabling
  natural delegation like "請團隊檢查" without requiring SOUL.md changes.
- **Release workflow_dispatch**: Release CI can now be manually re-triggered
  with `gh workflow run release.yml -f tag=vX.Y.Z` when tag-push CI fails.

### Fixed
- **Agent team awareness**: Agnes didn't recognize "duduclaw團隊" as her
  sub-agents because organizational context was missing from system prompt.



## [1.8.1] - 2026-04-19

### Added
- **Native multi-turn session management**: Claude CLI `--resume` with SHA-256
  deterministic session ID mapping. Fallback to XML-delimited history-in-prompt
  when session not found (e.g., account rotation).
- **Hermes-inspired turn trimming**: Long conversation turns (>800 chars) are
  trimmed to head 300 + tail 200 chars with `[trimmed N chars]` placeholder.
  CJK-safe char-level slicing. Zero LLM cost.
- **Direct API prompt cache strategy**: "system_and_3" cache breakpoint placement
  inspired by Hermes Agent for ~75% cache hit rate on multi-turn conversations.
- **Session compression summary injection**: Post-compression summaries (role=system)
  are now injected into system prompt instead of conversation turns.

### Removed
- **MemGPT 3-layer memory system** (-1,985 LOC): Core Memory, Recall Memory,
  Archival Bridge, Budget Manager, Consolidation Pipeline.
  The system prompt injection approach caused 6,500 tokens of bloat per prompt
  and "lost in the middle" attention degradation.
- **6 MCP tools**: `core_memory_get`, `core_memory_append`, `core_memory_replace`,
  `recall_search`, `archival_search`, `archival_insert`.
- 3 SQLite databases (`core_memory.db`, `recall_memory.db`) are no longer populated.

### Fixed
- **Session chain breakage**: Agnes losing context between consecutive messages
  ("幫我全部開啟" → "你指的是什麼？"). Root cause: stateless CLI subprocess
  per message with history in system prompt. Now uses native multi-turn.



## [1.7.2] - 2026-04-17

### Fixed
- **Stream-JSON empty result overwrite**: When Claude uses tools, the final `result`
  event often has an empty `result` field. The parser unconditionally overwrote
  accumulated assistant text with this empty string, causing false "Empty response"
  errors. Fixed in all 4 stream-json parsers (channel_reply, claude_runner, agent
  runner, gemini runtime).
- **Python SDK fallback OAuth awareness**: The Python SDK fallback now skips entirely
  for OAuth-only setups (it requires API keys) instead of producing the misleading
  "未設定任何 API 帳號" error. When an API key is available, it is explicitly
  passed to the subprocess.



## [1.6.0] - 2026-04-17

### Added
- **Git Worktree L0 isolation layer** (`worktree.rs`): lightweight per-task filesystem
  isolation via git worktrees. Cheaper than container sandbox — creates isolated working
  directories so concurrent agents don't step on each other's files.
  - `WorktreeManager`: full lifecycle management (create / remove / list / cleanup_stale)
  - **Atomic merge** with dry-run pre-check: merge → check → abort → real merge if clean.
    Protected by global `Mutex` to prevent concurrent merge corruption.
  - **Snap workflow** (inspired by agent-worktree): create → execute → inspect → merge/cleanup,
    with pure-function decision logic separated from I/O for testability.
  - **Friendly branch names**: `wt/{agent_id}/{adjective}-{noun}` from 50×50 word lists.
  - **copy_env_files**: copies `.env` etc. into worktree with path traversal jail,
    symlink rejection, and 1MB size limit.
  - **Structured exit codes**: `AgentExitCode` enum (Success/Error/Retry/KeepAlive).
  - **Resource limits**: max 5 worktrees per agent, 20 total.
- `ContainerConfig` extended with `worktree_enabled`, `worktree_auto_merge`,
  `worktree_cleanup_on_exit`, `worktree_copy_files` fields.
- Three-tier isolation routing in dispatcher: L0 Worktree → L1 Container → Direct.
- `WORKTREE_PATH` task-local in `claude_runner` for working directory override.

### Security (3-round deep review)
- Path traversal defense: canonical jail + absolute path rejection + `..` blocking.
- Agent ID sanitization: `sanitize_agent_id()` restricts to `[a-z0-9-]`.
- Branch name validation: `validate_wt_branch()` rejects `..`, leading `-`, non-`wt/` prefixes.
- Git command hardening: `--` separators on all `git merge` commands.
- `restore_head` validates branch names and commit hashes before `git checkout`.
- Symlink checks before `canonicalize()` to prevent TOCTOU bypass.
- Destination file removal before copy to prevent symlink race.
- Global merge lock via `OnceLock<Mutex<()>>` (not per-instance).

## [1.5.0] - 2026-04-17

### Added
- **SOUL.md content scanner** (`soul_scanner`): defends against "Soul-Evil Attack" —
  detects hidden HTML comments, invisible Unicode, zero-width steganography, data URIs,
  and hidden HTML tags in SOUL.md files.
- **Agent Stability Index** (`stability_index`): quantifies identity drift between
  SOUL.md versions with configurable thresholds (Warning / Critical).
- **Template sanitizer** (`template_sanitizer`): sanitizes prompt templates for
  injection resistance.
- **SoulSpec v0.5 compatibility**: soul_partition now recognizes SoulSpec v0.5 headers
  (Core Identity, Personality, Learned Patterns, etc.), with validation and export.
- **Audit Logs page**: new History tab showing JSONL audit events with severity icons,
  agent/channel/user badges, and expandable JSON details. Existing real-time log stream
  moved to Realtime tab.
- **Billing usage API** (`billing.usage`): returns live session count, active agents,
  connected channels, and inference hours from actual data sources.

### Changed
- GVU updater now runs soul_scanner + ASI checks before applying SOUL.md proposals.
- Soul guard integrity check includes content scan on every run and ASI on drift.
- BillingPage simplified — removed stub plan card, payment method, invoice history,
  and upgrade sections (not applicable to community edition).
- Logs nav icon changed from ScrollText to FileText; label renamed to "Audit Logs".

### Fixed
- Clippy: `sort_by_key` with `Reverse` instead of `sort_by` closure (3 occurrences).
- Windows sandbox test split with `cfg(not(windows))` / `cfg(windows)`.
- `clippy::collapsible_match` allow in webchat.
- CI: ignore RUSTSEC-2026-0098 and RUSTSEC-2026-0099.


All notable changes to DuDuClaw are documented here. For the authoritative
version history and per-commit detail, see `git log`.

## [v1.4.31] — 2026-04-16

### Fixed

- **GVU JSON fence parsing.** Rewrote `strip_json_fences()` to handle LLM
  responses with trailing text after the closing ` ``` ` fence. Previous
  implementation used `strip_suffix` which failed when judges appended
  commentary, causing 22 consecutive GVU trigger failures since 4/07.
  Unified fast-path and preamble-path into a single `rfind`-based approach.

### Changed

- Dashboard live data, logs fix, analytics API (from v1.4.30)

---

## [v1.4.29] — 2026-04-16

### Added

- **Skill auto-synthesis (Phase 3-4).** Gap accumulator detects repeated
  domain gaps → synthesizes skills from episodic memory (Voyager-inspired)
  → sandbox trial with TTL management → cross-agent graduation to global
  scope. New MCP tools: `skill_security_scan`, `skill_graduate`,
  `skill_synthesis_status`.

- **Task Board.** SQLite-backed task management with status/priority/
  assignment tracking and real-time Activity Feed via WebSocket. MCP tools:
  `tasks.list`, `tasks.create`, `tasks.update`, `tasks.assign`,
  `activity.list`, `activity.subscribe`.

- **Shared Knowledge Base.** Cross-agent wiki at `~/.duduclaw/shared/wiki/`
  for organizational knowledge (SOPs, policies, product specs). Wiki target
  classification (agent/shared/both), visibility control via `wiki_visible_to`
  capability, full-text search with author attribution. MCP tools:
  `shared_wiki_ls`, `shared_wiki_read`, `shared_wiki_write`,
  `shared_wiki_search`, `shared_wiki_delete`, `shared_wiki_stats`, `wiki_share`.

- **Autopilot rule engine.** Event-driven automation — triggers: task_created,
  task_status_changed, channel_message, agent_idle, cron. Actions: task_delegate,
  notify, skill_execute. Dashboard Settings → Autopilot tab for rule management
  and execution history.

- **Skill Market three-tab UI.** Marketplace / Shared Skills / My Skills with
  skill adoption flow and usage statistics.

- **Security status endpoint.** Exposes credential proxy, mount guard, RBAC,
  rate limiter, and SOUL drift state via API.

- **Analytics endpoints.** Conversation summaries and cost savings tracking.

### Enhanced

- MCP Server expanded from 70+ to 80+ tools.
- Dashboard i18n keys expanded from 540+ to 600+ (zh-TW / en / ja-JP).
- Evolution config extensibility for skill synthesis thresholds, graduation
  criteria, and curiosity-driven exploration.
- `CapabilitiesConfig` now includes `wiki_visible_to` with explicit `Default`
  implementation and `sanitize()` for safe deserialization.

## [v1.4.28] — 2026-04-15

### Fixed

- **Cognitive memory not persisted to database.** `StoreEpisodic` action
  from the prediction router was only debug-logged but never written to
  the per-agent `memory.db`. Dashboard Memory & Skills page showed empty
  even with cognitive memory enabled. Now creates
  `agents/<id>/state/memory.db` and stores `MemoryEntry` via
  `SqliteMemoryEngine`, making episodic observations queryable from the
  dashboard and MCP `memory.search` / `memory.browse` tools.

## [v1.3.17] — 2026-04-12

### Added

- **Action-claim verifier wired into live reply path (shadow mode).**
  The existing `duduclaw_security::action_claim_verifier` module (420
  lines, 13 unit tests, pure regex + audit-log cross-reference, zero
  LLM cost) was built but **never called from production code**. It is
  now invoked at two critical points:

  1. **Channel replies** ([channel_reply.rs](crates/duduclaw-gateway/src/channel_reply.rs)):
     immediately after the Claude CLI subprocess returns and before the
     reply is saved to the session / shipped to Discord / Telegram / LINE.
  2. **Cron task execution** ([cron_scheduler.rs](crates/duduclaw-gateway/src/cron_scheduler.rs)):
     after the scheduled agent responds and before `record_run` marks
     the task as successful.

  On both paths, a `dispatch_start_time` is captured before the CLI
  call. After the reply arrives, `detect_hallucinations(home_dir,
  agent_id, &reply, &dispatch_start_time)` extracts action claims via
  regex (zh-TW + English patterns for AgentCreated / AgentDeleted /
  SoulUpdated / MessageSent / AgentSpawned), reads the MCP tool-call
  audit log (`tool_calls.jsonl`) filtered to this turn + this agent,
  and cross-references each claim against actual successful tool calls.

  **Shadow mode**: detections are logged at `warn!` level and written
  to `security_audit.jsonl` via `log_tool_hallucination()`, but the
  reply text is **not modified**. This lets us collect a baseline
  `ungrounded_claim_rate` before flipping to enforce mode.

- **Implementation plan document** at [docs/TODO-agent-honesty.md](docs/TODO-agent-honesty.md):
  3-phase defence-in-depth roadmap (Action-Claim Verifier → Proxy State
  Verifier + Abstain Actions → Tool Receipts / NabaOS), backed by 6
  verified arxiv papers (ToolBeHonest 2406.20015, Agent-as-a-Judge
  2410.10934, Relign 2412.04141, MCPVerse 2508.16260, Agent Hallucination
  Survey 2509.18970, Tool Receipts 2603.10060). Day-by-day schedule,
  success metrics, known limitations, and enforce-mode policy options.

---

## [v1.3.16] — 2026-04-12

### Fixed

- **`duduclaw agent create` now writes `.mcp.json`.** New agents created
  via the CLI (or the `wizard` subcommand) previously got every scaffold
  file *except* `.mcp.json`, which meant the duduclaw MCP server never
  attached to their Claude Code sessions and tools like `create_agent`,
  `spawn_agent`, `list_agents`, `send_to_agent` were silently unavailable.
  SOUL.md's "always call `create_agent`" rule became unenforceable
  because the tool literally didn't exist in the model's toolbelt — the
  model either fell back to raw Bash writes (blocked by agent-file-guard
  since v1.3.15) or fabricated agent creation in plain text. Both the
  CLI (`cmd_agent_create`) and the industry wizard now write a
  `.mcp.json` pointing at the currently-running duduclaw binary.

- **Hint message placeholder not expanded.** `duduclaw agent create`
  used to print `Run \`duduclaw agent run {agent_name}\` to start a
  session` literally with `{agent_name}` unexpanded (because the string
  was passed to `style()` instead of `format!()`). The hint now shows
  the real agent name.

### Added

- **`duduclaw agent create` flags.** The subcommand previously took
  only a positional `name`. It now accepts `--display-name`, `--role`,
  `--reports-to`, `--icon`, and `--trigger` so teams can be scripted
  without post-hoc `sed` on `agent.toml`:

  ```sh
  duduclaw agent create xianwen-tl \
    --display-name "Xianwen TL" \
    --role team-leader \
    --icon 🎯
  ```

- **`AgentRole` enum gained `TeamLeader` and `ProductManager`** so
  planner/coordinator agents can declare a more specific role. The enum
  serialisation switched from `rename_all = "lowercase"` to
  `rename_all = "kebab-case"`; single-word variants (`main`, `worker`,
  `qa`, `planner`, …) look identical to the old encoding so existing
  `agent.toml` files keep parsing unchanged. Multi-word variants use
  kebab-case (`team-leader`, `product-manager`).

- **Lenient role parsing.** `AgentRole::from_str` normalises spacing /
  case / underscore vs hyphen and accepts common aliases: `engineer`
  (→ Developer), `tl`/`lead`/`teamlead` (→ TeamLeader), `pm`
  (→ ProductManager), `quality`/`quality-assurance` (→ Qa). The same
  aliases are accepted by serde via `#[serde(alias = …)]`, so
  round-tripping natural-language role input through `agent.toml`
  resolves to the canonical form on the next read.

- **`AgentRole::as_str()` + `Display` impl + `valid_values_help()`**
  helpers for error messages. The MCP `agent_update` handler now uses
  `AgentRole::from_str` with a single shared help string instead of its
  own private match table.

### Tests

- 6 new unit tests in `duduclaw_core::types::tests` covering round-trip
  (`agent_role_roundtrip_via_serde_json`), wire format
  (`agent_role_kebab_case_wire_format`), serde aliases
  (`agent_role_serde_aliases_accepted`), lenient `FromStr` parsing
  (`agent_role_from_str_lenient_normalisation`), rejection of garbage
  (`agent_role_from_str_rejects_garbage`), and `Display` round-trip.

---

## [v1.3.15] — 2026-04-11

### Fixed

- **agent-file-guard now blocks Bash-based agent-structure writes.** The
  PreToolUse hook matcher was previously `Write|Edit|MultiEdit` only, so a
  sub-agent could silently bypass the guard by invoking
  `Bash mkdir -p /some/project/.claude/agents/foo` or
  `Bash cat > /some/project/.claude/agents/foo/agent.toml`. The guard now
  also matches `Bash`, and `cmd_hook_agent_file_guard` dispatches on
  `tool_name` so that Bash commands are inspected against the new
  [`duduclaw_core::check_bash_command`] helper.

  **Policy:** any Bash command whose text contains the substring
  `.claude/agents/` is blocked. Rationale — the canonical agent root is
  `~/.duduclaw/agents/<name>/` and never contains that path segment, and
  project trees that an agent *works on* should never have an in-tree
  `.claude/agents/` directory (Claude Code's own config lives at
  `~/.claude/`, not nested in project repos). The rule is intentionally
  conservative: even read-only listings that mention `.claude/agents/`
  are blocked, since the correct replacement is the `list_agents` MCP
  tool or a direct `Read` on a known canonical path.

  Existing agents get the updated matcher automatically on next invocation
  (the hook installer runs on every `call_claude_for_agent_with_type` and
  updates the tagged hook entry in place — no manual action required).

### Tests

- 8 new unit tests in `duduclaw_core::agent_guard::tests`
  (`bash_mkdir_in_foreign_project_is_blocked`,
  `bash_write_to_agent_toml_via_heredoc_is_blocked`,
  `bash_with_quoted_path_is_blocked`,
  `bash_ls_mentioning_sentinel_is_also_blocked`,
  `bash_git_status_is_allowed`,
  `bash_ls_canonical_agent_dotclaude_is_allowed`,
  `bash_touching_claude_hooks_subdir_is_allowed`,
  `bash_nested_agents_under_home_is_still_blocked`).

---

## [v1.3.14] — 2026-04-11

### Added

- **SQLite-backed cron task store with hot reload.** Replaced the legacy `cron_tasks.jsonl` file with a proper relational store at `~/.duduclaw/cron_tasks.db` (WAL mode). The new `CronStore` module ([crates/duduclaw-gateway/src/cron_store.rs](crates/duduclaw-gateway/src/cron_store.rs)) exposes full CRUD (`list_all`, `list_enabled`, `get`, `get_by_name`, `insert`, `update_fields`, `set_enabled`, `delete`, `record_run`) and tracks run history (`last_run_at`, `last_status`, `last_error`, `run_count`, `failure_count`) so the dashboard can surface per-task reliability metrics.

- **Hot-reload signal for `CronScheduler`.** The scheduler's run loop now uses `tokio::select!` to wake on **either** a 30-second baseline tick **or** an `Arc<Notify>` pulse fired by `CronScheduler::reload_now()`. Dashboard edits (`cron.add` / `cron.update` / `cron.pause` / `cron.resume` / `cron.remove`) now take effect immediately — no more 5-minute reload window. MCP subprocess writes are picked up on the next 30-second tick via shared WAL-mode SQLite (no inter-process signal needed).

- **New dashboard RPC methods:** `cron.update` (partial-field update) and `cron.resume` (re-enable paused task). All cron handlers now accept either `id` or `name` for identification, and `cron` or `schedule` for the expression (legacy alias).

- **One-shot JSONL → SQLite migration.** On first startup after upgrade, `CronStore::migrate_from_jsonl` imports any existing `cron_tasks.jsonl` entries into the DB, then renames the file to `cron_tasks.jsonl.migrated` to avoid re-running. Idempotent and safe to re-invoke.

### Changed

- **MCP `schedule_task` writes to SQLite directly** instead of appending JSONL. Both the gateway process and the MCP subprocess share the same WAL-mode DB — safe for concurrent access.

- **Last-run merge strategy on reload.** When the scheduler reloads (either via hot-reload signal or baseline tick), each task's `last_run` is merged as `max(in-memory, DB last_run_at)` to prevent same-minute re-fires after a mid-cycle reload.

### Tests

- 2 new unit tests for `CronStore`: CRUD roundtrip + JSONL migration idempotency.

---

## [v1.3.13] — 2026-04-11

### Added

- **Stream-json diagnostics on CLI failures.** The `channel_reply::spawn_claude_cli_with_env` now tracks stream-json event counts (`lines_seen`, `events_parsed`, `assistant_events`, `text_blocks`, `thinking_blocks`, `tool_use_blocks`, `result_events`) and captures the last raw stream line, `result.subtype`, the latest `message.stop_reason`, and a tail of stderr. All of these are embedded into the error message when `spawn_claude_cli_with_env` returns `Empty response from claude CLI` or non-zero exit. `channel_failures.jsonl` is now self-describing — no more needing to reproduce manually in a shell to figure out *why* a reply was empty.

- **`DUDUCLAW_STREAM_DEBUG=1` env var.** When set on the gateway process, every raw line from `claude`'s stdout is appended to `<home>/claude_stream.log`. Off by default (the log can be large and contains user prompts).

- **Stderr draining.** A background tokio task drains `claude` CLI's stderr pipe concurrently and keeps the last 2 KiB for error diagnostics. Without this, `claude` could block forever if stderr filled its pipe buffer (~64 KiB).

### Changed

- **Classifier substring matching still works on diagnostic-suffixed errors.** The error strings returned by `spawn_claude_cli_with_env` now look like:
  ```
  Empty response from claude CLI (exit=0 lines=42 events=30 assistant=2 text_blocks=0 thinking=1 ...)
  ```
  `classify_cli_failure` uses substring matches so the same reason (`EmptyResponse`, `SpawnError`, etc.) is still detected. Two new regression tests lock this invariant.

### Tests

- **415 tests passing** (core: 21, gateway: 377, agent: 17). Added 2 new classifier tests for diagnostic-suffixed error strings.

---

## [v1.3.12] — 2026-04-11

### Fixed

- **Rotator broke keychain auth by injecting `CLAUDE_CONFIG_DIR=~/.claude`**
  (regression from the multi-account rotation introduced in v1.3.11). When
  the auto-detected default OAuth session was selected, `select()` set
  `CLAUDE_CONFIG_DIR` to `~/.claude` even though that *is* the claude CLI
  default — and the `claude` CLI, when the env var is set explicitly, stops
  looking at the macOS keychain for credentials. Every channel reply call
  then hit "Not logged in · Please run /login".
  Fix: `account_rotator::select()` now skips the `CLAUDE_CONFIG_DIR`
  injection when `credentials_dir` equals the default `~/.claude`, so
  claude CLI picks up keychain auth normally. Non-default profile
  directories (`~/.claude/profiles/work`, etc.) still get the env var.
  Regression tests in `account_rotator::select_env_tests` lock this in.

- **Stream parser silently swallowed `is_error: true` results.** The
  `claude` CLI emits terminal errors (auth failure, synthetic responses)
  as `type="result"` stream-json events with `is_error: true`, with the
  error text in the `result` field. Both `channel_reply::spawn_claude_cli_with_env`
  and `claude_runner::call_claude_streaming` were capturing the error
  text as `result_text` and returning `Ok(...)`, so users saw
  "Not logged in · Please run /login" posted to Discord/LINE/Telegram as
  Agnes's actual reply. Now:
  - `is_error: true` on a `result` event → `return Err("claude CLI stream error: ...")`
  - `error` field on an `assistant` event → same
  - Post-loop: any non-zero exit code is a hard failure (previously we
    only errored when `result_text` was empty, which let partial output
    leak through).

- **`FailureReason::AuthFailed` classifier** — new branch in
  `classify_cli_failure` detects `"Not logged in"` / `"authentication_failed"` /
  `"please run /login"` and surfaces a zh-TW message that actually tells
  the user to run `claude /login` instead of the misleading
  "`claude auth status`" hint (which only checks state, doesn't fix auth).

### Tests

- 2 new regression tests in `duduclaw-agent::account_rotator::select_env_tests`
- 2 new classifier tests + 1 end-to-end pipeline test in `channel_reply::fallback_tests` / `rotation_tests`
- **413 tests total passing** (core: 21, gateway: 375, agent: 17)

---

## [v1.3.11] — 2026-04-11

### Added

- **Agent file-write guard (Option 3 hardening)** — `duduclaw hook
  agent-file-guard` PreToolUse hook is now automatically installed into
  `<agent_dir>/.claude/settings.json` on every agent creation (MCP
  `create_agent`, dashboard `agents.create`, CLI `wizard`, channel reply
  spawn, dispatcher spawn, and gateway startup). Blocks agents from using
  raw Write/Edit/MultiEdit to create `agent.toml` / `SOUL.md` / `CLAUDE.md`
  / `MEMORY.md` / `.mcp.json` / `CONTRACT.toml` outside the canonical
  `<home>/agents/<name>/` tree. Agents must use the `create_agent` MCP
  tool instead, so the registry and dashboard always see newly-created
  sub-agents. Pure Rust enforcement — no shell dependencies, cross-platform
  (macOS/Linux/Windows).
  Files: `crates/duduclaw-core/src/agent_guard.rs`,
  `crates/duduclaw-gateway/src/agent_hook_installer.rs`,
  `crates/duduclaw-cli/src/lib.rs` (new `Hook` subcommand).

### Fixed

- **Channel reply: intermittent "Claude Code not found" error (#fallback-fix)**
  Root cause: the channel reply path (`channel_reply::call_claude_cli`) was
  bypassing the `AccountRotator` entirely and spawning `claude -p` against
  the ambient environment. When the single default OAuth session was cooling
  down (rate-limit / token refresh / billing), every attempt failed and the
  user saw a hardcoded "please run `claude auth status`" message that
  misrepresented the actual cause. The sub-agent dispatcher path already
  rotated correctly, which explained the "有機率" symptom.

  This release routes the channel reply path through a new testable
  rotation primitive `rotate_cli_spawn`, so **both** the dispatcher and
  channel paths now use the same multi-OAuth / API-key rotation, cooldown
  tracking, and billing-exhaustion handling.
  Files: `crates/duduclaw-gateway/src/channel_reply.rs`.

- **Misleading fallback error message → category-specific diagnostics**
  Replaced the hardcoded `"{name} 收到你的訊息，但目前無法回覆。請確認 Claude
  Code 已安裝並登入"` message with a classifier (`FailureReason`) that
  distinguishes:
  - `BinaryMissing` — actually missing binary (keeps the `auth status` hint)
  - `RateLimited` — 忙線中，請稍後再試
  - `Billing` — 帳號額度已用完
  - `Timeout` — 30 分鐘處理超時
  - `SpawnError` — 子程序啟動失敗
  - `EmptyResponse` — 空回應
  - `NoAccounts` — 尚未設定帳號
  - `Unknown` — 通用錯誤提示

  Each fallback also appends a structured JSONL record to
  `~/.duduclaw/channel_failures.jsonl` for dashboard surfacing.

- **`which_claude()` now discovers launchd / Finder-launched installs**
  Added candidate paths for `/opt/homebrew/bin/claude` (Apple Silicon
  Homebrew), `$HOME/.bun/bin/claude`, `$HOME/.volta/bin/claude`,
  `$HOME/.asdf/shims/claude`, plus NVM version-directory scanning
  (`$HOME/.nvm/versions/node/*/bin/claude`). Previously, gateways launched
  from Finder / Dock / launchd without Homebrew on `PATH` would fail to
  find `claude` even when it was installed.

  Also extracted `which_claude_in_home(home: &Path)` as a pure, testable
  helper that doesn't touch `PATH` or environment state.
  Files: `crates/duduclaw-core/src/lib.rs`.

### Added

- **`AccountRotator::push_account_for_test`** — cross-crate test helper
  (marked `#[doc(hidden)]`) so rotation unit tests can inject synthetic
  accounts without writing a config file or shelling out to `claude auth
  status`. Files: `crates/duduclaw-agent/src/account_rotator.rs`.

### Tests

- 7 new unit tests in `duduclaw-core::which_claude_tests` covering Bun,
  Volta, asdf, npm-global, NVM, candidate ordering, and "no candidates"
  fallback.
- 10 new unit tests in `duduclaw-gateway::channel_reply::fallback_tests`
  covering `classify_cli_failure` (rate-limit / billing / timeout / binary /
  empty / spawn / unknown) and `format_fallback_message` (message content
  assertions for zh-TW, agent name substitution, correct vs. misleading
  hints).
- 6 new async tests in `duduclaw-gateway::channel_reply::rotation_tests`:
  - `single_account_success_is_first_try` — smoke-replacement for the
    single-OAuth regression path
  - `rotation_advances_past_rate_limited_account` — verifies 2-account
    cycling and rotator state after `on_rate_limited`
  - `rotation_all_fail_propagates_last_error` — all-fail aggregator
  - `rotation_billing_error_triggers_long_cooldown` — 24h cooldown
  - `rotation_empty_rotator_returns_empty_exhausted` — primitive contract
  - `end_to_end_rate_limit_yields_busy_message` — full pipeline from
    rotation failure → classification → user message; guards against
    future regressions where the message incorrectly says "please install"

### Developer Notes

- `is_billing_error` and `is_rate_limit_error` in `claude_runner.rs` are now
  `pub(crate)` so the channel reply path can reuse the shared classifiers.
- `spawn_claude_cli_with_env` carries `#[allow(clippy::too_many_arguments)]`
  (8 args, pure extraction from the pre-existing 7-arg `call_claude_cli`).
- The rotation loop is now decoupled from the subprocess spawn: see
  `rotate_cli_spawn<F, Fut>(rotator, spawn, input_size_hint)`. This enables
  deterministic testing and future reuse (e.g., for other LLM backends).

---

Earlier versions: see `git log --oneline` for commit-level history.
Recent highlights:

- **v1.3.10** — Discord cross-channel reply error, cognitive memory toggle reset
- **v1.3.9** — Discord auto-thread sends guide message in channel
- **v1.3.8** — service stop kills process, all-channel attachment forwarding
- **v1.3.7** — Homebrew formula version alignment
