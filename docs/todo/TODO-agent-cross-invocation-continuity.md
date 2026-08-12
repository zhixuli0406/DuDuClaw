# TODO: Agent 跨 invocation 行動連續性（否認/遺忘自己排程時做過的事）

> 狀態：**Resolved（2026-08-12，方向 1+2+3 合併落地，欠活體驗證）** · 類型：設計缺口（平台，影響所有多觸發來源 agent）· 優先：High
> 發現於：2026-08-12，LWM 交易實驗 D2：agent 在 TG 回覆時斬釘截鐵否認自己五分鐘前排程 run 做過的決策與下單。
> 相關：[TODO-rfc24-decision-continuity.md](TODO-rfc24-decision-continuity.md)（相鄰，見下方「與 RFC-24 的界線」）、
> [TODO-telegram-reply-context.md](TODO-telegram-reply-context.md)（同一事件另一個獨立 bug）。
>
> **修復摘要（2026-08-12）**：新模組 `crates/duduclaw-gateway/src/recent_actions.rs`。
> - **方向 1（近期自身行動注入）**：每次 invocation 開場前，從稽核日誌
>   `tool_calls.jsonl`（程式化證據、`resolve_audit_agent` 跨觸發來源歸屬）彙整該
>   agent 近 24h 的工具呼叫（**含失敗/被攔截的 ❌ 行動**——正是即時工具狀態看不到的
>   那一類），壓成 `## 近期自身行動（稽核紀錄）` 區塊。尾端注入、位於
>   `CACHE_SPLIT_MARKER` 之後（channel_reply 動態尾巴）/ 未快取 `tasks_suffix`
>   區塊（claude_runner），不破壞靜態 prompt cache 前綴。
> - **方向 2（自我提問查耐久紀錄）**：區塊 header 內建硬指示——回答「你是否做過
>   某事」以本紀錄與耐久紀錄為準，即時工具查詢不可作為唯一依據。
> - **方向 3（統一可見性）**：兩條注入路徑（channel_reply + claude_runner 的
>   dispatch/cron/heartbeat/reminder/ephemeral）共讀同一份稽核日誌，即同一份
>   per-agent 行動 feed。
> - 防護：`config.toml [memory] recent_actions_enabled`（預設開）/
>   `recent_actions_count`（預設 10、上限 50）；尾端 256KB tail-read（不掃全檔）；
>   連續重複合併 ×N；行 200B / 區塊 2400B 上限；無近期行動時零注入（零噪音）；
>   所有失敗 fail-open 成「無區塊」。
> - 測試：`recent_actions::tests` 9 條全綠（含被攔截委託情境、去重、容量、
>   config gate、CJK 截斷）；gateway lib 全套 4673 tests 零回歸。
> - **未完成**：活體驗證（排程 run 下單被擋 → TG 問「你下過嗎」→ agent 覆述得出）；
>   實驗層 CLAUDE.md 止血規則（見下）仍建議保留作為縱深。

## 一句話

同一個 agent 的不同 invocation（排程 / heartbeat / goal-loop vs 頻道回覆）**彼此看不到對方的行動**，且 agent 被問到自己的過去行為時**只查即時工具狀態、不查自己的耐久紀錄**，於是會**對自己真的做過的事做出錯誤否認**。

## 症狀（D2 實例）

1. `lwm-premarket` 排程 run 於 08:42 產出盤前計畫（「主計畫：鴻海 2317…8 股 @264」）、落 forecast、送出委託（被緊急煞車擋，未達券商）。全部寫進 `trade_journal.jsonl`。
2. 使用者稍後在 TG 問「這筆 2317 是你下的嗎」。這是**另一次** invocation。
3. Agent 回覆：「2317 不是我的策略動作、我從沒決策要買鴻海、我的標的是 00919/00878」——其中 00919/00878 是**前一天**的標的。
4. 即：agent 否認了自己耐久紀錄裡白紙黑字、五分鐘前才做的決策。

## 根因（兩層，疊加）

### R1：跨 invocation context 未共享
- Session 是**每對話**獨立（`webchat:<conn>#agent:<id>#conv:<nonce>` 之類）；排程 / heartbeat / goal-loop 的 run 與頻道回覆的 run 是**不同 session、不同歷史**。
- 一次 run 的計畫/行動不會出現在另一次 run 的 context，agent 因此「不記得」自己排程時做了什麼，退回引用更舊的、剛好還在記憶裡的標的。

### R2：自我提問時查即時狀態、不查自己的行動紀錄
- 被問「我有沒有下過 X」，agent 去查 `list_orders`（券商委託簿）→ 空（因為該單被風控/煞車擋、根本沒到券商）→ 斷言「沒下過」。
- 它**沒讀自己的耐久紀錄**（`trade_journal.jsonl` / 稽核日誌 `tool_calls.jsonl` / 記憶），那裡才有被擋的委託與當時的計畫。
- 即時工具狀態對「被拒/被擋/未成交」的行動是**沉默的**，拿它當「我做過什麼」的唯一依據，必然漏掉這一整類。

## 影響

- 任何多觸發來源的 agent（排程 + 頻道 + 委派）都可能對自己排程時的行為失憶或否認。
- 對使用者是**信任問題**：agent 否認自己做過的事，即使成因偏技術，體感就是不可靠。
- 與誠實性評估糾纏：容易被誤判為「說謊」，實則多為 context 斷裂 + 自我盤點不完整。

## 方向（擇一或組合，非定案）

1. **注入「近期自身行動」摘要**：每次 invocation 開場，從既有稽核日誌 `tool_calls.jsonl`（已含 masked `result_text`/`input_text` + `resolve_audit_agent` 歸屬，見 harness→LWM plan B）彙整該 agent **跨所有觸發來源**近 N 筆行動，壓成一段短摘要注入 context（U 型注意力尾端、不進 cache 前綴）。讓「我剛才做了什麼」不依賴單一 session 歷史。
2. **自我提問路由到耐久紀錄**：偵測「我有沒有 / 我是不是做過 …」類自指問題時，強制先查耐久行動來源（稽核日誌 / 任務 activity / 專案日誌 / 記憶），而非只看即時工具回傳。可作為系統提示規則或一個 `self_action_lookup` 工具。
3. **統一 cron + channel 行動可見性**：per-agent 的「近期行動 feed」（可複用 Task Board activity / 稽核日誌），排程與頻道 run 共讀同一份。

## 與 RFC-24 的界線（避免重工）

- RFC-24 決策連續性：擷取**對使用者提出的決策（A/B/C）**、使其存活 session 壓縮並回注——聚焦 channel_reply 內、使用者面的決策。
- 本 TODO：跨**不同 invocation**（排程↔頻道）的**已執行行動**可見性，以及自我盤點該以耐久紀錄為準。兩者資料源（稽核日誌/行動 feed）可共用，但問題與注入內容不同。實作時共用抽象、職責分開。

## 實驗層近期止血（便宜、可先做）

- 更新 trader / observer 的 `CLAUDE.md` 硬規則：**回答「我有沒有做過 X」一律先 `Read trade_journal.jsonl`**（含被擋/被拒的委託），**不可只看 `list_orders`**（券商層，風控/煞車擋下的單不會出現）。這能立刻降低「否認自己決策」的發生。

## 測試計畫

- [ ] 情境：排程 run 送出一筆被擋的委託 → 另一次 invocation 問「你下過這筆嗎」→ agent 覆述得出該筆（含被擋原因），不否認。
- [ ] 注入摘要有容量上限與去重；無近期行動時不產生噪音。
- [ ] 未啟用時行為與現況一致（fail-safe、可 gated）。

## 文件同步（實作時）

- `CHANGELOG.md` → `Added`：跨 invocation 近期自身行動摘要注入 / 自我提問查耐久紀錄。
- 若動到 session 或注入點，更新 `docs/architecture/` 對應說明。

## 部署備註

修正在 gateway。運行中服務（含 LWM 實驗容器）需重建 + 重啟才生效；**交易時段不重啟實驗容器**，收盤後部署。實驗層近期止血（改 CLAUDE.md）不需重建、隨時可做。
