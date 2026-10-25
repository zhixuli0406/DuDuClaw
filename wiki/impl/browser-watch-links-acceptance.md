# Session-bound browser viewing links

Status: controlled local acceptance; unreleased. This record covers container
browsers. Local-browser target exclusion remains a cross-feature integration
gate until the Local implementation is present. CI, merge, provider operation
and user acceptance are separate gates.

## Contract

A signed navigation context identifies one agent and one `cu-` session in one
canonical gateway home, for at most 30 minutes. It grants no viewing right.
Opening, polling and stopping recheck the current viewer and exact session.
The link page exposes masked snapshots and exact-session stop even for direct
bindings. It never silently follows a replacement session. Normal dashboard
login, including second factor, preserves only the bounded same-origin
computer route; first claim and unrelated login routes retain their own flow.

Start notifications require the actual still-live inbound turn and runtime
proof. Mapped dashboard users can receive a viewing entry in WebChat or the
original Telegram, LINE, Discord or Slack account/thread. Unmapped external
users or an unusable dashboard base receive fixed text; there is no bot-account
fallback. Delivery is best effort. A bounded WebChat queue drains before the
final assistant frame. Background task/run association comes from host-owned
runtime scopes, not model-supplied task labels or ambient environment variables.
A matching task/run can retain its own viewing button; that button grants no
permission either.

## Separate local checks

- On the clean post-#209 integration, gateway focused checks passed in separate
  exit-0 invocations: 13 watch-link cases, five host-context cases, one
  recorded-run join, one notification-preview case, and two future-size tests
  covering four constructed futures. The gateway libtest was rebuilt from the
  integration source before these tests ran.
- CLI production and test type-check passed before the main #201 merge.
- Current frontend focus: five files and 39 tests passed. The full frontend
  suite passed 372 files and 3,070 tests; TypeScript and Vite production build
  both exited 0.
- The four main #201 manual documentation/catalogue resolutions retain both
  complete sources and were independently approved; owned Watch source
  remained unchanged except adjacent module/API additions. Independent code
  and security reviews of the clean integration found no blocking finding.
- `git diff --check` passed. A repository-wide `cargo fmt --all -- --check`
  with the local formatter still reports broad pre-existing main-tree format
  drift; it is retained as a failed advisory check and is not represented as a
  branch-local regression or a passing release gate.

## Actual browser/native run

The first attempt (R3) exited 1 after 370.31 seconds when the fixture timed
out. Its API phase checks passed, but the late screen showed connection loss.
The earlier UI PASS draft was marked invalid before completion. The attempt
remains FAIL; zero images during an outage are not revocation evidence.

A fresh R4 of the unchanged backend completed in 81.16 seconds, exit 0, and
reported cleanup complete. It used libtest binary SHA-256
`2999acf2a2fe0a116043b714b405bc8dcfddc7062d3553fa342c09cdf039483d`
and the local Docker overlay
`b79722bad6c9040dfa85ee01e63347c298c82cf02ecce07dd02ee570ffba2e8e`.
The fresh private home, synthetic users and host requester were confined by
the reviewed macOS sandbox; host HOME, Keychain and provider credentials were
unavailable. Chrome exercised Vite production assets against the debug
gateway, without embedded assets.

R4 independently passed baseline 11, watch context six, policy off four,
restore four and session-fence seven API cases. Actual Chrome verified:

- Normal fixture login returned to the exact signed link and displayed a
  masked synthetic page; canceling stop retained the session.
- A 390px viewport had body/scroll widths of 390px.
- Expired and revoked links showed the corresponding refusal, with no image
  or stop control. These were functioning-connection checks.
- Restoring policy and starting a replacement session did not revive the old
  link, including after the viewer gained a direct binding.
- Old-session stop refused while current-session stop succeeded.

Evidence is retained under the owned temporary directories
`dudu-team-native-MbQyZQ` (R3) and `dudu-team-native-ftxGwP` (R4), including
phase logs, result receipts and UI screenshots. R4 predates the main #201
merge; its bounded claim is not a native rerun of every later integration.

A current-main integration run then exercised source snapshot `358d01de` after
#209 had merged. Its freshly linked libtest binary SHA-256 was
`599fbf79d7b8d4a66a42f5fc3d72ae0146265bc6eba43f62ba7f823fd0a55f87`;
the fixed local image was
`sha256:50869233d29cbf14bd74c7c414310db24b2fbb453bf2d642e28421bf851545af`.
The isolated fixture at `dudu-team-native-LpwQCz` exited 0 after 276.75
seconds, the ignored native test passed, cleanup returned `Ok(())`, and the
existing production LWM fingerprint remained running and healthy.

That run passed baseline 11, watch-context six, policy-off four, restore four,
session-fence seven and finish six API checks. Actual Chrome against the same
gateway and the current Vite production build verified the exact `?l=` login
return, masked snapshot, cancel-without-stop, 390 px body and scroll widths,
expired and policy-revoked no-image/no-stop states, and refusal to follow a
replacement session even after direct access was granted. An initial browser
navigation used the obsolete `context` query name and correctly followed the
ordinary-login route; it was a harness input error, was not counted as a pass,
and the clean-origin `?l=` rerun supplied the acceptance evidence.

The notification delivery claim comes from production-path unit checks.
This fixture did not exercise an actual inbound model/channel delivery,
provider login, physical owner presence, native Linux/Windows, or user
acceptance. PR CI, merge and release remain separate gates. No image or
release was published.
