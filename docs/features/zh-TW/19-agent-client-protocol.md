# ACP 與 A2A 介面

DuDuClaw 分別提供 IDE 對話與外部任務介面，各自有不同的傳輸方式、憑證和任務生命週期。

| 介面 | 入口 | 用途 |
|---|---|---|
| Agent Client Protocol v1 | `duduclaw acp`，stdio | IDE 工作階段與提示回合 |
| 既有 A2A 指令 | `duduclaw acp server`，stdio | 使用 `agent/discover`、`message/send`、`tasks/send` / `tasks/get` / `tasks/cancel` 的本機腳本 |
| A2A v1.0 JSON-RPC，v1.80.0 起 | Gateway `POST /a2a` | 經認證的外部任務看板目標 |
| MCP | `duduclaw mcp-server` 或獨立 HTTP server | 提供 AI runtime 與 MCP client 使用的工具 |

## Gateway A2A v1.0（v1.80.0 起）

Gateway 在 `/.well-known/agent-card.json` 提供有簽章的 Agent Card，列出 JSON-RPC 介面、協定版本 `1.0`、管理員明確公開之員工實際啟用的技能，以及 Dashboard Bearer JWT 認證方式。簽章用來驗證卡片完整性，不會授予呼叫者任何技能權限。卡片不宣告串流或推播通知支援。僅公開名稱與說明皆非空的已啟用技能，並標記實際員工的 `employee:<id>`。未公開員工或沒有可公開技能時，公開卡片回覆 HTTP 503；登入者沒有可用技能時，`GetExtendedAgentCard` 回覆 `-32007`。`SendMessage` 只接受至少有一項可公開技能的已公開員工。簽章金鑰不可用時，探索端點回覆 HTTP 503，不會提供未簽章卡片。

預設不公開任何員工。在 `config.toml` 填入既有員工 id：

```toml
[a2a]
exposed_agents = ["researcher"]
public_url = "https://host.example/a2a"
trusted_proxy_cidrs = []
```

`public_url` 是外部可連線的 `/a2a` HTTPS 網址，TLS 由 gateway 前方的反向代理設定。公開名單仍受既有授權限制，不會替 Dashboard 帳號增加員工存取權、啟用停用的技能，或改變工具能力。名單為空、設定無法讀取或 A2A 設定無效時，拒絕任務存取。移除某位員工後，也無法再透過 A2A 存取該員工先前的任務。

`trusted_proxy_cidrs` 預設為 `[]`，最多接受 32 筆有效 IPv4／IPv6 CIDR，只應填入你控制的直接反向代理。符合名單的代理必須將 `X-Forwarded-For` 覆寫為單一 client IP；缺漏、重複、多層 IP 清單或無效值都會被拒絕。其他來源的標頭不採用。未設定時，同一代理後方的 client 仍在各流量類別內共用代理 IP 額度。例如，僅當 `192.0.2.10` 是你的直接代理時，才使用 `trusted_proxy_cidrs = ["192.0.2.10/32"]`；此設定不授予呼叫權限。

每個 RPC 都需要 `Authorization: Bearer <Dashboard access JWT>` 和 `A2A-Version: 1.0`。JWT 由既有 Dashboard 帳號登入流程取得；此端點不接受舊版共用管理員 token 或 MCP HTTP-server API key。公開卡片可匿名探索；`GetExtendedAgentCard` 需要認證，且保留全部存取限制。

版本標頭缺漏或為空時視為 `0.3`，HTTP 介面不支援此版本，回覆 `VersionNotSupportedError`（`-32009`）。請使用 v1.0 的 PascalCase 方法。既有 stdio 名稱 `message/send`、`tasks/get` 作為 HTTP 別名保留，但仍須提供 `1.0` 標頭與 v1.0 請求物件，不代表支援 0.3 傳輸。

### 提交與查詢

下列範例尚未經活體 gateway 驗證。請替換主機名稱與員工 id，並在 `DUDUCLAW_DASHBOARD_JWT` 提供有效的 Dashboard JWT。

```sh
curl --fail-with-body 'https://host.example/a2a' \
  -H "Authorization: Bearer $DUDUCLAW_DASHBOARD_JWT" \
  -H 'A2A-Version: 1.0' \
  -H 'Content-Type: application/json' \
  --data '{"jsonrpc":"2.0","id":1,"method":"SendMessage","params":{"metadata":{"agentId":"researcher"},"message":{"messageId":"request-001","role":"ROLE_USER","parts":[{"text":"Summarize the public project documentation."}]},"configuration":{"returnImmediately":true}}}'
```

`metadata.agentId` 指定公開的員工。`message.messageId` 必填，`role` 必須為 `ROLE_USER`，`parts` 只接受純文字。請設定 `configuration.returnImmediately = true`；此實作經由既有任務看板目標建立流程新增非同步目標，回傳 `result.task`。執行時沿用員工設定的 runtime 與 goal loop 權限閘。提交者須有直接 Operator 權限或為 Admin，僅有團隊授權不足以提交。每則訊息最多 4,000 字，HTTP 本文最多 64 KiB。僅接受新任務，請省略 `message.contextId` 與 `message.taskId`；回傳的 `contextId` 等於 task id。`historyLength` 只能省略或設為 `0`。登入帳號與 `messageId` 組成持久化唯一鍵。以相同完整 `params` 重送會回傳原任務；同一 id 搭配不同參數會被拒絕，JSON 物件欄位順序不影響判定。刪除任務不會釋出該 id，也不會因重送而重建任務；重送仍須通過目前授權。

去重比對完整正規化 `params`：忽略物件欄位順序、保留陣列順序，省略欄位與明確填入預設值視為不同參數。相同 id 的內容衝突回覆 `-32602`（`INVALID_ARGUMENT`）；任務刪除後重送回覆 `-32001`（`TASK_NOT_FOUND`）。分頁 token 綁定所有者與 context、status、status timestamp、artifacts 篩選條件；`pageSize` 可逐頁調整。

取得 task id 後，在相同端點送出以下請求，沿用認證與版本標頭：

```json
{"jsonrpc":"2.0","id":2,"method":"GetTask","params":{"id":"<task-id>"}}
{"jsonrpc":"2.0","id":3,"method":"ListTasks","params":{"pageSize":20}}
{"jsonrpc":"2.0","id":4,"method":"CancelTask","params":{"id":"<task-id>"}}
{"jsonrpc":"2.0","id":5,"method":"GetExtendedAgentCard","params":{}}
```

`ListTasks` 支援 `contextId`、`status`、`statusTimestampAfter`、`includeArtifacts`，`pageSize` 為 1 至 100，並使用不透明的 `pageToken` 游標；將 `nextPageToken` 帶入下一頁。任務依單調遞增的建立序號排序，第一頁固定序號上界，後續 token 沿用同一上界；分頁期間新增的任務須另開不帶 token 的列表才會出現。每頁仍重查權限與狀態條件，固定的是任務範圍，並非凍結任務狀態。`totalSize` 只計目前可讀的任務；單一呼叫者儲存的任務超過 10,000 筆時，回覆 `RESOURCE_EXHAUSTED`，不提供不完整的總數。讀取與取消只限同一登入帳號透過 A2A 建立的任務，並重新檢查公開名單、目前帳號與員工權限，以及任務閱覽對象。所有者身分保存在任務上，子任務繼承相同所有者。一般 Dashboard 的任務列表、讀取、修改、刪除、停止、留言、附件、活動紀錄與推播同樣受所有者限制；Admin 沒有例外，也未提供另外的管理員覆寫入口。任務刪除後保留所有者紀錄，繼續保護殘留證據。外部 MCP 呼叫者若沒有 Dashboard 所有者身分便會被拒絕；經驗證且受指派執行該任務的員工保留工作所需存取權。

狀態使用 `TASK_STATE_*`，包含 `TASK_STATE_SUBMITTED`、`TASK_STATE_WORKING`、`TASK_STATE_COMPLETED`、`TASK_STATE_FAILED`、`TASK_STATE_INPUT_REQUIRED` 與 `TASK_STATE_CANCELED`。等待核准或人工輸入不代表完成。取消沿用任務樹停止機制；若停止仍待確認或結果不確定，應持續查詢，不能把回覆當作外部程序或副作用已實體中止的證明。取消終止狀態的任務回覆 `TaskNotCancelableError`（`-32002`）。

### 支援範圍與錯誤

`forward.recent` 與 `forward.chain` 也檢查保留的所有者紀錄，包含已刪除的 A2A 任務；Admin 不能繞過別人的所有權。`tasks.flow_metrics` 先排除其他 A2A 所有者的任務，再計算數量、時間與輪數統計。非 A2A 任務的存取規則維持不變。

任務資料庫在任何 schema 遷移前，若已有 A2A 結構或重送紀錄，就必須有可讀取的 `task_access_principals` 表。表格缺失或無法讀取時拒絕開啟，不會從尚存任務重建空表；需從一致的備份還原完整所有者紀錄。全新資料庫及沒有 A2A 痕跡的舊版資料庫仍可遷移。

既有 `keys/a2a-signing.ed25519` 必須是一般檔案，不能是符號連結。Unix 另要求檔案屬於 gateway 的有效執行使用者，且權限精確為 `0600`；Windows 不檢查 Unix owner／mode。不合格的檔案直接拒絕，不會覆寫或自動改權限。限流先檢查並記錄來源 IP 額度，通過後才扣全域額度；來源已超限的請求不會耗掉其他來源的全域容量。

| 操作 | 支援範圍 |
|---|---|
| `SendMessage` | 新增純文字、非同步目標 |
| `GetTask`、`ListTasks` | 查詢自己建立且仍有權限的 A2A 任務 |
| `CancelTask` | 請求停止自己的任務樹 |
| `GetExtendedAgentCard` | 需認證的卡片，不增加權限 |
| `SendStreamingMessage`、`SubscribeToTask` | 不支援，沒有 SSE |
| 等待完成才回覆、接續既有任務、推播通知設定 | 不支援 |

JSON-RPC 協定錯誤使用標準錯誤碼；A2A 錯誤依情況使用 `-32001` 至 `-32009`，`error.data` 是含 `google.rpc.ErrorInfo` 型別資訊的陣列。未認證與未授權請求使用 HTTP 401/403。任務操作前會檢查輸入與方法權限，並留下安全稽核紀錄。公開探索、認證失敗與已認證 RPC 各自使用獨立的 IP 額度，每個 IP 每分鐘限 120 次；三類全域上限依序為每分鐘 600、600、1,200 次，RPC 另有每個呼叫者每分鐘 60 次的限制。匿名流量不會扣除已認證 RPC 額度。每個 IP／呼叫者限流器最多保存 10,000 個鍵，滿額拒絕新鍵，過期清理另有頻率限制。此介面僅提供 HTTP，WebSocket 的 `a2a.*` 呼叫會被拒絕。詳見[安全防護](05-security-defense.md)。

## IDE Agent Client Protocol

IDE agent panel 使用 `duduclaw acp`，支援 `initialize`、`session/new`、`session/prompt`、`session/update` 與 `session/cancel`。尚未設定的 home 會回覆 `AUTH_REQUIRED`，引導至終端 onboarding。提示回合使用 gateway 回覆管線，由 Main 角色員工處理。

Zed 設定：

```json
{"agent_servers":{"DuDuClaw":{"command":"duduclaw","args":["acp"]}}}
```

Gateway A2A HTTP 方法與 IDE 工作階段方法不同；IDE panel 請勿設定為 `duduclaw acp server`。

## 既有 stdio A2A

`duduclaw acp server` 保留既有本機協定。它的 `message/send` 以 `a2a-client` 身分寫入 gateway 委派佇列，受既有 `[acp] trusted` 政策限制。程序內的任務索引與盡力而為的佇列觀察，和 HTTP 持久化任務看板目標分開；重啟 stdio 程序會失去已索引的 task id，查無佇列回覆也不能證明任務完成。舊版 `tasks/send` 記憶體管理器同樣與 HTTP 任務看板分開。

上述 HTTP 公開名單與 JWT 任務歸屬規則適用於 `/a2a`，不會改寫 stdio 指令的信任模型。本文舊版將 HTTP 描述為未來僅供探索的擴充，並誤稱具備已實作的串流與多回合任務傳輸；HTTP 支援範圍以本頁表格為準。
