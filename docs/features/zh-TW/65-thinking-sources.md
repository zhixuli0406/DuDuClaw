# 思考內容從哪裡來

各個 runtime 怎麼把 AI 員工的思考回報給[思考過程顯示](64-thinking-display.md)。每一筆回報都先經過同一個每輪閘門（遮蔽、截短、CONTRACT 與輸出防護檢查）才會有人看到。這裡的任何設定都不會改變模型想多少、也不會改變一輪的費用，費用只看 `[model] effort`。

只有在讀這一輪的人可以看思考文字時（已登入的 WebChat 使用者、顯示設為 `summary`），runtime 才會改它送出的請求。排程、派工、心跳，以及讀者只能看計時的對話，送出的指令列與請求內容和以前完全一樣。

| Runtime | 顯示什麼 | 請求什麼時候會改 |
|---|---|---|
| Claude CLI | 即時思考文字（CLI 有給時）、持續更新的 token 估計、被加密的區塊、整輪思考 token 總數 | 只有讀者能看文字時才加 `--include-partial-messages` |
| Codex CLI | 推理摘要（`reasoning` 項目）與推理 token，執行完才出現 | 只有讀者能看文字時才加 `-c model_reasoning_summary=auto` |
| OpenAI 相容 API（DeepSeek、Qwen、vLLM、OpenRouter 等） | 模型原文推理（`reasoning_content`、`reasoning`，或答案開頭的 `<think>…</think>`）；去識別化開啟時才顯示文字 | 沒有文字讀者時一律不改；有的話工具迴圈改用串流，推理邊寫邊出現 |
| Anthropic Direct API | 思考摘要、未提供摘要與被加密的區塊，呼叫結束後出現 | 只有讀者能看文字、而且模型本來每次都會思考（Opus 5／5.5、Sonnet 5／5.5、Haiku 5.5、Fable、Mythos）時，才要求摘要顯示，費用不變 |
| Antigravity（`agy`） | 目前沒有（只有計時） | 不改 |
| Grok CLI | 沒有（只有計時） | 不改 |

## 細節

- **Claude CLI。** 新模型在 `-p` 模式下的思考區塊常常是空的，畫面會顯示「此模型未提供思考摘要」。較新的 CLI 會送 `system`／`thinking_tokens` 進度框，就算沒有文字也有 token 估計。部分訊息（partial message）那幾行由思考解析器吃掉，不會進任務樹、預警、操作員結果擷取，也不會進答案，所以答案不會被算兩次。簽章與被加密的思考資料不會離開 CLI 解析器。開了 `DUDUCLAW_STREAM_DEBUG=1` 時，`claude_stream.log` 裡每一行 `thinking_delta` 會記成 `{"type":"stream_event","elided":"thinking_delta"}`，不記內容；部分訊息行也不會進失敗診斷。
- **Codex CLI。** 推理項目在執行結束後才讀（CLI 輸出目前還沒有逐行串流），所以摘要和答案同時出現。
- **OpenAI 相容 API。** 開源模型回的是它自己的推理原文，所以只有開啟去識別化時才顯示文字，沒開時只顯示秒數、token 與提示。`<think>` 只在答案最開頭（去掉空白後）才算數；沒有收尾標籤時，後面全部當作推理。員工送出的答案不變：開頭的 `<think>` 區塊照舊會拿掉。工具迴圈的訊息（含推理段落）照供應商回傳的原樣送回模型。供應商不接受串流時改回一般呼叫，對話照常完成；串流答案沒有附用量時，用估計值記帳，並在 `cost_telemetry.db` 標 `usage_estimated = 1`。
- **Anthropic Direct API 與操作員工具迴圈。** 每次呼叫後回報摘要。簽章與 `redacted_thinking` 資料原樣送回模型，從不顯示。
- **Antigravity。** `tool` 以外的步驟類型只把名稱寫進 debug 紀錄，等知道詞彙後再對應。

尚未用真額度驗證：目前的模型在 `claude -p` 下會不會回非空的思考文字、`codex exec` 在真帳號上何時送出推理項目、agy 的步驟詞彙。解析器是用固定測資與假 CLI 測過的。
