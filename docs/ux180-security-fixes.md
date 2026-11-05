# UX180 安全審查修補交接（2026-11-05）

狀態：PARTIAL（第二輪 2 High＋1 Low 修補與測試已寫入，遠端編譯／執行測試及 Daybreak 覆審待完成）。僅修改 `/Users/lizhixu/Project/DuDuClaw-wt-ux180`，保留既有未提交變更；未 commit、未跑 cargo fmt、未本機建置 gateway／CLI，未建立新 worktree。

原安全報告：`/Users/lizhixu/.baton/runs/20261105-101751-codex-75237-143.md`（完整讀取）。原實作交接：`/Users/lizhixu/Project/commercial-wt-v180-linux/docs/plans/v180-handoff-reports/ux180.md`（僅讀取，未修改）。

## 第二輪修補（報告 20261105-120542）

本輪完整讀取 `/Users/lizhixu/.baton/runs/20261105-120542-codex-75237-194.md`。交辦的 H1、H2、L1 實作已寫入；原 NO-GO 尚未經遠端測試及指定安全覆審解除。

| Finding | 寫入的修正 |
| --- | --- |
| H1 跨 writer 繞過 | `CronStore::insert` 與所有設定 writer 共用 `BEGIN IMMEDIATE` admission。只要任一 `on_behalf_of`／`created_by_user_id`／`scoped_by_user_id` 存在，就檢查 15 分鐘 cadence、UTC、每歸屬人 20 筆與每代理 50 筆啟用上限。MCP-only principal 與 Dashboard marker 合併計數；多個相同歸屬只計一次。空白／不合法 principal 不降級成 Admin。 |
| H1 建立窗口與更新 | 每歸屬人每小時 20 次建立／新接管，receipt 與資料同交易，失敗會回滾；刪除及重啟不退款。一般更新／恢復不重複收建立次數，新歸屬人接管舊資料也受窗口限制。批次依名稱恢復整批交易，後一筆超限會全部回滾。單純暫停仍可縮減不合規舊資料。 |
| H1 scheduler | reload、workflow tick、workflow enqueue、fresh legacy dispatch 都拒絕不合規 scoped schedule；在 parse／trigger／模型前跳過，只記錄 count，不記 id、principal、cron 或 task。保留原本執行前 snapshot／權限重查。 |
| H2 run_now | spawn 前在同一記憶體鎖內檢查每排程 3 次／每人 10 次的 10 分鐘滑動窗口、每人 2 件／全域 4 件 outstanding，並 `try_acquire_owned` 執行席位。無席位就回覆明確 busy，不建立排隊 task；鎖 poisoned 或 semaphore 關閉時拒絕。RAII 在完成、early return、取消及 panic 釋放。 |
| L1 長度 | 非 Admin `cron.add`／`cron.update` 在 normalizer/parser 前檢查 256 bytes；共用 cadence validator 也先檢查長度，保護 store／scheduler 路徑。 |

邊界：未帶任何來源標記的 Admin 建立資料仍沿用原行為；帶人類來源的 MCP 紀錄一律受 store invariant 約束，即使該人是 Admin。MCP 人類來源新增排程未指定時區時預設 UTC；未標記的背景排程保留原本本地時區偵測。舊的非 UTC 人類來源排程會拒絕啟用／執行，仍可暫停／刪除或調整為 UTC。

手動執行 admission 為每個 scheduler instance 的記憶體狀態，gateway 重啟會清空窗口；user／row 索引各最多 4096 keys；過期的 row 窗口移除，user 窗口另須無 outstanding 才移除，滿額拒絕。已受理但後續失敗仍計入速率；拒絕不消耗次數。沒有新增 request-id 冪等 API，重試會依一般請求計次。既有未標記 Admin 手動路徑未改動。

### 本輪測試與實際驗證

新增 **18 個 Rust 測試**，全部尚未執行：

- store 4 個：三種來源 marker 與所有設定／恢復入口、跨 MCP／Dashboard 配額及持久窗口、換代理與同名批次原子回滾、unmapped／malformed principal。
- MCP writer 3 個：實際 `handle_schedule_task`、`handle_update_cron_task`、`handle_pause_cron_task`、`handle_delete_cron_task` 的 cadence／配額／建立窗口、背景更新保留人類限制、接管舊資料與回滾。
- scheduler 1 個：舊版／直接 SQL 留下的高頻三種 marker、cached snapshot 重讀、reload 跳過與未標記 Admin 回歸。
- run_now 9 個：row 跨人窗口、user 跨 row 窗口、精確到期、outstanding／global、忙碌／關閉、不消耗拒絕 receipt、8 執行緒競爭、abort／error 釋放、poisoned lock、索引容量及 scheduler 實際拒絕入口。
- RPC 1 個：新增及修改的 256-byte 接受邊界、257-byte 拒絕、多位元字串與無部分寫入。

另調整兩個既有 store fixture：建立上限種子資料的 receipt 到期設定，以及 Admin fixture 真正清空 scoped marker，避免以舊繞道建立非法測試資料。

| 檢查 | 本輪結果 |
| --- | --- |
| `git diff --check` | 通過。另檢查本輪新檔的尾端空白。 |
| SQLite SQL smoke | 通過：從現碼擷取 schema 與 migration，初始化兩次；33 個 production SQL 以 `EXPLAIN` 檢查，實際驗證跨 marker／principal 計數、原筆排除、不重複計數、交易 rollback。不等同 Rust 測試。 |
| 獨立普通 code review | 靜態覆讀無阻擋級發現；已採納建議，將 run_now 的 SQLite 失敗轉成固定 admission unavailable，避免 RPC 回傳資料庫細節。 |
| Rust 編譯／測試 | 0 次，依交辦由呼叫端遠端執行。未本機 build、未跑 cargo fmt。 |
| Daybreak／實際登入／真實模型執行 | 未執行，不宣稱安全覆審或實機驗收通過。 |

本輪遠端測試命令（由呼叫端執行）：

```bash
cargo test -p duduclaw-gateway cron_store
cargo test -p duduclaw-gateway cron_scoped_rpc_tests
cargo test -p duduclaw-gateway principal_tests
cargo test -p duduclaw-gateway scoped_run_now_
cargo test -p duduclaw-cli mcp_cron_shared_admission -- --test-threads=1
cargo test -p duduclaw-cli mcp::tests::part7 -- --test-threads=1
```

本輪修改檔案共 17 個：

| 檔案 | 本輪內容 |
| --- | --- |
| `crates/duduclaw-gateway/src/cron_store.rs` | 所有設定 writer 的共用交易、principal index、批次原子恢復。 |
| `crates/duduclaw-gateway/src/cron_store/scoped.rs` | marker／principal 配額與建立窗口、轉移檢查、長度上限、4 個新測試與 fixture 修正。 |
| `crates/duduclaw-gateway/src/cron_scheduler.rs` | cadence 執行前檢查、手動 admission 接線及固定錯誤。 |
| `crates/duduclaw-gateway/src/cron_scheduler/run_now_admission.rs` | 新增手動速率／outstanding／semaphore admission 及 9 個測試。 |
| `crates/duduclaw-gateway/src/cron_principal_tests.rs` | scheduler 舊資料拒絕／Admin 回歸。 |
| `crates/duduclaw-gateway/src/handlers/cron_scoped_rpc.rs` | 解析前長度檢查與清楚 busy／rate-limit 錯誤。 |
| `crates/duduclaw-gateway/src/handlers/tests/cron_scoped_rpc_tests.rs` | byte 邊界回歸測試。 |
| `crates/duduclaw-cli/src/mcp/cron_tasks.rs` | 人類來源新增排程預設 UTC。 |
| `crates/duduclaw-cli/src/mcp/tests/part7.rs` | 3 個 MCP writer 回歸測試。 |
| `CHANGELOG.md` | 本輪安全修補。 |
| `docs/features/05-security-defense.md`、`docs/features/zh-TW/05-security-defense.md`、`docs/features/ja-JP/05-security-defense.md` | 三語共用 invariant、手動執行及限制。 |
| `docs/guides/dashboard-settings.md`、`docs/guides/zh-TW/dashboard-settings.md`、`docs/guides/ja-JP/dashboard-settings.md` | 三語使用限制與 UTC 行為。 |
| `docs/ux180-security-fixes.md` | 本交接紀錄。 |

## 第一輪修補決策（保留原交接脈絡；cron 限制以第二輪為準）

- **H1**：新增與編輯的 quota、cadence 檢查和資料寫入在同一個 `BEGIN IMMEDIATE` SQLite 交易。每建立者／最近操作人最多 20 筆啟用排程；每代理最多 50 筆，含 Admin 現有排程。啟用／更新也檢查，編輯原筆不重複計數，暫停／刪除可縮減權限。每帳號滾動一小時最多建立 20 筆；獨立 receipt 不隨刪除排程消失，重新開啟 DB 仍計數。常數在 `cron_store/scoped.rs`。Admin 使用原本不受此限的寫入 API。
- **H1 cadence**：最短 15 分鐘，檢查所有秒／分／時組合與跨午夜間距，避免僅抽樣近期日期漏掉指定未來年份的高頻表達式。保守視選定日期為連續日。員工 API 不接受時區欄位，並拒絕接管非 UTC 的舊排程（允許空值與零時差 UTC/GMT 別名），避免 DST 壓縮間隔；這類紀錄仍可讀取、暫停、刪除，Admin 可管理。讀取回傳的操作能力與限制一致，過快的 UTC 排程仍可編輯修正。
- **H2 選擇限制主機執行對象**：本次不建置 container cron 路徑。員工新增／編輯／恢復／手動執行只允許存在且 `[container] sandbox_enabled = false` 的代理。registry 與磁碟上的 effective typed configuration 都要允許，缺檔或解析錯誤拒絕。`created_by_user_id`／`scoped_by_user_id` 保留來源；手動執行帶上目前操作人標記。排程執行與呼叫模型前重新檢查，改成 sandbox 或目標不可用時暫停並記錄 skipped。host runner 沿用代理 runtime、capabilities 與 native sandbox 設定；既有未帶員工標記的 Admin 排程保留 host 行為。設定檔讀取與程序啟動之間仍不是原子鎖，與一般 runtime 載入一致。
- **H2 輸入**：非 Admin 的新增／更新，掃描實際 `[Scheduled Task: {name}] {task}` 全文，使用對話既有 `scan_input_with_audit` 和相同 threshold；拒絕時留下 security audit，不保存排程。名稱也要掃描，因為它會進入模型提示。恢復／手動執行也掃描舊 task，防止接管既有未掃紀錄。
- **M1**：Admin 看全部可列示代理；其他帳號只看直接 Viewer 以上綁定、users.db 中本人目前部門，以及這些代理的 reports_to 祖先。範圍外祖先僅 name＋role，沒有部門、狀態、顯示名稱或父節點欄位。排除隱藏代理，祖先遍歷防循環。前端相容最小占位資料，不推定缺失狀態。
- **L1**：小型 `handlers/dashboard_scope.rs` 在 extension dispatch 前執行最低角色表。明列本輪 16 個 RPC；未知 cron／reminders／assign_schedules 方法，以及 agents.org_chart／skills.recent 的子方法預設 Admin。其他既有 namespace 保持現有分派權限，逐筆 Viewer／Operator、建立者及即時帳號檢查仍保留。

未變更 `agent_id` 禁止轉移、Viewer 綁定只讀、操作／執行需 Operator、最近技能每使用者隔離的既有契約。原建立者失去綁定後仍可暫停／刪除本人排程，但不能啟用或執行。

## 第一輪驗證與新增測試

| 檢查 | 實際結果 |
| --- | --- |
| `git diff --check` | 通過。 |
| SQLite SQL smoke | 通過：以 Python SQLite 執行從現碼擷取的三段 schema SQL、逐欄 migration，初始化及重開兩次；receipt 表與四個 admission indexes 存在。不是 Rust 執行結果。 |
| 獨立普通程式碼審查 | 完成，名稱注入與 UTC capability flags 意見均已修正；最後覆讀無剩餘發現。不是 Daybreak 安全覆審。 |
| Rust / TypeScript / Vitest | 實際執行 0；依交辦 gateway／CLI 由呼叫端遠端建置。前端依賴不存在，未安裝。 |
| Daybreak / 實際登入 / 真實模型執行 | 未執行，不宣稱安全審查或實機驗收通過。 |

本次新增 **18 個 Rust 測試**：store 配額／速率／cadence／跨連線交易 6 個；scoped RPC 掃描稽核／sandbox／高頻／非 UTC 4 個；scheduler sandbox 變更及 host/Admin 回歸 2 個；組織圖 live department、祖先最小化與 Admin/Manager 3 個；角色表及 extension 前拒絕 3 個。另外重寫原組織圖無綁定及 archived 測試契約。全部尚待遠端執行。

## 第一輪待呼叫端執行

```bash
cargo test -p duduclaw-gateway cron_store
cargo test -p duduclaw-gateway cron_scoped_rpc_tests
cargo test -p duduclaw-gateway principal_tests
cargo test -p duduclaw-gateway org_chart_
cargo test -p duduclaw-gateway dashboard_scope
cargo test -p duduclaw-gateway recent_skills
cargo test -p duduclaw-gateway assign_schedule
```

前端具備依賴的環境：

```bash
cd web
npx tsc -b
npx vitest run src/pages/OrgChartPage.test.tsx src/pages/RoutinesPage.test.tsx src/components/skills/RecentSkillChips.test.tsx
```

編譯／測試通過後仍須由指定 `gpt-daybreak-blue-latest` 針對實際差異覆審；原 NO-GO 報告未被本次普通 code review 取代。

## 第一輪修改檔案（不含僅由前次修改的檔案）

| 檔案 | 本輪內容 |
| --- | --- |
| `CHANGELOG.md` | 安全修補與 UTC 限制。 |
| `crates/duduclaw-gateway/src/cron_store.rs` | receipt/index migration、交易式新增及編輯 admission。 |
| `crates/duduclaw-gateway/src/cron_store/scoped.rs` | 新增配額、cadence、UTC、速率 helper 與 6 個測試。 |
| `crates/duduclaw-gateway/src/cron_scheduler.rs` | 主機對象 gate、磁碟設定重讀、手動來源標記、執行前複查。 |
| `crates/duduclaw-gateway/src/cron_principal_tests.rs` | 2 個 scheduler 回歸測試。 |
| `crates/duduclaw-gateway/src/handlers/cron_scoped_rpc.rs` | 名稱＋task 掃描、限制接線、操作能力回傳。 |
| `crates/duduclaw-gateway/src/handlers/tests/cron_scoped_rpc_tests.rs` | 4 個 RPC 回歸測試。 |
| `crates/duduclaw-gateway/src/handlers/agents_list_rpc.rs` | 組織圖可見範圍與最小祖先。 |
| `crates/duduclaw-gateway/src/handlers/dispatch_core.rs` | 組織圖傳入 caller。 |
| `crates/duduclaw-gateway/src/handlers/tests/live_acl_tests.rs` | 組織圖新契約與 3 個測試。 |
| `crates/duduclaw-gateway/src/handlers/dashboard_scope.rs` | 新增最低角色表與 1 個單元測試。 |
| `crates/duduclaw-gateway/src/handlers/mod.rs` | 註冊角色表並在 extension 前執行。 |
| `crates/duduclaw-gateway/src/handlers/tests/dashboard_scope_tests.rs` | 2 個 extension/live role 測試。 |
| `crates/duduclaw-gateway/src/handlers/tests/mod.rs` | 登記新測試模組。 |
| `web/src/lib/api.ts` | 組織圖占位欄位型別。 |
| `web/src/components/OrgChart.tsx` | 最小占位顯示與缺值處理。 |
| `docs/features/05-security-defense.md` | 英文安全契約。 |
| `docs/features/zh-TW/05-security-defense.md` | 繁中安全契約。 |
| `docs/features/ja-JP/05-security-defense.md` | 日文安全契約。 |
| `docs/guides/dashboard-settings.md` | 英文使用限制。 |
| `docs/guides/zh-TW/dashboard-settings.md` | 繁中使用限制。 |
| `docs/guides/ja-JP/dashboard-settings.md` | 日文使用限制。 |
| `docs/ux180-security-fixes.md` | 本交接紀錄。 |
