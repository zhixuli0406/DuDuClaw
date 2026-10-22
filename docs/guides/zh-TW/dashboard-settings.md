# 儀表板設定對照（v1.68.0）

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

`[general] log_level`（常用 → 一般設定 → 日誌等級）存檔後立即套用；只有在環境變數 `RUST_LOG` 已設定時無法套用，這時回報需要重啟。帳號輪替策略與限流冷卻時間存檔後會清掉輪替快取，下一次呼叫就用新設定（之前最多延遲 30 分鐘）。

## 通道管理

- 群組「只回提及」（`mention_only`，每個群組各自設定）：v1.76 起，LINE 群組開啟後只回應用 `@` 叫 AI 員工（或要 @ 名單）的訊息，以及被點名員工正在等的那一則；以前開了會整個群組沒有回應。Telegram 共用 bot 也接受員工名字。見[用 @ 呼叫 AI 員工](../../features/zh-TW/62-calling-ai-staff.md)。
- 網站聊天元件：「開放網站聊天元件」寫 `[webchat] public_widget`，「元件金鑰」寫 `[webchat] widget_key`（16 到 256 個可見 ASCII 字元，有「產生」按鈕；開啟元件時必須有金鑰；金鑰不會在回應中回傳）。立即生效。元件金鑰以明文存在 `config.toml`：它是公開的金鑰，會出現在網站的頁面原始碼裡，所以不當成密鑰處理。
- WhatsApp、飛書、Google Chat、Teams、企業微信、釘釘：六條 webhook 路由一律掛著。通道設定好之前回 404；設定好之後會驗證平台的簽章，簽章錯誤回 401。`channels.add` 不必重啟就會啟動通道，`channels.add` 回 `hot_started` 與 `restart_required: false`；憑證不完整時附 `not_started_reason`。個別員工的 Slack bot 也會在新增時直接啟動。

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
| 工具與權限 → 需要把關的工具（一律等人核可、視為不可逆、先由判官判斷、需任務授權） | `[capabilities] approval_required_tools`、`irreversible_tools`、`maybe_irreversible_tools`、`scoped_tools` |
| 工具與權限 → 平行分支 | `[fork] enabled` |
| 工具與權限 → 出站內容防護 | `[guardrails] enabled`、`block_secrets`、`block_injection_echo`、`redact_pii`、`deny_phrases` |
| 工具與權限 → 權限 | `[permissions]` 四個旗標（見下方） |
| 腦袋與引擎 → 推理力度 | `[model] effort` |
| 腦袋與引擎 → AI 後端／備援後端 | `[runtime] provider`／`fallback`，新增 qwen、kimi、copilot、kiro、cursor、vibe、opencode |
| 腦袋與引擎 → 精簡啟動內容 | `[runtime] minimal_context` |
| 腦袋與引擎 → 一員工四角色 | `[team] enabled`、`[team.roles.<角色>]` |
| 自動化 → 夜間整理 | `[night_engine] enabled` |
| 通道與整合 → 讓外部聯絡人用 @ 呼叫（v1.76） | `[channels] public_trigger`（只限管理員；見[用 @ 呼叫 AI 員工](../../features/zh-TW/62-calling-ai-staff.md)） |
| 自動化 → 記憶 → 決策延續、決定保留天數 | `[memory] decision_continuity`、`decision_ttl_days`（1 到 3650） |
| 進階 → 模型進階 → 進階鍵值 | 任意 `[區段] 鍵`，每列帶型別（字串、整數、小數、布林、字串陣列）；寫入前用完整的 `AgentConfig` 解析，失敗整筆拒絕。`agent`、`capabilities`、`container`、`permissions`、`channels`、`odoo`、`mcp`、`runtime` 區段不能在這裡改 |

**只限管理員的欄位。** `[agent] reports_to`、`department`、`name`，整個 `[capabilities]`，`[channels] public_trigger`（v1.76），`[container] sandbox_enabled`、`network_access`，以及 `[permissions] can_modify_own_soul`：非管理員送出改動會被拒絕，管理員改動成功後寫稽核事件 `agent_authority_changed`，被拒絕的嘗試寫 `agent_authority_refused`。`org.toml` 只在 `reports_to` 或 `department` 真的變了才更新。

**權限旗標開始生效（行為變更）。** `can_create_agents`、`can_send_cross_agent`、`can_modify_own_skills`、`can_schedule_tasks` 以前沒有任何讀取端。v1.68.0 起，寫成 `false` 的旗標會在 MCP 分派閘擋下對應工具（`create_agent`；`send_to_agent`、`spawn_agent`；`create_reminder`、帶 `schedule` 的 `tasks_create`；`skill_hub_install`、`shared_skill_adopt`、`skill_graduate`、`skill_pin`、`skill_from_recording`），沒寫或型別錯誤時放行。因為舊範本常把這些旗標寫成 `false`，gateway 升級後第一次開機會對每位員工做一次遷移：`[permissions]` 裡沒有 `permissions_enforced_since` 標記的檔案，四個旗標中的 `false` 一律改成 `true`，並加上 `permissions_enforced_since = "1.68.0"`，每次重設寫稽核事件 `permission_flags_reset`。之後在這頁或設定檔進階編輯寫入的 `false` 才算操作者的決定。臨時角色成員（`agents/.ephemeral/`）不在遷移範圍內，維持最小權限。

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

## 設定檔進階編輯

位置：系統設定 → 進階設定 → 設定檔進階編輯。只有管理員看得到，RPC 是 `config.raw.get` / `config.raw.set`。

- **可編輯的檔案：**系統設定（`config.toml`）、推理設定（`inference.toml`），以及每位 AI 員工的 `agent.toml`（必須是 `agents/` 底下的實際目錄，不接受符號連結）。
- **密鑰遮罩：**鍵名以 `_enc` 結尾、名為 `key` 或以 `_key` 結尾、或含 token、secret、password、passwd、api_key、apikey、widget_key、private_key、credential、service_account_json 的值，以及 `headers`、`otlp_headers`、`env` 表格裡的每個值、帶密碼的網址，都顯示成 `«set»`。存檔時原樣保留 `«set»` 就保留原值；原本沒有值卻寫 `«set»` 會被拒絕。`[mcp_keys]` 整段不顯示。
- **驗證：**先檢查 TOML 語法，錯誤會指出第幾行第幾欄；再用對應的型別（系統設定的各區段驗證、`InferenceConfig`、`AgentConfig`）檢查，失敗就不寫入。不能在這裡改員工的 `[agent] name`。
- **文件知識庫：**集合、文件、分享與存取紀錄在側邊欄「公司 → 文件知識庫」管理；`[knowledge]` 這些全域上限還沒有設定頁，它的各個鍵（`enabled`、`max_source_bytes`、`max_source_chars`、`max_sources_per_collection`、`max_collections`、`max_chunks_total`、`extract_timeout_secs`、`extract_memory_mb`、`access_log_retention_days`、`max_collection_bytes`、`max_total_bytes`，以及 `extract_when_unavailable` = `refuse`／`run_unsandboxed`）在這裡會檢查型別。`[knowledge.embedding]` 的各個鍵（`provider` = `local`／`openai`／`gemini`／`none`、`local_model`、`cloud_model`、`cloud_account`、`cloud_base_url`、`allow_private_endpoint`、`batch_size`、`threads`、`max_vector_rows`、`idle_unload_minutes`）也一樣；揭露確認紀錄 `cloud_disclosure_ack` 不能在這裡改（存檔會被拒絕），只能經由「文件知識庫搜尋」的揭露對話框寫入。員工的 `[knowledge] collections` 在「AI 員工 → 編輯設定 → 工具與權限 → 文件知識庫」設定（只能選已分享給它的集合），也可以直接改該員工的 `agent.toml`。詳見[文件知識庫](../../features/zh-TW/63-knowledge-base.md)。
- **衝突：**開啟時取得檔案內容的雜湊值，存檔時帶回。檔案在這段時間被改過就拒絕，請重新載入再改。
- **備份：**寫入前備份成 `<檔名>.bak-<Unix 時間>`（權限 0600），保留最近 5 份。
- **稽核：**每次寫入記 `config_raw_edited`。改到 `[delegation]`、`[acp]`，或員工的 `[agent]`、`[capabilities]`、`sandbox_enabled`、`network_access`、`can_modify_own_soul` 時等級為 Warning。
- **重啟清單：**存檔後列出需要重啟的部分。`config.toml` 的 `[gateway]`、`[server]`、`[telemetry]`、`[logging]`、`[channels]`、`[wiki]`、`[relay]`、`[decision]` 只在開機時讀；`general.name`、`rotation.health_check_interval_seconds`、`goal_loop.resume_on_restart`、`knowledge.max_source_bytes`（上傳路由的大小上限依它決定）也要重啟；日誌等級、常駐感知、去識別化能立即套用時就不列。`inference.toml` 存檔後一律立即重設推理引擎。員工檔案只有 `heartbeat.max_concurrent_runs` 需要重啟。

## 維修 SSH 儀表板（尚未發版）

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
流程尚未驗證；這是尚未發版的介面，不代表整機已驗收。
