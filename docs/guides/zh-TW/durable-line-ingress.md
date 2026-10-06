# LINE 收件持久化與故障復原

LINE webhook 驗簽成功後，閘道先把整批事件寫入 SQLite 並 commit，才回 HTTP 200。重送、重新啟動或不同傳輸路徑收到同一事件，都共用這份收件紀錄。處理事件仍走既有回覆、存取控制、聊天命令與工具閘門。

## 先確認兩件事

1. **直接 webhook 要在 LINE Developers Console 開啟 webhook redelivery。** 閘道寫不進收件匣時回 503，期待 LINE 稍後重送。redelivery 沒開，LINE 收到 503 就不會再送，那則訊息等於遺失。
2. **relay 路徑不保證可靠收件。** 走 `duduclaw-relay` 的部署（DuDuClaw OS 等），relay 收到 LINE 請求時就先回 200 給 LINE，之後才轉給閘道。閘道這端若因磁碟滿、收件匣打不開、停用開關關閉或設定讀不到而沒收下，事件就消失了，LINE 也不會重送。閘道會把這種情況計入 `relay_frames_total{channel="line",outcome="not_accepted"}`（簽章錯誤是 `bad_signature`），並寫一筆 Activity Feed `relay_line_rejected`（同一類每十分鐘最多一筆）。需要可靠收件的部署請用直接 webhook。

## 設定

```toml
[channel_ingress]
line_enabled = true          # 停用開關，預設 true
line_late_reply = "push"     # "push"（預設）或 "fail"
line_workers = 8             # 一般 worker 數，1～64，預設 8；重啟閘道後生效
retention_days = 90          # 已結束事件的保留天數，最少 1
stuck_alert_minutes = 15     # 對話卡住多久發告警，0 關閉
capacity_alert_mb = 512      # 資料庫＋WAL 超過多少 MB 發告警，0 關閉
```

除了 `line_workers`，其餘設定在下一次讀取時生效，不必重啟。`config.toml` 讀不到或無法解析時，worker 一律停派（視同 `line_enabled = false`），webhook 回 503。

### 停用開關 `line_enabled`

設成 `false` 後：新的 webhook 回 503，尚未開始的事件停在 `ready`，已在處理的回合跑完，但送出回覆或進度前會再檢查開關，關閉時不送。收件紀錄、去重紀錄和人工決策都保留，24 小時的 payload 清理照常進行。重新開啟後，仍有效的待辦繼續派工。

這個開關**不會回到 v1.69 以前的收件方式**，關掉就是 LINE 停擺。啟用可靠收件後和舊版的差異：

| 項目 | v1.69.x 以前 | 現在 |
|------|------|------|
| 回 200 的時機 | 驗簽後立刻回，背景處理 | 整批寫入 SQLite 並 commit 後才回 |
| 行程中途當掉 | 訊息遺失 | 已收下的事件重啟後繼續；可能已執行的標成 `uncertain` |
| 同一對話的多則訊息 | 各自並行 | 依收件順序一則一則處理 |
| 同時處理的對話數 | 不限 | `line_workers`（預設 8）＋一個決策 worker |
| 回覆 token 逾期 | 改用 Push | 依 `line_late_reply`，預設仍是 Push |
| 未設定 LINE 憑證時的 Verify | 200 | 503 |
| `config.toml` 讀不到 | 照常處理 | 503，停派 |
| 聊天命令與附件歸屬 | 主要 AI 員工 | 這則訊息被路由到的 AI 員工 |

### 回覆逾期 `line_late_reply`

LINE Messaging API 文件寫明：reply token 只能用一次，而且要在收到 webhook 後一分鐘內使用。閘道從「本機收件時間」與「事件的 `timestamp`」兩者較早的那個起算 60 秒。排隊或回合本身較久時，回覆期限會過。

**重送**的 webhook（`deliveryContext.isRedelivery = true`，只有開了 webhook 重送才會出現）一律當成已過期：LINE 說它的 token 可以用，除非原本那次已經用過或事件發生已超過 20 分鐘，而閘道無法確定第一次有沒有送到。所以這種事件的 token 一律不拿去試，直接照 `line_late_reply` 處理。Reply API 若回 HTTP 400、訊息為 `Invalid reply token`（已用過或過期），設為 `"push"` 時重新檢查後改用 Push；設為 `"fail"` 時事件標成 `undelivered`，原因 `reply_token_invalid`。其他拒收仍是 `undelivered`（`reply_rejected`）。

- `"push"`（預設）：期限過後改用 Push API，送給**原事件的同一個對話**（群組、聊天室或一對一使用者），不會送到別處。送出前做和一般回覆相同的檢查：處理租約仍有效、停用開關仍開、帳號憑證與路由授權版本沒變。請求帶 `X-Line-Retry-Key`，結果（含 LINE 的 `x-line-request-id`）寫進這次嘗試的回執，`delivered_via` 為 `push`。Push 會用掉官方帳號的訊息額度；免費或輕用量方案額度用完後 LINE 回 429，事件會變成 `undelivered`。
- `"fail"`：最後的回覆不改用 Push。worker 取到事件時若期限已過，**不執行回合**，事件直接標成 `failed_before_dispatch`、原因 `late_reply_expired`（重送的 webhook 為 `redelivered_reply_token_not_used`），並通知管理者。回合執行途中才逾期的，回覆不送，事件標成 `undelivered`。準時開始的回合仍會用 Push 送進度通知與核准／決定卡片，所以 Push 用量變少但不是零。代價是較久的回合使用者會收不到回覆。

## 接受事件的邊界

事件必須有 LINE `destination` 和 `webhookEventId`。同一簽章 envelope 的全部事件在一個 `IMMEDIATE` transaction 內寫入；任一失敗整批 rollback。SQLite 使用 WAL、`synchronous=FULL` 與五秒 busy timeout。200 代表這批事件已被持久接受，模型、工具或外部傳送是否成功另看事件狀態。

回 200 之前只做驗簽與寫入需要的檢查：讀一次 `config.toml` 取得憑證與停用開關、驗簽、解析 envelope、寫入。員工設定讀不到、某位員工的 `agent.toml` 壞掉、某個頻道設定值不合法，都不會讓 webhook 回 503。事件寫入時先記下「驗簽用的是哪一組憑證」，commit 後立即補上路由與授權快照。**快照補上之前，任何 worker 都不會領取這個事件。**讀不到快照時退避重試（5、10、20、40 秒）；連續 5 次失敗，或沒有快照的事件已超過 5 分鐘（例如中間閘道停了），事件就改成 `quarantined`，原因 `snapshot_unavailable`，不會默默改用之後的設定。這種事件從未執行，可以 `retry`；重試時以當下的設定補快照。補快照時發現憑證已換或路由已移除，原因是 `account_route_authorization_changed`。

穩定去重鍵是 `channel + account(destination) + webhookEventId` 的長度分隔 SHA-256。

初次執行的 `run_id` 等於 `ingress_id`。管理員明確 `retry` 或 `rerun` 時，同一交易保存新的授權 UUID 作為新 `run_id`；來源 `ingress_id` 不變。舊 worker 無法提交新 run 的回執。

## 路由與授權快照

派工前、送出回覆前和每次進度通知前，都重新計算快照並與事件保存的比對。快照只包含「這則訊息會交給誰、用什麼權限處理」：

- 路由：解析出的員工、它的 trigger／role／status、`[channels]`、`allowed_channels`、`default_agent`、這位使用者的員工綁定。
- 授權：LINE 憑證摘要；被路由員工實際生效的設定（綁了職務組合 preset 的員工讀 `agent_resolved/<id>.toml`，否則讀 `agent.toml`）中的身分欄位、capabilities、permissions、budget、container sandbox／network；這個對話與 global 的 allowlist、blocklist、pairing、admin、mention-only、binding 設定；這位使用者的配對狀態。

新增一位員工、修改無關員工的設定，不會改變快照。提示文字、SOUL、心跳與統計也不影響。

比對結果分兩種：

- **讀到了而且不同**（換綁、憑證輪替、權限改變、員工被移除）：事件 `quarantined`，原因 `account_route_authorization_changed`，不會用新帳號或新對象處理舊訊息。
- **讀不到或解析失敗**（設定正在被改寫、資料庫忙碌）：事件回到 `ready` 並退避重試（5、10、20、40 秒），連續 5 次仍讀不到才 `quarantined`，原因 `revalidation_unavailable`。這種隔離的事件從未執行，可以 `retry`。
- 送出最後回覆或進度通知前讀不到快照：在大約七秒內重讀幾次；仍讀不到就不送，事件標成 `undelivered`，原因 `revalidation_unavailable`。這不會被記成「授權已變更」（`authorization_changed_before_delivery`），後者只用在讀到了而且不同的情況。

## 狀態與人工處理

| 狀態 | 意義 | 是否擋住同對話後續訊息 | 可做的處理 |
| --- | --- | --- | --- |
| `ready` | 已接受，等待處理（可能在退避中） | 是 | |
| `claimed` | 已取得 90 秒租約，尚未執行；過期自動回 `ready` | 是 | |
| `dispatching` | 回合執行中，每 20 秒續租 | 是 | |
| `completed` | 回合跑完，回覆或 Push 已被 LINE 接受 | 否 | |
| `failed_before_dispatch` | 確定沒有執行（例如 `late_reply_expired`） | 否 | `close`、`retry` |
| `undelivered` | 回合已執行，回覆沒有送達（LINE 拒收、授權在送出前變了、逾期且設為 fail） | 否 | `close`、`rerun` |
| `uncertain` | 回合可能已執行，沒有送達回執（處理中行程消失、送出時連線中斷） | 是 | `close`、`rerun` |
| `quarantined` | 路由／授權已變、多次讀不到設定或快照、payload 過期，或從備份還原 | 是 | `close`；原因為 `revalidation_unavailable` 或 `snapshot_unavailable` 時另可 `retry`，為 `restored_from_backup` 時另可 `rerun` |
| `closed` | 管理者已結案，或保存期限到後由系統結案（`retention_closed`） | 否 | |

收件匣在 v1.69 之前沒有出貨過。預覽版建立的資料庫升級時：`failed` 改成 `undelivered`（那些事件都已執行過）；用舊格式快照保存的 `ready` 事件升級後會被隔離成 `account_route_authorization_changed`，只能結案。

處理動作：

- `close`：結案，保留紀錄，訊息內容與回覆 token 立即刪除。
- `retry`：只給確定沒執行過的事件，重新排入處理。
- `rerun`：給可能或確定已執行過的事件（`uncertain`、`undelivered`、因還原而暫停的 `quarantined`），**可能讓使用者收到重複回覆、重複觸發工具或重複建立任務**。必須帶 `confirm_duplicate_risk=true` 和理由；確認、理由與你核對過的 provider 回執編號（沒有就不填）會和新的 run 一起保存。

回覆 token 的時效從事件到達時開始算，重新執行不會延長它。所以：

- `line_late_reply = "push"`：重新執行（`retry` 或 `rerun`）的回覆一律改用 Push 送給原對話。
- `line_late_reply = "fail"`：事件已超過原回覆期限時，`retry` 和 `rerun` 直接拒絕，訊息是「回覆期限已過，目前設定為不改用 Push，重新執行不會有回覆；要送達請把 line_late_reply 設為 "push"，或直接結案」。這是為了避免跑完整個回合（工具、費用）卻確定送不出去。核准後才把設定改成 fail 的，worker 取到事件時一樣判定逾期、不執行。

這些動作都會重新通過原帳號／路由／授權快照檢查；快照變了一樣會被隔離。

### 告警

事件進入 `uncertain`、`quarantined`、`undelivered`、`failed_before_dispatch`（逾期），某個對話最早等待的訊息超過 `stuck_alert_minutes`，資料庫超過 `capacity_alert_mb`，或還原後暫停了事件，閘道會先把告警記進收件匣資料庫的佇列。每 30 秒依種類與原因、以十分鐘為一個時間窗彙總一次：

- 每個種類、原因、時間窗寫一筆 Activity Feed（`channel_ingress_uncertain`、`channel_ingress_quarantined`、`channel_ingress_undelivered`、`channel_ingress_late_reply_failed`、`channel_ingress_stuck`、`channel_ingress_capacity`、`channel_ingress_restored_held`），帶數量與前五筆事件編號；同一時間窗之後才到的告警，在時間窗結束時合成一筆補充；
- 透過主要 AI 員工的 `[proactive]` 通知對象推通知給管理者，同一類每十分鐘最多一則。

時間窗與推播的紀錄存在收件匣資料庫裡，重啟不會重複寫，也不會讓推播節流歸零。內容只有事件編號前 12 碼、狀態和原因代碼，沒有訊息內容、LINE 使用者 ID 或 token。告警指向下面的指令列與儀表板的待辦核准；**儀表板目前沒有收件匣頁面**（見「已知限制」）。

### 儀表板 RPC（Admin）

- `channel_ingress.list`：每頁最多 200 筆，回傳狀態、原因、退避時間、狀態數量、DB／WAL 位元組、目前設定與最近的嘗試和人工處理紀錄。`before_seq` 取更早頁面。不回傳 payload 或 reply token。
- `channel_ingress.inspect`：單一事件的嘗試（含 `provider_receipt`、`delivered_via`、進度推播統計）與重新授權紀錄（含 `action`、`confirmed_duplicate_risk`）。決策事件另以保存的 request ID 唯讀核對核准紀錄。
- `channel_ingress.resolve`：`ingress_id`（64 位十六進位）、`expected_revision`、`expected_attempt`、`action`（`close`／`retry`／`rerun`）、`note`、選填 `provider_receipt`、`rerun` 必填 `confirm_duplicate_risk=true`。每次呼叫（成功或被拒）都寫一筆安全稽核 `channel_ingress_resolution`。

這三個 RPC 每次都重讀使用者資料庫確認是 Admin，並與 LINE worker 共用同一個資料庫連線。

### 指令列

```bash
duduclaw ops channel-ingress list
duduclaw ops channel-ingress show <ingress_id>
duduclaw ops channel-ingress resolve <ingress_id> --note "理由" [--retry] [--provider-receipt <編號>]
duduclaw ops channel-ingress rerun <ingress_id> --note "理由" --confirm-duplicate-risk [--provider-receipt <編號>]
duduclaw ops channel-ingress batch --action close|retry|rerun --status <狀態> [--reason <原因代碼>] --note "理由" [--confirm-duplicate-risk] [--limit 200]
```

AI 員工在 Bash 裡執行 `duduclaw`／`duduclaw-pro` 的 `ops channel-ingress` 會被 agent-file-guard hook 擋下（`BlockedOperatorCommand`）。比對前先照 bash 的讀法還原指令（換行續行、引號拼接、黏在字後的重導向、`env`／`npx` 前綴、絕對路徑）。這只是減速：用變數、別名、腳本或改名的執行檔就能繞過，非 Claude 的 runtime 也不跑這個 hook；真正的關卡是下面的儀表板核准。

`list` 與 `show` 直接回答。`resolve` 和 `rerun` 第一次執行**不會生效**，只建立一筆核准請求，終端機印出「請到儀表板的待辦核准」與核准編號並以非零碼結束。指令列分不出下指令的是操作者還是有 Bash 的 AI 員工，所以這類變更一律要管理者（Admin）在儀表板核准；通道上的回覆不能核准。核准後 30 分鐘內再執行一次**同一個指令**（相同理由與回執編號）才會套用，每筆核准只能用一次；事件狀態在這之間有變動，核准就作廢，要重新申請。同時等待中的指令列請求最多 20 筆。請求等待期間事件狀態變了，等待中的卡片會被撤回，下次執行再建一張新的（不會原地改寫卡片內容）。每次申請、套用與拒絕都寫一筆安全稽核 `channel_ingress_cli_action`。帶有 AI 員工工作階段環境變數的行程通常會被拒，但拿掉這些變數就能繞過；真正的關卡是儀表板核准。急用時請改用儀表板 RPC。

v1.70.0 之後的版本起，所有操作者指令共用同一套核准規則（見〈[指令列的操作者動作共用一道核准](../../features/zh-TW/05-security-defense.md)〉）。LINE 收件匣因此多兩個上限：同一個事件最多 3 筆理由不同的請求在等；同一個事件（或同一個批次）每小時最多推播 2 次，超過的請求只留在儀表板待辦清單，並寫一筆安全稽核 `channel_ingress_approval_push_suppressed`。兩個終端機同時重跑同一個已核准的指令，只會套用一次，另一次被拒且不會另送請求。

`batch` 用一次核准處理一批事件：選出某一個狀態（`uncertain`、`quarantined`、`undelivered` 或 `failed_before_dispatch`）、可再限定一個原因代碼、而且這個動作適用的事件，最多 `--limit` 則（預設 200，上限 500），建立一筆核准，綁定動作、篩選條件、理由，以及每一則事件的編號與當時的狀態版本。管理者核准後，再執行一次同一個指令就套用一次：狀態沒變的事件照做，期間變動過的略過，輸出列出 `applied`、`skipped_changed`、`failed`。核准前篩選結果就變了，等待中的卡片會撤回並重建。批次 `rerun` 和單筆一樣要加 `--confirm-duplicate-risk`。

`list` 與 `show` 把 LINE 帳號與對話 ID 印成短摘要（`#` 加 12 個十六進位字元），同一對話的事件仍對得起來，但看不到 LINE 使用者或群組 ID。

## 操作者該知道的事

**順序規則。** 同一個帳號、同一個對話裡，一則一般訊息只要前面還有處於 `ready`、`claimed`、`dispatching`、`uncertain` 或 `quarantined` 的訊息，就會等著不處理（退避中的 `ready` 也算）。`completed`、`closed`、`undelivered`、`failed_before_dispatch` 不擋後面的訊息，所以一則訊息變成 `undelivered` 或 `failed_before_dispatch` 之後，同對話後面的訊息會繼續處理。`retry`／`rerun` 會讓事件以原本的收件順序回到 `ready`：它排在同對話所有還沒開始的訊息前面，但已經處理完的後續訊息不會重來，因此重新執行的回覆可能比後面訊息的回覆晚到。`uncertain` 和 `quarantined` 期間後面的訊息都在等，對這兩種事件 `rerun`／`retry` 後順序仍然正確；決策快線（核准、拒絕、回答）不排隊也不擋人，不同對話互不影響。

**從備份還原。** 裝置備份（`device.backup_create`、排程備份、`duduclaw export`）都會把 `channel_ingress.db` 一起打包。原裝置在備份之後可能已經處理過其中排隊中的訊息，所以裝置還原（`device.backup_restore`）**在搬動任何資料之前**先寫一個一次性標記；標記寫不進去，還原就中止並說明原因，資料不會換入。閘道下次開啟收件匣時，在任何 worker 取件之前，把備份當時處於 `ready`、`claimed`、`failed_before_dispatch`，或因可重試的原因（`revalidation_unavailable`、`snapshot_unavailable`）處於 `quarantined` 的事件全部改成 `quarantined`（原因 `restored_from_backup`），然後刪掉標記。這些事件在這台機器上從未執行過，但可能已在原裝置處理或重試過，所以不能用 `retry`，只能結案，或確認重複風險後 `rerun`。暫停時會寫一筆 Activity Feed（`channel_ingress_restored_held`）並通知管理者，告訴你暫停了幾則。備份當時處於 `dispatching` 的事件會變成 `uncertain`，同樣不會自動重跑。用 `duduclaw export` 打包、再手動解壓到新機器的做法不會留下標記，這種情況請先停用 LINE（`line_enabled = false`），檢查後再開啟。

## 進度通知與文件通知

進度通知（長回合中每分鐘最多一則 Push，即使訊息來自群組，也是送到發訊者的一對一聊天）是附屬訊息，不受 `line_late_reply` 影響。每次送出前重驗；結果另記在嘗試回執的進度統計，失敗不會讓已成功的回合變成 `uncertain`，也不會卡住對話。回合結束時最多等 10 秒收齊結果，沒回來的記為 unknown。

📎DELIVER 產生的「檔案已備妥，請到 Dashboard 下載」通知不另外 Push，直接附在這次回覆的文字後面，走同一條回覆（或逾期 Push）路徑、同樣的重驗與回執。

## 資料保存與容量

原始單一事件 JSON（含 reply token）存在獨立的 `ingress_payload` 表，最多保存 24 小時；事件 `completed` 或被 `close` 時立即刪除。仍在等待的事件 payload 過期後改成 `quarantined`（`payload_retention_expired`），只能結案。`undelivered` 與 `failed_before_dispatch` 事件的 payload 同樣 24 小時後刪除，之後就不能 `retry` 或 `rerun`，只能結案。

已結束（`completed`、`closed`）的事件，連同嘗試、回執、重新授權與人工處理紀錄，在 `retention_days`（預設 90 天）後刪除。`undelivered` 與 `failed_before_dispatch` 不擋任何訊息，超過 `retention_days` 後由系統結案（原因 `retention_closed`）並一起刪除。LINE 的重送發生在數小時內，90 天後刪除不影響去重。會擋住對話的 `uncertain` 與 `quarantined` 不會自動刪除。資料庫＋WAL 超過 `capacity_alert_mb` 時每天告警一次。容量用盡時來源得到 503，不會刪除仍需去重的紀錄。

資料庫檔權限為 0600；開啟時拒絕符號連結，且不會對既有資料庫檔做「開啟再關閉」（那會讓本行程在該檔的 POSIX 鎖失效）。SQLite `secure_delete` 與 WAL checkpoint 清除 live database 中已刪內容，不承諾刪除備份或外部副本。

## 已知限制

- **儀表板沒有收件匣頁面。** 事件在指令列檢視與處理（`list`、`show`、`resolve`、`rerun`、`batch`），在儀表板的待辦清單核准，或用 Admin RPC。事件多的時候請逐批處理。
- `X-Line-Retry-Key` 只在同一個 run 裡去重。`rerun` 是新的 run、新的 key，所以 `push_delivery_uncertain` 之後 LINE 分不出重新執行的 Push 和第一次的；重新執行前請先核對 provider 回執。
- Push 額度用完（HTTP 429）時事件變成 `undelivered`；回覆沒有保存，之後要補送就得 `rerun` 整個回合（模型與工具重跑）。
- `line_late_reply` 等設定以檔案的修改時間與大小快取。時間戳記精度較粗的檔案系統上，同一個時間刻度內改寫且長度不變（`"push"` ↔ `"fail"`）可能晚一點才讀到；下一次修改或重啟就會生效。
- 回覆是空的時候事件標成完成但不送任何東西（既有行為）。

## 驗證範圍

本地測試涵蓋：設為 fail 時逾期事件的 `retry`／`rerun` 被拒、核准後才改成 fail 的重新執行不會執行也不送出任何請求；Bash 通道擋下 `ops channel-ingress`；還原標記存在時排隊中的事件在 worker 取件前就被暫停、標記只生效一次；驗簽、整批 rollback／磁碟滿／資料庫唯讀／資料庫忙碌時不回 200；同一事件 20 個並發 webhook 只執行一次；排隊造成逾期時兩種設定（push 送到同一對話、fail 不執行也不送任何請求）；進度推播失敗不影響事件狀態；文件通知併入回覆；preset 生效設定進入快照；無關員工變更與損毀不影響快照及 ACK；讀不到設定時退避而非立即隔離；經過 webhook handler 與 worker 的實際 OS 強殺（ACK 後、執行中）；指令列需儀表板 Admin 核准、核准只用一次、狀態變動作廢；快照補上前事件不可領取、讀不到快照時退避、停機期間沒補到快照的事件隔離成 `snapshot_unavailable`；重送的 webhook 不試 reply token（push 時改用 Push、fail 時不執行）；reply token 被拒時 push 改用 Push、fail 維持 `undelivered`；送出前讀不到記成 `revalidation_unavailable`、讀到而且不同記成 `authorization_changed_before_delivery`；500 筆告警合成一筆帶數量的 Activity、重啟後不重複；批次只處理狀態沒變的事件、篩選結果變了就重建卡片；指令列輸出不含 LINE 原始 ID；標記寫不進去時還原在搬動資料前中止；Bash 通道擋下換行續行、引號拼接、重導向黏字的寫法。

真實 LINE 帳號的 redelivery、reply token 實際期限、token 已用過或過期時 LINE 實際回的錯誤內容（閘道比對訊息 `Invalid reply token`，其他 400 仍記 `reply_rejected`）、Push 額度用盡的回應、`X-Line-Retry-Key` 的去重效果，以及 relay 上游的行為，尚未在真實帳號驗證；本地測試以假的 provider 代替。
