# DuDuClaw 部署指南

> 更新日期：2026-03-30｜版本：v0.10.0

---

## 1. 本機開發

```bash
# Build
cargo build --release

# Run (starts gateway + channels + heartbeat + cron + dispatcher)
duduclaw run

# Access Dashboard
open http://localhost:18789
```

預設埠號：`18789`。可在 `~/.duduclaw/config.toml` 設定：

```toml
[gateway]
bind = "127.0.0.1"
port = 18789
```

### 健康檢查

```bash
curl http://localhost:18789/health
# {"status":"ok","version":"0.10.0","uptime_seconds":42,"agents_loaded":2,"channels_connected":["telegram","discord"]}

curl http://localhost:18789/health/ready  # 200 when agents loaded
curl http://localhost:18789/health/live   # 200 always (liveness probe)
```

---

## 2. Tailscale Funnel（建議用於 LINE Webhook）

LINE Messaging API 的 webhook 需要一個**對外公開的 HTTPS 網址**。Tailscale Funnel 不需要 VPS、固定 IP 或網域即可提供這個網址。

### 設定步驟

```bash
# 1. Install Tailscale
brew install tailscale       # macOS
curl -fsSL https://tailscale.com/install.sh | sh  # Linux

# 2. Authenticate
tailscale up

# 3. Enable HTTPS + Funnel
tailscale funnel 18789

# This gives you a URL like:
# https://your-machine.tail12345.ts.net/
```

### 設定 LINE

1. 前往 [LINE Developers Console](https://developers.line.biz/)
2. 選擇你的 Messaging API 頻道
3. 把 Webhook URL 設為：`https://your-machine.tail12345.ts.net/webhook/line`
4. 開啟「Use webhook」
5. 點擊「Verify」按鈕完成驗證

### 常駐 Funnel

```bash
# Run as background service
tailscale funnel --bg 18789

# Or via systemd (Linux)
# Add to duduclaw.service After=tailscaled.service
```

---

## 3. ngrok（替代方案）

```bash
# 1. Install
brew install ngrok        # macOS
snap install ngrok        # Linux

# 2. Authenticate (free account)
ngrok config add-authtoken YOUR_TOKEN

# 3. Start tunnel
ngrok http 18789

# Copy the HTTPS URL (e.g., https://abc123.ngrok-free.app)
# Set as LINE Webhook URL: https://abc123.ngrok-free.app/webhook/line
```

**注意**：免費版 ngrok 的網址每次重啟都會改變。若要固定網址，請搭配保留網域使用 `ngrok http 18789 --domain=your-domain.ngrok-free.app`。

---

## 4. Cloudflare Tunnel（長期穩定）

最適合正式環境使用：免費、網址穩定、不需要轉發埠口。

```bash
# 1. Install cloudflared
brew install cloudflared  # macOS

# 2. Login
cloudflared tunnel login

# 3. Create tunnel
cloudflared tunnel create duduclaw

# 4. Configure (in ~/.cloudflared/config.yml)
cat > ~/.cloudflared/config.yml << 'EOF'
tunnel: YOUR_TUNNEL_ID
credentials-file: /Users/YOU/.cloudflared/YOUR_TUNNEL_ID.json

ingress:
  - hostname: duduclaw.yourdomain.com
    service: http://localhost:18789
  - service: http_status:404
EOF

# 5. Add DNS record
cloudflared tunnel route dns duduclaw duduclaw.yourdomain.com

# 6. Run
cloudflared tunnel run duduclaw
```

設定 LINE Webhook：`https://duduclaw.yourdomain.com/webhook/line`

---

## 5. 反向代理（Caddy / Nginx）

### Caddy（自動 TLS）

```Caddyfile
duduclaw.yourdomain.com {
    reverse_proxy localhost:18789

    # WebSocket support (auto-detected by Caddy)
    # No extra config needed
}
```

```bash
caddy run --config Caddyfile
```

### Nginx

```nginx
server {
    listen 443 ssl;
    server_name duduclaw.yourdomain.com;

    ssl_certificate     /etc/letsencrypt/live/duduclaw.yourdomain.com/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/duduclaw.yourdomain.com/privkey.pem;

    location / {
        proxy_pass http://127.0.0.1:18789;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_read_timeout 86400;
    }
}
```

### WebSocket Origin 白名單（反向代理 / tailnet 必讀）

Dashboard 的即時連線（WebSocket、WebChat）預設只接受來自
loopback（`localhost` / `127.0.0.1` / `[::1]`）的瀏覽器 `Origin`。當你透過
**反向代理網域**或 **Tailscale/tailnet 網址**開啟 dashboard 時，HTTP 頁面會正常
載入，但 WebSocket 升級會被 403 擋掉、畫面持續轉圈圈。把對外網域加進白名單即可
解決：

```toml
# ~/.duduclaw/config.toml
[gateway]
# host、host:port，或含 scheme 的完整 origin 都可（載入時會正規化）
allowed_origins = ["duduclaw.yourdomain.com", "box.your-tailnet.ts.net"]
```

或用環境變數（逗號分隔，與 config.toml 的清單**合併**，不是二選一）：

```bash
DUDUCLAW_ALLOWED_ORIGINS="duduclaw.yourdomain.com,box.your-tailnet.ts.net"
```

- 內建 loopback 三項永遠有效，不需列出；清單為空時行為與舊版完全一致。
- 每個項目是**精確**的 host 或 host:port 比對，不支援萬用字元；port-less 項目
  匹配該 host 的任意 port。後綴攻擊（`duduclaw.yourdomain.com.evil.com`）會被擋。
- 啟動時會印一行 info log 列出生效的額外 origins，方便排錯。
- 也可直接在 dashboard **設定 → 系統 → 遠端存取網址**新增／刪除，不必手改 config.toml；
  **存檔即時生效，不用重開 gateway**（環境變數提供的項目仍會保留）。

### 通道推播的儀表板深連結（`[dashboard] public_url`）

當 AI 員工在 LINE／Telegram／Slack 等通道推播「請至儀表板處理」的訊息時，會附上一個
可直接點擊的連結，直達該任務／審批的詳情頁（不是首頁）。這個連結怎麼組出來：

1. 優先讀 `config.toml` 的 `[dashboard] public_url`（你對外的網域，例如透過反向代理
   或 tailnet 開放時使用）；
2. 沒有設定時，退化為 `http://localhost:<[gateway] port>`——僅在使用者跟 gateway
   在同一台機器時才會真的打得開；
3. 兩者都沒有 → 不附連結，訊息文字維持原樣（不會出現空連結）。

透過反向代理或 tailnet 對外開放 dashboard 時，建議設定 `public_url`：

```toml
# ~/.duduclaw/config.toml
[dashboard]
public_url = "https://duduclaw.yourdomain.com"
```

### Telegram 內的審批詳情卡（`[miniapp] enabled`，試作，預設關閉）

`public_url` 是 **https** 時，可以再打開一個試作功能：Telegram 的高風險動作核可卡片
多一顆「🔎 查看詳情」，在對話裡直接展開完整說明、模擬後果與到期倒數，看完就地按同意
或拒絕，不用切到瀏覽器。

```toml
# ~/.duduclaw/config.toml
[miniapp]
enabled = true
```

`public_url` 不是 https、或卡片送到群組（Telegram 規定這種按鈕只能在私訊出現）時，
不會附這顆按鈕，卡片與沒開這個功能時完全相同。完整說明與安全模型見
[docs/features/43-telegram-miniapp.md](../../features/zh-TW/43-telegram-miniapp.md)。

---

## 6. Docker Compose

> **→ 詳細版：** [docs/guides/docker.md](../docker.md)，包含三大 CLI 認證設定、port 詳解、volume 備份、watchtower、疑難排解。

```bash
cd /path/to/DuDuClaw
docker compose up -d
```

```yaml
# docker-compose.yml
services:
  gateway:
    build:
      context: .
      dockerfile: container/Dockerfile.server
    ports:
      - "18789:18789"
    volumes:
      - ~/.duduclaw:/home/duduclaw/.duduclaw
    environment:
      - DUDUCLAW_HOME=/home/duduclaw/.duduclaw
    env_file:
      - .env
    restart: unless-stopped
    healthcheck:
      test: ["CMD", "curl", "-f", "http://localhost:18789/health"]
      interval: 30s
      timeout: 10s
      retries: 3
      start_period: 10s
```

`.env` 檔案：

```bash
# Required for channel bots (encrypted at rest via duduclaw onboard)
# ANTHROPIC_API_KEY=sk-ant-...  # Only if not using encrypted config
```

---

## 7. 系統服務（launchd / systemd）

```bash
# Install as system service (auto-detects OS)
duduclaw service install

# Management
duduclaw service start
duduclaw service stop
duduclaw service status
duduclaw service logs --lines 50
duduclaw service uninstall
```

`install` / `uninstall` 會註冊（或移除）**使用者層級**的開機自動啟動項目，不需要 sudo
權限，也不會影響正在執行中的 gateway；異動要到下次登入才會生效。同一個註冊也能從
儀表板切換（設定 → 一般 → 開機自動啟動），onboarding 精靈的最後一步同樣提供這個選項。

### macOS（launchd）

會建立 `~/Library/LaunchAgents/com.duduclaw.gateway.plist`

### Linux（systemd）

會建立 `~/.config/systemd/user/duduclaw.service`，並透過 `default.target.wants`
符號連結啟用（等同於 `systemctl --user enable duduclaw`）。

### Windows

會在 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` 下建立 `DuDuClaw` 值。

---

## 8. 自動更新

gateway 每 6 小時會檢查一次 GitHub Releases，儀表板（設定 → 更新）也提供手動
**檢查／安裝**流程，兩條路徑走的是同一套流水線：

1. 下載對應平台的安裝檔（`duduclaw-<platform>.tar.gz` / `.zip`）
2. 驗證 SHA-256 校驗檔**與** minisign Ed25519 簽章（`<asset>.minisig`，公鑰內建於
   執行檔中，未簽章或遭竄改的版本一律拒絕，沒有例外）
3. 驗證新執行檔可正常執行（`duduclaw version`），再原子性地就地替換（備份＋改名，
   失敗自動回滾）
4. 優雅關閉後，**在同一個行程內重新執行新版執行檔**，macOS/Linux 上會保留原本的
   PID，因此 launchd/systemd 仍會持續監控；沒有監控的前景執行方式（npm wrapper、
   `duduclaw run`）也會一併重新啟動
5. 已開啟的儀表板分頁會顯示重啟橫幅並自動重新載入

要開啟無人值守更新：

```toml
# ~/.duduclaw/config.toml
[gateway]
auto_update = true   # default: false — dashboard notification only
```

或用 `DUDUCLAW_AUTO_UPDATE=1`（環境變數優先於設定檔）。

依安裝方式而異的注意事項：

| 安裝方式 | 行為 |
|----------------|----------|
| Standalone／npm | 就地自我更新（npm registry 的中繼資料會暫時過期，直到下次 `npm i -g duduclaw`，無傷大雅） |
| Homebrew（已停止維護） | 拒絕自我更新；這個 tap 已經停用，不會再收到新版本，請改用 npm 或桌面應用程式重新安裝 |
| 原始碼（`cargo`/`target/`） | 允許自我更新，但重新編譯會覆蓋掉它 |

---

## 9. Prometheus + Grafana 監控

### Prometheus 抓取設定

```yaml
# prometheus.yml
scrape_configs:
  - job_name: 'duduclaw'
    static_configs:
      - targets: ['localhost:18789']
    metrics_path: '/metrics'
    scrape_interval: 30s
```

### 可用指標（v0.12.0+）

| 指標 | 型別 | 說明 |
|--------|------|-------------|
| `duduclaw_requests_total` | Counter | 依 agent、通道、runtime、狀態分類的總請求數 |
| `duduclaw_tokens_total` | Counter | 依 agent、類型（input/output/cache_read）分類的總 token 數 |
| `duduclaw_request_duration_seconds` | Histogram | 依 agent、runtime 分類的請求延遲 |
| `duduclaw_active_sessions` | Gauge | 目前活躍中的 session 數 |
| `duduclaw_channel_connected` | Gauge | 通道連線狀態（1/0） |
| `duduclaw_failover_total` | Counter | Provider failover 事件次數 |
| `duduclaw_budget_remaining_cents` | Gauge | 各帳號剩餘預算 |

### Grafana 儀表板

把以下 JSON 匯入 Grafana（Dashboards > Import）：

```json
{
  "dashboard": {
    "title": "DuDuClaw",
    "panels": [
      {"title": "Requests/min", "type": "stat", "targets": [{"expr": "rate(duduclaw_requests_total[5m])*60"}]},
      {"title": "Token Usage", "type": "timeseries", "targets": [{"expr": "rate(duduclaw_tokens_total[5m])*60"}]},
      {"title": "Response Time p95", "type": "stat", "targets": [{"expr": "histogram_quantile(0.95, rate(duduclaw_request_duration_seconds_bucket[5m]))"}]},
      {"title": "Channels", "type": "table", "targets": [{"expr": "duduclaw_channel_connected"}]},
      {"title": "Budget", "type": "bargauge", "targets": [{"expr": "duduclaw_budget_remaining_cents"}]}
    ]
  }
}
```

### 監控快速開始

```bash
# docker-compose with monitoring
docker compose -f docker-compose.yml -f docker-compose.monitoring.yml up -d
```

---

## 10. 企業區網部署（員工桌機 → 公司 gateway）

常見的企業架構：在共用伺服器上跑一個 gateway，讓每位員工的**桌面應用程式**透過
辦公室網路連上去。桌面應用程式內建**Gateway picker**（登入前顯示），會透過 mDNS
自動找到區網上的 gateway，員工完全不用手動輸入 IP。

### 伺服器端：在區網上廣播

gateway 會透過 mDNS/DNS-SD 以 `_duduclaw._tcp.local.` 廣播自己。廣播功能**預設
關閉**（需主動開啟），只有你刻意標記為「辦公室 gateway」的那一台才會出現在區網上，
員工的桌機與零散的實例不會意外變成可被發現的 gateway。在共用伺服器上，請開啟這個
選項並綁定到區網介面（而非 loopback）：

```toml
# ~/.duduclaw/config.toml on the gateway server
[gateway]
bind = "0.0.0.0"        # reachable on the LAN (default 127.0.0.1 = local only)
port = 18789

[general]
name = "Office Gateway"  # shown as the instance name in the desktop picker

[server]
mdns_advertise = true    # default FALSE; set true to broadcast on the LAN
tls = false              # set true when the gateway is fronted by HTTPS (below)
```

你也可以直接在儀表板的**設定 → 系統 → 伺服器**（僅限管理員）切換這些選項，不必
手動改檔案；顯示名稱、綁定介面、mDNS 開關都能在那裡編輯（綁定／廣播的異動會註明
需要重啟 gateway）。

- `mdns_advertise = false`（預設值）時，gateway 完全不會廣播；員工得在 picker
  裡手動輸入 `host:port` 才能連線。
- 環境變數 `DUDUCLAW_MDNS_ADVERTISE=0|1` 的優先權高於設定檔。桌面應用程式內建的
  sidecar 會自行設成 `=0`，所以不管 `config.toml` 怎麼寫，跑桌面應用程式的筆電
  永遠不會被廣播到網路上。
- 廣播內容的 TXT record 只帶 gateway 的**版本號**、**顯示名稱**與**`tls` 旗標**，
  不會廣播任何憑證或機密資訊。
- 廣播是盡力而為：如果 mDNS 註冊失敗（網路被鎖死、不支援 multicast），gateway
  會記一筆警告並照常提供服務。
- gateway 優雅關閉時會撤回廣播（mDNS goodbye），員工就不會繼續看到一個其實已經
  下線的 gateway。

### HTTPS 建議

mDNS 探索得到的是純 `http://<ip>:<port>` 端點，在**受信任的內部網路**上沒問題。
若流量會經過不受信任的網段（或單純想預設就做好強化），建議在 gateway 前面加一層
做 TLS 終止的反向代理（見第 5 節 Caddy/Nginx），並設定 `[server] tls = true`，
讓 picker 顯示的端點是 HTTPS。員工也可以手動輸入 HTTPS 代理的主機名稱。記得把
代理的主機名稱加進 `[gateway] allowed_origins`（見第 5 節的 WebSocket Origin
白名單），否則儀表板的 WS 升級會被拒絕。

### 員工端：桌面 Gateway picker

桌面應用程式啟動時會**自動選擇**一個 gateway 並直接連線、不會多問，只有在無法
判斷時才會退回顯示 picker：

1. 如果它**記得**某個 gateway，且該 gateway 的 `/healthz` 有回應，就直接連上去
   （不顯示 picker、不倒數）。
2. 否則會掃描區網：剛好找到**一個** gateway 就直接連上去並顯示一則簡短提示；
   找到**多個**就顯示 picker 清單；**都沒找到**就啟動並連上本機內建的 gateway。
3. 如果記得的那個 gateway 連不上，就會退回 picker 讓員工自己選。

picker 本身提供三種連線方式：

1. **本機 / Local**：應用程式自帶的內建 gateway（適合單機使用）。
2. **區網偵測 / On your network**：透過 mDNS 找到的 gateway，附重新掃描按鈕；
   每一列會顯示名稱、`host:port` 與版本號。
3. **手動輸入 / Manual**：輸入 `192.168.1.10:18789` 或 `https://gw.company.com`。
   連線前會先用 `/healthz` 驗證這個位址，位址錯誤會顯示錯誤訊息，並且**不會**跳轉。

之後若要切換 gateway，可以在應用程式的工具列選單裡使用**切換 Gateway / Switch
Gateway**，會重新打開 picker。選擇遠端 gateway 會釋放本機的 sidecar（不會留下
互相搶佔的本機實例）。只接受 `http`/`https` 位址（fail-closed，驗證失敗一律拒絕）。

> 探索機制廣播的只是一個*顯示名稱*，並不是身分驗證的邊界。登入與授權永遠由目標
> gateway 強制執行，因此就算有人偽造廣播，頂多讓你看到一個誤導性的名稱，無法
> 繞過驗證。

---

## 快速參考

| 方式 | 網址 | 使用情境 |
|--------|-----|----------|
| 純本機 | `http://localhost:18789` | 開發 |
| 企業區網 | mDNS 自動探索（桌面 picker） | 員工 → 公司 gateway |
| Tailscale | `https://xxx.ts.net` | 家用伺服器、LINE webhook |
| ngrok | `https://xxx.ngrok-free.app` | 快速展示 |
| Cloudflare | `https://duduclaw.yourdomain.com` | 正式環境 |
| Docker | `docker compose up -d` | 伺服器部署 |
| Service | `duduclaw service install` | 開機自動啟動 |
