# Google Workspace tools and security

For setup, use [Connect Google](google-setup.md). Native REST tools cover eight services and nineteen tools. The Apps Script path covers three services and eight actions including status. The dashboard remains visible with the integration disabled; tools are hidden until `[integrations] google_workspace = true`. DuDuClaw does not ship shared OAuth credentials.

## What you get

Nineteen agent-facing MCP tools, gated by two scopes (`google:read` /
`google:write`):

| Tool | Class | What it does |
|------|-------|--------------|
| `google_status` | read | Identity and credential diagnostics; output depends on the account path. Does not prove per-service access. Hidden while Google integration or the employee identity is disabled. |
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
- Credential protection in the system keychain (Security page, see
  [Security defense](../features/05-security-defense.md#person-credentials-in-the-os-keychain-v1760)).
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
installation behaves exactly as before. `none` hides the 19 Google tools
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

- `gmail.compose` lets whoever holds the access token send mail. The tools
  only create drafts, but an employee with an unrestricted shell can call
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
