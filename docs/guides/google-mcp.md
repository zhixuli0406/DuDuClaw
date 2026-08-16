# Google's official remote MCP mount (advanced option, not the shipped path)

> **Read this first**: DuDuClaw's supported path for Google Workspace is **native
> tools** — Gmail / Calendar / Sheets / Drive / Docs / Slides / Forms / Tasks, all
> eight services, all through the GA REST API, 19 MCP tools. See
> [google-workspace.md](google-workspace.md). **You don't need to read this page
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

## Coverage across the eight services

| Service | Native tools (**the shipped path**, no enrollment needed) | Official MCP (preview, internal use only) |
|---|---|---|
| Gmail | ✅ 4 tools | ✅ `preset = "google:gmail"` (13 tools) |
| Calendar | ✅ 2 | ✅ `google:calendar` (9) |
| Sheets | ✅ 2 | ✅ `google:sheets` (7) |
| Drive | ✅ 2 | ✅ `google:drive` (8) |
| Docs | ✅ 2 | ✅ `google:docs` (2) |
| Slides | ✅ 1 (read-only) | ✅ `google:slides` (2) |
| Forms | ✅ 2 | ❌ none official |
| Tasks | ✅ 4 | ❌ none official |

The official MCP's tool surface is richer than the native tools for Gmail/Drive/Sheets
(label management, permission queries, formula writes, and more) — that's its only
advantage; the cost is the preview enrollment and the ban on shipping. The native tool
count is 19 total (including `google_status`).

The "none official" verdict for Forms and Tasks is a verified fact, not a guess:
`formsmcp.googleapis.com` / `tasksmcp.googleapis.com` both return 404 in practice, and
Google's MCP documentation doesn't mention either service (not even as "coming soon").
Separately, Google also offers Chat (`preset = "google:chat"`, usable) and People
(different endpoint naming, not included in a preset) — neither currently has a native
tool equivalent.

### Per-service tool list (from a live `tools/list` call)

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
- OAuth is interactive-only — **no service account / headless authorization path**.
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
