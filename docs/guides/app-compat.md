# App compatibility layer: running Windows / Android apps on DuDuClaw OS

DuDuClaw OS is itself a Linux-based operating system, but plenty of people
still depend on Windows or Android apps they can't give up. The **app
compatibility layer** exists for exactly that: a set of pluggable
"compatibility layer components (runners)" that let those apps install and
run on DuDuClaw OS. The approach follows how SteamOS runs Windows games on
Linux (Proton), extended to cover Windows desktop apps and Android apps
too.

**Set expectations up front**: this isn't "any app is guaranteed to run."
Every compatibility layer component states plainly what it supports, what
it doesn't, and what's currently missing — never a vague "should work."

## What compat.d is

`compat.d` is the **registry directory** for compatibility layer
components. Each component (Bottles, Waydroid, and so on) drops a
declaration file here describing:

- which system it bridges apps in from (Windows games / Windows apps /
  Android / "connect back to your own Mac")
- how to launch it
- which tools it needs to work

Declaration files live at two tiers, and **the later one wins**:

| Tier | Location | Written by |
|---|---|---|
| Shipped | `/usr/share/duduclaw/compat.d/` | Built into the system image at the factory |
| Data | `~/.duduclaw/compat.d/` (or your configured data directory) | An override you or an operator drop in by hand |

When the same component ID exists at both tiers, the data tier wins — so
you can override or tweak a component's settings without reflashing the
system.

Third parties can hook their own compatibility layer components into the
same rules (a community-maintained Proton alternative, for example) —
DuDuClaw doesn't need to add any extra support for that.

> This release (CP-1) only does "registration and health checks":
> `duduclaw compat list` tells you which components exist and what tools
> are missing, but it doesn't yet launch an app for you with one click —
> that's later-release work.

## Seeing what compatibility layer components you have

```bash
duduclaw compat list
```

Example output:

```
相容層 runner（3 個）：

ID             名稱                             來源系統        狀態
------------------------------------------------------------------
bottles        Bottles（Windows 應用轉譯）        windows-app     ready
waydroid       Waydroid（Android 應用容器）       android         missing: waydroid, lxc-start
               → 見 docs/guides/app-compat.md 的 Waydroid 自裝入口段：...
windows-vm     Windows 應用程式（完整 Windows 虛擬機） windows-app     missing: docker, xfreerdp3
               → 尚未設定：先執行 `duduclaw compat windows-vm setup`（會走過資源門檻建議、硬體虛擬化檢查、授權責任揭露，再啟動容器）。
```

The command's `狀態` (status) column honestly reflects the current state:

- `ready`: every required tool is present, the component is usable
- `missing: <tool name>`: the component is registered, but some required
  tool is still missing (see each component's own section below)
- `malformed`: the declaration file itself is broken (usually a hand-edit
  gone wrong on the operator side) — it doesn't affect the other
  components displaying normally

Add `--json` when you need a machine-readable format, for scripting:

```bash
duduclaw compat list --json
```

## Windows desktop apps: Bottles

Bottles runs Windows apps through a Wine compatibility layer, installed
via Flathub:

```bash
flatpak install flathub com.usebottles.bottles
```

Once it's installed, `duduclaw compat list` flips `bottles`'s status from
`missing: flatpak` (or the component not installed at all) to `ready`.

You can also find Bottles directly in the desktop graphical Launcher's
可安裝 (Installable) category and click 安裝 (Install) to walk the same
confirmation flow — it shows the download size and install location — with
no command line required. That category's compatibility badge reads
"Partial" (usable, but with what's missing spelled out): Bottles itself has
been tested and confirmed working on DuDuClaw OS (a live QEMU test —
installed from Flathub, GUI opens normally, Wine ran a real Windows
executable, notepad, and drew its window). The badge isn't 已驗證
("Verified") rather than "Partial" because that tier speaks to the overall
usable range of "running Windows software with it" — the explicit
unsupported list below (recent Office, LINE, AutoCAD 2018+) still stands.

**Take Bottles' scope at face value** — it only promises:

- Older, standalone, portable-style tools
- Software rated "Silver" or above in the Wine compatibility database
  (AppDB) — older Photoshop CC, older Acrobat, that kind of thing

**Explicitly not promised to work (community testing consistently found
these unusable)**:

- Microsoft Office from the last three years (2023 onward)
- LINE for Windows
- AutoCAD 2018 and later

Even with Bottles installed, testing shows these apps are very unlikely to
work properly. If you need 100% compatibility for software like this, the
right path is an actual Windows machine — see the next section, "Windows
apps (full virtual machine)" — rather than forcing it through Bottles.

## Windows apps (full virtual machine)

Bottles only translates the Windows API, so it makes no promises at all
(see the previous section) for recent Microsoft 365, AutoCAD 2018 and
later, or accounting/ERP software that needs a printer or a USB license
dongle. That category of software needs an actual Windows machine, and
DuDuClaw OS has a separate route built in for it: spin up a full Windows
virtual machine locally and run a single Windows app through a
window-in-a-window (it looks like an ordinary app window, not a full
remote-desktop screen).

**This isn't a pre-installed feature**: DuDuClaw never ships, resells, or
manages any Windows license key, and it never pre-downloads a Windows
install image. The whole flow is guided step by step, but you trigger and
confirm every one of those steps yourself.

### Hardware requirements

| Item | Minimum | Recommended |
|---|---|---|
| Total memory on the host (the appliance) | — | 16GB or more |
| Memory reserved for the VM | 4GB | 8GB |
| VM disk space | 32GB | Depends on the app (Windows itself plus whatever you install) |

If the host's memory is below the recommended threshold,
`duduclaw compat windows-vm setup` only prints a warning — it doesn't stop
you from continuing. It's a recommendation, not a hard limit.

### Licensing disclosure

Before you run `setup`, confirm these three points — they aren't
DuDuClaw's terms, they're the real state of the licensing:

1. **You need to bring your own legitimate license for Windows 11 Pro or
   above.** This virtual machine installs actual Windows, and the
   licensing responsibility is yours — DuDuClaw doesn't provide or bundle
   a license.
2. **Home edition doesn't support RemoteApp windowed mode.** This is a
   hard requirement spelled out in the upstream WinApps project's own
   documentation, not a DuDuClaw limitation — with a Home edition license
   the app may still install, but you won't get the window-in-a-window
   experience.
3. **The OEM license that came with your computer or laptop usually
   doesn't include virtualization rights.** This is Microsoft's own
   licensing terms speaking — installing the Windows license that shipped
   with your machine into this virtual machine may violate those terms.

`setup` prints this disclosure in full and requires you to explicitly
confirm it (typing a confirmation phrase at an interactive terminal, or
passing `--yes` to signal you've read and agreed) before it continues.

### How to use it

```bash
# 1. Guided setup: resource-threshold advice -> hardware virtualization check -> license disclosure confirmation -> generate config -> start the container
duduclaw compat windows-vm setup

# Resources and Windows version are adjustable (both have defaults, so you can skip this):
duduclaw compat windows-vm setup --ram 16 --disk 128 --version 11

# Non-interactive environments (scripts/CI): --yes means you've read and agree to the licensing responsibility above
duduclaw compat windows-vm setup --yes

# 2. Check the container's status
duduclaw compat windows-vm status

# 3. Once installed, launch a Windows app in windowed mode
duduclaw compat windows-vm app winword.exe --name "Word"
```

`setup` only starts an empty Windows virtual machine container —
**the Windows install image is never pre-downloaded or bundled**; the
container triggers the download itself the first time it starts, and you
can open `http://127.0.0.1:8006` in a browser to watch the install
progress (the first install takes a while — how long depends on your
network and the Windows version you picked). Until it finishes,
`compat windows-vm app` can't connect yet.

The `app` subcommand connects through FreeRDP 3's RemoteApp (RAIL) mode,
so it needs to run inside a graphical session with an X11/XWayland display
(the same prerequisite as Bottles/Wine). The password is always passed to
the RDP client over standard input — it never shows up in command-line
arguments or a process listing.

### Pinning a Windows app to the launcher

Once a Windows app is installed, use `app-add` to pin it to the desktop
graphical Launcher, so you don't have to type a command every time:

```bash
# Pin an app (<executable> uses the same path notation the app subcommand uses once setup is done)
duduclaw compat windows-vm app-add winword.exe --name "Word"

# See what's currently pinned
duduclaw compat windows-vm app-list

# Unpin one (<executable> must match exactly what you passed to app-add — no fuzzy matching)
duduclaw compat windows-vm app-remove winword.exe
```

Once pinned, it shows up in the Launcher's "應用程式" (Apps) list as
"<name> (Windows)" within 60 seconds at most (the Launcher's background
rescan interval); clicking it runs
`duduclaw compat windows-vm app <executable>` in the background to open
the windowed app for you — the same path as typing it at the command line
yourself, just without the typing. These entries get their own 疊窗
(stacked-window) icon — two layered window shapes, representing that the
program runs inside the VM and gets projected to the desktop in
window-in-a-window mode. Since programs inside the VM have no real icon
file to draw from, every pinned entry shares this one honest placeholder
mark rather than faking a per-app icon.

**On a machine that has never run `setup`, the Launcher shows no Windows
entries at all** — this is deliberate, honest silence: the registry file
that `app-add`/`app-list` read and write
(`~/.duduclaw/windows-vm/apps.toml`) simply doesn't exist yet on a machine
where `setup` hasn't run, so the Launcher doesn't show an error or an
empty section for it — that whole category just doesn't appear.

On DuDuClaw OS the registry lives at `/data/system/windows-vm/apps.toml` instead (the image sets `DUDUCLAW_WINDOWS_VM_APPS_DIR=/data/system/windows-vm` for the gateway and for root's shell): `/data/duduclaw` is the gateway's home for the bundled AI CLIs and holds their login tokens, so it is `0700` and the kiosk shell could not read a file inside it. Only the registry moves; `compose.yaml` and the VM storage stay under `/data/duduclaw/windows-vm`.

Running `app-add` again for the same executable overwrites the existing
display name instead of creating a duplicate entry.

### Hardware virtualization (KVM) is a hard requirement

This route requires the host to support hardware virtualization (the
CPU's VT-x/AMD-V feature, with the Linux kernel's `kvm` module enabled and
loaded). `setup` checks for `/dev/kvm` before it even shows the licensing
disclosure — if it's not there, setup fails outright with an explanation
rather than falling back to software emulation (that would be too slow to
be practical, and dockur/windows itself has no such fallback anyway).

### Known limitations (this release)

- `setup`/`status` are still command-line flows (steps that need a human
  to explicitly confirm — the licensing disclosure, the resource-threshold
  advice — don't fit well into a one-shot graphical wizard popup); the
  part that makes "the app shows up in the graphical launcher with one
  click" already exists — see the previous section, "Pinning a Windows app
  to the launcher."
- Whether a Windows VM actually boots, and how good the windowed
  experience is, can only be verified on real hardware or a cloud host —
  the QEMU test environment itself typically has no nested virtualization,
  so it gets honestly blocked by the KVM check described above.
- Windows entries pinned to the launcher share one 疊窗 (stacked-window)
  origin icon, and Bottles shows a 雙瓶 (double-bottle) icon in the
  Installable list (design decision from the 2026-08-31 design review);
  real icon files for individual programs inside the VM are still
  unavailable — that's a platform limitation, not something on the to-do
  list.

## Android apps: Waydroid

Waydroid is an Android app container that lets you install and run mobile
apps (LINE, for example) on DuDuClaw OS.

### Why you have to set it up yourself

DuDuClaw OS **doesn't ship with** the Google services framework (GApps) or
ARM app translation components built in. The reason is straightforward:
those components come from an opaque source (reverse-engineered from
Windows Subsystem for Android), and DuDuClaw has no legal right to
redistribute them — so it won't, and can't, install them for you ahead of
time.

That means:

1. You need to complete **Google Play device certification** yourself
   (once per device is enough) — this is Google's own self-service flow,
   the same step you'd go through after flashing a regular Android
   device. It's nothing DuDuClaw built.
2. If the app you want only ships an ARM build and not an x86/x86_64 one,
   you'll need to add an ARM translation component yourself; the quality
   and compatibility of these components varies by source, and DuDuClaw
   can't vouch for them on your behalf.

Seeing `duduclaw compat list` report `waydroid` as missing pieces is
normal, not a broken system — it's honestly reflecting that "the
container itself is registered, but the underlying dependencies
(`waydroid`/`lxc-start`) or the certification/translation components you
need to add yourself aren't in place yet."

### Known working case

LINE has been fully verified end to end on arm64 hardware — install,
launch, scan the QR code to log in — using LINE's 副裝置 (Sub device)
mode, which keeps your phone logged in as normal. This is currently the
only case backed by actual test evidence; evaluate any other app on your
own, since this isn't a blanket guarantee.

## macOS apps: we don't do local execution

**DuDuClaw OS will never, and cannot, let you run macOS apps directly on
non-Apple hardware** — this isn't a technical choice, it's a legal line
drawn by Apple's software license terms and US case law (Apple v.
Psystar), and DuDuClaw won't cross it.

What we offer instead is a legal, practical alternative: **a built-in
remote connection tool that lets you operate your own Mac directly from
the DuDuClaw OS desktop** (the Mac needs to stay powered on and logged in
as you). Your Mac-only software still runs on your Mac — DuDuClaw OS is
just one more window onto it.

In other words: DuDuClaw OS works hand in hand with your Mac — it doesn't
replace it.
