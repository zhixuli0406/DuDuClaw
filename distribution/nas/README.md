# 把 AI 員工裝進你的 NAS（Synology／QNAP）

> 你公司那台 NAS 就是 AI 員工的宿舍——24 小時開機、資料不出門、不用再買新機器。
> NAS 上跑的是 DuDuClaw gateway（通道、記憶、排程）；AI 推論走你的 Anthropic 訂閱或 API key。

## 機型前提

| 平台 | 需求 |
|---|---|
| Synology | DSM 7.2+ 且機型支援 **Container Manager**（Plus 系列等 x86 機型；J 系列入門機不支援）|
| QNAP | QTS/QuTS hero 且已安裝 **Container Station** 3 |
| 記憶體 | 建議 ≥2GB 可用 |

## Synology（Container Manager「專案」）

1. 套件中心確認已裝 **Container Manager**。
2. File Station 建資料夾：`docker/duduclaw`。
3. Container Manager → **專案** → 新增：
   - 專案名稱 `duduclaw`、路徑選 `docker/duduclaw`
   - 來源選「建立 docker-compose.yml」→ 貼上本目錄的 `docker-compose.yml` 內容
   - 有 API key 的話，把 `ANTHROPIC_API_KEY=` 那行的值填上（或之後在儀表板設定）
4. 建置並啟動 → 瀏覽器開 `http://<NAS-IP>:18789` → 跑首次設定精靈。
5. 通道頁綁 LINE → 掃 QR → 手機開聊。

## QNAP（Container Station）

1. App Center 安裝 **Container Station**（v3+）。
2. Container Station → 應用程式 → **建立** → 貼上 `docker-compose.yml` 內容 → 命名 `duduclaw` → 建立。
3. 開 `http://<NAS-IP>:18789` → 首次設定精靈 → 綁 LINE。

## 訂閱帳號接入（Claude Pro/Max，OAuth）——最容易卡的一步

容器裡跑 `claude setup-token` 需要瀏覽器交接，NAS 上沒有瀏覽器。兩條路：

- **建議：先用 API key 起步**（compose 的 `ANTHROPIC_API_KEY`），之後要省成本再換訂閱。
- **訂閱帳號**：在你的電腦上跑 `claude setup-token` 拿到 token → 儀表板「帳號」頁新增 OAuth 帳號貼上 token（或照 [docs/guides/docker.md](../../docs/guides/docker.md) 的 token 注入流程）。

## 日常維運

- **備份**：整個 `docker/duduclaw/duduclaw-data` 資料夾（Hyper Backup／HBS 直接排程）。
- **升級**：Container Manager／Container Station 對專案「重新建置」（image 是 `latest`；要鎖版就把 tag 改成 `:1.56.0` 這類固定版）。
- **遠端存取**：手機端日常用 LINE/Telegram 就好（通道即行動介面）；要遠端開儀表板，建議 Tailscale 或 Cloudflare Tunnel，不要直接把 18789 埠開上公網。

## 疑難排解

| 症狀 | 檢查 |
|---|---|
| 開不了 18789 | 容器 log 是否啟動完成；NAS 防火牆是否放行；port 是否被占用 |
| AI 已讀不回 | 儀表板「帳務」頁看額度/斷路器；「系統日誌」看失敗原因 |
| 重啟後資料不見 | volume 是否掛對（`duduclaw-data` 必須在，見 compose） |
