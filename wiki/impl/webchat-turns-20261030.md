# WebChat 並行回合與取消：實作及驗證紀錄

> 原始日誌只留在本機 `artifacts/`，未進 repo；內文指向 `round*-logs/` 的連結不會解析。

日期：2026-10-30。以下先保留第一輪歷史紀錄（當時狀態 PARTIAL）；最新修正與驗證請見下方 **Round 6**。

分支 `feat/webchat-concurrent-turns-20261030`，HEAD `8d30a212`。未 commit、push、stash、切分支、發版或移除使用者提供的 worktree。

## Registry 與通訊

`WebChatState` 持有所有 socket 共用的 `Arc<Registry>`。每筆記錄包含已驗證使用者、實際 session id、conv、client msg_id、server UUID、取消 token 與交付狀態。每個 session 用 oneshot 前後連結維持 FIFO；ticket 同時持有使用者額度與 FIFO 位置，離開時釋放。不同 session 可以並行，同一 session 在不同 socket 上仍共用同一條佇列。

`config.toml [webchat] max_in_flight_turns` 預設 **3**、允許 **1–64**，執行中與排隊中都占額度。跨 socket 合計；超額立即回 `error` / `turn_limit`，相同 session 的重複 pending msg_id 回 `duplicate_message`。設定無法讀取或值錯誤時拒絕新回合。

F3 ownership 與 C-ACL 檢查保留；排隊結束後，依原本明確選取／resume／routing 的判斷再次檢查員工權限。等待中的 `/交辦` 處理也移進 FIFO worker。

Worker 只持有 outbound channel，不持有 WebSocket sink。回合訊框新增 `session_id`、`conv`、`msg_id`，保留原有欄位；reader 不等待自己發出的 ack，worker 則等待實際 socket write 的 ack，使 CCR guard 涵蓋真正的傳送期間。

連線上限在驗證前按 IP 計算 3 條，驗證後改按已驗證使用者計算 3 條；全域仍為 10 條。關閉 socket 不取消已接受的回合或排隊訊息，工作繼續完成。

## 取消語意

- `cancel` 指定 `session_id` 或 `conv`；有 `msg_id` 時可選特定執行中或排隊回合，省略時選 active turn。外人／未知／不明確／已進入交付階段者回 `turn_not_found`，無副作用。命令開始執行後不能取消。
- 取消會 drop scoped work，沿用既有 CLI `kill_on_drop` 與 process-group guard。`sessions.db` 寫入 system 訊息，JSON 含 `status=cancelled`、server `turn_id`、client `msg_id`。既有 session 的員工 owner 不變；新建立的取消 session 使用已驗證的使用者 origin。
- ApprovalStore 增加 nullable `origin_session_id`、`origin_turn_id`。來源限 gateway task-local scope 或既有 host runtime 環境變數，沒有採用工具參數。`ApprovalBroker::withdraw_turn` 先持久化 tombstone，再透過既有 `withdraw` CAS 撤回精確 session + server turn 的 pending approvals，記錄 `decided_by=system:cancel:webchat_turn`。其他回合與已決定項目不受影響；較晚到達的 MCP 插入會被原子檢查拒絕。
- raw pipeline 與 `GuardedReply` constructor 增加 rollback guard，避免取消時留下尚未完成來源驗證的 assistant row。Guard 建立時捕捉清理工作收集器；FIFO ticket 會等待資料庫撤回工作完成，錯誤／panic 也向上傳遞。
- 取消紀錄、approval withdrawal、delivery cleanup 全成功才回 `cancelled`，否則回 `cancel_failed`。排隊訊息的取消清理與回覆會等前一回合結束。已完成動作或已建立的交辦任務不回滾。

## Egress

取消沿用 `SinkLease::Drop`／既有 release 判斷，不清空整回合 leases、不假造 `terminal_response`。未送出或已確認終態者可正常釋放；已送出但未確認結果者保留。CLI/MCP 被終止後不能執行 Rust Drop，尚未回報 release 的持久 lease 同樣保留至既有 **6 小時 TTL**。取消不繞過外送決策。

## 修改範圍

完整清單見 `changed-files.txt`。

- Gateway：`webchat.rs`、新增 `webchat/turns.rs`、`webchat/turns/run.rs`、registry／process／socket tests。
- Approvals：`approval/mod.rs`、`approval/store.rs`、新增 `approval/turn_origin.rs` 與相關測試。
- Reply pipeline：`channel_reply/inner.rs`、`guarded.rs`、`ccr_delivery.rs`、`tests/ccr_guarded_reply.rs`。
- 測試替身：`agent_channels/e2e_seam.rs`，按 temp home 隔離，沒有真模型或 API 呼叫。
- Dashboard：`web/src/stores/chat-store.ts`、`chat-store.concurrent.test.ts`，依 conv/msg_id/session 分流，背景錯誤不清掉前景回合。未新增取消按鈕。
- 文件：CHANGELOG Unreleased Added/Changed，以及英文、繁中、日文 dashboard settings 的 WebChat 協定說明。

## 驗證命令與結果

所有 Cargo 呼叫均串行，前綴為 `CARGO_TARGET_DIR="$PWD/target" CARGO_INCREMENTAL=0`，固定 `-j 4`。未跑 workspace fmt 或整個 workspace test suite。

### 編譯

`cargo check -j 4 -p duduclaw-gateway --tests`

exit 0。

```text
Finished `dev` profile [unoptimized + debuginfo] target(s) in 1m 34s
```

保留既有 54 項 library-test warnings（48 duplicates）。

### WebChat

`cargo test -j 4 -p duduclaw-gateway --lib webchat`

```text
test result: FAILED. 103 passed; 1 failed; 0 ignored; 0 measured; 10265 filtered out; finished in 2.76s
```

通過項目包含真實本機測試子行程終止、取消入庫、approval 隔離／晚到請求拒絕、CCR rollback／錯誤傳遞，以及下列完整協定測試：

```text
test webchat::turns::socket_tests::webchat_memory_socket_concurrency_fifo_user_limits_and_foreign_cancel ... ok
```

這個測試使用 Axum 的實際 HTTP upgrade、WebSocket client、reader/writer，以 `tokio::io::duplex` 提供記憶體傳輸，沒有 DNS 或網路連接埠。驗證單連線雙 session 並行、同 session FIFO、跨連線使用者額度、本人／外人取消、斷線續跑、每人 3 條及全域 10 條連線限制。TCP 與記憶體 transport 共用同一組情境。

唯一失敗是 TCP 測試在開始網路行為前被沙箱拒絕：

```text
WebChat socket integration cannot run: loopback bind failed (including sandbox PermissionDenied): Os { code: 1, kind: PermissionDenied, message: "Operation not permitted" }
```

**TCP 行為驗收未執行，不列為通過。** 真實供應商 CLI 的端到端取消也未測；本機子行程測試使用相同 process-group／kill-on-drop 路徑。

### 外送租約

`cargo test -j 4 -p duduclaw-gateway --lib egress_registry_`

```text
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 10364 filtered out; finished in 2.04s
```

涵蓋未送出的取消釋放、已送出結果未明時持續保護、未追蹤 sink，以及等待核准後重新檢查。

`cargo test -j 4 -p duduclaw-gateway --lib egress_two_phase_lease_retains_uncertain_dispatch_until_ttl`

```text
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 10368 filtered out; finished in 0.05s
```

### Clippy

`cargo clippy -j 4 -p duduclaw-gateway --tests -- -D warnings`：exit 101；先被未修改的 `duduclaw-license` 5 項、`duduclaw-identity` 5 項 lint 阻擋。

再執行 `cargo clippy -j 4 -p duduclaw-gateway --tests --no-deps -- -D warnings`：exit 101。

```text
error: could not compile `duduclaw-gateway` (lib) due to 435 previous errors
```

診斷位於未改動程式碼（含 `channel_reply/inner.rs` 的既有行）；有位置資訊的診斷與本次新增行沒有交集，見 `clippy-scope.json`。**嚴格 clippy 並未通過**，library 失敗後 test target 的 lint 也未完成。未修改無關模組或放寬 lint。

### Dashboard

在 `web/` 執行：

- `npm ci --offline`：exit 0。
- `npm exec tsc -- -b`：exit 0，無輸出。
- `npm test -- src/stores/chat-store.test.ts src/stores/chat-store.thinking.test.ts src/stores/chat-store.identity.test.ts src/stores/chat-store.computer-watch.test.ts src/stores/chat-store.concurrent.test.ts src/stores/chat-thinking.test.ts`

```text
Test Files  6 passed (6)
Tests  68 passed (68)
```

- `npm run build`：exit 0；`✓ 4153 modules transformed.`、`✓ built in 599ms`。已產生 Rust embed 所需靜態檔。保留既有 Vite native-config 與 >500 kB chunk warning。
- `git diff --check`：exit 0。

## 尚未完成的驗收

1. 在允許 loopback bind 的環境重跑 TCP 整合測試。
2. 處理或獨立確認既有 clippy baseline；嚴格 test-target lint 未完成。
3. Claude 的完整測試／原生驗收尚待執行；沒有部署或供應商實機驗收。
4. 此 session 沒有可呼叫的 Daybreak／Grok delegation 工具，因此指定的 Daybreak 安全審查未執行。已完成獨立唯讀 subagent diff 審查並修復其阻擋問題，但不把它當成 Daybreak 審查。
5. Dashboard 取消按鈕未加入；協定端已可傳 `cancel`。

## Round 2（2026-10-30）：雙重審查修正

本節取代上方第一輪關於 system 取消紀錄、只靠 kill-on-drop、未限制全域回合、取消逐筆撤回，以及 pending assignment 移入 FIFO 的描述。沿用使用者指定工作樹與分支；沒有 commit、push、stash、切換分支或 workspace `cargo fmt`。本 session 沒有建立 worktree，因此沒有移除這個使用者提供的工作樹。

### 逐項修正

下列路徑省略 `crates/duduclaw-gateway/src/`，除非另註明。

| 項目 | 修改與證據位置 |
|---|---|
| H1 | `session.rs:471` 把取消寫到 `webchat_turn_cancellations`，不存在的 session 不建立；同交易核對 owner。`session.rs:494`、`channel_reply/inner.rs:915` 只讀 `sessions.summary` 的真壓縮摘要，system 控制列不再覆蓋摘要。新增實際 history RPC、下一回合實際 prompt 測試，包含舊 system cancel JSON。 |
| H2 | `crates/duduclaw-core/src/process_tree.rs:92` 共用原有 `spawn_guarded`／`ManagedChild` 與 Windows Job Object；`claude_runner.rs:3581` streaming 改用 ManagedChild；`crates/duduclaw-llm/src/mcp_client.rs:534` stdio MCP 同樣護住整棵程序樹。Claude channel、Codex、Grok、Antigravity、generic CLI 原本已走 guarded helper；本輪補的是 streaming 與 API MCP。測試走真 streaming、真 MCP connect、production spawn helper，再確認孫行程結束。 |
| H3 | `webchat.rs:799` widget 全連線期間保留 IP slot；JWT 使用者仍以 user slot 計算。`webchat/turns.rs:43,146` 同一 registry 原子限制全部使用者的 active＋queued 回合，`max_global_turns` 預設 10、範圍 1–64，讀取失敗拒絕。實際 handler 測 IP 三條、跨身分重連仍受 global cap、全域十條連線。 |
| M1 | `approval/turn_origin.rs:122` 同一 BEGIN IMMEDIATE 寫 tombstone、撤回 pending、收集已核准 ids；`approval/store.rs:350` 與 `approval/binding.rs:392` 的 decision SQL 同步檢查 fence。`cancelled.approved_before_cancel` 明示取消前已核准，不宣稱回滾該動作。既有 withdraw 沒有通知事件，沿用讀取已提交狀態的 polling lifecycle。 |
| M2 | `approval/turn_origin.rs:74` env 半組來源拒絕。已評估既有 HMAC：只驗 agent id，session／turn 新簽章需要各 runtime 與 MCP 傳遞新 token，本輪未實作；檔頭與三語文件明載 Bash 可偽造 env 的邊界。 |
| M3 | `approval/turn_origin.rs:98` open／expiry／cancel sweep；新增有回合來源的核准 TTL 上限 30 日，tombstone 至少 31 日、舊更長 TTL 延長保留。硬上限 100,000，先刪符合期限者，仍滿額便拒絕新增 fence，不逐出近期保護紀錄。補期限與硬上限測試。 |
| M4 | `webchat/turns.rs:297` 監督 panic 並回帶 session/conv/msg_id 的 internal_error。`channel_reply/ccr_delivery.rs:79,155` unwind 後先等待所有撤回工作；等待資料庫期間取消也不會遺失撤回工作。補 building panic、完成 build 後 panic、FIFO 讀回及 pool 忙碌測試。 |
| M5 | `webchat.rs:1301` 控制錯誤使用 request_id，不冒用目標 turn msg_id；registry 先判 duplicate 再判 quota，避免滿額重送造成錯誤終態。`web/src/stores/chat-store.ts:363` 控制錯誤不終止其他 turn，新增 4 個 vitest 回歸。 |
| M6 | `webchat/turns.rs:173,192,276` ack 與 writer 共用 10 秒絕對 deadline；排隊過期或 ack receiver 已關閉便拒絕傳送，傳送中也監看 receiver 結束；所有 socket 寫入（含 ping/auth）有上限。補 stalled ack、stalled sink、過期排隊 frame 不得送出的測試。 |
| L1 | `approval/turn_origin.rs:41` 嚴格驗證限 gateway scope；完整但 malformed 的 feedback/env 來源 warn 後忽略，半組 env 仍拒絕。測試以子行程隔離 env，避免污染其他測試。 |
| L2 | `approval/store.rs:177,198` `approval_turn_origins` 記錄 keyed 多來源；另一來源仍 live 時不撤回共用卡，tombstoned retry 遇已存在卡片不錯誤。只有同 contract 才加入新來源。 |
| L3 | `webchat/turns/run.rs:300` begin_delivery 拒絕即同步 refuse_delivery；`channel_reply/guarded.rs:355` WebChat 即使沒有 CCR lease 也保留 rollback 至交付。回歸測試走 production attach helper，不只手造 GuardedReply。 |
| L4 | `webchat.rs:917,1211,1286` arrival 取得 seq 與穩定 event，pending assign 先於 routing；使用既有 arm_on_admission，queued `/交辦` 不會被下一訊息超車。已開始 arming 的命令在 admission 標成不可取消；無 msg_id 的命令亦不會在 worker 重複 arming。補真 socket 三訊息序列。 |
| L5 | `webchat/turns.rs:308` 重查與 admission 同一 predicate；純 explicit selection 不額外要求 may_call。`WebRoute.called` 區別同員工的 @ 呼叫，仍重查 operational/may_call。ACL 等待中取消優先，照常回 cancelled 並記錄。 |
| L6 | `webchat.rs:1306` 取消方立即收到 cancel_accepted＋request_id，最終結果仍送原回合 sink；補跨 socket 測試。 |
| L7 | `session.rs:471` current owner 檢查與 marker insert 同一 immediate transaction，不符拒絕；補 owner changed 測試。 |
| L8 | `approval/store.rs:151` ALTER 錯誤重新讀 schema，欄位已存在則視為另一 process 完成升級；否則保留錯誤。 |
| L9 | cleanup failure 使用 `type=notice`，舊 dashboard 忽略，避免 assistant_done 後第二個錯誤泡泡；取消清理失敗仍有 cancel_failed 終態。 |
| L10 | `webchat/turns/socket_tests.rs:138` boxed tungstenite error，移除 result_large_err 的大 Err 回傳。 |

### 界線與審查

- Grok PTY fallback 在 base 已由 `PTY_TREE_CLEANUP_VERIFIED=false` 及入口拒絕關閉，WebChat 沒有可達的 PTY spawn。沒有修改 `duduclaw-cli-runtime`、沒有宣稱修復或驗收 ConPTY。
- Windows 沿用既有 Job Object 實作，本輪未跑 Windows 原生驗收。已送到遠端的 HTTP/API/MCP 操作不能靠本機取消回滾；Unix group guard 也不是抵抗刻意脫離 process group 的 OS sandbox。
- M2 依允許方案保留未簽署 env origin，沒有把 agent-only HMAC 當成 turn 綁定。
- 獨立 code-reviewer 檢視實際工作樹，發現後已修復 duplicate-at-capacity、過期 queue frame、assignment arrival、same-agent @、無 lease rollback 與取消中遺失 revocation 等競態；最後靜態複查沒有剩餘阻擋。沒有可呼叫的 Daybreak/Grok 審查工具，這次不是 Daybreak 安全審查。
- TCP 若 bind 被沙箱拒絕會明確印出 NOT RUN 並跳過；即使 Rust harness 因提前 return 標示 ok，也不當作 TCP 行為驗收。

### Round 2 驗證紀錄

本輪 19 項修正皆已落地；**驗收為 PARTIAL（26 個 loopback bind 測試受沙箱限制）**。可執行的指定回歸皆通過。沒有未完成的程式修正；M2 採明載未簽署的允許方案，H2 的不可達 PTY 與 Windows 原生驗收限制如上。

最終版本 `cargo check --tests` 通過，原始命令、sandbox 排除重跑與實際結果如下。所有 Cargo 均串行，固定 `CARGO_TARGET_DIR="$PWD/target" CARGO_INCREMENTAL=0`、`-j 4`。本輪沒有重跑整個 workspace 或嚴格 clippy；L10 以 boxed error 修正，沒有以 allow 壓掉它。

前端在 `web/` 執行（本輪前端檔案之後未再修改）：

- `npm exec tsc -- -b`：exit 0，無輸出。
- `npm test -- src/stores/chat-store.test.ts src/stores/chat-store.thinking.test.ts src/stores/chat-store.identity.test.ts src/stores/chat-store.computer-watch.test.ts src/stores/chat-store.concurrent.test.ts src/stores/chat-thinking.test.ts`：

```text
Test Files  6 passed (6)
Tests  72 passed (72)
```

- `npm run build`：exit 0。

```text
✓ 4153 modules transformed.
✓ built in 599ms
```

保留 Vite native-config 與 >500 kB chunk 既有警告。


#### Rust 最終結果

`CARGO_TARGET_DIR="$PWD/target" CARGO_INCREMENTAL=0 cargo check -j 4 -p duduclaw-gateway --tests`：exit 0。

```text
Finished `dev` profile [unoptimized + debuginfo] target(s) in 3m 06s
```

保留既有 library-test warnings（54 warnings，48 duplicates）。

下表測試指令同樣帶上前述環境變數；結果行未改寫。`FAILED` 行中的失敗皆發生在 loopback bind，**測試情境未執行，不是驗收通過**。

| 命令 | 實際結果行 |
|---|---|
| `cargo test -j 4 -p duduclaw-gateway --lib webchat` | `test result: ok. 123 passed; 0 failed; 0 ignored; 0 measured; 10274 filtered out; finished in 3.75s` |
| `cargo test -j 4 -p duduclaw-gateway --lib 'approval::'` | `test result: FAILED. 181 passed; 1 failed; 0 ignored; 0 measured; 10215 filtered out; finished in 2.61s` |
| `cargo test -j 4 -p duduclaw-gateway --lib 'approval::' -- --skip approval::delivery::provider_roundtrip_tests::interleaved_account_thread_transport_scope_never_crosses_and_error_cleans_up` | `test result: ok. 181 passed; 0 failed; 0 ignored; 0 measured; 10216 filtered out; finished in 2.67s` |
| `cargo test -j 4 -p duduclaw-gateway --lib channel_reply` | `test result: FAILED. 178 passed; 2 failed; 0 ignored; 0 measured; 10217 filtered out; finished in 1.60s` |
| `cargo test -j 4 -p duduclaw-gateway --lib channel_reply -- <精確排除 2 個 bind 測試>` | `test result: ok. 178 passed; 0 failed; 0 ignored; 0 measured; 10219 filtered out; finished in 1.65s` |
| `cargo test -j 4 -p duduclaw-gateway --lib egress_registry_` | `test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 10393 filtered out; finished in 2.03s` |
| `cargo test -j 4 -p duduclaw-gateway --lib process_group` | `test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 10388 filtered out; finished in 0.31s` |
| `cargo test -j 4 -p duduclaw-llm --lib mcp_client` | `test result: FAILED. 44 passed; 22 failed; 0 ignored; 0 measured; 270 filtered out; finished in 2.41s` |
| `cargo test -j 4 -p duduclaw-llm --lib mcp_client -- <精確排除 22 個 bind 測試>` | `test result: ok. 44 passed; 0 failed; 0 ignored; 0 measured; 292 filtered out; finished in 2.39s` |

精確排除命令的完整 argv 在 [round2-sandbox-retry-commands.json](round2-sandbox-retry-commands.json)，26 項未執行清單在 [round2-sandbox-unrun-tests.txt](round2-sandbox-unrun-tests.txt)。原始完整輸出在 [round2-logs/](round2-logs/)。

另外執行 `cargo test -j 4 -p duduclaw-gateway --lib webchat_real_socket -- --nocapture`，確認輸出：

```text
NOT RUN: WebChat TCP integration: loopback bind denied by sandbox: Operation not permitted (os error 1)
```

因此 WebChat 的 `123 passed` 是 harness 結果，其中包含 1 個明確提前跳過的 TCP 案例；實際執行 122 個，不把 TCP 列為行為通過。Memory HTTP upgrade/WebSocket 整合情境確實執行並通過。

實際程序樹測試已通過，包含：

- `claude_runner::process_tree_cancellation_tests::webchat_claude_streaming_cancel_kills_grandchild`。
- `mcp_client::process_tree_cancellation_tests::mcp_stdio_cancel_during_handshake_kills_grandchild`。
- gateway `process_group` 共 9 個測試。

首次最終回歸曾是 `121 passed; 2 failed`：兩個新測試的 fixture 未建立 `agents/`，在進入產品邏輯前失敗。已只補上目錄建立、保留全部撤回斷言，獨立複查確認後重跑至上列 `123 passed; 0 failed`；原始失敗紀錄保存為 `round2-logs/webchat-round2-webchat-fixture-failure.log`。

`git diff --check`：exit 0。分支仍為 `feat/webchat-concurrent-turns-20261030`、HEAD 仍為 `8d30a21275b347212a07b6f3751f7c5dc8ccb677`。工作樹檔案清單見 [round2-working-tree-files.txt](round2-working-tree-files.txt)。

#### 尚待外部環境驗收

1. 在允許 loopback bind 的環境重跑上述 26 個網路測試。
2. 真供應商與 Windows 原生程序樹驗收未執行；Grok PTY 仍是 base 原有的停用路徑，並未啟用或宣稱驗收。
3. Daybreak 安全複查沒有可呼叫工具，本輪只有獨立唯讀 code review；env origin 沒有新增簽章。

CHANGELOG Unreleased 與三語 WebChat 文件已同步。文件也區分登入使用者與 widget 訪客：widget 重連後的身分不同，不能繼續或取消前一條連線的回合，但舊工作仍占全域額度。

## Round 3

日期：2026-10-30。本節記錄 Daybreak Blue 第二輪安全複查所列 R3-M1～M3、R3-L1～L3 的修正；取代 Round 2 的 tombstone 滿額拒絕、malformed fallback origin 忽略與無界進度佇列行為。沿用指定分支及工作樹，未 commit、push、stash、切分支或執行 workspace `cargo fmt`。本 session 未建立 worktree，保留使用者提供的工作樹。

### 逐項修正

以下相對路徑省略 `crates/duduclaw-gateway/src/`，除非另註明。

| 項目 | 修改及證據位置 |
|---|---|
| R3-M1 | `approval/turn_origin.rs:98,123` 移除 100,000 筆 tombstone 容量拒絕；取消仍在 immediate transaction 寫入 fence 並撤回該回合的 pending approvals。僅取 `approval_turn_origins` 關聯核准的最大 TTL 計算保留期；無回合來源的長 TTL 不再延長保留。保留至少 31 日及 legacy 關聯長 TTL 保護，不逐出尚在保護期間的 fence。測試：100,000 筆後仍撤回、持久 fence、拒絕晚到 insert；未關聯長 TTL 可清理，已關聯 legacy 長 TTL 仍保留。 |
| R3-M2 | `channel_reply/ccr_delivery.rs:405` 的 `confirm_send` 在任何 send error 呼叫 `refuse_delivery()`；`webchat/turns/run.rs:440,466,483` 讓 `no_reply` 與 `assistant_done` 共用此路徑，失敗時撤回本次新建封存副本；既有 delivery cleanup collector 等待歷史列撤回後才釋放 FIFO。補 ack timeout、closed writer、closed ack 與 successful ack 後關閉的歷史／delivery verdict 測試。刻意不回覆與序列化失敗也標記未交付。 |
| R3-M3 | 新增 `webchat/turns/outbound.rs:5,54,62,157`：每 socket 32 筆有界控制 FIFO＋1 筆可覆寫進度槽；`webchat.rs:850` 使用獨立 writer，reader 的控制訊框滿額時背壓，worker 終態沿用實際寫入 ack 與 10 秒絕對 deadline。`webchat/turns/progress.rs:9` 把 callback 的無界通道改成每回合 1 筆最新進度槽，ModelInfo 另外保留。終態增加 `progress_dropped` 並記錄 log，清掉本回合舊進度；有丟棄時省略無法代表實際送出事件的 thinking 統計。測試涵蓋兩層十萬次進度覆寫、控制 FIFO 滿額背壓、reader 恢復後終態優先實際寫出。 |
| R3-L1 | `approval/turn_origin.rs:41,60,83`：gateway scope、feedback task-local、env 只要有來源但不完整或無效就拒絕核准，log 記錄來源與拒絕原因，沒有記錄來源原值。只有來源完全 absent 才保留 unscoped。環境測試以子行程隔離，涵蓋完整有效、完全 absent、半組、空值、控制字元、Unix non-UTF-8；另測 gateway／feedback malformed 與半組。沒有新增 origin 簽章。 |
| R3-L2 | `session.rs:101` 每條 pooled connection 啟用 foreign keys；`:739,844,920` 在手動 purge、GDPR 與 retention purge 明確刪除 cancellation rows；`:505,515,907` 新增每 session 1,000 筆上限與 `min(session retention,90日)` sweep，啟動時也清理 90 日以前／orphan／超額標記。測試強制第二條連線執行 purge，另外關閉 FK 驗證 explicit delete，並驗證 30／180 日 retention 與 per-session cap。 |
| R3-L3 | `crates/duduclaw-core/src/process_tree.rs:57` 將巢狀 `if` 合併為 let-chain，沒有放寬 lint。 |

### 複查時補強

- 進度合併不能吃掉 `thinking.kind=redacted`，否則舊 dashboard 會留下先前已顯示、後來遭拒的文字。新增 `webchat/turns/progress.rs:52` 不可覆寫的 redaction latch，最多追蹤 64 個已允許顯示的 block；觸發後不再顯示新的思考文字，先以既有 redacted 控制訊框清除已追蹤區塊，再送終態。不需要前端協定變更。測試刻意讓 redaction 在 callback 被 keepalive 覆寫，使用 acknowledged sink 驗證仍走無 ack 控制 FIFO，且清除先於終態。
- 完成 reply 後先同步執行 `begin_delivery()`，才等待最後的 redaction 控制訊框，避免新增 await 讓取消誤發布 `delivered=true`。
- 獨立 code-reviewer 已檢視實際修改並確認上述修正，最後靜態複查沒有剩餘阻擋。這不是新的 Daybreak 安全驗收；本 session 沒有可直接呼叫 `gpt-daybreak-blue-latest` 進行指定唯讀複查的 delegation 工具。
- CHANGELOG `[Unreleased]` 與三語 dashboard-settings 的 WebChat 節已更新。Round 3 未修改 `web/` 程式碼。

### Round 3 驗證紀錄

所有 Cargo 命令串行，使用 `CARGO_INCREMENTAL=0 CARGO_TARGET_DIR="$PWD/target"`，固定 `-j 4`。原始輸出保存於 `round3-logs/`。首次 `cargo check` 的模組可見性錯誤已修正，原始失敗記錄保存為 `check-initial-visibility-failure.log`。

程式修正與可執行回歸已完成；整體驗收標為 **PARTIAL**：14 個不同的 socket-binding 案例因沙箱拒絕而 **NOT RUN**，另有原本標記 ignored 的 13 個 Docker／live CLI 案例未執行。沒有將它們計為行為驗收通過。

`cargo clippy -j 4 -p duduclaw-core --tests`：exit 0。仍有既有 77 項 lib-test warnings（68 duplicates）；輸出沒有 `process_tree.rs` 的 lint，未使用 `-D warnings` 或 lint allow。

```text
Finished `dev` profile [unoptimized + debuginfo] target(s) in 9.17s
```

最新來源的 `cargo check -j 4 -p duduclaw-gateway --tests`：

exit 0，最終輸出：

```text
warning: `duduclaw-gateway` (lib test) generated 54 warnings (48 duplicates)
Finished `dev` profile [unoptimized + debuginfo] target(s) in 5m 56s
```

日誌：[check-final.log](round3-logs/check-final.log)。先前可見性修正後的檢查也成功（7m 04s，`check.log`）；最後兩處交付順序／redaction 修正由本次最終檢查與測試執行檔覆蓋。

測試命令與原始結果行（皆使用本節開頭的環境變數）：

| 命令 | 實際結果行 |
|---|---|
| `cargo test -j 4 -p duduclaw-gateway --lib webchat -- --nocapture` | `test result: ok. 135 passed; 0 failed; 0 ignored; 0 measured; 10275 filtered out; finished in 3.59s` |
| `cargo test -j 4 -p duduclaw-gateway --lib 'approval::'` | `test result: FAILED. 182 passed; 1 failed; 0 ignored; 0 measured; 10227 filtered out; finished in 2.81s` |
| `cargo test -j 4 -p duduclaw-gateway --lib channel_reply` | `test result: FAILED. 182 passed; 2 failed; 0 ignored; 0 measured; 10226 filtered out; finished in 1.61s` |
| `cargo test -j 4 -p duduclaw-gateway --lib session` | `test result: FAILED. 336 passed; 11 failed; 13 ignored; 0 measured; 10050 filtered out; finished in 3.14s` |

以上原始 failure 都停在 bind（`PermissionDenied: Operation not permitted`）。已讀取對應原始碼確認是 TCP 或 Unix socket bind；沒有略過其他失敗。精確排除後重跑：

| 過濾器與排除數 | 實際結果行 |
|---|---|
| `approval::`，精確排除 1 個 bind 案例 | `test result: ok. 182 passed; 0 failed; 0 ignored; 0 measured; 10228 filtered out; finished in 2.86s` |
| `channel_reply`，精確排除 2 個 bind 案例 | `test result: ok. 182 passed; 0 failed; 0 ignored; 0 measured; 10228 filtered out; finished in 1.61s` |
| `session`，精確排除 11 個 bind 案例 | `test result: ok. 336 passed; 0 failed; 13 ignored; 0 measured; 10061 filtered out; finished in 3.12s` |

完整重跑 argv 與 exit code：[sandbox-retry-commands.json](round3-logs/sandbox-retry-commands.json)。去重後 14 項 NOT RUN 清單：[sandbox-not-run.txt](round3-logs/sandbox-not-run.txt)。原本 13 個 ignored 的名稱與原因：[existing-ignored-tests.txt](round3-logs/existing-ignored-tests.txt)。所有原始日誌：[round3-logs/](round3-logs/)。

WebChat 的 `135 passed` 是 Rust harness 結果，其中 TCP 案例提前 return，實際執行且通過 **134** 項。輸出明確記錄：

```text
NOT RUN: WebChat TCP integration: loopback bind denied by sandbox: Operation not permitted (os error 1)
```

記憶體 HTTP upgrade／WebSocket 整合測試確實執行並通過。新增的 cap 取消、origin 各來源拒絕、ack timeout／closed writer／closed ack 撤回及成功 ack 保留、兩層進度洪流、redaction drain、第二連線 purge、retention 與 per-session cap 測試全部通過。首次測試編譯耗時 24m 28s；後續過濾器共用同一執行檔。

Round 3 沒有更動前端，因此沒有重跑 Web 測試；不將前兩輪結果當成本輪重新驗證。沒有執行 Windows 原生測試、真供應商 CLI 取消或新一輪 Daybreak 安全複查，也沒有部署／發版。沙箱拒絕的 14 項仍待外部環境重跑。

`git diff --check`：exit 0。分支仍為 `feat/webchat-concurrent-turns-20261030`，HEAD 仍為 `8d30a21275b347212a07b6f3751f7c5dc8ccb677`。



## Round 4

依 Daybreak Blue 第三輪複查的 R4-M1、R4-M2、R4-L1 修正。沿用 `feat/webchat-concurrent-turns-20261030`，HEAD `8d30a21275b347212a07b6f3751f7c5dc8ccb677`；未 commit、push、stash、切分支或執行 workspace `cargo fmt`。本 session 未建立 worktree，保留使用者指定工作樹及原有修改。

### 逐項修正

以下相對路徑省略 `crates/duduclaw-gateway/src/`，除非另註明。

| 項目 | 修改及證據位置 |
|---|---|
| R4-M1 | `approval/turn_origin.rs:98`：逐筆以 tombstone 的 `session_id + turn_id` 關聯 `approval_turn_origins` 與 approvals，取該來源最大 TTL；保留期為 `max(30日, 該來源最大TTL) + 1日`。共用卡的第二來源也計入，其他來源不影響。`:462` 測試同 session 不同 turn、同 turn 不同 session、共用第二來源、31 日 floor 與 legacy 期限結束後清理。 |
| R4-M2 | `approval/store.rs:189`：舊表新增 nullable `principal` 欄位及 principal／時間索引，保留 legacy rows 並容許並行 migration。`approval/turn_origin.rs:123,129,150`：持久化滾動一小時 100 筆已開始回合取消配額，100,000 筆全域 tombstone admission 門檻；只限制新回合，取消寫 fence 不受門檻阻擋。`webchat/turns.rs:46,104` 與 `webchat.rs:1275`：登入者使用帳號 id，widget 使用 transport IP，接受 ticket 前讀 DB，超限／DB 不可讀回 `turn_limit`，重複 msg_id 維持 `duplicate_message`。`:383,409` 在 work 第一次 poll 才標記 started；未開始的排隊或 ACL 等待中取消，不建立 fence、不消耗取消配額。 |
| R4-L1 | `webchat/turns/outbound.rs:8,88,146`：32 格 control 與 64 格 terminal 分別保留容量，暫存與 channel 共用 semaphore，總量不因 staging 增加。優先挑沒有自身先前 control 的 terminal，避免 A terminal 等控制訊框時連帶卡住 B terminal；同回合較早 control 與多個 terminal 保序，較晚 notice 不超車。`webchat/turns.rs:228`：未開始寫入就過期／ack 已關閉時送 false ack 並略過，writer 繼續；已開始的 socket write 失敗／逾時仍關閉，因可能有半個 frame。 |
| 文件 | CHANGELOG `[Unreleased]`、`docs/guides/dashboard-settings.md` 與 zh-TW／ja-JP 版本 WebChat 節同步取消配額、`turn_limit`／`request_id`、逐筆保留期、終態通道與過期訊框語義。 |

### 複查與界線

- 獨立 code-reviewer 直接讀取工作樹；HOL 補強後再檢查容量、同回合順序與 shutdown，未發現剩餘阻擋。這是靜態程式碼複查，沒有宣稱新的 Daybreak Blue 安全驗收；本 session 未取得可呼叫指定 `gpt-daybreak-blue-latest` 的 delegation 工具。
- 已接受的回合仍可取消，因此取消數與全域 fence 數可能略超 admission 門檻；不逐出有效 fence。舊版 NULL principal 的 fence 無法回推帳號，仍納入全域門檻。
- terminal 保留通道不能中斷已開始的 socket write；寫入仍受既有 10 秒上限約束。
- 額外發現並修正 ack precheck 之後、首次 send poll 之前的關閉競態：`webchat/turns.rs:248` 的 `deliver_acknowledged` 在 timeout 內追蹤真正的 send poll；零 poll 的 ack 關閉／deadline 到期回 false ack 並保留 writer，已 poll 後錯誤仍關閉。`webchat/turns/outbound.rs:466,491,513` 三個 deterministic tests 覆蓋此交錯，獨立 reviewer 複查沒有剩餘阻擋。此補強晚於第一份 `check-final.log`，以最後補跑結果為最新驗證。
- 每次 admission 目前開啟 broker，執行 schema 檢查／backfill／sweep，再讀取消配額；靜態複查指出重複 sweep 的同步成本屬非阻擋效能改善項，未在本輪擴大改寫 broker 生命週期。

### 實測發現後補強

- 第一組 `webchat` 回歸發現同 socket 的取消終態超前 `cancel_accepted`。沒有放寬原記憶體 WebSocket 測試的順序斷言；改為每個回合只保留一份 cancellation receipt，Registry 在發出 cancel token 前綁定。worker 仍立即停止工作並寫 fence／session，僅終態等待自己的 receipt（原始 10 秒 deadline）；成功的 receipt 使用保留優先通道，滿載的其他回合 controls 不會間接卡住取消終態。重複取消回 `turn_not_found`，不替換或累積 receipt。新增測試確認 fence 先落盤、receipt 完成後才有終態，以及滿 32 格 controls 下 receipt／terminal 仍即時依序寫出。
- 第一組 `webchat` 的 widget 案例發生 default-stack `SIGABRT`；同一執行檔單獨重跑也重現，`RUST_MIN_STACK=8388608` 診斷執行通過。沒有以放大 stack 當驗收通過；實作在 reader box admission future，並在進入 cancellation state machine 前 box 大型 `run::run` future，以減少巢狀 async state 的堆疊複製。以最後預設 stack 重跑結果為準。
- 中間一次 `approval::` 編譯發現新增測試的 `unfold(signal, …)` 缺少 `async move`（E0373），已修正，原始失敗日誌保留。
- 獨立 reviewer 再次檢查 receipt 先存後 cancel、鎖不跨 await、fence 先寫再等、reserved receipt 通道與測試，沒有剩餘靜態阻擋。這些補強均需以下最後驗證結果，不引用先前版本的通過當作最新通過。

### Round 4 驗證紀錄

驗證進行中；完成後於本節記錄精確命令、結果與 NOT RUN 清單。原始日誌在 `round4-logs/`。本 session 的 Cargo 串行，固定 `CARGO_INCREMENTAL=0 CARGO_TARGET_DIR="$PWD/target"`、`-j 4`。

第一次 `cargo check -j 4 -p duduclaw-gateway --tests` exit 0：

```text
warning: `duduclaw-gateway` (lib test) generated 54 warnings (48 duplicates)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 8m 46s
```

日誌：`round4-logs/check.log`。執行期間仍補上 terminal HOL 與 migration 測試，因此另跑最新來源的 `check-final.log`。

共用機器限制：首輪編譯開始時 `pgrep` 被 sandbox 拒絕，後續用 `lsof -a -c cargo -d cwd` 發現另一個 `DuDuClaw-wt-v178-integ` 正在跑 Cargo。嘗試只暫停本次 Cargo／rustc 時被 sandbox 拒絕（`Operation not permitted`），沒有終止或改動對方行程。首輪存在外部編譯重疊，不能宣稱整台機器全程只有一個 Cargo；首輪結束後確認無其他 Cargo，後續每個命令都先等 Cargo 空閒再啟動。第二次 check 期間也觀察到 `DuDuClaw-wt-remove-agy` 隨後啟動 Cargo；本 session 無法阻止其他 session 在檢查空閒後開始編譯。


## Round 5

依 Claude 的簡化決策移除 TTL 關聯保留、全域 admission 拒絕與 cancellation receipt。沿用使用者指定分支與工作樹；未 commit、push、stash、切分支、執行 workspace `cargo fmt` 或啟動審查子代理。本 session 未建立 worktree，保留使用者提供的工作樹。

### 逐項變更

以下程式路徑省略 `crates/duduclaw-gateway/src/`。

| 項目 | 修改與位置 |
|---|---|
| R5-1 | `approval/turn_origin.rs:92` 以具名常數定義取消後固定 24 小時期限，移除逐筆核准 TTL join、31 日期限與相關測試；`approval/store.rs:255` 移除帶回合來源核准的 30 日 TTL 上限。人為決定的 tombstone 檢查保留。到期測試使用已撤回的 365 日 TTL 核准，確認 25 小時的 fence 仍會清除。 |
| R5-2 | `approval/turn_origin.rs:99` expiry sweep 每次共最多刪除 1,000 筆；超過 100,000 筆時，依實際時間由最舊開始刪除超過一小時的 rows 並 warn，未滿一小時絕不逐出。全域筆數不參與 admission。每個 principal 滾動一小時 100 個已開始回合取消的配額保留；NULL principal 不計入。三語文件明載同 NAT 共用 IP 配額及最長一小時限制。 |
| R5-3 | `webchat.rs:489,500` 在 WebChatState 初始化一次並共用 broker，取消清理也重用它；`webchat/turns.rs:55,113,161` 先原子保留使用者及全域名額，再查配額，通過才加入 session FIFO，拒絕直接 drop 名額。`approval/turn_origin.rs:137` 查詢移至 `spawn_blocking`，沿用既有 `(principal, julianday(cancelled_at))` 索引。migration 僅在 open 執行；移除 open、admission、cancel 的 sweep，僅保留既有 `expire_stale` 呼叫。測試 counter 確認 2,000 次超過記憶體名額的請求完全不增加 quota read，另驗證 quota refusal 釋放名額與不影響 FIFO。 |
| R5-4 | `webchat/turns.rs:133,422`、`webchat.rs:1322`、`webchat/turns/outbound.rs` 移除 receipt receiver、ack/deadline barrier、專用 receipt send 與專屬測試。registry 僅保存取消 `request_id`，終態 `error/code=cancelled` 攜帶相同 id。`cancel_accepted` 走普通控制通道，盡力傳送，不保證先於終態。memory-socket 測試斷言兩個 frame 都收到，接受任意先後；跨 socket 測試也核對終態 id。保留 reserved terminal lane、expired-frame handling 及原有大型 future 的 boxing。 |
| 文件 | CHANGELOG 項目留在 `[Unreleased]`；英文、繁中、日文 `dashboard-settings.md:76,89,91` 同步新語義，記載 `setsid` 逃離行程群組仍是非沙箱邊界。 |

### 驗證

所有 Cargo 串行，固定 `CARGO_INCREMENTAL=0 CARGO_TARGET_DIR="$PWD/target"`、`-j 4`，每個指定命令只執行一次。日誌在 `round5-logs/`。實際結果如下；未執行審查子代理、部署、發版或原生平台驗收。

修改量（相對本輪開始時乾淨的 HEAD，程式與三語文件共 11 個 tracked 檔案）：新增 187 行、刪除 256 行，淨減 69 行。報告與原始日誌位於既有忽略的 artifacts 目錄，另行保留，不混入程式碼行數。

`cargo check -j 4 -p duduclaw-gateway --tests`：exit 0。

```text
warning: `duduclaw-gateway` (lib test) generated 54 warnings (48 duplicates)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 8m 41s
```

日誌：`round5-logs/check.log`。編譯開始後、gateway crate 檢查前收斂了 reservation／FIFO 的實作，測試也使用最終版本。觀察到 `DuDuClaw-wt-remove-agy` 的 Cargo 同時執行；本 session 的命令保持串行，沒有干預其他 session。

最終驗證狀態：**PARTIAL**。三個指定 Cargo 命令均只執行一次，沒有以排除案例或加大 stack 重跑。

| 命令 | exit | 原始結果行 |
|---|---|---|
| `cargo test -j 4 -p duduclaw-gateway --lib webchat` | 101 | `test result: FAILED. 148 passed; 1 failed; 0 ignored; 0 measured; 10274 filtered out; finished in 6.46s` |
| `cargo test -j 4 -p duduclaw-gateway --lib approval::` | 101 | `test result: FAILED. 182 passed; 1 failed; 0 ignored; 0 measured; 10240 filtered out; finished in 2.75s` |

WebChat 測試執行檔編譯耗時 `15m 47s`，approval 沿用同一份執行檔（`0.63s`）。日誌：`round5-logs/webchat.log`、`round5-logs/approval.log`。

- WebChat 唯一失敗：`webchat::turns::tests::queued_cancel_keeps_fifo_and_never_creates_session`。本輪更新參數時，fixture 在呼叫前就自行 `ApprovalBroker::open`，與原有「未開始的取消不開核准資料庫」斷言衝突。已將 `webchat/turns/tests.rs:312` 改為 `finish_cancel(None, …)`，保留原斷言。這是測試 fixture 修正，產品程式沒有在測試後再變更；**依只跑一次的限制，最後這一行 fixture 修正未重新編譯或執行，不能列為已通過**。
- approval 唯一 failure：`approval::delivery::provider_roundtrip_tests::interleaved_account_thread_transport_scope_never_crosses_and_error_cleans_up`，在 `test_channel_provider.rs:98` 綁定 socket 時得到 `PermissionDenied: Operation not permitted`。情境未執行，列 **NOT RUN（沙箱）**，不是產品斷言失敗，也不是通過。
- `webchat::turns::socket_tests::webchat_real_socket_concurrency_fifo_user_limits_and_foreign_cancel` 的 harness 顯示 ok，但此案例在 PermissionDenied 時提前 return（`socket_tests.rs:54`）；本輪依指定命令未加 `--nocapture`，未取得該 return 分支的輸出，因此**不把它列為 TCP 行為驗收，列 NOT RUN／待沙箱外確認**。
- 真正的記憶體 WebSocket 整合案例已通過，含兩個取消訊框任意順序與終態 request_id。新增／更新的 25 小時清理、長 TTL 不延長 fence、row cap bounded eviction／保留近期 rows、不拒絕新回合、NULL principal、2,000 次 memory cap 洪流零額外 quota read、quota refusal 釋放 slot、FIFO reservation 測試均通過。保留的 terminal lane 與 expired-frame 測試也通過。
- `git diff --check`：exit 0。最後修改量仍為 **11 個 tracked 檔案，+187 / -256，淨減 69 行**。分支保持 `feat/webchat-concurrent-turns-20261030`。

### 尚未完成

1. Claude 在沙箱外重跑最後 fixture 修正後的完整 WebChat／approval 套件，確認上述兩個 socket 情境。
2. 本輪未做額外審查（依交辦不啟動 review subagent）、真供應商 CLI 或原生平台驗收；沒有部署或發版。

## Round 6

依 Claude 的 Round 5 安全審查修正 R6-1～R6-3，保留開始時的 commit 與 Round 5 未提交變更。開始／結束分支為 `feat/webchat-concurrent-turns-20261030`、HEAD `b207cf6f`；未 commit、push、stash、切分支、執行 workspace `cargo fmt` 或啟動任何子代理。本 session 沒有建立 worktree，保留使用者交付的工作樹。Claude 沙箱外 Round 5 gateway **10373/0** 是交辦提供的前輪結果，並非本輪重新驗證結果。

### 逐項變更

下列程式路徑省略 `crates/duduclaw-gateway/src/`。

| 項目 | 實作與證據位置 |
|---|---|
| R6-1 | `webchat/turns.rs:437,449`：先等待 broker 提交 tombstone，再清除同一 session＋server turn 的 pending 交辦；開啟／更新 queue 的任何錯誤都進入 `cancel_failed`。`:465` 的成功終態新增 `delegations_cancelled`，保留 `approved_before_cancel` 與 `request_id`。`message_queue.rs:280` 重用既有 `failed` 終態與 `origin turn cancelled` 原因，不改 schema／狀態 enum；只更新 pending，acked／processing／done 不受取消清理影響。`:290` 原子 claim 只接受仍為 pending 的列，避免讀取快照後取消的列被 ACK 復活。`dispatcher.rs:238,534,1369`：使用 claim CAS，並在所有準備 await 完成、真正 poll runtime 前重查取消 fence；已取消時寫 failed 並 warning，拒絕派送。`approval/turn_origin.rs:30` 使用 blocking pool、唯讀查詢；沒有 approval DB 時不建立，查詢失敗拒絕這次派送，保留既有 stale queue retry。 |
| R6-2 | `server.rs:926,929` 共用既有 hourly cost-telemetry 維護 timer（既有啟動等待 10 分鐘，之後每小時），每次以 `gateway_instance::held` 確認只有持鎖程序清理。`approval/turn_origin.rs:151` 在 blocking pool 執行既有 bounded sweep。`approval/broker.rs:415` 移除 dashboard `expire_stale` 的 tombstone sweep，產品只保留單一排程路徑，不依賴儀表板活動，也不在 admission 清理。未使用 approval DB 的安裝不因此建立資料庫。 |
| R6-3 | `webchat/turns.rs:34,87,146,170`：reservation 初始 `admitted=false`，只有 quota 通過並在同一 registry lock 內加入 FIFO 時才設 true；cancel 僅比對 admitted entries。quota 等待期間以 session／conv、帶 msg_id 或省略 msg_id 取消都找不到目標，沿用 handler `turn_not_found`，也不留下 cancel token。 |
| 文件 | CHANGELOG `[Unreleased]` Fixed／Security；英文、繁中、日文 `dashboard-settings.md:91,93` 記載排隊交辦停止、`delegations_cancelled`、queue 失敗、已交給其他員工的工作繼續執行、FIFO 前不可取消，以及 gateway 每小時清理。 |

### 保留、不實作的決策

依 Claude 決定，維持 100,000 列 cap 與逐出超過一小時最舊紀錄的規則，不改成全域拒絕。R6-1 終止來源行程樹並清除 pending 交辦，dispatcher 再擋競爭入列；因此不為超過一小時的 fence 加入全域 admission 拒絕，以免任何人藉洪流阻止所有使用者送出回合。既有 `setsid`／同 OS 使用者不受限 Bash 的非沙箱界線仍保留，沒有宣稱實作程序隔離。

### 回歸測試範圍

- `message_queue.rs:695`：相同 turn／不同 session、其他 turn、已取走列分離；取消後舊 pending 快照不可復活；failed 列不進入 pending／stale retry。
- `dispatcher_fence_tests.rs:624,655,660`：執行真正 `poll_and_dispatch_sqlite`，只有 runtime 換成既有 dry-run probe。測試取消後清除 pending，以及清除後才競爭入列兩種交錯，皆確認取消回合零模型呼叫、另一回合正常完成。
- `webchat/turns/tests.rs:150,177`：真正取消清理會將 pending 交辦寫成終態，並回報 `delegations_cancelled=1`。
- `webchat/turns/tests.rs:477`：直接保留與 quota await 相同的 reservation 狀態，確定加入 FIFO 前不能取消、通過 quota 加入後可取消；不以 sleep 或 blocking-pool 時序製造測試窗口。
- `webchat/turns/tests.rs:494`：讓 queue DB 路徑無法開啟，確認 fence 已提交仍回 `cancel_failed`。
- `approval/turn_origin.rs:478,496`：舊 fence 清除／近期保留與 row cap 測試改為直接呼叫維護 sweep，不經 dashboard pending／expiry。

### 驗證

所有本 session Cargo 呼叫串行、`-j 4`，設定 `CARGO_INCREMENTAL=0 CARGO_TARGET_DIR="$PWD/target"`。指定 check 與四個 filter 各跑一次。未改動 duduclaw-cli，因此沒有額外跑 CLI check。測試加 `-- --nocapture` 保存 socket 沙箱拒絕證據；完整原始日誌為本目錄 `round6-*.log`，程序 exit／時間為 `round6-test-results.json`。

`cargo check -j 4 -p duduclaw-gateway --tests`：exit 0，`round6-check.log`。

```text
warning: `duduclaw-gateway` (lib test) generated 54 warnings (48 duplicates)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2m 36s
```

四組測試各執行一次，實際結果如下。

共用機器觀察：測試執行檔建置期間，`lsof -a -c cargo -d cwd` 顯示本工作樹及 `DuDuClaw-wt-v178-integ` 各有一個 Cargo。本 session 的命令維持串行，沒有干預其他 session；不宣稱整台機器只有一個 Cargo。


| filter／命令 | exit | 原始結果行 |
|---|---|---|
| `cargo test -j 4 -p duduclaw-gateway --lib webchat -- --nocapture` | 0 | `test result: ok. 153 passed; 0 failed; 0 ignored; 0 measured; 10275 filtered out; finished in 5.71s` |
| `cargo test -j 4 -p duduclaw-gateway --lib approval:: -- --nocapture` | 101 | `test result: FAILED. 182 passed; 1 failed; 0 ignored; 0 measured; 10245 filtered out; finished in 2.85s` |
| `cargo test -j 4 -p duduclaw-gateway --lib message_queue -- --nocapture` | 0 | `test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 10422 filtered out; finished in 0.06s` |
| `cargo test -j 4 -p duduclaw-gateway --lib dispatcher -- --nocapture` | 0 | `test result: ok. 81 passed; 0 failed; 0 ignored; 0 measured; 10347 filtered out; finished in 0.92s` |

第一組測試執行檔建置耗時 `17m 21s`；後三組使用同一份測試執行檔。程式碼在 check 前已收斂，check 與測試之後只補文件與結果紀錄，沒有未經這批驗證的程式修改。`git diff --check` 最後 exit 0；分支與 HEAD 保持不變。

新增的五個測試全部通過：queue 精確取消／快照防復活一個、真正 dispatcher 的兩個交錯、quota reservation 一個、queue failure 一個。原有取消流程測試也通過 `delegations_cancelled=1` 斷言。記憶體 WebSocket 整合測試通過。tombstone sweep／capacity 兩個測試通過，沒有透過 dashboard 觸發清理。

### NOT RUN 與尚未完成的驗證

本輪狀態：**程式修正與指定可執行情境通過；socket 環境驗證 PARTIAL**。沒有把 harness 的提前 return 當 TCP 驗收通過，也沒有把環境拒絕改成產品測試通過。

1. **NOT RUN（沙箱）**：`webchat::turns::socket_tests::webchat_real_socket_concurrency_fifo_user_limits_and_foreign_cancel`。harness 列 ok，但本輪實際輸出：

   ```text
   NOT RUN: WebChat TCP integration: loopback bind denied by sandbox: Operation not permitted (os error 1)
   ```

   此案例的 TCP 行為未執行；日誌 `round6-webchat.log`。
2. **NOT RUN（沙箱）**：`approval::delivery::provider_roundtrip_tests::interleaved_account_thread_transport_scope_never_crosses_and_error_cleans_up`。harness 列 failed，於 `test_channel_provider.rs:98:75` 綁定 loopback 時中止，未進入驗收情境：

   ```text
   called `Result::unwrap()` on an `Err` value: Os { code: 1, kind: PermissionDenied, message: "Operation not permitted" }
   ```

   因此 approval suite exit 101，未宣稱全綠；日誌 `round6-approval.log`。
3. 未啟動真 gateway 等待一小時驗證 resident timer；本輪以編譯、直接 sweep 回歸與 `gateway_instance::held` 呼叫位置確認排程接線。未做真模型／供應商 CLI、原生平台、部署或發版驗收。
4. 依交辦未啟動 review subagent，也沒有額外安全審查。請 Claude 在允許 loopback bind 的環境完成上述兩個 socket 情境；本輪未重跑任何指定 Cargo 命令。

## Round 6 更正（Claude，2026-10-30）

上方「row cap 保留」的理由寫成「fence 超過一小時已沒有遲到來源」，這句不精確。Daybreak Blue 指出：已派送、正在執行的受託員工會繼承原 `session_id`／`turn_id`，取消只清 pending 列，所以它在 fence 到期或被逐出後，仍可能開新核准卡或再交辦。

決定不改程式：這些卡片都是 pending，必須人工決定，不會自動核准；文件已載明已交給其他員工的工作會繼續執行。row cap 保留的理由改為：全域拒絕會讓任何人擋住所有使用者的新回合，代價大於 fence 提早失效，而 fence 失效的最壞結果只是多一張待人工決定的卡。

後續（另案，屬產品決定）：取消是否沿交辦鏈傳遞到已派送的員工。

## Round 7：執行中憑證撤銷與核准來源拒絕

- 每個已開始的認證回合擁有自己的憑證監看 future，每 30 秒強制讀取當下身分；不依賴 socket。撤銷立即取消，連續三次無法讀取也取消，有效結果重設失敗次數。監看與工作在同一個 select 中，沒有額外 spawn；工作完成便丟棄監看，不等待下一個 tick。Widget 不監看，單純斷線不取消。
- 監看只觸發既有 cancel token，沿用 future drop、delivery cleanup、撤回核准／持久化 fence、取消 pending 交辦與 session 記帳。清理先於終態傳送，因此 writer 拒絕撤銷後的所有資料或 socket 消失，仍完成清理；已派送交辦沿用既有界線。
- admission 後、FIFO 恢復與執行前的身分檢查，Unavailable 回傳終態 `error`／`credential_check_unavailable`，提示稍後重試且釋放 ticket；Revoked 保持不傳回合資料。終態經正常 sink，writer 當下仍不可讀時可以扣住輸出。
- `approval/turn_origin.rs::LIVE_ORIGIN_SQL` 共用於一般與 bound decision：只有兩欄皆 NULL 才是 unscoped；scoped 必須兩欄皆非 NULL 且至少一筆未取消 detail。缺 detail、一欄 NULL、全部取消均拒絕。
- 已掃描所有 approvals UPDATE：人工 approved／denied／answered 決策只有 `store.rs` 與 `binding.rs` 兩條 SQL；durable 與 workflow decide 也經這兩處。`turn_origin.rs` 的 system cancellation denial 是撤回路徑，保留原條件；其餘 status UPDATE 是 invalidation。唯一產品 INSERT 在 `store.rs::insert_row`，一般與 if_absent 都在同一交易記錄來源；相同 keyed card 重用時只加未取消來源，合法 unscoped 重用會明確清空兩欄。
- Web 前端 `chat-store.ts` 的 error 分支以 `mentionErrorText(data) ?? data.message` 顯示未知碼，停止 streaming；`mention.ts` 對未知碼回 NULL。本 checkout 無 `crates/duduclaw-tui`，只有 RFC，無法確認 TUI 實作的未知碼行為。
- 測試補上真憑證 MFA 撤銷＋長 future drop＋核准撤回／拒絕決策＋fence（有／無 sink）、三次不可讀／有效重設、正常斷線、監看壽命，以及三個 gate 的拒絕／FIFO 接續。post-admission 案例直接呼叫 reader 共用 gate，不是完整 reader 整合測試；FIFO 與 before-run 案例執行真正 execute。一般／bound 決策各覆蓋六種來源資料狀態。

驗證命令與結果於本輪完成後補記；未啟動審查子代理、提交、推送、stash、切換分支或 workspace fmt。

### 首輪驗證與 fixture 修正

- `CARGO_INCREMENTAL=0 cargo check -j 3 -p duduclaw-gateway --tests` 首輪 exit 0（2m 36s），reader recovery 更新後再跑 exit 0（2m 26s）。原始日誌：`/private/tmp/duduclaw-credential-check.log`、`/private/tmp/duduclaw-credential-check-final.log`。
- 固定 `DUDUCLAW_P1_PILOT_BINARY=/Users/lizhixu/Project/DuDuClaw-wt-merge/target/debug/duduclaw`，執行 `CARGO_INCREMENTAL=0 cargo test -j 3 -p duduclaw-gateway --lib -- --test-threads=4 webchat approval --nocapture`。建置 6m 08s，exit 101：`test result: FAILED. 471 passed; 13 failed; 0 ignored; 0 measured; 10111 filtered out; finished in 14.64s`。日誌：`/private/tmp/duduclaw-credential-tests.log`。
- 其中一項是本輪新增 fixture 錯誤：`credential_revocation_drops_work_and_fences_even_without_socket` 在 `reply_ctx` 掃描前沒有建立 `agents` 目錄，尚未進入憑證測試本體。已在 `credential_fixture` 補 `create_dir_all`；產品程式沒有再修改。修正後重新跑 check 與完整指定測試，結果另記。
- 另外 12 個 harness failure 都在 socket bind 時收到 `PermissionDenied: Operation not permitted`，情境列 NOT RUN。WebChat TCP 案例雖顯示 ok，實際輸出 `NOT RUN: WebChat TCP integration: loopback bind denied by sandbox: Operation not permitted (os error 1)`，同樣列 NOT RUN。首輪其餘六個新增測試通過，沒有將首輪 fixture failure 混成環境問題。

### NOT RUN 清單（socket sandbox）

以下需由 Claude 在沙箱外重跑，不把 harness 提前 return 當作通過：

1. `approval::delivery::provider_roundtrip_tests::interleaved_account_thread_transport_scope_never_crosses_and_error_cleans_up`：`test_channel_provider.rs:98` TCP bind。
2. `codrive::tests_atspi_locate::consequential_locate_step_still_waits_for_approval`：`codrive/tests.rs:87` Unix socket bind。
3. `codrive::tests_mode::plan_approval_approved_lets_the_session_run`：`codrive/tests_mode.rs:71` Unix socket bind。
4. `codrive::tests_mode::plan_approval_denied_aborts_without_ever_connecting`：同上。
5. `codrive::tests_mode::plan_approval_expiry_is_a_denial`：同上。
6. `codrive::tests_mode::plan_approval_off_files_no_card_at_all`：同上。
7. `codrive::tests_registry::consequential_api_action_step_still_waits_for_approval`：`codrive/tests.rs:87` Unix socket bind。
8. `codrive::tests_takeover::explicit_take_over_step_completes_and_skips_approval`：同上。
9. `workflow::f1b_e2e_tests::activation_card_is_admin_only_and_reports_self_approval`：`workflow/pilot_test_factory.rs:59` TCP bind。
10. `workflow::f5a_e2e_tests::a_card_that_lapsed_with_its_run_ends_as_approval_expired`：同上。
11. `workflow::resume_tests::resume_approval_step_then_human_gated_effect_completes_the_same_run`：同上。
12. `workflow::security_race_tests::security_race_request_approval_write_cannot_revive_terminal_projection`：同上。
13. `webchat::turns::socket_tests::webchat_real_socket_concurrency_fifo_user_limits_and_foreign_cancel`：`webchat/turns/socket_tests.rs:54` TCP bind，harness 提前回傳。

另：本 checkout 無 TUI crate，未知 error code 的 TUI 顯示行為未驗證；未做原生平台、真模型、真 provider、部署或發版驗收。依交辦沒有審查子代理。

### 修正後驗證

最後的 fixture 修正後，`CARGO_INCREMENTAL=0 cargo check -j 3 -p duduclaw-gateway --tests` exit 0：

```text
warning: `duduclaw-gateway` (lib test) generated 54 warnings (48 duplicates)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1m 32s
```

日誌：`/private/tmp/duduclaw-credential-check-fixed.log`。指定測試完整重跑的日誌為 `/private/tmp/duduclaw-credential-tests-fixed.log`，命令與 exit／耗時保存在 `/private/tmp/duduclaw-credential-fixed-results.json`。

共用主機上，以 `lsof -a -c cargo -d cwd` 觀察到 `DuDuClaw-wt-v178-integ` 也有 Cargo 在執行。本 session 自己的 Cargo 全部串行、`-j 3`、`CARGO_INCREMENTAL=0`，沒有干預其他工作樹，不宣稱整台主機全程只有一個 Cargo。

最終重跑 exit 101，建置 14m 02s：

```text
test result: FAILED. 472 passed; 12 failed; 0 ignored; 0 measured; 10111 filtered out; finished in 18.54s
```

本輪新增 7 個測試全部通過，包括修正 fixture 後的 `credential_revocation_drops_work_and_fences_even_without_socket`（真 MFA epoch 撤銷、工作 future drop、核准撤回／拒絕決策、fence、有／無 sink）。其餘新增測試涵蓋三次不可讀、Valid 重設、斷線不取消、監看生命週期、三個啟動 gate 的終態與 FIFO，以及一般／bound 的六種來源狀態。

最後 12 個 harness failure 全部是上述清單第 1–12 項的 `PermissionDenied` bind 拒絕，沒有其他 assertion failure。第 13 項仍輸出 NOT RUN 後提前回傳；所以不能把 472 個 harness pass 全部視為情境通過，共有 13 個 socket 情境未執行。記憶體 WebSocket 測試與可執行的既有程序樹取消回歸通過。

最終狀態為 **PARTIAL（程式與可執行測試通過，13 項 socket 行為待 Claude 沙箱外驗收；TUI 原始碼不在 checkout）**。最後測試後只更新本紀錄，沒有再修改程式。分支維持 `feat/webchat-concurrent-turns-20261030`；未提交、推送、stash、切分支、執行 workspace fmt／clippy 或啟動子代理；`git diff --check` exit 0。本 session 沒有建立 worktree，保留交付的工作樹。

- 交付與憑證撤銷取消共用 registry 鎖：取消先取得鎖便拒絕 begin_delivery；交付先取得鎖便停止憑證輪詢並保持監看 future pending，讓 send_terminal 以最新憑證檢查完成拒絕與封存回復；補交付優先、取消優先、競態及真實 execute 入口核准撤回測試；cargo check --tests 通過，指定 webchat 測試回報 167 passed／0 failed（實際通過 166 項，另 1 項 TCP bind 遭沙箱拒絕，列 NOT RUN），新增四項皆通過。
