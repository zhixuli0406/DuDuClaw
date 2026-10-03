# 團隊即員工 (Team-as-Agent)

> 一位 AI 員工，內部分四個角色：規劃 / 執行 / 審核 / 合成。每個角色可以跑在不同廠商的模型上。對你來說，它仍是一位員工、一個聲音。

---

## 目前狀態

**WP-1 到 WP-5 已落地：基礎元件、角色運行的載體、驅動一輪的 composer，以及寫出封包的 `team_handoff` 工具。** 一輪現在可以從 規劃 → 執行 → 審核 完整跑完。我們針對一個真實 goal 跑了十四輪實機測試（Claude 規劃、Codex 執行、Claude 審核），暴露出下文描述的整合缺陷；第十四輪被既有的判官接受。

| 項目 | 狀態 |
|---|---|
| `[team]` 設定 schema（全域 + 每位員工，逐欄 cascade） | **已出貨** (WP-1) |
| 規格驗證（runtime 白名單、model↔runtime 綁定、審核者家族規則、effort） | **已出貨** (WP-1) |
| `TaskPacket`，跨角色交接的型別與其上限 | **已出貨** (WP-1)，見 [spec/task-packet.md](../../spec/task-packet.md) |
| 可拆性閘（預設 Solo，L0/L1 規則） | **已出貨** (WP-1)，為純函式 |
| 暫時性**角色成員**（每角色 `(runtime, model, effort)` 骨架、立即拆除、獨立的速率與容量預算） | **已出貨** (WP-2) |
| 每角色 effort，在 `duduclaw-core::effort` 解析一次，spawn 時依各 runtime 的寫法輸出 | **已出貨** (WP-3) |
| 每任務規格凍結（`tasks.team_spec_json`）、閘的呼叫點、三階段一輪、預算降級鏈、逐角色歸因（`role_turns.jsonl`） | **已出貨** (WP-4) |
| `team_handoff` MCP 工具（封包寫入者）、單一 leg 上的封包 fan-out、`constraints` / `audience` 永不裁剪的渲染 | **已出貨** (WP-5) |
| 成員的**原生**工具事件持久化為稽核證據；**產物收據**（path / bytes / sha256）進入審核者 prompt 與 settle 的判官輸入 | **已出貨**（實機第 8 輪） |
| codex 的結構化判官輸出（`codex exec --output-schema`） | **已出貨**（實機第 8 輪） |
| 角色×模型能力矩陣：執行 + 審核格、瓶頸探測、`role_model_matrix.toml` | **已出貨** (P2)，見[哪個角色用哪個模型 (P2)](#哪個角色用哪個模型-p2)；P2b 全團隊 2×2 探測完成一次四臂實機煙霧測試（四個獨立審核判定全為 FAIL）；樣本為 unresolved，且 composer 尚未讀取該檔 |
| 逐角色成本報表、儀表板團隊卡片 | 規劃中 |
| 任務詳情「角色」分頁（階段狀態、模型與實測 token 數） | **已出貨**；封包／工具下鑽與美元成本仍在規劃中 |

自 v1.66 起 `[team] enabled` 預設為 **`true`**。這只是一個旗標的改變，實際會跑什麼並沒有變：要成團仍須三件獨立的事同時成立，而從未寫過 `[team]` 區段的安裝，行為與以往完全相同。

1. **規格必須通過驗證。** 沒有 `[team.roles]` 時，執行與審核都會 cascade 到員工自己的 runtime 與 model，因此共用同一個模型家族，而與執行者同家族的審核者會被直接拒絕（見[規則 3](#3-審核者不得與執行者同模型家族)）。實務上，指名第二家廠商才會讓團隊真正啟用，旗標只是不再擋路。
2. **閘必須判定 Team。** `auto` 需要四個訊號中的三個；一般任務得到 Solo，逐位元組不變。
3. **預算必須付得起一輪。** 若任務的 `[dispatch.team_budget]` 連一輪完全降級後的第一輪都付不起，會改走 Solo，而不是為了一件從未開始的工作停下來等人處理。

`enabled = false` 仍是真正的終止開關，員工明寫的 `false` 仍然勝過全域的 `true`。

(1) 的拒絕刻意保持**安靜**：未設定的部署只會得到一行 `debug!`，不寫稽核列。否則每個安裝的每個 goal 任務都會被蓋上一列 `team_refused`，真正的拒絕就找不到了。明寫 `enabled = true`，或設了角色卻驗證失敗的操作者，仍會得到吵版、有稽核的拒絕。

團隊 goal 被接受時，TaskPacket 中明確列名的檔案會歸檔到員工的 `attachments/` 區，即使這些檔案是 shell 指令建立、沒有原生 Write 事件。歸檔會重新檢查每個來源是員工工作區內的一般檔案，並遵守與一般 goal 交付物相同的大小上限。任務詳情的「角色」分頁在任務 Viewer 權限下讀取 `tasks.role_turns`，未知的 token 用量顯示為未知，而不是零。

逐角色成本帳本會在 provider 回報 token 時，把 Claude CLI 與其他 runtime 的用量記在 goal 任務 ID 之下。團隊角色會等它的用量寫入完成，該階段才結束。未回報的 token 維持未知；估算值與實測用量分開標示。
失敗的角色列也會記錄階段邊界（`failure_edge`）與封閉集合的 `fault_side`。寫入者會把含糊的失敗標為 `unknown`，觀察保真度缺漏或無法辨識的階段不能把責任歸給模型。這些欄位只用來定位症狀，在其餘歸因閘接上之前，不授權據此進行 playbook 學習。

角色成員的 `SOUL.md` 先放一段穩定的角色合約，接著是獨立的快取邊界，再接員工的身分文字（若有）。更動該身分文字不會改變角色合約。任務標題、封包與驗收標準放在每輪的派工 prompt，不再為每個短命成員重寫 system 前綴。明確的邊界適用於 Direct API 快取；CLI runtime 自行管理快取。在宣稱成本下降之前，provider 的快取命中仍需要量測。
Claude CLI 的角色 spawn 會把主代理與子代理的快取 TTL 設為一小時；其他 agent 的 spawn 維持既有的 TTL 行為。
團隊一輪開始時，來源對話會透過既有的 goal 通知路徑收到一行進度訊息。其預估分鐘數等於設定的進度間隔乘以三個必要階段，這是進度估計，不是完成保證。

> **仍是需要你盯著的實驗。** 已有一次正式環境的團隊一輪完整被接受。一次單案例、四臂的全團隊實機探測，每一臂都走到了獨立審核者，但每個矩陣格仍是 `unresolved`，無法可靠地挑選正式環境的模型。v1.66 預設值翻轉，移除的是一個讓這條路徑從未被執行的旗標，並沒有把它變成經過量測的正式環境勝利。只要你在 `[team.roles]` 指名第二家廠商，請盯著最初幾輪；`enabled = false` 與 `gate = "always_solo"` 都能讓你退回去。

---

## 構想

**員工**是你唯一看得到的單位：一個 agent 目錄、它的 SOUL.md、通道、記憶與 playbook。這一點不變。

**團隊**是一位員工*內部*的角色分組：

| 角色 | 工作 | UI 用語 |
|---|---|---|
| `planner` | 拆解目標並規劃 | 規劃 |
| `executor` | 做事；可以 fan-out | 執行 |
| `verifier` | 獨立地接受或駁回 | 審核 |
| `utility` | 摘要、分類、雜務；不佔 spawn 名額 | 合成 |

每個角色綁定自己的 `{runtime, model, effort}`。員工仍以一個聲音說話，團隊只是一位員工完成一項任務的實作細節。

---

## 預設 Solo

團隊**不是**開機就待命的四個角色。每個任務都要先過可拆性閘，多數任務維持今天的單一 agent 路徑，逐位元組不變。

理由出自 Anthropic 自己的指引：「當工作是一條相依的鏈，或能放進單一 context 時，orchestrator 要為規劃、交接與合併付費，而單一模型這些都是免費得到的。」OneFlow（arXiv:2601.12307）發現，單一 agent 逐輪迴圈能與同質的工作流打平，同時保持快取溫熱；arXiv:2609.19759 則發現，多 agent 的優勢只在長時程*且*相依稀疏時才會出現。

閘是零 LLM、確定性的：

**沙箱規則，最先檢查：** `agent.toml [container] sandbox_enabled = true` 的員工一律 Solo（理由 `sandbox_enabled`）。這條規則排在所有模式之前，`always_team` 也蓋不過它，所以開了沙箱的員工不會組成團隊，也不會有角色成員在主機上執行；它的 goal 回合在[任務沙箱](../../guides/zh-TW/task-sandbox.md)裡以 Solo 執行。

**硬性排除，一律 Solo：** 即時通道回合（由門面回答）、仍在等核准的 plan-first goal、含不可逆動作的計畫，或 goal loop 剩餘預算不足 3 輪。

**四個訊號。** 三個以上觸發 ⇒ 成團：

1. **量大**：至少 4 個可獨立執行的工作項目，且實測相依樞紐為零。
2. **脈絡**：任務放不進單一 context window。
3. **能力差距**：執行者與規劃者候選的差距，至少達到能力矩陣宣告的最小可偵測效應（MDE）。*低於* MDE 的差距是雜訊（Miller, arXiv:2411.00640），不計入。
4. **長時程**：至少 3 條驗收標準，且任務會產出真實的產物。

**恰好兩個訊號 ⇒ 灰帶。** 規劃者跑一次（這個呼叫本來就是任務要付的），然後依計畫實際拆出的結果重跑閘。絕不擲硬幣。

**其餘 Solo**，而 Solo 判定仍可附帶一個 effort 提示：Anthropic 的成本指引記載，調 effort 勝過改架構，所以這條路徑上最便宜的收益通常是一個旋鈕，而不是一個團隊。

未量測的訊號就是不觸發，這讓閘偏向 Solo。這個方向是刻意的，昂貴的錯誤是為了一個 agent 自己就能做完的工作而成團。

每個判定都帶著穩定的原因代碼，之後可以拿來對照任務的實際結果評分。在這份校準獲得統計支持之前，儀表板會把閘標示為實驗性。從未被評分過的閘只是一個假設。

---

## 設定角色

全域預設放在 `config.toml`，每位員工的覆寫放在該員工的 `agent.toml`。兩者使用相同的形狀：

```toml
[team]
enabled = true             # 自 v1.66 起預設為 true；false 是終止開關
executor_fanout = 1        # 1..=3；超出範圍會被夾住，不會被拒絕
gate = "auto"              # auto | always_solo | always_team

[team.roles.planner]
runtime = "claude"
model   = "claude-fable-5-1"
effort  = "high"

[team.roles.executor]
runtime = "codex"
model   = "gpt-5.5"
effort  = "medium"

[team.roles.verifier]
runtime = "antigravity"
model   = "gemini-3.7-flash"
effort  = "low"

[team.roles.utility]
runtime = "claude"
model   = "claude-haiku-4-5"
```

### Cascade

cascade 是**逐欄**的，分三層：

1. 員工 `agent.toml [team]` 的值（若有寫）。
2. 否則取 `config.toml [team]`。
3. 否則，對於既沒指名 runtime 也沒指名 model 的角色，取員工自己的 `[model] preferred`。

所以只覆寫 `[team.roles.executor] effort` 的員工，該角色仍沿用全域的 runtime 與 model。「未設定」與「明寫 false」是不同的狀態，這就是為什麼單一員工的 `enabled = false` 是真正的退出，而不是繼承來的 `true`。

### `gate`

`auto` 執行上述規則。`always_solo` 是終止開關。`always_team` **僅供測試**，它會繞過所有硬性排除，包括不可逆動作那一條，但繞不過沙箱規則。無法辨識的值會降級為 `auto` 並回報。

### `effort`

可用值：`low`、`medium`、`high`、`xhigh`、`max`（不分大小寫）。列舉本身、各 runtime 的上限，以及各廠商拼法不同的 CLI 旗標，全都集中在一處（`duduclaw-core::effort`）。`[team]` 只把這個鍵當作原始字串，交給那裡解析，因此 crate 不會對同一個旋鈕留有兩種拼法。無法解析的值會拒絕成團（`invalid_effort`）。

第一批五個 runtime 中有四個支援逐次呼叫指定 effort（2026-09-24 以真實 binary 探測）：Claude Code `--effort`、Codex `-c model_reasoning_effort=`、Antigravity `--effort`、Grok `--reasoning-effort`。Gemini CLI 沒有文件記載的對應項，跑在 `gemini` 上的角色仍可宣告 effort，由 spawn 層讓它成為空操作。在設定階段就拒絕，會讓同一份設定無法跨 runtime 移植，卻沒有換到任何安全性。

宣告在既不綁 runtime 也不綁 model 的角色上的 effort 是無效的（該角色會 cascade 到員工的模型，而員工的模型帶的是員工的 effort）。這種情況會被回報，而不是靜默丟棄。

---

### 從儀表板設定（v1.68.0）

- 全域預設：系統設定 → 進階設定 → 自動化引擎 →「一員工四角色（全域預設）」寫入 `config.toml [team] enabled`、`gate` 與 `[team.roles.<角色>] runtime`／`model`／`effort`。畫面上的「合成」角色存成 `utility`。下一個 goal 生效。
- 個別員工：員工編輯頁 →「腦袋與引擎」→「一員工四角色」寫入 `agent.toml [team] enabled` 與 `[team.roles.<角色>]`。
- 系統設定 → 進階設定 → 自動化引擎 →「派工策略」新增 `role_team`。

## 驗證器強制執行的三條規則

違反任何一條的規格都不會成團。任務改走 Solo，儀表板會說明原因，絕不會形成殘缺的團隊。

### 1. 角色是 `(role, runtime, model)` 三元組

Goose 的 Lead/Worker 功能儲存的角色設定只有模型名稱，結果 `qwen-*` 的 lead 最後由 Claude 後端執行（goose#10731）。在這裡，模型家族不屬於其宣告 runtime 的情況會被直接拒絕（`model_runtime_mismatch`），runtime 目錄不認得其家族的模型也會被拒絕。平台絕不會替無法歸屬的模型 id 猜測 provider。

只宣告 `model` 沒問題，目錄會把它綁到服務該家族的 runtime。只宣告 `runtime` 也沒問題，model 會 cascade 到員工的 `[model] preferred`。

**這個 cascade 有一個陷阱**：員工的 `preferred` 必須屬於所宣告 runtime 的家族。在 `preferred` 是 Claude 模型的員工身上宣告 `runtime = "codex"`，每一輪都會拒絕該成員（`validate_role_runtime_model`），產出零個封包，直到 `DISPATCH_FAILURE_LIMIT` 結束迴圈。它是 fail-closed，但每一輪都會失敗，所以只要角色的 runtime 與員工自己的不同，就請明確指定 model。

### 2. 只限第一批 runtime

`claude`、`codex`、`gemini`（v1.67.0 起棄用，v1.70.0 移除：請改用 `antigravity`）、`antigravity`、`grok`。其他一律拒絕，包括 `openai_compat`、`qwen`、`copilot`、`cursor`（`runtime_not_allowed`）。

原因在工具，與能力無關：這五個 runtime 原生註冊 DuDuClaw 的 MCP server，所以跑在上面的角色拿得到完整的工具面。角色若靜默失去工具，會產生自信滿滿、沒有任何工具呼叫的敘述，審核者分不出它與真正做完的工作。

### 3. 審核者不得與執行者同模型家族

同家族 ⇒ 拒絕成團（`verifier_same_family`），這是拒絕，不是警告。去相關就是整個機制（arXiv:2607.13918：相關的審核者讓失敗率只以多項式速度衰減，關鍵槓桿是獨立性，而不是疊更多判官）。

家族由 runtime 目錄推導，所以 `antigravity` 與 `gemini` 會合併為同一個家族：兩者都服務 `gemini-*` 模型，把它們配對在設定上看起來像兩家廠商，實際上買不到任何獨立性。

一項相關發現值得直接講：在 VP-CONTROL（arXiv:2609.10969）中，**共用證據**的跨模型投票仍放行了 62.9% 的不安全提案，而**獨立證據來源**把這個比例降到 22.9%。證據獨立性的貢獻是 40.9 個百分點，模型多樣性只有 11.3。因此審核者讀的是凍結的合約、封包與工具活動摘要，而不是執行者對自己工作的描述。

### 規則 2 與 3 每輪重新檢查，不限於凍結時

驗證只在規格凍結時跑一次。但凍結後的規格是以 JSON 存放在可變的任務列中，而記錄每個角色模型家族的欄位，直到這次修正之前**完全沒有讀取者**。規則出現之前的備份還原，或手動修補過的列，會產生不再滿足不變量的規格，下游卻沒有任何東西察覺。

現在每一輪在花掉一次 spawn 之前，都會重新檢查儲存的規格：每個角色的 runtime 仍須是那五個之一（規則 2，以前執行者路徑會過濾這一項，審核者路徑沒有），且審核者儲存的家族仍須與執行者不同（規則 3）。不通過的規格會以 `Failed` 結束，稽核列帶 `frozen_spec_family_violation` / `frozen_spec_runtime_not_allowed`，並附上給操作者的訊息，說明是哪個角色、該怎麼修。檢查讀的是**儲存的**家族字串，而不是重新解析目錄：重新解析回答的是「這份設定今天會代表什麼」，而這裡要問的是這個任務實際在跑的規格是否仍然成立。

---

## 角色交換封包，不交換逐字稿

唯一能跨過角色邊界的是 `TaskPacket`：目標、輸出格式、工具範圍、邊界、列舉的約束、受眾白名單、驗收斷言、對產物 / wiki / 記憶 / 狀態的參照、結構化的發現與下一步、觀察保真度，以及預算。

刻意沒有逐字稿、沒有廠商的 tool-call 區塊、沒有 thinking 或推理 payload，也沒有無型別的逃生口，而且這是由型別強制，不靠慣例。完整欄位表、上限與範例：**[spec/task-packet.md](../../spec/task-packet.md)**。

### 封包如何歸檔 (WP-5)

角色呼叫 `team_handoff` MCP 工具來交出封包。工具從 `(task, round, from_role, to_role)` 推導檔案路徑，從不接受外部給的路徑：

```
~/.duduclaw/team_packets/<task_id>/r<round>/planner-to-executor.json
~/.duduclaw/team_packets/<task_id>/r<round>/planner-to-executor.01.json
~/.duduclaw/team_packets/<task_id>/r<round>/planner-to-executor.02.json   … 最多到 .99
```

一條 leg 合理地會帶多個封包：規劃者把一個 goal fan-out 成四個子任務就會寫四個。所以在已被占用的 leg 上出現**新的** `packet_id` 時，會取下一個編號槽位；重新歸檔**相同的** `packet_id` 則覆寫它自己的檔案，因此逾時後的重試是冪等的，不會重複產生子任務。寫入是原子的（暫存檔、fsync、rename），並在規範化路徑上取跨行程鎖，所以 composer 絕不會讀到寫到一半的封包。這對**兩個**寫入者都成立：歸檔新封包的 `team_handoff`，以及在補上缺漏的 `sha256` 或修正誤宣告的 `fidelity` 時就地改寫封包的 composer。兩者在同一個規範化 leg 路徑上取鎖，所以彼此序列化，而不是各自對著空氣。

只有呼叫者自己的 `[team_member]` 區段能說明是誰在歸檔。沒有退回到 `[agent] role` 的後備：那個欄位是*組織*角色，自己也有一個 `planner` 變體，承認它會讓任何這樣設定的員工把偽造的 planner→executor 封包歸檔到別的任務目錄裡。沒有帶 `task_id`/`round` 釘選的身分會被直接拒絕，理由相同：封包的兩項交叉檢查都會被略過。

composer 以數字順序讀回相同的槽位，先是規範檔，再是 `.01`、`.02`……這也就是抵達順序。檔名從不被信任：封包只有在自己的 `from_role` / `to_role` / `goal_id` / `round` 都與正在讀取的 leg 相符，*且*通過驗證時才會被使用。任何一項不過的檔案，會附一列稽核後被跳過，而不是靠猜測；單一壞檔也不會讓整條 leg 的其他封包丟失；當一條 leg 上所有候選都壞掉時，會再多記一列，說明該階段的空白是損毀所致，不是沉默。

`team_handoff` 在自我回聲拒絕清單上，所以角色寫的封包永遠不能充當該角色自己的 grounding 證據。

### 兩個值得知道的接縫

- **約束與受眾永遠不壓縮。** composer 把這兩個欄位渲染在固定標題（`## 約束`、`## 受眾`）之下，而 prompt 壓縮管線拒絕裁剪這些標題。實測的失敗模式是預算會先對邊界課稅，把明確的約束靜默地變回隱含的約束。封包中其他一切仍可壓縮，受保護的區段到封包結束處為止。

  **豁免依據的是來源，不是文字。** 這四個標題只是普通的 markdown，而壓縮管線的 `history` 是十一個通道的對話歷史，*包括使用者自己打的訊息*。所以光有標題無法保護任何東西。composer 在每個標題正下方寫一行無法猜測的標記（`<!-- ddc-protected:… -->`，32 個 CSPRNG 位元組，每個 gateway 行程產生一次），只有緊跟著那一行完全相同標記的標題才會開啟受保護區段。使用者無法產生這個值：它從不出現在通道回覆中，而輔助摘要器拿到的逐字稿已先移除受保護區段。三個失敗方向是刻意的：沒有 sentinel 的讀取者（`duduclaw-llm` 的 CCR 預覽，除非其嵌入者傳入 sentinel，以及任何非 gateway 行程）什麼都不保護；gateway 重啟前產生的標記不再相符，所以舊摘要會衰退為一般可壓縮的歷史；而若 OS 的 CSPRNG 不可用，sentinel 為空，豁免就直接關閉。過度裁剪真正的約束代價高但可補救，把豁免交給不受信任的文字則否。

  兩個上限作為縱深防禦保留，防止*合法*的輸出者渲染出超過封包欄位上限的內容：預算下限在整個歷史中最多尊重約 6k token 的受保護內容，而 session 摘要器最多逐字釘住 4 KiB 的受保護文字，裁掉時會在摘要中說明。這個標記會在 composer 的 settle 摘要中再次被移除（渲染後的封包文字到達人類眼前的唯一路線），所以它不會出現在 `/goals` 或通道推播中。一個值得知道的後果：由 `DelegationEnvelope::to_prompt` 組出、由 agent 提供的 `## Constraints` 區塊不帶標記，也不受保護（它本來就不屬於 `history`，所以對它實際上沒有任何改變）。
- **角色成員永遠持有 `team_handoff`。** `[capabilities] allowed_tools` 早於團隊功能的員工，無法列名一個當時還不存在的工具，所以不論該白名單為何，交接通道都會對角色成員放行。這是團隊自己的機制，觸及範圍與 `working_state_*` 相同：`team_packets/` 底下推導出的一條路徑，沒有別的。角色要求的其他*每一個*工具仍會對照員工的包絡檢查，明確的 `denied_tools` 條目仍然勝出，而面向 agent 的 `spawn_ephemeral` 路徑完全不享有這種豁免。

---

## 角色實際在哪裡執行 (WP-2)

角色不是第二位員工。它是**角色成員**：位於 `~/.duduclaw/agents/.ephemeral/` 下的用完即丟 agent 目錄，為一組 `(task, round, role)` 建立，該輪結束就移除。員工仍是你唯一看到的東西：成員不在 registry、不在名冊、沒有 heartbeat、沒有演化，它的花費在成本報表中折回員工名下。

成員與 DuDuClaw 原有的暫時性子 agent 的差別，在於它帶著自己的大腦分配。它的 `agent.toml` 先從員工的複製而來，再覆寫四件事：

| 鍵 | 值 |
|---|---|
| `[runtime] provider` | 角色的 runtime，已規範化（`agy` 寫成 `antigravity`） |
| `[model] preferred` | 角色的 model id |
| `[model] effort` | 角色的 effort，**只有角色有宣告時才寫入** |
| `[team_member] role` / `task_id` / `round` / `parent` | 這個成員填的是哪個槽位 |

員工設定的其他一切（輔助模型、帳號池、容器隔離、預算）都會繼承。有兩個鍵刻意*不*繼承：未設定的 effort 不寫任何鍵，所以 spawn 不帶旗標，由廠商自己的預設深度生效；而繼承來的 `[runtime] fallback` 會被丟掉，因為在角色第一次失敗時靜默換到另一家廠商，會抹掉團隊存在所依賴的執行者／審核者獨立性。

除了 `agent.toml` 與 `SOUL.md`，成員還有自己的 `.mcp.json`，內含 duduclaw MCP server 加上成員的身分環境變數（`DUDUCLAW_AGENT_ID`、簽章 token，以及 MCP 子行程無法繼承的 home / port）。這個檔案是成員取得任何工具的唯一途徑（包括交接通道），因為 CLI 從工作目錄讀它，而 gateway 的啟動修補從不深入 `.ephemeral/`。員工的其他 MCP server（例如瀏覽器）刻意不複製：成員的觸及範圍維持在該輪要求的工具子集。成員也不蒸餾記憶：它在該輪 settle 時就被刪除，所以它學到的任何東西都會歸在一個已不存在的 id 之下。

### 每個角色拿到哪些工具

成員的 `[capabilities] allowed_tools` 是該角色的工具子集，由員工推導而來，而不是固定的字面值：

| 角色 | 工具 | codex 成員的有效沙箱 |
|---|---|---|
| `planner` | `team_handoff`、`wiki_search`、`memory_search` | `read-only` |
| `executor` | **員工自己的有效工具** + `team_handoff` + `memory_search` + `wiki_read` | `workspace-write` |
| `verifier` / `utility` | `team_handoff` | `read-only` |

「員工自己的有效工具」指的是：員工有白名單時，逐字取其 `[capabilities] allowed_tools`；否則取一般派工所用的同一組預設集合（`Read`、`Write`、`Edit`、`Bash`、`Glob`、`Grep`、`TodoWrite`、`WebFetch`、`WebSearch`、`mcp__duduclaw__*`）。無論哪種，執行者都是員工的子集：唯讀的員工仍會產生唯讀的執行者，員工 `denied_tools` 中的工具也絕不會被要求。

執行者為什麼不像其他角色那樣是一份固定的小清單：它是*定義上*負責做事的角色，而兩層強制機制都讀這份清單。Codex 把它對應到單一的粗粒度沙箱模式，清單中沒有任何寫入類工具就代表 `--sandbox read-only`，成員無法 `mkdir`，`apply_patch` 也被拒絕。Claude 則把它原樣傳給 `--allowedTools`，沒有 `Write` / `Edit` / `Bash` 的白名單在上一層是同樣的拒絕。實機第 5 輪同時撞上了這兩者。

規劃者與審核者刻意維持唯讀：會編輯檔案的規劃者是在做執行者的工作，而能改寫自己所審工作的審核者就不是獨立的審核者。

### 模型仍然不由模型自己選

角色路徑是唯一接受原始 model id 的地方，且**只有 gateway 的 composer 能觸及**。面向 agent 的 `spawn_ephemeral` 工具沒有改變：它仍只接受三個層級關鍵字之一（`cheap` / `standard` / `preferred`），仍拒絕原始 id。無論有沒有團隊，AI 員工都不能指名自己要跑在哪個模型上。

在建立任何東西之前，這組配對會對照 runtime 目錄檢查：runtime 必須是第一批五個之一，且模型的家族必須是該 runtime 實際服務的。目錄不認得的家族是錯誤，不是最佳猜測，`gpt-5.4` 不會送到 Claude binary 再於別人的 API 失敗。

### 成員在員工的工作區運作，而非自己的骨架目錄

骨架存放的是**設定**：`agent.toml`、`SOUL.md`、`.mcp.json`、`.claude/`。**工作**發生在員工自己的目錄（`~/.duduclaw/agents/<employee>/`），也就是該員工的 Solo 一輪會用的同一個工作目錄。

這會直接影響結果。在第一次走到執行階段的實機一輪中，成員以自己的骨架為工作目錄，把筆記寫在那裡，然後該輪結束的拆除把它們刪掉了。審核者去找檔案已被建立的證據，什麼都沒找到，並正確地駁回該輪：它被問到的產物指向的目錄已不存在。

移動工作目錄帶來第二個問題：Claude CLI 從 `<cwd>/.mcp.json` 探索它的 MCP server，所以待在員工目錄中的成員會以**員工的**身分啟動 duduclaw MCP server，每個工具呼叫、每次交接、每一列稽核都會被歸到父層。所以身分改為明確指名，而非探索：Claude 成員以 `--mcp-config <member_dir>/.mcp.json --strict-mcp-config` 啟動（第二個旗標用來阻止 CLI 同時合併環境中的設定）。Codex 成員拿到 `--cd <employee_dir>`，而它的 `-c mcp_servers.duduclaw.env.DUDUCLAW_AGENT_ID=…` 覆寫帶著成員 id，兩者在結構上互相獨立。若成員的 `.mcp.json` 不存在，該階段會被拒絕（`member_mcp_config_missing`），而不是帶著父層身分派工。

**成員目前尚不支援：gemini、antigravity、grok，以及一般的 print-mode CLI。** 這些 runtime 從工作目錄*裡面*的檔案讀取它們的 MCP 註冊（`.gemini/settings.json`、`.grok/config.toml`……），所以移動 cwd 會讓身分跟著移動。在這些 runtime 上的成員仍以自己的骨架為工作目錄，它的檔案同樣不會活過該輪。在這個缺口補上之前，需要寫檔的角色請用 claude 或 codex。

**另一個後果必須補上，而不是只寫進文件。** 共用員工的工作目錄，也代表共用其中的 `.claude/settings.json`，而註冊在那裡的檔案保護 hook 把*員工*當作呼叫者，所以 hook 把角色成員的寫檔讀成員工在寫自己的檔案，直接放行。成員因此可以編輯員工 `agent.toml [capabilities]`：翻轉 `os_native` / `computer_use`、放寬 `allowed_tools`、清空 `denied_tools`、替自己授予 `db_sources` 條目，而下一輪的子集檢查會承認被放寬的包絡。平台對「角色成員永遠不能持有員工沒有的工具」的承諾，正是從那個檔案讀出來的，而一個可能是第三方的模型可以改寫它。

`[capabilities]` 現在與組織欄位一樣對 hook 凍結：透過 Write / Edit / Bash 對該區段任何鍵的任何變更都會被拒絕。比較走的是兩側鍵的聯集，而不是手動維護的清單，所以後續版本新增的 capability 鍵在加入當天就被凍結。操作者不受影響：儀表板、MCP `agent_update` 工具與一般編輯器都是從 hook 之外寫檔。

一個值得知道的後果：cwd 被覆寫的 Codex 成員，其角色指示放在 prompt 內（以 XML 界定的 `<role_system_prompt>` 區塊），而不是透過 `AGENTS.md`。`codex exec` 在 0.156.1 上沒有 system-prompt 旗標，工作根目錄的 `AGENTS.md` 是唯一的檔案通道，而把成員的指示寫進員工的 `AGENTS.md` 會蓋掉員工自己的檔案，也會與同輩成員競爭。prompt 區塊的位置比真正的 system prompt 弱，這裡明說，而不是靜默略過。

### 產物路徑會對照工作區檢查

封包的 `artifacts[].path` 現在是對一個真實目錄中真實檔案的宣稱，所以會被檢查：解析後落在員工工作區之外的路徑會被**拒絕並留下稽核列**（`team_packet_artifact_refused`），而不是靜默接受。已存在的路徑會被規範化（因此放在工作區內、指向工作區外的 symlink 會被抓到）；在檔案存在之前宣告的路徑則退回詞法檢查，確認 `..` 不會爬出去。不是一般檔案，或大於 64 MiB 的路徑也拿不到收據：composer 在讀取前先 stat，所以 FIFO 無法卡住 worker，巨大的檔案也不會被拉進記憶體做雜湊。

拒絕現在**對審核者可見**，稽核軌跡之外也看得到。渲染後的封包帶有一個 `artifacts:` 區段，每個宣告一行，各帶稽核記錄的狀態：有收據的路徑為 `exists` / `missing` / `mismatch`，逃出工作區的為 `outside_workspace`，刻意不讀取的（不是一般檔案，或超過 64 MiB）為 `unverified`，只有 `artifacts.jsonl` id 而沒有路徑的宣告為 `id_only`。在 2026-09-28 之前，渲染器完全不輸出 `artifacts[]`，所以被拒絕的路徑會直接當作普通產物進入審核者的輸入，而包含檢查所承諾的「審核者看到的與稽核看到的相同」並不成立。給人看的 settle 摘要渲染同一個區段（狀態為 `unchecked`，因為摘要不做檔案系統工作）。

這些狀態來自產生稽核列的**那一次**驗證：每個宣告的路徑在檢查封包時只 stat、讀取、雜湊一次，渲染器拿到的是結果判定的查找表。它自己不做任何檔案系統存取。這一節的第一版在組 prompt 時重跑了整個檢查，把每個宣告的檔案再雜湊一次（每個最多 64 MiB，走阻塞 I/O），還把*第二份*觀察擺到審核者面前，卻沒有任何機制保證它與稽核的那份相符。composer 從未驗證過的產物（派工出錯前已歸檔的成員，或同一輪先前嘗試留下的槽位）會回報為 `unchecked`，而不是在 prompt 時重讀，或標成另一個封包的觀察結果。

仍有一個誠實的限制：宣告的路徑是一項*宣稱*，從不是豁免。指名 agent 機制檔案的產物（`SOUL.md`、`CLAUDE.md`、`state/`、`.claude/`、`logs/`、`memory/` 底下的任何東西……）會被排除在 settle 時的歸檔之外，所以永遠不會變成可下載的「產物」。

### 審核者與判官實際看到什麼

兩個證據區塊，都建自同一個 `tool_calls.jsonl` 時窗，都由團隊審核者**以及** settle 路徑的 evaluator 與 MAV 判官團讀取，所以角色的成品絕不會依據兩份不同的事件說法被評判。

**時窗是該輪自己的開始時間**，這一點做對與否，就是這個機制有效或空轉的差別。它以前是 `tasks.claimed_at`，而這個值對團隊任務在結構上就是 `None`（沒有任何東西會*認領*它，composer 以 `team-composer` 身分完成它），所以審核者每一輪都只拿到 `(無工具活動紀錄)` 且完全沒有收據，而 settle 路徑的時窗則靜默放寬到 `created_at`，讓第 1 輪的收據替第 3 輪背書。composer 現在在第一個成員執行之前就蓋上該輪的開始時間並傳遞下去；settle 路徑則讀取自己那一輪的 `task_iterations.dispatched_at`，退而求其次取該輪最早的 `role_turns.jsonl` 時間戳。兩者都沒有時，就**沒有**時窗：grounding 降級為略過，而不是往前回溯到先前的輪次。無法確立的時窗會渲染成明確的「時窗不明」一行，絕不會是代表「沒有用過任何工具」的同一個空區塊，因為無法分辨這兩者的審核者，會駁回誠實的工作，或依據一個它從未量測的「缺席」接受宣稱。

**`<tool_activity>`**：每個不同的工具一行，`name: N ok, M err`，範圍是員工 ∪ 該輪的成員。

**`<artifact_receipts>`**：封包宣告的每個產物一行，這是證據中確定性的那一半：

```
notes/a.md 128B sha256=3f2a… exists
notes/b.md missing
notes/c.md 44B sha256=aa11… mismatch (declared bb22…)
```

每個被包含的路徑在封包被讀取時就會 stat 並雜湊。沒有宣告雜湊的封包，會被寫回觀察到的值；宣告的雜湊與實際位元組**不一致**的封包，保留它宣告的值，記為 `mismatch` 並寫入 `team_packet_artifact_mismatch` 稽核列：調包必須保持可見，所以封包絕不會被悄悄改正。只有 `exists` 算作確認：`missing` 或 `mismatch` 的收據記為失敗的觀察，永遠不能作為宣稱的 grounding。

研究在這一點上毫不含糊。VP-CONTROL（arXiv:2609.10969）量到，共用單一證據來源的跨模型審核團放行了 62.9% 的不安全提案，證據來源獨立時則為 22.9%，其中 40.9 個百分點來自證據，11.3 個百分點來自模型多樣性。磁碟上位元組的 sha256 就是那個獨立的來源。

### 原生工具作業也算證據

一輪下游的一切（審核者的摘要、零 LLM 的 grounding 預檢、判官的稽核摘要）讀的都是 `tool_calls.jsonl`。用**原生**工具作業的成員（codex 的 `shell` 呼叫、Claude 的 `Write`）不會發出 MCP 呼叫，所以那裡什麼都沒寫，實機第 8 輪就是這樣走到一次 settle，對三個明明存在的檔案說「沒有任何工具活動可以證明這些檔案被建立」。

成員的每個原生工具事件現在都會以該成員的 id 寫成一列 `tool_calls.jsonl`，帶著工具名稱、結果、遮罩後的呼叫輸入與遮罩後的結果文字，加上 `source = "native"`、`evidence_source = "native_tool_event"`，以及產生它的 `runtime` / `model`。失敗的事件帶 `error_class = "native_tool_error"`：被擋下或失敗的呼叫必須留下痕跡，與 MCP 分派閘遵守的規則相同。

Antigravity `agy` 1.2.10 現在透過它的 `stream-json` 輸出提供終端工具事件。解析器忽略進行中的事件，並使用最後的 usage 區塊取得實測 token 數。實機探測中觀察到過被沙箱拒絕的指令；Antigravity 工具成功執行的情況仍需另行實機驗證。

與 MCP 列有兩處刻意的差異，都是收緊而非放寬：

- **輸入一律擷取**，即使是唯讀工具名稱也一樣。它只用來從算作 grounding 證據的內容中*扣除*自我回聲的區段。
- **自我回聲工具的結果文字會被抑制。** 像 `team_handoff` 這樣的工具，回覆的大部分是呼叫者自己的話，而 codex 成員把它的 MCP 呼叫回報為原生事件，所以保存該輸出會讓角色用自己的封包摘要為宣稱建立 grounding。呼叫本身仍會記錄，只有它的輸出被保留不存。

每個成員最多 200 個原生事件進入軌跡。超出的部分會被計入一列誠實的 `native_tool_events_truncated`，而不是靜默丟棄。

### 成員在該輪結束時清理，而不是數小時後

一般的暫時性子 agent 在完成後有一小時寬限，上限 24 小時。角色成員兩者都沒有：該輪一進入終態，骨架就消失。

這是算術，不是整潔。每輪每個角色一個成員，三個角色，最多五輪，最多三個任務同時進行，就是 45 個存活目錄，而預設上限是 32。依寬限窗口政策，溢出的會排隊然後過期，一輪就會悄悄少跑一個角色，而日誌裡沒有任何東西把它與垃圾回收連起來。在能被正確拆除之前該輪就死掉的成員，會在下一次維護掃描時被清掃，不必等舊的窗口。非團隊子 agent 的清理完全不變。

已寫入的成本列維持原狀：它們以成員的 id 存在 SQLite 中，並在報表時折回員工名下，所以刪除目錄不會丟失任何帳目。

同一個「終態」現在也會釋放該輪的**准入票券**。當暫時性上限已滿，成員的 spawn 會被持久地排隊，而不是直接失敗，每個等待中的輪次在往前走時會移除自己的票券。這只涵蓋了一般路徑：在等待者走到之前就結束的輪次（提早拒絕、失敗的階段、panic）會把票券留在共用佇列裡，占著 `queue_max_depth` 直到 TTL 過期。現在一輪在整個生命週期中持有一個清除守衛，所以每一種離開路徑（accepted、rejected、needs_human、cancelled、failed、panicked）都會清掉恰好該輪排隊的成員，不會動到同輩輪次的。

### 角色在一輪之中不換廠商

一般派工在設定的 runtime 無法連上時，可能故障轉移到另一個 runtime，並替換成後備實際服務的模型。角色成員不會：成員的跨家族故障轉移會被**拒絕**。若 codex 執行者無法 spawn，該階段失敗，`team_stage_failed` 記錄角色、runtime、model 與錯誤，帳本列寫 `outcome=failed`，然後由該輪既有的降級鏈決定接下來怎麼辦（執行者副本 → 不含修復 pass 的審核 → 停下來等人）。

原因是實機第 3 輪。每個 codex spawn 都在啟動時因為一個設定 bug 而死；故障轉移悄悄換成 Claude，工作由 Claude 完成，而帳本仍寫著 `runtime=codex … completed`。靜默地換掉廠商，會抹除團隊存在所依賴的執行者／審核者獨立性，而且不說一聲就這麼做，會讓之後每一次「哪個模型擅長什麼」的量測，量到的都是錯的對象。

### 必須區分的兩個限制

兩者都是界限，都不是新的逃生口。

- **速率。** 角色成員的 spawn 以自己的路徑種類（`role_team`）與自己的預算（`[dispatch_guard] role_team_max_in_window`，預設 60）計數，而不是與保護員工自身 spawn 的每分鐘 20 次預算共用。設定正確的三角色團隊，一個任務每分鐘就有 9 次 spawn，三個同時進行約 30 次，光算術就超出共用預算，那會讀成「平台壞了」，而不是「達到限制」。
- **容量。** 當存活骨架上限（`[dispatch] ephemeral_max_active`）已達到時，角色成員現在是被**持久地排隊**，而不是直接拒絕，使用與先前相同的佇列、深度與 TTL 設定。排隊的成員以要求它們的那一輪為範圍，所以結束的輪次會清掉自己待處理的成員，不會留下無人讀取的答案在等待。*無效*的請求（錯誤的 runtime/model 配對、員工沒有的工具）仍會立即被拒絕，絕不排隊：重試不可能成功。

因為這個上限與其他所有暫時性子 agent 共用，開啟團隊之前它至少要是 `max_concurrent × iteration_cap × roles`：以今天的預設是 45，而預設值是 32。團隊路徑會檢查這個算術並明說，而不是讓它之後以審核者缺席的形式浮現。兩個鍵請見 [`config/duduclaw.example.toml`](../../../config/duduclaw.example.toml)。

---

## 一輪，三個階段 (WP-4)

有了凍結的規格與 Team 判定，一輪 goal 由單一的喚醒訊息，變成同一位員工內部的三個階段。

### 規格依任務凍結

goal 任務建立時，合併後的 `config.toml [team]` + `agent.toml [team]` 會驗證一次，結果存在任務本身。從此再也不變：明天重新排序模型的角色→模型矩陣或 bandit，影響的是*下一個*任務，不是已在跑的任務。紀律與凍結的驗收標準相同：光看任務列，你永遠答得出「這個任務實際是由哪個團隊工作與評判的」。

驗證失敗的規格**完全不成團**。拒絕會被記錄，不存任何東西，任務以單一員工執行。不存在殘缺的團隊。

凍結在資料庫層是只能設定一次的（`WHERE team_spec_json IS NULL`），所以兩條建立路徑在同一任務上競爭是正常且安全的。以前不安全的是*輸家*對此的視角：goal loop 只處理「我凍結了」這個答案，所以輸掉競爭的那一輪持續讀自己過期的任務副本，找不到規格，悄悄跑成 Solo，而之後每一輪讀的是新的列，跑成 Team。同一個任務，兩種執行形態，任何地方都沒有痕跡。輸家現在會重讀該列，採用實際勝出的規格。

### 規劃 → 執行 → 審核

1. **規劃**：一個角色成員收到目標、凍結的驗收標準與風險邊界，針對每個可獨立完成的子任務交回一個封包，子任務之間的相依性明確列出。它自己不做工作。什麼都沒交回的規劃者會讓任務停下來等人，而不是從它的文字裡猜出一種拆解。
2. **執行**：每個子任務封包一個成員，最多到設定的 fan-out 數，**彼此之間不通訊**。每個成員交回自己的封包：做了什麼、證據在哪、還有什麼未結。沒有證據參照的宣稱屬於「未解問題」，不屬於發現。
3. **審核**：審核者跑在自己廠商的模型上，看到三樣東西：凍結的驗收標準、執行者封包，以及實際發生過的工具呼叫稽核軌跡。它**看不到**規劃者的敘述，也看不到執行者的文字說明。若它判定工作失敗，同一個執行者會得到一次範圍限於所指缺口的修復 pass，不是重寫，也不是更寬的任務簡報。

執行者最後產出的東西，接著交給**既有的**驗收路徑：兩段式 evaluator、三面向判官團、gap 指紋、振盪偵測、最佳輪挑選。團隊不會新增第二套裁決系統，它改變的是誰做工作，不是誰決定工作做完了。

### 灰帶

當閘恰好落在兩個訊號時，它有一個誠實的答案，代價是任務本來就需要的一次呼叫：跑一次 規劃，數它實際拆出多少。有四個以上真正獨立的子任務就成團；更少，該輪退回一般的單一員工派工，計畫留在磁碟上供之後的輪次讀取。

### 預算吃緊時

一輪要花 spawn，而任務對 spawn 有上限。一輪的計費是 `planner? + executors + verifier + repair?`：審核者是輔助呼叫而不是骨架，但它會寫自己的 `role_turns.jsonl` 列，而預算計數器計的正是帶有成員 id 的那些列。規劃以前漏掉了這一項，所以每一輪都悄悄比計畫多花一個，`max_spawns_per_task = 12` 的任務規劃了第四輪，就會在途中以「budget exhausted」死掉。規劃與計費現在使用相同的算術，夾取下限是 3（規劃者 + 執行者 + 審核者），不是 2。

隨著剩餘預算縮減，能力會依固定順序放棄，而不是讓該輪直接失敗：

1. **合成**（輔助）不再使用，這是損失最便宜的一項。
2. 審核者的**修復 pass** 取消：失敗的一輪直接進入驗收路徑，不再有第二次機會。
3. **Fan-out 收合為一個執行者。**
4. 沒有東西可放棄 ⇒ 任務以「budget exhausted」停下來等人，並附上它做到的最佳輪。

走到了哪一步會被記錄，所以便宜的一輪讀起來就是便宜的一輪，而不是較差的團隊。

### 設定

```toml
# config.toml，全域預設
[dispatch.team_budget]
max_spawns_per_task = 12                 # 4 個角色 x 3 輪；夾取為 >= 3
max_turns_per_role  = 3
degrade_order = ["utility", "verifier_second_pass", "executor_replica"]
```

無法辨識的 `degrade_order` 條目會被丟棄並警告；清單中沒有任何可用項目時，保留預設鏈（把每次超支直接變成人工升級，會是比打錯字所要求的更嚴厲的改變）。

### 逐角色歸因：`role_turns.jsonl`

每個階段都會在 `<home>/role_turns.jsonl` 附加一列：同樣的 advisory 鎖附加、同樣的 0600 權限、同樣的雜湊鏈與限制大小的輪替，與 `tool_calls.jsonl` 相同。讀取者會讀最近一份輪替世代（`.jsonl.old`）**加上**現行檔：spawn 預算是依這些列計算的，只讀現行檔會讓進行中任務途中的一次輪替，悄悄把它已花的預算重設為零。（只有往回一個世代，每次輪替都會覆寫前一個 `.old`。兩個檔案的雜湊鏈在邊界處重新開始，所以這是為了讀取與計數的串接，絕不是鏈條連續的宣稱。）一列帶有 `task_id`、`round`、`role`、`member_id`、**要求的** `runtime` 與 `request_model`、**實際回答的** `runtime_used` / `response_model` / `provider` 加上 `failover` 旗標、`effort`、它產出的封包、其觀察的**證據等級**（`full` / `mcp_only` / `none`，絕不混為一談）、它如何結束，以及一個在角色的 runtime 或 model 改變時會跟著改變的 `config_fingerprint_hard`。

要求與回答分開，是因為這一列以前只能重複設定。在實機第 3 輪中，codex 執行者悄悄故障轉移到 Claude，而這一列寫的是 `runtime=codex … completed`；`runtime_used` 與 `response_model` 由執行路徑自己回報，而只要回答的 runtime 與要求的不同，`failover` 就為 true。從未 spawn 的階段所寫的列，這三者都留空，而不是猜測。

證據等級由 composer 填寫，絕不由成員填寫：runtime 自己的工具事件串流看到呼叫時為 `full`，成員在其派工時窗內有稽核列時為 `mcp_only`，什麼都沒觀察到時為 `none`。成員自己的宣稱會被覆寫，不一致會被稽核（`team_packet_fidelity_corrected`），在此之前，每個封包回來都是 `none`，因為根本沒有東西填這個欄位。有 fan-out 時，數個執行者共用一條 leg，所以 composer 在派工前會逐位元組為該 leg 拍下快照，只對該成員建立或改寫的檔案評級，絕不依別人的觀察重新評定同輩的封包。

這是平台以前沒有的維度：其他每個歸因面都以 agent id 為鍵，而一旦一位員工的任務由三個模型處理，「哪個 agent」就不再能辨識是誰做了什麼。`fault_side` **有**計算並寫入（在寫列時確定性地由封閉集合的 `FaultSide` 列舉得出，證據無法確立一方時為 `unknown`）。設計中列出、但目前沒有任何東西能計算的欄位（trace/span id 與角色 Shapley 值）則刻意**不存在**，而不是寫成空值。

runtime 沒有回報用量時，用量欄位會整個省略。寫 `0` 等於宣稱「已量測，而且是免費的」。

用量取整個**階段**的總和，不取最後一個 leg。一個階段可以經由數個 leg 回答（openai-compat 的 tool loop，或主要嘗試在後備回答前已耗掉 token 的故障轉移鏈），而在 2026-09-28 之前，這一列只發布最後一個，`cost_telemetry` 卻記錄每一次呼叫，所以兩份帳本對同一階段的說法不一致。現在每個 leg 的數字都會加總（飽和加法），只有一個 leg 量測到的維度保留該 leg 的值，不會被一個從未回報的 leg 歸零，而 `usage_legs` 說明有多少個 leg 貢獻。`usage_legs > 1` 表示這個階段不只一次呼叫；沒有這個欄位表示沒有任何 leg 回報用量（這也是該欄位出現之前寫的每一列的形狀）。

### 操作者看到什麼

對話中沒有任何新東西：員工仍以一個聲音回答，進度仍在同一個看板上到達，`needs_human` 仍帶著六種暫停類別之一。團隊的一輪顯示為一般的 goal-loop 進度；角色細節在稽核日誌與 `role_turns.jsonl` 中，直到儀表板團隊卡片出貨。

---

## 實機輪次帶來的改變

在第二個使用暫時 home 的 gateway 實例上，針對一個真實 goal 跑了八輪（規劃者 Claude Sonnet 4.6、執行者在 Codex CLI 0.156.1 上的 Codex gpt-5.6-sol、審核者 Claude Sonnet 4.6）。每一輪都比上一輪走得更遠，每一輪都物有所值：

| 輪次 | 走到哪裡 | 發現了什麼 |
|---|---|---|
| 1 | 規劃者有跑，零個封包 | 成員沒有 `.mcp.json`，所以 `team_handoff` 對它們不存在。 |
| 2 | 規劃者呼叫了 `team_handoff` 13 次 | 從工具描述猜不出封包 schema；身分讀取者找錯了暫時成員的目錄。 |
| 3 | 完整的 規劃 → 執行 → 審核，判官駁回 | 四個缺陷，見下。 |
| 4 | codex 成員仍然無法啟動 | `codex exec` 拒絕不是受信任 git repo 的工作目錄，除非加 `--skip-git-repo-check`，而且會在開著的 stdin 上等待。 |
| 5 | codex 成員啟動、看到工具、什麼都做不了 | 兩項拒絕，見下。 |
| 6–7 | 直接探測六種 codex 核准變體 | 只有 `--approve-for-me` 讓 MCP 呼叫通過，而它與 `--sandbox <MODE>` 互斥。 |
| 8 | 完整管線跑在真實後端上，settle 駁回 | 兩個缺陷，見下。 |

第 3 輪的駁回是**正確的**：審核者與 settle 路徑確實看不到支撐該工作的任何工具活動。有四件事是錯的：

1. **每個 codex spawn 都在啟動前就死了。** `codex exec -c key=value` 會把值的那一半當 TOML 解析；成員 env 區塊中的 port 沒有加引號輸出，codex 把 `18999` 讀成整數，而它的 `mcp_servers.<id>.env` 表要求字串，spawn 以 1 結束。這是既有的 codex-runtime bug，一般的 codex agent 與 codex 驗收判官同樣被它弄壞。現在每個 `-c` 純量都是帶引號的 TOML 字串。
2. **成員靜默換了廠商。** 見[角色在一輪之中不換廠商](#角色在一輪之中不換廠商)。
3. **成員在一個即將被刪除的目錄裡工作。** 見[成員在員工的工作區運作](#成員在員工的工作區運作而非自己的骨架目錄)。
4. **沒有人填封包的證據等級**，所以每個封包都讀成 `none`。見[逐角色歸因](#逐角色歸因role_turnsjsonl)。

第 5 輪讓成員跑起來，工具清單中有 duduclaw 工具（`mcp__duduclaw__team_handoff` 與 `working_state_handoff` 都在），然後它一個也用不了。兩項拒絕，各來自一層強制機制：

1. **每個 MCP 呼叫都回 `MCP tool call requires approval, but approval policy is never`。** 這是 codex 在 fail-closed，不是壞掉：非互動的 `codex exec` 沒有人能回答核准提示，所以需要核准的工具只能被拒絕。逐 server 的逃生口是 `mcp_servers.duduclaw.default_tools_approval_mode = "auto"`，現在與註冊的其餘部分一起走同一個 `-c` 通道傳入。它的範圍只限 DuDuClaw 自己的 server：codex 的 shell 與 patch 工具仍依 `--sandbox` 允許的範圍，操作者註冊的任何其他 MCP server 也保有自己的預設。duduclaw 工具的真正授權在 MCP server 裡（逐工具 scope、agent 的 capability 閘、稽核日誌），不在沒人看著的核准提示裡。
2. **工作區是唯讀的**（`mkdir` 與 `apply_patch` 都被拒絕），因為執行者的工具清單沒有任何寫入類工具，而 codex 從這份清單推導它單一的沙箱模式。見[每個角色拿到哪些工具](#每個角色拿到哪些工具)。

第 8 輪終於在真實後端上跑完整條管線：規劃者在 Claude 上，執行者在**真實的 codex gpt-5.6-sol** 上（`runtime_used = codex`、保真度 `full`）在員工工作區建立 `notes/a.md`、`b.md` 與 `index.md`，封包送給審核者，審核者在 Claude 上，然後 settle。它仍被駁回，而且同樣是當時屬實的理由：

1. **證據從未被寫下來。** 執行者的工作是用原生 shell 與檔案工具完成的。這些事件有被計數（`fidelity: full` 就是從這裡來的），但只存在一個隨成員派工結束而消失的任務區域清單中，而下游每個讀取者看的都是 `tool_calls.jsonl`。所以最需要被相信的那一輪，在結構上恰恰是最難被相信的。以兩種方式修正：[原生工具作業也算證據](#原生工具作業也算證據)，以及[產物收據](#審核者與判官實際看到什麼)會對照檔案系統檢查宣告的路徑，而不是採信封包的說法。
2. **codex 判官無法產生可解析的判定。** 在 `[dispatch] judge_provider = "codex"` 下，兩個裁決階段都以散文回答，兩個解析器都 fail-closed 地拒絕（「evaluator reply has no string `decision` field」、「面板回覆無法解析」）。兩者都沒錯：自動接受垃圾是判官解析器絕不能做的一件事，但結構上無法被解析的判官，是一個行不通的接縫。`codex exec --output-schema <FILE>` 從源頭解決：每個階段現在發布自己解析器所要求的 JSON schema（evaluator 的 `decision` 列舉、判官團的面向物件），由解析器的契約推導，所以兩者不會漂移。沒有結構化輸出旗標的 runtime 會記錄這個要求並忽略它：schema 是偏好，不是前提，裁決不能因為後端無法約束它的輸出而失敗。

還有一件事是結構性的，不是 bug：審核者與驗收路徑讀取的工具證據，都是**限定在單一 agent id** 的。團隊的工作由各自 id 下的暫時成員完成，所以員工的稽核時窗是空的，「沒有工具活動」是對它們所見內容的誠實解讀。團隊一輪的證據集合現在是員工 ∪ 該輪的成員，兩條路徑皆然；非團隊任務不會加入任何 id，看到的證據逐位元組相同。

第 9–13 輪暴露了 settle 期間的路徑解讀與第二實例的過期狀態。修正記錄在設計文件的內部實機測試日誌，這份日誌沒有公開。第 14 輪在一次團隊輪次中通過了既有的驗收判官；它仍只是單一的整合結果，不是比較式的品質量測。

## 哪個角色用哪個模型 (P2)

角色可以各自跑在不同廠商的模型上，那麼*哪個*模型配*哪個*角色？P2 以量測而非排行榜來回答：`duduclaw eval --matrix`。完整的操作者參考：[guides/evals.md → Capability matrix](../../guides/evals.md#capability-matrix---matrix)。

### 量測什麼

每個 `(domain, role, runtime, model)` 一格，其中 domain 是一個 eval-suite 目錄：

- **執行 (executor)**：在該 `(runtime, model)` 上即時跑 suite 的案例，並對確定性的 `[expect]` 斷言評分。不涉及 LLM 判官。
- **審核 (verifier)**：把案例已錄製的逐字稿加上驗收標準給模型看，要求回答 `PASS`/`FAIL`，並把這個判定與同一份逐字稿上的斷言結果對照評分。該格回報一致率，加上帶 Wilson 區間的**誤接受**與**誤駁回**率，並把第一行既沒有 PASS 也沒有 FAIL 的回覆計為 *unparseable*，排除在所有比率之外，因為「無法以要求的形式回答」與「判斷得很差」是不同的發現。
- **規劃 (planner)**：`--team-2x2` 對每個案例，在 eval home 的隔離副本上跑四個配對的正式 composer 輪次。案例必須宣告 `[case] team_acceptance`；固定的獨立審核者的 PASS 為 1，FAIL 為 0。無法取得的判定不計分，只有四臂完整的列才會貢獻於 Shapley 結果與規劃者格。一般的 `--roles planner` 仍被拒絕，因為單一規劃者的回覆不是有評分的團隊結果。

審核格合理地重播一份已錄製的逐字稿：被測的對象是被要求評判它的模型，worker 不在其中，而那個模型永遠是即時呼叫。透過凍結的逐字稿比較*執行者*會是 Replay Gap，這就是 `--matrix --replay` 被直接拒絕的原因。

### 該把錢花在哪個角色

給定 `--weak` 與 `--strong`，每個角色在相同案例上得到 Δ = score(strong) − score(weak)（逐案例配對、cluster-robust SE）。較大的 Δ 是模型花費買到最多的角色，但只有當它的信賴區間排除所有其他角色的區間時，才會被**命名**為瓶頸；否則答案是 `unresolved`，這是一個真實的答案。

這是 AgentCARD 的 Shapley 探測（arXiv:2606.20629）的**解耦**形式：每個角色獨立量測，沒有聯合的團隊執行。這使它負擔得起（2 個角色 × 2 個模型，而不是 |models|^|roles| 種團隊設定），也同樣是它看不到的東西。真正的交互作用（只有在弱執行者背後才有回報的強審核者）在結構上看不到。請把它讀成「先該把錢花在哪個角色」，絕不是團隊層級的歸因。

### 它寫出什麼，以及什麼仍然沒有讀取者

`--report` 會寫出 JSON 報告，並在旁邊寫出 **`role_model_matrix.toml`**：一個 `[header]`（宣告的 MDE、α、power、K、cluster key、`planner = "deferred"`），以及每個已量測的格一個 `[[cell]]`，帶 `n`、平均值、區間、達成的 MDE 與三態的 `verdict`。無法計算的統計量是缺席的鍵，絕不是捏造的數字；沒有任何可用觀察的格會有報告列，但沒有矩陣格；檔案在寫入與讀取時都會驗證，所以手動編輯出來的重複格或未知 runtime id 會被拒絕，而不是被相信。

**composer 把它當作先驗來讀。** 把 `role_model_matrix.toml` 放進 `<DUDUCLAW_HOME>`，沒設定 `model` 的角色會採用矩陣的勝者，而不是直接落到員工的 `[model] preferred`。五個條件，每一個都只會收窄：

- **明確設定的** `[team.roles.*] model` 永遠勝出：設定是決定，矩陣是量測；
- **`unresolved` 的格完全忽略。** 區間無法解析宣告 MDE 的格不是排名輸入，高分的 unresolved 格也絕不會勝過 resolved 的格；
- **只採計角色自己 runtime 上的格**，所以先驗永遠無法改寫規格凍結時的 `(role, runtime, model)` 三元組；
- 勝者必須維持角色的**模型家族**，否則執行者 ≠ 審核者的不變量會在底下悄悄被打破；
- **平手不算勝者**，而在這台主機上沒有 CLI 或憑證的 runtime 會被跳過。

任何不明確之處都退回矩陣之前的 cascade，所以沒有矩陣檔的部署行為逐位元組相同。composer 沒有自己的 domain（goal 任務不是 eval-suite 目錄），所以相同 `(role, runtime, model)` 在各 domain 的格，以 `n` 加權平均聚合。

會讓這個檔案保持新鮮的線上 bandit 仍是 P5，而閘的 `capability_gap` 訊號仍沒有資料來源（見下）。

### 第一次煙霧測試發現了什麼

第一次探測執行（2026-09-25，`hr-recruit`，4 個案例，在 haiku 與 codex 上跑執行 + 審核）產生三項發現，現在都已處理：

| 發現 | 處理 |
|---|---|
| 執行格 12/12 出錯：探測 home 只佈建了一個 agent，沒有 suite 的 `hr-recruit` | `--agent <id>` 讓每個案例都在同一個已佈建的 agent 下執行。矩陣量的是模型，不是人設，所以這是可接受的探測折衷，並在報告標頭與每次執行中宣告為 `agent_override`，絕不推斷 |
| Codex 審核格 4/4 `unparseable`：codex 不會穩定地以裸的 `PASS`/`FAIL` token 開頭回覆 | 每次審核呼叫現在都透過既有的 `--output-schema` 管線要求 `{"verdict":"PASS"\|"FAIL","reasons":[...]}`（與以往一樣只對 codex），而解析器接受該 JSON 物件**或**散文首 token 形式，仍以 fail-closed 落到 `unparseable` |
| 每個案例的 gold 都是 FAIL（錄製的逐字稿已過期），所以總是說 FAIL 的審核者拿到一致率 1.00 | 每個審核格回報 `gold_pass` / `gold_fail`，而單一類別的 gold 會設 `degenerate_gold: true`，強制 `verdict: "unresolved"`（原因 `degenerate_gold`），並讓該角色退出瓶頸比較 |

### 煙霧測試 3 發現了什麼

第三次探測執行（真實的 `hr-recruit` 人設、4 個重新錄製且 gold 為 2P/2F 混合的案例、在 haiku / codex / sonnet 上跑執行 + 審核）產生了**真實的分數**，以及兩個會讓這些分數說謊的 bug：

| 發現 | 處理 |
|---|---|
| 每一格都只有一個 cluster（一個 suite 目錄），在這種情況下 cluster-robust SE *恆為零*。CI 縮成一個點（`mean 0.25 ci=[0.25,0.25]`），兩個 Δ 區間寬度皆為零，瓶頸在四個案例上被宣告為 **resolved** | 少於兩個 cluster 時，現在回報未分群的 CLT SE 並標明（`se_source: clt_single_cluster`），格與配對 Δ 皆然。同一個缺陷也已在一般 eval 路徑的 suite 層級列上掃過。真正寬度為零的區間（n=1，或所有觀察完全相同）強制為 `unresolved`，原因 `degenerate_interval`，而瓶頸拒絕在這種區間上 resolve |
| Codex 審核者的回覆是 `unparseable`，因為被解析的文字是 `{"type":"turn.completed","usage":{…}}`，也就是串流的最後一個事件，不是 agent 的訊息 | 現在從 agent 的**訊息**讀取判定，在 runtime 交回原始事件串流時從中復原（codex 的 `agent_message`/`message` 項目、claude 的 `assistant`/`result` 事件）。fail-open：真正的答案原樣通過。觸發復原的執行會標記 `message_recovered_from_stream` |

第二項的根因在**上游，仍未解決**：`runtime/codex.rs::parse_codex_stdout` 只認得 `item.type == "message"` + `content[].type == "output_text"` 的形狀，所以輸出 `agent_message` 形狀的 CLI 會得到空內容，而 `CodexRuntime::execute` 退回 `stdout.lines().last()`。該 runtime 的每個呼叫者都繼承了這一點；eval 路徑現在免疫，但 gateway 的修正屬於可以編輯 `duduclaw-gateway` 的那一波。

**這留下的 P2 負債。** 出貨的 premium suite 所錄製的逐字稿對它們目前的斷言已過期（P0 實機測試在重播上量到 98/360，對前一版 binary 沒有改變），所以對它們的**每一個**審核格都是 `degenerate_gold`。在審核矩陣有任何意義之前，必須重新錄製，或修正它們的斷言。執行格不受影響：它們即時執行，從不讀錄製的逐字稿。

### 小矩陣的誠實限制

矩陣會宣告自己的解析度。在煙霧測試的規模（6 個案例、`K=1`），達成的 MDE 是數十個百分點，所以幾乎每一格都回傳 `unresolved`，足以分辨 Haiku 等級與 Opus 等級，離排出兩個相鄰模型的名次還差得遠。宣告的 MDE 會印在主控台摘要，並存在檔案標頭，正是為了讓之後沒有人引用樣本數撐不起的排名（research 13 §4.2：40 個案例、K=3 約解析 21pp；200 個案例解析約 9pp）。

---

## 仍然缺少的

誠實的清單，讓這裡沒有任何東西讀起來比實際更完整：

- **被接受的一輪只是單一煙霧結果。** 第 14 輪通過了既有的驗收判官，但這不能證明它在正式環境勝過 Solo。角色×模型矩陣的四案例煙霧測試無法解析宣告的 MDE。
- **會寫檔的角色只限 claude 與 codex。** gemini / antigravity / grok / 一般 CLI 的成員以自己的骨架為工作目錄，所以它們的檔案活不過該輪。
- 角色×模型能力矩陣現在由 composer 當作先驗讀取（見上），但**只在角色沒設定模型時**，且只來自沒有 `unresolved` 的格。自 2026-09-29 起，同一個檔案也回答閘的能力差距訊號：矩陣中該執行者 `(role, runtime)` 的勝出模型，與該角色今天實際運行的模型之間，以 `n` 加權的距離（百分點），並與矩陣標頭自己宣告的 MDE 配對，所以低於雜訊底線的差距不會觸發。這項比較的兩邊都需要 resolved 的格，所以**在每一份出貨的矩陣上，這個訊號仍然是暗的**：四個格全是 `unresolved`，使閘維持原狀（四個訊號中兩個可量測，而成團需要三個）。它會在操作者第一次量出能 resolve 的矩陣時啟用，在那之前不會。會讓檔案保持新鮮的線上 bandit 仍是 P5。P2b 全團隊產生器完成了一次三案例、12 臂的實機管線探測，有真實的審核判定與交接。四個模型格全是 `unresolved`；來自單一 domain 的三個合成案例不滿足正式矩陣的驗收門檻，這在實務上代表今天出貨的矩陣完全選不出任何東西。
- **預設開啟的翻轉尚未以預設值實機測試。** 它在未設定安裝上產生的行為由單元測試涵蓋（安靜的 Solo、無稽核列、派工逐位元組相同）；尚未執行的是這樣一個機群：已設定 `[team.roles]` *並且*依賴 `enabled` 關閉來維持 Solo。這樣的部署現在會成團，`enabled = false` 是一行的還原。
- **審核格只有在重新錄製的 suite 上才量得到東西。** 出貨的 premium suite 的逐字稿給出單一類別的 gold，所以其上的格誠實地是 `degenerate_gold`；煙霧測試 3 顯示，用混合 gold 重新錄製四個案例能產生真實分數，所以修法已知，只是還沒套用到 18 個出貨的 suite。
- **單一目錄的 suite 完全無法使用 cluster-robust SE**，所以它的區間來自較弱的 CLT 估計量，看不到目錄內的相關性。真正的解析度需要設計中的 ≥8 個目錄（research 13 §4.3），不是更多案例。
- gateway 會擷取 codex 的 `agent_message` 項目，而團隊審核者現在向 codex 要求與離線審核格相同的嚴格 PASS/FAIL JSON schema。以 codex 為審核者的團隊實機一輪仍待檢查。
- **目前模型的價格仍需操作者填寫。** 矩陣在派工前載入 `<DUDUCLAW_HOME>/models.toml`，任何模型沒有價格時就拒絕 `--budget-usd`。它的估算仍無法精確封頂 provider 帳單；大型工具執行的花費可能超過執行前的預留。
- 閘自己的校準（依結果對其決策做 Brier 評分）已設計但未接上。
- Fan-out 是把*不同的子任務*分給同一個執行者模型上的成員。每個副本的模型多樣性在設計中，但無法用出貨的 `[team.roles.executor]` 形狀表達。
- **封包的 `tool_scope` 與 `irreversible` 被攜帶，但沒有任何東西讀取。** 兩者都被驗證並儲存，都沒有到達任何 spawn，而 `render_packet_for_prompt` 甚至不渲染它們，所以規劃者寫 `tool_scope.denied = ["mail_send"]` 完全沒有效果。角色成員真正的工具包絡來自員工的 `[capabilities]`，透過 `check_tool_subset`，而 ActionGuard 無論旗標為何，都在呼叫點守衛不可逆的呼叫。型別文件與 [`spec/task-packet.md`](../../spec/task-packet.md) 以前宣稱的是另一回事（「這裡的拒絕是真正的拒絕」）；現在改寫為「not wired」。接上它們是未來的工作。
- 閘不會評估 L0「計畫中含不可逆動作」的排除；ActionGuard 仍在呼叫點守衛不可逆的呼叫，團隊不改變這一點。
- ~~**永不裁剪的豁免是以四個字面 markdown 標題為鍵，而不是以 composer 為鍵。**~~ 已關閉：豁免以每行程的 sentinel 標記行綁定到 composer，兩個上限則保留作縱深防禦。見「兩個值得知道的接縫」。
- **`role_turns.jsonl` 把審核者的一列算進 `plan_round` 沒有規劃到的 spawn 預算。** 審核者是輔助呼叫，但它的帳本列帶 `member_id = "team-verifier"`，而 `spawns_used_for_task` 計入每一列有成員 id 的列，所以每一輪都比計畫預估的多花一個 spawn。
- `cost.by_role` 現在回報依角色與模型的實測團隊階段花費，可選擇以任務 id（`episode_id`）收窄。沒有角色的舊列仍未歸屬。v1.68.0 起團隊設定已能在儀表板修改（見「設定角色」），封包／工具下鑽仍待接上。
- 審核者的 token 用量在其 runtime 提供 usage 區塊時擷取；否則它的 `usage_*` 欄位缺席，而不是零。同一規則適用於規劃者與執行者的用量。實機探測的角色成本帳本不完整，不能當作 provider 帳單。
- **結構化判官輸出只限 codex。** `--output-schema` 是唯一接上的結構化輸出旗標；gemini 或 openai-compat 判官仍得靠 prompt 說服。團隊審核者現在共用離線審核者的 schema，接受嚴格 JSON 與首 token 散文；以 codex 做團隊審核仍需要一次實機一輪。
- **產物收據只涵蓋封包宣告的路徑。** 成員寫了卻沒列在 `artifacts[]` 的檔案，有它的原生工具列作為證據，但沒有雜湊。掃描工作區找出未宣告的變更是未來的工作。
- 脫離的 composer 現在在存活骨架上限達到時，會等待一張 FIFO 准入票券。過期、遺失或無法讀取的票券會讓該階段明確失敗；被取消的等待會移除自己的票券。佇列狀態與骨架釋放的測試通過；仍需要一次有負載的實機一輪測試。

---

## 參考

- 設計：`commercial/docs/DESIGN-team-as-agent-2026-09.md`（§1、§3.1–§3.6、§3.8–§3.11）
- 封包參考：[spec/task-packet.md](../../spec/task-packet.md)
- 相關：[13-multi-runtime.md](13-multi-runtime.md)、[goal-loop 指南](../../guides/zh-TW/goal-loop.md)
