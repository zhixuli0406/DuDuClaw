# Google delegated identity acceptance

Status: IMPLEMENTED. Local backend and isolated mock integration passed on
`375f8588`; strict clippy retains existing baseline failures. The EXT adds
dedicated-key execution, employee-specific HTTP MCP mounts, actual service GET
probes and completed-mail-poll status. Required CI, merge, deployment, real
provider acceptance and user acceptance remain separate gates in [PR #212](https://github.com/zhixuli0406/DuDuClaw/pull/212).

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

After integrating #199 `b5417bc5` and main `da2170b7`, independently reviewed
source `375f8588` passed the final six-crate command with a captured exit0:
45 unit/integration/doc result blocks totaled 13,931 passed, 0 failed,
60 ignored and 32 filtered. The selected packages were agent, CLI, core,
gateway, LLM and security, with no default dashboard feature. The five
workflow E2E groups run separately by CI remained excluded locally:
`three_page_pilot_`, `security_race_tests`, `resume_tests`, `f1b_e2e_tests` and
`f5a_e2e_tests`. This is not an unfiltered workspace test claim.

Shipping and mock binaries were rebuilt from that same integrated source;
both builds returned exit0 and saved-binary hashes matched the build outputs.
The ordinary baseline then repeated all original assertions with the new mock
binary and returned exit0 with confirmed cleanup: 19 tools, six approved
writes, four service-resource GETs, unavailable mailbox status and both
actual mounted model tool loops. All Google/MCP/model endpoints and accounts
were newly created local fixtures; no real provider/keychain acceptance is
claimed.

Strict `clippy -D warnings` returned exit101 at 66 existing core diagnostics.
All 92 reported primary/secondary source locations matched dependency #199's
existing code. No lint allowance or CI rule was changed; this is not strict
clippy PASS. The previous two-async-lint exception was not applied to a
different baseline failure. The earlier six-crate continuation log reached
all successful result blocks but its process exit was not retrieved; the new
captured exit0 belongs only to the integrated final run and does not rewrite
that history.

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

## 2026-10-25 follow-up: integrated M1/W3 and requester-policy validation

This section adds the latest focused result; every earlier failure, baseline,
remaining-gate statement and native-probe outcome above remains a historical
record. In particular, the three earlier failed focused attempts remain failed.
Their three fixture-precondition corrections were independently reviewed; a
new execution passed without dropping the original success assertions.

The integrated source is `4e37c1f30b6b3e93b5e2e8d1e694301f3ff71db5`
(tree `f0237f928f0dd9ccce31e82cb6a53393317ccf94`). It includes multiple
unmerged changes, so these results must not be attributed to an isolated PR
head. Code/security review and the independent actual-execution review passed.

| Focused local gate | Result |
| --- | --- |
| Four libtest binary builds | Four natural exit0 results; 486.4104803330265 seconds |
| Runtime | 22 commands, 107 unique tests; natural exit0 for every command |
| Test outcome | 107 passed, 0 failed, 0 ignored, 0 measured; 143.9706507079536 seconds |
| Coverage groups | M1/CLI 43; W3 58; requester-policy regressions 6 |
| Binary totals | Gateway 91; core 2; CLI 0; LLM 14 |

This is `PASS_FOCUSED107_ONLY`, not a complete workspace suite or a shipping
CLI stdio acceptance result. PR CI applies to each PR's own latest head;
local integrated evidence cannot replace it. Earlier D1/native evidence
retains its own source and scope.

### Requester policy and Bridge boundary

A non-Admin direct Owner disabling Google retains the prior `requesters`
restriction or a valid narrower value; explicit widening remains refused.
Disabling sets company fallback to `none`. Re-enabling the caller's own
`user:<your-user-id>` account, or `owner` only when the resolved primary
Owner is the caller, keeps that requester policy and still requires the
holder's sharing confirmation. Admin `none`/`home` policy clearing, generic update narrowing,
live ACL, CAS and consent checks retain their existing contracts.

The native `attendees` contract is a comma-separated string. Bridge refuses
a nonempty `cc`, nonempty attendees parsed from CSV, or `with_meet=true`
before I/O. Empty `cc` or attendees and `with_meet=false` do not request
those unsupported features and can proceed as ordinary requests. This does
not close all argument-validation gaps: an array
passed as `attendees` is still ignored by the existing `as_str()` parsing
path. The focused PASS must not be read as rejection of every malformed or
unknown argument.

### Gates still requiring separate evidence

Real Google/Workspace Console configuration and provider compatibility,
acceptance of a token by an official remote MCP endpoint, actual Keychain
behavior, physical Windows operation, shipping CLI stdio, paid-license and
browser user acceptance remain unverified by this run. Keychain/external
secret rotation without a revision feed retains its per-operation snapshot
limitation; synchronous OS calls and check-to-send races remain as documented.
No release, deployment, CI approval or user acceptance is inferred.

See the [personal-connection guide](../../docs/guides/google-personal-connections.md)
and the source regressions in
`crates/duduclaw-gateway/src/handlers/tests/google_identity_rpc_tests.rs`,
`crates/duduclaw-gateway/src/google_personal/owner_change.rs` and
`crates/duduclaw-gateway/src/google_identity/home_source/`.
