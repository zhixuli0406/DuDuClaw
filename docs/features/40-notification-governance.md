# 通知治理（Notification Governance）

DuDuClaw 的 AI 員工會主動找你：任務卡關、預算用完、高風險動作要你點頭、演化迴圈停滯、通道送不出訊息。這些訊息各自都合理，但在 v1.54 之前它們共用一個問題——**沒有分級**。凌晨三點的「演化事件」和凌晨三點的「任務卡住等你決定」用同一條路推出去，同樣會亮螢幕、同樣會出聲。

通知治理是擋在所有推播出口前面的那一層。它做四件事：把每則通知標上級別、在勿擾時段把不急的延後、每天彙整一則摘要、並且量測「這類通知到底有沒有人理」。

---

## 一、四級階梯（escalation ladder）

每個推播點都必須在程式碼裡標明級別（`NotifyLevel`），沒有預設值——新增一個推播出口時，你被強迫回答「這件事值不值得吵醒人」。

| 級別 | 判準 | 勿擾時段行為 | DuDuClaw 實例 |
|---|---|---|---|
| **L0** | 沒有狀態變化 | 根本不進這一層 | 心跳、巡邏無事 |
| **L1 週知** | 發生了，但不需要人 | 延後，醒來後合併 | 演化事件、SOUL 整併、技能缺口摘要、每日摘要、預算恢復 |
| **L2 待確認** | 需要人知道並按一下 | 延後，醒來後單獨投遞 | 派工核准（任務還沒開始）、自動規則跳閘暫停 |
| **L3 須處理** | 又急、又重要、又可行動、又是真的 | **照發，不擋** | 自主任務等你決定、高風險審批、安裝簽核、預算停工、通道故障 |

判準借自 Google SRE 的告警三分法（Page / Ticket / Report）。實務上的分界線是「等到早上有沒有代價」：派工核准等到早上只是晚八小時開工；任務卡在半路等你決定，是整條自主迴圈停擺。

L3 永遠不受勿擾時段影響，這是刻意的——一個可以被設定關掉的緊急通知不叫緊急通知。

---

## 二、勿擾時段（quiet hours）

### 設定

```toml
# ~/.duduclaw/agents/<agent>/agent.toml
[proactive]
quiet_hours = "22:00-08:00"   # 可選；未設定 = 不啟用
timezone    = "Asia/Taipei"   # 可選；無法解析時用主機系統時區
```

```toml
# ~/.duduclaw/config.toml — 全域退路
[notify]
quiet_hours = "22:00-08:00"
```

員工自己的設定優先，沒有才吃全域值。跨午夜的區間（`22:00-08:00`）正常運作；區間是左閉右開，所以設 `08:00` 結束代表 08:00 整就會投遞。

### 解析失敗一律「不啟用」

格式錯誤、空字串、`start == end`（例如 `00:00-00:00`）全部視為沒有勿擾時段，並在日誌留下 `warn` 記錄告訴你哪個值被忽略了。理由很直接：解析器的 bug 不該讓一個部署整晚收不到通知。設定寫錯的代價是「照常收到通知」，不是「靜悄悄什麼都收不到」。

### 舊欄位不會自動接管

`[proactive]` 底下還有一組舊的 `quiet_hours_start` / `quiet_hours_end`（數字小時），它的預設值是 23–8，而且**每個員工都有**。治理層刻意不讀它——否則每個既有安裝都會在沒人要求的情況下，突然被靜音一整晚。那組欄位維持原本較窄的職責：排程 `[proactive]` 主動檢查的時間。

### 延後不是丟掉

被擋下的通知寫進 `~/.duduclaw/notify_queue.jsonl`（附檔案鎖），時段結束後由背景排程器投遞：

- **同一個收件目的地的 L1 通知合併成一則**（`🌙 勿擾時段收到 3 則通知：` + 編號清單），不是三則分開的訊息。
- **決定卡單獨投遞**，因為每張卡有自己的按鈕。
- 佇列有雙重上限：最多 500 則、超過 36 小時的直接丟棄（一天半前的「任務完成了」不是新聞）。**丟棄一定會寫 `warn` 日誌**，不會無聲消失。

已知限制：在勿擾時段內被人從儀表板處理掉的決定，早上仍會投遞一張過期的卡片。按下去會被該決定的儲存層以「此決定已處理」擋掉（fail-closed），所以代價是一張多餘的卡片，不會重複執行。

### 儀表板要說出來

`agent.get` RPC 的 `proactive` 區塊多回傳兩個欄位：

- `quiet_hours` — 生效中的區間字串（`"22:00-08:00"`），沒有勿擾時段時為 `null`
- `quiet_hours_note` — 一句可直接渲染的繁中說明，明確寫出「哪些會延後、哪些照常送達」

會被靜默的條件必須攤在使用者看得到的地方，這是硬要求，不是加分項。

---

## 三、每日摘要

```toml
# ~/.duduclaw/config.toml
[notify]
daily_digest    = false     # 預設關閉，需明確開啟
daily_digest_at = "09:00"   # 本地時間
```

每天一則，推到 `[general] default_agent` 的 `[proactive]` 目的地，內容彙整過去 24 小時：

- 完成任務數
- 待你決定的件數（審批 + 安裝申請 + 卡在 `needs_human` 的任務）
- 學習事件數（`gvu_*` / `playbook_*` / `soul_*` / `evolution_*` 活動流事件）
- 花費
- 通道異常告警次數
- 行動率過低的通知類別（見下節）

### 無事不寄

全部歸零的一天，**不發訊息**——不是發一則「今日無事」。一個你每天都可以不看的摘要，會訓練你連該看的那天也不看。

上限硬性一則：狀態檔記錄「最後送出的本地日期」，重啟不會補送第二則；但 09:00 時 gateway 沒開機、11:00 才起來的話，當天的摘要仍會送出一次。

---

## 四、行動率量測

每則通知送出時記一筆，每次有人真的把決定按掉時記一筆，存在 `~/.duduclaw/notify_events.jsonl`。

```
duduclaw dashboard → notify.stats RPC
{
  "days": 30,
  "broken_threshold": 0.5,
  "min_sample": 10,
  "types": [
    { "type": "decision.install", "pushed": 12, "actionable": 12, "acted": 5, "action_rate": 0.42, "broken": true },
    { "type": "decision.goal",    "pushed": 12, "actionable": 12, "acted": 12, "action_rate": 1.0,  "broken": false }
  ]
}
```

`broken` 直接搬 Google SRE 的判準：**準確率低於 50% 的告警就是壞掉的告警**。條件是至少 10 筆「可按的」推播，且行動率低於 50%。

兩個刻意的設計：

- **同一張卡按兩次只算一次行動**（用決定 id 比對，不是計數），所以行動率不會超過 100%。
- **被拒絕的按壓不算行動**——按了但被權限或狀態擋掉，是失敗的互動，不是「這則通知有用」的證據。
- 純週知類（沒有按鈕可按）會回報 `actionable: 0` 且**永遠不會被標記 broken**。說「FYI 的行動率是 0%」是同義反覆，不是發現。

儀表板圖表已實作，位置見下一節。

---

## 五、通道故障記錄的 schema

`channel_failures.jsonl` 有兩項調整：

1. **`channel` 欄位普及**：所有能從 session id 推出平台的寫入點（`channel_reply_silent` / `channel_reply_fallback` / `runtime_fallback_substitution` / `trajectory_anomaly` / `foresight_alarm`）都補上 `channel`，讓儀表板統一日誌能回答「這次失敗發生在哪個平台」。推不出平台的（cron / bus / heartbeat 這類內部 session，以及只拿得到工作目錄的 PTY fallback）寫 `null` 或省略——不假造歸屬。

2. **恢復事件**：通道從告警狀態回到正常時，寫一筆
   ```json
   {"event":"channel_recovered","channel":"telegram","reason":"recovered","resolved":true,"resolves":"telegram_send_failed","timestamp":"…"}
   ```
   舊的失敗行**不會被改寫**（append-only 稽核檔），消費端靠「同一 channel 有沒有更晚的 `channel_recovered`」判斷這次故障是否仍然相關。

⚠️ 連帶的行為修正：通道故障告警的判定條件從「有 `channel` 欄位」改成「`event` 在送出失敗白名單內 **且** 有 `channel` 欄位」。少了這一步，第 1 點會讓每次 LLM 逾時、每次軌跡異常都被當成通道斷線來告警。白名單目前只有 `telegram_send_failed`；要加入新項目，該事件必須真的代表「這個通道送不出訊息」。

---

## 六、儀表板介面位置

這幾項設定／資訊都有 RPC，但入口分散在不同頁面，直接列出：

| 設定／資訊 | 位置 | 說明 |
|---|---|---|
| 勿擾時段（員工自己的 `quiet_hours`，完整脈絡） | AI 員工列表 → 選一位員工 → 編輯（`/agents/:id/edit`）→「自動化」分頁 | 進階區塊有一個 `HH:MM-HH:MM` 輸入框，格式錯誤會就地標紅；下方即時顯示這位員工目前生效中的說明句（`quiet_hours_note`）。 |
| 勿擾時段（跨員工快速切換） | 系統設定 →「主動行為」分頁 | 同一顆 `[proactive] quiet_hours`，頁面頂端有員工下拉選單可切換。若這位員工的新格式欄位是空的、但偵測到舊版數字式 `quiet_hours_start`/`quiet_hours_end` 曾被改成非預設值（代表操作者早年透過這個頁面設定過勿擾時段），會顯示轉換提示並自動代填成新格式，儲存後才正式生效——避免「以為改了勿擾時段，其實治理層完全看不到」這種靜默陷阱（W2-9）。 |
| 每日摘要開關（`[notify] daily_digest` / `daily_digest_at`） | 系統設定 →「系統」分頁 | 開關 + 時間欄位；全域一則，非逐員工設定。 |
| 通知成效（`notify.stats` 行動率） | 分析報表頁 →「通知成效」卡 | 依第四節的 SRE 50% 判準列出各通知類型的推播數／行動率，行動率過低的類別長條標紅並附一句提示文字（不只靠顏色，避免色弱或黑白列印看不出來）；純週知類（沒有按鈕可按）永遠不會被標記壞掉。 |

同一顆 `[proactive] quiet_hours` 有兩個編輯入口：員工編輯表單的「自動化」分頁是完整脈絡（跟通知目的地、check-in 排程放在一起）；系統設定的「主動行為」分頁是給操作者不進員工詳情就能快速切換、查看多位員工勿擾時段的捷徑。兩邊寫的是同一個 `agent.toml` 欄位，存檔後立即互相反映，不會各自為政。

---

## 相關

- 設計依據：`commercial/docs/ux-redesign-2026-08/02-ux-methodology.md` 主題 4（P4-1、P4-4、P4-5、P4-6）與 `03-analogous-products.md` C1／C7／C8／C12
- 程式碼：`crates/duduclaw-gateway/src/notify_governance.rs`、`notify_stats.rs`、`notify_digest.rs`
- 前端：`web/src/pages/agent-form/EditAgentPage.tsx`（自動化分頁）、`web/src/components/settings/sections/ProactiveTab.tsx`、`SystemTab.tsx`、`web/src/pages/ReportPage.tsx`
- 相關功能：[34-goal-loop.md](34-goal-loop.md)（needs_human 決定卡）、[23-autopilot-engine.md](23-autopilot-engine.md)（規則跳閘通知）
