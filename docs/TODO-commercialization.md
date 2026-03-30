# TODO: DuDuClaw 商業化實作規劃

> 對應 [business-plan.md](./business-plan.md) 的技術實作項目
> Priority: P0 = 立即（M1-2）、P1 = 短期（M3-6）、P2 = 中期（M6-12）、P3 = 長期（Y2+）
> Last updated: 2026-03-30 | Project version: v0.9.6

---

## Phase 0: 本週立即啟動（Week 1-2）

> 這些行動優先於任何產品開發，應立即啟動

### 0.0 授權與智財保護 [P0-NOW]

- [x] ~~**授權模式變更**~~ ✅ 已完成
  - [x] `LICENSE` — 已採用 **Elastic License 2.0 (ELv2)**（比原計畫 BSL 1.1 更嚴格，禁止託管服務）
  - [x] `LICENSING.md` — 已完成中英文 FAQ（個人/企業內部免費、託管服務需商業授權）
  - [x] `Cargo.toml` license = "Elastic-2.0"
  - [x] README.md License badge 已更新

- [x] ~~**Open Core 邊界定義**~~ ✅ 已完成
  - [x] `commercial/` 目錄已建立（含 `.gitkeep`）
  - [x] ~~填充商業閉源子目錄~~ ✅ 目錄架構 + README 已建立
    - [x] `commercial/duduclaw-license/README.md` — 授權驗證 crate 規劃書
    - [x] `commercial/templates-premium/README.md` — 產業 SOUL.md 調校版規劃書
    - [x] `commercial/evolution-params/README.md` — Evolution 最佳參數集規劃書
    - [x] `commercial/dashboard-enterprise/README.md` — ROI 報表、稽核匯出規劃書
  - [x] ~~安全補丁發布 SOP 文件~~ ✅ `docs/security-patch-sop.md` 已建立

- [ ] **商標註冊啟動**
  - [ ] 確認「DuDuClaw」+ 爪印 Logo 在智慧財產局無衝突
  - [ ] 提交商標申請（類別 9: 電腦軟體、類別 42: 軟體即服務）
  - [ ] 預算：NT$6,000-10,000（政府規費 + 代辦）

### 0.0.1 內容分層策略 [P0-NOW]

- [x] ~~**內容分類規則文件**~~ ✅ 已完成
  - [x] `docs/content-policy.md` — 三層分類已定義（公開/半公開/付費專屬）

- [ ] **YouTube 內容規則**（待開始產出內容時執行）
  - [ ] 每支影片結尾固定 CTA：LINE OA QR Code + 官網連結
  - [ ] Pro/Enterprise 功能：只展示效果畫面，不教完整設定步驟
  - [ ] Evolution 演化：展示 SOUL.md 自動改善的前後對比，不教參數
  - [ ] Odoo 整合：展示「詢價 → 自動查價目表 → 回覆」流程，不教設定

- [ ] **客戶黏著機制實作**
  - [ ] 自動更新：付費版 `duduclaw update` 即時取得最新版 + 安全補丁
  - [ ] 模型庫訂閱概念：持續更新產業專用 GGUF 模型（含在維護合約中）
  - [ ] SOUL.md 模板市集：經實戰驗證的 Agent 人格，持續新增
  - [ ] 數據飛輪提醒：Dashboard 顯示「您的 Agent 已累積 N 筆記憶，演化 M 次」

---

### 0.1 用自己的產品推廣自己（Dog-fooding）[P0-NOW]

- [ ] **LINE 官方帳號**
  - [ ] 建立 DuDuClaw LINE OA（企業版）
  - [ ] 部署 DuDuClaw Agent 為官方客服（24/7 自動回覆）
  - [ ] SOUL.md 設定：產品顧問人格 + 能回答功能/定價/安裝問題
  - [ ] Memory 載入：FAQ、功能矩陣、安裝指南、常見錯誤排除
  - [ ] 加入自動導購流程：體驗 → 體驗營報名 → 導入諮詢
  - [ ] LINE OA 圖文選單設計（功能介紹 / 立即體驗 / 聯絡我們）
  - [ ] 話術範例：「你正在跟 DuDuClaw Agent 對話 — 它運行在一台 Mac Mini 上、零 API 成本。想要一套一樣的嗎？」

- [ ] **Discord 社群**
  - [ ] 建立 DuDuClaw Discord Server
  - [ ] 頻道規劃：#announcements / #general / #support / #show-and-tell / #feature-requests
  - [ ] 部署 DuDuClaw Agent 自動歡迎新成員 + FAQ 回覆
  - [ ] 設定 Agent 引導新成員至安裝文件

- [ ] **Telegram 群組**
  - [ ] 建立 DuDuClaw TW 群組
  - [ ] 部署 Agent 即時回答技術問題
  - [ ] 設定雙語支援（zh-TW + en）

### 0.2 GitHub 病毒式引爆 [P0-NOW]

- [ ] **README.md 強化**（現有基礎已佳，需補強以下項目）
  - [x] ~~Badge 牆：License / Version / CI / Rust / Python~~ ✅ 已有
  - [x] ~~安裝指令（Homebrew / one-liner / source build）~~ ✅ 已有
  - [x] ~~核心特色表格~~ ✅ 已有完整 feature table
  - [x] ~~架構圖 + Agent 目錄結構~~ ✅ 已有
  - [ ] 首屏：30 秒 GIF Demo（LINE 對話 → Agent 回覆 → Evolution 演化 → 零成本）
  - [ ] Star History 動態圖表（star-history.com embed）
  - [ ] 對比表：DuDuClaw vs OpenClaw vs NanoClaw vs MicroClaw
  - [ ] Discord 在線人數 badge（待 Discord 建立後加入）

- [ ] **Awesome Lists 收錄**
  - [ ] 提交 PR 至 `awesome-rust`
  - [ ] 提交 PR 至 `awesome-ai-agents`
  - [ ] 提交 PR 至 `awesome-mcp-servers`
  - [ ] 提交 PR 至 `awesome-claude`（如存在）
  - [ ] 提交 PR 至 `awesome-self-hosted`
  - [ ] 提交 PR 至 `awesome-chatbot`

- [ ] **GitHub Trending 衝刺策略**
  - [ ] 選定發布日（週日晚間 UTC+8）
  - [ ] 預先在 Discord/Telegram/Twitter 通知社群「明天正式發布」
  - [ ] 發布後 48hr 內集中社群力量 star
  - [ ] 同步發布至 Reddit: r/rust, r/selfhosted, r/LocalLLaMA, r/ClaudeAI

- [ ] **GIF Demo 錄製**
  - [ ] 場景 1：`brew install` → `duduclaw onboard` → LINE 對話（30 秒）
  - [ ] 場景 2：Dashboard 總覽 → Agent 建立 → 頻道熱啟動（20 秒）
  - [ ] 場景 3：本地推論切換 → Confidence Router → 零成本對話（20 秒）
  - [ ] 工具：asciinema（終端）+ Kap/OBS（GUI）→ 轉 GIF

### 0.3 #BuildInPublic 啟動 [P0-NOW]

- [ ] **X/Twitter 帳號**
  - [ ] 建立 @duduclaw 帳號（或個人帳號專用 thread）
  - [ ] 每日 1-2 則推文：開發進度 / 技術決策 / 使用者回饋 / Demo GIF
  - [ ] 使用 hashtag：#BuildInPublic #AItools #RustLang #ClaudeCode #MCP
  - [ ] 與 Anthropic、Rust 社群、AI 工具開發者互動

- [ ] **首週推文計畫**
  - [ ] Day 1：「我用 Rust 打造了一個自我進化的 AI Agent 系統」+ 架構圖
  - [ ] Day 2：「讓 AI Agent 90% 對話零成本的秘密 — Prediction-Driven Evolution」+ 原理圖
  - [ ] Day 3：「一台 Mac Mini 跑 70B 模型？Exo P2P Cluster 實測」+ 影片
  - [ ] Day 4：「12 個 Rust Crate 組成的 AI Agent 基礎設施」+ 架構圖
  - [ ] Day 5：「LINE + Telegram + Discord 三頻道統一管理」+ Dashboard 截圖
  - [ ] Day 6：「從開源到商業化 — DuDuClaw 的下一步」+ 定價頁截圖
  - [ ] Day 7：「本週總結 + 下週計畫」+ Star 數截圖

---

## Phase 1: 立即啟動（Month 1-2）

### 1.1 授權系統 `duduclaw-license` [P0]

> 此 crate 為商業閉源模組，放在 `commercial/duduclaw-license/`，不推至公開 repo。
> ELv2 授權（§0.0）已完成，此 crate 可直接開始實作。

#### 1.1.1 新增 Crate

- [ ] 建立 `commercial/duduclaw-license/` crate（閉源）
  - [ ] `Cargo.toml` — 依賴 `ed25519-dalek`、`serde`、`chrono`、`base64`
  - [ ] `src/lib.rs` — 公開 API
  - [ ] `src/key.rs` — Ed25519 授權金鑰簽發/驗證（複用 `duduclaw-security` 的密碼學基礎）
  - [ ] `src/license.rs` — `License` 結構體（tier, issued_at, expires_at, machine_fingerprint, features）
  - [ ] `src/tier.rs` — `LicenseTier` enum: `Community | Pro | Enterprise | OEM`
  - [ ] `src/gate.rs` — `FeatureGate` trait：依授權等級啟用/停用功能
  - [ ] `src/fingerprint.rs` — 機器指紋（MAC + hostname hash）
  - [ ] `src/elv2.rs` — ELv2 合規檢查（託管服務偵測 + license key 保護）
  - [ ] `src/error.rs` — 授權相關錯誤類型

#### 1.1.2 功能閘門定義

- [ ] 定義功能矩陣（`features.toml` 或 embedded constant）：

  ```
  Community (ELv2 — 非託管服務免費):
    max_channels: 1
    max_agents: 1
    evolution_enabled: true (full features, non-hosted-service only)
    odoo_enabled: true (non-hosted-service only)
    container_sandbox: true
    local_inference: true (all backends)
    compression: all
    security_hooks: all layers
    hosted_service: false (ELv2 restriction)
    security_patch_delay: 30 days

  Pro (商業授權 — 單機):
    max_channels: unlimited
    max_agents: unlimited
    evolution_enabled: true
    odoo_enabled: false
    container_sandbox: true
    local_inference: true (all backends)
    compression: all
    security_hooks: all layers
    commercial_use: true
    security_patch_delay: 0 (immediate)
    premium_templates: true

  Enterprise (商業授權 — 站點):
    (all Pro features +)
    odoo_enabled: true
    site_license: true
    audit_export: true (PDF/CSV)
    priority_support: true (SLA)
    security_patch_delay: 0 (immediate)
    partner_portal: true

  OEM (嵌入分發):
    (all Enterprise features +)
    white_label: true
    redistribution: true
  ```

#### 1.1.3 整合至現有模組

- [ ] `crates/duduclaw-cli/src/main.rs`
  - [ ] 新增 `License` 子命令：`activate <key>`, `deactivate`, `status`, `verify`
  - [ ] 啟動時檢查授權狀態，寫入 `~/.duduclaw/license.key`
  - [ ] 授權過期提前 30 天 / 7 天 / 1 天 Warning（複用 OAuth expiry 的 pattern）

- [ ] `crates/duduclaw-gateway/src/lib.rs`
  - [ ] 在路由層加入 `FeatureGate` middleware
  - [ ] 未授權功能回傳 HTTP 402 + 友善升級提示
  - [ ] MCP Server 工具依授權過濾可用清單

- [ ] `crates/duduclaw-agent/src/registry.rs`
  - [ ] `create_agent()` 檢查 `max_agents` 限制
  - [ ] 超額時回傳具體錯誤訊息（目前幾個 / 上限幾個 / 如何升級）

- [ ] `crates/duduclaw-gateway/src/channels/`
  - [ ] 各頻道啟動時檢查 `max_channels` 限制
  - [ ] `ChannelsPage` hot-start 受限於授權

#### 1.1.4 授權金鑰管理工具

- [x] ~~建立金鑰簽發 CLI（內部使用，不公開）~~ ✅ 已建立完整實作
  - [x] `tools/license-keygen/` — 獨立 Rust binary（Cargo.toml + src/main.rs）
  - [x] 輸入：tier + duration + customer_name + machine_fingerprint
  - [x] 輸出：Base64 encoded Ed25519 signed license key
  - [x] 支援批次簽發（CSV 輸入 → 批次產出）
  - [x] 額外子命令：`keygen`（產生簽章金鑰對）、`verify`（驗證授權碼）、`fingerprint`（產生機器指紋）

---

### 1.2 前端授權 UI [P0]

#### 1.2.1 新增頁面

- [ ] `web/src/pages/LicensePage.tsx`
  - [ ] 授權狀態卡片（tier 名稱、到期日、機器指紋）
  - [ ] 授權金鑰輸入框 + 啟用按鈕
  - [ ] 功能矩陣對比表（Community / Pro / Enterprise）
  - [ ] 升級 CTA 按鈕（連結至官網購買頁）
  - [ ] 到期倒數顯示（<30 天 amber 提醒、<7 天 rose 警告）

#### 1.2.2 修改現有頁面

- [ ] `web/src/pages/DashboardPage.tsx`
  - [ ] 新增授權狀態 StatCard（tier + 到期日）
  - [ ] Community 方案顯示「升級解鎖更多功能」Banner

- [ ] `web/src/pages/SettingsPage.tsx`
  - [ ] 新增「授權」Tab
  - [ ] 顯示授權詳情 + 管理操作

- [ ] `web/src/App.tsx`
  - [ ] 新增 `/license` 路由
  - [ ] Sidebar 加入 License 圖示（Shield 或 Key icon）

#### 1.2.3 API 端點

- [ ] `web/src/lib/api.ts`
  - [ ] `GET /api/license` — 取得授權狀態
  - [ ] `POST /api/license/activate` — 啟用授權碼
  - [ ] `POST /api/license/deactivate` — 停用授權
  - [ ] `LicenseInfo` TypeScript interface

- [ ] `crates/duduclaw-gateway/src/api/`
  - [ ] 新增 `license.rs` — Axum handler for license endpoints
  - [ ] 註冊至 router

---

### 1.3 產品官網 [P0]

- [ ] 建立 `website/` 目錄（獨立專案或 GitHub Pages）
  - [ ] Landing Page — 產品介紹 + 3 大賣點 + Demo 影片
  - [ ] 定價頁 — 3 方案對比表 + 購買按鈕
  - [ ] 案例頁 — 客戶成功故事（初期放 PoC 成果）
  - [ ] 文件頁 — 連結至 GitHub docs/
  - [ ] 聯絡頁 — 表單 + LINE OA + Email
  - [ ] 部落格 — 技術文章 + 產業洞察（SEO 用）
- [ ] 域名註冊：duduclaw.tw 或 duduclaw.dev
- [ ] SSL + CDN 設定

### 1.4 付款整合 [P0]

- [ ] 評估付款方案
  - [ ] 綠界 ECPay（台灣本地，支援超商代碼/ATM/信用卡）
  - [ ] Stripe（國際客戶 + 訂閱制自動扣款）
  - [ ] 選定後整合至官網購買流程
- [ ] 建立自動化流程：付款完成 → 產生授權碼 → Email 寄送

### 1.5 體驗營教材 [P0]

- [ ] 簡報製作（20 頁）
  - [ ] AI Agent 市場趨勢（5 頁）
  - [ ] DuDuClaw 架構與 Demo（5 頁）
  - [ ] Hands-on 操作指引（5 頁）
  - [ ] 產品方案與定價（5 頁）
- [ ] Hands-on 操作手冊
  - [ ] 前置準備清單（帳號、環境）
  - [ ] Step-by-step LINE Bot 建立指南
  - [ ] 常見問題排除
- [ ] Demo 環境
  - [ ] 預裝 DuDuClaw 的 Demo VM / Container Image
  - [ ] 學員帳號批次產生腳本
  - [ ] Demo 用 LINE Bot 帳號（開發者模式）

### 1.6 垂直產業模板 — 餐飲業 [P0]

#### 1.6.1 Agent 模板

- [x] ~~`templates/restaurant/` 目錄~~ ✅ 已建立完整模板
  - [x] `agent.toml` — 餐飲業預設配置（LINE+Telegram、本地推論優先、預算控制）
  - [x] `SOUL.md` — 餐飲客服人格（親切、快速、熟悉菜單、雙語支援）
  - [x] `CONTRACT.toml` — 行為邊界（不推薦競品、不洩漏成本、過敏原警告）
  - [x] `FAQ.json` — 完整 FAQ 範本（營業時間、地址、訂位、菜單、過敏原、付款方式）

#### 1.6.2 記憶體匯入工具

- [ ] `crates/duduclaw-memory/src/import.rs`
  - [ ] CSV 匯入（菜單、價格、營業時間）
  - [ ] Excel 匯入（xlsx via `calamine` crate）
  - [ ] JSON 匯入（POS 系統匯出格式）

- [ ] `duduclaw import` CLI 子命令
  - [ ] `duduclaw import --agent <name> --file menu.csv --type faq`
  - [ ] 自動分類為 Memory entries with tags

#### 1.6.3 訂位擴充

- [ ] 擴充 `schedule_task` MCP 工具
  - [ ] 支援「訂位」類型任務（日期、時間、人數、姓名、電話）
  - [ ] 整合 CronScheduler 提前提醒（訂位前 1hr 推播）
  - [ ] 衝突偵測（同時段超額預約警告）

### 1.7 Product Hunt + Hacker News 國際發布 [P0]

- [ ] **Product Hunt 發布準備**
  - [ ] 製作 5 張高品質截圖（Dashboard / Agent 對話 / Evolution / 本地推論 / 安全面板）
  - [ ] 製作 90 秒 Demo 影片（英文旁白 + 字幕）
  - [ ] 撰寫 200+ 字 Maker Comment（Why / How / What + 技術亮點）
  - [ ] 聯繫 Top Hunter（Chris Messina / Kevin William David）代為提交
  - [ ] 預熱：T-14 天 Twitter 預告 + waitlist
  - [ ] 發布：太平洋時間週二 00:01
  - [ ] 發布後全天候回覆評論
  - [ ] 將 PH badge 加入 README + 官網

- [ ] **Hacker News 發布**
  - [ ] 標題：「Show HN: DuDuClaw – Rust-based Claude Code extension for self-evolving AI agents」
  - [ ] 發布時間：美東時間週二-四上午 9-11 點
  - [ ] 準備 FAQ 回覆草稿（Rust 選型理由 / 與 OpenClaw 差異 / Evolution 原理）
  - [ ] 同步監控 HN 評論 12hr

- [ ] **Reddit 多版發布**
  - [ ] r/rust — 「I built a 12-crate AI Agent system in Rust」
  - [ ] r/selfhosted — 「Self-hosted AI Agent with zero cloud dependency」
  - [ ] r/LocalLLaMA — 「Multi-backend local inference: llama.cpp + mistral.rs + Exo P2P」
  - [ ] r/ClaudeAI — 「Claude Code extension layer with Evolution Engine」
  - [ ] 各版文章客製化角度，非重複貼文

### 1.8 免費 → 付費轉換飛輪 [P0]

- [ ] **免費方案設計**
  - [ ] Community 方案限制：1 頻道 + 1 Agent + 1,000 對話/月
  - [ ] 接近限制時溫和提醒（Dashboard Banner + CLI Warning），非硬性阻斷
  - [ ] 每月可免費試用 Pro 功能 3 天（「Pro 體驗期」按鈕）

- [ ] **推薦獎勵機制**
  - [ ] 實作推薦碼系統（`duduclaw refer generate` → 產生唯一推薦碼）
  - [ ] 推薦 1 人安裝並註冊 → 推薦者解鎖 +500 對話額度
  - [ ] 推薦 3 人 → 免費 1 個月 Pro 功能
  - [ ] 推薦 10 人 → 永久 Pro（Champions 等級）
  - [ ] 被推薦者首月享 Community+ 待遇（2 頻道 + 2 Agent）
  - [ ] Dashboard 顯示推薦進度（已推薦 N 人 / 下一等級差 M 人）

- [ ] **特殊優惠**
  - [ ] 教育優惠：`.edu` / `.ac` email 驗證 → 5 折 Pro
  - [ ] 開源貢獻者：被 merge 的 PR → 免費 6 個月 Pro
  - [ ] 早鳥方案：前 100 位付費用戶享終身 7 折

### 1.9 台灣線上社群攻勢 [P0]

- [ ] **PTT Soft_Job 板**
  - [ ] 撰寫「開發心得」文：「我花了一年用 Rust 打造 AI Agent 系統的心路歷程」
  - [ ] 角度：個人痛點出發 → 技術選型 → 成果展示，自然帶出工具
  - [ ] 避免商業感：不放定價、不放購買連結，僅放 GitHub
  - [ ] 每 2 週 1 篇深度文章

- [ ] **Facebook 社團**
  - [ ] 目標社團清單：
    - [ ] 「台灣軟體工程師社群」
    - [ ] 「Rust Taiwan」
    - [ ] 「AI 工程師 Taiwan」
    - [ ] 「LINE Bot 開發者社群」
    - [ ] 「台灣 Telegram 開發者」
  - [ ] 每週 1 篇貼文（技術分享 / Demo 影片 / 使用案例）
  - [ ] 積極回覆社團內 AI Agent / LINE Bot 相關問題，自然推薦

- [ ] **Medium / Dev.to 技術文章**
  - [ ] 「Prediction-Driven Evolution: 如何讓 AI Agent 90% 對話零成本」
  - [ ] 「用 Rust + PyO3 打造 Claude Code 擴展層」
  - [ ] 「Confidence Router: 本地推論三層路由設計」
  - [ ] 「SOUL.md 版本控制 — AI Agent 人格的 Git」
  - [ ] 每週 1 篇，交叉連結回 GitHub

- [ ] **Dcard 科技板**
  - [ ] 故事性包裝：「我用一台 Mac Mini 取代了三套月租 SaaS」
  - [ ] 每月 1 篇

---

## Phase 2: 短期衝刺（Month 3-6）

### 2.1 導入方法論文件 [P1]

- [x] ~~`docs/implementation-methodology/`~~ ✅ 完整 5 階段文件 + 模板已建立
  - [x] `01-discovery.md` — 需求訪談模板 + 場景盤點清單 + PoC 範圍定義
  - [x] `02-poc.md` — PoC 執行 SOP（14 天流程）+ 6 項成效衡量指標 + 結案報告模板
  - [x] `03-build.md` — 建置檢查清單（5 大類）+ 驗收標準 + 簽核表
  - [x] `04-pilot.md` — 陪跑期每日監控指標 + 每週調校流程 + Edge Case 處理
  - [x] `05-handover.md` — 結案交付物清單 + 維護合約範本 + 滿意度調查
  - [x] `templates/quotation-template.md` — 完整報價單模板（授權/導入/硬體/維護）

### 2.2 實戰班教材 [P1]

- [ ] 16hr 完整教材
  - [ ] Day 1 Lab 環境（pre-built Docker image + 所有依賴）
  - [ ] Day 1 Lab 1：安裝部署（duduclaw onboard 全流程）
  - [ ] Day 1 Lab 2：Agent 設計（SOUL.md + CONTRACT.toml）
  - [ ] Day 1 Lab 3：多頻道串接（LINE + Telegram）
  - [ ] Day 1 Lab 4：MCP 工具開發
  - [ ] Day 2 Lab 5：Evolution Engine 操作
  - [ ] Day 2 Lab 6：本地推論配置
  - [ ] Day 2 Lab 7：Odoo ERP 整合
  - [ ] Day 2 Lab 8：安全防禦與 Red-team
  - [ ] 期末專題評分標準
- [ ] 線上課程錄影版（剪輯後上架 Hahow）

### 2.3 Onboard Wizard（產業別快速設定）[P1]

- [ ] `web/src/pages/OnboardWizardPage.tsx`
  - Step 1：選擇產業別（餐飲/製造/貿易/零售/其他）
  - Step 2：基本資料（公司名、聯絡人、主要頻道）
  - Step 3：選擇功能模組（客服/業務/內部助理/庫存）
  - Step 4：匯入資料（CSV/Excel 上傳）
  - Step 5：確認 + 一鍵部署

- [ ] `crates/duduclaw-cli/src/wizard.rs`
  - [ ] CLI 版 Wizard（`duduclaw wizard`）
  - [ ] 依產業自動套用模板 + SOUL.md + CONTRACT.toml
  - [ ] 自動建立 Agent + 設定頻道 + 匯入資料

### 2.4 垂直產業模板 — 製造業 [P1]

- [x] ~~`templates/manufacturing/`~~ ✅ 已建立完整模板
  - [x] `agent.toml` — 製造業預設（Telegram+LINE、Heartbeat 5min、異常通報優先）
  - [x] `SOUL.md` — 廠務助理人格（精確、簡潔、數據導向、嚴重度分級）
  - [x] `CONTRACT.toml` — 行為邊界（不修改產線參數、異常必須通報、需人工確認重啟）
  - [x] `SOP-template/machine-abnormality.md` — 設備異常 SOP 範本（三級警戒、聯絡人表格）

### 2.5 垂直產業模板 — 貿易業 [P1]

- [x] ~~`templates/trading/`~~ ✅ 已建立完整模板
  - [x] `agent.toml` — 貿易業預設（LINE+Telegram 雙頻道、業務導向配置）
  - [x] `SOUL.md` — 業務助理人格（專業、禮貌、快速報價、多語系、B2B 談判意識）
  - [x] `CONTRACT.toml` — 行為邊界（報價需查最新價目表、不承諾交期、大單需主管簽核）
  - [x] `price-list-template.csv` — 價目表匯入範本（含 HS Code、MOQ、交期）

### 2.6 ROI 報表模組 [P1]

- [ ] `crates/duduclaw-gateway/src/analytics.rs`
  - [ ] 對話統計（總量、自動回覆率、轉人工率）
  - [ ] 回應時間統計（平均/P95/P99）
  - [ ] 成本節省估算（每月對話量 × 人力單價 vs Agent 成本）
  - [ ] 演化效率（零成本對話佔比、GVU 觸發次數）
  - [ ] 資料匯出（CSV/PDF）

- [ ] `web/src/pages/ReportPage.tsx`
  - [ ] ROI 儀表板（圖表 + 數字）
  - [ ] 日/週/月切換
  - [ ] PDF 匯出（用於客戶結案報告）

- [ ] API 端點
  - [ ] `GET /api/analytics/summary` — 總覽
  - [ ] `GET /api/analytics/conversations` — 對話統計
  - [ ] `GET /api/analytics/cost-savings` — 成本節省
  - [ ] `GET /api/analytics/export` — 匯出

### 2.7 台灣線下技術社群活動 [P1]

- [ ] **COSCUP 2026（7-8 月）**
  - [ ] 投稿 Talk：「用 Rust 打造自我進化的 AI Agent — DuDuClaw 架構剖析」
  - [ ] 準備 40 分鐘簡報 + Live Demo
  - [ ] 攤位申請（展示 DuDuClaw Box + 即時體驗）
  - [ ] 現場掃 QR Code 加 LINE OA / Discord

- [ ] **MOPCON 2026（10-11 月）**
  - [ ] 投稿 Talk：「AI Agent 如何幫傳產數位轉型 — 從零到上線的實戰分享」
  - [ ] 贊助攤位（展示垂直產業方案）

- [ ] **RustCon TW**
  - [ ] Lightning Talk：「12 個 Rust Crate 組成的 AI Agent 基礎設施」
  - [ ] 準備 5 分鐘精華版 + 技術深潛 Blog 連結

- [ ] **PyCon TW**
  - [ ] Talk：「PyO3 Bridge — 在 Rust 系統中無縫整合 Python AI 生態」

- [ ] **g0v 黑客松（雙月）**
  - [ ] 帶 DuDuClaw 參加，展示公民科技 / NPO 應用場景
  - [ ] 招募開源貢獻者

- [ ] **Rust Meetup Taipei（月 1 次）**
  - [ ] 定期分享開發進度
  - [ ] 培養核心技術社群

### 2.8 KOL 與開發者大使計畫 [P1]

- [ ] **科技 KOL 合作清單**
  - [ ] 科技 YouTuber：聯繫合作（免費 DuDuClaw Box 置換 or NT$20,000-50,000/支）
  - [ ] 技術部落客：邀請撰寫深度使用文（NT$5,000-15,000/篇）
  - [ ] Podcast：申請上「星箭廣播」「寫點科普」等節目分享
  - [ ] 大學教授：提供教學版授權 + 探討共同發表論文

- [ ] **DuDuClaw Champions 開發者大使計畫**
  - [ ] 撰寫招募公告 + 申請表單
  - [ ] 招募 10-20 位活躍使用者
  - [ ] 提供：免費 Enterprise 授權 + 新功能早鳥存取 + 專屬 Discord 頻道
  - [ ] 義務：每月 1 篇技術文章或影片 + 參加 1 次社群活動
  - [ ] 推薦獎金：每成功推薦 1 位付費客戶 → NT$3,000

### 2.9 YouTube 內容行銷 [P1]

- [ ] **YouTube 頻道建立**
  - [ ] 頻道名稱、Banner、簡介設定
  - [ ] 影片系列規劃：

- [ ] **「5 分鐘上手」系列**（搜尋導向，吸引新用戶）
  - [ ] 「5 分鐘架好你的 LINE AI 助理」
  - [ ] 「一行指令部署 AI Agent」
  - [ ] 「Mac Mini 變身 AI 伺服器」

- [ ] **「架構深潛」系列**（技術權威，吸引資深工程師）
  - [ ] 「Evolution Engine 原理 — 90% 對話零成本的秘密」
  - [ ] 「Confidence Router 三層路由設計」
  - [ ] 「GVU 自我演化 — AI Agent 的自我進化機制」

- [ ] **「對比評測」系列**（搜尋流量最高）
  - [ ] 「Cursor vs Cline vs DuDuClaw — 自建 AI Agent 誰最強？」
  - [ ] 「OpenClaw vs DuDuClaw — 哪個適合你？」

- [ ] **「客戶故事」系列**（轉換用，Month 4+ 開始）
  - [ ] 「這家餐廳用 AI 接單，省了 2 個人力」
  - [ ] 「製造業老闆：異常通報從 30 分鐘變 30 秒」

- [ ] **Live Coding 直播**
  - [ ] Twitch / YouTube 同步直播
  - [ ] 用 DuDuClaw 現場開發真實功能
  - [ ] 每月 1-2 次，每次 1-2 小時

### 2.10 SEO 長尾關鍵字佈局 [P1]

- [ ] **目標關鍵字 + 對應內容**
  - [ ] 「claude code extension」→ Blog：「如何擴展 Claude Code 的 10 種方式」
  - [ ] 「ai agent framework rust」→ Blog：「Rust AI Agent 框架比較」
  - [ ] 「line bot ai 自動回覆」→ Blog + YT：「LINE Bot + AI 全自動客服建置教學」
  - [ ] 「ai 客服機器人 推薦」→ Blog：「2026 年 AI 客服機器人推薦」
  - [ ] 「本地 llm 推論 mac」→ Blog + YT：「Mac 上跑 LLM：完整指南」
  - [ ] 「odoo ai 整合」→ Blog：「Odoo ERP + AI Agent 無痛整合」
  - [ ] 「telegram bot 機器人」→ Blog：「Telegram Bot 進階：接入 AI Agent」
  - [ ] 「開源 ai 客服」→ Blog：「開源 AI 客服方案完整比較」
- [ ] 每篇文章內嵌 DuDuClaw 安裝引導 + LINE OA QR Code

### 2.11 政府補助申請文件 [P1]

- [ ] SBIR Phase 1 計畫書
  - [ ] 技術創新說明（Evolution Engine + Confidence Router）
  - [ ] 市場分析（台灣 AI Agent 市場規模）
  - [ ] 執行計畫（6 個月里程碑）
  - [ ] 經費預算表
- [ ] SIIR 計畫書（需公司設立後）
- [ ] 數位轉型補助 — 客戶端申請輔導 SOP

---

## Phase 3: 中期擴張（Month 6-12）

### 3.1 DuDuClaw Box 部署自動化 [P2]

- [ ] `scripts/box-setup/`
  - [ ] macOS 自動安裝腳本（Homebrew + DuDuClaw + 模型下載）
  - [ ] 首次開機設定精靈（觸控友善 UI）
  - [ ] 硬體自檢腳本（GPU/RAM/磁碟空間）
  - [ ] 自動更新機制（定期拉取 Homebrew 更新）

- [ ] NAS 整合
  - [ ] Synology DSM Package（`.spk` 打包）
  - [ ] QNAP QPKG 打包
  - [ ] Docker Compose 範本（通用 NAS）

### 3.2 經銷商管理系統 [P2]

- [ ] `web/src/pages/PartnerPortalPage.tsx`（獨立入口或子域名）
  - [ ] 經銷商登入 + 客戶管理
  - [ ] 授權碼批次產生（依經銷商等級折扣）
  - [ ] 業績追蹤儀表板
  - [ ] 技術支援工單系統（簡易版）
  - [ ] 行銷素材下載中心

- [ ] 經銷商 API
  - [ ] `POST /api/partner/licenses/generate` — 批次產生授權碼
  - [ ] `GET /api/partner/sales` — 銷售統計
  - [ ] `GET /api/partner/customers` — 客戶清單

### 3.3 傳產地推戰術 [P2]

- [ ] **商圈實地拜訪**
  - [ ] 準備攜帶式 Demo 設備（MacBook + DuDuClaw + LINE Bot 即時展示）
  - [ ] 目標區域：台北東區 / 信義區 / 永康街 等餐飲密集區
  - [ ] 話術卡片：「你的 LINE 官方帳號一個月多少錢？加 NT$4,990，AI 幫你 24 小時自動接客」
  - [ ] 每週拜訪 5-10 家，收集名片 + 痛點紀錄

- [ ] **AI 客服免費健檢**
  - [ ] 設計「LINE OA 效率健檢表」（回覆速度 / 自動化程度 / 漏接率）
  - [ ] 免費分析企業 LINE OA 現況，出具 1 頁改善報告
  - [ ] 報告末尾附 DuDuClaw 解決方案 + 報價
  - [ ] 作為獲客漏斗頂端（健檢 → PoC → 導入）

- [ ] **公會合作說明會**
  - [ ] 聯繫台北市餐飲業公會 — 合辦「AI 客服實戰說明會」
  - [ ] 聯繫台灣連鎖加盟促進協會 — 「連鎖品牌 AI 升級」
  - [ ] 聯繫台北市進出口公會 — 「貿易業 AI 業務助理」
  - [ ] 公會動員（免費）+ DuDuClaw 出講師 + 現場 Demo
  - [ ] 每場收集 10-20 位潛在客戶名單

- [ ] **異業合作推廣**
  - [ ] iCHEF（餐飲 POS）：POS 資料 → DuDuClaw Memory 匯入整合
  - [ ] 肚肚（餐飲訂位）：訂位系統 → Agent 自動通知整合
  - [ ] LINE 代理商：聯合推廣「LINE OA + AI 客服」方案
  - [ ] 合作模式：互相推薦 / 分潤 / 聯合方案

- [ ] **首批客戶口碑擴散**
  - [ ] 邀請首批客戶在公會/群組分享心得
  - [ ] 錄製 30 秒客戶見證影片
  - [ ] 撰寫案例白皮書（含具體數字：省了多少時間/人力/成本）

### 3.4 話題製造與 PR 事件 [P2]

- [ ] **「一台 Mac Mini 挑戰」**
  - [ ] 公開直播 72hr：一台 NT$25,000 的 Mac Mini 能否取代 NT$30,000/月的客服 SaaS？
  - [ ] 即時顯示：對話數 / 自動回覆率 / API 成本（目標 $0）/ 客戶滿意度
  - [ ] 直播平台：YouTube + Twitch 同步
  - [ ] 結束後發布完整數據報告

- [ ] **「零成本 AI 30 天挑戰」**
  - [ ] 30 天完全使用本地推論 + Evolution Engine
  - [ ] 每日記錄 API 帳單（目標 $0）
  - [ ] 每週發布進度報告（Blog + Twitter thread）
  - [ ] 最終報告：實際成本 vs 傳統方案成本對比

- [ ] **「傳產老闆 AI 體驗日」**
  - [ ] 邀請 10 位完全不懂技術的老闆
  - [ ] 挑戰：1 小時內上線自己的 AI 客服
  - [ ] 全程影片記錄 → 剪輯為 YT 影片
  - [ ] 媒體邀請（數位時代 / TechOrange / iThome）

- [ ] **開源公益活動**
  - [ ] 為 NPO/NGO 免費建置 AI 客服
  - [ ] 目標：動物收容所 / 食物銀行 / 社福團體
  - [ ] 公益形象 + 媒體曝光 + 真實使用案例

### 3.5 戰略合作卡位 [P2]

- [ ] **Anthropic**
  - [ ] 申請 Claude Code Partner Program / Ecosystem Partner
  - [ ] 目標：官方認證背書 + 共同行銷 + 技術支援優先通道

- [ ] **LINE Developers**
  - [ ] 申請 LINE API Expert 認證
  - [ ] 目標：LINE 開發者社群曝光 + 技術合作

- [ ] **Apple**
  - [ ] 強調 Metal + MLX 優化，申請 Apple Developer Spotlight
  - [ ] 撰寫 Apple Developer 技術文章

- [ ] **Synology / QNAP**
  - [ ] 申請開發者計畫 + 上架 Package Center
  - [ ] 每台 NAS 都是潛在 DuDuClaw 部署節點

- [ ] **Odoo 台灣社群**
  - [ ] 聯合辦活動 + 整合 Demo
  - [ ] Odoo 用戶 = DuDuClaw 天然客戶

### 3.6 企業稽核報表 [P2]

- [ ] `commercial/dashboard-enterprise/audit_export.rs`（閉源）
  - [ ] 稽核日誌匯出（CSV/PDF）
  - [ ] 安全事件時間軸報表
  - [ ] SOUL.md 變更歷史紀錄
  - [ ] Agent 行為合規性報告（CONTRACT.toml 違規統計）

### 3.7 多語系支援強化 [P2]

- [x] ~~`web/src/i18n/` 基礎框架~~ ✅ 已有 Zustand i18n store
  - [x] zh-TW 翻譯 ✅ 已有
  - [x] en 翻譯 ✅ 已有
- [x] ~~完善 zh-TW / en 翻譯覆蓋率至 100%~~ ✅ 經確認 155 key 完全對齊，無遺漏
- [ ] 新增 ja-JP（日本市場，LINE 普及）
- [ ] SOUL.md 模板多語版本

### 3.8 SaaS 基礎設施準備 [P2]

- [ ] 多租戶架構設計
  - [ ] 資料隔離方案（per-tenant SQLite 或 shared PostgreSQL + RLS）
  - [ ] 租戶識別（subdomain 或 header-based routing）
  - [ ] 租戶配置管理（config per tenant）
- [ ] K8s 部署配置
  - [ ] Helm Chart 或 Kustomize
  - [ ] 自動擴縮（HPA based on agent count）
  - [ ] 監控（Prometheus + Grafana）

---

## Phase 4: 長期佈局（Year 2+）

### 4.1 MCP Server 市集 [P3]

- [ ] `crates/duduclaw-marketplace/`
  - [ ] MCP Server 上架 API（metadata + 安全掃描）
  - [ ] 計量計費引擎（per-tool-call metering）
  - [ ] 開發者帳戶 + 收益分潤
  - [ ] 評分與評論系統

- [ ] `web/src/pages/MarketplacePage.tsx`
  - [ ] Server 瀏覽 + 搜尋 + 分類
  - [ ] 一鍵安裝 + 付款流程
  - [ ] 開發者控制台（上架 + 收益統計）

### 4.2 Billing SaaS 模組 [P3]

- [ ] `crates/duduclaw-billing/`
  - [ ] 用量追蹤（對話數、Agent 數、頻道數、推論時數）
  - [ ] 計費引擎（訂閱 + 超額用量）
  - [ ] 發票產生（串接綠界 or 自建）
  - [ ] Stripe Subscription API 整合

- [ ] `web/src/pages/BillingPage.tsx`
  - [ ] 當期用量儀表板
  - [ ] 帳單歷史
  - [ ] 付款方式管理
  - [ ] 方案升降級

### 4.3 國際化 [P3]

- [ ] 日本市場
  - [ ] ja-JP UI + SOUL.md 日文模板
  - [ ] LINE Japan 整合測試
  - [ ] 日本經銷商招募
- [ ] 東南亞市場
  - [ ] Telegram Bot 在東南亞普及度驗證
  - [ ] 本地化合作夥伴

---

## 非技術 TODO

### 法律與行政

- [x] ~~授權文件~~ ✅ ELv2 已完成（見 §0.0）
- [ ] 商標註冊 — 見 §0.0（待執行）
- [ ] 公司設立（有限公司，資本額 NT$500,000）
- [ ] 統一編號申請
- [ ] EULA 商業授權合約撰寫（委託律師，需涵蓋 ELv2 商業託管服務條款）
- [ ] 服務條款 + 隱私權政策（官網 + SaaS）
- [ ] 專業責任保險投保
- [ ] 記帳事務所委託

### 行銷與銷售

> 社群建立、內容行銷、KOL 合作等細項已整合至 Phase 0 / 1 / 2 的推廣章節

- [ ] 社群帳號建立（Facebook 粉專、LinkedIn）— X/Twitter 見 §0.3、LINE OA 見 §0.1、YouTube 見 §2.9
- [ ] 首批客戶名單收集（體驗營 + 社群 + 地推 + 人脈）
- [ ] 報價單 / 提案書模板設計
- [ ] 名片 + 簡介 DM 設計印刷
- [ ] Email 行銷系統設定（Mailchimp / Resend）— 體驗營/活動後 nurture 序列
- [ ] 「AI Agent 效能基準排行榜」— 定期發布（回應速度/記憶準確率/成本效率），維持技術話題度

### 合作洽談

> 戰略合作細項已整合至 §3.5，傳產地推已整合至 §3.3

- [ ] Apple 經銷商聯繫（Studio A / 德誼）— Box 合作
- [ ] 系統整合商洽談（精誠 / 凌群 / 叡揚）— 聯合解決方案
- [ ] 大學 AI 中心聯繫（產學合作 / 教學版授權）
- [ ] 政府輔導顧問資格申請

---

## 技術債優先清理

> 商業化前建議先處理的技術債，提升產品穩定性

- [ ] 完善錯誤處理：所有 `unwrap()` 替換為 proper error handling
- [ ] Dashboard E2E 測試覆蓋率提升至 80%
- [ ] API 端點文件化（OpenAPI/Swagger spec）
- [ ] `duduclaw doctor` 補完所有檢查項（目前部分為 placeholder）
- [ ] 效能基準測試（benchmark：每秒可處理多少訊息）
- [ ] 安全審計（OWASP Top 10 + 依賴項目漏洞掃描）
- [ ] Cross-platform 測試（Linux ARM64、Windows WSL2）

---

## 推廣 KPI 追蹤

| 指標 | Week 2 | Month 1 | Month 3 | Month 6 | Month 12 |
|------|--------|---------|---------|---------|----------|
| GitHub Stars | 200 | 500 | 2,000 | 5,000 | 10,000 |
| Discord 成員 | 50 | 150 | 500 | 1,000 | 2,000 |
| LINE OA 好友 | 100 | 300 | 1,000 | 3,000 | 5,000 |
| Twitter 追蹤 | 100 | 500 | 2,000 | 5,000 | 10,000 |
| 官網月訪客 | — | 1,000 | 5,000 | 15,000 | 30,000 |
| YouTube 訂閱 | — | 50 | 300 | 1,000 | 3,000 |
| 免費用戶 | 10 | 50 | 200 | 500 | 1,000 |
| 付費用戶 | 0 | 1 | 10 | 50 | 200 |
| 免費→付費轉換率 | — | — | 5% | 8% | 10% |
| 技術文章發布數 | 2 | 8 | 24 | 48 | 96 |
| 活動場次 | 0 | 1 | 5 | 12 | 24 |

---

## 檔案結構規劃

```
DuDuClaw/
├── LICENSE                    # [✅ DONE] ELv2 授權
├── LICENSING.md               # [✅ DONE] 授權 FAQ（中英文）
├── crates/
│   ├── duduclaw-billing/      # [NEW] P3 — 計費引擎
│   ├── duduclaw-marketplace/  # [NEW] P3 — MCP 市集
│   └── (existing 12 crates — ELv2 授權)
├── commercial/                # [✅ DONE] 商業閉源模組（目錄架構 + README 已建立）
│   ├── duduclaw-license/      # [✅ DONE] 授權驗證規劃書
│   ├── templates-premium/     # [✅ DONE] 產業模板規劃書
│   ├── evolution-params/      # [✅ DONE] Evolution 參數集規劃書
│   └── dashboard-enterprise/  # [✅ DONE] ROI 報表規劃書
├── templates/                 # [✅ DONE] 產業模板（3 產業完整建立）
│   ├── restaurant/            # agent.toml + SOUL.md + CONTRACT.toml + FAQ.json
│   ├── manufacturing/         # agent.toml + SOUL.md + CONTRACT.toml + SOP-template/
│   └── trading/               # agent.toml + SOUL.md + CONTRACT.toml + price-list-template.csv
├── tools/
│   └── license-keygen/        # [✅ DONE] 授權金鑰產生器（Cargo.toml + src/main.rs）
├── website/                   # [NEW] P0 — 產品官網
├── marketing/                 # [✅ DONE] 推廣素材目錄已建立
│   ├── gif-demos/             # [READY] 待錄製 GIF Demo
│   ├── screenshots/           # [READY] 待截圖
│   ├── social-templates/      # [✅ DONE] Twitter thread + PTT 文章模板
│   ├── press-kit/             # [READY] 待製作媒體素材
│   └── slide-decks/           # [READY] 待製作簡報
├── docs/
│   ├── business-plan.md       # [✅ DONE] v1.1 — 含授權策略 + 護城河
│   ├── content-policy.md      # [✅ DONE] 內容分層規範（三層策略）
│   ├── security-patch-sop.md  # [✅ DONE] 安全補丁發布 SOP
│   ├── TODO-commercialization.md  # [✅ DONE] 本文件
│   └── implementation-methodology/  # [✅ DONE] 完整 5 階段導入方法論 + 報價單模板
├── web/src/pages/
│   ├── LicensePage.tsx        # [NEW] P0
│   ├── OnboardWizardPage.tsx  # [NEW] P1
│   ├── ReportPage.tsx         # [NEW] P1
│   ├── BillingPage.tsx        # [NEW] P3
│   └── MarketplacePage.tsx    # [NEW] P3
└── scripts/
    └── box-setup/             # [NEW] P2 — 一體機部署腳本
```
