# 儀表板設定對照（v1.80.0）

## 進階設定與公司資訊（v1.80.0）

兩個版本的管理員都可開啟「設定 → 進階」（`/app/system/settings/advanced`）。較少使用的設定集中在同一頁，分成下列四組。搜尋會依目前語言（繁體中文、英文、日文）即時篩選區塊名稱、說明與欄位文字；展開符合的區塊即可編輯，沿用原有儲存方式、重啟提示及 RPC 權限。「一般、帳號、語音、主動通知」保留在常用設定。非管理員看不到兩個新頁面的導覽，也無法直接開啟。

| 分組 | 設定與固定錨點 |
|---|---|
| 系統與帳務 | 系統 `#system`、帳號輪替 `#rotation`、診斷 `#doctor`、帳務 `#billing`、設定檔編輯 `#configRaw` |
| 安全與紀錄 | 沙箱、檔案存取、信任、追蹤及秘密管理 `#security`；日誌等級 `#logging`；容器 `#container`；委派 `#delegation`；去識別化 `#redaction`；瀏覽器 `#browser` |
| AI 模型 | 背景模型 `#utility`、知識搜尋 `#knowledgeSearch` |
| 實驗功能 | 心跳 `#heartbeat`、自動化 `#automation`、校準 `#calibration`、自動駕駛 `#autopilot`、技能合成 `#skillSynthesis` |

舊設定連結如 `?tab=system`、`?tab=automation`、`?tab=configRaw` 會轉到對應錨點。一般設定不再重複提供帳號輪替與日誌等級，改為連到新位置。下方對照表仍描述相同的設定鍵，編輯入口已集中到這一頁。

「設定 → 公司資訊」（`/app/system/settings/company`）可設定最多 120 字的公司名稱（不接受控制字元、換行分隔字元與 Unicode 格式／雙向控制字元），以及不超過 1 MiB 的 PNG、JPG 或 WebP 標誌。SVG 與動畫圖片會被拒絕。伺服器檢查實際圖片內容，限制原圖最大 4096 × 4096，解碼後移除中繼資料，再產生不超過 256 × 256 的 PNG 縮圖。儲存時先透過 RPC 更新名稱與選填的移除標誌操作，再透過 HTTP 上傳選取的圖片；兩者不是單一交易。圖片上傳失敗時，已儲存的名稱仍會保留，選取的檔案也會保留供重試，畫面不會顯示全部儲存成功。名稱留空時使用產品名稱；沒有公司標誌或標誌載入失敗時，沿用產品標誌。

側欄與登入頁上方顯示公司識別，邀請信及寄送前預覽也使用公司名稱與標誌。簽章驗證的經銷商產品識別保留在關於頁與登入頁下方，不會被公司設定改寫。設定清單的公司名稱項目改讀這份公司資訊並連到此頁；原本的一般顯示名稱仍用於 gateway 的區網名稱。

`company.get` 與 `company.update` 都需要管理員權限；後者只接受 `name` 與選填的 `remove_logo`，拒絕 `logo_base64`。標誌透過 `POST /api/company/logo` 傳送原始二進位檔，將已儲存的 access token 放在 `Authorization: Bearer`，不使用 Cookie。gateway 在讀取內容前驗證 JWT 與目前的管理員帳號；路由和串流讀取各自執行 1 MiB 上限，Content-Type 必須符合 PNG、JPEG 或 WebP 檔頭。每個帳號與 TCP 來源 IP 各限 60 秒內六次（代理後方的用戶共用該 IP 額度）。IP 限流先於身分驗證，超額直接回 429、不逐筆稽核；通過此限制的請求與結果才會稽核。帳號限流在即時管理員驗證後執行。內容讀取逾時 30 秒，同時最多兩項上傳工作。全域 WebSocket 訊息上限維持 1 MiB。更新透過既有鎖定設定寫入流程儲存 `[company] name`、`logo_hash`，清理後的圖片位於 `<home>/company_logos/`。公開的 `/api/company` 僅回傳名稱與標誌網址，供登入頁顯示。公開讀取器只開啟標誌一次，在同一檔案描述符上確認一般檔案、單一連結與大小上限，再驗證 PNG 內容與 SHA-256 雜湊。標誌網址含該雜湊，以 PNG 和 `nosniff` 回應並允許快取一年。舊縮圖即使移除後仍保留，供已寄出的邀請信與快取頁面使用，請視為公開素材。儲存的分頁立即更新，其他分頁重新取得焦點或重連時會重新讀取。

**Codex 外部設定檢查（尚未發佈）：** 外部設定不存在或未定義 MCP 時，一般員工、Explore、utility、Task Sandbox 與 Discovery 可執行；偵測到 MCP 或既有設定無法讀取／解析時，回報來源並拒絕。明確指定的 Codex 判官遭拒時，驗收失敗，不改用預設判官。這是啟動前的本機／快取快照檢查，無法保證後續雲端更新的完整隔離；[來源範圍與限制](../../architecture/extension-gates.md)。

儀表板放在路徑前綴下時，請在 `config.toml` 設定 `[dashboard] public_url = "https://host/agents"`，網址必須包含路徑。nginx 範例：`location /agents/ { proxy_pass http://127.0.0.1:18889/; }`；Google Cloud 請登記 `https://host/agents/api/me/google/callback`（遠端 MCP 使用 `https://host/agents/oauth/mcp/callback`）。只有公開網址與瀏覽器已驗證的儀表板 origin 相同時，OAuth 才會套用這個前綴；區網存取與貼回網址流程維持原行為。升級至包含此修正的版本並更新已登記的回呼網址後，可移除 v1.79.1 為無前綴 `/api/me/google/callback` 與 `/me/connections` 加上的 nginx 暫時規則。

企業版設定（v1.76.0，成員管理 → 組織設定）：`users.org_settings.get/update` 管理 `[people] manager_team_history`，有效設定檔預設開，變更立即生效；重新開啟會重設時間窗，進階編輯器也列為受保護稽核鍵。開啟前請先告知職員。見[主管唯讀團隊對話](../../features/zh-TW/68-manager-team-history.md)。

這份文件列出 v1.68.0 起可以在儀表板設定的項目：在哪一頁、寫到哪個設定鍵、存檔後是否要重啟 gateway。最後一節說明「設定檔進階編輯」，用來修改沒有專屬控制項的鍵。

英文版：[../dashboard-settings.md](../dashboard-settings.md)　日文版：[../ja-JP/dashboard-settings.md](../ja-JP/dashboard-settings.md)

目標費用與重複呼叫防護目前在「設定檔進階編輯」設定，沒有獨立的自動化控制項。`config.toml [goal_loop]` 的 `default_cost_cap_cents = 0` 是未設定個別上限的目標所使用的美分上限（`0`＝不限額）；`tool_streak_pause = 8` 表示一輪結束後，相同工具與參數連續呼叫達此次數就暫停（`0`＝關閉）。透過進階編輯器儲存會重新載入目標迴圈；直接修改 `config.toml` 則需重新啟動 gateway。`tool_streak_advisory` 另行控制既有提醒。個別上限可在交辦目標時填寫；`/goals` 顯示費用，並讓有權限的操作者提高暫停目標的上限後重試。


## 公司設定清單與主要 AI 員工（v1.79.0）

企業版管理員可從「管理」或首頁入口卡片開啟「設定清單」（`/setup`）。個人版及其他角色不顯示此頁。清單分成「公司、人員、能力」，各項連到既有設定頁；清單本身只讀取資料。必填項目包含公司名稱、知識庫、AI 帳號、未封存的 AI 員工、至少兩名啟用中的使用者、已連線的通道、每位一般員工使用者都有 AI 員工綁定、技能。Google、Odoo、自動化為選填，不計入「已完成 N / M」。讀取失敗顯示「無法確認」，不算完成。必填項目全部完成後，首頁卡片自動隱藏；也可關閉卡片，選擇會儲存在目前瀏覽器的本機儲存空間。之後仍可從「管理」開啟清單。

一般員工可在側欄 AI 員工的選單中選「設為主要」。該員工會標示「主要」並排在最前面。已登入使用者可透過 `users.set_primary_agent {agent_id}` 選擇目前已綁定的員工，傳入 `{agent_id: null}` 可清除偏好；這不會新增權限或變更綁定。非管理員開始網頁新對話時，伺服器優先使用仍已綁定、可呼叫、已載入且不是主代理的主要員工；否則只在恰好有一位可呼叫的非主代理時使用該員工。沒有符合的員工或有多位可選時，不會開始對話。有多位時請先在一般員工側欄「設為主要」；若你的角色看不到此選項，或沒有可用員工，請聯絡管理員。

## 員工健康（v1.80.0）

管理員與主管可在兩種版本開啟「員工健康」（`/employee-health`）。頁面先顯示摘要，展開各位員工後可查看最近一次有紀錄的回覆、過去 24 小時或 7 天的失敗分類、等你決定或暫停中的目標，以及帳號可用狀態。失敗分成空白回覆、額度或預算限制、請求過於頻繁、認證失效、逾時、權限檢查拒絕、傳送失敗與其他錯誤。帳號狀態只反映符合員工明確設定 `account_pool` 的帳號；未設定帳號池時顯示無法確認，也不會測試 CLI 本身的登入狀態或即時連線服務商。

最近一次成功回覆代表共用通道回覆流程已產生非空白內容，**不代表通道已送達**；舊紀錄可能無法確認這個時間。失敗次數合併 `channel_failures.jsonl` 與獨立的 `employee_health.jsonl` 回合紀錄，各最多讀取最後 4 MiB，結果約快取 30 秒。顯示資料不完整時，代表紀錄無法取得或已達讀取上限，所選時間窗內的次數可能不齊全。工具呼叫成功或已有費用紀錄，都不會當成回覆成功。

健康提醒沿用各通道共用的回覆流程。預設連續五次回覆失敗，或收到訊息後持續六小時沒有成功產生回覆，就提醒一次；成功回覆後重設。已有認證中斷提醒時，不再重複發送只有認證失效的健康提醒。通知透過既有推播與勿擾時段流程，送給啟用中的管理員及該員工擁有者所連結、已驗證的通道。傳送前會先記錄這段異常已提醒，因此每段異常最多嘗試一次；傳送失敗也不會重試，須等成功回覆後才重設。沒有已連結或可到達的通知對象時，無法保證送達；已嘗試提醒也不等於通知已送達。

管理員可在既有「設定檔進階編輯」調整 `config.toml`，健康頁本身不提供另一份設定表單：

```toml
[employee_health]
enabled = true
consecutive_failures = 5 # 1..100
no_success_hours = 6    # 1..168
```

設定檔不存在或省略這個區段時，使用以上預設值；其他讀取錯誤或設定無效時，關閉健康提醒。關閉提醒不會增加員工權限，也不改變任務執行；健康頁仍為唯讀。

## 一般員工側欄

非個人版的一般員工依序看到儀表板、收件匣、自動化、技能庫、記憶、AI 虛擬辦公室與唯讀組織圖，再接綁定的 AI 員工與對話紀錄。搜尋開啟既有命令面板。主要 AI 員工排在最前，其餘依 Owner、Operator、Viewer 排序，最多顯示八位。收件匣只保留一個入口。個人選單顯示綁定 Email、我的連線、設定、亮暗度、語言、取得幫助與登出；語言會開啟帳號設定的語言選擇。伺服器提供經銷商資訊時，「取得幫助」連到該入口，否則開啟產品指南。員工版只在「關於」顯示版本號。管理員、主管與個人版維持原有導覽配置。

自動化由伺服器逐筆檢查：員工可讀取本人建立的紀錄與直接綁定 AI 員工的紀錄；Viewer 綁定只能查看，Operator、Owner 綁定可管理。建立時仍須能操作目標 AI 員工，解除綁定會在下一次請求生效。組織圖只提供組織結構欄位，不提供模型、預算、憑證、聯絡資料或編輯控制。

非 Admin 的 cron 使用 UTC 排程，僅能用未啟用任務沙箱、在主機執行的 AI 員工；每人最多 20 筆啟用排程、每位 AI 員工 50 筆，每帳號滾動一小時最多建立 20 筆，最短間隔 15 分鐘。新增／更新訊息會經過對話輸入防護，拒絕時留下稽核。組織圖只顯示直接綁定與本人部門，範圍外上級只提供名稱與角色。Admin 既有行為不變。

這些限制也適用於經 MCP 新增或修改、帶人類來源的排程；這類 MCP 排程預設 UTC，未帶來源標記的 Admin／背景排程保留本地時區偵測。手動執行每 10 分鐘最多受理每排程 3 次、每帳號 10 次，每帳號最多 2 件、全域最多 4 件未完成工作；忙碌時立即拒絕。手動執行的窗口在 gateway 重啟後重置。員工 cron 表達式上限為 256 bytes。

新對話列出目前帳號最近送出使用、且仍可使用的最多五個技能及簡短說明。可連續點選多個標籤，把 `/技能名稱` 帶入草稿，再補充需求並送出；點選本身不會執行。紀錄由伺服器在對話或任務受理後保存，讀取時重新檢查技能權限，不代表 AI 已成功執行技能。交辦任務頁在啟用時也使用同一份紀錄。

任務看板（`/tasks`）、目標任務（`/goals`）與交辦任務（`/assign`）入口對所有角色隱藏，包含管理員與個人版。側欄主要按鈕和手機底部中央的 ＋ 都會開啟新對話；原本帶入的文字成為尚未送出的草稿。`/assign` 轉到 `/chat`。仍可透過原有連結開啟 `/tasks`、`/tasks/:id` 與 `/goals`，通知和任務卡片的連結保留，權限照舊。任務資料、排程與後端 API 沒有移除。「責任」（`/responsibilities`）與「預見」（`/foresight`）維持原有可見性與權限。

`config.toml [dashboard] employee_tasks` 仍會解析，並由 `system.status` 回傳，但不再恢復任務看板、目標任務或交辦任務入口。它對「責任」與「預見」的原有影響保留；此設定沒有專用設定頁。

```toml
[dashboard]
employee_tasks = false
```
## 網頁對話的 AI 員工存取

所有版本的非管理員都不再看到對話上方的員工選擇列。標題、歷史紀錄與對話清單會跟隨伺服器決定的員工。輸入 `@` 仍會列出可呼叫的員工，但不包含角色為主代理的員工，也不包含閘道的預設員工；即使有明確綁定，非管理員仍不能操作它們。每則訊息都會再次檢查，綁定變更最遲在既有的 60 秒權限快取到期後生效。繼續舊的主代理對話時，會顯示與其他不可用員工相同的訊息，不會偷偷換成其他員工。沒有可用員工時，輸入框停用。管理員（含個人版的單一使用者）與舊式管理員權杖連線維持原有聊天行為。任何版本的非管理員帳號都不能透過 `agents.callable` 或儀表板工作 RPC 使用主代理，也不能啟用主代理、決定其審批，或恢復及交還其電腦操作工作階段。

佇列工作缺少發送者時會遭拒並留下稽核紀錄。發往主代理或閘道預設員工的工作若沒有使用者身分，僅接受既定系統發送者（例如 cron、heartbeat），不受 `[delegation]` 模式影響。已知限制：佇列資料尚未簽章，因此擁有同一作業系統使用者之無限制檔案存取權的員工仍可偽造系統發送者；閘道簽章入列機制預計於 v1.80 處理。

## 選用的獨立計畫審查

在 `config.toml [goal_loop]` 設定 `plan_review = "goldfish"`，即可在計畫生成成功後、顯示給人核准前加入獨立審查。預設為 `"off"`；未設定或格式錯誤也視為關閉。三次全新的呼叫在目前任務內並行，各有 45 秒逾時，最多增加 45 秒：重述目標並找出不清楚的地方、刪減不必要的工作、檢查執行所需的決策、權限與資料。每位審查者只收到經 XML 跳脫的目標、驗收標準與計畫，這些內容都視為資料；不帶入對話、記憶、SOUL 或員工身分。回覆要求繁體中文、最多五個條列，每個視角保留最多 400 字元。

呼叫以 `goal-plan-review` 記為系統工具用量，沿用系統的供應商解析方式，不傳入員工目錄，**不計入員工預算**。具版本的 `plan_review` JSON 只供人閱讀，不改寫 `plan_pending` 或 `judge_feedback`，不放進執行或驗收提示，也不取代人工核准。失敗、空白或逾時都顯示「未執行」；即使三者全部失敗，任務仍停在 `needs_human`。任務詳情與收件匣會顯示預設收合的「獨立審查意見（未看過對話）」。通道只顯示已完成視角數量與儀表板提示，不帶審查文字；該數字是完成的視角數，並非逐條意見數。

## 每位員工的工具清單精簡

管理員可在員工總覽的「工具清單精簡」卡片選擇模式、展開未使用工具與查看預估節省量；啟用時可還原清單。模式選單沿用既有員工更新流程。`agents.tool_pruning_status {agent_id}` 與 `agents.tool_pruning_restore {agent_id}` 都限 Admin。估算方式是各候選工具定義序列化後的長度除以四，不是 tokenizer 的實際結果，也不代表供應商實測省下的用量。

```toml
# agent.toml
[capabilities]
tool_pruning = "shadow" # "off" | "shadow" | "on"
```

預設 `shadow` 僅觀察與回報，`tools/list` 回覆與 `off` 完全相同。未知或格式錯誤的值視為 `off`，並警告一次。只有 `on` 會在所有既有清單篩選之後隱藏符合條件的工具。這不會授予或撤銷權限：隱藏工具仍可通過原有 dispatch 閘呼叫；成功呼叫後，工具會經 `notifications/tools/list_changed` 重新出現在清單。

計算只讀取 `tool_calls.jsonl` 最後 5 MB。紀錄不存在、無法讀取或不足十個已記錄回合時，不精簡任何工具。工具至少連續十個回合未使用且跨兩個 session，或跨五個 session 未使用，才符合條件。記憶、任務、工作狀態、訊息、核准、wiki 讀取、工具探索、Admin／未知效果類別工具，以及有有效任務範圍授權的工具都保留。回合以主機寫入的 `(session_id, turn_id)` 唯一組合計算，不是 log 行數。頻道呼叫使用對話 session 與 turn；派工作業使用自身的 dispatch session 與 run，一次 run 算一回合。舊紀錄缺少這些欄位時，不建立回合樣本，但成功使用仍會重設該工具的未使用期間。門檻從各工具最後一次成功使用之後開始計算。每個行程快取五分鐘。還原會寫入該員工的重設標記，不再採計標記時間之前的使用紀錄，讓符合既有權限的完整清單回來；原有權限篩選仍適用。

Claude CLI 的 `minimal_context` 透過 `--tools` 控制內建 CLI 工具，不影響 MCP 工具定義；此流程維持原樣。

## 帳號角色與可見的 AI 員工

給 AI 員工 Bash 或機器操作工具，就代表綁定者可以指揮它使用這些工具。綁定前請先確認這位員工能做什麼，只授權給合適的人；管理員仍可授予一般 AI 員工原生工具。

使用 `system-operator` 預設組的員工只供管理員使用，伺服器會檢查解析後的預設組綁定。非管理員不能綁定、聊天、建立交辦任務或目標，也不能以自己的身分派工給它；`agents.callable` 不會列出它。既有綁定保留，但使用時拒絕。非管理員即使保留 `system-operator` 員工的舊綁定或團隊授權，也讀不到它的對話紀錄、任務、記憶、檔案、自有知識集合與匯出，也收不到即時更新、執行步驟或審批卡片；綁定紀錄保留。非管理員先前建立的交辦排程不再為這位員工建立任務或目標，觸發時自動暫停，恢復時會拒絕。外送審批卡片（CLI 內建工具與 MCP 工具都算）僅供管理員查看，主管不再看到遮罩後的卡片。通道與 Mini App 僅顯示純文字通知，不再顯示外送審批卡片、摘要或操作按鈕。管理員與已授權的無人值守工作維持原有行為。Personal 只有在帳號資料庫不存在、沒有帳號，或僅有一個已完成首次密碼變更的啟用中 Admin 帳號時才適用單一擁有者豁免；停權與離職帳號也計入總數。多帳號部署即使因付費授權到期而回到 Personal，仍須有效管理員身分；帳號資料庫無法讀取時也一樣。

一般員工帳號只看自己直接綁定的 AI 員工。主管也能在側欄、AI 員工清單、時間軸與執行紀錄看到部屬綁定的 AI 員工，並標示為團隊成員的 AI 員工。這些團隊項目只能查看：時間軸顯示工作卡片與時間，執行紀錄顯示狀態與時間，不含對話內容或工具步驟。經由部屬取得的權限不能開啟記憶、員工設定或人格檔案；最高仍是 Operator。直接綁定沿用原有等級，組織入口的人數門檻會合計直接綁定與團隊員工。

在「管理 → 成員」可以逐一查看每個帳號直接綁定的 AI 員工、權限等級，並移除綁定。新增時必須自行選擇 AI 員工，不會預選第一位；預設權限是 Operator。`users.bind_agent` 未填 `access_level` 時也使用 Operator。既有綁定不會自動更動，請在此檢查並移除不該保留的權限。解除綁定後，下一次 RPC 或檔案請求立即採用新權限；舊 token 的下載與預覽連結也一樣。

共享 Wiki 依成員設定中的帳號部門判斷（`users.db` 的 `users.department`）。非管理員可讀公司頁面，以及自己部門的頁面，也必須符合 `visible_to_departments` 限制；未設定部門的帳號只能讀公司頁面。清單、搜尋、頁面內容、統計與匯出都使用相同規則。讀不到權限或可見範圍設定損壞時拒絕讀取。管理員仍可讀全部公司頁面。分享頁面仍需要來源 AI 員工的 Owner 綁定或管理員權限。

公司用量（`billing.usage`）與每日成長報告需要主管權限。小工具生成與模型刷新會使用模型額度或連線到服務商，也需要主管權限。系統日誌需要管理員；一般同仁的自動化依上述逐筆權限開放。報表與首頁的費用區塊、專家頁的上傳安裝按鈕只對管理員顯示；其他角色不會載入這些受限內容。

## Codex CLI 更新

在 macOS 與 Linux，一般員工以 WorkspaceWrite 權限執行時需要 Codex CLI 0.164.0 以上。已安裝版本過舊或無法確認時，Admin 會在儀表板各頁上方看到目前版本與「**更新 Codex CLI**」按鈕。按下後會在閘道主機執行 `npm install -g @openai/codex` 並顯示進度。確認更新成功後，提示會消失，不必重啟閘道。

npm 未安裝或安裝失敗時，提示會提供完整的手動指令與複製按鈕。如果優先找到的仍是 Homebrew 或獨立安裝的舊版，提示會顯示該執行檔路徑；請更新或移除那份舊版，再按「重新偵測」。無法確認版本時也會另行說明，不會顯示成功。Windows 不適用這項版本要求。

## 存檔的共通規則

- 系統設定（`config.toml`）透過 `system.update_config` 寫入，只有管理員能用。頁面只送出有改動的欄位；沒有改動時不送出，顯示「沒有變更，不需要儲存。」
- 寫入前會鎖檔並重新讀取。如果檔案在你讀取之後被其他寫入者改過，這次存檔會被拒絕，重新載入後再存即可。
- `config.toml` 無法解析時，`system.update_config`、常駐感知資料來源與設定檔進階編輯都會拒絕寫入，請先在設定檔進階編輯修好語法。其他會寫 `config.toml` 的設定頁（通道、帳號、Odoo 等）也一樣拒絕寫入。
- 存檔是原地修改檔案，只改你動到的鍵。你寫的註解、空行、鍵的順序和沒動到的鍵的排版都原樣保留，改了值的那一行行尾註解也還在。新增的鍵放在所屬區段的最後，新增的區段放在檔案最後。所有會寫 `config.toml` 或 `inference.toml` 的設定頁都是這樣，不只設定檔進階編輯。
- 需要重啟才生效的鍵會出現在回應的 `restart_required`。系統設定頁與通道管理頁上方會顯示「以下設定要重啟 gateway 才生效」，gateway 重啟後自動消失，也可以手動關閉。推理頁與 AI 員工編輯頁不顯示這條提示。
- 下列鍵每次改動都會另寫一筆稽核事件 `config_protected_key_changed`（含改動前後的值）：`acp.trusted`、`tick.allow_command_sources`、`container.sandbox.when_unavailable`、`container.sandbox.script_when_unavailable`、`memory.supersession_trust_guard`。

## 系統設定 → 進階設定 → 自動化引擎

| 區塊 → 控制項 | 設定鍵 | 生效時機 |
|---|---|---|
| 目標迴圈與派工 → 派工策略，新增「一員工四角色（規劃／執行／審核／合成）」 | `[dispatch] policy = "role_team"` | 立即（派工引擎重新啟動） |
| 目標迴圈與派工 → 驗收判官使用的 AI 執行環境 | `[dispatch] judge_provider` | 立即 |
| 目標迴圈與派工 → 驗收判官使用的模型 | `[dispatch] judge_model` | 立即 |
| 知識與記憶 → 夜間整理（模型階段） | `[night] llm_enabled` | 立即 |
| 真人接手（獨立卡片） | `[takeover] enabled`、`duration_minutes`、`max_duration_minutes`（暫停時間不可超過上限） | 立即 |
| AI 員工信箱（獨立卡片） | `[mail] enabled`、`gmail_enabled`、`dropfolder_enabled`、`default_agent`、`auto_trigger` | 立即 |
| 一員工四角色（全域預設） | `[team] enabled`、`gate`、`[team.roles.<角色>] runtime`／`model`／`effort`（畫面上的「合成」存成 `utility`） | 下一個 goal |
| 常駐感知 → 啟用、預設節奏、允許「執行指令」類來源、網址解析快取（秒） | `[tick] enabled`、`preset`、`allow_command_sources`、`dns_ttl_secs` | gateway 已載入常駐感知時立即重新啟動資料來源，否則要重啟 |
| 常駐感知 → 資料來源（新增、編輯、刪除） | `[[tick.sources]]`，RPC `tick.sources.list` / `upsert` / `remove`（管理員） | 同上；`headers` 的值不會回傳，只回傳數量。寫入 `command` 類來源另記 `config_protected_key_changed` |

## 系統設定 → 進階設定 → 系統

| 區塊 → 控制項 | 設定鍵 | 生效時機 |
|---|---|---|
| 全域背景工作 → 全域輔助後端／模型 | `[runtime] utility_provider`／`utility_model` | 立即生效（每次呼叫重新讀取） |
| 一般 → 日誌格式 | `[logging] format`（`json`；其他值都是純文字） | 重啟 |
| 顯示名稱 | `[general] name` | 重啟（區網廣播在開機時建立） |
| 帳號健康檢查間隔 | `[rotation] health_check_interval_seconds` | 重啟 |
| 任務沙箱 → 沙箱映像、沙箱無法使用時（任務）、沙箱無法使用時（程式碼執行） | `[container.sandbox] image`、`when_unavailable`、`script_when_unavailable` | 立即（逐任務讀取） |
| 任務沙箱 → 資源上限 | `memory_bytes`、`tmp_bytes`、`workspace_bytes`、`pids`、`cpu_millis`、`max_turns`；整段一起驗證（`tmp_bytes + workspace_bytes` 不可超過 `memory_bytes`） | 立即 |
| 任務沙箱 → 電腦操作映像 | `[computer_use] image` | 立即 |
| 記憶、追蹤與信任 → 記憶可信度守門 | `[memory] supersession_trust_guard` | 新的工作階段 |
| 記憶、追蹤與信任 → OpenTelemetry 追蹤送往 | `[telemetry] otlp_endpoint`；只有編譯時含 `otel` 功能的版本才顯示（`system.status` 的 `otel_compiled`） | 重啟 |
| 記憶、追蹤與信任 → GitHub 工具 | `[integrations] github`（也可以關閉） | 立即 |
| 記憶、追蹤與信任 → 信任外部 A2A 請求（只有管理員看得到） | `[acp] trusted` | 立即 |
| 地端檔案根目錄 | `[files] allowed_roots`（完整路徑，不接受檔案系統根目錄，最多 64 個） | 立即 |
| 密鑰管理 → 進階：密鑰後端 | 1Password：`onepassword_host`、`onepassword_vault`、存取權杖；Infisical：`infisical_addr`、`infisical_project_id`、`infisical_environment`、存取權杖。權杖加密存成 `*_enc` | 不回報需要重啟 |

沒有指定員工的全域背景工作（例如選擇執行員工、建立遮罩規則）預設使用 Claude／`claude-haiku-4-5`。選擇支援的後端並填入相容模型；伺服器拒絕已移除的 `gemini` 與未知後端。清空模型會移除設定鍵：Claude 使用 `claude-haiku-4-5`，OpenAI 相容端點必須指定模型。Codex 也可用受限輔助模式執行，模型留白時使用 CLI 預設值。Grok 與通用 CLI 即使可儲存設定，執行時仍拒絕。非 Claude 後端填入已知其他廠商的模型時，頁面會顯示警告。儲存需要管理員權限，沿用既有設定寫入流程。所選全域後端為 Claude、且沒有員工使用 Claude 執行環境時，頁面會顯示警告；這不代表系統已檢查 Claude 登入狀態。

`[general] log_level`（設定 → 進階 → 安全與紀錄 → 日誌等級）存檔後立即套用；只有在環境變數 `RUST_LOG` 已設定時無法套用，這時回報需要重啟。帳號輪替策略與限流冷卻時間存檔後會清掉輪替快取，下一次呼叫就用新設定（之前最多延遲 30 分鐘）。

## 通道管理

- 群組「只回提及」（`mention_only`，每個群組各自設定）：v1.76 起，LINE 群組開啟後只回應用 `@` 叫 AI 員工（或要 @ 名單）的訊息，以及被點名員工正在等的那一則；以前開了會整個群組沒有回應。Telegram 共用 bot 也接受員工名字。見[用 @ 呼叫 AI 員工](../../features/zh-TW/62-calling-ai-staff.md)。
- 網站聊天元件：「開放網站聊天元件」寫 `[webchat] public_widget`，「元件金鑰」寫 `[webchat] widget_key`（16 到 256 個可見 ASCII 字元，有「產生」按鈕；開啟元件時必須有金鑰；金鑰不會在回應中回傳）。立即生效。元件金鑰以明文存在 `config.toml`：它是公開的金鑰，會出現在網站的頁面原始碼裡，所以不當成密鑰處理。
- WhatsApp、飛書、Google Chat、Teams、企業微信、釘釘：六條 webhook 路由一律掛著。通道設定好之前回 404；設定好之後會驗證平台的簽章，簽章錯誤回 401。`channels.add` 不必重啟就會啟動通道，`channels.add` 回 `hot_started` 與 `restart_required: false`；憑證不完整時附 `not_started_reason`。個別員工的 Slack bot 也會在新增時直接啟動。
- 通訊軟體的思考顯示（沒有專屬控制項，在設定檔進階編輯裡設定，每次回覆都重新讀取）：`[thinking] channel_audience` = `internal`（預設：通道身分已驗證、帳號啟用中，且是管理員或綁定回答員工的發話者看得到計時與遮蔽後的摘要，其他人只看計時）、`all`（一對一聊天的所有發話者都看得到摘要）或 `none`（都不顯示）；`[thinking] channel_group_display` = `status`（預設：群組聊天只顯示計時）、`summary` 或 `off`。`[thinking] display` 與回答員工的 `[model] show_thinking` 取較嚴格者，仍是兩者的上限。`config.toml` 讀不到或值不認得時都不顯示。網站訪客（Odoo 線上客服、WebChat 網站小工具）永遠看不到思考。詳見[通訊軟體的思考顯示](../../features/zh-TW/66-thinking-in-messaging-apps.md)。

### WebChat 協定與並行回合

`/ws/chat` 沿用 `auth` 與 `user_message` 訊框。同一條連線的不同 session 可同時執行；同一 session 的訊息依接收順序排隊，跨連線也一樣。session 所有權與員工存取權檢查仍會執行。每個使用者及 gateway 全域上限可在設定檔進階編輯裡調整：

```toml
[webchat]
max_in_flight_turns = 3
max_global_turns = 10
```

`max_in_flight_turns` 每位使用者預設 **3**；`max_global_turns` 整個 gateway 預設 **10**。兩者都允許 **1–64**，執行中、排隊中與斷線後仍在執行的回合都計入。使用者額度跨連線合計，任一額度超限立即回傳 `error`／`turn_limit`；設定值錯誤或設定檔無法讀取時拒絕新回合。連線另有上限：驗證前每個 IP 3 條，dashboard JWT 驗證後每位使用者 3 條，全域 10 條。Widget 訪客在整段連線期間仍占每個 IP 的 3 條額度；重新連線取得新的訪客身分，也不能繞過全域回合上限。

接受新回合時，先原子保留記憶體中的使用者與全域名額，再用共用 broker 在 blocking pool 查詢持久化配額；拒絕時釋放名額，通過後才加入對話 FIFO。每個登入帳號在滾動一小時內最多 **100 個已開始回合的取消紀錄**；Widget 訪客以來源 IP 合計。同一 NAT 後方的訪客共用配額，限制最晚在最後一筆計入的取消滿一小時後解除。重新連線或 gateway 重啟不會歸零。配額只限制該帳號或 IP，全域封鎖紀錄筆數不會拒絕新回合，舊版 NULL principal 不計入任何配額。配額超限或資料庫無法讀取時回 `turn_limit`，以 `request_id` 指出被拒絕的訊息。既有回合仍可取消；從未開始工作的排隊回合取消時不寫封鎖紀錄，也不消耗取消配額。

暫時無法確認登入狀態（`Unavailable`）時，若連線仍可傳送訊框，回傳終端 `error`／`identity_unavailable`，含 `msg_id`、`request_id`、`conv`、`session_id`；只帶固定提示，不含回覆、思考、附件或 session 查詢結果。接受回合前的提示為「暫時無法確認登入狀態，這則訊息沒有送出，請稍後再試。」；交付階段為「暫時無法確認登入狀態，無法交付回覆，請稍後再試。」。原回合憑證無法確認時仍拒絕交付並回滾，包含 writer 最後重查發現的失效；如果只有其中一條接收連線失效，則跳過該連線並保留原回合的歷史。憑證已撤銷時不發這個通知，維持關閉失效連線。

重開最後一則仍是使用者訊息的對話時，儀表板會顯示未回覆提示，前兩分鐘每 5 秒重讀歷史，之後每 30 秒重讀，最多 30 分鐘；收到同 session 的完成訊框會立即重讀。離開、卸載、開始新回合或讀到回覆即停止，自動重讀不會改動草稿。期限過後提示可重新傳送或請管理員查紀錄。送出後 60 秒沒有回合活動（包含斷線重連後），顯示每分鐘更新的溫和等待提示；keepalive 心跳不算回合活動。其他對話失敗只顯示固定通知，不終止目前回合；失敗文字不寫入對話歷史。

用戶端讀取較慢時，進度、思考與步驟更新會合併，只保留最新待送更新。思考文字被拒絕後，清除通知會保留在控制通道，於終態之前清掉先前顯示的文字；每回合最多顯示 64 個思考區塊。終態有獨立保留容量的有界通道，同回合較早的控制訊框仍先於終態傳送。尚未開始寫入就過期的訊框只算該回合未交付，writer 繼續服務其他回合；socket 寫入失敗才關閉。終態訊框的 `progress_dropped` 記錄略過的更新數量。回覆完成後先確認已存入對話歷史，再嘗試即時傳送；傳送逾時、writer 關閉或未收到確認，不再撤回歷史回覆與封存副本。真正的來源撤銷、回合取消及原登入憑證失效，仍保留拒絕交付與回滾機制。

驗證後可送 `{"type":"subscribe","session_id":"<自己的 session>","conv":"<本機對話識別>"}`，讓新連線接收該對話的完成回覆。沿用續聊的擁有權與員工權限檢查；`session_id: null` 清除訂閱，切換訂閱也會使尚在佇列的舊交付失效。只有同一登入使用者、訂閱完全相同 session 的連線能收到回覆。Widget 身分仍逐連線產生，不能接續另一訪客的 session。送出訊息會自動訂閱；舊用戶端最多保留 64 個對話訂閱，超過時移除最舊訂閱，歷史仍可讀取。

使用者訊息會把用戶端 `msg_id` 存為 `client_msg_id`；保存後的 `turn_accepted` 回傳該列 `user_row_id`、實際 `session_id`、`conv` 與 `msg_id`。復原先確認精確的 session 與 `user` 角色，再比對已確認的訊息列 ID；若接收確認遺失，則要求回傳歷史中只有一筆使用者訊息符合該持久化用戶端訊息 ID；識別不唯一時不自動復原。純附件與重複文字也適用，不以內容或前綴猜測；沒有精確匹配時保留本機訊息。重連會沿用伺服器回傳的 session ID，包含 `@` 路由。若從未收到路由後的 session ID，也無法由已知對話確定，自動復原便無法找到該對話，不會猜測 `@` 目標。自己的回合已接受但歷史列尚未建立時也能訂閱；拒絕時回 `subscription_unavailable`，不應清除本機對話畫面。

連線可寫入後，writer 再次確認使用者能否使用實際回答的 AI 員工，接著在短暫的 `BEGIN IMMEDIATE` 交易內比對 ACL 版本並同步呼叫 `start_send`，讓權限修改與訊框交付有明確先後。交易不跨越等待連線或非同步 flush；先獲准交付的資料無法因之後的撤銷收回。助理歷史列預設帶有待確認的隱藏標記，最後權限與來源檢查通過才公開；被拒絕的列即使清除內容失敗、日後重新授權，仍不出現在歷史。單純傳送失敗仍保留已授權回覆。本項修正尚待後端編譯、原生平台與真實 socket 驗收，不能以程式修改視為已通過。

明確登出或切換帳號會先送 `{"type":"logout"}` 再關閉聊天連線。伺服器收到後撤銷該連線發起的在途回合，以及重連後以相同 session 與原本 `conv` 接續的回合，包含等待交付的回覆；其他分頁使用不同 `conv`，不受影響。一般斷線不送此訊框；若網路已中斷，登出通知無法送達，仍依憑證檢查與對話擁有權隔離。

WebChat 與儀表板 RPC 連線原本就由伺服器每 30 秒送出 WebSocket protocol ping，收到 pong 算存活；超過 60 秒無回應時於心跳檢查關閉。protocol pong 不需要網頁 JavaScript 計時器，作業系統休眠仍可能中斷網路。現在分頁回到前景會立即重連、重新訂閱目前對話並讀取歷史；已有回覆就結束等待顯示，尚未回覆則沿用有期限的歷史輪詢。

既有訊框欄位不變。回合訊框增加 `session_id`、`conv` 與 `msg_id`，供用戶端把進度、回覆及錯誤送回原本的回合。每個 `user_message` 應使用不同的 `msg_id`；若以 session 指定回合，使用伺服器回傳的 session id：

```json
{"type":"user_message","content":"你好","conv":"c1","msg_id":"m1"}
{"type":"cancel","conv":"c1","msg_id":"m1"}
```

命令開始執行後不能取消。取消訊框必須指定 `session_id` 或 `conv`，`msg_id` 可省略；省略時選取正在執行的回合。對話指向不明確、其他使用者的 session、找不到回合、已接受取消，或回合已進入回覆交付階段時，回傳 `turn_not_found`，不取消任何回合。控制請求回覆使用 `request_id`，與目標回合的 `msg_id` 分開；`turn_not_found`、`duplicate_message` 不代表目標回合結束。`turn_limit` 以 `request_id` 指出被拒絕的送出訊息。

發出取消的連線會盡力收到帶 `request_id` 的 `cancel_accepted`；不保證它與終態的先後順序。最後結果送到回合原本的連線：`error`／`cancelled` 會攜帶取消請求的同一個 `request_id`，紀錄、核准或交辦佇列清理失敗時回 `cancel_failed`。用戶端無論先收到哪個訊框，都可對應取消請求。排隊中的訊息會等前一回合結束後完成清理。若取消前已有核准通過，`cancelled` 會在 `approved_before_cancel` 列出這些核准 id；先前獲准的動作可能已經執行。

取消會停止該回合的 CLI 行程群組；只在對話已存在時寫入內部取消標記，歷史訊息與壓縮摘要都不讀取它。尚未開始的排隊回合被取消時，不會因此建立新對話。同一回合尚未決定的核准，會與取消封鎖紀錄在同一交易內撤回（`decided_by = system:cancel:webchat_turn`）。封鎖紀錄從取消時間起固定保留 24 小時，不再依核准 TTL 延長，帶回合來源的核准也不再有 30 日 TTL 上限。由持有 gateway lock 的程序共用每小時維護迴圈清理，不依賴儀表板活動；每次最多刪除 1,000 筆；超過 100,000 筆時，也會由最舊開始逐出超過一小時的紀錄並記錄警告。未滿一小時的紀錄絕不逐出，因此筆數可能暫時超過上限。接受新回合時不做 migration 或清理。封鎖紀錄防止終止程序期間競爭插入的晚到核准，並阻擋競爭入列交辦的派送；透過 `setsid` 逃離行程群組的程序屬非沙箱邊界，不在此保證內。對話取消標記保留期限取 session 保留期限與 90 日的較短者，每個 session 最多 1,000 筆；永久刪除對話時一併移除。已完成的動作與已建立的任務不會回復。取消不會繞過資料外送防護：尚未送出或已確認結束的外送照常釋放保護；已送出但結果未確認，或 MCP 行程被停止而未能釋放的保護，仍按既有規則保留最多 6 小時。

取消也會停止同一 session／回合仍在交辦佇列等待的工作，`cancelled` 以 `delegations_cancelled` 回報筆數；交辦佇列清理失敗時回 `cancel_failed`。若還有符合來源的交辦正在執行，同一個 `type: error, code: cancelled` 訊框會再帶上 `downstream_running` 與一次性、不透明的 `downstream_ref`；沒有這類工作時，兩個欄位都省略。用戶端只收到數量，不會收到員工名稱、工作內容或訊息 id。

已登入的用戶端可接著送出 `{"type":"cancel_downstream","downstream_ref":"<識別值>","request_id":"<可省略的 id>"}`。閘道重新檢查目前憑證，在背景執行停止，最多等待 15 秒，以 `type: downstream_stopped` 回傳 `request_id`、`stopped` 與 `still_stopping`；仍在收尾的不算已停止。識別值綁定原使用者與操作身分，10 分鐘後失效；gateway 重啟或有界儲存空間逐出舊項目時也會遺失。未知、他人、過期及已使用的識別值一律回 `downstream_ref_unknown`，不停止工作。憑證查詢失敗回可重試的 `credential_check_unavailable`，憑證遭撤銷則拒絕。Widget 訪客不會收到詢問，也不能使用 `cancel_downstream`。若原回合是因憑證撤銷而取消，則直接要求停止可追溯的交辦，不再詢問。

不傳送訊框就讓執行中的交辦繼續，沒有「拒絕」訊框。既有取消封鎖有效期間，這些執行仍不能再次交辦或新增核准。停止只追蹤相同來源對話與回合，排除歸屬其他人的工作。已執行的外部動作無法追回；本回合建立的任務板任務、目標不會跟著停止；遺失來源資訊的執行也找不到。配額檢查尚未通過、尚未加入 FIFO 的回合不可取消，此時回 `turn_not_found`。[終端控制室](tui.md#聊天與輸入焦點)可在清單焦點回答這項詢問。

主機提供的核准來源若不完整或無效，會拒絕建立核准；只有完全未提供來源時才保留舊版無回合綁定的行為。透過環境變數繼承的核准來源尚未簽章。具有不受限 Bash 的員工可以偽造這些變數；Bash 守門只是減速帶，不能當作隔離。需要實際隔離時，應不授予 Bash 或使用任務沙箱。

關閉連線**不會取消**已接受的回合，排隊中的訊息也會繼續執行至完成。登入儀表板的使用者重新連線後可繼續自己的 session，或取消仍在執行的回合。Widget 訪客重新連線會取得新身分，無法繼續或取消前一條連線的回合；那些回合仍計入全域額度。儀表板會顯示排隊中的訊息與各則的取消按鈕，並提供停止目前前景回覆的按鈕；支援此協定的用戶端也可直接傳送 `cancel`。


## 整合 → Odoo → 連線設定

「允許連到內網的 Odoo」寫入 `[odoo] allow_private_network`（只有管理員能改，預設關閉；隨連線設定由 `odoo.configure` 一起儲存，「測試連線」用畫面上的開關值）。關閉時，Odoo 網址（全域或個別員工）必須是 HTTPS 且指向公開主機（只有 `localhost` 可用 `http://`）。開啟後允許 192.168.x.x、10.x.x.x 這類內網位址，HTTPS 或 HTTP 都可以；本機位址、link-local 與雲端 metadata 位址（169.254.169.254、`metadata.google.internal`）仍一律拒絕，連線會釘在解析時檢查過的位址上，也不走代理。升級後第一次開機時，若已設定的 Odoo 網址解析到內網位址，系統會自動把它打開（稽核事件 `odoo_allow_private_network_auto_enabled`），舊客戶不會斷線。下次連線時生效。

## 整合 → Odoo → 結構快照

「**重新掃描 Odoo 結構**」呼叫 `odoo.schema_scan`，只有管理員可用，RPC 也會檢查相同角色。請先儲存連線設定；掃描使用已存的全域連線，不使用表單尚未儲存的值。畫面顯示上次掃描時間、掃描時伺服器版本、模型／欄位數、是否截斷、最近失敗及差異摘要。版本不同時建議重掃。不修改 Odoo 資料、權限或共享 Wiki。 手動掃描（含失敗）每個已存 profile 每 60 秒最多嘗試一次，重啟不重設；提早重試會顯示還要等幾秒。 兩個 dashboard 動作（`odoo.schema_scan`、`odoo.discover_schema`）共用此限速與掃描鎖。Wiki 流程寫入前會驗證名稱、篩查標籤，明示外部中繼資料只能當作資料參考，並保留技術名稱、欄位類型與關聯。

成功連線後，沒有快照或偵測到版本變更會在背景掃描，不拖延連線。每個有效 profile 每小時最多自動嘗試一次，失敗也計入。同版本的客製變更請用按鈕重新掃描。原本的「解析資料庫」仍是另一個寫入共享 Wiki 的流程。上限及儲存方式詳見[結構掃描](../../features/zh-TW/12-industry-templates.md#結構掃描與欄位相容)。

## 整合 → Odoo → 員工權限

到員工編輯頁勾選可用資料表，並新增動作規則。沒有勾選資料表代表不增加資料表限制；沒有動作規則代表不增加動作限制。Odoo 自己的 ACL、功能模組開關與內建敏感資料表封鎖仍然有效。每條規則選擇一個資料表（或「所有資料表」）及動作：「查詢與讀取」同時儲存 `read`、`search`；「新增」、「修改」、「刪除」、「執行動作」分別儲存 `create`、`write`、`unlink`、`execute`。設定格式不變：所有資料表用單一動詞，指定資料表用 `verb:model`，例如 `read:hr.employee`、`search:hr.employee`。

`hr.employee` 等標示「敏感」的資料表含有敏感資料，選取時需要確認。在資料表清單勾選或在動作規則選取，確認後都會自動加入該員工的封鎖例外；移除最後一個使用處時，會一起移除例外。員工頁不再有獨立的解鎖欄位。`ir.model`、`ir.model.fields`、`ir.attachment` 即使確認後仍然唯讀。全域 Odoo 頁也改用敏感資料表核取清單與相同確認流程；它影響全域連線，員工覆寫的例外清單則會取代全域值。

資料表選項使用已存全域連線的 `odoo.schema_scan` 快照。管理員專用的 `odoo.permission_options` RPC 驗證連線後讀取既有快取（連線最多等 10 秒），回傳資料表名稱、標籤及快取的帳號權限；`odoo.config` 也會回傳後端維護的敏感與唯讀清單。開啟編輯頁會沿用快取；首次連線若沒有帳號權限資料，會排程背景掃描。尚未掃描或連線失敗時可按掃描按鈕，已設定的資料表與敏感資料表仍會保留在畫面上。若員工連到其他資料庫，也能保留全域掃描未列出的既有資料表名稱。

可用動作來自連線所用 Odoo 帳號本身的讀取、新增、修改與刪除權限，於連線或結構掃描時探測，並連同時間戳記快取。若在 Odoo 調整帳號權限，請按「重新掃描」。執行動作以修改權限作為可用提示，實際能執行哪些按鈕由 Odoo 判斷，資料列規則仍由 Odoo 檢查。沒有權限的動作無法勾選；無法確認的動作保留可選並顯示提示。已儲存但無權限的動作保留勾選與警告，可取消勾選移除。「所有資料表」不限制勾選，因各表實際權限不同；DuDuClaw 禁止讀取的資料表不會送出探測。 動作可用性使用已儲存的全域連線；若員工使用獨立連線、憑證或公司範圍，編輯器會顯示無法確認，不會套用其他帳號的拒絕結果。

舊規則依資料表分組。只有 `read` 或 `search` 的舊設定顯示「查詢與讀取／僅部分」，儲存時補成一對。既有 `action` 規則（執行時對應 `action_*`、`button_*`、`toggle_*` 方法）與不認得的項目列在唯讀的「其他設定」，可逐項移除；未移除就原樣保留。伺服器只允許保留已存在的舊值，新增的不合法規則仍會拒絕。Odoo 總覽以「資料表：動作、動作」呈現每條規則。

## 整合 → Odoo → 網站線上客服

用自己的儲存按鈕透過 `odoo.livechat.set`（管理員）寫入 `[odoo_livechat]`。它和 `[odoo]` 分開，因為儲存 Odoo 連線設定時會整段改寫 `[odoo]`。機器人 API 金鑰加密存成 `bot_api_key_enc`，不會回傳；欄位留空表示不變。每次儲存與每次「交回 AI」都寫入稽核紀錄。詳見 [features/zh-TW/61-odoo-livechat.md](../../features/zh-TW/61-odoo-livechat.md)。

| 控制項 | 設定鍵 | 生效時機 |
|---|---|---|
| 啟用網站線上客服 | `enabled` | 幾秒內（每次輪詢都重讀設定；關閉時機器人在 Odoo 改為離線） |
| 機器人帳號／API 金鑰 | `bot_username`、`bot_api_key_enc` | 下一次輪詢（重新連線） |
| 預設 AI 員工、開場白、檢查間隔 | `default_agent`、`greeting`、`poll_interval_seconds`（2～60） | 下一次輪詢 |
| 服務的線上客服頻道 | `livechat_channel_ids` | 下一次輪詢 |
| 轉接真人 | `handoff_user_ids`、`handoff_keywords`、`handoff_message` | 下一次輪詢 |
| 分眾規則 | `[[odoo_livechat.audience_rules]]`（`name`、`agent`、`enabled`、`livechat_channel_ids`、`chatbot_script_ids`、`country_codes`、`langs`、`visitor`、`partner_tags`、`keywords`）；指定的 AI 員工必須存在，否則拒絕儲存 | 新對話（已分派的對話維持原本的 AI 員工） |

## 本地推理

存檔（`inference.update`，管理員）成功後 gateway 會重設推理引擎，下一則回覆就用新設定，不必重啟。

- 信心路由 (Router) → 進階：`[router] local_tools`、`ucci_fast_router`、`ucci_strong_router`、`ucci_observations`、`ucci_shadow_strong`、`ucci_shadow_max_inflight`（1 到 16）、`ucci_drop_stop_token`；`[generation] capture_logprobs`、`capture_top_logprobs`。
- llamafile 本機伺服器：`[llamafile] enabled`、`dir`、`default_file`、`host`、`port`、`gpu_layers`、`context_size`、`extra_args`。已知限制：在畫面上清空欄位不會刪除已存的值，要清除請用設定檔進階編輯。

## AI 員工編輯頁

這頁用 `agents.update` 寫入該員工的 `agent.toml`，只送出有改動的欄位，所以存檔不會再把心跳排程（`[heartbeat] cron`）清空。

| 分頁 → 區塊 | 設定鍵 |
|---|---|
| 預算 → 每日上限 | `[budget] daily_cap_cents` |
| 預算 → 費用資料讀不到時 | `[budget] when_unavailable`（`allow` / `deny`） |
| 技能 → 這位員工的技能（v1.76.0） | 不是設定鍵：開關存在 `<home>/skill_state/<agent_id>.json`（`skills.set_enabled`），移除的技能搬到 `<home>/skill_archive/<agent_id>/`（`skills.remove`／`skills.restore`）；需員工負責人或管理員。見[技能庫頁面](../../features/zh-TW/15-skill-lifecycle.md) |
| 工具與權限 → 需要把關的工具（一律等人核可、視為不可逆、先由判官判斷、需任務授權） | `[capabilities] approval_required_tools`、`irreversible_tools`、`maybe_irreversible_tools`、`scoped_tools` |
| 工具與權限 → 平行分支 | `[fork] enabled` |
| 工具與權限 → 出站內容防護 | `[guardrails] enabled`、`block_secrets`、`block_injection_echo`、`redact_pii`、`deny_phrases` |
| 工具與權限 → 權限 | `[permissions]` 四個旗標（見下方） |
| 腦袋與引擎 → 推理力度 | `[model] effort` |
| 腦袋與引擎 → AI 後端／備援後端 | `[runtime] provider`／`fallback`，新增 qwen、kimi、copilot、kiro、cursor、vibe、opencode |
| 腦袋與引擎 → 精簡啟動內容 | `[runtime] minimal_context` |
| 腦袋與引擎 → 單次執行時間上限 | `[runtime] timeout_secs`：整數秒，預設 300，範圍 30～3600；適用 Codex／Grok／其他 CLI／OpenAI 相容 API；Claude 另有固定 30 分鐘上限 |
| 腦袋與引擎 → 輔助模型 | `[model] utility`：Codex 與 OpenAI 相容 API 留白時沿用主要模型；Claude 預設 `claude-haiku-4-5`；Grok 與通用 CLI 改用全域背景設定 |
| 腦袋與引擎 → 一員工四角色 | `[team] enabled`、`[team.roles.<角色>]` |
| 自動化 → 夜間整理 | `[night_engine] enabled` |
| 整合 → 專屬通道（v1.76.0） | `[channels.telegram] bot_token_enc`、`[channels.googlechat] project_number`／`service_account_json_enc`，經 `agents.channels.*` 設定（管理員設定，負責人看得到狀態；見[專屬通道](per-employee-channels.md)） |
| 通道與整合 → 讓外部聯絡人用 @ 呼叫（v1.76） | `[channels] public_trigger`（只限管理員；見[用 @ 呼叫 AI 員工](../../features/zh-TW/62-calling-ai-staff.md)） |
| 自動化 → 記憶 → 決策延續、決定保留天數 | `[memory] decision_continuity`、`decision_ttl_days`（1 到 3650） |
| 進階 → 模型進階 → 進階鍵值 | 任意 `[區段] 鍵`，每列帶型別（字串、整數、小數、布林、字串陣列）；寫入前用完整的 `AgentConfig` 解析，失敗整筆拒絕。`agent`、`capabilities`、`container`、`permissions`、`channels`、`odoo`、`mcp`、`runtime`、`prompt`、`os_watch`、`budget` 區段不能在這裡改（v1.76.0：新增 `prompt`、`os_watch`、`budget`，裡面有只限管理員的設定，例如 `prompt.cli_bare_mode`，管理員改用設定檔進階編輯修改） |

**員工預算。** `[budget] daily_cap_cents` 與 `monthly_limit_cents` 分別比對最近 24 小時與 30 天的已記錄花費，`hard_stop = true` 時達上限就拒絕下一次模型呼叫；0 代表不限額，執行中的呼叫仍可能超過上限。檢查涵蓋 Claude 與其他員工後端、派工與排程、提醒、郵件、心跳、輔助工作及 CLI 分支／評測。短期員工的已記錄花費也算進父員工，先以千分之一美分加總，再換算美分。員工設定檔可讀時，`[budget] when_unavailable = "allow"`（未填時的預設值）會在費用資料庫或查詢讀不到時留下明確警告與稽核紀錄並繼續；設成 `"deny"` 則在模型呼叫前拒絕。設定檔本身讀不到或無法解析時，無法確認這項政策，一律拒絕。未知政策值按 `"deny"` 處理並警告。無法檢查會顯示自己的原因與稽核事件，不會說成「花費達上限」，也不會發出斷路器開關通知。預算拒絕不觸發帳號輪替或備援。沒有員工 id 的呼叫不適用此政策。 指定員工目錄的輔助呼叫，以有效設定的 `[agent].name` 檢查預算並記錄後續花費，即使呼叫端原本使用作業名稱。舊有只記在作業名稱下的費用，無法可靠回填到各員工。

單次執行時間上限與容器任務的逾時設定分開儲存。伺服器會拒絕非整數或超出 30～3600 秒的值；手動編輯檔案時，超出範圍的整數會調整到界線，型別錯誤則用預設值，兩者都會記錄警告。OpenAI 相容 API 員工需要在 `utility` 或 `preferred` 指定模型；Codex 可使用 CLI 預設模型；Grok 與通用 CLI 員工改用全域背景設定。輔助模型與 runtime 的廠商家族不符時，畫面會提醒對話壓縮、事實擷取等背景工作將失敗。

提高 `timeout_secs` 的實際上限需要管理員。員工擁有者可降低上限；清空設定會還原為 300 秒，若因此高於目前上限，同樣需要管理員。

Claude 輔助呼叫使用 CLI 可用憑證走輕量路徑，不進入帳號輪替；一般對話仍保留原有帳號輪替。

輔助呼叫（含分類與評審）使用專用執行模式。Claude 使用無工具、單回合的輕量路徑；OpenAI 相容 API 只送出一次不帶工具的請求，不進入工具迴圈。Codex 使用受限模式：唯讀沙箱、不注入平台 MCP 設定、每次建立只有擁有者能存取的空白暫存工作目錄、忽略使用者設定，且不保存工作階段。呼叫結束後刪除暫存目錄，包含錯誤與逾時；系統提示放進 prompt，不寫入 `AGENTS.md`。明確指定 Codex 的 judge 與團隊 verifier 也使用此模式。Grok 與通用 CLI 仍不支援輔助執行，員工背景工作改用全域輔助後端／模型（預設 Claude／`claude-haiku-4-5`），每個員工／後端組合記錄一次警告。全域後端必須是 Claude、Codex 或 OpenAI 相容 API；不支援的全域後端或明確指定會被拒絕。明確指定的 Codex 判官遭拒時直接使驗收失敗，不以預設判官重試；其他後端維持既有降級策略。Codex 可能保留內建讀取工具，剩餘風險見[安全防禦](../../features/zh-TW/05-security-defense.md)。

**只限管理員的欄位。** `[agent] reports_to`、`department`、`name`，整個 `[capabilities]`，`[container] sandbox_enabled`、`network_access`、`readonly_project`，`[permissions]` 的五個旗標（`can_modify_own_soul`、`can_create_agents`、`can_send_cross_agent`、`can_schedule_tasks`、`can_modify_own_skills`）與（v1.76.0）`[permissions] allowed_channels`，`[guardrails] enabled` 與 `block_secrets`，`[channels] public_trigger`（v1.76），以及（v1.76.0）各員工自己的 bot token `[channels.discord]`、`[channels.telegram]`、`[channels.slack]`、`[channels.googlechat]`、整個 `[odoo]`、`[runtime] provider` 與 `fallback`、`[prompt] cli_bare_mode`、整個 `[os_watch]`、`[model] account_pool`、設成或取消 `role = "main"`，還有放寬預算：調高 `monthly_limit_cents` 或 `daily_cap_cents`、拿掉上限（0 代表不限）、關閉 `hard_stop`，或把 `when_unavailable` 從 `deny` 改成 `allow`。員工的負責人仍可以新增或調低上限、打開 `hard_stop`。非管理員送出改動會被拒絕，管理員改動成功後寫稽核事件 `agent_authority_changed`，被拒絕的嘗試寫 `agent_authority_refused`。換模型會自動切換執行環境時，非管理員會看到說明。`org.toml` 只在 `reports_to` 或 `department` 真的變了才更新。`agents.inspect` 以 `admin_only_fields` 回傳對應的表單欄位，編輯頁對非管理員停用這些欄位。

**誰能存檔（v1.76.0，行為變更）。** `agents.update` 每次存檔都從帳號資料庫重新讀取呼叫者的角色與員工綁定，寫入前在檔案鎖內再讀一次。解除綁定、停用帳號或要求改密碼，在對方下一次存檔就生效，不用等登入過期。任何途徑寫 `agent.toml`（這頁、聊天裡的 `/model`、設定檔進階編輯、系統自動修改）都會保留註解與鍵的順序、留下版本（見[設定檔](#設定檔v1760)），並寫 `agent_file_changed`。存檔後無法重新載入員工清單時，訊息改為「已儲存，但暫時無法立即套用；請稍後重新整理，仍未生效請重新啟動服務」（系統沒有定期重新載入）。

**權限旗標開始生效（行為變更）。** `can_create_agents`、`can_send_cross_agent`、`can_modify_own_skills`、`can_schedule_tasks` 以前沒有任何讀取端。v1.68.0 起，寫成 `false` 的旗標會在 MCP 分派閘擋下對應工具（`create_agent`；`send_to_agent`、`spawn_agent`；`create_reminder`、帶 `schedule` 的 `tasks_create`；`skill_hub_install`、`shared_skill_adopt`、`skill_graduate`、`skill_pin`、`skill_from_recording`），沒寫或型別錯誤時放行。因為舊範本常把這些旗標寫成 `false`，gateway 升級後第一次開機會對每位員工做一次遷移：`[permissions]` 裡沒有 `permissions_enforced_since` 標記的檔案，四個旗標中的 `false` 一律改成 `true`，並加上 `permissions_enforced_since = "1.68.0"`，每次重設寫稽核事件 `permission_flags_reset`。之後在這頁或設定檔進階編輯寫入的 `false` 才算操作者的決定。臨時角色成員（`agents/.ephemeral/`）不在遷移範圍內，維持最小權限。

## 管理 → 成員：直屬主管與組織圖（企業版，1.76.0）

只限管理員。個人版沒有這一頁。

| 控制項 | 寫入 | 效果 |
|---|---|---|
| 新增／編輯成員 → 直屬主管 | `users.create`／`users.update` 的 `manager_user_id`（選「未指定」＝清除） | 存檔後下一則訊息與下一次 dashboard 操作就照新的主管關係判斷。下拉選單只列在職、角色是主管或管理員的人，不含本人和他底下的人；伺服器仍會再檢查一次，拒絕時以白話說明原因：會繞成一圈、超過 8 層、主管角色不足，或（把有部屬的人改成一般員工時）還有直屬部屬。 |
| 清單 → 角色欄 | — | 顯示「主管：○○」。 |
| 清單／組織圖 | — | 只改變顯示方式。組織圖依直屬主管畫出上下層，每個人旁邊列出他綁定的 AI 員工；沒有主管也沒有部屬的人放在「未指定主管」。離職的人不顯示，停權的人照常顯示並標示。點名字可以編輯。 |

主管能從部屬那裡拿到什麼，見[用 @ 呼叫 AI 員工](../../features/zh-TW/62-calling-ai-staff.md#主管叫得到團隊的員工)。

## 組織 → 我的團隊（企業版，1.76.0）

角色是主管或管理員的人才看得到（側欄「組織」）。頁面列出向你回報的人（往下最多 8 層，停權的人照列，離職的不列），以及透過每個人你叫得到的 AI 員工：

- 標籤「可交辦」或「只能對話」：部屬的綁定是 Owner 或 Operator 時，你拿到的是 Operator，可以交辦；是 Viewer 時只能對話。
- 「進行中 N 件」：指派給這位員工、還沒完成、取消或失敗的任務。
- **對話**：開啟 WebChat，輸入框先填好 `@名字 `。員工停用時不能按。
- **新對話**：取代原本的交辦按鈕，開啟新的網頁對話，不建立任務。

資料來自 RPC `users.my_team`，和每一次呼叫、交辦用的是同一套判斷，所以列出來的都叫得到。個人版與一般員工呼叫這個 RPC 會被拒絕。

## AI 員工 → 暫停／恢復

暫停或恢復員工（`agents.pause`／`agents.resume`，參數 `agent_id`）須同時具備 Manager 角色及該員工的直接 Operator 或 Owner 綁定；管理員維持完整存取權限。寫入前會從 `users.db` 重新讀取角色與綁定，缺少或找不到員工編號、權限資料無法讀取時一律拒絕。主管經由部屬取得的權限不包含這項操作。

## 員工頁 → 資料來源（v1.76.0）

綁定這位員工的人（檢視者以上）都看得到；新增與移除檔案需要這位員工的擁有者綁定或管理員身分（每次呼叫都重新從帳號資料庫讀取），而且還要是員工專用集合的編輯者或擁有者。

| 操作 | 寫入 | 生效 |
|---|---|---|
| 新增檔案（第一次）：敏感度、「也讓目前和這位員工一起工作的同事讀取」 | `knowledge.db` 裡新的集合 `<顯示名稱> 的資料`、它的授權，以及員工設定裡既有的 `[knowledge] collections` 清單 | 立即。授權：建立的人（擁有者）、這位員工（檢視者），勾選時再加上所有綁定這位員工的有效帳號（擁有者 → 擁有者，操作者／檢視者 → 檢視者）。 |
| 新增檔案 | 員工專用集合裡的一份來源 | 背景讀取；還有檔案在讀取時，清單每 3 秒更新。 |
| 移除（可勾選「也請這位員工忘記」） | 刪除來源；勾選時送出遺忘申請給管理員 | 立即查不到；遺忘申請要等核准。 |
| 在文件知識庫管理 | — | 打開這個集合（權限、試搜尋、存取紀錄、設定）。 |

封存或刪除員工時，專用集合一起封存（只有身為擁有者的同事保留存取）；恢復員工時，放回封存時移除的授權。第一次上傳也會把集合加進既有的 `[knowledge] collections` 清單，之後的上傳不再動這份清單。細節見[文件知識庫](../../features/zh-TW/63-knowledge-base.md#在儀表板操作)。

## 員工頁 → 連線（v1.76.0）

綁定這位員工的人（檢視者以上）都看得到它用哪個 Google 帳號、現在能不能用（`agents.google_identity_get`）。變更與「測試連線」只有管理員能做。

| 控制項 | 寫入 | 生效 |
|---|---|---|
| 變更 → 公司共用帳號／代表某位同事（網域委派）／不使用 | `agent.toml [google] identity` = `home`／`delegate:<email>`／`none`（`agents.google_identity_set`） | 下一次 Google 工具呼叫。`none` 會讓這位員工看不到 19 個 Google 工具（工具清單會變）。信箱必須在允許的網域內。 |
| 「萬一這位同事的帳號用不了，改用公司共用帳號」 | `[google] fallback = "home"`（不勾就不寫這個欄位） | 只涵蓋：還沒匯入金鑰、同事停權或離職、Google 拒絕或連不上。發問者沒有權限、設定錯誤、憑證保護的問題一律不改用。 |
| 測試連線 | 不寫入 | 替同事取權杖（先完整清單，再逐一服務），確認 Gmail 回報同一個信箱。不寄任何東西。 |

`[google] requesters`（預設 `bound_people`，另有 `owner_only`、`anyone`）這一頁還沒有控制項，用 `agents.google_identity_set`（`anyone` 要加 `confirm_anyone: true`）或設定檔進階編輯（管理員）設定。非管理員的擁有者改 `agent.toml` 時，`[google]` 只能改得更窄（改成 `none`、拿掉 `fallback`、收緊 `requesters`）。細節見[每位 AI 員工用不同的 Google 帳號](google-workspace.md#每位-ai-員工用不同的-google-帳號公司授權代表v1760)。

## 整合 → Google → 公司授權代表（v1.76.0）

只有管理員，兩個版本都有。

| 控制項 | 寫入 | 生效 |
|---|---|---|
| 匯入金鑰檔／換一個金鑰檔 | `<home>/protected/google-service-account.sealed`，用憑證保護金鑰封存（`google.delegation.import_key`） | 立即。「安全 → 憑證保護」還沒改放系統金鑰圈時會被拒。金鑰不會傳回瀏覽器；面板顯示服務帳號、用戶端 ID、權限清單與管理控制台路徑。 |
| 移除 | 刪除封存的金鑰（`google.delegation.remove_key`，要確認） | 所有代表同事的員工停止使用 Google（允許改用公司帳號的會改用）。 |
| 允許代表的網域 | `config.toml [integrations.google_identity] allowed_subject_domains`（`google.delegation.settings_set`） | 下一次呼叫。網域完全相同才算。空白時只允許 `[integrations.google_service_account] subject` 的網域，沒有就一律不行。 |
| （無控制項）`default_identity` | `[integrations.google_identity] default_identity` = `home`（預設）／`none` | 沒寫 `[google]` 的員工用什麼。 |

最下方列出每位代表同事的員工，以及目前不能用的原因。

## 系統設定 → 進階設定 → 文件知識庫搜尋

只限管理員（1.76.0 新增）。顯示文件知識庫目前是依語意搜尋還是只比對關鍵字；只比對關鍵字時會說明原因，並顯示已有向量的段落比例。

| 控制項 | 寫入 | 效果 |
|---|---|---|
| 計算方式：本機模型／OpenAI／Google Gemini／關閉 | `[knowledge.embedding] provider`（雲端另加 `cloud_model`、`cloud_account`） | 下一次搜尋生效。選雲端供應商會開啟對話框，寫明會收到段落的供應商與主機；勾選確認框之前，**改用 …** 按鈕維持停用，確認結果存成 `cloud_disclosure_ack`。去識別化沒有啟用時，對話框會提醒（此時什麼都不會送出）。改選其他方式會移除這份確認。 |
| 安裝（大小） | 下載到 `models/embedding/<model>/` 與 `lib/onnxruntime/` | 背景下載並顯示進度，按**取消**後可以續傳；完成後替每一段排入建立向量的工作。 |
| 重新檢查（離線安裝） | — | 先用發行金鑰驗證離線安裝包的簽章（`.tar` 加 `.tar.minisig`，不要解壓，放在 `models/embedding/`），通過才解開、逐檔比對雜湊值，再標示為已安裝。 |
| 改用這個模型 | `local_model` | 切換本機模型；背景會替每一段建立新模型的向量（前一個模型的向量會保留，但不會拿來比對）。 |
| 移除 | 刪除模型檔 | 搜尋改為只比對關鍵字並顯示原因；執行環境函式庫保留。 |
| 補齊尚未建立的向量 | — | 替所有還缺向量的來源排入處理。 |

詳見[文件知識庫](../../features/zh-TW/63-knowledge-base.md#向量模型)。

## 設定檔（v1.76.0）

RPC `agent_files.list / get / diff / history / save / rollback / withdraw / pending_get`；員工編輯頁的「行為界線」也走同一套（`contract.update`）。檔案：人格設定（`SOUL.md`）、自我介紹（`IDENTITY.md`）、服務對象（`USER.md`，key `user`，最多 4,000 個 Unicode 字元）、工作規則（`INSTRUCTIONS.md`）、行為界線（`CONTRACT.toml`）、基本設定（`agent.toml`，這裡只能看，機密遮蔽；要改請到編輯頁，管理員也可用設定檔進階編輯），以及這位員工自己的技能（`SKILLS/` 底下已經存在的 `.md`，檔案代號 `skill:<路徑>`，例如 `skill:quote/SKILL.md`）。技能在這裡只能修改，不能新增或刪除（新增請用自建技能流程），旁邊的程式檔不會被碰到；路徑每一層都必須是單一名稱、不能有 `..`、不能有看不見的字元（零寬、方向控制等）、最多四層；捷徑（symlink）與硬連結一律拒絕，存檔時在檔案鎖內確認資料夾沒有被換掉才寫入（鎖檔放在員工資料夾外）。其他寫技能檔的途徑（自建技能、員工自己的技能工具、技能畢業）不經過這裡，沒有版本紀錄，也不拿同一把鎖。上限：Markdown 與技能每檔 64 KB，`CONTRACT.toml` 32 KB，`must_not` 與 `must_always` 各最多 100 條、每條最多 500 字。

- **設定檔分頁：** 員工詳情頁的「設定檔」分頁（`/agents/<員工>/files`）。左邊是白話的檔案清單，小字灰色是實際檔名，標記「等待審核」「在系統外被改過」「你只能檢視」；右邊先以唯讀方式顯示，負責人與管理員按「編輯」後可以修改，下方顯示大小與上限，按鈕有「檢視差異」「儲存並檢查」「捨棄修改」。檢查結果用同一個確認對話框。存檔時若別人剛存過，會並排顯示你的內容與目前內容，可以「複製我的內容」再「重新載入」。「版本紀錄」（Operator 以上）列出時間、修改人與來源，可以檢視、和目前內容比較或還原。還沒存的修改在切換檔案、切換分頁或關閉頁面前會先詢問（從側邊欄等其他連結離開時目前不會詢問）。只能檢視的人會看到「你可以檢視；只有這位員工的負責人或管理員可以修改。」
- **工作規則與 AGENTS.md／Agent.md：** 清單與標題把「工作規則」寫成「工作規則（AGENTS.md／Agent.md）」，因為很多人用業界通用的名字找它；實際檔名仍是小字的 `INSTRUCTIONS.md`。匯入功能把 AGENTS.md、CLAUDE.md、GEMINI.md、Agent.md、copilot-instructions 的內容合併進這份檔案，每次對話都會交給員工，不論它用哪一種執行環境。新員工沒有這個檔案時，分頁顯示「建立工作規則」（只有負責人與管理員看得到按鈕），按下去帶出一段短範本，按「儲存並檢查」才會寫入，走的是同一條存檔、檢查與送審流程，沒有另開寫入路徑。
- **執行環境自動產生的檔案（唯讀）：** 分頁最下方收合的區塊，第一次展開才讀取，顯示建立員工時替它的執行環境產生的 `CLAUDE.md` 與 `AGENTS.md`（RPC `agent_files.runtime_files`，Viewer 以上；只列這兩個固定檔名，沿用員工資料夾的檢查，捷徑與非一般檔案一律不讀，每檔最多顯示 64 KB）。這裡不能修改，要改規則請改工作規則。
- **搜尋：** 命令面板（⌘K）輸入 `agent.md`、`agents.md`、`CLAUDE.md` 或「工作規則」，每位員工會有一列「<員工> · 設定檔」，直接開啟該員工的設定檔分頁。
- **誰能做什麼：** 綁定這位員工的人都能看檔案；Operator 以上能看版本與差異；員工的負責人與管理員能存檔。每次呼叫都重讀帳號資料庫，不看登入時的快照。審核只有管理員能決定。
- **每次存檔都檢查：** 先跑固定規則（大小、控制字元、嚴格解析 `CONTRACT.toml`、隱藏或看不見的內容、這次新增的文字或新的 `must_always` 裡的注入句型、像金鑰的文字、人格設定被刪掉大半、行為界線刪改 `must_not`／`must_always` 或調高／取消工具次數上限；技能另外檢查：開頭的 `---` 設定區要能照技能載入器的格式讀出，`name` 必須是單一名稱，技能掃描器在這次新增的行上找到的執行指令、外送資料、注入句型會升級為高風險（以實際新增的行判斷，同類的第二行照樣算新的），裸露的網址與提到行為界線禁止的事項只標為需要注意；原本就在檔案裡的高風險內容會列為「這個技能裡原本就有可疑內容」；技能在上次儲存後被系統外改過時整份重新檢查，並和上次在這裡存的版本比較），再請部署的輔助模型看一次差異（`[runtime] utility_provider`／`utility_model`，可用 `[agent_files] review_provider`／`review_model` 指定；費用記在部署的 `agent-file-review`，不扣員工預算）。模型只能讓結果更嚴。結果：`pass` 直接套用；`warn` 要寫理由（至少 4 個字）或送管理員審核；`block` 非管理員只能送審，管理員要寫理由並勾「我了解風險」才能套用（稽核記為 Critical）。檢查沒完成時（沒有輔助模型、逾時、回答無法使用、額度用完），或內容太長只檢查了一部分，結果至少是 `warn`（設 `[agent_files] on_analysis_failure = "block"` 則是 `block`），非管理員只能送管理員審核，管理員要勾「我了解風險」才能套用；不會默默放行，也不能關掉。每天的額度：整個部署 200 次、每人 40 次、每位員工 60 次（`review_daily_limit`、`review_daily_limit_per_user`、`review_daily_limit_per_agent`），另外每人每分鐘最多檢查 10 次（`checks_per_minute`）；同一份修改 15 分鐘內再檢查沿用上次結果。確認時不再跑一次模型；確認必須是同一人、同一檔、同一個起始版本與同樣內容，並在 15 分鐘內，固定規則用你現在的權限重跑（比檢查時嚴就要重新檢查），確認高風險的修改時會在檔案鎖內再確認一次你仍是管理員。在系統外被改過的 SOUL.md、IDENTITY.md、USER.md 或 INSTRUCTIONS.md 會整份重新檢查，並和上次核可的版本比較（原因「在系統外被改過」）。
- **存檔：** 在檔案鎖內比對編輯器讀到的雜湊（`base_hash`），期間被別人改過就當作衝突拒絕。新內容從員工的下一則對話開始生效，通道的下一則回覆會附一行提示。
- **版本：** 存在員工資料夾外的 `<home>/agent_file_history/<員工>/<檔案>/`（只有擁有者能讀），每檔預設保留 20 份（`[agent_files] history_keep`，5–100）。第一次修改也會留下修改前的內容。還原版本會跑同樣的檢查。刪除員工時版本整份移到 `agent_file_history/_trash/`，之後同一個代號的新員工從零開始。
- **核可基準：** 存 SOUL.md、IDENTITY.md、USER.md 或 INSTRUCTIONS.md 會更新核可指紋（INSTRUCTIONS.md 現在也有，heartbeat 以 `_instructions_integrity_drift` 回報）。在系統外被改過的檔案會標示出來，可以還原到上次核可版本，或檢查後接受目前內容（照原樣再存一次：整份重新檢查，至少要寫理由確認或送審，確認後不改寫檔案，只把目前內容記成一個版本並設為核可基準；沒有被改過的檔案照原樣存一次仍然是「沒有變動」）。員工改名會同步 SOUL.md／IDENTITY.md 裡的名字，原本核可的 SOUL 維持核可，原本已在系統外被改過的 SOUL 仍然標示。名字必須是純文字（1–64 字，不能換行、不能有看不見的字元或 `< > [ ] { }`、反引號），改名後的內容若被固定規則標出問題就不同步（列在 `identity_files_skipped`）。
- **送審：** 種類 `agent_file_change`，只能由目前仍是管理員的人在儀表板決定，期限 3 天（逾期視為拒絕）。通道只收通知、沒有按鈕。提出的內容存在 `<home>/agent_file_pending/`，申請結束就刪除。核准時先套用；申請期間檔案被改過，申請會被拒絕，需要申請者重新送出。
- **稽核：** `agent_file_changed`、`agent_file_change_refused`、`agent_file_review_failed`、`agent_file_change_requested`（只記雜湊，不記內容）。

- **匯出設定檔：** Manager 與管理員可從有權檢視的員工頁選單選擇「匯出設定檔」，支援 DuDuClaw、OpenClaw、Hermes、AGENTS.md 與 Claude Code subagent。服務對象介紹預設不包含，要明確勾選。OpenClaw／Hermes 請逐一下載並放入同一個資料夾。秘密、通道、能力、權限、帳號池與記憶不會匯出。見 [Agent File 參考](../../spec/zh-TW/agent-file-spec.md)。
- **服務對象：** 使用相同的鎖定、版本與審查流程儲存。提示詞以 `<user_profile>` 資料區塊載入，明確註明不是指令，並經過與人格相同的輸入檢查。最多 4,000 個字，優先於一般 Markdown 的位元組上限。

## 排程交辦（v1.76.0）

可透過聊天交辦或 RPC 建立一次性或週期排程，管理員另可用自訂 cron。儀表板的建立表單已隱藏。設定當下不建立任務，時間到才重新確認建立者權限並交辦。

直接開啟 `/goals` 的「我交辦的 → 排程」可暫停、恢復與刪除，管理員可切換看所有人的排程。暫停或刪除先於任務落地，就不會建立該輪任務；已落地的任務需另外停止。恢復會重驗建立者權限與目前執行間隔限制。

設定檔 `[goal_defaults] assign_schedule_max_active` 預設每人 20 個（0–200）；`assign_schedule_min_interval_minutes` 預設非管理員最短 60 分鐘，只能調高，既有排程下次執行也會檢查。詳見[排程交辦](../../features/zh-TW/67-assigning-tasks.md)。

## 系統 → 系統更新（v1.76.2）

這一頁照同一個順序更新整套安裝。桌面 App 連到不是自己啟動的 gateway 時（launchd／systemd／企業版 gateway，或遠端 gateway），頁面會列出兩個元件：gateway 的目前版本與要更新到的版本，桌面 App 的目前版本與已下載版本。一個按鈕先更新 gateway，等它重新上線，再更新 App。App 只會更新到 gateway 的版本；App 下載到較新的版本時會先等著，頁面會說明原因。App 自己帶 gateway sidecar 時，兩者照舊一起更新。

磁碟上已經有較新的 gateway binary 但還沒執行時（npm 安裝、手動換檔、更新後沒重新啟動），按鈕會顯示「重新啟動 gateway 套用 v…」，呼叫 `system.restart_pending_update`（管理員）。gateway 只有在磁碟版本確實不同於執行中版本時才接受；容器與桌面 sidecar 由各自的擁有者重新啟動，這裡會拒絕。重開桌面 App 不會重啟外部 gateway。

## 思考過程顯示

目前沒有專用控制項，請在下方的設定檔進階編輯修改。兩個檔案每輪都重新讀取，改完下一則回覆就生效。

| 檔案 | 設定鍵 | 值 |
|---|---|---|
| `config.toml` | `[thinking] display` | `summary`（預設：WebChat 顯示計時、token 與遮蔽後的思考文字）、`status`（只有計時與 token）、`off`（不送思考訊框；WebChat 自己的計時仍在） |
| `config.toml` | `[thinking] max_chars` | 每輪思考文字上限，預設 4000，限制在 200–20000 |
| `config.toml` | `[thinking] anthropic_display` | `summarized`（預設）或 `updates`（beta，只有部分 Claude 模型支援） |
| `agent.toml` | `[model] show_thinking` | `summary`／`status`／`off`；不設定就跟隨 `[thinking] display`。兩者取較嚴格的 |

`config.toml` 讀不到、解析失敗，或 `display` 值看不懂，都當成 `status`。思考文字只給登入 dashboard 的 WebChat 使用者，以及通訊軟體上 `[thinking] channel_audience` 允許的發話者（見「通道管理」段）。細節見[思考過程顯示](../../features/zh-TW/64-thinking-display.md)。

## 設定檔進階編輯

位置：系統設定 → 進階設定 → 設定檔進階編輯。只有管理員看得到，RPC 是 `config.raw.get` / `config.raw.set`。

- **可編輯的檔案：**系統設定（`config.toml`）、推理設定（`inference.toml`），以及每位 AI 員工的 `agent.toml`（必須是 `agents/` 底下的實際目錄，不接受符號連結）。
- **密鑰遮罩：**鍵名以 `_enc` 結尾、名為 `key` 或以 `_key` 結尾、或含 token、secret、password、passwd、api_key、apikey、widget_key、private_key、credential、service_account_json 的值，以及 `headers`、`otlp_headers`、`env` 表格裡的每個值、帶密碼的網址，都顯示成 `«set»`。存檔時原樣保留 `«set»` 就保留原值；原本沒有值卻寫 `«set»` 會被拒絕。`[mcp_keys]` 整段不顯示。
- **驗證：**先檢查 TOML 語法，錯誤會指出第幾行第幾欄；再用對應的型別（系統設定的各區段驗證、`InferenceConfig`、`AgentConfig`）檢查，失敗就不寫入。不能在這裡改員工的 `[agent] name`。
- **文件知識庫：**集合、文件、分享與存取紀錄在側邊欄「公司 → 文件知識庫」管理；`[knowledge]` 這些全域上限還沒有設定頁，它的各個鍵（`enabled`、`max_source_bytes`、`max_source_chars`、`max_sources_per_collection`、`max_collections`、`max_chunks_total`、`extract_timeout_secs`、`extract_memory_mb`、`access_log_retention_days`、`max_collection_bytes`、`max_total_bytes`，`extract_when_unavailable` = `refuse`／`run_unsandboxed`、`auto_retrieve`、`inject_budget_chars_max`）在這裡會檢查型別。`[knowledge.embedding]` 的各個鍵（`provider` = `local`／`openai`／`gemini`／`none`、`local_model`、`cloud_model`、`cloud_account`、`cloud_base_url`、`allow_private_endpoint`、`batch_size`、`threads`、`max_vector_rows`、`idle_unload_minutes`）也一樣；揭露確認紀錄 `cloud_disclosure_ack` 不能在這裡改（存檔會被拒絕），只能經由「文件知識庫搜尋」的揭露對話框寫入。員工的 `[knowledge] collections` 在「AI 員工 → 編輯設定 → 工具與權限 → 文件知識庫」設定（只能選已分享給它的集合），也可以直接改該員工的 `agent.toml`；回覆相關的 `auto_retrieve`、`answer_mode` = `assist`／`kb_only`、`top_k`、`inject_budget_chars` 也在那裡。詳見[文件知識庫](../../features/zh-TW/63-knowledge-base.md)。
- **衝突：**開啟時取得檔案內容的雜湊值，存檔時帶回。檔案在這段時間被改過就拒絕，請重新載入再改。
- **備份：**寫入前備份成 `<檔名>.bak-<Unix 時間>`（權限 0600），保留最近 5 份。
- **稽核：**每次寫入記 `config_raw_edited`。改到 `[delegation]`、`[acp]`，或員工的 `[agent]`、`[capabilities]`、`sandbox_enabled`、`network_access`、`can_modify_own_soul` 時等級為 Warning。
- **重啟清單：**存檔後列出需要重啟的部分。`config.toml` 的 `[gateway]`、`[server]`、`[telemetry]`、`[logging]`、`[channels]`、`[wiki]`、`[relay]`、`[decision]` 只在開機時讀；`general.name`、`rotation.health_check_interval_seconds`、`goal_loop.resume_on_restart`、`knowledge.max_source_bytes`（上傳路由的大小上限依它決定）也要重啟；日誌等級、常駐感知、去識別化能立即套用時就不列。`inference.toml` 存檔後一律立即重設推理引擎。員工檔案只有 `heartbeat.max_concurrent_runs` 需要重啟。

## 維修 SSH 儀表板（v1.75.0）

到「裝置 → 維修模式」，由管理員選期限與功能、重新輸入密碼，
再輸入 `MAINTENANCE`。勾選 SSH 還需要貼上一把技術人員的 OpenSSH
公開金鑰。畫面先檢查格式；誤貼私鑰會立即清空，不會送出。閘道與
裝置輔助程式仍會各自驗證。RSA 須為 3072–8192 位元。無法確認支援、
舊版或更新不完整的映像會停用 SSH 選項，診斷詳情仍可使用。
密碼連錯 5 次後，15 分鐘內不能開啟。

視窗開啟後會顯示 `ssh duduclaw-maint@duduclaw.local`、可用的私有
IPv4 位址、技術人員與本機指紋、到期時間、連線與登入失敗次數。
登入前請核對管理員推播中的兩個指紋；沒有推播通道時，到裝置本機
螢幕核對，無法只靠此儀表板確認裝置身分。已設定通道不代表推播
已送達。22 埠只接受私有／連結本機來源與 VPN CGNAT `100.64.0.0/10`，
不接受全域 IPv6；來源位址符合範圍不代表可信。維修選單可查指定
診斷資料、重啟本地模型或閘道；重啟閘道會關窗並中斷 SSH。

關閉未獲裝置確認時會持續顯示警告。展開「維修紀錄」可查最近的
視窗金鑰指紋與連線事件。畫面每 30 秒讀取一次狀態，到期或裝置
自行關閉可能要等下次讀取才顯示。完整 OS 映像、真實 SSH 與推播


## 安全 → 憑證保護（v1.76.0）

「安全」頁的「憑證保護」卡片（管理員，兩版都有）顯示一行：「憑證保護：檔案」
或「憑證保護：系統金鑰圈（…）」，下面說明代表什麼。它只管讓 AI 員工代表特定人
的憑證；不論卡片顯示什麼，其他設定都照舊運作。

- **改放系統金鑰圈**（`security.credential_protection_enable`）在 macOS 金鑰圈
  或 Windows 認證管理員建立保護金鑰。一般的 Docker 或 Linux 安裝顯示「檔案
  （此處無法使用個人帳號）」，沒有按鈕。
- **開啟憑證保護**：用 person-credentials 疊加設定啟動的 Docker 會顯示這個按鈕
  （見 [Docker 指南](docker.md#74-個人帳號person-credentials-疊加設定)）。金鑰是
  啟動時交給 gateway 的獨立根金鑰，那一行顯示「獨立金鑰（AI 員工讀不到）」。
  啟動檢查沒過時，卡片顯示「獨立金鑰無法使用」與原因（例如掛載了 Docker
  socket），不提供按鈕。持有這把金鑰期間，NER 去識別化、本機知識庫向量與
  Kokoro 語音會關閉。這裡的「換一把新金鑰」不會換掉根金鑰，換根金鑰見 Docker 指南。
- **需要重新授權**：macOS 升級後或金鑰圈鎖著時出現。在那台 Mac 的終端機執行
  `duduclaw credentials authorize`，允許一次對話框即可。
- **保護金鑰不見了／被更換／設定錯誤**：個人帳號停用。按**重新啟用**
  （`reset: true`）會產生新金鑰，已保存的個人憑證作廢，大家要重新連結。
- **換一把新金鑰**（同樣是 `reset: true`）在一切正常時也提供，用於懷疑金鑰和記錄
  一起被換掉的情況，效果和重新啟用相同。macOS 執行檔沒開 hardened runtime 時，那一行
  會顯示「同一個帳號的程式讀得到」，因為注入 DuDuClaw 的程式碼讀得到金鑰。
- **停用**（`security.credential_protection_disable`，`confirm: true`，需輸入
  確認文字）刪除金鑰，已保存的個人憑證作廢。

[原生 Linux 受保護服務](linux-protected-service.md#容器引擎介面暴露後的恢復)的卡片也會顯示最近的根金鑰撤銷、UTC 時間與具體的引擎 socket 診斷。介面暴露期間個人帳號暫停；預設每隔 60 秒檢查，連續通過三次後正常關機，再由 root 啟動器重新交接金鑰，每個滾動小時最多四次恢復重啟。狀態檔讀寫失敗會阻止自動恢復。本次沒有新增儀表板開關；要維持人工重啟方式，請在 `config.toml` 設定 `[credential_protection] auto_recover = false`。

終端機用 `duduclaw credentials status`（腳本可加 `--json`）看同樣的狀態。細節見
[安全防護](../../features/zh-TW/05-security-defense.md#指定人的憑證放系統金鑰圈v1760)。

直接綁定歷史隱私：`[people] bound_history_privacy = true` 預設開啟，缺值或格式錯誤也維持限制。管理員可在進階編輯器明確設 `false` 過渡，恢復先前 Viewer／Operator 的廣泛歷史讀取。Owner／Admin 及聊天／呼叫權限不變；搜尋、匯出、附件與 NER 限制見功能 68。

## 安全 → 資料外送防護（v1.77.0）

安全頁可把 `config.toml [security] egress_guard` 設成四種值：**關閉**（`off`）、**僅記錄**（`shadow`，預設）、**先詢問**（`ask`）、**直接阻擋**（`block`）。存檔沿用既有加鎖的 `system.update_config` 寫入；每次呼叫時由防護讀取設定。員工的 `agent.toml [capabilities] egress_guard` 只能更嚴格（`off < shadow < ask < block`）。沒有 `config.toml` 時採用預設的僅記錄；設定無效或讀不到時，該次呼叫會被阻擋。

AI 員工在同一回合成功讀過私人資料後，之後的外送呼叫會先經過防護。僅記錄會在稽核寫下 `would_ask` 與讀過的工具，照常執行；先詢問會開出一次性的核准單，列出目的地、完整內容、讀過的工具與到期時間，只有目前具 Admin 權限的儀表板帳號能核准（通道按鈕與聊天回覆都不行）；直接阻擋則拒絕。涵蓋平台 MCP 呼叫與第三方 MCP 代理，不涵蓋 Claude CLI 內建工具與其他 runtime 的原生工具；以回合為單位追蹤。詳見[資料外送防護](../../features/zh-TW/69-egress-guard.md)。

設定下方的僅 Admin 可讀觀察報表，可選最近 7 或 30 天，查看去重後的預估核准單數、員工／工具／來源／日期分類，以及 CLI 內建工具與 MCP 入口的數量。原本會拒絕的紀錄不增加預估開單數；沒有觀察紀錄或收集失敗不代表安全。`duduclaw doctor` 可看租約健康；停止所有 gateway 與外送動作後，可用 `duduclaw egress-leases list` 取得確認 token，再用 `clear` 離線復原。完整步驟見資料外送防護的復原章節。

## 安全 → 資料刪除收據

斷開通道、Google 帳戶、資料庫來源或 Remote MCP 時，會自動準備記憶刪除申請。「已斷開」與「記憶已刪除」是兩個不同的結果。

1. 在對應設定頁斷開服務。批次建立後，回應會提供「查看審批」。若 `[memory] forget_source` 已關閉，或設定無法讀取，就不建立計畫；規劃失敗也會另外回報。
2. 管理員到收件匣開啟卡片，查看每位 AI 員工的筆數。批次七天後到期；執行前刪除範圍擴大，需要重新核准，範圍縮小可繼續。
3. 核准結果存妥後，gateway 建立持久的背景工作，重試未完成步驟，重啟後也會繼續。刪除或必要後續步驟尚未完成時，不會產生收據。
4. 開啟**系統 → 安全 → 資料刪除收據**，展開連線並按「下載 JSON」。可重新整理查看新完成的工作。筆數分開列出已刪記憶、關鍵事實、封存副本、無法歸屬，以及保留的系統紀錄。

Google 個人連線依實際使用憑證的使用者歸屬，收據則按 AI 員工列出刪除筆數。沒有連線標記的舊記憶無法事後確定歸屬。對話原文、`tool_calls.jsonl`、wiki、備份不在此次刪除範圍。Remote MCP 原有 `forget` 選項是刪除已儲存的連線紀錄；獨立的 `forget_memories` 選項控制記憶申請，預設啟用。

非 Google 的 OAuth 撤銷目前沒有連線歸屬，會回報略過記憶刪除。清除 Google 設定的憑證路徑，不會同時撤銷另一份獨立儲存的 OAuth 授權；請用該授權自己的斷開操作處理。

連線來源標記會保留來源與角色；單純佐證不會取得刪除所有權。另一個仍連線的來源也提供過同一筆記憶時，該記憶會保留，計畫、管理員核准卡與收據以 `retained_shared` 逐員工列出數量。只有全部擁有來源的連線都已斷開，且沒有其他仍有效的直接來源，才會刪除；因此第二個連線斷開時，才可能選入先前保留的共享記憶。歸屬不明的記憶（包含無法還原角色的舊標記）保留並計入無法歸屬。衍生記憶也逐筆檢查來源。規劃與套用前會重新核對連線狀態；透過平台重新連線時，會先撤銷舊的斷線證據再發布連線。

核准會綁定排序後的孤兒實體向量識別值，不只綁定數量；即使數量相同，替換成其他實體也必須重新核准。未完成工作會持久保存嘗試次數與下次執行時間，以 60／120／240／480 秒退避，並公平選取已到期工作。連續五次失敗或執行中斷後會隔離；管理員可在「資料刪除收據」查看不含記憶內容的失敗原因並按「重試」。重試沿用既有核准並重查範圍，擴大時仍須重新核准。


## 對話憑證

Admin 可在「系統 → 安全 → 憑證稽核」查看中繼資料、篩選、編輯服務、標記誤判，以及匯出有稽核紀錄的本頁 CSV。旁邊的清冊以唯讀方式列出系統設定憑證。透過 `system.update_config` 的 `security.chat_credentials` 或設定編輯器設定 `[security] chat_credentials = "audit" | "mask"`。預設 `audit`；未知值視為 `mask`。稽核模式保留對話原文；遮罩模式只改新進站訊息中偵測到的值，不改舊紀錄。兩種模式都遮罩衍生儲存中偵測到的值。偵測限制與登錄配額見[憑證稽核](../../features/zh-TW/70-credential-audit.md)。服務名稱與備註不可填入真正的憑證。

## 管理 → 整合 → Google 設定

管理員可在 `/manage/integrations?tab=google` 開啟可續接的設定精靈，依情況選公司委派、Internal OAuth、Apps Script 或個人 External OAuth。進階表單仍可使用。替換生效路徑前須確認；OAuth 權杖保留到明確中斷連線。測試逐項回報服務結果，Sheets、Docs 僅檢查權限時標記未驗證。主控台步驟、遠端轉送與 Testing 到期見[連接 Google](google-setup.md)。


## 收件匣與工作時間軸

收件匣的「待處理」只列出還需要你處理的事，先排快到期的項目，再排高風險、員工提問與等你決定的任務、預算通知。「等太久」列出已等超過一天的待處理事項。點選或用鍵盤才會開啟並標記已讀，滑鼠移過不會切換內容。核准、安裝申請與等你決定的任務，必須先處理才能封存；其他項目可在任何分頁封存，包含沒完成的工作。在收件匣頁面按 ⌘Z 或 Ctrl+Z 復原封存，輸入文字時保留原本的復原功能；U 標記未讀。處理後會顯示結果，不再顯示同意、拒絕按鈕；電腦與手機的決策按鈕都固定在詳情底部。清單與詳情在畫面可用高度內各自捲動，選另一項時從詳情頂端開始。首頁沿用收件匣的急迫排序，包含核准期限與風險。側邊欄、手機鈴鐺、收件匣標題與首頁共用待辦數，排除已處理與已封存的項目；沒有待辦時仍可點手機鈴鐺。員工提問目前只檢查前 12 位員工，頁面會提示涵蓋範圍與載入失敗。

工作時間軸回答「這段時間誰完成了什麼、誰卡住、哪件在等我」。點選「完成、進行中、等你決定、沒做完」可篩選下方員工卡片。等你決定的員工排最前，其中距上次更新最久的先列出，再來是沒做完、進行中與沒有工作紀錄的員工。等待時間標示為「已等 N 天（距上次更新）」。點任務名稱就能查看任務；距上次更新的時間可用來判斷是否等太久，但不代表精準的狀態開始時間。「查看時間分布」預設收合，顏色與不同形狀共同代表狀態，長條表示任務開啟多久，不代表實際工作時數。任務長條可用鍵盤或觸控開啟。

## 雙因素驗證強制設定

```toml
[auth]
mfa_enforced = true
```

新的團隊安裝預設強制 MFA；既有安裝保留目前政策，尚無 MFA 政策紀錄的舊版也不會自動開啟。Admin 可在儀表板「使用者 → 雙因素登入管理」的專用開關啟用或停用強制。新團隊的第一位 Admin 以密碼登入後，需由主機操作員執行 `duduclaw ops users mfa-enrollment-code <email> <challenge_ref>` 核發註冊碼，再完成驗證器註冊；未設定 Email 寄信時，首次註冊無法只靠儀表板完成；已設定者可改用信箱驗證。既有未強制的安裝可繼續自願註冊，開啟前請查看 `mfa.overview`、確認通道送達並測試主機救援。兩種 dashboard 設定寫入都要求目前有效的 Admin。註冊及後續憑證變更透過帳號 epoch 作廢舊 JWT；單純切換強制則讓既有 access token 自然到期，拒絕未註冊者 refresh。強制時拒絕舊共用 token WebSocket 登入。

設定使用啟動時載入的記憶體快照；dashboard 寫入後更新，外部檔案每 30 秒重載。失敗時警告並保留最後有效值，任何版本從未載入有效值都拒絕；啟動時既有政策資料庫無法讀取或已損壞也會拒絕。缺少 `[auth]` 表格或設定鍵時保留資料庫中的政策；新團隊初始化會在設定檔及帳號資料庫明確記錄 true。所有版本都依儲存的政策強制驗證，授權過期、驗證失敗回退或指定 Personal 啟動都不會停用；只有 Admin 主動關閉才取消強制。沒有政策資料庫的全新 Personal 安裝仍預設不強制；本機自動登入不能略過已啟用的強制驗證。Personal 同樣可使用主機救援。證明欄位、工作階段綁定及 `duduclaw ops users mfa-reset`／`mfa-enrollment-code` 主機救援見[安全防禦](../../features/zh-TW/05-security-defense.md)。金鑰輪替不在本階段範圍。

啟用前請從 `mfa.overview` 檢查通道綁定時間；只有早於強制啟用的已驗證綁定可作註冊證明。持有密碼者在強制前植入的綁定仍無法辨識。請將回傳的 12 字元 `challenge_ref` 讀給另一位 Admin，以 `users.mfa_enrollment_code {user_id,challenge_ref}`，或主機 `duduclaw ops users mfa-enrollment-code <email> <challenge_ref>` 核發該次註冊碼。重載失敗會在 `mfa.overview.policy` 及 `system.status.mfa_policy` 顯示；最後有效值為 false 就保持 false。兩個設定寫入路徑都在寫入鎖內重查 Admin。CLI 匯入也會在搬移資料前檢查目前資料庫政策；團隊匯入若包含 users.db 或其 SQLite 附屬檔，設定檔須明確保留目前的 mfa_enforced 值，避免備份中的資料庫改變強制設定。

CLI 家目錄匯入以同一份私有快照檢查並解壓縮，壓縮備份上限為 8 GiB。兩階段皆沿用備份還原的預設上限：最多 200,000 個 tar 項目、單項 2 GiB、解壓總量 8 GiB。先檢查原始標頭，再讓 tar 解析 GNU／PAX 中繼資料；中繼資料單項限 1 MiB，大小檢查包含 PAX 覆寫的實際值。第三方 PAX 備份若因大小覆寫而無法逐一讀取原始標頭，可能被拒絕；本程式匯出的 GNU 長路徑仍可匯入。路徑元件去除尾端點與空白，先做 NFC 正規化，再以大寫轉小寫的鍵值比對。不採 NFKC，因此全形 `ｃｏｎｆｉｇ.toml` 不視為 `config.toml` 的別名。所有平台皆拒絕受保護檔名別名（如 `Config.toml.`）與重複目的地；只有 Windows 拒絕冒號（含 NTFS 替代資料串流）、DOS 保留名稱與 `CONFIG~1.TOM` 等可能的 8.3 短檔名。Unix 合法的 `aux`、`notes/2026-11-04T07:00` 可匯出再匯入；所有平台仍拒絕反斜線與非一般路徑元件。`--force` 搬動原本的員工資料夾前，會先完成拒絕條件檢查與必要的強制設定持久寫入，發布失敗時 `agents/` 保持原位。目前強制雙因素驗證且替換帳號資料庫或 SQLite 附屬檔時，略過備份內的設定檔，即使文字相同也一樣；Windows 使用等待寫入完成的替換方式。

儀表板一次只接受一份待套用還原；已有待處理還原時，請先重新啟動閘道套用，沒有取消還原的 RPC。每次還原先在獨立隨機目錄解壓與驗證，全程持有該資料目錄的還原鎖，成功後先同步所有一般檔案與目錄，再改名發布、同步資料目錄、持久寫入標記並再次同步資料目錄，全部成功才刪除上傳檔。Windows 因 std 不支援目錄同步，僅同步檔案。失敗只清除該次暫存，不會修改已發布資料；若發布後寫入標記失敗，會保留資料並拒絕後續覆寫，提示管理員檢查。

備份還原會保留還原前目前有效的 `auth.mfa_enforced`，只覆寫備份中的這個設定鍵，其餘資料照常還原。讀不到目前設定時採嚴格值（所有版本強制啟用）；備份值不同時儀表板會提示，還原後記錄 `backup_restore_mfa_policy_preserved` 稽核。需要變更時請使用雙因素驗證專用開關；設定匯入仍會直接拒絕變更這個值。每次啟動都只在早期嘗試取得一次執行鎖，即使當時沒有還原標記也一樣；若失敗，即使稍後才出現標記且其他閘道退出，該行程也不會重試。還原在讀取目前政策或修改暫存資料前，必須已獨占閘道資料目錄；若另一個閘道仍在使用，會延後還原，保留還原標記與暫存資料；閘道仍依既有降級模式啟動，該行程整個生命週期都不執行工作流程復原、派工與清理，即使其他閘道退出也不會重新取鎖，須重新啟動才套用待處理還原。還原保留現有執行鎖目錄，不換入備份中的鎖，避免政策檢查與資料換入之間覆蓋 Admin 剛完成的開關操作。

### Email 驗證碼、通行金鑰與保持登入（未發行）

在「設定 → 帳號」可保留原有驗證器 App，再加入 Email 驗證碼或多把具名的通行金鑰（Passkey）。新增或移除前需輸入目前密碼。Email 設定會將六位數代碼寄到帳號信箱，驗證成功才啟用。管理員需先完成既有 `[channels.email]` SMTP 寄信設定（`smtp_host`、`from_addr`，以及寄信服務需要的帳密）；未設定可用的寄信服務時，介面會顯示無法使用。因素變更會留下稽核，已設定寄信時也會通知本人。

輸入密碼後，可選擇已設定的驗證方式，預選上次成功使用的方式。Email 驗證碼限用一次、10 分鐘到期，每次登入挑戰最多嘗試五次；每分鐘可重寄一次，每個帳號每小時最多五次。通行金鑰使用裝置鎖定、指紋、臉部辨識或安全金鑰驗證。管理員需將公開儀表板網址設定於 `[dashboard] public_url`，並以完全相同的 HTTPS 來源開啟儀表板；只有本機 localhost 可用 HTTP。每把金鑰可命名、個別移除，建議另留一種驗證方式，以免失去裝置後無法登入。首次只註冊通行金鑰不會產生復原碼，既有驗證器復原碼維持有效。首次 Email 註冊會顯示一次復原碼，確認保存後才能繼續。

強制兩步驟驗證接受任何一種已設定因素，也會拒絕移除最後一種。原有驗證器與復原碼不需重新註冊。強制首次 Email 註冊以信箱代碼證明收信權；驗證器及通行金鑰仍需獨立的通道／管理員證明。以通道碼完成第一步登入者還需目前密碼。管理員重設會清除所有因素與保持登入的裝置。操作者可用目前密碼或新的驗證器／復原碼確認身分，因此只用通行金鑰的管理員也能執行。

登入頁的「保持登入」預設不勾選。勾選後以輪替的瀏覽器 Cookie 保持登入，預設採 180 天滑動續期：每次成功更新登入，都從該次使用時間重新計算到期日。管理員可在「安全 → 保持登入」設定 1 至 365 的整數，預設 180 天。政策永久存入 `users.db`；讀取失敗或值無效時回到 180 天，不會變成無限期。變更設定不立即改動既有到期日，下次成功續期才改為該次使用時間加上新天數，縮短天數也一樣適用。開發版原有的 `[auth] remember_days` 不再讀取。未勾選時維持原有工作階段行為。帳號設定的「已登入的裝置」列出保持登入的裝置、建立與最近使用日期及已記錄的 IP，可個別撤銷或「登出所有裝置」。密碼、因素、帳號狀態或角色變更、兩步驟驗證重設及刪除帳號都會撤銷保持登入；登出會撤銷目前裝置。

管理員可在「同仁 → 編輯同仁」開啟帳號詳情，按「登出此同仁的所有裝置」並確認，立即撤銷該帳號所有一般及保持登入工作階段。停用或移除同仁也會撤銷登入；既有 access token 會被拒絕，已連線的 WebSocket 在既有即時撤銷檢查（含下一次心跳）停止，不必等到180天到期。僅限管理員的 `sessions.policy`、`sessions.policy_update`、`users.revoke_sessions` 會重查目前權限；變更在認證稽核中記錄操作者、目標與結果，不記錄憑證。

| 控制項 | 儲存位置／RPC | 生效時間 |
|---|---|---|
| 安全 → 保持登入天數，僅限管理員 | `users.db` → `remembered_policy_state.days`；`sessions.policy`／`sessions.policy_update` | 不需重啟，新登入及下次成功續期生效；1–365，預設 180。 |
| 同仁 → 編輯同仁 → 登出此同仁的所有裝置，僅限管理員 | `users.revoke_sessions`；憑證版本、裝置撤銷及認證稽核 | 確認後立即寫入撤銷；既有連線在原有檢查點停止。登出自己走既有帳號 HTTP 端點，避免連線撤銷後收不到成功回覆。 |

使用 Cookie 驗證的登入請求必須帶有 `X-DuDuClaw-CSRF: 1`，並完整比對 Origin 的通訊協定、主機與有效連接埠。內建 HTTP loopback Origin 只接受閘道的監聽連接埠；反向代理請在 `[gateway] allowed_origins`（或 `DUDUCLAW_ALLOWED_ORIGINS`）列出完整的 `https://主機[:連接埠]` Origin，這些端點不接受只有主機名稱的設定。更新登入與登出會在變更工作階段前，拒絕跨連接埠頁面與一般 HTML 表單。

更新登入以經過簽章驗證的 family 世代編號輪替，每個 family 只保留目前一筆 token 雜湊；重用有效的舊世代憑證會撤銷該裝置，偽造世代編號則無法撤銷。獨立的 `users.remember-key` 簽章金鑰僅限檔案擁有者讀寫，備份與還原時必須與 `users.db` 一起保存；已有世代工作階段卻遺失金鑰，或金鑰損壞時，認證資料庫會拒絕開啟。世代功能前的開發版工作階段會在升級時撤銷。每個帳號最多保留 20 台有效裝置及 100 筆 family 紀錄（含已撤銷裝置）；達到紀錄上限時，請先使用一般登入，等候舊紀錄到期。紀錄在最後續期的到期日再加七天後，於下一次成功的保持登入操作清除。以帳號登入的管理介面與 WebChat 連線最長存活 24 小時，之後須重新連線驗證；原有較短的 MFA 到期限制仍適用，避免舊連線存活超過撤銷紀錄保留期限。


長期憑證不放入 localStorage，JavaScript 也無法讀取：Cookie 使用 HttpOnly、SameSite=Strict，範圍限定 `/api/session`，HTTPS 下加上 Secure。伺服器只保存雜湊，每次使用就輪替；舊憑證重用會撤銷整個憑證家族。Cookie 端點會檢查 Origin，儀表板 WebSocket 升級也會檢查 Origin。

實際 SMTP 送達、各平台瀏覽器的通行金鑰註冊與登入、跨平台 Rust 建置、依賴稽核及完整 gateway 整合測試仍待驗收；介面程式與已撰寫測試不代表上述項目已通過。

### 儀表板雙因素登入

Enterprise 與強制驗證中的 Personal 使用者在「設定 → 帳號」選擇「設定雙因素登入」，輸入目前密碼，以驗證器 App 掃描 QR Code（或手動輸入密鑰），再輸入六位數代碼確認。複製或下載十組復原碼，勾選「我已保存復原碼」後繼續。密鑰與復原碼只保留在元件記憶體，離開畫面即清除；複製及下載需自行按下按鈕。註冊與停用會換發目前工作階段的權杖並保留連線，其他工作階段則失效。復原碼會顯示到你確認保存或離開畫面。權杖簽發與 MFA 變更在同一交易提交；簽發失敗時不會完成註冊或停用，可重試。重產復原碼及停用需提供新的驗證器代碼或未使用復原碼；停用另需目前密碼，強制期間無法停用。

密碼與通道驗證碼登入保留原表單，只有 gateway 要求第二因素時才顯示驗證器代碼步驟，可切換使用單次復原碼。登入要求五分鐘內有效，最多五次嘗試。強制註冊會要求獨立的通道／管理員代碼，以及目前密碼（通道登入必填）；只把畫面上的註冊識別碼交給管理員，勿分享註冊憑證。

Admin 在「使用者 → 雙因素登入管理」查看註冊狀態與通道驗證時間，使用自己的新因素重設其他帳號，或依對方這次的註冊識別碼核發一次性代碼。不能自我重設或自我核發。強制開關顯示未設定帳號數、登入受阻警告，並要求輸入畫面上的確認文字。確認開啟時固定本次動作，重新整理會清除確認；切換 RPC 必須提供 `expected_state`，設定鎖內發現狀態不同就拒絕。設定重載降級時會警告並停用開關。新團隊安裝一開始就強制驗證。既有仍關閉的安裝，請先確認註冊、通道送達與主機救援流程，再由 Admin 開啟。Personal 在政策要求註冊或帳號已註冊時顯示帳號 MFA 設定；未強制且未註冊時仍隱藏。強制中的 Personal，Admin 可從「系統 → 安全 → 雙因素登入管理」使用同一套管理面板與專用開關主動取消強制。所有版本皆可由主機操作員執行 `duduclaw ops users mfa-reset <email>` 救援帳號；重設帳號不會關閉強制政策。
Enterprise 使用者在「設定 → 帳號」選擇「設定雙因素登入」，輸入目前密碼，以驗證器 App 掃描 QR Code（或手動輸入密鑰），再輸入六位數代碼確認。複製或下載十組復原碼，勾選「我已保存復原碼」後繼續。密鑰與復原碼只保留在元件記憶體，離開畫面即清除；複製及下載需自行按下按鈕。註冊與停用會換發目前工作階段的權杖並保留連線，其他工作階段則失效。復原碼會顯示到你確認保存或離開畫面。權杖簽發與 MFA 變更在同一交易提交；簽發失敗時不會完成註冊或停用，可重試。重產復原碼及停用需提供新的驗證器代碼或未使用復原碼；停用另需目前密碼，強制期間不能移除最後一種驗證方式。

密碼與通道驗證碼登入保留原表單，只有 gateway 要求第二因素時才顯示驗證器代碼步驟，可切換使用單次復原碼。登入要求五分鐘內有效，最多五次嘗試。強制註冊會要求獨立的通道／管理員代碼，以及目前密碼（通道登入必填）；只把畫面上的註冊識別碼交給管理員，勿分享註冊憑證。

Admin 在「使用者 → 雙因素登入管理」查看註冊狀態與通道驗證時間，使用自己的新因素重設其他帳號，或依對方這次的註冊識別碼核發一次性代碼。不能自我重設或自我核發。強制開關顯示未設定帳號數、登入受阻警告，並要求輸入畫面上的確認文字。確認開啟時固定本次動作，重新整理會清除確認；切換 RPC 必須提供 `expected_state`，設定鎖內發現狀態不同就拒絕。設定重載降級時會警告並停用開關。新團隊安裝一開始就強制驗證。既有仍關閉的安裝，請先確認註冊、通道送達與主機救援流程，再由 Admin 開啟。Personal 可自願設定驗證方式，本機自動登入維持原狀。

呼叫端連線會在註冊／停用的成功回應取得新權杖，並在一般送出檢查前完成身分交接；HTTP 註冊也在註冊交易提交前簽發權杖。發版驗收仍須完成真實瀏覽器／WebSocket 驗證，包括跨過心跳後復原碼仍可見，以及另一工作階段失效；行程內 handler 與前端單元測試不能代表這些驗收通過。

## 語音合成

`auto` 依序嘗試已安裝 Piper → 已安裝 Kokoro → MiniMax → OpenAI → Azure，並遵守產品政策。指定供應商時只用該供應商；`off` 停用合成。`duduclaw doctor` 會列出可用供應商，不發送合成請求。

從 **設定 → 語音** 下載 Kokoro（約 134 MB，Apache-2.0）。安裝會驗證檔案與共用 ONNX Runtime，下載中斷後可續傳；模型權重不內含於執行檔。模型位於 `<home>/models/kokoro-zh/`。預設聲音為 `zf_001`，另提供 `zm_010` 與純英文用的 `af_maple`。中文帶有中國大陸口音，多音字可能誤讀。繁體字與數字會正規化，但發音規則無法涵蓋所有詞語。輸入上限 2,000 字，每段最多 500 tokens，段落間加入 150 ms 靜音。Kokoro 輸出 24 kHz WAV；Telegram 透過 `sendAudio` 收到 `reply.wav`。

Azure 會將文字傳送至 Microsoft。請在 gateway 環境設定 `AZURE_SPEECH_KEY` 與 `AZURE_SPEECH_REGION`。預設中文聲音為 `zh-TW-HsiaoChenNeural`，英文為 `en-US-AriaNeural`，輸出 MP3。`tts_voice` 可指定該供應商支援的聲音。

Admin 專用 RPC：`voice.kokoro_status` 回報安裝狀態、缺少的檔案、大小、執行環境、進度與錯誤；`voice.kokoro_install` 以可重複呼叫的方式啟動背景安裝。語音下載卡會輪詢狀態並顯示安裝錯誤。

## 在 WebChat 回覆期間繼續傳訊息

AI 員工回覆時，你仍可輸入並送出下一則訊息。同一個對話中的新訊息會顯示「排隊中，這一輪結束後處理」，等目前的回覆結束後再處理，不會插入正在進行的回覆。該則訊息開始傳回進度或回覆時，排隊標示就會消失。透過 `@` 呼叫另一位員工時，可能由另一條處理佇列執行。

按等待訊息上的「取消排隊」可取消該則訊息；按送出旁的「停止」可停止目前的回覆。取消結果以伺服器確認為準，送出取消請求不代表已經取消完成。附件使用方式維持不變。

預設每位使用者最多可同時有三則處理中或排隊中的訊息。超過上限時，訊息會顯示錯誤並保留原文。等目前的回覆完成後，按「重新填入輸入框」即可編輯並再次送出。

## 邀請成員加入

管理員在「管理 → 成員 → 新增成員」勾選「寄送邀請信」，確認信件預覽後建立帳號。邀請帳號沒有可用密碼。已有帳號若仍在職，且尚未設定密碼，可以選「重新寄送」。清單會標示「邀請中」及「邀請已過期」。

在 `config.toml` 設定 `[gateway] public_url = "https://ai.example.com"`，填入成員能連到的儀表板網址。未設定時，儀表板會以管理員目前瀏覽器的來源網址傳入 `public_url`。Gateway 不會採用 HTTP Host 或 Origin 標頭產生連結。沒有網址會回覆 `public_url_required`；設定的網址格式錯誤時會拒絕寄送。所有邀請公開網址一律要求 HTTPS，包含 `localhost`、`127.0.0.1` 與 `::1`。

邀請信沿用 `[channels.email]` 的 SMTP 設定，支援既有加密憑證；目前儲存庫沒有 Resend 或 SendGrid 寄件程式。信內連結七天有效，限用一次。尚未設定寄信或寄送失敗時，管理員會看到一次可複製連結；寄送成功不會回傳連結。關閉結果視窗後不再顯示。重新寄送會撤銷之前未使用的連結，保留歷史紀錄。

收件者開啟 `/invite#<token>`，設定並確認密碼（至少八個字元，最多 1,024 UTF-8 位元組），再走一般登入流程。此頁不會直接登入，原有 MFA 註冊與驗證要求仍適用。過期、已使用、已撤銷及不存在的連結顯示相同結果。停權或離職帳號不能使用邀請。變更帳號信箱、密碼或停用帳號，也會撤銷未使用的邀請。

員工首頁在個人連線尚未完成時顯示「還差幾步」，各項連往「我的連線」。全部完成或讀取失敗時不顯示，也可以在目前瀏覽器關閉卡片。系統記錄建立、寄送結果、使用與撤銷事件，不記錄權杖或連結。頁面讀取網址片段後立即從瀏覽紀錄移除，只在元件狀態保留權杖。公開 `/invite` 頁面不受登入或 FirstRunGate 限制。API 僅提供 `POST /api/invite/check`，以 `{token}` 回傳 `{valid,email_masked?}`，以及 `POST /api/invite/redeem`，接收 `{token,password}`；不再提供帶權杖的 API 路徑。請勿記錄請求本文。同一 TCP 來源 IP 與權杖 SHA-256 前 16 個十六進位字元共用每 15 分鐘 10 次額度；不設來源 IP 的總額度，避免反向代理後的使用者互相阻擋。記憶體表最多保存 10,000 個桶；容量滿時優先淘汰過期桶；沒有過期桶時，才淘汰最早插入的桶。排序索引讓每次請求都不必掃描全表。不採用轉送標頭。
