# 治理層（Governance Layer）

> 建築法規加上水電錶——以宣告式 YAML 政策守住每個 Agent 的每個動作，fail-safe，且免重啟即可熱重載。

---

## 比喻：建築法規與水電錶

一棟現代建築仰賴兩種治理。

**建築法規**決定你*被允許*做什麼。你可以加蓋陽台，但不能敲掉承重牆。有些變更你自己動手就好；有些（重接總電源）則需要簽核的許可證與檢查員。規則白紙黑字、公開張貼，除非授予特定例外，否則人人適用。

**水電錶**決定你被允許*消耗*多少。你的電錶不在乎你*做*什麼——它只追蹤總用量對上每月額度。越過軟性門檻，帳單上會跳出警告；越過硬性上限，供電就被切斷，直到下個計費週期重置電錶為止。

DuDuClaw 的治理層就是這兩道閘門，立在每個 Agent 之前。**Permission 與 Lifecycle 政策**是建築法規——Agent 可以做什麼、什麼需要核准。**Rate 與 Quota 政策**是錶——Agent 在被限速或切斷前可以消耗多快、多少。兩者皆以純 YAML 宣告，皆於修改時熱重載，且當規則手冊缺失或格式錯誤時皆 fail closed。

---

## 四種政策類型

`duduclaw-governance` crate 將所有治理建模為單一帶標籤枚舉 `PolicyType`，共四個變體：

| 政策類型 | 治理對象 | 關鍵欄位 | 預設處置 |
|----------|----------|----------|----------|
| **Rate**（`RatePolicy`） | 單位時間內的操作次數 | `resource`、`limit`、`window_seconds`、`action_on_violation` | `reject` |
| **Permission**（`PermissionPolicy`） | Agent 可用的 scope | `allowed_scopes`、`denied_scopes`、`requires_approval` | denied 優先 |
| **Quota**（`QuotaPolicy`） | 每日消耗預算 | `daily_token_budget`、`max_concurrent_tasks`、`max_memory_entries`、`reset_cron` | 於 `00:00` UTC 重置 |
| **Lifecycle**（`LifecyclePolicy`） | Agent 健康與閒置行為 | `max_idle_hours`、`health_check_interval_seconds`、`auto_suspend_on_violation_count` | 自動暫停 |

每個變體都帶有 `policy_id`（唯一識別碼）與 `agent_id`——其中 `"*"` 代表該政策作用於**所有** Agent。每個變體都實作 `validate()`，非法政策（例如 `limit: 0`）會被拒絕，而非默默接受。

---

## 政策解析：Agent 覆寫 Global

`PolicyRegistry` 從目錄載入政策，並逐 Agent 解析。優先序嚴格且單向：

```
解析順序（優先序由高至低）
─────────────────────────────────────────
  policies/{agent_id}.yaml      ← Agent 專屬覆寫
        ↓ 覆寫
  policies/global.yaml          ← 全域預設（"*"）
        ↓ 覆寫
  系統程式碼內建預設
```

呼叫 `get_policies_for_agent("alice")` 時，registry：

```
1. 收集 "alice" 的 Agent 專屬政策
2. 收集全域政策（agent_id = "*"）
3. 對每條全域政策：
     是否存在具有相同
     (policy_id, type_name) 的 Agent 政策？
        ├─ 是 → 跳過全域（Agent 版本勝出）
        └─ 否 → 繼承全域
4. 回傳合併後的清單
```

去重鍵是 **(policy_id, type_name)**——而非只看 `policy_id`。這很關鍵：某 Agent 名為 `shared` 的 `permission` 政策，絕不可抹掉一條剛好也叫 `shared` 的全域 `quota` 政策。只有相同 id *且*相同 type 的 Agent 政策才會覆寫其全域對應版本，否則兩者並存。

---

## Fail-Safe 載入：跳過非法、保留有效

當一條政策格式錯誤就整個崩潰的治理系統，比沒有還糟——它會讓整道閘門失效。registry 以 **fail-safe** 方式載入：壞政策連同警告一起跳過，其餘每條有效政策照常運作。

```
load() 走訪 policies/
     |
     v
對每個 *.yaml 檔案：
     |
     ├─ 解析 YAML
     │     ├─ 解析錯誤 → 警告 + 跳過整個檔案
     │     └─ 成功 ↓
     |
     ├─ 對檔案中每條政策：
     │     ├─ validate() 失敗 → 警告 + 跳過「這條」政策
     │     └─ validate() 成功 → 保留
     |
     v
檔名 stem = agent_id
     └─ stem 未通過 [a-zA-Z0-9\-_] 檢查
           → 警告 + 跳過檔案（防路徑穿越）
```

具體而言：一份 `global.yaml` 含一條 `limit: 0`（非法）與一條 `limit: 100`（有效）的政策，最終恰好載入**一條**——好的那條。一份完全無法解析的 `global.yaml` 載入**零條**政策，系統以空治理規則繼續運作而非 panic。目錄不存在也不算錯誤，同樣產生空政策集。

檔名防護是一道安全邊界：agent_id 來自檔名，因此名為 `../evil.yaml` 或 `agent.name.yaml` 的檔案，在其 `file_stem` 被當作查詢鍵之前就會被拒絕。

---

## 免重啟熱重載

registry 可用 `notify` 監聽其目錄（Linux 上是 inotify，macOS/Windows 上有對應實作）。當 YAML 檔案被建立、修改或刪除時，registry 非同步重新載入：

```
operator 編輯 policies/global.yaml
     |
     v
notify 觸發 Create/Modify/Remove 事件
     |
     v
事件路徑以 .yaml 結尾？
     ├─ 否 → 忽略
     └─ 是 → tokio::spawn(registry.load())
              "Policy file changed, reloading..."
```

重載走的是同一條 fail-safe 路徑，因此線上運行期間的錯誤編輯會優雅降級——壞檔被跳過，記憶體中先前的有效集只會被新的有效集取代。免重啟、零停機、不會留下半套規則手冊。

---

## Quota：軟性追蹤、硬性執行

`QuotaManager` 是一個獨立的逐 Agent 用量追蹤器，供 evaluator 查詢。它對照 `QuotaPolicy` 執行三個消耗維度，且執行是**硬性**的——越過預算回傳帶型別的 `QuotaError`，而非警告。

```
consume_tokens(agent, policy, tokens)
     |
     v
needs_reset()？（Utc::now() >= reset_at）
     ├─ 是 → 重置計數器、排定下一個 00:00 UTC、
     │        發射 governance_quota_reset 事件
     └─ 否 ↓
     |
     v
new_total = token_used + tokens
     |
     ├─ new_total > daily_token_budget
     │     → Err(TokenBudgetExhausted { used, budget })   ← 硬性中止
     │
     └─ new_total <= daily_token_budget
           → token_used = new_total                        ← OK
```

邊界很精確：唯有當累計總量會**嚴格超過**預算時，該次消耗才被拒絕。剛好*達到*預算是被允許的——最後一個合法 token 仍塞得下。但一旦 `token_used >= budget`，`check_token_budget()` 即回報耗盡，後續任何正值消耗都無法成功。兩種視角保持一致。

三個 quota 維度：

| 維度 | 方法 | 越界時的錯誤 |
|------|------|--------------|
| 每日 token | `consume_tokens` / `check_token_budget` | `TokenBudgetExhausted { used, budget }` |
| 並發任務 | `increment_concurrent_tasks`（與 `decrement_concurrent_tasks` 成對） | `ConcurrentTasksExceeded { current, max }` |
| 記憶條目 | `set_memory_entries` | `MemoryEntriesExceeded { current, max }` |

計數器逐 Agent 且彼此隔離——`agent-a` 耗盡自己的預算絕不會碰到 `agent-b`。每日重置時間計算為明天的 `00:00` UTC；重置時會以 fire-and-forget 方式發射 `governance_quota_reset` 稽核事件（`reset_type` 為 `daily`、`manual` 或整批的 `"*"`）。

---

## 錯誤碼：從違規到 HTTP

當 evaluator 拒絕某操作時，違規會被對應到一個穩定的 `PolicyErrorCode` 與固定的 HTTP 狀態——讓呼叫端與 dashboard 得到一致、可機器解析的回應：

| 錯誤碼 | HTTP | 說明 |
|--------|------|------|
| `POLICY_RATE_EXCEEDED` | 403 | 速率限制超限 |
| `POLICY_PERMISSION_DENIED` | 403 | scope 不被允許 |
| `POLICY_QUOTA_EXCEEDED` | 403 | 每日配額耗盡 |
| `POLICY_LIFECYCLE_VIOLATION` | 403 | 違反生命週期政策 |
| `POLICY_NOT_FOUND` | 404 | 政策不存在 |
| `POLICY_CONFLICT` | 409 | 政策衝突（同 id、不同 type） |
| `POLICY_INVALID_SCHEMA` | 422 | schema 驗證失敗 |
| `POLICY_APPROVAL_REQUIRED` | 202 | 操作已排入核准佇列（非錯誤） |

請注意 `POLICY_APPROVAL_REQUIRED` 是 `202 Accepted`，而非 4xx——需要核准的操作是*等待中*，不是*被拒絕*。一個僅觸發 `warn` 等級速率規則但仍被允許的操作，則完全不產生 API 錯誤。

---

## 一份完整的政策檔案

隨附的 `policies/global.yaml` 宣告了每個 Agent 都會繼承的預設規則集——也在同一份檔案中示範了全部四種類型：

```yaml
policies:
  # ── Rate：每分鐘 200 次 MCP 呼叫 ──
  - policy_type: rate
    policy_id: default-rate-mcp
    agent_id: "*"
    resource: mcp_calls
    limit: 200
    window_seconds: 60
    action_on_violation: reject

  # ── Quota：每日 500k tokens、5 個並發任務 ──
  - policy_type: quota
    policy_id: default-quota-daily
    agent_id: "*"
    daily_token_budget: 500000
    max_concurrent_tasks: 5
    max_memory_entries: 10000
    reset_cron: "0 0 * * *"

  # ── Permission：拒絕 admin，agent CRUD 需核准 ──
  - policy_type: permission
    policy_id: default-permission
    agent_id: "*"
    allowed_scopes:
      - memory:read
      - memory:write
      - wiki:read
      - wiki:write
      - messaging:send
      - mcp:call
    denied_scopes:
      - admin
      - governance:write
    requires_approval:
      - agent:create
      - agent:modify
      - agent:remove

  # ── Lifecycle：閒置 48 小時自動暫停 ──
  - policy_type: lifecycle
    policy_id: default-lifecycle
    agent_id: "*"
    max_idle_hours: 48
    health_check_interval_seconds: 300
    auto_suspend_on_violation_count: 10
```

要單獨提高某個 Agent 的 MCP 速率上限，放一份 `policies/duduclaw-eng-infra.yaml`，內含一條 `limit: 500` 的 `default-rate-mcp` 政策即可——它只為該 Agent 覆寫全域上限，而全域的 permission 與 quota 政策仍照常繼承。

---

## 為什麼這很重要

### 宣告式，而非寫死

限制值住在 YAML 裡，而非散落在程式碼各處的 magic number。`global.yaml` 是每個預設上限的唯一真實來源——可稽核、納入版本控制、且免重新編譯即可編輯。

### 設計即 Fail-Safe

治理遵循本專案「安全閘門 fail closed」的規則。格式錯誤的政策被跳過而非致命；缺失的規則手冊產生空規則而非 panic。一次錯誤編輯絕不會讓整道閘門離線。

### 免分叉即可覆寫

逐 Agent 檔案以 `(policy_id, type)` 覆寫全域預設——強力 Agent 拿到較高上限，隔離中的 Agent 拿到較嚴上限，兩者皆不需動到共用的 `global.yaml`。同 id 不同 type 的政策共存，而非互相抹除。

### 一致且可機器解析的拒絕

每個違規都對應到唯一穩定的錯誤碼與 HTTP 狀態。dashboard、MCP 客戶端與稽核日誌看到的是同一套詞彙——`POLICY_QUOTA_EXCEEDED` 永遠表示同一件事、永遠回傳 403。

### 線上維運的熱重載

事故期間收緊速率上限只是編輯存檔，而非重新部署。watcher 接住變更，新上限在下一次評估即生效。

---

## 總結

一棟建築同時需要規則手冊（你可以蓋什麼）與錶（你可以用多少）。DuDuClaw 的治理層兩者兼具，立在每個 Agent 之前：Permission 與 Lifecycle 政策說明*什麼被允許*，Rate 與 Quota 政策說明*允許多少*。全部以 fail-safe 的 YAML 宣告、以 Agent 覆寫全域的方式解析、免重啟即可重載，並以一致的錯誤詞彙拒絕。這道閘門讓你能大規模運行不可信、會自我演化的 Agent，而無須盲目信任它們。
