# Local proxy — lend your account pool to Aider, Cline and Codex

`duduclaw proxy` puts an OpenAI-compatible HTTP endpoint on localhost that forwards to
the account pool DuDuClaw already manages. Any tool that speaks the OpenAI chat API —
Aider, Cline, Continue, Codex, a `curl` one-liner — can point at it and use the keys and
quota you have already configured, without a second copy of your credentials on disk.

---

## Quick start

```bash
duduclaw proxy --bind 127.0.0.1:8788
```

On first run with no key configured it prints a temporary Bearer key and reminds you
where to set a permanent one. Then point a client at it:

```bash
curl http://127.0.0.1:8788/v1/chat/completions \
  -H "Authorization: Bearer $DUDUCLAW_PROXY_KEY" \
  -H "Content-Type: application/json" \
  -d '{"model":"anthropic/claude-sonnet-5","messages":[{"role":"user","content":"hi"}]}'
```

Aider:

```bash
export OPENAI_API_BASE=http://127.0.0.1:8788/v1
export OPENAI_API_KEY=$DUDUCLAW_PROXY_KEY
aider --model anthropic/claude-sonnet-5
```

---

## Endpoints

| Method | Path | Auth | Notes |
|---|---|---|---|
| POST | `/v1/chat/completions` | Bearer | Streaming (SSE) and buffered |
| GET | `/v1/models` | Bearer | The vendored model catalogue |
| GET | `/healthz` | none | Liveness probe |

---

## Model names

A `provider/model` prefix decides the provider:

```
anthropic/claude-sonnet-5   → anthropic
openai/gpt-5.5              → openai
gemini/gemini-3-pro         → gemini
deepseek/deepseek-chat      → the openai-compat preset
```

A bare id with no prefix falls back to `--default-provider`, which itself defaults to
`anthropic`. So `gpt-4o` started with `--default-provider openai` resolves to
`openai/gpt-4o`. There is no `config.toml` key for this — it is a per-run flag.

---

## Authentication

A Bearer key is **always** required. Resolution order:

1. `--key <value>` on the command line
2. `DUDUCLAW_PROXY_KEY` environment variable
3. `config.toml [proxy] key`
4. Nothing set ⇒ a random key is generated and printed; it dies with the process

```toml
[proxy]
key = "ddk-proxy-…"
```

Comparison is constant-time. The default bind is loopback; binding to a routable
address exposes your whole account pool to anything that can reach the port, so put it
behind Tailscale or an SSH tunnel rather than on 0.0.0.0.

Rate limiting is per client IP, using the same token bucket the MCP HTTP server uses.

---

## Known limitation: OAuth seats are not forwarded

The account rotator holds two kinds of account: **API-key** accounts and
**subscription OAuth seats** (Claude Pro / Team / Max). Only API-key accounts can be
forwarded through this proxy. If the rotator selects an OAuth seat, the request is
refused with an explicit message rather than a silent empty completion:

> 選定帳號 `<name>`（OAuth）為訂閱制 OAuth seat，proxy 轉發需 API key 帳號（OAuth 轉發為 PENDING-LIVE）

So: add at least one API-key account before relying on the proxy. Subscription
forwarding is not implemented, and this page will say so until it is.

---

## Failure behaviour

Fail-closed throughout. No usable account produces a `503` with a zh-TW reason —
never a blank completion that a coding agent would happily treat as an answer. Upstream
errors are mapped to the nearest OpenAI-compatible error shape.

---

## Related

- [Multi-account rotation](../guides/deployment-guide.md) — configuring the account pool
  the proxy borrows from
- [Remote MCP](../guides/remote-mcp.md) — the other direction: letting an external
  client drive DuDuClaw's tools rather than its models
