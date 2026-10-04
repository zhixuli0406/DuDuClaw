# 棄用與移除

這頁列出改名或合併過的公開名稱，哪些已經移除，哪些還在棄用期。

**政策**：舊名稱保留 **兩個 minor 版本**，之後移除。政策沒有改變。v1.69.0 移除了
v1.66.0 標為棄用的所有項目，例外見 [仍在棄用期](#仍在棄用期) 與
[撤下的棄用項目](#撤下的棄用項目)。

棄用期內，各介面怎麼標示：

| 介面 | 標示方式 | 還能用嗎？ |
|---|---|---|
| MCP 工具 | `description` 開頭加 `[deprecated → <新工具> <參數>]`；`tool_catalog` 標 `deprecated: true` | 能。仍列在 `tools/list`、仍可呼叫，因為把工具藏起來會讓它無法呼叫，違背緩衝期的用意 |
| CLI 子命令 | clap `hide = true`，`--help` 看不到，但照樣解析 | 能 |
| `config.toml` 設定值 | 讀取時每個行程 `warn!` 一次；經儀表板寫入時另留審計事件 | 能。已設定的行為不會被偷偷換掉 |
| 儀表板 | 只提供新名稱。已存的舊值仍顯示，標「已棄用」 | 能 |
| Agent runtime | 讀取時每個行程 `warn!` 一次；經儀表板寫入時另留 `runtime_provider_deprecated` 審計事件。儀表板不再提供該選項，已存的值標「已棄用」 | 能，解析與執行方式跟以前完全一樣 |

---

## v1.69.0 已移除

### MCP 工具

八個工具名稱不再宣告，不會出現在 `tools/list`。呼叫時會收到一則工具錯誤，內容指名
替代寫法。工具總數由 249 變為 241。

| 已移除 | 改用 |
|---|---|
| `shared_wiki_ls` | `wiki_ls` 帶 `scope="shared"` |
| `shared_wiki_read` | `wiki_read` 帶 `scope="shared"` |
| `shared_wiki_write` | `wiki_write` 帶 `scope="shared"` |
| `shared_wiki_search` | `wiki_search` 帶 `scope="shared"` |
| `shared_wiki_stats` | `wiki_stats` 帶 `scope="shared"` |
| `shared_wiki_lint` | `wiki_lint` 帶 `scope="shared"` |
| `schedule_task` | `tasks_create` 帶 `schedule="<cron 運算式>"` |
| `skill_bank_search` | `skill_search` 帶 `source="bank"` |

`shared_wiki_delete` 與 `wiki_share` 從來就不是別名，名稱照舊，不受影響。

**員工設定裡還寫著已移除的工具名稱**：`agent.toml [capabilities]` 的工具清單、
提示詞或技能裡可能還有舊名稱。舊名稱的效果依清單而不同：

| 舊名稱寫在哪裡 | 升級後的效果 |
|---|---|
| `allowed_tools` | 這個項目不再比對到任何工具。允許清單只列了舊名稱的員工會失去該能力，請換成新名稱 |
| `denied_tools` | MCP 把關仍會拒絕對應的新寫法（例如帶 `scope="shared"` 的 `wiki_write`）。Claude CLI 旗標不再比對得到，請改寫新名稱，才會在所有地方生效 |
| `approval_required_tools`、`irreversible_tools`、`maybe_irreversible_tools` | 把關仍會套用在對應的新寫法上。沒有帶 `scope="shared"` 的 `wiki_write` 不受影響 |
| `scoped_tools` | 對應的新寫法仍然需要階段授權，授權以清單裡寫的名稱申請與記錄 |
| `config.toml [provenance] sensitive_tools` | gateway 仍會替這個新名稱把關 |

除了 `allowed_tools`，舊名稱仍然發揮把關作用，但名稱已經過時，建議改成新名稱。
`duduclaw doctor` 有一項檢查會找已移除的工具名稱，範圍是：每位員工 `agent.toml
[capabilities]` 的清單、員工目錄根層的提示檔（`SOUL.md`、`IDENTITY.md`、
`CLAUDE.md`、`AGENTS.md`、`GEMINI.md`、`CONTRACT.toml`）、`SKILLS/` 與 `wiki/`
底下的 Markdown，以及 `config.toml` 的 `[provenance]` 與 `[[ccr.allowed_sources]]`。
排程任務與自動化規則裡的提示文字、`evals/` 與 playbook 裡的工具斷言、`.mcp.json`、
共享知識庫都不在檢查範圍內。

三個合併後的入口，行為跟以前一致：

- **wiki**：`wiki_*` 多一個 `scope: "agent" | "shared"` 參數，預設 `agent`，所以既有
  的 `wiki_*` 呼叫完全不變。`.scope.toml` 命名空間政策、`wiki_visible_to` 可見度、
  刪除時「只有原作者或主 agent」的判定都沒有改動。
- **建立工作**：`tasks_create` 有 `kind`（`"task"` 為預設，或 `"goal"`）與
  `schedule`（5 或 6 欄的 cron 運算式登記週期性工作，RFC3339 時間點如
  `2026-10-01T09:00:00+08:00` 登記一次性喚醒）。兩種排程都需要 `notify_channel` 與
  `notify_chat_id` 才能送出結果，少了就會被拒。`kind="goal"` 加上 `schedule` 也會被
  拒。指派授權檢查（部門與階層）在這個入口先跑一次，再進任何分支。
  `goals_create`（Initiative、Project、Issue 階層中的節點）與 `create_task`（把明確
  列出 `steps` 的多步驟計畫交給 TaskSpec 派工器）是不同用途的工具，從沒被棄用。
- **技能搜尋**：`skill_search` 有 `source`：`"all"`（預設，hub 加上本機學會的
  skill bank，依技能名稱去重）、`"github"`、`"hub"`、`"bank"`。skill bank 目前仍是
  空的 in-memory stub，所以 `source="bank"` 會回報空的。

### CLI 舊拼法

下列拼法仍然能被解析，執行時會印一行訊息指名新拼法，並以結束碼 2 離開，不會執行
任何動作。

| 已移除 | 改用 |
|---|---|
| `duduclaw migrate-from <平台>` | `duduclaw migrate from <平台>` |
| `duduclaw audit …` | `duduclaw export audit …` |
| `duduclaw gdpr export <聯絡人>` | `duduclaw export gdpr <聯絡人>` |
| `duduclaw playbook export --agent <員工>` | `duduclaw export playbook --agent <員工>` |
| `duduclaw acp-server` | `duduclaw acp server` |
| `duduclaw expert install <來源>` | `duduclaw pack install <來源>` |

`duduclaw expert install` 與 `duduclaw pack install` 跑的是同一條安裝流程，所以安裝
出來的結果沒有變。儀表板的一鍵安裝、上傳安裝、AI 草稿安裝現在改走 `pack install`。
`duduclaw gdpr erase` 與 `duduclaw playbook migrate-soul` 不受影響。

### `config.toml [dispatch] judge`

`evaluator_only` 與 `human_only`（以及別名 `evaluator`、`human`）已移除。有效值只剩
`mav`（預設）與 `external`。儀表板與 `system.update_config` 會拒絕寫入已移除的值。

設定檔裡還留著舊值時，閘道的處理方式：

| 舊值 | 閘道現在怎麼做 | 你該怎麼辦 |
|---|---|---|
| `evaluator_only` | 改用 `mav` 驗收。驗收比以前嚴格，判官費用會增加。`[dispatch] two_stage_judge`（預設開）仍會先跑便宜的 evaluator，只有完成候選才付判官團的費用 | 改成 `judge = "mav"` |
| `human_only` | 不會退回機器驗收。每件送驗的工作都停在 `needs_human`，暫停原因顯示為系統問題，並附上改法 | 改成 `judge = "mav"` 或 `external`。需要人工把關的員工，改用每個員工的 `[capabilities] autonomy_level` 與 `approval_required_tools`。要放行已停住的工作，可在任務上按「標記完成」；或改好設定後按「重試」，重試會把任務放回 `pending`，清掉已存的結果摘要與認領，並把選填的備註當成下一輪的指示；輪次計數會接著算，已經寫出的檔案不會被刪除 |

兩個值都會每個行程警示一次。每個閘道行程、每個資料目錄各寫一次，時間點是第一件工作
進入驗收時，內容是一筆稽核事件 `judge_mode_removed` 與一筆 Activity Feed 通知；閘道
重啟後會再寫一次。稽核事件 `judge_mode_deprecated`
不再產生。`duduclaw doctor` 會列出這個狀況：`human_only` 顯示為失敗（每件送驗的工作
都會停住），`evaluator_only` 顯示為警告，`config.toml` 讀不到或解析失敗時顯示警告，
說明未能檢查。

### 升級前要做的檢查

1. 用 grep 掃你的 agent 提示詞、技能與自動化流程裡的八個已移除 MCP 工具名稱，並
   檢查每個 `agent.toml [capabilities]` 的工具清單。
2. 用 grep 掃你的腳本、cron 設定與 systemd unit 裡的六個已移除 CLI 拼法。已移除的
   拼法現在會以結束碼 2 離開，不再執行。
3. 檢查 `config.toml [dispatch] judge` 是否還是 `evaluator_only` 或 `human_only`。
4. 執行 `duduclaw doctor`，它會列出殘留的已移除工具名稱與已移除的判官設定值。

---

## 仍在棄用期

### 板模包的舊格式

板模包的舊 manifest `expert.toml`、`team.toml` 與產業包目錄結構已棄用。v1.69.0 仍然
讀取這些格式，沒有移除任何一個。它們會跟改寫後的付費板模一起，在之後的版本移除，
目前沒有訂版號。

**為什麼先留著**：新格式 `pack.toml` 目前只能以職務 preset（`kind = "preset"`）安裝。
`duduclaw pack install` 讀得懂 `pack.toml` 的團隊包或產業包，但接著會把目錄交給專家包
安裝程式，而那個程式只認 `expert.toml`（或 Claude Code plugin、單一 Agent Skill），
其他一律拒絕。在安裝程式能安裝 `pack.toml` 的團隊包與產業包之前，舊格式不能拿掉。
目前該怎麼寫，見 [製作自己的專家包](build-your-own-pack.md)。

| 現況 | 狀態 |
|---|---|
| `expert.toml`（團隊包與產業包） | 繼續使用。已棄用，仍可讀取 |
| `team.toml`、產業包目錄 | 已棄用，照原樣讀取，不做磁碟遷移 |
| `pack.toml` 且 `kind = "preset"` | 職務 preset 的現行格式 |
| `pack.toml` 且 `kind = "team"` 或 `"template"` | 可以讀取與檢視（`pack inspect`），目前還不能安裝 |

### Gemini CLI runtime

**Gemini CLI agent runtime**（runtime id `gemini`、執行檔 `gemini`、npm 套件
`@google/gemini-cli`）在 **v1.67.0** 標為棄用。移除時間由 v1.69.0 改到 **v1.70.0**。
接手的是 **Antigravity CLI runtime**（`antigravity`，執行檔 `agy`）。

| 舊 | 新 |
|---|---|
| `agent.toml [runtime] provider = "gemini"` | `provider = "antigravity"` |
| `agent.toml [runtime] fallback = "gemini"` | `fallback = "antigravity"` |
| `config.toml [runtime] utility_provider = "gemini"` | `utility_provider = "antigravity"` |
| `config.toml [dispatch] judge_provider = "gemini"` | `judge_provider = "antigravity"` |
| `[team.roles.*] runtime = "gemini"` | `runtime = "antigravity"` |
| `[discovery.attempt.runtimes.gemini]` | `[discovery.attempt.runtimes.antigravity]` |

**為什麼延後**：原先公告的移除前提，是用真的 Gemini API key 驗證 Antigravity 的 API
key 模式。驗證時發現，預設權限等級的 Antigravity 員工呼叫平台工具時，會被 Antigravity
CLI 自己拒絕。修正隨 v1.69.1 出貨：閘道會在操作者的
`~/.gemini/antigravity-cli/settings.json` 補上兩條放行 `duduclaw` MCP 伺服器的規則
（細節見[多 runtime](../../features/zh-TW/13-multi-runtime.md)）。移除仍排在
v1.70.0，前提是這個修正出貨後，再用真的 Gemini API key 重新驗證 Antigravity。重新
驗證尚未進行。

**還能用的部分**：上面每個舊值照樣解析、照樣執行。讀到時每個行程記一次警告；經儀表板
（`agents.create`／`agents.update`）寫入時，另記一筆 `runtime_provider_deprecated`
審計事件。儀表板不再提供 Gemini，但已存的 `gemini` 照樣顯示並標「已棄用」；員工編輯頁
的 runtime 選單有 Antigravity。「依所選模型自動對齊 runtime」的步驟不會再寫入已棄用的
runtime。首次設定精靈的預設值是 Antigravity。Docker image 仍內含 Gemini CLI，
`duduclaw doctor` 會列出 `provider` 或 `fallback` 用到已棄用 runtime 的員工。

**不在棄用範圍**：**Gemini API provider**（provider id `gemini`、`GEMINI_API_KEY`、
LLM 層的 `generateContent` 協定、`gemini` provider 帳號）不受影響，Antigravity 的
API key 模式也會用到它。

**為什麼要淘汰 Gemini CLI**：Google 在 2026-06-18 停止以 Gemini CLI 服務免費、
Google AI Pro 與 Google AI Ultra 的個人帳號，並引導這些使用者改用 Antigravity CLI。
API key 與企業（Gemini Code Assist）使用者不受影響，Gemini CLI 本身仍在維護（來源：
維護者的
[公告](https://github.com/google-gemini/gemini-cli/discussions/28017)；Google 的
[遷移指南](https://antigravity.google/docs/cli/gcli-migration/)）。Gemini CLI 並沒有
被關閉。

**遷移步驟**：

1. 在 `agent.toml` 把 `[runtime] provider = "gemini"`（以及 `fallback = "gemini"`）
   改成 `"antigravity"`。
2. 驗證方式：用 Google 帳號登入的人，在主機的終端機執行 `agy` 完成登入。原本用 API
   key 驗證 Gemini CLI 的人，設定 `config.toml [antigravity] auth = "api_key"`，繼續
   用同一把 Gemini API key（`gemini` provider 帳號，或 `GEMINI_API_KEY`）。
3. 模型名稱：以 `agy models` 列出的名稱為準；從 Gemini CLI 設定抄過來的 id 不一定會
   選到同一個模型。
4. 執行 `duduclaw doctor`，它會列出 `provider` 或 `fallback` 用到已棄用 runtime 的
   員工。

在 v1.70.0 移除之前，請檢查每個 `agent.toml` 有沒有 `provider = "gemini"` 與
`fallback = "gemini"`，也檢查 `config.toml` 的 `utility_provider`、
`[dispatch] judge_provider`、`[team.roles.*] runtime` 與
`[discovery.attempt.runtimes.gemini]` 有沒有還設成 `gemini`。

---

## 撤下的棄用項目

下列名稱曾公告為棄用，現在是正式支援的行為。

| 名稱 | 狀態 | 原因 |
|---|---|---|
| `duduclaw data-migrate` | 保留為 `duduclaw migrate data` 的隱藏別名 | 已出貨的 DuDuClaw OS 映像，在唯讀根檔案系統的開機 unit 執行 `duduclaw data-migrate --run`，這個拼法必須繼續可用。新的腳本請用 `duduclaw migrate data` |
| `duduclaw migrate` | 正式支援，等同 `duduclaw migrate schema` | 裸形式是文件化的行為 |
| `duduclaw export --out …` | 正式支援，等同 `duduclaw export data --out …` | 裸形式是文件化的行為 |
| `duduclaw acp` | 正式支援，等同 `duduclaw acp client` | 裸形式是文件化的行為 |
| `duduclaw expert list` | 保留 | 它列的是已安裝的紀錄，跟 `duduclaw pack list`（已安裝的包加上可安裝的清單）顯示的內容不同 |
| `preset.toml` | 保留 | 它是職務 preset 的儲存格式。`pack.toml` 搭配 `kind = "preset"` 是另一種撰寫方式 |

`preset_bindings.toml`（哪個員工套用哪個職務 preset）是狀態，不是包格式，從沒被棄用。
製作端指令 `expert pack`、`publish`、`export`、`convert-teams`、`hooks`、`remove`
維持在 `duduclaw expert` 底下。
