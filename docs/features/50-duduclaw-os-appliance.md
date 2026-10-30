# DuDuClaw OS Appliance

> Plug in power and a network cable, and a small PC becomes a headless AI
> employee — the dashboard comes up on your LAN in under two minutes, no
> screen or keyboard required.

## What It Is

DuDuClaw normally runs as a binary you install on a machine you already
manage. The appliance is the other end of that spectrum: a bootable disk
image that turns a small, off-the-shelf mini-PC into a purpose-built
DuDuClaw box. There's no OS to configure, no dependencies to install, no
terminal to open — the box boots straight into DuDuClaw and stays there.
Everything after power-on happens in a browser, on your own LAN.

It's built for the same audience the rest of DuDuClaw serves: someone who
wants an AI employee watching Telegram/LINE/Discord/Slack around the clock,
without dedicating a laptop to it or trusting a third party with the
account credentials.

## From Power-On to Ready

1. **Install.** Each release ships two forms per machine. The live
   installer ISO: flash it to a USB stick (or burn it to a disc), boot the
   target machine from it in UEFI mode with Secure Boot **off** (the
   published images are not Secure-Boot signed — see Editions and Trust
   Chain below), pick the internal SSD in the graphical installer, and
   reboot into the installed system. The whole-disk image (`.wic.zst`)
   skips the installer: decompress it and write it straight to the target
   disk. A channel-installed unit can skip this step entirely if its drive
   was pre-flashed before it shipped.
2. **First boot.** The box syncs its clock over NTP and sets its default
   timezone before touching the network (a clock that's wrong breaks OAuth
   and TLS silently, so this happens first), picks up an address over wired
   DHCP, and announces itself on the LAN as `duduclaw.local`. The dashboard
   comes up bound to your local network — not just to the box itself — but
   the firewall only lets the dashboard port and the discovery protocol
   through; nothing else on the box is reachable, and nothing is ever
   exposed to the public internet by default.
3. **Connect.** Open `http://duduclaw.local` from any Mac, iPhone, or
   Windows machine on the same network. Android doesn't resolve `.local`
   addresses, so the box also offers a small companion discovery page that
   shows its address directly. Whoever connects first, before setup is
   finished, is the one who sets it up — the same first-come convention
   used by other self-hosted appliances.
4. **Set up an administrator**, then pick a language and confirm the
   timezone.
5. **Connect a model account and a chat channel.** A setup wizard drives
   `claude setup-token` on the box and shows you a link (and a QR code) —
   you approve it in your own browser, paste the short code back, and the
   box makes one real API call before it saves anything, so a bad or
   expired token is never mistaken for a working one. Before it starts, the
   wizard shows a subscription risk notice you have to accept: Anthropic's
   [policy](https://code.claude.com/docs/en/legal-and-compliance) does not permit
   third-party products to collect or store Claude.ai credentials or route
   requests through Free/Pro/Max plan credentials on their users' behalf,
   and Anthropic may enforce this without notice. Pasting an API key
   directly works too, with no OAuth step at all, and is the path Anthropic
   documents for products. For chat channels:
   Telegram, Discord, Slack, and the web chat connect directly outbound —
   just paste a token. LINE and the other webhook-style channels can't
   receive anything on a box sitting behind a home router's NAT, so those
   route through an official relay service; the relay only ever forwards
   opaque bytes over a connection the box itself opened, and the actual
   signature check on each incoming webhook still happens on the box, not
   in the relay (see Security Design below).
6. **Pick an industry template**, which creates your first AI employee, and
   send it a test message on the channel you just connected. A reply
   confirms the box is actually working end to end, not just that the
   dashboard loaded.
7. **Standby.** From here the box just runs. A systemd watchdog restarts a
   hung gateway process, and a run of failed boots automatically falls back
   to the previous OS version instead of getting stuck.

## The Device Page

An appliance install gains a **Device** page — visible only there; a
regular desktop or server install never sees it in navigation at all, and
the page itself refuses to render off an appliance rather than showing
something misleading.

- **Status** — a live CPU / memory / disk / temperature / network snapshot.
- **Update center** — the operating system itself updates independently of
  the DuDuClaw application, on an A/B partition scheme: apply an update,
  and if the box fails to boot afterward it falls back to the version it
  was running before, automatically. One-click rollback on request (as
  opposed to the automatic on-failure kind) is intentionally reported as
  not yet available rather than guessed at — the underlying update tool
  has no built-in "undo" command, and picking a boot slot without being
  certain of the mechanism risks bricking a box with no screen attached to
  debug it from.
- **Network** — today, a read-only view of the box's network interfaces;
  editing a static IP from the dashboard isn't wired up yet.
- **Backup** — archive everything the box has learned and stored (agent
  memory, conversation history, configuration) to a file you can download.
- **Danger zone** — factory reset (requires typing the word "RESET" to
  confirm, not just clicking a button), restart, and shutdown, each behind
  its own confirmation.

Everything on this page is also reachable two other ways — an AI employee
can ask for it as a tool, and an operator can run it from the box's own
terminal. All three go through one implementation
(`crates/duduclaw-gateway/src/os_ops.rs`), so an answer never depends on
which door you came in by; only the permission check differs, because each
door has a different way of knowing who is asking. See
[33-os-native-perception.md](33-os-native-perception.md) for the details.

## Editions and the Desktop

Every DuDuClaw OS image boots into DuDuClaw's own desktop — a compositor and
shell of its own, with a lock screen, a first-run wizard, and a Cmd+K bar
that hands work to an AI employee from any app. A person and the AI share
the machine, and the compositor guarantees the person always wins the
input: touch the keyboard or mouse and whatever the AI was driving on your
screen freezes. How that works, and what the AI is and is not allowed to do
on your desktop, is its own article: [52-desktop-edition.md](52-desktop-edition.md).

Two images ship per release, and they differ in what sits on top of that
desktop:

| Image | What it is | Ships as |
|---|---|---|
| **Desktop edition** (`duduclaw-image-appliance`) | The full machine: the desktop plus Chromium / LibreOffice preloaded offline (Steam installs from Flathub), the app compatibility layer (Bottles, Windows VM, Waydroid), read-only root, the firewall, first-boot provisioning, and login hardening. | The whole-disk `.wic.zst`, and its own live installer ISO (`installer-desktop`, shipping since v0.1.0 on 2026-09-04). |
| **Base image** (`duduclaw-image-ab`) | The same A/B layout, desktop shell and gateway, without the app layer or read-only root (from v0.4.0 it has the same firewall as the desktop edition; v0.3.0 and earlier have none). A bring-up artifact rather than a product. | The payload of the plain live installer ISO (`installer`). |

What the desktop does when no monitor is attached — whether it falls back
to a headless dashboard-only box — has not been defined on real hardware
yet; it is an open bring-up item. The dashboard-on-the-LAN flow described
above works the same with or without a screen because it is served by the
gateway, not by the desktop.

## Security Design

- **Root-level operations go through a small helper with a fixed command
  list.** Operations that need root privileges (reboot, shutdown, applying
  or rolling back an OS update, factory reset, network and time settings)
  are handled by a separate helper, `duduclaw-sysd`, that accepts exactly
  20 fixed commands (18 up to platform v1.74.0) over a local Unix socket
  and checks the caller's user
  id before doing anything. If no allowed caller is configured, it refuses
  every request. From DuDuClaw OS v0.4.0 (embeds platform v1.73.4) the
  gateway runs as its own non-root user (`duduclaw`, uid 1000), and the
  helper accepts only the gateway's main process, verifies OS updates
  itself (signature, hashes, version) before installing them, and refuses
  to roll back to an image older than v0.4.0. AI employees run as the same
  user as the gateway, so this keeps them away from root, not away from the
  gateway's own powers (see [Security defense](05-security-defense.md)).
  On v0.3.0 and earlier images the gateway still runs as root and the
  helper accepts root, so there the split does not limit what the gateway
  can do.
- **Inbound connections are denied by default, and SSH is off.** From
  DuDuClaw OS v0.4.0 both editions (the desktop edition and the base
  edition written by the plain `installer` ISO) ship a firewall that drops
  everything inbound except the dashboard port (18789), mDNS for local
  network discovery (UDP 5353) and basic ICMP such as ping. The dropbear
  SSH server is installed but off by default. The SSH switch in the
  dashboard's maintenance mode (platform v1.73.3 or later; v0.4.0 embeds
  v1.73.4) has `duduclaw-sysd` start `dropbear.socket` and add port 22 to
  the firewall's `maintenance_tcp_ports` set; when the window closes it
  stops dropbear, ends every session process and removes the port. No
  account on the images can log in over SSH yet (root is locked and
  dropbear refuses root logins), so for now the switch only controls the
  port. Platform v1.75.0 adds the
  helper side of a dedicated maintenance login: an account
  `duduclaw-maint` (uid 4800) and one public key per window, which the
  helper checks, installs and removes when the window closes, with its
  own deadline and audit file (see
  [Security defense](05-security-defense.md)). It does nothing until an
  OS image ships the account; on such an image the helper refuses the
  old port-only switch.
  The v1.75.0 gateway side asks for the technician's public key in the
  same request as the typed confirmation and the password re-entry
  (five wrong passwords in a row lock opening for 15 minutes), records the
  window before asking the helper, and never falls back to the port-only
  switch: with a v0.4.0 or older image, or a partly updated one, the SSH
  option is reported as unsupported. A separate background task started
  with the gateway closes windows at their TTL, closes any key the helper
  holds without a matching window, copies the helper's sign-in records
  into the maintenance history, and tells every Admin the key and device
  fingerprints when a window opens. If the helper cannot be reached when a
  window closes, the close is recorded as unconfirmed, never as done.
  On v0.3.0 and earlier images dropbear listens on port 22 from boot. The
  desktop edition's firewall has no rule for it, so SSH cannot be reached
  from the network there, but the base edition has no firewall, so on it
  dropbear and the dashboard port are reachable from the LAN. Those images
  embed platform v1.73.1 or older, and up to platform v1.73.2 the SSH
  switch changed neither case: it started and stopped a unit named
  `ssh.service` (the name used by the earlier Debian-based image), which
  these images do not have, and it never added a firewall rule. A platform
  v1.73.3 or later gateway on such an image cannot open SSH either: the
  desktop edition's firewall has no maintenance port set, so opening is
  refused, and the close that follows the refusal stops the dropbear that
  was already running; on the base edition, closing likewise stops it. On
  both, dropbear starts again on the next boot.
- **The webhook relay never sees your channel secrets.** For webhook-style
  channels that need it, the relay's job is limited to forwarding an
  incoming request's raw bytes to the box over a connection the box opened
  first and authenticated with its own key — the relay itself never
  verifies a signature, never stores a channel secret, and never parses a
  payload. If the relay is ever compromised, it has nothing useful to leak.
- **The lock screen checks your password without needing your email.**
  (DuDuClaw OS v0.3.0 and later, which ship shell `duduclaw-shell` 1.65.3
  with platform v1.73.1. On v0.2.0 and earlier images the lock screen still
  checks the password as `admin@local`.)
  It asks the gateway `POST /api/owner/verify` with `{"password": "..."}`,
  which finds the owner account by its owner flag, so unlocking keeps
  working after you replace `admin@local` with your own address. The
  endpoint only answers yes (`{"ok": true}`) or no (401): it never hands out
  a login token and never returns the email. It accepts calls only from the
  machine itself and refuses anything that came through a reverse proxy.
  Every local caller shares one attempt limit with a local `/api/login` for
  the owner (5 tries per 15 minutes), so it is not a second way to guess the
  password. The flip side: any program running on the machine can use up
  those 5 tries, and because the 15-minute window is fixed, a program that
  keeps running can use them up again each time the window resets, so the
  lock screen can keep refusing for as long as that program runs. We accept
  that, because a hostile program already running on the box is a bigger
  problem than a locked screen; the fix is to find that program and stop
  it.
- **Nothing about the image is "trust us."** The build recipe that produces
  the image is public — see Building It Yourself below — so anyone can
  read every line that goes into the box before flashing it, rather than
  taking a vendor's word for what's inside.

## Building It Yourself

The image isn't shipped as a mystery binary — it's built from a public
Yocto layer you can audit and reproduce on your own machine, and every
release artifact is published with a SHA-256 and a minisign signature.
Since 2026-09 the layer and its release pipeline live in the standalone
[DuDuClaw-OS](https://github.com/zhixuli0406/DuDuClaw-OS) repo; see
[the build guide](../guides/appliance-build.md) for where to download a
signed release or build one from source. The project's own site,
[os.duduclaw.dudustudio.monster](https://os.duduclaw.dudustudio.monster),
mirrors the full OS documentation — including this page, the OS README,
CHANGELOG, and SECURITY notes — under `/docs/`.

## Current Status

This is a young part of the platform, and it's worth being direct about
where it stands rather than rounding up. As of DuDuClaw OS v0.4.0
(2026-10-16, embedding platform v1.73.4; the earlier v0.3.0, v0.2.0 and
v0.1.0 releases stay on the Releases page, but a device that has booted
v0.4.0 successfully cannot roll back to them):

- x86-64 images exist and are published for both machines, in three forms
  per machine (the desktop-edition whole-disk image, a live installer ISO
  for the desktop edition, and a live installer ISO for the base image),
  all signed with SHA-256 and minisign (the whole-disk image also ships a
  `.manifest.json` for provenance only). For v0.4.0, the QEMU machine's
  whole-disk image passed the release smoke boot under QEMU (software
  emulation, no KVM), reaching the login prompt in about 65 seconds, and
  both QEMU-machine installer ISOs booted to the live environment's
  automatic root login in about 125 seconds each. These boot checks do not
  establish installer-wizard completion. The real-hardware machine
  (`duduclaw-genericx86-64`) was built but not boot-tested, and it has not
  booted on real hardware yet.
- The non-root gateway was checked on a test variant of the QEMU
  machine's desktop image, under the same headless QEMU on the build
  server, in two verification rounds. The second round passed 35 of 36
  checks; the one failure was the `duduclaw doctor` headline, which still
  said PASS while the container features had correctly turned themselves
  off. That wording is fixed on the platform's main branch (#132), after
  v1.73.4. Not verified for v0.4.0: upgrading an existing v0.3.0 device
  through the A/B update path (the data ownership migration and the
  cleanup of old boot entries), the full task sandbox, computer use and
  Discovery flows that need LLM credentials, the Windows VM (it needs
  KVM), Wi-Fi and audio on real hardware, and booting
  `duduclaw-genericx86-64`.
- Upgrading to v0.4.0 is one-way. `duduclaw-sysd` refuses rollbacks to
  images older than v0.4.0, and once the first v0.4.0 boot has been marked
  good the older boot entries are deleted, so the boot loader cannot fall
  back to them either (those images run the gateway as root). A bad later
  release is fixed by installing a newer image or using the rescue entry.
- Full real-hardware validation — burn the media, boot it, install, walk
  through setup, exchange a message on a chat channel, apply an OS update,
  force a rollback, and run a factory reset, all on the actual certified
  hardware — hasn't happened yet. It is the most important open item.
- The trust chain is wired in the build layer but only partly switched on
  in the published images. Shipped since v0.1.0 and still in v0.4.0: A/B
  atomic update with rollback (from v0.4.0, rollback stops at v0.4.0
  itself), a read-only root in the desktop edition, and a minisign
  signature on every release artifact. Still **not** enabled as of v0.4.0:
  Secure Boot signing of the UKIs and dm-verity root
  verification (both come from the `sb-signing` build overlay), and TPM2 +
  LUKS key sealing (the `tpm-luks` overlay; its automatic enrollment is
  still an open defect that needs a real-hardware TPM to close). No
  release so far has chained either overlay: `release-os.sh` adds one
  release overlay per build (`serial1.yml` by default; v0.3.0 used
  `release-linux-builder.yml`, which only sets build parallelism and turns
  off SPDX output). The published v0.3.0 whole-disk images were taken
  apart and have no verity partition, and their UKIs and boot loader carry
  no signature, so they boot with Secure Boot off; the v0.4.0 release
  notes state that Secure Boot, dm-verity and TPM2 are still not enabled
  on release images.
- The OS is versioned independently of the platform (`0.x` = bring-up;
  `1.0.0` will mark the first GA). Release-by-release status lives in the
  [DuDuClaw-OS CHANGELOG](https://os.duduclaw.dudustudio.monster/docs/os/changelog/).

None of this blocks flashing and experimenting with a release today; it's
what's left before the appliance is something you'd hand to someone who
isn't comfortable debugging a boot failure themselves.

## Maintenance SSH dashboard (v1.75.0)

In Device → Maintenance mode, an Admin selects the duration and capabilities,
re-enters their password and types `MAINTENANCE`. Selecting SSH also requires
one technician public key in OpenSSH format. The dashboard checks the input
and immediately clears a pasted private key; the gateway and device helper
remain the authority. RSA keys must be 3072–8192 bits. Unknown support or an
old/partly updated image disables SSH, while diagnostic details remain
available. Five wrong passwords lock opening for 15 minutes.

An open window shows `ssh duduclaw-maint@duduclaw.local`, available private
IPv4 addresses, the technician and device fingerprints, expiry, connections
and failed sign-ins. Compare both fingerprints with the Admin notification.
If no notification channel is available, use the local device screen; this
dashboard alone cannot authenticate the device. A configured channel does
not prove delivery. Port 22 accepts private/link-local and VPN CGNAT
`100.64.0.0/10` sources, not global IPv6; a matching range does not establish
trust. The menu permits selected diagnostics and restarting the local model
or gateway. Restarting the gateway closes the window and disconnects SSH.

An unconfirmed close stays visible as a warning. Expand Maintenance history
for recent window fingerprints and connection events. The UI polls every
30 seconds, so an expiry or device-side close may appear on the next poll.
The complete OS image and real SSH/notification flow are not yet verified;
the page works only with an OS image that ships the maintenance account, and it is not acceptance of the assembled device.
