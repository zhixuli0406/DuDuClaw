# DuDuClaw デプロイガイド

> 更新日：2026-03-30 ｜ バージョン：v0.10.0

---

## 1. ローカル開発

```bash
# Build
cargo build --release

# Run (starts gateway + channels + heartbeat + cron + dispatcher)
duduclaw run

# Access Dashboard
open http://localhost:18789
```

デフォルトポート：`18789`。`~/.duduclaw/config.toml` で設定します。

```toml
[gateway]
bind = "127.0.0.1"
port = 18789
```

### ヘルスチェック

```bash
curl http://localhost:18789/health
# {"status":"ok","version":"0.10.0","uptime_seconds":42,"agents_loaded":2,"channels_connected":["telegram","discord"]}

curl http://localhost:18789/health/ready  # 200 when agents loaded
curl http://localhost:18789/health/live   # 200 always (liveness probe)
```

---

## 2. Tailscale Funnel（LINE Webhook 向けに推奨）

LINE Messaging API の webhook には**公開 HTTPS URL** が必要です。Tailscale
Funnel を使えば、VPS も固定 IP もドメインも用意せずにこの URL を得られます。

### セットアップ

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

### LINE の設定

1. [LINE Developers Console](https://developers.line.biz/) を開く
2. 対象の Messaging API チャネルを選択する
3. Webhook URL を `https://your-machine.tail12345.ts.net/webhook/line` に設定する
4. 「Use webhook」を有効にする
5. 「Verify」ボタンをクリックして検証する

### 常駐 Funnel

```bash
# Run as background service
tailscale funnel --bg 18789

# Or via systemd (Linux)
# Add to duduclaw.service After=tailscaled.service
```

---

## 3. ngrok（代替手段）

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

**注意**：無料版 ngrok の URL は再起動のたびに変わります。固定したい場合は予約
ドメインを使って `ngrok http 18789 --domain=your-domain.ngrok-free.app` のよう
に実行してください。

---

## 4. Cloudflare Tunnel（長期安定運用向け）

本番運用に最適：無料で URL も安定しており、ポート転送も不要です。

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

LINE Webhook を設定：`https://duduclaw.yourdomain.com/webhook/line`

---

## 5. リバースプロキシ（Caddy / Nginx）

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

### WebSocket Origin 許可リスト（リバースプロキシ／tailnet 利用者は必読）

ダッシュボードのリアルタイム接続（WebSocket、WebChat）は、デフォルトでは
loopback（`localhost` / `127.0.0.1` / `[::1]`）からのブラウザ `Origin` しか
受け付けません。**リバースプロキシのドメイン**や **Tailscale/tailnet のアドレ
ス**からダッシュボードを開いた場合、HTTP ページ自体は正常に読み込まれますが、
WebSocket のアップグレードが 403 で拒否され、画面がずっとくるくる回り続けま
す。外部ドメインを許可リストに追加すれば解決します。

```toml
# ~/.duduclaw/config.toml
[gateway]
# host、host:port、または scheme 付きの完全な origin のいずれでも可（読み込み時に正規化されます）
allowed_origins = ["duduclaw.yourdomain.com", "box.your-tailnet.ts.net"]
```

または環境変数で指定することもできます（カンマ区切り。config.toml のリストと
は置き換えではなく**マージ**されます）。

```bash
DUDUCLAW_ALLOWED_ORIGINS="duduclaw.yourdomain.com,box.your-tailnet.ts.net"
```

- 組み込みの loopback 3 種は常に有効で、リストに書く必要はありません。リスト
  が空のときの挙動は旧バージョンと完全に同じです。
- 各エントリは host または host:port の**完全一致**で判定され、ワイルドカー
  ドには対応していません。ポートを省略したエントリは、その host の任意のポー
  トにマッチします。サフィックス攻撃（`duduclaw.yourdomain.com.evil.com`）は
  拒否されます。
- 起動時に有効な追加 origin を 1 行の info ログとして出力するので、トラブル
  シューティングに使えます。
- ダッシュボードの**設定 → システム → リモートアクセス URL**からも直接追
  加・削除でき、config.toml を手で編集する必要はありません。**保存すると即
  座に反映され、gateway の再起動は不要です**（環境変数から与えたエントリはそ
  のまま保持されます）。

### チャネルへのプッシュ通知に付くダッシュボードのディープリンク（`[dashboard] public_url`）

AI社員が LINE／Telegram／Slack などのチャネルで「ダッシュボードで対応してく
ださい」というメッセージをプッシュ通知する際、そのタスクや承認の詳細ページ
（トップページではなく）へ直接ジャンプできるリンクが添付されます。このリン
クは次のように組み立てられます。

1. まず `config.toml` の `[dashboard] public_url`（リバースプロキシや
   tailnet 経由で公開している外部ドメイン）を読み込みます。
2. 設定されていない場合は `http://localhost:<[gateway] port>` にフォール
   バックしますが、これはユーザーが gateway と同じマシン上にいる場合しか実
   際には開けません。
3. どちらも無ければリンクは付与されず、メッセージ本文はそのままです（空リ
   ンクが出ることはありません）。

リバースプロキシや tailnet 経由でダッシュボードを外部公開する場合は、
`public_url` を設定することをおすすめします。

```toml
# ~/.duduclaw/config.toml
[dashboard]
public_url = "https://duduclaw.yourdomain.com"
```

### Telegram 内の承認詳細カード（`[miniapp] enabled`、実験的機能、デフォルト無効）

`public_url` が **https** の場合、実験的な機能をもう一つ有効にできます。
Telegram の高リスク操作の承認カードに「🔎 詳細を見る」ボタンが追加され、チャ
ット内で完全な説明・シミュレーション結果・期限までのカウントダウンをその場
で展開できます。確認したらブラウザに切り替えることなく、その場で承認または
拒否できます。

```toml
# ~/.duduclaw/config.toml
[miniapp]
enabled = true
```

`public_url` が https でない場合、またはカードがグループに送信される場合
（Telegram の仕様上こうしたボタンは個人チャットにしか表示できません）は、こ
のボタンは付かず、カードは機能をオフにしている場合と全く同じになります。詳
細と安全性モデルについては
[docs/features/43-telegram-miniapp.md](../../features/ja-JP/43-telegram-miniapp.md)
を参照してください。

---

## 6. Docker Compose

> **→ 詳細版：** [docs/guides/docker.md](../docker.md) — 3 大 CLI 認証設定、ポートの詳細、ボリュームのバックアップ、watchtower、トラブルシューティングを含みます。

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

`.env` ファイル：

```bash
# Required for channel bots (encrypted at rest via duduclaw onboard)
# ANTHROPIC_API_KEY=sk-ant-...  # Only if not using encrypted config
```

---

## 7. システムサービス（launchd / systemd）

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

`install` / `uninstall` は**ユーザーレベル**の自動起動エントリを登録（また
は削除）します。sudo 権限は不要で、実行中の gateway に影響を与えることもあ
りません。変更は次回ログイン時に反映されます。同じ登録はダッシュボード（設
定 → 一般 → ログイン時に起動）からも切り替えられ、オンボーディングウィザー
ドの最終ステップでも同じオプションが提供されます。

### macOS（launchd）

`~/Library/LaunchAgents/com.duduclaw.gateway.plist` を作成します。

### Linux（systemd）

`~/.config/systemd/user/duduclaw.service` を作成し、`default.target.wants`
のシンボリックリンク経由で有効化します（`systemctl --user enable duduclaw`
と同等です）。

### Windows

`HKCU\Software\Microsoft\Windows\CurrentVersion\Run` 配下に `DuDuClaw` の値
を作成します。

---

## 8. 自動アップデート

gateway は 6 時間ごとに GitHub Releases を確認し、ダッシュボード（設定 →
アップデート）には手動の**確認／インストール**フローも用意されています。ど
ちらの経路も同じパイプラインを共有しています。

1. プラットフォームごとのアセット（`duduclaw-<platform>.tar.gz` / `.zip`）
   をダウンロードします。
2. SHA-256 のサイドカーファイルと minisign の Ed25519 署名
   （`<asset>.minisig`、公開鍵はバイナリに埋め込み済み）の両方を検証しま
   す。署名がない、または改ざんされたリリースは無条件で拒否され、回避手段
   はありません。
3. 新しいバイナリが実際に実行できるか（`duduclaw version`）を確認したうえ
   で、アトミックにその場で入れ替えます（バックアップ＋リネーム、失敗時は
   自動ロールバック）。
4. グレースフルシャットダウン後、**同一プロセス内で新しいバイナリを再実
   行**します。macOS/Linux では PID がそのまま維持されるため launchd/systemd
   の監視は継続し、監視されていないフォアグラウンド実行（npm wrapper、
   `duduclaw run`）も再起動されます。
5. 開いているダッシュボードのタブには再起動バナーが表示され、自動的にリ
   ロードされます。

無人アップデートを有効にするには：

```toml
# ~/.duduclaw/config.toml
[gateway]
auto_update = true   # default: false — dashboard notification only
```

あるいは `DUDUCLAW_AUTO_UPDATE=1`（環境変数が設定ファイルより優先されます）。

インストール方法別の注意点：

| インストール方法 | 挙動 |
|----------------|----------|
| Standalone／npm | その場で自己アップデートします（npm レジストリのメタデータは次回の `npm i -g duduclaw` まで古いままになりますが、無害です） |
| Homebrew（提供終了） | 自己アップデートを拒否します。この tap はすでに廃止され、新しいバージョンが配信されることはありません。npm かデスクトップアプリで再インストールしてください。 |
| ソースからのビルド（`cargo`/`target/`） | 自己アップデートは可能ですが、再ビルドすると上書きされます |

---

## 9. Prometheus + Grafana モニタリング

### Prometheus のスクレイプ設定

```yaml
# prometheus.yml
scrape_configs:
  - job_name: 'duduclaw'
    static_configs:
      - targets: ['localhost:18789']
    metrics_path: '/metrics'
    scrape_interval: 30s
```

### 利用可能なメトリクス（v0.12.0+）

| メトリクス | 種類 | 説明 |
|--------|------|-------------|
| `duduclaw_requests_total` | Counter | エージェント・チャネル・runtime・ステータス別の総リクエスト数 |
| `duduclaw_tokens_total` | Counter | エージェント・種類（input/output/cache_read）別の総トークン数 |
| `duduclaw_request_duration_seconds` | Histogram | エージェント・runtime 別のリクエストレイテンシ |
| `duduclaw_active_sessions` | Gauge | 現在アクティブなセッション数 |
| `duduclaw_channel_connected` | Gauge | チャネルの接続状態（1/0） |
| `duduclaw_failover_total` | Counter | プロバイダーフェイルオーバーの発生回数 |
| `duduclaw_budget_remaining_cents` | Gauge | アカウントごとの残予算 |

### Grafana ダッシュボード

以下の JSON を Grafana にインポートします（Dashboards > Import）：

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

### モニタリングのクイックスタート

```bash
# docker-compose with monitoring
docker compose -f docker-compose.yml -f docker-compose.monitoring.yml up -d
```

---

## 10. 企業 LAN 環境へのデプロイ（社員のデスクトップ → 会社の gateway）

よくある企業構成は、共有サーバー上で 1 台の gateway を稼働させ、各社員の
**デスクトップアプリ**をオフィスネットワーク経由でそこに接続させるというも
のです。デスクトップアプリには**Gateway picker**（ログイン前に表示）が組み
込まれており、mDNS 経由で LAN 上の gateway を自動的に見つけるため、社員が
IP アドレスを手入力する必要はありません。

### サーバー側：LAN 上でアドバタイズする

gateway は mDNS/DNS-SD 経由で `_duduclaw._tcp.local.` として自身をアドバタ
イズします。アドバタイズ機能は**デフォルトで無効**（オプトイン）です。意図
的に「オフィス gateway」としてマークした gateway だけが LAN 上に現れるた
め、社員のデスクトップや偶発的に立ち上がったインスタンスが誤って発見可能な
gateway になることはありません。共有サーバー側では、この機能を有効にした
うえで LAN インターフェースにバインドしてください（loopback ではなく）。

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

ファイルを編集する代わりに、ダッシュボードの**設定 → システム → サーバー**
（管理者限定）からこれらを切り替えることもできます。表示名、バインドする
インターフェース、mDNS のスイッチはすべてそこから編集できます（バインド／
ブロードキャストの変更には gateway の再起動が必要である旨が表示されます）。

- `mdns_advertise = false`（デフォルト）の場合、gateway は一切ブロードキャ
  ストせず、社員は picker で `host:port` を手入力して接続することになりま
  す。
- 環境変数 `DUDUCLAW_MDNS_ADVERTISE=0|1` は設定ファイルより優先されます。
  デスクトップアプリに同梱された sidecar 自体が `=0` を設定するため、
  `config.toml` の内容にかかわらず、デスクトップアプリを実行しているノー
  ト PC がネットワーク上にアドバタイズされることはありません。
- アドバタイズ内容の TXT レコードには gateway の**バージョン**、**表示
  名**、**`tls` フラグ**のみが含まれ、資格情報や機密情報がブロードキャス
  トされることはありません。
- アドバタイズはベストエフォートです。mDNS の登録に失敗した場合（ネット
  ワークがロックダウンされている、マルチキャスト非対応など）、gateway は
  警告をログに記録したうえで通常どおりサービスを提供します。
- gateway がグレースフルシャットダウンする際にはアドバタイズを取り下げま
  す（mDNS goodbye）。これにより、社員がすでに落ちている gateway をいつま
  でも見続けることはありません。

### HTTPS に関する推奨事項

mDNS ディスカバリーで得られるのは素の `http://<ip>:<port>` エンドポイント
で、**信頼できる内部ネットワーク**であればそれで問題ありません。信頼できな
いセグメントをまたぐ場合（あるいはデフォルトの強化策として）は、gateway の
手前に TLS を終端するリバースプロキシを置き（第 5 節の Caddy/Nginx を参
照）、`[server] tls = true` を設定して picker がエンドポイントを HTTPS と
して表示するようにしてください。社員は HTTPS プロキシのホスト名を手入力す
ることもできます。プロキシのホスト名を `[gateway] allowed_origins`（第 5
節の WebSocket Origin 許可リストを参照）に追加するのを忘れないでくださ
い。追加しないとダッシュボードの WS アップグレードが拒否されます。

### 社員側：デスクトップの Gateway picker

デスクトップアプリは起動時に**自動的に** gateway を選んで、確認なしで接続
します。判断できない場合にのみ picker にフォールバックします。

1. gateway を**記憶していて**、その gateway の `/healthz` が応答する場合
   は、そのまま即座に接続します（picker もカウントダウンも表示されませ
   ん）。
2. それ以外の場合は LAN をスキャンします。ちょうど**1 台**見つかればそれ
   に接続して短いトーストを表示し、**複数台**見つかれば picker のリスト
   を表示し、**1 台も**見つからなければローカル内蔵の gateway を起動して
   接続します。
3. 記憶していた gateway に接続できない場合は picker にフォールバックし、
   社員が自分で選べるようにします。

picker 自体は 3 通りの接続方法を提供します。

1. **本機 / Local**：アプリに内蔵された gateway（単独利用向け）。
2. **區網偵測 / On your network**：mDNS で発見された gateway。再スキャン
   ボタン付きで、各行に名前・`host:port`・バージョンが表示されます。
3. **手動輸入 / Manual**：`192.168.1.10:18789` や `https://gw.company.com`
   を入力します。接続前にそのアドレスが `/healthz` で検証され、無効なア
   ドレスの場合はエラーが表示されて画面遷移**しません**。

後で gateway を切り替えたい場合は、アプリのトレイメニューにある**切換
Gateway / Switch Gateway**を使うと picker が再度開きます。リモートの
gateway を選ぶとローカルの sidecar は解放されます（競合するローカルインス
タンスが残ることはありません）。受け付けられるのは `http`/`https` アドレ
スのみです（fail-closed、検証に失敗すれば必ず拒否されます）。

> ディスカバリー機能がアドバタイズするのは*表示名*だけであり、認証の境界
> ではありません。ログインと認可は常に接続先の gateway 側で強制されるた
> め、なりすましのアドバタイズがあったとしても、誤解を招く名前が表示され
> る程度で、認証を回避することはできません。

---

## クイックリファレンス

| 方法 | URL | 用途 |
|--------|-----|----------|
| ローカルのみ | `http://localhost:18789` | 開発 |
| 企業 LAN | mDNS 自動検出（デスクトップ picker） | 社員 → 会社の gateway |
| Tailscale | `https://xxx.ts.net` | 自宅サーバー、LINE webhook |
| ngrok | `https://xxx.ngrok-free.app` | 簡易デモ |
| Cloudflare | `https://duduclaw.yourdomain.com` | 本番運用 |
| Docker | `docker compose up -d` | サーバーデプロイ |
| Service | `duduclaw service install` | 起動時に自動実行 |
