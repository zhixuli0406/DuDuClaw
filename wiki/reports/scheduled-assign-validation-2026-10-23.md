# 排程交辦驗證紀錄（2026-10-23）

L2 開發紀錄。功能分支：`feat/scheduled-assign`。最新程式快照 `167eaabb` 已接上 main `1cc573e5`；較早的驗證快照分別列出，沒有混用。尚未發版。

## 獨立審查

- P1：暫停／刪除與任務建立競爭。已把 active slot CAS、任務插入、排程推進放在同一個 `BEGIN IMMEDIATE` 交易；共用的目標建立核心在 plan-first 等待後才做最終插入檢查。加入不同連線的 stale due snapshot（goal／board × pause／remove）與交易回滾測試。
- P2：提高最短執行間隔後，舊排程未重驗。執行與恢復均重讀目前限制，不符合就暫停或拒絕恢復。
- P2：設定讀取／解析／型別錯誤會回到較寬鬆預設。現在只有不存在的檔案／鍵才有預設；錯誤阻止新增、恢復及執行，仍可列出、暫停及刪除。未知 limits 回傳 null，儀表板提供修復提示。
- 三項均經獨立 code-reviewer 靜態複審，未留下阻擋；實跑結果另列如下。

## 本機檢查

所有 cargo 透過原 compile-slot 腳本，`CARGO_INCREMENTAL=0`，使用此 worktree 自己的 `target`。最新全量使用 `CARGO_BUILD_JOBS=1`。

| 檢查 | 結果與來源範圍 |
|---|---|
| 承接紀錄的 gateway 全量 | 原紀錄 9216 passed、1 failed（未改動的 discovery 容器清理 timeout）；相同承接快照本輪重跑 lib 9217 passed，整體含 integration／doctest 共 9343 passed，exit 0。這是審查修補前的基準結果。 |
| P1／interval 修補後定向測試 | `cargo test -p duduclaw-gateway --no-default-features --lib assign_schedule`：35 passed；尚不含後加的壞設定回歸。 |
| 最新 gateway 全量 | caps 錯誤拒絕修補後，lib 9222 passed、0 failed、46 ignored、32 filtered；含 integration／doctest 共 9348 passed，exit 0。編譯 14m06s，lib 執行 297.91s。 |
| clippy | 原始命令 exit 101：兩個未修改的既有 fixture 觸發 `clippy::async_yields_async`（`channel_reply/tests/ccr_guarded_reply.rs:125`，原 commit `d78ed07a8`；`wiki_ingest.rs:5047`，原 commit `fb0c26ee4`）。只放寬這個 lint 後 exit 0（`-- -A clippy::async_yields_async`，4m20s）；既有 313 warnings 保留，檢查未見警告／錯誤指向本次新增行。 |
| Web | `npx tsc --noEmit`、10 個定向測試檔 54 tests、`npm run build` 通過。含新增排程後同頁更新、壞設定仍保留縮權控制與三語文案。 |
| main 整合後定向回歸 | 最新 `167eaabb`：assign_schedule 38、schedule_chat 5、goal_create_core 5，共 48 passed；含 completed history 不得擠掉 active／paused 控制的最終 SQL 回歸。測試與 shipping CLI 重新建置均 exit 0，DEBUG0、保留 debug assertions。 |

gateway 全量使用專案指定的略過清單：

上述全量與 clippy 快照完成後，另補「200 筆 completed history 不應擠掉 active／paused 控制」的 SQL 排序與回歸；後者已由最新 48 項定向測試驗證，不能用較早的 9348 passed 代替。

```sh
cargo test --no-fail-fast -p duduclaw-gateway --no-default-features -- \
  --skip three_page_pilot_ --skip security_race_tests --skip resume_tests \
  --skip f1b_e2e_tests --skip f5a_e2e_tests
```

## 隔離 gateway 活測

第一輪 probe 使用 `tasks.list` 而未帶 `agent_id`，被既有 API 契約拒絕；修正 probe 為 `tasks.list_page`，另建乾淨 home，保留第一輪紀錄，沒有覆寫或刪除。

成功輪使用標準 `make-home.sh`，loopback 18957、獨立 OS home、production-shaped 員工；停用 dispatch／proactive，不使用操作者登入或真模型額度。

- 實際 HTTP 登入與 WebSocket RPC 建立一次性 board task／goal 排程，設定當下沒有任務。
- 等實際 90 秒到期與 gateway 的 30 秒 scheduler tick，各產生一筆任務，`fire_count = 1`，建立者與 dashboard 來源正確。
- peer 列不出或暫停別人的排程；pause／remove 的排程未執行；撤綁者自動暫停為 `access_lost`。
- 實際 `/ws/chat` 完成 `/交辦` 排程建立、列出、暫停與刪除；記錄來源為 webchat。
- 證據：本機 `/private/tmp/dudu-b6-live-fixed.cuhrBo/evidence.json` 與同目錄 `tasks.db`／`gateway.log`。gateway 已停止，資料保留。
- 此輪 binary SHA-256：`c366d198313df2344c5dc1fbb212f84489f770bb0c9bbee4c3694afd2dff836b`。包含 slot fence 與 interval 修補；**不含後加的 caps 錯誤拒絕修補**，因此不作為後者的活測證據。

最新 caps binary 活測亦已完成，使用同一隔離 home、重新編譯的 `167eaabb` shipping CLI，SHA-256 `f44085baf3308482f53ced9242fd033fe4ea95d404e27b020cb2dfaaef7a52d7`：

- 先建立本人 board 排程、暫停中的排程及主管代部屬建立的 goal 排程；在真實到期前，把最短間隔設定改成錯誤的布林型別。
- 新增／恢復均被拒、limits 回 null。跨過 14:12 UTC 到期與真 scheduler tick 後，兩個 active 排程均沒有執行或推進。
- 修復為整數 60 後，14:13:03 UTC 的實際 tick 各建立一次本人任務／主管 goal，`fire_count=1`；主管的 `on_behalf_of` 保留實際主管 user id，暫停項目仍不執行，控制恢復後刪除這輪測試排程。
- `caps-state.json` 為 `status=PASS, stage=recovered-pass`。原始失敗 probe、較早證據與新 `caps-{prepare,blocked,recovered}-final.log`／`gateway-final.log` 全部保留；這不是模型執行或外部通知驗證。

## 驗證邊界

未驗證真模型工作結果、外部通道通知送達、瀏覽器實際點選、原生 Linux／Windows、正式部署與使用者親測。CI 結果與合併／發布決定另由 PR 流程確認。
