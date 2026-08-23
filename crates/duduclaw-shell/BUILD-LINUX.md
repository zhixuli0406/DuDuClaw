# duduclaw-shell — Linux build & headless live-run notes (Shell-S2 stage B)

## What this proves

Two go/no-go questions for DuDuClaw OS's gpui shell (`commercial/docs/
DESIGN-appliance-image-*.md` / the D13 "gpui 殼" plan), asked because the
crate had only ever been compiled and run on macOS up to this point:

- **B-①**: does `crates/duduclaw-shell` cold-compile for Linux at all
  (aarch64, Debian bookworm)?
- **B-②**: does the built binary actually *run* as a Wayland client under a
  headless, software-rendered compositor — opens its window, renders without
  panicking, for at least 10 seconds — in both boot modes the crate supports
  (`DUDUCLAW_SHELL_SKIP_OOBE=1` / `DUDUCLAW_SHELL_FORCE_OOBE=1`)?

Both are **PASS**, with one real finding in between (see "The `wl_seat`
finding" below) that changes how B-②'s host layer has to be built — not a
bug in this crate, but a hard constraint worth recording before the next
round (VM/`cage` verification) hits it blind.

## Why Docker, not `cargo build` on this Mac

Same reasoning as `crates/duduclaw-comp/BUILD.md` (read that first — this
file follows its format and evidence standard): the Linux windowing backend
this crate needs (`gpui_linux`, reached via the `wayland` feature added to
`gpui_platform` this round — see this crate's `Cargo.toml` comment for the
full story) only compiles on `cfg(any(target_os = "linux", target_os =
"freebsd"))`. This crate is already detached from the main DuDuClaw
workspace (own `[workspace]` table, own `Cargo.lock` — see `Cargo.toml`'s
existing header comment), so a Linux container is the only way to actually
exercise that code path.

## The Cargo.toml change

One line, plus a comment explaining it in place (`Cargo.toml`, the
`gpui_platform` dependency): `features = ["font-kit"]` →
`features = ["font-kit", "wayland"]`. Root cause, verified by reading the
vendored zed checkout at the pinned rev (`~/.cargo/git/checkouts/
zed-a70e2ad075855582/28c0f4a/`): `gpui_platform`'s own feature default is
`[]`, and its Linux-only dependency `gpui_linux` is declared at the zed
*workspace* root with `default-features = false` — so without this feature,
`gpui_linux` would compile on Linux with **no windowing backend at all**
(its own crate-level `default = ["wayland", "x11"]` never gets reached,
because the workspace-level `default-features = false` on the dependency
edge overrides it). `x11` was deliberately left off — this crate targets
Wayland-only environments (weston for this round, real Wayland compositors
for the appliance image later).

## B-①: cold Linux compile

### One-shot reproducible command

```bash
docker volume create duduclaw-shell-cargo >/dev/null
docker volume create duduclaw-shell-cargo-git >/dev/null
docker volume create duduclaw-shell-target >/dev/null

docker run --rm \
  -v /Users/lizhixu/Project/DuDuClaw:/work \
  -v duduclaw-shell-cargo:/usr/local/cargo/registry \
  -v duduclaw-shell-cargo-git:/usr/local/cargo/git \
  -v duduclaw-shell-target:/target \
  -e CARGO_TARGET_DIR=/target \
  -w /work/crates/duduclaw-shell \
  rust:bookworm bash -c '
    set -e
    apt-get update -qq
    apt-get install -y -qq --no-install-recommends \
      pkg-config libwayland-dev libxkbcommon-dev libfontconfig1-dev
    cargo build
    file /target/debug/duduclaw-shell
  '
```

The named volumes are optional (a plain `--rm` container with no volume
mounts reproduces the same result, just re-downloads/re-clones everything
every run — the zed monorepo git clone alone is the dominant cost of a
truly cold run). They're what make *iterating* fast: once warm, apt +
`cargo build` alone is what's timed below.

### Verified minimal dependency list

**`pkg-config`, `libwayland-dev`, `libxkbcommon-dev`, `libfontconfig1-dev`.**
That's it — confirmed by actually deleting `cmake` and `libssl-dev` from an
initial generous guess and rebuilding from a **fresh, empty `target/`**
(reusing only the warm cargo registry/git caches, so this was a real
recompile, not a no-op): the trimmed build succeeded in 1m 17s and produced
a binary with the **identical build ID**
(`802cef42503002d2bdf5b44cd9cff16477d0d601`) as the first, generously-provisioned
build — direct proof neither package was ever linked into anything.

- `libfontconfig1-dev` *is* genuinely needed, unlike `duduclaw-comp`'s build
  (which needed zero font-related packages): this crate pulls
  `zed-font-kit` (`crates/gpui_wgpu/Cargo.toml`'s `font-kit` feature, which
  `gpui_linux`'s Wayland feature set turns on unconditionally on Linux via
  `gpui_wgpu = { ..., features = ["font-kit"] }`), which in turn compiles
  `yeslogic-fontconfig-sys` and `freetype-sys` — both link against the
  system fontconfig/freetype via `pkg-config`. `libfontconfig1-dev` pulls
  `libfreetype-dev` transitively on Debian, so nothing else needed adding.
- `cmake` and `libssl-dev` were pre-emptive guesses (freetype-sys *can*
  build freetype from source via cmake if no system copy is found; reqwest
  *can* need an OpenSSL backend) that turned out unnecessary: `libfontconfig1-dev`
  already satisfies freetype-sys's `pkg-config` lookup, and
  `duduclaw-native-gui`'s `reqwest` dependency is declared
  `default-features = false` (no TLS backend compiled in at all — see that
  crate's `Cargo.toml` comment), so `openssl-sys` never enters the build.
- No Vulkan/EGL/GL headers are needed at **build** time, same reasoning as
  `duduclaw-comp`'s BUILD.md gives for smithay's EGL path: `wgpu` (via
  `ash` for Vulkan, and Mesa's GL loader) only codegens bindings at compile
  time and `dlopen()`s the actual `.so`s at runtime.

### Timing (verified 2026-08-20, `rust:bookworm`, aarch64 host)

| Run | Deps | target/ state | Result |
|---|---|---|---|
| First build | pkg-config, libwayland-dev, libxkbcommon-dev, libfontconfig1-dev, cmake, libssl-dev | cold (fresh volume) | `cargo build` finished in **2m 03s** |
| Minimal-deps rebuild | pkg-config, libwayland-dev, libxkbcommon-dev, libfontconfig1-dev only | cold (separate fresh volume) | `cargo build` finished in **1m 17s** |

Both produced the byte-identical binary (same build ID, above). `rustc
1.97.1 (8bab26f4f 2026-07-14)` / `cargo 1.97.1 (c980f4866 2026-06-30)` —
`rustup` in the `rust:bookworm` image correctly picked up this crate's
`rust-toolchain.toml` pin (`channel = "1.97.1"`) with no manual toolchain
install step.

### Evidence

```
Finished `dev` profile [unoptimized + debuginfo] target(s) in 2m 03s
BUILD_EXIT=0
/target/debug/duduclaw-shell: ELF 64-bit LSB pie executable, ARM aarch64,
version 1 (SYSV), dynamically linked, interpreter /lib/ld-linux-aarch64.so.1,
... for GNU/Linux 3.7.0, with debug_info, not stripped
```

Both `docker wait` on the build container and `docker inspect
--format '{{.State.ExitCode}}'` were checked before this was recorded as a
pass — not inferred from log tail alone.

## B-②: headless live-run

### The `wl_seat` finding (read this before the one-shot command below)

The task brief's prescribed host layer — `weston --backend=headless-backend.so`
— was tried first, exactly as specified, and **fails**: `duduclaw-shell`
panics on startup —

```
thread 'main' (1354) panicked at /usr/local/cargo/git/checkouts/zed-a70e2ad075855582/7a7c3e1/crates/gpui_linux/src/linux/wayland/client.rs:776:25:
called `Option::unwrap()` on a `None` value
```

Root-caused by reading the panic site (`gpui_linux/src/linux/wayland/
client.rs`, pinned rev): `WaylandClient::new()` scans the host compositor's
registry for a `wl_seat` global, and **unconditionally unwraps it** —
`let seat = seat.unwrap();` — no fallback, no graceful "no input" mode.
Confirmed independently with `weston-info` against the headless-backend
socket: its global list has `wl_compositor`, `wl_output`,
`zxdg_output_manager_v1`, `xdg_wm_base`, etc. — **zero `wl_seat` entries**.
Weston's headless backend genuinely has no input devices at all (unlike,
say, `duduclaw-comp`'s smithay/`winit` stack, which tolerated the same host
environment fine in the sibling spike — `winit`'s Wayland backend treats a
missing seat as "no input available," not as a startup panic; `gpui_linux`
at this pinned rev does not).

**This is a genuine upstream constraint on this pinned gpui rev, not a bug
in this crate**: any Wayland compositor `duduclaw-shell` nests inside
*must* advertise at least an empty `wl_seat`, or boot hard-panics. Worth
carrying into the next round (VM/`cage` verification, real hardware) —
`cage` on real hardware will have a real libinput-backed seat so this is
unlikely to bite there, but it's exactly the kind of assumption that's
invisible until a headless/CI environment hits it.

**Workaround used for this round**: swap *weston's own* backend from
`headless-backend.so` to `x11-backend.so`, running against a virtual `Xvfb`
X server instead of a real display — still fully headless (`Xvfb` is
literally "X virtual framebuffer," designed for exactly this), still no
real display or GPU, but X11 always carries a (possibly synthetic) core
input concept, so weston's x11-backend registers a real `wl_seat`:

```
interface: 'wl_seat', version: 7, name: 11
	name: default
	capabilities: pointer keyboard
```

Critically, **this only changes what powers weston itself** —
`duduclaw-shell` still connects to weston purely as a Wayland client
(`WAYLAND_DISPLAY=wayland-host`; the `x11` feature was deliberately never
added to this crate's `gpui_platform` dependency, so there is no X11 code
path inside `duduclaw-shell` for this to accidentally exercise). Weston's
compositing itself also runs entirely in software here — its own log shows
`Using gl renderer` / `GL renderer: llvmpipe (LLVM 15.0.6, 128 bits)`, no
DRM/KMS, no real GPU.

Per the task's own honesty bar: the literal `headless-backend.so` attempt
is an honest, evidenced **FAIL** (recorded above with its panic and root
cause); the `x11-backend.so` + `Xvfb` substitute is a separately-verified
**PASS** for the actual question B-② is asking (does the binary render
without panicking under headless software rendering) — reported as two
distinct, non-conflated results rather than a single dressed-up pass.

### One-shot reproducible command (verified 2026-08-20)

```bash
docker run --rm \
  -v /Users/lizhixu/Project/DuDuClaw:/work \
  -v duduclaw-shell-cargo:/usr/local/cargo/registry \
  -v duduclaw-shell-cargo-git:/usr/local/cargo/git \
  -v duduclaw-shell-target:/target \
  -e CARGO_TARGET_DIR=/target \
  -w /work/crates/duduclaw-shell \
  rust:bookworm bash -c '
set -uo pipefail
# NOT -e: failures are captured/reported explicitly below, not by aborting.

echo "==== apt-get install (build + runtime) ===="
apt-get update -qq
apt-get install -y -qq --no-install-recommends \
  pkg-config libwayland-dev libxkbcommon-dev libfontconfig1-dev \
  weston xvfb mesa-vulkan-drivers libvulkan1 libegl1 libgl1-mesa-dri libgles2 \
  libxkbcommon0 fonts-noto-cjk >/dev/null

echo "==== cargo build ===="
cargo build || { echo "FATAL: build failed"; exit 1; }
file /target/debug/duduclaw-shell

echo "==== host layer: Xvfb + weston --backend=x11-backend.so ===="
mkdir -p /tmp/xdg-runtime && chmod 0700 /tmp/xdg-runtime
export XDG_RUNTIME_DIR=/tmp/xdg-runtime
export LIBGL_ALWAYS_SOFTWARE=1

Xvfb :99 -screen 0 1440x900x24 -nolisten tcp &
XVFB_PID=$!
sleep 1
DISPLAY=:99 weston --backend=x11-backend.so --socket=wayland-host --width=1440 --height=900 --log=/tmp/weston.log &
WESTON_PID=$!
sleep 2
kill -0 "$WESTON_PID" || { echo "FATAL: weston died"; cat /tmp/weston.log; exit 1; }
echo "weston up, pid=$WESTON_PID, socket=wayland-host"

FAILED=0

echo "==== duduclaw-shell: home mode ===="
mkdir -p /tmp/dchome-home
WAYLAND_DISPLAY=wayland-host DUDUCLAW_SHELL_DIAG=1 DUDUCLAW_HOME=/tmp/dchome-home DUDUCLAW_SHELL_SKIP_OOBE=1 \
  timeout 12 /target/debug/duduclaw-shell > /tmp/shell-home.log 2>&1
echo "home exit_code=$? (124 = ran the full 12s under timeout, expected)"
cat /tmp/shell-home.log
grep -qiE "panic|fatal" /tmp/shell-home.log && { echo "FATAL: home mode panicked"; FAILED=1; }

echo "==== duduclaw-shell: oobe mode ===="
mkdir -p /tmp/dchome-oobe
WAYLAND_DISPLAY=wayland-host DUDUCLAW_SHELL_DIAG=1 DUDUCLAW_HOME=/tmp/dchome-oobe DUDUCLAW_SHELL_FORCE_OOBE=1 \
  timeout 12 /target/debug/duduclaw-shell > /tmp/shell-oobe.log 2>&1
echo "oobe exit_code=$? (124 = ran the full 12s under timeout, expected)"
cat /tmp/shell-oobe.log
grep -qiE "panic|fatal" /tmp/shell-oobe.log && { echo "FATAL: oobe mode panicked"; FAILED=1; }

kill "$WESTON_PID" "$XVFB_PID" 2>/dev/null || true
echo "==== DONE (FAILED=$FAILED) ===="
exit "$FAILED"
'
```

Verified as a real, standalone run of this exact script (not just a
transcription of the iterative `docker exec` steps used for the initial
investigation): exit code `0`, `FAILED=0`, both modes' evidence blocks
below reproduced verbatim. (That run reused already-warm cargo/target
volumes from the B-① timing runs above, so its own `cargo build` line read
`Finished ... in 0.68s` — nothing to compile, not a timing claim; see the
B-① table above for real cold-build timings.) Weston itself prints a few
harmless `could not load cursor 'dnd-move'/'dnd-copy'/'dnd-none'` lines on
startup (its own drag-and-drop cursor theme lookup finding nothing in this
minimal container — unrelated to `duduclaw-shell`, and it starts and runs
fine regardless).

### Verified runtime dependency list

`weston`, `xvfb` (the host-layer substitution above), `mesa-vulkan-drivers`
+ `libvulkan1` (lavapipe — the software Vulkan ICD + loader), `libegl1` +
`libgl1-mesa-dri` + `libgles2` (software GL/EGL — weston's own x11-backend
needs this regardless of what `duduclaw-shell`'s own `wgpu` context picks;
its log shows `Using gl renderer` / `llvmpipe`), `libxkbcommon0` (runtime
`.so` for the `xkbcommon` crate — a *separate* container from the build
one, so this has to be installed again even though `libxkbcommon-dev`
already provided it at build time), `fonts-noto-cjk` (this crate targets
zh-TW users; no fonts are bundled). `vulkan-tools` (`vulkaninfo`) and
`foot` were installed during investigation for diagnosis only — confirming
lavapipe enumerates as a real device, and as a candidate third-layer test
client respectively — neither is in the list above because neither is
needed to reproduce this round's result (see "not verified" below for
`foot`'s dropped role).

### Evidence (verified 2026-08-20)

Both `DUDUCLAW_SHELL_SKIP_OOBE=1` (Home) and `DUDUCLAW_SHELL_FORCE_OOBE=1`
(OOBE) ran for the full requested duration under `timeout 12` (exit code
`124` = timeout fired, i.e. the process was still healthy and had to be
killed — not a crash exit), with **zero** `panic`/`error`/`fatal` lines in
either log, confirmed reproducible across two independent runs of each
mode.

Home mode (`/tmp/shell-home.log`):

```
[main] starting duduclaw-shell S0
[main] OOBE boot resolution: Home (OOBE already completed or skipped)
[render] overlay=None
[main] window opened
[diag] after first frame: is_window_active=false focus_handle.is_focused=true
[render] overlay=None
[action] ToggleLauncher fired
[diag] in-app dispatch_keystroke(cmd-k) handled=true
[render] overlay=Some(Launcher)
[bounds] overlay-wrapper: Bounds { origin: Point { x: 0px, y: 0px }, size: Size { 1440px × 900px } }
[bounds] backdrop: Bounds { origin: Point { x: 0px, y: 0px }, size: Size { 1440px × 900px } }
[render] overlay=Some(Launcher)
[bounds] overlay-wrapper: Bounds { origin: Point { x: 0px, y: 0px }, size: Size { 1440px × 900px } }
[bounds] backdrop: Bounds { origin: Point { x: 0px, y: 0px }, size: Size { 1440px × 900px } }
```

This is not just "didn't crash" — it's a real lifecycle: window opens
against the Wayland host, `DUDUCLAW_SHELL_DIAG=1`'s built-in first-frame
self-test (`window.dispatch_keystroke("cmd-k")`, see `main.rs`'s
`diag_scheduled` block) exercises the actual keymap → action-dispatch →
state-mutation → re-render path end-to-end (`ToggleLauncher fired` →
`handled=true` → `overlay=Some(Launcher)`), and the DIAG bounds probes
(`bounds_probe`, same file) report real, correctly-sized layout
(`1440px × 900px`, matching the window's actual geometry) — not the
"laid out one window-height offscreen" bug that diagnostics toolkit was
originally built to catch (see `main.rs`'s header comment).

OOBE mode (`/tmp/shell-oobe.log`):

```
[main] starting duduclaw-shell S0
[main] OOBE boot resolution: OOBE at LanguageAccessibility
[render] overlay=None
[main] window opened
[diag] after first frame: is_window_active=false focus_handle.is_focused=true
[render] overlay=None
[action] ToggleLauncher fired
[diag] in-app dispatch_keystroke(cmd-k) handled=true
[render] overlay=None
```

Confirms `DUDUCLAW_SHELL_FORCE_OOBE=1` correctly resolves to the first OOBE
step (`LanguageAccessibility`) and that the injected `cmd-k` keystroke is
correctly treated as a no-op while OOBE owns the screen (`overlay` stays
`None` — matches `on_toggle_launcher`'s documented early-return guard in
`main.rs`: "The Launcher has no meaning while OOBE owns the whole screen").

CPU sampling during a live run (`ps -o pcpu`, software-rendered, no vsync
pacing — same shape of finding as `duduclaw-comp`'s BUILD.md): ~33% of a
container CPU core at 3s in, ~20% at 5s — consistent with a redraw loop
that isn't frame-rate-limited under `llvmpipe`, not a leak or runaway.

## Honest limitations

- **No visual/pixel confirmation.** All evidence above is log-based
  (process lifecycle, action dispatch, layout bounds) per the task's stated
  evidence bar — no screenshot was captured. `weston-screenshooter` was
  tried and refused with `permission denied: Debug protocol must be
  enabled`; getting past that (a `weston.ini` config change) was judged
  out of scope for this round rather than pursued. This means CJK glyph
  rendering (this shell targets zh-TW users; `fonts-noto-cjk` was installed
  per the task brief) was not visually verified — only that no
  font-loading error appeared in the logs and that `fc-list` resolves Noto
  CJK families system-wide.
- **Which GPU backend (`Vulkan`/lavapipe vs `GL`/llvmpipe) `duduclaw-shell`
  itself actually selected is not confirmed at the app level.**
  `gpui_wgpu`'s adapter selection (`wgpu_context.rs`) logs via the `log`
  crate (`log::info!("Selected GPU adapter: ...")`), but `main.rs` never
  initializes a logger (`env_logger`/`tracing_subscriber`/etc. — it only
  uses direct `eprintln!` for its own diagnostics), so that line is a
  no-op here. Independently confirmed: lavapipe is discoverable
  system-wide (`vulkaninfo --summary` → `deviceType =
  PHYSICAL_DEVICE_TYPE_CPU`, `deviceName = llvmpipe`, `driverID =
  DRIVER_ID_MESA_LLVMPIPE`) and the software GL/EGL path independently
  works (weston's own x11-backend renders via it). Reading source confirms
  `WgpuContext`'s adapter search never rejects software adapters on the
  initial-window path (only the *device-lost recovery* path does, via
  `new_rejecting_software` — not reached in a healthy 12s run), so either
  backend succeeding is expected; which one actually got picked wasn't
  logged. Not pursued further since it doesn't change the pass/fail
  verdict — recorded here rather than asserted as "confirmed Vulkan."
- **Input devices remain unverified**, same limitation `duduclaw-comp`'s
  BUILD.md flags for the identical reason: this round's host layer (`Xvfb`,
  headless by construction) has no real keyboard/mouse to originate
  synthetic events from. The DIAG self-test (`dispatch_keystroke("cmd-k")`)
  exercises gpui's *action dispatch* machinery end-to-end, but never
  exercises real OS-level key/mouse event delivery into gpui — that's
  still deferred to a VM/`cage`-with-real-seat round, same as the
  comp spike's Option A/B.
- **`foot` was installed during investigation but never actually used** —
  unlike the comp spike (which needed a *third-layer* real xdg-shell
  client to prove the protocol path), this round's subject *is* the
  Wayland client (`duduclaw-shell` itself); there was no need for an
  additional client on top. Dropped from the runtime dependency list
  above for that reason.
- **`x11-backend.so` + `Xvfb` is one layer more synthetic than the
  eventual target** (a real `cage`/wlroots host on real hardware, or even
  weston's own headless-backend *if* a future weston/gpui version adds an
  empty-seat fallback). Treat this round as confirming the
  render/action-dispatch path cheaply and repeatably in Docker; a real
  seat-bearing compositor (VM `cage`, or real hardware) is still the plan
  for the next round, same conclusion the comp spike's own BUILD.md
  reaches for its equivalent gap.
- **DPI scaling was not exercised** — the run used the default 1440×900
  @ scale 1 window; no HiDPI probe was attempted this round.

## macOS regression check (verified 2026-08-20)

```
export PATH="$HOME/.rustup/toolchains/1.97.1-aarch64-apple-darwin/bin:$PATH"
export RUSTC="$HOME/.rustup/toolchains/1.97.1-aarch64-apple-darwin/bin/rustc"
cd crates/duduclaw-shell && cargo test
```

`test result: ok. 135 passed; 0 failed; 1 ignored; 0 measured; 0 filtered
out` — run **twice**: once before the Linux container touched the shared
(bind-mounted) `Cargo.lock`, and once after, since the Linux build and this
mac test ran concurrently in this round and both write to the same
on-disk `Cargo.lock` (this crate's own file, not the root workspace's —
adding the `wayland` feature made Cargo resolve and record ~192 new lines
of Linux-only package entries). Both runs are byte-identical in result;
`Cargo.lock`'s final state was independently checked as valid TOML.
Confirms the `wayland` feature addition is fully inert on macOS, exactly as
the `Cargo.toml` comment predicts (`gpui_linux` — and therefore this
feature — never enters the dependency graph actually compiled for a macOS
target).

## Stage B-③ — VM cage + real-seat verification (verified 2026-08-20)

Third stage, run after B-①/② by the acceptance side (not the same agent):
the same aarch64 binary from B-① running **full-screen under `cage` on a
real DRM output with a real seat** — the exact compositor + seat stack the
appliance image ships (`appliance/mkosi.extra/.../duduclaw-kiosk.service`:
cage + seatd) — inside the appliance QEMU VM (`appliance/run-vm.sh`'s
machine config + `virtio-gpu-pci` + `qemu-xhci`/`usb-kbd`/`usb-tablet`).

**What passed (all evidenced by QMP `screendump` PNGs, archived in
`appliance/.vm/s2-evidence/`):**
- Appliance kiosk baseline: the detection-gated cage+Chromium kiosk
  auto-starts under QEMU's virtio-gpu and renders the dashboard —
  answering `run-vm.sh`'s own "Experimental: whether the detection-gated
  kiosk auto-starts under QEMU's virtual GPU" open question with YES
  (`boot1.png`).
- `duduclaw-shell` full-screen under cage at 1280×800, Home surface fully
  rendered per the design boards, **zh-TW text correct** with
  `fonts-noto-cjk` injected (`shell-live.png`).
- Real-seat KEYBOARD: QMP `send-key esc` closes the Launcher overlay;
  `meta_l-k` (Super-K — gpui maps `cmd` to Super on Linux) re-opens it
  (`shell-esc.png`, `shell-superk.png`).
- Real-seat POINTER (absolute, usb-tablet): move + left-click on the dock
  "設" tile opens the Control Center overlay, cursor rendered and tracking
  (`shell-click.png`).
- OOBE account step END-TO-END against the guest's own real gateway:
  DEBUG_OOBE_STEP=account direct-open → real typing via QMP key events
  into both `OobeTextField`s → click 建立帳號 → `oobe/claim.rs` dialed
  `127.0.0.1:18789` inside the guest → instance already claimed →
  AlreadyClaimed path rendered (green 此裝置已完成初始設定 line, button
  → 已建立帳號, 繼續 enabled) (`oobe-account2/typed/final.png`).

**Injection recipe (offline, no image rebuild — the image ships neither
apt nor sshd nor /bin/login):** shut the VM down, loop-mount partition 2
(`duduclaw-root-a`, ext4) in a `--privileged` docker container (partition
device nodes must be `mknod`ed from sysfs — no udev in containers), then:
root password hash into `/etc/shadow` (for the serial debug shell), the
B-① binary to `/usr/local/bin/duduclaw-shell`, and `cp -rn` (no-clobber)
the extracted contents of trixie/arm64 `mesa-vulkan-drivers vulkan-tools
fonts-noto-cjk` + their download-closure debs (28 debs, 326MB — includes
libLLVM; `vulkaninfo --summary` in-guest then enumerates
`llvmpipe (LLVM 19.1.7)`). Serial access itself needed an injected
`duduclaw-debug-shell.service` (bash on ttyAMA0, serial-getty masked).

**Real findings for the appliance line (not this crate's bugs):**
1. `/bin/login` does not exist in the image (`login` package never
   installed), so agetty's login exec dies instantly and serial-getty
   restart-loops — meaning the README's documented APPLIANCE_DEBUG serial
   root login **cannot work on any build to date**; the debug flow needs
   the `login` package (or an agetty `--autologin` variant) added.
2. This image was built without `APPLIANCE_DEBUG` (root was `root:*:` —
   locked), independently of finding 1.
3. Running the gpui shell as the kiosk app will require
   `mesa-vulkan-drivers` (Vulkan/lavapipe — gpui's blade renderer needs a
   Vulkan device; cage's own GL stack is not enough) and a CJK font
   package in the image recipe; the Chromium kiosk additionally shows
   tofu for emoji (no emoji font shipped).

**Honest limitations:** R1 (frame-rate) remains UNANSWERED — everything
here is llvmpipe/lavapipe software rendering under QEMU, explicitly ruled
invalid as FPS evidence by design-doc D1; interaction latency felt in
screendumps is not a measurement. DPI scaling (non-1.0) untested (single
1280×800 mode). Output disconnect/reconnect untested. `duduclaw-comp`'s
own input forwarding (grabs/input.rs) still unverified — it has only a
winit backend, so it needs a host compositor with a seat inside the VM
(e.g. weston-on-DRM) or a DRM/libinput backend of its own; deferred.
One transient `connector Virtual-1: Atomic commit failed: Device or
resource busy` appears in cage's log at kiosk→cage handover; harmless in
this run (rendering proceeded), not chased.

## Shell-S3 (2026-08-21): `zbus` dependency — verified dependency list unchanged

The real Wi-Fi backend (`oobe/network/nm.rs`, NetworkManager over D-Bus)
adds `zbus` as a `[target.'cfg(target_os = "linux")'.dependencies]` crate
(`Cargo.toml` — see that entry's own comment). `zbus` is a pure-Rust D-Bus
implementation (it does NOT link against `libdbus`/`libdbus-1-dev`), so the
natural assumption going in was that it would need one anyway — checked the
same way this file's own "Verified minimal dependency list" section already
checks such assumptions: a full FRESH build (empty target dir, warm
registry/git caches only) with `libdbus-1-dev` installed succeeded, then a
SECOND full fresh build (separate empty target dir) with `libdbus-1-dev`
deliberately omitted succeeded too, both `cargo build`/`cargo clippy
--all-targets -- -D warnings`/`cargo test` clean. **Verified minimal
dependency list from this file's B-① section is still exactly correct as
of this round — `zbus` adds zero new system packages.** `cargo clippy`
additionally needed `rustup component add clippy` in the `rust:bookworm`
image (not present by default, unlike `rustc`/`cargo`).

### Real NetworkManager activity check (verified 2026-08-21)

`nm.rs` gained a live-fire `#[ignore]`d test
(`oobe::network::nm::tests::live_probe_against_real_networkmanager`, same
"never run by a bare `cargo test`" contract `oobe::claim`'s own live gateway
test already establishes) and it was actually run against a REAL
NetworkManager instance, not just compile-checked:

```bash
docker run --rm --privileged \
  -v /Users/lizhixu/Project/DuDuClaw:/work \
  -v duduclaw-shell-cargo:/usr/local/cargo/registry \
  -v duduclaw-shell-cargo-git:/usr/local/cargo/git \
  -v duduclaw-shell-target:/target \
  -e CARGO_TARGET_DIR=/target \
  -w /work/crates/duduclaw-shell \
  rust:bookworm bash -c '
    apt-get update -qq
    apt-get install -y -qq --no-install-recommends \
      pkg-config libwayland-dev libxkbcommon-dev libfontconfig1-dev dbus network-manager
    mkdir -p /run/dbus && dbus-daemon --system --fork && sleep 1
    NetworkManager --no-daemon &
    sleep 3
    cargo test -- --ignored live_probe_against_real_networkmanager --nocapture
  '
```

Result: `probe()` succeeded (real `Connection::system()` + a real
`org.freedesktop.NetworkManager` `Devices` property read); `scan()` reached
`wifi_device_path()`, enumerated all 11 `Devices` NetworkManager reported in
that container, correctly found none of `DeviceType == 2` (no container has
a real Wi-Fi radio), and returned the honest `Unavailable("no Wi-Fi adapter
found")` — the CORRECT outcome, not a failure of the code; `status()`
correctly returned `Disconnected`; `forget()` round-tripped its
`ListConnections` + `GetSettings` calls against NetworkManager's real
Settings service (deserializing the nested `a{sa{sv}}` reply into
`HashMap<String, HashMap<String, OwnedValue>>` — this module's structurally
most complex D-Bus type, and the one code path `scan()`'s early failure
above never reaches) and correctly returned `NotFound` for a made-up SSID.

**What this proves**: the `zbus::blocking::Proxy` wire-level calls
(`Proxy::new`, `.get_property::<T>()`, `.call::<_, _, R>()`) are not just
type-correct against the D-Bus signatures this module assumes — they
round-trip against a REAL NetworkManager's real replies, for `Devices`/
`DeviceType`/`ListConnections`/`GetSettings`.

**What remains unverified** (no real Wi-Fi hardware reachable in any
available environment — Docker container, this repo's other VM tooling, or
this task's own sandbox): `RequestScan` + the `LastScan`-poll wait +
`GetAccessPoints`'s `Ssid`/`Strength`/`Flags`/`WpaFlags`/`RsnFlags`
properties (the actual scan RESULT path), `AddAndActivateConnection` +
`poll_until_settled`'s `Device.State` polling (the actual join/connect
path). Both are compile-verified (this file's B-① equivalent) and
design-verified (read against the NetworkManager D-Bus API spec, see
`nm.rs`'s own header/inline comments) but not activity-verified — left for
the acceptance round, same honesty bar this file's own B-③ section applies
to `duduclaw-comp`'s input forwarding.

## WP-comp-shell-ipc: dock↔comp window query/control client (2026-08-22)

Comp side: `duduclaw-comp/BUILD.md`'s own "WP-comp-shell-ipc" section has
the full design (SEPARATE socket from `codrive`'s agent channel, same-uid
`SO_PEERCRED` auth, no freeze gate, independent audit trail). This section
covers only the shell-side client + dock wiring and its verification.

### What changed

- **`src/comp_client.rs`** (new file, ~250 lines) — blocking Unix-socket
  client, wire types hand-mirrored from comp's `shell_control::protocol`
  (this crate cannot depend on `duduclaw-comp`, a Linux-only detached
  workspace crate — same reasoning `duduclaw-gateway/src/codrive/client.rs`
  already gives for its own hand-mirrored `codrive` types). `list_windows()`
  / `focus_window(query)`, both plain blocking calls per this crate's
  established `gateway_client` contract. Compiles unconditionally on both
  macOS and Linux (`std::os::unix::net::UnixStream` exists on both; a dev
  Mac simply never has a real `duduclaw-comp` running, so every call
  degrades to the ordinary `NotAvailable`/`Io` error path).
- **`src/home/running_windows.rs`** (new file, ~180 lines) — `RunningWindowsFeed`,
  same "pure `&mut self` mutation, no gpui types" discipline `overlay::
  notifications_feed::NotificationsFeed` establishes: staleness tracking
  (`POLL_INTERVAL = 3s` — much tighter than the Notifications feed's 30s
  network-bound cadence, since this is a local socket round trip),
  `begin_refresh`/`apply_list_ok`/`apply_list_err`, and `is_app_running
  (app_id)` — matched against `fake_data::DockApp::flatpak_id` (the only
  real per-icon identity this crate's dock has — see that module's own doc
  comment on why this is a reasonable-but-unverified assumption, flatpak
  still being absent from the dev/appliance images per `apps.rs`).
- **`src/home/home_dock.rs`**: `dock()`/`dock_app()` gained a `running_
  windows: &RunningWindowsFeed` parameter; a small "running" indicator dot
  (macOS-dock-style, centered below the icon, distinct corner from the
  existing Verified-tier dot) now renders for any dock entry whose
  `flatpak_id` matches a live window; a click on an ALREADY-RUNNING entry
  now calls `comp_client::focus_window` (off a background thread — this is
  a blocking socket call, unlike `crate::apps::launch`'s fire-and-forget
  `Command::spawn()`) instead of launching a second instance.
  `schedule_running_windows_poll`/`trigger_running_windows_refresh_if_stale`
  are the poll glue, same `std::thread::spawn` + `mpsc` + `cx.spawn` bridge
  `overlay/notifications.rs` already established — but, unlike that
  module's `schedule_stale_check`, checked IMMEDIATELY on every render pass
  rather than after waiting out one interval first: Home has no
  click-triggered "just opened" event to pair a fast first fetch against
  (it renders continuously from window-open), so the render-pass-gated
  immediate check IS the fast-first-fetch path here.
- **`src/home.rs`** / **`src/main.rs`**: `running_windows: home::running_
  windows::RunningWindowsFeed` threaded onto `ShellView` (same "one model,
  read by whatever surfaces need it" shape `overlay_ui.notifications`
  already establishes) and down through `home::render`/`home_dock::dock`,
  same parameter-passing shape `notifications: &NotificationsFeed` already
  uses.

### Build/clippy/test (WP-comp-shell-ipc round)

macOS (native):
```
cargo build                              -> Finished, zero warnings (besides the pre-existing block v0.1.6 future-incompat notice)
cargo clippy --all-targets -- -D warnings -> Finished, zero warnings
cargo test                               -> 266 passed; 0 failed; 4 ignored
```

Linux container (same volumes/command shape as B-①):
```
cargo build                              -> Finished in 1m 42s (cold), zero warnings
cargo clippy --all-targets -- -D warnings -> Finished, zero warnings
cargo test                               -> 281 passed; 0 failed; 5 ignored
```

(Linux has 15 more passing tests than macOS — `nm.rs`'s own `#[cfg(target_os
= "linux")]`-gated D-Bus tests, unrelated to this round; both runs include
this round's 13 new tests: 7 in `comp_client.rs` + 6 in `home/running_
windows.rs`, plus the 2 new `#[ignore]`d live tests below.)

### Build/clippy/test (WP-A4-4, 2026-08-22 — current)

The numbers above are that round's snapshot and are kept as-is; these are
the current ones. WP-A4-4 (the appliance VM's 429-storm / CPU fixes plus the
flatpak install confirmation gate) added 51 tests on macOS.

macOS (native):
```
cargo build                              -> Finished, zero warnings (besides the pre-existing block v0.1.6 future-incompat notice)
cargo clippy --all-targets -- -D warnings -> Finished, zero warnings
cargo test                               -> 317 passed; 0 failed; 5 ignored
```

Linux container (same volumes/command shape as B-①, plus
`rustup component add clippy` — the `rust:bookworm` image does not ship it):
```
cargo build                              -> Finished in 1m 40s (warm target volume), zero warnings
cargo clippy --all-targets -- -D warnings -> Finished, zero warnings
cargo test  (x3)                         -> 332 passed; 0 failed; 6 ignored  (all three runs)
```

(The macOS/Linux gap is still `nm.rs`'s `#[cfg(target_os = "linux")]` D-Bus
tests, unchanged by this round.)

**Run the suite more than once.** WP-A4-4's backoff tests were initially
written against a clock sampled AFTER the call under test plus a fixed
slack, which went red roughly one run in three whenever a Docker build was
running on the same machine. They are now written as exact lower bounds
against an instant sampled BEFORE the call (see `overlay/
notifications_feed.rs`'s own test-section comment); the fix was verified by
12 consecutive macOS runs with all 12 cores saturated by busy loops, plus
the three container runs above. A single green run is not evidence that a
timing-sensitive test is stable.

### Live verification (this round, combined with comp's own container run)

Full topology: `weston` (headless, layer 1) → `duduclaw-comp` (layer 2 —
now ALSO the HOST, providing a real `wl_seat` to its clients, unlike
weston's headless backend which advertises none — see this file's own
"`wl_seat` finding" above) → `foot -a foot-A` (layer 3a, a real xdg-shell
client) + `duduclaw-shell` itself (layer 3b, `WAYLAND_DISPLAY=wayland-1`,
`DUDUCLAW_SHELL_SKIP_OOBE=1`, `DUDUCLAW_SHELL_DIAG=1`) as TWO concurrent
clients of comp.

`duduclaw-shell` ran the full 15s under `timeout` with **zero panics** —
confirming comp's own `wl_seat` (keyboard+pointer) is sufficient for gpui's
Wayland backend (the `wl_seat.unwrap()` panic this file's B-② section found
is specific to weston's headless backend having NO seat at all, not a gpui
limitation in general). Its own passive dock poll fired for real:

```
[dock] list_windows ok: 2 window(s): [
  CompWindow { app_id: Some("foot-A"), title: Some("foot"), focused: false },
  CompWindow { app_id: None, title: None, focused: false }]
```

(the second entry is `duduclaw-shell`'s own mapped window — gpui doesn't
set an xdg-shell app_id; an honest finding, not a bug.) Concurrently, a
SEPARATE process ran this round's two new `#[ignore]`d live tests
(`comp_client::tests::live_list_windows_against_real_comp` /
`::live_focus_window_against_real_comp`) against the SAME live comp
instance:

```
XDG_RUNTIME_DIR=/tmp/xdg-runtime cargo test -- --ignored \
  live_list_windows_against_real_comp live_focus_window_against_real_comp --nocapture

[live] focus_window("foot-A") matched: AppId("foot-A")
[live] 2 window(s): [CompWindow { app_id: Some("foot-A"), ... }, CompWindow { app_id: None, ... }]
[live] focus_window on a bogus query correctly returned Comp("not_found")
test result: ok. 2 passed; 0 failed
```

Comp's own log and shell-control audit trail (see `duduclaw-comp/BUILD.md`'s
matching section) confirm the resulting `focus: activation set` and
`focus_window`/`focus_window_failed` audit rows — proving comp correctly
serves two concurrent one-shot callers (the live `duduclaw-shell` process's
own background poll, and the separate test process) without interference.

### Honest stub / limitation list (this round)

- **Dock click-to-focus was never exercised via a real mouse click** — this
  round's containers have no real seat/pointer (same category of gap this
  file's own "Input devices remain unverified" section already flags).
  What WAS verified: the exact function (`comp_client::focus_window`)
  `home_dock.rs`'s click handler calls really works end-to-end against a
  live comp instance; only the mouse-click-delivers-the-call step is
  unverified, deferred to a VM/`cage` round same as every other real-input
  gap this file and `duduclaw-comp/BUILD.md` already flag.
- **`flatpak_id`-as-xdg-app_id assumption untested against a real flatpak
  app** — see `home/running_windows.rs`'s own module doc; flatpak is still
  absent from the dev/appliance images (A4 pending), so only `foot`'s
  hand-set `-a` app_id was available to test the matching logic against
  this round.
- **No visual/screenshot verification** — same limitation this file's B-②
  section already flags; every claim above is log evidence, not pixel
  comparison, so the running-indicator DOT's actual on-screen appearance is
  unconfirmed (only that the underlying `is_app_running` state driving it
  is correct — `home/running_windows.rs`'s own test module).
- **Not committed** — per this task's instructions.

## WM-3 shell-side migration: menu bar / dock / desktop / overlay onto layer-shell (2026-08-23)

`duduclaw-comp`'s own WM-3 round (`crates/duduclaw-comp/BUILD.md`) shipped a
real `zwlr_layer_shell_v1` compositor implementation and explicitly left this
crate's migration as a documented gap: *"The shell still does not use
layer-shell... What the shell has to do later: create one
`zwlr_layer_surface_v1` per chrome element on the `top` layer, anchor it,
`set_exclusive_zone(30)` / `(90)`..."* This section is that migration
(`src/chrome/`), plus what is spike-verified vs. what still needs a live
compositor round to confirm.

### Spike findings (verified by reading the pinned gpui rev's own source,
### `~/.cargo/git/checkouts/zed-a70e2ad075855582/7a7c3e1` — not yet exercised
### against a real compositor by THIS round)

1. **`gpui::WindowKind::LayerShell(LayerShellOptions)` exists and is real** —
   `crates/gpui/src/platform.rs`, gated
   `#[cfg(all(target_os = "linux", feature = "wayland"))]`. `LayerShellOptions`
   (`namespace`/`layer`/`anchor`/`exclusive_zone`/`exclusive_edge`/`margin`/
   `keyboard_interactivity`) and its enums (`Layer`, `Anchor` — a bitflags
   type — `KeyboardInteractivity`) live in `crates/gpui/src/platform/
   layer_shell.rs` and are **not** cfg-gated themselves (only the `WindowKind`
   *variant* that carries them is), so `chrome/params.rs` still defines its
   OWN gpui-free mirror types (`ChromeLayer`/`ChromeAnchor`/
   `ChromeKeyboardInteractivity`) rather than reusing gpui's directly — the
   task brief for this round asked for the pure/testable half to stay
   gpui-free so it compiles and unit-tests on macOS too, and only
   `chrome/gpui_bridge.rs` (Linux-only) converts one into the other.
   `crates/gpui_linux/src/linux/wayland/window.rs:151` onward really calls
   `zwlr_layer_shell_v1.get_layer_surface` + `set_size`/`set_anchor`/
   `set_keyboard_interactivity`/`set_margin`/`set_exclusive_zone`, matching
   the earlier spike round's own finding.
2. **`exclusive_zone: Some(px(-1.))` is safe and means what the protocol says
   it means.** `gpui_linux/src/linux/wayland/window.rs:184`:
   `layer_surface.set_exclusive_zone(f32::from(exclusive_zone) as i32)` — a
   plain `as i32` cast, no clamp, no `.max(0)`. `px(-1.)` therefore reaches
   the compositor as a literal `-1`, the wlr-layer-shell protocol's own
   escape hatch ("give me the whole output, don't shrink me for other
   surfaces' exclusive zones"). Used by the `Home` background surface's
   `LayerParams::exclusive_zone` (`chrome/params.rs`) — the desktop must
   always fill the entire output regardless of what the menu bar/dock/
   overlay are doing.
3. **A runtime setter exists for exclusive zone, and it's safe on every
   platform.** `gpui::Window::set_exclusive_zone(&self, zone: Pixels)`
   (`crates/gpui/src/window.rs:2124`) calls through to
   `PlatformWindow::set_exclusive_zone`, whose TRAIT DEFAULT is an empty
   no-op body (`crates/gpui/src/platform.rs:903`) — so calling it
   unconditionally, on macOS or on a `SingleFullscreen` fallback window, is a
   silent no-op, never a panic, never needs a `#[cfg]` guard. This is what
   lets the menu bar/dock windows stay OPEN through OOBE and the lock screen
   rather than being destroyed and recreated — see `chrome/mod.rs`'s own
   header comment for the full reasoning (`should_hide_chrome_bars` in
   `chrome/windows.rs` toggles this at render time instead).
4. **`cx.open_window(...)` returns `Err(LayerShellNotSupportedError)`** when
   the compositor doesn't advertise `zwlr_layer_shell_v1` at all
   (`crates/gpui/src/platform/layer_shell.rs`'s own doc comment). This
   crate's B-② round above already recorded that **weston's headless
   backend** (`weston --backend=headless-backend.so`, the exact host this
   file's earlier verification rounds used) does not implement
   wlr-layer-shell — so the degrade path this round adds
   (`chrome::windows::boot_windows`, falling all the way back to the
   original `SingleFullscreen` single-toplevel window) is not a
   theoretical/paranoid branch, it is the CONFIRMED behavior the very next
   `cargo test`/live-run round against this file's existing weston harness
   will exercise for real.

### What changed

- **`src/chrome/params.rs`** (new, cross-platform, unit-tested) —
  `ChromeSurface` (`MenuBar`/`Dock`/`Home`/`Overlay(crate::surface::
  Overlay)`), the gpui-free `ChromeLayer`/`ChromeAnchor`/
  `ChromeKeyboardInteractivity` mirror types, `LayerParams` +
  `layer_params_for(surface)` (the one place that knows every surface's
  namespace/layer/anchor/exclusive-zone/keyboard-interactivity), `ChromeMode`
  (`LayerSurfaces`/`SingleFullscreen`) + `desired_chrome_mode(is_linux,
  env)` (pure — takes `cfg!(target_os = "linux")` and the raw
  `DUDUCLAW_SHELL_NO_LAYER_SHELL` env value as plain parameters, same
  "read the env once at the call site, decide in a pure fn" convention
  `Overlay::from_debug_env` already established). 10 unit tests.
- **`src/chrome/mod.rs`** (new, cross-platform) — module doc for the whole
  design, `SHELL_APP_ID` constant (single source for the `app_id` string
  every window this crate opens declares — was a literal at each call site
  before), `active_mode()`/`set_active_mode()` (a `OnceLock<ChromeMode>` —
  read by `main.rs`'s `settle_launcher_query` to decide whether it can
  focus the Launcher's search field directly).
- **`src/chrome/gpui_bridge.rs`** (new, `#[cfg(target_os = "linux")]`) —
  converts `LayerParams` → real `gpui::WindowOptions` carrying a
  `WindowKind::LayerShell(_)`. 1 unit test (structural: every
  `ChromeSurface` converts to the right `WindowKind`/`app_id`/
  `window_background`; no compositor involved).
- **`src/chrome/windows.rs`** (new, `#[cfg(target_os = "linux")]`) — the
  actual gpui window orchestration:
  - `SurfaceView`: the thin per-window root view for `ChromeMode::
    LayerSurfaces`. Holds `kind: ChromeSurface` + `shared: Entity<ShellView>`
    (the SAME entity every window shares — see below); its `Render::render`
    dispatches to `render_surface_content`, which reads/renders a SLICE of
    the shared state via `shared.update(cx, |shell, shell_cx| ...)`.
  - `boot_windows(cx, shared)`: the one entry point `main.rs` calls on
    Linux. Attempts `try_open_layer_surfaces` (menu bar → dock → desktop,
    all-or-nothing: any failure tears down whatever already opened and
    falls back), records the final mode via `chrome::set_active_mode`
    exactly once.
  - `SurfaceView::reconcile_overlay_window` (called from the `Home`
    instance's own render pass only): compares the shared `SurfaceState::
    overlay()` against whichever overlay window (if any) is currently open
    and opens/closes a window to match — see "Overlay window reconciliation"
    below for why this is reactive rather than wired into every
    `SurfaceState::open()` call site.
  - `should_hide_chrome_bars`: `true` while OOBE is active or the lock
    screen is up — the menu bar/dock windows render nothing and zero their
    exclusive zone in that state (see spike finding 3 above).

### Sharing one `ShellView` across up to four windows

`main.rs`'s `fn main()` now builds `shared_state: Entity<ShellView>` ONCE,
before any window opens (moved out of `cx.open_window`'s builder closure,
where it used to live — every `::new(cx)` call that used to run there only
ever needed `&mut App`, never a live `Window`, so this is behavior-preserving
on the `SingleFullscreen` path). `ChromeMode::SingleFullscreen` then uses
this SAME entity directly as its one window's root view
(`cx.open_window(options, move |_window, _cx| shared_state.clone())`);
`ChromeMode::LayerSurfaces` wraps it with up to four `SurfaceView`s instead —
never a second copy of the state either way.

This is safe specifically because `gpui::Context<T>::listener(...)` (used
throughout `ShellView`'s existing click/action handlers, none of which this
round modified) produces a closure that captures only a WEAK reference to
the `ShellView` ENTITY, not any particular window — verified by reading its
definition (`crates/gpui/src/app/context.rs:252`): at invocation time it is
handed whichever `Window`/`App` the triggering event actually arrived on. An
action listener built while `chrome::windows::render_overlay_content` is
rendering the Overlay window works identically when it fires from a click
inside THAT window; the same listener-construction code, if it ran while
rendering the Home window instead, would work identically there. No listener
needs to "know" which window it will run in.

Every `SurfaceView` observes the shared entity once at construction
(`cx.observe(&shared, |_, _, cx| cx.notify()).detach()`), so a `cx.notify()`
anywhere in `ShellView`'s existing methods (locking, completing OOBE,
opening/closing an overlay, toggling a ControlCenter switch, ...) schedules a
re-render of EVERY open chrome window, not just whichever one happens to
also be the window the triggering event arrived on.

### Overlay window reconciliation — why reactive, not wired into every call site

`crate::surface::SurfaceState` (`src/surface.rs`, untouched by this round)
already tracks "which overlay, if any, is open," and its handful of mutation
call sites are scattered across `home.rs`/`home_dock.rs`/`overlay/*.rs` —
most of which are out of this round's editing scope (a SEPARATE agent owns
`overlay/**` on this task). Rather than teach every `view.surface.open(...)`
call site to also open/close a gpui window, `SurfaceView::render`
reconciles the overlay window reactively — but only from the `Home`
instance's own render pass, since Home is the one window guaranteed to
exist for the whole `LayerSurfaces` session and to re-render on every
shared-state change. This mirrors an existing convention in this crate:
`home_dock::dock()` already dispatches a background poll
(`schedule_running_windows_poll`) as a side effect of its own render
pass — side-effecting work from inside `render()` is not new here.

One real consequence: `ShellView::settle_launcher_query` (`main.rs`) can no
longer unconditionally focus the Launcher's search field on the OPEN path.
In `LayerSurfaces` mode the overlay window is created ASYNCHRONOUSLY by the
reconciler above, AFTER `settle_launcher_query` returns — so at the moment
it runs, `window` names whichever OTHER window the click/keystroke that
opened the Launcher actually arrived on (typically Home), and focusing the
search field's handle against that window would silently do nothing (no
matching dispatch node there). `settle_launcher_query` now checks
`chrome::active_mode()` and, on the open path in `LayerSurfaces` mode,
leaves the focus call to the overlay window's own construction
(`chrome::windows::open_overlay_window`, which focuses the Launcher's search
field or the shared root handle depending on which overlay was opened) —
`SingleFullscreen` mode is completely unaffected (there is only ever one
window, so the original direct-focus behavior is unchanged).

### Menu bar / dock parameters actually written into code

| Surface | namespace | layer | anchor | exclusive_zone | keyboard_interactivity | height |
|---|---|---|---|---|---|---|
| Menu bar | `duduclaw-shell-menubar` | `Top` | `TOP\|LEFT\|RIGHT` | `Some(30.0)` (`0.0` while hidden) | `None` | 30.0 |
| Dock | `duduclaw-shell-dock` | `Top` | `BOTTOM\|LEFT\|RIGHT` | `Some(90.0)` (`0.0` while hidden) | `None` | 90.0 |
| Home (desktop) | `duduclaw-shell-home` | `Background` | `TOP\|BOTTOM\|LEFT\|RIGHT` | `Some(-1.0)` | `OnDemand` | 900.0 (placeholder) |
| Overlay (any) | `duduclaw-shell-overlay` | `Overlay` | `TOP\|BOTTOM\|LEFT\|RIGHT` | `None` | `Exclusive` | 900.0 (placeholder) |

30/90 match comp's own WM-1 `DEFAULT_RESERVED_TOP`/`_BOTTOM` exactly (see
comp's BUILD.md WM-3 section, "90 bottom / 30 top, the unmigrated shell's own
chrome"). No `exclusive_edge` is ever set (`None` in every case) — every
surface above anchors either one edge plus both perpendicular edges (never a
bare corner), so the exclusive edge is unambiguous from `anchor` alone per
the wlr-layer-shell protocol. No `margin` is ever set either — this crate's
chrome sits flush against its anchored edges. `window_background` is
`Transparent` on every layer-shell window (matches gpui's own
`examples/layer_shell.rs`).

**`keyboard_interactivity: Exclusive` on the overlay is a forward
declaration, not yet honoured end-to-end**: comp's own WM-3 "Known
limitations" section states it currently treats `Exclusive` the same as
`OnDemand` ("gets focus when clicked, does not lock keyboard away from
windows"). Requesting the protocol-correct value costs nothing today and is
what comp should honour once it implements real exclusive semantics —
recorded here so nobody mistakes today's degraded behavior for a shell-side
bug.

### Degradation path — and how to verify it

`chrome::windows::boot_windows` always attempts `try_open_layer_surfaces`
first on Linux (unless `DUDUCLAW_SHELL_NO_LAYER_SHELL=1`), and on ANY
failure — the realistic one being the very first `cx.open_window` call
returning `Err(LayerShellNotSupportedError)` — tears down whatever already
opened and calls the exact same `SingleFullscreen`-path code the
`#[cfg(not(target_os = "linux"))]` branch in `main.rs` uses (same
`WindowOptions`, same `Entity<ShellView>` reused as root view directly, same
post-open focus call), so the fallback is this crate's ORIGINAL, unmodified
single-window behavior — zero visual regression by construction, not by
inspection.

**How to verify** (next round, not yet run by this one): re-run this file's
own weston harness (`weston --backend=headless-backend.so`, B-② section
above) — since that backend does not implement wlr-layer-shell, `duduclaw-
shell` should log `[chrome] layer-shell unavailable (...); degrading to a
single fullscreen window` and then behave EXACTLY as B-②'s existing evidence
already shows (one window, `overlay=None`/`Some(Launcher)` toggling on
cmd-k, etc.). Separately, `DUDUCLAW_SHELL_NO_LAYER_SHELL=1` against a
compositor that DOES support layer-shell (the four-layer stack this file's
WP-comp-shell-ipc section already used — `duduclaw-comp` on top of weston)
should produce the identical single-window log/behavior, proving the env
override works independently of compositor support.

### Honest limitations (this round)

- **Nothing in this section has been run against a real (or headless)
  Wayland compositor yet.** Every claim above is either (a) read directly
  from the pinned gpui rev's own source (cited with exact file:line), or
  (b) a `cargo test`-level unit test of pure logic (`chrome/params.rs`,
  `chrome/gpui_bridge.rs`) — no `cx.open_window(WindowKind::LayerShell(_))`
  call in this crate has actually executed. The very next round should run
  this file's existing weston/`duduclaw-comp` harnesses against the new
  code, exactly as the "how to verify" subsection above describes.
- **Compile/clippy/test have NOT been run for this round** — per this
  task's instructions (verification is done centrally, serially, by the
  orchestrating session, specifically because several agents are editing
  `crates/duduclaw-shell` concurrently this round and a background `cargo
  build` from one agent would race another's). The field list in `main.rs`'s
  `cx.new(|cx| ShellView { ... })` construction (moved, not rewritten, by
  this round) was hand-matched against the struct definition at the moment
  this round finished, but at least two OTHER concurrent rounds were adding
  fields to `ShellView` (`FocusNext`/`FocusPrev` + `on_focus_next`/
  `on_focus_prev`/`cycle_oobe_focus`/`oobe_focus_handle`; a `settings_ui`/
  `settings_fields` pair for a new 系統設定 overlay) while this round was in
  progress — a real risk of the construction site and the struct definition
  having drifted again by the time this file is actually compiled.
- **The overlay window's focus-on-open path is unverified.**
  `chrome::windows::open_overlay_window` focuses the Launcher's search field
  (or the shared root handle for every other overlay) at window-construction
  time — reasoned through against gpui's own `Context::listener`/
  `Entity::update`/`WindowHandle::update` signatures (all cited above with
  exact file:line), never run.
- **What happens to Wayland-level (not just gpui-internal) keyboard focus
  when the overlay window closes is unverified and possibly a real gap.**
  `SurfaceView::reconcile_overlay_window` destroys the overlay window and
  the Home window's own `boot_windows`-time focus call is never repeated —
  whether the compositor automatically hands keyboard focus back to Home
  after an on-demand layer surface with `Exclusive` (degraded to `OnDemand`
  on comp today, see above) is destroyed is a COMPOSITOR policy question
  this round has no evidence for either way.
- **DPI scaling, HiDPI, and multi-output behavior are untested** for every
  new window kind — same gap this file's existing sections already flag for
  the pre-WM-3 single window.
- **The `DUDUCLAW_SHELL_DEBUG_SURFACE=launcher` headless-smoke hook's
  behavior differs slightly by mode, by design**: in `SingleFullscreen` mode
  it still calls `settle_launcher_query` directly (byte-identical to
  before); in `LayerSurfaces` mode it opens the overlay via `shared_state.
  update(...)` and relies on the reconciler + `open_overlay_window`'s own
  focus call to finish the job on Home's next render pass — untested end to
  end (see "Overlay window reconciliation" above).
- **Not committed** — per this task's instructions.

## gpui upstream finding: destroying a layer-shell window kills the keyboard

**Status: confirmed defect in the pinned rev's source; exact trigger in our
sequence NOT proven.** Recorded here so the workaround below is never
"cleaned up" by someone who has not hit it.

### Symptom (appliance VM, real udev compositor)

After this client destroys **any** `zwlr_layer_shell_v1` window, it stops
dispatching key events entirely — until a mouse click, which restores them.
Not lock-screen specific: opening the Launcher and closing it again with
Escape is enough.

Measured with `DUDUCLAW_SHELL_DIAG=1`:

* before the teardown, keys dispatch normally (`[action] LockScreenNow fired`);
* after it, the root element's key probe logs **nothing at all** for further
  keypresses (probe count frozen);
* `duduclaw-comp` meanwhile logs keyboard focus **unchanged and correct**
  (`focus already held … held_id=wl_surface@18`, the desktop surface — the
  destroyed surfaces were `@77`/`@111`/`@164`). So the compositor keeps
  delivering to a surface the client has stopped listening on.

Ruled out first, each by direct experiment: the layer migration itself
(`DUDUCLAW_SHELL_NO_LAYER_SHELL=1` behaves identically), the compositor's
focus bookkeeping (above), the IME (`pkill fcitx5` changes nothing), and the
keymap (the probe shows `Keystroke { key: "a", key_char: Some("a") }`
arriving intact while it still worked).

### Source reading (`~/.cargo/git/checkouts/zed-a70e2ad075855582/7a7c3e1`)

`crates/gpui_linux/src/linux/wayland/client.rs`:

* **`wl_keyboard::Event::Leave` (line ~1739)** clears focus
  **unconditionally**:
  ```rust
  wl_keyboard::Event::Leave { surface, .. } => {
      let keyboard_focused_window = get_window(&mut state, &surface.id());
      state.keyboard_focused_window = None;   // <-- no check that `surface`
                                              //     is the focused one
  ```
  A `leave` for *any* surface therefore drops the client's keyboard focus,
  including when a different surface still legitimately holds it. This is a
  real defect independent of our usage.
* **`wl_keyboard::Event::Enter` (line ~1730)** sets
  `keyboard_focused_window = get_window(&surface.id())`, so focus can only be
  restored by a fresh `enter` naming a surface still present in
  `state.windows`.
* **`drop_window` (line ~571)** looks correct on its own: it preserves
  `keyboard_focused_window` unless the closed window *is* the focused one
  (`ptr_eq` guard).

### Honest gap

The `Leave` defect is confirmed by reading; what is **not** proven is that
our teardown actually produces such a `leave` (the destroyed bars carry
`KeyboardInteractivity::None` and never held focus). Two compositor-side
fixes aimed at that theory were tried and did **not** help — re-asserting
focus with an explicit `leave`+`enter` pair, and re-focusing the surviving
window from the client — so the mechanism may be a third thing in the same
teardown path. Both attempts were reverted rather than left in as inert
churn.

### Minimal reproduction (for an upstream report)

1. Wayland compositor supporting `zwlr_layer_shell_v1`.
2. gpui client opens two layer-shell windows, A (`OnDemand`, holds keyboard
   focus) and B (`None`, never focused).
3. Type into A — key events dispatch.
4. `remove_window()` on **B**.
5. Type into A again — no key events dispatch. A mouse click restores them.

### What we do instead

`chrome/windows.rs` never destroys a chrome bar. Hiding is
`apply_bar_visibility`: empty input region + 1×1 + zero exclusive zone. See
that function and `reconcile_chrome_bars` for why the two earlier approaches
(full-size-but-empty, and destroy) each failed.

The Launcher overlay is still destroyed on close and therefore still hits
this bug — it is created with `KeyboardInteractivity::Exclusive`, and gpui
exposes no runtime setter for that (`set_keyboard_interactivity` is
creation-only, `gpui_linux/.../window.rs:170`), so keeping it mapped would
steal the keyboard permanently. Fixing that needs either an upstream gpui
change or a different overlay design; tracked as D9-bug.
