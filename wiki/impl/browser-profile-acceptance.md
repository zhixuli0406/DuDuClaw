# Browser login snapshot acceptance

Status: PARTIAL, unreleased. This branch implements B9a login persistence only.
Team oversight, conversation links, live-view acceptance and the local headed
browser remain separate required work.

## Verified locally

- Gateway and CLI `cargo check --tests --no-default-features` completed.
- Archive helper: six Python unit tests passed, including path/link/special-file
  rejection and byte/file limits. Python compilation and entrypoint shell syntax
  checks passed.
- Dashboard initial focused run: TypeScript and 60 tests passed. Two later
  explicit held-session keep-login UI tests have been added and await execution.
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

## Remaining gates

- Run final profile/session/config/RPC Rust tests and full relevant suites.
- Run the final UI tests/build and isolated native gateway/dashboard journey.
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
