# Changelog

## [Unreleased]

## [1.43.0] - 2026-07-23 — Grok 訂閱帳號全鏈路：headless 根治、dashboard 裝置碼登入與 doctor 診斷

### Fixed
- **Grok CLI headless 空輸出根治**：經銷商客戶機上 `grok` 互動模式正常、SuperGrok device-auth 已登入，但 DuDuClaw spawn 的 `grok -p` 一直回空 stdout（exit 0）——與 2026 Claude `-p` OAuth 訂閱被擋同構。`GrokRuntime` 現在（1）顯式注入使用者真實 `HOME`/`USERPROFILE`（優先取 gateway 已解析的 `<user>/.duduclaw` 之父，launchd/Docker 下 `$HOME` 錯誤時仍正確）並轉發 `GROK_HOME`，讓 grok 找得到 `~/.grok` 憑證；（2）以 word-boundary、大小寫不敏感的樣式集辨識未登入/憑證過期 stderr（含誤殺防護），命中即回傳含 "not logged in"/"authentication" 的錯誤讓 `classify_cli_failure` 落 `AuthFailed`（zh-TW 指引「請執行 `grok login --device-auth`」）；（3）空 stdout+exit 0 且非 auth 錯誤時，用 portable-pty 在真 TTY 下一次性重跑同一 `grok -p`（`pty_retry=true/false` tracing 供遠端判讀），救回 headless-under-pipe 這一類；native sandbox 需求時 fail-closed 跳過重試。`duduclaw doctor` 新增「Grok CLI 診斷」段：binary/版本 + 活體 `grok -p "ping"`（15s）回報 exit/stdout 長度/stderr tail/auth 判定/PTY 重試結果，一條指令產出遠端除錯全部證據。

## [1.42.0] - 2026-07-23 — OS 原生主動感知（P2–P4）、API 模型 MCP 工具面與空回覆斷鏈根治

### Added
- **API-mode 非 Claude 模型獲得完整 MCP 工具面**（根治性缺口）：多 runtime 匯流點的 openai-compat
  `AgentRuntime`（`crates/duduclaw-gateway/src/runtime/openai_compat.rs`）過去只送純 messages（無
  `tools`），導致以 API 直連的 Grok／DeepSeek／MiniMax 等 agent 永遠碰不到 15 個 Odoo／memory／channel
  等 MCP 工具——問 Odoo 客戶資料只會回「我先找出 Odoo 設定」就停。現在當 agent 有可用工具時，
  `execute()` 改走 `duduclaw-llm` 的 openai-compat provider ＋ `run_tool_loop`：spawn 一個 duduclaw
  `mcp-server` stdio child（帶 `DUDUCLAW_AGENT_ID`／`HOME`／`PORT`／`INSTANCE`，比照 CLI runtime 慣例）、
  掛 `ToolRegistry`，工具表先過 capabilities 過濾（deny-by-default：`denied_tools` 一律剔除、
  `allowed_tools` 非空取交集）再暴露給模型，並套用靜態 `PolicyKernel` 政策；迴圈上限沿
  `DEFAULT_MAX_TOOL_ITERS`，多輪 token 用量累計進 `RuntimeResponse`。`[capabilities] scoped_tools`／
  `approval_required_tools` 等 dispatch 層閘門在 mcp-server 端本來就會執行（MCP dispatch 是強制點），
  此路徑不重複實作。**失敗降級**：MCP child 起不來／registry 空／capability 過濾後無工具 → warn log
  後回退到現行純 messages 路徑（沒有工具總比不回好）；provider／model 傳輸錯誤則向上傳播交給 failover。
  現行未 commit 的空 content⇒Err、歷史空 turn 過濾行為全數保留；tool_use-only 回合不誤判為空回覆
  （只有迴圈終了仍無文字才算 EmptyResponse）。新增 6 個單元測試涵蓋工具過濾、迴圈終止、tool-only
  回合、token 累計與 provider-prefixed model id。
- **Server 映像內建 Grok CLI（xAI Grok Build）**：`container/Dockerfile.server` 比照 `agy`（Antigravity）
  段落樣式，新增 `grok` 官方安裝步驟——`curl -fsSL https://x.ai/cli/install.sh | bash` 下載/驗證後，
  以 `install -m 0755` 從 `$HOME/.grok/bin/grok`（installer 產生的符號連結，`install` 會解引用取得
  真正的 binary）relocate 到 `/usr/local/bin/grok`，再跑 `grok --version` 驗證可執行；未內建
  `XAI_API_KEY`（runtime env 才提供，與其餘 CLI 一致）。同步補上 `~/.grok` 目錄（比照
  `.claude`/`.codex`/`.gemini` 慣例）的 `mkdir` + `VOLUME` 宣告，以及根目錄 `docker-compose.yml`
  的 `duduclaw-grok` 具名 volume 與 `XAI_API_KEY` env 轉發（`docker-compose.quickstart.yml` 因本就
  未列 `OPENAI_API_KEY`/`GEMINI_API_KEY`，維持精簡未變動）。
- **OS-native agent P4-3+：dashboard「OS」頁事件即時流**：「近期感知事件」面板從純快照＋手動刷新
  升級成 WS 即時流。新增 dashboard RPC `os.events.subscribe`／`os.events.unsubscribe`
  （`require_admin`，與其餘 `os.*` 同門檻——os_file／os_frontmost 事件帶檔案路徑與視窗標題，
  敏感度比一般活動事件高，因此沿用 `logs.subscribe` 的每連線旗標訂閱模式，而非
  `activity.new`／`task.*` 的無條件全連線廣播）；訂閱後即時推送 `os.events.entry` frame
  （與 `os.events.recent` 的 `EventRow` 同形狀，僅少一個尚未寫入 `events.db` 的 `id`）；
  每連線滑動 1 秒視窗限速 20 筆，超過丟棄＋計數，斷線重連天然重置；連線關閉隨 task 結束自動
  取消訂閱。前端 `OSPage.tsx` 認證後自動訂閱，推播事件前插進列表（合成負數 id 兼作進場動效
  標記，複用既有 `.animate-fade-up`、`prefers-reduced-motion` 全域已 gate）、環狀緩衝上限 200
  筆；連線離開 `authenticated` 時降級顯示「即時更新已中斷，顯示快照」並保留手動刷新鈕。後端新增
  9 個單元測試（`cargo test -p duduclaw-gateway --lib` 2649 passed / 0 failed）、前端新增 2 個
  （`npx vitest run` 680 passed）。契約詳見 `commercial/docs/TODO-os-native-agent.md`
  「P4-3+ 事件即時流已接線」。
- **OS-native agent P4-3：dashboard「OS」頁後端（RPC＋個人版配額＋主動功能熱重載）**：交付
  dashboard OS 頁的全部後端（前端 `web/` 待接）。**五個 dashboard WebSocket RPC**（`require_admin`）：
  `os.status`（全 agent os_native／watch 路徑＋即時統計／frontmost 輪詢秒數＋running／footprint／
  proactive 三欄／induced 規則數，外加 fleet 配額 `{limit, used}` ＋ edition）、`os.settings.update`
  （per-agent 寫 `os_native`／`[os_watch] footprint`＋`frontmost_poll_secs`／`[proactive]`
  `enabled`＋`base_threshold`(1–5)＋`max_per_hour`(0–1000) → 走配額閘＋熱重載，remap 到既有
  `agents.update` 寫入路徑複用驗證器）、`os.gate.recent`（`proactive_gate.jsonl` 尾部 N 行〈預設
  50、上限 200〉＋四象限聚合）、`os.events.recent`（`events.db` 近 N 筆 `os_*` 事件、newest first）、
  `os.doctor.run`（on-demand 昂貴呼叫：複用 `duduclaw-os` 的 notification／frontmost／calendar 探針
  ＋`mdfind` 存在性，TCC 拒絕 report-only 不繞過）。**個人版 os_native 配額＝1**（鎖 quota 不鎖能力）：
  新增 `license_runtime::os_native_agent_quota(edition)`（個人版 `Some(1)`／企業版 `None`＝無限，
  未發明不存在的 license 欄位），寫入面（設 `os_native=true` 超額 → 結構化錯誤
  `os_native_quota_exceeded`＋zh-TW 文案）與啟動面（`os_events::resolve_os_native_allowed` 穩定
  排序取前 N，其餘跳過＋warn＋audit `os_native_quota_skipped`）**共用同一 quota helper**，fail-closed
  一致。**主動功能熱重載**：新增 `OsFrontmostRegistry`（補齊 P2-4 遺留的 frontmost 輪詢 hot-reload
  債）、`FootprintTracker` 成員資格改 interior-mutable ＋ `set_enabled`（就地啟停聚合，停用丟棄當日
  bucket）、`[proactive]` 三欄因 `ProactiveGate` per-evaluate 讀檔而寫檔即生效（無需 registry，已查
  證）；三者統一走 `agents.update` 既有 `hot_reload_os_watcher` hook，改配置不需重啟 gateway。
  `events_store` 新增 `fetch_recent_by_prefix`。新增 ~18 個單元測試全綠。介面文件（完整 RPC 契約表）
  見 `commercial/docs/TODO-os-native-agent.md`「P4-3 後端已實作」。
- **OS-native agent P4-1：PBD 規則歸納（最小版）**：新增
  `duduclaw-gateway/src/rule_induction.rs`——從 `events.db` 近 7 天事件確定性（零 LLM）歸納
  重複的「OS 感知 → 使用者反應」模式，經 HITL 確認後才生效（ALLOY arXiv:2510.10049 /
  TaskMind CHI'25；設計取捨見 `commercial/docs/research-os-native-agent-methodology.md` §3.2）。
  **偵測**：同一 `(agent, 事件型態, 檔案副檔名／路徑前綴／app)` 的 `os_file`／`os_frontmost`
  感知事件，若在其後固定時間窗（預設 600s）內出現同 agent 的使用者互動（`task.created`／
  `activity.new`）達 ≥N 次（預設 5），即命中——取不到反應訊號就不歸納（不腦補）。
  **候選**：命中模式 → 生成候選 autopilot 規則，action 一律 `proactive_notify`（最保守，建議
  而非代做，天然過 ProactiveGate），複用 dashboard 既有 `validate_autopilot_trigger_event`／
  `validate_autopilot_action` 寫入前驗證；指紋去重，狀態存 `<home>/rule_induction_state.json`
  （`with_file_lock`）。**HITL**：候選走 `ApprovalBroker` request（zh-TW 白話提案文字，感知
  token 過 `sanitize_perception_text`）——核准才寫入 autopilot_store（`enabled=true` + metadata
  `induced=true`/`induced_at`/`fingerprint`/`source`），拒絕／TTL 過期則指紋進 blocklist 不再提；
  **任何候選未經核准絕不生效（fail-closed：無 broker／無可投遞頻道／偵測異常皆壓下）**。頻率
  上限每 agent 每日 2 個候選。`autopilot_rules` 加 additive `metadata` 欄（冪等 migration，
  dashboard rule JSON 增 `metadata` 欄供未來標示與退場）；`EventBusStore` 加 `append_with_ts`
  以保留原始事件時間。**events.db 接線（同批完成）**：新增 `os_events::spawn_os_event_persistence`
  訂閱橋，把 `os_file`／`os_frontmost` 廣播事件持久化進 `events.db`（`EventBusStore` 加 `source`
  欄 + `append_with_source`，冪等 `ALTER TABLE` migration），標記 `source=internal_broadcast`；
  `autopilot_engine::spawn_events_db_poll` 新增 `should_rebroadcast()` 檢查該標記並跳過，避免
  同一事件被 in-process 廣播與 events.db poll 雙重派發。30 分鐘歸納 tick 掛進 `server.rs`
  （`rule_induction::spawn_induction_loop`，master 開關 `config.toml [rule_induction] enabled`，
  `RuleInductionConfig::from_home` 每 tick 重讀），生產 channel resolver 複用既有
  `goal_notify::agent_notify_target`。新增 26 測試（P4-1 本體 16：7 純偵測＋3 候選 JSON＋6 HITL
  整合；events.db 接線 10：rule_induction config load 3＋events_store 3＋os_events 3＋
  autopilot_engine 1），gateway lib 全綠、零回歸。
- **OS-native agent P4-2：persona 抑制規則自動化**：新增
  `duduclaw-gateway/src/persona_induction.rs`——把「主動介入被打槍」的歷史確定性（零 LLM）歸納成
  「何時別打擾」的 persona 規則餵回 `ProactiveGate`（ContextAgent arXiv:2505.14668 persona
  ablation −12.3% F1；設計取捨見 `commercial/docs/research-os-native-agent-methodology.md`
  §1.3/§②-3）。**歸納**：讀 `proactive_gate.jsonl`，按時段（Asia/Taipei `工作時間`/`深夜`）×
  事件型態×interruptibility 三分位分群，同群 `false_alarm` 累計 **≥3 次且跨 ≥2 個不同 UTC 日**
  （GovMem arXiv:2607.02579 式獨立證據門檻，同日多次不算）→ 生成「{時段}的 {event} 類主動通知
  曾被多次忽略/打槍，預設沉默」規則。**雙寫**：`store_temporal` 落地治理記錄（origin=`agent_derived`，
  confidence／origin_trust 依 v1.41 origin ceiling 0.6，掛既有 `PROBATION_RULE_TAG`）；橋接寫入
  `key_facts`（`store_fact`）讓規則實際可被 `ProactiveGate` 既有的
  `autopilot_engine::fetch_persona_lines`→`search_facts` 路徑檢索到——`store_temporal` 寫的
  `memories` 表與 `search_facts` 查的 `key_facts` 表是兩個不同 store，此橋接補上這道落差。
  **Janus probation（撤銷）**：同情境後續若出現 `correct_detection`（證明其實該打擾），下次歸納
  時偵測到晚於規則產生時間的正確偵測 → 對同一 `(subject,predicate)` 再寫一筆 `object="lifted"`
  的 temporal memory row，觸發既有 supersession 鏈自動撤銷。指紋去重（同群不重複歸納）＋
  每 agent 上限 10 條，滿了淘汰最舊現行規則。獨立每小時 tick 迴圈、實際聚合工作每 UTC 日僅跑一次
  （`last_run_day` 短路，不掛進 P2-3 既有 60s 迴圈本體）。**已知限制**：`key_facts` 無
  delete-by-id API，撤銷時橋接的舊規則文字靠既有 `purge_stale_facts` janitor 自然汰除，非立即
  移除；時段固定台灣時區，無 per-agent 設定。未碰 `proactive_gate.rs` 本體／`channel_reply.rs`／
  `failover.rs`／`runtime/`／`situation_classifier.rs`／`rule_induction.rs`／`os_events.rs`。
  新增 19 測試（分組聚合、GovMem 門檻、`plan_induction` 純規劃去重/撤銷/淘汰、`store_temporal`
  寫入形態、key_facts 橋接可檢索、supersession 鏈、端到端流程、daily-tick 閘門），既有
  `proactive_gate`/`proactive_feedback`/`reflexion`/`rule_lifecycle` 69 測試零回歸。
- **OS-native agent P4-4：數位足跡 memory 化**：新增 `duduclaw-gateway/src/footprint_distill.rs`——
  把 `os_file`/`os_frontmost` 感知事件流聚合成溫故式 temporal memory（Memory for Autonomous LLM
  Agents arXiv:2603.07670；設計取捨見
  `commercial/docs/research-os-native-agent-methodology.md` §4.2/§②-5）。**聚合不逐事件寫**：
  `FootprintTracker` 純記憶體按 UTC 日累計每 agent 的前景 app 秒數、活躍目錄事件數、活躍小時分佈；
  背景 ticker 每 15 分鐘檢查一次 UTC 日界，跨界才蒸餾（寫入頻率 O(agents × days) 不是
  O(events)）。**三個 predicate**：`subject="user"`、`daily_active_app` / `active_directory` /
  `active_hours`，Top-N 依統計量排序後編碼進 `object`，同 `(subject,predicate)` 靠既有
  `store_temporal` supersession 自動取代前一天、`get_history` 保留完整鏈。**origin/sensitivity**：
  `origin="agent_derived"`（v1.41 既有類別 ceiling 0.6，未新增 origin class）；
  `daily_active_app`/`active_hours`（源自 `os_frontmost`）標 `Sensitivity::Personal`，
  `active_directory`（源自 `os_file`）標 `Sensitivity::Internal`，對齊 P3-2 既有感知源分級表；
  `stamp_metadata` 的第一個生產呼叫點。**data minimization**：`directory_of()` 在原始路徑上先丟棄
  檔名只留目錄，之後才過 `sanitize_perception_text`——下游從未接觸過檔名任一子字串。**opt-in**：
  `[os_watch] footprint = true`（deny-by-default，疊在 `os_native` 之上，且在聚合層就拒絕，未選用
  的 agent 從不進入追蹤集合，不只是寫入層跳過）；啟動時一次性掃描決定追蹤集合，無 dashboard
  熱重載（同 P2-4 `frontmost_poll_secs` 既有取捨）。`handlers.rs` 的 `apply_os_watch_to_table` 加
  `footprint` 布林寫入，`read_os_watch_json` 原樣回顯整個表故自動帶出新欄位、免改動。**已知限制**：
  聚合狀態純記憶體、重啟遺失當日未蒸餾部分（影響最多一天）；`memory_search`/`search_layer` 尚無
  retrieval-side 依 `sensitivity` 過濾結果的消費者（P3-2 的群聊剝除只做在 persona 區塊/wiki
  namespace 兩層），本次以 `Sensitivity::allowed_in_session()` 組合測試證明寫入面打標正確、可供
  未來消費者使用。未碰 `channel_reply.rs`/`failover.rs`/`runtime/`/`proactive_*.rs`/
  `situation_classifier.rs`/`rule_induction.rs`/`persona_induction.rs`。新增 24 測試（config
  reader、目錄丟檔名、聚合、渲染、origin/sensitivity 打標、群聊剝除組合證明、跨日
  supersession、carry-forward、檢索面 `search_layer` 驗證），既有 `duduclaw-gateway`/
  `duduclaw-memory` 測試零回歸。
- **OS-native agent P3-1：VeriOS 情境五分類 ASK gate**：新增
  `duduclaw-gateway/src/situation_classifier.rs`——OS **行動類**工具（`os_open`，未來 L5b
  原生桌面動作）執行前先做情境五分類 `normal` / `anomaly` / `sensitive` / `missing_info` /
  `user_choice`，**分類標籤即決策依據，明確取代「用機率信心分數決定要不要問人」路線**
  （VeriOS arXiv:2509.07553；不用 confidence 的理由見
  `commercial/docs/research-os-native-agent-methodology.md` §④-5）。兩層分類器：**第一層**
  確定性零 LLM（target 落在敏感集合＝路徑含 credentials/keys/.env/.ssh/系統目錄、非 https URL、
  或被 perception sanitizer 標 suspicious → `sensitive`；缺／空 target → `missing_info`；含萬用字元或
  多候選 → `user_choice`；路徑比對 component-anchored 非裸 substring allow-check，方向 fail-safe），
  **第二層**規則判不出時一次 utility LLM 分類呼叫（account rotator，JSON 輸出，parse **fail-closed
  → `anomaly`**）。決策映射：`normal` → 放行（仍疊 ActionGuard 靜態 always-list，`merge_with_force_approval`
  取更嚴者）；`anomaly`/`sensitive` → ApprovalBroker 人工核准（走既有審批通道，**TTL 過期 = DENY**）；
  `missing_info`/`user_choice` → 不執行、回明確追問訊息給 agent 由上層 LLM 補參數。**每次分類寫
  `tool_calls.jsonl` 審計**（`situation_class` / `situation_source` / `situation_decision` /
  `force_approval`）。**兩套機制收斂定奪**（`os_open` 原走 ActionGuard maybe-irreversible，尚未接
  task-scoped grant 的欠帳）：兩閘**層次分明不平行**——依既有 MCP dispatch 順序，`[capabilities]
  scoped_tools` 若列了 OS 工具則 §3.65 task-scoped grant 閘**先行**（授權此 task 階段可用），本 ASK
  gate 於 §3.7 **其後**處理「授權已核發但此次呼叫情境異常」的殘餘問題；沒列則 ASK gate 全責。grant-gating
  維持 operator opt-in，不強制掛在 OS 工具上。`os_open` 由 ASK gate **取代**原 ActionGuard maybe-judge
  （殘餘情境仍只做一次 utility LLM 呼叫，無雙判），仍與 ActionGuard 靜態 `irreversible_tools` /
  `approval_required_tools` 取更嚴者。18 個模組單測（含 CJK 敏感路徑、注入樣本、正常樣本零誤殺、
  LLM parse fail-closed、決策映射與 merge）＋ 4 個 dispatch 級測試（missing_info/user_choice 追問、
  sensitive→approval 流活體核准 proceed、TTL=DENY）。
- **OS-native agent P3-3：輕量 CEP 時序 pattern matcher**：新增
  `duduclaw-gateway/src/cep_matcher.rs`——autopilot 規則 JSON 新增可選欄位 `sequence`
  （`{"first":{event,match},"then":{event,match},"within_secs":N,"negate":bool}`），
  「A 事件後 N 秒內出現 B」或（`negate=true`）「N 秒內未出現 B」的 in-process 時間窗匹配，
  **100% 確定性 Rust code，不引入任何串流平台（Kafka/Flink/Autogen）也不讓 LLM 生成時序邏輯**
  （arXiv:2501.00906 只借「事件序列模式」概念，`[verified-caveat]` 見
  `commercial/docs/research-os-native-agent-methodology.md` §3.1）。State 完全 in-process
  （`HashMap<rule_id, VecDeque<PendingMatch>>`）、每規則 pending 上限 100（超過丟最舊 + log，
  no-silent-caps）、30s tick 掃描 negate 到期。解析出的 pattern 以合成事件
  `AutopilotEvent::CepTrigger` 重新送回既有 autopilot broadcast bus，`AutopilotEngine` 對此變體
  走與一般規則完全相同的觸發尾段（三態斷路器 → `execute_action` → history/activity），因此
  `proactive_notify` 仍過 `ProactiveGate`、斷路器計數也共用同一計數路徑，未繞過任何既有 gate。
  `autopilot_store.rs` 新增 `sequence` 欄位（additive migration）；write-time 結構驗證（事件名／
  運算子／`within_secs` 範圍）擋掉打不到的規則於建立時，而非首次匹配才發現。24 個單元測試。
- **OS-native agent P3-4：`[os_watch] goal_template` 從檔案事件自主 kickoff goal loop**：`agent.toml
  [os_watch]` 新增可選 `goal_template`（`{path}`/`{file_name}`/`{kind}` 佔位符，沿用
  `AutopilotEvent::OsFileEvent` 已曝露給規則作者的同一組欄位名）與可選 `goal_acceptance`
  （缺省時渲染後的目標描述本身兼作驗收基準）。`os_file` 事件觸發時，`AutopilotEngine` 獨立於一般
  `trigger_event`/`conditions` 規則派發迴圈跑一次 kickoff 檢查：同 `(agent, path)` 10 分鐘內只允許
  成功建立一次（純函式防抖，只有真正建立成功才起算冷卻）→ 佔位符值（`path`/`file_name`/`kind`）
  全部先過 `sanitize_perception_text`（P2-5，含跨進程 `events.db` 橋接可能塞入的任意 `kind` 字串）
  → 渲染模板 → **過 `ProactiveGate`**（P3-4 是 P2-2 文件早已點名的「goal kickoff 前門」——沿用同一份
  `[proactive] enabled` 開關與每小時頻率上限，與 `proactive_notify` 共用同一預算；gate 未接線或
  `[proactive]` 未啟用 → deny-by-default 直接壓下，零 LLM 呼叫）→ Allow 才建立 `goal_mode=true` 的
  任務（`created_by = "goal:os_watch"` 區分於 `/goal` 聊天指令），完全走既有 `GoalLoopDriver` 的
  autonomy_level 核准流／iteration cap／MAV 判官驗收，不重新實作任何 kickoff 後治理。**P1 遺留小修**：
  `validate_autopilot_trigger_event` 的 `KNOWN` 清單一併補上 `os_file`/`os_frontmost`（engine 早就會
  發出這兩個事件，但 dashboard 建立的一般規則此前完全訂閱不到，與先前修過的 `run_at_risk` 同一類
  回歸）。31 個相關單元測試（防抖、CJK 路徑＋注入檔名消毒後渲染零外洩、無模板零成本 no-op、
  無 gate deny-by-default、`[proactive]` disabled 真實走 gate 零 LLM 呼叫、Allow/Suppress 分別驗證
  建立/不建立、dashboard 讀寫欄位同步、trigger_event 白名單修正）。
- **OS-native agent P3-5：隱私回歸案例納入 `duduclaw eval`**：新增 `evals/_privacy/README.md`
  索引 + 3 個獨立 Rust 整合測試檔（**刻意**放在被測 crate 的 `tests/`，不是被測原始檔自己的
  `#[cfg(test)]`，也不是 `duduclaw eval` TOML／transcript——後者是給「agent 行為」用的，這四條是
  gateway/security in-process 不變量，硬湊 transcript 等於零覆蓋，見 README 說明），16 案全綠：
  (a) `os_native=false`（缺／明確 false／TOML 損毀）→ 6 個 `os_*` MCP 工具全數 fail-closed 拒絕
  （`crates/duduclaw-cli/tests/privacy_regression_os_native_gate.rs`，4 案，走真實
  `McpDispatcher::dispatch_tool_call`）；(b) 注入檔名（instruction-override、`<system>`／ChatML／
  `[INST]` 標記）經 `sanitize_perception_text` 中和後不含原始結構性突破字元，含正常 CJK 檔名零誤殺
  對照（`crates/duduclaw-cli/tests/privacy_regression_perception_neutralize.rs`，4 案）；
  (c) `[proactive] enabled=false` 不消耗 LLM 呼叫即壓下、LLM 錯誤／無法解析／超出值域皆
  fail-closed 壓下，含高分放行正對照與注入事件文字不繞過（`crates/duduclaw-gateway/tests/
  privacy_regression_proactive_gate.rs`，6 案，走真實 `ProactiveGate::evaluate_with`）；
  (d) `os_notify` 注入載荷中和但不丟棄、寫入 `security_audit.jsonl`，乾淨內容零審計雜訊
  （同 (b) 檔案，2 案；刻意不跑真實 `osascript` 發送以免每次 `cargo test` 跳出真實桌面通知，
  改直接呼叫生產程式碼實際串接的 `sanitize_perception_text` → `sanitize_osascript` →
  `audit::log_injection_detected` 三段）。未觸碰任何 gateway/security 生產程式碼。
- **OS-native agent P2-1：interruptibility 打擾成本分數**：新增
  `duduclaw-gateway/src/interruptibility.rs`——`InterruptibilityTracker` 訂閱既有 autopilot
  broadcast（`os_frontmost` / `os_file` / `agent_idle`），維護每 agent 15 分鐘滑動視窗，
  `score() -> 0.0..=1.0`（0=可打擾、1=勿擾）。確定性公式：switch 頻率權重最高（0.6，依 CHI'18
  「切換頻率為最強訊號」）＋ file 密度（0.4）＋ idle 乘法 relief；無訊號回中性 0.5。**只讀**
  既有 `AgentIdle` 訊號、不自建第二條 idle 判斷源。8 個單測。
- **OS-native agent P2-2：ProactiveGate 主動介入閘門**：新增
  `duduclaw-gateway/src/proactive_gate.rs` ＋ autopilot 新 action `proactive_notify`（既有
  `notify`/`delegate`/`run_skill` 確定性規則不受影響）。感知文字先過 `sanitize_perception_text`
  → 組 prompt（sanitized 事件 XML DATA + persona 偏好 `search_facts` Ebbinghaus 排序 +
  interruptibility）→ 一次 utility LLM 呼叫（account rotator）算 `proactive_score` 1–5，JSON
  parse fail-closed → 動態閾值 `𝒯ℛ = base(3) + round(interruptibility × 2)` → `≥𝒯ℛ` 才放行原
  notify，否則壓下。**fail-closed**：LLM error / 30s timeout / parse fail → 不打擾。每次決策寫
  `<home>/proactive_gate.jsonl`（`with_file_lock`，含保留給 P2-3 的 `outcome` 欄位）。
  `[proactive]` 新表（`enabled=false` deny-by-default / `base_threshold` / `max_per_hour`
  頻率上限）。MetaCognition base 校準以「可注入 base + `metacognition_base()` mapping + 掛鉤點」
  最小接入。12 個單測。
- **OS-native agent P2-3：四象限成效追蹤 + 校準回饋**：新增
  `duduclaw-gateway/src/proactive_feedback.rs`——把 P2-2 保留的 `proactive_gate.jsonl`
  `outcome=null` 回填成 ProactiveAgent（arXiv:2410.12361）四象限：`correct_detection` /
  `false_alarm`（allow 決策 + 顯式 dismiss 或 session 活動判定，取自既有 `feedback.jsonl` /
  session 資料，訊號缺失一律 `unknown` 不腦補）、`missed_need` / `correct_silence`（suppress
  決策 + 使用者是否於窗內對同 agent 發起事件關鍵詞相關請求，CJK-safe `word_contains_ci`）。
  每 60 秒背景迴圈掃描（只對尾部 500 行嘗試訊號蒐集，冪等回填），`quadrant_stats()` 聚合 + 新
  evolution event（`proactive_quadrant`）。**校準回饋**：False-Alarm / Missed-Need 率 EMA
  平滑後透過既有 `metacognition_base()` mapping 換算，`published_base` 每 UTC 日曆天最多移動
  ±1，存 `<home>/proactive_calibration.json`；`autopilot_engine.rs::action_proactive_notify`
  改讀 `effective_proactive_config()` 疊加校準值（`proactive_gate::read_proactive_config` 本身
  零改動，12 個既有測試零回歸）。25 個新單測，`cargo test -p duduclaw-gateway --lib -- proactive`
  全綠（37 個）。
- **OS-native agent P2-4：結構化感知源**：`duduclaw-os` 新增三個唯讀 shell-out 模組——
  `frontmost.rs`（macOS `osascript`/System Events 取前景 app + 視窗標題，Linux
  `xdotool` 取視窗標題，Windows 不支援）、`spotlight.rs`（`mdfind` 包裝，query 一律走
  argv 陣列不經 shell、上限 20/200 筆、scope 目錄需存在）、`calendar.rs`（`osascript -l
  JavaScript` 唯讀取今日行事曆事件，JXA 腳本為固定字面量無注入面）。三者皆
  CJK-safe 截斷（`truncate_chars`），TCC 拒絕從 stderr 分類為 `PermissionDenied`。
  Gateway 新增 `os_frontmost.rs`：per-agent `[os_watch] frontmost_poll_secs`
  （opt-in，0/缺省=不輪詢）低頻輪詢前景視窗，僅在 app/標題實際變化時發出新
  `AutopilotEvent::OsFrontmostEvent`（`os_frontmost` 觸發器），是純感知訊號、
  不另開 idle 判斷源。MCP 新增三個唯讀工具 `os_frontmost` / `os_spotlight_search`
  / `os_calendar_today`（`Scope::OsNative` + `[capabilities] os_native` 閘控，同
  P1 工具的 dispatch 主閘，無 ActionGuard）。`duduclaw os doctor` 增加 System
  Events 自動化權限、行事曆權限（皆為活體試呼叫）、`mdfind` 可用性三項檢查，
  缺權限時輸出系統設定路徑指引，不嘗試繞過。

### Fixed
- **空回覆靜默斷鏈（經銷商回報，Grok 幾乎必現）**：非 Claude runtime（Grok CLI /
  OpenAI-compat（xAI）/ Codex / Gemini / Antigravity）回傳空內容時，先前被當作
  「成功」——failover 記成健康、通道端（Telegram/WebChat 等全通道）直接跳過空訊息
  不發送，使用者看到的就是「已讀不回」；且空的 assistant turn 被寫入 session 歷史，
  下一輪帶著空 turn 送回上游，模型學著繼續回空——session 鏈自此斷裂。修正四層：
  ① `failover.rs` 將「Ok 但空內容」視為失敗（觸發 fallback runtime，雙雙落空則回
  可分類的 `Empty response` 錯誤）；② OpenAI-compat runtime 空 `content` 改回 Err，
  診斷附 `finish_reason` 與 `reasoning_content` 長度（Grok/DeepSeek 推理模型把
  8192 max_tokens 全燒在思考、正文為空即 `finish_reason=length`），且組歷史時過濾
  空 turn（已污染的 session 自動復原）；③ 四個 CLI runtime（grok/codex/gemini/agy）
  exit 0 但 stdout 為空改回 Err 並附 stderr tail；④ `channel_reply` 匯流點最後防線：
  空回覆一律轉入 fallback 鏈，使用者收到分類後的「空回應」錯誤訊息，事件寫入
  `channel_failures.jsonl`，不再無聲失蹤。
- **failover 靜默頂替無可觀測性（同一經銷商實測：容器內無 `grok` CLI，`GrokRuntime`
  未註冊，failover 靜默頂替成 Claude 回答，使用者以為在跟 Grok 對話）**：
  ① `failover.rs::execute_with_failover` 的 `registry.get(primary)` 回 `None`（CLI
  未安裝／偵測失敗）先前完全無聲直接落到 fallback；現在會 `warn!` 並呼叫
  `record_failover()` 記入 `duduclaw_failover_total` 指標，且以 `reason` 欄位
  區分「未註冊」（`not_registered`）與既有的「執行失敗」（`execution_failed`）
  「空回應」（`empty_response`）三種 fallback 成因；fallback 端同樣未註冊時也補
  `warn!`。② `channel_reply.rs` 非 Claude 分支改用 `run_agent_prompt`（保留完整
  `RuntimeResponse`，不再用丟棄 metadata 的 `run_agent_prompt_text`）：當實際回答
  的 `runtime_name` 與 agent 設定的 provider 不一致時，記一行
  `channel_failures.jsonl`（`event: "runtime_fallback_substitution"`，含
  requested/actual/agent/session）＋ `warn!`，並在有 `on_progress` 時發
  `ProgressEvent::ModelInfo { model: "<實際模型>（備援）" }`，讓 WebChat 顯示
  「備援」而非讓使用者誤以為在跟原設定的模型對話；provider 一致時行為不變。

### Security
- **感知輸入安全（OS 原生 Agent P2-5，indirect prompt injection 防護）**：所有 OS 感知取得的
  文字（檔名/路徑/通知文字，未來的視窗標題/行事曆/搜尋結果）一律降格為不可信 DATA。新增
  `duduclaw-security::perception::sanitize_perception_text`（純函式）：CJK-safe 截斷 → 剝控制
  字元/ANSI/零寬 → 複用既有 `input_guard` 規則引擎＋新增「檔名即攻擊面」規則類
  （`filename_role_marker` 抓 `<system>`/ChatML/`[INST]`、`filename_tool_call` 抓 tool-call 樣式
  JSON）→ **中和不阻擋**：角括號 defang、標記 `suspicious`＋附 warning，fail-closed（全被剝光
  → placeholder，絕不回原文，IPIGuard 2508.15310 / Firewalls 2510.05244 精神）。接線三處：
  ① autopilot `os_file` 事件的 `path`/`file_name` 進 delegate/notify/run_skill prompt 前清洗並前置
  安全 banner——**規則匹配走原文、進 prompt 走清洗後文字，兩者分離**；② `os_notify` MCP 的
  `title`/`body` 在 dispatch 層過同一清洗（防污染 agent 用通知社工使用者），仍發送中和後文字；
  命中一律寫 `security_audit.jsonl`（warning 級、不阻擋事件）。正常中英文/CJK 檔名零誤殺（測試涵蓋）。
- **Sensitivity label + context-collapse 防護（OS 原生 Agent P3-2，最小版）**：防止 agent 把某位
  使用者的個人 context 縫進其他人看得到的群聊 prompt（Local-Is-Not-Sufficient arXiv:2606.10173
  §5.1 六風險之 **context collapse**）。新增 `duduclaw_core::Sensitivity`（`Public < Internal <
  Personal < Restricted`，serde 小寫）＋感知源常數表（os_file/spotlight=Internal、frontmost/
  calendar=Personal、clipboard/screen=Restricted、未知源 fail-closed=Personal）。新增純函式
  `duduclaw_core::is_private_session(session_id, user_id)`——**fail-closed** 判定 1:1 私聊 vs
  群組/共享 session（`slack:group:`/`discord:thread:`/telegram 負數 chat id 為群組標記；否則
  「群組 id 與 user id 不同構」，`rest == user_id` 才判私聊；discord/feishu/gchat/teams 無法證明私聊
  → 當群聊）。`channel_reply` 每輪算一次 `is_private`：`## Key Facts About This User` 與
  `## About This User` 兩個 persona 塊在群聊 session **完全不注入**（並 `debug!` 記剝除）；wiki 注入
  `ranked_wiki_injection` 收 `allow_personal`，`.scope.toml` 標 `sensitivity = "personal"/"restricted"`
  的 namespace 在群聊剝除整個 namespace 的頁（仍可搜尋），私聊/群聊各自 cache key 不互污。
  Memory 面 additive：`duduclaw_memory::sensitivity::{stamp_metadata, read_from_metadata}` 把分級
  存進既有 `metadata` JSON blob（不動 schema，同 origin binding 慣例；讀取預設 Internal 保舊資料相容）。
  ProactiveGate 提供 `persona_lines_for_destination` 純helper（目的地非私聊回空 persona）。`.scope.toml`
  `sensitivity` 沿用 `knowledge_owner` 同一表與解析慣例，malformed → fail-safe。26 個新單測（core 17／
  memory 5／gateway 4，含各通道私聊判定、感知表、malformed fail-safe）＋既有 channel_reply/proactive_gate/
  ranked_wiki 測試零回歸。**未竟**：剪貼簿/螢幕感知源本身、感知→memory 寫入打標（P4-4）、per-user
  加密、ProactiveGate 目的地隱私接線——本輪只做 context-collapse 一條最小閉環。

## [1.41.0] - 2026-07-22 — 信任記憶強化、OS 原生整合 Phase 1 與 WebChat model 顯示根治

### Added
- **OS-native agent Phase 1**：新增 `duduclaw-os` crate——跨平台檔案系統監看
  （notify）、原生桌面通知、開啟目標 helper。Gateway `os_events.rs` 將 per-agent
  `[os_watch]` 檔案事件串入 autopilot bus（新 `os_file` 觸發器 + stats 檔），由
  opt-in `[capabilities] os_native` 閘控（預設關閉；未開啟時 `os_notify` /
  `os_watch_status` / `os_open` MCP 工具在 dispatch 主閘直接拒絕）。CLI 新增
  `duduclaw os` 子指令（通知 helper、doctor 診斷）；dashboard agent 表單新增 os_native
  開關 + `[os_watch]` 編輯器（i18n en/ja/zh-TW），設定變更透過既有 `agents.update`
  RPC 即時熱重載對應 watcher，不需重啟 gateway。另外 Settings › Automation 分頁
  （既有的 goal-loop / dispatch / topology 等自動化設定頁，與 os_native 無關）本次
  透過 `system.update_config` 加上熱重載能力。

- **記憶寫入來源綁定（TMA-NM，arXiv:2606.24322）**：新增 `duduclaw-memory` origin 分類表
  （`origin.rs`，8 類 + trust 天花板），`store_temporal` 強制 non-malleable 上限——
  最終 `origin_trust = min(呼叫端值, 該來源類別天花板, derived_from 最小值)`，呼叫端
  只能調低不能超標；未標註來源一律落 `unattributed`（0.6），不再預設滿信任。
  reaffirm 佐證改為 Sybil-resistant：僅 ≥2 個相異且非自我衍生的來源類別可小幅上調
  confidence（+0.1/次，cap 1.0），agent 自我摘要與工具回聲互相佐證不再加分。
  全部 production 寫入路徑（MCP `memory_store`、`log_mood`、GVU StoreEpisodic、
  reflexion 整併、decision capture、night engine、批次匯入）補上顯式 origin。
- **階段性可撤銷 capability（PORTICO，arXiv:2606.22504）**：`agent.toml [capabilities]
  scoped_tools` 清單中的工具改為「持有效 grant 才可用」。新增 `capability_grants.rs`
  grant store（`approvals.db`，fail-closed）、MCP 工具 `capability_request`（走
  ApprovalBroker 人工核准，TTL 預設 1h 可由 `grant_ttl_secs` 覆寫）、goal kickoff
  核准時可依 task tags `grant:<tool>` 原子性授予。任務終態（accept / reject /
  needs_human / cancel / escalate）自動撤銷該 task 的全部 grant——授權活不過任務階段。
  MCP dispatch 主閘 + CLI spawn disallowedTools 雙層強制執行。
- **eval trace-grounding 斷言（GroundEval，arXiv:2606.22737）**：`duduclaw eval` 新增
  `[[expect.grounded]]` 確定性斷言——要求指定工具有 ≥1 次非錯誤呼叫，且最終回答與
  該工具的 result 共享 ≥N 字元連續片段（CJK-safe），可選 `output_regex` 要求命中片段
  出現在工具結果中。transcript parser 補上 `tool_use`↔`tool_result` 配對（舊 transcript
  相容）。失敗歸類 MAST FM-3.3（驗證缺失）。
- **MAV 判官證據區塊**：goal task 驗收判官 prompt 附上 `<tool_activity>`（自
  `tool_calls.jsonl` 審計取 claim→review 時間窗的工具活動摘要）；correctness 面向指令
  明確要求「worker 聲稱的動作未出現在 tool_activity 視為未證實」，堵住自報 result_summary
  無法查證的盲點。

### Changed
- **規則整併加 GovMem 晉升門（arXiv:2607.02579）**：reflexion 整併前按 `source_kind`
  分組獨立計數（RFC-24 決策缺失與一般任務失敗不再互相湊數），且需 ≥2 個相異 session
  與 ≥2 種相異錯誤描述才視為獨立佐證充分——同一 session 連續重複的相關觀測不再被
  當成獨立證據觸發整併。`mistakes` 表新增 `source_kind` 欄（idempotent migration）。
- **新規則試用期（Janus，arXiv:2606.31121）**：整併規則 seed 由 helpful=2 降為 1 並帶
  `probation-rule` 標籤；試用期內首次 harmful 即退休、累計 helpful≥3 轉正；注入排序
  同分時試用規則排後。壞規則從「需扣兩次才退場」變為「一次即退場」。
- **cache-aware 壓縮守門（arXiv:2607.12161 實證：激進壓縮破壞 prompt cache 反而更貴）**：
  `maybe_compress_history` 在 agent 近 1h cache 效率 >50% 且預算超標 <15% 時跳過壓縮
  管線（門檻可由 `[budget] cache_guard_min_eff` / `cache_guard_max_overshoot` 覆寫，
  0 停用）。`token_usage` 表新增 `compressed` / `compression_stages` 欄；新增
  Prometheus 計數器（壓縮次數 per stage、守門跳過、疑似 cache-break）；
  `cache_attribution_snapshot()` 接上消費者（每小時 adaptive routing check 記錄
  top-10 破 cache 原因 + evolution event）。

### Fixed
- **WebChat model 顯示與 Agent 設定不一致（經銷商回報）**：四項根治——
  ① `agents.update` 後的 registry re-scan 從 500ms best-effort 改為保證完成
  （無條件取寫鎖 + 3 次重試；gateway 無週期性 rescan，漏掉一次就永久顯示舊
  model 直到重啟）；② 設定變更即時廣播（`agent_config_events`）至活躍 WebChat
  連線，重發帶 `refresh: true` 的 `session_info`——開著的分頁立即更新名稱/圖示/
  model，且前端只更新 agent metadata、不動 session 狀態；③ 新增 dashboard-only
  `ProgressEvent::ModelInfo`：解析 stream-json `message.model`（反映 CLI 端替換），
  WebChat 蓋章到 `assistant_done.model`，UI 以實際值優先於設定意圖，文字頻道比照
  Step 忽略；④ 無 agent 時的 hardcoded fallback `claude-sonnet-4-20250514` 統一為
  `DEFAULT_PREFERRED_MODEL`（`claude-sonnet-4-6`，與 scaffold 預設一致），
  `session_info` 同步顯示該值而非空白。

### Security
- 記憶投毒防護強化：見 Added 的來源綁定（寫入端）與 GovMem 晉升門（整併端）。
  scoped_tools 授權閘全程 fail-closed（grant store 不可讀 = 無授權）。

## [1.40.0] - 2026-07-21 — 經銷商實測修復：遠端存取白名單、PTY 韌性與 WebChat 對話隔離

### Fixed
- **中文輸入法（注音/拼音）打字時 Enter 誤送半截訊息 → 組字期間的 Enter 不觸發送出。**
  IME 組字時第一次 Enter 是「選字/確認」卻被當成送出，訊息只送一半。新增共用工具
  `web/src/lib/keyboard.ts` 的 `isImeComposing(e)`（同時檢查 `nativeEvent.isComposing`
  與 Safari 的 `keyCode === 229` 邊緣行為），所有「Enter 送出/確認」的自由文字輸入框
  都改為 `e.key === 'Enter' && !isImeComposing(e)` 才動作。掃同類共修 14 個檔案 15 處：
  WebChat 輸入框、workspace PromptBar、標籤 ChipEditor、InlineEditor 改名、CommandPalette
  篩選、Skill 市集搜尋＋repo URL、共享 Wiki 搜尋、Identity 解析、MCP fetch、部門建立、
  Knowledge 策展查詢×2、Knowledge Hub 搜尋、系統設定遠端存取白名單、CLI 登入回應。
  純按鈕啟用（Enter/Space 當點擊，如 Logs/Onboard/Mascot）與全域快捷鍵監聽
  （CommandPalette/ConnectorChips/AgentModelPicker）、以及會排除可編輯目標的 InboxList
  導覽鍵不涉及 IME，未動。
- **WebChat「新對話」完成後不會即時出現在對話列表、切回舊對話就回不去 → 每則回覆入庫後刷新列表。**
  承接上一版 conv-nonce 架構：新對話 B 的 session bucket（`…#conv:<nonce>`）在伺服器端
  存在且可 resume，但左側列表沒刷新、B 進不了列表就無法點回。修法：store 新增
  `sessionsRevision` 計數器，每收到一則 `assistant_done`（含被歸屬守衛丟棄的其他對話）
  即 +1；WebChat 頁面 watch 此值刷新 `chat.sessions.list`，讓剛建立的對話在第一則回覆
  落地時就進列表並保持可 resume。已知殘留（cosmetic）：目前開啟中的新對話因 client 端
  `sessionId` 仍為連線基底 id、列表列為 composed id，列表「使用中」高亮不會標到它——
  不影響點選 resume 與續聊。
- **WebChat「新對話」回覆錯投到新對話 → 每個對話有獨立 session bucket＋回覆按對話歸屬。**
  修復在對話 A 發長任務、任務進行中按「新對話」開對話 B 時，A 的回覆完成後出現在 B
  裡的問題。根因：WebChat 的 server session 綁「WS 連線 id」而非「對話」，且 `/new`
  是 `delete_session` 原地刪除、不輪替新 id——A 與 B 共用同一個 session bucket，
  in-flight 回覆完成時就投到目前打開的對話。修法：
  - **前端每個對話帶一個 conversation nonce（`conv`）。** `user_message` 帶上 `conv`，
    在 `/new`、切換 AI 員工、resume 歷史對話時輪替。回覆 frame（`assistant_done` /
    `progress` / `step`）由伺服器原樣回帶 `conv`；socket 收到與目前對話 `conv` 不符的
    frame 直接丟棄不渲染（typing/任務板進度/工具步驟同樣受此閘門保護，不會錯投）。
  - **伺服器端每個對話獨立 session bucket。** new/continue 路徑把消毒過的 `conv`
    併入 session id（`…#conv:<nonce>`），A 的 in-flight 回覆持久化到 A 的歷史、
    不再混進剛開的 B。`sanitize_conv_nonce` 只保留 `[A-Za-z0-9_-]`、限長 64、
    去除 `:`/`#` 結構分隔符（防注入額外 bucket 結構）；resume-ownership 守衛
    （`starts_with("{session_id}#")`）仍接受這些 id。缺 `conv`（舊 client）→
    byte-compatible 單 bucket 舊行為。
  - **「新對話」按鈕改為輪替 nonce、不再送 `/new`。** 舊行為會 `delete_session`
    刪掉可能仍在跑長任務的對話；新行為改為開一個空的新 bucket（AI 自然從乾淨脈絡
    開始），並保留舊對話在「對話列表」可 resume。連帶讓 WebChat 的過往對話列表
    真正可用（先前所有對話共用一個 id，列表形同只有一筆）。`/new` 指令本身不變，
    其他頻道與手動輸入 `/new` 照舊。
- **PTY pool 互動 REPL 卡住抓不到回覆 → 快速失敗並降級。** 修復 `[runtime]
  pty_pool_enabled = true` + OAuth 訂閱帳號時，channel 回覆走互動 REPL 卻抓不到
  sentinel 回覆、一直轉到 30 分鐘才逾時的問題群：
  - **互動 REPL 逾時改為「停滯偵測（stall detection）＋寬鬆硬上限」**（取代原本
    固定 180s deadline，見下方 Changed）。新增可設定的 idle/停滯視窗
    （`agent.toml [runtime] pty_idle_timeout_secs`，或 env
    `DUDUCLAW_PTY_IDLE_TIMEOUT_SECS`，缺省 **120s**）：互動 REPL 只在**連續無實質
    進度**達 idle 視窗才快速失敗降級，長任務（多分鐘工具呼叫／agentic 工作）不再被
    誤殺。「實質進度」以 **token 計數上升 or 去噪 prose 內容變化**判定（spinner 動畫
    ＋每秒跳動的經過計時器**不算**進度——依 Claude Code 2.1.173 活體擷取校準）。
    `pty_interactive_timeout_secs` 改為**絕對硬上限安全網**（缺省 **1800s**）。
    API-key 的 `claude -p` one-shot 路徑維持原本的長 deadline。
  - **fallback 紀錄新增 `reason` ＋ `mid_task` 欄位。** `channel_failures.jsonl` 的
    `pty_pool_fallback` 事件標明失敗原因（`stall`／`hard_cap`／`boot`／`other`）；
    停滯或硬上限發生在**已觀察到進度之後**（任務執行中段）時 `mid_task=true`，並額外
    warn「task may have partially executed」（fallback 重跑可能重複副作用，仍以可用性
    優先照樣降級）。`InvokeStall`／`InvokeHardCap` 兩個新錯誤型別可讓上層分類。
  - **多行 prompt 送不出去 → 停滯（submit watchdog）。** 多行 prompt 以
    bracketed-paste 寫入後緊接的 `\r`，實測對真 claude 2.1.173 有約 3/4 機率**不會觸發
    送出**（TUI 還在吃 paste，`\r` 被丟棄），prompt 躺在輸入框、REPL 空等到停滯逾時。
    `collect_response_interactive` 新增 submit watchdog：送出後若 1.5s 內未見「turn
    正在跑」的跡象（spinner／`esc to interrupt`／sentinel），重送 `\r`，有界 3 次、
    間隔遞增。活體驗證：watchdog 送出成功率 4/4（原 1/4）。
  - **首回合 TUI welcome box 被當成回覆送出（fail-closed 過濾）。** fresh session
    首回合的整屏重繪會把「Welcome back／What's new／release-notes／org email／agent
    路徑」的 welcome box 夾進兩個 sentinel 之間，舊的 chrome filter 不認得而把它當答案
    送給使用者。改法：payload 逐行過濾時，**任何 box-drawing／block 字元
    （U+2500–U+259F）或 welcome 關鍵字（Welcome back／What's new／release-notes…）
    的行一律視為 chrome**；前導 welcome chrome 略過（後面真答案保留），welcome 出現在
    答案之後則停止收集。全被濾掉時回空 payload → 觸發既有 empty-payload retry／
    fallback（寧可空也不送 chrome）。fixture 取自 2.1.173 活體擷取。另補 composer
    輸入框狀態列（`ctrl+g to edit in Vim`、`⏵⏵ automode on (shift+tab to cycle)`、
    `← for agents`、`? for shortcuts` 等）——這些會黏在答案末尾漏出；改用**整行去空白
    後精確相等／符號前綴**比對（非子字串），確保「答案內文真的在講 vim 快捷鍵」不被誤殺。
  - **互動 REPL 失敗自動 fallback 到 fresh-spawn `claude -p`。**
    `call_claude_cli_pty_rotated` 在 pool 路徑回可復原錯誤（逾時／空 payload／boot
    失敗／帳號耗盡）時，改走與 `FreshSpawn` 同源的 `call_claude_cli_rotated`，並記
    warn log ＋寫一筆 `pty_pool_fallback` 到 `channel_failures.jsonl`（不再靜默失敗）。
    MoA 設定錯誤不 fallback（fresh-spawn 同樣會拒）。
  - **Boot dance 補指紋並改為快速失敗。** 互動 boot 除 trust 對話框外，新增
    theme picker／onboarding／login-method 首次啟動畫面的偵測（去 ANSI ＋去空白
    ＋小寫比對，送 `\r` 接受預設值）；boot 結束仍未見 REPL-ready 指紋即把 session
    標記不健康並回 `BootTimeout`（取代舊的硬 proceed），讓上層走上述 fallback，把
    「卡 30 分鐘」變成「數秒失敗＋降級可用」。
  - **REPL-ready 指紋更新至 Claude Code 2.1.x ＋ MCP 核准畫面處理**（活體驗證
    2.1.173）。舊指紋（`? for shortcuts`／`Try "edit`）在 2.1.x TUI 已不存在，
    導致 REPL 明明就緒仍判 boot 逾時；補上 `Try "how does`／`shift+tab to cycle`。
    另新增「New MCP server found in this project」核准畫面的顯式指紋，`\r` 接受
    預設選項（Use this MCP server）。
  - **補 dispatcher 路徑的 per-account 憑證注入（Gap A）。** sub-agent dispatch
    走 PTY pool 時，`AcquireOptions` 先前沒帶 `account_id`／`env`，會用到 ambient
    OAuth；改為比照 channel 路徑用 `rotate_cli_spawn` 逐帳號注入 env ＋ account_id。
  - **keychain 預設 OAuth 帳號給穩定 account_id（Gap B）。** rotator 對預設 keychain
    帳號只發空字串 `ANTHROPIC_API_KEY` force-OAuth sentinel；先前 `account_id` 解析回
    None 導致 env 不被 stash／注入，PTY child 可能繼承 gateway 殘留的 API key 而蓋掉
    OAuth。改為給它 `oauth-keychain-default` 穩定 id，使 sentinel env 被注入。

### Changed
- **PTY pool 重新定位為「備援、預設關」並補上已知限制說明。** Anthropic 原訂 2026-06-15
  把程式化用量（`claude -p` / Agent SDK / GitHub Actions）拆到獨立 Agent SDK credit，
  但**已於當天暫停**，`claude -p` 對 OAuth 訂閱帳號照舊可用 → 預設的 `FreshSpawn` 路徑
  完整可用、PTY pool 非必要。功能維持保留、預設關（`pty_pool_enabled` 缺省 false，
  `runtime_mode_for_agent` fail-safe 回 `FreshSpawn`），文件（`docs/features/27-pty-pool-runtime.md`
  含 zh-TW/ja-JP、`CLAUDE.md`）新增「何時才需要開」與**已知限制**：pool session 以
  `(agent, cli_kind, bare_mode, account, model)` 為 key、**不含對話維度**，多對話 agent
  會跨對話共用同一條 REPL 而洩漏脈絡——開啟前必須理解此行為。**預設 fresh-spawn `claude -p`
  路徑不受影響**：其脈絡完全來自 `get_messages(session_id)`、session id 逐對話
  （WebChat 含 `#conv:<nonce>`），已驗證無跨對話洩漏；`--resume` 確定性 session id 路徑
  早已移除（所有呼叫點傳 `None`）。附帶把 WebChat 的 session-id 組合抽成純函式
  `compose_session_id` 並加單元測試鎖住「conv nonce 參與分桶」不變式。
- **`agent.toml [runtime] pty_interactive_timeout_secs` 語意變更：從「固定殺 turn 的
  invoke deadline」改為「絕對硬上限安全網」，缺省值 180s → **1800s**（對齊 fresh-spawn
  的 `HARD_MAX_TIMEOUT`）。** 日常「session 是否卡住」的判定改由新的停滯偵測負責（新設
  定 `pty_idle_timeout_secs`，缺省 120s，見 Fixed）。**影響**：先前靠此值在 180s 主動
  殺掉長任務的使用者，現在長任務會一直跑到 30 分鐘硬上限或先被停滯偵測攔下——若要回到
  舊的積極上限，自行把 `pty_interactive_timeout_secs` 設回 180。managed-worker 的
  per-invoke 硬上限 clamp 也從 10 分鐘提高到 31 分鐘以容納新的硬上限；`InvokeParams`
  新增 `idle_timeout_ms`（向後相容，舊 client 省略時只套硬上限）。

### Added
- **Dashboard 即時連線的可設定 Origin 白名單。** 新增 config.toml
  `[gateway] allowed_origins`（陣列，元素可為 `host`、`host:port` 或含 scheme 的
  完整 origin）與環境變數 `DUDUCLAW_ALLOWED_ORIGINS`（逗號分隔，兩者**合併**）。
  解決經銷商／使用者透過 tailnet（`*.ts.net`）或反向代理網域開 dashboard 時，HTTP
  頁面正常但 WebSocket 升級被 403 擋掉一直轉圈圈的問題。內建 loopback 三項
  （`localhost` / `127.0.0.1` / `[::1]`）永遠有效；清單為空時行為與舊版
  byte-identical（零回歸、fail-closed）。每個項目做精確 authority 比對，不支援
  萬用字元，後綴攻擊（`localhost.evil.com`）仍被擋。啟動時印一行 info log 列出
  生效的額外 origins。文件見 `docs/guides/deployment-guide.md` §5 與
  `docs/guides/docker.md` §13。
- **Dashboard 設定頁可直接管理 Origin 白名單。** 設定 → 系統 → 遠端存取網址提供
  新增／刪除 chip 介面（`system.config` / `system.update_config` RPC），存檔後
  透過 `set_allowed_origins` **熱生效、免重啟** gateway；`DUDUCLAW_ALLOWED_ORIGINS`
  環境變數提供的項目在 UI 存檔時會重新併入、不被洗掉。經銷商／使用者不必再手改
  config.toml。

## [1.39.0] - 2026-07-20 — Graph Engineering — 雙時間軸記憶、投毒防護、知識圖策展

### Added
- **記憶系統升級為雙時間軸（bi-temporal）＋建構期溯源（D1）。** 每筆事實新增
  `ingested_at`（交易時間軸：系統何時得知，與「事實何時在真實世界成立」的
  `valid_from` 分離）；supersession 發生時在被取代的舊列記錄
  `invalidated_by_event`／`invalidated_at`（是哪則來源事件、何時把它失效）。
  同一 `(subject, predicate)` 且 object 與內容實質相同的事實重複出現時不再新增列，
  改在既有列的 metadata 記 `reaffirmed_by`（上限 20 筆）並累加 access_count，
  避免記憶膨脹。
- **新增 `invalidate_by_origin` 按來源回滾原語**（engine + MCP `memory_invalidate_by_origin`）：
  一次 expire（非刪除）某來源（精確相等比對，非子字串）自某時刻起的全部
  currently-valid 事實，並沿 `derived_from` 級聯把衍生事實的信任度降到 ≤ 0.1。
  history 完整保留、仍可查。這是偵測到來源投毒後的止血閥（Admin scope）。
- **新增 MCP 工具 `memory_get_history` / `memory_get_at`**：查詢某三元組
  `(subject, predicate)` 的完整 supersession 鏈與任一時點的有效事實（原本 engine-only）。
- **知識寫入投毒防護管線（D2，對應 PoisonedRAG 2402.07867）。** 自動蒸餾寫入前，
  對每筆事實的內容與 `(subject, predicate, object)` 跑既有的注入規則引擎；命中即
  「不寫入」（fail-closed）並記 `prompt_injection` 稽核事件。新增同源突增偵測
  `knowledge_guard`（沿用 dispatch_guard 的滑窗＋跨行程 advisory lock 模式）：同一
  `(agent, origin, subject)` 在窗內寫入 ≥ `max_per_subject` 筆即把該批標為隔離。
  `config.toml [knowledge_guard]`（`enabled`／`window_secs`／`max_per_subject`，缺省
  一律退回內建預設）。
- **記憶列新增 `quarantined` 欄（idempotent migration）。** 隔離中的事實一律
  inert——不 supersede 任何現有事實，且被所有檢索讀路徑排除（FTS、graph、vector、
  `search`／`search_layer`／`list_recent`／`summarize`／`list_valid_by_source_event`）。
  依 id 明確取用的 `get_by_id`／`get_by_ids` 與溯源檢視 `get_history`／`get_at` 仍可回傳。
- **隔離處置走 ApprovalBroker**（`action_kind = "knowledge_quarantine"`）＋發
  `knowledge.quarantined` 事件到 events.db。核准 → 解除隔離（`quarantined = 0`，恢復可
  檢索）；拒絕 → expire（`invalidated_by_event = "quarantine_reject"`）並把 `origin_trust`
  降到 ≤ 0.1；TTL 過期 = 拒絕（fail-closed）。dashboard `approvals.decide` 已接上此側效。
- **記憶知識圖檢索演進（D3，對應 HippoRAG 2 / LightRAG）。** ① 每 agent 的 SPO 圖
  改為持久快取（generation counter 失效，>500 條三元組才啟用；快取命中的排序與
  現建 byte-identical，位元級測試驗證）；② 實體別名歸併：新 `entity_alias` 表把
  「老闆／李老闆」等表面形式收斂到同一節點，提升建圖與播種命中率，新增 MCP 工具
  `memory_alias_add`（write scope）／`memory_alias_list`（read scope）；③ 述語（predicate）
  以邊標籤附掛入圖（PPR 分數不變），並新增 `engine.export_graph(agent, limit)` 可序列化
  快照（含隔離標記）供策展 UI 使用；④ 可選 embedding 播種（`[memory] graph_embed_seed`，
  預設關閉）：啟用且掛 embedder 時以 query 向量對實體向量取 top-k 聯集擴充 FTS 播種，
  關閉或無 embedder 時 byte-identical。
- **Goal loop 平行派工 DAG（D4，LLMCompiler 式）。** 可選 planner
  （`[goal_loop] planner_enabled`，預設關閉）把 goal 拆為帶依賴標注的子任務 DAG；
  依賴全部完成的子任務平行派發（仍受 `max_concurrent` 與 dispatch_guard 約束）。
  循環／無效計畫整包拒絕、退回單任務模式；上游依賴失敗或升級人工時，下游任務
  繼承升級為 needs_human（不孤兒化）。
- **可插拔派工策略（D4）。** `config.toml [dispatch] policy`：`fixed_hierarchy`（預設，
  行為與先前一致）／`round_robin`（per task-class 輪詢）／`llm_select`（utility LLM 選人，
  輸出不在 roster 或解析失敗一律 fail-closed 回 fixed_hierarchy；不硬編碼模型）。
- **動態判官深度（D4，MaAS 式）。** 零 LLM 的本地難度啟發式將 goal 分為 Simple／
  Complex：Simple 用兩面向判官（correctness + safety，省 completeness 成本）並套
  `[goal_loop] iteration_cap_simple`（預設 3）；Complex 維持三面向 MAV panel 與既有
  iteration_cap（預設 8）。**safety 面向在任何深度都保留**，synthesis 仍是全過才
  accept、缺面向 fail-closed。
- **半自動拓撲演化（D5，GPTSwarm edge-optimization 的 human-gated 版，預設關閉）。**
  背景驅動器（`[topology_evolution] enabled`，預設 false）聚合 per (agent, task_class)
  的拒絕率／needs_human／oscillation 證據，對持續低落的路由產生「改派給 sibling」
  提案——每個提案永遠經 ApprovalBroker 人工核准（無 LLM judge 裁量、不受
  autonomy_level 放寬、TTL 過期＝拒絕）；核准後寫入 `routing_overrides.json`
  （advisory lock＋原子替換，損毀 fail-safe 視為無 override），預設派工層命中即改派；
  24h 觀察期內新 agent 未優於基準即自動回滾。防提案風暴：同一 (task_class, from_agent)
  7 天至多一案。新增 `topology.list` dashboard RPC 與 `topology.*` 事件；詳見
  docs/guides/topology-evolution.md。
- **知識圖策展台（D6）。** 知識庫新增「策展台」分頁：SPO 知識圖譜可視化（d3 力導向、
  來源可信度分層配色、點邊看溯源側欄）、事實歷史時間線（supersession 鏈、現任高亮）、
  待審知識佇列（核准／拒絕／清除此來源三鍵，清除來源有確認框）。新增 dashboard RPC
  `memory.graph`／`memory.get_at`／`memory.invalidate_origin`（破壞性，僅 dashboard 面），
  `approvals.list` 支援 `action_kind` 過濾。文案面向終端使用者（zh-TW／en／ja 三語齊）。

### Security
- **origin_trust 正式參與檢索排序（D2）。** `RetrievalWeights` 新增 `w_trust`（預設 0.10，
  由 `w_fts` 0.40 → 0.35 讓利，總權重感覺不變）；每筆候選分數乘上
  `(1 - w_trust) + w_trust · origin_trust`，未驗證的 channel 蒸餾事實（trust 0.3）不再能
  蓋過人工 curated 事實（trust 1.0）。graph_rank 的邊權乘上該三元組的 `origin_trust`，
  壓制「單條假 triple 經 PPR 兩跳放大」的攻擊路徑。所有 `origin_trust = 1.0` 的既有資料
  排序與 D2 前 byte-identical（含 graph PPR 位元級一致，已寫測試驗證）。

### Changed
- **`store_temporal` 的 supersession 改為依真實世界時間 `valid_from` 判定，具備亂序韌性。**
  當新事實帶有 `valid_from` 且早於現任事實時，插入為「歷史段」（`valid_until` 設為
  現任的 `valid_from`），不動現任、不建 supersession 鏈——正確處理離婚／結婚等亂序
  ingest 的情境。無 `valid_from` 的寫入完全沿用既有 ingestion 順序行為。
  副作用：以完全相同內容重複 consolidate 使用者側寫時，現在會 reaffirm 既有摘要列
  （id 穩定）而非每次都新建一列。

### Fixed
- **分享全域 Skill 不再報「Skill not found」。** 「我的技能」清單同時列出 agent 自有
  與全域（`~/.duduclaw/skills/`）兩種來源，但 `skills.share` 只到 agent 的 SKILLS
  目錄找檔案，分享全域 skill（如 pptx/docx）必定失敗。現在 share 依同一聯集解析
  （agent 版優先、全域版遞補），並補上 agent_id 與 skill_name 的路徑安全驗證
  （拒絕 traversal 與點開頭名稱，與 `skills.adopt` 既有防護對齊）。
- **dashboard 導覽補齊 v2 步驟文案（三語）。** v2「嘟嘟事務所」導覽的六站
  （對話／收件匣／任務看板／Skill／成長／管理）先前在 zh-TW／en／ja-JP 三語
  皆缺翻譯鍵，導覽走到這些頁會顯示原始鍵名；已全數補齊，並移除十個已無程式
  引用的 v1 導覽殘留鍵。
- **ja-JP 介面補完 113 條未翻譯字串。** 先前以 `[EN]` 前綴標記的殘留
  （voice／proactive／settings.update／sharedWiki／mcp 等區塊）全數翻為日文；
  三語鍵集合完全一致（各 3,422 鍵）。
- **部門名稱驗證拒絕所有 `.` 開頭名稱（fail-closed 補洞）。** `is_valid_department`
  原本只擋 `.`／`..`，導致 `.hidden` 之類的點目錄會被 `departments.list` 列為部門
  （既有測試 `list_skips_invalid_names_fail_closed` 在乾淨 main 上即失敗）。現改為
  一律拒絕點開頭名稱，與該測試註解的原始意圖一致。

## [1.38.1] - 2026-07-19 — Dashboard self-update redirect fix

### Fixed
- **儀表板系統更新不再因 GitHub CDN 換域而失敗。** GitHub 已將 release 資產下載的
  重導向目標從 `objects.githubusercontent.com` 改為
  `release-assets.githubusercontent.com`(Azure blob 簽名網址),`apply_update`
  的 redirect 白名單只認得舊網域,導致下載被自身安全政策擋下、儀表板顯示
  「更新失敗」(audit log:`Download failed: error following redirect`)。白名單
  加入新網域(舊網域保留以防 GitHub 分階段切換);`check_update` 的 API 端點
  政策不受影響、維持原樣。注意:已在跑的舊版 binary 仍帶舊白名單,升級到本版
  需手動安裝一次,之後即可恢復儀表板一鍵更新。

## [1.38.0] - 2026-07-19 — Dashboard auto-save settings, high-risk toggle confirmations, OAuth PTY defaults

### Added
- **員工編輯頁改為即時儲存（2026-07-19）。** `/agents/:id/edit` 移除「儲存」按鈕：
  任何修改後 1 秒自動寫入（單一儲存航班＋尾隨補寫，不併發、不漏改），右上角顯示
  儲存中／已儲存狀態；CONTRACT.toml 區塊併入同一自動儲存循環，離開頁面時盡力補送
  未儲存變更。高風險開關（技能自動啟用、可建立 agent、可改 SOUL、Computer Use、
  瀏覽器 via Bash、網路存取、worktree 自動合併）在開啟時彈出二次確認框，關閉則
  直接生效。
- **偵測 Claude Code CLI OAuth → PTY 連線池預設開啟。** `agents.inspect` 現在回傳
  `agent.toml [runtime]` 實際內容（僅含檔案裡存在的 key，執行環境分頁從 write-only
  變為顯示真實設定值）；當偵測到 Claude OAuth、provider=claude、api_mode=cli/auto
  且 `pty_pool_enabled` 從未設定過時，一次性把 PTY 連線池＋ Worker 子程序託管預設
  為開並寫入 agent.toml（寫入後即為明確值，之後手動關閉不會被翻回）。

### Fixed
- **對話頁員工列不再重複顯示 main agent。** 頂端頭像列的 DuDu 小助理入口本就路由到
  main agent，員工卡片改為過濾 `role == 'main'`；既有選取若指向 main agent 會自動
  正規化回 DuDu 入口，避免「員工只有一位、對話卻出現兩顆頭像」的誤解。

## [1.37.1] - 2026-07-19 — Signed NFR test licenses + distributor self-service

### Added
- **NFR(Not-For-Resale)簽章標記(2026-07-18)。** License 新增簽章內 `nfr`
  布林欄位(向後相容:`false` 時序列化位元組與舊版完全一致,舊簽章照常驗證;
  從 `license.json` 移除標記即簽章失效 → fail-closed 降級 OpenSource)。用途:
  經銷商內部測試授權——完整 Self-Host Pro 功能(含白牌,可測 branded build),
  但授權頁會顯示白牌蓋不掉的「內部測試授權(NFR)— 不得轉售」標記(zh-TW/en/ja),
  轉售的副本一眼可辨。`LicenseSnapshot` 增列 `nfr`;refresh/rebind 均保留標記,
  re-sign 後不會「洗白」。

## [1.37.0] - 2026-07-18 — Goal Loop autonomous agents, custom dashboard widgets, DuDuClaw design system

### Added
- **自主目標迴圈（Goal Loop，2026-07-17）。** 在頻道丟一個目標，AI 員工就自主
  規劃、執行、自我驗收，做到完成或卡住時回來通知你——把「一問一答」升級為
  「給目標自主做完、卡住問你」的成套模式。整套預設關閉，靠 `[dispatch] enabled`
  啟用，不影響現有對話。
  - **驗收驅動終止**：完成訊號只認驗收判官核可（`LlmAcceptanceJudge` 接上帳號
    輪替），不信任 AI 自評「做完了」；未通過帶回饋重試（Generator-Verifier）。
  - **外層迴圈驅動器**（`goal_loop.rs`）：把待辦目標任務派上既有喚醒軌，重試即時
    推進；硬終止守衛（派工上限 / 牆鐘上限 / 並行上限 / 進度震盪偵測）任一踩線即
    轉人工，runaway 不可能。
  - **分級自主**（`AutonomyLevel` 五級 operator→observer，per-agent
    `agent.toml [capabilities] autonomy_level`，預設 Approver）＋ needs_human /
    kickoff 審批往返（Telegram / Discord / Slack / LINE 內嵌按鈕，冪等且
    fail-closed）。
  - **回饋路徑斷路器**（`duduclaw-core::dispatch_guard`）＋ 級聯 hop-depth 防再生型
    無限迴圈，`config.toml [dispatch_guard]` 可調。
  - **`/goal` 使用者入口**：`/goal <目標>`、`/goal <目標> || <驗收標準>`、
    `/goal status`；任務記住來源頻道＋對話（`tasks.source_channel/source_chat_id`
    兩欄，idempotent migration），進度與需人工通知推回發起對話。文件：
    `docs/guides/goal-loop.md`。
- **自訂 Widget 系統（2026-07-16）。** 儀表板 widget 從固定五款內建擴充為可自訂：
  - **沙箱執行環境**：自訂 widget 是單檔 HTML，在 `sandbox="allow-scripts"` 的
    iframe 內執行（無 `allow-same-origin` ⇒ 拿不到 JWT/DOM/localStorage），
    渲染時注入 CSP（禁外部資源與網路外呼）與 SDK shim；資料只能經
    postMessage **唯讀允許清單橋**（`agents.summary`／`tasks.summary`／
    `cost.summary`／`channels.status`／`system.status`，10 req/s 上限，
    繼承當前使用者的角色與資料範圍）。主題跟隨儀表板明暗、高度自動。
  - **AI 產生（一般使用者）**：`/widgets/new` 引導式流程（資料來源＋呈現型態＋
    自由描述）→ `widgets.custom.generate`（走 rotated CLI → Direct API 備援、
    零工具 caps）→ 沙箱即時預覽 → 不滿意可帶回饋「再改一版」→ 儲存才落庫。
  - **HTML 完整客製（管理員/經銷商）**：`/widgets/new?mode=html` 原始 HTML
    編輯＋即時預覽；匯出/匯入 `.json` 讓經銷商跨客戶搬運。
  - **Widget 工坊（`/widgets`）**：「我的／團隊分享」兩 tab，卡片帶 **lazy 縮圖
    預覽**（進 viewport 才掛縮放沙箱、不可互動），一鍵加入儀表板、分享/取消
    分享、複製他人分享、匯出入、刪除（管理員可下架任何分享）。
  - 首頁 layout 以 `custom:<id>` 引用（fail-closed：只放行自己可見的
    widget id），編輯模式抽屜可加入自訂 widget。**view-as** 檢視下屬儀表板時
    自訂 widget 一併渲染（html 隨 `dashboard.layout.view` 內嵌下發——受同一道
    strict-rank 閘保護，下屬私有 widget 不經 `widgets.custom.get` 外洩）。
  - 後端：`custom_widgets.rs` SQLite store（256 KB/widget 上限、擁有權在
    store 層強制）＋ `widgets.custom.list/get/create/update/remove/share/generate`
    七支 RPC。設計文件：`commercial/docs/custom-widgets-design-2026-07-16.md`、
    公開文件：`docs/features/30-custom-widgets.md`。
  - **產生鏈活體驗證過**（claude-sonnet-4-6 真跑兩輪）：第一輪暴露模型會輸出
    說明文字而非 HTML、且橋接資料缺日期欄位——已補 prompt 尾端輸出紀律、
    `extract_html_fragment()` 伺服端剝除環繞散文（純散文＝硬錯誤不落庫）、
    `tasks.summary` 增 `completed_today` 與 `completed_at`；第二輪產出 4.1 KB
    合規 fragment（`<` 開頭、用橋接、用 CSS 變數、零外部資源）。
  - **每人 widget 數量上限（2026-07-17）**：`custom_widgets.rs` 新增
    `max_widgets_per_user()`（預設 20，讀 `DUDUCLAW_MAX_WIDGETS_PER_USER`
    覆寫、`0`＝無限——與 `EditionProfile::personal_max_agents()` 同款慣例），
    在 store 層 `create()` 內強制（非只在 RPC 層），達上限回 zh-TW 錯誤訊息；
    `widgets.custom.list` 回應加 `max_per_user` 供前端顯示，工坊「我的」tab
    標籤在有上限時顯示「我的（{count}/{cap}）」。
  - **橋接層結果快取（2026-07-17）**：`widget-bridge.ts` 對唯讀橋接方法加
    15 秒 TTL 的模組層快取＋in-flight 去重（快取存 promise，同時到達的請求
    共享同一次呼叫；失敗立即從快取移除、不快取錯誤結果）——避免工坊縮圖
    同屏掛出數十個 iframe 時對同一 method 重複打 API；rate limit（每 widget
    10 req/s）維持原邏輯，快取命中一樣計入該 widget 的請求窗口。

- **個人版 Agent 規模策略：軟提示，不設硬上限（2026-07-15 提案、07-16 拍板 B+C）。**
  維持「self-host 永不設限」的開源承諾：個人版**預設無 Agent 數上限**
  （`personal_max_agents()` 預設 `0`=無限）。超過建議規模（3 個）時儀表板 Agent
  頁顯示**溫和升級提示**（可永久關閉、不阻擋任何操作），升級動機交給既有的企業
  能力閘（部門／簽核／多帳號／白牌）。託管部署若需要硬上限可設
  `DUDUCLAW_PERSONAL_MAX_AGENTS`（機制保留：`agents.create`、
  `templates.create_agent` 與 MCP `create_agent` 三個入口都會執行）。企業版不受
  此機制影響，維持依 license tier（含 self-host 豁免與簽章 override）。
- **決策/簽核事項主動推播到通道（Feature C，2026-07-15）。** 安裝簽核申請送出或
  進入下一關時，主動 DM 該關卡的簽核人（依角色＋部門解析：員工→同部門主管；
  主管關已過→管理員）到其**已綁定的通道**（`channel_identities`）。訊息含功能說明
  與安全審查摘要。全通道文字投遞（重用 `channel_sender`），best-effort 非阻斷。
  新模組 `install_notify`（`approvers_for` 有 5 個單元測試）。
- **通道核准/拒絕按鈕（Feature D，2026-07-15；07-16 補全四通道）。** `channel_format`
  新增 `duduclaw:install_approve|deny:{id}` 動作與 Telegram/Discord/Slack/LINE 四通道
  的按鈕 builder。**四通道端到端接通**：通知帶內嵌核准/退回按鈕（Telegram inline
  keyboard、Slack Block Kit、Discord DM components——自動開 bot↔user DM channel、
  LINE quickReply postback），點擊經 `install_notify::decide_from_channel` 把點擊者
  的通道帳號對回儀表板身分、依角色＋部門授權後 `InstallRequestStore::decide`，
  最終核准即伺服端重掃並安裝（`apply_install_request`，通道路徑略過即時 registry
  rescan，下次掃描熱載）。無按鈕通道走文字通知＋儀表板提示。
- **簽核最終結果回報申請人（2026-07-16）。** 申請被退回／核准安裝完成／核准但
  安裝失敗時，主動 DM 申請人的已綁定通道（`install_notify::notify_requester`），
  儀表板與通道兩條決策路徑都會觸發。先前申請人送出後只能自己去儀表板刷新。
- **MCP `create_agent` 補上 Agent 數上限（2026-07-16）。** MCP server 是獨立行程，
  儀表板的 `tier_limit_message` 閘門管不到它——先前任何 agent 可經 MCP 工具無限建
  agent，繞過 P-License 簽章 `max_agents` 配額（及託管部署設定的個人版硬上限）。
  新增 `license_runtime::agent_cap_message_from_disk()`（從磁碟 bootstrap license，
  與 gateway 同規則），`handle_create_agent` 建立前先過閘。
- **簽核按鈕點擊後即拆（2026-07-16）。** 決策落地後按鈕不再留在訊息上：Telegram
  `editMessageText` 改寫原通知並附上結果、Discord 用 interaction type 7
  UPDATE_MESSAGE 清空 components、Slack 走 `response_url` `replace_original`；
  未授權／已被他人處理的點擊仍以短訊回覆、按鈕保留給有權限的人。LINE quickReply
  本身即拋，無需處理。
- **Edition 背景變化也會推播（2026-07-16）。** 60 秒輪詢 edition 的安全網：
  phone-home 降級、CRL 撤銷、寬限期到期等**不經 RPC** 的授權轉換，現在也會廣播
  `system.status_changed` 讓開著的儀表板即時反映（RPC 路徑維持即時 inline 廣播）。
- **Edition 變更即時推播到儀表板（2026-07-16）。** 授權啟用（`license.activate`）
  或夥伴碼兌換（`license.redeem`）成功後，後端廣播 `system.status_changed`（帶新的
  `edition_profile`），開著的儀表板**不需手動重整**即反映 personal→enterprise 的
  切換（前端 system-store 早已訂閱此事件，先前後端從未發送）。`system.status`
  payload 抽成共用 `system_status_payload()` 供 RPC 與廣播共用。活體驗證：無 license
  開機為 personal → activate → 5 秒內收到事件帶 enterprise。

### Fixed
- **通道通知讀錯 LINE token 欄位（2026-07-16）。** `install_notify` 原以
  `{channel}_bot_token` 硬組 config 欄位名，LINE 的欄位其實是
  `line_channel_token`——LINE 簽核通知會靜默跳過。改用與 OTP 投遞相同的
  `token_field()` 對照表（單一事實來源）。同類掃描：cron 通知的全域 token
  fallback（`cron_scheduler::resolve_channel_token`）有一樣的硬組欄位名問題，
  LINE 全域 fallback 一併修正。
- **無按鈕通道漏掉儀表板提示（2026-07-16）。** 提示旗標語意反轉：WhatsApp／飛書等
  無按鈕通道的簽核通知反而**沒有**「請至儀表板核准」提示，收到通知的人無從行動。
- **`apply_install_request` 補齊 fail-closed 驗證（2026-07-16）。** 通道路徑的安裝
  執行漏了儀表板孿生（`execute_approved_install`）既有的 `agent_id`／
  `server_name`／department 識別字驗證（識別字會組檔案路徑）；另外 skill
  frontmatter 的 `name:` 曾直接組 temp 檔名，`name: ../../x` 可逃出 temp 目錄——
  新增 `sanitize_tmp_file_stem()` 並同步修補儀表板路徑 `run_skill_install` 的同類問題。

### Changed
- **儀表板全站設計系統重構（2026-07-18）。** 整個網頁儀表板換上全新視覺語言：
  沉穩的四層表面疊出深度、克制的動效、成體系的圓角與陰影，資訊密度更高卻更
  好讀。底層改用 `web/src/components/mds/` 新元件庫（一套按鈕／卡片／列表／
  對話框／設定版型等原語），65+ 個頁面全數遷移到這套原語上，舊的 Calm Glass／
  Soft Play 設計系統與其殘留元件、CSS 一併移除。功能一個不減，只換外觀與結構。
  使用者可感知的變化：全新的整體視覺、**側邊欄導航重新分組**（個人／工作／
  公司／設定四區）、**管理區收斂為統一的「設定式」版型**（左側分組導覽＋設定列）、
  **AI 員工的設定拆成多個子分頁**（能力與設定分頁瀏覽，不再是一張擠滿欄位的
  巨型表單）。深淺主題與三語（繁中／英／日）維持同步。設計文件：`web/DESIGN.md`。
- **Create/Edit Agent 從彈窗改為獨立頁面（2026-07-16）。** `/agents/new` 與
  `/agents/:id/edit` 取代 AgentsPage 內的兩個巨型 Dialog（頁面可深連結、
  表單不再擠在彈窗內捲動）；行為與欄位完全保留（含個人版隱藏部門欄位）。
  `AgentsPage.tsx` 由 2,423 行縮至 452 行，表單拆至 `pages/agent-form/`；
  順手移除從未能開啟的 `InspectDialog` 死碼。
- **個人版隱藏部門/企業設定（2026-07-15）。** 個人版是單人形態，無部門概念：
  導航「部門管理」改 `enterprise` 閘（個人版隱藏）；新建／編輯 Agent 對話框的
  部門下拉、Skill 安裝的 `department:` scope 選項在個人版一併隱藏（沿用
  `system.status.edition_profile === 'personal'` 既有 gate 慣例）。多帳號成員頁
  原本就是 enterprise 閘，個人版本來就看不到。

## [1.36.0] - 2026-07-15 — Vetted Skill/MCP install-from-URL + department-routed install approvals

### Added
- **Skill / MCP 安裝簽核鏈（2026-07-15）。** 管理員以外的使用者安裝 Skill／MCP
  前，須送出**簽核申請**，內容包含該項目的**功能說明**與**安全審查結果**
  （風險等級＋逐項發現），核准後系統才會實際安裝。簽核鏈：**員工** → 部門主管
  核准 → 管理員核准 → 安裝；**主管** → 管理員核准 → 安裝；管理員仍為直接安裝。
  管理員核准可一次涵蓋兩關（上級短路）。新增 store `install_requests`（SQLite
  兩階簽名鏈，角色感知 decide，fail-closed：逾時＝拒絕、終態不可翻轉、風險 ≥
  High 於申請與執行兩端都拒絕）；admin/manager RPC `install_requests.list`／
  `install_requests.decide`（最終核准即伺服端重掃並安裝），任一登入者
  `skills.install_request`／`mcp.install_request`／`install_requests.mine`。
  審批頁（ApprovalsPage）新增「安裝簽核申請」區；Skill／MCP 引入對話框對非
  管理員改為「送出簽核申請」並顯示待簽狀態。
- **簽核鏈的部門路由（2026-07-15）。** 使用者新增 `department` 欄位（`users`
  表冪等 migration；成員管理頁建立／編輯對話框以 datalist 帶出既有部門，
  `is_valid_department` 驗證）。員工的安裝申請只路由給**同部門**的主管簽核
  （大小寫不敏感精確比對）——主管的「安裝簽核申請」清單只列出自己部門、且尚
  待主管簽的員工申請；跨部門主管與無部門主管都無法簽（會回「屬於其他部門」）。
  未設部門的申請退回「任一主管」的相容行為；管理員不受部門限制、仍可短路兩關。
  路由判斷集中在 `InstallRequest::manager_may_sign`。

### Changed
- **`skills.vet` 與 `mcp.import.fetch` 放寬為任一登入者可呼叫（2026-07-15）。**
  這兩支是唯讀的抓取＋安全掃描（無任何寫入、共用 SSRF 防護），開放給非管理員
  是為了讓其在送出安裝簽核前能預覽功能與掃描結果。實際安裝仍受管理員／簽核鏈
  把關。
- **Skill 從 GitHub / URL 引入（2026-07-15）。** Skill 頁（原「Skill 市場」，
  導航改名為「Skill」）市場分頁新增「從 URL 引入」：支援 GitHub repo（自動
  讀取 SKILL.md）、blob 檔案連結、GitLab、Gist 與任意 raw 檔案網址。內容由
  後端抓取（共用 SSRF 防護：擋 loopback／私有網段／雲端 metadata，逐跳
  redirect 重驗，1MB 上限，HTML 頁面拒收）並先過安全掃描，掃描未通過無法
  安裝；`skills.install` 伺服端 fail-closed 重掃不變。
- **MCP Server 從 GitHub / URL 引入（2026-07-15）。** MCP 工具頁新增
  「從 URL 引入」：貼 GitHub / GitLab repo（依序尋找 `.mcp.json`／`mcp.json`／
  `server.json`（含 **MCP Registry 2025 schema**：npm→npx、pypi→uvx、
  oci→docker、remotes→自動以 `npx -y mcp-remote <url>` 橋接）／**README
  設定範例**（fenced code block 內的 `mcpServers` 片段，跨平台重複片段自動
  去重）／`package.json`（有 `bin` 才推斷 `npx -y <pkg>`））或任意 JSON
  manifest 網址，後端抓取後**逐一安全掃描**
  （新掃描器 `mcp_scan`：shell／下載器／提權指令、inline eval、shell
  metacharacters、docker `--privileged`／根目錄掛載、env 指令替換等，
  與 skill 掃描共用風險分級，risk ≥ High 拒絕），管理者審視 command／args／
  env 與掃描結果後才可安裝，可選同時寫入 Marketplace 清單
  （`~/.duduclaw/marketplace.json`）供重複使用。新增 admin RPC
  `mcp.import.fetch` / `mcp.import.install`（安裝端 fail-closed 重掃）。

### Changed
- **既有 MCP 安裝路徑補上同一道安全閘（2026-07-15）。** `mcp.update` 的
  add 動作與 `marketplace.install`（含使用者自帶 marketplace.json）現在
  也會掃描 server 定義並在 risk ≥ High 拒絕——先前這兩條路徑完全不經
  掃描直接寫入 `.mcp.json`。內建 catalog 全數通過掃描（有回歸測試鎖住）。

### Fixed
- **`skills.vet` 補上 SSRF 防護（2026-07-15）。** 先前 dashboard 的 skill
  安全掃描 RPC 直接抓取任意 URL，可被用來探測內網／雲端 metadata 端點；
  現在與 web_fetch 共用同一個 `validate_url` 閘，並限制回應大小與 redirect。
- **Dashboard 團隊板模一鍵備妥（templates.* RPC，2026-07-14）。** 首次登入
  onboarding 可選擇產業：後端「備妥」該產業的部門板模但**不建立任何 Agent**，
  由管理者逐一建立——建立時可選部門角色，SOUL.md 以文本編輯器呈現可修改，
  CONTRACT.toml / agent.toml 亦可在進階區修改（後端 TOML 驗證 fail-closed，
  改壞不寫入）。另提供跨產業 CEO（營運總管）板模作為第一位 AI 員工的建議起點。
  新增 admin-gated RPC 五支：`templates.industries` / `templates.stage` /
  `templates.roster` / `templates.role` / `templates.create_agent`（license
  `premium_templates` 閘，未解鎖回 upsell 旗標）。premium 板模的檔案系統探索
  邏輯自 `duduclaw-cli` 上移至新模組 `duduclaw-gateway::premium_templates`
  （cli 改 re-export，wizard 行為不變）；agent.toml 身分接線與合規 overlay
  append 以 `toml_edit` 保留註解地組裝。WelcomePage 精靈改 4 步（新增產業選擇），
  AgentsPage 建立對話框支援套用板模＋SOUL.md 編輯＋進階 TOML 編輯（i18n 三語）。

- **Dashboard 授權升級 UI（2026-07-14）。** LicensePage 新增「升級／啟用授權」卡：
  本機指紋顯示＋複製（購買時提供）、授權金鑰啟用（貼 base64 或 JSON）、夥伴
  NFR 兌換碼免費路徑；首跑期間啟用成功可一鍵返回設定精靈。後端三支 admin-gated
  RPC：`license.fingerprint` / `license.activate` / `license.redeem`——啟用走
  fail-closed 驗證（簽章→指紋→效期，全過才寫檔；dashboard 不接受檔案路徑輸入），
  成功後 `LicenseRuntime::install_and_reload` 熱重載，premium 功能**免重啟**即解鎖
  （活體驗證：鎖定→啟用→22 產業板模即時解鎖）。

- **Agent 衣帽間（2026-07-14）。** AI 員工造型改為遊戲式配件組合：帽子／頭部／
  身體／手持／腳部／裝飾六個槽位＋主色（10 色）自由搭配（30+ 內建配件——高帽、
  皇冠、工程帽、墨鏡、西裝、圍裙、咖啡、扳手、球鞋、光環…）。員工詳情頁
  「造型」卡開啟衣帽間對話框：即時 bust 預覽（就是列表用的同一個角色元件）、
  隨機、還原預設；儲存後**同步顯示在員工列表、所有頭像與世界地圖**（PixiJS
  全身角色含腳部配件）。未打扮的員工維持原本的種子造型（零視覺變化）；打扮
  過的員工以角色渲染優先於已上傳照片（照片上傳降級為進階摺疊選項）。新增
  admin RPC `agents.set_outfit`（形狀＋字元集 fail-closed 驗證，`outfit: null`
  還原），`agents.list` / `agents.inspect` 帶回 `outfit`，持久化為
  `agents/<id>/outfit.json`。
- **主管唯讀檢視下屬儀表板（view-as，2026-07-14）。** 主管／管理者可檢視
  **嚴格低於自己階級**成員的個人儀表板（管理者可看主管與員工；主管只能看
  員工，看不了同級主管與管理者）：成員管理頁每列的「檢視他的儀表板」眼睛
  按鈕、或首頁右上「檢視成員儀表板…」下拉。檢視模式顯示唯讀橫幅、隱藏
  「編輯版面」，畫面用**對方的** widget 目錄與版面，資料範圍縮到對方綁定的
  AI 員工（WP11 員工資料範圍）。「不能代為修改」是結構性保證——寫入 RPC
  只存在 `dashboard.layout.set`（永遠寫呼叫者自己的檔），沒有 set-for-others。
  新增 RPC：`dashboard.layout.view {user_id}`（manager+，階級不足回 generic
  permission denied 防枚舉）、`users.subordinates`（manager+，僅回 id／顯示
  名／角色三欄，遠窄於 admin 的 `users.list`）。
- **客製化個人儀表（WP15 MVP，2026-07-14）。** 首頁下半部改為 per-user widget
  版面：右上「編輯版面」進入編輯態（上移／下移／隱藏、底部「已隱藏的元件」
  抽屜重新加入），完成後以**個人 user 身分**存 server 端（`dashboard/layouts/
  <user_id>.json`），重登入仍在；每人互不影響。首發元件：需要我（manager+）、
  正在進行、最近活動、最近任務、通道健康（admin）。新增 RPC 三支：
  `dashboard.widgets.catalog`（**依角色過濾 fail-closed**——無權的 widget 不
  下發，`layout.set` 對無權 id 直接拒絕而非靜默丟棄）、`dashboard.layout.get`
  / `dashboard.layout.set`。戰報 HUD 與世界舞台維持固定不入版面系統。
- **去識別化欄位級設定（2026-07-14）。** 每個輸入來源（工具結果／使用者輸入／
  系統提示／子代理回覆／排程情境）除模式外，可再細選**哪些欄位**要去識別化：
  `only_categories`（只遮這些）／`exclude_categories`（排除這些，重疊時排除
  優先）。TOML 同時接受舊的字串形式（`user_input = "off"`）與新的表格形式
  （向後相容，空清單自動收斂回字串形式）。dashboard 去識別化分頁改版：
  「偵測規則集」從盲打名稱改為勾選清單（內建 5 組＋自訂，顯示各組涵蓋欄位）；
  來源列可展開欄位範圍選擇器（身分證字號、手機、Email、信用卡…18 種欄位
  中文標籤）。`redaction.get` 回應新增 `available_profiles` 目錄（含每組
  規則數與欄位類別）；`sources` 改為詳細物件形式。引擎端過濾在 pipeline
  逐 match 套用，audit 與 vault 行為不變。**設定即時生效（熱重載）**：
  `redaction.update` 寫入 config.toml 後就地重建 RedactionManager 熱插拔
  （vault GC 任務隨之重啟，處理中的訊息沿用舊規則自然收尾，下一則訊息即用
  新規則），不再需要重啟 gateway；重建失敗時保留變更前的即時規則並在回應
  `warning` 誠實回報（dashboard 以錯誤 toast 呈現）。
- **部門管理頁＋新增 AI 員工時的組織定位（2026-07-14）。** 新增「管理 → 部門」
  頁（admin）：預先建立部門（實體化為 `shared/wiki/departments/<dept>/` 知識
  空間，維持 WP7「部門＝衍生」設計，不引入新儲存）、檢視各部門成員／知識頁／
  技能數、刪除（有成員拒絕；有內容需二次確認）。新增 RPC 三支：
  `departments.list`（manager+）/ `departments.create` / `departments.remove`
  （admin）。新增 AI 員工對話框加入「上級 AI 員工」與「隸屬部門」下拉（板模
  路徑預設沿用板模接線，可覆寫）；`agents.create` / `templates.create_agent`
  接受 `reports_to`（須為既有員工，建立前驗證）與 `department`（WP7 allowlist）
  參數，建立當下寫入 agent.toml。編輯對話框的部門 datalist 併入註冊表清單。

### Fixed
- **Self-Host Pro／Partner 授權啟用後「帳號管理／治理」企業面板消失（2026-07-14）。**
  `EditionProfile::from_tier_key` 只把 business/oem 判為 Enterprise，自架線的
  企業方案 `self_host_pro` 與 `partner` 落到 Personal——啟用授權後 dashboard
  的多帳號管理（`/manage/users`）、治理等 `enterprise` 導覽項反而被隱藏。
  修正對應表（與 features.toml `dashboard_enterprise = true` 的 tier 同步，
  snake_case 與 kebab-case 皆接受）；`system.status` 的 `edition_profile`
  即時反映，授權熱啟用後免重啟企業面板即出現。帳號管理頁的「綁定 AI 員工」
  對話框同時從手打名稱改為現有員工下拉選單（已綁定者自動排除，roster 讀取
  失敗時退回文字輸入），zh-TW 介面用語統一為「AI 員工」。
- **首跑親測四修（2026-07-14）：**①console 首跑訊息改引導至 dashboard 直接設定
  管理者密碼（loopback bind 不再印一次性密碼——與 first-run claim 流程一致；
  非 loopback bind 仍印，因 claim 端點僅限 localhost）；②全新／閒置系統不再
  彈出全零的「昨日戰報」（`reportHasActivity` 閘，靜默燒當日標記）；
  ③`runtime.detect` 的 Claude OAuth 偵測在 macOS 永遠回 false——憑證在
  Keychain 不在 `.credentials.json`，檔案探測 miss 時改問 `claude auth status`
  （8s timeout）；同函式把 `~/.duduclaw` 誤當使用者 HOME 傳給
  `which_*_in_home`，nvm/bun 安裝的 CLI 全數隱形——改 PATH 優先＋真使用者
  HOME（五個 runtime 一併修）；④首跑精靈的「前往授權頁」被 FirstRunGate
  彈回第一步——`/license` 加入 first-run 白名單，精靈進度（不含 API key）
  以 sessionStorage 續存，返回時從原步驟繼續。
- **Template agent.toml 帶 `[container]` 節時 registry 靜默略過（2026-07-14）。**
  `ContainerConfig.additional_mounts` 缺 `#[serde(default)]`，任何板模部署的
  agent.toml 只要有 `[container]` 節而未寫該 key，typed 解析即失敗、agent 被
  scan 靜默跳過（免費 `templates/` 中五個帶 `[container]` 的板模與全部 premium
  板模都中招）。已改為 default-empty，由 114 角色全量活體掃雷驗證。
- **建立 main agent 失敗仍降級現任 main（`agents.create` 與 `templates.create_agent`）。**
  名稱撞既有 agent 時 `demote_current_main` 已先執行，現任 main 被永久降級。
  兩處都改為先原子取得目錄（`create_dir`，同名併發只有一方成功）再降級，
  失敗即回滾刪目錄；`templates.create_agent` 的部分寫入失敗不再留下缺
  CONTRACT 的半成品 agent（agent.toml 原子改名移至最後作為 commit point）。
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
  request goes through a compression pipeline (turn trim → drop oldest
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
- **Turn trimming**: Long conversation turns (>800 chars) are
  trimmed to head 300 + tail 200 chars with `[trimmed N chars]` placeholder.
  CJK-safe char-level slicing. Zero LLM cost.
- **Direct API prompt cache strategy**: "system_and_3" cache breakpoint placement
  for ~75% cache hit rate on multi-turn conversations.
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
