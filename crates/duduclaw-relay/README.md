# duduclaw-relay

A small, standalone webhook relay + LAN device-discovery service for
DuDuClaw boxes that sit behind NAT/CGNAT and can't receive inbound webhooks
directly (e.g. a LINE Official Account webhook, which needs a public HTTPS
URL). A box opens one outbound WebSocket connection to this service; the
relay forwards inbound webhook requests to that connection as opaque bytes.

This crate is the **cloud half only**. The box-side WebSocket client that
connects to `/v1/device/ws` and consumes the wire protocol below is a
separate component, built in a later round — this service does not depend
on it and can be built/tested/deployed on its own.

## Design

- **The relay never verifies webhook signatures, never parses webhook
  payloads, and never holds a channel secret** (e.g. a LINE channel
  secret). It only forwards raw bytes + a small header allowlist to the
  device that owns them; the device is the only party that ever verifies
  anything.
- **Device auth is Ed25519 challenge-response**, trust-on-first-use at
  registration: a device generates its own keypair locally, registers the
  public half once, and thereafter proves possession of the private key on
  every WebSocket connect. The relay never sees or stores a private key.
- **At most one live connection per device.** A new authenticated
  connection evicts (kicks) whichever connection currently holds that
  device's slot.
- **An offline device never causes webhook senders to retry-storm.**
  `/v1/hook/...` always returns `200` once the request is structurally
  valid; an unreachable device's frame is placed on a small bounded
  per-device queue and flushed in order on reconnect.

## API

| Method | Path | Auth | Behavior |
|---|---|---|---|
| `GET` | `/healthz` | none | Liveness probe. Always `200 ok`, no DB dependency. |
| `POST` | `/v1/device/register` | none (TOFU + rate limit) | First-time device key registration. Body: `{"device_id","pubkey_b64","name"?}`. Re-registering the same `device_id` with the **same** key is idempotent (`200`, updates `name`); with a **different** key it's refused (`409`). Malformed `device_id`/`pubkey_b64` → `400`. |
| `GET` | `/v1/device/ws?device_id=...` | Ed25519 challenge-response | Box's long-lived relay connection. See wire protocol below. Unregistered `device_id` → `404` before upgrade. |
| `POST` | `/v1/hook/{channel}/{device_id}` | none (relay trusts nothing here by design) | Webhook entry point. `{channel}` is currently only `line`; unsupported channels → `400`. Unknown `device_id` → `404`. Body capped at 2MB (`413` over). Rate-limited per device (60/min, `429` over). Otherwise always `200 {"status":"ok"}` — forwarded live if the device is connected, queued (FIFO, cap 256/device) otherwise. |
| `GET` | `/v1/find` | none | HTML page listing online devices that last reported the **same public IP** as the caller — a "same office network" discovery convenience. Shows only device name + LAN IP, nothing secret. No claim/login flow here — clicking a link goes straight to the box's own login screen on the LAN. |

### `/v1/device/ws` wire protocol

Server → device (JSON text frames):
```json
{"type":"challenge","nonce_b64":"..."}
{"type":"ready"}
{"type":"error","message":"..."}
{"type":"hook","id":"...","channel":"line","headers":{"content-type":"...","x-line-signature":"..."},"body_b64":"...","received_at":"2026-08-18T12:00:00Z"}
```

Device → server:
```json
{"type":"auth","signature_b64":"..."}
{"type":"lan_ip","ip":"192.168.1.23"}
```

Standard WebSocket ping/pong is the heartbeat in both directions; the
server pings every 30s and closes the connection if no pong is seen for
60s. `lan_ip` reports are what `/v1/find` shows; the public IP shown
alongside it is observed directly from the WebSocket connection (via
`X-Forwarded-For` behind a trusted proxy, or the raw peer address), never
self-reported by the device.

## Build / run

```bash
# From the repo root (this crate is a workspace member):
cargo build -p duduclaw-relay
cargo test -p duduclaw-relay

RELAY_BIND=127.0.0.1:8080 RELAY_DB_PATH=./relay.db \
  cargo run -p duduclaw-relay
```

### Environment variables

| Var | Default | Notes |
|---|---|---|
| `RELAY_BIND` | `0.0.0.0:8080` (or `0.0.0.0:$PORT` if `PORT` is set) | Plain-HTTP bind address. No TLS is terminated here. |
| `RELAY_DB_PATH` | `./relay.db` | SQLite file (WAL mode). Parent directory is created if missing. |
| `RUST_LOG` | `info` | Standard `tracing-subscriber` env filter. |

### Docker

```bash
# Build context is the repo root, not this directory:
docker build -f crates/duduclaw-relay/Dockerfile -t duduclaw-relay .
docker run -p 8080:8080 -v duduclaw-relay-data:/data duduclaw-relay
```

## Deployment notes / known risks (not yet acted on — deployment itself is
a separate, later step)

- **This service assumes a single running instance.** The connection
  registry (which device is connected to which process) and the offline
  queue are in-process memory, and device state lives in a local SQLite
  file. Running more than one Cloud Run instance means a device's live
  WebSocket connection lives on exactly one instance while an inbound
  webhook can land on any instance behind the load balancer — that
  instance would have no way to know the device is connected elsewhere and
  would always queue instead of delivering live. Deploy with
  `--max-instances=1` (and consider `--min-instances=1` to avoid cold-start
  gaps) until a shared broker/store is built for horizontal scaling.
- **Cloud Run's local filesystem is not durable.** A fresh revision or a
  cold-started instance starts with an empty disk, so the SQLite file (and
  every registered device) would be lost on every deploy/restart unless a
  persistent volume is mounted. Cloud Run does not offer a real persistent
  disk; options are a Cloud Storage FUSE volume (works, but its POSIX/lock
  semantics are a known poor fit for SQLite's WAL mode under concurrent
  writers — acceptable here only because of the single-instance
  constraint above) or migrating to a managed database later. This needs a
  decision before the first production deploy.
- **Cloud Run WebSocket timeout.** Cloud Run supports WebSockets but caps
  request duration via `--timeout` (max 3600s); a box's connection will be
  cut at that ceiling regardless of activity and must reconnect — the
  30s/60s ping/pong heartbeat here only detects dead peers faster, it does
  not extend Cloud Run's own ceiling. The (separate, later) box-side client
  needs its own reconnect-with-backoff loop; the relay's eviction logic
  already makes reconnecting from scratch safe (a stale connection is
  cleanly kicked, never silently duplicated).
- **`X-Forwarded-For` trust.** Both `/v1/find` and the WS handler's
  `last_public_ip` recording trust the *last* entry of `X-Forwarded-For` as
  the value appended by Cloud Run's front end. This should be re-verified
  against the actual deployed environment before relying on it for
  anything beyond the current low-stakes use (LAN-discovery grouping and
  connection metadata, not an access-control decision).
- **Registration has no shared secret**, by design (see architecture doc)
  — safety rests on a strict `device_id` format, per-IP rate limiting, and
  refusing to overwrite an existing `device_id`'s key. This is intentional
  TOFU, not an oversight, but is worth re-confirming against the box-side
  provisioning flow once that component exists.
