# Google Workspace integration (all eight services, native)

> **開放狀態**：本整合在目前版本預設隱藏（原廠 Google OAuth App 驗證進行中），將於後續版本正式開放。操作者可在 `config.toml` 加 `[integrations] google_workspace = true` 搶先啟用（dashboard 分頁需同版本開關）。

Connect a Google account so your AI employees can search and read mail, prepare
draft replies, list your calendar, create events (with Google Meet links), and
read/append rows in your Google Sheets, read Google Forms responses, and manage
Google Tasks. DuDuClaw talks to the Google REST APIs natively — there is no
third-party MCP server to install. The access token is stored in DuDuClaw's
encrypted OAuth vault and refreshed automatically.

**All eight Workspace services are covered natively** — Gmail, Calendar,
Sheets, Drive, Docs, Slides, Forms, Tasks — on GA REST APIs, so nothing here
depends on Google's Developer Preview program and any customer can use it.
(Google's own remote MCP servers cover six of the eight but are Preview-only,
and their terms forbid exposing Pre-GA APIs to users outside your own domain.
They remain available as an advanced opt-in: [google-mcp.md](google-mcp.md).)

## What you get

Nineteen agent-facing MCP tools, gated by two scopes (`google:read` /
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
| `forms_get` | read | Read a Form's structure: title, description, and every question with its `question_id`, type and choice options. |
| `forms_list_responses` | read | List a Form's submitted responses (up to 50). Answers are keyed by `question_id` — pair with `forms_get` to map ids to titles. |
| `gtasks_lists` | read | List the account's Google Tasks lists (id + title). `@default` targets the default list without a lookup. |
| `gtasks_list` | read | List tasks in one list (pending only by default; `show_completed=true` includes finished + hidden). |
| `gtasks_create` | write | Create a real task in the user's Google Tasks. |
| `gtasks_complete` | write | Mark a task completed. |
| `drive_search` | read | Search Drive by file **name and full text** (trashed files excluded, newest first). Optional exact MIME filter. |
| `drive_read` | read | Read a Drive file as text: Docs/Slides export as plain text, Sheets as CSV (**first sheet only**), text-like blobs verbatim. Binary types return metadata + a note, never binary content. |
| `docs_read` | read | Read a Google Doc's text in document order, including table cell text. |
| `docs_append` | write | Append text to the **end** of a Doc. Append-only — no tool rewrites or deletes existing content. |
| `slides_read` | read | Read a presentation's text slide by slide (shapes, grouped shapes, table cells). |

> **No Slides write tool** ships on purpose: DuDuClaw's office document suite
> already produces real `.pptx` files, which is both safer and better output
> than driving the Slides `batchUpdate` element API.

> **Naming:** the Google Tasks tools are `gtasks_*`. DuDuClaw's own task board
> keeps `tasks_*` (`tasks_list` / `tasks_create` / `tasks_complete` / …) — two
> separate systems, deliberately distinct prefixes so an agent never confuses
> "my work queue" with "the user's Google Tasks".

**No official MCP server exists for Forms or Tasks** (verified 2026-07-30: both
`formsmcp`/`tasksmcp` endpoints 404 and neither appears in Google's MCP docs),
which is why they are served natively here.

### Safety design

- **Drafts never send.** `gmail_create_draft` only saves a draft; there is no
  "send" tool. Delivery is always a human decision.
- **Read stays read.** The read-class tools cannot modify anything in Gmail or
  Calendar.
- **Forms, Drive and Slides are read-only.** No tool creates or edits a form,
  writes to Drive, or modifies a presentation. Only Gmail (draft), Calendar,
  Sheets, Docs (append) and Tasks have write tools.
- **Least privilege.** Drive is requested `drive.readonly` (never `drive` or
  `drive.file` — nothing creates Drive files) and Slides
  `presentations.readonly`. Docs needs full `documents` only because
  `docs_append` writes.
- **Optional approval gate.** For extra caution, list the write tools under an
  agent's `agent.toml [capabilities] approval_required_tools` so each draft,
  event, or spreadsheet write waits for HITL approval:

  ```toml
  [capabilities]
  approval_required_tools = ["gmail_create_draft", "calendar_create_event", "sheets_append", "gtasks_create", "gtasks_complete", "docs_append"]
  ```

## Prerequisites: create a Google OAuth client

You supply your own Google OAuth client (DuDuClaw never ships shared
credentials). One-time setup:

1. Open the [Google Cloud Console → Credentials](https://console.cloud.google.com/apis/credentials)
   page (create/select a project first).
2. Enable these eight APIs for the project (APIs & Services → Library), or in
   one command:

   ```bash
   gcloud services enable gmail.googleapis.com calendar-json.googleapis.com \
     sheets.googleapis.com drive.googleapis.com docs.googleapis.com \
     slides.googleapis.com forms.googleapis.com tasks.googleapis.com \
     --project=PROJECT_ID
   ```
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
https://www.googleapis.com/auth/drive.readonly
https://www.googleapis.com/auth/documents
https://www.googleapis.com/auth/presentations.readonly
https://www.googleapis.com/auth/forms.body.readonly
https://www.googleapis.com/auth/forms.responses.readonly
https://www.googleapis.com/auth/tasks
https://www.googleapis.com/auth/userinfo.email
```

> **Scope change (v1.45):** the `spreadsheets` scope was added for the Sheets
> tools. A Google account connected before v1.45 will get a `403` from the Sheets
> APIs because its token predates this scope — `google_status` flags the missing
> scope, and reconnecting from the dashboard re-consents with the full set.
>
> **Scope change (v1.47):** Drive (`drive.readonly`), Docs (`documents`),
> Slides (`presentations.readonly`), Forms (`forms.body.readonly`,
> `forms.responses.readonly`) and Tasks (`tasks`) were added for the new native
> tools. Same rule as above — an older token yields `403` with re-auth
> guidance; reconnect from Integrations → Google to re-consent.

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
