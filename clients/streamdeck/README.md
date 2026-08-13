# DuDuClaw Stream Deck plugin

實體 HITL：審批鍵的 LCD 顯示「最舊待審項目摘要＋件數」（看得到才按——絕不盲按），按下走與儀表板按鈕同一條 `approvals.decide` 裁決路徑；狀態鍵顯示待審數、按下開儀表板。只連你設定的 gateway，無遙測。

## 開發

```bash
npm install && npm run build   # 產出 com.duduclaw.deck.sdPlugin/bin/plugin.js
```

## 安裝（側載）

1. 裝 Elgato 官方 CLI：`npm i -g @elgato/cli`
2. `streamdeck link com.duduclaw.deck.sdPlugin`（或把資料夾複製進 Stream Deck 外掛目錄）
3. Stream Deck app → 拖三顆鍵上盤面 → 首次需在全域設定填 gateway URL／Email／密碼（Property Inspector 設定頁為後續補強項；目前可用 `streamdeck` CLI 的 global settings 或先以 profile 內建值測試）

## 上架前（👤）

- `streamdeck validate com.duduclaw.deck.sdPlugin` 過官方驗證
- Maker Console 提交（可先側載發佈，不卡審核）
