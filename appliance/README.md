# DuDuClaw OS — appliance image

A bootable Debian-based disk image that turns a small x86-64 PC into a
headless DuDuClaw appliance: plug in power and ethernet, and the dashboard
comes up on the LAN at `http://duduclaw.local`. Everything after that
(model accounts, channel bots, first agent) is configured in the browser —
the box itself has no screen, keyboard, or interactive setup.

If a display *is* plugged into the box (HDMI/DP), it self-detects at boot
and shows the dashboard full-screen instead of sitting idle — see "Kiosk
display session" below. This is additive only: a box with nothing plugged
into its video output boots exactly as described above, unchanged.

This directory is the mkosi build recipe plus the supporting scripts. It
does **not** contain a pre-built image — you build one from source, on
your own machine, and can audit every line that goes into it (there is no
"trust us" step: the image is the recipe, run).

## Layout

```
appliance/
├── mkosi.conf              distro, packages, boot config (the recipe entry point)
├── mkosi.conf.d/            architecture-gated drop-ins (kernel package: amd64 vs arm64)
├── mkosi.version            image version string
├── mkosi.repart/            GPT partition layout (ESP, root A/B, /data)
├── mkosi.skeleton/           files copied into the OS tree BEFORE packages install
├── mkosi.extra/               files copied into the OS tree AFTER packages install
│   ├── etc/chromium/policies/managed/   kiosk Chromium enterprise policy (JSON)
│   ├── etc/flatpak/installations.d/     app repository redirected onto /data
│   ├── etc/systemd/system/     first-boot + gateway + kiosk systemd units
│   ├── etc/sysupdate.d/        A/B update transfer definitions
│   └── usr/local/sbin/         the scripts those units run
├── postinst.d/           scripts run once, inside the build chroot
├── Dockerfile.mkosi-runner   mkosi build environment for non-Linux hosts
├── build.sh                  build entry point
└── smoke-qemu.sh             UEFI boot smoke test under QEMU
```

Every `mkosi.*` directory/file name is a recognized mkosi convention, not
an arbitrary choice — see the comments inside each file for what's been
independently checked against the upstream
[mkosi manual](https://github.com/systemd/mkosi/blob/main/mkosi/resources/man/mkosi.1.md)
and [systemd manuals](https://www.freedesktop.org/software/systemd/man/latest/)
versus what's a documented-but-not-live-tested assumption.

## What gets built

One GPT disk image (`mkosi.output/duduclaw-os.raw`):

| Partition | Contents | Notes |
|---|---|---|
| ESP | systemd-boot + Unified Kernel Images | shared by both root slots |
| root A | the actual OS (Debian 13 + duduclaw + Node/Python/Docker/…) | read-only, populated at build time |
| root B | empty, same size as A | filled by the first `systemd-sysupdate` run |
| /data | empty ext4, grows to fill the disk | `~/.duduclaw`, Docker storage, model files — the only writable partition |

The **same image** is both the installer and the shipped product: dd it to
a USB stick and boot a machine with an internal NVMe from it, and the
image writes a copy of itself onto that NVMe and powers off (see
`mkosi.extra/usr/local/sbin/duduclaw-usb-install.sh`). There is no
separate "installer" build.

## Prerequisites

- **Linux**: `mkosi` (Debian/Ubuntu: `apt-get install mkosi`), Docker (to
  build the `duduclaw` binary — see below).
- **macOS**: Docker Desktop only. `build.sh` runs mkosi inside a container
  (`Dockerfile.mkosi-runner`) since mkosi builds GPT disk images with
  loopback/mount operations that don't exist on macOS directly.
  **Docker Desktop memory**: the default VM (8GB RAM / 4 CPUs on a fresh
  install) is tight — `cargo build --release -p duduclaw-cli -p
  duduclaw-gateway` compiling with an uncapped job count OOM-kills partway
  through linking `duduclaw-gateway` (`SIGKILL`, Docker reports it as
  `ResourceExhausted: ... cannot allocate memory` — this was hit on a real
  first build attempt, not a theoretical concern). `build.sh` now caps
  parallelism via `CARGO_JOBS` (default `2`) to work around this, but if
  you still see the OOM, raise Docker Desktop's VM memory (Settings →
  Resources) to **12GB or more**, or lower `CARGO_JOBS` further (`1` is
  the safest floor). Symptom to watch for either way: the `rust-builder`
  stage dies mid-`cargo build` with no Rust compiler error, just a killed
  process.
- **QEMU smoke test** (either OS): `qemu-system-x86_64` and/or
  `qemu-system-aarch64` + OVMF/edk2 UEFI firmware — see below for which
  one you need. macOS: `brew install qemu` installs both. Debian/Ubuntu:
  `apt-get install qemu-system-x86 ovmf` (x86-64 target) and/or
  `apt-get install qemu-system-arm qemu-efi-aarch64` (arm64 target).

## Build

Two build paths, selected by the `APPLIANCE_ARCH` environment variable
(default `x86-64`):

- **Shipping build** (`APPLIANCE_ARCH=x86-64`, the default — no need to
  set it): produces the actual product target. On an amd64 Linux host or
  CI runner this is a same-architecture build (fast, no emulation). On an
  Apple Silicon Mac, `build.sh` now passes `docker build --platform
  linux/amd64` explicitly (previously it silently used the host's native
  platform — on Apple Silicon that meant an aarch64 binary landing in an
  image whose `mkosi.conf` expects x86-64, which cannot run there at all),
  so this path is correct but runs under Docker Desktop's x86-64
  emulation the whole way through, which is slow and is where the memory
  pressure above bites hardest.
- **Local arm64 smoke build** (`APPLIANCE_ARCH=arm64`): builds a
  native-architecture image on an Apple Silicon Mac — no cross-arch
  emulation for either the Rust binary or the mkosi/Debian bootstrap —
  for a fast local QEMU smoke test (with `-accel hvf`, see below) during
  development. This is **not** the shipping target; it exists purely to
  make local iteration on this recipe practical without needing a Linux
  box or CI turnaround for every change.

```sh
appliance/build.sh                       # shipping build: x86-64
APPLIANCE_ARCH=arm64 appliance/build.sh  # local Apple Silicon smoke build
```

This does two things:

1. Builds a Linux `duduclaw` release binary matching `APPLIANCE_ARCH` via
   Docker (reusing `container/Dockerfile.server`'s `rust-builder` stage —
   the same pipeline `scripts/release.sh` already uses to produce Linux
   release binaries, so there's exactly one place that knows how to build
   DuDuClaw for Linux, not two). Skip this by pointing
   `DUDUCLAW_BIN_PATH=/path/to/a/linux/duduclaw` (matching architecture)
   at an already-built binary. `CARGO_JOBS` (default `2`) caps compile
   parallelism to avoid the Docker Desktop OOM described above.
2. Runs `mkosi build --architecture=<arch>`, injecting that binary via
   `--extra-tree=<path>:/usr/local/bin/duduclaw` (natively on Linux, via
   `Dockerfile.mkosi-runner` everywhere else). The architecture-specific
   kernel package (`linux-image-amd64` vs `linux-image-arm64`) is selected
   automatically via `mkosi.conf.d/10-arch-{x86-64,arm64}.conf`.

Output lands in `appliance/mkosi.output/`.

## QEMU smoke test

```sh
appliance/smoke-qemu.sh                          # boots an x86-64-built image
APPLIANCE_ARCH=arm64 appliance/smoke-qemu.sh      # boots an arm64-built image
# or: appliance/smoke-qemu.sh path/to/some-other-image.raw
```

`APPLIANCE_ARCH` must match whatever `build.sh` was run with — it selects
both the QEMU binary and the UEFI firmware candidates, not just a display
label.

- `APPLIANCE_ARCH=x86-64` (default): `qemu-system-x86_64`, `q35` machine,
  QEMU's TCG software CPU emulation — slow, but it doesn't need hardware
  virtualization support, so it runs the same way on any build host,
  including cross-architecture (an x86-64 image on an Apple Silicon Mac
  can only ever use TCG here, never hardware acceleration).
- `APPLIANCE_ARCH=arm64`: `qemu-system-aarch64`, `virt` machine. On an
  Apple Silicon host this uses `-accel hvf` (native hardware acceleration
  — fast, the whole point of the local arm64 build path above);
  everywhere else it falls back to TCG like the x86-64 path does.

It passes when the serial console shows both:

- `Reached target Multi-User System` (the OS booted all the way up), and
- some sign `duduclaw-gateway.service` was started (started or failed —
  either proves the unit is wired up; testing that the gateway actually
  *works* needs real model credentials and is out of scope for a boot
  smoke test).

This is a boot-reachability check, not a full functional validation —
first-boot provisioning, A/B updates, USB self-install, and the actual
onboarding flow all need a real machine.

## Boot sequence (what the shipped image actually does)

1. UEFI → systemd-boot picks a UKI (highest boot-counting priority) → root
   mounts read-only from that slot.
2. `duduclaw-usb-install.service`: no-op unless booted from removable
   media with an internal NVMe present (see the script's own header for
   the exact unattended-install safety gate).
3. `duduclaw-firstboot-repart.service`: re-applies the shipped partition
   definitions against the real disk, growing `/data` to fill whatever
   space is available beyond the fixed-size ESP/root-A/root-B.
4. `duduclaw-firstboot-provision.service`: seeds a persisted machine-id
   copy, a placeholder device key, and a minimal `config.toml` (LAN-bound
   dashboard) onto `/data` — then disables itself.
5. `duduclaw-gateway.service` starts, Avahi advertises `duduclaw.local`,
   nftables allows only the dashboard port + mDNS in from the LAN. It now
   also `sd_notify`s systemd (`Type=notify` + `WatchdogSec=60`) once its
   listener is bound, and pings the watchdog every 30s after that — a
   hung-but-still-running process gets killed and restarted, not just a
   crashed one.
6. `duduclaw-kiosk.service`: `ExecCondition=` checks
   `/sys/class/drm/*/status` for any `connected` output. No display
   attached (the common case) → clean skip, nothing else happens. Display
   attached → `seatd.service` (already running by this point) hands DRM
   access to the session compositor, which fullscreens the one and only
   client. See "Kiosk display session" below for which compositor and
   which client, and for the automatic fallback chain between them.
7. `duduclaw-flatpak-setup.service`: creates `/data/flatpak` and adds the
   flathub remote to it (retried on later boots if there's no uplink yet).
   See "Flatpak app layer" below.

## Kiosk display session

Off in effect on every headless box, on automatically the moment a
monitor is attached and the box (re)boots — no config flag either way, see
`mkosi.extra/etc/systemd/system/duduclaw-kiosk.service` for the detection
mechanism and the full device-access chain (`seatd` → `video`/`render`
group membership → compositor → client), each link verified against
upstream/Debian source rather than assumed.

### Which compositor, which client

`duduclaw-kiosk-launch.sh` picks one of three session shapes and falls
back automatically. Nothing here needs a rebuild to change:

| Shape | Compositor | Client | Selected when |
|---|---|---|---|
| `comp` | `duduclaw-comp` (owns DRM/KMS via seatd) | `duduclaw-shell` | both binaries present and comp isn't failure-blacklisted — **the preferred shape, but see the pin below** |
| `cage` | `cage` | `duduclaw-shell` | shell present, comp absent/blacklisted/failed |
| `chromium` | `cage` | Chromium at `http://127.0.0.1:18789` | no shell in the image (the pre-shell behavior, unchanged) |

Which binaries are in the image is a build-time choice:
`DUDUCLAW_SHELL_BIN_PATH=` and `DUDUCLAW_COMP_BIN_PATH=` on `build.sh`
(steps 1c/1d). Neither is built by this repo's build script — both come
from detached workspaces (`crates/duduclaw-shell/BUILD-LINUX.md`,
`crates/duduclaw-comp/BUILD.md`).

**Automatic fallback.** `comp` is attempted, not assumed. The launcher
starts it, waits for its Wayland socket, starts the shell against that
socket, and requires *both* processes to survive a probe window before
calling the session healthy (the socket alone is a weak signal — smithay
creates the listener before the backend is up, so "socket exists" does not
prove comp got the DRM device). Anything short of that → kill both, log
the reason, wait a second for the DRM device to be released, and start the
`cage` shape instead. The journal always says which shape actually ran and
why. Three consecutive early failures write a breadcrumb
(`/data/duduclaw-kiosk/.kiosk-comp-failures`) that makes auto-selection
stop trying comp until it's removed; a healthy session clears it.

**Live-verified (2026-08-22, A4 wave).** The `comp` shape was taken end to
end on the appliance VM against real hardware: libseat session on `seat0`,
`/dev/dri/card0`, connector `Virtual-1` + CRTC, a 1280x800 output, real
pixels on screen, absolute-pointer input landing where it was sent, and
**idle CPU measured at 0.00%** (against `cage`'s ~100% and the winit
backend's 32%) — with `duduclaw-shell` running on it, load average was
0.10 versus 5.6 for the old cage+shell stack. Two defects found in that
round and fixed in the same wave, both of which would have shipped a
box that boots to an unusable screen:

- **comp did not flush Wayland clients unless it rendered.** `flush_clients()`
  sat at the end of the render path, which is skipped whenever nothing is
  dirty — so a client still doing its opening `wl_registry`/`wl_display.sync`
  roundtrip never got the replies (nothing in that roundtrip damages an
  output). The shell hung forever at 0% CPU. Fixed by flushing every event
  loop iteration; idle cost is unchanged.
- **`duduclaw-shell` received no keyboard or pointer events at all.** Root
  cause is client-side: gpui's Wayland backend stores exactly one seat
  (`gpui_linux/.../wayland/client.rs:309` is literally
  `// TODO: Multi seat support`) and each new seat's capabilities event
  *releases* the previous seat's keyboard/pointer — so the shell was left
  holding the co-drive **agent** seat while focus lives on the human seat.
  comp advertises two `wl_seat` globals on purpose (that separation is what
  makes freeze/e-stop structurally agent-only), so the fix is
  `seat_order.rs`: advertise the agent seat FIRST so gpui's last-wins lands
  on the human seat. Zero change to the co-drive safety model. Override with
  `DUDUCLAW_COMP_SEAT_ORDER=human-first` (which reproduces the broken state
  exactly, useful as an A/B). Known cost: a gpui client can no longer be
  agent-driven, since it releases the agent seat's resources; every other
  client keeps both seats (`foot` re-verified under the new order).
  The upstream gpui patch is written up in `crates/duduclaw-comp/BUILD.md`
  for whenever a zed fork exists to carry it.

**Escape hatches** (drop a file from a serial/debug session, then
`systemctl restart duduclaw-kiosk.service` — no image rebuild):

- `/etc/duduclaw/kiosk-app` — one word, forces the shape: `chromium`,
  `comp`, or `cage` (`shell` is accepted as the historical spelling of
  `cage`). Checked before everything else, including the blacklist.
- `/etc/duduclaw/kiosk.env` — optional shell fragment, sourced with
  `set -a`. Knobs: `DUDUCLAW_KIOSK_DBUS=0`,
  `DUDUCLAW_KIOSK_COMP_MAX_FAILURES`,
  `DUDUCLAW_KIOSK_COMP_SOCKET_WAIT_SECS`,
  `DUDUCLAW_KIOSK_COMP_SHELL_PROBE_SECS`, `DUDUCLAW_COMP_BACKEND`.

**Session D-Bus bus.** This unit is a plain system service, not a
PAM/logind session, so nothing would otherwise create a session bus. The
launcher re-execs itself under `dbus-run-session` so the whole session
tree shares one, and pushes `WAYLAND_DISPLAY`/`XDG_RUNTIME_DIR`/
`XDG_SESSION_TYPE` into the bus's activation environment once the
compositor is up — without that, a D-Bus-activated `xdg-desktop-portal`
would be spawned with no idea where the display is. Fail-open: if
`dbus-run-session` isn't installed the session starts exactly as before
and says so in the journal (Flatpak apps then have no portal support).

**Don't set `LIBGL_ALWAYS_SOFTWARE` on the compositor.** Mesa refuses
("Not allowed to force software rendering when API explicitly selects a
hardware device") and the compositor segfaults. It was found this way with
`cage`; the same rule now points at `duduclaw-comp`.

**Estimated cost** (not measured on real hardware this round — flagged as
an estimate per the task, not a benchmark result):

- **Image size**: Chromium dominates — Debian trixie's `chromium` package
  alone is ~87.5 MB download / **~317 MB installed** on amd64
  ([packages.debian.org](https://packages.debian.org/trixie/chromium)).
  `cage` + `seatd` + the `xwayland`/`libwlroots-0.18` stack `cage` pulls in
  add well under 5 MB combined (`cage` itself is ~21 KB, `seatd` ~27 KB,
  `xwayland` ~883 KB download-size — the wlroots shared libraries are the
  only non-trivial addition beyond Chromium, still an order of magnitude
  smaller than it). Rough total: **image grows by roughly 320–340 MB**
  regardless of whether a display is ever actually attached (packages are
  installed either way; only the *running* cost is detection-gated).
- **RAM**: not independently measured this round — a single Chromium tab
  rendering a dashboard-weight web app typically lands somewhere in the
  **300–600 MB** range in general practice (varies heavily with page
  complexity, extensions — none installed here — and GPU compositing
  path), plus `cage`/`seatd`'s own footprint (tens of MB, wlroots
  compositors are lightweight). Only paid when a display is attached and
  the unit actually starts; headless boxes pay nothing beyond the
  binaries already sitting unused on disk.

## Flatpak app layer

Third-party desktop apps run as Flatpaks. Three packages are installed
(`flatpak`, `xdg-desktop-portal`, `xdg-desktop-portal-gtk`) and
`xdg-desktop-portal-wlr` deliberately is not: Screenshot/ScreenCast
backends forward frames captured through `wlr-screencopy`, and
`duduclaw-comp` implements no screen-capture protocol, so that package
would be dead weight rather than a partial win. Everything behind these
choices — including a live container run of a real Flathub Chromium
against `duduclaw-comp` with **zero** portal backends installed, which
started and rendered fine — is written up in
`research/native-os-2026-08/flatpak-portal-scope-2026-08.md`.

**The app repository lives on `/data`, never on root.** A single Chromium
plus the freedesktop runtime measures 2.4 GB on disk; the root partition
is a fixed 5 GB with well under 1.4 GB free. `mkosi.extra/etc/flatpak/
installations.d/10-duduclaw-data.conf` declares a named installation
`data` at `/data/flatpak`, and it ships as static image content precisely
because it has to be in place *before* the first `flatpak install` — the
repository layout is created by that first install, and once it lands in
`/var/lib/flatpak` moving it is surgery.

**Every command needs `--installation=data`.** This adds an installation;
it does not move the default one. A bare `flatpak install <app>` still
targets `/var/lib/flatpak` on the root partition and will fill it.

```bash
flatpak --installation=data list
flatpak remote-ls --installation=data flathub
flatpak install --installation=data flathub org.libreoffice.LibreOffice
```

`duduclaw-flatpak-setup.service` runs on every boot and does three things,
all idempotent: create `/data/flatpak` (refusing to proceed if `/data`
isn't actually mounted), write a default sandbox environment
(`XDG_SESSION_TYPE=wayland`), and add the flathub remote. Only the last
needs the network, so it retries and is stamped once it succeeds — a box
that first booted without an uplink picks it up on a later boot rather
than needing operator action.

Two measured findings worth keeping in view:

- **`flatpak override` cannot target a named installation.** On flatpak
  1.16.6, in both documented option positions, it exits 0, prints no
  warning, reads the value straight back with `--show` — and writes to
  `/var/lib/flatpak/overrides/global`, which no app under `/data` reads.
  The setup script therefore writes `/data/flatpak/overrides/global`
  itself, in flatpak's own file format.
- **Wayland is not auto-selected by Chromium-family apps.** With
  `WAYLAND_DISPLAY` already exported, Flathub's Chromium still picked the
  X11 ozone backend and failed; it needs an explicit
  `--ozone-platform=wayland`. That is argv, and no environment variable
  expresses it, so the durable fix belongs to whatever launches the app
  (a per-app-id argv policy in DuDuClaw's own launcher) — not to an edited
  `.desktop` file inside the app, which the next `flatpak update` deletes.

**Size**: the three packages cost **113.5 MB** installed *on top of this
image's existing package set* (86 new packages, no recommends — measured
by diffing the apt dependency closure with and without them, not
estimated). The 2.4 GB figure above is app/runtime content and lands on
`/data`.

## Known open points

Verified against upstream documentation where noted inline in each file;
these specific points were **not** independently confirmed by an actual
build or boot this round (per the current task scope: recipe + scripts
only, no live image build) and are the first things worth checking once a
real Linux build environment is available:

- **Non-verity root=PARTUUID auto-wiring.** mkosi's docs confirm it
  auto-embeds the root partition's roothash into UKI cmdlines for the
  dm-verity case; the same behavior for a plain (non-verity) root
  partition built via `mkosi.repart/` is a reasonable reading of the same
  general mechanism, not a verbatim-quoted confirmation.
- **`systemd-repart` re-run against a live, already-populated root-A
  partition.** Reasoned to be a safe no-op (repart resizes/creates,
  doesn't recreate an already-matching partition — see the comment in
  `duduclaw-firstboot-repart.sh`), but not observed in an actual run.
- **machine-id stability across reboots on a read-only root.** The
  persistence approach in `duduclaw-firstboot-provision.sh` copies
  whatever ID the current boot has to `/data`; whether systemd's own
  early machine-id generation runs too early in boot for any regular unit
  to intercept and override it on *subsequent* boots (making every boot
  get a fresh transient ID instead of the persisted one) is flagged
  in-line as a real open question, not assumed solved.
- **A/B boot-counting ↔ `systemd-sysupdate` interaction.** Both pieces
  (boot-loader-spec tries-counter, `systemd-bless-boot.service`) exist
  upstream and are referenced in `mkosi.extra/etc/sysupdate.d/`, but the
  exact mechanics of a sysupdate-written UKI picking up the counting
  suffix weren't traced end-to-end.
- **OVMF/edk2 firmware paths in `smoke-qemu.sh`.** The macOS/Homebrew
  candidates (both x86-64 and arm64) were confirmed by actually running
  `brew install qemu` and listing `$(brew --prefix qemu)/share/qemu/`
  (qemu 11.1.0, arm64_tahoe bottle, 2026-08) — those specific filenames
  are real, not guessed. The Debian/apt candidates (`/usr/share/OVMF/*`
  for x86-64, `/usr/share/AAVMF/*` for arm64) are still best-effort:
  several candidate paths are tried since exact filenames vary by
  distro/package version, and the Linux ones specifically weren't
  independently confirmed this round.
- **`mkosi.repart/` partition types are x86-64-only** (`Type=root-x86-64`
  in `20-root-a.conf`/`21-root-b.conf`; `mkosi.extra/etc/sysupdate.d/
  10-duduclaw-root.conf` matches the same literal string). Booting under
  QEMU still works for `APPLIANCE_ARCH=arm64` because mkosi wires
  `root=PARTUUID=<uuid>` directly into the UKI cmdline (root mounting
  doesn't depend on the partition *type* GUID matching the running
  architecture) — but the type label itself is cosmetically wrong for an
  arm64-built image, and `systemd-sysupdate`/discoverable-partition
  tooling that keys off the Discoverable Partitions Spec's per-arch type
  GUIDs would not treat an arm64 image's root partition as
  architecture-correct. Not fixed this round because `APPLIANCE_ARCH=arm64`
  is explicitly a local QEMU-smoke-test path, not a second shipping
  target with its own A/B update story — revisit if that ever changes.
- **`SSH disabled by default` via system-preset.** The skeleton-tree
  ordering this depends on (preset file must exist before
  `openssh-server`'s postinst runs) is documented mkosi behavior, but the
  resulting preset outcome wasn't observed against a real Debian
  `openssh-server` install.
- **`ExtraTrees=mkosi.repart:/usr/lib/repart.d` composing correctly**
  alongside mkosi's own auto-discovered `mkosi.extra/` tree (both are
  `ExtraTrees=` entries, expected to append per mkosi's list-setting
  semantics) — not exercised by an actual build.
- **Kiosk display session, end-to-end.** Every individual piece (package
  names, the `/sys/class/drm/*/status` values, the `seatd -g video` /
  render-node-group requirement, `ExecCondition=`'s skip-not-fail
  semantics, the `cage`/Chromium flag strings) is verified against
  upstream or Debian source this round — see
  `mkosi.extra/etc/systemd/system/duduclaw-kiosk.service`'s own comment
  block for the citations — but the pieces have never been run together
  against real DRM/KMS hardware. Specific sub-points still open:
  - `seatd`'s own VT-allocation behavior beyond the one explicit
    `Conflicts=getty@tty1.service` handled here (e.g. interaction with an
    autovt unit on some *other* VT) wasn't traced end-to-end.
  - Chromium's own sandbox (unprivileged user namespaces) running under
    this unit's `NoNewPrivileges=yes` — expected to be compatible (the
    namespace sandbox doesn't need the legacy SUID-sandbox privilege
    escalation `NoNewPrivileges=` blocks), not confirmed by an actual run.
  - Whether Debian's `chromium` package auto-detects a Wayland session
    without `--ozone-platform=wayland` was not tested either way; the flag
    is passed explicitly so the outcome doesn't depend on that
    auto-detection succeeding.
  - The Chromium enterprise policy file
    (`mkosi.extra/etc/chromium/policies/managed/duduclaw-kiosk.json`) has
    its path and key name verified against Chromium source
    (`policy_paths.cc`, `config_dir_policy_loader.cc`,
    `translate_url_fetcher.cc`'s own traffic-annotation example), but
    Chromium actually picking it up was not observed on a real boot.

- **`duduclaw-comp` as the session compositor, on real hardware.** The
  selection + fallback decision tree in `duduclaw-kiosk-launch.sh` is
  verified — 31 assertions, in a Debian trixie container against stub
  binaries covering comp-dies-instantly, the three-strike blacklist, all
  three escape-hatch tokens, a junk token, a healthy comp handing a real
  Unix socket to its client, a missing binary, and the no-shell path — but
  a *stub* is not a compositor. Two things it therefore cannot prove:
  - `duduclaw-comp`'s own DRM/udev backend. That is being implemented in
    parallel (A4-1); this wiring assumes the contract "no `WAYLAND_DISPLAY`
    in the environment ⇒ take the hardware", with `DUDUCLAW_COMP_BACKEND`
    passed explicitly on top so the target is never implicit. A comp build
    that ignores that variable still gets the same path from the unset
    `WAYLAND_DISPLAY`; a comp build that wants a *different* variable name
    needs one line changed in `run_comp_session`.
  - Whether the compositor's socket appears where the launcher looks
    (`$XDG_RUNTIME_DIR/wayland-*`, matched by "is a socket" rather than by
    name, so a rename can't silently break it) once the winit backend is
    no longer the one creating it.
- **`dbus-run-session`'s package provenance.** The `dbus` package is
  already installed and is expected to pull the binary in transitively,
  but which Debian trixie binary package actually ships
  `/usr/bin/dbus-run-session` was not confirmed. The launcher degrades
  cleanly (no session bus, journal line, everything else unchanged) rather
  than failing, so the first boot's journal answers this; if it is absent,
  add `dbus-bin` to `mkosi.conf`'s `Packages=`.
- **Input devices under `duduclaw-comp`.** The kiosk user is deliberately
  *not* in the `input` group, because a libseat-based compositor never
  opens `/dev/input/event*` itself — seatd does, exactly as it already
  does for `cage` on this image. If a comp build reaches libinput without
  going through libseat it will see zero keyboards and zero pointers; the
  fix is `input` in the `useradd -G` list in
  `postinst.d/20-users-and-units.sh`. Recorded so the first failing boot
  is one step, not five.
- **Root partition headroom is arithmetic, not a measurement.** The "~3.6 GB
  used of 5 GB" baseline comes from `mkosi.repart/20-root-a.conf`'s own
  comment, not from `df` on a built image. Against it, the A4 additions are
  113.5 MB (flatpak trio, measured) plus the `duduclaw-comp` binary
  (~146 MB as an unstripped debug build, materially less stripped or built
  release) — so no partition resize, with roughly 1 GB still free. If a
  build ever dies with "No space left on device while populating file
  system", the levers in order are: `APPLIANCE_STRIP` (on by default —
  check the build log actually reported a strip rather than a warning),
  handing in a release build of the two binaries, then raising `SizeMinBytes=`
  /`SizeMaxBytes=` in **both** `20-root-a.conf` and `21-root-b.conf`
  together (A/B slots must stay identical for sysupdate).

None of these are silently assumed solved; each is called out at its
source in the relevant file's comments as well, so nothing here is a
surprise if you go read the recipe itself.

## Explicitly out of scope for this recipe

- Actually building or shipping an image (this round is recipe + scripts
  only — see the task notes above).
- Secure Boot signing / dm-verity root integrity (read-only mount + GPT
  read-only attribute is the current integrity story).
- Wi-Fi provisioning (wired DHCP only).
- Kiosk hot-plug re-detection (a display attached after boot needs a
  restart of `duduclaw-kiosk.service` to be picked up — see "Kiosk display
  session" above; the detection itself, gated on boot, is implemented).
- The real update-channel infrastructure `mkosi.extra/etc/sysupdate.d/`
  transfers assume (they're written for a local staging directory today,
  not a signed remote release feed).
