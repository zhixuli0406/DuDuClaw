# 思考過程顯示

在 dashboard 的 WebChat 送出訊息後，回覆區馬上顯示 AI 員工正在思考，計時一路跑到回覆送達。模型有回報思考摘要時，回覆送達後可以展開看遮蔽、截短過的版本。在通訊軟體上，綁定 dashboard 帳號的同仁看得到遮蔽後的摘要，其他人只看得到計時，見[通訊軟體的思考顯示](66-thinking-in-messaging-apps.md)。

其他語言：[English](../64-thinking-display.md) · [日本語](../ja-JP/64-thinking-display.md)

## WebChat 上看到什麼

- **回覆還在路上**：動畫圓點旁顯示「思考中… 12 秒」。計時從按下送出那一刻開始，所以每個 runtime 都有，連模型完全不回報思考的（Grok、Antigravity、Gemini CLI）也一樣。超過一分鐘顯示「思考中… 1 分 5 秒」。模型有回報 token 數時旁邊多一個「約 1.2k tokens」，下面一行是模型最新的進度短句；沒有的話顯示正在執行的工具步驟。
- **回覆送達後**：即時那一列收成回覆上方任務動態框頂端的「思考了 12 秒 · 約 1.2k tokens」。秒數是從送出到回覆送達的牆上時間，和即時計時最後顯示的數字相同。同一段對話裡較早的回覆保留一行較小的同樣資訊。
- **展開**：只有實際顯示過思考文字時才能展開。每一段標示「思考摘要」「思考原文（已遮蔽機密）」或「進度」，並附這一段的秒數；被截短的段落結尾有「（已截短）」。
- **沒有文字的情況**：模型不提供摘要時顯示「此模型未提供思考摘要」，沒有展開箭頭。思考文字被輸出檢查擋下時顯示「部分思考內容因安全規則未顯示」，那一段已經顯示在畫面上的文字會被移除。開源模型的原始推理在去識別化關閉時顯示「原文模型的思考內容需啟用去識別化後才會顯示」。

畫面上的文案寫的是「思考摘要」，沒有宣稱是完整推理過程。現在多數模型的供應商只回摘要，或什麼都不回。

螢幕閱讀器只會聽到一次固定的「思考中…」；跳動的秒數和進度短句不播報，避免每秒被打斷。開啟「減少動態效果」時圓點改成靜態圖示、不做淡入，計時照常跳。

## 誰看得到什麼

| 對象 | 計時 | token | 思考文字 |
|---|---|---|---|
| 登入 dashboard 的 WebChat 使用者 | 有 | 有 | `display = "summary"` 時有 |
| 網站聊天小工具的訪客（公開小工具金鑰） | 沒有（gateway 不送任何思考訊框） | 沒有 | 沒有 |
| Odoo 網站 Live Chat 訪客 | 什麼都沒有 | 沒有 | 沒有 |
| 通訊軟體（LINE、Telegram、Discord、Slack…） | 思考狀態訊息或一行「💭 思考了 N 秒」，見[通訊軟體的思考顯示](66-thinking-in-messaging-apps.md) | 沒有 | 只有綁定 dashboard 帳號的發話者，預設只在一對一聊天（`[thinking] channel_audience`） |
| ACP、內部工作階段 | 最多一行狀態「💭 已收到，正在思考（已 N 秒）。」（見[通知治理](40-notification-governance.md)） | 沒有 | 永遠沒有 |

WebChat 的計時在瀏覽器裡算，所以就算關掉思考顯示，登入的使用者仍看得到計時。

## 遮蔽

思考文字在 gateway 送出前就遮蔽好，不靠瀏覽器：

- `<REDACT:…>` 代號換成「〔已遮蔽·類別〕」，**不會**還原成原值（一般回覆會還原，思考文字不會）。
- 去識別化開啟時，對文字跑一次去識別化規則（不寫保管庫、不跑裝置上的 NER 模型）。
- 看起來像 API 金鑰或密碼的字串整段換掉。
- 對文字跑輸出防護與員工的 `CONTRACT.toml`。被擋下時，這一段和本輪之後的每一段都只顯示固定的「未顯示」說明，`security_audit.jsonl` 記一筆（只記規則名稱）。回覆本身照常送出。
- 每段最多 1,500 字（開源模型原始推理 1,200 字），每輪合計受 `[thinking] max_chars` 限制。還在串流的段落最後幾個字先不送，等這段結束才送，避免半個金鑰出現在畫面上。

思考文字不存在任何地方：對話紀錄、執行紀錄、費用統計都沒有。瀏覽器只在開著的頁面記憶體裡保留。「執行紀錄」頁每輪留一列秒數與 token 數，例如「思考了 8 秒 · 約 1.2k tokens」。

## 設定

```toml
# config.toml
[thinking]
display = "summary"              # summary | status | off（整個 gateway 的上限）
max_chars = 4000                 # 每輪思考文字合計（200–20000）
anthropic_display = "summarized" # summarized | updates（beta，只有部分 Claude 模型支援）
```

```toml
# agents/<id>/agent.toml
[model]
show_thinking = "summary"        # summary | status | off；不設定＝跟隨 [thinking] display
```

- `summary`：計時、token、遮蔽後的思考文字。
- `status`：計時與 token，沒有文字。
- `off`：gateway 不送思考訊框；WebChat 自己的計時仍在。
- 兩者取較嚴格的（`off` < `status` < `summary`），所以員工設定只能收窄 gateway 允許的範圍。
- `config.toml` 存在卻讀不到或解析失敗，或 `display` 值看不懂，都當成 `status`（不顯示文字）。`show_thinking` 值看不懂時忽略並記警告。
- 兩個檔案每輪都重新讀取，改完下一則回覆就生效。目前沒有專用的 dashboard 控制項，請在「系統設定 → 進階設定 → 設定檔進階編輯」修改（見 [dashboard 設定地圖](../../guides/zh-TW/dashboard-settings.md#思考過程顯示)）。

顯示思考不會改變費用。思考 token 不論有沒有顯示，供應商都照量計費；這裡只有 `[model] effort` 會影響模型想多少。DuDuClaw 不會為了顯示而替預設不思考的模型打開思考。

## 目前的限制

- 各 runtime 在後續版本才開始回報思考。在那之前，WebChat 每則回覆都有計時，「思考了」那一行沒有 token 數，也沒有可展開的內容。
- `assistant_done` 只有在這一輪送過思考訊框時才帶 `thinking: {tokens, blocks, kinds}`。
