# Google delegated identity acceptance

Status: PARTIAL. This branch implements gateway-issued domain delegation for
native Google tools and account-aware Agent Mail receipts. It is not the
complete per-employee Google identity work package.

## Open requirements

| ID | State | Remaining work |
| --- | --- | --- |
| B2a-EXT | OPEN | External per-employee API keys currently lack the gateway-issued employee proof needed by the signed token route. Even an explicitly allowed `requesters = "anyone"` client cannot obtain a delegated token. Add a separately authenticated route path; keep anonymous and general external keys unable to select a person. |
| B2a-MCP | OPEN | `oauth://google` mounts are skipped for delegated identities. Support requires account-specific mounting plus a per-call interceptor that rechecks the current requester and refuses after access changes. Do not substitute the shared token. Acceptance against Google's remote MCP endpoints remains separate. |
| B2a-SVC | PARTIAL | Connection testing mints all scopes and checks Gmail identity, then mints each service scope. Only Gmail, Calendar, Drive and Tasks also read a service endpoint. Sheets, Docs, Slides and Forms data access is not verified. |
| B2a-MAIL-STATUS | OPEN | `PollReport.account_unavailable` is counted and logged but not yet exposed on the mailbox page. |

## Security boundaries

- Every native delegated call re-enters gateway authorization. Only minting
  is cached in the gateway, after checking settings, requester access,
  account state, credential protection and the sealed key.
- The `[google]` parse is part of `AgentTomlSections`; readers use the same
  effective preset artifact as other employee configuration readers. Invalid
  Google fields deny Google without discarding other sections.
- Personal mail authorization follows each received row's actual
  `google_account`, not the current selector. Shared fallback receipts,
  legacy rows and drop-folder mail do not become a person's data when a
  selector changes. Listings filter before pagination; unauthorized reads
  neither reveal nor mark messages read. Gmail dedupe includes the account.
- The outbox and state-changing mail RPCs retain the Manager gate and a
  conservative persistent personal-account privacy floor. Personal inbox
  reads do not grant SMTP decision rights.
- A local "configured" status does not prove key unsealing, token issuance
  or service access. Use the connection test and report its actual steps.
- Real Workspace authorization, real keychains on each supported OS,
  Windows execution and user acceptance require separate evidence.

## Verification

The repeatable isolated gateway/MCP probe is
`scripts/live-test/google-delegated-identity.mjs`. It requires a debug binary
with `live-test-mocks`; the release profile refuses that feature. It creates
its own temporary home, loopback fake Google and ephemeral RSA key, uses an
in-memory credential store, disables model dispatch and keeps artifacts for
inspection. It does not exercise the open external-key or remote-MCP paths.

Final local test counts and live-probe outcomes are recorded in the PR; CI,
merge, deployment, real-provider acceptance and user acceptance are distinct
gates.

The 2026-10-23 isolated native probe passed all 12 scenarios after a real
RPC regression fixed unnecessarily reconfirming an `anyone` policy when
switching to `none` or `home`. It verified signed subject isolation,
per-call reauthorization in one MCP process, domain/employee-proof refusals,
explicit shared-account fallback, tool hiding, master switch, offline denial
and token-free audit rows. The mock binary was built with no default features
and `live-test-mocks`; no real Google account or provider consent was used.

The earlier full local command returned failure only in the existing core
gateway-instance lock reacquisition test. Its gateway and CLI suites passed;
30 focused reruns and a separate full core rerun passed. These separate runs
do not change the original command's failure result.
