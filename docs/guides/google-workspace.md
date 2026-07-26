# Google Workspace integration (Gmail + Calendar + Sheets)

> **開放狀態**：本整合在目前版本預設隱藏（原廠 Google OAuth App 驗證進行中），將於後續版本正式開放。操作者可在 `config.toml` 加 `[integrations] google_workspace = true` 搶先啟用（dashboard 分頁需同版本開關）。

Connect a Google account so your AI employees can search and read mail, prepare
draft replies, list your calendar, create events (with Google Meet links), and
read/append rows in your Google Sheets. DuDuClaw talks to the Google REST APIs
natively — there is no third-party MCP server to install. The access token is
stored in DuDuClaw's encrypted OAuth vault and refreshed automatically.

## What you get

Eight agent-facing MCP tools, gated by two scopes (`google:read` /
`google:write`):

| Tool | Class | What it does |
|------|-------|--------------|
| `google_status` | read | Connection diagnostics: connected?, granted scopes, token validity. Reads local state only. |
| `gmail_search` | read | Search the mailbox using Gmail query syntax (`from:… is:unread`, etc.). Returns sender/subject/date/snippet. |
| `gmail_read` | read | Read one message in full: headers, plain-text body (truncated if long), attachment manifest (filename + size only — never downloaded). |
| `gmail_create_draft` | write | Create a Gmail **draft**. Never sends — sending stays a manual human action. |
| `calendar_list_events` | read | List primary-calendar events (defaults to the next 7 days). |
| `calendar_create_event` | write | Create a real, externally-visible event; optional Google Meet link. |
| `sheets_read` | read | Read a cell range from a spreadsheet (accepts a spreadsheet ID or a full sheet URL). Returns up to 200 rows of formatted values. |
| `sheets_append` | write | Append one row to a spreadsheet using `USER_ENTERED` input (numbers/dates/formulas parsed as if typed). |

### Safety design

- **Drafts never send.** `gmail_create_draft` only saves a draft; there is no
  "send" tool. Delivery is always a human decision.
- **Read stays read.** The read-class tools cannot modify anything in Gmail or
  Calendar.
- **Least privilege.** Only the scopes below are requested. Drive is not
  requested (no Drive tools ship).
- **Optional approval gate.** For extra caution, list the write tools under an
  agent's `agent.toml [capabilities] approval_required_tools` so each draft,
  event, or spreadsheet write waits for HITL approval:

  ```toml
  [capabilities]
  approval_required_tools = ["gmail_create_draft", "calendar_create_event", "sheets_append"]
  ```

## Prerequisites: create a Google OAuth client

You supply your own Google OAuth client (DuDuClaw never ships shared
credentials). One-time setup:

1. Open the [Google Cloud Console → Credentials](https://console.cloud.google.com/apis/credentials)
   page (create/select a project first).
2. Enable the **Gmail API**, **Google Calendar API**, and **Google Sheets API**
   for the project (APIs & Services → Library).
3. Configure the OAuth consent screen (External or Internal). Add your own
   Google account as a test user if the app stays in "Testing".
4. Create an **OAuth client ID** of type **Web application**.
5. Under **Authorized redirect URIs**, add exactly:

   ```
   http://localhost:3000/api/mcp/oauth/callback
   ```

6. Copy the generated **Client ID** and **Client secret**.

The requested scopes are:

```
https://www.googleapis.com/auth/gmail.readonly
https://www.googleapis.com/auth/gmail.compose
https://www.googleapis.com/auth/calendar.events
https://www.googleapis.com/auth/spreadsheets
https://www.googleapis.com/auth/userinfo.email
```

> **Scope change (v1.45):** the `spreadsheets` scope was added for the Sheets
> tools. A Google account connected before v1.45 will get a `403` from the Sheets
> APIs because its token predates this scope — `google_status` flags the missing
> scope, and reconnecting from the dashboard re-consents with the full set.

## Connect from the dashboard

1. Go to **Integrations → Google** (`/manage/integrations?tab=google`).
2. Paste the Client ID and Client secret, then click **Connect Google**.
3. A Google consent window opens. Approve access. The window confirms success
   and the dashboard flips to **Google is connected**.

The client credentials are persisted (secret encrypted at rest) so the access
token can be refreshed automatically, and so re-authorizing later does not
require re-entering the secret.

To disconnect, click **Disconnect** on the connected view. Your stored client
credentials are kept so you can reconnect in one click; the access token is
removed.

## How refresh works

Google issues a refresh token only when the authorization requests offline
access, so the connect flow adds `access_type=offline&prompt=consent`
automatically for Google. When the access token expires, `get_valid_google_token`
runs a refresh grant with your stored client credentials, saves the new token,
and continues. If refresh is not possible (no refresh token, or missing stored
credentials), the tools return a clear message directing you back to the
Integrations → Google page to reconnect.

## Re-authorizing after a scope change

A token authorized before this integration shipped (older scope set) will get a
`403` from the new write APIs. The tools detect this and return guidance listing
the scopes to grant. Reconnect from Integrations → Google to re-consent with the
current scopes.

## Troubleshooting

- **"Google is not connected."** — No token stored. Connect from the dashboard.
- **`401 Unauthorized`** — The authorization was revoked or is invalid.
  Reconnect.
- **`403` with a scope list** — The token is missing required scopes. Reconnect
  to re-consent.
- **Redirect URI mismatch during consent** — The redirect URI in your Google
  OAuth client must be exactly `http://localhost:3000/api/mcp/oauth/callback`.

Run `google_status` at any time for a live diagnosis.
