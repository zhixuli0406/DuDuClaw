# 帳號輪替使用教學

> 適用版本：v0.6.5+

DuDuClaw 支援 **OAuth 訂閱帳號** 和 **API Key** 兩種認證方式的混合輪替。

---

## 快速開始

### 情境 1：我只有一個 Claude 訂閱帳號（最簡單）

不需要任何設定。DuDuClaw 會自動偵測 `~/.claude/.credentials.json` 中的 OAuth session。

```bash
# 確認已登入
claude auth status

# 直接啟動
duduclaw run
```

### 情境 2：我有一個 API Key

```bash
# 在 onboard 時設定
duduclaw onboard

# 或手動編輯 config.toml
vim ~/.duduclaw/config.toml
```

```toml
[api]
anthropic_api_key_enc = "..."  # duduclaw onboard 會自動加密
```

### 情境 3：我有多個帳號想輪替（推薦）

編輯 `~/.duduclaw/config.toml`：

```toml
# ── 帳號設定 ────────────────────────────────

# 帳號 1：Claude Max 訂閱（優先使用，零額外費用）
[[accounts]]
id = "my-subscription"
type = "oauth"
profile = "default"           # 使用 ~/.claude/.credentials.json
email = "me@example.com"
subscription = "max"
priority = 1                  # 數字越小越優先

# 帳號 2：團隊的 Claude Team 帳號
[[accounts]]
id = "team-account"
type = "oauth"
profile = "work"              # 使用 ~/.claude/profiles/work/.credentials.json
email = "me@company.com"
subscription = "team"
priority = 2

# 帳號 3：API Key 備用（訂閱額度用完時自動切換）
[[accounts]]
id = "api-backup"
type = "api_key"
api_key_enc = "..."           # 用 duduclaw onboard 加密
priority = 10                 # 最低優先（僅當 OAuth 全部不可用時使用）
monthly_budget_cents = 5000   # 月度上限 $50

# ── 輪替策略 ────────────────────────────────

[rotation]
strategy = "least_cost"                     # 優先使用 OAuth（零成本）
cooldown_after_rate_limit_seconds = 120     # Rate limit 時冷卻 2 分鐘
```

---

## 認證方式詳解

### OAuth 帳號（訂閱制）

適用於 Claude Pro / Team / Max 訂閱用戶。

**優點**：
- 不按 token 收費（訂閱包含的用量內免費）
- `LeastCost` 策略下會被優先選擇
- 自動偵測，零設定即可使用

**設定方式**：

```bash
# 方式 A：使用預設帳號（自動偵測，不需設定）
claude auth login

# 方式 B：建立多個 Profile
mkdir -p ~/.claude/profiles/work
CLAUDE_CONFIG_DIR=~/.claude/profiles/work claude auth login
```

**config.toml 設定**：

```toml
[[accounts]]
id = "personal"
type = "oauth"
profile = "default"    # default = ~/.claude/.credentials.json
priority = 1

[[accounts]]
id = "work"
type = "oauth"
profile = "work"       # ~/.claude/profiles/work/.credentials.json
priority = 2
```

### API Key 帳號（按量付費）

適用於直接使用 Anthropic API 的用戶。

**優點**：
- 精確的成本追蹤和預算控制
- 多把 key 分散風險
- 適合高吞吐量場景

**設定方式**：

```bash
# 透過 onboard 加密存入（推薦）
duduclaw onboard

# 或手動加密
# 1. 確保 ~/.duduclaw/.keyfile 存在
# 2. 在 config.toml 中設定 api_key_enc
```

```toml
[[accounts]]
id = "primary-key"
type = "api_key"
api_key_enc = "base64_encrypted_key_here"
priority = 5
monthly_budget_cents = 10000   # $100/月

[[accounts]]
id = "backup-key"
type = "api_key"
api_key_enc = "another_encrypted_key"
priority = 6
monthly_budget_cents = 5000    # $50/月
```

---

## 輪替策略

| 策略 | 行為 | 適用場景 |
|------|------|---------|
| `priority` | 按 priority 數字排序，使用最小值 | 有明確主備關係 |
| `least_cost` | **OAuth 優先**（零成本），再選最少花費的 API key | 混合帳號（推薦） |
| `failover` | 和 priority 相同，失敗才換下一個 | 穩定性優先 |
| `round_robin` | 循環輪流使用所有帳號 | 均勻分散負載 |

### 推薦設定

```toml
[rotation]
strategy = "least_cost"                    # OAuth 零成本，自動優先
cooldown_after_rate_limit_seconds = 120    # Rate limit 冷卻 2 分鐘
```

---

## 健康追蹤與容錯

### 自動容錯流程

```
選擇帳號 → 呼叫 claude CLI
  ├─ 成功 → 記錄花費，重置錯誤計數
  ├─ Rate Limit (429) → 進入冷卻期，嘗試下一個帳號
  └─ 其他錯誤 → 錯誤計數 +1
       └─ 連續 3 次失敗 → 標記為不健康，跳過該帳號
```

### OAuth Token 過期處理

DuDuClaw 會檢查 `expiresAt` 時間戳，過期的 OAuth session 會被標記為不可用。

解決方式：
```bash
claude auth login   # 重新登入取得新 token
```

---

## 監控與管理

### Dashboard

在 Web Dashboard 的「帳號與預算」頁面可以看到：
- 每個帳號的認證方式（OAuth / API Key）
- 健康狀態（健康 / 降級 / 不健康）
- 當月花費和預算
- 總請求數

### WebSocket RPC

```json
{"type":"req","id":"1","method":"accounts.list","params":{}}
```

回傳範例：
```json
{
  "accounts": [
    {
      "id": "my-subscription",
      "auth_method": "oauth",
      "priority": 1,
      "is_healthy": true,
      "is_available": true,
      "email": "me@example.com",
      "subscription": "max",
      "spent_this_month": 0,
      "monthly_budget_cents": 0,
      "total_requests": 42
    },
    {
      "id": "api-backup",
      "auth_method": "apikey",
      "priority": 10,
      "is_healthy": true,
      "is_available": true,
      "spent_this_month": 1250,
      "monthly_budget_cents": 5000,
      "total_requests": 8
    }
  ]
}
```

### 手動輪替測試

```json
{"type":"req","id":"2","method":"accounts.rotate","params":{}}
```

---

## 常見問題

### Q: 我沒有設定任何帳號，DuDuClaw 能用嗎？

可以。載入順序：
1. `config.toml` 中的 `[[accounts]]`
2. 自動偵測 `~/.claude/.credentials.json`（`claude auth login` 後就有）
3. `config.toml` 中的 `[api]` 單一 key
4. `ANTHROPIC_API_KEY` 環境變數

只要有任何一個來源有效，就能使用。

### Q: OAuth 和 API Key 可以混合使用嗎？

可以，這是推薦的設定方式。`least_cost` 策略會自動優先使用 OAuth（零成本），API Key 作為備用。

### Q: Rate Limit 時會怎樣？

被限速的帳號會進入冷卻期（預設 120 秒），期間自動切換到下一個可用帳號。冷卻結束後自動恢復。

### Q: 預算超限時會怎樣？

API Key 帳號的 `monthly_budget_cents` 超限時，該帳號會被跳過（不再被選中），直到下月重置。OAuth 帳號沒有預算限制。

### Q: 如何新增一個 OAuth Profile？

```bash
# 建立 profile 目錄
mkdir -p ~/.claude/profiles/work

# 在該 profile 下登入
CLAUDE_CONFIG_DIR=~/.claude/profiles/work claude auth login

# 在 config.toml 加入
# [[accounts]]
# id = "work"
# type = "oauth"
# profile = "work"
# priority = 2
```
