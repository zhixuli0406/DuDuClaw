# DuDuClaw × Odoo ERP 整合規劃

> 版本：Phase 3 (v0.7.0) 規劃
> 日期：2026-03-23
> 狀態：已實作（2026-03-23）

---

## 一、設計原則

### 1.1 中間層架構（Middleware Bridge）

DuDuClaw **不直接依賴 Odoo**。所有整合透過獨立的中間層 `duduclaw-odoo-bridge` 完成：

```
┌──────────────────────────────────────────────────────┐
│  Claude Code (AI Brain)                              │
│  ↕ MCP Protocol                                     │
│  DuDuClaw (Plumbing)                                 │
│  ↕ MCP Tools: odoo_*                                │
├──────────────────────────────────────────────────────┤
│  duduclaw-odoo-bridge (中間層)                        │
│  ├─ OdooConnector: 連線池 + 認證 + 重試              │
│  ├─ ModelMapper: Odoo model ↔ DuDuClaw 型別轉換      │
│  ├─ EventBridge: Odoo webhook → DuDuClaw bus         │
│  └─ ConfigResolver: 自動偵測 CE/EE 功能差異          │
├──────────────────────────────────────────────────────┤
│  Odoo ERP                                            │
│  ├─ Community Edition (CE) — XML-RPC / JSON-RPC      │
│  └─ Enterprise Edition (EE) — 同上 + 額外模組        │
└──────────────────────────────────────────────────────┘
```

### 1.2 核心約束

| 約束 | 說明 |
|------|------|
| **社群版優先** | 所有功能必須在 CE 上可用；EE 功能為增強而非必要 |
| **零 Odoo 修改** | 不要求安裝自訂 Odoo 模組（但可選提供 `duduclaw_connector` 模組加速整合） |
| **中間層獨立** | `duduclaw-odoo-bridge` 是獨立 crate/package，可單獨測試和部署 |
| **雙協議支援** | 同時支援 XML-RPC（CE 預設）和 JSON-RPC（效能更好） |
| **安全邊界** | Odoo 憑證由 bridge 管理，不暴露給 AI agent |

---

## 二、Odoo API 能力矩陣

### 2.1 社群版 (CE) vs 企業版 (EE)

| 功能 | CE | EE | API 方式 |
|------|:--:|:--:|---------|
| CRM（潛在客戶/商機） | v | v | `crm.lead` XML-RPC |
| 銷售（報價/訂單） | v | v | `sale.order` XML-RPC |
| 庫存（產品/庫存移動） | v | v | `stock.picking` XML-RPC |
| 會計（發票/付款） | v | v | `account.move` XML-RPC |
| 專案管理（任務） | v | v | `project.task` XML-RPC |
| HR（員工/出勤） | v | v | `hr.employee` XML-RPC |
| 工作室（Studio 自訂欄位） | - | v | 同上（自動擴展 model） |
| 核准流程（Approval） | - | v | `approval.request` XML-RPC |
| 文件簽署（Sign） | - | v | `sign.request` XML-RPC |
| 行銷自動化 | - | v | `marketing.campaign` XML-RPC |
| IoT 整合 | - | v | 專用 API |
| 地圖路線規劃 | - | v | 專用 API |

### 2.2 API 端點

| 協議 | 端點 | 用途 | 認證 |
|------|------|------|------|
| **XML-RPC** | `/xmlrpc/2/common` | 認證（`authenticate`）、版本查詢 | 無（版本）/ 帳密 |
| **XML-RPC** | `/xmlrpc/2/object` | CRUD（`execute_kw`） | uid + 密碼/API Key |
| **XML-RPC** | `/xmlrpc/2/db` | 資料庫管理（Odoo.sh 被停用） | master password |
| **JSON-RPC** | `/jsonrpc` | 同上，JSON 格式（推薦，payload 更小） | uid + 密碼/API Key |
| **JSON-RPC** | `/web/session/authenticate` | Session 認證 | 帳密 → session cookie |
| **JSON-RPC** | `/web/dataset/call_kw` | ORM 呼叫（Web Client 用） | session cookie |
| **REST** | 需安裝 OCA `base_rest` 或 `odoo-fastapi` | RESTful 端點（CE 可用） | 自訂 |

#### JSON-RPC 呼叫範例

```json
{
    "jsonrpc": "2.0",
    "method": "call",
    "params": {
        "service": "object",
        "method": "execute_kw",
        "args": [
            "mydb",
            2,
            "api_key_here",
            "crm.lead",
            "search_read",
            [[["stage_id.name", "=", "Qualification"]]],
            {"fields": ["name", "email_from", "expected_revenue"], "limit": 20}
        ]
    },
    "id": 1
}
```

#### 可用 ORM 方法

| 方法 | 說明 | 用於 |
|------|------|------|
| `search` | 回傳符合條件的 record IDs | 分頁查詢 |
| `read` | 根據 IDs 讀取指定欄位 | 精確查詢 |
| `search_read` | search + read 合一（最常用） | 列表頁面 |
| `search_count` | 回傳符合條件的記錄數 | 統計/分頁 |
| `create` | 建立新記錄，回傳 ID | 新增 |
| `write` | 更新現有記錄 | 修改 |
| `unlink` | 刪除記錄 | 刪除 |
| `fields_get` | 取得模型欄位定義 | 動態 introspection |
| `name_get` | 取得記錄顯示名稱 | UI 呈現 |
| `default_get` | 取得欄位預設值 | 表單初始化 |

### 2.3 認證方式

| 方式 | CE | EE | 說明 |
|------|:--:|:--:|------|
| 帳號密碼 + DB 名稱 | v | v | `authenticate(db, user, pwd, {})` → `uid` |
| API Key | v (14+) | v | 在 Settings → Users → API Keys 產生，取代密碼使用（推薦） |
| OAuth2 | - | v | EE 內建；CE 需安裝 OCA `oauth2_provider` |
| Session Token | v | v | `/web/session/authenticate` → session cookie |

**推薦**：JSON-RPC + API Key — 單一端點 (`/jsonrpc`)、JSON payload、可撤銷、可多把。

### 2.4 效能最佳實踐

| 項目 | 做法 |
|------|------|
| **指定 fields** | 永遠傳 `fields` 參數，不指定會回傳所有欄位（含大型 HTML/二進位欄位）|
| **分頁** | 使用 `limit` + `offset`，預設無上限 |
| **批次操作** | `create` 和 `write` 支援 list 批次，避免迴圈逐筆呼叫 |
| **快取 uid** | `authenticate` 結果不變，快取不重複呼叫 |
| **context** | 傳 `{'lang': 'zh_TW'}` 控制語言和時區 |
| **連線數** | 不超過 Odoo worker 數量的並發請求（預設 2 workers）|
| **timeout** | XML-RPC 預設無 timeout，務必設定（建議 30s）|

### 2.5 部署差異

| 面向 | Odoo.sh | 自建主機 |
|------|---------|---------|
| HTTPS | 強制 | 自行設定 |
| `xmlrpc/2/db` | **被停用** | 可用（建議停用）|
| Worker 數量 | 依方案自動 | 自行調整 `--workers` |
| 直接 SQL | Production 禁止 | 完全存取 |
| Rate limit | Nginx 基礎保護 | 自行設定 |

---

## 三、中間層模組設計

### 3.1 模組架構

```
duduclaw-odoo-bridge/
├── src/
│   ├── lib.rs              # 公開 API
│   ├── connector.rs        # OdooConnector: 連線/認證/重試
│   ├── rpc.rs              # XML-RPC + JSON-RPC 雙協議客戶端
│   ├── models/             # Odoo model 對應
│   │   ├── crm.rs          # crm.lead, crm.stage
│   │   ├── sale.rs         # sale.order, sale.order.line
│   │   ├── inventory.rs    # product.product, stock.picking
│   │   ├── accounting.rs   # account.move, account.payment
│   │   ├── project.rs      # project.project, project.task
│   │   ├── hr.rs           # hr.employee, hr.attendance
│   │   └── common.rs       # res.partner, res.users
│   ├── mapper.rs           # Odoo 欄位 → DuDuClaw 型別轉換
│   ├── events.rs           # Odoo webhook → bus_queue 事件橋接
│   ├── edition.rs          # CE/EE 功能偵測與降級
│   └── config.rs           # Odoo 連線配置
├── Cargo.toml
└── tests/
    ├── mock_odoo.rs        # Mock XML-RPC server
    └── integration.rs      # 整合測試（需要真實 Odoo）
```

### 3.2 OdooConnector

```rust
pub struct OdooConnector {
    url: String,              // e.g. "https://mycompany.odoo.com"
    db: String,               // 資料庫名稱
    uid: Option<i64>,         // 認證後的 user ID
    session: Option<String>,  // JSON-RPC session token
    protocol: Protocol,       // XmlRpc | JsonRpc
    edition: Edition,         // Community | Enterprise | Unknown
    api_key: Option<String>,  // Odoo 17+ API key
    http: reqwest::Client,
}

pub enum Protocol { XmlRpc, JsonRpc }
pub enum Edition { Community, Enterprise, Unknown }

impl OdooConnector {
    /// 連線並認證，自動偵測 CE/EE
    pub async fn connect(config: &OdooConfig) -> Result<Self>;

    /// 通用 CRUD：search, read, create, write, unlink
    pub async fn execute_kw(
        &self, model: &str, method: &str,
        args: Vec<Value>, kwargs: Map<String, Value>,
    ) -> Result<Value>;

    /// 便捷方法
    pub async fn search_read(
        &self, model: &str, domain: Vec<Value>,
        fields: &[&str], limit: usize,
    ) -> Result<Vec<Value>>;

    /// 偵測 Odoo 版本和已安裝模組
    pub async fn detect_edition(&mut self) -> Result<Edition>;
}
```

### 3.3 Edition 功能偵測

```rust
impl OdooConnector {
    pub async fn detect_edition(&mut self) -> Result<Edition> {
        // 1. 查詢 Odoo 版本
        let version = self.call_common("version", vec![]).await?;

        // 2. 檢查 EE 專有模組是否已安裝
        let ee_modules = ["web_studio", "approvals", "sign", "marketing_automation"];
        let installed = self.search_read(
            "ir.module.module",
            vec![json!(["name", "in", ee_modules]), json!(["state", "=", "installed"])],
            &["name"], 10,
        ).await?;

        if installed.is_empty() {
            self.edition = Edition::Community;
        } else {
            self.edition = Edition::Enterprise;
        }
        Ok(self.edition.clone())
    }
}
```

### 3.4 Odoo Model 欄位對照表

#### CRM (`crm.lead`)

| Odoo 欄位 | 型別 | 說明 | MCP 映射 |
|-----------|------|------|---------|
| `name` | string | 商機名稱 | `name` |
| `partner_id` | many2one | 客戶 | `partner_name` |
| `email_from` | string | Email | `email` |
| `phone` | string | 電話 | `phone` |
| `expected_revenue` | float | 預期營收 | `expected_revenue` |
| `probability` | float | 成交機率 | `probability` |
| `stage_id` | many2one | 階段 | `stage` |
| `user_id` | many2one | 負責人 | `salesperson` |
| `team_id` | many2one | 銷售團隊 | `team` |
| `type` | selection | lead/opportunity | `type` |

#### 銷售 (`sale.order`)

| Odoo 欄位 | 型別 | 說明 | MCP 映射 |
|-----------|------|------|---------|
| `name` | string | 訂單編號 | `order_number` |
| `partner_id` | many2one | 客戶 | `customer` |
| `date_order` | datetime | 訂單日期 | `date` |
| `state` | selection | draft/sent/sale/done/cancel | `status` |
| `amount_total` | float | 總金額 | `total` |
| `order_line` | one2many | 明細行 | `lines[]` |
| `pricelist_id` | many2one | 價目表 | `pricelist` |

#### 庫存 (`stock.quant` + `product.product`)

| Odoo 欄位 | 型別 | 說明 | MCP 映射 |
|-----------|------|------|---------|
| `product_id` | many2one | 產品 | `product` |
| `quantity` | float | 在手量 | `qty_on_hand` |
| `reserved_quantity` | float | 保留量 | `qty_reserved` |
| `location_id` | many2one | 庫位 | `location` |

#### 會計 (`account.move`)

| Odoo 欄位 | 型別 | 說明 | MCP 映射 |
|-----------|------|------|---------|
| `name` | string | 編號 | `invoice_number` |
| `partner_id` | many2one | 對象 | `partner` |
| `move_type` | selection | out_invoice/in_invoice/... | `type` |
| `state` | selection | draft/posted/cancel | `status` |
| `amount_total` | float | 總金額 | `total` |
| `amount_residual` | float | 未付餘額 | `balance_due` |
| `payment_state` | selection | not_paid/partial/paid | `payment_status` |

#### 基礎 (`res.partner`)

| Odoo 欄位 | 型別 | 說明 | MCP 映射 |
|-----------|------|------|---------|
| `name` | string | 名稱 | `name` |
| `email` | string | Email | `email` |
| `phone` | string | 電話 | `phone` |
| `is_company` | boolean | 是否為公司 | `is_company` |
| `street` | string | 地址 | `address` |
| `country_id` | many2one | 國家 | `country` |

### 3.5 Model Mapper 實作範例

```rust
/// 將 Odoo JSON-RPC 回應轉換為 DuDuClaw 可讀格式
pub fn map_crm_lead(odoo_data: &Value) -> CrmLead {
    CrmLead {
        id: odoo_data["id"].as_i64().unwrap_or(0),
        name: odoo_data["name"].as_str().unwrap_or("").to_string(),
        contact_name: extract_many2one_name(&odoo_data["partner_id"]),
        email: odoo_data["email_from"].as_str().unwrap_or("").to_string(),
        phone: odoo_data["phone"].as_str().unwrap_or("").to_string(),
        stage: extract_many2one_name(&odoo_data["stage_id"]),
        expected_revenue: odoo_data["expected_revenue"].as_f64().unwrap_or(0.0),
        probability: odoo_data["probability"].as_f64().unwrap_or(0.0),
    }
}

/// Odoo many2one 欄位回傳格式為 [id, "name"] 或 false
fn extract_many2one_name(val: &Value) -> String {
    match val {
        Value::Array(arr) if arr.len() >= 2 => {
            arr[1].as_str().unwrap_or("").to_string()
        }
        _ => String::new(),
    }
}
```

---

## 四、MCP 工具設計（15 個工具）

### 4.1 連線管理

| 工具 | 說明 | CE | EE |
|------|------|:--:|:--:|
| `odoo_connect` | 建立 Odoo 連線並認證 | v | v |
| `odoo_status` | 查詢連線狀態、版本、已安裝模組 | v | v |

### 4.2 CRM

| 工具 | 說明 | CE | EE |
|------|------|:--:|:--:|
| `odoo_crm_leads` | 搜尋/列出潛在客戶 | v | v |
| `odoo_crm_create_lead` | 建立新潛在客戶 | v | v |
| `odoo_crm_update_stage` | 推進商機階段 | v | v |

### 4.3 銷售

| 工具 | 說明 | CE | EE |
|------|------|:--:|:--:|
| `odoo_sale_orders` | 搜尋/列出銷售訂單 | v | v |
| `odoo_sale_create_quotation` | 建立報價單 | v | v |
| `odoo_sale_confirm` | 確認報價為訂單 | v | v |

### 4.4 庫存

| 工具 | 說明 | CE | EE |
|------|------|:--:|:--:|
| `odoo_inventory_products` | 搜尋產品及庫存量 | v | v |
| `odoo_inventory_check` | 檢查特定產品的即時庫存 | v | v |

### 4.5 會計

| 工具 | 說明 | CE | EE |
|------|------|:--:|:--:|
| `odoo_invoice_list` | 列出發票（草稿/已過帳/已付款） | v | v |
| `odoo_payment_status` | 查詢付款狀態 | v | v |

### 4.6 通用

| 工具 | 說明 | CE | EE |
|------|------|:--:|:--:|
| `odoo_search` | 通用 model 搜尋（進階用戶） | v | v |
| `odoo_execute` | 通用 method 呼叫（進階用戶） | v | v |
| `odoo_report` | 生成並下載 PDF 報表 | v | v |

---

## 五、事件橋接（Event Bridge）

### 5.1 Odoo → DuDuClaw（推送）

Odoo 原生不支援 outbound webhook（需安裝模組或用 automated action）。Bridge 提供兩種方式：

#### 方式 A：Polling（零模組安裝）

```
CronScheduler (每 60 秒)
  → OdooConnector.search_read("crm.lead", [("write_date", ">", last_poll)])
  → 比對差異
  → 寫入 bus_queue.jsonl 作為事件
```

#### 方式 B：Webhook（需安裝 duduclaw_connector 模組）

```
Odoo Server Action (on write)
  → HTTP POST to DuDuClaw gateway /api/odoo/webhook
  → EventBridge 解析並路由到對應 agent
```

### 5.2 DuDuClaw → Odoo（操作）

```
Agent (透過 MCP 工具)
  → odoo_crm_create_lead(name="新客戶", email="...")
  → duduclaw-odoo-bridge.connector.execute_kw("crm.lead", "create", ...)
  → Odoo 建立記錄
  → 回傳 ID 給 Agent
```

### 5.3 事件類型

| 事件 | 觸發條件 | 通知目標 |
|------|---------|---------|
| `odoo.crm.lead_created` | 新潛在客戶建立 | Telegram/LINE 通知 |
| `odoo.crm.stage_changed` | 商機階段推進 | Agent 自動跟進 |
| `odoo.sale.order_confirmed` | 訂單確認 | Agent 通知倉庫 |
| `odoo.sale.payment_received` | 收到付款 | Agent 更新客戶記錄 |
| `odoo.inventory.low_stock` | 庫存低於安全量 | Agent 建議補貨 |
| `odoo.invoice.overdue` | 發票逾期 | Agent 發送催款通知 |

---

## 六、配置格式

### 6.1 `config.toml` 新增 `[odoo]` 區段

```toml
[odoo]
# 連線設定
url = "https://mycompany.odoo.com"
db = "mycompany"
protocol = "jsonrpc"      # "xmlrpc" | "jsonrpc"

# 認證（三選一）
auth_method = "api_key"   # "password" | "api_key" | "oauth2"
username = "admin"
# api_key_enc / password_enc 使用 AES-256-GCM 加密
api_key_enc = "..."
# 或
# password_enc = "..."

# 輪詢設定（方式 A）
poll_enabled = true
poll_interval_seconds = 60
poll_models = ["crm.lead", "sale.order", "account.move"]

# Webhook 設定（方式 B，需 duduclaw_connector 模組）
webhook_enabled = false
webhook_secret = "..."

# 功能開關
features_crm = true
features_sale = true
features_inventory = true
features_accounting = true
features_project = false
features_hr = false
```

### 6.2 Agent `agent.toml` 新增 Odoo 權限

```toml
[permissions]
# ... 現有欄位 ...
odoo_read = true           # 可查詢 Odoo 資料
odoo_write = false         # 可建立/修改 Odoo 記錄
odoo_allowed_models = ["crm.lead", "sale.order"]  # 限制可存取的 model
```

---

## 七、Dashboard 整合

### 7.1 Odoo 連線狀態卡片

在 Dashboard 首頁新增 Odoo 連線狀態：
- 連線狀態（已連線 / 未設定 / 錯誤）
- Odoo 版本 + 版次（CE/EE）
- 已啟用的模組數
- 上次同步時間

### 7.2 Odoo 頁面 (`/odoo`)

| 區塊 | 功能 |
|------|------|
| CRM Pipeline | 拖拉式看板，顯示商機階段 |
| 最近訂單 | 表格顯示最近 20 筆銷售訂單 |
| 庫存警報 | 低庫存產品清單 |
| 同步日誌 | 最近的 Odoo 事件 |

---

## 八、安全設計

### 8.1 憑證管理

- Odoo 密碼/API Key 使用與 Anthropic API key 相同的 AES-256-GCM 加密儲存
- 中間層 `OdooConnector` 持有解密後的 token，不暴露給 MCP 工具
- Agent 只能透過 MCP 工具操作 Odoo，不能直接取得連線 URL 或憑證

### 8.2 權限控制

- `odoo_read` / `odoo_write` 權限由 `agent.toml` 控制
- `odoo_allowed_models` 白名單限制 Agent 可存取的 Odoo model
- 寫入操作需 `odoo_write = true`，預設關閉
- 所有 Odoo 操作記錄到 `security_audit.jsonl`

### 8.3 輸入驗證

- 所有 MCP 工具參數經過 `input_guard` 掃描
- Odoo domain filter 中的值經過類型驗證（防止 injection）
- Model 名稱白名單驗證（防止存取敏感 model）

### 8.4 敏感 Model 黑名單

以下 Odoo model **永遠不允許**透過通用 `odoo_search` / `odoo_execute` 工具存取：

```rust
const BLOCKED_MODELS: &[&str] = &[
    "ir.config_parameter",   // 系統設定（含 master password hash）
    "res.users",             // 使用者帳號（含密碼 hash）
    "ir.cron",               // 排程任務（可執行 Python）
    "ir.actions.server",     // 伺服器動作（可執行 Python）
    "ir.rule",               // 存取規則
    "ir.model.access",       // 權限矩陣
    "base.automation",       // 自動化動作
    "fetchmail.server",      // 郵件伺服器（含密碼）
    "ir.mail_server",        // SMTP 伺服器（含密碼）
];
```

### 8.5 現有 Odoo MCP 生態

| 專案 | 說明 | 狀態 |
|------|------|------|
| `odoo-mcp-server` (GitHub) | 將 Odoo ORM 暴露為 MCP tools | 早期實驗性 |
| `mcp-odoo` | JSON-RPC 連接 Odoo 的 MCP server | 社群維護 |

DuDuClaw 的差異化：整合進完整的 Agent 編排系統（heartbeat、evolution、multi-channel），而非獨立的 MCP server。

---

## 九、CE/EE 功能降級策略

| 功能 | CE 行為 | EE 增強 |
|------|---------|---------|
| CRM Pipeline | 基本 stage 推進 | + 預測分數 + AI 建議 |
| 報表 | 基本 PDF 生成 | + 自訂報表 (Studio) |
| 核准流程 | MCP 工具回傳「需人工核准」 | + 自動建立核准請求 |
| 文件簽署 | 回傳「請手動簽署」 | + 自動發送簽署請求 |
| 行銷自動化 | 不可用（明確告知） | + 建立/查詢行銷活動 |

降級邏輯由 `edition.rs` 中的 `EditionGate` 處理：

```rust
pub struct EditionGate {
    edition: Edition,
    installed_modules: HashSet<String>,
}

impl EditionGate {
    pub fn can_use(&self, feature: &str) -> bool {
        match feature {
            "approval" => self.installed_modules.contains("approvals"),
            "sign" => self.installed_modules.contains("sign"),
            "studio" => self.installed_modules.contains("web_studio"),
            "marketing" => self.installed_modules.contains("marketing_automation"),
            _ => true, // CE 基本功能永遠可用
        }
    }

    pub fn fallback_message(&self, feature: &str) -> &str {
        match feature {
            "approval" => "此功能需要 Odoo Enterprise 的核准模組。請手動在 Odoo 中處理核准。",
            "sign" => "文件簽署需要 Odoo Enterprise Sign 模組。請手動寄送簽署連結。",
            _ => "此功能在您的 Odoo 版本中不可用。",
        }
    }
}
```

---

## 十、使用場景

### 場景 1：客服 Agent 自動建立 CRM Lead

```
[Telegram] 用戶：我想了解你們的產品 A
[DuDu Agent]：
  1. 回覆用戶：「收到！讓我為您建立一筆商機...」
  2. 呼叫 odoo_crm_create_lead(name="產品 A 詢問", contact_name="用戶名", source="telegram")
  3. 回覆用戶：「已為您建立商機 #1234，我們的業務團隊會盡快與您聯繫！」
```

### 場景 2：業務 Agent 每日商機摘要

```
[Cron: 每天 09:00]
DuDu Agent:
  1. odoo_crm_leads(domain=[("stage_id.name", "=", "Qualification")], limit=20)
  2. 整理清單
  3. send_message(channel="telegram", chat_id="業務群組", text="今日待跟進商機...")
```

### 場景 3：庫存預警自動通知

```
[EventBridge: odoo.inventory.low_stock]
DuDu Agent:
  1. 收到低庫存事件
  2. odoo_inventory_check(product_id=42)
  3. odoo_sale_create_quotation(vendor_id=5, product_id=42, quantity=100)
  4. send_message(channel="line", chat_id="採購主管", text="產品 X 庫存不足，已建立補貨報價單 #PO-2026-0042")
```

---

## 十一、實作階段

### Phase 3A — 基礎連線（4 項）

- [x] **[O-1a]** 實作 `OdooConnector` + XML-RPC/JSON-RPC 雙協議
- [x] **[O-1b]** 實作 CE/EE 自動偵測 + `EditionGate`
- [x] **[O-1c]** 實作 `config.toml [odoo]` 區段解析 + 加密憑證
- [x] **[O-1d]** MCP 工具：`odoo_connect` + `odoo_status`

### Phase 3B — CRM + 銷售（5 項）

- [x] **[O-2a]** Model mapper: `crm.lead`, `crm.stage`
- [x] **[O-2b]** MCP 工具：`odoo_crm_leads`, `odoo_crm_create_lead`, `odoo_crm_update_stage`
- [x] **[O-2c]** Model mapper: `sale.order`, `sale.order.line`
- [x] **[O-2d]** MCP 工具：`odoo_sale_orders`, `odoo_sale_create_quotation`, `odoo_sale_confirm`
- [x] **[O-2e]** Agent 權限控制：`odoo_read` / `odoo_write` / `odoo_allowed_models`

### Phase 3C — 庫存 + 會計（4 項）

- [x] **[O-3a]** Model mapper: `product.product`, `stock.picking`, `stock.quant`
- [x] **[O-3b]** MCP 工具：`odoo_inventory_products`, `odoo_inventory_check`
- [x] **[O-3c]** Model mapper: `account.move`, `account.payment`
- [x] **[O-3d]** MCP 工具：`odoo_invoice_list`, `odoo_payment_status`

### Phase 3D — 事件橋接 + Dashboard（5 項）

- [x] **[O-4a]** Polling 事件橋接（CronScheduler 整合）
- [x] **[O-4b]** Webhook 接收端點 (`/api/odoo/webhook`)
- [x] **[O-4c]** 6 種事件類型路由到 bus_queue
- [x] **[O-4d]** Dashboard Odoo 連線狀態卡片
- [x] **[O-4e]** Dashboard Odoo 頁面（CRM pipeline + 訂單 + 庫存警報）

### Phase 3E — 通用工具 + 報表（3 項）

- [x] **[O-5a]** MCP 工具：`odoo_search`, `odoo_execute`
- [x] **[O-5b]** MCP 工具：`odoo_report`（PDF 生成）
- [x] **[O-5c]** 可選 `duduclaw_connector` Odoo 模組（Python）

---

## 十二、技術選型

| 項目 | 選擇 | 理由 |
|------|------|------|
| XML-RPC 客戶端 | `xmlrpc` crate | Rust 原生，輕量 |
| JSON-RPC 客戶端 | `reqwest` + 手動封裝 | 已有依賴，無需新增 |
| Odoo 模組語言 | Python | Odoo 原生框架 |
| 事件輪詢 | 複用 `CronScheduler` | 已有排程基礎設施 |
| Webhook 接收 | 複用 `axum` 路由 | 已有 gateway 伺服器 |

---

## 十三、相容性矩陣

| Odoo 版本 | 支援狀態 | 備註 |
|-----------|---------|------|
| 18.0 (最新) | 完整支援 | 首要目標 |
| 17.0 | 完整支援 | API Key 原生支援 |
| 16.0 | 基本支援 | 需帳號密碼認證 |
| 15.0 | 有限支援 | 部分 model 欄位不同 |
| < 15.0 | 不支援 | API 差異過大 |
| Odoo.sh | 完整支援 | 同 self-hosted API |
| Odoo Online | 完整支援 | 需開啟外部 API |

---

## 十四、實作記錄

### 2026-03-23 — Phase 3 全部完成

**Build 狀態**：PASS (`cargo check` 零錯誤)

#### 新增 crate：`duduclaw-odoo`

| 檔案 | 功能 | 對應任務 |
|------|------|---------|
| `src/config.rs` | `[odoo]` config 解析 | O-1c |
| `src/rpc.rs` | JSON-RPC 雙協議客戶端 | O-1a |
| `src/connector.rs` | 連線池 + 認證 + CRUD + 黑名單 | O-1a |
| `src/edition.rs` | CE/EE 偵測 + EditionGate | O-1b |
| `src/events.rs` | PollTracker + WebhookPayload + EventBridge | O-4a/4b/4c |
| `src/models/crm.rs` | CrmLead mapper | O-2a |
| `src/models/sale.rs` | SaleOrder mapper | O-2c |
| `src/models/inventory.rs` | Product + StockQuant mapper | O-3a |
| `src/models/accounting.rs` | Invoice mapper | O-3c |
| `src/models/common.rs` | Partner + many2one helpers | 共用 |

#### MCP 工具（15 個）

| 工具 | 對應任務 |
|------|---------|
| `odoo_connect` | O-1d |
| `odoo_status` | O-1d |
| `odoo_crm_leads` | O-2b |
| `odoo_crm_create_lead` | O-2b |
| `odoo_crm_update_stage` | O-2b |
| `odoo_sale_orders` | O-2d |
| `odoo_sale_create_quotation` | O-2d |
| `odoo_sale_confirm` | O-2d |
| `odoo_inventory_products` | O-3b |
| `odoo_inventory_check` | O-3b |
| `odoo_invoice_list` | O-3d |
| `odoo_payment_status` | O-3d |
| `odoo_search` | O-5a |
| `odoo_execute` | O-5a |
| `odoo_report` | O-5b |
