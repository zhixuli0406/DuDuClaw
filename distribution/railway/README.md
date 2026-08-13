# Railway 模板（含 kickback 分潤）

Railway 模板不是 repo 內檔案，而是在 Railway dashboard 用「Template Composer」定義後發佈到模板市集；上架者可拿使用者用量費的 15%（有維護支援 25%）kickback。本文件是一次性建立＋每版維護的 runbook。

## 一次性建立（Railway 帳號，👤）

1. Railway dashboard → New Template → **Deploy from Docker image**：
   - Image：`ghcr.io/zhixuli0406/duduclaw:latest`
   - Service 名稱：`duduclaw`
2. Service 設定：
   - **Volume**：mount path `/home/duduclaw/.duduclaw`（沒 volume 重啟就掉資料——必設）
   - **Networking**：expose port `18789`（Railway 會給 `https://<app>.up.railway.app` 公網域名）
   - **Healthcheck**：path `/health`
3. 環境變數（模板欄位，讓部署者填）：
   - `DUDUCLAW_BIND=0.0.0.0`（固定值）
   - `ANTHROPIC_API_KEY`（optional，說明文字引導：也可部署後在 dashboard 設定帳號）
   - `DUDUCLAW_ALLOWED_ORIGINS`（optional；說明：填自訂網域時用）
4. 模板說明頁（zh-TW＋en）：貼 README 首段＋「部署完成 → 開網址 → 首次設定精靈」三步；標注資源需求（≥1GB RAM 建議）。
5. Publish → 開啟 kickback（Template 設定內）。

## 每版維護

- 模板釘 `latest` 則零維護；若改釘版本 tag，release 後到 Template Composer 更新 image tag。

## 驗證

- 從模板市集頁實際部署一次：精靈可跑完、WebChat 可對話、重啟後資料還在（volume 生效）。

## 備註

- Railway 是公網環境：模板說明必須提醒部署者第一時間設定 dashboard 密碼（首次設定精靈會做），並考慮 `[gateway] allowed_origins` 加自訂網域。
- Render 的 Deploy Button（`render.yaml`）可日後順手加；Fly.io 無一鍵按鈕，不做。
