# DuDuClaw OS hardware requirements and compatibility guide

DuDuClaw OS is an **x86-64 Agent-Native operating system** that runs on ordinary consumer PCs, DIY builds, or mini-PCs — no special hardware, no discrete GPU required. Most mainstream x86 machines from 2015 onward can run it.

This document splits hardware into **two layers**. This is the most important frame for understanding the whole document — don't conflate them:

1. **Hardware that can run DuDuClaw OS itself** — x86-64 + AVX2 (x86-64-v3) + UEFI + SSD. Consumer mini-PCs, DIY builds, and even most x86 laptops qualify if they meet these conditions.
2. **Hardware that can't run the OS, but has a clear role in the DuDuClaw ecosystem** — ARM SBCs like the Raspberry Pi, and microcontrollers (MCUs) like ESP32/Arduino. Their role is as **sensor endpoints**: they feed data to an agent running on x86 hardware through DuDuClaw's resident sensing (`[[tick.sources]]`), not to run the OS itself.

Start with Hard requirements (miss one and it won't boot), then check the sizing table or compatibility checklist for your use case.

## Table of contents

- [Hard requirements](#hard-requirements-three-conditions-or-it-wont-boot)
- [Sizing table](#sizing-table)
- [DIY PC compatibility checklist](#diy-pc-compatibility-checklist)
- [Evaluating x86 laptops](#evaluating-x86-laptops)
- [Recommended consumer hardware](#recommended-consumer-hardware-buy-these-directly)
- [Known driver gaps](#known-driver-gaps-avoid-these-when-choosing-hardware)
- [Non-x86 hardware and IoT endpoints (Raspberry Pi / Arduino / ESP32)](#non-x86-hardware-and-iot-endpoints-raspberry-pi--arduino--esp32)
- [Two-layer matrix](#two-layer-matrix)
- [Why you can't validate on Mac / ARM emulation](#why-you-cant-validate-on-mac--arm-emulation)
- [The right way to flash boot media](#the-right-way-to-flash-boot-media)

## Hard requirements (three conditions, or it won't boot)

1. **x86-64 CPU with AVX2 support** (= the x86-64-v3 baseline: AVX, AVX2, BMI1, BMI2, F16C, FMA, LZCNT, MOVBE, OSXSAVE)
   - Intel: Haswell (2013, 4th-gen Core) or newer. The Atom-derived Celeron/Pentium N series doesn't get there until **Gracemont** (Alder Lake-N, e.g. N100/N200/N305, 2023 onward) — the earlier Goldmont/Goldmont Plus/Tremont generations don't necessarily have AVX2 (see the DIY PC compatibility checklist below for details and how to check).
   - AMD: starting with **Excavator** (2015, mobile APUs), then Zen/Zen+/Zen 2/Zen 3/Zen 4/Zen 5 (2017 onward), all supported. The earlier Bulldozer/Piledriver/Steamroller generations (FX-series desktops, some APUs, 2011–2014) **don't support** AVX2/BMI2, even though they're nominally "x86-64" too.
   - ❌ **arm64 isn't supported**: this includes Apple Silicon Mac, Raspberry Pi, and ARM mini-PCs. The OS image is compiled for x86-64, so an ARM machine (or UTM/QEMU emulation on an ARM Mac) either won't boot or will be painfully slow — that's not a supported path.
2. **UEFI boot** (present on nearly every motherboard since 2012)
   - A/B atomic updates plus systemd-boot depend on UEFI; a traditional BIOS-only machine isn't supported.
   - Secure Boot can be off for now (the self-signed chain isn't enforced yet).
3. **SSD** (A/B updates and the gpui desktop are both IO-sensitive; an HDD will feel sluggish)

## Sizing table

| Item | Minimum | Recommended | Comfortable (gaming / local LLM) |
|---|---|---|---|
| **CPU** | x86-64-v3 (needs AVX2): Intel Haswell or newer / AMD Excavator or newer | Intel N100 / N305, AMD Ryzen 5 | AMD Ryzen 7 8845HS |
| **Memory** | 4 GB | 8 GB | 16 GB+ |
| **Storage** | 64 GB SSD | 128 GB SSD | 256 GB+ SSD |
| **Display / GPU** | No discrete GPU needed (integrated graphics is enough) | Integrated graphics | Integrated / discrete graphics |
| **Firmware** | UEFI | UEFI | UEFI |
| **Network** | Wired or Wi-Fi (iwd) | — | — |

> **Why no discrete GPU is needed.** DuDuClaw OS currently runs on **llvmpipe software rendering** (the CPU draws the desktop), which is why it's so friendly to consumer hardware — even the cheapest mini-PC without a discrete GPU can run the full graphical desktop. Integrated or discrete graphics just leave room for hardware acceleration later; they aren't required.

## DIY PC compatibility checklist

A DIY build just needs to meet the hard requirements above — there's no "certified list." But real-world builds run into a few common pitfalls. Check each of these:

| Check | Pass condition | Common pitfall |
|---|---|---|
| **Motherboard / chipset** | Intel 8-series chipset (LGA1150, matching Haswell) or newer, or AMD AM4 / AM5 (2017 onward); the board itself needs UEFI | Ancient BIOS-only boards (pre-2012) aren't supported, full stop |
| **CPU** | See the Intel/AMD generation list in Hard requirements above | Secondhand AMD FX chips (Bulldozer/Piledriver family), and the low-end Atom/Celeron J series common in old NAS boxes (Bay Trail/Cherry Trail/Goldmont generations), may lack AVX2 — the machine won't boot even though it's technically "x86-64." **Check the `avx2` flag with `lscpu` or CPU-Z before buying**, don't go by CPU brand or release year alone |
| **GPU** | No discrete GPU needed (llvmpipe software rendering) | A discrete GPU (NVIDIA/AMD/Intel Arc) won't cause problems, but it isn't used for acceleration automatically yet — the room is just reserved for later |
| **Wired NIC** | Intel (I219/I225/I226, etc.) and mainstream Realtek chips (RTL8111/8168 family) have mature Linux support | **RTL8125 (2.5GbE)**: the in-tree `r8169` driver has basic support merged, but the community has repeatedly reported it's less stable than Realtek's own out-of-tree `r8125` DKMS module ([Arch Linux forum](https://bbs.archlinux.org/viewtopic.php?id=262120)); avoid it when choosing hardware, or have the DKMS module ready |
| **Wi-Fi NIC** | Intel AX series and MediaTek `mt7921`/`mt7925` series have good mainline support | **MT7927 (Wi-Fi 7 chip)**: as of the check date it still has **no mainline driver** — `mt7925e` explicitly refuses to bind its PCI ID, and the community relies on out-of-tree packages like `mediatek-mt7927-dkms` to fill the gap ([jetm blog](https://jetm.github.io/blog/posts/mt7927-wifi-the-missing-piece/)); confirm the actual M.2 card chip with `lspci -nn` before buying, don't trust the motherboard spec sheet alone |
| **Storage** | NVMe or SATA SSD both work | An HDD will feel sluggish (A/B updates and the desktop are both IO-sensitive) — not recommended |
| **Boot mode** | The "Boot Mode" setting in the board's BIOS/UEFI must be **UEFI** (not CSM/Legacy) | Many boards ship with CSM compatibility mode on by default; switch to pure UEFI manually. "Fast Boot"/"Ultra Fast Boot" options can skip the USB boot menu — turn them off before flashing a USB boot drive |
| **Secure Boot** | On or off both work | The self-signed chain isn't enforced yet, but some boards enable it by default — if boot fails, turn it off in the BIOS first to rule it out as the cause |

## Evaluating x86 laptops

Technically, any laptop that meets the hard requirements (x86-64 + AVX2 + UEFI + SSD) can run DuDuClaw OS — but **DuDuClaw OS hasn't been specifically compatibility-tested on laptops** (the appliance project's target hardware is desktop mini-PCs; see the recommended hardware below). The points below are general known risk areas for Linux laptop support, not DuDuClaw test results:

- **Power management / ACPI**: battery life, fan curves, and lid-close suspend often need vendor-specific ACPI patches. Brands with strong community support (ThinkPad, Tuxedo) carry lower risk; gaming and ultra-thin models carry more.
- **Wi-Fi / Bluetooth**: laptops often use MediaTek/Qualcomm/Realtek modules different from the M.2 cards used in desktops, and can hit the same kind of mainline driver gap as MT7927. Always confirm with `lspci -nn` after booting.
- **Trackpad**: modern Precision Touchpads (I2C HID) have mature support in recent kernels. Older PS/2-emulated trackpads may have incomplete gesture and multi-touch support.
- **Secure Boot**: most OEM laptops ship with it on by default; you need to go into the BIOS/UEFI and turn it off to boot from USB and install.
- **Touchscreens, fingerprint readers, and HDR panels** are outside what DuDuClaw OS is designed for, and aren't guaranteed to work.

Bottom line: running DuDuClaw OS on a laptop is technically possible, but it's on you to verify — boot from USB first and try it out before overwriting your only system drive.

## Recommended consumer hardware (buy these directly)

| Tier | Example hardware | Notes |
|---|---|---|
| 💰 Budget | Intel N100 mini-PC (Beelink / GMKtec, roughly NT$4,000–6,000) | Fanless, low power — enough for desktop use, office work, and browsing |
| ⚖️ Balanced | Beelink SER series (Ryzen), Intel NUC | Good balance of performance and price |
| 🎯 Target hardware | Intel N305 / AMD Ryzen 8845HS mini-PC | The project's target hardware — handles gaming / local LLM workloads more comfortably |

## Known driver gaps (avoid these when choosing hardware)

The hardware below currently has **insufficient driver support**. Avoid it when choosing hardware, or keep a compatible NIC on hand as a backup:

- **MT7927 Wi-Fi 7 chip** — as of the check date, still no mainline driver. `mt7925e` refuses to bind its PCI ID, and the only workaround is to have a community DKMS package ready ([jetm blog](https://jetm.github.io/blog/posts/mt7927-wifi-the-missing-piece/), [GitHub](https://github.com/danmeedev/mt7927-bazzite)). This has nothing to do with whether you maintain your own kernel — even a stock distro package won't pick it up, because upstream hasn't written the driver yet.
- **RTL8125 2.5G wired NIC** — the in-tree `r8169` driver has basic support merged ([LKML](https://lkml.kernel.org/netdev/de076b11-2523-4116-ec08-b7e331497509@gmail.com/)), but several community reports say it's less stable than Realtek's own out-of-tree `r8125` DKMS ([Arch forum](https://bbs.archlinux.org/viewtopic.php?id=262120)). The exact kernel version where support landed, and how big the current stability gap actually is, **couldn't be traced to a credible primary source** — flagged as unverified, needs testing on real hardware.

Before buying, check which network chip the motherboard or mini-PC actually uses (`lspci -nn`), avoid the two chips above, or at least go in knowing you'll need to install a DKMS module.

## Non-x86 hardware and IoT endpoints (Raspberry Pi / Arduino / ESP32)

None of the hardware in this section **can run DuDuClaw OS itself**. That doesn't mean it's useless — it plays a clear second-layer role in the DuDuClaw ecosystem: **sensor endpoint**.

### Raspberry Pi

**Why it can't run DuDuClaw OS**: the Raspberry Pi 4 uses a Cortex-A72 and the Pi 5 uses a Cortex-A76 — both 64-bit **ARMv8-A (aarch64/arm64) architecture**, not x86-64 ([Raspberry Pi 5 specs, cross-checked against Wikipedia](https://en.wikipedia.org/wiki/Raspberry_Pi): quad-core Cortex-A76 @ 2.4GHz, with 1/2/4/8/16GB RAM options). The DuDuClaw OS image is compiled for x86-64 only. This is an **instruction-set incompatibility**, not a missing driver — the same reason you can't boot it on an Apple Silicon Mac (see the section below). No software update fixes it; the only path is compiling a separate ARM64 image.

The Raspberry Pi's boot mechanism also differs from an x86 PC: by default it uses Broadcom's closed-source GPU firmware plus a device-tree boot flow, and has **no built-in UEFI like a standard PC** (the Pi 4/5 can run an optional Raspberry Pi UEFI firmware, but that's a separate project, not out of the box). This matters when weighing whether to ship an ARM64 build in the future.

**Cost of a future ARM64 build** (an honest list of the work involved, no made-up time estimates):

- This isn't starting from zero. DuDuClaw's old (now-frozen) Debian/mkosi appliance build pipeline already had an `APPLIANCE_ARCH=arm64` "smoke-build" path (`appliance/mkosi.conf.d/10-arch-arm64.conf`). But that path's stated purpose was "local build on Apple Silicon plus a QEMU smoke test" — the file itself notes "x86-64 remains the shipping target." In other words, it used a **generic arm64 Debian kernel** and was never boot-verified on real ARM hardware, let alone a Raspberry Pi. That's a different thing from "supports the Raspberry Pi."
- The current main build pipeline (Yocto / `meta-duduclaw`) only has x86-64 machine configs (`duduclaw-genericx86-64.conf` / `duduclaw-qemux86-64.conf`) — no arm64 or Raspberry Pi-specific machine definition exists yet.
- Actually supporting the Raspberry Pi would break down roughly into these pieces: (1) a whole new Yocto machine BSP — the Yocto ecosystem already has an upstream BSP layer like `meta-raspberrypi` to start from, but it still needs integrating into this project's image and update pipeline; (2) a Raspberry Pi-specific boot chain — no standard UEFI, so it needs either optional Pi UEFI firmware or a switch to U-Boot, and the existing UEFI + systemd-boot A/B dual-slot update design would need re-validating for that path; (3) the graphics stack (currently llvmpipe software rendering on x86-64) needs the whole chain re-validated on aarch64; (4) x86-64-v3 CPU tuning doesn't apply to ARM at all, so a separate tune/sstate cache is needed, which doubles the build matrix; (5) the Steam/gaming use case the "comfortable" tier is built around has no official support on ARM Linux, so that feature would effectively have to be dropped; (6) the Raspberry Pi's Broadcom display/Wi-Fi firmware and drivers need separate investigation and integration.
- Overall this is "add a whole new platform target" scale of work, not "add a compile flag" scale. There's no reliable basis for a precise time estimate, so this deliberately lists the work items without making up numbers.

**The Raspberry Pi's actual role in the DuDuClaw ecosystem**: run a standard Linux (Raspberry Pi OS, Debian arm64, etc.) as an **edge agent node or IoT gateway**. Write a small service on the Pi that reads sensor/GPIO/camera state and exposes it over HTTP, so the DuDuClaw gateway running on x86 hardware can pull the data in through `[[tick.sources]]` (the connection method is the same as "How sensor endpoints connect into resident sensing" below, including the LAN private-IP restriction). It also works well as an aggregation node for several Arduino/ESP32 devices talking over short-range protocols like BLE/LoRa/Zigbee — one endpoint out, fewer sources for DuDuClaw to manage.

### Arduino / ESP32

**Why these are microcontrollers (MCUs), not computers that can run an OS**:

- **Arduino Uno**: built around the ATmega328P, an **8-bit** MCU with **2 KB of SRAM**, 32 KB of flash, and a 16 MHz clock ([Arduino's own docs](https://docs.arduino.cc/hardware/uno-rev3/)). It's a pure microcontroller — running even a stripped-down Linux is out of the question. That's a **resource** problem, not an "incompatible architecture" problem: even if you swapped in an x86 instruction set, 2 KB of memory still couldn't run any modern operating system.
- **ESP32**: Xtensa LX6 (some newer variants use LX7 or RISC-V), mostly dual-core at 240MHz, with roughly 256–768 KiB of built-in SRAM depending on model (520 KiB on the original ESP32), and **no MMU** ([Wikipedia, ESP32](https://en.wikipedia.org/wiki/ESP32)). Natively it only runs FreeRTOS or bare metal (the ESP-IDF SDK) — no Linux. Even newer variants with PSRAM expanding memory to a few MB fall well short of the memory footprint and MMU-based virtual memory management a modern Linux desktop needs.

Both belong in the same role: **sensor endpoint** — send readings out over Wi-Fi (native on the ESP32) or an add-on network module (Arduino commonly paired with an ESP8266/ESP32 shield), and let a device higher up the stack (a Raspberry Pi or an x86 host) pull or receive them.

### How sensor endpoints connect into DuDuClaw's resident sensing

One architectural limit is easy to misunderstand, so it's worth stating clearly — this comes **straight from the code** (`crates/duduclaw-gateway/src/tick_config.rs` / `tick_source.rs` / `tick_source_poll.rs` / `tick_source_ws.rs`), not from memory:

1. **Both `http_poll` and `websocket` (non-loopback) go through the `web_fetch::validate_url` SSRF gate, which rejects private-range IPs (`192.168.x.x`, `10.x.x.x`, `169.254.x.x`, etc.) outright.** The `tick_config.rs` test `ssrf_urls_disable_the_source` explicitly verifies that an address like `http://192.168.1.10/x` gets rejected. That means **the DuDuClaw gateway can't `http_poll` an ESP32 sitting on a home or office LAN directly** — this is a deliberate security boundary, not a bug or a gap.
2. **The `websocket` tick kind is a WS client that dials out** — it's not a WS server (see `connect_source` in `tick_source_ws.rs`). So the intuitive description "the ESP32 pushes to DuDuClaw" isn't quite right. The accurate version is: "DuDuClaw dials into a WS server the ESP32 (or some relay) opens, and once connected, the other side decides when to push a frame." And a non-loopback `wss://` still goes through the same SSRF gate above, which still blocks LAN private IPs.

So there are, in practice, **three legitimate ways to connect an IoT device on the LAN**:

1. **`command`** (needs `[tick] allow_command_sources = true` set in `config.toml` first — off and fail-closed by default): the operator writes their own argv (a `curl` call, or a script that hits the sensor API) that reaches the LAN device. This subprocess's network calls **don't go through** the tick_source's own SSRF gate (that gate only applies to the `http_poll` branch), so it can legitimately reach private IPs. This is currently the most direct way to connect a LAN sensor.
2. **`file_tail`**: set up a separate cron/systemd timer script on the machine running the DuDuClaw gateway. It polls the LAN device's API on a schedule and appends each result as a line of JSON to a local log file. `file_tail` only reads a file and never touches the network, so it's naturally outside the SSRF gate.
3. **`websocket` plus a local relay (loopback)**: run a small relay service on the same host as the gateway (it subscribes to the ESP32's MQTT publishes, or polls it directly). The relay opens its own WS server at `ws://127.0.0.1:PORT`, and DuDuClaw's `websocket` tick source connects to that loopback address. This is exactly the "documented local-relay path" the code comments describe as the intended design.

**A worked example: ESP32 temperature/humidity sensor → tick → autopilot reaction**

1. The ESP32 is wired to a DHT22, and its firmware runs a minimal HTTP server; `GET /sensor` returns `{"temp":32.1,"humidity":58}`.
2. A cron script on the gateway host (running once a minute) hits this API through either the `command` or `file_tail` path. With `file_tail`:

   ```bash
   # cron: append the reading to the log every minute
   curl -s http://192.168.1.50/sensor >> /var/log/duduclaw/esp32-temp.jsonl
   echo >> /var/log/duduclaw/esp32-temp.jsonl
   ```

3. `config.toml` setup:

   ```toml
   [tick]
   enabled = true

   [[tick.sources]]
   id = "esp32-temp"
   kind = "file_tail"
   path = "/var/log/duduclaw/esp32-temp.jsonl"
   json_fields = { temp = "/temp", humidity = "/humidity" }
   ```

   (If you'd rather have DuDuClaw pull the data itself and skip the external cron job, use `command` instead: `kind = "command"`, `command = ["curl", "-s", "http://192.168.1.50/sensor"]`, `interval_secs = 60`, and add `allow_command_sources = true` under `[tick]`.)

4. Every time a new line arrives, DuDuClaw adds `prev_temp` / `delta_temp` / `pct_temp` to `{temp, humidity}` automatically and broadcasts it as `AutopilotEvent::Tick{source:"esp32-temp", fields:{...}}` (handled entirely on the Rust side, at zero LLM cost).
5. An autopilot rule can then be written directly:

   ```json
   {
     "trigger_event": "tick",
     "conditions": {
       "all": [
         { "field": "source", "op": "eq", "value": "esp32-temp" },
         { "field": "temp", "op": "gt", "value": 30 }
       ]
     },
     "action": {
       "type": "notify",
       "channel": "telegram",
       "chat_id": "...",
       "text": "機房溫度 {temp}°C 超過 30 度，請檢查空調"
     }
   }
   ```

When the temperature crosses the threshold, the agent sends a notification automatically — this is exactly the design philosophy behind resident sensing: System 1 (cheap, always-on, deterministic Rust rules) does the filtering, and System 2 (the cloud agent) only wakes up when a rule fires. The LLM isn't triggered until judgment is actually needed.

## Two-layer matrix

### Layer 1: hardware that can run DuDuClaw OS itself (x86-64 hardware)

| Hardware class | Support status | Role in the ecosystem |
|---|---|---|
| DIY x86 PC (Intel Haswell or newer / AMD Excavator or newer, UEFI, SSD) | ✅ Fully supported | Host / runs the OS itself |
| x86 consumer mini-PC (N100 / N305 / 8845HS, etc.) | ✅ Fully supported, recommended tier | Host / runs the OS itself |
| x86 laptop (meets the hard requirements) | ⚠️ Works in theory, not specifically validated | Host / runs the OS itself (at your own risk) |
| Old AMD FX/Bulldozer family, old Atom (Bay Trail/Cherry Trail/Goldmont generations) | ❌ Not supported (no AVX2) | N/A |
| Apple Silicon Mac (native) | ❌ Not supported (ARM architecture) | N/A — can only run the gateway as a dev machine (not the OS itself) |
| Apple Silicon Mac + QEMU/UTM emulating x86 | ⚠️ Boots technically, but extremely slow | Not recommended for real use — good only for a quick look at the boot screen |

### Layer 2: peripherals / sensor endpoints (can't run the OS itself)

| Hardware class | Support status | Role in the ecosystem |
|---|---|---|
| Raspberry Pi 4 / 5 (ARM64 SBC) | ❌ Can't run DuDuClaw OS (incompatible ARM architecture) | ✅ Works as an edge agent node / IoT gateway, feeding data through `http_poll`/`command`/`file_tail` |
| Other ARM Linux SBCs (same reasoning applies; not individually verified) | ❌ Same as above | ✅ Same as above |
| ESP32 family (Wi-Fi/BLE MCU) | ❌ Can't run any Linux (no MMU, SRAM measured in KB) | ✅ Sensor endpoint, connected through `command`/`file_tail`/loopback `websocket` relay |
| Arduino Uno / Nano and other 8-bit MCUs | ❌ Same as above (even lower resource level) | ✅ Same as above, usually needs a network module (like an ESP8266 shield) to get online |

## Why you can't validate on Mac / ARM emulation?

DuDuClaw OS is x86-64. Emulating x86-64 on an Apple Silicon Mac with UTM/QEMU is **software emulation (TCG)** — it's slow, and display and input aren't always smooth. It's fine for a quick look at the boot screen, **but not for real use**. To get the actual experience at native speed, flash it to a USB drive and boot real x86 UEFI hardware.

## The right way to flash boot media

Each release ships two artifact forms per machine, and they're flashed differently (for the artifact list, verification public key, and commands, see the [DuDuClaw-OS repo](https://github.com/zhixuli0406/DuDuClaw-OS) README's Quick Start section; verify the `.minisig` and `.sha256` before flashing):

- **Installer `.iso` (recommended)**: flash it to USB with balenaEtcher or `dd`, or burn it to a disc. Booting in UEFI mode drops you into the graphical installer wizard — pick the target SSD, install, and reboot. This is the only artifact that supports "boot from disc / Boot from ISO."
- **Whole-disk `.wic.zst`**: decompress it with `zstd -d` and write it straight to the target disk (or to a USB drive to boot it as a disk):

```bash
sudo dd if=duduclaw-os-*.wic of=/dev/rdiskN bs=4m status=progress
# Replace rdiskN with the actual target device number; confirm with diskutil list (macOS) or lsblk (Linux) first, and be careful not to pick the wrong disk
```

⚠️ **`.wic` can't be burned to a disc, and can't boot through "Boot from ISO" or QEMU's cdrom device.** When a GPT disk image goes through the disc (`/dev/sr0`) path, the kernel's `sr` driver `GENHD_FL_NO_PART` restriction means no GPT partition nodes get created for the disc, so the boot chain can't find a partition. To boot from a disc or an ISO, use the installer `.iso` — it's an ISO9660 live environment and doesn't depend on a partition table on the boot media.

## Related documents

- [appliance-build.md](appliance-build.md) — Getting and building the DuDuClaw OS image (the OS track has moved to the DuDuClaw-OS repo)
- [deployment-guide.md](deployment-guide.md) — Deployment (server side)
- [features/41-resident-sensing.md](../features/41-resident-sensing.md) — Full resident sensing feature description (the four source kinds `http_poll` / `command` / `file_tail` / `websocket`, SSRF protection, rate cap, delta derivation)
