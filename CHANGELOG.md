# Changelog

## [Unreleased]

### Added
- **第三方 MCP 工具的動作類型與動作規則**：`.mcp.json` 其他 MCP server 的工具，依其 `tools/list` 的 `annotations` 分類（`destructiveHint` ⇒ `delete`；`readOnlyHint` 只有在 `agent.toml [capabilities] trusted_read_hint_servers` 列出該伺服器時才算 `read`，否則 `modify`；沒有標記一律 `modify`），並在 `duduclaw mcp-proxy` 與 `duduclaw mcp-remote-bridge` 套用 `action_rules`：`block` 從員工看到的 `tools/list` 隱藏並以 `-32003` 拒絕（稽核 `third_party_tool_refused`），`ask` 走 ApprovalBroker（核准系統不可用即拒絕，稽核 `third_party_tool_approval`），探索通道只開放 `read`。工具規則可寫 `<伺服器>.<工具>`。啟動時的 `.mcp.json` 改寫在員工有 `action_rules` 或處於探索通道時也會把 stdio 伺服器包進 proxy（原本只有遮蔽啟用時），心跳主動檢查的 spawn 一併改用。儀表板 `MCP → 遠端伺服器` 新增「第三方工具與其動作類型」（RPC `mcp.tool_effects`，管理者）。只有 Claude CLI 涉及；`url`／`type` 項目不涵蓋。
- **MCP Events 接收端**（草案 MCP Events 擴充的 webhook 模式，依 Triggers & Events 工作小組設計草稿實作）：管理者 RPC `mcp.events_subscribe`／`_list`／`_unsubscribe`／`_rotate`，對已連線的遠端伺服器呼叫 `events/subscribe`，事件送到一律掛載的 `POST /webhook/mcp-events/{id}`（不認得的 id 回 404）。每個訂閱有自己的 `whsec_` 簽章金鑰（以本機金鑰檔加密，可更換、可撤銷），投遞依 Standard Webhooks 以常數時間驗證，5 分鐘時間窗、重播快取、256 KiB 上限、每訂閱每分鐘 120 次；回應驗證挑戰，記錄 `gap`／`terminated`，排程在授權到期前重新訂閱。接受的事件寫成 `events.db` 的 `mcp.event`（`input_guard` 掃描、資料上限 16 KiB），自動化規則新增觸發 `mcp_event`，持續責任新增事件來源 `mcp.event`。事件啟動的工作預設在唯讀探索通道執行（佇列新增 `lane` 欄位、Claude CLI 帶 `DUDUCLAW_LANE=explore` 與唯讀內建工具），訂閱時明確允許才用一般模式；非 Claude runtime 的員工無法執行這類工作。需要 `config.toml [mcp_events] public_base_url`。儀表板「遠端伺服器的事件」區塊，三種語言。
- **遠端 MCP 橋接補強**：連線時可設定額外請求標頭（加密保存，不進 `.mcp.json`／argv／環境變數；`Authorization`、`Host`、`Content-Length`、`Mcp-Session-Id` 等傳輸用標頭與含 CR/LF 的值一律拒絕）；中斷連線或移除時，若授權伺服器宣告 `revocation_endpoint` 則在本機刪除後於背景依 RFC 7009 撤銷權杖（稽核 `remote_mcp_token_revocation`）；可逐一開啟的伺服器主動推送 `GET` 串流（預設關，帶 `Last-Event-ID` 續接，405/404 停止）。
- **`duduclaw http-server` 接受 Client ID Metadata Document**：未註冊、帶路徑的 https `client_id` 會被讀取為用戶端中繼資料（只連公開位址、5 秒、5 KiB、快取一小時、`client_id` 必須相符、只接受公開用戶端），授權伺服器中繼資料宣告 `client_id_metadata_document_supported`。`docs/guides/remote-mcp.md` 新增「從 ChatGPT 使用」（三種語言）。
- **持續任務頁面**（`/responsibilities`，三語）：依員工列出持續任務的狀態、是否唯讀、下次叫醒、本期花費／上限、本期次數／上限、連續失敗、到期日；詳細視窗有最近 20 輪與目前這一輪的停止按鈕；依觀看者綁定顯示新增、暫停／恢復／停用／啟用與清除連續失敗（後者需 Manager）。功能關閉時明說並列出 `[responsibilities] enabled` 與 `[dispatch] enabled`，不畫示範資料。新 RPC `responsibilities.status`。新增「每日簡報（唯讀）」「每週回顧（唯讀）」兩個安全範本。
- **持續任務的唯讀執行**：合約可帶 `lane = "explore"`（存在 `scope_json`，沒帶的合約位元不變）。這種持續任務的每一輪都帶 `DUDUCLAW_LANE=explore`：Claude CLI 的內建工具只剩唯讀五個、自動核准只剩 DuDuClaw MCP 工具與這五個；OpenAI 相容執行環境與本地推論工具迴圈的 MCP 子程序也帶這個變數；Codex、Gemini CLI、Antigravity、Grok、通用 CLI 與任務沙箱一律拒絕執行（`explore_lane_unsupported`，fail closed）。
- **員工動作的單次核准（ActionGrant）**：`send`／`purchase` 類工具與 `action_rules` 判 `ask` 的呼叫，核准請求帶 `payload.action_grant`（員工、工具、動作類別、正規化參數的 SHA-256 指紋、遮蔽後的參數摘要）；核准後必須與實際呼叫一致並且只能消耗一次（`consumed:action_grant:<uuid>`），否則拒絕。收件匣詳細面板新增「這份核准涵蓋的內容」。
- **員工摘要「你不在的時候」**：`config.toml [digest] enabled`（預設開，`false` 關閉；`config.toml` 無法解析或值不對時視為關）、`hour`、`timezone`、`exclude_agents`。持有 gateway 實例鎖的 gateway 每天組一次（不呼叫模型），存成 `<home>/digest/<date>.json`、顯示在首頁並以純文字送給管理員已驗證的連結通道；以 `<home>/digest/state.json` 每天只送一次。首頁的完成項目可給 👍／👎／需要修改，寫入 `feedback.jsonl`（`source = "deliverable"`），由演化反思的使用者回饋訊號讀取。新 RPC `digest.latest`、`digest.feedback`。
- **回覆較慢時的進度狀態**：`[channel_reply] interim_status`（預設開，`false` 關閉）、`interim_status_secs = 8`、`interim_status_show_task = false`。外部通道的回覆超過設定秒數仍無任何顯示時，送一行不經模型的狀態（已等多久、員工另有背景工作多久），每輪最多一次。預設開啟（擁有者決定）；多數 CLI 回覆都超過 8 秒，不能原地編輯的通道（LINE、WhatsApp 等）幾乎每輪會多一則訊息並用到推播額度，在意的話請關閉或調高秒數。
- **電腦操作的閒置保留、儀表板即時畫面與接手、疑似注入時暫停**（2026-10-08，都只會收緊員工能做的事）：
  - 閒置保留：`agent.toml [capabilities.computer_use_config] keep_alive_minutes`（預設 0 = 不變，上限 240）。閒置 2 分鐘的 session 改由清掃程式 `docker pause` 暫停容器，員工下一次 `computer_*` 呼叫或儀表板觀看時 `docker unpause` 恢復，暫停超過設定的分鐘數才結束；`max_session_minutes`、`max_actions`、威脅等級、緊急停止、權限與工作區檢查照常，暫停中仍佔用同時 5 個的名額。持有實例鎖的 gateway 會直接移除超過期限標籤的暫停容器（先恢復再移除），其他 gateway 仍在期限後 10 分鐘移除。稽核 `session_pause`／`session_resume`。
  - 即時畫面：員工頁新增「電腦」分頁（三種語言）顯示 session 狀態、已用動作、剩餘時間、閒置保留與觀看人數，可觀看、接手／交還、恢復、結束。RPC `computer_sessions.{status,view,takeover,hand_back,resume,stop}` 每次重新讀取帳號角色與綁定（觀看：管理員、綁定該員工的主管、或以 Operator 以上綁定的帳號；接手／交還／恢復：管理員或以 Operator 以上綁定的主管）。容器內的 `x11vnc` 由 gateway 按需啟動（`duduclaw-vnc`，密碼走 stdin），只聽 root 專用目錄裡的 unix socket、不開 TCP port；儀表板拿一次性、30 秒有效的票連 `/ws/computer-view`，gateway 檢查 Origin、重新判斷權限、每個 session 最多 4 位觀看者，再經 `docker exec -i … duduclaw-vnc-relay` 轉送，主機上不公開任何 port。gateway 解析觀看端的 RFB 訊息：鍵盤、滑鼠、剪貼簿只放行接手者，調整畫面大小與 `xvp` 關機請求一律丟棄，認不得就斷線。每條連線稽核 `view_start`／`view_stop`（誰、多久、轉送幾則輸入）。畫面用 noVNC（新增依賴 `@novnc/novnc` 1.7.0，MPL-2.0；1.6.0 的 npm 版是含頂層 await 的 CommonJS，無法打包），按需載入。
  - 接手：每個 session 一份接手權，持有期間員工的點擊、輸入、按鍵、捲動、導覽與工作區寫入一律以 `human_has_control` 拒絕（截圖、狀態、結束照常）。持有者 `takeover_idle_minutes`（預設 10，上限 60）分鐘沒有輸入、按「交還」、串流啟動失敗或 session 結束時釋放，串流回到唯讀，並透過工作狀態寫一則結構化 `continue` 交接告訴員工有人操作過（誰、多久、幾次輸入、經 `input_guard` 的留言、接手前的交接）。稽核 `takeover_start`／`takeover_end`。
  - 疑似注入時暫停：每次 `computer_screenshot` 經 `duduclaw-eval-dom` 讀出頁面文字交給 `input_guard`，命中封鎖等級就暫停 session（員工動作與工作區寫入以 `injection_suspected` 拒絕、截圖整張遮罩），寫 Activity Feed 並推 L3 通知，稽核只記命中的規則類別；要有人在儀表板按「恢復」。讀不到頁面文字時截圖整張遮罩（`text_unscanned`）但不暫停。MCP 工具文字新增這兩種遮罩原因的說明。
  - 映像：新增 `duduclaw-vnc` 與 `duduclaw-vnc-relay`，移除從沒被 gateway 使用、會在 TCP 5900 以預設密碼對外監聽的 `VNC_ENABLED` 路徑與 `EXPOSE 5900`；映像 workflow 的 smoke test 加上 relay 的 RFB 開頭、瀏覽器帳號連不到 socket、沒有 VNC TCP port 的檢查。
  - 未驗證：開發環境沒有 Docker，暫停／恢復、VNC、轉送與串流只用替身與單元測試驗證，真實瀏覽器中的 noVNC 也沒跑過；`x11vnc` 是否接受 `-rfbport 0 -unixsock` 未驗證（不接受時串流拒絕啟動）。即時串流本身不遮罩。說明：`docs/features/08-browser-automation.md`「Keep-alive, live view and takeover」（三種語言）。

### Changed
- 主動訊息的「被忽略」判斷略過 `feedback.jsonl` 中 `source = "deliverable"` 的列。

### Not verified
- 以上都只對本機假伺服器測試：沒有真的 MCP Events 提供者、沒有真的 ChatGPT 帳號、沒有真的會撤銷權杖或要求自訂標頭的服務商。MCP Events 未實作 poll／push、cursor 重播、`deliveryStatus` 與 `v1a` 簽章。

## [1.71.0] - 2026-10-07 — MCP 獨立模式（mcp init）、外部金鑰的 MCP 授權修補、預設英文 README、商業連結改導總經銷

### Added
- **動作類型、動作規則、探索通道與動作審查**（2026-10，都只會收緊、不會放寬既有閘門）：
  - 每個 DuDuClaw MCP 工具都有副作用類型（`duduclaw_core::tool_effect`：`read`／`draft`／`send`／`publish`／`purchase`／`delete`／`modify`／`admin`），表裡沒有的名稱一律當 `admin`，`duduclaw-cli` 有測試確保每個對外宣告的工具都有明確分類；`tools.catalog` 多了 `effect` 欄位。
  - `agent.toml [capabilities] action_rules = [{ effect = "send", verdict = "ask" }, { tool = "mail_send", verdict = "block" }]`：`tool` 規則優先於 `effect` 規則，同類取最嚴格；`block` 在 MCP dispatch 閘拒絕（`-32003`，稽核 `action_rule`）並從 `tools/list` 隱藏，`ask` 併入核准閘的靜態「一律詢問」走 ApprovalBroker，`allow` 不會解除任何名稱清單的核准或拒絕。`computer_*` 與 `computer_workspace_*` 在 gateway 的電腦操作路由套用同一套判定。格式錯誤的規則或讀不到的 `agent.toml` 讓每個有副作用的呼叫至少「先問人」。放在 `[capabilities]`，員工自己的寫入會被 `org_field_guard` 擋下。儀表板員工編輯頁新增「依動作類型設定規則」區塊（三種語言），經只限管理員的 `agents.update` 寫入並記為 `agent_authority_changed`；`agents.inspect` 回傳 `capabilities.action_rules`。
  - 探索通道：MCP server 以 `DUDUCLAW_LANE=explore` 啟動時只列出、只執行 `read`／`draft` 工具（其他呼叫 `-32003`，稽核 `explore_lane`），變數為其他值或空字串時拒絕所有呼叫。心跳主動檢查的 Claude CLI 帶上這個變數，內建工具只剩 `Read`／`Glob`／`Grep`／`WebFetch`／`WebSearch`；提示文字改為說明這次執行是唯讀。`DUDUCLAW_LANE` 加入 AI 工作階段變數清單。
  - `duduclaw-gateway/src/decide.rs`：封閉選項的 `decide()`，回覆必須剛好是 `{"choice":"<選項>"}`（嚴格 JSON 契約），其他一律 `None`；context 以 `<decide_context>` 圍起當作資料。
  - 動作審查：`config.toml [action_review] mode = "off" | "shadow" | "enforce"`（預設 `off`，每次呼叫重讀；`system.update_config` 接受 `action_review.mode`，變更記為受保護鍵）。只在靜態閘門判定自動執行、且工具有副作用時審查，輸入只有工具名稱、類型、參數鍵、ActionGuard finding token 與 `CONTRACT.toml` 的 `must_not`，不含參數值。`shadow` 只在 `tool_calls.jsonl` 記錄 `action_review`；`enforce` 遇 `block` 拒絕，遇 `ask` 或沒有判定時問人。不認得的值或讀不到的 `config.toml` 視為 `enforce`。
  - 未涵蓋：Claude Code 內建工具與 `.mcp.json` 裡其他 MCP server 沒有分類；動作審查不跑 OS 動作工具、`skill_hub_install` 與 `computer_*`。三層都還沒在真的 gateway 與模型上驗證。說明：`docs/features/05-security-defense.md`「Action rules, explore lane and action review」（三種語言）。
- `duduclaw mcp init`：不裝 gateway 也能用 MCP server。指令會在資料目錄不存在時建立它、簽發一把只帶 `memory:read`、`memory:write`、`wiki:read`、`wiki:write` 的外部金鑰（client id `standalone-<用戶端>`，效期 90 天，只存雜湊），然後依 `--client` 印出設定：`claude-code` 問過之後執行 `claude mcp add duduclaw -s user`（`--yes` 不問；沒裝 Claude Code CLI 就只印指令），`codex` 印 `~/.codex/config.toml` 區塊，`cursor` 印 `~/.cursor/mcp.json`，`print`（預設）三種都印、不替任何用戶端註冊。每次執行都會簽發一把新金鑰（`print` 也一樣），不同的 `--client` 是不同的金鑰與記憶命名空間（`external/standalone-<用戶端>`）。Claude Code 已有 `duduclaw` 時，先讀 user 設定檔（`$CLAUDE_CONFIG_DIR/.claude.json` 或 `~/.claude.json`）：之前 `mcp init` 寫的直接取代，其他的在簽發金鑰前就拒絕並列出（密鑰遮蔽），要加 `--replace`；取代前舊項存到 `~/.duduclaw/mcp_init/`（0600），新的加不進去時用 `claude mcp add-json` 放回。從 npx 快取執行時，設定寫 `npx -y duduclaw@<這一版> mcp-server` 而不是快取裡的路徑；其他情況用執行檔啟動時的路徑，不展開連結（Homebrew／nvm 的版本目錄升級後會消失）；印出的指令在 Windows 版改用雙引號；有設 `DUDUCLAW_HOME` 時設定會帶同一個值。`--scopes` 只接受外部用戶端可以持有的 scope。在 AI 員工的工作階段裡執行會被拒絕（與 `memory forget-source` 同一份環境變數清單；訊息英文在前，並列出是哪個變數）；Bash 通道也把 `duduclaw mcp init` 與 `duduclaw mcp issue-refresh-token` 列為員工不可執行的操作者指令（`duduclaw_core::MCP_KEY_COMMANDS`）。`standalone-` 開頭的名稱保留，不能用來建立 AI 員工（`is_reserved_agent_id`）。說明：`docs/guides/mcp-standalone.md`（三種語言）。
- 官方 MCP Registry 的 `distribution/registries/mcp/server.json` 改成單獨模式的介紹與實際工具數（24），加上 `DUDUCLAW_MCP_API_KEY` 環境變數（必填、secret，說明指向 `duduclaw mcp init`），版本改為 1.70.1。`scripts/release.sh` 會隨其他平台一起改這兩個版本號（新類別 `mcp_registry`），`audit` 與改版後的檢查兩個欄位都讀，不一致即算落差。`distribution/registries/README.md` 註明第一次送件要等含有 `mcp init` 的版本發佈到 npm。原本的說明超過 schema 的 100 字元上限，送件會被擋；版本停在 1.56.0。`distribution/registries/README.md` 的送件步驟一併更新。
- MCP 註冊表搜尋與安裝：儀表板 `MCP → MCP 註冊表` 分頁與 RPC `mcp.registry_search { query, cursor? }`（任何已登入帳號，與 `mcp.import.fetch` 同級）、`mcp.registry_install { name, version?, agent_id, remote?, server_name?, env? }`。Gateway 只連 `https://registry.modelcontextprotocol.io`（呼叫者不能指定網址），經同一套公開位址檢查與固定解析，逾時 20 秒、回應上限 2 MiB、搜尋結果在記憶體快取 10 分鐘；結果整理成名稱、標題、說明（安全截斷）、版本、套件種類、是否有遠端端點、原始碼網址、必填環境變數，以及能否安裝與原因（`no_supported_transport`、`sse_remote_only`、`custom_headers`、`deprecated`、`deleted`）。安裝會依版本重新抓取 `server.json`，交給既有的 `parse_mcp_manifest_value`，npm／PyPI 套件固定為顯示的版本，再走既有的安裝流程：管理者用 `mcp.import.install`（同一個安全掃描與加鎖的 `.mcp.json` 寫入器），其他人變成 `mcp.install_request`（主管 → 管理者核准）。三種語言的介面字串。
- 原生遠端 MCP（Streamable HTTP）與 OAuth 登入，取代 `npx mcp-remote` 橋接：RPC `mcp.remote_connect { agent_id, name, url?, auth: oauth|bearer|none, bearer?, redirect_origin?, client_id?, client_secret? }`、`mcp.remote_status`、`mcp.remote_disconnect { forget? }`（皆限管理者，連線／中斷寫稽核 `remote_mcp_connect_started`／`remote_mcp_connected`／`remote_mcp_connect_failed`／`remote_mcp_disconnected`），Gateway 路由 `GET /oauth/mcp/callback`，儀表板 `MCP → 遠端伺服器` 分頁。OAuth 依 MCP 授權規格：RFC 9728 受保護資源中繼資料（`WWW-Authenticate` 的 `resource_metadata`，否則 well-known 路徑）、RFC 8414／OIDC 授權伺服器中繼資料（`issuer` 必須相符、必須宣告 `S256`）、RFC 7591 動態註冊（`client_name` 為 DuDuClaw，公開用戶端；也可填自己的 client id／secret）、授權碼＋PKCE S256、RFC 8707 `resource`、refresh token 輪替；`state` 為 32 位元組亂數、綁定該次登入、10 分鐘、只能用一次。轉回網址為 `<儀表板來源>/oauth/mcp/callback`（來源是 loopback 或 `[gateway] allowed_origins` 中的 https 來源，`origin_host_matches`）；用區網 IP 或主機名稱以 http 開儀表板時（或 https 但不在白名單），改轉回瀏覽器所在電腦的 `http://127.0.0.1:<儀表板埠>/oauth/mcp/callback`（RFC 8252），在 Gateway 主機上會自動完成，在其他電腦上把網址列的完整網址貼回對話框即可（RPC `mcp.remote_complete`，限管理者；只接受與該次登入註冊的轉回網址主機／埠／路徑相同的本機網址，state 只能用一次）。所有出站網址限 https（本機伺服器才可 http）、只能解析到公開位址且固定解析結果、POST 不跟隨轉址。網址與所有登入資訊存在 `<home>/remote_mcp/servers.json`（0600、跨行程鎖），以本機金鑰檔加密，無法加密就不存；token 更新以每個連線的鎖檔排隊。員工 `.mcp.json` 的項目是 `<duduclaw> mcp-remote-bridge --agent <id> --server <name>`（環境只有 `DUDUCLAW_HOME`），網址與權杖不會出現在設定、argv 或環境變數；隱藏指令 `duduclaw mcp-remote-bridge` 轉送所有 JSON-RPC 訊息、處理 `Mcp-Session-Id` 與 `MCP-Protocol-Version`、到期前一分鐘或收到 401 時更新權杖一次後重試，refresh token 被拒時標示需要重新登入。遮蔽啟用時橋接項目照樣由 `mcp-proxy` 包住；`duduclaw doctor` 的員工 MCP 列會標出橋接項目的主機與連線狀態。說明：`docs/guides/mcp-ecosystem.md`（三種語言）。
- MCP Marketplace 新增 Zapier 與 Composio 兩張遠端卡片（`McpCatalogItem.remote`：預設端點、建議登入方式、說明連結、第三方提示），端點取自官方 MCP 註冊表，按「連線」走上面的遠端連線流程；介面提醒資料會經過第三方，只開啟員工需要的動作。`marketplace.install` 對遠端卡片會拒絕並指向 `mcp.remote_connect`。沒有用真的 Zapier／Composio 帳號測試。

### Changed
- 匯入（`mcp.import.fetch`／`.mcp.json` 的 `url` 項目／註冊表 `server.json` 的 `remotes[]`）遇到遠端伺服器時，不再產生 `npx -y mcp-remote <url>`，改成原生橋接的候選項（`duduclaw mcp-remote-bridge --url <url>`），安裝時把網址記到加密的遠端伺服器紀錄並寫入不含網址的橋接項目，之後由管理者連線。`mcp-remote` 會在 Gateway 主機開瀏覽器登入、把權杖明文存在 `~/.mcp-auth`，無螢幕的 Gateway 與 DuDuClaw OS 無法完成。唯一保留的是 `"type": "sse"`（舊版 HTTP+SSE 協定，原生橋接不支援），說明文字會標明。`mcp_manifest_tests.rs` 的對應預期一併更新。從員工伺服器清單移除遠端伺服器時，也會刪除它保存的登入資訊。
- 儲存庫預設的 README 改成英文，讓從 Hacker News、Reddit 與 awesome 清單來的訪客先看到英文：原本的繁體中文 README 改名為 `README.zh-TW.md`，原本的 `README.en.md` 改名為 `README.md`。三個語言版本的語言切換列、`SECURITY.md` 的連結與 `scripts/release.sh` 的版號徽章清單一併更新；三個 README 首屏在徽章下方加上兩行快速開始指令，MCP 工具數改為 247（npm 與 PyPI 套件描述原本分別寫 249 與 243，也一併改為 247），Computer Use 頻道核准的說明移到「信任與安全」一節。
- 儀表板的升級、續期與商用連結改指向授權總經銷 未來企業（https://www.futurecorp.tw/），文案不再提訂閱方案；已啟用白標且設有經銷商網站與公司名稱時，連結與文案改用白標品牌的值。原本指向 `duduclaw.dudustudio.monster#pricing` 的四處入口（授權到期橫幅、授權等級提示橫幅、AI 員工頁的成長提示、授權頁）與 zh-TW／en／ja-JP 的相關字串一併更新。
- `tools/list` 對「不持有 `admin`、也不是 AI 員工」的金鑰只列它的 scope 叫得動的工具（儀表板 `mcp_keys.create` 建立、不帶 `admin` 也不對應員工的內部金鑰也在其中，清單因此變短），並且不列替行程本身的 agent 動作的工具（`working_state_*`、`memory_search_by_layer`、`memory_successful_conversations`、`memory_episodic_pressure`、`memory_consolidation_status`、`shared_wiki_delete`、`wiki_namespace_status`、`canvas_push`、`canvas_clear`、`team_handoff`、`mail_*`、`office_script`；對這類呼叫者它們會回 `unknown agent`、讀到預設員工的資料，或在預設員工的畫布上作畫）。gateway 內部金鑰、單一員工金鑰、gateway 為員工啟動的行程，以及持有 `admin` 的金鑰，清單不變（腳手架員工仍是 167 個）。外部金鑰也適用：沒有 scope 的外部金鑰原本會列出 7 個舊白名單工具，每個呼叫都被 scope 檢查拒絕，現在清單是空的。
- gateway 開機時只替有 `agent.toml` 的員工目錄建立／修正 `.mcp.json`（`ensure_mcp_absolute_paths_all`）；外部金鑰的 wiki 目錄 `agents/<client_id>/` 不再被寫入一份帶內部金鑰的 `.mcp.json`。即將啟動員工前的修正（`refresh_for_spawn`）不變。
- `duduclaw mcp-server` 沒有 `DUDUCLAW_MCP_API_KEY` 時的錯誤訊息多一句 `Run: duduclaw mcp init --client claude-code`。
- **Gemini CLI runtime 的移除時間由 v1.71.0 再延到 v1.72.0**（runtime id `gemini`，仍是棄用狀態，行為不變）：v1.69.1 修正後用真的 Gemini 金鑰重驗 Antigravity 的步驟還沒做，移除等重驗完成。`duduclaw doctor` 與 `runtime.detect` 顯示的移除版本同步改為 v1.72.0，三種語言的文件一併更新。

### Security
- 替行程 agent 動作的工具（見上方 Changed 第一項的清單）改由 dispatch 閘依清單同一條規則拒絕（`-32003`，稽核 `error_class` = `process_agent_tool`）：對象是清單規則適用的金鑰，以及所有外部金鑰（帶 `admin` 的也算）。已發佈版本受影響：自外部金鑰可授予 scope 起，帶 `memory:read`／`memory:write`／`wiki:write` 的外部金鑰（OAuth 用戶端也是）就能直接呼叫這些工具，讀到預設員工的記憶、改寫它的 working state、在它的畫布上作畫。
- 外部金鑰用 `wiki_write` 帶 `scope="shared"` 一律拒絕（`-32003`，稽核 `external_shared_wiki_write`）：共享 wiki 的寫入會以行程的預設員工判斷部門、`.scope.toml` 權限與作者。外部金鑰的共享 wiki 讀取（`wiki_ls`／`_read`／`_search`／`_stats`／`_lint`）改用「沒有部門」的身分，只看得到所有呼叫者都能看的頁面；之前會帶著預設員工的部門可見範圍（已發佈版本受影響）。`wiki_share` 不變（以金鑰自己的 client id 寫入）。

## [1.70.1] - 2026-10-07 — 1.70.1 修補：discovery 鎖、GDPR 歸檔、AI 員工任務可見名單、操作者指令列核准閘統一

### Fixed
- Discovery 的 `discovery.db` 開啟時會另外開一個檔案 handle 比對身分再關掉；在 POSIX 上關掉任何一個 handle 會讓本行程在該檔的所有鎖失效，同一行程裡先開的 Discovery store 可能失去 SQLite 鎖，另一個行程就可能在它底下 checkpoint 並移除 WAL。改成開啟前後各用 `lstat` 比對檔案身分，不再多開 handle（與 1.70.0 修好的核准、工作流程與 LINE 收件資料庫同一種修法）。
- `duduclaw gdpr erase` 不會刪記憶衰減與 `forget` 移進 `memories_archive` 的副本，同一個人的資料留在歸檔表裡。現在 erase 在同一筆交易裡一併刪除（歸檔副本只留文字，所以用文字提及比對，另外也刪掉與被刪記憶同 id 的殘留副本），報告與指令列輸出多了歸檔筆數，`export gdpr` 也會列出歸檔副本。試跑（不帶 `--confirm`）與 `export gdpr` 的歸檔筆數只算文字提及這個人的歸檔列，實際刪除時另外刪掉與被刪記憶同 id 的副本，所以刪除後的筆數可能比試跑多。歸檔表只保存文字，只靠 subject／object 連到這個人、文字沒提到他的歸檔列找不到。
- `duduclaw gdpr export|erase`（與 `export gdpr`）接受空白或很短的聯絡人：空字串會變成 `%%`，erase 會刪掉這位 AI 員工的全部記憶（現在也包括歸檔表）。現在聯絡人去掉前後空白後必須至少 3 個字元（以字元計，不是位元組），否則在開啟任何資料庫、印出試跑摘要之前就拒絕並說明規則；比對使用去掉前後空白後的值。
- MCP `tasks_list`、`activity_list` 與提示裡的任務板原本不看任務的可見名單（1.70.0 的已知限制）。現在 AI 員工呼叫時，被名單限制的任務只列給它的負責人、認領者、建立者，以及名單用 `role:<員工 id>` 點名的員工（儀表板角色名稱 admin／manager／employee 不算員工名稱）；交接資料無法讀取的任務只列給所屬員工；`activity_list` 依同一規則略過這些任務的動態（員工自己寫的那幾筆照列）。操作者的呼叫不受影響。提示裡的任務板本來就只列員工自己負責的任務，所以實際上不會少列。
- 工作流程（P1-B）程式碼裡五處沒用到的 import 造成的編譯警告已移除。

### Changed（指令列核准閘統一）
- 四個操作者指令（`ops channel-ingress`、`responsibility`、`memory forget-source apply`、`ops computer-workspaces`）的儀表板核准閘改用同一份實作 `approval/operator_cli_gate.rs` 與同一張種類表；`approvals.decide` 的四段 Admin 檢查、儀表板專用種類、逾期文字、提醒排除與推播上限都從這張表讀。
- 請求等待期間對象狀態變了，等待中的卡片一律撤回（`state_changed`）並另建一筆；電腦操作工作區原本會原地改寫卡片內容，管理員可能核准到打開後被改寫過的內容。
- 持續任務的請求如果是對舊狀態送出的，再執行同一個指令時會撤回（`state_changed`），不再占用每個對象 3 筆的待決名額；已核准但狀態變了的請求同樣撤回，不會被套用。
- 兩次執行同時搶同一筆核准時，搶輸的那次不再另送一筆新請求。
- 依來源忘記的核准現在只認儀表板上的決定，也加上每種類 20 筆的待決上限與每位 AI 員工每小時 2 次的推播上限；LINE 收件匣加上每個事件 3 筆的待決上限與每小時 2 次的推播上限（稽核 `channel_ingress_approval_push_suppressed`）。
- `ops computer-workspaces fence` 的 `--reason` 納入核准綁定，換理由要重新核准。
- `duduclaw responsibility` 在 AI 員工工作階段中的判定改用與 `memory forget-source` 相同的變數清單（原本只看身分與權杖兩個變數，空值不算）；LINE 收件匣與電腦操作工作區的指令改用同一份清單，該清單多了 `DUDUCLAW_MCP_API_KEY`、`DUDUCLAW_DATA_FILE_GUARD`。
- 儀表板待辦清單為三個指令列核准種類補上說明（三種語言）。
- AI 員工工作階段的變數清單多了 `DUDUCLAW_TASK_ID`（gateway 在目標回合設定的任務編號）；LINE 收件匣與電腦操作工作區的指令改用 `var_os` 讀取，值不是 UTF-8 時也算有設定（原本會當成沒設定而略過檢查）。
- 兩次執行同時處理同一筆核准時，被另一次用掉或作廢（例如 `state_changed`）的那次，訊息改為「已被另一次執行用掉或作廢」。
- **升級須知**：新的閘把綁定資訊存在請求內容的 `gate` 物件裡，1.70.0 以前建立的請求沒有這個物件，新版本不會比對到也不會計數。升級前送出的請求，就算已經核准、還沒套用，升級後也不會被套用；再執行一次指令會另建一筆，要重新核准。舊的等待中卡片在過期前照常收到提醒推播（持續任務的請求本來就不提醒），但核准它不會有任何作用。

## [1.70.0] - 2026-10-06 — 代理平台 P0–P2：耐久收件與通道決定、可審查成果與有界工作流程、持續任務、記憶來源遺忘、電腦操作工作區

這一批的功能尚未在真實的 gateway 與通道上做過活體驗證。

### Fixed
- Windows：`duduclaw doctor` 的「單一 gateway」列讀不到持有者（`LockFileEx` 鎖住的檔案其他 handle 不能讀），被拒的第二個閘道也報不出是誰持有。持有者那行改寫在旁檔 `<home>/locks/gateway.lock.holder`，鎖檔本身保持空白；macOS／Linux 行為不變。
- 審查快照只能接受任務最新的一份（畫面停在舊快照時被拒），沒收齊產物的快照不能接受也不能建立草稿；接受紀錄不可刪除。擷取、接受、試跑、送審、套用、撤銷與取消執行都需要對該 AI 員工有 Operator 以上的綁定。
- 五種試跑的負面案例要真的帶著它所指的情況並以該關卡自己的錯誤碼結束（過期、注入、缺少權限各有固定的錯誤碼）；格式錯誤、人工拒絕或案例自帶的人工決定不再算通過。
- 產物列表的證據種類只在快照重新驗證通過時顯示，種類取自儲存的快照，最新快照優先；不再回傳產物的絕對路徑。
- 儀表板排程的「立即執行」每次按下產生 request ID、重試沿用，綁定工作流程的排程不再被拒，成功後顯示實際狀態。工作流程啟用狀態「已暫停」有文案、變動類別與恢復方式。
- 工作流程成本有了真正的帳本：每次步驟派送在寫入「執行中」檢查點的同一筆交易裡記帳（讀取每次嘗試、核准與寫入步驟各一次），單次與每月上限都從帳本判斷，寫入操作沒有預扣就無法被認領。單價由操作者在 `config.toml [workflow.unit_cost_micros]` 設定（預設 0，是估價而非帳單），全部為 0 時 `workflow_runs.get` 明示 `money_limits_effective = false`；讀不出的單價按上限計。用完額度或次數以 `blocked`（失敗類別 `limit`）結束，不計入連續失敗。
- 寫入操作的對象在啟用時固定：每種寫入工具指定一個對象參數（`tasks_update`→`task_id`、`update_cron_task`→`id`），效果範本必須寫明且步驟輸入必須由定義本身產生同一個 id；來自讀取結果或執行輸入的 id 無法啟用，表外工具不能當寫入操作，`update_cron_task` 不接受依名稱選取。
- 在儀表板決定綁定卡片需要可存取該 AI 員工，工作流程步驟卡片還須在該執行的可見對象內；`approvals.list` 以即時角色依員工綁定過濾，綁定細節與提問答案只給能決定的人。
- 工作流程啟用只能由管理員在儀表板決定，通道按鈕與回覆一律無效，決定時重讀角色；可自己核准自己送出的申請，卡片與稽核標明「送審者＝核准者」。共用的通道拒絕文字不再寫成「知識審核」。
- 已啟用排程的手動執行必須帶 request ID：`cron.run_now` 缺少時拒絕並回傳 `run_id` 與實際狀態，MCP `run_cron_task` 等無法提供 request ID 的路徑一律拒絕，不再每次重試另開一個執行。
- LINE webhook 驗簽後先持久化整批事件才回 200；穩定事件去重、同會話排序與有界 worker 補齊已接受事件的重啟復原。派工前與交付前重驗帳號、路由與授權；換綁或憑證輪替隔離舊 backlog。未知 dispatch 不盲重送，原 uncertain 回執保留。回 200 前只做驗簽與寫入需要的檢查，無關員工設定壞掉不再讓 webhook 回 503；路由與授權快照只看被路由的員工（含 preset 生效設定）、LINE 憑證與該對話的設定。設定讀不到時退避重試，連續 5 次才隔離。進度推播失敗不再改變事件狀態。續租遇到暫時性資料庫錯誤時在租約內重試。
- 已啟用的工作流程停在核准或提問步驟後，決定（核准、拒絕、回答）會在同一個交易排入同一個 run 的恢復；逾期由常駐巡檢（開機一次、之後每 60 秒）發現。拒絕結束為 `failed`／`workflow_approval_denied`，逾期為 `workflow_approval_expired`。每個步驟最多一張可決定的卡。等待上限改為 24 小時（或授權到期，取早者），每次派送另有 15 分鐘執行期限。
- Gateway 重啟會釋放舊行程的 run 租約與已取走未結的佇列訊息並繼續執行；effect 已開始的步驟改顯示 `uncertain`，不重送。執行中背景續約；「已在執行」與基礎設施錯誤延後重試，不再把佇列訊息標成失敗，重試用盡時封鎖為 `workflow_transient_retry_exhausted` 且不計入連續失敗。一筆壞的交接列不再讓其他觸發失敗。工作流程派送改在獨立 task 執行，不再擋住其他員工的佇列訊息。
- 聊天通道上的核准回覆只在「動詞＋完整請求編號」時才當成決定。開頭是「確認／取消／回答／approve／deny／answer」但沒有合法編號的一般訊息（例如員工問「要送出嗎」後回的「確認」）照常送給員工，bot 不再回「請提供完整的請求編號」；回覆舊決定卡片的「取消」「approve」「deny」等文字裁決恢復由卡片原本的流程處理。帶完整編號的指令容許多個空白、全形空白、句尾標點與大寫或無連字號的編號寫法，照樣當成決定，不會送給員工。Telegram 與 Slack 不在允許清單的群組裡，一般訊息也恢復走原本的流程。
- 頻道允許清單恢復 v1.69.1 的判斷：設了 `allowed_users` 且沒開配對時，發送者與對話兩者都要在清單內才放行。這一輪未發布的變更曾改成「任一在清單內就放行」（把群組對話放進清單會讓群內所有人通過），已撤回；決定的檢查只會比一般訊息更嚴。
- Discord 的決定訊息與決定按鈕改用獨立的處理名額，不再與一般回覆共用十個名額，忙碌時確認不會排到逾期。要先通過通道存取檢查才拿得到這種名額，決定訊息不下載附件，一般訊息的附件下載最多 30 秒。
- Telegram 群組裡 `@bot 確認 <編號>` 形式的確認改在接收端處理，不再排在它要解除的電腦操作後面；只去掉開頭、指向這個 bot 的 mention，不分大小寫。
- `~/.duduclaw/threat_level` 只有檔案不存在才視為 GREEN；檔案存在卻讀不到、是空的、或內容不是 GREEN／YELLOW／RED 時，間隔 50 毫秒再讀兩次，仍然如此才視為 RED。開頭的 UTF-8 BOM 與前後空白會略過。
- 排程啟用（含重新啟用）從當下開始，不補跑停用期間的時段；超過 24 小時回補窗口的時段寫入跳過紀錄並顯示在排程狀態。找最近時段不再逐秒掃描。
- 暫停的工作流程啟用不會再以任何方式回到啟用中：`commit_activation` 拒絕已暫停與已到期的啟用，儲存層禁止離開這兩個狀態（只能往撤銷走），暫停時核准紀錄裡的授權一併撤銷。
- 開機時的租約清理與重排、工作流程派送與背景巡檢只由持有 `<home>/locks/gateway.lock` 的閘道執行；同一資料目錄的第二個閘道記錄錯誤並不碰工作流程。
- 寫入操作開始前遇到的基礎設施錯誤（另一個程序占著操作、資料庫忙碌）改為暫時性錯誤重試，不再當成被拒絕、也不計入連續失敗。
- 資料時效只看實際讀取時間：輸入寫死在定義裡的寫入操作不受執行輸入時效限制，人花很久才核准不再讓它以「輸入過期」失敗；資料過期的封鎖改列為獨立類別 `stale`，不計入連續失敗。
- 權限比對看到雜湊不同時先重讀確認，兩次讀到一致且解析成功的不同內容才暫停；設定讀不到改為暫時性錯誤，重試用盡仍讀不到才暫停。`[redaction]` 與自訂去識別化規則納入比對。
- 步驟卡片逾期而執行期限同時到了時，結局是 `failed`／`workflow_approval_expired`，不再是計入斷路器的 `workflow_deadline_expired`。
- 每種只能在儀表板決定的卡片過期時各有自己的文字（知識審核、工作流程啟用、收件處理指令），其他種類用通用文字；LINE 指令列卡片過期不再顯示「知識審核」。
- 決定與恢復佇列在 WAL 下跨資料庫不保證整體原子，文件改為「同一交易，當機時由巡檢補齊」。巡檢的佇列前綴查詢改走主鍵範圍，未投遞的恢復佇列加部分索引；`WORKFLOW_BUSY` 的延後重試不再累加重試次數。
- **已取消的 goal 任務不會再被翻回「等人處理」**。兩個把任務轉人工的寫入原本只比對任務 id、不看狀態，一個讀到舊資料的巡檢可以把已取消的任務改回 needs_human，之後有人按重試它就復活了。現在這兩個寫入跳過已完成、已取消、已失敗的任務，對所有任務生效。

- LINE 收件匣：路由與授權快照補上之前，worker 不會領取該事件；讀不到快照時退避重試，連續 5 次或沒有快照的事件超過 5 分鐘就隔離成 `snapshot_unavailable`（可 `retry`，重試時以當下設定補快照），不再默默採用之後的設定。LINE 重送的 webhook（`deliveryContext.isRedelivery`）不再拿 reply token 去試，直接照 `line_late_reply` 處理；回覆期限從本機收件時間與事件 `timestamp` 較早者起算。Reply API 回 400 `Invalid reply token` 時，設為 `"push"` 重驗後改用 Push，設為 `"fail"` 記 `reply_token_invalid`。送出前重驗暫時讀不到時，約七秒內重讀數次，仍讀不到記 `revalidation_unavailable`，不再記成授權已變更。路由員工的 `agent.toml` 不存在視為已變更（`agent_removed`），不再退避。憑證讀不到而隔離時也會告警。裝置還原的標記改在搬動任何資料前寫入，寫不進去就中止還原；還原時 `failed_before_dispatch` 與可重試原因的 `quarantined` 也一併暫停。指令列等待中的核准卡在事件狀態變動時撤回重建，不再原地改寫。Bash 通道比對 `duduclaw ops channel-ingress` 改用 org_field_guard 的 shell 解析（換行續行、引號拼接、黏字重導向），比對函式抽成共用的 `duduclaw_core::bash_operator_command_decision`。

### Changed
- **Gemini CLI runtime 的移除時間由 v1.70.0 再延到 v1.71.0**（runtime id `gemini`，仍是棄用狀態，行為不變）：v1.69.1 修正後用真的 Gemini 金鑰重驗 Antigravity 的步驟還沒做，移除等重驗完成。`duduclaw doctor` 與 `runtime.detect` 顯示的移除版本同步改為 v1.71.0。
- 工作流程草稿不再要求停止條件（沒有任何地方執行它）；介面把「需要的工具」標明為只對照工具清單的上限估計。審查與草稿相關 RPC 的錯誤改回傳封閉代碼 `{code, message}`，不再回傳伺服器內部文字。移除沒有呼叫端的對話訊息草稿卡片。
- `policy_revision` 改為只雜湊授權相關的已解析欄位（實際生效的 `[capabilities]`／`[permissions]`／`[agent]` 上級部門角色、`CONTRACT.toml`、職務範本綁定、`org.toml` 本人與上級、`config.toml` 的 `[delegation]`／`[acp]`／`[provenance]`／`[integrations]`、`KILLSWITCH.toml`）；改通道 token、即時監控來源或記錄等級不再讓待審卡片與已啟用工作流程失效。升級後所有既有 revision 值都會變一次：待審的綁定卡片需重新送出，已啟用的工作流程第一次執行時會進入暫停，需以新版本重新核准。
- 已啟用工作流程遇到上述權限變動時，啟用進入 `suspended`：當次執行擋下、不再開新執行、排程關閉，動態牆與管理員通知列出變動類別；恢復需新版本重新核准。啟用後來源任務與產物的變更不再擋下執行。
- 工作流程啟用的有效期不再綁在試跑證據的時效上：試跑只需在管理員核准當下仍有效，核准後啟用有效 `config.toml [workflow] activation_days` 天（預設 30，範圍 1–365，無效值拒絕送審），到期日顯示在核准卡與草稿頁。到期前三天通知管理員一次；到期時進入 `expired`、排程關閉、寫動態牆並通知管理員。不能延長，續用需新版本重新送審。
- 啟用時用定義本身比對次數上限與單次額度（步驟、讀取、寫入數，以及以目前單價計算最便宜一輪的費用），超過就無法啟用；之後上限調低到放不下，下一次觸發即拒絕並以 `limit` 暫停。連續 `max_consecutive_failures` 次因上限結束也會暫停並通知管理員。
- 單價全為 0 時，草稿頁與啟用核准卡改說明目前生效的是次數上限，不再顯示費用上限（三種語言）。
- LINE 回覆 token 逾期時依 `[channel_ingress] line_late_reply` 處理：`"push"`（預設，與舊版相同）在重驗後改用 Push 送給同一個對話並寫入回執；`"fail"` 在取件時就判定逾期、不執行回合並通知管理者。一般 worker 數改為 `line_workers`（預設 8）。收件狀態 `failed` 拆成 `failed_before_dispatch`（確定沒執行，可 `retry`）與 `undelivered`（已執行未送達）；`undelivered` 與 `uncertain` 只能用 `rerun` 並確認重複風險，確認、理由與 provider 回執一起保存；升級時舊的 `failed` 改為 `undelivered`。📎DELIVER 的文件通知附在回覆裡，走同一條重驗與回執。`line_enabled` 改稱停用開關（關閉即 LINE 停擺，不會回到舊版收件方式）。未配置憑證、停用或儲存失敗回 503。設為 `"fail"` 時，已超過原回覆期限的事件不能 `retry`／`rerun`（回覆 token 不會因重新執行而延長）。裝置還原（`device.backup_restore`）後，收件匣在任何 worker 取件前把備份裡排隊中的事件改成 `quarantined`／`restored_from_backup`，寫 Activity Feed 並通知管理者，由操作者結案或確認重複風險後 `rerun`。AI 員工在 Bash 執行 `duduclaw ops channel-ingress` 會被 agent-file-guard 擋下（`BlockedOperatorCommand`，減速而非隔離）。詳見 [LINE 收件復原](docs/guides/durable-line-ingress.md)（[繁體中文](docs/guides/zh-TW/durable-line-ingress.md)／[日本語](docs/guides/ja-JP/durable-line-ingress.md)）。
- **派工前重新確認任務沒有被停止**：每一輪 goal 任務在派出前，都會再查一次任務與它的上層任務是否已被停止，已停止就不派（包含一般 goal 任務）。被停止的任務不能再用 `tasks.update`、`tasks_update` 改回其他狀態，也不能「接著做」；在被停止的任務底下用 `tasks_create` 開子任務會被拒絕。
- **`activity_post` 拒絕系統保留的事件類型**：`responsibility.` 與 `task.stop` 開頭的動態只有閘道能寫，AI 員工貼這類動態會被拒絕，所以持續任務的推播次數與停止紀錄不會被偽造。

- LINE 收件匣告警改為先記進收件匣資料庫的佇列，每 30 秒依種類、原因與十分鐘時間窗彙總成一筆帶數量的 Activity Feed；時間窗與推播節流存在資料庫裡，重啟不重複也不歸零。告警文字改指向指令列與儀表板的待辦核准（儀表板目前沒有收件匣頁面）。`undelivered` 與 `failed_before_dispatch` 超過 `retention_days` 由系統結案（`retention_closed`）並刪除。指令列 `list`／`show` 的 LINE 帳號與對話 ID 改印短摘要。`line_late_reply` 為非字串時記警告。
- 衍生寫入若指名的父記憶不存在，現在會拒絕這次寫入；以前只是把這筆寫入的信任度壓低。
- 匯入記憶的來源鍵改為內容定址：檔案以解析後的路徑識別，每筆紀錄以內容雜湊識別。同一個檔案換順序、重新匯入，仍算同一個來源，所以被忘記的檔案不會因為重新匯入而回來；放在另一個路徑的副本視為新來源。
- 員工程序從 gateway 收到的回合或執行資訊格式不合法時，記憶工具現在會拒絕寫入，不再當成匿名的外部呼叫記下來。
- 記憶寫入改在單一交易內完成：記憶列與它的來源列一起寫入或一起不寫。
- `duduclaw memory migrate-namespace assign` 搬移記憶列時，如果列的來源已在目標命名空間被忘記，這一列留在原處不搬，輸出會標示原因。

### Added
- `TypedSchema` 新增 `nullable`，回執資料中可為空的欄位寫得出正確的輸出結構。
- 試跑案例可依步驟預寫人工決定（`approve`／`deny`／`answer`）：不建卡、不推播，只對試跑有效；缺少決定以 `fixture_decision_missing` 失敗。
- CI 新增工作流程端到端與 stdio 檢查點測試步驟（ubuntu），並新增一個走真正 runner 的撤銷競態測試。
- 工作流程啟用卡與步驟卡會發不含內容的通知：啟用卡送給管理員已驗證的聊天，步驟卡送給能決定的人（經理或管理員、對該員工有操作權限、在執行可見對象內）；沒有人綁定聊天的部署只會在收件匣看到。`duduclaw doctor` 新增「單一 gateway」列。
- LINE 收件匣的可見性與操作入口：事件進入 `uncertain`／`quarantined`／`undelivered`、逾期未執行或對話卡住超過 `stuck_alert_minutes`（預設 15）時寫 Activity Feed 並推播給管理者；relay 轉來但沒收下的 LINE webhook 計入 `relay_frames_total{outcome="not_accepted"}` 並寫 Activity Feed。隱藏指令 `duduclaw ops channel-ingress {list,show,resolve,rerun}`，會改變狀態的動作只建立儀表板核准請求，Admin 核准後才套用。已結束事件與其紀錄保留 `retention_days`（預設 90）天後刪除，容量超過 `capacity_alert_mb` 時告警。
- `workflow_runs.get`／`list`（run 與每個步驟的狀態、錯誤碼、核准卡與 operation 狀態）、`workflow_runs.cancel`（停止單一 run 並撤回待決卡）、`workflow_runs.reset_failures`（Admin 解除連續失敗鎖定，不需重新啟用）。
- **電腦操作工作區**：AI 員工的檔案可以在電腦操作 session 結束後留下來。`computer_session_start` 帶 `workspace`（`"new"` 或既有的 `ws-…`）掛上一個由 gateway 保管的工作區；新工具 `computer_workspace_list`／`computer_workspace_read`／`computer_workspace_write`（只能寫進自己這次 session 掛上的工作區，單檔 48 KiB 的 UTF-8 文字，可帶 `expected_revision`）。容器裡在 `/workspace/files` 唯讀，放在只有 root 能進的 tmpfs 底下，瀏覽器帳號讀不到。預設關閉：`config.toml [computer_use.workspaces] enabled` 加上員工的 `[capabilities.computer_use_config] workspace`；可設配額、保留期限（到期不刪檔、不佔名額）與磁碟下限。同一個工作區同時只有一個 session（租約 90 秒；gateway 在 session 啟動途中當掉，最多約 8.5 分鐘不能再掛）。工作區綁定建立它的那一位員工（隨機憑據），同名重建的員工拿不到。操作者指令 `duduclaw ops computer-workspaces`：除了 `list`，指令列上所有會改變狀態的動作（`fence`／`revoke`／`regrant`／`renew`／`delete`）都要先由管理員在儀表板核准（核准 30 分鐘內、狀態未變才有效，只能用一次），每次提出、套用與拒絕都寫安全稽核；緊急處置用儀表板或總開關。`duduclaw doctor` 新增「電腦操作工作區」一列。只支援 macOS 與 Linux。只有持有資料目錄實例鎖的 gateway 會整理工作區登錄並移除過期的工作區容器；同一資料目錄上的第二個 gateway 只服務並續約自己的 session，不做登錄維護。詳見[電腦操作工作區](docs/guides/computer-workspaces.md)。

- **持續任務**（`config.toml [responsibilities] enabled`，預設關）：讓一位 AI 員工在一段期間內依排程（cron 加時區）、事件（`task.created`／`task.updated`，由 MCP 任務工具寫入；儀表板上的變更不產生事件）或操作者的回答反覆醒來，每次建立一個有期限、迭代上限與花費上限的 goal 任務，照一般流程驗收。兩次之間沒有任務存在，不佔執行名額、不呼叫模型。上限包含每次與每期（日／週／月）的花費、每期次數、兩次間隔、連續失敗自動暫停與必填的結束時間；花費計入那一次執行底下記錄的子任務：員工在持續任務某一輪建立的任務由系統依正在執行的那一輪掛到它底下，模型指定的上層任務必須在那一輪的任務樹內，否則拒絕；輪次資訊存在但空白或格式不對也拒絕。一般 goal 輪次與任務看板喚醒照舊，不預設上層任務。每個任務最多 200 個尚未結束的子任務（排程與提醒不算）、最多 64 層。子任務由任務看板喚醒時的花費算在該子任務上。派工端在啟動 runtime 之前寫下這一輪已開始的紀錄；有這筆紀錄但量不到花費的一輪以單次上限全額計（之後就算失敗、或閘道當機後訊息退回佇列也一樣），沒有 token 數的用量紀錄也算量不到；沒有這筆紀錄的一輪（派工前被擋下、開始前被拒、仍在佇列中，重啟後也一樣）不計花費，所以暫停再恢復不會把上限吃掉。員工或驗收判官使用可能不回報用量的 runtime（Antigravity、Gemini CLI）時，一次執行通常只跑得了一輪；建立持續任務時會提示，`duduclaw doctor` 也會列出。每一輪派出前也會檢查員工自己的月預算。單次上限是在兩輪之間檢查，進行中的那一輪和同時在跑的子任務可能讓花費超過上限。事件只認訂閱生效之後發生的、屬於這位員工的事件，員工自己製造的事件不會喚醒自己，每期另有事件喚醒次數上限（`max_event_wakes_per_period`，預設 12）。通知預設關，要持續任務的通知設定、員工的 `[proactive] enabled`、每期推播上限與主動通知閘四關都過才推，內容只有名稱、狀態與連結。需要派工引擎開啟。說明見 [持續任務、任務中送指示與停止](docs/guides/continuous-responsibilities.md)。
- **`duduclaw responsibility` 指令列**：`list`、`get`、`occurrences`、`fires` 查詢；`create`、`update-contract`、`pause`、`resume`、`disable`、`enable`、`clear-failures`、`stop` 每一個都只建立一筆核准請求，只能由管理員在儀表板決定，核准後 30 分鐘內（`[responsibilities] operator_approval_minutes`，1–1440）再執行同一個指令才套用一次。重新啟用不會把連續失敗次數歸零，只有清除失敗（儀表板要 Manager、指令列要管理員核准）會。核准綁定動作、對象、內容與對象當下的狀態；相同請求合併，同一動作同一對象最多 3 筆不同內容的請求等待中，同一對象每小時最多推播 2 次，這類請求不發提醒。`create` 與 `update-contract` 的合約先檢查，不合格的不會建立請求。核准卡片由伺服器產生，列出 AI 員工、動作、上限與喚醒條件，工作內容以引用資料顯示並截短。每次提出、套用、拒絕各寫一筆安全稽核（`responsibility_cli_requested`／`_applied`／`_refused`）。所有子指令在 AI 員工的工作階段中都拒絕執行；功能關閉時會放寬權限或花費的動作直接拒絕。同一資料目錄只有持有 `<home>/locks/gateway.lock` 的閘道會執行持續任務的喚醒、停止對帳、指示清理與耐久派工修補；其他閘道照舊派送 goal 輪次。儀表板的持續任務、指示與停止 RPC 每次重讀帳號身分，指示與停止也經過任務可見名單。某位員工的任務看板連續 3 次讀取失敗時寫一筆 Activity Feed。
- **任務中送指示**（`[goal_loop] steering_enabled`，預設關）：goal 任務詳情頁可以寫下一輪的指示（每則最多 4000 字，每個任務最多 10 則等待中）。只有那一輪真的派出去才算送達（頁面顯示「已排進第 N 輪」），開始前被擋下或取消時退回等下一輪，已經開始的一輪之後失敗也算送達；指示不改驗收標準，判官看不到；內容會做注入掃描，命中時頁面標示並提醒員工只當參考。帶指示的那一輪由員工單人執行。
- **停止任務**：任務詳情頁的「停止任務」立即取消任務與所有子任務（大的任務樹分批處理，期間樹底下不能新建、認領或完成任務，派工引擎與心跳的任務看板喚醒都不會派出樹內任務，佇列中等待的心跳喚醒一併取消），佇列裡等待的那一輪取消、未決核准作廢、尚未開始的外部動作不再執行。已經在跑的那一輪會先跑完，狀態依序顯示 `cancel_pending`、`stopped`，有無法確認的項目時是 `stopped_uncertain`。確認框記下打開時的任務版本，任務變動後要重新確認。已發出的委派不跟著停。
- **三個 MCP 工具**：`responsibility_get`、`responsibility_followup`（在自己的那一次執行中安排一次後續喚醒，計入每期次數）、`responsibility_ask`（問操作者一個問題，經注入掃描與長度上限，答案只當資料；通知只有名稱與連結、不發提醒，實際上在儀表板收件匣回答）；功能關閉時不列出。工具總數 241 → 244。說明見 [MCP 工具](docs/guides/mcp-tools.md)。
- **儀表板 RPC**：`responsibilities.create`／`list`／`get`／`occurrences`／`fires`／`update_contract`／`pause`／`resume`／`disable`／`enable`／`clear_failures`（`clear_failures` 需要 Manager）、`tasks.steer`／`tasks.steering`／`tasks.stop`／`tasks.stop_status`；`system.update_config` 接受 `responsibilities.enabled` 與 `goal_loop.steering_enabled`。儀表板還沒有 `/responsibilities` 頁面。
- 頻道請求區分核准與問題，完整 ID 回覆保存決定或答案；問題不授權工具。新增持久操作租約、fence、receipt 與 Admin 未知結果核對入口。
- `duduclaw ops channel-ingress batch`：一次核准處理同一狀態（可加原因代碼）的一批 LINE 事件，最多 500 則；核准綁定每則事件的編號與狀態版本，套用時只處理狀態沒變的事件，並列出略過的。詳見 [LINE 收件復原](docs/guides/durable-line-ingress.md)。
- **依來源忘記記憶**（`duduclaw memory forget-source`，只能在操作者的終端機執行）。操作者可以忘掉一個來源（一則訊息、一整段對話、一次排程或派工執行、一個匯入檔案）：硬刪由它直接或間接產生的記憶、關鍵事實與封存副本，寫下封鎖紀錄，之後同一個來源再寫入會被擋下，並記成稽核事件 `memory_write_fenced`。子指令有 `list`（列出來源）、`plan`（預演，不刪任何東西）、`show`、`apply --confirm` 與 `resume`。`plan` 會列出會刪的項目、連帶影響（一筆記憶若同時由被忘的來源與其他來源產生，整筆刪除；只是被再次提到的記憶保留）、需要人工檢視的 wiki 頁，以及不在範圍內的清單。`apply` 之後會續做 `memory.db` 以外的步驟（刪自動建檔頁、撤回審核卡、對員工隱藏被忘的訊息、清除對話的壓縮摘要）；沒做完時結果是 `DEGRADED`、結束碼 3，用 `resume` 重跑，gateway 開機時與之後每 10 分鐘也會重試。忘記一則使用者訊息時，同一輪的員工回覆與員工在那一輪自行存入的記憶一起忘記，之後只帶那一輪回合代號的寫入也會被擋；忘記整段對話則涵蓋其中每一輪。派工時 bus 訊息只帶了一半上游對話身分，員工的記憶照常寫入，上游記成不明，計入計畫的「沒有完整來源紀錄的記憶」。每次回覆後寫下的強化學習軌跡檔（`rl_trajectories.jsonl` 與 `rl_trajectories/`）含整段對話原文，不在範圍內。`[memory] forget_source = false` 可停止建立新計畫與套用。操作步驟見 `docs/guides/memory-and-knowledge.md` 的 4.5 節。
- **記憶帶來源（來源譜系）**。每一筆寫進 `memories` 與 `key_facts` 的記憶，都在同一個交易內記下來源：通道訊息、排程或派工執行、員工的 MCP 回合、外部 MCP 用戶端的呼叫、匯入檔案的一筆紀錄、足跡日，或系統。由其他記憶衍生的記憶會帶著所有父記憶的來源。升級前寫入的記憶大多沒有來源紀錄，計畫會顯示它們的數量，但不會刪。
- **忘記需要管理員在儀表板核准**。`plan` 會送出一筆核准請求，綁定計畫 id 與計畫雜湊；只有管理員能在儀表板決定，通道按鈕與回覆一律拒絕。`apply --confirm` 只有在核准有效、計畫未過期時才執行，核准期限等於計畫期限（預設 30 分鐘，最長 24 小時）。卡片只顯示數量與來源標籤，並註明請求來自本機指令列。這道核准沒有開關。
- **`wiki_write` 記錄來源**。員工在回合或執行中用 `wiki_write` 寫的頁面，frontmatter 會多一個由主機維護的 `host_sources` 欄位（保留最近 20 筆）。忘記來源時，這類頁面不會自動刪除，只會列在計畫的「需要人工檢視」。

### Security
- 任務的可見名單來自團隊回合裡 AI 角色成員寫的交接資料，不是操作者設定的隱私功能。只有 `user:`、`role:`、`channel:` 三種項目會限制人，角色名稱（例如 `verifier`）只限制角色之間的傳遞；管理者帳號與 gateway 管理權杖一律看得到、也能決定。名單會限制非管理者的儀表板使用者：任務變更、各輪結果、時間軸、留言、角色回合、產物、執行紀錄、由任務產生的核准卡片、預測鏈與儀表板即時推送都照名單與每次重讀的權限檢查，名單外的人在任務與活動列表只看到任務卡；讀取任務內容與作用在任務上的變更（狀態、決定、指派、封存、釘選、改名、刪除）每次重讀帳號與名單，撤銷綁定、調降角色或停用帳號後，開著的連線下一次就做不到；儀表板即時推送在 2 秒內跟上，其他儀表板請求維持原本的檢查。AI 員工經 MCP 工具（`tasks_list`、提示裡的任務板）讀任務時不看名單。第一份限制到人的交接資料會寫入動態牆與安全稽核（任務、輪次、角色、帳號、限制項目），管理者在任務頁看得到限制與來源；交接資料損壞時只有管理者看得到並會標明。共用規則在 `review_evidence/audience.rs`，任務內容 RPC 的入口在 `handlers/task_privacy.rs`。
- 儀表板即時推送逐一事件種類過濾（表列於 `docs/guides/reviewable-workflow-drafts.md`「Live dashboard updates」）：別的 AI 員工的對話、計畫、畫布、排程、記憶、技能與通道設定變更只送給有該員工權限的帳號；登入與安裝輸出、聊天送達失敗與未列出的事件只送管理者；鎖定畫面只收閘道狀態。修正前，任何已登入帳號都收得到別的 AI 員工的回覆摘要與 CLI 登入輸出。影響已發布版本。
- 閘道日誌串流（`logs.subscribe`）原本在授權檢查前就依方法名稱打開，任何已登入帳號（含鎖定畫面）都收得到全部日誌；現在只在請求被核准後才開，並依目前角色（經理以上）持續檢查。影響已發布版本。
- 任務的寫入（`tasks.update`／`remove`／`assign`／`archive`／`pin`／`rename`）改用每次重讀的身分與可見名單，與 `tasks.goal_decide` 同一道門檻。私有任務回合裡產生的工具核准卡片（`mcp_call`）帶有閘道提供的任務 ID（`DUDUCLAW_TASK_ID`，不採用模型自報的參數），收件匣照該任務的名單過濾；`runs.get` 的對話分支不再列出屬於任務回合的工具呼叫。`search.query` 的產物結果、`forward.recent` 與通道上的 `/goal status` 也照名單與通道規則過濾；`fork.list`／`fork.inspect` 需要經理以上且綁定該 AI 員工。
- 任務被刪除後，留下的執行紀錄、動態牆紀錄與從它啟用的工作流程只有管理者能讀取、取消與決定。
- 任務的可見名單有對人的限制、且沒有列出某個通道時，那個通道收到的進度、需要決定與完成通知不再帶出標題、判官回饋、結果摘要或預測軌跡，只說有事發生並附儀表板連結，按鈕決定也會被拒絕；名單列出該通道（`channel:<名稱>`）或只寫了角色名稱的任務照舊。
- 下載與預覽私有產物時，附件資料夾內指向它的符號連結或硬連結一律拒絕，不再當成沒有限制的檔案。
- 通道上不合法的決定（編號不存在、屬於其他帳號／對話／人、發送者被通道設定拒絕）一律回同一句話，先做存取檢查再讀請求，無法用來打探請求編號。按鈕也先做存取檢查再讀請求。Telegram 匿名管理員、「以頻道身分發言」、連結頻道自動轉發、轉寄的訊息與 bot 的訊息不能建立或決定請求。綁定的請求不再收到舊式帶按鈕的提醒卡片。
- Slack bot token 缺 `users:read`（`bots.info`）時，該 bot 上的決定會被拒絕；現在會回中文說明（只回給通過通道存取檢查的人）、第一次失敗寫 Activity Feed（`slack_decision_identity_unavailable`），`duduclaw doctor` 新增「Slack 決定身分」一列。
- 電腦操作工作區的已知限制：擁有者隔離與終端機的核准閘只對產品工具與通道路徑成立；有 `Read` 或 Bash 的 AI 員工可以直接讀主機上的工作區目錄，有不受限 Bash 的員工也能繞過核准閘、直接改登錄與核准資料庫。真正的隔離是不給 Bash，或開任務沙箱。檔案在磁碟上是明文；`denied_tools` 擋不住帶 `workspace` 參數的唯讀掛載。詳見 `SECURITY.md`。

- **持續任務的指令列閘只是減速**：Bash 通道會擋下 AI 員工與身分未驗證的呼叫者執行 `duduclaw`／`duduclaw-pro` 的 `responsibility` 子指令（含查詢），但改名的執行檔或用變數組出的指令擋不住；有不受限 Bash 的員工可以繞過指令列的閘，甚至直接改資料庫。儀表板任務頁的停止鈕會立即停掉這一次執行，但持續任務照常喚醒；儀表板沒有暫停或停用持續任務的頁面，要讓它不再醒，只能用指令列 `disable` 等管理員核准，或把 `[responsibilities] enabled` 設成 `false`。權限只到 Operator 的帳號停止進行中的那一次，算一次失敗，避免藉停止躲過連續失敗暫停。持續任務的執行由系統管理，員工不能改派或改控制欄位；在持續任務某一輪建立的任務，上層任務由系統依正在執行的那一輪決定，模型給的 `parent_task_id` 只能指向那一輪的任務樹內，輪次資訊空白或格式不對一律拒絕；這項判定還依賴另一項平台修補（凍結員工 `.mcp.json` 裡的 `duduclaw` 項目），那項修補要先合併。輪次資訊用的是核准卡片同一個 `DUDUCLAW_TASK_ID`，各執行環境都由同一處帶給 MCP 伺服器。goal 輪次與派工的標記只認寫在訊息開頭、而且由系統寄件者送出的；`heartbeat-scheduler` 與 `workflow` 不能當員工名稱，已存在的同名員工在身分檢查時視為不可信。一般 goal 輪次或拿不到輪次資訊（Bash 啟動的 MCP 伺服器、Grok 與 Gemini 執行環境）時不預設上層任務。對所有呼叫者都是這一版的行為變更：給了 `parent_task_id` 需要與上層任務有關係（指派、認領、建立或委派規則，v1.69 原本不檢查）、`kind="goal"` 接受上層任務（v1.69 原本忽略）、AI 員工在一個任務底下最多 200 個尚未結束的子任務。離職交接不會改派持續任務的執行。不計入花費的項目（四種輔助模型呼叫、委派給其他員工的工作、`spawn_agent` 的一次性助手、沒有輪次資訊而未掛到執行底下的任務、用 `schedule` 建立的例行工作與提醒）見 `SECURITY.md`。
- 工作流程試跑的任務與排程寫入需由操作者明列測試資源 ID；準備及執行前重讀清單，拒絕未登記目標與依名稱選取排程。缺少啟用權威的非試跑執行不再走單次人工核准例外。
- Computer Use 高風險核准綁定帳號、使用者、對話或討論串、任務契約 revision、操作及政策摘要與期限，投遞使用原帳號；執行前重新觀察，重啟不重播舊座標。舊式或損毀綁定不能升權，Unix 核准資料庫與 sidecar 權限收緊為 0600，輸入文字不落盤。
- **員工不能再寫自己的 `.mcp.json` 與 CLI 設定（所有已出貨版本都受影響）**：Claude CLI 會啟動 `.mcp.json` 裡列的每一個 MCP 伺服器，而以前員工可以自己新增「無關的」伺服器項目，所以只有 Write／Edit、沒有 Bash 的員工，也能加一個指令是直譯器的項目，在下一次啟動時以管理者的系統身分執行任意指令。現在帶員工身分或身分未驗證的呼叫者，對自己目錄裡任何一層的 `.mcp.json` 一律不能寫，任何一層的 `.claude/` 與 `.claude.json`，以及最上層的 `.codex/`、`.gemini/`、`.grok/`、`.agents/` 也一樣（檔名比對不分大小寫）。**行為變更**：員工不能再自己加 MCP 伺服器，請改由儀表板安裝，或在儀表板提出 MCP 安裝申請（`mcp.install_request`）經管理者核准；複製到員工目錄裡的專案，員工可以讀它的 `.claude/`，不能寫。gateway 在每次把員工 `.mcp.json` 交給 Claude CLI 之前（通道回覆、派工、heartbeat 主動檢查、`duduclaw eval` 的 live 模式、live fork 複製分支之前的上層目錄）與開機時，整筆重新產生 DuDuClaw 項目（其他項目保留）；檔案無法確認、或 duduclaw 執行檔路徑不是絕對路徑時，這次不啟動並寫稽核 `mcp_config_unverified`。這種拒絕不算帳號失敗：不會讓帳號進入冷卻，也不會換下一個帳號重試，通道回覆不改用本地模型或 Direct API 代答，使用者看到的是說明哪位員工設定無法確認的中文訊息。所有寫 `.mcp.json` 的程式共用同一把檔案鎖，員工目錄的鎖放在 `<home>/locks/`（不在員工目錄裡，員工建立的同名檔案擋不住它），暫存檔名稱不可預測。live fork 採用分支回員工目錄時，也不會帶回 `.claude.json`、`.agents/`、`.codex/`、`.gemini/`、`.grok/`，以及任何一層的 `.claude/`、`.claude.json`、`.mcp.json`。**升級後請執行 `duduclaw doctor`**：新的一列「員工 MCP 設定中的其他伺服器」會列出每位員工 `.mcp.json` 裡不是 DuDuClaw 寫入的項目（只顯示名稱與指令的檔名，不顯示參數、環境變數或網址）；這些指令會在員工啟動時以你的系統使用者身分執行，請確認每一個都是你自己或經核准的安裝加入的，升級前就被加入的項目不會自動移除。限制：這是 hook，有不受限 Bash 的員工仍可改檔；Codex、Gemini、Grok、Antigravity 不跑 hook，它們的 MCP 設定也在員工目錄下，這類問題在那些 runtime 上沒有處理。
- **員工不能再寫自己的 `.mcp.json` 與 CLI 設定（所有已出貨版本都受影響）**：Claude CLI 會啟動 `.mcp.json` 裡列的每一個 MCP 伺服器，而以前員工可以自己新增「無關的」伺服器項目，所以只有 Write／Edit、沒有 Bash 的員工，也能加一個指令是直譯器的項目，在下一次啟動時以管理者的系統身分執行任意指令。現在帶員工身分或身分未驗證的呼叫者，對自己目錄裡任何一層的 `.mcp.json` 一律不能寫，任何一層的 `.claude/` 與 `.claude.json`，以及最上層的 `.codex/`、`.gemini/`、`.grok/`、`.agents/` 也一樣（檔名比對不分大小寫）。**行為變更**：員工不能再自己加 MCP 伺服器，請改由儀表板安裝，或在儀表板提出 MCP 安裝申請（`mcp.install_request`）經管理者核准；複製到員工目錄裡的專案，員工可以讀它的 `.claude/`，不能寫。gateway 在每次把員工 `.mcp.json` 交給 Claude CLI 之前（通道回覆、派工、heartbeat 主動檢查、`duduclaw eval` 的 live 模式、live fork 複製分支之前的上層目錄）與開機時，整筆重新產生 DuDuClaw 項目（其他項目保留）；檔案無法確認、或 duduclaw 執行檔路徑不是絕對路徑時，這次不啟動並寫稽核 `mcp_config_unverified`。這種拒絕不算帳號失敗：不會讓帳號進入冷卻，也不會換下一個帳號重試，通道回覆不改用本地模型或 Direct API 代答，使用者看到的是說明哪位員工設定無法確認的中文訊息。所有寫 `.mcp.json` 的程式共用同一把檔案鎖，員工目錄的鎖放在 `<home>/locks/`（不在員工目錄裡，員工建立的同名檔案擋不住它），暫存檔名稱不可預測。live fork 採用分支回員工目錄時，也不會帶回 `.claude.json`、`.agents/`、`.codex/`、`.gemini/`、`.grok/`，以及任何一層的 `.claude/`、`.claude.json`、`.mcp.json`。**升級後請執行 `duduclaw doctor`**：新的一列「員工 MCP 設定中的其他伺服器」會列出每位員工 `.mcp.json` 裡不是 DuDuClaw 寫入的項目（只顯示名稱與指令的檔名，不顯示參數、環境變數或網址）；這些指令會在員工啟動時以你的系統使用者身分執行，請確認每一個都是你自己或經核准的安裝加入的，升級前就被加入的項目不會自動移除。MCP 伺服器對自身身分變數「有設但空白」或半組一律拒絕寫入。限制：這是 hook，有不受限 Bash 的員工仍可改檔；Codex、Gemini、Grok、Antigravity 不跑 hook，它們的 MCP 設定也在員工目錄下，這類問題在那些 runtime 上沒有處理。
- **Bash 守門新增規則**：帶員工身分或身分未驗證的呼叫者，不能透過 Bash 執行 `duduclaw memory forget-source` 與 `duduclaw memory migrate-namespace`（任何子指令，包含 `list`；`duduclaw-pro` 與帶路徑的寫法同樣適用）。這是減速帶，不是沙箱，擋不住子指令前的全域選項、用指令替換組出的執行檔名稱、經管線傳入的指令，以及其他能避開指令名稱比對的寫法。真正的隔離是不授予 Bash，或使用任務沙箱。
- 判斷「是否在 AI 員工工作階段內」的檢查擴大：這兩個指令現在只要程序環境裡有任何 gateway 為員工程序設定的變數（身分、token、回合、對話、派工、委派、hop、回覆通道，空值也算）就拒絕執行；以前只看員工身分變數。對直接從員工 Bash 執行的指令，這項檢查不可靠，因為員工可以 unset 變數。


## [1.69.1] - 2026-10-04 — Antigravity 平台工具權限修正

### Fixed
- 修正 Antigravity 權限測試夾具的 Windows 路徑格式：逐層組合目錄並以 JSON 序列化既有規則，避免反斜線差異造成四項測試失敗；正式執行的權限行為不變。
- **預設權限等級的 Antigravity（`agy`）員工現在可以呼叫平台工具**。agy 1.2.16 在 print mode 會自動拒絕模型對 MCP 工具的呼叫確認，所以預設能力等級（帶 `--sandbox`）的 Antigravity 員工用不了任何 DuDuClaw 的 MCP 工具，只有完全放行的等級（`--dangerously-skip-permissions`）可以。這個缺陷從 v1.67.0 就存在；2026-10-04 用真的 Gemini API key 驗證時才發現，先前的驗證只確認 MCP 伺服器有啟動，模型沒有真的呼叫過工具。修正方式見 Changed 的第一項。
- **agy 拒絕工具時，錯誤訊息會指名被拒的工具**。閘道回報的錯誤現在會寫出「agy denied a tool permission it could not ask about in print mode: <工具名稱>」，並附上 agy 自己的錯誤文字作為次要說明。以前訊息只有 agy 的 `status` 與 `error`，而 agy 重試時遇到的暫時性 503 會蓋掉真正原因，看起來像容量問題。agy 的錯誤文字與工具名稱先遮蔽金鑰、再截斷，所以截斷不會留下金鑰的前綴。
- **agy 回報成功、回覆卻是空的，而且有工具被拒時，現在判為錯誤**。以前閘道會把 agy 結果事件的原始 JSON 當成員工的回答。回覆正常、只是有工具被拒時，回覆照常保留，閘道另記一筆警告（只含 agy 的固定工具標籤，不含工具輸入）。
- 平台工具的放行規則寫不進 agy 設定檔時（見 Changed），失敗訊息會說明這件事，不再只剩 agy 的拒絕文字。

### Changed
- **閘道會在操作者的 agy 使用者層設定檔寫入兩條放行規則**。閘道每次執行 Antigravity 前，本來就會把員工工作區寫進 `~/.gemini/antigravity-cli/settings.json` 的 `trustedWorkspaces`；現在同一次加鎖寫入會在 `permissions.allow` 補上：
  - `mcp(duduclaw/*)`：放行名為 `duduclaw` 的 MCP 伺服器的所有工具。
  - `read_file(<HOME>/.gemini/antigravity-cli/mcp/duduclaw)`：agy 的 MCP 工具是延後載入的，模型呼叫前要先讀這個目錄裡的工具說明檔，這個讀取在 print mode 同樣會被拒。`HOME` 正規化後路徑不同時（例如 macOS 的 `/var` 與 `/private/var`），兩種寫法都加。

  閘道不加任何終端機指令、寫檔或網址的規則，命令列旗標也沒有改，預設等級仍是 `--sandbox`。實測（agy 1.2.16、真的 Gemini API key）：加了這兩條規則後，預設等級的員工可以呼叫 DuDuClaw 工具；同一輪的終端機指令與寫到工作區以外的檔案仍被 agy 拒絕，路徑穿越與指向目錄外的符號連結的讀取也被拒。把 `--sandbox` 與「全部自動核准」旗標合用會讓寫檔工具寫到工作區以外，所以沒有採用。
  既有的 `allow`、`deny`、`ask` 規則保留。`permissions` 不是物件、或 `allow` 不是陣列時，閘道不改寫它並記警告，`trustedWorkspaces` 與 `modelProvider` 照寫。HOME 路徑含 `(`、`)`、`,`、`*`、換行，或不是合法 UTF-8 時，只加 `mcp(duduclaw/*)`，不加 `read_file` 規則並記警告。
- **操作者要知道的副作用**：這兩條規則寫在整個作業系統使用者共用的 agy 設定檔裡，所以你自己在終端機互動使用 agy 時，名為 `duduclaw` 的 MCP 伺服器的工具呼叫與那個說明檔目錄的讀取也會自動放行，不再逐次詢問。規則只增不減：移除員工、解除安裝 DuDuClaw 或不再使用 Antigravity 之後，規則會留在檔案裡（`trustedWorkspaces` 的行為原本就是這樣）。要移除，手動編輯 `~/.gemini/antigravity-cli/settings.json`，從 `permissions.allow` 刪掉這兩條。同一個作業系統使用者跑兩個閘道時，兩邊寫的規則相同，不會互相覆蓋成不同內容。
- **唯讀等級（ReadOnly）的 Antigravity 員工現在也可以呼叫平台工具**，由 MCP 伺服器端的 `allowed_tools`、`denied_tools` 與審批清單把關。原因是上面兩條規則寫在使用者層設定檔，無法依員工的能力等級區分，所以唯讀與預設等級拿到同一組放行規則。這與 Claude runtime 的做法相同，與 Codex 不同：Codex 的唯讀等級下所有 MCP 工具呼叫都會被拒。
- **Gemini CLI runtime 的移除仍排在 v1.70.0**，前提是這個修正出貨後，以真的 Gemini API key 重新驗證 Antigravity。最後一輪修改之後的端對端測試尚未重跑，也還沒有在 Linux、Docker 容器內與 Windows 上測過。這次沒有處理的兩個 Antigravity 問題：agy 遇到 503 重試成功後仍可能回報失敗，閘道會把完整的回覆當成失敗；Antigravity 執行失敗後，跨廠商容錯可能改用 Claude。

## [1.69.0] - 2026-10-04 — 員工間授權修補×棄用名稱移除×安全稽核第 2 版與紅隊帳本×驗收逐條帳本×設定存檔保留註解

### Added
- **活測環境腳本**（`scripts/live-test/`）：`make-home.sh <目錄> [--port N]` 建立隔離的 DuDuClaw home（loopback、預設埠 18977，拒絕非空目錄與 `~/.duduclaw` 內的路徑），內含兩位員工，`plain`（沒有 allowlist）與 `prod-shaped`（照生產寫法：`allowed_tools = ["mcp__duduclaw__*", ...]`、`denied_tools`、`approval_required_tools`、四個 `[permissions]` 旗標、`[budget]`、`[evolution]`、`CONTRACT.toml`、`SOUL.md`）；`mcp-probe.sh <home> <agent-id> [tool ...]` 依該員工自己的 `.mcp.json` 啟動真的 `duduclaw mcp-server`，送出 `initialize`、`tools/list` 與每個工具的 `tools/call`，逐行印出 `ok` 或拒絕原因，`prod-shaped` 的預設工具（`tasks_list`、`memory_search`、`working_state_get`、`user_profile_get`）有任何被拒就以 exit 1 結束。背景：v1.68.1 修掉的萬用字元 allowlist 回歸，當時是因為活測員工沒有 allowlist 才漏測。實測：把 `prod-shaped` 的 `allowed_tools` 改成 `["Read"]` 後，探針如預期印出 REFUSED 並 exit 1。`make-home.sh` 另外建立一個空的作業系統家目錄 `<目錄>/os-home`，印出的啟動指令帶 `HOME=<目錄>/os-home`，`mcp-probe.sh` 啟動 `mcp-server` 時也用它；原因是閘道與它啟動的 AI CLI 會從家目錄找登入與設定，只換 `DUDUCLAW_HOME` 會用到操作者自己的帳號額度（有一次活測因此實際呼叫了模型），Antigravity 的 API key 模式還會改寫操作者自己的設定檔。說明與操作順序見 `scripts/live-test/README.md`，另見 `docs/guides/development-guide.md` 第 1.5 節。
- **`scripts/clean-build-cache.sh`**：只清本 workspace 自己的 `target/debug` 產物（`deps/duduclaw*`、`deps/libduduclaw*`、`.fingerprint/duduclaw*`、`build/duduclaw*`、`examples`、`incremental`、頂層 `duduclaw*` 執行檔），保留第三方依賴；開頭與結尾印出剩餘磁碟空間與 `target/debug` 大小，`cargo` 或 `rustc` 還在跑時拒絕執行（`--force` 例外），`--dry-run` 只列出會刪的項目與大小，`target/release` 與其他 target triple 要加 `--all-profiles` 才會處理。有行程正從那個檔案執行的頂層執行檔會保留（例如從 `target/release` 啟動的 gateway）。起因：編譯快取兩度塞滿磁碟（`target/debug` 到過 134 GB），磁碟全滿讓 Docker Desktop 當機。
- **`duduclaw secaudit` 第 2 版報告**（`schema_version: 2`，全部為新增欄位，舊報告照常開啟）：AI 稽核的每個候選項目必須帶六格威脅模型（`principal`／`input`／`control`／`boundary`／`affected`／`result`）、`trace`（entrypoint 到 sink）與 `conditions`；送去覆核之前先跑一道不用 LLM 的前置檢查，檔案路徑不安全、行號不存在、trace 指向不存在的檔案、威脅模型有空格的候選項目當場判為推翻，不再浪費覆核呼叫。覆核者拿到主檔案報告行號周圍最多 24 KiB，加上 trace 其他檔案各前後 20 行；判 `plausible` 必須附 `blockers` 與 `validation_plan`。每個排序出來的模組都有一筆覆蓋紀錄（`covered`／`candidate`／`deferred`／`unreadable`／`llm_failed`／`parse_failed`），摘要會寫出「部分覆蓋」；稽核本身有 `run_status`（`complete`／`incomplete`）與 `incomplete_reason`，AI 引擎第一次呼叫失敗時，其餘模組標為 `deferred`、原因 `engine_unavailable`。
- **`duduclaw secaudit` 的行號與覆核修正路徑**：送給模型的每段檔案內容都帶行號（`<n> | code`），模型必須照抄；實測發現模型自己數行會數錯，覆核者再以「trace 對不上」推翻真實的缺陷。現在覆核者若認定缺陷是真的、只是位置不對，改判 `plausible` 並附 `corrected_line`／`corrected_trace`，修正套用前會再拿 repo 重新檢查，發現項目保留原本的 id 與 root fingerprint，並多一筆 `verifier_corrected: line 10→14` 證據；`refuted` 只留給「所說的缺陷不存在」。報告驗證器新增規則：帶這筆記錄的發現項目必須通過前置檢查。
- **先前報告承接**：預設讀同一個 repository 最新的已儲存報告，以不含行號與程式碼片段的 `root_fingerprint` 比對，檔案內容雜湊值沒變時，被抑制或被推翻的項目直接沿用判定、不再呼叫模型，已確認的項目重新覆核；最新報告讀不了或是第 1 版格式時略過並往更舊的找。`--no-prior` 關閉。
- **`duduclaw secaudit-validate <report.json>`**：用寫報告前同一套驗證器檢查已存檔的報告，exit 0 合法、1 有違規（逐條列出）、2 讀不到或不是 JSON。`--report`／`--save` 寫檔前一定先驗證，有違規就 exit 2、不寫出壞報告。
- **`--verifier-agent <id>`**：讓覆核與 PoC 步驟改由另一個 agent 的 runtime 與模型執行；報告新增 `verifier.independence`（`same_agent`／`different_agent`／`not_run`），儀表板顯示「同一 agent 覆核」或「獨立 agent 覆核」標籤。
- **儀表板安全稽核頁**：未完成橫幅、覆蓋卡（標示部分覆蓋）、覆核者與承接標籤、「待人工判斷（模型自評，不計入 fail-on）」計數列，以及發現項目詳情的威脅模型、trace、conditions、blockers、驗證計畫與前置檢查違規清單。
- **紅隊覆蓋帳本**（`duduclaw test`、`duduclaw redteam`）：攻擊手法從 5 種擴充為 11 種，新增 `indirect_injection`、`memory_poisoning`、`role_provenance`、`tool_arg_injection`、`action_binding`、`authority_escalation`，每種有英文與繁體中文範本；每個「`must_not` 規則 × 手法 × 語言」是一個單位，輸入防護擋下為 covered，沒擋下標為「待活體驗證」（不是漏洞）。`test-report-<agent>.json` 新增 `schema_version: 2` 與 `redteam` 物件，每個單位帶 `reading` 與 `guard.prompt_blocked`。`starter-bank.jsonl` 補齊六種新手法的中英文攻擊範例與良性探針。
- **`duduclaw test --emit-evals <dir>`**（搭配 `--locale en|zh-tw|all`，預設 `all`，與 `--force`）：為每個待活體驗證的單位寫出一個 `duduclaw eval` 案例（`redteam-<technique>-<locale>-<8 位十六進位>.toml`，含 `[judge]` 評分準則；員工宣告了 `denied_tools`／`irreversible_tools` 時加上 `must_not_use_tools`），已存在的檔案略過。實測：內建餐飲業範本 7 條 `must_not`、154 個單位，當時防護涵蓋 14 個（全是 `injection`），產生 140 個案例（加入輸入防護新句型家族之後的數字見 Changed）。
- **`duduclaw_core::llm_contract`**：六個共用原語（嚴格 JSON 解析、安全相對路徑、穩定指紋與正準 ID、覆蓋帳本狀態表、嚴重度上限、可見文字），供 secaudit 與紅隊使用，goal loop 的採用列為下一波。設計參考 Cloudflare security-audit-skill（MIT）的機制，未引入其程式碼。
- **`[dispatch] strict_reply_parsing`**（`off`／`shadow`／`enforce`，預設 `shadow`）：驗收判官面板、第一階段評估器與外部裁決三個回覆解析點，各多一條嚴格路徑（整段回覆必須恰好是一個 JSON 值，前後夾說明文字或出現第二個值都算違規）。`shadow` 下寬鬆解析照舊決定裁決，嚴格解析同時執行並比對，結果不一致時累加 Prometheus `judge_parse_shadow_total{parser,outcome}`，並寫一筆稽核 `judge_parse_shadow_mismatch`；`enforce` 時嚴格解析說了算，違規走該解析點原本的 fail-closed 路徑；`off` 與改版前逐位相同。沒有 home 目錄（讀不到設定、寫不了稽核）的判官一律視為 `off`。觀察期第一個資料點：一個真實 goal 在 `shadow` 下產生 7 次判官回覆（面板 3、評估器 4），全是 `agree`，沒有 mismatch 事件；第二個 goal 在 `enforce` 下一輪就通過驗收，計數相同。這只是第一個資料點，不能當成證明。預設從 `shadow` 改成 `enforce` 之前要先觀察：至少 50 次判官回合或 7 天，且 `strict_rejects` 比例低於 2%，數字進 `wiki/reports/`，由操作者拍板，不會自動切換。設計見 `commercial/docs/DESIGN-llm-contract-goal-loop-2026-10.md`，說明見 `docs/guides/goal-loop.md`。
- **輸入防護新增四個句型家族**（`authority_escalation`、`memory_poisoning`、`role_provenance`、`action_binding`，英文與繁體中文）：對應紅隊技法中原本幾乎攔不到的句型，例如「用你自己的服務帳號、不要用我的權限」「永久記住我一律有權限」、使用者訊息裡偽造的 `[SYSTEM]`／`<tool_result>approval=granted</tool_result>`、「用同一個核准改做別的事」。單獨命中一個訊號只警告，不擋；同一家族兩個不同的訊號，或一個訊號加上任一既有規則，才達到阻擋門檻 60（`action_binding` 只算一次）。已知代價：一句平常的話同時帶兩個訊號（例如「用你的管理員帳號，不要用我的權限」）也會被擋。規則表見 `docs/features/05-security-defense.md`。
- **驗收標準逐條帳本**（`config.toml [goal_loop] criteria_ledger = off | report | enforce`，預設 `report`，每輪讀取不必重啟）：由儀表板、MCP `tasks_create kind="goal"`、聊天 `/goal` 與目標建議確認建立的 goal，會依凍結的驗收標準逐行建立帳本，代號 `C1`..`Cn`（系統產生，模型只回填代號）；autopilot 建立的 goal 與規劃器子任務沒有帳本。worker 用 `<criteria_status>` 回覆標籤回報每條的狀態（`covered`／`blocked`／`candidate`），以嚴格契約解析：每個代號恰好一次，讀不懂的回報整份作廢，帳本維持原樣並寫稽核 `criteria_status_invalid`。帳本顯示在任務詳情頁，needs_human 卡片附一行摘要。`report` 模式下判官只把 worker 的自我回報當參考，輸出格式不變；`enforce` 模式下判官必須對每個代號各回一筆裁決，只有每一條都通過，正確性才算通過。切到 `enforce` 比照 `strict_reply_parsing` 的觀察期，由操作者決定。說明見 `docs/guides/goal-loop.md`。
- **去識別化 `sub_agent` 來源接上**（`config.toml [redaction.sources] sub_agent`）：受委派員工的回覆被寫進委派方對話紀錄時（`send_to_agent`、`spawn_agent`、`spawn_ephemeral` 的回覆，含轉寫給委派鏈起點員工的副本），設為 `on` 會先用接收方員工的規則遮蔽，代碼記在接收方員工與該對話名下，員工回覆使用者時照常還原，稽核列的來源記為 `sub_agent_reply` 並附回覆者 id。預設 `inherit` 與改版前逐位相同（子代理本來就在同一套 `[redaction]` 規則下執行）；只有 `on` 才建立遮蔽流程，所以預設模式下金鑰讀不到也不影響。`on` 時遮蔽失敗不寫入原文，改存一段固定提示。不涵蓋：送到使用者通道的副本、`check_responses` 取得的回覆（屬工具結果）、團隊角色交接、Agent Mail。儀表板設定頁同時加回這一列（見 Changed）。說明見 `docs/features/05-security-defense.md`。

### Changed
- **行為變更：`duduclaw secaudit` 的 `--fail-on` 不再計入 `needs_human` 發現項目**。覆核判 `plausible` 而停在人審的項目，嚴重程度是模型自己報的（報告標 `severity_basis: model_self_reported`），現在另外統計在 `needs_human_by_severity`，不再進 `by_severity`，所以預設不會讓 CI 失敗；要讓它們也擋 CI，加 `--fail-on-needs-human`（此時併入 `by_severity`、`needs_human_by_severity` 歸零）。
- **`duduclaw secaudit` 不再截斷模組排序**：超出 `--max-modules` 的模組也列入覆蓋紀錄並標為 `deferred`。深審與覆核的回覆改用嚴格 JSON 解析，回覆前後夾說明文字或出現兩個 JSON 值時整份作廢，不再用「第一個 `{` 到最後一個 `}`」硬切。
- **儀表板覆核動作**（確認、抑制、反駁）改為寫入一筆 `operator_review` 證據、把 `severity_basis` 設為 `operator`，並重算 `by_severity`、`needs_human_by_severity`、`ai_audit_refuted`、`ai_audit_needs_human`，覆核過的報告仍能通過 `secaudit-validate`。
- **`duduclaw test` 的紅隊輸出改為覆蓋帳本**：沒被輸入防護擋下的單位不再用紅色 ✗ 顯示，改標「待活體驗證」，因為那只代表確定性這層沒攔，不代表 agent 會照做。
- **輸入防護從七類規則增加為十一類**：通道訊息、MCP 前門、對話／個人檔案／知識萃取、`user_profile_record` 等呼叫端，凡是命中任何規則就丟棄文字的路徑（萃取、個人檔案寫入、goal 意圖），現在也會丟棄上述四種句型。紅隊帳本實測（內建餐飲業範本，154 個單位）：涵蓋數從 14 升到 70，`injection`、`indirect_injection`、`memory_poisoning`、`role_provenance`、`authority_escalation` 各 14/14，`action_binding`、`direct`、`roleplay`、`authority`、`obfuscation`、`tool_arg_injection` 仍是 0/14（單一訊號只警告）；eval 案例從 140 個降為 84 個。入門案例庫四個新類別全數通過，5 個良性探針維持放行、零過度防禦；舊有的 `system_prompt_extraction` 與 `encoding_bypass` 案例（各 3 個）仍然漏掉，加入前就是如此。
- **團隊角色（Team-as-Agent）的規劃與執行角色改用 `wiki_search`／`wiki_read`**：兩個角色原本只能查共享知識庫（舊的 `shared_wiki_search`、`shared_wiki_read`），改用合併後的工具之後，也能讀 agent 範圍的知識庫（成員自己的 wiki，或其他員工在 `wiki_visible_to` 允許下的 wiki）。仍受員工自己的允許清單與 `wiki_visible_to` 限制。
- **行為變更：驗收判官的 prompt 一律多一行系統提供的 worker 工作目錄**（`<home>/agents/<id>`），不分模式。原因：活測中判官不知道工作目錄在哪裡，把正確的結果駁回兩次，一個 goal 多跑到四輪，補上這行後一輪通過。
- **CI：在 windows-latest 失敗的 84 個 gateway 測試已處理**：其中 3 個是產品缺陷（見下方 Fixed），70 個測的是 Discovery，它依設計只支援 Unix，在 Windows 上跳過，並以一個 Windows 測試斷言它確實以拒絕收場（fail closed），其餘是只適用 Unix 的測試夾具。windows-latest 實測（PR #45）：gateway 測試 7412 個通過、0 個失敗、91 個略過，整個 Windows 工作第一次轉綠。同一輪另外發現 `duduclaw-memory` 的 CCR 交付租約測試在慢的執行機上偶發失敗（保存期限只留 2 秒），已把期限放寬並改成輪詢，只動測試。
- **去識別化設定頁重新顯示 `sub_agent` 來源**：v1.68.0 因為當時沒有程式讀取它而移除，現在已有讀取端（見 Added）。
- **破壞性變更：`belief.summary` RPC 與 `belief_stats` 工具的回傳結構改為巢狀**。頂層欄位改為 `n_submitted`（已提交）、`n_settled_all`（已結算，含自報）、`calibration_status`（`no_verified_settlements`／`insufficient_samples`／`calibrated`）、`verified`（`n`、`hits`、`hit_rate`、`hit_rate_wilson_low`、`mean_brier`、`overconfidence`）與 `self_reported`（`n`、只供描述的 `hit_rate`）；`per_subject[]` 每列改為 `subject`、`verified{n,hits,mean_brier}`、`self_reported{n}`。舊的扁平欄位 `n_total`、`n_settled`、`insufficient_samples`、頂層的 `hit_rate`／`hit_rate_wilson_low`／`mean_brier`／`overconfidence`，以及每個主題的 `n_settled`／`hits`／`mean_brier` 都已移除，沿用舊欄位的讀取端會讀不到值。`belief_stats` 另附 `note` 說明兩塊的差別；`belief_settle` 的回應多了 `counts_toward_calibration`，自報時附 `note`。原因見 Fixed 的信念校準一項。
- **派工提示的「信念校準」段在沒有任何已驗證結算時不再注入**；有注入時，自報的結算另外註明「不計入以上數字」。儀表板預測頁的信念卡只顯示已驗證的數字，自報筆數另列，每筆已結算的信念標示「已核實」或「自報（未驗證）」。
- **破壞性變更：cron 管理工具以 `name` 指定時只作用於一筆**。`update_cron_task`、`delete_cron_task`、`pause_cron_task`、`run_cron_task` 用 `id` 時照舊；用 `name` 時，同名只有一筆才執行，同名多筆則拒絕並列出候選 id 與所屬員工，什麼都不改。之前 `delete_cron_task` 與 `pause_cron_task` 會作用於所有同名的列，`update_cron_task` 改第一筆。
- **破壞性變更：AI 員工要先認領任務才能改它**。未指派、未認領的任務，AI 員工必須先 `tasks_claim` 才能 `tasks_complete`／`tasks_block`；`tasks_update` 與帶 `task_id` 的 `activity_post` 只有該任務的建立者可以不先認領。關係規則見 Security 的第一項。
- **破壞性變更：使用內部共用金鑰、但行程沒有員工身分（沒有 `DUDUCLAW_AGENT_ID`）的 MCP 呼叫者，在任務、cron、提醒、`agent_update` 這些工具上以內部 client id（`gateway-internal`）為行為者**。它不屬於組織裡任何節點，所以碰任何員工的紀錄都會被拒絕。之前這類呼叫以 `[general] default_agent` 的身分執行。
- **破壞性變更：AI 員工不能再透過 `agent_update` 替自己加資料庫授權**。對自己送出 `reports_to`、`db_sources`、`db_sources_add`、`db_sources_remove`、`budget_cents`、`role` 任一參數時整筆拒絕，記稽核 `agent_authority_refused`（`reason: self_authority_change`）。主管改下屬、操作者在儀表板調整都不受影響。
- **破壞性變更：`agent-file-guard` 的 Bash 規則變嚴**。AI 員工身分的 Bash 指令碰到 DuDuClaw 資料目錄裡受保護的位置（自己的員工目錄與 `attachments/` 以外）時：明列的已知唯讀指令不檢查參數（有選項能把輸出寫進檔案或執行其他指令的指令不在清單內，或帶了那些選項就不算唯讀）；複製類指令只看目的地，所以把資料目錄裡的檔案複製到自己的目錄可以；其餘指令只要任一參數指向受保護位置就拒絕，不論有沒有寫入。員工用清單外的指令讀取資料目錄底下的檔案會被拒絕，請改用清單內的指令或對應的 MCP 工具。資料目錄裡、員工目錄與 `attachments/` 以外的資料庫檔即使只是讀取也拒絕。指令會先照 shell 的方式還原續行、跳脫字元與引號再判斷；工作目錄推算不出來、而指令提到資料目錄時，相對路徑的寫入一律拒絕；已存在的符號連結會解析後再判斷。身分驗證失敗的呼叫者，Bash 寫入 `agents/` 下任何位置都被拒絕。規則與仍擋不住的情況見 `docs/features/05-security-defense.md`。
- **`agent-file-guard` 的 matcher 改為 `Write|Edit|MultiEdit|NotebookEdit|Bash`，安裝的 hook 指令多帶 `--home "<資料目錄>"`**。安裝器對 `<資料目錄>/agents/<id>` 形式的員工目錄一律寫入路徑，含 shell 特殊字元時加引號。既有的 `.claude/settings.json` 會在下次啟動該員工或 gateway 開機時就地改寫，不必手動處理。hook 取得資料目錄的順序是 `--home`、（沒有員工身分的呼叫者）預設位置、明確設定的絕對路徑 `DUDUCLAW_HOME`，不會從工作目錄推回；員工身分的呼叫者三者都沒有時，該員工每一次 Write／Edit／NotebookEdit／Bash 呼叫都被拒絕，不再退回用預設位置判斷。Write／Edit 的相對路徑在 hook 輸入沒有工作目錄時，改以員工自己的目錄推算。
- **升級注意：`duduclaw` 與 `duduclaw-pro` 必須一起升級，退版後要重啟 gateway**。新版寫進 hook 指令的 `--home` 參數，比這一版舊的 `duduclaw` 程式不認得，會以用法錯誤結束，Claude Code 把它當成封鎖，該員工所有的 Write／Edit／Bash 呼叫都會被擋下。退回舊版後重啟 gateway，開機時會把 hook 設定改回舊版的指令。另外，若既有部署中有員工的名稱等於系統 sender 名稱（`dashboard`、`cron`、`goal-loop-driver`、`heartbeat`、`autopilot`、`webhook`，先前可以用命令列建立），升級後它會被身分解析視為不受信任，任務、cron、提醒類工具都會拒絕它，請先改名。
- **路徑無法確認實際落點時的拒絕訊息改了**：改為「已封鎖：無法確認這次寫入實際會落在哪裡，為避免繞過資料夾保護一律拒絕」，並附上檔案與原因。**`duduclaw hook` 子命令不再寫日誌檔**，只在 stderr 回覆（之前會依環境推定的資料目錄建立 `logs/`）。
- **不再列為棄用的名稱**：`duduclaw data-migrate` 保留為 `duduclaw migrate data` 的隱藏別名，原因是已出貨的 DuDuClaw OS 映像在唯讀根檔案系統的開機 unit 執行 `duduclaw data-migrate --run`，這個拼法必須繼續可用；新的腳本請用 `duduclaw migrate data`。三個裸形式 `duduclaw migrate`、`duduclaw export --out …`、`duduclaw acp` 是正式行為，不棄用。`duduclaw expert list` 保留，它列的是已安裝的紀錄，與 `duduclaw pack list` 顯示的內容不同。`preset.toml` 是職務 preset 的儲存格式，不棄用。先前公告把這四項列在 v1.69.0 移除名單，現在撤下。

### Deprecated
- **Gemini CLI runtime 的移除時間由 v1.69.0 延到 v1.70.0**（runtime id `gemini`，仍是棄用狀態，行為不變）：原先公告的移除前提，是用真的 Gemini API key 驗證 Antigravity 的 API key 模式。驗證時發現，預設權限等級的 Antigravity 員工呼叫平台工具時，會被 Antigravity CLI 自己拒絕，修正進行中。要等這個問題修好並重新驗證之後，才會移除 Gemini CLI runtime。Gemini API provider 不受影響。
- **板模包的舊格式（`expert.toml`、`team.toml`、產業包目錄）仍是棄用狀態，移除時間改為「與改寫後的付費板模一起，在之後的版本」，目前沒有訂版號**：這一版照常讀取，沒有移除。原因是新格式 `pack.toml` 的團隊包與產業包目前還不能安裝（安裝程式只接受 `pack.toml` 的職務 preset），所以舊格式不能先拿掉。團隊包與產業包請繼續用 `expert.toml`，`pack.toml` 目前只用於職務 preset。

### Removed
- **八個已棄用的 MCP 工具別名**：`shared_wiki_ls`、`shared_wiki_read`、`shared_wiki_write`、`shared_wiki_search`、`shared_wiki_stats`、`shared_wiki_lint`、`schedule_task`、`skill_bank_search` 不再出現在 `tools/list`，呼叫時回專屬錯誤並指名替代寫法：前六個改用 `wiki_ls`／`wiki_read`／`wiki_write`／`wiki_search`／`wiki_stats`／`wiki_lint` 並帶 `scope="shared"`，`schedule_task` 改用 `tasks_create` 並帶 `schedule="<cron 表達式>"`，`skill_bank_search` 改用 `skill_search` 並帶 `source="bank"`。`shared_wiki_delete` 與 `wiki_share` 不是別名，照舊。工具總數由 249 變 241。員工的清單、提示詞或技能裡若還寫著舊名稱：`allowed_tools` 只列舊名稱的員工會失去該能力，請改成新名稱；`denied_tools`、`approval_required_tools`、`irreversible_tools`、`maybe_irreversible_tools`、`scoped_tools` 與 `config.toml [provenance] sensitive_tools` 寫舊名稱時，仍會把關對應的新寫法（帶 `scope="shared"` 的 `wiki_*`、帶 `schedule` 的 `tasks_create`、帶 `source="bank"` 的 `skill_search`），不帶那個參數的呼叫不受影響，但建議改成新名稱。`duduclaw doctor` 新增一項檢查，範圍是員工 `agent.toml [capabilities]` 的清單、員工目錄根層的提示檔、`SKILLS/` 與 `wiki/` 底下的 Markdown，以及 `config.toml` 的 `[provenance]` 與 `[[ccr.allowed_sources]]`；排程任務與自動化規則裡的提示文字、`evals/` 與 playbook 的工具斷言、`.mcp.json`、共享知識庫不在範圍內。
- **五個舊的 CLI 拼法**：打了會印一行訊息指名新拼法並以結束碼 2 離開，不會執行。`duduclaw migrate-from <平台>` 改用 `duduclaw migrate from <平台>`；`duduclaw audit` 改用 `duduclaw export audit`；`duduclaw gdpr export <聯絡人>` 改用 `duduclaw export gdpr <聯絡人>`；`duduclaw playbook export --agent <員工>` 改用 `duduclaw export playbook --agent <員工>`；`duduclaw acp-server` 改用 `duduclaw acp server`。請檢查你的腳本、cron 與 systemd unit。
- **`duduclaw expert install`**：改用 `duduclaw pack install <來源>`，舊拼法同樣印訊息並以結束碼 2 離開。兩者跑同一條安裝流程，安裝流程與結果不變。儀表板的一鍵安裝、上傳安裝、AI 草稿安裝改走 `pack install`。
- **`config.toml [dispatch] judge` 的 `evaluator_only` 與 `human_only`**（含別名 `evaluator`、`human`）：有效值只剩 `mav`（預設）與 `external`，儀表板與 `system.update_config` 拒絕寫入這兩個值。設定檔裡還留著舊值時：`evaluator_only` 改用 `mav` 驗收（驗收變嚴、判官費用增加）；`human_only` 不會退回機器驗收，每件送驗的工作都停在 `needs_human`（暫停原因顯示為系統問題）並附改法，操作者可以直接按「標記完成」，或改好設定後按「重試」（重試把任務放回 `pending`，清掉已存的結果摘要與認領，輪次計數接著算，已寫出的檔案不會被刪除）。兩者每個行程警示一次；每個閘道行程、每個資料目錄各寫一次稽核事件 `judge_mode_removed` 與一筆 Activity Feed 通知，時間點是第一件工作進入驗收時，閘道重啟後會再寫一次；稽核事件 `judge_mode_deprecated` 不再產生。`duduclaw doctor` 會列出這個狀況：`human_only` 顯示為失敗（每件送驗的工作都會停住），`evaluator_only` 顯示為警告，`config.toml` 讀不到或解析失敗時顯示警告，說明未能檢查。需要人工把關的員工，改用 `[capabilities] autonomy_level` 與 `approval_required_tools`。

### Fixed
- **任務詳情「變更」分頁看不到共享知識庫的寫入**：共享知識庫寫入的稽核列沒有 `input` 欄（只有 `params_summary = "path=<頁面> size=<位元組>"` 與列層的 `scope`），而過濾器只讀 `input`，所以舊名稱 `shared_wiki_write` 與新寫法 `wiki_write`（`scope="shared"`）的寫入從來沒有出現在分頁裡。現在兩種稽核列都讀得到；寫自己 wiki 的 `wiki_write`（`scope` 省略或 `agent`）不是變更。
- **`[ccr] verified_wiki_routes` 開啟時，帶 `scope` 參數的 `wiki_read` 結果被整筆扣住**：驗證器只接受 `page_path` 與 `agent_id`。現在 `scope` 省略、`agent`、`local` 照原本的路徑驗證；`scope="shared"` 不經壓縮、原樣交給模型（舊的 `shared_wiki_read` 本來就不走這條路）；其他值仍然拒絕。這個功能是實驗性的，預設關閉。
- **已移除工具名稱的專屬回應移到速率限制與輸入掃描之後**：原本任何通過認證的呼叫者可以不限次數呼叫已移除的名稱，每次寫一列 `tool_calls.jsonl`。
- **`duduclaw doctor` 的判官檢查分得出「確認沒有使用」與「未能檢查」**：`config.toml` 讀不到（權限等）、解析失敗、`[dispatch] judge` 不是字串時，原本顯示正常，現在顯示警告並說明原因。
- **Windows：並行閘把「鎖被別人佔著」當成 I/O 錯誤**（`crates/duduclaw-core/src/concurrency_gate.rs`）：Windows 上鎖衝突回的是 `ERROR_LOCK_VIOLATION`，不是 `WouldBlock`，所以兩個重疊的租約續期可能讓操作者的租約失效。現在用和因果儲存相同的方式辨識鎖衝突。
- **Windows：`[container.sandbox] executables` 的每個合法路徑都被拒絕**：這些項目是容器映像內的路徑（`/usr/bin/...`），之前卻套用主機的路徑規則驗證；現在一律當 POSIX 路徑驗證。Discovery 的評估器與策略容器路徑在所有主機上都用 `/` 組合。
- **非 Unix 主機上的 Discovery 會直說原因**：回報 `discovery is unavailable on this platform…`，不再誤報成資料庫資料列損毀；行為（拒絕執行）不變。
- **儀表板設定頁存檔會把 `config.toml` 重排、丟掉註解**（`crates/duduclaw-gateway/src/handlers/config_commit.rs`）：`system.update_config`、常駐感知資料來源、通道、帳號、Odoo、委派、身分、推理等設定 RPC 都是把整份檔案讀成 `toml::Table`、改完再用 `toml::to_string_pretty` 整份寫回，操作者寫的註解全部消失，鍵的順序也被打亂（只有設定檔進階編輯是原文寫回）。現在兩個共用寫入點 `commit_table_locked`（`system.update_config`、`tick.sources.*`）與 `write_config_table`（其餘設定頁，也涵蓋 `inference.toml`、`KILLSWITCH.toml`、`CONTRACT.toml`）改用 `toml_edit` 原地修改：只動有變更的鍵，沒動到的鍵連同上方註解、行尾註解、空行、順序、inline table 與陣列寫法逐位元保留；改值的那一行保留行尾註解；新鍵加在所屬區段末尾，新區段加在檔案最後；`[[tick.sources]]`、`[[accounts]]` 這類陣列表格與 `*_enc` 加密值沒被指定就原樣不動。解析失敗照舊拒絕寫入，鎖檔、內容雜湊比對、owner-only 寫入、`PROTECTED_KEYS` 稽核與 `restart_required` 都沒變。繞過共用寫入點的三處（憑證清理、語音設定 HTTP 端點、開機時的內部 MCP 金鑰輪替）一併改成同一套；語音設定端點順帶補上鎖檔與雜湊比對。改寫結果讀回來若和預期的表格不一致，退回整份重新序列化（值一定正確，只是不保留排版）並記 warn。
- **Windows：`files.allowed_roots` 驗證測試用了 Unix 路徑**：`/srv/a` 在 Windows 沒有磁碟代號，不算絕對路徑，測試因此失敗；改用各平台各自的絕對路徑，Windows 另驗 `C:\` 磁碟根目錄會被拒。程式行為不變。
- **信念校準把 AI 員工自行回報的結算當成已驗證**。之前命中率、Wilson 下界、Brier 與過度自信用所有已結算的信念計算，而正式環境沒有任何路徑做交叉驗證：`belief_settle` 不帶比對價格，gateway 也沒有對 TickHub 結算的程式，所以這些數字全是自報。現在只有 `settle_source` 完全等於 `agent+tick_verified` 的結算計入校準，30 筆門檻也以已驗證筆數計；`agent_unverified`、未知值、空值與舊資料的缺值都算自報，另外計數。因為目前沒有驗證路徑，現有部署的每一筆結算都是自報，校準狀態會顯示「沒有已驗證的結算」。gateway 端對 TickHub 結算尚未實作。
- **`agent-file-guard` 在非預設資料目錄的部署形同沒有作用在資料目錄上**。gateway 啟動員工 CLI 時清空環境變數，白名單沒有 `DUDUCLAW_HOME`，hook 一直以 `$HOME/.duduclaw` 判斷路徑，資料目錄不在那裡的部署，所有路徑都被當成資料目錄以外。現在由安裝器把資料目錄寫進 hook 指令（見 Changed）。
- **`agent-file-guard` 在受保護檔存在但讀不到時放行**。`agent.toml`、`config.toml`、`.mcp.json` 讀取失敗會被當成新檔，比對不到受保護欄位；現在只有檔案不存在才算新檔，其他讀取錯誤一律拒絕。
- **Bash 指令把輸出丟到 `/dev/null`（`2>/dev/null`）或複製檔案描述元（`2>&1`）時被當成寫入**，提到受保護檔名的唯讀指令因此被擋。現在這兩種重導不算寫入；重導到檔案照樣算。

### Security
- **`duduclaw secaudit --profile deep` 的檔案路徑驗證**（已發布版本受影響）：對抗式覆核與 PoC 步驟會把模型回傳的 `file` 直接接在 repository 根目錄後面，沒有驗證，絕對路徑可以讓讀檔跑到 repository 外面。現在每個模型回傳的路徑在開檔前都先通過 `SafeRepoPath` 驗證（只允許相對路徑，拒絕 `..`、磁碟機與 UNC 前綴、反斜線、控制字元、Windows 保留名稱）。
- **MCP 工具會改動或觸發其他 AI 員工的紀錄，沒有檢查呼叫者與擁有者的關係**（已發布的版本皆受影響）。`tasks_create` 與 `schedule_task` 建立時會檢查委派關係，之後的修改路徑卻沒有，任何員工都能改寫別的部門的任務、暫停或手動觸發別人的例行工作，或建立喚醒別的員工的提醒。現在 `tasks_update`、`tasks_claim`、`tasks_complete`、`tasks_block`、帶 `task_id` 的 `activity_post`、`update_cron_task`、`delete_cron_task`、`pause_cron_task`、`run_cron_task`、`create_reminder` 共用同一條規則（`crates/duduclaw-cli/src/mcp/record_authz.rs`）：操作者（金鑰不對應任何 AI 員工、行程也沒有員工身分）不受限；呼叫者就是擁有者則允許（任務的擁有者是受派者、認領者與建立者，`tasks_complete`／`tasks_block` 只認受派者與認領者；cron 是該列的執行者，`default` 解析為主員工；提醒是 `agent_id`，省略時是呼叫者自己）；否則要通過既有的委派述詞（同部門、`reports_to` 上下級、白名單配對，依 `[delegation] policy`）；擁有者或呼叫者身分不明一律拒絕。每次拒絕都記稽核（委派述詞拒絕記 `delegation_denied`，其餘記 `tool_calls.jsonl`，reason 為 `owner_unknown`、`caller_unknown` 等）。`tasks_claim` 認領指派給別人的任務、`tasks_update` 把別人的任務改派給自己，也要有這層關係。
- **系統 sender 名稱可以當成 MCP 行程的身分**（已發布的版本皆受影響）。`dashboard`、`cron`、`goal-loop-driver`、`heartbeat`、`autopilot`、`webhook` 在委派述詞裡無條件放行，而 MCP 行程的身分可以自行宣稱成這些名稱。MCP 的 `create_agent` 工具、儀表板與 `duduclaw agent create` 指令建立員工時都拒絕這些名稱，gateway 啟動員工時一律寫入員工目錄 id，所以在沒有員工使用這些名稱的情況下，只有自行宣稱的身分會帶著它們。現在身分解析把系統 sender 名稱視為不受信任，紀錄檢查也直接拒絕；替換時在 `tool_calls.jsonl` 留一筆（工具 `mcp_identity`、reason `system_sender_identity`，記下被宣稱的名稱與來源 `env`／`config`），同一個行程只記一次。
- **AI 員工可以改寫控制任務行為的欄位**（已發布的版本皆受影響）。goal 任務（`goal_mode`）的 `title`、`description` 是判官讀的目標，現在與 `acceptance_criteria` 一樣對 AI 員工凍結（稽核 reason `goal_contract_frozen`）；任何任務上，AI 員工不能新增、移除或調換 `outcome:` 開頭、`grant:` 開頭與 `auto-research` 這幾個控制用 tag（`tasks_update` 比對前後順序，`tasks_create` 不接受呼叫者在 `tags` 帶入；`kind = "goal"` 由伺服器自己產生的驗收契約 tag 不受影響；稽核 reason `reserved_tag_change`）。
- **AI 員工可以用 `agent_update` 放寬自己的權限**（已發布的版本皆受影響）：組織閘對「改自己」直接放行，員工因此能替自己加資料庫授權、調高預算、把自己的 `role` 改成 `main`。見 Changed。
- **AI 員工可以直接寫入 DuDuClaw 資料目錄裡的稽核紀錄、評測集與資料庫**（已發布的版本皆受影響）。`agent-file-guard` 之前只擋別的員工目錄、`config.toml` 與幾個組織／身分檔名，`tool_calls.jsonl`（判官、grounding 與近期行動區段讀的證據）、`evals/`（含留出集與別的員工的評測集）、各 SQLite 資料庫、斷路器狀態、授權與組織檔、全域 skills、共用 wiki 都可以用 Write／Edit／Bash 改。現在 AI 員工身分的呼叫者在資料目錄底下只能寫自己的員工目錄與 `attachments/`（允許清單，新增的檔案預設就受保護）；只有沒有員工身分的呼叫者不受影響（hook 指令沒帶 `--agent`、環境裡也沒有 `DUDUCLAW_AGENT_ID`）；安裝的 hook 指令一定帶 `--agent`，所以操作者在員工目錄裡手動執行 `claude` 會以該員工的身分判定，要改這些檔案請用儀表板或直接用編輯器。同時：Write／Edit 以符號連結解析後的實際落點判斷，無法確認落點的路徑拒絕；`NotebookEdit` 納入涵蓋；Write／Edit 的相對路徑以 hook 輸入的工作目錄解析，沒有工作目錄時以員工自己的目錄解析。
- **AI 員工可以改自己 `agent.toml` 裡安全檢查讀取的設定**（已發布的版本皆受影響），例如權限旗標、沙箱、預算、執行環境與外部 MCP server。現在對 AI 員工的 Write／Edit 採允許清單：只有列為可編輯的區段可改，其餘區段一律凍結，日後新增的區段預設受保護；可編輯區段裡的 `[agent] role`、`[prompt] cli_bare_mode`、`[model] account_pool` 也凍結。操作者在員工目錄裡手動執行 `claude` 也會以該員工的身分判定，要改這些設定請用儀表板或直接用編輯器。可編輯清單見 `docs/features/05-security-defense.md`。
- **身分驗證失敗的呼叫者在 Bash 通道上不受限**：Write／Edit 早已拒絕它寫入 `agents/` 下任何位置，Bash 卻只擋別人的目錄。現在兩條通道一致。Bash 通道仍是減速帶：它比對指令文字，由變數、指令替換或其他計算產生的路徑、編碼後的指令、先寫成腳本再執行、把 here-document 餵給直譯器、別名與函式、透過環境變數讓之後啟動的 shell 載入檔案、清單內唯讀指令沒有被考慮到的寫檔選項、沒有寫明目的地的解壓縮與下載指令（寫進目前的工作目錄，這條通道不判斷目前目錄，先切換進資料目錄再執行就不會被擋）、在同一條指令裡先建立連結再寫入、硬連結，以及檢查到實際執行之間的時間差，都擋不住；反過來，受檢位置上的路徑解析失敗（含懸空連結）一律拒絕，資料目錄以外的路徑也會被誤擋；Read 不在 hook 涵蓋範圍，留出集與稽核紀錄仍可讀取；Claude 以外的 runtime 不跑這個 hook；員工自己目錄裡的狀態檔與共用的 `attachments/` 不在保護範圍。


## [1.68.1] - 2026-10-03 — 工具清單萬用字元修正（v1.67.0 起 mcp__duduclaw__* 讓平台工具全被拒）

### Fixed
- **`allowed_tools` 寫成 `mcp__duduclaw__*` 的 AI 員工，所有平台工具都被拒絕**（影響 v1.67.0、v1.67.1、v1.68.0）。v1.67.0 起 MCP 閘門改用實際執行的員工身分查 `[capabilities]`，而清單比對只做完整名稱相等，`mcp__duduclaw__*` 被當成名為 `*` 的工具，什麼都對不上，每次呼叫都回「不在此代理的 allowed_tools 允許清單中」。我們自己的文件與付費範本都用這個寫法。現在清單項目照 Claude CLI 的規則比對，而且一律從開頭錨定：`*` 對應所有工具；`mcp__duduclaw__*`（或 `mcp__duduclaw`）對應所有 DuDuClaw 工具；結尾的 `*` 代表前綴（`mcp__duduclaw__odoo_*`、`memory_*`，`memory_*` 不會對到 `agent_memory_x`）；`*` 出現在其他位置只當一般字元；`mcp__<其他伺服器>__…` 不會對到任何 DuDuClaw 工具（舊的比對會把 `mcp__masterlink__foo` 當成 DuDuClaw 的 `foo`）。`denied_tools` 仍優先。MCP 派送閘門、`tools/list` 可見性、電腦操作工具、API 模式的工具清單、`scoped_tools`、`approval_required_tools`／`irreversible_tools`／`maybe_irreversible_tools` 與安全頁的權限摘要都改用同一個比對函式。傳給 Claude CLI 的 `--allowedTools`／`--disallowedTools` 內容不變。

### Changed
- **macOS 桌面版自本版恢復提供**（Apple 晶片與 Intel 各一個 `.dmg`，已簽章並通過 Apple 公證）。v1.67.0 到 v1.68.0 沒有 macOS 桌面版，這幾版只有 Windows 與 Linux。已安裝的 macOS 桌面版可經自動更新升到 v1.68.1。README 桌面版表格的註記同步更新（三語）。

## [1.68.0] - 2026-10-03 — 儀表板開關全面補齊×存檔無作用欄位修復×記憶命名空間統一×權限旗標生效×通道管理指令與管理權杖安全修補

### Added
- **儀表板補上之前只能手改設定檔的開關**。完整對照（頁面、設定鍵、是否需要重啟）見 `docs/guides/dashboard-settings.md`（三語）。系統設定 → 進階設定 → 自動化引擎：派工策略 `role_team`、驗收判官的執行環境與模型（`[dispatch] judge_provider`／`judge_model`）、夜間整理模型階段（`[night] llm_enabled`），以及真人接手（`[takeover]`）、AI 員工信箱（`[mail]`）、一員工四角色全域預設（`[team]`）、常駐感知（`[tick]`）四張卡片。系統分頁：任務沙箱（`[container.sandbox]` 全部欄位，整段驗證）、電腦操作映像（`[computer_use] image`）、記憶可信度守門、OpenTelemetry 送往位址（只在含 `otel` 的版本顯示，`system.status` 新增 `otel_compiled`）、GitHub 工具開關（可關）、信任外部 A2A 請求（`[acp] trusted`，只限管理員）、地端檔案根目錄（`[files] allowed_roots`）、日誌格式、1Password／Infisical 密鑰後端欄位。通道管理：網站聊天元件（`[webchat] public_widget`／`widget_key`）。本地推理：llamafile 欄位、信心路由進階（`local_tools`、`ucci_*`）、`capture_logprobs`／`capture_top_logprobs`。AI 員工編輯頁：每日花費上限、四份需要把關的工具清單、平行分支、出站內容防護、推理力度、七個新執行環境（qwen、kimi、copilot、kiro、cursor、vibe、opencode）、員工層級的一員工四角色、精簡啟動內容、決策延續、夜間整理，以及帶型別的進階鍵值編輯器。
- **常駐感知資料來源可以在儀表板新增、編輯、刪除**：新 RPC `tick.sources.list`／`upsert`／`remove`（管理員）。存檔後 gateway 已載入常駐感知時會立即重新啟動資料來源，否則回報需要重啟。`headers` 的值不回傳，只回傳數量。
- **設定檔進階編輯**（系統設定 → 進階設定，只限管理員，RPC `config.raw.get`／`config.raw.set`）：直接編輯 `config.toml`、`inference.toml` 與各員工的 `agent.toml`。密鑰類的值顯示為 `«set»`，原樣保留就保留原值；先檢查 TOML 語法（錯誤指出行與欄）再用對應型別驗證，失敗不寫入；寫入前備份 `<檔名>.bak-<時間>`（保留 5 份）；以開啟時的雜湊值偵測同時修改；每次寫入記稽核 `config_raw_edited`；回傳需要重啟的部分。
- **需要重啟的設定會說清楚**：`system.update_config` 回傳新的 `restart_required` 清單，系統設定頁與通道管理頁顯示提示，gateway 重啟後自動消失。
- **`duduclaw memory migrate-namespace`**（隱藏指令，只限操作者終端機）：`list`、`export`、`assign`、`archive`，用來處理 v1.68.0 以前留在共用記憶池的資料，見 Changed 的記憶命名空間一項。

### Changed
- **行為變更：MCP 記憶工具改用員工自己的命名空間**。gateway 啟動的員工以內部金鑰呼叫記憶工具（`memory_*`、`user_profile_record`／`user_profile_get`、`user_code_profile`）時，若 `DUDUCLAW_AGENT_ID` 通過 `DUDUCLAW_AGENT_TOKEN` 驗證（`identity.key` 存在時 gateway 會寫進 `.mcp.json`），就讀寫該員工 id 的命名空間，也就是 gateway 萃取、核准審核、注入提示所用的那一個。身分無法驗證時維持舊的共用池 `internal/gateway-internal`；HTTP 傳輸一律不沿用行程環境的身分；client id 是現有員工的個別金鑰對應到該員工。v1.68.0 以前寫進共用池的資料不會自動搬移，升級後員工透過工具看不到它們，由操作者用 `duduclaw memory migrate-namespace` 搬：`list` 依 `tool_calls.jsonl` 列出每筆的作者，`export --out` 匯出（0600），`assign --to <員工> --all|--ids|--attributed [--include-inferred] [--refused hold|skip] [--include-flagged]` 搬移（看起來像提示注入的資料預設不搬，要加 `--include-flagged`；不加 `--confirm` 只是預演；被目標中更可信的事實擋下的預設轉為待審並建立收件匣項目），`archive --confirm` 讓剩下的失效並加標籤 `namespace-archived`；實際執行記稽核 `memory_namespace_migrated`；在 AI 員工的工作階段中拒絕執行。說明見 `docs/features/20-memory-intelligence.md`。
- **行為變更：`[permissions]` 四個旗標開始生效**。`can_create_agents`、`can_send_cross_agent`、`can_modify_own_skills`、`can_schedule_tasks` 寫成 `false` 時，MCP 分派閘會拒絕對應工具（`create_agent`；`send_to_agent`／`spawn_agent`；`schedule_task`／`create_reminder`／帶 `schedule` 的 `tasks_create`；`skill_hub_install`／`shared_skill_adopt`／`skill_graduate`／`skill_pin`／`skill_from_recording`），回 -32003 並記 `permission_denied`。沒寫或型別錯誤時放行，`agent.toml` 存在但讀不到或無法解析時拒絕，外部金鑰不受影響。因為以前的範本常寫 `false`，升級後第一次開機會對每位員工遷移一次：`[permissions]` 裡沒有 `permissions_enforced_since` 的檔案，四個旗標的 `false` 一律改成 `true` 並加上 `permissions_enforced_since = "1.68.0"`（稽核 `permission_flags_reset`）。臨時角色成員（`agents/.ephemeral/`）不遷移，它們範本裡的 `false` 從此真的生效。新建員工與內建範本改寫 `true`。
- **行為變更：Odoo 的 `features_*` 開關開始生效**。`odoo_search`／`odoo_execute`／`odoo_schema_fields` 與各模組工具在模組關閉時拒絕呼叫（模型前綴對應：`crm`、`sale`、`stock`／`product`、`account`、`project`、`hr`；`res.partner` 等其他模型不受限）。預設 crm、sale、inventory、accounting 開，**project 與 hr 關**，所以沒寫 `features_project`／`features_hr` 的既有安裝不能再查 `project.*`／`hr.*`。工具仍出現在 `tools/list`。`[odoo] protocol = "xmlrpc"` 存檔時會被拒絕，只支援 JSON-RPC。
- **行為變更：Discord「指定 AI 員工」（`agent_override`）的全域設定開始生效**，伺服器層級的值優先，沒有時用全域值，私訊也套用；個別員工綁定的 bot 仍然優先。
- **行為變更：`agents.update` 只寫收到的欄位**。之前編輯頁每次存檔都會把 `[heartbeat] cron` 清空（`agents.inspect` 沒回傳它），一些分段也會用預設值蓋掉已存的設定。`agents.inspect` 現在回傳 `heartbeat.cron`。
- **行為變更：權限類欄位只限管理員**。`agents.update` 改動 `[agent] reports_to`／`department`／`name`、整個 `[capabilities]`、`sandbox_enabled`、`network_access`、`can_modify_own_soul` 時，非管理員會被拒絕，成功的改動記稽核 `agent_authority_changed`，被拒絕的嘗試記 `agent_authority_refused`；`org.toml` 只在 `reports_to` 或 `department` 真的變了才更新。
- **進階鍵值編輯器改為帶型別**（`{section, key, value, type}`），寫入前用完整的 `AgentConfig` 解析，失敗整筆拒絕。之前所有值都存成字串，可能讓員工因解析失敗從清單消失。不能編輯 `agent`、`capabilities`、`container`、`permissions`、`channels`、`odoo`、`mcp`、`runtime` 區段。
- **設定改動不必重啟**：`inference.update` 存檔後重設推理引擎；輪替策略與限流冷卻存檔後清掉輪替快取（之前最多延遲 30 分鐘）；日誌等級透過 reload handle 立即套用（`RUST_LOG` 已設時除外）；WhatsApp、飛書、Google Chat、Teams、企業微信、釘釘的六條 webhook 路由一律掛著，通道設定好之前回 404，設定好之後驗證平台簽章（簽章錯誤回 401）；`channels.add` 不必重啟就啟動通道（之前要重啟，平台送來的請求得到 404），`channels.add` 回 `restart_required: false` 與 `not_started_reason`；個別員工的 Slack bot 新增後立即啟動。
- **`system.update_config` 與 `tick.sources.*`、`config.raw.set` 寫入前檢查檔案**：`config.toml` 無法解析時拒絕寫入（之前會當成空表，存檔後只剩這次送的鍵）；寫入時鎖檔並比對讀取時的雜湊，期間被其他寫入者改過就拒絕。其他設定 RPC 仍沿用舊的讀取方式。
- **行為變更：安裝需要環境變數的 MCP 卡片時要一併填入值**。`marketplace.install` 多了 `env` 參數（`{ "變數名": "值" }`），卡片 `required_env` 列出的每個變數都要有非空的值；缺少、空字串或寫成 `${NAME}` 參照都會被拒絕，訊息列出缺少的變數名稱（不含任何值）。不在 `required_env` 裡的變數名稱也會被拒絕。`mcp.update` 以卡片 id 安裝同一張卡片時套用同樣的檢查，值放在 `server_def.env`。值以字面字串寫進該 AI 員工的 `.mcp.json`（檔案權限 0600，與 `claude mcp add -e` 的存法相同），不寫進記錄。目錄裡的 `default_def.env` 改成空字串佔位。
- **`mcp.list` 不再回傳環境變數的值**：每個 server 的 `env` 改成 `{ "變數名": "set" | "not_set" | "reference" }`（`reference` 代表 `${NAME}` 參照），目錄卡片的 `default_def.env` 也一樣。之前會原樣回傳，包括 AI 員工 `.mcp.json` 裡的金鑰。

### Deprecated
- **v1.66.0 棄用項目的移除時程順延一個 minor，由 v1.68.0 改為 v1.69.0**：`shared_wiki_*`、`schedule_task`、`skill_bank_search`、舊 CLI 寫法、三種舊包格式與 `expert install`／`expert list`、`[dispatch] judge` 的 `evaluator_only`／`human_only` 都延到 v1.69.0 才移除，與 Gemini CLI runtime 同版；目的是讓 v1.68.0 維持純功能版本。本版不移除任何東西，各舊名稱照常可用，對照表見 `docs/guides/deprecations.md`。

### Removed
- **存檔後沒有作用的儀表板欄位**（已存的值留在檔案裡，不再被讀取）：語音的 `asr_provider`／`asr_language`／`voice_reply_enabled`；推理的 `max_memory_mb`、`[generation] gpu_layers`／`context_size`（llamafile 改用 `[llamafile]` 的同名欄位）、`[embedding]`；員工的 `[model.local] backend`／`context_length`／`gpu_layers`、貼圖卡片、停滯偵測卡片、`skill_security_scan`／`skill_auto_activate`、容器 `cmd`／`env`／掛載／`readonly_project`／`max_concurrent`、`token_budget_per_check`、`check_interval`；員工層級的 LINE／WhatsApp／飛書／企業微信／釘釘憑證（這五個通道只讀 `config.toml [channels]`；Discord、Telegram、Slack 的員工 bot 不變）；去識別化「資料來源保護」的 `sub_agent`；Odoo 頁的通訊協定選單；KILLSWITCH 的 `[audit]` 區段。進階鍵值編輯器拒絕寫入 `[sticker]`、`[ptc]`、`[cultural_context]`。
- **沒有讀取端的設定解析**：`config.toml [secaudit]`（`secaudit_config.rs`）、員工 `[memory]` 的 MemGPT 殘留鍵（`enabled`、`core_tokens`、`recall_tokens`、`archival_tokens`、`recall_auto_inject`、`archival_auto_retrieve`）、`[task_forward_model] cold_start_llm`／`min_samples`／`mature_n`。舊檔案照常載入，這些鍵被忽略。
- **新建的員工（儀表板、MCP 與 CLI 建立路徑）不再寫入 `micro_reflection`／`meso_reflection`／`macro_reflection`**：沒有程式讀取這三個鍵；既有員工檔案裡的值保持原樣。
- **永遠失敗的瀏覽器審批視窗**（`ApprovalModal.tsx`）：它呼叫的 `browser.respond_approval` 在 gateway 沒有處理函式。審批一律走收件匣。

### Fixed
- **Browserbase 卡片裝好後拿不到金鑰**：卡片寫進 `.mcp.json` 的是 `${BROWSERBASE_API_KEY}` 這類參照。Claude CLI 從自己的環境展開參照，而 gateway 啟動 AI 員工的 CLI 時只保留白名單環境變數（`*_API_KEY` 一律不帶），參照展開成空字串，server 沒有金鑰就啟動。現在安裝時填入的值會直接寫進 `.mcp.json`（見 Changed）。
- **`duduclaw doctor` 在 Docker 半掛時仍說「Docker daemon is reachable」**：之前只看 `/_ping`。2026-10-03 的 Docker Desktop 回應了 ping，`/info` 與 `docker ps` 卻回 EOF，同一次 doctor 的任務沙箱與電腦操作兩列則說無法連線。現在 doctor 的 Docker 列、`duduclaw status`、任務沙箱、電腦操作（doctor 與開始工作階段）都用同一個檢查：`docker info` 要回伺服器版本、`docker ps` 要成功，各 5 秒逾時，結果共用 5 秒；EOF、空回應、逾時都算無法使用。腳本沙箱（PTC `execute_program`、`secaudit` PoC）用 Docker API 走同一套判定（`info` ＋ 列容器）。電腦操作在 Docker 半掛時改回報「Docker 沒有回應」，之前會誤報 image 不在本機、叫人 `docker pull`。儀表板「診斷」頁的容器執行環境列仍用舊的 `docker info` 檢查，可能與 CLI 結果不同。
- **Windows：live fork 結果發佈前的復原備份一律失敗**：`fork_recovery` 備份分支檔案後會逐檔 flush，Windows 上用唯讀 handle 呼叫 `FlushFileBuffers` 會被拒絕，發佈因此在寫入上層工作區之前中止。改用可寫 handle flush（帶唯讀屬性的備份檔暫時取消屬性、flush 後還原）。這是 v1.67.0 已知問題裡四個只在 Windows CI 失敗的 `mcp_fork_exec` 測試的原因；修正是否完整要等 Windows CI 跑過才知道。
- **存了沒作用的儀表板欄位，現在接上讀取端**：
  - 安全設定的緊急停止門檻（`KILLSWITCH.toml [triggers]`）：只有檔案裡寫了、且在範圍內的鍵才生效，儀表板每個門檻多了啟用勾選，送 `null` 會取消該門檻；檔案修改時間變了就重讀。`cost_limit_usd` 比對 24 小時全域花費，達到時把全域降到 L2，之後要等 failsafe 自動恢復或有人 `!RESUME` 才解除；`max_replies_per_minute` 依對話計算，超出的訊息靜默丟棄；`max_consecutive_errors` 與 `error_rate_threshold`（最近 20 次、至少 10 次）讓該對話的 failsafe 升一級。`killswitch.get` 回傳 `triggers_enforced`。
  - 去識別化的資料來源保護：`user_input`（送進 AI 前）、`system_prompt`（組好的提示，預設只套用標了 `apply_to_system_prompt` 的規則）、`cron_context`（條件腳本的觸發訊息）。出錯時停止這一輪。`purge_after_expire_days` 接上保管庫清理（之前固定 30 天）。
  - Telegram、LINE 語音訊息改用儀表板的語音轉文字設定（`[voice] stt_*`），沒設定時才退回 `OPENAI_API_KEY` 的 Whisper；已設定但失敗時不退回。
  - Discord `thread_archive_minutes`（60、1440、4320、10080，其他值當 1440）。
  - 飛書、釘釘的 `mention_only` 與 `allowed_channels`（私訊不過濾）。WhatsApp 與企業微信沒有這兩項，儀表板也不再顯示。
  - 帳號 `tags`，`account_pool` 也能用標籤比對（完全相同、不分大小寫）。
  - 品牌副標題顯示在登入頁與安全設定頁。
  - `[identity.notion] refresh_seconds`（快取查詢結果，0 不快取）。
  - `[memory] graph_embed_seed`／`graph_embed_seed_top_k` 套用到員工的記憶檢索（之前只影響儀表板自己的搜尋）。
  - `[logging] format = "json"`（重啟後 stderr 與檔案日誌輸出 JSON）。
- **帳號 e-mail 遮罩**：非 ASCII 的 e-mail 本地部分不再造成 panic（改用字元計算）。

### Security
- **AI 員工 `.mcp.json` 寫入時就是 0600**：`add_server_to_config`／`remove_server_from_config` 先建立權限 0600 的暫存檔再寫入、改名，之前暫存檔以預設權限寫完才改權限，中間有一段時間其他使用者可讀。
- **六個 webhook 通道與 WebChat 讓任何人執行管理指令**（已發布版本受影響）：WhatsApp、飛書、Teams、企業微信、Google Chat、釘釘呼叫聊天指令時把 `is_admin` 寫死成 true，任何傳訊者都能執行 `!STOP`、`!STOP ALL`、`!RESUME`、`/model`。現在依該通道的 `admin_users` 設定比對傳訊者或對話 id（完全相同），沒設定就沒有人是管理員；Google Chat 與 Teams 也能設定 `admin_users` 了。WebChat 只有啟用中的儀表板管理員帳號算管理員，網站元件訪客一律不算。
- **儀表板設定的管理權杖重啟後消失**（v1.22.0 起的版本受影響）：儀表板把 `[gateway] auth_token` 加密存成 `auth_token_enc`，啟動時卻只讀明文，重啟後 gateway 沒有管理權杖。現在依序讀環境變數 `DUDUCLAW_AUTH_TOKEN`、`auth_token_enc`、明文 `auth_token`（也接受 `secret://` 參照）。
- **`agents.update` 讓非管理員擁有者改動權限欄位且不留紀錄**（已發布版本受影響）：見 Changed 的權限類欄位一項。
- **`acp.trusted`、`tick.allow_command_sources`、`container.sandbox.when_unavailable`、`script_when_unavailable`、`memory.supersession_trust_guard` 的每次改動記稽核 `config_protected_key_changed`**（含前後值）。

## [1.67.1] - 2026-10-03 — 記憶可信度守門×自動化規則表單×中文注入掃描×Marketplace 與推論設定修正

### Added
- **自動化規則可以在儀表板編輯**：「設定 → 自動化」的每條規則多了編輯按鈕。編輯時只送出有改動的欄位；表單無法攤成清單的條件、事件序列規則、以及委派／通知／執行技能以外的動作會顯示為鎖定並原樣保留，`screen`、`context_ticks` 等表單不管的動作欄位也原樣保留。

### Changed
- **行為變更：沒有條件的自動化規則現在每次事件都會執行**。建立規則時沒帶 `conditions` 會存成 `{}`；之前引擎把 `{}` 當成欄位為空的條件，這種規則從來沒觸發過。現在 `{}`、`null`、沒帶 `conditions`、`{ "all": [] }` 都代表「觸發事件每次發生都執行」（`{ "any": [] }` 仍然永遠不匹配）。升級後原本不會動的規則可能開始執行，請先在 gateway 主機查出已啟用的這類規則，不想要的停用或補上條件：`sqlite3 ~/.duduclaw/autopilot.db "SELECT id, name, trigger_event FROM autopilot_rules WHERE enabled = 1 AND sequence IS NULL AND trim(conditions) = '{}';"`。在儀表板用編輯按鈕打開這種規則，會顯示「目前沒有設定條件」。說明見 `docs/features/23-autopilot-engine.md`。
- **自動化規則的條件在儲存時檢查**：`autopilot.create`、`autopilot.update` 與啟用歸納規則時，條件樹的每個節點必須是 `all`／`any` 群組或帶非空 `field` 的條件，`op` 必須是已知運算子，巢狀最多 16 層。不合格會拒絕儲存，訊息寫出出錯節點的路徑（例如 `conditions.all[1]`）。之前這種條件會被存下來，然後永遠不匹配。
- **行為變更：`cron_tick` 不能再用來建立新規則**：沒有任何程式送出這個事件，掛在上面的規則永遠不會觸發。`autopilot.create` 與規則歸納會拒絕它，訊息指向排程工作。`autopilot.update` 只在規則原本就是 `cron_tick` 時接受（已存的規則照常可以儲存），把其他規則改成 `cron_tick` 會和建立時一樣被拒絕。要定時執行請用「例行工作」頁或帶 `schedule` 的 `tasks_create`。
- **行為變更：本地推論設定只接受 `openai_compat`**：`inference.update`（儀表板推論頁）收到 `llama_cpp`、`mistral_rs` 等已移除的 `backend` 會在寫入前拒絕，訊息指向 `openai_compat`；空值會刪掉這個鍵，由引擎自動選用。檔案裡已經存的舊值原樣送回時照收，所以改掉它之前其他設定仍能儲存。推論頁的「推論後端」改成下拉選單（未指定／OpenAI 相容伺服器），舊值會標示「已停止支援」；選「未指定」儲存時送出空值，檔案裡的舊後端因此被移除；被拒絕時在頁面上顯示伺服器訊息。`[model.local] backend` 的預設值、`duduclaw onboard` 與 `duduclaw wizard`、內建的 evaluator／manufacturing／restaurant／trading 範本與 system-operator preset 改寫 `openai_compat`（之前寫 `llama_cpp`）。
- **行為變更：`user_profile` 來源的可信度上限由 1.0 降為 0.6**。`user_profile_record` MCP 工具（AI 員工記下的使用者資料）與使用者輪廓萃取（說話者描述自己，之前寫成 `channel`，0.3）現在都用 `user_profile` 類別、上限 0.6。兩者可以互相更正、不需審核，但都無法取代操作者核准過的值（1.0）或匯入的值（0.7）；工具說明註明 `origin_trust` 參數最多存成 0.6。舊資料列在比較時一律以 0.6 計。
- **爆量隔離的核准也經過可信度檢查**：同一來源一小時內對同一主題寫入 5 筆以上而被隔離的批次，核准時每筆照一般時序規則套用（之前核准只是清掉隔離旗標）；會和可信度更高的現有內容衝突的那幾筆，改成各自的審核項目，不直接套用。寫入當下就會被擋的爆量事實直接進審核項目。轉換時文字超過 600 字的不建立審核項目，只記稽核 `memory_supersession_refused`（`not_held_reason: "too_long"`）。部分失敗後再次核准同一個爆量項目，會補建還缺的衝突審核項目。
- **行為變更：知識審核只能在儀表板決定**。`knowledge_quarantine` 類審核推到聊天通道時只是一則沒有按鈕、不含說法內容的通知（不會送回說法來源的對話，見 Security）；按舊按鈕或用文字回覆決定都會被拒絕，並提示到儀表板處理。需要 manager 或 admin 角色。過了 24 小時期限的項目無法核准。同一個項目的核准與拒絕依序逐一處理；變更已套用但決定沒能記錄時（項目剛好逾期，或儲存發生錯誤），儀表板會收到說明已套用了什麼的錯誤，並寫入稽核 `knowledge_review_decision_unrecorded`。收件匣的知識審核不再顯示「在通道開啟」按鈕。
- **npm 套件說明**：`npm/duduclaw/package.json` 的 description 改為現況：Claude Code、Codex、Antigravity 等 AI CLI，249 個 MCP 工具、11 個訊息通道、單一 Rust 執行檔。Claude Code plugin marketplace 清單與 MCP registry 的 `server.json` 也把「200+」改成 249。

### Removed
- **儀表板上沒有程式讀取的欄位**：「設定 → 語音」的語音回覆模式、語音辨識、語言（`voice_reply_enabled`、`asr_provider`、`asr_language`）；推論頁的記憶體上限（`max_memory_mb`）與生成設定的 GPU Layers、Context 大小（`[generation] gpu_layers`／`context_size`）；AI 員工編輯頁本地模型區的 Context 長度與 GPU Layers（`[model.local] context_length`／`gpu_layers`）。這些值由外部推論伺服器自己管理，或根本沒有讀取端。已經存在設定檔裡的值保持原樣，頁面儲存時不再送出或改動它們。語音分頁保留文字轉語音的供應商與聲音（`tts_provider`／`tts_voice`，`POST /api/tts` 會讀），以及進階卡片的語音轉文字設定。
- **MCP 工具市集移除六張卡片**：GitHub、Slack、PostgreSQL、SQLite、Fetch、Brave Search。它們指向的套件不存在，或上游已不再維護。內建的替代：GitHub 用 `github_*` 工具（需在儀表板連接 GitHub），PostgreSQL／SQLite 用內建唯讀資料庫連接器（`db_*` 工具，需替 AI 員工授權資料來源），抓網頁用 `web_fetch_cached`／`web_extract`，搜尋用 `web_search`。Slack 沒有內建的替代工具。要繼續用這些伺服器，可以把定義寫進 `~/.duduclaw/marketplace.json` 或手動加入 `.mcp.json`。

### Fixed
- **儀表板建不出自動化規則**：「設定 → 自動化」的表單送出的動作欄位是 `agent_id`／`prompt_template`（伺服器要 `target_agent`／`prompt`），還提供不存在的 `schedule` 觸發事件，伺服器每次都拒絕。新表單照伺服器的格式送出：觸發事件是建立規則時接受的十二個事件（含 `tick` 監控來源與 `odoo_event` Odoo 資料變動）；條件是「欄位／比較方式／比較值」的列，可選全部符合或任一符合，`tick` 與 `odoo_event` 會提示欄位名稱的來源；動作是委派、通知（十個通道）、執行技能。儲存被拒絕時在對話框內顯示伺服器訊息。
- **MCP 工具市集的一鍵卡片指向不存在的 npm 套件**（`@anthropic-ai/mcp-server-*`）。目錄現在只有四張卡片：Playwright（`@playwright/mcp`）、Browserbase（`@browserbasehq/mcp`，需要 `BROWSERBASE_API_KEY`、`BROWSERBASE_PROJECT_ID`、`GEMINI_API_KEY`）、Filesystem（`@modelcontextprotocol/server-filesystem`）、Memory（`@modelcontextprotocol/server-memory`），皆以 `npx -y` 執行。程式內產生 Playwright／Browserbase 設定的函式也改用這兩個套件。已經安裝到 AI 員工 `.mcp.json` 的舊項目不會被自動改寫，CLI 啟動它時會失敗。仍在目錄中的四張卡片，在工具市集重新安裝會覆寫同名項目；已移除卡片留下的項目請在工具伺服器設定刪除，或手動修改。
- **個人版的整合頁不再顯示「身分解析」分頁**：個人版的 gateway 拒絕所有 `identity.*` 呼叫，這個分頁在個人版只會顯示錯誤。`?tab=identity` 的連結改開第一個分頁。
- **在聊天通道按知識審核按鈕沒有作用**：之前按下核准只會把審核標成已決定，隔離的知識一筆也沒釋放。現在這類審核只在儀表板決定（見 Changed）。
- **新增任務、新增子任務必須選負責的 AI 員工**：伺服器一直要求 `assigned_to`，對話框卻預設「未指派」，送出後建立失敗、沒有任何提示。現在沒選會提示，建立失敗時對話框保持開啟並顯示訊息。
- **收件匣的知識審核**：衝突項目以純文字並排顯示目前的值與內容、新的值與說法；決定失敗時顯示伺服器的錯誤，項目保留在清單上。
- **個人版通道設定的「成員」連結**：個人版沒有「成員」頁，通道詳細設定裡原本指向它的連結改成一句說明：把通訊帳號對應到使用者（真人接手需要這一步）是企業版功能。
- **「WeCom」通道名稱**：對話來源標籤補上 `wecom`，之前顯示原始代碼。
- **README 桌面版表格**：註明 macOS 桌面版最新是 v1.66.1；v1.67.0 的 macOS 版在 Apple 公證失敗，沒有發佈。
- **文件**：`personal-edition-portability.md` 三語版原本說 `agent.toml [edition] profile` 可以切換版本，沒有程式讀這個鍵；改為實際的判定順序（`DUDUCLAW_EDITION` 環境變數 → 授權方案 → 個人版）。真人接手文件註明通道綁定要在只有企業版才有的「成員」頁操作，個人版不會自動接手。瀏覽器自動化與開發指南的 Playwright 範例改用 `@playwright/mcp`。`web/package.json` 的授權欄位由 `ISC` 改成與專案一致的 `Apache-2.0`。`personal-edition-portability.md` 與 `56-team-as-agent.md` 三語版拿掉了指向未公開文件的連結。

### Security
- **行為變更：聊天內容不能再蓋掉更可信的記憶**（影響已發佈版本）。之前同一個 `(AI 員工, subject, predicate)` 的新事實一律取代目前事實，不看來源：從聊天萃取的事實（`channel`，可信度 0.3）會取代同一 subject 與 predicate 的現有事實，不論它的可信度，例如 AI 員工自己推得的事實或沒有記錄來源的舊資料（0.6），鍵值相同時也包括匯入的事實（0.7）；能跟 AI 員工對話的人都能影響它。操作者核准過的事實（1.0）從這個版本起才有（由下述的審核核准寫入），並受到保護。現在可信度嚴格較低的寫入不能取代目前事實；相同或更高照舊取代，舊值留在歷史裡。對話事實與使用者輪廓特徵這兩條自動萃取路徑，被擋下的說法會暫存起來，並在儀表板收件匣出現審核項目。項目只依存下來的說法產生，列出核准後會寫入的全部內容：完整的新說法與新值，並排目前內容（超過 600 字會截斷並註明）與目前的值；使用者輪廓會寫出是誰的輪廓。核准時寫入的正是這筆說法（以摘要值綁定），以操作者權限取代；拒絕就捨棄；說法或受保護的事實在送審後改變時，核准不寫入任何內容並說明情況已改變，同一說法再出現時會重新送審。套用失敗時項目維持待審，可以重試（之前會先記錄決定）。超過 600 字的說法不送審，只寫稽核（`not_held_reason: "too_long"`）。相同說法不重複送審；每位 AI 員工每個 UTC 日最多新增 20 筆，當天第一次超過時動態牆出現一則 `knowledge_review_cap_reached` 事件，超過的部分只寫稽核紀錄（`memory_supersession_refused`，`review_cap_hit`）；每天的清理會關閉審核項目已不在等待中的暫存資料。其他寫入路徑遇到拒絕時：`user_profile_record` 回傳錯誤（「a more trusted value already exists for this field, so it was not changed」），`migrate-from` 回報略過，足跡萃取、reflexion 規則與夜間整理略過該項目並記錄 log。資料主體的匯出與刪除涵蓋暫存資料；`duduclaw gdpr erase` 另外會撤回涵蓋被刪資料的待審項目、清掉審核紀錄裡這些項目（不論狀態）的文字、刪除對應事件；沒有帶資料列 id 的隔離事件會留到 7 天的事件保存期限後清除。前一步失敗時後面的步驟照樣執行，失敗會列出並以非零結束，提示重新執行同一個指令（可安全重複）；重新執行時即使已找不到記憶資料列，仍會用完全相同的 subject 從審核項目與事件中移除這個人的文字。限制：只比對完全相同的 subject 與 predicate，拼法不同的同一件事會並存。`config.toml [memory] supersession_trust_guard = false` 可恢復舊行為（gateway 經記憶工廠建立的引擎與 MCP server 會讀這個設定，其他直接建立的引擎一律開啟）。尚未在真實聊天通道上驗證。說明見 `docs/features/20-memory-intelligence.md` 與 `SECURITY.md`。
- **AI 員工不能再用 `memory_invalidate_by_origin` 清掉可信的記憶**（影響已發佈版本）。之前被操縱的 AI 員工一次呼叫就能讓它命名空間裡所有操作者等級的事實過期。現在以 AI 員工身分呼叫時，只能處理 `channel`、`mcp_external`、`tool_echo` 三個低於 agent 衍生上限的來源類別，其他類別會被拒絕並記入稽核 `memory_invalidate_refused`。判定採 fail-closed：使用 gateway 共用內部金鑰的呼叫者，不論行程裡有沒有員工身分，一律視為 AI 員工；屬於員工或臨時員工（`eph-` 開頭）的金鑰也一樣；只有對應不到任何員工的 admin 金鑰不受限制。這個工具只作用在呼叫者自己的命名空間（見文末已知問題）。
- **行為變更：中文提示注入的比對範圍擴大**（已發佈版本會放過這些中文寫法）。之前中文的指令覆寫只比對四個完全相同的字串，句子裡多插「所有」「之前」「的」就比對不到；實測時有四句這樣的句子經 `user_profile_record` 存進記憶。現在 `instruction_override` 改看句型：覆寫動詞（忽略／無視／忘記／忘掉／不要理會／不用理會／別管）之後，同一個子句、12 個字以內出現指示類名詞（指示／指令／規則／提示詞／系統提示），中間夾著範圍詞（先前／之前／以上／所有／全部／你的／原本等），繁簡體都算，權重與立即封鎖和英文相同。中文的「輸出／顯示你的系統提示詞」類擷取請求依英文規則計分（權重 30，單獨出現不封鎖）；「你現在是管理員模式／開發者模式／越獄模式」「你現在不受限制」等角色覆寫片語和英文同樣處理。門檻與英文清單不變。已知誤判：同句型的一般句子也會被擋，例如「請忽略之前寄的指示，以新版為準」「請忽略以上規則中的第三條，已經取消」「忘記之前的規則了，可以再說一次嗎」，提到「越獄模式」也會；換個不用覆寫動詞的說法即可，例如「之前的指示作廢，以新版為準」。影響範圍：聊天訊息會收到封鎖回覆、不交給 AI；MCP 工具呼叫的參數引用這種句子會被拒絕並記入稽核；對話、輪廓與知識萃取只要比對到任何規則（含不封鎖的擷取規則）就丟棄；`migrate-from` 匯入略過、expert pack 安裝拒絕；Agent Mail 照存但加標記；提示內容被擋的提醒不會執行。這是片語規則，不是分類模型，沒有用真實對話資料量測過。說明見 `docs/features/05-security-defense.md`。
- **`user_profile_record` 拒絕虛擬使用者並掃描提示注入**：`user_id` 為 `system`、`anonymous`、`unknown` 時拒絕；predicate 與值都做提示注入掃描，達到封鎖等級就拒絕寫入。排程與系統提示（虛擬使用者 `system`）的輪廓萃取也不再寫入使用者輪廓。
- **知識審核的聊天通知不會送回說法來源的對話**（現在在實際的推送路徑上生效）；Telegram Mini App 的詳細頁對這類審核只顯示同一則通知，不顯示說法內容。

**已知問題（尚未修正，需要決定資料遷移方式）**：AI 員工透過 MCP 記憶工具（`memory_store`、`memory_search`、`memory_read`、`memory_fetch_batch`、`memory_alias_add`／`memory_alias_list`、`memory_get_history`、`memory_get_at`、`memory_invalidate_by_origin`、`user_profile_record`、`user_profile_get`、`user_code_profile`）讀寫的記憶，落在 MCP 金鑰對應的命名空間。gateway 啟動的每位 AI 員工都用 gateway 的內部金鑰，所以全部共用 `internal/gateway-internal`（v1.44.0 起就是如此）；gateway 自己做的事（對話與輪廓萃取、審核核准、把重點事實與輪廓區塊注入提示）用的是員工自己的 id。後果：同一個 gateway 的員工共用透過這些工具存的記憶；員工透過工具存的內容不是 gateway 注入它提示的內容，它的 `memory_search` 也看不到 gateway 萃取的內容；可信度檢查只在同一個命名空間內比較，它保護 gateway 萃取與操作者核准的事實不被聊天衍生的寫入取代，但不在兩邊之間仲裁。員工之間的記憶隔離只對 gateway 寫入的記憶成立。說明見 `docs/features/20-memory-intelligence.md` 與 `SECURITY.md`。

## [1.67.0] - 2026-10-02 — Discovery 探索×沙箱重建×電腦操作工具接真×Gemini CLI 棄用×安全性修補

### Added
- **Goal 輪次帳本與週報存活表**：保存評估器／判官判決、派工序號、Solo／Team 與閘門輸入、狀態區塊、旋鈕快照及人工暫停原因；成本紀錄帶 task 與 round。`weekly-report --format json` 同步提供分層存活表、Wilson 區間與小樣本標籤。
- **唯讀旋鈕存活表（整合驗證中）**：`knobs survival` 使用經來源雜湊驗證的私人快照，依封存重試上限、完整派工難度、Solo／Team 與可信人工重試分組；人工成果核准另列，未綁輪次只保留件數。缺失、非空 WAL／journal、來源變更或超過資料量／期限上限時明示錯誤，不補造空歷史或舊設定。Unix 的 `--output` 使用目錄描述符保護，其他平台提示使用 stdout。
- **夜間整理成果事件**：有睡眠推理、預取、schema 或整併產出的 pass 寫入 Activity Feed；空 pass 維持靜默。
- **Discovery 探索介面（整合驗證中）**：`tasks.create kind="discovery"` 使用核准工作區 ID、伺服器權限與 manager 核准；Goals 探索樹呈現費用來源並提供雜湊驗證成品下載。正式路徑要求 Container 與專用帳號池；零 LLM 夜間階段以整個 task 與新 held-out 證據比較策略，不宣稱因果效益。探索摘要（`discovery.list`／`discovery.tree`）帶有 `approval_status`（含 `approved`／`denied`／`expired`／`withdrawn`）、24 小時內有效的 `approval_expires_at`、只在真的缺少完整 OS 隔離時才為 true 的 `isolation_degraded`，以及封閉集合的 `stop_code`／`cancel_code`；Goals 卡片據此說明「沒有可用 AI 帳號」之類的停止原因，申請人取消等待中的請求時會一併撤回待核准單。狀態以三語標籤顯示，「每分支改良次數」標示更正（每輪節點數 = 分支數 × (改良次數 + 1)），收件匣的探索核准單以可讀摘要呈現。探索任務在任務看板與任務詳情頁是唯讀的，狀態與內容由目標頁的探索區管理；一般的任務修改、刪除與員工交接都不會動到探索任務。三語使用與設定參考見 `docs/features/60-discovery.md`。
- **Discovery 支援全部六個 runtime 系列**：Claude、Codex、Gemini、Antigravity（`agy`）、Grok、OpenAI 相容在相同保證下執行（`[discovery.attempt.runtimes.<鍵>]` 新增 `codex`、`antigravity`、`grok`）。步數上限與「只有檔案與 shell 工具」改由 gateway 逐行讀取 attempt 事件串流自行執行（Codex 與 Antigravity 由 gateway 計數步數，所有 CLI 系列檢查工具白名單），CLI 旗標退為第二道防線。步數到頂時停止 attempt，工作區照常評分；用了檔案與 shell 以外的工具則捨棄該 attempt，並以新的停止代碼 `tool_violation` 結束探索（`stop_code` 封閉集合由 11 個增為 12 個）。Codex 與 Grok 的訂閱登入可用憑證文件提供（專用帳號池內的 OAuth 帳號，其密鑰內容為該 CLI 的 `auth.json`，容器內更新的 token 不會寫回，建議使用 Discovery 專用登入）；Antigravity 只接受 Gemini API key。CLI 回報認證失敗時，該帳號在這次 attempt 內不再使用，帳號池沒有其他可用帳號就以 `no_account` 結束；對該 runtime 沒有可用憑證的帳號會被跳過。別名 `agy` 一律以 `antigravity` 儲存與顯示。`sandbox = "none"` 實驗模式仍只支援 Claude。使用方式見 `docs/features/60-discovery.md`。
- **任務沙箱的工作目錄大小設定 `workspace_bytes`**：`config.toml [container.sandbox] workspace_bytes` 設定 `/workspace` tmpfs 的大小，正整數，預設 `536870912`（512 MiB），最多 16 GiB。同一段的 `memory_bytes` 最多 64 GiB、`tmp_bytes` 最多 16 GiB；兩個 tmpfs 都算在容器的記憶體上限裡，`tmp_bytes + workspace_bytes` 超過 `memory_bytes` 時整段設定無效，沙箱不可用。
- **任務沙箱殘留自動清理**：gateway 啟動時與之後每 10 分鐘，背景清掉這個 gateway home 的殘留（同一個 Docker daemon 上其他 home 的容器不碰）：已結束或 dead、或超過截止時間 600 秒以上的容器，以及沒有容器在用、超過 600 秒的 `<home>/sandbox/runs/` 目錄。列不出 Docker 容器時什麼都不刪。背景清理或任務結束時的清理失敗，會寫入稽核事件 `task_sandbox_cleanup_failed`（`reason`、`count`）。
- **Discovery 容器不自動下載 image**：attempt、評分與策略容器都以 `--pull never` 建立；本機沒有的評分 image 會在建立容器時失敗，請先自行 `docker pull` 釘選的 image。
- **Computer use 映像的發布流程**：新增 `.github/workflows/computer-use-image.yml`，在 git tag `v*` 或手動觸發時，於原生 runner 分別建置 `linux/amd64` 與 `linux/arm64`；推送前先用 gateway 自己的容器參數做 smoke test（視窗管理器起來、拍得到截圖、`duduclaw-eval-dom` 回傳 `[]`），通過才發布 `ghcr.io/zhixuli0406/duduclaw-computer-use:<tag>` 與 `:latest`。這個 workflow 從 v1.66.1 之後的第一個 release tag 才開始執行，v1.66.1 以前沒有已發布的 computer-use 映像。`scripts/release.sh` 只在結尾提示中提到它，`verify` 不檢查這個映像。
- **`config.toml [computer_use] image`**：全域覆寫 computer use 使用的映像（可用 digest 參照），沒有逐員工的設定。區段無效（未知的鍵、不可用的參照、`config.toml` 無法解析）時電腦操作停用並附上說明，不會退回預設映像。
- **`duduclaw doctor` 新增「電腦操作」一列**：顯示將使用的映像、是否已在本機、哪些 AI 員工設了 `[capabilities] computer_use = true`。沒有員工使用時為 Pass；有員工使用，且映像不在本機、Docker 無法連線或設定無效時為 Warn。仍有員工設定 `[capabilities] computer_use_mode = "native"`（已移除的模式）時，不論其他狀況都為 Warn 並列出這些員工；另列出每位員工的網站白名單有幾個可用網域、幾個項目被略過。
- **`computer_*` MCP 工具可以真正操作電腦了，並新增 `computer_navigate`**：先前這七個工具只是回傳一段 JSON 描述的佔位品，什麼都不執行（session 登錄表在 gateway 行程裡，工具卻跑在另一個 `duduclaw mcp-server` 行程）。現在工具把每次呼叫以簽章請求轉給本機 gateway（`POST /api/internal/computer-use`），由 gateway 持有 session 與容器並執行所有檢查：每位員工一個 session、全域最多 5 個、閒置 2 分鐘或達 `max_session_minutes`／`max_actions` 即結束、臨時角色成員一律拒絕，`computer_use_mode = "native"` 以錯誤代碼 `native_unsupported` 拒絕（不會偷偷改用容器），閘道自行重查 `denied_tools`／`allowed_tools`／`scoped_tools` 與三個審批清單，高風險操作須在該輪對話的通道內由人確認（沒有通道就拒絕）。`computer_screenshot` 回傳 MCP 圖片區塊（已遮蔽）加文字；整張被遮蔽時，gateway 的回應帶 `fully_masked` 與 `mask_reason`（`several_pages`、`title_sensitive`、`title_unreadable`、`helper_failed`），瀏覽器稽核列一併記錄，工具文字會說明整張畫面為了安全被遮住、原因與下一步（多個視窗：再呼叫一次 `computer_navigate`；最前面的視窗敏感或讀不到：該視窗在最前面時無法顯示；其他：再截一次，持續發生就結束並重開 session）。`duduclaw-eval-dom` 在多個頁面可見時以狀態碼 3 結束（其他失敗為 1），gateway 只依狀態碼判斷。`computer_type` 的內容在稽核中只記字數。新工具 `computer_navigate(url)` 是開網頁的唯一方式（kiosk 瀏覽器沒有網址列）。這是唯一的電腦操作路徑，不需要 Anthropic API 金鑰，任何拿得到 DuDuClaw MCP 工具的 runtime 都能用。已用真實模型（Claude Sonnet 5.5，經真正的 MCP server）實測：開啟 session、開啟白名單網頁、從截圖讀出頁面文字、依截圖座標點擊連結、認出白名單外的錯誤頁並結束 session。尚未驗證：由 gateway 在真實通道的對話輪次中啟動的 session；WebChat 上的高風險確認提示可能送不到人。工具總數由 243 變為 249。見 `docs/features/08-browser-automation.md`。
- **電腦操作的網站白名單 `agent.toml [capabilities.computer_use_config] allowed_domains`**：只接受完整主機名（不支援萬用字元、IP、埠號或路徑，最多 20 個，不合格的項目略過並記錄警告）。沒有設定時，session 容器完全不連網（`--network=none`），`computer_navigate` 會拒絕並說明要設定哪個鍵。有設定時，gateway 在 session 開始時自行解析每個網域，答案中只要有任何非公開位址或沒有 IPv4 就略過該網域，其餘以 `--add-host` 釘進容器，並交給容器的出站過濾器：預設全擋，只放行到這些位址的 TCP 443，不允許 DNS。`computer_navigate` 只接受 `https://`、不帶帳號密碼、埠號省略或 443、主機必須完全等於這個 session 解析成功的網域之一。已實測：開啟 `https://example.com/` 並截圖可見；未列網域、仿冒網域（`example.com.evil.test`）、`http://`、非 443 埠、帶帳號的網址、`file://` 都被拒絕；容器內白名單網域解析到釘住的位址、其他名稱無法解析、其他位址與埠被拒；點擊白名單外的連結顯示瀏覽器的 DNS 錯誤頁；停止時容器被移除。仍需注意：白名單網站本身收得到 AI 在上面輸入或送出的任何內容；在自家網域上做代理或轉址的網站可以轉送內容；位址在 session 開始時釘住，期間換位址的網站要開新 session 才能用；白名單逐員工設定。尚未驗證：真實通道上的高風險確認、映像 workflow 在 GitHub Actions 上的執行、amd64 主機與原生 Linux Docker Engine（只在 macOS arm64 的 Docker Desktop 上實測過）、實際觸發一次下載（下載與列印封鎖只確認了政策已被接受、快捷鍵沒有開出任何東西）、WSL2／Windows。
- **Computer use 容器內的瀏覽器政策**：Chromium 以受管政策（`container/scripts/chromium-policy.json`）啟動，每一項都在 `chrome://policy` 確認為已接受：網頁不能要求存取本機網路或 loopback（不會跳出詢問，網頁對 `127.0.0.1:9222` 的 fetch 或 WebSocket 直接失敗）、名稱解析只走 `/etc/hosts`、關閉無痕／訪客／新增使用者、檔案對話框、列印，封鎖下載與彈出視窗，封鎖通知、定位、USB、序列埠、HID、檔案系統、direct sockets、視窗管理權限，關閉音訊／視訊／螢幕擷取、密碼管理員與自動填入、書籤列與書籤編輯，新分頁為 `about:blank`，網址封鎖 `file://`、`chrome://`、`chrome-untrusted://`、`devtools://`、`view-source:`、`javascript://`。刻意不設 `DeveloperToolsAvailability`：它會連同遮蔽與導覽輔助程式依賴的 loopback DevTools 協定一起關掉，DevTools 介面改以 `devtools://` 網址封鎖擋下。已知限制：沒有政策能擋 `ctrl+t`／`ctrl+n`，`ctrl+n` 會開出有網址列的一般視窗（能連的仍只有釘住的網域）；同時有多個頁面可見時截圖整張遮蔽，下一次 `computer_navigate` 會關掉多餘頁面只留一頁。

### Changed
- **沙箱任務失敗時，委派方會收到原因與處理方式**：委派任務失敗時的回覆原本一律是「子任務處理失敗：<分類>」，沙箱自己產生的訊息（例如 image 不在本機、沒有可用帳號、AI 用了不允許的工具）也被蓋掉。現在沙箱拒絕執行時回覆 `⚠️ 子任務未執行（任務沙箱）：Task sandbox unavailable (<代碼>): …`，執行後失敗時回覆 `⚠️ 子任務失敗（任務沙箱，<代碼>）：<訊息>`，並在 gateway log 記一行 `warn`。AI CLI 與 Docker 的原始輸出只留在主機端的 log，不進回覆；其他種類的失敗維持原本的分類訊息。
- **公開文件對照程式碼全面校正**：`ARCHITECTURE.md` 依現況改寫（24 個 crate、十一通道、多 runtime、AEE、SQLite cron；移除不存在的 `duduclaw-bus`／`duduclaw-bridge`／PyO3 橋接描述）；`SECURITY.md` 的防禦層清單改為實際機制（沒有 RBAC 引擎與 credential proxy，CONTRACT 只有 `must_not` 會在通道回覆上強制）；儀表板認證改寫為 JWT 帳號登入或管理員 token（Ed25519 challenge-response 路徑已移除，見 Removed）；DuDuClaw MCP server 的註冊位置更正為逐員工 `.mcp.json`；`CONTRACT.toml` 規格與 JSON Schema 改為與解析器一致（所有鍵皆可省略、`[browser]` 各鍵從未被讀取、`must_always` 只注入 prompt）；瀏覽器自動化、行為契約、認知記憶、演化開關、評測、goal loop、多 runtime 各頁修正不存在的指令與設定鍵；三語翻譯補齊到與英文逐節一致。
- **腳本沙箱與任務沙箱共用同一個 image 設定**：PTC 與 `secaudit` PoC 用的腳本沙箱原本寫死 `duduclaw-agent:latest`（沒有任何流程會建置它，等於從沒跑過容器），現在讀同一個 `config.toml [container.sandbox] image`，預設是 `ghcr.io/zhixuli0406/duduclaw:v<版本>`。行為變更：不再自動下載 image，本機沒有時沙箱視為不可用並提示 `docker pull <image>`；容器以主機使用者執行（主機行程是 root 時改用 image 自己的 `1000:1000`），丟棄所有 capability、加上 `no-new-privileges`、唯讀根檔案系統、`--network=none`，記憶體 2 GiB 且不用 swap、256 個行程、1 顆 CPU、`/tmp` tmpfs，log 有上限（json-file 8m，讀回最多 2 MiB），單次執行硬上限 600 秒，呼叫端取消時強制移除容器；並明確覆寫 image 的 entrypoint，容器內的直譯器一律用 `python3`（原本沿用 host 的指令名稱，Windows 上會變成 `python`）。後端在 macOS 與 Linux 一律用 Docker（Apple Container 後端建不了容器，不再被選上），Windows 先試 WSL2 再試 Docker；WSL2 這條路（含新增的 Windows→WSL 路徑轉換）只做過交叉編譯，尚未在真正的 Windows 主機上執行。腳本容器沒有啟動時的殘留清理，執行腳本的行程若中途死掉，容器會跑到腳本自己結束為止。沙箱不能用時 PTC 的處置見 Security 段。
- **Discovery 費用來源**：非 Claude runtime 的模型若不在價目表內，改標為「未知」並以每次呼叫的完整預留額度計費，不再拿 Claude 的價格估算；把模型加進 `~/.duduclaw/models.toml` 即可變成估算值。Codex 的 attempt 在步數上限被停止時，因 Codex 只在回合結束時回報用量，費用同樣為未知。
- **CE Docker image 改為每架構原生 runner 建置**（`.github/workflows/docker-image.yml`）：amd64 在 `ubuntu-latest`、arm64 在 `ubuntu-24.04-arm` 各自以 digest 推送，再由 merge job 用 `docker buildx imagetools create` 合成多架構 manifest 並打 `<tag>`／`latest`。先前單一 job 在 amd64 runner 以 QEMU 模擬 arm64 要 5–5.5 小時，貼近 GitHub 6 小時 job 上限（v1.66.0 5h41m、v1.66.1 首次 5h15m）。
- **Dockerfile 的 CLI 工具層拆成獨立 stage**（`container/Dockerfile.server`，企業 image Dockerfile 同步）：apt／Node 22／npm CLI（claude、codex、gemini）／agy／grok 的安裝移到 `cli-tools` stage，runtime stage `FROM cli-tools` 再疊 Rust binary。BuildKit 會讓它與 Rust 編譯並行，第三方安裝腳本壞掉會在幾分鐘內失敗，而不是等數小時編譯結束才死在最後一步（v1.66.1 首次 image 建置就是這樣失敗的）；`docker build --target cli-tools` 可單獨煙霧測試工具層。
- **任務沙箱不能用時改為讓任務失敗，不再靜默改成不隔離執行（行為變更）**：`sandbox_enabled = true` 但 Docker 連不上、image 不在本機、gateway 以 root 執行、設定無效或沒有可用帳號時，任務失敗並寫入稽核事件 `task_sandbox_unavailable`（附原因代碼）。舊版在這些情況會直接在主機上不隔離地執行。`network_access = false` 的員工現在會被拒絕（沙箱內的 AI 必須連到模型供應商，只放行供應商的出站限制尚未實作），要用沙箱請設為 `true`。出貨的 evaluator 板模預設改為 `sandbox_enabled = false`（既有部署維持各自 `agent.toml` 的值）。想保留舊行為可設 `config.toml [container.sandbox] when_unavailable = "run_unsandboxed"`，每個不隔離執行的任務都會寫入 `task_sandbox_bypassed`。這個鍵只管任務沙箱，PTC 腳本另有 `script_when_unavailable`。
- **任務沙箱的涵蓋範圍改為明確規則（行為變更）**：開了 `sandbox_enabled` 的員工，進沙箱的是 bus 或儀表板派來的任務、heartbeat 從任務看板領工作的喚醒、autopilot 的 `delegate`／`run_skill`、goal 回合與多步驟計畫的各步驟。這類員工不再組成團隊：可拆性閘在所有規則之前回傳 Solo（理由 `sandbox_enabled`，連 `gate = "always_team"` 也蓋不過），goal 回合一律在沙箱裡以 Solo 執行，不會有角色成員在主機上跑。Agent Mail 的到信觸發對這類員工直接跳過，信件留在收件匣等人處理。通道回覆、排程（cron）任務、提醒、heartbeat 主動檢查、代替員工產生的 ephemeral agent、`duduclaw acp` 工作階段與 live `duduclaw eval` 依設計仍在主機執行，但不再無聲無息：每位員工的每條途徑在每個 gateway 行程第一次發生時，寫一筆稽核事件 `task_sandbox_not_applied`（details `{path, action}`，`path` 為 `channel_reply`／`cron`／`reminder`／`mail`／`proactive`／`ephemeral`／`acp`／`eval`，`action` 為 `ran_on_host` 或 `skipped`），並在 log 記一行警告。沒開沙箱的員工行為不變。`duduclaw doctor` 的任務沙箱一項在有員工開沙箱時多一行，說明哪些工作仍在主機執行、goal 回合一律 Solo、新信件不會喚醒他們。
- **儀表板契約編輯器的文字改成對照實際行為**：`must_not` 標為「禁用詞句」，說明含有其中詞句的回覆會被攔下，而且只適用於聊天回覆；`must_always` 標為「行為指引」，說明會寫進 AI 員工的工作指示，系統不會檢查回覆是否照做；每回合工具呼叫數說明為寫進指示的數字，系統不會強制中止。
- **Computer use 預設改用帶版本號的已發布映像，且不會自動下載（行為變更）**：預設映像由 `duduclaw-computer-use:latest` 改為 `ghcr.io/zhixuli0406/duduclaw-computer-use:v<gateway 版本>`；`docker run` 帶 `--pull never`，session 開始前先確認映像在本機，不在就停止並寫出映像名稱與兩種處理方式（`docker pull <image>`，或在 repo 根目錄 `docker build -f container/Dockerfile.computer-use -t duduclaw-computer-use:latest .` 後設定 `[computer_use] image = "duduclaw-computer-use:latest"`）。升級注意：只有本機自建 `duduclaw-computer-use:latest` 的機器，必須拉取帶版本號的映像，或設定覆寫鍵。
- **Computer use 容器加固**：每個電腦操作容器都帶 `--security-opt no-new-privileges`；有釘住網域的 session 明確使用 `--network bridge`。出站過濾器在釘住模式與「有網路但沒有白名單」模式下，loopback 只放行 `127.0.0.1` 與 `::1`，並拒絕 Docker 內建解析器 `127.0.0.11`（在自訂 Docker 網路上它會替任何名稱解析，可被拿來經 DNS 外傳資料；gateway 本身用預設 bridge，原本就連不到）。過濾器自己送出的 TCP RST 與 ICMP unreachable 回應可在 loopback 上通過，所以被擋的連線會立即被拒絕，不用等到逾時：白名單外的位址連 443、釘住的位址連 80 都立即被拒，白名單外的名稱立即解析失敗。IPv6 規則已安裝，但預設 bridge 沒有 IPv6 路由，尚未以實際流量驗證。輔助程式的 JavaScript 一律在隔離環境（`Page.createIsolatedWorld`）執行，網頁重新定義 `document.visibilityState`、`querySelectorAll` 或 `getBoundingClientRect` 也改變不了遮蔽範圍（已用這種網頁實測）。`duduclaw-navigate` 只從 stdin 讀網址（`docker exec -i`），帶任何參數都是用法錯誤，完整網址不會出現在行程參數列。截圖改存 `/tmp/duduclaw-root/screen.png`，該目錄屬 root、權限 0700，由 entrypoint 在任何瀏覽器行程啟動前建立，瀏覽器使用者無法預先建立或替換檔案；Chromium 記錄也移到這裡。映像發布 workflow 的 smoke test 另外確認 `duduclaw-navigate` 在沒有網路時乾淨地失敗。已知限制：以 `docker exec` 啟動的 root 行程仍保有容器的 `NET_ADMIN`（只有 PID 1 的行程樹會丟掉它），而只有 gateway 會 exec 進容器。
- **聊天訊息不再觸發電腦操作（行為變更）**：含「打開」「截圖」「click on」等關鍵字的通道訊息改走一般回覆路徑，由 AI 員工自己決定要不要呼叫 `computer_*` 工具；gateway 不再在通道裡回報進度、傳截圖、提供暫停／繼續或回覆「電腦操作無法啟動」。電腦操作不再讀取 `ANTHROPIC_API_KEY`／`[api] anthropic_api_key`（直接 API 的回覆後備路徑仍會讀）。通道訊息只剩緊急停止詞仍有作用：會結束所有電腦操作 session。這個改走一般回覆的行為尚未在真實通道上驗證。
- **以非內部金鑰經 HTTP 呼叫 `create_agent`／`agent_remove` 時改以該金鑰的身分判斷（行為變更）**：先前這類呼叫被當成 MCP server 行程的預設員工，現在以金鑰自己的 client id 判斷，組織範圍檢查套用到真正的呼叫者。
- **`agent_remove` 的回覆**：不再回傳垃圾桶路徑或 `rm -rf` 提示，只說明員工已移除、管理者可以復原、名稱會保留。
- **`computer_navigate` 的審批**：先檢查網址，會被拒絕的網址不會建立審批單；審批文字只寫通過檢查的主機名稱，不含路徑與查詢字串。
- **一般工具的審批單改以「執行工具」呈現**：非安裝類工具的審批單種類由 `mcp_install` 改為 `mcp_call`，收件匣、通道推播與回給 AI 員工的拒絕／逾時訊息都改成「工具「<名稱>」的呼叫…」，不再寫成「安裝要求」。安裝類工具維持原樣。八個 `computer_*` 工具改由 gateway 的電腦操作路徑詢問（三個清單都一律詢問人），MCP 端的審批閘跳過它們，避免同一個呼叫被問兩次。
- **`tool_calls.jsonl` 的 `computer_*` 紀錄**：八個工具都會記錄；`computer_type` 只記字數，`computer_navigate` 只記主機與路徑長度（不記路徑與查詢字串），`computer_screenshot` 不含圖片。電腦操作截圖保留 7 天，每位員工最多 500 個檔案或 200 MiB，超過時從最舊的刪起（先前沒有任何東西會刪截圖）。

### Deprecated
- **Gemini CLI runtime 標為棄用，v1.67.0 起生效、預計 v1.69.0 移除**：runtime id `gemini`（執行檔 `gemini`、npm `@google/gemini-cli`）改由 Antigravity CLI runtime（`antigravity`／`agy`）接手。原因是 Google 已在 2026-06-18 停止以 Gemini CLI 服務免費、AI Pro、AI Ultra 的個人帳號；API key 與企業使用者不受影響，Gemini CLI 本身仍在維護。棄用期間 `provider`、`fallback`、`utility_provider`、`judge_provider`、團隊角色 `runtime` 與 `[discovery.attempt.runtimes.gemini]` 設為 `gemini` 都照常解析與執行，讀到時每個行程記一次警告，經儀表板寫入時另記 `runtime_provider_deprecated` 審計事件；「依模型自動對齊 runtime」不再寫入已棄用的 runtime，`duduclaw doctor` 會列出用到它的員工。儀表板不再提供 Gemini（已存的值標「已棄用」），員工編輯頁的 runtime 選單補上原本缺少的 Antigravity，首次設定精靈的預設值由 Gemini 改為 Antigravity。Gemini API provider（`GEMINI_API_KEY`、`generateContent`）不受影響，Docker image 在移除前仍內含 Gemini CLI。移除前須先用真的 Gemini API key 驗證 Antigravity 的 API key 模式。遷移步驟見 `docs/guides/deprecations.md`。

### Removed
- **從未建置過的沙箱 image `duduclaw-agent:latest`、`container/Dockerfile.agent` 與 `container/agent-entrypoint.sh`**：任務沙箱與腳本沙箱（PTC、`secaudit` 的 PoC 步驟）都改用平台自己發布的 image，不再有第二個 image 需要維護。
- **聊天訊息觸發的電腦操作迴圈與 native（操作主機桌面）模式**：先前 gateway 會在通道訊息看起來像電腦操作要求時，自己呼叫 Anthropic Messages API 的 `computer_20251124` 工具、在容器裡執行動作並把進度與截圖傳回通道；`computer_use_mode = "native"` 則要透過 `enigo` 直接操作主機桌面。兩者都直接移除而不是先棄用，原因與下方 Ed25519 路徑相同：在任何已發布版本裡都無法完成一個 session。v1.66.1 中它啟動的容器使用一個沒有任何流程建置或發布的映像，而那份 Dockerfile 根本起不來（瀏覽器是 snap 空殼、entrypoint 在 `--network=none` 下中止、沒有視窗管理器）；native 模式在任何主機動作之前也先呼叫同一個容器啟動。現在 `computer_*` 工具是唯一的電腦操作路徑。`agent.toml [capabilities] computer_use_mode` 仍可解析：`"native"` 會被工具以 `native_unsupported` 拒絕並說明請刪除這個鍵或改成 `"container"`，`"auto"` 與未設定都等同 `"container"`；`duduclaw doctor` 會警告並列出仍設定 native 的員工。Gateway crate 不再依賴 `enigo`／`duduclaw-desktop`，`desktop` cargo feature 保留為空（release workflow 仍會傳入）；瀏覽器稽核列不再出現 tier `L5b`。儀表板的員工編輯頁不再提供這個模式；已存成這個模式的員工會看到「已移除」標示與說明，存檔其他欄位時不會被偷偷改掉。
- **儀表板的 Ed25519 challenge-response 認證路徑**：這條路徑從來無法啟用：沒有任何設定會讀取公鑰，只有測試建構過它，儀表板也從未實作 client 端。已從 gateway 移除。儀表板認證只有兩種：JWT 帳號登入（密碼以 Argon2id 雜湊存在 `users.db`），或 gateway 管理員 token。授權簽章、更新驗證與 relay 裝置協定仍使用 Ed25519，不受影響。

### Fixed
- **電腦操作的滑鼠動作每次卡約 18 秒**：`xdotool mousemove --sync` 在指標已經位於目標位置時，會等一個永遠不會來的移動事件，約 18 秒後才放棄；所以在同一點點兩次、或在同一位置連續捲動，每個動作都要約 18 秒（指標一開始在螢幕中央，預設螢幕上第一個打在 640,400 的動作也會碰到）。點擊、右鍵、雙擊、捲動、移動與縮放都已拿掉 `--sync`，實測每個動作 0.2–0.3 秒，並在真實容器中確認移動後的點擊仍落在指定座標。
- **去識別化代碼在工具與通道之間對不上**：v1.65 起，MCP 工具結果裡的去識別化代碼存在 `gateway-internal` 名下，通道回覆因此無法還原；反過來，通道為員工產生的代碼在工具出口會被判成捏造而拒絕（`-32007`）。現在兩邊都以實際員工為鍵。
- **Umbrel 散發包的 image 標籤指向不存在的 tag**：`distribution/umbrel/duduclaw/docker-compose.yml` 寫 `duduclaw:1.56.0`，但發布流程推的標籤都帶 `v` 前綴（`1.56.0` 回 404、`v1.56.0` 回 200），已改為 `v1.56.0`。
- **AI 員工編輯頁的沙箱開關永遠顯示為關**：`agents.inspect` 沒有回傳 `[container] sandbox_enabled` 與 `network_access`，所以兩個開關不論實際設定為何都顯示為關。現在兩個欄位都會回傳，表單以實際值初始化。
- **Antigravity 的 API key、登入與 MCP 工具**：API key 路徑原本指向不存在的 `ANTIGRAVITY_API_KEY`，現在以 `config.toml [antigravity] auth = "api_key"` 啟用並使用 Gemini API key（`auth = "login"` 切回 Google 登入）；儀表板對 Antigravity 的一鍵登入原本會報錯（`agy login` 不存在），改為說明在主機終端機執行 `agy` 登入；Antigravity 員工原本拿不到平台的 MCP 工具（註冊寫在 agy 不讀的檔案），改寫到員工工作區的 `.agents/mcp_config.json`，身分改由啟動環境傳遞，磁碟上不再留明文的員工 token。
- **非管理員（manager、employee）的收件匣與首頁資料為空**：收件匣一律顯示紅色「部分內容未載入」橫幅，且從不列出 `blocked`／`needs_human` 任務；首頁任務卡、任務看板、活動串流與共享計畫清單也靜默為空。原因是 gateway 要求非管理員必須把 `tasks.list`／`activity.list`／`plans.list` 限定在自己綁定的 agent，而儀表板沒有帶 agent 就呼叫。儀表板現在逐一查詢檢視者可存取的每個 agent 並合併結果，收件匣也不再呼叫該角色無權讀取的資料來源；任一員工的查詢失敗時保留其餘結果並照樣顯示橫幅，安裝申請清單載入失敗也不再被靜默吞掉。任務看板的 WIP 量表（`tasks.flow_metrics`）同樣改為逐員工查詢，量表數值取全系統值、不重複加總。任務看板、共享計畫、活動串流與首頁健康區在部分員工載入失敗時會顯示一行提示並保留已載入的資料。gateway 的權限規則不變。
- **單一頁面元件出錯不再讓整個儀表板空白**：任務狀態圖示遇到不認得的狀態（例如探索任務的「等待核准」「排隊中」）會退回預設圖示，任務看板把這類任務放在待辦欄；頁面層與應用程式層各加一道錯誤邊界，出錯時顯示可重新載入的訊息，側邊欄仍可操作。
- **微調歷史配對**：舊 accepted 輪沒有 `worker_excerpt` 時，改用同樣節錄長度的 `tasks.result_summary`，恢復可用的偏好配對。
- **人工選擇 fork 分支**：待裁決工作區保留到 TTL 清理，選擇時真正複製成果回父目錄；遺失時回錯誤並保留未採用狀態。
- **子行程與人工重試**：fork 測試逾時終止整個行程群組；外部判官限制輸出並於取消終止。人工重試追加未封存列，不覆寫先前判決或污染下一輪輸入。
- **遷移測試密封**：OpenClaw 遷移改用明確 home，測試不再讀取操作者的真實技能目錄。
- **探索期限與限流**：已確認的最佳成果逐次保存，兄弟逾時仍能交付；首次 provider 限流或用量封鎖會取消整個探索、不換帳重試，報告標記停止原因。
- **Claude 判官費用歸屬**：utility CLI 在終端結果返回前同步記錄實測用量，保留 goal 的 task／round 與快取 token；錯誤回應有用量仍記帳。模型依 CLI 實際回報，不把 `<synthetic>` 當型號，也不補造缺失用量。
- **任務沙箱（`agent.toml [container] sandbox_enabled`）原本從沒跑通，現已重建**：舊實作用的 image `duduclaw-agent:latest` 沒有任何腳本或 CI 會建置、Claude 的金鑰以 Claude Code 不讀的 `ANTHROPIC_API_KEY_FILE` 傳入、容器內沒有可寫的 HOME、預設又完全不連網（AI CLI 連不到模型供應商），所以開了沙箱的員工被委派任務時每次失敗。新實作（`task_sandbox.rs`）重用 Discovery 的 attempt 容器機制：唯讀根檔案系統、非 root（gateway 的 uid 或 gid 為 0 時拒絕）、丟棄所有 capability、記憶體／行程／CPU 上限、容器以 `--rm --pull never` 建立；工作目錄是容器內有大小上限的 `/workspace` tmpfs，隨容器丟棄、不寫主機磁碟；員工目錄只掛白名單項目（見 Security）；憑證依員工的 runtime 從帳號輪替器取得。預設 image 是平台自己發布的 `ghcr.io/zhixuli0406/duduclaw:v<版本>`，需自行 `docker pull`；可用 `config.toml [container.sandbox]` 調整 image、資源上限、工作目錄大小與步數。沙箱內只有檔案與 shell 工具，沒有平台 MCP 工具，產出只有最後的回覆文字。`duduclaw doctor` 新增沙箱檢查。撰寫時尚未做實機活體驗證。見 `docs/guides/task-sandbox.md`。
- **Computer use（L5）容器映像原本從沒跑通，現已修好並驗證**：舊的 `container/Dockerfile.computer-use` 以 `ubuntu:24.04` 為基底，其 `chromium-browser` 套件只是 snap 過渡用的空殼，瀏覽器在容器裡起不來；entrypoint 載入網域過濾器時在 `set -u` 下碰到未設定的 `ALLOWED_DOMAINS` 就中止，所以預設的 `--network=none` 一啟動就結束；映像沒有視窗管理器，就緒檢查與健康檢查（`xdotool getactivewindow`）永遠不會成功；也沒有 gateway 用來找敏感區塊的 `duduclaw-eval-dom`，每張截圖都整張塗黑。新映像以 `debian:trixie-slim` 為基底，內含真正的 `chromium` 套件、視窗管理器 `openbox` 與 Python 3；Chromium 以 kiosk 模式從 0,0 鋪滿虛擬顯示器、裝置縮放比例 1，瀏覽器與視窗管理器以非特權使用者 `sandbox` 執行，DevTools port 只在容器內的 127.0.0.1 監聽。新增的 `duduclaw-eval-dom` 只遮 `input[type=password]`、`.credit-card`、`[data-sensitive]` 的區塊；在測試頁實測沒有敏感像素外露，每邊最多多蓋 2 px，瀏覽器縮放後同樣正確；任何失敗都讓 gateway 整張遮掉。偵測不到不屬於頁面的瀏覽器介面（縮放提示、權限詢問、自動填入選單、alert 對話框）、跨來源 iframe 與 shadow DOM。Gateway 啟動容器的行程上限由 100 調為 512（一般含 web worker 的頁面就會用完 100 個），輔助程式呼叫加上 10 秒逾時。映像約 1.09 GB。v1.66.1 以前沒有已發布的 computer-use 映像；從 v1.66.1 之後的第一個 release tag 起由 `.github/workflows/computer-use-image.yml` 發布，**要用 computer use，請在升到含這個 workflow 的版本後 `docker pull ghcr.io/zhixuli0406/duduclaw-computer-use:v<版本>`**，或在 repo 根目錄執行 `docker build -f container/Dockerfile.computer-use -t duduclaw-computer-use:latest .` 自行建置，再設定 `config.toml [computer_use] image = "duduclaw-computer-use:latest"`。見 `docs/features/08-browser-automation.md`。

### Security
- **被移除員工的名稱對 AI 呼叫者保留**：會監管其他員工的 AI 員工原本可以 `agent_remove` 一位下屬、再用同一個名稱 `create_agent`，拿到同名但沒有管理者原先設定的 `CONTRACT.toml`、`[capabilities]` 限制與沙箱設定的位置。現在 AI 呼叫者建立員工時，只要 `<home>/agents/_trash/` 有該 id 的項目（`<id>_<14 位時間戳>`，整個 id 比對）、`org.toml` 仍記錄該 id 但目錄已不在、或垃圾桶無法列出（fail closed），名稱就會被拒絕。所有 MCP 呼叫者都視為 AI；CLI 建立員工的路徑（`duduclaw agent create`、pack／expert install、migrate-from）在偵測到 `DUDUCLAW_AGENT_ID`／`DUDUCLAW_AGENT_TOKEN` 時套用相同規則，`agent-file-guard` hook 另外擋下 Bash 的 `duduclaw agent create <保留名稱>`，以及 AI 對 `agents/_trash/` 的寫入、搬移與刪除（Bash 為啟發式）。稽核：`agent_name_reserved`（`requested_name`、`path_kind`、`reason`）與 `agent_removed`。管理者不受限制：儀表板與終端機前的人可以使用保留名稱；復原或清除要手動處理 `~/.duduclaw/agents/_trash/<id>_<時間戳>`，儀表板沒有對應功能。已知缺口：Claude／Codex／Gemini 員工的身分在 `.mcp.json` 而不在 Bash 環境，所以從它們的 Bash 執行 `pack install`、`expert install`、`migrate-from` 不在保護範圍；有 Bash 的員工總能繞過啟發式規則。已用員工自己的註冊資訊經真的 MCP server 實測（移除、同名重建被拒、換名字可建立、hook 的攔阻、稽核紀錄）；CLI 建立員工的那條路徑只有單元測試。
- **MCP HTTP client 的本機判斷改為精確比對**：`duduclaw-llm` 的 MCP HTTP client 只在解析後的主機正好是 `localhost`、`127.0.0.0/8` 內的位址或 `::1` 時才接受純 `http://` 端點；先前以前綴比對，`http://localhost.evil.com` 也會被當成本機。
- **Odoo 網址檢查拒絕帶帳號的網址**：網址改為先解析，任何帳號／密碼部分一律拒絕；先前 `http://localhost:3000@evil.com` 會被當成本機，`https://user@10.0.0.1/` 也可能藏住私有位址。
- **對外連線的位址檢查漏掉多個內部與保留網段，已發布版本受影響**：`web_fetch_cached`、`web_extract` 與 gateway 其他走同一檢查的出站路徑，原本只擋 `127.0.0.0/8`、`10.0.0.0/8`、`172.16.0.0/12`、`192.168.0.0/16`、`169.254.0.0/16`、`0.0.0.0`、`::1` 與 `fc00::/7`，其餘一律當成公開位址。因此 `http://[::ffff:127.0.0.1]:<埠>/` 這類 IPv4-mapped 位址會通過檢查並連到本機 loopback，NAT64、6to4 形式的內部位址也一樣。現在所有出站 SSRF 檢查共用 `duduclaw_core::net_addr::is_public_ip`（`web_fetch_cached`、`web_extract`、媒體下載、常駐感知資料來源、relay 設定、MCP 匯入、skills RPC、Odoo 網址檢查、wiki 聯邦節點檢查、電腦操作的位址釘選）。新增拒絕的 IPv4：`0.0.0.0/8`、`100.64.0.0/10`、`192.0.0.0/24`、`192.0.2.0/24`、`198.18.0.0/15`、`198.51.100.0/24`、`203.0.113.0/24`、`224.0.0.0/4`、`240.0.0.0/4`；IPv6：全域單播 `2000::/3` 以外的一切，以及 Teredo `2001::/32`、`2001:db8::/32`、`3fff::/20`。IPv4-mapped（`::ffff:0:0/96`）、NAT64（`64:ff9b::/96`）、6to4（`2002::/16`）依內嵌的 IPv4 判斷；IPv4-compatible `::/96` 與 `64:ff9b:1::/48` 整類拒絕。Odoo 網址檢查原本已涵蓋私有、loopback、link-local、CGNAT、廣播與 IPv4-mapped／link-local／unique-local IPv6，這次新增的是文件、基準測試、`192.0.0.0/24`、`0.0.0.0/8`、多播與保留網段，以及上列 IPv6 形式。
- **每位員工的工具審批設定在正式路徑上從未生效，現已修正（行為變更）**：MCP 審批閘把 gateway 內部金鑰的名稱（`gateway-internal`）當成員工 id 去讀設定，那個員工目錄不存在，所以 `agent.toml [capabilities]` 的 `approval_required_tools`、`irreversible_tools`、`maybe_irreversible_tools` 在 gateway 啟動的員工身上形同沒設，列在裡面的工具會直接執行。現在以實際呼叫的員工為準。**升級後，已列在這三個清單裡的工具會開始等待人工核可**（最長 300 秒，逾時視為拒絕）；審批單與注入攔截的稽核也改記在該員工名下，收件匣與通道推播會正確歸屬。stdio、HTTP、SSE 三種傳輸都適用，外部金鑰的行為不變。
- **任務沙箱不再把一家廠商的金鑰交給另一家的 CLI**：開啟 `[container] sandbox_enabled` 的員工若使用非 Claude 的 runtime，舊沙箱會把 Anthropic API key 以該 runtime 的變數名注入（例如 `OPENAI_API_KEY`、`GEMINI_API_KEY`），該 CLI 會把它當成自己的憑證送給對應廠商。重建後的沙箱只從帳號輪替器取該員工 runtime 對應的帳號，以該 CLI 自己的變數名或憑證文件交給容器，不再掛載 Anthropic 金鑰檔。
- **任務沙箱只掛載員工目錄的白名單項目**：容器有網路，而員工目錄的 `.mcp.json` 存著該員工的 MCP key 與身分 token，整個目錄掛進去，容器內的 AI 就讀得到也送得出去。現在只有存在的 `SOUL.md`、`IDENTITY.md`、`CLAUDE.md`、`AGENTS.md`、`GEMINI.md`、`CONTRACT.toml`、`SKILLS/`、`wiki/` 各自唯讀掛在 `/agent/<名稱>`；符號連結、有硬連結的檔案、種類不符或解析到員工目錄外的項目會略過並記警告。`.mcp.json`、`.claude/`、`state/`、`agent.toml` 與資料庫在容器內都看不到。
- **PTC 腳本容器不再掛載主機的暫存目錄**：容器內只看得到一個放腳本的私有目錄（唯讀掛在 `/workspace`）；主機共用的暫存目錄、`DUDUCLAW_PTC_SOCKET` 與 `/run/duduclaw` 都不再進容器，腳本也因此無法回頭呼叫平台工具（容器內沒有 RPC socket）。
- **PTC `execute_program` 預設不再在主機上執行（行為變更）**：舊版在腳本沙箱不能用時會默默改用主機子行程、不做任何隔離；因為沙箱 image 從沒建置過，實際上每一次都是這樣。現在由新鍵 `config.toml [container.sandbox] script_when_unavailable` 決定，預設 `"fail"`：腳本不執行，工具回傳 `Script sandbox unavailable (<代碼>): …` 並提示 `docker pull <image>`，同時寫入稽核事件 `script_sandbox_unavailable`（附 `reason`、`language`）。**升級後 `execute_program` 會一直失敗，直到在本機 `docker pull` 沙箱 image，或設定 `script_when_unavailable = "run_unsandboxed"`**；後者讓腳本照舊在主機上執行，每次寫入 `script_sandbox_bypassed`。原因代碼：`invalid_config`、`no_runtime`、`runtime_unhealthy`、`image_missing`、`create_failed`、`start_failed`。這個鍵與任務沙箱的 `when_unavailable` 分開設定，互不影響：讓被委派的任務不隔離執行，和讓送進來的腳本在主機上執行，是兩種不同的風險。`secaudit` 的 PoC 步驟一向不在主機上執行，現在也一樣。
- **Discovery 的密鑰不出現在主機行程命令列**：attempt 容器的 API key 與憑證文件用 `docker create --env 名稱` 傳遞，值放在 docker 用戶端行程的環境裡；值不出現在命令列（同一台主機的其他使用者看得到行程清單）。後續的 `docker start`／`docker rm` 不帶這些值。
- **Discovery 事件串流檢查**：attempt 的輸出出現超過 3 行無法解析的內容、Claude／Grok 回覆中出現文字與允許工具以外的區塊型別、或 Antigravity 有白名單外的工具未被原生 hook 擋下時，該 attempt 作廢並以 `tool_violation` 結束探索。這項檢查讀的是容器內 CLI 自己的輸出，容器內行程刻意偽造串流的情況由容器隔離與預算上限圍住，已在功能文件的已知限制說明。
- **工作區複製與提升**：共用複製規則排除 `.env*`、金鑰及根外連結；探索採更嚴格規則，丟棄全部連結與多重硬連結。分支不再自動攜帶這些機密檔案，需自行配置憑證。
- **macOS 沙箱環境**：修復 `env_clear()` 被環境重建忽略而洩漏父行程變數的問題；加入執行時見證，無法辨識時拒絕執行。
- **分支採用權限與競爭**：人工採用、背景結果發布及保留資料清理共用跨行程鎖；其他員工不能終止或刪除不屬於自己的分支。 Windows 上的已知問題：以系統管理員身分執行時 fork 一律失敗的缺陷已修正（程式原本拒絕使用自己建立、擁有者為 Administrators 群組的暫存目錄），但 Windows CI 仍有 4 個發布流程測試未通過（自動採用與發布鎖的先後順序、資料庫故障後的復原）。live fork 需逐員工以 `agent.toml [fork] enabled = true` 開啟，預設關閉；在這些測試通過之前，請不要在 Windows 上開啟。
- **探索策略與評分器**：正式 Python 策略使用釘住 image 的資源受限容器；評分容器先確認建立，再啟動具截止時間的可信 supervisor。成果副本須符合評分前雜湊，才可寫入有效帳本並成為最佳快照。
- **AI 員工不能再改寫自己的 `CONTRACT.toml`**：PreToolUse hook `duduclaw hook agent-file-guard` 原本只擋別的員工的契約，員工用 Write／Edit／MultiEdit 或 Bash 就能刪掉自己的 `must_not` 界線。現在員工身分的呼叫寫自己目錄裡的 `CONTRACT.toml` 一律擋下（判定 `BlockedOwnContractWrite`），沒有任何開放旗標；Bash 規則同時辨識 `agents/<自己>/CONTRACT.toml` 與 `CONTRACT.toml`、`./CONTRACT.toml` 這類相對寫法，自己的 `SOUL.md` 也比照辦理。擋下時的訊息會請員工去找操作者，操作者照舊在儀表板修改契約（`contract.update`，僅限管理者）。Bash 規則是減速帶，把檔名藏起來的指令仍可能繞過；真正的隔離是不給員工 Bash。
- **live fork 採用不再把 agent 結構檔帶回員工目錄**：分支被採用回員工目錄（`<home>/agents/<id>`，含 ephemeral 員工）時，目錄根部的 `agent.toml`、`SOUL.md`、`CLAUDE.md`、`MEMORY.md`、`.mcp.json`、`CONTRACT.toml` 與整個 `.claude/` 一律維持上層原本的內容，分支裡的改動不會覆蓋回去；指向這些檔案的符號連結也不會被重建。`fork_run` 的自動採用、手動選擇與儀表板上由操作者選分支三條路徑都適用。分支仍然可以讀取這些檔案，子目錄裡的同名檔（例如 `docs/CLAUDE.md`）與一般專案目錄的 fork 不受影響。上層是 gateway home 或 `agents/` 本身時，採用不會寫進任何員工目錄。
- **Computer use 容器的網域過濾器裝不上規則時拒絕啟動（行為變更）**：`domain-filter.sh` 設定預設拒絕的出站規則需要 `NET_ADMIN`，Docker 預設不給；舊版裝不上規則時照樣往下跑，開了網路的 session 出站完全沒過濾。現在只要裝不上這條規則、容器又有非 loopback 的路由，容器就直接結束；`--network=none`（預設）不受影響。Gateway 只在 session 有解析成功的白名單網域時加上 `--cap-add=NET_ADMIN`，其他情況維持 `--network=none`。手動以網路啟動這個映像時須自行加 `--cap-add=NET_ADMIN`。
- **讀不到視窗標題時整張截圖遮掉**：DOM 遮罩之後 gateway 會讀焦點視窗標題，原本只有標題含憑證字樣才整張遮掉；現在標題讀不到（指令錯誤、逾時、非零結束碼、輸出不是 UTF-8）同樣整張遮掉。成功讀到的空標題不會觸發。

## [1.66.1] - 2026-09-29 — hotfix：macOS 26 機器指紋改綁 IOPlatformUUID×舊指紋相容放行

### Fixed
- **macOS 26 起一般行程讀不到任何 MAC，機器指紋退化成只綁 hostname**：指紋的硬體分量一直取「第一個非 loopback 介面的 MAC」。macOS 26（Darwin 27）對未授權行程遮蔽**所有**介面的 MAC——2026-09-29 實測：`/sbin/ifconfig`（Apple 簽章）看得到 `en0 = d0:11:e5:db:58:67`，但 binary／`node`／裸 `getifaddrs` 探針拿到的每個介面都是 `02:00:00:00:00:00`——指紋因此塌成「只綁主機名稱」，同 OS 版本的每台 Mac 共用同一個硬體分量。1.66.1 起 **macOS 改以 `IOPlatformUUID` 綁定硬體**（`ioreg -rd1 -c IOPlatformExpertDevice`，一般行程讀得到；絕對路徑、stdout 上限 64 KiB、2 秒逾時，任何失敗退回 MAC 路徑）；**Linux／Windows 的指紋一個位元都沒變**——MAC 分量維持 `mac_address` 的 `Display` 大寫格式，原本第一個介面就有效的主機算出來與 1.66.0 逐位相同。
- **升級／換 OS 不再讓既有授權整批失效**：指紋現在附帶一份**相容候選清單**（`[平台 UUID, 過濾後 MAC, 舊碼會取到的原始 MAC（含 `02:00:…` 佔位值）, hostname::00:00:00:00:00:00]`，去重）。以舊指紋簽出的 license 照樣通過驗證，gateway 只在開機留一則 WARN 建議重簽（`duduclaw license rebind`，即控制平面 `/v1/license/rebind`），不降級成 OpenSource。`duduclaw license status` 多一行 `Fingerprint binding: strong | legacy (re-issue recommended)`；`duduclaw license fingerprint` 維持只印強指紋（客戶回報／新簽發用），新增 `--all` 列出本機接受的全部候選（第一行仍是強指紋，腳本照抓第一行）。
- **MAC 挑選不再盲取第一個**：`select_primary_mac` 跳過全零、跳過 `02:00:00:00:00:00`、跳過 multicast 位址——任一種被選中都會讓指紋失去識別力。
- **「本機根本沒有硬體身分」從靜默變成明講**：既無平台 UUID 也無可用 MAC 時（容器／受限環境），CLI（`license fingerprint`／`license status`）與 gateway 開機各明確報告一次，不再讓弱綁定被當成強綁定。

### Security
- **授權機器綁定強度回復**：macOS 改綁 `IOPlatformUUID` 之後，同一 OS 版本的不同 Mac 不再共用硬體分量；其他平台則堵住「選到佔位／全零／multicast 位址而塌成只綁 hostname」的路徑。相容候選只在**驗證既有授權**時參與比對，簽發與對外回報一律用強指紋，舊的弱綁定不會被延續到新單；gateway 的 phone-home／散發包簽章沿用「實際生效的那個指紋」，與控制平面既有的 row 保持一致。**行為變更**：所有既有 macOS 安裝的指紋都會改變——舊 license 仍放行但標示 legacy，請排程重簽。

## [1.66.0] - 2026-09-29 — 功能盤點大清理×真資料進料×Team／AEE 預設開×介面收斂

### Fixed
- **P2／P2b 多目錄案例鍵碰撞**：量表以前只用 TOML 檔名當案例 ID，三個目錄都叫 `checkins.toml` 時 12 個團隊活測臂被統計覆寫成一個配對；一般矩陣亦可能在取案例時選錯首筆。兩種矩陣現在共用相對套件路徑（如 `north/checkins`）作報告、派工與配對鍵，`--case` 仍接受原檔名；舊報告保留為缺陷證據，不作選角依據。
- **P5 團隊開始回報**：成團派工沿用既有目標通知去重與來源對話路由，顯示「已交給團隊，預計 N 分鐘回報」；N 依進度間隔與三個必要階段推算，非完成保證。Solo 文案與派工流程不變。
- **P4 角色故障歸屬欄位**：`role_turns.jsonl` 在寫入時由角色、結果與封閉錯誤 token 記錄 `failure_edge`／`fault_side`，隔離團隊量表也保留這兩欄；無觀測或原因不明一律 `unknown`，完成與跳過的回合不造故障。這只建立保守遙測，不把不明錯誤送進 AEE 模型學習。
- **Grok 角色活測可明示關閉沙盒**：`eval --matrix --team-2x2 --team-grok-sandbox-off` 僅在當次隔離回合對 Grok 傳入 `--sandbox off`，讓無法套用沙盒的 macOS 主機仍能驗證真實團隊流程；報告會記錄此設定。正式 gateway 呼叫及其能力／MCP 工具限制維持原樣。
- **獨立執行的 P2b 量表沒有 MCP 認證，所有規劃角色都看不到交接工具**：探針現在先在指定的隔離 eval home 建立內部 MCP key，再複製到四個臂，並把 key 經既有子行程環境傳給角色 MCP server；正式 gateway 的 key 仍由原本啟動流程管理。
- **隔離團隊角色的 MCP 子行程誤指向來源首頁**：scaffold `.mcp.json` 與 Grok／Codex／Gemini／Antigravity 的 MCP 設定現在以當次角色首頁覆寫繼承的 `DUDUCLAW_HOME`，身分 token 也由同一首頁簽發；隔離量表不再把交接工具呼叫寫進來源首頁。
- **P2b 量表可設定單輪執行容量**：新增 `--team-fanout 1..3`（預設 1），讓多子任務案例的驗收契約與實際執行人數一致；粗估預算依此容量預留，報告明列設定值。
- **P2b 單一配對案例不再顯示假精確區間**：只有一個叢集或分數零變異時，角色格仍保留實測平均與 `unresolved`，但省略不可估的 CI 與 MDE；不再把 `[0,0]` 和 `MDE=0` 寫成像是已解析的數值。
- **P3 角色系統前綴穩定化第一層**：暫態角色的 `SOUL.md` 先放只依角色變動的契約，再以 Direct API 快取邊界隔開母員工的身分文字；任務、封包與驗收標準留在本次派工 prompt。Claude CLI 只對有效角色成員設定主對話及 subagent 的一小時快取 TTL；普通 ephemeral 員工的 `SOUL.md`／TTL 保持原樣。此變更只建立可快取前綴，不預先宣稱命中率或省費。
- **P2b 整輪團隊四格探針**：新增 `duduclaw eval --matrix --team-2x2`，同案例以規劃 weak/strong × 執行 weak/strong 跑四個隔離的 production composer 回合，固定跨家族審核者，用實際 PASS/FAIL 組成成對 Shapley 與 planner/executor 矩陣格。未走到審核的臂不計分、不形成假配對；報告保留各角色 outcome 與封包資訊，可固定四臂 `--team-effort`。缺驗收契約、缺角色模型、不可套用的 paired seed 都明確拒絕；預算在派工前估價停止。團隊角色沒有交出 TaskPacket 時，帳本列 `empty/missing_packet` 而非 `completed`。這是量表入口，尚未完成四題活體煙霧與 P3–P5。
- **Antigravity 原生事件與用量**：`agy` 1.2.10 的 `stream-json` 已在隔離活測觀察到，runtime 改讀最後的回覆與實測 input/output/cache-read token，並將終態工具事件送入既有遮罩後的 native collector；未結束的 ACTIVE 事件不當成工具成功。缺最後 result／用量會明確失敗。
- **Antigravity Gemini 3.7 無 effort 無法啟動**：`agy` 1.2.10 實測 `gemini-3.7-flash` 未帶 `--effort` 會在模型檢查階段拒絕；角色設定未指定 effort 時此模型家族預設 `medium`，其他模型的命令列保持原樣。這使隔離量表的 `antigravity:gemini-3.7-flash` 可實際執行。
- **Playbook 跨模型語義先保守落地**：條目新增 `transferability`（`fact_constraint`／`model_guidance`），舊資料與新自動產生條目預設為模型綁定的 `model_guidance`；gene 匯出與匯入保留此欄，避免語義在交換時遺失。跨模型注入仍待角色 scope 與證據驗證接線。
- **聯合團隊四格歸因的統計基礎**：加入成對 2×2 Shapley 計算，明列規劃／執行的 φ、交互作用與可信區間；一個叢集退 CLT，零寬區間不報精確度。整輪團隊量表的資料產生器仍待接線，現行 `--matrix` 不會冒充量到 planner。
- **非 Claude 基準逐字稿可明確重錄**：eval 案例可同時宣告 `[case] runtime` 與 `[case] model`，`--record` 依此固定組合走現有 runtime；CLI 的臨時 `--runtime`／`--model` 覆寫仍禁止錄製，以免覆蓋不相符的基準。非 Claude 的逐字稿標示為合成事件來源。
- **團隊審核的 Codex 嚴格判決格式**：生產 composer 與離線審核格共用同一份 `PASS`／`FAIL` JSON Schema，團隊審核可解析受約束的 JSON 回覆，同時保留其他 runtime 的首詞純文字判決路徑；格式不合法仍保守拒絕。
- **團隊角色成本可歸屬**：`token_usage` 增加可空的 `role`／`episode_id`，composer 把角色與任務 ID 透過 task-local 傳入既有成本紀錄路徑，`cost.by_role` 以管理員權限查詢實測的角色／模型支出；舊資料不猜角色，避免重複記帳。
- **團隊成本資料在回合結束前落盤**：非 Claude 角色的用量紀錄改在角色呼叫結束前完成，避免短命隔離回合清除資料庫後遺失；Claude CLI 的兩條用量路徑也寫入 task-local `role`／`episode_id`。P2b 探針把 process-wide 成本庫固定於操作者的 eval home，業務工具與任務仍在各臂副本執行。
- **能力量表預算價格閘**：`--matrix --budget-usd` 在第一個活體呼叫前載入 `<DUDUCLAW_HOME>/models.toml`，若任一候選模型仍無價格就明確拒跑，避免未知模型的固定估價偽裝成可執行的美元上限；同模型後續呼叫改以實測最高單次成本的兩倍預留。無預算的診斷量表保留已標示的粗估值；預算仍是估計，非供應商帳單硬上限。
- **Team-as-Agent 角色權限、容量、帳本、產物下載與檢視**：角色成員的 MCP `tools/list` 與呼叫閘門改用 `.ephemeral/<id>/agent.toml` 能力設定；scaffold 無效時兩端都拒絕。暫態成員不能建立派工器不會掃描的 TaskSpec。容量滿時 composer 等待自己的 FIFO 票券，票券過期或遺失會明確失敗，取消時釋出票券。角色回合帳本記下執行 runtime 回傳的 token usage；獨立審核 utility 亦有回合列，無用量或實際 provider 中繼資料時維持未知。接受的團隊任務會把 TaskPacket 明列、但未出現在 native Write 事件中的工作區產物封存到 `attachments/`，供既有下載入口使用，並沿用路徑與大小檢查。任務詳情新增「角色」分頁，透過 task Viewer ACL 讀取狀態、模型與已測得的 token 數。
- **codex runtime：讀得到 0.156.x 的 `agent_message` 文字**：`parse_codex_stdout` 只認舊版 `message/output_text` 事件，新版 CLI 的最終訊息是 `item.completed/agent_message/text`，抽不到就退化成 stdout 最後一行（`turn.completed` 的 usage JSON），判官／verifier 因此永遠解析失敗；P2 煙霧三實錘後補上此形狀（含真實串流測試）。
- **單一叢集的標準誤塌成 0，把「量不出來」講成「非常確定」（量表 smoke 3 抓到，同類一併掃掉）**：`--cluster-by dir` 之下，所有案例都在**同一個目錄**的套件只有一個叢集，而 Miller 的叢集穩健估計量在那裡**恆等於 0**——一個叢集時組間殘差和依定義為零，剛好把 CLT 項整個抵消掉。那是一個**沒用的估計量**的正確值，卻被當成信賴區間報出去：每一格都變成一個點（`mean 0.25 ci=[0.25,0.25]`）、兩個角色的 Δ 區間都是零寬（`Δ executor = +0.250 [+0.250,+0.250]`），於是**用 4 題就宣告瓶頸「已解析」**——正是這整層誠實機制存在要防的那句假話。修法：叢集數 < 2 時改報**未叢集的 CLT 標準誤**並且說出來（`se_source: "clt_single_cluster"` 配 `se_used`，兩個原始估計量 `se_clt`/`se_clustered` 仍照報留痕），格與**成對 Δ** 都套同一條規則；那是誠實的較弱估計（它看不見同目錄內的相關性，所以 `small_cluster_warning` 照樣亮），而不是一個編出來的零。真正的零寬區間（`n == 1`，或每筆觀測完全相同）才留著，但那是**未估到的離散度不是精確度**，所以該格強制 `unresolved`＋`verdict_reason: "degenerate_interval"`，建立在它上面的 Δ 標記 `degenerate`，`bottleneck()` 拒絕在它上面解析並點名每個被排除的角色與原因。**同類掃描**：普通單套件路徑的 suite 層列（`stats.suite`）有一模一樣的洞（它的逐目錄列早就因為這個理由改用 CLT 了，suite 列沒有），一併修掉並在 console 標明實際用的估計量（單叢集不再被標成 "clustered"）。活測實錘（4 題單目錄、混合黃金標籤、零 LLM）：修前 `±0.0pp`，修後 `n=4 clusters=1 pass=50.0% ±49.0pp (unclustered CLT, 1 cluster) | q=0.02 → unresolved`，`se_clustered: 0.0` / `se_used: 0.25` 並列可稽核。順手修掉 console 的第二個誤導：有均值但 `q` 算不出來的格原本印 "no usable observation"（**跟事實相反**），現在照印均值與區間，並說明 `q` 為什麼缺（`q=n/a (zero-variance interval …)`）。
- **codex 審核判決被當成 unparseable，因為解析到的不是 agent 的訊息（量表 smoke 3）**：報告裡每一筆 codex run 的 `verifier.first_line` 都是 `{"type":"turn.completed","usage":{…}}`——那是 codex 事件流的**最後一行**，不是 agent 的回答。根因在上游、且不在本波可改範圍：`runtime/codex.rs::parse_codex_stdout` 只認得 `item.type == "message"` ＋ `content[].type == "output_text"` 這個形狀，CLI 改吐 `agent_message` 形狀時它抽不到內容，`CodexRuntime::execute` 便退回 `stdout.lines().last()`——**該 runtime 的每個呼叫端都繼承這個缺陷**。eval 側現在先把 agent 訊息還原出來再評分（last-wins：codex `item.completed` 的 `agent_message`/`message`（`item.text` 或 `content[]`，比照 codex.rs 既有的雙名容忍）、claude 的 `assistant`/`result` 事件），**fail-open**：看起來不是事件流的、或還原不出訊息的，一律原封不動回傳，所以結構化判決 `{"verdict":"PASS"}`（沒有 `type` 鍵）永遠不會被誤判成逐字稿而改寫；判斷「是不是事件流」用的是封閉的事件型別白名單，不是猜。有觸發還原的 run 標記 `message_recovered_from_stream`，讓這個繞路**留痕而不是默默遮住上游的 bug**。執行格的合成逐字稿走同一個還原（否則 `output_contains`／`[[expect.grounded]]` 會拿 JSONL 去比對）。
- **團隊 verifier 每一輪都拿到零證據，WP-4 的獨立證據機制在正式路徑上是空轉的**：`run_verifier` 以 `tasks.claimed_at` 當證據時窗起點，但團隊任務**從不被認領**——`try_team_dispatch` 直接以 `team-composer` 身分 `complete_task`，全 repo 只有 MCP `tasks_claim` 會寫 `claimed_at`。於是 `claimed_at` 恆為 `None`，`tool_activity_block_for_agents` 第一行 `let since = since?` 直接回 `None`，verifier prompt 每一輪都渲染成 `(無工具活動紀錄)` 且 `<artifact_receipts>` 整塊消失——verifier 只剩封包自述可讀，正是 VP-CONTROL（arXiv:2609.10969）量到 40.9 pp 效果的那個證據來源被抽掉。修法：`run_team_round` 在第一個成員跑之前就取下本輪起點並傳給 verifier（取自當下而非回讀 store，才不會跟它要框住的成員賽跑）；settle 端（`dispatch_engine`）對團隊任務改讀 `task_iterations.dispatched_at`，退路是該輪 `role_turns.jsonl` 最早一列的 `timestamp`，兩者皆無時**不讀時窗**——grounding 退成 `Skip`，而不是像修前那樣回退到 `created_at` 把整個任務生命週期當本輪。同時 `member_ids_for_task` 的全輪次退路對團隊任務改成以本輪時窗過濾（`member_ids_for_task_since`），先前那句「`since` 窗口已經排除其他輪的成員」的註解對團隊任務本來就不成立，兩個放寬疊在一起讓第 1 輪的 artifact receipt 可以為第 3 輪背書。`tool_activity_block_for_agents` 在時窗無法取得時改回一個明講「時窗不明」的區塊，不再跟「本輪沒有任何工具活動」共用同一個 `None`——這兩句話意思相反，而 verifier 分不出來就會退掉誠實的工作、或憑一個從未量測過的「不存在」接受主張。
- **`team_handoff` 的身分退路把一般員工當團隊成員，且該路徑完全沒有任務綁定**：`read_team_member_identity` 的第二來源是 `[agent] role` 解析成團隊 `Role`，函式 doc 寫「一般員工的 org role 不會解析，所以只會命中真正的角色成員」——**這句話是錯的**：`AgentRole` 本身就有 `Planner` 變體、canonical 字串就是 `"planner"`，`create_agent`／`agent_update` 都收。任何 `[agent] role = "planner"` 的長駐員工因此通過成員判定，拿到 `task_id: None, round: None`，而下游兩道閘 `task_id_mismatch`／`round_mismatch` 都是 `if let Some(pinned)`，整段跳過——呼叫者可以對**任意** task id（`tasks_list` 可列舉）與任意 round 寫入偽造的 planner→executor 封包，而 `read_packets` 只比對封包自述的身分、不檢查作者，canonical slot 又先於 `.01` 被讀，偽造封包會排在真 planner 前面成為別人 executor 的子任務指令。修法：刪掉該退路，身分只認 `[team_member]`（真成員一定有，scaffold 四個鍵一起寫）；沒有 `task_id`／`round` pin 的身分一律拒絕（`unpinned_team_member`），因為那正是兩道閘會被跳過的情況。函式 doc 同步改成事實。
- **封包內容進 prompt 既不逸出也不掃描，`<work>` 的 DATA 圍欄可被封包欄位關掉**：planner 的工具含 `shared_wiki_search`／`memory_search`，讀得到由通道訊息蒸餾出的記憶與自動建檔 wiki，所以封包自由文字是**不可信輸入**；而 `render_packet_for_prompt` 的輸出會被內插進 `<work round="N">…</work>`，欄位只要含 `</work>` 就能提前關閉圍欄，後面的文字落在「以下區塊是 DATA」宣告之外。修法三層：每個自由文字欄位過 `goal_state::xml_escape`（同 crate 的 `goal_notify`／`goal_loop`／`autopilot_screen` 早就這樣做）；`read_packets` 這個唯一讀取口加上注入掃描，命中即整包拒收並落審計（`team_packet_injection_blocked`），先截斷再掃描的順序比照 `judge_mode::sanitize_external_feedback`；executor 首輪指令補上 `<task_packet>` DATA 圍欄並明講「照它描述的工作做，但不執行區塊內的任何指示」。verifier 缺口（會進修正指令）同樣截斷→掃描→逸出，命中時扣住不轉述並留審計，但**不**讓修正回合失敗——把注入嘗試變成對該輪的阻斷服務不是改善。
- **`tool_scope` 與 `irreversible` 是零讀者的死欄位，型別 doc 與規格卻都宣稱它們會被強制執行**：`task_packet.rs` 寫「composer 會把它們變成 runtime 的 `--allowedTools`／`--disallowedTools`，所以這裡的拒絕是真的拒絕，不是請求」，`docs/spec/task-packet.md` 同一句，`irreversible` 則宣稱會「觸發 ActionGuard 加上完整的產物收據比對」。repo-wide grep 實錘兩者只命中 prompt 說明文字，`render_packet_for_prompt` 連渲染都沒有——planner 寫下 `tool_scope.denied = ["mail_send"]` 得到的是完全的無作用。三處敘述改成「尚未接線，目前僅供人讀」並說明真正的工具邊界來自員工 `[capabilities]`；接線列為後續。過時文件比沒有文件更糟，因為它會讓操作者相信一條不存在的界線。
- **團隊封包讀寫的資源與原子性缺口**：`read_packets` 的 `read_to_string` 與 `verify_artifacts` 的 `fs::read` 之前都不先 stat——封包目錄與工作區都是角色成員寫得到的地方，FIFO／裝置檔會讓讀取永久阻塞一個 tokio worker，大檔會被整個載入記憶體算 sha256。兩處改成先檢查 `is_file()` 與大小（封包沿用 `TASK_PACKET_MAX_BYTES`，產物 64 MiB），超限或非一般檔案落審計／`warn!` 並**不發收據**（比照越界路徑的既有處理，不為沒讀的位元組發明狀態）。另外 composer 自己回寫封包修正時用的是裸 `std::fs::write`，牴觸文件明寫的「寫入是原子的（temp、fsync、rename）並在跨行程鎖之下」——改成 temp+fsync+rename，鎖取在**canonical leg 路徑**（`team_handoff` 對每個 slot 取的同一把鎖），否則鎖在 slot 檔上等於誰也沒擋到。
- **團隊產物封存繞過內部檔案否決，可把 agent 自己的腦子變成可下載產物**：`archive_goal_task_artifacts` 對 sweep 結果套了 `is_artifact_path`，但 team packet 候選是**接在 filter 之後**才 extend 進去的，之後只檢查 containment 與大小。一個封包宣告 `artifacts[].path = "SOUL.md"`／`"state/working_state.json"`／`".claude/settings.json"`（全在工作區內，containment 通過）就會被複製進 `attachments/` 並由 `/files` 提供下載。修法：把 `is_artifact_path` 的否決半邊抽成 `is_internal_agent_path` 並套到封包候選上。**刻意不套副檔名白名單**——那是用來從檔案 sweep 裡「猜」產物的啟發法，而封包的 `artifacts[]` 是明示交付，`.txt`／`.json`／`.py` 都是真的產物（活測實錘：整套 `is_artifact_path` 會把既有測試的 `notes/result.txt` 一起擋掉）。
- **團隊輪在盲觀測下只記功不記過**：成員的 native 事件寫在**成員自己的 id** 底下，而 `observe_round`／`capability_blocked_in_window` 仍只讀員工單一 id，所以團隊輪的 `observation.fidelity` 結構性為 `None` → `fault_attribution` R0 回 `Unknown` → 該輪被排除學習；但 `Negligible/Moderate` 輪仍照常給注入規則加 `helpful`，形成不對稱膨脹。在證據聯集接進那兩個呼叫端之前，先讓 `Unknown`（盲觀測）**同時**抑制 helpful 側：看不見的一輪在兩個方向上都沒有證據。
- **故障歸屬把憑證失效算成模型的錯、把 `McpOnly` 的結構性空值算成 harness**：`is_environment_failure` 漏掉 `AuthFailed` 與三個 `AccountsCoolingDown*`，憑證失效造成的失敗輪落到 R3/Model → `counts_for_learning() == true` → 進 `MistakeNotebook` 與 playbook credit，方向正好相反，補進 R2。另一邊 R3 第一條 `native_tool_events == 0 && reply_claims_tool_use → Harness` 沒看保真度：`McpOnly`（專案自稱的主分支）與無 native collector 的 PTY pool 路徑下零 native 事件是**結構性**的，搭配 `TOOL_USE_CLAIM_PHRASES` 裡的 `已執行`／`已查詢`（`word_contains_ci` 對 CJK 退化成純子字串），一則「任務已執行完畢」的正常回覆就會把真正的模型過失標成 Harness 並排除學習。該條加上 `fidelity == Full` 條件；capability 被擋那半邊不加條件，因為一筆拒絕列本身就是正面證據。
- **never-trim 標題是任何通道使用者都能打出來的純文字，一行字即可關閉壓縮並把任意文字永久釘進 session summary**：`is_never_trim_header` 比對的是 `## 約束`／`## Constraints`／`## 受眾`／`## Audience` 四個**通用 markdown 標題**，而 `compress_history_for_budget` 的 `history` 就是十一通道的對話歷史、含使用者自己送的訊息。使用者送一則含單獨一行 `## Constraints` 加大段文字的訊息後：floor 超過預算 → `BudgetExceeded { stages_tried: [] }` → `channel_reply` 的 `!(stages_tried.is_empty() && protected_section_tokens > 0)` 讓這種情況**連非同步 bisect summary 都跳過**；更糟的是 `session_summarizer_task` 把 protected 文字以原文**無上限**寫進 `set_summary`，此後每一輪都注入系統提示，且與預算設定無關、不會自癒。本次是**有界緩解**不是根治：floor 只承認至多 `NEVER_TRIM_FLOOR_MAX_TOKENS`（6k，高於一個合法封包的上限 ~4.5k，且是**整份 history 的總和**所以也擋住分散多則訊息的手法），超出部分視為一般可壓縮內容並留痕，管線因此一定會跑；session summary 的 protected 段加 4 KiB 硬上限，截斷這件事寫進被持久化的文字本身而不只寫進 log。真正的修法是把豁免權綁在**來源**（只有 composer 能發出的 sentinel）而不是文字上，那是設計決定，列為待拍板。
- **`paired_comparison` 在單目錄套件上的標準誤恆為 0，把「估計量退化」誤標成「資料不夠」**：`evals/demo/*.toml` 全在同一目錄（＝ smoke run 的實際形狀）只有一個叢集，`se_clustered` 在那裡**恆等於 0**；`mod.rs` 的 `effective_variance = 0` → `n_required = 0` → `q = NaN` → `classify` 回 `(Unresolved, Candidate)`——於是 `--baseline` 在單目錄套件上**永遠 unresolved，再多 case 也救不回**，而 `Candidate` 的語意是「樣本數不足」。同一批程式的 `matrix::choose_se` 與 suite row 早就做了退回，唯獨這裡漏掉。兩個分支都改走 `choose_se`，`PairedResult` 與 `baseline_comparison` JSON 帶上 `se_source`／`degenerate`；既有那條只斷言 `fallback_to_unpaired` 旗標、**沒斷言 `se`** 的測試（正是這個缺陷溜過去的原因）補上 SE 與區間寬度的斷言。
- **`[case] runtime` 只被驗證、從未被任何執行路徑消費**：作者照文件寫 `[case] runtime = "codex"` 加 `model = "gpt-5.6-sol"`，不帶 CLI override 跑 `--record`（`record_override_conflict` 唯一允許的錄製方式），`RunOverrides::effective_runtime()` 連 `case` 參數都沒有，回 `Claude` → `is_claude_cli_path()` true → 把 `gpt-5.6-sol` 當 `--model` 餵給 `claude` CLI，宣稱的 non-Claude baseline 根本不會發生。`effective_runtime`／`is_claude_cli_path` 改接 `&EvalCaseFile` 並以 `case.case.runtime` 為 fallback（CLI override 仍優先），與 `effective_model(case)` 對稱。`docs/guides/evals.md` 同檔自相矛盾的舊敘述（「non-Claude baseline 需要一個 P2 刻意沒有加的 `[case] runtime` 欄位」）刪除，`[case]` 範例補上 `runtime` 與 `team_acceptance`。
- **`--repeats` 在兩個團隊量表出口都被當成獨立案例，憑空製造 √K 的解析度**：`team_probe` 的 `matched` 鍵是 `(domain, case_id, repeat)`，`--repeats 3` × 3 case 產生 9 個 row 全數進 `joint_shapley_2x2` 與四個 `cell_from_scores`，`MatrixCell.n` 記成 9——而該型別契約明寫「Distinct cases contributing to mean（**NOT runs**：K repeats of one case aggregate into that case's own pass rate first）」，`matrix.rs` 一直有 `per_case` 收斂，`team_probe` 沒有。同一份報告裡 `verifier_cell` 的 Wilson 分母也是 runs 不是 cases，被高估精度的正好是文件點名「最貴的錯誤」false-accept。兩處都改成先按案例折疊：team_probe 進 `rows` 前對 repeat 取平均（某臂在所有 repeat 都沒完成的案例維持 incomplete，不會被平均成 0.0），verifier tally 改以案例為單位多數決，平手一律往保守方向判（平手不算 agreement、平手算 false accept／false reject），並新增 `cases` 欄讓同一份報告裡的兩個 n 不再互相矛盾；`--repeats 1` 時兩者都是恆等變換，數字逐位不變。
- **退化的矩陣格仍把零寬區間與 `mde = 0.0` 寫進 `role_model_matrix.toml`**：`matrix.rs` 的 JSON 半邊早就報 `degenerate_interval`，但**持久化的 cell** 照寫 `ci95_low`/`ci95_high`/`mde`，違反該型別模組 doc 的「zero variance ⇒ absent key」，等於把一個讀起來像確定的區間交給 P5 要讀的耐久 prior（`team_probe::cell_from_scores` 一直有做，`matrix.rs` 沒有）。改成退化時三個鍵一律缺席；既有那條只檢 JSON 半邊的測試補上對 `matrix_cell` 的斷言。另外 `team_probe` 的 `--budget-usd` 截斷在 console 完全靜默（`matrix.rs` 有 WARNING），`matrix.save()` 照寫、exit 0，操作者看到的畫面與跑完全程相同——補上 WARNING，明講停在哪個案例／哪個臂、跑了幾筆、矩陣是**部分**的。
- **Claude utility 路徑把 `hint.effort` 整個丟掉**：Claude 正是預設 utility provider（團隊 verifier、goal-loop 判官都走這裡），`call_claude_cli_public` 對 `call_claude_cli_rotated` 的 effort 參數硬寫 `None`，連 `output_schema` 都還有的 `debug!` 留痕它也沒有。接線需要改 `channel_reply` 的函式簽章（不在本波範圍），先補 `warn!` 讓「我的判官無視 `effort = high`」從讀原始碼才知道變成看 log 就知道；接線列為後續。
- **同一個 UTC 日可以靠 RFC3339 拼法差異凍結兩份影子預測**：`decision_shadow_targets` 的每日唯一性靠 SQLite `CAST(strftime('%s',target_day_utc) AS INTEGER)` 比對，寫入端的守衛卻是 chrono。chrono 接受 `t`／`T`／空白三種分隔符與 `+00:00`，SQLite 的日期函式對小寫 `t` 直接回 NULL，`NULL = ?` 與 `NULL >= ?` 都不成立，於是用 `2026-03-01t00:00:00Z` 建的預留對每一道日期閘都是隱形的：同一天可以再凍結第二份獨立預測、兩份都能被當成合法的事前承諾讀回與評分，而 `supersede_shadow_policy` 的「cutoff 之後已有預留就不准交接」與 `validate_shadow_sla_prior_boundary` 的跨日開單身分比對會直接放行（fail-open）。新增 `shadow_utc_day_key()`：政策生效區間、交接 cutoff、預測目標日在寫入前一律正規化成 `YYYY-MM-DDTHH:MM:SSZ`，別名拼法照常接受但絕不原樣落地；五處 `strftime` 查詢補上 `IS NOT NULL`，並在讀取前先檢查同一 lineage 是否存在 SQLite 解析不了的舊列，有就回 `Corrupt`（fail-closed），不再當成「這一天沒有預測」。回歸測試先用一個一次性測試實錘「chrono 收、SQLite 回 NULL」這個前提，再鎖住別名先寫、正規後寫必須衝突。
- **16 條 decision 端點在授權前就把 body／query 反序列化了**：`/api/decision/compare`、`engineering-validation`、`forecast-validation`、`empirical-resampling`、`synthetic-pilot`(+`/lifecycle`)、`event-compare`、`replay`、`pilot-review/request|status`、`policy-sweep`、`sensitivity`、`ticket-sources/scrub` 與三條 `Query` 端點（`overview`／`catalog`／`shadow-monitor`）用的是 axum `Json<T>`／`Query<T>` extractor，而 extractor 在 handler 本體之前跑完，`authorize_causal_admin` 永遠沒有機會先開口。未認證的呼叫者送一個 `{"tenant_id":1}` 就會拿到 422 與 serde 訊息（`deny_unknown_fields` 回吐欄位名、`invalid type` 回吐送進去的值），等於未認證即可測繪這些 admin-only 端點的精確 schema。同一批程式的 shadow／model-candidate／outcome 端點本來就走 `body: Bytes` ＋先授權再解析，現在這 16 條一律對齊：`Json` 型改 `Bytes` ＋ `parse_decision_model_request`（`empirical-resampling` 沿用它自己的 32 KiB 上限），`Query` 型改 `RawQuery` ＋授權後手動解析（重複鍵與未知鍵都拒絕，維持原本的 `deny_unknown_fields` 語意）。新增表驅動回歸測試：無 token 對十六條端點送壞 body，一律 401/403，且回應不得出現送進去的欄位名、值或任何 serde 措辭。
- **工單原文的保留期限沒有任何排程執行者**：`decision_ticket_source_blobs.retention_until` 過去只有三種方式會被執行：管理員按 `/api/decision/ticket-sources/scrub`、手動跑 `duduclaw decision-scrub-expired-ticket-sources`，或剛好有人讀到那一筆而觸發 lazy 撤銷。沒有人做這三件事時，逾期的原始工單列就一直留在 SQLite 檔裡。gateway 開機起比照連接器生命週期任務，額外排一個每小時的掃除任務逐 scope 執行保留期限；失敗的 scope 逐一留 `warn` 並繼續掃其他 scope（保留期是對所有 scope 的承諾），從不中止 gateway；`decisions.db` 不存在時整個跳過，不會替沒用過 Decision Lab 的部署憑空建檔。
- **彙總影子篩選載入時不重算裁決**：`load_shadow_review_screen` 只驗雜湊與來源綁定，沒有用當前引擎把評估重跑一次比對裁決；SLA 版與 outcome-model 版都有這道。取得 `~/.duduclaw/decisions.db` 寫入權限的人只要把 `failed_checks` 清空、`eligible_for_human_review` 設成 true、雜湊自行算好，就能通過 `request_shadow_screen_review` 並產生一張人工檢視收據。現在引擎相符時一律重算並逐欄比對，不符回 `Corrupt`。回歸測試把一份「裁決被竄改但雜湊自洽」的紀錄寫進儲存後要求載入失敗。
- **大整數誤差和與 KPI 以原生 JSON number 回傳，前端無法用 BigInt 檢查**：同一條工作線的其他回應早就把 u64／u128／i128 轉成十進位字串、前端也確實用 `BigInt()` 比較，只有四處沒照辦（`DecisionForecastValidationSummary` 的五個誤差和、`DashboardCandidateKpis` 的五個 KPI、候選比較的兩個 delta、engineering-validation 內嵌的 backtest 與 SLA holdout 誤差和）。超過 2^53 時 `JSON.parse` 已先截斷，而前端正是用 `final_backlog_delta === candidate.final_backlog - parent.final_backlog` 這條等式偵測後端竄改，精度一掉這個檢查就會誤判。四處全部改成十進位字串（engineering-validation 用只供輸出的鏡像型別，儲存紀錄仍是 `u128`，歷史 payload 摘要逐位不變），前端型別、驗證與渲染改 `decimal()` ＋ `BigInt()`。
- **來源版本清單靜默截斷成 16 筆，看起來像完整清單**：三個儀表板投影一律 `.take(16)`，但一張 21 天的彙總篩選收據綁的是 42 份來源（每個到期日 2 筆），SLA 版每天 4 筆，366 天上限 732 筆；前端 `validScreen` 驗的是 `length <= 16`，於是把截斷後的清單當成完整清單接受並渲染給人看。回應新增 `source_version_hashes_total`，前端在截斷時顯示「(16 / 42)」，儲存端不受影響（完整清單仍在紀錄裡）。
- **改寫篩選雜湊只清掉篩選，屬於舊篩選的評估數字留在畫面上**：`DecisionShadowWorkflowPanel` 的篩選雜湊欄位 `onChange` 只清 screen 與 review，`assessment` 沒清，於是「篩選」卡片消失、「評估」卡片仍顯示上一張篩選凍結的到期／已評分／技能／覆蓋率，看起來像當前政策的即時評估。該路徑補上與 `loadScreen()` 相同的清除。
- **`decision-event-replay --save-run` 用無上限 `std::fs::read` 重讀來源檔**：同檔其餘 12 處來源讀取都是 `take(2 MiB + 1)` ＋長度檢查，只有 `--save-run` 那一次會在被拒絕之前先配置整個檔案大小的記憶體。兩次讀取現在共用同一個有上限的讀取函式。
- **本機 `decision-import-pilot` 沒有拒絕保留的合成前綴**：儀表板匯入明確拒絕 `snapshot_id` 以 `synthetic-support-` 開頭（避免操作者上傳的資料佔用內建 fixture 的命名空間），本機 CLI 匯入沒有同一道檢查。**行為變更**：CLI 匯入現在也拒絕該前綴，以合成 fixture 直接跑本機匯入的流程需要改用自己的 `snapshot_id`。
- **兩個抽樣原語沒有自我防護**：`DrawStream::inclusive` 對 `min > max` 會在 release build 繞回巨大的 `width` 讓拒絕取樣門檻失效，`sample` 對空切片會在 `len() - 1` underflow。前者加 `debug_assert` ＋ `saturating_sub`，後者改回 `Option`；目前所有呼叫端都先驗證過，兩者是下一個呼叫端的防呆。
- **`DecisionStore::open()` 每一次純讀取都重建 schema 並取寫鎖**：全檔 33 處 `open()` 大多是純讀路徑，每一次都無條件跑 21 句 `CREATE TABLE/INDEX IF NOT EXISTS` 再 `BEGIN IMMEDIATE` 做遷移檢查；`assess_shadow_policy` 的每日迴圈上限 366 天，一次請求可以做上千次寫鎖取得，5 秒 busy timeout 下會與任何並行寫入互相序列化。現在先讀 `PRAGMA user_version`，與當前 `SCHEMA_VERSION` 相符就整段跳過（常數旁註明「改動 DDL 或遷移必須同步 bump」），`assess_shadow_policy_at` 的每日預留查詢也改成迴圈外開一條連線重用。
- **歷史模擬結果讀不到「這是舊引擎算的」訊號**：`load_daily_run`／`load_event_run` 刻意不重算（升級引擎不該改寫歷史結果），但讀者也拿不到任何提示。新增 `load_daily_run_with_engine_state`／`load_event_run_with_engine_state` 回傳紀錄加一個 `engine_matches_current`，並替 `StoredDailyRun` 補上與 `StoredEventRun` 對稱的註解；事件引擎識別碼改由 `decision_event::event_engine_sha256()` 單一來源提供，不再在兩處各算一份。
- **codex runtime：`codex exec` 加 `--approve-for-me`，並支援 `--output-schema`**：codex 0.156.x 對每個 MCP 工具呼叫都要求核准，`approval_policy=never` 反而變成自動拒絕，`mcp_servers.<id>.default_tools_approval_mode="auto"`（`-c` 與 config 檔皆試）與 `projects.<path>.trust_level` 都無效；活測六種變體實錘只有 `--approve-for-me`（自動審核，非 dangerous bypass）能讓 MCP 呼叫通過；它與 `--sandbox <MODE>` 互斥，sandbox 等級改以 `-c sandbox_mode="<level>"` 宣告（實測 `read-only` 在此模式下**不會**真的擋寫入，codex 的 ReadOnly 平價降為 advisory；`danger-full-access` 維持顯式 bypass 旗標）。活測第八輪另證實 codex 判官的散文回覆會被兩個 parser fail-closed 拒收，所以本次也接上 `codex exec --output-schema <FILE>`（官方 0.156.1 旗標）：呼叫端給 JSON schema 時寫進暫存檔並傳旗標，暫存檔握到 spawn 結束才釋放；沒給 schema 時 argv 與此前逐位相同，寫檔失敗一律降級成「不帶旗標」而非讓裁決失敗。
- **codex runtime：`codex exec` 加 `--skip-git-repo-check` 並關閉 stdin**：agent 目錄不是 git repo，codex 0.156.x 在非信任目錄會以「Not inside a trusted directory and --skip-git-repo-check was not specified」退出（活測第四輪實錘，暫存目錄手動重現）；stdin 未關時 codex 會等待「additional input from stdin」。與上一條 env 引號化同屬既有 codex-runtime 缺陷。
- **codex runtime 的 `-c` 設定覆寫把字串值送成 TOML 整數，每一次 codex spawn 都在起手死掉（既有 codex-runtime bug，與團隊無關）**：`codex exec -c key=value` 的 value 半邊是**當 TOML 解析**的（`codex exec --help` 明載「The `value` portion is parsed as TOML. If it fails to parse as TOML, the raw string is used as a literal」），所以剛好能解析成「錯型別」的值會被照那個型別收下。`runtime/codex.rs::mcp_override_args` 原本用 `format!("mcp_servers.duduclaw.env.{k}={val}")` 裸貼，gateway `.mcp.json` env 區段裡的 `DUDUCLAW_PORT`（JSON 字串 `"18999"`）就變成 TOML 整數 `18999`，而 codex 那邊 `mcp_servers.<id>.env` 的型別是 `HashMap<String, String>` ⇒ `Error loading config.toml: invalid type: integer \`18999\`, expected a string in \`mcp_servers.duduclaw.env.DUDUCLAW_PORT\``（exit 1）。**這不是團隊路徑的缺陷，是既有 codex runtime 的 bug**：它同樣打死每一個普通 codex AI 員工，也同樣打死 `[dispatch] judge_provider = "codex"` 的驗收判官（與 P0 活測第一輪的 `--ask-for-approval` 是同一族，只是被跨家族備援接走後看起來像「codex 有點慢」）；團隊活測只是它第一次被實錘。修法：新增 `toml_string_literal()` 單一引號化來源（TOML 1.0 basic string 規則——反斜線與引號轉義、四個緊湊控制字元轉義、其餘控制字元 `\uXXXX`、非 ASCII 原樣保留），`command` / `args` / 每一個 env 值全數走它；非字串 JSON scalar（數字、布林）改**字串化**而不是靜默跳過（env 的語意就是行程環境，一切都是文字，跳過等於讓成員的 MCP server 少拿到 port）；陣列／物件／null 沒有環境變數語意，照舊跳過。`render_mcp_toml` 裡那份只轉義 `\` 與 `"` 的本地拷貝同步收斂到同一個函式（順帶補上控制字元，先前一個控制字元就能產生解析不了的 config.toml）。新增回歸測試把每個送出的值 parse 回 TOML 並斷言型別是 string。
- **團隊角色成員在「即將被刪掉的目錄」裡工作，驗收於是看不到任何工具活動（活測第三輪 E3）**：成員的 cwd 原本是自己的 scaffold（`<home>/agents/.ephemeral/<eph-id>/`），它把 `notes/a.md` 寫在那裡，回合結束 `finish_role_member` 立刻刪目錄，後續成員又把已拆掉的目錄建回來（殘留 `eph-…-planner-…/notes/`）；審核者與 settle 的 evaluator 於是只能誠實地說「no tool activity supports file creation」並駁回每一輪。修法：scaffold 只放設定（`agent.toml` / `SOUL.md` / `.mcp.json` / `.claude`），**工作一律在母員工的工作區**（`<home>/agents/<employee>/`，與該員工單跑一輪時 `claude_runner::effective_work_dir` 用的同一個目錄）。搬 cwd 會帶出身分問題——Claude CLI 從 `<cwd>/.mcp.json` 自動發現 MCP server，成員坐在母員工目錄裡就會以母員工身分開 MCP server（工具呼叫、handoff、稽核歸屬全被算到母身上）——所以身分改成**顯式指名**：Claude 成員帶 `--mcp-config <member_dir>/.mcp.json --strict-mcp-config`（後者才能阻止 CLI 另外併入環境設定），codex 成員帶 `--cd <employee_dir>`，它的 `-c mcp_servers.duduclaw.env.DUDUCLAW_AGENT_ID` 覆寫本來就與 cwd 無關。成員 `.mcp.json` 不存在時**拒絕派工**（`member_mcp_config_missing`）而不是以母身分跑。gemini／antigravity／grok／generic-CLI 的 MCP 註冊檔就在 cwd 裡（`.gemini/settings.json`、`.grok/config.toml`…），搬 cwd 會連身分一起搬走，故**明確標為「成員尚不支援」**並文件化：這些 runtime 的成員仍在自己的 scaffold 裡工作，檔案不會活過該回合。另：codex 成員在 cwd 被覆寫時，角色指令改以 `<role_system_prompt>` XML 區塊放進 prompt（`codex exec` 0.156.1 沒有 system-prompt 旗標，工作根目錄的 `AGENTS.md` 是唯一檔案通道，寫進母員工的 `AGENTS.md` 會蓋掉它、並與同輪另一名成員互搶），這是刻意的取捨而非靜默丟棄。`finish_role_member` 補上「刪完又冒出來就 `warn`」的留痕（重入仍是 no-op）。封包 `artifacts[].path` 現在會做母工作區 containment 檢查，逃逸者**拒絕並留審計**（`team_packet_artifact_refused`），既不靜默接受也不靜默丟棄；存在的路徑走 canonicalize（抓得到指向外面的 symlink），尚未寫出的路徑退回詞法檢查（`..` 不得爬出去）。
- **角色成員會靜默換家族做完工作，帳本卻照抄設定（活測第三輪 E2）**：codex executor 每次 spawn 都被上面那個 TOML bug 打死，`failover.rs` 於是把它換成 Claude 做完工作，而 `role_turns.jsonl` 仍記 `runtime=codex request_model=gpt-5.6-sol outcome=completed`——帳本說了謊，執行者/審核者去相關性（團隊存在的理由）也被抹掉。修法兩件：① 角色成員一律 `allow_cross_family_failover = false`（WP-B 補丁已有的欄位，經新的 `DispatchOverrides { work_dir, allow_cross_family_failover, mcp_config_path }` 從 composer 一路帶到 `claude_runner`，非 Claude runtime 走 `runtime::SPAWN_OVERRIDE` task-local；既有呼叫端逐位不變）；成員 spawn 失敗 ⇒ `team_stage_failed{role, runtime, model, error}` 審計＋`role_turns` 記 `outcome=failed`，交給既有降級鏈（executor 副本 → 審核不給修補回合 → needs_human），**絕不同輪換家族**。② `role_turns.jsonl` 新增 `runtime_used` / `response_model` / `failover` 三欄，由執行路徑自己回報（`failover.rs` 主／備兩條腿與 `claude_runner` 的 CLI 輪替成功處皆寫入 `runtime::record_runtime_outcome`），`provider` 也改成「真正回答的那一家」而不是設定的拷貝；沒有回報者的列三欄一律缺席（不猜）。舊列（沒有這三個鍵）照樣可以反序列化。
- **審核者與 settle 只看一個 agent id 的工具證據，所以團隊回合看起來像「什麼都沒做」（活測第三輪的結構性缺口）**：團隊的工作是 ephemeral 成員以**自己的 id** 做的，母員工的 `[claimed_at, now]` 稽核窗口裡往往只有開回合的那一通 bookkeeping 呼叫。新增 `role_turns::member_ids_for_task_round()`／`member_ids_for_task()`，審核者的 `<tool_activity>` digest 與 settle 的證據匯聚（grounding precheck ＋ MAV 判官 digest）改成讀「母員工 ∪ 該輪成員」的聯集（`dispatch_engine::tool_activity_block_for_agents` / `read_tool_activity_records_for_agents`，重複 id 收斂、單一 id 時逐位相同）；round 對不上時退回整個任務的成員集合（goal loop 的 in-flight `iter` 與 settle 的 `revision_round + 1` 是兩個各自維護的計數器，重啟會分歧，而時間窗本來就排除了其他輪的成員）。
- **封包的證據保真度（`fidelity`）沒有人填，全部是 `none`（活測第三輪 E4）**：那個欄位是模型打什麼就是什麼，而沒有模型會打。改由 composer 依**實際觀察**回填：runtime 自己的工具事件流有記錄 ⇒ `Full`，成員在派工時窗內有 MCP 稽核列 ⇒ `McpOnly`，都沒有 ⇒ `None`；成員自報的值一律覆寫，不一致就留審計（`team_packet_fidelity_corrected`）並把磁碟上的封包改正（後續每一個讀者看到的都是真話，不只這一輪）。扇出時同一條 leg 上有多名 executor，所以「回合結束時 leg 上有哪些封包」不等於「這名成員寫了哪些封包」——派工前先對該 leg 取一份逐位快照（不只看檔案存不存在，因為修補回合會把同一個 `packet_id` 重寫回同一格），只有新增或內容改變的檔案算這名成員的，免得把 A 的觀察蓋到 B 的封包上（那正是這個欄位存在要防的「憑空證據」）。
- **團隊角色成員交不出封包（`team_handoff` 讀錯目錄 → `not_a_team_member` → 整輪 `planner_no_packets`）**：角色成員是 scaffold 在 `<home>/agents/.ephemeral/<eph-id>/` 下的，但 `team_handoff` 的身分讀取寫死 `<home>/agents/<caller>/agent.toml`，於是每一個真實角色成員的 `[team_member]` 區段都讀不到。第二輪活測實錘：planner（Claude Sonnet 4.6）兩分鐘內呼叫 13 次，前 12 次因封包格式被拒，好不容易湊出一個驗證器收的封包，卻換來 `not_a_team_member`，然後放棄，該輪結束於 `planner_no_packets`。改走既有的 `ephemeral::resolve_agent_dir`（它本來就用雙向 canonicalize 證明 containment，非 `eph-` id 回 `None` 正好落回原路徑），兩種佈局都通；判定「誰算成員」一個字沒放寬——`.ephemeral/` 下沒有 `[team_member]` 的普通臨時工還是照樣被拒。順帶修掉同一條路徑上的產物歸屬：provenance 以 `<home>/agents/<caller>` 的正規拼法交給 `artifacts::record_saved`，否則角色成員的那一行會落進 scaffold 自己的 ledger、`agent_id` 還是空字串，任務詳情頁的「產物」分頁永遠看不到。
- **封包格式對模型過嚴又講不清（12 次 `invalid_packet` 全花在格式上，不是花在工作上）**：同一次活測的另一半。工具描述只列了 7 個「required keys」加一句「see docs/spec/task-packet.md」——那份文件模型打不開。三處一起修：① `output_format` 現在也吃裸字串（`"markdown"` / `"json"` / `"diff"` / `"files"`，trim + ASCII 大小寫不敏感、精準 token 比對），寫進磁碟前一律正規化回既有的 `{"kind":…}` tagged 形式，所以 composer 與任何讀者看到的拼法完全沒變（要帶 schema 仍然只能用 tagged 形式）；② `goal_id` / `round` / `from_role` / `to_role` 可以整個省略，由呼叫者自己的 `[team_member]` 紀錄補上（`to_role` 從 `from_role` 推得——三個流程角色各自只有一條合法出邊），**但呼叫者寫了的值一律原封不動送去交叉檢查**，冒用他人角色／任務／回合仍然照舊被拒，補值是便利不是後門；③ 拒絕訊息改成可行動：`invalid_packet` 直接把 serde 原文擺第一（`` missing field `objective` ``、`` unknown field `objectives`, expected one of … ``），後面接一份七鍵最小範例與可選鍵清單；驗證錯誤則指名到「哪一筆」（`constraints[3].text is 240 chars (max 200)`、`audience[1]`、`acceptance[0].value`），十二條約束時「是哪一條」正是可行動的那一半。最小範例只有一份權威（`duduclaw_core::task_packet::MINIMAL_PACKET_EXAMPLE`），工具描述、拒絕訊息、composer 角色抬頭（已預填該角色的 `to_role`）三處由測試釘住不准漂移。`deny_unknown_fields` 與 provider 欄位（`transcript` / `tool_use` / `thinking` / …）的硬拒絕完全沒動。
- **codex runtime 自 Codex CLI 0.156.1 起每一次 spawn 都失敗（`--ask-for-approval` 這個旗標已不存在）**：`runtime/codex.rs::sandbox_args` 送的是 `--ask-for-approval never --sandbox <level>`，但 0.156.1 的 `codex exec` 直接以 `error: unexpected argument '--ask-for-approval' found`（exit 2）拒收——也就是說**每一個 codex AI 員工的每一次呼叫都在 spawn 當下就死掉**，然後靜靜地被跨 runtime 備援接走，看起來像「codex 很慢／怪怪的」而不是「codex 完全沒在跑」。本次由真實判官測試抓到。`codex exec --help` 在 0.156.1 只列出 `-s/--sandbox`、`--approve-for-me`、`--dangerously-bypass-approvals-and-sandbox` 與 `-c/--config`：核准政策已經從旗標改成 config key，所以現在改走 `-c approval_policy=never`，跟既有註冊 MCP server 用的是同一個 `-c` 覆寫通道，`--sandbox <level>` 維持不動。**本機實測驗證，不是推論**：`codex doctor` 回報 `approval policy OnRequest`、`codex doctor -c approval_policy=never` 回報 `approval policy Never`、`codex doctor -c approval_policy=untrusted` 直接 1 fail——與官方 config reference 一致（`approval_policy` 收 `on-request | never | {granular=…}`，並明講「`untrusted` 不支援、`on-failure` 已棄用，非互動執行請用 `never`」）。**任何層級都不使用 `--dangerously-bypass-approvals-and-sandbox`**：那會連 sandbox 一起關掉，`sandbox_level_for` 的意義就沒了；連 `FullAccess` 也維持明確的 `--sandbox danger-full-access`（0.156.1 真實存在的 `SANDBOX_MODE` 值），讓層級是被宣告的而不是被繞過的。附上會鎖死這個旗標不再復活的回歸測試（三個 sandbox 層級各驗一次「沒有 `--ask-for-approval`」「核准政策仍被表達」「sandbox 未被繞過」）。**活測第五輪補完另外半邊：`approval_policy=never` 會讓每一個 MCP 工具呼叫被拒**——成員確實看得到 duduclaw 的工具（tool list 裡有 `mcp__duduclaw__team_handoff`、`working_state_handoff`），但每一次呼叫都回 `MCP tool call requires approval, but approval policy is never`。這是 codex 的 fail-closed 讀法而非故障：非互動的 `codex exec` 沒有人可以回答核准提示，需要核准的工具只能被拒。修法走官方 config reference（2026-09-24 取得）給的 per-server 出口：`mcp_override_args` 多送一條 `-c mcp_servers.duduclaw.default_tools_approval_mode="auto"`（同一個 `-c` 通道、同一個 `toml_string_literal` 引號化）。**這不是把核准關掉**：作用域只有 DuDuClaw 自己那台 server，codex 本身的 shell／apply_patch 仍受 `--sandbox` 層級約束，操作者另外註冊的 MCP server 各自保留預設，全域 `approval_policy` 維持 `never`、`--sandbox` 一個位元都沒動。真正的授權本來就在 MCP server 那一側（逐工具 `Scope::*`、agent 的 `allowed_tools`/`denied_tools`/`scoped_tools` 分派總門、`tool_calls.jsonl` 稽核），不在一個沒有人看著的 TTY 提示上。回歸測試鎖住三件事：覆寫存在且是 TOML 字串、`-c` 旗標與 payload 成對、以及這條覆寫不得外溢成全域 `approval_policy` 放寬或碰到 sandbox。
- **「執行」角色被發給三個唯讀工具，於是那個定義上「負責做事」的角色什麼都做不了（活測第五輪）**：`team_composer::plan_tools` 給 executor 的是與規劃者同一份固定清單（`team_handoff`／`memory_search`／`shared_wiki_read`），兩層強制各自把它讀成「不准寫」：**codex** 從能力集推導單一沙箱模式（`sandbox_level_for` → `write_tools_allowed`），一份沒有任何寫入類工具的 `allowed_tools` 就是 `--sandbox read-only`，成員回報 `工作區為唯讀，mkdir 與 apply_patch 均遭拒`；**claude** 把同一份清單原樣當 `--allowedTools`，少了 `Write`/`Edit`/`Bash` 是同一個拒絕，而且清單裡沒有 `mcp__duduclaw__*`，連 handoff 工具的限定名都落在白名單外。修法是**從母員工推導而不是寫死**：executor 拿「母員工的有效工具」＋`team_handoff`＋兩個既有唯讀助手；「母員工的有效工具」＝有白名單時原樣沿用該白名單，沒有白名單（＝不受限）時用派工路徑本來的預設有效集（`DISPATCH_DEFAULT_BUILTIN_TOOLS` ＋ `mcp__duduclaw__*`，即 `prepare_claude_cmd` 的 `DEFAULT_ALLOWED_TOOLS`）。因此**權限只跟著員工走、不會無條件放寬**：唯讀的員工仍然得到唯讀的執行者，母員工 `denied_tools` 裡的工具一律不請求（先前請求一個被禁的工具會讓 `check_tool_subset` 拒絕整個回合），落在非空白名單外的助手改成丟掉並留 `debug!`、不再為了一個便利工具賠掉整輪。`team_handoff` 仍然無條件請求，所以明確禁掉交接管道得到的是一個看得見的失敗回合（`TEAM_INTRINSIC_TOOLS` 原本就記載的行為）。規劃者與審核者維持唯讀子集（會改檔案的規劃者是在做執行者的事，能改寫自己正在評的東西的審核者不是獨立審核者）。為了讓「推導的清單」與「scaffold 實際檢查的信封」不可能漂移，`ephemeral.rs` 把母員工 `agent.toml` 的讀取收斂成單一 `read_parent_config`，並開一個 `parent_capabilities()` 給 composer 讀同一份；讀不到時 composer 退回預設信封、由 scaffold 產出那唯一一條 fail-closed 的審計拒絕，不多開一條錯誤路徑。
- **跨 runtime 備援不再把「別家的模型」交給備援後端**：`[runtime] fallback` 把呼叫轉到另一家 provider 時，`failover.rs` 原本拿同一份 `RuntimeContext` 直接執行——一個 codex AI 員工的 `[model] preferred = "gpt-5.4"` 會被拿去 spawn Claude runtime（`--model gpt-5.4`），後端拒收或悄悄替換，而 `model_used` 還回報 `gpt-5.4`，成本歸屬跟著錯。現在備援前會先決定模型，四個順序分支：① `agent.toml [model] fallbacks` 裡第一個「家族明確屬於該備援 runtime」的條目（`provider/model` 形式會依 Direct-API 既有方言去掉限定詞，家族辨識不出來的條目直接跳過——這正是 `model_matches_provider` 寬鬆規則會漏放外來 id 的那個缺口）；② 原本要求的模型本來就是該 runtime 服務得了的就原樣保留（`openai_compat` 不宣告任何模型家族，因此照舊代理任意 id）；③ runtime catalog 為該後端列的第一個模型；④ 都沒有就**拒絕 spawn**，回報 `no model configured for fallback runtime <P>` 並計為一次失敗（設定壞掉的備援會進冷卻，不會被無限重試）。每次替換都留一筆 `warn!`，帶 `agent`／`from_runtime`／`to_runtime`／`from_model`／`to_model`。
- **Telegram `/ask` 拿群組 chat_id 當 CCR principal，同群成員可以取回彼此的工具原文**：`handle_command` 的三個 `/ask` 分支把 `scope_id`（＝chat_id）當成回覆的 `user_id` 傳下去，它會成為 `CHANNEL_REPLY_USER_ID`，再由 `ccr_runtime::for_agent` 雜湊成 `source_acl`——同一個群組的 A 和 B 因此導出完全相同的 scope，B 的回合看得到 A 的 `## Historical CCR originals`，`duduclaw_ccr_find`／`duduclaw_ccr_retrieve` 也會通過所有檢查回傳 A 的來源片段。三處改傳寄件者本人的 Telegram user id；頻道貼文沒有 `from` 時傳空字串，`source_acl_for_principal` 會因此回 `None`，該回合的 CCR 直接關閉（fail-closed），不再退回房間 id。`/takeover` 那條同樣的 fallback 一併修掉。另外加了一條結構性回歸測試，掃這個檔案裡每一個 `build_guarded_reply_*` 呼叫的 `user_id` 位置，確保不會有人再把 `scope_id` 放回去。其餘十個 adapter 已核對過，傳的都是人的 id。
- **CCR 交付被拒時，那則引用了已撤銷來源的回覆仍留在對話紀錄裡，還會被蒸餾進記憶／wiki**：assistant 訊息在 `build_reply_with_session_inner` 裡就落庫，而租約要到 `GuardedReply::new()` 才第一次重驗——順序倒過來了，所以撤銷發生在送出前時，使用者看到固定拒絕文案，`session_messages` 卻留著那則答案，下一輪照樣當歷史送回模型、儀表板照樣顯示、`wiki_ingest` 照樣寫進記憶與自動建檔頁。改法是替每個回合建一道交付閘（task-local）：落庫時登記該列的 row id，蒸餾改成等交付判定後才啟動；判定為拒絕就同步把那列標 `undone_at` 並覆寫成拒絕文案（`get_messages` 與各儀表板查詢都已排除 `undone_at` 非空的列），蒸餾則直接不跑。沒有閘在範圍內的呼叫端（非通道路徑）行為逐位不變。
- **持有 CCR 交付租約時，wiki／memory 來源抹除觸發 `RAISE(ABORT)`，整個 `/api/causal/*` 回 500、上游記憶寫入與刪除硬失敗**：`causal_ccr_prevent_leased_update` 規定有活租約時不得改動 artifact 的 content／invalidated_at，但只有 retention 清掃寫了 `NOT EXISTS (leases)` 護欄，`scrub_wiki_artifact` 與兩個 memory trigger 都沒有。結果是：wiki 頁在交付期間被編輯 ⇒ 下一次 `CausalStore::open()` 內的 `sync_wiki_sources` 被 ABORT，而每一條因果讀寫都以 `open()` 起頭，因此治理面在租約釋放前全面回錯；memory 那側則是 `UPDATE memories` / `DELETE FROM memories`（含 GDPR 刪除）整句被回滾。兩處都補上同一條護欄，改成延後抹除：wiki 每次 open 重算失效狀態，租約一放掉就完成抹除；memory 則因為 trigger 一定會遞增 `causal_memory_revisions`，改由 open 的維護區段比對版本尾碼把落後的複本清掉。同時 `CausalCcrDeliveryLease::still_valid()` 對 memory 來源改看那個 revision 計數，所以上游一改動，在途的交付立刻失效（複本立刻不可交付，資料列稍後清除），不會用「延後抹除」換來「繼續交付舊位元組」。memory trigger 本體改為每次開啟重建（DROP＋CREATE 同一交易），舊資料庫不會繼續跑到修正前的版本。
- **`still_valid()` 看不到 wiki 頁面變更，provider 呼叫期間被改寫的頁面照樣產生候選**：租約的重驗刻意以唯讀連線查表（避免與寫入路徑互鎖），因此不會跑到唯一偵測活頁變更的 `sync_wiki_sources`；`causal_extraction_runner` 在 provider 呼叫前後做的四次檢查於是全部回 true。改成對 `wiki_agent`／`wiki_shared` 的 artifact 額外純讀檔比對匯入時釘住的 `raw_sha256`／`body_sha256`，不開寫交易、不取信任庫的柵欄鎖（信任狀態變更仍由下一次 open 捕捉，已在註解說明）。
- **到期、wiki 變更、memory 變更三條抹除路徑沒有清掉 `causal_claims.context_json` 與負對照覆核的 `rationale`**：這兩處存的是模型抽出的原始變數名與管理員自由文字，經常是來源逐字子字串，過去只有 `erase_artifact` 會清。現在抽成共用 helper（memory 端以相同 SQL 寫進 trigger body），四條路徑一致；保留期到了就是刪除承諾，不再留明文在磁碟上。
- **`leave-one-stratum-out` 對零效果估計誤報符號翻轉**：同檔另外兩個診斷都寫了「兩邊都不為零才算翻轉」與非有限值回 `None`，只有分層版沒有，於是 `estimate == 0.0`（合成資料常見）時任何正的重算都被報成翻轉，並持久化進 `diagnostics_json` 顯示在儀表板的敏感度欄位。補上同一組護欄並抽成可單測的函式。
- **`ccr-compare-run --base-url` 的 loopback 判斷可用 userinfo 繞過**：`starts_with("http://localhost:")` 這種前綴比對會讓 `http://localhost:1@evil.example/` 通過，而實際連線的主機是 `evil.example`，`resolve_env_key` 取到的真實 provider API key 會以 bearer 送過去。改成以 `url::Url` 解析後比對 host，並直接拒絕帶有帳號／密碼的 URL（慣例 ②：安全判斷不用未錨定字串比對）。
- **`import-wiki` 在驗證路徑之前就先 stat 檔案**：`live_page`／`live_shared_page` 先用未驗證的 `page_path` 取檔案大小，之後才在 `read_page_with_raw` 內部拒絕 `..`；讀取雖然擋得住，回應時間與長度判斷仍構成一個檔案存在／大小的 oracle。驗證提到接觸檔案系統之前。
- **UCCI 觀測 JSONL 用行程內鎖，第二個實例寫同一個檔案時可能交錯**：`observe()` 以 per-instance `tokio::Mutex` 串行化，另一個 `UcciCascade`（另一個 agent 的引擎，或熱重載產生的第二份）指到同一路徑時兩把鎖互不相干。改用 `duduclaw_core::with_file_lock`（慣例 ③）並整包移到 blocking 執行緒；這個檔案正是 `scripts/ucci_fit.py` 的訓練輸入，損毀一行就污染離線擬合。
- **ACP 通道把「這個通道無法保留交付租約」講成「來源已變更，請重試」**：ACP 走的是不保留租約的 legacy 建構器，只要回覆用到受保護來源就固定回拒絕字串，即使來源完全沒變，使用者重試永遠得到同一句話。改成專用文案，說明是通道限制並建議改用支援租約的通道；來源真的被撤銷時仍回原本那句「來源已變更」。
- **`system.doctor` 會被卡住的 Docker 拖死**：`check_docker()` 對 `docker info`／`podman info` 用 `.output().await` 沒有逾時，旁邊的 mcp（10 秒）與 grok（15 秒）探測都有；Docker Desktop 的 VM 卡住時 `docker info` 永不回傳（2026-09-28 本機實錘：daemon 行程活著、socket 存在、`docker info` 超過 60 秒無回應），儀表板的 `system.doctor`／`doctor_repair` 與 gateway 測試套件因此整個掛住。現在探測包 10 秒逾時並 `kill_on_drop`，逾時回 `warn`「daemon 可能卡住，請重啟容器執行環境」，不再留下孤兒 `docker info` 行程。
- **五個 webhook 通道的匿名寄件者共用同一個 CCR 取回範圍**：LINE／飛書／Google Chat／釘釘／Teams 在 payload 讀不到寄件者 id 時，會把字面值 `"unknown"` 當成回覆管線的 `user_id`，而那個值正是 CCR principal——於是同一通道上所有「查不到身分」的訊息都雜湊成同一個 `source_acl`，彼此看得到對方存進 CCR 的工具原文。五個 adapter 現在只把送進 `build_guarded_reply_*` 的那一個參數改走 `ccr_runtime::reply_principal_for_sender`（查無真人身分就回空字串 → 該回合 CCR 直接關閉，fail-closed）；`"unknown"` 在紀錄檔、session key、推播目標、稽核欄位等處維持原值不動。同時在 `source_acl_for_principal` 補一道防線：principal 去除空白後精確（ASCII 大小寫不敏感）等於 `unknown`／`anonymous`／`system` 或為空一律不產生範圍——刻意用精確比對而非 substring，`unknown-user-42` 這種真 id 仍照常取得自己的範圍。
- **CCR 預覽把所有「以括號開頭」的文字都當成 JSON 而永不壓縮**：`preview_with_query` 的保護判斷只看 trim 後第一個位元組是不是 `{`／`[`，所以 `[2026-09-28T10:00:00Z] INFO …` 這類 log 尾段、以 Markdown 連結開頭的散文全部被排除在 CCR 之外，長結果原封不動灌進 prompt。改成必須「兩端都是對應括號」（trim 後 `{…}` 或 `[…]`）才視為 JSON 文件——真 JSON 一定滿足，括號開頭的散文一定不滿足；`1e400` 這類 serde 解不開的結構化內容仍保留原始位元組不做失真預覽。
- **儀表板看不出歷史執行是用哪一版引擎算的**：`load_daily_run_with_engine_state`／`load_event_run_with_engine_state` 建好後零呼叫端，決策實驗室的擬合、審查篩選與候選比較把「已保存的舊引擎結果」顯示得和「目前引擎仍能重現的結果」一模一樣（載入刻意不重算，所以畫面上完全無從分辨）。三處輸出 DTO 現在都帶 `engine_matches_current`：擬合與其審查篩選回報所評分的每日執行，擬合若為舊引擎另加一條明確限制說明；候選比較的父／候選 KPI 由紀錄中的引擎摘要推得。被雜湊的儲存 payload 一律未動。
- **決策孿生模組註解把「公開報告只含彙總」講得比實際寬**：`decision_forecast_dashboard.rs` 的模組註解宣稱 public reports 只含彙總，但工程驗證的操作者上傳分支會回傳逐日序列（預測對實際的到達／積壓、逐日區間帶、逐日 SLA 達成數）。註解改為誠實描述：任何路徑都不外洩工單列，但真實支援佇列的逐日量與 SLA 達成數確實會在該分支離開 gateway（送給上傳它的同一位 instance admin）。
- **Codex 的 ReadOnly 能力等級是 fail-open，唯讀 agent 其實寫得了檔**：`ReadOnly` 以前和 `WorkspaceWrite` 共用同一組旗標——`--approve-for-me` 加上 `-c sandbox_mode="read-only"`。但 `--approve-for-me` 的自動審核本身跑在 workspace-write 沙盒裡，所以那句 read-only 只是宣告，寫入從來沒有被擋過（同一個檔案裡兩段註解對這件事的說法還互相矛盾，一段說有擋、一段承認只是 advisory）。`ReadOnly` 改用真正會擋寫入的 `-s read-only`；代價是該旗標與 `--approve-for-me` 互斥，所以這個等級的 MCP 工具呼叫會被 `approval_policy=never` 自動拒絕——agent 仍能讀與推理，但那一輪沒有 duduclaw 工具。每次 spawn 會 `warn!` 一次說明這個縮水。`WorkspaceWrite` 與 `FullAccess` 的 argv 逐位不變；需要保留工具的 agent 請給 WorkspaceWrite。
- **團隊角色成員在 gemini／antigravity／grok 上寫出的檔案會被立刻回收**：`SPAWN_OVERRIDE.work_dir`（team composer 用來把角色成員放進員工工作區，好讓產出比即刻 GC 的臨時 scaffold 活得久）以前只有 codex 這一個 runtime 會讀，另外三個一律用 `context.agent_dir`——所以那三個 runtime 上的角色成員把工作寫進自己的 scaffold，幾秒後連同 scaffold 一起被刪（正是設計 §4.3 E3 那個缺陷，只是當初沒接到另外三家）。四個 runtime 現在共用同一個解析點，並補上「override 必須是真的目錄，否則 warn 後退回 agent 目錄」的驗證（照 `claude_runner.rs` 既有規則；codex 原本讀兩次、驗證失敗也可能翻轉旗標，一併合併成一次）。原生 OS 沙盒改以同一個工作根為範圍。身分不受影響：MCP 註冊與 agent id 仍綁 agent 目錄。已知限制：grok 的 `.grok/config.toml` 與 `.grok/sandbox.toml` 是以 cwd 為根解析的，被覆寫的工作根會拿到自己的一份，因此兩個 grok 角色成員共用同一個工作區時會互相覆寫宣告的 env 區塊（command/args 兩半在成員間相同，MCP 子行程實際認證用的是逐行程的身分，影響面僅限宣告區塊）。
- **判官／評估器在「判官家族剛好等於預設 utility 家族」時會被悄悄換成別家模型作答**：跨家族 failover 的 opt-out 以前看的是「hint 有沒有把 provider 搬離預設值」。全域 `[runtime] utility_provider` 與 `[dispatch] judge_provider` 同設 codex 時——一個很普通、而且去相關判官設定遲早會走到的組態——hint 沒搬動 provider，旗標於是回到 `true`，判官 spawn 失敗就被 failover 換成 worker 自己家族的模型作答。改成：只要是帶 hint 的 utility 呼叫（判官／評估器路徑）一律 `allow_cross_family_failover = false`，與 base provider 是否相同無關；指定家族就是指定家族，不因為它剛好也是預設值而失效。未帶 hint 的 utility 呼叫行為逐位不變。
- **Antigravity 的 stream-json 解析器沒有降級路徑，答案到手也整組失敗**：`parse_stream_output` 在六個地方嚴格失敗——任一行 JSON 解不開、沒有 `result` 事件、`result` 少 `response`、`usage` 少一個整數，全都回 `Err`，而 `execute()` 直接往上拋，於是一個**已經答完**的 `agy` 被當成 spawn 失敗、整個角色成員跟著丟掉。改成形狀不合就降級、事實不降級：解不開的那一行跳過；沒有 result／response 退回「取最後一個非空行當回覆」；usage 缺漏或不完整回報為未知而不是捏造的 0（每種降級各 `warn!` 一次說明缺了什麼）。唯一仍然硬失敗的是**明示**非 `SUCCESS` 的 status——那是 agy 告訴我們這輪失敗，不是我們沒認出形狀。
- **codex `-c` dotted key 的鍵半邊沒有驗證（潛伏面）**：值半邊一直有 `toml_string_literal` 把關，鍵半邊是原樣內插。今天所有鍵都來自固定的 `DUDUCLAW_*` 集合所以沒有已知損壞，但一個含 `=`、引號、點、換行或空白的鍵會把覆寫指到別的設定路徑或產生解析不了的 payload。鍵半邊現在必須是 bare TOML key（`[A-Za-z0-9_-]`，刻意排除 `.`——這裡的片段是單一環境變數名，點會靜默變成巢狀 table），否則 `warn!` 後丟棄而不是硬加引號。另加一條測試鎖住「可以出現在 codex argv 的 env 鍵集合」，讓新的秘密不能無聲加進去。
- **團隊角色成員可以改寫員工的 `[capabilities]` 自我提權**：角色成員（規劃／執行／審核）是另一個 principal——常常是便宜的第三方模型——但它以**員工工作區**為 cwd 執行，於是載入的是員工目錄的 `.claude/settings.json`，那支檔案保護 hook 在安裝時就把身分烤死成員工本人。`check_caller_scope` 比對目錄擁有者與 caller 相等，判成 `NotAgentFile` 放行；`check_protected_toml_write` 接著只 diff `AGENT_ORG_FIELDS`（`name`／`reports_to`／`department`），`[capabilities]` 不在凍結清單。一行 `Edit` 就能翻開 `os_native`／`computer_use`、放寬 `allowed_tools`、清掉 `denied_tools`、塞進 `db_sources`，下一輪 `check_tool_subset` 就照新的 envelope 放行——被打破的正是「成員永遠不可能持有員工沒有的工具」這條平台明文承諾。現在整段 `[capabilities]` 一併凍結：比對走兩邊鍵的聯集而不是手維護的清單，所以之後版本新增的權限鍵當天就受保護；把 table 換成純量或整段刪掉同樣擋下。操作者不受影響——儀表板、MCP `agent_update` 與直接用編輯器改檔都不經這個 hook（hook 只看得到 Claude Code 自己的 Write／Edit／Bash 呼叫）。
- **團隊回合的規劃額度比實際扣款少算一次，第四輪會死在半路**：審核者是 utility 呼叫而不是 scaffold，但它會寫一筆帶 `member_id = "team-verifier"` 的 `role_turns.jsonl`，而 `spawns_used_for_task` 正是數「帶 member_id 的列」。`plan_round` 的 `projected` 卻沒把它算進去，於是每一輪實際扣的額度都比規劃多 1——預設 `max_spawns_per_task = 12` 規劃得出四輪，第四輪跑到一半就 `needs_human(budget_exhausted)`。`projected` 改為 `planner? + executors + verifier + repair?`（審核者無條件計入，因為第三階段一定會跑，降級鏈也只放棄它的修補回合、從不放棄審核本身），`MIN_ROUND_SPAWNS` 同步由 2 改為 3，否則鉗到下限的預算連一輪都排不下。
- **凍結的團隊規格不再重檢家族與 runtime 允許清單**：`FrozenRole.family` 的 doc 寫著「存起來給後面的讀者檢查 verifier ≠ executor」，repo 全域 grep 卻是零個讀者；審核者的 runtime 也只問 `RuntimeType::from_id`，沒有像執行者路徑那樣再比對 `TEAM_ROLE_RUNTIME_ALLOWLIST`。規格是可變 `tasks` 列裡的 JSON——手改的資料庫、規則上線前的備份還原，都會產生一份不再成立的規格而下游毫無察覺。現在每一輪開跑前先用**凍結的**欄位重檢兩條：四個角色的 runtime 都必須仍在允許清單內，審核者的家族必須仍不同於執行者；不符即 `Failed` 並以 `frozen_spec_runtime_not_allowed`／`frozen_spec_family_violation` 留審計，訊息指名是哪個角色。刻意讀存下來的 family 而不是重查目錄（重查回答的是「這份設定今天代表什麼」，這裡要問的是「這個任務正在跑的規格還成不成立」）。
- **回合結束沒有清掉自己的角色派工排隊票券**：`invalidate_role_members` 的 doc 寫著「回合每一個終態都要呼叫」，實際上全 repo 零個生產呼叫端——正常取消靠每個等待者自己的 `QueuedRoleTicket::drop`，所以提早拒絕、階段失敗或 panic 而結束的回合，票券會一直佔著共用的 `queue_max_depth` 直到 TTL 過期。回合現在整段生命週期持有一個清理 guard，accept／reject／needs_human／cancel／`Failed`／panic 六種離開路徑都會清掉**這一輪**的排隊成員，且不動到其他輪的票券。另外把確定沒有生產呼叫端的 `try_admit_role_member`／`dequeue_role_member` 標為 `#[deprecated]` 並在 doc 說明被誰取代（前者被 `ephemeral::admit_role_member` 的 TOCTOU-safe 計數取代，後者的「依位置彈出」對等待中的回合是錯的原語）。
- **`role_turns.jsonl` 輪轉會讓進行中任務的已用額度靜默歸零**：帳本在 16 MiB 會 rotate 成 `.jsonl.old`，讀取端卻只看 live 檔——而團隊派工額度就是數這些列，所以任務進行中一旦輪轉，已用額度歸零、任務等於多拿到無上限的輪數，直接牴觸 `spawns_used_for_task` 自己 doc 的「預算撐得過 gateway 重啟」。讀取端改為先讀最近一代的 `.jsonl.old` 再讀 live（只有一代，因為每次輪轉都會覆蓋前一份）。兩個檔的雜湊鏈在邊界重新開始，所以這是為了讀取與計數的串接，不是「鏈是連續的」的宣稱；鏈驗證仍是逐檔的。
- **團隊規格被別人搶先凍結時，那一輪會靜默退回 Solo**：`freeze_for_task` 的 set-once 保證來自資料庫的 `WHERE team_spec_json IS NULL`，所以兩條建立路徑互搶是正常且安全的；不安全的是**輸的那一方**的視角。goal loop 以前只 match `FreezeOutcome::Frozen(..)`，`AlreadyFrozen` 什麼都不做，於是本地那份 task 還是 `team_spec_json = None`，下一行 `frozen_spec(&task)?` 回 `None`，該輪安靜地跑 Solo——而之後每一輪讀到新鮮的列又都跑 Team。同一個任務兩種執行形狀，哪裡都沒有痕跡。現在 `AlreadyFrozen` 會重讀該列把規格回填並留 log；`Disabled`／`Refused` 維持原樣（`freeze_for_task` 內部已經記錄／稽核過）。
- **一則 CCR 回覆會讓全機器所有 agent 的 wiki／trust 寫入立刻失敗**：wiki 交付柵欄是 `<home>` 底下的**單一檔鎖**（`for_wiki_dir` 把任何 agent 的 wiki 目錄都折回同一個 home），而交付端取得共享鎖之後**一路持有到通道送完**；所有寫入端用的又是**非阻塞**排他鎖、零重試。結果是：agent A 一次 40 秒的回覆期間，agent B 的 `shared_wiki_write`、auto-distill 建檔、`update_frontmatter_trust`、trust 訊號寫入全部立刻回 `Busy`，而 `feedback_bus` 對這種失敗只記一行 log 就把訊號丟掉。三件事一起改：**(1) 柵欄粒度降到每個目錄**——每個 agent 的 wiki 根、shared wiki 根、以及持有 `wiki_trust.db` 的 home 各自一把鎖與各自一個持久 epoch（鎖檔改放 `<dir>/.delivery_fence.lock`）；trust 列的寫入改取「該列所屬頁面的 wiki 根」的鎖（批次寫入依目錄分組、排序去重後逐一取），只有真的無法歸屬到單一根的寫入（`meta_set`／保留期清理／原始連線存取／開檔探測與 schema 遷移）才用 home 那把。**(2) 寫入端改有界等待**——頁面與 trust 列寫入以 5 秒上限（25 毫秒輪詢）取排他鎖，開檔探測／遷移用 750 毫秒，逾時才回 `Busy`；`Busy` 變成獨立錯誤種類（`WikiFenceError::Busy`／`CausalStoreError::Busy`，判定用錨定前綴而非未錨定 `contains`），`causal_wiki::trust_store()` 不再把它偽裝成 `InvalidInput`（`/api/causal/*` 因此不再回「invalid causal evidence input」400，而是 503），`feedback_bus` 對 `Busy` 改成 warn＋重試一次，丟棄時也明確標記，不再靜默。**(3) 交付租約縮到「讀取＋驗證」窗口**——交付端不再整輪持鎖，改為在讀取時記下該 wiki 根與 trust home 的 epoch，送出前連同頁面／trust 摘要一起重驗，變了就拒送。fail-closed 語意不變，只是從「讓所有寫入者等」換成「交付端偵測到變更即拒」；同位元組重寫仍因 epoch 前進而被擋下。
- **Codex 派工把 MCP 憑證的值放進 argv，同機任何人 `ps` 都讀得到**：註冊 duduclaw MCP server 只能走 codex 的設定通道（實測與原始碼雙重確認：codex 對 stdio MCP 子行程 `env_clear()`，只補 11 項白名單與設定宣告的 env，gateway 自己的行程環境到不了），而 `-c mcp_servers.duduclaw.env.<K>="<值>"` 就是把那條通道放進命令列，`DUDUCLAW_MCP_API_KEY` 與 `DUDUCLAW_AGENT_TOKEN` 因此暴露在 `ps -ww`／`/proc/<pid>/cmdline`。現在依 codex 版本分流：`codex-cli 0.157.0` 以上改用 `-c mcp_servers.duduclaw.env_vars=["名稱", …]`，**只有變數名稱進 argv**，值改設在 codex 行程環境由 codex 轉交子行程；判定憑證用精確後綴（`_API_KEY`／`_TOKEN`／`_SECRET`／`_PASSWORD`，不分大小寫），其餘非機密鍵（`DUDUCLAW_HOME`／`DUDUCLAW_PORT`／`DUDUCLAW_AGENT_ID`…）維持原本的 `env.<K>` 形式。版本閘的理由是 `env_vars` 只在 0.157.1 實測可用而最低支援版本未確認，舊版遇到未知設定鍵可能整批 spawn 死亡或靜默漏掉憑證：gateway 每個 binary 路徑每行程只跑一次 `codex --version`，低於 0.157.0、解析不出版本、或探測失敗／逾時（5 秒）一律視為不支援，回退到原本的 argv 形式並留一次 `warn!`——探測失敗永遠不會讓派工失敗。舊 codex 使用者的行為逐位不變；要讓憑證離開 argv，升級 codex CLI 到 0.157.1 以上即可，不需要改任何設定。
- **`team_handoff` 掛在外部可授予的 `memory:write` 上，拿到一把 `memory:write` 外部 MCP key 就能寫進跨任務共用的封包目錄**：這個對應當初的理由是「填封包跟寫 working state 同一個信任層級」，但 `working_state_*` 只會寫呼叫者**自己**的 `<agent_dir>/state/`，而未 pin 的 `team_handoff` 寫的是 `<home>/team_packets/<task>/r<n>/`——跨任務共用的目錄；`memory:write` 又在 `EXTERNALLY_GRANTABLE_SCOPES` 裡，於是任何操作者發給外部客戶端的 `memory:write` key 都摸得到團隊交接通道（既有那句 `assert!(!EXTERNAL_TOOLS_WHITELIST.contains("team_handoff"))` 只覆蓋白名單那條規則，讀起來像「外部不可達」但不是）。新增內部專用 scope `team:handoff`（`Scope::TeamHandoff`，第 25 個 MCP scope），**刻意不放進** `EXTERNALLY_GRANTABLE_SCOPES`：外部 principal 無論宣稱 `memory:write`、`team:handoff` 還是 `admin` 都一律不可達。內部呼叫者逐位不變、也**不需要重發任何 key**——MCP 派分閘接受 `Scope::Admin` 代位任何所需 scope，而 gateway spawn 的每個 MCP 子行程都用 `admin` 的 `gateway-internal` key 認證；反過來把新 scope 字串寫進 `config.toml` 才有害（舊版 binary 讀同一份檔時 `parse_scopes(..).unwrap_or_default()` 會把整把 key 的 scope 清空），這個取捨寫在 `mcp_internal_key::internal_key_entry` 的 doc 裡。
- **MCP 的 `scoped_tools` 階段性授權閘在正式路徑上是 fail-open，且 `eph-*` 角色成員的目錄從來沒被解析過**：授權閘讀 `agents/<principal.client_id>/agent.toml`，但 gateway spawn 的每個 MCP 子行程 client_id 都是 `gateway-internal`，讀不到任何 `scoped_tools` → 閘直接不套用——這正是同一個函式上一層（能力閘）在 2026-09-05 修掉的同一個缺陷，漏在這一處；`has_active_grant` 的查詢鍵同樣用錯（授權是由 `acting_agent_id` 發出的）。兩處改用與能力閘相同的 `gate_agent`，並比照解析 `.ephemeral/`。**行為變更**：設有 `[capabilities] scoped_tools` 的 agent 從現在起真的會被擋，需要先經 `capability_request` 取得人工核准（這是該機制原本宣稱的語意）。另外逐一盤點了 `mcp.rs` 的 37 處生產 `home_dir.join("agents").join(...)`（另 116 處在 `#[cfg(test)]` 內），把以**呼叫者身分**推導目錄的 12 處改走會認 `.ephemeral/` 的解析：`wiki_*` 的 `resolve_wiki_dir`（修前不只把角色成員的 wiki 寫到 scaffold 外，還會**建立**一個在 GC 後仍留著的假 `agents/eph-…/` 註冊目錄，現在無效的 `eph-` id 一律拒絕而不是生出目錄）、`office_script`（修前角色成員的內建 office 腳本一律回報 Script not found）、安裝／工具核准閘（修前對角色成員 fail-OPEN）、`capability_request`、computer-use 能力閘、`odoo_connect` 的 per-agent 覆寫、`skill_gaps`／`skill_extract`／`shared_skill_share`、wiki 可見度與部門解析。註冊管理類（`create_agent`／`agent_update`／`agent_remove`／`agent_update_soul`／`evolution_*`／`reports_to` 驗證）刻意**不**解析 `.ephemeral/`——那個命名空間本來就不屬於註冊表，往那裡解析等於把管理面擴張到 scaffold 上。
- **UCCI `ucci_shadow_strong` 的影子生成不再擋住使用者回覆**：開啟這個選擇性的觀測收集後，每則被接受的 Fast 回覆以前都要先等一次完整的 Strong 生成才送得出去——一個只給離線擬合腳本讀的資料列，卻由回覆路徑付延遲。影子生成改為 `tokio::spawn` 背景執行（資格檢查仍留在呼叫端，沒開就完全不 spawn），因此觀測列**允許落後**於它所描述的那則回覆：`scripts/ucci_fit.py` 以 `request_id` 配對、不看檔案順序，而每次 append 本來就握著跨行程 advisory lock，不會截斷或交錯。新增 `InferenceEngine::flush_shadow_observations()` 讓嵌入端在行程結束前把在途的影子排空；不呼叫則最多遺失最後一列，其餘已落地。`route_and_generate` 的接收型別隨之改為 `&Arc<Self>`（正式呼叫端本來就持有 `Arc`）。升級判準的 log 也補上 `router`（`ucci`／`post_hoc_logistic`）、`tier_from`／`tier_to` 與 `margin`——先前兩種路由器共用同一行 legacy 訊息，看不出是誰做的決定、根據什麼。
- **CCR／Decision Lab 使用者面文案洩漏內部實作詞彙**：`replay hash`／`lineage`／`artifact`／`SHA-256`／`CCR`／`ACL`／`tenant` 這類內部代號直接出現在三語系（zh-TW／en／ja-JP）的欄位標籤與說明文字裡；CCR 儀表板頁面標題甚至直接曝露內部代號「CCR」。改寫約 80 個 `ccrDashboard.*`／`decisionLab.*` 鍵的三語系值成人話——`replay hash`→verification code（重放檢核碼／再生検証コード）、`SHA-256`→content fingerprint（內容指紋／コンテンツ指紋，首次出現於 `decisionLab.sourceVersion` 括註原詞）、`artifact`→source record（來源檔／ソース記録，依語境區分「來源檔案」與泛用「紀錄」兩義）、`tenant`→organization（組織）、`ACL`→access scope（存取範圍）；CCR 頁面標題改為「可回溯壓縮儲存與稽核」，副標括註 `(CCR)`。鍵名不變，`technical-terms.test.ts` 維持綠燈。
- **`support_pilot_review` 核准卡的審核深連結對非管理員是死路**：`ApprovalDetailPanel` 為這類核准渲染「在 Decision Lab 檢視指定模擬」按鈕，但該連結指向的 `/app/system/decision-lab` 受 `RoleGuard minRole="admin"` 保護，而核准收件匣 `/inbox` 對所有已登入角色開放——非管理員審核者點下去會被 `RoleGuard` 靜默導回 `/`，沒有任何說明。面板現在依目前使用者角色決定渲染：管理員維持原按鈕＋提示；非管理員改顯示「需要管理員在 Decision Lab 檢視。」純文字說明，不渲染死路連結。
- **`role_turns.jsonl` 的角色用量只記最後一段，和 `cost_telemetry` 對同一個階段給出兩個數字**：`record_role_usage` 是 last-writer-wins（`*guard = usage`），但一個成員階段可能跑好幾段——openai-compat 的 tool loop、`failover.rs` 的主要嘗試失敗後由備援回答——每一段的 token 都真的付了，帳本卻只留最後一段。改為累加（飽和加法，不會回繞成看起來合理的小數字）：某個維度只有其中一段量到就保留那一段的值，不會被沒回報該維度的那一段歸零；`None + None` 仍然是 `None`（沒量到就是沒量到，`Some(0)` 是另一種說法）。新增 `usage_legs` 欄位記錄有幾段回報了用量，`> 1` 就是「這個階段不是一次呼叫」的訊號；完全沒有任何一段回報用量時整組欄位仍然不寫出來，舊資料列照樣讀得進來。
- **送進驗收判官的證據文字被未錨定的字串取代改寫**：`dispatch_engine::strip_workspace_prefixes` 用 `String::replace` 抹掉裸的 `agents/<id>/`，於是工作者回報裡一句 `restored /mnt/backup/agents/agnes/old.md` 會被改寫成 `/mnt/backup/old.md`——一個實際不存在的檔案，而那段被改過的文字正是判官讀到的證據。現在只在「路徑 token 真的可能開始的位置」剝除前綴：文字開頭、空白之後，或引號／反引號／`(`／`[` 之後（刻意不含 `/`，那正是備份路徑會誤中的原因）。掃描以整個 char 前進，CJK 不會被從字元中間切開。原本要解決的顯示問題（live rounds 9–11 的絕對路徑誤判）行為不變。
- **越界的 `artifacts[].path` 只留稽核、判官看不到**：`enforce_packet_invariants` 的 doc 寫著「verifier sees the same thing the audit does」，但 `render_packet_for_prompt` 根本不渲染 `artifacts[]`——逃出工作區的路徑被拒絕並稽核之後，仍以一般產物的身分進了驗收者的輸入。封包渲染新增 `artifacts:` 段，每一筆都帶狀態標記：`exists`／`missing`／`mismatch`（與 `artifact_receipt` 稽核列同一次觀測）、`outside_workspace`（容器檢查的拒絕）、`unverified`（在工作區內但刻意沒讀——非一般檔案或超過 64 MiB）、`id_only`（只有 id 沒有路徑）、`unchecked`（沒有給工作區，不做任何檔案系統動作）。注入掃描器讀的就是這份渲染，所以路徑本身現在也會被掃到；人類看的結算摘要改用同一段渲染，移除原本重複的單行 `artifacts:` 清單。
- **`eval --team-2x2` 的 eval home 複製沒有形狀上限，成本退回粗估時完全靜默**：複製本來只有 256 MiB 位元組上限與 symlink 拒絕，目錄深度與檔案數都無界（複製是遞迴的，先撐爆的是堆疊不是磁碟），而每個臂每次重複都複製一份。現在加上 32 層深度與 20,000 檔上限，兩者都以操作者看得懂的訊息拒絕。成本方面：`measured_cost` 拿不到實測用量時（最常見原因是成本遙測 singleton 已被行程內他處綁定）第一次會印 `WARNING`，結束時再印「N 筆是粗估不是實測」的總計；該 DB 改為唯讀開啟，探針不再因為「讀」而在操作者的 eval home 生出一個空的 `cost_telemetry.db`。
- **`role_model_matrix.toml` 的格子沒有說明量測條件**：2×2 探針量規劃格時執行者固定為 strong、量執行格時規劃者固定為 strong，兩者落在同一個檔案裡，卻沒有任何欄位說明它們不是同條件量測——讀的人會把兩個不同的實驗直接比較。`MatrixCell` 新增 `conditioned_on` 短標記：`--matrix` 單角色量測（迴圈裡沒有其他角色的模型）寫 `solo`，2×2 探針寫 `executor=strong`／`planner=strong`。空字串一律拒絕（那是「在沒有任何條件下量的」這個主張，不是缺值），缺鍵表示產生者沒有記錄條件（舊檔）。
- **因果證據圖每次讀取都重建整份 schema、重讀磁碟上所有 Wiki 來源**：`CausalStore::open()` 每一次呼叫都跑完整的 DDL 批次（約 30 個 `CREATE`＋4 個 trigger）、重寫兩個 memory trigger（含一次 `BEGIN IMMEDIATE` 寫鎖）、把每一筆匯入的 Wiki 來源整檔讀回來比對，再開一次保留期清掃的寫交易。而讀取面是 N+1：一次 `GET /api/causal/claims?limit=100` 會做 100 次 open，模型覆核視圖每條邊一次（上限 256 條），`duduclaw causal-effect-eval` 每個案例一次（上限 1000 個）——家目錄有 10 份各 1 MiB 的 Wiki 來源時，單一個列表請求就是約 1 GB 的磁碟讀。三層修法：DDL 批次與一次性 backfill 由 `PRAGMA user_version` 短路（`SCHEMA_VERSION` 改動時必須同步 bump，測試會焊住這點）；memory trigger 改由同一交易寫下的標記短路，但 trigger 被刪或 `memories` 表稍後才出現時仍會重裝；live-Wiki 重掃、保留期清掃與延後的 memory 清掃改為**每個 store 實例每 30 秒最多一次**，首次 open 必跑。`read_claim`／`read_model`／`model_state`／`effect_readiness`／`read_effect_estimate` 各自多一個 `*_with_conn` 版本，上述三條多列路徑改為共用一條連線。**節流不會放寬撤銷**：gateway 每個 HTTP 請求各自建一個 store，所以每個請求照樣先重掃再回答；而外送面的 `still_valid()` 本來就每次都自己讀活頁面與 memory revision 比對，不經過 open，因此節流視窗內改寫頁面仍當場讓進行中的投遞失效（已加回歸測試）。
- **行程硬當機會在 `.ccr-leases/` 留下永遠回收不掉的鎖檔**：`acquire_ccr_delivery_lease` 先建鎖檔並取得 OS 排他鎖，之後才插入 lease 資料列；回收端 `ensure_ccr_delivery_drained` 卻是**以資料列為起點**去找鎖檔，所以在這兩步之間被 SIGKILL 留下的鎖檔沒有任何一列指得到它，目錄只會單調成長。causal-store 的維護區段現在會列出該目錄，只刪除「沒有對應 lease 列」**且**這個行程拿得到排他鎖的檔案——還被活著的取得者持有的檔案永遠不假設它死了，回收失敗也不會讓 open 失敗。
- **撤銷 fence 只寫不刪，中途放棄就永久隱藏來源**：`begin_ccr_revocation` 寫進 `causal_ccr_revoking` 的那一列，全 repo 沒有任何 DELETE。撤銷一啟動來源就對讀者隱藏（這是刻意的），但若撤銷卡在活躍租約或後續 UPDATE 失敗而操作者沒有重試，那份仍然完好的來源就再也看不到，UI 上卻只顯示「忙碌中」。新增 `CausalStore::clear_revocation_fence` 與 admin-only 的 `POST /api/causal/source/clear-revocation-fence`（走既有 `authorize_causal_admin`，無論放行或拒絕都寫 `causal_clear_revocation_fence` 稽核列）以及 `duduclaw causal-clear-revocation-fence`。這**不是**「取消撤銷」：artifact 已被 invalidate／erase、CCR tombstone 通知還在 outbox、或該版本仍有投遞租約時一律回 `Conflict`——這三種情況下游都已經依撤銷行動過了。
- **Wiki／記憶來源匯入回傳的 `retention_at` 超出瀏覽器安全整數範圍**：沒有保留期限的來源（Wiki 頁面、無 `valid_until` 的記憶）在資料庫裡以 `i64::MAX` 當哨兵，序列化成 JSON 是 `9223372036854775807`，瀏覽器 `JSON.parse` 會失真成 `9223372036854776000`——第一個真的拿這個欄位做比較的呼叫端會讀到一個錯誤的到期日。`SourceArtifact` 現在把這個哨兵序列化成 `null`（「沒有到期」正是它的語意，而且沒有客戶端會把 `null` 誤讀成時間戳），有限值照原樣輸出；`web/src/lib/causal-api.ts` 的匯入回傳型別同步補上 `retention_at?: number | null`。
- **CCR 交付租約重驗跑在 async reactor 上，一則十段回覆等於三四十次帶寫鎖的 SQLite open**：`CcrDeliveryGuards::still_valid()` 與 `GuardedReply::still_valid()` 都是同步函式，卻被十一個通道 adapter 在每一段送出前直接呼叫；單一個 entry guard 就會開三到四條連線，wiki 來源那一路還要再 canonicalize、讀檔、開 `wiki_trust.db`，`busy_timeout` 是 5 秒——DB 有競爭時整個 tokio worker 停擺。兩個彙總方法改為 `async`（內部 `spawn_blocking`，panic／取消一律 fail-closed 判 false；無 runtime 時原地執行不 panic），並保留同步版 `still_valid_blocking()` 給 `capture_delivery_guards` 與 `office_docs::DeliveryCheck` 這類無法 async 的呼叫端。`tool_loop`、十一個通道 adapter、WebChat 與文件投遞路徑共 40 餘處呼叫點同步更新；新增共用的 `channel_reply::guard_lost()` 讓 adapter 不必各自重寫判斷。
- **CCR 來源在逐段送出途中被撤銷時，已寫進對話歷史的那則回覆仍然留著**：`CcrTurnDelivery::settle()` 只在 `GuardedReply::new()` 當下判一次；之後 adapter 每段送出前的 `still_valid()` 失敗只會停止送出，session 列與蒸餾排程都不會回退。`GuardedReply` 現在持有該回合的撤銷句柄（`CcrTurnDelivery` ＋ `SessionManager`），任何一次重驗觀察到租約消失就自動走同一條撤銷路徑（`revoke_message` ＋ 把蒸餾判定設為「未交付」），adapter 不必改動；交付判定改在回覆被 drop 時才公布，且每回合只公布一次。**沒有用到 CCR 來源的回合維持原行為**（建構當下就公布交付判定），ACP 這類無法保留租約而改回拒絕文案的路徑也一併回退該回合。
- **`CcrStore::open()` 每次呼叫都重建整份 schema、取寫鎖跑 migration、再清一次過期列**：而 `still_valid()` 一次就會 open 三四次。改為 `PRAGMA user_version`／`SCHEMA_VERSION` 短路（與 `decision_store.rs` 同一套契約，DDL 或 migration 改動時必須 bump，測試焊住這點），過期清掃改為**每個 store handle 每 60 秒最多一次**（`Arc<AtomicI64>`，clone 共用；首次 open 必跑）。節流不放寬過期語意：`find`／`retrieve` 本來就在 SQL 裡比 `expires_at`，跳過清掃只會讓死列多留一會兒在磁碟上（已加回歸測試）。
- **`duduclaw_ccr_find` 的 SQL 對 scope 內每一列重算 `lower(original)` 最多 17 次，而且 scope 欄位沒有索引**：`find` 是模型可見工具，一輪 loop 可呼叫 `max_iters` 次，單筆 `original` 上限 2 MiB。新增 `idx_ccr_scope (tenant_id, agent_id, session_id, source_acl)`；排序用的 8 個 GLOB 運算式改由一個 `MATERIALIZED` CTE 算一次（CTE 只投影小整數，不物化小寫全文——那會把 CPU 的一半 DoS 換成記憶體的一半）；並加上**每個 tool loop 每 scope 最多 8 次 `find`** 的上限，第 9 次回明確的 `is_error` 工具結果（`ToolLoopTelemetry.ccr_find_rate_limited` 計數，僅在記憶體與日誌，不新增資料表欄位）。
- **tool loop 日誌記下明文 CCR handle，與稽核表「只存 digest」的宣告不一致**：`ccr_retrieval_audit` 只存 `sha256(requested_id)`，spec 也這麼寫，但 `tracing::info!/warn!` 直接記 `id = %audit_id` 明文，連同 tenant／agent／session，等於一個 spec 沒描述的第二稽核面。三處日誌改記 `id_sha8`（同一個 SHA-256 的前 8 碼）。
- **`channel_reply` 三個零呼叫端的 legacy wrapper**：`build_reply_with_session`／`build_reply_with_session_with_artifact`／`build_reply_for_agent_with_artifact` 全 workspace 已無實際呼叫端，卻是 `pub` 所以不會有 dead-code 警告，下一個 adapter 很容易誤用成「安靜丟掉租約」的那條路。三者移除；ACP 仍在用的 `build_reply_for_agent` 保留，`build_reply`／`build_reply_with_progress` 改為直接走 `build_guarded_reply_with_session`。
- **團隊角色成員（`eph-*`）在三支工具上讀不到自己的設定**：`db_sources`／`db_tables`／`db_select`／`db_query` 的授權、`plan_start` 的 `[planner]` 設定、以及 `skill_from_recording` 蒸餾時用的 AI 員工目錄，都還在用 `agents/<id>` 這個裸路徑。團隊角色成員是 `eph-` 開頭的臨時 AI 員工，實際落在 `agents/.ephemeral/<id>/`，所以三處都讀到不存在的檔案：資料庫授權一律判成「沒有任何授權」（即使真的授權過）、`plan_start` 永遠回預設的「先問清楚」腳手架（即使該角色關掉了）、蒸餾則丟失該角色的模型／runtime 設定。三處改走與 `mcp.rs` 相同的 `caller_agent_dir`（內部是既有的 `ephemeral::resolve_agent_dir`，會先證明路徑確實包含在 `.ephemeral/` 內）；一般註冊 AI 員工的解析結果與修正前逐位相同，且不會再為 `eph-` id 憑空建出註冊目錄。
- **關機時遺失進行中的 UCCI 影子觀測**：本地推論的 `ucci_shadow_strong` 影子生成改成背景執行（不再拖慢回覆）之後，多了一個 `flush_shadow_observations()` 排空 hook，但從來沒有呼叫端——gateway 一關機，還在生成的影子連同它的校準紀錄就直接消失。現在接進 graceful shutdown 序列，位置固定在「預測引擎 flush 之後、worker supervisor 關閉之前」，上限 5 秒，逾時只記 `warn` 不擋重啟；從未初始化本地推論引擎的安裝（絕大多數）完全零成本。
- **CCR 租約保留在 reactor 上同步重驗，卡住整個執行緒**：`capture_delivery_guards` 是每個 tool loop 結果都會走一次的同步函式，內部呼叫 `still_valid_blocking()`——一條租約鏈要開好幾個 SQLite 連線，wiki 來源還要讀檔，而四個 runtime 轉接層（local 推論／Direct API／openai-compat／operator fallback）全在 async 情境呼叫它，等於每產生一次答案就把 reactor 釘住一次磁碟往返。改成 `async fn`，內部沿用 `CcrDeliveryGuards::still_valid()`（`spawn_blocking`，panic／取消一律 fail-closed 判為失效）；空租約集合仍在原地直接回答，不碰 blocking pool。四個呼叫端加上 `await`（`claude_runner` 的 `Result::map` 閉包改寫成 `match`，同步閉包無法 await）；因為已無同步呼叫端，不保留 `_blocking` 版本。規格文件「每次重驗都在 blocking thread、不在 reactor」的宣告至此才真正成立。
- **`duduclaw_ccr_find` 仍對每一列重算 8 次 `lower(original)`**：上一輪把 8 個排序用 GLOB 收進一個 `MATERIALIZED` CTE 只算一次分數，但刻意不物化小寫全文，所以每列仍要做 8 次全字串小寫掃描——而詞數是攻擊者可塑的，位元組上限不是。現在改成兩層 `MATERIALIZED` CTE：內層套完所有 scope／撤銷過濾後，每列只投影一次 `' ' || lower(original) || ' '`，外層 8 個 GLOB 一律比對該欄。暫存記憶體受既有 `MAX_STORE_BYTES`（64 MiB）自然約束，取捨已寫進 `find` 的 doc comment。`exact_hit` 刻意維持 `instr(original, ?)` 比對原始欄位（精確片語本來就區分大小寫），結果集與排序逐位不變。
- **`ccr_find_rate_limited` 只進記憶體與日誌，操作者看不到**：每輪 loop 最多 8 次 `duduclaw_ccr_find` 的上限被撞到，是「模型在原地打轉」最直接的訊號，卻只存在於 `ToolLoopTelemetry` 與一行 log。現在落成 `ccr_loop_telemetry` 的欄位（CCR store `SCHEMA_VERSION` 1→2）；舊檔案帶著舊版號重新進入 migration 窗口拿到 `ALTER TABLE ... ADD COLUMN ... DEFAULT 0`，既有列全部保留並讀到預設值，再蓋上新版號。儀表板彙總補上該計數；`ccr_dashboard` 是唯讀投影不做 migration，遇到尚未升級的舊檔案回報誠實的 0，而不是讓整份快照因為缺一欄就失敗。
- **團隊 verifier prompt 對同一批產物再算一次 sha256**：`enforce_packet_invariants` 已經把每個宣告產物 stat＋讀取＋雜湊過一次（上限 64 MiB／檔）並落成 `artifact_receipt` 稽核列，W3-3a 讓 `render_packet_for_prompt(_, Some(workspace))` 在組 prompt 時又呼叫一次 `verify_artifacts` 來標狀態——同一份位元組被讀兩次、雜湊兩次，而且交到 verifier 手上的是**第二次**觀測，沒有任何東西保證它和稽核記下的那一次一致（兩次之間檔案被換掉，收據說 `exists`、prompt 說 `mismatch`，兩句話都會出現在同一輪裡）。改法：驗證改成單次走訪（`observe_artifacts`），一次同時產出收據與每筆宣告的 `ArtifactVerdict`（顯示路徑＋狀態 token），沿 `run_member` → `run_team_round` → `run_verifier` → `build_verifier_prompt` 以封包檔為鍵傳下去，渲染只查表、**完全不碰檔案系統**；查不到對應觀測的產物（成員派工失敗但已交出封包、同一輪先前嘗試留下的 slot）一律標 `unchecked`，不借用別的封包的觀測、也不在 prompt 路徑補讀。順帶修掉同一段的既有缺陷：收據以**顯示路徑**（活測第九輪起改成工作區相對）為鍵，填補 `sha256` 的查表卻用封包**宣告的原文**比對，因此以絕對路徑宣告的產物即使存在也永遠填不到雜湊——改走同時帶兩種拼法的 verdict。另外把 `verify_artifacts`／`read_packets`／leg 快照這三處阻塞檔案 I/O 移進 `spawn_blocking`（呼叫端都是 async，先前直接在 reactor 上跑），失敗降級為「這批封包以 `unchecked` 進 verifier」並留 `error!`，不靜默。
- **因果證據審查頁未走 i18n**：`CausalCurationPage`／`CausalDagView`／`CausalEffectPanel`／`CausalExtractionEvalPanel` 先前把使用者可見文字整段寫死在元件裡（包括 `Tenant ID`／`ACL` 這類內部詞字面比對），繁體中文以外的語系開這頁只會看到中文或殘留英文標籤。四檔全面改走 `react-intl`（`causalCuration.*` 命名空間，213 個鍵，三語系鍵集一致），並依內部詞替換原則統一詞彙：tenant → 組織 ID／Organization ID、ACL → 存取範圍／Access scope、artifact → 來源檔／source record、SHA-256 → 內容指紋、lineage → 來源系譜（`來源 lineage` 這類先前混用英文原詞的字面一併統一）。
- **技能自動合成鏈斷在三個地方，觸發後就是死路**：`CLAUDE.md` 宣傳的「缺口累積 → 從情節記憶合成技能 → 沙盒試用 → 跨員工畢業」有三處與設定無關的斷裂。① `GapAccumulator::confirm_synthesis()`／`cancel_pending()` 全 repo 零生產呼叫端，所以一個主題觸發一次之後就永遠卡在 `pending`、再也不會第二次觸發；② `SandboxStore::add()` 同樣零呼叫端，`active_names()` 恆空，`channel_reply.rs` 那整段試用評估迴圈是 no-op；③ `skill_lifecycle/gap.rs` 寫 `signal_type`、消費端 `external_factors.rs` 讀 `type`，每一列技能缺口訊號都被讀成 `unknown`。現在新增 `skill_lifecycle::synthesis_runner` 作為觸發的消費端：產生 → 解析 → 安全掃描（沿用 `skill_security_scan` 那支掃描器，fail-closed）→ 進沙盒試用並 `confirm_synthesis`；任何一步失敗都走 `cancel_pending`，讓主題重新累積而不是燒掉冷卻時間。**成本面**：整條路徑由 `agent.toml [evolution] skill_synthesis_enabled`（**預設 false**）把關，預設安裝一毛錢都不會多花，關閉時只會清掉 `pending`；開啟後還要先湊滿 `skill_synthesis_threshold` 次同主題缺口並通過每主題冷卻。鍵名統一為 `type`（`submit_feedback` 一直用的那個），兩邊讀取端都同時接受舊的 `signal_type` 拼法，磁碟上的舊資料不會失效。
- **`--features otel` 在 macOS／Windows 上根本編不起來**：五個 OpenTelemetry optional 相依（`opentelemetry`／`opentelemetry_sdk`／`opentelemetry-otlp`／`tracing-opentelemetry`／`tonic`）被放在 `[target.'cfg(target_os = "linux")'.dependencies]` 底下，所以在非 Linux 平台上開這個 feature 時那些 crate 完全沒有被連進來，`otel.rs` 的 15 個 import 一律 `unresolved module or unlinked crate`。因為全 repo 從來沒有任何 build 開過這個 feature（盤點報告 T2-H7：`--features otel` 在 `Cargo.toml`／`otel.rs`／文件以外零命中），這個破損一直沒人發現。五行移回一般的 `[dependencies]`——它們本來就是 `optional = true`，那才是「預設不進 build graph」的機制，再疊一層 target 閘只會少掉平台。預設 build 逐位不變（feature 仍預設關）。同時交出一條 CI 步驟（`cargo check -p duduclaw-gateway --features otel`）避免再次腐爛。
- **`[memory] novelty_gate` 在十三條 gateway 內部路徑上靜默失效**：`memory_factory::build_memory_engine` 是唯一會掛上 embedder（因而讓反假驚訝寫入閘真的生效）的建構點，但大量生產路徑仍直接呼叫 `SqliteMemoryEngine::new`，於是那個預設開啟的設定在這些路徑上是文件化的 no-op。現已全部改走單一建構點：`channel_reply`（關鍵事實寫入、使用者輪廓區塊、事實召回、決策擷取／自動結案／TTL 清理）、`profile_distill`、`goal_loop` 的任務規則注入、`chat_commands` 的 `/rules`、`autopilot_engine` 的人格偏好行、`night_engine` 的 N1–N4、技能合成的佐證查詢，以及 `server.rs` 的排程 decay job。三類**刻意保留**原本的建構方式並寫進模組文件：操作者／儀表板 RPC（人工策展不經篩選，`CLAUDE.md` 原本就這樣寫）、只跑 schema migration 再用 SQL 複製列的 `memory_migrate::merge_one`（讓去重閘介入等於靜默丟列），以及測試。附帶效果：這些路徑的檢索排序現在也吃得到 `w_vec` 語意訊號（權重 0.15），與其他早就走建構點的路徑一致。
- **Odoo 事件同步整條線從未接上，但儀表板的輪詢開關預設是開的**：`duduclaw-odoo` 的 `PollTracker::poll_model`／`classify_event`／`parse_webhook` 自橋接上線以來零生產呼叫端——沒有 `/webhook/odoo` 路由、沒有輪詢工作，而 Odoo 設定頁照實把 `poll_enabled`／`poll_models`／`webhook_enabled` 寫進 `config.toml`，操作者設定完事件同步卻什麼都不會發生。新增 `odoo_events.rs` 補上缺的那一半：背景輪詢工作（每輪重讀 config，所以儀表板改設定不必重啟）與 `POST /webhook/odoo`，兩條管道都收斂成同一個 `AutopilotEvent::OdooEvent`，記錄的頂層純量欄位攤平到規則條件可直接讀。同時把 `OdooConfig::default().poll_enabled` 與 `OdooPage.tsx` 的預設值一起改成 `false`——先前是 `true`，只因為沒有任何消費者才無害。三道 fail-closed：webhook 關著回 404、密鑰不符回 401、**密鑰設成空字串也一律拒絕**；輪詢要同時滿足 `poll_enabled`／連線已設定／至少一個合法模型名稱才會起工作。
- **身分解析設定了 Notion，AI 員工看到的還是本地 wiki 快取**：`channel_reply::build_sender_block`（每輪注入的 `<sender>` 區塊）與 `identity_resolve` MCP 工具兩個生產點都硬編 `WikiCacheIdentityProvider`，各自留了一句「之後的步驟再接」的註解；只有儀表板的 `identity.resolve` RPC 真的去讀 `config.toml [identity] provider`。結果是：操作者設定 Notion 後，在儀表板看得到 Notion 的答案，每個 AI 員工其實只讀得到本地快取。三個呼叫點現在共用 `identity_provider::build_identity_provider`，選擇規則 fail-safe（Notion 選了但缺 `database_id` 或金鑰一律降級回快取，不是報錯）。`ChainedProvider` 補三個回歸測試：快取命中時**完全不呼叫**上游、四種上游錯誤一律降級成 `Ok(None)`（不是只有 `Unreachable`）、上游命中**不回寫快取**（把 module doc 明講的設計釘死成測試）。RFC-21 承諾但未實作的 `identity_list_project_members`／`identity_invalidate_cache` 已在 `docs/features/25` 與 RFC-21 標為未落地。
- **Google 整合分頁看得到、按下去 403**：後端 `[integrations] google_workspace` 預設 `false`，前端 `GOOGLE_INTEGRATION_ENABLED` 硬編 `true`，所以分頁永遠可見、設定步驟全部做完、憑證測試還會變綠，但每個 AI 員工的 Google 工具呼叫都被拒絕，唯一的出路是手動改 `config.toml`。`GoogleIntegrationPage` 改讀後端旗標：關著時顯示啟用引導與一顆「啟用」按鈕（新增 `google.integration.set` RPC，admin only，可開可關），開著時顯示狀態與停用入口；讀不到狀態時兩個橫幅都不顯示，不會把「讀取失敗」講成「尚未啟用」。
- **`cost_anomaly::detect()` 的模組文件宣稱已接 notify 路徑，實際零生產呼叫端**：燒錢速率異常偵測寫好了、有測試，但只有自己的測試在呼叫它。現在從 `budget::check_agent_budget`（每次 LLM 呼叫都會過的派工關卡）接上，而且**刻意放在 `is_inert` 短路之前**——固定上限與相對異常回答的是不同問題，沒設上限的員工正是沒有別的東西會發現它燒錯錢的那一個。自我節流：每個員工每小時最多一次 SQL 查詢（行程內），每個員工每 UTC 日最多一次告警（檔案化去重，跨重啟有效）。命中時寫 log＋Activity Feed＋L1（FYI）通知，不阻擋任何呼叫。一併修掉「今天」的判定：`daily_cost_millicents` 略過沒有花費的日子，用最後一筆當今天會把舊的忙碌日誤判成今天——新增帶日期標籤的 `daily_cost_series`，今天只認日期字串。
- **GitHub 整合缺少 Google 那種預設關閘門**：五個 GitHub 工具沒有 `integration_enabled` 這一層，保險庫裡有 token 就等於「這台機器上每個 AI 員工都可以用操作者的身分公開留言」（`github_issue_comment` 是對外可見的寫入）。補上 `[integrations] github`（預設 `false`，缺檔／壞 TOML／非布林值一律讀成關），工具在 `tools/list` 一併隱藏（discoverable ⇔ callable），呼叫時回指向設定頁的拒絕訊息；從儀表板完成 GitHub OAuth 連線會自動打開，跟 Google 同一條「連線＝opt-in」規則，既有使用者不必學新開關。
- **Expert pack 被靜默丟棄，操作者拿不到任何訊號**：`expert_generate::standalone_catalog_entries` 對三種情況都是無聲 `continue`——`expert.toml` 讀不到、解析失敗、`[expert] name` 與目錄 slug 不符——所以一個包沒出現在 `experts.catalog` 裡時，沒有任何地方說得出為什麼。三種情況都改成 `warn!` 並帶上包名與原因（列表仍是 best-effort，一個壞包不會讓整份目錄消失）。另補兩個測試：壞鄰居不影響正常包上架；`commercial/templates-premium/experts/` 下所有非 `*-team` 包的 manifest name 必須等於目錄 slug（`commercial/` 未 checkout 時跳過）。附帶查證：盤點報告記載 `pharmacy-pro` 的 manifest name 是 `pharmacy-assistant`——實際 `[expert] name = "pharmacy-pro"` 與目錄相符，`pharmacy-assistant` 是包內第一個 `[[expert.agents]]` 的名字；目錄對位這一半已經是對的，真正還活著的缺陷是上面那個靜默丟棄。
- **`data-file-guard` 在 Windows 上靜默失效**：RFC-23 §14.4 的資料檔守門是一支 shell script，在沒有 `bash` 在 `PATH` 上的 Windows 主機，hook 指令本身執行失敗，而 Claude Code 把非 2 的結束碼當放行——守門就在最沒人會發現的地方消失（這件事寫在它自己的原始碼註解裡）。改寫成 Rust 子指令 `duduclaw hook data-file-guard`，與姊妹 `agent-file-guard` 同形，判斷邏輯收進 `duduclaw_core::data_file_guard` 供 CLI 與 gateway 共用；shell script 刪除，installer 改註冊子指令，並在下次 spawn 時刪掉殘留的 `.claude/hooks/data-file-guard.sh`。模式解析改嚴（只認 `on`／`read_only`，其餘一律 off），JSON 信封改用真正的解析器（舊版無 python3 時退回 `sed`，指令裡的跳脫引號會截斷比對）。仍是啟發式而非沙箱：`Bash` 那道檢查比對檔名，動態組路徑的指令依然繞得過去，這點沒有改變也沒有隱瞞。

### Added
- **工單 SLA 影子預測接進 Decision Lab（管理員專用，與彙總影子預測並列）**：彙總積壓那條線去年就接進儀表板了，工單層級的 SLA 那條一直只有 CLI——卡在兩個地方：工單匯出（期初開單身分＋當日工單流水）動輒上 MB，打不進 decision 端點的 16 KiB 上限；SLA 人工檢視只有「要求」與「已核准才放行」，查不到 pending／denied／expired。這一包把兩個洞都補上。十一個新端點（`/api/decision/shadow-sla-forecast/create|load`、`shadow-sla-score/create|load|load-current`、`shadow-sla-policy/assess`、`shadow-sla-screen/evaluate|save|load`、`shadow-sla-screen/review/request|status`）全部走既有的 `authorize_causal_admin`；兩個 create 用**專屬**的 2 MiB＋64 KiB 上限（2 MiB 來源＋JSON 信封餘裕），其餘沿用 16 KiB，不動彙總那條線的任何一個位元組。檢查順序照抄 CLI：載入凍結預測→時間窗→保留期限→大小→`preview_*` 預檢→同 ID 同內容回讀既有紀錄（否則 409）→註冊來源→寫入；寫入失敗時**只**清掉「這次建立且沒有被已提交紀錄引用」的那一份來源，兩個併發請求各自帶自己的來源版本，失敗的那個不會連坐成功的那個。**回應一律不含工單資料**：只有數量、SHA-256、ID、時間戳與限制說明，沒有 ticket ID、沒有 opening_tickets、沒有 tickets 逐筆、沒有 cohort 明細；來源解析器的診斷訊息（欄位名可能來自操作者上傳的檔案）一律降級成通用的「無效請求」，不回吐任何欄位名。前端新增「工單 SLA 影子預測」面板，來源三擇一（成品 ID／上傳檔案／貼上 JSON），超過 2 MiB、非 UTF-8、非 JSON 在瀏覽器就擋下來，不浪費一次往返。誠實邊界：這是本機提交、來源身分未經上游認證的合成／工程證據，**不是 SLA 達成的證明、不是校準過的未來區間、也不是模型晉升或人力調度的授權**；政策登錄與交接、SLA 評分修正仍只在 CLI。
- **Team-as-Agent 候選案例來源稽核**：L3 證據新增可重跑的案例盤點與雜湊，區分既有單員工題、已具團隊驗收條件的案例及外部基準原始題；外部題未接模擬器與判分器前不計入正式矩陣。另以既有四臂活測粗估價做 USD 600 預算啟動前檢查，不把估價當帳單。
- **量表第一次 smoke 抓到三件事，三件都補上了（Team-as-Agent P2 追加）**：①**`--agent <id>`**——執行格 12/12 全錯，錯在探針用的 home 只備了一個員工、沒有套件宣告的那個 `hr-recruit`。量表量的是**模型**不是人格，所以借一個已備妥的員工去承載別人的套件是可接受的探針妥協，但它會換掉每個案例跑起來的 system prompt，因此一律**明說不推定**：報告標頭寫 `agent_override`、每筆 run 寫實際用的 `agent`；`--agent` 本身過 `is_valid_agent_id`（`../escape` 直接拒絕，不讓它走到 `home/agents/<id>`），沒給覆寫時「找不到員工」的錯誤逐字不變，覆寫本身不存在時訊息**同時點出兩個 id**（分得出是「我的覆寫不存在」還是「案例宣告的員工不存在」——舊訊息分不出來）。②**審核格要結構化判決**——codex 審核格 4/4 `unparseable`，因為 codex 不會可靠地用裸的 `PASS`/`FAIL` 開頭，prompt 寫得多白都一樣。現在每次審核呼叫都經**既有的** `--output-schema` 管路（wave-3 的 `OUTPUT_SCHEMA` task-local ＋ `UtilityModelHint.output_schema` ＋ `runtime/codex.rs::output_schema_args`，一行新管路都沒加）要求 `{"verdict":"PASS"|"FAIL","reasons":[...]}`；解析器改成**兩種形狀都收**：JSON 物件（可包在 ``` 圍欄裡，任何 runtime 都適用）或原本的首 token 形式，其餘一律 fail-closed 到 `unparseable`。**JSON 先試是刻意的**：`{"verdict":"PASS","reasons":["… would FAIL if …"]}` 這種回覆若先走散文規則，會被「第一行出現 FAIL 就算 FAIL」的保守拆解反轉成 FAIL——測試裡直接把這個反例焊死（`parse_first_token_verdict` 單獨看確實會判錯）。schema 仍只有 codex 真的約束得住，其餘 runtime 照舊 log 後忽略，所以它是偏好不是前提。③**退化黃金標籤（degenerate gold）**——錄製逐字稿相對現行斷言已過時，黃金標籤**每一題都是 FAIL**，於是一個無條件回 FAIL 的審核者一致率拿到 1.00（haiku 4/4）。現在每個審核格回報 `gold_pass`/`gold_fail`，任一類缺席就設 `degenerate_gold: true`、把 `verdict` 強制成 `unresolved`（`verdict_reason: "degenerate_gold"`）、console 印警告，並把該角色從瓶頸比較中**排除**（理由點名是哪些模型的格退化，不是靜默丟掉一臂）。統計數字照樣全報、另存 `verdict_statistical`/`label_statistical`——**不藏數字，藏的是結論**。零筆 run 不算退化（那是 `n = 0` 的「沒量到」，硬說退化等於對沒發生的事下判斷）。連帶在文件記下兩筆 P2 欠帳：**premium 套件必須先重錄逐字稿（或修斷言）審核格才有意義**（P0 活測實錘 replay 只過 98/360，換舊版二進位同樣 98/360，非本次回歸），以及 `ModelRegistry` 的內建表**不認識** `claude-sonnet-4-6` 與 `gpt-5.6-sol`，所以對這些模型的 `--budget-usd` 目前只是 run 計數器不是花費上限，要有意義得先把實際在用的模型與價格加進 `~/.duduclaw/models.toml`。
- **量得出「哪個角色該換哪家模型」：角色→模型能力量表（Team-as-Agent P2，`duduclaw eval --matrix`）**：一個 AI 員工的規劃／執行／審核可以各跑一家廠商的模型——那就冒出一個跑一遍測試套件回答不了的問題：**哪個角色的選模真的有差，以及這個模型到底做不做得來那個角色**。這一包把它變成量測，不是排行榜。一格（cell）＝一組 `(domain, role, runtime, model)`，domain 就是一個 eval 套件目錄：**執行格**把案例的 prompt 真的跑在那組 `(runtime, model)` 上，只看零 LLM 的 `[expect]` 斷言（這一輪執行格完全不用 LLM 判官）；**審核格**問的是另一件事——這個模型分不分得出好壞——所以它不重跑工人，而是把案例**既有的錄製逐字稿**＋驗收標準給模型看，要它回 `PASS`/`FAIL`，再拿同一份逐字稿上的斷言結果當黃金標籤來對分。審核格回報三個數字而不是一個：**一致率**、**誤放率**（黃金說 FAIL 它說 PASS——這是貴的那種錯，壞東西被放過去了）、**誤擋率**（只多花一輪修補），各自附 Wilson 區間（沿用 `prediction::calibration::wilson_bounds`，不另開第二套統計方言）；第一行讀不出判決的回覆一律記成 **unparseable** 並且**不併進任何一個比率**——「這個模型交不出可解析的判決」跟「這個模型判得爛」是兩個不同的、要分開處理的發現。**規劃格明確延後到 P2b**：規劃呼叫產出的是子任務封包，不是斷言分得出對錯的答案，要評分得先有完整的團隊回合載具；`--roles planner` 直接拒絕，`role_model_matrix.toml` 標頭寫 `planner = "deferred"`，所以規劃格的缺席讀起來是「沒量」而不是「量出來很爛」。給了 `--weak`／`--strong` 之後，每個角色算一次 Δ = 強模型分數 − 弱模型分數（**同一批案例逐題成對**、叢集穩健 SE；只有兩臂完全不共用 case id 時才退成非成對均值差，而且會明說退了）；Δ 大的那個角色就是花錢最有效的地方——但**只有它的信賴區間完全排除其他角色的區間時才敢叫它瓶頸**，否則答案是 `unresolved`，而那是一個真答案，正是點估計排名會判錯的那一格。這是 AgentCARD（arXiv:2606.20629）Shapley 探針的**解耦版**：每個角色各自量、沒有聯合團隊回合，這才付得起（2 角色×2 模型，而不是 |模型|^|角色| 種團隊組合），代價也在同一句話裡——真正的角色交互作用（強審核只有配弱執行才划算）在這個設計下**結構上看不見**，所以它只回答「先花在哪個角色」，永遠不是團隊層級的歸因。硬規則都寫進程式不是只寫在文件：`--matrix` 拒絕 `--replay`（拿凍結逐字稿比不同模型就是 Replay Gap，arXiv:2608.08239）、`--matrix` 絕不 `--record`（會用別的模型的跑法覆蓋掉該領域的基準逐字稿）、宣告的 `--temperature` 低於生產值一律拒跑（Miller 2024 §3.3：壓低溫度＝壓掉重跑變異＝製造部署後不存在的解析度）、`q < 1` 的格一律 `unresolved` 不得當名次讀、被 failover 換成**另一組** `(runtime, model)` 回答的那次 run 從該格剔除並記成 `substituted`（絕不把別人的答案記在被問的模型頭上）、以及所有 run **嚴格串行**一次一個 spawn（這些 run 吃的是操作者自己的帳號配額，平行跑等於自己限自己的流、還讓樣本互相關聯）。`--report` 同時寫出 JSON 報告與旁邊的 **`role_model_matrix.toml`**：一個 `[header]`（宣告 MDE／α／power／K／叢集鍵／`paired_seeds`）＋每格一個 `[[cell]]`（`n`／均值／區間／該格實際達到的 MDE／三態 `verdict`），算不出來的統計量是**缺鍵**而不是編一個數字，零可用觀測的格有報告列但沒有矩陣格，而且寫入與讀取**兩邊都驗證**——手改出重複格或不存在的 runtime id 會被拒絕而不是被相信。**這份檔案目前沒有任何程式讀它**：接進 composer 的選角是 P5，所以可拆性閘的 `capability_gap` 訊號依舊沒有資料源。順帶一提誠實的解析度：smoke 規模（6 題、K=1）實際達到的 MDE 是數十個百分點，幾乎每格都會回 `unresolved`——夠分 Haiku 檔與 Opus 檔，遠不夠排兩個相鄰模型；宣告 MDE 會印在 console 摘要並存進檔案標頭，就是為了不讓人事後引用一個樣本數撐不起的名次。
- **`duduclaw eval` 可以指定跑在哪一家 runtime／哪個模型上（`--runtime` / `--model` / `--paired-seeds`）**：live 模式原本只會 spawn `claude`（`eval/runner.rs` 寫死），量表要比較不同廠商就卡在這裡。現在 `--runtime claude`（預設）與省略時**逐位維持原本的直接 CLI 路徑**，其餘 runtime 走 gateway 的多 runtime 抽象（`runtime_dispatch::run_agent_prompt`），所以 codex 那套已經活測過的 argv 紀律（`--skip-git-repo-check`／`-c approval_policy=never`／能力推導出的 `--sandbox`／MCP `-c` 覆寫／stdin null）**留在 `runtime/codex.rs` 裡，不在 eval 複製一份**；工具事件透過既有的 `NATIVE_TOOL_COLLECTOR` task-local 收集（跟 `team_composer` 同一個機制），再**合成**一份 stream-json 形狀的逐字稿，讓 `must_use_tools`／`max_tool_calls`／`[[expect.grounded]]` 全部沿用同一個既有解析器，而不是多寫第二套逐字稿建構器與第二套斷言語意；合成檔第一行會自報 `duduclaw_eval_synthetic`，不會被誤認成真的 CLI 錄製。保真度限制明文寫在程式與文件裡：合成逐字稿只帶得動 runtime 事件流真的帶了的東西——一個文字塊（所以 `min_text_blocks` 永遠只觀測到 `1`）、沒有 thinking 塊、工具輸入是 collector 遮罩截斷後的字串而非原始 JSON，因此**兩條路徑的這些訊號不可互比**，一格量表絕不混用。`--model` 覆寫每個案例的 `[case] model`，報告標頭的 `model`／新增的 `runtime` 一律寫**實際跑的那個**；未知的 `--runtime` 直接拒絕，絕不靜默退回 claude（那會把 claude 的量測掛在別家名下）。`--paired-seeds` 依 `(case_id, repeat)` 推導決定性種子（FNV-1a，刻意不用版本相依的 `DefaultHasher`，也刻意與模型無關，這才叫「成對」）——但這個 build 裡**沒有任何 runtime 收得下 seed**（CLI 都沒這個旗標、`duduclaw-llm` 的 `ChatRequest` 也沒有這個欄位，2026-09-25 實查），所以它是**只記錄不套用**，每一筆 run 都寫 `seed_applied: false`，不假裝取樣被釘住了。
- **一個目標回合可以由「規劃→執行→審核」三段組成，每段換一家廠商模型（Team-as-Agent P1/WP-4，預設關閉）**：WP-1 落地了設定與驗證、WP-2 落地了角色成員的載體，這一包把兩者接上目標迴圈——**建立任務時把這一輪的團隊規格凍結一次**（`tasks.team_spec_json`，與驗收標準同樣的「凍結一次、永不再寫」語意，寫入走 `WHERE team_spec_json IS NULL` 的 set-once 保證），之後角色→模型量表或 bandit 改了只影響**下一個**任務。規格驗證不過（最常見的是審核者與執行者同一個模型家族）就**完全不成團**：什麼都不存、審計 `team_refused`、任務照原本的單一員工路徑跑，不存在「半個團隊」。派工前跑一次零 LLM 的可拆性閘（`team_gate`），**量不到的訊號一律不觸發**（偏向 Solo 是刻意的——為一個單一員工本來就做得完的任務組隊，等於白付一次計畫、一次交接、一次合併）；四個訊號要中三個才成團，通道即時對話、計畫待核准、剩餘輪數少於 3 都是硬排除。剛好中兩個時走灰帶：先跑一次規劃（這通呼叫本來就要付），再用**規劃實際拆出幾個子任務**（讀封包數與封包宣告的依賴，不從敘述臆測）重跑一次閘，拆出 ≥4 個獨立子任務才成團，否則這一輪退回單一員工派工、計畫留在磁碟上給後面的輪次讀。一輪三段的紀律：規劃只拆解不動手，交不出任何子任務封包就 `needs_human(需要決策)`（不從它的散文猜一份拆解）；執行每個子任務一名成員、**同輪成員之間零通訊**；審核跑在自己的 `(runtime, model)` 上，只看**凍結的驗收標準＋執行封包＋工具活動稽核摘要**，看不到規劃的敘述、也看不到執行者的散文——獨立證據源才是槓桿（VP-CONTROL arXiv:2609.10969 實測證據面貢獻 40.9pp、模型多樣性只有 11.3pp）。審核不過給同一個執行家族**一次**修補（只補指名的缺口，不重寫、不擴張範圍），然後把最後一份執行產物交給**既有**的驗收路徑（兩段式 evaluator＋三面向 MAV 判官＋gap 指紋＋震盪偵測＋最佳輪）——團隊換的是誰做事，不是誰判定做完了。預算降級鏈是固定順序（`[dispatch.team_budget] degrade_order`，預設 `utility → verifier_second_pass → executor_replica`）：先不用合成、再砍審核的修補回合、再把扇出收成一名執行者，全砍完仍排不下才 `needs_human(預算用盡)` 並交最佳輪成品；砍了哪幾步會留痕，所以一個便宜的回合讀起來是「便宜」而不是「比較爛的團隊」。每一段都往 `role_turns.jsonl` 追加一行（與 `tool_calls.jsonl` 同一把 advisory lock、同樣 0600、同樣 hash chain 與 16MB 輪替）：`task_id / round / role / member_id / runtime / provider / request_model / runtime_used / response_model / failover / effort / packet_path / observation_fidelity / outcome / error_type / config_fingerprint_hard`——這是平台原本沒有的維度（既有的成本、錯誤、規則、eval 全部以 agent 為最細粒度，一個員工的任務由三家模型做完時「哪個 agent」就不再能指認誰做了什麼）。設計文件列出但目前**沒有任何程式算得出來**的欄位（trace/span id、fault_side、role Shapley）**刻意不寫**，寧可欄位不存在也不要一張「看起來有填」的 schema；runtime 沒回報用量時 `usage_*` 整組省略，不寫 `0`（那會變成「量過，而且免費」這種假話）。角色成員 spawn 走自己的斷路器桶與預算（`role_team`，預設 60/分鐘），並在第一次真的考慮組隊時（不是開機時）檢查一次 `[dispatch] ephemeral_max_active` 是否撐得住 `max_concurrent × iteration_cap × 角色數`（預設 45 對 32）並說出該調到多少。派工政策新增第四種 `role_team`，但**選出來的仍然是那個員工**（`tasks.list`、看板指派、通道綁定、成本歸屬語意全不變）——回傳三個 id 會把內部角色洩漏到每一個使用者面。`[team]` 整段不存在時（預設）：不凍結、不審計、不掃 roster，派工路徑與這一包之前**逐位相同**。`[dispatch.team_budget]`（`max_spawns_per_task` / `max_turns_per_role` / `degrade_order`）三把鑰匙連同預設值已寫進 `config/duduclaw.example.toml`，就放在既有的 `[dispatch]` 區塊旁邊。**WP-1～WP-5 至此全數落地，一個回合可以真的走完規劃→執行→審核**，但**尚未對真實目標活體驗證過**（全部只有單元測試），所以預設仍是關的：要開請在自己盯著的部署上開。文件：[`docs/features/56-team-as-agent.md`](docs/features/56-team-as-agent.md)、[`docs/guides/goal-loop.md`](docs/guides/goal-loop.md)。
- **團隊回合的證據終於寫得下來：成員原生工具事件落稽核、宣告的產出逐檔核對雜湊（Team-as-Agent 活測第八輪，隨團隊一起預設關閉）**：第八輪第一次把整條管線跑在真後端上——規劃（Claude）→ 執行（**真的 codex gpt-5.6-sol**，`runtime_used=codex`、fidelity `full`）在員工工作區建出 `notes/a.md b.md index.md` → 封包 → 審核（Claude）→ settle，然後 settle 仍然駁回：兩段式 evaluator 說「沒有任何工具活動可以佐證那些檔案被建立過」。駁回在當下是對的。成員用**原生**工具（codex 的 `shell`、Claude 的 `Write`）做事，這些事件雖然被算進去了（`fidelity: full` 就是從那裡來的），卻只活在派工 scope 的一個 task-local 陣列裡，隨成員拆除一起消失；而下游每一個讀者（審核者的 `<tool_activity>` digest、settle 的零 LLM grounding 前置檢查、MAV 判官的稽核摘要、`recent_actions`）看的都是 `tool_calls.jsonl`。換句話說，最需要被相信的那個 runtime，結構上最不可能被相信。兩個修法，都只是「把證據寫進既有讀者本來就在看的地方」，不新增任何儲存：① **原生工具事件持久化**——每一筆成員原生工具事件寫成一列 `tool_calls.jsonl`，掛在**成員自己的 id** 下（正是證據匯聚已經在要的那個 id），欄位沿用既有形狀（`tool_name`／`success`／遮罩後的 `input`／`result_text`）另加 `source="native"`、`evidence_source="native_tool_event"`、`runtime`／`model`，失敗的事件帶 `error_class="native_tool_error"`（失敗必須留痕，與 MCP 分派門同一條規矩）。兩處刻意與 MCP 列不同、且兩處都是**收緊**：`input` 一律捕捉（即使工具名看起來唯讀，它只會被用來從 grounding 證據裡**扣掉**自我回音），`result_text` 對 self-echo 工具一律不寫（`team_handoff` 回的本來就是呼叫者自己的話，而 codex 會把 MCP 呼叫也報成原生事件，寫下去等於讓角色拿自己的封包摘要替自己背書；`check_grounded` 的 deny-list 是在**寫入端**生效的，所以只能在這裡擋）。單一成員上限 200 列，超出的部分寫一列誠實的 `native_tool_events_truncated` 計數，不靜默丟棄。② **產出收據（artifact receipts）**——封包宣告的每個 `artifacts[].path` 在讀進來的當下就對員工工作區做一次確定性核對（存在／大小／sha256），組成 `<artifact_receipts>` 區塊，同時餵給審核者的 prompt **和** settle 給判官／evaluator 的證據，並且每張收據寫一列 `artifact_receipt` 稽核（`evidence_source="artifact_bytes"`），所以 grounding 也看得到：`notes/a.md 128B sha256=… exists`／`notes/b.md missing`／`notes/c.md 44B sha256=… mismatch (declared …)`。沒宣告雜湊的封包會被回填觀察到的值；宣告的雜湊**對不上**時保留原宣告、記 `team_packet_artifact_mismatch`，調包必須看得見，絕不悄悄改正。只有 `exists` 算確認，`missing`／`mismatch` 記成失敗觀察，永遠無法替任何宣稱背書。這條線的依據很硬：VP-CONTROL（arXiv:2609.10969）實測跨模型判官**共用證據**放行 62.9% 的不安全提案，**獨立證據源**只有 22.9%——證據面 40.9pp、模型多樣性 11.3pp，磁碟上那串 sha256 就是那個獨立來源。沒有宣告產出的任務不會多出任何區塊，沒有原生工具成員的單一員工回合與這一包之前逐位相同。文件：[`docs/features/56-team-as-agent.md`](docs/features/56-team-as-agent.md)、[`docs/guides/goal-loop.md`](docs/guides/goal-loop.md)。
- **判官可以要求「回覆必須長這個形狀」，codex 判官因此終於能被解析（`codex exec --output-schema`，無設定鍵）**：`[dispatch] judge_provider = "codex"` 時兩個裁決階段都用散文回覆，兩個 parser 都 fail-closed 拒收（「evaluator reply has no string `decision` field」／「面板回覆無法解析」）。parser 沒有做錯——把垃圾自動判成通過是判官解析器唯一絕對不能做的事——但一個結構上無法被解析的判官，就是一條不會動的接縫。現在每個階段會發布**它自己的 parser 要求的** JSON schema（evaluator 的 `decision` 三值枚舉＋兩個非空欄位；MAV 面板依當輪難度的 aspect 集各一個 `{pass, reason}`），schema 直接從 parser 的契約推導，所以 parser 改了 schema 跟著改、不會各走各的。目前只有 codex 接上（寫進暫存檔後以 `--output-schema <FILE>` 傳入），其他 runtime 記一筆 debug 後忽略——**schema 是偏好，不是前提**：沒有任何裁決會因為某個後端不能約束輸出而失敗，連暫存檔寫不出來都是「這次不帶旗標」而非「這次不裁決」。`UtilityModelHint` 的 `output_schema` 刻意不列入 `is_empty()`（與 `effort` 同理由：它改的是回覆形狀，不是誰來回答），所以一個只帶 schema 的 hint 對路由完全惰性，降級路徑逐位不變。
- **推理力度（effort）成為逐次呼叫的一等參數（`agent.toml [model] effort`，P1/WP-3）**：現代推理模型除了「用哪個模型」還有一個獨立的「這一次要想多深」旋鈕，但五家 CLI 拼法全不同、可接受的值也不同。現在它是一個設定：`agent.toml [model] effort = "low|medium|high|xhigh|max"`，**不設就完全不送旗標**，每個 spawn 與加上這個功能之前逐位相同（provider 自己的預設深度）。串接路徑一路到底：`RuntimeContext` / `AgentPrompt` / `UtilityModelHint` 三個型別各自帶一個 `Option<Effort>`，多 runtime 選路總門（`run_agent_prompt`）與 Claude 兩條主呼叫路徑（`call_with_rotation` 派工／cron／heartbeat／goal-loop，`call_claude_cli_rotated` 十一通道回覆）都是「明確指定的 effort 優先，否則回退讀該 AI 員工自己的 `agent.toml`」，所以呼叫端不必改也能吃到設定，而未來的團隊角色規格 `{runtime, model, effort}` 只要傳值就能覆寫。**逐 runtime 旗標對照（2026-09-24 對已安裝二進位實測，非文件推論）**：claude 2.1.258 `--effort <v>`（`low medium high xhigh max`）、codex 0.156.1 `-c model_reasoning_effort=<v>`（`low medium high xhigh`，無專屬旗標，走既有的 `-c` config override 通道）、antigravity 1.2.10 `--effort <v>`（`low medium high`）、grok 1.0.41 `--reasoning-effort <v>`（別名 `--effort`）、gemini CLI **沒有這個旗標**（debug 留痕後忽略，絕不臆造一個會讓 spawn 失敗的旗標）。**值一律往下夾（clamp）而非原樣轉送**：`max` 在 codex 上跑 `xhigh`、在 antigravity／grok／openai_compat 上跑 `high`。grok 的上限刻意壓在 `high`——`--help` 只寫了旗標名稱、**沒有列出可接受的值**，送 `xhigh`／`max` 有可能被拒而拖垮整個 spawn；openai_compat 同理（八個異質 preset）。兩個上限都住在同一處（`duduclaw-core/src/effort.rs`），實測確認後可以單點調高。**Direct API 面**（`duduclaw-llm`）`ChatRequest` 新增 `reasoning_effort: Option<String>`，逐 provider 映射到各家原生欄位：Anthropic `output_config.effort`（GA，無需 beta header）、OpenAI Responses `reasoning.effort`（明確指定的 effort 蓋過既有較粗的 `ReasoningHint`）、openai-compat 頂層 `reasoning_effort`、Gemini `generationConfig.thinkingConfig.thinkingLevel`——**最後這個鍵名未經驗證**：`thinkingConfig` 這個容器是確定的（既有的 `thinkingBudget` 就住在裡面），但 `thinkingLevel` 這個同層鍵兩次 WebFetch ai.google.dev 都只拿到被截斷的 `GenerationConfig` 參考、始終沒提到它，Interactions API 那邊拼作 `generation_config.thinking_level`，所以 camelCase 的 `generateContent` 孿生版是推論不是引用；已 gate 在「欄位有值」之後，不設就永遠不會送出這個鍵，程式碼旁標了 `[unverified]`，依賴前請對真 API 重驗。**PTY session pool**（`[runtime] pty_pool_enabled`，預設關）把 effort 納入 **session cache key**：常駐 session 開機時就把 `--effort X` 烤進去並服務後續多次呼叫，兩個不同 effort 共用一個 session 會讓第二次悄悄跑在第一次的深度上，所以它們現在拿到各自的 session（`AgentKey::effort`，`cache_key`／`log_key`／`redact_cache_key_str` 三處同步加寬到 6 段）。**成本與快取**：調高 effort 會提高 token 花費；且 effort 參與快取前綴，**對話中途改 effort 會讓 prompt cache 失效**——選定一個值就放著，不要逐回合微調。唯一刻意**不**跟著 AI 員工設定走的是輕量抽取路徑（session 壓縮／GVU／wiki 建檔），固定 `medium`：機械式抽取的成本不該因為某個對話型 AI 員工被調到 `max` 而跟著變貴。見 `docs/features/13-multi-runtime.md`「Effort」一節。
- **「團隊即員工」的地基：一個 AI 員工內部可以分成規劃／執行／審核／合成四個角色，每個角色綁自己的廠商模型（`[team]` 設定、驗證、可拆性閘；P1/WP-1，全部預設關閉）**：同一個員工對外仍然只有一個聲音、一份記憶、一份人格，但接到一個目標任務時可以在內部分工——規劃用一家模型、執行用另一家、審核再換第三家。本次只落地**純資料層**：設定結構、驗證規則、交接型別與決策閘，四樣都是 `duduclaw-core` 的無 I/O 純函式；真正會派工的 composer、`team_handoff` MCP 工具、goal loop 三段派工、per-role 成本與歸屬都還沒接上，因此**現在打開 `[team] enabled` 不會有任何行為變化**（文件已誠實列出未完成清單）。**① 設定結構**：`config.toml [team]`（全域預設）與 `agent.toml [team]`（逐員工覆寫）同一形狀——`enabled`（預設 `false`）、`executor_fanout`（1..=3，超範圍**夾住並回報**而不是拒絕，一個扇出打錯字不該讓整個團隊消失）、`gate`（`auto`／`always_solo`／`always_team`，後者僅供測試，認不得的值退回 `auto` 並回報），加上 `[team.roles.{planner,executor,verifier,utility}]` 各自的 `runtime`／`model`／`effort`。串接是**逐欄位**的：員工只覆寫某個角色的 `effort`，該角色的 `runtime`/`model` 仍沿用全域；完全沒宣告的角色才落到該員工自己的 `[model] preferred`。「沒設」與「明確設 false」在型別上就是兩種狀態，所以某個員工寫 `enabled = false` 是真的退出，不會被全域的 `true` 蓋掉。整個區段走既有 `crate::lenient` 容錯慣例（型別寫錯降級成預設、絕不讓一個錯字把員工從 registry 裡弄消失），並同時掛在 `AgentConfig` 上，避免 `agent_update` 改寫 `agent.toml` 時整段被吃掉。**② 三條會「拒絕成團」的驗證規則**（不是警告；壞掉的設定一律退回單 agent 跑並標示原因，絕不組出半套團隊）：**(a) 角色是 `(role, runtime, model)` 三元組**——模型家族對不上宣告的 runtime 就硬拒（`model_runtime_mismatch`），catalog 認不出家族的模型也拒，平台永不替一個它放不進任何家族的模型「猜」provider（Goose #10731 的坑：角色設定只存模型名，結果 `qwen-*` 被送進 Claude 後端）；只寫 `model` 可以，catalog 會把它綁到服務該家族的 runtime，只寫 `runtime` 也可以，模型往下串接。**(b) 首批只開五家 runtime**（`claude`／`codex`／`gemini`／`antigravity`／`grok`），其餘含 `openai_compat`／`qwen`／`copilot`／`cursor` 一律拒（`runtime_not_allowed`）——理由是工具面不是能力：這五家都會原生註冊 DuDuClaw 的 MCP server，一個悄悄失去工具的角色會產出自信的「我去查一下」式空話，而審核者分辨不出那跟真的做完有什麼差別。**(c) 審核者的模型家族必須與執行者不同**，同家族直接拒絕成團（`verifier_same_family`）——去相關性就是整個機制本身（arXiv:2607.13918：相關的驗收者只讓失敗率多項式衰減；VP-CONTROL arXiv:2609.10969 實測共用證據的跨模型投票仍放行 62.9% 不安全提案，獨立證據源只 22.9%）。家族是從 runtime catalog 推出來的，所以 `antigravity` 與 `gemini` 會塌到同一個家族（兩者都服務 `gemini-*`，配成執行＋審核在設定上看起來像兩家廠商，實際買到零獨立性）。`effort` 的列舉本身不住在這一層——解析、逐 runtime 上限與各家 CLI 旗標對應由同批的 `duduclaw-core::effort` 單獨擁有（同一個 crate 不該長出兩份同義列舉），本層只把 `[team.roles.*] effort` 當原始字串收下再交給它解析（`low|medium|high|xhigh|max`，大小寫不敏感），認不得的值拒絕成團（`invalid_effort`）；Gemini CLI 根本沒有 effort 旗標，這**刻意不當成設定錯誤**（拒收會讓同一份 `[team]` 無法跨 runtime 沿用卻換不到任何安全性），要在 spawn 層變成 no-op。宣告在「沒綁 runtime 也沒綁 model」的角色上等於無效（該角色會串接到員工自己的模型，帶的是員工的 effort），這種情況**回報而非默默丟掉**。**③ `TaskPacket`（`crates/duduclaw-core/src/task_packet.rs`）——角色之間唯一的交接型別**：目標、輸出格式、工具範圍、邊界、逐條列舉的 `constraints`、`audience` 白名單、可零 LLM 回放的 `acceptance` 斷言（形狀與既有 `EntryAssertions` 一對一，轉換無損）、artifacts／wiki／memory／state 一律只傳參照、structured `findings`/`open_questions`/`blockers`/`next_steps`、觀測保真度 `fidelity`（`full`/`mcp_only`/`none` 三態絕不混同）、`budget` 與 `irreversible`。**刻意不存在** transcript、`tool_use`/`function_call`/`functionCall`、`thinking`/`reasoning`/`encrypted_content`，以及任何 `serde_json::Value` 逃生口——這一條是**強制的不是寫在註解裡的**：每個結構都 `deny_unknown_fields`，帶了這些鍵的 packet 會反序列化失敗，而不是被默默接受再忽略。上限一律**整筆拒絕、絕不截斷**（整包 16384 bytes、`constraints` ≤12 條×≤200 字、`audience` ≤16 筆、每種 `acceptance` ≤6 條×≤80 字），字數一律算 Unicode 字元而非位元組，所以 200 個中文字的約束就是 200 不是 600；截斷約束正是 arXiv:2608.29028 量到的那個失效（含糊的約束讓違規率從 <15% 跳到 50–73%，audience 白名單則「幾乎消除」外洩）。**④ 可拆性閘（`crates/duduclaw-core/src/team_gate.rs`）——預設 Solo**：零 LLM、零 I/O 的純函式。硬排除一律 Solo（通道即時對話、`plan_first` 未核准、計畫裡有不可逆動作、預算少於 3 輪）；四個訊號命中 ≥3 才成團（① ≥4 個可獨立工作項且**量到**依賴圖零 hub ② 內容量超過單一 context ③ 執行者與規劃者候選的能力差距達到量表宣告的 MDE——低於 MDE 是雜訊不是差距，Miller arXiv:2411.00640 ④ 驗收標準 ≥3 條且產出真的產物）；剛好命中 2 個落**灰帶**，由規劃者先跑一次（本來就要付的呼叫）再依真實拆解重判，不擲硬幣；其餘 Solo，並在工作明顯小的時候附一個 effort 建議（Anthropic 自己的成本指引記載調 effort 勝過改架構）。沒量到的訊號就是不命中，也就是偏向 Solo——這個方向是刻意的，貴的錯誤是替一個單 agent 本來就能做完的任務組團隊。每個判定都帶穩定的理由 token，方便日後拿任務真實結果算 Brier 校準；在校準達到統計支持前，儀表板必須標「實驗中」。設計全文 `commercial/docs/DESIGN-team-as-agent-2026-09.md`；欄位參考 `docs/spec/task-packet.md`；使用者面說明 `docs/features/56-team-as-agent.md`。
- **驗收判官可以指定跑在「另一家」模型上（`[dispatch] judge_provider` / `judge_model`）**：驗收判官與第一階段評估器原本一律跑在預設的 utility 模型上，也就是跟工作者同一個模型家族——同一家族的判官會原諒同一家族犯的錯，第二意見的價值遠低於表面（arXiv:2607.13918）。現在 `config.toml [dispatch]` 新增兩個選填鍵 `judge_provider`（runtime id）與 `judge_model`（該 runtime 內的模型 id），把判官與評估器一起搬到另一個 `(runtime, model)`；兩者各自獨立，不設就跟以前逐位相同（`[runtime] utility_provider` / `utility_model`）。**範圍是全域，沒有 per-agent 版本**——per-agent 覆寫要住在 `agent.toml [dispatch]`，但 `duduclaw_core::agent_toml::AgentTomlSections` 沒有 `[dispatch]` 區段讀取器，第二/三波才剛把 62 處手刻影子 reader 收斂掉，這裡不再長第 63 處；哪天該區段落地，`judge_mode::judge_model_hint_from_home` 就是唯一要疊加的地方。**三種無法照辦的情況一律降級回預設 utility 模型並留稽核（`judge_seam_degraded`），絕不讓一個路由偏好卡住裁決**：① `judge_model` 是別家族的模型但沒設 `judge_provider` ⇒ **fail-closed 拒絕，不猜 provider**（Goose #10731 的教訓：被錯後端回答的判官比不跑的判官更糟）；② `judge_provider` 不是這個建置認得的 runtime id ⇒ 整份覆寫丟棄（warn 並列出合法值，絕不靜默替換）；③ `judge_provider` 的 CLI／後端沒裝在這台機器上 ⇒ spawn 前就偵測到並丟棄。判斷全部發生在 spawn 之前，所以降級零 token 成本，真正的 LLM 錯誤仍照舊往上拋，不會被偷偷換一個模型重跑。兩個新稽核事件讓設定的實際效果可被看見而非靠推測：`model_routed`（覆寫真的把判官移離工作者模型時，帶 `{slot: "judge", from, to, provider, reason: "dispatch.judge_model"}`）與 `judge_same_family`（工作者與判官其實同家族、交叉驗證沒發生；**僅告警不駁回**，且每個 (AI 員工, 判官模型) 每次 gateway 執行只記一次，長 goal loop 只留一行）。與 `[dispatch] judge` 同一套熱生效機制：每次裁決重讀 `config.toml`，不必重啟 gateway。**第四種降級（活體測試抓到的洞）**：判官 runtime 可用、被選用、然後**執行失敗**（argv 錯誤、認證失敗、crash）——原本 `failover.rs` 會接手把模型換成預設家族的 `claude-opus-4-6`，判官於是悄悄由工作者的同一家族完成，稽核紀錄一行都沒有。現在 `RuntimeContext` 新增 `allow_cross_family_failover`（預設 `true`，既有呼叫端行為逐位不變），provider 被 hint 移動過的呼叫一律帶 `false`：`execute_with_failover` 在解析出 fallback 模型後以**模型家族**比對（不是 runtime 比對，所以 Claude 換另一個 Claude 級別仍然放行），跨家族就拒絕該次 fallback 並回傳 primary 的原始錯誤（不把 fallback 記為失敗——它沒失敗，是被婉拒）；無法證明同家族（任一邊模型 id 不在目錄裡）亦視為跨家族而拒絕。判官端接著誠實降級：以預設 utility 模型重跑一次，並寫 `judge_seam_degraded { reason: "hinted_runtime_failed", provider, model, error, fallback_model }`，若重跑落點真的就是工作者的家族，再補一筆 `judge_same_family`。裁決永遠不會因為路由偏好而失敗。內部新增 `runtime_dispatch::UtilityModelHint` / `run_utility_prompt_with_hint`，既有 `run_utility_prompt` 成為傳 `None` 的薄包裝，23 個既有呼叫端行為逐位不變。見 `docs/guides/goal-loop.md`。
- **失敗歸因：判官、環境、鷹架的錯不再被學成「模型的錯」（`[evolution] fault_attribution`，預設開）**：自我演化迴圈（AEE playbook 信用結算、`rule_lifecycle` 的 helpful/harmful 記分、shadow 候選評分、MistakeNotebook → F2b 彙整）過去只知道「這輪失敗了」，因此**只有一種歸因方向可表達：模型做錯了**。The Misattribution Gap（arXiv:2605.22842）實測一套歸因系統在 64/64 個真因不在模型的失敗上全部歸咎模型——照這種標籤學下去，不只是浪費輪次，而是**主動製造**去「修正」本來沒壞的行為的規則。新增 `crates/duduclaw-gateway/src/fault_attribution.rs`：一個**零 LLM、寫入時決定、封閉列舉**的歸因層（Model or Harness?，arXiv:2607.28802 的硬要求——真因無法確立就記 `Unknown`，絕不默默併進「模型的錯」），五個歸因方向 `FaultSide::{Model, Harness, Environment, Grader, Unknown}`，依序四條規則第一個命中者勝：**R0** 觀測保真度為 `None`（什麼都沒看到）⇒ `Unknown`；**R1** 零成本 grounding 前置檢查與判官互相矛盾（有真工具結果佐證卻被駁回，或判官放行但硬性 grounding 斷言失敗）⇒ `Grader`；**R2** 記錄到基礎設施失敗（`RateLimited`／`Billing`／`Timeout`／`BinaryMissing`／`SpawnError`／`NoAccounts`）⇒ `Environment`；**R3** 回覆聲稱用了工具卻零筆 native tool event，或該輪窗口內有工具呼叫被能力閘擋掉（讀 `tool_calls.jsonl` 的 `error_class`）⇒ `Harness`；其餘 ⇒ `Model`。**歸因非 Model 的失敗輪一律視為「無證據」**：`dispatch_engine` 的 settle 端兩個閘門（注入規則信用結算、shadow 候選評分）在該輪 `ErrorCategory` 為 `Significant`／`Critical` 時直接跳過，**刻意不改標成 helpful**（把它映射成良性 category 會反過來把功勞算給一條根本沒影響這輪的規則）；`take_injected_task_rules` 仍照常呼叫（remove-once 記帳必須消耗），只有信用結算被抑制。`mistakes` 資料表加兩個附加欄位 `fault_side TEXT NOT NULL DEFAULT 'model'` 與 `counts_for_learning INTEGER NOT NULL DEFAULT 1`（沿用既有「重複欄位錯誤即忽略」的冪等 migration 慣例），`MistakeEntry::with_fault_side()` 一處同時設定兩者以免呼叫端漂移，`reflexion.rs` 的 F2b 彙整在分組前濾掉 `counts_for_learning = false` 的紀錄——**但 F2a 的提示注入與 GVU Generator 仍看得到它們**（「有東西出錯了」不論是誰的錯都是有用的上下文，只有「從它學出一條長期規則」需要真的是模型的錯）。每次非 Model 歸因寫一筆稽核事件 `fault_attributed {task_id, round, fault_side, reason}`，走既有 `security_audit.jsonl` sink（判官 seam 的降級事件也寫在這裡），不另立新 sink——這個決定會**抑制學習**，操作者必須看得到哪些輪次被排除、為什麼。`reason` 是規則代號（如 `r1_judge_rejected_grounded_answer`）而非敘事，因為敘事正是論文實測不可靠的那一半。預設開；`config.toml [evolution] fault_attribution = false` 時歸因強制為 `Model`，所有閘門與 v1.65 逐位相同。**已知限制（誠實揭露）**：① 本次只接 goal-loop 的 settle 路徑，該路徑上 R2 結構性休眠（基礎設施失敗根本不會產生送進驗收的 worker 結果，`failure_reason` 恆為 `None`；欄位已備好給會分類失敗的通道回覆 settle 路徑）；② `dispatch_engine` 的 settle 區沒有任何 mistake 寫入點（goal 任務的失敗不寫 MistakeNotebook），所以 `fault_side` 欄位在正式環境目前只由 schema／API 支援，尚無生產寫入端——真正的寫入端在 `channel_reply.rs`／`dispatcher.rs`，屬另一工作包；③ `reply_claims_tool_use` 是**啟發式**（zh＋en 共 13 條片語常數表，經 `word_contains_ci` 做 ASCII 詞界比對、CJK 退化為子字串），只作為 R3 合取的一半，漏判就退回 `Model`（＝今天的行為），絕不單獨決定任何事；④ `FailureReason::AuthFailed` 與三個 `AccountsCoolingDown*` 刻意**不**在 R2 集合內（雖然它們也算環境，但擴大 R2 是行為決策，留給後續拍板）。設計研究基礎：arXiv:2605.22842、arXiv:2607.28802。見 `docs/features/38-aee-playbook-evolution.md`。
- **`duduclaw eval` 報告加上統計誠實層，為「角色→模型能力矩陣」鋪路（`--repeats`／`--baseline`／`--mde`／`--cluster-by`，P0/WP-D）**：eval 報告過去只有原始通過率，樣本數小或案例彼此相關時很容易把運氣當訊號——要拿這份資料去比較不同模型的能力，統計上站不住腳就會餵出一張錯的矩陣。新增純函式模組 `crates/duduclaw-cli/src/eval/stats.rs`（零 I/O，每個公式都附手算驗證的單元測試）：`mean`／`se_clt`／`se_clustered`（Miller 2024《Adding Error Bars to Evals》arXiv:2411.00640 §2.2 與 App. C 的 cluster-robust 公式，以案例所在目錄為預設叢集鍵）／`se_ratio`；K 重複變異縮減 `Var(mean|K)=Var(mean|K=1)·(1+2/K)/3`（LLM 重複取樣受同一 prompt 相關,不會像獨立抽樣一樣趨近 0,而是收斂在單次變異的 1/3）；樣本數規劃 Eq. 9 `n_required_for_mde` 與其反函數 Eq. 10 `mde_for_n`；resolution ratio `q = n / n_required`（arXiv:2605.30315,`q < 1` 一律回報 `unresolved`,絕不硬湊出勝負）；`(verdict, label)` 三態分類,`label` 命名比照 `duduclaw-gateway::prediction::calibration::HonestLabel` 的 `Supported`／`Candidate`／`IndistinguishableFromLuck` 語意,但獨立複製列舉在本 crate 內,不吃 gateway 依賴。CLI 新增四個旗標：`--repeats N`（同一案例跑 K 次聚合通過率,transcript 檔名自動加 `.r<N>` 後綴避免互相覆蓋,`N<=1` 與既有單次行為逐位相同）、`--baseline <report.json>`（對先前一份 `--report` 做配對比較——逐案例算差值、`corr_with_baseline`、clustered 配對 SE、95% CI；相關係數 <0 時配對設計反而放大變異,自動退回非配對 SE 並標記 `fallback_to_unpaired`）、`--mde <0.10>`（宣告的最小可偵測效果,預設 10 個百分點,拒絕 0 或 ≥1 的荒謬值）、`--cluster-by dir`（目前只實作 `dir`,給別的值直接拒絕整個執行,不悄悄退回未叢集化的數字）。Replay Gap 防呆（arXiv:2608.08239——重播的逐字稿是某次過去執行的凍結結果,拿去跟不同模型的即時執行比較會捏造出一個假的能力差距）：report JSON 新增 `model` 表頭欄位（與既有 `mode` 並列）,兩份報告任一是 `--replay` 且表頭模型不同時,`--baseline` 比較整段拒絕、只在 `stats.baseline_comparison.error` 留一句解釋,頂層 `verdict`/`label` 退回本次執行自己的通過率-vs-機率檢定,絕不用被拒絕的比較結果冒充。Report JSON 新增 `stats` 區塊（表頭 `declared_mde`／`alpha`／`power`／`repeats`／`cluster_by`／`replay_forbidden_for_model_comparison`,整體與逐目錄兩層列都含 mean／SE／95% CI／`resolution_ratio_q`／`verdict`／`label`）與頂層 `verdict`／`label`／`resolution_ratio_q` 鏡像；每次執行結尾印一行人類可讀摘要 `n=… clusters=… pass=…% ±…pp (clustered) | MDE@n=…pp | q=… → <verdict>`,`se_ratio > 2` 時額外印一行 WARNING,提醒同目錄案例高度相關、未叢集化的數字過度自信。既有 report 欄位（`suite`／`mode`／`total`／`passed`／`per_case`／`cases`…）逐位不變,新增全部是附加欄位。詳見 `docs/guides/evals.md`「Honest statistics」一節。
- **每個模型花多少錢終於看得到（`cost.by_model` 與 MCP `cost_summary`／`cost_agents` 的 `by_model`）**：`token_usage` 從第一版 schema 就有 `model` 欄，但所有彙總都只按 AI 員工／使用者／日期分組——「錢花在哪個模型上」在儀表板與 MCP 都問不出來。新增 `CostTelemetry::summary_by_model(agent_id, since_unix)`：按模型分組、成本高的排前面，回傳 `requests`／`input_tokens`／`output_tokens`／`cache_read_tokens`／`cache_creation_tokens`／`cost_millicents`＋`cost_usd`／`cache_efficiency`；沒記到模型 id 的列歸在 `"(unknown)"`，不猜。成本一律是把既有 `cost_millicents` 欄位加總——跟 `summary_global`／`summary_by_agent`／`all_agents_summary` 走同一條計價路徑（`cost_for`，在寫入當下算一次），這裡不重算第二份；`cost_usd` 只是同一個數字的單位換算（該欄歷史單位其實是「分」，見 `estimated_cost_millicents` 的單位註記）。曝露面是加法式的：MCP `cost_summary`／`cost_agents` 回應多一個 `by_model` 陣列（`cost_agents` 的員工列改掛在 `agents` 鍵下——JSON 最外層是陣列時無法再長出具名兄弟鍵），儀表板新增 RPC `cost.by_model`（參數 `agent_id?`、`days?`，預設 7、鉗在 1–365），授權沿用 `cost.*` 同族的 admin 閘。`cost_summary`／`cost_agents` 內的彙總若失敗只會讓 `by_model` 變成空陣列，不會拖垮呼叫端真正要的那份摘要。
- **角色之間的交接只走一個門：`team_handoff` MCP 工具（P1/WP-5）**：「團隊即員工」裡的規劃／執行／審核三個角色要把工作交給下一棒時，最自然的做法是把上一棒的對話逐字稿貼過去——而那正好是跨模型搬不動的東西（各家 tool-call 線格式兩兩不相容，OpenAI 自己明講切換模型家族時 reasoning 會被靜默省略），實測也輸給結構化筆記（arXiv:2606.02875，事件數 −20~59%、prompt token −42~63%）。所以交接只有一種型別、一個入口：`team_handoff` 收一份完整的 `TaskPacket`（WP-1 已落地的型別），**明確的工具呼叫，絕不從回覆文字解析**——照 `working_state_set` 的紀律。身分不是參數：角色、任務、輪次從呼叫者自己的 agent 目錄讀（spawn 腳手架寫的 `[team_member]`，退回 `[agent] role`），packet 自稱是別人一律拒絕；合法的交接邊只有 `planner→executor`／`executor→verifier`／`verifier→executor`（修補），其餘（含任何碰到 `utility` 的邊）全拒。`transcript`／`messages`／`tool_use`／`tool_calls`／`function_call`／`functionCall`／`thinking`／`reasoning`／`encrypted_content` 出現在 packet 任何一層都是硬錯誤，而且**錯誤訊息會指名是哪一個鍵**（型別本身已 `deny_unknown_fields`，這層是為了教得清楚）。超上限一律整筆拒絕，絕不截斷。成功寫入 `<home>/team_packets/<task_id>/r<round>/<from>-to-<to>.json`（暫存檔＋fsync＋rename，跨行程檔案鎖，0600；同一 `packet_id` 重送覆寫自己＝逾時重試冪等，**不同** `packet_id` 落同目錄編號兄弟檔 `…-to-….01.json`——規劃者把一個目標扇出成多個獨立子任務時每個子任務一份封包，composer 讀整個輪次目錄並逐檔重驗角色，覆寫同一路徑會靜默只留最後一份），並同時落一列 `artifacts.jsonl` 產物來源（origin `produced`，帶 task_id 所以任務詳情頁是「精確歸屬」不是時窗推定）與一筆 `team_handoff` 稽核事件（`constraints` 記條數、`audience` 記原文——「這份內容可以流向誰」正是這個 allowlist 存在的意義）。權限沿用 `working_state_*` 同一個 scope（`memory:write`），**刻意不開新 scope**：角色成員的工具集本來就是母員工的子集，開新 scope 只會逼每個可能組隊的員工都多拿一個權限，零隔離收益。工具同時進 `SELF_ECHO_TOOL_NAMES`——packet 是寫的人自己的摘要，永遠不能拿來當自己主張的證據。寫入與讀取共用**同一個**路徑推導（`duduclaw_core::task_packet::packet_path`，composer 端原本那份自己手刻的 `packet_dir`/`packet_path` 已刪除）：一條 leg 上同一個 `packet_id` 覆寫自己那一檔（逾時重交是冪等，不會把同一個子任務變成兩份），不同 `packet_id` 取下一個編號槽（`…-to-….01.json` … `.99.json`）。composer 改成**按槽號枚舉**這條 leg 的檔案（canonical 先、再 `.01`、`.02`…），不再掃整個 round 目錄後按檔名排序——舊做法的位元組排序會把 `-2` 後綴排在 canonical **之前**（扇出順序與到達順序不一致），而且會讀到任何被丟進目錄的外來檔。每一份仍然**不信檔名**：`from_role`／`to_role`／`goal_id`／`round` 與所讀的 leg 不符、或 `TaskPacket::validate()` 不過，就跳過並落一列 `team_packet_skipped` 稽核（含 error_type 與檔名）；一份壞檔不會賠掉整條 leg，整條 leg 全壞時再多落一列 `all_packets_invalid`——「這一段沒寫」與「這一段寫出來的全是垃圾」從此分得出來。
- **`## 約束` 與 `## 受眾` 兩段永不被壓縮（prompt compression never-trim）**：壓縮預算對「邊界」是單方面課稅的——arXiv:2608.29028 量到 boundary 存活率 0.80→0.57，而約束一旦從明確變模糊，違規率從 <15% 跳到 50–73%；受眾 allowlist 則「幾乎清零」洩漏。所以 TaskPacket 的 `constraints`／`audience` 這兩段在 `prompt_compression` 的三個階段（TurnTrim／DropOldestToolEchoes／BisectAndSummarize）全部豁免，與 `working_state` 權威區段同級。段落以 composer 匯出的兩個常數（`SECTION_HEADER_CONSTRAINTS = "## 約束"`／`SECTION_HEADER_AUDIENCE = "## 受眾"`，另認英文 `## Constraints`／`## Audience`）開頭、到下一個一/二級標題為止；標頭比對是去空白後**完全相等**，`## 約束（勿刪）` 這種裝飾版刻意不受保護——模糊比對等於讓任意 agent 自撰文字宣告自己免受預算限制。保住這兩段就超出 token 預算時，管線**明確失敗**（`BudgetExceeded.protected_section_tokens > 0`，且提前退出不空跑階段）交由呼叫端原封不動送出，而不是去截斷約束：貴是可回復的，被悄悄砍短的約束不是。沒有這兩段的訊息，三個階段的輸出逐位不變。composer 渲染封包時**直接引用**這兩個常數（不再自己寫 `constraints:` 這種標頭），所以「標了但沒被保護」不可能發生；兩段一律排在封包最後，後面接一行 `## 封包結束` 把保護區關掉——否則保護區會一路吃掉封包之後的驗收標準、風險邊界與工作狀態，把可壓縮的內容也變成不可壓縮的，反而更容易撞上 `BudgetExceeded`。
- **角色跑在哪裡：ephemeral 成為「團隊角色成員」的載體，每個角色綁自己的 `(runtime, model, effort)`，回合結束即刻回收（P1/WP-2，預設不改變任何行為）**：「團隊即員工」的角色不是第二個 AI 員工，而是一個**角色成員**——`~/.duduclaw/agents/.ephemeral/` 下的臨時目錄，為某個 `(任務, 回合, 角色)` 而生、回合終態即消失，不進 registry、不進「你的團隊」名冊、不心跳、不自我演化，成本仍自動折回該員工（`eph-` 前綴是成本歸屬 JOIN 的鍵，故刻意保留）。與既有臨時子代理唯一的差別是它帶自己的腦：`agent.toml` 先逐字複製員工的 `[model]/[runtime]/[container]/[budget]`，再**只覆寫四樣**——`[runtime] provider`（canonical id，別名 `agy` 寫成 `antigravity`）、`[model] preferred`（角色的原始模型 id）、`[model] effort`（**只有宣告時才寫這個鍵**）、以及 `[team_member] role/task_id/round/parent` 這筆指派紀錄。員工設定的其餘鍵（utility 模型、account_pool、容器隔離、預算）全部繼承；兩個鍵刻意**不**繼承：effort 沒設就完全不寫鍵（spawn 不送旗標、走 provider 預設深度，與 WP-3 的契約一致），而繼承來的 `[runtime] fallback` 一律移除——第一次失敗就悄悄把角色換到另一家廠商，正好抹掉團隊存在的理由（執行者與審核者的家族去相關）。**「模型不自選」原封不動**：接受原始模型 id 的入口只有 `scaffold_role_member`，而它**不對 MCP 開放**——agent 面的 `spawn_ephemeral` 仍然只收 `cheap`/`standard`/`preferred` 三個 tier 關鍵字並拒絕原始 id，只有 gateway 內部的 composer 走角色路徑。建立任何東西之前先驗證 `(runtime, model)`：runtime 必須在首批五家白名單內、模型家族必須是該 runtime 真的服務的家族，catalog 認不出的家族一律報錯**絕不猜**（Goose #10731），所以 `gpt-5.4` 不會先被送進 Claude 二進位再到別人家的 API 才失敗。同時修掉一個會讓整套設定失效的既有邏輯：`resolve_tier_model_for_dir` 現在對角色成員**原始模型優先於 tier**——過去只有「非 Claude provider 才忽略 tier」，於是一個 Claude 家族的角色成員（例如審核者指定 `claude-haiku-4.5`）會被 `tier_model` 換成員工的 tier 陣容，四家廠商的團隊悄悄塌回一家。**回合結束即刻回收，不等寬限期**：新增 `finish_role_member(home, member_id, outcome)` 寫 `.completed` 標記**並立即刪除目錄**（先寫標記再刪，萬一刪除失敗仍由既有 sweep 補收，最壞是晚一小時而不是永遠不收）。這是算術不是潔癖：一角色一回合一份 scaffold，3 角色 × 5 輪 × 3 併發任務 = 45 份同時存活，而 `[dispatch] ephemeral_max_active` 預設 32——沿用「完成後 1 小時寬限 + 24 小時 TTL」的舊政策，溢出會進佇列然後靜默過期，某一輪就這樣少一個角色，而 log 裡沒有任何一行把它連回 GC。回合在 `finish_role_member` 之前就死掉的成員，改由 sweep 在下一次維護時收掉（`.completed` 存在即到期）；**普通臨時子代理的 GC 政策逐位不變**。已寫入的成本列全部保留（在 SQLite 裡，刪目錄不損帳）。**兩個必須分開的上限**（兩者都仍是上限，不是新的逃生口）：① **頻率**——角色成員 spawn 記在自己的 path kind `role_team` 與自己的預算 `[dispatch_guard] role_team_max_in_window`（預設 60 = 3 角色 × 5 輪 × 3 併發），不再跟守著員工自己 spawn 的 20/分鐘共用；桶的 key 本來就是 `(path_kind, agent_id)`，所以兩邊既不共桶也不共預算——一個設定正確的三角色團隊單任務一分鐘就 9 次、三任務約 30 次，共用預算下會在算術上直接跳閘，而那看起來像「平台壞了」而不是「碰到上限」。② **容量**——撞到 `ephemeral_max_active` 時角色成員改**進既有的持久佇列**（同一個 state file、同一組 `queue_max_depth`/`queue_item_ttl_secs`）而不是硬 `Err`；票券獨立一個 class（`role_team`）以免每小時的 ephemeral drain 把它當普通 ephemeral 重播、整份模型指派被靜默丟棄，replay 歸 composer 所有。票券以「回合」為 owner scope，回合終態即清掉自己的待審成員（沒有人會去讀的答案不該還在排隊）。**驗證失敗（模型家族不符、越權工具、parent 壞掉）一律立即拒絕、永不入列**——重試不可能成功。另附一個純函式容量顧問 `role_team_capacity_check(ephemeral_max_active, max_concurrent, iteration_cap, roles)`，在 `ephemeral_max_active < max_concurrent × iteration_cap × roles` 時回一句警告（預設值就是 45 > 32），讓這件事在啟用團隊時被說出來，而不是日後從「少一個審核者」反推。工具子集多一條例外：`TEAM_INTRINSIC_TOOLS = ["team_handoff"]` 對角色成員一律放行，**只在 composer 這條路**（MCP `spawn_ephemeral` 不吃這份名單，agent 自己合成的 ephemeral 永遠拿不到）。理由是它不是一種能力而是團隊自己的交接管道——推導出來的 `team_packets/` 路徑寫一份封包，其餘什麼都碰不到，射程與 `working_state_*` 相同；沒有這條例外，任何一個在團隊之前就寫好 `[capabilities] allowed_tools` 的員工都不可能組成團隊（那份白名單不可能列出當時還不存在的工具）。其他工具照舊逐一比對母員工的信封，母員工的 `denied_tools` 仍然勝過例外（明確禁掉就是一個看得見的失敗回合，不是靜默越權），工具名字元檢查也不放過。**角色成員自己的 `.mcp.json`（活體驗證抓到的 ship-blocker）**：scaffold 現在在 `agent.toml`／`SOUL.md` 之外一併寫出 `.mcp.json`——只有 duduclaw 一台伺服器，加上 `DUDUCLAW_AGENT_ID`／簽章 token／home／port 的身分 env 區塊——寫不出來（含 duduclaw 二進位解析不到絕對路徑這種會靜默寫不出檔的情形）就整個 scaffold 失敗並刪掉目錄：拿不到 MCP 的成員交不回東西，留著只會占掉一格容量額度。先前**沒有任何一處**替 `.ephemeral/` 下的目錄寫這個檔——Claude CLI 是從工作目錄自動發現 `.mcp.json` 的，而開機那次修補只走 `<home>/agents/*`、不下探 `.ephemeral/`，於是規劃者連 `team_handoff` 都沒有，整輪以 `planner_no_packets` 結束（第一次真實團隊回合的實錘）。身分 env 區塊也是那台 mcp-server 唯一能知道 home/port 的地方：spawn 白名單刻意把 `DUDUCLAW_HOME`／`DUDUCLAW_PORT` 從子行程環境濾掉，繼承不來。員工的其他 MCP 伺服器（瀏覽器之類）刻意不複製，成員的射程維持在該回合要求的工具子集（能力過濾與去識別化 proxy 是 per-spawn 的事，不在這裡決定）。**同一輪活測的第二件事：角色成員不再蒸餾記憶**——成員跑完後 gateway 照常跑對話蒸餾，把 5 筆時序記憶寫在 `eph-agnes-r1-planner-9d9044` 這個用完即丟的 id 底下，而該目錄下一刻就被回收；`wiki_ingest` 這條管線的單一入口（不是兩個呼叫端，以免日後有第三個呼叫端把它漏掉）現在先看 `[team_member]` 標記，是角色成員就整段跳過（含 profile 那一階，那也是一次記憶寫入），debug 留一行 `role member: memory distill skipped`。標記判定刻意只看區段**存不存在**而不要求欄位完整（壞掉的標記也該擋下副作用）。事實**不**改寫到母員工名下：那是 provenance 決定（這筆觀察算誰的、origin trust 給多少），另案處理。設計全文 `commercial/docs/DESIGN-team-as-agent-2026-09.md` §3.7／§3.8；設定見 `config/duduclaw.example.toml`；使用者面說明 `docs/features/56-team-as-agent.md`。
- **可逆上下文取回（CCR，預設關）**：`config.toml [ccr]` 區段（`enabled`／`allowed_sources`／`verified_causal_routes`／`verified_wiki_routes`／`min_compress_bytes`，全部預設關閉）讓一次工具呼叫的長回覆先落地成一份有 scope 與租約保護的原文，模型在對話裡看到的只是精簡 preview；模型可自行呼叫新的 MCP 工具 `duduclaw_ccr_retrieve`／`duduclaw_ccr_find`，依當下的 scope 取回自己有權限的原文片段（跨 scope 一律拒絕）。管理端新增 `GET /api/ccr/dashboard`（唯讀投影，admin-only）與 `POST /api/ccr/replay`（四臂 replay，admin-only）；CLI 補 `ccr-revoke-scope`／`ccr-revoke-artifact-version`（人工撤銷）與 `ccr-eval`／`ccr-compare`／`ccr-compare-replay`／`ccr-compare-run`／`ccr-compare-synthetic`（離線評估與比較）。通道 session 新增 `session_ccr_references` 表記錄每輪引用了哪些 handle，供撤銷時清理。詳見 [`docs/spec/reversible-context-ccr.md`](docs/spec/reversible-context-ccr.md)。
- **證據綁定的因果圖（因果證據圖擷取，預設關）**：`config.toml [causal_extraction]` 區段（預設 `enabled = false`）讓管理員把受限範圍的來源文字送給指定 provider（Anthropic／OpenAI／Gemini 任一）擷取候選的因果主張，經人工逐條核准或覆核後才進入 `causal_claims`／`causal_model_edges`／`causal_evidence` 等表；未經人工核准的候選不影響任何既有行為。新增 `/api/causal/*` 24 條 admin-only 路由（claim 清單/核准/改寫、model 建立/核准/假設檢視、effect estimate、negative-control review、來源匯入/撤銷/抹除、alias 設定/撤銷等）與新頁面 `/app/system/causal`（Causal Curation）。CLI 補 `causal-eval`／`causal-eval-compare`／`causal-effect-eval`／`causal-observational-demo`，供離線驗算合成資料生成器與效果估計器。詳見 [`docs/spec/causal-evidence-graph.md`](docs/spec/causal-evidence-graph.md)。
- **決策孿生／Decision Lab（管理員專用，目前僅合成與操作者自行上傳資料驗證）**：新頁面 `/app/system/decision-lab` 與 `/api/decision/*` 34 條 admin-only 路由，把「客服積壓」決策孿生從 CLI 搬上儀表板——探索性排班情境模擬（合成 fixture 或操作者上傳的歷史匯出兩條分支）、前瞻性影子預測日誌（凍結預測、事後計分、通過篩選才可人工檢視的審查閘）、以及把「這筆篩選通過了」的事實記成一張可稽核的人工檢視收據。`decision-*` CLI 子命令族提供對等的本機操作路徑，供未接上儀表板前的離線驗證。**明確邊界**：通過篩選只代表「符合人工檢視資格」，不促成任何模型、不授權任何排班或人力調度決定；目前僅以合成資料與操作者自行上傳的歷史匯出驗證引擎正確性，尚未接上任何真實客服系統或即時工單流。詳見 [`docs/spec/support-decision-twin.md`](docs/spec/support-decision-twin.md)。
- **UCCI 校準級聯（本地推論路由，實驗性，預設關）**：`inference.toml [router]` 新增 `ucci_fast_router`／`ucci_strong_router`／`ucci_shadow_strong` 三個欄位（皆預設 `None`／`false`），可為 LocalFast／LocalStrong 各掛一個已離線擬合好的等滲校準 router，改用 top-2 token margin 不確定性決定是否升級到下一層——啟用其中一層後，該層不再諮詢既有的 `g = sigmoid(α·p̄+β)` post-hoc 判斷，未配置 UCCI 檔的層則直接接受本地答案。`scripts/ucci_fit.py`／`scripts/ucci_pair.py` 是兩支需要人工審閱觀測資料、離線執行的 Python 擬合工具，不在 gateway 裡自動觸發。詳見 [`docs/features/57-ucci-calibrated-cascade.md`](docs/features/57-ucci-calibrated-cascade.md)。
- **`ucci_shadow_strong` 影子生成並行上限**：`[router]` 新增 `ucci_shadow_max_inflight`（預設 `1`，`0` 視同 `1`）。影子生成改成背景 `tokio::spawn` 之後少了並行上限，一波被接受的 Fast 回覆可能同時疊出多個背景 Strong 生成，跟下一則請求的前景生成搶同一個後端／模型槽位。現在用 `Arc<Semaphore>` 的 `try_acquire_owned` 限流：達上限時直接跳過該次影子（不排隊、不阻塞前景），計入 `shadow_skipped_inflight` 並留 `debug!` 紀錄；`flush_shadow_observations()` 仍會等所有已成功派生的影子跑完。詳見 [`docs/features/57-ucci-calibrated-cascade.md`](docs/features/57-ucci-calibrated-cascade.md)。
- **決策孿生的關機開關 `config.toml [decision] enabled`**：Decision Lab 這條線出貨時一個 config 鍵都沒有，操作者除了重編譯之外沒有任何方法關掉它——對一條自己的 spec 就寫著「exploratory」的表面來說，這是最大的治理缺口。新增 `[decision] enabled`（**預設 `true`**，維持現狀逐位不變），設成 `false` 時 gateway 在路由前就把每一個 `/api/decision/*` 請求回 `404`；`/api/causal/*` 與 `/api/ccr/*` 有各自的開關，刻意不受影響。路徑比對錨定在路徑段邊界（`/api/decisions`、`/api/decision-lab` 不受牽連），設定缺漏或寫壞一律退回預設（開著），旗標在開機時解析一次，改了要重啟 gateway 才生效。
- **三條實驗線的示範模式提示**：決策實驗室、因果證據審查、可回溯壓縮儲存三頁在標題下方多一行可關閉的提示，明說「這一頁目前跑的是合成資料，還沒有真實資料試點」；決策實驗室會依後端自己回報的狀態（`synthetic_only_exploratory` 對 `operator_supplied_exploratory`）切換成「數字來自你上傳的檔案」的措辭。關閉狀態逐頁記在瀏覽器本機，讀不到瀏覽器儲存時照常顯示。三語系齊備。
- **Telegram 小程式終於有儀表板開關**：`config.toml [miniapp] enabled` 先前完全沒有 UI（`grep miniapp web/src` 零命中），想開只能手改檔案，等於這個功能對一般使用者不存在。設定 → 系統新增「Telegram 小程式 / 在 Telegram 裡直接審批」開關，走既有的 `system.config`／`system.update_config`（管理員限定），i18n 三語齊備。**預設維持關閉**，開啟後下一則訊息就生效（路由本來就每次請求重讀設定），不需重啟；儀表板網址不是 https 時按鈕依然不會出現（Telegram 的限制，不是本開關造成）。
- **夜間引擎功能文件（`docs/features/58-night-engine.md`，三語）＋ `config/duduclaw.example.toml` 的 `[night]` 段**：2,478 行的夜間整理引擎有兩道各自獨立的 opt-in 開關、預設全關、沒有功能文件，而範例設定檔連 `[night]` 這個段名都沒出現過——使用者不可能知道要開什麼。新文件寫明四個子階段（N1/N2 走輔助模型、N3/N4 確定性零成本）、兩道開關各自的職責、每 pass 花費上限與每日斷路器、閒置判定，以及怎麼從日誌確認它真的跑過。**程式行為一個位元都沒動。**
- **本機 proxy 功能文件（`docs/features/59-local-proxy.md`，三語）**：`duduclaw proxy`（1,591 行）讓 Aider／Cline／Codex 借用已付費的帳號池，卻沒有任何 feature 文件。新文件涵蓋三個端點、`provider/model` 名稱解析、Bearer 金鑰四段解析順序、per-IP 流量限制與 loopback 預設，並誠實寫明**訂閱制 OAuth 席次無法轉發**（輪替器選到 OAuth 席次時會明確回 503，不是靜默空回覆）。**程式行為未動。**
- **Streamable-HTTP 與 Remote OAuth 的 HTTP 層測試**：`/mcp` 與 `/oauth/*` 這兩個對外可達面先前只有 2 與 5 個單元測試，沒有任何一個走過真正的路由。新增九個整合測試：無 token 回 401 且帶 RFC 9728 的 `WWW-Authenticate`、格式正確但未註冊的 token 回 401、壞 JSON 回 `-32700`、未知 `MCP-Protocol-Version` 回 400、無狀態語意（不發 `Mcp-Session-Id`、`GET`／`DELETE` 回 405、帶著自編 session id 仍照常服務）、DCR 全迴路（註冊 → 操作者核可 → PKCE 換 token → 用該 token 打通 `/mcp` → 重放授權碼失敗）、非 loopback 的 `http://` redirect_uri 在註冊時就被拒、外部 key 與無效 key 都無法核可授權且不會把授權碼導向任何地方、兩份 discovery 文件免認證可取得且欄位齊全。
- **自動化規則可以訂閱 Odoo ERP 變更**：新增 `odoo_event` 觸發事件（輪詢與 `POST /webhook/odoo` 兩條管道共用），可用在一般規則與 `sequence` 時序規則的兩側——「訂單確認後 7 天內沒有收款」正是後者存在的理由。條件除了 `event_type`／`model`／`record_id`，記錄的頂層純量欄位會攤平上來，所以直接寫 `{"field":"state","op":"eq","value":"sale"}` 即可，不需要新運算子。保留欄位名（`event`／`event_type`／`model`／`record_id`／`record`）不可被記錄內容覆蓋。
- **`duduclaw pack {list,install,inspect}`：三套包格式合一的單一入口（T5／O2）**：「一組配置好的 AI 員工」先前有三種 manifest 方言（專家包 `expert.toml`、付費團隊劇本 `team.toml`、職務組合 `preset.toml`）配三個安裝動詞，加上第四種沒被列進稽核表的 `<industry>-pro/` 產業包目錄。現在共用一份 `pack.toml` schema 與一個 in-memory 型別（`duduclaw-core::pack`，`kind = preset｜team｜template`、`tier = free｜premium`、`agents[]`／`humans[]`／`excluded[]`／`eval_suites`／`autopilot_rules`／`examples`），四個 legacy loader 原樣讀舊檔、**不遷移磁碟上任何一個位元組**。`pack list` 把已安裝的包、本機職務組合與內建目錄併成一張表；`pack inspect` 印出正規化後的樣子並說明來源是哪一代格式，`--emit-canonical` 印出對應的 `pack.toml`（只印不寫——付費內容樹的法規 overlay 是逐字人審過的，不交給機器改寫）；`pack install` 依 `kind` 分流：職務組合寫進職務組合庫，團隊／產業包交給**原封不動**的專家包安全管線（`safe_zip`、注入與技能掃描、hooks 隔離、`org_store` 回寫），所以安裝結果與舊路徑逐位相同。`team.toml` loader 與 `expert convert-teams` 的輸出逐欄對齊（前台無 department／rank `manager`／trigger `@<顯示名>`；worker rank `staff`、department 由 `org::department_for_kit` 推導），並以 repo 內已 committed 的 `experts/<產業>-team/expert.toml` 當 golden 對照（clinic／lawfirm／pharmacy 三包＋`cad-drafter`／`marketing-designer` 兩個 standalone）。設計：`commercial/docs/DESIGN-pack-format-unification-2026-09.md`。

- **`config.toml [tick] preset`：常駐感知的兩組預設旋鈕（T5／O15）**：速率上限、DNS TTL、閒置看門狗、client ping、baseline 壽命、`persist_every_n` 再加上初篩層的 fail-open 政策，是稽核點名「旋鈕遠超一般使用者可理解」的一節。新增 `preset = "conservative" | "aggressive"`：`conservative` 給 `max_events_per_minute = 30`／`baseline_max_age_secs = 900`／`action.screen.on_unavailable = "drop"`（fail-closed），`aggressive` 給 `600`／`21600`／`pass`。**preset 只補你沒寫的鍵**——來源上明寫的 `max_events_per_minute`、規則上明寫的 `on_unavailable` 永遠優先，包含你寫的值剛好等於舊預設值的情況（判斷依據是原始 TOML 有沒有這個鍵，不是解析後的值跟預設值比大小）。不寫 `preset` 時所有預設值逐位不變；值打錯會記一筆 warn 並忽略，不替你猜。儀表板「即時監控來源」卡片唯讀顯示目前生效的 preset（`ticks.sources` 新增 `preset` 欄位）。文件：`docs/features/41-resident-sensing.md`（三語）、`config/duduclaw.example.toml`。

- **任務板成為決策孿生的第一個真實資料來源（X1 方案 1）**：決策孿生線上線以來只跑過合成資料與「操作者自己上傳的檔案」，稽核把「沒有真實試點」列為它最大的存續風險。新增 `decision_task_board_export.rs`：從本機 `tasks.db` 唯讀取出 `id`／`created_at`／`completed_at`／`assigned_to` 四欄（不碰標題與描述，工單內容永遠不會進入 pilot 來源檔），組成符合既有契約的匯出；佇列是 `task-board:<agent>` 或整板 `task-board:all`。**誠實邊界寫在每一份收據裡**：`staffing[].agents` 取的是該員工 `agent.toml [heartbeat] max_concurrent_runs`，這是**靜態代理值**不是實際排班，成本軸固定 0（任務板沒有薪資），`sla_days = 2`（同日或隔日結案）是規定的定義不是議定的服務目標——任何依賴「人力隨時間變動」的結論在這個來源上都不成立。時窗一律整日且結束於最近一個完整的 UTC 午夜，跑到一半的當日會被拒絕而不是截斷（否則會報出假的到件量下滑）。出口三條：`POST /api/decision/task-board-export`（admin）、`duduclaw decision-task-board-export`、以及 `config.toml [decision] task_board_shadow`（**預設關**，另受 `[decision] enabled` 總閘管制）的背景排程。排程每期走**同一條** `import_operator_pilot` 路徑，所以收據、來源綁定、replay 雜湊與 held-out SLA 診斷全是既有且審過的程式碼，再算一筆隔日 SLA 前瞻預測寫進 `decision_task_board_runs.jsonl`；該預測**刻意不寫成已計分的 `shadow_sla_forecast`**——那個儲存體要求操作者先建立 shadow policy 與目標午夜後才取得的期初存量來源，背景工作不得代替人做這個事前登記。文件：`docs/spec/support-decision-twin.md`、`config/duduclaw.example.toml`。
- **Odoo Helpdesk／Project 轉接器（X1 方案 4）**：新增 `duduclaw-odoo::support_export` 與 `decision_odoo_export.rs`，把 `helpdesk.ticket`（EE）或 `project.task`（CE）映射到同一份 pilot 契約。建立與結案時間是實測值（Odoo 的 naive UTC `"2026-09-01 08:30:00"` 只在一個函式裡轉成 RFC3339，轉不了的列計入 `skipped_rows` 而不是當成「現在」）；`project.task` 以 `date_end` 優先，只有在 folded 階段（`project.task.type.fold = true`）才退到 `date_last_stage_update`——單看後者會把每一張被動過的工單都報成已結案。`staffing[].agents` 是當日有建單或結案的**不同承辦人數**（代理值，Odoo 沒有排班表），`fixed_extra_capacity` 恆為 0。只讀 `id`、時間、佇列 many2one 與承辦人，主旨／描述／客戶／email 永遠不進來源檔；每頁 500 列、上限 20 萬列。出口：`POST /api/decision/odoo-export` 與 `duduclaw decision-odoo-export`，兩者都用該 agent 自己的 `agent.toml [odoo]` 覆寫疊在全域 `[odoo]` 上，憑證走既有 `config_crypto`（`secret://` 參照照常解析，永不把參照字面值送給 Odoo）；`--profile` 與該 agent 實際 profile 不符一律拒絕，不靜默退到別人的身分。**兩個轉接器都只產檔案、不自動匯入**——匯入是操作者的動作。
- **稽核紀錄成為因果證據圖的第一個真實來源（X1 方案 2，零 LLM、零外送）**：目標任務結算為 `rejected`／`needs_human` 時，`causal_audit_ingest.rs` 把該輪的 `tool_calls.jsonl` 原始位元組寫成來源檔，並對它提一條候選主張：cause 是觸發的歸因規則 token（有失敗工具時細化成 `tool_error:<tool>`，沒有就維持原 token，**絕不憑空造一個工具名**），effect 是 `task_rejected:<goal_kind>`／`task_escalated:<goal_kind>`，證據是指向該稽核列的位元組區間（可逐字回放）。**這是轉錄不是因果推論**：記下來的是「這些位元組寫在同一輪」，要成為證據仍必須有人在因果審核頁按下通過。邊界：`config.toml [causal] audit_ingest`（**預設開**，因為零模型呼叫、資料不出機器）、每 agent 每 UTC 日 50 條、`(agent, cause, effect, 日)` 去重（與配額在同一個 `with_file_lock` 讀改寫裡，兩個並行結算不會都認為自己是第一個），任何失敗一律 fail-open 到「不寫」。
- **AEE 新增 `causal_support` 量測維度（X1 方案 2 的消費端）**：候選 playbook 條目的 `signals_match` token 中，有多少比例被**已人工審核通過**的因果主張支持（精確比對，不是子字串——否則每個 token 都會「有支持」）。零主張時是 `None`（未量測）而**不是 0.0**。它**刻意不進 commit 閘的 `dimensions()`**：一條轉錄的共現紀錄加上一個人的放行不等於品質量測，讓它左右採用判定就是規格明文禁止的「過了篩選＝獲准晉升」。要它有牙齒得顯式開 `config.toml [evolution] require_causal_evidence`（**預設關**）：開啟時無支持的新條目帶既有 `shadow-rule` 標籤進場——照樣儲存與評分，但在證據補上前不注入；條目是留著不是丟掉。關閉時寫入路徑逐位不變。
- **`[ccr] builtin_sources`（預設開）：CCR 終於接得住自家高輸出工具**：把 DuDuClaw 自己 MCP server 的 11 個唯讀高輸出工具（`db_select`／`db_query`／`csv_read`／`xlsx_read`／`file_read`／`web_fetch_cached`／`web_extract`／`shared_wiki_read`／`wiki_read`／`memory_search`／`memory_fetch_batch`，以實際註冊名為準）併進 `allowed_sources`。**這只改變「哪些來源可被接住」，不改變 CCR 是否開啟**：`[ccr] enabled` 維持 `false`（預設），關著時 allowlist 仍為空、`for_agent` 仍回 `None`；`min_compress_bytes`（4096）不變，小輸出逐位不變；`builtin_sources = false` 與改動前逐位相同。清單只收唯讀且可回取的工具，有不變量測試擋掉會改狀態或只回吐 agent 自己摘要的工具（`SELF_ECHO_TOOL_NAMES`）。預設維持 `enabled = false` 的理由與量測方法誠實記在 `wiki/reports/ccr-real-measurement-2026-09-29.md`——本機沒有任何 provider 憑證（`env | grep -c '_API_KEY=' == 0`、`config.toml` 無 `[accounts]` 區段），四臂成本／品質比較**未量測**，不以合成數字冒充實測。
- **三個實驗頁的資料來源標示區分「合成」與「本機真實」**：`DemoModeNotice` 新增 `taskBoard`／`auditTrail` 兩種來源，決策孿生頁在 catalog 出現 `task-board:` 佇列時、因果審核頁在 scope 為 `local`／`audit` 時改顯示「資料來源：⋯（不是合成資料）」，並在同一句話裡講明代理值與「共現不等於因果」的限制——這裡的「真實」只代表「不是生成的」，永遠不代表「已驗證」。三語同步。

### Changed
- **O7 `tools/list` 依呼叫者能力裁剪（行為變更）**：MCP 工具 schema 是**每次 spawn 都要付的固定 prompt 成本**——CLI 每個 session 讀一次 `tools/list`，整包在第一個使用者 token 之前就進了模型上下文。先前只有 Google／GitHub 整合閘、`allowed_tools`／`denied_tools` 與 `db_sources` 會裁剪，其餘五道 per-agent 預設關的能力閘（`os_native` 6 個、`recording` 5 個、`system_operator` 19 個、`codrive` 2 個、`computer_use` 7 個）與 `[fork] enabled`（6 個）、PORTICO `scoped_tools`（無有效授權者）完全不裁剪：一個剛建立的 AI 員工每次開機都收到 45 個它呼叫必被拒的工具 schema。現在 `tools/list` 的每一道過濾都讀**派工層強制時用的同一個常數**（`mcp_dispatch::{OS_NATIVE_TOOLS, RECORDING_TOOLS, SYSTEM_OPERATOR_TOOLS, CODRIVE_TOOLS, COMPUTER_USE_TOOLS, DB_SOURCE_TOOLS, FORK_TOOLS}`，`tools_list.rs` 裡原本那份手抄的 `DB_SOURCE_TOOLS` 副本一併收斂；`computer_use` 的派工 match 臂也改讀同一份清單），不可能再漂移。**量測（同一個 scaffold agent fixture，前後對照）：215 個工具／120,092 bytes → 170 個工具／81,743 bytes，−31.9%。** 隱藏**不是**授權決定：呼叫未列出的工具仍然打到真正的閘、仍然被原訊息拒絕（有回歸測試釘住）。棄用別名（`shared_wiki_*`／`schedule_task`／`skill_bank_search`）不受影響，仍然列出。
- **MCP server 宣告 `tools.listChanged` 並發出 `notifications/tools/list_changed`**：隱藏工具過去等於永久拿不到（用戶端只在開場讀一次 `tools/list`），這是 CLAUDE.md 把 scaffold curation 列為 DEFERRED 的原因。stdio server 現在在 `initialize` 回應宣告 `"tools": { "listChanged": true }`，並每 5 秒重算呼叫者的可見集合、**只在集合真的改變時**送出通知（比較的是名稱集合而非檔案 mtime：碰了設定檔但內容沒變不送、WAL 寫入 stat 不到也照樣察覺）。所以 `capability_request` 核准、儀表板 `agent_update`、操作者手改 `agent.toml`、階段性授權於任務終態被撤銷——四條路徑都不必重啟就會讓工具出現／消失。第一次 `tools/list` 之前不送任何通知（沒有東西可作廢）。
- **每個工具／參數說明上限 200 bytes，長說明移到 `docs/guides/mcp-tools.md`**：94 個工具說明與 28 個參數說明超過上限（最長的 `codrive_run` 是 2,171 bytes），全部改寫；安全關鍵句（`mail_send`「這不會寄出」、`github_issue_comment`「這是公開可見的」、`team_handoff`「整筆拒絕絕不截斷」）一律保留，設計理由、逐項範例與內部代號搬進新的三語指南。**量測（全部 243 個工具）：工具說明合計 51,822 → 33,944 bytes，參數說明 31,604 → 27,120 bytes，合計 83,426 → 61,064（−26.8%）。** `team_handoff` 的最小封包範例先前在工具說明與 `packet` 參數各存一份（同樣 216 bytes 每次 spawn 付兩次），現在只留在 `packet`。唯一列明的例外是 `team_handoff.packet`（上限 1,024 bytes，原 1,525）：它就是 TaskPacket 契約本身，而封包是整筆拒絕不截斷的，看不到形狀就產不出合法封包；例外集中在一份清單並有測試在它不再必要時失敗。
- **CCR 兩個工具的 schema 收斂到單一註冊權威**：`duduclaw_ccr_retrieve`／`duduclaw_ccr_find` 的名稱與 JSON schema 先前以字面值內嵌在 `duduclaw-llm` 的 tool loop 裡——一套遠離專案其他所有工具定義的第二註冊面，沒有東西把它連到目錄、沒有東西讓它受說明預算約束。現在定義在 `duduclaw_core::tool_catalog::ccr_tool_schemas()`（`duduclaw-llm` 與 `duduclaw-cli` 都構得到的那個 crate），llm crate 只負責 handler 與注入，`duduclaw_llm::{CCR_RETRIEVE_TOOL, CCR_FIND_TOOL}` 以 re-export 保持公開 API 不變。它們刻意**不**進 `builtin_tool_catalog()`：那是 MCP 目錄，而 CCR 工具由直連 API 的 tool loop 注入、從不經 MCP 派工閘——有測試釘住這條界線。
- **`mcp/tools_def.rs`（4,252 行單一 `const TOOLS` 字面值）拆成 16 個工具族檔**：`tools_def/{channels,memory,agents,skills,ops,inference,odoo,google,integrations,wiki,exec,tasks,fork,data,os,recording}.rs`，最大一檔 528 行。分組是**原註冊順序的連續切片**，所以 `tools/list` 的順序逐位不變。Rust 不能串接 `const` 切片，所以 `TOOLS` 改為 `TOOL_GROUPS: &[&[ToolDef]]` ＋ `tools()` 迭代器（零配置、零 `LazyLock`），6 個呼叫端隨之更新。
- **來源撤銷時通道回覆改成固定拒絕字串**：啟用 CCR 時，若一則工具結果所綁定的來源在模型作答期間被撤銷，通道不再送出可能引用到已撤銷內容的回覆，改送固定文案（`The source changed before this reply could be sent. Please try again.`）；使用者可直接重試。
- **決策孿生的誤差總和一律改用十進位字串**：決策簡報的 `exploratory_forecast` 五個絕對誤差總和（`arrival`／`model`／`no_change`／`seasonal_naive`／`mean_change`）以前送 JSON number，儀表板 `POST /api/decision/forecast-validation` 對同一筆已保存紀錄卻送字串；同一個數字兩種型別，兩邊無法直接比對，超過 2^53 還會在瀏覽器靜默進位。現在兩邊都送十進位字串，前端一律以 `BigInt` 解析。**CLI `decision-brief` 這五個欄位的 JSON 型別隨之從數字改為字串**；儲存紀錄仍是 `u128`，雜湊後的 payload 一個位元都沒動。
- **三個決策 CLI 命令的 JSON 外層新增 `engine_matches_current`**：`decision-record-outcome`、`decision-load-outcome-model-screen`、`decision-event-load-run` 原本只印出已保存紀錄本身，讀者無從得知該筆歷史執行是否仍能被目前安裝的模擬引擎重現（載入時刻意不重算）。現在輸出改為 `{"record": <原本的物件>, "engine_matches_current": <bool>}`——旗標是讀取時重算的衍生事實，刻意放在紀錄外層，不得被誤認成雜湊 payload 的一部分。**這改變了這三個命令的最外層 JSON 形狀**；`record` 內容與先前逐位相同。
- **CLI 巨型 `Commands` enum 拆成扁平化子命令群（純內部結構，使用者面逐位不變）**：`duduclaw` 的 114 個頂層子命令以前全部展開在同一個 `Commands` enum，clap derive 因此產生**單一**約兩千行的 `augment_subcommands` 建構函式，在未最佳化的 debug 測試建置下光是呼叫一次 `Cli::try_parse_from` 就撐爆 libtest 預設的 2 MiB 執行緒堆疊，整個 `duduclaw-cli` 測試二進位在任何斷言跑之前就 SIGABRT（先前只能靠 `test_support::run_on_big_stack` 把 16 個測試搬到 32 MiB 堆疊上迴避）。現在依**原本相鄰的變體區段**切成九個 `#[command(flatten)]` 子 enum（`DecisionCommands`／`DecisionShadowCommands`／`DecisionOutcomeCommands`／`CcrCommands`／`DecisionPilotCommands`／`CausalCommands`／`OpsCommands`／`ToolingCommands`／`MaintenanceCommands`），每群各自擁有一個 `augment_subcommands` 函式、各自一個堆疊框。因為分群是連續區段、flatten 變體就插在原區段的位置，子命令的插入順序不變——`duduclaw --help` 與全部 208 個子命令 `--help` 畫面經前後快照比對**逐位相同**，指令名稱、旗標、說明文字皆未變動。`run_on_big_stack`（32 MiB）刻意保留為縱深防禦，並新增在**刻意限制為 2 MiB** 的執行緒上建構整棵命令樹的回歸測試，日後某一群再度長過上限時只會紅一個測試，不會再拖垮整個測試二進位。
- **never-trim 豁免權改綁「來源」而非「文字」（行為變更）**：`## 約束`／`## Constraints`／`## 受眾`／`## Audience` 是四個通用 markdown 標題，而壓縮管線的 `history` 就是十一通道的對話歷史、**含使用者自己送的訊息**——先前一行標題就能開出受保護區段，關掉壓縮並把後面的文字原文釘進 session summary、此後每輪注入系統提示。現在受保護區段的定義是「標題行**緊接**一行行程專屬標記（`<!-- ddc-protected:<64 hex> -->`，開機時由 OS CSPRNG 取 32 bytes 產生一次）」，而這行標記只有 `team_composer::render_packet_for_prompt` 會發出。使用者猜不到也拿不到：標記不進任何通道回覆，摘要用的 utility prompt 在送出前已把受保護段整段剝掉。三個失效方向刻意都倒向「不受保護」——呼叫端沒有 sentinel（`duduclaw-llm` 的 CCR preview 除非嵌入端傳入、任何非 gateway 行程）、gateway 重啟後舊摘要裡的舊標記、CSPRNG 不可用時 sentinel 為空：壓掉一條真約束是貴但可回復的，把豁免權交給不可信文字不是。標題常數與判定邏輯一併下沉到 `duduclaw-core::protected_section`，`duduclaw-llm/src/ccr.rs` 裡第二份手抄的標題清單（任何印出 `## Constraints` 的工具結果都能靜默跳過 CCR 壓縮）隨之收斂到同一個來源。既有的兩道上限（預算 floor 6k tokens、session summary protected 段 4 KiB）保留為縱深防禦，改為針對「合法發出端渲染超量」而非使用者攻擊。標記在 `compose_summary`（唯一會把封包文字給人看的路徑）送出前剝除，`/goals` 與通道推播都看不到。**相容性**：本次之前寫進 session summary 的受保護段沒有標記，之後一律視為可壓縮內容。`DelegationEnvelope::to_prompt` 產生的 `## Constraints` 屬 agent 提供的資料、刻意不加標記（它本來就不在 `history` 裡，實際行為未變）。
- **決策簡報其餘的累加整數也一律改十進位字串**：延續上一條，`baseline`／`alternative`／`delta` 的 `final_backlog`／`total_resolved`／`resolved_within_sla`／`total_staff_cost_cents`、`exploratory_event` 的同名總計與 `resolved_wait_seconds_p50`／`p95`、以及 `resolved_wait_seconds_p95_delta` 全部從 JSON number 改為十進位字串（delta 帶正負號，先以 `i128` 相減再輸出，字串就是精確差值，不經 `f64`）。`null` 仍代表「該情境沒有已解決工單」，不會變成 `"0"`。`exploratory_sla_holdout.diagnostic` 原本直接內嵌已雜湊的 `SlaHoldoutDiagnostic`——簡報裡唯一還把 `u128` 當 JSON number 送出的結構——現改為與 `POST /api/decision/engineering-validation` 相同的 wire mirror，同一筆已保存紀錄到哪裡顯示都只有一種線上形狀；四個誤差總和是字串，逐日計數維持數字。轉換一律在紀錄重載、依原始來源位元組重算、並取得 `record_sha256` **之後**才做，儲存結構與任何雜湊 payload 一個位元都沒動。**CLI `decision-brief` 與 `POST /api/decision/compare` 這些欄位的 JSON 型別隨之從數字改為字串**；前端以 `BigInt` 解析與渲染。
- **`[task_forward_model]` 的文件與程式碼對齊（文件錯，程式對）**：`impl Default` 自 v1.54 起就是 `enabled = true` / `calibration_enabled = true` / `held_out_gate_enabled = true`，但 `task_forward_store.rs` 的欄位註解、`handlers.rs::forward_model` 的註解、`docs/features/feature-inventory.md`、`docs/guides/evolution-switches.md`、`docs/guides/goal-loop.md`（含 ja-JP／zh-TW 副本）全都還寫著「預設關」。九處敘述一次改齊，`config/duduclaw.example.toml` 同步；程式行為一個位元都沒動。
- **`config/duduclaw.example.toml` 從 10 段補到全段涵蓋**：範例檔先前只示範約 10 個頂層段落，實際被程式讀取的至少 50 個——`[tick]`／`[goal_loop]`／`[redaction]`／`[mail]`／`[limits]`／`[belief]`／`[task_forward_model]` 這些一級功能全部缺席，使用者從範例檔根本看不出它們存在。現在每一段都有一句用途、每個鍵的預設值（全部註解掉，複製過去不會改變任何實際行為）與對應文件連結；常用的段落排在前面，進階的在後。另修正一處誤導：`[evolution]` 底下的 `gvu_enabled`／`max_silence_hours` 等六個鍵其實只在各 AI 員工自己的 `agent.toml` 生效，`config.toml` 從來沒讀過它們，範例檔現在明講這件事。
- **AI 智慧偵測卡明講量測到的極限**：去識別化設定頁的 NER 卡片先前只在勾選後的警告框裡寫「不是匿名化保證」。現在描述文字下方固定顯示一行：第二層輔助偵測、不是法遵保證、實測 zh-TW 整體召回率約 80%、人名約 72%、Intel 版 macOS 不支援。數字取自 `docs/features/55-data-sources.md` 的實測表，不是修辭。三語系齊備；`docs/features/55-data-sources.md` 的 zh-TW／ja-JP 譯本補上先前漏譯的同一條邊界說明。
- **PyPI 套件描述修正**：`pyproject.toml` 對外寫「80+ MCP tools, 7 messaging channels」，實測是 245 個 MCP 工具、11 個通道，且與同 repo README 的「200+」互相矛盾。改成實測值。
- **`[team] enabled` 預設從 `false` 改為 `true`（行為變更）**：Team-as-Agent 先前是**雙層預設關**——旗標關、可拆性閘再預設 Solo——所以三個月的工程量押在一條從未被實際執行過的路徑上。現在旗標預設開，但**真正決定成團的是有沒有在 `[team.roles]` 指名第二家廠商**，不是旗標：沒寫角色時執行與審核雙雙 cascade 到該員工自己的 runtime／model（`types::cascade_unbound_roles`，執行／審核以外的角色刻意不填，否則會憑空多出一個沒人設定的規劃階段），同家族因而被去相關規則拒絕，任務照舊走 Solo。這個拒絕是**安靜的**：新的 `FreezeOutcome::SoloByDefault` 只留一行 `debug!`，不寫 `team_refused` 稽核列——若每個部署的每一則目標任務都蓋一列，真正的拒絕就再也找不到了；明寫 `enabled = true`、或設了角色卻驗證失敗的操作者，仍然拿到原本的吵版稽核列。另外兩道安全條件：可拆性閘仍需四個訊號中的三個（一般任務逐位不變地走 Solo），`[dispatch.team_budget]` 連一輪降級後的最小編組都付不起時**第一輪改判 Solo**（`budget_forces_solo`），不把一個還沒開工的任務丟給人；已經花掉角色派工額度的任務維持原本的 `needs_human(budget_exhausted)` 並交出最佳輪成品。`enabled = false`（全域或單一員工）與 `gate = "always_solo"` 都是一行還原。
- **角色×模型量表接上 composer，成為未指定模型時的先驗**：`duduclaw eval --matrix` 產出的 `role_model_matrix.toml` 先前沒有任何消費者（量了沒人用）。現在放在 `<DUDUCLAW_HOME>` 底下時，`[team.roles.*]` **未寫 `model`** 的角色會先讀它、再退回員工自己的 `[model] preferred`。五道只會收窄的條件：明寫的 `model` 永遠優先（設定是決策，量表是量測）；`unresolved` 格一律忽略（含高分的 unresolved 不得壓過已解析格）；只採計該角色**自己 runtime** 上的格，不改動凍結的 `(role, runtime, model)` 三元組；勝出模型必須維持該角色的模型家族，否則會靜默破壞執行≠審核的去相關不變式；平手不算勝出，CLI 或憑證不在本機的 runtime 跳過。沒有矩陣檔時與接線前逐位相同。composer 沒有 domain 概念（目標任務不是 eval 套件目錄），因此同一 `(role, runtime, model)` 跨 domain 的格以案例數加權平均彙整。

- **`templates/orchestrator/` 與 `templates/KILLSWITCH.toml` 移到 `docs/examples/`**：`templates/` 底下其他六個子樹都有真接線（`include_str!` 編進 binary，或由 wizard 部署），這兩個**零程式引用**、只能手動 `cp`——擺在同一層會讓人以為它們會自動生效。移到 `docs/examples/` 並在每個檔案的檔頭明寫「MANUAL COPY ONLY／手動複製」與該複製到哪裡；`docs/README.md` 的目錄樹補上 `examples/` 一節。檔案內容除檔頭說明外未變。
- **license 的 feature flag 分成「能力閘」與「服務承諾」兩類（行為變更）**：`features.toml` 的 boolean 有兩種，先前混在一起長得一模一樣。真正有 `check()` 呼叫端、會改變程式行為的只有三個（`premium_templates`／`white_label`／`industry_evolution_params`）；另外八個（`dashboard_enterprise`／`priority_security_patch`／`private_discord_support`／`odoo_integration_supported`／`redistribution`／`dedicated_engineer`／`cloud_only`／`self_host_only`）描述的是「訂閱承諾某個人會做什麼」——支援管道、修補 SLA、部署形態、轉售權——binary 裡沒有任何強制點，卻長得像功能開關，等著某個未來的作者「gate 在上面」然後出一個什麼都不做的開關。現在 `FeatureGate::check()` **一律拒絕**這八個名字（回 `false`，不管 TOML 寫什麼），顯示用途改走新的 `FeatureGate::service_commitment()`；`duduclaw license status` 相應拆成「Unlocked commercial modules」與「Service commitments (not enforced by the software)」兩區。`features.toml` 檔頭寫明這兩類的差別與各自的成員清單。
- **`duduclaw reforward` 與 `duduclaw evolution clear-holdout-rotation` 改為 `--help` 隱藏（仍可執行）**：兩者都是操作者的事故復原工具而非產品表面。`reforward` 是 v1.8.21 的卡住派工回覆補送；`clear-holdout-rotation` **刻意不刪**——AEE 的 commit 閘仍會在平手提交時升起 `holdout_rotation_due`，而這是唯一能清掉它的東西，刪掉等於把那個旗標變回單向閥。
- **`[evolution] gvu_enabled` 出廠改為 `true`（行為變更）**：新建的 AI 員工從第一天起就會累積經驗法則，不必有人去開開關。先前的 fail-closed 預設（2026-08-06 WP0.1）成立於「引擎能整份改寫 `SOUL.md`」的前提，而那條路徑已於同批次移除（見 `### Removed`）——現在演化的落點是 playbook：一條條可獨立退場、各自綁至少一個 eval case、各自定案的行為規則，`SOUL.md` 對 AI 員工維持唯讀。**成本面**：一輪 AEE 的 LLM 呼叫由兩端節制——`gvu_cooldown_minutes`（預設 60 分）決定多久才能起一輪，零 LLM 成本的 Gate（`G-Safety`／`G-Contract`／`G-Canary-Static`／`G-Schema`／`G-Assertions`／`G-Capacity`）在付判官錢之前先否決注定失敗的候選，內迴圈上限 3 輪。受影響的產生點：`duduclaw onboard`（互動提示預設改為「是」，非互動模式從 `false` 改為 `true`）、`wizard.rs` 的產業板模、MCP `create_agent`，四份 `templates/*/agent.toml` 同步補上 `strategy = "balanced"`。**執行期的 fail-closed 語意一個位元都沒動**：`gvu::trigger::agent_gvu_enabled` 缺鍵／格式錯誤仍讀成 `false`，既有 `agent.toml` 維持原值，`duduclaw agent freeze` 與 `[evolution] enabled = false` 總開關照舊一票否決。ephemeral（單次任務）員工刻意維持 `gvu_enabled = false`。
- **`[evolution]` 四個技能旋鈕接上讀取端（行為變更）**：`max_active_skills`／`skill_synthesis_threshold`／`skill_synthesis_cooldown_hours`／`skill_graduation_min_lift` 先前實際值硬編在 `channel_reply.rs`（`SkillActivationController::new(5)`、`GapAccumulator::new(3, 24)`、`GraduationCriteria::default()`），使用者在儀表板調了不會生效。兩個控制器是 `ChannelContext` 上的行程級單例、旋鈕卻是 per-agent，所以改成在每輪對話的預測區段以 `set_agent_max`／`set_agent_limits` 登記該員工的設定值，沒登記的員工沿用建構子預設——**未設定這些鍵的員工行為逐位不變**。`0` 一律鉗到 1（上限 0 會讓技能一啟用就被淘汰、門檻 0 會在零證據時觸發，兩者都是設定打錯字而非意圖）。
- **`/manage/governance` 改導向新的 `/manage/wiki-trust`（G2／C-9）**：舊路徑與舊的 legacy `/governance` 別名都改成轉址，Wiki 信任成為獨立頁而不再是治理殼底下的一個分頁。側邊欄那一列的標籤與說明改用 `wikiTrust.title` 與新的 `wikiTrust.navDesc`（三語）。**舊書籤照常可用**；企業版閘門與 admin 角色限制不變。
- **`docs/features/05-security-defense.md`（三語）改寫為現況（G1）**：原本描述的「三階段漸進式防禦」（確定性黑名單 → 混淆偵測 → Haiku AI 判讀）與 GREEN／YELLOW／RED 威脅等級狀態機，其腳本已於 `ba015a48` 隨 `.claude/` 移出公開 repo 時刪除，出貨 binary 零讀取端。改寫成現役四道守衛：`duduclaw hook agent-file-guard`（Rust 子命令）、同一 hook 內的 `org_field_guard` 欄位凍結、`duduclaw hook data-file-guard`（RFC-23 §14.4）、`duduclaw_security::input_guard`（**7 類規則**，文件原寫 6 類），並新增「這些守衛擋不住什麼」一節。`docs/architecture/overview.md`、`docs/features/feature-inventory.md`（各三語）與 CLAUDE.md 同步；`commercial/docs/TODO-security-hooks.md` 與 `code-review-security-hooks.md` 檔頭標為 HISTORICAL。
- **`docs/features/08-browser-automation.md`（三語）改寫為三組 MCP 工具（G7）**：移除五層自動路由與不存在的 L4 敘事，改列 L1 `web_fetch_cached`／L2 `web_extract`／L3 外部 Playwright·Browserbase MCP server（可選、不在 binary 內、L2 不會自動降級過去）／L5 七個 `computer_*`，並明講 `browser_via_bash` 已不再設環境旗標。`commercial/docs/TODO-browser-automation.md` 檔頭標為部分 HISTORICAL。
- **`docs/features/11-token-compression.md`（三語）改寫為 Prompt 預算強制（G8）**：v1.33 已移除的「壓縮三刀流」（Meta-Token／LLMLingua-2／StreamingLLM）換成現行 `prompt_compression.rs` 管線的誠實說明：`[budget] max_input_tokens` 預設不啟用、CJK 校準後的 token 估算（1.306 tokens/CJK codepoint）、三個階段（`turn_trim` → `drop_oldest_tool_echoes` → `bisect_and_summarize`）、never-trim 區段、預設**開啟**的快取守衛（arXiv:2607.12161），以及塞不下時回 `BudgetExceeded` 而非默默送出。
- **`docs/features/14-voice-pipeline.md`（三語）改寫（G8）**：SenseVoice、Deepgram、Silero VAD、`symphonia`、LiveKit 五項全 repo 零程式碼零依賴（只有 `.cargo/audit.toml` 一行提到 `livekit-api`，而 `Cargo.lock` 沒有那個 crate），一併移除。改為誠實記錄兩條**分開接線**的路徑：fail-closed 的 `POST /api/stt`／`/api/tts`（`stt.rs` 兩個供應商、`tts.rs` 四個供應商＋`TtsRouter` 三策略），以及 Telegram 的 `transcribe_voice`——後者把供應商與語言**寫死**成 `WhisperMode::Api` ＋ `"zh"`、語音回覆直接 new `EdgeTtsProvider`，所以 `inference.toml [voice]` 的四個鍵目前對 Telegram 路徑毫無作用。這是已知缺口，寫進文件而不是留給操作者踩。
- **`docs/features/03-confidence-router.md`（三語）後端章節改為指向 `53-local-models.md`（G8）**：移除 llama.cpp／mistral.rs／MLX／Exo 四項已刪後端的可用性敘述與「Exo → llamafile」優先序圖，`inference_mode` 的模式清單同步更正。
- **三份已解決的 TODO 移出 `docs/todo/`（G8）**：`TODO-rate-limit-warning-misread-as-failure.md` 與 `TODO-spawn-env-allowlist-fallout.md` 檔頭都寫「✅ fixed 2026-08-17」卻仍留在待辦目錄；`TODO-bootstrap-admin-ws-deadlock.md` 更寫著「confirmed, **not started**」，但修法早在 `bc14b96e` 落地（`must_change_password` 改 RPC 層限制＋強制改密重導，`handlers.rs`／`server.rs` 有回歸測試）。三份加上 ARCHIVED 檔頭搬到 `wiki/reports/resolved-todos/`，`docs/README.md` 索引同步移除，程式碼裡四處指向舊路徑的註解一併更新。
- **多處過時數字與敘述（G8）**：CLAUDE.md 的「~191 MCP tool schemas」改為實測 245（附重算指令）、Architecture Overview 標題從 v1.15.0 改為 v1.65.1、刪除「Python subprocess bridge for skill vetting」（Rust `skill_lifecycle::security_scanner` 零 Python 呼叫端）、LOCOMO `cron_runner` 的「daily 03:00 UTC」改為誠實的「手動 CLI 入口，repo 內沒有任何 crontab／systemd timer／CI／gateway task 會觸發」、「第三份手刻 sender 已移除（終章）」補上 `dispatcher.rs` 的 `forward_to_channel` 仍是第四份、並新增整段缺席已久的 **OS-native 線**（`duduclaw-os` 感知、`duduclaw-sysd`＋`device_ops` 值班機、`codrive` 桌面共駕、shell／comp／native-gui 四層，對應 `docs/features/33`／`50`／`51`／`52`）。`README.md`／`README.en.md` 的「200+ MCP 工具」四處改為 245（`pyproject.toml` 已由同批次改成 245）。`docs/guides/docker.md` 三語的 `duduclaw cost summary` 範例（`cost` 子命令族已於同批次移除）改為 `duduclaw doctor`。
- **兩處自我矛盾的程式碼註解（G8）**：`duduclaw-memory/src/user_code.rs` 寫「No production path consumes it yet」——但 `user_code_profile` 是活的 MCP 工具（`mcp.rs` `ToolDef` → `handle_user_code_profile`，`memory:read` scope）；真正仍成立的是「gateway 不會自動呼叫 `UserProfile::check`」。`duduclaw-agent/src/skill_hub.rs` 寫 `skills-sh` 是「stub only, excluded from defaults」——它其實在 `DEFAULT_HUB_IDS` 裡，每次聚合搜尋都會打它，只是沒有 token 會回 401，走 `[unreachable: <hub>: …]` 回報。兩處都改成誠實敘述。
- **`duduclaw compat` 子命令族改為 cargo feature `app-compat`，平台二進位預設不含（行為變更）**：稽核表原本把它列為「runner 只回報不執行」的死碼，查證後推翻——DuDuClaw OS 的殼在 `crates/duduclaw-shell/src/apps/windows_vm.rs` 直接 spawn `duduclaw compat windows-vm app <exe>`，OS recipe 的 `duduclaw-compat-runners/files/windows-vm.toml` 也把 `entrypoint` 指向同一條命令，firstboot 與 data-binds 另外依賴 `app-add`；而 OS 映像裡的 `duduclaw` 二進位就是主 repo 的原始碼快照建的，刪掉等於讓 OS 的 Windows RemoteApp 啟動失效。所以**不刪，改成預設關的 feature**：`duduclaw-cli` 的 `app-compat`（轉發 `duduclaw-core/app-compat`，後者閘住 `compat_runners.rs`）同時閘住 `compat_cmd`／`compat_windows_vm` 兩個模組、`OpsCommands::Compat` 變體、`CompatCommands`／`CompatWindowsVmCommands` 兩個 enum 與其 match 臂。**平台安裝看不到 `duduclaw compat`**（clap 會回 unknown subcommand），其他 113 個頂層子命令逐位不變；OS recipe（`meta-duduclaw/recipes-duduclaw/duduclaw-cli/*.bb`）以 `--features app-compat` 建置。主 repo CI 新增一步 `cargo check -p duduclaw-cli --features app-compat`，理由與既有的 otel 檢查相同：沒有任何平台建置會編到它，這一步是防 OS-only 表面在兩次烤製之間悄悄腐爛。
- **`os_*` 裝置／系統能力收斂成單一權威 `os_ops.rs`（純內部重構，三個前門逐位不變）**：同一組能力先前實作了三次——agent 面的 `os_*` MCP 工具（`duduclaw-cli::mcp_os_ops`）、操作者的 `duduclaw os <群組> <動詞>` CLI（`duduclaw-cli::os_drive`）、儀表板的 `device.*`／`network.*` RPC（`duduclaw-gateway::handlers`）。三份 `update-check` 組合讀取、三份 appliance／confirm 拒絕字串、兩份 Wi-Fi 稽核寫入、兩份 doctor 檢查列建構，改一處就要記得改另外兩處。現在效果與正規 payload 各只有一份，落在新的 `crates/duduclaw-gateway/src/os_ops.rs`：一個能力一個 `pub async fn`，輸入型別化（`PowerAction`／`WifiAudit`／`CheckUpdateOptions`），錯誤統一成 `OsOpError`（刻意保留原始的 `DeviceOpError`／`WifiError`，因為三個前門對同一個失敗的渲染方式本來就不同）。**權限閘一個位元都沒動**，仍各自留在各自的前門：MCP 的 `Scope::OsNative`／`Scope::Admin` 與 `[capabilities] os_native`、RPC 的 `require_admin!()`／`require_appliance!()`／`require_confirm!()`、CLI 的操作者終端身分與 `os_drive::approval::gate`。工具名、命令名、RPC 名、成功與失敗的 JSON 形狀與字串全部逐位不變——兩處前門原本就不同的字（CLI 的 `序列化失敗：{e}` 對上 MCP／RPC 的 `<what> serialize failed: {e}`、`os_check_update` 詳版與 CLI 簡版的 `system` 欄位集、兩句只差一個子句的非 appliance 提示）改成呼叫端具名的選項（`OsOpError::Serialize` 只帶 serde 細節、`CheckUpdateOptions`），而不是第二份實作。**刻意不收斂**的三處在 `os_ops.rs` 的模組說明裡寫明理由：`os_notify`／`os_open`／`os_frontmost`／`os_spotlight_search`／`os_calendar_today`／`os_watch_status`（只有一個前門，權威本來就在 `duduclaw-os` crate）、`os.status` 等 OS 頁 RPC（只有儀表板）、`system.apply_update` 與 `os_apply_update(target="system")`（前者信任 gateway 行程記憶體裡的 `pending_update` 快取，後者跨行程讀不到而改為當場重解析一組可信 URL——兩套機制、同一條不變式，不是重複）。
- **付費包的授權判定收斂成一處，並補上 `expert install` 這個缺口（行為變更，T5／O2）**：「這是不是付費內容」先前由四段程式各自從目錄路徑推導（CLI wizard 的 `premium_unlocked()`、儀表板的 `premium_templates_unlocked()`、`preset install-builtin`、`experts.install_builtin`），而 `duduclaw expert install` **完全沒有這道閘**——白牌／OEM 散發包會把 `templates-premium/` 放在執行檔旁邊，所以未授權的使用者可以直接 `duduclaw expert install ./templates-premium/experts/<slug>` 繞過。現在包自己帶 `tier`（canonical `pack.toml` 明寫，或由「這份檔案位於已解析的 `templates-premium/` 樹內」推定，路徑比對用 `canonicalize` 錨定而非子字串），只有一個判斷式讀它，並套用在統一的安裝前門上。讀不出來的 `tier` 一律當付費（fail-closed）。**受影響**：沒有 `premium_templates` 授權時，從付費樹內安裝任何包會被明確拒絕而不是靜默成功；公開 repo 內的包、自製包、Claude Code plugin 與單一 Agent Skill 的 `tier` 都是 `free`，行為逐位不變。儀表板的一鍵安裝本來就先查授權，這道閘對它是冗餘的第二層。

- **goal loop 八個小模組併成三個、`org_field_guard` 規則表資料化、MCP 認證的預留抽象移除（T5／O8＋O9＋O14，純內部重構）**：三處都是對外行為、config 鍵與工具名逐位不變的收斂。**O8**：`goal_state`／`goal_visit_graph`／`goal_gap_fingerprint`／`goal_budget_best_round`／`goal_bail_detect`／`goal_tool_streak`／`goal_plan`／`pause_reason` 八個 crate 根模組依職責併入 `goal_loop/{signals,state,plan}.rs`（訊號抽取／任務狀態／計畫），舊的 `crate::goal_*`／`crate::pause_reason` 路徑以 re-export 再保留一個版本，約 120 個呼叫端一個字都沒改。**O9**：`org_field_guard.rs`（2,374 行）拆成 `org_field_guard/{mod,rules,matcher,tests}.rs`，三份手寫的 `diff_*` 函式收斂成一張 `FROZEN_FIELDS` 表加一個通用比對器——凍結一個新欄位從此是加一列資料，不是加一個函式再加一個 `if`；82 個既有測試原樣保留當 golden，另加兩個直接盯住表本身的回歸測試（資料化最怕的是表悄悄變短，那不會有任何行為測試發現）。**O14**：`mcp_auth_strategy.rs`（679 行）是一個**從未被任何生產程式碼建構**的 Strategy Pattern——`dyn AuthStrategy` trait、兩個永遠回 `InvalidFormat` 的 P2 佔位策略、只有一個實作的 `KeyRotationPolicy` trait、以及把兩者各裝一個 Box 再轉發的 `McpAuthMiddleware`。抽象移除，它真正承載的行為（憑證／環境變數選擇、30 天輪換窗、整個 registry 的輪換掃描）以普通函式與常數留在 `mcp_auth/strategy.rs`；`mcp_auth.rs`（2,110 行）同時拆成 `mcp_auth/{mod,scope,grants,strategy}.rs`，**25 個 `Scope` 變體與整張 `tool_requires_scope` 表逐字照搬**（`scope_enum_matches_canonical_list` 與 `test_catalog_scopes_match_tool_requires_scope` 是護欄）。
- **對外名稱收斂：wiki、建任務、技能搜尋、CLI 動詞（T5／O3＋O4＋O10＋O13）**：同一件事有好幾個入口，模型與使用者都沒有規則可以選。現在各留一個主入口，舊名稱全部保留為可呼叫別名兩個 minor 版本（v1.68.0 移除，對照表在 `docs/guides/deprecations.md`，三語）。**wiki**：`wiki_ls`／`wiki_read`／`wiki_write`／`wiki_search`／`wiki_stats`／`wiki_lint` 新增 `scope: "agent" | "shared"`（預設 `agent`，既有呼叫逐位不變），六個 `shared_wiki_*` 變成落在完全相同 handler 的別名；`.scope.toml` SoT 政策、`wiki_visible_to` 可見度、刪除的作者判定一律原樣。`shared_wiki_delete` **刻意不併**——它沒有 agent-local 對應版本，做成 `wiki_delete scope="agent"` 等於新增一個破壞性能力而不是移除重複。**建任務**：`tasks_create` 新增 `kind: "task" | "goal"` 與 `schedule`（cron 運算式或 RFC3339 時間點）。`kind="goal"` 走的是儀表板 `tasks.goal_create` 抽出來的同一個 `goal_create_core`，H9-G 驗收契約凍結、結構化 outcome 解析、每目標時鐘與風險邊界、I-1c `plan_first` 全部共用，不再有第二份會漂移的拷貝；`schedule` 是 cron 就交給 `schedule_task` 原本那條路，是時間點就交給一次性提醒（cron 列表達不出「只跑一次」，把日期釘進 cron 欄位會每年再放一次）。`kind="goal"` 與 `schedule` 併用整包拒絕。**指派授權檢查（部門 × 階層）移到合併後的單一入口強制一次**，呼叫端不能再挑四個舊工具裡檢查最鬆的那個洗過去。`goals_create` 與 `create_task` **都不棄用**——前者建的是目標階層節點（Initiative → Project → Issue），後者是把一份明確列出 `steps` 的多步驟計畫交給 TaskSpec 派工器，`tasks_create` 沒有對應參數，標棄用等於承諾一個不存在的替代品；兩者說明文字改為明講自己是什麼、並指向 `tasks_create` 處理它們不負責的情況。**技能搜尋**：`skill_search` 新增 `source: "all" | "github" | "hub" | "bank"`（預設 `all`：hub 與學會的 skill bank 一起查、依技能名稱去重、標來源），`skill_bank_search` 變別名。skill bank 目前仍是空的 in-memory stub，`source="bank"` 誠實回報空結果，不拿 hub 結果充數。**CLI**：`duduclaw migrate [schema|from|data]` 收掉三個語意互不相干的 `migrate`／`migrate-from`／`data-migrate`；`duduclaw export [data|audit|gdpr|playbook]` 收掉四個語意完全不同的匯出；`duduclaw acp [client|server]` 收掉靠 doc comment 免責的 `acp` vs `acp-server`。裸寫法（`duduclaw migrate`／`duduclaw export --out`／`duduclaw acp`）一律維持原意，舊名稱改為 clap `hide = true` 的別名，`--help` 看不到但照樣解析。`duduclaw tooling wizard` 補上說明文字（先前 `--help` 那一行是空白）。
- **四份同形通知模組收斂成一個推播入口（T5／O5）**：`goal_notify`／`approval_notify`／`install_notify`／`autopilot_notify` 各自寫了一份「解 token → 送卡片 → 記下第一個成功的目的地」迴圈，四份的 doc comment 還互相承認同形（「mirroring `install_notify.rs`」「the same situation `autopilot_notify` is in」）。現在收斂成 `notify_push::push(card, dest)` 一個入口：`dest` 只分兩種 token 方言——送給 **AI 員工**的卡片走該員工自己的 `[channels.<ch>]` 加 `reports_to` 繼承，送給**儀表板真人**的卡片走部署層私訊 bot token 並逐一嘗試。四個模組保留原本的 `pub fn`，只負責決定「卡片寫什麼」與「送到哪裡」。**授權邏輯一行未改**（`decision_notify::authorize_press` 與各模組的 `delivered_targets`）：推播是對外副作用，不該有機會放寬誰能按下按鈕。同時把 `send_with_markup`（Telegram inline keyboard／Slack blocks／Discord DM 開房／LINE quickReply）與 `send_plain_text` 從 `goal_notify.rs` 搬進 `channel_sender.rs`——它們從來就不是 goal 專屬，`decision_notify`／`install_notify`／`takeover`／`channel_alerts`／`notify_governance`／`decision_card` 六個模組都得 import goal 模組才拿得到。卡片內容、按鈕 action id、審計事件、勿擾時段行為逐位不變。`decision_notify::deliver` 這個布林包裝隨之移除（四個呼叫端都改讀 `Receipt`）。
- **三層備援收成一層模組樹（T5／O1）**：「失敗後換誰」原本散在四處，其中 `duduclaw-llm::FallbackRouter` 已先移除（見 Removed）。剩下的模型級 `llm_fallback.rs` 併進 `failover.rs`，成為 `failover::model`，與 runtime 級的 `FailoverManager` 並排；檔頭用一張表寫死三層邊界（帳號 → 模型 → runtime）與各自的觸發條件，不再靠散落的註解維持。`claude_runner` 的三個呼叫點原本各自手寫 `is_llm_fallback_error(e) && should_attempt_model_fallback(a, b)`，改走單一決策 `FailoverManager::model_fallback_for(primary, fallback, error)`。分類規則、審計事件名（`llm_fallback_triggered`）、錯誤訊息格式全部不變；原本的 35 個單元測試搬進 `tests/failover_model_test.rs` 並補 3 個「facade 必須與兩個述詞逐輸入一致」的回歸測試。
- **兩份 openai-compat client 併成一份（T5／O11）**：`duduclaw-inference` 自己維護了一份 reqwest chat/completions client（514 行），與 `duduclaw-llm/providers/openai_compat.rs`（908 行）做同一件事；它活下來的唯一理由是 UCCI 需要的 per-token logprobs 擷取，而共用 provider 沒有。現在 `ChatRequest` 加上 `top_p`／`stop`／`logprobs`／`top_logprobs` 四個可選欄位（未設定時請求 body 逐位不變），provider 新增 `complete_with_logprobs()` 把 token logprobs 與正規化回覆並排回傳，本地推論後端改成它的薄殼。兩項地端專屬設定顯式帶過去而不是繼承：300 秒請求逾時（CPU 生成常超過共用的 120 秒），以及**逐字模型 id**——本機 server 的 `qwen/qwen3-4b` 是名字不是 `provider/model` 限定詞，沿用共用的切分規則會送出伺服器根本沒有的模型名。UCCI 的邊際不確定度計算**刻意留在 inference 側**（共用 provider 負責運送訊號，不負責評分），`ucci_fit.py`／`ucci_pair.py` 的輸入格式不變。兩處可察覺的線路差異已記錄：請求 body 現在明寫 `"stream": false`（原本是省略），`top_p` 經 `serde_json::Value` 由 f32 拓寬為 f64（`temperature` 在這個介面上一直如此）。

- **關掉 `[decision] enabled` 現在連儀表板頁與導覽一起收起來（X1）**：`config.toml [decision] enabled = false` 本來只讓 `/api/decision/*` 全部回 404，SPA 卻照舊渲染決策實驗室的側欄列、進階設定索引、⌘K 指令與 `/app/system` 卡片——操作者明明關掉了，看到的仍是一個點進去只會失敗的入口。`system.status` 新增 `decision_enabled` 欄位（直接讀同一份 `DecisionConfig::from_home`），四處入口一併消失；路由**刻意保留可達**，改成一張「這台主機的設定把它關掉了」的說明卡並指名那個鍵（要開回來是主機端改設定加重啟，不是頁面上按得到的動作），書籤點進來看到的是這句話，不是一個看起來像壞掉的重導。舊版 gateway 沒有這個欄位時一律視為開啟（fail-open：缺欄位不該讓活著的功能消失），真正的閘仍然是那道 404 中介層。三語文案同步。
- **Team 灰帶的能力差訊號接上 `role_model_matrix.toml`（③ capability gap）**：`build_gate_inputs` 原本把 `capability_gap_pp` 寫死 `None`，四個訊號只有 `bulk`／`long_horizon` 量得到，灰帶不可能湊到成團所需的三個。現在同一份 H11 已經在讀的矩陣也回答這個訊號：取執行角色 `(role, runtime)` 下**最佳 resolved 模型**與**該角色今天實際跑的模型**（明寫的 `[team.roles.executor] model`，否則員工的 `[model] preferred`）之 n 加權平均差，單位百分點；勝出模型若會離開凍結的模型家族就不算（與 P6 先驗同一條規則），兩邊都必須有 resolved 格，否則維持 `None`。判斷噪音的 MDE 與差值**同檔成對取得**（矩陣 header 的 `declared_mde`）——差值配另一份矩陣的 MDE 會是兩份檔案都沒做過的宣稱。**出貨矩陣每一格都是 `unresolved`，所以預設部署行為逐位不變**：要等操作者真的量出一份能解析的矩陣，這個訊號才會亮。閘的門檻與 `is_team()` 判定未動。

### Deprecated
- **三種舊包格式與 `duduclaw expert install`／`expert list` 舊名，v1.68.0 移除（T5／O2）**：`expert.toml`、`team.toml`、`preset.toml` 三種 manifest 方言仍被原樣讀取兩個 minor 版本，屆時只保留 `pack.toml`。`duduclaw expert install` 與 `expert list` 即日起是 `duduclaw pack install`／`pack list` 的別名（同一條程式路徑、同樣的輸出），製作端動詞（`pack`／`publish`／`export`／`convert-teams`／`hooks`／`remove`）不在棄用範圍、仍留在 `duduclaw expert` 之下。遷移工具是 `duduclaw pack inspect <dir> --emit-canonical`：它印出對應的 `pack.toml` 供人審閱，不會動你的檔案。儀表板的 `experts.install`／`experts.install_builtin` RPC 與 MCP 工具名皆未更動。
- **`shared_wiki_*` 六個工具、`schedule_task`、`skill_bank_search`，v1.68.0 移除（T5／O3＋O4＋O13）**：分別由 `wiki_* scope="shared"`、`tasks_create schedule="<cron>"`、`skill_search source="bank"` 取代。舊工具**仍列在 `tools/list`、仍可呼叫、行為逐位不變**——MCP 的 tools/list 是宣告面，把工具藏起來會讓它不可呼叫，那跟棄用緩衝期的用意正好相反。辨識方式：`description` 開頭加 `[deprecated → <新工具> <參數>]`（可 grep），`duduclaw_core::tool_catalog` 對應條目標 `deprecated: true`，儀表板的工具挑選器不再把它們當新選項提供。
- **CLI 舊寫法 `duduclaw migrate-from`／`data-migrate`／`audit`／`gdpr export`／`playbook export`／`acp-server`，v1.68.0 移除（T5／O10）**：改用 `duduclaw migrate from`／`migrate data`／`export audit`／`export gdpr`／`export playbook`／`acp server`。舊寫法改為 clap `hide = true`，`--help` 不再列出但照樣解析，行為不變。
- **`[dispatch] judge` 的 `evaluator_only` 與 `human_only`，v1.68.0 移除（T5／O12）**：判官 seam 的四個模式裡只有 `mav`（預設）與 `external` 有人用。`evaluator_only` 的省成本動機已被預設開啟的 `[dispatch] two_stage_judge` 涵蓋（先跑便宜的 evaluator，只有完成候選才付判官團的錢），改用 `mav`；`human_only` 改用 `mav` ＋ 每 agent 的 `[capabilities] autonomy_level`／`approval_required_tools`，該等人的地方等人，不必整個平台停掉機器裁決。**四個值仍然全部解析得到**，已設定棄用模式的部署行為完全不變，只會每個行程記一次警告；經 `system.update_config` 寫入時另記一筆 `judge_mode_deprecated` 審計事件。儀表板只提供 `mav`／`external`，但已存的舊值仍顯示並標「已棄用」，不會被偷偷換掉。
### Removed
- **`duduclaw-cli::mcp_auth_strategy` 模組（T5／O14）**：`AuthStrategy` trait、`ApiKeyAuthStrategy`／`JwtAuthStrategy`／`OAuth2AuthStrategy`、`KeyRotationPolicy` trait、`ThirtyDayRotationPolicy` 與 `McpAuthMiddleware` 全數移除——workspace 內零建構點，兩個 P2 佔位策略對每一個請求都回 `InvalidFormat`。取代者是 `mcp_auth::strategy` 裡的普通函式與常數（`authenticate`／`rotation_status`／`is_rotation_due`／`check_any_key_rotation_due`／`MAX_KEY_AGE_DAYS`／`ROTATION_WARN_DAYS`／`STRATEGY_NAME`），行為與原本那條唯一活著的 API key 路徑相同。
- **Python 套件的 `duduclaw.channels` 與 `duduclaw.sdk`**：兩個子模組與 Rust 端完全脫鉤（`crates/` 內 grep `Command::new("python")`／`duduclaw.channels`／`duduclaw.sdk` 全部零命中），功能分別由 Rust 的十一通道實作與 `account_rotator` 取代，最後實質變更停在 2026-03-19／2026-04-15。整包移除，`python/README.md` 的模組清單同步改寫。
- **`duduclaw.memory_eval` 移出發行的 wheel 與 sdist**：7,223 行的記憶體品質量表占 PyPI 套件 58%，每一版都隨 wheel 送到使用者手上，卻沒有任何人（含 CI）跑它，而且少了 `aiohttp`／`asyncpg`／`datasets` 三個宣告外相依根本 import 不起來。程式碼留在 repo（沒有刪），`pyproject.toml` 兩個 build target 都加上 `exclude`，改由 `python/README.md` 說明如何從原始碼 checkout 執行。順帶更正一項長年誤述：`data/golden_qa_set.jsonl` 的 200 筆基準全部是手工標註（每列 `source: "manual"`），**不是** LOCOMO 資料，也不是從 LOCOMO 衍生的。
- **`duduclaw-llm::FallbackRouter`（`router.rs`，451 行）**：`FallbackRouter`／`cooldown_for`／三個 COOLDOWN 常數／`CandidateOutcome` 在 crate 外零呼叫端，只出現在 `lib.rs` 的 `pub use`。平台實際在跑的「失敗後換誰」是 `gateway/failover.rs`（runtime 級的 `FailoverManager` 與模型級的 `failover::model`，O1 已併成一層）與帳號輪替器的冷卻；這第四套從未接線。`lib.rs` 的 re-export、doc 行與 8 個單元測試一併移除。無 config 鍵、無 MCP 工具、無其他 crate 相依。
- **MLX bridge（`duduclaw-inference/src/mlx_bridge.rs`，200 行）**：`MlxBridge::generate()` 全 repo 零呼叫端，唯一活路徑是 `inference_status` 的一行「MLX bridge: available」顯示；文件宣稱的「在 Apple Silicon 上本地跑演化反思」在程式碼裡沒有對應路徑。一併移除 `inference.toml [mlx]` 區段、`InferenceConfig.mlx` 欄位、`InferenceEngine::mlx_available()`、`inference.update` 的 `mlx` pass-through、儀表板「本地推論」頁的 mlx 設定卡。想在 Apple Silicon 上跑本地模型，改用 `[openai_compat]` 指向本機 llama-server／Ollama。
- **行程內 llama.cpp backend（`llama_cpp.rs`）與 `llama-cpp-2` 相依**：`generate()` 直接回「not yet fully implemented，請改用 openai_compat 或 mistral_rs」，而且它被 `metal`/`cuda`/`vulkan` feature 閘住，`release.sh` 從未帶過這三個 feature——出貨的 binary 從來沒有編譯過它。`Cargo.toml` 的 `llama-cpp-2` 相依與 `metal`／`cuda`／`vulkan` 三個 feature（含 `duduclaw-gateway` 的同名轉發 feature）一併移除。
- **mistral.rs backend（`mistral_rs.rs`）與 `mistralrs-core`／`indexmap`／`either` 三個相依**：feature 預設關、release 腳本不帶，等同從未出貨。`mistralrs`／`mistralrs-metal`／`mistralrs-cuda`／`mistralrs-flash-attn` 四個 feature（含 gateway 的兩個轉發 feature）、`inference.toml [mistralrs]` 區段與 `MistralRsConfig`／`SpeculativeConfig`／`SpeculativeMethod`、`inference.update` 的 `mistralrs` pass-through、儀表板對應設定卡全部移除。**相容性**：`BackendType::LlamaCpp`／`MistralRs` 兩個列舉值**刻意保留**，舊的 `inference.toml` 仍能解析；選到它們時回一個明確的 `BackendUnavailable`，訊息指向 `openai_compat`，而不是解析失敗。硬體偵測的 `recommended_backend` 一律改回 `openai_compat`。
- **Exo P2P 叢集客戶端（`exo_cluster.rs`）**：2026-04 起未動，repo 內沒有任何範例 `inference.toml` 示範 `[exo]`，等於使用者無法抵達；而把 `[openai_compat] base_url` 指向 Exo 端點就能達成同一件事。`InferenceMode::ExoCluster`、`ManagerStatus.exo_available`、`InferenceManager::exo()`、`inference_mode` MCP 工具的 Exo 狀態行、`inference.update` 的 `exo` pass-through 與儀表板設定卡一併移除。
- **JitRL 零梯度續學（`duduclaw-inference/src/jitrl/` 五模組，1,249 行）與 MCP 工具 `jitrl_feedback`**：`[jitrl] enabled` 預設 false，且要有人手動呼叫 `jitrl_feedback` 才會產生樣本，唯一有真實偏置面的是 openai-compat 那一層（llama.cpp 那層本來就是 stub），零文件、零儀表板、無使用痕跡。一併移除 `inference.toml [jitrl]` 區段、`InferenceEngine` 的注入與 record 兩條路徑、`~/.duduclaw/jitrl_experience.jsonl` 的寫入端、`mcp_auth` scope 表與 `tool_catalog` 目錄中的條目，以及只為它存在的 `GenerationParams.logit_bias` 與 openai-compat 請求的 `logit_bias` 欄位（無人寫入後該欄位恆為 `None`）。
- **本地路由的 legacy post-hoc 校準閘（`[router] post_hoc_enabled`／`post_hoc_alpha`／`post_hoc_beta`／`post_hoc_accept_threshold`）**：四個旋鈕的預設值**從未擬合**——α 4.0／β −2.0／門檻 0.5 讓 `g = sigmoid(α·p̄+β) ≥ 0.5` 恰好等價於「平均 logprob ≥ ln 0.5」，也就是一個穿著邏輯斯迴歸外衣的固定切點；而用來擬合它的 `assess_response()` 在 gateway 端零呼叫端，`(p̄, g, accepted)` 從來沒有跟結果標籤一起落地過。**UCCI（`inference.toml [router] ucci_*`）成為唯一的校準閘**，它有離線擬合腳本與論文依據；沒有掛 UCCI router 檔的層直接接受本地答案（與先前 post-hoc 關閉時的行為相同）。`ConfidenceRouter::evaluate_post_hoc`／`assess_post_hoc`／`PostHocAssessment`、`RoutingDecision.post_hoc` 欄位與 `InferenceEngine::assess_response`／`post_hoc_enabled` 一併移除；舊 `inference.toml` 留著這四個鍵不會報錯，只是被忽略。三層路由骨架（LocalFast／LocalStrong／CloudApi）完全保留。
- **Code Mode Phase 0 量測閘（`tool_loop_probe.rs` 977 行 ＋ `duduclaw cost tool-loop` 子命令）**：一次性決策工具，本機量測已產出 `INSUFFICIENT_DATA`，而 Code Mode 本身至今未立案。三個觀測注入點（openai-compat runtime／direct API／本地推論的工具迴圈）一併拆除，工具迴圈本身逐位不變；既有的成本遙測（`cost_telemetry.db`）不受影響。`CostCommands` enum 只有這一個葉命令，故 `duduclaw cost` 整個子命令族一併移除。設計文件 `commercial/docs/DESIGN-code-mode-2026-08.md` 檔頭已標 HISTORICAL。
- **`duduclaw-cli-runtime::Supervisor`／`RestartPolicy`（`supervisor.rs`）**：crate 外零呼叫端；檔頭自承「Phase 1 ships the minimal contract; Phase 2 wires it into the pool's eviction logic」，Phase 2 一年未到。`unhealthy()`／`evict_where()`／`len()`／`is_empty()` 沒有任何呼叫端，`pool.rs` 只做 `track`／`untrack` 的純寫入記帳，沒有任何地方讀回——移除後 pool 行為逐位不變。
- **`gateway/webhook.rs`（280 行）**：`webhook_router()`／`WebhookState` 零引用，`server.rs` 從未 mount `/webhook/{agent}`。現役的 webhook 入口是各通道自己的路由（LINE／WhatsApp／Feishu／Google Chat／Teams／WeCom／DingTalk）。
- **`gateway/activation.rs`（158 行）**：`lib.rs` 從未宣告這個 `mod`，整檔從未被編譯過，連同 8 個永遠跑不到的測試一併移除。
- **`gateway/delegation_scope.rs`（192 行）**：三個公開符號在 crate 外零引用，7 個測試只測自己。委派授權的唯一權威是 `duduclaw-core/delegation_policy.rs`（六條規則的 `reports_to` 樹謂詞）＋`capability_grants.rs`（任務範圍的工具授權）。
- **`gvu/shadow_mode.rs` 與 `gvu/diversity.rs`（合計 509 行）**：8 個 `pub` 項目零引用、零測試、半年未動，只有 `gvu/mod.rs` 的模組說明還在宣傳它們（該兩行一併刪除）。
- **`prediction/foresight_gate.rs`（304 行）**：檔頭自寫「No consumer wired」，零呼叫端。只被它讀取的 `config.toml [task_forward_model] foresight_tau`／`foresight_recent_k` 兩個欄位一併移除（serde 忽略未知鍵，舊 `config.toml` 照常解析）。目標迴圈現役的早停機制是 `goal_visit_graph.rs` 的重複造訪偵測與 `goal_gap_fingerprint.rs` 的停滯指紋，兩者都還在。
- **`duduclaw-memory/src/search.rs` 與 `embedding::VectorIndex`（合計約 160 行）**：`search.rs` 是整個 crate 唯一沒有 `pub use` 的模組，零呼叫端；`VectorIndex` 同樣零呼叫端。**`cosine_similarity` 保留**（`engine.rs` 的向量重排真的在用它），它原本就住在 `embedding.rs`，位置不變。
- **`duduclaw-security` 五個孤兒模組（`filter_chain`／`template_sanitizer`／`os_reconcile`／`credential_proxy`／`mount_guard`，合計約 1,440 行）與兩個重複檔（`src/mod.rs`／`src/unicode_tests.rs`）**：五組型別名 workspace-wide 零外部命中；後兩檔與 `src/tests/unicode_tests.rs` 位元組相同，且因為 `lib.rs` 從未宣告 `mod mod`，它們從來沒有被編譯過。現役的對應機制分別是 `input_guard`（注入掃描）、`secret_ref`／`secret_manager`（憑證）、沙箱本身（掛載限制）與 `posture_watch`（安全姿態）。`duduclaw-os` 不依賴 `duduclaw-security`（已 grep 確認），故 `os_reconcile` 直接刪除而非搬移。
- **LINE OA B2C 點數計費（`gateway/credit.rs` 228 行）與多官方帳號路由**：帳本、費率換算與 `duduclaw ops credit grant/balance/history` 三個子命令都寫好了，但計量的另外一半從未接線——`CreditLedger` 全 repo 唯一呼叫端就是那支 CLI 自己，模組文件承諾的「餘額 ≤ 0 就在呼叫 LLM 前擋下」在回覆路徑零呼叫端，`LineAccount::resolve_accounts()` 零呼叫端，`line.rs` 內連 `destination` 字串都零命中。換句話說它是一本手動記帳本，賣出去會超額服務。整組移除：`credit.rs`＋`credits.db` 讀寫、`CreditCommands` 三個子命令、`duduclaw_core::types` 的 `LineAccount`／`LineChannelConfig::accounts`／`resolve_accounts()`，以及 `docs/guides/line-oa-b2c.md`（三語）。**相容性**：`LineChannelConfig` 沒有 `deny_unknown_fields`，既有 `config.toml` 留著 `[[channels.line.accounts]]` 不會報錯，只是被忽略，單一官方帳號的 `channel_token`／`channel_secret` 路徑完全不受影響（已加回歸測試）。若日後要恢復 B2C 轉售計價，正確做法是接到既有的 `budget.rs`／license 配額層，而不是再維護第二套點數帳本。
- **PTY session 連線池、worker 子行程與整套周邊（`duduclaw-cli-worker` crate、`worker_supervisor.rs`、`runtime_status.rs`、`pty_default_migration.rs`、`duduclaw-cli-runtime` 的 `pool`／`session`／`envelope`／`progress` 模組，合計約 8,000 行）**：它存在的理由是「萬一 Anthropic 封鎖 OAuth 訂閱帳號的 `claude -p`，翻一個旗標就能繼續運作」。那個變更排在 2026-06-15，**當天就暫停**，十五個月後 `claude -p` 對 OAuth 訂閱仍然可用；而連線池的 session key 是 `(agent, cli_kind, bare_mode, account, model)`、**沒有對話維度**，同一個 agent 的兩個 WebChat 對話共用一個活 REPL 並互相看得到工作狀態——`docs/features/27` 自己就用「啟用前請先讀這段」寫著這件事。一個沒人能安全打開的備援，維護成本每次跨回覆路徑的重構都要付一次。一併移除：`RuntimeMode::PtyPool` 與 `channel_reply`／`claude_runner` 的 PTY 分支、`agent.toml [runtime] pty_pool_enabled`／`worker_managed`／`pty_idle_timeout_secs`／`pty_interactive_timeout_secs` 四個鍵與儀表板開關（三語 i18n 同刪）、`DUDUCLAW_DISABLE_PTY_POOL` kill switch、`GET /api/runtime/status`、`pty_pool_*` 與 `worker_*` 全部 Prometheus 指標、`scripts/smoke-pty-pool.{sh,ps1}`、一次性遷移 `wp10-pty-default-reset` 與它專用的 `runtime.migrated` 儀表板事件。**保留** `oneshot_pty_invoke`／`pty_runtime::invoke_oneshot`（Grok runtime 與 CLI 登入輔助仍在用，它們的 CLI 堅持要真 TTY）與 `strip_ansi`。**相容性**：`agent.toml` 留著那四個鍵不會報錯（未知鍵一律容忍），只是被忽略。設計文件 `commercial/docs/runtime-pty-pool-design.md` 與 `TODO-cli-pty-pool-worker.md` 檔頭已標 HISTORICAL；要重做的話請從第一個 commit 就把對話維度放進 session 身分。
- **`crates/duduclaw-docuseal-mcp`（727 行）**：DocuSeal 簽署工作流的第一方 stdio wrapper，單一 commit（2026-07-30）後未動、`scripts/release.sh` 從不建它（使用者得自己 `cargo build`）、workspace 零引用，而 DocuSeal 自 2026-03 起就有官方 MCP server。整個 crate 與 workspace member 移除；`docs/guides/docuseal.md`（三語）改寫成「用 `[[mcp.external]]` 掛官方 server，cloud 租戶走 REST API」，`docs/guides/mcp-bridge.md` 的 DocuSeal 條目同步更正（原本寫「尚無 server，請自己做一個」，這在 2026-03 之後就不成立了）。
- **`duduclaw export --format agentcompanies`（`export_to.rs` 1,328 行、13 測試）**：對 paperclip 生態的單向 v1-draft 匯出，但 paperclip 路線 2026-07 已轉辦公協作、再轉 Agent-Native OS，沒有已知消費者。`--format`／`--agent`／`--all`／`--json` 四個旗標隨之移除，`duduclaw export` 回到單一用途（個人版 `.tar.gz`）。**匯入方向不受影響**：`duduclaw migrate-from paperclip` 仍能讀 agentcompanies/v1 套件。
- **Git worktree L0 隔離（`gateway/worktree.rs` 1,151 行）**：`[container] worktree_enabled` 預設關、零個出貨板模啟用、2026-06-22 後未動；而目標迴圈的主線走 `message_queue`，根本不經過 dispatcher 的 worktree 分支。一併移除 `[container] worktree_enabled`／`worktree_auto_merge`／`worktree_cleanup_on_exit`／`worktree_copy_files` 四個鍵與 AI 員工設定頁的四個控制項（三語 i18n 同刪）、`dispatcher.rs` 的 L0 分支與 `WORKTREE_PATH` task-local、`docs/features/18-worktree-isolation.md`（三語）與索引。並行隔離現役的機制是 L1 容器沙箱（`[container] sandbox_enabled`）。**相容性**：`ContainerConfig` 沒有 `deny_unknown_fields`，既有 `agent.toml` 留著這四個鍵不會報錯。
- **`duduclaw auth device --provider qwen`**：Qwen 於 2026-04-15 停掉免費 OAuth 並從 qwen-code 的登入對話移除該流程，模組自己標記 PENDING-LIVE（端點抄自開源碼、無法實機驗證）。Qwen 的 `DeviceFlowConfig`、JSON bundle 憑證格式、seat 模型清單與只為它存在的 PKCE 機制（`generate_pkce`、`uses_pkce`／`verified` 兩個欄位）一併移除。**Copilot 座位完全保留**，它是實機驗證過的。
- **`skill_lifecycle` 四個死模組（`curiosity.rs`／`dependency_resolver.rs`／`reconstruction.rs`／`recommender.rs`，合計 1,165 行）**：13 個 `pub` 項目 workspace-wide 零命中；`reconstruction::reconstruct_skill` **連自己的測試都沒呼叫過**，而 `docs/features/15` 把它當成「Stage 4 重建」在賣。功能文件改寫成六階段並明說 Stage 4 為什麼不見了（三語），Stage 2（壓縮）與 Stage 5（診斷）的敘述同時對齊實作——壓縮做的是三層漸進式載入不是技能去重，診斷師讀的是預測誤差不是替技能打分數。**`vetting.rs` 刻意保留**：它在 `sandbox_trial::graduate_skill_to_disk` 有真呼叫端，是寫入磁碟前的第二道安全閘。
- **`metrics.rs` 六條從未遞增的 Prometheus 序列**：`duduclaw_requests_total`／`duduclaw_tokens_total`／`duduclaw_request_duration_seconds`／`duduclaw_active_sessions`／`duduclaw_channel_connected`／`duduclaw_budget_remaining_cents` 每次 scrape 都渲染，但生產程式碼沒有任何遞增端——`record_request()` 只有自己的測試在呼叫，`update_budgets()` 連測試都沒有。建在它們上面的 Grafana panel 顯示恆為零，那不是「系統閒置」而是「這個數字是假的」。序列本體與 `record_request`／`update_channels`／`update_budgets` 三個 setter 一併移除，`docs/guides/deployment-guide.md`（三語）的指標表與範例 Grafana dashboard 改成實際會動的序列。逐請求的 token 與成本資料在 `cost_telemetry.db`（`cost_summary`／`cost_agents`／`cost_recent` MCP 工具與儀表板成本頁），通道連線狀態在儀表板通道頁。
- **license `features.toml` 三個零讀取端的 quota 欄位**：`max_local_models`／`max_messages_per_month`／`office_hour_hours_per_month` 在每個 tier 都有值、`gate.rs` 都有 getter，但沒有任何呼叫端讀它們（RFC-27 自己就點名過其中一個）。欄位、getter 與相關測試一併移除。計價真正在跑的上限是 `max_agents`／`max_channels`／`memory_quota_gb`。
- **MCP 工具 `log_mood`**：描述只有四個字「Log user mood」，是 245 個工具裡最短的；語意與 `user_profile_record` 及一般記憶寫入重疊，等於用一個獨立工具 schema 的固定 token 成本換一條可以直接寫記憶的紀錄。工具定義、handler、`tool_catalog` 目錄條目與 `prediction/tool_class.rs` 的兩處分類一併移除。
- **一次性／零文件 CLI：`duduclaw ops tunnel`、`duduclaw ops memory bench`、`duduclaw rl export/stats/reward`**：`tunnel` 是 Cloudflare quick-tunnel 精靈（正式路徑在 `docs/guides/deployment-guide.md`，不需要它）；`memory bench` 是 LightRAG 分割決策的一次性量表；`rl` 三葉在 `docs/features` 與 `docs/guides` 零條目，`duduclaw docs` 的主題表也打不到，使用者不可能知道它存在。三組合計移除 `tunnel.rs`(134)＋`cmd_memory_bench`(36)＋`RlCommands` 與 `cmd_rl`／`print_rewards`(154)。`MemoryCommands` 與 `CostCommands` 一樣只剩空殼，`duduclaw ops memory` 整個子命令族一併移除。**軌跡收集本身不受影響**（`channel_reply` 仍寫 `rl_trajectories`），拿掉的只是那三個查詢 CLI。
- **前端孤兒 `_WipPlaceholder.tsx`(35) 與 `ApprovalsPage.tsx`(251)＋其測試**：前者 `grep -rl "WipPlaceholder" web/src` 只命中自己，檔頭自述是 v2 改版期間的「建置中」代打；後者未被任何頁面 import、`/approvals` 早已 `Navigate to="/inbox"`，而且**它自己的測試 describe 字串就寫著 `(MDS, unrouted — …)`**。卡片邏輯已由 `components/console/ApprovalRequestCard.tsx` 完整複製（該檔註解明寫 "duplicated here (not imported)"），刪除前已確認該副本自帶測試且為現役。
- **殘留腳本與檔案**：`scripts/build-release.sh`（49 行、2026-03-15 後未動、全 repo grep 只命中自己，被 `scripts/release.sh` 與 `.github/workflows/release.yml` 完全取代）、`scripts/build.sh`（17 行包裝、零 CI 引用）、`scripts/smoke-pty-pool.{sh,ps1}`（隨 PTY 連線池）、`evals/.DS_Store`。`scripts/smoke-fork.{sh,ps1}` 與 `scripts/smoke-decision-continuity.sh` **保留**（對應功能仍在）。
- **GVU legacy SOUL.md 演化路徑整條移除（約 6,900 行）**：`gvu/generator.rs`／`proposal.rs`／`updater.rs`／`consolidate.rs`／`observation_finalizer.rs` 五個模組、`loop_.rs` 的 SOUL patch 閉環與 `run_consolidation`、`verifier.rs` 的 legacy 驗證鏈（`verify_all`／`verify_all_with_mistakes`／`verify_deterministic`／`verify_metrics`／`verify_mistake_regression`／`verify_canary_compatibility`／`VerificationResult`／`build_judge_prompt`）、`version_store.rs` 的 `SoulVersion`／版本化／24 小時觀察期／自動回滾／deferred retry／consolidation 稽核，以及 `GvuOutcome::Applied`／`Deferred`／`TimedOut` 三個變體，全部移除。**取代者是 AEE**（`gvu/aee/*`，Evolution v3 自 2026-08-06 起的預設路徑，演化對象是 playbook 經驗法則而非人格檔）。刪除的理由是這條路徑守護的寫入面本身已經不存在：`SOUL.md` 自 v3（WP1.1）起對 AI 員工唯讀，MCP `agent_update_soul` 與檔案守衛 hook 都會拒絕 agent 身分的寫入。既有 `evolution.db` 的 `soul_versions`／`gvu_consolidations`／`deferred_gvu` 等資料表不刪、就地保留，只是不再有讀取端；`agent.toml` 留著 `legacy_soul_evolution = true` 會被忽略。**保留**：AEE 全套、`stagnation.rs`、`reward_hack.rs`、`mistake_notebook.rs`、`telemetry.rs`、`champion.rs`，以及 `soul_partition.rs`（`duduclaw playbook migrate-soul` 仍依賴它）。AEE 的條目級定案掃描原本掛在 `ObservationFinalizer` 上，改由新的 `gvu/aee/sweeper.rs` 承接，`server.rs` 的 30 分鐘排程與行為不變。`gvu/generator.rs` 唯一還有外部呼叫端的 `escape_xml_tag`（prompt XML 圍欄，`wiki_ingest` 在用）搬到中性的 `gateway/xml_fence.rs`。
- **`duduclaw evolution finalize` CLI**：唯一用途是手動收尾 SOUL.md 觀察窗，隨上一條一併移除。`duduclaw evolution` 子命令族只剩 `clear-holdout-rotation`（AEE 用，仍在）。
- **儀表板 `evolution.history`／`evolution.versions`／`evolution.consolidations` 三個 RPC 與記憶頁「自主學習」分頁的版本歷史卡、整併紀錄卡**：三者只服務 SOUL.md 版本資料，資料來源移除後恆為空清單。`evolution.status` 的 `total_versions`／`last_applied_at` 兩個欄位改成 `total_rounds`／`applied_rounds`（改讀 AEE 實驗記錄）。停滯偵測卡、駁回分布卡與 playbook 條目卡不受影響。
- **`[evolution]` 八個只寫不讀的技能旋鈕**：`skill_graduation_enabled`／`skill_recommendation_enabled`／`skill_recommendation_threshold`／`curiosity_enabled`／`curiosity_threshold`／`curiosity_max_daily`／`skill_behavior_monitor_enabled`／`skill_behavior_drift_threshold` 由儀表板驗證並寫進 `agent.toml`、在 `types.rs` 有型別，但 gateway 端**零讀取端**——使用者調了會以為生效，其實不會。型別、儀表板表單欄位、`agents.update` 的驗證清單、前端預設值與三語 i18n 鍵一併移除；`AgentConfig` 沒有 `deny_unknown_fields`，既有 `agent.toml` 留著這些鍵仍可正常載入，只是被忽略（那本來就是它們實際的行為）。
- **`[evolution] max_gvu_generations` 與 `observation_period_hours`**：兩者只服務 legacy SOUL 迴圈的輪數上限與觀察期，隨 S11 一併移除，含 MCP `evolution_toggle` 欄位表、`agents.update` 數值鍵清單、AI 員工編輯頁兩個欄位與三語 i18n 鍵。AEE 的對應設定是 `gvu_cooldown_minutes` 與 `aee_settle_hours`。
- **`docs/features/02-gvu-self-play-loop.md` 與 `06-soul-versioning.md`（含 zh-TW／ja-JP 副本）**：兩篇功能文件描述的機制已不存在，留著會主動誤導。索引（`docs/README.md`、`docs/features/README.md` 三語）同步移除；`docs/architecture/evolution-engine.md` 第四、七、八、九章保留為歷史紀錄並在檔頭明講「已非活程式碼」。
- **Governance Layer 的儀表板頁、三個 RPC 與 `policies/*.yaml` 讀寫（G2）**：`duduclaw-governance` crate 已於 `b0639b96` 刪除，**零 enforcer**——操作者在「治理」頁寫下的 rate／permission／quota／lifecycle 政策，執行期沒有任何一處會讀。假的安全感是負價值。移除 `GovernancePage.tsx`／`GovernanceShell.tsx`（含測試）、`governance.list`／`.upsert`／`.remove` 三個 RPC 與 `handlers.rs` 的 `gov_*` YAML emitter／parser／validator（約 600 行含 5 個測試）、企業閘的 `governance` 家族、`api.ts` 的 `GovPolicy` 型別群與 client 方法、三語 `gov.*`／`nav.governance`／`manage.governance`／`governance.tab.*`／`crosslink.governance.rbacMatrix` i18n 鍵、安全頁指向治理頁的 RBAC 交叉連結（它承諾的編輯面什麼都不會改，真正的設定是各 AI 員工的 `[capabilities] approval_required_tools`）、帳務頁的「治理配額」列，以及 `docs/features/21-governance-layer.md` 三語與索引。**實際覆蓋這些職責的是**：`mcp_rate_limit`（速率）、`delegation_policy` ＋ MCP scope 表（權限）、`license_runtime`（配額）。**Wiki 信任分頁保留**並升格為獨立路由 `/manage/wiki-trust`（C-9：舊的 legacy `/governance` 別名只渲染裸 `GovernancePage`、看不到 Wiki 信任，兩條路徑現在都導向同一頁，舊書籤照常可用）；側邊欄那一列改指它。`policies/global.yaml` 留在 repo 未刪，只是不再有讀取端。
- **`docs/features/22-durability-framework.md`（含 zh-TW／ja-JP 副本）＋三語索引與 `feature-inventory` 條目（G3）**：`duduclaw-durability` crate 早已刪除、workspace 零引用，該頁描述的五大支柱（idempotency／retry／circuit_breaker／checkpoint／dlq）沒有對應程式碼。`feature-inventory` 的「Governance Layer」「Quota Manager」兩列一併移除（同 G2）。
- **`crates/duduclaw-gateway/src/browser_router.rs`（438 行）＋`lib.rs` 宣告（G7）**：2026-04 寫成的「五層自動路由器」，**零呼叫端、CHANGELOG 零提及、從未出貨**。它描述的 `L4 Sandbox Browser` 從來不曾以獨立能力存在；它欄位上的 trusted/blocked 網域、每 session 頁數上限、截圖稽核與逐動作人工核准也全都沒有實作（不可逆動作的核准實際走 `ApprovalBroker` ＋ `approval_required_tools`／`irreversible_tools`）。真實出貨的是三組 MCP 工具（L1 `web_fetch_cached`／L2 `web_extract`／L5 七個 `computer_*`）加上可選的 per-agent Playwright／Browserbase MCP server，由 agent 自行選擇。`approval.rs` 與 `codrive/registry.rs` 兩處指向它的註解同步更正。
- **四處懸空的 `DUDUCLAW_BROWSER_VIA_BASH=1` env 設定（G1）**：`channel_reply.rs`（fresh-spawn 與 PTY 兩處）、`claude_runner.rs`、`duduclaw-agent/runner.rs` 仍在 spawn 時設這個旗標，但唯一會讀它的 `bash-gate.sh` 已隨 `.claude/hooks/` 於 `ba015a48` 一併刪除。`[capabilities] browser_via_bash` 本身**維持有效**（餵 `disallowed_tools()` 與 `CapabilitiesConfig::sandbox_level()`），只是不再設一個沒人讀的環境變數。
- **安全頁的「憑證代理」與「掛載守衛」兩張卡＋`security.status` 的對應欄位（G2 同族）**：`duduclaw-security::credential_proxy` 與 `mount_guard` 兩個模組已於同批次刪除（零呼叫端），而這兩塊 RPC 資料量的也不是名字所說的東西——「已注入密鑰數」數的是 **gateway 行程自己**名稱含 `API_KEY`／`TOKEN`／`SECRET` 的環境變數，而 v1.61 的 spawn-env 白名單正是刻意把它們從 agent 子行程擦掉的；「掛載規則」則是**第一個** agent 的容器掛載清單被當成全域政策渲染。兩個 KPI 格、兩張卡、`StatusRow`／`RuleRow` 兩個只服務它們的元件、`api.ts` 型別欄位與九個三語 i18n 鍵（`security.credentialProxy.*`／`security.mountGuard.*`／`security.proxyStatus`／`security.vaultBackend`／`security.injectedSecrets`／`security.active`／`security.inactive`）一併移除。`security.status` 其餘欄位（`rbac`／`rate_limiter`／`soul_drift`）不變。**殘留**：`duduclaw-native-gui` 的安全頁仍渲染這兩張卡（欄位缺失時退成 inactive／零筆，不會 panic），與 native GUI 的治理頁同屬另一批次的收斂範圍。
- **`inference.update` 的 `llmlingua`／`streaming_llm` 兩個死設定區段與對應的儀表板卡（G8）**：它們設定的三策略壓縮器已於 v1.33 從 `duduclaw-inference` 移除（`grep -rn 'llmlingua\|streaming_llm' crates/` 除了這兩處之外零命中），儀表板一直在寫沒有任何讀取端的鍵。`handlers.rs` 的 pass-through 清單只剩 `llamafile`／`embedding`，`api.ts` 的兩個欄位與 `InferencePage.tsx` 的兩張 `BackendSection` 卡一併移除；既有 `inference.toml` 裡留著的區段會被忽略（那本來就是它們實際的行為）。
- **`crates/duduclaw-shell`／`duduclaw-comp`／`duduclaw-native-gui` 三個 crate，連同 `.github/workflows/native-gui-desktop-release.yml` 與 `scripts/desktop/bundle-native-gui-macos.sh`，移至 DuDuClaw-OS repo**：三者在根 `Cargo.toml` 本來就列在 `[workspace] exclude`、各自帶 detached `Cargo.lock`，主 `Cargo.lock` 從未引用它們，所以搬家對本 workspace 的建置與鎖檔零影響（`[workspace] exclude` 只剩 `tools/license-keygen`／`commercial/duduclaw-license`／`src-tauri`）。native-gui 的桌面發布 workflow（tag `native-gui-v*`，從未打過 tag）與它唯一的打包腳本整組跟著走；`scripts/desktop/sign-notarize-macos.sh` 是共用腳本，主 repo 保留供 Tauri app 使用（頭註解改成中性描述），OS repo 另有一份副本。`scripts/release.sh` 的 excluded-crate sibling lock bump 機制**保留不動**，只更新註解說明對象已搬走、`crates/*/Cargo.lock` 目前無命中所以自然不觸發。**前提**：兩個 repo 各自 commit，OS repo 側的 `crates/` 新增、三支 `refresh-src.sh` 路徑推導與 recipe 旗標見該 repo 的 CHANGELOG。
- **`docs/guides/app-compat.md`（英／zh-TW／ja-JP 三語）移至 DuDuClaw-OS repo `docs/guides/`**：整份文件描述的是 OS 映像的 Bottles／Waydroid／Windows VM 相容層與 `compat.d` 登記，跟著它所描述的 crate 與子命令一起走。`docs/README.md` 該列改為一行移轉說明；三語 README、`docs/features/52-desktop-edition.md` 三語、`compat_cmd.rs`／`compat_windows_vm.rs`／`lib.rs` 的 doc 註解與一句 CLI 提示文字全部改指 OS repo。

## [1.65.1] - 2026-09-24 — 資料庫來源授權三路（儀表板×AI 員工頁×聊天）與內部 key 身分修復

### Added
- **資料庫來源授權不用再手改 agent.toml：儀表板兩處＋跟 AI 員工說一句就能開**：哪些 AI 員工可以用哪個資料庫（`agent.toml [capabilities] db_sources`，預設拒絕）以前只能手動編輯檔案。現在三條路都通，且都不需要重啟 gateway（授權是每次工具呼叫時重新讀取的）：① **設定 → 去識別化 → 外部系統與資料來源**：資料庫類型的精靈第 4 步改為「哪些 AI 員工可以使用這個資料庫」勾選清單（原本只顯示寫回「不適用」），已設定的資料庫列上多一個「N 位 AI 員工可用」徽章，點開就能改；② **AI 員工設定頁 → 能力**：新增「可使用的資料庫來源」勾選清單，勾選中但已不存在於設定的來源會標示出來讓你取消，不會被默默丟掉；③ **對 AI 員工說**「把客戶 CRM 資料庫開給小美」：MCP 工具 `agent_update` 新增 `db_sources`（整份取代，空字串＝全部撤銷）／`db_sources_add`／`db_sources_remove`（同一次呼叫依「取代→新增→撤銷」順序套用），誰能改誰仍由既有的委派政策決定（自己與 `reports_to` 子樹），每次變更寫入稽核紀錄 `db_sources_grant_changed`（呼叫者、對象、新增、撤銷、結果清單）。**驗證一律 fail-closed**：`db_sources`／`db_sources_add` 與儀表板寫入的每個來源 id 都必須是 `config.toml [db_sources.<id>]` 已設定的來源，不存在就整筆拒絕並列出可用 id（只列 id，不含連線字串），什麼都不寫；`db_sources_remove` 刻意**不**對照設定，所以操作者刪掉來源後留在 AI 員工身上的殘留授權仍可點名撤銷。新增兩支管理員 RPC：`db_sources.grants.list`（每個來源被哪些 AI 員工持有、全部 AI 員工清單、以及 `stale` 列出引用了已不存在來源的 AI 員工，絕不靜默吞掉）與 `db_sources.grants.set`（一次設定某個來源的持有者集合，回 `added`／`removed`／`unchanged`）；`agents.update` 的 `capabilities.db_sources` 支援整份取代；`agents.inspect` 一律回 `capabilities.db_sources` 陣列；`db_sources.remove` 刪來源時順手撤銷所有 AI 員工的授權並回 `revoked_from`。每次授權變更會在活動紀錄留一筆 `db_source_grant_changed`。工具端被拒的提示文字改為指向這三條路，不再叫使用者手改檔案。**已知限制**：授權在下一個對話回合生效（每回合重新啟動 CLI）；若開了預設關閉的 `[runtime] pty_pool_enabled`，長駐 REPL 的工具清單要等該 session 回收後才會看到新工具。

### Fixed
- **原生資料庫連接器在正式環境從 v1.64.0 起其實一直不能用（gateway 派生的 AI 員工即使有授權也被拒）**：gateway 幫每個 AI 員工啟動的 MCP server 用的是內部 key（client id `gateway-internal`），而 `db_sources`／`db_tables`／`db_select`／`db_query` 四支工具拿這個 client id 當 AI 員工 id 去讀 `agent.toml` 的授權，找不到檔案就一律「沒有任何資料庫來源授權」；派工閘門本身早已正確把內部 key 對應到 `DUDUCLAW_AGENT_ID`，只有工具處理器沒跟上，所以單元測試全綠、真實環境全拒。本次活測（真 mcp-server＋內部 key）抓到後修正：新增單一對應函式 `acting_agent_id`，內部 key 一律對應到該行程的預設 AI 員工，外部 client id 維持不繼承（精確比對，`gateway-internal-evil` 這類前綴不算），並附上會真的重現 1.64.0 行為的回歸測試（拿掉修法測試即紅）。**同類掃描結果，影響比資料庫工具更廣**，以下工具在 gateway 派生的 AI 員工上自 1.64.0 起同樣全部失效，一併修正：本機檔案去識別化工具 `file_read`／`csv_read`／`xlsx_read`（路徑圍欄綁到不存在的目錄）、`os_watch_status`（讀錯統計檔）、`browser_record_*`／`desktop_record_*`／`skill_from_recording`（錄製歸屬）、`capability_request`（PORTICO 任務授權永遠回「不在 scoped_tools 清單」）；`os_factory_reset` 的審批卡申請者標籤同步改為真實 AI 員工（純顯示，無授權變更）。`audit_trail_query`／`reliability_summary` 只把 client id 用於日誌與管理員判定，未動。

## [1.65.0] - 2026-09-24 — 去識別化 AI 智慧偵測×我的規則×CI Windows 修復

### Added
- **去識別化「AI 智慧偵測」：人名、地址、生日、帳號這類沒有固定樣態的個資，不必指定資料表欄位也抓得到（後端）**：新增內建規則集 `ai_pii`（介面名「AI 智慧偵測」）與新規則型別 `type = "ner"`。以往規則只認得固定樣態（身分證、信用卡、API key），人名／地址／生日／帳號要靠「資料表欄位規則」逐欄指定，貼上來的一段郵件、一份沒預期到的試算表就抓不到。現在改用 OpenAI Privacy Filter（Apache-2.0，1.5B 參數／50M 活躍的雙向 token 分類器）**在本機跑**：走 Rust 原生 ONNX Runtime，推論期間零網路，不送任何文字出去。偵測八類並對應到既有 category：`PERSON`／`ADDRESS`／`EMAIL`／`PHONE`／`URL`／`DATE`／`ACCOUNT_NUMBER`／`SECRET`（Email 與網址刻意沿用 regex 規則同一個 category，來源過濾寫 `EMAIL` 才不會只蓋到一半）。一條 `ner` 規則在編譯期展開成「每個標籤一條規則」，八條共用同一顆 session 與同一份以文字為鍵的 LRU 快取（256 筆），所以一輪只付一次推論；`priority` 預設 30，低於所有內建 regex 規則（50–100），重疊時精確樣態勝出，模型只補 regex 沒抓到的部分。新增 `[redaction.ner]` 設定（`threads` 預設 4、`idle_unload_minutes` 預設 10、`min_chars` 預設 24、`max_chars` 預設 32000、`cache_entries` 預設 256、`model_dir` 可覆寫），長文依段落切塊、位移回推，短於 `min_chars` 的字串根本不送模型。**⚠️ 需先下載模型**：release binary 不含模型也不連結 ONNX Runtime，首次啟用時下載約 945 MB 模型（Hugging Face `openai/privacy-filter`，釘死 commit）＋ 7–75 MB 的 ONNX Runtime 執行庫（Microsoft 官方 GitHub release，對應 `ort` 所指向的 1.24.2）；每一個檔案的 URL／大小／sha256 都釘死在 binary 內，下載走串流＋`Range` 續傳＋`.part` 暫存，**先校驗 sha256 才改名就位**，所以落在最終路徑上的檔案一定是驗過的檔案；任一檔校驗失敗就刪掉重來並讓整次安裝失敗，不會留下「一半的模型」。新增四支儀表板 RPC（權限閘與既有 `redaction.*` 相同）：`redaction.model.status`（安裝狀態、下載進度、以及來自真實推論的滾動延遲統計）／`.install`（背景任務、冪等）／`.cancel`（保留 `.part` 讓下次續傳）／`.remove`（只刪模型，保留執行庫）；`redaction.get` 的 `available_profiles[]` 新增 `requires_model`（依規則內容判定，不是寫死 `ai_pii` 這個名字，所以匯入的規則包若含 `ner` 規則一樣會提示下載）。稽核紀錄 `AuditEvent::Redact` 新增 `engine`（`"rule"`／`"ner"`）與 `model_revision`，模型抓到的 token 現在可稽核到是哪一版模型判的；舊的稽核行沒有這個欄位，讀回來一律視為 `"rule"`。**誠實揭露（這不是匿名化保證）**：實測 25 句繁中共 119 個 span，整體 recall 79.8%（不計類別 89.1%），電話／Email 100%、地址 88%、網址 83%、**人名 72%**、日期 58%、帳號 60%（不計類別 100% — 台灣銀行帳號常被判成電話，兩類都會遮所以資料仍被保護）；誤抓 5.5%，全部是 span 往前多吃了「出生日期」這類中文欄位名，是多遮不是漏遮。第三方在網頁爬取／病歷／法律文件等語料上的 recall 只有 10–38%，**缺口幾乎全在 recall**，所以 regex 規則一定要繼續開著，這是第二層不是替代品。效能：Apple Silicon 10 核 CPU、4 執行緒，繁中短句 63–161 ms、長文約 214 ms／千字，載入時常駐 1.1–1.7 GB（閒置逾 `idle_unload_minutes` 自動卸載、下次呼叫再載入）；機器記憶體不足導致權重被換出時單句會退化到數秒，請預留空間。**平台**：macOS（Apple Silicon）、Linux（x86-64／arm64）、Windows（x64／arm64）。**Intel macOS 不支援** — Microsoft 在 ONNX Runtime 1.24 已不再發布 `osx-x86_64` 建置，沒有可以誠實安裝的東西，所以卡片直接標示不支援、規則 fail-closed，而不是裝一個載不起來的庫。**fail-closed 語意**：勾了規則集但模型沒裝、平台不支援、或這個建置沒開 `ner` 編譯功能，三者都讓規則編譯失敗、整份 `RedactionManager` 建不起來（走既有毒化狀態，`duduclaw mcp-server` 拒絕啟動），絕不讓一條不會動的規則看起來像在保護你；`type = "ner"` 本身在所有建置下都能解析，規則集因此跨建置可攜。見 `docs/features/55-data-sources.md`。
- **去識別化「我的規則」：不會寫 regex 也能自己加偵測規則（後端）**：偵測規則集原本只有五個內建的，只能勾選、不能新增；公司自己的員工編號、專案代號、客戶代碼一律抓不到。現在可以自己建：每條規則＝一個**資料類型名稱**（自由輸入，例如「員工編號」）＋**關鍵字清單**或**樣式**兩種比對方式之一，可單獨停用而不必刪除。新增六支儀表板 RPC（權限閘與既有 `redaction.*` 相同，只有管理員能動）：`redaction.custom_rules.list` / `.upsert` / `.remove` / `.set_enabled` 管理規則，`redaction.profiles.import` / `redaction.profiles.remove` 匯入與移除整包 TOML 規則包（給 SI 一次佈署多個客戶用；匯入支援 `dry_run` 先看報告再決定，壞掉的規則逐條列出原因與所在行，一條都不可用時直接報錯而不是建一個空規則集）。規則存成 `~/.duduclaw/redaction/profiles/custom.toml`（匯入包存成 `<slug>.toml`；slug 依序取「明確指定的 `name`」→「`[meta] name` 的 ASCII slug」→「`pack_<meta.name 正規化後 SHA-256 前 8 碼>`」，所以中文包名如「製造業客戶包」不必也不能被要求自己取英文名，且同一包重匯是覆蓋而不是長出第二份；`[meta] name` 原文仍是介面上顯示的規則集名稱），寫檔一律走檔案鎖＋暫存檔原子改名，存完自動把規則集加進 `config.toml [redaction] profiles` 並即時套用，不必重開 gateway。內建規則集名稱與保留字 `custom` 不接受被匯入包佔用；內建規則集不能刪除（只能取消勾選）。**安全語意**：`custom.toml` 讀不到或解析失敗會直接回報錯誤，絕不當成「你沒有規則」而回一份空清單。
- **去識別化「貼幾個例子，自動產生樣式」（`redaction.suggest_pattern`）**：貼 2–5 個真實值（例如 `EMP-2024-0133`）與最多 3 個「長得像但不是」的反例，後端產出一條樣式並附逐筆命中表。引擎順序是**本機推論 → 雲端 utility 模型（走帳號輪替）→ 純規則啟發式**，回應會標明實際用了哪一種，沒有模型可用時也一定有答案（「未用 AI」）。產出的樣式**必須**通過驗證才會回傳：每個範例都要整串命中、每個反例都不可以被命中（反例用「會不會在句子裡命中」判定，因為實際引擎是搜尋而不是整串比對）；模型第一次沒過會帶著失敗原因重試一次，仍沒過就降級到啟發式，三種引擎都給不出可用樣式時回 `pattern: null` 而不是硬湊一條。範例值只進提示詞，不寫 log、不進稽核紀錄；送進模型時包在 XML 標記裡並明確標示為「資料、不是指令」。每位操作者每分鐘最多 10 次。
- **偵測規則可單獨停用（`enabled`）**：規則定義（profile 檔與 `config.toml [redaction.rules.*]`）新增 `enabled` 欄位，預設 `true`（既有設定行為完全不變）；設為 `false` 時引擎在編譯階段就略過該規則，五種規則型別（`regex` / `keyword` / `identity` / `json_path` / `db_field`）一體適用。
- **試跑可以測「還沒存檔的規則」（`redaction.dry_run` 擴充）**：精靈第三步「試一試」現在在**存檔前**就能驗證。`redaction.dry_run` 新增兩個選填參數：`sample_text`（貼純文字即可，後端自動包成 JSON 字串值，不必自己組 JSON；同時給 `sample_json` 時以 `sample_json` 為準）與 `draft_rules`（`[{ id, label?, category, kind, keywords?, pattern? }]`，驗證邏輯與 `redaction.custom_rules.upsert` 完全共用一份程式碼，預覽不會通過一個存檔會被拒的規則）。草稿規則只在該次呼叫編譯成候選規則集，不寫入任何地方；疊在現有規則之上，同 id 會覆蓋現有規則（這正是「編輯中預覽」的語意）。草稿命中的結果以草稿自己的 `id` 當 `rule_id`，介面才數得出「這條規則 · N 處」，其他規則的回報維持原樣。任何一條草稿不合法就整次呼叫報錯，不做半套試跑。
- **規則集可自訂資料類型顯示名稱（`[meta.labels]`）**：profile 檔新增 `[meta.labels]` 區塊（資料類型代碼 → 給人看的名稱，例如 `CUSTOM_EMPLOYEE_ID = "員工編號"`），`redaction.get` 回應新增 `category_labels`（合併所有已勾選規則集的對照表）與 `available_profiles[].custom`（`builtin` 的反義，指出哪些規則集可被刪除）。儀表板顯示順序：先查 `category_labels`，再查內建翻譯，最後才顯示原始代碼。

### Fixed
- **CI (Windows leg)**: `duduclaw-sysd`'s non-unix `server::bind` stub had drifted from the unix signature (`allowed_uid` argument), so the workspace failed to compile on Windows since the 1.63.0 tag build. The stub now mirrors the unix signature exactly. The Windows test build then surfaced three more unix-only leaks in test code, all gated to `cfg(unix)`: `duduclaw-core` `data_migrations` (`PermissionsExt`), the gateway's UDS-backed `sysd_integration` tests (`nix` is a `cfg(unix)` dependency), and the non-unix `CodriveClient` stub now derives `Debug` so its own fail-closed test compiles. On the Windows test *run*, `DUDUCLAW_COMPAT_DIRS` was split on `:` — which cuts a `C:\…` entry at the drive colon — and now uses `std::env::split_paths`; the four `data_migrations` tests that execute real `bash` scripts (a Linux-appliance feature) are `cfg(unix)`; and the `compat_runners` test `PATH` override now prepends instead of replacing, which removes a latent cross-module race that made the `data_migrations` tests fail whenever the scheduler interleaved the two modules (reproduced on macOS). CI test steps run with `--no-fail-fast` so one red crate no longer hides the rest. Follow-up round: the `data-file-guard` hook test harness tolerates `EPIPE` when the script exits before reading stdin (`off`/unset mode; a scheduler-dependent flake seen on Ubuntu), the `duduclaw-inference` `models_dir` assertions compare through `Path::join` instead of a hard-coded `/`, and **`duduclaw-security` audit-chain appends on Windows are now serialized through the sidecar `<path>.lock`** — `LockFileEx` on the append-only handle never actually held, so concurrent writers could fork the hash chain (the Windows test leg reproduced it; unix keeps `flock`). Gateway round: `RemoteGpuConfig::llamafactory_cli` joined the *remote* (POSIX) interpreter path with the local OS separator, so a Windows host would have handed `venv\bin\llamafactory-cli` to the GPU box — now an explicit `/` join; the rest are test-only Windows fixes (TOML/JSON path interpolation that treated `\` as escapes, platform-absolute paths in the approval and eval-suites-root tests, directory-handle backup semantics for the backup-prune mtime setup, and the unix-syscall / shell-script-probe smoke tests gated to `cfg(unix)`). Last round: the oversize-archive fixture had the same `format!`-into-JSON escape problem, the prune test's Windows directory handle needs `FILE_WRITE_ATTRIBUTES` for `SetFileTime`, and the tick fetch-failure test now polls for the counter (20s ceiling) with a per-platform failing command instead of trusting a fixed 1.5s sleep on a loaded runner.

### Security
- Bumped `rustls` 0.23.38 → 0.23.45 (RUSTSEC-2026-0285, TLS 1.3 handshake messages accepted across encryption-level boundaries).
- `cargo audit` now ignores RUSTSEC-2026-0293 (`ringbuf` 0.4.8, double free when an element's `Drop` panics) with a documented reachability argument in `.cargo/audit.toml`: songbird — the only consumer, behind the non-default `discord-voice` feature — only ever stores `u8` in its ring buffers, and upstream songbird (0.6.0) still pins `ringbuf = "0.4"`, so no lockfile bump can reach the 0.5.2 fix.

## [1.64.0] - 2026-09-23 — 去識別化資料表欄位規則×資料來源與資料庫連接器×MCP proxy

### Added
- **去識別化新增「資料表欄位」規則**：新增 `type = "db_field"`（`fields = ["res.partner.name", "hr.employee.*"]`，Odoo `model.field` 語法糖，`*` 排除 `id`）與通用 `type = "json_path"`（`paths`／`match_tool` 尾碼 glob／`match_args`／`exclude_keys`，自製路徑子集無新依賴）兩種去識別化規則；命中欄位整值 token 化，不靠 pattern 比對。先前 `engine.rs` 遇到 `JsonPath` 一律 `tracing::warn!` 後跳過，現在 `JsonPath` 真的編譯進結構化引擎：結構化 pass 對工具回傳的 `Value` 與 `content[].text` 內嵌的 JSON 字串皆生效，跑完再接既有文字 pass。新不變量：文字 pass 先掃出既有 token 的 span 再比對，token 不會被二次去識別化（`\d{8}` 類規則不再誤中 token hash 裡連續的 8 位數字）。`AuditEvent::Redact` 新增可選欄位 `path`（JSON pointer）；MCP 節流點（`mcp_redaction.rs`）把工具呼叫引數一併傳進管線；`duduclaw redaction verify` 新增 JSON 模式（`--tool <name>`／`--arg key=value`），附樣本 `docs/examples/redaction-sample-odoo.json`。dashboard 欄位規則編輯器留作後續。
- **去識別化補上 `IdentityRule`（讀組織名冊自動遮人名）**：RFC-23 v1.14.0 曾標記完成、實際從未落地的欠帳補齊。新增 `type = "identity"`（`source = "wiki"`，可省略或留空字串、效果相同，其他值在載入時直接報錯）；讀 `<home>/shared/wiki/identity/people/*.md` frontmatter 的 `display_name`（透過 `duduclaw-identity` 新增的同步介面 `WikiCacheIdentityProvider::list_people_sync`，不重複 YAML 解析；email 已由既有 regex profile 涵蓋、人員檔案本身也沒有 alias 欄位，故都不重複遮）。比對語意與 `KeywordRule` 相同（ASCII whole-word／CJK 子字串、不分大小寫，兩者共用抽出的 `match_needles`）；名單是快照，目錄 mtime 變動或距上次掃描逾 60 秒即重掃，新人到職／改名免重啟。`source` 填未知值、或人名目錄不存在，是設定錯誤，規則編譯失敗並拖垮整份 `RedactionManager::open`（fail-closed）；目錄存在但零筆人名是正常狀態（identity 同步尚未跑過），只 warn 不報錯。新增 `EngineOptions.identity_people_dir` / `ManagerPaths.identity_people_dir` 把名冊路徑帶進引擎。
- **去識別化「資料表欄位」規則編輯器 RPC**：`redaction.get` 回應新增 `field_rules`（只列 `db_field`／`json_path` 兩種規則，regex／keyword／identity 規則仍只能寫 TOML）；`redaction.update` 新增 `field_rules: { <id>: <規則> | null }` upsert-merge（`null` 刪除、缺席不動，語意同 `tool_egress`），寫入前先對整份設定試編一次，共用 `RedactionManager::open` 內部的 `resolve_rule_specs`（不另開第二套解析邏輯），路徑語法錯／model 不合法／未知 connector 一律擋在存檔前；新增 `redaction.dry_run { sample_json, tool?, args? }`，貼一段 JSON 樣本跑真實管線並回傳命中清單（`pointer`／`rule_id`／`category`／`token`），不回傳任何原值。`web/src/lib/api.ts` 新增對應型別（`RedactionFieldRule`／`RedactionDryRunResult` 等）與 `api.redaction.dryRun()`。去識別化 crate 把 `escape_pointer`／`scan_tokens`／`collect_token_locations` 三個共用小函式搬進新模組 `duduclaw_redaction::locate`，`duduclaw redaction verify` 的 CLI 端也改吃這份共用實作，不再各自維護一份。
- **去識別化新增「資料來源」登錄表，`db_field` 的 `connector` 改用 `source`**：新增 `[redaction.data_sources.<name>]`（`tools`／`table_arg` 或 `table`／`record_paths`／`key_alias`），把 `db_field` 規則原本寫死的 Odoo 對照表抽成任何工具都能綁的登錄表；兩個內建來源 `odoo`（原 `ODOO_TOOLS` 原樣轉換）與 `duduclaw_db`（見下一項）不可重新定義，未知 `source`／空 `tools`／壞掉的 `record_paths` 一律載入失敗。`RuleKind::DbField` 新增 `source` 欄位，`connector` 降級為已棄用別名（兩者都給且值不同即報錯，省略兩者仍預設 `odoo`）；資料表名驗證放寬，SQL 風格的無點資料表（`customers`、`public.customers`）與 Odoo `model.model` 寫法都合法。`redaction.get` 回應新增 `data_sources`（內建＋自訂），`redaction.update` 新增 `data_sources: { <name>: <定義> | null }` upsert-merge（拒絕改寫內建名稱、拒絕刪除仍被規則引用的來源、寫入前整份試編，與 `field_rules` 同一套語意）。
- **`duduclaw mcp-proxy`：讓客戶自接的外部 MCP server 也套上去識別化**：新增隱藏子指令 `duduclaw mcp-proxy --server <name> -- <cmd> [args…]`，一支 stdio JSON-RPC 轉發程式，對 `tools/call` 的 `arguments` 套 egress 決策、對回應的 `result` 套 `redact_value`，工具名命名空間成 `<server>.<tool>`。gateway 在去識別化生效時，把該 agent `.mcp.json` 裡除 `duduclaw` 以外的每個 stdio server 改寫成透過這支 proxy 啟動；接線涵蓋 `channel_reply.rs` 的兩個 Claude CLI spawn 路徑（一般 spawn 與 one-shot PTY，通道回覆會走到的地方）與 `claude_runner.rs` 的 `prepare_claude_cmd`（派工／cron／心跳／goal-loop 輪次，經共用的新函式 `mcp_proxy_cli_args`），去識別化未生效時兩邊都不加旗標、行為不變。原始 `env`（如密碼）走新環境變數 `DUDUCLAW_MCP_PROXY_ENV`（JSON 字串）帶給上游子行程，不進 argv，避免 `/proc/<pid>/cmdline` 洩漏。`duduclaw-llm` 新增 `ToolInterceptor` trait（`before_call`／`after_call`），`run_tool_loop` 接受可選的攔截器；gateway 用 `RedactionToolInterceptor` 實作它、接上同一個 `RedactionManager`，讓 openai-compat 直連 API 的工具迴圈（Grok／DeepSeek／MiniMax 等）享有同一套規則。已知限制：PTY session pool（`pty_pool_enabled`，預設關，文件標為備援路徑；pooled REPL 跨越多次呼叫，這道改寫依附的每次 spawn 暫存設定沒有地方可掛，需要 session 自己持有的改寫，尚未實作）、HTTP/SSE 型 MCP server（改寫時只 warn、原樣不動）、codex／gemini／antigravity 的 MCP 註冊、以及本機推論的工具迴圈（`local_llm.rs`）都還沒接上。
- **新 crate `duduclaw-db`：唯讀 SQL 資料來源連接器（四個 MCP 工具＋`db:read` scope）**：新增 `crates/duduclaw-db`，新依賴 sqlx 0.8.6（最小 feature 集：`runtime-tokio`／`tls-rustls`／`postgres`／`mysql`／`sqlite`／`chrono`／`json`／`uuid`／`bigdecimal`，不開 `any`／`macros`／`migrate`），三層各自獨立的唯讀保證（語句守門單一 `SELECT`/`WITH`、driver 層 `BEGIN READ ONLY`／`START TRANSACTION READ ONLY`／SQLite 唯讀檔案控制代碼、`max_rows`／`timeout_ms` 上限）。`config.toml [db_sources.<name>]`（`driver`、`url` 走 `secret://` 或 `url_enc`、明文只放行 `sqlite`、`allowed_tables` 必填非空、`max_rows`／`timeout_ms`）。四個 MCP 工具 `db_sources`／`db_tables`／`db_select`／`db_query`（`db_query` 只在 `allowed_tables = ["*"]` 才開放，否則連線前就拒絕），新增 MCP scope `Scope::DbRead`（`db:read`）與 `agent.toml [capabilities] db_sources` deny-by-default 授權清單，兩者於 MCP 分派節流點一併檢查，被授權的 `source` 名稱再由工具處理函式覆核一次。儀表板新增五支 admin-only RPC `db_sources.list`／`test`／`upsert`／`remove`／`tables`（`list` 只回 `SecretStatus` 絕不回連線字串，`upsert` 預設先連線測試成功才落盤，除非明確傳 `skip_test: true`）。
- **去識別化 dashboard「資料表欄位規則」與「資料來源」卡片上線**：設定 → 去識別化分頁新增兩張卡。「資料表欄位規則」：列表、簡易 `db_field` 表單（含 `source` 下拉，有連線的來源可直接選資料表與欄位）、進階 `json_path` 表單、儲存前整份試編（就地顯示錯誤）、貼樣本跑真實管線的「試跑」按鈕（只顯示命中位置與 token，不回原值）。「資料來源」：Odoo／`duduclaw_db` 標「內建」、自訂項目，新增表單分「經工具回傳」與「資料庫連線」兩分頁，後者含「測試連線」按鈕。`redaction.get` 的 `poisoned` 欄位現在真的接上 UI：壞掉的 `[redaction]` 設定會在卡片頂端顯示紅色橫幅（標題、原始錯誤原因、時間戳），存檔修好、熱重載成功即清除。
- **去識別化涵蓋地端檔案（CSV／Excel／文字）：新增 `file_read`／`csv_read`／`xlsx_read` 三個 MCP 工具＋資料檔守門 hook**：agent 讀地端檔案原本走 Claude CLI 內建 `Read`／`Bash`（curated 工具清單本就含這兩個），內建工具不是 MCP 工具，回傳完全不經去識別化節流點；通道附件只把存檔路徑附在訊息後，同一個缺口一樣存在。新增 `crates/duduclaw-cli/src/mcp_files.rs` 三個工具：`file_read`（純文字，512 KiB 上限）、`csv_read`（`csv` crate，檔案 64 MiB、`limit` 上限 2000）、`xlsx_read`（新依賴 `calamine = "=0.36.1"`，pure Rust，僅開 `dates` feature，支援 xlsx／xlsm／xls／ods，檔案 32 MiB 上限）；三工具共用同一道路徑圍欄（canonicalize＋根目錄限制：agent 目錄、`<agent_dir>/attachments`、`<home>/attachments`、`config.toml [files] allowed_roots`），受新 scope `files:read`（`Scope::FilesRead`）保護，但沒有額外的 per-agent 授權清單（圍欄本身已回答「這是誰的資料」）；`file_read` 刻意拒讀 csv/tsv/xlsx/xlsm/xls/ods，避免欄位規則被繞過（讀成一整塊文字會讓 `$.rows[*]` 規則永遠命不中）。去識別化登錄層新增 `TableSource::FromResult`（表名讀自工具回傳結果的 JSON pointer，不是引數）與 `free_form_names` 旗標（表名／欄位名可為任意文字，含中文與副檔名），JsonPath 語法新增 `['key']` 引號形式（接受除單引號、換行以外的任何 Unicode 字元）；內建來源 `duduclaw_files` 綁定 `csv_read`／`xlsx_read`，`db_field` 規則可直接寫 `fields = ["customers.csv.name", "客戶清單.xlsx.地址"]`（以最後一個點切分表名與欄位，不分工作表）。新增 PreToolUse hook `data-file-guard.sh`（隨既有安全 hook 一起安裝到 agent `.claude/hooks/`），依 `[redaction] data_file_guard`（`on`／`read_only`／`off`，預設 `on`，僅該 agent 去識別化生效時才作用）擋內建 `Read`／`Bash` 讀資料檔；`format_attachment_ref` 對 csv/tsv/xlsx/xls/ods/txt/md/json 附加「請用 csv_read／xlsx_read／file_read 讀取」提示。誠實揭露：守門的 `Bash` 檢查是檔名啟發式判斷，`python -c` 動態組路徑仍可繞過；守門本身是 shell script，沒有 `bash` 的 Windows 主機上形同不存在；三個 MCP 工具本身才是真正的保護面，守門只是降低誤走內建路徑的機率。`redaction.get.data_sources` 回應新增 `table_result`／`free_form_names`，`redaction.update` 接受同樣欄位。見 `docs/features/55-data-sources.md`。

### Changed
- **⚠️ 行為變更：MCP 去識別化層改 fail-closed；dashboard 用語「欄位」改「資料類型」**：單次工具呼叫時 agent 的 pipeline 建不起來（例如金鑰目錄壞掉），原本 warn 一聲後原樣放行工具結果，現在整個回傳值換成 `[redaction failed — value withheld]` 佔位字串。`config.toml [redaction] enabled = true` 但初始化失敗（規則編譯錯、identity 名冊路徑壞掉等），原本印一行「continuing WITHOUT redaction」照常啟動，現在 `duduclaw mcp-server` / `duduclaw http-server` 直接以錯誤結束、拒絕啟動。`[redaction]` 區塊本身解析失敗（TOML 語法錯、規則 `type` 打錯字、規則缺必填欄位）以前也被吞掉、行為等同「沒開去識別化」，現在收斂到同一條路：`mcp-server` / `http-server` 一樣拒絕啟動並印出解析錯誤原文，`duduclaw redaction verify` 也直接報錯，不再安靜退回內建的 `general` profile 產出一份驗證了另一組規則的報告。**⚠️ 行為變更（gateway 開機毒化狀態）**：gateway 開機讀 `config.toml` 現在分三種結果：沒有 `[redaction]` 區塊或 `enabled = false` 什麼都不建（跟關閉相同）；區塊解析成功且 `enabled = true` 照常建 manager；`config.toml` 整檔解析失敗、`[redaction]` 區塊本身寫壞、manager 建構失敗、或設定檔讀不到，四種情況一律進入毒化狀態：ERROR log、Activity Feed 留一筆 `redaction_init_failed`、`redaction.get`／`redaction.policy_status` 都多回一個 `poisoned: { reason, since }`（正常是 `null`）。毒化期間 gateway 照常開機，但沒有去識別化 manager，`duduclaw mcp-server` 遇到同一份壞設定本來就拒絕啟動，所以 MCP 工具整批不可用；純聊天不經工具不受影響。用 `redaction.update` 把設定改好存檔、熱重載成功即可清除毒化並補一筆 `redaction_recovered`，把去識別化整個關掉存檔一樣算恢復。順帶把 `redaction.fields.*`／`redaction.profiles.desc`（三語）的文案由「欄位」改「資料類型」：這裡的選項本來就是 PII 類別（EMAIL、TAIWAN_ID...），跟資料庫欄位是兩回事，key 不變。
- **Documentation currency pass (README / docs, 三語)**：修正一批已過期的公開文件敘述，讓文件跟上 v1.63.0 平台與 DuDuClaw OS v0.2.0 的實際狀態。通道數「九個 / nine channels」全面更新為現行的十一個（新增 WeCom、DingTalk），僅 `docs/adr/ADR-003-excluded-channels.md`（三語）保留「九」並加註這是該 ADR 撰寫當時的計數。DuDuClaw OS 相關文件（`docs/features/50-duduclaw-os-appliance.md`／`52-desktop-edition.md`／`docs/guides/appliance-build.md`／`hardware-requirements.md`，三語）由 v0.1.0 更新為現行 v0.2.0（內嵌平台 v1.63.0），trust-chain「現況」章節改為「截至 v0.2.0 仍未啟用 Secure Boot／dm-verity／TPM2」；同時修正 `appliance-build.md` 中文／日文版一段誤稱 dm-verity 與 Secure Boot「已啟用」的錯誤敘述（與英文版說法不一致，且與事實相反）；`docs/features/52-desktop-edition.md`（三語）移除映像未內建的 Kiro CLI。文件連結改指向已上線的官方文件站 `https://os.duduclaw.dudustudio.monster/docs/...`（OS README／CHANGELOG／硬體需求等頁面），GitHub 連結只保留給 repo／Releases 本身。`docs/README.md` 索引版本號與 feature 篇數（50→54）更新為現況。
- **`db_field` 的 `connector` 欄位降級為 `source` 的已棄用別名；MCP scope 從 22 個增加到 23 個**：新寫的規則請用 `source`，`connector` 仍照樣解析（兩者都給且值不同會直接報錯，避免靜默選邊）。新增的 `Scope::DbRead`（`db:read`）是 `duduclaw-cli` 認證層第 23 個 scope。
- **MCP scope 從 23 個增加到 24 個**：新增的 `Scope::FilesRead`（`files:read`）是 `duduclaw-cli` 認證層第 24 個 scope，`file_read`／`csv_read`／`xlsx_read` 三個地端檔案工具受它保護。
- **去識別化 dashboard「外部系統」與「資料來源」兩張卡合併成一張「外部系統與資料來源」**：內建登錄項 `odoo`／`duduclaw_db`／`duduclaw_files` 不再各自列成一列，新增改走統一的四步精靈（類型 → 連線／工具 → 測試 → 寫回，編輯從第 2 步開始）；寫回政策的 `tool_egress` key 改由系統自動推導（Odoo 對應 `odoo_*`，自訂工具來源對應共同前綴或逐一精確 key），操作者不再看到 key 本身；移除鼎新／Salesforce／HubSpot 固定樣板，改由通用的「自訂 MCP 工具」類型涵蓋；「資料表欄位規則」表單的來源下拉一律改顯示名稱，三個內建登錄 id 永遠不出現。

### Security
- **外部 MCP 工具的回傳結果現在也會經過去識別化**：先前去識別化只攔得到 DuDuClaw 自己的 MCP server（`mcp_dispatch.rs` 單一節流點），客戶自接的外部 MCP server（`.mcp.json` 裡宣告的 stdio server）由 Claude CLI 直接啟動，回傳完全不經過這條管線。`duduclaw mcp-proxy`（CLI 路徑）與新的 `ToolInterceptor`（openai-compat 直連 API 路徑）補上這個缺口；HTTP/SSE 型 MCP server 與 codex／gemini／antigravity runtime 尚未涵蓋，改寫邏輯遇到時只留警告日誌，不會假裝已處理。
- **`db_query` 在設有資料表白名單的資料來源上一律拒絕**：`duduclaw-db` 的 `db_query`（自由 SQL）只在來源的 `allowed_tables` 恰好等於 `["*"]` 時才可用。任意 SQL 語句實際碰了哪些表，沒有真正的 SQL 解析器就無法對照白名單檢查，所以設有白名單的來源在連線建立前就整支工具直接拒絕，不做半調子過濾。
- **新增地端資料檔守門，預設在去識別化生效時擋下內建 Read／Bash 讀資料檔**：`[redaction] data_file_guard` 預設 `on`，防止 agent 繞過新的 `file_read`／`csv_read`／`xlsx_read`，直接用內建 `Read`／`Bash` 讀 csv/tsv/xlsx/xlsm/xls/ods 而讓地端資料表欄位規則落空。兩個限制誠實揭露，不留到之後才被發現：`Bash` 的檔名比對是啟發式判斷，`python -c` 動態組出的路徑仍可繞過；守門本身是 shell script，沒有 `bash` 的 Windows 主機上這個 hook 直接失效。真正的保護面是三個 MCP 工具本身，守門只是降低誤走內建路徑的機率。

### Fixed
- **Dashboard 去識別化「Odoo egress」預設鍵自 v1.14.0 起從未生效**：`EgressEvaluator::find_rule` 把結尾 `*` 當前綴 glob 處理，dashboard 內建的 Odoo 預設鍵 `odoo.*`（前綴 `odoo.`）從未比對到真正的內建 Odoo MCP 工具名（`odoo_search`、`odoo_partner_search` 等一律 `odoo_` 開頭），操作者在儀表板為 Odoo 開啟「需要時還原真實值」形同沒開、仍停在預設 deny，且沒有任何錯誤或警告可察覺。現在 `EgressEvaluator::new`（每條設定載入路徑的唯一建構點）把舊鍵 `odoo.*` 當 legacy alias：載入時若沒有明確設定 `odoo_*` 就自動補上同一條規則（原始鍵保留，設定可原樣回存），並 `warn!` 一次提示改名；dashboard 內建的 Odoo 預設鍵改為 `odoo_*`，`presetForKey` 同時認得新舊兩種鍵，既有存檔的 `odoo.*` 仍會顯示為「Odoo」預設而非「自訂」。外部 MCP server（經 `duduclaw mcp-proxy` 命名為 `<server>.<tool>`，如 `crm_pg.*`）本來就是句點命名，不受影響。
- **gateway 內部 MCP 金鑰滿 30 天後全平台失去工具面（開機自動輪替修復）**：`mcp_internal_key::ensure_internal_mcp_key` 只看 `[mcp_keys]` 裡有沒有 `client_id = "gateway-internal"` 的條目，有就原樣沿用、不看 `created_at`；但 `duduclaw-cli::mcp_auth` 對超過 30 天的金鑰一律回 `API key expired (N days old, max 30)`。結果是首次開機滿 30 天的那一刻起，所有由 CLI spawn 的 `duduclaw mcp-server` 全部認證失敗，agent 安靜地失去整個 duduclaw 工具面，`duduclaw doctor` 的 MCP 冷啟動檢查會直接標 fail（實機實錘：金鑰建於 2026-08-16，2026-09-15 起全滅）。現在開機時改為解析 `created_at`：未滿 25 天（`ROTATE_AFTER_DAYS`，刻意留 5 天緩衝）的最新一把才沿用，否則鑄一把新的；超過 30 天硬上限、或 `created_at` 壞掉無法解析（認證層本來就會整列跳過）的 `gateway-internal` 條目一併清掉，仍在有效期內的舊金鑰則保留，讓拿著前一把金鑰跑到一半的 MCP 子行程做完自己的生命週期。輪替後的新值由既有開機修補 `ensure_mcp_absolute_paths_all` 覆寫進每個 agent `.mcp.json` 的 `duduclaw` server env（同 commit 補上回歸測試鎖住這個覆寫行為），所以重啟一次 gateway 即可恢復。非 `gateway-internal` 的金鑰完全不碰，穩態（金鑰還年輕、無可清理項）一個位元組都不寫，`config.toml` 的 mtime 維持不變。：比對前的小寫轉換（`match_needles` 共用函式，`KeywordRule` 與 `IdentityRule` 都吃這條路）原本直接呼叫 `str::to_lowercase`，少數 Unicode 字元（如土耳其文大寫 İ，U+0130）小寫後位元組長度會變（2 bytes 變 3 bytes），用小寫後字串算出的位移回頭切原文時可能落在字元中間、觸發 panic。改用保長度的折疊：單一字元只在小寫結果的位元組長度不變時才套用，長度會變的字元維持原字、退回大小寫敏感比對，涵蓋率不受影響（目前用得到的 CJK／ASCII 字元都不在此列）。

## [1.63.0] - 2026-09-08 — 多 runtime 開箱即用×裝置內建本地模型×帳號憑證硬化

### Added
- **WP-A：provider-aware `[[accounts]]`（`docs/todo/TODO-ai-runtimes-2026-09.md` §3）**：
  `accounts.add` 新增 `provider` 參數（`duduclaw_core::provider_env::KNOWN_PROVIDER_IDS` 之一，
  未帶時預設 `"anthropic"`，未知值 fail-closed 拒絕）；`build_account_entry` 依 provider 決定金鑰欄位
  名——`anthropic` 維持 `anthropic_api_key(_enc)` 相容既有設定，其餘 provider 一律寫可攜的
  `api_key(_enc)`（`duduclaw-agent::account_rotator::resolve_api_key` 早就讀得到這個備援欄位名，
  此前只是從未有寫入者真的用過）；`account.toml` 每筆帳號同步寫入 `provider = "<id>"`。`accounts.list`
  與 `accounts.budget_summary`（改用共用的 `account_status_to_json`）皆回傳 `provider`，dashboard
  帳號卡片同步顯示服務商。`AddAccountDialog` 新增服務商選單（`web/src/lib/provider-catalog.ts`，11
  家、各自的金鑰格式提示與取得金鑰連結；`"google"` 因是 `"gemini"` 的環境變數別名、實際路由永遠不會
  選到它而刻意不列出，避免使用者選到一個永遠死掉的選項）。新增 gateway／account-rotator 單元測試與
  `provider-catalog.test.ts`。
- **§1-1 訂閱登入風險告知（同 TODO §1 決策 1B）**：`CliLoginModal`／`SubscriptionSetupWizard`（以及只會
  開啟這兩者之一的 OOBE `RuntimeSetupCard`）新增共用元件 `SubscriptionRiskDisclosure`——登入流程開始前
  一律先顯示告知（Anthropic 與 Google 自 2026-03 起已在伺服器端封鎖第三方產品使用消費者訂閱帳號登入、
  且已有帳號因此被停權；OpenAI 政策不明）並要求勾選「我了解風險並自行承擔」才會啟用「繼續」按鈕；
  API 金鑰路徑不受影響、仍是預設建議。三語 i18n（`subscriptionRisk.*`）。
- **多 runtime 註冊表與七款新 AI CLI 後端（WP-B，`docs/todo/TODO-ai-runtimes-2026-09.md` §3）**：新增單一編譯期資料表 `crates/duduclaw-core/src/runtime_catalog.rs`——每個 runtime 的 id／顯示名／執行檔／別名／安裝管道／headless 呼叫模板／輸出格式（text｜json｜jsonl）／模型旗標／登入方式／憑證路徑／MCP 支援／模型家族前綴／ToS 註記／三語文案，全部寫在同一處。偵測（`runtime.detect`）、一鍵安裝（`runtime_install`）、模型探索（`runtime_models::discover_all`）、CLI 登入（`cli_auth::spec_for`）、`infer_provider_for_model`／`model_matches_provider`、`RuntimeType`、`CliKind`、容器 sandbox 的 argv 與金鑰環境變數，改為全部讀這張表；`VALID_RUNTIME_PROVIDERS` 與錯誤訊息裡的廠商清單也由表格生成，不會再過期。安裝白名單仍是**編譯期常數**、仍是精確比對、仍無 default arm——安全模型不變。
- **通用 print-mode runtime `runtime/generic_cli.rs`**：依 `RuntimeSpec` 驅動任何「一次問答、答案印在 stdout」的 CLI——模板 argv、prompt 走參數或 stdin、模型旗標（分離式 `--model X`／等號式 `--model=X`／環境變數）、工作目錄、逾時、text/JSON/JSONL 解析、非零離開碼與「需要登入」訊號對應到具名錯誤（`GenericCliError`，Display 保留 `classify_cli_failure` 依賴的關鍵字，failover 照舊運作）。
- **七款新 runtime**：Qwen Code（`qwen`，npm `@qwen-code/qwen-code`）、Kimi Code（`kimi`，npm `@moonshot-ai/kimi-code`，裝置碼登入）、GitHub Copilot CLI（`copilot`，npm `@github/copilot`，GitHub OAuth 裝置流程）、Kiro CLI（`kiro-cli`，curl 安裝腳本）、Cursor CLI（`cursor-agent`，`cursor.com/install`）、Mistral Vibe（`vibe`，PyPI `mistral-vibe`）、OpenCode（`opencode`，`opencode.ai/install`）。每一項的 headless 旗標都對照廠商官方文件查證並在 catalog 條目註明出處；查不到的一律標記而非臆造（見下方 Security 節的 Kiro 條款事項）。
- **`which_runtime(id)`／`which_runtime_in_home`／`detect_runtime`**（`duduclaw-core`）：依 catalog id 解析執行檔的單一探測函式，取代散落各處的 `which_codex`／`which_gemini`／`which_agy`／`which_grok` 呼叫；`claude` 保留原本更完整的 `which_claude`（NVM／Volta／bun／asdf／`.claude/bin`／Windows `.exe` 優先），`grok` 保留第三方 `grok-cli` 後備名稱。候選路徑新增 `/opt/duduclaw/runtimes/bin`（OS 映像內建 CLI 的位置）。
- **`runtime.detect` 回傳 `runtimes` 陣列**：每個 runtime 一列，含顯示名、執行檔、是否安裝、安裝管道與指令、登入方式與是否遠端可用、API 金鑰環境變數、憑證是否存在、MCP 支援、headless 旗標是否已驗證、廠商連結與三語 ToS 註記——dashboard／OOBE 不必再自己維護一份表。既有的 `claude_cli`／`claude_oauth`／`claude_subscription` 與各 runtime 的布林旗標維持原樣。
- **[WP-D] 裝置內建本地模型（gateway＋dashboard，TODO-ai-runtimes-2026-09 §3 WP-D／§1 決策 3A）**：DuDuClaw OS 映像帶 llama.cpp 的 `llama-server` 但不帶權重，這一包把「開箱沒有權重」到「本地模型正在回答」的路補起來。
  ① **appliance 預設值**（`duduclaw-inference/src/appliance.rs`）：偵測到 appliance 旗標**且** `/usr/bin/llama-server` 真的存在時（兩者缺一不套用），`inference.toml` 缺哪個 key 就補哪個——`enabled = true`、`backend = "openai_compat"`、`[openai_compat] base_url = http://127.0.0.1:8080/v1`、`models_dir = $DUDUCLAW_HOME/models`；**operator 寫過的值一律不覆蓋**（含 `enabled = false`），`[general] inference_mode` 完全不碰，維持 `hybrid`，設定好的雲端 runtime 照樣優先。
  ② **新 RPC `inference.local.*`（admin gated，`inference_local.rs`）**：`catalog`（六個精選 GGUF，每列附本機相容燈與是否已下載）、`download {id}`（背景下載，沿用市集的 job registry／續傳／100 GB 上限）、`serve {model_file, ctx?}`（原子寫入 `<home>/llama-server.env` 後重啟本地模型服務；非 appliance 主機照樣寫檔但誠實回 `restarted: false` 與原因）、`stop`、`status`（即時探測端點、回報實際載入的權重、下載進度、已安裝清單）。
  ③ **精選清單全數對 Hugging Face API 實查**（2026-09-05，repo `gated` 欄位＋`tree/main` 的 `lfs.size`＋匿名 HEAD 200）：Qwen3 1.7B／4B／8B、Gemma 3 4B、Llama 3.2 3B、Qwen2.5-Coder 7B，皆 Q4_K_M、皆未設門禁，repo commit SHA 記在原始碼註解裡。查證改變了兩列——Qwen 官方 `Qwen3-1.7B-GGUF` 根本沒有 Q4_K_M（只有 Q8_0）故改用 `unsloth`；Google 的 Gemma 3 GGUF repo 是 `gated: "manual"`、匿名下載回 401，故改用 `ggml-org` 版。
  ④ **dashboard「本地模型」頁新增內建面板**（`components/localmodels/BuiltInLocalModel.tsx`）：狀態橫幅（來自即時探測，不看設定檔）、相容燈、下載進度、「設為本地模型並啟動」、關閉；沒有內建引擎的主機整塊不顯示，Hugging Face 市集維持原樣。文案誠實標明本地推理由裝置自身處理器與內顯運算，且**不給任何沒實測過的 tokens/秒數字**。i18n 三語。
  ⑤ **文件**：新特稿 `docs/features/53-local-models.md`（三語）＋三語索引。
- **微調與後訓練（WP-E，`docs/todo/TODO-ai-runtimes-2026-09.md` 裁決 4C）**：dashboard 新增「微調與後訓練」頁（`/manage/finetune`，admin only，三語）與 gateway `finetune.*` RPC 家族。產品框架是**在這裡整理、到別處訓練、再收回這裡**——目標機種（N305／8845HS 內顯）訓練不了任何模型，2026 年的 LLaMA-Factory／Unsloth／Axolotl 全部需要 CUDA／ROCm，所以本功能明文不做本機訓練，UI 每一頁都寫明 GPU 在哪裡。
  - **資料集**（`finetune.datasets.list/create/delete/build/preview/export`）：從 `sessions.db` 的對話、`tasks.db` 的 `result_summary`、`approvals.db` 的審批決定，以及 `task_iterations` 的覆核裁決，建構 ShareGPT／Alpaca SFT JSONL 與 DPO 偏好對，附 LLaMA-Factory `dataset_info.json` 讓遠端免轉檔。無回答的對話、無結果的任務、未決的審批一律不產生資料列——回報的筆數就是真的寫進檔案的量，新機器誠實建出零筆。
  - **訓練工作**（`finetune.jobs.list/create/status/cancel`）：`FinetuneBackend` trait 三個實作——`dry_run`（只驗證設定並寫出 `train.yaml`＋`plan.json`，狀態永遠停在 `planned`，不會漂移成完成）、`remote_gpu_ssh`（SSH＋rsync 到使用者自己的 GPU 主機跑 `llamafactory-cli train`，取回 adapter，遠端有 llama.cpp `convert_lora_to_gguf.py` 時再取回 GGUF）、`together`（wire format 於 2026-09-05 對 `docs.together.ai` 線上 OpenAPI 實地查證：`training_type`／`training_method` 是物件而非平鋪欄位、9 種狀態列舉、DPO 用 `preferred_output`／`non_preferred_output` 的自有 JSONL 格式）。**不編造進度**：沒有任何百分比欄位，SSH 後端的狀態來自遠端 PID 與 adapter 檔案是否存在、日誌是 `train.log` 原文結尾，Together 用對方的 `status` 一對一對應；主機一時連不上時保留原狀態，斷線不等於訓練失敗。
  - **匯入**（`finetune.import`）：GGUF／LoRA（`.gguf`／`.safetensors`／`.bin`，本機路徑或 https URL）複製進 `<DUDUCLAW_HOME>/models`——與 `local_models.rs` 掃描、`duduclaw-inference` `InferenceConfig::models_dir` 指向的同一個目錄，所以成果直接出現在「本地模型」頁，不需要第二套登記表。
  - **資料離機閘門**：`datasets.export` 與對遠端後端的 `jobs.create` 未帶 `acknowledged_data_leaves_device: true` 時一律拒絕，回傳 `code = "data_leaves_device_not_acknowledged"` 的結構化錯誤，dashboard 據此顯示勾選確認而非紅色錯誤。失敗時關閉：欄位缺席＝拒絕，不認識的後端一律視為遠端。SSH 後端所有會進入遠端 shell 的欄位（host／user／workdir／python／job id／模型名）先過嚴格字元白名單，`BatchMode=yes` 避免密碼提示卡住 gateway。
  - 文件：`docs/features/54-finetune.md`（三語）、`docs/guides/remote-gpu-host.md`（三語，Ubuntu＋CUDA＋venv 裝 LLaMA-Factory＋llama.cpp 轉檔＋SSH 金鑰授權），features／guides 索引三語同步，三語 README 功能總覽各補一列。

- **[OS] DuDuClaw OS Yocto 基底 bring-up（Y 線，`meta-duduclaw/`，MAP-agent-native-os-2026-08.md 裁決⑥）**：去 Debian 化的新基底重建線開工——layer 骨架＋kas 設定＋UKI/systemd-boot 接通，QEMU 開機驗證到 login prompt（Y1-1）；`duduclaw-cli`／`duduclaw-sysd` 兩顆 Rust binary 的 cargo class recipe 完成並實際建置出 RPM（Y2-1／Y2-3，`duduclaw-comp` recipe 已寫但未 build-verified）；真機 genericx86-64 kernel provider 接通＋建置成功（Y2-2／Y2-3）；QEMU 雙驗證（sysd socket＋gateway `/healthz`）全綠。**同版同發工程形態**（Y3-3）：OS 版本單一源機制上線——`meta-duduclaw/conf/distro/include/duduclaw-platform-version.inc` 為唯一數字源（由 `scripts/release.sh` 通用 bump 迴圈同步，新增 `yocto_inc`／`yocto_bb` 兩種 manifest kind），`DISTRO_VERSION` 改為 `${DUDUCLAW_PLATFORM_VERSION}-y1-bringup`（里程碑後綴維持人工維護），`duduclaw-cli`/`duduclaw-sysd`/`duduclaw-comp` 三顆 recipe 的檔名版號（Yocto `<pn>_<pv>.bb` 慣例）由 release 腳本 `git mv` 同步；順帶修正 `duduclaw-comp` 版號孤兒漂移（Cargo.toml/Cargo.lock 停留在 spike 期 `0.1.0`，已正規化到平台版號 `1.62.0`，並補上 release.sh 對所有 workspace-excluded crate 自身 Cargo.lock 版號條目的通用同步，堵住這類漂移的機制性缺口）；OS image 的實際建置／簽章／發佈是獨立、不隨每次平台 release 自動觸發的人工步驟（`scripts/release-os.sh audit/plan/package`，見 `commercial/docs/DESIGN-unified-release-2026-08.md`）。**尚未出貨**：本節記錄的是 Y 線目前的 bring-up 狀態，不代表有可安裝的 OS image 存在。（2026-09-04 追記：本線已拆至 DuDuClaw-OS repo，v0.1.0 已於該 repo 發布；後續 OS 變更記於該 repo 的 CHANGELOG。）

- **features/52 桌面版（三語）**：新特稿 `docs/features/52-desktop-edition.md`——人與 AI 共用一台機器且不影響日常使用：agent 專屬 seat、影子工作區（headless 第二輸出＋子母畫面）、人輸入即凍結（QEMU 實測 3–4 ms）、Super+Enter 明確交還／Super+Esc 急停、watch mode、共駕預設關閉、後果性動作先審批、憑證一律交人、畫面文字視為 DATA；並誠實列出已驗證（容器＋QEMU 真輸入）與未驗證（真機 DRM、雙螢幕、AT-SPI2 真機點擊）項目。索引三語同步。

- **DuDuClaw OS 桌面殼：OOBE「AI Runtime 授權」改為 provider 清單**（`crates/duduclaw-shell`，2026-09-05，`docs/todo/TODO-ai-runtimes-2026-09.md` WP-C）：原本只收一把 Anthropic 金鑰的單欄位，改為可捲動的 17 家清單（Claude Code／Codex／Gemini CLI／Grok／Qwen／Kimi／Copilot／Kiro／Cursor／Mistral Vibe／OpenCode，加上 DeepSeek／MiniMax／Z.ai GLM／Groq／Together／OpenRouter），每列標明未設定／已存金鑰／已登入，並有兩個動作：「輸入 API 金鑰」（`accounts.add` 帶 `provider`，帳號 id `oobe-<provider>`）與「登入帳號」（走 gateway 既有的 `auth.cli_login.*`，畫面顯示解析自 CLI 輸出的裝置代碼與登入網址，可用機器上的瀏覽器開啟）。訂閱登入前先顯示 §1-1 風險告知（Anthropic／Google 自 2026-03 伺服器端封鎖第三方產品使用訂閱憑證、已有帳號被停權；OpenAI 政策不明），勾選「我了解風險，由我自行承擔」才會開始；未勾選時按鈕完全不掛 click handler。**失效即拒**：gateway 的 `RuntimeType::parse` 對未知 runtime 名會預設回 Claude，所以 Kimi／Copilot／Kiro／Cursor／Vibe／OpenCode 這六列在 WP-B 教會 gateway 之前一律不可啟動（會用錯廠商的身分登入），畫面誠實說明。每家的結果持久化到 `OobeSelections::runtime_providers`（`#[serde(default)]`，舊 state 檔照載），完成頁摘要改顯示「已授權 N 家」。三語文案齊備；`auth.cli_login.*` 一路標記 `UNVERIFIED: needs live gateway`（依 TODO §4，活體驗證等 WP-A／WP-B 合併）。
- **認證失效告警**：所有 Claude 帳號同時因認證錯誤（token 無效或組織停用）失敗時，DuDuClaw 現在會發一則 Activity Feed 事件並推一則通知到受影響 agent 的通知管道，說明排程與自動回覆已停擺、請到「設定→帳號」更新 token；只要有任何帳號恢復成功就再發一則復原通知。告警只在狀態變化時發送一次，故障持續期間不會重複打擾。
- **帳號卡片憑證狀態徽章**：帳號卡片新增憑證狀態徽章（未驗證／憑證損壞／token 無效／組織停用），健康帳號不顯示任何徽章。
- **儲存前先驗證憑證**（`accounts.add`，僅 Anthropic）：貼上 token／API key 時先向 `GET /v1/models` 認證一次（不耗 token、不計費）再決定要不要寫進 `config.toml`。401 與 403 直接拒絕並說明該怎麼修；離線或探測被限流時仍然存檔，但回應誠實標記 `verified: false`（成功驗證為 `true`，非 Anthropic 服務商為 `null`）。2026-09-08 事故中被放行的那把「短效 access token」（`sk-ant-at01-`）現在在送出任何網路請求之前就被擋下，並指向 `claude setup-token`。
- **`duduclaw doctor` 逐帳號憑證檢查**：每個帶有已儲存 token／金鑰的 Anthropic 帳號各印一列——有效／token 無效（401）／組織停用（403）／無法連線。連不上網只會是 WARN，不會謊稱憑證已死；金鑰只存在鑰匙圈、或非 Anthropic 的帳號直接略過（這裡沒有可以拿去認證的東西）。同時在 `claude auth status` 那一列下方加註：它只代表登入檔／環境變數存在，不代表 token 仍然有效。

### Changed
- **`compat.d` 的 `from_os` 新增 `linux-container`**（`duduclaw_core::compat_runners::FromOs::LinuxContainer`）：以 OCI 容器（docker／podman）交付的 Linux 工作負載，例如 DuDuClaw OS 的 LLaMA-Factory LlamaBoard 微調工作台 runner。首版宣告檔寫成 `linux-gpu` 被列舉拒絕為 malformed（這正是該列舉存在的目的），故正式加入變體而非放寬解析。
- **Windows RemoteApp 登錄檔可改放到 `$DUDUCLAW_WINDOWS_VM_APPS_DIR`**：`duduclaw compat windows-vm app-add/app-remove/app-list` 讀寫的 `apps.toml` 位置新增環境變數覆寫（未設定時仍是 `<DUDUCLAW_HOME>/windows-vm/apps.toml`；`compose.yaml` 與 VM 儲存不受影響）。DuDuClaw OS 因 gateway 家目錄改為各家 AI CLI 的 `$HOME` 並收緊為 `0700`，kiosk 殼讀不到裡面的檔案，映像遂把兩端都指向 `/data/system/windows-vm`——`duduclaw-shell` 的 `apps::windows_vm` 讀取路徑同步改為 `/data/system/windows-vm/apps.toml`（原本硬編碼 `/data/duduclaw/...`）。`docs/guides/app-compat.md`（三語）補述。
- **⚠️ 行為變更：`RuntimeType::parse` 改回傳 `Option<RuntimeType>`，未知字串不再默默變成 Claude**。單一 runtime 時代這是容錯，十二個 runtime 之後這是正確性漏洞——`auth.cli_login.start {runtime: "kimi"}` 打到不認得 `kimi` 的版本，過去會安靜地跑 `claude setup-token`，把別家的登入畫面交給使用者。現在請求型呼叫端一律拒絕並回報可接受的清單（清單由 catalog 生成）；只有讀「已存下的設定欄位」時才允許退回預設值，且集中在 `runtime_config::parse_provider_or_default` 一處、以 `error!` 記錄壞值與正確清單。`[runtime] fallback` 讀到不認得的值改為忽略（「沒有 fallback」本來就是合法狀態），不再退回預設。
- **`invoke_agent` OTel span 的 `gen_ai.system`／`gen_ai.provider.name` 改依實際 runtime**：原本寫死 `"anthropic"`，等於每一次 Qwen／Kimi／Copilot／Grok 的執行在 trace 裡都被記成 Anthropic，依供應商切分的成本與延遲儀表板對所有非 Claude agent 都是錯的。現在在 `[runtime] provider` 解析出來後即時 record；多供應商殼層（Copilot／Cursor／OpenCode／Kiro）沒有單一 vendor，回報 runtime id 而不是隨便挑一家。
- **`os_update.rs::MAX_ROOT_BYTES` 由 8 GiB 提高到 9 GiB**：OS 的 A/B root 槽由 7168 MiB 升到 8192 MiB（TODO-ai-runtimes-2026-09 §1-2，為了容納內建 AI runtime 套組），而 raw 槽映像的大小就等於槽大小——上限若不高於 8192 MiB，一個合法的滿槽 root 會在下載途中被拒。新增測試釘住「上限必須大於 A/B 槽」這個關係，避免兩邊再次各走各的。
- **`infer_provider_for_model` 現在認得新 runtime 的模型家族**：`qwen*`／`kimi*`／`mistral*`／`codestral*`／`devstral*`／`magistral*` 會對應到對應 runtime（CLI 已安裝）或 `openai_compat`（未安裝，API 模式服務任何模型）。先前這些模型一律回 `None`、不做自動對齊。多供應商殼層（Copilot／Cursor／OpenCode／Kiro）刻意不宣告任何模型家族，以免把 `gpt-5` 之類的 id 從 codex 手上搶走。
- **`CliKind` 補齊 12 個變體並與 catalog id 對齊**；`duduclaw-cli-runtime` 維持零 DuDuClaw 相依（它是獨立的 PTY pool crate），兩邊的一致性由 gateway 的測試把關。`RuntimeType` ↔ `CliKind` 只有一條橋接函式 `pty_runtime::cli_kind_for_runtime`。PTY pool 仍只接受有互動式 REPL 協定的四種（Claude／Codex／Gemini／Antigravity），其餘一律走 oneshot print-mode 路徑。
- **DuDuClaw OS 線拆分為獨立 repo（2026-09-04）**：`meta-duduclaw/`（Yocto 層）、`appliance/`（已凍結的 Debian/mkosi 線）與 `scripts/release-os.sh` 連同歷史移至 [DuDuClaw-OS](https://github.com/zhixuli0406/DuDuClaw-OS)，本 repo 只保留 Rust workspace（OS 以剪枝快照 vendor）。`scripts/release.sh` 移除 `yocto_inc`／`yocto_bb` 兩種版號同步 kind，平台版號流不再碰任何 OS metadata；OS 改採獨立版號（該 repo 的 `VERSION` 檔，起始 0.1.0 bring-up）並走該 repo 的 GitHub Releases。文件端同步：`docs/guides/appliance-build.md` 改為指向 OS repo 的入口頁；`docs/features/50` 的安裝步驟與現況改寫為 v0.1.0 事實（兩式產物、真機仍未驗證）；`docs/guides/hardware-requirements.md` 燒錄段落區分 `.wic`（僅 USB／磁碟）與安裝器 `.iso`（可光碟開機）；`docs/todo/TODO-H1-ISO-x86-installer.md` 加追記並補進索引。
- **DuDuClaw OS 文件事實修正（拆開 v0.1.0 發布 wic 查證）**：兩槽 UKI 與 systemd-boot 皆無簽章、GPT 無 verity 分割、無 TPM 套件——這三項是 OS repo 的建置 overlay 選項，v0.1.0 未啟用；安裝器 ISO 寫入的 `duduclaw-image-ab` 也不是無頭，它有同一個桌面殼與 gateway，只是沒有應用層。`docs/features/50` 安裝步驟改「Secure Boot 關閉」、「Optional Kiosk Display」（Debian 線敘述）改為「Editions and the Desktop」、現況段改寫；README 三語 OS 小節同步改為「人機共用且不影響日常使用」框架。
- **docs/features 與 docs/guides 三語對齊**：2026-08-16「三語規範化」之後累積的落差一次補平——features 48／49／50 與 guides `appliance-build.md` 補齊 zh-TW／ja-JP 譯本；features 51 與 guides `app-compat.md`、`hardware-requirements.md` 原本是繁中直接放在英文 root，改為 root 英文版＋繁中移入 `zh-TW/`＋新增 ja-JP；features 28 的 See also 與 31 的 Provenance（I-2b）段落補進兩語譯本；`guides/zh-TW/custom-mcp-tool.md` 半英文舊稿重譯；三語 features README 索引補 48–51 並更新版本／日期。三語 README 新增 DuDuClaw OS：為什麼表格加一列、架構一覽補出貨形態、安裝一節新增「DuDuClaw OS(值班機映像,pre-GA)」小節、功能總覽加一列、文件清單加入口；OS 小節首句改為「AI 原生住民的作業系統：整碟映像＝自家桌面、安裝器 ISO＝無頭版」，不再把整個 OS 寫成無頭值班機。

- **⚠️ 行為變更：認證失敗的帳號第一次就退場，不再等三振**（`claude_runner` 派工路徑與 `channel_reply::rotate_cli_spawn`）。過去 `oauth_org_not_allowed`／`authentication_failed` 走的是泛用 `on_error`：連續三次才標記不健康，冷卻 2 分鐘後又放回輪換。2026-09-08 的 403 就是這樣被復活了 18 小時，每個 cron tick 再燒一次 spawn。現在這類失敗直接呼叫 `on_auth_failed`，帳號立刻標成 `AuthDead`（區分 token 無效／組織停用），冷卻從 15 分鐘起跳、每次連續失敗加倍、上限 6 小時——重新簽發的 token 仍會自行恢復，真的死掉的則不再拖累排程。速率限制與帳務耗盡的分類與行為完全不變。
- **`accounts.list`／`accounts.budget_summary` 每列新增 `credential_state` 與 `credential_detail`**：`credential_state` 是帶種類的字串（`ok`／`unverified`／`broken`／`auth_dead:invalid_token`／`auth_dead:org_disabled`）——刻意不用 `CredentialState` 的 serde 形式（那會塌成沒有種類的 `auth_dead`），因為「重發 token」與「找組織管理員」是兩個不同的動作。

### Security
- **Kiro CLI 的廠商條款明文禁止第三方 harness**：AWS 的 Kiro FAQ 寫著「不允許透過第三方自動化 harness、將請求繞過 Kiro 原生介面」，而用 DuDuClaw 驅動 Kiro 正屬此類（自行在 CI 直接呼叫 `kiro-cli` 則被允許）。Kiro 的 catalog 條目因此把安裝管道設為 `Manual`、decline reason 為 `vendor_tos_restricts_third_party_harness`，並在三語 `tos_note` 中原文載明——gateway 不會替使用者自動安裝，啟用與否是使用者明確的決定。
- **catalog 的 `verified` 欄位**：headless 旗標若無法對照廠商文件或實機驗證，條目必須標 `verified: false` 並在註解寫明是哪一項——嚴禁臆造旗標。此欄位會經 `runtime.detect` 上浮到 dashboard，讓 UI 誠實顯示「尚未實機驗證」。目前全部 12 條皆為 `verified: true`（旗標來源已在各條目註明），未實機跑過的部分在 `docs/features/13-multi-runtime.md` 與本節如實說明。
- **通用 print-mode runtime 的能力執行邊界**：這些 CLI 在 headless 模式下必須帶自動核准旗標才會真的執行工具，因此該旗標寫在 catalog 的 argv 模板中。硬性、fail-closed 的約束是選用的原生 OS sandbox（`[capabilities] native_sandbox`，與 `runtime/grok.rs` 相同）；當 agent 宣告了本 runtime 無法轉譯的工具限制時，每次 spawn 都會發出結構化 `warn!`（與 Antigravity 既有的處理一致），不會靜默丟棄。
- **h2 0.4.13 → 0.4.18 修補 RUSTSEC-2026-0258**（unbounded empty DATA frames，2026-08-17 公告）——透過 reqwest/hyper 間接依賴，`cargo update -p h2` 鎖檔升版。

### Fixed
- **`release.sh` bump 會改到表格式依賴的版本行**：舊的 `sed 's/^version = "<semver>"/…/'` 對整個檔案生效，v1.63.0 bump 把 `crates/duduclaw-comp/Cargo.toml` 的 `[dependencies.smithay] version = "0.7.0"` 改成 1.63.0，OS 烤製在 `duduclaw-comp do_compile` 死於 `failed to select a version for the requirement smithay = "^1.63.0"`。現在只改 `[package]`／`[project]` 區段內第一個 version 行（awk），smithay 已還原 0.7.0。
- **`release.sh` bump 漏掉 detached crate lock 裡的 sibling 路徑依賴**：`crates/duduclaw-shell/Cargo.lock` 同時記錄 `duduclaw-native-gui`（`path = "../duduclaw-native-gui"`），舊的 awk 只改 crate 自己的版本項，v1.63.0 bump 後該項仍是 1.62.0。DuDuClaw-OS 2026-09-08 fix14 烤製把這份 lock vendor 進去，bitbake 的 `cargo build --frozen` 無法自行同步 lock，把 native-gui 的依賴當成未鎖定、去載入 zed 的 git 來源而離線失敗。現在 awk 對 lock 裡每個平台 crate 的 `name = …` 項都改版本（第三方套件不動）；本次 lock 已用 `cargo metadata --offline` 同步。
- **Launcher 底部提示「Super 鍵隨時喚起」承諾了一個不存在的手勢**（`crates/duduclaw-shell` `fake_data::LAUNCHER_FOOTER_RIGHT`）：整個堆疊沒有任何一層綁定單擊 Super——shell 綁的是 `cmd-k`（Linux 上 gpui 把 cmd 對到 Super）、comp 的全域手勢是 Super+K、選單列膠囊也標「⌘K」。2026-09-08 在 appliance VM 實測：單擊 Super 無反應，Super+K 與膠囊點擊皆可開啟。提示改為「⌘K 隨時喚起」並加單元測試鎖住。
- **本地引擎第一次探測失敗就整個程序永久停用**（`claude_runner::get_inference_engine`）：appliance 上第一件交辦若發生在模型尚未下載／服務前，`INFERENCE_UNAVAILABLE` 旗標會讓之後 `inference.local.serve` 起好的模型完全不被使用，直到 gateway 重啟（2026-09-06 fix12 走查發現）。改為 60 秒重探視窗，且 `inference.local.serve`／`stop` 完成後立即清掉引擎快取重探。
- **OOBE 完成頁在有線網路下顯示「網路 未連線」**（`crates/duduclaw-shell`）：只有 Wi-Fi 連線會設 `network_connected`，以有線上線通過網路步驟時旗標仍是 false。現在離開網路步驟時若有線在線即記錄已連線，完成頁顯示「有線網路已連線」（三語）。
- **goal loop：派工同步失敗仍占用並行配額**（`goal_loop.rs`）：work message 被 dispatcher 標為 `failed`（runtime 未安裝、本地引擎未就緒、憑證被拒…）時，任務仍留在 in-flight 並持有 RFC-27 edition lease，Personal 版 cap=2 之下所有後續交辦都被「edition concurrency cap reached」擱置到 30 分鐘 TTL 到期；gateway 重啟也不會解除（2026-09-06 appliance 走查重現兩次）。現在 in-flight 記錄帶 `message_id`，每個 tick 檢查該訊息是否 `failed`：是則立刻釋放 slot 與 lease、寫 activity、進入退避（60→120→240 秒），連續三次轉 `needs_human`（原因 `infra`）並附錯誤文字；driver 啟動時清掉前一個程序殘留的 `goal` lease（`duduclaw_core::concurrency_release_class`）。四個回歸測試。
- **`system.update_config {log_level}` 寫到沒人讀的鍵**：handler 寫 `[logging] level`，CLI 啟動只讀 `[general] log_level`，dashboard 回報成功但等級從未改變。改寫 `[general] log_level`。
- **本地工具迴圈把整包系統提示＋全部 MCP 工具（約 33k token）送給 8192 ctx 的 llama-server**，每輪都被 `HTTP 400 exceeds the available context size` 打回再退到 bare completion。`local_llm` 現在先向 `/props` 問 `n_ctx`（llama.cpp），依視窗預留 1024 token 給生成後，工具依註冊順序裝到 45% 預算（`tasks_*` 永遠保留），系統提示超出就從尾端截短並附可見標記；不支援 `/props` 的伺服器維持原樣。四個純函式測試。
- **OOBE「AI Runtime 授權」金鑰欄位對每家 provider 都顯示 `sk-ant-…` 佔位字**（`crates/duduclaw-shell` `RuntimeAuthFields`）：同一個欄位服務十七家，改為中性的 `API key…`。2026-09-06 QEMU 走查在 Codex 列發現。
- **本地推理設定檔 `backend = "openai_compat"` 反序列化失敗**（`duduclaw-inference::types::BackendType`）：列舉靠 `rename_all = "snake_case"` 產生的線上名稱是 `open_ai_compat`，但 `[openai_compat]` 表、`inference.update` 的後端驗證、dashboard 與 appliance 預設值寫的都是 `openai_compat`，導致引擎回報「no available backend」、本地交辦派工失敗（2026-09-06 QEMU 活體、模型已下載且 llama-server 已就緒的情況下發現）。變體改為 `rename = "openai_compat"` 並保留 `open_ai_compat` 別名；新增線上名稱測試。
- **[WP-D] `[openai_compat]` 端點活著卻回「No model loaded」**（`duduclaw-inference/src/openai_compat.rs`）：HTTP backend 的「載入模型」只是記下名字，權重在伺服器那邊。但 `loaded_model` 只有在 `load_model()` 被呼叫時才寫入，而 `InferenceEngine::generate` 只在 request 帶 `model_id` 或設了 `default_model` 時才會呼叫它——所以一份只寫了 `[openai_compat]`、沒寫 `default_model` 的設定（appliance 的預設形態，也是不少人手寫的形態），對著一台跑得好好的伺服器每次都回 `NoModelLoaded`。改為在 backend 建構時就以設定中的 model 名字登記；`model` 為空字串維持 `None`（沒指名模型就不替它捏一個）。
- **[WP-D] `models_dir` 的 `~` 與 `DUDUCLAW_HOME` 各說各話**：`InferenceConfig` 預設 `"~/.duduclaw/models"`，gateway 的下載與 `models.list` 用的是 `<home>/models`。兩者只有在 `DUDUCLAW_HOME` 沒設時才一致——而 appliance 正是設了（`/data/duduclaw`），於是引擎去 `$HOME/.duduclaw/models` 找模型、下載卻落在 `$DUDUCLAW_HOME/models`。`InferenceConfig::load` 改為在缺 key 時補上 `<home_dir>/models`（appliance 與否都套用），檔案裡寫死的值照舊優先。
- **[WP-D] 沒有權重的裝置回報連線錯誤而非「沒有本地模型」**：appliance 預設把端點指向 127.0.0.1:8080，但沒人下載過權重時本地模型服務不會啟動，每次本地推理都以一則無從處理的連線錯誤收場。`call_local_inference` 改為在送出請求前先判斷（僅限「appliance 預設生效**且** models 目錄沒有任何 .gguf」這一種情況）並回報 `NO_LOCAL_MODEL`；operator 自己設定的端點永遠不做這個判斷。
- **DuDuClaw OS 三功能走查（交辦／共駕／相容層）抓到的缺陷**（2026-09-05，QEMU 實機操作＋root 序列埠診斷）：
  ① **新機器沒有任何 AI 員工，交辦必定失敗**：OOBE「套用 Express」只寫本機旗標、從不呼叫 gateway；
  三張產業板模卡是 `fake_data`。現在 OOBE 完成（四條路徑）時若 `agents.list` 為空就以 `agents.create`
  建立 `role: "main"` 的「總管助理」（依 OOBE 語言命名），結果以通知卡回報；板模頁改為誠實的
  「產業板模需 Pro 授權」提示（`oobe::seed`）。
  ② **「AI Runtime 授權」的「立即設定」什麼都沒做**：現在是一個遮罩的 API 金鑰欄位，經 `accounts.add`
  存進 gateway（`oobe_runtime_fields`／`steps::runtime_auth::try_submit`），文案明說「存入、第一次交辦時驗證」。
  ③ **交辦送出後毫無回饋**：主畫面只列 `in_progress`，剛建立的 `todo` 任務看不到。任務 feed 改為
  `needs_human`／`in_progress`／`todo` 三種開放狀態（`gateway_client::OPEN_STATUSES`，卡片標 需要你／進行中／排隊中），
  交辦成功當下就發「已交辦」通知卡。
  ④ **launcher 的「交辦給 財務助理」、dock 的兩位 agent、控制中心「2 位在值 · 1 件等你」、通知中心「審批 2」分頁與
  「今天」活動列全是假資料**：新增 `overlay::agents_feed`（`agents.list`，30 秒同步），交辦卡顯示真實預設 agent
  或「還沒有 AI 團隊成員」、dock 依真實名單畫頭像並由該 agent 的開放任務推導狀態點、控制中心與分頁用真實計數、
  假活動列移除。
  ⑤ **app 安裝按了沒下文**：`flatpak install` 原本 fire-and-forget；現在等它結束並以通知卡回報成功／失敗
  （stderr 末行），且若映像內建離線倉庫 `flathub-offline` 有該 app 就從離線倉庫安裝、不走網路。
  ⑥ **gateway：機器上沒有 Claude CLI 時每次派工都死在「Claude CLI not found」**（appliance 不裝 CLI）：
  `claude_runner` 偵測到沒有 CLI 就視同 `api_mode = "direct"` 走 Direct API，讓真正的阻礙（缺 API 金鑰）浮上來。
  ⑦ **MCP 能力閘門用錯身分**：內部金鑰（`gateway-internal`）啟動的 MCP 子行程一律拿 client_id 當 agent id 讀
  `agents/gateway-internal/agent.toml`，os_native／recording／system_operator／codrive 在正式路徑上永遠 false；
  改為內部金鑰時以實際 agent（`DUDUCLAW_AGENT_ID`）為準（`mcp_dispatch.rs`）。
  ⑧ **OOBE 重跑時網路頁卡死**：gateway 的首次設定網路 API 在 admin 帳號建立後一律回 403，殼顯示「找不到網路服務」且無法繼續。
  gateway 的 403 現在帶 `code`（`first_run_completed`／`not_loopback`／`not_appliance`），殼把「首次設定已完成」
  （含舊 gateway 的無 code 403）視為網路已就緒（`NetError::FirstRunCompleted`），網路頁顯示說明、可直接繼續。
  ⑨ **Direct API 不讀 `accounts.add` 存的金鑰**：`try_direct_api` 只看 `ANTHROPIC_API_KEY` 與 `[api]`，
  機器上明明有一把 `[[accounts]]` 金鑰仍報「No API key available」；現在先問 rotator 的 API-key 帳號。
  ⑩ **launcher 開 app 留殭屍行程**：`apps::launch` spawn 後丟掉 `Child`，每點一次留一個 `[flatpak] <defunct>`；
  改為背景 reap，非零退出一律記 log（失敗的啟動終於有跡可循）。
  ⑪ OS 側修正（Flatpak `data` 安裝區與 remote 設定、polkit 放行桌面帳號、verify 目錄權限、gateway `[codrive]` socket 路徑）記在 DuDuClaw-OS repo CHANGELOG。
- **DuDuClaw OS 桌面殼：QEMU 全流程走查抓到的七個缺陷**（`crates/duduclaw-shell`，2026-09-05）：
  ① 安裝精靈的選碟清單把安裝媒介本身列為目標——改用 `lsblk -J` 含子裝置與掛載點的完整樹，
  排除 rom／loop／唯讀裝置、任何自身或分割區有掛載點的磁碟，以及以 findmnt＋PKNAME 解析出的
  安裝媒介所在磁碟；偵測失敗不再靜默 fail-open，畫面顯示「無法確認安裝媒介，請小心選擇」。
  ② 進度步驟的「進度未知（無 pv）」誤導文案改為「準備中…」（三語）。
  ③ 主畫面的占位內容（「晚上好，Louis」、三張假任務卡、假的「今日」摘要、假電池 86%、
  假時鐘 22:58）全部移除：問候語＝時段＋真實操作者名稱（與鎖定畫面同源；OOBE 的四個完成路徑
  ——Enter／完成／略過／範本頁略過——收斂為單一 `ShellView::complete_oobe`，主題與名稱在完成當下即帶入，
  先前只有 Enter 路徑帶主題、名稱則四處都沒帶，滑鼠點「開始使用」後主畫面與鎖定畫面都沒名字）、
  任務卡＝真實待核准與進行中任務（空狀態「還沒有交辦的任務」）、選單列時鐘＝真實本地時間每 20 秒更新、
  電池由 sysfs 讀取並以 30 秒快取，無電池即隱藏。
  ④ Cmd+L 鎖定後解鎖，鍵盤焦點從未交回 Home，交辦列點了打字沒反應——解鎖時立起一次性
  focus reclaim 旗標，由 Home 視窗下一次 render 認領（overlay 開啟時延後）。
  ⑤ 兩張品牌 PNG 從已拆到 OS repo 的 `appliance/branding/` 改為 vendor 進 crate 的 `assets/branding/`，
  主 repo 的 shell crate 拆分後首次恢復可編譯。
  ⑥ Boxed 單行文字欄（含鎖定畫面密碼欄）加上 overflow 裁切：按鍵自動重複時遮罩圓點會畫出欄位外直到螢幕邊緣。
  ⑦ 走查時發現的其餘問題（輸入法附著時序、硬殺後重開慢）記在 OS repo `wiki/eval/desktop-iso-qemu-walkthrough-2026-09-05.md`。
- **sysd 拒絕未授權連線時，拒絕回應可能被 Linux RST 摧毀**：server 對 uid 不符的 peer 寫完 `unauthorized` 回應後直接關閉，socket 收件佇列裡未讀的 request 使 close 變成 RST——client 收到 `ECONNRESET` 而非結構化錯誤（macOS 語義不同從未在本機重現，只在 Linux CI 以 `mismatched_uid_is_rejected` 閃失敗現形）。現在回應寫出後做尺寸與時間雙重上限（500ms）的 bounded drain 再關閉，未授權 peer 也無法藉此拖住連線。
- **`resolve_duduclaw_bin_from_exe` 測試在 Windows 矩陣必失敗**：解析器在 Windows 探測的是 `duduclaw.exe`（與實際出貨檔名一致，生產行為正確），但測試 fixture 用無副檔名檔名。fixture 改依平台命名。
- **帳號健康探測改成真的問 Anthropic，不再被 `claude auth status` 的假陽性騙過**：`claude auth status` 的 `loggedIn: true` 只代表環境裡有某個 `CLAUDE_CODE_OAUTH_TOKEN`，跟「這個帳號的 token 還能用」是兩件事——2026-09-08 一顆已被組織停用的 token 就這樣每 60 秒被「復活」一次，燒了 18 小時的排程。有存 token 的 OAuth 帳號與 API key 帳號，健康檢查現在改打一次零成本的 `GET /v1/models`：200 才視為有效並復活帳號，401/403 維持不健康，429／網路錯誤則不動、下一輪再試。沒有存 token、依賴 keychain 登入的帳號維持用 `claude auth status`。另加探測排程退避：401/403 這類確定性失敗會把下次探測往後排（1 分鐘起跳、逐次加倍、30 分鐘封頂），確定已死的憑證不再每 60 秒被重問一次；無法判斷的結果（429／網路錯誤）不改排程，探測成功、實際請求成功或又撞上一次認證失敗都會立刻把排程歸零，`accounts.list` 一併帶出 `next_probe_at` / `probe_failures` 供儀表板顯示。
- **無法解密的憑證在載入時就被排除，不再靜默用空憑證開子行程**：帳號的加密憑證解不出來（或解出空字串）時，開機時記一筆警告並標記為憑證損壞，永遠不進入輪替候選，直到憑證被重新儲存為止。
- **新增帳號對話框終於會顯示伺服器實際的錯誤訊息**：先前伺服器回傳的錯誤文字被前端丟棄、一律顯示通用失敗訊息；現在會原樣顯示（例如上述憑證驗證的拒絕原因）。

## [1.62.0] - 2026-08-21 — goal 意圖路由×代碼安全審計×記憶與停滯修復

### Added
- **通道 goal 意圖路由（`[goal_intent]`，預設開）**：通道自然語言現在會被判斷是否為交辦——三層分類（L0 零成本硬排除 → L1 訊號分數表零 LLM → L2 灰帶問小模型），命中即回覆確認選單（TG/Discord/Slack/LINE 三按鈕「建立目標／想一想／只是聊聊」，其餘七通道文字 1/2/3），**絕不自動建立**。確認後走與 `/goal` 完全相同的建立路徑（存取閘、autonomy 開工審批照舊），誤判成本壓到一則可忽略的訊息，因此敢預設開。L2 引擎 `mode`：`auto`（有本地推論引擎則走本地 YES/NO screening，否則摺進本來就要發生的主回覆、零額外呼叫）／`local`／`reply_tag`／`off`；任何模型不可用一律 fail-open 回聊天。`/goal ... || 想一想` 讓通道也能要求先出計畫再執行（原本只有 dashboard 有）。設計：`commercial/docs/DESIGN-goal-intent-router-2026-08.md`；使用者說明：`docs/features/48-goal-intent-router.md`。
- **代碼安全審計 `duduclaw secaudit`（對標 CodeBuddy Security）**：靜態掃描器編排（gitleaks/semgrep/cargo-audit/osv-scanner，缺工具誠實列 `engines_missing` 不靜默跳過）＋威脅建模式 intake（git 安全熱點）＋AI 深度審計（`--profile deep`，per-module 讀碼找靜態掃描追不到的路徑漏洞，`--max-modules` 防燒錢）＋**零共享上下文對抗式覆核**（新 agent 從乾淨重讀證偽候選，可疑留待人審、絕不自動 confirm）＋PoC 沙箱（`--poc`＋High 以上，container 內無網路執行，無沙箱一律不在宿主機裸跑）。`--save` 落報告到 `<home>/secaudit/reports/`；dashboard「安全審計」頁（manager 限定）看 findings 證據鏈＋人審三鍵。CI：exit 0/1/2，缺所有掃描器仍 exit 0；已證偽/已壓制不計入嚴重度統計與 `--fail-on`。設計：`commercial/docs/DESIGN-code-security-audit-2026-08.md`；使用者說明：`docs/features/49-code-security-audit.md`。
- **VS Code 插件 v2（0.3.0）**：AI 員工選擇器（吃伺服器端已按綁定過濾的 `agents.list`）、角色感知 UI（employee 不再撞審批權限牆）、選取內容問 AI 員工（DATA 圍欄）、交辦／想一想模式（`tasks.goal_create`）、任務 tab（含 needs_human 核准/重試/中止）、跨重啟對話 resume。上架材料備妥（尚未 publish）。設計：`commercial/docs/DESIGN-vscode-client-v2-2026-08.md`。
- **WebChat 顯式 agent 存取控制**：`user_message` 指定 `agent` 時現在強制檢查該使用者的 agent 綁定（非 admin 需綁定、fail-closed、60s 快取），resume 既有 session 沿用非預設 agent 時同樣重查——先前任何登入者可指定任意 agent 對話，且解綁後仍可靠舊 session 續聊。default agent 路徑逐位不變。
- **MCP scope 清單單一權威**：22 個 scope 字串收斂到 `duduclaw-core::mcp_scopes`，cli enum 與 gateway 複本改引用同一份＋雙向鎖同步測試；前端補齊先前缺的 12 個 scope（三語系描述），修掉它們在 dashboard 發不出 key（"Unknown scope"）的漂移。

### Changed
- **release.sh 每版自動同步 control plane 版本 allowlist**：pro image／pro binary 資產上傳成功後，`DUDUCLAW_PRO_VERSIONS`（Cloud Run env，`pro_versions::resolve` 的 ① 號最高優先來源）自動更新為 git 最新三個 `vX.Y.Z` tag——2026-08-17 生產事故實錘 auto-discovery（GitHub releases × AR manifest 探測）會靜默降級成 `["latest"]`，把 Pro 更新通道與 console 版本下拉一起清空；allowlist 從此由 release 流程維護為權威源，discovery＋registry fallback 退居安全網（control plane 端的探測留痕與 fallback 修正在 commercial 樹）。best-effort：失敗只 WARN 附手動指令，不炸 release；`DUDUCLAW_SKIP_PRO_VERSIONS_ENV=1` 可跳過。

### Fixed
- **duduclaw-sysd 令 Windows 目標編譯失敗**（v1.62.0 首發 tag 的 release CI 斷版,本 tag 重建修復）：值班機 root daemon crate 的 UDS client/server 未加平台門,`x86_64-pc-windows-msvc` 矩陣炸在 `UnixStream`/`Permissions::from_mode`。unix-only 模組（server/dispatch）cfg 門起來,非 unix 目標提供 fail-closed stub（`bind`/`SysdClient::call` 以 `Unsupported` 明確拒絕,絕不靜默 no-op）,維持 crate「全平台可編譯」的既定意圖;`cargo check --target x86_64-pc-windows-msvc` 實測通過。
- **AEE 停滯偵測自我迴圈死鎖**（2026-08-20 實錘：trader-lead「進化迴圈健康度」三訊號全亮,但 10 次「嘗試」實為 3 次合法空答案＋7 次偵測器自傷,零次真嘗試）：signal 3 把 `skipped`（「proposed no change — a legitimate empty answer」）當「重複駁回原因」→ AEE 起飛前檢查直接升級人工並記 `abandoned`「AEE escalated to a human」→ 該紀錄（恰為 24 字元前綴）又成為下一輪的「重複駁回」→ 永久死鎖、計數單調上升,引擎從此一輪都不能跑。三路修法：① 升級輪改記獨立 outcome `escalated`（共用常數＋描述前綴,舊資料靠前綴相容排除,容器 db 免手術）；② 三個停滯訊號一律只統計「真的生成過候選」的輪——`skipped` 與升級 meta 紀錄既不算嘗試也不算駁回、也不打斷連續計數（拍板 B：卡片「嘗試 N 次」名實相符,安靜 agent 不再亮紅）；③ 起飛前升級改「每個駁回連段只升一次」——snapshot 新增最新真駁回/最新升級時間戳,已升級且無新駁回證據就照常跑輪,真卡關仍會通報（告警指紋去重不變）,但偵測器再也無法餵養自己。
- **儀表板「關鍵洞察」／記憶頁被一筆 prediction 寫入永久打空**（2026-08-20 實錘：trader-lead 萃取 147 筆關鍵事實、儀表板顯示 0）：讀取端 `agent_memory_db_path` 優先讀 per-agent `agents/<id>/state/memory.db`（假設它只存在於舊安裝），但 prediction episodic 寫入路徑會在新安裝上憑空建立這個檔——一旦建立，該 agent 的所有記憶讀取 RPC（關鍵洞察／記憶清單／playbook 讀寫／skill 合成證據）從此全部解析到近乎全空的 per-agent 檔，而關鍵事實與經驗法則持續累積在共享 `memory.db` 裡不被看見。兩路修法：① prediction episodic 改寫進共享 db（與其他所有生產寫入端一致，並改走 `memory_factory` 單一建構點）；② 新增開機自癒 `memory_migrate::merge_per_agent_memory_dbs`——把殘留／舊制 per-agent memory.db 逐列（id 為鍵、絕不覆蓋既有共享列、FTS 同步回填）併入共享 db 後將原檔改名封存，冪等、單檔失敗不擋開機也不動原檔。
- **Windows：spawn-env 白名單缺整組系統變數,新版 Claude Code 起手即崩**（使用者現場回報,v1.61.x 全系列;第三起白名單事故）：v1.61.0 的 P3 env 擦洗上線時,Windows 平台清單只帶 USERPROFILE/APPDATA/LOCALAPPDATA/COMPUTERNAME——漏掉 `SystemRoot` 讓 Node 系 CLI（claude.exe）瞬間 fail-fast（exit 0xC0000409,零輸出,debug.log 只剩「子程序失敗」）。白名單補齊 Windows 系統組（`SystemRoot`/`windir`/`SystemDrive`/`ComSpec`/`PATHEXT`/`TEMP`/`TMP`/`OS`/`NUMBER_OF_PROCESSORS`/`PROCESSOR_ARCHITECTURE`/`HOMEDRIVE`/`HOMEPATH`/`USERNAME`/`ProgramFiles`(x86)/`ProgramData`/`ALLUSERSPROFILE`——全是機器形狀變數,非操作者資料,與 PATH/TMPDIR 同類）；`worker_supervisor` 的手刻四名複本收斂到共用常數（手刻複本正是漏修的溫床）；兩個平台常數改為全平台編譯,密鑰形狀防護測試從此在任何開發平台都蓋得到它們。升級本版後,現場的 claude.exe 墊片（補 SystemRoot 再轉呼真品）可以拆除。
- **os_watch 觸發的 goal 未凍結 `acceptance_criteria_baseline`**：三條 goal 建立路徑（dashboard／`/goal`／os_watch）中，os_watch 是唯一漏寫驗收基準的一條，判官因此讀到 NULL 基準。現已對齊另兩條的凍結契約（goal 意圖路由 2026-08-17 稽核順帶抓出）。
- **Discord 元件互動閘吞掉 goal 意圖確認按鈕**：`handle_component_interaction` 開頭的 `if ns != "duduclaw"` 早退會靜默丟棄所有 `gintent:` 按鈕；已把意圖分支上移至該閘之前，並補 deferred-response（想一想會呼叫 LLM，可能超過 Discord 3 秒 ack 窗）。
- **secaudit：cargo-audit 真實輸出的 `"url": null` 使整份報告解析失敗**（活體 dogfood 2026-08-18 抓到，本 repo 4 個真實 rustls-webpki findings 被靜默歸零）——欄位改 `Option`；且已證偽／已壓制的 findings 先前仍計入嚴重度統計與 `--fail-on`，讓對抗覆核對 CI gate 失效，已改為只計可行動 findings。

### Security
- **升級 rustls-webpki 0.103.12 → 0.103.14 修補 RUSTSEC-2026-0104**（憑證撤銷列表解析可達 panic）：secaudit 首次 dogfood 就掃出本 repo 實際依賴圖的 rustls-webpki 中此洞——連帶 `aws-lc-rs` 1.16.3→1.18.0、`aws-lc-sys` 0.40→0.44，已完整重驗（全 workspace build＋7575 測試＋真 TLS 呼叫確認握手正常）。Cargo.lock 另有 `rustls-webpki` 0.102.8（經 `rustls` 0.22.4）殘留孤兒，不在實際依賴圖、不編進 binary（cargo-audit 掃 lock 全表才報的 3 個 finding），為避免跨 target/feature 破壞未強行移除，待確認無任何 target 使用後再清。

## [1.61.2] - 2026-08-17 — 更新通道修復與 Pro 自動升級

### Added
- **Pro 自動升級通道（開源側 seam）**：`GatewayExtension` 新增 `update_provider()` hook（預設 `None`，CE 行為逐位不變）；updater 開出可重用的下載重試／參數化驗簽（`verify_archive_with_pubkey`）／換裝（`install_verified_binary`，內含容器閘——容器內一律拒絕行程內換 binary，image 部署走 image 更新）公開面。6h 更新迴圈與 `system.check_update`／`apply_update` 在 provider 存在時改走 provider；回應與廣播新增 `update_channel`（`control_plane`／`github`／`none`）與 `containerized` 欄位，更新頁據此顯示正確動線（Pro 無通道→說明卡、容器→主機端 update.sh 指引、CE 照舊）。`InstallMethod` 新增 `pro`。企業側的 `ControlPlaneUpdateProvider`（license-proof 認證、獨立 pro 簽章 key）與 control plane 端點在 commercial 樹（設計：`DESIGN-pro-auto-update-2026-08.md`）。同步收掉一個噪音洞：Pro 部署開 `auto_update` 先前每 6h 撞拒絕防呆留一筆 `auto_update_failed` 審計，現在每個新版本只記一行 info。release.sh 新增 pro binary 資產步驟（依 build host triple 自動標平台、pro key 簽章、GCS 上傳、verify 檢查）。

### Fixed
- **儀表板系統更新在標準 macOS 佈局上必失敗**：updater 的目錄權限閘用 `mode & 0o022` 一刀切，把 group-writable 一律當不安全——但 macOS 的 `/usr/local/bin` 出廠就是 `drwxrwxr-x …:admin`（admin 群組成員本來就是管理員），於是每一台標準安裝的 Mac 按「安裝更新」都得到「更新失敗」（audit log：`Binary directory is world/group writable`）。新的 `is_unsafe_update_dir` 維持 world-writable 一律拒絕，group-writable 只放行 admin-class 群組（root/wheel gid 0 全平台、macOS `admin` gid 80）；macOS `staff`（gid 20，所有本機使用者都在）之類仍拒絕。錯誤訊息現在帶目錄路徑與修復指令。
- **企業版 wrapper（`duduclaw-pro`）自我更新會把部署換成 CE binary**：公開 release 資產是開源 `duduclaw`，蓋掉 wrapper 會同時失去 Enterprise extension，且 wrapper 忽略 argv 而 CE binary 要求 subcommand——無參數的 launchd/systemd 服務單元重啟後直接 clap 錯誤 crash-loop。現在 `apply_update` 偵測到 `duduclaw-pro` 即拒絕並說明正確升級路徑（企業散發包／重建流程），比照桌面版 sidecar 的既有防呆。
- **配額預警 frame 被當成 rate-limit 失敗**（[TODO](docs/todo/TODO-rate-limit-warning-misread-as-failure.md)）：`claude` CLI 的 `rate_limit_event`（`allowed_warning`，run 照常完成）先前被兩個 stream parser 丟棄，但 frame 原文可經診斷字串（`last_line`）進入錯誤訊息，`rateLimitType` 小寫後含 `ratelimit` 子字串→健康帳號被誤判 rate-limited 進冷卻、已成功的呼叫被換帳號重打——恰好發生在配額最緊的時候（2026-08-17 實錘：七日窗 92% 時派工被回報成 rate-limit 失敗）。現在 frame 解析為 telemetry（新 `rate_limit_watch` 模組：節流 warn 日誌＋`system.status` 回傳 `quota_warning`），診斷字串不再夾帶 frame，`is_rate_limit_error` 額外中和 advisory token（真拒絕的分類逐位不變，含回歸測試）。
- **spawn-env 白名單事故收尾**（[TODO](docs/todo/TODO-spawn-env-allowlist-fallout.md)，v1.61.0 CRITICAL）：① `setup-token` 容器部署的 OAuth session 現在把 token 記在帳號上顯式注入（先前 P3 擦洗後子行程拿不到任何憑證，派工全滅 `authentication_failed`，而手動 `claude -p` 因繼承 shell env 反而正常——矛盾正是難查的原因）；② 白名單丟棄 gateway 環境中存在的 `DUDUCLAW_*` 變數時，開機警告一次（只記名不記值，附 `.mcp.json` env block 修法）——兩起 production 事故各花數小時，正因丟棄完全無聲；③ `DUDUCLAW_SEMANTIC_VECTORS`（功能旗標，非憑證）加入白名單，操作者設 `=1` 重新在 spawn 出的 MCP server 生效；④ 白名單不含密鑰形狀名字的型別層保證不變。

## [1.61.1] - 2026-08-16 — 客戶端 WebSocket 協定修正

### Fixed
- **三個客戶端的儀表板功能全掛（VS Code／Chrome／Stream Deck）**：它們的 RPC 都是照 JSON-RPC 2.0 寫的（`{"jsonrpc":"2.0",…}`），但 gateway 用的是自家的 `WsFrame` 協定（`{"type":"req",…}`，回覆是 `ok`＋`payload` 而非 `result`）。少了 `type` 標籤，gateway 的反序列化直接失敗、判定成握手失敗並關閉連線，客戶端則把它回報成 `connection closed` 之類的傳輸錯誤——訊息完全指不到真因。三個客戶端的收送兩端都已改正，並過濾掉沒有 `id` 的 `event` 推播（先前會被誤當成回應）。Stream Deck 連 committed 的建置產物 `bin/plugin.js` 一併重建（只改原始碼會讓出貨的檔案仍是壞的）。**聊天功能不受影響**：它走 `/ws/chat` 的另一套協定，本來就正確——這個分裂正是 bug 能出貨而沒被發現的原因（測試時對話會通，看起來像好的）。Obsidian 與 WordPress 只用聊天協定，無此問題。追蹤：[`docs/todo/TODO-client-ws-protocol-mismatch.md`](docs/todo/TODO-client-ws-protocol-mismatch.md)。
- **WebSocket 握手把「格式錯誤」與「認證失敗」混為一談**：第一個 frame 若無法反序列化成 `WsFrame`，gateway 只記一句 `WebSocket auth failed`，於是上面那個純協定 bug 被一路當成憑證問題查。現在格式錯誤會另記一行明說是 client 協定錯誤而非憑證問題，並提示期待的 frame 形狀。純日誌變更，行為不變。

## [1.61.0] - 2026-08-16 — cron 星期慣例修正×Claude Code 匯入×通道能力表×上下文瘦身

### Added
- **上下文膨脹正解（`[runtime] minimal_context`，預設開）**：每次 spawn 官方 CLI 現在帶 `--tools <策展清單>`（只送該 agent 實際會用的內建工具 schema，其餘走 MCP）＋`--setting-sources project,local`（不再把操作者個人全域 `~/.claude/CLAUDE.md`／rules 塞進對客 agent）。本機實測固定開銷 35,892 → 10,974 tokens/次（省 ~69%）。安全：**不用** `--setting-sources ""`（那會停用 cwd `.claude/settings.json` 的 agent-file-guard 安全 hook）；`project,local` 省一樣多 token 但保留 hook。可用 env `DUDUCLAW_MINIMAL_CONTEXT=0`（全域）或 `agent.toml [runtime] minimal_context = false`（每-agent）關閉。行為變更：曾靠操作者全域 CLAUDE.md 塑形對客 agent 的部署升級後會有變（反模式，off-switch 已備）。
- **⌘K 內容搜尋（`search.query`）**：命令面板新增跨來源內容搜尋——對話、產物、記憶、wiki 聚合查詢，結果依來源分組、點擊跳轉；各來源有結果上限、CJK-safe 截斷。個人版恢復先前缺失的搜尋觸發器。
- **`/files` 升級「產物與檔案」**：檔案頁加搜尋（檔名/來源）、依任務關聯篩選、日期範圍（`GET /api/files` 新增 `q`/`task_id`/`since`/`until` 參數，全 optional、回傳結構不變）。
- **任務清單操作集（I-3b）**：`/goals` 每個任務可置頂／歸檔／重新命名（新 RPC `tasks.archive`/`unarchive`/`pin`/`unpin`/`rename`）；`tasks` 表加 `archived`/`pinned` 欄位（冪等 migration）；置頂排序置前、歸檔預設隱藏可切換顯示；先前 20 筆硬截解除，改走 `tasks.list_page` 分頁（limit clamp＋offset＋total）＋載入更多；頂部搜尋框。
- **`/presets` preset 儀表板（唯讀）**：新頁列出可用 preset 目錄與各 agent 目前綁定、被覆寫欄位（`presets.list`/`presets.status` RPC，admin 限定，preset P1 唯讀範圍）。
- **runtime 狀態匯入 P0（`duduclaw migrate-from claude-code`，含逐字稿）**：把 Claude Code 的記憶、CLAUDE.md 與對話逐字稿單向匯入 DuDuClaw。memory shard → semantic 記憶＋SPO（`store_temporal` 冪等，`valid_from` 取 frontmatter `modified`）；CLAUDE.md → agent wiki（`layer=context`，不佔系統提示注入預算）；session 逐字稿 → 精簡對話＋零 LLM 摘要（噪音濾除只留人類 prompt＋assistant 最終回覆，丟棄 thinking/tool_use/hook——實測逐字稿有效訊號僅約 1.5%）。匯入內容一律 `origin=import`（trust ≤ 0.7，不偽裝一手觀察）、一律當 DATA、redaction 預設開（`--no-redact` 關）、skill 過 `skill_security_scan` fail-closed。需 `--agent <id>`（缺漏 fail-fast）。真實 `~/.claude` dry-run 活測 53 專案、1114 項可匯、注入掃描正確擋下可疑 shard。仍需人工 `--apply` 才真正寫入。Codex/Gemini 平台為 P1（本機未安裝，未實作）。
- **通道能力表（plugin P2，消除靜默失效）**：新增 `channel_capabilities.rs` 單一權威表——11 通道 × 7 能力（檔案/照片上傳、互動按鈕、edit-in-place、typing、原生 markdown、引用回覆）＋進度節流秒數；先前散落於各通道的硬編判斷（含 10 處進度節流字面值）收斂到查表。不支援某能力的通道從「靜默 no-op」改為留下結構化 log（`send_document`/`send_photo`/typing fallback），使用者能觀察到為何某功能在某通道未生效。
- **Agent Mail 拒絕備註**：`mail.decide` 加 optional `note`——拒絕時以操作者原因取代系統文案並即時結算，核准時另記 `decision_note`（保留背景 worker 為唯一寄信/結算者的不變量）；儀表板拒絕動作加備註輸入框。

### Changed
- **cron 星期欄改用標準 crontab 慣例（行為變更，BREAKING）**：`cron` crate 0.15 的星期欄是 Quartz 慣例（1=週日…7=週六），而 DuDuClaw 先前從未轉譯——使用者照 crontab man page 寫的 `* * 1-5`（意圖週一到五）實際排的是「週日到週四」：週日幽靈觸發＋週五整天靜默跳過（2026-08-16 LWM 實驗實錘：台股休市的週日早上被派了盤前班）。現在 `duduclaw_core::cron_tz::normalise_cron` 在解析時把數字星期從 Unix 慣例（`0`/`7`=週日、`1-5`=週一到五）轉譯成 crate 序數；cron 排程器、heartbeat、MCP 建立/更新驗證、dashboard 驗證全走同一份 normaliser。名字寫法（`MON-FRI`）與 `*`、`*/step` 語意不變；數字範圍展開成明確列表（`6-7`→`1,7`）避免反向範圍。通道回條 `humanize_cron_zh` 同步改讀 Unix 慣例（`0`/`7` 都是週日、`1`=週一）。**升級注意**：先前刻意照 Quartz 慣例寫數字星期的排程升級後會位移一天（想要週日請寫 `0`、`7` 或 `SUN`）；照 crontab 直覺寫的排程（絕大多數）則是升級後才第一次正確。內建「週報彙整」模板（`0 17 * * 5`，標示每週五）先前實際在週四觸發，本次起與標示一致。`/goals` 點任務改導向 `/tasks/:id` 正式詳情頁，舊的 goal dialog 移除、內容併入四分頁詳情（驗收/風險/產出/kickoff/輪次＋活動時間軸＋MAV 徽章）；`/goals?task=` 保留為轉址相容層。needs_human 三按鈕審批動線不變，goal 任務旁補上先前 dashboard 漏掉的第 4 顆「交給我」按鈕（對齊通道卡片本有的四按鈕）。
- **`task_row_to_json` 輸出 `archived`/`pinned`**：`tasks.list`／`task.updated` 廣播現在帶這兩個狀態欄位。

### Security
- **per-agent git 憑證授權（合規恢復 git push／簽章）**：第八波 env 擦洗刻意排除 `SSH_AUTH_SOCK`/`GNUPGHOME` 後，靠 SSH/GPG 從 spawn CLI 做 git push／commit 簽章的 agent 會失效。新增 `agent.toml [capabilities] git_credentials`（預設 `false`）——明確開啟才讓該 agent 的 spawn 子行程額外拿到 `SSH_AUTH_SOCK`/`SSH_AGENT_PID`/`GPG_TTY`/`GNUPGHOME`，git push over SSH 與 GPG 簽章即恢復；預設關的 agent 與第八波逐位相同（拿不到操作者 SSH/GPG 身分＝合規不變）。每次實際追加 git 憑證 env 時寫審計（`git_credentials_env_granted`，只記變數名不記值）。可在 dashboard 編輯 AI 員工的能力頁以危險區開關設定（二次確認＋風險提示，比照 computer_use），或手改 `agent.toml`。誠實揭露：開啟即把操作者完整 ssh-agent/gpg 身分交給該 agent（ssh-agent/gpg 協定無更細粒度的「只給 git 用」介面），故預設關、逐 agent 明確授權。
- **credentials P2 零重啟輪換**：改過憑證不再需要重啟 gateway。帳號池（Claude/OAuth/API key）寫入（`accounts.add`/`update`/`update_budget`）觸發 rotator 快取失效，同進程即時生效（取代原本 5 分鐘 TTL；跨行程 CLI 直寫 `config.toml` 因打不到行程內失效仍留 30 分鐘 backstop）；Telegram 每輪 `getUpdates` 重解析 token（不再烤進 api_base）；Feishu／WhatsApp／WeCom／DingTalk／Google Chat／Teams 六個 webhook 通道 inbound 驗簽改 per-request resolve（比照 LINE），outbound 衍生 session token 保留有界 TTL；Odoo 全域／per-agent 憑證更新後下次呼叫即重連（`set_global` 改 `disconnect_all`，並修掉 profile 變更導致孤兒連線永不釋放的 bug）。仍需重啟：Discord／Slack（WebSocket 長駐連線持有 token）。
- **credentials P3 env 擦洗**：spawn 官方 CLI 子行程改用白名單 env（`duduclaw-core/spawn_env.rs`）——只傳 PATH/HOME 等非敏感必需項，濾除所有 `*_API_KEY`/`*_TOKEN`/`*_SECRET`/`*_PASSWORD`；vendor 金鑰改由呼叫端顯式注入（rotator 解析的帳號 env），子行程不再繼承 gateway 的 `ANTHROPIC_API_KEY` 等。型別層測試焊死白名單不得含密鑰形狀名字。**行為變更**：白名單刻意排除 `SSH_AUTH_SOCK`/`GNUPGHOME`——靠 SSH/GPG 從 spawn 出的 CLI 做 git push／簽章的 agent 會受影響（範圍窄，屬應經 per-agent 明確授權而非全域白名單放行的能力）。
- **secret:// resolver 收斂第二輪**：`account_rotator`（2 處）與 `duduclaw-cli/mcp.rs`（2 處）各自手刻的「`_enc` 解密→明文 fallback→`secret://` 參照」實作收斂到 `duduclaw-security::secret_ref` 單一 resolver；順帶修掉 Odoo 一處加密指標字面值外送洩漏（變嚴）。provider→env 名稱表三份收斂成一份權威（`duduclaw-core/provider_env.rs`）＋一致性測試，並修掉 `is_available` 誤報 TOGETHER/MISTRAL 可用的假陽性。

### Fixed
- **歸檔任務仍可被派工引擎認領**：`claimable_tasks` 未排除 `archived`，歸檔中的 pending／未認領任務理論上仍可能被 dispatch engine 撈走；已加 `AND archived = 0`，與 `list_tasks_filtered` 慣例一致。
- **needs_human 共用重試補備註欄（I-3c）**：`/goals` 卡片的重試早能帶備註，但收件匣／詳情頁共用的 `NeedsHumanActions` 三按鈕版本未暴露——現補上（重試改「展開備註→送出」兩段式，比照 /goals 卡片；後端本就支援 note，僅前端缺欄位）。
- **LINE 進度事件過濾缺口**：LINE 的進度轉發先前未過濾 `Step`／`ModelInfo`（`to_display()` 為空）事件，可能對 LINE push API 送空訊息；現比照其他通道過濾（順帶記錄 Discord 同缺此過濾，列後續）。
- **`estimate_tokens` CJK 校準**：從對所有字元一律 `chars/1.5` 改成依 codepoint 分類（CJK 1.306 tok/char、非 CJK 1/3.6，沿用 `duduclaw-llm` 既有 Unicode range），修正約 22% 低估——先前低估會讓 `[budget] max_input_tokens` 壓縮閘觸發過晚。
- **`--exclude-dynamic-system-prompt-sections` 空操作移除**：該旗標與 `--system-prompt-file` 併用時被 CLI 忽略（活測 total_ctx 帶不帶逐位相同），三處無效使用移除、修正基於錯誤前提的註解。
- **`handle_tools_list` 依 capability 過濾**：MCP `tools/list` 先前不論呼叫者 capability 一律送全部工具 schema；現在依呼叫者 `allowed_tools`/`denied_tools` 過濾（鏡像 dispatch gate，discoverable ⊆ callable），受限 agent 不再收到無法呼叫的工具宣告。（對 scaffold agent 的激進策展需 MCP 動態擴充/meta-invoke 新功能，DEFER 待拍板。）
- **docs/features 索引補齊**：37-47 號功能檔先前在英文 README（46-47）與 zh-TW／ja-JP README（37-47）索引缺席，已補齊。

## [1.60.0] - 2026-08-15 — 三 harness 借鑑收官——可換判官×Agent Mail×交付與附件安全×交辦 UX

### Added
- **needs_human 暫停原因封閉分類（pause_reason）**：goal 任務轉 `needs_human` 時，除了既有的自由文字 `judge_feedback`，現在同時蓋上一個六選一的封閉分類——`no_progress`（卡住沒進展）／`budget_exhausted`（次數或時限用盡）／`blocked_needs_decision`（等你決策）／`infra`（系統問題）／`restart`（系統重啟後暫停）／`unknown`（需要人工確認）。分類在**觸發現場**靜態標記，絕不從 `judge_feedback` 的 LLM 敘述反解——避免把模型自己的用詞當成路由依據。`/goals` 看板卡片、任務詳情頁、通道 needs_human 審批訊息（含 Observer 全自動模式的純通知）都新增這個分類 chip／一行「類型」；未分類或既有舊任務一律讀成「需要人工確認」（安全方向，不猜測），任務被人工決定（重試／完成／放棄）後分類欄清空，不殘留到下一次卡住。
- **逾時進度通報**：一個已被認領（`in_progress`）但超過 `[goal_loop] progress_report_minutes`（預設 10 分鐘，`0` 關閉）沒有任何可觀察進度訊號的目標任務，會收到一則「已執行 X 分鐘未回報進度」通知（Activity Feed ＋來源對話），同一任務每輪最多發一次。**純粹是提醒，不介入**：不會重派工、不升級、不取消，`stalled_secs`／`iteration_cap`／`wall_clock_hours` 仍是唯一會真的動手的護欄。
- **工具連擊 advisory**：同一輪內對同一工具、同一組（遮罩後）參數連續呼叫達 3／5／8 次，會在下一輪派工的 `<state>` 區塊注入一段逐級加重的提醒文字（3：建議先重讀上次結果；5：建議換方法；8：強烈建議收斂或用 `tasks_block` 求助）。零 LLM 成本、純 advisory——不會擋下、重試或否決任何一次派工，決策權留給 agent 自己。`[goal_loop] tool_streak_advisory`（預設 `true`）可整體關閉。
- **ephemeral spawn 准入排隊**：子代理（ephemeral spawn）撞到並發上限時，新設定 `[dispatch] admission`（**預設 `"queue"`，行為變更**）改成有界 FIFO 排隊而非直接拒絕——請求會持久排隊等候空位釋放，不再憑空消失。每張排隊券帶 TTL（`queue_item_ttl_secs`，預設 600 秒），逾期丟棄並落稽核；佇列本身有深度上限（`queue_max_depth`，預設 64）防止無界佇列反過來變成新的失控風險；來源 turn／session 結束時會批次作廢其名下所有還在排隊的票券；並發上限本身仍是「可調但不可為 0」（`ephemeral_max_active`，`0` 會被鉗到 `1` 並記警告）。想恢復先前「超限直接拒絕」的行為，把 `config.toml [dispatch] admission` 設回 `"fail"` 即可。
- **AI 團隊召喚卡片**：「AI 團隊」頁的產業團隊卡片新增團隊成員構成（AI 員工姓名＋職責摘要，逐條列出前台＋所有 worker）、明確保留給真人的崗位（「這些崗位留給真人：…」）、刻意不建 AI 的項目與原因、任務示例（優先用 `team.toml` 作者寫的 `examples`，沒有時退回真實 worker 摘要的前幾條，絕不虛構）。安裝動作文案從管理員視角的「安裝」改為使用者視角的「召喚這組團隊」／「加入我的團隊」，已安裝狀態可直接連到該團隊主管 agent 的頁面。
- **兩段式驗收裁決（two-stage judge）**：goal 任務進入 `review` 時，先跑一個便宜的第一階段評估——無工具、單次 LLM 呼叫、JSON 三值 `continue`/`candidate_complete`/`blocked`——只有判定為完成候選才會進到既有三面向 MAV 判官團，多數還沒做完的輪次不再需要付一次完整判官呼叫。`continue` 直接用評估器給的下一步當回饋重新派工（計入迭代上限，走既有駁回路徑）；`blocked` 直接轉 `needs_human`，不必假裝走完一輪判官。**任何故障（逾時／解析失敗／呼叫錯誤）一律降級直接跑 MAV 判官，絕不因為第一階段故障就自動通過或拒絕**。新設定 `config.toml [dispatch] two_stage_judge`（預設 `true`）。
- **驗收判官紀律條款**：MAV 判官 prompt 新增四條規則——反棘輪（驗收標準沒變時不得每輪找新毛病）、只稽核不自建證據（判官只能比對 agent 提交的證據與工具稽核摘要，不得自行想像或補寫）、反契約外擴張（不得拿驗收標準以外的要求當駁回理由）、agent 自稱完成不是證據。治的是「判官假陽性讓正確工作卡死」這個先前活測抓到過的失敗模式。
- **停滯偵測改用 gap 指紋比對**：新模組 `goal_gap_fingerprint.rs`，從駁回回饋抽取 `path:line` 引用與反引號關鍵詞、正規化（暫存路徑歸一、大小寫忽略、去重排序）成一組指紋，讓「換句話說的同一個 gap」也能被判定為同一次卡住，不再只有逐字相同才算。抽不到任何引用時退回既有逐字比對（行為相容）。連續兩輪同指紋才轉 `needs_human` 的門檻沒有變。
- **提前收工偵測（bail detection）**：新模組 `goal_bail_detect.rs`，九條 zh+en 正則比對 agent 回合最後一段非空文字（「無法繼續」「已放棄」「請稍後再來查看」「自簽 VERDICT」「已提交待審」……）。命中會記一筆 Activity Feed 事件、累加 Prometheus `goal_loop_bail_pattern_total{pattern}`，並把提示帶進下一輪派工的 `<state>` 區塊、第一階段評估器輸入、MAV 判官輸入——純粹是訊號與提醒，不會自己駁回或卡住任務。
- **重啟不自動復活（`resume_on_restart`）**：新設定 `[goal_loop] resume_on_restart = "auto" | "pause"`（機制首次上線時預設 `auto`、行為不變；同一批次內的預設翻轉見下方「Changed」）。設成 `pause` 後，gateway 每次啟動會把所有還在跑的 `goal_mode` 任務轉 `needs_human`（原因 `gateway_restart`），走既有的通道通知，不會在流程重啟或崩潰復原後悄悄接著跑一個沒人重新確認過的目標。
- **交接契約結構化欄位（`working_state_handoff`）**：新增可選欄位 `status`（`continue`/`complete`/`blocked`）＋`next_steps`／`evidence`／`blocker`，帶 `status` 時強制校驗——`continue` 要非空 `next_steps` 且不能有 `blocker`；`complete` 要非空 `evidence` 且不能有 `blocker`/`next_steps`；`blocked` 要非空 `blocker`。合併後總位元組數超過 `config.toml [memory] working_state_handoff_max_bytes`（預設 16384，CJK 安全計算）**整筆拒絕，絕不截斷**——截斷可能剛好刪掉讓交接可信的那段證據或下一步，卻仍看起來像一份權威交接。純文字交接（不帶任何新欄位）行為完全不變。
- **needs_human 決策卡「變更」分頁（批准前看得見改動）**：收件匣的「等你決定」卡片與任務詳情頁新增「變更」分頁，列出這個任務歷輪實際動過的檔案——路徑（可一鍵複製）、操作類型（新建／覆寫、修改、刪除、指令）、時間、輪次、成功或失敗，以及沿用稽核紀錄遮罩結果的摘要片段。證據來自兩條既有軌跡：執行期原生工具事件（Write／Edit／NotebookEdit／檔案效果明顯的 shell 指令）在每輪派工後落成 `task_changes.jsonl`（以任務 id 歸屬、0600、8MB 輪替、每輪上限 50 筆），MCP 稽核紀錄（`shared_wiki_write` 等）則沿用判官 `<tool_activity>` 同一套「認領→驗收時間窗＋執行者」歸屬。失敗與被攔截的呼叫一併列出並標記——那正是即時查詢工具狀態看不到的一半。**查無即誠實回空**：沒有紀錄時顯示「此任務沒有留下檔案變更紀錄」，不以敘述硬湊。新 RPC `tasks.changes`（唯讀，與 `tasks.comments`／`tasks.iterations` 同一套任務範圍權限閘）。目前顯示「動過哪些檔案」，尚非逐行 before/after diff（需寫入前快照，列為後續項）。
- **目標契約凍結＋四要素引導**：`/goal` 與 `tasks.goal_create` 建立目標時，把驗收標準凍結成不可變的 `acceptance_criteria_baseline`；判官與第一階段評估器一律讀這份 baseline，不讀事後可能被改動的欄位。只有儀表板操作者可經 `tasks.update` 編輯顯示用的可變副本（新能力——但這不會回頭改動判官仍在檢核的 baseline，是刻意設計，避免驗收門檻被悄悄放水或收緊）。`/goal` 沒帶 `||` 驗收標準時仍照常建立任務，但確認訊息會多附一段「目標／輸入／輸出格式／約束」四要素引導與 3-5 條 outcome 式驗收標準建議；`planner_enabled` 的子任務拆解 prompt 也同步補上同一套契約紀律（寫結果不寫作法、標準精簡到 3-5 條、範圍外事項列為 Non-goals 而非驗收項）。

- **統一交辦面板（個人版終於有交辦按鈕）**：側欄、手機底欄中央鍵、任務板、目標管理台、AI 員工卡片與詳情頁——所有「把工作交給 AI 員工」的入口現在都開同一個交辦面板；個人版先前完全沒有主要動作按鈕的缺陷一併修正。面板提供四要素引導（目標／輸入／輸出格式／約束含完成標準）、AI 員工選擇、「問一問」（純對話不改檔）與「交辦」（啟動目標迴圈、走驗收判官）雙模式、驗收標準與輸出格式欄（輸出格式併入驗收標準，交由判官檢核）。全站動詞統一為「交辦任務」；目標管理台與員工頁的兩個重複表單移除。
- **已完成／失敗的目標任務可以「接著做」**：任務詳情頁對已結束的目標任務提供補充說明輸入框，送出後任務帶著你的補充重新開一輪（迭代計數延續、仍經驗收判官檢核）——不必為了追加需求另開新目標。
- **「AI 團隊」頁對所有人開放**：原 `/experts`（產業專家包）從 admin 限定、個人版進階區底部，升級為一般層級主選單項目並改名「AI 團隊」——22 組產業團隊劇本的一鍵安裝入口不再藏在管理員深處。
- **憑證衛生卡（安全頁）**：偵測 `config.toml` 中的明文憑證殘留（只回報鍵路徑，絕不回傳值）；對「已有加密孿生」的明文欄位提供一鍵清理（自動備份、原子寫回、審計記錄、冪等）；附 OAuth token 輪替指引。設定檔損毀時 fail-closed，絕不假綠、絕不誤蓋。
- **任務產物物件化（I-2b）**：新增 provenance ledger `artifacts.jsonl`，記錄 `attachments/` 底下每個檔案的來源——五種 origin：`declared`（AI 員工主動用 `📎DELIVER:` 交付）、`swept`（忘記標記、被既有的自動回收網撈到）、`uploaded`（真人上傳）、`produced`（執行中產出，讀側推導不落地）、`unknown`（證據不足，誠實承認不知道）。方向一律在**寫入當下**記錄，不事後用時間窗猜測；任務歸屬分兩級並標示清楚——`exact`（紀錄本身就帶任務 id，或任務自己的變更紀錄點名了這個檔案）與 `inferred`（只靠「認領→驗收」時間窗慣例推定，與 `tasks.changes` 用同一套窗口），UI 絕不把推定當事實呈現。合併鍵改用真實 basename（原本的 CJK sanitize 顯示名稱可能把兩個不同來源的檔案誤併成一筆）。開機時的冪等回填只採信既有證據（`office_docs.rs` 的宣告/回收紀錄、`task_changes.jsonl`），對證據不足的舊檔案一律留 `unknown`，不猜方向。新增任務詳情頁「產物」分頁（可複用元件）；`/files` 頁新增「來源」欄與依任務篩選。goal 派工路徑的封存副本已在同版補上（見下方「goal 任務產物封存副本」條目）。
- **靈感畫廊 /gallery（P2-b，newIn 1.60.0）**：curated MVP——把既有 22 組產業團隊劇本 `team.toml` 裡的任務範例（`examples`）扇出成一張張成果卡，每張都能「做一個同款」：一鍵預填交辦面板（目標描述帶入範例文字並提示依實際狀況調整對象/時間/資料來源，驗收標準帶入「已完成且留下具體結果可查核」），已安裝的團隊直接開交辦面板，未安裝的先導向「AI 團隊」頁加入。新 RPC `gallery.list`（admin only，與 `experts.catalog` 同一套 license／已部署套件判定，沒有 premium 樹時 fail-safe 回空清單而非報錯）。這波刻意不做的：使用者自己完成的目標任務被收進「我的」畫廊——那需要先有產物物件化（I-2b）才有素材可展示，留待下一波。
- **「想一想」計畫模式（I-1c）**：交辦面板新增第三種模式，與既有「問一問」「交辦」並列。選「想一想」時，`tasks.goal_create` 帶 `plan_first: true`：後端先同步呼叫工具用 LLM 產出一份 3-8 條的執行計畫（純文字、非 JSON），任務直接誕生在 `needs_human`（沿用既有的 `blocked_needs_decision` 分類，沒有新增分類）等你核准——核准前不會跑任何一輪派工。計畫存在新的 `plan_pending` 欄位，刻意與 `judge_feedback` 分開（核准動作那個既有的「重試」按鈕會覆寫 `judge_feedback`，若共用同一欄位，核准當下計畫就會被自己的核准動作洗掉）；核准後的第一輪派工會把計畫包成 `<execution_plan>` 區塊注入提示，且只消費一次——後續輪次不會重複貼同一份計畫。核准動作**沒有新增按鈕種類**，用的就是任何 `needs_human` 任務既有的「重試」。計畫本身不是免驗收的保證，執行結果仍要通過既有的驗收判官。**規劃器本身失敗時 fail-closed**：任務照樣停在 `needs_human`，但分類改成 `infra`（系統問題），且不帶 `plan_pending`（沒有計畫可注入），絕不悄悄放行成自動執行。
- **goal 任務產物封存副本**：goal 任務通過驗收（accept）時，把歷輪實際寫出的檔案複製封存進該 AI 員工既有的 `attachments/` 目錄——產物分頁與 `/files` 頁因此有真正可下載的副本，不再只顯示寫入路徑（補上「任務產物物件化」原本的已知限制）。封存有硬規則：來源路徑 canonicalize 後必須落在該員工工作區內（symlink 逃逸直接拒絕）、單檔 20MB／單次結算總量 100MB 上限（超過記 skipped 並留警告，不靜默）、封存失敗只記錄絕不影響任務判定結果；重跑冪等不重複封存。刻意只在 accept 時封存，修訂輪迴中不做（避免磁碟 churn）；`needs_human`／`reject` 任務維持原本「有寫入路徑、無下載連結」的呈現。
- **憑證後端 P1**：`secret://` 參照新增兩個本機 backend——`secret://keychain/<service>/<account>`（OS 原生鑰匙圈，`keychain` feature，feature 未開啟時明確報錯而非靜默查無）與 `secret://file/<path>`（Docker secrets／K8s projected volume 掛載檔：固定 root 集、canonicalize containment 擋 symlink 逃逸、64KB 上限、world-writable 一律拒絕）。`[[tick.sources]]` 的 `headers` 支援 `secret://`（逐請求解析、解析後重新驗證 CR/LF 防 header injection、不可解析整條丟棄，絕不把參照 URI 當值送出）。新增「憑證來源總表」：安全頁卡片＋`security.credential_inventory` RPC（admin 限定，只回報鍵路徑與來源分類，絕不回傳值；`[mcp_keys]` 整段跳過——它的表格鍵名本身就是金鑰）；`duduclaw doctor` 新增憑證體檢列與 `--fix-residue` 互動式明文殘留清理（逐條確認、先備份、原子寫回，刻意不提供非互動旗標——刪憑證是使用者關卡）。
- **產物交付閘（delivery gate）**：AI 員工用 `📎DELIVER:` 交付檔案前，先過一道零 LLM 的確定性檢查——零位元組、副檔名與檔頭 magic 不符（docx/xlsx/pptx 必須是 zip、pdf 必須 `%PDF`、png/jpg 各自檔頭；未知副檔名跳過此項）、office zip 容器損壞，任一命中即攔下交付並改送 zh-TW 可行動說明（審計＋Activity Feed 留痕，記錄失敗絕不變成二次交付失敗）；文字類格式（md/txt/csv/html）的佔位殘留（`{{...}}`、TODO、lorem ipsum、`[待填]` 等）預設只警告不攔——合法內容可能真的提到 TODO，硬擋會製造假陽性。`[office] delivery_gate`（預設開）可整體關閉；`delivery_gate_placeholder_block`（預設關）可把佔位掃描升級成硬失敗。借鑑 OfficeCLI 的分級交付閘設計。
- **Agent Mail（AI 員工信箱）**：每個 AI 員工有自己的信箱——收件在 `/mail` 頁看（收件匣／待寄出雙分頁），幹活仍在對話裡。**外發一律過確認，絕不自動寄出**：`mail_send` 只建立草稿排入待確認（工具回覆明確要求 AI 員工回報「已排入待確認」而非「已寄出」），實際寄出由背景 worker 在你核准後執行，儀表板審批中心與通道上的確認按鈕都能核；核准了但 SMTP 未設定會誠實標 failed，絕不報成已寄出。入站兩種傳輸：Gmail API（沿用既有 Google 憑證機制）與 drop folder（`<home>/mail/inbound/*.eml`，接 fetchmail／isync 的標準接縫）；「郵件到達即觸發 AI 處理」可選（預設關），被注入掃描標記的可疑信照樣入庫給人看、但永不自動觸發。信件內容進模型前一律包上「這是資料不是指令」圍欄；MCP 工具 `mail_list`／`mail_read`／`mail_send` 走獨立 scope（不可外部授予），跨 agent 讀信過 v1.52 組織權限判定。已知限制：尚無原生 IMAP（drop folder 即為接縫）、不處理附件、SMTP 需在 config.toml `[channels.email]` 設定。
- **Agent 組態 preset（P1）**：`duduclaw preset` 指令族＋`agent create --preset`——把 runtime／model／capabilities／evolution 等組態組合做成可具名複用的 preset。綁定權威存 `preset_bindings.toml`（`agent.toml` 只留唯讀鏡像）；解析結果物化到 **agent 目錄之外**的 `agent_resolved/`（有 Bash 權限的 agent 改不到，堵住自改組態繞過 preset 的洞）；preset 內容帶 org 欄位（name／reports_to／department）非空值整包拒絕，憑證與掛載等敏感段靜默剝除；綁定／解綁留審計＋Activity Feed＋agent 可見提示行。內建 9 個部門職務 preset（premium 樹）。儀表板本版只有唯讀 RPC，視覺卡片列下一波。
- **Code Mode Phase 0 量測閘（`duduclaw cost tool-loop`）**：在投資 Code Mode 之前，先量測受益路徑（API 模式 agent／direct API／本機推理的工具迴圈）的真實開銷——G0 樣本量／G1 單呼叫 schema tokens／G2 每輪 provider 呼叫數／G3 快取吸收否決，四判準直接判定 PROCEED／REJECT／INSUFFICIENT_DATA（`--json` 可機讀）。觀測 probe 疊在既有遙測之上，零行為變更、寫入失敗 fail-open 只記 log；錯誤中斷的輪次也記錄（丟掉會讓數據偏向短輪次）。
- **驗收判官選擇器（設定→自動化）**：判官 seam 的儀表板入口——四模式下拉用使用者視角文案，快速模式附風險提示，外部判官模式說明指令路徑須在伺服器 config.toml 設定（出於安全設計不提供輸入欄位）；改變下一次驗收即生效，不需重啟。外部判官子行程繼承 gateway 環境變數一事已在指南誠實揭露。
- **`duduclaw evolution clear-holdout-rotation`**：AEE 的 holdout 輪替旗標先前被系統舉起後**沒有任何介面能放下**（唯一的清除方法全 repo 零呼叫端）；新增操作者 CLI 出口（`--dry-run`、冪等、審計留痕）。同時每輪 AEE 紀錄快照 14 個 harness 旋鈕值進 evolution events（把宣告了卻從未發送的 `aee_round` 事件真正接上）——未來要評估任何旋鈕調整，終於有歷史資料可比。
- **驗收判官可替換（judge seam）**：新設定 `[dispatch] judge = "mav"（預設）| "evaluator_only" | "external" | "human_only"`。`mav` 與先前行為逐位相同；`evaluator_only` 只跑第一階段便宜評估器（明確定位為低成本模式、接受較弱驗收，evaluator 故障一律轉 needs_human 絕不自動通過）；`external` 把裁決交給你指定的外部指令（`judge_command`，stdin 餵結構化 JSON、stdout 收 verdict）——spawn 失敗／逾時／非零離開碼／壞 JSON 全部**降級回 MAV 判官團**（降級＝變嚴）並記審計，外部判官的 feedback 文字視為未受信資料、先截斷再過注入掃描才進下一輪 prompt；`human_only` 每個完成候選一律轉 needs_human 等人工驗收。未知值回退 `mav`（最強驗收，非最便宜）；`system.update_config` 只收列舉的 `judge` 值，`judge_command` 刻意不開放 RPC（它指定可執行檔）。「一切皆插件」的第一個真 seam。
- **儀表板憑證欄位「來源」選擇器**：通道設定／帳號／Odoo／本機推理四個表單的憑證輸入框旁新增來源下拉（直接輸入／環境變數／OS 鑰匙圈／掛載檔案／秘密管理服務），選定後引導式填寫、自動組出 `secret://` 參照，既有 `secret://` 值載入時自動反解回對應模式。純前端組字，後端能力第四波已就緒，本次補上使用者看得見的入口。
- **任務詳情改分頁式（產物／檔案／變更／過程）**：任務詳情頁下半部從直式堆疊改為四分頁——「產物」「檔案」（該 AI 員工檔案空間中與此任務相關的檔案，含來源分組）「變更」「過程」（對話＋活動合併成單一時間流）。產物與變更分頁帶計數 badge；分頁切換保留捲動位置與已載入資料（不重打 RPC）；needs_human 決策卡位置不變，審批動線零增加；手機版分頁橫向可滑。
- **預算耗盡改交「最佳輪成品」**：goal 任務因迭代上限／時限／判官重試預算耗盡轉 needs_human 時，不再空手升級——從歷輪紀錄**確定性**挑一個最佳輪（優先序：最後一個真的進過判官團的輪→駁回 gap 最少的輪→最後一輪），把該輪成品節錄＋還差哪幾條 gap 附進升級通知與任務詳情，升級訊息從「我做不完」變成「這是第 N 輪成品＋差距清單」。零 LLM 成本；一輪都挑不出來時維持原樣空手升級，誠實不硬湊。新欄位 `task_iterations.worker_excerpt` 在駁回當下快照成品節錄（`result_summary` 先前每次駁回都會被清空，事後救不回來）；判官重試預算（`max_retries`，預設值下實務最常打到的耗盡點）路徑一併接上。借鑑 AutoDesign（arXiv 2608.13560）。

### Changed
- **README 三語首句換新定位**：從「24 小時值班的 AI 助理」改為「交得出東西的 AI 員工：常駐九個通訊軟體、交件前有獨立判官驗收、花費記帳」（英日版重新翻譯非直譯）。
- **任務板「新增任務」降為次要按鈕**：純記錄用途的建卡入口保留但視覺降級，主要動作讓位給「交辦任務」。
- **`resume_on_restart` 預設由 `auto` 改為 `pause`**：`[goal_loop] resume_on_restart`（見上方「重啟不自動復活」）先前以 opt-in 上線、預設 `auto`（行為不變）；現在預設翻轉成 `pause`——gateway 每次重啟（非預期崩潰、部署重啟）都會把還在跑的 `goal_mode` 任務轉 `needs_human`，等你按下重試才會繼續，不再悄悄接續一個沒人重新確認過的目標。想改回舊行為，把 `config.toml [goal_loop] resume_on_restart` 設回 `"auto"`，或到儀表板「設定 → 自動化」的「gateway 重啟後的進行中目標任務」切換（`system.update_config` 已加入白名單，只收 `"auto"`/`"pause"` 兩值）——這項設定只在 gateway 下次真正重啟時生效，儲存當下不會立即套用。
- **`[dispatch] two_stage_judge` 預設開啟**：goal 任務 `review` 流程從單段式判官預設變成先跑第一階段評估器才進 MAV 判官團（見上方「兩段式驗收裁決」）；設 `= false` 可退回舊行為。
- **MAV 判官 prompt 套用新紀律條款**：無 config 開關，即刻套用到所有 goal 任務（見上方「驗收判官紀律條款」）。
- **停滯偵測的比對依據**：從駁回回饋逐字比對改成 gap 指紋比對，無 config 開關（見上方「停滯偵測改用 gap 指紋比對」）——同一卡點換句話說也會被抓到，可能比過去更早觸發 `needs_human`。
- **agent 不能再修改自己 goal 任務的驗收標準**：先前 agent 身分呼叫 MCP `tasks_update` 可直接改掉 goal 任務的 `acceptance_criteria`；現在一律拒絕並留審計紀錄（`goal_contract_frozen`，見上方「目標契約凍結」）。
- **`agent.toml` 影子讀取器遷移第二期**：`capabilities`／`budget`／`evolution`／`skills`／`mcp.external`／`os_watch`／`pty`／`agent` 各區段的手刻 `toml::Value` 讀取全部收斂進共用的型別化解析點（延續上一波 `agent_update` 資料遺失修復的同一條清理路線，純內部重構）；唯一使用者可觀察到的語意調整是 `allowed_tools`／`denied_tools` 混合型別陣列時，兩條讀取路徑現在一致改成「丟棄壞元素、保留字串元素」，不會再因為陣列裡混了一個非字串值就讓整個 agent 讀取失敗。
- **`accounts.add` 不再寫明文孿生鍵**：新增帳號時加密成功只寫 `<field>_enc`，不再同時寫入明文欄位（加密失敗才降級寫明文並記警告）——先前「明文與密文並存」正是憑證殘留事故的製造者。讀取端本來就以 `_enc` 優先，功能不受影響，但 `config.toml` 寫出的形狀與先前不同。
- **credentials 同步路徑 async 化收尾**：per-agent channel token 啟動路徑（telegram／discord／slack）、reports_to token 級聯、DM 候選鏈、dispatcher 跨頻道轉發、本機推理 api_key 讀取全部轉 async resolver——`secret://vault/…` 等網路 backend 參照現在在這些路徑也能解析（先前只有 env／keychain／file 本機 backend 可用）。sync 版本保留給未來可能的真同步死角並如實記錄，生產呼叫端已清零。`accounts.add` 的「只寫 _enc 不寫明文孿生鍵」行為補上回歸測試（純函式抽取＋端到端各一組，含既有帳號逐位元組不變）。
- **AEE 冠軍 bootstrap 改為同形量測**：先前 bootstrap 冠軍用「不含 held-out」量測、候選卻含 held-out——第一輪比較蘋果比橘子，且該輪的 held-out 圍欄因冠軍側缺維被跳過，「可見集收益償付保留集損失」的候選在第一輪可判成 improves 並提交（反向驗證實錘）。現在 bootstrap 也量 held-out 子集（零 LLM 成本），第一輪起圍欄即生效；既有舊格式冠軍快照沿用單邊跳過語意不炸、下一次提交後自癒。考據：原本的 `false` 是繼承自首版的保守預設，查無刻意理由。
- **agent_id 驗證同族第二期收斂**：agent IPC／記憶聯邦信任存放區的兩份逐位元組重複實作收斂到 core 權威版；憑證 vault 金鑰檔名的 agent_id 長度上限從 128 收斂到 64（考據無刻意理由、呼叫鏈上游已鎖 64、無存量風險，方向變嚴）；兩處與行為不符的過時註解（action_claim 的 regex 差異、identity_token 聲稱禁底線）改為誠實描述，行為不動。
- **AEE 驗收閘拆 visible／held-out 兩維（防遮蔽）**：playbook 候選的 commit 比較先前把 held-out 與可見 eval case 混成單一 `cases` 平均——「可見集大幅改善、保留集小幅退步」的候選只要混合平均落在 noise band 內就會 commit，held-out 的防 gaming 意義被平均稀釋。現在 `dimensions()` 並排新增 `cases_visible` 與 `cases_holdout` 兩維：holdout 用更嚴的 band（預設取生效後主 band 的一半、顯式值鉗不得寬於主 band，`[evolution.noise_band] holdout` 可設定），且兩個新維**只能否決、不能晉升**——不會把 `Matches` 拱成 `Improves` 而順手重置 anti-drift 計數、關掉觀察窗。settle 期的全域退步柵欄（`suite_verdict`）同一問題同一修法，band 在 commit 當下凍結進觀察窗（settle 不回頭讀可能已被改動的 config）。無 held-out case 的 agent 行為與先前逐位相同。借鑑 AutoDesign 的雙集獨立條件；缺口由調研以論文為鏡頭讀源碼實錘。
- **agent_id 驗證器五份手刻拷貝收斂成雙權威版**：兩份與 core 廣義版逐位元組相同的（autopilot／files_api）直接收斂；三份「小寫 slug」規則（handlers／MCP／CLI）經逐條語意對照確認是刻意的產品契約（錯誤訊息早已如此承諾），升格為 core 第二權威版 `is_valid_new_agent_id` 後三處委派。telegram chat_id 的兩處內嵌判斷經查是刻意不同（打字指示須支援群組負數 id、Mini App 僅限私聊正數 id），不予統一。
- **`agent_update` 覆寫時刪除未型別化 `agent.toml` 區塊的資料遺失 bug**：`[runtime]`／`[guardrails]`／`[os_watch]`／`[fork]` 整段，以及 `[capabilities]` 的 `scoped_tools`／`grant_ttl_secs`／`approval_required_tools`／`irreversible_tools`／`maybe_irreversible_tools`／`autonomy_level`、`[model]` 的 `fallbacks`／`standard`／`delegation_routing` 這些欄位過去只被少數模組用手刻 `toml::Value` 讀取，型別化的 `AgentConfig` 完全看不到它們——經 MCP `agent_update` 對 `AgentConfig` 重新序列化寫回 `agent.toml` 的任何一次編輯（哪怕只是改個 icon）都會把這些欄位整段洗掉。現在全部收斂進 `AgentTomlSections` 單一型別化解析點，`agent_update` 的寫回不再遺漏未被它自己編輯的區塊；5 個影子讀取模組（`mcp_fork.rs`／`capability_grants.rs`／`guardrail.rs`／`os_events.rs`／`runtime_config.rs`）改走新解析點，逐欄位鎖了缺鍵方向的回歸測試（含刻意保留的歷史怪癖，如 `[model] preferred` 帶整數字面值會被忽略）；讀取本身刻意不加快取，維持原本「每次呼叫即時讀檔」的行為不變。全專案影子讀取器實際盤點 62 處／16 檔，本輪遷移 5 檔，其餘清單留待下一輪。
- **googlechat／teams AI 員工被靜默跳過 needs_human／evolution 通知**：`goal_notify` 的 bot token 判定先前只認一般通道的單一欄位 token，而 Google Chat／Teams 這類「自我設定」通道的憑證是多欄位、只存在全域 `config.toml`（不走 per-agent `agent.toml [channels]`），判定邏輯因此永遠讀不到 token，把「其實已設定」的通道當成「未設定」，needs_human 審批、逾時通報、GVU 演化通知全部靜默不送。現在改用與 `cron_scheduler.rs` 一致的「檢查標記欄位是否存在」判定。
- **提醒（reminder）發送改走統一的十通道發送工廠**：先前只手刻 Telegram／LINE／WhatsApp 三通道，其餘七個通道的提醒送出後靜默失敗（無錯誤、也無送達）。現在改走與其他通知路徑共用的 `create_sender` 工廠，十通道全數支援；WebChat 明確拒絕（無持久連線可送，直接回錯誤而非假裝送達），避免背景排程回報「已送出」卻其實沒有接收者。
- **驗收判官 JSON 面板截斷時的假陽性風險**：面板 JSON 被截斷成不完整片段時，先前會落回舊版單一 `PASS`/`FAIL` 掃描器，若片段裡剛好帶著 `pass` 字樣就可能誤判通過；現在截斷或畸形 JSON 一律直接判定失敗（fail-closed），絕不再落到舊版掃描器。
- **驗收判官 `PASS` 誤判**：舊版掃描器只要回覆第一行「任何位置」出現 `PASS` 字樣就算通過（例如 `[THE, RESULT, DOES, NOT, PASS, …]` 這種列表也會被誤判為通過）。現在要求 `PASS` 必須是第一行「開頭」的 token 才算數。

- **Google Chat／Teams 發送器靜默缺臂**：`create_sender` 工廠缺這兩個通道的分支，部分路徑（如 cron 交付）會落入 NullSender 靜默不送；同時修正 cron 發送前的憑證檢查對多欄位憑證通道（googlechat/teams）誤判為「未設定 bot token」而拒送的問題。
- **autopilot notify／MCP `send_message` 補齊十通道**：`resolve_channel_target` 統一抽到 `channel_sender.rs` 單一來源（`reminder_scheduler` 改成 re-export，行為不動）。Autopilot 規則引擎的 `notify` 動作先前硬編碼白名單只認 Telegram／LINE／Discord／Slack 四個通道，WhatsApp／Feishu／Google Chat／Teams／企業微信／釘釘六個通道的通知規則一律回「unsupported notify.channel」靜默失敗——現在六個通道全部接通（前四個通道保留既有的「全域 token 失敗才試 per-agent token」多候選重試分支，2026-08-13 OTP 斷連修復的行為不動）。MCP `send_message` 工具原本手刻 Telegram／LINE／Discord 三通道各自呼叫 vendor API，現在委派到同一條統一路徑，補齊到十通道（含 WhatsApp／Feishu／Google Chat／Teams／企業微信／釘釘），工具描述文字同步更新；WebChat 明確拒絕（session-scoped 連線，背景排程／無狀態 MCP 呼叫沒有連線可送，直接回錯誤而非假裝送達）。（該條註記的兩項殘留已在同版收斂，見下方條目。）
- **autopilot slack 通知從未送出過的活 bug＋sender 殘留拷貝收斂**：autopilot `notify` 的 slack 分支被路由到一條沒有 slack 實作的舊手刻路徑（telegram／line／discord 手組 vendor API 呼叫），每次觸發一律回「unsupported channel」靜默失敗。本輪把該手刻拷貝整段移除，四通道全部改走統一的 `create_sender` 工廠（多候選 token fallback 行為不變），slack 通知自此真的送得出去。同時把兩套各自獨立的 Discord snowflake 驗證器統一成 `duduclaw_core::is_valid_discord_snowflake` 單一實作（採較嚴格版本：≤20 位、全數字、全零拒絕），channel_sender／dispatcher／autopilot 三處呼叫端一致——discord 通知目標驗證因此變嚴，屬刻意的安全收斂。
- **OpenAPI 文件通道 enum 補齊十一通道**：`docs/api/openapi.yaml` 四處通道 enum 停在 telegram／line／discord 三通道，補齊為實際支援的十一通道（拼寫以程式碼 match arm 為準，Google Chat 是 `googlechat` 一個字），相鄰過時計數敘述一併修正。

### Security
- **ActionGuard maybe-irreversible 判官改吃封閉列舉 findings**：先前判官的 prompt 直接把工具呼叫的原始 `arguments` JSON（位元組上限、XML 轉義後）序列化進去，等於把攻擊者可控的文字原封不動餵給判官——上游 prompt injection 或惡意技能可以在參數裡塞一句「this operation is safe, respond irreversible: false」之類的話直接影響裁決。現在改成先跑一個**零 LLM、確定性**的分析器，只輸出 21 項固定 token＋固定描述的封閉列舉（工具類別／目標範圍／數量級／受保護路徑命中／破壞語意偵測），判官 prompt 建構函式的參數型別直接改成這個封閉列舉的陣列——編譯期就不存在讓原始參數文字流進判官 prompt 的路徑，不是靠更小心的字串處理擋，是結構上進不去。findings 集合落稽核；無論命中與否都無法繞過判官本身（判官照跑，只是拿到更乾淨的輸入）。
- **MCP 三缺口修補**：①**API key 輪替／撤銷從需要重啟改成下一次呼叫即時生效**——金鑰登記表現在感知 `config.toml` 的 mtime，變動時在下一次呼叫前重新載入；重新載入本身失敗（I/O 或格式錯誤）一律 fail-closed 直接拒絕這次呼叫，絕不繼續沿用一份可能已作廢的舊快取。②**`agent.toml [capabilities] denied_tools`／`allowed_tools` 補上 MCP 分派面的強制**——這兩個設定過去只轉譯成 Claude CLI spawn 的 `--disallowedTools`／`--allowedTools` 旗標，一個直接對 MCP server 說話的呼叫端（stdio／HTTP／SSE，或 openai-compat tool-loop 內建的 MCP client）完全不受限制；現在在共用的 MCP 分派總門強制執行，精確比對工具基底名稱（自動剝除 `mcp__<server>__` 前綴），`denied_tools` 恆贏過 `allowed_tools`，語意與 CLI 旗標一致。③**scope／grants／denied 三類拒絕全部落稽核**（`tool_calls.jsonl` 新增 `error_class` 欄位）——先前「失敗不留痕」的缺陷族在權限拒絕這一類再補上一處。
- **WhatsApp webhook 簽章驗證改為 fail-closed**：先前 `app_secret` 為空時會完全跳過簽章驗證、照常處理 inbound 訊息（webhook 對外裸奔）；現在空 secret、缺簽章 header、驗章失敗一律回 401 拒收，並在啟動時對「已啟用但未設 App Secret」的通道記明確警告。**行為變更**：沒設 App Secret 的部署升級後會收不到 WhatsApp 訊息，到儀表板通道設定補上 App Secret 即恢復——這是刻意的 fail-closed，與 LINE／Feishu 既有行為對齊。
- **`system.config` RPC 遮罩補上巢狀遞迴**：敏感欄位遮罩先前不深入 `[[accounts]]` 這類 array-of-tables，其中的 `oauth_token` 等值可經儀表板 RPC 原樣讀出；現在遮罩完整遞迴 tables／arrays／arrays-of-tables。全 repo 同類掃描確認其餘結構化遮罩實作皆已正確處理陣列，此為唯一缺口。
- **憑證讀取單一化，修 `secret://` 參照字面值被當真憑證送出的 bug（SecretRef／Secret 型別，WP-H1 P0）**：過去有七套各自手刻的「先試 `<field>_enc` 解密、失敗退回明文」實作，其中只有兩套認得 ADR-004 就已支援的 `secret://<backend>/<name>` 參照語法——其餘同步呼叫路徑會把整串 `secret://vault/foo` 原樣送給 Telegram／Discord／Feishu 等 vendor API 當成憑證本身。設計文件盤點出的四條路徑之外，本波驗收段抽查又自掃出 dispatcher.rs 的第五份平行實作（跨頻道轉發／callback 的全域 token 讀取），一併修正；同時刪除 Discord 啟動流程裡的第四份手刻副本與六處各自重複的空值檢查。新增 `Secret` 型別（`Debug`／`Display` 一律印 `<redacted>`、drop 時 zeroize、無 `Serialize`、不能持有空字串）與 `SecretRef`／`SecretStatus`，`describe()` 系列 API 只回報「有沒有設定／來源／能不能被儀表板寫入／是否有明文殘留」，本身從不持有值。所有同步呼叫路徑遇到需要網路 backend（Vault／1Password／Infisical）解析的參照一律 fail-closed（視同未設定＋記警告），非同步路徑正常解析（再改成全面 async 化列為 P1）。13 個行為等價測試鎖住每條路徑改寫前後結果相同。
- **`system.config` 補上 `[mcp_keys]` 鍵名遮罩洞**：`[mcp_keys]` 把每把內部 MCP API key 存成**表格鍵名**本身（如 `[mcp_keys."ddc_prod_a1b2c3d4e5f6"]`），既有的敏感欄位遮罩只改寫「值」，鍵名原樣通過——經 `system.config` RPC 讀出整份 `config.toml` 時，每把內部管理權限金鑰都是明文可見。新增 `mask_keyed_secret_tables`，用與 `mcp_keys.list` RPC 相同的遮罩規則改寫鍵名本身，讓兩個管理介面看到一致的遮罩結果；遮罩後鍵名碰撞時附加數字後綴，不會讓筆數悄悄變少。
- **wiki 刪頁 main-agent 判定改型別化角色檢查**：`shared_wiki_delete` 原本用「整份 `agent.toml` 檔案內容做無錨定字串比對」判斷呼叫者是不是 main agent（`content.contains("role = \"main\"")`）——任何 agent 只要 SOUL.md 或註解裡剛好含有這串字面文字，就能被誤判成 main agent，取得跨作者刪頁權。改成解析型別化的 `[agent] role` 欄位（走共用的 `duduclaw_core::agent_toml` 解析點），並補上雙向回歸測試（冒充字面值必須被拒絕、真正的 main agent 必須仍能刪除）。這是本波驗收段抽查發現的既有洞，不在原規劃範圍內。
- **`secret://` 參照第 6、7 種方言收斂（兩個真洩漏 bug）**：Google Apps Script bridge 的共享密鑰讀取先前「解密失敗就原樣回傳」，`secret://vault/…` 參照會被原封不動當共享密鑰 POST 給部署的 script；本機推理設定的 `api_key` 明文分支同樣原樣回傳，參照會被當 bearer token 送給 vendor API（除了 401，還把 secret backend 佈局塞進 Authorization header 外洩）。兩處都收斂到 `SecretRef` 單一 resolver；Apps Script 的 `BridgeConfig.secret` 同時改用 `Secret` 型別（先前 derive 的 `Debug` 會把完整共享密鑰印進日誌）。至此設計文件盤點的七種解密方言全數收斂。
- **`resume_on_restart=pause` 誤攔從未開跑的排隊任務（活測抓到）**：boot 掃描先前把 `todo`／`pending`（使用者建立後還在排隊、一輪都沒跑過）的 goal 任務也轉 `needs_human`——契約寫的是「還在跑的任務」，排隊任務在建立時就被使用者確認過，重啟後照常開跑不是「悄悄接續未確認的執行」；被攔等於每次部署重啟都要把排隊目標重新核准一遍。掃描範圍修正為真正中途的四種狀態（`revising`／`in_progress`／`review`／`blocked`），排隊任務重啟後正常派工。本修正由活體驗證（真 gateway 重啟）抓到。
- **multi-runtime API 模式的 `secret://` 字面值洩漏（同款第 8 處）**：`runtime/openai_compat.rs` 的 provider 憑證解析（API 模式 Grok／DeepSeek／MiniMax agent 走的那條 multi-runtime 路徑）先前沒有任何 `secret://` 檢查——參照字面值會被當 bearer token 直接送給 vendor API。收斂到 SecretRef 單一 resolver；回歸測試連「錯誤訊息也不得出現參照字串」都鎖住，並防護開發機環境變數造成的測試假綠。
- **潛伏地雷模組移除**：`duduclaw-security::key_vault`（`resolve_agent_keys`）零生產呼叫端且實作危險——`_enc` 欄位連解密都沒做、直接把 ciphertext 字串當 key 回傳，哪天被接上就是無法察覺的憑證故障。三重確認零呼叫端後整檔刪除；`duduclaw-cli` 的死函式 `decrypt_api_key_from_config`（手刻解密＋明文 fallback、無 secret:// 檢查、零呼叫端）同刪。修復不如刪除：為零驗證的介面背書比缺一個功能更危險。
- **MCP `create_agent`／`spawn_agent` 等入口的 agent_id 驗證漂移修補**：MCP 面的驗證器相對其他兩份「小寫 slug」實作漏掉了「頭尾不得為連字號」檢查（實作各自手刻導致的漂移），已隨五份收斂一併補上——`-agent`／`agent-` 這類 id 現在在 MCP 入口也會被拒絕（行為變嚴，掃過全部測試與 fixture 無依賴此形狀者）。
- **入站 office／壓縮附件資源上限（`[limits]`）**：gateway 行程自身不解析入站 office 檔（盤點實錘），但它是三個下游解析器的守門人——儀表板檔案預覽 spawn `soffice` 之前、AI 員工 `office_script` spawn Python 解析器之前、專家包上傳解壓之前，現在都先過 `DocumentLimits` 檢查：解壓總量 256MiB／entry 數 4096／單 entry 壓縮比 100:1／XML 巢狀深度 128（深巢狀會打爆下游遞迴解析器，對單 binary 常駐 gateway 是致命死法）／zip-in-zip 深度 4。全部 fail-closed：違規即拒並回 zh-TW 訊息，config `0` 解讀為「用預設」而非「無上限」，`[limits]` 區塊獨立解析、別區段損毀不開洞。守衛本體零遞迴（顯式 work stack＋平坦位元組掃描），自己炸不了堆疊。刻意不做 `regex_timeout`——三條路徑都沒有使用者可控 regex，不加沒人用的假旋鈕。借鑑 OfficeCLI 的 DocumentLimits 上限表。
- **專家包解壓「header 謊報」繞過修補**：`safe_zip` 的總量上限先前只累加 zip header **宣告**的大小——header 說謊（宣告 0、實際巨大）時總量檢查形同虛設，多個 entry 可各寫 50MB 到磁碟。改為累加**實際寫入位元組**並對剩餘預算取上限（多取一 byte 用於偵測超量而非靜默截斷成壞檔），同時補上 entry 數與壓縮比兩道先前缺席的檢查。

## [1.59.0] - 2026-08-15 — 信念迴圈×目標契約＋排程可靠性修復

### Added
- **信念迴圈（Belief Loop）**：task forward model 之外的第二個預測迴圈——agent 對「外部世界」（任何領域，不限投資）的結構化信念記帳。三個 MCP 工具（`belief_submit`／`belief_settle`／`belief_stats`）寫入 `prediction.db` 新表 `belief_log`；結算為確定性三向 Brier（方向 vs 提交時判斷基準值、可調 flat band 預設 ±0.3%、有 tick 資料時交叉核對申報值、容差 1% 外拒絕結算——agent 不能自報現實）；兩個程式化注入鉤點：目標派工 prompt 附校準統計區塊（<30 筆只給計數不下結論；逐筆記錄 `stats_injected` 供事後 A/B——文獻查無「注入歷史統計會變準」的一手證據，誠實以實驗形態上線）、autopilot tick 喚醒 prompt 附「你申報的方向 vs 現值」一行對照（程式化 diff，源自 arXiv:2605.29463 自由回憶 0% vs 程式化注入 86% 的證據；tick 欄位對映支援 `[belief] tick_subject_map` 顯式設定，慣例 `zXXXX→XXXX` 為 fallback）。統計復用既有 `calibration.rs`（Wilson 下界／proper scoring），不造第二套口徑。儀表板 /foresight 改雙分頁，新增「信念與驗證」分頁（`belief.recent`/`belief.summary` RPC，三態誠實標籤）。設計依 2026-08-14 六路文獻調研（詳 `docs/features/46-belief-loop.md`）。
- **目標指派表單 v2（per-goal 時長與風險邊界）**：/goals 指派與 `tasks.goal_create` 新增 `duration_hours`（到期未過驗收 → `needs_human`，覆蓋全域 wall-clock 取較早者）與 `risk_boundary`（留空自動套用基本款——法規遵循／資安紅線／不得繞過硬性風控／金流與不可逆動作過人審／對外發言過人審，`[goal_defaults] baseline_boundary` 可客製）；邊界每輪程式化注入派工 prompt 並成為 MAV 判官 safety 面向的檢核基準（違反即退回，fail-closed 不變；底層 ActionGuard/ApprovalBroker/dispatch_guard 護欄照常疊加）。
- **信念迴圈×目標契約的統一操作面補完**：①設定 → 自動化新增「信念迴圈」卡——持平判定帶（`flat_band_pct`）與 tick 欄位↔主題映射（`tick_subject_map`，一行一對 key=value，前後端同規則驗證）直接在儀表板編輯、寫入即生效；②AI 員工設定新增「自主研究」開關（`agent.toml [research] self_study`/`self_study_hour` 預設 20 點）——當日有信念失準（settled miss）的員工，於設定時間自動獲派一個晚間研究目標（題目＝當日 Brier 最差的主題，每員工每日至多一個，`SelfStudyScheduler` 5 分鐘巡掃、kv 標記＋任務標籤雙重防重）；③指派目標表單新增「要求結構化預測」勾選——伺服器端在目標描述與驗收標準附加信念申報教導段，成績自動進 /foresight 信念與驗證分頁。`/goal` 聊天指令同步支援 `時限:36h`／`3天` 與 `邊界:<紅線>` 段（格式錯誤 fail-closed 退回用法說明）；目標詳情框顯示時限（剩餘/已逾期）與本目標風險邊界。
- **/foresight 預測列表可讀化**：`forward.recent` 補預測/實際的 outcome 與 artifact 欄位＋任務標題解析；列表顯示「預測 X → 實際 Y ✓/✗」與任務名；詳情對照補「產出形式」列（先前唯一失準維度在頁面上不可見）。

### Changed
- **派工引擎預設開啟（goal loop 總開關）**：`[dispatch] enabled` 預設由 false 改 true——目標任務管理台與預測驗證頁（v1.58）讓目標迴圈成為一級功能後，「指派了目標卻沒人執行」成為預設體驗的缺陷。閒置成本僅週期性 SQLite 輪詢（驗收判官只在目標真正進入 review 時才花 LLM 呼叫）。同時把引擎的建構/啟動搬上 `respawn_dispatch_engine`（與 goal-loop driver、topology driver 同款 abort+respawn 模式），`system.update_config` 支援 `dispatch.enabled`，儀表板「設定 → 自動化」新增「派工引擎」開關——切換即熱生效（含 forward model 的執行期建構），免重啟。

### Fixed
- **`/healthz` 納入排程器活性（背景任務層靜默全滅事故）**：cron／heartbeat 排程迴圈每 tick 寫入時戳，`/healthz` 於任一迴圈停擺逾 5 分鐘（或開機後從未啟動）時回 503 並附 `schedulers` 診斷欄位；四個產品 compose 與實驗部署的 healthcheck 由永遠回 "ok" 的 `/health` 改指 `/healthz`——2026-08 實驗容器排程層全滅期間，容器連續多日顯示 healthy。boot 序列補上階段可見標記（channel 啟動前後、HTTP bind 前），排程死亡不再只能靠「log 的缺席」反推。
- **Google Chat／Teams 通道 client 補上 30s timeout**：兩者是 gateway boot 路徑上僅有的無 timeout 網路 await（token 預取直接 `.await` 在啟動序列上）——OAuth 端點無回應會無限期卡住其後的 heartbeat／cron／tick 啟動。與其他七個通道 client 對齊。
- **/goals 頁的 goal_mode 過濾下推後端＋非 admin 首載修復**：先前整張任務看板拉回前端再篩（大看板白拉資料），且非 admin 未帶員工篩選時首載直接吃權限錯誤、與空看板不可區分。現在 `tasks.list` 支援 `goal_mode` 參數（SQL 層過濾）；非 admin 自動帶入第一個綁定員工、隱藏「全部員工」選項；載入失敗顯示錯誤狀態（可重試），不再偽裝成空看板。
- **/foresight 頁權限錯誤與「沒有資料」在 UI 不可區分**：`forward.*` RPC 失敗（如角色不足）先前被吞成空陣列、與零預測的空狀態長得一模一樣。現在顯示帶錯誤訊息的錯誤狀態與重試按鈕。

## [1.58.0] - 2026-08-14 — 目標任務管理台＋預測與驗證頁＋通道 OTP 修復與設定整合

### Added
- **目標任務管理台（工作 → 目標任務）**：目標迴圈任務先前只能在聊天通道用 `/goal` 指令建立，儀表板上散在任務看板／收件匣各看到一角。新頁面把「指派 → 迴圈 → 介入」收成一站：儀表板直接指派目標給 AI 員工（目標描述＋驗收標準＋優先度，與 `/goal` 同一套語義——判官迴圈、needs_human 升級、autonomy 啟動閘全部照舊）；每個目標展開完整執行時間軸（每輪派工→提交→驗收裁決與退回理由、啟動核准、震盪偵測、人工決策節點，走新的 `tasks.timeline` 聚合查詢——任務範圍的活動紀錄不再被全域窗口沖掉）；人工介入節點就地操作——啟動核准／拒絕、卡住任務的重試（可帶下一輪指示）／標記完成／放棄／我來接手。**同時修正一個實質缺陷**：儀表板先前對 needs_human 的裁決走裸 `tasks.update`，與通道按鈕的 `resolve_needs_human` 行為分歧（不清舊 claim/lease/產出、舊判官回饋會滲進下一輪、無 fail-closed 狀態保護）；新的 `tasks.goal_decide` RPC 與通道按鈕走完全同一條路徑（含活動紀錄與通道卡片收合），任務列也補齊了 `goal_mode`／驗收標準／產出摘要等先前前端只能用啟發式猜的欄位。
- **預測與驗證頁（工作 → 預測與驗證）**：LLM→LWM 任務前瞻層先前只在記憶頁的一個分頁看得到片段（總數／平均誤差／最近清單）。獨立成頁後補齊整個迴圈：預測 → 執行 → 觀測 → 對照的四段視覺與計數；點任一筆展開該任務**每一輪的預測 vs 實際對照**（預期工具類別／呼叫次數帶 vs 實際、預期結果 vs 實際結果、觀測保真度、誤差等級與 Brier 分——新 `forward.chain` RPC 首次把存了但從未可查的 prediction/observation JSON 解開）；每位員工一張**預測能力判定卡**（查詢時即算的 Brier 技能分＋Murphy 分解＋信心-命中對照條，三態誠實標籤：有真實預測能力／樣本還不夠／與運氣無法區分——絕不出現「看起來有效」）；「世界模型累積」卡首次讓 `task_state_models`（LWM 真正學到的狀態桶）有了讀取面。記憶頁的舊「預測校準」分頁功成身退。

### Added
- **側邊欄「新功能」標籤機制**：新功能頁面加進導覽時標上出貨版本（`newIn`），側邊欄自動掛「新功能」chip，過了該版本的下一個 minor 自動消失、免手動清理。本版的「目標任務」與「預測與驗證」即掛此標籤，且兩頁在個人版從「進階」搬上主要導覽列（指派與監看目標是日常工作，不該摺疊）。此後所有新功能頁面（不論一般或進階層）一律照此規範標示。

### Added（目標迴圈可觀測性補洞，2026-08-14 第二批）
- **驗收判官的三面向裁決結構化落庫**：MAV 判官（正確性／完整性／安全性）先前在落庫前被壓平成一段文字，時間軸只能顯示合成回饋。現在每輪的逐面向結果（`[{name, pass, reason}]`）存進 `task_iterations.verdict_json`，目標任務時間軸以徽章呈現「正確性 ✓／完整性 ✗／安全性 ✓」（零成本 deterministic 退回沒有面板，誠實顯示為無徽章）。
- **執行紀錄 ↔ 目標輪次的持久連結**：`dispatch_runs` 新增 `task_id`／`round` 欄（錄製時從派工提示的 goal-loop 標記解析，先前只能事後字串比對且會被遮罩截斷破壞）；目標任務時間軸每輪直接連到該輪的執行紀錄回放（`/runs?run=…`），`runs.list` 也回傳連結欄位。
- **派工重試與無進展訊號落庫**：每輪的實際派工次數（stall 重派）與 visit-graph 狀態重複 streak 先前只存在 driver 記憶體、重啟即失。現在落在輪次列上（`dispatch_count`／`state_hash`／`repeat_streak`），時間軸顯示「重派 ×N」「狀態重複 N 輪」警示。
- **預測子誤差落庫**：任務前瞻預測結算時的四維子誤差（工具選擇／呼叫量／結果／產出形式）先前算完即丟。現在存進 `task_prediction_log.error_json`，預測與驗證頁的任務迴圈視圖以維度徽章顯示「這輪預測是哪一維失準」。

### Fixed
- **儀表板所有 needs_human 裁決面統一走 fail-closed 路徑**：收件匣決策面板、任務看板卡片、任務詳情頁（共用 `NeedsHumanActions`）先前走裸 `tasks.update` 狀態寫入——重試不清舊 claim／lease／產出、上一輪判官回饋滲入下一輪、無「僅限 needs_human 狀態」保護。現在與通道按鈕、目標任務頁一致，全部經 `tasks.goal_decide`。
- **通道 OTP 登入在「bot 綁在單一 AI 員工」的部署會靜默失敗**：登入驗證碼的送信先前只讀全域 `[channels]` bot token——把 Telegram 從全域搬成某位 AI 員工的專屬通道後（全域 token 清空），聊天照常運作、OTP 卻永遠送不出去（稽核紀錄：`channels.telegram_bot_token not configured`），而登入頁因防列舉設計仍顯示「已送出」。現在送信候選是「全域 token 優先，再逐一嘗試各 AI 員工的專屬 bot token（去重、依員工 ID 排序）」，逐一送到成功為止；在 Telegram 上這不只是備援——只有使用者實際聊過的那顆 bot 才有資格私訊他。同類掃描一併修正：安裝審批 DM、autopilot `notify` 動作、Telegram 加好友深連結的 bot username 解析，全部套用同一條候選鏈。

### Changed
- **Agent 完整設定「整合→通道」與「管理→通道」整合**：agent 設定頁先前保有 13 個裸 token 欄位，跟通道管理頁是兩套互不知情的編輯器（本次 OTP 事故的溫床——同一份設定兩個入口，搬移後彼此不同步）。現在該區塊改為顯示這位 AI 員工的專屬通道即時狀態（連線燈、測試、編輯、移除），新增／重設走與通道管理**同一個**對話框（`channels.add`，儲存即熱啟動 bot——舊表單路徑寫完要等重啟）；webhook 類全域通道（LINE／WhatsApp／Feishu 等）導向通道管理設定。移除的舊欄位中，LINE／WhatsApp／Feishu 的 per-agent 寫入本就是無任何讀者的死設定。

## [1.57.0] - 2026-08-13 — 生態系擴展：IDE／Remote MCP 直連、五通道文字裁決、客戶端與散發矩陣＋本地模型市集與工作狀態

### Added
- **六個免費產業入門包（premium 降級版）＋板模畫廊擴充**：從付費產業板模降級出六個免費入門包——電商客服、房仲業務、補習班招生、健身房會務、寵物醫院前台、診所前台（`distribution/packs/`，expert pack 格式、單一 AI 員工、`expert install` 一鍵匯入）。降級刀法固定：**安全邊界與轉真人規則全數保留**（免費核心不閹割——獸醫法／醫師法／消保法紅線、急症清單、禁語表一字不減），移除的是付費版的產業法規知識包（wiki）、FAQ 題庫、多員工團隊劇本與加購話術。六包 dry-run 安裝驗證全過＋一包真安裝活測（persona／設定合併正確）；板模畫廊同步收錄六頁（SEO landing＋一鍵匯入指令），公開上架待 registry repo 建立與畫廊部署。
- **Remote MCP：claude.ai 可以直連你自架的 DuDuClaw 了（標準 `/mcp` 端點＋完整 OAuth 2.1）**（[docs/guides/remote-mcp.md](docs/guides/remote-mcp.md)）：`duduclaw http-server` 先前只有 DuDuClaw 自定的 REST 包裝（`/mcp/v1/call`＋SSE），標準 MCP 客戶端（claude.ai 自訂連接器、Claude 行動版、MCP Inspector）無法接上。現在補齊兩塊：**①規範原生端點** `POST /mcp`——`initialize` 版本協商（2024-11-05／2025-03-26／2025-06-18）、`ping`／`tools/list`／`tools/call`，通知回 202、無 session 的無狀態模式（spec 合法；GET/DELETE 405）、`MCP-Protocol-Version` 標頭驗證、`Origin` 錨定白名單 fail-closed（防 DNS rebinding）。**②最小而完整的 OAuth 2.1 授權面**——RFC 9728 資源中繼資料（401 附 `WWW-Authenticate` 探索指標）、RFC 8414 AS 中繼資料、RFC 7591 動態註冊（僅收 https 與 loopback 回呼、拒自訂 scheme）、授權碼＋PKCE S256 必須、操作者同意頁（貼一把**內部** MCP key 證明是操作者本人；外部 key 不能自我升級）、refresh token 每用即輪替。安全模型與既有面**同一套、沒有第二條規則**：OAuth 簽發的權杖永遠是外部客戶端等級（C4 對外工具謂詞照管），scope 收斂到可對外授與白名單（連接器／執行類／Admin 永不透過 OAuth 開放）；權杖落盤只存 SHA-256（0600）、授權碼單次 10 分鐘、存取權杖 1 小時。所有行為對照官方 spec repo 2025-06-18 版逐條核驗；活體測試 45 項全過（transport 16＋OAuth 全流程 29，含負面：code 重用拒、錯 PKCE 拒、舊 refresh 燒毀、admin 工具不露出、落盤無明文）；mcp 家族 341 測試零回歸。claude.ai 端實測需公網網址（tunnel／網域），留待部署後驗收。
- **`duduclaw acp` — 真正的 Agent Client Protocol server，IDE 直連 AI 員工**：Zed／JetBrains／nvim 的 agent panel 現在可以直接指向 `duduclaw acp`（Zed 設定：`{"agent_servers": {"DuDuClaw": {"command": "duduclaw", "args": ["acp"]}}}`），在編輯器裡跟自己的 AI 員工對話。實作 ACP v1（stdio 換行分隔 JSON-RPC，全部形狀對照官方 schema repo 逐項核驗，非憑記憶）：`initialize` 版本協商＋能力/認證方法宣告 → `session/new` 綁定 Main 角色員工 → `session/prompt` 走與通訊頻道**同一條** gateway 回覆管線（session 多輪記憶、契約檢查），過程即時串流 `session/update`（工具呼叫起訖 → `tool_call`／`tool_call_update`、TodoWrite 看板 → `plan`、回覆 → `agent_message_chunk`）；`session/cancel` 依 spec 以 `stopReason: "cancelled"` 收尾且不影響後續回合。home 尚未設定時回合規 `AUTH_REQUIRED`（-32000）並宣告指向終端機 `duduclaw onboard` 的認證方法。活體驗證：協定測試腳本全迴路（含真模型回覆、取消、取消後續聊）全數通過。既有 `duduclaw acp-server`（A2A 協定，先前與此撞名）行為零變化，兩協定刻意分開在不同指令。
- **本地模型市集（管理→本地模型）**：本地模型設定先前要求使用者懂 GGUF、懂 quant 代號、自己找 repo、手改設定檔——學習成本高到只有進階者可用。新頁面把整條路收斂成三步：選**用途**（聊天助理／寫程式／長文件／中文優先）→ 看**硬體適配燈**（綠=可舒適執行、黃=勉強可跑、紅=裝不下——依這台機器的實際記憶體算給你看）→ **一鍵安裝**（自動挑最適合的量化版本、續傳、裝完自動出現在清單）。模型來源是 Hugging Face 上五家品質驗證過的發布者（unsloth／bartowski／mradermacher／lmstudio-community／ggml-org，同一模型多來源自動去重），免帳號即可瀏覽安裝；需授權的模型會標示。**MoE 模型有雙軌判定**：像 30B-A3B 這類「總參數大、每次啟用少」的模型，全載入裝不下時會另外標示「省顯存模式可用」——這是 turbo-fieldfare 驗證的 expert-offload 路線，16GB 的機器也能被合理推薦 30B 級模型（執行層今天可經 llamafile `extra_args = ["--cpu-moe"]` 真實啟用；llama.cpp 原生 expert streaming 待上游 PR #25294 合併後跟進）。進階抽屜保留全部量化版本手選（含 imatrix 標示與每檔適配燈）與「手動輸入 repo」逃生口——舊的清單機制正式退役，原推理設定頁頂部加導流說明。24 小時快取、HF 不可達時退回快取或空清單、絕不擋頁面。
- **排程工作也會累積記憶與知識了（派工路徑接上蒸餾管線）**：記憶蒸餾／知識自動歸檔（`wiki_ingest`）先前**只掛在頻道對話路徑**——一個純排程驅動的 AI 員工跑再久也不會累積任何東西（自主投資實驗實測：觀察員員工四天零記憶，連記憶資料庫都沒建立；操盤員工 202 次盤中巡檢的真實決策全數流失，只有聊天內容有被萃取）。現在成功的排程／派工執行也會餵進同一條蒸餾管線（同樣的零成本分級、novelty 防重複、每日配額全部沿用），並以**每個員工每小時最多一次**節流——盤中每 3 分鐘的巡檢內容多為重複，不節流會讓分級器裡的策略關鍵詞每次都觸發雲端萃取呼叫。節流檔案出錯時一律跳過本輪萃取（fail-open 朝「不多花錢」的方向）。
- **排程／派工執行紀錄終於看得到（執行紀錄頁納入 cron 與派工 run）**：先前「執行紀錄」頁只列得出頻道對話折出來的 run——排程（cron）與匯流排派工的每一次執行**在儀表板上零紀錄**（自主投資實驗實測：盤中排程跑了 202 次，執行紀錄頁一筆都沒有）。現在每次 cron／派工呼叫結束都會落一筆執行紀錄（起訖時間、成功或失敗、進出摘要皆經秘密遮罩與長度上限）並附上這次實際呼叫的工具步驟；「執行紀錄」頁與詳情回放直接看得到（`channel` 欄標示 cron／dispatch 來源）。與目標迴圈的證據收集器共存不互搶（外層已有收集器時唯讀共用，絕不遮蔽 settle 側的證據消費者）；一般對話與進化呼叫不重複錄（各自已有呈現面）；紀錄沿用執行步驟儲存的 7 天保留期；錄製全程 fail-open——儲存失敗絕不影響回覆本身。：v1.53 任務預測（forward model）與 v1.54 校準層先前**沒有任何儀表板呈現面**——AI 員工在目標任務前做的預測、事後的對照計分，全部只存在資料庫裡。LLM→LWM 是平台功能而非實驗產物，所以這個呈現面做成通用的：新分頁對**每一個** AI 員工顯示預測總數／已結算數／平均誤差分（Brier，低=準）、四級結果分布（如預期／小偏差／明顯偏差／嚴重偏差）、觀測保真度與預測來源（專屬統計／同類統計／預設經驗值／AI 推估），加一張「最近的預測與結果」對照列表——全部用白話字彙，不洩漏引擎術語。後端是兩個唯讀 RPC（`forward.summary`／`forward.recent`，manager 以上），只讀既有的 `prediction.db` 稽核軌跡：資料庫缺失回空集合而非錯誤、統計視窗有界且誠實標示掃描範圍（不假裝涵蓋全史）。
- **工作狀態（Working State）：AI 員工跨喚醒的唯一權威狀態**（[docs/features/44-working-state.md](docs/features/44-working-state.md)）：長駐員工的每次喚醒（排程／心跳／目標迴圈／九通道對話）都是全新呼叫，操作規則若只存在筆記裡，哪次醒來讀到哪份筆記、規則就是哪一版——自主投資實驗 D3 實測一天寫出三條互相矛盾的停損線（文獻稱 ghost memory）。現在每個員工有一份鍵值化的權威工作狀態（`stop_loss.2317 = 262` 這類「現行值」）加一段交接註記，閘道在**每一次喚醒**自動注入提示詞動態尾端（快取分層之後，不打斷前綴快取；空狀態零注入），注入文案明示：筆記／日誌與此矛盾的數字一律是已作廢的歷史值。變更只收顯式工具呼叫（`working_state_set`／`working_state_clear`／`working_state_handoff`／`working_state_get`，四個 MCP 工具、五種執行環境通用）且 `reason` 必填，舊值進入可稽核的取代鏈（`working_state_history.jsonl`）；`expected_value` 提供 Letta `memory_replace` 式的並發保護（與現值不符即拒寫並回報現值，每 3 分鐘的排程巡檢不能互相蓋寫）；`ttl_hours` 讓當日規則自動到期（昨天的盤中停損線不會變成今天的權威）；32 個 key 上限到頂即拒收並列出現有 key（逼收斂，不讓狀態表膨脹成第二座筆記山）。狀態寫入工具列入自我迴聲清單（自己寫的值不能拿來自我接地）與變異稽核清單；`config.toml [memory] working_state_enabled`（預設開）只關注入、不動工具與檔案；所有讀寫失敗一律 fail-open 成「無區塊」，絕不擋喚醒。
- **`duduclaw tunnel` — 一行指令讓儀表板可以從外面開**：跑 `duduclaw tunnel` 就啟動一條 Cloudflare 快速通道（免帳號、免網域、免設定），畫面直接給你一個 https 網址與必要的一行 `allowed_origins` 設定（精靈刻意**不會**自動改安全設定——擴大來源白名單永遠是人的決定）。誠實標註快速通道的特性：網址每次啟動都會變、無 SLA，日常遠端建議 Tailscale、正式對外參閱部署指南。沒裝 cloudflared 時給三平台安裝指引與替代方案，不會悶著失敗。
- **外部 MCP 客戶端的工具面改為權限範圍驅動（預設行為不變）**：外部應用先前只能用硬編碼的 7 個白名單工具，要多開任何一個都得改程式重編譯。現在改為權限範圍（scope）政策：**沒帶額外 scope 的既有 key 行為與先前完全一致**（仍是那 7 個）；操作者簽發 key 時顯式帶上 `memory:read`／`memory:write`／`wiki:read`／`wiki:write`／`messaging:send` 之一，該 key 就能使用對應**家族**的全部工具（如批次讀取、別名管理、工作狀態）。可對外授與的 scope 是一張刻意保守的白名單——連接器類（Odoo/Google/Notion/GitHub）、執行類（fork/OS 原生/技能執行/錄製）、人員名冊與 Admin 永遠不對外，key 就算聲稱了也無效；Admin scope 也不能替代顯式授權。工具「看得到」與「叫得動」用同一條判斷（tools/list 與呼叫閘同謂詞）。
- **穿戴裝置逐字稿直灌記憶（`POST /ingest/transcript`）**：AI 錄音吊墜／手環（Omi、Plaud 等）的 webhook 現在可以直接把逐字稿存進 AI 員工的記憶——`duduclaw http-server` 新增一個對廠商 webhook 友善的端點：同一把 Bearer key、同樣的速率限制，寬容接受各家 payload 形狀（`text`／`transcript`／`summary`／`segments[].text`），寫入走與 `memory_store` 工具完全相同的管線（權限範圍、對外工具白名單、寫入時來源綁定全部照常生效，逐字稿以外部來源的信任上限入庫）。Bee 手環則更簡單——官方 CLI 本身就是 MCP server，掛 `[[mcp.external]]` 純設定接入。Apple Watch 捷徑配方（問 AI 員工／存記憶，錶面一鍵）一併收錄在新指南 [docs/guides/shortcuts-and-wearables.md](docs/guides/shortcuts-and-wearables.md)。
- **Pack registry 消費與發佈（`expert install registry:<slug>`／`expert publish`）**：expert pack 現在可以從社群 registry 一鍵安裝——安裝端在客戶機上**自行驗證**（registry 被攻破也不足以塞進竄改的包）：下載的 zip 必須符合 index 記載的 sha256；含 hooks/skills 的包（code lane）還必須通過發佈者 minisign 簽章驗證（發佈者公鑰註冊在 registry 的 `publishers/` 下），缺簽章、缺公鑰、驗證失敗一律拒裝。純 agents＋知識頁的包（data lane）免簽——宣告式資料與可執行程式碼是兩個風險等級。`expert publish` 幫發佈者把「打包→算雜湊→產 registry entry JSON→PR 三步指引」一次做完，含 code/data lane 自動判別。registry 本體（index repo＋CI 驗證）種子在 `distribution/registry/`，正式 repo 上線另行公告。
- **第三方開發者入口（CONTRIBUTING.md＋pack 創作教學）**：repo 先前沒有任何「怎麼擴充 DuDuClaw」的入口——想貢獻的人只能逆向工程。現在 repo 根有 CONTRIBUTING.md（五種免 fork 的擴充單位對照表：expert pack／SKILL.md／外掛 MCP server／畫廊條目／gateway API 客戶端，加上可執行工件「必宣告能力」的安全底線），並新增 [docs/guides/build-your-own-pack.md](docs/guides/build-your-own-pack.md) 動手教學：10 分鐘做出最小可用包、本機安裝測試迴路、團隊/知識頁/前置需求進階，到 convert-teams 與 claude-plugin 格式轉出。
- **公開網站聊天 widget（訪客模式＋WordPress 外掛初版）**：gateway 新增 WebChat 訪客模式——`config.toml [webchat] public_widget = true`＋`widget_key`（至少 16 字元）後，網站訪客可用公開 widget key 匿名跟 AI 員工對話，不需儀表板帳號。**預設關閉**，每一道檢查（未設定/未啟用/key 過短/比對失敗）都直接拒絕；key 採 constant-time 比對；每個連線發放唯一隨機訪客身份，訪客之間永遠無法接續彼此的對話；既有的 per-IP 連線上限與全域連線閘照常生效。widget key 出現在公開網頁原始碼是設計使然——它只授權「匿名訪客對話」一件事。配套出貨 WordPress 外掛初版（`clients/wordpress/duduclaw-webchat/`，尚未上架）：設定頁填 gateway 位址與 key，前台浮動聊天泡泡直連站主自己的 gateway，角標「Powered by DuDuClaw」，無第三方服務、無遙測（readme 含完整外部服務揭露）。
- **免費版通道回覆尾註「Powered by DuDuClaw 🐾」**：免費層（未授權／OpenSource／Hobby）的 AI 員工在**對外通道**（LINE/Telegram/Discord/Slack 等 10 平台）回覆末尾附一行品牌尾註；付費授權可在 `config.toml [branding] reply_footer = false` 關閉（此設定經授權層把關，免費安裝改了也不生效）。範圍刻意只含終端客戶看得到的表面——儀表板自家的 WebChat 主控台與內部排程回覆一律不加；刻意靜默的空回覆也不會因此變成一則訊息。與分版原則一致：免費層鎖的是品牌露出，不是能力。
- **回覆審批卡打「同意」就等於按下按鈕（Telegram／Discord／Slack／LINE／Teams）**：智慧手錶收得到審批卡卻按不到按鈕——錶端 app 只給通知與文字回覆。現在**回覆**某張決定卡（審批/目標/安裝/自動規則）並送出整句裁決詞（「同意」「拒絕」「重試」「完成」「中止」「暫停」，中英皆可）就等於按下那顆按鈕：Telegram/Discord 用回覆該卡、Slack 直接在卡片的討論串裡回、LINE 與 Teams 用「引用回覆」該卡——皆不需要 @ 提及，引用卡片本身就是在對 bot 說話。五個通道走同一條裁決路徑（同樣的授權檢查、同樣的重複按壓保護、同樣的行動率記帳），不開第二套規則。裁決詞必須是**整則訊息**（容忍句尾標點）——「我不同意這個方案，先討論」是對話不是裁決，照常交給 AI 員工；回覆的也必須是一張真實存在的決定卡，其他一切照舊。為此 LINE 與 Teams 的決定卡現在會記下送出後的訊息編號（兩者都不做就地編輯，既有收斂行為不受影響；Teams 的引用回覆從 HTML 附件的 `blockquote itemid` 解析出被引用的卡片編號）。Teams 決定卡實為純文字＋連結形式（先前文件稱其有卡片按鈕是誤記，已更正）——文字裁決正好補上了 Teams 行動端一鍵決定的缺口。
- **LINE 加好友 QR／店面海報／NFC 桌牌**：通道頁新增「LINE 加好友 QR」——一鍵取得官方帳號的加好友深連結與 QR（QR 在瀏覽器本地產生，不經任何外部服務），可直接複製連結或列印成店面桌牌／海報（零 PDF 依賴，走瀏覽器列印）。同一條連結寫進 NFC 標籤（NTAG213）就是「碰一下開聊」的實體桌牌，對話框內附寫入教學；完整的實體觸點指南（QR／自製 NFC／LINE 官方「LINE Touch」readiness 檢查清單）見 [docs/guides/line-touch-nfc.md](docs/guides/line-touch-nfc.md)。說明文案標明一對一回覆走 LINE Reply API 不計費，免費帳號即可跑正式服務。掃碼 → 加好友 → 跟 AI 員工開聊，是台灣門市最短的開通路徑。
- **Chrome 擴充功能（`clients/chrome/`，初版，尚未上架）**：側邊欄跟自架 gateway 上的 AI 員工對話、審批待決動作，右鍵選單把網頁選取內容連同來源網址餵給 agent 存記憶。MV3、無建置步驟的純 vanilla 實作；host 權限只預設 loopback，遠端 gateway 在登入時才請求選擇性授權；無遙測。一次性設定：把選項頁顯示的 extension id 加進 gateway `allowed_origins`（擴充功能連線帶 `chrome-extension://` Origin，gateway 的來源檢查對任意 scheme 取 authority 比對，白名單填裸 id 即可——零 gateway 端改動）。
- **VS Code 擴充功能（`clients/vscode/`，初版，尚未上架）**：在編輯器側邊欄跟自架 gateway 上的 AI 員工對話（走既有 WebChat 通道協定），並直接查看/裁決待審批項目（走既有儀表板 RPC）——零 gateway 端改動。登入用儀表板帳密換 JWT，token 存 VS Code Secret Storage，過期自動 refresh；只連使用者設定的 gateway 位址、無遙測（README 含完整 network disclosure）。連線架構刻意把所有 socket 放在 extension host（Node 端不帶瀏覽器 Origin，gateway 的來源檢查按非瀏覽器客戶端放行；webview 是純 UI，JWT 從不進 webview）——webview 的 `vscode-webview://` Origin 是隨機值，走 webview 直連會被 gateway 擋下且無法列白名單。`vsce package` 可產出 .vsix 側載安裝；Marketplace 與 Open VSX 上架另行處理。

### Changed
- **儀表板技能市集搜尋改用五-hub 聚合器（與 MCP `skill_search` 一致）**：`skills.search` 先前仍走舊的 GitHub-only 索引（四組固定搜尋字串），與 MCP 端 `skill_search` 的五-hub 聚合器（anthropic-skills／github／clawhub／lobehub／skills-sh）結果集不一致——儀表板看到的市集比 AI 員工少四個來源。現在兩邊走同一個 `HubRegistry`：結果附信任分級、60 天安裝數與來源安全判定供安裝前判斷；個別 hub 失敗誠實列在 `hub_errors`，不再靜默縮小結果集。前端契約保留：`total_indexed` 為 0 仍代表「市集本身不可達」（全部 hub 失敗），可達但查無符合不會誤顯示成「索引未載入」。

### Changed
- **付費產業板模的顯示名稱改為資料驅動**：先前板模在精靈/儀表板顯示的中文名稱是程式裡的一張硬編碼對照表——新增第 23 個產業板模得改 code 才有正式名稱。現在板模目錄裡放一個 `template.toml`（一行 `label = "..."`）就能自帶顯示名稱；沒有 manifest 或 manifest 壞掉（解析失敗、空值、超長、控制字元）一律安全退回原本的內建表格，既有 22 包行為零變化。

### Removed
- **Homebrew 安裝通路確定廢棄**：tap 兩度凍結（1.7.2、1.50.0）後拍板終止此通路——倉庫移除 `HomebrewFormula/`（duduclaw／duduclaw-beta／duduclaw-pro 三個 formula），不再提供任何 brew 安裝方式。既有以 brew 安裝的使用者：CLI `duduclaw update` 與儀表板更新頁都會明確告知通路已終止，請改用 `npm install -g duduclaw` 或桌面版重新安裝以繼續收到更新（先前 CLI 端仍指示 `brew upgrade`，那是一條永遠等不到新版的死路，已一併修正）。

### Fixed
- **企業版容器裡新建 AI 員工的 MCP 設定指向錯誤的執行檔（員工靜默失去全部 duduclaw 工具）**：`.mcp.json` 的產生邏輯用「目前行程的執行檔路徑」當 MCP server 指令——在企業容器裡目前行程是 `duduclaw-pro`，而它的 `mcp-server` 呼叫會啟動第二個閘道、撞埠即死。結果：企業環境建立的每個 AI 員工從第一天起就**靜默失去整個 duduclaw 工具面**（記憶、知識庫、任務板全部不可用），從稽核日誌看起來只是「員工從不使用這些工具」（自主投資實驗實測：兩個員工四天零記憶累積，逐層排查才發現工具面根本不存在）。修正：解析執行檔時偵測「目前行程不是開源 `duduclaw`」，優先採用同目錄的開源 `duduclaw`（企業映像兩個執行檔並存）；單一執行檔安裝行為不變；既有的錯誤設定會在下次閘道啟動的掃描中自動改寫修正。附回歸測試。
- **授權標示全面校正為 Apache-2.0**：倉庫的 `LICENSE` 一直是 Apache-2.0，但行銷模板與本次新增的多個上架產物寫成 Elastic-2.0——已裁決並全面修正（三個客戶端 README、VS Code package.json、ACP/Claude marketplace/winget 上架 metadata、Twitter 模板等 13 處），.vsix 重新打包；其中 Homebrew formula×2 與 release.sh 模板的修正隨本版 Homebrew 通路廢棄一併移除（見 Removed）。Odoo addon 因其 manifest 授權欄位為固定枚舉維持 LGPL-3（薄殼另授權，與 Apache 產品無衝突）。
- **ACP 功能文件的協定範圍更正（過時文件止血）**：`docs/features/19-agent-client-protocol.md`（en／zh-TW／ja-JP）先前教使用者把 Zed／JetBrains／nvim-acp 指向 `duduclaw acp-server`——實際上該 server 說的是 **A2A 協定**（agent/discover／message/send／tasks/*），並未實作 IDE agent panel 使用的 Agent Client Protocol（initialize／session/prompt），照做會在 initialize 就收到 Method not found。三語文件與 features README/inventory 已加上明確狀態更正：今天可用的對外整合面是 A2A、MCP server（stdio＋HTTP/SSE）與 dashboard WebSocket RPC；真正的 Agent Client Protocol 支援（含 ACP registry 要求的 Terminal Auth）列入生態系擴展工作包。
- **文件裡的 MCP 工具數全面校正**：README 三語版寫 130+／138、`custom-mcp-tool.md` 寫 52+、`ARCHITECTURE.md`／README 的 Odoo 工具數寫 15——實測 TOOLS 陣列為 **206**（Odoo 子集 **17**）。行銷面統一改「200+」抗過時，技術指南錨定版本寫「200+（206 as of v1.56）」；DocuSeal「10 工具」經實測為正確、未改動。`docs/api/openapi.yaml` 版本欄從 0.12.0 同步至 1.56.0，並在檔頭加註覆蓋範圍說明（0.12.0 之後新增的 RPC 方法家族尚未收錄，權威方法清單以 `handlers.rs` dispatch table 為準）。

## [1.56.0] - 2026-08-13 — 儀表板 UX 重構：資訊架構分層、記憶視覺化、白話化與個人版強化
### Added
- **經驗法則會標記「來源已更新」（Hindsight #6 對位；預設隨既有 24h 掃描運作）**：先前 DuDuClaw 只在**事實層**做時間性取代——寫入更新的 `(主體, 述詞, 客體)` 事實會讓舊事實失效（`valid_until`／`superseded_by`）——但這個訊號沒有往上傳到「靠這些事實歸納出的經驗法則」。現在補上這一層（`prediction::rule_staleness`，沿用既有 `rule_lifecycle` 記憶列，**不另開 store**）：規則可在自身 metadata 的 `source_facts` 記下它由哪些事實記憶 id 歸納而來；每次 playbook 掃描（既有 24h/員工節流）會問記憶引擎新增的 `SqliteMemoryEngine::superseded_fact_ids`，只要其中任一來源事實已被取代，該規則就被標上 `source-stale` 標籤＋`source_staleness` 明細（哪幾筆來源、何時偵測）。注入 `## Learned Rules` 時 source-stale 規則**一律排在新鮮規則之後**（無論淨分數多高，仍可注入但降權），並附白話標記「（來源已更新，僅供參考）」。新增查詢 `list_source_stale_rules`（唯讀，供儀表板「重新整理這條過時規則」下期接線）。**fail-open 是硬不變量**：沒有記錄 `source_facts` 的規則（絕大多數既有規則——由 MistakeNotebook 歸納、無 F1 事實來源）**永遠不會被誤判為 stale**，找不到的來源 id 也不算 stale。reflexion 合併路徑已接上記錄 API（因其由錯誤筆記而非事實歸納，目前以空清單呼叫、保持 fail-open；未來讀事實的合併來源只需在此傳入 id 即可生效）。
- **整合失敗可查（Hindsight #7）**：reflexion 合併先前在數個關卡靜默回傳「沒有合併」，使用者問「為什麼這幾筆重複錯誤沒被學起來」時無從查起。新增 `consolidation_failures` 遙測模組：只記錄**真正達到門檻後才被關卡擋下**的整合失敗——B2 證據過濾把原始達標的群組砍到門檻以下（`insufficient_verified_evidence`，附 raw／verified／threshold）、GovMem 獨立性閘判定證據相關（`needs_more_evidence`，附 distinct_sessions／distinct_lessons）、B1 新穎性閘判定近重複（`novelty_rejected`，附 matched_id／similarity／threshold）。**刻意不記錄**「還在累積、未達門檻」這種正常進度（幾乎每筆錯誤都會觸發），所以每一列都是真正的「為什麼沒合併」。存於 `<home>/consolidation_failures.jsonl`（`with_file_lock` 附加、上限 2000 列滾動保留最新尾段），提供 `list_failures(home, agent, limit)` 查詢介面（最新在前、可依員工過濾）；寫入全程 best-effort，遙測失敗只記 log、絕不阻斷合併路徑。儀表板 UI 下期接線。
- **個人版併發上限（限「同時執行的目標任務數」，不限 AI 員工數量）**：落實 D1-C／D2-B 裁決——個人版永遠不設 AI 員工硬上限（自架承諾），改為限制**同時在跑的自主目標任務數**。限的是資源不是能力：單人幾乎不會同時開兩個自主目標，商用一次排三個以上才會撞到。新增 `duduclaw-core::concurrency_gate`——一個**跨行程的 in-flight 租約計數器**（`concurrency_leases.json`，`with_file_lock`＋原子寫入，沿用 `dispatch_guard` 的紀律；但它算的是「同時持有數」而非「時間窗內事件數」，兩者正交、都保留）。目標迴圈在派工入口為每個**新**任務取一張租約，達上限時**排隊而非拒絕**（下一 tick 有空位就派，絕不丟掉使用者交辦的目標）；任務進終態即釋放，長任務每 tick 續租，行程崩潰時租約靠 TTL（預設 1800s）自動回收、計數自癒。版本判定沿用既有 `resolve_edition_profile()` 鏈（環境變數 > 程式覆寫 > 授權方案），**不另造第二套**：個人版預設上限 **2**（小於目標迴圈既有的行程內 spawn 保護上限 3，否則此閘等於沒作用），企業版 `None`＝無限（且**完全不碰檔案**，零成本）。`config.toml [dispatch] personal_max_concurrent`（`0`＝無限）／`concurrency_lease_ttl_secs` 可調，`DUDUCLAW_PERSONAL_MAX_CONCURRENT` 環境變數覆寫。**失效即放行**：租約檔壞了、鎖不到，一律 fail-open 放行——這是資源節流閥不是安全閘，絕不能因為計數檔壞掉就卡死自主目標完成。既有 `[goal_loop] max_concurrent` 行程內保護與此閘正交並存，實際併發＝兩者取小。設計見 [`docs/rfc/RFC-27-personal-edition-concurrency-cap.md`](docs/rfc/RFC-27-personal-edition-concurrency-cap.md)。
- **歸納型教訓現在真的能轉正（補齊 shadow-scoring 流程）**：v1.54 的 held-out 學習閘會把沒有程式化證據的歸納型教訓標成候選、不注入提示詞，並要求它先在樣本外證明自己。自主任務迴圈那側 v1.54 已接好計分，但一般對話這側先前只有「已注入規則」的記帳，候選本身沒有任何累積樣本的機會——這正是功能文件「還沒做的部分」明寫的保守取捨：這類教訓實質上停在候選狀態，永遠轉不了正。現在補上另一半：每次組提示詞時，觸發訊號吻合本輪情境的候選會被登記（仍然不注入）；回合的最終誤差級別出爐後，對每個登記過的候選記一筆樣本外命中或落空（候選的隱含預測是「這種情境風險偏高」，實際出狀況＝命中、虛驚＝落空），樣本夠了走既有的 Wilson 下界＋Bonferroni 閘：贏過基準率才轉正、開始被注入，持續虛驚則退役。基準率用該員工自己的對話高風險底率（歷史不足八筆時退回五五波，寧可保守）；先前被降級收起來的規則也因此有了恢復路徑，訊號再吻合時可以重新累積樣本翻身。觸發訊號只剩萬用字元的候選（多半是早期資料升級來的舊列）**不會被登記試驗**：這種候選每一輪都會觸發，命中率必然收斂到基準率本身，統計上永遠分不出勝負，登記它只是白寫紀錄；它們留在候選區不注入，等人工整理。
### Changed
- **建立 AI 員工時要自己選模型（不再靜默套 `claude-sonnet-4-6`）**：儀表板新增員工的空白路徑先前完全沒有模型欄位，後端 `agents.create` 就內聯寫死 `preferred = "claude-sonnet-4-6"`，每個新員工都被靜默套上這個模型。現在建立表單多了一個模型選擇器（複用員工編輯頁「腦袋與引擎」那顆 `ModelSelect`，從即時 model registry 拉清單、可重新探測），**不預填任何模型**——沒選就不能按「建立」，強迫明確選擇；之後仍可在員工設定隨時更換。用板模建立時不顯示這個選擇器（板模自帶模型）。硬編碼的 `claude-sonnet-4-6` 只留作 MCP／API 程式化呼叫沒帶 `model_preferred` 時的最後 fallback（那些路徑行為不變）。
- **「打字即接手」（真人接手）改為 opt-in（預設關閉）**：先前只要儀表板認得的管理者在通道裡發話，AI 就會停止回覆該對話一段時間（「管理者打字＝無縫接手」）——但這個功能**預設是開的**，而它分不清「管理者插手 AI 正在替團隊處理的對話」和「管理者正在跟自己的 AI 對話」。結果：個人版（唯一使用者本身就是管理者）跟自己的 AI 正常聊天、或團隊管理者把 AI 當個人助理用，每一句話都被誤判成「接手」、AI 直接噤聲。跨平台無法可靠分辨「私訊 vs 群組」而不用脆弱啟發式，因此改把自動接手的預設關掉（`config.toml [takeover] enabled`，預設 `false`）：預設不再自動接手，個人版與團隊管理者正常對話都不會被噤聲。真的要「客服團隊管理者打字即接手」的隊伍再明確設 `enabled = true`。顯式的 `/takeover` 指令不受此開關影響，照常可用。
- **對話路徑的注入規則結算補上 held-out 閘路由**：學習閘開啟時，對話回覆注入的規則先前仍走舊的 net-score 記帳，與自主任務路徑（v1.54 起走數字閘）不一致。現在兩條路徑一致：注入規則的去留由樣本外命中紀錄決定，表現退步的規則被收起來（保留履歷、可翻身）而非直接照舊累計。`held_out_gate_enabled = false` 時對話路徑行為與先前 **byte-identical**。
- **指定 AI 員工可以用哪些帳號（`agent.toml [model] account_pool`）**：員工編輯表單裡的帳號下拉先前寫得進設定檔，但**後端沒有任何地方去讀它**——選了等於沒選。現在這份名單真的會限制該員工的帳號輪替範圍：填了就只從名單內的帳號挑，名單可以寫帳號 id 或儀表板上看到的顯示名稱（完全比對，不做部分字串比對，所以 `main` 不會誤中 `main-backup`）。四種輪替策略（優先序／最省成本／故障轉移／輪流）本身**完全沒有改動**——名單只是把候選帳號先篩一輪再交給原本的策略。**名單失效時一律放行**：名單裡的帳號全被刪掉、改名，或當下全部在冷卻中，會記一筆 `warn` 然後退回使用全部帳號，絕不會因為一份過期的名單讓員工變成「沒有可用帳號」而整個啞掉。沒填或留空的員工行為與先前**完全一致**（零行為變更）。生效範圍涵蓋通道對話（九個通道）、子代理派工、排程、心跳、目標迴圈，以及多執行環境轉接點（`RuntimeContext.account_pool`）；系統層的內部呼叫（演化／夜間引擎／儀表板產生器）不套用任何名單。

### Changed
- **新建 AI 員工的預設帳號名單改為「全部帳號」**：所有建立員工的路徑（`duduclaw onboard`／設定精靈／`agent create`／MCP `create_agent`／儀表板新增員工）與五份內建板模先前一律寫入 `account_pool = ["main"]`。這個值來自早期只有單一 API key（帳號 id 固定叫 `main`）的年代；在今天以 OAuth 訂閱為主的安裝上，`main` 這個帳號根本不存在。名單接上輪替器之後（見上）這會讓每次呼叫都走一次「名單失效→退回全部帳號」並記一筆 `warn`。改為 `account_pool = []`（不限制）——行為與失效退回後**完全相同**，但少了一整排無意義的警告。**已經建立的員工不會被自動改寫**：`agent.toml` 裡仍寫著 `["main"]` 的員工會照常運作（fail-open），但每次呼叫會提醒你到員工編輯頁把名單清空。

### Fixed
- **Docker 映像編譯失敗（v1.55.0 GHCR 映像因此延後補發）**：v1.55.0 新增的 `duduclaw docs` 指令在編譯期 `include_str!` 內嵌 `docs/README.md`，但 `container/Dockerfile.server` 與 `Dockerfile.edition-smoke` 的建置階段只 COPY `crates/`＋`templates/`，容器內缺檔導致 cargo 編譯中止（本機與 CI 的非容器建置不受影響）。兩個 Dockerfile 補上 `COPY docs/README.md`。

### Security
- **多人團隊版功能改由伺服器把關（個人版繞不過去了）**：成員管理、部門、治理政策、經銷商／發授權、白牌品牌、夥伴入口、身分解析、知識庫信任稽核、可靠性報告這些多人團隊版專屬畫面，先前**只有前端在隱藏**——個人版使用者手動改網址、走舊版路由別名，或直接對 WebSocket 介面發指令，一樣叫得動。現在改由 gateway 在指令分派的**唯一入口**擋下，個人版收到一句白話說明（「此功能屬多人團隊版」）而不是真的執行。判斷沿用既有的版本判定鏈（`DUDUCLAW_EDITION` 環境變數 > 程式覆寫 > 授權方案 > 個人版），不另造第二套。**個人版原本用得到的一律不受影響**：改自己的密碼、委派權限、審批、稽核日誌、緊急煞車、自動規則、帳務預算、共用知識庫、組織圖照常可用；授權啟用／兌換也刻意保持開放（擋掉就永遠升不了級）。名單為逐項列舉，比照 MCP 權限表慣例，家族比對採**完整區段相等**（不是字串開頭比對），因此日後新增的同家族指令預設就是受管制的。

## [1.55.0] - 2026-08-12 — UX 重設計 Wave 0–3+常駐感知+真人接手
### Added
- **Telegram 內的審批詳情卡（Mini App，試作，`config.toml [miniapp] enabled` 預設關閉）**：高風險動作核可的 Telegram 卡片多一顆「🔎 查看詳情」，在對話裡直接展開完整說明、事前模擬的後果、每秒更新的到期倒數與同意／拒絕兩顆大按鈕，不必切到瀏覽器。身分靠 Telegram 簽章的 `initData` 證明——照官方演算法重算 `secret_key = HMAC_SHA256(<bot_token>, "WebAppData")` 後定值時間比對 `hash`，另檢查 `auth_date` ≤1 小時、不得指向未來、`user` 必須含數字 id；**任何一項不過就完全不回資料**（連該筆審批存不存在都不透露），網址上的編號不是憑證。決定本身走既有的 `decision_notify::route_press`——Mini App 認出的 Telegram 使用者 id 與按按鈕回報的是同一個，**不開第二套授權**（管理員／主管，或尚未有人綁定身分時僅限收件帳號本人）；查看詳情套用同一組判斷，不給比較寬鬆的讀取規則。按鈕只在**同時**滿足「功能已開啟」「`[dashboard] public_url` 是 https」「收件端是一對一私訊」時附加——後兩項是 Telegram 對 `web_app` 按鈕的硬性規定，任一不滿足就誠實降級，卡片與開啟本功能前完全相同（群組硬塞會讓整則訊息送不出去）。頁面自包含（CSS/JS 全內嵌、無 CDN、無框架、無字型），跟隨 Telegram 佈景深淺色；唯一外部資源是 telegram.org 的平台 SDK，取不到時改從網址片段讀同一份簽章資料。bot token 只當 HMAC 金鑰材料，不寫日誌、不回應、不進頁面。範圍刻意限一張卡（雙軌架構驗證）：LINE LIFF／Teams Dialog／飛書 Web App 具備對等能力但本期未做，Slack／WhatsApp／Google Chat／Discord 沒有對等機制，維持卡片按鈕那一軌。見 [docs/features/43-telegram-miniapp.md](docs/features/43-telegram-miniapp.md)。
- **通知治理**：所有主動推播（決定卡、預算斷路器、演化停滯／整併、技能缺口摘要、通道故障）改走同一層治理。每個推播點標上四級 `NotifyLevel`（L1 週知／L2 待確認／L3 須處理），級別是必填參數而非預設值。新增 `agent.toml [proactive] quiet_hours = "22:00-08:00"`（可選，另有 `config.toml [notify] quiet_hours` 全域退路；時區吃 `[proactive] timezone`，無法解析時用系統時區）：勿擾時段內 L1/L2 通知延後排隊、時段結束後**同一收件目的地合併成一則**投遞；L3（需人工決定、高風險審批、安裝簽核、預算停工、通道故障）照發不擋。格式解析失敗一律 fail-open 視為未設定並記 `warn`——設定寫錯的代價是照常收到通知，不是整晚靜音。既有的 `quiet_hours_start`/`quiet_hours_end` 數字欄位刻意不接管（其預設 23–8 套用在每個員工身上，接管等於讓所有既有安裝無聲靜音一整晚），維持原本「排程主動檢查」的職責。被延後的通知寫入 `notify_queue.jsonl`（檔案鎖），佇列上限 500 則／36 小時，逾限丟棄一定寫 `warn`。`agent.get` 回傳 `proactive.quiet_hours` 與 `proactive.quiet_hours_note`（可直接渲染的繁中說明，明寫哪些延後、哪些照常）供 UI 呈現。見 [docs/features/40-notification-governance.md](docs/features/40-notification-governance.md)。
- **每日摘要**（`config.toml [notify] daily_digest`，**預設關閉**；`daily_digest_at` 預設 09:00 本地時間）：每天一則彙整前 24 小時的完成任務數、待你決定件數、學習事件、花費、通道異常次數，推到 `[general] default_agent` 的 `[proactive]` 目的地。**無事不寄**——全部歸零的一天不發訊息，而不是發一則「今日無事」。硬性上限一則／日（狀態檔記錄本地日期，重啟不補送）；gateway 在設定時間沒開機的話，開機後當天仍會補送一次。
- **通知行動率量測**：推播與「決定真的被處理」各記一筆到 `notify_events.jsonl`，新增 `notify.stats` RPC（Manager+）回傳近 N 天每類通知的推送數／可行動數／行動數／行動率，並依 Google SRE 的「準確率低於 50% 的告警就是壞掉的告警」判準標記 `broken`（需 ≥10 筆可行動樣本）。同一張卡按兩次只算一次行動；被拒絕的按壓不算行動；純週知類（沒有按鈕）永遠不會被標 broken。儀表板圖表下期。
- **通道推播的儀表板深連結**：新增 `duduclaw-gateway::deep_link` 模組——依 `config.toml [dashboard] public_url`（優先）或 `[gateway] port`（退化為 `http://localhost:<port>`）組出落在物件詳情頁的可點連結（任務 → `/tasks/<id>`、審批/安裝簽核 → `/inbox`、通道 → `/manage/channels`、花費 → `/manage/billing`、系統日誌 → `/manage/logs`、自動規則 → `/manage/system?tab=autopilot`）。三個通道推播模組（goal/approval/install）的「請至儀表板」純文字提示全部改附此連結；有按鈕的通道也在卡片尾附同一連結作為按鈕失效時的 fallback；未設定時維持原文字不變。見 [docs/guides/deployment-guide.md](docs/guides/deployment-guide.md)。
- **決定卡就地收斂**：通道上的審批/需人工卡片在按下按鈕後就地改寫成一行結果（例「✅ 已同意（由 王小明 於 14:32）」）並移除按鈕，不再留下可點但已失效的殘卡。新增 `decision_card`（Telegram `editMessageText`／Slack `chat.update`／Discord `PATCH`，bot-token 直呼不依賴會過期的互動 token）與 `decision_message_store` 兩模組；LINE 無編輯 API，維持「回覆即結果」；編輯失敗誠實降級為追加一行文字，決定本身不受影響。
- **花費斷路器雙向通知**：AI 員工因花費達上限被停下時，管理員的通知通道會收到一則推播（附帳務深連結，恢復時亦通知，同一次觸發只推一次）；正在對話的使用者則收到白話說明（「已停工（花費達上限），已通知管理員」），不再無聲已讀不回。
- **通道故障告警**：同一通道 10 分鐘內 ≥3 次發送失敗時，推播到管理員**另一個仍正常的通道**（附通道管理深連結），並記入活動流；全部通道都不可達時誠實降級為日誌+活動流。恢復後告警狀態重置。
- **自動規則觸發保護通知**：自動規則短時間連續觸發被自動暫停時，推播含「暫停這條規則」按鈕的通知（Telegram/Slack/Discord/LINE），按下即停用該規則並就地收斂卡片；授權沿用審批按鈕同一套規則（Admin/Manager 或收件帳號本人）。
- **收件匣逾時倒數**：審批/安裝簽核卡片顯示到期倒數，剩餘不足 1/3 時亮「即將逾時」標記，並明示「逾時未決會自動拒絕」。
- **AI 員工自主等級設定**：員工編輯表單新增五級自主等級單選（含每級白話後果說明），先前只能手改 `agent.toml` 的關鍵旋鈕補上 UI；未知值拒絕寫入。
- **失敗訊息帶「去哪看」連結**：通道上的失敗回覆附一行「🔎 詳情」深連結（額度類 → 帳務頁、其他 → 系統日誌頁）；`channel_failures.jsonl` 同步記錄 console/doc 兩條連結供儀表板呈現（文件連結只在對應文件真實存在時給出）。
- **Onboarding 通道導流**：首次設定精靈的部署成功頁主要動作改為「讓 AI 員工上線」（前往通道綁定），導覽最後一步走到通道頁——修復新使用者跑完精靈不知道要綁通道的斷點。
- **行為調整試行結果進活動流**：AI 員工行為調整的試行結果（生效/回退/證據不足未採用）現在會記入活動流，不再只有 gateway 日誌可查。
- **「經驗法則」成為看得見的一等物件**：AI 員工從做過的事裡歸納出、之後會自動遵守的規則，先前只以給模型看的原始條文呈現，讀起來像機器語言。新增 gateway 端純樣板改寫層（`playbook::humanize`，**零模型呼叫、零延遲成本**），把規則組裝成一句白話（「當任務出現〈做不到、能力不足〉時，我會〈先確認手上有哪些工具〉」）；組不出通順句子時**不硬編**，改為顯示原始條文並明確標示「這條還無法自動改寫成白話」。`playbook.list` RPC 增加 `humanized` 欄位（含白話句、狀態、「為什麼有這條」與證據計數）。儀表板記憶頁的規則卡片改以白話為主、原始條文收進可展開區塊，並固定顯示「為什麼有這條」（歸納自幾次失敗、幾個驗收案例把關、實際用過幾次／幾次有幫助）；先前直接印在卡片上的觸發訊號機器 token（`mistake:capability` 這類）已移除。狀態徽章統一為白話語彙：**觀察中（尚未生效）／試用中／生效中／很久沒用到，已收起來／已淘汰**。三語同步；`Playbook`／`プレイブック`／「行為手冊」等內部詞已從介面全數移除，對外一律「經驗法則」。
- **通道指令 `/rules`**：`/rules` 列出該 AI 員工目前生效中的前 3 條經驗法則（白話版＋用過幾次／幾次有幫助），`/rules all` 列出全部（含觀察中、已收起來；已淘汰的留在儀表板）。與其他斜線指令一樣在進入 AI 之前攔截，**零模型成本**。
- **AI 能說出「我為什麼這樣做」**：注入給 AI 的經驗法則區塊現在帶編號（`[法則 N]`）與一行說明，讓 AI 回答時能直接指出依據的是哪一條（「因為我學過：…」），而不是事後編一個聽起來合理的理由。編號與說明一併計入原有的注入字元預算，不會擠掉規則以外的內容。
- **每日摘要拆出經驗法則試行結果**：摘要的「學習事件 N 則」底下多一行 `↳ 經驗法則試行結果：採用 X 條、回退 Y 條、證據不足 Z 條`——單一數字先前無法分辨「全部採用」與「全部回退」。沒有任何試行落地的那天不會多印這行（也不會印「採用 0 條」）。AI 員工自主更新規則、以及管理者手動停用規則，現在都會留下活動流紀錄供摘要統計（先前這兩件事在活動流上完全不可見）；兩者都是週知級，只進摘要、不單獨推播。

- **統一「待辦決定」管線**：goal 需人工/派工核准、通用審批、安裝簽核、自動規則暫停五種來源的通道按鈕收斂為單一 action-id 編碼（`duduclaw:decide:<source>:<verb>:<id>`，長度以測試釘在 Telegram callback_data 64-byte 硬限制內）、單一授權模型（goal 按鈕先前不驗證按鍵者身分的缺口已關閉——現在與審批一致：有使用者系統時要求 Admin/Manager，否則僅限收件帳號本人）、單一推播管線與狀態語彙。通道上仍存活的舊格式按鈕全部向後相容可按。一人決定後，同一決定推播到多位管理員/多通道的**所有**卡片同步收斂（先前只收斂按鍵者那張）。
- **自主任務卡片「交給我」（接手）**：需人工卡片新增第四顆動作，按下後任務標記由你接手（`claimed_by`），並就地收斂為「👤 已接手（由 X 於 HH:mm）」；由於任務仍停在 `needs_human`，自主迴圈本就不會再對它派工，等同立即停止自動重試。四顆動作超過單則訊息「主要動作 ≤3」的原則，因此「放棄」與「交給我」收進次級：Telegram 第二排按鈕、Discord 第二排按鈕、Slack `overflow` 選單；LINE 沒有對應的次級選單機制，改列為純文字並附儀表板連結。此為分期實作的第一層（停止自動重試＋標記＋收斂卡片）；完整的對話控制權轉移由同版的「真人接手對話」補齊（見下）。
- **決定卡片加「原因」標籤**：五種待辦決定的通道卡片第一行統一加上可一眼分辨的來源標籤（🤔 自主任務等你決定／🚀 新任務要開工／⚠️ 高風險動作需要你同意／📦 安裝申請／🔁 自動規則已暫停），收件匣的分類標籤文字同步對齊（三語）。
- **統一收件匣**：收件匣現在自行處理全部三類決定——安裝簽核詳情不再跳轉舊審批頁；自主任務「等你決定」的重試/標記完成/放棄可直接在收件匣操作（先前這類任務甚至不會出現在收件匣）；新增「已處理」個人整理標記（處理完的沉底，不消失）。舊審批頁保留書籤相容並加導流橫幅。
- **儀表板決定同步收斂通道卡片**：在儀表板按同意/婉拒/重試後，先前推到通訊軟體的對應卡片也會就地改寫成結果、移除按鈕。
- **學習訊號帶話題脈絡**：記憶頁的系統學習紀錄現在會記下該次學習關於哪些話題（列表列與卡片顯示「話題：…」）；既往紀錄寫入時未存話題，維持原狀不假造。
- **真人接手對話（Human takeover）**：已在儀表板完成**已驗證**通道綁定的管理員／主管，只要在通道對話裡**直接發言**，AI 就會停止回覆那一個對話（預設 60 分鐘，`config.toml [takeover] duration_minutes`）——不用按任何按鈕，發言本身就是宣告。判定刻意保守：一般員工帳號、未驗證的綁定、陌生帳號、換一個通訊軟體的同名 id 一律不觸發；**尚未有任何人綁定通道帳號的部署完全不會自動接手**（那種情況下唯一可用的身分證明是「訊息來自設定的目的地」，在通道裡等於「你自己」，套用的結果會是老闆講第一句話 AI 就永遠閉嘴）。接手成立時一次完成三件事：暫停該對話的 AI 回覆、把該對話產生且尚未結束的自主任務標記為由你處理（看板不再顯示成「AI 在做」）、活動流記一筆。接手期間**每一條**會把 AI 訊息送進該對話的路徑都被擋住，不只是「不開新工作」：通道回覆（訊息仍記入對話紀錄，AI 恢復後脈絡完整）、自主任務派工（凍結而非轉人工——要找的人正在現場）、任務進度／「等你決定」卡片／任務結束通知／自動規則跳閘卡片與一般週知／待確認推播（延後到交還後合併投遞，按鈕完整保留）、任務看板叫醒（略過）、主動關懷訊息／例行工作結果／交辦回報（丟棄——遲到一小時的「你已經連續工作兩小時了」是錯的訊息，不是晚到的訊息）。每次略過都寫日誌，不會有無聲黑洞。**三種「須處理」級通知刻意不擋**（高風險動作核可、安裝申請簽核、通道故障告警）——這一級本來就不受勿擾時段限制，且與被接手的對話無關，為了少一次打擾而壓住「這動作可能不可逆，要同意嗎？」是拿真實風險換方便。生命週期：`/takeover` 查詢、`/takeover +30m` 延長（上限 12 小時）、`/takeover end` 提前交還、到期自動恢復；管理者每發一句話計時重新起算。接手／交還時對話裡會發「👤 <名字> 已接手對話」／「🤖 AI 已恢復回應」（名字取自儀表板顯示名稱，未設定時顯示「管理員」，**永不外露通道帳號 id**）。狀態只寫 `~/.duduclaw/takeover_state.json`，**不動任何全域設定或 `agent.toml`**；接手範圍是單一對話，同一位 AI 員工在別的群組／私訊照常回覆。新增唯讀 `takeover.list` RPC（主管以上）；刻意沒有寫入端點——接手是「人在那個對話裡」這件事本身，做成儀表板按鈕會產生第二套授權模型並讓人「接手」一個自己不在場的對話。見 [docs/features/42-human-takeover.md](docs/features/42-human-takeover.md)。
- **常駐感知＋訊號喚醒（Resident Sensing）**：外部資料流（行情輪詢、日誌檔追蹤、任意指令輸出）現在可以常駐接進 autopilot 事件匯流排——新增 `tick` 事件與 `config.toml [tick]` / `[[tick.sources]]`（`http_poll`／`command`／`file_tail`／`websocket` 四種來源，**預設關閉**）。`websocket` 來源掛著一條連線收推播（每則文字訊息一筆觀測），本機以外一律要 `wss://`、明文 `ws://` 只准 loopback，斷線走指數退避重連（起點 `interval_secs`、上限 60 秒、含抖動），二進位訊息丟棄並記入新的 `non_text` 丟棄原因。每筆數值型觀測欄位自動衍生 `prev_<f>`／`delta_<f>`／`pct_<f>` 三個比對欄位，規則不用學新運算子就能寫「漲跌幅 gt 2」。deterministic 規則（含既有 CEP 時序判斷）命中後，可選掛一道**本地模型初篩**（`action.screen`，只走本地推理、絕不外呼雲端）擋掉不值得喚醒 AI 員工的雜訊；初篩逾時／不可用／回覆解析不出 YES/NO 一律依 `on_unavailable` 政策處理（預設放行，可設為攔截）。tick 事件預設不落 `events.db`（近期歷史留在記憶體環形緩衝，256 筆／來源，可選 `persist_every_n` 稽核）；喚醒 `delegate` 的提示詞可附最近觀測窗口（`context_ticks`，預設 10、上限 50）。`command` 來源需全域 `allow_command_sources = true` 才會執行（fail-closed）。儀表板 Autopilot 分頁新增「即時監控來源」唯讀卡片（`ticks.sources`／`ticks.recent` RPC），另新增 `tick_events_total`／`tick_dropped_total`（`reason` 含 `rate_cap`／`unchanged`／`oversize`／`fetch_error`／`non_text`）／`tick_screen_total`／`tick_wakes_total` 四個 Prometheus 指標。`websocket` 來源另有閒置看門狗與主動 ping（`ping_interval_secs` 預設 30、`idle_timeout_secs` 預設 300，任一設 `0` 關閉），把「連著但對方早就不推了」的假活連線抓出來立刻重連；`http_poll` 與 `websocket` 可設自訂 `headers`（最多 8 個，拒收傳輸層保留頭與含 CR/LF 的值，**值永不落 log、API 只回數量**），兩者發出請求／建立連線的當下都會重新解析 DNS 並要求所有 IP 皆為公網位址後才釘選連線（DNS rebinding 防護）。真實行情流實測後另修三處：漲跌欄位的比較基準改為**逐欄位**「上次真的有值的那一次」（穿插的心跳訊息不再洗掉價格基準，實測九成 tick 算不出漲跌的問題）、抽取到的**數值字串自動轉成數字**（Kraken／Binance 的價格都是字串，原本 `gt` 與 delta 全部靜默失效；前導零如 `"007"`、前導 `+`、`inf`／`NaN` 一律不轉）、設了 `json_fields` 卻**一個都抽不到的訊息不再當成觀測**（丟棄並計入新的 `no_fields` 丟棄原因，不進匯流排也不佔環形緩衝；沒設 `json_fields` 的來源與非 JSON payload 的 `raw_len` 行為不變）。`delta_` 欄位比照 `pct_` 一併四捨五入到小數六位——浮點減法的末位雜訊（`63724.8 - 63724.7` 原始值是 `-0.10000000000582077`）會讓 `delta_price gt 0.1` 這類門檻被翻轉；`prev_` 保持 feed 原值、整數 delta 維持整數。另新增兩個設定：每來源 `baseline_max_age_secs`（預設 3600、`0` = 永不過期）讓漲跌比較基準有保鮮期——某欄位停報一整天後再出現時，不會拿一天前的舊值算出一個假的巨幅漲跌，而是視同首筆（三件套缺席）並就地重立基準；全域 `[tick] dns_ttl_secs`（預設 60、`0` = 每次解析）讓已通過內網檢查的解析結果在 TTL 內重用，1 秒輪詢的來源不必一天查 8 萬次 DNS（快取的是已驗證的公網位址集，rebinding 需要新鮮解析才能翻轉，`web_fetch` 不受影響）。見 [docs/features/41-resident-sensing.md](docs/features/41-resident-sensing.md)。

- **跨觸發來源的「近期自身行動」注入**（`config.toml [memory] recent_actions_enabled`，預設開；`recent_actions_count` 預設 10、上限 50）：同一位 AI 員工的排程／心跳／目標迴圈與頻道對話先前互相看不到彼此做過的事——被問「這筆單是你下的嗎」時只查即時工具狀態（被風控攔下的委託在券商端根本不存在），於是斬釘截鐵否認自己五分鐘前才寫進日誌的決策。現在每次呼叫（頻道回覆與派工／排程／心跳兩條路徑都涵蓋）開場前，從稽核日誌 `tool_calls.jsonl` 彙整該員工近 24 小時實際執行過的工具呼叫（含失敗與被攔截的 ❌ 行動，這正是即時狀態看不到的那一類），壓成一段短摘要注入提示詞尾端（不進 prompt cache 前綴），並明確指示「回答『你是否做過某事』以耐久紀錄為準，即時工具查詢不可作為唯一依據」。只讀檔案尾端 256KB、連續重複行動合併計數、無近期行動時完全不注入（零噪音）；資料來源是 MCP 層在工具實際執行後寫入的程式化證據，非 AI 自陳。見 [docs/todo/TODO-agent-cross-invocation-continuity.md](docs/todo/TODO-agent-cross-invocation-continuity.md)。

### Wave 3 — 周邊態與人格
- **真人接手(takeover)**:已驗證的管理者在通道對話中發言即自動接手——AI 對該對話暫停回覆 60 分鐘(`/takeover +30m` 延長、`/takeover end` 提前結束、到期自動恢復),期間該對話的全部自動派發(goal 迴圈、心跳、排程回覆、轉發)凍結或延後,不會出現「真人處理完 AI 舊訊息突然冒出來」;接手/恢復都有明確揭露訊息。判定保守:僅限已綁定儀表板身分的 Admin/Manager,一般使用者發言絕不觸發。
- **經驗法則看得見**:AI 員工歸納的行為法則以白話呈現(模板改寫,零 LLM 成本)——儀表板卡片口語版為主+「為什麼有這條」證據句;通道 `/rules` 列出生效法則、`/rules off <N>`(管理者)可就地停用;試行結果計入每日摘要。
- **貼 ID 直達**:⌘K 搜尋貼上任務/審批/員工/對話 id 直達該物件;主要列表頁篩選狀態同步進網址,可書籤可分享。
- **開發者面板**:`~` 鍵召喚的底部面板(事件流/通知明細/系統與接手狀態),三段收納,收到最小仍保留告警徽章;僅 Manager+ 可見。
- **伺服器端遠端導航**:審批即將逾時時,已開啟的儀表板自動導向該筆(正在編輯表單時降級為可點提示,不強制跳轉)。
- **`duduclaw docs <topic>`**:終端機列出/開啟對應文件(清單直接解析 docs/README.md,不另建會過時的副本)。

### Wave 2 — 雙模深化
- **首頁 overview-first**:頂部一行健康摘要(N 位待命/M 件等你決定/K 件沒送出去)+「需要你處理」清單(點擊直達收件匣對應項)+「你不在的時候」聚合區塊(以上次訪問為錨,聚合成事件類型而非逐筆 log);全綠時顯示安心態。
- **通道「行為與存取」設定進儀表板**:只在被 @ 回、自動開討論串、允許/封鎖名單、配對要求、`admin_users`(誰能下停止指令)終於在通道頁可見可管;與通道端既有工具共用同一驗證與儲存層;`admin_users` 僅限儀表板寫入(AI 員工不能自我授權),全部寫入過稽核。
- **「在通道中開啟」反向直達**:任務/待辦決定/安裝申請詳情頁一鍵跳回產生它的那則對話——Telegram(私聊+群組訊息級)、WhatsApp 立即可用;Discord/Slack/Teams 座標開始持久化(收訊即記錄,建任務時快照),LINE/飛書/Google Chat 平台無此能力則誠實不顯示按鈕。
- **通知治理**:勿擾時段(`quiet_hours`,FYI/需確認級延後合併投遞,需操作級照發)、每日摘要(預設關,有事才發)、每類通知的行動率量測(`notify.stats` RPC,低於 50% 即視為 broken 的判準內建);通道故障記錄補平台欄位與恢復事件。
- **收件匣單筆直達**:`/inbox?item=<id>` 深連結落在該筆並捲動到位;通道推播與首頁清單全部帶單筆座標。
- **修復**:`wiki_scope.update` 對格式錯誤的政策檔先前會靜默清空其他 namespace 的宣告(fail-open),已改為拒絕寫入;首頁「等我處理」小工具漏算等你決定類任務。

### 語彙統一
- 全站狀態詞依同一張語彙表對齊（通道與儀表板逐字相同）：「需人工」→「等你決定」、「受阻」→「卡住了」、「失敗」→「沒做完」、「核准/退回」→「同意/拒絕」（安裝申請用「婉拒」）、「審核中」→「驗收中」，三語（zh-TW/en/ja-JP）同步。

### Changed
- **通道故障記錄補平台欄位＋恢復事件**：`channel_failures.jsonl` 的所有寫入點只要能從 session id 推出平台，就補上 `channel` 欄位（`channel_reply_silent` / `channel_reply_fallback` / `runtime_fallback_substitution` / `trajectory_anomaly` / `foresight_alarm`）；推不出平台的（cron / bus / heartbeat 等內部 session，以及只拿得到工作目錄的 PTY fallback）寫 `null` 或省略，不假造歸屬。通道從告警狀態恢復時寫一筆 `{"event":"channel_recovered","channel":…,"resolved":true,"resolves":…}`；舊的失敗行不改寫（append-only 稽核檔），儀表板靠「同一 channel 有沒有更晚的恢復事件」判斷故障是否仍相關。舊行沒有 `channel` 欄位照常解析。
  - ⚠️ 連帶修正：通道故障告警的判定條件從「有 `channel` 欄位」改為「`event` 在送出失敗白名單內 **且** 有 `channel` 欄位」。少了這步，上述補欄位會讓每次 LLM 逾時、每次軌跡異常都被誤判為通道斷線而告警。白名單目前只有 `telegram_send_failed`。
- **「待辦決定」收斂為單一介面**：安裝簽核、自主任務卡關、自主任務啟動核准、高風險動作核可、自動規則跳閘這五種「要你點頭才會繼續」的事，先前各有一套按鈕編碼、各一套授權規則、各一段通道路由。現在共用同一套：
  - **授權統一**：自主任務的按鈕先前完全不驗證按的人是誰（任何看得到卡片的人都能重試／標記完成／放棄別人的任務），現已套用與核可按鈕相同的規則——已綁定儀表板身分者依角色（管理員／主管），未建立任何身分系統的單人部署則只認收到卡片的那個帳號本人。群組收到的卡片不會讓群組成員自動取得決定權。
  - **文案統一**：按鈕與結果沿用同一組詞（已同意／已拒絕／已婉拒／已重試／已標記完成／已放棄／已暫停）。安裝申請的退回改稱「婉拒」（語氣較軟）、啟動核准的「已核准」改稱「已同意」、逾時改稱「逾時未決，已自動拒絕」。
  - 舊按鈕仍可用：輪替期間通道上尚未被按的舊卡片照常運作。

- **「測試通道」按鈕改為真測試**：實際送出一則測試訊息到該通道，成功判準是通道裡真的收到；無可用目的地時誠實回報「僅驗證憑證存在，未實際發送」（琥珀色警告），不再把「token 欄位非空」當成功。
- **儀表板改設定後通道端有感**：行為邊界（CONTRACT）或模型變更後，該員工下一輪回覆末尾附一行「（行為規則已於 X 更新）」/「（已切換至新模型）」，只提示一次。

### Fixed
- **五個通道的「回覆／引用訊息」內容靜默遺失**：使用者長按一則訊息選「回覆」再追問時，被引用的內容先前完全沒有帶進 AI 的輸入——AI 只收到新打的那句話，誠實地回「我沒看到你說的那段」，體感像裝傻。全通道掃描後確認這是系統性缺口（11 個通道的接收路徑全數未解析引用上下文），本次修復 payload 內已含引用內容、零額外 API 呼叫的五個：**Telegram**（`TgMessage` 補宣告 `reply_to_message`，serde 先前對未宣告欄位靜默丟棄；被引用者是 bot 自己時明確標示「你（bot）先前發送的訊息」——使用者引用 bot 通知來追問正是最常見情境；純媒體引用給型別占位說明）、**Discord**（`referenced_message` 內嵌完整內容，先前整個被忽略；已刪除的引用來源誠實跳過）、**Slack**（「分享訊息」的 `attachments[].text`＋作者標示；純連結展開預覽不誤判為引用；分享而未加註解的訊息先前會被當空訊息整則丟棄，一併修正）、**Teams**（引用內容藏在 `text/html` 附件的 `<blockquote>`，現擷取並去標籤）、**WhatsApp**（`context` 物件先前未宣告——平台只給被引用訊息的 id 不給原文，故標註「使用者引用了一則先前訊息」並標示轉發訊息，不假造引用內容）。五通道共用同一個引用區塊格式（`channel_format::format_quoted_context`，CJK 安全截斷 2000 bytes）。見 [docs/todo/TODO-telegram-reply-context.md](docs/todo/TODO-telegram-reply-context.md)。
- **群組「只在被 @ 時回覆」模式忽略對 bot 訊息的直接回覆**：Telegram／Discord 群組開 mention-only 時，使用者對 bot 的訊息按「回覆」追問（最自然的對話手勢、不會另打 @）先前被靜默略過。現在回覆 bot 的訊息視同提及。
- **Telegram 轉發訊息來源遺失**：`forward_origin` 先前未解析——轉貼進來的內容無法與使用者本人發言區分。現在標注「由使用者轉發，原始來源：〈使用者／頻道／群組名〉」。
- **一人決定後其他人的卡片沒收斂**：安裝簽核會同時推給多位簽核人的多個通道，先前只收斂按鍵者自己那一張，其餘卡片仍顯示可按的按鈕（按下去只得到「已被處理過」）。現在一次決定會回收該筆決定的**所有**卡片。
- **任務類自動規則跳閘後推播無處可去**：`task_created`／`task_updated` 事件只提供 `task` 欄位，因此以 `task.assigned_to` 篩選的規則（最自然的寫法）在跳閘時解析不到通知對象，保護通知被靜默略過。現在也接受 `task.assigned_to` 作為規則的目標 AI 員工。
- **六個平台 sender 誤報成功**：Telegram/LINE/Discord/Slack/WhatsApp/Feishu 的發送實作先前只檢查傳輸層結果、從不檢查 HTTP 狀態與回應體——被平台撤銷的 token 或 `channel_not_found` 都會回報成功。現在逐平台檢查（Slack/Feishu 的失敗訊號在 JSON body，HTTP 200 也可能是失敗）。
- **Google Chat / Teams 純文字通知靜默丟失**：`send_plain_text` 路徑先前對這兩個平台落到空實作、訊息無聲消失，現正確路由到對應 sender。
- **Telegram 決定按鈕未真正移除**：先前按鈕按下後 `editMessageText` 沒清 `reply_markup`，按鈕仍殘留可再點。
- **收件匣審批詳情的 TTL 欄位永遠顯示「剛剛」**：未來時間戳誤用了 time-ago 格式化。
- **`[gateway] port`/`bind` 寫了等於沒寫**：`duduclaw run` 先前只讀 `DUDUCLAW_PORT`/`DUDUCLAW_BIND` 環境變數（預設埠 18789），完全不看 `config.toml [gateway] port`/`bind`——即使 `write_minimal_config` 在首次開機時就把使用者選的埠寫進 `config.toml`，第二次之後的每次啟動都會忽略它、悄悄退回 18789。同時 `deep_link::dashboard_base_url`（通道推播裡「👉 前往儀表板」連結的來源）與 MCP OAuth 的 redirect URI（`mcp_oauth::redirect_uri`）都是直接讀 `config.toml [gateway] port`，兩邊分岔的結果是：手改設定檔的埠之後，通知裡的連結與 OAuth 回呼位址都指向一個沒有服務在監聽的埠。新增 `duduclaw_core::gateway_bind_for_home`/`gateway_port_for_home`（優先序：環境變數 > `config.toml [gateway]` > 內建預設，兩者共用同一份解析邏輯，不會再各自分岔）：`duduclaw run`、`duduclaw service stop`（macOS，先前用同樣過期的埠去找程序，導致「服務已停止」但其實還在跑）、MCP OAuth redirect URI、A2A Agent Card 的 `url` 欄位全部改走這份共用解析。啟動横幅印出目前生效的 bind/port 各自來自 env／config.toml／預設值。既有行為（環境變數與設定檔都沒設定）不變。

## [1.54.0] - 2026-08-10 — 校準式 forward model + held-out 學習閘

### Added
- **校準式 forward model + held-out 學習閘**（v1.54）：任何 AI 員工行動前先對「這一步
  會不會成功」落檔一個機率預測（`TaskPrediction.confidence`），事後用工具實際回傳的
  結果（外部證據，不是 AI 員工自我陳述）計算 proper-score（Brier / RPS，有界分數，
  拒用小樣本下不穩定的 log score），並做 Murphy 分解（reliability / resolution /
  uncertainty）。只有鑑別力（resolution）隨時間上升才算真的學到預測模式，可靠度
  單獨變好只代表學會了保守報均值。自我反省產生的候選教訓不再自己判斷是否可信：
  有工具紀錄／稽核可查證的教訓照舊直接採用，沒有程式化證據的歸納型教訓先進
  shadow（不注入提示詞），累積樣本外命中紀錄、用 Wilson 信賴區間下界（多候選時
  Bonferroni 校正）贏過凍結基準才轉正，轉正後表現退步會自動降級或退休（keep-better，
  歷史保留不刪除）。系統只給三種誠實結論：`SUPPORTED` / `CANDIDATE` /
  `INDISTINGUISHABLE_FROM_LUCK`。樣本不夠時「還不知道」是合法輸出，不會為了顯得
  有用硬給模糊的「初步驗證」。shadow 候選的樣本外晉升閉環已接通：每次任務結算對
  觸發命中的 shadow 候選記一筆樣本外命中，打贏凍結的高風險基準率就自動轉正注入。
  這套能力 **v1.54 起預設開啟**（`enabled` / `calibration_enabled` /
  `held_out_gate_enabled` 三層皆預設 `true`，冷啟動零 LLM），每個 AI 員工開箱即用；
  可在 dashboard「進階設定 → 預測校準」或 `config.toml [task_forward_model]` 逐層關掉，
  關掉那一層即回到與本功能之前 byte-identical 的行為。能力與具體業務無關，任何 AI
  員工（客服、coding、操盤等）皆適用。見
  [`docs/features/39-calibrated-forward-model.md`](docs/features/39-calibrated-forward-model.md)。
- Dashboard「進階設定 → 預測校準」分頁：admin 可即時切換上述三個開關（`task_forward_model.get`
  / `.set` RPC，部分更新保留 `config.toml` 其他區段；`enabled` 變更需重啟 gateway 生效）。

## [1.53.0] - 2026-08-07 — 任務層世界模型與 AEE 進化引擎

### Added
- **問題回報與建議網頁**：`https://zhixuli0406.github.io/DuDuClaw/` 上線一張
  中文回報表單（GitHub Pages 靜態頁，零自建伺服器、前端零秘密）。使用者填完
  表單導向 GitHub issue 預填頁送出（截圖/影片直接拖曳上傳）；帶表單標記的
  issue 由 GitHub Actions 觸發 `claude-haiku-4-5` 做語意分類（bug/enhancement/
  question/documentation + 嚴重度）、標題正規化與內容格式化，自動上
  `feedback` 與分類標籤，原文保留在 `<details>`。issue 內容降格為資料
  （XML 包裹 + JSON schema 約束輸出）防 prompt injection；workflow 以
  `gh api` + `jq` 組請求防 script injection。需設 repo secret
  `ANTHROPIC_API_KEY`，未設時自動略過整理。見 `docs/guides/feedback-page.md`。
- **A2A 部門與階級隔離**（WP21）：團隊多人協作時，委派權限現在遵循組織結構與部門邊界。
  三層政策（`config.toml [delegation] policy`）自主選擇：`department`（預設，上下級+同部門橫向）、
  `hierarchy`（僅上下級）、`open`（舊行為逃生門）。跨部門合作支援精準白名單配對
  （`allow = [["sales-lead", "warehouse-lead"]]`，無序=雙向）。MCP 前門(`send_to_agent`/`spawn_agent`)、
  任務派遣（`tasks_create` / `tasks_update`）、多步驟計畫（`create_task`）、
  例行工作（`schedule_task`）、bus 隊列消費皆執行驗證；手動寫入 bus_queue.jsonl
  偽造發送者會被拒絕並寫入 audit log（本版對「完全沒有發送者欄位」的舊格式任務仍
  放行並記 warning，下一版轉為拒絕）。Dashboard「進階設定 → 委派權限」卡讓管理員
  即改即生效，無需重啟；`[acp] trusted = true` 可開通外部 A2A 呼叫（預設拒，
  文件附風險警語）。見 `docs/features/37-delegation-isolation.md`。

### Added
- **MCP caller 身分識別**（WP21 進階安全）：gateway 啟動時自動在 `~/.duduclaw/identity.key`
  生成一組 256-bit 隨機密鑰（檔案權限 0600），spawn 子 agent 時自動生成簽名身分 token（HMAC-SHA256 綁定 agent id），
  注入 `DUDUCLAW_AGENT_TOKEN` 環境變數。MCP server 端驗證 token，無效身分會被拒絕。
  `config.toml [delegation] require_identity_token = false`（預設軟模式：缺失/無效只警告；設為 true 進入嚴格模式，無效身分直接拒絕 MCP 啟動）。
  升級順序很重要：先重啟 gateway 讓所有 agent 的 MCP 設定重新簽署，再開嚴格模式，順序反了會 MCP 拒絕開機。
- **組織資料寫入保護**（WP21 權限邊界）：agent 經 Write/Edit/Bash 工具變更合法變更改由 `agent_update` 或儀表板統一管道。
  凍結目標包括：`agent.toml [agent]` 的 `name`、`reports_to`、`department` 欄位；`config.toml [delegation]` 與 `[acp]` 全段；
  `.mcp.json` 內的 `DUDUCLAW_AGENT_ID` 與 `DUDUCLAW_AGENT_TOKEN` 區塊；`.claude/settings.json` 整個檔案；`identity.key`。
  PreToolUse hook 會攔截嘗試並記入 `tool_calls.jsonl` 帶 `org_placement_denied` 標記。
- **白名單名稱正規化**（WP21 使用者友善）：儀表板委派權限卡接受 agent 的顯示名或目錄名，儲存時自動正規化為目錄名，
  與判定端保持同一名稱空間（目錄名），避免顯示名異動導致配對失效。
- **團隊佈署自動部門**（WP21 批量設定）：dashboard 一鍵佈署團隊時，佈署套件中的所有 agent 成員（前台人物 + 背景 worker）
  的 `[agent] department` 欄位自動設為該產業代碼（來自 `team.toml [team] industry`）；產業包單獨安裝時也一併帶上 department。
- **組織資料的權威儲存**（WP22 加固第二輪）：組織結構（誰匯報給誰、部門）的權威來源改為中央 `~/.duduclaw/org.toml`。
  Gateway 首次啟動時自動從各 agent 設定匯入建立（`--seeded` 標記檔防重複），之後組織變更一律透過新指令 `duduclaw org sync`（CLI 操作者終端執行）
  或儀表板進行，不再從 agent 目錄下 `agent.toml` 的組織欄位自動吸收。手動編輯 agent.toml 的 `reports_to` / `department` 後無法自動生效——必須執行
  `duduclaw org sync` 同步。`duduclaw org show` 檢視目前的組織結構。`duduclaw doctor` 會偵測權威檔遺失、鏡像與權威不一致（漂移）等狀態。
- **跨員工檔案隔離**（WP22 邊界強化）：AI 員工無法透過 Write/Edit/Bash 工具修改其他員工目錄下的任何檔案
  （含 SOUL.md、記憶、設定等）；`~/.duduclaw/config.toml` 對所有員工整檔唯讀。非 Claude runtime
  (codex/gemini 等) 在 workspace-write 沙箱下寫不到 `~/.duduclaw/` 目錄，從而無法篡改自己的組織設定影響委派判定（FullAccess 沙箱除外，屬操作者顯式選擇）。
  跨員工的合法變更（改部員的 reports_to/department）必須走儀表板或 `agent_update` MCP。
- **設定檔權限自動化**（WP22 安全基線）：含身分驗證 token 與 MCP 身分的執行時設定檔（`.mcp.json`、`identity.key` 等）
  自動收緊為 0600（檔案所有者唯讀），防止其他進程讀取。
- **重名防禦**（WP22 完整性）：建立與既有員工顯示名或目錄名重複的新員工時被拒；委派白名單輸入歧義名稱時要求改用目錄名。
  系統保留名稱(`dashboard`, `webhook`, `cron` 等) 不能再用來建立 AI 員工。
- **任務層 forward model**（Harness→LWM 方案 A）：goal 任務派工前依歷史統計預測本輪工具使用與成敗，
  驗收後與實際軌跡（`tool_calls.jsonl`，程式化證據、不採 AI 自述）做 diff 累積統計，全程零 LLM、四階退階、
  對五種 runtime 一視同仁。`config.toml [task_forward_model] enabled` 控制，**預設關閉**；關閉時派工行為逐位元組不變（有整合測試證明）。
- **goal 任務結構化狀態**（StateAct）：每輪派工 prompt 帶 `<state>` 區塊（目標／已確認事實／待驗證假設／已排除做法）。
  已排除做法由判官駁回紀錄程式化推導；已確認事實只來自零 LLM 檢查（outcome spec、grounding）通過項；
  假設由 AI 員工經 `<state_update>` 標記自報、逐輪回填（解析失敗沿用上輪，不臆測）。狀態隨任務持久化，重啟不丟。
- **goal 任務探索記帳**：`(state, action)` 訪問圖取代舊震盪偵測——同一狀態第二次駁回重派即升級人工介入
  （時機與舊版一致），重複嘗試過的做法會在 prompt 中顯式標註禁止重複。
- **驗收 grounding 前置檢查**：goal 任務結果宣稱在進 LLM 判官前，先以零 LLM 檢查與工具實際回傳內容的重疊
  （`tool_calls.jsonl` 現會記錄遮罩後的工具輸出摘要）。自我回音類工具（`tasks_complete` 等）不計入證據；
  查資料型任務（read-only 工具）自動跳過不誤殺。`[dispatch] grounding_precheck_enabled` 控制（預設開，保守門檻）。
- **審批附模擬後果**（simulate-before-act）：不可逆工具的審批訊息與 goal 任務 `needs_human` 推播，
  現在附上「若核准，接下來預計發生什麼」的模擬敘述與風險點（一次 LLM 呼叫、15 秒逾時、失敗降級照舊出按鈕），
  模擬參考受保護的共享 wiki SOP（唯讀 namespace）。可逆性判定邏輯不變、解析失敗照舊 fail-closed 需要人工。
- **記憶反偽驚訝閘門**：新語意記憶／反思規則寫入前先與既有內容比對相似度（char n-gram cosine，閾值 0.92，
  與 playbook 去重同款），近似重複拒寫並記 audit，防記憶庫自我膨脹。`config.toml [memory] novelty_gate` 控制（預設開），
  與語意向量檢索開關（`DUDUCLAW_SEMANTIC_VECTORS`）解耦。時間性事實更新（supersession）不受影響。
- **錯誤筆記證據化**（Honest Lying 防護）：MistakeNotebook 條目新增程式化軌跡證據欄位
  （哪個工具回錯、哪條檢查失敗），生產寫入路徑全數自動附上確定性證據；
  無證據的純 AI 自述診斷不再參與反思規則合成，防止錯誤自我診斷被固化傳播。

- **任務經驗規則管線**（WALL-E 式 induce/update/prune）：forward model 的預測誤差達 Significant 以上時，
  從偏離維度**確定性合成**一條任務層規則（零 LLM、只陳述偏離不臆測原因），經反偽驚訝閘門後寫入語意記憶，
  下輪派工注入「## 任務經驗規則」（上限 2 條），依下輪結果 helpful/harmful 結算、淨分歸零自動退休——
  全部複用既有規則生死簿機制，不開新帳本。`[task_forward_model] rule_induction` 控制（隨總開關，預設關）。
- **原生工具可觀測性升級（Full 保真度）**：goal 任務派工路徑上，claude／codex／gemini／openai-compat
  四種 runtime 的原生工具呼叫（不只 MCP 工具）現在會被收集，並同時餵給三個消費者——
  forward model 觀測（工具類別取聯集、次數取 max 不重複計數）、驗收判官的工具證據區塊
  （原生事件標注 `(native)`，判官不再把檔案操作誠實完成的工作誤判為「零工具證據」而退回）、
  grounding 驗收的分級理由。原生事件並攜帶遮罩後的輸入／輸出文字摘要（僅供 grounding 比對，
  不進判官 prompt），使誠實引用工具原文的回覆在 goal 任務上能真正達到「有佐證」判定，
  回音扣除規則同樣適用。antigravity／grok 無結構化工具事件，維持 MCP-only。
  codex/gemini 的事件欄位名以雙名容錯解析，建議真 CLI 環境抽查後再依賴。
- **審批模擬軌跡上儀表板**：審批詳情與卡片列表現在顯示「若核准，接下來預計…」模擬敘述與風險點
  （無模擬資料時不顯示）；自主目標啟動核准（kickoff）訊息也附啟動後前三步預覽。
- **記憶去重閘門開關**：儀表板系統設定新增「記憶去重閘門」開關（`[memory] novelty_gate`），
  套用於下一次新對話。覆蓋面含 AI 員工經 MCP 工具主動寫入，以及閘道器內部的語意記憶
  自動寫入路徑（反思規則合成、任務經驗規則、人格歸納、知識庫指標等——經統一工廠建構）。
- **playbook 條目 E1 斷言**（Evolution v3 WP2.8，D8 拍板）：新的 playbook 條目必須自帶
  機器可驗的合規形狀（必用／禁用工具、輸出必含／必不含子串），寫入時結構驗證、
  對已錄製的 eval case transcript 零 LLM 重放（違反即決定性否決；無錄製則降級為提示，
  隨 transcript 錄製逐案自動武裝）。既有條目不回填。
- **演化手段稽核**（Evolution v3 WP2.10，D11 拍板）：playbook 演化提案除分數外同時稽核
  「達成手段」——題庫洩漏（條目背誦測驗題）、驗證器弱化（恆真斷言）、失敗抑制指令
  三類簽名決定性否決；judge 取悅類記入遙測與提示（不否決，待真實分佈再調閾值）。
- **SOUL.md → playbook 遷移工具**（Evolution v3 WP1.4）：`duduclaw playbook migrate-soul --agent <id>`
  從 SOUL.md 的行為規則分區抽取歷史累積的規則成人審草稿（身分分區永不觸碰），
  審閱補上 eval case 連結與 E1 斷言後 `--apply` 走標準驗證管線入庫，逐條回報接受／拒絕。
- **題庫基線 transcript 全量錄製**（Evolution v3 WP2.1 收尾）：18 個產業 agent × 360 題的
  行為題庫已在隔離沙箱完成真機錄製並全數通過健康檢查，離線 replay 全套件可跑——
  自我進化引擎的 case 評分維度與 E1 斷言重放自此有真實資料可用。基線現況 98 通過／262 失敗
  屬預期（題庫的目的就是暴露行為缺口）。
- **eval 錄製隔離修正**：live 錄製時 agent 的 `.mcp.json` 改寫為指向本次 eval home 的臨時副本
  （原檔不動），且 `DUDUCLAW_MCP_API_KEY` 一律換成佔位值——修掉「沙箱錄製仍把工具副作用
  寫進生產資料」與「生產 MCP 金鑰被 CLI init 事件回顯進 transcript」兩個隱患；
  `error_max_turns` 的 run 改為可評測的失敗基線（工具活動完整錄下，由 `max_tool_calls`
  等斷言判失敗），僅基礎設施錯誤（限流／session 上限）維持硬錯誤不入庫。
- **題庫隨產業包安裝**（方案 A）：從產業板模建立 AI 員工時，若該角色隨附行為題庫，
  自動安裝到 `<home>/evals/<員工名>/`（改名部署會同步改寫題目的 agent 欄位；
  既有題庫目錄永不覆蓋）——包裝出的員工第一天就有自我進化的評測基線。
- **`duduclaw eval-scaffold`**（免費功能）：從 agent 自己的 SOUL.md 行為規則產生 eval
  草稿題（零 LLM、身分分區不觸碰），落在 `evals-drafts/` 避免未審閱題目混入基線；
  操作者補上 prompt 與斷言、移入題庫後即可 `--record`——自建 agent 也能吃到完整
  演化迴路（playbook 新條目的 eval-case 硬要求自此對免費用戶可達成）。
- **法律事務所團隊補接案／諮詢兩崗**：lawfirm-team 新增 `law-intake`（接案行政）與
  `law-consult`（法律諮詢）兩名 AI 成員（lawfirm 專屬 kit，法律業紅線內建於 SOUL/CONTRACT，
  行為題庫直接對其出題）；原 `law-assistant` 題庫改綁前台總機 `lawfirm-assistant`。

### Changed
- **goal loop 震盪偵測**改由訪問圖結構性判定（對外事件與回饋字串不變）。
- **`tool_calls.jsonl` 輪替上限 5MB→16MB**（記錄新增 result_text 欄位後單列變大，維持稽核保存窗）；
  檔案權限收緊為 0600（記錄現含業務資料），既有寬鬆權限的檔案在下次寫入時自動收緊。

### Fixed
- **goal／cron／heartbeat／autopilot 派工的工具稽核歸屬錯誤**（活體驗證抓到的 P0）：
  worker AI 員工執行的 MCP 工具呼叫，先前會被記到派工者（如 `goal-loop-driver`）名下，
  導致 forward model 觀測永遠拿不到資料、grounding 驗收永遠跳過、驗收判官看不到工具證據而把
  誠實完成的工作退回。現在系統發送者派工時稽核一律歸屬實際執行的員工；真人／員工間委派行為不變。

### Security
- **SOUL.md 人格層對 AI 員工唯讀化**（Evolution v3 WP1.1）：修補一個從未真正生效的閘門——
  MCP 工具 `agent_update_soul` 自承「Bypasses file-protect hooks」可整份改寫 SOUL.md，
  且預設的 in-process agent 身分持有 Admin 權限，實務上 AI 員工可自行改寫（甚至偽裝改寫他人）
  人格檔；唯一理論上的守門旗標 `agent.toml [permissions] can_modify_own_soul`
  過去**沒有任何程式碼讀取它**（`duduclaw-security::rbac` 全專案零呼叫者的死碼），20 個產業板模
  寫 `= false` 但形同虛設。現在此工具對「AI 員工身分」呼叫者一律拒絕（除非該員工的
  `can_modify_own_soul` 明確設為 `true`，且僅能自寫，不得跨員工），操作者／儀表板路徑不受影響；
  Write/Edit/Bash 三個工具也新增 SOUL.md 自寫防護（先前的跨員工檔案隔離只擋改別人的檔案，
  沒擋自己直接用檔案工具改自己的 SOUL.md）。從未生效的 `rbac.rs` 死碼已刪除，
  其唯一有語意的功能（人格自寫旗標）已在 MCP 前門實作，另一功能（`validate_agent_creation`）
  已被 WP21/WP22 的 `delegation_policy` 完整取代。見 `commercial/docs/DESIGN-evolution-v3-aee.md` §1.9。
- **稽核遮罩強化**：工具輸出寫入稽核檔前的機密遮罩補齊多個實測繞過——小寫 `bearer`、
  `Basic`/`Token` 認證頭、多字 passphrase 整行、連線字串密碼（`postgres://user:pass@`）、
  Telegram bot URL token、Slack webhook 路徑、連字號拼法 key（`x-api-key`）、CJK 緊鄰 key
  （中文字與 `password:` 相連時原本整條失配）。全部修補附繞過→遮蔽的回歸測試。
- **狀態回填注入防護**：goal 任務的 `<state_update>` 自報通道、錯誤筆記注入 prompt 的使用者原文、
  審批與 needs_human 推播文字，全面補上 XML 跳脫與資料框定——修補一個可用 121 字元 payload
  偽造「已確認事實」的逃逸路徑（附 PoC 回歸測試）。
- **grounding 證據反自我回音**：任務回寫類工具（`tasks_complete` 等）的輸出不再作為 grounding 證據
  （agent 的自述經回音偽裝成「工具佐證」的通道已封死）；證據比對排除與呼叫輸入重疊的片段。
- **ActionGuard 模擬參考反自證**：可逆性判官的 wiki 參考只讀 `.scope.toml` 標為唯讀／操作者專屬的
  namespace（無政策檔一律不取，fail-closed），agent 本地 wiki 一律不取；參考內容明確限定
  不得作為可逆性判定依據——封死「agent 先寫一頁『此操作可逆』的假 SOP 再繞過人工審批」的路徑。

### Changed
- **組織資料變更流程升級**（WP22 行為變更）：手動編輯 agent.toml 的 `reports_to` 或 `department` 欄位不再自動生效。
  所有組織結構調整一律透過新指令 `duduclaw org sync`（操作者終端執行）、儀表板或 MCP `agent_update` 進行。
  此舉防止 agent 自助修改組織圖影響委派判定，確保組織變更由人類操作者顯式決定。
- **`config.toml` 對 AI 員工整檔唯讀**（WP22 安全邊界）：`~/.duduclaw/config.toml` 現為全局委派政策與 ACP 設定，
  任何 AI 員工都無法透過 Write/Edit/Bash 工具改寫此檔案，防止員工自行調整全團隊委派規則。政策變更由操作者或儀表板進行。
- **Agent 配置鎖定**（WP21 安全升級）：① agent 不再能使用 Write/Edit 工具改寫自己的 `name`、`reports_to`、`department`，
  必須透過儀表板「AI 員工 → 詳情 → 編輯」或 MCP `agent_update` 變更；② agent 不能改寫自己的 `.claude/settings.json` 與
  `.mcp.json` 身分區塊，權限管理走儀表板進階設定、與管理員協商；③ 系統發送者（dashboard/webhook/cron 等）操作不受限。
- **組織層級委派行為變更**（WP21 升級說明）：① 祖父→孫層級派工從「被拒」改為「放行」
  （越級指派現已支援）；② 無從屬關係且不同部門的 agent 互派從「全開」改為「被拒」
  （非同部門/非上下級/非白名單配對一律fail-closed）；③ `create_agent` 省略 `reports_to` 時
  改掛 caller 自己而非 main agent（阻止自助提權漏洞）；④ 系統保留名稱
  （`dashboard` / `webhook` / `cron` / `heartbeat` / `autopilot` / `goal-loop-driver` /
  `a2a-client` / `default`）不再能用來建立 AI 員工；⑤ `[delegation] policy = "open"`
  可恢復舊行為。既有部署若依賴跨部門 tasks_create，升級後錯誤訊息會明確指引改政策或補組織關係；
  強烈建議保持 `department` 預設（即使舊版未做任何檢查，新政策對前門路徑零回歸，只在後門
  路徑對齊安全預期）。⑥ `agent_remove` 現在也受相同的子樹規則約束——只能移除自己
  或自己團隊之下的 AI 員工，系統發送者與 `open` 政策照舊豁免。
- **v1.53 預告**：所有系統派工來源已完成發送者戳記，v1.53 起無發送者標記的佇列任務（舊格式 bus_queue.jsonl 項目）將從「警告放行」改為「拒絕」，
  強制升級到帶發送者的新格式；自動化腳本若直接 append bus_queue.jsonl 需要補上 `sender` 欄位。

### Added
- **Playbook 條目模型（Evolution v3 WP1.2，GEP gene 形）**：進化的新落地目的地。擴建既有
  `rule_lifecycle`（Janus probation 底座不變）為 gene 形 schema——`category`
  （repair/optimize/innovate）、`signals_match` 觸發信號（與 `MistakeCategory`/`FailureReason`
  打通）、失敗歷史、至少連結 1 個 eval case（無連結一律拒絕入庫）、capsule 式應用記錄
  （outcome/score）+ `success_streak`。條目內容強制緊湊（≤400 字元，論文實證擴寫成文件反而
  降效）。確定性 delta 合併（非 LLM）；近重複偵測用 char n-gram cosine，閾值 0.92（刻意保守——
  誤合併會靜默失去一條不同的規則，比多留一條冗餘更糟），命中即拒寫並記 audit；
  per-agent 容量上限 + stale/archive 生命週期（複用既有 Ebbinghaus retrievability，不硬刪）。
  新 CLI `duduclaw playbook export --agent <id> [--out <path>]` 匯出該 agent 目前生效條目為
  GEP-gene 形 JSON（僅本地匯出，不接任何外部 hub，不 vendor 任何第三方程式碼）。
- **注入通道升級（Evolution v3 WP1.3）**：「## Learned Rules」通道從「靜態 net-score 前 3 名」
  改為「信號匹配優先＋分數排序補位」——當前錯誤模式／`FailureReason`／對話關鍵詞會先比對條目的
  `signals_match`，命中的條目優先注入，其餘名額才依既有 net-score 排序遞補；token 預算
  仍受 `prompt_compression` 管線約束，cache 斷點策略不變。
- **Agentic Evolution Engine 迴圈（Evolution v3 WP2.1-2.5）**：GVU 的預設演化路徑改為 AEE——
  每輪先依 `[evolution] strategy` 決定意圖（repair 消化 MistakeNotebook／optimize 精修低
  streak 條目／innovate 探索新規則），Generator 內迴圈（≤3 輪，shadow 套用＋自跑 eval 子集，
  不滿意就改，過程不落地）產出 playbook delta；**Gate/Measure 閘門分離**取代舊 8 層否決鏈——
  Gate（`G-Safety`/`G-Contract`/`G-Canary-Static`/`G-Schema`/`G-Capacity`，零 LLM、保留否決權）
  先過濾，只有通過 Gate 的候選才進 **Measure**（eval case 分數＋L3 judge 降級為一維分數＋
  反諂媚＋新穎度＋mistake 相關度，皆無否決權，判官呼叫失敗記為「缺資料」而非「0 分」）；
  提交閘採 **matches-or-improves**（AVO P7）——候選與現任 **champion**（整份 playbook 快照，
  非單條目比較，避免「改善一條、悄悄搞砸三條」）逐維度比對，落在雜訊帶內視為打平可提交，
  搭配三道防漂移配套。觀察窗判定降到**條目粒度**：條目 confirm/rollback 由其連結的 eval case
  裁定，只回滾退步的那一條，不影響同批其他條目。全程遙測（`gvu/telemetry.rs`）記錄每層拒絕
  原因，供 dashboard 拒絕分佈圖查詢。**AEE 從不寫入 SOUL.md**——SOUL cap 超標的整份壓回仍走
  WP0.2 consolidate 路徑，與 AEE 迴圈正交。多 runtime 中立：eval 重放走子行程 +
  `--report` JSON（不假設常駐 kernel 或特定 CLI stream 格式），LLM 判斷一律經
  `run_utility_prompt` → account rotator，schema 不含任何單一 provider 專屬欄位。
  詳見 `commercial/docs/DESIGN-evolution-v3-aee.md` 第一至三章。
- **`duduclaw eval` 精準選題與跨 crate 重放（Evolution v3 WP2.1/2.2 前置，B4）**：新增
  `--case <id>[,<id>...]`（依 `EvalCaseRef`——case 檔名 stem 精準比對，不同於 `--filter` 的
  人類可讀 `[case] name` 子字串比對，後者唯一性不受強制）、`--exclude-dir <name>`（排除指定
  目錄下的題目，用於 held-out 子集輪替，題庫內同名 id 會被檢查唯一性）、`--report <path>`
  輸出 JSON 報告。AEE 迴圈透過子行程呼叫這條既有 CLI 路徑跑重放（B1 裁定：子行程 +
  `--report` JSON，沿用 `migrate.scan` 的既有跨 crate 模式，不打破 cli→gateway 單向依賴）。
- **新設定鍵**：`config.toml [evolution] eval_suites_root` / `eval_binary`（AEE 重放子行程找
  題庫與 CLI 二進位的路徑，可覆寫預設）；`agent.toml [evolution] legacy_soul_evolution`
  （`= true` 時該 agent 改走舊版 SOUL.md 改寫路徑，逃生門，見下方 Changed）、
  `gvu_cooldown_minutes`（預設 60 分鐘）、`aee_settle_hours`（AEE 條目觀察窗，預設 24 小時，
  上限 30 天）、`strategy`（`balanced`（預設）/`innovate`/`harden`/`repair_only`，決定每輪
  repair/optimize/innovate 配比，取代裸 ε 探索，錯誤值會 `warn!` 並退回 `balanced`）、
  `[evolution.noise_band]`（`cases`/`judge` 雜訊帶寬，供 matches-or-improves 判定）。
- **Dashboard「自主學習」分頁**（記憶頁新分頁）：進化模式總覽（AEE/legacy 統計、啟用 agent
  數）、版本歷史、停滯偵測卡（連續 N 輪全拒／D 天零套用／拒絕原因重複三種訊號）、拒絕遙測圖
  （依驗證層級的拒絕次數分佈，7/30 天可切換）、整併（consolidate）紀錄、Playbook 條目卡片
  （分類/狀態/helpful-harmful/success streak/連結 eval case 數，可一鍵匯出 gene JSON 或
  手動 retire 單一條目）。新增 RPC：`evolution.status`、`evolution.versions`、
  `evolution.stagnation`、`evolution.telemetry`、`evolution.consolidations`、
  `playbook.list`、`playbook.export`、`playbook.retire`。

### Changed
- **進化引擎預設路徑改為 AEE（Evolution v3，行為變更）**：GVU 的預設演化目的地從「整份改寫
  SOUL.md」轉為「擴建 playbook 條目」（見上方 Added）。維持舊行為需在 `agent.toml [evolution]`
  明寫 `legacy_soul_evolution = true`，該旗標下 agent 繼續走本節下方「GVU 進化引擎止血」描述的
  舊版 4 層驗證＋append-only SOUL.md 路徑（含本次的止血修復），**但舊路徑不會再獲得
  AEE 的新防護**（Gate/Measure 分離、champion、條目級觀察窗、停滯偵測告警）。
  升級後兩種路徑並存，非強制遷移；既有 35 版 SOUL.md 歷史可用人審流程半自動抽成 playbook 條目
  （WP1.4，工具尚未隨本版出貨，見 `commercial/docs/TODO-evolution-v3-2026-08.md`）。
- **`gvu_enabled` 預設統一為 opt-in（Evolution v3，行為變更）**：見下方 Fixed 的 R3 說明——
  struct 層預設值原本誤寫為 `true`，現與實際 runtime 閘門一致統一為 `false`。多數線上安裝
  的板模生成路徑本就明寫此 key（受影響有限），但**手動建立、未寫此 key 的 agent**，
  升級後 GVU/AEE 會從「意外啟用」變成「需顯式開啟」——若原本依賴自動進化，
  升級後請確認 `agent.toml [evolution] gvu_enabled = true`。

### Fixed
- **GVU/AEE 進化引擎止血（Evolution v3 Phase 0）**：三個月實證鑑識發現進化引擎不是「不會進化」，
  是被自己的護欄與死鎖絞死、且死了沒人知道。本次修六處根因：
  - **`gvu_enabled` 預設不一致（R3）**：`agent.toml [evolution] gvu_enabled` 的兩套預設方向相反——
    struct 層預設 `true`、實際 runtime 閘門（`gvu::trigger::agent_gvu_enabled`）缺 key 時預設 `false`，
    造成 dashboard 顯示與實際行為不一致。統一為 fail-closed opt-in（預設 `false`），並修正三處
    生成 agent.toml 卻沒明寫此 key／寫成 `true` 的板模生成路徑（`duduclaw wizard`、`duduclaw onboard`
    headless 模式、22 個產業付費板模 `commercial/templates-premium/*-pro`）。
  - **SOUL.md cap 死鎖（R2）**：`SOUL_MAX_LINES`/`SOUL_MAX_BYTES` 過去是單向閥——一旦 SOUL.md
    超過上限，之後所有提案都在同一道閘被拒（"Manual review required" 只寫 log，無人看得到），
    agent 永久卡死無法再進化。新增 consolidate 模式（`gvu/consolidate.rs`）：偵測到已超 cap
    或套用後會超 cap 時，改請 Generator 整份重寫壓回上限，並以六道防護抵禦 context collapse——
    人格分區逐位元組鎖定（SHA-256 校驗）、章節結構鎖定、壓縮後 Measure 分數不得低於壓縮前
    （否則整批拒絕）等；"Manual review required" 現在也會推 dashboard + 通道通知，不再只寫 log。
  - **觀察窗品質閘空轉（R5）**：舊版「對話數 < 5 且超 72h → 無條件 Confirm」導致抽查的 confirmed
    版本 post_metrics 全為 0——安全閥變成了「假裝有證據」的主要來源。移除無條件 confirm，新增
    兩段式訊號：72h 軟性告警（仍延長觀察，但 dashboard 出現一次性提醒）、14 天硬性上限後標記
    `ExpiredNoData`（**不算 confirmed，也不當作驗證失敗**，SOUL.md 內容維持原狀，僅止血不誤判）。
  - **通道路徑繞過 GVU 節流（R4）**：`channel_reply.rs` 的 ε 探索／沉默計時器觸發路徑會直接呼叫 GVU，
    未檢查 `gvu_enabled` 也沒有任何節流——實測某窗口 5 小時內連燒 6 次 GVU（每次 4-5 分鐘 LLM 時間）。
    現在補上 `gvu_enabled` 檢查，並在 GVU 執行入口新增每 agent 冷卻時間
    （`agent.toml [evolution] gvu_cooldown_minutes`，預設 60 分鐘，涵蓋所有呼叫路徑）。
  - **判官順序顛倒（B2）**：確定性 `verify_all` 過去排在 L3 LLM judge 之後，B 窗全拒案例因此
    白付 judge 的 LLM 費用；現在改為先跑零成本的確定性檢查，鐵定會被拒的候選不再燒 judge 錢。
  - **MetaCognition 閾值只降不升（R7）**：`negligible_upper`/`significant_upper` 過去只有收緊
    規則、沒有對稱的回升規則，長期會單向漂移到過度敏感。新增對稱回升邏輯，長期無異常訊號時
    閾值會逐步鬆回預設值附近。
  - **停滯偵測器（AVO §2.4）**：新增 `gvu/stagnation.rs`，每 30 分鐘掃描 `evolution.db`，偵測「連續 5 次
    GVU 皆未套用」「14 天內有觸發但零套用」「拒絕原因重複 ≥3 次」三種停滯訊號，觸發時發 Activity Feed
    貼文 + evolution event + log 警告（同一停滯狀態不重複告警）。
  - 拒絕遙測（每次 verify/apply 記錄層級 + 原因，落 dashboard 拒絕分佈圖）與 MistakeNotebook
    程式化軌跡證據化（`TrajectoryEvidence`，見上方「錯誤筆記證據化」）一併於本輪完成。
  - Phase 1/2（SOUL.md 唯讀化、Playbook 條目模型、AEE 迴圈重排）已隨本版一併出貨，見上方
    Added/Changed 與 `docs/features/38-aee-playbook-evolution.md`。設計全文見
    `commercial/docs/TODO-evolution-v3-2026-08.md` / `commercial/docs/DESIGN-evolution-v3-aee.md`。

## [1.52.4] - 2026-08-05 — 系統更新頁重啟並更新按鈕

### Fixed
- **通知叫你點「重啟並更新」，畫面上卻沒有這顆按鈕**：桌面殼更新下載完成的通知
  指向系統匣選單裡的「重啟並更新」項目，但使用者直覺打開的「系統更新」設定頁
  只有一段說明文字、沒有任何可以立即套用的按鈕——通知指的按鈕根本找不到。
  桌面殼新增 `desktop_update_status` / `desktop_restart_and_update` 兩個 command
  （後者沒有已暫存更新時回錯誤，不會空轉重啟）；設定頁在偵測到殼端已暫存新版時，
  直接在頁面頂部顯示「v{version} 已在背景下載完成」橫幅與「重啟並更新」按鈕
  （不需先按檢查更新，30 秒輪詢涵蓋背景下載中途完成的情況），瀏覽器開啟或舊版殼
  則維持原本純說明文字。桌面版說明文字同步改寫，講清楚按鈕何時會出現。

## [1.52.3] - 2026-08-05 — 桌面 OAuth 彈窗修復+Google 整合自動啟用

### Fixed
- **桌面版 OAuth 授權彈窗全滅**：桌面殼的 wry webview 沒有 new-window handler，
  `window.open` / `target="_blank"` 是靜默 no-op——MCP OAuth（Google/Notion/GitHub）
  的授權頁點了沒反應、CLI 登入連結打不開、所有外連（定價頁、檔案下載）全數死路。
  桌面殼新增 `open_external_url` command（Rust 端驗證 http/https 才交給系統瀏覽器，
  remote 頁面拿不到 opener 原生權限）；前端 `openExternal()` helper 接手三處 OAuth
  `window.open`，並在開機時安裝全域 `target="_blank"` 攔截器涵蓋其餘外連。授權進行中
  面板會顯示可全選複製的授權網址——就算殼太舊沒有這個 command，貼到任何瀏覽器
  一樣能完成（callback 是 gateway 的 HTTP endpoint，輪詢會自動偵測成功）。
- **Google 連好了工具卻到不了 AI 員工面前**：dashboard 完成 OAuth 連線／儲存憑證後，
  `[integrations] google_workspace` 總開關仍是關的——19 個 Google 工具不會出現在
  tools/list，憑證測試綠燈但每次呼叫都死路，唯一的線索是一條要求手改 config.toml
  的警告。現在「完成連線就是選擇加入」：OAuth callback 成功（google）與憑證儲存
  都會自動把開關寫進 config.toml（toml_edit 原地修改，保留註解與排版；config 格式
  錯誤時拒寫並留 log，不會整檔覆蓋）。
- **「Google 未連線」／「Odoo 未設定」看不出在哪找過**：gateway 與 agent 端 MCP server
  各自解析 `DUDUCLAW_HOME`，一旦不一致，dashboard 連好的帳號在 agent 眼裡就像從沒
  連過，錯誤訊息卻隻字不提路徑。現在兩者都會列出實際查找的檔案路徑
  （`mcp-oauth-tokens.json` / `config.toml`）並提示比對 gateway 的 home 目錄，
  home 不一致從玄學變成一眼可判。

## [1.52.2] - 2026-08-05 — 對話痕跡全面可見+決策句知識圖譜

### Added
- **對話終於在儀表板留下痕跡**：三條先前完全靜默的路徑接上活動流（紀錄／即時動態，
  含即時推播）——每次頻道回覆（`agent_reply`：「回覆 Telegram 對話『…』」）、
  key-fact 萃取入庫（`memory_distilled`：「從對話萃取 N 筆關鍵事實」）、以及
  `wiki_write`／`shared_wiki_write` 知識頁寫入（`wiki_written`：「寫入知識庫『…』」）。
  在此之前，一段建立四人團隊、寫了知識頁、存了三筆記憶的對話，在儀表板上是零動靜。

### Fixed
- **決策句萃不出知識圖譜三元組**：「把 ADLC 當成團隊標準」這類決策發言被分類成
  Local 蒸餾級——而 Local 萃取只對使用者文字跑實體啟發式、完全不看回覆內容，
  結果是零筆入庫、策展台知識圖譜永遠空白。分類器新增決策／標準關鍵字
  （標準、規範、決定、採用、當成、作為、定案、納入、policy、standard、adopt 等），
  這類對話升級 Cloud 萃取產出真正的 SPO 三元組，餵進知識圖譜與事實歷史。
  蒸餾 tier 選擇同步升為 info log，之後「這輪為什麼沒進記憶」直接看 log 可判。
- **CI 三個 Windows-only 測試修正**：capability_grants 清理 tempdir 前先關閉
  SQLite 連線（Windows 鎖檔 error 32）；migrate 絕對路徑測試分平台
  （`/Users/x` 在 Windows 不是絕對路徑）；os_events 監看路徑寫入 TOML 前
  正規化 `/`（反斜線是跳脫序列會 parse-fail）。Windows CI 全綠。

## [1.52.1] - 2026-08-05 — 記憶頁與對話側欄修復+桌面真熱更新

### Added
- **桌面版真正的熱更新**：Tauri v2 的 updater plugin 是被動的——只註冊不會有任何
  動作（v1 時代的 `"dialog": true` 設定在 v2 被直接忽略），所以桌面殼從來不會更新
  自己，「重新啟動 App 就會套用」在此之前是一句空話。現在桌面殼補上主動端：啟動後
  30 秒與每 6 小時檢查 `desktop-updater` feed，發現新版**背景下載暫存**（下載中完全
  不打擾使用者），完成後跳系統通知並把系統匣項目改成「🔄 重啟並更新到 vX.Y.Z」——
  點了就地安裝並重啟；就算忽略通知，正常結束 App 時也會順手安裝，下次啟動就是新版
  （Chrome/VSCode 式體驗）。系統匣新增「檢查更新」手動觸發（已是最新／失敗都有
  通知回饋）。Windows 安裝模式設為 passive。
  ※ 既有安裝（≤1.52.0）沒有這段主動邏輯，需要手動下載安裝這一版一次，之後的版本
  才會自動更新。

### Fixed
- **共享 wiki 在 Windows 上輸出反斜線路徑**：`shared_wiki_ls`／`stats`／`lint` 直接用
  `Path::display()` 印 wiki 相對路徑，Windows 上變成 `departments\art\palette.md`——
  與 `page_path` API 的 `/` 形式不一致（複製貼上會查不到頁），CI 的 Windows 測試也
  因此紅了三個。頁面路徑是平台無關識別子：`collect_visible_shared_pages` 現在統一
  回傳 `/` 正規化字串。
- **記憶頁永遠空白**：儀表板的記憶／關鍵洞察／記憶搜尋等 RPC 讀的是
  `agents/<id>/memory.db`，但所有寫入路徑（對話蒸餾、key-fact 累積器）寫的是共享的
  `~/.duduclaw/memory.db`——每一台安裝的機器上這些頁面都會靜默回空，不管後端存了
  多少。讀取端現在補上共享檔 fallback（既有的 per-agent 檔案仍優先，舊安裝不受影響；
  引擎查詢本來就以 `agent_id` 過濾，讀共享檔仍是逐員工隔離）。技能合成的 episodic
  evidence 取證鏡像同一段路徑解析，一併修正。
- **通道對話不會出現在「對話紀錄」側欄**：側欄只在本機 webchat 發言時重抓列表，
  程式註解宣稱的 `chat.sessions.*` 推播事件實際上不存在——開著儀表板時從 Telegram
  對話，側欄永遠不更新。gateway 現在在每次通道回覆存檔後廣播
  `chat.sessions.updated`，前端訂閱後自動重抓。另外列表載入失敗以前會顯示成
  「還沒有對話紀錄」，現在會顯示「載入失敗，點一下重試」。
- **建立 AI 員工在儀表板零動靜**：MCP `create_agent` 建完員工不留任何可見痕跡
  （不寫活動流、不推事件）——從 Telegram 一口氣建了四人團隊，儀表板的紀錄／
  即時動態完全空白。現在成功建立會寫入一筆 `agent_created` 活動並推事件，
  出現在活動流與員工詳情的紀錄頁。
- **「這輪為什麼沒進記憶」無法從 log 判讀**：對話蒸餾的提前退出（瑣碎對話跳過、
  抽取結果為零）只記在 `debug!`，而生產 gateway 跑 INFO——事後完全無法回答哪個
  分支放棄了這輪。三個退出點升為 `info!`。

## [1.52.0] - 2026-08-05 — 開機自動啟動

### Added
- **開機自動啟動，四個入口一次到位**：gateway 現在可以註冊成登入即啟動的常駐服務，
  而且四個地方都能設——儀表板「設定 → 一般」的開關（即改即生效，附結果提示）、
  新手引導最後一步的勾選（預設開啟，部署助理時一併套用；就算註冊失敗也不會擋部署）、
  CLI `duduclaw service install / uninstall / status`（從「只印指令要你自己貼」升級為
  真正寫入），以及桌面版系統匣選單的「開機自動啟動」勾選項（桌面殼自己登入自啟，
  gateway 以 sidecar 隨行）。三平台都是**使用者層級**註冊，不需要管理員權限：
  macOS 寫 LaunchAgent（`com.duduclaw.gateway.plist`）、Linux 寫 systemd user unit
  並直接建立 enable symlink（沒有可用的 `systemctl` 也能運作）、Windows 寫
  HKCU Run 登錄值。所有入口共用同一個 `duduclaw-core::autostart` 模組
  （新 RPC：`system.autostart.status` / `system.autostart.set`，admin 限定），
  介面之間不會再各寫各的漂移。啟用／停用**只改開機註冊、絕不碰正在執行的服務**——
  否則從儀表板按「停用」會把正在回應這個請求的 gateway 自己殺掉。

### Fixed
- **`setup-macos.sh` 產生的 launchd plist 指向不存在的 `serve` 子命令**：照著一鍵
  腳本裝完，開機後 gateway 根本起不來（launchd 反覆重啟一個立刻退出的進程）。
  改為實際存在的 `run --yes`。同時統一 launchd label：CLI 舊說明用的
  `dev.duduclaw` 與腳本的 `com.duduclaw.gateway` 並存會造成雙重註冊，現在啟用／
  停用都會順手清掉舊 label 的 plist。

## [1.51.2] - 2026-08-04 — 審批推通道按鈕核決+migrate 測試修正

### Fixed
- **核可請求永遠等不到人、TTL 一到自動拒絕**：AI 員工要安裝技能／取得額外權限時會
  開一筆待審核可，逾時未決就 fail-closed 自動拒絕——但這筆待審**從來沒有被送到任何
  通道**。唯一看得到它的地方是儀表板的審批頁；從 Telegram 叫 AI 安裝技能的人，只會
  在五分鐘後收到一句「逾時未核可，已自動拒絕」，而且完全不知道自己剛剛錯過了什麼。
  現在待審一建立就推播到「看得到的人」那裡，並附上同意／拒絕按鈕。
  （既有的「安裝簽核申請」推播走的是另一個資料表，不涵蓋這條路徑；目標任務的
  needs_human 按鈕也只涵蓋目標迴圈。）

### Added
- **核可請求直接推到通道，按按鈕就能決定**：目的地依序解析——發起這次動作的那個
  對話（例如你在 Telegram 叫它安裝，就推回同一個對話）→ 該員工的通知通道 →
  有核准權限者已綁定的通道。Telegram／Slack／Discord／LINE 顯示「✅ 同意 / ❌ 拒絕」
  按鈕，按下後就地改寫原訊息顯示結果；其餘通道退化為文字並指向儀表板。訊息以
  終端使用者看得懂的話描述（「安裝新技能／工具」而不是 `mcp_install`），含期限與
  「逾時自動拒絕」提示。涵蓋所有走同一個審批中樞的類別：技能安裝、高風險工具、
  權限授予、技能啟用、待審知識、自動化規則動作等。
- **快到期會再提醒一次**：待審用掉三分之二的期限仍未決時補推一次提醒（每筆只推
  一次），掛在既有的輪詢與清掃路徑上，不新增背景迴圈；期限短於 2 分鐘者不提醒
  （提醒與自動拒絕會前後腳到達，沒有反應時間）。提醒同時是首推失敗的重試，送達
  後會回寫實際落點，避免出現「按了沒反應」的死按鈕。

### Security
- **未驗證的通道綁定被當成已認證身分**：核准按鈕的身分比對用的是會一併回傳
  *未驗證* 綁定的查詢（該查詢本來就要讓 OTP 流程找到待驗證的那一列）。結果是：
  只要在儀表板對某個主管／管理員帳號填入自己的通道 ID（不需完成驗證），就能從那個
  對話繼承對方的核准權限。安裝簽核與新的核可推播兩處都改為 verified-only 查詢，
  並在 `duduclaw-auth` 新增 `find_verified_user_id_by_channel` 讓兩者共用。
- **核可按鈕不再認「同一個群組」**：未綁定身分時的退路只認**送達對象本人的帳號 ID**，
  不再接受對話 ID。否則核可訊息一落到群組，群組內任何成員（含被拉進來的人）都能
  代為核准。一對一私訊不受影響（Telegram／Discord／LINE 的私訊帳號 ID 與對話 ID
  相同）；送到群組的核可請求則必須由已綁定身分者或儀表板決定。

## [1.51.1] - 2026-08-04 — 技能庫回填+更新自動重試+引導名稱

### Fixed
- **技能庫頁一片空白**：新 AI 員工本來就該內建 docx／xlsx／pptx／pdf 等技能，但這段
  種子程式只掛在 MCP `create_agent` 一條路徑上。從 dashboard 引導、`duduclaw onboard`
  或產業精靈建的員工，`SKILLS/` 是空的，而 `~/.duduclaw/skills/` 也沒有任何東西會去
  寫——技能庫頁「什麼都看不到」是字面上的事實，跟頁面讀取邏輯無關。五條建立路徑
  現在都會種子（既有檔案不覆寫，操作者改過的版本優先）。**既有安裝會在下次啟動時
  一次性回填**：內建技能補進全公司層 `~/.duduclaw/skills/`，所有現有員工立即共用。
  回填以 `~/.duduclaw/migrations/wp19-builtin-skills-seed.done` 標記，只跑一次——
  之後刻意刪掉的技能不會被蓋回來；同名檔案一律不覆寫；失敗只留 warn，不擋啟動。
- **「我的技能」預設選錯員工**：分頁開啟時選的是 `agents[0]`，也就是 gateway 的
  HashMap 剛好先吐出來的那一個——順序每次重啟都不同，也跟使用者實際在 Telegram
  上對話的員工無關。技能放在別的員工身上的人，看到的是永遠空白的清單，且畫面上
  沒有任何線索指向「選擇員工」下拉。預設改為「全部 AI 員工」聚合檢視，並新增「歸屬」
  欄位標示全公司／部門／個人。
- **`skills.list` 聚合檢視回傳掃描當下的快取**：不帶 `agent_id` 的分支讀的是
  `agent.skills` 快照，出現「單一員工檢視看得到、全部檢視看不到」的矛盾；現在兩個
  分支都直接讀磁碟，並排除重複列出的全公司層。
- **MCP `skill_extract` 找不到任何技能**：目錄名誤植為小寫 `skills`（其餘所有路徑都是
  `SKILLS`）。macOS 大小寫不敏感所以看不出來，Linux 上每次都回報「技能不存在」。

### Added
- **技能庫空畫面會說明原因**：`skills.list` 回傳實際掃描過的資料夾路徑、是否存在與各層
  技能數，空狀態直接把它們列在畫面上，並說明技能與內建工具的差別、去哪裡取得技能。
  讀取失敗改以錯誤區塊呈現（附重試），不再退化成看起來像「你沒有技能」的空清單。
- **技能市場索引載入失敗會講明白**：market 索引向 GitHub 更新失敗時是靜默的，任何
  搜尋都回 0 筆、看起來像「市場是空的」。現在依 `total_indexed` 區分「索引沒載入」
  與「查無符合」。

## [1.51.0] - 2026-08-04 — 現場回饋整批:通道穩定+憑證顯示+記憶完整內容+主題關聯圖

### Changed
- **記憶頁不再被學習訊號淹沒**：預測引擎每次回覆落差都會寫一筆遙測（「學習訊號
  70% → 52%」），這些條目和真正的記憶混在同一份清單、共用同一個筆數上限，聊天多
  一點的 AI 員工整頁只剩遙測，使用者看到的結論是「記憶還沒更新」。現在 `memory.browse`
  與 `memory.search` 由後端切成兩份清單回傳：主清單只放「你告訴它的事」，遙測改走
  獨立的「系統學習紀錄」區塊，預設收合、只顯示筆數與一句白話說明。切分在 SQL 做，
  兩份清單各有各的筆數預算——一千筆遙測再也擠不掉一則真的記憶。判定依既有的
  `source_event` 欄位（`duduclaw_memory::is_system_signal` 為唯一判準；舊版沒標記
  來源的資料才退回內容前綴比對）。列表摘要同時放寬為兩行、且不再只取第一行，
  「客戶資料更新／公司／電話」這種分行寫的記憶不會只剩標題。
- **側邊欄依實際使用順序重排**：主導覽改為 新對話 → 例行工作 → 技能庫 → 記憶 →
  AI 員工 → 世界，任務看板收進「進階」群組（個人版）／移到「工作」群組末尾（企業版），
  兩版順序一致。路由、⌘K、書籤全部不變。
- **側邊欄底部改顯示版本號**：原本常駐的當日花費金額移除（帳務仍在 管理 → 進階設定
  → 帳務），改顯示目前執行中的 gateway 版本，升級後想確認自己在哪個版本不用再翻設定頁。
- **管理 → 進階設定整理**：帳務、授權、經銷排在最前，「設定」固定殿後；安全、可靠性、
  日誌三項對個人版隱藏（企業版保留，路由仍可直接開啟）。
- **一鍵登入移出預算數字堆**：原本是夾在「輪換」「新增」之間的小按鈕、擠在花費報表
  上方；現在是「帳戶與登入」頁最上方的整列主要卡片，附一句說明。
- **知識庫關聯圖改為主題導向**（客戶回饋「跟 Obsidian 不太一樣、不是主題的關聯」）：
  節點分頁面與主題兩種——主題來自標籤、分類目錄與 AI 學到的實體，三種來源同名
  自動合併；邊有頁面-主題、明確引用、共享冷門主題（虛線）、主題-主題四種；熱門
  主題只留樞紐不做兩兩連線，避免毛球。單一頁面也至少連著自己的主題，不再出現
  「1 個節點、0 條連線」的孤點畫面；還沒有關聯時用一句白話說明取代空圖。

### Fixed
- **側邊欄最上方一列被切掉**：導覽區的上下淡出遮罩無條件套用，讓第一列（新對話）
  頂端 12px 永遠是半透明的，看起來像被什麼蓋住。改成只有該側真的還有內容被捲走時
  才淡出，沒得捲就完全不淡。
- **更新後 Google 綁定資訊「看起來消失」**（實際憑證都在、測試連線成功）：兩個根因
  ——後端在總開關未開時把 Google 整個濾出 provider 清單，前端因此判定「未設定」而
  渲染空白表單；以及狀態端點把「access token 過期但有 refresh token」當成未連線，
  而工具路徑會自動續期，兩邊對同一份憑證給出相反答案。現在已存的 client ID 直接
  顯示、密鑰顯示「已儲存 ••••尾碼，留空表示不變更」、卡片有「已綁定／已連線」徽章；
  同類掃描把 MCP OAuth 卡（前後端欄位不同調導致已連線一律顯示未授權）、Odoo 與
  身份解析的「存了跟沒存長一樣」一併修成同一種呈現；所有回應中密鑰本體零外流
  （整包序列化掃描測試釘住）。
- **Telegram 通道「設定更新後會掉」**：只要 gateway 啟動時那一次 `getMe` 探測遇到
  網路還沒起來（剛更新完、機器剛喚醒、代理暫斷），Telegram bot 就直接不啟動、
  也不再重試，通道永久顯示離線到有人重存 token 或重開 gateway 為止。現在區分
  「Telegram 明確回絕」（token 真的錯，停）與「根本沒收到回應」（先照常啟動輪詢，
  由既有的 3 秒重試自行恢復）；輪詢端對 401/404 連續三次才收手，並顯示可行動的
  中文訊息。斷線重試改為 3 秒指數退避、60 秒封頂（原本固定 3 秒，一夜斷網等於
  每小時敲 API 1,200 次）；通道狀態只在「連線與否／錯誤內容」真的變動時才推播與
  落盤，重試迴圈不再每 3 秒重寫一次 `channel_status.json`。
- **Bot Token 格式在存檔前檢查**：`channels.add` 與 AI 員工設定的通道 token 過去
  只檢查非空，格式壞掉的 token 會被靜靜加密存起來，第一個症狀是通道無聲離線。
  現在存檔當下就驗證形狀，缺冒號（`7000000001-AAExample…`）這種明確可判定的壞值自動
  修回冒號，其餘無法解讀的值直接退回並附上正確格式說明。讀取端另有同樣窄條件的
  相容修補，已經存進設定檔的壞值不必手動重存也能恢復。

### Security
- **通道錯誤訊息不再外洩憑證**：Telegram 的憑證在 URL 路徑裡（`/bot<token>/getMe`），
  WeCom／DingTalk 在查詢字串裡，而 reqwest 的錯誤訊息會把整個 URL 印出來——這些
  文字會進到儀表板通道狀態、`channels.status_changed` 事件、`channel_status.json`
  以及 gateway 日誌。新增形狀導向的憑證遮罩（Telegram／Slack／Discord／JWT／
  GitHub／Anthropic／Google／Meta 前綴、以及敏感查詢參數），套在通道狀態的單一
  匯聚點與日誌串流上；遮罩後仍保留主機、方法與分隔符號，診斷資訊不減。

### Removed
- **Homebrew 安裝通路停止維護**：tap 凍結於 1.50.0，之後不再推送新版本。既有以
  `brew install` 安裝的使用者請改用 `npm install -g duduclaw` 或下載桌面版重新安裝
  以取得後續更新；儀表板「更新」頁對偵測到的 Homebrew 安裝會顯示遷移提示。

## [1.50.0] - 2026-08-04 — 現場 P0 修復+記憶知識自動分流+介面重整

### Added
- **重要長效資料自動進知識庫**：貼進對話的公司章程／SOP／規格這類文件，零成本
  判別器（灰帶才用小模型仲裁）自動建檔到知識庫的 `auto/` 命名空間，不用再唸
  「寫進維基」咒語；閒聊與情境資訊照舊進記憶。全文只落知識庫，記憶側只留一條
  可檢索的指標（享時態取代）；同主題再貼會更新同一頁並在頁尾留版本紀錄；自動頁
  **不進 system prompt 注入**（誤判最壞後果是知識庫多一頁髒資料，不會污染任何
  一次回覆）；寫入前整頁過注入掃描 fail-closed，另有每日 20 頁與同主題爆量兩道
  斷路器，`.scope.toml` 政策照常尊重。策展台隨之改為事後審計面：「自動建檔」分頁
  可檢視、封存（可還原）、升格、一鍵分享到共享知識庫（僅自動頁）；待審佇列分頁
  退場（後端佇列保留）。
- **使用者基礎資料自動記住**：對話中自述的稱呼、偏好、回覆方式自動寫入個人檔案，
  之後每一輪都生效。「叫我老李就好」「稱呼我…」「call me …」這類明確講法最可靠；
  「記得叫我開會」這種請託不會被誤記成稱呼。寫入端過注入掃描與名字合理性檢查；
  WebChat 的個人檔案改綁穩定身分，重新整理頁面不再失憶。
- **新手引導可以代安裝與協助登入**：選到未安裝的 CLI，一鍵讓 DuDuClaw 代裝
  （claude／codex／gemini 走 npm，antigravity 走官方腳本；grok 因探測路徑限制
  改給可複製指令），進度即時顯示、裝完自動重新偵測；已安裝未登入則就地開啟
  登入流程，全程不碰終端機。安裝命令硬編碼白名單、僅管理員可觸發、全程審計、
  300 秒逾時整組收殺；「重新偵測」按鈕新增，登入／安裝完成也會自動重偵測，
  不必再關掉重開。
- **完整對話列表頁**（`/conversations`）：搜尋、依 AI 員工篩選、分頁；側邊欄
  「對話紀錄」只顯示最新 5 筆＋「查看全部」。
- **通道動作即時回饋 dashboard**：在 Telegram 等通道建立例行工作、寫入記憶、
  產出技能，例行工作頁／記憶頁／技能庫即時出現（事件推播＋去抖動），不必重新
  整理；通道端回一句人話回執，排程描述看不懂的形狀誠實顯示原始 cron 表達式。
  技能清單改為現讀磁碟並保留全域／部門分層——修掉「合成的技能要重啟 gateway
  才看得到」。
- **思考動畫**：聊天等待與語音聆聽改用 thinking-orbs Canvas 動畫（MIT、零執行期
  依賴、供應鏈審查通過），尊重 `prefers-reduced-motion`（JS gate＋靜態替代）、
  分頁不可見自動暫停。
- 文件：Google Workspace 整合 how-to（三條接法＋11 個 scope 逐一用途；勘誤：
  會議中的「19」是工具數）；終端使用者版「記憶與知識庫」說明
  （docs/guides/memory-vs-knowledge.html）；品牌爪印 logo 透明背景版
  （marketing/press-kit）。

### Changed
- **側邊欄與管理選單重整**：新對話置頂並強調；收件夾退出常駐導覽（待辦改由
  鈴鐺 badge 承接，有事才亮，路由保留）；AI 員工頁放回（含個人版）；管理選單
  收斂為五項——通道管理 → 整合 → 帳戶與登入（自帳務抽出）→ 系統更新（自設定
  抽出）→ 進階設定（其餘 11 項收納，所有路由與書籤照常可用）。
- **整合頁改為四張卡**：MCP 工具／Google／Odoo／身份解析；獨立 OAuth 頁移除，
  需授權的 MCP 直接在卡片上顯示連線按鈕（含伺服器推導的 redirect URI 唯讀顯示，
  非預設 port 也不會走進死路）。
- **記憶頁改為一條條記憶列表**：一句話摘要＋展開詳細（層級、來源、取代鏈），
  可刪除、不可編輯；預設最近優先；空狀態用人話說明記憶怎麼產生；截斷改
  grapheme 安全（國旗、家庭這類組合 emoji 不再被腰斬）。
- **一鍵登錄授權網址改用 OAuth 參數證據排序**：antigravity 等 CLI 的授權網址
  不再抓錯——文件連結、服務條款、已用掉的回呼網址都會讓路給真正的同意頁。
- README 三語安裝章節重整：桌面程式優先推薦（引導在應用程式內完成）、移除
  一行安裝方式、快速開始不再要求先跑 `duduclaw onboard`（瀏覽器引導已涵蓋，
  無頭場景仍可用 `--yes`）。

### Deprecated
- **`agent.toml` 的 `cognitive_memory` 欄位**：認知記憶改為常駐開啟，不開放
  設定。舊欄位仍可解析但一律視為開啟（首次讀到 `false` 記一次提醒），agent
  編輯表單與演化頁的開關移除。順帶修正一個舊缺陷：無頭安裝（`onboard --yes`）
  與子 agent 建立過去會寫入 `false`，這些 agent 從未擁有認知記憶——現已一律開啟。

### Fixed
- **Telegram 回覆夾帶 AI 執行層的內部訊息**（2026-08-04 客戶現場截圖）：回覆裡出現
  `⚠ Transcript saving is off — inherited CLAUDE_CODE_CHILD_SESSION marker`、
  `⏵⏵ manual mode on`、`paste again to expand` 等字樣。這些是互動式 CLI 終端介面
  自己畫的狀態列（已用本機 `claude` 2.1.220 的 PTY 實錄比對確認），原本只由
  `duduclaw-cli-runtime` 的 chrome 過濾器攔截，但它的字串清單得跟著每次 CLI 改版
  補，一漏就直接送到使用者眼前。新增 `cli_noise` 模組，改以**模式**（transcripts／
  session 持久化警告、`CLAUDE_CODE_*` 等環境變數提示、貼上與模式狀態列、MCP 認證
  提示、更新／額度／壓縮／費用摘要、轉圈詞、codex 與 gemini 的啟動訊息、信任對話框…）
  比對，並掛在**所有通道共用**的兩個出口——回覆組裝點與委派回送／主動推播的
  `forward_to_channel`——因此 Telegram／Discord／Slack／LINE／WhatsApp／Feishu／
  Google Chat／Teams／WebChat 一併涵蓋，沒有旁路。
  過濾刻意保守，**比對到還不夠，必須看起來像終端畫面**才會刪：該行帶終端狀態符號
  （⚠ ⏵ ⎿ ·）、或多字詞以「無空白黏連」形式出現（`pasteagaintoexpand`，正常文章
  寫不出來）、或位於訊息最後三行且短且不像句子。另有四道硬否決：程式碼區塊內不動、
  含中日韓／假名／諺文的行不動、超過 200 字不動、過濾後會變空訊息則整段還原。
  絕大多數模式（含 transcripts／環境變數／`/compact`／`Ctrl+C` 等客服回答真的會寫的
  字眼）只認前兩種證據，位置不算數。被擋下但未刪的都記 `warn` 留痕。
- **同一則訊息裡英文空格全部消失**（同一張截圖，中文段正常）：根因不在 Telegram
  渲染，而在來源。終端介面重繪時用游標位移碼（`ESC[<n>C`／`ESC[<n>G`）表示水平
  間距而非空白字元，`strip_ansi` 把它們一併丟棄，於是英文單字黏在一起（中文本來
  就沒有詞間空格，所以看起來正常）。此行為刻意維持不動——模組內所有 chrome 判斷
  與哨兵掃描都是照「無空格」形式調校的，改動風險大於效益；改為在上面那道過濾器
  以「忽略空白」的方式比對，兩種呈現形式都攔得到。同時補上回歸測試：混合中英文
  經六種通道渲染後空格必須完好，證明渲染鏈無罪。
- **對話標題自動生成變簡體字**（zh-TW 客戶看到「了解用户个人档案信息」）：標題
  提示詞原本只說「使用對話本身的主要語言」，多數工具模型把「中文」預設成簡體。
  改為偵測對話語系後明講——繁中對話要求繁體中文（臺灣用語）、簡中對話維持簡體，
  仍然跟隨對話語言而非寫死。另加一道確定性的安全網 `duduclaw_core::zh_variant`：
  296 個對應唯一的簡體字改寫回正體（帳號用臺灣的「帳」而非「賬」），對應不唯一者
  （冲、划、别、历、发、复、尽、汇、签、钟）只偵測不改寫並記錄，兩套皆同時存在的字
  （游、丰、术、据、种、後、里、只、面、干、雲、台…）**兩張表都不收**——否則
  「上游 API」這種正體文字會被誤判成簡體。
- **單一 OAuth 帳號被 REPL 停滯拖垮，之後每則訊息都失敗**（2026-08-04 客戶現場，
  90 分鐘內停滯 4 次、帳號耗盡 2 次）。三層根因分開修：
  1. **不該走互動 REPL**：agent 編輯頁有一段「偵測到 OAuth 就自動開啟 PTY pool」的
     副作用（2026-07-19 起），配合自動存檔，使用者**只要打開該頁**就會把
     `pty_pool_enabled = true` + `worker_managed = true` 寫進 `agent.toml`——與文件
     宣稱的「預設關閉」相反。當初的前提（Anthropic 封鎖 OAuth 的 `claude -p`）已於
     2026-06-15 暫停且從未生效。移除自動開啟；開關保留給使用者自行決定。
  2. **停滯被算到帳號頭上**：REPL 卡死屬於**傳輸層**故障，同一個帳號改走
     fresh spawn `claude -p` 完全正常，但舊碼把它記成帳號錯誤，三次就把唯一帳號
     踢出輪替，於是「All accounts exhausted」後全數訊息陣亡。改為分類後不計入帳號
     健康度；另補兩道防線：`on_error` 標記不健康時一併給冷卻時間（原本
     `cooldown_until = None` 會讓帳號**永久**不可用，只能等 5 分鐘快取重建），
     以及新增降級斷路器——同一 agent 連續 2 次傳輸層卡死就改走 `claude -p`
     30 分鐘，成功一次即復原。使用者訊息也從「請先到儀表板設定帳號」改成說明
     冷卻中會自動恢復。
  3. **行程活著但 dashboard 逾時（HTTP 000）**：主因是關機 drain 無上限（見下一條，
     同版修復）。額外補上此版發現的缺口：in-process PTY pool **從未**接上關機流程
     （`PtyPool::shutdown` 寫好了卻沒人呼叫），未託管 worker 時每個池內互動
     `claude` 子行程會在 gateway 結束時變成孤兒並**活過重啟**——重啟不治的一部分。
     現已加入關機鏈（10 秒上限）。
- **一次性 migration：清掉儀表板自動寫入的 PTY 殘值**。移除上述自動開啟只擋新案例，
  已經被寫進 `agent.toml` 的既有 agent 不會自己好。gateway 啟動時（早於 PTY runtime
  初始化）掃一次 `<home>/agents/*/agent.toml`，**只有完整命中當初那段 effect 的簽名**
  才重置：`pty_pool_enabled = true` 且 `worker_managed = true` 且 runtime provider 為
  claude（或未設）。只單獨開了 `pty_pool_enabled` 的視為刻意設定，一律不動；非 claude
  provider 也不動。寫檔走原子路徑（temp + rename），其餘 `[runtime]` 鍵原封不動，
  失敗只警告不擋啟動。以 `<home>/migrations/wp10-pty-default-reset.done` 標記保證只跑
  一次，標記檔內容即稽核紀錄（時間、被重置的 agent 清單、原因說明）。重置後仍想用
  互動 REPL 的使用者，可在 agent 的執行環境設定重新開啟，且不會再被 migration 蓋掉。
- **Dashboard 更新後 gateway 卡成殭屍行程**（2026-08-03 客戶實測）：套用更新後的
  優雅關機會等**所有連線**結束才重啟，但 dashboard WebSocket／SSE 是永不主動關閉
  的長連線，於是行程停在「port 已關、PID 還活著、永遠等不到 re-exec」。修法：
  flush 完成後啟動 10 秒 drain watchdog，逾時強制收尾讓重啟繼續；prediction flush
  （20s）／metacognition 持久化（10s）／worker supervisor 關閉（15s）各自加上時限，
  任何一步卡住都不再擋整條關機鏈。活體驗證：掛一條永不完成的請求送 SIGINT，修前
  永久卡死、修後 10 秒退出；無卡死連線時 1 秒正常退出、watchdog 不觸發。同修
  一般 Ctrl+C 與 auto-update 重啟路徑（同一條關機鏈）。**已知成本**：只要還有
  dashboard 分頁／WebSocket／SSE 連線開著，關機與自動更新重啟就會固定等滿 10 秒
  drain 上限才收尾——這是刻意的取捨，換掉原本「永遠等不到」的卡死。後續優化：
  關機時先廣播主動 close 給長連線，讓它們自己收線、不必等滿逾時。
- **桌面版重開後「偵測到 gateway」但實際連不上**：sidecar spawn 後立即自稱
  Running（不等 port 可連）、重開時會 attach 到前一個實例垂死中的 gateway、狀態
  卡死後「連線」按鈕永遠無法自救。修法：新增 Starting 狀態（port 實測可連才轉
  Running）、`start()` 對 Running 做活性複驗並重新規劃、殺孤兒行程後等它真的退出、
  世代計數防止被淘汰子行程的事件亂入；另修 `stop()` 後自動重啟被永久停用的潛伏 bug。

## [1.49.0] - 2026-08-03 — 對話截斷與續聊修復＋chat 內切換模型

### Fixed
- **長回覆只剩最後幾個字**（客戶回饋第五輪）：stream-json 解析把每個 text block
  `text = ...` **覆蓋**而非累加，所以一則跨多個 text block／多個 `assistant` 事件
  的回覆只留下最後一個片段——使用者看到的是「幾個字」，token 數只有兩位數，而
  模型其實完整輸出了。先寫三個重現測試證明（3 個 block 只剩第三段、3 個串流事件
  只剩結尾、tool-use 回合只剩答案沒有敘述），再依邊界修：同一則訊息內與跨事件的
  text **串接**，終端 `result` 事件的非空文字**取代**累積值（附加會整段重複，另有
  測試守住）。**同類掃描找到四個複製品**：PTY one-shot 解析器、串流主路徑（網頁版
  實際走的那條）、子 agent 派工路徑都有同樣的覆蓋，一併修；`computer_use` 本來就
  正確使用 `push`。
- **網頁版對話無法續聊，一直跳「已為你開啟新對話」**：`/ws/chat` 通過 JWT 認證後
  **把回傳的使用者身分丟掉**，resume 所有權改綁每次連線隨機產生的 UUID。結果是
  重新整理頁面等同換一個身分，所有既有對話都被判定為「不是你的」——歷史讀得到
  （走已認證的 dashboard RPC）卻永遠續不了。改綁已認證使用者（SHA-256 前 12 碼，
  不洩帳號識別）。跨使用者、跨通道、前綴仿冒（`{tag}evil:`）、空 tag 皆 fail-closed，
  七個測試守住。**既有 webchat session 會因 id 格式改變而無法續聊一次**（它們本來
  就續不了）。
- **`[sender_id: …]` 出現在使用者自己的訊息與對話紀錄標題**：那是給模型辨識發話者
  的內部標記，卻被存進逐字稿，於是未命名的對話在側邊欄顯示成
  `[sender_id: webchat:127.0.0.1:…]`。新增 `strip_sender_prefix`，套用於標題 fallback
  與逐字稿回放；標記格式不完整時寧可原樣顯示也不吞掉使用者的字。

### Added
- **`/model <名稱>` 真的可以切換模型**（原本只回「read-only，請改 agent.toml」）：
  寫入 `agent.toml [model] preferred` 並熱重載，下一則訊息生效；熱重載失敗會明講
  需要重啟而不是謊報成功。**設定形式限管理員**（改模型會改變每一輪的成本，不能讓
  群組任何成員改），純查詢的 `/model` 維持開放。模型名稱做嚴格識別字驗證——這個值
  會成為 CLI 參數並寫入設定檔。編輯 `agent.toml` ＋ 熱重載的實作抽成
  `update_agent_toml_with`，與 dashboard 的 `agents.update` 共用同一份。

### Changed
- **側邊欄「新對話」改為外框按鈕樣式**（客戶回饋：「改得醒目一點」）：它是唯一
  「執行動作」而非「前往頁面」的列，原本混在導覽項目中難以辨識。
- **管理選單兩個同名「系統」**：營運群組的入口改稱「設定」（進階設定群組內另有
  一個「系統」分頁），描述同步說明涵蓋範圍。
- **Google Workspace 分頁開放**：原本等的是原廠 OAuth App 驗證，但驗證只擋
  「使用者自建 OAuth client」那一條路徑，服務帳號網域委派與 Apps Script 橋接都
  不需要它，分頁沒有理由繼續隱藏。後端 `[integrations] google_workspace` 總開關
  維持不變，設定頁在它沒開時會明講工具不會出現在 AI 員工面前。
- **設定側欄的「系統更新」在有新版時顯示提示點**：常駐強調一天後就沒人看得見，
  只有真的有更新才亮才保有意義；檢查失敗不亮，不虛報。

## [1.48.0] - 2026-08-03 — 免自建 OAuth client 的 Google 憑證路徑＋側邊欄對話紀錄

### Added
- **Google Workspace 兩條免自建 OAuth client 的憑證路徑**：原本每個客戶都要
  自己到 Google Cloud 開 OAuth client（且我方 app 未通過驗證前不能對外服務），
  現在同一組 19 個工具有三種授權方式，`google_status` 會明講目前生效的是哪一種。
  - **服務帳號＋網域委派**（`google_service_account.rs`）：我方出一個服務帳號，
    客戶的 Workspace 超管在 Admin console 授權該 client id 與 scope 清單即可，
    **不需要 Google 應用程式驗證或 CASA**。設定走 `config.toml`
    `[integrations.google_service_account] key_file / subject`；RS256 assertion
    以 `jsonwebtoken` 簽發，token 依 `(client_email, subject)` 快取並提前 120 秒
    續期。限制照實寫進錯誤訊息與文件：**只支援 Workspace 網域，個人 @gmail.com
    不可能委派**。設定了但壞掉一律回報錯誤，不靜默退回 OAuth。
  - **Apps Script 橋接**（`google_apps_script.rs` ＋
    `templates/apps-script/duduclaw-bridge.gs`）：使用者在自己帳號部署一份 web
    app，DuDuClaw 用共用密鑰呼叫。**個人 Gmail 也能用**，因為 Google 只看到
    使用者執行自己的腳本。覆蓋 Gmail（搜尋／讀取／草稿）、行事曆（列出／建立）、
    Sheets（讀取／附加）八個動作；Drive／Docs／Slides／Forms／Tasks 明確回
    「橋接不支援」而非空結果。安全面：網址白名單為 `script.google.com`（精確
    比對，擋 `script.google.com.evil.test`）、強制 https 與 `/exec` 路徑、
    重導每一跳重新驗證、密鑰比照通道 token 加密存放且不進日誌。
  - `GoogleBackend` 後端選擇器把三條路收斂成一個工具面：七個 Gmail／行事曆／
    Sheets handler 改走 `*_via` 包裝，橋接回應映射成與原生工具相同的結構，
    agent 分不出是哪條路答的。
  - **Dashboard 設定介面**：管理 → 整合／工具連線 → Google 新增「憑證方式」區塊
    （`GoogleCredentialPaths.tsx`），三選一切換 ＋ 儲存 ＋ **測試連線**（服務帳號
    真的去換 token、橋接真的打 `status` 動作並回報腳本以哪個 Google 帳號執行）。
    後端 `google.credentials.get/set/test` 三個 admin-gated RPC：`get` 只回報
    密鑰「有沒有設」而不回傳內容、存檔前先驗證（金鑰檔讀得到、subject 是信箱、
    網址過白名單）、切換模式會清掉另一種的設定避免實際生效與畫面不一致、
    留白密鑰時沿用既有值。scope 清單附複製按鈕給客戶管理員。
    總開關 `google_workspace` 沒開時顯示警告——否則憑證測試會過但工具不會出現。
  - 新文件 `docs/guides/google-no-oauth-client.md`。

### Changed
- **側邊欄用語與結構調整**（2026-07-30 客戶回饋第四輪）：「首頁」改稱
  「儀表板」、「Skill」改稱「技能庫」、「記憶與知識」改稱「記憶」；原本的
  「對話」列改成「新對話」（點下去直接開一段新的，舊對話保留可續聊），
  其下新增可收合的「對話紀錄」群組，列出最近 15 則對話（新的在上，跨通道
  的對話帶 LINE／Telegram 等來源標籤）。個人版與企業版同步套用；⌘K 的
  「新對話」與側邊欄同一行為。折疊成 icon 模式時對話紀錄不顯示。
- **對話清單與續聊收斂到 `useConversationsStore`**：側邊欄「對話紀錄」與
  `/chat` 左欄共用同一份列表與 resume 實作，不再各寫一份而漂移。`/chat`
  左欄維持原樣（可跨 AI 員工、跨通道看到全部對話），側邊欄只是捷徑。

### Fixed
- **OAuth 重新導向網址寫死 3000 埠，導致整個授權流程走不完**（Google／Notion／
  GitHub 三個整合共用同一段程式，全部受影響）：`handle_mcp_oauth_providers` 與
  `handle_mcp_oauth_start` 都硬編 `http://localhost:3000/api/mcp/oauth/callback`，
  但 callback 路由掛在 gateway 自己的埠上（預設 18789）。實測：3000 連線被拒、
  18789 回 200。使用者照畫面指示在 Google Console 註冊 3000，授權完瀏覽器被導到
  沒有東西在聽的埠，token 永遠拿不到，dashboard 一直停在「未連線」。改由
  `mcp_oauth::redirect_uri()` 依實際監聽埠推導（沿用 `DUDUCLAW_PORT` 的解析順序），
  並隨 `mcp.oauth.providers` 回傳，前端改顯示後端給的真實值而非常數。
- **OAuth 設定教學漏掉兩個必卡步驟**：畫面上的四步沒有「啟用八個 Google API」與
  「設定 OAuth 同意畫面（測試中狀態要加自己為測試使用者）」——前者讓每個工具呼叫
  回 403，後者讓授權當場被 Google 擋掉。兩步已補進面板（`extraSetupSteps` 插槽，
  其他 provider 不受影響），並附上測試中狀態 refresh token 七天過期的提醒。
- **`google_status` 對非 OAuth 憑證回報假的「NOT connected」**：它只檢查 OAuth
  vault，所以用服務帳號或 Apps Script 橋接的客戶會看到「未連線」，但工具其實
  是通的。改為先回報實際生效的憑證來源，再列 OAuth 細節。
- **從側邊欄續聊會被連線握手蓋掉**：`/chat` 尚未連線時從側邊欄點開歷史
  對話，聊天 socket 隨後送來的 `session_info` 會把 session id 覆寫成這條
  連線自己的 id——畫面留著恢復的逐字稿，下一句卻開了新對話。改由
  `resumedExplicitly` 旗標保護明確恢復的 session，判定邏輯抽成純函式
  `sessionIdAfterHello` 並加上回歸測試。

## [1.47.0] - 2026-07-30 — Google Workspace 全覆蓋＋遠端 MCP＋記憶知識整併

### Added
- **記憶與知識庫合併成單一「記憶與知識」頁**（2026-07-30 客戶回饋）：
  原本分開的「記憶」與「知識庫」兩個側邊欄入口整併成一頁，分頁列為
  記憶／個人知識庫／共享知識庫／關鍵洞察／自主進化。個人版只有一個
  知識庫，共享分頁整個隱藏、標籤簡化為「知識庫」。舊 `/knowledge`
  路由 302 到 `/memory?tab=wiki`，書籤與導覽導引不會斷。
- **記憶改為主題分頁瀏覽（Perplexity 式）**：左側分類導航（工作與專案／
  人物與聯絡／客戶與業務／偏好與習慣／規則與決策／工具與系統／時間與
  地點／費用與帳務／使用足跡／學習訊號／其他）＋右側分類卡片，點分類
  展開完整清單。分類器 `lib/memory-category.ts` 是零 LLM 成本的確定性
  兩段式判定（來源 `source_event`/`tags` 優先，其次雙語關鍵字計分），
  ASCII 詞需詞界、CJK 直接子字串比對，不做位元組切片。
- **單筆記憶刪除**：記憶列滑鼠移過去出現垃圾桶（鍵盤 focus 同樣顯示），
  兩段式確認後刪除。新增 `memory.forget` RPC 與
  `SqliteMemoryEngine::forget()`——**軟刪除**：先寫入 `memories_archive`
  再清 FTS 索引與主表，單一交易，管理者仍可回復；跨 agent id 一律 no-op，
  重複刪除不算錯誤。刪除會 bump graph generation，避免 SPO 圖快取殘留。
- **`docs/guides/memory-and-knowledge.md`**：記憶與知識庫的完整使用說明
  （兩者差異對照、知識庫寫入方式與四個預設目錄、L0–L3 層級如何決定
  「多常被想起」、自動帶入 vs 主動搜尋的分界、記憶自動記什麼／不記什麼、
  刪除方式、FAQ）。

- **MCP Bridge 遠端掛載（Streamable HTTP）**：`[[mcp.external]]` 新增 `url`
  模式——`McpClient` 多了 HTTP transport（單端點 POST、JSON/SSE 回應、
  `Mcp-Session-Id` 回聲、僅 https），可直接掛遠端 MCP server；`bearer_token`
  支援 literal / `env://` / `secret://` / **`oauth://google`**（重用 dashboard
  已連接的 Google 帳號 token，自動 refresh），`headers` 自訂請求標頭。
  憑證解析不到即整台跳過（fail-safe）。
- **Google Workspace 八服務全原生覆蓋（11 個新工具）**：Gmail／Calendar／
  Sheets 之外補齊 **Drive／Docs／Slides／Forms／Tasks**，全部走 GA REST API
  ——`drive_search`／`drive_read`（Docs/Slides 匯出純文字、Sheets 匯出 CSV
  第一張表、二進位檔回中繼資料不回位元組）、`docs_read`／`docs_append`
  （**僅追加**，無任何工具能改寫或刪除既有內容）、`slides_read`（唯讀）、
  `forms_get`／`forms_list_responses`（唯讀，答案按 question_id 對應）、
  `gtasks_lists`／`gtasks_list`／`gtasks_create`／`gtasks_complete`。
  合計 19 個原生工具，兩個 scope（`google:read`／`google:write`）。
  **不需要 Google Developer Preview 資格**，任何客戶都能用。
  指南 `docs/guides/google-workspace.md`。
  - Google Tasks 工具刻意用 `gtasks_` 前綴，與 DuDuClaw 自家任務看板的
    `tasks_*` 工具區分（agent 不會混淆兩套任務系統）。
  - Slides 不提供寫入工具：既有 office 文件套件已能產出真正的 `.pptx`，
    比驅動 Slides `batchUpdate` element API 更安全也更好。
- **MCP Bridge 遠端掛載新增 `preset`（進階選項）**：`preset = "google:<svc>"`
  一行掛 Google 官方 remote MCP（Gmail 13／Calendar 9／Drive 8／Docs 2／
  Sheets 7／Slides 2／Chat；端點與工具清單皆對正式端點實測），bearer 自動用
  `oauth://google`。**非出貨路徑**——官方 server 仍是 Developer Preview，
  且 Program Terms 禁止讓自家網域以外的終端使用者使用 Pre-GA API；產品面
  一律走上述原生工具。詳見 `docs/guides/google-mcp.md`。
- **DocuSeal 簽署工作流**：新開源 crate `duduclaw-docuseal-mcp`——MCP stdio
  wrapper 包 DocuSeal REST API（cloud + self-hosted，`X-Auth-Token`），10 個
  工具涵蓋「建板模 → 寄送簽署 → 查狀態 → 取簽署檔」＋官方內建 MCP（僅
  self-hosted）沒有的歸檔/重寄/prefill 更新。指南 `docs/guides/docuseal.md`
  （含 webhook 驗簽格式）。

### Changed
- **`memory.browse` / `memory.search` 回傳擴充**：多帶 `layer`、`source_event`、
  `importance`、`access_count` 四個欄位，供儀表板做來源優先的主題分類。
  既有欄位不變，舊消費端不受影響。
- **側邊欄調整**：企業版「公司」群組移除獨立的「知識庫」項目；個人版主區
  的知識庫入口換成「記憶與知識」（`/memory` 從「進階」升上主區）。
  導覽導引的知識庫步驟移除（該頁已併入記憶頁）。
- **Google 整合 scopes 擴充（最小權限）**：新增 `drive.readonly`、
  `documents`、`presentations.readonly`、`forms.body.readonly`、
  `forms.responses.readonly`、`tasks`。Drive 只要唯讀（沒有任何工具建立
  Drive 檔案，故不要 `drive`／`drive.file`）、Slides 只要唯讀；Docs 需要完整
  `documents` 是因為 `docs_append` 會寫入。**v1.47 之前連好的 Google 帳號需
  重新連接一次**——舊 token 缺 scope 會回 403 並附重授權引導（不會靜默失敗）。
- **記憶頁空狀態文案**：不再誤導「啟用認知記憶後才會儲存」（認知記憶預設
  即開啟）——改為說明「實質對話會自動萃取重點儲存於此；閒聊不記錄」，僅在
  agent 明確停用認知記憶時才顯示停用變體＋前往設定連結（三語系）。

### Removed
- `KnowledgeShell` 頁面元件（`/knowledge` 的個人／共享分頁殼）——功能由
  `MemoryPage` 的知識庫分頁取代，路由改為重導，元件與其測試一併刪除。

### Fixed
- **認知記憶開關對 conversation distill 失效**：對話自動萃取的呼叫點繞過了
  `[evolution] cognitive_memory` 閘（直接用 `ctx.memory_db_path` 還帶
  home_dir 兜底）——關掉認知記憶的 agent 對話仍會被萃取寫入 memory.db。
  現與其他記憶路徑共用同一 gate，關閉即全停。

## [1.46.3] - 2026-07-29 — 更新誤判修復＋個人版再精簡

### Added
- **系統更新頁「已安裝待重啟」提示**：檢查更新時同步探測磁碟上 binary 的實際
  版本，若與執行中程序不同（例如 `npm i -g duduclaw` 更新後 gateway 還沒重啟、
  桌面 App 舊 sidecar 還在跑），頁面直接顯示「新版本 vX 已安裝完成，重新啟動
  後生效」，不再讓 CLI 與 dashboard 各說各話。

### Changed
- **個人版再精簡**（客戶回饋第二輪）：側邊欄的「＋交辦任務」按鈕與搜尋框、
  行動版底部導航的中央 ＋交辦 按鈕在個人版一併隱藏（任務看板仍可由
  進階群組與 ⌘K 進入）。

### Fixed
- **系統更新誤判「已是最新版本」**（客戶實測：最新版本顯示 `vdesktop-v1.46.2`）：
  桌面安裝包 release（`desktop-v*`）比 CLI release 晚幾分鐘發布，會搶走 GitHub
  的 Latest 標記，`releases/latest` 便回傳安裝包 release——版本比較把
  `desktop-v1.46.2` 解析成 (0,46,2) 而誤判不需更新。三層修復：
  ① `check_update` 遇到非 CLI 版本 tag 時自動解析對應的 `v*` release
  （API 失效時退回確定性資產網址）；② `desktop-release.yml` 發完桌面版後把
  Latest 標記還給 CLI release；③ `install.sh` / `install.ps1` 解析 latest tag
  時剝除 `desktop-` 前綴並拒絕非版本 tag。GitHub 上的 Latest 已手動指回
  `v1.46.2`，既有部署的更新檢查立即恢復正確。

## [1.46.2] - 2026-07-29 — 更新檢查限網環境自救（api.github.com 不通 fallback）

## [1.46.1] - 2026-07-29 — prompt 洩漏修復＋跨通道對話清單

### Added
- **對話頁跨通道 session 清單**（客戶回饋）：對話頁左欄現在列出所有通道的
  對話紀錄——Telegram、Discord、LINE、Slack 等外部通道的 session 與網頁對話
  一起按 session 分開顯示，每列帶通道徽章與負責的 AI 員工；點選即載入該對話
  並自動切換到對應員工接續。內部工作 session（cron／派工）不混入清單。
  非 admin 使用者維持原本的單員工範圍（伺服器端 fail-closed）。
- **session 標題自動跟隨討論內容**：新增背景 titler 任務（10 分鐘一輪，
  沿用 summarizer 的 utility LLM 通道）——活躍 session 產生 ≤20 字簡短標題，
  討論推進 6 輪以上自動重新命名；未命名前維持「第一則使用者訊息」fallback。
  成本護欄：僅處理 48 小時內活躍的 session、每輪最多 5 個。

### Changed
- **個人版側邊欄重組**（客戶回饋）：主區精簡為 首頁／收件匣／對話／例行工作／
  世界／Skill／知識庫；任務看板、共同計劃、執行紀錄、畫布、檔案、工作時間軸、
  分析報表、OS、記憶、專家包、Widget 工坊、成長 全部收進底部新設的「進階」
  群組（預設收起）；「設定」在個人版也預設收起。個人版隱藏：AI 員工名冊
  （含側欄員工區）、公司組織圖、授權（頁面仍可由網址進入）。桌寵工作室改為
  僅桌面版顯示（瀏覽器不再出現占位頁入口）。企業版佈局不變。

### Fixed
- **WebChat 回覆洩漏整段 prompt 內部結構**（客戶實測：回覆裡出現
  `<conversation_history>`／協定 reminder／框架指令原文）。兩個根源都堵死：
  ①PTY pool 的空回覆 retry reminder 含 sentinel 字面——reminder 是「打字」進
  互動 REPL 的，TUI 會把輸入重繪多次（輸入框回顯＋送出後 transcript），兩次
  回顯就湊成一對 sentinel，抽取器「取最後一對」規則把 reminder＋整段 prompt
  當成答案回給使用者。reminder 改為描述 sentinel 而不輸出字面；所有 pool 回傳
  路徑（含 managed worker）加 `answer_leaks_prompt_scaffold` 閘——含鷹架標記
  的「答案」視同協定失敗（標記 unhealthy → fresh-spawn fallback），絕不出口。
  ②grok 空 stdout 的 PTY 重試原樣回傳 `strip_ansi(stdout)`——TTY 下 CLI 會
  渲染輸入，等於整段 prompt 跟著回覆一起出去。新增 `salvage_pty_stdout`：
  切掉 prompt 尾端之前的所有輸出，殘留鷹架或空結果一律誠實回錯（fail-closed），
  寧可走錯誤分類也不洩漏。
- **知識庫空狀態文案洩漏內部術語**（「透過 MCP wiki_write 工具建立第一個頁面」
  ——使用者不知道 MCP 是什麼）：個人＋共享知識庫空狀態改為可操作指引
  （在對話裡吩咐 AI 員工「幫我把退貨規則記到知識庫」即建立第一頁；
  專家包附帶現成 SOP 頁面），三語同步。

## [1.46.0] - 2026-07-29 — 專家包組織化＋OS 主動關懷＋桌寵漫遊

### Added
- **OS 自動化範本（一鍵套用）**：OS 頁新增「自動化範本」卡——①新檔案自動處理
  （監看資料夾出現新檔 → AI 員工判斷性質、歸檔、回報；自動補 `[os_watch] paths`）
  ②切到特定 App 提醒（前景切換符合關鍵字 → 推播提醒到你的頻道）。皆經既有
  `autopilot.create` 完整驗證與熔斷保護，建立後可在 設定 → Autopilot 管理。
- **footprint 蒸餾重啟持久化**：日聚合每 15 分鐘快照到
  `os/<agent>/footprint-aggregate.json`（原子寫入），啟動時回載——重啟最多損失
  一個檢查間隔，不再整天歸零（先前常重啟的 gateway 永遠蒸不出足跡記憶）；
  跨午夜重啟的昨日 bucket 會在首個 tick 補蒸餾。

### Fixed
- **收到寒暄卻重啟舊任務、狂呼叫 MCP 工具**（客戶實測：一句 hi 等三分鐘、
  agent 自己去 Google Drive 搜前一輪任務的檔案）：對話歷史注入沒有任何框架
  指令，熱心 persona 把歷史裡未完成的任務當成常設工單。修：
  `<conversation_history>` 之後明示「歷史僅供參考、只回應 <current_message>、
  舊任務非經要求不得重啟」，並在常駐系統規則加「互動節奏」段（寒暄直接回覆、
  不呼叫工具）——CLI 與 Direct API 全路徑生效。
- **OS 員工主動關懷從未作動（四層斷點全修）**：實測回報 os_native agent 開一天多
  零輸出。①主動關懷檢查要求手寫 PROACTIVE.md、缺檔靜默跳過（連 log 都沒有）——
  os_native agent 現在缺檔自動改用內建「OS 關懷」檢查（預設回 PROACTIVE_OK、
  寧靜默勿打擾）；②OS 感知資料與關懷檢查完全沒接線——前景 app 輪詢現在落地
  當日 JSONL（`os/<agent>/frontmost-<日期>.jsonl`，僅 app 名＋時間、絕不含視窗
  標題；跨重啟保留今昨兩日），檢查時聚合成使用時長摘要以 `<os_observations>`
  DATA 區塊注入；③通知目標未設 → 訊息直接丟棄——現在 fallback 到該 agent 最近
  的可推播頻道對話（sessions.db，webchat 除外）；④**bus_queue 的
  `proactive_notification` 從來沒有消費者**（dispatcher 只認 agent_message）——
  就算前三關全過，訊息也永遠死在佇列（Odoo webhook 主動通知同樣斷頭）。
  dispatcher 現在消費並經 forward_to_channel（token cascade＋分段）真正送出。
  quiet hours（預設 23–8）照舊生效。
- **設定列窄容器跑版**：`SettingsRow` 控制欄固定寬＋label 欄可壓到 0，在雙欄卡片
  等窄容器中 CJK label 被壓成一字一行（本地推理頁實測回報）。改 flex-wrap＋
  label flex-basis 下限：空間不足時控制欄自動換行成整行，不再擠壓 label。
- **內建專家包裝完的櫃檯主管載入失敗**：expert.toml roster 的 `role = "front_desk"`
  被原樣寫進 agent.toml，但 `AgentRole` 沒有這個變體——gateway 啟動掃描直接跳過
  該 agent（`unknown variant front_desk`）。三層修法：①核心 `AgentRole` 接受
  `front_desk`／`front-desk` 別名（對應 TeamLeader）——**已裝壞的 agent 免手術、
  升級後自動復活**；②安裝時 role 一律正規化為 canonical 字串，未知值退 worker
  並回報，不再原樣直寫；③安裝測試新增「真 registry 解析器 round-trip」回歸
  （`rank_for_role` 同步接受 kebab/underscore 兩形）。

### Added
- **桌寵自主行為引擎（Codex Pets 式漫遊）**：桌寵不再只會原地待機——待機 6–14 秒
  後加權隨機挑行為：走路（真的移動桌寵視窗橫越桌面，約 55px/s，碰到螢幕邊緣自動
  折返；新 Tauri 指令 `pet_move_by` 以螢幕邊界 clamp）、坐下休息（4–9 秒）、揮手、
  跳躍；任何點擊／拖曳／agent 訊號立即中斷漫遊，`prefers-reduced-motion` 全程尊重。
  同時強化 8×9 烘焙表的每列動作符號（跑步彈跳＋前傾＋步態擺動、跳躍完整弧線
  蹲—躍—伸展、waiting 列改為真正的坐姿蹲伏、failed 深度垂頭），並修正 FIT_H 168
  導致跳躍頂點裁頭、動作被壓平成「每格都一樣」的問題（148＋預留 headroom）。
  寵物工作室生成／啟用／刪除補 busyRef 重入閘——連點不再重複觸發生成。
- **專家包部門×階級（WP-ORG 完整組織藍圖）**：專家包不再攤平混列——
  ①目錄按 5 大類別分組（醫療健康／專業服務／零售與電商／生活服務／教育與托育；
  對照表在 `duduclaw_core::org`），premium `experts/` 下的單一專家包（如 CAD 製圖員）
  也一併上目錄（`kind: expert`，與團隊包同流程一鍵安裝）；②`expert.toml` 新增
  `[expert] category` 與每位成員 `department`／`rank`（executive/manager/staff；
  unknown-keys 前向相容，`expert pack` 驗證非法值）；③convert-teams 自動蓋章
  （front_desk→manager、worker→staff＋共用 kit→部門對照：docs-admin→行政、
  billing-admin→財務、care-followup→客服、dispatch-scheduler→營運、inventory→倉管、
  marketing→行銷、sales-followup→業務）；④安裝時把 `department` 寫入
  `[agent] department` 並確保 `shared/wiki/departments/<部門>/` 知識空間存在——
  裝完的 AI 員工直接落入部門頁與組織圖；⑤新 `--attach-under <agent-id>`
  （CLI flag＋`experts.install`/`install_builtin`/`install_draft` RPC 參數）：
  包的根主管可掛接到既有主管（如 CEO 幕僚長）之下，目標不存在先擋下、絕不半裝
  （fail-closed）；dashboard 安裝與自製生成流程都有「匯報對象」選單（候選＝main
  或已有直屬部下者）；⑥組織圖節點顯示部門、首登團隊精靈建立的 worker 也自動落部門。
  內建包轉換快取改版為 `cache/experts-builtin-v2`（舊快取自動失效重轉）。
- **錄製 → 技能閉環（WP3.3）**：新增五個 MCP 工具——`browser_record_start` /
  `browser_record_stop`（真實 Playwright 瀏覽器帶 tracing + HAR + UI 動作記錄器，
  停止時 HAR 就地脫敏：Authorization/cookie/set-cookie/token 類 query 與 body 值
  一律替換為 `<env:VAR>` 佔位符）、`desktop_record_start` / `desktop_record_stop`
  （macOS 每秒截圖＋前景視窗標題，不記錄鍵入內容）、`skill_from_recording`
  （把錄製蒸餾成 SKILL.md 草稿：安全掃描 fail-closed → 隔離草稿區＋管理員審批，
  絕不直接進技能庫）。功能預設關：需 `agent.toml [capabilities] recording = true`
  ＋新 MCP scope `recording`（dispatch gate 雙重把關、fail-closed）。錄製檔落
  `~/.duduclaw/recordings/<id>/`（權限 700，30 分鐘自動停止上限）。
  詳見 `docs/guides/recording-to-skill.md`。
- **內建專家包目錄＋LLM 引導自製**：專家包頁新增「內建專家包」目錄——22 產業團隊包
  一鍵安裝（與首登精靈同一 premium 資料源；`experts.catalog`／`experts.install_builtin`，
  gateway 以冪等 convert-teams 快取＋既有安裝管線子行程執行，premium 未部署顯示
  fail-safe 空狀態）。「自製專家包」四步引導：產業／描述／團隊規模／通路 → LLM 生成
  完整草稿（模型只輸出 STRICT JSON、expert.toml 後端確定性渲染，消除 TOML 語法失敗
  模式）→ 預覽＋回饋重生成（上限 5 輪）→ 安裝（LLM 產物視同外來內容過完整安全掃描；
  no-hooks 三層防線：schema 無欄位＋prompt 禁止＋遞迴後驗，安裝邊界再驗）。draft
  24h 過期清理、全 RPC admin-only、路徑／slug 全圍欄（含 agent.partial 白名單防
  `[agent]` 劫持）。gateway 2882 測試綠。
- **config/CLI-only 功能搬上 dashboard（四項）**：①AI 員工編輯頁 danger zone 新增
  「錄製功能」開關（`[capabilities] recording`，開啟才允許錄製工具，fail-closed 不變）；
  ②設定頁技能分區新增「每日技能推薦摘要」開關（`[skills] gap_digest_enabled`，寫入即
  生效免重啟）；③**專家包管理頁**（`/experts`，admin-only）——已安裝清單（hooks 狀態
  badge）、上傳 .zip 安裝（`POST /api/experts/upload`：JWT＋admin、50MB 上限、PK magic
  ＋檔名淨化＋路徑圍欄；安裝走子行程重用 CLI 完整管線含 zip-slip 防護與安全掃描；
  dashboard 安裝 hooks 一律進審批、永不 auto-trust）、移除（wiki 圍欄＋空目錄修剪）、
  hooks 審批核准後一鍵套用；共用契約下沉 gateway `expert_admin.rs`，CLI re-export
  同一實作零漂移；④AI 員工「主動通知目標」（`[proactive] notify_channel/chat_id/
  thread_id`，goal loop 與缺口推薦的推送目的地）進編輯頁＋設定頁。新增 14 測試
  （非 admin 全拒／上傳 traversal 全困／hooks fail-closed／remove 圍欄），gateway 2861
  全綠。仍留 CLI：expert pack/export/convert-teams、eval/test 等開發者工具。
- **專家包 hooks 審批全迴路＋團隊劇本批次遷移**：專家包含 hooks 時 install 隔離至
  `hooks-disabled/` 並經 ApprovalBroker 建立 `expert_hooks_enable` 審批（TTL 24h、
  fail-closed：拒絕／逾期／無授權一律停用），`duduclaw expert hooks <slug>` 套用決定，
  CLI `--trust-hooks` 顯式放行（比照 plugin `--trust` 慣例）；dashboard 審批中心
  相容顯示免新 UI。新增 `duduclaw expert convert-teams`：22 個產業團隊劇本批次轉為
  expert 包（front_desk/workers reports_to 佈線、產業 SOUL overlay、分派劇本→
  Agent Skills 格式 skill、wiki SOP per-slug 命名空間），轉換冪等，3 包真 binary
  全生命週期活測通過；順修 `expert remove` 空 wiki 目錄殘留。
- **Skill 市集每日聚合推薦（WP2.6 P1）**：騎既有 CronScheduler tick 的 24h 摘要——
  聚合過去一天的能力缺口（gap accumulator）取 top3、以聯邦市集搜尋掛推薦技能，
  推送到 `[proactive]` 通路；無缺口完全靜默。預設關（`config.toml [skills]
  gap_digest_enabled = true` 啟用）。
- **Google Chat 入向附件**：webhook 訊息帶附件時以 service account 走 Chat media API
  下載並歸檔到 agent attachments/（訊息附 `[📎 filename](path)` 參照，與其他通路一致）；
  Drive 型附件因 `chat.bot` scope 不含 Drive 明確降級標註不硬做；任何附件失敗
  皆不擋訊息。純附件無文字的訊息現在也會進 agent。
- **功能開關（Feature switches）— 面向一般使用者的 capabilities 設定**：AI 員工編輯頁
  「工具與權限」新增白話「功能開關」區塊——14 組日常語言功能組（辦公文件／訊息與語音／
  記憶／知識庫／排程／團隊協作／技能管理／Google／Notion／GitHub／Odoo ERP／電腦整合／
  檔案與程式／系統管理），每組一個 Switch＋風險徽章，成員清單由 `tools.builtin_catalog`
  單一事實來源提供（新工具自動歸組，未知分類落入系統管理組）。語意為 deny-compile：
  預設全開（空白名單＝全部可用，`office_script` 因此為預設工具）；關閉某組即把該組
  qualified 工具名寫入 `denied_tools`，重新開啟則移除；部分停用顯示「部分自訂」徽章。
  原始 allowed/denied 清單、wiki 可見範圍、Progent policy 編輯器摺疊進「進階工具設定」
  （agent 已有白名單或 policy 時自動展開）；白名單模式下功能開關停用並顯示說明橫幅＋
  一鍵清除白名單。三語 i18n。

- **工具型錄補完（65 工具）＋完整性守衛**：`builtin_tool_catalog` 補上先前缺漏的 65 個
  MCP 工具（任務看板 tasks_*/goals/plan、上網查資料 web_search/web_fetch_cached/
  web_extract、費用 cost_*、電腦操作 computer_*、技能生態 skill_search/skill_hub_install
  等、reminder/cron、channel_status、模型管理、平台雜項），scope 一律如實標
  `admin`（對齊 mcp_auth C2 fail-closed fall-through 實況）。功能開關新增「任務看板」
  「上網查資料」兩組；cost/computer 歸入系統管理組。新增**完整性守衛測試**：tools/list
  廣播的每個工具必須存在於型錄，未來新增工具漏補型錄會直接讓 build 變紅（與既有
  scope-drift 測試互補）。

- **檔案頁 Office 預覽（LibreOffice→PDF）**：新增 `GET /api/files/preview`——
  docx/xlsx/pptx/odt/ods/odp/csv 由 gateway 以 `soffice --headless` 轉 PDF 後 inline
  串流（mtime 驗證快取於 `<home>/cache/preview/<agent>/`、隔離 LO profile 防 lock 打架、
  60s timeout）；未裝 LibreOffice 回明確 503 zh-TW 訊息。pdf/圖片維持原生 inline。
  檔案頁 office 列現在顯示「預覽（轉為 PDF）」眼睛鍵——先前文案宣稱可預覽但 office
  檔根本沒有預覽鍵。與 download 相同的 JWT 驗證＋路徑圍欄。

- **儲存時 model↔runtime provider 自動對齊**：agent 設定存檔（`agents.update`）時，
  若 `[model] preferred` 與 `[runtime] provider` 為確信的家族不一致（例如 grok-4.5 配
  claude——活體事故：走 Claude CLI 撞 model_not_found 整隻 agent 無法回應），gateway
  直接把 provider 改寫為能服務該模型的 runtime：家族 CLI 已安裝→家族 CLI，未安裝→
  `openai_compat`（API 模式可服務任意模型）；`openai_compat` 永遠視為相容故 API 模式
  設定不受影響；未知模型家族不猜。回應帶 `runtime_provider_aligned`，dashboard 以
  toast 告知「已自動將執行環境調整為 X」並同步表單，不會留下與畫面不同的靜默設定。

### Fixed
- **Grok CLI runtime「只旁白不執行」三層根因（客戶實測 2026-07-28）**：grok-4.5 agent
  回「正在查詢…」但從不真的呼叫工具。①grok 對專案層 `.grok/config.toml` 的 MCP 註冊
  設有互動式資料夾信任閘，headless `-p` 永遠無法核准 → duduclaw MCP server 從未啟動；
  spawn env 加 `GROK_FOLDER_TRUST=0`（活體驗證 `grok inspect` trusted no→yes）。
  ②duduclaw `mcp-server` 在 log 目錄不可寫的環境會因 rolling appender 初始化 panic
  整個死掉（宿主看到 handshake Broken pipe、整個工具面消失）；改用 fallible builder，
  寫不了檔就降級純 stderr logging，絕不為診斷便利犧牲工具面。③`grok -p` 無法顯示
  工具核准提示 → spawn 加 `--permission-mode bypassPermissions`＋能力對映
  `--sandbox`；built-in workspace/read-only profile 會連 MCP 子行程一起關進沙箱、
  弄死 SQLite 狀態（活體實錘「Failed to open memory DB」），故改寫入自訂 profile
  `duduclaw-ww`/`duduclaw-ro`（extends 內建＋放行 duduclaw home）。活體驗證：
  `grok -p` 實際呼叫 `list_agents` 回傳三 agent 名單。
- **產檔交付雙保險（活體事故 2026-07-28）**：agent 真做出 .docx 卻寫到 ~/Desktop 且
  沒帶 `📎DELIVER:` 標記——使用者只收到文字、檔案頁無歸檔。①提示詞硬化：交付規則
  從 office SKILL.md 升級為 channel system prompt **常駐區塊**（靜態文字、prompt-cache
  友善）：產檔必須存工作目錄、回覆末行逐檔標 `📎DELIVER:<絕對路徑>`；②確定性安全網：
  無標記但回覆提及文件產出時，回覆後掃描 agent 目錄（深度≤3、15 分鐘窗、office 副檔名、
  對 attachments 歸檔去重、上限 3 檔）自動送檔＋歸檔（`sweep_undeclared_deliverables`）。
- **桌寵「調整大小」只放大隱形視窗**：`pet_set_scale` 縮放的是原生視窗，但
  `PetRuntime` 的 canvas／原相 `<img>`／內建 DuDu 都寫死 170px——調大只撐大透明外框，
  調小會被裁切。顯示尺寸改為跟隨視窗短邊（92%，含陰影與跳躍動畫餘裕）並監聽 resize，
  小／標準／大現在真的縮放像素寵物本體。
- **API 模式 agent 的 qualified 工具名比對失效**：dashboard 工具選單寫入的
  `mcp__duduclaw__<name>` 格式在 `filter_tool_defs`（openai-compat／Direct API 工具迴圈
  的能力過濾器）中比對不到 registry 廣播的裸名——qualified 白名單會把 API 模式 agent 的
  MCP 工具全數濾掉、qualified 黑名單則靜默不生效。`tool_base_name` 現會剝除
  `mcp__<server>__` 前綴後再比對（deny 方向跨 server 過寬屬 fail-safe，已註記）。

### Changed
- **Gateway 選擇器 v2：自動選擇＋防 mDNS 蔓延（WP-GW §2.5）**：桌面 app 啟動改為
  **自動選擇**——記住的 gateway `/healthz` 通就直接連（不再顯示清單或倒數）；沒記住者
  掃描區網，恰 1 個自動連並 toast、多個顯示清單、0 個自動啟動本機；記住的 gateway 連不上
  才落到 picker。tray 選單新增「切換 Gateway」隨時重開 picker（帶 `switch=1` 標記略過
  自動連）。防蔓延三道閘：`[server] mdns_advertise` **預設值改為 false**（明確開啟制，
  只有刻意設定的辦公室 gateway 會出現在偵測清單）；新增 env override
  `DUDUCLAW_MDNS_ADVERTISE`（`0`/`1`，優先於 config）；桌面 app spawn 的本機 sidecar
  一律注入 `DUDUCLAW_MDNS_ADVERTISE=0`（員工桌面永不變成區網 gateway）。廣播開關納入
  dashboard **設定 → 系統 → 伺服器**（admin-only，含顯示名／bind／mDNS 開關，bind 與
  廣播變更標示需重啟）。`docs/guides/deployment-guide.md` §10 同步更新。

### Added
- **桌面版 Gateway 選擇器 + 區網 mDNS 偵測（WP-GW）**：企業把 gateway 架在公司伺服器，
  員工桌面 app 啟動即進入「選擇 Gateway」頁（登入前、bundled UI），可選本機、
  區網自動偵測到的 gateway，或手動輸入公司位址。Gateway 端以 mDNS/DNS-SD 廣播
  `_duduclaw._tcp.local.`（TXT 帶 version／name／tls），`config.toml [server]
  mdns_advertise` 預設開、可關（企業防掃描），graceful shutdown 反註冊，廣播失敗
  只 warn 不擋啟動；新增 `/healthz` JSON 探針（version／name）。桌面端四個 Tauri
  command：`gateway_discover`（browse 3 秒去重）／`gateway_health`（healthz 2s
  timeout）／`gateway_select`（scheme 白名單 http/https fail-closed → 存
  `~/.duduclaw/desktop.json` last+recent≤5 → navigate 主視窗；選遠端釋放本機
  sidecar）／`gateway_last`。桌面主視窗改為 debug/release 一律先開 bundled
  `/gateway-picker`（根治舊 debug build 停在 `tauri://localhost` 的 port 錯位），
  由 picker 觸發 navigate；記住上次選擇並 3 秒倒數自動連（任意互動取消）。三語
  i18n、Calm Glass。附 `docs/guides/deployment-guide.md` §10 企業區網部署（含
  `mdns_advertise` 開關與 HTTPS 建議）。

## [1.45.0] - 2026-07-26 — 客戶實測修復與一鍵串接

### Added
- **Notion／GitHub／Google Sheets 一鍵串接**：整合頁新增 Notion 與 GitHub 分頁（與
  Google 相同的三態連接流程，抽成共用元件），Sheets 併入既有 Google 授權（scope
  增補，舊授權會引導重新授權）。新工具 13 個：Notion `notion_search`／
  `notion_page_read`／`notion_page_append`／`notion_status`；GitHub
  `github_search_issues`／`github_issue_read`／`github_pr_read`（僅檔案清單不拉
  diff）／`github_issue_comment`（對外可見，建議搭配審批）／`github_status`；
  Sheets `sheets_read`／`sheets_append`（支援直接貼試算表網址）。新增
  `NotionRead/Write`、`GithubRead/Write` 權限範疇，未列舉工具照舊 fail-closed。
  OAuth 交換層修正兩個 provider 相容陷阱：Notion token 端點要求 HTTP Basic 認證
  ＋JSON body（並於授權 URL 補 `owner=user`）、GitHub 需 `Accept: application/json`
  才回 JSON——交換請求組裝已純函式化並以單元測試鎖住。附 `docs/guides/notion.md`
  與 `docs/guides/github.md`。
- **Google Workspace 一鍵串接（Gmail／日曆／Meet）**（本版預設隱藏：整組工具與 dashboard 分頁
  待原廠 OAuth App 通過 Google 驗證後開放；操作者可設 `config.toml [integrations]
  google_workspace = true` 搶先啟用）：整合頁新增 Google 分頁——填入
  自家 Google OAuth 用戶端後一鍵授權，AI 員工即獲得六個原生工具：`gmail_search`／
  `gmail_read`（唯讀搜尋與讀信，附件只列清單不下載）、`gmail_create_draft`（**只建
  草稿不寄送**——寄出由使用者在 Gmail 按下，安全預設）、`calendar_list_events`／
  `calendar_create_event`（可同時產生 Google Meet 會議連結，即 Meet 支援）、
  `google_status`（連線診斷）。全部原生實作、不依賴第三方套件；token 以 AES 加密
  落地並自動 refresh；新增 `GoogleRead`／`GoogleWrite` 權限範疇（未列舉工具照舊
  fail-closed）；寫入類工具建議搭配 `approval_required_tools` 審批。附
  `docs/guides/google-workspace.md` 申請與連接指南。順帶修復既有 OAuth 缺陷：
  用戶端憑證過去從未持久化（授權完即丟，token 過期後 refresh 必然失敗、
  「已設定」狀態恆為否），現存於加密設定檔並於重授權時預填；Google 授權 URL 補
  `access_type=offline`（否則 Google 不核發 refresh token）。`config.toml [general] default_language`（全新安裝預設
  `zh-TW`）＋ dashboard 一般設定下拉（跟隨輸入／繁體中文／English／日本語）。語言
  指令改由系統層在 system prompt 動態注入，SOUL.md 模板內約 20 處重複的語言段落
  同步移除（行業慣用語彙保留）——新 AI 員工不必再各自指定語言。
- **任務類路徑補 Telegram typing indicator**：過去只有一般聊天有輸入中指示，任務
  的「接收」與「執行」拆成兩個非同步階段，執行端（bus 委派／SQLite 佇列／goal
  loop／cron／提醒 Agent 回呼）在長時間 CLI 執行期間完全沒有 typing。四條路徑現在
  於執行前解出目的地（委派走新增的非消費性 `peek_callback`；goal loop 由 task 來源
  反查）建立 RAII typing guard，查不到目的地時靜默降級、絕不影響任務執行。
- **Odoo 客製資料庫探索與安全解鎖**：客戶的 Odoo 幾乎都是客製的，過去 agent 一查
  客戶主檔（`res.partner`）或自訂模型就被硬編碼黑名單擋下、且沒有任何 schema 探索
  能力。本次補上四塊：**(A) 黑名單可配置化**——保留現有安全預設不變，新增
  `agent.toml [odoo] unblock_models`（全域 `config.toml [odoo]` 亦可）opt-out；被擋
  時錯誤訊息分流「安全預設封鎖，可由管理員解鎖」vs「白名單未涵蓋」，`ir.model` /
  `ir.model.fields` / `ir.attachment` 這類系統表即使 unblock 也只允許讀取（寫入／刪除
  一律 fail-closed）。**(B) Schema introspection**——`OdooConnector::introspect_schema()`
  特權唯讀掃描 `ir.model`／`ir.model.fields`（只回中繼資料、絕不回資料列），過濾
  transient／框架雜訊、上限保護；dashboard `odoo.discover_schema` RPC 回傳模型清單並
  把結構摘要寫進共享知識庫（`odoo/schema` context 層可自動注入、`odoo/schema-fields`
  deep 層供搜尋，遵守 `.scope.toml` 政策）；新增 agent MCP 工具 `odoo_schema_fields`
  查單一模型欄位。**(C) 安全客戶搜尋**——新增 MCP 工具 `odoo_partner_search`，對
  `res.partner` 唯讀查詢且欄位白名單固定（僅 name/email/phone/city/ref 等，不含銀行
  ／稅務欄位），繞過黑名單但仍受 `OdooRead` scope 與 per-agent read 權限管制，補上
  「沒有客戶搜尋工具」缺口。**(D) Dashboard UI**——Odoo 頁測試按鈕旁新增「解析
  資料庫」按鈕，掃描結果可搜尋過濾、x_ 自訂模型醒目標示、勾選一鍵加進 AI 員工的
  allowed_models；三語系（zh-TW／en／ja-JP）齊備。
- **迭代式看板（Iterative Kanban）**：把 Task Board 從「一次性 Q&A」重塑為「人機迭代
  循環」導向。新增 `revising` 狀態——判官否決 goal-mode 任務後不再默默回 `pending`，
  而是進入可見的「修訂中」欄並帶「第 N 輪」徽章；看板欄位 5→7
  （待辦／進行中／驗收中／修訂中／需人工／已完成／受阻，`failed` 併入受阻欄以徽章
  區分，`review` 首次在看板可見——先前 review 任務在板上隱形是缺口）。新增
  `task_iterations` 明細表（每輪 dispatched→submitted→verdict＋否決理由，先例
  vibe-kanban `coding_agent_turn`／Linear `AgentSession`）與 `tasks.iterations` RPC；
  詳情頁新增修訂時間軸與**雙時鐘**（Agent 處理時間 vs 完整人機週期）。軟上限徽章
  「報酬遞減」（達 `soft_cap` 時亮琥珀色但不擋，證據：外部回饋下 2 輪拿走 76-95%
  增益 arXiv:2604.10508、Self-Refine 官方上限 4 arXiv:2303.17651）、租約逾期
  「stale」徽章。新增 `tasks.flow_metrics` RPC（per agent：一輪過件率 first-pass
  yield／平均輪次／agent 秒數 vs 週期秒數／驗收佇列深度）與 `review` 欄 WIP 上限
  （`config.toml [task_board] review_wip_limit`，預設 10；超標亮警示＋以 Little's Law
  推算等待時間＝佇列深÷近 7 日日均驗收數）。三語系（zh-TW／en／ja-JP）齊備。
- **帳號頁新增「CLI 訂閱憑證」區塊**：Grok／Codex／Gemini 的訂閱登入存在 CLI 自家
  憑證檔（`~/.grok/auth.json` 等）、不是 rotator 帳號條目，過去登入成功後在
  「帳號與預算」頁完全看不到。新增 `accounts.cli_credentials` RPC（只讀存在性
  與 mtime，絕不讀憑證內容）＋帳號頁卡片區：顯示已登入／未登入、憑證更新時間、
  「登入／重新登入」按鈕直接開一鍵登入 modal；未安裝的 CLI 不顯示。Claude 刻意
  排除（其登入本來就以 rotator 帳號卡呈現）。

### Changed
- **goal loop `iteration_cap` 預設 8→5（硬上限）**：迭代式看板調研顯示 critique-revise
  型迴圈（判官否決→重試）的增益上限低——外部回饋下 2 輪拿走 76-95%
  （arXiv:2604.10508），第 3 輪起趨近零、純自審多輪甚至負報酬（arXiv:2310.01798），
  故 Complex goal 的硬上限由 8 下修為 5。Simple goal 維持 3（`iteration_cap_simple`
  不變）。新增 `soft_cap`（預設 3）只標記「報酬遞減」不擋。覆寫方式：
  `config.toml [goal_loop] iteration_cap = <n>` / `soft_cap = <n>`。oscillation（連續
  兩輪同構否決）與 hard cap → `needs_human` 的既有行為與優先序不變。

### Fixed
- **AI 員工改名後仍自稱舊名字（「改資訊沒有改到靈魂」）**：改名 RPC 過去只寫
  `agent.toml` 的 `display_name`，但 system prompt 的自稱完全來自建立時燒進
  SOUL.md 的名字，兩邊脫鉤。雙重修復：① prompt 組裝層（cron／通路回覆／minimal
  三個入口）最前面注入 `agent.toml` 的權威名字宣告，SOUL 內文過期也壓得住；
  ② 改名時（dashboard `agents.update` 與 MCP `agent_update` 兩路）自動把
  SOUL.md／IDENTITY.md 內的舊名精確替換為新名（原子寫回），預設 `@舊名` trigger
  一併連動。
- **回覆有時不會發到 Telegram（已讀不回）**：一組疊加的靜默失敗——
  ① `forward_to_channel` 的 Telegram 分支獨家把發送失敗吞成成功（其他通道都回
  `Err` 觸發 5 次重試），現已比照其他通道回 `Err`；② goal loop 進度推播「先標記
  已送再發送」且不看結果，`done`（最終答案）一次網路抖動即永久遺失，通知結果現在
  三態化（送達／無目標／發送失敗），失敗於下一輪 tick 重試（上限 3），needs_human
  ／kickoff 通知同步修正；③ 委派子任務失敗過去只寫 DB 不通知任何人，現在轉發
  分類式保底訊息（不洩 stderr／內部路徑），JSONL 與 SQLite 兩條佇列一致；
  ④ Telegram 發送層對 429／5xx 加退避重試（尊重 `retry_after`）、HTML→純文字
  fallback 結果不再被丟棄、語音發送失敗補送文字版、進度訊息刪除移到空回覆判斷
  之後；⑤ 五個刻意靜默分支（封鎖／熔斷／L3 靜音）補記 `channel_failures.jsonl`
  （`silent_by_design` 標記），dashboard doctor 查得到「為什麼這個人已讀不回」。
- **Grok／Codex／Gemini 一鍵登入成功卻顯示「未自動加入帳號（no token captured）」警告**：
  只有 `claude setup-token` 會把 token 印在畫面上供擷取；其他 CLI 登入成功時是把憑證
  寫進自家儲存（`~/.grok/auth.json` 等，這正是 cli_auth 判定成功的檔案監看訊號），
  runtime 直接讀該儲存、本來就不需要帳號條目——「無 token 可擷取」是這些 CLI 的
  唯一正常成功路徑，卻被當成異常回報。`auth.cli_login.finalize` 現在對非 Claude
  runtime 回傳 `reason="cli_store"` + 憑證檔路徑，CliLoginModal 據此顯示
  「登入成功，憑證已存入 ~/.grok/auth.json，此 CLI 的 AI 員工可直接使用」；
  Claude 登入抓不到 token 仍維持原警告（那才是真正的擷取異常）。

## [1.44.0] - 2026-07-24 — MCP 工具面認證斷鏈根治、dashboard 健康診斷活體探測、Homebrew 通路復活

### Added
- **Dashboard 健康診斷納入 MCP／Grok 活體探測**：`system.doctor`（設定 → 健康診斷）
  新增兩張卡片——「MCP 工具服務」（spawn 一次 `mcp-server` 送 initialize，判定 M6
  認證斷鏈，即「agent 叫不到工具」根因）與「Grok CLI」（binary＋版本＋活體
  `grok -p "ping"`＋登入判定；未安裝則不顯示）。探測邏輯抽成
  `duduclaw-gateway::doctor_probes` 共用模組，CLI `duduclaw doctor` 與 dashboard
  讀同一份實作不再漂移；三個外部探測並行執行、前端 doctor RPC timeout 提高到
  60s；DoctorTab 卡片標題改走 i18n（zh-TW／en／ja，未知檢查名 fallback 原字串）。
  Docker 部署的經銷商從此不必進 container 跑 CLI 即可取得診斷證據。

### Fixed
- **非 Claude runtime 的 MCP 工具面在 v1.31 之後全滅（Grok「查 Odoo 不行」根因）**：
  M6 fail-closed 認證（v1.31）讓 `duduclaw mcp-server` 沒有 `DUDUCLAW_MCP_API_KEY`
  就拒絕啟動，但全產品沒有任何 spawn 路徑注入這把 key——Grok 這類 CLI 只用 config
  宣告的 env 區塊起 MCP child（不繼承父環境），duduclaw MCP server 開機即死，agent
  無聲失去整個工具面（Claude 因 CLI 會傳完整 env、在 gateway 環境有 key 時才「碰巧」
  存活）。修法：① gateway 啟動時自動 provision 一把 internal key（`config.toml
  [mcp_keys]`，client_id=`gateway-internal`，首次生成、之後重用，`with_file_lock`
  防多實例競態；operator 自設的 env key 永遠優先）；② 新增
  `duduclaw_core::mcp_forward_env_vars()` 作為所有 MCP env 組裝點的單一事實來源
  （home/port/instance + MCP 認證），並套用到全部六個組裝點——grok
  `.grok/config.toml`、codex `-c` overrides、gemini/antigravity settings.json、
  Claude `.mcp.json` template（含 global settings 註冊）、direct tool-loop
  `mcp_client_envs`；③ `duduclaw doctor` 新增「MCP Server 冷啟動診斷」：跑與 gateway
  相同的 provisioning 鏈後實際 spawn 一次 `mcp-server` 送 initialize，一條指令判定
  「agent 叫不到工具」是不是認證斷鏈。外部／stdio 呼叫者沒 key 仍被拒（M6 的安全
  目標不變）。另澄清：Odoo `allowed_actions = []` 空清單語義是全放行（permissive），
  截圖中「空白名單會擋工具」為模型誤述，非本次斷鏈原因。
- **Homebrew tap 凍結在 v1.8.8（落後 35 個版本）**：tap formula 過去靠手動更新、
  不在 release.sh 的任何清單裡，v1.8.8 之後就再也沒人記得——`brew install
  zhixuli0406/tap/duduclaw` 一直裝到 2026-05 的舊版。現在 release.sh 補上完整
  Homebrew 流程：`audit`／`verify` 各多一列 tap 版本檢查（drift 會被點名），新增
  `./scripts/release.sh homebrew [version]` 子指令——抓 release assets 的官方
  `.sha256`（缺任一平台即 fail-closed 中止）、重新產生 formula、commit + push 到
  tap，冪等可重跑；bump 後的 next steps 也加入此步驟。formula 本身同步改為
  **預編譯二進位安裝**（macOS／Linux × arm64／x64 四平台，含 Python SDK），
  不再要求使用者裝 rust + node 從源碼編譯。

## [1.43.0] - 2026-07-23 — Grok 訂閱帳號全鏈路：headless 根治、dashboard 裝置碼登入與 doctor 診斷

### Fixed
- **Grok CLI headless 空輸出根治**：經銷商客戶機上 `grok` 互動模式正常、SuperGrok device-auth 已登入，但 DuDuClaw spawn 的 `grok -p` 一直回空 stdout（exit 0）——與 2026 Claude `-p` OAuth 訂閱被擋同構。`GrokRuntime` 現在（1）顯式注入使用者真實 `HOME`/`USERPROFILE`（優先取 gateway 已解析的 `<user>/.duduclaw` 之父，launchd/Docker 下 `$HOME` 錯誤時仍正確）並轉發 `GROK_HOME`，讓 grok 找得到 `~/.grok` 憑證；（2）以 word-boundary、大小寫不敏感的樣式集辨識未登入/憑證過期 stderr（含誤殺防護），命中即回傳含 "not logged in"/"authentication" 的錯誤讓 `classify_cli_failure` 落 `AuthFailed`（zh-TW 指引「請執行 `grok login --device-auth`」）；（3）空 stdout+exit 0 且非 auth 錯誤時，用 portable-pty 在真 TTY 下一次性重跑同一 `grok -p`（`pty_retry=true/false` tracing 供遠端判讀），救回 headless-under-pipe 這一類；native sandbox 需求時 fail-closed 跳過重試。`duduclaw doctor` 新增「Grok CLI 診斷」段：binary/版本 + 活體 `grok -p "ping"`（15s）回報 exit/stdout 長度/stderr tail/auth 判定/PTY 重試結果，一條指令產出遠端除錯全部證據。

## [1.42.0] - 2026-07-23 — OS 原生主動感知（P2–P4）、API 模型 MCP 工具面與空回覆斷鏈根治

### Added
- **API-mode 非 Claude 模型獲得完整 MCP 工具面**（根治性缺口）：多 runtime 匯流點的 openai-compat
  `AgentRuntime`（`crates/duduclaw-gateway/src/runtime/openai_compat.rs`）過去只送純 messages（無
  `tools`），導致以 API 直連的 Grok／DeepSeek／MiniMax 等 agent 永遠碰不到 15 個 Odoo／memory／channel
  等 MCP 工具——問 Odoo 客戶資料只會回「我先找出 Odoo 設定」就停。現在當 agent 有可用工具時，
  `execute()` 改走 `duduclaw-llm` 的 openai-compat provider ＋ `run_tool_loop`：spawn 一個 duduclaw
  `mcp-server` stdio child（帶 `DUDUCLAW_AGENT_ID`／`HOME`／`PORT`／`INSTANCE`，比照 CLI runtime 慣例）、
  掛 `ToolRegistry`，工具表先過 capabilities 過濾（deny-by-default：`denied_tools` 一律剔除、
  `allowed_tools` 非空取交集）再暴露給模型，並套用靜態 `PolicyKernel` 政策；迴圈上限沿
  `DEFAULT_MAX_TOOL_ITERS`，多輪 token 用量累計進 `RuntimeResponse`。`[capabilities] scoped_tools`／
  `approval_required_tools` 等 dispatch 層閘門在 mcp-server 端本來就會執行（MCP dispatch 是強制點），
  此路徑不重複實作。**失敗降級**：MCP child 起不來／registry 空／capability 過濾後無工具 → warn log
  後回退到現行純 messages 路徑（沒有工具總比不回好）；provider／model 傳輸錯誤則向上傳播交給 failover。
  現行未 commit 的空 content⇒Err、歷史空 turn 過濾行為全數保留；tool_use-only 回合不誤判為空回覆
  （只有迴圈終了仍無文字才算 EmptyResponse）。新增 6 個單元測試涵蓋工具過濾、迴圈終止、tool-only
  回合、token 累計與 provider-prefixed model id。
- **Server 映像內建 Grok CLI（xAI Grok Build）**：`container/Dockerfile.server` 比照 `agy`（Antigravity）
  段落樣式，新增 `grok` 官方安裝步驟——`curl -fsSL https://x.ai/cli/install.sh | bash` 下載/驗證後，
  以 `install -m 0755` 從 `$HOME/.grok/bin/grok`（installer 產生的符號連結，`install` 會解引用取得
  真正的 binary）relocate 到 `/usr/local/bin/grok`，再跑 `grok --version` 驗證可執行；未內建
  `XAI_API_KEY`（runtime env 才提供，與其餘 CLI 一致）。同步補上 `~/.grok` 目錄（比照
  `.claude`/`.codex`/`.gemini` 慣例）的 `mkdir` + `VOLUME` 宣告，以及根目錄 `docker-compose.yml`
  的 `duduclaw-grok` 具名 volume 與 `XAI_API_KEY` env 轉發（`docker-compose.quickstart.yml` 因本就
  未列 `OPENAI_API_KEY`/`GEMINI_API_KEY`，維持精簡未變動）。
- **OS-native agent P4-3+：dashboard「OS」頁事件即時流**：「近期感知事件」面板從純快照＋手動刷新
  升級成 WS 即時流。新增 dashboard RPC `os.events.subscribe`／`os.events.unsubscribe`
  （`require_admin`，與其餘 `os.*` 同門檻——os_file／os_frontmost 事件帶檔案路徑與視窗標題，
  敏感度比一般活動事件高，因此沿用 `logs.subscribe` 的每連線旗標訂閱模式，而非
  `activity.new`／`task.*` 的無條件全連線廣播）；訂閱後即時推送 `os.events.entry` frame
  （與 `os.events.recent` 的 `EventRow` 同形狀，僅少一個尚未寫入 `events.db` 的 `id`）；
  每連線滑動 1 秒視窗限速 20 筆，超過丟棄＋計數，斷線重連天然重置；連線關閉隨 task 結束自動
  取消訂閱。前端 `OSPage.tsx` 認證後自動訂閱，推播事件前插進列表（合成負數 id 兼作進場動效
  標記，複用既有 `.animate-fade-up`、`prefers-reduced-motion` 全域已 gate）、環狀緩衝上限 200
  筆；連線離開 `authenticated` 時降級顯示「即時更新已中斷，顯示快照」並保留手動刷新鈕。後端新增
  9 個單元測試（`cargo test -p duduclaw-gateway --lib` 2649 passed / 0 failed）、前端新增 2 個
  （`npx vitest run` 680 passed）。契約詳見 `commercial/docs/TODO-os-native-agent.md`
  「P4-3+ 事件即時流已接線」。
- **OS-native agent P4-3：dashboard「OS」頁後端（RPC＋個人版配額＋主動功能熱重載）**：交付
  dashboard OS 頁的全部後端（前端 `web/` 待接）。**五個 dashboard WebSocket RPC**（`require_admin`）：
  `os.status`（全 agent os_native／watch 路徑＋即時統計／frontmost 輪詢秒數＋running／footprint／
  proactive 三欄／induced 規則數，外加 fleet 配額 `{limit, used}` ＋ edition）、`os.settings.update`
  （per-agent 寫 `os_native`／`[os_watch] footprint`＋`frontmost_poll_secs`／`[proactive]`
  `enabled`＋`base_threshold`(1–5)＋`max_per_hour`(0–1000) → 走配額閘＋熱重載，remap 到既有
  `agents.update` 寫入路徑複用驗證器）、`os.gate.recent`（`proactive_gate.jsonl` 尾部 N 行〈預設
  50、上限 200〉＋四象限聚合）、`os.events.recent`（`events.db` 近 N 筆 `os_*` 事件、newest first）、
  `os.doctor.run`（on-demand 昂貴呼叫：複用 `duduclaw-os` 的 notification／frontmost／calendar 探針
  ＋`mdfind` 存在性，TCC 拒絕 report-only 不繞過）。**個人版 os_native 配額＝1**（鎖 quota 不鎖能力）：
  新增 `license_runtime::os_native_agent_quota(edition)`（個人版 `Some(1)`／企業版 `None`＝無限，
  未發明不存在的 license 欄位），寫入面（設 `os_native=true` 超額 → 結構化錯誤
  `os_native_quota_exceeded`＋zh-TW 文案）與啟動面（`os_events::resolve_os_native_allowed` 穩定
  排序取前 N，其餘跳過＋warn＋audit `os_native_quota_skipped`）**共用同一 quota helper**，fail-closed
  一致。**主動功能熱重載**：新增 `OsFrontmostRegistry`（補齊 P2-4 遺留的 frontmost 輪詢 hot-reload
  債）、`FootprintTracker` 成員資格改 interior-mutable ＋ `set_enabled`（就地啟停聚合，停用丟棄當日
  bucket）、`[proactive]` 三欄因 `ProactiveGate` per-evaluate 讀檔而寫檔即生效（無需 registry，已查
  證）；三者統一走 `agents.update` 既有 `hot_reload_os_watcher` hook，改配置不需重啟 gateway。
  `events_store` 新增 `fetch_recent_by_prefix`。新增 ~18 個單元測試全綠。介面文件（完整 RPC 契約表）
  見 `commercial/docs/TODO-os-native-agent.md`「P4-3 後端已實作」。
- **OS-native agent P4-1：PBD 規則歸納（最小版）**：新增
  `duduclaw-gateway/src/rule_induction.rs`——從 `events.db` 近 7 天事件確定性（零 LLM）歸納
  重複的「OS 感知 → 使用者反應」模式，經 HITL 確認後才生效（ALLOY arXiv:2510.10049 /
  TaskMind CHI'25；設計取捨見 `commercial/docs/research-os-native-agent-methodology.md` §3.2）。
  **偵測**：同一 `(agent, 事件型態, 檔案副檔名／路徑前綴／app)` 的 `os_file`／`os_frontmost`
  感知事件，若在其後固定時間窗（預設 600s）內出現同 agent 的使用者互動（`task.created`／
  `activity.new`）達 ≥N 次（預設 5），即命中——取不到反應訊號就不歸納（不腦補）。
  **候選**：命中模式 → 生成候選 autopilot 規則，action 一律 `proactive_notify`（最保守，建議
  而非代做，天然過 ProactiveGate），複用 dashboard 既有 `validate_autopilot_trigger_event`／
  `validate_autopilot_action` 寫入前驗證；指紋去重，狀態存 `<home>/rule_induction_state.json`
  （`with_file_lock`）。**HITL**：候選走 `ApprovalBroker` request（zh-TW 白話提案文字，感知
  token 過 `sanitize_perception_text`）——核准才寫入 autopilot_store（`enabled=true` + metadata
  `induced=true`/`induced_at`/`fingerprint`/`source`），拒絕／TTL 過期則指紋進 blocklist 不再提；
  **任何候選未經核准絕不生效（fail-closed：無 broker／無可投遞頻道／偵測異常皆壓下）**。頻率
  上限每 agent 每日 2 個候選。`autopilot_rules` 加 additive `metadata` 欄（冪等 migration，
  dashboard rule JSON 增 `metadata` 欄供未來標示與退場）；`EventBusStore` 加 `append_with_ts`
  以保留原始事件時間。**events.db 接線（同批完成）**：新增 `os_events::spawn_os_event_persistence`
  訂閱橋，把 `os_file`／`os_frontmost` 廣播事件持久化進 `events.db`（`EventBusStore` 加 `source`
  欄 + `append_with_source`，冪等 `ALTER TABLE` migration），標記 `source=internal_broadcast`；
  `autopilot_engine::spawn_events_db_poll` 新增 `should_rebroadcast()` 檢查該標記並跳過，避免
  同一事件被 in-process 廣播與 events.db poll 雙重派發。30 分鐘歸納 tick 掛進 `server.rs`
  （`rule_induction::spawn_induction_loop`，master 開關 `config.toml [rule_induction] enabled`，
  `RuleInductionConfig::from_home` 每 tick 重讀），生產 channel resolver 複用既有
  `goal_notify::agent_notify_target`。新增 26 測試（P4-1 本體 16：7 純偵測＋3 候選 JSON＋6 HITL
  整合；events.db 接線 10：rule_induction config load 3＋events_store 3＋os_events 3＋
  autopilot_engine 1），gateway lib 全綠、零回歸。
- **OS-native agent P4-2：persona 抑制規則自動化**：新增
  `duduclaw-gateway/src/persona_induction.rs`——把「主動介入被打槍」的歷史確定性（零 LLM）歸納成
  「何時別打擾」的 persona 規則餵回 `ProactiveGate`（ContextAgent arXiv:2505.14668 persona
  ablation −12.3% F1；設計取捨見 `commercial/docs/research-os-native-agent-methodology.md`
  §1.3/§②-3）。**歸納**：讀 `proactive_gate.jsonl`，按時段（Asia/Taipei `工作時間`/`深夜`）×
  事件型態×interruptibility 三分位分群，同群 `false_alarm` 累計 **≥3 次且跨 ≥2 個不同 UTC 日**
  （GovMem arXiv:2607.02579 式獨立證據門檻，同日多次不算）→ 生成「{時段}的 {event} 類主動通知
  曾被多次忽略/打槍，預設沉默」規則。**雙寫**：`store_temporal` 落地治理記錄（origin=`agent_derived`，
  confidence／origin_trust 依 v1.41 origin ceiling 0.6，掛既有 `PROBATION_RULE_TAG`）；橋接寫入
  `key_facts`（`store_fact`）讓規則實際可被 `ProactiveGate` 既有的
  `autopilot_engine::fetch_persona_lines`→`search_facts` 路徑檢索到——`store_temporal` 寫的
  `memories` 表與 `search_facts` 查的 `key_facts` 表是兩個不同 store，此橋接補上這道落差。
  **Janus probation（撤銷）**：同情境後續若出現 `correct_detection`（證明其實該打擾），下次歸納
  時偵測到晚於規則產生時間的正確偵測 → 對同一 `(subject,predicate)` 再寫一筆 `object="lifted"`
  的 temporal memory row，觸發既有 supersession 鏈自動撤銷。指紋去重（同群不重複歸納）＋
  每 agent 上限 10 條，滿了淘汰最舊現行規則。獨立每小時 tick 迴圈、實際聚合工作每 UTC 日僅跑一次
  （`last_run_day` 短路，不掛進 P2-3 既有 60s 迴圈本體）。**已知限制**：`key_facts` 無
  delete-by-id API，撤銷時橋接的舊規則文字靠既有 `purge_stale_facts` janitor 自然汰除，非立即
  移除；時段固定台灣時區，無 per-agent 設定。未碰 `proactive_gate.rs` 本體／`channel_reply.rs`／
  `failover.rs`／`runtime/`／`situation_classifier.rs`／`rule_induction.rs`／`os_events.rs`。
  新增 19 測試（分組聚合、GovMem 門檻、`plan_induction` 純規劃去重/撤銷/淘汰、`store_temporal`
  寫入形態、key_facts 橋接可檢索、supersession 鏈、端到端流程、daily-tick 閘門），既有
  `proactive_gate`/`proactive_feedback`/`reflexion`/`rule_lifecycle` 69 測試零回歸。
- **OS-native agent P4-4：數位足跡 memory 化**：新增 `duduclaw-gateway/src/footprint_distill.rs`——
  把 `os_file`/`os_frontmost` 感知事件流聚合成溫故式 temporal memory（Memory for Autonomous LLM
  Agents arXiv:2603.07670；設計取捨見
  `commercial/docs/research-os-native-agent-methodology.md` §4.2/§②-5）。**聚合不逐事件寫**：
  `FootprintTracker` 純記憶體按 UTC 日累計每 agent 的前景 app 秒數、活躍目錄事件數、活躍小時分佈；
  背景 ticker 每 15 分鐘檢查一次 UTC 日界，跨界才蒸餾（寫入頻率 O(agents × days) 不是
  O(events)）。**三個 predicate**：`subject="user"`、`daily_active_app` / `active_directory` /
  `active_hours`，Top-N 依統計量排序後編碼進 `object`，同 `(subject,predicate)` 靠既有
  `store_temporal` supersession 自動取代前一天、`get_history` 保留完整鏈。**origin/sensitivity**：
  `origin="agent_derived"`（v1.41 既有類別 ceiling 0.6，未新增 origin class）；
  `daily_active_app`/`active_hours`（源自 `os_frontmost`）標 `Sensitivity::Personal`，
  `active_directory`（源自 `os_file`）標 `Sensitivity::Internal`，對齊 P3-2 既有感知源分級表；
  `stamp_metadata` 的第一個生產呼叫點。**data minimization**：`directory_of()` 在原始路徑上先丟棄
  檔名只留目錄，之後才過 `sanitize_perception_text`——下游從未接觸過檔名任一子字串。**opt-in**：
  `[os_watch] footprint = true`（deny-by-default，疊在 `os_native` 之上，且在聚合層就拒絕，未選用
  的 agent 從不進入追蹤集合，不只是寫入層跳過）；啟動時一次性掃描決定追蹤集合，無 dashboard
  熱重載（同 P2-4 `frontmost_poll_secs` 既有取捨）。`handlers.rs` 的 `apply_os_watch_to_table` 加
  `footprint` 布林寫入，`read_os_watch_json` 原樣回顯整個表故自動帶出新欄位、免改動。**已知限制**：
  聚合狀態純記憶體、重啟遺失當日未蒸餾部分（影響最多一天）；`memory_search`/`search_layer` 尚無
  retrieval-side 依 `sensitivity` 過濾結果的消費者（P3-2 的群聊剝除只做在 persona 區塊/wiki
  namespace 兩層），本次以 `Sensitivity::allowed_in_session()` 組合測試證明寫入面打標正確、可供
  未來消費者使用。未碰 `channel_reply.rs`/`failover.rs`/`runtime/`/`proactive_*.rs`/
  `situation_classifier.rs`/`rule_induction.rs`/`persona_induction.rs`。新增 24 測試（config
  reader、目錄丟檔名、聚合、渲染、origin/sensitivity 打標、群聊剝除組合證明、跨日
  supersession、carry-forward、檢索面 `search_layer` 驗證），既有 `duduclaw-gateway`/
  `duduclaw-memory` 測試零回歸。
- **OS-native agent P3-1：VeriOS 情境五分類 ASK gate**：新增
  `duduclaw-gateway/src/situation_classifier.rs`——OS **行動類**工具（`os_open`，未來 L5b
  原生桌面動作）執行前先做情境五分類 `normal` / `anomaly` / `sensitive` / `missing_info` /
  `user_choice`，**分類標籤即決策依據，明確取代「用機率信心分數決定要不要問人」路線**
  （VeriOS arXiv:2509.07553；不用 confidence 的理由見
  `commercial/docs/research-os-native-agent-methodology.md` §④-5）。兩層分類器：**第一層**
  確定性零 LLM（target 落在敏感集合＝路徑含 credentials/keys/.env/.ssh/系統目錄、非 https URL、
  或被 perception sanitizer 標 suspicious → `sensitive`；缺／空 target → `missing_info`；含萬用字元或
  多候選 → `user_choice`；路徑比對 component-anchored 非裸 substring allow-check，方向 fail-safe），
  **第二層**規則判不出時一次 utility LLM 分類呼叫（account rotator，JSON 輸出，parse **fail-closed
  → `anomaly`**）。決策映射：`normal` → 放行（仍疊 ActionGuard 靜態 always-list，`merge_with_force_approval`
  取更嚴者）；`anomaly`/`sensitive` → ApprovalBroker 人工核准（走既有審批通道，**TTL 過期 = DENY**）；
  `missing_info`/`user_choice` → 不執行、回明確追問訊息給 agent 由上層 LLM 補參數。**每次分類寫
  `tool_calls.jsonl` 審計**（`situation_class` / `situation_source` / `situation_decision` /
  `force_approval`）。**兩套機制收斂定奪**（`os_open` 原走 ActionGuard maybe-irreversible，尚未接
  task-scoped grant 的欠帳）：兩閘**層次分明不平行**——依既有 MCP dispatch 順序，`[capabilities]
  scoped_tools` 若列了 OS 工具則 §3.65 task-scoped grant 閘**先行**（授權此 task 階段可用），本 ASK
  gate 於 §3.7 **其後**處理「授權已核發但此次呼叫情境異常」的殘餘問題；沒列則 ASK gate 全責。grant-gating
  維持 operator opt-in，不強制掛在 OS 工具上。`os_open` 由 ASK gate **取代**原 ActionGuard maybe-judge
  （殘餘情境仍只做一次 utility LLM 呼叫，無雙判），仍與 ActionGuard 靜態 `irreversible_tools` /
  `approval_required_tools` 取更嚴者。18 個模組單測（含 CJK 敏感路徑、注入樣本、正常樣本零誤殺、
  LLM parse fail-closed、決策映射與 merge）＋ 4 個 dispatch 級測試（missing_info/user_choice 追問、
  sensitive→approval 流活體核准 proceed、TTL=DENY）。
- **OS-native agent P3-3：輕量 CEP 時序 pattern matcher**：新增
  `duduclaw-gateway/src/cep_matcher.rs`——autopilot 規則 JSON 新增可選欄位 `sequence`
  （`{"first":{event,match},"then":{event,match},"within_secs":N,"negate":bool}`），
  「A 事件後 N 秒內出現 B」或（`negate=true`）「N 秒內未出現 B」的 in-process 時間窗匹配，
  **100% 確定性 Rust code，不引入任何串流平台（Kafka/Flink/Autogen）也不讓 LLM 生成時序邏輯**
  （arXiv:2501.00906 只借「事件序列模式」概念，`[verified-caveat]` 見
  `commercial/docs/research-os-native-agent-methodology.md` §3.1）。State 完全 in-process
  （`HashMap<rule_id, VecDeque<PendingMatch>>`）、每規則 pending 上限 100（超過丟最舊 + log，
  no-silent-caps）、30s tick 掃描 negate 到期。解析出的 pattern 以合成事件
  `AutopilotEvent::CepTrigger` 重新送回既有 autopilot broadcast bus，`AutopilotEngine` 對此變體
  走與一般規則完全相同的觸發尾段（三態斷路器 → `execute_action` → history/activity），因此
  `proactive_notify` 仍過 `ProactiveGate`、斷路器計數也共用同一計數路徑，未繞過任何既有 gate。
  `autopilot_store.rs` 新增 `sequence` 欄位（additive migration）；write-time 結構驗證（事件名／
  運算子／`within_secs` 範圍）擋掉打不到的規則於建立時，而非首次匹配才發現。24 個單元測試。
- **OS-native agent P3-4：`[os_watch] goal_template` 從檔案事件自主 kickoff goal loop**：`agent.toml
  [os_watch]` 新增可選 `goal_template`（`{path}`/`{file_name}`/`{kind}` 佔位符，沿用
  `AutopilotEvent::OsFileEvent` 已曝露給規則作者的同一組欄位名）與可選 `goal_acceptance`
  （缺省時渲染後的目標描述本身兼作驗收基準）。`os_file` 事件觸發時，`AutopilotEngine` 獨立於一般
  `trigger_event`/`conditions` 規則派發迴圈跑一次 kickoff 檢查：同 `(agent, path)` 10 分鐘內只允許
  成功建立一次（純函式防抖，只有真正建立成功才起算冷卻）→ 佔位符值（`path`/`file_name`/`kind`）
  全部先過 `sanitize_perception_text`（P2-5，含跨進程 `events.db` 橋接可能塞入的任意 `kind` 字串）
  → 渲染模板 → **過 `ProactiveGate`**（P3-4 是 P2-2 文件早已點名的「goal kickoff 前門」——沿用同一份
  `[proactive] enabled` 開關與每小時頻率上限，與 `proactive_notify` 共用同一預算；gate 未接線或
  `[proactive]` 未啟用 → deny-by-default 直接壓下，零 LLM 呼叫）→ Allow 才建立 `goal_mode=true` 的
  任務（`created_by = "goal:os_watch"` 區分於 `/goal` 聊天指令），完全走既有 `GoalLoopDriver` 的
  autonomy_level 核准流／iteration cap／MAV 判官驗收，不重新實作任何 kickoff 後治理。**P1 遺留小修**：
  `validate_autopilot_trigger_event` 的 `KNOWN` 清單一併補上 `os_file`/`os_frontmost`（engine 早就會
  發出這兩個事件，但 dashboard 建立的一般規則此前完全訂閱不到，與先前修過的 `run_at_risk` 同一類
  回歸）。31 個相關單元測試（防抖、CJK 路徑＋注入檔名消毒後渲染零外洩、無模板零成本 no-op、
  無 gate deny-by-default、`[proactive]` disabled 真實走 gate 零 LLM 呼叫、Allow/Suppress 分別驗證
  建立/不建立、dashboard 讀寫欄位同步、trigger_event 白名單修正）。
- **OS-native agent P3-5：隱私回歸案例納入 `duduclaw eval`**：新增 `evals/_privacy/README.md`
  索引 + 3 個獨立 Rust 整合測試檔（**刻意**放在被測 crate 的 `tests/`，不是被測原始檔自己的
  `#[cfg(test)]`，也不是 `duduclaw eval` TOML／transcript——後者是給「agent 行為」用的，這四條是
  gateway/security in-process 不變量，硬湊 transcript 等於零覆蓋，見 README 說明），16 案全綠：
  (a) `os_native=false`（缺／明確 false／TOML 損毀）→ 6 個 `os_*` MCP 工具全數 fail-closed 拒絕
  （`crates/duduclaw-cli/tests/privacy_regression_os_native_gate.rs`，4 案，走真實
  `McpDispatcher::dispatch_tool_call`）；(b) 注入檔名（instruction-override、`<system>`／ChatML／
  `[INST]` 標記）經 `sanitize_perception_text` 中和後不含原始結構性突破字元，含正常 CJK 檔名零誤殺
  對照（`crates/duduclaw-cli/tests/privacy_regression_perception_neutralize.rs`，4 案）；
  (c) `[proactive] enabled=false` 不消耗 LLM 呼叫即壓下、LLM 錯誤／無法解析／超出值域皆
  fail-closed 壓下，含高分放行正對照與注入事件文字不繞過（`crates/duduclaw-gateway/tests/
  privacy_regression_proactive_gate.rs`，6 案，走真實 `ProactiveGate::evaluate_with`）；
  (d) `os_notify` 注入載荷中和但不丟棄、寫入 `security_audit.jsonl`，乾淨內容零審計雜訊
  （同 (b) 檔案，2 案；刻意不跑真實 `osascript` 發送以免每次 `cargo test` 跳出真實桌面通知，
  改直接呼叫生產程式碼實際串接的 `sanitize_perception_text` → `sanitize_osascript` →
  `audit::log_injection_detected` 三段）。未觸碰任何 gateway/security 生產程式碼。
- **OS-native agent P2-1：interruptibility 打擾成本分數**：新增
  `duduclaw-gateway/src/interruptibility.rs`——`InterruptibilityTracker` 訂閱既有 autopilot
  broadcast（`os_frontmost` / `os_file` / `agent_idle`），維護每 agent 15 分鐘滑動視窗，
  `score() -> 0.0..=1.0`（0=可打擾、1=勿擾）。確定性公式：switch 頻率權重最高（0.6，依 CHI'18
  「切換頻率為最強訊號」）＋ file 密度（0.4）＋ idle 乘法 relief；無訊號回中性 0.5。**只讀**
  既有 `AgentIdle` 訊號、不自建第二條 idle 判斷源。8 個單測。
- **OS-native agent P2-2：ProactiveGate 主動介入閘門**：新增
  `duduclaw-gateway/src/proactive_gate.rs` ＋ autopilot 新 action `proactive_notify`（既有
  `notify`/`delegate`/`run_skill` 確定性規則不受影響）。感知文字先過 `sanitize_perception_text`
  → 組 prompt（sanitized 事件 XML DATA + persona 偏好 `search_facts` Ebbinghaus 排序 +
  interruptibility）→ 一次 utility LLM 呼叫（account rotator）算 `proactive_score` 1–5，JSON
  parse fail-closed → 動態閾值 `𝒯ℛ = base(3) + round(interruptibility × 2)` → `≥𝒯ℛ` 才放行原
  notify，否則壓下。**fail-closed**：LLM error / 30s timeout / parse fail → 不打擾。每次決策寫
  `<home>/proactive_gate.jsonl`（`with_file_lock`，含保留給 P2-3 的 `outcome` 欄位）。
  `[proactive]` 新表（`enabled=false` deny-by-default / `base_threshold` / `max_per_hour`
  頻率上限）。MetaCognition base 校準以「可注入 base + `metacognition_base()` mapping + 掛鉤點」
  最小接入。12 個單測。
- **OS-native agent P2-3：四象限成效追蹤 + 校準回饋**：新增
  `duduclaw-gateway/src/proactive_feedback.rs`——把 P2-2 保留的 `proactive_gate.jsonl`
  `outcome=null` 回填成 ProactiveAgent（arXiv:2410.12361）四象限：`correct_detection` /
  `false_alarm`（allow 決策 + 顯式 dismiss 或 session 活動判定，取自既有 `feedback.jsonl` /
  session 資料，訊號缺失一律 `unknown` 不腦補）、`missed_need` / `correct_silence`（suppress
  決策 + 使用者是否於窗內對同 agent 發起事件關鍵詞相關請求，CJK-safe `word_contains_ci`）。
  每 60 秒背景迴圈掃描（只對尾部 500 行嘗試訊號蒐集，冪等回填），`quadrant_stats()` 聚合 + 新
  evolution event（`proactive_quadrant`）。**校準回饋**：False-Alarm / Missed-Need 率 EMA
  平滑後透過既有 `metacognition_base()` mapping 換算，`published_base` 每 UTC 日曆天最多移動
  ±1，存 `<home>/proactive_calibration.json`；`autopilot_engine.rs::action_proactive_notify`
  改讀 `effective_proactive_config()` 疊加校準值（`proactive_gate::read_proactive_config` 本身
  零改動，12 個既有測試零回歸）。25 個新單測，`cargo test -p duduclaw-gateway --lib -- proactive`
  全綠（37 個）。
- **OS-native agent P2-4：結構化感知源**：`duduclaw-os` 新增三個唯讀 shell-out 模組——
  `frontmost.rs`（macOS `osascript`/System Events 取前景 app + 視窗標題，Linux
  `xdotool` 取視窗標題，Windows 不支援）、`spotlight.rs`（`mdfind` 包裝，query 一律走
  argv 陣列不經 shell、上限 20/200 筆、scope 目錄需存在）、`calendar.rs`（`osascript -l
  JavaScript` 唯讀取今日行事曆事件，JXA 腳本為固定字面量無注入面）。三者皆
  CJK-safe 截斷（`truncate_chars`），TCC 拒絕從 stderr 分類為 `PermissionDenied`。
  Gateway 新增 `os_frontmost.rs`：per-agent `[os_watch] frontmost_poll_secs`
  （opt-in，0/缺省=不輪詢）低頻輪詢前景視窗，僅在 app/標題實際變化時發出新
  `AutopilotEvent::OsFrontmostEvent`（`os_frontmost` 觸發器），是純感知訊號、
  不另開 idle 判斷源。MCP 新增三個唯讀工具 `os_frontmost` / `os_spotlight_search`
  / `os_calendar_today`（`Scope::OsNative` + `[capabilities] os_native` 閘控，同
  P1 工具的 dispatch 主閘，無 ActionGuard）。`duduclaw os doctor` 增加 System
  Events 自動化權限、行事曆權限（皆為活體試呼叫）、`mdfind` 可用性三項檢查，
  缺權限時輸出系統設定路徑指引，不嘗試繞過。

### Fixed
- **空回覆靜默斷鏈（經銷商回報，Grok 幾乎必現）**：非 Claude runtime（Grok CLI /
  OpenAI-compat（xAI）/ Codex / Gemini / Antigravity）回傳空內容時，先前被當作
  「成功」——failover 記成健康、通道端（Telegram/WebChat 等全通道）直接跳過空訊息
  不發送，使用者看到的就是「已讀不回」；且空的 assistant turn 被寫入 session 歷史，
  下一輪帶著空 turn 送回上游，模型學著繼續回空——session 鏈自此斷裂。修正四層：
  ① `failover.rs` 將「Ok 但空內容」視為失敗（觸發 fallback runtime，雙雙落空則回
  可分類的 `Empty response` 錯誤）；② OpenAI-compat runtime 空 `content` 改回 Err，
  診斷附 `finish_reason` 與 `reasoning_content` 長度（Grok/DeepSeek 推理模型把
  8192 max_tokens 全燒在思考、正文為空即 `finish_reason=length`），且組歷史時過濾
  空 turn（已污染的 session 自動復原）；③ 四個 CLI runtime（grok/codex/gemini/agy）
  exit 0 但 stdout 為空改回 Err 並附 stderr tail；④ `channel_reply` 匯流點最後防線：
  空回覆一律轉入 fallback 鏈，使用者收到分類後的「空回應」錯誤訊息，事件寫入
  `channel_failures.jsonl`，不再無聲失蹤。
- **failover 靜默頂替無可觀測性（同一經銷商實測：容器內無 `grok` CLI，`GrokRuntime`
  未註冊，failover 靜默頂替成 Claude 回答，使用者以為在跟 Grok 對話）**：
  ① `failover.rs::execute_with_failover` 的 `registry.get(primary)` 回 `None`（CLI
  未安裝／偵測失敗）先前完全無聲直接落到 fallback；現在會 `warn!` 並呼叫
  `record_failover()` 記入 `duduclaw_failover_total` 指標，且以 `reason` 欄位
  區分「未註冊」（`not_registered`）與既有的「執行失敗」（`execution_failed`）
  「空回應」（`empty_response`）三種 fallback 成因；fallback 端同樣未註冊時也補
  `warn!`。② `channel_reply.rs` 非 Claude 分支改用 `run_agent_prompt`（保留完整
  `RuntimeResponse`，不再用丟棄 metadata 的 `run_agent_prompt_text`）：當實際回答
  的 `runtime_name` 與 agent 設定的 provider 不一致時，記一行
  `channel_failures.jsonl`（`event: "runtime_fallback_substitution"`，含
  requested/actual/agent/session）＋ `warn!`，並在有 `on_progress` 時發
  `ProgressEvent::ModelInfo { model: "<實際模型>（備援）" }`，讓 WebChat 顯示
  「備援」而非讓使用者誤以為在跟原設定的模型對話；provider 一致時行為不變。

### Security
- **感知輸入安全（OS 原生 Agent P2-5，indirect prompt injection 防護）**：所有 OS 感知取得的
  文字（檔名/路徑/通知文字，未來的視窗標題/行事曆/搜尋結果）一律降格為不可信 DATA。新增
  `duduclaw-security::perception::sanitize_perception_text`（純函式）：CJK-safe 截斷 → 剝控制
  字元/ANSI/零寬 → 複用既有 `input_guard` 規則引擎＋新增「檔名即攻擊面」規則類
  （`filename_role_marker` 抓 `<system>`/ChatML/`[INST]`、`filename_tool_call` 抓 tool-call 樣式
  JSON）→ **中和不阻擋**：角括號 defang、標記 `suspicious`＋附 warning，fail-closed（全被剝光
  → placeholder，絕不回原文，IPIGuard 2508.15310 / Firewalls 2510.05244 精神）。接線三處：
  ① autopilot `os_file` 事件的 `path`/`file_name` 進 delegate/notify/run_skill prompt 前清洗並前置
  安全 banner——**規則匹配走原文、進 prompt 走清洗後文字，兩者分離**；② `os_notify` MCP 的
  `title`/`body` 在 dispatch 層過同一清洗（防污染 agent 用通知社工使用者），仍發送中和後文字；
  命中一律寫 `security_audit.jsonl`（warning 級、不阻擋事件）。正常中英文/CJK 檔名零誤殺（測試涵蓋）。
- **Sensitivity label + context-collapse 防護（OS 原生 Agent P3-2，最小版）**：防止 agent 把某位
  使用者的個人 context 縫進其他人看得到的群聊 prompt（Local-Is-Not-Sufficient arXiv:2606.10173
  §5.1 六風險之 **context collapse**）。新增 `duduclaw_core::Sensitivity`（`Public < Internal <
  Personal < Restricted`，serde 小寫）＋感知源常數表（os_file/spotlight=Internal、frontmost/
  calendar=Personal、clipboard/screen=Restricted、未知源 fail-closed=Personal）。新增純函式
  `duduclaw_core::is_private_session(session_id, user_id)`——**fail-closed** 判定 1:1 私聊 vs
  群組/共享 session（`slack:group:`/`discord:thread:`/telegram 負數 chat id 為群組標記；否則
  「群組 id 與 user id 不同構」，`rest == user_id` 才判私聊；discord/feishu/gchat/teams 無法證明私聊
  → 當群聊）。`channel_reply` 每輪算一次 `is_private`：`## Key Facts About This User` 與
  `## About This User` 兩個 persona 塊在群聊 session **完全不注入**（並 `debug!` 記剝除）；wiki 注入
  `ranked_wiki_injection` 收 `allow_personal`，`.scope.toml` 標 `sensitivity = "personal"/"restricted"`
  的 namespace 在群聊剝除整個 namespace 的頁（仍可搜尋），私聊/群聊各自 cache key 不互污。
  Memory 面 additive：`duduclaw_memory::sensitivity::{stamp_metadata, read_from_metadata}` 把分級
  存進既有 `metadata` JSON blob（不動 schema，同 origin binding 慣例；讀取預設 Internal 保舊資料相容）。
  ProactiveGate 提供 `persona_lines_for_destination` 純helper（目的地非私聊回空 persona）。`.scope.toml`
  `sensitivity` 沿用 `knowledge_owner` 同一表與解析慣例，malformed → fail-safe。26 個新單測（core 17／
  memory 5／gateway 4，含各通道私聊判定、感知表、malformed fail-safe）＋既有 channel_reply/proactive_gate/
  ranked_wiki 測試零回歸。**未竟**：剪貼簿/螢幕感知源本身、感知→memory 寫入打標（P4-4）、per-user
  加密、ProactiveGate 目的地隱私接線——本輪只做 context-collapse 一條最小閉環。

## [1.41.0] - 2026-07-22 — 信任記憶強化、OS 原生整合 Phase 1 與 WebChat model 顯示根治

### Added
- **OS-native agent Phase 1**：新增 `duduclaw-os` crate——跨平台檔案系統監看
  （notify）、原生桌面通知、開啟目標 helper。Gateway `os_events.rs` 將 per-agent
  `[os_watch]` 檔案事件串入 autopilot bus（新 `os_file` 觸發器 + stats 檔），由
  opt-in `[capabilities] os_native` 閘控（預設關閉；未開啟時 `os_notify` /
  `os_watch_status` / `os_open` MCP 工具在 dispatch 主閘直接拒絕）。CLI 新增
  `duduclaw os` 子指令（通知 helper、doctor 診斷）；dashboard agent 表單新增 os_native
  開關 + `[os_watch]` 編輯器（i18n en/ja/zh-TW），設定變更透過既有 `agents.update`
  RPC 即時熱重載對應 watcher，不需重啟 gateway。另外 Settings › Automation 分頁
  （既有的 goal-loop / dispatch / topology 等自動化設定頁，與 os_native 無關）本次
  透過 `system.update_config` 加上熱重載能力。

- **記憶寫入來源綁定（TMA-NM，arXiv:2606.24322）**：新增 `duduclaw-memory` origin 分類表
  （`origin.rs`，8 類 + trust 天花板），`store_temporal` 強制 non-malleable 上限——
  最終 `origin_trust = min(呼叫端值, 該來源類別天花板, derived_from 最小值)`，呼叫端
  只能調低不能超標；未標註來源一律落 `unattributed`（0.6），不再預設滿信任。
  reaffirm 佐證改為 Sybil-resistant：僅 ≥2 個相異且非自我衍生的來源類別可小幅上調
  confidence（+0.1/次，cap 1.0），agent 自我摘要與工具回聲互相佐證不再加分。
  全部 production 寫入路徑（MCP `memory_store`、`log_mood`、GVU StoreEpisodic、
  reflexion 整併、decision capture、night engine、批次匯入）補上顯式 origin。
- **階段性可撤銷 capability（PORTICO，arXiv:2606.22504）**：`agent.toml [capabilities]
  scoped_tools` 清單中的工具改為「持有效 grant 才可用」。新增 `capability_grants.rs`
  grant store（`approvals.db`，fail-closed）、MCP 工具 `capability_request`（走
  ApprovalBroker 人工核准，TTL 預設 1h 可由 `grant_ttl_secs` 覆寫）、goal kickoff
  核准時可依 task tags `grant:<tool>` 原子性授予。任務終態（accept / reject /
  needs_human / cancel / escalate）自動撤銷該 task 的全部 grant——授權活不過任務階段。
  MCP dispatch 主閘 + CLI spawn disallowedTools 雙層強制執行。
- **eval trace-grounding 斷言（GroundEval，arXiv:2606.22737）**：`duduclaw eval` 新增
  `[[expect.grounded]]` 確定性斷言——要求指定工具有 ≥1 次非錯誤呼叫，且最終回答與
  該工具的 result 共享 ≥N 字元連續片段（CJK-safe），可選 `output_regex` 要求命中片段
  出現在工具結果中。transcript parser 補上 `tool_use`↔`tool_result` 配對（舊 transcript
  相容）。失敗歸類 MAST FM-3.3（驗證缺失）。
- **MAV 判官證據區塊**：goal task 驗收判官 prompt 附上 `<tool_activity>`（自
  `tool_calls.jsonl` 審計取 claim→review 時間窗的工具活動摘要）；correctness 面向指令
  明確要求「worker 聲稱的動作未出現在 tool_activity 視為未證實」，堵住自報 result_summary
  無法查證的盲點。

### Changed
- **規則整併加 GovMem 晉升門（arXiv:2607.02579）**：reflexion 整併前按 `source_kind`
  分組獨立計數（RFC-24 決策缺失與一般任務失敗不再互相湊數），且需 ≥2 個相異 session
  與 ≥2 種相異錯誤描述才視為獨立佐證充分——同一 session 連續重複的相關觀測不再被
  當成獨立證據觸發整併。`mistakes` 表新增 `source_kind` 欄（idempotent migration）。
- **新規則試用期（Janus，arXiv:2606.31121）**：整併規則 seed 由 helpful=2 降為 1 並帶
  `probation-rule` 標籤；試用期內首次 harmful 即退休、累計 helpful≥3 轉正；注入排序
  同分時試用規則排後。壞規則從「需扣兩次才退場」變為「一次即退場」。
- **cache-aware 壓縮守門（arXiv:2607.12161 實證：激進壓縮破壞 prompt cache 反而更貴）**：
  `maybe_compress_history` 在 agent 近 1h cache 效率 >50% 且預算超標 <15% 時跳過壓縮
  管線（門檻可由 `[budget] cache_guard_min_eff` / `cache_guard_max_overshoot` 覆寫，
  0 停用）。`token_usage` 表新增 `compressed` / `compression_stages` 欄；新增
  Prometheus 計數器（壓縮次數 per stage、守門跳過、疑似 cache-break）；
  `cache_attribution_snapshot()` 接上消費者（每小時 adaptive routing check 記錄
  top-10 破 cache 原因 + evolution event）。

### Fixed
- **WebChat model 顯示與 Agent 設定不一致（經銷商回報）**：四項根治——
  ① `agents.update` 後的 registry re-scan 從 500ms best-effort 改為保證完成
  （無條件取寫鎖 + 3 次重試；gateway 無週期性 rescan，漏掉一次就永久顯示舊
  model 直到重啟）；② 設定變更即時廣播（`agent_config_events`）至活躍 WebChat
  連線，重發帶 `refresh: true` 的 `session_info`——開著的分頁立即更新名稱/圖示/
  model，且前端只更新 agent metadata、不動 session 狀態；③ 新增 dashboard-only
  `ProgressEvent::ModelInfo`：解析 stream-json `message.model`（反映 CLI 端替換），
  WebChat 蓋章到 `assistant_done.model`，UI 以實際值優先於設定意圖，文字頻道比照
  Step 忽略；④ 無 agent 時的 hardcoded fallback `claude-sonnet-4-20250514` 統一為
  `DEFAULT_PREFERRED_MODEL`（`claude-sonnet-4-6`，與 scaffold 預設一致），
  `session_info` 同步顯示該值而非空白。

### Security
- 記憶投毒防護強化：見 Added 的來源綁定（寫入端）與 GovMem 晉升門（整併端）。
  scoped_tools 授權閘全程 fail-closed（grant store 不可讀 = 無授權）。

## [1.40.0] - 2026-07-21 — 經銷商實測修復：遠端存取白名單、PTY 韌性與 WebChat 對話隔離

### Fixed
- **中文輸入法（注音/拼音）打字時 Enter 誤送半截訊息 → 組字期間的 Enter 不觸發送出。**
  IME 組字時第一次 Enter 是「選字/確認」卻被當成送出，訊息只送一半。新增共用工具
  `web/src/lib/keyboard.ts` 的 `isImeComposing(e)`（同時檢查 `nativeEvent.isComposing`
  與 Safari 的 `keyCode === 229` 邊緣行為），所有「Enter 送出/確認」的自由文字輸入框
  都改為 `e.key === 'Enter' && !isImeComposing(e)` 才動作。掃同類共修 14 個檔案 15 處：
  WebChat 輸入框、workspace PromptBar、標籤 ChipEditor、InlineEditor 改名、CommandPalette
  篩選、Skill 市集搜尋＋repo URL、共享 Wiki 搜尋、Identity 解析、MCP fetch、部門建立、
  Knowledge 策展查詢×2、Knowledge Hub 搜尋、系統設定遠端存取白名單、CLI 登入回應。
  純按鈕啟用（Enter/Space 當點擊，如 Logs/Onboard/Mascot）與全域快捷鍵監聽
  （CommandPalette/ConnectorChips/AgentModelPicker）、以及會排除可編輯目標的 InboxList
  導覽鍵不涉及 IME，未動。
- **WebChat「新對話」完成後不會即時出現在對話列表、切回舊對話就回不去 → 每則回覆入庫後刷新列表。**
  承接上一版 conv-nonce 架構：新對話 B 的 session bucket（`…#conv:<nonce>`）在伺服器端
  存在且可 resume，但左側列表沒刷新、B 進不了列表就無法點回。修法：store 新增
  `sessionsRevision` 計數器，每收到一則 `assistant_done`（含被歸屬守衛丟棄的其他對話）
  即 +1；WebChat 頁面 watch 此值刷新 `chat.sessions.list`，讓剛建立的對話在第一則回覆
  落地時就進列表並保持可 resume。已知殘留（cosmetic）：目前開啟中的新對話因 client 端
  `sessionId` 仍為連線基底 id、列表列為 composed id，列表「使用中」高亮不會標到它——
  不影響點選 resume 與續聊。
- **WebChat「新對話」回覆錯投到新對話 → 每個對話有獨立 session bucket＋回覆按對話歸屬。**
  修復在對話 A 發長任務、任務進行中按「新對話」開對話 B 時，A 的回覆完成後出現在 B
  裡的問題。根因：WebChat 的 server session 綁「WS 連線 id」而非「對話」，且 `/new`
  是 `delete_session` 原地刪除、不輪替新 id——A 與 B 共用同一個 session bucket，
  in-flight 回覆完成時就投到目前打開的對話。修法：
  - **前端每個對話帶一個 conversation nonce（`conv`）。** `user_message` 帶上 `conv`，
    在 `/new`、切換 AI 員工、resume 歷史對話時輪替。回覆 frame（`assistant_done` /
    `progress` / `step`）由伺服器原樣回帶 `conv`；socket 收到與目前對話 `conv` 不符的
    frame 直接丟棄不渲染（typing/任務板進度/工具步驟同樣受此閘門保護，不會錯投）。
  - **伺服器端每個對話獨立 session bucket。** new/continue 路徑把消毒過的 `conv`
    併入 session id（`…#conv:<nonce>`），A 的 in-flight 回覆持久化到 A 的歷史、
    不再混進剛開的 B。`sanitize_conv_nonce` 只保留 `[A-Za-z0-9_-]`、限長 64、
    去除 `:`/`#` 結構分隔符（防注入額外 bucket 結構）；resume-ownership 守衛
    （`starts_with("{session_id}#")`）仍接受這些 id。缺 `conv`（舊 client）→
    byte-compatible 單 bucket 舊行為。
  - **「新對話」按鈕改為輪替 nonce、不再送 `/new`。** 舊行為會 `delete_session`
    刪掉可能仍在跑長任務的對話；新行為改為開一個空的新 bucket（AI 自然從乾淨脈絡
    開始），並保留舊對話在「對話列表」可 resume。連帶讓 WebChat 的過往對話列表
    真正可用（先前所有對話共用一個 id，列表形同只有一筆）。`/new` 指令本身不變，
    其他頻道與手動輸入 `/new` 照舊。
- **PTY pool 互動 REPL 卡住抓不到回覆 → 快速失敗並降級。** 修復 `[runtime]
  pty_pool_enabled = true` + OAuth 訂閱帳號時，channel 回覆走互動 REPL 卻抓不到
  sentinel 回覆、一直轉到 30 分鐘才逾時的問題群：
  - **互動 REPL 逾時改為「停滯偵測（stall detection）＋寬鬆硬上限」**（取代原本
    固定 180s deadline，見下方 Changed）。新增可設定的 idle/停滯視窗
    （`agent.toml [runtime] pty_idle_timeout_secs`，或 env
    `DUDUCLAW_PTY_IDLE_TIMEOUT_SECS`，缺省 **120s**）：互動 REPL 只在**連續無實質
    進度**達 idle 視窗才快速失敗降級，長任務（多分鐘工具呼叫／agentic 工作）不再被
    誤殺。「實質進度」以 **token 計數上升 or 去噪 prose 內容變化**判定（spinner 動畫
    ＋每秒跳動的經過計時器**不算**進度——依 Claude Code 2.1.173 活體擷取校準）。
    `pty_interactive_timeout_secs` 改為**絕對硬上限安全網**（缺省 **1800s**）。
    API-key 的 `claude -p` one-shot 路徑維持原本的長 deadline。
  - **fallback 紀錄新增 `reason` ＋ `mid_task` 欄位。** `channel_failures.jsonl` 的
    `pty_pool_fallback` 事件標明失敗原因（`stall`／`hard_cap`／`boot`／`other`）；
    停滯或硬上限發生在**已觀察到進度之後**（任務執行中段）時 `mid_task=true`，並額外
    warn「task may have partially executed」（fallback 重跑可能重複副作用，仍以可用性
    優先照樣降級）。`InvokeStall`／`InvokeHardCap` 兩個新錯誤型別可讓上層分類。
  - **多行 prompt 送不出去 → 停滯（submit watchdog）。** 多行 prompt 以
    bracketed-paste 寫入後緊接的 `\r`，實測對真 claude 2.1.173 有約 3/4 機率**不會觸發
    送出**（TUI 還在吃 paste，`\r` 被丟棄），prompt 躺在輸入框、REPL 空等到停滯逾時。
    `collect_response_interactive` 新增 submit watchdog：送出後若 1.5s 內未見「turn
    正在跑」的跡象（spinner／`esc to interrupt`／sentinel），重送 `\r`，有界 3 次、
    間隔遞增。活體驗證：watchdog 送出成功率 4/4（原 1/4）。
  - **首回合 TUI welcome box 被當成回覆送出（fail-closed 過濾）。** fresh session
    首回合的整屏重繪會把「Welcome back／What's new／release-notes／org email／agent
    路徑」的 welcome box 夾進兩個 sentinel 之間，舊的 chrome filter 不認得而把它當答案
    送給使用者。改法：payload 逐行過濾時，**任何 box-drawing／block 字元
    （U+2500–U+259F）或 welcome 關鍵字（Welcome back／What's new／release-notes…）
    的行一律視為 chrome**；前導 welcome chrome 略過（後面真答案保留），welcome 出現在
    答案之後則停止收集。全被濾掉時回空 payload → 觸發既有 empty-payload retry／
    fallback（寧可空也不送 chrome）。fixture 取自 2.1.173 活體擷取。另補 composer
    輸入框狀態列（`ctrl+g to edit in Vim`、`⏵⏵ automode on (shift+tab to cycle)`、
    `← for agents`、`? for shortcuts` 等）——這些會黏在答案末尾漏出；改用**整行去空白
    後精確相等／符號前綴**比對（非子字串），確保「答案內文真的在講 vim 快捷鍵」不被誤殺。
  - **互動 REPL 失敗自動 fallback 到 fresh-spawn `claude -p`。**
    `call_claude_cli_pty_rotated` 在 pool 路徑回可復原錯誤（逾時／空 payload／boot
    失敗／帳號耗盡）時，改走與 `FreshSpawn` 同源的 `call_claude_cli_rotated`，並記
    warn log ＋寫一筆 `pty_pool_fallback` 到 `channel_failures.jsonl`（不再靜默失敗）。
    MoA 設定錯誤不 fallback（fresh-spawn 同樣會拒）。
  - **Boot dance 補指紋並改為快速失敗。** 互動 boot 除 trust 對話框外，新增
    theme picker／onboarding／login-method 首次啟動畫面的偵測（去 ANSI ＋去空白
    ＋小寫比對，送 `\r` 接受預設值）；boot 結束仍未見 REPL-ready 指紋即把 session
    標記不健康並回 `BootTimeout`（取代舊的硬 proceed），讓上層走上述 fallback，把
    「卡 30 分鐘」變成「數秒失敗＋降級可用」。
  - **REPL-ready 指紋更新至 Claude Code 2.1.x ＋ MCP 核准畫面處理**（活體驗證
    2.1.173）。舊指紋（`? for shortcuts`／`Try "edit`）在 2.1.x TUI 已不存在，
    導致 REPL 明明就緒仍判 boot 逾時；補上 `Try "how does`／`shift+tab to cycle`。
    另新增「New MCP server found in this project」核准畫面的顯式指紋，`\r` 接受
    預設選項（Use this MCP server）。
  - **補 dispatcher 路徑的 per-account 憑證注入（Gap A）。** sub-agent dispatch
    走 PTY pool 時，`AcquireOptions` 先前沒帶 `account_id`／`env`，會用到 ambient
    OAuth；改為比照 channel 路徑用 `rotate_cli_spawn` 逐帳號注入 env ＋ account_id。
  - **keychain 預設 OAuth 帳號給穩定 account_id（Gap B）。** rotator 對預設 keychain
    帳號只發空字串 `ANTHROPIC_API_KEY` force-OAuth sentinel；先前 `account_id` 解析回
    None 導致 env 不被 stash／注入，PTY child 可能繼承 gateway 殘留的 API key 而蓋掉
    OAuth。改為給它 `oauth-keychain-default` 穩定 id，使 sentinel env 被注入。

### Changed
- **PTY pool 重新定位為「備援、預設關」並補上已知限制說明。** Anthropic 原訂 2026-06-15
  把程式化用量（`claude -p` / Agent SDK / GitHub Actions）拆到獨立 Agent SDK credit，
  但**已於當天暫停**，`claude -p` 對 OAuth 訂閱帳號照舊可用 → 預設的 `FreshSpawn` 路徑
  完整可用、PTY pool 非必要。功能維持保留、預設關（`pty_pool_enabled` 缺省 false，
  `runtime_mode_for_agent` fail-safe 回 `FreshSpawn`），文件（`docs/features/27-pty-pool-runtime.md`
  含 zh-TW/ja-JP、`CLAUDE.md`）新增「何時才需要開」與**已知限制**：pool session 以
  `(agent, cli_kind, bare_mode, account, model)` 為 key、**不含對話維度**，多對話 agent
  會跨對話共用同一條 REPL 而洩漏脈絡——開啟前必須理解此行為。**預設 fresh-spawn `claude -p`
  路徑不受影響**：其脈絡完全來自 `get_messages(session_id)`、session id 逐對話
  （WebChat 含 `#conv:<nonce>`），已驗證無跨對話洩漏；`--resume` 確定性 session id 路徑
  早已移除（所有呼叫點傳 `None`）。附帶把 WebChat 的 session-id 組合抽成純函式
  `compose_session_id` 並加單元測試鎖住「conv nonce 參與分桶」不變式。
- **`agent.toml [runtime] pty_interactive_timeout_secs` 語意變更：從「固定殺 turn 的
  invoke deadline」改為「絕對硬上限安全網」，缺省值 180s → **1800s**（對齊 fresh-spawn
  的 `HARD_MAX_TIMEOUT`）。** 日常「session 是否卡住」的判定改由新的停滯偵測負責（新設
  定 `pty_idle_timeout_secs`，缺省 120s，見 Fixed）。**影響**：先前靠此值在 180s 主動
  殺掉長任務的使用者，現在長任務會一直跑到 30 分鐘硬上限或先被停滯偵測攔下——若要回到
  舊的積極上限，自行把 `pty_interactive_timeout_secs` 設回 180。managed-worker 的
  per-invoke 硬上限 clamp 也從 10 分鐘提高到 31 分鐘以容納新的硬上限；`InvokeParams`
  新增 `idle_timeout_ms`（向後相容，舊 client 省略時只套硬上限）。

### Added
- **Dashboard 即時連線的可設定 Origin 白名單。** 新增 config.toml
  `[gateway] allowed_origins`（陣列，元素可為 `host`、`host:port` 或含 scheme 的
  完整 origin）與環境變數 `DUDUCLAW_ALLOWED_ORIGINS`（逗號分隔，兩者**合併**）。
  解決經銷商／使用者透過 tailnet（`*.ts.net`）或反向代理網域開 dashboard 時，HTTP
  頁面正常但 WebSocket 升級被 403 擋掉一直轉圈圈的問題。內建 loopback 三項
  （`localhost` / `127.0.0.1` / `[::1]`）永遠有效；清單為空時行為與舊版
  byte-identical（零回歸、fail-closed）。每個項目做精確 authority 比對，不支援
  萬用字元，後綴攻擊（`localhost.evil.com`）仍被擋。啟動時印一行 info log 列出
  生效的額外 origins。文件見 `docs/guides/deployment-guide.md` §5 與
  `docs/guides/docker.md` §13。
- **Dashboard 設定頁可直接管理 Origin 白名單。** 設定 → 系統 → 遠端存取網址提供
  新增／刪除 chip 介面（`system.config` / `system.update_config` RPC），存檔後
  透過 `set_allowed_origins` **熱生效、免重啟** gateway；`DUDUCLAW_ALLOWED_ORIGINS`
  環境變數提供的項目在 UI 存檔時會重新併入、不被洗掉。經銷商／使用者不必再手改
  config.toml。

## [1.39.0] - 2026-07-20 — Graph Engineering — 雙時間軸記憶、投毒防護、知識圖策展

### Added
- **記憶系統升級為雙時間軸（bi-temporal）＋建構期溯源（D1）。** 每筆事實新增
  `ingested_at`（交易時間軸：系統何時得知，與「事實何時在真實世界成立」的
  `valid_from` 分離）；supersession 發生時在被取代的舊列記錄
  `invalidated_by_event`／`invalidated_at`（是哪則來源事件、何時把它失效）。
  同一 `(subject, predicate)` 且 object 與內容實質相同的事實重複出現時不再新增列，
  改在既有列的 metadata 記 `reaffirmed_by`（上限 20 筆）並累加 access_count，
  避免記憶膨脹。
- **新增 `invalidate_by_origin` 按來源回滾原語**（engine + MCP `memory_invalidate_by_origin`）：
  一次 expire（非刪除）某來源（精確相等比對，非子字串）自某時刻起的全部
  currently-valid 事實，並沿 `derived_from` 級聯把衍生事實的信任度降到 ≤ 0.1。
  history 完整保留、仍可查。這是偵測到來源投毒後的止血閥（Admin scope）。
- **新增 MCP 工具 `memory_get_history` / `memory_get_at`**：查詢某三元組
  `(subject, predicate)` 的完整 supersession 鏈與任一時點的有效事實（原本 engine-only）。
- **知識寫入投毒防護管線（D2，對應 PoisonedRAG 2402.07867）。** 自動蒸餾寫入前，
  對每筆事實的內容與 `(subject, predicate, object)` 跑既有的注入規則引擎；命中即
  「不寫入」（fail-closed）並記 `prompt_injection` 稽核事件。新增同源突增偵測
  `knowledge_guard`（沿用 dispatch_guard 的滑窗＋跨行程 advisory lock 模式）：同一
  `(agent, origin, subject)` 在窗內寫入 ≥ `max_per_subject` 筆即把該批標為隔離。
  `config.toml [knowledge_guard]`（`enabled`／`window_secs`／`max_per_subject`，缺省
  一律退回內建預設）。
- **記憶列新增 `quarantined` 欄（idempotent migration）。** 隔離中的事實一律
  inert——不 supersede 任何現有事實，且被所有檢索讀路徑排除（FTS、graph、vector、
  `search`／`search_layer`／`list_recent`／`summarize`／`list_valid_by_source_event`）。
  依 id 明確取用的 `get_by_id`／`get_by_ids` 與溯源檢視 `get_history`／`get_at` 仍可回傳。
- **隔離處置走 ApprovalBroker**（`action_kind = "knowledge_quarantine"`）＋發
  `knowledge.quarantined` 事件到 events.db。核准 → 解除隔離（`quarantined = 0`，恢復可
  檢索）；拒絕 → expire（`invalidated_by_event = "quarantine_reject"`）並把 `origin_trust`
  降到 ≤ 0.1；TTL 過期 = 拒絕（fail-closed）。dashboard `approvals.decide` 已接上此側效。
- **記憶知識圖檢索演進（D3，對應 HippoRAG 2 / LightRAG）。** ① 每 agent 的 SPO 圖
  改為持久快取（generation counter 失效，>500 條三元組才啟用；快取命中的排序與
  現建 byte-identical，位元級測試驗證）；② 實體別名歸併：新 `entity_alias` 表把
  「老闆／李老闆」等表面形式收斂到同一節點，提升建圖與播種命中率，新增 MCP 工具
  `memory_alias_add`（write scope）／`memory_alias_list`（read scope）；③ 述語（predicate）
  以邊標籤附掛入圖（PPR 分數不變），並新增 `engine.export_graph(agent, limit)` 可序列化
  快照（含隔離標記）供策展 UI 使用；④ 可選 embedding 播種（`[memory] graph_embed_seed`，
  預設關閉）：啟用且掛 embedder 時以 query 向量對實體向量取 top-k 聯集擴充 FTS 播種，
  關閉或無 embedder 時 byte-identical。
- **Goal loop 平行派工 DAG（D4，LLMCompiler 式）。** 可選 planner
  （`[goal_loop] planner_enabled`，預設關閉）把 goal 拆為帶依賴標注的子任務 DAG；
  依賴全部完成的子任務平行派發（仍受 `max_concurrent` 與 dispatch_guard 約束）。
  循環／無效計畫整包拒絕、退回單任務模式；上游依賴失敗或升級人工時，下游任務
  繼承升級為 needs_human（不孤兒化）。
- **可插拔派工策略（D4）。** `config.toml [dispatch] policy`：`fixed_hierarchy`（預設，
  行為與先前一致）／`round_robin`（per task-class 輪詢）／`llm_select`（utility LLM 選人，
  輸出不在 roster 或解析失敗一律 fail-closed 回 fixed_hierarchy；不硬編碼模型）。
- **動態判官深度（D4，MaAS 式）。** 零 LLM 的本地難度啟發式將 goal 分為 Simple／
  Complex：Simple 用兩面向判官（correctness + safety，省 completeness 成本）並套
  `[goal_loop] iteration_cap_simple`（預設 3）；Complex 維持三面向 MAV panel 與既有
  iteration_cap（預設 8）。**safety 面向在任何深度都保留**，synthesis 仍是全過才
  accept、缺面向 fail-closed。
- **半自動拓撲演化（D5，GPTSwarm edge-optimization 的 human-gated 版，預設關閉）。**
  背景驅動器（`[topology_evolution] enabled`，預設 false）聚合 per (agent, task_class)
  的拒絕率／needs_human／oscillation 證據，對持續低落的路由產生「改派給 sibling」
  提案——每個提案永遠經 ApprovalBroker 人工核准（無 LLM judge 裁量、不受
  autonomy_level 放寬、TTL 過期＝拒絕）；核准後寫入 `routing_overrides.json`
  （advisory lock＋原子替換，損毀 fail-safe 視為無 override），預設派工層命中即改派；
  24h 觀察期內新 agent 未優於基準即自動回滾。防提案風暴：同一 (task_class, from_agent)
  7 天至多一案。新增 `topology.list` dashboard RPC 與 `topology.*` 事件；詳見
  docs/guides/topology-evolution.md。
- **知識圖策展台（D6）。** 知識庫新增「策展台」分頁：SPO 知識圖譜可視化（d3 力導向、
  來源可信度分層配色、點邊看溯源側欄）、事實歷史時間線（supersession 鏈、現任高亮）、
  待審知識佇列（核准／拒絕／清除此來源三鍵，清除來源有確認框）。新增 dashboard RPC
  `memory.graph`／`memory.get_at`／`memory.invalidate_origin`（破壞性，僅 dashboard 面），
  `approvals.list` 支援 `action_kind` 過濾。文案面向終端使用者（zh-TW／en／ja 三語齊）。

### Security
- **origin_trust 正式參與檢索排序（D2）。** `RetrievalWeights` 新增 `w_trust`（預設 0.10，
  由 `w_fts` 0.40 → 0.35 讓利，總權重感覺不變）；每筆候選分數乘上
  `(1 - w_trust) + w_trust · origin_trust`，未驗證的 channel 蒸餾事實（trust 0.3）不再能
  蓋過人工 curated 事實（trust 1.0）。graph_rank 的邊權乘上該三元組的 `origin_trust`，
  壓制「單條假 triple 經 PPR 兩跳放大」的攻擊路徑。所有 `origin_trust = 1.0` 的既有資料
  排序與 D2 前 byte-identical（含 graph PPR 位元級一致，已寫測試驗證）。

### Changed
- **`store_temporal` 的 supersession 改為依真實世界時間 `valid_from` 判定，具備亂序韌性。**
  當新事實帶有 `valid_from` 且早於現任事實時，插入為「歷史段」（`valid_until` 設為
  現任的 `valid_from`），不動現任、不建 supersession 鏈——正確處理離婚／結婚等亂序
  ingest 的情境。無 `valid_from` 的寫入完全沿用既有 ingestion 順序行為。
  副作用：以完全相同內容重複 consolidate 使用者側寫時，現在會 reaffirm 既有摘要列
  （id 穩定）而非每次都新建一列。

### Fixed
- **分享全域 Skill 不再報「Skill not found」。** 「我的技能」清單同時列出 agent 自有
  與全域（`~/.duduclaw/skills/`）兩種來源，但 `skills.share` 只到 agent 的 SKILLS
  目錄找檔案，分享全域 skill（如 pptx/docx）必定失敗。現在 share 依同一聯集解析
  （agent 版優先、全域版遞補），並補上 agent_id 與 skill_name 的路徑安全驗證
  （拒絕 traversal 與點開頭名稱，與 `skills.adopt` 既有防護對齊）。
- **dashboard 導覽補齊 v2 步驟文案（三語）。** v2「嘟嘟事務所」導覽的六站
  （對話／收件匣／任務看板／Skill／成長／管理）先前在 zh-TW／en／ja-JP 三語
  皆缺翻譯鍵，導覽走到這些頁會顯示原始鍵名；已全數補齊，並移除十個已無程式
  引用的 v1 導覽殘留鍵。
- **ja-JP 介面補完 113 條未翻譯字串。** 先前以 `[EN]` 前綴標記的殘留
  （voice／proactive／settings.update／sharedWiki／mcp 等區塊）全數翻為日文；
  三語鍵集合完全一致（各 3,422 鍵）。
- **部門名稱驗證拒絕所有 `.` 開頭名稱（fail-closed 補洞）。** `is_valid_department`
  原本只擋 `.`／`..`，導致 `.hidden` 之類的點目錄會被 `departments.list` 列為部門
  （既有測試 `list_skips_invalid_names_fail_closed` 在乾淨 main 上即失敗）。現改為
  一律拒絕點開頭名稱，與該測試註解的原始意圖一致。

## [1.38.1] - 2026-07-19 — Dashboard self-update redirect fix

### Fixed
- **儀表板系統更新不再因 GitHub CDN 換域而失敗。** GitHub 已將 release 資產下載的
  重導向目標從 `objects.githubusercontent.com` 改為
  `release-assets.githubusercontent.com`(Azure blob 簽名網址),`apply_update`
  的 redirect 白名單只認得舊網域,導致下載被自身安全政策擋下、儀表板顯示
  「更新失敗」(audit log:`Download failed: error following redirect`)。白名單
  加入新網域(舊網域保留以防 GitHub 分階段切換);`check_update` 的 API 端點
  政策不受影響、維持原樣。注意:已在跑的舊版 binary 仍帶舊白名單,升級到本版
  需手動安裝一次,之後即可恢復儀表板一鍵更新。

## [1.38.0] - 2026-07-19 — Dashboard auto-save settings, high-risk toggle confirmations, OAuth PTY defaults

### Added
- **員工編輯頁改為即時儲存（2026-07-19）。** `/agents/:id/edit` 移除「儲存」按鈕：
  任何修改後 1 秒自動寫入（單一儲存航班＋尾隨補寫，不併發、不漏改），右上角顯示
  儲存中／已儲存狀態；CONTRACT.toml 區塊併入同一自動儲存循環，離開頁面時盡力補送
  未儲存變更。高風險開關（技能自動啟用、可建立 agent、可改 SOUL、Computer Use、
  瀏覽器 via Bash、網路存取、worktree 自動合併）在開啟時彈出二次確認框，關閉則
  直接生效。
- **偵測 Claude Code CLI OAuth → PTY 連線池預設開啟。** `agents.inspect` 現在回傳
  `agent.toml [runtime]` 實際內容（僅含檔案裡存在的 key，執行環境分頁從 write-only
  變為顯示真實設定值）；當偵測到 Claude OAuth、provider=claude、api_mode=cli/auto
  且 `pty_pool_enabled` 從未設定過時，一次性把 PTY 連線池＋ Worker 子程序託管預設
  為開並寫入 agent.toml（寫入後即為明確值，之後手動關閉不會被翻回）。

### Fixed
- **對話頁員工列不再重複顯示 main agent。** 頂端頭像列的 DuDu 小助理入口本就路由到
  main agent，員工卡片改為過濾 `role == 'main'`；既有選取若指向 main agent 會自動
  正規化回 DuDu 入口，避免「員工只有一位、對話卻出現兩顆頭像」的誤解。

## [1.37.1] - 2026-07-19 — Signed NFR test licenses + distributor self-service

### Added
- **NFR(Not-For-Resale)簽章標記(2026-07-18)。** License 新增簽章內 `nfr`
  布林欄位(向後相容:`false` 時序列化位元組與舊版完全一致,舊簽章照常驗證;
  從 `license.json` 移除標記即簽章失效 → fail-closed 降級 OpenSource)。用途:
  經銷商內部測試授權——完整 Self-Host Pro 功能(含白牌,可測 branded build),
  但授權頁會顯示白牌蓋不掉的「內部測試授權(NFR)— 不得轉售」標記(zh-TW/en/ja),
  轉售的副本一眼可辨。`LicenseSnapshot` 增列 `nfr`;refresh/rebind 均保留標記,
  re-sign 後不會「洗白」。

## [1.37.0] - 2026-07-18 — Goal Loop autonomous agents, custom dashboard widgets, DuDuClaw design system

### Added
- **自主目標迴圈（Goal Loop，2026-07-17）。** 在頻道丟一個目標，AI 員工就自主
  規劃、執行、自我驗收，做到完成或卡住時回來通知你——把「一問一答」升級為
  「給目標自主做完、卡住問你」的成套模式。整套預設關閉，靠 `[dispatch] enabled`
  啟用，不影響現有對話。
  - **驗收驅動終止**：完成訊號只認驗收判官核可（`LlmAcceptanceJudge` 接上帳號
    輪替），不信任 AI 自評「做完了」；未通過帶回饋重試（Generator-Verifier）。
  - **外層迴圈驅動器**（`goal_loop.rs`）：把待辦目標任務派上既有喚醒軌，重試即時
    推進；硬終止守衛（派工上限 / 牆鐘上限 / 並行上限 / 進度震盪偵測）任一踩線即
    轉人工，runaway 不可能。
  - **分級自主**（`AutonomyLevel` 五級 operator→observer，per-agent
    `agent.toml [capabilities] autonomy_level`，預設 Approver）＋ needs_human /
    kickoff 審批往返（Telegram / Discord / Slack / LINE 內嵌按鈕，冪等且
    fail-closed）。
  - **回饋路徑斷路器**（`duduclaw-core::dispatch_guard`）＋ 級聯 hop-depth 防再生型
    無限迴圈，`config.toml [dispatch_guard]` 可調。
  - **`/goal` 使用者入口**：`/goal <目標>`、`/goal <目標> || <驗收標準>`、
    `/goal status`；任務記住來源頻道＋對話（`tasks.source_channel/source_chat_id`
    兩欄，idempotent migration），進度與需人工通知推回發起對話。文件：
    `docs/guides/goal-loop.md`。
- **自訂 Widget 系統（2026-07-16）。** 儀表板 widget 從固定五款內建擴充為可自訂：
  - **沙箱執行環境**：自訂 widget 是單檔 HTML，在 `sandbox="allow-scripts"` 的
    iframe 內執行（無 `allow-same-origin` ⇒ 拿不到 JWT/DOM/localStorage），
    渲染時注入 CSP（禁外部資源與網路外呼）與 SDK shim；資料只能經
    postMessage **唯讀允許清單橋**（`agents.summary`／`tasks.summary`／
    `cost.summary`／`channels.status`／`system.status`，10 req/s 上限，
    繼承當前使用者的角色與資料範圍）。主題跟隨儀表板明暗、高度自動。
  - **AI 產生（一般使用者）**：`/widgets/new` 引導式流程（資料來源＋呈現型態＋
    自由描述）→ `widgets.custom.generate`（走 rotated CLI → Direct API 備援、
    零工具 caps）→ 沙箱即時預覽 → 不滿意可帶回饋「再改一版」→ 儲存才落庫。
  - **HTML 完整客製（管理員/經銷商）**：`/widgets/new?mode=html` 原始 HTML
    編輯＋即時預覽；匯出/匯入 `.json` 讓經銷商跨客戶搬運。
  - **Widget 工坊（`/widgets`）**：「我的／團隊分享」兩 tab，卡片帶 **lazy 縮圖
    預覽**（進 viewport 才掛縮放沙箱、不可互動），一鍵加入儀表板、分享/取消
    分享、複製他人分享、匯出入、刪除（管理員可下架任何分享）。
  - 首頁 layout 以 `custom:<id>` 引用（fail-closed：只放行自己可見的
    widget id），編輯模式抽屜可加入自訂 widget。**view-as** 檢視下屬儀表板時
    自訂 widget 一併渲染（html 隨 `dashboard.layout.view` 內嵌下發——受同一道
    strict-rank 閘保護，下屬私有 widget 不經 `widgets.custom.get` 外洩）。
  - 後端：`custom_widgets.rs` SQLite store（256 KB/widget 上限、擁有權在
    store 層強制）＋ `widgets.custom.list/get/create/update/remove/share/generate`
    七支 RPC。設計文件：`commercial/docs/custom-widgets-design-2026-07-16.md`、
    公開文件：`docs/features/30-custom-widgets.md`。
  - **產生鏈活體驗證過**（claude-sonnet-4-6 真跑兩輪）：第一輪暴露模型會輸出
    說明文字而非 HTML、且橋接資料缺日期欄位——已補 prompt 尾端輸出紀律、
    `extract_html_fragment()` 伺服端剝除環繞散文（純散文＝硬錯誤不落庫）、
    `tasks.summary` 增 `completed_today` 與 `completed_at`；第二輪產出 4.1 KB
    合規 fragment（`<` 開頭、用橋接、用 CSS 變數、零外部資源）。
  - **每人 widget 數量上限（2026-07-17）**：`custom_widgets.rs` 新增
    `max_widgets_per_user()`（預設 20，讀 `DUDUCLAW_MAX_WIDGETS_PER_USER`
    覆寫、`0`＝無限——與 `EditionProfile::personal_max_agents()` 同款慣例），
    在 store 層 `create()` 內強制（非只在 RPC 層），達上限回 zh-TW 錯誤訊息；
    `widgets.custom.list` 回應加 `max_per_user` 供前端顯示，工坊「我的」tab
    標籤在有上限時顯示「我的（{count}/{cap}）」。
  - **橋接層結果快取（2026-07-17）**：`widget-bridge.ts` 對唯讀橋接方法加
    15 秒 TTL 的模組層快取＋in-flight 去重（快取存 promise，同時到達的請求
    共享同一次呼叫；失敗立即從快取移除、不快取錯誤結果）——避免工坊縮圖
    同屏掛出數十個 iframe 時對同一 method 重複打 API；rate limit（每 widget
    10 req/s）維持原邏輯，快取命中一樣計入該 widget 的請求窗口。

- **個人版 Agent 規模策略：軟提示，不設硬上限（2026-07-15 提案、07-16 拍板 B+C）。**
  維持「self-host 永不設限」的開源承諾：個人版**預設無 Agent 數上限**
  （`personal_max_agents()` 預設 `0`=無限）。超過建議規模（3 個）時儀表板 Agent
  頁顯示**溫和升級提示**（可永久關閉、不阻擋任何操作），升級動機交給既有的企業
  能力閘（部門／簽核／多帳號／白牌）。託管部署若需要硬上限可設
  `DUDUCLAW_PERSONAL_MAX_AGENTS`（機制保留：`agents.create`、
  `templates.create_agent` 與 MCP `create_agent` 三個入口都會執行）。企業版不受
  此機制影響，維持依 license tier（含 self-host 豁免與簽章 override）。
- **決策/簽核事項主動推播到通道（Feature C，2026-07-15）。** 安裝簽核申請送出或
  進入下一關時，主動 DM 該關卡的簽核人（依角色＋部門解析：員工→同部門主管；
  主管關已過→管理員）到其**已綁定的通道**（`channel_identities`）。訊息含功能說明
  與安全審查摘要。全通道文字投遞（重用 `channel_sender`），best-effort 非阻斷。
  新模組 `install_notify`（`approvers_for` 有 5 個單元測試）。
- **通道核准/拒絕按鈕（Feature D，2026-07-15；07-16 補全四通道）。** `channel_format`
  新增 `duduclaw:install_approve|deny:{id}` 動作與 Telegram/Discord/Slack/LINE 四通道
  的按鈕 builder。**四通道端到端接通**：通知帶內嵌核准/退回按鈕（Telegram inline
  keyboard、Slack Block Kit、Discord DM components——自動開 bot↔user DM channel、
  LINE quickReply postback），點擊經 `install_notify::decide_from_channel` 把點擊者
  的通道帳號對回儀表板身分、依角色＋部門授權後 `InstallRequestStore::decide`，
  最終核准即伺服端重掃並安裝（`apply_install_request`，通道路徑略過即時 registry
  rescan，下次掃描熱載）。無按鈕通道走文字通知＋儀表板提示。
- **簽核最終結果回報申請人（2026-07-16）。** 申請被退回／核准安裝完成／核准但
  安裝失敗時，主動 DM 申請人的已綁定通道（`install_notify::notify_requester`），
  儀表板與通道兩條決策路徑都會觸發。先前申請人送出後只能自己去儀表板刷新。
- **MCP `create_agent` 補上 Agent 數上限（2026-07-16）。** MCP server 是獨立行程，
  儀表板的 `tier_limit_message` 閘門管不到它——先前任何 agent 可經 MCP 工具無限建
  agent，繞過 P-License 簽章 `max_agents` 配額（及託管部署設定的個人版硬上限）。
  新增 `license_runtime::agent_cap_message_from_disk()`（從磁碟 bootstrap license，
  與 gateway 同規則），`handle_create_agent` 建立前先過閘。
- **簽核按鈕點擊後即拆（2026-07-16）。** 決策落地後按鈕不再留在訊息上：Telegram
  `editMessageText` 改寫原通知並附上結果、Discord 用 interaction type 7
  UPDATE_MESSAGE 清空 components、Slack 走 `response_url` `replace_original`；
  未授權／已被他人處理的點擊仍以短訊回覆、按鈕保留給有權限的人。LINE quickReply
  本身即拋，無需處理。
- **Edition 背景變化也會推播（2026-07-16）。** 60 秒輪詢 edition 的安全網：
  phone-home 降級、CRL 撤銷、寬限期到期等**不經 RPC** 的授權轉換，現在也會廣播
  `system.status_changed` 讓開著的儀表板即時反映（RPC 路徑維持即時 inline 廣播）。
- **Edition 變更即時推播到儀表板（2026-07-16）。** 授權啟用（`license.activate`）
  或夥伴碼兌換（`license.redeem`）成功後，後端廣播 `system.status_changed`（帶新的
  `edition_profile`），開著的儀表板**不需手動重整**即反映 personal→enterprise 的
  切換（前端 system-store 早已訂閱此事件，先前後端從未發送）。`system.status`
  payload 抽成共用 `system_status_payload()` 供 RPC 與廣播共用。活體驗證：無 license
  開機為 personal → activate → 5 秒內收到事件帶 enterprise。

### Fixed
- **通道通知讀錯 LINE token 欄位（2026-07-16）。** `install_notify` 原以
  `{channel}_bot_token` 硬組 config 欄位名，LINE 的欄位其實是
  `line_channel_token`——LINE 簽核通知會靜默跳過。改用與 OTP 投遞相同的
  `token_field()` 對照表（單一事實來源）。同類掃描：cron 通知的全域 token
  fallback（`cron_scheduler::resolve_channel_token`）有一樣的硬組欄位名問題，
  LINE 全域 fallback 一併修正。
- **無按鈕通道漏掉儀表板提示（2026-07-16）。** 提示旗標語意反轉：WhatsApp／飛書等
  無按鈕通道的簽核通知反而**沒有**「請至儀表板核准」提示，收到通知的人無從行動。
- **`apply_install_request` 補齊 fail-closed 驗證（2026-07-16）。** 通道路徑的安裝
  執行漏了儀表板孿生（`execute_approved_install`）既有的 `agent_id`／
  `server_name`／department 識別字驗證（識別字會組檔案路徑）；另外 skill
  frontmatter 的 `name:` 曾直接組 temp 檔名，`name: ../../x` 可逃出 temp 目錄——
  新增 `sanitize_tmp_file_stem()` 並同步修補儀表板路徑 `run_skill_install` 的同類問題。

### Changed
- **儀表板全站設計系統重構（2026-07-18）。** 整個網頁儀表板換上全新視覺語言：
  沉穩的四層表面疊出深度、克制的動效、成體系的圓角與陰影，資訊密度更高卻更
  好讀。底層改用 `web/src/components/mds/` 新元件庫（一套按鈕／卡片／列表／
  對話框／設定版型等原語），65+ 個頁面全數遷移到這套原語上，舊的 Calm Glass／
  Soft Play 設計系統與其殘留元件、CSS 一併移除。功能一個不減，只換外觀與結構。
  使用者可感知的變化：全新的整體視覺、**側邊欄導航重新分組**（個人／工作／
  公司／設定四區）、**管理區收斂為統一的「設定式」版型**（左側分組導覽＋設定列）、
  **AI 員工的設定拆成多個子分頁**（能力與設定分頁瀏覽，不再是一張擠滿欄位的
  巨型表單）。深淺主題與三語（繁中／英／日）維持同步。設計文件：`web/DESIGN.md`。
- **Create/Edit Agent 從彈窗改為獨立頁面（2026-07-16）。** `/agents/new` 與
  `/agents/:id/edit` 取代 AgentsPage 內的兩個巨型 Dialog（頁面可深連結、
  表單不再擠在彈窗內捲動）；行為與欄位完全保留（含個人版隱藏部門欄位）。
  `AgentsPage.tsx` 由 2,423 行縮至 452 行，表單拆至 `pages/agent-form/`；
  順手移除從未能開啟的 `InspectDialog` 死碼。
- **個人版隱藏部門/企業設定（2026-07-15）。** 個人版是單人形態，無部門概念：
  導航「部門管理」改 `enterprise` 閘（個人版隱藏）；新建／編輯 Agent 對話框的
  部門下拉、Skill 安裝的 `department:` scope 選項在個人版一併隱藏（沿用
  `system.status.edition_profile === 'personal'` 既有 gate 慣例）。多帳號成員頁
  原本就是 enterprise 閘，個人版本來就看不到。

## [1.36.0] - 2026-07-15 — Vetted Skill/MCP install-from-URL + department-routed install approvals

### Added
- **Skill / MCP 安裝簽核鏈（2026-07-15）。** 管理員以外的使用者安裝 Skill／MCP
  前，須送出**簽核申請**，內容包含該項目的**功能說明**與**安全審查結果**
  （風險等級＋逐項發現），核准後系統才會實際安裝。簽核鏈：**員工** → 部門主管
  核准 → 管理員核准 → 安裝；**主管** → 管理員核准 → 安裝；管理員仍為直接安裝。
  管理員核准可一次涵蓋兩關（上級短路）。新增 store `install_requests`（SQLite
  兩階簽名鏈，角色感知 decide，fail-closed：逾時＝拒絕、終態不可翻轉、風險 ≥
  High 於申請與執行兩端都拒絕）；admin/manager RPC `install_requests.list`／
  `install_requests.decide`（最終核准即伺服端重掃並安裝），任一登入者
  `skills.install_request`／`mcp.install_request`／`install_requests.mine`。
  審批頁（ApprovalsPage）新增「安裝簽核申請」區；Skill／MCP 引入對話框對非
  管理員改為「送出簽核申請」並顯示待簽狀態。
- **簽核鏈的部門路由（2026-07-15）。** 使用者新增 `department` 欄位（`users`
  表冪等 migration；成員管理頁建立／編輯對話框以 datalist 帶出既有部門，
  `is_valid_department` 驗證）。員工的安裝申請只路由給**同部門**的主管簽核
  （大小寫不敏感精確比對）——主管的「安裝簽核申請」清單只列出自己部門、且尚
  待主管簽的員工申請；跨部門主管與無部門主管都無法簽（會回「屬於其他部門」）。
  未設部門的申請退回「任一主管」的相容行為；管理員不受部門限制、仍可短路兩關。
  路由判斷集中在 `InstallRequest::manager_may_sign`。

### Changed
- **`skills.vet` 與 `mcp.import.fetch` 放寬為任一登入者可呼叫（2026-07-15）。**
  這兩支是唯讀的抓取＋安全掃描（無任何寫入、共用 SSRF 防護），開放給非管理員
  是為了讓其在送出安裝簽核前能預覽功能與掃描結果。實際安裝仍受管理員／簽核鏈
  把關。
- **Skill 從 GitHub / URL 引入（2026-07-15）。** Skill 頁（原「Skill 市場」，
  導航改名為「Skill」）市場分頁新增「從 URL 引入」：支援 GitHub repo（自動
  讀取 SKILL.md）、blob 檔案連結、GitLab、Gist 與任意 raw 檔案網址。內容由
  後端抓取（共用 SSRF 防護：擋 loopback／私有網段／雲端 metadata，逐跳
  redirect 重驗，1MB 上限，HTML 頁面拒收）並先過安全掃描，掃描未通過無法
  安裝；`skills.install` 伺服端 fail-closed 重掃不變。
- **MCP Server 從 GitHub / URL 引入（2026-07-15）。** MCP 工具頁新增
  「從 URL 引入」：貼 GitHub / GitLab repo（依序尋找 `.mcp.json`／`mcp.json`／
  `server.json`（含 **MCP Registry 2025 schema**：npm→npx、pypi→uvx、
  oci→docker、remotes→自動以 `npx -y mcp-remote <url>` 橋接）／**README
  設定範例**（fenced code block 內的 `mcpServers` 片段，跨平台重複片段自動
  去重）／`package.json`（有 `bin` 才推斷 `npx -y <pkg>`））或任意 JSON
  manifest 網址，後端抓取後**逐一安全掃描**
  （新掃描器 `mcp_scan`：shell／下載器／提權指令、inline eval、shell
  metacharacters、docker `--privileged`／根目錄掛載、env 指令替換等，
  與 skill 掃描共用風險分級，risk ≥ High 拒絕），管理者審視 command／args／
  env 與掃描結果後才可安裝，可選同時寫入 Marketplace 清單
  （`~/.duduclaw/marketplace.json`）供重複使用。新增 admin RPC
  `mcp.import.fetch` / `mcp.import.install`（安裝端 fail-closed 重掃）。

### Changed
- **既有 MCP 安裝路徑補上同一道安全閘（2026-07-15）。** `mcp.update` 的
  add 動作與 `marketplace.install`（含使用者自帶 marketplace.json）現在
  也會掃描 server 定義並在 risk ≥ High 拒絕——先前這兩條路徑完全不經
  掃描直接寫入 `.mcp.json`。內建 catalog 全數通過掃描（有回歸測試鎖住）。

### Fixed
- **`skills.vet` 補上 SSRF 防護（2026-07-15）。** 先前 dashboard 的 skill
  安全掃描 RPC 直接抓取任意 URL，可被用來探測內網／雲端 metadata 端點；
  現在與 web_fetch 共用同一個 `validate_url` 閘，並限制回應大小與 redirect。
- **Dashboard 團隊板模一鍵備妥（templates.* RPC，2026-07-14）。** 首次登入
  onboarding 可選擇產業：後端「備妥」該產業的部門板模但**不建立任何 Agent**，
  由管理者逐一建立——建立時可選部門角色，SOUL.md 以文本編輯器呈現可修改，
  CONTRACT.toml / agent.toml 亦可在進階區修改（後端 TOML 驗證 fail-closed，
  改壞不寫入）。另提供跨產業 CEO（營運總管）板模作為第一位 AI 員工的建議起點。
  新增 admin-gated RPC 五支：`templates.industries` / `templates.stage` /
  `templates.roster` / `templates.role` / `templates.create_agent`（license
  `premium_templates` 閘，未解鎖回 upsell 旗標）。premium 板模的檔案系統探索
  邏輯自 `duduclaw-cli` 上移至新模組 `duduclaw-gateway::premium_templates`
  （cli 改 re-export，wizard 行為不變）；agent.toml 身分接線與合規 overlay
  append 以 `toml_edit` 保留註解地組裝。WelcomePage 精靈改 4 步（新增產業選擇），
  AgentsPage 建立對話框支援套用板模＋SOUL.md 編輯＋進階 TOML 編輯（i18n 三語）。

- **Dashboard 授權升級 UI（2026-07-14）。** LicensePage 新增「升級／啟用授權」卡：
  本機指紋顯示＋複製（購買時提供）、授權金鑰啟用（貼 base64 或 JSON）、夥伴
  NFR 兌換碼免費路徑；首跑期間啟用成功可一鍵返回設定精靈。後端三支 admin-gated
  RPC：`license.fingerprint` / `license.activate` / `license.redeem`——啟用走
  fail-closed 驗證（簽章→指紋→效期，全過才寫檔；dashboard 不接受檔案路徑輸入），
  成功後 `LicenseRuntime::install_and_reload` 熱重載，premium 功能**免重啟**即解鎖
  （活體驗證：鎖定→啟用→22 產業板模即時解鎖）。

- **Agent 衣帽間（2026-07-14）。** AI 員工造型改為遊戲式配件組合：帽子／頭部／
  身體／手持／腳部／裝飾六個槽位＋主色（10 色）自由搭配（30+ 內建配件——高帽、
  皇冠、工程帽、墨鏡、西裝、圍裙、咖啡、扳手、球鞋、光環…）。員工詳情頁
  「造型」卡開啟衣帽間對話框：即時 bust 預覽（就是列表用的同一個角色元件）、
  隨機、還原預設；儲存後**同步顯示在員工列表、所有頭像與世界地圖**（PixiJS
  全身角色含腳部配件）。未打扮的員工維持原本的種子造型（零視覺變化）；打扮
  過的員工以角色渲染優先於已上傳照片（照片上傳降級為進階摺疊選項）。新增
  admin RPC `agents.set_outfit`（形狀＋字元集 fail-closed 驗證，`outfit: null`
  還原），`agents.list` / `agents.inspect` 帶回 `outfit`，持久化為
  `agents/<id>/outfit.json`。
- **主管唯讀檢視下屬儀表板（view-as，2026-07-14）。** 主管／管理者可檢視
  **嚴格低於自己階級**成員的個人儀表板（管理者可看主管與員工；主管只能看
  員工，看不了同級主管與管理者）：成員管理頁每列的「檢視他的儀表板」眼睛
  按鈕、或首頁右上「檢視成員儀表板…」下拉。檢視模式顯示唯讀橫幅、隱藏
  「編輯版面」，畫面用**對方的** widget 目錄與版面，資料範圍縮到對方綁定的
  AI 員工（WP11 員工資料範圍）。「不能代為修改」是結構性保證——寫入 RPC
  只存在 `dashboard.layout.set`（永遠寫呼叫者自己的檔），沒有 set-for-others。
  新增 RPC：`dashboard.layout.view {user_id}`（manager+，階級不足回 generic
  permission denied 防枚舉）、`users.subordinates`（manager+，僅回 id／顯示
  名／角色三欄，遠窄於 admin 的 `users.list`）。
- **客製化個人儀表（WP15 MVP，2026-07-14）。** 首頁下半部改為 per-user widget
  版面：右上「編輯版面」進入編輯態（上移／下移／隱藏、底部「已隱藏的元件」
  抽屜重新加入），完成後以**個人 user 身分**存 server 端（`dashboard/layouts/
  <user_id>.json`），重登入仍在；每人互不影響。首發元件：需要我（manager+）、
  正在進行、最近活動、最近任務、通道健康（admin）。新增 RPC 三支：
  `dashboard.widgets.catalog`（**依角色過濾 fail-closed**——無權的 widget 不
  下發，`layout.set` 對無權 id 直接拒絕而非靜默丟棄）、`dashboard.layout.get`
  / `dashboard.layout.set`。戰報 HUD 與世界舞台維持固定不入版面系統。
- **去識別化欄位級設定（2026-07-14）。** 每個輸入來源（工具結果／使用者輸入／
  系統提示／子代理回覆／排程情境）除模式外，可再細選**哪些欄位**要去識別化：
  `only_categories`（只遮這些）／`exclude_categories`（排除這些，重疊時排除
  優先）。TOML 同時接受舊的字串形式（`user_input = "off"`）與新的表格形式
  （向後相容，空清單自動收斂回字串形式）。dashboard 去識別化分頁改版：
  「偵測規則集」從盲打名稱改為勾選清單（內建 5 組＋自訂，顯示各組涵蓋欄位）；
  來源列可展開欄位範圍選擇器（身分證字號、手機、Email、信用卡…18 種欄位
  中文標籤）。`redaction.get` 回應新增 `available_profiles` 目錄（含每組
  規則數與欄位類別）；`sources` 改為詳細物件形式。引擎端過濾在 pipeline
  逐 match 套用，audit 與 vault 行為不變。**設定即時生效（熱重載）**：
  `redaction.update` 寫入 config.toml 後就地重建 RedactionManager 熱插拔
  （vault GC 任務隨之重啟，處理中的訊息沿用舊規則自然收尾，下一則訊息即用
  新規則），不再需要重啟 gateway；重建失敗時保留變更前的即時規則並在回應
  `warning` 誠實回報（dashboard 以錯誤 toast 呈現）。
- **部門管理頁＋新增 AI 員工時的組織定位（2026-07-14）。** 新增「管理 → 部門」
  頁（admin）：預先建立部門（實體化為 `shared/wiki/departments/<dept>/` 知識
  空間，維持 WP7「部門＝衍生」設計，不引入新儲存）、檢視各部門成員／知識頁／
  技能數、刪除（有成員拒絕；有內容需二次確認）。新增 RPC 三支：
  `departments.list`（manager+）/ `departments.create` / `departments.remove`
  （admin）。新增 AI 員工對話框加入「上級 AI 員工」與「隸屬部門」下拉（板模
  路徑預設沿用板模接線，可覆寫）；`agents.create` / `templates.create_agent`
  接受 `reports_to`（須為既有員工，建立前驗證）與 `department`（WP7 allowlist）
  參數，建立當下寫入 agent.toml。編輯對話框的部門 datalist 併入註冊表清單。

### Fixed
- **Self-Host Pro／Partner 授權啟用後「帳號管理／治理」企業面板消失（2026-07-14）。**
  `EditionProfile::from_tier_key` 只把 business/oem 判為 Enterprise，自架線的
  企業方案 `self_host_pro` 與 `partner` 落到 Personal——啟用授權後 dashboard
  的多帳號管理（`/manage/users`）、治理等 `enterprise` 導覽項反而被隱藏。
  修正對應表（與 features.toml `dashboard_enterprise = true` 的 tier 同步，
  snake_case 與 kebab-case 皆接受）；`system.status` 的 `edition_profile`
  即時反映，授權熱啟用後免重啟企業面板即出現。帳號管理頁的「綁定 AI 員工」
  對話框同時從手打名稱改為現有員工下拉選單（已綁定者自動排除，roster 讀取
  失敗時退回文字輸入），zh-TW 介面用語統一為「AI 員工」。
- **首跑親測四修（2026-07-14）：**①console 首跑訊息改引導至 dashboard 直接設定
  管理者密碼（loopback bind 不再印一次性密碼——與 first-run claim 流程一致；
  非 loopback bind 仍印，因 claim 端點僅限 localhost）；②全新／閒置系統不再
  彈出全零的「昨日戰報」（`reportHasActivity` 閘，靜默燒當日標記）；
  ③`runtime.detect` 的 Claude OAuth 偵測在 macOS 永遠回 false——憑證在
  Keychain 不在 `.credentials.json`，檔案探測 miss 時改問 `claude auth status`
  （8s timeout）；同函式把 `~/.duduclaw` 誤當使用者 HOME 傳給
  `which_*_in_home`，nvm/bun 安裝的 CLI 全數隱形——改 PATH 優先＋真使用者
  HOME（五個 runtime 一併修）；④首跑精靈的「前往授權頁」被 FirstRunGate
  彈回第一步——`/license` 加入 first-run 白名單，精靈進度（不含 API key）
  以 sessionStorage 續存，返回時從原步驟繼續。
- **Template agent.toml 帶 `[container]` 節時 registry 靜默略過（2026-07-14）。**
  `ContainerConfig.additional_mounts` 缺 `#[serde(default)]`，任何板模部署的
  agent.toml 只要有 `[container]` 節而未寫該 key，typed 解析即失敗、agent 被
  scan 靜默跳過（免費 `templates/` 中五個帶 `[container]` 的板模與全部 premium
  板模都中招）。已改為 default-empty，由 114 角色全量活體掃雷驗證。
- **建立 main agent 失敗仍降級現任 main（`agents.create` 與 `templates.create_agent`）。**
  名稱撞既有 agent 時 `demote_current_main` 已先執行，現任 main 被永久降級。
  兩處都改為先原子取得目錄（`create_dir`，同名併發只有一方成功）再降級，
  失敗即回滾刪目錄；`templates.create_agent` 的部分寫入失敗不再留下缺
  CONTRACT 的半成品 agent（agent.toml 原子改名移至最後作為 commit point）。
- **成本／快取效率遙測 dashboard RPC（#3，2026-07-12）。** 為既有的
  `cost_summary` / `cost_agents` / `cost_recent` MCP 工具補上三個 admin-gated
  （`require_admin!` fail-closed）dashboard RPC，位於
  `crates/duduclaw-gateway/src/handlers.rs`，**復用同一條 `CostTelemetry` 查詢
  邏輯**（`summary_global` / `all_agents_summary` / `recent_records`），未另寫成本
  或快取效率公式：**`cost.summary {hours?=24}`**（總請求／各類 token／
  `avg_cache_efficiency`（＝`cache_hit_rate`）／成本 millicents／快取節省 ＋ 由
  `near_price_cliff_for` 衍生的 200K price-cliff 狀態 block）；**`cost.agents {hours?}`**
  （per-agent 明細 ＋ `cache_health`）；**`cost.recent {limit?=20,≤500}`**（近期
  per-request 記錄）。遙測未初始化時回良構的空／零 payload（`available:false`），
  不報錯。僅後端 RPC，前端下一波接線。
- **記憶時序／取代鏈 dashboard RPC（F1 Temporal Memory v1.19.0，#5，2026-07-12）。**
  為 `SqliteMemoryEngine` 既有的 `get_history` / `get_at` 補上 dashboard 操作面，
  authz 對齊既有 `memory.*`（`check_agent!(Viewer)` per-agent 可見性），
  `crates/duduclaw-gateway/src/handlers.rs`：**`memory.history {agent_id, subject,
  predicate | memory_id}`**（回該事實的完整取代鏈，各版本
  `valid_from`/`valid_until`/`superseded_by`/`supersedes`/`confidence` ＋
  `is_current` 旗標 ＋ `current_id`；給 memory_id 時先以新增的
  `SqliteMemoryEngine::triple_for_id` 解析出 triple，非 triple／非本 agent 回空鏈
  不報錯）；**`memory.at {agent_id, subject, predicate, at(RFC-3339)}`**（point-in-time，
  回當時有效的事實；查無回 `found:false`）。復用引擎方法，未重寫時序邏輯。
- **per-agent Odoo 憑證隔離 dashboard RPC（RFC-21 §2，#8，2026-07-12）。**
  三個 admin-gated RPC 讓 `agent.toml [odoo]` override 可讀寫測試，
  `crates/duduclaw-gateway/src/handlers.rs`：**`odoo.agent_config_get {agent_id}`**
  （回 profile／url／db／username／allowed_models／allowed_actions／company_ids ＋
  `api_key_set`／`password_set` 布林 ＋ 遮罩 `***set***`，**永不回傳明文或密文**）；
  **`odoo.agent_config_set {agent_id, url, db, user|username, api_key, password,
  profile, allowed_models, allowed_actions, company_ids}`**（api_key/password 走
  AES-256-GCM 加密存 `*_enc`，遮罩佔位符被拒收不覆蓋既有密鑰；url/db 沿用與全域
  相同的 SSRF/HTTPS/db-name 驗證器——復用 `apply_odoo_to_table`）；
  **`odoo.agent_test {agent_id}`**（以「全域 config.toml [odoo] ＋ 該 agent 覆寫」
  的有效設定測連線，憑證優先取 agent、否則全域；對實際撥號的 url 再做一次
  fail-closed SSRF 檢查；不寫入磁碟）。
- **`native_sandbox` ＋ Progent `policy` 經 dashboard 完整 round-trip（#6，2026-07-12）。**
  `agents.update` 的 `apply_capabilities_to_table` 補上 `capabilities.native_sandbox`
  （bool，Seatbelt/Landlock）與 `capabilities.policy[]`（Progent 參數級 tool policy：
  `{tool, effect: allow|forbid|ask, when: [{arg, op: equals|contains|starts_with,
  value}]}`）的寫入與嚴格驗證（非法 effect/op/缺 tool → fail-closed 拒絕整包）；
  `agents.inspect` 現回傳完整 `capabilities`（serde 直出 `CapabilitiesConfig`，含
  native_sandbox ＋ policy 的精確 ToolPolicy 形狀）供前端編輯器 round-trip。
- **Identity Resolution dashboard surface（RFC-21 §1，2026-07-12）。** 為既有的
  `duduclaw-identity` crate ＋ `identity_resolve` MCP 工具補上 dashboard 操作面，
  客戶可當場示範「AI 怎麼知道發訊的人是誰、怎麼拒絕非專案成員」。新增三個
  admin-gated（`require_admin!` 全數 fail-closed）dashboard RPC，位於
  `crates/duduclaw-gateway/src/handlers.rs`：**`identity.resolve`**（輸入 email／帳號
  ＋ channel，走與 MCP `identity_resolve` 同一條 `IdentityProvider` trait 解析路徑，回
  `ResolvedPerson` ＋ `is_project_member`；查無回 `found:false` 不是錯誤）；
  **`identity.config_get` / `identity.config_set`**（讀寫 config.toml `[identity]`：
  provider 選擇 wiki_cache／notion／chained、Notion database id ＋ `field_map`、
  `refresh_seconds`；Notion 整合金鑰 write-only、AES-256-GCM 加密存 `api_key_enc`、
  讀回遮罩 `***set***`，對齊既有 channel token 加密慣例）。provider 依設定建構
  （`build_identity_provider`）：Notion 未設定完整時 fail-safe 降級回 wiki_cache。
  前端：Integrations 頁（`web/src/pages/IntegrationsPage.tsx`）新增「身分解析」分頁
  `web/src/pages/IdentityPage.tsx`——provider 選擇 ＋ Notion 設定表單 ＋「測試解析」輸入框
  （即時查出姓名／角色／專案／是否專案成員）；`web/src/lib/api.ts` 補型別與
  `api.identity.*` 呼叫；i18n 三語（zh-TW／en／ja-JP）。
- **R4 Grok CLI 成為第六個 `AgentRuntime`（xAI「Grok Build」，2026-07-12）。**
  `RuntimeType::Grok`（`crates/duduclaw-core/src/types.rs`）＋ `which_grok` /
  `which_grok_in_home`（`crates/duduclaw-core/src/lib.rs`，官方 `grok` 優先、第三方
  `grok-cli` fallback）。新 `GrokRuntime`（`crates/duduclaw-gateway/src/runtime/grok.rs`，
  仿 antigravity）：偵測到 binary 才由 `RuntimeRegistry` 註冊，`execute()` 走 oneshot
  `grok -p`，system prompt＋history 內嵌進 prompt（無已驗證 `--system` flag、CJK-safe），
  duduclaw MCP server 寫入 `<agent_dir>/.grok/settings.json`，token 用量估算。接線：
  `runtime_config::model_matches_provider`（grok-* ↔ provider grok）、
  `model_capabilities::supports_vision`（Grok 保守 fail-closed，僅顯式 vision id）、
  `cli/lib.rs` `agent create --runtime grok`（scaffold `AGENTS.md`＋`CLAUDE.md`、typo 拒絕）、
  container `sandbox.rs`（`XAI_API_KEY` 注入＋`grok -p` 指令組裝）、dashboard
  `runtime_detect` 與 `runtime_models` 探測。**僅 CLI 偵測＋headless spawn**；
  SuperGrok OAuth（accounts.x.ai device-flow）與 Grok CLI 實際 flag / MCP config 路徑 /
  context 檔名皆標記 UNVERIFIED，列為 follow-up（見 `runtime/grok.rs` 檔頭）。
- **WP4 AI 員工離職生命週期（後端，2026-07-12）。** 新增 admin-gated（`require_admin!`
  全數 fail-closed）員工離職流程：**封存** `agents.archive` / **解封** `agents.unarchive`
  （`status=archived` + 復用 freeze kill-switch 停 heartbeat/evolution；`agents.list`
  預設排除、帶 `include_archived=true` 才回，回傳物件標 `archived` 旗標，零刪除可復原）；
  **交接** `agents.handoff`（memory/wiki/tasks 三開關預設全開：memory 走
  `duduclaw-memory::reassign_agent`（同庫 agent_id re-key，含 FTS 一致性、temporal
  supersession 鏈保留、交易內完成）或 `reassign_agent_cross_db`（per-agent memory.db
  ATTACH 跨庫搬移）；wiki 合併 `agents/<from>/wiki`→`agents/<to>/wiki`（衝突加尾綴不覆蓋）；
  tasks 走 `TaskStore::reassign_open_tasks`（未完成任務改指派）；完成後可選 `auto_archive`
  預設 true；全程冪等，任一子項失敗如實回報 `status:"PARTIAL"` 不吞錯）；**大頭貼上傳**
  `agents.set_avatar` / `agents.clear_avatar`（PNG/JPEG/WebP data URI，magic-byte 驗證＋
  512KB 上限，原子寫 `agents/<id>/avatar.<ext>`，`agents.get`/`inspect` 回傳 data URI、
  `agents.list` 回傳 `has_avatar`）。
- **WP4 移除語意改為軟刪除。** `agents.remove` 由硬刪（移入 `_trash/`）改為 `status=deleted`
  + freeze kill-switch，agent 目錄與 memory.db **資料保留不刪**、從所有清單/路由隱藏，
  拒絕對 main agent 執行（RPC 名維持相容）。archive/remove/handoff 皆寫入 audit log。
- **WP3 WebChat 歷史對話續聊（2026-07-12）。** 新增 `chat.sessions.list`（per-agent 歷史
  session 清單：首句摘要 CJK-safe 截斷、最後更新、輪數；非 admin 依 agent 可見性 fail-closed）
  與 `chat.sessions.history`（載入指定 session 對話輪，取最新 window 維持時序）RPC；`/ws/chat`
  UserMessage 幀帶 session_id 可 resume 既有 session（回送 session_info 確認；不帶維持原行為
  byte-compatible），前端 header `SessionHistoryMenu` 選歷史對話續聊。
- **WP5 agent 自助裝工具審批閘（2026-07-12，安全）。** MCP install-class 工具
  （`skill_hub_install`）一律經 `ApprovalBroker` 送收件匣人工拍板：安全掃描在前（High risk
  直接 DENY 不進審批），broker 不可用/逾時/拒絕皆 fail-closed DENY；唯一豁免為 operator 顯式
  `agent.toml [capabilities] auto_approve_install = true`（預設 false）。反向強制清單
  `approval_required_tools` 覆蓋豁免。補上會議指出的「agent 自主裝工具無授權」缺口。
- **WP6 Grok（xAI）direct-API 支援（2026-07-12）。** `ModelRegistry` 補 `grok-4.3`（1M ctx）
  /`grok-4.5`（500K ctx）＋既有 `grok-4.1-fast`；gateway `runtime` PROVIDERS 表補 xai entry
  對齊 duduclaw-llm preset（OpenAI-compat，tool calling/streaming 齊全）。使用者可經
  `~/.duduclaw/models.toml` override。
- **WP7 部門層知識庫/skill（公司→部門→個人，2026-07-12）。** `agent.toml [agent] department`
  欄位（選填，向後相容）；shared wiki `departments/<dept>/` 命名空間（agent 只讀寫自己部門＋公司層，
  跨部門 fail-closed）；skill 三層查找 per-agent > 部門 > global（近者優先），install scope 文法
  擴充 `department:<dept>`（admin/approval gate 沿用）；`wiki_namespace_status` 回報部門與讀隔離狀態。
- **WP8 白牌欄位級編輯分層（2026-07-12，安全）。** OEM license 攜帶簽章 `branding_editable` claim：
  系統方簽發的經銷商 token 與經銷商簽發的客戶 token 授予不同可編輯品牌欄位範圍。`branding.get`
  回 `editable_fields` 供前端遮罩；`branding.set`/`reset` 依範圍過濾（違規欄位整請求拒絕、部分範圍
  writer 不能清掉經銷商其他欄位）；`distributor.issue` 收選填 `branding_editable`。未顯式升級為 vendor
  級的欄位預設 system-only（fail-closed），System 範圍需真實簽章驗證的 issuer 私鑰、config 存在
  不足以升權。向後相容：無 claim 的 license 解析為完整 vendor 集。
- **WP9 Telegram 共用 bot + 員工綁定連結/QR（2026-07-12）。** 一間公司共用一個 Telegram bot，
  員工掃 QR／開連結即綁定到自己的 AI 員工，取代「一員工一 bot token」（規避 Telegram 多 bot
  帳號鎖定風險）。新增 `crates/duduclaw-gateway/src/agent_binding.rs`：`AgentBindingStore` 持久化
  `~/.duduclaw/agent_bindings.json`（`with_file_lock` 冪等原子寫，fail-closed）—— 存
  `(channel, external_user_id) → agent_id` 綁定表＋一次性綁定 token（SHA-256 digest 存儲、
  預設 TTL 15 分、用過即失效、`max_uses` 上限、過期自動修剪；plaintext 僅存在於 deep-link）。
  admin-gated RPC `channels.telegram_bind_token`（輸入 target agent，getMe 即時取 bot username
  不硬編，回 `token`/`deep_link`(`https://t.me/<bot>?start=<token>`)/`bot_username`/`expires_in_minutes`/
  `max_uses`）。全域 bot 收 `/start <token>` → 驗 token → 綁定該 Telegram user → target agent
  （成功同時 `approve_user` 過存取閘），無效/過期明確友善拒絕不靜默；每則訊息先 `resolve_bound_agent`
  路由到綁定 agent（agent 已刪則 fail-closed 不誤路由），未綁定且開啟 `shared_bot_binding` 設定時
  回引導訊息，否則維持既有 default-agent 行為。per-agent token 多 bot 模式不受影響（維持原路由）。
  external_user_id exact 比對、跨 channel token 不混用。前端通道設定頁「員工綁定連結」對話框：選
  AI 員工 → 產 token → 顯示 deep-link＋QR（純前端 `qrcode-generator` 產 SVG，無外部 CDN/服務）＋複製鈕；
  i18n 三語同步。

### Changed
- **排程任務設定統一到「例行工作」頁,移出系統設定→進階（2026-07-13）。** 原本排程管理是
  分裂的:`/routines`(例行工作)頁只能列出+暫停/恢復/刪除,而新增/編輯藏在「設定→進階→排程任務」
  的 `CronTab`。改為把新增/編輯(共用同一個 `ScheduleBuilder`)直接併入 `RoutinesPage`——頁首
  「新增例行工作」按鈕 + 每列「編輯」鈕 + 共用 create/edit 對話框;`SettingsPage` 移除 `cron`
  分頁(TabId/VALID_TABS/TAB_META/ADVANCED/render 一併清掉),舊 `?tab=cron` deep-link 自動
  導向 `/routines`;刪除已無引用的 `CronTab.tsx`。同一批 `cron.*` RPC,無後端變更;i18n 三語同步。
- **Grok Build runtime 依 docs.x.ai 官方文件重寫,去除 R4 的 UNVERIFIED 假設（2026-07-13）。**
  以 docs.x.ai 一手來源核對後重寫 `runtime/grok.rs`:headless `-p/--single`(確認)、模型
  `--model`(確認)、工具限制 `--tools`/`--disallowed-tools`(確認,疊在 `native_sandbox` 硬閘上)、
  auth `XAI_API_KEY` 環境變數(非先前誤設的 `GROK_API_KEY`)、`AGENTS.md` 指令檔家族。**最大修正**:
  MCP 設定改寫為 `[mcp_servers.duduclaw]` **TOML**(寫入 per-agent `<agent_dir>/.grok/config.toml`,
  merge 保留其他表)——先前誤仿 Gemini 寫成 `.grok/settings.json` JSON。agent 身分另經 spawn env
  轉發作為 fallback;`--version` 探測加逾時(裸 `grok` 會開 TUI,不能讓探測卡住)。殘餘(需 live CLI):
  `--tools` 清單分隔符、專案本地 config 探索、`--output-format json` schema、完整 `--model` 名冊
  (`grok models`;目前僅 `grok-4.5`/`grok-build-0.1` 經文件確認)。`docs/features/feature-inventory.md`
  同步更新。
- **去識別化設定 UI 改用白話，外部系統（ERP/CRM）成一等公民（2026-07-12）。** 客戶回饋
  「設定太難懂，連開發者都看不懂」——`RedactionTab`（`web/src/components/settings/sections/`）
  重寫：主開關配一句白話說明（明確點名檔案／Wiki／記憶／Odoo／鼎新 ERP／CRM 皆涵蓋）；
  「資料來源保護」每列加白話標題＋一行說明，主保護點「AI 讀取的外部資料」置頂，遮蔽模式
  改稱一律遮蔽／不遮蔽／智慧遮蔽／沿用上游；工程術語的「工具出口規則＋glob」改成
  **「外部系統（ERP／CRM／資料庫）」**區塊，用 Odoo／鼎新 ERP／Salesforce／HubSpot／自訂
  一鍵加入（底層仍是既有 `tool_egress`，無後端 schema 變更），外送政策白話化為
  完全不外送（最安全，預設）／需要時還原真實值／原樣傳遞代號。保管期限／清除／設定檔收進
  「進階設定」摺疊區。誠實標註：Odoo 為內建連接器，鼎新／Salesforce／HubSpot 標
  「範本·連接器規劃中」（規則可預先武裝，不假裝已有 live 整合）。i18n 三語同步、新增
  `common.remove`。純前端＋文案，引擎不動（所有 MCP 工具結果早已在 `mcp_dispatch.rs`
  單一節流點以 `Source::ToolResult` 去識別化，外部系統本就在保護範圍內）。
- **首屏/清單效能三修（2026-07-12）。** (E1) 新增輕量 `agents.avatar` RPC（輸入 `agent_id`，
  只讀 `agents/<id>/avatar.<ext>` 回 data URI 或 null，**不**跑 telemetry 月度聚合、**不**序列化
  SOUL/identity/skills/model config；authz 沿用 `check_agent!(Viewer)` fail-closed）；前端 avatar
  store 改呼叫它取代 `agents.inspect`，首屏 N 個有頭貼員工不再各發一次重 RPC（`agents.inspect`
  的 `avatar` 欄位保留相容）。(E2) `chat.sessions.list` 改為先在內層 `SELECT id ... ORDER BY
  last_active DESC LIMIT` 縮小列集，外層才對這批 id 算 title/turn 相關子查詢，並加
  `idx_sessions_last_active(agent_id, last_active)` 索引（冪等建於建表路徑），大量 session 時
  popover 不再全表掃描；清單內容與排序不變。(E3) `agents.list` 的 `agent_has_avatar` 由每員工最多
  3 次 `Path::exists` stat 改為單次 `read_dir`。 `AgentStatus` 新增 `is_operational()` /
  `is_listable(include_archived)`（fail-closed，非 Active 皆非 operational），取代散落的 ad-hoc
  match；掛到 MCP `list_agents`（恆隱藏 Deleted）、`spawn_agent`/`agents.delegate`（拒絕
  non-operational 目標不 enqueue）、「Your Team」名冊、dashboard `agents.list`。修正軟刪/封存 agent
  仍可被 spawn/委派/列名的漏洞。
- **部門 wiki 讀隔離與 `.scope.toml` 寫政策正交（2026-07-12，安全）。** 部門讀隔離永遠生效，不再被
  `.scope.toml` 對 `departments` 命名空間的宣告反向關閉；`shared_wiki_stats`/`lint` 亦依部門過濾。
- **agent 封存/解封保留原演化旗標。** archive 快照 `evolution.enabled`/`heartbeat.enabled` 原值，
  unarchive 還原（無快照保守維持 false），不再無條件開啟自我演化。

### Security
- **WP5 安裝審批閘上收 dispatch 層 — 補 `approval_required_tools` fail-open（2026-07-13）。**
  審批閘原先只嵌在 `handle_skill_hub_install`,導致 operator 在 `agent.toml [capabilities]
  approval_required_tools` 列出**其他**工具時被靜默忽略(死設定 = fail-open)。新增
  `mcp::gate_tool_approval_dispatch` 並在 `mcp_dispatch::dispatch_tool_call` 的統一節流點
  (complete mediation I3,涵蓋 stdio/HTTP/SSE)於派工前執行:任何 `install_approval_required`
  為真的工具都先過審批,fail-closed(拒絕/逾時/broker 不可用皆不派工)。`skill_hub_install`
  維持自身「掃描後才審批」的較佳流程並在 helper 內排除,避免重複提示。
- **avatar/logo 影像驗證器合併為單一 fail-closed 來源,logo 補預解碼 DoS 防護（2026-07-13）。**
  `branding::validate_image_data_uri` 成為 PNG/JPEG/WebP data URI 的唯一驗證器(SVG 拒收、
  magic-byte 比對、解碼上限),avatar (`handlers.rs`) 與 logo (`branding.rs`) 都改呼叫它。
  合併時把 avatar 既有的「解碼前先擋 encoded 長度」(F8)提升為共用行為——**logo 路徑原本缺這道
  防護**,惡意超大 base64 會先被完整解碼進記憶體才檢查大小,現已在解碼前擋下。
- **WebChat resume 擁有權閘（2026-07-12）。** `/ws/chat` resume 強制 resumed session 屬本連線
  （id 相符或 `{session_id}#` 前綴），跨 channel / 他連線一律拒絕（fail-closed）；關閉「送他人
  session_id 讀寫其對話」的越權。殘餘限制：webchat session 無 user 維度，前一連線的 session 無法
  跨連線 resume（誠實 DEGRADED，待 webchat 導入真實使用者驗證）。
- **收尾波（2026-07-12）— P2 兩項＋G12 落盤＋四項欠帳接線。** G13 Talk Mode（dashboard
  對話模式：WebAudio RMS VAD-lite＋狀態機＋Talk 切換鈕，疊在既有 PTT/STT/TTS 上；
  不做 wake word／barge-in；mic 迴圈 PENDING-LIVE）；G15 Live Canvas（agent
  `canvas_push`/`canvas_clear` 推 HTML 視覺工作區，寫入時 ammonia allowlist＋渲染時
  `<iframe srcdoc sandbox="">`＋自包含 CSP 三層防護，`canvas.get` RPC＋`/畫布` 頁）；
  G12 step 事件落盤（`run_steps.db` 持久化 tool_step/todo_update，runs.get 合併真步驟，
  祕密 mask-before-truncate）；`user_code_profile` MCP 工具；`[fork] judge = "llm"` 生產
  LlmJudge 建構點（FallbackJudge 降級）；ephemeral 成本父歸因（`ephemeral_parents` JOIN）。
- **P1 尾輪十五項（2026-07-12）。** G4 session 可攜三件套（`/handoff` 歧義即拒防跨使用者外洩、
  `/undo` tombstone 軟刪、`/rollback` 對話水位、世系 `#N`；chat 指令派發補齊 TG/DC/LINE）；
  G5 skill hub taps（clawhub/lobehub 一手驗證、**裝前必過安檢掃描 fail-closed**）＋curator
  生命週期（30d stale／90d 封存／pin 豁免，無使用訊號絕不自動封存）；G7 MoA 虛擬模型
  （`[moa.<name>]`＋`moa:` 解析、提案 `<data>` 降格、usage 誠實加總、gateway direct-API
  路由＋CLI 路徑明確拒絕）；G9 agentcompanies 雙向互通（export 確定性＋secret 全 scrub＋
  PARTIAL 誠實標注、npm `@duduclaw/paperclip-adapter` 建置測試綠未發布）；G12 執行紀錄
  `/runs`（sessions.db＋tool_calls.jsonl 誠實重建，未持久事件明示不偽造）；M2 User-as-Code
  read-only 實驗（typed 規則＋四階確定性衝突解決）；M3 JitRL 零梯度學習（OpenAI-compat
  `logit_bias` Tier B 上線、llama.cpp seam、預設關、僅顯式回饋 `jitrl_feedback` MCP）；
  S3 紅隊外部靶場（`duduclaw test --bank`＋over-defense 追蹤＋25 案例起始 bank）；
  R2 Foresight 前綴預警（確定性零 LLM、`run.at_risk` autopilot 事件）；R3 MAST 14 模式
  失敗分類（channel_failures＋eval 報告）；R4 audit 輸入捕捉（先遮罩後截斷、白名單擴充
  20 項高危工具）；O2 `spawn_ephemeral` 動態子代理（四元組、特權升級 fail-closed、GC
  圍堵）；O3 FineVerify 細粒度裁判（確定性聚合）；O4 誠實成本守門（委派 vs 直接回覆報表
  ＋spawn 成本提示，粒度限制如實輸出）；U4 共同計畫（`plans` 表＋`/plans` 頁＋agent
  holder-guard 步驟更新＋prompt 注入）。
- **白牌 P2 後段 — HTML 區塊、簽章散發包、主題色、金鑰自帶回家地址、channel 白牌。**
  ① **About HTML 區塊**（`about_html`）：新增 `ammonia` 依賴，保守 allowlist 消毒
  （`<a>` 強制 `rel="nofollow noopener noreferrer" target="_blank"`、`<img>` 僅
  `data:image/png|jpeg|webp` 且沿用 logo 的 magic-bytes/512KB 驗證、剝除
  `style/class/id/on*/<script>`、>64KB 直接拒絕）；消毒時機為 `branding.set` 儲存前＋
  `branding::load` 讀出後（防手改檔）。新 RPC `branding.preview`（所見即所存）。
  ② **主題色**（`accent_color`，驗證 `#rrggbb`）隨品牌散發。
  ③ **簽章散發包**（`branding.bundle.json`）：由 owner issuer 金鑰簽章，任何 instance
  只要驗簽通過即自動套用品牌，**無需 white_label license** 即可*顯示*（*編輯*仍受 gate）；
  gateway 品牌解析順序 local `branding.json` > 驗簽通過的 bundle > 預設，回應新增頂層
  `source` 欄位；owner gateway 新增 `POST /v1/branding/sign`（issuer-gate、per-IP 10/min、
  複用 refresh 的 subscription+fingerprint 閘）；RPC `branding.bundle.create`（經銷商自助）
  與 `distributor.bundle.sign`（owner 離線代簽）。vendor 名永遠疊加在最上層，散發包蓋不掉。
  ④ **金鑰自帶回家地址**：`config.toml [distributor] public_url` 設定後，`distributor.issue`
  將 owner URL 嵌入金鑰 `control_url`，客戶 instance 無需設 `DUDUCLAW_CONTROL_URL` 即自動
  續期（根治 60 天離線降級）。⑤ **Channel 白牌**：Telegram/Discord/Slack/Google Chat/
  WebChat 使用者可見的 "DuDuClaw" 字樣改吃品牌生效名稱（`effective_product_name`，短 TTL
  快取；log/內部識別不動）。詳見 `docs/guides/white-label.md`「Shipping your branding to
  customers」節。
- **G1 派工引擎收尾 — lease 續租＋Goal 鏈（G8）。** 續租三面向接上：dispatch engine 的
  `LeaseRenewalGuard` RAII ticker（lease/3 週期）、外部認領者的 `tasks_renew` MCP 工具
  （holder-guarded）、系統提示 Task Queue 段與 heartbeat 喚醒訊息的續租教學＋
  `tasks_claim` 回應附 `lease_note`。依賴閘控改為真強制：`atomic_claim` 在單一
  IMMEDIATE transaction 內驗證每個 `depends_on` 皆存在且 `done`（fail-closed，回傳
  `BlockedByDeps`）。新 `goals` 表（parent 鏈、循環拒絕、TOCTOU 防護）＋
  `goals_create`/`goals_list` MCP＋`tasks_create goal_id`；pending-task 注入攜帶
  root-first goal ancestry（byte-stable、CJK-safe）。`pending` 任務納入 heartbeat 拉取
  與提示注入。背景引擎預設仍關（`[dispatch] enabled = true` 開啟即安全）。
- **G10 — 企業微信（WeCom）＋釘釘（DingTalk）通道。** WeCom 自建應用：回調簽章
  known-answer 驗證＋AES-256-CBC 加解密（panic-safe 邊界檢查）＋gettoken 快取＋
  text/markdown 發送＋圖片上傳＋±1h 重放窗。DingTalk 企業內部機器人：HMAC-SHA256
  驗簽 fail-closed（1h 時鐘窗）＋sessionWebhook 回覆（conversation 持久化 90 分鐘窗，
  超窗主動發送誠實回錯）＋`*.dingtalk.com` 錨定允許清單防 SSRF。接線鏡射 feishu 全部
  註冊點（含 dashboard ChannelsPage、sender factory、GATED_CHANNELS、委派轉發）。
  活體驗證需真租戶憑證（PENDING-LIVE）。
- **G11 — Work Timeline（公司級時間軸）。** 新 `timeline.list` RPC（authz 同
  `activity.list`、非 admin fail-closed agent filter）＋純 SVG Gantt 頁 `/timeline`
  （每 AI 員工一 lane、重疊自動堆疊、1h/6h/24h/7d、now 線、雙主題、三語 i18n、30s
  靜默刷新）。誠實呈現：任務板有真起迄 ⇒ bar；活動/心跳只有單點 ⇒ dot，不捏造時長。
- **N1/N2 — 夜間引擎真 LLM adapter（活體驗證）。** `night_llm::RotatedNightLlm` 走既有
  帳號輪替路徑（CLI → Direct API fallback、haiku 級 utility model），每次呼叫前過
  DailyCircuitBreaker（rolling-24h 花費上限，狀態原子落盤 `night_breaker.json`，重啟
  不歸零）。`config.toml [night] llm_enabled` 預設關（fail-safe：關閉時與 scaffold
  byte-identical）。夜間 spawn 鎖零工具 capabilities、注入記憶一律 `<data>` 降格。
- **G2 收尾 — 訂閱 seat device-code 登入＋proxy 轉發。** `duduclaw auth device
  --provider copilot|qwen`（RFC 8628 device flow、長效 GitHub token AES-256-GCM 加密
  落庫、短效 Copilot token 按需鑄造 <5min 刷新、公開 client id 可 config 覆寫）；
  `duduclaw proxy` seat 轉發（SSE passthrough、無 seat ⇒ models 不列出 fail-closed）；
  `/v1/models` 補 60 req/min 限流（pre-auth）。Qwen 官方已停免費 OAuth ⇒ PENDING-LIVE。
- **S2 — 引數級 provenance v1（PACT，arXiv:2605.11039，library 層）。**
  `duduclaw-llm::provenance` 污染 span ledger（window-hash ≥12 字元比對、CJK-safe、
  512 span 上限鎖存）＋`run_tool_loop` 政策 `Off/Warn/Enforce`：Enforce 只擋「敏感工具
  ×污染引數」組合並以 `is_error` 回饋讓模型重規劃，非敏感工具照跑。預設 Off，既有
  呼叫者零行為變更；v1 限制（子字串比對非資料流追蹤）明載模組 doc。
- **O1 — 信心感知派工路由（OI-MAS，arXiv:2601.04861）。** `delegation_router` 三層
  tier 純函式（機械動詞＋短 ⇒ utility 模型；架構/安全/除錯訊號 ⇒ Preferred；模糊不
  降級），僅作用於 Dispatch 派工路徑（channel reply 不可能被改道）、非 Claude runtime
  不注入 Claude model id。`[delegation] confidence_routing` 預設關，關閉時 byte-identical。
- **U1 — 主動訊息時機引擎。** `proactive_timing::TimingGate` 純函式：24 槽日節律直方圖
  判 quiet hours＋10 分鐘 mid-flow 偵測（sessions.db 唯讀隨查隨算）。只延遲不丟訊息
  （6h 硬上限）、cold start 隱形、使用者排程 reminder 結構豁免；silence breaker 與
  PROACTIVE.md 兩條 send path 皆過 gate。預設開，`[proactive] natural_timing = false`
  kill switch 回復原行為。

- **Distributor white-label portal (backend).** Resellers whose license carries the
  `white_label` feature (tier = Oem) can rebrand the dashboard while the upstream
  vendor credit ("嘟嘟數位科技有限公司 / DuDu Digital Technology Co., Ltd.") stays
  const-assembled into every response — never config-sourced, never writable. New
  `crates/duduclaw-gateway/src/branding.rs` (`branding.json` atomic persistence;
  fail-closed validation: SVG rejected, logo magic-byte + 512 KB + data-URI prefix
  whitelist, CJK-safe codepoint length caps) and
  `crates/duduclaw-gateway/src/distributor_store.rs` (SQLite `distributor.db`,
  WAL/0600, `distributors` + `issued_licenses`). New dashboard RPCs `branding.get`
  / `about.get` (any logged-in user), `branding.set` / `branding.reset`
  (`require_admin!` **+** fail-closed white_label gate), and `distributor.status /
  list / add / update / remove / issue / revoke` (`require_admin!`). Issuance reuses
  the License v2 format (tier = Oem, `public_key_id = v2`, machine-bound, default
  365-day term), self-verifies against the binary's baked v2 public key before
  booking, and never logs the issuer private key; issue/revoke write
  `security_audit.jsonl`. Issuer key path comes only from `[distributor]
  issuer_key_path` (unset = explicit zh-TW error, no path guessing). **Known limit
  (§7):** a self-signed OEM license downgrades to OpenSource after 60 days without a
  phone-home — surfaced as an issue-time warning; a lightweight refresh/CRL endpoint
  is deferred to P2. 17 new unit tests.
- **Distributor white-label control-plane (P2 — refresh & revocation).** The owner
  gateway now acts as a lightweight control-plane for the OEM keys it signs, so they
  no longer trip the 60-day offline downgrade and revocations propagate. New
  `crates/duduclaw-gateway/src/license_serve.rs` mounts two public, issuer-gated
  endpoints (unset `[distributor] issuer_key_path` ⇒ `404` — a plain gateway exposes
  nothing): `POST /v1/license/refresh` re-signs the caller's license with
  `last_phone_home = now` (**never extends the term**; returns `revoked` for a
  revoked key, `403` for a fingerprint mismatch or expired key, echoes the request
  `nonce` verbatim for the anti-replay client) and `GET /v1/license/crl` serves an
  Ed25519-signed CRL (7-day TTL, canonical payload byte-aligned to the client
  verifier). Trust is proven by `subscription_id` + `machine_fingerprint` (no
  bearer, matching the cloud plane); per-IP rate limits (refresh 30/min, crl
  60/min); the issuer private key is read per request, never logged, never echoed.
  `distributor_store` gains `get_license_by_subscription_id`, an idempotent
  `last_refresh_at` column migration + `touch_refresh`, and a pure
  `resign_license_for_refresh` (shares the sign + self-verify kernel with issuance).
  Zero client code change — a reseller sets `DUDUCLAW_CONTROL_URL` to the owner
  gateway. `distributor.status` reports `refresh_endpoint_active`; the console shows
  an endpoint badge, the `DUDUCLAW_CONTROL_URL` setup snippet, and per-key last
  refresh time; the issue-time warning now guides distributors to point at the
  gateway instead of only warning about the 60-day downgrade. 10 new unit tests + 6
  integration tests.
- **Distributor white-label portal (dashboard).** New `useBrandingStore`
  (`web/src/lib/branding.ts`) hydrates from a localStorage cache pre-auth and from
  `branding.get` post-auth, driving the sidebar mark, login page, document title,
  favicon, and agent-name fallbacks (defaults unchanged: DuDuClaw / 🐾). New
  `/about` page shows the reseller's own branding above a fixed vendor block
  ("嘟嘟數位科技有限公司 / DuDu Digital Technology Co., Ltd.") sourced from the
  backend const with a hard-coded front-end fallback. New "品牌設定" settings tab
  (logo upload via base64, PNG/JPEG/WebP only, 512 KB pre-check; read-only with an
  upgrade notice when `white_label` is not licensed) and an admin-only
  `/manage/distributors` console (add resellers, issue machine-bound OEM keys with
  a copyable activation blob, revoke with an honest CRL note; empty state explains
  `[distributor] issuer_key_path`). i18n keys added across zh-TW / en / ja-JP.
- **G6 — A2A v1.0 signed agent cards.** `/.well-known/agent-card.json` now carries
  an EdDSA (Ed25519) detached-JWS `signatures` array so a recipient can verify the
  card's authenticity; `GET /.well-known/jwks.json` serves the public key (OKP /
  Ed25519, RFC 8037). The signing key is auto-generated at
  `~/.duduclaw/keys/a2a-signing.ed25519` (chmod 600) on first start. Fail-closed:
  a key error yields the unsigned card + a warning, never a 500. New
  `crates/duduclaw-gateway/src/a2a_signing.rs` (14 unit tests, sign→verify
  roundtrip). Existing card fields are unchanged — signatures are additive.
- **G2 — Subscription-OAuth breadth + local `duduclaw proxy`.** The `AccountRotator`
  OAuth path is generalized beyond hardcoded Anthropic: OAuth seats carry a
  `provider` field (Claude / ChatGPT-Codex / Copilot / Qwen Portal are catalogued),
  and non-Anthropic seats never fabricate env-var names or leak a seat token as an
  API key. `duduclaw proxy --bind 127.0.0.1:PORT` exposes the account pool as a
  local OpenAI-compatible endpoint (`POST /v1/chat/completions` with SSE,
  `GET /v1/models`, `GET /healthz`) so external tools (Aider / Cline / Codex) can
  borrow subscription quota; Bearer-guarded, loopback by default, 503 fail-closed
  on no accounts. Copilot/Qwen device-code token acquisition and OAuth-seat
  forwarding through the proxy are **PENDING-LIVE** (need per-vendor credentials /
  a CLI-runtime bridge).
- **U2 — Evidence-based approval UX (CHI/FAccT 2026).** Approval cards in the Inbox
  are redesigned around three empirical findings: plan-first (arXiv:2604.04918 —
  "what this AI employee intends to do" summary leads, approve/deny follows),
  heuristic verification (arXiv:2606.05391 — risk badge + opt-in one-tap spot-check
  instead of forcing a full read), and reviewer-fatigue protection
  (arXiv:2606.08919 — daily approval count + same-kind batch hint, never
  auto-approve). Confidence/risk is shown only at the whole-action level, never
  token-level (arXiv:2605.28571) — codified in `web/DESIGN.md` §13. High-risk
  actions gate through the shared `ConfirmDialog`. Pure risk-tiering in
  `web/src/lib/approval-risk.ts` (30 tests); backend approval RPCs unchanged.
- **G1 — Durable multi-agent dispatch engine.** Upgrades cross-agent delegation
  from the fragile file-based IPC (`bus_queue.jsonl`) to a durable SQLite task
  lifecycle, closing the gap against Hermes Kanban swarm / paperclip wakeup queue.
  `crates/duduclaw-gateway/src/task_store.rs` gains an idempotent column
  migration (`claimed_by` / `claimed_at` / `lease_expires_at` / `depends_on` /
  `retry_count` / `max_retries` / `goal_mode` / `acceptance_criteria` /
  `result_summary` / `judge_feedback`) plus four durability primitives: **atomic
  claim** (`atomic_claim` — conditional `UPDATE ... WHERE status='pending' AND
  claimed_by IS NULL`, exactly one worker wins), **zombie reclaim**
  (`reclaim_zombies` — a leased task whose worker died is requeued with a retry
  count, or marked `failed` once the budget is spent; NULL-lease board tasks are
  never touched), **dependency unlock** (`claimable_tasks` gates a task until
  every `depends_on` id is `done`), and **goal mode** (`complete_task` routes a
  `goal_mode` task to a `review` state; acceptance is decided by a judge). A new
  background loop `crates/duduclaw-gateway/src/dispatch_engine.rs`
  (`DispatchEngine`, heartbeat-cadence tick, spawned in `server.rs`) drives
  zombie reclaim every tick and runs goal-mode review through a pluggable
  `AcceptanceJudge` (`LlmAcceptanceJudge` reuses the fork crate's `LlmCaller`
  abstraction). **Fail-safe:** a judge error parks the task as `needs_human` —
  never auto-accepted, never looped. The MCP `tasks_claim` tool now uses the
  atomic primitive (falling back to the legacy claim for pre-G1 board tasks),
  `tasks_complete` routes goal-mode tasks to review, and `tasks_create` accepts
  `depends_on` / `goal_mode` / `acceptance_criteria` / `max_retries` / `durable`.
  The `bus_queue.jsonl` rail is retained as a compatibility path (unchanged). 19
  new unit tests (12 task_store + 7 dispatch_engine) cover atomic-claim
  concurrency, zombie requeue/fail at the retry cap, dependency gating, and
  goal-mode accept/reject/judge-failure, plus a guard so `complete_task` cannot
  overwrite a task already in a terminal (`done`/`cancelled`) state. **The
  synchronous primitives (atomic claim, dependency gating, complete) are live
  via the MCP task tools; the background `DispatchEngine` loop is default-OFF**
  (`config.toml [dispatch] enabled = true` / `DUDUCLAW_DISPATCH_ENGINE=1`):
  its zombie-reclaim would falsely requeue any task running longer than the
  fixed 300s lease because there is no mid-task lease-renewal signal yet
  (`renew_lease` has no caller). It stays off until renewal is wired. The
  acceptance judge is likewise spawned with `None`, so goal-mode `review` tasks
  sit safely in `review`.
- **Night Engine — idle-time compute suite (N1–N4).** "The AI employee tidies
  its memory and pre-reads tomorrow's work while it sleeps": four paper-grounded
  capabilities layered on the existing heartbeat scheduler + evolution engine,
  default OFF per agent (`agent.toml [night_engine] enabled = true`; new
  `NightEngineConfig` in `duduclaw-core`). A gateway-side idle-aware scheduler
  (`crates/duduclaw-gateway/src/night_engine.rs`, `spawn_night_engine`) reuses the
  heartbeat agent registry, reads `sessions.last_active` to detect idle agents,
  and fires a night pass bounded by a **per-pass cost cap** (`PassBudget`) and a
  **per-agent daily circuit breaker** (`DailyCircuitBreaker`) so idle compute can
  never run away. **N3 Schema induction** (DCPM arXiv:2606.09483) and **N4
  Recurrence-gated consolidation + deterministic trust verification** (RecMem
  arXiv:2605.16045 + TRUSTMEM arXiv:2606.25161) are live and fully deterministic
  (zero LLM), implemented in `crates/duduclaw-memory/src/night.rs`: N3 scans
  episodic memory for token themes recurring across ≥ `schema_min_support`
  memories and promotes each into a `night-schema` semantic entry (superseded by
  theme via the temporal chain); N4 only consolidates a theme that recurs ≥
  `recurrence_threshold` times, then gates the merge on a coverage / preservation
  / faithfulness check and **rolls back** (never stores) a merge that fails, so
  the store can't degrade the more it tidies. **N1 Sleep-time compute**
  (arXiv:2504.13171) and **N2 Proactive prefetch** (ProAct arXiv:2605.25971) are
  scaffolded behind a `NightLlm` trait — orchestration, budget gating, prompt
  building and `night_cache.jsonl` write are implemented and unit-tested with a
  mock LLM, but the live LLM adapter is not yet wired into the running scheduler
  (passes `None`), so N1/N2 are PENDING-LIVE. 28 new unit tests (12 memory + 16
  gateway) cover the tokenizer, theme detection, recurrence gate, three-axis
  verification, idle detection, circuit breaker, budget, prompt/cache, and the
  end-to-end deterministic + mock-LLM passes.
- **Event-triggered cron (G3, parity with OpenClaw 2026.7).** Cron tasks gain
  two event-driven trigger kinds on top of the existing time schedule. New
  `crates/duduclaw-gateway/src/condition_eval.rs` holds the fail-closed decision
  logic (pure, unit-tested): `TriggerKind` state machine (`time` | `condition` |
  `on_exit`), condition-output parsing (`{fire, message?, state?}` with a 16 KiB
  `state` cap and logs-then-JSON tolerance), and sandboxed execution via the
  existing `duduclaw-sandbox` native OS primitive (macOS Seatbelt / Linux
  Landlock). A **`condition`** task runs its script at each due cron slot and
  fires only on `fire:true`, persisting `state` (≤16 KiB, oversize rejected
  fail-closed) across evaluations and injecting the script's `message` into the
  fired prompt; an **`on_exit`** task fires only when its watch command exits 0.
  `CronStore` gains four idempotently-migrated columns (`trigger_kind`,
  `condition_script`, `condition_state`, `watch_command`) plus `update_trigger` /
  `update_condition_state` setters; `cron.add` / `cron.update` RPCs read and
  strictly validate the new fields (unknown `trigger_kind`, or a
  condition/on_exit task missing its script/command, is rejected at write time).
  Every failure mode — sandbox refusal, spawn error, 30 s timeout, non-JSON
  output, missing `fire`, oversized state — resolves to *do not fire*. The cron
  schedule acts as the evaluation cadence; `last_run` advances at each due slot
  regardless of the gate outcome, so a condition is re-checked only on schedule
  (no per-tick runaway).
- **Lightweight trajectory anomaly detection (R1, Trajectory Guard arXiv:2601.00516).**
  New `crates/duduclaw-gateway/src/trajectory_guard.rs` — a **deterministic,
  zero-LLM-cost** heuristic guard over the structured tool-step stream
  (`StepEvent`/`StepTracker`) plus a cost-slope signal. Four pure, unit-tested
  rules emit `AnomalySignal { kind, severity, evidence }`: **repeated-tool loop**
  (same tool + normalized input ≥N in a window — the runaway-loop fingerprint),
  **excessive depth** (outstanding tool-call nesting past a threshold),
  **cost-slope spike** (cumulative spend rate past `baseline × multiplier`), and
  **trajectory stall** (many steps, overwhelmingly read-only, no productive
  output). Severity grades Low/Medium/High with per-kind de-dup + escalation.
  Wired into the `channel_reply` stream loop: every parsed step feeds the guard;
  a High-severity signal is appended to `channel_failures.jsonl` (with an
  `anomaly` classification, under an advisory file lock) and logged in zh-TW.
  **Fail-safe, not fail-closed** — the guard NEVER kills a task; tripping an
  existing circuit breaker is an explicit operator opt-in (`intervene = true`)
  surfaced as a pure `should_intervene` decision, default report-only. Tunable
  via `config.toml [trajectory_guard]` (conservative defaults, `enabled = true`).
  36 new unit tests (each rule normal/anomaly + window boundaries + cost-slope
  math + severity grading + config parse/clamp + stateful de-dup/escalation).
- **Memory benchmark integration (M1): LongMemEval-V2 + PersonaMem-v2.**
  Two 2026 memory benchmarks wired into the existing `python/duduclaw/memory_eval`
  pipeline alongside LOCOMO/RR/RA. New modules `longmemeval_v2.py`
  (arXiv:2605.12493 — 451 questions, 5 memory abilities, agentic-trajectory
  context) and `personamem_v2.py` (arXiv:2512.06688 — HF `bowen-upenn/PersonaMem-v2`,
  1000 user-chatbot interactions, 300+ implicit-preference scenarios), both
  mirroring `retrieval_accuracy.py`'s loader + metric + alert shape. They measure
  a **retrieval-level `recall@k`** (does the gold evidence memory land in the
  top-K search over the SqliteMemoryEngine batch-query surface), broken down per
  ability / per scenario — a memory-system proxy; full QA answer-correctness needs
  an LLM judge and is left explicitly **PENDING-LIVE**. `to_report()` emits the
  same report shape as LOCOMO so the dashboard shows all three benchmarks together.
  Dataset acquisition via new `fetch_benchmarks.py` (downloads from HuggingFace,
  converts to local jsonl; on no-network / no-`datasets` / unknown-repo it
  **fails honestly** with the exact fetch commands and never fabricates data —
  LongMemEval-V2's repo id must be operator-supplied since it wasn't verified).
  Each benchmark ships a hand-crafted 4-5 question **sample fixture**
  (`data/<bench>/sample.jsonl`) plus a zero-dependency `InMemoryMemoryClient`
  (`fixture_client.py`) so smoke + unit tests run fully offline. Wired into the
  cron runner: the daily 03:00 UTC smoke test gains `longmemeval_v2_sample` /
  `personamem_v2_sample` cases (TC-4/TC-5, sample fixtures); weekly KPIs include
  the benchmark suite (full dataset if fetched, else sample fallback marked
  `dataset: "sample"`); new `monthly_benchmarks` command runs the full sets and
  reports `pending_live` when they aren't downloaded. 30 new unit tests
  (loader / recall metric / alerts / offline end-to-end / honest-fail); downloaded
  `full.jsonl`/`*.raw.jsonl` are gitignored, only sample fixtures tracked.
- **Revocable, epoch-bound capability lifecycle (PORTICO, arXiv:2606.22504).**
  New `crates/duduclaw-gateway/src/capability.rs` upgrades HITL from a *one-shot*
  `request → decide` (where a human approval was a **permanent** grant that was
  never taken back) to `request → grant → invoke`. Approving now mints a
  **`CapabilityGrant`** — an epoch-bound handle tied to a task/session subgoal
  (`scope_epoch`) via `CapabilityBroker::grant` / `grant_from_approval`. A tool
  runs `invoke(handle)` before acting; when the subgoal closes,
  `close_scope(epoch)` **auto-revokes** every handle bound to it, so any later
  `invoke` of the same handle is denied (PORTICO's post-closure "N/N blocked").
  Fully **fail-closed**: unknown handle → `NotFound`, revoked → `Revoked`,
  past-TTL or unparseable expiry → `Expired`, closed scope (even if the row's
  `revoked_at` wasn't yet stamped) → `ScopeClosed`, store error → `Store` — never
  a silent allow; granting *into* an already-closed scope is rejected up front
  (stale-write guard). Storage shares the `approvals.db` file with the approval
  store but owns two tables (`capability_grants`, `capability_closed_scopes`;
  WAL, parameterized SQL only). **Wired:** `approvals.decide` mints a grant on
  approve when the approval payload carries a `scope_epoch` (optional
  `capability_ttl_seconds`); `tasks.update → status=done` calls `close_scope(task_id)`
  to auto-revoke that subgoal's capabilities. Session-end auto-close and the
  MCP-tool `invoke` gate expose the API (`close_scope` / `invoke`) but their
  call-sites are follow-up wiring. 12 unit tests cover grant→invoke, post-closure
  reuse denial, expiry, unknown/revoked handles, and the stale-write guard.
- **Painless migration `duduclaw migrate-from <openclaw|hermes|paperclip>`.**
  One command to move from the three big competitor platforms into DuDuClaw.
  New `crates/duduclaw-cli/src/migrate_from/` module (`openclaw.rs` / `hermes.rs`
  / `paperclip.rs` / `report.rs` + shared `mod.rs`). **Default is a dry-run** that
  prints the migration plan; `--apply` performs the writes; `--rename` imports
  under a `-imported` suffix on a name clash instead of skipping. Every item is
  reported honestly — `IMPORTED` / `PARTIAL` / `SKIPPED(reason)` /
  `CONFLICT(reason)` — rolling up to `COMPLETE` / `DEGRADED` / `PARTIAL`; the
  report is also written to `~/.duduclaw/imported/<platform>/migration-report.md`
  and all token values are masked (first 4 + last 4). Maps source agents
  (workspace `SOUL.md` → `SOUL.md`, `MEMORY.md`/`USER.md` bullets → Semantic
  memory tagged `imported-from-<platform>`), channel tokens (telegram/discord/slack
  → AES-256-GCM-encrypted config.toml `[channels]`, **never overwriting** an
  existing token → `CONFLICT`), the Anthropic API key, `[model] preferred`
  (strips the `anthropic/` prefix; non-Claude models flagged `PARTIAL` for manual
  review), legacy cron `jobs.json` (defensive parse → SQLite cron store), and
  skills (each `SKILL.md` runs through the prompt-injection scanner **before**
  install — a flagged skill is `SKIPPED(security)`, fail-closed). paperclip goes
  via the official `paperclipai company export` directory (`--source` required):
  `reportsTo` → `reports_to` with topological creation order + cycle detection,
  `TASK.md` → Task Board, `recurring` → cron, `COMPANY.md` → shared wiki. Original
  session/conversation files are archived verbatim to
  `~/.duduclaw/imported/<platform>/raw/` (v1 does not parse them into
  `sessions.db`). The `agent create` scaffold logic was extracted into a shared
  `scaffold_agent_dir` helper so imported agents and hand-created agents stay
  byte-compatible. See `docs/guides/migrate-from.md`.
- **Dynamic runtime model discovery (`runtime_models.rs`).** The dashboard model
  picker was backed by a hard-coded, hand-edited cloud model list in
  `handle_models_list` that drifted stale (Claude listed "opus-4-6" with no Fable;
  codex/gemini hard-coded too). Replaced with a per-provider discovery chain that
  probes the *real* installed CLIs / APIs and caches to
  `~/.duduclaw/runtime_models.json` (12h background refresh + startup probe).
  Discovery chain per provider returns `{models, source, fetched_at}` where
  `source ∈ live_api / cli_probe / help_parse / pty_probe / fallback`:
  **claude** → Anthropic `GET /v1/models` when an API key is configured (10s
  timeout) → parse `claude --help` `--model` aliases (5s timeout, stdin closed)
  → static fallback (marked); **codex / gemini / agy** → best-effort `--help`
  `models` subcommand probe (≤5s, stdin closed) → static fallback. All CLI probes
  close stdin + hard-timeout + `kill_on_drop` so a probe can never drop into the
  interactive REPL or hang. Optional `pty_probe` source (drives the interactive
  `/model` menu) is **default OFF**, opt-in via `config.toml [models] pty_probe`.
  New `models.refresh` RPC (login-readable) forces a live re-probe. Each
  `models.list` entry now carries `provider` / `source` / `fetched_at`; the
  picker shows "updated N ago" + a 🔄 refresh button and flags fallback groups as
  "（預設清單，未能即時取得）". Live discovery failures never fabricate a
  live-looking list — they surface the static fallback, clearly marked.

- **Gamification growth persistence (V10-T10.0).** New `growth.rs` — SQLite
  `~/.duduclaw/growth.db` (WAL) storing only facts (achievement unlock
  timestamps, an XP-snapshot audit log, a per-day daily-report cache). A **pure**
  judging engine (`compute_snapshot`, fully unit-tested, byte-identical on
  recompute) scores real internal surfaces (tasks/skills/wiki/cron/custom-skills)
  into XP (task +12 / skill +25 / knowledge page +8 / routine run +5 + one-time
  achievement bonuses) and `Lv = floor(sqrt(XP/100))`. Declarative achievement
  table; sources we cannot read honestly (`inbox_zero_streak_7`,
  `custom_skill_saved_100h`) surface as `available: false` with a documented
  reason instead of a fabricated estimate. New RPCs `growth.snapshot` /
  `growth.daily_report` (login-readable, non-admin).
- **Human × agent custom skills backend (V13-T13.0).** New `custom_skills.rs` —
  SQLite `custom_skill_registry` with a `draft → generating → pending_approval →
  approved / rejected / retired` state machine (illegal transitions refused).
  Pre-approval `SKILL.md` bodies are quarantined in `~/.duduclaw/skills-drafts/`,
  which is **never** a skill-loader scan root (isolation asserted by test). Six
  RPCs `skills.custom_create / custom_generate / custom_update / custom_submit /
  custom_list / custom_retire`; generation reuses the existing `bus_queue`
  delegation channel. Submit runs the mandatory `scan_skill` safety pass
  (includes prompt-injection) — **high/critical risk is refused (fail-closed)**;
  a pass routes to the shared `ApprovalBroker` (`action_kind = "skill_create"`,
  7-day TTL = DENY on expiry). Approval side-effect installs the draft into the
  real global skills dir; deny marks the row rejected with a reason. Single-admin
  self-approval is audited (`self_approved`). Fail-closed unit tests cover all
  three invariants (TTL-expiry = deny, high-risk cannot submit, drafts unscanned).
- **Single-binary commercial upgrade.** A stock `duduclaw` verifies a signed
  `~/.duduclaw/license.json` out of the box — no separate `duduclaw-pro` binary.
  `license_runtime::production_registry()` bakes the production issuer public key
  (env `DUDUCLAW_LICENSE_PUBKEY_*` still overrides; empty/malformed baked key
  fails safe to OpenSource). Upgrade path: drop in `license.json` → restart.
- **Perpetual (buy-out / OEM) licenses.** `license-keygen issue --perpetual`
  issues a no-expiry license (100-year term); mutually exclusive with `--days`
  (clap-enforced). Term licenses (`--days N`) unchanged.
- **Proactive license-expiry warnings.** Gateway logs a warning in the 30/7-day
  pre-expiry window (at boot + daily via the CRL loop); the dashboard shows a
  cross-page `LicenseExpiryBanner` (warning ≤30d / critical ≤7d / expired),
  complementing the passive LicensePage countdown. Thresholds shared with the
  gateway (`classify_expiry_urgency`). Unit-tested.
- **Dashboard: per-AI-staff live status glyph (WP10-T10.2).** CSS-animated
  presence dots on the roster (`AgentStatusGlyph` + `agent-activity-store`):
  idle / 回覆中 / 工具執行中 / 背景固化 / 等待審批, derived non-invasively from the
  existing `activity.new` + `browser.approval_request` WS events (transient TTL
  decay, no new backend truth source). Reduced-motion safe. Unit-tested.
- **Dashboard: owner home incident banner (WP14-T14.2, partial).** A red
  "需要你關注" strip that stays fully silent when all is well and deep-links each
  incident chip to its page. Ships with paused-AI-staff + offline-channel
  sources (existing read paths); budget/approval sources await their read RPCs.
- **Dashboard: activity feed three-tier denoising (WP14-T14.3).** Headline vs
  secondary vs routine tiers — routine per-message chatter is hidden behind a
  "顯示全部細節" toggle, and ≥3 consecutive same-AI-staff updates fold into one
  "N 筆連續更新" row. Unit-tested.
- **Suspected-private-use detection guards (WP6-T6.4b, core).** Labour-relations
  sensitive, so the false-positive guards ship first as `workforce_private.rs`
  (opt-in, off by default): fail-closed with no operator business-scope baseline;
  only high-confidence "suspected private" is flagged ("undetermined" never is);
  exempt list; flag TTL auto-expiry (default 30 days, unparseable timestamp =
  expired). Advisory only — never grounds for discipline, never employee-visible.
  Unit-tested. Haiku classification batch + operator-only UI are the follow-up.
- **CEO/Board governance mode (WP17, core + ADR).** Opt-in `[governance] board_mode`
  (default off — solo deployments unchanged). New `governance.rs`: a typed
  `ApprovalKind` (serde-compatible with the stored `action_kind` strings; also used
  by WP8/WP16) plus fail-closed invariants — `StrategicPlan`/`AgentHire` are
  Board-human-only (an agent identity is refused, "Board = human"), Initiatives are
  board-human-created only, and in board_mode no agent may edit `[budget]` via MCP
  (anti self-promotion). Unit-tested. Design: `docs/adr/ADR-007-board-governance-mode.md`.
  Strategic-proposal flow + Board panel + cascade budget are follow-up integration.
- **LINE OA B2C credit metering (WP7, core).** `LineChannelConfig` gains an
  additive `[[channels.line.accounts]]` array (multi Official Account, each bound
  to an agent with a `credit_rate`); legacy single-OA config still works via
  `resolve_accounts()`. New `credit.rs` ledger (`credits.db`): per-`(oa, user)`
  points balance + append-only events, atomic deduct, fail-closed gate (balance ≤0
  ⇒ no LLM call), `tokens_to_points`. `duduclaw credit grant|balance|history` CLI
  for operator top-ups (PayUni settlement is separate). Webhook `destination`
  routing + per-account signature verify is the follow-up integration.
- **Channel-side approval buttons (WP16, core).** `RichComponent::Buttons` +
  `ActionButton`/`ButtonStyle` model for cross-platform action buttons, plus
  `channel_approval.rs`: the `approval:<id>:<approve|deny>:<nonce>` action-id codec
  (fits Telegram's 64-byte callback cap), fail-closed exact-match approver
  authorization (a forwarded button can't be actioned by the recipient), and a
  one-time nonce against replay. Pure + unit-tested; per-platform native render
  (TG inline keyboard / Slack Block Kit / Discord components / LINE quick reply)
  and the four click-event routes are the follow-up integration.
- **Delegation permission decay — "narrower wins" (WP4, core).** New
  `delegation_scope.rs` defines the permission-snapshot shape carried across a
  delegation hop and the intersection rule: allow-lists intersect (empty = no
  restriction, so the restrictive party wins), deny-lists union, Odoo model/action
  allow-lists intersect — so agent A delegating to a wider-privileged agent B can
  never widen what A could reach. Depth cap prevents unbounded hops. Pure,
  fully unit-tested; dispatcher/Odoo runtime wiring is the follow-up integration.
- **Per-user cost attribution (WP6) — "which employee is spending?".** `token_usage`
  gains additive `user_id` + `channel` columns (idempotent migration); the channel-reply
  path attributes spend to the end user via new `record_attributed` + `CHANNEL_REPLY_USER_ID`
  task-local. New `summary_by_user` query + admin-scoped `cost_users` MCP tool rank users by
  cost; non-human traffic buckets under `(system)`. Guide: `docs/guides/workforce-analytics.md`.
- **Skills speak the employee's language (WP8).** `SkillMeta` gains a `display`
  map (`zh-TW`/`en`/`ja-JP` → localised name+description) with a
  `locale → zh-TW → original` fallback chain, so non-English-reading employees
  see what a skill does. Presentation (`skill_list`, spec) uses it; skills
  predating the field render unchanged. Plus a skill-activation approval helper
  (`skill_approval.rs`, `action_kind = skill_activation`) that carries the
  "省多少分鐘?" estimate (`estimated_minutes_saved`) into the manager's Approval
  Inbox — the data source for the WP10 leaderboard. Spec: `docs/spec/skill-md-spec.md`.
- **Shared-wiki `agent_allowlist` namespace mode — "who may write" control.** `.scope.toml`
  gains a fourth mode: `mode = "agent_allowlist"` + `agents = ["agnes", "boss"]` restricts a
  namespace's writes to those exact agent ids (exact-equality, no substring), operator always
  allowed. Empty list is fail-closed (denies every agent). Honoured by both `shared_wiki_write`
  and `shared_wiki_delete`; surfaced in `wiki_namespace_status`.
  `crates/duduclaw-cli/src/wiki_scope.rs`.
- **`duduclaw redaction verify` — prove de-identification works, don't just claim it.**
  Runs a CSV/text file through the REAL redaction pipeline (vault writes included)
  and emits a Markdown evidence report: every hit (masked original `王**` × rule id
  × token × category), `PASS-THROUGH` lines, and a reversibility check that restores
  each token and asserts it round-trips (`restore OK n/n`). Ships a demo dataset at
  `docs/examples/redaction-sample.csv`. `crates/duduclaw-cli/src/redaction_verify.rs`.
- **Keyword redaction rules (no regex needed).** The `keyword` rule kind is now
  compiled by the engine — operators add a customer name (`Amazon`, `台積電`) via
  `type = "keyword"` and it is redacted with whole-word, CJK-safe matching
  (ASCII terms respect word boundaries; CJK terms match as substrings).
  `crates/duduclaw-redaction/src/rules/keyword.rs`.
- **`code_map` MCP tool — Aider-style repository symbol graph.** tree-sitter
  symbol extraction (Rust/Python/JS/TS/TSX) over the existing HippoRAG-lite
  Personalized-PageRank engine (`graph_rank.rs`): ranks a repo's source files by
  relevance to a query, with cross-file reference edges (def weight 4, ref weight
  1) and `chat_files` personalization. `crates/duduclaw-memory/src/code_map.rs`;
  MCP tool gated by `MemoryRead`, excluded from the external whitelist.
- **Semantic vector memory retrieval (`w_vec`).** Third re-rank signal alongside
  `w_fts`/`w_graph`: pluggable `EmbeddingProvider` with a zero-dependency,
  CJK-safe, stable char-n-gram default (`NgramHashEmbedder`); embeddings stored
  as additive `embedding` BLOB columns; brute-force cosine KNN respecting
  agent/temporal isolation and embedder-identity binding. Opt-in via
  `DUDUCLAW_SEMANTIC_VECTORS=1`. No signal ⇒ ranking byte-identical.
  `crates/duduclaw-memory/src/vector.rs`.
- **Budget circuit breaker — cost enforcement, not just observation.** Hard
  per-agent rolling-window spend caps (`[budget] daily_cap_cents` +
  `monthly_limit_cents`) that block new LLM calls at the dispatch choke-point and
  reply with a zh-TW notice; writes `budget_events.jsonl`. Fail-open on telemetry
  outage. `crates/duduclaw-gateway/src/budget.rs`.
- **MCP Bridge — mount external third-party MCP servers.** `[[mcp.external]]` in
  `agent.toml` spawns external MCP servers (Plane/Chatwoot/Gmail/…) alongside the
  internal server, with a deny-by-default per-server tool allow/deny filter
  (`duduclaw_llm::ToolFilter`), `env://` credential resolution, and fail-safe
  skip/degrade. `crates/duduclaw-gateway/src/mcp_external.rs`; guide at
  `docs/guides/mcp-bridge.md`.
- **Audit export + SIEM sink.** `duduclaw audit` aggregates the existing JSONL
  audit trails (security / tool-calls / channel-failures / budget) into a
  normalized, absolute-time-sorted NDJSON stream with `--since` filtering,
  writable to a file and/or POSTable to a SIEM/webhook.
  `crates/duduclaw-gateway/src/audit_export.rs`.
- **Output guardrail hook** (opt-in `[guardrails]`): scans the outbound reply
  for leaked secrets, prompt-injection echoes, and operator deny-phrases, and
  redacts PII — blocking/redacting before send. Deterministic default (Llama
  Guard is the documented upgrade). `crates/duduclaw-gateway/src/guardrail.rs`.
- **Burn-rate cost anomaly detection**: rolling mean+stddev over an agent's
  per-day spend flags statistical outliers (relative baseline, not a fixed
  threshold). `crates/duduclaw-gateway/src/cost_anomaly.rs` +
  `CostTelemetry::daily_cost_millicents`.
- **Security posture report**: `duduclaw security` scores active protections
  (fail-closed MCP auth, signed updates, injection scanning, HITL, hooks,
  no-plaintext-secrets, budget caps) as a checklist + weighted score.
  `crates/duduclaw-gateway/src/security_posture.rs`.
- **CI red-team scan**: `duduclaw redteam` synthesizes jailbreak prompt variants
  from an agent's `CONTRACT.toml` `must_not` rules and reports which the
  deterministic input-guard catches. `crates/duduclaw-gateway/src/redteam.rs`.
- **`duduclaw backup` / `duduclaw restore`**: timestamped home archive with a
  SHA-256 sidecar verified on restore (fail-closed on mismatch).
- **`duduclaw session replay <id>`**: print a stored session's turns in order
  (with `--tools` to interleave tool-call audit lines).
- **ADR-003**: records the decision to exclude Signal / personal WeChat / Viber
  channels (`docs/adr/ADR-003-excluded-channels.md`).
- **`duduclaw gdpr export|erase <contact>`**: data-subject requests over the
  memory store. Export = a JSON bundle of every row referencing the contact (as a
  triple subject/object or a free-text mention, LIKE-escaped). Erase = a single
  transactional hard delete across `memories` + `memories_fts` + `key_facts` +
  `key_facts_fts` (no FTS orphan), recording a SHA-256-pseudonymised erasure
  tombstone. `--confirm` gates deletion (dry-run preview otherwise).
  `crates/duduclaw-memory/src/gdpr.rs`.
- **`duduclaw memory bench`**: times HippoRAG-lite Personalized-PageRank over the
  live triple count and prints P50/P95 + a partition recommendation (the LightRAG
  subgraph-partition gate — measure before building). Thresholds: ≥10k triples or
  P95 ≥50 ms. `crates/duduclaw-memory/src/bench.rs`.
- **Cross-session user profile.** Per-user preference traits stored via temporal
  supersession (`subject = "user:<id>"`), a deterministic (prompt-cache-stable)
  `## About This User` render, and reflexion-style consolidation into one durable
  `profile_summary`. `crates/duduclaw-memory/src/user_profile.rs`.
- **MCP/skill trust tiering.** `classify_trust_tier` derives an official / active
  / orphan tier from a repo's last-push age + owner type + stars; `SkillIndexEntry`
  now carries `pushed_at` / `owner_type` / `stars` / `trust_tier` so users are
  steered away from abandoned MCP servers. `crates/duduclaw-agent/src/trust_tier.rs`.
- **Secret manager: 1Password Connect + Infisical backends**, and `secret://`
  resolution wired into the MCP Bridge. `[[mcp.external]]` credentials may now be
  `secret://<backend>/<name>`, resolved at spawn time against the configured
  secret manager; an unresolvable ref drops the server fail-safe (mirrors
  `env://`). New read-only adapters (`onepassword.rs`, `infisical.rs`) with
  fail-closed `put`/`delete`.
- **`## About This User` reply injection + `user_profile_record` /
  `user_profile_get` MCP tools.** The cross-session user profile is now
  end-to-end: an agent records preference traits, they render into a
  session-stable `## About This User` block injected into that user's future
  replies (next to Past Mistakes / Learned Rules), and are readable back. Agent
  scope is the server-injected namespace, never a client param.
- **GDPR erase/export now also covers the session store.** A `duduclaw gdpr`
  request matches sessions by the `<channel>:<chat_id>` session-id prefix (and its
  threads) and hard-deletes the matching `sessions` + `session_messages`
  transactionally; export includes the session turns.
  `SessionManager::sessions_for_contact` / `erase_sessions_for_contact`.
- **Email channel (SMTP send + inbound parse).**
  `crates/duduclaw-gateway/src/email.rs`: async SMTP send via `lettre` (rustls;
  STARTTLS / implicit-TLS / plaintext), a dependency-free RFC822 inbound parser
  (`parse_inbound`, header-unfolding), and `[channels.email]` config. Doubles as a
  fail-safe alert sink. Send path is loopback-verified; the IMAP poll transport +
  gateway channel-lifecycle wiring are the documented PENDING-LIVE remainder.
- **MCP Bridge SaaS recipes** in `docs/guides/mcp-bridge.md`: per-service
  `[[mcp.external]]` config for Gmail/Calendar, Plane, Invoice Ninja, Chatwoot,
  WooCommerce (+ DocuSeal / Monica notes), each with credential provisioning and
  `approval_required_tools` guidance for write tools.

### Changed
- **`License` 新增可選 `control_url` 欄位（相容性：向後相容）。** 白牌 §10.5 讓 issuer
  金鑰自帶續期端點。此欄位 **不進 canonical_payload**（與 `signature` 同級排除，不影響簽章），
  且 `#[serde(default, skip_serializing_if="Option::is_none")]`：舊 `license.json`（無此欄位）
  讀出即 `None`，行為不變；新舊 binary 互讀相容（License 無 `deny_unknown_fields`）。控制面
  URL 解析順序改為 `DUDUCLAW_CONTROL_URL` env > `license.control_url` > 內建預設（gateway
  `license_runtime` 與 CLI `license refresh` 同步）。`resign_license_for_refresh` 保留原
  `control_url`。
- **World stage is a PixiJS 2D isometric scene, now with an immersive full-width
  `/world` page.** After live testing, the interim three.js real-3D renderer was
  dropped in favour of a 2:1 isometric PixiJS renderer (`stage-scene.ts`): a
  batched floor/walls ground layer (town bakes it via `cacheAsTexture`), a
  depth-sorted actors layer for agents + ambient cars, per-agent characters with
  always-on nameplates, head-top emotes and fading CJK-safe speech bubbles, and
  eight-colour town buildings with lit windows. The camera is 2D (no rotation):
  cursor-anchored wheel / pinch zoom (0.5×–2.5×), bounds-clamped drag pan, and a ⟲
  recenter that snaps back to the contain-fit framing. Beyond the compact 38vh
  Home band (which gains a ⤢ 展開 link), the world now has a dedicated full-bleed
  `/world` page (openhuman Tiny Place style — edge-to-edge canvas, floating ROOM
  scene panel top-right, info card top-left) reachable from the sidebar (員工/公司
  → 世界) and the Org "世界" tab (now a link so the heavy scene mounts in one
  place). CSP note: PixiJS's WebGLRenderer uses `new Function` for uniform sync,
  so the renderer imports `pixi.js/unsafe-eval` eagerly alongside `pixi.js` to run
  under the dashboard's `script-src 'self'` CSP; WebGPU is forced off
  (`preference:'webgl'`) with a 10s init timeout. pixi.js loads in a lazy chunk
  (dynamic `import()`) — the main bundle is unchanged. The state / behaviour layer
  (`useWorldState`, `SCENES`, `traffic`, degradation chain, click→route map, scene
  persistence) is unchanged.
- **Dashboard: single Home spine (workspace/dashboard shell modes removed).**
  The `ui-mode-store` toggle, `WorkspacePage`, and `ModeToggle` were dropped;
  Home is the only index and carries the one-line launcher (`PromptBar`) hero at
  the top, which hands off to `/webchat` (shared chat session). `/workspace`
  aliases to Home so old bookmarks keep working.
- **Licensing: issuer key rotated v1 → v2.** v1's private key was unaccounted-for
  on the issuing side, so v1 trust was removed from the binary and a fresh v2
  issuer keypair (private key held offline) replaces it (`PROD_ISSUER_KEY_ID = "v2"`,
  v2 public key baked). No customer impact — no v1 licenses were issued.
- **Dashboard navigation regrouped into four owner-oriented groups
  (WP14-T14.1).** 總覽 / 工作 / 團隊 / 公司, replacing the previous six groups
  (代理→團隊, 知識 folded into 工作, 整合+營運+系統 folded into 公司). **All routes
  unchanged** — only grouping and labels moved, so bookmarks keep working.
- **Dashboard terminology pass toward "AI 員工" (WP14-T14.9, partial).** zh-TW UI
  copy on the primary owner surfaces (nav labels + descriptions, AI-staff roster,
  org chart, marketplace/reliability/wiki-trust/skill-synthesis) now says
  「AI 員工」instead of Agent/代理; measure table recorded in `web/DESIGN.md` §5.
  Deeper admin/edit-dialog strings remain on the follow-up list.
- **Sessions are soft-deleted, not destroyed (WP5).** `delete_session` now
  ARCHIVES (sets `archived_at`, keeps messages, hides from normal listings) — the
  conversation stays replayable and searchable. Real, irreversible deletion is the
  new `purge_session`. `cleanup_inactive` archives inactive sessions and only
  purges them after a 90-day retention window. New `record_message_session` /
  `session_for_reply` back per-task session resume: replying to a specific bot
  message resumes that task's session (`message_session_map` table, additive).
  **Behaviour change**: anything that called `delete_session` expecting a hard
  delete now archives instead — use `purge_session` for the old behaviour.
- **LINE inbound voice messages are now transcribed** to text (mirrors the
  existing Telegram path) via `duduclaw_inference::whisper::transcribe`, folded
  into the agent's input; the saved attachment reference is retained so a
  keyless/failed transcription degrades gracefully.
- `BudgetConfig` gains `daily_cap_cents` (`#[serde(default)]`, backward-compatible).
- MCP Bridge `env` values now accept `secret://` refs in addition to `env://`
  (see Added). `SecretBackend` gains `OnePassword` / `Infisical` variants.

### Security
- **收尾波複查修正（2026-07-12）。** Live Canvas 補自包含 srcDoc CSP（`default-src 'none';
  img-src data:`）——關掉 CSS `background:url()` 的外連信標通道（沙箱已擋 script，此層擋
  網路 egress，使 Canvas 成為全離線視覺面）。
- **P1 尾輪雙鏡頭複查修正（2026-07-12）。** skill 安裝 `meta.name` 路徑穿越（掃描乾淨的
  skill 可覆寫其他 agent 的 SOUL.md——全安裝路徑單點消毒，CRITICAL）；全通道
  `handle_command` 的 `is_admin=true` hardcode（任何群組成員可 `!STOP ALL` 全域停機——改
  `admin_users` 頻道設定，fail-closed 無名單即無管理員，CRITICAL）；指令攔截點前置中央
  存取閘（pairing/allowlist 不再被繞過）；export 不再跟隨 symlink＋contract/skill 目錄
  全檔 scrub＋token 形狀擴充；LobeHub manifest SSRF 允許清單；hub 安裝暫存檔移出共享
  /tmp；agents.update 通道密鑰不再以明文複活；全通道 config 密鑰 enc-only（presence
  檢查改 `_enc`-aware，移除 TG/DC/LINE 最後三個明文例外）；`spawn_ephemeral`/
  `spawn_agent` bus 入列補跨程序檔鎖；ephemeral 32 上限防 TOCTOU；`/handoff` 跨使用者
  對話外洩（歧義即拒）。
- **雙鏡頭複查修正（2026-07-11 第二輪）。** WeCom `corpsecret`/access_token 與 DingTalk
  sessionWebhook token 不再經 reqwest 錯誤 URL 洩漏到 log/dashboard（`scrub_reqwest_err`
  全站套用）；wecom/dingtalk 密鑰不再以明文與 `_enc` 並存於 config；
  `dingtalk_sessions.json` 與 `teams_conversations.json` 改 0600 原子建檔；DingTalk
  sessionWebhook 加 `*.dingtalk.com` 錨定允許清單（sign 不覆蓋 body 的 SSRF 面）；
  WeCom 回調加 ±1h 重放窗；夜間 LLM spawn 由預設 capabilities（skip-permissions 全工具
  面）收緊為零工具 allowlist＋注入記憶 `<data>` 降格；N3/N4 夜間升格記憶 importance
  封頂 5.0＋來源 tag（重複注入內容無法壓過 curated 條目）；`complete_task`/`block` 補
  claim-holder guard（殭屍工人不能蓋掉新持有者結果）。

### Fixed
- **去識別化外部系統移除後不再靜默復活（2026-07-12）。** `redaction.update` 的
  `tool_egress` 是 upsert-merge（僅 `null` 值移除，缺席 key 不動）。`RedactionTab` 存檔原本
  只送當前 map,移除某外部系統/工具規則後該 key 只是缺席→後端不刪除→重載復活。改為存檔時
  對「先前存在但現已移除」的 key 明確送 `null`,並在載入/存檔後更新基準 key 集。
- **P1 尾輪雙鏡頭複查修正（2026-07-12）。** MoA 在派工/cron 路徑原會毒化共享帳號池
  （rotator 之前即攔截＋direct-API 正確路由＋成本入 telemetry＋對話 history 接通）；
  JitRL 回饋零 caller＋record/generate model key 不一致（已接 MCP 工具＋統一 key）；
  audit 輸入捕捉零 caller 死碼（已接中央稽核站）；curator 會封存從未被 stamp 的活躍
  skill（無使用訊號改 stale-only）；`run_at_risk` 缺席 autopilot trigger 白名單；Slack
  指令 session key 錯位（幽靈 session）；handoff 佈告 marker 順序（`/rollback` 不再能
  整段吃掉匯入歷史）；runs.get／儀表板計數補 tombstone 過濾；proactive_timing 排除已
  撤銷輪；channel_reply 兩處 CJK 不安全位元組切片；curator 巢狀配置不再每日誤報。
- **雙鏡頭複查修正（2026-07-11 第二輪）。** 依賴閘控死碼（`claimable_tasks` 零呼叫者
  ⇒ 已 fold 進 `atomic_claim` 單一交易）；外部 provider seat 不再壓掉 anthropic OAuth
  自動偵測（channel reply 全斷回歸）；API-key qwen 帳號不再被 seat 分支劫持成 503；
  wecom/dingtalk 補 `create_sender` factory arm（cron/OTP/computer-use 不再靜默丟訊息）；
  goals/depends_on 循環檢查包 IMMEDIATE txn（TOCTOU）；zombie reclaim 加
  `lease_expires_at` CAS；`tasks_claim` legacy fallback 不能偷走已指派任務；timeline
  `truncated` 旗標修正＋`taskStatus.failed` 三語補齊；cli MCP `VALID_CHANNELS` 補
  wecom/dingtalk。
- **Evolution "off" now really stops evolution (master kill-switch).** Turning
  off self-evolution previously left two bypass paths — the heartbeat
  silence-breaker and the channel prediction path never checked the toggle, so a
  "disabled" agent kept reflecting and rewriting `SOUL.md`. `[evolution] enabled`
  (default `true`, backward-compatible) is now a single master switch enforced at
  every trigger point: GVU trigger, heartbeat silence-breaker, channel prediction
  path (skill synthesis/graduation + GVU), sub-agent dispatch, and the
  skill-synthesis auto-run scheduler. Prediction-error logging (passive telemetry)
  still runs. New `duduclaw agent freeze|unfreeze <id>` one-shot enterprise
  escape hatch (also disables heartbeat, writes an audit record). Guide:
  `docs/guides/evolution-switches.md`.
- **Dashboard secret-manager backend validation drift**: the settings handler
  accepted `config` / `keychain` (backends that do not exist) and *rejected*
  `local` (the default), so any valid backend selection failed. Now validates the
  real enum: `local` / `vault` / `env` / `onepassword` / `infisical`
  (`handlers.rs`).
- **GDPR erase closes an FTS-orphan leak**: the delete cascades `key_facts_fts`
  alongside `key_facts` (the pre-existing `purge_stale_facts` path leaves the FTS
  row behind); documented as a follow-up sweep target.
- **Infisical `exists()`** no longer folds auth/network failures into
  `Ok(false)` — only a genuine 404 is `false`; other errors propagate
  (fail-closed, matching the 1Password adapter).

## [1.35.0] - 2026-07-07 — True auto-update, Ed25519-signed releases, channel UX

### Added
- **Per-channel rich rendering & live progress**: platform-native markdown
  rendering (Telegram HTML / Slack blocks / WhatsApp markup / Feishu Card 2.0 /
  Google Chat / Teams / LINE plain text, CJK-width-aware monospace tables),
  typing indicators on all channels, and a live 📋 task-progress board for
  long-running jobs (parsed from Claude `TodoWrite` events, edited in place).
- **Google Chat and Microsoft Teams channels** (JWT-verified webhooks,
  service-account / Bot Framework proactive sending) — nine channels total.
- `web_fetch` (L1) / `web_extract` (L2) MCP tools wired to the browser
  auto-routing ladder; `slack` / `access_control` modules reconnected
  (previously dead code never declared in `lib.rs`).
- **True auto-update with in-process restart**: after a self-update installs
  (dashboard "Install update" button or the 6-hourly checker with
  `[gateway] auto_update = true`), the gateway now re-execs the new binary
  after graceful shutdown (`platform::self_restart()` — `execv` on Unix keeps
  the same PID so launchd/systemd supervision is undisturbed; Windows spawns a
  detached replacement). Works for unsupervised foreground runs (npm wrapper,
  `duduclaw run`) too — previously the process just exited and stayed down.
- **minisign Ed25519 release signatures**: every release asset is signed in CI
  (`MINISIGN_SECRET_KEY`) and the updater verifies the `.minisig` against a
  public key pinned in the binary — hard fail-closed; a compromised GitHub
  release without a valid signature can no longer install. SHA-256 sidecar
  check retained as defense in depth.
- Dashboard: `system.update_installed` is now broadcast on manual installs
  too; all tabs show a "restarting" banner and auto-reload once the updated
  gateway is back, so the new embedded dashboard assets load automatically.
- `InstallMethod::Npm` detection (binary under `node_modules/`) — self-update
  supported; npm registry metadata goes stale until the next `npm i -g`.

### Changed
- README rewritten in all three languages (zh-TW / en / ja): plain-language
  opening, 5 badges, one feature-overview table with links into `docs/`,
  refreshed facts (9 channels, 5 runtimes, signature verification).

### Removed
- Dead modules `analytics.rs` / `browserbase.rs` / `input_guard.rs`
  (superseded by the v1.34 security layer; no call sites).

### Fixed
- Updater never matched real release assets: `platform_asset_suffix()` looked
  for `arm64-apple-darwin.tar.gz`-style names while CI publishes
  `duduclaw-darwin-arm64.tar.gz` — update checks always reported "no download
  for this platform".
- Windows `.sha256` sidecar was written in PowerShell `Format-List` layout the
  updater could not parse; CI now writes standard `hash  filename` and the
  parser tolerates both.
- systemd unit template: `Restart=on-failure` → `Restart=always` (self-update
  exits 0 after graceful shutdown, which `on-failure` would not relaunch).
- Linux: re-exec path is pinned before the binary is replaced
  (`/proc/self/exe` reads `… (deleted)` after the update swaps the file).
- Pairing flow lock-in bug (stale pairing lock blocked re-pairing).

## [1.34.0] - 2026-07-06 — Runtime-agnostic security reference monitor

Moves the security boundary off prompts/hooks/config and onto deterministic
choke points and OS primitives. The MCP dispatch path becomes a true reference
monitor (complete mediation, tamper-proof, verifiable) so every runtime —
Claude / Codex / Gemini / Antigravity, plus the direct-API and local-inference
tool loops — is governed by the same zero-LLM policy. Every new control is
fail-closed and (where opt-in) backward compatible. New `duduclaw-sandbox`
crate; ~90 new tests, zero workspace warnings.

### Added
- **PolicyKernel reference monitor** (`duduclaw-security`): deterministic,
  zero-LLM `evaluate()` over a parameter-level static tool policy
  (`agent.toml [capabilities] policy`, Progent-style tool+arg matcher). Canonical
  `fs_write`/`shell_exec`/`mcp_call` families give one rule uniform reach across
  runtimes; precedence Forbid > Ask > Allow > default-deny; empty policy abstains
  (backward compatible). Wired into MCP dispatch (Ask → ApprovalBroker,
  TTL-expiry = deny) and the direct-API/local tool loop (`PolicyExecutor`).
- **Egress "secret in-use"** at the shared MCP choke point: `<REDACT:…>` tokens
  restored only for whitelisted tools, everything else denied (`-32007`), results
  re-redacted — now covering stdio **and** HTTP/SSE transports uniformly.
- **Native OS process sandbox** (`duduclaw-sandbox`, opt-in
  `[capabilities] native_sandbox`): confines the spawned agent CLI via macOS
  Seatbelt (live-verified) / Linux Landlock, derived from `SandboxLevel`;
  fail-closed when required but unavailable.
- **Origin-bound memory trust** (`duduclaw-memory`): temporal memories gain
  `origin`/`origin_trust`/`derived_from`; a derived fact's trust is clamped to
  ≤ min(source trusts) — non-malleable, can't be laundered upward. Distilled
  conversational facts are marked lowest-trust; search down-weights accordingly.
- **CONTRACT.toml runtime enforcement**: `must_not` boundaries validated on the
  final user-facing reply bytes (after secret restoration); violations blocked
  and audited.
- **SecurityPosture** state machine ({Green,Yellow,Red}, escalate-fast /
  decay-slow) + **OS ground-truth reconciliation** (`os_reconcile`: pure
  two-way diff of tool-call claims vs observed OS effects; macOS `eslogger`
  parser; Linux eBPF staged).

### Changed
- MCP dispatch pipeline now runs injection scan → PolicyKernel → egress at one
  shared choke point, so HTTP/SSE get the same enforcement as stdio.
- Antigravity spawn derives `--sandbox` / `--dangerously-skip-permissions` from
  `SandboxLevel` (never both) instead of unconditional skip-permissions.

### Fixed
- Egress domain filtering is fail-closed: an empty/invalid allowlist now denies
  all egress (`--network=none` / deny-all) instead of leaving the network
  unfiltered; allowlist entries are canonicalized (reject control bytes, `%`,
  CRLF, IP-literals).
- Inbound prompt-injection blocks on the channel reply path are now audited.


## [1.33.0] - 2026-07-05 — Model-agnostic core + AI harness infrastructure

The largest structural release to date, in three movements: a deep prune
(−19k lines of provably-orphaned code), a research-driven upgrade pass over
memory/routing, and the promotion of "Multi-Runtime" from a text shell into a
genuinely model-agnostic platform with 2026 harness table stakes. Net diff vs
v1.32.0 is roughly line-neutral (+20k/−21k) — redundancy traded for
infrastructure. 53 test suites green, zero workspace warnings.

### Added
- **`duduclaw-llm` crate — unified provider layer**: one normalized
  `ChatRequest`/`ContentPart`/`StreamEvent`/`NormalizedUsage` shape over four
  native protocols — Anthropic Messages (layered `cache_control`, thinking
  replay), OpenAI **Responses API**, Gemini `generateContent`
  (`thoughtSignature` echoed verbatim), OpenAI-compat chat/completions
  (8 presets + local llamafile/vLLM/Ollama). Real SSE on all four. Ten-way
  `LlmError` classification, `ModelRegistry` with vendored 2026 prices
  (millicents/MTok, price cliffs, cache rates, `~/.duduclaw/models.toml`
  override), `FallbackRouter` with per-(provider,model) cooldowns and
  context-window-aware candidate filtering.
- **MCP client + agentic tool loop** (`duduclaw-llm`): stdio JSON-RPC MCP
  client + provider-agnostic `run_tool_loop`, so the direct-API and
  local-inference paths finally get the full MCP tool surface (previously
  CLI-backends-only). Local models (llamafile/Exo/vLLM) become first-class
  tooled backends via a gateway `LocalChatProvider` adapter;
  `inference_mode = "local"` is now honored on the channel-reply path
  (local-first with CLI fallback).
- **Cross-provider fallback + rotation on the hot path**: `agent.toml
  [model] fallbacks = ["openai/gpt-5.4", "compat:deepseek/…"]` failover
  chain; `AccountRotator` generalized with per-account `provider` —
  multi-account, budget and cooldown machinery now applies to
  openai/gemini/deepseek/… (env-var fallback when unconfigured); optional
  OS-keychain master-key storage (`keychain` feature).
- **Harness infrastructure**: OpenTelemetry GenAI tracing
  (`invoke_agent`/`chat`/`execute_tool` spans per `gen_ai.*` semconv,
  OTLP/gRPC export with auth headers, opt-in `otel` feature, default OFF);
  `duduclaw eval` behavioral regression suite (golden-task TOML, live +
  replay modes, deterministic tool/regex assertions + optional LLM judge,
  CI-gateable); universal HITL `ApprovalBroker` (SQLite, TTL expiry = deny,
  wired into autopilot `require_approval`); A2A v1.0 Agent Card
  (`/.well-known/agent-card.json`) with a real `message/send` that enqueues
  onto the dispatcher bus and honest `submitted`/`working`/`completed`
  states.
- **Memory/routing research upgrades** (2024–2026 literature pass):
  Ebbinghaus retrievability (`R = exp(-t/S)`) for retrieval ranking and
  decay; HippoRAG-lite Personalized PageRank over the v1.19 SPO triple
  graph (multi-hop recall, supersession-aware); ACE/ExpeL rule lifecycle
  (helpful/harmful counters, net-zero rules retired); calibrated cascade
  routing (post-hoc logprob acceptance, opt-in); summarized-failure retry
  (context decontamination on Timeout/EmptyResponse); layered Direct-API
  cache breakpoints (`CACHE_SPLIT_MARKER`) + per-block invalidation
  attribution.
- **Non-Claude runtime parity**: `RuntimeContext` carries
  `CapabilitiesConfig`; codex/gemini spawn with capability-derived sandbox
  flags (no more blanket `--full-auto`/`yolo`); PTY-pooled sessions inject
  per-agent `--allowedTools`/`--disallowedTools` (previously zero
  restrictions reached the pool); codex/gemini/agy auto-register the
  duduclaw MCP server in their native configs; `agent create --runtime`
  scaffolds AGENTS.md/GEMINI.md and rejects typo'd providers.
- **Wiki ↔ memory boundary**: conversation distillation now persists to
  temporal memory (supersession) instead of wiki pages; session-stable wiki
  injection (15-min pinned selection, prompt-cache friendly); wiki/memory
  injection dedup (wiki wins); `.scope.toml` `knowledge_owner =
  "wiki"|"memory"` deterministic conflict arbitration.

### Changed
- CostTelemetry prices per model via the registry — non-Anthropic usage was
  previously billed at hardcoded Claude Sonnet rates (DeepSeek overbilled
  ~30×); price-cliff warnings now model-aware.
- `try_direct_api` routes by registry-resolved provider (OpenAI → Responses,
  Gemini → native, compat presets); Anthropic path byte-identical.
- Dashboard model suggestions follow installed runtimes; provider↔model
  mismatch (`preferred = "gpt-5"` on the Claude path) warns once with
  guidance.

### Removed (deep prune, −19k lines)
- Orphaned crates never linked by any binary: `duduclaw-governance`,
  `duduclaw-durability`, `duduclaw-bus`, `duduclaw-bridge`.
- Unwired gateway modules (AFM compression, stale `tool_classifier`,
  `experiment/`, `sticker/`), the three-strategy inference compressor
  (Meta-Token/LLMLingua-2/StreamingLLM — duplicated the live
  `prompt_compression.rs` pipeline; `compress_text`/`decompress_text` MCP
  tools retired), and the unwired voice subsystem (asr/vad/deepgram/
  sensevoice/livekit; `whisper.rs` + `embedding.rs` kept) — drops
  symphonia/livekit/tokio-tungstenite deps.
- Both crate-wide `#![allow(dead_code)]` suppressions; everything the
  compiler then surfaced (−1.3k lines) plus the 8 remaining pre-existing
  warnings — the workspace now builds with **zero warnings** including
  `--all-targets`.

### Security
- Fixed: non-Claude runtimes ran with zero tool-capability enforcement
  (blanket permission-bypass flags + PTY pool passing no restrictions).
  Capability enforcement is now fail-closed across every runtime path, with
  structured warnings where a CLI offers no enforcement mechanism
  (antigravity).


## [1.32.0] - 2026-07-03 — Dashboard UX: command palette, self-explanatory nav, mobile shell

A deep UX pass on the Calm Glass dashboard, driven by a full-page UX audit. The
headline is a **command palette (⌘K / Ctrl+K)** — the Raycast-aligned answer to
moving through a 37-page console without scrolling-and-hunting — plus a nav that
explains itself and a shell that finally works on a phone. Frontend-only; no
backend RPC / WS protocol change.

### Added
- **Command palette (⌘K / Ctrl+K)** — `components/CommandPalette.tsx`, mounted
  once in `MainLayout`. Dependency-free fuzzy search (`lib/fuzzy.ts`, CJK-safe,
  Latin aliases derived from each `nav.*` id + the localized description) across
  every role/edition-gated nav route plus quick actions (switch theme, language,
  workspace⇄dashboard shell, logout). Empty query surfaces recently-visited
  routes (`stores/command-palette-store.ts`, persisted MRU). ARIA
  combobox+listbox, arrow/Enter/Esc keyboard nav, match highlighting. The Header
  gains a discoverable `Search… ⌘K` trigger (⌘ on macOS, Ctrl elsewhere).
- **Self-explanatory sidebar** — every nav item now shows a one-line description
  under its label (and as a searchable subtitle in the palette) so functions are
  clear without guessing from the icon. `NavItem.desc` added to `nav-model.ts`
  for all 27 items, localized in zh-TW / en / ja-JP.
- **Shared loading primitives** — `ui/Skeleton.tsx` (`Skeleton` / `SkeletonList`,
  `role="status" aria-busy`, reduced-motion safe), applied to the Dashboard task
  columns in place of an ad-hoc pulse; and a `Button` `pending` prop (swaps the
  leading icon for a spinner, disables, sets `aria-busy`) for async submits.

### Changed
- **Mobile shell** — below `md` the dashboard sidebar is now an off-canvas drawer
  (`stores/sidebar-store.ts`) toggled by a Header hamburger and dismissed on
  navigation or backdrop tap; at `md`+ it stays a static column (no change on
  desktop). Header padding tightens to `px-4` on small screens.
- **DESIGN.md** documents the command palette, mobile drawer, `Skeleton`, and
  `Button.pending` as first-class patterns of the Calm Glass system.

### Testing
- `tsc -b` clean, `vite build` green, **98 web unit tests pass** (+18 new: 13 for
  the fuzzy matcher, 5 for the palette store's MRU de-dup/cap/persist).



## [1.31.0] - 2026-06-30 — Workspace shell + desktop lifecycle hardening

Ships the **Genspark-style 工作空間 (Workspace) 外殼** — a consumer-grade landing
layer (central prompt bar + capability launcher grid + "Claw, your first AI
employee" entry) layered on top of the existing Calm Glass power-user dashboard,
with a simple ⇄ advanced mode toggle. The full power dashboard is untouched and
remains the default for enterprise / existing users. No backend RPC / WS protocol
change — the workspace is purely a new frontend assembly over the existing
`/ws/chat` + stores. See `docs/todo/TODO-genspark-workspace-shell.md`.

Also lands the **Tauri 2 desktop scaffold** (Phase D) — a native window that wraps
the `duduclaw` gateway as a sidecar — and this release hardens its lifecycle.

### Added
- **Workspace shell (web)**: `WorkspacePage` (Hero + PromptBar + LauncherGrid),
  reusable `chat/` components (`MessageBubble` / `TypingIndicator`) shared with
  WebChat, `AgentModelPicker` / `ConnectorChips`, the Claw value-prop section, a
  `ui-mode-store` (workspace/dashboard, persisted, personal-edition default), and
  a Header `ModeToggle`. Full zh-TW / en / ja-JP i18n, a11y, and unit tests.
- **Desktop mode override (§D1)**: `DUDUCLAW_DESKTOP_MODE=auto|attach|spawn`
  controls whether the desktop shell attaches to an externally-managed gateway
  (launchd / CLI), always spawns its own sidecar, or auto-decides (default).
  Replaces the unbuilt settings-panel toggle with a testable env override.
- **`scripts/desktop/gen-icons.sh`**: generates the app icon set via
  `cargo tauri icon`, with a `sips` / `iconutil` fallback on macOS (PNGs + .icns;
  warns that the Windows `.ico` still needs the Tauri CLI / ImageMagick).

### Fixed / Changed
- **config.toml port priority (§D2.2)**: the desktop sidecar now resolves the
  gateway port as `DUDUCLAW_PORT` env > `~/.duduclaw/config.toml [gateway] port` >
  default `18789`, respecting the operator's persisted choice when the env var is
  absent.
- **Double-instance avoidance (§D1/§D2.2)**: attach-detection now probes *every*
  known port (env / config.toml / default), so a non-default `config.toml` port
  can no longer make the desktop app miss — and double-spawn over — a gateway
  already running on the default port. The whole attach-vs-spawn decision matrix
  is unit-tested via an injectable liveness probe (`decide_plan`).


## [1.30.1] - 2026-06-30 — LINE replies actually send

### Fixed
- **LINE webhook reply delivery**: `line_webhook_handler` processed each event
  inline and only returned 200 after the model reply was generated and sent.
  LINE times out a slow webhook response and the `reply_token` is short-lived, so
  when LINE disconnected the handler future (and the in-flight reply) was
  cancelled — the bot was read but never replied ("已讀沒回應"). The reply now runs
  in a detached task and the webhook returns 200 immediately.

> Operational note: the interactive-REPL PTY path (`[runtime] pty_pool_enabled =
> true`) can hang on its boot/sentinel dance in a headless container; OAuth
> **setup-token** accounts are reliably served by the legacy `claude -p` path
> (`pty_pool_enabled = false`), which is the recommended setting for them.

## [1.30.0] - 2026-06-30 — One-click login → working agent, end to end

Makes the dashboard **Claude 一鍵登入** flow actually produce a usable account,
and fixes the LINE channel + per-account PTY auth so an agent can reply.

### Fixed
- **One-click login UX** (`cli_auth`): the OAuth authorize URL is surfaced as a
  clickable button (PTY widened to 600 cols so it stays on one line); the ANSI
  console is de-garbled; the pasted code is submitted by sending Enter as a
  **separate** keystroke after the paste (Ink swallowed a CR that arrived in the
  same write, so the code never submitted); success/failure are detected through
  the Ink TUI's escape-separated words.
- **OAuth token capture**: `claude setup-token` only prints its long-lived token
  once — it's now scraped and registered as an account. The ANSI parser was
  rewritten to a correct CSI/OSC state machine (CSI ends on a 0x40–0x7E byte, not
  "the first letter"); the old one dropped a character from the token
  (`sk-ant-oat01-…` → `sk-ant-at01-…`), producing a 401 on every reply.
- **PTY binary resolution**: the PTY/OAuth reply path now falls back to a PATH
  lookup (`which_claude`), not just the HOME candidate list — the Docker image
  installs the CLI in `/usr/bin`, which the curated list omitted ("binary not
  found").
- **PTY pool per-account auth (HIGH-2, was deferred)**: the in-process pool now
  injects the rotator-resolved per-account credential env
  (`CLAUDE_CODE_OAUTH_TOKEN` / `CLAUDE_CONFIG_DIR` / `ANTHROPIC_API_KEY`) at spawn
  time via an account-keyed side-channel. Previously the spawned CLI used
  whatever ambient OAuth lived in `~/.claude/`, so a registered account never
  authenticated.
- **LINE channel save**: choosing an agent no longer makes the save fail —
  LINE/WhatsApp/Feishu are single global webhook endpoints, so they persist to
  the global `[channels]` and bind the selected agent as `[general] default_agent`
  instead of erroring "Per-agent channels not supported for: line".
- **LINE webhook live**: `/webhook/line` is now **always mounted** and the handler
  reads the token/secret per request, so configuring LINE in the dashboard takes
  effect with no gateway restart (previously: 405 on Verify + status stuck on
  "連線中"). Status refreshes live on save.
- **Account list refresh**: a one-click login invalidates the rotator cache so the
  new account shows immediately instead of after the 5-minute TTL.
- **Self-service password change**: a new "帳號安全" Settings tab lets the
  single-owner edition rotate the dashboard admin password from the UI.

### Ops
- `commercial/gateway-vm`: `CLOUD_BUILD=1 ./deploy.sh` builds the image remotely
  (no local Docker); the VM deploy prunes old images afterwards to stop the 30GB
  boot disk filling (which crash-looped the gateway with SQLite "disk I/O error").
- Repo-root `.gcloudignore` keeps the Cloud Build context to source only.

## [1.29.1] - 2026-06-28 — Fix placeholder domain

Replaces the never-registered placeholder `duduclaw.tw` with the real deployed
domains.

### Fixed
- **`DEFAULT_CONTROL_URL`** (CLI + gateway) → `https://api.duduclaw.dudustudio.monster`.
  `duduclaw license refresh / redeem / rebind` and the gateway phone-home now
  reach the real control-plane by default (still overridable via
  `DUDUCLAW_CONTROL_URL`).
- Dashboard pricing links + upsell strings (premium templates / wizard /
  tier-limit message) → `https://duduclaw.dudustudio.monster#pricing`.
- Contacts: security SOP → `louis.li@dudustudio.monster`; support / refund →
  `info@dudustudio.monster`. Marketing drafts point at the real domain.

## [1.29.0] - 2026-06-27 — Cloud-tier agent/channel caps

Enforces the per-tier `max_agents` / `max_channels` from `features.toml` that
were declared but never actually applied — so the free/entry tiers (Hobby
1 agent/1 channel, Solo 1/2, Studio 3/5) now hold. **Self-host is never
capped** (Apache 2.0 promise); the limit only applies to managed Cloud tenants.

### Added
- **Cloud-tier resource caps** — `agents.create` and `channels.add` reject once
  the active tier's cap is reached, with an upgrade message. Gated on
  `DUDUCLAW_DEPLOYMENT=cloud` (set only inside managed tenant containers); a
  self-hosted deployment (the default) is never limited, and `max_* = 0` in
  `features.toml` also means unlimited.
- **Soft-limit banner** now shows concrete usage (`Agents X/Y · Channels A/B`)
  and an upgrade CTA when a personal cloud tenant reaches its plan limit —
  non-blocking, dismissible.
- `license_runtime::cap_exceeded()` pure helper + `is_self_host_deployment()`
  exposed for the gateway to query.

### Notes
- The free-tier limit text mirrors `features.toml`; the gate is enforced
  server-side regardless of the dashboard hint.

## [1.28.0] - 2026-06-27 — Partner (NFR) licenses + license self-service

Adds a free **Partner (NFR — Not For Resale)** license path and closes the
remaining license-acquisition gaps: emailed keys for every issuance, machine
re-binding, remote subscription status, and deployment-mode enforcement. The
managed/Cloud purchase flow was already end-to-end; this release makes the
self-host and partner paths first-class.

### Added
- **Partner (NFR) tier** (`LicenseTier::Partner`, `[partner]` in
  `features.toml`) — a free, self-host, non-resellable grant for integration /
  channel partners. Unlocks the same commercial modules as Self-Host Pro
  **except** white-label / redistribution. Independently revocable; never sold
  through checkout (price 0).
- **Partner code redemption** (free path) — `POST /v1/partner/redeem` exchanges
  a code + machine fingerprint for a signed partner license (atomic one-use
  reservation, `max_uses` enforced, best-effort email); `POST /v1/partner/codes`
  (admin) mints codes. CLI: `duduclaw license redeem <code>`.
- **CLI self-service** — `duduclaw license redeem` / `rebind` / `subscriptions`
  (redeem a free partner code, move a license to this machine, check remote
  renewal status).
- **License key email on every issuance** (Gap) — `POST /v1/license/issue` now
  emails the key when an `email` is supplied (previously only the PayUni
  webhook did), so admin / self-host issuance no longer needs a manual send.
- **Self-service machine rebind** (Gap) — `POST /v1/license/rebind` re-signs a
  license for a new fingerprint, ownership proven by the current fingerprint
  (atomic, no operator round-trip).
- **Remote subscription status** (Gap) — `POST /v1/license/status` (self, by
  fingerprint) + `GET /v1/license/subscriptions` (admin, by customer).
- **Deployment-mode binding** (M51) — the gateway now enforces tier ↔
  deployment via `DUDUCLAW_DEPLOYMENT` (`cloud` vs self-host, default
  self-host): cloud-only tiers are refused on self-host and vice-versa,
  fail-closed to OpenSource.

### Notes
- Self-host paid checkout (PersonalProSelfHost / SelfHostPro) and the PayUni
  **sandbox toggle** were already wired; only live sandbox e2e remains, gated
  on a PayUni merchant account.
- `keygen` learns `--tier partner` / `--tier personal_pro_self_host`.

## [1.27.0] - 2026-06-27 — Industry templates (Pro) + license-gated wizard unlock

Ships four research-backed **premium industry templates** for Taiwan SMB
verticals and wires the previously-missing **"unlock" path** so the
`premium_templates` license feature actually surfaces them in the setup wizard
— fail-closed, so the public OSS binary and unlicensed users never receive the
closed content.

### Added
- **Premium industry templates** (Pro / Studio / SelfHostPro /
  PersonalProSelfHost / OEM) — `ecommerce` / `clinic` / `realestate` /
  `education`, each a full kit (SOUL.md + compliance-hardened CONTRACT.toml +
  vertical-tuned agent.toml + FAQ.json + glossary / SOP / compliance wiki) with
  cited Taiwan statutes (消保法 / 醫療法 / 不動產經紀業管理條例 /
  補習及進修教育法). Closed-source; shipped only in licensed builds.
- **License-gated template unlock** (`duduclaw-cli` `premium_templates` module)
  — `premium_unlocked()` / `find_premium_templates_dir()` /
  `available_premium_industries()` / `resolve_premium_template()`. Fail-closed:
  a missing / expired license, the OpenSource tier, an absent template tree, or
  an unsafe slug all resolve to *locked*; the slug is validated against path
  traversal before any filesystem access.
- **Wizard premium industries** — `duduclaw wizard` appends unlocked premium
  verticals to the industry menu, and shows a one-line upsell hint
  (`🔒 … 需 Pro 授權`) when the templates are present on disk but the license is
  locked.

### Notes
- `priority_security_patch` remains a support-SLA value-add (tier display
  only), not a code gate, by design.

## [1.26.0] - 2026-06-27 — Personal / Enterprise editions + one-click CLI login

Introduces an explicit **product form-factor** dimension (Personal vs
Enterprise) that is orthogonal to the license tier and **never gates a core
feature** — it only changes defaults and which management surfaces the
dashboard shows. Adds a **Dashboard one-click login** for every AI CLI, bundles
the Antigravity CLI in the server image, and ships personal-edition data
portability.

### Added
- **Personal / Enterprise editions** (`EditionProfile` in `duduclaw-core`):
  - `Personal` (default) = single-owner, zero-config; `Enterprise` = multi-seat
    / compliance management surfaces. Resolution precedence:
    `DUDUCLAW_EDITION` env > `agent.toml [edition]` > license tier > `Personal`.
  - Gateway resolves it per request and returns `edition_profile` on
    `system.status` / `system.version`.
  - Dashboard reads it to hide enterprise nav (org / users / governance /
    partner / wiki-trust) on Personal, shows an **EditionBadge**, and a
    non-blocking **soft-limit banner** near a plan's agent/channel limit.
- **`PersonalProSelfHost` license tier** — the individual-developer self-host
  tier (NT$490/mo or NT$4,900/yr): unlocks premium templates + priority patches
  without the enterprise modules.
- **Dashboard one-click CLI login** (`auth.cli_login.*`): drives each CLI's
  native login (Claude / Codex / Gemini / Antigravity) in a PTY on the gateway,
  streams the output to a dashboard terminal, and relays the user's pasted code
  back. Flags `remote_safe` per CLI (paste-back vs localhost-callback). Reachable
  from the Accounts page. Claude `setup-token` flow verified end-to-end.
- **Antigravity CLI (`agy`) bundled** in `container/Dockerfile.server` alongside
  claude / codex / gemini (Google's official installer), so the Antigravity
  runtime works out of the box.
- **Personal-edition data portability**: `duduclaw export` / `duduclaw import`
  package `~/.duduclaw/` as a portable `.tar.gz` (agents / memory / config /
  license; skips models / logs / backups) to move between machines or switch
  self-host ↔ managed. Guide: `docs/guides/personal-edition-portability.md`.

### Changed
- `.dockerignore` added at the repo root (keeps the build context small).

### Tests
- `duduclaw-core` EditionProfile (7), `duduclaw-license` tier (83),
  `duduclaw-gateway` cli_auth (6) + full suite, `duduclaw-cli` portability (3);
  web `tsc` + `vitest` (44). Live `docker run` smoke confirms
  `edition_profile` resolves from `DUDUCLAW_EDITION` in a real container.


## [1.25.0] - 2026-06-26 — Browser-first onboarding + guided product tour

First-run setup moves out of the terminal and into the dashboard. A fresh
install now boots straight into a warm, friendly setup flow ("👋 開始建立第一個
Agent 吧") and, after the first agent is created, offers a skippable guided
tour of the key pages. The `duduclaw onboard` CLI wizard is kept but
soft-deprecated.

### Added
- **Dashboard first-run wizard** (`WelcomePage`, `/welcome`): a 3-step flow —
  welcome → choose AI backend → name the agent. The AI-backend step covers
  five paths, each mapped to the right config via existing RPCs:
  - **Claude subscription** (OAuth) — `inference_mode=hybrid`, shows detected
    login status.
  - **Claude API key** — `accounts.add` + `api_mode=direct`.
  - **Generic API (OpenAI-compatible)** — any OpenAI-compatible endpoint
    (OpenAI / vLLM / Ollama / llamafile / Exo …) via `runtime=openai_compat` +
    `inference.update`.
  - **Local model (offline)** — `inference_mode=local` + `[model.local]`.
  - **Other CLI** — Codex / Gemini / Antigravity runtime.
- **`FirstRunGate`**: installs with zero agents are routed to `/welcome`
  automatically (loop-safe; a new agents-store `loaded` flag prevents a
  redirect flash before the first agent list resolves).
- **Guided product tour** (`GuidedTour`, lightweight self-built spotlight, no
  new deps): offered after the first agent is created, walks the user through
  the important pages, skippable any time (Esc), shown once per user
  (localStorage). Replayable from Settings → General. Sidebar links carry
  `data-tour` anchors.
- **`runtime.detect` RPC**: reports which AI runtime CLIs are installed
  (claude / codex / gemini / antigravity) plus Claude OAuth status — presence
  only, no secrets — driving the backend picker's "detected / not installed"
  badges.
- **`duduclaw_core::write_minimal_config`**: writes a bootable minimal
  `config.toml` (`[general]` + `[gateway]`) atomically.
- Empty-state CTA on the Agents page ("create your first agent").
- `welcome.*` / `tour.*` i18n strings across zh-TW / en / ja-JP.

### Changed
- **Gateway boots without a config**: `duduclaw run` on a fresh install now
  auto-writes a minimal config and starts straight into the dashboard instead
  of hard-stopping with "run `duduclaw onboard` first". The CLI `onboard`
  wizard remains for headless/advanced use but prints a soft-deprecation hint.

### Fixed
- **`agents.create` now honors the `soul` parameter** (it was silently
  dropped) and writes the `[runtime]` section at create time, so the dashboard
  can set an agent's persona and backend in one call.



## [1.24.0] - 2026-06-25 — Antigravity CLI (`agy`) runtime; PtyPool unbound from Claude

Google retired the personal-tier Gemini CLI on 2026-06-18 in favour of the
**Antigravity CLI** (`agy`). This release adds `agy` as a first-class multi-runtime
backend and unbinds the PtyPool / cli-worker layer from a hardcoded Claude so all
CLI kinds have real call points.

### Added
- **Antigravity (`agy`) runtime** (`RuntimeType::Antigravity`, `runtime/antigravity.rs`).
  Driven via oneshot `agy -p --dangerously-skip-permissions --print-timeout 300s`
  (`--model` / `--add-dir` when set), verified end-to-end against the real binary.
  - Binary auto-resolve (PATH → `~/.local/bin/agy`); system prompt + conversation
    history embedded in the prompt argument (agy has no `--system` flag); CJK-safe
    truncation; token usage estimated via the shared heuristic (print mode exposes
    no stats). Auth via `ANTIGRAVITY_API_KEY`; MCP config at
    `~/.gemini/antigravity-cli/settings.json`.
  - Pre-seeds the agent dir into agy's `trustedWorkspaces` (under a cross-process
    lock) so the interactive trust prompt never hangs a headless subprocess.
  - Registry auto-detection, vision-capability gating, and `[runtime] provider`
    validation all recognise `antigravity` (alias `agy`).
- **Per-CLI binary discovery** in `duduclaw-core`: generic `which_cli` /
  `which_cli_in_home` plus `which_codex` / `which_gemini` / `which_agy`.
- Docs: `docs/todo/TODO-antigravity-cli-migration.md`, development-guide §1.4
  (Multi-Runtime), and `[runtime]` examples in all agent.toml templates.

### Changed
- **PtyPool / cli-worker unbound from Claude.** `CliKind::Antigravity` added;
  `resolve_program` and the worker's `spawn_session_default` now resolve all four
  CliKinds (Codex/Gemini/Antigravity no longer return `None`/reject). New
  `cli_kind_for_provider()` derives the PtyPool kind from the agent's
  `[runtime] provider`, replacing the two hardcoded `CliKind::Claude` acquire sites
  (`claude_runner`, `channel_reply`).
- The interactive PtyPool REPL remains Claude-only by design: non-Claude providers
  route through the oneshot `runtime_dispatch` path. Reconnaissance showed agy's
  full-screen alt-screen TUI plus the missing system-prompt flag make the sentinel
  protocol a poor and unnecessary fit (`agy -p` already works); decision recorded
  in the migration TODO.

### Notes
- The legacy `gemini` CLI backend is retained for paid `GEMINI_API_KEY` / enterprise
  users, whose access continues past the 2026-06-18 personal-tier shutdown.



## [1.23.0] - 2026-06-22 — Decision Continuity (RFC-24): durable cross-session decisions

### Added
- **Decision Continuity (RFC-24).** When an agent offers the user an enumerated
  choice ("方案 A/B/C", "Option 1/2"), each option is now persisted into the
  Temporal Memory **semantic** layer — independent of session turns and untouched
  by `compress()` — and still-open decisions are re-injected into the next turn's
  prompt. A later "用方案 C" (new turn / session / process) resolves from durable
  state instead of being guessed from unrelated history. Opt-in per agent via
  `agent.toml [memory] decision_continuity = true` (default off).
  - **Detection** is deterministic and zero-LLM on the main path (方案/選項/Option
    /bare letter·digit/emoji keycap markers, conservative homogeneity + keyword
    gates). A suspected-but-unparsable choice (e.g. 甲/乙/丙, ①②) triggers a
    single background Haiku second-pass; plain prose never does.
  - **Resolution**: `decision_resolve` / `decision_list` MCP tools, plus
    auto-resolution when the user references an open option. Resolving supersedes
    the decision's status, records the choice as a long-lived semantic fact, and
    expires the option artifacts (fail-closed on unknown id / key / owner).
  - **Anti-guessing**: referencing a decision with no durable record records an
    F2 Reflexion learning signal so the agent learns to acknowledge the gap and
    query rather than fabricate.
  - **Lifecycle & ops**: per-agent TTL (`[memory] decision_ttl_days`, default 7)
    self-prunes stale open decisions; Dashboard "待決事項" panel with
    `decisions.list` / `decisions.dismiss` RPC (dismiss marks a false positive);
    Prometheus `decision_captured/resolved/expired/false_positive` counters;
    `scripts/smoke-decision-continuity.sh`.
  - Design: `docs/rfc/RFC-24-decision-continuity.md`; tracking:
    `docs/todo/TODO-rfc24-decision-continuity.md`.



## [1.22.1] - 2026-06-21 — Core gateway drops the Python runtime dependency

### Changed
- **Skill vetting is now Rust-native.** The dashboard `skills.vet` path no longer
  shells out to `python3 -m duduclaw.evolution.run`; it uses
  `skill_lifecycle::security_scanner::scan_skill` — the same scanner already
  backing the MCP `skill_security_scan` tool and the sandbox-trial gate, so the
  dashboard, agents, and lifecycle pipeline share one verdict.
- **Channel delegate / fallback is now Rust-native.** The `channel_reply`
  3rd-tier fallback and `agents.delegate` (wait=true) call
  `direct_api::call_direct_api` (new `call_direct_api_delegate`) instead of the
  `duduclaw.sdk.chat` Python subprocess.
- **The core gateway/CLI installed via npm/Homebrew now has no Python runtime
  dependency.** `pip install duduclaw` is optional — the `duduclaw` PyPI package
  is a standalone importable library only. Advanced local inference
  (MLX / LLMLingua-2) still depends on the separate `mlx_lm` / `llmlingua` ML
  packages, not on `duduclaw`. Docs updated across README (zh/en/ja),
  ARCHITECTURE, overview, evolution-engine, docker, and feature docs.

### Removed
- Deleted `gateway/src/evolution.rs` (its sole content was the Python vet
  bridge), the dead `vet_skill_native` fallback (which used non-compliant
  unanchored `contains`), and the `call_python_sdk_v2` / `find_python_path`
  helpers in `channel_reply`.



## [1.22.0] - 2026-06-21 — RFC-26 Live Forking · skill-synthesis scheduler · Calm Glass dashboard

Inspired by [vstorm-co/pydantic-deepagents](https://github.com/vstorm-co/pydantic-deepagents),
this adds **Live Run Forking** — split an in-flight agent task into N competing
branches that explore different strategies in isolated copy-on-write workspaces,
then let an AI judge pick the winner. **Default off**; per-agent opt-in via
`agent.toml [fork] enabled = true`. See `docs/rfc/RFC-26-deep-agents-alignment.md` and
`docs/todo/TODO-rfc26-live-forking.md`.

### Added
- **New crate `duduclaw-fork`** — the forking engine: `Branch`/`BranchState`,
  copy-on-write `BranchOverlay` (reads fall through to parent, writes stay local,
  `promote()` merges the winner), per-branch + aggregate `budget::Pool`,
  `ForkController` over a decoupled `BranchExecutor` trait, a `JudgeAgent` with the
  deep-agents confidence formula (`quality·0.4 + test_pass·0.4 + consistency·0.2`),
  4 merge modes (`manual`/`auto`/`auto_with_fallback`/`vote`), and a `test_runner`
  that scores branches by their configured test command. (49 unit tests.)
- **6 MCP tools** gated by the new `Scope::ForkExecute` + the `[fork] enabled`
  toggle: `fork_run`, `inspect_branches`, `diff_branches`, `merge_or_select`,
  `terminate_branch`, `fork_cost` (`crates/duduclaw-cli/src/mcp_fork.rs`).
- **`RotatingBranchExecutor`** — runs each branch through the `AccountRotator`
  + a real `claude` spawner, enforcing per-branch and aggregate USD budgets; forks
  execute in a **background** task so the MCP stdio loop never blocks. Branch
  outcomes + spend recorded to `~/.duduclaw/fork_history.jsonl` (advisory-locked)
  with in-process `FORK_METRICS` counters (`crates/duduclaw-cli/src/mcp_fork_exec.rs`).
- **Checkpoint fork/rewind** (`duduclaw-durability`) — `fork(checkpoint_id, new_task)`
  copies state under a new lineage, `rewind(task, checkpoint_id)` restores an earlier
  snapshot, `Checkpoint.parent_checkpoint_id` tracks lineage; id-addressable archive.
- **Smoke harness** `scripts/smoke-fork.{sh,ps1}`.

### Added (round 2 — cross-process + parity follow-ups)
- **Shared SQLite fork store** (`duduclaw-fork::ForkStore`, WAL at `~/.duduclaw/fork_store.db`) — the
  cross-process source of truth. `mcp_fork`/`mcp_fork_exec` refactored onto it.
- **Gateway `/metrics`** emits `duduclaw_fork_*` lines (read from the store at scrape time).
- **Dashboard ForkPage** (`web/`) + `fork.list/inspect/resolve` WebSocket RPC — list forks, compare
  branches side by side, see the judge's winner, resolve manually. New `/forks` route + nav.
- **`memory_improve`** MCP tool — clusters memories by tag into a propose-not-apply reflection scaffold.
- **`plan_start`** MCP tool (Plan Mode) — clarify-first planning scaffold, `agent.toml [planner]` toggle.
- **Built-in skills** — `code-review`/`refactor`/`test-writer`/`git-workflow` seeded idempotently into
  every new agent's `SKILLS/` at creation.
- **Checkpoint durability** — `CheckpointManager::with_persistence` SQLite backend; fork/rewind/lineage
  survive restart. **Task Board** — `claim_task` (atomic CAS) + parent-cycle detection.
- **Fork executor hardening** — branches capped to distinct available accounts (logged); pre-spawn
  cancellation registry for `terminate_branch`.

### Added (round 3 — the last deferred items)
- **Native copy-on-write overlay** — `BranchOverlay` clones the parent workspace via `clonefile(2)`
  (`cp -c`, macOS/APFS) or `cp --reflink` (Linux btrfs/XFS); `detect_backend()` probes once and falls
  back to the snapshot copy if unavailable.
- **Streaming budget enforcement + external SIGKILL** — `ClaudeCliSpawner` streams stream-json, charges
  `total_cost_usd` incrementally, and kills the child mid-stream on per-branch overspend
  (`SpawnOutcome::BudgetExceeded`); a per-branch kill-switch registry lets `terminate_branch` SIGKILL an
  in-flight subprocess (`→ Terminated`).
- **Activity-Feed mirroring** — fork resolutions write a `fork_resolved` row into the gateway's
  cross-process `activity` table (`<home>/tasks.db`), surfacing on the dashboard Activity Feed.

### Added (round 4 — cross-branch aggregate pre-emption)
- **`duduclaw_fork::LiveAggregate`** — a streaming-time companion to `budget::Pool`, shared across a
  fork's concurrent branches. It tracks every in-flight branch's live `total_cost_usd`; the moment their
  combined spend crosses the aggregate cap it names the **most-expensive in-flight branch** (deterministic
  tie-break) so it can be pre-emptively killed — instead of waiting for each branch to hit its own
  per-branch cap. (5 unit tests.)
- **Spawner wiring** (`mcp_fork_exec.rs`) — each stream-json cost update runs the pure
  `stream_budget_decision` (per-branch cap → aggregate `observe`): the priciest over-budget branch
  self-kills if it is the observer, otherwise the observer `request_budget_kill`s the sibling. The
  aggregate kill is tagged so the woken branch maps to `BudgetExceeded` (→ `BudgetKilled`), distinct
  from an operator `terminate_branch` (`Cancelled` → `Terminated`); `LiveAggregate::finish` frees a
  branch's budget for survivors once it ends. (5 unit tests.) Completes RFC-26 §4.2.

### Added (skill synthesis — W19-P1)
- **Periodic auto-run scheduler** (`skill_synthesis_pipeline::scheduler`) — runs the
  rollout-to-skill pipeline on a fixed interval instead of waiting for a manual
  `skill_synthesis_run` MCP call. **Off by default**, **dry-run by default even when
  enabled**, hot-reloaded config (`config.toml [skill_synthesis] auto_run/dry_run/
  interval_hours/lookback_days/target_agent`), and non-blocking (pipeline errors are
  captured into the run summary, never abort the loop).
- **Dashboard config RPCs** — admin-gated `skill_synthesis.get` / `skill_synthesis.update`
  (validated writes onto `config.toml [skill_synthesis]`); `skill_synthesis_threshold`
  is a `u32` count (no longer a float — fixes registry scan rejecting `0.7`).
- **`fetch_episodic_evidence`** with path-traversal rejection + tests.

### Added (dashboard — Calm Glass redesign)
- **Calm Glass design system** — shared component library (`web/src/components/ui/`:
  Page / PageHeader / Section / Card / StatCard / Button / Badge / Field / Tabs /
  EmptyState) + a 6-group sidebar nav model (`layout/nav-model.ts`), applied across
  every dashboard page. Design spec in `web/DESIGN.md`; design tokens in `index.css`.
- i18n keys synchronized across `en` / `ja-JP` / `zh-TW`.

### Documentation
- **Feature docs reorg + trilingual coverage** — 10 new feature deep-dives (20–29:
  memory-intelligence, governance-layer, durability-framework, autopilot-engine,
  task-board, identity-resolution, mcp-http-sse, pty-pool-runtime, live-forking,
  evolution-events) in `en` / `ja-JP` / `zh-TW`; back-translated 16–19 to `ja-JP` /
  `zh-TW`; `feature-inventory` refreshed `v1.8.14 → v1.22.0`; README indexes updated.

### Housekeeping
- Removed residual local artifacts (test `.profraw`/coverage, stale 5.6 GB git
  worktrees) — gitignored cruft only, no repo content affected.


## [1.21.1] - 2026-06-18 — Channel routing: stop bot "identity mixing"

### Fixed
- **Agent-bound bot token now takes precedence over the global poller**
  (Telegram / Slack / Discord). These channels' long-poll (`getUpdates`) and
  gateway sessions are exclusive per bot token. When the same token was
  configured both globally (`config.toml`) and on a specific agent, the dedup
  kept the generic **global** poller and skipped the agent one — so two pollers
  fought over the same token (Telegram **409 Conflict**, dropped messages) and
  the surviving generic poller routed via `default_agent`, causing **identity
  mixing** (e.g. a CEO bot sometimes answered as COO). Precedence is reversed:
  agent tokens are collected first and the global poller is skipped (with a
  `WARN` naming the owner) for any token an agent already binds. Extracted the
  shared `find_global_token_owner` helper with unit tests.
- **`default_agent` validation at startup** — a dangling `default_agent`
  (pointing at a renamed/removed agent) silently fell back to an arbitrary main
  agent at routing time, the other root cause of identity mixing. The gateway
  now validates `default_agent` against the loaded registry at boot and `WARN`s
  loudly (listing available agents), and the per-turn fallback path warns too.



## [1.21.0] - 2026-06-17 — RFC-25 §5 Followups: non-Claude path fully functional

RFC-25 v1.20.0 compiled the multi-runtime abstraction but left the non-Claude
(Codex / Gemini / OpenAI-compat) path as a thin opt-in with documented gaps.
v1.21.0 closes all 11 of those gaps so non-Claude agents are first-class, and
hardens the release tooling so PyPI can no longer be silently skipped.

### Added
- **Multi-turn context for non-Claude runtimes** (A1): `conversation_history`
  threaded through the choke-point and consumed by Codex / Gemini / OpenAI-compat
  (OpenAI-compat uses native multi-turn `messages`); duplicate `ConversationTurn`
  consolidated onto `runtime::ConversationTurn`.
- **Non-Claude cost telemetry** (A3): `run_agent_prompt` records token usage to
  `CostTelemetry` (detached, classified by `request_type`) so Codex/Gemini/OpenAI
  usage is visible to cost summaries, the 200K price-cliff warning, and adaptive routing.
- **Non-Claude channel keepalive** (A4): periodic `ProgressEvent::Keepalive` during
  long non-Claude replies so channels don't look stalled / hit idle timeouts.
- **`scripts/release.sh` multi-platform sync**: `audit` (per-platform version + drift),
  synchronized bump across every manifest, a post-bump assertion that aborts if any
  platform is left behind, and `verify <version>` that queries PyPI + npm.

### Changed
- **Pending-tasks for non-Claude delegation** (A2): the Task-Board queue is inlined
  into the non-Claude sub-agent system prompt.
- **Routing decision centralized** (B1): `RuntimeSettings::non_claude_provider()`;
  "Claude into the registry" is an explicit non-goal (orphan `runtime/claude.rs`).
- **Single `agent.toml` parse per reply** (B2): `RuntimeSettings` + `load_runtime_settings`
  threaded via `AgentPrompt.runtime_settings` (3 → 1 reads/reply).
- **Per-(home,provider) failover health** (R1): choke-point routes through
  `FailoverManager` (3 failures → 60s cooldown → fallback), keyed per home to avoid
  cross-tenant bleed.
- **Per-home `RuntimeRegistry` cache** (R2): replaces the first-home-binding `OnceCell`.
- **A2A target resolution** (R3): `resolve_target_agent` (default → Main agent, else
  validated) + per-home `AgentRegistry` cache with agents-dir mtime invalidation.
- **Utility provider routing** (N2): summarizer / wiki-ingest / reflection / synthesis
  honour the agent's (or global `config.toml [runtime] utility_provider`) runtime.
- `DEFAULT_UTILITY_MODEL` is a single source in `duduclaw-core` (B3).

### Fixed
- **PyPI release miss**: `pyproject.toml` had drifted to 1.18.0 (while Cargo/READMEs
  were at 1.20.0), so the CI `pypi-publish` job built a stale wheel that
  `skip-existing` silently dropped. `release.sh` now syncs every manifest and
  asserts they all reach the new version; this release also heals the drift
  (pyproject + npm manifests → 1.21.0).
- `record_usage` no longer blocks the reply on a synchronous SQLite write, and skips
  empty-`agent_id` (agent-less utility) attribution.



## [1.20.0] - 2026-06-16 — RFC-25 Multi-Runtime Unlock + A2A

The "Multi-Runtime four-backend" abstraction was previously **orphan, uncompiled
source** — every execution path hardcoded Claude. RFC-25 compiles and wires it,
and routes the LLM-calling subsystems through a single provider-agnostic
choke-point. Existing agents are unaffected (provider defaults to Claude); agents
can now opt into Codex / Gemini / OpenAI-compat via `agent.toml [runtime] provider`.

### Added

- **`RuntimeType` (duduclaw-core)** — `{Claude, Codex, Gemini, OpenAiCompat}`,
  unblocking the never-compiled `runtime/` abstraction (`AgentRuntime` trait,
  `RuntimeRegistry`, four runtime impls) + `failover.rs` — now compiled for the
  first time.
- **`runtime_dispatch::run_agent_prompt` choke-point** — resolves the agent's
  `[runtime] provider` → selects from a lazily-built, auto-detecting
  `RuntimeRegistry` → executes, falling back to the configured fallback then Claude.
- **`runtime_config`** — reads `[runtime] provider`/`fallback` and `[model] utility`;
  `ModelConfig.utility` (default `claude-haiku-4-5`) centralizes the previously
  scattered hardcoded utility-model literals.
- **A2A real execution** — the ACP stdio server's `tasks/send` now runs the target
  agent (via the provider-aware gateway dispatch) instead of a placeholder; the
  responding agent's `[runtime] provider` is honoured.

### Changed

- **GVU evolution allowlist relaxed** — the hard `ALLOWED_EVOLUTION_MODELS` reject
  (which forced `claude-haiku-4-5` and blocked everything else) is now a warning.
- **Channel reply** routes non-Claude providers through the choke-point; Claude
  keeps the optimized OAuth-rotation/PTY path (zero regression).
- **GVU loop + sub-agent delegation** (`call_claude_for_agent_with_type`,
  plain + worktree paths) route through the choke-point, honouring per-agent provider.

### Fixed

- `failover::is_non_retryable` now matches free-form "content policy" (a never-run
  test in the previously-uncompiled module).

### Notes

- `a2a/1` HTTP capability stays gated (separate transport, not wired to A2A
  execution). Sandbox (container) delegation and home-only utility tasks remain on
  Claude — documented follow-ups in `commercial/docs/RFC-25-multi-runtime-unlock.md`.


## [1.19.0] - 2026-06-16 — Memory Intelligence: Temporal Memory + Reflexion Loop + Batch Fetch

Three W18/W19-designed memory features, implemented non-invasively on the live
Rust `SqliteMemoryEngine` (the original PostgreSQL/Python designs were updated to
the actual SQLite architecture). No new infrastructure.

### Added

- **F1 Temporal Memory** (`duduclaw-memory`) — `memories` gains temporal /
  knowledge-graph columns (`valid_from`, `valid_until`, `superseded_by`,
  `supersedes`, `subject`, `predicate`, `object`, `confidence`, `metadata`) via
  the existing idempotent migration loop, plus two indexes. New
  `store_temporal(entry, TemporalMeta)` performs automatic conflict resolution:
  writing the same `(agent, subject, predicate)` supersedes the prior fact
  (closes its `valid_until`, links the supersession chain). `search()` /
  `search_layer()` now return only currently-valid memories by default;
  `get_history()` / `get_at()` expose the chain and point-in-time lookups.
  `MemoryEntry` is **unchanged** (zero blast radius across 22+ construction sites).
- **F2a Reflexion recall** (`duduclaw-gateway/channel_reply`) — an agent's recent
  unresolved `MistakeNotebook` entries are now injected into the **answering
  prompt** (`## Past Mistakes to Avoid`), not just the GVU SOUL.md generator —
  delivering cross-task learning. CJK-safe (topic match with recency fallback).
- **F2b Reflexion consolidation** (`duduclaw-gateway/reflexion`) — when the same
  `MistakeCategory` accumulates ≥3 unresolved mistakes, they are distilled into a
  single **semantic** memory rule (via F1 supersession) and the source mistakes
  are marked resolved. Runs detached so it never delays replies. Deterministic
  synthesis (zero LLM cost).
- **F3 `memory_fetch_batch`** — new MCP tool + `SqliteMemoryEngine::get_by_ids`
  fetch up to 100 entries by ID in one call (`Scope::MemoryRead`, namespace /
  ownership enforced, partial hits return `missing_ids` without error).

### Notes

- Trigger signal for reflexion is the existing `ErrorCategory` (Significant /
  Critical, MetaCognition-adaptive) — **not** the GVU Verifier (which validates
  SOUL.md proposals, not task quality). No hard-coded score threshold.
- The standalone Python MCP memory track remains superseded by the Rust CLI
  endpoints (not revived).


## [1.18.0] - 2026-06-15 — Dashboard budget/usage accuracy + reliability fixes

Dashboard now reads real spend from the persistent `CostTelemetry` ledger
instead of the `AccountRotator`'s in-memory counter (which reset on every
5-minute rebuild / gateway restart and stayed 0 for OAuth-subscription
accounts), plus a sweep of dashboard runtime-bug fixes.

### Added

- **`marketplace.install`** handler — installs a catalog MCP server into a
  chosen agent's `.mcp.json` (was previously an error stub). Frontend gains
  a target-agent picker dialog.
- **`system.version` returns `edition`** so the dashboard can gate Pro-only
  UI (auto-update toggle).
- **Voice & Proactive settings persistence** — `system.update_config` writes
  `[voice]` to `inference.toml`; per-agent `[proactive]` saved via
  `agents.update` and surfaced through `agents.inspect`.
- Language switcher + theme store wired into the dashboard Header.
- 88 missing i18n keys across `zh-TW` / `en` / `ja-JP`.

### Fixed

- **Budget/usage shows real data**: `agents.list` / `agents.inspect` report
  per-agent month-to-date spend (was one all-account aggregate shown
  identically for every agent); `accounts.budget_summary` reports the real
  global month-to-date total.
- **Cost unit correction**: `cost_millicents` holds whole cents — removed the
  erroneous `/10` in analytics savings/cost which under-reported by 10x.
- **Schedule (cron) add**: sends required `name` + `task` (was failing).
- **MCP servers** consumed as the array shape the backend serializes.
- Division-by-zero guard in budget progress bars; silent account errors now
  surface as toasts; assorted async-state and timer-cleanup fixes.

## [1.17.0] - 2026-06-10 — RFC-24 License v2.0 (Open Core foundation)

Bootstrap of the DuDuClaw Open Core commercial layer. Apache 2.0 remains
**fully usable with no limits and no commercial modules** — paid
subscription tiers unlock value-add modules under `commercial/*`
(premium templates, evolution params, enterprise dashboard, priority
security patches). No license installed → OpenSource tier (current
behavior, zero regression).

### Added

- **`duduclaw-license` crate** — verification-only client (signing key
  stays in `commercial/duduclaw-license`).
  - 7 tiers with inheritance chain: `OpenSource` / `Hobby` / `Solo` /
    `Studio` / `Business` / `SelfHostPro` / `Oem`.
  - Ed25519 trust registry seeded from `DUDUCLAW_LICENSE_PUBKEY_<ID>`
    env vars; empty registry collapses to OpenSource (fail-safe).
  - CRL polling for emergency revocations.
  - Machine binding via hostname + MAC fingerprint.
  - `~/.duduclaw/license.json` (`0o600`) storage.
  - `features.toml` v2 subscription matrix: Solo NT$990 / Studio
    NT$2,990 / Business NT$8,900 / Self-Host Pro NT$1,490·mo or
    NT$14,900·yr / OEM (Year 2+).
- **Gateway `license_runtime`** — bootstraps on `start_gateway`,
  phone-home loop, CRL poll, process-global `LicenseRuntime`. Every
  failure mode (missing file / empty key registry / signature mismatch
  / expired / grace exceeded) collapses to OpenSource — gateway never
  crashes on license errors.
- **`license.status` RPC** (manager-only) — returns `LicenseSnapshot`
  that intentionally **omits the raw Ed25519 signature and customer
  email** so it is safe to render in the dashboard.
- **`duduclaw license` CLI** — subcommands: `activate` / `status` /
  `refresh` / `export` / `import` / `deactivate` / `fingerprint`.
- **Web Dashboard `LicensePage`** — tier status, expiry warnings,
  unlocked modules grid, CTAs (renew / pricing / docs). i18n: 39 keys
  per locale (zh-TW / en / ja-JP).
- **README Hire me block** — Fiverr / LinkedIn / Portfolio links above
  the badges (GitHub long-URL SEO).
- **`scripts/dev-replace-binary.sh`** — local build → npm-installed
  binary replacement loop (rebuild dashboard + cargo `--release` +
  backup + install + smoke). Does NOT auto-restart side-effectful
  processes.
- **`.cargo/config.toml`** — pin `PYO3_PYTHON=python3.13` (BLK-7
  workaround; PyO3 0.24 max supported Python is 3.13, system default
  is 3.14).
- **Operational docs** — `marketing/blog/`, `marketing/press-kit/`,
  `wiki/eval/`, `wiki/impl/`, `wiki/reports/`, `wiki/sprint-reports/`,
  `docs/wiki/reports/`, `reports/tl-daily/`.

### Backward compatibility

- No license installed → OpenSource tier (current behavior, zero
  regression).
- All existing CLI / RPC / config surface unchanged.



## [1.16.0] - 2026-06-01 — MCP Refresh Tokens + GVU Consolidate Op

Two operationally-driven additions surfaced by the 12-day post-v1.15.2
production soak.

**Symptom 1**: agnes' Claude Desktop MCP server stopped working on
2026-05-30 15:59 UTC with `API key expired (31 days old, max 30)` and
sat broken for 2 days before being investigated. The 30-day rotation
policy was sound, but the user-experience around it was not — Claude
Desktop quietly disconnected and never retried, and there was no CLI
to rotate cleanly.

**Symptom 2**: agnes' SOUL.md has been growing at ~5 lines/week under
healthy GVU operation and is now at 132 lines / 7909 bytes — within the
150-line / 8 KB cap, but ~3 weeks from cap-rejection on current
trajectory. The structured patch path can only grow SOUL.md; it has
no shrink primitive.

### Added — MCP refresh tokens (Phase A)

- **New module `duduclaw-cli::mcp_refresh`** — SQLite-backed refresh
  tokens that supersede the 30-day legacy API keys with 90-day
  lifetime, per-token revocation, and a hash-only store (the raw
  token is never persisted).
  - Format: `ddc_refresh_<env>_<64hex>` (twice the entropy of legacy).
  - Storage: `~/.duduclaw/mcp_tokens.db` table `refresh_tokens` with
    columns `jti, token_hash, client_id, scopes, is_external,
    issued_at, expires_at, revoked_at`.
  - Validation: `authenticate_with_refresh_token` mirrors
    `mcp_auth::authenticate_with_key`'s error model so the existing
    dispatcher just needs prefix-based routing.

- **`mcp_auth::authenticate_from_env`** now prefix-routes credentials:
  values starting with `ddc_refresh_` go through the refresh-token
  validator, everything else through the legacy `ddc_<env>_<32hex>`
  validator. Both paths return the same `Principal` type so downstream
  code is unchanged. Legacy keys keep working — no migration required
  before refresh tokens are adopted.

- **New CLI subcommand `duduclaw mcp …`**:
  - `issue-refresh-token --env <prod|staging|dev> --client-id <id>
    --scopes <csv> [--external]` — generates a fresh token, persists
    its hash, prints the raw token ONCE.
  - `revoke-token <jti>` — soft-deletes a refresh token by its
    16-hex jti prefix.
  - `list-tokens` — table view of all tokens with status / remaining
    TTL / scopes.

### Added — GVU `SoulPatchOp::Consolidate` (Phase B)

- **New variant** in `crate::gvu::proposal::SoulPatchOp`.
  Semantically equivalent to `Replace` but with a hard size-shrink
  invariant: the patch is rejected if
  `content.trim().len() >= existing_body.trim().len()`. Used when
  SOUL.md is approaching the line/byte caps and the LLM is asked to
  merge redundant bullets or tighten verbose phrasing without
  changing behavior.

- **`apply_patch_to_soul` enforces the shrink invariant** before
  swapping. A `Consolidate` whose content does not shrink the section
  is rejected with `"Consolidate must shrink the section — new
  content is N bytes but existing body is M bytes"`. Empty existing
  body is also rejected (cannot consolidate nothing).

- **Generator prompt updated** with the `consolidate` op semantics so
  the LLM can self-trigger it when it sees SOUL.md approaching cap.
  Prompt change is additive — existing flows that don't need to
  consolidate are unaffected.

### Fixed

- **`mcp_auth` test suite was a time-bomb**. Six test fixtures used
  the hardcoded `created_at = "2026-04-29T00:00:00Z"`. On 2026-06-01
  (33 days later) every test expecting `Ok(Principal)` started
  failing with `KeyExpired { days_old: 33 }`. Replaced with
  `Utc::now().to_rfc3339()` so the suite stays robust to time. Five
  similar fixtures in `mcp_auth_strategy` already had this issue
  (5/30 onward) and were similarly fixed.

### Test coverage

12 new unit tests, total workspace **1537 passing**:

- `mcp_refresh::tests` ×8 — roundtrip, format rejection, unknown,
  revoke, expired, list ordering, jti determinism, env-label
  rejection at issue.
- `soul_patch_tests::consolidate_*` ×4 — shrinks existing,
  rejects-when-grows, rejects unknown section, rejects empty body.

### Operator action items

**Migrating Claude Desktop to a refresh token**:

```
duduclaw mcp issue-refresh-token \
    --env dev \
    --client-id claude-desktop \
    --scopes memory:read,memory:write,wiki:read,wiki:write,messaging:send
```

Paste the printed token into `~/Library/Application Support/Claude/claude_desktop_config.json`
under `mcpServers.duduclaw.env.DUDUCLAW_MCP_API_KEY`, then Quit and
relaunch Claude Desktop. After verifying the new token works, revoke
the old legacy key by removing it from `~/.duduclaw/config.toml` (or
running `duduclaw mcp revoke-token <jti>` if it was already a
refresh token).

**Legacy keys remain supported indefinitely** — refresh tokens are
strictly opt-in. Operators who prefer the file-based registry can
continue using `[mcp_keys]` entries.


## [1.15.2] - 2026-05-20 — agent_update_soul Audit + Drift Detection

Follow-up to v1.15.1. Investigating an unexpected 11-line SOUL.md growth
during agnes' 24h observation period revealed three pre-existing security
gaps in the `agent_update_soul` MCP backdoor that long predated v1.15.1 but
became visible thanks to the structured-patch path making routine GVU
writes traceable by contrast.

### Fixed

- **`agent_update_soul` now refreshes the `soul_guard` integrity hash**
  via `accept_soul_change` on every successful write. Before this fix the
  stored fingerprint was never updated, so legitimate calls left permanent
  drift that `check_soul_integrity` would (eventually, if a human invoked
  it) flag as tampering. agnes' 2026-05-19 02:27Z self-modification was
  the canonical observation.

- **`agent_update_soul` now writes to `tool_calls.jsonl`** for every
  invocation — success path with hash prefix + size, and four distinct
  rejection paths (invalid agent_id / empty content / nonexistent agent /
  tmp-write / rename failures). The trusted MCP backdoor was previously
  invisible to post-hoc audit; `tool_calls.jsonl` had no `agent_update_soul`
  entries between 2026-04-22 and today despite the tool being exercised at
  least once on 2026-05-19.

- **Heartbeat now runs `soul_guard::check_soul_integrity` per agent per
  tick** via the new `check_soul_integrity_with_audit` helper.
  Out-of-band SOUL.md modifications (whether legitimate-but-unaudited or
  malicious) now produce a `WARN` log and a `_soul_integrity_drift`
  synthetic audit row within one heartbeat interval (default 1 h). Prior
  to this fix the integrity check had exactly one caller — the manual
  `duduclaw test <agent>` CLI red-team — so drift sat silently until an
  operator chose to investigate. Agents without a `SOUL.md` are silently
  skipped (stub-agent configuration is documented and supported).

### Why this is separate from v1.15.1

These three gaps existed before v1.15.1 — the bloat-fix surfaced them but
did not introduce them. They are filed as a separate patch release
because their root cause (the `agent_update_soul` MCP tool bypassing the
GVU safety stack) is structurally orthogonal to the GVU verifier path and
deserves its own audit narrative.

### Test coverage

9 new tests, total workspace 1525 unit tests passing:

- `mcp::wiki_namespace_tests::agent_update_soul_refreshes_soul_guard_hash`
- `mcp::wiki_namespace_tests::agent_update_soul_appends_audit_row`
- `mcp::wiki_namespace_tests::agent_update_soul_audits_validation_rejections`
- `heartbeat::tests::soul_integrity_check_skips_agent_without_soul`
- `heartbeat::tests::soul_integrity_check_clean_when_hash_matches`
- `heartbeat::tests::soul_integrity_check_emits_audit_on_drift`

### Operator action items

After upgrading to 1.15.2 you may see `_soul_integrity_drift` audit rows
in `tool_calls.jsonl` for agents whose SOUL.md was last modified by the
pre-1.15.2 `agent_update_soul` (which never updated the hash). The drift
is real — the stored hash genuinely doesn't match the file — but it's a
historical artefact, not active tampering.

To clear the false-positive baseline, delete the stored hash so the next
integrity check re-fingerprints the current file as the new baseline:

```
rm ~/.duduclaw/soul_hashes/<agent>.hash
```

`check_soul_integrity` treats a missing hash as "first run" and stores
the current SHA-256 automatically. Subsequent out-of-band modifications
will then flag genuine drift.


## [1.15.1] - 2026-05-18 — SOUL.md Bloat Containment + Structured Patch Path

Customer-reported regression: a production COO bot (`agnes`) had its
`SOUL.md` balloon from 61 to 592 lines over 5 GVU cycles, with 88% of the
file being accumulated proposal-meta narrative (`## 診斷` / `## rationale` /
`## expected_improvement` / `## wiki_proposals`). Each subsequent cycle saw
the bloated file, generated another correction, and the updater appended it
verbatim — an infinite feedback loop that bypassed every safety check by
progressively expanding the baseline so ASI's content-weighted threshold
stayed permanently satisfied.

This release fixes the failure mode in three layers and ships an unrelated
MCP server stdout-pollution bug fix.

### Fixed

- **`Updater::apply` no longer appends LLM proposal-meta narrative.**
  - New `strip_proposal_meta()` drops `## 診斷` / `## Analysis` /
    `## rationale` / `## expected_improvement` / `## wiki_proposals` /
    `## proposed_changes` headers and their bodies before the legacy append.
  - New `SOUL_MAX_LINES = 150` and `SOUL_MAX_BYTES = 8 KB` hard caps reject
    any proposal that would push SOUL.md beyond either limit, independent of
    ASI (which becomes permissive on a growing baseline because
    `for_baseline_size` weakens the threshold proportionally).
  - The legacy "always append, never replace" safety justification is
    preserved — the strip+cap layer makes append bounded instead of unbounded.

- **L1 verifier now simulates the real apply path.** When
  `proposal.patch.is_some()`, `verify_deterministic` calls
  `apply_patch_to_soul` to compute the true post-apply SOUL.md and runs
  must_always / must_not / size / sensitive-pattern checks against it,
  instead of the legacy `current + content` fake append. Without this fix
  the structured-patch path was DOA: `proposal.content` becomes a human
  summary like "Add refusal rule" and the must_always pattern (a chunk of
  contract text) was never found inside it — observed on agnes 2026-05-18
  where 3 generations rejected for the same phantom must_always failure
  despite the LLM's `soul_patch` JSON containing exactly the required text.

- **`Generator::parse_response` extracts JSON from markdown code fences.**
  LLMs commonly wrap structured output in ` ```json ... ``` ` fences with
  surrounding narrative (e.g. "根據分析... ```json\n{...}\n``` ...核心邏輯...").
  The parser previously failed pure-JSON parsing on the fence, fell back to
  section extraction, dropped the `soul_patch` field, and silently
  downgraded to the legacy strip+cap append. Reuses `verifier::strip_json_fences`
  for consistency with `parse_judge_response`.

- **MCP server stdout pollution.** `tracing_subscriber::fmt::layer()` now
  routes to stderr instead of stdout. Claude Desktop's MCP stdio transport
  parses stdout as JSON-RPC 2.0; the previous default tracing destination
  corrupted every session with `Unexpected token '', "[2m2026-0..." is not
  valid JSON` errors. The downstream `cmd_mcp_server` re-init via
  `try_init` silently no-opped once the global subscriber was already
  installed. Independent of GVU — affects every MCP client.

### Added

- **`SoulPatch { section, op, content }` structured edit type** with four
  ops — `Replace`, `AppendWithin`, `PrependWithin`, `AddSection`. Located in
  `crates/duduclaw-gateway/src/gvu/proposal.rs`. Optional `patch:
  Option<SoulPatch>` field on `EvolutionProposal` so on-disk proposals
  deserialize unchanged.

- **`apply_patch_to_soul(current, patch) -> Result<String, String>`** in
  `gvu/updater.rs` — locates the target `## <title>` header, edits the
  section body in place per the op, reassembles SOUL.md. Section names
  containing newlines or `##` tokens are rejected (prompt-injection
  defence); patch.content is capped at 4 KB per edit.

- **Generator prompt asks LLM to emit a `soul_patch` field.** Schema with
  op semantics, hard rules forbidding `[保留現有內容]` placeholders and
  whole-file rewrites, plus a concrete example. Prompt length grew from
  ~22 KB to ~24.5 KB (≈11% per GVU run).

### Production validation

Run on `agnes` 2026-05-18 09:53Z → 10:42Z, four iterations against the same
Round 1-4 conversation script (boundary probe → off-scope medical question
→ negative feedback). Final state: SOUL.md grew 61 → 85 lines with one
cleanly-added new section, zero meta-narrative residue, zero duplicate
headers, zero embedded JSON. GVU `outcome=applied`, generation 1, 60.2s
duration, ASI=0.679 (warning, not critical), L1+L3 verifier approved.

### Test coverage

106 GVU unit tests, +22 new:
- `proposal_meta_stripper_tests` — Chinese + English meta sections, blank-line
  collapsing, case-insensitive headers, cap sanity.
- `updater_apply_caps_tests` — meta-only rejection, line-cap overshoot
  rejection, clean-proposal application.
- `soul_patch_tests` — all four ops, section-not-found, prompt-injection
  guards, oversized content rejection, replace-then-replace idempotence.
- `soul_patch_apply_e2e_tests` — end-to-end through `Updater::apply` with
  `proposal.patch = Some(...)`.
- `generator_tests` — fence-stripped JSON parse, missing-patch fallback,
  prompt-includes-schema regression guard.
- Patch-aware L1 verifier — append-patch satisfies must_always check,
  rejection when pattern truly missing, invalid-section gives clear error,
  must_not check uses patch content not human summary.

### Backward compatibility

- `proposal.patch` is `Option`, `#[serde(default, skip_serializing_if =
  "Option::is_none")]` — proposals serialized before this release
  deserialize unchanged.
- `Updater::apply` falls back to strip+cap legacy append when `patch` is
  `None`. LLMs that haven't adopted the new schema continue to work, just
  with the bounded-growth safety net instead of unbounded append.
- No `CONTRACT.toml` or `agent.toml` schema changes.
- No migration required. Existing SOUL.md files already polluted by prior
  bloat are not auto-cleaned — operators should hand-truncate or wait for
  ObservationFinalizer's rollback path to fire on metric regression.


## [1.15.0] - 2026-05-17 — Cross-Platform PTY Pool + Worker

Anthropic blocked `claude -p` for OAuth-subscription accounts in mid-2026 and
recommended driving the real interactive `claude` REPL instead. v1.15.0 ships
the runtime to do exactly that: long-lived `claude` sessions driven through a
real PTY (ConPTY on Win 10 1809+, openpty on Unix) with a sentinel-framed
in-band response protocol — no scrollback scraping, no sidecar.

Default **OFF**; per-agent opt-in via `agent.toml [runtime] pty_pool_enabled =
true`. The existing fresh-spawn `claude -p` path is preserved for API-key
accounts and remains the global default. Every PTY path falls back to legacy
`tokio::process::Command + claude -p` on error, so a missing worker /
unhealthy pool / spawn failure is recoverable, not fatal.

### Added

- **New crate `duduclaw-cli-runtime`** — cross-platform PTY runtime built on
  [`portable-pty`](https://crates.io/crates/portable-pty).
  - `PtySession` lifecycle + `SpawnOpts` per-CLI configuration
    (`CliKind::Claude / Codex / Gemini`).
  - `PtyPool` with per-agent semaphore, idle eviction, supervisor + restart
    policy. Cache-hit / spawn / 3 eviction-reason counters surface to gateway.
  - `Envelope` / `Frame` / sentinel constants + ANSI-stripping
    `extract_payload_with_chrome_filter`.
  - `oneshot_pty_invoke` for the `claude -p` PTY-wrapped fallback (API-key
    accounts).
- **New crate `duduclaw-cli-worker`** — standalone worker binary wrapping the
  pool over a localhost HTTP+JSON-RPC API.
  - Bearer-token auth via `DUDUCLAW_WORKER_TOKEN` env var + on-disk
    `TokenStore` (`ring::SecureRandom`).
  - Endpoints: `POST /rpc` (`invoke` / `shutdown_session` / `stats`),
    `GET /healthz` (no auth).
  - Library re-export so the gateway shares the protocol types directly.
- **Gateway integration**:
  - `crates/duduclaw-gateway/src/pty_runtime.rs` — adapter owning the global
    `PtyPool`, `RuntimeMode::{FreshSpawn, PtyPool}` per-agent routing,
    `acquire_and_invoke` + `acquire_and_invoke_with` public surface,
    optional `MANAGED_WORKER` `WorkerClient` for Phase 7.
  - `crates/duduclaw-gateway/src/worker_supervisor.rs` — Phase 7 supervisor
    for the out-of-process worker. Resolves binary, spawns with loopback
    bind + token + `--home-dir`, polls `/healthz` until ready, runs a 30s
    health-check loop with N-strike restart, and **sequences
    SIGTERM/SIGKILL into the gateway graceful-shutdown future** (after
    prediction-engine flush, before axum drains) instead of racing it from
    a detached task (Round 2 review HIGH-4).
  - `crates/duduclaw-gateway/src/runtime_status.rs` — Phase 8.5 JSON status
    endpoint `GET /api/runtime/status` (loopback-only, no auth) with
    transport + kill-switch + session / invoke / worker stats.
  - `crates/duduclaw-gateway/src/channel_reply.rs` —
    `call_claude_cli_pty_rotated` PTY-routed mirror of
    `call_claude_cli_rotated`; OAuth → interactive REPL, API-key →
    `oneshot_pty_invoke + claude -p`. `parse_claude_stream_json_complete`
    is a buffer-based mirror of the streaming parser;
    `StreamDiagnostics` is embedded in error messages so
    `channel_failures.jsonl` post-mortem identifies what went wrong inside
    the PTY response (`exit / lines / events / assistant / text_blocks /
    thinking / tool_use / result_subtype / stop_reason / last_line /
    stderr_tail`).
  - `crates/duduclaw-gateway/src/claude_runner.rs` — dispatcher-side
    short-circuit: when `pty_pool_enabled = true`, sub-agent invocations
    skip local-offload + hybrid routing and go straight through the pool
    (channel reply + dispatcher consistent).
- **Phase 8 production observability** (`crates/duduclaw-gateway/src/metrics.rs`):
  - `pty_pool_acquires_total` + `pty_pool_acquires_cache_hit_total` +
    `pty_pool_acquires_spawn_total`.
  - `pty_pool_evicted_idle_total` + `pty_pool_evicted_unhealthy_total` +
    `pty_pool_evicted_shutdown_total`.
  - `pty_pool_invokes_ok_total` + `pty_pool_invokes_empty_total` +
    `pty_pool_invokes_error_total` + `pty_pool_invokes_timeout_total`.
  - `pty_pool_invoke_duration_buckets[8]` (shared bounds with the main
    request histogram) + `pty_pool_invoke_duration_sum_ms`.
  - `worker_health_misses_total` + `worker_restarts_total`.
  - `pty_pool_managed_worker_active` gauge (0 = in-process, 1 = managed).
- **Smoke harness**:
  - `scripts/smoke-pty-pool.sh` (Unix/macOS) — build cli-runtime + spike
    example + run cli-runtime / gateway `pty_runtime::` /
    `channel_reply::routing_helper_tests` / `stream_json_parser_tests`.
    `CLAUDE_SPIKE=1` runs the live interactive spike (consumes OAuth quota).
  - `scripts/smoke-pty-pool.ps1` — Windows equivalent.

### Operational notes

- **Kill switches**:
  - Per-agent: `agent.toml [runtime] pty_pool_enabled = false` (default).
  - Global: env-var kill switch disables PTY routing without rolling back.
- **Out-of-process mode** (`[runtime] worker_managed = true` in
  `<home>/config.toml`) promotes the in-process pool to the
  `duduclaw-cli-worker` subprocess. The supervisor is best-effort: spawn
  failure leaves the gateway in in-process mode with a warn log.
- **Cross-platform**:
  - Windows: `windows` crate Job Objects for child-process containment
    (Win10 1809+).
  - Unix: `nix` for signal + process-group control.
- **References**: [`dorkitude/maude`](https://github.com/dorkitude/maude)
  (Unix-only tmux shim that inspired the interactive driving idea) and
  [`runtorque/torque`](https://github.com/runtorque/torque) (Unix-only
  PTY + UDS frame protocol). `portable-pty` is what makes one code path
  span mac/Linux/Windows.

### Design docs

- `commercial/docs/runtime-pty-pool-design.md` — full architecture, kill
  switches, security stance.
- `commercial/docs/TODO-cli-pty-pool-worker.md` — phase-by-phase TODO with
  verify steps.


## [1.14.0] - 2026-05-14 — RFC-23 Redaction Pipeline

新增獨立 crate `duduclaw-redaction` 與 gateway 整合層，預設**未啟用**。

### Added

- **New crate `duduclaw-redaction`** — source-aware redaction +
  reversible restoration. Internal data (Odoo / shared wiki / file tools)
  is replaced with `<REDACT:CATEGORY:hash8>` tokens before the LLM sees
  it; tokens are restored at trusted boundaries (user channel reply,
  whitelisted tool egress).
- **Encrypted SQLite vault** at `~/.duduclaw/redaction/vault.db` using
  AES-256-GCM (reused from `duduclaw-security`), with per-agent 32-byte
  keys (`0o600` permission), TTL 7d default, two-stage GC (mark expired
  → purge after 30d).
- **Five built-in profiles** embedded in the binary: `general`,
  `taiwan_strict`, `taiwan_minimal`, `financial`, `developer`. Selected
  via `[redaction] profiles = [...]`.
- **Five-layer enable/disable resolver** (`compute_effective_enabled`):
  channel `force_on` (banked) → env + CLI flag emergency override → env
  alone → CLI flag → agent.toml → config.toml. Full truth-table coverage.
- **Channel `force_on` lock** with audited `--force-disable-redaction`
  emergency break-glass; persistent override-flag file
  (`~/.duduclaw/redaction/override.flag`) and CRITICAL audit per affected
  channel.
- **Tool egress whitelist** with default deny. Whitelisted tools can
  `restore_args = true` (real values), `passthrough` (keep tokens), or
  `deny`. Hallucinated tokens always result in deny.
- **JSONL audit sink** at `~/.duduclaw/redaction/audit.jsonl` with 10MB
  rotation; events: `redact / restore_ok / restore_denied / restore_miss
  / egress_allow / egress_deny / vault_gc / force_on_override`.
- **Background GC tokio task** running `mark_expired` every 6h and
  `purge_expired` every 24h, with graceful cancel.
- **Dashboard read-only RPCs**: `redaction.stats`,
  `redaction.recent_audit`, `redaction.override_status`,
  `redaction.policy_status`.
- **Gateway integration shim** at `crates/duduclaw-gateway/src/redaction_integration.rs`
  providing `build_manager_from_home()`,
  `compute_effective_for_channel()`, `cli_flag_from_env()`, and
  `force_disable_active()`.
- **Full gateway wiring**:
  - `MethodHandler` carries `Option<Arc<RedactionManager>>` + setter +
    4 `redaction.*` Dashboard RPC handlers (`stats`, `recent_audit`,
    `override_status`, `policy_status`).
  - `start_gateway()` parses `[redaction]` from `config.toml`, builds the
    manager, spawns the 6h-mark/24h-purge GC task, and injects the
    manager into `MethodHandler` and `ReplyContext`.
  - `build_reply_with_session` / `build_reply_for_agent` apply
    `restore` at the public-API exit so the user channel sees real
    values while LLM-bound text retains tokens.
- **MCP-layer integration** (`crates/duduclaw-cli/src/mcp_redaction.rs`):
  - `McpRedactionLayer` reads `DUDUCLAW_AGENT_ID` + `DUDUCLAW_SESSION_ID`
    env vars (set by gateway when spawning the Claude CLI subprocess).
  - On every `tools/call`: pre-check tool args for `<REDACT:...>` tokens
    and run the egress evaluator (whitelisted → restore; otherwise →
    JSON-RPC error). Post-process the tool result Value by walking every
    string leaf through `RedactionPipeline.redact` so the LLM never sees
    raw internal data.
- **CLI flags**: global `--redact=on/off` (overrides agent/global config
  but not channel `force_on`) and `--force-disable-redaction` (requires
  `DUDUCLAW_REDACTION=off`, writes a persistent override flag + CRITICAL
  audit + dashboard red banner).
- **RFC-23** at `commercial/docs/RFC-23-redaction-pipeline.md` + detailed
  per-phase TODO at `commercial/docs/TODO-redaction-pipeline.md` +
  operator guide at `commercial/docs/redaction-operator-guide.md`.

### Tests

- 98 unit tests + 11 end-to-end integration tests in
  `crates/duduclaw-redaction/`, covering: token format & HMAC salt
  derivation; rule compile + ReDoS-surface limits; vault round trip
  (encrypt blob never contains plaintext); cross-session and cross-agent
  isolation; per-rule cross-session-stable override; TTL → expired
  marker → 30-day purge; reveal counter bookkeeping; egress decisions
  (allow/passthrough/deny + nested JSON + hallucinated tokens); profile
  merge with id collision; five-layer toggle truth table with channel
  force_on priority; force-override flag persistence + banner; GC task
  mark+stop cycle.

### Default behaviour

`config.toml [redaction] enabled = false` — existing deployments are
unaffected unless operators explicitly opt in. See
[`commercial/docs/redaction-operator-guide.md`](commercial/docs/redaction-operator-guide.md)
for the five-step adoption recipe.


## [1.13.2] - 2026-05-12

Bug fix for fresh-install clients that have never run the CLI keyfile
init flow.

### Fixed

- **Dashboard credential save no longer fails with "Encryption
  unavailable" on a fresh install.** `encrypt_value()` now calls a new
  `load_or_create_keyfile()` helper that auto-generates the 32-byte
  AES-256 keyfile (`~/.duduclaw/.keyfile`, owner-only permissions) the
  first time the gateway is asked to encrypt a credential. Previously
  the helper was read-only and any client that hit the dashboard
  without first running `duduclaw init` would see the Odoo / channel
  token / API key save fail with a misleading "Ensure keyfile exists"
  message. The decrypt path stays read-only by design so a missing
  keyfile never silently destroys an existing ciphertext.
  (`crates/duduclaw-gateway/src/config_crypto.rs`)
- **Better error messages on the rare encryption failures that remain.**
  The Odoo configure handler now distinguishes the new failure modes
  (RNG / disk write) from the old "keyfile missing" case and points
  operators at the gateway log instead of telling them to fix a file
  the gateway is now able to create itself.
  (`crates/duduclaw-gateway/src/handlers.rs`)

### Tests

- 7 new unit tests covering: keyfile auto-creation, encrypt→decrypt
  round trip after auto-create, rejection of empty plaintext (does not
  pollute the home dir), keyfile stability across successive encrypts,
  decrypt-side read-only invariant, and `mkdir -p` of a fully absent
  home directory.


## [1.13.1] - 2026-05-12

Dashboard UX fix for the Odoo connection page.

### Changed

- **`odoo.test` RPC now accepts inline params** — when the dashboard
  sends `{ url, db, protocol, auth_method, username, api_key?, password? }`,
  the connector is built from those values without writing to
  `config.toml`, so users can verify credentials before persisting. When
  the credential field is empty in inline mode, the handler falls back
  to the stored encrypted secret so a small URL tweak does not require
  retyping the API key. Calling `odoo.test` with no params preserves the
  original "test the saved config" behaviour.
  (`crates/duduclaw-gateway/src/handlers.rs`)
- The Test Connection button on the Odoo page now uses the form's live
  values instead of requiring a prior save. The button is gated on
  url + db being present.
  (`web/src/pages/OdooPage.tsx`)
- `handleSave` / `handleTest` surface the real backend error string
  instead of swallowing it — the previous generic "save failed" /
  "Odoo not configured" messages were undiagnosable from the UI alone.

### Security

- Inline-mode params go through the same SSRF / HTTPS / db-name
  validators as `odoo.configure`. The test path cannot be used to
  bypass safety rules.
- New `scrub_odoo_error()` caps connector failure text at 240 chars
  before forwarding to the dashboard so HTML error pages or full URLs
  with query strings are not leaked.

### Tests

- 16 new unit tests covering happy path, every validation branch, the
  `fc00.*` hostname regression (not an IPv6 ULA), credential fallback,
  and the error-scrubber.


## [1.13.0] - 2026-05-12

Runtime-health overhaul covering 16 issues across two rounds. Round 1
restores GVU/SOUL self-evolution (was effectively dead since 5/3); Round 2
introduces architectural fixes for the cron-driven 200 K token cliff.

See `commercial/docs/TODO-runtime-health-fixes-202605.md` for the
issue-by-issue audit log with verification evidence.

### Added

- **`[prompt] mode = "minimal"` agent config** — opt-in Anthropic
  Skills-style system prompt: SOUL core (≤ 5 KB) + identity + contract +
  MCP tool index. Wiki / skill content fetched on demand instead of
  inlined upfront. Stable prefix → near-perfect prompt-cache hit.
  Expected cliff reduction: 75% on knowledge-rich agents.
  (`crates/duduclaw-gateway/src/prompt_minimal.rs`)
- **`[budget] max_input_tokens` enforcement** — when set, an agent's
  request goes through a compression pipeline (turn trim → drop oldest
  tool echoes → bisect-and-summarize) before send. `cost_pressure` flag
  from §6.3 tightens thresholds automatically. Non-fatal: falls back to
  full history on pipeline failure.
  (`crates/duduclaw-gateway/src/prompt_compression.rs`)
- **`[prompt] cli_bare_mode = true` agent config** — when set, the agent's
  Claude CLI subprocesses launch with `--bare`, suppressing the
  CLAUDE.md auto-discovery leak documented in the spike (see
  TODO #15). Requires an API-key account in the rotator; OAuth accounts
  are skipped with a warn.
  (`crates/duduclaw-gateway/src/claude_runner.rs` `BARE_MODE` task-local)
- **Async session summarizer** — background task (10-min cadence) folds
  older session turns into Haiku-generated bullet summaries. Stored in
  three new columns on `sessions` (`summary_of_prior`,
  `summarized_through_turn`, `last_summarized_at`). `channel_reply`
  prepends the summary as a synthetic assistant recap turn.
  (`crates/duduclaw-gateway/src/session_summarizer*.rs`)
- **TF-IDF wiki relevance ranking** — wiki injection now ranks L0/L1
  pages by user-message relevance (char-bigram TF-IDF, CJK-safe) before
  hitting the 6 KB cap. Auto-enabled, no config required; empty query
  preserves file order for back-compat.
  (`crates/duduclaw-gateway/src/relevance_ranker.rs`,
   `crates/duduclaw-gateway/src/ranked_wiki_injection.rs`)
- **`duduclaw lifecycle flush` CLI** — quarterly cold/hot separation of
  wiki pages. Uses file mtime as access proxy (real counter deferred).
  `--dry-run` by default; pass `--apply` to commit moves to
  `wiki/.archive/`.
  (`crates/duduclaw-gateway/src/lifecycle_flush.rs`)
- **GVU trigger module** — sub-agent dispatches now fire GVU via the
  same path as channel-facing root agents. Previously only `agnes` ever
  evolved; now `duduclaw-tl` etc. can too.
  (`crates/duduclaw-gateway/src/gvu/trigger.rs`)
- **`prompt_audit` observability** — per-section byte-count breakdown
  emitted as `INFO target=prompt_section_audit` when total exceeds 50 KB.
  Surfaces *which* section bloated, not just that total was high.

### Fixed

- **`log_level` config now resolves correctly** — three-tier
  `RUST_LOG → config.toml [general] log_level → "warn"` instead of the
  previous hard-coded `"warn"` fallback. Restores visibility of
  `Heartbeat firing`, `forced_reflection`, `SilenceBreaker consumer
  started`, and other INFO-level diagnostics that were silently dropped.
- **L1 generator `must_always` injection** — Generator now receives the
  contract's `must_always` patterns and emits a `<must_include>` block
  flagging any pattern absent from current SOUL. Unblocks the
  5/3-onwards deferred loop on agnes where every generation failed the
  same L1 check.
- **L1 `must_not` catch-22** — now checks `proposal.content` instead of
  `simulated_final`. Previously, agents that mirrored a `must_not` rule
  into SOUL.md as a self-reminder would have every subsequent proposal
  rejected because the rule statement was in `current_soul`.
- **Discord token-check backoff** — exponential 60 → 120 → 240 → 480 → 900
  seconds (capped 15 min) instead of flat 60 s; respects `Retry-After`
  header. Adds 24 h sliding-window storm detector that emits a
  `discord_invalid_session_storm` security audit event after 5 events.
- **GVU `Skipped` log level** — `debug!` → `info!` so trigger-fired-then-silent
  scenarios (e.g. agent in observation window) are debuggable without
  enabling debug logging.
- **`ObservationFinalizer` 72 h no-traffic cap** — sub-agents without
  channel traffic no longer sit in `observing` forever. After 72 h with
  conversations < 5, auto-confirm so the next GVU can proceed.
- **`skill_loader` recursive scan** — supports the official Anthropic
  Skills `<skill>/SKILL.md` layout (case-insensitive) alongside the
  legacy flat `<name>.md` form. Nested `references/*.md` correctly
  treated as supporting material, not separate skills. Symlink
  containment, hidden-entry skip, 8-level depth cap.
- **`skill_synthesis` pipeline tools** — added regression-guard tests
  ensuring all four pipeline tools (`memory_episodic_pressure`,
  `skill_synthesis_status`, `skill_synthesis_run`, `activity_post`) are
  visible to internal principals. Root cause of the 5/7 incident was a
  stale gateway binary, not missing implementation.

### Stats

- 1264 → 1390 tests green (+126 new unit tests)
- 9 new modules in `duduclaw-gateway`
- 31 files changed, +5790 / −164



## [1.12.3] - 2026-05-08

Hot-fix on top of v1.12.2 — Dashboard 編輯 agent 時 evolution 與 sticker
欄位顯示為預設值而非 agent.toml 真實值。

### Fixed

- **`agents.list` response 漏 `evolution` / `sticker` 區段**
  - Symptom: 在 Dashboard 把 agent 的 `skill_auto_activate` 從 false 改 true
    並儲存，response 回 `success: true` / `hot_reloaded: true`，`agent.toml`
    也確實寫入 `skill_auto_activate = true`；但重新打開 agent 編輯框仍
    顯示 false
  - Root cause: `handle_agents_list_filtered` 回傳 JSON 沒有 `evolution`
    與 `sticker` 兩個區段（只有 `agents.inspect` 有）。前端
    `EditAgentDialog` 從 list response 初始化表單，
    `agent.evolution?.skill_auto_activate ?? false` 因 `agent.evolution`
    為 `undefined` 永遠 fallback 到 `false`
  - 其他 3 個 evolution 欄位（`gvu_enabled` / `cognitive_memory` /
    `skill_security_scan`）剛好預設 `?? true` 對齊大多 agent.toml 真實值，
    使用者沒察覺；只有 `skill_auto_activate` 預設 `?? false` 與真實值衝突，
    才把這個顯示 bug 暴露出來。Sticker 區段也有同樣問題
  - Fix: 把 `evolution` + `sticker` 區段補進 `agents.list` response，與
    `agents.inspect` 對齊



## [1.12.2] - 2026-05-07

Dashboard 死局與假性「設定無反應」修復。使用者回報 Dashboard 設定幾乎無法
操作、任務無法操作；Telegram 與 Odoo 路徑正常。深入追查後發現 4 個獨立
問題交互疊加，本版一次解決。

### Fixed

- **JWT auto-refresh 缺失導致 WebSocket 死循環**（CRITICAL）
  - Symptom: gateway log 連續 4000+ 次 `WebSocket auth failed – closing connection`，
    最後一次成功認證 2026-05-06T02:17:52，之後 dashboard 全面失效
  - Root cause: access token TTL 30 分鐘，前端只在 `loadFromStorage` 啟動時
    呼叫一次 `/api/refresh`，過期後 WS 持續用過期 token 重連被拒
  - Fix: `auth-store` 加 25 分鐘 setInterval + `visibilitychange` listener；
    `ws-client` 加 `authRefreshHook`，handshake 失敗訊息含 `jwt`/`auth` 時
    下次 `doConnect` 前先 await refresh

- **重整頁面看不到資料、需切走再切回**（HIGH）
  - Symptom: 頁面 reload 後資料空白；切換頁面再切回才正常
  - Root cause: React effects 由葉子向根 commit，page useEffect 比 App
    `connectWithAuth` 早跑；`waitForReady` 在 state=disconnected & 無
    reconnectTimer 時 fast-reject `"Not connected"`
  - Fix: `AuthGuard` 多 gate 一層 `wsState === 'authenticated'`，protected
    route 在 WS 就緒後才 mount

- **agents.update 寫入後 registry 沒立刻 reload**（MEDIUM）
  - Symptom: 修改 agent 設定後使用者誤以為沒生效
  - Root cause: `update_agent_toml` 拿 registry write lock 用 500ms timeout
    但 timeout 後 silent fail，agent.toml 已寫入但記憶體 registry 沒重載
  - Fix: 改回傳 `Result<bool, String>`（bool = hot_reloaded），timeout / scan
    失敗時 `warn!` 一行；`agents.update` response 加 `"hot_reloaded": bool`
    與對應 message

- **per-agent channel token 變更不會 hot-restart bot**（MEDIUM）
  - Symptom: 修改 Discord/Telegram per-agent token 後，下次發訊息仍走舊
    token，需重啟 gateway
  - Root cause: bot 啟動時 capture token，registry rescan 不會觸發 bot 重啟；
    只有 `channels.add` / `channels.remove` RPC 走 hot-restart 路徑
  - Fix: 新增 `hot_restart_agent_channels(channel_types, agent_name)` helper；
    `handle_agents_update` 偵測到 `discord_bot_token` / `telegram_bot_token`
    入參時，寫檔成功後自動 hot-restart 對應 bot；response 加
    `"channels_restarted": [...]`。LINE 是 webhook 不需處理；Slack / WhatsApp
    / Feishu 仍需 gateway 重啟（缺 hot-restart helper）

### Notes

- 升版後第一次開啟 dashboard 仍需清除瀏覽器 localStorage 的
  `duduclaw-refresh-token` 重新登入，才能拿到走新 auth flow 的 fresh JWT。
- Telegram / Odoo / channel_reply 路徑本來就 OK，不受本版影響。



## [1.12.0] - 2026-05-06

W22 Sprint deliverables — two W22-P0 ADRs ship together with a multi-agent
coordination overhaul (RFC-22) driven by a 2026-05-04 → 2026-05-06 端到端
incident that exposed agnes silently fabricating sub-agent replies, autopilot
mass-firing on malformed events, and channel-path token usage going entirely
unrecorded.

### Added

#### W22-P0 ADR-002 — `x-duduclaw` capability negotiation

Every HTTP response from the MCP HTTP server now carries machine-readable
capability metadata, and clients can declare capability requirements that
trigger an early 422 rather than silent partial failures.

- **`mcp_headers.rs`** — `CAPABILITY_REGISTRY` static table (9 capabilities:
  `memory/3`, `mcp/2`, `audit/2`, `governance/1`, `skill/1`, `wiki/1` enabled;
  `a2a/1`, `secret-manager/1`, `signed-card/1` disabled/pending).
  `API_VERSION = "1.2"`. Builder/parser/negotiation functions. 23+ unit tests.
- **`mcp_capability.rs`** — `inject_capability_headers` outer middleware
  (appends `x-duduclaw-version` + `x-duduclaw-capabilities` to every
  response) and `negotiate_capabilities` inner middleware (returns 422
  Unprocessable Entity when client requirements unmet, with structured
  JSON body + `x-duduclaw-missing-capabilities` header). Permissive when
  header absent/empty/malformed. 11 Axum integration tests.
- **`mcp_http_server.rs`** — Both layers wired into `build_router()` with
  correct outer/inner ordering. Adds 11 integration tests for healthz,
  unauthorized 401, malformed JSON-RPC, and capability negotiation 422.
- **`docs/ADR-002-x-duduclaw-capability-negotiation.md`** — Full ADR.

#### W22-P0 ADR-004 — Secret Manager

Unified abstraction over three backends behind a `secret://<backend>/<name>`
URI scheme so MCP clients (Brave Search, Figma, Notion) can reference
credentials without embedding them in code or env vars.

- **`crates/duduclaw-security/src/secret_manager/`** — new module:
  - `mod.rs` — `SecretAdapter` async trait, `SecretUri` parser, config
    loader (`[secret_manager]` in `config.toml`), `Backend::Local|Vault|Env`.
  - `local.rs` — In-process AES-256-GCM encrypted store (dev/testing).
  - `vault.rs` — HashiCorp Vault KV v2 HTTP client (production), reads
    `vault_addr`, `vault_token`/`vault_token_enc`, `vault_mount`.
  - `env.rs` — Reads from process environment (CI/override).
- 26 unit tests covering URI parsing, config parsing, encrypted at-rest
  verification, error variants, cross-backend round-trips.

#### RFC-22 — Multi-agent coordination principles

- **`docs/RFC-22-multi-agent-coordination-principles.md`** — Four design
  decisions: (1-C) Two-tier Task/Wiki, (2-C) Hybrid spawn+bus fallback,
  (3-D) Channel mapping, (4-D) Hallucination forbidden + audit trail.
- **`crates/duduclaw-core/src/types.rs`** — `ChannelBinding { kind, id,
  description }` + `DiscordChannelConfig.bindings: Vec<ChannelBinding>` so
  per-thread routing can target sub-agents directly.
- **`crates/duduclaw-agent/src/resolver.rs`** — `AgentResolver` step-2
  channel/thread binding match between trigger word and coarse permission
  grant. 8 new unit tests.
- **`crates/duduclaw-security/src/audit.rs`** — `append_tool_call_with_extras`
  helper for attaching wiki authorship audit fields
  (`claimed_authors_in_content`, `matches_caller`, `actual_caller`).
- **`crates/duduclaw-cli/src/mcp.rs`** — `detect_claimed_authors_in_wiki`
  parses `## <agent> 的觀點`, `**回覆人**：<agent>`, signature, and
  frontmatter `claimed_authors:` patterns. Recorded on every
  `shared_wiki_write`. 6 new unit tests.

### Changed

- `x-duduclaw-version` bumped to `1.2` (second backward-compatible HTTP API change).
- **`crates/duduclaw-gateway/src/autopilot_engine.rs`** — `lookup_path_opt`
  returns `Option<Value>` so missing fields no longer match `eq null`,
  fixing the 5/5 mass-fire bug where 5 task_created events all triggered
  Rule A. `apply_op` short-circuits `None` to `false`. 4 regression tests
  (P1-9b).
- **`crates/duduclaw-gateway/src/channel_reply.rs`** — `build_system_prompt`
  now injects `CONTRACT.toml` boundaries via `contract_to_prompt`
  (P1-8 / P1-9a). `spawn_claude_cli_with_env` parses the result event's
  `usage` field and records via `cost_telemetry` against a
  `CHANNEL_REPLY_AGENT_ID` task_local set in `build_reply_with_session_inner`
  — channel replies now produce token usage rows (P1-7).
- **`crates/duduclaw-gateway/src/claude_runner.rs`** — adds
  `CHANNEL_REPLY_AGENT_ID` task_local for per-agent cost attribution.
- **`crates/duduclaw-cli/src/mcp.rs`** — MCP server boot log now logs
  `caller_agent` alongside `client_id` so observers can distinguish API
  key owner from actual sub-agent (P1-10). `handle_spawn_agent` surfaces
  underlying I/O error when `bus_queue.jsonl` write fails, with RFC-22
  reminder not to fabricate a reply (W1).
- **`crates/duduclaw-cli/Cargo.toml`** — `default = ["dashboard"]` so
  `cargo build -p duduclaw-cli --release` produces a binary whose
  dashboard SPA fallback is mounted (without this every HTTP path except
  `/health` and `/ws` returned 404).

### Tests

  duduclaw-gateway: 838 passed (incl. 4 new autopilot regression tests)
  duduclaw-agent:    39 passed (incl. 8 new resolver binding tests)
  duduclaw-cli:     365 passed (incl. 6 new wiki author + 13 HTTP transport)
  duduclaw-core:     80 passed
  duduclaw-security: 179 passed (incl. 26 new secret_manager tests)

  Total **1501 / 1501 green** across all crates.

### Hygiene

- **`.gitignore`** — adds `*.profraw` (cargo test residue),
  `docs/{tl,pm}/daily-report-*.md` (agent operational logs belong on
  shared wiki), `/research/` (researcher agent local notes), `/python/spikes/`
  (active spike workspaces, promoted to production on completion), `/uv.lock`.

---

## [1.11.0] - 2026-05-04

RFC-21 — Identity Resolution & Per-Agent Credential Isolation. Closes
[#21](https://github.com/zhixuli0406/DuDuClaw/issues/21) by addressing all
three architectural gaps the reporter identified: identity resolution
walked the shared wiki instead of an authoritative external source, Odoo
MCP credentials shared one global admin slot across every agent, and the
shared wiki had no source-of-truth boundary so an evolving agent could
silently overwrite externally-synced data. All three are now enforced at
the system layer (dispatcher / pool / namespace policy) instead of relying
on SOUL.md prompt-layer self-restraint.

### Added — `duduclaw-identity` crate (§1)

- **`IdentityProvider` async trait** + `ResolvedPerson` (`person_id`,
  `display_name`, `roles`, `project_ids`, `emails`, `channel_handles`,
  `source`, `fetched_at`) + `ChannelKind` enum (Discord / Line / Telegram
  / Slack / WhatsApp / Feishu / WebChat / Email + `Other(_)` catch-all
  with stable wire format) + `IdentityError` (Unreachable / Malformed /
  Unsupported / Io / Internal).
- **`WikiCacheIdentityProvider`** reads `<home>/shared/wiki/identity/people/*.md`
  per-person YAML frontmatter records; tolerates malformed files and
  missing optional fields; mtime-driven `fetched_at`.
- **`NotionIdentityProvider`** queries Notion `databases/query` with
  configurable `NotionFieldMap` (property names + `ProjectsKind`
  multi_select / relation). HTTP errors classify cleanly: 5xx /
  network ⇒ Unreachable (chained provider degrades), 4xx ⇒ Malformed.
- **`ChainedProvider`** combines cache + upstream — cache hit
  short-circuits; cache miss falls through; upstream unreachable
  degrades to `Ok(None)` rather than hard-erroring; project membership
  prefers upstream then falls back to cache.
- **`identity_resolve` MCP tool** + new `Scope::IdentityRead`
  ("identity:read") gates the tool. Audit row emitted per call.
- **`<sender>` XML block auto-injection** into channel reply system
  prompt (`crates/duduclaw-gateway/src/channel_reply.rs`). Sender is
  resolved once per turn; XML-escaped to keep the envelope intact;
  optional fields omitted when empty. Empty result ⇒ block omitted ⇒
  v1.10.1 behaviour preserved.

### Added — Per-agent Odoo credential isolation (§2)

- **`agent.toml [odoo]` override block** parsed via new
  `duduclaw-odoo::AgentOdooConfig`: `profile` / `username` /
  `api_key_enc` / `password_enc` / `allowed_models` /
  `allowed_actions` / `company_ids`. Empty / malformed block returns
  None; agent without override falls back to global config.
- **`OdooConfigResolver`** layers global + per-agent; `pool_key_for`
  produces stable `(agent_id, profile)` pool keys.
- **`OdooConnectorPool`** (new `crates/duduclaw-cli/src/odoo_pool.rs`)
  replaces the v1.10.1 global `Arc<RwLock<Option<OdooConnector>>>` with
  a `(agent_id, profile)`-keyed pool. Outer `RwLock<HashMap>` for
  membership reads + per-slot `tokio::sync::Mutex` for first-use
  connect serialisation. `get_or_connect(decrypt)` → cached
  `Arc<OdooConnector>` or cold-connect via merged credentials.
  `set_global` preserves per-agent overrides on hot-reload;
  `disconnect`/`disconnect_all`/`is_connected` complete the lifecycle.
- **`Scope::OdooRead` / `OdooWrite` / `OdooExecute`** added to
  `mcp_auth.rs`. All 14 `odoo_*` tools registered into
  `tool_requires_scope` — read class (status / connect / search /
  CRM leads / sale orders / inventory / invoice / payment), write
  class (create lead / update stage / create quotation), execute class
  (sale confirm / generic execute / report).
- **`allowed_models` / `allowed_actions` defence-in-depth filter** —
  `check_action_permission(verb, model)` runs before any HTTP request
  leaves the process; supports bare verbs (`"read"` → all models) and
  qualified verbs (`"write:crm.lead"` → only crm.lead). Policy denials
  audited as DENIED rows.
- **Audit attribution**: `tool_calls.jsonl` rows for Odoo calls now
  carry `params_summary = "profile=<profile>; tool=<name>; ok=<bool>"`
  so Odoo activity is traceable to the originating agent rather than
  the shared admin user inside Odoo's own audit log.
- **`handle_odoo_connect`** now reload-and-reconnect: re-reads
  `config.toml [odoo]` (set as global), re-reads
  `agents/<caller>/agent.toml [odoo]` (registers as override),
  forces `disconnect(caller)`, then `get_or_connect`. The connection
  report includes the resolved `(agent, profile)`.

### Added — Shared wiki SoT namespace policy (§3)

- **`~/.duduclaw/shared/wiki/.scope.toml`** declares which top-level
  namespaces are read-only / operator-only. Three modes:
  `agent_writable` (default — same as v1.10.1, no regression),
  `read_only { synced_from = "<capability>" }` (only the named internal
  capability or operator may write), `operator_only` (never writable
  via MCP).
- **Enforcement** in both `handle_shared_wiki_write` and
  `handle_shared_wiki_delete` — the namespace policy is the authority,
  not the per-page ACL. Read-only namespaces deny even the original
  page author from deleting.
- **`wiki_namespace_status` MCP tool** lets agents introspect the
  active policy before attempting a write.
- **Fail-safe**: absent file ⇒ empty policy ⇒ everything writable.
  Malformed TOML ⇒ logged warning + treated as no policy. Hot-reload
  is automatic — every write/delete re-reads the file (KB-sized; not
  on the hot path).
- **Reserved policy filename**: `.scope.toml` is implicitly rejected by
  the existing `.md` extension check in `validate_wiki_page_path`; no
  separate reserved-list entry needed.

### Added — Documentation

- **`docs/RFC-21-identity-credential-isolation.md`** — original design
  doc with three-section migration plan, acceptance criteria, risks,
  and rollout strategy.
- **`docs/RFC-21-operator-guide.md`** — step-by-step deployment
  playbook for all three sections, with verify commands, common
  pitfalls, and migration sequence from the v1.10.1 single-tenant
  deployment.
- **`docs/features/17-wiki-knowledge-layer.md`** updated with the
  namespace SoT policy section.
- **`CLAUDE.md`** Architecture Overview header bumped to v1.11.0; new
  bullets summarising RFC-21 §1 / §2 / §3 in the relevant sections.

### Tests

Cross four crates, **1193 unit + integration tests pass** with no
regression:

- `duduclaw-identity` 31/31 (15 wiki_cache + 7 chained + 9 notion) +
  1 doctest
- `duduclaw-odoo` 27/27 (15 new agent_config tests on top of existing
  12)
- `duduclaw-cli` 301/301 — 15 wiki_scope unit + 12 odoo_pool unit + 14
  odoo_pool_dispatch integration + 4 identity_resolve integration + 7
  new wiki_schema_tests for namespace policy enforcement
- `duduclaw-gateway` 834/834 (7 new sender_block tests)

### Backwards compat

Every section preserves v1.10.1 behaviour for deployments that don't
opt in:

- Absent `.scope.toml` ⇒ no namespace restrictions.
- Absent `[identity]` ⇒ no `<sender>` block; `shared_wiki_read` for
  identity continues to work.
- Absent `agent.toml [odoo]` ⇒ pool collapses to `(agent_id,
  "default")` slot using global config exactly as before.

No flag-day migration required.

### Commits

`867e719` (RFC) → `1a967f5` (§3) → `53e19a8` (§1 step 1-2) → `5c0b116`
(§1 step 4) → `a17ba5a` (§2) → `9a40c18` (§1 step 3) → `3269ca0`
(operator guide + status reflection) → `<this commit>` (v1.11.0 release).


## [1.10.1] - 2026-05-04

### Fixed — Release pipeline
- **PyPI publish 失敗修正**：`pyproject.toml` 仍停留在 `1.8.0`（自 v1.8.0 release 後未隨 workspace 同步），導致 v1.10.0 release workflow 嘗試重複上傳已存在的 `duduclaw-1.8.0-py3-none-any.whl`，被 PyPI 拒以 `400 File already exists`。本版同步將 Python SDK 版本提升至 `1.10.1`，與 Cargo workspace 對齊。
- **`pypa/gh-action-pypi-publish` 加上 `skip-existing: true`**：未來若同一版本被重新觸發（workflow_dispatch 重跑、tag 重推），PyPI 步驟會跳過而非整個 release job 失敗。Trusted Publisher 與 token fallback 兩條路徑都套用。

### 內容差異
- v1.10.0 的 GitHub Release 二進位、npm 套件已成功發佈；本 patch 主要是把 PyPI 的 `duduclaw` 套件補上來，並順帶 bump 一個 Cargo workspace patch 版本以走完整 release pipeline。Rust / web 程式碼相對 v1.10.0 無新增功能。


## [1.10.0] - 2026-05-03

### Added — Wiki RL Trust Feedback（核心新功能）
- **`duduclaw-memory` 新增** `trust_store.rs` / `feedback.rs` / `janitor.rs` — 預測誤差驅動的 wiki 信任反饋系統。
  - `WikiTrustStore`（SQLite，PK `(page_path, agent_id)` 每 agent 獨立 trust）
  - `CitationTracker` 用 turn_id 為 drain key、session_id 為 cap budget key（兩級 id），LRU + bounded-time 雙條件 eviction 防 keep-alive DoS
  - `WikiJanitor` 每日 pass：3 negatives in 30d 加 `corrected` tag、隔離 30d 後 archive 至 `wiki/_archive/`、frontmatter ↔ live trust 同步
  - 防禦：per-page daily cap (10/day)、per-conv Δ cap (0.10)、`VerifiedFact` ×0.5 抗性、`lock=true` 人工 override、0.10/0.20 archive hysteresis
- **`duduclaw-gateway` 新增** `prediction/feedback_bus.rs` / `wiki_trust_federation.rs` — `TrustFeedbackBus` 在每次 `PredictionError` 後 drain `CitationTracker` 並 dispatch 簽名 deltas（error < 0.20 → positive、≥ 0.55 → negative）；GVU 結果以 2× magnitude 經 `on_gvu_outcome` 進信任反饋。
- **Federation 同步**（Q3）：trust 信號可跨機 export/import，衝突取均值、`do_not_inject` 取 OR、`schema_version` 拒絕未來版本、5000 updates/push + 1 MiB body 上限 + `constant_time_eq` bearer。
- **MCP 工具**：`wiki_trust_audit` / `wiki_trust_history`；RPC `wiki.trust_audit / trust_history / trust_override`。
- **Search ranking** 改為 `score × (0.5 + live_trust) × source_type_factor`（verified_fact ×1.2，raw_dialogue ×0.6）。
- **Web** 新增 `WikiTrustPage.tsx` 儀表板（trust 列表、history、override、archive 操作）。
- 文件：[docs/wiki-trust-feedback.md](docs/wiki-trust-feedback.md) runbook + 架構說明。

### Added — v1.10 收尾
- **Sub-agent enqueue turn_id 完整貫通**：`DUDUCLAW_TURN_ID` / `DUDUCLAW_SESSION_ID` 兩個 env var 常數，gateway spawn Claude CLI 時 set，MCP `send_to_agent` 讀 env 並寫入 `message_queue.{turn_id, session_id}`，dispatcher 從 queue 讀回後重新 scope。channel → 頂層 agent → MCP send_to_agent → SQLite queue → dispatcher → 子 agent CLI 全鏈 turn_id/session_id 正確傳遞。
- **`flock` for `wiki_trust.db`**：advisory file lock 防多 process 共用 home_dir 造成 archive race / frontmatter 競爭，第二個 process fail-fast 並回明確錯誤。
- **Atomic batch upsert（真正單 Tx）**：`WikiTrustStore::upsert_signal_batch` 一次 `BEGIN IMMEDIATE` 處理整批；32 citations / 1 prediction error 從 32 fsync 收斂為 **1 fsync**；任何中途錯誤自動 rollback。原本延後到 v1.11 的計畫**提前在 v1.10 完成**。
- **ABS migration once-only**：`wiki_trust_meta` 標記 conv_cap ABS migration 已完成，避免每次 boot 全表掃描。

### Schema migration
- `message_queue.turn_id` / `message_queue.session_id` columns 自動新增（既有資料庫升級時 NULL，新訊息會帶值）
- `wiki_trust_meta(key, value)` 新表 + `conv_cap_abs_migration_done` 標記
- `wiki_trust_state` / `wiki_trust_history` / `wiki_trust_rate` / `wiki_trust_conv_cap`（PK rename `conversation_id` → `cap_budget_id`）/ `idx_wiki_trust_history_agent_kind_ts` / `idx_wiki_trust_history_ts`

### Tests
- Backend **126 tests pass**（duduclaw-memory），包含 5 個 v1.10 regression test：flock、batch order、batch cap-budget shared、batch single-Tx、migration once-only
- 5 輪深度審查（code / security / database / architecture）+ Round 5 SHIP-BLOCK 修復全數收斂


## [1.9.4] - 2026-05-02

### Added
- **`duduclaw-durability` crate** — five-pillar durability framework:
  `idempotency` (key 管理防止重複執行)、`retry`（指數退避 + jitter）、
  `circuit_breaker`（三態 Closed/Open/HalfOpen）、`checkpoint`（任務進度
  斷點續傳）、`dlq`（Dead Letter Queue 終態失敗訊息）。完整 unit +
  integration tests 涵蓋高並發場景。
- **`duduclaw-governance` crate**（W19-P1 M1-A）— PolicyRegistry +
  4 種 PolicyType（Rate / Permission / Quota / Lifecycle）+ YAML 載入 +
  熱重載 + Agent 優先序合併 + fail-safe（非法政策跳過、非法 YAML 不
  panic）+ 並發 upsert 安全。新增 `quota_manager.rs`（每 agent / 每
  policy 配額 soft/hard 強制）+ `error_codes.rs`（QUOTA_EXCEEDED /
  POLICY_DENIED 等標準化錯誤碼）+ `evaluator` / `violation` /
  `approval` / `audit` 完整 PolicyEngine。預設政策集 `policies/global.yaml`
  含 default-rate-mcp（200/min MCP 呼叫限制）等六項。
- **MCP HTTP/SSE Transport**（W20-P1/P2）— 新增 `duduclaw http-server
  --bind 127.0.0.1:8765` 子命令。`mcp_http_server.rs` 提供
  `POST /mcp/v1/call`（單次 JSON-RPC 2.0 工具呼叫）、
  `GET /mcp/v1/stream`（SSE 長連接事件流，Bearer / `?api_key=`）、
  `POST /mcp/v1/stream/call`（async + SSE 結果推送）、`GET /healthz`
  （無需認證）。`mcp_rate_limit.rs` 新增 `OpType::HttpRequest`（60
  req/min token bucket），`mcp_sse_store.rs` 連線管理與 broadcast
  channel 事件推送，`mcp_http_auth.rs` / `mcp_http_errors.rs` 處理
  認證 + JSON-RPC↔HTTP 錯誤映射。
- **`skill_synthesis_run` MCP tool**（W20-P0）— Internal principal 可見、
  external 隱藏。`pipeline.rs::graduate_trajectories()` 取代 Phase 2
  stub，串起 memory_search → skill_extract → security_scan →
  skill_graduate 完整流程。
- **`duduclaw-memory` 評測 batch query API** — 新增 `MemoryEngine`
  方法支援評測批次查詢，配合 LOCOMO 評測系統。
- **LOCOMO 記憶評測系統**（W21）— `python/duduclaw/memory_eval/`：
  `retrieval_accuracy` / `retention_rate` / `locomo_integrity_check`
  + `cron_runner`（每日 03:00 UTC 排程）+ 5 分鐘 `smoke_test` P0 +
  `build_golden_qa`（從 LOCOMO 資料集建構黃金 QA）+
  `data/golden_qa_set.jsonl`（首批 200 筆 golden QA）+ `client.py` /
  `config.py` / `db/consolidation.py`。
- **Python `agents/` + `mcp/` 模組** — `agents/capabilities/`
  （manifest 載入 + matcher）、`agents/routing/`（capability-based
  router + resolution + memory_resolver）；`mcp/auth/`（API Key 驗證
  含 key masking 防洩漏）、`mcp/tools/memory/`（store / read / search
  / namespace / quota 含 scope 強制驗證）。
- **LLM Fallback** — `claude_runner.rs` + `llm_fallback.rs`：主模型
  逾時 / 503 / 429 / overloaded 時自動切換 fallback 模型。新增
  `is_llm_fallback_error` / `should_attempt_model_fallback` 純函式
  + 完整 unit tests。
- **Evolution Events 系統擴充** — `schema.rs` 新增 30+ event schema
  定義（+483 行）、`emitter.rs` 非同步發送支援 batch + retry（+190
  行）、新增 `query.rs`（EvolutionEvent 查詢介面，1685 行）+
  `reliability.rs`（事件可靠性保證機制，324 行）。Gateway HTTP
  endpoints 暴露於 `handlers.rs`（+154 行）。
- **Web `ReliabilityPage`**（+328 行，`/reliability` 路由）— circuit
  breaker 狀態、retry 統計、DLQ 佇列深度即時儀表板。`api.ts` 新增
  `getEvolutionEvents` / `getReliabilityStats` / `getDlqItems`。
- **`duduclaw evolution finalize` CLI 子命令**（v1.9.1 引入，v1.9.4
  封版穩定）— `--dry-run` / `--agent <id>`，一次性回收逾期 SOUL.md
  觀察視窗。
- **`claude_desktop_config.example.json`** — Claude Desktop MCP Server
  整合設定範例。

### Fixed (W21 QA 4-round CRITICAL/HIGH 全清)
- **CRITICAL — 記憶 MCP scope 認證缺口**：`mcp/tools/memory/store.py`、
  `read.py`、`search.py` 在 `execute()` 進入點補上 `memory:write` /
  `memory:read` scope 強制檢查。修補先前任意有效 API Key 都能繞過
  scope 限制的認證缺口。
- **HIGH — XSS 儲存型注入**：`validation.py::validated_tags` 改用
  `_sanitize(tag)` 處理使用者輸入的 tag。
- **HIGH — SSRF 防護**：`client.py::build_client()` 新增 URL
  scheme/netloc 驗證，拒絕指向內網或私有位址的 URL。
- **HIGH — circuit breaker 幽靈探測**：`circuit_breaker.rs`
  OPEN→HALF_OPEN 轉換時補上 `probe_inflight.saturating_add(1)`。修復
  並發探測數比設計上限多 1 的 bug。
- **HIGH — `claude_runner.rs` hard deadline 邏輯**：移除 partial
  output 時 `break` 的分支，統一回傳含 "hard timeout" 字串的 `Err`，
  確保 `is_llm_fallback_error` 正確觸發 fallback。
- **HIGH — UTF-8 truncation panic**：`llm_fallback.rs` truncation 改用
  `char_indices` 安全 UTF-8 char boundary 切片。修復多位元組字元在
  byte 512 邊界處切割時的 runtime panic。
- **Web 高危依賴**：`vite` 8.0.0-8.0.4 → 8.0.5+（GHSA-4w7w-66w2-5vf9
  + GHSA-v2wj-q39q-566r + GHSA-p9ff-h696-f583：Path Traversal in
  Optimized Deps、`server.fs.deny` bypass、Arbitrary File Read via
  WebSocket）；`postcss` <8.5.10 → 8.5.10+（GHSA-qx2v-qp2m-jg93：XSS
  via Unescaped `</style>` in CSS Stringify Output）。npm audit 0
  vulnerabilities。
- **Inference 編譯**：`ProgressCallback` 補上 `Sync` trait bound，修復
  多執行緒共享場景編譯錯誤。

### Tests
- 549+ tests, 0 failures（包含 `duduclaw-durability`、
  `duduclaw-governance` 73 tests + integration 22 個 W19-P1 M1-A
  驗收項、MCP HTTP transport tests、LLM fallback unit tests、Python
  agents routing + memory MCP tools 含 api_key_masking 安全測試）。

### Build/Repo
- `.gitignore` 排除 Python coverage db (`.coverage` /
  `**/.coverage`)、`release artifacts/`、各平台 `npm/*/bin/` 預建
  binary（應透過 npm publish）。
- `pyproject.toml` 更新 Python 依賴版本（memory_eval / agents / mcp
  相關套件）。


## [1.9.3] - 2026-04-28

### Fixed
- **Heartbeat: task-board pull 對所有 agent 生效，無視 enabled flag**。
  `poll_assigned_tasks` 之前在 `execute_heartbeat` 內，僅當 agent 心跳
  config `enabled=true` 才會跑。生產環境 17 個 agent 中有 16 個預設
  `enabled=false`，於是新加的 task board pull 對最需要它的 agent 從
  未觸發 — 包括 2026-04-28 12:27 觀察到的 26 個未路由 backlog 任務。
  修正：將 pull 上移到 `HeartbeatScheduler::run` 的 tick body，每 30s
  掃描整個 agent registry。`poll_assigned_tasks` 原有的 1-hour LIKE
  marker cooldown 已防止 stampede。task board pull 概念上屬 scheduler
  層級而非 per-agent evolution，agent 不該為了被指派工作時被叫醒而
  opt-in。


## [1.9.2] - 2026-04-28

### Fixed
- **Discord Gateway: 真正實作 RESUME (op 6) + stall watchdog**
  （`discord.rs`）。
  - 持久化 `session_id` + `resume_gateway_url` + sequence 跨重連。
    先前每次重連都發新的 IDENTIFY，丟掉 Discord 在斷線期間緩衝的所有
    事件。
  - 第三個 `select!` arm 加入 stall watchdog：超過 2× heartbeat
    interval 沒有任何流量就 break。修復 2026-04-28 11:17Z 觀察到的
    silent zombie 狀態，gateway loop 卡住 18 分鐘無任何 log 輸出。
  - heartbeat channel capacity `1 → 16` + `try_send` 防止 `select!`
    消費慢時反向阻塞。
  - Op 9 Invalid Session 讀 `d.bool` 決定 RESUME vs IDENTIFY，依
    Discord docs 加 1-5s jitter。
  - close codes 4007/4009/4003 清掉 session state 觸發新 IDENTIFY。
  - backoff cap 300s → 60s；不要懲罰已經跑了好幾小時的 session。
  - 處理 `RESUMED` dispatch event。


## [1.9.1] - 2026-04-28

### Added
- **`duduclaw evolution finalize` CLI subcommand** with `--dry-run` and
  `--agent <id>` filters. One-shot recovery for SOUL.md observation
  windows that should already have transitioned but never did.

### Fixed (self-evolution pipeline — 5 audit gaps from 2026-04-28 health check)
- **SOUL.md observation windows now actually close.**
  `VersionStore::get_expired_observations` and `Updater::execute_confirm /
  execute_rollback` had no callers, so the very first applied SOUL change
  blocked all subsequent GVU proposals indefinitely. agnes was stuck for
  6 days locally. Adds a 30-min `ObservationFinalizer` background task
  that computes post-metrics from `prediction.db` + `feedback.jsonl`,
  runs the existing `judge_outcome` tolerance logic, and confirms /
  rolls back / extends accordingly.
- **EvolutionEvents audit log now writes to a stable absolute path.**
  Default base directory was `data/evolution/events` — relative to cwd.
  Gateway boot from `cwd=$HOME` silently dropped every audit event. Now
  resolves via layered fallback: `$EVOLUTION_EVENTS_DIR` →
  `$DUDUCLAW_HOME/evolution/events` → `$HOME/.duduclaw/evolution/events`
  → legacy. Boot also injects the env var before any emitter is
  constructed and runs a `.healthcheck` self-test that surfaces IO
  failures via `tracing::error!` instead of silent `eprintln!`.
- **Silence breaker now actually triggers a forced reflection.**
  `heartbeat.rs` previously only emitted `warn!` and reset its own timer
  — the system advertised "self-reflection on long silence" but never
  did anything. Adds a `SilenceBreakerEvent` mpsc channel; the gateway
  consumes it and writes a typed `silence_breaker` row to
  `prediction.db.evolution_events`, with a 4-hour per-agent cooldown to
  prevent loops.
- **MetaCognition rehydrates counters from `prediction.db` on startup.**
  `total_predictions` and `predictions_since_last_eval` were stuck at 0
  across restarts because `metacognition.json` only persisted at
  evaluation time. With `evaluation_interval=100` the threshold became
  unreachable and adaptive thresholds never recalibrated. Now takes
  `max(disk, in-memory)` and runs a one-shot `evaluate_and_adjust` if
  the in-memory counter is overdue. Also anchors
  `original_sig_improvement_rate` baseline on the first eval that has
  ≥5 Significant samples (was previously stuck at `null`).
- **Sub-agent dispatches now record prediction samples.**
  `prediction.db.user_models` had only the channel-facing root agent
  (1/19 in our deployment); 18 sub-agents accumulated nothing because
  the prediction hook only ran in `channel_reply`, not in
  `dispatcher.rs`. Adds a fire-and-forget `subagent_prediction` module
  that synthesises `user_id = "agent:<sender_or_origin>"`, builds a
  2-message `ConversationMetrics` snapshot from the dispatched payload
  + response, and runs the same `predict → calculate_error →
  log_evolution_event → update_model` cycle as the channel path. Hooks
  both the JSONL and SQLite dispatch loops; deliberately does NOT
  trigger the GVU loop from this path (preserves the channel-only
  invariant for SOUL evolution).

### Tests
- 23 new unit tests across `observation_finalizer`, `evolution_events::logger`,
  `prediction::forced_reflection`, `prediction::metacognition` (BUG-4 group),
  and `prediction::subagent_prediction`.
- Workspace tests after the change:
  duduclaw-gateway 730 ✓, duduclaw-agent 31 ✓, duduclaw-cli 80 ✓.

### Dashboard
- ActivityFeed no longer crashes when the gateway emits an unknown
  `ActivityType`. Adds explicit entries for `autopilot_triggered` and
  `autopilot_lag`, plus a neutral `FALLBACK_CONFIG` so future unknown
  types render as a generic row instead of throwing on `config.icon`.


## [1.8.34] - 2026-04-27

### Fixed
- **Local-fallback path silently failed for users running a remote
  OpenAI-compatible inference server (vLLM / SGLang / llamafile).**
  Reproducer: Linux gateway with no Claude CLI installed,
  `inference_mode = "local"` in `config.toml`, and `[openai_compat]`
  pointing at `http://192.168.168.244:8000/v1` in `inference.toml`.
  Sending a message via the dashboard webchat returned
  `DuDu 暫時無法回應：系統找不到 Claude Code CLI` even though the
  remote vLLM endpoint was reachable and the model id matched.

  Root cause: `InferenceEngine::load_model` unconditionally called
  `ModelManager::resolve_path`, which only finds GGUF files under
  `~/.duduclaw/models/`. For remote backends the model lives on a
  server, so `resolve_path` returned `ModelNotFound` and the engine
  errored before `OpenAiCompatBackend` ever saw the request — making
  the `channel_reply` local-fallback path silently fail with the
  misleading "Claude Code CLI not found" final message.

  Gateway log evidence:
  ```
  WARN duduclaw_inference::engine: Failed to auto-load model
    model="qwen3.6-35b-a3b" error=Model not found: qwen3.6-35b-a3b
  WARN duduclaw_gateway::channel_reply: Local inference unavailable:
    Local inference error: Model not found: qwen3.6-35b-a3b
  WARN duduclaw_gateway::channel_reply: Channel reply fallback —
    all providers failed agent=DuDu reason=BinaryMissing
    last_error=claude CLI not found in PATH
  ```

  Fix: add `InferenceBackend::requires_local_file` (default `true`,
  override `false` in `OpenAiCompatBackend`) and gate `resolve_path`
  on it. Remote backends now receive the raw model id, which matches
  what `OpenAiCompatBackend::load_model` already does (ignores the
  path arg and uses `[openai_compat].base_url + .model` from
  `inference.toml`).

  Adds two regression tests in `engine::tests` using a stub backend:
  - `load_model_skips_path_resolution_for_remote_backends`
  - `load_model_still_resolves_path_for_local_backends`

  Workaround for users on ≤ 1.8.33: `touch
  ~/.duduclaw/models/<model-id>.gguf` to satisfy the path check.
  Safe to delete after upgrading to 1.8.34.


## [1.8.33] - 2026-04-27

### Fixed
- **Windows: BatBadBut spawn error persisted on hosts where the
  `@anthropic-ai/claude-code` npm package ships a native `.exe`
  instead of a JS CLI.** The customer reproducer on 2026-04-27
  (after v1.8.32 still failed) revealed the `claude.cmd` shim
  contents:

  ```bat
  @ECHO off
  GOTO start
  :find_dp0
  SET dp0=%~dp0
  EXIT /b
  :start
  SETLOCAL
  CALL :find_dp0
  "%dp0%\node_modules\@anthropic-ai\claude-code\bin\claude.exe"   %*
  ```

  `@anthropic-ai/claude-code` ≥ 2.x ships a real `claude.exe` inside
  the npm package and the cmd shim is just a transfer wrapper. The
  v1.8.32 shim parser only matched `.js`/`.mjs`/`.cjs` references,
  returned `None` for the `.exe` line, fell through to known-layout
  probes (which also only checked for `cli.js` / `cli.mjs`), returned
  `None` there too, and the caller spawned the `.cmd` directly →
  BatBadBut. The diagnostic log added in v1.8.32 confirmed it:

  ```
  INFO Resolved claude binary
    path=C:\Users\USER\AppData\Roaming\npm\claude.cmd
    candidates=[..., "...\\claude.cmd"]   ← no .exe in pool
  WARN claude CLI spawn error: batch file arguments are invalid
  ```

  **Fix**: extend the shim parser and probe table to follow shims
  that point to a real `.exe` (not just JavaScript scripts). Three
  rule changes in [`platform::resolve_cmd_shim`](crates/duduclaw-core/src/platform.rs):

  1. `clean_shim_token` now matches `.exe` in addition to
     `.js`/`.mjs`/`.cjs`. The result is typed:
     `ShimTarget { kind: Exe | Script, rel: String }`.

  2. **Per-line target selection rule**:
     - Line has BOTH `.exe` AND a script → **Script wins** (the
       `.exe` is the runtime — `node.exe` / `bun.exe` — and the
       script is the actual target). Handles Bun / pnpm / yarn
       JS shims.
     - Line has ONLY `.exe` → **Exe wins** (new-style native shim;
       the `.exe` IS the target). Handles the customer's case.
     - Line has ONLY a script → **Script wins** (legacy npm shims).

  3. `known_cli_subpaths` → `known_target_subpaths` now contains 5
     native-`.exe` probes covering npm / yarn / Bun / pnpm globals —
     each terminating at `node_modules/@anthropic-ai/claude-code/bin/claude.exe`.
     Legacy `cli.js` / `cli.mjs` probes are retained for older
     installs.

  After this change, the customer's spawn path becomes:
  `Command::new("C:\\Users\\USER\\AppData\\Roaming\\npm\\node_modules\\@anthropic-ai\\claude-code\\bin\\claude.exe")` —
  a direct `.exe` invocation with zero `cmd.exe` involvement and
  zero BatBadBut hazard, regardless of prompt content.

### Changed
- `resolve_cmd_to_node` (private) renamed to `resolve_cmd_shim` and
  now returns `Option<(String, Vec<String>)>` — a real executable
  plus prefix args — so callers can spawn either a direct `.exe`
  (`vec![]`) or `node + cli.js` (`vec![cli.js]`) uniformly.
  `command_for` / `async_command_for` updated accordingly.

### Tests
- Shim parser tests overhauled around the new `parse_shim_target`
  API. 14 cross-platform unit tests now cover:
  - the new-style native-`.exe` shim (the customer's exact
    `claude.cmd` content reproduced verbatim),
  - legacy JS shims for npm v9 / Bun / pnpm / yarn classic,
  - the **Script-wins-over-Exe-when-both-present** priority rule,
  - the multi-token-per-line ordering for both `.exe` and `.js`,
  - the empty-shim, unquoted-hand-written, and `.cjs` extension
    edge cases,
  - a `known_target_subpaths_cover_native_and_legacy` assertion
    that the probe table contains ≥4 native-`.exe` probes and ≥4
    JS probes, all targeting `@anthropic-ai/claude-code`.


## [1.8.32] - 2026-04-27

### Fixed
- **Windows: BatBadBut spawn error persisted after v1.8.31 because
  `which_claude` short-circuited on `where.exe` results before
  HOME-rooted candidates were consulted.** v1.8.31 reordered the HOME
  candidate list so `.exe` came before `.cmd`, but missed the more
  fundamental bug: [`which_claude`](crates/duduclaw-core/src/lib.rs)
  ran `where.exe claude` first and **returned the first matching
  `.exe` OR `.cmd` line**, never reaching the HOME scan. On hosts
  with both a clean `~/.local/bin/claude.exe` install AND a leftover
  `%APPDATA%\npm\claude.cmd`, `where.exe` typically returned the
  `.cmd` first when PATH included `%APPDATA%\Roaming\npm` (which it
  often does for service / launchd / Explorer-launched processes
  even though the user's interactive shell shows it empty). The
  `.cmd` then triggered Rust 1.77+'s
  [BatBadBut][batbadbut] rejection (CVE-2024-24576) for any prompt
  containing newlines / quotes / `&` — i.e. essentially every prompt.

  [batbadbut]: https://blog.rust-lang.org/2024/04/09/cve-2024-24576/

  **Fix**: `which_claude` now **pools** results from PATH discovery
  AND the HOME-rooted scan (deduped), then applies a strict
  precedence regardless of source:

  1. any `.exe` in the pool wins (always safe to spawn)
  2. then any `.cmd` (parsed by `resolve_cmd_to_node` into
     `node.exe + cli.js` to avoid handing args to `cmd.exe`)
  3. then extensionless paths with `.exe`/`.cmd` appended via FS check
  4. last resort: first existing entry as-is

  On the customer machine that was failing in v1.8.31, this means
  `where.exe claude` returning `%APPDATA%\Roaming\npm\claude.cmd`
  AND the HOME scan finding `~/.local/bin/claude.exe` now resolves
  to the `.exe` — bypassing the BatBadBut hazard entirely.

### Added
- **One-shot `INFO` log of the resolved `claude` binary path on the
  first `which_claude` call.** The log line includes both the chosen
  path and the full discovery pool. This means future Windows /
  multi-installer issue reports arrive with the resolved path
  already in the logs:

      INFO duduclaw_core: Resolved claude binary
        path="C:\\Users\\X\\.local\\bin\\claude.exe"
        candidates=["C:\\Users\\X\\AppData\\Roaming\\npm\\claude.cmd",
                    "C:\\Users\\X\\.local\\bin\\claude.exe"]

  Subsequent `which_claude` calls (there are 11 call sites — channel
  reply, account rotation, heartbeat, etc.) are silent so this never
  becomes log spam.

### Tests
- 7 new cross-platform unit tests in `which_claude_tests` exercise
  the new precedence rules:
  `windows_pref_exe_beats_cmd_even_when_cmd_listed_first`,
  `windows_pref_picks_cmd_when_no_exe_exists`,
  `windows_pref_returns_none_for_empty_pool`,
  `windows_pref_first_exe_wins_among_multiple_exes`,
  `windows_pref_first_cmd_wins_among_multiple_cmds_when_no_exe`,
  `windows_pref_extension_check_is_case_insensitive` (handles
  uppercase `.EXE` / `.CMD` from PATHEXT-style discovery), and
  `windows_pref_falls_back_to_first_for_extensionless_when_no_fs_match`.

  Compile-gated with `#[cfg(any(windows, test))]` on the helper
  `pick_windows_preferred` so macOS / Linux CI runners can validate
  the Windows-only logic without needing a Windows host.


## [1.8.31] - 2026-04-27

### Fixed
- **Windows: `claude CLI spawn error: batch file arguments are
  invalid` blocking every channel reply.** Rust 1.77+ rejects spawning
  `.bat`/`.cmd` files when argv contains characters that could be
  reinterpreted by `cmd.exe` (newlines, quotes, `&`, `|`, …) — the
  [BatBadBut][batbadbut] mitigation for CVE-2024-24576. User prompts
  and system prompts routinely contain those characters, so `claude
  -p` subprocess calls failed at spawn time on every Windows host
  whose `which_claude` resolved to `%APPDATA%\npm\claude.cmd` (or any
  other npm/Bun/pnpm/yarn `.cmd` shim). The rotator interpreted the
  spawn failure as an account error, retried each account in turn, and
  surfaced the misleading `All accounts exhausted` to the user.

  [batbadbut]: https://blog.rust-lang.org/2024/04/09/cve-2024-24576/

  **Two-layer fix in `duduclaw-core`:**

  1. [`which_claude_in_home`](crates/duduclaw-core/src/lib.rs) on
     Windows now **prefers `.exe` over `.cmd`** in candidate ordering.
     A host with both a real `.exe` install (e.g. Claude Code native
     installer at `~/.local/bin/claude.exe`) and a leftover npm
     `.cmd` shim previously matched the `.cmd` first and tripped
     BatBadBut. Reordered so every `.exe` location is checked before
     any `.cmd`. Also added the **`~/.local/bin/claude.exe`** path
     (the official native installer's XDG-style location on Windows,
     previously missing) plus pnpm / Yarn-classic / Bun-`.cmd` /
     Volta-`.cmd` fallbacks.

  2. [`platform::resolve_cmd_to_node`](crates/duduclaw-core/src/platform.rs)
     — the npm-shim parser that converts a `.cmd` shim into a
     `node.exe + cli.js` invocation (so we never hand args to
     `cmd.exe`) — previously only matched paths containing
     `node_modules` ending in `.mjs`/`.js`. Bun (`..\packages\…`),
     pnpm (`..\global\5\node_modules\…`), and Yarn classic
     (`..\lib\node_modules\…`) all parsed as `None` and fell through
     to the BatBadBut path. New parser scans every quoted segment +
     every whitespace token, expands `%~dp0` / `%dp0%` / `%~dpn0` /
     `%~f0` / `%CD%` to empty, normalizes `\` to `/` for
     cross-platform path joining, accepts `.cjs`, and picks the
     *last* JS token per line so wrapper scripts don't shadow the
     real `cli.js`. When parsing still fails (binary wrappers, custom
     shims), a known-layout probe checks 6 well-known relative paths
     from the shim directory to `@anthropic-ai/claude-code/cli.js`
     for npm / Bun / yarn / pnpm.

  **Diagnostic note**: `where claude` returning empty on the customer
  machine was a red herring — `which_claude`'s HOME-rooted candidate
  scan still found `~/.local/bin/claude.exe`. The actual root cause
  was the `.cmd`-before-`.exe` ordering shadowing it.

### Tests
- 11 new cross-platform unit tests in `platform::shim_parser_tests`
  exercise npm v9 / Bun / pnpm / Yarn-classic shim formats, the
  pure-`.exe`-wrapper case, multi-`.js`-per-line ordering, `.cjs`
  extension handling, and unquoted-token fallback. Compile-gated with
  `#[cfg(any(windows, test))]` so they run on macOS/Linux CI hosts
  and validate the parser without needing a Windows runner.


## [1.8.30] - 2026-04-24

### Fixed
- **Native Claude Code tools (`WebSearch` / `WebFetch` / `Read` /
  `Write` / `Edit` / `Glob` / `Grep` / `Bash` / `TodoWrite`) were
  silently unavailable to `claude -p` subprocesses**, causing
  researcher cron tasks to receive 0 results and bail out even when
  the same tools worked in interactive Claude Code sessions.

  **Root cause**: [`claude_runner.rs`](crates/duduclaw-gateway/src/claude_runner.rs)
  passed `--allowedTools "mcp__duduclaw__*"` to `claude -p`. Claude
  Code treats `--allowedTools` as an **exclusive** auto-approve list,
  not an *additive* one: anything not matching would need interactive
  confirmation, which is impossible in subprocess mode. The built-in
  tools therefore returned empty / no-oped with no error signal.

  User-visible symptom (from the 2026-04-24 evening cron run): the
  `ai-papers-researcher` / `ai-repos-researcher` agents correctly
  followed their updated SOUL.md and cron prompts (which now direct
  them to use native `WebSearch` instead of the DDG-blocked MCP
  `web_search`), invoked `WebSearch`, got 0 results, and — per the
  hard-stop rule — aborted with "搜尋工具失效" inside six seconds.
  The equivalent query run interactively via Claude Code returned
  normal results immediately.

  **Fix**: expand the `--allowedTools` list to explicitly include the
  native tool names researchers actually need:

      mcp__duduclaw__*,WebSearch,WebFetch,Read,Write,Edit,
      Glob,Grep,Bash,TodoWrite

  This keeps the deny-by-default posture for anything not listed
  (e.g. no `KillBash` / `NotebookEdit` / etc.) while restoring the
  research capability that interactive Claude Code has had all along.
  `disallowed_tools` from `agent.toml [capabilities]` still layers on
  top via `--disallowedTools`, so explicit per-agent blocks are
  unchanged.


## [1.8.29] - 2026-04-24

### Fixed
- **Misleading "No auth token configured" startup banner.** The CLI
  always printed that message whenever `DUDUCLAW_AUTH_TOKEN` and
  `[gateway].auth_token` were both unset — but the WebSocket auth gate
  in `server::handle_socket` *also* requires JWT when `users.db`
  contains any rows (legacy `auth_token` and JWT are independent gates).
  Operators saw the message, assumed authentication was off, and then
  got spammed with `WebSocket auth failed – closing connection` once
  per second as the dashboard reconnected — with no hint that the real
  fix was to log in at `/login`.

### Changed
- [`duduclaw run`](crates/duduclaw-cli/src/lib.rs) now probes
  `~/.duduclaw/users.db` at startup (via `probe_users_db`). When any
  user exists the banner switches from "no auth token" to:

  ```
  🔐 JWT auth required: N user(s) in ~/.duduclaw/users.db
    Dashboard login: http://localhost:PORT/login
  ```

  so the correct next action is obvious.

- When `admin@local`'s stored password hash still verifies against the
  literal `"admin"` seeded by
  `duduclaw_auth::UserDb::ensure_default_admin`, an additional line
  warns: `⚠ Default admin still in use: admin@local / admin — change the
  password at /settings`. The verification uses the `argon2` crate
  directly (now a direct `duduclaw-cli` dep) rather than the full
  `duduclaw-auth` crate to keep the CLI's dependency surface narrow.

### Added
- 6 new unit tests in `startup_probe_tests` covering: missing
  `users.db`, empty users table, default-admin detection,
  non-default-password non-detection, admin@local absence, and
  garbage-PHC input handling.


## [1.8.28] - 2026-04-24

### Fixed
- **Cron notifications failed silently with Discord 401 Unauthorized
  in multi-bot setups.** When a cron-fired agent (e.g. `xianwen-pm`,
  `ai-papers-researcher`) had no per-agent
  `[channels.discord] bot_token` set in its `agent.toml`, the token
  resolver fell straight to the **global** `config.toml [channels]
  discord_bot_token_enc`. If that global token belongs to a different
  bot from the one that opened the notify target — and Discord threads
  are bot-scoped so only the opening bot can post into them — every
  delivery attempt returned `401 Unauthorized` even though the agent
  LLM call had already succeeded. User-visible symptom: cron
  `last_status = success` but nothing arrives in the Discord thread.

  **Fix**: new `resolve_agent_channel_token_via_reports_to` in
  [`config_crypto.rs`](crates/duduclaw-gateway/src/config_crypto.rs)
  walks the `reports_to` chain and returns the first ancestor's token.
  Cycle-safe (tracks visited ids) and bounded (`MAX_REPORTS_TO_HOPS =
  8`). Wired into both:

  1. [`cron_scheduler::resolve_channel_token`](crates/duduclaw-gateway/src/cron_scheduler.rs) — the cron
     `deliver_cron_result` path.
  2. [`dispatcher::resolve_forward_token`](crates/duduclaw-gateway/src/dispatcher.rs) — the
     `forward_delegation_response` path that relays sub-agent replies
     back to the originating channel.

  After this change, a cron-fired `xianwen-pm` with no Discord bot of
  its own inherits `xianwen-tl`'s token, or `agnes`'s if the TL also
  has none configured — matching the `reports_to` hierarchy the user
  already declared.

### Changed
- `resolve_forward_token` now does the `reports_to` cascade on **both**
  `callback_agent_id` AND `origin_agent` (the thread opener). The
  v1.8.20 behaviour of falling back to `origin_agent`'s direct token
  is preserved as step 3 in the cascade; steps 1-2 add the new walk so
  agents deeper in the hierarchy are covered without needing every TL
  / PM / researcher to have the same bot token pasted into their
  `agent.toml`.

- The stale single-purpose `get_agent_channel_token` helper in
  `dispatcher.rs` is removed — superseded by the shared cascade helper
  in `config_crypto.rs`.

### Added
- 8 new unit tests in `config_crypto::tests` covering the cascade:
  own-token wins, parent-token cascade, `None` when chain is empty,
  nearest-ancestor-not-farthest preference, cycle detection, missing
  agent.toml, `reports_to = ""` treated as root, and per-channel
  independence.


## [1.8.27] - 2026-04-23

### Added
- **Multica-inspired Agent integration layer** — agents are now
  first-class teammates on the task board, not just tools. Ships three
  coupled pieces:

  1. **12 new MCP tools** (`crates/duduclaw-cli/src/mcp.rs`) —
     `tasks_list`, `tasks_create`, `tasks_update`, `tasks_claim`,
     `tasks_complete`, `tasks_block`, `activity_post`, `activity_list`,
     `autopilot_list`, `shared_skill_list`, `shared_skill_share`,
     `shared_skill_adopt`. All mutating tools enforce
     `is_valid_agent_id` on the caller, and `tasks_list` defaults to
     the calling agent so noise stays low.
  2. **Pending task queue injection into the agent system prompt**
     (`crates/duduclaw-gateway/src/claude_runner.rs`) — every call to
     `call_claude_for_agent*` renders the top-5 open tasks (priority-
     ordered, `in_progress` → `todo` → `blocked`) into a
     `## Your Task Queue` block. Uses a shared `Arc<TaskStore>` via
     `OnceLock` so system-prompt composition doesn't open a fresh
     SQLite connection per turn. On the Direct API path the block is
     passed as an uncached second system block via
     `direct_api::call_direct_api_with_dynamic`, so the static 5–20k
     token prefix stays cacheable.
  3. **Autopilot trigger engine** (`autopilot_engine.rs`, new) —
     `tokio::broadcast::Sender<AutopilotEvent>` (capacity 8192) fed by
     both WebSocket handlers (in-process) and a SQLite event bus
     (out-of-process, see below). Typed variants: `TaskCreated`,
     `TaskUpdated`, `TaskStatusChanged`, `ActivityNew`, `ChannelMessage`,
     `AgentIdle`, `CronTick`. Condition DSL supports nested `all`/`any`
     + `eq`/`neq`/`in`/`not_in`/`gt`/`gte`/`lt`/`lte`/`contains`. Three
     action executors: `delegate` (MessageQueue enqueue), `notify`
     (Telegram/LINE/Discord/Slack via shared `reqwest::Client` from
     `OnceLock`), `run_skill` (reads the agent's `SKILLS/<name>.md`
     and delegates it as a prompt).

- **SQLite event bus** (`events_store.rs`, new) — `events.db` replaces
  the legacy `events.jsonl` file bus. WAL mode + `busy_timeout=5000` +
  monotonic auto-increment `id` give the tail reader a simple
  `WHERE id > ?` watermark; 7-day retention prune runs every 6 hours.
  Eliminates the file-bus hazard matrix in one swap (rotation race,
  partial-line reads, 0644 permissions, unbounded growth).

- **Dashboard Task Board preview widget** (`DashboardPage.tsx`) —
  `TasksPreviewCard` renders a mini 4-column Kanban with per-column
  task counts and links to `/tasks`. Loading skeleton, error banner,
  and empty-state tri-state so users can distinguish "never loaded"
  from "loaded empty".

- **Autopilot rule dashboard schema validation** (`handlers.rs`) —
  `autopilot.create` / `autopilot.update` reject unknown
  `trigger_event` values and `action` JSON missing required fields
  per type, so malformed rules fail immediately on the dashboard
  instead of silently during the first fire.

- **i18n keys** `tasks.preview.{title,viewAll,empty}` synced across
  `zh-TW`, `en`, `ja-JP`.

- **47 new unit tests** — 18 in `mcp::task_board_tests`, 18 in
  `autopilot_engine::tests` (including Closed/Open/HalfOpen state
  transitions), 7 in `handlers::autopilot_validation_tests`, 4 in
  `events_store::tests`. Full gateway lib suite: 611 tests passing.

### Changed
- **Task Board always renders four columns** (`TaskBoardPage.tsx`) —
  v1.4.29 hid the entire board behind an `tasks.length === 0`
  early-return, breaking the Kanban design intent that empty columns
  themselves *are* the affordance. Grid is now
  `grid-cols-1 md:grid-cols-2 lg:grid-cols-4` with each column keeping
  its own drop-hint placeholder.

- **Agent-facing MCP caller validation** is now consistent across
  `tasks_create` / `tasks_claim` / `tasks_complete` / `tasks_block` /
  `activity_post`. Wildcard (`*`) and path-traversal-like values are
  rejected at the boundary with a clear error message.

- **Autopilot circuit breaker is now a proper 3-state FSM** (Closed /
  Open / HalfOpen). 10 fires in 60s trip to Open (60s cooldown),
  HalfOpen allows one probe; retry within 30s re-trips, quiet window
  returns to Closed. All transitions are logged to `autopilot_history`
  and the Activity Feed so operators can see rule loops get contained
  and recover. Replaces the v1.8.27-dev sliding-window rate limiter.

- **Autopilot broadcast channel** capacity raised from 1024 → 8192 and
  the `RecvError::Lagged` branch escalated from `warn!` → `error!`
  with a detached `append_activity` task (so logging the lag no longer
  amplifies event drops).

### Fixed
- **Autopilot rule storage silently accepted malformed JSON**, so
  broken rules would only surface their error when first fired (and
  only in `autopilot_history`, invisible during rule authoring). Now
  rejected at write time.

- **`action_run_skill` had no path guard** — a crafted rule with
  `skill_name: "../../../etc/passwd"` could have escaped the
  SKILLS directory. Defense in depth: alphanumeric allowlist on both
  `target_agent` and `skill_name`, plus `canonicalize()` containment
  check against `<home>/agents/<agent>/SKILLS/`.

- **`events.jsonl` rotation race lost in-flight events** — writers
  holding an `O_APPEND` fd at the moment of `rename()` would land
  writes on the orphaned `.jsonl.1`, which the tail task ignored.
  Made moot by the SQLite event bus swap.

- **`build_pending_tasks_section` silently returned `None` when
  TaskStore open failed**, hiding a broken task board from operators.
  Now logs a warning at `warn!` level while still degrading gracefully
  (the agent just loses its task queue for that turn).

### Security
- **`events.db` is owned exclusively by the gateway/MCP process
  writing it** — SQLite handles file permissions (`0600` under default
  umask). Event payloads containing task descriptions / metadata are
  no longer world-readable on multi-user systems.


## [1.8.26] - 2026-04-22

### Added
- **`shared_wiki_lint` MCP tool** — audits `~/.duduclaw/shared/wiki/`
  for Karpathy LLM Wiki schema compliance. Reports: pages missing
  any of the six required frontmatter fields (`title`, `created`,
  `updated`, `tags`, `layer`, `trust`), pages containing fallback-
  content markers (e.g. "基於訓練資料", "web_search failed",
  "無法取得", "查無結果", "based on training data" …) that were not
  explicitly tagged `fallback-mode`, plus the existing graph-level
  checks (orphans / broken links / stale pages) delegated to
  `WikiStore::lint()`. Unlike per-agent `wiki_lint`, this tool
  takes no `agent_id` — shared wiki is a single global namespace.

### Fixed
- **Shared wiki accepted pages authored from stale LLM priors,
  polluting the cross-agent knowledge base.** When
  `ai-papers-researcher` / `ai-repos-researcher` cron tasks ran
  while `web_search` was failing, they silently fell back to
  recalling training data and wrote reports whose frontmatter
  looked legitimate but whose body was unanchored to any verifiable
  source (7/7 Hugging Face model URLs returned HTTP 200 + `<title>
  404` body in one case). These entered `shared/wiki/` unchallenged
  and drifted there indefinitely. Project rule: 「有 fallback 的資
  料不應該混入共用 wiki 中產生雜訊」.

  **Fix A** — `handle_shared_wiki_write` now enforces two gates
  before the write:

  1. **Frontmatter schema gate** (`validate_wiki_frontmatter`):
     page must open with a `---…---` block declaring *all* of
     `title, created, updated, tags, layer, trust`. `trust` must
     parse as a float in `[0.0, 1.0]`. Missing or malformed
     frontmatter → hard reject with a message pointing at the
     missing fields.
  2. **Fallback-content gate** (`detect_fallback_content`): body
     scanned for any of 14 CJK / English fallback markers. On
     match, reject unless the page explicitly opts in with
     `fallback-mode` in its `tags` (for post-mortem archives
     where a human deliberately wants the record preserved; those
     pages are still expected to carry `trust: 0.2` or lower).

  Per-agent `wiki_write` is intentionally left permissive — private
  wikis can hold speculative or fallback material; only the shared
  bus is strict.

- **Four research-pipeline cron prompts pushed fabricated content
  into `shared/wiki/` when search tools failed.**
  `ai-papers-morning`, `ai-papers-evening`, `ai-repos-morning`, and
  `ai-repos-evening` (rows in `~/.duduclaw/cron_tasks.db`) have been
  rewritten to:

  - **Abort on search failure** instead of falling through to
    training-data recall. The new prompts open with a hard
    precondition: if `web_search` returns 0 results, immediately
    notify `agnes` that "本日研究暫停：搜尋工具失效" and exit the
    task. Explicit ban on the 無法取得 / 基於訓練資料 / 查無結果
    narrative patterns (which now trip the shared-wiki fallback
    gate anyway).
  - **Two-layer URL verification** before any wiki write: a HEAD
    fetch must return HTTP 200 *and* the body must not contain
    `<title>404` (the Hugging Face gotcha where bad model URLs
    return 200 with a 404 page body). Items failing either check
    are dropped — the prompts are explicit that filling with
    unverified items is prohibited.
  - **Atomic-entity page layout per Karpathy LLM Wiki**: one
    entity page per paper/repo under `entities/YYYY-MM-DD-<slug>.
    md`, plus a daily digest under `research/ai-papers/YYYY-MM-DD-
    (08|20).md` whose `related:` points back to every entity.
    Frontmatter is spelled out explicitly inline (all six required
    fields, `layer: context`, `trust: 0.5` default, `sources:`
    list), and heading decoration emoji are banned.

  Backup of the pre-rewrite rows saved to
  `~/.duduclaw/cron_tasks.db.v1.8.25.bak` in case rollback is
  needed.

- **Two fabricated shared-wiki pages from 2026-04-22** were
  removed: `research/ai-repos/2026-04-22-08.md` (web_search
  fallback, 0 real URLs) and `research/ai-repos/2026-04-22-20.md`
  (7/7 HF model URLs were 404-in-body). `_index.md` cleaned and
  `_log.md` appended with `delete … by:operator (fabricated: …)`
  entries. Both surviving `research/ai-papers/*.md` pages were
  retrofitted with the full nine-field Karpathy frontmatter
  (`title`, `created`, `updated`, `author`, `tags`, `related`,
  `sources`, `layer: context`, `trust: 0.5`) so they pass the new
  `shared_wiki_lint` tool.

### Tests

**12 new** (all passing, all in `mcp::wiki_schema_tests`):

- `frontmatter_validator_accepts_full_schema`
- `frontmatter_validator_rejects_missing_frontmatter`
- `frontmatter_validator_rejects_missing_required_fields`
- `frontmatter_validator_rejects_out_of_range_trust`
- `frontmatter_validator_rejects_non_numeric_trust`
- `detect_fallback_catches_cjk_marker`
- `detect_fallback_catches_english_marker`
- `detect_fallback_ignores_clean_body`
- `shared_wiki_write_rejects_fallback_content`
- `shared_wiki_write_rejects_missing_frontmatter`
- `shared_wiki_write_allows_fallback_mode_opt_in`
- `shared_wiki_write_accepts_clean_karpathy_page`

Full workspace lib suite still green.


## [1.8.25] - 2026-04-22

### Fixed
- **Cron tasks scheduled `0 8 * * *` expecting 8 am local fired 8 am
  UTC instead**. Creating a task via MCP `schedule_task` without
  specifying `cron_timezone` fell through to UTC evaluation — so a
  Taipei user got their "morning" cron at 16:00 local and their
  "evening" cron at 04:00 the *next morning*. New
  `detect_local_timezone()` helper reads the host's IANA name
  (`iana_time_zone::get_timezone()` on Unix / Windows) and
  round-trips it through `duduclaw_core::parse_timezone` to guarantee
  `chrono-tz` acceptance. `handle_schedule_task` now auto-populates
  `cron_timezone` from the detected TZ when absent; explicit
  `cron_timezone='UTC'` still forces UTC (opt-out), any explicit IANA
  name still wins. Logs the detected zone at info level for
  observability. `cron_timezone` tool schema description updated to
  reflect the new auto-detect default. New direct dep
  `iana-time-zone = "0.1"` on `duduclaw-cli` (already a transitive
  dep of `chrono`, no new vendored C). New test
  `detect_local_timezone_returns_valid_iana_name` asserts
  parse_timezone round-trip and tolerates None on hosts with no
  discoverable TZ (minimal Docker images).
- **Cron agents' nested `send_to_agent` replies silently dropped
  (same class as v1.8.16 but for cron-initiated chains)**. The cron
  scheduler dispatched tasks via `call_claude_for_agent_with_type`
  wrapped only in `DELEGATION_ENV.scope` — never in
  `REPLY_CHANNEL.scope`. So when a daily-report agent called
  `send_to_agent("agnes", "here's my report")`, no
  `delegation_callbacks` row was ever registered (MCP's
  `send_to_agent` only inserts callbacks when
  `DUDUCLAW_REPLY_CHANNEL` env is set). Agnes's response landed in
  `message_queue.response` and was then dropped at
  `forward_delegation_response`'s no-callback silent-return branch.
  Fix: `run_task` now wraps the dispatch future in
  `REPLY_CHANNEL.scope(cron_reply_channel_string(task), …)` when
  the task has a `notify_channel` target. New helper
  `cron_reply_channel_string` builds the
  `<channel_type>:<chat_id>[:<thread_id>]` grammar that
  `mcp.rs::send_to_agent` parses; Discord threads stored as
  `chat_id=<thread_id>, thread_id=NULL` emit `discord:<thread_id>`
  (matching `deliver_cron_result`'s existing API-level "thread is
  a channel" semantics). Effect: nested cron delegations now
  register callbacks → forward through v1.8.20 token cascade →
  session-append via v1.8.24 chain-root cascade. The cron agent's
  own top-level response still goes through `deliver_cron_result`
  (direct POST) unchanged; this patch strictly closes the nested
  path. 5 new tests in `cron_scheduler::tests` covering None /
  Discord thread-as-chat-id / Discord parent+thread / Telegram
  without thread / Telegram forum topic thread.



## [1.8.24] - 2026-04-22

### Fixed
- **Sub-agent replies disappeared from the root agent's session on
  nested delegations (chain-root session-append gap)**. v1.8.17 Fix 2
  wrote an XML-delimited `<subagent_reply agent="X">` turn into the
  parent agent's session, but only when the session owner matched
  `callback.agent_id` — a deliberate cross-agent-bleed guard. The
  unintended side effect: sub-agents spawned by the dispatcher (TL,
  eng-agent, eng-infra, marketing, …) don't have their own sessions in
  `sessions.db` — only agnes does. So when eng-agent replied to TL's
  `send_to_agent` call, the owner-mismatch skip fired:
  `callback.agent_id=duduclaw-tl` vs `session owner=agnes` → warn +
  silent drop → agnes's next turn had no record of the engineer's
  output → root agent couldn't synthesise the chain's total work.
  Fix: same cascade pattern as v1.8.20 token resolution.
  `append_subagent_reply_to_parent_session` now takes
  `chain_root_agent: Option<&str>` and accepts an owner match at
  either tier. Tier 1 (parent direct) uses the existing
  `<subagent_reply agent="X">` grammar. Tier 2 (chain root)
  writes `<subagent_reply agent="X" via="Y">` where Y is the
  callback agent — the `via=` attribute lets the root LLM tell a
  direct reply apart from one relayed via a sub-agent. Tier 3
  (neither match) still skips, so the cross-agent-bleed guard
  holds. `forward_delegation_response` already computed the
  chain root for v1.8.20's token cascade; just wires it down.
  `safe_agent_tag` helper factored out so direct and relayed
  content share the same `[A-Za-z0-9_-]` sanitisation. 4 new
  regression tests in `dispatcher::tests`
  (`append_cascades_to_chain_root_when_parent_has_no_session`,
  `cascade_appends_via_annotation`,
  `cascade_does_not_override_direct_parent_match`,
  `cascade_skipped_when_neither_parent_nor_root_owns_session`).
  Sub-agents still don't get their own persistent sessions —
  session-per-agent-per-chain remains a separate, larger design
  decision.



## [1.8.23] - 2026-04-22

### Added
- **Timezone-aware cron evaluation (#16 Level 2)**. Both the heartbeat
  scheduler and the per-task cron scheduler now honour a new
  `cron_timezone` field. Setting it to an IANA name
  (e.g. `"Asia/Taipei"`) lets the user write cron expressions in their
  wall clock and have the scheduler do the UTC conversion —
  `"0 9 * * *"` with `cron_timezone = "Asia/Taipei"` now actually fires
  at 09:00 Taipei every day. Empty / absent `cron_timezone` preserves
  the pre-v1.8.23 UTC behaviour, so nothing moves for existing
  deployments. The field lives on `HeartbeatConfig` (agent.toml
  `[heartbeat]`) and on `cron_tasks` DB rows (accepted by MCP
  `schedule_task` and dashboard `cron_add` / `cron_update`). A shared
  `duduclaw_core::should_fire_in_tz` makes both schedulers use
  identical evaluation semantics. Typos are caught at call time in the
  MCP tool and dashboard handlers (IANA validation via `chrono-tz`),
  so a bad zone name surfaces as an error instead of silently firing
  in UTC. If a bad name does reach the scheduler somehow, it logs a
  single warn line at load time and falls back to UTC — the cron
  keeps firing instead of going silent. DB migration is idempotent
  `ALTER TABLE`: reopening a v1.8.22 database adds the column with all
  existing rows inheriting `NULL` (= UTC). Documented in all 5
  `templates/*/agent.toml` and in the dashboard cron-input hint.
  18 new tests across `duduclaw-core` (8: Taipei, New York EDT, UTC
  fallback, invalid names, `*/5` tz-invariance, trimming), agent
  heartbeat (5: tz set / empty / invalid / disabled, next_fire UTC
  instant), and cron_store (5 including a `cron_timezone` roundtrip
  + `update_cron_timezone` clearing, and migration idempotency across
  reopen).


## [1.8.22] - 2026-04-21

### Fixed
- **Proactive check could not use the agent's MCP tools (#14)**.
  `heartbeat.rs`'s proactive spawn hard-coded
  `--print --no-input --system-prompt --max-turns 3` without
  `--mcp-config`. Two breakages stacked: Claude CLI ≥2.1 removed
  `--no-input` (so the spawn hard-errored on the current CLI), and
  the missing `--mcp-config` meant any PROACTIVE.md that said "query
  Notion for open tasks" silently no-opped — the sub-agent could not
  see the tool. Rewritten to mirror `spawn_claude_cli_with_env`:
  system prompt via `--system-prompt-file` (no `/proc/PID/cmdline`
  exposure), auto-attach `<agent_dir>/.mcp.json` with
  `--strict-mcp-config` when present, and `--max-turns` now reads
  from a new `ProactiveConfig.max_turns` field (default 8, clamped
  1–64) so checks that chain multiple tool calls have headroom.
- **Cron task results never reached the chat channel (#15)**.
  `cron_scheduler::execute_cron_task` only called `record_run` +
  hallucination audit; the response text lived in the DB only, and
  any prompt asking the agent to "send to Discord via send_message"
  silently failed because `call_claude_for_agent_with_type` does not
  attach MCP. Users were wrapping cron jobs in external shell scripts
  that called Discord/Notion APIs directly. Fix adds row-level
  routing: three new columns on `cron_tasks`
  (`notify_channel` / `notify_chat_id` / `notify_thread_id`, all
  `TEXT NULL`, idempotent `ALTER TABLE` migration that tolerates
  "duplicate column name" so reopening a v1.8.21 DB is safe). New
  `deliver_cron_result` resolves the bot token through the same
  cascade the dispatcher uses (per-agent `agent.toml [channels.<ch>]`
  encrypted or plaintext → global `config.toml [channels]`), clamps
  the response to 3500 chars (Discord's 2000-char cap is the tightest;
  CJK-safe codepoint count), prefixes with a task-name header, and
  calls the unified `ChannelSender`. Discord thread routing uses
  `notify_thread_id` as the effective chat_id. Delivery failures log
  but never flip `record_run` — the agent did its work, only the
  postage failed. `CronTaskRow::has_notify_target()` gates delivery
  so legacy rows without notify columns stay completely silent. MCP
  `schedule_task` and dashboard `cron_add` / `cron_update` both
  accept the three new optional params with symmetric validation
  ("both or neither" for channel + chat_id). Two new tests cover
  round-trip + `update_notify` clearing, and the reopen-the-DB
  migration idempotency contract.

### Documented
- **`[heartbeat] cron` is UTC — was not documented (#16 Level 1)**.
  `heartbeat.rs:251` and `cron_scheduler.rs:151` both call
  `chrono::Utc::now()`, and `ProactiveConfig.timezone` only affects
  `quiet_hours_*` — not the cron evaluation. Taipei (UTC+8) users
  writing `"0 9 * * *"` expecting 09:00 local actually got 17:00.
  Added comments to all 5 `templates/*/agent.toml` heartbeat blocks
  with the Asia/Taipei mapping (`"0 1 * * *"` → local 09:00),
  expanded the `HeartbeatConfig` doc-comment, clarified on
  `ProactiveConfig.timezone` that it is quiet-hours-only, added the
  same UTC caveat to the MCP `schedule_task` tool description and
  the dashboard `SettingsPage` cron-input hint. Timezone-aware cron
  evaluation (Level 2 — reading `cron_timezone` on the task row) is
  planned for a later release; this change is documentation-only so
  no behaviour change for existing crons.


## [1.8.21] - 2026-04-21

### Added
- **`duduclaw reforward <message_id> [--dry-run]`** — manual unstuck
  lever for completed delegations whose forward failed and got retry-
  queued. Before v1.8.20, nested sub-agent forwards to Discord threads
  hit 401 Unauthorized because token lookup didn't cascade to the
  chain-root agent; v1.8.20 fixes that going forward, but
  already-completed messages were stuck — the dispatcher only retries
  when a new `agent_response` arrives for the same message_id, which
  never happens for a message that's already `done`. The callback row
  ages out to 24h cleanup and the user loses the reply. New command:
  reads `message_queue.db` by id (requires `status='done'` and
  non-empty response), uses the existing `delegation_callbacks` row
  if present, synthesizes one from the stored `reply_channel` column
  if missing (`INSERT OR REPLACE` for idempotency across re-runs),
  then delegates to `forward_delegation_response` which uses the
  v1.8.20 token cascade and v1.8.17 Fix 2 session append. Reports
  `Sent` / `DryRun` / `Failed` with friendly output; exit 1 on error.
  New `pub async fn reforward_message` + `pub enum ReforwardOutcome`
  in `duduclaw_gateway::dispatcher` for library reuse. 9 new
  regression tests covering dry-run paths, error cases
  (pending / missing / empty response / no channel context), and the
  `parse_reply_channel` grammar incl. the `discord:thread:<id>`
  collapse rule. Production-verified: recovered message
  `78fbcfc8-735b-4053-9ee0-a03543fd904f` (a marketing report that had
  been stuck since 12:35 UTC) delivered to its Discord thread.



## [1.8.20] - 2026-04-21

### Fixed
- **Nested sub-agent forwards to Discord threads got 401
  Unauthorized when only the chain-root agent had a per-agent bot
  token**. Production-observed on v1.8.19 (message
  `78fbcfc8-735b-4053-9ee0-a03543fd904f`, TL→marketing depth=2 — the
  marketing agent finished the report, response text in DB, but the
  HTTP POST to Discord thread `1496095418805780591` returned 401).
  `forward_to_channel`'s token lookup cascaded from `callback.agent_id`
  (the `send_to_agent` caller, e.g. `duduclaw-tl` — no per-agent bot
  configured) straight to the global `config.toml` token, skipping
  the chain-root agent (agnes) who actually owned the bot that
  opened the thread. Discord threads are scoped to the bot that
  opened them (v1.8.14 already documented this), so the 401 loop
  was inevitable for any nested delegation whose immediate caller
  lacked its own bot. New `resolve_forward_token` helper cascades
  three tiers: (1) callback agent's own token → (2) chain-root
  agent's token (looked up from `message_queue.origin_agent` via
  new `lookup_origin_agent`) → (3) global config token. The four
  channel arms (telegram/line/discord/slack) in `forward_to_channel`
  all route through the helper so the cascade applies uniformly,
  though only Discord's thread-bot scoping actually triggered the
  production failure. Handles the `origin_agent == callback_agent`
  self-loop, missing `message_queue.db`, and NULL
  `origin_agent` column cleanly. 7 new regression tests in
  `dispatcher::tests`, including
  `resolve_token_cascades_to_chain_root_when_callback_agent_has_none`
  that replays the production scenario.



## [1.8.19] - 2026-04-21

### Fixed
- **`Failed to initialize inference engine: Backend unavailable:
  llama.cpp` WARN flood**. When an agent's `[model.local]` had
  `use_router = true` but the gateway binary was built without
  `--features metal`/`cuda`/`vulkan` (the default for the
  npm-distributed binary to avoid pulling libclang + cmake into the
  release build), every single request ran the local-offload path,
  hit `InferenceEngine::init`, got `BackendUnavailable`, warned, fell
  back to SDK, and repeated next request. Functionally harmless — the
  fallback always worked — but drowned real warnings and wasted
  ~100ms per request on a doomed init attempt. Added a process-
  lifetime `AtomicBool` negative cache next to the existing
  `INFERENCE_ENGINE` singleton in `claude_runner.rs`: on the first
  failed `init` (or first successful init that still reports no
  available backend), the flag latches to `true` and every subsequent
  `get_inference_engine` short-circuits to `None` silently. The WARN
  is now one-shot per gateway process, with an actionable hint on how
  to enable a backend (rebuild with `--features metal/cuda/vulkan`, or
  configure `[openai_compat]` in `inference.toml` for a remote
  backend). A gateway restart resets the cache — which is also when
  operators would have rebuilt the binary, so the trade-off aligns.



## [1.8.18] - 2026-04-21

### Fixed
- **Dual-rail dispatch race silently defeated v1.8.16's reply_channel
  propagation**. Production-observed on a live v1.8.17 chain (agnes →
  TL → [eng-agent + eng-infra]): TL's outgoing delegations to the two
  eng-agents had `reply_channel=NULL` in `message_queue.db` even
  though `DUDUCLAW_REPLY_CHANNEL` was scoped correctly in the
  dispatcher. Effect: when eng-agent replied, no callback was
  registered, the forward lookup silently skipped, and the engineer's
  output never reached TL's session. `DUDUCLAW_DELEGATION_DEPTH`
  still propagated correctly in the same chain — the "half-propagated"
  pattern (correct depth + NULL reply_channel) was the telltale.
  Root cause: `mcp.rs::send_to_agent` was dual-writing every
  delegation to both `bus_queue.jsonl` (legacy) and
  `message_queue.db` (SQLite, authoritative since v1.8.1).
  The gateway's dispatcher polled both every 5 seconds:
  `poll_and_dispatch` (legacy) `tokio::spawn`'s a per-message
  dispatch task, which drops task-local `REPLY_CHANNEL` at the
  spawn boundary; `poll_and_dispatch_sqlite` (v1.8.16) scopes
  `REPLY_CHANNEL` correctly. Whichever side reached
  `prepare_claude_cmd` first determined whether
  `DUDUCLAW_REPLY_CHANNEL` was set on the target's Claude CLI
  subprocess. `DELEGATION_ENV.scope` nested INSIDE `dispatch_to_agent`
  applies to both paths equally, explaining why depth propagated but
  reply_channel didn't. Fix: removed the `bus_queue.jsonl` write from
  `send_to_agent`. SQLite has been the authoritative rail since
  v1.8.1 — the jsonl write was dead weight kept around for migration
  safety and, by causing the race, actively defeating the v1.8.16
  fix. `queued` flag now derives from the SQLite INSERT rowcount
  (v1.8.16 schema-downgrade fallback preserved). `poll_and_dispatch`
  (legacy) is left untouched; it still handles `task_created`
  signals and orphan-response recovery, both of which use separate
  writers not affected by this change. New
  `mcp::tests::send_to_agent_never_writes_bus_queue_jsonl`
  regression guard. Two existing E2E tests
  (`e2e_send_to_agent_increments_depth`,
  `e2e_depth_zero_defaults_origin_to_caller`) migrated from reading
  `bus_queue.jsonl` to `message_queue.db`.



## [1.8.17] - 2026-04-21

### Fixed
- **MCP server used the global `default_agent` as caller identity,
  silently breaking supervisor-relation authorization for every
  sub-agent**. `mcp.rs::get_default_agent` read `config.toml [general]
  default_agent` (typically the top-level `agnes`) regardless of which
  agent's Claude CLI actually spawned the MCP subprocess. When
  `duduclaw-tl` called `send_to_agent("duduclaw-eng-agent", …)`, the
  supervisor check asked "is agnes the parent of duduclaw-eng-agent?",
  saw `reports_to=duduclaw-tl`, and rejected the call as a pattern
  violation — even though the delegation was correct. The TL agent's
  own Discord message diagnosed this accurately ("MCP Server 在驗證
  委派權限時，仍以發起 Session 的身份（agnes）作為呼叫者") and
  proposed `方案 B: 由我代替產出` as a workaround — improvising around
  the bug instead of the system enforcing the correct chain. New
  `duduclaw_core::ENV_AGENT_ID = "DUDUCLAW_AGENT_ID"`;
  `mcp.rs::get_default_agent` preference order is now env var → config
  `default_agent` → `"dudu"`. `duduclaw-agent::mcp_template::
  ensure_duduclaw_absolute_path` (called from `server.rs:344` on
  gateway startup) injects `{ "DUDUCLAW_AGENT_ID": "<agent-dir-name>" }`
  into each agent's `.mcp.json` `env` block — preserving other env
  vars, preserving other `mcpServers` entries (playwright,
  browserbase), handling legacy `duduclaw-pro` key, idempotent on
  repeated calls. Empty string falls through to config to avoid
  lockout on botched migrations. After this: `agnes → duduclaw-tl`
  still allowed, `duduclaw-tl → duduclaw-eng-agent` now allowed,
  `agnes → duduclaw-eng-agent` correctly rejected.
- **Sub-agent replies never reached the parent agent's session,
  breaking conversation continuity across delegations**.
  `forward_delegation_response` delivered a sub-agent's reply to the
  originating channel (Discord/Telegram/LINE/Slack) and stopped.
  Parent agents had no record in their SQLite session of what the
  sub-agent said, so the next user turn replying to the parent
  referenced content the parent couldn't see. Production-observed
  symptom (Discord 2026-04-21 07:24): TL replied with "方案 A/B/C",
  user said "@Agnes 方案A", Agnes's next invocation had no trace of
  A/B/C and asked the user to disambiguate between Fabric / Besu /
  PoA (from an earlier unrelated branch). Fix: after
  `forward_to_channel(...)` returns `Ok(())`,
  `forward_delegation_response` appends a single assistant-role turn
  to the parent's session with XML-delimited content
  `<subagent_reply agent="X">...</subagent_reply>` (same grammar as
  `channel_reply::format_history_as_prompt`). Agent name sanitised
  to `[A-Za-z0-9_-]`. Token count uses the CJK-aware estimator.
  `sessions.total_tokens` + `last_active` updated in the same
  transaction. New `candidate_session_ids` tries both
  `discord:thread:<id>` and `discord:<id>` forms (the `thread:`
  marker was collapsed in `mcp.rs::send_to_agent` callback insert)
  and matches by `owner_agent` to prevent cross-agent bleed on
  shared channels. Session store errors are swallowed at warn level —
  Discord delivery already succeeded, dropping the session append is
  strictly better than losing the forward. Append happens only on
  HTTP success, so retry loops don't double-append.



## [1.8.16] - 2026-04-21

### Fixed
- **Nested sub-agent replies silently dropped at delegation depth ≥ 2**.
  A user-visible chain like `agnes → duduclaw-tl → [eng-agent +
  eng-infra] → synthesis` would deliver the first-level "dispatch
  confirmation" (depth=1, from `channel_reply`), complete all three
  sub-agent messages in `message_queue.db` with status=`done`, but
  never forward the status update (depth=2) nor the 16 KB final
  synthesis (depth=3) to the originating Discord channel — no WARN,
  no error, just silence. Root cause: MCP `send_to_agent` only
  registers a `delegation_callbacks` row when `DUDUCLAW_REPLY_CHANNEL`
  is set in env, which `channel_reply::REPLY_CHANNEL.scope()` does for
  inbound channel messages but `dispatcher::dispatch_to_agent` did
  NOT, so nested sub-agent processes had no channel context, their
  callback rows were never inserted, and `forward_delegation_response`
  took its no-callback silent-return branch. Fix propagates channel
  context through the chain: (1) `message_queue` gains a
  `reply_channel TEXT` column with idempotent `PRAGMA table_info` +
  `ALTER TABLE ADD COLUMN` migration; (2) MCP `send_to_agent` captures
  `DUDUCLAW_REPLY_CHANNEL` from env on INSERT, with a schema-downgrade
  fallback for the cross-process race on first v1.8.16 boot; (3)
  `dispatcher::dispatch_to_agent` now wraps the dispatch future in
  `claude_runner::REPLY_CHANNEL.scope(msg.reply_channel, ...)` when
  the row carries channel context, so the spawned Claude CLI
  subprocess inherits the env var and its own nested `send_to_agent`
  calls register callbacks correctly. Chain propagation is automatic:
  depth-1's row stores discord:..., depth-2 inherits via env during
  dispatch and writes it back to its own row, depth-3 does the same.
- **`forward_delegation_response` no-callback path was fully silent**,
  making the above bug invisible in logs. Added
  `tracing::debug!` so future drops surface under
  `RUST_LOG=duduclaw_gateway::dispatcher=debug` with the message-id +
  responder agent. Still expected-and-benign for cron / reminder /
  non-channel delegations; unexpected for user-facing sub-agent
  replies.



## [1.8.15] - 2026-04-21

### Fixed
- **Discord global `[discord]` 401 noise at gateway startup**. The
  global `config.toml [channels] discord_bot_token_enc` was eagerly
  validated on startup via `GET /users/@me`, printing a warn-level
  "token invalid (HTTP 401)" even when per-agent Discord tokens (the
  authoritative source since v1.8.14) were live and serving traffic.
  Users who migrated to per-agent tokens saw a scary warning that
  implied Discord was broken when it wasn't. `start_discord_bots` now
  collects per-agent tokens first and passes a `quiet_on_auth_failure`
  flag to `spawn_discord_bot`; a 401/403 on the global token when at
  least one per-agent token exists is logged at info level with an
  explicit note. A 401 with no per-agent fallback still warns.
- **GVU proposals on tiny SOUL.md baselines were always rejected as
  CRITICAL drift**. With a ~400-char baseline (e.g. the default agnes
  template), every evolution `append` made `compute_asi`'s 0.40-
  weighted char-bigram content similarity collapse to ~0.06 and trip
  the 0.50 critical threshold deterministically. Not a drift problem —
  a baseline-size problem. Added
  `duduclaw_security::stability_index::AsiConfig::bootstrap()`
  (content 0.40 → 0.20, semantic 0.30 → 0.45, critical 0.50 → 0.25)
  and `AsiConfig::for_baseline_size(bytes)` which dispatches to
  bootstrap when `bytes < 1024`, default otherwise. The updater now
  calls `for_baseline_size(current_content.len())` so agents with
  richer SOUL.md files still face the strict default threshold.
- **Claude CLI `--resume` was permanently unreachable — wasting 1
  extra CLI spawn per multi-turn conversation**. v1.8.1 introduced
  native multi-turn via `--resume <dd-{hex16}>` with a SHA-256 session
  ID. Claude CLI strictly requires either a canonical UUID or an
  exact session title match — `dd-5d8a35f9dba3408e` is neither, so
  the first `--resume` attempt was rejected 100% of the time before
  the `is_session_error`-guarded fallback retried with history-in-
  prompt (the only path that actually worked). Every multi-turn
  reply paid one wasted CLI spawn + startup latency + warn-level log
  line. `call_claude_cli_rotated` no longer attempts `--resume`:
  when conversation history exists, it is folded into the prompt via
  `format_history_as_prompt` and Claude CLI is spawned once. The
  `session_id` parameter is kept as `_session_id` for call-site
  compatibility. Removed dead `make_claude_session_id` and
  `is_session_error` helpers plus their 3 tests.



## [1.8.14] - 2026-04-21

### Fixed
- **Discord thread session id drifted across turns**. `auto_thread &&
  !is_thread` in the session-id formatter was only true on the first
  turn (when a thread was about to be created) — every follow-up turn
  the user typed inside the thread flipped `is_thread` to true and the
  session id silently switched from `discord:thread:{id}` to
  `discord:{id}`, loading a fresh empty session and losing all context.
  Condition is now `is_thread || created_thread` so a thread-scoped
  conversation keeps one session id for its entire lifetime. Also
  handles the edge case where `create_thread()` fails (returns
  `discord:{channel_id}` instead of a misleading `discord:thread:...`).
- **Sub-agent replies stuck in bus_queue.jsonl**. Three layered bugs
  prevented `send_to_agent` → sub-agent → user round-trips from ever
  completing:
  1. The `delegation_callbacks` parser split `<channel>:thread:<id>`
     by `:` and stored the literal string "thread" as `channel_id`;
     downstream `validate_channel_id` rejected it as non-numeric, so
     forwarding retry-looped forever. Parser now recognises the
     `<type>:thread:<id>` marker and stores `channel_id=<id>,
     thread_id=None`.
  2. `forward_to_channel` only ran immediately after a live dispatch;
     orphan `agent_response` entries left on disk after a crash /
     Ctrl+C / hotswap were never replayed. New
     `reconcile_orphan_responses` scans `bus_queue.jsonl` on
     dispatcher startup and atomically replays every callback whose
     row is still pending.
  3. Discord / Telegram / LINE / Slack arms read the global
     `[channels] <type>_bot_token` from config.toml. Discord threads
     are scoped to the bot that opened them — a different bot returns
     401 Unauthorized even in the same guild. New
     `get_agent_channel_token` reads the originating agent's per-agent
     token from `agents/<id>/agent.toml [channels.<type>] bot_token_enc`
     first, falling back to the global token only when the agent has
     none.
- **Long sub-agent replies silently truncated**. `forward_to_channel`
  capped responses at the channel byte limit and appended
  `_(回應過長，已截斷)_`, dropping most TL/PM report content. Rewritten
  to use the existing `channel_format::split_text` (paragraph/line
  aligned, UTF-8 safe) emitting chunks labelled
  `📨 **agent** 的回報 (1/N)` / `(續 2/N)`, each sized under the
  channel's byte budget (Discord 1900, Telegram 4000, LINE 4900, Slack
  3900) with a 250ms inter-chunk gap to stay within API rate limits.

### Changed
- **Default log level is now `warn`** when `RUST_LOG` is unset.
  Previous default (`EnvFilter::from_default_env()` with no fallback)
  dropped every log unless the user explicitly set `RUST_LOG`, which
  made issues like "401 on delegation forward" undiagnosable from the
  terminal and left `~/.duduclaw/logs/gateway.log` at 0 bytes. `warn`
  keeps the terminal quiet for end users while still surfacing real
  problems; run `RUST_LOG=info duduclaw run` for the verbose
  dispatcher / WebSocket / heartbeat trace when debugging.



## [1.8.13] - 2026-04-20

### Added
- **Memory page Key Insights tab**. The agent-local `memory.db` →
  `memories` table is populated by the prediction engine with
  satisfaction-error deltas ("Prediction deviation: expected 0.70,
  inferred 0.42 ..."), not conversational content — so the previous
  Memory tab looked empty / unhelpful on a running system. The real
  extracted insights live in the `key_facts` table (P2 Key-Fact
  Accumulator), which had zero dashboard exposure. New RPC
  `memory.key_facts(agent_id, limit)` queries that table directly
  and the Memory page now has a 4th tab "關鍵洞察 / Key Insights /
  主要インサイト" rendering each fact as a card with `access_count`
  badge, timestamp, and collapsible source metadata.
- **Unified multi-source audit log on the Logs page**. Previously
  `security.audit_log` read only `security_audit.jsonl` (rarely
  written), so the history panel showed "暫無審計事件" on systems
  with dozens of real tool calls. New RPC `audit.unified_log(params)`
  merges four JSONL sources (`security_audit.jsonl`,
  `tool_calls.jsonl`, `channel_failures.jsonl`, `feedback.jsonl`)
  into a common envelope — `timestamp` / `source` / `event_type` /
  `agent_id` / `severity` / `summary` / `details` — sorted
  newest-first, with per-source counts returned alongside. Severity
  rules: tool_call success=info, failure=warning,
  channel_failure=warning, feedback=info, security preserves its
  original severity. Missing files and malformed JSONL lines are
  tolerated silently. Summary truncation goes through
  `duduclaw_core::truncate_bytes` (CJK-safe).
- **Logs page history tab rewrite**. Source filter chips
  (全部 / 安全 / 工具呼叫 / 通道失敗 / 回饋) with live per-source
  counts, severity dropdown, severity-colored left borders
  (emerald / amber / rose), click-to-expand pretty-printed detail
  JSON. Realtime tab untouched. `handle_security_audit_log` is
  preserved intact for backward compatibility.



## [1.8.12] - 2026-04-20

### Fixed
- **Opaque `claude CLI stream error: Unknown stream-json error`** now
  carries the captured Claude CLI stderr tail (`| stderr: ...`, 500
  bytes max). When Claude CLI emits `is_error: true` on a `result`
  event with no `result` string, the caller previously got no
  actionable detail; now the real reason (stale `--resume` handle,
  internal CLI error, etc.) is surfaced in both the debug log and
  the rotator's error history.
- **Auto-fallback on generic `--resume` failures**. `is_session_error`
  now also matches "unknown stream-json error", so when Claude CLI
  can't spell out why `--resume` failed the caller retries once with
  the session history folded into the prompt. Worst case one extra
  turn of cost; best case the user gets a reply instead of an opaque
  error.
- **`schedule_task` MCP tool schema was missing `agent_id` and `name`**.
  The handler reads both (plus `task` / `prompt` / `description` as
  synonyms) but the declared `ParamDef` list exposed only `cron` and
  `description`. From the agent's point of view the tool looked half-
  built, so Agnes fell back to Claude Code's session-bound
  `/schedule` slash command (7-day auto-expiry) instead of DuDuClaw's
  persistent `CronScheduler`. Schema now lists `cron`, `task`, `name`
  (all required), and `agent_id` (optional, strongly recommended),
  and the description explicitly states the tool is persistent
  (`~/.duduclaw/cron_tasks.db`), survives restarts, and should be
  preferred over `/schedule`.



## [1.8.11] - 2026-04-20

### Fixed
- **Claude CLI `--bare` broke OAuth authentication** (Claude CLI
  2.1.110 regression). The flag was added to
  `spawn_claude_cli_with_env` for ~15-25% latency reduction by
  skipping hooks / LSP / plugin sync / CLAUDE.md auto-discovery, but
  also disabled OS-keychain credential lookup, causing every channel
  subprocess call to fail with "Not logged in · Please run /login"
  even when `claude auth status` confirmed a valid session. Removed
  from both `call_claude_cli_rotated` and `call_claude_cli_lightweight`
  paths.
- **CJK / emoji byte-index string slicing panicked tokio workers**.
  `s[..s.len().min(N)]` slices by byte, not by char, so any multi-byte
  codepoint straddling byte N (e.g. `學` = 3 bytes) triggered "byte
  index N is not a char boundary" panics that crashed reply dispatch
  silently. The pattern was copy-pasted across 31 sites in 16 files
  (Feishu, WhatsApp, LINE, Slack, Telegram, Discord, TTS, direct_api,
  handlers, dispatcher, tool_classifier, gvu/loop_, cli/mcp,
  cli/acp/handlers, runtime/openai_compat, computer_use, webchat,
  channel_reply).

### Added
- **`duduclaw_core::truncate_bytes` / `truncate_chars`** (new
  `duduclaw-core/src/text_utils.rs` module). `truncate_bytes` returns
  a `&str` sliced at the nearest UTF-8 char boundary ≤ the requested
  byte budget — a panic-safe drop-in for `&s[..N]`. `truncate_chars`
  counts codepoints. Six unit tests cover ASCII, mid-CJK, zero-budget,
  and emoji (4-byte) cases. Every unsafe byte-index slice on a
  user-text / LLM-text / HTTP-body string was migrated.



## [1.8.10] - 2026-04-20

### Added
- **`marketplace.list` RPC** serving the real built-in MCP catalog
  (Playwright, Browserbase, Filesystem, GitHub, Slack, Postgres,
  SQLite, Memory, Fetch, Brave Search) enriched with `author`,
  `tags`, and `featured` fields. Merges optional user entries from
  `~/.duduclaw/marketplace.json` without a rebuild.
- **Partner data model**: new SQLite-backed `PartnerStore`
  (`~/.duduclaw/partner.db`) with profile + customer CRUD and
  computed sales stats. Seven RPCs (`partner.profile`, `partner.stats`,
  `partner.customers`, `partner.profile.update`,
  `partner.customer.add`, `partner.customer.update`,
  `partner.customer.delete`) and 4 unit tests.
- **Toast notification system** (`web/src/components/Toast.tsx` +
  `web/src/lib/toast.ts`): module-scoped event bus, max-5 queue,
  auto-dismiss, warm stone/amber/emerald/rose variants,
  `prefers-reduced-motion` honored.
- **`cron.resume`** wired to a Resume button alongside Pause in the
  Settings cron task list.
- **SOUL.md evolution history UI** in Memory → Evolution tab with
  pre/post metric deltas (positive feedback, prediction error, user
  corrections) and status badges (Confirmed / RolledBack / Observing).

### Changed
- **`evolution.status`** returns real aggregate data
  (`enabled`/`mode`/`total_agents`/`gvu_enabled_count`/
  `total_versions`/`last_applied_at`) instead of hardcoded
  `{enabled: true, mode: "prediction_driven"}`.
- **`activity.subscribe`** returns honest metadata
  (`broadcast_mode: "all_events"` + note) — previously a bare stub.
  Per-topic filtering is not implemented; all authenticated WS
  clients receive all activity events.
- **ChannelsPage setup guides**: 42 hardcoded zh-TW strings extracted
  to i18n across Telegram / LINE / Discord / Slack / WhatsApp /
  Feishu in zh-TW / en / ja-JP.
- **MarketplacePage** loads from the real RPC; fake stars/prices and
  the 8-item `MOCK_SERVERS` constant removed. Category-based icon
  mapping (browser / data / communication).
- **PartnerPortalPage** rewired to real RPCs; mock constants
  (`PARTNER_STATUS`, `SALES_STATS`, `MOCK_CUSTOMERS`) and the
  preview banner removed. Added onboarding card (empty-profile
  state) and Add Customer modal.
- Inline error feedback added to MarketplacePage install,
  PartnerPortalPage license generation, and ApprovalModal WS
  response failures (previously all silently swallowed).

### Removed
- **`activity.unsubscribe`** RPC (backend dispatch arm and frontend
  method) — broadcasts cannot be stopped without closing the WS
  itself, so the RPC was dead.
- **`evolution.skills`** handler — fully redundant with
  `skills.list`, which returns richer per-agent + global structure.

### Fixed
- 23 silent `console.warn("[api]", e)` catches across DashboardPage,
  ReportPage, BillingPage, SkillMarketPage, SettingsPage, MemoryPage,
  AgentsPage, ChannelsPage, and KnowledgeHubPage now surface errors
  to users via toast while preserving devtools visibility.



## [1.8.9] - 2026-04-20

### Added
- **Wiki knowledge layer system** (Vault-for-LLM inspired): 4-layer
  architecture (L0 Identity / L1 Core / L2 Context / L3 Deep) with
  `layer` and `trust` (0.0-1.0) frontmatter fields. Search results
  ranked by trust-weighted score. Backward-compatible defaults for
  existing pages.
- **Wiki system prompt injection**: `build_system_prompt()` now
  auto-injects L0+L1 wiki pages into the WIKI_CONTEXT module.
  Agents automatically reference their accumulated knowledge without
  manual `wiki_search` calls.
- **FTS5 full-text index**: `WikiFts` SQLite-backed index with
  `unicode61` tokenizer for CJK support. Auto-syncs on every
  `write_page` / `delete_page`. Manual rebuild via `wiki_rebuild_fts`
  MCP tool.
- **Wiki dedup detection**: `wiki_dedup` MCP tool detects duplicate
  pages by title match and tag Jaccard similarity (>= 0.8).
- **Wiki knowledge graph**: `wiki_graph` MCP tool exports Mermaid
  diagrams with BFS-limited center+depth focused view. Node shapes
  vary by knowledge layer.
- **Wiki search filters**: `wiki_search` / `shared_wiki_search` now
  support `min_trust`, `layer`, and `expand` (1-hop related/backlink
  expansion) parameters.
- **Reverse backlink index**: `build_backlink_index()` scans
  `related` frontmatter + body markdown links for bidirectional
  mapping.
- **Layer-aware context injection**: `build_injection_context()` +
  `collect_by_layer()` for system prompt budget-aware injection.
- **CLAUDE_WIKI.md template**: Now included in agent CLAUDE.md on
  creation, providing wiki MCP tool usage guide to Claude Code.
- **A2A stdio JSON-RPC server** (`acp::server::run_acp_server`):
  `duduclaw acp-server` is now functional (previously a stub). Runs a
  line-delimited JSON-RPC 2.0 loop on stdin/stdout with
  `agent/discover`, `tasks/send`, `tasks/get`, `tasks/cancel`
  methods, backed by the `A2ATaskManager`. Enables Zed / JetBrains /
  Neovim IDE integration via the Agent Client Protocol.
- **Behavioral contract injection**: `AgentRegistry` now loads
  `CONTRACT.toml` into `LoadedAgent.contract`. `must_not` /
  `must_always` rules are rendered as a CONTRACT module in the
  system prompt, giving every runtime (Claude / Codex / Gemini)
  consistent behavioral boundaries.
- **Memory decay daily scheduler**: Gateway spawns a background
  task that runs `duduclaw_memory::decay::run_decay` every 24h,
  archiving low-importance entries older than 30 days and
  permanently deleting archived entries older than 90 days.
- **Dashboard WebSocket heartbeat**: Server sends a WebSocket
  `Ping` every 30s and closes idle sockets after 60s without a
  `Pong`. Client sends an application-level `ping` RPC every 25s
  (browsers can't issue control frames). New `ping` method on the
  gateway method handler returns `{pong:true}`.
- **`/metrics` Prometheus endpoint**: New `duduclaw_gateway::metrics`
  module exposed as `GET /metrics` on the gateway HTTP server for
  scraping runtime metrics.
- **RL trajectory collector + CLI**: New
  `duduclaw_gateway::rl::collector` module writes per-agent
  trajectories to `~/.duduclaw/rl_trajectories.jsonl` during
  channel interactions. `duduclaw rl export|stats|reward` is now
  functional (previously stub), including composite reward
  computation (outcome × 0.7 + efficiency × 0.2 + overlong × 0.1).
- **Cognitive memory MCP tools**: `memory_search_by_layer`
  (episodic/semantic filter), `memory_successful_conversations`
  (high-importance episodic recall by topic),
  `memory_episodic_pressure` (observation-density score for
  scheduling Meso reflections), `memory_consolidation_status`
  (count of un-consolidated high-importance episodes).
- **Streaming ASR providers**: `AsrRouter` now accepts
  `Box<dyn StreamingAsrProvider>` (e.g. Deepgram WebSocket) via
  `add_streaming_provider` / `streaming_provider()` for real-time
  transcription alongside existing batch providers.
- **Compression strategy selector**: `compress_text` MCP tool gains
  a `strategy` param — `meta_token` (lossless), `llmlingua` (lossy
  2-5×), `streaming_llm` (window management), or `auto`.
- **Marketplace + Partner Portal dashboard pages**: Wired into
  router and sidebar (manager+ gate for Partner Portal). New
  Browser Automation tab under Settings with ToolApproval,
  SessionReplay, and BrowserAudit panels. `ApprovalModal` mounted
  at app root for synchronous tool approval prompts.

### Changed
- **Cloud ingest prompt**: Now instructs Claude to include `layer`
  and `trust` in extracted wiki page frontmatter.
- **Auto-ingest defaults**: Source pages default to `layer: context,
  trust: 0.4`; entity pages to `layer: deep, trust: 0.3`.
- **Backlink logging**: `write_page()` logs info-level suggestions
  when referenced pages lack reciprocal backlinks.
- **`wiki_search` / `shared_wiki_search` response**: Hits now
  include `weighted_score`, `trust`, and `layer` fields alongside
  the existing `score`.
- **`duduclaw-agent` crate**: Now depends on `duduclaw-memory` to
  build the WIKI_CONTEXT injection module at prompt assembly time.

### Fixed
- **Wiki-to-LLM disconnect (all runtimes)**: Wiki system previously
  accumulated knowledge via channel ingest and GVU evolution but
  never fed it back into LLM system prompts. Now L0+L1 pages are
  auto-injected into ALL three system prompt assembly paths:
  - CLI interactive (`runner.rs` — `WIKI_CONTEXT` module)
  - Channel reply (`channel_reply.rs` — `## Wiki Knowledge` section,
    serves Telegram/LINE/Discord → Claude/Codex/Gemini/OpenAI)
  - Dispatcher/Cron (`claude_runner.rs` — `# Wiki Knowledge` section,
    serves agent-to-agent delegation and scheduled tasks)
- **FTS desync**: FTS index was completely disconnected from write
  operations. Now auto-syncs on every page write/delete.
- **CLAUDE_WIKI template unused**: Template existed but was never
  included in agent CLAUDE.md files.
- **`duduclaw rl` / `duduclaw acp-server` stubs**: Both commands
  previously printed a placeholder and returned; they now execute
  the real collector / JSON-RPC server.


## [1.8.8] - 2026-04-20

### Fixed
- **Lightweight CLI effort level**: Changed from `--effort low` to
  `--effort medium` for instruction/fact extraction tasks. Prevents
  quality degradation in extracted pinned instructions and key facts
  while maintaining cost savings from other lightweight flags.



## [1.8.7] - 2026-04-19

### Added
- **Claude CLI lightweight path**: New `call_claude_cli_lightweight()` for
  single-turn metadata tasks (compression, instruction/fact extraction). Uses
  `--bare --effort low --max-turns 1 --no-session-persistence --tools ""`.
  Estimated 25-40% cost reduction for metadata tasks.

### Changed
- **Claude CLI `--bare` mode**: Main channel reply path now uses `--bare` to
  skip hooks/LSP/plugins/CLAUDE.md discovery (15-25% latency reduction).
- **Claude CLI `--exclude-dynamic-system-prompt-sections`**: Stabilizes system
  prompt across turns for better prompt cache hit rate (10-15% token reduction).
- **Claude CLI `--strict-mcp-config`**: Explicit MCP isolation per agent.
- **Gemini CLI system prompt**: Fixed from non-existent `--system-instruction`
  flag to `GEMINI_SYSTEM_MD` env var (temp file). Added `--approval-mode yolo`
  and conversation history prefix.
- **Codex CLI system prompt**: Fixed from non-existent `--instructions` flag
  to `AGENTS.md` file write. Added conversation history prefix.

### Fixed
- **Gemini runtime**: `--system-instruction` flag doesn't exist in Gemini CLI.
- **Codex runtime**: `--instructions` flag doesn't exist in Codex exec.



## [1.8.6] - 2026-04-19

### Added
- **Instruction Pinning** (P0): First user message → async Haiku extraction of
  core task instructions → stored in `sessions.pinned_instructions` → injected
  at system prompt tail (high-attention position). Survives session compression.
- **Snowball Recap** (P0): Each turn prepends `<task_recap>` with pinned
  instructions to user message. Zero LLM cost, utilizes U-shaped attention tail.
- **Clarification Accumulation**: When agent asks a question and user answers,
  the answer is appended to pinned instructions (capped at 1000 chars).
- **P2 Key-Fact Accumulator**: Lightweight cross-session memory replacing
  MemGPT Core Memory. Extracts 2-4 key facts per substantive turn via Haiku,
  stores in `key_facts` table with FTS5 search, injects top 3 relevant facts
  into system prompt. ~100-150 tokens vs MemGPT's 6,500 (87% reduction).



## [1.8.5] - 2026-04-19

### Fixed
- **MCP tools unavailable in channel reply**: Claude CLI in `-p
  --dangerously-skip-permissions` mode does NOT read global
  `~/.claude/settings.json` MCP servers — only project-level `.mcp.json`.
  Reverted v1.8.4's global migration back to per-agent `.mcp.json` with
  gateway startup auto-creation/fixup for all agents.



## [1.8.4] - 2026-04-19

### Changed
- **Global MCP server registration**: DuDuClaw MCP server (platform tools:
  `send_to_agent`, `list_cron_tasks`, `create_agent`, etc.) is now registered
  in `~/.claude/settings.json` (global) instead of per-agent `.mcp.json`.
  Gateway startup auto-migrates existing per-agent entries to global.
  Agent-specific MCP servers (Playwright, Browserbase) stay per-agent.
  This eliminates the class of bugs where agents lacked MCP tool access.



## [1.8.3] - 2026-04-19

### Fixed
- **Cron jobs invisible to MCP**: `list_cron_tasks` filtered by `default_agent`,
  hiding sub-agent cron tasks (duduclaw-pm, xianwen-pm, etc.). Dashboard showed
  them but agents couldn't see or manage them. Now returns all tasks by default.
- **Missing `.mcp.json` for agents**: Agnes pointed to non-existent `duduclaw-pro`
  binary; other agents had no `.mcp.json` at all, causing "沒有 MCP 通訊工具".
  Gateway startup now auto-creates/fixes `.mcp.json` for all agents.



## [1.8.2] - 2026-04-19

### Added
- **Sub-agent team roster injection**: System prompt now automatically includes
  a "Your Team" section listing sub-agents (by `reports_to` hierarchy), enabling
  natural delegation like "請團隊檢查" without requiring SOUL.md changes.
- **Release workflow_dispatch**: Release CI can now be manually re-triggered
  with `gh workflow run release.yml -f tag=vX.Y.Z` when tag-push CI fails.

### Fixed
- **Agent team awareness**: Agnes didn't recognize "duduclaw團隊" as her
  sub-agents because organizational context was missing from system prompt.



## [1.8.1] - 2026-04-19

### Added
- **Native multi-turn session management**: Claude CLI `--resume` with SHA-256
  deterministic session ID mapping. Fallback to XML-delimited history-in-prompt
  when session not found (e.g., account rotation).
- **Turn trimming**: Long conversation turns (>800 chars) are
  trimmed to head 300 + tail 200 chars with `[trimmed N chars]` placeholder.
  CJK-safe char-level slicing. Zero LLM cost.
- **Direct API prompt cache strategy**: "system_and_3" cache breakpoint placement
  for ~75% cache hit rate on multi-turn conversations.
- **Session compression summary injection**: Post-compression summaries (role=system)
  are now injected into system prompt instead of conversation turns.

### Removed
- **MemGPT 3-layer memory system** (-1,985 LOC): Core Memory, Recall Memory,
  Archival Bridge, Budget Manager, Consolidation Pipeline.
  The system prompt injection approach caused 6,500 tokens of bloat per prompt
  and "lost in the middle" attention degradation.
- **6 MCP tools**: `core_memory_get`, `core_memory_append`, `core_memory_replace`,
  `recall_search`, `archival_search`, `archival_insert`.
- 3 SQLite databases (`core_memory.db`, `recall_memory.db`) are no longer populated.

### Fixed
- **Session chain breakage**: Agnes losing context between consecutive messages
  ("幫我全部開啟" → "你指的是什麼？"). Root cause: stateless CLI subprocess
  per message with history in system prompt. Now uses native multi-turn.



## [1.7.2] - 2026-04-17

### Fixed
- **Stream-JSON empty result overwrite**: When Claude uses tools, the final `result`
  event often has an empty `result` field. The parser unconditionally overwrote
  accumulated assistant text with this empty string, causing false "Empty response"
  errors. Fixed in all 4 stream-json parsers (channel_reply, claude_runner, agent
  runner, gemini runtime).
- **Python SDK fallback OAuth awareness**: The Python SDK fallback now skips entirely
  for OAuth-only setups (it requires API keys) instead of producing the misleading
  "未設定任何 API 帳號" error. When an API key is available, it is explicitly
  passed to the subprocess.



## [1.6.0] - 2026-04-17

### Added
- **Git Worktree L0 isolation layer** (`worktree.rs`): lightweight per-task filesystem
  isolation via git worktrees. Cheaper than container sandbox — creates isolated working
  directories so concurrent agents don't step on each other's files.
  - `WorktreeManager`: full lifecycle management (create / remove / list / cleanup_stale)
  - **Atomic merge** with dry-run pre-check: merge → check → abort → real merge if clean.
    Protected by global `Mutex` to prevent concurrent merge corruption.
  - **Snap workflow** (inspired by agent-worktree): create → execute → inspect → merge/cleanup,
    with pure-function decision logic separated from I/O for testability.
  - **Friendly branch names**: `wt/{agent_id}/{adjective}-{noun}` from 50×50 word lists.
  - **copy_env_files**: copies `.env` etc. into worktree with path traversal jail,
    symlink rejection, and 1MB size limit.
  - **Structured exit codes**: `AgentExitCode` enum (Success/Error/Retry/KeepAlive).
  - **Resource limits**: max 5 worktrees per agent, 20 total.
- `ContainerConfig` extended with `worktree_enabled`, `worktree_auto_merge`,
  `worktree_cleanup_on_exit`, `worktree_copy_files` fields.
- Three-tier isolation routing in dispatcher: L0 Worktree → L1 Container → Direct.
- `WORKTREE_PATH` task-local in `claude_runner` for working directory override.

### Security (3-round deep review)
- Path traversal defense: canonical jail + absolute path rejection + `..` blocking.
- Agent ID sanitization: `sanitize_agent_id()` restricts to `[a-z0-9-]`.
- Branch name validation: `validate_wt_branch()` rejects `..`, leading `-`, non-`wt/` prefixes.
- Git command hardening: `--` separators on all `git merge` commands.
- `restore_head` validates branch names and commit hashes before `git checkout`.
- Symlink checks before `canonicalize()` to prevent TOCTOU bypass.
- Destination file removal before copy to prevent symlink race.
- Global merge lock via `OnceLock<Mutex<()>>` (not per-instance).

## [1.5.0] - 2026-04-17

### Added
- **SOUL.md content scanner** (`soul_scanner`): defends against "Soul-Evil Attack" —
  detects hidden HTML comments, invisible Unicode, zero-width steganography, data URIs,
  and hidden HTML tags in SOUL.md files.
- **Agent Stability Index** (`stability_index`): quantifies identity drift between
  SOUL.md versions with configurable thresholds (Warning / Critical).
- **Template sanitizer** (`template_sanitizer`): sanitizes prompt templates for
  injection resistance.
- **SoulSpec v0.5 compatibility**: soul_partition now recognizes SoulSpec v0.5 headers
  (Core Identity, Personality, Learned Patterns, etc.), with validation and export.
- **Audit Logs page**: new History tab showing JSONL audit events with severity icons,
  agent/channel/user badges, and expandable JSON details. Existing real-time log stream
  moved to Realtime tab.
- **Billing usage API** (`billing.usage`): returns live session count, active agents,
  connected channels, and inference hours from actual data sources.

### Changed
- GVU updater now runs soul_scanner + ASI checks before applying SOUL.md proposals.
- Soul guard integrity check includes content scan on every run and ASI on drift.
- BillingPage simplified — removed stub plan card, payment method, invoice history,
  and upgrade sections (not applicable to community edition).
- Logs nav icon changed from ScrollText to FileText; label renamed to "Audit Logs".

### Fixed
- Clippy: `sort_by_key` with `Reverse` instead of `sort_by` closure (3 occurrences).
- Windows sandbox test split with `cfg(not(windows))` / `cfg(windows)`.
- `clippy::collapsible_match` allow in webchat.
- CI: ignore RUSTSEC-2026-0098 and RUSTSEC-2026-0099.


All notable changes to DuDuClaw are documented here. For the authoritative
version history and per-commit detail, see `git log`.

## [v1.4.31] — 2026-04-16

### Fixed

- **GVU JSON fence parsing.** Rewrote `strip_json_fences()` to handle LLM
  responses with trailing text after the closing ` ``` ` fence. Previous
  implementation used `strip_suffix` which failed when judges appended
  commentary, causing 22 consecutive GVU trigger failures since 4/07.
  Unified fast-path and preamble-path into a single `rfind`-based approach.

### Changed

- Dashboard live data, logs fix, analytics API (from v1.4.30)

---

## [v1.4.29] — 2026-04-16

### Added

- **Skill auto-synthesis (Phase 3-4).** Gap accumulator detects repeated
  domain gaps → synthesizes skills from episodic memory (Voyager-inspired)
  → sandbox trial with TTL management → cross-agent graduation to global
  scope. New MCP tools: `skill_security_scan`, `skill_graduate`,
  `skill_synthesis_status`.

- **Task Board.** SQLite-backed task management with status/priority/
  assignment tracking and real-time Activity Feed via WebSocket. MCP tools:
  `tasks.list`, `tasks.create`, `tasks.update`, `tasks.assign`,
  `activity.list`, `activity.subscribe`.

- **Shared Knowledge Base.** Cross-agent wiki at `~/.duduclaw/shared/wiki/`
  for organizational knowledge (SOPs, policies, product specs). Wiki target
  classification (agent/shared/both), visibility control via `wiki_visible_to`
  capability, full-text search with author attribution. MCP tools:
  `shared_wiki_ls`, `shared_wiki_read`, `shared_wiki_write`,
  `shared_wiki_search`, `shared_wiki_delete`, `shared_wiki_stats`, `wiki_share`.

- **Autopilot rule engine.** Event-driven automation — triggers: task_created,
  task_status_changed, channel_message, agent_idle, cron. Actions: task_delegate,
  notify, skill_execute. Dashboard Settings → Autopilot tab for rule management
  and execution history.

- **Skill Market three-tab UI.** Marketplace / Shared Skills / My Skills with
  skill adoption flow and usage statistics.

- **Security status endpoint.** Exposes credential proxy, mount guard, RBAC,
  rate limiter, and SOUL drift state via API.

- **Analytics endpoints.** Conversation summaries and cost savings tracking.

### Enhanced

- MCP Server expanded from 70+ to 80+ tools.
- Dashboard i18n keys expanded from 540+ to 600+ (zh-TW / en / ja-JP).
- Evolution config extensibility for skill synthesis thresholds, graduation
  criteria, and curiosity-driven exploration.
- `CapabilitiesConfig` now includes `wiki_visible_to` with explicit `Default`
  implementation and `sanitize()` for safe deserialization.

## [v1.4.28] — 2026-04-15

### Fixed

- **Cognitive memory not persisted to database.** `StoreEpisodic` action
  from the prediction router was only debug-logged but never written to
  the per-agent `memory.db`. Dashboard Memory & Skills page showed empty
  even with cognitive memory enabled. Now creates
  `agents/<id>/state/memory.db` and stores `MemoryEntry` via
  `SqliteMemoryEngine`, making episodic observations queryable from the
  dashboard and MCP `memory.search` / `memory.browse` tools.

## [v1.3.17] — 2026-04-12

### Added

- **Action-claim verifier wired into live reply path (shadow mode).**
  The existing `duduclaw_security::action_claim_verifier` module (420
  lines, 13 unit tests, pure regex + audit-log cross-reference, zero
  LLM cost) was built but **never called from production code**. It is
  now invoked at two critical points:

  1. **Channel replies** ([channel_reply.rs](crates/duduclaw-gateway/src/channel_reply)):
     immediately after the Claude CLI subprocess returns and before the
     reply is saved to the session / shipped to Discord / Telegram / LINE.
  2. **Cron task execution** ([cron_scheduler.rs](crates/duduclaw-gateway/src/cron_scheduler.rs)):
     after the scheduled agent responds and before `record_run` marks
     the task as successful.

  On both paths, a `dispatch_start_time` is captured before the CLI
  call. After the reply arrives, `detect_hallucinations(home_dir,
  agent_id, &reply, &dispatch_start_time)` extracts action claims via
  regex (zh-TW + English patterns for AgentCreated / AgentDeleted /
  SoulUpdated / MessageSent / AgentSpawned), reads the MCP tool-call
  audit log (`tool_calls.jsonl`) filtered to this turn + this agent,
  and cross-references each claim against actual successful tool calls.

  **Shadow mode**: detections are logged at `warn!` level and written
  to `security_audit.jsonl` via `log_tool_hallucination()`, but the
  reply text is **not modified**. This lets us collect a baseline
  `ungrounded_claim_rate` before flipping to enforce mode.

- **Implementation plan document** at [docs/TODO-agent-honesty.md](docs/todo/TODO-agent-honesty.md):
  3-phase defence-in-depth roadmap (Action-Claim Verifier → Proxy State
  Verifier + Abstain Actions → Tool Receipts / NabaOS), backed by 6
  verified arxiv papers (ToolBeHonest 2406.20015, Agent-as-a-Judge
  2410.10934, Relign 2412.04141, MCPVerse 2508.16260, Agent Hallucination
  Survey 2509.18970, Tool Receipts 2603.10060). Day-by-day schedule,
  success metrics, known limitations, and enforce-mode policy options.

---

## [v1.3.16] — 2026-04-12

### Fixed

- **`duduclaw agent create` now writes `.mcp.json`.** New agents created
  via the CLI (or the `wizard` subcommand) previously got every scaffold
  file *except* `.mcp.json`, which meant the duduclaw MCP server never
  attached to their Claude Code sessions and tools like `create_agent`,
  `spawn_agent`, `list_agents`, `send_to_agent` were silently unavailable.
  SOUL.md's "always call `create_agent`" rule became unenforceable
  because the tool literally didn't exist in the model's toolbelt — the
  model either fell back to raw Bash writes (blocked by agent-file-guard
  since v1.3.15) or fabricated agent creation in plain text. Both the
  CLI (`cmd_agent_create`) and the industry wizard now write a
  `.mcp.json` pointing at the currently-running duduclaw binary.

- **Hint message placeholder not expanded.** `duduclaw agent create`
  used to print `Run \`duduclaw agent run {agent_name}\` to start a
  session` literally with `{agent_name}` unexpanded (because the string
  was passed to `style()` instead of `format!()`). The hint now shows
  the real agent name.

### Added

- **`duduclaw agent create` flags.** The subcommand previously took
  only a positional `name`. It now accepts `--display-name`, `--role`,
  `--reports-to`, `--icon`, and `--trigger` so teams can be scripted
  without post-hoc `sed` on `agent.toml`:

  ```sh
  duduclaw agent create xianwen-tl \
    --display-name "Xianwen TL" \
    --role team-leader \
    --icon 🎯
  ```

- **`AgentRole` enum gained `TeamLeader` and `ProductManager`** so
  planner/coordinator agents can declare a more specific role. The enum
  serialisation switched from `rename_all = "lowercase"` to
  `rename_all = "kebab-case"`; single-word variants (`main`, `worker`,
  `qa`, `planner`, …) look identical to the old encoding so existing
  `agent.toml` files keep parsing unchanged. Multi-word variants use
  kebab-case (`team-leader`, `product-manager`).

- **Lenient role parsing.** `AgentRole::from_str` normalises spacing /
  case / underscore vs hyphen and accepts common aliases: `engineer`
  (→ Developer), `tl`/`lead`/`teamlead` (→ TeamLeader), `pm`
  (→ ProductManager), `quality`/`quality-assurance` (→ Qa). The same
  aliases are accepted by serde via `#[serde(alias = …)]`, so
  round-tripping natural-language role input through `agent.toml`
  resolves to the canonical form on the next read.

- **`AgentRole::as_str()` + `Display` impl + `valid_values_help()`**
  helpers for error messages. The MCP `agent_update` handler now uses
  `AgentRole::from_str` with a single shared help string instead of its
  own private match table.

### Tests

- 6 new unit tests in `duduclaw_core::types::tests` covering round-trip
  (`agent_role_roundtrip_via_serde_json`), wire format
  (`agent_role_kebab_case_wire_format`), serde aliases
  (`agent_role_serde_aliases_accepted`), lenient `FromStr` parsing
  (`agent_role_from_str_lenient_normalisation`), rejection of garbage
  (`agent_role_from_str_rejects_garbage`), and `Display` round-trip.

---

## [v1.3.15] — 2026-04-11

### Fixed

- **agent-file-guard now blocks Bash-based agent-structure writes.** The
  PreToolUse hook matcher was previously `Write|Edit|MultiEdit` only, so a
  sub-agent could silently bypass the guard by invoking
  `Bash mkdir -p /some/project/.claude/agents/foo` or
  `Bash cat > /some/project/.claude/agents/foo/agent.toml`. The guard now
  also matches `Bash`, and `cmd_hook_agent_file_guard` dispatches on
  `tool_name` so that Bash commands are inspected against the new
  [`duduclaw_core::check_bash_command`] helper.

  **Policy:** any Bash command whose text contains the substring
  `.claude/agents/` is blocked. Rationale — the canonical agent root is
  `~/.duduclaw/agents/<name>/` and never contains that path segment, and
  project trees that an agent *works on* should never have an in-tree
  `.claude/agents/` directory (Claude Code's own config lives at
  `~/.claude/`, not nested in project repos). The rule is intentionally
  conservative: even read-only listings that mention `.claude/agents/`
  are blocked, since the correct replacement is the `list_agents` MCP
  tool or a direct `Read` on a known canonical path.

  Existing agents get the updated matcher automatically on next invocation
  (the hook installer runs on every `call_claude_for_agent_with_type` and
  updates the tagged hook entry in place — no manual action required).

### Tests

- 8 new unit tests in `duduclaw_core::agent_guard::tests`
  (`bash_mkdir_in_foreign_project_is_blocked`,
  `bash_write_to_agent_toml_via_heredoc_is_blocked`,
  `bash_with_quoted_path_is_blocked`,
  `bash_ls_mentioning_sentinel_is_also_blocked`,
  `bash_git_status_is_allowed`,
  `bash_ls_canonical_agent_dotclaude_is_allowed`,
  `bash_touching_claude_hooks_subdir_is_allowed`,
  `bash_nested_agents_under_home_is_still_blocked`).

---

## [v1.3.14] — 2026-04-11

### Added

- **SQLite-backed cron task store with hot reload.** Replaced the legacy `cron_tasks.jsonl` file with a proper relational store at `~/.duduclaw/cron_tasks.db` (WAL mode). The new `CronStore` module ([crates/duduclaw-gateway/src/cron_store.rs](crates/duduclaw-gateway/src/cron_store.rs)) exposes full CRUD (`list_all`, `list_enabled`, `get`, `get_by_name`, `insert`, `update_fields`, `set_enabled`, `delete`, `record_run`) and tracks run history (`last_run_at`, `last_status`, `last_error`, `run_count`, `failure_count`) so the dashboard can surface per-task reliability metrics.

- **Hot-reload signal for `CronScheduler`.** The scheduler's run loop now uses `tokio::select!` to wake on **either** a 30-second baseline tick **or** an `Arc<Notify>` pulse fired by `CronScheduler::reload_now()`. Dashboard edits (`cron.add` / `cron.update` / `cron.pause` / `cron.resume` / `cron.remove`) now take effect immediately — no more 5-minute reload window. MCP subprocess writes are picked up on the next 30-second tick via shared WAL-mode SQLite (no inter-process signal needed).

- **New dashboard RPC methods:** `cron.update` (partial-field update) and `cron.resume` (re-enable paused task). All cron handlers now accept either `id` or `name` for identification, and `cron` or `schedule` for the expression (legacy alias).

- **One-shot JSONL → SQLite migration.** On first startup after upgrade, `CronStore::migrate_from_jsonl` imports any existing `cron_tasks.jsonl` entries into the DB, then renames the file to `cron_tasks.jsonl.migrated` to avoid re-running. Idempotent and safe to re-invoke.

### Changed

- **MCP `schedule_task` writes to SQLite directly** instead of appending JSONL. Both the gateway process and the MCP subprocess share the same WAL-mode DB — safe for concurrent access.

- **Last-run merge strategy on reload.** When the scheduler reloads (either via hot-reload signal or baseline tick), each task's `last_run` is merged as `max(in-memory, DB last_run_at)` to prevent same-minute re-fires after a mid-cycle reload.

### Tests

- 2 new unit tests for `CronStore`: CRUD roundtrip + JSONL migration idempotency.

---

## [v1.3.13] — 2026-04-11

### Added

- **Stream-json diagnostics on CLI failures.** The `channel_reply::spawn_claude_cli_with_env` now tracks stream-json event counts (`lines_seen`, `events_parsed`, `assistant_events`, `text_blocks`, `thinking_blocks`, `tool_use_blocks`, `result_events`) and captures the last raw stream line, `result.subtype`, the latest `message.stop_reason`, and a tail of stderr. All of these are embedded into the error message when `spawn_claude_cli_with_env` returns `Empty response from claude CLI` or non-zero exit. `channel_failures.jsonl` is now self-describing — no more needing to reproduce manually in a shell to figure out *why* a reply was empty.

- **`DUDUCLAW_STREAM_DEBUG=1` env var.** When set on the gateway process, every raw line from `claude`'s stdout is appended to `<home>/claude_stream.log`. Off by default (the log can be large and contains user prompts).

- **Stderr draining.** A background tokio task drains `claude` CLI's stderr pipe concurrently and keeps the last 2 KiB for error diagnostics. Without this, `claude` could block forever if stderr filled its pipe buffer (~64 KiB).

### Changed

- **Classifier substring matching still works on diagnostic-suffixed errors.** The error strings returned by `spawn_claude_cli_with_env` now look like:
  ```
  Empty response from claude CLI (exit=0 lines=42 events=30 assistant=2 text_blocks=0 thinking=1 ...)
  ```
  `classify_cli_failure` uses substring matches so the same reason (`EmptyResponse`, `SpawnError`, etc.) is still detected. Two new regression tests lock this invariant.

### Tests

- **415 tests passing** (core: 21, gateway: 377, agent: 17). Added 2 new classifier tests for diagnostic-suffixed error strings.

---

## [v1.3.12] — 2026-04-11

### Fixed

- **Rotator broke keychain auth by injecting `CLAUDE_CONFIG_DIR=~/.claude`**
  (regression from the multi-account rotation introduced in v1.3.11). When
  the auto-detected default OAuth session was selected, `select()` set
  `CLAUDE_CONFIG_DIR` to `~/.claude` even though that *is* the claude CLI
  default — and the `claude` CLI, when the env var is set explicitly, stops
  looking at the macOS keychain for credentials. Every channel reply call
  then hit "Not logged in · Please run /login".
  Fix: `account_rotator::select()` now skips the `CLAUDE_CONFIG_DIR`
  injection when `credentials_dir` equals the default `~/.claude`, so
  claude CLI picks up keychain auth normally. Non-default profile
  directories (`~/.claude/profiles/work`, etc.) still get the env var.
  Regression tests in `account_rotator::select_env_tests` lock this in.

- **Stream parser silently swallowed `is_error: true` results.** The
  `claude` CLI emits terminal errors (auth failure, synthetic responses)
  as `type="result"` stream-json events with `is_error: true`, with the
  error text in the `result` field. Both `channel_reply::spawn_claude_cli_with_env`
  and `claude_runner::call_claude_streaming` were capturing the error
  text as `result_text` and returning `Ok(...)`, so users saw
  "Not logged in · Please run /login" posted to Discord/LINE/Telegram as
  Agnes's actual reply. Now:
  - `is_error: true` on a `result` event → `return Err("claude CLI stream error: ...")`
  - `error` field on an `assistant` event → same
  - Post-loop: any non-zero exit code is a hard failure (previously we
    only errored when `result_text` was empty, which let partial output
    leak through).

- **`FailureReason::AuthFailed` classifier** — new branch in
  `classify_cli_failure` detects `"Not logged in"` / `"authentication_failed"` /
  `"please run /login"` and surfaces a zh-TW message that actually tells
  the user to run `claude /login` instead of the misleading
  "`claude auth status`" hint (which only checks state, doesn't fix auth).

### Tests

- 2 new regression tests in `duduclaw-agent::account_rotator::select_env_tests`
- 2 new classifier tests + 1 end-to-end pipeline test in `channel_reply::fallback_tests` / `rotation_tests`
- **413 tests total passing** (core: 21, gateway: 375, agent: 17)

---

## [v1.3.11] — 2026-04-11

### Added

- **Agent file-write guard (Option 3 hardening)** — `duduclaw hook
  agent-file-guard` PreToolUse hook is now automatically installed into
  `<agent_dir>/.claude/settings.json` on every agent creation (MCP
  `create_agent`, dashboard `agents.create`, CLI `wizard`, channel reply
  spawn, dispatcher spawn, and gateway startup). Blocks agents from using
  raw Write/Edit/MultiEdit to create `agent.toml` / `SOUL.md` / `CLAUDE.md`
  / `MEMORY.md` / `.mcp.json` / `CONTRACT.toml` outside the canonical
  `<home>/agents/<name>/` tree. Agents must use the `create_agent` MCP
  tool instead, so the registry and dashboard always see newly-created
  sub-agents. Pure Rust enforcement — no shell dependencies, cross-platform
  (macOS/Linux/Windows).
  Files: `crates/duduclaw-core/src/agent_guard.rs`,
  `crates/duduclaw-gateway/src/agent_hook_installer.rs`,
  `crates/duduclaw-cli/src/lib.rs` (new `Hook` subcommand).

### Fixed

- **Channel reply: intermittent "Claude Code not found" error (#fallback-fix)**
  Root cause: the channel reply path (`channel_reply::call_claude_cli`) was
  bypassing the `AccountRotator` entirely and spawning `claude -p` against
  the ambient environment. When the single default OAuth session was cooling
  down (rate-limit / token refresh / billing), every attempt failed and the
  user saw a hardcoded "please run `claude auth status`" message that
  misrepresented the actual cause. The sub-agent dispatcher path already
  rotated correctly, which explained the "有機率" symptom.

  This release routes the channel reply path through a new testable
  rotation primitive `rotate_cli_spawn`, so **both** the dispatcher and
  channel paths now use the same multi-OAuth / API-key rotation, cooldown
  tracking, and billing-exhaustion handling.
  Files: `crates/duduclaw-gateway/src/channel_reply.rs`.

- **Misleading fallback error message → category-specific diagnostics**
  Replaced the hardcoded `"{name} 收到你的訊息，但目前無法回覆。請確認 Claude
  Code 已安裝並登入"` message with a classifier (`FailureReason`) that
  distinguishes:
  - `BinaryMissing` — actually missing binary (keeps the `auth status` hint)
  - `RateLimited` — 忙線中，請稍後再試
  - `Billing` — 帳號額度已用完
  - `Timeout` — 30 分鐘處理超時
  - `SpawnError` — 子程序啟動失敗
  - `EmptyResponse` — 空回應
  - `NoAccounts` — 尚未設定帳號
  - `Unknown` — 通用錯誤提示

  Each fallback also appends a structured JSONL record to
  `~/.duduclaw/channel_failures.jsonl` for dashboard surfacing.

- **`which_claude()` now discovers launchd / Finder-launched installs**
  Added candidate paths for `/opt/homebrew/bin/claude` (Apple Silicon
  Homebrew), `$HOME/.bun/bin/claude`, `$HOME/.volta/bin/claude`,
  `$HOME/.asdf/shims/claude`, plus NVM version-directory scanning
  (`$HOME/.nvm/versions/node/*/bin/claude`). Previously, gateways launched
  from Finder / Dock / launchd without Homebrew on `PATH` would fail to
  find `claude` even when it was installed.

  Also extracted `which_claude_in_home(home: &Path)` as a pure, testable
  helper that doesn't touch `PATH` or environment state.
  Files: `crates/duduclaw-core/src/lib.rs`.

### Added

- **`AccountRotator::push_account_for_test`** — cross-crate test helper
  (marked `#[doc(hidden)]`) so rotation unit tests can inject synthetic
  accounts without writing a config file or shelling out to `claude auth
  status`. Files: `crates/duduclaw-agent/src/account_rotator.rs`.

### Tests

- 7 new unit tests in `duduclaw-core::which_claude_tests` covering Bun,
  Volta, asdf, npm-global, NVM, candidate ordering, and "no candidates"
  fallback.
- 10 new unit tests in `duduclaw-gateway::channel_reply::fallback_tests`
  covering `classify_cli_failure` (rate-limit / billing / timeout / binary /
  empty / spawn / unknown) and `format_fallback_message` (message content
  assertions for zh-TW, agent name substitution, correct vs. misleading
  hints).
- 6 new async tests in `duduclaw-gateway::channel_reply::rotation_tests`:
  - `single_account_success_is_first_try` — smoke-replacement for the
    single-OAuth regression path
  - `rotation_advances_past_rate_limited_account` — verifies 2-account
    cycling and rotator state after `on_rate_limited`
  - `rotation_all_fail_propagates_last_error` — all-fail aggregator
  - `rotation_billing_error_triggers_long_cooldown` — 24h cooldown
  - `rotation_empty_rotator_returns_empty_exhausted` — primitive contract
  - `end_to_end_rate_limit_yields_busy_message` — full pipeline from
    rotation failure → classification → user message; guards against
    future regressions where the message incorrectly says "please install"

### Developer Notes

- `is_billing_error` and `is_rate_limit_error` in `claude_runner.rs` are now
  `pub(crate)` so the channel reply path can reuse the shared classifiers.
- `spawn_claude_cli_with_env` carries `#[allow(clippy::too_many_arguments)]`
  (8 args, pure extraction from the pre-existing 7-arg `call_claude_cli`).
- The rotation loop is now decoupled from the subprocess spawn: see
  `rotate_cli_spawn<F, Fut>(rotator, spawn, input_size_hint)`. This enables
  deterministic testing and future reuse (e.g., for other LLM backends).

---

Earlier versions: see `git log --oneline` for commit-level history.
Recent highlights:

- **v1.3.10** — Discord cross-channel reply error, cognitive memory toggle reset
- **v1.3.9** — Discord auto-thread sends guide message in channel
- **v1.3.8** — service stop kills process, all-channel attachment forwarding
- **v1.3.7** — Homebrew formula version alignment
