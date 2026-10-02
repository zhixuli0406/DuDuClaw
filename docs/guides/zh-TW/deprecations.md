# 已棄用名稱

這頁列的名稱都還能用，但將來會拿掉。目前一個都還沒移除：每個舊名稱照舊接受、行為
完全不變；MCP 工具也還留在 `tools/list` 裡，學過舊名稱的模型可以繼續呼叫。

**政策**：舊名稱保留 **兩個 minor 版本**。本頁項目在 **v1.66.0** 標為棄用，預計於
**v1.68.0** 移除；唯一例外是 Gemini CLI runtime（見 [Runtime](#runtime)），它在
**v1.67.0** 標為棄用，預計於 **v1.69.0** 移除。

各介面怎麼標示：

| 介面 | 標示方式 | 還能用嗎？ |
|---|---|---|
| MCP 工具 | `description` 開頭加 `[deprecated → <新工具> <參數>]`；`tool_catalog` 標 `deprecated: true` | 能。仍列在 `tools/list`、仍可呼叫。把它藏起來會讓它「不可呼叫」，那跟棄用緩衝期的用意正好相反 |
| CLI 子命令 | clap `hide = true`——`--help` 看不到，但照樣解析 | 能 |
| `config.toml` 設定值 | 讀取時每個行程 `warn!` 一次；經儀表板寫入時另留審計事件 | 能。已設定的行為絕不會被偷偷換掉 |
| 儀表板 | 只提供新名稱。已存的舊值仍顯示，標「已棄用」 | 能 |
| Agent runtime | 讀取時每個行程 `warn!` 一次；經儀表板寫入時另留 `runtime_provider_deprecated` 審計事件。儀表板不再提供該選項，已存的值標「已棄用」 | 能，解析與執行方式跟以前完全一樣 |

---

## MCP 工具

### wiki：收成一套 `wiki_*`，多一個 `scope` 參數

`wiki_*` 跟 `shared_wiki_*` 原本是兩份幾乎鏡像的 API，差別只在指向哪個 wiki。現在
合成一套，用 `scope: "agent" | "shared"` 區分，預設 `agent`——所以既有的 `wiki_*`
呼叫完全不變。

| 舊 | 新 |
|---|---|
| `shared_wiki_ls` | `wiki_ls` 帶 `scope="shared"` |
| `shared_wiki_read` | `wiki_read` 帶 `scope="shared"` |
| `shared_wiki_write` | `wiki_write` 帶 `scope="shared"` |
| `shared_wiki_search` | `wiki_search` 帶 `scope="shared"` |
| `shared_wiki_stats` | `wiki_stats` 帶 `scope="shared"` |
| `shared_wiki_lint` | `wiki_lint` 帶 `scope="shared"` |

沒動的部分：`.scope.toml` 命名空間 SoT 政策、`wiki_visible_to` 可見度、刪除時
「只有原作者或主 agent」的判定。只有入口合併了。

**`shared_wiki_delete` 刻意不併**。它沒有 agent-local 對應版本，如果做成
`wiki_delete` 帶 `scope="agent"`，等於「新增」一個破壞性能力，而不是「移除」一份
重複。它保留原名，也不標棄用。

### 建立工作：收成一個 `tasks_create`

| 舊 | 新 |
|---|---|
| `schedule_task`（週期性 cron） | `tasks_create` 帶 `schedule="<cron 運算式>"` |

`tasks_create` 多了兩個選填參數：

- **`kind`**——`"task"`（預設，看板任務）或 `"goal"`（自主目標：驗收標準在建立當下
  凍結，由 AI 判官團決定算不算做完；另有選填的 `plan_first` 會先產計畫等人核准）。
  `kind="goal"` 走的是儀表板「交辦」面板同一條程式路徑。
- **`schedule`**——cron 運算式（5 或 6 欄）登記週期性工作；RFC3339 時間點
  （`2026-10-01T09:00:00+08:00`）登記一次性喚醒。兩者都需要 `notify_channel` ＋
  `notify_chat_id` 才能把結果送出去；一次性排程少了它們會直接被拒，而不是建出一個
  永遠送不到的提醒。

`kind="goal"` 不能跟 `schedule` 併用——目標只跑到完成一次。這種組合會整包拒絕，不會
做一半。

**`goals_create` 與 `create_task` 都不標棄用**。`goals_create` 建的是目標「階層」
節點（Initiative → Project → Issue），也就是接手者看到的 why-chain。`create_task`
是把一份明確列出 `steps` 的多步驟計畫交給 TaskSpec 派工器執行，而 `tasks_create`
根本沒有對應的參數——標它棄用等於承諾一個不存在的替代品。兩者的說明文字現在都會
講清楚自己是什麼，並指向 `tasks_create` 處理它們不負責的那種情況。

指派授權檢查（部門 × 階層）在**合併後的單一入口強制一次**，任何分支跑之前先跑。這
是合併的安全重點：呼叫端不能再挑四個舊工具裡檢查最鬆的那個，把跨部門指派洗過去。

### 技能搜尋：收成一個 `skill_search`，多一個 `source` 參數

| 舊 | 新 |
|---|---|
| `skill_bank_search` | `skill_search` 帶 `source="bank"` |

`skill_search` 多了 `source`：

- `"all"`（預設）——已設定的技能 hub **加上** 本機學會的 skill bank，依技能名稱去重；
- `"github"`——只查 GitHub hub；
- `"hub"`——只查策展過的 registry；
- `"bank"`——只查本機學會的 skill bank。

給模型的一句話規則：除非你已經知道這個技能在哪裡，否則不要動 `source`。

skill bank 目前仍是空的 in-memory stub，所以 `source="bank"` 會誠實回報「空的」，
不會偷偷拿 hub 結果充數。

---

## CLI 子命令

舊寫法從 `--help` 隱藏，但照樣解析得到。

### `migrate`

三個語意互不相干的命令，help 文字還得互相澄清：

| 舊 | 新 |
|---|---|
| `duduclaw migrate` | `duduclaw migrate schema`（裸 `duduclaw migrate` 仍是這個意思） |
| `duduclaw migrate-from <platform>` | `duduclaw migrate from <platform>` |
| `duduclaw data-migrate` | `duduclaw migrate data` |

### `export`

四種語意完全不同的匯出，只靠所屬群組區分：

| 舊 | 新 |
|---|---|
| `duduclaw export --out …` | `duduclaw export data --out …`（裸寫法仍是這個意思） |
| `duduclaw audit …` | `duduclaw export audit …` |
| `duduclaw gdpr export <contact>` | `duduclaw export gdpr <contact>` |
| `duduclaw playbook export --agent …` | `duduclaw export playbook --agent …` |

`duduclaw gdpr erase` 與 `duduclaw playbook migrate-soul` 不受影響。

### `acp`

兩個不同協定，只靠 doc comment 的免責聲明區分：

| 舊 | 新 |
|---|---|
| `duduclaw acp`（編輯器端 Agent Client Protocol） | `duduclaw acp client`（裸 `duduclaw acp` 仍是這個意思） |
| `duduclaw acp-server`（A2A agent 對 agent） | `duduclaw acp server` |

---

### `pack`

三個安裝動詞裝的其實一直是同一種東西：一組預先設定好的 AI 員工。`duduclaw pack` 是唯一前門（T5/O2）；`duduclaw expert install`／`expert list` 是同一份程式的別名，舊的 manifest 方言全部照原樣讀取（不做磁碟遷移）。

| 舊 | 新 |
|---|---|
| `duduclaw expert install <src>` | `duduclaw pack install <src>` |
| `duduclaw expert list` | `duduclaw pack list` |
| `expert.toml`（專家包 manifest） | `pack.toml`（`kind = "team"`） |
| `team.toml`（付費團隊劇本） | `pack.toml`（`kind = "team"`、`tier = "premium"`） |
| `preset.toml`（職務組合內容檔） | `pack.toml`（`kind = "preset"`） |

`preset_bindings.toml`（哪個員工套用哪個職務組合）是狀態不是包格式，不在棄用範圍。製作端動詞（`expert pack`／`publish`／`export`／`convert-teams`／`hooks`／`remove`）維持在 `duduclaw expert` 底下。

## 設定值

### `[dispatch] judge`

| 舊值 | 改用 | 理由 |
|---|---|---|
| `evaluator_only` | `mav` | `[dispatch] two_stage_judge`（預設開）本來就先跑便宜的 evaluator，只有完成候選才付判官團的錢，省成本的動機已經被涵蓋，不必為此放寬驗收 |
| `human_only` | `mav` ＋ 每 agent 的 `[capabilities] autonomy_level` 與 `approval_required_tools` | 該等人的地方等人，而不是整個平台停掉機器裁決 |

`mav` 與 `external`不受影響。四個值仍然全部解析得到：已經設定棄用模式的部署行為
完全不變，只會每個行程記一次警告；若該值是從儀表板寫入的，另記一筆
`judge_mode_deprecated` 審計事件。儀表板只提供 `mav` 與 `external`，但已存的舊值
會照樣顯示（加上標籤），不會被偷偷換掉。

---

## Runtime

### Gemini CLI runtime

**Gemini CLI agent runtime**（runtime id `gemini`、執行檔 `gemini`、npm 套件
`@google/gemini-cli`）在 **v1.67.0** 標為棄用，預計於 **v1.69.0** 移除。接手的是
**Antigravity CLI runtime**（`antigravity`，執行檔 `agy`）。

| 舊 | 新 |
|---|---|
| `agent.toml [runtime] provider = "gemini"` | `provider = "antigravity"` |
| `agent.toml [runtime] fallback = "gemini"` | `fallback = "antigravity"` |
| `config.toml [runtime] utility_provider = "gemini"` | `utility_provider = "antigravity"` |
| `config.toml [dispatch] judge_provider = "gemini"` | `judge_provider = "antigravity"` |
| `[team.roles.*] runtime = "gemini"` | `runtime = "antigravity"` |
| `[discovery.attempt.runtimes.gemini]` | `[discovery.attempt.runtimes.antigravity]` |

**還能用的部分**：上面每個舊值到 v1.69.0 之前照樣解析、照樣執行。讀到時每個行程記
一次警告；經儀表板（`agents.create`／`agents.update`）寫入時，另記一筆
`runtime_provider_deprecated` 審計事件。儀表板不再提供 Gemini，但已存的 `gemini`
照樣顯示並標「已棄用」；員工編輯頁的 runtime 選單則補上了 Antigravity。「依所選模型
自動對齊 runtime」的步驟不會再寫入已棄用的 runtime。首次設定精靈的預設值由 Gemini
改成 Antigravity。Docker image 在移除前仍內含 Gemini CLI，`duduclaw doctor` 會列出
`provider` 或 `fallback` 用到已棄用 runtime 的員工。

**不在棄用範圍**：**Gemini API provider**（provider id `gemini`、`GEMINI_API_KEY`、
LLM 層的 `generateContent` 協定、`gemini` provider 帳號）不受影響，Antigravity 的
API key 模式也會用到它。

**原因**：Google 在 2026-06-18 停止以 Gemini CLI 服務免費、Google AI Pro 與 Google AI
Ultra 的個人帳號，並引導這些使用者改用 Antigravity CLI。API key 與企業（Gemini Code
Assist）使用者不受影響，Gemini CLI 本身仍在維護（來源：維護者的
[公告](https://github.com/google-gemini/gemini-cli/discussions/28017)；Google 的
[遷移指南](https://antigravity.google/docs/cli/gcli-migration/)）。Gemini CLI 並沒有被
關閉。

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

**移除前提**：在 v1.69.0 移除這個 runtime 之前，必須先用真的 Gemini API key 驗證過
Antigravity 的 API key 模式。目前只驗過「金鑰無效」的錯誤路徑。

---

## v1.68.0 會發生什麼

上面每個舊名稱都會被移除，只有 Gemini CLI runtime 例外，它保留到 v1.69.0（見下一節）。升級到 v1.67.x 以上之前：

1. 用 grep 掃你的 agent prompt、skill、自動化流程裡的舊 MCP 工具名稱。
2. 用 grep 掃你的腳本、cron 設定、systemd unit 裡的舊 CLI 寫法。
3. 檢查 `config.toml [dispatch] judge` 是不是還停在棄用值。

每個受影響工具說明開頭的 `[deprecated → …]` 前綴就是為了讓你 grep 得到。

## v1.69.0 會發生什麼

Gemini CLI runtime 會被移除（`runtime/gemini.rs`、catalog 項目、Discovery 的 Gemini
family，以及 Docker image 內的 `gemini-cli` 套件）。Gemini API provider 保留。升級
到 v1.68.x 以上之前：

1. 用 grep 掃每個 `agent.toml` 裡的 `provider = "gemini"` 與 `fallback = "gemini"`，
   或直接執行 `duduclaw doctor`。
2. 檢查 `config.toml` 的 `utility_provider`、`[dispatch] judge_provider`、
   `[team.roles.*] runtime` 與 `[discovery.attempt.runtimes.gemini]` 有沒有還設成
   `gemini`。
3. 完成上述 Antigravity 的登入或 API key 設定。
