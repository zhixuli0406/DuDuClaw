# Google Workspace without creating an OAuth client

The default path ([google-workspace.md](google-workspace.md)) asks each customer
to create a Google Cloud OAuth client. Two alternatives skip that step. Pick by
account type: a Workspace domain with an IT admin wants delegation, a personal
`@gmail.com` account wants the Apps Script bridge.

One path that no longer exists: **app passwords over IMAP/SMTP**. Google
disabled basic authentication for every Google account in March 2025 (Workspace
completed in May 2025), so IMAP, POP, SMTP, CalDAV and CardDAV all require OAuth
now. Any guide that tells you to generate an app password is out of date.

---

## Option A — service account with domain-wide delegation

**Requires a Google Workspace domain.** Personal `@gmail.com` accounts belong to
no domain and cannot be impersonated; use Option B for those.

The service account belongs to whoever runs DuDuClaw. The customer's super admin
authorizes its client id once, and from then on DuDuClaw mints tokens for users
in that domain with no consent screen — and, importantly, **no Google app
verification or CASA review**, which is what blocks the OAuth path from serving
customers outside the developer's own domain.

### What you are asking the customer for

Be straight with them: delegation lets the authorized client impersonate any
user in the domain, within the scopes they approve. Google's own
[best-practice guidance](https://support.google.com/a/answer/14437356) tells
admins to be sparing with third-party delegation, and orgs with multi-party
approval enabled (available since August 2024) need a second super admin to
sign off. Expect questions, and expect some admins to say no.

### Setup

1. In your Google Cloud project, create a service account and download its JSON
   key. Note the numeric **client id** shown on the service-account details page.
2. Store the key file on the DuDuClaw host and lock it down:

   ```bash
   mkdir -p ~/.duduclaw/keys && mv ~/Downloads/sa-key.json ~/.duduclaw/keys/google-sa.json
   chmod 600 ~/.duduclaw/keys/google-sa.json
   ```

3. Send the customer's super admin the client id and the scope list (below).
   They go to **Admin console → Security → Access and data control → API
   controls → Manage Domain Wide Delegation → Add new**, paste the client id,
   paste the scopes, and save. Propagation is usually quick but Google allows up
   to 24 hours.
4. Configure DuDuClaw, either from the dashboard or by hand.

   **Dashboard** (no restart needed): Manage (管理) → Integrations (整合／工具連線)
   → Google → Credential path (憑證方式) → **Service account (服務帳號委派)**.
   Fill in the key-file path and the user to impersonate, press Save (儲存),
   then Test connection (測試連線) — it mints a real token, so a green result means the
   admin's authorization has actually landed. The scope list has a copy button
   next to it.

   **By hand:**

   ```toml
   [integrations]
   google_workspace = true

   [integrations.google_service_account]
   key_file = "keys/google-sa.json"   # relative paths resolve against ~/.duduclaw
   subject  = "boss@customer.com"      # the Workspace user to act as
   ```

   A hand-edited config needs a gateway restart; the dashboard writes the same
   section and takes effect on the next tool call.

5. Run the `google_status` tool. It reports `Credential source: direct API
   token`.

Scope list to hand the admin — paste as one comma-separated line:

```
https://www.googleapis.com/auth/gmail.readonly,https://www.googleapis.com/auth/gmail.compose,https://www.googleapis.com/auth/calendar.events,https://www.googleapis.com/auth/spreadsheets,https://www.googleapis.com/auth/drive.readonly,https://www.googleapis.com/auth/documents,https://www.googleapis.com/auth/presentations.readonly,https://www.googleapis.com/auth/forms.body.readonly,https://www.googleapis.com/auth/forms.responses.readonly,https://www.googleapis.com/auth/tasks,https://www.googleapis.com/auth/userinfo.email
```

### When it goes wrong

`unauthorized_client` almost always means the scope list in Admin console does
not exactly match what DuDuClaw requests — Google compares the whole set, and a
single missing entry fails the mint. The error message includes the client id
to paste, so you can check it against what the admin entered.

A configured-but-broken service account is reported as an error rather than
falling back to the OAuth vault: you asked for this credential, so a
misconfiguration has to be visible instead of masked by whatever token happens
to be stored.

---

## Option B — Apps Script bridge

**Works for personal `@gmail.com` and Workspace alike.** The user deploys a
script inside their own account; DuDuClaw calls its URL. Google only ever sees
the user running their own script, so there is no third-party app to verify.

Coverage is a subset: **Gmail (search / read / draft), Calendar (list / create),
Sheets (read / append)**. Drive, Docs, Slides, Forms and Tasks are not available
on this path and return an explicit "not available through the Apps Script
bridge" error rather than an empty result.

### Setup

1. Open <https://script.google.com> and create a new project.
2. Replace the file contents with
   [`templates/apps-script/duduclaw-bridge.gs`](../../templates/apps-script/duduclaw-bridge.gs).
3. Generate a secret and paste it over `CHANGE_ME_TO_A_LONG_RANDOM_STRING`:

   ```bash
   openssl rand -base64 32
   ```

4. **Deploy → New deployment → Web app**, with:
   - Execute as: **Me**
   - Who has access: **Anyone**
5. Google shows an "unverified app" consent screen. That is expected — the
   unverified app is the user's own script. Choose **Advanced → Go to (project
   name)** and approve.
6. Copy the `/exec` URL (not `/dev`, which only authorizes the script owner's
   own browser session).
7. Configure DuDuClaw, either from the dashboard or by hand.

   **Dashboard** (recommended — it encrypts the secret for you): Manage (管理) →
   Integrations (整合／工具連線) → Google → Credential path (憑證方式) →
   **Apps Script bridge (Apps Script 橋接)**. Paste the `/exec`
   URL and the secret, press 儲存, then 測試連線 — a green result names the Google
   account the script runs as, which is the fastest way to catch "deployed under
   the wrong login". Editing the URL later without retyping the secret keeps the
   stored one.

   **By hand** (the secret is then stored in plain text — prefer the dashboard):

   ```toml
   [integrations]
   google_workspace = true

   [integrations.google_apps_script]
   url    = "https://script.google.com/macros/s/AKfyc.../exec"
   secret = "the string you generated in step 3"
   ```

8. Run `google_status`. It reports
   `Credential source: apps-script bridge at script.google.com`.

### Security properties

- **The URL and the secret together are a credential.** "Who has access: Anyone"
  means the endpoint is reachable without a Google login; the secret is the only
  thing keeping strangers out. Treat the pair like a password — never paste it
  into a chat, an issue, or a screenshot.
- The secret is stored encrypted at rest, the same way channel bot tokens are.
- DuDuClaw will only POST to `script.google.com` (and follow redirects to
  `script.googleusercontent.com`), over https, to a path ending in `/exec`.
  A mistyped or tampered `url` is rejected before the secret is sent — including
  look-alike hosts such as `script.google.com.evil.test`.
- Rotate by changing `SECRET` in the script, redeploying, and updating
  `config.toml`. The old secret stops working immediately.
- The bridge has no send-mail action, matching the native tools: an agent can
  prepare a draft, a human presses send.

### Quotas

Apps Script enforces daily per-account limits, tightest on consumer accounts.
This path suits interactive assistant use, not bulk synchronisation.

---

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
version-specific SQLite schema. Gmail on this path goes through Option B
instead.
