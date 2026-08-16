# Wrist and wearables: Shortcuts, and AI recording pendant ingestion

> Prerequisite: `duduclaw http-server --bind 127.0.0.1:8765` is already running, and you have a
> Bearer key from `duduclaw mcp issue-refresh-token` or `~/.duduclaw/mcp_keys.toml`. Your phone
> or wearable needs to be able to reach that address (a home network IP, or Tailscale).

## 1. iPhone / Apple Watch Shortcuts (no app store, no review)

Shortcuts' "Get Contents of URL" action can call DuDuClaw's HTTP API directly; turn on
"Show on Apple Watch" for a shortcut and it becomes a one-tap watch face complication.

**Shortcut A: ask your AI employee** (voice dictation → reply as a notification)
1. "Dictate Text"
2. "Get Contents of URL": POST `http://<gateway>:8765/mcp/v1/call`
   - Headers: `Authorization: Bearer <key>`, `Content-Type: application/json`
   - Body (JSON):
     ```json
     {"jsonrpc":"2.0","id":"1","method":"tools/call",
      "params":{"name":"send_message","arguments":{"content":"聽寫文字"}}}
     ```
3. "Show Notification" with the response content.

**Shortcut B: save a memory**: same as above, but swap `params.name` for `memory_store` and put
the dictated text in `arguments.content`.

> Approval decisions are even simpler on the wrist: skip the shortcut entirely and just
> **reply** to the LINE/Telegram approval card with "Agree" (同意) or "Reject" (拒絕) — this
> works across four channels (see the CHANGELOG).

## 2. Bee wristband (Amazon): configuration only, nothing to build

Bee's official CLI is itself an MCP server, so mounting it into an agent's external MCP list is
enough; the agent can then query your conversation summaries directly:

```toml
# agent.toml
[[mcp.external]]
name = "bee"
command = "bee"          # Bee CLI (install and log in per the official docs)
args = ["mcp"]
# Tool surface is deny-by-default as usual; enable only what you need
allow = ["get_conversations", "get_todos"]
```

See [mcp-bridge.md](mcp-bridge.md) for how external MCP mounting works.

## 3. Omi / Plaud: webhook straight into memory

The gateway's `POST /ingest/transcript` is a thin adapter built for wearable-vendor webhooks: it
shares the same Bearer key and the same `memory_store` write pipeline (scope checks and origin
binding still apply as normal). Accepted fields: `text` / `transcript` / `summary` /
`segments[].text`.

- **Omi**: App → Developer → Integration webhook, set it to
  `https://<your-public-endpoint>/ingest/transcript` (if you need `Authorization: Bearer <key>`,
  set it via its header configuration; if your Omi version doesn't support custom headers, put a
  Cloudflare Worker in front to add the header)
- **Plaud**: Developer Platform → Webhooks, point it at the same endpoint (do signed-webhook
  verification at your reverse-proxy layer, or just trust the Bearer key directly)
- Manual test:
  ```bash
  curl -X POST http://127.0.0.1:8765/ingest/transcript \
    -H "Authorization: Bearer <key>" -H "Content-Type: application/json" \
    -d '{"source":"plaud","summary":"客戶說週五前要報價"}'
  # → {"stored":true,...}
  ```

Memories written this way carry a `wearable,<source>` tag and an external origin (the trust
ceiling still follows v1.41 origin binding, so a wearable transcript is never treated as a
high-trust fact).

## Security notes

- The key only grants access to MCP's external allow-listed tools (memory/wiki/send_message, 7
  in total); if it leaks, revoke and reissue with `duduclaw mcp`.
- Before exposing `/ingest/transcript` to the public internet, confirm the rate limit
  (60 req/min/key) and the origin restrictions at your reverse-proxy layer.
