# 探索樹介面驗收 fixture

這個頁面只使用合成 RPC 回覆，沒有後端或模型呼叫。它不代表正式服務已完成整合，也不會登入正式帳號。

從專案根目錄啟動：

```sh
web/node_modules/.bin/vite web --host 127.0.0.1 --port 5197 --strictPort
```

開啟 `http://127.0.0.1:5197/test-fixtures/discovery.html`，檢查三語、窄畫面、輪次與父子關係、費用來源、DEGRADED、建立表單及合成成品下載。下載只含 `ok`。

目前證據：Vitest 與 TypeScript 已檢查；fixture HTML／轉譯模組回應 HTTP 200。2026-10-01 已用 Chrome 實際驗收三語桌面樹／表單、預算限制、輪次收合、合成建立及成品清單。使用者後續指定下載位置後，合成檔案落地及內容 `ok` 已核對；窄畫面尚未驗證；fixture 沒有取消或 pending transition，也不驗證伺服器 SHA。正式建立、核准、取消與下載仍須配合已登入的合格後端環境。詳見 [本輪驗收報告](../../commercial/docs/REPORT-dream-rsi-chrome-ui-acceptance-2026-10-01.md)。
