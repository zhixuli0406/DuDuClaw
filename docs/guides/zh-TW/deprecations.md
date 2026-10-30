# 棄用與移除

## 舊版員工記憶檔

`MEMORY.md` 標為 legacy，僅互動式 agent runner 仍會注入，頻道、精簡模式與任務沙箱不讀取，也不包含在 Agent File 匯出中。管理者維護的服務對象介紹請使用原生 `USER.md`；學到的事實使用既有記憶工具。既有檔案不搬移、不刪除，目前沒有排定移除版本。


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

## 沒有棄用期的變更（v1.74.0）

這些變更會讓既有的呼叫方失效，而且沒有兩個版本的緩衝期，因為它們是補上漏洞，不是改名。

### 訂閱登入 RPC 需要 `accept_subscription_risk`

自己寫的 WebSocket／RPC 用戶端呼叫下列方法時要帶 `accept_subscription_risk: true`（JSON 布林值），否則 gateway 會拒絕，錯誤碼 `subscription_risk_not_accepted`：

- `auth.cli_login.start`
- `accounts.setup_token_start`
- `type = "oauth"` 的 `accounts.add`
- `config.raw.set`，當送出的 `config.toml` 新增了儲存檔裡沒有的 `type = "oauth"` `[[accounts]]` 帳號

請在使用者讀過訂閱登入風險說明之後才送出這個參數。API 金鑰帳號與其他進階編輯不受影響，dashboard 勾選後本來就會送。操作者指令 `duduclaw auth device --provider copilot` 現在要加 `--accept-subscription-risk`，呼叫它的腳本要補上這個旗標。

DuDuClaw OS 的首次設定（OOBE）也會呼叫 `auth.cli_login.start`。v0.4.0 以前的 OS 映像內嵌平台 v1.73.4，不受影響；OS shell 送出旗標的修改在 [DuDuClaw-OS #21](https://github.com/zhixuli0406/DuDuClaw-OS/pull/21)，OS 映像內嵌平台 v1.74.0 以上之前必須先出貨。

`config.raw.set` 只在出現新的帳號 id 時要求同意。在進階編輯器裡修改既有的 `oauth` 帳號（換 token，或改 `provider`）不會再問一次。詳見[訂閱登入風險告知](../../features/zh-TW/13-multi-runtime.md#訂閱登入風險告知)。

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

## 已移除的 CLI runtime

| Runtime | 移除版本 | 棄用預告期 | 操作者需採取的動作 |
|---|---|---|---|
| Gemini CLI（`gemini`） | v1.75.0 | 自 v1.67.0 棄用 | 自行選擇支援的 runtime 與相容模型 |
| Antigravity（`antigravity`、`agy`） | v1.77.1 | 無 | 自行選擇支援的 runtime 與相容模型 |

員工 `[runtime] provider` 與 fallback、judge、`[team.roles]`、
`config.toml [runtime] utility_provider` 若仍指定已移除的 runtime，系統會拒絕執行，
並要求操作者改選其他 runtime 與模型，不會自動改用其他廠商。
`gemini-*` 模型 id 不再對應任何 CLI runtime。Runtime 目錄、執行路徑、
Dashboard 選項與 Docker image 都已移除這些 CLI。

Gemini API provider（`generateContent`）、`GEMINI_API_KEY` 與 OpenAI 相容的
Gemini 預設值繼續支援。先前留在本機的 CLI 設定檔，以及寫入作業系統使用者
CLI 設定檔的權限規則不會自動刪除，操作者可自行清理。

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
