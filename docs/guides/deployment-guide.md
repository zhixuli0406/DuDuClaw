# DuDuClaw deployment guide

Official Linux CLI binaries require **glibc ≥ 2.31**: Ubuntu 20.04+, Debian 11+, or RHEL/Rocky Linux 9+. Use Docker on older systems. This baseline applies to the CLI/gateway, including the desktop CLI sidecar; the desktop GUI also requires its own WebKitGTK/system libraries.

> Updated: 2026-03-30 | Version: v0.10.0

---

## 1. Local development

```bash
# Build
cargo build --release

# Run (starts gateway + channels + heartbeat + cron + dispatcher)
duduclaw run

# Access Dashboard
open http://localhost:18889
```

Default port: `18889`. Configure in `~/.duduclaw/config.toml`:

Upgrades do not rewrite an explicit `DUDUCLAW_PORT` or an existing
`[gateway] port`. Installations already using the former `18789` default keep
that endpoint until the operator changes it; fresh, unconfigured installs use
`18889`.

For an existing Docker data volume, keep its current port mapping and
healthcheck. To migrate, change `[gateway] port`, the Compose `ports` mapping,
and the healthcheck URL to `18889` in one maintenance window; do not replace
only the new-install Compose template.

```toml
[gateway]
bind = "127.0.0.1"
port = 18889
```

### Health check

```bash
curl http://localhost:18889/health
# {"status":"ok","version":"0.10.0","uptime_seconds":42,"agents_loaded":2,"channels_connected":["telegram","discord"]}

curl http://localhost:18889/health/ready  # 200 when agents loaded
curl http://localhost:18889/health/live   # 200 always (liveness probe)
```

---

## 2. Tailscale Funnel (recommended for LINE webhook)

LINE Messaging API requires a **public HTTPS URL** for webhooks.
Tailscale Funnel provides this without a VPS, static IP, or domain.

### Setup

```bash
# 1. Install Tailscale
brew install tailscale       # macOS
curl -fsSL https://tailscale.com/install.sh | sh  # Linux

# 2. Authenticate
tailscale up

# 3. Enable HTTPS + Funnel
tailscale funnel 18889

# This gives you a URL like:
# https://your-machine.tail12345.ts.net/
```

### Configure LINE

1. Go to [LINE Developers Console](https://developers.line.biz/)
2. Select your Messaging API channel
3. Set Webhook URL to: `https://your-machine.tail12345.ts.net/webhook/line`
4. Enable "Use webhook"
5. Verify by clicking "Verify" button

### Persistent funnel

```bash
# Run as background service
tailscale funnel --bg 18889

# Or via systemd (Linux)
# Add to duduclaw.service After=tailscaled.service
```

---

## 3. ngrok (alternative)

```bash
# 1. Install
brew install ngrok        # macOS
snap install ngrok        # Linux

# 2. Authenticate (free account)
ngrok config add-authtoken YOUR_TOKEN

# 3. Start tunnel
ngrok http 18889

# Copy the HTTPS URL (e.g., https://abc123.ngrok-free.app)
# Set as LINE Webhook URL: https://abc123.ngrok-free.app/webhook/line
```

**Note**: Free ngrok URLs change on restart. Use `ngrok http 18889 --domain=your-domain.ngrok-free.app` with a reserved domain.

---

## 4. Cloudflare Tunnel (long-term stable)

Best for production — free, stable URL, no port forwarding.

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
    service: http://localhost:18889
  - service: http_status:404
EOF

# 5. Add DNS record
cloudflared tunnel route dns duduclaw duduclaw.yourdomain.com

# 6. Run
cloudflared tunnel run duduclaw
```

Set LINE Webhook: `https://duduclaw.yourdomain.com/webhook/line`

---

## 5. Reverse proxy (Caddy / Nginx)

### Caddy (auto TLS)

```Caddyfile
duduclaw.yourdomain.com {
    reverse_proxy localhost:18889

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
        proxy_pass http://127.0.0.1:18889;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_read_timeout 86400;
    }
}
```

### WebSocket origin allowlist (required reading for reverse proxy / tailnet setups)

By default, the dashboard's live connections (WebSocket, WebChat) only accept
a browser `Origin` from loopback (`localhost` / `127.0.0.1` / `[::1]`). When you
open the dashboard through a **reverse proxy domain** or a **Tailscale/tailnet
address**, the HTTP page loads fine, but the WebSocket upgrade gets rejected
with 403 and the screen spins forever. Add the external domain to the
allowlist to fix it:

```toml
# ~/.duduclaw/config.toml
[gateway]
# host, host:port, or a full origin with scheme all work (normalized on load)
allowed_origins = ["duduclaw.yourdomain.com", "box.your-tailnet.ts.net"]
```

Or via an environment variable (comma-separated, merged with the config.toml
list rather than replacing it):

```bash
DUDUCLAW_ALLOWED_ORIGINS="duduclaw.yourdomain.com,box.your-tailnet.ts.net"
```

- The three built-in loopback entries are always allowed and don't need to be
  listed; an empty list behaves exactly like older versions.
- Each entry is an **exact** host or host:port match — no wildcards. A
  port-less entry matches that host on any port. Suffix attacks
  (`duduclaw.yourdomain.com.evil.com`) are rejected.
- On startup the gateway logs one info line with the active extra origins, for
  easier troubleshooting.
- You can also add/remove entries directly from the dashboard under
  **Settings → System → Remote access URLs**, without touching config.toml —
  changes take effect immediately on save, no gateway restart needed
  (entries from the environment variable are preserved either way).

### Dashboard deep links in channel push messages (`[dashboard] public_url`)

When an AI employee pushes a "please handle this in the dashboard" message
over LINE, Telegram, Slack, and similar channels, it attaches a clickable link
that goes straight to that task's or approval's detail page (not the home
page). Here's how that link gets built:

1. It first reads `[dashboard] public_url` from `config.toml` (your external
   domain — for example, the one exposed through a reverse proxy or tailnet);
2. If that isn't set, it falls back to `http://localhost:<[gateway] port>`,
   which only actually opens when the user is on the same machine as the
   gateway;
3. If neither is available, no link is attached — the message text stays as
   is (no empty link appears).

When you expose the dashboard externally through a reverse proxy or tailnet,
set `public_url`:

```toml
# ~/.duduclaw/config.toml
[dashboard]
public_url = "https://duduclaw.yourdomain.com"
```

### In-Telegram approval detail card (`[miniapp] enabled`, experimental, off by default)

When `public_url` is **https**, you can turn on an experimental feature: the
Telegram card for approving a high-risk action gains a "🔎 View details"
button that expands the full explanation, a simulated outcome, and an
expiry countdown right inside the chat — approve or reject on the spot,
no browser switch needed.

```toml
# ~/.duduclaw/config.toml
[miniapp]
enabled = true
```

When `public_url` isn't https, or when the card is sent to a group (Telegram
only allows this kind of button in private chats), the button is omitted and
the card is identical to having the feature off. Full details and the
security model are in
[docs/features/43-telegram-miniapp.md](../features/43-telegram-miniapp.md).

---

## 6. Docker Compose

> **→ Full guide:** [docs/guides/docker.md](./docker.md) — covers the three CLI auth setups, port details, volume backups, watchtower, and troubleshooting.

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
      - "18889:18889"
    volumes:
      - ~/.duduclaw:/home/duduclaw/.duduclaw
    environment:
      - DUDUCLAW_HOME=/home/duduclaw/.duduclaw
    env_file:
      - .env
    restart: unless-stopped
    healthcheck:
      test: ["CMD", "curl", "-f", "http://localhost:18889/health"]
      interval: 30s
      timeout: 10s
      retries: 3
      start_period: 10s
```

`.env` file:

```bash
# Required for channel bots (encrypted at rest via duduclaw onboard)
# ANTHROPIC_API_KEY=sk-ant-...  # Only if not using encrypted config
```

---

## 7. System service (launchd / systemd)

```bash
# Install as system service (auto-detects OS)
duduclaw service install

# Management
duduclaw service start
duduclaw service stop
duduclaw service restart
duduclaw service status
duduclaw service logs --lines 50
duduclaw service uninstall
```

`install` / `uninstall` register (or remove) a **user-level** autostart entry —
no sudo required — and never touch a running gateway; the change takes effect
at the next login. The same registration can be toggled from the dashboard
(Settings → General → Start at login), which the onboarding wizard also offers
on its final step.

### macOS (launchd)

Creates `~/Library/LaunchAgents/com.duduclaw.gateway.plist`

After a verified gateway update, a separate one-shot LaunchAgent automatically
re-registers and restarts the running gateway. It first allows up to 55 seconds
for the gateway to flush and drain, then unloads and bootstraps the job. The
helper runs the installed binary and bounds re-registration to 90 seconds. If
the original process has not exited within 55 seconds, it refuses forced unload.
It does not retry indefinitely. Its log is `~/.duduclaw-update-relaunch/relaunch.log`; its private
plist is removed when consumed. An exited helper registration is inert and is
replaced at the next update. If automatic relaunch fails, use
`duduclaw service restart`. If `duduclaw doctor` reports launchd
exit code 78, run that command to re-register the job with the updated binary.
The first upgrade from a version without this fix can still need a manual
restart: the new behavior starts once the fixed gateway is running.

**Folder access after an update.** macOS may ask again whether DuDuClaw may
read `~/Documents`, `~/Desktop`, `~/Downloads`, iCloud Drive or external
volumes. Until someone clicks **Allow** on that Mac, AI employees cannot read
files there; the dashboard shows a banner and admins get one channel notice.
See [macOS folder access](macos-folder-access.md) for how to re-grant it, where
to keep work files so it never matters, and the enterprise PPPC profile.

### Linux (systemd)

Creates `~/.config/systemd/user/duduclaw.service` and enables it via the
`default.target.wants` symlink (equivalent to `systemctl --user enable duduclaw`).

### Windows

Creates the `DuDuClaw` value under
`HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.

---

## 8. Auto-update

The gateway checks GitHub Releases every 6 hours and the dashboard
(Settings → Update) has a manual **Check / Install** flow. Both paths share
the same pipeline:

1. Download the platform asset (`duduclaw-<platform>.tar.gz` / `.zip`)
2. Verify the SHA-256 sidecar **and** the minisign Ed25519 signature
   (`<asset>.minisig`, public key pinned in the binary — unsigned or
   tampered releases are rejected, no override)
3. Verify the new binary executes (`duduclaw version`), then atomically
   swap it in place (backup + rename, auto-rollback on failure)
4. On macOS, a gateway managed by the standard or legacy DuDuClaw LaunchAgent
   is re-registered by the independent helper described above. Other installs
   keep graceful shutdown followed by in-process re-exec (PID preserved on
   Unix). Desktop sidecars continue to be updated and restarted by the app.
3. Verify the new binary executes (`duduclaw version`), then replace it.
   On Unix, staging is created exclusively in the install directory with mode
   `0755`, flushed, and installed by one rename over the old path, followed by
   a directory sync. A hard-link or flushed copy preserves the old binary first;
   rollback also uses one rename. Windows uses `ReplaceFileW` with a backup,
   then tries `MoveFileExW` with replacement and write-through. If a running
   executable blocks both, the rename-aside fallback restores the old file on
   a reported install failure; a crash between those moves can still leave the
   install path empty. Rollback uses native replacement only; if blocked, it reports a retained recovery copy.
   CLI self-update uses this same pipeline. Protected Linux service upgrades
   and desktop updates have separate installers.
4. Graceful shutdown, then **re-exec the new binary in-process** — the PID
   is preserved on macOS/Linux, so launchd/systemd keep supervising, and
   unsupervised foreground runs (npm wrapper, `duduclaw run`) restart too
5. Open dashboard tabs show a restart banner and reload automatically

Enable unattended updates:

```toml
# ~/.duduclaw/config.toml
[gateway]
auto_update = true   # default: false — dashboard notification only
```

Or `DUDUCLAW_AUTO_UPDATE=1` (env wins over config).

Notes by install method:

| Install method | Behavior |
|----------------|----------|
| Standalone / npm | Self-update in place (npm registry metadata goes stale until the next `npm i -g duduclaw`, harmless) |
| Homebrew (discontinued) | Self-update refuses; the tap is retired and will never receive new versions — reinstall via npm or the desktop app instead |
| Source (`cargo`/`target/`) | Self-update allowed but a rebuild will overwrite |

### Troubleshooting: dashboard update from v1.79.2 or older to v1.79.3 fails

On macOS Apple Silicon, the original v1.79.3 executable is 210,561,920 bytes
(200.8 MiB). Updaters in v1.79.2 and older reject an extracted executable at
200 MiB (209,715,200 bytes), so every attempt fails. The old dashboard may show
only “Update failed”. Raising the limit in the new release cannot change the
updater that is still running. New release builds remove symbols and check
archive sizes before publication to keep them compatible with these updaters.

For an ordinary standalone installation, replace the executable manually once:

1. Download the archive for the intended version, OS and CPU from
   [GitHub Releases](https://github.com/zhixuli0406/DuDuClaw/releases), along
   with its `.sha256` and `.minisig`. Install `minisign` first if necessary.
   Example for v1.79.3 on macOS Apple Silicon:

```sh
version=1.79.3
asset=duduclaw-darwin-arm64.tar.gz
stage=$(mktemp -d)
cd "$stage"
base="https://github.com/zhixuli0406/DuDuClaw/releases/download/v$version"
curl -fL "$base/$asset" -o "$asset"
curl -fL "$base/$asset.sha256" -o "$asset.sha256"
curl -fL "$base/$asset.minisig" -o "$asset.minisig"
shasum -a 256 -c "$asset.sha256" &&
  minisign -Vm "$asset" -x "$asset.minisig" \
    -P 'RWTh5pOpk0YmdBgm3VyB2bzxFtajNLXr7zFDhbcc75TgM8YfeV+NSzXh'
```

2. Continue only if **both** checks succeed. The public key above is the
   official release key, also pinned in the updater; do not substitute a key
   downloaded alongside the archive. Extract with `tar -xzf "$asset" duduclaw`
   and run `./duduclaw version` to confirm the version.
3. Find the executable path used by the service (the launchd plist or systemd
   unit in section 7). Resolve any symlink: npm's `duduclaw` command can point
   to a JavaScript wrapper, which must not be replaced with the native binary.
   Keep a copy of the actual executable, for example `<binary>.pre-upgrade`.
4. Stop the service and confirm it has stopped: on macOS use the installed
   executable's `service stop`; on Linux use `systemctl --user stop duduclaw`.
   For a foreground installation, stop its process in the original terminal.
   Copy the verified `./duduclaw` over the actual executable path and restore
   executable permissions (`chmod 755 <binary>`). Leave the data directory intact.
5. Start the same service: on macOS use `<binary> service restart` (reloads the
   registered LaunchAgent); on Linux use `systemctl --user start duduclaw`.
   Foreground installations use their original start command. Confirm the
   dashboard version and service status. If startup fails, stop it, restore the
   saved executable and start it again; keep the backup until verification succeeds.

Linux archives use `duduclaw-linux-x64.tar.gz` or `duduclaw-linux-arm64.tar.gz`;
use `sha256sum -c` if `shasum` is unavailable. This procedure is for ordinary
installations. Desktop apps use their app updater; protected Linux services use
[the protected service upgrade procedure](linux-protected-service.md).


---

## 9. Prometheus + Grafana monitoring

### Prometheus scrape config

```yaml
# prometheus.yml
scrape_configs:
  - job_name: 'duduclaw'
    static_configs:
      - targets: ['localhost:18889']
    metrics_path: '/metrics'
    scrape_interval: 30s
```

### Available metrics (v0.12.0+)

| Metric | Type | Description |
|--------|------|-------------|
| `duduclaw_failover_total` | Counter | Provider failover events |
| `duduclaw_goal_loop_bail_pattern_total` | Counter | Goal-loop early-bail phrases, by pattern |
| `duduclaw_tick_events_total` / `_dropped_total` | Counter | Resident-sensing observations and drops, by source |
| `duduclaw_prompt_compression_runs_total` | Counter | Prompt-compression stage runs, by stage |
| `duduclaw_relay_frames_total` | Counter | Relay WebSocket frames, by channel and outcome |

> **Removed in 2026-09.** `duduclaw_requests_total`, `duduclaw_tokens_total`,
> `duduclaw_request_duration_seconds`, `duduclaw_active_sessions`,
> `duduclaw_channel_connected` and `duduclaw_budget_remaining_cents` were
> rendered on every scrape but never incremented by any production code path —
> a Grafana panel built on them showed a flat zero, not an idle system. They
> were deleted rather than left as decoration. Per-request token and cost data
> lives in the cost-telemetry SQLite store (`cost_summary` / `cost_agents` /
> `cost_recent` MCP tools and the dashboard's cost page); channel connection
> state is on the dashboard's Channels page. The `pty_pool_*` and `worker_*`
> families went with the PTY pool in the same release.

### Grafana dashboard

Import the following JSON into Grafana (Dashboards > Import):

```json
{
  "dashboard": {
    "title": "DuDuClaw",
    "panels": [
      {"title": "Failovers/min", "type": "stat", "targets": [{"expr": "rate(duduclaw_failover_total[5m])*60"}]},
      {"title": "Sensing events", "type": "timeseries", "targets": [{"expr": "rate(duduclaw_tick_events_total[5m])*60"}]},
      {"title": "Sensing drops", "type": "timeseries", "targets": [{"expr": "rate(duduclaw_tick_dropped_total[5m])*60"}]},
      {"title": "Relay frames", "type": "table", "targets": [{"expr": "duduclaw_relay_frames_total"}]}
    ]
  }
}
```

### Monitoring quick start

```bash
# docker-compose with monitoring
docker compose -f docker-compose.yml -f docker-compose.monitoring.yml up -d
```

---

## 10. Enterprise LAN deployment (employee desktops → company gateway)

See [Personal and Enterprise editions](editions.md) for what the Enterprise edition adds.

A common enterprise setup: run one gateway on a shared server and have each
employee's **desktop app** connect to it over the office network. The desktop
app ships a **Gateway picker** (shown before login) that finds gateways on the
LAN automatically via mDNS, so employees never type an IP.

### Server side — advertise on the LAN

The gateway advertises itself over mDNS/DNS-SD as `_duduclaw._tcp.local.`.
Advertising is **off by default** (opt-in) — only a gateway you deliberately
mark as an "office gateway" appears on the LAN, so employee desktops and stray
instances never turn into discoverable gateways. On the shared server, opt in and
bind to a LAN interface (not loopback):

```toml
# ~/.duduclaw/config.toml on the gateway server
[gateway]
bind = "0.0.0.0"        # reachable on the LAN (default 127.0.0.1 = local only)
port = 18889

[general]
name = "Office Gateway"  # shown as the instance name in the desktop picker

[server]
mdns_advertise = true    # default FALSE; set true to broadcast on the LAN
tls = false              # set true when the gateway is fronted by HTTPS (below)
```

You can flip these in the dashboard under **Settings → System → Server** (admin
only) instead of editing the file — the display name, bind interface, and mDNS
switch are all editable there (bind/broadcast changes note that a gateway restart
is required).

- With `mdns_advertise = false` (the default), the gateway never broadcasts;
  employees connect by typing `host:port` manually in the picker.
- Env override: `DUDUCLAW_MDNS_ADVERTISE=0|1` takes precedence over config. The
  desktop app's own bundled sidecar sets `=0`, so a laptop running the desktop
  app is never advertised on the network regardless of its `config.toml`.
- The advertisement carries the gateway **version**, the **display name**, and a
  **`tls` flag** in its TXT record; no credentials or secrets are broadcast.
- Advertising is best-effort: if mDNS registration fails (locked-down network,
  no multicast), the gateway logs a warning and serves normally.
- On graceful shutdown the gateway withdraws the advertisement (mDNS goodbye), so
  employees stop seeing a gateway that has gone away.

### HTTPS recommendation

mDNS discovery yields a plain `http://<ip>:<port>` endpoint, which is fine on a
**trusted internal network**. For anything crossing untrusted segments — or as a
default hardening step — front the gateway with a reverse proxy that terminates
TLS (see §5 Caddy/Nginx) and set `[server] tls = true` so the picker shows the
endpoint as HTTPS. Employees can also type the HTTPS proxy hostname manually.
Remember to add the proxy hostname to `[gateway] allowed_origins` (see §5's
WebSocket Origin allowlist) or the dashboard WS upgrade will be rejected.

### Employee side — the desktop Gateway picker

On launch the desktop app **auto-selects** a gateway and connects without asking,
falling back to a picker only when it can't decide:

1. If it **remembers** a gateway and that gateway's `/healthz` responds → connect
   to it straight away (no picker, no countdown).
2. Otherwise it scans the LAN: exactly **one** gateway found → connect to it and
   show a brief toast; **several** found → show the picker list; **none** found →
   start and connect to the local bundled gateway.
3. If the remembered gateway is unreachable, it falls to the picker so the
   employee can choose.

The picker itself offers three ways to connect:

1. **本機 / Local** — the app's own bundled gateway (for solo use).
2. **區網偵測 / On your network** — gateways discovered via mDNS, with a rescan
   button. Each row shows name, `host:port`, and version.
3. **手動輸入 / Manual** — type `192.168.1.10:18889` or `https://gw.company.com`.
   The address is validated against `/healthz` before connecting; a bad address
   shows an error and does **not** navigate.

To switch gateways later, use **切換 Gateway / Switch Gateway** in the app's tray
menu — it reopens the picker. Choosing a remote gateway releases the local
sidecar (no competing local instance is left running). Only `http`/`https`
addresses are accepted (fail-closed).

> Discovery only advertises a *display name* — it is not an authentication
> boundary. Login and authorization are always enforced by the target gateway,
> so a spoofed advertisement can at most show a misleading name, never bypass
> auth.

---

## Quick reference

| Method | URL | Use Case |
|--------|-----|----------|
| Local only | `http://localhost:18889` | Development |
| Enterprise LAN | mDNS auto-discovery (desktop picker) | Employees → company gateway |
| Tailscale | `https://xxx.ts.net` | Home server, LINE webhook |
| ngrok | `https://xxx.ngrok-free.app` | Quick demo |
| Cloudflare | `https://duduclaw.yourdomain.com` | Production |
| Docker | `docker compose up -d` | Server deployment |
| Service | `duduclaw service install` | Auto-start on boot |
