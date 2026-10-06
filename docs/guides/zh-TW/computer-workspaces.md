# 電腦操作工作區：讓 AI 員工的檔案在電腦操作結束後留下來

電腦操作（`computer_*` 工具）的容器用完就刪，瀏覽器裡看到的東西、員工整理出來的筆記也跟著消失。電腦操作工作區是一個由 gateway 保管的資料夾：員工在一次電腦操作中把整理好的文字寫進去，下一次（就算 gateway 重開過）再掛回同一個工作區接著做。

一個工作區屬於一位 AI 員工。只有 gateway 會寫入；容器裡看得到它，但只能讀。預設關閉，要同時打開全域開關和該員工的開關。

本功能只支援 macOS 與 Linux。Windows 上會明確拒絕（不會默默退回別的做法）。

## 前置條件

- 電腦操作本身已經能用：Docker、電腦操作的 image、員工的 `computer_use = true`。見[瀏覽器自動化與電腦操作](../../features/zh-TW/08-browser-automation.md)。
- gateway 在執行中。三個工作區工具都是轉給 gateway 處理的。
- 磁碟剩餘空間高於 `min_free_bytes`（預設 512 MiB），否則寫入會被拒絕。

## 開啟

全域開關在 `config.toml`：

```toml
[computer_use.workspaces]
enabled = true             # 預設 false
max_per_agent = 3          # 每位員工的工作區數量上限，1–20
max_bytes = 67108864       # 每個工作區的容量上限（64 MiB），1 MiB–1 GiB
max_files = 1000           # 每個工作區的檔案數上限，1–10000
retention_days = 30        # 多久沒掛載就到期；0 = 不會到期，最多 3650
min_free_bytes = 536870912 # 主機磁碟至少要留的空間（512 MiB）
admin_approval_minutes = 30  # 終端機管理動作的儀表板核准，有效幾分鐘，1–1440
```

這一節逐鍵嚴格檢查：型別錯、超出範圍或出現不認得的鍵，整節視為無效，功能等於關閉（不會套用預設值），`duduclaw doctor` 會標示失敗。設定每次使用時重新讀取，改完不用重開 gateway。

員工的開關在該員工的 `agent.toml`：

```toml
[capabilities]
computer_use = true

[capabilities.computer_use_config]
workspace = true
```

兩個開關缺一就不能建立、掛載或寫入。

## 員工怎麼用

| 工具 | 做什麼 |
|---|---|
| `computer_session_start` 加 `workspace` 參數 | `"new"` 建立一個新工作區並掛上；填工作區 id（`ws-` 開頭）掛回既有的。回覆會帶 `workspace_id`、`mount_path`、版本與用量 |
| `computer_workspace_list` | 列出自己的工作區：狀態、版本、用量、配額、到期時間、是否正被使用，以及檔案清單（路徑、大小、sha256） |
| `computer_workspace_read` | 讀一個 UTF-8 文字檔（最多 48 KiB）。內容包在資料圍欄裡，並附注入掃描結果 |
| `computer_workspace_write` | 寫一個 UTF-8 文字檔到「自己這次 session 掛上的」工作區。可帶 `expected_revision`（上次看到的版本），工作區被改過就拒絕 |

典型流程：`computer_session_start(workspace="new")` → 瀏覽、截圖 → 把整理好的內容 `computer_workspace_write` 進去 → 結束 session。之後 `computer_workspace_list` 找到 id，用 `computer_session_start(workspace="ws-…")` 掛回去繼續。

讀與列不需要開著 session；寫入一定要在掛著這個工作區的 session 裡進行。

## 檔案在哪裡

- 主機上：`<home>/computer_workspaces/<id>/data/`。目錄權限 0700，只有執行 gateway 的使用者能進去。
- 容器裡：`/workspace/files`，唯讀。它放在一個只有 root 能進的小 tmpfs（`/workspace`，權限 0700）底下，所以瀏覽器使用的 `sandbox` 帳號讀不到，只有 gateway 用 root 執行的指令讀得到。
- 員工自己的狀態檔 `agents/<員工>/state/computer_workspaces.json` 記著它擁有的工作區憑據（見下方「員工被移除或同名重建」）。

## 規則與上限

- 路徑：相對路徑，最多 4 層，每一層只能用文字、數字、空白與 `-_.()（）`，不能以 `.` 開頭或結尾有空白。路徑以 NFC 正規化。
- 單檔最多 48 KiB 的 UTF-8 文字。工具請求的本體上限是 64 KiB（JSON 編碼後），內容裡有大量引號、反斜線、換行或控制字元時，跳脫後會變長，實際能寫的會少於 48 KiB；這時請分成幾個檔案。
- 配額：超過 `max_bytes` 或 `max_files` 的寫入不會執行，原本的檔案都還在。
- 寫入是原子的：先寫暫存檔、`fsync`、`rename`，再同步目錄。中途失敗，舊檔維持原樣。
- 同一個工作區同時只有一筆寫入（跨行程的檔案鎖）。兩筆帶同一個 `expected_revision` 的寫入，只有一筆會成功，另一筆收到「版本不符」。
- 無法處理的項目：超過 48 KiB 的檔案、硬連結、符號連結、特殊檔案、不合規則的名稱，不會被讀，也不會列出名字，`computer_workspace_list` 只回報 `unprocessable_items` 的數量。這些東西只會在有人從主機直接放檔時出現。
- 雜湊：清單的 sha256 來自 gateway 的帳本，不會每次重讀整個資料夾。gateway 讀不到某個檔案時，該檔回報 `sha256: null`、`hash_unknown: true`。
- 寫入在 session 停止、暫停或威脅等級不是 GREEN 時會被拒絕；讀與列照常。

## 保留期限

`retention_days` 天沒有掛載，工作區轉成「已到期」：不能再掛載或寫入，但檔案不會刪除，擁有者仍可列出與讀取。到期的工作區不佔 `max_per_agent` 的名額。管理者可以用 `renew` 延長（見下方）。

## 租約：同時只有一個 session

一個工作區同時只能被一個 session 掛著。gateway 每 15 秒續約一次，租約 90 秒。

- 掛著的 session 結束後，租約立刻釋放。
- gateway 在一般運作中當掉，別的 session 最多等 90 秒就能掛。
- **gateway 在 session 啟動途中當掉**（例如正在等核准或等容器起來），起始租約要涵蓋整個啟動時間，最長約 8.5 分鐘（90 秒加上 415 秒的啟動上限）不能再掛這個工作區。

**只有一個 gateway 做登錄維護。** 只有持有資料目錄實例鎖的那個 gateway，會在啟動時與每一輪定期清掃（每 10 分鐘）整理工作區登錄（過期租約、寫入意圖、沒做完的建立與刪除、保留期限），並移除過期的工作區容器。同一個資料目錄上的第二個 gateway 照常服務自己的 session、為自己的 session 續租約，但不做登錄維護，也不移除工作區容器；已結束或超過期限的殘留容器仍由每個 gateway 各自清掉。儀表板與指令列的操作不受影響。

## 員工被移除或同名重建

工作區認的是建立它的那一位員工，名字相同的後來者不算。建立時會產生一組隨機憑據，同時寫進登錄與員工目錄的狀態檔；每次掛載、讀、寫、列都會比對。

- 員工被移除（`agent.toml` 不見，或 `_trash` 裡有比工作區新的同名項目），或
- 同名的員工是後來重建的、手上沒有這組憑據，

任一成立，工作區就轉成「無主」，只有管理者能處理（刪除）。這一版沒有把工作區改指派給別的員工的功能。

## 操作者的指令

```bash
duduclaw ops computer-workspaces list [--owner <員工>]
duduclaw ops computer-workspaces fence <工作區 id> [--reason <原因>]
duduclaw ops computer-workspaces revoke <工作區 id>
duduclaw ops computer-workspaces regrant <工作區 id>
duduclaw ops computer-workspaces renew <工作區 id>
duduclaw ops computer-workspaces delete <工作區 id> --confirm
```

| 動作 | 做什麼 | 需要核准 |
|---|---|---|
| `list` | 列出全部工作區（狀態、擁有者、用量、租約到期時間） | 不需要 |
| `fence` | 收回目前 session 的控制權 | 需要 |
| `revoke` | 暫停授權：只剩狀態可看，不能讀、寫、掛載 | 需要 |
| `regrant` | 解除暫停 | 需要 |
| `renew` | 延長保留期限；已到期的恢復可用 | 需要 |
| `delete` | 刪除工作區與裡面所有檔案，無法復原 | 需要 |

**指令列上所有會改變狀態的動作都要到儀表板核准**（只有 `list` 不用）。終端機分不出下指令的是管理者，還是有 Bash 的 AI 員工；如果 `fence`／`revoke` 立即生效，任何有 Bash 的員工都能停掉別人的工作區。**緊急處置請用儀表板**（`computer_workspaces.*`，由已登入的 Admin 執行，立即生效並等進行中的動作結束）**或關掉 `[computer_use.workspaces]` 總開關**。

流程：第一次執行只會在儀表板的待辦清單建立一筆請求，指令印出「請到儀表板的待辦核准」並以非零結束碼退出。管理員（Admin）在儀表板核准後，在 `admin_approval_minutes`（預設 30 分鐘）內**再執行一次同一個指令**才會真的動作。

- 每筆核准綁定「動作、工作區 id、工作區狀態版本」。核准之後狀態有變（例如有 session 掛上、被 fence），核准就失效，要重新申請。
- 每筆核准只能用一次。核准用掉之後動作若失敗，訊息會寫「核准已使用，需要重新申請」。
- 只能在儀表板由 Admin 決定；從聊天通道按按鈕或回覆一律不生效，非 Admin 帳號在儀表板也不能核准。
- 同一個工作區、同一個動作同時只會有一筆待決請求；狀態變了會更新那筆請求的內容，不會另外再發通知。每個工作區每小時最多推播 2 次通知，超過的只留在儀表板待辦清單並記一筆稽核。
- 通知卡片只有工作區 id、擁有者、動作、檔案數與位元組數，以及「這筆請求由本機指令列建立，系統無法確認下指令的人是誰」。

**其他規則**：

- 指令偵測到任何 DuDuClaw 工作階段或身分的環境變數（例如 `DUDUCLAW_AGENT_ID`、`DUDUCLAW_TURN_ID`）就拒絕執行。
- AI 員工的 Bash 執行 `duduclaw`／`duduclaw-pro` `ops computer-workspaces` 會被 agent-file-guard 擋下（未驗證身分的呼叫者也一樣）。
- 終端機的動作在稽核裡記成「本機指令列（身分未驗證）」。
- 每一次指令列動作的提出、套用與拒絕都寫一筆安全稽核（`security_audit.jsonl`，事件 `computer_workspace_cli_action`）。
- 核准後由終端機套用的 fence／revoke／delete 只改登錄（終端機這個行程沒有進行中的 session）：還沒通過最後一道檢查的動作會立即被拒，已經通過的那一個會跑完，session 最慢在下一次操作或約 15 秒後的續約時結束。

## `duduclaw doctor`

「電腦操作工作區」一列：

- 未啟用：通過，寫明未啟用。
- 設定無法解析、平台不支援、`computer_workspaces/` 根目錄不安全、登錄打不開：失敗。
- 取不到 Docker daemon id、磁碟低於 `min_free_bytes`、有刪除未完成／建立失敗／無主的工作區、有結果無法確認的寫入、有在失去掛載後才落地的寫入、資料目錄讀不到：警告，並列出工作區 id。

功能沒啟用、也從沒建立過工作區時，gateway 不會建立 `computer_workspaces.db`。

## 關掉功能會怎樣

把 `enabled` 改回 `false`（或關掉員工的 `workspace`）只會停止新的建立、掛載與寫入，掛著的 session 在下一次操作時結束。擁有者仍可列出與讀取既有內容。要讓內容也讀不到，請用 `revoke`；要清掉，請用 `delete`。

## 已知限制

- **擁有者隔離只對三個工作區工具成立。** 有 `Read` 或 Bash 的 AI 員工可以直接讀主機上的 `<home>/computer_workspaces/`。有不受限 Bash 的員工也可以繞過指令列的身分判斷與核准閘，直接改 `approvals.db`、`computer_workspaces.db` 與工作區目錄，或在儀表板核准之前就把資料庫裡的請求改成已核准。Bash 那道檢查只是減速。真正的隔離是不給 Bash，或開[任務沙箱](task-sandbox.md)。
- 擁有者憑據放在員工自己的目錄裡，員工本身讀得到也改得到；有 `Read` 的員工也能讀到已移除的前任留在 `agents/_trash/` 裡的憑據。這道比對擋得住只用產品工具的同名新員工，擋不住有不受限檔案存取的員工。
- 檔案在磁碟上是明文。去識別化的「不還原」只代表寫入時不把遮蔽代號換回原值，不代表磁碟上沒有個人資料。
- `denied_tools` 列了工作區工具，並不會阻止 `computer_session_start` 帶 `workspace` 參數把工作區唯讀掛進容器。真正的開關是 `[capabilities.computer_use_config] workspace`。
- 功能關閉後，擁有者仍可列出與讀取既有內容（見上一節）。
- 瀏覽器以 `--no-sandbox` 執行（電腦操作既有的情況）。
- 掛載來源的檢查與 `docker run` 之間有一小段時間差；能利用它的是和 gateway 同一個 OS 使用者的程式。容器裡只有 root 進得了 `/workspace`，目前沒有任何工具從容器裡讀工作區。
- root 的 `docker exec` 可以寫 `/workspace` 那個 64 KiB 的 tmpfs（`/workspace/files` 本身唯讀）；只有 gateway 會這樣執行。
- gateway 在 session 啟動途中被中斷（逾時或連線斷掉）時，沒有立即釋放租約，要等起始租約到期（最長約 8.5 分鐘）。
- 無主的工作區只能刪除，不能改指派給別的員工。
- 真容器測試只在 macOS arm64 的 Docker Desktop 上跑過；Linux 原生 Docker、amd64、Windows 都沒有驗證。Windows 上這個功能明確不可用。
