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
