# Browser login snapshot acceptance

Status: PARTIAL, unreleased. This branch implements B9a login persistence only.
Team oversight, conversation links, live-view acceptance and the local headed
browser remain separate required work.

## Verified locally

- Gateway and CLI `cargo check --tests --no-default-features` completed.
- Archive helper: six Python unit tests passed, including path/link/special-file
  rejection and byte/file limits. Python compilation and entrypoint shell syntax
  checks passed.
- Final dashboard focused run: TypeScript, seven files / 62 tests and the
  production build passed, including both held-session keep-login decisions.
- The integrated core, CLI and gateway run after merging main `1cc573e5`
  completed with exit 0: 12,799 tests passed across 39 unit, integration and
  documentation suites (59 ignored). Gateway library: 9,305 passed, 47 ignored,
  32 filtered; CLI: 2,236 passed, four ignored; core: 1,044 passed, one ignored.
  This run preceded the stdin EOF correction below. Both builds used the CI
  no-default-feature configuration, debug assertions and debug info level zero.
- The actual Dockerfile built on Docker Desktop macOS arm64. With the production
  read-only root, private tmpfs, no-new-privileges, 512 MiB, one CPU and no network,
  a synthetic persistent Cookie survived export and a fresh Chromium container.
  Export took about 1.49 seconds. Synthetic Local Storage/IndexedDB files crossed
  the archive boundary; this does not prove real database/application semantics.
  Browser-started late imports and sandbox access to root markers were refused.
- The first native probe exposed a real loss of cookie data when all Chromium
  processes were signalled. It is retained as a failed run. The helper now requests
  `Browser.close`, waits for the browser and its restart loop to quiesce, and refuses
  to export if a graceful close cannot finish. The corrected fresh-container run
  passed. No forced-kill snapshot is accepted.
- Independent static review closed atomic pointer/previous-generation retention,
  owner suspension cycles, ten-second save budget and unsafe end-state findings.
  The graceful-close correction was independently approved as well.
- The real gateway-manager test initially failed because profile settings were
  absent from the strict image-config vocabulary. Its next run exposed a second
  defect: Tokio's Unix child-stdin shutdown does not close the pipe. The importer
  waited for EOF while its parent awaited output. Both failures remain recorded.
  The corrected code owns and explicitly drops stdin; it was independently
  approved. The rebuilt test binary then passed the actual gateway start, Cookie
  seed, encrypted save, fresh-container restore and reset flow in 17.32 seconds.
  The encrypted generation was 30,929 bytes and contained no plaintext synthetic
  Cookie. Only the fresh fixture's labelled containers were removed; the existing
  production-container fingerprint was unchanged.
- On that rebuilt revision `c139daba`, all nine profile tests, four session-policy
  tests and the strict image-config regression passed. The native test used image
  `sha256:75335e96884213916b7e523b2ddee0466036604501aadde0a56bb099005df48f`
  on Docker Desktop macOS arm64. Test-binary SHA-256:
  `e096e7c4ef9974b11adfc869b4c665ed7e8b069cf1da0992189c7a341febbbd1`.
- The final shipping CLI (SHA-256
  `5ef7b834486a174a458f2d2dab1896f4853b6e903866f8e13aad638d0883e15e`)
  and the actual production dashboard build completed an isolated Chrome journey:
  normal Stop displayed saved state; a fresh start returned restored and actual
  CDP Cookie inspection confirmed the synthetic value; dashboard Clear stopped
  the running container and removed saved state. The 390px layout had no horizontal
  overflow. The gateway, preview process and owned containers were stopped.
- This dashboard journey also found an independent existing VNC defect: x11vnc's
  IP allowlist rejected its Unix peer named localhost. The live picture succeeded
  with the separate [VNC fix #207](https://github.com/zhixuli0406/DuDuClaw/pull/207)
  overlaid on the local profile image. Chrome displayed a synthetic coloured page
  and viewer count one. This is a combined local candidate, not a published image
  or a claim that the original VNC helper passed.
- The manually prepared GUI fixture initially lacked a machine encryption key.
  Saving correctly failed closed. After initializing an owner-only random mock
  machine key in that isolated home, the save/restore/clear journey passed. No
  real OS keychain, website account or fresh-install key provisioning was tested.

## Remaining gates

- Complete final CI and integrate the separately reviewed VNC helper correction.
- Verify real website account sign-in separately; no personal account has been
  used in the fixture. No broad claim about website compatibility is supported.
- CI on the final revision, Linux/amd64/Windows boundaries, merge and user
  acceptance are separate gates. This branch does not publish an image or release.
- When integrating personal Google identity, each primary-owner transition must
  call the profile reset hook, including A-to-B-to-A changes. The final owner
  fingerprint alone cannot detect that sequence.

Snapshots are encrypted with AES-256-GCM and context-bound to the canonical home,
employee instance and profile id. The metadata file is an atomic generation
pointer. RED, emergency stops, reset, capability loss and unapproved held-session
endings do not export the current state. A late stop/threat/deadline/ownership
change is checked again immediately before the pointer is committed.
