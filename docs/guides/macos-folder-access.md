# macOS folder access after an update

Languages: English · [繁體中文](zh-TW/macos-folder-access.md) · [日本語](ja-JP/macos-folder-access.md)

macOS protects some folders with a privacy consent ("Files and Folders"):
`~/Desktop`, `~/Documents`, `~/Downloads`, iCloud Drive
(`~/Library/Mobile Documents`), cloud-storage folders
(`~/Library/CloudStorage`), external and network volumes (`/Volumes/…`).
An app has to be allowed once before it can read them.

## What you will notice

After DuDuClaw updates, macOS may ask again. If nobody is at the Mac to click
**Allow**:

- AI employees cannot read files in those folders. A read either fails at
  once, or — while the macOS prompt is on screen — waits until it times out.
- The dashboard shows a banner to admins listing the affected folders.
- Admins with a linked chat channel get **one** message when the state changes
  (waiting → refused → restored). The same state is never sent twice, and the
  message waits for the end of quiet hours.
- The dashboard health check shows a `macOS folder access` row.
- File tools answer with an explicit message instead of a generic failure, and
  a chat reply that times out while macOS is waiting says so.

DuDuClaw checks the folders it actually uses: `DUDUCLAW_HOME`, every entry in
`[files] allowed_roots`, and work folders of delegated tasks. It checks at
start-up (the restart after an update), right after you save
`[files] allowed_roots` in the dashboard, and every 10 minutes while access is
not granted. There is no way to ask macOS for the current state without
triggering the question, so the check performs a real read of each folder.

## Why updates ask again

The LaunchAgent runs the gateway program directly, so macOS treats the gateway
as the program asking for access — for itself and for every AI tool it starts.
Today that program is a command-line binary without a stable app signature, so
each update looks like a new program to macOS, and macOS asks once per update.

When the signed DuDuClaw app build ships, the identity stays the same across
updates and macOS remembers your answer: you allow once and are not asked
again. Until then, expect one prompt per update.

## How to re-grant access

1. On the Mac running DuDuClaw, click **Allow** on the prompt.
2. If you missed the prompt or clicked **Don't Allow**: open
   **System Settings › Privacy & Security › Files and Folders**, find
   **duduclaw**, and turn on the folders it needs (Documents, Desktop,
   Downloads, Network Volumes, Removable Volumes).
3. Nothing needs restarting. Answering a prompt that DuDuClaw was waiting on
   is noticed within seconds; a change in System Settings is noticed at the
   next 10-minute check. The banner then disappears and admins get a
   "restored" message.

## Avoid the question altogether: where to keep files

The simplest fix is to keep DuDuClaw's files out of protected folders.

- **Work files for AI employees:** create a normal folder such as
  `~/DuDuClaw-files`, move the files there, and add it to
  `[files] allowed_roots` (Settings in the dashboard). Folders directly in
  your home folder are not protected.
- **`DUDUCLAW_HOME`:** keep it at the default `~/.duduclaw`. Never put it in
  Documents, Desktop or iCloud Drive — iCloud can also evict files to the
  cloud, which breaks databases.
- **Shared drives:** a network or USB volume is always protected. If you
  must use one, allow **Network Volumes** / **Removable Volumes** for duduclaw
  after every update (until the signed app build).

## Full Disk Access (optional)

**System Settings › Privacy & Security › Full Disk Access** also works, but
it is much broader: it lets DuDuClaw — and every AI tool it starts — read
everything on the Mac, including Mail, Messages and other apps' data. Prefer
the per-folder switches above.

Known issue: on macOS 26.1 and 26.2, a Full Disk Access grant for a bare
command-line binary (which the gateway is until the signed app build) does not
reliably take effect. If reads still fail after granting it, use the
per-folder switches instead.

## Enterprise: pre-approve with a PPPC profile

Managed Macs can pre-approve folder access with a Privacy Preferences Policy
Control (PPPC) profile from your MDM. **This applies only to the signed
DuDuClaw app build** (Team ID `7469HYQ6HH`, identifier
`ai.duduclaw.gateway`); the current command-line binary has no stable
signature for the profile to match.

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>PayloadType</key><string>Configuration</string>
  <key>PayloadIdentifier</key><string>ai.duduclaw.pppc</string>
  <key>PayloadUUID</key><string>REPLACE-WITH-A-UUID</string>
  <key>PayloadVersion</key><integer>1</integer>
  <key>PayloadDisplayName</key><string>DuDuClaw folder access</string>
  <key>PayloadContent</key>
  <array>
    <dict>
      <key>PayloadType</key><string>com.apple.TCC.configuration-profile-policy</string>
      <key>PayloadIdentifier</key><string>ai.duduclaw.pppc.tcc</string>
      <key>PayloadUUID</key><string>REPLACE-WITH-ANOTHER-UUID</string>
      <key>PayloadVersion</key><integer>1</integer>
      <key>Services</key>
      <dict>
        <key>SystemPolicyDocumentsFolder</key>
        <array>
          <dict>
            <key>Identifier</key><string>ai.duduclaw.gateway</string>
            <key>IdentifierType</key><string>bundleID</string>
            <key>CodeRequirement</key>
            <string>identifier "ai.duduclaw.gateway" and anchor apple generic and certificate leaf[subject.OU] = "7469HYQ6HH"</string>
            <key>Authorization</key><string>Allow</string>
          </dict>
        </array>
        <!-- Repeat the same <array> for the folders you need:
             SystemPolicyDesktopFolder, SystemPolicyDownloadsFolder,
             SystemPolicyNetworkVolumes, SystemPolicyRemovableVolumes -->
      </dict>
    </dict>
  </array>
</dict>
</plist>
```

Grant only the folders your teams actually use.

## Install with install.sh, not npm, on macOS

macOS keys the permission to the program's path. The npm package installs the
binary under a folder that includes the Node.js version
(for example `…/node/v22.x/lib/node_modules/…`), so upgrading Node — or using
a Node version manager — moves the binary and macOS asks again. The
`install.sh` installer keeps it at a fixed path (`~/.duduclaw/bin/duduclaw`):

```bash
curl -fsSL https://raw.githubusercontent.com/zhixuli0406/DuDuClaw/main/scripts/install.sh | sh
```
