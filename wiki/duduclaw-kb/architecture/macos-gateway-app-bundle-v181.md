# macOS DuDuClaw Gateway.app (RM-155 item 3, v1.81.0)

Status: design, 2026-11-08. Owner decisions: D1=B (Developer ID signing + app bundle), D2 FDA optional in docs,
D3 CE/Pro share identifier `ai.duduclaw.gateway`, D4 docs recommend install.sh on macOS.

Root cause: CE/Pro macOS binaries are ad-hoc linker-signed (identifier `duduclaw-<hash>`, DR = cdhash); the
LaunchAgent (autostart.rs:68-110) runs the bare binary, so TCC treats every update as a new program.

## Key constraints (code)
- Old updaters match any `duduclaw` file in a tarball (`updater.rs:1437-1439`) ⇒ bundle ships as a separate asset
  `duduclaw[-pro]-darwin-<arch>.app.zip`; the bare tarball stays (now Developer ID-signed) for old updaters/npm.
- `.app/Contents/` paths are classified Desktop (`updater.rs:462`) ⇒ detect by bundle id first.
- `current_exe()` keeps symlink paths on macOS (`platform.rs:659-673`, `lib.rs:434-466`) ⇒ canonicalize.
- `.mcp.json` command self-heals (`mcp_template.rs` ~334, 421-446, 506); hook commands already quoted.
- ONNX Runtime load-dynamic ⇒ hardened runtime needs `disable-library-validation`; no JIT in the tree.

## Design
- Bundle: `Contents/{Info.plist, MacOS/duduclaw|duduclaw-pro, _CodeSignature, Resources/AppIcon.icns}`;
  Info.plist via plistlib: CFBundleIdentifier ai.duduclaw.gateway, LSUIElement, LSMinimumSystemVersion 11.0,
  NS*FolderUsageDescription, NSAppleEventsUsageDescription, NSLocalNetworkUsageDescription.
- Entitlements `packaging/macos/gateway.entitlements`: disable-library-validation, automation.apple-events.
- Sign `codesign --force --options runtime --timestamp --identifier ai.duduclaw.gateway --entitlements … --sign`;
  pinned DR const (identifier + Developer ID + team 7469HYQ6HH); `codesign -R="=<DR>"`.
- Notarize .app + bare binary in one ditto zip via notarytool; staple the .app.
- install.sh → `~/Applications/DuDuClaw Gateway.app` (DUDUCLAW_APP_DIR override); CLI shim = symlink
  `~/.duduclaw/bin/duduclaw` → bundle exe. npm: bundle inside platform package, bin wrapper prefers it.
- LaunchAgent: canonical executable path, `AssociatedBundleIdentifiers`, boot migration of old plists.
- Updater (`updater/macos_bundle.rs`): bundle-id detection, `.app.zip` asset, strict zip extractor (no symlinks,
  no `..`, one exe), verify release marker + version + pinned DR + old DR + Info.plist id/version + edition guard,
  atomic `renamex_np(RENAME_SWAP)` with fallback and rollback, post-swap verify, cleanup of staging dirs,
  restart with installed_exe.
- Pro: release.sh builds/signs/notarizes with local keychain profile; private update provider `kind:"app-bundle"`.
- Linux/Windows/Desktop unaffected (desktop id ai.duduclaw.desktop stays Desktop).
- CI (release.yml macOS rows): import certs, build bundle, sign, verification gate (DR, runtime flag,
  spctl Notarized Developer ID, stapler validate, version).

## Tasks
T1 bundle builder+entitlements · T2 sign/notarize/verify scripts · T3 release-asset tooling · T4 CI wiring ·
T5 core constants/paths · T6 LaunchAgent · T7a detection/asset · T7b strict zip extractor · T7c codesign gate ·
T7d swap/rollback/cleanup · T8 dispatch+migration+restart · T9 install.sh (also make sha256 mismatch fatal) ·
T10 npm · T11 Pro public half · T11p Pro private half · T12 docs/CHANGELOG/doctor probe.

## Real-Mac acceptance
Fresh install + Login Items name; TCC grant persists across RC update (TCC.db client ai.duduclaw.gateway);
FDA via picker; bare→bundle migration prompts once; npm pack round trip; hardened runtime with ORT/osascript/
computer use/Python; Pro over CE keeps grants; Desktop+Gateway coexist; interrupted swap is atomic;
quarantined .app.zip opens without Gatekeeper prompt.
