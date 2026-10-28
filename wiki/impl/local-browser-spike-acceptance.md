# Local browser implementation evidence

Status: **PARTIAL — synthetic native spike passed; product backend and desktop companion source is wired, with final integration validation outstanding. No release approval.**

Additional product boundary found during implementation: blocking keyboard print
shortcuts alone does not establish that scripted printing is disabled. Chromium
defines [`printing.enabled` as a profile preference](https://github.com/chromium/chromium/blob/main/chrome/common/pref_names.h),
and the browser-side print manager reads that preference. The dedicated profile
must disable it and the restricted native fixture must verify the behavior;
neither a JavaScript replacement for `window.print` nor a preference readback
is an acceptance result. No system printing policy or real printer is changed.

Branch: `feat/local-headed-browser`, initial dependency head `d77c5dc9`. All runs below used an internally derived, disposable HOME/profile and synthetic content. The user's normal Chrome profile, existing account credentials and desktop input were not test targets. No physical screen lock or real account login was performed.

## Native spike

The checked script is `scripts/live-test/local-browser-spike.mjs`. Native execution required an independently reviewed, immutable script hash. These runs retain Chrome's native sandbox; the successful native run does **not** have an additional OS sandbox and does not prove product acceptance.

| Run | Result | Evidence and boundary |
| --- | --- | --- |
| r1, extra OS seatbelt | EXIT 1 | Browser.getVersion worked over descriptor 3/4; Chrome renderer/network sandbox initialization failed under nested seatbelt, then Page.enable timed out. |
| r2, extra OS seatbelt | EXIT 1 | Same renderer limitation; owned browser and server cleanup assertions passed. |
| r2, native host | EXIT 1, genuine network failure | Chrome 156.0.8078.47 sent **4 STUN UDP packets** despite `--force-webrtc-ip-handling-policy=disable_non_proxied_udp`. A UDP candidate was visible. The zero-packet assertion correctly failed. Download denial/cancellation, successful synthetic-download negative seam, file chooser interception, proxy authentication and HTTP loopback refusal produced evidence. |
| r3, native host | EXIT 1, **INVALID** | An uncaught ECONNRESET from an owned fixture socket terminated Node before result/cleanup evidence could be written. Subsequent read-only inspection found no matching dedicated Chrome argv or open profile files and a closed proxy port; it cannot retrospectively prove every detached descendant exited. No unrelated Chrome process was signaled. |
| r4, native host | EXIT 0, **PASS_SYNTHETIC_SPIKE** | Dedicated-profile WebRTC preference applied; STUN UDP 0, WebTransport UDP 0, no UDP candidate. The WebTransport constructor existed in a secure synthetic HTTPS origin and its actual handshake failed. HTTP loopback trap 0. Authenticated proxy challenge, intercepted chooser, closed extra target, denied download with cancellation and successful download negative seam all passed. No TCP listeners on inventoried owned Chrome processes; owned browser process group and fixture servers exited. |

r4 script SHA-256: `96be1e313099593fd720b92420cd360a7c18565a6338091b2f12e1f1a8816249`.

The agent-facing navigation boundary refused `file:`, `data:` and `chrome:` before sending a navigation command. A real synthetic sentinel existed and its contents did not enter the page. **Raw private CDP itself is not a filesystem sandbox**; product page/frame observations, target handling and privacy gates remain necessary.

The successful script additionally used `--use-mock-keychain` for its disposable fixture. Shipping launch arguments deliberately exclude it: production dedicated-profile cookies must use the OS credential store. No OS managed browser policy was modified.

Actual local evidence is preserved outside the repository:

| Evidence | Location |
| --- | --- |
| Original seatbelt failure | `/tmp/dudu-local-browser-spike-r1.log` |
| Seatbelt cleanup regression | `/tmp/dudu-local-browser-spike-r2-seatbelt.log` |
| STUN red result | `/tmp/dudu-local-browser-spike-r2-native.log` |
| Invalid r3 harness run | `/tmp/dudu-local-browser-spike-r3-native.log` |
| r4 successful run | `/tmp/dudu-local-browser-spike-r4-native.log` |
| r4 disposable evidence directory | `/var/folders/n5/pzp1c08n27n9pyhr4zk8kspm0000gn/T/ddc-local-browser-spike-Iyflu6` |

Do not rerun into or mutate the successful evidence directory. Each run creates a new directory.

## Source verification

Common foundation r1 subsequently received **REQUEST_CHANGES** for two correctness/security gaps: attendance was bound to the original conversation rather than the trusted final called/own-bot session, and the Slack producer could treat its missing-user `unknown` log placeholder as a human sender. The four passing r1 tests did not cover those producer/routing paths and do not close these findings. The r2 repair first runs real reply-routing/turn-registry and actual Slack classification regressions red, then reruns them after the implementation fix. Until that succeeds and independent review approves r2, do not integrate or describe the common foundation as accepted.

The r2 RED run returned EXIT 101 (**1 passed, 2 failed**): the actual LINE called-session observation reached `#agent:sales` but lacked attendance, and a Slack message with no user acquired the placeholder's attendance. The corrected source requires a single exact host stamp after routing (source conversation, final session, employee and turn); an unrouted adapter scope grants no model proof. Final-session prefixes and rebinding are refused. Slack classifies the original typed U/W ID independently of logging/addressing placeholders. The focused GREEN run returned EXIT 0 (**9 passed, 0 failed**). Two future-size tests and the real fake-API two-employee own-bot smoke test also passed from that same compiled binary. Independent review subsequently approved this exact common-ingress r2 foundation. It is committed separately from the local-browser product.

- Initial gateway `check --no-default-features --tests`: EXIT 0, 4m38s. At that point only the initial policy, presence and proxy modules were connected.
- Expanded gateway `check --no-default-features --tests`: EXIT 0, 2m46s, covering CDP pipe, launch and native companion primitives.
- Core channel protocol focused tests: **2 passed**, EXIT 0. Home namespace hashing subsequently changed from lossy text to exact OS path bytes; that final small delta still needs a focused rerun.
- Initial session test compile: EXIT 101, a WebChat String was incorrectly treated as an Option. The type-only call-site correction preserved the gate.
- Session focused rerun: **138 passed, 0 failed, 10 ignored**, EXIT 0, 2m42s compile + 5.89s tests. It included two attendance-proof tests and twelve local-browser primitive tests. Ignored real-container cases were not executed.
- Final common-attendance focused run: **4 passed, 0 failed**, EXIT 0, 6m16s compile + 0.01s tests. This separately verified actual turn-registry success, cross-key refusal, normal scope completion and cancellation while a registration was deliberately retained. Production attendance code was unchanged from the earlier 138-pass snapshot; two meaningful tests were added.

Later product-source results retain their own boundaries:

- Backend focused r2: EXIT 101, **162 passed / 1 failed / 10 ignored**. The new fake-native-refusal test expected 403, whereas the existing `approval_denied` error is 409. Only that assertion was corrected.
- Backend focused r3: EXIT 0, **166 passed / 11 ignored**, including signed current-runtime routing, lifetime fencing, profile reset/backup exclusions and CDP writer revocation. The ignored native Chrome fixture was not executed.
- Frontend full r1: EXIT 0, **357 files / 2959 tests**. Focused settings/session/payload tests: **42 passed**. TypeScript and Vite production build also returned 0. These checks precede the later Rust-only start/consent changes.
- Tauri check r3 returned 0; its two pure tests passed without starting an app, accessing Keychain or launching Chrome. The subsequent native start dialog and bitmap indicator changes still require their own compiled result.
- Native-start authority primitive: actual production source compiled through an isolated dependency-light test crate. RED was **2 passed / 2 failed**; GREEN was **4 passed**, including exact expiry and clone invalidation when the host scope closes. This is not native GUI acceptance.
- Existing-page read-consent route: real signed HTTP with a synthetic backend returned **200 in all three forbidden cases** (recreated runtime connection, changed requester and owner A→B→A). The new regression returned EXIT 101. That original failure is retained in `/tmp/dudu-b9b-read-consent-route-red.log`.
- Gateway and CLI `check --no-default-features --tests --locked --offline` for the first native-start/consent source returned 0. The later actual route/stop-race tests and deadline recheck are awaiting focused execution; this typecheck is not their GREEN receipt.

All heavy Rust commands use the coordinator's two-slot wrapper, this worktree's independent CoW-seeded target, jobs=1, incremental=0, dev/test DEBUG=0. These are debug assertions with symbols removed, not release builds. No shared target or backup was cleaned.

### Current focused receipts (2026-10-24)

These later results supersede only the named source checks; the original failed runs above remain evidence.

- Signed HTTP read-consent, first-pending-owner epoch, native-start, profile lifecycle and caller scopes: gateway focused **186 passed, 0 failed, 11 ignored**, EXIT 0 (`/tmp/dudu-b9b-sessions-lifecycle-green-r1.log`). Ignored Chrome tests were not executed. The manual-login reaper regressions first returned **2 passed / 2 failed**: a fresh signed controller heartbeat lost a 121-second-old session, while a stale controller incorrectly survived recent AI activity. Fresh Local controller heartbeats now preserve the session only until its existing hard deadline; 30-second controller loss ends it. Physical presence and read/write consent remain separate gates. Container idle behavior is unchanged.
- Normal owned Local-window exit first failed its real manager regression by deleting the dedicated profile. The later 186-pass run verifies the host-only normal-close reason preserves eligible login state, while owner change, held/uncertain exits and explicit discard still reset it.
- **LB-S1** MCP `computer_key` persistence: the actual JSONL regression first returned **0 passed / 3 failed**. The repair returns **4 passed**, EXIT 0 (`/tmp/dudu-b9b-computer-key-privacy-green.log`), covering arguments, summary, success/error echo and the actual refusal writer, including legal Return and a malformed synthetic private marker. Only keypress metadata is retained. Independent final-delta review is still required.
- Final exact home-byte channel namespace/HMAC tests: **2 passed**, EXIT 0 (`/tmp/dudu-b9b-core-native-channel-r2.log`). Tauri pure native-channel/details/intent/indicator tests: **5 passed**, EXIT 0 (`/tmp/dudu-b9b-tauri-focused-r4.log`); no app, OS Keychain, browser or physical input was used.
- Latest frontend ACK disclosure/session/settings/payload checks: **42 passed**, EXIT 0; TypeScript and Vite returned 0 (`/tmp/dudu-b9b-frontend-focused-r3.log`, `/tmp/dudu-b9b-frontend-build-r3.log`). The disclosure separates this product's memory-only viewport/audit path from external-runtime transcripts, provider caches, model responses and Chrome profile state.
- **LB-S2 remains OPEN**: mask rectangles and raster capture happen in different render steps. The new ignored native probe uses a page-owned CSS A→B→A animation, a test seam that only waits, an in-RAM red-sensitive/blue-nonsensitive pixel calibration, and the actual backend screenshot path. A repeated rectangle comparison is not sufficient. The probe has not been executed, and needs independent frozen-source approval before any native run. No raw frame is written to its logs.
- The first restricted mask probe r1 subsequently ran from the exact approved 111-source snapshot and matching test binary `27020f99…`. It returned launcher EXIT 1 / test EXIT 101 **INVALID**, before browser start: the fixture passed a WebChat session to the messaging-adapter-only attendance producer, so its final-route assertion failed. No browser inventory or owned machine-lease file was created, and cleanup was correctly not reported as PASS. The original receipt is `/tmp/dudu-b9b-mask-probe-r1-native.exit.json`; its owned root is `/private/tmp/ddc-b9b-native-FwUMgx`. Do not reuse that root. This is not the masking RED. Probe r2 uses the authenticated WebChat fixture producer, adds a browser-free producer regression, and brackets the actual animation start with host monotonic timestamps; capture must lie wholly in the B plateau and pixel calibration must prove red in B and absent from A. It needs its own frozen-source approval and matching binary before execution.
- Per-agent primary-owner generation is a separate pending cross-branch dependency. Merely adding its read API does not close the concurrent reset/reset→new permit→B→A race; a same-transaction counter bump, one-snapshot owner fingerprint and actual overlap regression are required before acceptance.

## Primary-source findings

- [Chromium pipe descriptor constants](https://chromium.googlesource.com/chromium/src/+/HEAD/components/devtools/devtools_pipe/devtools_pipe.h): inherited read/write descriptors 3/4. There is no TCP fallback in the new transport.
- [Chrome default-profile debugging restriction](https://developer.chrome.com/blog/remote-debugging-port): a dedicated user-data directory is necessary; product validation also rejects normal browser data paths and symlinks.
- [Chromium macOS developer test flags](https://chromium.googlesource.com/chromium/src/+/HEAD/docs/mac_build_instructions.md): mock keychain is a test-only behavior change.
- [Crash reporter environment override](https://chromium.googlesource.com/chromium/src/+/HEAD/chrome/app/chrome_crash_reporter_client.cc): fixture crash output is redirected with `BREAKPAD_DUMP_LOCATION`.
- [WebRTC preference names](https://chromium.googlesource.com/chromium/src/+/HEAD/chrome/common/pref_names.h), [renderer preference application](https://chromium.googlesource.com/chromium/src/+/HEAD/chrome/browser/renderer_preferences_util.cc) and [handling enum conversion](https://chromium.googlesource.com/chromium/src/+/HEAD/third_party/blink/common/peerconnection/webrtc_ip_handling_policy.cc): production must apply the dedicated-profile preference, including clearing broader per-origin overrides. The observed flag-only failure remains a regression case.

## Work remaining

1. Complete final-source runtime validation and independent review of native start permits, per-call read consent, all event/page guards and cleanup. The standalone OS actor remains unverified platform identity; it never seeds chat authority.
2. Obtain independent approval of the frozen product-backend native launcher before executing it. Validate actual pipe/profile/proxy/UDP/download/upload/masking behavior using fresh synthetic state. The older spike is not a substitute.
3. Validate signed app bootstrap, OS-keychain storage, actual native modal/button/indicator/stop/presence behavior separately. No real user credentials, provider account, normal browser profile or physical screen lock has been used in these tests.
4. Integrate the team-view snapshot route and primary-owner mutation hook with server-side local-target refusal and epoch invalidation. No dashboard account may receive local page data; status and stop remain available.
5. Complete final appropriate full Rust checks and independent code/security review, then open a candidate PR. Physical screen lock, a person signing in to a provider, supported-platform shipping and owner release approval remain distinct gates.
