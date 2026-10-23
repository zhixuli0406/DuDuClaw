# Connect a personal Google account (Enterprise, unreleased)

Give an AI employee access to your own Google account without changing the company account. You choose which employees may use it and can withdraw permission at any time.

## Before connecting

Ask an administrator to configure the existing Google application under Integrations → Google, enable Google integration when appropriate, and enable credential protection using the supported system keychain. Personal credentials are sealed with that protected key. A password-only protection mode, an unavailable keychain, or an unreadable credential store refuses personal access. Linux and Docker do not currently have a supported system-keychain implementation.

The administrator must register the exact return address shown by My connections in the Google application. It ends in `/api/me/google/callback`. For an approved HTTPS dashboard origin, this is the dashboard address. Other origins use an HTTP loopback address on the dashboard port and offer paste completion. The port and full path must match Google's registered redirect URI. A redirect error requires correcting the registration; do not weaken authentication or expose a local callback publicly.

Google's external apps in **Testing** status issue refresh tokens that expire after seven days, unless the requested access is limited to the basic identity scopes listed by Google. This connection requests work services, so that exception does not apply. Internal Workspace applications and approval for production personal-Gmail access are different cases. Restricted Gmail access can require additional Google verification and a security assessment. The administrator must complete the applicable provider requirements before production use. See [Google's token rules](https://developers.google.com/identity/protocols/oauth2#expiration) and [web-server authorization guide](https://developers.google.com/identity/protocols/oauth2/web-server).

## Connect your account

1. Sign in to the Enterprise dashboard, open **My connections**, and select **Connect my Google**. Older login credentials require signing in again.
2. Complete Google approval in the new window. Google allows individual services to be deselected. Only the permissions actually returned by Google are saved; a service you did not allow cannot be used.
3. The browser returns to My connections. If the loopback page cannot load on your computer, copy its complete address-bar URL into the original dashboard's return-URL field. Do not send that URL to anyone else.
4. Check the connected Google email. It may differ from your DuDuClaw sign-in email; the page explicitly highlights the difference.
5. If Google integration is disabled, the connection is saved but unavailable until the administrator enables it. Connecting a personal account never changes that installation-wide switch.

A return code alone cannot finish a connection. The gateway requires the same person and verified dashboard login that started it, within ten minutes. A finished, expired, replaced or disconnected attempt cannot be reused. Access and refresh credentials never appear in dashboard responses, URLs, usage history, or employee configuration.

## Let an employee use it

An administrator can choose **Follow primary owner** or **Choose colleague** in an employee's Connections tab. The selected person must be an active, directly bound Owner. With several Owners, an explicitly chosen primary Owner wins; an old ambiguous binding requires an administrator to choose one. Adding a second Owner preserves the existing single Owner as primary. Removing or demoting the primary removes that designation; a sole remaining Owner resolves automatically.

A bound Owner can choose **Use my Google** or disable Google. The Owner may narrow who can request use and disable company fallback, but cannot widen those settings or select somebody else's account. The page asks the Owner to acknowledge the sharing boundary before saving their own account.

An administrator's selection does **not** grant consent. The account holder opens My connections and allows or denies each employee. The page lists bound colleagues, allowed channels and the requester rule before consent. Public visitors are always refused. Scheduled work may use a connected account under the same employee/person/consent checks. Under `owner_only`, work carrying a human requester must belong to that person.

Withdrawing consent refuses future calls immediately, including calls through an administrator-configured employee. The generic employee editor cannot introduce a self-account selection; use Connections so self-consent and audit are recorded together.

An employee-specific external connection key does not identify a human requester. The default rule refuses personal-account use; an administrator can explicitly select `requesters = "anyone"` to permit these authorized external tools, while personal consent is still required. Public website visitors remain refused even under `anyone`.

## Manage and disconnect

- **Reconnect** when Google access expires or you need additional services. Every tool call rechecks the active person, current Owner, consent, application, granted service permissions and connection revision. Refresh happens only in the gateway and concurrent refreshes are serialized.
- **Disconnect** deletes the local connection and cancels pending attempts before trying provider revocation. An administrator may also disconnect a person in Integrations → Google. Offboarding and removing a person drop the personal connection; an inactive person is refused even if a cleanup could not complete.
- Google revocation can affect an entire grant. If another saved personal record uses the same Google subject or email, or the company account matches, the gateway removes only the local connection. An unreadable store, unknown bridge account, expired home credential, or failed home-account probe also skips provider revocation. Review Google's connected-app settings yourself when you intend to revoke the full grant.
- Company delegation is separate. Disconnecting your personal connection does not remove an administrator's domain-wide delegation.
- The last-30-day history exposes only retained timestamps, employee names, tool names and success state, up to 500 entries. Inputs, outputs and mail content are excluded. Log rotation can remove older entries.
- Received personal mail is filtered per message. An active account holder can read their own saved-account mail; shared/legacy/drop-folder mail retains its stricter gate. Read access does not grant permission to approve or send outgoing mail.

## Revocation uncertainty and offline recovery

Grant exchange, account verification and publication share one cross-process lease with the revoke decision and provider request. This includes the home OAuth callback, the home refresh path used by CLI/native tools, and dynamic remote OAuth exchange/refresh. Local disconnect and its persistent fence happen **before** waiting for that lease. Restart all gateway/CLI processes when upgrading so an older process cannot bypass the new coordination.

Only the trusted Google revocation endpoint's synchronous HTTP 200 response counts as confirmation for that request. Other responses, transport errors, timeouts, process cancellation or a crash after sending leave `<home>/locks/google-grant-revoke-uncertain.json` in place. It is written and synced before the request starts. Future Google/remote grant creation is refused while that marker exists; the disconnected person's local access remains stopped. Unknown remote accounts, other unidentifiable home OAuth records and account-probe failures use local-only disconnection, report `provider_revoked=false`, and record the reason in the audit log.

There is no online “clear” RPC and no automatic timeout cleanup. A second successful request, a different client ID or a page in Google's account settings does not prove that an earlier timed-out request has stopped. Recovery is an **external human verification exception**, not a safety guarantee the product can check:

1. Keep new connections disabled. Preserve the uncertainty marker, its operation ID, the relevant audit rows and the incident record.
2. Revoke the old app grant in Google and obtain external confirmation that the earlier request has finished. If that cannot be established, keep the barrier; contact the provider rather than guessing a delay.
3. Stop every gateway/CLI process using that home. Only after the external check, an offline operator may archive the marker outside its active filename, keeping the operation ID and reason with the audit evidence. Do not delete other credential or lock files.
4. Restart and have the account holder connect again. Record that the barrier was removed by an operator relying on external evidence; it was not programmatically verified to eliminate a late provider request.

Different OAuth clients can belong to the same Google Cloud project, so changing a client ID is not a supported automatic recovery proof.

On Unix, a provider request requires successful marker-file, locks-directory and home-directory synchronization. Windows currently lacks the required directory-durability implementation, so personal disconnection there is local only; normal connection and refresh remain supported. The Unix tests are not Windows power-loss evidence. Dynamic remote OAuth disconnection is always local only, independently of whether personal/home credential files currently exist; revoke those apps at the provider yourself.

## Administrator reference

```toml
# config.toml
[integrations.google_identity]
allowed_domains = ["example.com"] # empty means unrestricted
# agent.toml
[google]
identity = "owner"               # or "user:<direct-owner-id>"
fallback = "none"                # administrator may explicitly choose "home"
requesters = "bound_people"      # or "owner_only"; "anyone" needs confirmation
```

`allowed_domains` checks Google's hosted-domain claim first, otherwise the email domain. It restricts account domains and is not independent proof of Workspace membership. Invalid configuration refuses access.

Self-service RPCs are `me.google.status`, `connect_start`, `connect_finish`, `connect_paste`, `disconnect` and `consents.set` under the `me.google` family. The gateway derives the person from the verified login. `users.google_disconnect` and `agents.primary_owner_set` require an administrator. These personal-connection surfaces are Enterprise only; the Personal edition keeps its existing company/home connection.

The sealed store is `<home>/person_credentials/store.json`, with an owner-only directory/file and a persistent version fence. It does not migrate or replace the existing company OAuth store. Preserve the supported system keychain when backing up or moving an installation; do not downgrade while personal connections are in use.

The platform gate does not isolate an employee with unrestricted Bash running as the same OS user. Such a process may bypass product paths or tamper with local state. Give employees the narrowest necessary capabilities. Gmail draft permission itself also permits sending at Google's API layer, even though the native draft tool does not send.

Related: [Native Google Workspace](google-workspace.md), [Google's PKCE guidance](https://developers.google.com/identity/protocols/oauth2/native-app#step1-code-verifier), [OpenID user info](https://developers.google.com/identity/openid-connect/openid-connect#obtaininguserprofileinformation), [Google revocation](https://developers.google.com/identity/protocols/oauth2/web-server#tokenrevoke).
