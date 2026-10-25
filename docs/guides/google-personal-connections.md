# Connect a personal Google account (Enterprise, unreleased)

Give an AI employee access to your own Google account without changing the company account. You choose which employees may use it and can withdraw permission at any time.

## Before connecting

Ask an administrator to configure the existing Google application under Integrations → Google, enable Google integration when appropriate, and enable credential protection using the supported system keychain. Personal credentials are sealed with that protected key. A password-only protection mode, an unavailable keychain, or an unreadable credential store refuses personal access. Linux and Docker do not currently have a supported system-keychain implementation.

The administrator must register the exact return address shown by My connections in the Google application. It ends in `/api/me/google/callback`. For an approved HTTPS dashboard origin, this is the dashboard address. Other origins use an HTTP loopback address on the dashboard port and offer paste completion. The port and full path must match Google's registered redirect URI. A redirect error requires correcting the registration; do not weaken authentication or expose a local callback publicly.

Google's external apps in **Testing** status issue refresh tokens that expire after seven days, unless the requested access is limited to the basic identity scopes listed by Google. This connection requests work services, so that exception does not apply. Internal Workspace applications and approval for production personal-Gmail access are different cases. Restricted Gmail access can require additional Google verification and a security assessment. The administrator must complete the applicable provider requirements before production use. See [Google's token rules](https://developers.google.com/identity/protocols/oauth2#expiration) and [web-server authorization guide](https://developers.google.com/identity/protocols/oauth2/web-server).

## Connect your account

Before DuDuClaw opens Google, it shows what Google data the requested scopes permit, where credentials and retrieved content are processed or stored, and that any configured AI provider must not use Google Workspace data to train or improve a generalized model. Review the linked privacy policy and explicitly confirm the disclosure. The official UI cannot continue without that confirmation, and the RPC must explicitly carry the acknowledgement. If an administrator cannot confirm that the configured provider follows this restriction, use a suitable local model or do not connect Google.

Google authorization is accepted only through the dedicated Google connection flows. A custom OAuth provider or remote MCP whose issuer, authorization endpoint, token endpoint, or requested Workspace scope identifies Google is refused. Until training rows carry reliable source lineage that can prove Google Workspace data is absent, DuDuClaw also refuses dataset builds, dataset exports, and new fine-tuning jobs for every installation. Existing jobs can still be inspected or cancelled, and datasets can be deleted.

Connections created before this disclosure revision cannot be used or refreshed until they are reconnected and the current disclosure is accepted. They remain visible for local disconnect and provider-revocation cleanup; DuDuClaw does not silently treat an older connection as accepted.

1. Sign in to the Enterprise dashboard, open **My connections**, review and confirm the Google data disclosure, and select **Connect my Google**. Older login credentials require signing in again.
2. Complete Google approval in the new window. Google allows individual services to be deselected. Only the permissions actually returned by Google are saved; a service you did not allow cannot be used.
3. The browser returns to My connections. If the loopback page cannot load on your computer, copy its complete address-bar URL into the original dashboard's return-URL field. Do not send that URL to anyone else.
4. Check the connected Google email. It may differ from your DuDuClaw sign-in email; the page explicitly highlights the difference.
5. If Google integration is disabled, the connection is saved but unavailable until the administrator enables it. Connecting a personal account never changes that installation-wide switch.

A return code alone cannot finish a connection. The gateway requires the same person and verified dashboard login that started it, within ten minutes. A finished, expired, replaced or disconnected attempt cannot be reused. Access and refresh credentials never appear in dashboard responses, URLs, usage history, or employee configuration.

## Let an employee use it

An administrator can choose **Follow primary owner** or **Choose colleague** in an employee's Connections tab. The selected person must be an active, directly bound Owner. With several Owners, an explicitly chosen primary Owner wins; an old ambiguous binding requires an administrator to choose one. Adding a second Owner preserves the existing single Owner as primary. Removing or demoting the primary removes that designation; a sole remaining Owner resolves automatically.

Changing the primary Owner requires confirmation: it stops the employee's active browser sessions and clears saved browser sign-ins before selecting the replacement. A failed reset leaves the primary Owner unchanged. The gateway checks the administrator's current account status, role and password-reset requirement again in the final ownership transaction, so a permission change while browser cleanup waits cannot authorize the write. Returning A→B→A does not restore A's previous browser sign-ins.

The ownership transaction also advances a per-employee primary-owner version. Saved browser profiles validate that version together with the Owner rows from one database snapshot, so overlapping A→B→A changes cannot revive a login created between cleanup and the writes. Unrelated employees keep their own versions. Existing saved sign-ins using the older ownership format are discarded on next use after this update; sign in again to create a new profile.

A bound Owner can choose **Use my Google** or disable Google. The Owner may narrow who can request use and disable company fallback, but cannot widen those settings or select somebody else's account. The page asks the Owner to acknowledge the sharing boundary before saving their own account.

For a non-Admin direct Owner, disabling Google preserves the current `requesters` policy or accepts a valid narrower value; an explicit wider value is refused. Disabling also turns company fallback off. Re-enabling the caller’s own `user:<your-user-id>` account, or `owner` only when the resolved primary Owner is the caller, preserves that requester policy and still requires the account holder to confirm the sharing boundary. Admin changes to `none` or `home` keep their existing policy-clearing behavior. Generic employee updates, live access checks, compare-and-swap checks and consent requirements are unchanged.

An administrator's selection does **not** grant consent. The account holder opens My connections and allows or denies each employee. The page lists bound colleagues, allowed channels and the requester rule before consent. Public visitors are always refused. Scheduled work may use a connected account under the same employee/person/consent checks. Under `owner_only`, work carrying a human requester must belong to that person.

Withdrawing consent refuses future calls immediately, including calls through an administrator-configured employee. The generic employee editor cannot introduce a self-account selection; use Connections so self-consent and audit are recorded together.

An employee-specific external key does not identify a person. Dedicated native Google execution uses the account and final arguments fixed by the one-time approval, with current requester, Owner and consent checks. A dedicated key therefore needs the administrator’s explicit `requesters = "anyone"` permission for a personal selection. Remote HTTP Google mounts for a configured personal account use the actual requester and current permissions for each operation; they do not inherit an earlier discovery caller. Personal `oauth://google` stdio mounts remain refused. The original in-product personal Google native-tool route still applies its requester and consent checks and refuses public visitors. Support for other external routes requires separate verification.

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

## Unreleased account selection and status

Company-account fallback fixes an existing granted scope to the originally selected native tool and preserves the current any-of service policy. It selects the same company-shared OAuth connection only when the original personal account is unavailable and an administrator has allowed company fallback. A healthy personal selection cannot switch to company fallback after a later error.

The dedicated connection's approval flow fixes the selected account and final arguments. For company fallback, the approval card states that this call uses the company-shared connection and that its Google account subject has not been verified; it does not present a verified email address. `google_status` consumes that one-time approval and reports local authorization metadata without obtaining a token or testing a Google connection.

The selected-account dedicated sender covers the 18 native HTTP tools. It keeps the approved account throughout the operation and does not retry or follow redirects. A later personal-account error does not switch to company fallback. Company fallback adds the same notice to the result that its Google account subject is unverified. The remote Google MCP extension below uses a separate operation budget. The Home service-account, Apps Script and Delegate paths use the source rules below.

If an operation may have reached Google but its result cannot be confirmed, it returns `result_unconfirmed`. For writes, inspect Google before trying again; cancellation or a timeout does not undo a remote change. The dedicated route keeps its 120-second budget and each HTTP request has a 30-second timeout. It refuses responses over 4 MiB, more than 16 MiB of response data per operation, a request URL plus body over 1 MiB, more than 26 HTTP requests, or a full result envelope over 512 KiB. Gmail search is limited to 25 messages. An oversized result is refused rather than reported as a partial success. Existing legacy routes keep their current limits.

For remote Google MCP over HTTP, each operation fixes the original operator entry, endpoint, headers, tool and final arguments. A healthy personal selection cannot switch to company fallback after an error. An authorized company fallback uses OAuth only and reports that its Google subject is unverified. Home-only and domain-delegation routes retain their existing behavior and are outside this extension. A changed mount or a newly unauthorized requester is refused.

The remote operation has one deadline covering queueing, preparation, the three protocol POSTs and result processing. Its request frame is limited to 16 MiB and each POST response is separately limited to 16 MiB; this is not a 16 MiB total across the operation. It does not retry, follow redirects or use environment proxies. A late result is withheld, including its account notice and new retrieval handle. Cancellation cannot undo a Google write; inspect Google before retrying an unconfirmed operation. Local metadata maintenance that has already started can finish later, so cancellation does not promise immediate deletion of every local record.

The remote extension remains unreleased. Its M1/CLI focused local tests and independent code/security review have passed within the integrated validation described below. Earlier D1 evidence remains separate; live Google verification and user acceptance are not established by these tests.

System keychain and synchronous local reads or writes cannot be forcibly interrupted by a timeout; the same deadline is checked again when they return, before sending or delivering a result.

### Company source selection and confirmation

A configured Home service account is tried first; OAuth is tried only when no service account is configured. A direct credential failure can select the configured Apps Script connection before approval. The approval card distinguishes OAuth, service account and Apps Script without inventing an OAuth identity or a verified Google email. Delegate requester/protection refusals stop; only an eligible soft failure with explicit Home fallback may use the company source.

Personal-to-company fallback prepares credentials once before the existing Ask. The server has one 18-second preparation budget; the CLI keeps its 20-second timeout. The nominal approval window stays 1800 seconds, but the prepared credential can expire earlier. The preparation deadline also covers returning its completed result; a late synchronous check cannot admit a new operation. Moving the system clock backwards does not extend a captured OAuth credential lifetime. Expiry or a changed source requires selecting and approving again; execution never refreshes or chooses another backend after Ask. Pending selections keep their existing limits: 1024 total, 32 per employee, 64 KiB per entry and 8 MiB aggregate, including retained credential capacity and in-flight reservations. Large secret references can exceed the entry limit and be refused.

Home service accounts and Delegate support the 18 native HTTP tools and remote Google MCP with the full existing scope list. Apps Script supports Gmail search/read/draft, Calendar list/create and Sheets read/append; CC, attendees and Meet remain unavailable through that bridge. Remote MCP cannot use Apps Script as a bearer. Metadata status obtains no token and performs no Google connection test. Data operations keep the approved source, disable automatic retry, redirects and ambient proxies, and keep the existing response limits. Bridge may make one credential-free result GET after a validated 302/303; POST and GET share one 60-second deadline. A result URL reflecting the original Bridge secret is refused before GET; it is never redacted and sent. A possible write failure is unconfirmed: check Google before retrying.

The native `attendees` argument is a comma-separated string, not an array. Bridge refuses a nonempty `cc`, nonempty attendees parsed from CSV, or `with_meet=true` before I/O. Empty `cc` or attendees and `with_meet=false` do not request those unsupported features and can proceed as ordinary requests. This does not guarantee rejection of every unknown or malformed field.

File/environment/config/marker changes are observed locally. Keychain and external secret managers without a revision feed use a per-operation snapshot; out-of-band rotation and external provider recovery cannot be detected immediately without another request. Synchronous OS/key-store calls cannot be hard-preempted; checks on return prevent starting the next step after expiry. Cross-file observation and check-to-send races remain limitations. These changes remain unreleased. W3 focused local tests and independent code/security review have passed; actual Google/Apps Script compatibility, platform checks and user acceptance require separate evidence. CI follows each PR’s latest head. Legacy automation APIs such as mail polling are outside this change.

Selection performs no Apps Script health POST. A historical direct credential failure fixes the pre-Ask choice; it is not current provider health. A direct data request that may have been sent never triggers a second Bridge request. The legacy direct-to-Bridge behavior remains in the legacy API lane.

## Verification scope (2026-10-25)

Integrated source `4e37c1f3` compiled four test binaries and passed 107 focused local tests across 22 commands: M1/CLI 43, W3 58 and requester-policy regressions 6, with no failures or ignored tests. Independent code, security and execution-record reviews passed. This is neither a full-suite result nor validation of an isolated PR head; check each PR’s latest CI separately. Historical failures remain in the [acceptance record](../../wiki/impl/google-personal-identity-acceptance.md).

These tests do not establish real Google/Workspace Console behavior, acceptance of the token by an official remote MCP endpoint, real Keychain behavior, physical Windows operation, shipping CLI stdio acceptance, paid-license or browser user acceptance. Out-of-band credential rotation retains the snapshot limitations above. No release or deployment is claimed.
