# 通訊軟體的思考顯示

在 Telegram、LINE、Slack 等通訊軟體和 AI 員工對話的人，都看得出它正在思考；綁定了 dashboard 帳號的同仁還能讀到一小段遮蔽過的思考摘要。外部客戶只看得到計時，群組聊天預設也只顯示計時，網站訪客什麼都看不到。

摘要和 dashboard 聊天顯示的是同一份文字（見[思考過程顯示](64-thinking-display.md)與[思考內容從哪裡來](65-thinking-sources.md)）：先遮蔽（去識別化代號換成〔已遮蔽·類別〕且絕不還原、像金鑰的字整段換掉、再跑 CONTRACT 與輸出防護），再截短，而且不存檔。

## 各平台怎麼顯示

| 平台 | 思考中 | 答案好了 |
|---|---|---|
| Telegram、Discord、Slack、Microsoft Teams | 一則狀態訊息「💭 思考中… 12 秒」，最快每 3 秒更新一次；可看摘要的人另外看到最新一行 | 同一則訊息改成「💭 思考了 12 秒」加摘要：Telegram 是可展開的引用、Discord 是防雷（不會 @ 到任何人）、Slack 是小字 context 區塊、Teams 取前 300 字。答案接在下面 |
| LINE（一對一） | LINE 的載入動畫（每 50 秒續一次） | 在同一次回覆裡、答案前面多一則小訊息「💭 思考了 12 秒」（加摘要前 300 字）。答案已經用滿一次回覆的 5 則訊息、或 reply token 過期改用推播送出答案時，就不加：思考顯示絕不多花一次推播 |
| LINE（群組） | 沒有（LINE 群組沒有載入動畫） | 同樣多一則小訊息，預設只有計時 |
| WhatsApp、飛書、企業微信、釘釘、Google Chat | 沿用原本「已收到」的狀態行 | 答案最後、「Powered by DuDuClaw」上方加一行「💭 思考了 12 秒」（加摘要前 300 字） |

「12 秒」是從訊息送達到答案備妥的時間，和 dashboard 聊天顯示的是同一個數字。模型完全沒回報思考時不顯示任何東西。在可編輯的平台上，思考狀態顯示期間不再另外送「已收到」狀態行；最後沒有產生回覆時，狀態訊息會被刪掉。用語跟著 `[general] default_language`（繁體中文、英文或日文）。

## 誰看得到什麼

`config.toml`：

```toml
[thinking]
channel_audience = "internal"     # internal | all | none
channel_group_display = "status"  # summary | status | off
```

| 設定 | 一對一聊天 | 群組聊天 |
|---|---|---|
| `internal`（預設），發話者已綁定 dashboard 帳號 | 計時＋摘要 | 計時（`channel_group_display`） |
| `internal`，其他發話者 | 計時 | 計時 |
| `all` | 計時＋摘要 | 計時（`channel_group_display`） |
| `none` | 都不顯示 | 都不顯示 |

- 「已綁定」指發話者的通道身分已驗證，而且屬於啟用中的 dashboard 帳號（和 `@` 呼叫員工用的是同一個檢查）。查詢失敗一律當作未綁定。
- `channel_group_display` 是群組聊天最多顯示到哪裡，不論誰發話。設成 `summary`，已綁定的同仁在群組裡也看得到摘要。
- `[thinking] display` 與回答員工的 `[model] show_thinking` 取較嚴格者，是所有情況的上限：`status` 只有計時、`off` 都不顯示。用 `@` 叫來的員工套用它自己的設定。
- `config.toml` 讀不到、解析失敗，或值不認得，都不顯示。
- 網站訪客（Odoo 網站線上客服、WebChat 網站小工具）不論怎麼設定都看不到思考。

只有可以看摘要的發話者，才會讓 runtime 去要思考文字（見[思考內容從哪裡來](65-thinking-sources.md)）；其他人的請求和以前完全一樣。顯示思考不會改變一輪的費用。

尚未在真帳號上驗證：Telegram 可展開引用與 Discord 防雷在各版本用戶端的顯示、Slack 討論串裡的 context 區塊、LINE 額外訊息在真手機上的樣子、Teams 的編輯。訊息內容是用本機的假 API 檢查的。
