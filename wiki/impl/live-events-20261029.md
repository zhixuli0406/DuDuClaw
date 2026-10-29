# Live events 靜態實作交付（2026-10-29）

狀態：程式、測試與文件已寫入，**編譯／測試／原生與實際 runtime 驗收均未執行**。依交辦，未執行 cargo、rustc、clippy、cargo fmt、npm、pnpm 或任何 build/test；未 commit、push、stash、切分支。工作位置為既有 `DuDuClaw-wt-live-events`，分支 `feat/live-events-20261029`，交辦基底 `84b73ae2`。未建立新 worktree，保留原工作目錄供 reviewer 接續。

已讀根目錄 CLAUDE.md 與另一 worktree 的 RFC-28 §6、§8；未將 RFC 複製進此分支。

## 事件來源與行程

下列路徑相對 repository root，行號為這次交付時的位置。

| 事件 | 發送點 | 行程與路徑 |
|---|---|---|
| `agent.turn_started` / `agent.turn_finished` | `crates/duduclaw-gateway/src/dashboard_feedback.rs:165` / `:218`；建構點 `channel_reply/inner.rs:1518`、`claude_runner.rs:977`、`dispatcher.rs:1465` | Gateway。跨 runtime 分流前建立 LiveTurn，Drop 結束／取消；home-keyed sender → event_tx |
| `goal.round_started` / `goal.round_finished` | `crates/duduclaw-gateway/src/goal_loop/mod.rs:128` / `:136`；Solo 經 LiveTurn，Team 在 `goal_loop/enqueue.rs:183` | Gateway。實際 runtime／composer 呼叫邊界，排隊不發 started；Team scope 抑制 role 子呼叫重複 round |
| `goal.done` / `goal.failed` | `crates/duduclaw-gateway/src/goal_loop/tick.rs:180` / `:212`，共用 `goal_loop/notify_bridge.rs:396` | Gateway goal loop 觀察完成／失敗狀態，event_tx；不寫入 task.updated bus |
| `task.needs_human` | `crates/duduclaw-gateway/src/goal_loop/escalate.rs:63` / `:132` | Gateway。直接 escalation 與 needs_human reconciliation，task_id、agent_id、PauseReason；不含 reason prose |
| `task.created` | 既有 `crates/duduclaw-cli/src/mcp/tasks.rs:360` / `:558` | mcp-server → `mcp/autopilot_skills.rs:310` → events.db；新增 dashboard allow-list |
| `task.updated` | 既有 `crates/duduclaw-cli/src/mcp/tasks.rs:805` / `:927`、`mcp/tasks_goals.rs:82` / `:150` | mcp-server → events.db；新增 dashboard allow-list。既有 RPC direct push 同步精簡為 id/assigned_to/status |
| `approval.created` | `crates/duduclaw-gateway/src/approval/store.rs:208` | 任何開啟 broker 的行程（含 mcp-server/proxy/os-ops 等）；broker insert 成功後 → events.db |
| `approval.decided` / `approval.expired` | `crates/duduclaw-gateway/src/approval/store.rs:296` / `:303`；bound decision `approval/binding.rs:418`；invalidation `:488` | 同上。一般 decide/poll/await/sweep/withdraw 共用 conditional transition；成功改變狀態才發事件 |
| `agent.status_changed` | `crates/duduclaw-gateway/src/handlers/agents_ops.rs:216`、`handlers/agents_update.rs:1063`；`crates/duduclaw-cli/src/mcp/agent_update.rs:631` | Gateway RPC → event_tx；MCP 成功寫檔後 → events.db |
| `budget.breaker_open` | `crates/duduclaw-gateway/src/budget.rs:351` | Gateway breaker transition → event_tx |
| `autopilot.breaker_open` | `crates/duduclaw-gateway/src/autopilot_engine.rs:1128` | Gateway circuit open／retrip → event_tx |
| `run.step` | channel append：`crates/duduclaw-gateway/src/run_steps.rs:302` / `:317`（上游 `channel_reply/cli_env.rs:624`）；dispatch `claude_runner.rs:579` / `:682` | Gateway，**不走 event_tx**；獨立有上限的 per-connection mpsc hub，先比對 selector，再逐次授權 |

共用 database bridge：`dashboard_feedback.rs:80` allow-list、`:102` payload projection；既有 `autopilot_engine.rs:2399` 每約兩秒 tail。broker append 實作在 `approval/live_events.rs:75`，不依賴 gateway sender。新增 emitters 不附提示詞、工具參數、檔案內容或錯誤全文；task direct/bridge 都移除 description/result 等。

`source` 僅 channel/goal/cron/heartbeat/responsibility/assign/autopilot。runtime 為呼叫開始時選用 provider；既有 failover 可能另用其他 provider，不能當成最終 provider receipt。turn/round finished 包含取消，並非驗收成功。

## 推播權限與恢復

`handlers/push_filter.rs:46` 明列新名稱；未知名稱仍 Admin-only，未採任意 prefix 開放。已知 agent 事件缺 agent_id 拒絕；既有 global employee 通知缺 id 時限 Admin。

- Agent：目前員工 Viewer binding，既有 live identity cache 最多兩秒。
- Task／goal：現存 task ownership + TaskPacket audience；task board 受限卡片沿用遮蔽，goal 私有內容不送。
- Approval：重新讀 broker record。必須是當下 Manager，需要改密碼則拒絕。可見性依種類，與決定路徑對齊：`knowledge_add_source` 為 Admin，或該集合目前的 Editor／Owner（集合讀不到時非 Admin 拒絕，不另要求員工 Viewer）；operator CLI、workflow activation（含 payload 判定）、`support_pilot_review`、`requires_board`（`strategic_plan`、`agent_hire`）、`agent_file_change` 只有當下 Admin；`knowledge_quarantine` 與 `expert_skills_enable` 維持員工 Viewer。其後再套工作流啟用快照與未綁定任務受眾。list、get 與 live push 共用 `ApprovalVisibility::may_read`。使用 metadata-only gate，不呼叫會解析外部 channel link 的 list renderer。
- Breakers／未知事件：Admin。
- `run.step`：一般 broadcast 永不放行；只能經 opt-in queue，metadata-only 檢查與 runs.get 的 task/employee/history 權限一致。Filtered conversation history 不提供 tool steps；撤權、hidden/undo、未知 run 一律拒絕。

`live_events.rs` 的 EventSequence 在 filter 後才編號，每條連線從 1 起算，包含 logs/os/run 與 loss control frame；u64 overflow 關閉連線。boot_id 是本次 gateway 行程 UUID，connect 與每個 event 都帶。server.rs:1133 broadcast capacity 1024；:16263/:16285/:16339 的 Lagged 發 `events_lost`，不暴露包含其他 audience 的 dropped count。沒有 replay buffer 或新 snapshot RPC。

同一事件可經 direct RPC 與 DB bridge 重複送達；client 必須當 invalidation 重讀 list。Web 用 100ms 合併重複事件；fetch 期間又有事件時補讀，舊回應不能清掉較新的 stale 狀態。Sidebar 與 Inbox 每五分鐘 fallback，錯誤保留原數量並顯示 `?`。重連、boot 變更、events_lost 都 refetch。

## Run 訂閱與來源限制

`runs.subscribe` / `runs.unsubscribe` 僅接受 run_id 或 agent_id 其中之一。每條連線 16 selectors；每秒 20 commands，最多一個訂閱命令在處理；步驟每秒 20、queue 64。subscribe 授權成功後才加入，已滿回錯；unsubscribe idempotent；斷線 drop listener 即全部解除。queued step 送出前再比對 selector／權限。

Channel run id 使用 host `TURN_USER_MESSAGE` 固定 row + session + agent，不能查最新訊息替代；缺來源或本輪被 hidden/undo 時不推送，避免舊 CLI 的步驟誤投到下一輪。run.step 工具欄位限 identifier 字元，summary 為 truncate_chars 的固定標籤。

目前 live steps 僅 Claude；其他 runtime 的既有 durable collector 不變，不對外聲稱它們沒有歷史證據。Channel 開始步驟在 append 時推送；dispatch 的 id 在整輪 record_dispatch_run 後才產生，因此是完成後批次送 `phase=finished`，不是執行期間逐步串流。未新增 runtime streaming parser 或 run 預先配置協定。

## Approval digest

新增 approvals.get（id 必填，仍用 `{approvals,count}`）。list/get 卡片帶 digest，決策資料不可見時 null；decide 可選 expected_digest，省略維持原行為。hash 為遞迴排序 key、陣列不排序的 compact UTF-8 JSON → SHA-256 lowercase hex。

固定欄位：id、agent_id、kind、summary、可見 payload（bound 為 payload_hash）、request_kind、status、binding、answer、invalidated_reason、created_at、ttl_seconds、expires_at、simulation、decided_in_dashboard_only、workflow_activation（含當下 pricing）。不含 channel link、permission flags 或 digest 本身。list/get 同一份 activation projection 同時計算 hash 與回傳；decision task-local 帶 home，transaction guard 會再讀 projection。初讀、特殊操作鎖後與 decision transaction 都驗證，mismatch 不寫決策。

## 已寫測試（全部未執行）

| 檔案／測試群組 | 範圍 |
|---|---|
| `approval/tests/live_event_cases.rs` | 獨立 broker 共享 DB、payload 無秘密、expiry 各入口只發一次、canonical hash、accept/reject、keyed dedupe、pricing 變更拒絕、invalidation 一次 |
| `approval/tests/bound_cases.rs` | bound decision digest guard |
| `handlers/approvals_topology_rpc.rs::live_event_tests` | list/get digest 一致、錯誤格式、無 digest 舊行為、dashboard-only |
| `dashboard_feedback.rs` 新測試 | broker→DB→bridge frame、task projection、source/runtime matrix、turn 配對、home 隔離 |
| `goal_loop/tests/escalate_cases.rs` 新測試 | PauseReason 無 reason prose、無 task.updated bus；enqueue 不發 started；取消閉合/team 不重複 |
| `handlers/push_filter.rs::live_event_tests` | 明確分類／unknown default；task audience 後才 seq；approval stored metadata/Manager/Admin/audience；direct task redaction |
| `live_events.rs::tests` | 每連線 seq、loss frame、cap/rate/idempotence、targeted queue/redaction/overflow |
| `handlers/runs_live.rs::tests` | selector、私有任務 audience、agent-wide 不擴權、撤權、未知 run、偽造 agent_id、filtered history、hidden turn |
| `run_steps.rs::live_step_identity_tests` | 固定本輪 user row；後續訊息不改歸屬；hidden/缺 host 不回退到其他輪 |
| `claude_runner.rs::live_dispatch_step_tests` | 非 Claude 不推、Claude 不重播共用 collector 前段 |
| `server.rs::live_events_socket_tests` | 真正 loopback `/ws`；獨立 broker→events.db→正式 bridge transform→connection gate→seq/boot；禁止 run broadcast 不占 seq；1024 overflow→events_lost。手動驅動 bridge transform，未覆蓋 production poller 的兩秒排程 |
| `web/src/stores/live-events.test.ts` | task/approval/agent refetch、duplicates、fetch 競態、失敗 stale |
| `web/src/lib/ws-client.live-events.test.ts` | reconnect/handshake/boot 與舊 gateway 相容 |

## Reviewer 優先檢查與限制

1. Rust async `Send`：broker SQLite transaction 離開 scope 後 emit await、run metadata 授權、LiveTurn/LiveRuntimeRound/task-local 與 team composer future。
2. 所有 WsFrame::Event struct literals 的新 optional boot_id；本次搜尋並補既有 constructors，尚未編譯確認其他條件編譯路徑。
3. 新 `/ws` fixture 的 Axum/tungstenite 型別、測試使用的 SessionManager schema／privacy fixture；目前只靜態檢查，沒有宣稱測過傳輸。
4. TypeScript 嚴格型別、Vitest fake timers/WebSocket mock，尤其 Inbox fetch 舊回應及需要人工介入任務計數。既有 store 測試也應回歸。
5. 核准卡片可見性依種類。`knowledge_add_source` 對集合編輯者開放；operator CLI、工作流啟用、`agent_file_change`、`support_pilot_review` 與董事會種類只有當下 Admin。`knowledge_quarantine` 與 `expert_skills_enable` 維持員工 Viewer（後者的 agent id 是套件 slug；決定路徑只有 Manager，可見性不放寬成所有 Manager，也不收成只有 Admin）。尚未實際 UI 驗收。
6. broker 狀態 commit 與 events.db append 不是同一 transaction、沒有 outbox；crash window 由 fallback refetch 修復。既有外部先執行再記決策的路徑，副作用不能和 digest guard 原子回滾；final mismatch 拒絕寫 decision，但無法保證先前副作用未發生。
7. goal done/failed 仍是 goal loop cadence 觀察結果。dispatch live step 目前只能完成後批次通知。非 Claude turn 事件有 hook/static matrix，未啟動真正 Codex/Grok/API 執行驗收。
8. 尚未進行指定 `gpt-daybreak-blue-latest` 安全審查；本次僅獨立 code reviewer 靜態 diff review，不能等同安全放行。

靜態驗證：`git diff --check` 無輸出；分支與 working tree 留給 reviewer。沒有 public deploy／merge／release。

## 異動檔案

共 62 個檔案（包含本紀錄；既有事件 literal 增補 boot_id 的檔案也列入）。

- `CHANGELOG.md`
- `crates/duduclaw-cli/src/mcp/agent_update.rs`
- `crates/duduclaw-gateway/src/approval/binding.rs`
- `crates/duduclaw-gateway/src/approval/live_events.rs`
- `crates/duduclaw-gateway/src/approval/mod.rs`
- `crates/duduclaw-gateway/src/approval/store.rs`
- `crates/duduclaw-gateway/src/approval/tests/bound_cases.rs`
- `crates/duduclaw-gateway/src/approval/tests/live_event_cases.rs`
- `crates/duduclaw-gateway/src/approval/tests/mod.rs`
- `crates/duduclaw-gateway/src/autopilot_engine.rs`
- `crates/duduclaw-gateway/src/budget.rs`
- `crates/duduclaw-gateway/src/canvas.rs`
- `crates/duduclaw-gateway/src/channel_reply/inner.rs`
- `crates/duduclaw-gateway/src/claude_runner.rs`
- `crates/duduclaw-gateway/src/dashboard_feedback.rs`
- `crates/duduclaw-gateway/src/dispatcher.rs`
- `crates/duduclaw-gateway/src/goal_loop/enqueue.rs`
- `crates/duduclaw-gateway/src/goal_loop/escalate.rs`
- `crates/duduclaw-gateway/src/goal_loop/mod.rs`
- `crates/duduclaw-gateway/src/goal_loop/notify_bridge.rs`
- `crates/duduclaw-gateway/src/goal_loop/tests/escalate_cases.rs`
- `crates/duduclaw-gateway/src/goal_loop/tick.rs`
- `crates/duduclaw-gateway/src/handlers/agent_files_rpc.rs`
- `crates/duduclaw-gateway/src/handlers/agents_ops.rs`
- `crates/duduclaw-gateway/src/handlers/agents_update.rs`
- `crates/duduclaw-gateway/src/handlers/approval_decider.rs`
- `crates/duduclaw-gateway/src/handlers/approvals_decide.rs`
- `crates/duduclaw-gateway/src/handlers/approvals_topology_rpc.rs`
- `crates/duduclaw-gateway/src/handlers/cli_login_rpc.rs`
- `crates/duduclaw-gateway/src/handlers/dispatch_org.rs`
- `crates/duduclaw-gateway/src/handlers/handshake_license.rs`
- `crates/duduclaw-gateway/src/handlers/lifecycle.rs`
- `crates/duduclaw-gateway/src/handlers/mod.rs`
- `crates/duduclaw-gateway/src/handlers/push_filter.rs`
- `crates/duduclaw-gateway/src/handlers/runs_live.rs`
- `crates/duduclaw-gateway/src/handlers/system_config_rpc.rs`
- `crates/duduclaw-gateway/src/handlers/tasks_rpc.rs`
- `crates/duduclaw-gateway/src/knowledge/add_source_approval.rs`
- `crates/duduclaw-gateway/src/lib.rs`
- `crates/duduclaw-gateway/src/live_events.rs`
- `crates/duduclaw-gateway/src/protocol.rs`
- `crates/duduclaw-gateway/src/run_steps.rs`
- `crates/duduclaw-gateway/src/runtime_install.rs`
- `crates/duduclaw-gateway/src/server.rs`
- `docs/api/openapi.yaml`
- `docs/features/24-task-board.md`
- `docs/features/ja-JP/24-task-board.md`
- `docs/features/zh-TW/24-task-board.md`
- `web/src/components/layout/AppSidebar.tsx`
- `web/src/i18n/en.json`
- `web/src/i18n/ja-JP.json`
- `web/src/i18n/zh-TW.json`
- `web/src/lib/api.ts`
- `web/src/lib/live-events.ts`
- `web/src/lib/ws-client.live-events.test.ts`
- `web/src/lib/ws-client.ts`
- `web/src/pages/InboxPage.tsx`
- `web/src/stores/agents-store.ts`
- `web/src/stores/approvals-store.ts`
- `web/src/stores/live-events.test.ts`
- `web/src/stores/tasks-store.ts`
- `wiki/impl/live-events-20261029.md`

## Web 修復與實測補記（2026-10-29）

- 已依本次追加授權執行 npm／npx，前文「全部未執行」為原始交付狀態；Rust 仍未執行，留由 reviewer 驗證。
- `api.ts` 的 TS6133 來自 `installRequests.decide` 誤加的 `expectedDigest`，已移除；`approvals.decide` 原有的 `expected_digest` 與 gateway 一致，Inbox 已從列表卡片傳入 digest，保留此路徑。新增 `approvals-api.test.ts` 涵蓋批准／拒絕帶 digest、舊 gateway 無 digest 與安裝申請 RPC；`InboxPage.test.tsx` 補驗列表 digest 到決策 RPC。
- 側欄失敗由本分支引入：修改前原測試 12 通過／1 失敗；以 `origin/main`（`84b73ae2`）的 `AppSidebar.tsx` 臨時副本搭配原測試與目前相依模組實跑為 13 通過／0 失敗，副本已清除。原因是 stale 狀態覆蓋 Inbox 的 accessible name；修正後名稱與 tooltip 同時保留 Inbox 和狀態，保留 `?`，原斷言未刪除，新增四種 stale／count 組合回歸測試。
- 實測：三個相關測試檔 32 通過／0 失敗；`npm test` 全量 **377 個檔案、3177 個測試通過，0 失敗**；`npm run build` 通過（exit 0，保留 Vite bundle 大小提示）；`git diff --check` 通過。全量測試記錄 `/tmp/live-events-web-test-full.log`，build 記錄 `/tmp/live-events-web-build.log`。
- 本輪僅修改 `web/src/lib/api.ts`、`web/src/components/layout/AppSidebar.tsx`、其測試、`web/src/lib/approvals-api.test.ts`、`web/src/pages/InboxPage.test.tsx` 與本筆記；獨立一般程式碼審查無阻擋發現。未執行 cargo、commit、push、stash、切分支或建立 worktree；未進行 live gateway／原生平台驗收。

## Daybreak 第一輪修補

本節記錄 2026-10-29 的 do-not-ship 修補，取代前文相關舊行為描述。沿用 `feat/live-events-20261029` 與既有未提交變更，未 commit、push、stash、切分支或建立 worktree。下列行號為本輪修補後的位置。

| 發現 | 修補與證據位置 | 回歸測試 |
|---|---|---|
| erase 測試失敗 | 根因是 broker insert 新增 `approval.created`，scrub 的 withdraw 再新增 `approval.decided`，原流程只刪 `knowledge.quarantined`。`wiki_ingest.rs:2955` 在 withdraw 後、替換來源文字前清掉同卡片 lifecycle；三條 erased／forgotten／contact 路徑累加清除數。`events_store.rs:76` 原子寫入 exact card-id tombstone 並刪事件，`:186` 阻擋另一行程遲到 append。原 empty assertion 保留；清除數由 1 改為真實的 3。 | `erase_scrubs_cards_and_events`、`erase_rerun_scrubs_by_contact`、`approval_lifecycle_scrub_is_exact_and_retryable`；含重試、其他卡片保留及重開 store 後的遲到 writer。 |
| P2-1 turn 洩漏識別碼 | `dashboard_feedback.rs:250` 的 started／finished payload 僅 `agent_id`、`source`、`runtime`；移除 session 參數與 dispatch session lookup，task id 僅供既有 goal-round 內部關聯。`channel_reply/inner.rs:1518` 同步。已核對 `web/src/stores/agents-store.ts:43`：事件僅重讀 roster，不使用 session/task id。 | `balanced_turn_pair_is_in_process_and_home_scoped` 檢查恰好三個欄位及無兩種 id；source/runtime matrix；Web roster invalidation。 |
| P2-2 不可信 owner／metadata | `dashboard_feedback.rs:99` 明列各事件取用欄位，task/agent 採 core `is_valid_agent_id`（task packet 同一驗證器）；approval 加 UUID 格式驗證。`:155` 只把 bus 當 changed-id hint，重讀 task owner/status、approval record、agent.toml status；缺列、讀不到、非法 ID、owner mismatch 全丟棄。`task_store/tasks.rs:24` 與 `approval/store.rs:247` 是唯讀查詢，不執行 migration。`handlers/push_filter.rs:223` 再讀 task/approval；`:293` 不接受 payload owner 覆寫。舊 cron/memory/skill/channel 橋接僅輸出驗證過的 agent_id 或空物件，前端只據此重讀。 | `task_bus_projection_excludes_prose_and_rejects_forged_owner`、`approval_and_agent_bridge_use_authoritative_metadata`、`bridge_reads_existing_schema_without_initializing_or_migrating`、push_filter 偽造 owner 測試。 |
| P2-3 解析／fan-out／refetch 負載 | `dashboard_feedback.rs:94` 設 8 KiB byte cap，`:99` 在 JSON parser 前檢查；`:195` 每輪每 (event, entity) 最多一個、最多 64 個候選（同時限制權威查詢與推播），超額／錯誤仍推進 cursor，每輪只記一次計數 log。`autopilot_engine.rs:2424` 接入；`:2201` dashboard-only 類型直接返回，不再被 autopilot 重複 parse，task.created/updated 的 autopilot 消費保持原樣。`web/src/lib/live-events.ts:15` 每個事件訂閱刷新最短 2 秒，leading + trailing，失敗依 2/4/8…60 秒退避；三個 store 與 Inbox 回報成功旗標。 | oversized／flood／cursor 測試；Web flood 50 events/5s、trailing、退避恢復與 in-flight 部分失敗。 |
| P2-4 未授權步驟先入列 | 選擇交辦允許的 exact run_id 方案：`live_events.rs:52` 拒絕 agent-wide；`:108` listener 帶連線身分驗權，`:183` 通過與 runs.get 同一 metadata gate 才 enqueue。`server.rs:16029` 注入 gate，`:16233` 出列再 fresh check，才處理 lost／20/s rate。 | `private_and_revoked_steps_never_enter_queue_or_signal_loss` 對私有與撤權後各送 100 步；`authorization_work_is_bounded_without_pre_authorization_loss` 送 1000 候選；selector／history／hidden-turn 測試。 |
| P3 撤權 cache | task／approval 的 identity、task owner 與 audience 都 fresh read；`push_filter.rs:119` 不保留 task cache，task identity 在 filter 當下讀取，approval 原 async gate 同樣 fresh。其他 agent/global 推播仍用既有最多兩秒 identity cache，這項限制只記實作筆記，不作 CHANGELOG 功能宣告。 | `forged_owner_and_immediate_revocation_never_use_cached_authority`、`owner_and_audience_changes_apply_to_next_task_event`，無等待 TTL。 |

路徑未特別註明者均相對 `crates/duduclaw-gateway/src/`。OpenAPI、三語 `docs/features/*24-task-board.md` 與 CHANGELOG 的未發行訂閱描述已同步。

### 本輪驗證

測試指令與最終結果補在下表；本輪 cargo 全部序列執行、`-j 4`、`CARGO_INCREMENTAL=0`，未跑 cargo fmt 或全量 gateway suite。Gateway 均帶 `DUDUCLAW_P1_PILOT_BINARY="$PWD/target/debug/duduclaw"`。

- 首次 `cargo test ... --lib dashboard_feedback` 編譯失敗：既有 `skills_install_scan_tests.rs` 還用舊的兩參數同步橋接函式。已更新 home + await，並提供權威 agent fixture；未刪原推播斷言。後續結果見下方追加。
- Web 定向：`cd web && npx vitest run src/stores/live-events.test.ts src/stores/agents-store.test.ts src/stores/stale-response.test.ts src/stores/tasks-store.partial.test.ts src/pages/InboxPage.test.tsx`：5 檔、41 通過、0 失敗。
- Web 全量：`cd web && npm test`：377 檔、3181 通過、0 失敗。記錄 `/tmp/live-events-daybreak-web-full.log`。
- Web build：`cd web && npm run build`：exit 0；仍有 bundle size 提示。記錄 `/tmp/live-events-daybreak-web-build.log`。
- 一般獨立 code review：首次發現 task metadata 若呼叫 TaskStore::open 會重跑 migration，已改為唯讀單筆查詢並加測試；第二次建議 legacy payload 精簡也已採納；末次窄複查無剩餘阻擋。這不是 Daybreak 安全複審。

### Rust 最終實測結果

共用指令（各 filter 分別執行，同時只有一個 cargo）：

```sh
CARGO_INCREMENTAL=0 DUDUCLAW_P1_PILOT_BINARY="$PWD/target/debug/duduclaw" \
  cargo test -j 4 -p duduclaw-gateway --lib '<filter>' -- --test-threads=4
```

| `<filter>` | 通過 | 失敗 | 結果 |
|---|---:|---:|---|
| `dashboard_feedback` | 18 | 0 | 含最後補強的合法超大 JSON 斷言；初次通過後最後重跑仍 18/0 |
| `wiki_ingest::tests::erase_` | 2 | 0 | 原 empty assertion 及 contact 重試均通過 |
| `push_filter` | 6 | 0 | 含偽造讀者已綁定 owner 與無 TTL 等待的撤權 |
| `live_events::tests` | 5 | 0 | queue、loss、admission cap、seq |
| `runs_live` | 6 | 0 | 私有／撤權步驟不入列、不標 lost；history／hidden turn |
| `events_store` | 9 | 0 | exact scrub、tombstone、遲到 writer、既有讀寫 |
| `approval::tests::live_event_cases` | 7 | 0 | lifecycle、digest、expiry 等 |
| `task_privacy_tests` | 10 | 0 | 既有 task 隱私回歸 |
| `skills_install_scan_tests` | 9 | 0 | 橋接新簽章與權威 agent fixture |
| `live_step_identity_tests` | 1 | 0 | channel 固定 turn identity |
| `live_dispatch_step_tests` | 2 | 0 | Claude dispatch／非 Claude 行為 |
| `runtime_round_boundaries_close` | 1 | 0 | turn／goal round 配對 |
| `autopilot_engine::tests::row_to_event` | 7 | 0 | 既有 autopilot 映射回歸 |
| `review_cards_of_forgotten_rows_are_withdrawn_and_scrubbed` | 1 | 0 | forget-source 卡片清除與重試 |
| `live_events`（較廣篩選） | 5 | 1 | WebSocket fixture 的 `TcpListener::bind` 被 sandbox 拒絕 |
| `approval`（較廣篩選） | 276 | 12 | 全部失敗皆在 TCP／Unix socket fixture bind，未進入功能斷言 |

filter 可能選到重複測試，不把上表相加宣稱為不重複測試總數。最終 `git diff --check` 通過；分支仍為 `feat/live-events-20261029`。本輪未重跑 core／CLI 全套、gateway 全套或 clippy；交辦提供的既有數字不當成本輪實測。Rust 保留既有 unused/dead-code warnings 與 macOS linker unwind-size 提示。

日誌：`/tmp/live-events-daybreak-<filter 將 :: 換成 ->.log`；例如 `/tmp/live-events-daybreak-dashboard_feedback.log`、`/tmp/live-events-daybreak-approval.log`、`/tmp/live-events-daybreak-live_events.log`。

**未完成驗證：13 個 socket 相關測試。** 錯誤一致為 `Os { code: 1, kind: PermissionDenied, message: "Operation not permitted" }`。當前 sandbox 禁止 bind，且執行環境不允許權限提升；未換路徑繞過，未刪除或放寬測試。可在允許 socket 的原環境重跑上述 `live_events` 與 `approval` 指令。

- `server::live_events_socket_tests::broker_database_bridge_reaches_ws_with_filtered_sequence_and_loss`：`server.rs:17914` 的 TCP bind。
- `approval::delivery::provider_roundtrip_tests::interleaved_account_thread_transport_scope_never_crosses_and_error_cleans_up`：`test_channel_provider.rs:98` 的 TCP bind。
- `codrive::tests_atspi_locate::consequential_locate_step_still_waits_for_approval`、`codrive::tests_registry::consequential_api_action_step_still_waits_for_approval`、`codrive::tests_takeover::explicit_take_over_step_completes_and_skips_approval`：`codrive/tests.rs:87` 的 Unix socket bind。
- `codrive::tests_mode::{plan_approval_approved_lets_the_session_run, plan_approval_denied_aborts_without_ever_connecting, plan_approval_expiry_is_a_denial, plan_approval_off_files_no_card_at_all}`：`codrive/tests_mode.rs:71` 的 Unix socket bind。
- `workflow::f1b_e2e_tests::activation_card_is_admin_only_and_reports_self_approval`、`workflow::f5a_e2e_tests::a_card_that_lapsed_with_its_run_ends_as_approval_expired`、`workflow::resume_tests::resume_approval_step_then_human_gated_effect_completes_the_same_run`、`workflow::security_race_tests::security_race_request_approval_write_cannot_revive_terminal_projection`：`workflow/pilot_test_factory.rs:59` 的 TCP bind。

### 限制與未驗證

- 每連線最多 4 個非排隊的 run 授權工作；全忙時新候選靜默捨棄，不發 events_lost，避免未授權活動透過 loss 洩漏。授權工作完成順序可能不同；以 `runs.get` 重讀補齊。
- exact run 訂閱需要先知道 run_id。dispatch id 直到完成記錄才產生，接著立即批次 publish，一般用戶端無法提前訂閱該批步驟；dispatch 仍應讀歷史，沒有宣稱這條路已能實時串流。
- byte cap／coalesce 僅限制 dashboard bridge。events.db 仍允許原有 task/autopilot payload，SQL fetch 仍會載入該列 TEXT；沒有宣稱限制整體資料庫容量或 autopilot 的原有解析成本。位元組上限、每輪 256 列解析上限，或通過權威查詢之後的 64 筆傳送額度，任一造成捨棄時仍推進游標，並在該輪廣播一次不含筆數與 id 的 `events_lost`。無效或查無的 id 不占那 64 筆額度。任務、員工狀態與待處理清單收到 `events_lost` 會重讀；這三份清單沒有另外的定時備援。
- Web 2 秒與退避限制的是事件驅動刷新；手動操作、初次載入與既有五分鐘 fallback 維持原行為。
- erase tombstone 長期保留 opaque approval id，不含 source/card text。舊版 binary 或直接 SQL 寫入不受新版 append 防線約束；已送入記憶體／瀏覽器的通知無法收回。
- 尚未重跑指定模型 Daybreak 安全審查，亦未進行真實 gateway/runtime／Windows／Linux 驗收、merge、部署或發版。本輪測試不等於安全或上線放行。

### 本輪實際修改檔案

- Rust：`dashboard_feedback.rs`、`channel_reply/inner.rs`、`goal_loop/tests/escalate_cases.rs`、`autopilot_engine.rs`、`handlers/push_filter.rs`、`task_store/tasks.rs`、`approval/store.rs`、`events_store.rs`、`wiki_ingest.rs`、`handlers/runs_live.rs`、`live_events.rs`、`server.rs`、`run_steps.rs`、`claude_runner.rs`、`handlers/tests/skills_install_scan_tests.rs`（以上均在 `crates/duduclaw-gateway/src/`）。
- Web：`web/src/lib/live-events.ts`、`web/src/stores/{agents,approvals,tasks}-store.ts`、`web/src/stores/live-events.test.ts`、`web/src/pages/InboxPage.tsx`。
- 文件：`CHANGELOG.md`、`docs/api/openapi.yaml`、`docs/features/24-task-board.md` 及 `zh-TW/`、`ja-JP/` 版本、本紀錄。
- 工作樹其餘原有未提交變更保留；沒有把整個 working tree diff 誤算成本輪新寫內容。

## Daybreak 第二輪修補

本輪補 Daybreak 複審的 4 個 MEDIUM。實作已在工作樹，尚未 commit。本 session 沒有終端機，`cargo check`、`cargo test`、`git diff`、`git show`、`vitest`、`npm test`、`npm run build` 都沒有執行，所以下面沒有通過／失敗數。第一輪那組數字不能拿來當本輪結果。

### MEDIUM-1：隔離卡片與事後刪除

判定：晚到的新 UUID 卡片是 `origin/main` 既有行為，這條分支沒有改那扇窗，因此沒有在這裡重做隔離流程。

證據：公開倉庫 commit `7040fac74c88ed38b4bad860cb5cdf186affb395`（前一輪記錄的 `origin/main`）上，`crates/duduclaw-gateway/src/wiki_ingest.rs` 的 `dispatch_quarantine_side_effects` 在第 1911–2057 行。順序是先 `live_memory_ids`，再 `events.append("knowledge.quarantined")`（第 2016 行），再 `broker.request`（第 2024–2033 行，新 UUID），最後才做 post-check 並 `scrub_review_store_for_forgotten`（第 2041–2052 行）。本工作樹同一函式第 1911–2057 行與此相同。這不是本機 `git show origin/main:...` 的輸出；本 session 沒跑 git。若本機 `origin/main` 已離開 `7040fac`，要以本機那次 `git show` 覆核。

這條分支新增的 `approval.created` 寫在既有卡片插入的同一筆 SQLite 交易裡，沒有在交易外再發一次：

- `crates/duduclaw-gateway/src/approval/store.rs:167-239`：`action_kind == knowledge_quarantine` 時先 `ATTACH events.db`，`INSERT` 卡片與 `insert_quarantine_creation_event` 同一筆 `Immediate` 交易；成功後跳過交易外的 `emit_lifecycle`。
- `crates/duduclaw-gateway/src/approval/live_events.rs:75-93`：事件列用 `NOT EXISTS scrubbed_approval_events`。註解寫明這只收掉卡片提交前多出來的非同步窗，擋不住 erase 之後另開一張新 UUID 卡片。跨資料庫 WAL 在斷電時也不保證原子。
- `knowledge.quarantined` 仍是上面那個既有 append，沒有進 `dashboard_feedback.rs:80-91` 的 `DASHBOARD_EVENTS`，所以沒有另開一條 live push。

後續：erase 與晚到 `broker.request` 之間的新 UUID 卡片競爭仍在。墓碑只認得當時那張卡的 id。

### MEDIUM-2：待審卡片的即時推送與列表同一套可見性

`approvals.list` 與 `approvals.get` 都走 `handlers/dispatch_org.rs:669-676`，再進 `handle_approvals_list`。`fresh_reader` 每個請求只讀一次（`handlers/approvals_topology_rpc.rs:36-48`）；每一列仍用 `ApprovalVisibility::load`，再以那份身份呼叫 `may_read`（`:63`）。身份讀不到時整筆請求回 `permission denied`，不改成空清單。即時推送在 `handlers/push_filter.rs:123` 對同一個 frame 載入同一型別，`filter_prepared`（`push_filter.rs:297-306`）只拿當下身份對那份快照呼叫 `may_read`。同步的 `filter()`（`push_filter.rs:263`）仍直接丟棄 approval，socket 路徑用的是 `server.rs:16336` 的 `filter_prepared`。

`ApprovalVisibility::may_read` 在 `handlers/approval_visibility.rs:135-160`，種類分流是 `dashboard_only_reader`（`:86-106`）：

- 必須是當下的 Manager，且沒有待改密碼。
- `knowledge_add_source`：Admin 直接可看（集合不存在也一樣，對齊 `add_source_approval::may_decide` 的 Admin 短路）。其他人要有該集合的 Editor 或 Owner（`acl::role_from_grants`，使用者或部門授權）。不另要求員工 Viewer。集合讀不到時，非 Admin 拒絕。
- 只有當下 Admin：`operator_cli_gate::spec_for` 的種類、workflow activation（`is_activation_record`，action kind 或 payload）、`agent_file_change`、`support_pilot_review`、`requires_board`（`strategic_plan`、`agent_hire`）。
- `knowledge_quarantine` 與 `expert_skills_enable`：員工 Viewer。決定路徑只要求 Manager；可見性不放寬成所有 Manager。`expert_skills_enable` 的 `agent_id` 是套件 slug。
- 其餘種類：員工 Viewer。通過種類閘之後，再檢查工作流啟用快照與未綁定任務的受眾。

測試在 `approval_visibility.rs:183`（非編輯者的 Manager 收不到、編輯者與 Admin 收得到、停權與刪 ACL 之後下一幀擋下）與 `:243`（`knowledge_quarantine` 跟員工 Viewer，撤銷綁定後列表與 live 都是 0）。這兩個測試本輪沒跑。

文件同一句話改過：`docs/features/24-task-board.md:274`、`docs/features/zh-TW/24-task-board.md`、`docs/features/ja-JP/24-task-board.md`，以及 `docs/api/openapi.yaml` 的 `approvals.list` 說明。

### MEDIUM-3：假 id 不能吃掉真失效通知

`dashboard_feedback.rs:94-97` 把額度拆開：傳送額度 64（`MAX_BRIDGE_EVENTS_PER_TICK`），解析／工作上限 256（`MAX_BRIDGE_ROWS_PER_TICK`）。`bridge_tick`（`:197-237`）先取 `bridge_hint` 當去重鍵，再經 `dashboard_push_frame` 做 `project_hint`。查無或偽造的 id 只記 dropped，不進 64 筆額度，也不發 `events_lost`。超過位元組上限、256 列工作上限，或通過權威查詢之後仍超過 64 筆，才把 `lost` 設上，游標照樣前進，並廣播一筆 payload 為 `{}` 的 `events_lost`（沒有筆數、沒有 id）。

前端 `web/src/lib/live-events.ts:3-7` 與 `:15`：任務失效清單含 `events_lost` 與 `connection.ready`，刷新下限 2 秒。`web/src/stores/tasks-store.ts:87-88` 與 `agents-store.ts:44-47` 都訂閱 `events_lost` 並重讀。`web/src/stores/live-events.test.ts:163` 斷言這條重讀。第一輪紀錄第 233 行已改成這個契約，不再寫「64 個候選後由列表重讀恢復」。

### MEDIUM-4：授權磁碟工作離開 socket 的 select

每個 raw frame 只組一次快照。`server.rs:16029-16053` 在 `OnceCell` 裡訂閱 `event_tx`，呼叫 `PreparedPush::prepare`，再把 `Arc<PreparedPush>` 廣播給連線。init future 先複製 `event_tx` 與 `home_dir`，避免 future 借走 `AppState` 同時又借 `prepared_events`。每條連線在 `server.rs:16336` 只跑 `filter_prepared`。

磁碟工作在 `push_filter.rs:82-88` 的 `spawn_blocking`，全行程 semaphore 8。任務受眾在 `PreparedPush::load` 讀一次；連線端 `push_filter.rs:184-191` 在快照 id 相符時不再重讀最多 256 個 packet。審批快照在 `approval_visibility.rs:118-129` 載入集合 ACL、工作流與任務受眾。讀者身份每次 `fresh_reader`（`approval_visibility.rs:165-174`）重讀，沒有把身份放進 TTL。`allows_log_tail`（`push_filter.rs:203-208`）仍在 select 迴圈上做同步讀取，這輪沒改。

沒有同時連線數上限。`server.rs:31-44` 的 `check_ws_rate_limit` 是每個 IP 每分鐘 30 次新連線。依交辦，這輪不加同時連線上限，記成後續。

### 第二輪後續修正（審查回饋）

審查回報兩條 gateway 測試失敗（Manager 看得到 `responsibility_operator_change` 列表、看得到 `computer_workspace_admin` 即時推播），以及 `dashboard_push_frame` 沒有正式呼叫端。四項都改在工作樹，本 session 沒有終端機，沒有跑 cargo。

1. 可見性依種類，list／get／live push 共用 `may_read`。`dashboard_only_reader` 在 `handlers/approval_visibility.rs:86-106`，套用在 `:135-160`。決定路徑旁的對照註解在 `handlers/approvals_decide.rs:195-196`。新增測試 `knowledge_add_source_editor_sees_list_and_push_non_editor_does_not`（`approval_visibility.rs:282`）：非 Admin 的集合 Editor 在列表與推播都看得到，沒有編輯權的 Manager 兩邊都看不到。
2. ACL 比對抽成 `duduclaw_knowledge::acl::role_from_grants`（`crates/duduclaw-knowledge/src/acl.rs:355-376`）。`user_role` 在存在檢查與 Admin 捷徑之後呼叫它（`:403-427`）。`may_read` 在 `approval_visibility.rs:147` 呼叫同一函式。單元測試 `role_from_grants_matches_user_department_and_skips_disallowed`（`acl.rs:600`）。
3. `approvals.list` 每個請求只讀一次身份：`handlers/approvals_topology_rpc.rs:36-48`。失敗（含 `spawn_blocking` join 失敗）回 `permission denied`（`:46`），不跳過單列、不回空的成功清單。列迴圈的 `may_read` 在 `:63`。
4. `dashboard_feedback.rs:226` 的 `bridge_tick` 改呼叫 `dashboard_push_frame`（函式本體仍在 `:190`）。去重鍵仍用前面那次 `bridge_hint`；額度與 `events_lost` 的條件沒改。

### 本輪測試

未執行。預定指令（一次只跑一個 cargo，`-j 4`，`CARGO_INCREMENTAL=0`）：

```
CARGO_INCREMENTAL=0 cargo check -j 4 -p duduclaw-gateway --tests
CARGO_INCREMENTAL=0 cargo test -j 4 -p duduclaw-gateway --lib push_filter
```

以及同一套旗標、filter 分別為 `dashboard_feedback`、`live_events`、`runs_live`、`events_store`、`approval`、`wiki_ingest`、`approvals_topology`。需要 pilot binary 的測試要設 `DUDUCLAW_P1_PILOT_BINARY=$PWD/target/debug/duduclaw`。Web：`cd web && npx vitest run src/stores/live-events.test.ts`，然後 `npm test` 與 `npm run build`。

會 bind localhost 的測試若被 sandbox 擋下，應如實記 `Operation not permitted`，不要略過。中斷前使用者回報工作樹約有 2 個 `duduclaw-gateway` 編譯錯誤；本輪補了測試模組的 `use std::sync::Arc`、`task_privacy_tests` 的可見性改成 `pub(in crate::handlers)`，以及 `get_or_init` 前先複製 sender 與 home。這三處都還沒經過 rustc。

### 後續

- 隔離列在 liveness 檢查之後、`broker.request` 之前被 erase，仍可能寫出墓碑不認得的新 UUID 卡片。這是 `7040fac` 上已有的窗。
- 沒有 WebSocket 同時連線上限，只有每 IP 每分鐘 30 次新連線。
- 本輪 cargo、vitest、本機 `git show` 都還沒跑。

## Daybreak 第三輪（替代審查）修補

Daybreak Blue 這輪沒有上場，Codex 額度用完。第三輪由 Grok 唯讀審查，加上一位獨立的 Claude 審查。下面的行號是改完之後的位置。寫這段的 session 沒有終端機，沒有跑 cargo。

### A：訂閱回應不再分辨拒絕原因

`runs.subscribe` 與 `runs.unsubscribe` 共用 `handle_runs_subscription`（`handlers/runs_live.rs:6-26`）。`authorize_live_run`（`:32-82`）內部仍分開記「Run not found」、`permission denied`、Filtered 的「run steps are not visible to this reader」，`tracing::debug` 留在 `:17-19`。回到客戶端的句子一律是 `Run not found`，dispatch 與 session 兩支都走這一個出口。外層不再先做同步的 `bound_history_live`。解析錯誤（`RunSelector::parse`）維持原句。

測試 `subscription_refusals_are_all_run_not_found`（`runs_live.rs:287`）：不存在的 `dispatch:999999`、讀者看不到的員工的既有 run、Filtered 讀者看既有 session run，三個回應相同，都是 `Run not found`。授權讀者仍可訂閱。

### B：授權離開 socket 的 select，並共用 push 的 blocking pool

`push_blocking`（`handlers/push_filter.rs:82-91`）改成 `pub(crate)`，仍是全行程 8 個 permit 的 `spawn_blocking`。`runs_live.rs:85-92` 的 `block_auth` 走這個 pool，不另開。Dispatch 先在 blocking 裡讀身份與 `get_dispatch_run`（`load_dispatch_live`，`:101-113`），中間的任務閘維持 async，再 blocking 重讀身份。Session 先 blocking 讀身份，await `resolve_run`，再以 `confirm_session_run`（`:117-148`）一次做完 sessions.db、history mode 與覆核。每次檢查都重讀身份。

入隊仍用 `may_push_run_step`（`server.rs:16073-16078`）。送出前的覆核改在 `forward_authorized`（`live_events.rs:143-163`），由 `server.rs:16080-16099` 的 spawned task 寫進容量 1 的 channel。select 臂（`server.rs:16293-16314`）只收已通過覆核的 step，做 selector、`take_lost`、既有速率限制，然後送出。覆核失敗直接丟掉，不設 `events_lost`。容量 1 的通道裡最多留一筆已經通過覆核的 step，這是這次拆法留下的窗。

既有測試 `revoked_binding_denies_existing_subscription_delivery`（`runs_live.rs:183`）保留。新增 `queued_step_is_dropped_when_reader_revoked_before_send`（`:332`）：還有權限時先送出一筆，解除綁定後同一筆已排隊的 step 不再送出，且 `take_lost()` 為 false。這個測試打的是 `forward_authorized`，沒有把整段 socket select 跑起來。

### C：Admin-only 種類改讀當下角色

`support_pilot_review`（`approvals_decide.rs:213-220`）與 `kind.requires_board()`（`:222-229`）都改呼叫 `require_admin_decider(&self.home_dir, ctx)`。拒絕句維持原樣。Operator-CLI 那支原本就走同一個函式（`:202-208`）。

測試 `demoted_or_suspended_admin_cannot_decide_board_or_pilot_cards`（`approvals_decide.rs:833`）：連線上下文仍宣稱 Admin，users.db 降成 Manager 或停權之後，`strategic_plan` 與 `support_pilot_review` 都拒絕，卡片維持 pending。角色與狀態恢復後，同一位使用者可以決定這兩張卡。

### D：權威庫錯誤要發出 events_lost

`project_hint`（`dashboard_feedback.rs:159-200`）改回 `Result<Option<String>, ()>`。`TaskStore::live_metadata` 與 `ApprovalStore::get_readonly` 的 `Err` 是 `Err(())`；查無、偽造、狀態檔讀不到、序列化失敗是 `Ok(None)`。`dashboard_push_frame`（`:202-206`，`#[cfg(test)]`）只留給測試，簽名仍是 `Option<String>`，`Err` 經 `unwrap_or_default` 折成 `None`。`bridge_tick`（`:242-261`）直接看 `project_hint`：`Err` 設 `lost`、不寫進 64 筆傳送額度；`Ok(None)` 只記 dropped。游標照舊在列首前進，一輪最多一筆 `events_lost`。

測試 `lookup_error_sets_events_lost_and_missing_rows_do_not`（`dashboard_feedback.rs:804`）：沒有 `tasks.db` 時合法 id 會丟 `events_lost`；開過的庫裡查無的 id，以及過不了 hint 的偽造 id，不發 `events_lost`。`approvals.db` 是目錄時，合法 UUID 的 `approval.created` 發 `events_lost`。80 筆查詢錯誤（超過 64 的傳送額度、低於 256 的工作上限）不佔額度，同一輪後面的 `cron.changed` 仍送出，且只有一筆 `events_lost`。

### E：沒有訂閱者不開 sessions.db

`has_run_subscriber`（`live_events.rs:167-186`）看 hub 裡有沒有 selector 對上這個 run id。鎖中毒時放行，呼叫端仍做自己的查詢。`run_steps.rs:310-332` 在 inline 的 channel tool step 上，先用 `{session}#{row}` 問 hub，沒有訂閱者就跳過開啟與 publish。有訂閱者時仍跑原本的 hidden/undone 查詢，查到才 publish。

測試 `has_run_subscriber_tracks_exact_run_id`（`live_events.rs:242`）。既有 `channel_steps_stay_on_the_host_turn_and_never_fall_back_after_undo` 在有訂閱者時仍要送出，這次沒有改它的斷言。

### 這輪留下的後續

- 沒有 WebSocket 同時連線數上限。`server.rs:31-44` 的 `check_ws_rate_limit` 只限制每個 IP 每分鐘 30 次新連線。
- 晚到的隔離卡競爭仍在：隔離列在 liveness 檢查之後、`broker.request` 之前被 erase 時，仍可能寫出墓碑不認得的新 UUID 卡片。這是 `7040fac` 上已有的窗，這輪沒動。
- `runs.get` 的 dispatch 分支對任務閘失敗仍回既有的 permission denied 畫面（`handlers/runs_rpc.rs:192-197`）。這是 base 上的行為，這輪沒改。訂閱路徑已經收成 `Run not found`。
- 本段的 cargo 沒有在這個 session 跑。

## 第三輪修補的驗證與獨立審查（Claude，2026-10-30）

寫碼者更動：Codex 額度用完（至 2026-11-04），Grok 額度也在 2026-10-30 用完，依備援規則以下修補由 Claude 撰寫。Daybreak Blue 同樣不可用，審查改由獨立的 Claude subagent 唯讀進行。

驗證（Claude 在沙箱外實跑，第三輪修補與 clippy 修正後）：core 1073/0、knowledge 174/0、cli 2283/0、gateway lib 10260/0、web 3182/0 與 build、新增行 clippy 0。上一段「本段的 cargo 沒有在這個 session 跑」指的是寫碼當下，之後已由 Claude 補跑。

獨立審查結果：A、C、D、E 關閉；B 關閉但修法本身帶進一個 MEDIUM 與一個 LOW。

| 發現 | 嚴重度 | 處理 |
|---|---|---|
| 每條 WebSocket 連線的 run 轉送任務與 listener 永不釋放：轉送任務持有 `RunSubscriptions`，listener 擁有 inbound 的 sender，所以 `inbound.recv()` 永遠不會回 `None`；連線結束後任務仍在，hub 清單只增不減 | MEDIUM（第三輪修法引入） | `live_events.rs` `forward_authorized` 改成同時等 `inbound.recv()` 與 `outbound.closed()`，連線丟掉接收端即結束並釋放 listener。新增測試 `forwarder_exits_and_releases_listener_when_connection_closes` |
| 撤權後最多仍可送出兩筆撤權前已授權的步驟（一筆在通道、一筆卡在 `send().await`） | LOW（第三輪修法引入） | 收到步驟後先 `outbound.reserve()` 取得空位，再授權，通過後 `permit.send`；撤權後最多剩一筆已在通道裡的步驟，加上當下正在寫出的那一筆。連線關閉時，排隊中的授權也會一併放棄 |
| 已看不到的 run 無法 `runs.unsubscribe`（取消訂閱也走完整授權） | LOW（先前輪次既有） | 未改，列後續。直接跳過授權會把 handle 解析後的原始 session id 回傳給已無權限的讀者；要不洩漏又能移除，需改訂閱儲存結構（同時記住用戶端送的 id）。影響是重新連線前佔一個名額並重複做會失敗的授權 |
| 任務閘（`task_privacy.rs`）仍在 tokio worker 上同步讀 users.db 與受眾檔，不在共用 blocking pool 內 | LOW（不在 select 迴圈上） | 未改，列後續 |

後續待辦（累計）：WebSocket 同時連線數上限；晚到的隔離卡競爭（base 既有）；`runs.get` dispatch 分支的 permission denied 區分（base 既有）；`runs.unsubscribe` 對已不可見 run 的移除；任務閘移入 blocking pool。
