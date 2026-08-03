# Notion integration (search + read + append)

Connect a Notion workspace so your AI employees can search pages and databases,
read a page's full contents, and append notes to a page. DuDuClaw talks to the
Notion REST API natively — there is no third-party MCP server to install. The
access token is stored in DuDuClaw's encrypted OAuth vault.

## What you get

Four agent-facing MCP tools, gated by two scopes (`notion:read` /
`notion:write`):

| Tool | Class | What it does |
|------|-------|--------------|
| `notion_status` | read | Connection diagnostics: connected? Reads local state only. |
| `notion_search` | read | Search pages and databases shared with the integration (Notion search syntax matches titles). Returns id/title/type/last-edited/url. |
| `notion_page_read` | read | Read one page in full: metadata plus the page body flattened to plain text (common block types — paragraph/heading/list/to-do/quote/code/callout/table; up to ~200 blocks). |
| `notion_page_append` | write | Append text as new paragraph blocks to an existing page (one block per non-empty line). Never deletes or overwrites existing content. |

### Safety design

- **Append-only writes.** `notion_page_append` only adds paragraph blocks to the
  bottom of a page; there is no delete or overwrite tool.
- **Read stays read.** The read-class tools cannot modify anything in Notion.
- **Explicit sharing required.** Your integration can only see pages/databases
  you have shared with it (in Notion: open the page → ••• → Connections → add
  your integration). Nothing else is reachable.
- **External knowledge source, not the shared wiki.** Notion content is surfaced
  for query and citation only. It is **never** copied into DuDuClaw's shared
  wiki automatically — the two knowledge stores stay separate.
- **Optional approval gate.** For extra caution, list the write tool under an
  agent's `agent.toml [capabilities] approval_required_tools`:

  ```toml
  [capabilities]
  approval_required_tools = ["notion_page_append"]
  ```

## Prerequisites: create a Notion OAuth integration

You supply your own Notion integration (DuDuClaw never ships shared
credentials). One-time setup:

1. Open [Notion → My integrations](https://www.notion.so/my-integrations).
2. Click **New integration**. Set the integration type to **Public** — only a
   public integration exposes an OAuth client ID/secret. (An internal
   integration uses a fixed token and has no OAuth flow.)
3. Under the integration's **OAuth Domain & URIs**, add exactly this redirect
   URI:

   ```
   http://localhost:18789/api/mcp/oauth/callback
   ```

4. Copy the **OAuth client ID** and **OAuth client secret**.
5. Share the pages/databases you want your AI to reach with the integration
   (open each page → ••• → Connections → select your integration).

## Connect from the dashboard

1. Go to **Manage → Integrations → Tool servers** (`/manage/integrations`).
2. Scroll to **Services that need authorization** and find the **Notion** card.
3. Click **Configure** on the card. Paste the OAuth client ID and secret — the
   dialog also shows the exact callback URL to register, which must match what
   you entered in step 3 above.
4. A Notion consent window opens. Choose the workspace and pages to grant, then
   approve. The card flips to **Authenticated**.

The client credentials are persisted (secret encrypted at rest) so
re-authorizing later does not require re-entering the secret.

## About the token

Notion access tokens are **long-lived and do not expire**, and Notion issues no
refresh token. This means:

- The connected view shows no expiry — that is normal, not a bug.
- There is nothing to refresh. If a token is ever revoked in Notion, the tools
  return a `401` and guide you to reconnect.

## Token exchange details (for the curious)

Notion's token endpoint differs from the OAuth norm: it requires **HTTP Basic
auth** (`client_id:client_secret`) plus a JSON body — not a form POST — and the
authorize URL needs `owner=user`. DuDuClaw handles this automatically in the
`notion` provider branch of the OAuth layer; you don't need to configure
anything.

## Troubleshooting

- **"Notion is not connected."** — No token stored. Connect from the dashboard.
- **`401 Unauthorized`** — The integration token was revoked. Reconnect.
- **`403` / `404` "not found"** — The page/database has not been shared with your
  integration. Share it (page → ••• → Connections) and retry.

Run `notion_status` at any time for a live diagnosis.
