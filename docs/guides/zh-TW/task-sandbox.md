# 任務沙箱：讓 AI 員工的委派任務在上鎖的容器裡執行

任務沙箱會把某位員工被委派的任務放進 Docker 容器執行：該員工的 AI CLI 在唯讀、非 root、有資源上限的容器裡啟動，使用一個用完即丟的私有工作目錄，只有最後的回覆文字會傳回來。預設關閉，逐位員工開啟。

它不是 `duduclaw secaudit`（PoC 步驟）與 PTC 使用的腳本沙箱。那是另一條獨立的程式路徑：跑的是腳本，沒有 AI CLI，預設不連網。兩者共用的只有 image：腳本沙箱讀同一個 `config.toml [container.sandbox] image`，預設也是同一個平台 image，同樣不會自動下載，所以 `docker pull` 一次兩邊都能用。

## 前置條件

- Docker，且執行 gateway 的使用者連得上。任務沙箱只支援 Docker。
- gateway 不能以 root 執行。uid 為 0 會在任何東西啟動前就被拒絕，gid 為 0 則在建立容器時被拒絕。
- 本機要有沙箱 image。預設是平台自己發布、對應目前版本的 image：`ghcr.io/zhixuli0406/duduclaw:v<版本>`。系統不會自動下載，要自己執行：

  ```bash
  docker pull ghcr.io/zhixuli0406/duduclaw:v<版本>
  ```

  `<版本>` 換成你的 gateway 版本（標籤前面有 `v`，例如 `v1.67.0`），或改用自己的 image（見下方設定表）。
- 容器內的 AI 能用的帳號（見[各 runtime 的憑證](#各-runtime-的憑證)）。
- 支援的 runtime：Claude、Codex、Gemini（已棄用）、Antigravity、Grok、OpenAI 相容。

## 為單一員工開啟

在該員工的 `agent.toml` 打開沙箱並允許網路：

```toml
[container]
sandbox_enabled = true
network_access = true   # 必須：容器內的 AI 要連到模型供應商
timeout_ms = 600000     # 單一任務的時間上限（選填）
```

`network_access = false` 會在建立容器之前就被拒絕，因為 AI CLI 連不到供應商就無法運作。沙箱不會偷偷幫你開網路。只放行模型供應商的出站限制目前沒有實作（見[已知限制](#已知限制)）。

任務時間上限仍然是 `agent.toml [container] timeout_ms`。

## 這個開關涵蓋哪些任務

開啟 `sandbox_enabled = true` 後，gateway 啟動這位員工 AI CLI 的每一種途徑，都歸在下面三組之一。

**在沙箱裡執行。** gateway 整件派給員工的工作：

- 其他員工委派的任務，或從儀表板送出的任務（員工之間的 bus），
- heartbeat 從任務看板領取工作的喚醒，
- autopilot 的 `delegate` 或 `run_skill` 動作，
- goal 回合（一律 Solo，見下方），
- 多步驟任務計畫裡的各個步驟。

**開了沙箱的員工不會這樣執行。**

- Team 回合：開了沙箱的員工不會組成團隊。可拆性閘會先於其他所有規則回傳 Solo，理由是 `sandbox_enabled`，連 `gate = "always_team"` 也蓋不過它；所以每一輪 goal 回合都在沙箱裡以 Solo 執行，不會有任何角色成員（規劃、執行、審核、合成）在主機上跑。
- Agent Mail 的到信觸發會跳過。觸發後那次執行的效果要靠沙箱裡沒有的平台工具，而且它處理的是不受信任的外來內容。信件留在員工的收件匣等人處理，不會執行任何東西。這時會寫下方的稽核事件，`path = "mail"`、`action = "skipped"`。

**依設計仍在主機上執行。** 這些需要沙箱刻意不給的平台工具與對話狀態：

- 通道回覆（員工回答聊天訊息），
- 排程（cron）任務，
- 提醒，
- heartbeat 的主動檢查（`PROACTIVE.md`），
- 代替員工產生的 ephemeral agent，
- `duduclaw acp` 工作階段，
- live 模式的 `duduclaw eval`。

這些都會留下紀錄。開了沙箱的員工第一次走到其中一條途徑（執行或跳過）時，gateway 會寫稽核事件 `task_sandbox_not_applied`，details 是 `{path, action}`，並在 log 寫一行警告。`path` 是 `channel_reply`、`cron`、`reminder`、`mail`、`proactive`、`ephemeral`、`acp`、`eval` 之一；`action` 是 `ran_on_host` 或 `skipped`。同一位員工的同一條途徑，每個 gateway 行程只寫一次，忙碌的通道不會灌爆稽核紀錄；gateway 重啟後第一次使用時會再寫一次。沒開沙箱的員工不會產生這個事件。

## `config.toml [container.sandbox]` 設定表

所有鍵都可省略。每次沙箱任務都會重新讀取，改了下一個任務就生效，不用重啟。出現未知的鍵或超出範圍的值，整段設定即視為無效；設定無效代表沙箱不可用，不會退回預設值。例外是 `when_unavailable` 與 `script_when_unavailable`，兩者各自單獨讀取，所以你在修別的鍵時逃生口仍然有效。兩者填了無法辨識的值，都當成 `"fail"`。

| 鍵 | 預設 | 說明 |
|---|---|---|
| `image` | `ghcr.io/zhixuli0406/duduclaw:v<目前版本>` | 要執行的 image，必須已在本機。 |
| `[container.sandbox.executables]` | `claude`、`codex`、`gemini`：`/usr/bin/<名稱>`；`antigravity`：`/usr/local/bin/agy`；`grok`：`/usr/local/bin/grok`；`openai-compat`：`/usr/local/bin/python3` | 各 runtime 在 image 內的執行檔絕對路徑，鍵名為 `claude`、`codex`、`gemini`、`antigravity`、`grok`、`openai-compat`。只有自訂 image 才需要改。 |
| `memory_bytes` | `4294967296`（4 GiB） | 記憶體上限（swap 與記憶體相同）。最多 64 GiB。 |
| `pids` | `128` | 行程數上限。 |
| `cpu_millis` | `1000` | CPU 上限，單位是千分之一核。 |
| `tmp_bytes` | `268435456`（256 MiB） | 可寫入的 `/tmp`（tmpfs）大小。最多 16 GiB。 |
| `workspace_bytes` | `536870912`（512 MiB） | 工作目錄 `/workspace`（tmpfs）的大小。最多 16 GiB。 |
| `max_turns` | `30` | 單一任務的步數上限。 |
| `when_unavailable` | `"fail"` | 任務沙箱不能用時，被委派任務的處置：`"fail"` 或 `"run_unsandboxed"`。只管任務沙箱。 |
| `script_when_unavailable` | `"fail"` | 腳本沙箱不能用時，PTC `execute_program` 腳本的處置：`"fail"` 或 `"run_unsandboxed"`。只管腳本沙箱。 |

範例：

```toml
[container.sandbox]
image = "ghcr.io/zhixuli0406/duduclaw:v<版本>"
memory_bytes = 4294967296
max_turns = 30
when_unavailable = "fail"
```

所有數值都必須是正整數。其他上限：`pids` 65536、`cpu_millis` 256000、`max_turns` 1000。兩個 tmpfs 都算在容器的記憶體上限裡，所以 `tmp_bytes + workspace_bytes` 不能超過 `memory_bytes`。違反任何一條，整段設定即視為無效，沙箱也就不可用。

## 各 runtime 的憑證

沙箱依員工的 runtime 向帳號輪替器取帳號，並遵守該員工的 `account_pool`。只有能交給容器的憑證才有用。

| Runtime | 可用的帳號 |
|---|---|
| Claude | API key，或帶 token 的 OAuth 帳號（用 `claude setup-token` 建立）。只存在於主機鑰匙圈的登入狀態進不了容器。 |
| Codex | API key，或憑證文件：OAuth 帳號所存的密鑰內容是 CLI 的 `auth.json`。 |
| Grok | API key，或憑證文件（CLI 的 `auth.json`）。 |
| Gemini / Antigravity | Gemini API key。 |
| OpenAI 相容 | 該 provider 的 API key。 |

沒有合適的帳號時任務會失敗，錯誤訊息會指出這個 runtime 需要哪一種憑證。

## 沙箱內有什麼、沒有什麼

有：

- 唯讀的根檔案系統、非 root 使用者、丟棄所有 Linux capability、`no-new-privileges`。
- 上表的記憶體、行程、CPU 限制。
- 工作目錄 `/workspace`：大小為 `workspace_bytes` 的 tmpfs，擁有者是容器執行時的使用者，隨容器一起丟棄。任務寫在這裡的東西不會落到主機磁碟。
- 可寫入的 `/tmp`，大小為 `tmp_bytes` 的 tmpfs。
- 員工目錄裡的少數項目，存在時各自以唯讀方式掛在 `/agent/<名稱>`：`SOUL.md`、`IDENTITY.md`、`CLAUDE.md`、`AGENTS.md`、`GEMINI.md`、`CONTRACT.toml`、`SKILLS/`、`wiki/`。項目若是符號連結、有硬連結的檔案、種類不符（該是目錄的地方是檔案，或反過來），或解析後落在員工目錄之外，就會略過，並在 gateway 日誌留下警告。
- 檔案與 shell 工具。

沒有：

- 員工目錄的其他部分。`.mcp.json`（裡面有該員工的 MCP key 與身分 token）、`.claude/`、`state/`、`agent.toml`、資料庫，以及上面沒列到的任何項目都不會掛進去。
- 平台的 MCP 工具（記憶、任務、通道）。
- 網頁工具與子代理。
- 寫回員工目錄。沙箱內的檔案變更都會被丟棄，任務的產出只有最後的回覆文字。

AI 若使用了允許範圍（檔案與 shell）以外的工具，任務會被中止並寫入稽核事件 `task_sandbox_tool_violation`。步數到頂時，會回傳它最後的回覆文字；沒有回覆文字則回錯誤。

## 沙箱不能用的時候

`sandbox_enabled = true` 時，只要符合下列任一項，任務就會失敗，不會改成不隔離執行：Docker 連不上、image 不在本機、gateway 以 root 執行、主機不是 unix 系統、設定無效、`network_access = false`、沒有可用帳號、runtime 不支援。每次失敗都會寫入稽核事件 `task_sandbox_unavailable`（附原因代碼），並回傳可讀的錯誤。

逃生口：在 `config.toml [container.sandbox]` 設 `when_unavailable = "run_unsandboxed"`。之後任務會像舊版一樣在主機上不隔離地執行，而且每個這樣的任務都會寫入稽核事件 `task_sandbox_bypassed`。只有在你接受不隔離執行時才用。

PTC `execute_program` 使用的腳本沙箱有自己的鍵 `script_when_unavailable`；`when_unavailable` 管不到它，兩個鍵互不影響。分開的理由是風險不同：一個讓被委派的 AI 任務在主機上不隔離地執行，另一個讓送進來的腳本在主機上執行。預設 `"fail"` 時，腳本沙箱不能用就完全不執行腳本：工具回傳 `Script sandbox unavailable (<代碼>): …` 並附上 `docker pull <image>` 指令，同時寫入稽核事件 `script_sandbox_unavailable`。設成 `script_when_unavailable = "run_unsandboxed"` 時，腳本會像舊版一樣在主機上執行，而且每次都寫入 `script_sandbox_bypassed`。兩個事件都帶 `reason` 與腳本的 `language`。原因代碼有 `invalid_config`、`no_runtime`、`runtime_unhealthy`、`image_missing`、`create_failed`、`start_failed`。`duduclaw secaudit` 的 PoC 步驟沒有逃生口，永遠不在主機上執行。

## 殘留清理

任務容器以 `--rm` 與 `--pull never` 建立，任務結束時 gateway 會把它移除。gateway 若在任務中途被終止，留下的東西由背景清理移除。它在 gateway 啟動時執行，之後每 10 分鐘一次，只處理這個 gateway home 的殘留（同一個 Docker daemon 上其他 home 的容器一律不碰）：

- 已結束（exited）或 dead 的容器，以及超過截止時間 600 秒以上的容器；
- `<home>/sandbox/runs/` 底下沒有任何剩餘容器在用、而且超過 600 秒的執行目錄。

列不出 Docker 的容器時，什麼都不刪。不論是背景清理還是任務結束時的清理，失敗都會寫入稽核事件 `task_sandbox_cleanup_failed`，附 `reason` 與 `count`。從沒跑過沙箱的 home 會直接略過，不會去連 Docker。

## 用 `duduclaw doctor` 檢查

```bash
duduclaw doctor
```

任務沙箱這一項會回報：Docker 是否連得上、沙箱 image 是否已在本機、哪些員工開了沙箱，並對其中 `network_access = false` 的員工發出警告。只要有員工開了沙箱，這一項會多一行，說明這些員工的通道回覆、排程任務與提醒仍在主機上執行、他們的 goal 回合一律 Solo（不組團隊）、收到新信件也不會喚醒他們。這項只檢查前置條件，不會真的跑任務。

## 疑難排解

沙箱任務失敗時，會回覆給委派這個任務的一方。沙箱拒絕執行時，回覆是 `⚠️ 子任務未執行（任務沙箱）：Task sandbox unavailable (<代碼>): …`，對應下表第一組列。任務開始後才失敗時，回覆是 `⚠️ 子任務失敗（任務沙箱，<代碼>）：<訊息>`。回覆只帶原因與處理方式；AI CLI 或 Docker 的原始輸出不會放進回覆，會寫進 gateway log（每次失敗一行 `warn`，附員工與原因代碼）。

| 你看到的 | 原因 | 處理 |
|---|---|---|
| `(docker_unreachable)` | Docker 常駐程式沒在跑，或 gateway 使用者用不了。 | 啟動 Docker；確認 gateway 使用者能執行 `docker version`。 |
| `(image_missing)` | image 不在這台機器上。 | 執行錯誤訊息中給的 `docker pull <image>`，或把 `image` 指向你已有的 image。 |
| `(network_disabled)` | 該員工是 `network_access = false`。 | 在該員工的 `[container]` 設 `network_access = true`。 |
| `(root_user)` | gateway 以 uid 0 執行。 | 改用一般使用者執行 gateway。 |
| `(unsupported_platform)` | 主機不是 unix 系統（例如 Windows）。 | 在 Linux 或 macOS 上執行 gateway。 |
| `(no_account)` | 沒有帳號能在容器內跑這個 runtime。 | 依錯誤訊息補上對應種類的帳號（見憑證表）。 |
| `(unsupported_runtime)` | 該員工的 runtime 不能在沙箱內執行。 | 改用支援的 runtime。 |
| `(invalid_config)` | `[container.sandbox]` 有不合法的值（包括 `tmp_bytes + workspace_bytes` 超過 `memory_bytes`），或員工沒有設定模型、逾時為 0。 | 修正錯誤訊息指到的那個鍵。 |
| `authentication failed: ...` | 供應商拒絕了這份憑證。 | 更換錯誤訊息中那個帳號的憑證。 |
| `... rate-limited or rejected the request for quota` | 供應商額度問題。 | 等待，或加一個帳號。 |
| `... used a tool outside the allowed file/shell surface` | AI 用了沙箱不允許的工具。 | 把這個任務交給沒開沙箱的員工，或調整任務。 |
| `... reached the sandbox step limit` | 還沒回覆就到了 `max_turns`。 | 調高 `max_turns` 或拆分任務。 |
| `... timed out after N s` | 到了員工的 `timeout_ms`。 | 調高 `timeout_ms`。 |
| `... sandbox container refused: the gateway runs as root (uid or gid 0)` | gateway 的群組 id 是 0。 | 改用主要群組不是 root 的使用者執行 gateway。 |
| `Script sandbox unavailable (<代碼>): ...`（來自 `execute_program`） | 腳本沙箱不能用，且 `script_when_unavailable` 是 `"fail"`。代碼是 `invalid_config`、`no_runtime`、`runtime_unhealthy`、`image_missing`、`create_failed`、`start_failed` 其中之一。 | 啟動 Docker，執行錯誤訊息中給的 `docker pull <image>`；若是 `invalid_config` 則修正 `[container.sandbox]`。只有在你接受腳本在主機上執行時，才設 `script_when_unavailable = "run_unsandboxed"`。 |

## 已知限制

- 出站沒有限定在模型供應商，容器使用一般的 bridge 網路。主機網路連得到的地方它都連得到，包括雲端的 metadata 端點（`169.254.169.254`），以及在 Docker Desktop 上監聽於主機的服務。
- 交給 AI CLI 的憑證，容器內的 AI 看得到（環境變數與 CLI 自己的憑證檔）；出站沒有限制，所以也送得出去。請用可以承受外洩、隨時能輪替的帳號，不要用主要帳號。
- 只有[這個開關涵蓋哪些任務](#這個開關涵蓋哪些任務)第一組的任務種類會進沙箱。通道回覆、cron 任務、提醒與該節列出的其他主機途徑都沒有隔離；`task_sandbox_not_applied` 稽核事件只記錄它們跑過，不會擋下它們。
- 沙箱內沒有平台 MCP 工具。
- 檔案產出不會寫回員工目錄。
- 只支援 Docker；任務沙箱不使用 Apple Container 與 WSL2。
- 不會自動下載 image。
- 腳本沙箱沒有啟動時的清理。執行腳本的行程（服務該員工的 `duduclaw mcp-server`，或 `duduclaw secaudit`）若在中途結束，腳本的容器會一直跑到腳本自己結束為止。
- 在 Windows 上，腳本沙箱先試 WSL2，再試 Docker。WSL2 這條路（包括把 Windows 路徑轉成 WSL 路徑）只做過交叉編譯，還沒在真正的 Windows 主機上執行過。
- gateway 本身跑在容器裡時再啟用任務沙箱，本指南沒有涵蓋，也沒有驗證過。
- 實際跑過的範圍：用已發布的平台 image（`v1.66.1`），Codex、Antigravity、Grok 的 CLI 能在沙箱內啟動並連到各自的供應商（以無效金鑰驗證，供應商回拒絕）；另以 Codex 跑過一次完整的委派任務（那次用的是本機自建、內含相同 CLI 的 image，不是已發布的那一個）。Claude、Gemini CLI 與 OpenAI 相容 runtime 還沒有在沙箱內對真實供應商跑過。
