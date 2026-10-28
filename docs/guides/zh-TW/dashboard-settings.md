# 儀表板設定對照（v1.68.0）

企業版設定（v1.76.0，成員管理 → 組織設定）：`users.org_settings.get/update` 管理 `[people] manager_team_history`，有效設定檔預設開，變更立即生效；重新開啟會重設時間窗，進階編輯器也列為受保護稽核鍵。開啟前請先告知職員。見[主管唯讀團隊對話](../../features/zh-TW/68-manager-team-history.md)。

這份文件列出 v1.68.0 起可以在儀表板設定的項目：在哪一頁、寫到哪個設定鍵、存檔後是否要重啟 gateway。最後一節說明「設定檔進階編輯」，用來修改沒有專屬控制項的鍵。

英文版：[../dashboard-settings.md](../dashboard-settings.md)　日文版：[../ja-JP/dashboard-settings.md](../ja-JP/dashboard-settings.md)

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

沒有指定員工的全域背景工作（例如選擇執行員工、建立遮罩規則）預設使用 Claude／`claude-haiku-4-5`。選擇支援的後端並填入相容模型；伺服器拒絕已移除的 `gemini` 與未知後端。清空模型會移除設定鍵：Claude 使用 `claude-haiku-4-5`，OpenAI 相容端點必須指定模型。Codex 也可用受限輔助模式執行，模型留白時使用 CLI 預設值。Grok、Antigravity 與通用 CLI 即使可儲存設定，執行時仍拒絕。非 Claude 後端填入已知其他廠商的模型時，頁面會顯示警告。儲存需要管理員權限，沿用既有設定寫入流程。所選全域後端為 Claude、且沒有員工使用 Claude 執行環境時，頁面會顯示警告；這不代表系統已檢查 Claude 登入狀態。

`[general] log_level`（常用 → 一般設定 → 日誌等級）存檔後立即套用；只有在環境變數 `RUST_LOG` 已設定時無法套用，這時回報需要重啟。帳號輪替策略與限流冷卻時間存檔後會清掉輪替快取，下一次呼叫就用新設定（之前最多延遲 30 分鐘）。

## 通道管理

- 群組「只回提及」（`mention_only`，每個群組各自設定）：v1.76 起，LINE 群組開啟後只回應用 `@` 叫 AI 員工（或要 @ 名單）的訊息，以及被點名員工正在等的那一則；以前開了會整個群組沒有回應。Telegram 共用 bot 也接受員工名字。見[用 @ 呼叫 AI 員工](../../features/zh-TW/62-calling-ai-staff.md)。
- 網站聊天元件：「開放網站聊天元件」寫 `[webchat] public_widget`，「元件金鑰」寫 `[webchat] widget_key`（16 到 256 個可見 ASCII 字元，有「產生」按鈕；開啟元件時必須有金鑰；金鑰不會在回應中回傳）。立即生效。元件金鑰以明文存在 `config.toml`：它是公開的金鑰，會出現在網站的頁面原始碼裡，所以不當成密鑰處理。
- WhatsApp、飛書、Google Chat、Teams、企業微信、釘釘：六條 webhook 路由一律掛著。通道設定好之前回 404；設定好之後會驗證平台的簽章，簽章錯誤回 401。`channels.add` 不必重啟就會啟動通道，`channels.add` 回 `hot_started` 與 `restart_required: false`；憑證不完整時附 `not_started_reason`。個別員工的 Slack bot 也會在新增時直接啟動。
- 通訊軟體的思考顯示（沒有專屬控制項，在設定檔進階編輯裡設定，每次回覆都重新讀取）：`[thinking] channel_audience` = `internal`（預設：通道身分已驗證、帳號啟用中，且是管理員或綁定回答員工的發話者看得到計時與遮蔽後的摘要，其他人只看計時）、`all`（一對一聊天的所有發話者都看得到摘要）或 `none`（都不顯示）；`[thinking] channel_group_display` = `status`（預設：群組聊天只顯示計時）、`summary` 或 `off`。`[thinking] display` 與回答員工的 `[model] show_thinking` 取較嚴格者，仍是兩者的上限。`config.toml` 讀不到或值不認得時都不顯示。網站訪客（Odoo 線上客服、WebChat 網站小工具）永遠看不到思考。詳見[通訊軟體的思考顯示](../../features/zh-TW/66-thinking-in-messaging-apps.md)。

## 整合 → Odoo → 連線設定

「允許連到內網的 Odoo」寫入 `[odoo] allow_private_network`（只有管理員能改，預設關閉；隨連線設定由 `odoo.configure` 一起儲存，「測試連線」用畫面上的開關值）。關閉時，Odoo 網址（全域或個別員工）必須是 HTTPS 且指向公開主機（只有 `localhost` 可用 `http://`）。開啟後允許 192.168.x.x、10.x.x.x 這類內網位址，HTTPS 或 HTTP 都可以；本機位址、link-local 與雲端 metadata 位址（169.254.169.254、`metadata.google.internal`）仍一律拒絕，連線會釘在解析時檢查過的位址上，也不走代理。升級後第一次開機時，若已設定的 Odoo 網址解析到內網位址，系統會自動把它打開（稽核事件 `odoo_allow_private_network_auto_enabled`），舊客戶不會斷線。下次連線時生效。

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
| 技能 → 這位員工的技能（v1.76.0） | 不是設定鍵：開關存在 `<home>/skill_state/<agent_id>.json`（`skills.set_enabled`），移除的技能搬到 `<home>/skill_archive/<agent_id>/`（`skills.remove`／`skills.restore`）；需員工負責人或管理員。見[技能庫頁面](../../features/zh-TW/15-skill-lifecycle.md) |
| 工具與權限 → 需要把關的工具（一律等人核可、視為不可逆、先由判官判斷、需任務授權） | `[capabilities] approval_required_tools`、`irreversible_tools`、`maybe_irreversible_tools`、`scoped_tools` |
| 工具與權限 → 平行分支 | `[fork] enabled` |
| 工具與權限 → 出站內容防護 | `[guardrails] enabled`、`block_secrets`、`block_injection_echo`、`redact_pii`、`deny_phrases` |
| 工具與權限 → 權限 | `[permissions]` 四個旗標（見下方） |
| 腦袋與引擎 → 推理力度 | `[model] effort` |
| 腦袋與引擎 → AI 後端／備援後端 | `[runtime] provider`／`fallback`，新增 qwen、kimi、copilot、kiro、cursor、vibe、opencode |
| 腦袋與引擎 → 精簡啟動內容 | `[runtime] minimal_context` |
| 腦袋與引擎 → 單次執行時間上限 | `[runtime] timeout_secs`：整數秒，預設 300，範圍 30～3600；適用 Codex／Grok／Antigravity／其他 CLI／OpenAI 相容 API；Claude 另有固定 30 分鐘上限 |
| 腦袋與引擎 → 輔助模型 | `[model] utility`：Codex 與 OpenAI 相容 API 留白時沿用主要模型；Claude 預設 `claude-haiku-4-5`；Grok、Antigravity 與通用 CLI 改用全域背景設定 |
| 腦袋與引擎 → 一員工四角色 | `[team] enabled`、`[team.roles.<角色>]` |
| 自動化 → 夜間整理 | `[night_engine] enabled` |
| 整合 → 專屬通道（v1.76.0） | `[channels.telegram] bot_token_enc`、`[channels.googlechat] project_number`／`service_account_json_enc`，經 `agents.channels.*` 設定（管理員設定，負責人看得到狀態；見[專屬通道](per-employee-channels.md)） |
| 通道與整合 → 讓外部聯絡人用 @ 呼叫（v1.76） | `[channels] public_trigger`（只限管理員；見[用 @ 呼叫 AI 員工](../../features/zh-TW/62-calling-ai-staff.md)） |
| 自動化 → 記憶 → 決策延續、決定保留天數 | `[memory] decision_continuity`、`decision_ttl_days`（1 到 3650） |
| 進階 → 模型進階 → 進階鍵值 | 任意 `[區段] 鍵`，每列帶型別（字串、整數、小數、布林、字串陣列）；寫入前用完整的 `AgentConfig` 解析，失敗整筆拒絕。`agent`、`capabilities`、`container`、`permissions`、`channels`、`odoo`、`mcp`、`runtime`、`prompt`、`os_watch`、`budget` 區段不能在這裡改（v1.76.0：新增 `prompt`、`os_watch`、`budget`，裡面有只限管理員的設定，例如 `prompt.cli_bare_mode`，管理員改用設定檔進階編輯修改） |

單次執行時間上限與容器任務的逾時設定分開儲存。伺服器會拒絕非整數或超出 30～3600 秒的值；手動編輯檔案時，超出範圍的整數會調整到界線，型別錯誤則用預設值，兩者都會記錄警告。Antigravity 的外層最後上限另加 30 秒。OpenAI 相容 API 員工需要在 `utility` 或 `preferred` 指定模型；Codex 可使用 CLI 預設模型；Grok、Antigravity 與通用 CLI 員工改用全域背景設定。輔助模型與 runtime 的廠商家族不符時，畫面會提醒對話壓縮、事實擷取等背景工作將失敗。

提高 `timeout_secs` 的實際上限需要管理員。員工擁有者可降低上限；清空設定會還原為 300 秒，若因此高於目前上限，同樣需要管理員。

Claude 輔助呼叫使用 CLI 可用憑證走輕量路徑，不進入帳號輪替；一般對話仍保留原有帳號輪替。

輔助呼叫（含分類與評審）使用專用執行模式。Claude 使用無工具、單回合的輕量路徑；OpenAI 相容 API 只送出一次不帶工具的請求，不進入工具迴圈。Codex 使用受限模式：唯讀沙箱、不注入平台 MCP 設定、每次建立只有擁有者能存取的空白暫存工作目錄、忽略使用者設定，且不保存工作階段。呼叫結束後刪除暫存目錄，包含錯誤與逾時；系統提示放進 prompt，不寫入 `AGENTS.md`。明確指定 Codex 的 judge 與團隊 verifier 也使用此模式。Grok、Antigravity 與通用 CLI 仍不支援輔助執行，員工背景工作改用全域輔助後端／模型（預設 Claude／`claude-haiku-4-5`），每個員工／後端組合記錄一次警告。全域後端必須是 Claude、Codex 或 OpenAI 相容 API；不支援的全域後端或明確指定會被拒絕。既有評審上層可依降級策略，不帶不支援的指定重試，仍須遵守相同模式。Codex 可能保留內建讀取工具，剩餘風險見[安全防禦](../../features/zh-TW/05-security-defense.md)。

**只限管理員的欄位。** `[agent] reports_to`、`department`、`name`，整個 `[capabilities]`，`[container] sandbox_enabled`、`network_access`、`readonly_project`，`[permissions]` 的五個旗標（`can_modify_own_soul`、`can_create_agents`、`can_send_cross_agent`、`can_schedule_tasks`、`can_modify_own_skills`）與（v1.76.0）`[permissions] allowed_channels`，`[guardrails] enabled` 與 `block_secrets`，`[channels] public_trigger`（v1.76），以及（v1.76.0）各員工自己的 bot token `[channels.discord]`、`[channels.telegram]`、`[channels.slack]`、`[channels.googlechat]`、整個 `[odoo]`、`[runtime] provider` 與 `fallback`、`[prompt] cli_bare_mode`、整個 `[os_watch]`、`[model] account_pool`、設成或取消 `role = "main"`，還有放寬預算：調高 `monthly_limit_cents` 或 `daily_cap_cents`、拿掉上限（0 代表不限）、關閉 `hard_stop`。員工的負責人仍可以新增或調低上限、打開 `hard_stop`。非管理員送出改動會被拒絕，管理員改動成功後寫稽核事件 `agent_authority_changed`，被拒絕的嘗試寫 `agent_authority_refused`。換模型會自動切換執行環境時，非管理員會看到說明。`org.toml` 只在 `reports_to` 或 `department` 真的變了才更新。`agents.inspect` 以 `admin_only_fields` 回傳對應的表單欄位，編輯頁對非管理員停用這些欄位。

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
- **交辦**：寫下要完成什麼（和選填的完成標準），建立一個目標任務（`tasks.create`，`kind = "goal"`）。送出後留在這一頁並顯示「已交辦給【名字】」；主管目前還打不開部屬員工的任務內容。

資料來自 RPC `users.my_team`，和每一次呼叫、交辦用的是同一套判斷，所以列出來的都叫得到。個人版與一般員工呼叫這個 RPC 會被拒絕。

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

RPC `agent_files.list / get / diff / history / save / rollback / withdraw / pending_get`；員工編輯頁的「行為界線」也走同一套（`contract.update`）。檔案：人格設定（`SOUL.md`）、自我介紹（`IDENTITY.md`）、工作規則（`INSTRUCTIONS.md`）、行為界線（`CONTRACT.toml`）、基本設定（`agent.toml`，這裡只能看，機密遮蔽；要改請到編輯頁，管理員也可用設定檔進階編輯），以及這位員工自己的技能（`SKILLS/` 底下已經存在的 `.md`，檔案代號 `skill:<路徑>`，例如 `skill:quote/SKILL.md`）。技能在這裡只能修改，不能新增或刪除（新增請用自建技能流程），旁邊的程式檔不會被碰到；路徑每一層都必須是單一名稱、不能有 `..`、不能有看不見的字元（零寬、方向控制等）、最多四層；捷徑（symlink）與硬連結一律拒絕，存檔時在檔案鎖內確認資料夾沒有被換掉才寫入（鎖檔放在員工資料夾外）。其他寫技能檔的途徑（自建技能、員工自己的技能工具、技能畢業）不經過這裡，沒有版本紀錄，也不拿同一把鎖。上限：Markdown 與技能每檔 64 KB，`CONTRACT.toml` 32 KB，`must_not` 與 `must_always` 各最多 100 條、每條最多 500 字。

- **設定檔分頁：** 員工詳情頁的「設定檔」分頁（`/agents/<員工>/files`）。左邊是白話的檔案清單，小字灰色是實際檔名，標記「等待審核」「在系統外被改過」「你只能檢視」；右邊先以唯讀方式顯示，負責人與管理員按「編輯」後可以修改，下方顯示大小與上限，按鈕有「檢視差異」「儲存並檢查」「捨棄修改」。檢查結果用同一個確認對話框。存檔時若別人剛存過，會並排顯示你的內容與目前內容，可以「複製我的內容」再「重新載入」。「版本紀錄」（Operator 以上）列出時間、修改人與來源，可以檢視、和目前內容比較或還原。還沒存的修改在切換檔案、切換分頁或關閉頁面前會先詢問（從側邊欄等其他連結離開時目前不會詢問）。只能檢視的人會看到「你可以檢視；只有這位員工的負責人或管理員可以修改。」
- **工作規則與 AGENTS.md／Agent.md：** 清單與標題把「工作規則」寫成「工作規則（AGENTS.md／Agent.md）」，因為很多人用業界通用的名字找它；實際檔名仍是小字的 `INSTRUCTIONS.md`。匯入功能把 AGENTS.md、CLAUDE.md、GEMINI.md、Agent.md、copilot-instructions 的內容合併進這份檔案，每次對話都會交給員工，不論它用哪一種執行環境。新員工沒有這個檔案時，分頁顯示「建立工作規則」（只有負責人與管理員看得到按鈕），按下去帶出一段短範本，按「儲存並檢查」才會寫入，走的是同一條存檔、檢查與送審流程，沒有另開寫入路徑。
- **執行環境自動產生的檔案（唯讀）：** 分頁最下方收合的區塊，第一次展開才讀取，顯示建立員工時替它的執行環境產生的 `CLAUDE.md` 與 `AGENTS.md`（RPC `agent_files.runtime_files`，Viewer 以上；只列這兩個固定檔名，沿用員工資料夾的檢查，捷徑與非一般檔案一律不讀，每檔最多顯示 64 KB）。這裡不能修改，要改規則請改工作規則。
- **搜尋：** 命令面板（⌘K）輸入 `agent.md`、`agents.md`、`CLAUDE.md` 或「工作規則」，每位員工會有一列「<員工> · 設定檔」，直接開啟該員工的設定檔分頁。
- **誰能做什麼：** 綁定這位員工的人都能看檔案；Operator 以上能看版本與差異；員工的負責人與管理員能存檔。每次呼叫都重讀帳號資料庫，不看登入時的快照。審核只有管理員能決定。
- **每次存檔都檢查：** 先跑固定規則（大小、控制字元、嚴格解析 `CONTRACT.toml`、隱藏或看不見的內容、這次新增的文字或新的 `must_always` 裡的注入句型、像金鑰的文字、人格設定被刪掉大半、行為界線刪改 `must_not`／`must_always` 或調高／取消工具次數上限；技能另外檢查：開頭的 `---` 設定區要能照技能載入器的格式讀出，`name` 必須是單一名稱，技能掃描器在這次新增的行上找到的執行指令、外送資料、注入句型會升級為高風險（以實際新增的行判斷，同類的第二行照樣算新的），裸露的網址與提到行為界線禁止的事項只標為需要注意；原本就在檔案裡的高風險內容會列為「這個技能裡原本就有可疑內容」；技能在上次儲存後被系統外改過時整份重新檢查，並和上次在這裡存的版本比較），再請部署的輔助模型看一次差異（`[runtime] utility_provider`／`utility_model`，可用 `[agent_files] review_provider`／`review_model` 指定；費用記在部署的 `agent-file-review`，不扣員工預算）。模型只能讓結果更嚴。結果：`pass` 直接套用；`warn` 要寫理由（至少 4 個字）或送管理員審核；`block` 非管理員只能送審，管理員要寫理由並勾「我了解風險」才能套用（稽核記為 Critical）。檢查沒完成時（沒有輔助模型、逾時、回答無法使用、額度用完），或內容太長只檢查了一部分，結果至少是 `warn`（設 `[agent_files] on_analysis_failure = "block"` 則是 `block`），非管理員只能送管理員審核，管理員要勾「我了解風險」才能套用；不會默默放行，也不能關掉。每天的額度：整個部署 200 次、每人 40 次、每位員工 60 次（`review_daily_limit`、`review_daily_limit_per_user`、`review_daily_limit_per_agent`），另外每人每分鐘最多檢查 10 次（`checks_per_minute`）；同一份修改 15 分鐘內再檢查沿用上次結果。確認時不再跑一次模型；確認必須是同一人、同一檔、同一個起始版本與同樣內容，並在 15 分鐘內，固定規則用你現在的權限重跑（比檢查時嚴就要重新檢查），確認高風險的修改時會在檔案鎖內再確認一次你仍是管理員。在系統外被改過的 SOUL.md 或 INSTRUCTIONS.md 會整份重新檢查，並和上次核可的版本比較（原因「在系統外被改過」）。
- **存檔：** 在檔案鎖內比對編輯器讀到的雜湊（`base_hash`），期間被別人改過就當作衝突拒絕。新內容從員工的下一則對話開始生效，通道的下一則回覆會附一行提示。
- **版本：** 存在員工資料夾外的 `<home>/agent_file_history/<員工>/<檔案>/`（只有擁有者能讀），每檔預設保留 20 份（`[agent_files] history_keep`，5–100）。第一次修改也會留下修改前的內容。還原版本會跑同樣的檢查。刪除員工時版本整份移到 `agent_file_history/_trash/`，之後同一個代號的新員工從零開始。
- **核可基準：** 存 SOUL.md 或 INSTRUCTIONS.md 會更新核可指紋（INSTRUCTIONS.md 現在也有，heartbeat 以 `_instructions_integrity_drift` 回報）。在系統外被改過的檔案會標示出來，可以還原到上次核可版本，或檢查後接受目前內容（照原樣再存一次：整份重新檢查，至少要寫理由確認或送審，確認後不改寫檔案，只把目前內容記成一個版本並設為核可基準；沒有被改過的檔案照原樣存一次仍然是「沒有變動」）。員工改名會同步 SOUL.md／IDENTITY.md 裡的名字，原本核可的 SOUL 維持核可，原本已在系統外被改過的 SOUL 仍然標示。名字必須是純文字（1–64 字，不能換行、不能有看不見的字元或 `< > [ ] { }`、反引號），改名後的內容若被固定規則標出問題就不同步（列在 `identity_files_skipped`）。
- **送審：** 種類 `agent_file_change`，只能由目前仍是管理員的人在儀表板決定，期限 3 天（逾期視為拒絕）。通道只收通知、沒有按鈕。提出的內容存在 `<home>/agent_file_pending/`，申請結束就刪除。核准時先套用；申請期間檔案被改過，申請會被拒絕，需要申請者重新送出。
- **稽核：** `agent_file_changed`、`agent_file_change_refused`、`agent_file_review_failed`、`agent_file_change_requested`（只記雜湊，不記內容）。

## 排程交辦（v1.76.0）

交辦任務頁「複雜任務」分頁的「什麼時候做」可選指定時間一次、每天、平日、每週或每小時，管理員另有自訂 cron。時間以瀏覽器時區解讀；設定當下不建立任務，時間到才重新確認建立者權限並交辦。

「目標 → 我交辦的 → 排程」可暫停、恢復與刪除，管理員可切換看所有人的排程。暫停或刪除先於任務落地，就不會建立該輪任務；已落地的任務需另外停止。恢復會重驗建立者權限與目前執行間隔限制。

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
流程尚未驗證；這頁要搭配帶有維修帳號的 OS 映像才能使用，也不代表整機已驗收。

## 安全 → 憑證保護（v1.76.0）

「安全」頁的「憑證保護」卡片（管理員，兩版都有）顯示一行：「憑證保護：檔案」
或「憑證保護：系統金鑰圈（…）」，下面說明代表什麼。它只管讓 AI 員工代表特定人
的憑證；不論卡片顯示什麼，其他設定都照舊運作。

- **改放系統金鑰圈**（`security.credential_protection_enable`）在 macOS 金鑰圈
  或 Windows 認證管理員建立保護金鑰。Linux 與 Docker 顯示「這個平台不支援系統
  金鑰圈」，沒有按鈕。
- **需要重新授權**：macOS 升級後或金鑰圈鎖著時出現。在那台 Mac 的終端機執行
  `duduclaw credentials authorize`，允許一次對話框即可。
- **保護金鑰不見了／被更換／設定錯誤**：個人帳號停用。按**重新啟用**
  （`reset: true`）會產生新金鑰，已保存的個人憑證作廢，大家要重新連結。
- **換一把新金鑰**（同樣是 `reset: true`）在一切正常時也提供，用於懷疑金鑰和記錄
  一起被換掉的情況，效果和重新啟用相同。macOS 執行檔沒開 hardened runtime 時，那一行
  會顯示「同一個帳號的程式讀得到」，因為注入 DuDuClaw 的程式碼讀得到金鑰。
- **停用**（`security.credential_protection_disable`，`confirm: true`，需輸入
  確認文字）刪除金鑰，已保存的個人憑證作廢。

終端機用 `duduclaw credentials status`（腳本可加 `--json`）看同樣的狀態。細節見
[安全防護](../../features/zh-TW/05-security-defense.md#指定人的憑證放系統金鑰圈v1760)。

直接綁定歷史隱私：`[people] bound_history_privacy = true` 預設開啟，缺值或格式錯誤也維持限制。管理員可在進階編輯器明確設 `false` 過渡，恢復先前 Viewer／Operator 的廣泛歷史讀取。Owner／Admin 及聊天／呼叫權限不變；搜尋、匯出、附件與 NER 限制見功能 68。
