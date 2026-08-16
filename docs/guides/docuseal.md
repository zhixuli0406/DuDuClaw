# DocuSeal — document signing workflow

[DocuSeal](https://github.com/docusealco/docuseal) is an open-source DocuSign
alternative (cloud or self-hosted). DuDuClaw ships `duduclaw-docuseal-mcp`, an
open-source MCP stdio wrapper that lets an agent run the full "generate
contract → send for signature → check status → retrieve the signed file"
flow.

## Two paths, and how to choose

| Path | Fits | Auth |
|---|---|---|
| **`duduclaw-docuseal-mcp` (this wrapper)** | Supports **both** cloud (api.docuseal.com / .eu) and self-hosted; broader tool surface (archiving, resending, prefill updates, signed-document URLs) | `X-Auth-Token` API key |
| **DocuSeal's official built-in MCP** (since 2026-03) | Self-hosted only; 5 tools (search/load/create template, send, search documents) | Bearer token generated from the instance's Settings → MCP Server, `url = "https://<host>/mcp"` mounted directly via the [MCP Bridge](mcp-bridge.md) |

## The wrapper's 10 tools

`docuseal_list_templates`, `docuseal_get_template`,
`docuseal_create_template_from_pdf` (base64 or URL; text tags like
`{{field;role=Signer1;type=signature}}` inside the PDF place fields
automatically),
`docuseal_create_submission` (send for signature, returns each signer's
signing link `embed_src`),
`docuseal_get_submission` (status + events + `audit_log_url`),
`docuseal_list_submissions`, `docuseal_archive_submission`,
`docuseal_get_submission_documents` (download URLs for the completed signed
documents),
`docuseal_resend_submitter_email`, `docuseal_update_submitter` (prefill /
contact updates).

## Configuration

Environment variables:

| Variable | Description |
|---|---|
| `DOCUSEAL_API_KEY` | Required. Get it from <https://console.docuseal.com/api> on cloud; from the instance's API settings when self-hosted |
| `DOCUSEAL_BASE_URL` | Optional. Defaults to `https://api.docuseal.com`; EU cloud is `https://api.docuseal.eu`; self-hosted is `https://<host>/api` |

Mounting in `agent.toml` (stdio):

```toml
[[mcp.external]]
name = "docuseal"
command = "duduclaw-docuseal-mcp"
env = { DOCUSEAL_API_KEY = "secret://local/docuseal_api_key" }
# self-hosted: also add DOCUSEAL_BASE_URL = "https://sign.example.com/api"
allowed_tools = [
  "docuseal_list_templates", "docuseal_get_template",
  "docuseal_create_submission", "docuseal_get_submission",
  "docuseal_get_submission_documents", "docuseal_resend_submitter_email",
]
```

Sending and archiving are outward-facing, semi-irreversible actions —
consider putting `docuseal_create_submission` and
`docuseal_archive_submission` in `[capabilities] approval_required_tools`
for HITL approval.

## Signature completion → automatic notification (webhook)

DocuSeal's webhook can only be configured in its UI (cloud: Console →
Webhooks; self-hosted: Settings → Webhooks) — the API can't set it up for
you. Point `form.completed` / `submission.completed` at your automation
entry point, and you can chain an autopilot rule to "notify a channel /
create a task on completion." The payload envelope is
`{"event_type", "timestamp", "data"}`; the signature header is
`X-Docuseal-Signature` (`<unix_ts>.<hex_hmac>`, HMAC-SHA256 over
`<ts>.<raw_body>`, ±300s tolerance).

## Local verification

```sh
cargo build -p duduclaw-docuseal-mcp
printf '%s\n%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
  | DOCUSEAL_API_KEY=test ./target/debug/duduclaw-docuseal-mcp
```

The second response should list all 10 `docuseal_*` tools. Actual API calls
(`tools/call`) need a valid key; HTTP-layer failures come back to the agent
as `isError: true` without crashing the server.
