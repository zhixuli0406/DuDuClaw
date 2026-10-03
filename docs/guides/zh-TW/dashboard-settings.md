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

- 網站聊天元件：「開放網站聊天元件」寫 `[webchat] public_widget`，「元件金鑰」寫 `[webchat] widget_key`（16 到 256 個可見 ASCII 字元，有「產生」按鈕；開啟元件時必須有金鑰；金鑰不會在回應中回傳）。立即生效。元件金鑰以明文存在 `config.toml`：它是公開的金鑰，會出現在網站的頁面原始碼裡，所以不當成密鑰處理。
- WhatsApp、飛書、Google Chat、Teams、企業微信、釘釘：六條 webhook 路由一律掛著。通道設定好之前回 404；設定好之後會驗證平台的簽章，簽章錯誤回 401。`channels.add` 不必重啟就會啟動通道，`channels.add` 回 `hot_started` 與 `restart_required: false`；憑證不完整時附 `not_started_reason`。個別員工的 Slack bot 也會在新增時直接啟動。

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
| 自動化 → 記憶 → 決策延續、決定保留天數 | `[memory] decision_continuity`、`decision_ttl_days`（1 到 3650） |
| 進階 → 模型進階 → 進階鍵值 | 任意 `[區段] 鍵`，每列帶型別（字串、整數、小數、布林、字串陣列）；寫入前用完整的 `AgentConfig` 解析，失敗整筆拒絕。`agent`、`capabilities`、`container`、`permissions`、`channels`、`odoo`、`mcp`、`runtime` 區段不能在這裡改 |

**只限管理員的欄位。** `[agent] reports_to`、`department`、`name`，整個 `[capabilities]`，`[container] sandbox_enabled`、`network_access`，以及 `[permissions] can_modify_own_soul`：非管理員送出改動會被拒絕，管理員改動成功後寫稽核事件 `agent_authority_changed`，被拒絕的嘗試寫 `agent_authority_refused`。`org.toml` 只在 `reports_to` 或 `department` 真的變了才更新。

**權限旗標開始生效（行為變更）。** `can_create_agents`、`can_send_cross_agent`、`can_modify_own_skills`、`can_schedule_tasks` 以前沒有任何讀取端。v1.68.0 起，寫成 `false` 的旗標會在 MCP 分派閘擋下對應工具（`create_agent`；`send_to_agent`、`spawn_agent`；`schedule_task`、`create_reminder`、帶 `schedule` 的 `tasks_create`；`skill_hub_install`、`shared_skill_adopt`、`skill_graduate`、`skill_pin`、`skill_from_recording`），沒寫或型別錯誤時放行。因為舊範本常把這些旗標寫成 `false`，gateway 升級後第一次開機會對每位員工做一次遷移：`[permissions]` 裡沒有 `permissions_enforced_since` 標記的檔案，四個旗標中的 `false` 一律改成 `true`，並加上 `permissions_enforced_since = "1.68.0"`，每次重設寫稽核事件 `permission_flags_reset`。之後在這頁或設定檔進階編輯寫入的 `false` 才算操作者的決定。臨時角色成員（`agents/.ephemeral/`）不在遷移範圍內，維持最小權限。

## 設定檔進階編輯

位置：系統設定 → 進階設定 → 設定檔進階編輯。只有管理員看得到，RPC 是 `config.raw.get` / `config.raw.set`。

- **可編輯的檔案：**系統設定（`config.toml`）、推理設定（`inference.toml`），以及每位 AI 員工的 `agent.toml`（必須是 `agents/` 底下的實際目錄，不接受符號連結）。
- **密鑰遮罩：**鍵名以 `_enc` 結尾、名為 `key` 或以 `_key` 結尾、或含 token、secret、password、passwd、api_key、apikey、widget_key、private_key、credential、service_account_json 的值，以及 `headers`、`otlp_headers`、`env` 表格裡的每個值、帶密碼的網址，都顯示成 `«set»`。存檔時原樣保留 `«set»` 就保留原值；原本沒有值卻寫 `«set»` 會被拒絕。`[mcp_keys]` 整段不顯示。
- **驗證：**先檢查 TOML 語法，錯誤會指出第幾行第幾欄；再用對應的型別（系統設定的各區段驗證、`InferenceConfig`、`AgentConfig`）檢查，失敗就不寫入。不能在這裡改員工的 `[agent] name`。
- **衝突：**開啟時取得檔案內容的雜湊值，存檔時帶回。檔案在這段時間被改過就拒絕，請重新載入再改。
- **備份：**寫入前備份成 `<檔名>.bak-<Unix 時間>`（權限 0600），保留最近 5 份。
- **稽核：**每次寫入記 `config_raw_edited`。改到 `[delegation]`、`[acp]`，或員工的 `[agent]`、`[capabilities]`、`sandbox_enabled`、`network_access`、`can_modify_own_soul` 時等級為 Warning。
- **重啟清單：**存檔後列出需要重啟的部分。`config.toml` 的 `[gateway]`、`[server]`、`[telemetry]`、`[logging]`、`[channels]`、`[wiki]`、`[relay]`、`[decision]` 只在開機時讀；`general.name`、`rotation.health_check_interval_seconds`、`goal_loop.resume_on_restart` 也要重啟；日誌等級、常駐感知、去識別化能立即套用時就不列。`inference.toml` 存檔後一律立即重設推理引擎。員工檔案只有 `heartbeat.max_concurrent_runs` 需要重啟。
