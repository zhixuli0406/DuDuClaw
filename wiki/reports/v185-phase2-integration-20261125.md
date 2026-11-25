# v1.85.0 Phase 2：RM-062 合併與指定測試

本次只修改工作檔，沒有執行 Git add、commit、merge continue／abort，也沒有執行 cargo fmt。整合分支與提交由 orchestrator 管理。Git index 的 unmerged 狀態須由 orchestrator 處理。

## 衝突解決

24 個衝突檔案保留雙方行為：

- CLI：`mcp/recipient_gate.rs` 同時保留 RM-159 的 Google 快照收件人檢查與 Chat 空間收件人；`mcp_dispatch.rs`、`mcp_isolation_parity_g1_tests.rs`、`mcp_isolation_tests.rs` 的 MCP 總數固定為 **271**。
- Core：`google_identity/tools.rs` 合併三個新增工具與 write scope，原生 Google 工具固定為 **29**；`tool_effect.rs` 同時將 Gmail send 與 Chat send 分類為 Send，Calendar delete 仍為 Delete。
- Gateway：`google_personal/flow.rs` 同時接受聊天授權 request 與獨立 chat_send 選項，保留本人帳號綁定與增量授權檢查；`scopes.rs` 保留兩邊工具清單。
- Gateway：`google_workspace.rs` 保留三個新增工具的原生 REST 限制；`native_call.rs` 保留 RM-159 資源 ID 驗證與 Chat 參數解析；`native_contract_tests.rs` 固定為 **28** 個有原生 effect template 的工具（不含 google_status）；`native_program_tests.inc.rs` 保留雙方測試。
- 文件：英／繁中／日文的 `google-mcp.md`、`google-personal-connections.md`、`mcp-tools.md` 保留聊天授權連結、逐次確認寄信／刪除活動與本人 Chat 傳送說明；三語 `google-workspace.md` 與 `mcp-tools.md` 的 Google 數量同步為 29。
- UI：三語 JSON 均可解析、沒有重複鍵，且逐一比對 Git index 的 stage 2／3，雙方鍵皆保留；每份 **11,744** 個鍵。

## 編譯修正

- `google_workspace/native_program.inc.rs`：ChatSend 的 match arm 使用回傳 Result 的 async 區塊，讓 `?` 在正確回傳型別中傳播錯誤，外層仍回傳工具 JSON。
- `google_identity/effect_authority/native_operation.rs`：`ClosedNativeFailure::into_refusal` 可見範圍限於 `crate::google_identity`，供同模組下 Chat 傳送使用；拒絕與不確定結果的轉換不變。

首次 `cargo check --workspace --tests --offline` 發現上述錯誤；修正後第二次通過（7m29s，仍有警告）。所有首輪測試修正後，第三次相同檢查亦通過（9m48s，仍有警告）。逐項重跑結果待補。

## 驗證命令

所有 Cargo 命令均設置：

```sh
CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
```

stdin 為 `/dev/null`，使用 `--offline`。逐項測試為 `cargo test -p <crate> --offline --lib <完整名稱>`；Gateway 另加 `--no-default-features`。未執行完整測試套件、cargo fmt、Windows／Linux 原生驗收，亦未執行交辦指定略過的 history_privacy load flake。

## 逐項診斷與修正決策

| 測試 | 分類 | 診斷與處理 |
|---|---|---|
| `mcp::google_owner_client::tests::all_26_native_tools_are_in_owner_lane` | 測試 | 原生 Google 工具實際 29、舊預期 27；改為 29，逐工具 owner lane 檢查不變。 |
| `mcp_dispatch::google_tests::rm159_write_listing_and_dispatch_match_explicit_employee_scopes` | 測試 | 空 arguments 先遭必要參數驗證拒絕；補合法參數，仍要求 read-only 專用 key 無法列出／呼叫 write 工具。 |
| `pack_cmd::tests::install_pack_produces_the_same_tree_as_the_legacy_expert_pipeline` | 測試 | D12 為各安裝產生不同 authority epoch；先驗證 namespace 與磁碟 epoch，再只正規化隨機 epoch，檔案集合與其餘內容仍相等。 |
| `agent_channels::e2e_gchat_tests::own_app_requests_queue_in_order_while_other_apps_keep_running` | 環境 | 本機 TCP listener bind 被沙箱以 Operation not permitted 拒絕；未進入行為驗證，不修改、不重試。 |
| `google_identity::effect_authority::tests::rm159_closed_owners_require_a_fresh_snapshot_card_before_any_send_or_delete` | 環境 | 本機 TCP listener bind 被沙箱以 Operation not permitted 拒絕；未進入行為驗證，不修改、不重試。 |
| `google_identity::owner_tests::all_25_effect_tools_have_a_closed_native_template` | 測試 | 缺少 gmail_send_draft／calendar_delete_event 測例；補齊並固定 28 個 effect template（29 個原生工具減 google_status）。 |
| `google_workspace::native_call::tests::parse_goldens_preserve_trim_number_boolean_and_cell_rules` | 測試 | 新增工具造成陣列位置偏移；改按工具名稱選取測例，保留所有 trim／number／boolean／cell 斷言。 |
| `google_workspace::native_confirmation::tests::allow_and_admin_do_not_remove_new_single_use_card` | 測試 | strict policy snapshot 現在要求 active 員工；fixture 明確設 status=active，仍驗證 allow／Admin 無法免除單次確認。 |
| `google_workspace::native_confirmation::tests::block_still_blocks_before_request` | 測試 | 同一 fixture 缺 status=active；補齊，仍要求 block 在建立 approval 前拒絕。 |
| `google_workspace::native_confirmation::tests::host_context_refuses_missing_incarnation_or_policy_changes` | 測試 | 同一 fixture 缺 status=active；補齊，仍檢查政策改變與缺少員工生命週期時拒絕。 |
| `handlers::personal_data_gate::p1e_tests::p1e_every_dashboard_arm_is_gated_or_allowlisted` | 測試 | 新 seat-admission wrapper 將參數讀取移至下一層 helper；靜態掃描遞迴追蹤 self helper 並防止循環，沒有新增豁免或刪除安全斷言。 |
| `handlers::tests::agents_create_name_collision_tests::agents_create_fails_when_the_authority_refuses_and_leaves_no_directory` | 產品 | 發布前拒絕被誤當成已發布，對損壞 org 再次回滾而回錯誤；依實際目錄是否發布決定清理，保留 org 原始位元組及無殘留斷言。 |
| `handlers::tests::agents_main_commit_tests::update_and_resume_recheck_live_admin_role_inside_commit_guard` | 產品＋測試 | 權限唯讀查詢不再開啟會取得 seat lock 的 migration 入口；測試 ownership 在啟動 pending write 前建立，仍於 commit guard 前降級即時角色。 |
| `handlers::tests::agents_main_commit_tests::update_refuses_main_or_default_promoted_after_early_admission` | 產品 | system-operation 權限查詢改用既有唯讀 UserDb，避免已持有 seat lock 時再次 migration；保留 promotion 後整筆寫入拒絕斷言。 |
| `handlers::tests::credential_protection_rpc_tests::missing_home_below_symlink_ancestor_keeps_the_store_after_enable` | 測試 | D12 要求 home 已存在；先完成 lookup 不建立任何目錄的斷言，再以 owner-only DirBuilder 建立 home，維持相同 store 與 key 驗證。 |
| `handlers::tests::credential_protection_rpc_tests::missing_home_lookup_is_read_only_and_first_enable_keeps_the_store` | 測試 | 同上；沒有放寬 authority namespace 的擁有者、權限或 symlink 檢查。 |
| `handlers::tests::credential_protection_rpc_tests::missing_parent_before_existing_symlink_keeps_the_store_after_enable` | 測試 | 同上；保留包含缺失祖先與 symlink 的路徑／實體 store 一致性驗證。 |
| `handlers::tests::must_change_password_gate_tests::flagged_caller_can_change_password_and_unlock` | 測試 | fixture helper 返回時刪除了 TempDir；將 guard 返回給測試保活，仍驗證強制改密碼與解鎖。 |
| `handlers::tests::org_change_rpc_tests::main_create_failure_restores_previous_main` | 產品 | 建立失敗的假已發布狀態導致提前返回、未恢復舊 Main；修正清理判定且清理失敗仍執行 Main 恢復。 |
| `handlers::tests::users_change_email_tests::avatar_seats_create_when_full_recount_and_refuse_exemption_loss` | 產品 | users transaction 內 org policy fence 開第二個 writer，造成 database is locked；在持有 org lock 時先寫入 org／durable fence，transaction 內相同 upsert 為 no-op，失敗仍補償。 |
| `isolation_connector_clients::tests::isolated_native_connector_support_is_exact_and_has_no_other_provider_fallback` | 測試 | 三個新增 Google 工具使原生 connector 支援數由 35 增至 38；未知名稱與其他 provider 不 fallback 斷言不變。 |
| `isolation_lifecycle::tests::restore::adopt::f6fb::f6fb_adoption_enforces_agent_cap` | 測試 | 名額改以實際員工設定計算；舊 fixture 只存 allocation，補上 bob-desk 的實體員工樹，保留 approval／apply 兩階段超額拒絕。 |
| `isolation_lifecycle::tests::restore::avatar_restore_when_full_preserves_verified_provenance` | 產品 | 還原持有 authority writer 時 nonce rotation 再次取同一 lock；先執行自帶 lease 的 nonce rotation，再持有 writer lease 完成 users transaction，保留 provenance 與名額驗證。 |
| `machine_employee::tests::server_resolved_preset_limits_binding_tasks_goals_and_callable_list` | 產品 | Admin 權限查詢重入 seat-admission lock，等待後被錯誤拒絕；改為唯讀查詢，原有非 Admin 拒絕與 Admin 成功斷言不變。 |
| `runtime::codex::third_party::tests::overrides_keep_upstream_credentials_out_of_argv` | 測試 | env_vars 新增非秘密 DUDUCLAW_IDENTITY_SERVER 名稱；更新完整列表，所有秘密不得進 argv、檔案權限與清理斷言不變。 |
| `session_origin::tests::every_messaging_channel_and_its_buckets_are_channel` | 測試 | RM-182 要求完整合法 Google Chat space／thread key；更新 canonical fixture，並明確斷言舊 googlechat:chat-1 為 Unknown。 |
