# 錄製 → 技能（Recording to Skill）

把一次人工示範（瀏覽器操作或桌面操作）錄下來，蒸餾成一份可重放的 SKILL.md 草稿，
經管理員審批後安裝成正式技能。適合「教 AI 員工一個查報表 / 填單 SOP」的場景。

## 前置條件

1. **啟用能力（預設關）**：在目標 agent 的 `agent.toml` 加上：

   ```toml
   [capabilities]
   recording = true
   ```

   沒開這個開關時，五個錄製工具在 MCP dispatch 入口一律被拒（fail-closed）。
2. **MCP scope**：外部金鑰需帶 `recording` scope（內建 agent 的預設 principal 已含 Admin）。
3. **瀏覽器錄製**需要 Node.js 與 playwright 模組：

   ```bash
   npm install -g playwright
   npx playwright install chromium
   ```

   找不到模組時可用環境變數指定：`DUDUCLAW_PLAYWRIGHT_NODE_PATH=/path/to/node_modules`、
   `DUDUCLAW_NODE=/path/to/node`。
4. **桌面錄製**目前僅支援 macOS，且需授予「螢幕錄製」權限
   （系統設定 → 隱私權與安全性 → 螢幕錄製）。

## 工具總覽

| 工具 | 用途 |
|------|------|
| `browser_record_start(url, name?, headless?, max_seconds?)` | 開一個帶 tracing + HAR 的真實瀏覽器，人工示範流程 |
| `browser_record_stop(id)` | 停止並落地 `trace.zip` / `session.har`（自動脫敏）/ `actions.json` |
| `desktop_record_start(name?, max_seconds?)` | 桌面錄製：每秒截圖＋前景視窗標題（不記錄鍵入內容） |
| `desktop_record_stop(id)` | 停止桌面錄製 |
| `skill_from_recording(id, name?)` | 把錄製蒸餾成 SKILL.md 草稿並送審 |

錄製檔落在 `~/.duduclaw/recordings/<id>/`（目錄權限 700），並有 30 分鐘
（可調，上限 2 小時）自動停止的安全上限。

## 典型流程

1. 對 agent 說「我示範一次查報表，你錄下來」→ agent 呼叫
   `browser_record_start(url="https://erp.example.com", name="查月報 SOP")`。
2. 人在開出來的瀏覽器視窗完成整個流程，關掉視窗或請 agent 呼叫
   `browser_record_stop(id)`。
3. agent 呼叫 `skill_from_recording(id)`：
   - 解析脫敏後的 HAR（非靜態資源的 API 呼叫：method / URL / body 骨架）
     ＋ UI 操作序列，交給 LLM 蒸餾成 SKILL.md
     （frontmatter 含 `name` / `trigger` / `skill_type` / `requires_env`）。
   - 先過確定性安全掃描（含 prompt-injection 規則），High/Critical 風險直接擋下。
   - 通過後寫入隔離草稿區 `~/.duduclaw/skills-drafts/<id>/SKILL.md` 並建立審批單。
4. 管理員在 dashboard 審批中心核准 → 技能自動安裝生效；駁回則留在草稿區。

## 安全設計

- **不背景靜默錄製**：開始／結束都有明確回覆與 log 訊號。
- **HAR 脫敏**：`Authorization` / `Cookie` / `Set-Cookie` 等 header 值、所有 cookie 值、
  token 類 query 參數與 JSON body 欄位，一律替換成 `<env:VAR>` 佔位符；
  蒸餾出的 SKILL.md 以 `requires_env` 列出需要的環境變數，絕不含真實憑證。
- **桌面錄製不碰鍵入內容**：只記「切換到哪個視窗」；輸入事件流（rdev）尚未實作。
- **絕不直接進技能庫**：蒸餾產物一律走自建技能審批管線（草稿隔離區 → 人工核准 → 安裝），
  安裝時還會再跑一次安全掃描。

## 已知限制

- 桌面錄製目前是「截圖＋前景視窗」降級版：無輸入事件流，蒸餾僅依據視窗切換序列。
- 桌面重放本質上是 computer use 任務（`skill_type: desktop-sop`），重放時逐步執行、
  每步截圖驗證、失敗即停。
- 瀏覽器錄製需要本機可用的 Playwright；`headless=true` 僅適合驗證用途
  （人工示範請用預設有頭模式）。
