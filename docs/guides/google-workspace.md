# Google Workspace integration (all eight services, native)

> **Visibility status (updated in v1.49.0):** the dashboard's **Integrations →
> Google** tab is now **visible by default**. Earlier versions hid it pending
> Google's OAuth app verification, but verification turns out to gate only the
> "bring your own OAuth client" path (domain-wide delegation via service
> account and the Apps Script bridge are unaffected), so there was no reason to
> keep the tab hidden. Being visible does not mean the tools are active,
> though: there is still a separate master switch, `config.toml
> [integrations] google_workspace`, **defaulting to `false`**. Without it,
> credentials can be configured and the connection test can pass, but the
> tools never reach your AI employees, and the dashboard shows a clear yellow
> warning. For a guided walkthrough of the three credential paths, see
> [google-workspace-integration.md](google-workspace-integration.md).

> **Design decision (D5, 2026-08-04):** DuDuClaw does not ship shared Google
> OAuth credentials. Users supply their own OAuth client (or use the DWD /
> Apps Script paths instead), and DuDuClaw only stores and refreshes the
> resulting token — it never bundles a client id/secret of its own. See
> [google-workspace-integration.md](google-workspace-integration.md) for the
> decision context (D5).

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

## Choosing a credential path

Three ways to authorize the same nineteen tools. They differ in who has to set
something up, and in whether Google needs to have verified an app first.

| | Personal @gmail.com | Workspace domain | Who sets it up | Tool coverage |
|---|---|---|---|---|
| **OAuth client** (below) | ✅ | ✅ | each customer creates their own Google Cloud OAuth client | all 19 |
| **Service account + domain-wide delegation** | ❌ | ✅ | the domain's super admin authorizes one client id | all 19 |
| **Apps Script bridge** | ✅ | ✅ (unless the admin disables Apps Script) | the end user deploys a script in their own account | Gmail / Calendar / Sheets only |

When more than one is configured the order of precedence is service account →
OAuth vault → Apps Script bridge; the bridge sits last because it covers the
fewest tools. `google_status` names the source actually in effect.

The two credential-free paths are documented in
[google-no-oauth-client.md](google-no-oauth-client.md). To give each AI
employee its own colleague's account, see
[Per-employee Google accounts](#per-employee-google-accounts-company-delegation-unreleased).

## Per-employee Google accounts (company delegation, unreleased)

By default every AI employee uses the one Google account set up below (the
company's shared account). A Google Workspace customer can instead let each
employee act as one named colleague: the sales assistant reads and drafts in
`alice@customer.com`'s mailbox, the scheduling assistant works in
`bob@customer.com`'s calendar. One service account with domain-wide
delegation serves all of them; each employee names the colleague it acts as.

### What you need

- A Google Workspace domain and a super admin who can authorize one client
  id (Admin console → Security → Access and data control → API controls →
  Manage Domain Wide Delegation) for the full scope list the dashboard shows.
- Credential protection in the system keychain (Security page, see
  [Security defense](../features/05-security-defense.md#person-credentials-in-the-os-keychain-unreleased)).
  The service-account key is sealed with that protection key, so it is never
  a file an AI employee can read. macOS and Windows qualify. On Linux and in
  Docker there is no supported keychain yet, and acting as a colleague is
  refused.

### Set it up

1. **Security → Credential protection → Move to system keychain.**
2. **Integrations → Google → Company delegation → Import key file.** Pick
   the service-account JSON key. The gateway checks it, seals it and stores
   it as `<home>/protected/google-service-account.sealed` (owner-only). Delete
   the original file afterwards. The panel shows the client id and the scope
   list to hand to the super admin.
3. **Allowed domains.** Only colleagues in these domains can be named
   (exact domain match, no subdomains). Left empty, only the domain of the
   home service account's `subject` is allowed; with neither, nobody is.
4. **Employee page → Connections → Change → Act as a colleague (domain
   delegation)**, enter the colleague's work email, save.
5. **Test connection** mints a token for that colleague with the full scope
   list, checks Gmail reports the same address, then mints per-service scopes.
   Gmail, Calendar, Drive and Tasks also read a read-only endpoint; Sheets,
   Docs, Slides and Forms currently verify issuance only, not service access.
   It sends nothing and does not reuse or fill
   the token cache.

The account card's "Configured — test connection" badge is a local setup
check, not proof the sealed key opens or Google accepts the grant. Every
native delegated tool call asks the gateway to recheck current settings,
requester permissions and account status; only the gateway caches minting.

The same choices are available as RPCs (all Admin): `google.delegation.get`,
`google.delegation.import_key`, `google.delegation.remove_key`,
`google.delegation.settings_set`, `agents.google_identity_set`,
`google.identity.test`. `agents.google_identity_get` needs Viewer access to
the employee.

### Settings

```toml
# agent.toml (admin-set; employees cannot edit it with Write/Edit)
[google]
identity = "delegate:alice@customer.com"  # home | none | delegate:<email>
fallback = "none"          # none (default) | home
requesters = "bound_people" # bound_people (default) | owner_only | anyone

# config.toml
[integrations.google_identity]
default_identity = "home"              # what employees without [google] use: home (default) | none
allowed_subject_domains = ["customer.com"]
```

An employee without `[google]` uses `default_identity`, so an existing
installation behaves exactly as before. `none` hides the 19 Google tools
from that employee and refuses calls to them. A value that cannot be read
(a typo, an unknown key) refuses Google for that employee; it never turns
into the shared account. Enterprise employees may use `owner` or `user:<id>`
with an active direct Owner and that person's explicit consent; see
[Personal Google connections (unreleased)](google-personal-connections.md).

### Who can use the account through the employee

The colleague's mailbox is a person's data, so the employee only uses it for
some requesters (`requesters`):

| Setting | Allowed |
|---|---|
| `bound_people` (default) | People bound to this employee (any access level) and Admins, in the dashboard, WebChat or a verified channel binding; scheduled and system work (cron, heartbeat, Agent Mail); work delegated from one of those people |
| `owner_only` | The colleague's own DuDuClaw account and scheduled work |
| `anyone` | Anyone who can talk to the employee (needs `confirm_anyone: true` when set over RPC) |

Public website visitors (WebChat widget, Odoo Live Chat) are refused under
every setting. An unbound channel sender is refused unless `anyone` is set.

### When it cannot be used

The tool call fails with a reason and who can fix it. It does not quietly
use the shared account. The reasons: the caller is not a verified employee;
the requester is not allowed; the email is outside the allowed domains; the
colleague's DuDuClaw account is suspended or offboarded; credential
protection is not ready (or its key was replaced, in which case the sealed
key no longer opens: reset protection and import the key again); no key was
imported; Google rejected the delegation (`unauthorized_client` usually means
the scope list in the Admin console is incomplete, `invalid_grant` that the
account does not exist or is disabled); the gateway is not running.

With **"If the colleague's account cannot be used, use the company's shared
account"** ticked (`fallback = "home"`), the last group (no key imported,
colleague suspended, Google rejected or unreachable) uses the shared account
instead and the result starts with a note saying so. A refused requester, a
setting error and a protection problem never fall back.

### What else changes

- **`google_status`** starts with who the employee acts as. For a colleague
  it reports whether a token can be obtained right now instead of the shared
  account's details.
- **Agent Mail** reads the mailbox as the identity of the employee that owns
  arriving mail. When that is a colleague, the Mailbox pages are visible only
  to Admins and that colleague (behaviour change; a shared-account mailbox
  keeps the Manager gate). Agent Mail is still one mailbox per installation.
- **External Google MCP servers** (`bearer_token = "oauth://google"`) are
  skipped for an employee acting as a colleague in this release, because the
  bearer would otherwise be the shared account's token.
- **Audit:** every token the gateway issues or refuses is a `google_token`
  row in `tool_calls.jsonl` (employee, identity, account, requester kind,
  tool, refusal code); every Google tool call on a colleague's identity is a
  `google_call` row. Neither ever contains a token. Changing an employee's
  identity writes `agent.google_identity_set` to the account audit log and
  `agent_google_identity_set` to `security_audit.jsonl`.

### Limits

- `gmail.compose` lets whoever holds the access token send mail. The tools
  only create drafts, but an employee with an unrestricted shell can call
  Google directly with a token it was issued for its own colleague.
- An employee with a shell runs as the same OS user as the gateway; see the
  keychain limits in [Security defense](../features/05-security-defense.md#person-credentials-in-the-os-keychain-unreleased).
- Not yet verified against a real Workspace tenant: the exact Admin console
  flow, Google's real error strings, a Gmail-disabled organizational unit,
  and whether Google's own remote MCP endpoints accept delegated tokens.

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
   http://localhost:18789/api/mcp/oauth/callback
   ```

   18789 is the gateway's default port. If you run it elsewhere (`DUDUCLAW_PORT`),
   register that port instead — the dashboard's setup step shows the exact URI,
   derived from the port the gateway is actually listening on. A mismatch here
   is silent: Google redirects the browser to a port with nothing on it, so the
   token never arrives and the page stays on "not connected".

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
  OAuth client must be exactly `http://localhost:18789/api/mcp/oauth/callback`.

Run `google_status` at any time for a live diagnosis.
