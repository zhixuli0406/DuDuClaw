# 全樹 code review — 2026-09-28（Team-as-Agent ＋ CCR／因果圖／決策孿生）

> 範圍：工作樹相對 HEAD `cbdc4338` 的全部未 commit 變更（515 檔、+78k/−16k）。
> 方法：6 隻獨立審查 agent 依工作線分工、唯讀、以「rustfmt(HEAD) 對工作樹」的語意 diff 審（487 個 modified `.rs` 中 411 個只是 rustfmt 整檔重排，語意變更 ≤10 行）。每條 finding 標 CONFIRMED（追過呼叫路徑）／PLAUSIBLE。
> 詳細報告：session scratchpad `review_{ccr,causal,decision_core,decision_review,team,misc}.md`（本檔為彙整）。
> 「處置」欄於修正波完成後回填：F1＝CCR＋因果 fixer、F2＝決策孿生、F3＝Team-as-Agent、F4＝文件＋web 殼層；「拍板」＝需使用者決定；「留」＝已記錄未處理。

## 0. 編譯與測試基線（本次實跑）
- `cargo check --workspace --all-targets`：PASS（5m00s，2 個未用 import 警告在 `ccr_compare_cmd.rs`）。
- `RUST_MIN_STACK=33554432 cargo test -p duduclaw-gateway shadow --lib`：30 passed（含 `shadow_http_binds_prospective_sources_and_review_states`、`c7_synthetic_shadow_harness`）。Codex 停下時正在等這一組。
- web：實作 agent 全量 `vitest run` 255 passed / 1 failed（`technical-terms.test.ts`：codex 的 `ccrDashboard.connectorLifecycle*` 洩漏內部詞 `journal`）。

## 1. P1（必修）

| # | 工作線 | 位置 | 缺陷 | 處置 |
|---|---|---|---|---|
| 1 | CCR | `telegram.rs:1592,1608,1619` | 群組 `/ask` 把 chat_id 當 CCR principal，同群兩人互相取回對方工具結果原文；違反 spec「兩使用者不得互取」 | F1 |
| 2 | CCR | `channel_reply.rs:3681` vs `1366/1439` | assistant 回覆先落 session 再檢查 delivery lease；來源撤銷後「已拒絕送出」的答案仍留在歷史並會被蒸餾進記憶／wiki | F1 |
| 3 | 因果 | `causal_wiki.rs:174`、`causal_memory.rs:89,119` | 新 trigger「持租約禁改 artifact」未排除 wiki 同步抹除與 memory trigger，租約期間任何 wiki 變更讓 `CausalStore::open()` 失敗、整個 `/api/causal/*` 回 500 | F1 |
| 4 | 決策 | `decision_store.rs:913,3124` | UTC 拼法只用 chrono 驗、唯一性靠 SQLite `strftime`；小寫 `t` 分隔符 chrono 接受、SQLite 回 NULL → 同目標日可凍結兩份 forecast | F2 |
| 5 | 決策 | `decision_store.rs:2806,2875,3405` | 同根因讓政策交接閘與 SLA 跨日邊界檢查 fail-open | F2 |
| 6 | Team | `team_composer.rs:3128`、`goal_loop.rs:2545`、`dispatch_engine.rs:996,2265` | 團隊任務從不 claim、`claimed_at` 恆 None → verifier 永遠零證據；settle 時窗退化成整個任務生命週期，跨輪證據可為當輪背書 | F3 |
| 7 | Team | `mcp.rs:19987-19996` | `team_handoff` 身分退路把 `[agent] role="planner"` 一般員工當團隊成員，無 task/round 綁定可寫偽造封包進任意任務 | F3 |
| 8 | Team | `team_composer.rs:3044,2986,3164` | 封包自由文字進 prompt 無 `xml_escape`／`scan_input`，`</work>` 可關閉 DATA 圍欄 | F3 |
| 9 | Team | `task_packet.rs:373`、`docs/spec/task-packet.md:31,47` | `tool_scope`／`irreversible` 零讀者，但型別 doc 與 spec 宣稱強制執行 | F3（改文件為「未接線」） |
| 10 | Team | `eval/stats.rs:269-275` | `paired_comparison` 無單叢集 `choose_se` 退回，單目錄 suite 的 `--baseline` 永遠 unresolved 且誤標為樣本不足 | F3 |
| 11 | Team | `eval/case.rs:72`、`runner.rs:107` | `[case] runtime` 只驗證從未消費，非 Claude baseline 實際跑在 claude CLI 上 | F3 |
| 12 | Team | `eval/team_probe.rs:521,560` | `--repeats` 未收斂成每案一分，`MatrixCell.n` 記 runs，CI 以 √K 假縮窄 | F3 |
| 13 | Team | `prompt_compression.rs:284`、`session_summarizer_task.rs:195` | never-trim 標題是任何使用者可打出的純文字：一行 `## Constraints` 關閉壓縮並把任意文字無上限永久釘進 session summary | F3 只做上限（便宜緩解）；provenance 綁定＝**拍板** |
| 14 | Team | `runtime/codex.rs:291-305` | `DUDUCLAW_MCP_API_KEY`／`DUDUCLAW_AGENT_TOKEN` 進 argv，同機他人 `ps` 可讀（既有形狀，本波擴大曝光面） | **拍板**（改 `cmd.env()` 前需活測 codex 是否傳 env 給 MCP 子行程） |
| 15 | Team | `runtime/codex.rs:56-91` | codex `ReadOnly` 在 `--approve-for-me` 下不擋寫，權限閘 fail-open | **拍板**（拒 spawn vs `-s read-only`） |
| 16 | Team | `ephemeral.rs:1497`、`org_field_guard.rs:73` | 角色成員以員工工作區為 cwd，file-guard 判成員工本人；`[capabilities]` 不在凍結欄位 → 第三方 executor 可改員工 envelope 自我提權 | **拍板**（把 `[capabilities]` 納入 hook 凍結欄位是政策變更） |
| 17 | 文件 | `CHANGELOG.md [Unreleased]` | 完全沒有 CCR／因果／決策孿生／UCCI 條目（違反同 commit 原則） | F4 |
| 18 | 文件 | `docs/README.md`、`docs/features/README.md` | 4 份新文件未索引 | F4 |
| 19 | web | `nav-model.ts`、`SystemHomePage.tsx` | 三個新頁面（CCR／Causal／Decision Lab）全無 `newIn` 標籤 | F4 |

## 2. P2（應修）

| # | 工作線 | 位置 | 缺陷 | 處置 |
|---|---|---|---|---|
| 20 | 因果 | `causal.rs:1016`、`causal_memory.rs`、`causal_wiki.rs:196` | 到期／wiki／memory 三路只清 excerpt，不清 `claims.context_json` 與 `nc_reviews.rationale`（erase 有清） | F1 |
| 21 | 因果 | `causal.rs:90-132 still_valid()` | 唯讀連線不跑 wiki 同步，provider 呼叫期間 wiki 頁被改偵測不到 | F1 |
| 22 | 因果＋CCR | `wiki_fence.rs:80`、`trust_store.rs:476`、`wiki_mcp_source.rs:108` | delivery fence 全 home 單鎖、寫入端非阻塞無重試；一次 CCR 回覆讓所有 agent 的 wiki／trust 寫入 `Busy` 靜默失敗；`Busy` 又被映成 `InvalidInput` | **拍板**（鎖粒度與重試策略） |
| 23 | 因果＋CCR＋決策 | `CausalStore::open()`、`CcrStore::open()`、`DecisionStore::open()` | 每次讀取重建 schema＋`BEGIN IMMEDIATE` 寫鎖；N+1（100 claims → 100 次 open × W 個 wiki 檔整讀；366 天 assess → 數千次寫鎖） | F2 做決策低風險版；其餘留 |
| 24 | CCR | `ccr.rs:1768`、`channel_reply.rs:1098` | `still_valid()` 在 async reactor 上做 3-4 次帶寫鎖的 SQLite open＋檔案 I/O | 留（效能重構） |
| 25 | CCR | `ccr.rs:2240-2284` | `find` SQL 每列重算 `lower(original)` 達 17 次、scope 無索引、模型可重複觸發 | 留 |
| 26 | 決策 | `server.rs` 16 條 handler | `Json<T>`／`Query<T>` extractor 在 `authorize_causal_admin` 之前跑，未認證可探 schema／回吐輸入 | F2 |
| 27 | 決策 | `decision_store.rs:2006` | `scrub_expired_ticket_sources` 無背景排程呼叫端，工單原文逾期仍留磁碟 | F2 |
| 28 | 決策 | `docs/spec/support-decision-twin.md` | 23/34 條端點未記錄；§167 engineering-validation 描述過時 | F4 |
| 29 | Team | `artifacts.rs:1224` | team packet 產物候選繞過 `is_artifact_path`，`SOUL.md`／`state/…` 可被封存成可下載產物 | F3 |
| 30 | Team | `dispatch_engine.rs:2993,3023` | `observe_round`／`capability_blocked_in_window` 未套 evidence union → 團隊輪 fidelity 恆 None，學習只記功不記過 | F3 |
| 31 | Team | `team_composer.rs:1492,3268` | verifier 打不通時 `passed=true` 且 settle 產物無痕 | F3 |
| 32 | Team | `team_composer.rs:2083,972` | `verify_artifacts`／`read_packets` 無大小上限與 `is_file()` 檢查 | F3 |
| 33 | Team | `fault_attribution.rs:236,180` | R3 未看 fidelity（`McpOnly` 下正常回覆被判 Harness）；`is_environment_failure` 漏 `AuthFailed`／`AccountsCoolingDown*` | F3 |
| 34 | Team | `runtime_dispatch.rs:555,542` | Claude 分支丟掉 `hint.effort`；utility 與 judge 同為 codex 時跨家族 failover opt-out 失效 | F3 做前者；後者**拍板** |
| 35 | Team | `runtime/{gemini,antigravity,grok}.rs` | 三個 allowlist 內 runtime 不讀 `SPAWN_OVERRIDE.work_dir`，成員寫的檔隨 scaffold GC 消失 | **拍板** |
| 36 | Team | `eval/matrix.rs:1426`、`verifier_cell.rs:517`、`team_probe.rs:458` | 退化 cell 寫零寬區間；Wilson 分母是 runs；`--budget-usd` 截斷靜默 | F3 |
| 37 | Team | `mcp_auth.rs:751` | `team_handoff` 掛 `MemoryWrite` 可被外部 key 觸達，寫的是共用跨任務目錄 | **拍板** |
| 38 | UCCI | `ucci.rs:66-89` | 觀測 JSONL 用 per-instance Mutex 非 `with_file_lock` | F1 |
| 39 | UCCI | `engine.rs:281-309` | `ucci_shadow_strong` 同步阻塞使用者回覆 | 留（文件揭露） |
| 40 | 設定 | `config/duduclaw.example.toml` | `[causal_extraction] enabled = true` 與註解矛盾；缺 `[team]` 範例 | F4 |
| 41 | web | 三語系 | 使用者面文案洩漏內部詞（replay hash／lineage／artifact／CCR） | 留（另立 UX 走查） |

## 3. P3（次要，已指派者標 F*，其餘留）
CCR：`ccr_compare_cmd.rs` 未用 import（F1）、`ccr_run_cmd.rs:391` base-url 前綴比對可被 userinfo 繞過（F1）、`GuardedReply` doc「前後各檢查」與多數 adapter 不符（F1）、ACP 路徑對任何 CCR 回覆回「來源已變更」（F1）、受保護區段標題硬編碼第二份、tool loop 日誌記明文 handle、三個 legacy wrapper 無呼叫端。
因果：`sign_flip` 缺零值護欄（F1）、`validate_page_path` 在 `metadata()` 之後（F1）、`.ccr-leases/*.lock` 孤兒檔、`begin_ccr_revocation` fence 只寫不刪、wiki 匯入 `retention_at = i64::MAX` 超 2^53。
決策：`load_shadow_review_screen` 不重算 verdict（F2）、四處 u64/u128 原生 number（F2）、`digest_lineage` 靜默截 16（F2）、screen hash `onChange` 漏清 assessment（F2）、`--save-run` 無上限 `fs::read`（F2）、CLI 匯入缺 `synthetic-support-` 前綴守衛（F2）、`[..10]` byte-index 切片兩處（F2）、`inclusive`／`sample` 無自我防護（F2）、`load_daily_run`／`load_event_run` 不比對引擎 hash（F2）。
Team：封包修正非原子寫入（F3）、越界 `artifacts[].path` 只審計不標記、verifier runtime 未比對 allowlist、ledger rotate 讓額度歸零、`AlreadyFrozen` 未處理、`spawn_admission` 三函式零呼叫端、`strip_workspace_prefixes` 改寫證據文字、`record_role_usage` last-writer-wins、`FrozenRole.family` 零讀者、verifier utility 呼叫算進派工額度、`copy_tree` 無深度上限、planner/executor cell 條件不對稱。
UCCI／web：UCCI 升級沿用 legacy log 訊息；`SystemHomePage.test` 缺 Causal 卡測試（F4）；`support_pilot_review` 深連結指向 admin-only 路由而 `/inbox` 全角色可見。

## 4. 文件不實清單（審查者逐條實錘，F3／F4 修）
`docs/features/56-team-as-agent.md:270`（verifier 拿到兩塊證據——不成立）、`:381`（fault_side「刻意缺席」與 Status 節矛盾）、`:266`（越界路徑「verifier 看得到」）、Cascade 節（跨家族 preferred 會死路）、封包「原子寫入」（composer 修正寫入沒有）；`mcp.rs:19948` doc（org role 不會解析成 team Role——錯）；`task_packet.rs:373`／`docs/spec/task-packet.md:31,47`；`docs/guides/evals.md:48` vs `:63-64` 自相矛盾、`:92-123` 範例缺欄位；`role_model_matrix.rs` doc「零變異 ⇒ 缺鍵」與 `matrix.rs` 不符；`MatrixHeader::paired_seeds` doc vs `SEED_APPLIED_ANYWHERE=false`；`runtime/codex.rs:51-55` vs `:77-79` 相反；`docs/spec/causal-evidence-graph.md:39-40,82`；`docs/spec/reversible-context-ccr.md` 稽核 digest vs 日誌明文；`decision_forecast_dashboard.rs:2` 模組註解對 engineering-validation 不成立；`CLAUDE.md` Architecture Overview 無 CCR／因果／決策段。

## 5. 審查者明列「已驗證無問題」（省下一輪重審）
決策：41 個 `handle_decision_*` 全過 `authorize_causal_admin`；`shadow_dashboard_response` 降級涵蓋所有可能夾帶操作者文字的變體；inline source 併發清理只清自己；三條收據鏈 payload 逐欄比對＋到期＋`rec.status != polled`；`*_screen_matches_current` 覆寫 `assessed_at_utc` 不掩蓋其他欄位；c7 harness 時間注入僅 `#[cfg(test)]`；前端 `valid*` 拒絕跨 policy／跨 ID；時序不變量四條全在。
Team：`--matrix`／`--team-2x2`／`--budget-usd` 語意一致且 pre-flight 拒未知模型；`team_spec_json` 凍結帶 `WHERE team_spec_json IS NULL`；`role_turns.jsonl` 走 `append_chained_line` flock＋0600；`ROLE_COST_ATTRIBUTION` 為 task_local；`packet_path` fail-closed；`parse_verdict` PASS 判定已錨定；`team_gate` NaN 偏向 Solo。
因果：span 一律 `str::get()`；scope 精確比對；估計器假設不成立回 `Unknown`；授權面 admin/employee 分流有測。
CCR：SQL 內精確 scope 比對；content hash 從不當授權；三層 tombstone；bound source 無 validator 一律拒。
UCCI：預設全關；`margin_uncertainty_of` 缺資料安全回退；docs/57 與行為一致；i18n 三語鍵集一致（6278 鍵零差異）；三新路由 `RoleGuard minRole="admin"` 有測。

## 6. 供應鏈備註
`Cargo.lock` 唯一新增外部套件：`ucci` v0.1.0（git pin 至個人帳號 `varunkotte6/ucci` 特定 commit，非 crates.io，僅依賴 serde／serde_json；審查者讀過其 `signal.rs`／`router.rs`，NaN/Inf 防護完整）。其餘變動為 workspace 內部 crate 依賴表調整。修正波 F1 另為 `duduclaw-cli` 新增 `url = "2"` 依賴（gateway 已有，lock 無新 crate）。

## 7. 修正波結果（同日，四波並行、串行驗證）

**F1 CCR＋因果**：P1 #1（新 `reply_principal_for_sender`，`/takeover` 同類一併修，三條回歸測試含結構性掃描）、#2（選「撤銷時標 `undone_at`＋阻蒸餾」而非搬持久化：`CcrTurnDelivery` task-local＋`session.rs::revoke_message`）、#3（wiki／memory 兩路補 `NOT EXISTS leases`，memory trigger 改每次 DROP+CREATE，補延後清掃重試路徑）全 COMPLETE；P2 #20/#21/#38 與 P3 五項 COMPLETE。掃同類新發現：**line／feishu／googlechat／dingtalk／msteams 拿不到寄件者時傳字面值 `"unknown"`，所有匿名寄件者共用同一 CCR principal**（同類洞，改法待拍板：比照 Telegram 改空字串 fail-closed）。殘餘：`GuardedReply::new` 通過後、adapter 逐段送出前才撤銷的情況 session 列仍留（要動 11 個 adapter）。
**F2 決策孿生**：P1 #4/#5 COMPLETE（先用測試實錘 chrono 收小寫 `t`、SQLite `strftime` 回 NULL；`shadow_utc_day_key` 正規化＋`assert_readable_shadow_days` 對 legacy 列回 `Corrupt`；5 處 `strftime` 補 `IS NOT NULL`）；#26 16 條 handler 改 Bytes／RawQuery 授權後解析＋表驅動測試；#27 每小時 `run_ticket_retention_sweeper`；#23 決策 store 低風險版（`SCHEMA_VERSION` 相符跳過 DDL）；P3 九項 COMPLETE，其中 `engine_matches_current` PARTIAL（新 `LoadedRun<T>` wrapper 尚無呼叫端——接線位置待拍板）。**行為變更**：CLI `decision-import-pilot` 現在拒絕 `synthetic-support-` 前綴（與儀表板對齊）。
**F3 Team-as-Agent**：P1 #6（本輪起點改 `task_iterations.dispatched_at`／`role_turns` 本輪最早列，皆無則不讀時窗、grounding 退 `Skip`；e2e 測試斷言 prompt 含兩個證據區塊；改寫把缺陷鎖成正確行為的既有斷言）、#7（刪 fallback #2、移除正面斷言該缺陷的既有測試 `accepts_the_agent_role_fallback`）、#8（全欄位 `xml_escape`＋截斷後 `scan_input` 拒收並審計＋`<task_packet>` 圍欄）、#9（三處文件改「未接線」）、#10/#11/#12 全 COMPLETE；P2 #29（採「只否決 `INTERNAL_NAMES`／`INTERNAL_DIRS`」而非整個 `is_artifact_path` 白名單，因白名單會打壞既有 `.txt` 產物測試）、#31、#32、#33、#36 COMPLETE；#30 與 #34 前半採最小選項 PARTIAL；#13 只做上限（`NEVER_TRIM_FLOOR_MAX_TOKENS=6000`、summary protected 段 4 KiB）。
**F4 文件＋web**：#17/#18/#19/#28/#40 全 COMPLETE；UCCI 設定鍵核對後改正為 `inference.toml [router]`；另主動加 CLAUDE.md UCCI 條目與側欄入口（未同步個人版，依同類 admin 頁慣例）——兩項待拍板；`technical-terms.test.ts` 恢復綠燈。
**協調者裁定**：`decision_store::tests::causal_removal_requires_ccr_and_tombstones_before_failed_source_mutation` 是 codex 後期把撤銷改成「fence 一寫即對讀者隱藏」後自己的早期測試過時（codex log 顯示最後一次通過在該語意改變前，之後只跑過 shadow 篩選），已改寫為 fence 語意＋重試成功斷言；實作者的 `interleaved_sla_score_callers_keep_the_surviving_source` 並行 flaky 是兩執行緒同時對同一 causal DB 建來源，已改成 A 就緒後再起 B。**新發現 CI 阻斷項**：`duduclaw-cli` 的 `ccr_cmd::tests::principal_flag_revokes_the_gateway_derived_scope` 在預設測試執行緒堆疊下溢位使整個 cli 測試二進位 SIGABRT（CI 會紅）；c7 harness 文件要求 `RUST_MIN_STACK=33554432` 而 CI 未設——已另派修正。
**最終串行全量驗證**：見下方「8.」。

## 8. 最終串行全量驗證（協調者實跑，修正波全部落地後）

| 項目 | 結果 |
|---|---|
| `cargo check --workspace --all-targets` | PASS（1m40s；未用 import 警告清零，只剩 HEAD 既有 unsafe 區塊警告） |
| `duduclaw-core --lib` | 720 passed / 1 ignored |
| `duduclaw-llm --lib` | 267 passed（`ccr_stores_post_interceptor_original_before_marker` 原為 Codex 遺留失敗：測試自選的 `[redacted]` 前綴撞上 `ccr.rs:777` 的 JSON 保真閘，改用 `<REDACT:TEXT>` 形狀的標記，三個不變量未弱化） |
| `duduclaw-cli-runtime --lib` | 104 passed |
| `duduclaw-agent --lib` | 248 passed / 2 failed → 兩者單獨重跑通過；`account_rotator.rs` 與 HEAD 逐位相同，`credential_hardening_tests::{a_conclusive_probe_failure_suppresses_the_next_tick, consecutive_conclusive_failures_walk_the_probe_ladder}` 是 60–65 秒時窗的負載型 flaky（HEAD 既有） |
| `duduclaw-security --lib` | 359 passed |
| `duduclaw-memory --lib` | 359 passed |
| `duduclaw-inference --lib` | 118 passed |
| `duduclaw-gateway --lib` | 7,084 passed / 2 failed / 12 ignored / 1 skipped（192s）→ 2 失敗為 F2 改十進位字串後 `causal_curation_api_tests` 內四處舊 `as_u64()` 斷言（brief 的 `exploratory_forecast.model_abs_error_sum` 仍是 JSON number、report 為字串），改比對數字後 8/8 通過；skipped 的 `system_doctor_includes_mcp_server_check` 在 `check_docker()` 加 10 秒逾時後單獨跑 10.0s 通過 |
| `duduclaw-cli --lib` | 1,609 passed / 1 ignored（`run_on_big_stack` 套 16 個 `Cli::try_parse_from` 測試後無 SIGABRT） |
| `web`: `tsc -b` | exit 0 |
| `web`: `vitest run` 全量 | 2,051 passed / 1 failed → `ManageShell` 側欄排序測試補三個新 `newIn` 項後 37/37 通過（重跑 3 檔） |

**新發現與處置（驗證階段）**
- `check_docker()`（`handlers.rs`）對 `docker info` 無逾時（HEAD 既有）；本機 Docker Desktop VM 卡死（daemon 行程活著、socket 存在、`docker info` >60s 無回應）讓 gateway 全量測試掛 1 小時 5 分鐘。已加 `CONTAINER_RUNTIME_PROBE_TIMEOUT = 10s`＋`kill_on_drop`，逾時回 `warn`。**操作者需重啟這台機器的 Docker Desktop。**
- brief 與 dashboard report 對同一個 `model_abs_error_sum` 一個送 JSON number、一個送 decimal string（F2 P3 #5 只改了審查者點名的四處 DTO，`decision_brief.rs` 未在其中）——P3 一致性欠帳，列拍板。
- `CHANGELOG.md [Unreleased]` 曾出現兩個 `### Fixed`（四波並行追加所致），已合併為一（72 條）並補 docker 逾時條目；順序 Fixed → Added → Changed。

**未做活體驗證**：全部為 build＋單元／整合測試證據；沒有起 gateway、沒有真 CLI／真通道／真瀏覽器走查。SLA shadow 面板、`newIn` 徽章、CCR guarded reply 的撤銷路徑都只有 jsdom／單元層證據。

## 9. 第二波：拍板項全部照建議執行（同日，使用者拍板後）

| 代號 | 項目 | 結果 |
|---|---|---|
| W2-A | 五個 webhook adapter（line／feishu／googlechat／dingtalk／msteams）匿名寄件者 CCR principal fail-closed；`ccr_runtime` placeholder 防線（`unknown`／`anonymous`／`system`／空白，精確比對）；`ccr.rs` bracket 啟發式改「兩端對應括號」 | COMPLETE，15 條測試含修前 FAIL 實證；`"unknown"` 的 session key／稽核用途維持原值 |
| W2-B | wiki delivery fence 改 per-wiki-dir（trust 列依 `agent_id` 推導目錄、批次依目錄排序取鎖）；寫入端 5 秒有界等待、`Busy` 獨立錯誤並回 503、`feedback_bus` 重試一次且丟棄留痕；交付租約縮到 100 ms 讀取窗口，送出前重驗 epoch＋hash（同位元組重寫仍拒送） | COMPLETE；順帶修 `mcp.rs` 兩處 home 級 fence（否則新舊粒度混用留破口）；4 條既有測試改為新語意並說明 |
| W2-C | codex `ReadOnly` → `-s read-only -c approval_policy=never`（真擋寫，該級別 MCP 工具被自動拒絕並 warn）；gemini／antigravity／grok／codex 統一 `resolve_spawn_work_dir`（`is_dir` 驗證＋回退；grok 的 `.grok/` 以 cwd 為根，覆寫工作根另拿一份）；判官／評估器路徑一律禁跨家族 failover；antigravity 解析器降級（壞行跳過、無 result 退最後行、usage 缺漏回 None）；`-c` dotted-key 鍵半邊驗證 | COMPLETE；codex 對 MCP 子行程 `env_clear` 只留 11 白名單變數（三輪探測＋openai/codex `rmcp-client/src/utils.rs` 原始碼一致） |
| W2-C 續 | codex 憑證離開 argv：版本閘——`codex --version` 探測一次快取，`>= 0.157.0` 走 `-c mcp_servers.duduclaw.env_vars=[名稱]`＋值放 codex 行程 env，舊版／探測失敗逐位回退現行 argv 並 warn；機密判定＝`_API_KEY`／`_TOKEN`／`_SECRET`／`_PASSWORD` 精確後綴 | COMPLETE；三輪對照活測（第 5 輪拿掉 `env_vars` 值即不到、第 6 輪外部 `ps` 抓 argv 只見名稱），9 條測試；0.156.x 未實測（版本閘即為此而設） |
| W2-D | `[capabilities]` 整段納入 hook 凍結（鍵聯集 diff，未來新增鍵自動受保護；`agent_update`／dashboard 不經此 hook 有三處證據）；`plan_round` 計入 verifier（`MIN_ROUND_SPAWNS` 2→3，**行為變更**）；每輪重檢凍結家族與 allowlist；`RoundAdmissionGuard` 六種離開路徑清票券；`try_admit`／`dequeue` 標 deprecated；ledger 輪轉後仍讀 `.old`；`AlreadyFrozen` 回填 | COMPLETE，8 條先紅後綠；4 個 `plan_round` 既有測試的錯誤成本模型數字修正並說明 |
| W2-E | never-trim 改來源綁定：`duduclaw-core/protected_section.rs` 行程級 32-byte 隨機 sentinel，標題行下一行 `<!-- ddc-protected:<hex> -->` 才豁免；發出端只有 composer 封包渲染；`compose_summary` 剝除；三個失效方向全倒向不保護；標題常數下沉 core，`ccr.rs` 第二份清單收斂並由 `CcrRuntime::with_protected_sentinel` 傳入 | COMPLETE；使用者訊息裡 `## Constraints` 現在正常壓縮且不進 summary；既有 `a_user_authored_protected_section_cannot_disable_the_pipeline` 斷言改為新語意 |
| W2-F | `engine_matches_current` 接進 outcome fit／screen、candidate KPI DTO 與三個 CLI 輸出外層 `{record, engine_matches_current}`（不進雜湊 payload）；brief 五個誤差和改 decimal string 與 report 一致；`decision_forecast_dashboard.rs` 註解誠實化；前端 badge＋i18n 1 鍵 | COMPLETE／PARTIAL：`BriefKpis`／`BriefDelta`／`BriefEventKpis` 的 u64／i128 與 `sla_holdout.diagnostic` 未轉（上限約 5.7e14 遠低於 2^53，列欠帳） |
| W2-G | clap `Commands` 以 `#[command(flatten)]` 拆成 9 個連續區段子 enum（純前綴分群會改 help 順序，故依區段），CLI 表面逐位不變 | COMPLETE；208 個 `--help` 畫面前後 md5 相同、行為探針相同；2 MiB 堆疊下全量 1,612 通過並新增兩條釘住 2 MiB 的回歸測試；`run_on_big_stack` 保留為縱深防禦 |
| W2-H | 12 條留項固化進 `docs/todo/TODO-reversible-context-causal-simulation.md`「Engineering debt from the 2026-09-28 review」 | COMPLETE |

**協調者裁定（agent 提出、我決定）**：`"unknown"` 改空字串對 `blocked_users` 含字面值 `unknown` 的冷門設定不再命中 → 接受、文件化；`system` 納入 placeholder → 接受；bracket 啟發式對以 `]` 收尾的 log 仍保守拒壓 → 接受；W2-B 動 `mcp.rs` 與 100 ms reactor 阻塞 → 接受（reactor 阻塞列欠帳）；home 級 epoch 讓全體交付失效的保守選擇 → 接受；`MIN_ROUND_SPAWNS` 2→3 → 接受；deprecated 而非刪除 → 接受；委派信封 `## Constraints` 不給豁免、單一剝除點、子行程不共用 sentinel → 接受；CLI 外層包裹形狀 → 接受；brief 其餘欄位不轉 → 欠帳。**Docker Desktop 重啟未由我執行**（GUI 應用且可能承載使用者的實驗容器）。

**第二波最終串行全量驗證**：見「10.」。

## 10. 第二波後最終串行全量驗證（協調者實跑，所有 agent 收工後）

| 項目 | 結果 |
|---|---|
| `cargo check --workspace --all-targets` | PASS（1m48s） |
| `duduclaw-core --lib` | 733 passed / 1 ignored |
| `duduclaw-llm --lib` | 268 passed |
| `duduclaw-cli-runtime --lib` | 104 passed |
| `duduclaw-agent --lib` | 並行首跑 248/2 failed（同兩條 credential 時序測試）→ `--test-threads=1` 250/0 實錘純負載；兩測試改為以呼叫前後兩次讀鐘夾住預約時間（不再依賴 5 秒固定容忍）→ 並行重跑 **250 passed / 1 ignored** |
| `duduclaw-security --lib` | 359 passed |
| `duduclaw-memory --lib` | 363 passed |
| `duduclaw-inference --lib` | 118 passed |
| `duduclaw-gateway --lib` | **7,142 passed / 0 failed / 12 ignored**（192s；含 doctor 測試，Docker 仍卡但 10 秒逾時生效） |
| `duduclaw-cli --lib` | **1,612 passed / 1 ignored**（clap flatten 後） |
| `web`: `tsc -b` | exit 0 |
| `web`: `vitest run` 全量 | **256 檔 2,055 passed / 0 failed** |

工作樹：633 個變更項（109 個 untracked，含本 session 新增的 `decision_sla_shadow_dashboard.rs`、`protected_section.rs`、`DecisionSlaShadowWorkflowPanel(.test).tsx`、兩份 wiki 報告與 L3 證據）。全部**未 commit**。仍無活體驛證（未起 gateway、未走真通道／真瀏覽器）。

## 11. 第三波：欠帳清償（同日，使用者指示「將欠帳完成」）

| 代號 | 項目 | 結果 |
|---|---|---|
| W3-1 | `CcrStore::open()` 版本短路＋到期清理 60 秒節流、純讀不取寫鎖；`still_valid()` 改 async（40 個呼叫點全走 `spawn_blocking`，`dyn Fn` 回呼兩處保留 blocking 版）；`find` 加 `idx_ccr_scope`、CTE 物化排序整數（每列全文掃描 17→約 9 次）、每 tool loop 上限 8 次；撤銷回呼集中在 `GuardedReply`（重驗觀察到租約消失即回退該回合＋`Drop` 兜底，adapter 零改動）；日誌改記 handle digest；三個零呼叫端 legacy wrapper 移除 | COMPLETE；兩條先紅後綠；`lower(original)` 未物化全文（記憶體換 CPU 的取捨，接受） |
| W3-2 | `CausalStore::open()` 版本短路（多一道 `causal_artifacts` 存在守門，因 `memory.db` 共用）、維護節流 30 秒（CCR 投遞 `still_valid()` 仍每次讀活檔，專測鎖住）、三條多列讀取共用連線；孤兒 `.ccr-leases/*.lock` 回收；`clear_revocation_fence` store＋admin HTTP＋CLI（只還原未完成撤銷，活躍租約回 Conflict）；`retention_at` 無到期回 `null`（wiki 與 memory 來源） | COMPLETE；9 個既有測試插入 `reset_maintenance_throttle()` 斷言未改 |
| W3-3a | `record_role_usage` 逐維飽和累加＋`usage_legs`；`strip_workspace_prefixes` 只在路徑 token 邊界剝除（CJK 安全）；封包 `artifacts` 逐筆帶狀態渲染給 verifier；eval `copy_tree` 三上限、粗估 WARNING、唯讀 DB；`conditioned_on` 欄位 | COMPLETE；連帶 `failover.rs`／`claude_runner.rs` 各補 `usage_legs: None` |
| W3-3b | `Scope::TeamHandoff`（第 25 個 scope，不可外部授予；內部 key 走 admin 代位故零重發，避免舊版 binary 讀到未知 scope 清空整把 key）；153 處 `agents/` join 盤點（生產碼 37 處，13 處 (a)(b) 改走 `.ephemeral/` 解析，表在交接文件 §7.1）；**順帶修真 fail-open**：PORTICO `scoped_tools` 閘用 `client_id`（正式路徑恆為 gateway-internal）從未生效，改用 `gate_agent`——**行為變更**：設 `scoped_tools` 的 agent 從此真的被擋，需先 `capability_request` | COMPLETE；8＋2 測試，5 條先紅 |
| W3-4 | brief 其餘大整數全部 decimal string（`SlaHoldoutDiagnostic` 重用 `DecisionSlaHoldoutReport` 作 wire-mirror，`replay_hash` 轉換前算；測試實錘舊形狀超過 u64 時 serde_json 根本送不出去）；UCCI shadow 改背景 `tokio::spawn`（inline 版測試會卡死到 timeout）、升級 log 補 router／tier／margin | COMPLETE；`route_and_generate` 接收型別改 `&Arc<Self>`（唯一呼叫端本就持 Arc） |
| W3-5 | 三語系 82 個鍵去內部詞；審批卡 Decision Lab 深連結非管理員改提示 | COMPLETE；8 個測試檔 147 項通過 |
| 收尾 | `mcp_db.rs`／`mcp_planner.rs`／`mcp_recording_distill.rs` 同族 join 改走 `.ephemeral/` 解析（5 條測試）；UCCI `flush_shadow_observations()` 接進 `server.rs` 關機序列（prediction flush 之後、worker 之前，`bounded_step` 5 秒）；四處過時註解（多掃到 `webchat.rs` 兩處）；spec 交叉引用 | COMPLETE |

## 12. 第三波後最終串行全量驗證（協調者實跑）

| 項目 | 結果 |
|---|---|
| `cargo check --workspace --all-targets` | PASS（1m58s） |
| `duduclaw-core --lib` | 735 passed / 1 ignored |
| `duduclaw-llm --lib` | 275 passed |
| `duduclaw-cli-runtime --lib` | 104 passed |
| `duduclaw-agent --lib` | 250 passed / 1 ignored（並行，夾鐘寫法後穩定） |
| `duduclaw-security --lib` | 359 passed |
| `duduclaw-memory --lib` | 371 passed |
| `duduclaw-inference --lib` | 120 passed |
| `duduclaw-gateway --lib` | **7,155 passed / 0 failed / 12 ignored**（154s，含 doctor；Docker 已重啟正常） |
| `duduclaw-cli --lib` | 首跑 1,632 passed / 1 failed：`mcp_auth::tests::test_catalog_scopes_match_tool_requires_scope`——W3-3b 改閘門 scope 但 `duduclaw-core/tool_catalog.rs` 的目錄宣告仍 `memory:write`（這個漂移守衛正是為此而設）；協調者改目錄為 `team:handoff` 後重跑 **1,633 passed / 0 failed / 1 ignored**；`core tool_catalog` 11 passed、`gateway known_mcp_scopes` 2 passed |
| `web`: `tsc -b` | exit 0 |
| `web`: `vitest run` 全量 | **256 檔 2,057 passed / 0 failed** |
| i18n 三語鍵集 | 6,378 ／ 6,378 ／ 6,378，完全一致 |

工作樹 634 個變更項（109 untracked）。全部**未 commit**。無活體驗證。`target/` 已達 171 GB，建議清 `target/debug/incremental`。

**第三波後剩餘欠帳**（於第四波清償，見 §13）：verifier 路徑重複算一次 sha256；`capture_delivery_guards` 仍同步；UCCI shadow 無並行上限；`find` 未物化小寫全文；`ccr_find_rate_limited` 未進資料表；`CausalCurationPage.tsx` 硬編碼英文。刻意不做：`team:handoff` 不進 dashboard scope picker（內部專用）。

## 13. 第四波：殘餘欠帳清償＋target 清理（同日，使用者指示）

| 代號 | 項目 | 結果 |
|---|---|---|
| D1 | `team_composer`：單次走訪同時產出收據與 `ArtifactVerdict`，`render_packet_for_prompt` 只查表零檔案 I/O（測試先刪檔再改檔證明未重讀）；封包讀取／驗證迴圈移入 `spawn_blocking`（panic 降級為保守方向）；順手修「絕對路徑宣告永遠填不到 sha256」的既有缺陷 | COMPLETE（91 測試） |
| D2 | `capture_delivery_guards` 改 async（四個呼叫端，含 W3-1 漏列的 `channel_reply.rs:11467`；測試實錘改回 blocking 版會 FAIL）；`find` 拆兩層 CTE 每列 `lower()` 一次（精確片語仍比原欄位，結果逐位不變）；`ccr_find_rate_limited` 落表，`SCHEMA_VERSION` 1→2 帶條件式 ALTER，dashboard 對未升級舊庫回 0 | COMPLETE |
| D3 | `[router] ucci_shadow_max_inflight`（預設 1，0 鉗 1）以 `Semaphore` 限流，達上限不 spawn 不阻塞並計數；兩條並行測試證明「同時跑」 | COMPLETE（inference 125） |
| D4 | 四個因果頁面 211 鍵全走 i18n、三語一致、去內部詞；`App.routes.test` 一處字面中文斷言改查表 | COMPLETE（66 測試） |
| 清理 | `target/debug/incremental` 46 GB 刪除＋`tmutil thinlocalsnapshots`：target 174→138 GB、可用 41→77 GiB（exo 斷路器擋了兩次，使用者放行 `g-ab746193` 後以逐字相同指令通過） | COMPLETE；整包 `cargo clean` 於最終驗證後執行（見下） |

**第四波後最終串行全量驗證（協調者實跑，無 incremental 快取）**

| 項目 | 結果 |
|---|---|
| `cargo check --workspace --all-targets` | PASS（10m10s，非增量全編） |
| `duduclaw-core --lib` | 735 passed / 1 ignored |
| `duduclaw-llm --lib` | 277 passed |
| `duduclaw-cli-runtime --lib` | 104 passed |
| `duduclaw-agent --lib` | 250 passed / 1 ignored |
| `duduclaw-security --lib` | 359 passed |
| `duduclaw-memory --lib` | 371 passed |
| `duduclaw-inference --lib` | 125 passed |
| `duduclaw-gateway --lib` | **7,162 passed / 0 failed / 12 ignored**（165s） |
| `duduclaw-cli --lib` | **1,633 passed / 1 ignored** |
| `web`: `tsc -b` | exit 0 |
| `web`: `vitest run` 全量 | **256 檔 2,057 passed** |
| i18n 三語鍵集 | 6,589 ／ 6,589 ／ 6,589，完全一致 |

工作樹 634 個變更項（109 untracked），全部**未 commit**，無活體驗證。

**協調者裁定**：verifier 路徑重複算一次 sha256（列後續欠帳）；verifier 格 `conditioned_on="solo"` 接受；`usage_legs`＝有回報用量的段數（文件化）；`find` 物化方式與常數不進設定（接受）；`ccr_find_rate_limited` 不進資料表（接受）；有租約回合的蒸餾判定延後到回覆 drop 時、ACP 拒絕文案也回退該回合（接受）；`capture_delivery_guards` 仍同步（每 loop 一次，非每段，列欠帳）；shadow 並行上限不設（列欠帳）；`team:handoff` 不進 dashboard scope picker（接受）。**Docker**：使用者點啟動無效是因舊 `com.docker.backend`（PID 898，2 天 5 小時）卡死仍在，graceful quit 與 SIGTERM 無效，協調者 SIGKILL 後重開，daemon 29.5.3 約 5 秒就緒，`duduclaw-lwm-exp` 容器隨之重啟 healthy。
