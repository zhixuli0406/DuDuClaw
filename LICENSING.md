# DuDuClaw 授權說明 / Licensing FAQ

DuDuClaw 採用 **Open Core** 授權模式。

- **核心程式碼**（本 repository）：[Apache License 2.0](LICENSE)
- **商業加值模組**（`commercial/` 目錄，不在公開 repo 中）：閉源商業授權

---

## 開源部分（Apache 2.0）

以下模組完全開源，可自由使用、修改、分發，包含商業用途：

| 模組 | 說明 |
|------|------|
| `duduclaw-core` | 共用型別與 trait |
| `duduclaw-gateway` | HTTP/WebSocket 伺服器、頻道路由 |
| `duduclaw-agent` | Agent 設定、心跳排程、預算追蹤 |
| `duduclaw-memory` | SQLite + FTS5 記憶引擎 |
| `duduclaw-security` | AES-256-GCM 加密、SOUL.md 守衛、輸入掃描 |
| `duduclaw-inference` | 本地推論引擎（llama.cpp / mistral.rs / Exo） |
| `duduclaw-container` | Docker / Apple Container 沙箱 |
| `duduclaw-bus` | tokio broadcast + mpsc 訊息匯流排 |
| `duduclaw-cli` | CLI 入口、MCP Server、安全測試 |
| `duduclaw-dashboard` | rust-embed 靜態資源容器 |
| `duduclaw-odoo` | Odoo ERP JSON-RPC 橋接 |
| `duduclaw-bridge` | Python 互操作 |
| `web/` | React 19 Dashboard（全功能） |

## 商業加值模組（閉源，獨立 Repository）

Pro/Enterprise 功能位於獨立的私有 repository (`duduclaw-pro`)，
**不修改 CE 的任何功能**，而是透過 `GatewayExtension` trait 注入額外功能：

| 模組 | 說明 |
|------|------|
| `duduclaw-license` | Ed25519 授權驗證 |
| `duduclaw-pro-gateway` | Pro RPC 方法 (`pro.templates.*`, `pro.audit.*`, `pro.report.*`) |
| `duduclaw-pro-evolution` | GVU 自適應深度 (3-7 輪) + 產業參數集 |
| `duduclaw-pro-audit` | PDF/CSV 稽核匯出 |
| `duduclaw-pro-report` | ROI 分析 + 對話品質趨勢 |
| `duduclaw-pro-dashboard` | Pro Dashboard 擴展頁面 |
| `templates/` | 10+ 產業 SOUL.md 模板 |
| `evolution-params/` | GVU 最佳參數集 |

Pro binary (`duduclaw-pro`) 是 CE 的超集 — 包含所有 CE 功能加上上述增值模組。

---

## 常見問題

### 我是個人開發者，可以免費用嗎？

**可以。** Apache 2.0 允許個人自由使用、修改、部署。

### 我在公司內部用，需要付費嗎？

**不用。** 核心功能完全免費，無任何限制。

### 我可以拿去開 SaaS 嗎？

**可以。** Apache 2.0 不限制託管服務。但「DuDuClaw」名稱及爪印 Logo 為商標，不可冒用。

### 我 Fork 後改名字商業化，可以嗎？

**可以。** Apache 2.0 允許衍生作品，但需保留原始授權聲明和版權通知。

### 付費模組有什麼好處？

- **產業模板**：經實戰驗證的 SOUL.md，開箱即用
- **演化參數**：GVU 調校過的最佳參數，省去數週實驗時間
- **企業報表**：ROI 分析、稽核匯出，符合企業合規需求
- **優先支援**：SLA 保證回應時間 + 安全補丁即時推送

### 教育 / 研究用途呢？

**完全免費。** 核心程式碼無任何限制。

---

## 授權等級

| 等級 | 費用 | 內容 |
|------|------|------|
| **Community** | 免費 | 全部核心功能（Apache 2.0 開源） |
| **Pro** | 付費 | Community + 產業模板 + 演化參數 + 即時安全補丁 |
| **Enterprise** | 付費 | Pro + 企業報表 + SLA 支援 + 客製化諮詢 |

---

## 聯絡

授權相關問題請聯繫：

- GitHub Issues: [DuDuClaw/issues](https://github.com/zhixuli0406/DuDuClaw/issues)
- Email: （待補）
