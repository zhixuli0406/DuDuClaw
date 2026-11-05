# AG-UI 前端端點

外部前端可透過 `POST /api/ag-ui/agents/{agent_id}/runs` 傳訊息給 AI 員工，並接收 AG-UI 1.0 事件串流。目前實作範圍是本文列出的 HTTP/SSE 子集，沒有支援全部 32 種事件。

狀態：尚未發布。第一輪 Daybreak 審查提出四項問題，本次 patch 已加入修補；Gateway 編譯、真實前端與模型供應商驗收、Daybreak 複審仍待完成。

## 傳送訊息

在 `Authorization: Bearer …` 放入儀表板帳號的 access JWT。匿名網站小工具、widget key 與舊式共用管理員 token 都不能登入此端點。帳號狀態、強制改密碼、憑證撤銷與既有雙因子政策仍適用。

將 `support` 換成帳號有權呼叫的員工。非 Admin 帳號須有既有員工綁定或有效團隊授權，且不能透過此端點呼叫主要／預設員工。Admin 可選擇已載入、可運作的員工。執行期間每 500 毫秒重新檢查 JWT 與員工權限；過期或撤銷時，進行中的串流以 `RUN_ERROR` 回報。

```bash
# 將一般登入取得的 access JWT 設為 DUDUCLAW_ACCESS_TOKEN。
curl --no-buffer 'http://127.0.0.1:18889/api/ag-ui/agents/support/runs' \
  -H "Authorization: Bearer ${DUDUCLAW_ACCESS_TOKEN}" \
  -H 'Content-Type: application/json' \
  -H 'Accept: text/event-stream' \
  --data-binary '{
    "threadId": "support-chat-1",
    "runId": "run-1",
    "protocolVersion": "1.0",
    "messages": [{"id":"user-1","role":"user","content":"請簡短介紹你能幫忙的事情。"}],
    "tools": [],
    "context": [],
    "forwardedProps": {}
  }'
```

同一段對話沿用 `threadId`，每次請求使用新的 `runId`。Gateway 依已驗證的呼叫者、員工與 thread 產生對話識別；另一個帳號即使猜到 thread ID，也不能取得相同歷史。這些 session 使用既有 WebChat 所有者命名空間中的獨立 AG-UI 區段，`/ws/chat` 不能續接。

端點只將最後一則 user 訊息交給既有 WebChat 回覆管線。內容可為字串或文字 parts 陣列。前端提供的 system 訊息、assistant 歷史、`tools`、`context`、`state`、`forwardedProps` 不會取代伺服器對話，也不會放入 prompt。這些傳輸欄位可以傳入，但不會註冊或執行前端定義的工具。只有員工實際要求既有 runtime 已提供的工具時，工具進度才會出現相符名稱；其他 client tool 宣告會被忽略。事件不回傳 client state 或 forwarded properties。

## 讀取串流

每個 SSE `data:` frame 是一個 JSON 事件。正常回覆順序如下：

```text
RUN_STARTED（protocolVersion: "1.0"）
[TOOL_CALL_START → TOOL_CALL_END，管線有提供工具進度時]
TEXT_MESSAGE_START
TEXT_MESSAGE_CONTENT
TEXT_MESSAGE_END
RUN_FINISHED（outcome.type: "success"）
```

目前回覆管線提供檢查後的最終文字，因此會在既有輸出及交付檢查後，以一個 content chunk 傳送。工具進度含實際觀察到的工具名稱與產生的 call ID；不提供原始參數或工具結果，也不虛構 `TOOL_CALL_ARGS` 或 `TOOL_CALL_RESULT`。是否有工具進度依員工使用的 runtime 而定。

JSON、輸入格式、登入或員工權限不符時，會在開啟串流前拒絕。`RUN_STARTED` 之後的錯誤用 `RUN_ERROR` 回報。HTTP 200 不代表工作成功；連線結束卻未收到終止事件，表示串流中斷。回覆管線回報的 runtime 失敗也會轉成 `RUN_ERROR`，不當成成功的文字回覆。

## 核准與續跑

只有屬於目前伺服器對話與回合的待核准要求，才會產生 `RUN_FINISHED`、`outcome.type: "interrupt"` 與 `interrupts` 項目。項目使用 `confirmation` 原因，帶有核准 ID、到期時間，以及能否透過此端點決定的 `metadata["duduclaw:canDecide"]` 資訊。中斷前會送出 state 與 message snapshot，內容是端點的續跑資訊，不會回送任意 client state。

回答時，在同一 thread 建立新的 run，並將所有未回答的中斷放入 `resume`。例如將下方 ID 換成剛收到的核准 ID：

```json
{
  "threadId": "support-chat-1",
  "runId": "run-2",
  "protocolVersion": "1.0",
  "messages": [],
  "resume": [{
    "interruptId": "approval-id-from-interrupt",
    "status": "resolved",
    "payload": {"approved": true}
  }]
}
```

拒絕使用 `{"approved": false}`；`status: "cancelled"` 也代表拒絕，且必須省略 `payload`。不支援 `editedArgs`。缺少、多出、重複、過期或不屬於此回合的中斷 ID 都會被拒絕；傳入一般新訊息不能繞過待回答中斷。同一次 resume 的決定在單一交易內提交；任一卡片衝突、內容改變、來源失效或未完整涵蓋，都會回滾這次請求的全部決定。使用新的 run ID 重試時，不能改變先前成功請求已提交的決定；項目順序與 metadata 不影響決定是否相同。

只有仍有效的 Admin 可以在此決定一般、未綁定特定執行身分的 `mcp_call` 或 `mcp_install` 核准。限定儀表板及其他種類的核准只提供通知，仍須到原本授權的介面操作。在原介面同意或拒絕後，以新的 run ID 重新連線，在 resume 帶齊上次回覆的所有中斷（包含已由外部決定的卡片），approved 布林值須符合已記錄的結果。Gateway 會核對原始要求、回合來源及持久保存的決定，再繼續原回覆管線。仍有員工存取權的非 Admin 也可確認既有決定，不會改寫結果或成為決定者。尚未決定的卡片仍須由有權介面處理；已過期、失效或結果不符的卡片不能續跑。到期前已決定的卡片，可在原回合期限內於卡片到期後確認。`forbid_self_approval` 政策仍會擋住自行同意；`metadata["duduclaw:canDecide"]` 只是顯示資訊，不會授予權限，每次決定都重新查驗。

端點會在期限內保留原回合，接受 resume 後繼續等待中的回覆管線。既有 broker 持久保存核准 ID、期限與來源關聯；runtime 續跑狀態只在記憶體，gateway 重啟後不能恢復，過時的 resume 會回報錯誤且不重播工具。重啟後，有未完成回合的 thread 在原本 15 分鐘期限內會拒絕新工作；期限過後，下一個新 run 先撤回舊回合尚未決定的核准，才准入。同意核准本身不代表工具已執行，也不代表供應商已接受操作。

沒有存活回合也沒有匹配完成收據的 resume，會在寫入准入紀錄前拒絕。每個帳號的容量與速率配額跨重啟保留，合計所有員工、thread 與收據請求，超限回 HTTP 429。SQLite 保存准入紀錄，同一 thread 在 24 小時內重複使用 `runId` 會收到 HTTP 409，即使本文完全相同也一樣；重試 resume 要用新的 run ID。同一 gateway process 在最多 15 分鐘內，可對完全相同、已完成的 resume 回傳 `RUN_STARTED` 與帶有 success 收據的 `RUN_FINISHED`。收據只表示這些決定已被記錄，不重送文字、不重跑工具，也不證明原工作成功。收據快取最多 128 筆，重啟後不恢復。

## 限制

| 項目 | 限制或行為 |
|---|---|
| 請求本文 | 256 KiB |
| 輸入歷史 | 最多 256 則；ID 最多 128 字元，不能含控制字元 |
| 同時執行 | 每位呼叫者 2 個、gateway 共 10 個，包含保留中的中斷回合 |
| 同時串流 | 另外限制每位呼叫者 2 條、gateway 共 10 條，錯誤與收據 SSE 也計入 |
| 准入紀錄 | run 與 thread 合計最多 4,096 筆，每帳號最多 256 筆；run ID 保留 24 小時，同一 thread 重複 run ID 回 HTTP 409 |
| 准入速率 | 每帳號滾動 60 秒內最多准入 32 次；容量或速率超限回 HTTP 429 |
| 保留期限 | 原回合共 15 分鐘，包含等待核准時間 |
| SSE keep-alive | 每 15 秒 |
| 慢速讀取 | 事件佇列有上限，每次傳送最多等 15 秒，不會無限保留工作 |
| 斷線 | 中止進行中的交付，撤回該回合尚未決定的核准；正常送出 interrupt 後則保留回合等待 resume |
| 傳輸 | 僅 JSON HTTP/SSE；沒有 Protobuf 或 `Last-Event-ID` 重播 |
| 輸入／輸出 | 文字輸入與最終文字回覆；沒有附件上傳或檔案交付 API |

輸入掃描、工具授權、核准、預算與輸出防護沿用既有回覆管線。參閱[安全防禦](05-security-defense.md)。協定參考：[AG-UI 1.0 HTTP/SSE](https://docs.ag-ui.com/spec/1.0/basic/transports/http-sse.md)、[run input](https://docs.ag-ui.com/spec/1.0/basic/run-input.md)、[interrupts](https://docs.ag-ui.com/concepts/interrupts)。
