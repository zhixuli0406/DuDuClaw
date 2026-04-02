# DuDuClaw Deployment Guide

> Updated: 2026-03-30 | Version: v0.10.0

---

## 1. Local Development

```bash
# Build
cargo build --release

# Run (starts gateway + channels + heartbeat + cron + dispatcher)
duduclaw run

# Access Dashboard
open http://localhost:18789
```

Default port: `18789`. Configure in `~/.duduclaw/config.toml`:

```toml
[gateway]
bind = "127.0.0.1"
port = 18789
```

### Health Check

```bash
curl http://localhost:18789/health
# {"status":"ok","version":"0.10.0","uptime_seconds":42,"agents_loaded":2,"channels_connected":["telegram","discord"]}

curl http://localhost:18789/health/ready  # 200 when agents loaded
curl http://localhost:18789/health/live   # 200 always (liveness probe)
```

---

## 2. Tailscale Funnel (Recommended for LINE Webhook)

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
tailscale funnel 18789

# This gives you a URL like:
# https://your-machine.tail12345.ts.net/
```

### Configure LINE

1. Go to [LINE Developers Console](https://developers.line.biz/)
2. Select your Messaging API channel
3. Set Webhook URL to: `https://your-machine.tail12345.ts.net/webhook/line`
4. Enable "Use webhook"
5. Verify by clicking "Verify" button

### Persistent Funnel

```bash
# Run as background service
tailscale funnel --bg 18789

# Or via systemd (Linux)
# Add to duduclaw.service After=tailscaled.service
```

---

## 3. ngrok (Alternative)

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

**Note**: Free ngrok URLs change on restart. Use `ngrok http 18789 --domain=your-domain.ngrok-free.app` with a reserved domain.

---

## 4. Cloudflare Tunnel (Long-term Stable)

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
    service: http://localhost:18789
  - service: http_status:404
EOF

# 5. Add DNS record
cloudflared tunnel route dns duduclaw duduclaw.yourdomain.com

# 6. Run
cloudflared tunnel run duduclaw
```

Set LINE Webhook: `https://duduclaw.yourdomain.com/webhook/line`

---

## 5. Reverse Proxy (Caddy / Nginx)

### Caddy (auto TLS)

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

---

## 6. Docker Compose

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

`.env` file:

```bash
# Required for channel bots (encrypted at rest via duduclaw onboard)
# ANTHROPIC_API_KEY=sk-ant-...  # Only if not using encrypted config
```

---

## 7. System Service (launchd / systemd)

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

### macOS (launchd)

Creates `~/Library/LaunchAgents/com.duduclaw.gateway.plist`

### Linux (systemd)

Creates `~/.config/systemd/user/duduclaw.service`

```bash
# Enable auto-start on login
systemctl --user enable duduclaw
```

---

## 8. Prometheus + Grafana Monitoring

### Prometheus scrape config

```yaml
# prometheus.yml
scrape_configs:
  - job_name: 'duduclaw'
    static_configs:
      - targets: ['localhost:18789']
    metrics_path: '/metrics'
    scrape_interval: 30s
```

### Available metrics (v0.12.0+)

| Metric | Type | Description |
|--------|------|-------------|
| `duduclaw_requests_total` | Counter | Total requests by agent, channel, runtime, status |
| `duduclaw_tokens_total` | Counter | Total tokens by agent, type (input/output/cache_read) |
| `duduclaw_request_duration_seconds` | Histogram | Request latency by agent, runtime |
| `duduclaw_active_sessions` | Gauge | Currently active sessions |
| `duduclaw_channel_connected` | Gauge | Channel connection status (1/0) |
| `duduclaw_failover_total` | Counter | Provider failover events |
| `duduclaw_budget_remaining_cents` | Gauge | Remaining budget per account |

---

## Quick Reference

| Method | URL | Use Case |
|--------|-----|----------|
| Local only | `http://localhost:18789` | Development |
| Tailscale | `https://xxx.ts.net` | Home server, LINE webhook |
| ngrok | `https://xxx.ngrok-free.app` | Quick demo |
| Cloudflare | `https://duduclaw.yourdomain.com` | Production |
| Docker | `docker compose up -d` | Server deployment |
| Service | `duduclaw service install` | Auto-start on boot |

## 8. Prometheus + Grafana Monitoring

### Prometheus Configuration

```yaml
# prometheus.yml
scrape_configs:
  - job_name: 'duduclaw'
    static_configs:
      - targets: ['localhost:18789']
    metrics_path: '/metrics'
    scrape_interval: 30s
```

### Available Metrics

| Metric | Type | Labels |
|--------|------|--------|
| `duduclaw_requests_total` | Counter | -- |
| `duduclaw_tokens_total` | Counter | `type` (input/output/cache_read) |
| `duduclaw_request_duration_seconds` | Histogram | bucket boundaries: 0.1s -- 10s |
| `duduclaw_active_sessions` | Gauge | -- |
| `duduclaw_channel_connected` | Gauge | `channel` |
| `duduclaw_failover_total` | Counter | -- |
| `duduclaw_budget_remaining_cents` | Gauge | `account` |

### Grafana Dashboard

Import the following JSON into Grafana (Dashboards > Import):

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

### Quick Start

```bash
# docker-compose with monitoring
docker compose -f docker-compose.yml -f docker-compose.monitoring.yml up -d
```
