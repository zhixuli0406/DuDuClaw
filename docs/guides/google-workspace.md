# Google Workspace tools and security

Use **Integrations → Google → Drive writing** for the four actions, approval mode per action and folder list. Its readiness panel shows delegated scopes, a per-employee test and company-account consent. Members connect through **My connections → Drive writing** and review write confirmation cards there. For records, open **Logs → History → Drive writing**.

For setup, use [Connect Google](google-setup.md). Native REST tools cover nine services and thirty-eight tools. The Apps Script path covers three services and eight actions including status. The dashboard remains visible with the integration disabled; tools are hidden until `[integrations] google_workspace = true`. DuDuClaw does not ship shared OAuth credentials.

## What you get

Thirty-eight agent-facing MCP tools, gated by two scopes (`google:read` /
`google:write`):

| Tool | Class | What it does |
|------|-------|--------------|
| `google_status` | read | Identity and credential diagnostics; output depends on the account path. Does not prove per-service access. Hidden while Google integration or the employee identity is disabled. |
| `gmail_search` | read | Search the mailbox using Gmail query syntax (`from:… is:unread`, etc.). Returns sender/subject/date/snippet. |
| `gmail_read` | read | Read one message in full: headers, plain-text body (truncated if long), attachment manifest (filename + size only — never downloaded). |
| `google_chat_spaces` | read | List the connected user's spaces, group chats and DMs; requires Chat read opt-in. |
| `google_chat_messages` | read | Read recent Chat messages; scanned reference data, attachment names only. |
| `google_chat_send` | send | Send text to an existing space as the verified requesting member. Off until `[google] chat_send = true`; the member confirms every send. See [Send Google Chat as yourself](google-personal-connections.md#send-google-chat-as-yourself). |
| `gmail_send_draft` | send | Send an approved draft snapshot; a new confirmation is required every time. |
| `calendar_delete_event` | delete | Delete an approved event version; notifications default to none. |
| `gmail_create_draft` | write | Create a Gmail **draft**. Saves without sending; use `gmail_send_draft` with a separate confirmation to send. |
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
| `docs_append` | write | Append text to the **end** of a Doc. |
| `docs_create` | write | Create a Doc in My Drive root; optional initial text. |
| `docs_replace_text` | write | Replace matching text in a Doc; case-sensitive by default. |
| `sheets_create` | write | Create a spreadsheet, optionally with named sheets. |
| `sheets_update` | write | Update up to 10,000 cells; RAW by default. |
| `calendar_update_event` | write | Edit an event; always uses send approval rules because attendee calendars may change. |
| `slides_read` | read | Read a presentation's text slide by slide (shapes, grouped shapes, table cells). |

These five write tools and Gmail reply fields (`thread_id`, `in_reply_to`, `references`) require native REST. Apps Script Bridge hides unsupported tools and reply fields. Changing the backend triggers `notifications/tools/list_changed`. Gmail `to` and `cc` accept comma-separated email addresses (up to 1,000 characters per field); each mailbox is validated and control characters are refused.

Write results report only what Google returned. `calendar_create_event` lists how many attendees were added (or "Attendees: none") and never claims anyone was notified; a missing event ID is an error. `gmail_create_draft` with no draft ID back is an error. `sheets_append` shows row and cell counts only when Google reports them. On the Apps Script path, a row is always appended to the **first sheet** of the spreadsheet: the result says so and that the requested range was ignored. An Apps Script reply counts as success only in that action's success shape; an empty reply, `ok: false` or any `error` field is a tool error with the reason.

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

The table above lists the existing tools. The nine new cloud-drive write tools, their parameters, action switches and effect classes are listed in the [Drive write reference](google-mcp.md#cloud-drive-write-tools). They require native REST and `google:write`; the six existing Docs/Sheets write tools keep their existing gates.

### Safety design

- **Sending requires confirmation.** `gmail_create_draft` saves without sending. `gmail_send_draft` sends only after a new card is confirmed for that call.
- **Read stays read.** The read-class tools cannot modify anything in Gmail or
  Calendar.
- **Forms and Slides are read-only.** Drive writing uses nine separately gated tools; see [write tools and folder rules](google-mcp.md#cloud-drive-write-tools). All four action switches default off.
- **Separate write permission.** Ordinary Drive reads use `drive.readonly`. The separate write connection requests `drive`, `documents` and `spreadsheets`; its credential stays in the gateway. Recording import separately uses `drive.file`. Slides uses `presentations.readonly`.
- **Optional approval gate.** For extra caution, list the write tools under an
  agent's `agent.toml [capabilities] approval_required_tools` so each draft,
  event, or spreadsheet write waits for HITL approval:

  ```toml
  [capabilities]
  approval_required_tools = ["gmail_create_draft", "calendar_create_event", "sheets_append", "gtasks_create", "gtasks_complete", "docs_append", "docs_create", "docs_replace_text", "sheets_create", "sheets_update", "calendar_update_event"]
  ```

## Create and edit Google content

These native tools reuse the existing `documents`, `spreadsheets` and `calendar.events` OAuth scopes. They require `google:write`; a `google:read` employee cannot list or call them. Personal connections must also have the corresponding service scope. Domain-wide delegation uses the same native tool names. New tools and reply drafts require native REST access; Apps Script Bridge does not implement these operations.

| Native tool | Arguments and behavior |
|---|---|
| `docs_create` | Required `title`, optional `body`. Creates a Doc in My Drive root. |
| `docs_replace_text` | `document_id`, nonempty `find`, `replace`, optional `match_case` (default `true`). Replaces every match and returns `occurrencesChanged`. |
| `sheets_create` | Required `title`, optional `sheet_names` array (up to 100 names). Creates a spreadsheet in My Drive root. |
| `sheets_update` | `spreadsheet_id`, A1 `range`, 2-D `values` array (up to 10,000 cells). `value_input_option` is `RAW` by default; opt into `USER_ENTERED` for formulas, dates and typed values. |
| `calendar_update_event` | `event_id`, optional `calendar_id` (default `primary`), and at least one of `summary`, `description`, `location`, RFC3339 `start` / `end`, or `attendees` (email array; `[]` clears attendees). Every call applies the same `send` action rules and approval path as `calendar_create_event`: attendee calendars may change even with `sendUpdates=none`. This remains the default notification parameter; `notify_attendees=true` changes it to `all`. |

Text has a 20,000-character limit (titles: 1,000; sheet names: 100; A1 ranges: 1,000). Over-limit input is refused before action rules, approval or digest binding, and is never truncated. Results contain machine-readable `structuredContent`: `documentId` or `spreadsheetId` plus `url`, or the event `id` and `htmlLink`. IDs can be reused in later tool calls. Google content echoed in results is screened before return. Creating a Doc with initial text takes two requests; if insertion fails, check My Drive for the created document before retrying.

For a reply draft, call `gmail_create_draft` with its existing parameters plus `thread_id` and `in_reply_to` (the original email's RFC `Message-ID`, for example `<original@example.com>`). Keep the original subject, with a reply prefix if needed. Optional `references` holds the existing References chain including the original Message-ID; it defaults to `in_reply_to`. These inputs set Gmail `threadId`, `In-Reply-To` and `References`. The caller supplies the headers so a compose-only connection needs no additional read scope. This still saves a draft and never sends mail.

### Migrating a self-hosted Google MCP server

Use native names in skills when replacing a customer-managed server such as hcatwn-google. These are name mappings, not registered aliases; compare each server's argument schema before migrating.

| Self-hosted tool name | DuDuClaw native name |
|---|---|
| `calendar_list` | `calendar_list_events` |
| `gmail_draft` | `gmail_create_draft` |

Use the native names in the table above for new Docs, Sheets and calendar editing workflows. Other self-hosted servers may use different names; check their tool list. Google official remote MCP tools retain Google's names and are a separate integration.

## Read personal Google Chat conversations (opt-in)

`google_chat_spaces` lists spaces the connected user belongs to: resource name, display name, type and last active time when Google supplies it. `limit` defaults to 20 (1–50); `filter` accepts exactly `SPACE`, `GROUP_CHAT` or `DIRECT_MESSAGE`.

`google_chat_messages` reads recent messages from a strict `spaces/...` resource name, newest first. `limit` defaults to 30 (1–100); optional `since` must be RFC3339 and `thread` must be a thread resource within that space. Each message includes its sender, creation time, text (at most 2,000 characters), thread and attachment names. Attachments are never downloaded. Sender names use Google's returned display name or the `users/...` resource; no People API or extra membership permission is used. The complete returned content is capped at 30,000 characters.

1. An administrator enables **Google Chat read** in the dashboard under Manage → Integrations → Google (`[google] chat_read = true`, default false).
2. The user reconnects their own Google account in **My connections** and selects both Chat read permissions. The optional scopes are `https://www.googleapis.com/auth/chat.spaces.readonly` and `https://www.googleapis.com/auth/chat.messages.readonly`; existing users are not asked for them while the switch is off.
3. In the OAuth client's Google Cloud project, enable **Google Chat API** and configure a **Chat app**, even for user authentication. A Workspace administrator may also need to allow the OAuth app.

These tools use the same employee/account ownership, scope, approval and private-data egress gates as Gmail reads. They read the account selected by that existing identity policy; they do not accept a user/account argument. Apps Script does not support these Chat reads. This is separate from the Chat bot channel's DMs and mentions. Disabling the switch blocks new Chat reads, including existing tokens; it does not revoke Google's previously granted permissions.

Returned content is untrusted reference data, wrapped and scanned with the group-history policy. Any injection hit refuses the whole response with `not available`. Audit entries retain space identifiers and counts, never message text. Fixture and local tests do not establish real-account access; verify consent and reading with the customer's account before acceptance.


## Per-employee Google accounts (company delegation, v1.76.0)

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
- Credential protection in a supported protected store (Security page, see
  [Security defense](../features/05-security-defense.md#person-credentials-in-the-os-keychain-v1760)).
  The service-account key is sealed with that protection key, so it is never
  a file an AI employee can read. macOS and Windows qualify, and so does
  Docker started with the person-credentials overlay
  ([Docker guide](docker.md#74-personal-accounts-person-credentials-overlay)).
  The [native Linux protected system service](linux-protected-service.md) also qualifies.
  Ordinary Docker installs and Linux user services refuse acting as a colleague.

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
5. **Test connection** checks each scope independently, checks Gmail reports
   the same address when Gmail is authorized, then mints per-service scopes.
   Gmail, Calendar, Drive and Tasks read read-only endpoints. Sheets, Docs,
   Slides and Forms first find an accessible sample and then GET its service
   resource. With no sample, that service stays unverified; token issuance
   alone does not count as data access.
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
installation behaves exactly as before. `none` hides the 29 Google tools
from that employee and refuses calls to them. A value that cannot be read
(a typo, an unknown key) refuses Google for that employee; it never turns
into the shared account. Enterprise employees may use `owner` or `user:<id>`
with an active direct Owner and that person's explicit consent; see
[Personal Google connections (v1.76.0)](google-personal-connections.md).

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

**Google Chat senders (v1.82):** a Chat sender counts as a person here when
an Admin bound their Chat user id under **Users → Verify identity**, or — only
if an Admin turned on the optional, **off-by-default** setting
`[google] chat_identity_from_account` (Manage → Integrations → Google) — when
they connected their own Google account under **My connections** (matched on
the Google account id; this automatic match has **not yet been verified
against a real Google Workspace**; leave the setting off unless you have
confirmed in your own Workspace that the two ids match). There is no e-mail matching. Before v1.82 every Chat sender was
unbound, so only `anyone` let them through. See
[Dashboard settings → Users → Verify identity](dashboard-settings.md#users--verify-identity-channel-identities-v182).

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
- **External Google MCP servers** (`bearer_token = "oauth://google"`) use
  the employee's selected account on HTTP mounts. Permission failures skip
  that Google mount; other configured servers remain available. See below.
- **Audit:** every token the gateway issues or refuses is a `google_token`
  row in `tool_calls.jsonl` (employee, identity, account, requester kind,
  tool, refusal code); every Google tool call on a colleague's identity is a
  `google_call` row. Neither ever contains a token. Changing an employee's
  identity writes `agent.google_identity_set` to the account audit log and
  `agent_google_identity_set` to `security_audit.jsonl`.

### Dedicated employee API keys and Google MCP mounts (v1.76.0)

An authenticated employee-specific API key with explicit `google:read` or
`google:write` may use the corresponding tools in the closed 19-tool Google
surface. `admin` alone is not a substitute. The key must belong to a real
employee with readable configuration. General external key grant options and
other connectors are unchanged; OAuth authentication does not create this
execution receipt. A legitimate internal custom integration key without an
employee keeps its existing Home-account path.

These calls still pass the employee's tool allow/deny lists, action rules,
PolicyKernel, scoped grants and required approvals. The accepted employee,
tool and final arguments stay fixed through an approval wait. Expired keys,
removed employees and changed authority are refused, never replaced by a
process's ambient identity or another account. The gateway executes the
Google request and returns bounded tool results without returning a Google
bearer to the external MCP client.

A key identifies the employee, not a human requester. For a delegated person
account it is an **Unknown** requester: `bound_people` and `owner_only` refuse
it, even with Home fallback enabled. Only an operator's explicit `anyone`
setting permits this use; it does not authorize public website visitors.

For an employee's HTTP MCP mount, `bearer_token = "oauth://google"` resolves
that employee's account in the gateway. Each actual call rechecks the live
requester, selector, domain, colleague status and credential protection,
including when the connection is reused. A person-account stdio mount is
skipped because stdio does not consume the HTTP bearer. An actual configured
Home fallback adds a separate notice to that call's result; a successful
person-account call does not claim to have used Home. Provider JSON and
redaction/CCR processing retain their original structure.

Agent Mail also records the last completed poll time, unavailable-account
count and transport-error count. An unavailable or unreadable account is
shown separately from an empty mailbox; no completed poll is not reported as
successful zero-mail access. The per-installation mailbox and reader gates
remain unchanged.

Dedicated gateway execution makes one attempt for non-GET/HEAD REST requests: transport errors, 429 and 5xx do not automatically repeat a write. GET/HEAD retain one retry. This restriction applies to the dedicated execute route; existing Home callers keep their previous helper behavior.

### Limits

- `gmail.compose` lets whoever holds the access token send mail. The draft-creation tool saves drafts and `gmail_send_draft` requires per-call confirmation, but an employee with an unrestricted shell can call
  Google directly with a token it was issued for its own colleague.
- An employee with a shell runs as the same OS user as the gateway; see the
  keychain limits in [Security defense](../features/05-security-defense.md#person-credentials-in-the-os-keychain-v1760).
- Google's own remote MCP endpoints may not accept delegated tokens.


## What was ruled out, and why

**Reading Mail.app / Calendar.app locally via AppleScript.** Calendar works and
ships today (`os_calendar_today`), because a day of events is a small query.
Mail does not: measured against a real 54,000-message mailbox on macOS 15,
`Mail.inbox.messages.dateReceived()` took 17 seconds, and both
`whose({dateReceived: …})` and `whose({readStatus: false})` failed to return
within 60–90 seconds. Message order from `inbox.messages` is also not
chronological, so cheap index access returns arbitrary old mail. Spotlight does
not index mail messages (`kMDItemKind == 'Mail Message'` returns nothing), and
reading `~/Library/Mail` directly needs Full Disk Access plus an undocumented,
version-specific SQLite schema. Gmail on this path goes through Apps Script
instead.


## Company authorization and identity choices (v1.85)

Delegation requests only the scopes for the tool's service: Gmail, Calendar, Docs, Sheets, Drive, Slides, Forms or Tasks. Tokens are cached separately by service account, subject and normalized scope set. A missing grant for another service does not block an already granted service. The connection test lists each scope separately as granted or not confirmed, then reports service access separately; a token does not prove that a data API or sample file is available.

With `[google] chat_read = true`, delegation also supports `chat.spaces.readonly` and `chat.messages.readonly` (both under `https://www.googleapis.com/auth/`). Authorize those scopes in the Google Admin console and enable Google Chat API. The configuration default remains off. The setup wizard preselects Chat reading; it writes the choice only when an administrator saves it. Connected members missing either Chat scope or its local purpose see **Reconnect to add Google Chat** in **My connections**. Sending Chat messages as a person requires the separate `chat_send` opt-in and the member's confirmation for every send; see [Send Google Chat as yourself](google-personal-connections.md#send-google-chat-as-yourself).

To follow an AI employee's responsible member:

1. Bind the member as an Owner; if there are several Owners, select the primary owner in **Employee → Connections**.
2. Select **Company authorization: follow responsible member** (`identity = "delegate:owner"`). Configure allowed subject domains and import the sealed delegation key as above.

   After a permission change, the call already in progress may still complete as the previous responsible member; all subsequent calls are checked again.
3. In **Integrations → Google → Company authorization**, review the employee, current owner, login email and attestation status. Confirm each listed member's address using the attest action. This works outside protected mode too; it requires an active Admin and the existing password + authenticator step-up with a session-bound CSRF token. If the member set the address themselves, explicitly acknowledge that you independently verified it.
4. Test the connection. Every call derives the subject from the current primary-owner binding and login account, never a tool argument. Missing attestation, changed ownership (including changing back), changed email, inactive owner or an unreadable account record refuses the call and requires administrator attention. `requesters` defaults to `owner_only`, with the existing requester rules for scheduled work; `fallback = "home"` is forbidden for this selector. Protected mode retains its stricter tenant-route and requester rules.

The attestation uses `google.delegation.attest_subject {agent_id, subject, user_id, acknowledge_self_set?, csrf_token}`. The server checks the displayed user id and email against the current owner and records the administrator, subject user and email in the audit logs. This company delegation does not create personal OAuth consent.

For `owner` and `user:<id>`, the Admin **Test connection** button (`google.identity.test {agent_id}`) checks the connection, self-consent records, scope/purpose coverage, OAuth refresh and tokeninfo scope comparison. It does not call Gmail, Drive, Calendar or any other data endpoint. The test is audited with the actor and tested member, and the member receives a notice through their verified notification destinations. It does not supply consent, read data, or authorize AI use by itself.

Use the following for a separately managed external Google tool:

```toml
[google]
identity = "external"
external_server = "Partner Google MCP" # optional, at most 120 characters
```

The connection tab displays **Provided by an external tool**. Native Google tools are hidden and refused, just as with `none`; this selector does not configure or authorize the external server. The name is display text only, with control characters rejected. Older parsers reject the unknown selector (and optional key), so they refuse native Google access rather than using the company account.

### Confirmed Gmail send and Calendar delete

`gmail_send_draft {"draft_id":"r…"}` and `calendar_delete_event {"event_id":"…","calendar_id":"primary","notify_attendees":false}` require `google:write` and gateway native REST execution. Apps Script hides and refuses both. Legacy bearer-only Home execution hides and refuses them; use a dedicated employee Google connection or an account-bound gateway connector.

Every call creates a new confirmation card. `allow`, Admin scope and persistent workflow grants cannot waive it; `block` still refuses. The Gmail card comes from Google: sending account, To/Cc/Bcc, subject, body preview and attachment names/sizes. Approval binds that account, employee incarnation, draft ID, operation and MIME hash. The gateway rechecks the draft and sends the approved MIME snapshot; changed content or account requires a fresh confirmation. Recipient policy includes all To/Cc/Bcc addresses, including display-name addresses; ambiguous recipient syntax is refused.

The Calendar card shows the title, start/end with timezone, attendees, calendar, whole-series deletion flag and notification choice. It binds the event version and uses `If-Match`; a changed event requires a new confirmation. `calendar_id` defaults to `primary`; `sendUpdates=none` is explicit unless `notify_attendees=true` requests `all`. Empty HTTP 204 is success. Calendar deletion also reads the primary calendar account and selected calendar name/timezone using the existing `calendar.calendarlist.readonly` scope (or a broader calendar scope).

The MIME remains in gateway memory; the approval stores the curated preview and content hash. A denied, expired, consumed or mismatched approval cannot mutate Google. A timeout or uncertain write reports `result_unconfirmed`: inspect Google before retrying; the operation never automatically resends. Confirmation expires after 90 seconds.

The transport contract follows [Gmail drafts](https://developers.google.com/workspace/gmail/api/guides/drafts) and [Calendar conditional modifications](https://developers.google.com/workspace/calendar/api/guides/version-resources).
