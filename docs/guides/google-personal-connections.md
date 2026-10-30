# Personal Google connections for Enterprise administrators

Use this guide when each person should decide which AI employees may use their Google account. The account holder must connect and consent personally. For a shared company connection, OAuth client creation or domain-wide delegation, start with [Connect Google](google-setup.md).

## Prepare the installation

1. Configure the Google application in **Manage → Integrations → Google** and enable Google Workspace integration. A person's connection never changes that installation-wide switch.
2. Enable **Security → Credential protection → System keychain**. Personal connections require a supported macOS or Windows keychain. Password-only protection, an unavailable keychain or unreadable saved credentials refuse access. Linux and Docker do not currently support this personal-connection mode.
3. Register the exact return address shown by **My connections** in the Google client. This personal return address ends in `/api/me/google/callback`; the shared-connection wizard uses `/api/mcp/oauth/callback`. Register the address for the flow you are using. An approved HTTPS dashboard uses its dashboard address; other origins use local HTTP and offer a return-URL field. Match the host, port and path exactly. Do not expose a local callback publicly to work around a failure.
4. Check Google's account and application restrictions in the [setup guide](google-setup.md). External applications in Testing need renewed authorization every seven days. Restricted Gmail access can require Google verification and a security assessment before production use.
5. Review the Google data disclosure and the configured AI provider's terms. Google Workspace data must not train or improve a generalized model. Use a suitable local model or leave Google disconnected if you cannot confirm the provider follows this restriction.

Use the dedicated Google connection screens. Custom OAuth or remote-server setup cannot replace the required Google disclosure. Dataset builds, dataset exports and new fine-tuning jobs remain disabled for the installation because exclusion of Google Workspace data cannot yet be guaranteed. Existing jobs can be viewed or cancelled, and datasets deleted. Older connections must be reconnected with the current disclosure before they can be used or refreshed.

## Ask the account holder to connect

1. Have the person sign in to the Enterprise dashboard and open **My connections**. Ask them to sign in again if the dashboard requires a fresh login.
2. They read and accept the data disclosure, choose **Connect my Google**, and approve the requested services in Google's window. A service they decline remains unavailable.
3. They return to My connections. If the local return page cannot load, they paste its complete address-bar URL into the original dashboard's return-URL field. That URL must remain private. Completion needs the same person and login that started the attempt, within ten minutes; an expired or replaced attempt must be restarted.
4. They check the connected Google email. It can differ from their DuDuClaw sign-in email.
5. They allow or deny access for each employee. Choosing an account as administrator does not supply the account holder's consent.

## Choose the employee and sharing rule

1. Open the employee's **Connections** tab. Choose **Follow primary owner** or **Choose colleague**. The selected person must be active and directly bound as an Owner.
2. With several Owners, choose a primary Owner explicitly when none is established. Adding a second Owner preserves the original primary Owner. Removing or demoting the primary removes that choice; a sole remaining Owner is used automatically.
3. Confirm a primary-Owner change only after checking its effect: the employee's active browser sessions stop and saved browser sign-ins are cleared. If cleanup fails, the Owner stays unchanged. Changing back does not restore previous sign-ins. Some older saved sign-ins also require a new login after upgrading.
4. Set who may ask the employee to use the account. The account holder reviews the listed colleagues, channels and sharing rule before consenting.
5. Leave company fallback off unless using company data when the personal account is unavailable is intended and explained to the account holder.

| Requester rule | Who may request use |
|---|---|
| `bound_people` | People bound to that employee, subject to its existing access rules |
| `owner_only` | The selected account holder; scheduled work still needs a valid employee, account and consent |
| `anyone` | Requires explicit administrator confirmation; public website visitors are still refused |

A direct Owner can choose **Use my Google** or disable Google. They can narrow the requester rule and turn company fallback off, but cannot widen access or choose someone else's account. Disabling Google turns fallback off and retains or narrows the requester rule; reconnecting their own account still requires the sharing acknowledgement. Use Connections for these changes.

Company fallback is considered only when the personal account is unavailable and an administrator has allowed it. A refused requester, invalid setting or credential-protection failure does not silently become company access. Once a personal-account operation has started, a later error does not switch it to company data. Review the account stated on an approval card; a company fallback may explicitly say its Google email has not been verified.

## Test before handing it over

1. Check that the installation switch is on, the selected person remains an active Owner, and that person has consented to this employee.
2. Ask the employee to perform one read in each needed service using known accessible mail, calendars or documents. Check both an allowed requester and a requester who should be refused. Avoid writes for connectivity tests.
3. Use the dashboard test for the company connection separately. `google_status` can report account and local authorization information; a status response is not proof that a particular service can be read. The tool is hidden when Google integration or the employee's Google identity is disabled.
4. Reconnect when authorization expires or additional services are needed. The current person, ownership, consent and granted permissions are checked again when the account is used. Removing consent blocks future calls immediately; it cannot recall a request already sent to Google.

Employee-specific external API keys identify an employee, not a human requester. Personal-account use through that route needs the administrator's explicit `requesters = "anyone"` permission and the usual consent and approval checks. HTTP Google MCP mounts check the actual requester on every operation. Personal-account stdio mounts are refused. Other external integrations need separate acceptance testing.

## Disconnect, offboard and review use

1. The person chooses **Disconnect** in My connections, or an administrator disconnects the person under **Integrations → Google**. Local access and pending connection attempts stop before Google revocation is attempted.
2. On offboarding, suspend or remove the person. An inactive person is refused even if credential cleanup could not finish.
3. Check whether Google revocation was confirmed. DuDuClaw sometimes disconnects locally only, because revoking one Google grant could also affect another personal account or the company connection. An unknown account, an unreadable store or a failed company-account check also prevents automatic provider revocation. Review Google's connected-app settings when you intend to revoke the entire grant.
4. Company domain-wide delegation is separate; personal disconnection does not remove it. Windows personal disconnection is currently local only. Dynamic remote OAuth disconnection is also local only; revoke those applications in Google yourself.
5. Review up to 500 retained entries from the last 30 days. History shows time, employee, tool and success state, without arguments, results or mail content. Log rotation can remove earlier entries. Personal mail visibility does not grant permission to send mail or approve outgoing messages.

## If Google revocation is uncertain

A timeout, interrupted process or network failure after a revocation request may leave its result unknown. Only Google's immediate HTTP 200 reply confirms that request. DuDuClaw keeps local access stopped and blocks new Google or remote connections while the uncertainty record exists. There is no automatic expiry and no online button that safely clears it. Restarting, using another client ID or seeing an empty connected-app page does not prove the old request has finished.

1. Keep new connections disabled. Preserve the incident report, operation ID, audit evidence and `<home>/locks/google-grant-revoke-uncertain.json`.
2. Revoke the old application's grant in Google and obtain external confirmation that the earlier request has finished. If that cannot be established, keep the block and contact Google. Waiting an arbitrary period is insufficient.
3. Stop every gateway and CLI process using this installation. Only after that external confirmation may an offline operator archive the uncertainty record outside its active filename. Keep the operation ID and reason with the incident evidence; do not delete other credential or lock files.
4. Restart and ask the account holder to reconnect. Record that an operator removed the block using external evidence. The product cannot independently prove that a late provider request is now impossible.

Changing client IDs is not recovery proof because different clients can share one Google Cloud project. After an upgrade, restart all gateway and CLI processes before using these controls. Synchronous keychain operations may finish after a displayed timeout, and cancellation cannot undo a change already sent to Google. Inspect Google before retrying a write whose result is unknown, including `result_unconfirmed`.

## Administrator settings and limits

```toml
# config.toml
[integrations.google_identity]
allowed_domains = ["example.com"]
allowed_subject_domains = ["example.com"]

# agent.toml
[google]
identity = "owner"          # or "user:<direct-owner-id>"
fallback = "none"           # explicit company fallback: "home"
requesters = "bound_people" # or "owner_only"; "anyone" needs confirmation
```

`allowed_domains` limits personal OAuth account domains; empty permits any verified Google account, including Gmail. Google’s hosted-domain value is preferred, otherwise the email domain is used. This check alone does not prove Workspace membership. `allowed_subject_domains` controls delegated colleague addresses and has different empty-list behavior; see [Workspace security](google-workspace.md). Invalid settings refuse access.

Keep the supported system keychain with installation backups or moves, and do not downgrade while personal connections are in use. Credentials are protected on disk, but an employee with unrestricted Bash under the gateway's OS user can still bypass product controls or change local data. Give only the capabilities needed. Google's Gmail draft scope can also send mail through Google's API, although DuDuClaw's native draft tool does not send.

The explicitly selected company credential path takes precedence; a failed selected path is reported. Older installations keep legacy source selection until an administrator saves a path. Apps Script covers Gmail, Calendar and Sheets, with seven data actions plus status. It does not support CC, event attendees, Meet or use as a remote MCP bearer. Account selection is not a health check. An operation that may already have reached Google must not be retried through another path automatically.

Out-of-band credential changes cannot always be detected immediately. Timeouts and cancellation cannot force an already-running keychain or local disk operation to stop. Changes between checking settings and sending a request remain a limitation.
