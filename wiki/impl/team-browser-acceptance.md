# Team browser viewing acceptance

Status: implementation with controlled local acceptance; unreleased. This
record covers container sessions. Local desktop browser, chat links, real
provider acceptance and owner release approval have separate gates.

## Behavior

The standalone `/computer/:agentId` page does not expand `agents.get` or
configuration access. Existing direct bindings retain their existing controls.
Reporting-line access requires the live Enterprise team-history policy and
the actual starter's qualifying relationship. It exposes masked snapshots and
an exact-session stop only. Raw streaming, takeover, resume and login reset
remain unavailable through reporting-line access.

A requester identity or a live turn ID alone is insufficient. The gateway
requires a private proof tied to the verified inbound message, final route and
current runtime. Background source scopes cannot become attended through a
nested Chat call. A runtime context and the original ingress must both remain
live; context expiry and guard destruction revoke already-cloned proofs. The
admitted start fact remains recorded after that turn ends and grants no
continued presence or control.

## Local verification

The following were separate invocations, with overlapping tests:

| Check | Result |
| --- | --- |
| New origin/lifetime regressions before the fix | 15 passed, 2 failed |
| Same attended cases after the fix | 17 passed, 0 failed, exit 0 |
| All computer-use session unit tests | 148 passed, 0 failed, 11 ignored, exit 0 |
| Host context tests | 4 passed |
| Team RPC tests and shared poll budget | 3 plus 1 passed |
| Future-size checks | 4 passed |
| Launcher environment and signature golden vector | 2 passed |
| Final CLI production type-check | exit 0 |
| Final gateway library suite, five isolated workflow groups excluded | 9,395 passed, 0 failed, 49 ignored, 32 filtered, exit 0 |
| Final frontend focus, TypeScript and production build | 16 passed; type-check and build exit 0 |

Backend snapshot `7e22bdfa` was exercised using the opt-in
`computer_team_native_dashboard_fixture`, the isolated launcher
`scripts/live-test/computer-team-dashboard.mjs`, and its phased acceptance
driver. The actual libtest binary SHA-256 was
`417be251d59121effa2d61dfe0d6c37bf086fe304ae57e82e3f35ad481aa5344`.

The controlled native run used a fresh private home, synthetic accounts and
requester registration, a loopback gateway, an existing locally built Docker
image, and the real Chrome dashboard. It passed in 349.60 seconds, exit 0:

- Manager status/snapshot allowed; seven raw/control/configuration paths and
  a wrong screenshot session ID refused.
- Password-field region masked while the synthetic page remained visible.
  Stop opened a confirmation and cancellation retained the session.
- At 390px, body width and scroll width both measured 390px.
- After polling stopped for more than the 120-second idle interval, Docker
  was actually paused. Viewing returned the old, timestamped stale frame;
  Docker remained paused and no new capture appeared in the audit.
- Disabling team viewing cleared the already-displayed image and stop button.
  Re-enabling started a new window and did not reveal the original session.
- An old team-view stop confirmation could not stop a replacement session, including
  after the viewer gained a direct Operator binding.
- Cleanup completed before the PASS record; owned sessions/server exited and
  the existing production LWM container fingerprint was unchanged.

The first native attempt failed because its session correctly ended after
120 seconds of inactivity. Its evidence is retained. The second attempt set
only the synthetic fixture's keep-alive to ten minutes, leaving shipping idle
and hard-deadline rules unchanged. The driver refuses to overwrite a phase's
previous result.

## Verification boundary

The native frontend was a Vite production preview of the Team source, against
a debug backend without an embedded dashboard. A later header-only change
identifies the selected employee; it has its own frontend checks. Synthetic
host requester registration is not an actual inbound channel/model run or a
physical owner-presence test. The local Docker overlay is not a published
image. Real service accounts, native Linux/amd64/Windows, release deployment
and user acceptance are not established by this run.

The strict all-target clippy invocation exited 101 at 64 core diagnostics.
The repository already marks clippy advisory in CI; this run is retained as
a failure. The diagnostic files match main except `types.rs`, whose branch
change adds only `persist_login`, outside those diagnostic regions. No lint
policy or CI rule was relaxed.

An additional direct invocation of the isolated workflow pilot tests exited
101 (four failures) because the launcher omitted the required verified
`DUDUCLAW_P1_PILOT_BINARY`; these flows did not reach execution. A current
branch default-feature CLI build and the five isolated groups remain pending.
Current PR CI and merge are separate gates; focused PASS counts do not replace
them.
