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
   access to `cage`, which fullscreens Chromium at the loopback dashboard
   URL. See "Kiosk display session" below for the full design and its
   estimated cost.

## Kiosk display session

Off in effect on every headless box, on automatically the moment a
monitor is attached and the box (re)boots — no config flag either way, see
`mkosi.extra/etc/systemd/system/duduclaw-kiosk.service` for the detection
mechanism and the full device-access chain (`seatd` → `video`/`render`
group membership → `cage` → Chromium), each link verified against
upstream/Debian source rather than assumed.

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
