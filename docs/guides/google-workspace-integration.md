# Google Workspace integration setup guide

> **Decision (D5, 2026-08-04):** DuDuClaw does **not** ship its own Google
> OAuth client id / secret. All three connection paths require you (the
> operator) or your customer to request credentials directly from Google;
> DuDuClaw's job is only to store the resulting credential encrypted on the
> local machine and keep it refreshed automatically. If customers broadly
> report that "requesting credentials myself is too much hassle," a shared
> authorization option provided by DuDuClaw might be considered later — that
> point has not been reached yet.

This guide answers one question: **to let your AI employees use Gmail,
Calendar, Sheets, and the rest of Google Workspace, which connection path
should you pick, and what does each step require?** The full tool list,
security design, troubleshooting, and known limitations live in the three
deep-dive documents below; this page only covers choosing a path and
following it:

- [google-workspace.md](google-workspace.md) — the complete list of 19
  native MCP tools, the security design (drafts are never sent, appends
  never overwrite), and re-auth troubleshooting.
- [google-no-oauth-client.md](google-no-oauth-client.md) — the security
  properties of service account delegation and the Apps Script bridge,
  failure cases, and measured results for the approaches that were ruled
  out.
- [google-mcp.md](google-mcp.md) — mounting Google's official remote MCP
  server (an advanced option for personal use only; it cannot be shipped to
  customers).

## Choosing among the three paths

| Path | Who it's for | Tool coverage | Who has to act |
|---|---|---|---|
| 1. Your own OAuth client | A personal `@gmail.com` account, or a Workspace user who'd rather not go through IT | All 19 tools | You register it yourself in Google Cloud Console |
| 2. Service account domain-wide delegation (DWD) | Enterprise Workspace customers who don't want every account clicking through a consent screen | All 19 tools | The customer's domain super admin authorizes it once |
| 3. Apps Script bridge | A personal `@gmail.com` account that wants nothing to do with Cloud Console | Gmail / Calendar / Sheets only (3 of the 19 tools, not the full set) | The user deploys a script themselves |

All three paths authorize **the same set of tools**. The dashboard treats
them as mutually exclusive rather than stackable: saving one clears whatever
credential was stored for another, so "the path you think is active" and
"the path that's actually active" can never disagree.

## Scopes (11 total)

> **A gap versus the planning notes:** the planning document says "19
> scopes," but that conflates "MCP tool count" with "OAuth scope count."
> Checking the code (`crates/duduclaw-gateway/src/google_workspace.rs:98-116`,
> the `REQUIRED_SCOPES` constant) shows the actual number of requested scopes
> **is 11, not 19**; 19 is the number of MCP tools this credential unlocks.
> The UI copy (the `google.cred.intro` i18n key and others) and the existing
> docs consistently say "19 tools," so this document corrects the scope
> count to 11 based on the code.

| # | Scope | What it's for (one line) |
|---|---|---|
| 1 | `gmail.readonly` | Lets the AI search and read Gmail message content (read-only — it cannot delete or modify anything). |
| 2 | `gmail.compose` | Lets the AI create Gmail drafts (draft-only, no send permission). |
| 3 | `calendar.events` | Lets the AI read and create events on your primary calendar (including Google Meet links). |
| 4 | `spreadsheets` | Lets the AI read data from a Google Sheet and append a row at the end. |
| 5 | `drive.readonly` | Lets the AI search and read (export) Drive file content — read-only, it never creates or modifies files. |
| 6 | `documents` | Lets the AI read a Google Doc's full text and append text at the **end** (it cannot rewrite or delete existing content). |
| 7 | `presentations.readonly` | Lets the AI read a Google Slides deck's text slide by slide — read-only, with no matching write tool. |
| 8 | `forms.body.readonly` | Lets the AI read a Google Form's question structure (titles, question types, choices). |
| 9 | `forms.responses.readonly` | Lets the AI read the responses a Google Form has already collected. |
| 10 | `tasks` | Lets the AI read and create Google Tasks items, and mark them complete. |
| 11 | `userinfo.email` | Lets the system identify which Google account is currently connected, for connection-status display and diagnostics — it never touches message content. |

## Where the settings page is, and what each button does

**Location:** Manage → Integrations → Google (`/manage/integrations?tab=google`).

This tab has been visible by default since v1.49.0 (earlier versions hid it
while waiting on Google's OAuth app verification; it was later confirmed
that verification only blocks the "your own OAuth client" path — service
account delegation and the Apps Script bridge are unaffected — so there was
no longer a reason to keep it hidden). But **the tab being visible doesn't
mean the tools are active**: there's a separate master switch on the
backend, `config.toml [integrations] google_workspace`, defaulting to
`false`. With the switch off, credentials can still be configured and "Test
connection" can still show green, but the tools simply won't appear in
front of your AI employees. The page shows a yellow notice explaining this
clearly; it is not a silent failure. To turn it on:

```toml
[integrations]
google_workspace = true
```

The page has two sections:

1. **The connection panel at the top** (used only for path 1, your own
   OAuth client): enter the Client ID / Client secret → click **Connect
   Google** → a Google consent window opens → once complete, the panel
   switches to **Google is connected** and lists the granted scopes. Once
   connected, you can click **Edit credentials** to swap in a different
   client, or **Disconnect** — disconnecting only revokes the access token;
   the client configuration stays in place, so reconnecting is one click.
2. **The credential-path card below** (three tabs: OAuth connect / Service
   account / Apps Script bridge): the tabs only choose which method you're
   **editing**; the path that's actually **in effect** is shown by the **In
   effect** badge on the card. **Save** only writes the configuration — it
   does not validate it. **Test connection** actually calls the Google API
   once and returns the currently effective account or a specific error
   message. So save first, then test the connection — the two are not the
   same action.

## Path 1: your own OAuth client (personal accounts, or Workspace users who'd rather not involve an admin)

Who it's for: a personal `@gmail.com` account, or a user on a Workspace
domain who doesn't want to go through IT for the service-account flow. All
19 tools work.

1. Open [Google Cloud Console → Credentials](https://console.cloud.google.com/apis/credentials)
   (create or select a project first).
2. Under "APIs & Services → Library," enable these eight APIs: Gmail,
   Calendar, Sheets, Drive, Docs, Slides, Forms, Tasks. Any tool backed by
   an API you haven't enabled returns `403`. Or enable them all in one
   command:

   ```bash
   gcloud services enable gmail.googleapis.com calendar-json.googleapis.com \
     sheets.googleapis.com drive.googleapis.com docs.googleapis.com \
     slides.googleapis.com forms.googleapis.com tasks.googleapis.com \
     --project=PROJECT_ID
   ```
3. Under "APIs & Services → OAuth consent screen," configure the consent
   screen (either External or Internal works). If the app is stuck in
   "Testing" status, add your own Google account to the "Test users" list,
   or the authorization gets blocked outright. Refresh tokens issued while
   in Testing status also have only a **7-day lifetime**, so you'll need to
   reconnect once one expires.
4. Under "Credentials → Create credentials → OAuth client ID," choose type
   **Web application**.
5. Add this to "Authorized redirect URIs":

   ```
   http://localhost:18789/api/mcp/oauth/callback
   ```

   `18789` is the gateway's default port; if you changed it with
   `DUDUCLAW_PORT`, this dashboard page shows the correct, updated URI —
   trust what's on screen. A port mismatch here fails **silently**: Google
   redirects the browser to a port nothing is listening on, the screen
   stays stuck on "Not connected," and no clear error appears.
6. Copy the generated **Client ID** and **Client secret**.
7. Back in the dashboard, under "Integrations → Google," paste the Client
   ID / Client secret into the connection panel at the top and click
   **Connect Google**.
8. Google opens a consent window; after you approve it the window closes
   and the panel switches to **Google is connected**, listing the granted
   scopes.

## Path 2: service account domain-wide delegation (enterprise Workspace)

Who it's for: enterprise Workspace domain customers. The domain super admin
authorizes it once, and after that no individual user ever has to click
through a Google consent screen — it also **doesn't require Google app
verification**, which is its advantage over path 1. All 19 tools work. A
personal `@gmail.com` account doesn't belong to any domain, so this path
isn't available to it.

1. In your (the service provider's) Google Cloud project, create a
   **service account** and download its JSON key. Note the numeric
   **client ID** shown on the service account's detail page (the client
   id — not the same thing as the `client_email` field inside the key
   file).
2. Save the key file on the DuDuClaw host and lock down its permissions:

   ```bash
   mkdir -p ~/.duduclaw/keys && mv ~/Downloads/sa-key.json ~/.duduclaw/keys/google-sa.json
   chmod 600 ~/.duduclaw/keys/google-sa.json
   ```
3. Give the customer's domain **super admin** the client ID from the
   previous step, along with the scope list below (the credential card in
   the dashboard has a copy button for this — see the next step), and have
   them do the following, in order:

   **Admin console → Security (安全性) → Access and data control
   (存取權和資料控管) → API controls (API 控管) → Manage Domain Wide
   Delegation (管理網域範圍委派) → Add new (新增)** → paste the client ID
   → paste the scope list (comma-separated, one line) → save.

   This usually takes effect quickly, though Google's own documentation
   says it can take up to 24 hours. If the domain has "multi-party
   approval" turned on (available since August 2024), a second super admin
   needs to co-sign — remind the customer to leave enough time for that.
4. Back in the dashboard, under "Integrations → Google," switch the
   credential-path card below to the **Service account** tab:
   - Fill in "Service account key file" (relative paths resolve against
     `~/.duduclaw`, e.g. `keys/google-sa.json`).
   - Fill in "User to impersonate" (which Workspace account identity the
     API calls should act as, e.g. `boss@customer.com`).
   - There's a "Scope list for the admin" block above; click **Copy** to
     hand it straight to the admin from step 3, no retyping needed.
   - Click **Save**, then click **Test connection** — this actually
     requests a token, and **a green result is the only proof that the
     admin's authorization has really taken effect**; configuring it is
     not the same as confirming it worked.

## Path 3: Apps Script bridge (personal accounts, no Cloud Console at all)

Who it's for: a personal `@gmail.com` account (Workspace accounts can use
it too, unless an admin has disabled Apps Script) that doesn't want to
register an OAuth client or deal with IT. **Coverage is a subset**: only
Gmail (search / read / draft), Calendar (list / create), and Sheets (read /
append a row) — Drive, Docs, Slides, Forms, and Tasks are not supported on
this path; calling them returns an explicit "Apps Script bridge not
supported" error, not a silent empty result.

1. Open <https://script.google.com> and create a new project.
2. Replace the entire default file content with
   [`templates/apps-script/duduclaw-bridge.gs`](../../templates/apps-script/duduclaw-bridge.gs).
3. Generate a random secret and paste it into the script in place of
   `CHANGE_ME_TO_A_LONG_RANDOM_STRING`:

   ```bash
   openssl rand -base64 32
   ```
4. **Deploy → New deployment → Web app**:
   - Execute as: **Me**
   - Who has access: **Anyone**
5. Google shows an "unverified app" screen — this is expected (the
   unverified app is your own script). Choose "Advanced → Go to (project
   name)" and grant consent.
6. Copy the URL that ends in `/exec` (not `/dev` — that one only authorizes
   the script owner's own browser session, and DuDuClaw can't call it).
7. Back in the dashboard, under "Integrations → Google," switch the
   credential-path card below to the **Apps Script bridge** tab:
   - Paste the web app URL (ending in `/exec`).
   - Paste the secret generated in step 3 (on later edits, changing only
     the URL and leaving the secret field blank keeps the stored secret —
     it won't be cleared).
   - Click **Save**, then click **Test connection** — a green result shows
     exactly which Google account the script is actually running as, the
     fastest way to catch a "deployed under the wrong login" mistake.

## Troubleshooting

The error messages, re-auth flow, and known limitations for each of the
three paths live in their own deep-dive documents:
- OAuth client path `401` / `403` / redirect URI mismatches → [google-workspace.md#troubleshooting](google-workspace.md#troubleshooting)
- Service account `unauthorized_client` (usually a missing entry in the
  scope list the admin pasted), Apps Script URL/secret validation rules →
  [google-no-oauth-client.md](google-no-oauth-client.md)

The `google_status` MCP tool is available at any time and reports the
currently active credential source and granted scopes — it's the fastest
first diagnostic step.
