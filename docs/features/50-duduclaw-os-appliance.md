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
   expired token is never mistaken for a working one. Pasting an API key
   directly works too, with no OAuth step at all. For chat channels:
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
| **Desktop edition** (`duduclaw-image-appliance`) | The full machine: the desktop plus Chromium / LibreOffice / Steam preloaded offline, the app compatibility layer (Bottles, Windows VM, Waydroid), read-only root, the firewall, first-boot provisioning, and login hardening. | The whole-disk `.wic.zst`, and its own live installer ISO (`installer-desktop`, shipping since v0.1.0 on 2026-09-04). |
| **Base image** (`duduclaw-image-ab`) | The same A/B layout, desktop shell and gateway, without the app layer, read-only root or firewall. A bring-up artifact rather than a product. | The payload of the plain live installer ISO (`installer`). |

What the desktop does when no monitor is attached — whether it falls back
to a headless dashboard-only box — has not been defined on real hardware
yet; it is an open bring-up item. The dashboard-on-the-LAN flow described
above works the same with or without a screen because it is served by the
gateway, not by the desktop.

## Security Design

- **No new process runs as root just to make the dashboard's power/update
  buttons work.** The handful of operations that genuinely need root
  privileges — reboot, shutdown, applying an OS update, re-arming first-boot
  setup — are handled by a small, separate helper process that speaks
  exactly six fixed commands over a local Unix socket and checks the
  identity of whoever's asking before doing anything. If that identity
  check isn't configured, it refuses every request rather than defaulting
  to trust.
- **Closed by default, on every port.** The firewall denies all inbound
  connections except the dashboard port and local network discovery.
  Remote SSH access is off out of the box and only turns on if you
  explicitly enable it from the dashboard.
- **The webhook relay never sees your channel secrets.** For webhook-style
  channels that need it, the relay's job is limited to forwarding an
  incoming request's raw bytes to the box over a connection the box opened
  first and authenticated with its own key — the relay itself never
  verifies a signature, never stores a channel secret, and never parses a
  payload. If the relay is ever compromised, it has nothing useful to leak.
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
where it stands rather than rounding up. As of DuDuClaw OS v0.2.0
(2026-09-09, embedding platform v1.63.0; v0.1.0 from 2026-09-04 stays
available as a rollback target):

- x86-64 images exist and are published for both machines, in three forms
  per machine (the desktop-edition whole-disk image, a live installer ISO
  for the desktop edition, and a live installer ISO for the base image),
  all signed with SHA-256 and minisign (the whole-disk image also ships a
  `.manifest.json` for provenance only). The QEMU machine is boot-verified
  for every form; the real-hardware machine (`duduclaw-genericx86-64`) has
  been config-audited only — it has not booted on real hardware yet.
- Full real-hardware validation — burn the media, boot it, install, walk
  through setup, exchange a message on a chat channel, apply an OS update,
  force a rollback, and run a factory reset, all on the actual certified
  hardware — hasn't happened yet. It is the most important open item.
- The trust chain is wired in the build layer but only partly switched on
  in the published images. Shipped since v0.1.0 and unchanged in v0.2.0:
  A/B atomic update with rollback, a read-only root in the desktop edition,
  and a minisign signature on every release artifact. Still **not** enabled
  as of v0.2.0: Secure Boot signing of the UKIs and dm-verity root
  verification (both come from the `sb-signing` build overlay), and TPM2 +
  LUKS key sealing (the `tpm-luks` overlay; its automatic enrollment is
  still an open defect that needs a real-hardware TPM to close). The
  v0.2.0 build chains neither overlay — `release-os.sh` today chains only
  the `serial1.yml` release overlay — so the published images still boot
  with Secure Boot off.
- The OS is versioned independently of the platform (`0.x` = bring-up;
  `1.0.0` will mark the first GA). Release-by-release status lives in the
  [DuDuClaw-OS CHANGELOG](https://os.duduclaw.dudustudio.monster/docs/os/changelog/).

None of this blocks flashing and experimenting with a release today; it's
what's left before the appliance is something you'd hand to someone who
isn't comfortable debugging a boot failure themselves.
