# Google's official remote MCP mount (advanced option, not the shipped path)

> **Read this first**: DuDuClaw's supported path for Google Workspace is **native
> tools** — Gmail / Calendar / Sheets / Drive / Docs / Slides / Forms / Tasks / Chat,
> through the REST API, with 38 native MCP tools. See
> [google-setup.md](google-setup.md). **You don't need to read this page
> for normal use.**
>
> The official remote MCP documented here is an **advanced option** with two hard
> constraints: ① it's still a Developer Preview and requires enrollment; ② the
> Program Terms prohibit letting end users **outside your own domain/organization**
> use a Pre-GA API through your app before GA — in other words, **you can't ship
> it to customers** (unless each customer enrolls with their own eligibility and
> GCP project). Given that, the 2026-07-30 decision was: the product ships
> entirely on native tools, and this mount is kept only for internal use and for
> advanced users willing to enroll themselves.

## Client requirement from 2026-11-11

From 2026-11-11, Google's Workspace MCP servers reject clients that are not marked for an AI-powered agent. In Google Auth platform → Clients, create a separate client for this advanced MCP use and select “Use this client for an AI-powered agent”. REST-only applications are unaffected. DuDuClaw's native Google tools use REST. The requirement and Console paths use research checked 2026-10-30; Console screens were not re-verified live here.

## Service coverage

| Service | Native tools (**the shipped path**, no enrollment needed) | Official MCP (preview, internal use only) |
|---|---|---|
| Gmail | ✅ 4 | ✅ `preset = "google:gmail"` (13 tools) |
| Calendar | ✅ 4 | ✅ `google:calendar` (9) |
| Sheets | ✅ 6 | ✅ `google:sheets` (7) |
| Drive | ✅ 7 | ✅ `google:drive` (8) |
| Docs | ✅ 6 | ✅ `google:docs` (2) |
| Slides | ✅ 1 (read-only) | ✅ `google:slides` (2) |
| Forms | ✅ 2 | ❌ none official |
| Tasks | ✅ 4 | ❌ none official |

Native tools now include creation and editing. The remote tools retain Google's own schemas and eligibility requirements; they do not inherit the native write switches or folder checks. Use the native write tools below for controlled Drive writes.

The "none official" verdict for Forms and Tasks is a verified fact, not a guess:
`formsmcp.googleapis.com` / `tasksmcp.googleapis.com` both return 404 in practice, and
Google's MCP documentation doesn't mention either service (not even as "coming soon").
Separately, Google also offers Chat (`preset = "google:chat"`) and People
(different endpoint naming, not included in a preset) — People has no native equivalent here; Chat has native read and confirmed-send tools.

### Per-service tool list (historical 2026-07-30 `tools/list`, not re-verified)

- **Gmail**: `search_threads` `get_thread` `get_message` `create_draft`
  `list_drafts` `list_labels` `create_label` `label_message` `unlabel_message`
  `label_thread` `unlabel_thread` `apply_sensitive_message_label`
  `apply_sensitive_thread_label`. **No send-mail tool** — it stops at drafts.
- **Calendar**: `list_calendars` `list_events` `get_event` `search_events`
  `suggest_time` `create_event` `update_event` `delete_event`
  `respond_to_event`
- **Drive**: `search_files` `list_recent_files` `get_file_metadata`
  `get_file_permissions` `read_file_content` `download_file_content`
  `create_file` `copy_file`
- **Docs**: `read_doc` `update_doc`
- **Sheets**: `get_spreadsheet` `get_values` `update_values` `update_formulas`
  `update_spreadsheet` `insert_dimension` `copy_sheet_to_another_spreadsheet`
- **Slides**: `read_presentation` `update_presentation`

## One-time setup

1. **Join the Developer Preview Program**: <https://developers.google.com/workspace/preview>
   (free, approval takes a few days; applying requires a Workspace account). These
   servers are **still preview, not GA** — the terms restrict Pre-GA API use to your
   own domain/organization only.
2. **Enable the APIs on the GCP project** — each service needs both its "standard API"
   and its "MCP API" layer:

   ```bash
   gcloud services enable \
     gmail.googleapis.com gmailmcp.googleapis.com \
     calendar-json.googleapis.com calendarmcp.googleapis.com \
     drive.googleapis.com drivemcp.googleapis.com \
     docs.googleapis.com docsmcp.googleapis.com \
     sheets.googleapis.com sheetsmcp.googleapis.com \
     slides.googleapis.com slidesmcp.googleapis.com \
     --project=PROJECT_ID
   ```

3. **Connect a Google account**: the dashboard's Integrations → Google (整合 → Google)
   tab. The scopes already cover Drive/Docs/Sheets/Slides (see
   [google-workspace.md](google-workspace.md)); accounts connected before v1.47 need to
   reconnect once to pick up the new scopes.

## How authentication works

The `preset` sets the bearer to `oauth://google` — at mount time it fetches the current
valid access token, and refreshes it automatically with the refresh token when it
expires. When no Google account is connected, **the whole server is skipped**
(fail-safe: the agent loses those tools, but the reply doesn't fail).

You can also bypass the dashboard integration and bring your own token:

```toml
[[mcp.external]]
preset = "google:sheets"
bearer_token = "env://MY_GOOGLE_TOKEN"   # overrides the preset's default bearer
```

Technical detail: these are stateless Streamable HTTP servers, natively supported by
DuDuClaw's MCP client. There's **no need** for a stdio proxy like `npx mcp-remote` —
Google doesn't provide an official bridge, and its OAuth doesn't support Dynamic
Client Registration, so community proxies' default flow fails.

## Recommendation: narrow the tool surface

The official server hands over a lot of tools at once (8 for Drive, 13 for Gmail).
It's worth using `allowed_tools` to open only what you need, and routing write
tools through HITL approval:

```toml
[[mcp.external]]
preset = "google:calendar"
allowed_tools = ["list_events", "suggest_time", "create_event"]

# agent.toml
[capabilities]
approval_required_tools = ["create_event", "update_doc", "create_file"]
```

## Known limitations

- Still preview stage; rate limits aren't publicly documented (check the GCP
  Console quota page).
- The normal OAuth flow needs browser consent. A remote or headless host can use the SSH tunnel in [Connect Google](google-setup.md#remote-host-or-docker). Native service-account delegation is supported; acceptance of delegated tokens by each official remote MCP service needs separate verification.
- Gmail has no send-mail tool; the official reference page's tool count doesn't
  match the live endpoint (the reference lists 10, the endpoint actually has 13)
  — trust a live `tools/list` call over the docs.
- If the endpoints get renamed at GA, the only change needed is in
  `mcp_external.rs`'s `GOOGLE_MCP_PRESETS` — users' `agent.toml` files don't
  need to change.

Sources:
[configure-mcp-servers](https://developers.google.com/workspace/guides/configure-mcp-servers),
[Gmail MCP reference](https://developers.google.com/workspace/gmail/api/reference/mcp),
[Calendar MCP reference](https://developers.google.com/workspace/calendar/api/v3/reference/mcp),
[Developer Preview Program](https://developers.google.com/workspace/preview).

The `google:chat` preset does not add Chat authorization scopes. Native Chat read and send scopes are separate opt-ins; a mount still needs suitable authorization. The Google Chat channel uses `chat.bot` and is a separate connection.

## Chat authorization links

Missing personal access may produce a gateway-owned confirmation link. It requires the same member’s normal Dashboard login and explicit data-disclosure confirmation; it is not a login or SSO credential. No MCP tool accepts a ticket or selects the member, account, scopes or redirect. Authorization does not approve outbound sends. See [personal Google connections](google-personal-connections.md#confirm-google-access-from-a-conversation) for account checks, public URL requirements and continuation behavior. Official remote MCP service eligibility and existing capability gates still apply.

## Send Google Chat as yourself

`google_chat_send {space, text, thread?}` sends text to an existing space using the verified requesting member's own Google account. It requires `google:write` and is off until an administrator enables `[google] chat_send = true`. A home/shared account, another member, Apps Script, or a Chat app token (`chat.bot`) cannot substitute for the member.

For personal OAuth, connect your account first, then open **My connections → Google Chat sending** and grant the separate `chat_send` purpose. Additional-service consent explicitly requests the already-consented ordinary services plus `https://www.googleapis.com/auth/chat.messages.create`, without automatic merging of prior Google grants; the existing Google subject must remain unchanged and Google must actually grant the scope. Domain-wide delegation requests a separate send scope set for this tool; a Workspace administrator must authorize it. It is never added to every Google request.

Every send waits for the member's own single-use confirmation, valid for five minutes. The card shows the account, space resource name, thread and full final text, with a private-data warning. A verified private conversation can carry a short card; group requests and long messages use the member-only **My connections** page. Admin approval, an `allow` rule, Admin MCP scope and recipient allowlists cannot replace this confirmation. Any organisation-required egress approval still applies in addition. Editing the message, destination, account, credential or policy invalidates confirmation.

The gateway keeps one request ID with the operation and never automatically resends an unknown outcome (`result_unconfirmed`). Check Google Chat before asking for a new send. Attachments and space creation are not supported; a specified thread must belong to the same space. Google displays the user as sender and also attributes the application by name. [Google Chat API reference](https://developers.google.com/workspace/chat/api/reference/rest/v1/spaces.messages/create).

## Cloud-drive write tools

These nine native tools require `google:write`. Administrators control create, modify, move and trash separately; all four start off. Enabling an action or widening its policy requires an explicit acknowledgement of the risks and records the administrator and time. No additional sign-in is required for that settings change. The dashboard controls arrive with the settings page in this release; the Google integration and personal write-connection blocks are not available yet.

Each action defaults to confirmation by the member. An administrator can turn confirmation off per action, or require both member and administrator approval. Required member confirmation lasts five minutes and only that member can decide. Tool allowlists, Admin tokens and standing workflow approval cannot replace it. Background or unattended work is refused when confirmation is required. With personal connections or delegation to the owner, the requester must be the account holder; shared company and fixed-delegation connections use the verified requesting member.

| Tool | Parameters and result | Action switch | Effect class |
|---|---|---|---|
| `drive_create_file` | Required `name`; optional `parent_folder_id`. Choose exactly one of `text` (up to 100,000 characters), `attachment` (this conversation's attachment filename), or `folder=true`. Optional `convert_to`: `document` or `spreadsheet`; `mime_type`: `text/plain`, `text/csv` or `text/markdown`. Creates a file or folder. | Create | Publish |
| `drive_copy_file` | Required `file_id`, `parent_folder_id`; optional `name`. Copies a readable source file into the destination folder. | Create | Publish |
| `drive_update_file` | Required `file_id`; at least one of `name`, `description`, `text`. Renames, changes the description or replaces text (up to 100,000 characters, non-Google-format text files only). | Modify | Modify |
| `drive_move_file` | Required `file_id`, `destination_folder_id`. Moves within the same drive; current parents are read from Google. | Move | Publish |
| `drive_trash_file` | Required `file_id`. Moves the item to trash only. | Trash | Delete |
| `docs_insert_text` | Required `document_id`, `text` (1–20,000 characters). Choose `at="start"`, `at="end"` or `after_text` (a unique anchor, 1–1,000 characters). Optional `tab_id`. | Modify | Modify |
| `docs_format` | Required `document_id`, `operations` (1–50). Supports paragraph/text styles, table-cell background, a header and adding a document tab; details below. | Modify | Modify |
| `sheets_add_sheet` | Required `spreadsheet_id`, `title`; optional `copy_from_sheet` (an existing sheet name). Adds a blank sheet or duplicates a sheet in that spreadsheet. | Modify | Modify |
| `sheets_add_note` | Required `spreadsheet_id`, `range` (A1, up to 1,000 cells), `note` (up to 5,000 characters; empty clears notes). Sets cell notes, not threaded comments. | Modify | Modify |

IDs can also be supplied as supported Google file or folder URLs. Omitting `parent_folder_id` for creation means the My Drive root and is allowed only with an empty folder list. Uploads accept only attachments attached to the current conversation request, up to 5 MB (5 MiB); arbitrary local paths and employee workspace files are refused. Conversion to a Google document or spreadsheet is optional. Permanent deletion, emptying trash and sharing or permission changes are unavailable.

For `docs_format`, each operation has a `type` and only its supported fields:

- `paragraph_style`: `text` as a unique anchor, `named_style` (`NORMAL_TEXT`, `TITLE`, `SUBTITLE`, `HEADING_1`–`HEADING_6`), optional `alignment` (`START`, `CENTER`, `END`, `JUSTIFIED`).
- `text_style`: unique `text`, and at least one of `bold`, `italic`, `underline`, `font_size_pt` (6–96).
- `table_cell_style`: zero-based `table_index` (0–99), `row` (0–499), `column` (0–49), and `background_hex` (`#RRGGBB`).
- `header`: `text` (up to 1,000 characters).
- `add_tab`: `title` (1–100 characters).

### Folder rules and limits

An administrator can list folders or entire shared drives. A file in a listed shared drive is in scope. Otherwise every parent chain must reach a listed folder within 12 parent steps. Unreadable parents, trashed targets or ancestors, exceeded check limits, and insufficient Google permissions refuse the action. A call has a shared limit of 16 metadata lookups, including rechecks.

Create and copy check the destination folder; a copied source must be readable and not trashed. Modify, move and trash check the target's location. Listed folders themselves cannot be renamed, moved or trashed. Moves require both source and destination in scope and cannot cross from My Drive to a shared drive or between shared drives. Shortcuts cannot be listed as allowed folders or used as destinations. Modify refuses shortcuts; move and trash affect the shortcut itself and check its own parents, without following the target.

With an empty folder list, create and modify can act anywhere the account can write; move and trash are unavailable. Folder checks run before and after writes, with another check after approval. They are best effort: Google has no folder allowlist and concurrent changes can occur. Detected post-write violations notify administrators; incomplete checks are recorded. Every attempt, including refusals and uncertain outcomes, is logged without file contents or tokens. An uncertain result is not automatically retried; check Google before requesting another write.

The existing `docs_append`, `docs_create`, `docs_replace_text`, `sheets_append`, `sheets_create` and `sheets_update` keep their existing gates and remain outside these four switches and folder rules. The four new Docs/Sheets tools above use Modify. The official Google Drive remote MCP never receives a token with full Drive access; these write tools execute only inside the main gateway process.

See [connection setup](google-setup.md) and [personal write consent](google-personal-connections.md) for account requirements. Real Google OAuth isolation and revocation, delegated scopes, document tabs, conversion, shared-drive roles and private-message confirmation delivery still require real-account acceptance; this reference does not certify those checks.
