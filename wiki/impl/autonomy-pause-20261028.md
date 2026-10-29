# Autonomy pause 實作與驗證紀錄

工作分支：`feat/autonomy-pause-20261028`。工作目錄：`/Users/lizhixu/Project/DuDuClaw-wt-autonomy-pause`。未提交、推送、合併或發布。

## 儲存與 gate

選擇 `<home>/autonomy_pause.json`，版本化 JSON 加 `autonomy_pause.initialized` 標記。原因：heartbeat 位於較底層的 agent crate；放在 core 可共用同一實作，避免依賴 gateway 的 SQLite schema 或形成 crate 循環。寫入使用 `duduclaw_core::with_file_lock`，同目錄 create-new 暫存檔、fsync、rename，Unix 再 fsync 目錄，最後重讀實際落地檔。新檔 Unix 權限 0600。全域與個別暫停各自保存 reason（`truncate_chars`，1,000 字元）、who、RFC3339 at。

首次使用沒有檔案視為尚未設定；初始化後檔案遺失、損壞、版本不支援、特殊檔案或讀取錯誤一律拒絕所有自主工作。錯誤依變更記錄一次 ERROR，status 回傳 `state_error`；變更 RPC 拒絕覆蓋損壞狀態。錯誤僅含分類與 digest／行列，不洩漏 JSON 內容。每次 check 重新讀取，無放行快取。

```rust
// crates/duduclaw-core/src/autonomy_gate.rs:249
pub fn check(home: &Path, agent_id: &str, path: AutonomyPath) -> Result<(), PauseInfo>;
// crates/duduclaw-core/src/autonomy_gate.rs:242
pub fn check_global(home: &Path, path: AutonomyPath) -> Result<(), PauseInfo>;
```

core 的檢查同時要求員工原始設定與存在時的 resolved 設定都為 operational；缺失／錯型別／未知 status 拒絕。`default` 別名在 dispatcher、cron、reminder 先解析實際員工。臨時子員工亦檢查母員工鏈，以 org.toml 權威紀錄比對可寫 metadata。

## 直接 gate 呼叫位置

下表是 production source 中直接呼叫共用 gate 的位置；同一檔可能有 admission 與 handoff 兩次以上檢查。測試中的 check 不列入。

| 檔案 | 行號 |
|---|---|
| `crates/duduclaw-agent/src/heartbeat.rs` | 408, 470, 570, 796, 1068, 1293 |
| `crates/duduclaw-gateway/src/assign_schedule/fire.rs` | 165 |
| `crates/duduclaw-gateway/src/autopilot_engine.rs` | 1423, 1428, 1831, 1989, 2106 |
| `crates/duduclaw-gateway/src/cron_scheduler.rs` | 761, 769 |
| `crates/duduclaw-gateway/src/dispatch_engine/review.rs` | 8 |
| `crates/duduclaw-gateway/src/dispatcher.rs` | 219, 262 |
| `crates/duduclaw-gateway/src/ephemeral/dispatch.rs` | 50 |
| `crates/duduclaw-gateway/src/ephemeral/gc.rs` | 216, 348 |
| `crates/duduclaw-gateway/src/goal_loop/enqueue.rs` | 197 |
| `crates/duduclaw-gateway/src/goal_loop/tick.rs` | 256, 446, 920 |
| `crates/duduclaw-gateway/src/gvu/loop_.rs` | 255 |
| `crates/duduclaw-gateway/src/mail_worker.rs` | 455 |
| `crates/duduclaw-gateway/src/night_engine.rs` | 511 |
| `crates/duduclaw-gateway/src/persona_induction.rs` | 823 |
| `crates/duduclaw-gateway/src/reminder_scheduler.rs` | 818 |
| `crates/duduclaw-gateway/src/responsibility/notify.rs` | 286, 333 |
| `crates/duduclaw-gateway/src/responsibility/wake.rs` | 457, 535 |
| `crates/duduclaw-gateway/src/rule_induction.rs` | 577, 717 |
| `crates/duduclaw-gateway/src/self_study.rs` | 290 |
| `crates/duduclaw-gateway/src/skill_synthesis_pipeline/scheduler.rs` | 163 |
| `crates/duduclaw-gateway/src/team_composer.rs` | 1681, 3268, 3326, 3546 |
| `crates/duduclaw-gateway/src/workflow/runner/guard.rs` | 84 |
| `crates/duduclaw-gateway/src/workflow/service.rs` | 41 |

封裝 gate 的 production 呼叫點（別名解析、skip 紀錄、review park、臨時子員工 parent 檢查）：

| 檔案 | 行號 |
|---|---|
| `crates/duduclaw-gateway/src/dispatcher.rs` | 223, 296, 323, 473, 648, 890, 904, 1078, 1505, 1624, 1742, 1743, 1818, 2502 |
| `crates/duduclaw-gateway/src/cron_scheduler.rs` | 168, 527, 606, 787, 853 |
| `crates/duduclaw-gateway/src/reminder_scheduler.rs` | 647, 857, 896 |
| `crates/duduclaw-gateway/src/autopilot_engine.rs` | 1122, 1442, 1631 |
| `crates/duduclaw-gateway/src/dispatch_engine/review.rs` | 26, 42, 631, 793, 804 |
| `crates/duduclaw-gateway/src/ephemeral/dispatch.rs` | 148, 189 |
| `crates/duduclaw-gateway/src/ephemeral/gc.rs` | 384 |

## 各入口行為與額外盤點

| 入口 | 暫停與恢復行為 |
|---|---|
| Goal loop | 新候選不派發；進行中回合完成後 `needs_human/operator_paused`，不在模型尚未返回時提前停駐。解除 gate 後，已 needs_human 的目標仍需人確認。 |
| Dispatcher | claim 前與 runtime handoff 前檢查；保留 pending、不增加 retry。keyset 分頁避免暫停隊首餓死其他員工。涵蓋 SQLite、JSONL、TaskSpec 與已入列的 proactive_notification。late pause 使用專用 deferred 結果，保留原訊息與 retry；durable start 在最終 gate 通過後才記錄。 |
| Cron | due occurrence 記 skipped，不計 run/failure；恢復後下一次排程可執行。含條件腳本入口與 workflow cron。 |
| Heartbeat | status Paused 也排除；涵蓋 cycle、task-board pull、silence breaker、proactive CLI。 |
| Responsibility | due fire 記 dropped/autonomy_paused；恢復後新的 fire 可建立 occurrence。通知評分與推送也檢查，Activity 保留。 |
| Autopilot | rule screen/action、OS-watch goal 旁路、notify／queue handoff 均檢查；無員工身分的系統 action 仍受 global gate。 |
| Assign schedule | 本次 slot 記 skipped/autonomy_paused，前進下一 slot。 |
| Team-as-Agent／review | 成員容量等待、spawn、turn-end，以及 evaluator/judge 邊界檢查。 |
| Workflow | 入列及每步檢查；pause 使用 Deferred，保留同一 run/checkpoint/queue，不計失敗或消耗 retry；恢復後繼續。 |
| Mail／reminder | 收信 auto-trigger 停止；reminder AgentCallback 保留 Pending，恢復後可執行。 |
| Night／self-study／GVU | 在自主工作入口阻擋，不消耗原本的日／冷卻窗。 |
| Skill synthesis／rule/persona induction | 排程入口與 rule 啟用檢查；暫停不消耗排程間隔／日閘。 |
| Ephemeral admission | 暫停 parent 的 ticket 保留且不阻擋其他 parent；dequeue 後暫停會依 reservation 還原原 ID／順序／期限。owner invalidation 同時撤銷 reservation，防止已結束 turn 復活。 |

未加 gate 的項目：真人 WebChat／頻道直接對話（需求指定）；固定文字 Direct reminder、管理者已核准的郵件寄送（既定人類指示）；backup、retention、playbook 過期整理、stagnation 統計、已完成 run 的 prediction metrics、digest（維護或固定統計，不啟動員工 turn）；對話標題／摘要與知識索引（對話維護，不是員工自主派工）。因此暫停不保證所有模型用量歸零。

## RPC 與保護

| 方法 | 授權／可見性 |
|---|---|
| `autonomy.pause` / `autonomy.resume`，agent_id | 即時讀 users.db：Manager 以上且 Operator 以上 binding；Admin 可免 binding。 |
| 同上，all:true | 即時 Admin。agent_id 與 all 必須擇一。 |
| `autonomy.status {}` | 已認證使用者；個別 entry 依即時 Viewer binding 過濾，Admin 看全部；global 與 state_error 可見。 |
| `autonomy.changed` | gateway 程序內 broadcast；push filter 再讀即時身分，個別 scope 限能閱覽該員工者，全域給登入者。 |

pause/resume 先以 strict audit 寫 requested intent，寫入失敗不變更狀態；成功後寫 completion audit，若 completion 失敗仍回傳已套用 scope 與 audit_error，避免重試誤解。mutation 回覆僅含本次 scope，避免其他員工 reason 洩漏。沒有新增 MCP 控制工具；state、marker、lock 加入 agent-file-guard protected surfaces。它不隔離能逃過 guard 的同 OS 使用者／無限制 shell。

`PauseReason::OperatorPaused` 序列化為 `operator_paused`；新舊 reader 的未知字串 fallback 為 Unknown，不會讓目標自動續跑。舊 binary 不實施本次新 gate，依賴暫停時不可降版。前端 token/list 與三語顯示一併補齊。

## 驗證結果

本次 compile-and-fix（2026-10-29）使用指定分支與本 worktree 的 target；沒有設定外部 `CARGO_TARGET_DIR`。Cargo 全程串行，每次 `-j 4`，只指定 core、agent、gateway、cli 四個 package；沒有 commit/push。

| 次序 | 實際指令 | 結果 |
|---|---|---|
| 1 | `cargo check -j 4 -p duduclaw-core -p duduclaw-agent -p duduclaw-gateway -p duduclaw-cli --tests` | FAIL，exit 101；gateway `goal_notify.rs:1047` 多一個右括號，1 個編譯錯誤；測試執行 0。完整 log：`/tmp/autonomy-pause-check-20261029.log`。已移除誤加在 kickoff trajectory 的括號，保留 needs-human trajectory 的 scope。 |
| 2 | `cargo check -j 4 -p duduclaw-core -p duduclaw-agent -p duduclaw-gateway -p duduclaw-cli --tests` | FAIL，exit 101；`cron_routine_tests.rs:148` 呼叫漏傳新增 registry 參數（E0061），1 個編譯錯誤；測試執行 0。log：`/tmp/autonomy-pause-check-20261029-2.log`。已補同 isolated home 的空 registry，保留所有既有斷言；此測試驗證 disabled 先於 service/invoker 拒絕，無需建立員工。 |
| 3 | `cargo check -j 4 -p duduclaw-core -p duduclaw-agent -p duduclaw-gateway -p duduclaw-cli --tests` | PASS，exit 0，2m22s；0 編譯錯誤，測試執行 0。log：`/tmp/autonomy-pause-check-20261029-3.log`。兩項修補皆通過獨立只讀 diff 審查；目前未發現需修改 guard/await 設計的 compiler 問題。 |
| 4 | `cargo test -j 4 -p duduclaw-core autonomy` | PASS，exit 0；13 passed / 0 failed / 0 ignored（另兩個 integration target 各 0 tests）。log：`/tmp/autonomy-pause-test-core-20261029.log`。 |
| 5 | `cargo test -j 4 -p duduclaw-gateway autonomy` | FAIL，exit 101；建置完成 13m28s，開始 48 tests 後 `dispatcher::fence_tests::autonomy_pause_at_sqlite_handoff_defers_without_start_charge` stack overflow、自行 SIGABRT；未輸出任何 passed/failed 計數，不能記作 48 failed。log：`/tmp/autonomy-pause-test-gateway-20261029.log`。沒有外部 kill 證據。 |
| 6 | `cargo test -j 4 -p duduclaw-core autonomy` | PASS，exit 0；14 passed / 0 failed / 0 ignored，含新增 scope future 大小回歸。log：`/tmp/autonomy-pause-test-core-20261029-2.log`。 |
| 7 | `cargo check -j 4 -p duduclaw-core -p duduclaw-agent -p duduclaw-gateway -p duduclaw-cli --tests` | PASS，exit 0，1m22s；boxing 修補後 0 編譯錯誤，測試執行 0。log：`/tmp/autonomy-pause-check-20261029-4.log`。 |
| 8 | `cargo build -j 4 -p duduclaw-cli` | PASS，exit 0，5m34s；補建 workflow `DUDUCLAW_P1_PILOT_BINARY` 前置 fixture，測試執行 0。log：`/tmp/autonomy-pause-pilot-build-20261029.log`。 |
| 9 | `DUDUCLAW_P1_PILOT_BINARY=/Users/lizhixu/Project/DuDuClaw-wt-autonomy-pause/target/debug/duduclaw cargo test -j 4 -p duduclaw-gateway autonomy` | FAIL，exit 101；48 passed / 1 failed / 0 ignored。原 stack overflow 的 SQLite handoff 案例通過。唯一失敗為 workflow prepare/resume fixture 在 `pilot_test_factory.rs:59` bind localhost listener 時遭 sandbox `PermissionDenied/Operation not permitted`，尚未驗證實際 pause/execute；未修改測試或繞過權限限制。log：`/tmp/autonomy-pause-test-gateway-20261029-2.log`。 |

| 10 | `cargo test -j 4 -p duduclaw-gateway --lib autonomous_runtime` | test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 10219 filtered out; finished in 0.07s log：`/tmp/autonomy-path-autonomous_runtime.log`。 |
| 11 | `cargo test -j 4 -p duduclaw-gateway --lib goal_loop` | test result: ok. 258 passed; 0 failed; 0 ignored; 0 measured; 9970 filtered out; finished in 2.51s log：`/tmp/autonomy-path-goal_loop.log`。 |
| 12 | `cargo test -j 4 -p duduclaw-gateway --lib dispatcher` | test result: ok. 90 passed; 0 failed; 0 ignored; 0 measured; 10138 filtered out; finished in 1.49s log：`/tmp/autonomy-path-dispatcher.log`。 |
| 13 | `cargo test -j 4 -p duduclaw-gateway --lib cron_scheduler` | test result: FAILED. 23 passed; 1 failed; 0 ignored; 0 measured; 10204 filtered out; finished in 0.14s log：`/tmp/autonomy-path-cron_scheduler.log`。 |

| 14 | `cargo test -j 4 -p duduclaw-agent heartbeat` | test result: ok. 23 passed; 0 failed; 0 ignored; 0 measured; 329 filtered out; finished in 0.26s log：`/tmp/autonomy-path-heartbeat.log`。 |
| 15 | `cargo test -j 4 -p duduclaw-gateway --lib responsibility` | test result: FAILED. 104 passed; 1 failed; 0 ignored; 0 measured; 10123 filtered out; finished in 6.33s log：`/tmp/autonomy-path-responsibility.log`。 |
| 16 | `cargo test -j 4 -p duduclaw-gateway --lib autopilot_engine` | test result: ok. 87 passed; 0 failed; 0 ignored; 0 measured; 10141 filtered out; finished in 2.02s log：`/tmp/autonomy-path-autopilot_engine.log`。 |
| 17 | `cargo test -j 4 -p duduclaw-gateway --lib assign_schedule` | test result: ok. 39 passed; 0 failed; 0 ignored; 0 measured; 10189 filtered out; finished in 5.11s log：`/tmp/autonomy-path-assign_schedule.log`。 |

| 18 | `cargo test -j 4 -p duduclaw-gateway --lib workflow::runner_tests` | test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 10224 filtered out; finished in 0.61s log：`/tmp/autonomy-path-workflow_runner.log`。 |
| 19 | `cargo test -j 4 -p duduclaw-gateway --lib workflow::autonomy_tests` | test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 10227 filtered out; finished in 0.67s log：`/tmp/autonomy-path-workflow_autonomy.log`。 |
| 20 | `cargo test -j 4 -p duduclaw-gateway --lib dispatch_engine::tests::review` | test result: FAILED. 14 passed; 2 failed; 0 ignored; 0 measured; 10212 filtered out; finished in 0.97s log：`/tmp/autonomy-path-review.log`。 |
| 21 | `cargo test -j 4 -p duduclaw-gateway --lib paused_team_verifier` | test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 10227 filtered out; finished in 0.03s log：`/tmp/autonomy-path-team.log`。 |
| 22 | `cargo test -j 4 -p duduclaw-gateway --lib gvu::aee::tests` | test result: ok. 29 passed; 0 failed; 0 ignored; 0 measured; 10199 filtered out; finished in 0.86s log：`/tmp/autonomy-path-gvu.log`。 |
| 23 | `cargo test -j 4 -p duduclaw-gateway --lib night_engine` | test result: ok. 33 passed; 0 failed; 0 ignored; 0 measured; 10195 filtered out; finished in 0.30s log：`/tmp/autonomy-path-night.log`。 |

| 24 | `cargo test -j 4 -p duduclaw-gateway --lib ephemeral` | test result: ok. 63 passed; 0 failed; 0 ignored; 0 measured; 10165 filtered out; finished in 0.70s log：`/tmp/autonomy-path-ephemeral.log`。 |
| 25 | `cargo test -j 4 -p duduclaw-gateway --lib skill_lifecycle::synthesis_runner` | test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 10221 filtered out; finished in 0.06s log：`/tmp/autonomy-path-synthesis_runner.log`。 |
| 26 | `cargo test -j 4 -p duduclaw-gateway --lib skill_synthesis_pipeline` | test result: ok. 47 passed; 0 failed; 0 ignored; 0 measured; 10181 filtered out; finished in 0.07s log：`/tmp/autonomy-path-synthesis_pipeline.log`。 |
| 27 | `cargo test -j 4 -p duduclaw-gateway --lib push_filter` | test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 10228 filtered out; finished in 0.00s 0 tests 不算 coverage；實際 push filter 由 autonomy_rpc_push_is_scoped_and_success_is_audited 覆蓋。 log：`/tmp/autonomy-path-push_filter.log`。 |
| 28 | `cargo test -j 4 -p duduclaw-gateway --lib autonomy_rpc` | test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 10218 filtered out; finished in 1.23s log：`/tmp/autonomy-path-rpc.log`。 |
| 29 | `cargo test -j 4 -p duduclaw-gateway --lib moa_blocking_worker_preserves_caller_pause_between_members` | test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 10227 filtered out; finished in 0.06s log：`/tmp/autonomy-path-moa.log`。 |
| 30 | `cargo test -j 4 -p duduclaw-gateway --lib paused_trajectory` | test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 10227 filtered out; finished in 0.06s log：`/tmp/autonomy-path-trajectory.log`。 |

測試 fixture 修補（四項重測皆已通過，獨立只讀 diff 審查通過）：cron 的 request-id 案例建立 active `worker`；responsibility explore-lane 案例覆寫 `agent.toml` 時保留 active status；review 兩個 grounding 案例使用既有 `ensure_autonomy_agent`。四個失敗皆先被新 fail-closed gate 攔住，尚未到原測試要驗證的階段；未改斷言、未放寬 production gate。

| 31 | `cargo test -j 4 -p duduclaw-cli paused_` | test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 2276 filtered out; finished in 0.17s log：`/tmp/autonomy-path-mcp_paused.log`。 |
| 32 | `cargo test -j 4 -p duduclaw-cli --lib synthesis_authorizes_subject_and_checks_caller_and_target_pause` | test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 2277 filtered out; finished in 0.12s log：`/tmp/autonomy-path-mcp_synthesis.log`。 |

| 33 | `cargo test -j 4 -p duduclaw-gateway --lib cron_scheduler` | test result: ok. 24 passed; 0 failed; 0 ignored; 0 measured; 10204 filtered out; finished in 0.15s log：`/tmp/autonomy-path-cron_scheduler-2.log`。 |
| 34 | `cargo test -j 4 -p duduclaw-gateway --lib responsibility` | test result: ok. 105 passed; 0 failed; 0 ignored; 0 measured; 10123 filtered out; finished in 5.85s log：`/tmp/autonomy-path-responsibility-2.log`。 |
| 35 | `cargo test -j 4 -p duduclaw-gateway --lib dispatch_engine::tests::review` | test result: ok. 16 passed; 0 failed; 0 ignored; 0 measured; 10212 filtered out; finished in 0.89s log：`/tmp/autonomy-path-review-2.log`。 |
| 36 | `cargo test -j 4 -p duduclaw-gateway --lib push_events_follow_the_same_gate_per_connection` | test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 10227 filtered out; finished in 0.52s log：`/tmp/autonomy-path-push_events.log`。 |

| 37 | `cargo clippy -j 4 -p duduclaw-core -p duduclaw-agent -p duduclaw-gateway -p duduclaw-cli --tests -- -D warnings` | FAIL，exit 101；未改動的 license/identity 各 5 個既有警告升為 error，未到達本次主要程式碼。log：`/tmp/autonomy-pause-clippy-20261029.log`。 |

本輪設計修補：generic async scope 逐層內嵌大型模型 future，造成 dispatcher 測試堆疊溢位。core `autonomy_handoff` 與 gateway `autonomous_runtime` 改在建構 async frame **之前** boxing 子 future，父身分／capture 仍在 poll 時讀取，不改 admission guard 壽命；新增 64 KiB future 的 wrapper 大小界線與 poll-time 身分繼承回歸。原 dispatcher 崩潰案例與 core/gateway 大小回歸已通過；實際 diff 獨立只讀審查通過。只對這兩個實際修改檔案執行 `rustfmt --edition 2024`，未做 workspace fmt。

本次 E0275 修補驗證（2026-10-29）：僅使用本 worktree 的 `target/`（每次明確設定 `CARGO_TARGET_DIR="$PWD/target"`），每次 `-j 4`，串行執行；不跑測試套件。

| 次序 | 實際指令 | 結果 |
|---|---|---|
| E0275-1 | `cargo check -j 4 -p duduclaw-cli --tests` | FAIL，exit 101；CLI lib 與 lib test 在 `acp/client_protocol.rs:479` 重現 E0275 `RuntimeRegistry` Send/Sync 推導溢位。log：`/private/tmp/autonomy-pause-cli-check-before.log`。測試執行 0。 |
| E0275-2 | `cargo check -j 4 -p duduclaw-core -p duduclaw-agent -p duduclaw-gateway -p duduclaw-cli -p duduclaw-inference --tests` | FAIL，exit 101；`request_scope` 型別抹除後，Claude `invoke_recorded<F>` 漏傳 `Send` bound，gateway lib 回報 13 個 E0277 診斷；已補 bound。log：`/private/tmp/autonomy-pause-check-erased.log`。測試執行 0。 |
| E0275-3 | `cargo check -j 4 -p duduclaw-core -p duduclaw-agent -p duduclaw-gateway -p duduclaw-cli -p duduclaw-inference --tests` | PASS，exit 0；指定五個套件及測試程式碼全部檢查通過，0 編譯錯誤；仍有 warnings。log：`/private/tmp/autonomy-pause-check-erased-final.log`。測試執行 0。 |

### Lib test failures 修補驗證（2026-10-29）

本輪沿用指定 worktree／分支，沒有 commit、push 或建立／移除 worktree。每條 Cargo 指令均加 `CARGO_TARGET_DIR=/Users/lizhixu/Project/DuDuClaw-wt-autonomy-pause/target`，一次只執行一條、`-j 4`、限定套件；沒有 workspace fmt。只對實際修改的 Rust 檔執行 `rustfmt --edition 2024 --config skip_children=true`。

| 次序 | 實際指令（上列 target 環境變數省略） | 結果 |
|---|---|---|
| TESTFIX-1 | `cargo test -j 4 -p duduclaw-cli --lib task_board_tests` | PASS，exit 0；36 passed / 0 failed / 0 ignored。log：`/tmp/autonomy-fix-cli-task-board.log`。 |
| TESTFIX-2 | `cargo test -j 4 -p duduclaw-cli --lib` | FAIL，exit 101；2260 passed / 18 failed / 4 ignored。其中 17 件為 sandbox 限制（16 socket bind、1 uv cache），另 1 件 responsibility approval fixture 的 internal key 沒有 employee env，已補隔離子程序，後續重測另列。log：`/tmp/autonomy-fix-cli-full.log`。 |
| TESTFIX-3 | `cargo test -j 4 -p duduclaw-gateway --lib dispatch_engine` | PASS，exit 0；161 passed / 0 failed / 0 ignored；原 engine 與 grounding 兩件失敗皆通過。log：`/tmp/autonomy-fix-gateway-dispatch-engine.log`。 |
| TESTFIX-4 | `cargo test -j 4 -p duduclaw-gateway --lib runtime_dispatch` | PASS，exit 0；12 passed / 0 failed / 0 ignored，含 isolated CLI measured-usage regression。log：`/tmp/autonomy-fix-gateway-runtime-dispatch.log`。 |
| TESTFIX-5 | `DUDUCLAW_P1_PILOT_BINARY=/Users/lizhixu/Project/DuDuClaw-wt-autonomy-pause/target/debug/duduclaw cargo test -j 4 -p duduclaw-gateway --lib autonomy` | FAIL，exit 101；51 passed / 1 failed / 0 ignored。唯一失敗 `workflow::f1b_e2e_tests::autonomy_pause_after_prepare_preserves_effect_for_resume` 在 `pilot_test_factory.rs:59` localhost bind 遭 sandbox PermissionDenied；未繞過。log：`/tmp/autonomy-fix-gateway-autonomy.log`。 |
| TESTFIX-6 | `cargo test -j 4 -p duduclaw-core --lib autonomy_gate` | PASS，exit 0；14 passed / 0 failed / 0 ignored；含 first-use、corrupt/missing state、bounded lock、handoff lease、權限保護。log：`/tmp/autonomy-fix-core-gate.log`。 |
| TESTFIX-7 | `cargo test -j 4 -p duduclaw-cli --lib` | FAIL（環境限制），exit 101；2261 passed / 17 failed / 4 ignored。原始 28 項失敗全部通過，新增兩個 regression 也通過；17 件剩餘失敗均為 sandbox 拒絕（14 TCP bind、2 Unix socket bind、1 uv cache 存取）。log：`/tmp/autonomy-fix-cli-full-final.log`。 |

最終驗證：原始 CLI 28 + Gateway 3 項全部在本輪對應套件／filter 內通過；完整 CLI 與 gateway autonomy 因上述環境限制未全綠，不宣稱完整套件／端到端驗收通過。沒有修改測試以跳過環境拒絕，沒有移動 uv cache 或繞過 bind 限制。`git diff --check` 通過。workflow fixture 使用既有指定 binary，本輪未另外 rebuild CLI binary，也未完成被 bind 擋住的實際 execute/resume。

## 限制

這是 admission control，不是取消／殺死已執行程序。真人對話仍可用，員工原有 lifecycle Paused 的路由效果不變。未做真實 LLM、Windows/Linux 原生、真實 dashboard 瀏覽器或外部頻道驗收。指定的 Daybreak Blue 安全審查工具在本 session 不可用；已做獨立 code-reviewer 實際 diff 審查，但不冒稱指定模型審查完成。

## 續作進度（2026-10-29）

- 接手確認原分支與 68 個已修改檔案，保留原實作；`git diff --check` 通過。
- 已啟動 `cargo check --workspace --tests`；先遇 target lock。確認前次 gateway 測試編譯仍在執行，另有前次 core/clippy/check 等待者；嘗試終止本 worktree 的舊等待者遭 sandbox 拒絕（Operation not permitted），未終止任何程序，等待既有編譯釋鎖。
- 優先序維持 workspace check → autonomy 核心/RPC → 每路徑回歸 → clippy → 文件與呼叫點更新。所有新結果會另列，舊暫存 log 不當作本次通過證據。

- 續作審查發現並修補：Mail callback 恢復測試改為實際 invoker 0→1 與 triggered readback；RPC audit 改 strict intent 並測不可寫時 state 不變。Dispatcher late-pause 被錯算 failure/retry 的競態已補修，新增 SQLite／JSONL／TaskSpec handoff race 測試與 proactive notification gate。三語指南與 OpenAPI 已同步 audit_error、mail 不自動補觸發的界線。

- 獨立 code-reviewer 複審上述修補與 reminder invoker 測試後未留程式碼阻擋項目；仍待實際 cargo 結果。Reminder 以未知測試 channel 驗證到達送出邊界並確定性拒絕，沒有宣稱真實訊息送達。
- 既有邊界：JSONL bus 整體 read→rewrite→rename 未在整個交易持 sidecar lock（非本次新增）；本次只確保新增 append 遵守 with_file_lock。TaskSpec 真正 step failure 後 replanner 再遇 pause 的既有終止語義未擴充；模型仍受 gate 阻擋。

## 安全修補輪（2026-10-29，static-only）

使用者指定沿用本 worktree／分支與先前未提交內容。機器記憶體不足且另一個 session 持有 build slot；本輪**沒有執行 cargo、rustc、clippy、任何 build/test、cargo fmt 或其他 formatter，也沒有 commit/push**。先前段落的 cargo 紀錄屬於前輪，不是本輪結果。只有讀取、靜態修改與 `git diff --check`。以下「done」只代表程式與指定測試已寫，全部尚未編譯／執行，不能當成 ship approval。

| Finding | 本輪狀態 | 變更與未完成項目 |
|---|---|---|
| H1 | done（static） | MCP 外層 dispatch 在 approval／workflow 副作用前、內層 handler dispatch 在 verified RecordActor 解析後 gate caller；task handler 另有防禦。涵蓋 send/spawn/task/goal/assign/cron/reminder/責任/team/plan 及重新啟動 heartbeat 的設定。錯誤為繁中，指向 dashboard／`duduclaw tui`。SQLite/JSONL/TaskSpec 檢查 sender/origin；runtime scope 於 shared lease 內重查來源。Goal／heartbeat task 另從 host-stamped created_by/created_via 取得 employee source，goal tick、team member 也保留來源；新 MCP employee task 一律標記 created_via=agent。legacy 沒有 typed creator 標記時，只能辨識目前存在、仍有 pause entry 或 ephemeral 的 employee；無法重建已刪除且沒有來源紀錄的舊建立者。 |
| H2 | done（static） | synthesis runner entry、每個 model、publication；pipeline Direct Anthropic API／utility、write/graduation；channel sandbox graduation 均 gate。MCP handler 使用 RecordActor + 現有 org subject 授權，檢查 caller/target，保留 caller admission 至 pipeline 完成，防跨員工中途暫停漏接。長 pipeline 可能使 pause timeout，見 H3。 |
| H3 | **partial** | core 新增 async admit/admit_global、fd-owned shared lease、30 秒 bounded shared/exclusive acquisition，RPC mutation 在 spawn_blocking。CLI spawn（Claude 三處、Codex、Grok、Antigravity、generic CLI、heartbeat）於 spawn 後釋鎖；API plain/stream send、RecordingProvider、UsageTap、local inference、PTY、ephemeral、sandbox 加 admission。dispatcher default 只解析一次；來源與 target 在 final scope 內重查。durable started_at 搬至最後 admission 通過後，HANDED_OFF 區分首個 handoff 前拒絕與後續 API round 拒絕，後者不當成完全未啟動的 queue message 直接 defer。**未完成：provider trait、local tool-loop、PTY、sandbox、workflow execute 與合成／review 等高層 callback 部分仍持鎖到整個 future 返回，未全數做到 send/spawn 後立即 drop。** 因而 pause 可能在 30 秒後未套用。仍需拆出可靠 handoff 通知並做真正 race／跨程序驗收；未宣稱完整 H3 關閉。 |
| H4 | done（static） | effect 在 approval 與 lease renewal 後、execute 前 admit；拒絕回傳 WORKFLOW_AUTONOMY_PAUSED，保留 Prepared operation/checkpoint，沿既有 Deferred 路徑，不計 effect failure／queue retry。guard 保守持到 execute 返回（H3 partial）。 |
| M5 | done（static） | review evaluator/external/MAV、team verifier、trajectory、GVU generator/scorer/outer judge、Night N1/N2 各自取得 admission。operator_paused trajectory 直接 None；目前 agent paused 也不呼叫模型。GVU scope 包覆整輪，每次 inner generation 再 gate。模型 callback 的釋鎖粒度限制同 H3。 |
| M6 | done（static） | scaffold capacity lock 內檢查 parent chain；ticket enqueue_checked 在 queue lock 內執行 gate，包含 team role enqueue；paused parent 不建立 scaffold/ticket。 |
| H7/M7 | done（static） | MutationOutcome: NotApplied / Applied / AppliedButDurabilityUnknown；rename 為 commit point。兩種 Applied 都 completion audit + push + applied:true；durability unknown 帶固定 warning，不能宣稱未套用。同步 fixture API 保留相容性；production RPC 使用明確 outcome。 |
| L8 | done（static） | Observer autoclose 改已知原因 allowlist；Unknown、未識別新 token、OperatorPaused 都維持 needs_human。 |
| L9 | done（static） | event 僅 agent_id/all/paused/at，不帶 reason/who/audit_error；push 仍可能使用 2 秒身分快取，敏感欄位必須從 fresh-binding autonomy.status 取得。搜尋 web/src 沒有 autonomy.changed reason consumer，沒有新增 UI 修改。 |
| L10 | done（要求的 partial-hardening 範圍，static） | mutation off reactor + bounded admission lock；requested audit 明確 agent_id 與 all:boolean；新 lock file 加 file guard。三語說明 same-UID shell 可刪／改 state/marker/lock，真正 containment 需不授予 Bash／sandbox。 |
| Docs | done（static） | CHANGELOG Unreleased Security、三語 continuous-responsibilities、OpenAPI 更新 caller 禁止代派工、workflow deferred、等待／timeout／durability warning／minimal event 與 same-user 限制；公開文件明列 H3 尚未全數 handoff 即釋鎖。 |

### 本輪檔案範圍（不把前輪所有 dirty files 冒算成本輪產出）

- H1/M6：`crates/duduclaw-cli/src/mcp_dispatch.rs`、`mcp/{dispatch,record_authz,record_authz_tests,tasks,spawn}.rs`；core `spawn_admission.rs`；gateway `dispatcher.rs`、`dispatcher_fence_tests.rs`、`ephemeral/{mod,scaffold,role_member,dispatch}.rs`、`ephemeral/tests/role_member_cases.rs`、`goal_loop/tick.rs`、`team_composer.rs`。
- H2：CLI `mcp/skills_lifecycle.rs`、`mcp/tests/part3.rs`；gateway `skill_lifecycle/synthesis_runner.rs`、`skill_synthesis_pipeline/pipeline.rs`、`channel_reply/inner.rs`。
- H3/H7/L9/L10：core `autonomy_gate.rs`、`autonomy_gate/tests.rs`、`org_field_guard/mod.rs`；gateway 新增 `autonomous_runtime.rs`、`lib.rs`、`runtime_dispatch.rs`、`claude_runner.rs`、`direct_api.rs`、`pty_runtime.rs`、`runtime/{codex,grok,antigravity,generic_cli,openai_compat}.rs`、`channel_reply/{cli_spawn,cli_env}.rs`、`handlers/autonomy_rpc.rs`、`handlers/autonomy_rpc/tests.rs`；agent `heartbeat.rs`。
- H4/M5/L8：gateway `workflow/runner/effect.rs`、`workflow/f1b_e2e_tests.rs`、`dispatch_engine/review.rs`、`dispatch_engine/tests/review.rs`、`goal_notify.rs`、`gvu/loop_.rs`、`gvu/aee/{inner_loop,run,tests}.rs`、`night_engine.rs`、`goal_loop/{state,escalate}.rs`、`goal_loop/state/tests.rs`。
- 文件：`CHANGELOG.md`、`docs/api/openapi.yaml`、`docs/guides/{,zh-TW/,ja-JP/}continuous-responsibilities.md`、本筆記。

### 新增測試（全部未執行）

- H1：paused caller 對另一員工呼叫九類真實 MCP handler，不建立 DB/queue/scaffold/ticket；resume task/send/spawn；operator/direct-chat 保留；paused queue sender 保留 Pending；`paused_task_creator_cannot_hide_behind_goal_loop_sender`；`default_alias_cannot_retarget_after_admission_identity_is_selected`。
- H2：`paused_synthesis_does_not_call_model_or_publish`（個別/global）、`lifecycle_pause_after_generation_prevents_publication`、`production_entry_rejects_pause_before_registry_lookup`、`paused_direct_api_synthesis_does_not_write_or_graduate`、`synthesis_authorizes_subject_and_checks_caller_and_target_pause`。
- H3/H7/L9/L10：shared guard 阻擋 pause、shared/exclusive timeout、pause 成功後拒絕 admit、rename 後 fsync/readback failure 仍 Applied、RPC durability warning/audit/push、event exact keys、audit employee id=all 與 global 不混淆；`trusted_caller_is_rechecked_at_handoff_and_chat_is_unaffected`、`later_api_refusal_remembers_the_first_handoff`。
- H4：`autonomy_pause_after_prepare_preserves_effect_for_resume`，沿真實 runner fixture 的 prepare/execute barrier；需要現有 `DUDUCLAW_P1_PILOT_BINARY` 測試 fixture，尚未執行。
- M5：`paused_review_never_starts_evaluator_or_mav`、`pause_after_evaluator_prevents_the_next_judge_stage`、`paused_team_verifier_refuses_before_runtime_call`、`paused_trajectory_is_absent_without_starting_a_runtime`、`autonomy_pause_prevents_second_generator_round`、`pause_after_n1_prevents_n2_model_call`。
- M6：paused/global parent 不寫 scaffold/ticket、existing child 檢查整條 parent chain。
- L8：`observer_autoclose_is_an_allowlist`。
- callback 內的 pause-after-first-model 測試以 lifecycle status 改為 paused 模擬 gate 變更，避免在持 shared guard 的 callback 內同步申請 exclusive lock；真正 pause writer 競態由 core admission tests 單獨覆蓋，不能把兩者混稱同一種驗證。

### 靜態審查與後續驗證

獨立 code-reviewer 找到並回報：OpenAI UsageTap 漏 gate、Claude utility spawn/local inference/sandbox 漏 gate、task creator 來源遺失、重複 drop(admission) 的 move 錯誤、多輪 API 的未啟動／已啟動分類。已依回報補上程式；reviewer 結論仍為 do-not-ship，後續整合修改尚未再次完整獨立審查。`gpt-daybreak-blue-latest` 在本 session 可用模型中不存在，未冒稱已完成指定安全模型審查。

目前沒有已知尚未修掉的確定型別／簽章錯誤；但沒有 compiler 證據。尤其新增的 task-local generic scope、RuntimeDispatchFailure／LlmError 轉接、team scope 的借用、workflow fixture method/types、跨平台 fs2 error mapping 都只以現有定義靜態核對。下一輪必須先編譯與執行上述測試，再重新審查所有 runtime handoff（含 failover、tool-loop 第二輪、停在 admission 的 pause timeout）；H3 不應在這些工作前標 done。

## H3 handoff 續修（2026-10-29，static-only，取代上輪 partial 的程式狀態）

本輪沿用 `feat/autonomy-pause-20261028` 與既有未提交 worktree；沒有另開／移除 worktree。**沒有執行 cargo、rustc、clippy、任何 build/test、cargo fmt 或 formatter，沒有 commit/push**。機器記憶體不足、其他 session 持有 build slot 的限制全程保留。

**狀態：指定 H3 範圍已完成靜態實作與測試撰寫，驗證仍待辦，不能作為發版放行。** 舊段落記載的是前輪實作：整個 API/PTY/callback future 持鎖已移除；本輪使用者允許的 workflow 短 timeout 例外如下。

### 本輪逐點變更與釋鎖位置

行號為本輪收尾時 source；同列多個行號表示 complete/stream 或多個 callback。

| 檔案與行號 | 最終行為 |
|---|---|
| `crates/duduclaw-core/src/autonomy_handoff.rs:47` | 新增每次 transport admission；`handed_off` 消耗 guard，HTTP 失敗／取消 conservatively 記已嘗試，釋鎖不等 body。 |
| `crates/duduclaw-core/src/autonomy_gate.rs:327` | `admission_wait_timeout` 加本程序 holder 數量與 AutonomyPath 分類；其他程序標 unknown。shared/exclusive 仍各有 30 秒取得期限、拒絕即不啟動。 |
| `crates/duduclaw-gateway/src/autonomous_runtime.rs:20` | scope 合併 caller；owned transport hooks 每次重查 caller/target；跨 task/thread capture/restore，handoff/denied 使用共享原子旗標。 |
| `crates/duduclaw-llm/src/providers/anthropic.rs:495` | shared send 在 response head 後 drop；complete/stream 的 body 均不持鎖。 |
| `crates/duduclaw-llm/src/providers/openai.rs:555, 588` | complete/stream 各在 response head 後 drop（本檔兩處）。 |
| `crates/duduclaw-llm/src/providers/gemini.rs:531, 563` | complete/stream 各在 response head 後 drop（本檔兩處）。 |
| `crates/duduclaw-llm/src/providers/openai_compat.rs:743` | shared send 在 response head 後 drop，涵蓋 local inference 與工具迴圈每輪。 |
| `crates/duduclaw-gateway/src/claude_runner.rs:1629` | RecordingProvider complete/stream 移除外層整次持鎖，底層 provider 接管。 |
| `crates/duduclaw-gateway/src/claude_runner.rs:2924` | local inference 移除整個 tool-loop/router 持鎖，保留 preflight，底層每次 send admission。 |
| `crates/duduclaw-gateway/src/runtime/openai_compat.rs:698` | UsageTap complete/stream 移除整次 guard，保留計費。 |
| `crates/duduclaw-gateway/src/direct_api.rs:733` | MoA blocking worker capture/restore 完整身份；proposer/aggregator 由 provider 每次 admission。 |
| `crates/duduclaw-inference/src/engine.rs:405` | UCCI shadow detached spawn 保留 owned hooks，背景第二次模型也重新 admission。 |
| `crates/duduclaw-cli-runtime/src/oneshot.rs:140` | PTY 前 admit，交給 native spawn callback 消耗 guard。 |
| `crates/duduclaw-cli-runtime/src/pty.rs:189` | native `spawn_command` 成功立即 callback/drop，尚未 drain/output wait。 |
| `crates/duduclaw-gateway/src/pty_runtime.rs:121` | 刪除包住整個 PTY invocation 的 guard，使用底層 oneshot admission。 |
| `crates/duduclaw-gateway/src/runtime/grok.rs:884` | PTY 分支移除外層整個 invocation guard，交由底層 oneshot；普通 Command spawn 原本立即 drop 保留。 |
| `crates/duduclaw-container/src/docker.rs:381` | Docker start_container 成功回覆即 drop，不等容器工作完成。 |
| `crates/duduclaw-container/src/wsl2.rs:133` | WSL Docker start client process spawn 成功即 drop，不等 stdout。 |
| `crates/duduclaw-gateway/src/discovery/process.rs:125` | 新增 run_admitted；Command::spawn 成功即 drop。probe/cleanup 不取得自主 lease。 |
| `crates/duduclaw-gateway/src/task_sandbox/container.rs:759` | task sandbox Docker start --attach 使用 admitted process transport。 |
| `crates/duduclaw-gateway/src/dispatcher.rs:796` | 移除 sandbox 整段持鎖；handoff 前 denied → AutonomyPaused，已 handoff 後不冒稱完全未啟動。 |
| `crates/duduclaw-gateway/src/dispatch_engine/review.rs:28` | evaluator/external/MAV 外層僅 preflight；每個 task 保留 caller scope，真正 transport 每次 admission/drop。 |
| `crates/duduclaw-gateway/src/judge_mode.rs:1112` | external judge Command::spawn 成功即 drop。 |
| `crates/duduclaw-gateway/src/goal_notify.rs:612` | trajectory 改 caller/target scope，移除 callback 整段 guard。 |
| `crates/duduclaw-gateway/src/team_composer.rs:1680` | team entry 無持鎖 preflight；verifier 另裝 owner/caller scope，子模型每次 transport admission。 |
| `crates/duduclaw-gateway/src/team_composer.rs:4570` | verifier 不在整個 utility call 持鎖。 |
| `crates/duduclaw-gateway/src/gvu/aee/inner_loop.rs:280, 600` | generator 與 scorer 移除 callback guard（本檔兩處）；每次生成重新 preflight/transport admission。 |
| `crates/duduclaw-gateway/src/gvu/aee/run.rs:885` | outer judge 移除長 guard，沿原 scope 到 transport。 |
| `crates/duduclaw-gateway/src/night_engine.rs:644, 685` | N1/N2 各自 scope，移除等模型返回的 guard（本檔兩處）。 |
| `crates/duduclaw-gateway/src/skill_lifecycle/synthesis_runner.rs:115` | 生成 callback 改 scope；publication 本機寫入後明確 drop，再等 accumulator。 |
| `crates/duduclaw-gateway/src/skill_synthesis_pipeline/pipeline.rs:191` | pipeline 傳入 verified caller，每次模型/寫入重查；publication 完成本機 graduation 後 drop，再通知 dashboard。 |
| `crates/duduclaw-cli/src/mcp/skills_lifecycle.rs:353` | 刪除整條 pipeline caller guard，改 caller-aware entry。 |
| `crates/duduclaw-gateway/src/workflow/runner/effect.rs:215` | 唯一明列的不可分離 execute：持鎖 timeout=min(原 timeout,10秒)，小於 pause 30秒；失敗保留 outcome_unconfirmed 語義。 |

附帶 wiring：`crates/duduclaw-core/src/lib.rs` 匯出 `autonomy_handoff`；`crates/duduclaw-cli-runtime/Cargo.toml` 與 `Cargo.lock` 只加入既有 workspace core 依賴，沒有版本升級。CHANGELOG 與三語 continuous-responsibilities 指南同步了新的 handoff/timeout 行為與未驗證界線。

原本已在 spawn/send 後立即 drop 的位置維持：gateway direct_api 普通 HTTP、runtime/openai_compat 普通 HTTP/SSE、Claude 三個 Command spawn、Codex、Grok 普通 Command、Antigravity、generic CLI、agent heartbeat。此次重新盤點未把這些舊修改冒算成新的釋鎖修補。

### 不可分離與精確界線

- **Workflow `session.execute()`**：API 結合 MCP write、等待回覆、ledger receipt inspection，本輪未改其協定。持鎖區間以 `min(step_timeout, 10 s)` 包住 execute，pause exclusive 取得期限仍 30 s。timeout 保留 `workflow_effect_timeout_outcome_unconfirmed`，清掉 session；不宣稱遠端未執行，不當成普通 pause defer。尚未實跑 timeout／ledger recovery。
- **HTTP**：使用使用者指定的 response-head 邊界。`.send().await` 收到 headers 即釋鎖；error body、正常 body 與 SSE 消費不持鎖。若非串流服務端等生成完成才送 headers，或 headers 卡住，仍可能等到 30 秒 admission timeout；沒有承諾所有長模型呼叫都不會使 pause 逾時。HTTP send（含 Docker start request）失敗／取消無法證明未送達，保守記 attempt，避免下個拒絕被當成首輪完全沒啟動。
- **Sandbox**：Bollard start 以 daemon start 成功回覆為界；task sandbox／WSL CLI 以 Docker start client 的子程序 spawn 成功為界，後者不是 daemon 已確認容器啟動的證據。子程序在 handoff 後遵循既有當前回合完成語義。
- **本機 publication**：短 guard 仍保護實際檔案／sandbox/graduation 寫入，沒有跨模型呼叫；dashboard delivery 與後續 accumulator 等待在 guard 外。這不是取消已執行 turn。
- **診斷**：`local_holders`／`local_paths` 僅是本程序、同 home 的近似即時數量與 AutonomyPath 分類，不含員工 id/原因；外程序明列 `other_process_holders=unknown`。不將本程序 0 當成全系統 0。
- **Scope**：本輪處理自主呼叫已安裝的 host scope，沒有把真人對話或 standalone CLI 的所有容器使用變成自主工作。PTC script sandbox 的 CLI 本來未安裝此 scope，未宣稱新增全面 admission coverage；將來若接入，須避免把 start admission refusal 當 unavailable 而降級到 host fallback。

### 新增／改寫測試（全數只寫，未執行）

- `autonomous_runtime.rs::long_fake_model_does_not_block_pause_after_handoff`：fake 模型 handoff 後在 60 秒 sleep／completion barrier 等待，實際 pause 必須 2 秒內完成且模型尚未完成。
- `autonomous_runtime.rs::pause_waits_only_for_the_check_to_spawn_window`：pause 在 final check 與 fake spawn 間等待；handoff 後完成，下一次 admission 拒絕。
- `providers/openai_compat.rs::response_headers_release_admission_before_slow_body_and_tool_loop_next_call_refuses`：真實 localhost TCP、production provider、實際 run_tool_loop；headers 後鎖住 body，pause 先落地，放 body 後下一輪不再建立 HTTP connection。
- `autonomous_runtime.rs::nested_model_scope_retains_caller_and_handoff_flags`、`detached_core_transport_retains_caller_and_shared_flags`：巢狀／跨 task caller 與共享 flags 不遺失。
- `autonomous_runtime.rs::cancelled_send_cannot_be_replayed_as_an_unstarted_turn`：已嘗試 send 但未取得 headers 即取消，後續 pause 不可當成完全未啟動。
- `direct_api.rs::moa_blocking_worker_preserves_caller_pause_between_members`：實際 MoA executor、同 production blocking helper；第一 proposer 後暫停 caller，第二 proposer／aggregator 不啟動，flags 回到 parent。
- `dispatcher_fence_tests.rs::sandbox_admission_refusal_defers_only_before_first_handoff`：sandbox 首次 admission 拒絕分類為 defer；已啟動後不能重播。
- `autonomy_gate/tests.rs::timeout_reports_only_local_holder_counts_and_paths`：timeout 診斷只含本程序數量／路徑，不洩漏員工 id；原 shared/exclusive bounded fail-closed 測試保留，固定錯誤文字改檢查穩定前綴。
- synthesis 的 long fake model 測試、GVU `autonomy_pause_prevents_second_generator_round`、review `pause_after_evaluator_prevents_the_next_judge_stage`、Night `pause_after_n1_prevents_n2_model_call`：改為 fake transport handoff 後呼叫真正 `mutate_async`，不再用 lifecycle status 假裝 pause writer。

### 靜態審查與待驗證

獨立 code-reviewer 實讀 source/diff 找到 MoA thread-local 遺失與 sandbox late-pause failure 分類兩項 HIGH，已修。另盤點找到 UCCI shadow detached scope 遺失，已補 core owned-hook capture/restore。複審未回報其他確定的 H3 blocker；這不是編譯／測試證據，也不是指定 Daybreak Blue 安全模型審查。

本輪 `git diff --check` 通過。仍需待機器資源允許後編譯、執行以上測試與原 H3 回歸，並驗證真實 HTTP/SSE、PTY、Docker/WSL、workflow timeout／ledger，以及跨程序 pause 競態。沒有測試成功、平台實機成功或發版可用的宣稱。


## Daybreak 複審修補輪 2026-10-29，static-only

沿用 `feat/autonomy-pause-20261028` 與既有未提交 worktree。未建立／移除 worktree，沒有 commit／push；**沒有執行 cargo、rustc、clippy、rustfmt、cargo fmt、npm 或任何 build/test**。只讀取 source、靜態修改與 `git diff --check`。前段測試紀錄屬前輪，不是這次驗證證據。

四項 finding 已完成靜態修補與測試撰寫，尚未編譯／執行，不能當作 ship approval。獨立 code-reviewer 實讀本輪程式，發現 orphan audit token 靜默略過，已修；增量複查未再發現確定 blocker。這是一般靜態審查，沒有冒稱重新執行指定 `gpt-daybreak-blue-latest` 審查。

### 逐項修補與設計選擇

| Finding | 修改與界線 |
|---|---|
| HIGH-1 | Gateway production utility（含 hinted／Claude public／lightweight）入口先 `require_utility_context`，缺自主 scope 與明確 human marker 即拒絕，發生在 provider/config lookup 與 model probe 前。選擇使用者允許的最小 fail-closed 方案，保留既有 utility 簽章；新增 public `UtilityContext`／`with_utility_context` 供 CLI 明確宣告。Owner 綁定在工作入口，避免把 accounting label 當身份。Human marker 不跨 Tokio spawn，不能覆蓋 autonomous scope；非 Claude utility 的 Evolution 計費分類不會把明確真人 helper 誤判為自主工作。 |
| HIGH-1 sweep | Summary/title 攜帶 session agent_id；knowledge ingest 兩條 cloud 路徑與 scheduled ingest 都 scoped。額外補四個 chat detached lightweight 工作、autopilot local/proactive scorer、無員工的 detached expert job、CLI MCP fork judge/branch spawn。UCCI 無自主 hooks 時略過背景 shadow。同步人工入口另有明確 marker。 |
| HIGH-2 | Goal policy selection = assigned_to＋task caller scope，pause 拒絕不採 fallback 改派。Kickoff/needs-human trajectory 共用 scoped helper。Responsibility scorer = owner＋occurrence caller，task 缺失／讀錯拒絕，fixture 建真實 row。真正 transport 仍在 shared lease 內重新檢查，不以另一個裸 check 取代。 |
| MEDIUM | Gateway-owned task 擁有 requested audit → tracked mutation → completion audit/push；RPC await JoinHandle，斷線不取消。requested/completion 同一 change_id；state 在同次 rename 寫 applied_change_ids，重啟精確對帳，也能補被後續 pause/resume 覆蓋的變更。補回條標 reconciled:true，push 用目前狀態。最多保留 4096 個 token，滿則拒絕；只清 audit 已確認且同步的完成 token。Audit 缺失／損壞／orphan 明確報錯並保留，不偽造成功。 |
| LOW | Unix state/marker/lock 開啟後驗證 fd metadata：owner=euid、nlink=1；自己的過寬權限用 fd fchmod 收緊 0600，重讀確認，失敗 deny/state_error。Windows 維持原樣，not verified。 |

關鍵程式位置：

- `crates/duduclaw-gateway/src/autonomous_runtime.rs:44`
- `crates/duduclaw-gateway/src/runtime_dispatch.rs:398`
- `crates/duduclaw-gateway/src/goal_loop/tick.rs:413`
- `crates/duduclaw-gateway/src/goal_notify.rs:626`
- `crates/duduclaw-gateway/src/responsibility/notify.rs:322`
- `crates/duduclaw-gateway/src/handlers/autonomy_rpc.rs:203`
- `crates/duduclaw-gateway/src/handlers/autonomy_rpc.rs:301`
- `crates/duduclaw-gateway/src/server.rs:1937`
- `crates/duduclaw-core/src/autonomy_gate.rs:121`
- `crates/duduclaw-core/src/autonomy_gate.rs:488`

### 模型呼叫 sweep

搜尋 gateway、agent、CLI 的 run_utility_prompt／hinted variant、call_claude_cli、run_agent_prompt、direct API、provider complete/stream、本機 generation 與 detached spawn。表中是 production 呼叫脈絡；共用 H3 transport hooks 仍在 response headers／spawn 後釋放 lease，沒有改回整輪持鎖。行號依本輪收尾 source。

| 位置 | 分類與處置 |
|---|---|
| `crates/duduclaw-gateway/src/session_summarizer_task.rs:183` | Periodic summary：tick 保留 SummaryCandidate.agent_id，target scope。 |
| `crates/duduclaw-gateway/src/session_titler_task.rs:260` | Periodic title：TitleCandidate＋SQL 新帶 agent_id，tick target scope。 |
| `crates/duduclaw-gateway/src/wiki_ingest.rs:1067,1692` | 兩個 cloud 分支：run_ingest 入口 target scope；涵蓋 chat detached 與 scheduled ingest，後者額外 capture/restore 原 caller。 |
| `crates/duduclaw-gateway/src/channel_reply/history.rs:155` | 真人當回合 compression；繼承 guarded reply human marker；自主 scope 若存在仍優先。 |
| `crates/duduclaw-gateway/src/dispatch_policy.rs:326` | Goal policy model：goal_loop/tick 的 assigned_to＋task caller scope；pause 拒絕不採 policy fallback 改派。 |
| `crates/duduclaw-gateway/src/goal_notify.rs:609,1051` | needs-human 與 kickoff trajectory：run_task_trajectory 共用 target＋task caller scope。 |
| `crates/duduclaw-gateway/src/proactive_gate.rs:338` | Production scorer target scope，繼承 caller；responsibility 外層再加入 occurrence caller。缺 task／讀錯皆拒絕。Autopilot 的通知／OS-goal 評分同樣涵蓋。 |
| `crates/duduclaw-gateway/src/prediction/forced_reflection.rs:184` | Detached silence reflection callback：既有 gvu/loop_.rs target scope 包住 AEE。 |
| `crates/duduclaw-gateway/src/prediction/subagent_prediction.rs:200` | Detached prediction callback：既有 GVU target scope 包住模型工作。 |
| `crates/duduclaw-gateway/src/skill_lifecycle/synthesis_runner.rs:279` | 既有 trigger.agent_id scope 包生成 callback；publication 另用短 lease。 |
| `crates/duduclaw-gateway/src/skill_synthesis_pipeline/pipeline.rs:472` | 既有 caller-aware pipeline scope；utility 與 direct API 都重查 target/caller。 |
| `crates/duduclaw-gateway/src/team_composer.rs:4570` | Team verifier：既有 task.assigned_to＋task caller scope。 |
| `crates/duduclaw-gateway/src/dispatch_engine/judge.rs:411` | Review evaluator/MAV：既有 review target＋task caller scope；transport 每次 admission。 |
| `crates/duduclaw-gateway/src/goal_loop/plan.rs:243,322` | Chat decompose/plan-first 明確 human marker；MCP／排程共用 goal_create_core 依 task target＋creator scope，不以 planner accounting label 當身份。 |
| `crates/duduclaw-gateway/src/situation_classifier.rs:483` | MCP OS classification：由真實 agent_dir 取 target，安裝 autonomous scope。 |
| `crates/duduclaw-gateway/src/decide.rs:118` | Production caller action_review::review 依真實 agent_dir 安裝 target scope，非默認真人。 |
| `crates/duduclaw-gateway/src/os_intent.rs:1017` | Chat L2 router：guarded reply 明確 human marker。 |
| `crates/duduclaw-gateway/src/agent_files/review.rs:210` | 同步 dashboard file-review：明確 human marker。 |
| `crates/duduclaw-gateway/src/redaction_custom_rules.rs:1379` | 同步 operator suggestion：本機與 cloud 同受明確 human marker。 |
| `crates/duduclaw-gateway/src/channel_reply/inner.rs:2200,2457,2511,3690` | 四個 detached 工作：決策擷取、指令釘選、key-fact、壓縮，各綁定真實 target。壓縮遭 pause 直接保留歷史，不用 fallback 摘要覆蓋。 |
| `crates/duduclaw-gateway/src/autopilot_screen.rs:562`；`crates/duduclaw-gateway/src/autopilot_engine.rs:1369` | 事件／timer local screen：action target＋event agent caller；無員工則 global scope/admit_global。 |
| `crates/duduclaw-gateway/src/gvu/loop_.rs:306`；`crates/duduclaw-gateway/src/gvu/aee/inner_loop.rs:280,600`；`crates/duduclaw-gateway/src/gvu/aee/run.rs:885` | AEE generator/scorer/outer judge：既有 target scope，子模型每次 transport admission，不整輪持鎖。 |
| `crates/duduclaw-gateway/src/night_llm.rs:201`、`crates/duduclaw-gateway/src/night_llm.rs:245` | Night N1/N2：既有 night_engine callback target scope；CLI/API fallback 都受 gate。 |
| `crates/duduclaw-gateway/src/skill_synthesis_pipeline/pipeline.rs:468` | Direct synthesis API：同 caller-aware pipeline scope。 |
| `crates/duduclaw-gateway/src/expert_generate_staged/runner.rs:59`、`crates/duduclaw-gateway/src/expert_generate_staged/runner.rs:93`；`crates/duduclaw-gateway/src/handlers/experts_generate_rpc.rs:61` | Detached expert job 無員工，job 內 global scope，每個 stage/fallback 都 admit_global。 |
| `crates/duduclaw-gateway/src/handlers/widgets_custom.rs:230`、`crates/duduclaw-gateway/src/handlers/widgets_custom.rs:269` | 同步 operator widget generation：CLI/API 均明確 human marker。 |
| `crates/duduclaw-gateway/src/causal_extraction_runner.rs:145`；`crates/duduclaw-gateway/src/server.rs:7816` | 同步 admin HTTP：server 明確 human marker；沒有找到 background production caller。 |
| `crates/duduclaw-gateway/src/channel_reply/inner.rs:1539,1909`；`crates/duduclaw-gateway/src/channel_reply/direct_api.rs:35` | 真人 reply/MoA/direct fallback：guarded reply human marker；不是 detached maintenance。 |
| `crates/duduclaw-gateway/src/direct_api.rs:829` | 共用 direct/MoA transport 沿呼叫方 scope；MoA blocking worker 既有 capture/restore。每次 send 重新 admission。 |
| `crates/duduclaw-inference/src/engine.rs:390` | UCCI detached shadow：只有捕獲自主 hooks 才啟動；無 scope 略過 strong background generation，真人 fast reply 保留。 |
| `crates/duduclaw-cli/src/mcp_recording_distill.rs:760` | MCP distill：真實 target autonomous，保留 CLI env caller。 |
| `crates/duduclaw-cli/src/mcp/action_guard.rs:159` | MCP ActionGuard：額外傳真實 agent_id，target autonomous，保留 CLI env caller。 |
| `crates/duduclaw-cli/src/mcp_fork.rs:286` | Fork judge：persisted fork owner autonomous。 |
| `crates/duduclaw-cli/src/eval/judge.rs:95` | Operator eval：沒有任何 AI markers 才 HumanInteractive；employee env 走 target/caller，缺失／錯誤身份拒絕。 |
| `crates/duduclaw-cli/src/eval/verifier_cell.rs:632` | Operator verifier：同 CLI explicit context helper。 |
| `crates/duduclaw-cli/src/secaudit/llm_util.rs:130` | Operator AI audit/adversarial/PoC：同 CLI explicit context helper。 |
| `crates/duduclaw-cli/src/mcp_fork_exec.rs:843,1486` | Detached fork branch CLI：execute_fork 讀 persisted owner；executor capture/restore 跨 branch spawn；最終 spawn 前 lease，成功立即 handed_off，無 context 拒絕。 |
| `crates/duduclaw-agent/src/heartbeat.rs:1293` | Heartbeat timer：既有 employee admission；spawn 後釋放。 |
| `crates/duduclaw-agent/src/runner.rs:512` | Interactive CLI runner，未找到 gateway timer/detached caller；不是背景 utility。 |
| `crates/duduclaw-cli/src/proxy.rs:571`、`crates/duduclaw-cli/src/proxy.rs:629`、`crates/duduclaw-cli/src/ccr_run_cmd.rs:187` | Standalone client proxy／explicit CCR operator request；未找到 gateway background caller，不宣稱獨立客戶端受 gateway pause。 |
| `crates/duduclaw-gateway/src/prediction/engine.rs:638`；`crates/duduclaw-gateway/src/server.rs:940` | 可選 embedding 路徑；production gateway 使用 None/vocabulary fallback，並未啟用 provider。未來啟用必須補 scope，不宣稱所有 embedding 用量受控。 |

Persona/rule induction 是規則／統計／記憶操作，self_study 只建立 goal，未找到直接模型呼叫。Dispatcher／cron／mail／reminder／workflow／ephemeral 沿前輪 runtime scopes；Claude/Codex/Grok/Antigravity/generic CLI、API、local OpenAI-compat、PTY transports 延續 H3 每次 admission。Standalone CLI proxy 不當成已受 gateway 暫停管制的背景服務。

### 本輪檔案清單

只列本輪實際修改，不把前輪全部 dirty files 算進本輪：

- Core：`autonomy_gate.rs`、`autonomy_gate/tests.rs`。
- Gateway 共用／背景：`autonomous_runtime.rs`、`runtime_dispatch.rs`、`session.rs`、`session_summarizer_task.rs`、`session_titler_task.rs`、`wiki_ingest.rs`、`claude_runner.rs`、`channel_reply/{guarded,inner,cli_spawn}.rs`、`autopilot_engine.rs`、`proactive_gate.rs`。
- Gateway policy／通知：`goal_loop/tick.rs`、`goal_loop/tests/tick_cases.rs`、`goal_notify.rs`、`responsibility/notify.rs`、`responsibility/tests/notify_cases.rs`。
- Gateway 入口：`goal_create_core.rs`、`chat_commands.rs`、`situation_classifier.rs`、`action_review.rs`、`agent_files/review.rs`、`redaction_custom_rules.rs`、`handlers/{experts_generate_rpc,widgets_custom,autonomy_rpc}.rs`、`handlers/autonomy_rpc/tests.rs`、`server.rs`。
- CLI：`ai_session_guard.rs`、`mcp_recording_distill.rs`、`mcp/{action_guard,approval}.rs`、`mcp_fork.rs`、`mcp_fork_exec.rs`、`eval/{judge,verifier_cell}.rs`、`secaudit/llm_util.rs`。
- Inference：`duduclaw-inference/src/engine.rs`。
- 文件：`CHANGELOG.md`、三語 `docs/guides/{,zh-TW/,ja-JP/}continuous-responsibilities.md`、本筆記。

### 新增測試（全部未執行）

- `utility_without_declared_context_is_denied_before_model_entry`
- `human_marker_does_not_exempt_nested_or_detached_background_work`
- `paused_session_maintenance_keeps_real_owner_and_skips_models`
- `pause_after_goal_preflight_prevents_policy_model_handoff`
- `pause_after_trajectory_preflight_prevents_model_handoff`
- `pause_after_notice_preflight_prevents_scorer_model_handoff`
- `missing_notice_task_refuses_before_scoring`
- `autopilot_screen_pause_after_check_refuses_target_caller_and_global_handoff`
- `utility_context_requires_an_unmarked_operator_or_an_employee_identity`
- `detached_fork_branch_retains_target_and_caller_at_handoff`
- `ucci_shadow_without_autonomous_context_never_starts_strong_model`
- `cancelled_autonomy_rpc_still_completes_audit_and_push`
- `autonomy_startup_reconciles_exact_applied_tokens_once_even_if_superseded`
- `autonomy_orphaned_applied_token_reports_error_and_is_preserved`
- `existing_autonomy_files_are_tightened_before_use`
- `multiply_linked_autonomy_files_are_refused_without_chmod`
- `foreign_autonomy_owner_is_refused_by_metadata_validation`

HIGH-2、autopilot/fork pause-after 測試涵蓋 target/caller/global：真正 mutate_async 後才走 fake transport admission，斷言零模型 handoff。Foreign owner 以 metadata predicate 的不相符 expected euid 測拒絕，未做需 root 的實際 chown。既有四個 UCCI shadow 排程測試改明確 fake host hooks；utility billing fixture 改明確 global scope，保留原 billing 斷言。

### 檢查結果與尚待驗證

- `git diff --check` 通過，只代表 whitespace 檢查。
- 已靜態核對新增 scope/context、TaskRow/SessionManager、mutation/audit、core handoff APIs；TitleCandidate 建構處與 SQL 同步。沒有已知尚未修掉的確定型別錯誤，但**沒有 compiler 證據**。Generic future/Send、CLI branch capture、state schema 相容性及所有 fixture 須下一輪編譯確認。
- RPC cancellation/restart、provider race、Unix 權限與真實 fork 未實跑；Windows not verified。Windows audit reader 與 writer 的鎖路徑不同，錯誤 fail closed，平台一致性待驗收。
- Completion audit 執行期間不可寫時可能已套用並回覆 audit_error；下次 gateway 啟動補寫，不宣稱即時自癒或 durable push delivery。原 audit 遺失時需修復，不臆測回條。
- 仍是 admission control，不取消已 handoff 模型；HTTP headers 的 30 秒 timeout、workflow execute 10 秒與 same-UID shell 限制維持。


### E0275 編譯修補（2026-10-29）

本段為 static-only 複審之後的編譯修補，驗證命令與 exit code 另記於前面的「驗證結果」E0275 表格；前輪的 static-only 描述保留歷史原意。

- 根因：新增 `human_interactive` 將整個 chat pipeline 再包一層；ACP `tokio::spawn(run_prompt_turn(...))` 的 Send 證明經由 chat task-local、OS intent utility、runtime dispatch、`request_scope` 一直展開到 registry mutex／`RuntimeRegistry` 的 HashMap，超出預設 trait recursion 深度。原本 `Box::pin(future)` 仍是 `Pin<Box<F>>`，只限制 future 大小，沒有抹除具體 `F`，所以無法限制 auto-trait 推導深度；這不是證實 registry 缺少 Sync。
- 修補：`crates/duduclaw-gateway/src/autonomous_runtime.rs:161` 的共用 `request_scope` 在建立 async frame 前把內層轉為 `Pin<Box<dyn Future<Output = F::Output> + Send + 'a>>`，輸入與回傳明確要求 Send。沿用原本一次 boxing，沒有新增 allocation；task-local 的讀取仍在 poll 時，保留 caller 繼承、fail-closed utility context、transport admission 與取消/drop 邏輯。沒有調高 recursion_limit，也沒有要求 `F::Output: Send`。
- 呼叫端：`crates/duduclaw-gateway/src/claude_runner.rs:490` 的 `invoke_recorded<F>` 同步加上 `Send` bound。只有上述兩個 Rust 檔做程式修改與 `rustfmt --edition 2024`，既有測試與功能完整保留。
- 獨立 code-reviewer 以本輪前後快照做只讀 diff 審查，指出並修正 Claude helper 的 Send bound；增量複查無剩餘 blocker。這是一般靜態審查，不是 Daybreak 安全重審。
- 沒有執行測試套件、commit、push 或建立／移除 worktree；保留原分支與其他未提交變更。

最終驗證：上述五套件 `cargo check -j 4 ... --tests` exit 0，0 errors；`git diff --check` 通過。測試執行 0，執行期／平台驗收由 reviewer 後續處理。


### Lib test failures 根因與本輪修改（2026-10-29）

原始證據：`/private/tmp/claude-501/-Users-lizhixu-Project/d8fa32c7-53b7-413b-bb44-6c4fee7c0f59/scratchpad/verify/{cli,gateway}.log`。本輪前快照及增量 diff 分別位於 `/tmp/autonomy-test-fix-20261029-before/`、`/tmp/autonomy-test-fix-review.diff`；前輪未提交內容保留。

**正式程式修補：**

- Core `autonomy_gate::read_state` 原本已將「home 存在，state 與 marker 都不存在」視為未使用並放行；沒有把新安裝改成默認暫停。28 項 CLI 失敗的主要原因是 caller 缺 `agent.toml`／`[agent].status`，不是 missing pause state。Core 只將不存在的員工設定細分為固定 `agent_missing`，未知／錯型別 status、不可讀設定仍拒絕。
- MCP `record_authz::check_background_caller` 原先丟棄 `PauseInfo`，把所有拒絕誤寫成「已暫停」。現在分別顯示員工暫停、全域暫停、暫停狀態不可讀、無效 ID、員工設定不存在、員工狀態不可讀、非 operational、鎖逾時／無法取得及未知錯誤；不輸出原始檔案、路徑或 parser 資料。新增 unused-home、缺員工、錯 status、corrupt/unknown-version state、不可讀 state（目錄）及 marker-without-state 回歸。
- 外層 caller gate 移到 scope、denied/allowed tools、permissions 後；這些拒絕只寫診斷 audit，不會建立工作／核准單。`scoped_tools` 會開啟可寫 `approvals.db`，PolicyKernel Ask 與 approval_required 會建立核准單，均留在 caller gate 後。新增實際 dispatcher 測試確認 permission/list 原錯誤仍優先，paused scoped/approval/Ask 不建立 approvals.db/tasks.db/bus queue。
- 獨立審查額外發現既有 `WorkflowCall::parse` Execute 分支會 `ApprovalBroker::open` 建檔／遷移 schema，卻在 caller gate 前。已將 parse 移到 gate 後、PolicyKernel 前；保留原始 signed metadata，成功後才從 params_owned 移除 `_meta`，後續 prepare/effect/handler 使用清理後參數。此為正式路徑的副作用順序修補；沒有放寬任何 gate。

**Fixture 修補（原斷言保留）：**

| 群組 | 不符合正式環境之處 | 修補 |
|---|---|---|
| `mcp/task_board_tests/part1.rs:30`、`part2.rs:148`，16 件 | 以 agnes/bruno/lone-agent 呼叫員工 handler，但 home 沒有對應員工 | 各失敗案例使用既有 `create_test_agent`，只補該案例的 caller；self-assignment 仍無 org 關係。 |
| `mcp/merged_entry_tests.rs:180,211`，2 件 | goal/schedule 呼叫者 a 不存在 | 沿用相鄰 `mcp::tests::create_test_agent` 建 a；該 helper 僅改為 pub(super) 供 sibling test module 使用。 |
| `mcp/round_parent_tests.rs:304` 的 env child | malformed round env 測試尚未到 round check，就因 worker 不存在被擋 | 在 child fixture 建 worker，保留原拒絕與「沒有建立」斷言。 |
| `mcp/tests/part1.rs:144`、`part7.rs:129`，2 件 | scheduled create／host principal 測試缺 agnes | 使用原 `create_test_agent` 建 agnes，保留 cron changed 與 principal/created_via 斷言。 |
| `mcp_dispatch.rs:2386,3704`、`mcp_dispatch/tests/resp_tests.rs:128` | 共用 TOML fixture 只有 capabilities/permissions，沒有正式 employee 的 active status | 無 agent table 的政策片段補 name/status；明確的 agent table 保留原文，responsibility unlisted fixture 明寫 active。既有 malformed permissions 仍 malformed，原拒絕斷言不改。 |
| `mcp_dispatch/tests/resp_tests.rs:87` internal-key fixture | key 是 gateway-internal，但沒帶正式 MCP 已驗證程序的 ENV_AGENT_ID；`RecordActor` 正確視為未知 internal caller | 以隔離子程序設定 dudu 身分，保留 internal key 與核准單 agent=dudu 斷言，不在平行測試程序 set_var。 |
| gateway `dispatch_engine/tests/engine.rs:112`、`grounding.rs:487` | 自行 seed review task，漏了相鄰 seed_review 已有的 w 員工；preflight 因此先 park needs_human | 沿用 `ensure_autonomy_agent(&store, "w")`；保留 risk prompt 與 grounding-before-judge 斷言。 |
| gateway `runtime_dispatch.rs:935,953` CLI judge subprocess | engine 漏 with_home_dir，無 utility scope，require_utility_context 正確拒絕；正式 `handlers/lifecycle.rs` builder 已有 home | 加 with_home_dir 與 active fixture-worker，保留真 fixture CLI 與 measured usage 斷言；不把 goal-acceptance-judge 計費 label 當員工。 |

審查：獨立 code-reviewer 以本輪前後快照只讀審查，workflow parse 副作用問題已修且複查通過；internal-key fixture 追加修補也通過靜態複查。未執行指定 `gpt-daybreak-blue-latest`（本 session 沒有可呼叫該模型的工具），不宣稱完成安全重審。

主要正式程式位置：`crates/duduclaw-core/src/autonomy_gate.rs:346`（缺 employee 設定分類）、`crates/duduclaw-cli/src/mcp/record_authz.rs:325,341`（真實拒絕原因）、`crates/duduclaw-cli/src/mcp_dispatch.rs:1103,1123`（caller gate／workflow parse 順序）。測試入口：`mcp/record_authz_tests.rs:1249`、`mcp_dispatch.rs:4500`。共享 fixture 的可見性變更：`mcp/tests/mod.rs:59`。

## Reviewer 驗證（Claude，2026-10-29，沙箱外實跑）

在 Codex 沙箱外由 reviewer 自行執行，串行、`-j 4`、本 worktree target。

| 指令 | 結果 |
|---|---|
| `cargo check -j 4 -p duduclaw-core -p duduclaw-agent -p duduclaw-gateway -p duduclaw-cli -p duduclaw-inference --tests` | PASS |
| `cargo test -j 4 -p duduclaw-core --lib` | 1088 passed / 0 failed / 1 ignored |
| `cargo test -j 4 -p duduclaw-agent --lib` | 351 passed / 0 failed / 1 ignored |
| `cargo test -j 4 -p duduclaw-inference --lib` | 84 passed / 0 failed |
| `cargo test -j 4 -p duduclaw-cli --lib` | 2278 passed / 0 failed / 4 ignored |
| `DUDUCLAW_P1_PILOT_BINARY=…/target/debug/duduclaw cargo test -j 4 -p duduclaw-gateway --lib` | 10189 passed / 0 failed / 50 ignored（含先前因沙箱 bind 失敗的 workflow fixture） |
| `cargo clippy -j 4 <同五套件> --tests --no-deps -- -A clippy::approx_constant -A clippy::async_yields_async` | exit 0；本次新增行上 0 個 diagnostic（全體 1516 個皆為既有）。兩個 allow 對應既有 deny 級 lint：`openai_compat.rs` approx_constant、測試內 async_yields_async，皆未被本次修改。CI clippy 為 continue-on-error。 |

未做：真實 LLM、真實 dashboard 瀏覽器、Windows/Linux 原生、跨程序競態實機驗收。

## Daybreak 第三輪修補 2026-10-29，static-only

本段對應 security-fix round 4 的 MEDIUM-1、MEDIUM-2、LOW-1、LOW-2。沿用 `feat/autonomy-pause-20261028`，保留其他 session 的未提交修改，沒有 commit／push，沒有建立或移除 worktree。**未執行 cargo、rustc、clippy、rustfmt、npm 或任何 build／test**；只做 source／signature 靜態核對與 `git diff --check`。前面的「Reviewer 驗證」是修補前證據，不代表本輪已通過。

### 逐 finding 的實際變更

| Finding | 檔案與位置 | 修改 |
|---|---|---|
| MEDIUM-1 + MEDIUM-2 | `crates/duduclaw-core/src/autonomy_gate.rs:65,103,239` | `ChangeReceipt` 包含 change_id、typed method/scope、actor、request_digest、applied_at、completion_recorded。Scope enum 區分全域與名為 all 的員工。讀取時檢查 UUID 長度／格式、員工 ID、actor 256 bytes／控制字元、64 字元小寫 hex digest、RFC3339／時間範圍、重複 ID、4096 上限；壞資料回 state_error。 |
| MEDIUM-1 + MEDIUM-2 | `crates/duduclaw-core/src/autonomy_gate.rs:650,711,774` | Gateway tracked mutation 在 exclusive admission lease 內，同次 atomic rename 提交變更及 pending receipt；applied_at 在鎖內設定。請求 digest 使用固定順序 compact JSON tuple `[method, typed_scope, actor, effective_reason_or_null]` 的 SHA-256，不含產生的時間或無效／未知參數。Core 重新核對 digest／method／scope／actor，拒絕重複 token。滿額只移除 applied_at 最早且 completion_recorded=true 的一筆；全 pending 拒絕，不落地變更。 |
| MEDIUM-1 + MEDIUM-2 | `crates/duduclaw-core/src/autonomy_gate.rs:610,620` | Completion acknowledgement 使用同一 locked atomic writer，不更改目前 pause scope。若 rename 後 sync/readback 失敗，仍持原鎖嘗試寫回 pending；連復原都失敗則記錄 durability unknown，不能承諾故障磁碟已保存 false。 |
| MEDIUM-1 + MEDIUM-2 | `crates/duduclaw-gateway/src/handlers/autonomy_rpc.rs:115,155,228,272` | 移除 ChangeAudits／read_change_audits 及 audit-based reconstruction。Requested audit 在 mutation 前保存 change_id＋request_digest，僅供鑑識。Gateway-owned task 持續處理斷線後 completion。重啟只列舉 state receipts，逐 pending 寫自身欄位的 reconciled completion，push 目前狀態，再標 true；逐項錯誤 log＋continue。Completion／ack 失敗仍回 applied=true。 |
| LOW-1 | `crates/duduclaw-cli/src/mcp/tools_list.rs:498`、`mcp/server.rs:199,477` | `tools/list` 共用 record_actor_for／check_background_caller，任何 Err 隱藏 creates_background_work 工具。agent_update 等參數依賴工具仍列出。Watcher 原本每 tick 重算 visible set，無須新增 mtime watch；抽出共用通知產生函式並測實際輸出。 |
| LOW-2 | `crates/duduclaw-cli/src/mcp_dispatch.rs:744,812,1033,1061` | Rate limiter 移到會寫 audit 的 scanner 前。Scope／list／action-rule／permission verdict 先計算、保留原 error，不寫 audit；background pause gate 後才記單筆適用 audit。Paused refusal 用既有 audit helper、固定內容且不帶 args。一般 permission denial 只保留原 structured security audit，移除重複 tool_calls row。修正原「Pure」誤導註解。 |

LOW-1 fixture：`mcp/tools_list_budget_tests.rs:48,200`、`mcp/standalone_listing_tests.rs:38` 補正式員工的 active status；shared internal key 若沒有 verified employee env，其 background listing 依 dispatch 同樣 fail closed。既有 listing 測試改名為 `employee_listing_is_unchanged_and_unverified_internal_background_is_hidden`，保留 per-agent budget 斷言。

LOW-2 ordering：paused 與 permission/list/scope 同時成立時，仍回較早的具體 error，但 audit 只留固定 autonomy_pause；permission 拒絕不再另產生 injection audit 或重複 tool_calls。既有 `explicit_false_permission_refuses_and_audits` 的 structured security audit 斷言未放寬。

### Schema 與回條界線

- `applied_change_ids` 只存在這個尚未合併、未提交分支；此檔目前為 untracked，`git ls-tree origin/main crates/duduclaw-core/src/autonomy_gate.rs` 無項目。依交辦不做 migration；舊實驗 token 格式 fail closed，不讀 audit 猜測回條。無 receipts 的基本 v1 狀態仍可讀為空回條。
- 正常 crash-after-mutation 後，第一次啟動補寫並標記完成，第二次不重複。Audit/push 與 ack 是分開寫入；若中間再次 crash 或 ack 失敗，下次會重複同 change_id 的 completion，屬 at-least-once，並非全面 exactly-once。Push 維持 best-effort broadcast，無訂閱者不視為永久失敗。
- Reconciliation 的決策來源與 receipt 數量有界限，不讀 security_audit.jsonl 還原結果。**既有共用 audit append 基礎設施仍有殘餘成本**：`try_append_audit_event` → `audit_chain::last_line_hash`（`crates/duduclaw-security/src/audit_chain.rs:108`）為 hash chain 讀取整檔。本輪沒有修改全平台 audit writer，不能宣稱全部 audit I/O 已消除 O(whole audit) 或巨大單行成本。這與已刪除的 reconciliation 掃描／信任 audit row 不同，仍待獨立處理。

### 測試撰寫／改寫（全部未執行）

Core `autonomy_gate/tests.rs`：

- `autonomy_receipt_cap_prunes_oldest_completed_and_refuses_all_pending`（312）：4096 上限、按時間而非 vector 順序淘汰、全 pending 的 pause/resume 均拒絕且檔案不變。
- `autonomy_invalid_receipts_fail_closed`（352）：欄位／scope／時間／重複 ID 損壞皆拒絕。
- `autonomy_receipt_schema_accepts_no_receipts_but_rejects_unmerged_token_format`（387）。
- `autonomy_receipt_digest_binds_effective_request_and_scope`（401）：穩定 digest、all 員工不同於全域、錯 payload／重複 receipt 拒絕、ack 保留 pause。
- `autonomy_completion_sync_failure_restores_pending_without_changing_pause`（426）：注入 rename 後 sync error，pending 復原與重試。

Gateway `handlers/autonomy_rpc/tests.rs`：

- 改寫 `autonomy_rpc_reports_and_audits_applied_but_durability_unknown`（324）：真實 tracked receipt，保留 applied／warning 斷言。
- 擴充 `cancelled_autonomy_rpc_still_completes_audit_and_push`（353）：比對 requested/completion digest，等 ack 並驗證完成旗標。
- 改寫 `autonomy_startup_reconciles_state_receipts_once_even_if_superseded`（411）：rename 後缺 completion，無 requested audit 仍補齊；歷史 pause 後已 resume 的 push 反映目前狀態；再 reconcile 不重複。
- `autonomy_forged_duplicate_audits_cannot_change_reconciliation`（445）：重複 requested、同 UUID 假 completion、未套用 UUID 都不能改寫／抑制 state receipt。
- `autonomy_completion_failure_keeps_receipts_and_retries_on_startup`（484）：audit 不可寫仍回 applied，所有 receipts 留 pending，修復後補寫。
- `autonomy_acknowledgement_failure_reports_applied_and_preserves_pending`（510）：audit/push 成功但 ack 鎖無法開啟，回 applied＋audit_error、保留 pending；重試的重複紀錄同 ID。
- 移除舊 audit-scanning／orphan-token 測試與 helper，未保留舊 audit 信任路徑。

CLI：

- `paused_caller_hides_background_tools_and_resume_restores_them`（`mcp/tools_list_budget_tests.rs:410`，含 corrupt state）。
- `pause_resume_emit_tools_list_changed_once_per_visible_change`（同檔 430）。
- `paused_background_denial_writes_one_bounded_record_before_injection`（`mcp_dispatch.rs:4578`）。
- `rate_limited_injection_writes_no_audit`（同檔 4609）。

### 文件、審查與未驗證項目

同步 `CHANGELOG.md` [Unreleased] Security、三語 `docs/guides/{,zh-TW/,ja-JP/}continuous-responsibilities.md`，以及 `docs/api/openapi.yaml:279,322` 的 receipt／reconciliation／ack-error 語意。

一般獨立 code-reviewer 靜態核對找到 ack post-rename failure，已修補並複查，未回報確定型別錯誤或新增 HIGH blocker；不是指定 `gpt-daybreak-blue-latest` 安全重審。`git diff --check` 通過，只代表 whitespace。本輪 snapshot：`/tmp/autonomy-round4-before/{core,rpc}.rs`、`/tmp/autonomy-mcp-round4-before/`。

編譯風險交 reviewer：新增 serde scope enum 與 receipt schema、MutationOutcome 分支／atomic-writer closure ownership、CLI async verdict block 的型別推導，以及 listing fixtures 的原始數量斷言均只靜態核對，沒有 compiler／測試證據。測試均未執行；真實 RPC cancellation、crash、檔案系統 sync 故障、Windows/Linux 原生與 Dashboard 通知未驗收。共享 audit writer 的整檔 hash 成本仍是明確剩餘限制。


## 第四輪驗證修補

- **Ephemeral 工具列表**：失敗 fixture 使用 shared internal key，卻沒有員工程序身分，且 `agent.toml` 缺少 `[agent] status = "active"`。背景 gate 正確拒絕這兩種情況；`tools/list` 與 dispatch 都共用 `record_actor_for`／`check_background_caller`，正式授權邏輯未放寬。Fixture 改在隔離子程序提供員工身分與真實 shared-key scope，補 active status；驗證未暫停時可見 `team_handoff`、暫停後隱藏、恢復後重新出現，並保留損壞與遺失設定的拒絕測項。
- **Atomic rename 讀取競態**：讀者先 open 舊 state，writer 再 rename 新 state 時，舊 fd 的 inode 會變成 `nlink = 0`；原本「必須等於 1」的檢查把正常替換當成異常。現在僅對 read-only state 的 detached fd 丟棄重開，兩次 fstat 都處理，最多八次；每次重新檢查 owner、regular file、no-follow、權限與 link count。真正 hardlink（`nlink > 1`）、foreign owner、遺失狀態、持續替換耗盡與 marker／lock 異常仍 fail closed。確定性回歸測試用正式 writer 在兩個 fstat 時機替換狀態，確認讀到新 revision，並測硬連結、刪除與重試上限。
- **Workflow timeout**：原始 panic 經 `Resume::dispatch`，位於 `to_effect_card` 建立卡片的 connect／prepare 階段（各 120 秒），尚未進入 pause race 或 `autonomy_gate::admit`。Admission 的拒絕與 execute 的 10 秒逾時使用不同錯誤字串。未找到可支持修改 timeout 或 admission 的缺陷。使用本輪新建置的 CLI binary 隔離重跑時，fixture 在 `pilot_test_factory.rs:59` 建立 `127.0.0.1:0` listener 即遭 sandbox `PermissionDenied`（0 通過、1 失敗，0.12 秒），未進入 workflow；因此尚未完成三次隔離重跑，也未執行同模組整組測試。原本重負載是否為逾時根因仍未證實。
- **Clippy**：scope gate 合併巢狀 `if`，保留原拒絕條件與回應。State reopen 迴圈僅在 Unix 編譯，避免非 Unix 新增 `never_loop`。

本輪實測：ephemeral 指定測試修補前 0/1、修補後 1/0；`tools_list` 19/0、`record_authz` 26/0、`mcp_dispatch::tests::` 84/0、core `autonomy` 25/0、gateway `handlers::autonomy_rpc` 15/0（皆為通過／失敗數）。`cargo build -j 4 -p duduclaw-cli` 通過。Cargo 全部序列執行且使用 `-j 4`，未跑 fmt 或全工作區測試。一般獨立程式碼審查已完成；指定 Daybreak Blue 安全審查未執行，本環境未提供該模型。

`cargo clippy -j 4 -p duduclaw-cli --lib` 結束碼 0；本輪 scope gate 的 `collapsible_if` 已消失，其餘既有警告仍在。`git diff --check` 通過。

## Reviewer 驗證（Claude，2026-10-29，第四輪修補後，沙箱外實跑）

全部 `-j 4`、一次一個 cargo。

| 項目 | 結果 |
|---|---|
| `cargo check` core/agent/gateway/cli/inference `--tests` | 通過 |
| core lib | 第一次全量 1095／1 失敗（`gateway_instance::tests::a_home_held_elsewhere_is_refused_and_marked_for_doctor`）。根因：本分支新測試 `autonomy_special_files_fail_closed_without_blocking` 用子程序執行 `mkfifo`，fork 到 exec 之間子程序共用其他測試的 flock，`gateway_instance` 測試 drop 後立即重取鎖失敗。只跑這兩個測試時 5 次失敗 3 次；單執行緒或略過 mkfifo 測試則全過。改用 `libc::mkfifo` 後，全量 5 次皆 1096／0，兩測試並行 10 次皆通過。正式碼不受影響。 |
| agent / inference / cli lib | 351／0、84／0、2282／0 |
| gateway lib | 10191／0（524 秒）。前一次全量在 `handlers::tests::knowledge_quarantine_decide_tests::approve_and_deny_race_records_what_was_applied` 卡住 77 分鐘、CPU 0%；該測試與其程式路徑未被本分支修改，單獨重跑 5 次皆 <1 秒通過，下一次全量也通過。視為既有偶發卡死，**根因未查明**。 |
| `workflow::f1b_e2e_tests::autonomy_pause_after_prepare_preserves_effect_for_resume` | 單獨 3 次通過（9.9／5.3／5.4 秒），同模組 6／0。原失敗發生在另一 session 同時編譯的重負載期間。 |
| 新增行 clippy（`-A clippy::approx_constant -A clippy::async_yields_async`） | 0 個真正新增。工具回報 `mcp_dispatch.rs:765,766` collapsible_if，但那段 namespace 注入程式碼在 base 第 930 行即存在，是 diff 對齊把搬動的區塊算成新增；未修改。 |


## Daybreak 第四輪修補 2026-10-29，static-only

本段對應本次 MEDIUM-1（ephemeral 父鏈）、MEDIUM-2（MCP check-to-commit 競態）、LOW-1（兩個技能工具列表漏分類）。沿用 `feat/autonomy-pause-20261028`，保留既有未提交功能。未執行 cargo、rustc、clippy、cargo fmt、任何 build 或 test；沒有 commit、push、stash、切換分支或建立／移除 worktree。前面的 reviewer 實跑紀錄不適用於本次修改。

### 逐 finding 修改

下表路徑皆相對於 repo root；行號為本次靜態交付時的位置。

| Finding | 檔案與位置 | 修改 |
|---|---|---|
| MEDIUM-1 | `crates/duduclaw-core/src/autonomy_gate.rs:416,473,499,664` | 共用 `effective_identities` 回傳 caller 加 ephemeral 父鏈。每層驗證 `.ephemeral` containment、完整 sidecar、host-owned org parent 一致性；循環、資料無法讀取與超過 32 個身分拒絕。`check` 檢查整條鏈的 pause／live lifecycle，`admit` 在同一把 home-wide shared lease 內使用同一檢查。普通員工仍只檢查自己，不繼承一般組織上級的 pause。Ephemeral lifecycle 固定讀取 scaffold，普通目錄同名員工無法遮蔽它。 |
| MEDIUM-1 | `crates/duduclaw-gateway/src/ephemeral/dispatch.rs:41`；`crates/duduclaw-cli/src/mcp/record_authz.rs:325,379` | Gateway 改用 core resolver；MCP tools/list、外層 dispatch 與最終 admission 都經同一 core check。新增穩定 `ephemeral_parent_invalid` 類別；missing final parent／壞 agent status 沿用既有固定類別，不輸出 parser/path 原文。 |
| MEDIUM-2 | `crates/duduclaw-cli/src/mcp/record_authz.rs:347,356,367`；`crates/duduclaw-cli/src/mcp/dispatch.rs:192` | Task-local 只攜帶已驗證的 caller／operator 分類，不攜帶長時間 lease。`admit_background_caller`／`admit_background_identity` 在 durable boundary 重取 lease；拒絕沿用 dispatch formatter。Scope 在建立 future 時先 boxing，避免把巨大 dispatch future 放入 async 初始 frame。 |
| MEDIUM-2 | `crates/duduclaw-cli/src/mcp/delegation.rs:134`；`crates/duduclaw-cli/src/mcp/tasks.rs:338,354,535,770,859` | send_to_agent queue 與 callback blocking writers 各持有 Arc lease；tasks create／update／claim 保護資料列及 task wake events。Goal 使用 lazy admission，模型規劃完成才取鎖。若 goal 已建立後才被暫停，省略晚到的 best-effort wake event，仍回成功建立，避免重試產生重複任務。 |
| MEDIUM-2 | `crates/duduclaw-gateway/src/goal_create_core.rs:111,276,327`；`crates/duduclaw-gateway/src/discovery/service.rs:233,257,386`；`crates/duduclaw-cli/src/mcp/discovery.rs:16` | 可選 lazy admission 不改既有 dashboard 呼叫；goal 保護 insert、team freeze 與 activity，discovery 在 spec 驗證及 evaluator hashing 後才取鎖，保護 frozen request、approval、task insert。 |
| MEDIUM-2 | `crates/duduclaw-cli/src/mcp/cron_tasks.rs:165,492,615,655`；`crates/duduclaw-cli/src/mcp/reminders.rs:181`；`crates/duduclaw-gateway/src/reminder_scheduler.rs:490` | cron 新增／修改／切換與 reminder append 取得 lease；reminder guard 移入真正 blocking writer。 |
| MEDIUM-2 | `crates/duduclaw-gateway/src/cron_scheduler.rs:617,627,1272`；`crates/duduclaw-gateway/src/condition_eval.rs:316`；`crates/duduclaw-gateway/src/autonomous_runtime.rs:34,58,148,262` | Manual cron 保留 caller 及有效 target scope，condition/watch spawn 也重新 admission，spawn 後即釋鎖。跨 detached transport 保留第一個 typed PauseInfo，晚到拒絕不再被 cron 吞成成功 summary；即使隨後 resume，仍回原本的固定拒絕分類。 |
| MEDIUM-2 | `crates/duduclaw-cli/src/mcp/spawn.rs:132,318,364,465`；`crates/duduclaw-cli/src/mcp/bus_tasks.rs:212`；`crates/duduclaw-cli/src/mcp/tasks_goals.rs:203,563` | 保護 spawn bus、ephemeral scaffold／capacity queue／bus、legacy TaskSpec + signal、獨立 goals_create 及 plan_update_step。`plan_start` 只有回傳 scaffold，沒有 durable write。 |
| MEDIUM-2 | `crates/duduclaw-cli/src/mcp/team_handoff.rs:483`；`crates/duduclaw-cli/src/mcp/working_state.rs:112`；`crates/duduclaw-cli/src/mcp/responsibilities.rs:157,216` | Packet／working-state guard 移入 blocking writer；responsibility followup／ask 保護 wakeup／approval 寫入。 |
| MEDIUM-2 | `crates/duduclaw-cli/src/mcp/agents.rs:233`；`crates/duduclaw-cli/src/mcp/agent_update.rs:591`；`crates/duduclaw-cli/src/mcp/evolution.rs:155`；`crates/duduclaw-cli/src/mcp_fork.rs:401,568`；`crates/duduclaw-cli/src/mcp_recording_distill.rs:805` | 保護建立員工、能喚醒工作的設定、evolution toggle、fork insert／merge publication、錄製蒸餾的草稿與核准寫入。create_agent 的短檔案提交使用同步 I/O，避免 Tokio 檔案工作在取消後繼續但 guard 已釋放。 |
| MEDIUM-2 | `crates/duduclaw-gateway/src/skill_synthesis_pipeline/pipeline.rs:532,542,568`；`crates/duduclaw-gateway/src/skill_lifecycle/graduation.rs:99,114` | Synthesis 仍保留 target gate，另取得 caller lease；publication／graduation／cleanup／log 全部在持有 guards 的單一 blocking closure 內完成。模型與安全掃描在取鎖前完成。既有 async graduation API 共用同步實作。 |
| LOW-1 | `crates/duduclaw-cli/src/mcp/record_authz.rs:403`；`crates/duduclaw-cli/src/mcp/skills_lifecycle.rs:377`；`crates/duduclaw-cli/src/mcp_dispatch.rs:4529` | `skill_synthesis_run`／`skill_from_recording` 納入同一 classifier；保留 synthesis target-specific check，caller 使用共用拒絕文字。新增實際 dispatcher 與可見列表的 pause／resume parity test，啟用時用無效參數停止於驗證，不啟動模型。 |
| 文件 | `CHANGELOG.md:7`；`docs/guides/continuous-responsibilities.md:345`、`docs/guides/zh-TW/continuous-responsibilities.md:345`、`docs/guides/ja-JP/continuous-responsibilities.md:345` | 延伸現有 Security 項目，三語說明兩個技能工具、執行中 ephemeral 父鏈繼承及 durable commit 與 pause 的先後關係。 |

### Resolver 與 lease 界線

Admission lock 原本就是 home-wide；因此一把 shared lease 足以保護 caller 及整條父鏈，不需每層再取相同鎖。Pause writer 的 exclusive lease 與這把鎖互斥：pause 先完成，後續 admit 拒絕；write 先取得 lease，pause 等該次 durable write 完成才可成功。30 秒取鎖上限沿用既有值。沒有把 shared lease 包住整個 handler 的模型工作或長時間解析。資料庫 schema 開啟／讀取及診斷稽核不等於建立背景工作；競態測項檢查實際任務／佇列零筆落地。

### 同類入口掃描清單

逐一核對 classifier 的 24 個名稱：`send_to_agent`、`spawn_agent`、`spawn_ephemeral`、`tasks_create`（task／goal／cron／reminder／discovery）、`goals_create`、`create_task`、`tasks_update`、`tasks_claim`、`update_cron_task`、`run_cron_task`、`pause_cron_task`、`create_reminder`、`responsibility_followup`、`responsibility_ask`、`plan_start`、`plan_update_step`、`working_state_handoff`、`team_handoff`、`fork_run`、`merge_or_select`、`create_agent`、`evolution_toggle`、`skill_synthesis_run`、`skill_from_recording`。另核對依參數判斷的 `agent_update`（status／heartbeat）。

以 `autonomy_gate`、`autonomy_handoff`、`require_utility_context`、`with_autonomous_utility_context`、`admit_background` 掃描 CLI MCP handlers 及其 gateway 下游。直接 utility 呼叫對應 synthesis、recording、fork；`crates/duduclaw-cli/src/mcp/action_guard.rs:159` 是共通政策審查 judge，不是獨立 MCP tool，不把所有接受政策審查的讀取工具誤分類為背景建立工具。`require_utility_context` 本身在 gateway runtime 共通入口，未找到另一個獨立且漏列的背景建立工具。

### 新增／擴充測試（全部未執行）

- Core `crates/duduclaw-core/src/autonomy_gate/tests.rs:535,559,581,609,620`：`autonomy_ephemeral_chain_inherits_pause_and_admission_lease`、`autonomy_ephemeral_missing_and_corrupt_parent_metadata_fail_closed`、`autonomy_ephemeral_requires_host_authority_and_refuses_cycles_and_depth`、`autonomy_regular_employee_does_not_inherit_org_parent_pause`、`autonomy_ephemeral_lifecycle_uses_scaffold_not_regular_shadow`。**未執行**。
- CLI `crates/duduclaw-cli/src/mcp/wiki_namespace_tests/part1.rs:421`：擴充 `role_member_tools_list_uses_ephemeral_capabilities`，保留隔離 child process；補 active parent、完整 sidecar、org authority，測 child／parent pause、resume、missing parent、corrupt metadata、cycle 的 listing／dispatch gate 一致性；保留原 malformed capabilities／missing child 斷言。**未執行**。
- CLI `crates/duduclaw-cli/src/mcp/delegation.rs:774`：`send_to_agent_pause_between_check_and_insert_lands_no_message`；`crates/duduclaw-cli/src/mcp/tasks.rs:1163`：`tasks_create_pause_between_check_and_insert_lands_no_task`。只在 `#[cfg(test)]` 安裝 task-local barrier，固定在 early check 後、insert 前完成 pause；確認零筆資料及一致拒絕，resume 後確認一筆成功寫入。**未執行**。
- CLI `crates/duduclaw-cli/src/mcp_dispatch.rs:4529`：`synthesis_and_recording_pause_listing_matches_dispatch`。**未執行**。
- CLI `crates/duduclaw-cli/src/mcp/working_state_mcp_tests.rs:260`：`paused_handoff_preserves_the_previous_durable_state`；`crates/duduclaw-cli/src/mcp/team_handoff_mcp_tests/part1.rs:582`：`parent_pause_refuses_ephemeral_packet_commit_and_resume_restores_it`；`crates/duduclaw-cli/src/mcp/tests/part1.rs:622`：`paused_create_task_writes_no_spec_or_signal`。**未執行**。
- Gateway `crates/duduclaw-gateway/src/discovery/tests_service.rs:882`：`refused_mcp_admission_creates_no_discovery_task_or_approval`。**未執行**。
- Gateway `crates/duduclaw-gateway/src/autonomous_runtime.rs:702`：`captured_pause_survives_detached_transport_and_resume`；`crates/duduclaw-gateway/src/cron_scheduler.rs:1689`：`manual_cron_reports_caller_pause_as_typed_refusal`。**未執行**。

既有 fixture 僅補真實 active caller／parent／org authority，或明確以 `with_background_caller(RecordActor::Operator(...), ...)` 表示 direct-handler 測試的人類操作者；沒有將正式環境的 reserved caller 當成 operator，也沒有刪掉原斷言。涵蓋 team_handoff、working_state、plan、create_agent、evolution、fork 測試。Gateway ephemeral 舊自由文字錯誤斷言改比對固定類別。

### 靜態複查與交回 reviewer

獨立 agent 靜態複查發現 legacy create／goals／discovery 遺漏、task wake event、cron late refusal、blocking writer cancellation 及 future 初始 frame 過大風險，均已處理。沒有執行指定 Daybreak 模型安全重審；一般靜態複查不等於安全驗收。`git diff --check` 通過，只表示 whitespace 檢查。

Reviewer 請優先編譯核對：lazy `AdmissionFuture` 的 Send／型別推導、`with_background_caller` 包住 dispatcher 後三個 JSON-RPC 提早返回的 Result 型別、`Snapshot` partial moves 與 typed pause capture、synthesis blocking closure 的 guard ownership，以及新增 fixtures。所有 build／test／原生平台／跨程序競態實機結果仍未知；本次只交付程式、未執行測試及文件，不宣稱已通過驗收。

### 第四輪修補驗證 2026-10-29：fork publication fixture

本次只修 `crates/duduclaw-cli/src/mcp_fork_exec.rs` 的測試及本段文件。先替 barrier 測試的失敗斷言加入完整 `selected`，實跑重現 **0 passed / 1 failed**，錯誤為 `Error: merge_or_select 遭拒：員工狀態無法讀取，不能建立或委派背景工作。請由真人從儀表板或 duduclaw tui 操作。`。原 fixture 有 `agents/a1/agent.toml`，但只有 `[fork]`；`autonomy_gate.rs:520–537` 必須讀到 `[agent] status`，故類別是 `agent_status_unreadable`，不是 `agent_missing` 或 lease timeout。

保留正式 admission：`mcp_fork.rs:568` 的 gate 保護 `:631–644` 的 parent promotion、resolution 提交與 retained 清理；manual 是明確指定 branch，並不代表呼叫者是真人。`mcp/record_authz.rs:372–375` 已對已驗證的 Operator scope 豁免，無 task-local 的直接呼叫按員工處理。barrier 在 handler 前已通過，晚到 publisher 在 selection 回覆後才執行（`mcp_fork_exec.rs:1159–1169`），本次沒有鎖互等證據。

Fixture 補 `[agent] status = "active"`（`:1102`），保留原競態斷言及錯誤診斷。新增 `paused_manual_selection_preserves_publication_state_and_resume_allows_it`（`:1180`）：確認 `agent_pause` 與一致拒絕文字、fork／branch 完整資料列、parent／branch workspace 路徑、parent／retained 檔案內容皆不變；resume 後確認成功 promotion、resolution 與 retained 清除。兄弟測試先前會過，是因為只呼叫 `publish_resolution`，沒有走 merge/select admission。

所有 Cargo 指令逐一執行，固定前綴 `CARGO_INCREMENTAL=0 cargo test -j 4 -p duduclaw-cli --lib`；下表列出完整 filter 與本次結果。

| Filter | 階段 | Passed | Failed |
|---|---|---:|---:|
| `mcp_fork_exec::tests::barrier_late_background_publication_cannot_reopen_manual_resolution` | 修補前重現 | 0 | 1 |
| `mcp_fork_exec::tests::barrier_late_background_publication_cannot_reopen_manual_resolution` | 修補後 | 1 | 0 |
| `mcp_fork` | 修補後，包含新增測試 | 63 | 0 |
| `record_authz` | 修補後 | 26 | 0 |
| `tools_list` | 修補後 | 19 | 0 |

獨立唯讀程式碼複查未發現阻擋問題；`git diff --check` 通過。這是指定範圍的本機測試，不代表全 workspace、Daybreak 安全重審或原生平台驗收。沒有執行 cargo fmt、commit／push／stash／切換分支或建立／移除 worktree。原始輸出保存於 `/tmp/autonomy-pause-fixture-review/{reproduce,failing-fixed,mcp_fork,record_authz,tools_list}.log`。

## Daybreak 第五輪修補 2026-10-29，static-only

本輪處理 detached fork claim／publication admission、錄製蒸餾 rejected draft 寫入，以及純讀取 `plan_start` 誤分類。沿用 `feat/autonomy-pause-20261028` 與既有未提交修改。**未執行 cargo、rustc、clippy、任何 build／test、cargo fmt 或其他 formatter；沒有 commit、push、stash、切換分支、建立或移除 worktree。** 第四輪的實跑結果不代表本輪已通過。

### 逐 finding 修改

以下路徑相對於 repo root，行號為本輪交付位置。

| Finding | 檔案與位置 | 修改 |
|---|---|---|
| MEDIUM：detached claim 未重新 admission | `crates/duduclaw-cli/src/mcp_fork_exec.rs:453,487,665` | 將 fork row 的持久化 `agent_id` 傳入 detached executor；不依賴已結束 handler 的 task-local。blocking worker 先取得 home publication lock，再以 owner 取得新的 admission，僅持有到 `claim_pending_branches` commit。拒絕記 error log，所有 pending rows 與 parent workspace registry 維持原狀，該 executor 結束。 |
| MEDIUM：detached publication 未重新 admission | `crates/duduclaw-cli/src/mcp_fork_exec.rs:586,678,713,748,776` | `prepare_branches(...).await` 完成後才進 publication retry。blocking writer 依既有順序先取得 home lock、canonical parent lock，再取得全新的 owner admission；guard 在同一 blocking closure 內建立並持有，涵蓋 recovery preparation、parent promotion、branch／resolution 寫入與 retained cleanup。拒絕不消耗 `PreparedFork`，不寫 recovery、branch 或 resolution；釋放所有鎖後每秒重新嘗試。lock／DB fault 保留原 recovery journal、私人來源與 publication_failed 診斷。 |
| MEDIUM 的鎖等待配套 | `crates/duduclaw-fork/src/lib.rs:408`；`crates/duduclaw-fork/src/publication.rs:33` | 抽出 `PreparedFork::publish_under_parent_lock`，供已持有 parent capability 的 detached writer 使用，避免持 admission 等待發布鎖。入口核對 capability 的 canonical parent 與既有 inode／Windows identity。原 `publish_with_observer` 委派同一實作，manual `merge_or_select` 沒有改動。 |
| MEDIUM 的 blocking worker 配套 | `crates/duduclaw-core/src/autonomy_gate.rs:638,670` | 同步 `admit_blocking` 與 async `admit` 共用 `admit_scope_blocking`，保留同一 effective identity resolver、pause／lifecycle 檢查、30 秒 lease 等待與 holder tracking。blocking writer 不再以 `Handle::block_on` 間接排入第二個 blocking worker，避免 pool 飽和互等。拒絕使用既有 `background_caller_refusal`。 |
| LOW：rejected draft 在 admission 前寫入 | `crates/duduclaw-cli/src/mcp_recording_distill.rs:793,930` | security-scan rejection 先呼叫 `write_rejected_draft`，重新取得 caller admission 後才同步寫入 `skill.rejected.md` 與設權限。拒絕沿用 `background_caller_refusal` 文字，不建立或覆寫檔案；短同步寫入不會因 future 取消而讓 writer 脫離 lease。 |
| LOW：`plan_start` 誤分類 | `crates/duduclaw-cli/src/mcp/record_authz.rs:403`；`crates/duduclaw-cli/src/mcp/record_authz_tests.rs:1238`；`crates/duduclaw-cli/src/mcp/tools_list_budget_tests.rs:420` | 實讀 `mcp_planner.rs:78`：只驗證輸入、讀 planner 設定並回傳 scaffold，沒有狀態寫入或啟動工作。從 `creates_background_work` 移除，補暫停時仍列出且能回傳 scaffold 的斷言；`tasks_create` 仍分類並受 admission 保護。同步更新 CHANGELOG `[Unreleased]` Fixed。 |

### Fork 拒絕與恢復的選擇

claim 與 publication 是兩次獨立 admission。中間的 branch process／model／judge 等待不持有這兩把 lease；各分支原有 spawn admission 保留。兩次 mutation 都直接使用持久化 owner，不讓 detached task 遺失 `BACKGROUND_CALLER` 後變成 operator。

publication 拒絕時保留原本 `PreparedFork` 擁有的私人 TempDir overlays；這些目錄尚未移入 `fork_ws`，因此 retained TTL GC 不會看見。選擇不把拒絕寫成 `publication_failed`，因為那會改 branch rows／aggregate spend，且暫停後不應另做 recovery copy 或 journal 寫入。背景 task 釋鎖後等待恢復，不重新 claim、不重跑模型與 judge；admission 成功後才執行既有 recovery staging。home／parent publication locks 與 `current.resolved` 檢查讓正常的同程序 resume 只發布一次；人工已完成的選擇不會被晚到 publisher 覆寫。

claim 拒絕會結束 executor，留下 pending rows；本輪沒有新增 pending fork 的全域自動重排程器。publication 拒絕則由原 executor 保留結果與重試，兩者不是同一種恢復行為。

### 測試撰寫（全部未執行）

| 測試 | 位置與斷言 |
|---|---|
| `pause_after_handler_admission_before_detached_claim_preserves_pending_rows` | `crates/duduclaw-cli/src/mcp_fork_exec.rs:1484`：真實 `handle_fork_run` 返回後，以 `#[cfg(test)]` task-local checkpoint 卡在 detached claim 前，成功 pause 後放行；比對完整 fork／branch rows、parent registry 不變、fake spawn 次數為零。設定 `account_loading.inherit_host_credentials = false`，不讀真實帳號或啟動真實 backend。 |
| `pause_after_prepare_defers_publication_and_resume_publishes_exactly_once` | `crates/duduclaw-cli/src/mcp_fork_exec.rs:1527`：checkpoint 卡在 prepare 完成後，pause 並觀察實際 admission refusal；parent 兩個檔案、fork／branch rows、workspace pointer 不變且無 recovery 目錄。resume 後發布原結果、spawn 只一次、history 只一筆；重複 executor 不能覆蓋發布後的人工作業。deferred 時還能套用 global pause，驗證沒有殘留 lease。 |
| 單一 blocking worker fixture | `crates/duduclaw-cli/src/mcp_fork_exec.rs:1460`：checkpoint worker runtime 設 `max_blocking_threads(1)`，上述發布恢復測試也涵蓋避免 nested blocking admission 互等。所有 checkpoint 僅存在於 `#[cfg(test)]`，fixtures 保留 `agents/a1/agent.toml` 的 `[agent] status = "active"`。 |
| `prepared_publication_rejects_another_parent_lock` | `crates/duduclaw-fork/src/lib.rs:510`：錯誤 parent capability 在任何 observer／resolution callback 前拒絕，兩個 parent 都不寫入 branch 檔案。 |
| `paused_rejected_draft_writes_nothing_and_resume_preserves_it` | `crates/duduclaw-cli/src/mcp_recording_distill.rs:950`：比對標準 pause refusal；沒有舊檔時不建立、有舊檔時不覆寫；resume 後保留 rejected draft。 |
| 既有 classifier／listing 測試擴充 | `crates/duduclaw-cli/src/mcp/record_authz_tests.rs:1232`；`crates/duduclaw-cli/src/mcp/tools_list_budget_tests.rs:410`：`plan_start` 不再分類為背景工作，paused listing 保留它且 handler 回傳 scaffold；`tasks_create` 仍隱藏且維持背景工作分類。 |

### 限制與 reviewer 優先檢查

- **指定已知限制，未修**：`merge_or_select`（`crates/duduclaw-cli/src/mcp_fork.rs:568` 起）仍持有 admission 等待 fork-resolution／parent-publication locks，並跨越 `crates/duduclaw-fork/src/copy_policy.rs:254` 起無總時限的遞迴複製。巨大 workspace 或長鎖等待可能讓 pause 在 30 秒逾時；暫停未套用，操作者收到錯誤後可重試，並非成功暫停後被繞過。完整修正需要另做 manual 路徑的 lock-order redesign，本輪沒有修改該路徑。
- detached 路徑已將發布鎖等待移到 admission 前，但實際 recovery 複製、fsync、promotion 仍不能中途放掉 lease；巨大 workspace 的檔案 I/O 也可能讓 pause 逾時。本輪不宣稱所有發布都能在 30 秒內結束。
- **publication resume 僅保證原背景 task／程序仍存活的情況**。拒絕期間沒有持久化 recovery journal，程序退出、crash 或 task 取消後沒有新增自動重建 `PreparedFork` 的路徑；不能把本輪稱為跨重啟 exactly-once。恢復後真正進入 publication，才使用既有 durable recovery 設計；copy／DB 故障仍可能留下 truthful partial marker，並非跨檔案系統與 DB 的原子交易。
- Reviewer 請先編譯 core 的同步／async admission 共用型別、`PublicationAttempt` 跨 `spawn_blocking` 的 Send／ownership、`PreparedFork` 新 capability API 與 closure 回傳推導、`cfg(test)` task-local hook 和單一 blocking worker fixtures；再跑 core autonomy、CLI `mcp_fork`／`mcp_recording_distill`／`record_authz`／`tools_list`、fork library 的 publication 與新 capability 測試。既有 lock-fault、DB-fault、late manual selection、terminated winner 與 Windows recovery 測試都應保留驗證。
- 獨立一般程式碼靜態複查指出 nested blocking pool 互等後，已抽共用同步 admission 並再次複查，後續未見新的阻擋問題；這不是 Daybreak 模型安全重審。`git diff --check` 通過，只代表 whitespace 檢查。編譯、測試、Windows／Linux 原生與端到端驗收均未執行。


### publication 永久拒絕與退避補修（2026-10-29）

本補修取代上文「所有 publication 拒絕都每秒重試」的行為，僅修改 `mcp_fork_exec.rs` 與本紀錄。保留原始 `PauseInfo.reason`，以第一個冒號前的完整 code 分類，不比對中文訊息：`agent_missing`、`invalid_agent_id`、`ephemeral_parent_invalid` 終止；`agent_pause`、`global_pause`、`admission_wait_timeout`、`admission_unavailable`、`state_unreadable`、`agent_status_unreadable`、`agent_not_operational` 與未知原因維持拒絕發布，依 1、2、4、8、16、32、60 秒退避並維持 60 秒上限，原因改變也不重設。sleep 前已釋放所有 admission／publication locks。

永久拒絕沿用 prepare 失敗的 `set_all_branch_states(..., "failed")` CAS，只將 pending／running branches 改成 failed，保留 terminated 與已完成的人工選擇；fork resolution 與 parent 不變。丟棄 `PreparedFork` 讓私人 TempDirs 自動清除，並在 home lock 內呼叫既有 `discard_retained_fork` 清除未 resolved fork 的 retained workspace／DB 路徑。這次沒有開始發布，因此不沿用會保留來源與建立 recovery journal 的 publication I/O 故障路徑。executor 結束並記錄一次終止錯誤；若 lock／DB 清理本身失敗，錯誤會被回報，不能宣稱 failed 狀態已成功落盤。

新增 code-prefix 分類與純函式 backoff 測試，以及 prepare checkpoint 後刪除 owner 目錄的案例：驗證 worker 結束、branch failed、parent／完整 fork row 不變、私人與 retained workspace 清除、無 recovery／history 寫入。既有 pause→resume exactly-once 測試保留。一般程式碼獨立靜態複查未見阻擋問題；不是 Daybreak 安全重審。指定三組測試皆依序執行並通過（同時只跑一個 cargo，全部設 `CARGO_INCREMENTAL=0`、`-j 4`）；首次 CLI 編譯發現 `Box<PauseInfo>` 型別不符，已解包修正，該次沒有測試執行。不執行 cargo fmt、commit、push、stash 或切換分支。


| 指令 | 通過／失敗／忽略 | Log |
|---|---|---|
| `CARGO_INCREMENTAL=0 cargo test -j 4 -p duduclaw-cli --lib mcp_fork` | 68／0／0 | `/tmp/autonomy-publication-cli-fork.log` |
| `CARGO_INCREMENTAL=0 cargo test -j 4 -p duduclaw-fork --lib` | 112／0／0 | `/tmp/autonomy-publication-fork-lib.log` |
| `CARGO_INCREMENTAL=0 cargo test -j 4 -p duduclaw-cli --lib record_authz` | 26／0／0 | `/tmp/autonomy-publication-record-authz.log` |

`git diff --check` 通過。首次編譯失敗紀錄保留於 `/tmp/autonomy-publication-cli-fork-compile-failure.log`；修正後上述三組均 exit 0。未執行 Windows／Linux 原生、真實 gateway 或跨重啟驗收。

## Daybreak 第六輪修補 2026-10-29，static-only

本輪依既定重新設計移除 deferred automatic publication，取代第五輪及其永久拒絕／退避補修。沿用 `feat/autonomy-pause-20261028` 與既有未提交修改；沒有執行 cargo、rustc、clippy、build、test、cargo fmt 或其他 formatter，也沒有 commit、push、stash、切換分支、建立或移除 worktree。下列測試只完成撰寫，**未執行**；前輪通過紀錄不能當作本輪驗證。

### 行為與沿用路徑

- publication 仍先取得 home publication lock、canonical parent lock，再於同一 blocking closure 同步取得 owner admission。成功時，guard 持有到 recovery、promotion、資料庫落盤與 cleanup 完成；原成功流程保留。
- admission 被任何原因拒絕，都以 `PreparedFork::require_manual_resolution` 改為既有 `MergeDecision { winner: None, needs_confirmation: true, reason }`，且 `promoted = false`。`reason` 固定為 `autonomy_refused:<code>`，只取第一個冒號前的 code；不合 code 格式時使用 `unknown`，不帶路徑或錯誤全文。
- 沿用 `publish_prepared_resolution` → `PreparedFork::publish_under_parent_lock` → `publish_resolution_locked`：寫入原 branch results／outputs／花費與 retained workspace 路徑。沒有 winner 就不會呼叫 parent promotion，分支工作區移入 `retained_root(home)/<fork>/<branch>`，背景 task 隨即結束。保留資料由現有 retained TTL GC 管理；不再由等待恢復的活 task 持有私人 TempDirs。
- 刪除 `PublicationAttempt`、publication retry loop、backoff、永久／暫時拒絕分類與永久拒絕的破壞性清理。員工刪除、停用、org.toml I/O 失敗與一般暫停都不再決定是否銷毀結果。鎖／檔案／DB 故障仍沿用原有 `publication_failed` recovery 與錯誤回報，這不是自動重試；同時發生拒絕時先清除自動 winner，不可能進入 parent promotion。
- 原 store 沒有保存 `MergeDecision` 的原因／確認旗標；新增 nullable `resolution_reason`、`needs_confirmation` 欄位及 `set_resolution_decision`／`resolution_decision`，保存同一個既有 decision，沒有新增 fork state。`inspect_branches` 可讀到 reason 與 needs_confirmation；舊列為 null，人工完成選擇後由原 `set_resolution` 清除舊 decision metadata。
- claim 拒絕使用既有取消狀態 **`terminated`**（`SpawnOutcome::Cancelled` 對應的 branch state），而非假裝執行失敗。`cancel_pending_claim` 在 home lock 下以 SQLite IMMEDIATE 交易只取消這次請求的 pending rows；不覆蓋已取消、執行中或已 resolved 的資料。所有分支均 terminated 時，fork 記 `resolved=true`、`winner=None`、`promoted=false`、`needs_confirmation=false` 與拒絕 reason，表示「未執行即結束」。不 claim、不複製 parent、不選帳號、不呼叫模型或 spawn；資料庫錯誤回傳並由 executor 記錄。
- `discard_retained_fork` 改回傳 `std::io::Result<()>`，檔案刪除失敗不清掉 DB 路徑，`clear_fork_workspaces` 失敗也不吞掉。背景 publication 傳播錯誤；manual handler 原有呼叫點記錄 cleanup 警告，選擇／admission／鎖定與 resolved 檢查流程未改。

### 為什麼移除自動恢復發布

兩個 MEDIUM 都來自長時間 deferred window：一是暫停期間 parent 已有人修改，恢復後舊結果自動覆蓋新內容；二是 owner 只有可重用的 agent id，刪除後以相同 id 重建會讓新員工的 admission 放行舊 fork。現在拒絕即結束背景發布，只保存待人工選擇的結果，resume 或重建同 id 都不會自動套用。沒有實作 base manifest 或 lifecycle UUID；手動選擇仍沿用既有 owner-id 規則，不能宣稱同 id 重建後具有新的生命週期隔離。

**明確例外：允許 bookkeeping after pause。** 暫停回覆成功後，仍可將已完成結果、拒絕原因、retained 路徑及 claim 的取消終態落盤，並沿用 staging 所需的 recovery／清理與完成紀錄。這些是已開始工作的收尾記帳，不允許啟動新 branch 或寫入 parent workspace；因此本輪不再承諾「pause 後所有 branch／resolution rows 完全不變」。

**既有短視窗限制仍在：** branches 執行期間若 parent 被其他人修改，之後 admission 成功的正常自動發布或明確人工選擇仍會由 `copy_tree` 覆蓋同名檔案。這是既有 fork 設計，本輪沒有新增也沒有修復；沒有 base comparison，也不是跨檔案與 DB 的原子交易。manual `merge_or_select` 持 admission 等待鎖及複製的 30 秒 pause-timeout liveness 限制維持第五輪所記錄的現況。

### 測試撰寫（未執行）

| 測試 | 靜態新增／更新的斷言 |
|---|---|
| `pause_after_prepare_stages_manual_resolution_and_owner_selects_once_after_resume` | prepare checkpoint 後 pause 已成功，parent 再有新編輯；worker 有限時間結束，parent bytes 不變，decision 為 needs_confirmation／正確 reason，results／花費落盤，retained files 完整且私人 TempDir 釋放。暫停時人工選擇拒絕；resume 與重複 executor 不會自動發布；owner 明確選擇發布一次，第二次選擇拒絕且不覆蓋後續編輯。 |
| `deleted_owner_after_prepare_stages_manual_resolution_without_destroying_results` | 同一 checkpoint 刪除 owner，結果仍保留待選且 parent 不變；重建同 id 與重複 executor 不會自動發布，後續明確選擇沿用原授權規則。 |
| `pause_after_handler_admission_before_detached_claim_terminates_without_running` | 真 handler 留下 pending rows 後在 claim checkpoint 暫停；worker 結束、所有請求分支 terminated、fork resolved 但未 promoted／無 winner、reason 正確、spawn 次數零且 parent registry 不變。 |
| `autonomy_refusal_reason_contains_only_the_code` | 原永久／暫時各 code 皆只保存 prefix，不保存錯誤全文；格式不符回 unknown。取代舊分類與 backoff 測試。 |
| store：`manual_decision_survives_reopen_and_selection_clears_it` | metadata 跨 connection reopen 保留，原 manual selection setter 清除舊 decision。 |
| store：`refused_claim_cancels_pending_rows_once_without_overwriting_cancellation` | 不改既有取消列，拒絕 claim 不重開 fork，後續重複 claim／不同拒絕原因不能覆寫終態。 |
| store：`refused_claim_transaction_rolls_back_when_resolution_write_fails` | 注入 resolution 寫入錯誤，分支取消與 fork 狀態一起 rollback，錯誤回傳。 |
| `discard_retained_fork_reports_database_cleanup_failure` | 注入 clear workspace path 的 DB 錯誤，helper 回 Err，沒有靜默宣稱清理成功。 |

保留原 `background_publication_stages_results_and_paths_before_manual_selection`、`paused_manual_selection_preserves_publication_state_and_resume_allows_it`、late manual selection、terminated winner、publication lock／DB fault 等仍有意義的回歸測試。checkpoint worker 保留單一 blocking thread；fixtures 使用 `agents/a1/agent.toml` 的 `[agent] status = "active"`。

Reviewer 優先檢查 `spawn_blocking` ownership／Send、`Option<AdmissionGuard>` 的作用域、`PreparedFork` 轉 manual 後的共享 retention 路徑、nullable metadata migration／rusqlite 型別推導及 claim 交易取消條件。編譯與全部測試留給持有 build slot 的 reviewer；本輪不宣稱編譯、測試或 Daybreak 安全重審通過。

### 靜態收尾

一般程式碼獨立靜態複查未見新的阻擋問題；複查所指出的 fake spawner 預期已修正為實際的 `automatic answer`／0.01。這不是 Daybreak 安全重審。`git diff --check` 通過，僅代表 whitespace 檢查。

故障例外需分開看：publication lock 先失敗且 admission 同時拒絕時，仍沿用 `preserve_unpublished_failure` 的 `publication_failed`／`fork_recovery` 保存來源；不保證進入 needs_confirmation、`fork_ws` TTL 或保存拒絕 reason。正常取得鎖後的任意 admission 拒絕才會完成上述 manual staging。既有 recovery 檔案／DB 故障也可能使 staging 不能落盤；錯誤會記錄，不能宣稱結果已成功留存。

## Daybreak 第七輪修補 2026-10-29，static-only

本輪沿用 `feat/autonomy-pause-20261028` 與既有未提交修改，只處理第七輪交辦及為避免新增鎖問題所需的同類修補。未執行 cargo、rustc、clippy、build、test、cargo fmt 或任何 formatter，未觸碰 `target/`，未 commit、push、stash、切換分支或建立／移除 worktree。下列測試均為**已撰寫、未執行**；前輪或其他 session 的測試結果不能當成本輪驗收。

### Finding 對照

| Finding | 修補與範圍 | 靜態結果／限制 |
|---|---|---|
| MEDIUM-1：同 ID 重建員工可選取舊拒絕 fork | `mcp_fork.rs:612` 在既有 resolution lock、fresh resolved 檢查旁讀 `resolution_decision`。`needs_confirmation` 且原因以 `autonomy_refused:` 開頭時回覆需真人到儀表板檢視（`fork.resolve`），早於 retention sweep、promotion 或 store mutation。 | 恢復與同 ID 重建都不能由 MCP 發布；普通 manual fork 不受此條件影響。gateway 仍可人工選取；未改授權。 |
| MEDIUM-2：生命週期變更未等待 admission | `autonomy_gate.rs:621` 提供同步、async 取得及 closure 包裝的 exclusive barrier，使用與 pause/resume 相同的 home lock、30 秒上限。共用 profile writer 在 fresh 檔案鎖內比對狀態，先 drain，再做 history、replace；移除入口在副作用前取得屏障。 | 逾時不寫入此次生命週期變更。既有 handoff 在最後 auto-archive 前已搬移的任務／知識不回滾，仍回覆 PARTIAL。等待的是 admission 所涵蓋的寫入／handoff，不是所有已執行程序的結束。 |
| LOW：人工選取清除拒絕原因，失去稽核 | `fork_migrate_rpc.rs:199` 在同鎖內先取得原拒絕代碼，成功 `set_resolution` 後以 `try_append_audit_event` 寫一筆 `fork_autonomy_refusal_resolved`，包含 `fork_id`、`branch_id`、`prior_reason`、`actor_id`、`channel: dashboard`。`dispatch_org.rs:643` 僅傳遞現有 caller ID。 | 原因只保留受限代碼，不記路徑或內容。重複 resolve 已 resolved 時拒絕，不多寫一筆。audit I/O 失敗會明示「已發布、稽核失敗、請勿重試發布」；本輪沒有新增跨 DB／JSONL 交易或補記機制。 |

`ForkStore::set_resolution` 的 production resolver 盤點只有 MCP 與 dashboard：前者現在拒絕 autonomy-refused 決策，後者清除決策前保存原因供稽核。因此本輪不改 store API；其餘 set_resolution 呼叫為測試。

### 生命週期入口完整盤點

| 入口／檔案 | 屏障狀態 |
|---|---|
| `handlers/agents_ops.rs:116` agents.pause → `update_agent_status:203` | 已透過共用 `agent_files/writer.rs:128`。resume 回 active 不需 lifecycle barrier。 |
| `handlers/agents_archive.rs:77` offboard，涵蓋 agents.archive／agents.remove | 已透過共用 writer，status 與 freeze 欄位同次提交。 |
| `handlers/agents_archive.rs:291` handoff 最後 auto-archive | 已透過 offboard／共用 writer；先前 handoff 副作用與最後 archive 的結果分開回報。 |
| `handlers/agents_update.rs:348` status paused／terminated | 已透過共用 writer。 |
| `agent_files/flow.rs:459` 原始 profile 儲存、版本還原 | 已透過共用 writer，對真正將寫入的 artifact 判斷。 |
| `mcp/agent_update.rs:613` agent_update | 已透過 guarded writer。明確 stop 請求即使原狀態相同仍強制 exclusive；重新套用 requested status 到 fresh TOML，避免舊 snapshot 略過 status。caller 重查與 org_store upsert 在 drain 後，timeout 不先改 reports_to。 |
| `cli/src/lib.rs:10885` agent pause／resume | 改走 blocking worker 中的共用 profile writer；pause 有屏障，resume 無額外 lifecycle 屏障。 |
| `mcp/agent_admin.rs:44` agent_remove → `_trash` | blocking worker 從讀取／role 檢查、rename 到 org 清除都持 exclusive；取消 MCP future 不提前釋放。 |
| `ephemeral/gc.rs:28` TTL／completed GC | 掃描既有 namespace 時持 exclusive；逾時不刪除，記錄延後。 |
| `ephemeral/role_member.rs:163` finish_role_member | marker、containment 重查、移除與 org 清除都在 exclusive 內。 |
| `expert_admin.rs:371` CLI／dashboard 專家包 remove | blocking worker 整次持 exclusive；逾時不先刪其他 pack 資產。 |
| `cli/expert/reimport/retire.rs:86` 專家包重新匯入，舊員工移入備份 | 首次 journal 寫入前持 exclusive。失敗先釋鎖再進 guarded undo，避免重入；rollback 失敗保留 journal。 |
| `expert_replace_journal.rs:222` rollback／啟動 recovery 移走 failed-new 員工 | undo 先取 exclusive，失敗列入 failures，不搬移資料。 |
| `cli/portability.rs:97` 整個 home 匯入／備份還原 | agents 備份到 unpack 全程持 exclusive。封存內 admission lock 不還原，避免換 inode 繞過現有 lease。 |
| `cli/src/lib.rs:7240` onboarding reset | 第一筆 reset 副作用前取 exclusive；實際刪除由持 lease 的 blocking worker 完成，刪除失敗回覆錯誤。 |

不需 lifecycle barrier 的項目：轉回 active 的 resume／unarchive；只變更其他欄位的設定編輯；CLI freeze／unfreeze（沒有 status 轉移）；registry scan（讀取／產生 resolved artifact）；preset sanitize（移除 preset 的整個 `[agent]`，不提供另一個 status setter）。`agents_create`、`ephemeral/scaffold`、MCP spawn 與 queued replay 尚未成功派發的新 scaffold 建立失敗清除，保留原 rollback；它們不是既有 operational 員工的下線入口，且部分建立流程本來就持 shared，不能再巢狀取 exclusive。專家包 skill staging／archive 清除不是 live agent 目錄。

### 鎖與取消行為的補修

- 共用 writer 的順序是 profile file lock → admission → guard 重查 → history／replace。MCP `agent_update` 的一般背景設定編輯及 `evolution_toggle` 也改成在 file lock 內取得 shared admission，持到 writer 返回後釋放，避免原先 shared → file lock 與 lifecycle writer 互等。
- `agent_update` 明確 stop 使用 exclusive，而非先 shared 再升級；同狀態 stop 加 heartbeat／reports_to 的混合請求也維持此規則。
- MCP remove、pack remove、CLI status write 由 blocking worker 持有 lease；CLI reset 把 lease 移交實際刪除 worker。請求取消不會讓已啟動的移除工作先失去屏障。
- `finish_role_member`、`undo` 與既有 profile writer 沿用 `run_blocking`：多執行緒 Tokio 使用 block_in_place；current-thread runtime 仍可能同步等待最多 30 秒。這是待 runtime 驗證的限制。

### 測試已撰寫，未執行

| 檔案 | 測試內容 |
|---|---|
| `core/src/autonomy_gate/tests.rs:635` | shared admission 下 paused／archived／deleted／terminated／remove 不得套用；release 後成功，下一次 admission 拒絕。 |
| 同檔 `:666`、`:684` | 短 timeout 注入保留 agent.toml 原位元組；resume 與非 status 編輯不要求 lifecycle barrier。 |
| `gateway/src/agent_files/external.rs:204` | 真正共用 writer 對四種非 operational 狀態等待；等待期間不寫 history、audit、config，release 後完成。 |
| 同檔 `:238` | 等待期間撤銷 caller，drain 後 guard 拒絕，不寫 config/history。 |
| `cli/src/mcp/tests/part4.rs:597` | active／已 paused 的目標收到 status=paused＋heartbeat＋reports_to；guard 未 release 前 TOML 與 org 不變，release 後成功。 |
| `cli/src/mcp_fork_exec.rs:1547` 附近 | owner resume／同 ID 重建仍拒絕，fork、branch、decision、retained workspace 與 parent 檔案不變；既有普通 manual 成功案例保留。 |
| `gateway/src/handlers/fork_migrate_rpc.rs:453` | dashboard 對 agent_pause／agent_missing 原因成功 promotion，留下正確 actor／原原因且只一筆事件；普通 manual 不產生該事件。 |
| 同檔 `:489` | 發布後 audit I/O 故障，明確回覆已套用，避免重試。 |
| `gateway/src/expert_admin.rs:748` | held shared admission 下 pack 的員工與 record 保留，release 後移除。 |
| `cli/src/portability.rs:260` | 還原員工與備份舊員工時保留 live admission lock，不能由 archive 覆蓋。 |

### 另開修復分支的後續項目（本輪未改）

1. `dispatch_org.rs:641`／`fork_migrate_rpc.rs:190`：`fork.resolve` 缺少 live per-agent Operator access check，屬既有問題。這輪只傳 actor 給 audit，不修改授權。
2. `fork_migrate_rpc.rs` resolution 成功後 cleanup DB 的 `let _ = store.clear_fork_workspace_paths(...)` 忽略錯誤，屬既有問題，保留另修。

### 靜態驗證與編譯風險

一般 code-reviewer 獨立複查指出的同狀態 stop 無 lease、取消時提前釋鎖、鎖順序反轉，均已修補並再次靜態複查，未再指出阻擋問題。這不是 Daybreak 安全重審。`git diff --check` 通過，只代表 whitespace 檢查。

新 helper 的 generic closure、borrow／Send、blocking worker ownership、fork handler actor 參數與測試 fixture 只經靜態閱讀；未經 Rust 型別檢查、平台編譯或執行測試。鎖在不同程序／Windows 的實際行為、30 秒上限下的負載表現與 audit 故障路徑，仍待審查者執行驗證。編譯或測試成功不可由本紀錄推定。

本輪實際修改來源檔：core `autonomy_gate.rs`、`autonomy_gate/tests.rs`；CLI `lib.rs`、`mcp_fork.rs`、`mcp_fork_exec.rs`、`mcp/{agent_admin,agent_update,evolution,record_authz}.rs`、`mcp/tests/part4.rs`、`expert/reimport/retire.rs`、`portability.rs`；gateway `agent_files/{writer,external}.rs`、`handlers/{dispatch_org,fork_migrate_rpc}.rs`、`ephemeral/{gc,role_member}.rs`、`expert_admin.rs`、`expert_replace_journal.rs`。文件同步：CHANGELOG、本紀錄、三語 continuous-responsibilities guide。

### 本輪修改檔案／定位行號

以下為第七輪修改定位，不代表整份既有 uncommitted diff 都由本輪產生。

| 檔案（相對 repo root） | 行號 |
|---|---|
| `crates/duduclaw-core/src/autonomy_gate.rs` | 621（barrier helpers）、646（fresh status 判斷） |
| `crates/duduclaw-core/src/autonomy_gate/tests.rs` | 635、666、684 |
| `crates/duduclaw-cli/src/mcp_fork.rs` | 612（拒絕） |
| `crates/duduclaw-cli/src/mcp_fork_exec.rs` | 1547、1638（恢復／同 ID 重建的拒絕斷言） |
| `crates/duduclaw-cli/src/mcp/agent_update.rs` | 591、613（lease 選擇、fresh status、guard／org write） |
| `crates/duduclaw-cli/src/mcp/agent_admin.rs` | 32、44（blocking remove） |
| `crates/duduclaw-cli/src/mcp/evolution.rs` | 155（file lock 內 shared admission） |
| `crates/duduclaw-cli/src/mcp/record_authz.rs` | 380（blocking identity admission） |
| `crates/duduclaw-cli/src/mcp/tests/part4.rs` | 597（混合 stop 回歸） |
| `crates/duduclaw-cli/src/lib.rs` | 7240、7273（reset）、10885（agent status） |
| `crates/duduclaw-cli/src/expert/reimport/retire.rs` | 86、106、129 |
| `crates/duduclaw-cli/src/portability.rs` | 105、131、260 |
| `crates/duduclaw-gateway/src/agent_files/writer.rs` | 124、128、163 |
| `crates/duduclaw-gateway/src/agent_files/external.rs` | 63、104、204、238 |
| `crates/duduclaw-gateway/src/handlers/fork_migrate_rpc.rs` | 161、199、236、453、489 |
| `crates/duduclaw-gateway/src/handlers/dispatch_org.rs` | 643（actor 傳遞，授權未改） |
| `crates/duduclaw-gateway/src/ephemeral/gc.rs` | 28 |
| `crates/duduclaw-gateway/src/ephemeral/role_member.rs` | 163 |
| `crates/duduclaw-gateway/src/expert_admin.rs` | 371、748 |
| `crates/duduclaw-gateway/src/expert_replace_journal.rs` | 222 |
| `CHANGELOG.md` | 18（延伸自主暫停項目） |
| `docs/guides/continuous-responsibilities.md` | 347 |
| `docs/guides/zh-TW/continuous-responsibilities.md` | 347 |
| `docs/guides/ja-JP/continuous-responsibilities.md` | 347 |
| `wiki/impl/autonomy-pause-20261028.md` | 754（本輪紀錄） |

## 第七輪修補驗證（2026-10-29）

根因是測試 fixture 錯誤（題述 b），不是 production 少取 lifecycle barrier。原測試先重現 0 通過／1 失敗；保留同一請求、加入回傳值診斷後再現相同失敗：`writer = Ok(Ok(... isError: true ...))`，內容為 `sales-lead` 不得將員工掛到 `ceo`。`mcp/agent_update.rs:364` 的 placement gate 在 `:613` guarded writer 前拒絕；writer `agent_files/writer.rs:163` 仍以強制 stop 旗標取得 `autonomy_gate.rs:621` exclusive barrier。同一 home 的 shared lease 一直持有至明確 drop；另開 blocking worker 裡的 current-thread runtime 不會阻塞外層 timeout。

`mcp/tests/part4.rs:618` 改用合法且確實改變從屬的 `sales-rep2`，保留 active／已 paused 兩種情況、100 ms 必須仍等待、釋放前 TOML／org 不變；失敗訊息印出實際回傳值，釋放後另驗證 TOML 與 org store 均指向新主管。另只修指定三項 clippy：`agent_update.rs:637` 合併 if、`mcp_fork_exec.rs:676` 保留底層 `Box<PauseInfo>`、fork `lib.rs:452` 用 `?` 傳遞原錯誤。未改 `mcp_dispatch.rs` 的既有警告。

以下命令均逐一執行，前綴為 `CARGO_INCREMENTAL=0 cargo`（無同時執行的 Cargo）：

| 命令（接上述前綴） | 通過／失敗 |
|---|---|
| `test -j 4 -p duduclaw-cli --lib mcp::tests::part4::agent_update_stop_with_background_edits_always_drains_admissions` | 修正前 0／1；診斷版 0／1；修正後 1／0 |
| `test -j 4 -p duduclaw-cli --lib mcp::tests::part4` | 25／0 |
| `test -j 4 -p duduclaw-cli --lib agent_update` | 19／0 |
| `test -j 4 -p duduclaw-cli --lib mcp_fork` | 68／0 |
| `test -j 4 -p duduclaw-fork --lib` | 115／0（另含子程序測試 1／0，不重複加總） |
| `test -j 4 -p duduclaw-core --lib autonomy` | 33／0 |
| `test -j 4 -p duduclaw-gateway --lib agent_files::external::tests::lifecycle` | 2／0 |
| `clippy -j 4 -p duduclaw-cli -p duduclaw-fork --tests --no-deps -- -A clippy::approx_constant -A clippy::async_yields_async` | 修改前後均 exit 0；指定警告 3 → 0，其餘既有警告保留 |

本輪只修改以上四個 Rust 檔與本紀錄。`git diff --check` 通過；一般獨立靜態複查未見阻擋問題，非 Daybreak 安全重審。未重跑完整 workspace、未做跨平台或 live gateway 驗收。未 fmt、commit、push、stash、切換分支或建立／移除 worktree。診斷、各命令輸出與本輪程式碼差異 `task-only.patch` 保留於 `/tmp/autonomy-pause-round7-verification/`。

## Daybreak 第八輪修補

2026-10-29；本節記錄第八輪 do-not-ship findings 的實作與本地驗證，並非 Daybreak 重審放行。維持既有 `feat/autonomy-pause-20261028` 與未提交工作；不 fmt、commit、push、stash、切換分支或移除他人建立的 worktree。

### Findings 與修補

| Finding | 修補 |
|---|---|
| MEDIUM：preset bind/unbind、CLI freeze/unfreeze 的舊整檔 snapshot 可覆蓋後來完成的 archive/pause/resume | 改用 `agent_files::external::edit_agent_toml`。preset 只修改 `[preset]`；freeze 只修改 evolution/heartbeat enabled，均從 profile lock 內的 fresh TOML 開始，不設定 `[agent] status`。保留其他欄位及註解。unbind 在鏡像更新失敗時仍先留下已完成解除操作的稽核，再回報部分失敗。 |
| 同類盤點：expert、wizard 及舊 Python API | expert 共用 `patch_agent_toml` 與 wizard 範本修補也改走同一 API。permission migration 原本已持相同 profile lock、沒有 stale gap，本輪仍統一經 `external::write_file`。Python pause/resume 不再寫檔，使用固定 argv 呼叫 Rust CLI 的 agent pause/resume，帶相同 `DUDUCLAW_HOME`；啟動失敗不改檔，等待失敗回報結果未確認。 |
| LOW-a：raw profile save 在 barrier 前建立／裁剪備份 | 把 agent raw backup 移入共用 writer 現有 callback；callback 在 profile lock、expected hash CAS 與 lifecycle barrier 成功後、live replace 前執行。config/inference 原有備份流程不變。 |
| LOW-b：fork 已發布但 set_resolution 失敗沒有稽核 | 保留原 refusal code；錯誤分支嘗試寫 `fork_autonomy_refusal_published_unrecorded`，只含 fork_id、branch_id、prior_reason、actor_id、channel。回覆明示已發布、選擇結果未寫入、請勿重試；若 audit 也失敗則明示兩項記錄失敗。保留 retained workspace 與 refusal decision。 |

### 整檔寫回完整 sweep

搜尋涵蓋整個 workspace 的 `agent.toml`、`toml_edit::Document*`、`AgentConfig` serialize，以及候選檔案的 read/write/rename/atomic/copy 呼叫追蹤。以下 `cli/`、`gateway/`、`agent/`、`core/`、`fork/` 分別代表 `crates/duduclaw-*/src/`。

| 入口／檔案 | 結果與原因 |
|---|---|
| `cli/preset_cmd.rs` write_preset_mirror（bind） | 已修；只在 fresh table 修改 preset。 |
| `cli/preset_cmd.rs` clear_preset_mirror（unbind） | 已修；移除舊 write_agent_toml_atomic helper。 |
| `cli/lib.rs` cmd_agent_freeze → `cli/agent_freeze.rs` apply | 已修；freeze/unfreeze 共用鎖內欄位修改。 |
| `cli/expert/mod.rs` patch_agent_toml | 額外修正；涵蓋 install.rs 的 department／partial overlay、plugin.rs capabilities、agent_config/writer.rs capabilities、agent_config/slots.rs identity 五個呼叫點。overlay 如明確要求 status 變更，也會經共用 writer 判斷 barrier。 |
| `cli/wizard.rs` patch_agent_toml | 額外修正；雖是建立流程，範本已放在正式 agent 目錄，因此不再以舊 snapshot 寫回。 |
| `python/duduclaw/tools/agent_tools.py` _set_status | 額外修正；legacy 可 import API，未找到目前服務掛載；改呼叫 Rust CLI 共用 status writer，沒有 Python fallback 寫回。 |
| `gateway/handlers/agents_update_v168.rs` migrate_all_agent_permissions | 原本同 path lock 內 fresh read，只改 permissions，無 stale bypass；本輪仍統一到 shared writer。 |
| `cli/lib.rs` cmd_agent_set_status | 原已共用 external edit；pause/resume。 |
| `cli/mcp/agent_update.rs` | 原已共用 guarded writer；typed delta 套到 fresh table，stop status 重新套用。 |
| `cli/mcp/evolution.rs` | 原已共用 guarded writer；只套請求欄位。 |
| `gateway/channel_reply/mod.rs` update_agent_toml_edit | 原已共用 writer；鎖內 parse/mutate/render。 |
| `gateway/handlers/agents_ops.rs` | 原已共用 wrapper；status、一般設定、main demotion。 |
| `gateway/handlers/agents_archive.rs` | 原已共用 wrapper；offboard/unarchive；handoff 的前置副作用另見限制。 |
| `gateway/handlers/agents_update.rs` | 原已共用 update_agent_toml_as。 |
| `gateway/chat_commands.rs` | 原已共用 update_agent_toml_edit。 |
| `gateway/db_source_grants.rs` | 原已共用 update_agent_toml_with。 |
| `gateway/handlers/odoo_agent_rpc.rs` | 原已共用 wrapper。 |
| `gateway/handlers/channels_add.rs`、`channels_hot.rs` | 原已共用 wrapper；其他 atomic writes 目標為 config.toml。 |
| `gateway/handlers/agent_channels_rpc.rs`、`agent_channels_gchat_rpc.rs` | 原已共用 wrapper。 |
| `gateway/handlers/google_identity_rpc.rs` | 原已共用 wrapper。 |
| `gateway/handlers/dispatch_kb_agent.rs` | 原已共用 wrapper。 |
| `gateway/handlers/config_raw_rpc.rs` | 原已共用 writer + expected hash CAS；本輪只調整備份時序。 |
| `gateway/agent_files/flow.rs` | 原已共用 writer + base hash CAS；原始儲存／版本還原。 |
| `agent/registry.rs` | 不適用；讀 agent.toml，寫 resolved artifact，不寫回來源。 |
| `core/preset.rs` bind/resolve | 不適用；snapshot 用於 binding 計算，寫 preset_bindings.toml；profile mirror 在本輪已修 helper。 |
| `gateway/premium_templates.rs` | 不適用；純範本轉換交新建流程，另有 eval suite case 輸出，非既有 live profile 寫回。 |
| `gateway/handlers/agents_update_v168.rs` 型別驗證、`gateway/knowledge/agent_own.rs` | 不適用；serialize 供驗證，不寫 profile。 |
| `cli/lib.rs` onboarding/scaffold | 不適用於 stale RMW；從輸入產生新 TOML，未讀舊 profile 後整檔寫回；reset 已持 exclusive。 |
| `gateway/handlers/agents_create.rs`、`templates_create.rs` | 不適用；新建 profile，使用 create_dir 原子佔名。 |
| `cli/mcp/agents.rs` create | 不適用；新設定建立 profile，非舊 snapshot。 |
| `gateway/ephemeral/scaffold.rs` | 不適用；讀 parent、建立另一個 ephemeral profile，不寫回 parent。 |
| `python/duduclaw/tools/agent_tools.py` agent_create | 不適用於 stale RMW；從新設定生成 profile。 |
| `cli/portability.rs`、`cli/expert/reimport/retire.rs`、`gateway/expert_replace_journal.rs` | 不適用；目錄匯入／搬移／還原，第七輪已有 exclusive，不是 profile snapshot RMW。 |
| `fork/copy_policy.rs` promote_for_parent | 不適用；向 agent 目錄發布時排除 agent.toml 等結構檔。 |
| `scripts/live-test/google-connectors-baseline.mjs`、`google-delegated-identity.mjs` | 不適用；隔離驗收 home 的故障／race fixtures 與還原，非 production editor。 |
| `scripts/live-test/make-home.sh`、Rust/Python tests、CLI example | 不適用；fixture／範例建立。 |
| Python capabilities loader、Paperclip adapter、web | 不適用；讀取、註解或 RPC 呼叫，未找到直接 profile 寫回。 |

### 本輪修改檔案與定位

| 檔案（行號為本輪完成時定位） | 內容 |
|---|---|
| `crates/duduclaw-cli/src/preset_cmd.rs:128,192,368,383,399` | bind/unbind 接 shared writer、保留解除稽核、競爭測試。 |
| `crates/duduclaw-cli/src/lib.rs:81,10959` | 註冊 freeze helper／test-only hook；freeze 改 blocking worker 呼叫 helper。 |
| `crates/duduclaw-cli/src/agent_freeze.rs:1`（新增） | 鎖內修改 evolution/heartbeat，兩方向開關與三種 lifecycle 交錯測試。 |
| `crates/duduclaw-cli/src/profile_edit_test_hook.rs:1`（新增，整模組 cfg(test)） | 同步控制 snapshot → 另一執行緒 lifecycle 完成 → 延後欄位修改。 |
| `crates/duduclaw-cli/src/expert/mod.rs:279`、`expert/tests.rs:841` | expert shared writer 與保留 status／註解測試。 |
| `crates/duduclaw-cli/src/wizard.rs:262,581,792` | 範本 fresh edit 與回歸測試。 |
| `crates/duduclaw-cli/src/pack_cmd.rs:1141` | 兩條安裝路徑 parity 測試正規化新產生的 history／audit 時間；保留內容、雜湊、筆數與事件欄位比較。 |
| `crates/duduclaw-gateway/src/handlers/agents_update_v168.rs:423` | permission migration 也統一 shared writer。 |
| `crates/duduclaw-gateway/src/handlers/config_raw_rpc.rs:925,1153` | 備份移到鎖／屏障內，真實 timeout 與後續成功備份測試。 |
| `crates/duduclaw-gateway/src/handlers/fork_migrate_rpc.rs:269,539` | store 失敗 audit／勿重試訊息與隔離 SQLite trigger 測試。 |
| `python/duduclaw/tools/agent_tools.py:241`、`tests/test_agent_tools.py:52` | legacy status 轉 Rust CLI；成功／拒絕／啟動失敗／等待結果不明測試。 |
| `CHANGELOG.md:30`、本 wiki 第八輪節 | 使用者可見修復及完整工程紀錄。 |

### 驗證結果

所有 Cargo 均一次一個、`-j 4`、`CARGO_INCREMENTAL=0`，未執行 cargo fmt。下列為最終實際結果；輸出、修補前快照與 `task-only.patch` 保留於 `/tmp/autonomy-pause-round8/`。

| 最終命令 | 通過／失敗 |
|---|---|
| `CARGO_INCREMENTAL=0 cargo test -j 4 -p duduclaw-cli --lib preset -- --test-threads=1` | 8／0 |
| `CARGO_INCREMENTAL=0 cargo test -j 4 -p duduclaw-cli --lib freeze -- --test-threads=1` | 3／0 |
| `CARGO_INCREMENTAL=0 cargo test -j 4 -p duduclaw-cli --lib expert -- --test-threads=1` | 145／0 |
| `CARGO_INCREMENTAL=0 cargo test -j 4 -p duduclaw-cli --lib wizard -- --test-threads=1` | 13／0 |
| `CARGO_INCREMENTAL=0 cargo test -j 4 -p duduclaw-gateway --lib agent_files` | 80／0 |
| `CARGO_INCREMENTAL=0 cargo test -j 4 -p duduclaw-gateway --lib config_raw` | 12／0 |
| `CARGO_INCREMENTAL=0 cargo test -j 4 -p duduclaw-gateway --lib fork_migrate` | 6／0 |
| `CARGO_INCREMENTAL=0 cargo test -j 4 -p duduclaw-gateway --lib handlers::agents_update_v168::tests` | 8／0 |
| `CARGO_INCREMENTAL=0 cargo test -j 4 -p duduclaw-core --lib autonomy` | 33／0 |
| `PYTHONDONTWRITEBYTECODE=1 PYTHONPATH=python /opt/homebrew/bin/python3.12 -m pytest -q -p no:cacheprovider python/duduclaw/tools/tests/test_agent_tools.py` | 20／0 |

Rust 測試合計 308／0；Python 20／0。

`CARGO_INCREMENTAL=0 cargo clippy -j 4 -p duduclaw-cli -p duduclaw-gateway --tests --no-deps -- -A clippy::approx_constant -A clippy::async_yields_async`：exit 0。逐筆對照本輪 patch 新增行，警告 0；既有警告保留（CLI lib 159、lib test 210；gateway lib 438、lib test 428，含重複診斷，不可直接相加當唯一警告數）。完整輸出 `clippy.log`，新增行交叉比對 `clippy-added-lines.txt`。

`git diff --check` 通過；已確認負向測試的 stale_snapshot 注入不存在於正式程式。最終仍在 `feat/autonomy-pause-20261028`，未執行 fmt、commit、push、stash、切換分支或工作樹清除。所有本輪指定測試與 clippy 均完成；Daybreak 安全重審及已知限制中的實機驗收仍未執行。

已觀察並保留的前置驗證／失敗：

- preset 首次 8／0；負向 mutation 暫時把 hook 前讀到的舊 snapshot 塞回 writer closure，`cargo test -j 4 -p duduclaw-cli --lib delayed_` 得到預期的 0／2，兩者都是實際 `active`、預期 `archived`。測試腳本 finally 還原修補版，正式程式沒有 stale_snapshot 注入。
- 還原後 preset 平行執行為 7／1：既有 `resolve_hook_caller_tests::preset_bind_and_unbind_refuse_inside_an_agent_session` 暫設 `DUDUCLAW_AGENT_ID`，使同時執行的 legacy pack install 被 AI session gate 拒絕；不是 profile race 測試失敗。CLI 改加 `-- --test-threads=1`，preset 重跑 8／0、freeze 3／0。
- expert 首次 144／1：既有雙路安裝 parity 測試逐字比較 shared writer 新增的版本時間戳檔名。只正規化時間、version ID 的時間部分與由時間衍生的 audit chain 值，仍比較版本內容、hash、索引順序、筆數及事件欄位；重跑 145／0。wizard 13／0。
- 系統 Python 3.14 缺 pytest，未執行任何測試；改用既有 `/opt/homebrew/bin/python3.12`，最終 targeted Python 測試 20／0。沒有安裝依賴。
- 一般獨立程式碼複查發現 unbind 部分成功時的稽核次序、Python wait 失敗不應宣稱未變更；均修正且複查確認。這不代表 Daybreak 安全放行。

### 已知限制

- **handoff + auto_archive 為 PARTIAL 邊界（依交辦只記錄，未改程式）**：`gateway/handlers/agents_archive.rs` 先搬移 memory/wiki/tasks，最後才經 offboard 取得 lifecycle barrier。若 30 秒 admission drain 逾時，先前資料搬移不回滾，來源員工仍 active；應讀成「資料已搬移、來源未封存」，不可回報整筆交接原子成功。
- CLI scaffold、wizard、MCP create、Python create 的「exists 檢查 + create_dir_all」不是原子佔名；本輪沒有處理同名建立競爭。dashboard 新建使用 create_dir。這和已修的舊 snapshot 寫回是不同路徑。
- Python 轉接最初依賴 PATH 上的 Rust `duduclaw`，已由下方第九輪補記改為明確設定的絕對路徑；單元測試沒有把已安裝 CLI 視為本分支編譯產物或跨程序實機驗收。
- fork 發布、store、audit 仍非跨資源交易；store 失敗後仍保留未 resolved 資料，訊息要求勿重試，沒有新增自動修復／冪等補記。
- 本輪一般靜態複查不等於 `gpt-daybreak-blue-latest` 安全重審，也未做 live gateway／Windows／Linux 實體機驗收。

### 第九輪 LOW 補記：Python 子程序等待與 CLI 路徑

2026-10-29：`python/duduclaw/**` 沒有其他 CLI 查找慣例，因此沿用 Rust `resolve_duduclaw_bin` 既有的 `DUDUCLAW_BIN` 名稱，Python pause/resume 要求操作者明確設定可信、可執行的絕對路徑（例如 `DUDUCLAW_BIN=/opt/duduclaw/bin/duduclaw`）；解析符號連結，缺少、相對路徑或無效檔案均拒絕，沒有 PATH fallback，也沒有新增設定名稱。

`communicate()` 上限 45 秒；逾時、取消或 pipe 錯誤後 terminate，等待 2 秒，仍未結束則 kill，排空 pipes 並 wait 回收。清理不受重複取消打斷；逾時回覆與取消例外均明示結果未知、重試前須重讀員工狀態，取消完成清理後繼續拋出 `CancelledError`。測試使用 fake child 與 0.01 秒時限，涵蓋 terminate／kill／reap、重複取消及路徑拒絕／符號連結。

沿用既有 `/opt/homebrew/bin/python3.12`（實際 interpreter：`/opt/homebrew/opt/python@3.12/bin/python3.12`，pytest 9.0.2，非 venv），沒有安裝依賴。最終命令：`PYTHONDONTWRITEBYTECODE=1 PYTHONPATH=python /opt/homebrew/bin/python3.12 -m pytest -q -p no:cacheprovider tests/python/ python/duduclaw/tools/tests/test_agent_tools.py`，441 passed／0 failed；其中 agent tools 35 件。Ruff 與 `git diff --check` 通過；pytest-asyncio 既有 fixture loop scope 未設定警告保留。一般獨立程式碼複查未發現阻擋問題，不代表 Daybreak 安全重審；未跑 Cargo 或真實 CLI 寫入，未 commit／push／stash／切換分支。

## 收尾狀態與另開分支待辦（Claude，2026-10-29）

最後一次全量實跑（第八輪修補後，`CARGO_INCREMENTAL=0`、`-j 4`）：core 1104／0、agent 351／0、inference 84／0、cli 2301／0、gateway 10201／0、fork 115／0；Python（`/opt/homebrew/bin/python3.12 -m pytest tests/python/ python/duduclaw/tools/tests/test_agent_tools.py`）441／0，含 Python 呼叫 CLI 的 timeout／絕對路徑修補。新增行 clippy 真正新增 0（`mcp_dispatch.rs:765,766` 為 diff 對齊誤報，程式碼在 base 即存在）。

Daybreak Blue 第九輪：第八輪 MEDIUM 與兩個 LOW 已關閉；新找到的三個 MEDIUM 皆為既有問題，經擁有者決定（2026-10-29）另開 fix 分支，不卡本分支：

| 待辦 | 位置 | 說明 |
|---|---|---|
| `agent.resolved.toml` materialization 競態 | `crates/duduclaw-agent/src/registry.rs:211-247` | 無鎖、無 revision CAS 的寫入／刪除；舊掃描可蓋掉新結果，使 preset 限制暫時失效。需單一 writer＋per-agent lock＋revision 驗證。 |
| Python legacy `AgentTools` pause/resume 無呼叫者授權 | `python/duduclaw/tools/agent_tools.py`、`crates/duduclaw-cli/src/lib.rs` `agent pause/resume` | 不驗 caller／組織關係，resume 可把 archived 員工改回 active；CLI 的 `ai_session_guard` 未涵蓋。production 目前找不到掛載點。 |
| 建立員工的 exists＋create_dir_all 競態 | `crates/duduclaw-cli/src/lib.rs:10517`、`wizard.rs:218`、`mcp/agents.rs:239`、`agent_tools.py:82` | 同名並行建立時後到者以 `active` 覆蓋；改用原子 `create_dir` claim（dashboard 路徑已是）。 |
| `fork.resolve` 缺 per-agent Operator 檢查；清理 DB 錯誤被忽略 | `handlers/dispatch_org.rs:641`、`fork_migrate_rpc.rs:190,227` | 第七輪列出，既有問題。 |

已知限制維持：handoff 自動封存前已搬移資料；`merge_or_select` 持 admission 時可能使暫停 30 秒逾時；Windows 非 Unix 路徑缺 handle 層級驗證（擁有者未決）；跨程序競態、crash/fsync、Linux／Windows 原生與真實 LLM 未做活體驗收。

## Rebase 到 main 7040fac7 與合併後修補（Claude，2026-10-30）

分支從 cea6515a1 rebase 到 7040fac7（main 期間合進 fb37bc5c 背景工作改走員工 runtime、bd1b7f36 資料外送防護、287214cb 目標費用上限等）。27 段衝突由 Grok 合併；Grok 隨後額度用完，Codex 也不可用（至 2026-11-04），之後的修補由 Claude 撰寫，審查改由獨立 Claude subagent 唯讀進行。

合併要點：main 把壓縮、事實擷取、指令釘選、決策擷取、摘要、標題等背景工作改走各 runtime 後，admission 移到新的共用位置：`process_group.rs` `output_for_model`（CLI 類 runtime 持 lease 跨 spawn）、`runtime/codex.rs` `collect_codex_output`、OpenAI-compat 在 response headers 後釋放。版本探測（`--version`）仍走不取 lease 的 `output()`。獨立審查逐條確認每條 runtime 路徑在暫停時都會被擋。

rebase 後 11 個 main 新增測試不符本功能的閘門，全部只改測試：宣告自主或全域維護身分（`autonomous_runtime::scope`／`with_utility_context`）或補上 `status = "active"` 的員工設定檔，對應正式呼叫端的做法。`goal_judge` 測試的第 6 次 CLI 呼叫原本因缺身分被擋、斷言卻仍成立，補上 scope 後才真正測到。

審查發現與修補（Claude 撰寫）：

| 發現 | 嚴重度 | 修補 |
|---|---|---|
| 對話壓縮只認 `AUTONOMY_` 前綴；非 Claude runtime 會把拒絕包一層，暫停若落在檢查與 spawn 之間，整段對話歷史會被占位文字取代 | MEDIUM（合併後出現） | `channel_reply/inner.rs` 在 scope 內讀 `was_denied()`，被拒就 return |
| failover 把傳輸層的暫停拒絕記成供應端失敗，三次後讓 runtime 冷卻，真人聊天也受影響 | LOW | `failover.rs` 主要分支被拒即停、不記失敗、不試備援；備援分支被拒不記失敗。測試 `autonomy_refusal_never_puts_a_runtime_into_cooldown` 涵蓋兩個分支 |
| Claude 帳號輪替把暫停拒絕記到每個帳號 | LOW～MEDIUM（本功能既有） | `claude_runner.rs` `call_with_rotation` 與 `channel_reply/cli_spawn.rs` 輪替迴圈，被拒即回傳、不計帳號、不換下一個 |
| 提醒在檢查後才被暫停時，包過的拒絕對不上本地前綴，提醒被標成 Failed | LOW（本功能既有） | `reminder_scheduler.rs` 在 scope 內讀 `was_denied()`，被拒時加上 `autonomy paused: ` 前綴，維持 Pending |

未修、列後續：`DENIED` 旗標在同一個 root scope 內是黏著的；先前有過一次被吞掉的拒絕（例如 team round 內另一個已暫停身分的巢狀 scope）或長 scope 中途解除暫停時，之後真的供應端錯誤不會被計入、也不試備援。dispatcher 與 ephemeral dispatch 早已是同樣行為，影響小。可行修法：提供「在新的子旗標下執行並回傳本次是否被拒」的輔助函式。

### 再 rebase 到 c672814b（v1.77.0 的 #252–#254 合併後）

main 合進本 session 的三個小修補後再 rebase 一次，衝突 4 檔，由 Claude 解：

- `mcp/evolution.rs`：保留 #252 抽出的 `write_evolution_toggle`（鎖內重查呼叫者權限、以 `actor.id()` 記錄寫入者），改用 `edit_agent_toml_guarded`，把本功能的背景 admission 放在寫入前的 guard，持有到寫入完成。
- `lib.rs` 的 `agent pause/resume/freeze/unfreeze`：採用 #254 的 `agent_control_cmd`（AI 工作階段拒絕、共用加鎖寫入器、稽核）。本功能原本的 `agent_freeze.rs` 刪除；它保護的行為（凍結不覆蓋同時完成的暫停／封存）由共用寫入器提供，測試 `freeze_delayed_edit_preserves_completed_lifecycle_changes` 移到 `agent_control_cmd/agent_cli_guard_tests.rs`，改測真正的指令路徑。上文提到 `agent_freeze.rs` 的段落是 rebase 前的歷史紀錄。
- `wiki_namespace_tests/part3.rs`：保留本功能的 `with_background_caller` 包裝，加上 #252 新增的 `op` 參數。
- `CHANGELOG.md`：兩邊 `[Unreleased] Security` 條目都保留。
