# 錄製轉 Skill

> 示範一次給 agent 看:一段瀏覽器或桌面的操作示範,變成可重播的 SKILL.md 草稿,而且在真人核准之前絕不會執行。

---

## 這是什麼

有些 SOP 用做的比用說的容易:去 ERP 查月報、填一張表單、走一遍供應商入口網站。Recording to Skill 把這個迴路接了起來。你親自操作一次,agent 同步錄製;錄下的內容蒸餾成 SKILL.md 草稿;管理員在儀表板審核並核准;通過之後才成為已安裝的 skill。

五個 MCP 工具驅動整個迴路:

| 工具 | 用途 |
|---|---|
| `browser_record_start(url, name?, headless?, max_seconds?)` | 開一個帶 tracing + HAR 的真實瀏覽器,錄下人工示範 |
| `browser_record_stop(id)` | 停止;落地 `trace.zip` / `session.har`(已遮罩)/ `actions.json` |
| `desktop_record_start(name?, max_seconds?)` | 桌面錄製:每秒 1 張截圖 + 前景視窗標題 |
| `desktop_record_stop(id)` | 停止桌面錄製 |
| `skill_from_recording(id, name?)` | 把錄製內容蒸餾成 SKILL.md 草稿並送審 |

## 兩個錄製器

**瀏覽器**(需要本地 Node.js + Playwright):一個真實的 Playwright context 開著 tracing、HAR 擷取與注入的動作錄製器,由一支實體化進錄製目錄的小 Node script 驅動。你在有頭視窗裡示範;`headless=true` 僅供驗證用。模組探索可用 `DUDUCLAW_PLAYWRIGHT_NODE_PATH` 與 `DUDUCLAW_NODE` 覆寫。

**桌面**(目前僅 macOS,需要螢幕錄製權限):一個 detached worker 子程序每秒擷取一張截圖,加上前景視窗標題。它刻意**不錄任何按鍵**,沒有實作輸入事件流,所以蒸餾只靠視窗切換序列與截圖。

## 憑證絕不離開這台機器

HAR 在停止當下**就地**遮罩,任何下游讀取之前:

- 憑證類 header(`Authorization`、`Cookie`、`Set-Cookie`、`x-api-key` 及同類)的值被替換。
- 所有 cookie 值被替換。
- 名稱看起來像憑證的 query 參數與 JSON body 欄位(`token`、`secret`、`password`、`api_key`、`session`、`signature`……)被替換。

每個替換都變成一個 `<env:VAR>` 佔位符,蒸餾出的 SKILL.md 在 frontmatter 的 `requires_env` 之下列出這些變數:skill 記錄自己需要*哪些*憑證,但從不內含任何一個。

## 蒸餾與核准閘

`skill_from_recording` 解析遮罩後的 HAR(非靜態 API 呼叫:method、URL、body 骨架)連同 UI 動作序列,讓 LLM 蒸餾出一份 SKILL.md 草稿(frontmatter:`name` / `trigger` / `skill_type` / `requires_env`)。

草稿**永遠不會直接落進可載入的 skill 庫**:

1. 送審前先跑確定性安全掃描(含 prompt-injection 規則集);High/Critical 風險直接拒絕,fail-closed。
2. 通過者暫存在隔離的草稿區(`~/.duduclaw/skills-drafts/<id>/SKILL.md`),並經共用的 ApprovalBroker 建立待審紀錄、送到真人面前。
3. 只有儀表板的核准動作會安裝 skill,而且該安裝會再跑一次自己的重掃。被駁回的草稿留在隔離區。

桌面來源的 skill 帶 `skill_type: desktop-sop`,以 computer-use 任務重播:一步一步、每步之後截圖驗證、遇到第一個失敗就停。

## 能力閘

五個工具全部 deny-by-default。MCP dispatch gate **同時**要求呼叫方 agent 的 `agent.toml` 有 `[capabilities] recording = true`,**且**呼叫者的 key 帶有 `recording` MCP scope,缺任何一個就拒絕,fail-closed。錄製是操作者按 agent 逐一開啟的東西,不是任何 agent 伸手就拿得到的。

## 失控防護與檔案系統衛生

- 錄製絕不無聲:開始與停止各自產生明確的回覆與 log 訊號。
- 硬性自動停止預設把每段錄製限制在 30 分鐘(最高可設 2 小時),避免忘了關的錄製無限跑下去。
- 產物放在 `~/.duduclaw/recordings/<id>/`,權限僅限擁有者(目錄 700、檔案 600;Windows 繼承 profile ACL)。
- 錄製 id 遵循嚴格的 `rec-<timestamp>-<hex>` 格式,任何檔案系統操作前先驗證。id 是路徑元件,不合格式一律拒絕。
- 超過 50 MB 的 HAR 不在行程內解析(OOM 防護);`*_record_stop` 最多等 30 秒讓 worker 沖寫完產物。

## 限制

| 項目 | 限制 |
|---|---|
| 錄製長度 | 預設 30 分鐘,硬上限 2 h |
| 行程內解析的 HAR | ≤ 50 MB |
| 桌面輸入擷取 | 僅視窗標題,不含按鍵 |
| 桌面平台 | macOS(需螢幕錄製權限) |
| 安裝路徑 | 僅限核准管線,不可直接安裝 |
