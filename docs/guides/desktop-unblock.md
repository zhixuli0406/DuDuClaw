# Desktop app unblocking guide (Phase D + Phase 6 manual testing)

> Corresponds to the items marked `[ ]` (blocked) and `[~]` (written but not build-verified) in
> [TODO-genspark-workspace-shell.md](../todo/TODO-genspark-workspace-shell.md). What blocks these items
> is external resources — toolchain, credentials, a graphical environment, a second machine. Not missing code.
> This guide breaks every blocker into "why it's blocked → prerequisites → steps → matching TODO acceptance criteria."
>
> **Suggested order**: Gate A (local, free, half a day) → Gate E (update signing key, free) → Gate B (macOS signing, needs a paid account)
> → Gate C (Windows signing) → Gate D (Linux). Once A is done, you can use the app yourself and run every lifecycle check.

---

## Gate A — install the Tauri toolchain and get it running locally (no signing)

**Blocks**: D0 🧪, D1 🧪, D2.1/D2.3/D2.4/D2.5/D2.6 🧪, D5 item 1, P6.3 manual test + screenshots.
**Why it's blocked**: this writing environment has no Tauri CLI, no system WebView dev dependencies, and no display. Your Mac has all three.
**Cost**: free. **Time**: about 0.5–1 hour (including the first build).

### A.1 Prerequisites (macOS)
```bash
# Xcode Command Line Tools (if not installed)
xcode-select --install

# Rust (if not installed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Tauri CLI v2
cargo install tauri-cli --version "^2" --locked
cargo tauri --version   # should print tauri-cli 2.x
```
> Windows additionally needs the WebView2 Runtime (built into Win11) + MSVC Build Tools;
> Linux needs `libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf`.

### A.2 Generate the app icon (one-time)
```bash
cd src-tauri
# Prepare a square PNG ≥1024×1024 (🐾 on an amber background), e.g. save it to web/public/paw-1024.png
cargo tauri icon ../web/public/paw-1024.png
# Produces icons/32x32.png, 128x128.png, 128x128@2x.png, icon.icns, icon.ico
```

### A.3 Stage the sidecar + dev mode
```bash
# From the repo root
cargo build --release -p duduclaw-cli --bin duduclaw   # produces target/release/duduclaw
scripts/desktop/stage-sidecar.sh                        # copies it to src-tauri/binaries/duduclaw-<triple>

cd src-tauri
cargo tauri dev      # opens the dev window; beforeDevCommand starts Vite automatically
```

### A.4 Unsigned production build (verify locally)
```bash
cd src-tauri
cargo tauri build
# Output: src-tauri/target/release/bundle/{macos,dmg}/...
# Opening the .app directly for the first time gets blocked by Gatekeeper (expected — it isn't signed yet); allow it locally with:
xattr -dr com.apple.quarantine "target/release/bundle/macos/DuDuClaw.app"
open "target/release/bundle/macos/DuDuClaw.app"
```

### A.5 Item-by-item acceptance (matches the TODO)
| TODO item | How to verify |
| --- | --- |
| **D0 🧪** | `cargo tauri dev` launches, the window shows the login page, sending one chat message gets a response |
| **D1 🧪** | (a) First make sure no launchd/CLI gateway is running → open the app; it should **auto-start the sidecar**. (b) First run `duduclaw run` to occupy port 18789 → open the app; it should **attach without restarting it** (Activity Monitor shows only one `duduclaw` process) |
| **D2.1 🧪** | Open the app twice in a row → only one window gets focus, only one `duduclaw` process exists |
| **D2.3 🧪** | After a normal quit, `ps aux | grep duduclaw` shows no leftover process; after `kill -9`-ing the app and reopening it, it should reclaim the orphan pointed to by the old pidfile (`~/.duduclaw/desktop-sidecar.pid`) |
| **D2.4 🧪** | The tray icon shows status; the Start/Stop menu items drive the sidecar; closing the window minimizes to the tray instead of quitting |
| **D2.5 🧪** | Manually `kill <sidecar pid>` → the app should auto-restart with exponential backoff; after 5+ consecutive kills it should enter an error state and notify, not retry forever |
| **D2.6 🧪** | Launch the app from **Finder/Dock** (not the terminal) → confirm the child processes (Claude CLI, etc.) are still discoverable; you can trigger a chat action that needs the CLI to verify |
| **D5 item 1** | `cargo tauri build` produces a runnable app that auto-starts the sidecar, opens the workspace, and can send chat messages |
| **P6.3 manual test** | On first launch in the personal edition, land on the workspace → send one chat message → switch to "Advanced" to see the full dashboard → the mode choice persists after a refresh |
| **P6.3 screenshots** | Capture one light and one dark screenshot, and put them side by side with Genspark 4.0 for critique |

---

## Gate E — generate the Tauri auto-update signing key (free, do this first)

**Blocks**: D4.4 (replacing the update pubkey placeholder).
**Why it's blocked**: `tauri.conf.json > plugins.updater.pubkey` is currently the placeholder `REPLACE_WITH_...`, and the updater needs a real key pair before it can verify signatures. Until the key is ready, the updater is **fully disabled** (`plugins.updater.active = false` and `bundle.createUpdaterArtifacts = false`); otherwise a local `cargo tauri build` fails at the final updater-artifact signing step with `A public key has been found, but no private key`.

### Steps
```bash
cargo tauri signer generate -w ~/.tauri/duduclaw-updater.key
# The terminal prints the public key and writes the private key to ~/.tauri/duduclaw-updater.key
```
1. Paste the **public key** into `plugins.updater.pubkey` in [src-tauri/tauri.conf.json](../../src-tauri/tauri.conf.json).
2. **In the same file, turn the updater back on**: `plugins.updater.active = true`, `bundle.createUpdaterArtifacts = true`.
3. Set the **private key content** and its password as GitHub repo secrets:
   - `TAURI_SIGNING_PRIVATE_KEY` (the private key file's content)
   - `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`
4. **The private key never goes into the repo.** Losing it means published clients can no longer receive updates, so back it up to a password manager.

**Acceptance (half of D4.4)**: after a CI release, the artifacts include `latest.json` with signature fields. The full "old version → update" round trip needs two already-signed releases (do that after Gate B).

---

## Gate B — Apple Developer ID signing + notarization (macOS release)

**Blocks**: D3.1 🧪, D3.2 🧪, D4.1, D4.4 (Mac end-to-end), D5 signing/clean machine.

> **Current status (verified against Keychain, 2026-07)**: **signing is unblocked.** This machine has a valid
> `Developer ID Application: Dudu Technology Ltd. (7469HYQ6HH)` certificate (expires 2031-03, private key in
> Keychain, `codesign` verified working), and it's already set in
> [tauri.conf.json](../../src-tauri/tauri.conf.json) under `bundle.macOS.signingIdentity`,
> so `cargo tauri build` signs automatically (no env vars needed). **All that's left is notarization**: create
> an app-specific password (B.1 step 4) and pass `APPLE_ID` / `APPLE_PASSWORD` /
> `APPLE_TEAM_ID=7469HYQ6HH`, then verify D4.1 / D5 on a second, clean machine.

### B.1 Get the certificate and authentication details
1. ✅ Already have an Apple Developer Program account + Developer ID certificate (Team ID `7469HYQ6HH`).
2. ✅ The **Developer ID Application** certificate is already in Keychain and valid (see "Current status" above).
3. (For CI) Export it as a `.p12` (including the private key) and note the password.
4. ⬜ Create an **app-specific password**: appleid.apple.com → Sign-In and Security → App-Specific Passwords. (The only remaining step for notarization.)
5. ✅ **Team ID** = `7469HYQ6HH`.

### B.2 Sign and notarize locally (verify once by hand)
```bash
# signingIdentity is already set in tauri.conf.json, so the build signs automatically. Pass these three env vars for notarization:
export APPLE_ID="you@example.com"
export APPLE_PASSWORD="<app-specific-password>"   # B.1 step 4
export APPLE_TEAM_ID="7469HYQ6HH"
cd src-tauri && cargo tauri build          # signs + notarizes + staples (when the env vars are set)
# Or build first, then sign, notarize, and staple separately with the bundled script:
../scripts/desktop/sign-notarize-macos.sh "target/release/bundle/dmg/DuDuClaw_1.31.0_aarch64.dmg"
```
> The script uses the hardened runtime entitlements from [src-tauri/entitlements.plist](../../src-tauri/entitlements.plist).

### B.3 Set as CI secrets (for automated releases)
In the GitHub repo, go to Settings → Secrets and variables → Actions and add:
| Secret | Value |
| --- | --- |
| `APPLE_CERTIFICATE` | `base64 -i DeveloperID.p12` (the full output) |
| `APPLE_CERTIFICATE_PASSWORD` | the .p12 password |
| `APPLE_SIGNING_IDENTITY` | `Developer ID Application: <name> (<TEAMID>)` |
| `APPLE_ID` / `APPLE_PASSWORD` / `APPLE_TEAM_ID` | same as above |

### B.4 Item-by-item acceptance
| TODO item | How to verify | Status |
| --- | --- | --- |
| **D4.1 🧪** | Copy the signed and notarized `.dmg` to **another Mac that has never had your certificate installed**, then double-click it → it should **not** show "from an unidentified developer" (來自未識別開發者) | ✅ **Verified** (2026-07-01, `desktop-v1.31.0`): `stapler validate` = *worked*, `spctl -a` = accepted / Notarized Developer ID |
| **D3.1 🧪** | After signing with the hardened runtime, open the app and confirm the sidecar can still spawn the CLI / reach the network (trigger a chat action that needs the network) | ⬜ Still needs a real run inside the signed app |
| **D3.2 🧪** | The first time Computer Use runs, the system should show the Accessibility / Screen Recording permission prompt; after granting it, screenshots/simulated input should work | ⬜ Not yet verified |
| **D5 signing/clean machine** | Same as D4.1, and `spctl -a -vvv DuDuClaw.app` should return `accepted` | ✅ **Verified**: `spctl -a -vvv` = `accepted, source=Notarized Developer ID` |

---

## Gate C — Windows Authenticode signing

**Blocks**: D4.2.
**Why it's blocked**: it needs an **Authenticode code-signing certificate**.

> ⚠️ **Major change starting 2023/6**: the CA/B Forum now requires even OV (standard) certificates to be stored on FIPS-compliant hardware
> (a USB token or a cloud HSM) — you can no longer download a plain `.pfx` and drop it into CI. Automated signing therefore needs a cloud signing service;
> the plain-`.pfx` path only still works for old inventory certificates or temporary certificates exported from a cloud HSM.

### Where to buy one (cheapest to priciest)
| Option | Type | Price (approx.) | CI auto-sign | Best for |
| --- | --- | --- | --- | --- |
| **Azure Trusted Signing** | OV (Microsoft's own) | **~US$9.99/month** | ✅ native `signtool` dlib | **First choice**: cheapest, best SmartScreen reputation; requires identity verification |
| **Certum open-source code signing** | OV (open-source only) | **~US$30–70/year** | ✅ SimplySign cloud | DuDuClaw is Apache-2.0 → **qualifies**, budget pick |
| **SSL.com eSigner** | OV / EV | OV from ~US$249/year | ✅ eSigner cloud API | Established, well-documented |
| **DigiCert KeyLocker** | OV / EV | On the higher end | ✅ KeyLocker | Enterprise-grade |
| **Sectigo/Comodo** (resellers: The SSL Store, SignMyCode, Codegic) | OV / EV | OV ~US$200–400/year | Depends on the plan | Resellers often discount |

**OV vs. EV**: OV is cheaper, but its SmartScreen reputation needs **accumulated download volume** before warnings taper off; EV is pricier but clears SmartScreen right away.
You can buy either online by card from Taiwan; the process includes identity/organization verification.

### Route 1 (recommended) — Azure Trusted Signing (cloud, ~US$10/month)
1. In the Azure portal, create a **Trusted Signing account** + **Certificate Profile** and complete identity verification.
2. Create a service principal and set these CI secrets: `AZURE_TENANT_ID`, `AZURE_CLIENT_ID`, `AZURE_CLIENT_SECRET`,
   `AZURE_TS_ENDPOINT`, `AZURE_TS_ACCOUNT`, `AZURE_TS_PROFILE`.
3. Sign in CI using the official action (replacing the plain-`.pfx` step):
   ```yaml
   - name: Azure Trusted Signing
     if: matrix.os == 'windows-latest'
     uses: azure/trusted-signing-action@v0
     with:
       azure-tenant-id: ${{ secrets.AZURE_TENANT_ID }}
       azure-client-id: ${{ secrets.AZURE_CLIENT_ID }}
       azure-client-secret: ${{ secrets.AZURE_CLIENT_SECRET }}
       endpoint: ${{ secrets.AZURE_TS_ENDPOINT }}
       trusted-signing-account-name: ${{ secrets.AZURE_TS_ACCOUNT }}
       certificate-profile-name: ${{ secrets.AZURE_TS_PROFILE }}
       files-folder: src-tauri/target
       files-folder-filter: msi,exe
       file-digest: SHA256
       timestamp-rfc3161: http://timestamp.acs.microsoft.com
       timestamp-digest: SHA256
   ```

> ⚠️ **Regional restriction**: Azure Trusted Signing is currently open only to organizations in the **US/Canada/EU/UK**, and to
> **individual developers in the US/Canada**. **Taiwan/Macau and similar regions don't qualify**: you can fill out the form and create the resources, but
> identity validation gets stuck, so it's wasted effort. If you're outside those regions, use **Route 2 (Certum)** instead.

### Route 2 — Certum open-source certificate (cloud SimplySign, **no regional restriction, works for Taiwan/Macau**)
1. Go to [shop.certum.eu](https://shop.certum.eu/) and search for "Open Source Code Signing," then buy **"Open Source Code
   Signing in the Cloud"** (the cloud edition, about €49). The three editions differ like this:
   - *code* (€25): certificate only; you **supply your own** Certum crypto card and reader, so it doesn't fit here.
   - *set* (€69): includes a physical card and reader; needs international shipping and CI can't automate it, so it doesn't fit either.
   - **in the Cloud (€49): the certificate lives in the cloud, no hardware needed — pick this one.**
2. Complete individual identity verification (accepts international applicants, requires uploading ID), and attach the DuDuClaw GitHub link to prove it's open source.
3. Install **SimplySign** (maps the cloud certificate to a locally usable signing device), or use its CLI.
4. Signing tools:
   - Windows: `signtool` connected to SimplySign (PKCS#11 / CSP).
   - **Mac/Linux/CI**: use **`osslsigncode`** with the SimplySign cloud key — you can sign `.msi` files without ever opening Windows.

> 💳 **Payment note (verified 2026-06)**: Certum's payment processor (Autopay, EU) **only accepts Visa/Mastercard**,
> **not JCB**; cross-border Apple Pay often fails with "service unavailable" (無法取得服務). If you only have a JCB card:
> ① try PayPal (it usually accepts JCB); ② get a **Wise/Revolut virtual Visa** (available in Taiwan/Macau, and useful later
> for the Apple Developer $99 fee and various SaaS subscriptions — strongly recommended); ③ ask someone with a Visa/Mastercard to pay on your behalf.

### Route 3 (fallback) — plain .pfx (only for old inventory certificates / temporary certificates exported from an HSM)
Use the existing [sign-windows.ps1](../../scripts/desktop/sign-windows.ps1) script: set the secrets
`WINDOWS_CERT_PFX_BASE64` and `WINDOWS_CERT_PASSWORD`, then run it locally by hand:
```powershell
pwsh scripts/desktop/sign-windows.ps1 -Artifact path\to\DuDuClaw_1.30.1_x64.msi
```

> **Recommendation**: in the **US/Canada/EU/UK**, use Route 1 (Azure, ~$10/month, friendliest to SmartScreen);
> in **Taiwan/Macau and other regions**, use Route 2 (Certum Cloud, €49) — the only option with no regional restriction that still supports CI.

### ⏭️ This gate can wait (priority note)
**Windows signing is the lowest-priority, most deferrable item in all of Phase D.** Don't let it block the project:
- An unsigned Windows installer **still installs fine** — SmartScreen just shows an "Unknown publisher" (未知發行者) warning once, and the user
  clicks "Run anyway" (仍要執行) to proceed.
- If you're developing on macOS and the audience skews Mac/Taiwan, **do Gate A (get it running locally) + Gate B (macOS signing) first**;
  Windows can **ship unsigned initially**, and get signed later once you have a Wise/Revolut card or actual Windows users need it.
- In CI: when the repo variable `WINDOWS_SIGN_METHOD` isn't set, the signing step **auto-skips** (see
  [desktop-release.yml](../../.github/workflows/desktop-release.yml)), so it doesn't block releases on other platforms.

**Acceptance (D4.2 🧪)**: download the signed `.msi` on a clean Windows machine — SmartScreen should **not** block it (OV needs accumulated reputation; EV/Azure clears faster).

---

## Gate D — Linux packaging verification

**Blocks**: D4.3 🧪.
**Why it's blocked**: it needs a Linux environment/VM to test the `.AppImage`/`.deb`. No signing required.

### Steps
```bash
# On Ubuntu 22.04 (or an already-configured CI runner)
sudo apt-get install -y libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf
cd src-tauri && cargo tauri build
# Output: target/release/bundle/{appimage,deb}/...
```
**Acceptance**: run the `.AppImage` once each on Ubuntu and Fedora — the app should launch and connect to the gateway.

---

## Gate F — end-to-end auto-update (needs B + E done)

**Blocks**: D4.4 🧪, D5 auto-update.

### Steps
1. Confirm Gate E's pubkey is filled in and the private key is in the secrets.
2. Publish the first version: `git tag desktop-v1.30.1 && git push origin desktop-v1.30.1` (CI produces the release + `latest.json`).
3. Install that version on a test machine.
4. Bump the version in `src-tauri/tauri.conf.json` to `1.30.2` and publish a second tag.
5. Open the old app → it should detect the new version → verify the signature → download it → prompt for a restart → apply it.
6. **Negative test**: sign a fake update with the wrong key → the client should **refuse to install it** (signature verification fails).

---

## One-time checklist (clears everything)
- [ ] Gate A: `cargo tauri build` produces the app locally, all 7 lifecycle checks pass (D0/D1/D2.*/D5-1/P6.3)
- [ ] Gate E: updater key generated, pubkey filled in, private key in secrets (half of D4.4)
- [ ] Gate B: Apple certificate → signed and notarized, unblocked on a clean Mac (D3.1/D3.2/D4.1/D5)
- [ ] Gate C: Windows certificate → signed, SmartScreen doesn't block it (D4.2)
- [ ] Gate D: Linux `.AppImage`/`.deb` runs (D4.3)
- [ ] Gate F: auto-update between two versions succeeds, and signature verification rejects a mismatch (D4.4/D5)

> Once everything is done, flip the matching TODO items from `[ ]`/`[~]` to `[x]`.
