# 經銷商 Runbook：NAS 導入包（內部用）

> 客戶話術核心：「你機房那台 Synology 就能當 AI 員工的宿舍——不用買新機器、資料不出公司。」

## 售前檢核（報價前必問）

- [ ] NAS 機型：Synology 要 Plus 系列等 x86＋DSM 7.2+（**J 系列不行**）；QNAP 要能裝 Container Station
- [ ] 可用記憶體 ≥2GB（`資源監控` 看）
- [ ] 客戶的 AI 用量模式：起步 API key（好裝、按量計費）或 Claude 訂閱（固定月費、需 token 交接步驟）
- [ ] LINE 官方帳號有無（沒有→現場帶辦，10 分鐘）

## 安裝 SOP（現場 ~30 分鐘）

1. 照 `README.md` 部署 compose（Synology 專案／QNAP 應用程式）。
2. 首次設定精靈：管理員帳密＋選產業板模。
3. 通道頁貼 LINE token → **QR 自動跳出** → 用客戶手機掃碼實測第一句對話。
4. 帳務頁設預算上限（建議照客戶方案設 `hard_stop`）。
5. 交付驗收（下節）＋把 `duduclaw-data` 加進客戶既有備份排程。

## 交付驗收清單（客戶簽收前逐項）

- [ ] NAS 重開機後容器自動起來（restart: unless-stopped 生效）
- [ ] LINE 訊息 → AI 回覆 < 30 秒
- [ ] 儀表板從客戶內網電腦可開
- [ ] 備份排程含 `duduclaw-data`
- [ ] 預算斷路器已設且客戶知道停工訊息長怎樣

## 常見狀況

| 狀況 | 處理 |
|---|---|
| 客戶只有 J 系列 | 不硬裝——改推 Mac mini 主機方案（報價制）或雲端方案 |
| 訂閱 token 交接卡住 | 先 API key 上線，token 之後遠端補（doctor 遠端診斷） |
| 客戶要外網開儀表板 | Tailscale（免費、10 分鐘）；絕不直開 18789 上公網 |

## 商務備註

- 本包軟體授權照既有 license 層報價；NAS 安裝服務費由經銷商自訂。
- 桌牌／NFC 加購品見 [docs/guides/line-touch-nfc.md](../../docs/guides/line-touch-nfc.md)（co-brand 印刷）。
