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

1. **Install.** The image doubles as its own installer. Flash it to a USB
   stick, boot the target machine from that stick, and — after a
   double-confirmation prompt so an empty stick can never wipe the wrong
   disk — it writes a copy of itself onto the machine's internal drive and
   powers off. Remove the USB stick, power the machine back on, and it
   boots for real. A channel-installed unit can skip this step entirely if
   its drive was pre-flashed before it shipped.
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

## Optional Kiosk Display

A headless box with nothing plugged into its video output behaves exactly
as described above — that's the default and the common case. If a monitor
*is* connected at boot, the box notices and shows the dashboard full-screen
automatically instead of sitting idle behind a blank screen. This is purely
additive: it costs nothing on a box no one ever plugs a monitor into, and
it's meant for situations like an equipment rack or a shop counter where a
screen happens to be sitting there anyway.

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
recipe you can audit and reproduce on your own machine. See
[the appliance build guide](../guides/appliance-build.md) for the full
walkthrough (prerequisites, the `build.sh` entry point, a QEMU boot smoke
test, and the self-install USB flow).

## Current Status

This is a young part of the platform, and it's worth being direct about
where it stands rather than rounding up:

- The shipping build target is x86-64, and producing an actual, complete
  x86-64 image still needs to be done in a proper Linux/amd64 build
  environment — it hasn't been produced yet. A separate, faster local build
  path exists for Apple Silicon developers to iterate on the recipe itself,
  but that path targets arm64 purely for development speed and is
  explicitly **not** the shipping target.
- Full real-hardware validation — burn an image, boot it, walk through
  setup, exchange a message on a chat channel, apply an OS update, force a
  rollback, and run a factory reset, all on the actual certified hardware —
  hasn't happened yet either. What's been exercised so far is a boot-reachability
  smoke test under QEMU emulation.
- A handful of individual mechanisms (how the read-only root partition's
  identity survives a reboot, exactly how boot-counting interacts with the
  update tool, a few UEFI firmware path assumptions) are documented as open
  questions rather than confirmed behavior — see "Known open points" in
  [the build guide](../guides/appliance-build.md) for the complete, current
  list.

None of this blocks building and experimenting with the recipe today; it's
what's left before the appliance is something you'd hand to someone who
isn't comfortable debugging a boot failure themselves.
