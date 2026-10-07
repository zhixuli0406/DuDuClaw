# DuDuClaw 授權說明 / Licensing

DuDuClaw 採用 **Apache License 2.0** 開源授權。

本 repo 的程式碼全部採 Apache 2.0，可自由使用、修改、分發。執行檔裡有少數行為依授權金鑰（license key）的方案而定，見下方「授權金鑰控管的項目」。付費產業包等商業內容放在 `commercial/`，不在本 repo。

商用授權、企業導入、培訓與支援由授權總經銷 [未來企業股份有限公司](https://www.futurecorp.tw/) 提供，原廠不直接販售。

---

## 開源模組（Apache 2.0）

以下模組完全開源，可自由使用、修改、分發，包含商業用途：

| 模組 | 說明 |
|------|------|
| `duduclaw-core` | 共用型別、設定與 runtime 目錄 |
| `duduclaw-gateway` | HTTP/WebSocket 伺服器、頻道路由、派工與排程 |
| `duduclaw-agent` | Agent 設定、心跳排程、預算追蹤、帳號輪替 |
| `duduclaw-memory` | SQLite + FTS5 記憶引擎 |
| `duduclaw-security` | AES-256-GCM 加密、SOUL.md 守衛、輸入掃描 |
| `duduclaw-inference` | 本地推論引擎（OpenAI 相容 HTTP 後端、llamafile 管理） |
| `duduclaw-container` | 腳本沙箱的容器後端（Docker；Windows 先試 WSL2） |
| `duduclaw-sandbox` | 原生 OS 行程限制（macOS Seatbelt / Linux Landlock） |
| `duduclaw-cli` | CLI 入口、MCP Server、安全測試 |
| `duduclaw-cli-runtime` | 一次性 PTY 呼叫（給需要終端機的 CLI） |
| `duduclaw-llm` | 統一 LLM API 層與 MCP 用戶端 |
| `duduclaw-dashboard` | rust-embed 靜態資源容器 |
| `duduclaw-odoo` | Odoo ERP JSON-RPC 橋接 |
| `duduclaw-db` | 唯讀資料庫連接器（PostgreSQL / MySQL / SQLite） |
| `duduclaw-redaction` | 敏感資料去識別化 |
| `duduclaw-identity` | 身分解析 |
| `duduclaw-auth` | 儀表板帳號登入（Argon2id + JWT） |
| `duduclaw-license` | 授權金鑰解析、驗證與功能閘 |
| `duduclaw-fork` | Live Forking 平行分支 |
| `duduclaw-os` | OS 環境整合（檔案監看、原生通知） |
| `duduclaw-desktop` | 原生桌面控制（滑鼠、鍵盤、截圖） |
| `duduclaw-pets` | 照片轉桌面寵物 |
| `duduclaw-relay` | 雲端 webhook 中繼與區網裝置探索 |
| `duduclaw-sysd` | 值班機映像的特權系統服務 |
| `web/` | React 19 Dashboard |
| `python/` | Python SDK（`pip install duduclaw`） |
| `npm/` | npm 安裝包裝 |

---

## 授權金鑰控管的項目

Apache 2.0 授權條款本身不限制使用方式。以下是執行檔依授權金鑰方案改變行為的地方（方案定義見 `crates/duduclaw-license/features.toml`）：

| 項目 | 行為 |
|------|------|
| `premium_templates` | 安裝付費產業包與付費團隊劇本。studio、business、partner、personal_pro_self_host、self_host_pro、oem 方案可用 |
| `white_label` | 修改產品名稱、logo 等品牌設定。只有 oem 方案可用 |
| `industry_evolution_params` | 列在方案功能表中（business、partner、self_host_pro、oem）；本 repo 的程式碼沒有功能檢查這一項 |
| 通道回覆尾註 | opensource 與 hobby 方案（以及沒有授權檔的安裝）在外部通道的回覆後附上「— Powered by DuDuClaw 🐾」；其他方案可用 `config.toml [branding] reply_footer = false` 關閉 |
| AI 員工數上限 | 雲端方案（hobby、solo 各 1 個，studio 3 個）在雲端部署時生效；授權金鑰若帶有簽章的 `max_agents`，自架部署也會套用。其他情況不限數量 |

方案功能表中的其他布林值（客服管道、修補時效等服務承諾）只用於顯示，不會開關任何程式功能。

---

## 常見問題

### 我是個人開發者，可以免費用嗎？

**可以。** Apache 2.0 允許個人自由使用、修改、部署。

### 我在公司內部用，需要付費嗎？

**不用。** Apache 2.0 不限制公司內部使用。開源版執行檔可直接使用；付費授權只影響上方「授權金鑰控管的項目」。

### 我可以拿去開 SaaS 嗎？

**可以。** Apache 2.0 不限制託管服務。但「DuDuClaw」名稱及爪印 Logo 為商標，不可冒用。

### 我 Fork 後改名字商業化，可以嗎？

**可以。** Apache 2.0 允許衍生作品，但需保留原始授權聲明和版權通知。

### 教育 / 研究用途呢？

**完全免費。** Apache 2.0 不限制這類用途。

---

## 聯絡

授權相關問題請聯繫：

- GitHub Issues: [DuDuClaw/issues](https://github.com/zhixuli0406/DuDuClaw/issues)
- Email: louis.li@dudustudio.monster
