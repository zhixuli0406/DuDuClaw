# 組織圖編輯（回報對象與部門）

AI 員工向誰回報（`reports_to`）、屬於哪個部門，決定了誰可以把工作委派給誰。權威資料是 `<home>/org.toml`，任何 AI 員工都無法寫入；各員工 `agent.toml` 裡的同名欄位只是顯示用的副本。RM-027 新增了從儀表板調整這兩項的 gateway API。**組織圖頁面目前仍是唯讀，編輯介面會在之後的版本提供。**

## 誰可以調整

| 角色 | 結果 |
|------|------|
| 管理員 | 調整**立即生效**。儀表板會先顯示調整前後的確認畫面，送出時帶上畫面顯示時的 `base`；如果這段期間組織圖已被改過，這次調整會被拒絕，不寫入任何東西。每次套用都寫入稽核 `org_change_applied`，含調整前後與操作者。 |
| 經理 | 調整會成為**申請**，要等目前的管理員在審核中心核准後才生效。經理只能替自己可以交辦工作的 AI 員工提出（Operator／Owner 綁定，或 Operator 等級的團隊授權），新主管也必須是自己看得到的員工。把員工移到最上層（沒有主管）只能由管理員決定。 |
| 一般成員 | 不能調整組織圖。 |

每次呼叫都會重新從使用者資料庫讀取角色，降級在下一次請求就生效。AI 員工本身仍然無法修改自己的組織位置（`org_field_guard`）。

## 每次調整都要通過的規則

`agents.update`、每個 `org.*` 呼叫，以及管理員核准申請時，都會跑同一套檢查（`duduclaw_core::org_change::validate_org_change`）：

- 被調整的員工與新主管都必須存在。
- 員工不能向自己回報。
- 不能形成循環；沿著 `org.toml`（不是顯示副本）往上檢查，只存在於權威資料裡的循環也擋得住。
- 部門名稱必須合法；一旦已有任何部門，新部門必須已存在（有員工使用，或有共享知識／共享技能的部門資料夾，請先在「部門」建立）。清除部門一律允許。

`agents.update` 現在會用同樣的錯誤拒絕不合法的 `reports_to`（以前不檢查就寫入）；它的部門行為沒有改變。

儀表板的原始檔編輯器（`config.raw.set`，管理員）在 `agent.toml` 的 `[agent] reports_to` 或 `department` 被改動時，走同一條路徑：取得全域組織鎖，對權威資料做整棵樹的檢查（存在、自己、循環），先寫權威資料、再寫檔案；檔案寫入失敗時會把權威資料還原。檢查不通過時，這次原始檔儲存會連同驗證錯誤一起被拒絕，不寫入任何東西。沒有動到這兩個欄位的編輯，不會碰權威資料。

### 調整是怎麼寫入的

- **整棵樹共用一把鎖。** 所有組織寫入者都取得同一把跨行程的鎖 `<home>/locks/org-mutation.lock`，包含 `org.*` 呼叫、核准申請、`agents.update` 的組織欄位、MCP `agent_update` 的組織路徑，以及 `duduclaw org sync`。在鎖內，寫入者會重新讀取權威資料、比對 `base`、對整棵樹檢查規則，然後才提交。兩個相反方向的調整同時進行（A 掛到 B 底下、B 掛到 A 底下）不可能都通過。
- **權威資料必須可用。** 如果 `org.toml` 曾經建立過但現在遺失、無法讀取、不是合法 TOML，或是較新版 DuDuClaw 寫入的，所有組織調整都會以 `org_authority_unavailable` 拒絕。不會改用顯示副本代替，也不會從空的資料重建檔案。唯讀檢視（`org.placement`、`org.pending_changes`、組織圖）照常運作，`org.placement` 會在 `authority_error` 回報問題；儲存其他員工欄位也照常運作。只有 `duduclaw org sync` 能重建遺失或損壞的檔案，而且只能整份重建；損壞的原檔會保留為 `org.toml.corrupt`，較新版本的檔案絕不會被改寫。
- **先寫權威資料，再寫顯示副本。** 顯示副本寫入失敗時，調整仍然成立，因為權威資料才是準的。gateway 會記錄 `org_change_mirror_drift` 並回應 `mirror_sync_failed`。要重試，請再套用一次相同的位置，gateway 會回應 `status:"mirror_synced"`。這個狀態下**不要**執行 `duduclaw org sync`，它會把舊的顯示值複製回權威資料。`agents.update` 寫入 `agent.toml` 失敗時，會把先前的權威資料項目放回去。

### 對 AI 員工的保護

除了 `org.toml`，AI 員工也不能寫入、佔用或刪除 `<home>/locks/org-mutation.lock`，以及 `<home>/org_change_journal/` 底下的任何東西（檔案寫入防護與 shell 防護皆適用）。任何提到這個鎖檔的 shell 指令都會被拒絕，連 `flock … sleep` 這類看似唯讀的指令也一樣，因為佔住鎖會讓所有組織調整卡住。

## 申請與審核

- 每位 AI 員工同時只能有一筆申請在等待；第二筆會被拒絕（`pending_exists`，附上等待中那筆的編號）。
- 只能在儀表板決定。通訊軟體只收到沒有按鈕的通知，從通訊軟體送出的決定一律無效。
- 核准時，gateway 會重新檢查規則，並確認員工目前的位置仍是提出申請當時的位置；通過後才寫入權威資料與顯示副本，而且只有在寫入成功後才記錄決定。等待期間位置已被改過，或調整已不再合法，申請會被**拒絕**，不寫入任何東西。
- 調整已套用、但記錄決定失敗時，申請會維持等待狀態（`decision_unrecorded`），也不會寫最終稽核。已套用的結果連同審核編號存放在 `<home>/org_change_journal/`；再次核准會完成原本的審核，不會把調整套用第二次。對已套用的申請按拒絕會被擋下（`already_applied`）。稽核資料與通訊軟體的通知更新，都要等決定記錄成功後才寫入。
- 駁回不寫入任何東西。申請 3 天後逾期，逾期等同拒絕。
- 管理員直接調整一位有申請在等待的員工時，那筆申請會被撤回；已經套用、正等待重試的申請則不受影響。
- 每筆申請都會記錄提出者為 `on_behalf_of`。設定 `[approval] forbid_self_approval = true` 時，任何人（包含管理員）都不能核准自己提出的申請，申請裡記錄的提出者也會一併檢查；駁回自己的申請仍然允許。

稽核事件：`org_change_requested`、`org_change_applied`（含 `actor`；來自申請時另有 `approved_by` 與 `proposer`）、`org_change_rejected`（`why`：`rejected`、`expired`、`stale`、`invalid`、`malformed`）、`org_change_mirror_drift`、`org_change_mirror_resynced`、`org_change_decision_unrecorded`、`org_change_decision_mismatch`。

## Gateway RPC

四個 RPC 在個人版與多人團隊版都可用（組織圖是跨 AI 員工，不是跨人）。

| 方法 | 角色 | 參數 | 回傳 |
|------|------|------|------|
| `org.placement` | 經理以上 | `{agent_id}` | `{agent_id, placement:{reports_to, department}, source:"authority"\|"mirror", mirror_drift, base, authority_error}` |
| `org.apply_change` | 管理員 | `{agent_id, reports_to?, department?, expected_base}` | `{status:"applied", agent_id, before, after, base, registry_reloaded, withdrawn_pending:[id]}` |
| `org.propose_change` | 經理以上 | `{agent_id, reports_to?, department?, reason?}` | `{status:"pending_approval", approval_id, agent_id, before, after, base, expires_at}` |
| `org.pending_changes` | 經理以上 | `{agent_id?}` | `{count, pending:[{approval_id, agent_id, before, after, base, stale, proposer, proposer_email, reason, created_at, expires_at}]}` |

省略的欄位維持原值；空字串代表最上層／沒有部門。申請用既有的 `approvals.decide {id, approve, reason?}` 決定（審核種類 `org_change`）。規則檢查的錯誤是 `{code, message}`，`code` 為 `agent_not_found`、`self_report`、`target_not_found`、`cycle`、`invalid_department`、`unknown_department`、`no_change`、`root_requires_admin`、`pending_exists`、`stale_base`、`org_authority_unavailable`、`org_authority_write_failed`、`org_lock_unavailable`、`mirror_sync_failed`、`decision_unrecorded`、`already_applied` 之一；`stale_base` 另外附上 `current` 與 `current_base`。權限不足一律回傳 `permission denied` 字串。`org.pending_changes` 對經理只顯示他們自己提出的申請，以及他們可以調整的員工的申請；只有 Viewer 綁定的人看不到任何內容。

組織圖如何管控委派，請見[委派隔離](37-delegation-isolation.md)。
