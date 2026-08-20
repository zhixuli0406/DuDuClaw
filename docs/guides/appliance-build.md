# Building the DuDuClaw OS appliance image

This is the community build guide for the appliance recipe that lives in
[`appliance/`](../../appliance/) at the repository root — a Debian-based
disk image that turns a small x86-64 PC into a headless DuDuClaw box (see
[the appliance feature overview](../features/50-duduclaw-os-appliance.md)
for what the finished product looks like from a user's side). This guide
covers how to actually build one from source.

There is no separate "download the image" step covered here — the recipe
itself is the product of this guide, and building your own copy is how you
get to audit every line that ends up on the box before you ever flash it.

---

## 1. What you're building

One GPT disk image, produced by [mkosi](https://github.com/systemd/mkosi),
containing:

| Partition | Contents |
|---|---|
| ESP | systemd-boot + Unified Kernel Images |
| root A | Debian 13 + the `duduclaw` binary + Node/Python/Docker, read-only |
| root B | empty, same size as A — filled by the first update |
| /data | empty, grows to fill the disk — the only writable partition |

The same image is both the installer and the shipped product: write it to a
USB stick, boot a machine with an internal drive from it, and the image
installs a copy of itself onto that drive and powers off. There's no
separate "installer" build to maintain.

## 2. Prerequisites

**Linux**: `mkosi` (`apt-get install mkosi` on Debian/Ubuntu) plus Docker,
which is used to cross-compile the `duduclaw` binary itself.

**macOS**: Docker Desktop only — the whole build (including the mkosi step,
which needs loopback/mount operations macOS doesn't expose directly) runs
inside a container.

> **Docker Desktop memory.** The default VM (8GB RAM / 4 CPUs on a fresh
> install) is tight enough to OOM-kill the Rust build partway through
> linking the gateway binary — this has been hit on a real build attempt,
> not just a theoretical concern. The build script caps compile parallelism
> to work around it (see `CARGO_JOBS` below), but if you still see an OOM,
> raise Docker Desktop's VM memory allocation (Settings → Resources) to
> **12GB or more**.

**QEMU** (either OS, only needed for the boot smoke test below):
`qemu-system-x86_64` and/or `qemu-system-aarch64` plus OVMF/edk2 UEFI
firmware. On macOS, `brew install qemu` gets you both. On Debian/Ubuntu:
`apt-get install qemu-system-x86 ovmf` for x86-64, and/or
`apt-get install qemu-system-arm qemu-efi-aarch64` for arm64.

## 3. Build

```sh
appliance/build.sh                        # shipping build: x86-64 (the default)
APPLIANCE_ARCH=arm64 appliance/build.sh   # local Apple Silicon smoke build
```

There are two build paths, chosen by the `APPLIANCE_ARCH` environment
variable:

- **`x86-64`** (default, no need to set it) is the actual shipping target.
  On an amd64 Linux host this is a native, same-architecture build. On
  Apple Silicon it still produces a correct x86-64 image, but the whole
  build — Rust compile included — runs under Docker Desktop's x86-64
  emulation, which is slow and is exactly where the memory pressure above
  bites hardest.
- **`arm64`** builds a native-architecture image on an Apple Silicon Mac,
  with no cross-arch emulation anywhere in the pipeline. This exists purely
  so recipe changes can be iterated on quickly with a fast local QEMU boot
  test — it is **not** a second shipping target, and a couple of partition
  metadata fields are cosmetically wrong for it (harmless for booting under
  QEMU, since the kernel is told which partition to mount directly rather
  than discovering it by type; see Known Limitations).

Other environment variables:

| Variable | Default | Purpose |
|---|---|---|
| `DUDUCLAW_BIN_PATH` | *(unset)* | Point at an already-built Linux `duduclaw` binary (matching `APPLIANCE_ARCH`) to skip the Rust compile step entirely. |
| `CARGO_JOBS` | `2` | Caps cargo's build parallelism during the Rust compile, to avoid the Docker Desktop OOM described above. Raise it if your Docker VM has more memory to spare; `1` is the safest floor if you still see an OOM. |

The build does two things: (1) produces a Linux `duduclaw` release binary
for the target architecture (via the same Docker pipeline the project's own
release process uses for Linux binaries — there's exactly one place that
knows how to build DuDuClaw for Linux, not two), and (2) feeds that binary
into `mkosi build`, which assembles the actual disk image. Output lands in
`appliance/mkosi.output/`.

## 4. QEMU smoke test

```sh
appliance/smoke-qemu.sh                          # boots an x86-64-built image
APPLIANCE_ARCH=arm64 appliance/smoke-qemu.sh      # boots an arm64-built image
```

`APPLIANCE_ARCH` here must match whatever `build.sh` was run with — it
picks the right QEMU binary and firmware, not just a label. On an Apple
Silicon host, the arm64 path uses hardware acceleration (`-accel hvf`) and
is fast; the x86-64 path always uses software CPU emulation and is slow, no
matter which host it runs on.

The test passes when the serial console shows the OS reaching multi-user
mode and some sign the DuDuClaw gateway service was started — this is a
**boot-reachability check**, not a functional one. Confirming the gateway
actually serves a working dashboard needs a real machine (or a fuller QEMU
setup) with real model credentials, which is out of scope for this smoke
test.

## 5. Installing to real hardware

The same image built above is what you flash to a USB stick with a tool
like [balenaEtcher](https://etcher.balena.io/):

1. Flash the built `.raw` image to a USB stick.
2. Boot the target PC from that stick.
3. If the machine also has an internal drive (NVMe) present, the image
   detects this ("removable boot media + an internal disk to install onto")
   and shows a double-confirmation prompt before writing anything — an
   empty stick booted on a machine with nothing else attached does *not*
   silently wipe anything.
4. Once confirmed, it writes a copy of itself onto the internal drive and
   powers the machine off automatically.
5. Remove the USB stick and power the machine back on — this boot is the
   real, installed system, which then runs through first-boot setup (see
   [the feature overview](../features/50-duduclaw-os-appliance.md) for what
   that looks like from here).

A pre-flashed drive (as a channel partner might ship) can skip steps 1–4
entirely.

## 6. What's inside, briefly

On boot, the image: mounts its OS partition read-only, re-applies its
partition layout so `/data` grows to fill whatever space is available
beyond the fixed-size system partitions, seeds a persisted device identity
and a minimal LAN-bound configuration on first boot only, then starts the
DuDuClaw gateway and announces `duduclaw.local` on the network. A firewall
allows only the dashboard port and local discovery in from the LAN — see
the appliance feature overview's Security Design section for the full
security posture. Every one of these steps is an individually named,
individually auditable systemd unit or script under `appliance/mkosi.extra/`
— there's no hidden setup step baked into the image build itself.

## 7. Known limitations

This recipe is a young, actively-developed part of the project. Rather than
quietly hoping these hold up, they're tracked openly:

- **The x86-64 shipping image hasn't been produced end-to-end yet.**
  Building it needs a proper Linux/amd64 environment — building it *on* an
  Apple Silicon Mac works but runs the entire Rust compile and OS assembly
  under x86-64 emulation, which is where most build failures so far have
  come from (out-of-memory during linking, mainly — see the Docker Desktop
  memory note above).
- **Real-hardware validation is still pending.** What's been exercised is a
  boot-reachability check under QEMU; a full pass on certified physical
  hardware (install → first-boot setup → a real channel round-trip → an OS
  update → a forced rollback → a factory reset) hasn't happened yet.
- **A handful of specific mechanisms are documented assumptions, not
  confirmed behavior**, each called out at its source in the relevant
  file's own comments:
  - Whether the read-only root partition's machine identity actually
    survives across reboots, or whether systemd regenerates it too early
    in boot for the persistence approach to intercept.
  - The exact interaction between boot-counting (which slot gets tried
    next) and the update tool's own state.
  - A few OS-level partition metadata fields are only correct for the
    x86-64 build; the arm64 smoke-build path boots fine under QEMU anyway
    (the kernel is told which partition to mount directly, so it doesn't
    depend on that metadata), but the label itself is cosmetically wrong
    for that path.
  - UEFI firmware file paths for QEMU on Linux hosts are best-effort
    (several candidates are tried) rather than independently confirmed
    against every distro's packaging.
- **Not covered by this recipe at all, currently**: Secure Boot signing or
  dm-verity root integrity (a read-only mount is the current integrity
  story), Wi-Fi provisioning (wired networking only), and re-detecting a
  monitor plugged in after boot (the optional kiosk display session only
  activates its detection at boot time).

None of these are silently assumed away — each one is documented at its
source inside the recipe itself, so nothing here should be a surprise if
you go and read it.

## See also

- [DuDuClaw OS Appliance](../features/50-duduclaw-os-appliance.md) — what
  the finished box looks like and does, from a user's side.
- [`appliance/README.md`](../../appliance/README.md) — the recipe's own
  reference documentation: full directory layout, the complete boot
  sequence, and every open point with its supporting citations.
