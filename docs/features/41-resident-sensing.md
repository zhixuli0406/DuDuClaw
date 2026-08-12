# 常駐感知＋訊號喚醒（Resident Sensing）：外部資料流命中規則才叫醒 AI 員工

DuDuClaw 的 Rust 閘道層本來就 24 小時常駐（heartbeat、autopilot 事件匯流排、CEP、
OS 感知）。這篇文件說明新補上的一塊：把**外部**資料流（行情輪詢、日誌檔、任意指令
輸出、WebSocket 即時串流）接進同一條匯流排，讓便宜的 deterministic 規則常駐盯著它，只有真的命中訊號
才叫醒昂貴的雲端 AI 員工。

## 一句話說明

外部資料流進 autopilot 事件匯流排，deterministic 規則（可選 CEP 時序判斷）24
小時全量盯著，命中後可再加一道本地小模型二篩，只有訊號真的值得看，才委派給
雲端 AI 員工。整條路徑預設關閉，沒設定任何來源就跟這個功能不存在時完全一樣。

## 為什麼喚醒式平台需要這層

DuDuClaw 的核心互動模式是「AI 員工回應你的訊息」，但有些場景需要反過來——AI
員工要盯著一個不斷變動的外部數字（股價、庫存量、某支程式的輸出），等它真的
出現異常才主動說話。過去做這件事只有兩種辦法，而且都不好：

**辦法一，讓雲端 LLM 自己定時輪詢。** 每次輪詢都是一次 LLM 呼叫，多數時候
資料根本沒變化或沒超過門檻，等於花錢請 AI 員工盯著一個沒事發生的畫面。

**辦法二，寫死一支獨立腳本另外跑。** 判斷邏輯脫離 AI 員工的規則系統，無法用
既有的 autopilot 斷路器保護，也沒有統一的觀測介面，出問題只能自己查日誌。

哲學上，這對應 System 1／System 2 的分工：Rust 規則＋可選的本地小模型是便宜、
常駐、不會累的 System 1，只有規則判斷「這件事值得注意」，才觸發昂貴、有推理
能力的雲端 System 2。`cep_matcher.rs` 既有的原則不變——時序判斷永遠是
deterministic 的程式邏輯，不會交給 LLM 去猜。

## 架構

```
TickSource（http_poll / command / file_tail / websocket，每來源一個 tokio task）
    │  抽取 json_fields → 衍生 delta 欄位 → 寫入環形緩衝
    ▼
autopilot 事件匯流排（AutopilotEvent::Tick，與 task/channel/cron 等事件同一條）
    │
    ▼
deterministic 規則比對（all/any + eq/neq/in/not_in/gt/gte/lt/lte/contains）
    │  也可用 CEP 時序規則（"價格破線後 60 秒內沒有回穩訊號" 這類跨事件時序判斷）
    ▼
斷路器（沿用既有三態斷路器，高頻誤設也炸不了）
    │
    ▼
（可選）本地模型初篩 —— 只問 YES/NO，絕不外呼雲端
    │  NO / 逾時 / 答不出來 → 依 on_unavailable 政策放行或攔截
    ▼
delegate 喚醒雲端 AI 員工（提示詞可附上最近觀測窗口）
```

每一段都可以獨立關閉：不設來源，這條匯流排完全靜默；不加 `screen`，規則命中
就直接喚醒（跟其他事件類型的規則行為一致）；`screen` 判 NO，喚醒就此打住，
連 `delegate` 都不會執行。

## 設定：`config.toml [tick]`

總開關預設關閉，四種來源各示範一個：

```toml
[tick]
enabled = false                 # 總開關，預設關；沒開就完全不影響現有安裝
allow_command_sources = false   # command 來源的全域閘門，fail-closed
dns_ttl_secs = 60               # 已通過內網檢查的 DNS 解析結果可重用幾秒；0 = 每次都重新解析

# ── http_poll：定時 GET 一個網址 ──────────────────────────
[[tick.sources]]
id = "twse-2330"                 # ^[a-z0-9][a-z0-9-]{0,63}$
kind = "http_poll"
enabled = true
interval_secs = 10                # 下限 1 秒，低於此值會被拉高
url = "https://example.invalid/quote"   # 會過既有 SSRF 檢查（拒 localhost/內網/雲端 metadata）
headers = { "X-API-Key" = "把金鑰直接寫在這裡" }   # 選配，最多 8 個；值不會出現在任何 log 或 API
json_fields = { price = "/data/price", vol = "/data/volume" }  # 欄位名 → JSON pointer
emit_unchanged = false            # 內容沒變就不發事件（預設）
max_events_per_minute = 120       # 每來源速率上限，超出丟棄且計數
persist_every_n = 0               # 0 = 不落 events.db（預設）；填 N 表示每 N 筆存一筆稽核紀錄
baseline_max_age_secs = 3600      # 漲跌比較基準的保鮮期（秒）；0 = 永不過期

# ── command：執行一支指令，把 stdout 當成 payload ──────────
[[tick.sources]]
id = "custom-feed"
kind = "command"
enabled = true
interval_secs = 30
command = ["sh", "-c", "curl -s https://example.invalid/api"]  # argv 陣列，不經過 shell 字串解析
json_fields = { level = "/level" }
max_events_per_minute = 60
persist_every_n = 0

# ── file_tail：追蹤一個檔案新增的行 ─────────────────────────
[[tick.sources]]
id = "trade-log"
kind = "file_tail"
enabled = true
interval_secs = 5
path = "~/logs/trades.jsonl"      # 讀取時會 canonicalize，路徑必須真的存在
json_fields = { symbol = "/symbol", qty = "/qty" }
max_events_per_minute = 120
persist_every_n = 0

# ── websocket：掛著一條連線，每則文字訊息就是一筆觀測 ────────
[[tick.sources]]
id = "quote-stream"
kind = "websocket"
enabled = true
url = "wss://example.invalid/stream"   # 非本機主機一律要 wss://（見下方說明）
interval_secs = 5                      # websocket 不輪詢，這個值當作「重連退避的起點秒數」
subscribe = ['{"op":"subscribe","topic":"quotes"}']  # 連上後依序送出的原文訊息，最多 8 則、單則 ≤4KB
headers = { "X-API-Key" = "把金鑰直接寫在這裡" }      # 選配，掛在 WebSocket 升級請求上
ping_interval_secs = 30                # 沒收到任何訊息滿 30 秒就主動送一個 ping；0 = 關閉，其餘最小 5
idle_timeout_secs = 300                # 連 pong 都沒有滿 300 秒就回收連線重連；0 = 關閉，其餘最小 30
json_fields = { price = "/data/price" }
max_events_per_minute = 120            # 串流是最容易灌爆的來源，這道上限務必留著
persist_every_n = 0
```

`id`／`json_fields` 的欄位名有保留字：不能叫 `event`／`source`／`ts`／`kind`，
也不能以 `prev_`／`delta_`／`pct_` 開頭（這三個前綴是下面 D2 自動衍生欄位專用）。
違規的來源會在讀取設定時被停用並記一筆 `warn`，不會拖垮整個 gateway 開機——
其他合法來源照常運作。

### websocket 來源要知道的六件事

前三種來源都是「時間到了去拿一次」，`websocket` 則是掛著一條連線等對方推。
進到系統之後的處理完全一樣：每一則**文字**訊息就是一筆 payload，走同一條
JSON 解析 → `json_fields` 抽取 → delta 衍生 → 去重 → 速率上限 → 環形緩衝＋
事件匯流排的管線。差異只在取得資料的方式，以及下面六點：

1. **網址規則比 http_poll 嚴。** 只收 `ws://` 與 `wss://`。本機以外的主機一律
   要 `wss://`，明文 `ws://` 只准 `127.0.0.1`／`localhost`／`::1`（本機轉接
   程式的場景）。通過 scheme 檢查後，主機會用跟 `http_poll` **同一套** SSRF
   驗證再過一次（內網網段、link-local、雲端 metadata 主機一律拒絕），不合格
   的來源在讀設定時就被停用。
2. **`interval_secs` 改當退避起點。** 連線中斷後等 `max(1, interval_secs)` 秒
   重連，每失敗一次加倍，上限 60 秒，並加上最多 25% 的隨機抖動。連線一旦
   撐過 60 秒視為健康，下次斷線的退避從起點重新算。連不上或連線中途出錯，
   計入 `fetch_error` 丟棄計數。
3. **二進位訊息一律丟棄並計數。** 這條管線只吃文字。收到 binary frame 會記一筆
   `non_text` 丟棄（儀表板與 `tick_dropped_total` 都看得到），連線本身照常
   繼續。整條線一直沒資料但 `non_text` 一直跳，就是你的來源在推二進位格式。
4. **`subscribe` 是原文，不做模板替換。** 連上之後依序送出，最多 8 則、單則
   4096 bytes。想帶 token 就直接寫進字串（`config.toml` 本身就是機密檔案）。
5. **閒置看門狗會把「連著但沒資料」抓出來。** TCP 連線在對方停止推送之後
   可以維持「開著」好幾個小時，光看連線狀態完全正常，只有 tick 停了。所以
   有兩個時鐘同時在跑：`ping_interval_secs`（預設 30，`0` 關閉）——連續這麼多
   秒沒收到任何訊息就主動送一個 WebSocket ping；`idle_timeout_secs`
   （預設 300，`0` 關閉）——連 pong 在內完全沒有任何入站訊息滿這麼多秒，就記
   一筆 `warn` 並回收連線、**立刻**重連（不等退避；重連如果失敗才走上面第 2 點
   的退避）。任何入站訊息（文字、二進位、ping、pong）都會把兩個時鐘歸零。
   兩個都開啟時 `idle_timeout_secs` 必須大於 `ping_interval_secs`，否則 ping
   還來不及被回應就被判定閒置——設反了該來源會在讀設定時被停用。另外兩者都有
   下限：`ping_interval_secs` 非 0 時最小 5、`idle_timeout_secs` 非 0 時最小 30。
   低於下限的來源會被停用而**不是**被自動拉高——回收路徑是立刻重連（安靜的
   feed 不算失敗，不走退避），所以逾時值本身就是「對方永久沉默時多久重連一次」
   的唯一上界，設成 1 秒等於自己做了一台重連風扇；把它悄悄改成 30 只會讓你
   看不到自己設錯了。
6. **`headers` 掛在升級請求上。** 需要 `Authorization`／`X-API-Key` 這類驗證的
   feed，直接寫在來源設定裡（規則見下一節），不必再自己起一支本機轉接程式。

單筆 64KB 上限與 `max_events_per_minute` 速率上限跟其他來源共用同一套實作。
串流是最容易在幾秒內灌進上萬筆的來源，速率上限請當成硬前提看待。

### 自訂 headers（`http_poll` 與 `websocket`）

`headers = { "X-API-Key" = "…" }` 會掛在每一次 `http_poll` 的 GET 上，以及
`websocket` 的升級請求上。`command` 與 `file_tail` 沒有請求可掛，寫了會在讀
設定時被清掉（避免一份憑證莫名其妙被帶著跑）。限制如下，違反的來源直接停用：

| 項目 | 限制 |
|---|---|
| 數量 | 最多 8 個 |
| 名稱 | `^[A-Za-z0-9-]{1,64}$` |
| 保留名稱 | `Host`／`Content-Length`／`Connection`／`Upgrade`／`Transfer-Encoding`／`Sec-WebSocket-*` 一律拒絕（這些由傳輸層自己產生，被蓋掉會直接毀掉連線或偽造握手）|
| 值 | 最長 1024 bytes，只准可見 ASCII——CR／LF、控制字元、非 ASCII 一律拒絕（CR／LF 可以在請求裡插進第二組 header，是典型的 header injection）|

**值一律當成憑證處理**：不會寫進任何 log（連 debug 等級都不會）、不會出現在
`ticks.sources` API（那裡只回一個 `headers_count` 數量）、也不會被帶進喚醒
提示詞。設定被拒絕時的 `warn` 訊息只會提到 header 名稱，不會回顯值。

兩個例外規則：你可以自己指定 `User-Agent`（會覆蓋預設的 `DuDuClaw/1.0`），
但 `Metadata-Flavor: none` 永遠會被強制加上——它是擋雲端 metadata 端點的
防護，不是可以被設定覆蓋的便利選項。

### 網路安全：SSRF 檢查與 DNS re-pin

`http_poll` 的網址與 `websocket` 的非本機主機，除了在讀設定時過一次 SSRF
檢查（拒 `localhost`／內網網段／link-local／雲端 metadata 主機）之外，**每一次
發出請求或建立連線的當下會再解析一次 DNS**：

- 該主機解析出來的**每一個** IP 都必須是公網位址，只要有任何一個落在內網／
  loopback／link-local 就整組拒絕（不是「挑公網那個來連」——半套成功等於沒擋）。
- 通過之後，連線直接釘在剛剛驗證過的那些 IP 上：`http_poll` 走 reqwest 的
  位址釘選，`websocket` 直接對這些 IP 建 TCP 連線，TLS 的 SNI 與憑證驗證仍然
  用原本的主機名（所以釘選不會被拿來繞過憑證檢查）。
- 解析失敗或有內網 IP：**不發出請求**，計一筆 `fetch_error`。

擋的是 DNS rebinding——設定當下解析到公網 IP、真正連線時卻改指向
`169.254.169.254` 的攻擊。另外 `http_poll` 完全不跟隨 HTTP 轉址（一個通過檢查
的網址 302 到內網就是最典型的繞道），連線池則以「解析結果沒變」為條件重用，
所以每輪都重新驗證但不會每秒重建一次 TLS 連線。

本機 `ws://127.0.0.1` 這條明文路徑不走 re-pin：它的位址本來就是內網，也沒有
可以被 rebind 的對象，這是本機轉接程式場景刻意保留的特權。

**解析結果會快取 `dns_ttl_secs` 秒（全域設定，預設 60；`0` = 每次都重新解析）。**
一個 1 秒輪詢的來源如果每次都查 DNS，一天就是 8 萬多次查詢，換來的答案一小時
內大概都一樣。所以每個來源的 task 自己留一份「host:port → 已通過內網檢查的
位址集 ＋ 到期時間」（task 內私有，不用鎖）：TTL 內直接重用釘選好的位址
（`http_poll` 連釘選好的 client 都不用重建、websocket 重連省掉一次解析等待），
過期才重新解析並重新過篩。

這**不會**削弱 rebinding 防護，方向反而是相反的：要翻轉解析答案得先有一次
新鮮的解析，快取讓這種機會變少而不是變多；而且快取住的是**已經驗證過的公網
位址集**，不是一個待驗證的名字。最壞情況是對方真的搬家了，連到舊位址失敗，
下一輪就重新解析。這個 TTL 只作用於監控來源，`web_fetch` 的既有語意不受影響。

### 抽取值是數字字串會自動轉成數字

主流行情 feed（Kraken、Binance）的價格欄位型別是**字串**，例如
`"last": "63669.60000"`。抽取層會把「乾淨的數值字串」轉成 JSON 數字，
`price gt 60000` 這種規則才寫得出來，下面的漲跌欄位也才算得出來。

轉型很保守，寧可不轉也不會把識別碼弄壞：

| 輸入 | 結果 |
|---|---|
| `"63669.60000"`、`"42"`、`"-3"`、`"0"`、`"0.5"`、`" 7.5 "`、`"1e3"` | 轉成數字（整數在 i64 範圍內保持整數，`eq 42` 仍然成立）|
| `"007"`、`"-007"`、`"00.5"` | **不轉**——前導零通常是補零的單號 |
| `"+5"` | **不轉**——feed 不會這樣寫價格 |
| `"inf"`、`"NaN"`、`"1e400"` | **不轉**——非有限值會污染後面每一次比較 |
| `"12 USD"`、`"1,000.5"`、`"0x10"`、`".5"`、`"台積電"` | **不轉**——整個字串必須完整解析成數字 |

原本就是數字、布林、null 的欄位一律原樣保留，不受這條規則影響。

### 抽不到任何設定欄位的訊息不算一筆觀測

設了 `json_fields`、payload 也確實是 JSON，但一個設定的 pointer 都對不上時，
這筆 payload 會被丟棄並計入新的 `no_fields` 丟棄原因：不進事件匯流排、不佔
環形緩衝、不會混進喚醒視窗。真實 feed 的控制訊息就長這樣——Kraken 的
`{"channel":"heartbeat"}` 夾在報價之間，本身沒有價格。早期版本把它當成一筆
空欄位的觀測發出去，結果是規則被沒有內容的事件反覆叫醒、環形緩衝被心跳
塞滿。

兩個不受影響的情況：

- **沒設 `json_fields` 的來源**行為完全不變（照樣送出 `raw_len` ＋原文摘要）
  ——它本來就不是靠抽取欄位工作的。
- **payload 根本不是 JSON**（例如端點回了一頁 HTML 錯誤）仍然會送出
  `raw_len` ＋原文摘要，不會被算成 `no_fields`。這是刻意的：「你的 feed 不再
  吐 JSON 了」必須在儀表板上看得到內容，而不是只剩一個計數器在跳。

丟棄計數會照常出現在 `tick_dropped_total{reason="no_fields"}` 與儀表板的丟棄
分解裡。`no_fields` 一直跳但 `events_emitted` 不動，通常代表你的 pointer 路徑
寫錯了，或是這個來源大部分時間都在推控制訊息。

### 自動衍生的漲跌欄位（不用學新運算子）

每個數值型抽取欄位，只要**上一次出現過的值**還在，系統就會自動多加三個欄位：
`prev_price`（上次的值）、`delta_price`（差值）、`pct_price`（漲跌幅 %）。從沒
出現過的欄位沒得比，這三個欄位就是「缺席」而不是 `0`——缺席的欄位不滿足任何
比較條件，所以不會誤觸發規則。`prev` 是 `0` 時 `pct_` 欄位也是缺席（避免除以零）。

**兩個算出來的欄位（`delta_`／`pct_`）都四捨五入到小數六位，`prev_` 保持原值。**
浮點減法會在末幾位留下雜訊：`63724.8 - 63724.7` 的原始結果是
`-0.10000000000582077`，這個值一旦進了規則，`delta_price gt 0.1` 就會被雜訊
翻轉成命中——明明只跌了一毛。四捨五入之後它就是乾淨的 `-0.1`。六位小數是
刻意留的預算，`0.000002` 這種真的很小的變動仍然完整保留。`prev_` 是 feed 自己
報的值，不做任何加工。整數欄位的 `delta_` 維持整數（`delta_vol eq 200` 仍然
成立），不會被轉成浮點。

**比較基準是「逐欄位」的，不是「上一筆 tick」。** `prev_price` 拿的是
price 這個欄位上次真的有值的那一次，中間夾了幾筆沒帶 price 的觀測都不影響。
這點在真實行情流上是決定性的：Kraken 會在報價之間穿插
`{"channel":"heartbeat"}`，早期版本用「整筆覆蓋」的基準，心跳一來就把價格的
比較基準洗掉，實測**九成的 tick 算不出漲跌**。同理，一筆只帶 `price` 的觀測
不會動到 `vol` 的基準，反之亦然。

**基準會過期：`baseline_max_age_secs`（預設 3600 秒，`0` = 永不過期）。**
逐欄位基準有個副作用——某個欄位停報一整天，隔天第一筆會拿一天前的舊值算
出一個巨大的假漲跌，而規則分不出「真的暴漲」和「中間斷線了一天」。所以每個
欄位的基準都帶時間戳，超過保鮮期就整個丟掉：那一筆的
`prev_`／`delta_`／`pct_` 全部缺席（跟第一筆 tick 完全一樣的語意），並且用
當下這個值重新立基準，下一筆就恢復正常比較。過期也是逐欄位算的，`price`
的基準過期不會影響 `vol`。

保鮮期**沒有下限**，但兩種設法會在讀設定時記一筆 `warn`（只是提醒，來源照常
啟用）：

- **低於 60 秒**：你會得到一個幾乎永遠算不出漲跌的來源。這在功能上是合法的
  （超高頻 feed 確實可能只想比對最近幾秒），但因為它壞掉的樣子跟「設定寫錯」
  一模一樣，所以不讓它安靜發生。
- **比 `interval_secs` 還短**（`http_poll`／`command`／`file_tail`）：這是
  上面那件事的必然版本——這一輪輪詢立好的基準，下一輪還沒到就已經過期了，
  所以**每一筆** tick 都不會有 `prev_`／`delta_`／`pct_`。一天輪詢一次的來源
  配上預設的一小時保鮮期就會踩到。修法是把保鮮期設得比輪詢間隔大，或直接設
  `0` 關掉過期。`websocket` 不做這個檢查：它的 `interval_secs` 是重連退避的
  起點而不是觀測間隔，拿來比較沒有意義。

真的想關掉過期就明確寫 `0`。

## Autopilot 規則範例

一條「漲幅超過 2% 就喚醒交易員代理」的規則，`conditions` 直接用衍生的
`pct_price` 欄位，不用寫任何字串處理：

```json
{
  "name": "twse-2330 漲幅示警",
  "enabled": true,
  "trigger_event": "tick",
  "conditions": {
    "all": [
      { "field": "source", "op": "eq", "value": "twse-2330" },
      { "field": "pct_price", "op": "gt", "value": 2 }
    ]
  },
  "action": {
    "type": "delegate",
    "target_agent": "trader",
    "prompt": "台積電股價短時間內漲幅超過 2%，請查看近期走勢並判斷是否需要行動。",
    "context_ticks": 15,
    "screen": {
      "mode": "local",
      "prompt": "只有這確實是異常波動（不是正常盤中震盪）才回 YES",
      "on_unavailable": "pass",
      "timeout_secs": 10
    }
  }
}
```

- `context_ticks`（選填，預設 10，上限 50，設 0 關閉）：喚醒 `delegate` 時，
  在提示詞尾端附上該來源最近 N 筆觀測值的緊湊摘要，AI 員工看到的是趨勢而不是
  單一個孤立數字。
- `screen`（選填）：規則命中、斷路器放行之後，先問本地小模型一個 YES/NO
  問題，只有 `YES`（或本地模型不可用時的 `on_unavailable` 政策放行）才真的
  執行 `delegate`。細節見下方「初篩」小節。
- `screen` 不是 tick 專屬——它是規則層級的通用欄位，任何 `trigger_event`
  都能掛，只是本文以 tick 場景為主。

## Dashboard 觀測

系統設定 → Autopilot 分頁新增一張唯讀卡片「即時監控來源」（每 15 秒自動刷新），
底層是兩個 admin 專屬 RPC：

- **`ticks.sources`**：回傳目前設定的每個來源（含 `kind`／`enabled`／
  `interval_secs`／`max_events_per_minute`）與即時狀態（`last_tick_ts`、約略
  events/分鐘、累計發送數、六類丟棄原因分別計數：`rate_cap`／`unchanged`／
  `oversize`／`fetch_error`／`non_text`／`no_fields`），以及全域本地初篩通過／攔截／無法
  判定三個計數。自訂 headers 只回一個 `headers_count` 數量——**header 的值
  是憑證，任何 API 都不會吐出來**。
  來源存在但這個行程內從沒發過 tick，回傳全零快照，不是錯誤。
- **`ticks.recent`**：取單一來源最近的觀測值（`source` 必填，`limit` 上限
  50 筆），依時間由舊到新排列，展開卡片可看到每筆的時間戳與欄位內容。

卡片本身是唯讀的——新增或啟用一個來源是改 `config.toml` 的動作，不是儀表板上
可以按的按鈕。

## Prometheus 指標

掛在既有 `/metrics` 端點下：

| 指標 | 標籤 | 說明 |
|------|------|------|
| `tick_events_total` | `source` | 成功發出的 tick 事件累計數 |
| `tick_dropped_total` | `source`, `reason` | 被拒絕的 payload，`reason` 為 `rate_cap`／`unchanged`／`oversize`／`fetch_error`／`non_text`（websocket 二進位訊息）／`no_fields`（抽不到任何設定欄位）之一 |
| `tick_screen_total` | `outcome` | 本地初篩結果，`outcome` 為 `pass`／`drop`／`unavailable` 之一 |
| `tick_wakes_total` | `rule` | 該規則因 tick 觸發且真正執行動作（清過斷路器與初篩）的次數，用 `rule_id` 而非規則名稱（避免使用者自訂文字變成未跳脫的 Prometheus 標籤） |

## 本地初篩層

規則命中、斷路器允許執行之後，若 `action.screen` 存在，會在真正呼叫
`delegate`／`notify`／`run_skill` 之前，先問一次本地推理引擎一個二選一問題。
這一層存在的目的就是省雲端呼叫的錢，所以有幾條硬限制：

- **只走本地推理，永遠不會外呼雲端。** 這一層絕對不會碰到帳號輪替器或雲端
  API 客戶端；不想要雲端判斷的話，乾脆不要設 `screen`，讓規則命中就直接
  `delegate`。
- **判定字串很嚴格。** 只看回覆的第一個以空白分隔的 token，剝除頭尾的 ASCII
  標點符號後，必須完全等於（大小寫不分）`yes` 或 `no`。所以 `NO.`、
  `**YES**`、`"yes"` 都能解析；`I think yes`（判定詞不是第一個 token）、
  `maybe`、純標點、CJK 引號包住的 `「YES」`，一律視為「無法解析」。這個嚴格
  規則是 2026-08-11 驗收時修的：早期版本連小模型加的句點都判不出來，等於
  每次都落到 fail-open，喚醒了本來要省下的雲端呼叫。
- **無法判定時預設放行（fail-open）。** 本地引擎沒掛、逾時、或回覆解析不出
  YES/NO，預設一律當成「放行」，因為 deterministic 條件本來就已經命中了，
  初篩只是省錢用的第二道關卡；想改成保守（無法判定就不喚醒），把
  `on_unavailable` 設成 `"drop"`。
- **timeout_secs** 範圍 1–60 秒，預設 10 秒。
- **用 OpenAI 相容後端時，`inference.toml` 的頂層 `default_model` 一定要一起設。**
  只在 `[openai_compat]` 區塊裡寫 `model` 是不夠的——推理引擎取的是頂層
  `default_model`，沒設就會回 `NoModelLoaded`，初篩因此判定為「不可用」，
  再依 fail-open 預設**整條放行**。整個過程只有一行 debug log，外觀上完全像是
  初篩通過了，實際上根本沒問過模型。設定長這樣：

  ```toml
  # inference.toml
  default_model = "qwen2.5:7b"     # ← 少了這行，初篩永遠靜默不可用

  [openai_compat]
  base_url = "http://127.0.0.1:11434/v1"
  model = "qwen2.5:7b"
  ```

  想確認初篩到底有沒有真的在跑，看儀表板「即時監控來源」卡片的初篩三個計數：
  一直落在「無法判定」就是這個問題。

## 營運注意事項

- **`file_tail` 開機時從檔案結尾（EOF）起算，不會重播歷史內容。** 這是刻意
  設計：gateway 重啟不該把整個既有 log 當成一波新的 tick 炸出來。代價是
  gateway 停機期間新增的行不會補讀。
- **tick 事件預設不落 `events.db`。** 一秒一筆等於一天 8 萬多列，太髒。近期
  歷史只活在記憶體環形緩衝（每來源 256 筆），gateway 重啟就清空。要留稽核
  軌跡的來源，自行設定 `persist_every_n`（例如設 10，表示每 10 筆存一筆）。
- **閒置回收不是丟棄，不會出現在丟棄計數裡。** WebSocket 的閒置逾時回收記的是
  一行 `warn`（含 `recycles_total`），刻意不新增計數器、也不借用 `fetch_error`
  ——一個安靜的 feed 跟一個壞掉的端點是兩回事，混在同一個數字裡只會誤導。要判斷
  來源是不是在空轉，看儀表板的 `last_tick_ts`。
- **`command` 來源需要全域開關。** 光是某個來源寫 `kind = "command"`
  不會生效——`[tick] allow_command_sources` 必須明確設成 `true`，這是
  fail-closed 設計：單一來源的設定不足以授權執行任意程式。
- **本地初篩若指向付費端點，一樣會產生費用。** D4 保證的是「這條程式碼路徑
  永遠不會呼叫帳號輪替器或雲端 API 客戶端」，但本地推理引擎支援的
  OpenAI-相容 HTTP 後端（`inference.toml [openai_compat] base_url`）本身可以
  被操作者設定指向任何相容端點——包含真正的付費雲端 API。如果你把這個
  `base_url` 指到一個計費服務，初篩的每一次呼叫都會真的花錢，「零邊際成本」
  的設計前提就不成立了。確認你的本地後端真的是本地或自架服務。
- **初篩判定只認嚴格的 YES/NO 首詞**（見上一節）。若你的操作者提示詞
  容易誘導模型寫出長篇解釋，初篩會頻繁落到「無法判定」而不是真的攔截，
  請把提示詞寫得直接要求「只回一個字」。
- **喚醒視窗是「最近 N 筆觀測」，不是「觸發當下的快照」。** 同一輪輪詢讀進
  多行時（`file_tail` 一次讀到好幾行、或 `command` 一次吐出多筆），這些行會
  接連寫入環形緩衝，因此附在喚醒提示詞後的視窗可能已經包含「觸發那筆之後
  才到」的 tick。這是刻意的語意選擇：AI 員工被叫醒時看到的是最新現況，而不是
  已經過時幾秒的舊值。若你需要「究竟是哪個值觸發了規則」，用規則模板欄位取得
  （例如 `{price}`、`{pct_price}`）——模板永遠渲染觸發事件本身的欄位。

## 還沒做的部分

- query 簽章式驗證：`headers` 涵蓋 `Authorization`／`X-API-Key` 這類 header
  驗證，但需要「用密鑰對 query string 簽名、且簽章有時效」的 feed（部分交易所
  行情 API）還是得自己在本機起一支轉接程式，再用 `ws://127.0.0.1:...` 接進來。
- 初篩層的雲端 fallback：明確不做——它存在的目的就是擋雲端成本，加了雲端
  退路等於自我推翻。
- 行情專用解析器：目前用 `command` 接既有工具＋`json_fields` 抽取即可，沒有
  另外做交易所專屬的內建解析邏輯。

## 相關文件

- Autopilot 規則引擎本身：[`23-autopilot-engine.md`](23-autopilot-engine.md)
