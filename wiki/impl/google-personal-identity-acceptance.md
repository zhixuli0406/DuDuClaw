# Personal Google identity acceptance

Status: implemented and verified with local mocks; integration remains partial.
Nothing has been merged or released. Real Google and OS-keychain acceptance
are separate gates. The public three-language guide starts at
[`docs/guides/google-personal-connections.md`](../../docs/guides/google-personal-connections.md).

## Source and verification boundary

Production Rust is frozen at `1bb3e914`, based on B2a dependency `e5047107`
and main `1cc573e5` (merge `3bdcd00c`). Independent review approved the
credential/OAuth implementation, the five append-only merge resolutions and
the structured-refusal test fix. The final driver changes and this evidence
update require their own final review.

All local Rust commands used one build job, incremental compilation disabled,
an isolated target and the coordinated two-slot wrapper. Later dev/test builds
set `CARGO_PROFILE_DEV_DEBUG=0` and `CARGO_PROFILE_TEST_DEBUG=0`; debug assertions
remained enabled. These are debug builds, not release builds.

| Gate | Actual result |
| --- | --- |
| Auth library | 79 PASS; auth source unchanged by the main merge |
| First gateway focused execution | 22 PASS / 2 FAIL; fixture/assertion issues subsequently fixed, original result retained |
| First complete gateway execution | Exit 101; 25 groups: 9,477 PASS / 1 FAIL / 51 ignored. Lib: 9,351 PASS / 1 FAIL. Other 24 groups: 126 PASS / 0 FAIL / 5 ignored, including remote OAuth integration and doctests |
| Sole complete-run failure | The old helper converted the correct structured Personal-edition refusal to an empty string. Only the test was fixed; it now also asserts the exact refusal code |
| Final gateway lib execution | Exit 0; 9,352 PASS / 0 FAIL / 46 ignored / 32 filtered, using the five existing CI skip groups and two test threads |
| Personal OAuth and vault cases | All 24 personal tests and all 5 vault tests pass within the complete lib run |
| CLI/core complete execution | Exit 0; 14 groups: 3,383 PASS / 0 FAIL / 7 ignored. CLI lib: 2,241 PASS; core lib: 1,054 PASS; integrations/doctests also pass |
| Dashboard | Focused 102 PASS; integrated TypeScript/Vite build PASS; Dashboard CI PASS |
| Debug mock shipping binary | Build PASS with `live-test-mocks`; latest dashboard assets embedded |
| Native probe | Exit 0 in a second fresh home; seven groups pass, described below |
| Clippy, original command | Exit 101: two existing `async_yields_async` errors in unchanged main test files; original log retained |
| Clippy, targeted baseline allowance | Exit 0 with only `-A clippy::async_yields_async`; other lints remain active, existing warnings remain |
| Owner-change test after lint-only rewrite | Exit 0; 1 PASS / 9,429 filtered; production prefix is byte-identical to the frozen source |

The complete gateway integrations were not repeated after a test-only helper
change: their production source and test source were unchanged. The final lib
run is a separate execution; it does not rewrite the original 101 result as
a successful complete run.

The two baseline Clippy errors are in
`channel_reply/tests/ccr_guarded_reply.rs:125` and `wiki_ingest.rs:5047`;
their Git blobs match main exactly. Two `bool_assert_comparison` warnings in
the new Owner-change test were fixed without changing test meaning or
production behavior. The two core Google-identity style warnings (manual
`SectionRead` default and needless return) already exist in the B2a dependency
`e5047107`; they were not silently described as a clean lint baseline.

## Native probe

The real binary was run on macOS arm64 through an OS sandbox that denied
the operator's credential/keychain paths, securityd access and all outbound
connections except loopback. The parent driver and helper processes use
minimal environments and fresh HOME/DUDUCLAW_HOME directories. OAuth, userinfo,
Gmail and revocation were loopback mocks. No real Google write, model call or
real keychain operation was performed.

Binary SHA-256:
`47dfe42eaa9e8b41e6e9923dd834faa45771d9c7fd908913997c26466e1ed9f1`.
Guard SHA-256:
`30239411430d0765744ad00760caf6d04ee42b2097f5fc06071c2b69c872265e`.

The retained `result.json`, `proof.json`, gateway log and parent-driver log
identify the fresh fixture and driver hash. The probe verifies:

- Exact redirect, S256 verifier, actual partial scopes, cross-user/session
  refusal, same-session completion after JWT refresh, and single use.
- Saving while the integration master switch is off; explicit Anyone still
  needs self-consent; refresh works; denied draft writes send no service HTTP.
- Primary Owner A→B→A selects the correct person's grant without borrowing the
  previous Owner's connection.
- Self-refusal stops use; live status revalidation rejects suspension and
  reactivation preserves the grant. Reversible suspension is injected only
  into the fresh fixture DB because this base has no reversible status RPC.
- Invalid grant becomes `needs_reconnect`; local disconnect is visible before
  slow provider revocation; another connection waits through that revocation.
- The actual `users.remove` path removes the offboarded person's record and
  refuses their subsequent self-service requests.
- The connected vault contains sealed `pk:v1` data; neither vault nor audit
  contains raw access/refresh tokens.

The first native attempt passed its product assertions but the driver waited
for a second exit event after the gateway had already terminated by signal.
Only the lingering driver was terminated (exit 143); that run and its fixture
were retained. The driver now checks `signalCode` as well as `exitCode`, installs
exit listeners before signalling, and clears the shutdown timer. A new home
then passed and exited normally. The first attempt is not counted as an
end-to-end pass.

## Remaining gates

- Integrate the newer B2a work and its separately tracked external-key/MCP
  interceptor closure. This PR does not claim those work packages complete.
- Wire B9 profile invalidation after every successful primary-Owner change,
  including A→B→A; final-field equality is not enough. B9 is not in this base.
- Complete the final driver/evidence review and required PR checks.
- Real Google app configuration/review, real provider behavior and keychain
  storage, Windows/native Linux acceptance and browser-click user acceptance
  remain unverified.

The uncertainty barrier has no online reset or automatic expiry. Offline
recovery relies on external human evidence; the product cannot prove that a
timed-out provider request has stopped. See the guide's recovery section.

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
