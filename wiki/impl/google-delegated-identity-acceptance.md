# Google delegated identity acceptance

Status: IMPLEMENTED; final local validation in progress. The EXT continuation
adds dedicated-key execution, employee-specific HTTP MCP mounts, actual service
GET probes and completed-mail-poll status. This status does not mean real
provider acceptance, CI, merge, deployment or user acceptance.

## Engineering evidence and remaining gates

| ID | State | Evidence and remaining work |
| --- | --- | --- |
| B2a-EXT | LOCAL FIXTURE PASS | Closed 19-tool lane, explicit read/write scope and actual employee policy/approval gates. Gateway returns results, not Google bearers; accepted calls keep their employee/tool/final arguments and recheck live authority. |
| B2a-MCP | LOCAL FIXTURE PASS | HTTP `oauth://google` mounts select the employee account and recheck live requester/account permissions at each call. Exact-client hooks and separate actual-fallback notices preserve original payload redaction/CCR. Person-account stdio mounts are refused. Google's own endpoint acceptance remains unverified. |
| B2a-SVC | LOCAL FIXTURE PASS | Sheets, Docs, Slides and Forms find an accessible sample and GET the actual service resource. No sample stays unverified; issuance alone is not service access. |
| B2a-MAIL-STATUS | LOCAL FIXTURE PASS | Last completed poll timestamp, unavailable-account count and transport errors reach `mail.status` and the mailbox page, distinct from an empty or untested mailbox. |
| EXT-WRITE-RETRY | CONTROLLED UNIT PASS | Dedicated non-GET/HEAD writes make one attempt after transport/429/5xx. Actual signed-route503/429 RED observed two requests each; GREEN observes one. Three controlled tests pass, including GET/HEAD and legacy retry positives. Exact product delta independently approved; legacy caller semantics remain unchanged. |

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

The ordinary continuation baseline is
`scripts/live-test/google-connectors-baseline.mjs`. It requires a debug binary
with `live-test-mocks`, a fresh temporary home and the macOS loopback-only
sandbox. REST, MCP, model and RSA issuer are synthetic; the runtime has no host
model CLI in PATH. The baseline covers all 19 tools with normal approval for
mock writes, HTTP account mounting, actual configured fallback notice,
service GETs and mailbox availability status. It is not real-provider proof.

The historical `google-delegated-identity.mjs` evidence remains separate.
Do not re-run its paused P1 native scenarios. The seven prior actual P2
native RED cases, two fixture failures, lack of a reproduced P1 native RED,
and actual pure-unit P1 RED keep their original outcomes; later controlled
unit PASS does not rewrite them.

The 2026-10-24 ordinary baseline returned exit0: all19 dedicated tools,
six writes with normal Admin approvals, four actual service-resource GETs,
completed unavailable-mailbox status and two real gateway model tool loops
(person HTTP mount and explicitly configured Home fallback with its notice).
The success receipt records `cleanup_confirmed: true`; all owned processes,
connections and local servers stopped before it was written. The first two
ordinary attempts remain failures (missing mock Drive list response and mock
store path aliasing). Their minimal fixture corrections were independently
reviewed, with all original success assertions retained. No real provider,
host model account or OS credential store was used.

Latest-main integration, final backend tests and clippy, required CI, merge
and deployment remain independent gates. The earlier six-crate continuation
log reached all successful result blocks, but its process exit was not
retrieved; it is not relabeled as captured exit0.

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
