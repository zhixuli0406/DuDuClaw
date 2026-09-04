# Building the DuDuClaw OS image

> **Moved (2026-09).** The OS image is no longer built from this repository.
> The Yocto layer, the release pipeline, and the frozen Debian/mkosi
> appliance recipe that this page used to describe all live in the
> standalone [DuDuClaw-OS](https://github.com/zhixuli0406/DuDuClaw-OS) repo.
> This page stays as the stable entry point from the platform docs and
> tells you where to go.

DuDuClaw OS is the bootable image that turns a small x86-64 PC into a
headless DuDuClaw box (see [the appliance feature overview](../features/50-duduclaw-os-appliance.md)
for what the finished product looks like from a user's side). The platform
repo you are reading holds the Rust workspace that the OS vendors as a
trimmed snapshot; the OS itself has its own repo, its own release line, and
its own changelog.

---

## 1. Get an image

Two routes:

- **Download a signed release.** Each release on
  [DuDuClaw-OS Releases](https://github.com/zhixuli0406/DuDuClaw-OS/releases)
  publishes, per machine, a whole-disk image (`duduclaw-os-<machine>-v<ver>.wic.zst`)
  and a live installer ISO (`duduclaw-os-installer-<machine>-v<ver>.iso`),
  each with a `.sha256` and a minisign `.minisig`. Verify before flashing;
  the public key and the exact commands are in that repo's README
  ("快速開始" / "Quick start") and `SECURITY.md`.
- **Build from source.** Clone DuDuClaw-OS next to this repo and follow its
  README ("從原始碼建置" / "Build from source") and
  `meta-duduclaw/README.md` ("Usage"): a Docker builder container, `kas build`,
  and the `scripts/release-os.sh build → smoke → package → publish` pipeline.
  A sibling checkout of this platform repo is needed only to refresh the
  vendored Rust snapshot.

## 2. What you are building

The shipping image is `duduclaw-image-appliance` from the `meta-duduclaw/`
Yocto layer (Yocto 6.0 "wrynose", kernel 6.18): an A/B dual-slot layout with
atomic update and rollback, a read-only root verified by dm-verity,
self-signed Secure Boot with a dual-signed UKI per slot, and the DuDuClaw
gateway + dashboard payload. Two machines are defined:
`duduclaw-qemux86-64` (QEMU bring-up target, boot-verified) and
`duduclaw-genericx86-64` (real x86-64 hardware, config-audited; a real
hardware boot is still the open validation item). The per-image roles and
the partition/boot chain are documented in the OS repo, not duplicated here.

## 3. The Debian/mkosi line

The `appliance/` recipe this page originally documented (Debian 13 +
mkosi, self-installing USB image) is **frozen**: it is kept under
`appliance/` in the DuDuClaw-OS repo as a reference and transition
artifact, is not shipped, and does not receive fixes. Its own README there
still carries the full boot sequence and open points for anyone reading the
history.

## See also

- [DuDuClaw OS Appliance](../features/50-duduclaw-os-appliance.md) — what
  the finished box looks like and does, from a user's side.
- [Hardware requirements & compatibility](hardware-requirements.md) — what
  it runs on and how to flash the boot media.
- [DuDuClaw-OS repo](https://github.com/zhixuli0406/DuDuClaw-OS) — layer,
  pipeline, changelog, and the documentation index (`docs/README.md`).
