# Connect Google

Console instructions checked against the supplied research dated 2026-10-30. Google Cloud Console screens were not re-verified live for this guide.

| Your situation | Choose | Main limit |
|---|---|---|
| Company Google Workspace, with a super administrator | Service account with domain-wide delegation | Personal Gmail accounts cannot use delegation |
| Company Workspace, using your own OAuth application | OAuth, Internal | Organization members only; no seven-day Testing expiry |
| Personal @gmail.com, needing mail, calendar and spreadsheets | Apps Script bridge | Three services, eight actions including status |
| Personal @gmail.com, needing the full tool set | OAuth, External + Testing | Reauthorize every seven days while Testing |

## Start in DuDuClaw

1. Sign in as an administrator. Open **Manage → Integrations → Google** (`/manage/integrations?tab=google`) and start the setup wizard. The advanced form remains available.
2. Choose the matching row above. Read the data disclosure. Enter secrets only in the dashboard, never in an employee conversation.
3. Enable Google Workspace integration. The master switch is `config.toml [integrations] google_workspace = true`; it defaults to false. Saving an OAuth, service-account or Apps Script path enables this switch. Running the connection test does not change it.
4. Finish one of the paths below. When replacing a configured path, check the effective path and confirm the replacement. Stored OAuth tokens are kept until you explicitly disconnect them.

## OAuth

1. Open [Google Cloud Console](https://console.cloud.google.com/). Create or select a project using the project selector at the top.
2. Open **APIs & services → Library**. Search for and enable Gmail API, Google Calendar API, Google Sheets API, Google Drive API, Google Docs API, Google Slides API, Google Forms API and Tasks API in that project.
3. Open **Google Auth platform → Branding → Get Started**. Complete App Information, Audience, Contact Information and Finish. Choose Internal for organization members or External for a personal Gmail account.
4. In **Audience → Add users**, add every account that will connect while an External app is Testing. Testing permits up to 100 listed test users. Their authorizations and refresh tokens expire after seven days. Tell the wizard whether the app remains in Testing; after six days it warns that renewal is approaching.
5. Open **Data Access → Add or Remove Scopes**. Add the exact scopes shown in the dashboard. The list is also included below.
6. Open **Clients → Create Client** and select **Web application**. Both Web and Desktop client creation offer an optional “Use this client for an AI-powered agent” checkbox. Use a Web client for this dashboard flow.
7. Copy the redirect URI displayed in DuDuClaw into **Authorized redirect URIs** exactly. The default is `http://localhost:18889/api/mcp/oauth/callback`. The actual listening port, including a configured `[gateway] port`, determines the displayed URI. HTTPS is required except for localhost; raw IP addresses and wildcards are not allowed. A different host, port, path or trailing slash can fail authorization.
8. Save the client. Copy the client ID and download the client secret immediately. Google shows the secret only once and later displays its last four characters. Keep it in a password manager.
9. Return to the wizard, enter the client ID and secret, save, review the disclosure and select **Connect Google**. Complete consent in Google's window. Wait for the dashboard result. Retry from the dashboard if consent fails or polling times out.

### Remote host or Docker

The callback uses localhost on the computer running your browser. Forward the gateway port from the remote DuDuClaw host before connecting:

```sh
ssh -N -L 18889:127.0.0.1:18889 USER@DUDUCLAW_HOST
```

For Docker, publish the gateway port on the host's loopback interface first. Keep the tunnel open through consent. Replace both port values and the registered redirect URI when the gateway uses a different port. Do not register a raw public IP or expose the callback publicly as a workaround.

### Scope and lifetime limits

Gmail `gmail.send` is sensitive; `gmail.readonly`, `gmail.modify`, `gmail.compose` and `gmail.metadata` are restricted. Drive `drive.file` is non-sensitive; `drive` and `drive.readonly` are restricted. Restricted scopes with server-side data require annual CASA assessment when Google's verification requirements apply. Review Google's requirements before publishing an External app. Internal is limited to organization members.

The unverified-app screen permits 100 new users over the project's lifetime; this limit cannot be reset. It is separate from the 100 listed Testing users. Refresh tokens can also expire after six months without use. A client unused for six months is automatically deleted, with a warning email 30 days beforehand. A stored refresh token does not prove the connection still works.

## Service account and domain-wide delegation

1. Ask your Workspace super administrator to approve delegation. It lets the authorized service account act as domain users within the approved scopes. Personal @gmail.com accounts are ineligible.
2. In a Google Cloud project, enable the eight APIs listed under OAuth. Open **IAM & Admin → Service Accounts**, create an account and download a JSON key. Note its numeric client ID.
3. Give the super administrator the client ID and the dashboard's complete scope list. They open **Admin console → Security → Access and data control → API controls → Manage Domain Wide Delegation → Add new**, paste both and authorize. Google allows up to 24 hours for propagation.
4. For one shared company account, choose **Service account** under credential paths. Store the JSON key on the DuDuClaw host with owner-only permissions, enter its file path and the Workspace user's email, then save. The email is the user to act as, not the service-account address.
5. To act as a different colleague per employee, use **Security → Credential protection → Move to system keychain**, then **Integrations → Google → Company delegation → Import key file**. This separate flow seals the key and needs a supported macOS or Windows keychain. Linux and Docker cannot use this protected colleague flow. Set allowed domains and choose the colleague in the employee's Connections tab. See [account security and settings](google-workspace.md#per-employee-google-accounts-company-delegation-v1760).
6. Test the selected account. A token being issued proves authorization only; inspect the per-service results before using it.

`allowed_subject_domains` restricts delegated colleague addresses. `allowed_domains` restricts Enterprise personal OAuth accounts. Both are under `[integrations.google_identity]`; they serve different flows.

## Apps Script bridge

1. Open [Apps Script](https://script.google.com/) and create a project under the Google account you want employees to use.
2. Replace the script with the repository's [bridge template](../../templates/apps-script/duduclaw-bridge.gs). Generate a secret with `openssl rand -base64 32` and replace `CHANGE_ME_TO_A_LONG_RANDOM_STRING`.
3. Select **Deploy → New deployment → Web app**. Set **Execute as: Me** and **Who has access: Anyone**. Review the consent request for your own script and approve it.
4. Copy the deployment's `/exec` URL. `/dev` is for the owner's browser session and will not work here.
5. In the wizard, choose Apps Script, paste the URL and secret and save. Confirm replacement if another path is effective. Select **Test connection** and check the returned account.
6. Keep the URL and secret private. To rotate, change the script secret, redeploy and update the dashboard secret. Apps Script daily quotas apply, especially on consumer accounts.

The bridge supports Gmail search, read and draft; Calendar list and create; Sheets read and append. Including `status`, the template dispatches eight actions across three services. Drive, Docs, Slides, Forms and Tasks are unavailable. It has no mail-send action. The URL is publicly reachable; the secret protects access. The dashboard stores the secret encrypted.

## Verify and finish

1. Run the dashboard connection test. OAuth and service-account tests read Gmail `users.getProfile`, Calendar `calendarList.list` with `maxResults=1`, and Drive `about.get` with `fields=user`. Each result reports its service, success or reason and an API activation link when available.
2. Sheets and Docs are reported from granted scopes with `verified:false`. This does not prove document access. Ask the employee to read a spreadsheet or document you own before relying on that service. Test Slides, Forms and Tasks with a known accessible item or task list too.
3. The bridge keeps its own account/status test. Then run one read action in each of its three services. Do not create mail, events or rows merely to check connectivity.
4. Check the Google tools are visible to the intended employee. `google_status` is hidden when the integration gate is off or that employee's Google identity is disabled. Use the dashboard to save the selected path or enable the integration, then run its connection test. OAuth testing requires the integration to be enabled. Its identity-specific output can include local authorization metadata or token acquisition; it is not a substitute for a service read test.

## Troubleshooting

| Result | Action |
|---|---|
| `SERVICE_DISABLED` / `accessNotConfigured` | Open the displayed Google Console activation link and enable that API in the client's project, then test again |
| Insufficient scope | Reconnect and grant the required scopes |
| `access_denied` | Start again and approve access with the intended account |
| `admin_policy_enforced` | Ask your Workspace administrator to allow the application and requested services |
| `invalid_scope` | Compare Data Access and the delegated scope list with the dashboard list |
| `needs_reconnect` / refresh `invalid_grant` | Reconnect; Testing expiry, revocation or an expired refresh token can cause this |
| Redirect mismatch or timeout | Compare the exact URI, confirm the local port or SSH tunnel, then retry |
| Other `403` reason | Use the reported reason code to investigate; reconnecting does not enable a disabled API |
| `unauthorized_client` on delegation | Check numeric client ID, all scopes and administrator authorization |

App passwords and basic IMAP/SMTP authentication were retired in 2025. Use a supported connection path here.

## Requested scopes

`calendar.calendarlist.readonly` supports the read-only calendar-list probe. The [calendarList.list authorization table](https://developers.google.com/workspace/calendar/api/v3/reference/calendarList/list) lists its accepted scopes; `calendar.events` alone is insufficient.

```text
https://www.googleapis.com/auth/gmail.readonly
https://www.googleapis.com/auth/gmail.compose
https://www.googleapis.com/auth/calendar.events
https://www.googleapis.com/auth/calendar.calendarlist.readonly
https://www.googleapis.com/auth/spreadsheets
https://www.googleapis.com/auth/drive.readonly
https://www.googleapis.com/auth/documents
https://www.googleapis.com/auth/presentations.readonly
https://www.googleapis.com/auth/forms.body.readonly
https://www.googleapis.com/auth/forms.responses.readonly
https://www.googleapis.com/auth/tasks
https://www.googleapis.com/auth/userinfo.email
```

Tool behavior and the security model: [Google Workspace reference](google-workspace.md). Enterprise account-holder consent: [personal connections](google-personal-connections.md). Google Chat channel credentials are separate: [Chat setup](channels-googlechat-teams.md).
