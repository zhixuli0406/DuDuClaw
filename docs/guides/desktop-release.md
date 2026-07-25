# Desktop app — release, signing & auto-update

Covers the developer-credential work (TODO §D4). The pipeline is
[`.github/workflows/desktop-release.yml`](../../.github/workflows/desktop-release.yml);
trigger it by pushing a `desktop-v*` tag.

## 1. Updater signing keys (one-time)

```bash
cargo tauri signer generate -w ~/.tauri/duduclaw.key
```

- Put the **public** key in `src-tauri/tauri.conf.json > plugins.updater.pubkey`.
- Put the **private** key + password in repo secrets
  `TAURI_SIGNING_PRIVATE_KEY` / `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`.

The updater endpoint is the GitHub `latest.json` (already configured). On each
release `tauri-action` emits a signed `latest.json`; the client verifies the
signature before installing, and rejects a mismatch (§D4.4).

## 2. macOS — Developer ID + notarization

Secrets:

| Secret | What |
| --- | --- |
| `APPLE_CERTIFICATE` | base64 of the Developer ID Application `.p12` |
| `APPLE_CERTIFICATE_PASSWORD` | its password |
| `APPLE_SIGNING_IDENTITY` | `Developer ID Application: <Name> (<TEAMID>)` |
| `APPLE_ID` / `APPLE_PASSWORD` | Apple account + app-specific password |
| `APPLE_TEAM_ID` | 10-char team id |

`tauri-action` signs (hardened runtime, using `src-tauri/entitlements.plist`) and
notarizes when these are present. For a manual artifact use
[`scripts/desktop/sign-notarize-macos.sh`](../../scripts/desktop/sign-notarize-macos.sh).

**Acceptance (§D4.1):** download the `.dmg` on a machine that never saw the cert
→ it opens with no "unidentified developer" Gatekeeper prompt.

## 3. Windows — Authenticode

Secrets `WINDOWS_CERT_PFX_BASE64` / `WINDOWS_CERT_PASSWORD`. The workflow signs
the `.msi` via [`scripts/desktop/sign-windows.ps1`](../../scripts/desktop/sign-windows.ps1).
**Acceptance (§D4.2):** SmartScreen does not block (EV = immediate trust).

## 4. Linux

`.AppImage` + `.deb` are built unsigned (WebKitGTK runtime dep declared). Not a
release blocker (§D4.3).

## 5. Cut a release

The normal path is the core release ritual — `scripts/release.sh <bump>` bumps
`src-tauri/tauri.conf.json` with every other manifest and tags BOTH `v<X.Y.Z>`
and `desktop-v<X.Y.Z>` on the bump commit; `git push --tags` then triggers this
pipeline alongside the core one. (Desktop sat frozen at 1.33.0 for 11 releases
while these were separate manual steps — don't go back to hand-tagging.)

For a desktop-only rebuild of an existing version:

```bash
git tag desktop-v1.44.0 <bump-commit>
git push origin desktop-v1.44.0
```

The workflow builds the 4-target matrix, signs/notarizes, and **publishes the
release directly** (no draft) with installers + `latest.json`. A final job
copies `latest.json` to the fixed-tag `desktop-updater` release — that fixed
URL is the updater endpoint, because `releases/latest/...` would point at the
much more frequent core releases and 404.

## 6. Certificate hygiene

- Never commit certs/keys; only GitHub secrets.
- Rotate the Apple app-specific password and the updater key on personnel change.
- Keep `tauri.conf.json` version == core `Cargo.toml` workspace version so the
  updater never ships a shell/core mismatch (§D4.4 / §D4.5).
