# 信念迴圈（Belief Loop）

> 給 agent 一套像人一樣的對外部世界信念循環：先講出預測，讓現實打分，
> 下一次決策時看見自己的校準紀錄。

## 這是什麼

Task forward model（v1.53/v1.54）預測的對象是 agent *自身的執行*
（會用哪些工具、呼叫幾次、判官會不會放行）。信念迴圈補上先前缺少的外層：
對**外部世界**的結構化預測，主題不限，市場只是其中之一，一律以觀測到的
現實做決定性結算，並把 agent 的校準紀錄注回它的下一個決策提示詞。

迴圈全貌（所有掛鉤都是程式化的：平台計算每一筆信念與現實的差異並負責注入，
從未要求 agent 自己*回想*當初預測了什麼。這個設計受 arXiv:2605.29463
的反思虛構證據所迫，沒有選擇空間）：

```
belief_submit (MCP) ──▶ belief_log (prediction.db)
      ▲                        │
      │ 校準區段注入           │ tick 喚醒附上一行對照：
      │ 下一輪派工提示詞       │ 「你說 70% 會漲，現在跌 1.2%」
      │ （只算已驗證的結算）   ▼
      │                  belief_settle (MCP) ─▶ 決定性三向 Brier 結算
      └────────────────────────┘   （目前一律記為自行回報）
```

## 怎麼使用

Agent 拿到三個 MCP 工具：

- `belief_submit`：subject（任何要預測的外部對象，例如股票代號 `2317`
  或 `trial_conversion_rate` 這類 KPI）、horizon（自由格式的結算時點標籤，
  例如 `今日收盤` 或 `本週五`，上限 40 字元）、direction（`up`/`down`/`flat`）、
  probability（0 到 1）、一句簡短理由，以及方向據以衡量的參考值。
- `belief_settle`：信念 id 加上實現值。結算是決定性的：方向對照參考值、
  可設定的持平帶（`[belief] flat_band_pct`，預設 0.3%）、三向 Brier 計分。
  實現值是 agent 自己回報的。結算程式可以拿平台提供的價格交叉核對
  （容差 1%，偏差過大直接拒絕），核對通過才記為已驗證
  （`settle_source = "agent+tick_verified"`），但**目前沒有任何路徑提供這個價格**：
  `belief_settle` 工具不帶價格（MCP server 行程讀不到即時 tick 資料），
  gateway 端依即時 tick 結算也還沒實作。所以每一筆結算都記為自行回報
  （`settle_source = "agent_unverified"`）。回應帶有 `counts_toward_calibration`
  （自行回報時為 `false`），自行回報時另附 `note`，說明這筆已記錄但不計入校準。
- `belief_stats`：agent 自己的紀錄。校準數字放在 `verified` 底下，只用經過交叉核對的結算；
  `self_reported` 只是筆數加上描述用的比例。回應另附 `note` 說明這個區分。

只有 `settle_source` 完全等於 `agent+tick_verified` 的結算，才計入命中率、
Wilson 下界、平均 Brier 與過度自信。自行回報、未知或空值，以及沒有
`settle_source` 的舊紀錄都另外計數，不會進入任何校準數字。因為目前還沒有驗證路徑，
既有部署的每位 agent 都會顯示「沒有已驗證的結算」。

儀表板：Foresight 頁新增「信念與驗證」分頁，內容包含預測清單
（方向與信心對照實現結果、命中與否，每筆已結算的信念標示「已核實」或
「自行回報（未驗證）」）、Wilson 下界命中率、平均 Brier、過度自信程度，
這些數字都只用已驗證的結算。自行回報的結算另列筆數，不算進上面的數字。
沒有任何已驗證結算時，頁面直接說明目前無法評估準確度；已驗證結算未滿 30 筆時
只顯示筆數並明講原因，絕不出現「看起來有效」這種說法。

### `belief.summary`／`belief_stats` 回傳結構（破壞性變更）

儀表板 RPC `belief.summary` 回傳 `{ "stats": … }`，MCP 工具 `belief_stats`
回傳同一個物件（另加 `note`）。舊的扁平結構已移除：頂層的 `n_total`、`n_settled`、
`insufficient_samples`、`hit_rate`、`hit_rate_wilson_low`、`mean_brier`、
`overconfidence`，以及各 subject 的 `n_settled`／`hits`／`mean_brier` 都不再存在，
依舊結構寫的讀取端需要更新。

| 欄位 | 意義 |
|---|---|
| `agent_id` | 這位 agent |
| `n_submitted` | 提交過的所有信念，不論是否已結算 |
| `n_settled_all` | 所有已結算的信念（`verified.n + self_reported.n`） |
| `calibration_status` | `no_verified_settlements`（0 筆已驗證）、`insufficient_samples`（已驗證 1–29 筆）、`calibrated`（已驗證 30 筆以上） |
| `verified.n`、`verified.hits` | 已驗證結算的筆數與命中數 |
| `verified.hit_rate`、`verified.hit_rate_wilson_low`、`verified.mean_brier`、`verified.overconfidence` | `calibration_status` 不是 `calibrated` 時一律為 `null` |
| `self_reported.n` | 自行回報結算的筆數（含未知值與舊紀錄） |
| `self_reported.hit_rate` | agent 自己回報為命中的比例，只供描述，絕不當作校準結果；`n` 為 0 時為 `null` |
| `per_subject[]` | `{ subject, verified: { n, hits, mean_brier }, self_reported: { n } }` |

## 兩個例子

- **投資**（第一個通過驗證的使用場景）：subject 是股票代號，horizon 是
  `今日收盤`，參考值是送出當下的價格，實現值是收盤價。若有設定即時
  tick 來源，tick 喚醒時會把信念和即時值並列；tick 欄位對應到 subject 有兩條路：
  平台的 `zXXXX → XXXX` 命名慣例，或在 `config.toml` 明寫一筆
  `[belief] tick_subject_map`（鍵是 tick 欄位名、值是 subject）。
  結算本身仍是自行回報，tick 來源不會拿來驗證它。
- **業務 KPI**：subject 是 `trial_conversion_rate`，horizon 是 `本週五`，
  參考值是本週起始的轉換率。agent 每週一提交一筆對走向的信念，
  週末以 CRM 的實際數字結算。這條路完全不需要 tick 來源，
  改由排程的 goal 任務驅動結算即可。

## 誠實邊界

- 派工提示詞的校準區段只在 agent 至少有一筆已驗證結算時才注入；
  一筆都沒有時（目前所有部署都是如此）不注入任何內容。有注入時，
  自行回報的結算會另外列出筆數，並註明不計入任何數字。
- 把 agent 的校準歷史注入提示詞是一項**實驗，尚未被證明有效**。
  2026-08 的文獻掃描找不到任何一手證據支持或反對，因此每筆信念都記錄
  當時是否注入了統計（`stats_injected`），讓這個問題可以用你自己的資料回答。
- 校準與任務成果分開計分，兩者已知會分歧（arXiv:2607.03015）：
  校準良好的 agent 未必是高表現的 agent，儀表板也絕不把兩者混為一談。
- 已驗證結算未滿 30 筆時，任何數字都不會被當成結論呈現
  （全程使用 Wilson 下界與純筆數顯示）。自行回報的結算不計入這個門檻。
- 目前還沒有已驗證的結算路徑（gateway 端依即時 tick 結算尚未實作），
  所以各處的校準都還是空的。
