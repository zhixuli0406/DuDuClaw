# Phase 3: Build — 正式建置

## 目標

將 PoC 驗證的方案擴展為生產環境，完成完整部署與客戶驗收。

## 建置檢查清單

### 1. 基礎設施

- [ ] 硬體確認與效能測試
  - [ ] CPU/GPU/RAM 規格符合需求
  - [ ] 磁碟空間充足（模型 + 資料庫 + 日誌）
  - [ ] 網路穩定性測試（ping / bandwidth）
- [ ] DuDuClaw 安裝與更新至最新版
- [ ] `duduclaw doctor` 全項通過
- [ ] 備份策略設定（SQLite DB + SOUL.md + 設定檔）

### 2. Agent 配置

- [ ] SOUL.md 完整版（含所有 FAQ、產品資訊、流程知識）
- [ ] CONTRACT.toml 完整邊界定義
- [ ] agent.toml 生產環境參數
  - [ ] Budget 限制設定
  - [ ] Heartbeat 啟用（如需）
  - [ ] Evolution 參數微調
- [ ] 記憶體完整匯入（CSV / Excel / JSON）
- [ ] MCP 工具設定（如需外部系統整合）

### 3. 頻道串接

- [ ] LINE OA 正式帳號設定（非測試帳號）
- [ ] Webhook URL 設定與驗證
- [ ] Telegram Bot 建立與 token 設定
- [ ] Discord Bot 建立與 token 設定（如需）
- [ ] 頻道 hot-start 測試通過

### 4. 安全性

- [ ] API Key 加密儲存（AES-256-GCM）
- [ ] Ed25519 WebSocket 認證測試
- [ ] Prompt injection scanner 啟用
- [ ] Security hooks 三層防禦啟用
- [ ] 授權金鑰啟用（Pro / Enterprise）
- [ ] Audit log 啟用

### 5. 監控與告警

- [ ] Dashboard 可正常存取
- [ ] Cost Telemetry 啟用
- [ ] 異常告警設定（Agent 停止回覆、費用超標）
- [ ] 日誌保留策略設定

## 驗收標準

| 項目 | 標準 | 驗收方式 |
|------|------|---------|
| 功能完整性 | PoC 範圍內所有場景可正常運作 | 逐場景測試 |
| 回覆品質 | 自動回覆率 > PoC 結果 | Dashboard 統計 |
| 回覆速度 | 平均 < 5 秒 | Dashboard 統計 |
| 穩定性 | 連續 72hr 無中斷 | 監控日誌 |
| 安全性 | Red-team 測試通過 | `duduclaw test` |
| 備份恢復 | 可從備份完整恢復 | 實際演練 |

## 驗收簽核表

```
驗收日期：___
驗收人員：___

□ 功能驗收通過
□ 效能驗收通過
□ 安全驗收通過
□ 文件交付完整
□ 教育訓練完成

簽名：_______________
日期：_______________
```
