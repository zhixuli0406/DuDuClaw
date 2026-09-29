# DocuSeal — document signing workflow

[DocuSeal](https://github.com/docusealco/docuseal) is an open-source DocuSign
alternative (cloud or self-hosted). DuDuClaw connects to it through
DocuSeal's **own official MCP server**, mounted with `[[mcp.external]]` — no
DuDuClaw-side wrapper is involved.

> **Changed in 2026-09.** DuDuClaw used to ship a first-party stdio wrapper
> crate (`duduclaw-docuseal-mcp`, 10 tools). It was never built by
> `scripts/release.sh`, so every user had to `cargo build` it themselves, and
> DocuSeal shipped its own MCP server in 2026-03. The wrapper was removed;
> use the official server below.

## Mounting the official server

DocuSeal self-hosted exposes an MCP endpoint at `https://<host>/mcp`. Generate
a bearer token in the instance's **Settings → MCP Server**, then mount it via
the [MCP Bridge](mcp-bridge.md):

```toml
[[mcp.external]]
name = "docuseal"
url = "https://sign.example.com/mcp"
headers = { Authorization = "Bearer secret://local/docuseal_mcp_token" }
allowed_tools = [
  "search_templates", "load_template", "create_template",
  "send_document", "search_documents",
]
```

Sending a document for signature is an outward-facing, semi-irreversible
action — consider putting the send tool in `[capabilities]
approval_required_tools` so it goes through HITL approval.

## What the official server covers

Five tools: search templates, load a template, create a template, send a
document for signature, search documents. It is **self-hosted only** — the
cloud tenants (`api.docuseal.com` / `.eu`) do not expose an MCP endpoint.

If you are on DocuSeal cloud, or need the wider REST surface (archiving,
resending, prefill updates, signed-document download URLs), call the
[DocuSeal REST API](https://www.docuseal.com/docs/api) directly with the
`X-Auth-Token` header — either from a small MCP server of your own or through
an agent's HTTP tooling.

## Signature completion → automatic notification (webhook)

DocuSeal's webhook can only be configured in its UI (cloud: Console →
Webhooks; self-hosted: Settings → Webhooks) — the API can't set it up for
you. Point `form.completed` / `submission.completed` at your automation entry
point, and you can chain an autopilot rule to "notify a channel / create a
task on completion." The payload envelope is
`{"event_type", "timestamp", "data"}`; the signature header is
`X-Docuseal-Signature` (`<unix_ts>.<hex_hmac>`, HMAC-SHA256 over
`<ts>.<raw_body>`, ±300s tolerance).
