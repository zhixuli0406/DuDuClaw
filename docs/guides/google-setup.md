# Connect Google

Use this guide with the setup wizard to connect your Google account.

| Your situation | Choose | Main limit |
|---|---|---|
| Company Google Workspace, with a super administrator | Service account with domain-wide delegation | Personal Gmail accounts cannot use delegation |
| Company Workspace, using your own OAuth application | OAuth, Internal | Organization members only; no seven-day Testing (test mode) expiry |
| Personal @gmail.com, needing mail, calendar and spreadsheets | Apps Script bridge | Three services, eight actions including status |
| Personal @gmail.com, needing the full tool set | OAuth, External + Testing | Reauthorize every seven days while Testing |

## Start in DuDuClaw

1. Sign in as an administrator. Open **Manage → Integrations → Google** (`/manage/integrations?tab=google`) and select **Set up Google**. The original forms are under **Advanced settings**.
2. Choose the matching row above. OAuth paths show a data disclosure; read and confirm it. Enter secrets only in the dashboard, never in an employee conversation.
3. Finish one of the paths below. Saving service-account or Apps Script credentials, or completing OAuth authorization, enables the integration automatically. When switching to OAuth, the current connection stays active until the new authorization succeeds. Cancellation or rejection keeps the previous connection.

## OAuth

1. Open [Google Cloud Console](https://console.cloud.google.com/). Create or select a project using the project selector at the top.
2. Open **APIs & services → Library**. Search for and enable Gmail API, Google Calendar API, Google Sheets API, Google Drive API, Google Docs API, Google Slides API, Google Forms API and Tasks API in that project.
3. Open **Google Auth platform → Branding → Get Started**. Complete App Information, Audience, Contact Information and Finish. Choose Internal for organization members. For personal Gmail, choose External and keep Testing (test mode, limited to listed accounts). Do not select Publish app.
4. While an External app is Testing, add every account that will connect in **Audience → Add users**. Up to 100 test users can be listed. Their authorizations and refresh tokens expire after seven days. Check Testing in the wizard; after six days it warns that renewal is approaching.
5. Open **Data Access → Add or Remove Scopes**. Paste the complete scope list from the wizard into **Manually add scopes**, add it and save. The same list is included below.
6. Open **Clients → Create Client** and select **Web application**. Both Web and Desktop client creation offer an optional “Use this client for an AI-powered agent” checkbox. Use a Web client for this dashboard flow.
7. Copy the redirect URI displayed in DuDuClaw into **Authorized redirect URIs** exactly. Use the port shown on screen. Native installs default to 18889; the supplied Docker Compose defaults to 18789. HTTPS is required except for localhost; raw IP addresses and wildcards are not allowed. A different host, port, path or trailing slash can fail authorization.
8. Save the client. Copy the client ID and download the client secret immediately. Google shows the secret only once and later displays its last four characters. Keep it in a password manager.
9. Return to the wizard, enter the client ID and secret, confirm the data disclosure and select **Connect Google**. If Google shows “Google hasn't verified this app”, confirm it is your own project, select Advanced, then Go to (project name). Tick every permission on the consent screen and continue. Wait for the dashboard result.

### Remote host or Docker

localhost refers to the computer running your browser. A local Docker install needs no SSH tunnel. Only forward a port when DuDuClaw runs on a remote machine. This example assumes the screen shows 18889:

```sh
ssh -N -L 18889:127.0.0.1:18889 USER@DUDUCLAW_HOST
```

Keep SSH connected until authorization finishes on a remote install. If the screen shows 18789 or another port, replace both 18889 values in the command and register the exact displayed URI. The supplied Docker Compose publishes 18789 on all network interfaces, without a loopback-only restriction.

### Scope and lifetime limits

Among the scopes DuDuClaw requests, `gmail.readonly`, `gmail.compose` and `drive.readonly` are restricted. The complete list is below. See Google’s [Gmail classification](https://developers.google.com/workspace/gmail/api/auth/scopes) and [Drive classification](https://developers.google.com/workspace/drive/api/guides/api-specific-auth). Personal users should keep External + Testing and not publish the app. Offering these permissions publicly may require Google verification and a security assessment for server-side data handling. Internal is limited to organization members.

The unverified-app screen permits 100 new users over the project's lifetime; this limit cannot be reset. It is separate from the 100 listed Testing users. Refresh tokens can also expire after six months without use. A client unused for six months is automatically deleted, with a warning email 30 days beforehand. A stored refresh token does not prove the connection still works.

## Service account and domain-wide delegation

1. Ask your Workspace super administrator to approve delegation. It lets the authorized service account act as domain users within the approved scopes. Personal @gmail.com accounts are ineligible.
2. Have the person who manages the Google Cloud project enable the eight APIs listed under OAuth in that project. Open **IAM & Admin → Service Accounts**, create an account, then open it and select **Keys → Add key → Create new key → JSON** to download the key. Its numeric client ID is the `client_id` field in the JSON.
3. Give the super administrator the client ID and the dashboard's complete scope list. They open **Admin console → Security → Access and data control → API controls → Manage Domain Wide Delegation → Add new**, paste both and authorize. Google allows up to 24 hours for propagation.
4. For one shared company account, choose **Service account** in the wizard. Store the JSON on the DuDuClaw host with owner-only permissions. Enter an absolute path, such as `/Users/alex/.duduclaw/keys/google-sa.json` under the DuDuClaw home, replacing it with the actual host path. Do not use `~`; it is not expanded. Enter the Workspace user’s email and save. Use a real user account.
5. To act as a different colleague per employee, use **Security → Credential protection → Move to system keychain**, then **Integrations → Google → Advanced settings → Company delegation → Import key file**. This separate flow protects the key and needs a supported macOS or Windows keychain. Linux and Docker cannot use this protected colleague flow. Set allowed domains and choose the colleague in the employee’s Connections tab. See [account security and settings](google-workspace.md#per-employee-google-accounts-company-delegation-v1760).
6. Run service checks and inspect the selected account and each service result. Receiving authorization does not prove data was read successfully.

Company delegation settings limit which colleague domains can be used. Enterprise personal OAuth settings separately limit personal connection domains. See the [Workspace reference](google-workspace.md) for these settings.

## Apps Script bridge

1. Open [Apps Script](https://script.google.com/) and create a project under the Google account you want employees to use.
2. Replace the script with the repository’s [bridge template](../../templates/apps-script/duduclaw-bridge.gs). Use a password manager to generate a random password of 30 or more characters, or run `openssl rand -base64 32`, and replace `CHANGE_ME_TO_A_LONG_RANDOM_STRING`.
3. Select **Deploy → New deployment → Web app**. Set **Execute as: Me** and **Who has access: Anyone**. If Google shows “Google hasn't verified this app”, confirm it is your own project, select Advanced, then Go to (project name). Tick every permission on the consent screen and continue.
4. Copy the deployment's `/exec` URL. `/dev` is for the owner's browser session and will not work here.
5. In the wizard, choose Apps Script, paste the URL and secret and save. Confirm replacement if another path is active. Continue to service checks, select **Run service checks** and check the returned account.
6. Keep the URL and secret private. To rotate, change the script secret, select **Deploy → Manage deployments**, edit the existing deployment, select **New version** and deploy, then update the dashboard secret. This keeps the URL; New deployment creates a new URL. Already deployed scripts must also be redeployed this way to use the updated template’s account identification, which reports the executing Me account. Apps Script daily quotas apply, especially on personal accounts.

The bridge supports Gmail search, read and draft; Calendar list and create; Sheets read and append. Including `status`, the template dispatches eight actions across three services. Drive, Docs, Slides, Forms and Tasks are unavailable. It has no mail-send action. The URL is publicly reachable; the secret protects access. The dashboard stores the secret encrypted.

## Service checks and completion

1. Select **Run service checks** in the wizard and check the displayed account. OAuth and service-account checks inspect the Gmail account, Calendar list and Drive account information. The screen lists tested services, failures and available API activation links.
2. When those checks pass and the required Sheets and Docs scopes are granted, select **Done** to finish setup. The completion screen shows success and notes that Sheets and Docs have permission but documents have not been read. Slides, Forms and Tasks are not checked here.
3. Apps Script service checks confirm the executing account and script status without reading mail or spreadsheets. Select **Done** after they pass.
4. Confirm the intended employee can see Google tools. If they cannot, open the Google page to confirm the integration is enabled, then check that employee’s Google connection settings.

## Troubleshooting
When Google shows its own error page, the dashboard keeps waiting and receives no error reason. Close the Google tab, fix the issue below and start again from the dashboard. If already connected, open **Advanced settings**, select **Edit credentials**, then **Connect Google**.

| Result | Action |
|---|---|
| `SERVICE_DISABLED` / `accessNotConfigured` | Open the displayed Google Console link, enable that API in the client’s project and run service checks again |
| Insufficient scope | Select **Edit credentials**, then **Connect Google**, and tick every permission |
| `access_denied` | Appears when the account is missing from the test user list; add the actual account in Audience → Add users and start again |
| `admin_policy_enforced` | Ask your Workspace administrator to allow the application and requested services |
| `invalid_scope` | Compare Data Access and the delegated scope list with the dashboard list |
| `needs_reconnect` / refresh `invalid_grant` | Select **Edit credentials**, then **Connect Google**; Testing expiry, revocation or an expired token can cause this |
| `redirect_uri_mismatch` (Error 400) | Compare Google’s Authorized redirect URIs with the complete URI displayed by DuDuClaw character by character, including port, path and trailing slash |
| `org_internal` | The app is Internal but the account is outside the organization; use an account in the same organization |
| Waiting times out | Check the displayed port and remote SSH forwarding, then close the Google tab and start again |
| Other `403` reason | Investigate the displayed reason code; reauthorizing cannot enable a disabled API |
| `unauthorized_client` on delegation | Check numeric client ID, all scopes and administrator authorization |

Use one of the connection methods in this guide; no IMAP/SMTP setup is needed.

## Requested scopes

`calendar.calendarlist.readonly` checks the calendar list. The [calendarList.list authorization table](https://developers.google.com/workspace/calendar/api/v3/reference/calendarList/list) lists accepted scopes; `calendar.events` alone is insufficient. The comma-separated list below matches the wizard’s copy button.

```text
https://www.googleapis.com/auth/gmail.readonly,https://www.googleapis.com/auth/gmail.compose,https://www.googleapis.com/auth/calendar.events,https://www.googleapis.com/auth/calendar.calendarlist.readonly,https://www.googleapis.com/auth/spreadsheets,https://www.googleapis.com/auth/drive.readonly,https://www.googleapis.com/auth/documents,https://www.googleapis.com/auth/presentations.readonly,https://www.googleapis.com/auth/forms.body.readonly,https://www.googleapis.com/auth/forms.responses.readonly,https://www.googleapis.com/auth/tasks,https://www.googleapis.com/auth/userinfo.email
```

Tool behavior and the security model: [Google Workspace reference](google-workspace.md). Enterprise account-holder consent: [personal connections](google-personal-connections.md). Google Chat channel credentials are separate: [Chat setup](channels-googlechat-teams.md).
