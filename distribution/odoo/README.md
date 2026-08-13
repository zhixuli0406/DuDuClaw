# Odoo App Store 上架包（duduclaw_connector）

免費 connector 模組（lead-gen，繞開店內 30% 抽成——收費留在 DuDuClaw license 層）。裝在**客戶自己的 Odoo**，連**客戶自己的 gateway**：零 hosted multi-tenant 架構衝突。

## 模組內容

- 設定頁（一般設定 → DuDuClaw）：Gateway URL＋API Key＋儀表板捷徑＋**測試連線**按鈕（打 `/healthz`）
- `duduclaw.connector.send_to_memory(text, source)`：server action／automation 可呼叫——會議記錄、客戶脈絡一鍵進 AI 員工記憶（走 gateway `/ingest/transcript`，scope 與來源綁定在 gateway 端把關）
- stdlib-only（urllib），Odoo 主機零額外 Python 依賴

## 版本分支策略

`__manifest__.py` 版本 `18.0.x`；上架時另出 `17.0.x` 分支（欄位 API 相容，通常只改 manifest 版號）；Odoo 19 視 App Store 通路需求跟進。

## 上架（👤）

1. apps.odoo.com → 綁 git repo（建 `zhixuli0406/duduclaw-odoo-addons`，分支名對應 Odoo 版本：`17.0`/`18.0`）
2. 免費上架；描述照 manifest 的 external service disclosure（vendor guidelines 要求）
3. ⚠ manifest 出錯會整 repo 下架——推前先在乾淨 Odoo 實例裝一次（活測 👤）

## 活測清單（👤，需 Odoo 實例）

- [ ] 安裝/移除乾淨
- [ ] 設定頁存值＋測試連線兩態（成功/失敗通知）
- [ ] server action 呼叫 send_to_memory → gateway 記憶出現該筆（wearable/odoo 標籤）
