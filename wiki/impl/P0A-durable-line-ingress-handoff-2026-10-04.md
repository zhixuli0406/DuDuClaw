# P0-A LINE 收件持久化交接

狀態：LOCAL_VERIFIED / REVIEW_PENDING。程式、測試、產品文件與 source manifest 已凍結；最後 source 的本地 gates 通過，獨立複審、跨包整合與真 LINE 帳號驗收尚未完成。沒有 commit、push、merge、release。

- Worktree：`/Users/lizhixu/Project/DuDuClaw-wt-p0-ingress`
- Branch：`fix/durable-line-ingress`
- Base：fetch 後最新 `origin/main=303860bd`
- 最新 source snapshot SHA-256：`6aea4d2b3cf835c31112d102659ef0a9d0f7187155c0533e34244d1547a054b4`
- Source snapshot 包含以下 9 個檔案（順序亦參與 hash）。產品文件和本報告不參與 source hash。

- `crates/duduclaw-gateway/src/channel_ingress.rs`
- `crates/duduclaw-gateway/src/channel_ingress/tests.rs`
- `crates/duduclaw-gateway/src/line.rs`
- `crates/duduclaw-gateway/src/line/ingress.rs`
- `crates/duduclaw-gateway/src/channel_settings.rs`
- `crates/duduclaw-gateway/src/handlers/channel_ingress_rpc.rs`
- `crates/duduclaw-gateway/src/handlers/mod.rs`
- `crates/duduclaw-gateway/src/handlers/dispatch_ops.rs`
- `crates/duduclaw-gateway/src/lib.rs`

## 實作契約

整個已驗簽 envelope 一個 SQLite FULL/WAL IMMEDIATE transaction，成功 commit 才 ACK。固定 event/account dedup 不含變動 route 或 authorization revision；舊事件在換綁、權限變更和 secret/token rotation 後仍是同一來源，不會新建第二個 source。兩個 bounded worker 共用 store CAS，conversation 的前序未終結即阻塞後序。

Inbox 是交接 ledger，處理仍進原本的命令、goal 與回覆引擎。ready/claimed 前 crash 可重試，dispatching crash 進 uncertain，禁止自動重送。每次 dispatch attempt 的 operation_id 是 lease UUID，source ingress_id 固定；不是工具每一步的完整 operation store。

`ingress_payload` 保存 raw event/token 24h，active dispatch 完成再清理；`ingress` tombstone 不自動刪。Unix 0600、secure_delete、WAL checkpoint。Admin list 有 pagination、各狀態計數和 DB/WAL bytes；磁碟滿拒 ACK。備份與外部副本不在本清理保證內。

Admin close 不宣稱成功，保留 note；retry 要明確 confirm_duplicate_risk、expected_revision 及 expected_attempt CAS，另建立新 authorization UUID、predecessor operation_id、actor/note。舊 uncertain receipt 保存在 UPDATE/DELETE 被禁止的 immutable ingress_attempts。quarantined 不能 retry。

## 獨立審查問題與修正

1. 初版 uncertain retry 會覆寫 source status 而沒有不可變執行證據。現在 terminal/crash receipt 同 transaction 存 append-only attempts，operator retry 另記授權 lineage；新 claim 新 UUID，原 unknown 仍可列出。回歸：`uncertain_retry_retains_immutable_attempt_and_new_authorization_lineage`，含 SQL 不可 UPDATE/DELETE assertion。
2. 初版唯一 drain inline await LLM，其他 chat 被阻塞。現在每 home 一組兩個 bounded worker，weak singleton 避免 direct/relay 開第二組。回歸：`slow_conversation_does_not_block_others_and_same_chat_stays_ordered`，以實際 drain 的本地 test hook 暫停 C1 E1，驗 C2 完成且 C1 E2 仍 ready，再解鎖驗同 chat 順序。
3. 既有 settings cache 可掩蓋外部行程 SQL 寫入。新 authoritative refresh 讀 SQL並更新同一 handler cache，in-memory fallback或read error拒 snapshot。回歸：`authority_snapshot_refreshes_external_sql_writes_and_ignores_prompt_changes`。
4. progress push 仍是外部動作。既有功能保留，但每次送出重驗有效 lease、account/route/authority；所有已開始的 push join回執才 terminal，未知進 uncertain。最後 reply expiry不轉 push。

## 最後快照驗證紀錄

2026-10-04 本機 macOS，`--no-default-features`、`CARGO_BUILD_JOBS=2`、測試單執行緒。程式快照為上列 SHA-256，後續只修改文件；沒有重複使用舊 source 的通過結果。

| Filter | Passed | Log |
| --- | ---: | --- |
| `channel_ingress` | 12 | `artifacts/p0a-tests/store-final.log` |
| `line::ingress::durable_ingress_tests` | 7 | `artifacts/p0a-tests/line-ingress-final.log` |
| `line::tests` | 35 | `artifacts/p0a-tests/line-existing-final.log` |
| `line::ccr_principal_tests` | 2 | `artifacts/p0a-tests/ccr-principal-final.log` |
| `relay_client::tests` | 10 | `artifacts/p0a-tests/relay-final.log` |
| `channel_settings::tests` | 28 | `artifacts/p0a-tests/channel-settings-final.log` |

完整命令均為 `CARGO_BUILD_JOBS=2 cargo test -p duduclaw-gateway --lib --no-default-features <filter> -- --test-threads=1`。94 次通過，0 failed／ignored；`line::tests` 是 substring filter，真正 LINE 測試 8 項，其餘 27 項是 online／pipeline，因此實際相關測試為 67 項。relay 包含真正本地 relay server roundtrip；不是雲端 provider 驗收。

- Session `26906` 初次最後編譯失敗：snapshot SQLite Statement 跨 `.await` 不符合 Send；已改成 lexical scope 後於 `80779` 編譯及 12 tests 通過。
- 派工 token 讀取與 snapshot 之間的 credential rotation 競態，補同版本 digest 核對後，session `44802` 編譯及新 LINE 7 tests 通過。
- Session `31287` 依序完成剩餘 filters，exit 0；store 亦已在最後 snapshot 重跑，最新 log 覆寫舊結果。
- 非本變更 warning：`duduclaw-fork/src/overlay.rs:192` 的 `TempDir::keep` unused return value。
- `git diff --check` 通過。Rust gate 已釋出。未執行整 workspace／default dashboard／Windows gates。

`artifacts/p0a-tests/patch-manifest.txt` 列出 13 個交接檔；`source-manifest.json` 記錄 9 個 source 個別 hash，`test-results.json` 記錄順序命令及 exit。artifacts 是本地 ignored 證據，不列入產品檔 copy。原始 source 新檔未追蹤，整合者必須依 manifest 複製，不能只用 `git diff`。

## P0-B 整合接點

P0-A 首 PR 保持獨立 build，不依賴尚未落 main 的 P0-B 類型。`line/ingress.rs` 的 drain 在 signed payload `destination`、解析過的 `LineEvent.source` 和重新驗證的 current access token 都在場時，將整個 `process_line_events` scope 到下列 P0-B API。

```rust
let decision_context = crate::approval::DecisionContext {
    channel: "line".into(),
    account_id: account_id.clone(), // Signed LINE destination, not token hash.
    conversation_id: line_conversation(&event),
    principal_id: event.source.as_ref()
        .and_then(|s| s.user_id.clone()).unwrap_or_default(),
};
let target = crate::approval::TrustedReplyTarget::new(
    decision_context, token.clone(), line_conversation(&event), None,
);
crate::approval::scope_trusted_reply(
    target,
    process_line_events(vec![event], &state, &token, &agent, &account_id),
).await;
```

上述 snippet 是待整合呼叫，尚未在 A source 編譯。需依 B worker 最終 enum/field API核對。缺 principal 時不取得 trusted target，普通聊天可繼續，CU 核准必須 fail closed。postback 帳號亦使用 signed destination，不由 bearer token推出。

## 尚待關閉

- 獨立複審確認兩项初審 blocker 的修正及回歸；母代理已安排，尚無通過結論。
- P0-B trusted scope 在母代理整合 source 後 build/test；不能把單包通過當跨包通過。
- 真 LINE redelivery、provider reply-token期限與 relay上游 crash/retry acceptance。沒有實際channel授權不借 production credentials。
- 四項 required PR checks、使用者驗收、merge/release決定。

## 凍結交接檔案清單

以下全部位於本 worktree，tracked 與 untracked 一併交接；沒有其他產品檔。

```text
crates/duduclaw-gateway/src/channel_ingress.rs
crates/duduclaw-gateway/src/channel_ingress/tests.rs
crates/duduclaw-gateway/src/line.rs
crates/duduclaw-gateway/src/line/ingress.rs
crates/duduclaw-gateway/src/channel_settings.rs
crates/duduclaw-gateway/src/handlers/channel_ingress_rpc.rs
crates/duduclaw-gateway/src/handlers/mod.rs
crates/duduclaw-gateway/src/handlers/dispatch_ops.rs
crates/duduclaw-gateway/src/lib.rs
CHANGELOG.md
docs/README.md
docs/guides/durable-line-ingress.md
wiki/impl/P0A-durable-line-ingress-handoff-2026-10-04.md
```

既有 `line.rs` 的 tracked diff 是 193 additions／201 deletions，大型新邏輯已抽到獨立 module；沒有全檔 rustfmt。沒有新增 dependencies。既有 progress push 與 guarded 文件交付保留；最後 Reply API 自動 fallback push 已移除並於產品指南、changelog 記錄。
