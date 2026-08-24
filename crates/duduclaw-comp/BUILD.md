# duduclaw-comp — build & run notes (Shell-S0 smithay spike)

## What this is

A minimal self-built Wayland compositor, adapted from smithay's `smallvil`
example (MIT license — see the attribution note in `src/main.rs`). It exists
to answer one question for DuDuClaw OS's L1 layer: **can we build our own
compositor** (design doc: `commercial/docs/DESIGN-native-gui-gpui-2026-08.md`
§13.5, D11 — smithay self-built, MIT, closed-source-capable; anvil/cage
weight class, not niri/cosmic-comp weight class).

Scope of this spike: single output, xdg-shell server side, `winit`-nested
backend (runs as a window inside a host Wayland/X11 session — no DRM/KMS, no
libinput, no real hardware ownership), basic move/resize window management,
keyboard+pointer input forwarding. See the "what this deliberately does not
carry over" list in `src/main.rs`'s module doc.

> **Superseded in part by A4-1 (2026-08-22).** There are now **two** backends
> in the one binary, selected at runtime: the original `winit`-nested one
> (development/CI, everything below) and a real-hardware `udev`/DRM/KMS +
> libseat + libinput one. "no DRM/KMS, no libinput, no real hardware
> ownership" is still an accurate description of the **winit** backend only.
> See **"A4-1: udev/DRM backend"** at the bottom of this file for the new
> backend, the extra system build dependencies it needs, its damage-driven
> repaint scheduling, and exactly what is and is not verified.

## Why Docker, not `cargo build` on this Mac

smithay is **Linux-only**: it depends on `wayland-server`/`wayland-client`
and (via the `desktop`/keyboard input path) `libxkbcommon`, none of which
exist on macOS. This crate is deliberately **excluded from the main
DuDuClaw workspace** (root `Cargo.toml` `[workspace] exclude`) and carries
its own empty `[workspace]` table so it never touches the gateway build or
its `Cargo.lock`. Verification for this crate therefore happens inside a
Linux container, not via `cargo build` at the repo root.

## Reproducible build command (verified 2026-08-19)

> ### ⚠️ STALE as of A4-1 — do not copy the command in this section
>
> **The three-package list below stopped being sufficient when the A4-1
> `udev`/DRM/KMS backend landed** (see "A4-1: udev/DRM backend" at the
> bottom of this file). That backend is compiled into the *same* binary and
> selected at runtime, so its system libraries are linked
> **unconditionally** — there is no feature flag that keeps them out of an
> ordinary `cargo build`/`cargo test`. Running the command below today
> fails at link time with:
>
> ```
> /usr/bin/ld: cannot find -lgbm / -lseat / -ludev / -linput
> ```
>
> (Reproduced 2026-08-23 while verifying the WM-3 shell-migration package —
> this note exists because the stale command cost a real build cycle.)
>
> **Use this instead** — the current minimum for `cargo build` and
> `cargo test`:
>
> ```bash
> docker run --rm \
>   -v /Users/lizhixu/Project/DuDuClaw:/work \
>   -v duduclaw-comp-cargo:/usr/local/cargo/registry \
>   -v duduclaw-comp-cargo-git:/usr/local/cargo/git \
>   -v duduclaw-comp-target:/target \
>   -e CARGO_TARGET_DIR=/target \
>   -w /work/crates/duduclaw-comp \
>   rust:bookworm bash -c '
>     set -e
>     apt-get update -qq
>     apt-get install -y -qq --no-install-recommends \
>       pkg-config libwayland-dev libxkbcommon-dev \
>       libinput-dev libudev-dev libseat-dev libgbm-dev libdrm-dev
>     cargo test
>   '
> ```
>
> Add `libegl1 libgl1-mesa-dri libgles2 weston foot` on top of that only for
> the **live-run** sections further down (headless weston needs a software
> GL stack and a test client; a plain build/test does not).
>
> The rest of this section is kept as the historical record of the original
> spike, when the winit backend really was the only one.

```bash
docker run --rm \
  -v /Users/lizhixu/Project/DuDuClaw:/work \
  -w /work/crates/duduclaw-comp \
  rust:bookworm bash -c '
    set -e
    apt-get update -qq
    apt-get install -y -qq pkg-config libwayland-dev libxkbcommon-dev
    cargo build
  '
```

That was the **entire** system dependency list **at the time of the original
spike** — just `pkg-config`, `libwayland-dev`, `libxkbcommon-dev` (see the
stale-warning box above for what it is now). No mesa/EGL headers were
needed: the
`backend_egl`/`renderer_gl` smithay features (pulled in transitively by
`backend_winit`) only codegen GL bindings at build time and `dlopen()` the
actual GL/EGL libraries at run time via `libloading`, so there's nothing to
link against at compile time. No libinput/udev/drm either — this spike
doesn't enable smithay's `backend_libinput`/`backend_udev`/`backend_drm`
features, so those system deps were never needed.

Verified on: `rust:bookworm` image, `rustc 1.97.1` / `cargo 1.97.1`,
Debian 12 (bookworm), **aarch64** (Apple Silicon host, Docker Desktop's
default Linux/arm64 container). smithay 0.7.0's declared MSRV is 1.80.1, so
this margin is comfortable; there was no need to pin a specific `rust:X.Y`
tag over the floating `bookworm` tag for this spike.

A from-scratch run (fresh container, cold cargo registry cache, no prior
`target/`) completed `cargo build` in **9.8s** and produced a real ELF
binary:

```
target/debug/duduclaw-comp: ELF 64-bit LSB pie executable, ARM aarch64,
version 1 (SYSV), dynamically linked, interpreter /lib/ld-linux-aarch64.so.1,
... for GNU/Linux 3.7.0, with debug_info, not stripped
```

Zero warnings, zero errors, on both a cold run and a cached-registry rerun.

`target/` is not checked into this directory after verification (it's
~1GB of disposable Docker-container build output, and this crate's
directory is currently git-untracked entirely per the task — see the "not
committed" note below). Rerun the command above to reproduce it; it takes
under 10 seconds with a warm cargo registry.

## smithay version choice

Pinned to **smithay 0.7.0 from crates.io** (`[dependencies.smithay] version
= "0.7.0"`), not a git pin. Checked before deciding (2026-08-19):

- 0.7.0 is smithay's latest published crates.io release (2025-06-24,
  `rust-version = "1.80.1"`).
- `master`'s `smallvil` example has since moved to edition 2024 and gained a
  few surface-level API changes (e.g. `PhysicalProperties` gained a
  `serial_number` field, `main.rs`/`winit.rs` were restructured to pass
  `&mut Smallvil` directly instead of a `CalloopData` wrapper) — none of
  which are needed for this spike's scope (winit backend + xdg-shell +
  single output + move/resize). Git-pinning `master` would have bought
  nothing but drift risk against an unreleased API.
- The documented fallback, if a later round of this spike needs an API only
  present post-0.7.0 (e.g. layer-shell server-side support, which D11/§13.5
  eventually wants for the panel), is to git-pin a specific `rev` on
  `Smithay/smithay` and record the reason in `Cargo.toml`'s comment right
  above the `[dependencies.smithay]` table — not to move wholesale to
  tracking `master`.

## Not committed

Per this task's instructions, nothing in this round is committed. The
`crates/duduclaw-comp/` directory is git-untracked (`git status` shows it as
`??`), same as the existing `crates/duduclaw-native-gui/` precedent. The
root `Cargo.toml`'s `[workspace] exclude` entry for this crate was already
in place before this round (added by the orchestrating session) and was not
touched here.

## Honest stub / simplification list (vs. upstream smallvil)

This is close to a straight port, not a rewrite — deviation risk wasn't
worth it for a "prove we can build one" spike. What actually changed:

- **File/module names**: `winit.rs` → `winit_backend.rs` (avoids shadowing
  the `winit` crate name; matches the task's requested "winit backend"
  module split). `main.rs` was reorganized to keep `CalloopData` +
  `DisplayHandle` threading (smallvil's *0.7.0* shape) rather than master's
  newer single-`&mut Smallvil` shape, since we're pinned to 0.7.0's API.
- **Struct renamed** `Smallvil` → `DuduclawComp` throughout (cosmetic —
  this isn't smallvil, it's our own crate).
- **`std::env::set_var` wrapped in `unsafe {}`** — required unconditionally
  by the Rust std library as of 1.82 regardless of edition; the upstream
  0.7.0-tagged smallvil predates that and doesn't wrap it (upstream
  `master` already does, confirming this isn't a spike-specific hack).
- **Default test-client spawn removed**: upstream smallvil falls back to
  spawning `weston-terminal` with no `-c/--command` argument. This spike's
  target Docker/VM environments don't have `weston-terminal` installed by
  default, so silently trying-and-failing to spawn it was replaced with
  "spawn nothing, log the socket name" — `-c/--command <client>` still
  works to launch any client explicitly. This is the only behavioral (not
  just cosmetic) deviation from upstream.
- **Everything else** (state management, xdg-shell handling, move/resize
  grabs, input event translation, output setup) is the same logic as
  smallvil 0.7.0, module-for-module, with only the renames above.

What's **not implemented at all** (matches upstream smallvil — not a
regression introduced here, just scope this example never had):
popup grabs (`fn grab` is a documented no-op, same as upstream) — **now
implemented, see "A1 multi-window round" below** —, XWayland, layer-shell,
DRM/libinput/udev backends — **now implemented, see "A4-1: udev/DRM backend"
below** —, screen-copy protocols.

## Original next-round run plan (superseded — see "Nested headless live-run" below)

This was written when this round only proved `cargo build` succeeds inside a
Linux container, and assumed *actually running* the compositor needed a real
Wayland/X11 host session that a headless Docker container couldn't provide.
That assumption turned out to be wrong — see the "Nested headless live-run"
section below, which got a real xdg-shell client talking to `duduclaw-comp`
entirely inside Docker via a headless **software-rendered** host compositor
(`weston --backend=headless-backend.so`), no VM required. The two options
below are kept for record and are still the right next step for verifying
**real input devices** (keyboard/mouse event forwarding) and **hardware GL**,
neither of which a headless container can exercise:

### Option A — 值班機 QEMU VM, `cage` as host (matches production target)

Run `duduclaw-comp` **nested inside `cage`** the same way the appliance
image's kiosk mode will eventually nest DuDuClaw OS's real shell:

1. Boot the existing 值班機 QEMU VM (see
   `commercial/docs/DESIGN-appliance-image-2026-08.md` /
   `project_appliance_vm_test_build` for the known-working `run-vm.sh`
   flow — Arch-based, already has a working boot path from the 33-round
   iteration).
2. Install `cage` (wlroots kiosk compositor) plus a minimal Wayland client
   for smoke-testing — `foot` (lightweight terminal, Wayland-native) is a
   better pick than `weston-terminal` (pulls in the whole Weston stack for
   one binary).
3. `cage -- duduclaw-comp -c foot` — `cage` gives `duduclaw-comp` a full
   host surface to nest its own `winit` window inside; `duduclaw-comp` then
   spawns `foot` as its first xdg-shell client.
4. Verify: `foot` renders inside `duduclaw-comp`'s window, keyboard input
   reaches it, mouse click focuses/raises it, resize/move both work via
   the existing move/resize grab code.
5. This is the same VM the appliance-image work already stood up — no new
   infra, just an added package set + one binary copy.

### Option B — Lima/UTM Ubuntu desktop VM (faster iteration loop)

Lower-friction alternative for iterating on `duduclaw-comp` itself before
it needs to prove anything about the appliance image specifically:

1. `limactl start --name=duduclaw-comp-dev template://ubuntu` (or a UTM VM
   with an Ubuntu desktop ISO) — gets a full GNOME/Wayland session for free,
   so `duduclaw-comp` runs nested inside *that* host compositor via the
   existing `winit` backend with zero extra compositor setup.
2. `rsync`/mount this crate's source in, `cargo build`, run
   `./target/debug/duduclaw-comp -c foot` (or any installed Wayland client)
   directly from a terminal inside the VM's desktop session.
3. Faster inner loop than Option A (no `cage`/kiosk layer to fight with
   while iterating on window management bugs), but doesn't validate the
   actual kiosk-mode nesting path the appliance image needs — treat this as
   the dev-loop VM, Option A as the "does it work in the real target shape"
   VM.

**Recommendation for next round**: start with **Option B** to get a client
actually rendering and confirm input plumbing works end-to-end, then
validate the same binary under **Option A**'s `cage` nesting once B is
green — cheaper bug isolation (dev-loop VM first) before spending time in
the heavier appliance VM.

## Nested headless live-run (verified 2026-08-19/20)

Answers the question the previous section deferred: **can `duduclaw-comp`
actually run and accept a real xdg-shell client**, not just compile? Yes —
entirely inside Docker, no VM, no host GPU. No `cage`/VM step turned out to
be necessary for this: a **three-layer nested Wayland stack**, all
software-rendered, all in one container:

```
weston (headless-backend.so, layer 1 — stands in for a real host session)
  └─ duduclaw-comp (winit backend, layer 2 — the crate under test)
       └─ foot (layer 3 — a real xdg-shell terminal client)
```

- **Layer 1 — `weston --backend=headless-backend.so`**: Weston's headless
  backend renders into an off-screen buffer instead of DRM/KMS, so it needs
  no GPU passthrough and no real display — exactly what a Docker container
  can offer. It creates a `WAYLAND_DISPLAY=wayland-host` socket that acts as
  the "host compositor" `duduclaw-comp` nests inside, standing in for
  whatever real Wayland/X11 session would host it on a desktop or in `cage`.
- **Layer 2 — `duduclaw-comp`**: connects to layer 1 as a `winit` client
  (`WAYLAND_DISPLAY=wayland-host`) exactly like it would connect to any host
  compositor; internally it still runs its own full `wayland-server`
  listener and creates its own new socket for *its* clients (deterministically
  `wayland-1` — see the script comment below for why). `LIBGL_ALWAYS_SOFTWARE=1`
  forces Mesa's `llvmpipe` software GL rasterizer instead of trying (and
  failing) to find a real GPU device.
- **Layer 3 — `foot`**: a real, unmodified xdg-shell Wayland terminal
  (Debian package, not a custom test stub) connects to layer 2's socket
  (`WAYLAND_DISPLAY=wayland-1`) and requests an `xdg_toplevel`, proving the
  full server-side xdg-shell path — `new_toplevel` → initial `configure` →
  client ack + buffer commit — actually works against a real client
  implementation, not just against smithay's own test harness.

No EGL/GLES wall was hit. The concern flagged in the task brief — "winit在
headless 宿主下拿不到 EGL surface" — did not materialize: layer 2's EGL
context negotiates `PLATFORM_WAYLAND_KHR` against layer 1 and initializes
`llvmpipe (LLVM 15.0.6, 128 bits)` successfully; no GPU device, DRM node, or
Xvfb/X11 fallback was needed. The system dependency list from the earlier
`cargo build`-only section already covered the build side (`pkg-config`,
`libwayland-dev`, `libxkbcommon-dev`); this round adds the *runtime* side:
`libegl1`, `libgl1-mesa-dri`, `libgles2` (software GL/EGL), plus `weston`
(layer 1 host) and `foot` (layer 3 client, Debian's `foot` package —
`weston-terminal`, also apt-installable, works as an alternative layer-3
client but wasn't needed once `foot` connected cleanly).

### One-shot reproducible command

```bash
docker run --rm \
  -v /Users/lizhixu/Project/DuDuClaw:/work \
  -w /work/crates/duduclaw-comp \
  rust:bookworm bash -c '
set -euo pipefail

echo "==== apt-get install ===="
apt-get update -qq
apt-get install -y -qq --no-install-recommends \
  pkg-config libwayland-dev libxkbcommon-dev \
  libegl1 libgl1-mesa-dri libgles2 \
  weston foot >/dev/null

echo "==== cargo build ===="
cargo build

echo "==== layer 1: weston --backend=headless-backend.so ===="
mkdir -p /tmp/xdg-runtime
chmod 0700 /tmp/xdg-runtime
export XDG_RUNTIME_DIR=/tmp/xdg-runtime
export LIBGL_ALWAYS_SOFTWARE=1

weston --backend=headless-backend.so --socket=wayland-host \
  --width=1280 --height=800 --log=/tmp/weston.log &
WESTON_PID=$!
sleep 2
kill -0 "$WESTON_PID" || { echo "FATAL: weston died"; cat /tmp/weston.log; exit 1; }
echo "weston up, pid=$WESTON_PID, socket=wayland-host"

echo "==== layer 2: duduclaw-comp (nested winit client of layer 1) ===="
WAYLAND_DISPLAY=wayland-host RUST_LOG=info,duduclaw_comp=debug \
  ./target/debug/duduclaw-comp >/tmp/duduclaw-comp.log 2>&1 &
COMP_PID=$!
sleep 2
kill -0 "$COMP_PID" || { echo "FATAL: duduclaw-comp died"; cat /tmp/duduclaw-comp.log; exit 1; }
# Deterministically "wayland-1": smithay 0.7.0s ListeningSocketSource::new_auto()
# always skips "wayland-0" (see src/wayland/socket.rs), and XDG_RUNTIME_DIR
# starts empty in a fresh --rm container, so wayland-1 is the first free slot.
echo "duduclaw-comp up, pid=$COMP_PID, socket=wayland-1"

echo "==== layer 3: foot (real xdg-shell client of layer 2) ===="
WAYLAND_DISPLAY=wayland-1 foot >/tmp/foot.log 2>&1 &
FOOT_PID=$!
sleep 3
kill -0 "$FOOT_PID" || { echo "FATAL: foot failed to connect"; cat /tmp/foot.log; exit 1; }
echo "foot up, pid=$FOOT_PID, connected to WAYLAND_DISPLAY=wayland-1"

kill "$FOOT_PID" 2>/dev/null || true
sleep 1

echo ""
echo "==== duduclaw-comp.log: xdg lifecycle evidence ===="
grep -E "xdg client (connected|disconnected)|xdg_shell: (new toplevel|sending initial configure|toplevel commit)" /tmp/duduclaw-comp.log

kill "$COMP_PID" "$WESTON_PID" 2>/dev/null || true
'
```

Runs as a **single command, one container, start to finish** (apt install +
cargo build + all three layers + evidence grep) — no manual multi-step
`docker exec` needed to reproduce, even though this round's actual
verification was done iteratively via `docker exec` against a long-lived
dev container for faster inner-loop debugging.

### Timing (fresh `--rm` container, cold apt cache, cold cargo registry)

`time docker run --rm ...` end-to-end: **28.5s wall clock** (apt install +
full cargo build from a cold registry + all three layers up + evidence
captured + teardown). Comfortably inside a single command's timeout budget.

### Evidence (verified 2026-08-20 run, `duduclaw-comp.log`)

```
INFO duduclaw_comp::state: xdg client connected client_id=InnerClientId { id: 0, serial: 1 }
INFO duduclaw_comp::handlers::xdg_shell: xdg_shell: new toplevel created, mapping into space surface_id=ObjectId(wl_surface@3[0], 17)
INFO duduclaw_comp::handlers::xdg_shell: xdg_shell: sending initial configure to toplevel surface_id=ObjectId(wl_surface@3[0], 17)
DEBUG duduclaw_comp::handlers::xdg_shell: xdg_shell: toplevel commit (already configured) surface_id=ObjectId(wl_surface@3[0], 17)
DEBUG duduclaw_comp::handlers::xdg_shell: xdg_shell: toplevel commit (already configured) surface_id=ObjectId(wl_surface@3[0], 17)
DEBUG duduclaw_comp::handlers::xdg_shell: xdg_shell: toplevel commit (already configured) surface_id=ObjectId(wl_surface@3[0], 17)
INFO duduclaw_comp::state: xdg client disconnected client_id=InnerClientId { id: 0, serial: 1 } reason=ConnectionClosed
```

This is the full real lifecycle, not a partial/lucky match: client TCP-equivalent
(Unix socket) connect → `xdg_toplevel` object created and mapped into
`state.space` → server sends the mandatory initial `configure` → client acks
+ attaches a real pixel buffer (three commits — `foot` redraws a few times
as its cursor blinks/font loads) → clean disconnect when `foot` was killed.
The three tracing call sites that produce this (`ClientState::initialized` /
`disconnected` in `src/state.rs`, `new_toplevel` and the
`initial_configure_sent` branch in `src/handlers/xdg_shell.rs`) were added
this round specifically so this evidence is directly greppable instead of
having to infer client activity from smithay's own low-level EGL/protocol
debug spam.

### Honest stub / limitation list (this round)

- **Software rendering only, and unthrottled.** `llvmpipe` (Mesa's CPU
  rasterizer) is what actually draws every frame here — there is no GPU in
  this container. `duduclaw-comp`'s winit backend also redraws in a tight
  loop (`backend.window().request_redraw()` unconditionally at the end of
  every `WinitEvent::Redraw`, inherited unchanged from upstream smallvil —
  see the "Honest stub" list above), so the process pegs roughly one CPU
  core continuously (observed ~30-35% of a container CPU quota in `ps aux`
  during the run) rather than settling at a vsync-paced idle rate. Fine for
  a correctness spike; a real target (even nested in `cage` on real
  hardware) would want frame-rate pacing before this became a shipped
  compositor.
- **Keyboard/mouse input forwarding is NOT verified by this round.** `foot`
  connected, rendered, and was cleanly killed, but nothing in this headless
  container sent it a synthetic key or pointer event — `weston`'s headless
  backend has no input devices to originate them from, and no
  `wtype`/`ydotool`-equivalent injection tool was added to keep this round's
  scope to "prove the xdg-shell wire protocol path end-to-end." The
  move/resize grab code in `src/grabs/` and the input translation in
  `src/input.rs` are therefore still unexercised by any of this round's
  live-run evidence — that's still what Option A (`cage` on the 值班機 VM,
  which has a real keyboard/mouse-capable seat) is for.
- **Single test client, one session.** Only `foot` was tried (chosen because
  it's a small, purpose-built Wayland-native terminal already in Debian's
  repos — `weston-terminal` was installed alongside it as a fallback but
  never needed). Multi-window stacking, move/resize grabs, and popup
  handling (`grab()` is still a documented no-op, unchanged from upstream —
  see the earlier "Honest stub" list) are unexercised. **Closed by the "A1
  multi-window round" below** (3 concurrent real clients, move/resize grabs
  exercised against 2 of them, popup grabs implemented and exercised
  against a real GTK context menu).
- **`weston`'s headless backend, not a "real" host session.** It's a
  legitimate stand-in (it implements the real Wayland host-compositor
  protocols, just backed by an off-screen buffer instead of DRM/KMS), but
  it's still one layer more synthetic than the VM-based Options A/B above,
  which nest `duduclaw-comp` inside an actual GNOME/`cage` session with a
  real seat. Treat this round as confirming the *protocol/rendering* path
  end-to-end cheaply and repeatably in CI-friendly Docker; Options A/B
  remain the plan for confirming the *input* path on real hardware.
- **Zombie child processes in the long-lived `docker exec` dev container.**
  Purely an artifact of this round's iterative debugging style (backgrounding
  processes under a container whose PID 1 is `sleep infinity`, which doesn't
  reap children) — irrelevant to the one-shot `docker run --rm` reproduction
  command above, where the whole container (and all its processes) is torn
  down on exit regardless.

## VM cage real-seat input verification (verified 2026-08-20)

The "Option A" plan above, executed — closing the honest limitation the
nested headless live-run recorded ("鍵鼠輸入轉發未驗（headless 無輸入裝
置）——grabs/input.rs 未被活體覆蓋"). Run by the Shell-S2 acceptance side
inside the appliance QEMU VM (same instrumented invocation as
`duduclaw-shell/BUILD-LINUX.md`'s stage B-③ — virtio-gpu + usb-kbd +
usb-tablet + QMP + serial debug shell; see that file for the offline
injection recipe, which additionally placed this crate's binary at
`/usr/local/bin/duduclaw-comp` plus the `foot` + GL-runtime deb closure).

The three-layer stack, now on a REAL seat instead of weston headless:

```
cage (DRM/KMS + seatd — the appliance image's own kiosk compositor)
  └─ duduclaw-comp (winit backend, cage's single fullscreen client)
       └─ foot (xdg-shell client on duduclaw-comp's own wayland-1 socket)
```

**Evidence (QMP screendump PNGs in `appliance/.vm/s2-evidence/`):**
- `comp-foot.png` — foot's window (CSD titlebar + root shell prompt)
  rendered inside duduclaw-comp inside cage on the virtio-gpu output;
  comp's pointer cursor visible.
- `comp-input.png` — after QMP-injected REAL input: pointer moved into
  foot's window + left-click (click-to-focus), then key events typed
  `echo compinputok42` + Enter — the terminal shows the command line, its
  output, and a fresh prompt. Every event crossed
  virtio-kbd/tablet → cage (libinput/seat) → wayland → comp's winit
  window → **this crate's input forwarding** → foot.

**Launch details worth keeping:** `LIBGL_ALWAYS_SOFTWARE=1` must be scoped
to the duduclaw-comp CHILD only (`cage -d -- env LIBGL_ALWAYS_SOFTWARE=1
duduclaw-comp`) — putting it on cage itself makes Mesa refuse
("Not allowed to force software rendering when API explicitly selects a
hardware device") and cage segfaults. Also `$XDG_RUNTIME_DIR` must exist
with mode 0700 BEFORE cage starts (it segfaults, not errors, on a missing
dir — observed twice). Inside cage, comp negotiated
`PLATFORM_WAYLAND_KHR` EGL → GLES 3.2 on `llvmpipe (LLVM 19.1.7)` and
created its `wayland-1` socket exactly as in the headless run.

**Still unverified (at the time of this round):** window-management grabs
(move/resize drags — the smallvil-inherited `grabs/` module beyond plain
focus-click), multi-client, popup grabs (still no-op upstream), and
everything R1 (all software rendering; no frame-rate claims). **The first
three are closed by the "A1 multi-window round" below** — move/resize
drags, ≥3 concurrent real clients, and a real popup grab are all now
container-verified; R1 (software rendering, no frame-rate claims) remains
open, out of this round's scope.

## CD-0 codrive spike verification (2026-08-21)

Answers the go/no-go question for CD-0 in
`commercial/docs/DESIGN-codrive-desktop-2026-08.md` §5: agent seat + dual
cursor + injection socket + human-input freeze + emergency stop + audit
trail, wired into this crate's compositor body and exercised end-to-end,
not just compiled. Continues from a prior round's half-finished
`src/codrive/` module tree (`mod.rs`/`listener.rs`/`audit.rs`/
`keymap_ascii.rs`/`protocol.rs`/`cursor.rs`) that had never been declared
as a module from `main.rs` and had zero integration into `state.rs`/
`input.rs`/`winit_backend.rs` — so it had never compiled, let alone run.

### What changed

- **`src/main.rs`**: declares `mod codrive;`; calls
  `codrive::maybe_init_stdin_simulator(&mut event_loop)` (see "debug stdin
  simulator" below); the pre-existing `-c/--command` arg-parsing `match`
  was converted to `if let` (unrelated pre-existing clippy lint,
  `clippy::single_match`, that started failing once `-D warnings` ran
  against this file for the first time this round).
- **`src/state.rs`**: `DuduclawComp` gained `agent_seat: Seat<Self>`,
  `codrive: Arc<codrive::CodriveShared>`, `codrive_freeze_set_at:
  Option<Instant>`; `new()` calls `codrive::init(&mut seat_state, &dh,
  event_loop)` right after the human `"winit"` seat is created.
- **`src/input.rs`**: every arm of `process_input_event` (the human/
  `"winit"`-seat path) now calls `self.on_human_input(<kind>)` first. The
  keyboard arm's filter closure detects `Super+Esc` (`modifiers.logo &&
  handle.modified_sym() == Keysym::new(keysyms::KEY_Escape)`) and calls
  `data.emergency_stop("super+esc")` — structurally unreachable from the
  agent seat, since the agent's own key injection goes through a
  completely separate path (`codrive::handle_agent_inject`) that never
  calls into this file.
- **`src/winit_backend.rs`**: the `render_output` turbofish's custom-
  element type changed from `WaylandSurfaceRenderElement<GlesRenderer>`
  (previously paired with an always-empty `&[]`) to `SolidColorRenderElement`,
  fed `codrive::build_cursor_elements(human_pos, agent_pos,
  codrive.is_frozen())` computed fresh every redraw from each seat's
  `PointerHandle::current_location()`. `winit::init()` needed an explicit
  `::<GlesRenderer>()` turbofish once `GlesRenderer` stopped appearing
  anywhere else in the file for type inference to piggyback on.
- **`src/codrive/mod.rs`**: fixed two import paths the prior round had
  wrong and never compiled against (`XkbConfig` lives at
  `smithay::input::keyboard::XkbConfig`, not `smithay::wayland::seat::
  XkbConfig`); added `CodriveShared::is_frozen()`; added click-to-focus
  logic to the `InjectCmd::Button` press handler on the agent seat
  (raise + `keyboard.set_focus`) — without it, `InjectCmd::Text`/`Key` had
  no focused surface to route synthesized keys to, since each `wl_seat`'s
  keyboard focus is independent and nothing else ever set the agent
  seat's. Deliberately **duplicated** from (not refactored out of)
  `input.rs`'s human `PointerButton` arm, which already has VM-verified
  evidence above — this round did not want to touch or risk that path.
- **`src/codrive/keymap_ascii.rs`**: added `>` (shift+`.`) and `<`
  (shift+`,`) — needed once the live-run test tried to type a shell
  redirect (`echo x > file`) into `foot` and the run's own log surfaced
  `codrive: text op — character outside the ASCII-only synthesis table,
  skipped char='>'`, silently truncating the command into `echo x  file`
  (a no-op). Table is still an honest ASCII subset, just a slightly wider
  one now — see the module doc for what's still out of scope.
- **`src/codrive/debug_sim.rs`** (new file, ~95 lines): see below.
- **`Cargo.toml`**: unchanged in intent — noted here because it got
  externally corrupted mid-round and had to be restored; see "environment
  hazard hit this round" below.

### Debug stdin simulator (why it exists, and its blast radius)

Headless nested weston (this crate's container-level live-run host, see
"Nested headless live-run" above) advertises **zero input devices** —
`duduclaw-shell`'s `BUILD-LINUX.md` documents the identical upstream
constraint independently (`gnome`/weston's headless backend has no
`wl_seat` at all). That means the real human-input path
(`input.rs::process_input_event`, wired to actual winit-forwarded
keyboard/pointer events) structurally cannot fire inside this container —
and neither can the real `Super+Esc` detector. Both are implemented for
real hardware; hardware verification is VM/`cage` territory, same as this
file's own "VM cage real-seat input verification" section above did for
the base spike's move/resize grabs.

`src/codrive/debug_sim.rs` registers a calloop `Generic` source over
`std::io::stdin()` that turns two magic lines — `simulate_human` /
`simulate_super_esc` — into direct calls to `on_human_input`/
`emergency_stop`, letting this round's container verification exercise the
freeze/emergency-stop **state machine** end-to-end (flag flips, logs,
force-closes the connection) even though it can't exercise real hardware
event delivery. It is **opt-in via `DUDUCLAW_CODRIVE_DEBUG_STDIN=1`** —
unset (the default, including any real deployment), `maybe_init_stdin_simulator`
returns immediately without reading stdin or registering anything with the
event loop.

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
  -w /work/crates/duduclaw-comp \
  rust:bookworm bash -c '
set -uo pipefail
apt-get update -qq
apt-get install -y -qq --no-install-recommends \
  pkg-config libwayland-dev libxkbcommon-dev \
  libegl1 libgl1-mesa-dri libgles2 weston foot python3 >/dev/null

echo "==== build / clippy / test ===="
cargo build || exit 1
rustup component add clippy >/dev/null 2>&1
cargo clippy --all-targets -- -D warnings || exit 1
cargo test || exit 1

echo "==== layer 1+2+3: weston (headless) -> duduclaw-comp -> foot ===="
export XDG_RUNTIME_DIR=/tmp/xdg-runtime
mkdir -p $XDG_RUNTIME_DIR && chmod 0700 $XDG_RUNTIME_DIR
export LIBGL_ALWAYS_SOFTWARE=1

weston --backend=headless-backend.so --socket=wayland-host \
  --width=1280 --height=800 --log=/tmp/weston.log &
sleep 2

mkfifo /tmp/comp-stdin
exec 9<>/tmp/comp-stdin
WAYLAND_DISPLAY=wayland-host DUDUCLAW_CODRIVE_DEBUG_STDIN=1 RUST_LOG=info \
  /target/debug/duduclaw-comp <&9 >/tmp/duduclaw-comp.log 2>&1 &
sleep 2

WAYLAND_DISPLAY=wayland-1 foot >/tmp/foot.log 2>&1 &
sleep 2

echo "==== drive foot via the codrive socket: move, click, type a real shell command ===="
python3 - << "PYEOF"
import socket, json, time
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.connect("/tmp/xdg-runtime/duduclaw-codrive.sock")
for cmd in [
    {"op":"move","x":100.0,"y":100.0},
    {"op":"button","btn":"left","state":"press"},
    {"op":"button","btn":"left","state":"release"},
    {"op":"text","s":"echo codriveok987 > /tmp/codrive-proof.txt\n"},
]:
    s.sendall((json.dumps(cmd) + "\n").encode())
    print(s.recv(4096))
time.sleep(0.5)
PYEOF
cat /tmp/codrive-proof.txt   # should print codriveok987 — real proof text
                              # reached foots real shell via the agent seat

echo "==== simulate human input mid-stream -> expect freeze + drops ===="
echo simulate_human >&9
sleep 0.3
echo simulate_super_esc >&9
sleep 0.3

echo "==== audit trail ===="
cat $XDG_RUNTIME_DIR/duduclaw-codrive-audit.jsonl
'
```

(The actual verification run additionally used two longer Python scripts —
one to burst-send 400 rapid `move` commands in small chunks so the freeze
signal reliably lands mid-stream, one to drain and tally acks — omitted
above for brevity; the condensed version here still exercises every code
path, just with less precise latency data.)

### Evidence (verified 2026-08-21 run)

**Build/clippy/test, container-level:**

```
cargo build   -> Finished `dev` profile [unoptimized + debuginfo] target(s) in 3.18s
cargo clippy --all-targets -- -D warnings   -> Finished, zero warnings
cargo test    -> running 5 tests ... test result: ok. 5 passed; 0 failed
```

**Real client driven via the socket** — `foot`'s actual shell executed a
command synthesized entirely from `move`/`button`/`text` ops over the
unauthenticated injection socket, proven by a container-filesystem side
effect (stronger than a screenshot — no pixel comparison needed):

```
>>> {'op': 'text', 's': 'echo codriveok987 > /tmp/codrive-proof.txt\n'}  <<< {"ok":true,"frozen":false}
$ cat /tmp/codrive-proof.txt
codriveok987
```

**Freeze on human input, measured latency** (via the debug stdin
simulator — see above for why real hardware can't do this in a headless
container): a 400-command rapid `move` burst (5-command chunks, 2ms
between chunks, ~290ms total span) was in flight when `simulate_human` was
fired concurrently from the orchestrating shell:

```
PHASE2_BURST: sent=400 ok=10 frozen_dropped=390 dur_ms=290.79
```

The first 10 commands (2 chunks) landed before the freeze signal was
dispatched; every command from the 3rd chunk onward was cleanly dropped
(`{"ok":false,"frozen":true,"reason":"agent_seat_frozen"}`), not buffered —
matching DESIGN §3.1's "dropped, not buffered" freeze policy exactly.
Audit-log timestamps (millisecond resolution, `ts_ms`):

```
{"ts_ms":1787257189073,"kind":"freeze","op":"debug_stdin_simulated","frozen":true}
{"ts_ms":1787257189076,"kind":"inject_dropped","op":"move","x":101.0,"y":100.0,
  "detail":"agent seat frozen (human input active) — dropped, not buffered","frozen":true}
```

**Freeze latency: 3ms** (freeze audit event → first `inject_dropped`
audit event), well under the DESIGN §5 CD-0 target of <50ms. Cross-checked
client-side: `simulate_human` fired at `1787257189.074095`s, the client's
first observed `frozen:true` ack landed at `1787257189.076527`s — **2.4ms**
client-observed latency, consistent with the audit figure. All 390 drops
in this run resolved at the socket thread's own pre-check
(`listener.rs`) — none needed the narrower main-thread "queued-then-frozen
race" path in `codrive::handle_agent_inject` (that path's `latency_us`
logging is implemented and reviewed but did not get a live sample this
round — see honest-stub list).

Event-count cross-check (sanity, not just eyeballing): 4 (phase 1: move +
button press + button release + text) + 10 (pre-freeze burst) + 1
(post-resume move) = **15** `inject_applied` total; 390 `inject_dropped`;
4 + 400 + 1 = 405 attempted vs. 15 + 390 = 405 accounted for. Exact match.

**Resume + emergency stop:**

```
RESUME_ACK: {"ok":true,"frozen":false}
POST_RESUME_MOVE_ACK: {"ok":true,"frozen":false}
EMERGENCY_STOP_PUSH: b'{"event":"emergency_stop"}\n'
AFTER_PUSH_RECV (expect empty=EOF): b''
```

Resume clears the freeze (subsequent move applies cleanly); `Super+Esc`
(simulated) pushes `{"event":"emergency_stop"}` to the connected client
then force-closes it — the client's next `recv()` sees a clean EOF, not an
error, matching `emergency_stop`'s `shutdown(Both)` call in
`codrive/mod.rs`.

**Audit trail, entry-by-entry** — every session boundary and state
transition present, in order: `session_started` (phase 1) →
`inject_applied` ×4 → `session_ended` (phase 1 closed) → `session_started`
(phase 2) → `inject_applied` ×10 → `freeze` → `inject_dropped` ×390 →
`resume` → `inject_applied` ×1 (post-resume move) → `emergency_stop` →
`session_ended`. No gaps, no out-of-order timestamps, no malformed JSON
lines (every line parsed cleanly with Python's `json.loads` in the
verification script).

**Second cursor / render path**: `duduclaw-comp` ran continuously across
the whole multi-second verification (foot connect → drive → burst →
freeze → resume → emergency stop → force-close) with zero panics and zero
error-level log lines — the redraw loop's `render_output` call with the
`SolidColorRenderElement` custom-elements slice (both cursors, recomputed
every frame) executed successfully every frame for the entire run.
**Not** verified this round: actual on-screen pixel distinctness between
the two cursor shapes (no screenshot/QMP framebuffer read available in
this container — same category of limitation as R1 above; a real visual
check is VM/QMP acceptance-side work per the task brief).

### Honest stub / limitation list (this round)

- **Injection socket is unauthenticated by design at CD-0** — already
  flagged in `listener.rs`'s own module doc from the prior round; single
  connection at a time, chmod 0600, `$XDG_RUNTIME_DIR`-scoped. CD-1 adds
  caller-identity auth. Unchanged this round, restated here for
  completeness.
- **Super+Esc real-hardware detection is implemented but container-
  unverified** — `input.rs`'s keyboard filter closure correctly checks
  `modifiers.logo && handle.modified_sym() == Keysym::new(keysyms::KEY_Escape)`,
  reviewed against smithay 0.7.0's actual API (not guessed), but headless
  weston has no keyboard device to originate a real Super+Esc from. The
  debug stdin path verifies everything downstream of detection (the
  `emergency_stop` state machine itself); VM/`cage` round needed to close
  this, same as the base spike's move/resize grabs.
- **`keymap_ascii.rs` is still an ASCII subset**, now including `<`/`>`.
  No CJK, no full Unicode, no non-US layouts — unchanged limitation from
  the prior round, just a slightly wider table.
- **Main-thread "queued-then-frozen race" path unverified live** — the
  `handle_agent_inject` code that logs `latency_us` for a command that was
  already queued in the calloop channel before freeze flipped exists and
  was code-reviewed, but every drop observed this round resolved at the
  earlier socket-thread pre-check instead (arguably a *stronger* result —
  freeze took effect before any command even reached the channel — but it
  means this specific code path has zero live-run coverage). A tighter
  race (larger burst, smaller chunks, zero pre-delay) might hit it in a
  future round; not required for CD-0's own <50ms target, which this
  round's 3ms figure already clears via the earlier checkpoint.
- **Debug stdin simulator is new, CD-0-only tooling** — real deployments
  never set `DUDUCLAW_CODRIVE_DEBUG_STDIN`, and the function is a true
  no-op (no stdin read, no event-loop registration) when unset. It exists
  solely because headless nested weston cannot originate real human input
  events at all (see "Debug stdin simulator" above) — VM/`cage` real-seat
  verification (this file's "VM cage real-seat input verification"
  section for the base spike) is the eventual real-hardware closure for
  both freeze-latency and Super+Esc, left to the acceptance side per the
  task brief ("VM QMP 真機級留驗收端").
- **Click-to-focus on the agent seat is a deliberate near-duplicate** of
  `input.rs`'s human `PointerButton` arm rather than a shared helper — see
  "What changed" above for the reasoning (don't touch the already-VM-
  verified human path).

### Environment hazard hit this round (not a crate defect)

Partway through this round, `Cargo.toml` and four already-edited source
files (`main.rs`, `state.rs`, `input.rs`, `winit_backend.rs`) were found
reverted to their pre-round baseline on disk — most tellingly,
`Cargo.toml`'s `version` and the `smithay` dependency's `version` had both
been rewritten to `"1.62.0"` (a version of smithay that doesn't exist on
crates.io; `0.7.0` is still the latest published release) and the
`serde`/`serde_json` dependencies had vanished entirely. This has every
hallmark of an unrelated concurrent process in the same working tree doing
a blanket version-string bump/replace that matched *every* `version = "…"`
line in the TOML file, including a third-party dependency pin it had no
business touching (`crates/duduclaw-comp/` is git-untracked and
`publish = false` — no release tooling should be touching it at all).
Restored by hand (re-diffing against what this round had actually written)
and re-verified with a full build/clippy/test pass before continuing. Flag
for whoever owns the version-bump tooling: it should not be walking
`crates/duduclaw-comp/Cargo.toml`.

### Acceptance re-run findings (2026-08-21, verification side)

The acceptance side re-ran the one-shot command above independently and
added one probe the implementation round's harness did not have: **inject
over a brand-new connection while frozen**. It exposed a real red-line
violation — `accept_loop` used to clear `frozen` on every new connection
("a new connection is a new session"), so an agent could bypass an active
human freeze by simply reconnecting, violating DESIGN-codrive-desktop §6
red line 3 ("人輸入優先凍結無例外…agent 不可攔截/繞過"). Fixed in
`listener.rs` (connection lifecycle no longer touches `frozen`; only the
explicit `resume` op clears it — `terminated` still resets on reconnect,
unchanged) and re-verified end-to-end:

```
RECONNECT-WHILE-FROZEN inject -> {"ok":false,"frozen":true,"reason":"agent_seat_frozen"}
resume ->                        {"ok":true,"frozen":false}
post-resume inject ->            {"ok":true,"frozen":false}
```

Audit trail for the same run shows `session_started` with `"frozen":true`
(the new connection observes, not resets, the freeze), the dropped inject,
the explicit `resume`, and the applied post-resume inject, in order.
Carry-forward for CD-1: `resume` issuance moves to the human-side channel
entirely (at CD-0 the socket client is the trusted gateway, so
socket-`resume` stands in for the human "交還" action — documented
simplification, not the end-state contract).

## CD-0 VM/QMP real-seat verification (verified 2026-08-21)

Closes the three gaps the container-level CD-0 round above explicitly left
for "acceptance-side VM/QMP" work (DESIGN-codrive-desktop-2026-08.md §5 CD-0
line item requires all of these QMP/VM-verified, not just container-verified):
real-hardware freeze latency, real `Super+Esc` (not the debug stdin
simulator), and visual dual-cursor distinctness. Run inside the same
appliance QEMU VM Shell-S2 already used for `duduclaw-shell`'s real-seat
round (this file's own "VM cage real-seat input verification" section above)
— same disk, same `cage`/seatd/virtio-gpu stack, reused rather than
rebuilt.

### Corrected premise: the working VM is arm64, not x86-64

The task brief for this round assumed the appliance image was x86-64
("這是 x86 image"). Checked before trusting that, per repo doctrine ("以證據
為準，不以自己的假設為準"): `appliance/mkosi.conf`'s `[Distribution]
Architecture=` default is indeed `x86-64`, **but** the actual working VM
disk in use (`appliance/.vm/duduclaw-os-vm.raw`, the same Shell-S2 working
copy) was built from an **arm64** `mkosi.output/duduclaw-os.raw` — confirmed
by reading the PE header of `mkosi.output/duduclaw-os.efi` (machine type
`0xaa64` = ARM64) and the kernel (`file duduclaw-os.vmlinuz` → "Linux kernel
ARM64 boot executable Image"). This matches `appliance/run-vm.sh`'s own
`APPLIANCE_ARCH` default (`arm64`, deliberately different from
`smoke-qemu.sh`'s `x86-64` default — a local/QEMU-smoke-test vs.
shipping-target split documented in `mkosi.conf.d/10-arch-arm64.conf`'s own
comment). Booted with `qemu-system-aarch64 -machine virt,accel=hvf -cpu
host` (Apple Silicon HVF acceleration — fast, not the slow TCG path the
task brief anticipated for an assumed x86-64 target) rather than
`qemu-system-x86_64`. The comp binary built for this round (below) is
therefore also aarch64, matching Docker Desktop's default `linux/arm64`
container platform on this Apple Silicon host — no cross-compilation
needed, byte-for-byte the same toolchain path this file's earlier sections
already used.

### Getting the codrive-enabled binary into the VM

The disk already had a comp binary injected from an earlier (pre-codrive)
round (`/usr/local/bin/duduclaw-comp`, 139,931,832 bytes, dated inside the
guest filesystem before this round's `mod.rs`/`listener.rs` codrive work
existed). Rebuilding via this file's own "CD-0 codrive spike verification"
one-shot command's build step reused the still-warm `duduclaw-shell-cargo`/
`duduclaw-shell-cargo-git`/`duduclaw-shell-target` named Docker volumes from
that same round (`cargo build` completed in 0.21s — nothing to recompile),
producing an aarch64 ELF that was copied out of the volume via a throwaway
container (`docker run --rm -v duduclaw-shell-target:/target -v
<host-dir>:/out rust:bookworm cp /target/debug/duduclaw-comp /out/`).

Injection recipe (same shape as this file's "VM cage real-seat input
verification" section, spelled out in full here since that section only
summarized it): with the VM shut down, loop-mount the disk's partition 2
(`duduclaw-root-a`, ext4, confirmed via `parted -s <disk>.raw print`) inside
a `--privileged debian:bookworm` container —

```bash
LOOPDEV=$(losetup -f)
losetup -P "$LOOPDEV" /vm/duduclaw-os-vm.raw
# no udev in a container: partition device nodes need manual mknod from
# /sys/class/block/<loop>/<loop>pN/dev's "major:minor"
for p in /sys/class/block/$(basename $LOOPDEV)p*; do
  name=$(basename "$p"); devt=$(cat "$p/dev")
  mknod "/dev/$name" b "${devt%%:*}" "${devt##*:}"
done
mount -o rw "${LOOPDEV}p2" /mnt/root
cp /inject/duduclaw-comp /mnt/root/usr/local/bin/duduclaw-comp   # overwrite
```

Three things were changed in this same mount session: (1) the comp binary
swap above; (2) root's `/etc/shadow` hash rewritten to a known password
(`openssl passwd -6`) via an `awk -v NEWHASH=... -f set_root_pw.awk` field
rewrite — the disk already had *a* root hash set from an earlier round, but
its plaintext was unknown, so a fresh known one was needed for this round's
non-interactive serial login; (3) `serial-getty@ttyAMA0.service` enabled
(`ln -sf .../serial-getty@.service .../getty.target.wants/`) — the disk had
no getty on the arm64 `virt` machine's PL011 UART (`ttyAMA0`) enabled at
all, only `getty@tty1.service` (the virtual-console one, which
`duduclaw-kiosk.service` `Conflicts=` and stops anyway), so there was no
serial login path before this. **Real finding, not a comp bug**: contrary
to `duduclaw-shell`'s BUILD-LINUX.md stage B-③ note ("`/bin/login` does not
exist in the image... needs the `login` package added"), the current
`mkosi.conf` (`Packages=`) already lists `login` and `python3` — both were
present and working without any package-level fix needed; what was actually
missing was the *serial getty unit*, not the `login` binary. A prior
round's `duduclaw-debug-shell.service` custom unit (mentioned in that same
BUILD-LINUX.md section) was not present on this specific working-copy disk
at verification time — replaced here with the simpler stock
`serial-getty@ttyAMA0.service`, which needs no custom unit file at all.

Each mount/unmount was wrapped in a `trap cleanup EXIT` (`umount; losetup
-d`) and followed by `e2fsck -f -y` on the partition — caught and cleanly
recovered from one operator mistake this round (a first injection attempt
died mid-script on a shell-quoting bug before reaching its own `umount`/
`losetup -d`, leaving the loop device attached at the host-Docker-VM kernel
level across container exits — `losetup -a` in a fresh container confirmed
it was still attached; detached by hand, then `e2fsck -f -y` confirmed the
filesystem was undamaged before retrying). Loop devices on Docker Desktop
for Mac are **not** container-scoped — they persist at the shared Linux VM
kernel level after a `--privileged` container exits, so an aborted
loop-mount script must be detached explicitly, not assumed to clean itself
up with the container.

### Boot verification and seat handoff

Booted headless: `-display none` (no host window) plus `-device
virtio-gpu-pci -device qemu-xhci,id=usb -device usb-tablet -device
usb-kbd`, `-qmp tcp:127.0.0.1:47022,server,nowait -serial
tcp:127.0.0.1:47021,server,nowait`. Confirmed **`-display none` does not
disable `screendump`**: a `screendump` QMP call ~45s after launch returned
a full 1280×800 frame already showing the production dark Home kiosk
(`duduclaw-kiosk.service` had auto-started and rendered correctly), proving
QEMU's virtio-gpu console surface stays live and dumpable independent of
whether a host UI window exists — useful precedent for any future headless
QMP-driven acceptance work on this image (no need for `-vnc`/a host
display).

`duduclaw-kiosk.service` (the production kiosk, `cage -- chromium`
launching the dark-Home dashboard) auto-starts on boot because the
detect-display condition (`duduclaw-kiosk-detect-display.sh`) reads the
guest-visible virtio-gpu connector as "connected" regardless of `-display`
choice — this is a guest-kernel DRM connector state, not a host-UI concern.
It was stopped (`systemctl stop duduclaw-kiosk.service`) before starting
the manual verification session below, since both processes compete for
the same `seatd`-brokered DRM device and only one `cage` client can hold it
at a time.

Login used the systemd/PAM-managed serial session (`/bin/login` on
`ttyAMA0` via the newly-enabled getty), which — unlike a bare shell —
automatically creates `/run/user/0` (mode 0700) via `pam_systemd`'s
`user-runtime-dir@0` unit, so `$XDG_RUNTIME_DIR` needed no manual setup
this round (unlike the container-level round's headless weston path, which
had no login manager at all).

`cage -d -- env LIBGL_ALWAYS_SOFTWARE=1 RUST_LOG=info duduclaw-comp -c
foot` launched cleanly: EGL negotiated `PLATFORM_WAYLAND_KHR` → GLES 3.2 on
`llvmpipe (LLVM 19.1.7, 128 bits)` (two harmless `DRI2: failed to create
screen` warnings preceded the working `kms_swrast` fallback — foot's own
direct-rendering probe, not a comp issue, same shape as this file's
container-round EGL notes), `foot` connected as `duduclaw-comp`'s first
xdg-shell client, and the codrive listener came up at
`/run/user/0/duduclaw-codrive.sock` with its audit log alongside it. Zero
panics and only those two benign warning lines across the entire
multi-minute session (`grep -ci error /root/comp.log` → 2, both the DRI2
lines; `grep -c panic` → 0).

**One unplanned but informative event**: the agent seat froze itself
*before any deliberate test began* — audit line 1,
`{"kind":"freeze","op":"pointer_motion_absolute","frozen":true}`, fired the
instant `cage` attached the real `usb-tablet` device, because that device
reports an initial absolute position on attach. This is the freeze
mechanism correctly doing its job against a real (if incidental) hardware
event, and it meant every deliberate test below had to issue an explicit
`resume` first — consistent with DESIGN §6 red line 3 ("人輸入優先凍結無
例外"): even an incidental real event takes priority, no allowance for "but
nothing meant to move yet."

### Item 1 — real-hardware freeze (PASS)

Driven via a guest-local Python script (`python3` is present in the image
per `mkosi.conf`'s `Packages=` — no injection needed, unlike the task
brief's contingency plan) connecting to the codrive Unix socket and
bursting 400 `move` commands (3ms spacing, ~1.4s span) at the agent seat,
while the **host** fired a real `input-send-event` QMP keypress
(`shift`, a harmless key) partway through — landing on the guest's real USB
HID keyboard device, through `seatd`/libinput/Wayland/`cage`/comp's own
`input.rs::process_input_event`, exactly the same code path this file's
earlier "VM cage real-seat input verification" section already proved for
plain keyboard/mouse forwarding.

Audit trail (guest path `/run/user/0/duduclaw-codrive-audit.jsonl`,
`grep -n "freeze\|resume\|session_started\|session_ended"`, line numbers
from that grep):

```
179:{"ts_ms":1787287763607,"kind":"freeze","op":"keyboard","frozen":true}
```

`"op":"keyboard"` — not `"debug_stdin_simulated"` — is the load-bearing
fact here: this freeze was fired by `input.rs`'s real human-seat keyboard
arm, from a QMP-injected key event that actually traversed the kernel
input stack, not the container round's stdin-simulator shortcut. Line-by-
line context around the freeze:

```
{"ts_ms":1787287763607,"kind":"freeze","op":"keyboard","frozen":true}
{"ts_ms":1787287763611,"kind":"inject_dropped","op":"move","x":123.0,"y":123.0,
  "detail":"agent seat frozen (human input active) — dropped, not buffered","frozen":true}
{"ts_ms":1787287763612,"kind":"inject_dropped","op":"move","x":119.0,"y":119.0,
  "detail":"frozen at execution time (queued-then-frozen race, latency_us=Some(4568))","frozen":true}
```

**Freeze latency: 4ms** (freeze audit event at `763607` → first
`inject_dropped` at `763611`) — real hardware path end-to-end (QMP → QEMU
USB HID → guest kernel evdev → seatd/libinput → Wayland → `cage` →
`duduclaw-comp`'s winit backend → `input.rs::on_human_input` → codrive
freeze flag → next agent command dropped), well under the DESIGN §5 CD-0
<50ms target and in the same ballpark as the container round's simulated
3ms figure (expected — the actual freeze-to-drop path is the same
single-calloop-dispatch mechanism either way; only the *trigger* origin
differs between the two rounds).

Burst result (`/root/burst_result.txt`, written by the guest script):

```
BURST: sent=400 ok=173 frozen_dropped=227 errs=0 dur_ms=1405.66
```

**This round also exercised the "queued-then-frozen race" path the
container round's own honest-stub list flagged as never hit live**
(`codrive::handle_agent_inject`'s main-thread re-check, as opposed to
`listener.rs`'s socket-thread pre-check) — visible above as
`"detail":"frozen at execution time (queued-then-frozen race,
latency_us=Some(4568))"`. The tighter real-hardware timing (a real kernel
round-trip is slower than a same-process channel send) made commands land
in the channel queue before the freeze flag flipped, closing that specific
gap in coverage.

### Item 2 — real Super+Esc emergency stop (PASS)

A guest-local watcher script held a connection open (having first issued
`resume` + one `move` to prove it was live), then the **host** fired a real
`Super+Esc` chord via QMP `input-send-event`: `meta_l` down, `esc` down,
`esc` up, `meta_l` up — four separate `input-send-event` calls, matching
how a real keyboard reports a held-modifier chord. `input.rs`'s keyboard
filter closure (`modifiers.logo && handle.modified_sym() ==
Keysym::new(keysyms::KEY_Escape)`) is the same code this file's earlier
"VM cage real-seat input verification" round already proved reachable for
plain `Esc`/`Super-K`; this round is the first to actually hold `Super`
while pressing `Esc` on real hardware.

Watcher's observed sequence (`/root/estop_watch.log`):

```
resume: b'{"ok":true,"frozen":false}\n'
move: b'{"ok":true,"frozen":false}\n'
PUSHED: b'{"event":"emergency_stop"}\n'
EOF_OBSERVED (connection force-closed)
FINAL_BUFFER: b'{"event":"emergency_stop"}\n'
POST_STOP_NEW_CONN_INJECT: b'{"ok":false,"frozen":true,"reason":"agent_seat_frozen"}\n'
```

Audit trail:

```
{"ts_ms":1787287838227,"kind":"emergency_stop","detail":"super+esc","frozen":true}
```

`"detail":"super+esc"` — the real detector's reason string, not
`"debug_stdin_simulated_super_esc"`. The post-stop reconnect probe is worth
spelling out: a *new* connection resets `terminated` (per `listener.rs`'s
documented state machine) but not `frozen`, so its inject attempt was
rejected with `"reason":"agent_seat_frozen"` rather than
`"session_terminated"` — both are correct per the design (`terminated`
guards the just-force-closed connection's own tail; `frozen` is the
still-active human-priority gate, cleared only by an explicit `resume`),
and this round is what actually exercised that reconnect-after-real-
emergency-stop path end-to-end rather than by inspection.

### Item 3 — dual-cursor visual distinctness (PASS)

Two QMP `screendump`s, both saved as PNG in `appliance/.vm/s2-evidence/`:

- **`cd0-cursors-live.png`**: agent cursor issued a `move` to `(900, 500)`
  after `resume` (agent seat live, unfrozen) — renders as the amber cross/
  reticle (`AGENT_COLOR_LIVE`, `cursor.rs`) at that position; the human
  cursor renders as a small pale square (`HUMAN_COLOR`) at its own
  independent position (left over from the incidental `usb-tablet` attach
  event noted above). Directly `Read` and visually inspected: the two
  cursors are unambiguously distinct in both shape (square vs.
  cross/reticle) and color (pale white vs. amber), exactly matching DESIGN
  §3.3.2's "與人游標明確異形異色".
- **`cd0-cursors-frozen.png`**: a real QMP absolute-pointer move
  (`input-send-event`, `type: abs`) relocated the human cursor to a new
  position — this is itself a genuine human-seat event, so it froze the
  agent seat as a side effect (audit: `{"kind":"freeze",
  "op":"pointer_motion_absolute","frozen":true}`, confirmed before the
  screendump). The agent cursor, still at `(900, 500)`, is now rendered in
  `AGENT_COLOR_FROZEN` (dimmed red) — visually confirming the frozen-state
  color cue DESIGN §3.4 calls for ("系統級『共駕中』指示") actually renders
  correctly on real hardware, not just in `cursor.rs`'s source.

Both screenshots also incidentally show `foot`'s terminal with the Item-1
burst-test's earlier shell output still on screen (`echo cd0agentok987 >
/tmp/cd0-agent-proof.txt`), giving a second, independent visual
confirmation (beyond the file-system side-effect check below) that agent-
injected keystrokes really did reach a real xdg-shell client rendered by
`duduclaw-comp` under `cage`.

### Bonus — real-seat agent injection reaching a real shell (PASS, not one
of the three named items but exercised first as a smoke test)

Before the freeze/emergency-stop tests, a plain move→click→type sequence
over the codrive socket (`move` to `(100,100)`, left `button` press+
release, `text` synthesizing `echo cd0agentok987 > /tmp/cd0-agent-proof.txt
\n`) was sent to confirm the pipeline was alive on real hardware before
testing its failure modes. `cat /tmp/cd0-agent-proof.txt` on the guest
afterward printed `cd0agentok987` — a real shell command, synthesized
entirely from agent-seat keystrokes, executed by `foot`'s real shell,
running under `cage` on the VM's virtio-gpu output. Strictly stronger
evidence than the earlier container round's identical check (real seat
stack vs. headless weston), included here for completeness since it's the
precondition every other test in this section depends on.

### Cleanup

`{"execute":"quit"}` over QMP shut the VM down cleanly (confirmed via `ps`
— no leftover `qemu-system-aarch64` process). The one operator mistake
noted above (an aborted loop-mount leaving a loop device attached at the
Docker-Desktop-for-Mac kernel level) was caught and cleaned up
(`losetup -d`) before the retry, with `e2fsck -f -y` confirming no
filesystem damage either before or after. The disposable `vars-cd0.fd`
(UEFI varstore working copy, wiped fresh on launch as this file's
`run-vm.sh` section already documents doing) was deleted after the run;
the disk image itself (`duduclaw-os-vm.raw`) now permanently carries the
codrive-enabled comp binary, the known root password, and the enabled
serial getty — all three are durable changes to the shared Shell-S2/CD-0
working copy, not undone after this round (intentional: the whole point
was to leave a debuggable disk for whichever round needs it next).

### Honest stub / limitation list (this round)

- **Injection socket auth**: unchanged CD-0-known-gap, restated for
  completeness (see the container round's own note above).
- **`keymap_ascii.rs`'s ASCII-only table**: unchanged; not exercised
  further than the container round already did.
- **Root password / serial getty are now permanent disk changes**: fine
  for a shared debug/verification working copy, but anyone treating this
  disk as "the same as what Shell-S2 shipped" should know a debug login
  path now exists on it that didn't reliably exist before this round.
- **Frame-rate / DPI claims**: none made, none relevant — same R1 scope
  note as every other section of this file (all software rendering under
  QEMU).
- **Single verification pass, not repeated N times**: each of the three
  items passed on its first real attempt this round (no retries needed,
  so the stop-loss-at-5-attempts contingency in the task brief was never
  invoked) — a second independent run was not performed to check for
  flakiness, same evidentiary bar the container round itself used.

## CD-1 comp-side additions (2026-08-21)

Closes the three CD-0 carry-forward gaps DESIGN-codrive-desktop-2026-08.md
§9 named ("CD-1 承接欠帳：socket 未鑑別、resume 走 socket 暫代人側交還、
keymap ASCII 子集") plus three new comp-side primitives CD-1 needs: a
`status` query, named functional keys, and a target highlight box. All six
requirements landed in one round; see each file's own doc comments for the
detailed "why."

### What changed

- **`src/codrive/mod.rs`**: `CodriveShared` gained `auth_token: Option
  <String>` (generated fresh every process start via `/dev/urandom`, no
  new crate dependency), `check_token()` (best-effort constant-time-ish
  compare), and `push_event()` (best-effort state-transition push to the
  active connection, reused for both `frozen` and `resumed`). New
  `DuduclawComp::human_resume()` — the only code path that clears
  `frozen`, reachable solely from `input.rs`'s Super+Enter and
  `debug_sim.rs`'s `simulate_super_enter`. `handle_agent_inject` gained
  `KeyName` and `Highlight` arms (`Resume`/`Status` stay as
  never-actually-reached fail-safe arms, matching the pre-existing
  `Resume` pattern). `DuduclawComp::on_human_input` now pushes
  `{"event":"frozen"}` on the not-frozen→frozen transition.
- **`src/codrive/listener.rs`**: new `authenticate()` gate — every
  connection's first line must be `{"op":"auth","token":"<hex>"}` before
  anything else. **Security-relevant reordering**: session bookkeeping
  (clear `terminated`, record `session_started`, publish `active_conn`)
  moved from unconditional-on-`accept()` (in `accept_loop`) to
  after-auth-succeeds (in `handle_conn`) — the same class of gap the CD-0
  acceptance re-run already caught once for the plain-reconnect case (see
  that section above), now closed at the socket layer itself rather than
  relying on `frozen` alone staying untouched. `resume` is now
  unconditionally denied (`resume_is_human_only`); `status` is answered
  directly from the shared atomics, bypassing both the `frozen` and
  `terminated` gates (it's read-only and never touches the seat).
- **`src/codrive/protocol.rs`**: `InjectCmd` gained `KeyName`, `Status`,
  `Highlight` variants; new standalone `AuthLine` struct (deliberately NOT
  an `InjectCmd` variant — see its doc comment).
- **`src/codrive/keymap_ascii.rs`**: `ascii_to_xkb` now covers the full
  printable-ASCII range (0x20..=0x7E) — 23 punctuation marks added this
  round (the shifted number row, backtick/tilde, brackets/braces,
  backslash/pipe, quotes, colon, question mark) on top of CD-0's smaller
  table. New `key_name_to_xkb` allowlist (14 named keys). Non-ASCII
  (CJK/Unicode) stays unsupported — see "Honest stub" below, this is a
  researched decision, not an unresearched gap.
- **`src/codrive/cursor.rs`**: `AGENT_COLOR_LIVE` changed from private to
  `pub(super)` so `highlight.rs` can reuse the exact same amber, one
  constant instead of two copies that could drift.
- **`src/codrive/highlight.rs`** (new file, ~110 lines): target highlight
  box — `clamp_highlight_ms` (pure, unit-tested) and
  `DuduclawComp::codrive_highlight_elements` (called once per redraw from
  `winit_backend.rs`; clears the highlight as a side effect once expired).
  Four `SolidColorRenderElement` bars forming a hollow border, same
  zero-texture mechanism as `cursor.rs`.
- **`src/state.rs`**: `DuduclawComp` gained `codrive_highlight: Option<
  (Rectangle<f64, Logical>, Instant)>`, initialized `None`.
- **`src/input.rs`**: the keyboard filter closure that already detects
  Super+Esc now also detects Super+Enter (`Keysym::new(keysyms::
  KEY_Return)`) and calls `data.human_resume()` — structurally
  unreachable from the agent seat, same guarantee Super+Esc already has.
- **`src/codrive/debug_sim.rs`**: third magic stdin line,
  `simulate_super_enter` → `human_resume()` directly (headless containers
  have no keyboard device to originate a real Super+Enter from — real
  hardware coverage is VM/`cage` territory, same split as Super+Esc).
- **`src/winit_backend.rs`**: the redraw path's custom-elements vector now
  also gets `state.codrive_highlight_elements(Instant::now())` appended
  after the two cursors.
- **`Cargo.toml`**: unchanged — no new dependency was needed (the auth
  token uses `/dev/urandom` + a hand-rolled hex encoder, both already
  necessary since this crate is Linux-only). Checked before finishing this
  round per the task brief's explicit instruction not to touch it; no
  unexplained diff was found this time (contrast with the CD-0 round's
  "Environment hazard hit this round" note above).

### Wire protocol (final CD-1 shape)

Every connection's mandatory first line:

```
→ {"op":"auth","token":"<64-hex-char token from $XDG_RUNTIME_DIR/duduclaw-codrive.token>"}
← {"ok":true,"authenticated":true}          (success — proceed to the ops below)
← {"ok":false,"error":"auth_failed"}        (wrong/missing/malformed — connection closed)
```

Ops available after authentication (all existing CD-0 shapes unchanged
except `resume`; new ones marked **CD-1**):

| op | example | notes |
|---|---|---|
| `move` | `{"op":"move","x":100.0,"y":200.0}` | unchanged |
| `button` | `{"op":"button","btn":"left","state":"press"}` | unchanged |
| `key` | `{"op":"key","keycode":38,"state":"press"}` | unchanged (raw XKB keycode) |
| `text` | `{"op":"text","s":"hello"}` | unchanged (ASCII synthesis, now full printable range) |
| `key_name` **(CD-1)** | `{"op":"key_name","name":"enter","state":"press"}` | allowlist: enter/tab/backspace/escape/delete/space/up/down/left/right/home/end/pageup/pagedown |
| `status` **(CD-1)** | `{"op":"status"}` → `{"ok":true,"frozen":false,"terminated":false}` | read-only, answered even while frozen, never touches the seat |
| `highlight` **(CD-1)** | `{"op":"highlight","x":0.0,"y":0.0,"w":100.0,"h":40.0,"ms":800}` | `ms` optional, default 800, clamped [100,5000]; frozen → dropped like any other injection op |
| `resume` **(changed)** | `{"op":"resume"}` → always `{"ok":false,"error":"resume_is_human_only"}` | CD-0 behavior (clears `frozen`) is gone; "交還" is Super+Enter only |

Async push events on the connection (best-effort, unchanged shape from
CD-0's `emergency_stop`, now joined by two new ones):

```
{"event":"frozen"}          (CD-1: pushed on the not-frozen→frozen transition)
{"event":"resumed"}         (CD-1: pushed when human_resume actually clears frozen)
{"event":"emergency_stop"}  (unchanged from CD-0, connection force-closed right after)
```

### Token file

`$XDG_RUNTIME_DIR/duduclaw-codrive.token` — 64 lowercase hex characters (32
random bytes from `/dev/urandom`), mode 0600, created (not chmod'd
after-the-fact) with the correct mode via `OpenOptionsExt::mode` to avoid
any window where the secret is briefly world/group-readable. Regenerated
every process start; a stale file from a prior run is removed first. If
either the read from `/dev/urandom` or the file write fails, the injection
socket is disabled entirely for that run (fail-closed — logged at `error`
level) rather than falling back to any unauthenticated mode.

### Super+Enter

Human-side "交還", the CD-1 replacement for CD-0's socket-`resume`
stand-in. Detected in the exact same keyboard filter closure as Super+Esc
in `input.rs` (`modifiers.logo && handle.modified_sym() ==
Keysym::new(keysyms::KEY_Return)`), which only ever sees real/winit-seat
events — there is no code path from an injected agent key event into this
closure, so the agent cannot forge its own resume. Clears `frozen`, logs an
audit line (`kind:"resume", op:"human_super_enter"`), and pushes
`{"event":"resumed"}` to the connected client — but only if the seat was
actually frozen (a resume attempt while already live is a silent no-op,
per the task brief: no audit line, no event push).

### Verification (2026-08-21, this round)

**Build/clippy/test, container-level** (same volumes/command shape as the
CD-0 section above, `cargo check --all-targets` / `cargo clippy
--all-targets -- -D warnings` / `cargo test`, run separately rather than
chained in one script this round for faster iteration):

```
cargo check --all-targets   -> Finished, zero warnings, zero errors (first try)
cargo clippy --all-targets -- -D warnings   -> Finished, zero warnings (first try)
cargo test                  -> running 32 tests ... test result: ok. 32 passed; 0 failed
```

32 tests (up from CD-0's 5): auth token compare/generation (`codrive::
tests`), highlight ms clamp + border geometry (`codrive::highlight::
tests`), full-ASCII coverage + key_name allowlist (`codrive::keymap_ascii::
tests`), and — the load-bearing one — `codrive::listener::tests::
unauthenticated_connection_does_not_clear_terminated`, a real-socket
integration test that simulates a just-happened emergency stop, connects
with a WRONG token, and asserts `terminated` was never cleared. Companion
tests cover a correctly-authenticated connection, `resume` being denied
without ever clearing an active freeze, and `status` answering while
frozen without touching seat state.

**Live functional smoke test** (weston-headless → duduclaw-comp → foot,
same three-layer stack as CD-0's own live-run sections, driven via a real
socket client): wrong-token auth denied, correct-token auth accepted,
`status` while live, `resume` denied over the socket, then a real
functional proof stronger than CD-0's own — `text` synthesized a shell
command WITHOUT its own trailing Enter, and a separate `key_name":"enter"`
press+release was what actually submitted it to `foot`'s real shell
(`cat /tmp/cd1-proof.txt` → `cd1agentok654`), proving `key_name` drives the
agent seat for real, not just that `validate()` accepts it. `highlight`
was accepted and applied without any panic across the whole run (the
redraw path's `codrive_highlight_elements` executed every frame with the
new custom element in the slice) — audit line confirms `op":"highlight"`
with `x`/`y` recorded. Then: `simulate_human` (debug stdin) froze the
seat — a *second, freshly-authenticated* connection's `{"op":"status"}`
correctly read back `"frozen":true` (proving the freeze-during-a-new-
connection case DESIGN §6 red line 3 requires, matching the CD-0
acceptance re-run's earlier finding for the analogous case) — then
`simulate_super_enter` cleared it, verified via a third connection's
`status` reading `"frozen":false`. Audit trail end-to-end for this run
(abbreviated): `auth_fail(token mismatch)` → `session_started` →
`resume_denied` → `inject_applied`×7 (move/button×2/highlight/text/
key_name×2) → `session_ended` → `session_started`/`session_ended` (the
status-only connection) → `freeze(op:debug_stdin_simulated)` →
`session_started(frozen:true)`/`session_ended` → `resume(op:
human_super_enter)` → `session_started(frozen:false)`/`session_ended`. No
gaps, no out-of-order timestamps. Separately verified: a connection that
opens and disconnects WITHOUT ever sending an auth line (EOF before the
first `read_line` returns any bytes) does not crash or hang the
compositor — `authenticate`'s `Ok(0) => deny(...)` arm handles it, process
stayed alive and error/panic-free (`grep -c panic` → 0) afterward.

### Honest stub / limitation list (this round)

- **Real-hardware Super+Enter is implemented but container-unverified** —
  same category as CD-0's Super+Esc: headless weston has no keyboard
  device to originate a real chord from. The debug stdin path
  (`simulate_super_enter`) verifies everything downstream of detection;
  closing this for real hardware is VM/`cage`/QMP acceptance-side work,
  left to the acceptance round per the task brief ("留 VM 輪").
- **Highlight box visual rendering is implemented but not visually
  verified** — this round confirmed the code path executes every redraw
  without panicking and that the `highlight` op is accepted/applied/
  audited correctly, but headless weston has no screendump/framebuffer
  capture available (same limitation category as CD-0's cursor-
  distinctness check, which needed the VM/QMP round's `screendump` to
  close). A real pixel-level check (does the amber hollow border actually
  appear at the right position/size, distinct from the two cursors) is
  VM/QMP acceptance-side work, same as CD-0's own dual-cursor visual
  check.
- **Non-ASCII (CJK/Unicode) text synthesis is still unsupported** — this
  round specifically researched whether it's feasible (checked
  `smithay::input::keyboard::KeyboardHandle`'s actual 0.7.0 API rather
  than guessing) and found real capability (`set_keymap_from_string`/
  `set_xkb_config`/`with_xkb_state`), but judged implementing it a
  separate, independently-risky engineering effort — not a same-round
  bolt-on alongside five other requirements. Full reasoning (why it's not
  an incremental "add one symbol" API, why it's a whole-seat operation,
  why this crate's container-level verification has no cheap way to
  validate a generated keymap) is in `keymap_ascii.rs`'s module doc
  comment, specifically so a future round doesn't have to re-derive it
  from scratch. Unicode chars still hit `ascii_to_xkb`'s `_ => None`
  fallthrough and are warned-and-skipped, byte-identical to CD-0.
- **Constant-time token comparison is best-effort, not cryptographic-
  grade** — `CodriveShared::check_token` XOR-folds every byte position
  without early-returning on the first mismatch, but doesn't use SIMD or
  compiler timing barriers, and the `.get(i)` bounds check itself
  branches on length. Sized to this channel's actual threat model (a
  same-host Unix socket with filesystem-permission-gated access to the
  token file to begin with — not a network-exposed timing-attack
  surface), documented as such in the function's own doc comment rather
  than overclaiming.
- **Token file has no rotation story** — a fresh token is generated every
  process start (so a compositor restart naturally invalidates any
  previously-leaked token), but there's no in-process rotation while
  running. Not required by the task brief; noted for completeness.
  **Closed in CD-2 — see the "CD-2 socket token rotation" section below.**

## CD-1 live-bridge verification (2026-08-21, acceptance side)

First live proof that BOTH real CD-1 endpoints speak the same wire protocol:
the real gateway driver (`duduclaw-gateway/src/codrive/` — `run_script` +
`CodriveClient` + the real `ApprovalBroker`) driving THIS crate's real
compositor across a byte-verbatim TCP relay. The fake-comp integration tests
on the gateway side and this crate's own 32 tests each pin their half of the
contract; this round pins the two halves against each other. Harness:
`duduclaw-gateway/src/codrive/live_tests.rs` (permanent `#[ignore]`, module
doc = playbook).

### Topology

```
mac host                                   container (this crate's stack)
cargo test …codrive::live_tests            weston(headless) → duduclaw-comp → foot
   │  real CodriveClient                        ▲ socket: $XDG_RUNTIME_DIR/duduclaw-codrive.sock
   ▼                                            │
/tmp/cd1-live.sock ── python pump ── tcp:17777 ── socat ──┘
```

Why a bridge: Docker-for-Mac cannot share a Unix socket across the VM
boundary, and cross-building the gateway for Linux just to co-locate it with
comp is the expensive path this round didn't need. The relay copies bytes
verbatim — the protocol endpoints under test are both real; only the
transport hop is rigging. The full same-host chain (gateway + comp on the
appliance VM, MCP `codrive_run` entry, dashboard approval card as the
deciding surface) is the VM round's job, deliberately not claimed here.

### One-shot container command

Same as the CD-0 stack plus `socat` and a published port (host port 17777 —
7777 was taken on the verifying machine):

```bash
docker run -d --name cd1-live -p 127.0.0.1:17777:7777 \
  -v /Users/lizhixu/Project/DuDuClaw:/work \
  -v duduclaw-shell-cargo:/usr/local/cargo/registry \
  -v duduclaw-shell-cargo-git:/usr/local/cargo/git \
  -v duduclaw-shell-target:/target \
  -e CARGO_TARGET_DIR=/target -w /work/crates/duduclaw-comp \
  rust:bookworm bash -c '…apt-get install … socat; cargo build;
    weston --backend=headless-backend.so --socket=wayland-host … &
    WAYLAND_DISPLAY=wayland-host LIBGL_ALWAYS_SOFTWARE=1 /target/debug/duduclaw-comp &
    WAYLAND_DISPLAY=wayland-1 foot &
    exec socat TCP-LISTEN:7777,fork,reuseaddr,bind=0.0.0.0 \
      UNIX-CONNECT:$XDG_RUNTIME_DIR/duduclaw-codrive.sock'
docker cp cd1-live:/tmp/xdg-runtime/duduclaw-codrive.token /tmp/cd1-live-token
# host side: a ~20-line python pump binds /tmp/cd1-live.sock and pipes both
# directions to 127.0.0.1:17777 (see live_tests.rs module doc), then:
DUDUCLAW_CODRIVE_LIVE_SOCK=/tmp/cd1-live.sock \
DUDUCLAW_CODRIVE_LIVE_TOKEN=/tmp/cd1-live-token \
cargo test -p duduclaw-gateway --lib codrive::live_tests -- --ignored --nocapture
```

### Evidence (verified 2026-08-21 run)

- **Approve path**: driver report `final_state: "completed"`; the
  consequential Enter step carries the exact approval id the (test-side,
  real-`ApprovalBroker`) decider granted; container ground truth
  `cat /tmp/cd1-live.txt` → `cd1live` — foot's real shell executed the typed
  command only after approval.
- **Deny path**: report `final_state: "aborted_approval_denied"`, Enter step
  `outcome: "denied"` with the denied approval id; `/tmp/cd1-deny.txt` does
  NOT exist in the container; comp's audit for that session shows `text`
  applied and **zero** `key_name` events — the denied action was never
  injected, not injected-and-ignored.
- **Audit chain (comp side)**: session 1 `session_started → highlight → move
  → button×2 → text → key_name×2 → session_ended`; the ~505ms gap between
  `text` and `key_name` is the approval await, visible in the timestamps.
- **Ticker**: the temp gateway home's task store holds the full activity
  sequence (`codrive_session` start → four `codrive_step` narrations →
  `codrive_session` end) and `events.db` carries the `activity.new`
  broadcasts — the feed a dashboard/shell ticker consumes.
- **Auth, implicitly**: `session_started` only ever follows a successful
  handshake (see "CD-1 comp-side additions"), and the driver read the token
  file copied out of the container — a real end-to-end token round trip.

### Honest limitation list (this round)

- **Freeze/resume full-chain** not live-exercised across the bridge: this
  container ran without `DUDUCLAW_CODRIVE_DEBUG_STDIN`, so no mid-script
  human input could be simulated. Comp-side freeze/resume/status behavior is
  live-verified in the CD-1 comp-side round; driver-side pause/poll/re-apply
  is pinned by the fake-comp tests. The combined proof belongs to the VM
  round, where real QMP input events (the honest signal) exist.
- **Highlight visual** still pixel-unverified (no screendump here) — VM/QMP
  round, same as the CD-0 cursor precedent. The op is wire-accepted and
  audit-logged end to end.
- **MCP entry (`codrive_run` tool) and the dashboard approval card** were
  not the deciding surface here (the harness decides via the same
  `ApprovalBroker::decide` API the dashboard RPC calls). Full product-path
  decision flow is VM-round scope.

## CD-2 socket token rotation (2026-08-21)

Closes the CD-1 carry-forward item DESIGN-codrive-desktop-2026-08.md §9
flagged ("socket rotation") — the socket-auth token can now be rotated
WITHOUT restarting `duduclaw-comp`. Two independent triggers, both routed
through one function (`CodriveShared::rotate_token`, `codrive/mod.rs`):

1. An already-authenticated connection sending `{"op":"rotate_token"}`
   (`codrive/listener.rs`, alongside `status`/`resume`).
2. This process receiving `SIGHUP` — a dedicated thread turns the signal
   into the same `rotate_token` call (`block_sighup_on_current_thread` +
   `spawn_sighup_rotation_thread`, `codrive/mod.rs`).

### Design

- **Mechanism**: `auth_token` changed from a plain `Option<String>` (set
  once at process start) to `Mutex<Option<String>>`; `rotate_token`
  generates a fresh 32-byte token via the exact same
  `generate_token_bytes`/`hex_encode`/`write_token_file` path `init` used
  at startup, then swaps the mutex's value.
- **Old token invalidated immediately, existing connections unbroken**:
  this falls out of `authenticate()`'s existing structure rather than
  needing new bookkeeping — `check_token` is consulted exactly once per
  connection, at the very start (`listener.rs::authenticate`). Once past
  that gate, a connection never calls `check_token` again, so rotating the
  in-memory value only affects the NEXT connection attempt; nothing needs
  to notify or re-validate a connection that's already running (including
  the one that may have just requested the rotation itself).
- **SIGHUP via mask + `sigwait`, not a signal handler**: `rotate_token`
  does file I/O and takes a mutex — neither is async-signal-safe, so a
  real `signal()`/`sigaction()` handler was never an option. Instead:
  `block_sighup_on_current_thread` blocks SIGHUP on the main thread as the
  very first statement of `codrive::init` (before the agent seat, before
  any thread is spawned — every subsequently-spawned thread inherits the
  blocked mask via `pthread_create`'s standard inheritance rule), then
  `spawn_sighup_rotation_thread` runs a plain `loop { sigwait(...) }` that
  calls `rotate_token` as ordinary code. Getting the masking ORDER wrong
  (mask on some threads but not others) is the actual danger here — SIGHUP's
  default disposition is "terminate the process", so an unmasked thread
  receiving it instead of the dedicated `sigwait` thread would kill the
  whole compositor. This is why the live verification below sends a REAL
  signal to a REAL running process rather than trusting the reasoning alone.
- **`libc` promoted from a transitive to a direct dependency** (`Cargo.toml`)
  for `pthread_sigmask`/`sigwait`/`sigset_t` bindings — no new crate (already
  resolved via smithay's tree), no portability concern (crate is already
  Linux-only).
- **Fail-closed, matches init's existing posture**: `rotate_token` refuses
  (before touching `/dev/urandom`) if this run has no token file path at all
  (the listener was never started — CD-1's existing fail-closed disabled
  path); on a random-byte-read or file-write failure it returns `Err`
  without touching the in-memory token, so a failed rotation can never leave
  `auth_token` cleared or half-written. The SIGHUP thread is only spawned
  when masking AND the listener's own startup both succeeded — a broken
  setup gets no rotation thread instead of a thread that would just fail on
  every signal.
- **Audit**: `token_rotated` (existing event shape — `kind`/`op`/`detail`/
  `frozen`), `op` carrying the trigger (`"socket_op"` / `"sighup"`) purely
  for operator visibility.

### What changed

- **`Cargo.toml`**: added `libc = "0.2"` (see "Design" above).
- **`src/codrive/protocol.rs`**: `InjectCmd` gained a `RotateToken` variant
  (wire op `rotate_token`, no fields) + `describe()` arm.
- **`src/codrive/listener.rs`**: new `InjectCmd::RotateToken` match arm
  (control-plane, handled synchronously like `status`/`resume`, before the
  frozen/terminated gates); `validate()` updated; module doc updated; new
  integration test `rotate_token_over_socket_invalidates_old_token_without_
  dropping_the_caller` (a real `UnixListener`, three sequential connections
  — the middle two must each be FULLY closed, both the original stream and
  its `try_clone()`, before the next connects, since the listener accepts
  one connection at a time; the first draft of this test hung for exactly
  that reason and was caught before this round's container verification,
  not left as a debt).
- **`src/codrive/mod.rs`**: `CodriveShared::auth_token` is now
  `Mutex<Option<String>>` (was `Option<String>`), new `token_path:
  Option<PathBuf>` field; `check_token` adapted for the mutex; `init` now
  masks SIGHUP as its very first statement and, on the listener's success
  path, spawns the SIGHUP-rotation thread (both via the new `rotation`
  submodule below); new `for_test_with_token_path` test constructor; module
  doc updated. Grew past this project's 800-line file cap partway through
  this round (CD-1 already had it near the limit at 619 lines) — see
  `src/codrive/rotation.rs` below for the fix.
- **`src/codrive/rotation.rs`** (new file, 228 lines): everything CD-2
  actually added that isn't inline plumbing in `mod.rs`/`listener.rs` — a
  second `impl CodriveShared` block holding `rotate_token` (Rust allows an
  inherent type's methods to be split across multiple `impl` blocks in
  different files of the same crate), plus `block_sighup_on_current_thread`
  and `spawn_sighup_rotation_thread` (both `pub(super)`, called only from
  `mod.rs::init`). Its own `#[cfg(test)] mod tests` holds the three unit
  tests (`rotate_token_swaps_check_token_and_rewrites_the_file`,
  `rotate_token_two_rotations_produce_different_tokens`,
  `rotate_token_fails_closed_without_a_token_path`) — moved here, not
  duplicated, alongside the code they test. This mirrors the same
  "new focused file, not a bigger existing one" split
  `duduclaw-gateway/src/codrive/identity.rs` demonstrates for CD-2 item 2 on
  the gateway side (see that crate's own `BUILD.md`-equivalent — its
  `tests.rs`/`driver.rs` header comments — for the parallel convention
  note).

### Verification (2026-08-21, this round)

**Build/clippy/test, container-level** (same volumes/command shape as prior
rounds):

```
cargo build                                 -> Finished, zero errors
cargo clippy --all-targets -- -D warnings   -> Finished, zero warnings
cargo test                                  -> running 36 tests ... test result: ok. 36 passed; 0 failed
```

36 tests, up from CD-1's 32 (all 32 prior tests still pass unchanged; 4 new:
3 `rotate_token` unit tests in `codrive::rotation::tests` + 1 real-socket
integration test in `codrive::listener::tests`) — re-confirmed after the
file split above (moving code between modules is exactly the kind of change
that's easy to get subtly wrong via a stray visibility or import mistake, so
this was a full rebuild+clippy+test, not just a compile check).

**Live SIGHUP verification** (weston-headless → duduclaw-comp, real process,
real signal — not a unit test double): read the token file, confirmed it
authenticates; sent a REAL `kill -HUP <pid>` to the running compositor;
confirmed the process **survived** (`kill -0` still succeeds — this is the
check that actually matters, since SIGHUP's default disposition is to
terminate the process, and a masking-order bug would kill it silently);
confirmed the token file **changed**; confirmed the OLD token now gets
`auth_failed` on a fresh connection while the NEW token authenticates;
repeated a SECOND `SIGHUP` to confirm rotation is repeatable, not one-shot
(third token differs from the second, process still alive); grepped the
audit log:

```
{"kind":"token_rotated","op":"sighup", ...}
{"kind":"token_rotated","op":"sighup", ...}
```

Zero panics across the whole run (`grep -ci panic` → 0).

**Live socket-op verification** (same stack, a real Python client over the
real Unix socket): authenticated, sent `{"op":"rotate_token"}` →
`{"ok":true,"rotated":true}`, then sent `{"op":"status"}` on the SAME
connection → succeeded (proving the requesting connection survives its own
rotation request); a NEW connection presenting the pre-rotation token got
`auth_failed`; a NEW connection presenting the freshly-rotated token
authenticated. Audit: `{"kind":"token_rotated","op":"socket_op", ...}`. Zero
panics.

### Honest stub / limitation list (this round)

- **SIGHUP masking is scoped to threads spawned by this process after
  `codrive::init` runs** — correct for this binary's actual startup order
  (verified: `codrive::init` runs before `winit_backend::init_winit`, and
  nothing before `init` spawns a thread), but this is a structural
  invariant of `main.rs`'s call order, not something the type system
  enforces. A future refactor that spawns a thread before `codrive::init`
  runs would silently reopen the "SIGHUP might kill the process instead of
  rotating the token" risk — flagged in `block_sighup_on_current_thread`'s
  doc comment specifically so this is checked, not re-derived, if that
  order ever changes.
- **No rate limit on rotation** — a caller (or a script sending repeated
  `SIGHUP`s) can rotate arbitrarily often. Not a concern for THIS channel's
  threat model (same reasoning as `check_token`'s "best-effort constant-time"
  doc comment — a same-host, filesystem-permission-gated control channel,
  not a network-exposed one), but noted for completeness since nothing
  currently caps it.
- **The gateway driver (`duduclaw-gateway/src/codrive/`) has no code path
  that requests a rotation** — `CodriveCmd` (the gateway's independent,
  hand-mirrored copy of this wire protocol) was deliberately NOT extended
  with a `RotateToken` variant this round: the task brief asked for comp to
  SUPPORT rotation, not for the gateway to actively trigger it as part of
  script execution. An operator-facing "rotate now" gateway-side trigger
  (CLI command, dashboard button, or a cron-style periodic rotation) is a
  reasonable follow-up but is new scope, not part of this round's "small
  debt" brief.

## CD-2 shadow workspace verification (WP-CD2-shadow, headless output + PiP)

Implements `commercial/docs/DESIGN-codrive-desktop-2026-08.md` §3.3.4 —
"影子工作區（headless output＋PiP 旁觀）", the item both that design's own
staged plan (as CD-3) and the unified roadmap
(`commercial/docs/REPORT-duduclaw-os-status-map-2026-08-20.md` §3 milestone
10, todo item ②) call out. Scoped strictly to headless output + PiP per the
task brief's charter — freeze/handback semantics were extended just enough
to satisfy the task brief's own item 4 (see `codrive/shadow.rs`'s module
doc for the honest scope line against DESIGN §3.1 point 2's fuller
"shadow work runs unaffected by human-desktop freeze" claim, which this
round deliberately did NOT implement).

### What changed

- **`src/codrive/shadow.rs`** (new file, 396 lines): everything CD-2 shadow
  workspace actually added.
  - `create_shadow_output(&DisplayHandle) -> Output` — a second
    `smithay::output::Output` ("duduclaw-shadow-0"), registered as a real
    `wl_output` global, never bound to any real display backend.
  - `SHADOW_ORIGIN: (i32, i32) = (0, 100_000)` — the logical-space point the
    shadow output is mapped at (`Space::map_output`, called from
    `state.rs::new`). Chosen so `Space`'s own per-output geometry filtering
    (`smithay::desktop::space::space_render_elements` →
    `Space::render_elements_for_region`, confirmed by reading the vendored
    smithay 0.7.0 source before relying on it, not guessed) gives the main
    output and the shadow output structural, zero-manual-filtering
    isolation: a window mapped at `SHADOW_ORIGIN` is never a geometry match
    for the main output's own render pass, and vice versa — the same trick
    real multi-monitor desktops use for extended-desktop layouts.
  - `DuduclawComp::codrive_set_shadow(enable: bool)` — the
    `{"op":"shadow","enable":true|false}` handler (reached via
    `codrive::handle_agent_inject`'s new `Shadow` arm, `mod.rs`). Moves the
    window currently focused by the AGENT seat's keyboard to/from
    `SHADOW_ORIGIN`; idempotent re-assertion is audited as a no-op rather
    than silently ignored.
  - `DuduclawComp::codrive_handback_shadow_if_active(reason)` — shared
    handback path (task brief item 4's MVP reading: "接手＝shadow 視窗搬回
    主 output 並列印稽核事件"), called unconditionally (not nested inside a
    frozen check) from both `emergency_stop` (Super+Esc — DESIGN §6 red
    line 3, "急停一樣殺 shadow session") and `human_resume` (Super+Enter) in
    `mod.rs`.
  - `DuduclawComp::codrive_render_pip(...)` — the PiP: offscreen-renders
    the shadow output into a persistent `GlesTexture` (`Offscreen<
    GlesTexture>::create_buffer` + `Bind<GlesTexture>::bind`, both real
    smithay 0.7.0 GLES APIs checked against the vendored source, not
    assumed), then wraps it as a `TextureRenderElement<GlesTexture>`
    positioned at a fixed bottom-right corner of the main output. Full
    native-texture `src` + smaller destination `size` (240×150, same 8:5
    aspect ratio as the shadow output's 1280×800) is what makes this an
    actual downscale rather than a crop — the exact pitfall (a `size`-only
    call silently defaulting `src` to `size` itself) is documented in the
    function's own comment since it's the one place in this round's design
    research a wrong-but-compiling call would have produced a subtly wrong
    picture instead of an error.
  - Two unit tests: `PIP_SIZE`/`SHADOW_SIZE` aspect-ratio parity, and a
    guard that `SHADOW_ORIGIN`'s margin stays large enough for the
    isolation property above to hold.
- **`src/codrive/protocol.rs`**: `InjectCmd` gained a `Shadow { enable:
  bool }` variant + `describe()` arm (`"shadow"`). Unlike `Status`/
  `Resume`/`RotateToken`, this is NOT answered synchronously by the socket
  thread — it touches `self.space`, so it goes through the same
  `InjectCmd` channel and frozen/terminated gates as `move`/`button`/
  `key`/`text`/`highlight`.
- **`src/codrive/listener.rs`**: `validate()` gained a
  `Shadow { .. } => Ok(())` arm (bool payload, nothing to range-check).
- **`src/codrive/mod.rs`**: `mod shadow;` + `pub use shadow::
  {create_shadow_output, SHADOW_ORIGIN};`; `handle_agent_inject`'s match
  gained a `Shadow { enable }` arm delegating to `codrive_set_shadow`
  (falls through to the existing generic `inject_applied` audit line,
  same as `Highlight`); `emergency_stop`/`human_resume` each gained one
  call to `codrive_handback_shadow_if_active`. Grew from 723 to 761
  lines — still under this project's 800-line cap, but flagged here per
  convention (same note `rotation.rs`'s own module doc left for its own
  split) in case the next CD-2+ round needs to split further.
- **`src/state.rs`**: `DuduclawComp` gained `shadow_output: Output` and
  `codrive_shadow_active: bool` fields; `new()` creates the shadow output
  and maps it into `space` right after `codrive::init` (needs only
  `&DisplayHandle`, no real-backend dependency — unlike the main "winit"
  output, which `winit_backend::init_winit` creates later once
  `backend.window_size()` is available).
- **`src/handlers/xdg_shell.rs`**: `new_toplevel` now branches on
  `self.codrive_shadow_active` — a toplevel created while shadow mode is
  already active maps straight to `SHADOW_ORIGIN` instead of the main
  output's `(0, 0)` (covers the case where the agent opens a SECOND client
  mid-shadow-session; a window that already existed before shadow mode
  turned on is instead moved by `codrive_set_shadow` itself).
- **`src/winit_backend.rs`**: top-level `render_elements! { pub
  CodriveElement<=GlesRenderer>; Solid=SolidColorRenderElement,
  Pip=TextureRenderElement<GlesTexture>, }` — the same "compositor-internal
  render element" convention `codrive/cursor.rs`/`codrive/highlight.rs`
  established for the two cursors and the highlight box, extended with a
  real sampled texture via smithay's own `render_elements!` macro (checked
  against the vendored source's own macro-doc example for a
  concrete-renderer enum, `MyRenderElements<=GlesRenderer>`, not
  guessed). `init_winit` gained `pip_texture: Option<GlesTexture>` and
  `pip_damage_tracker: OutputDamageTracker` locals (same capture shape as
  the pre-existing `output`/`damage_tracker` locals); the `WinitEvent::
  Redraw` arm now builds `Vec<CodriveElement>` instead of `Vec<
  SolidColorRenderElement>`, calls `backend.renderer()` to do the offscreen
  PiP render BEFORE `backend.bind()`'s own (separately-borrowed) renderer
  access, and pushes the resulting `CodriveElement::Pip` into the same
  custom-elements slice `render_output` already consumed for the two
  cursors and the highlight box.

### Wire protocol addition

```
{"op":"shadow","enable":true}   -> {"ok":true,"frozen":false}   (window(s) moved to the shadow output)
{"op":"shadow","enable":false}  -> {"ok":true,"frozen":false}   (window(s) moved back to the main output)
```

Same auth/frozen/terminated gating as every other seat-touching op (`move`/
`button`/`key`/`text`/`highlight`) — NOT special-cased like `status`/
`resume`/`rotate_token`.

### Verification (2026-08-21, this round)

**Build/clippy/test, container-level** (same volumes/command shape as prior
CD-0/CD-1/CD-2 rounds):

```
cargo build                                 -> Finished, zero errors (first try — every smithay 0.7.0
                                                API used here was checked against the vendored
                                                registry source before writing the call, not guessed)
cargo clippy --all-targets -- -D warnings   -> Finished, zero warnings
cargo test                                  -> running 38 tests ... test result: ok. 38 passed; 0 failed
```

38 tests, up from CD-2 token-rotation's 36 (all 36 prior tests still pass
byte-for-byte unchanged — confirms non-shadow paths are untouched; 2 new:
`codrive::shadow::tests::shadow_origin_and_size_share_an_aspect_ratio_with_pip_size`
+ `codrive::shadow::tests::shadow_origin_is_far_from_any_realistic_main_output_rect`).

**Live functional verification** (weston-headless → duduclaw-comp → foot,
same three-layer stack as every prior round, driven via a real authenticated
socket client — no `DEBUG_STDIN` needed for the socket-op half of this
test):

1. **Baseline control**: `move`+`button`(press/release, focuses `foot`)+
   `text` synthesizes `echo pre-shadow-ok987 > /tmp/cd2-pre.txt\n` — real
   shell executes it (`cat /tmp/cd2-pre.txt` → `pre-shadow-ok987`), proving
   the ordinary main-output path is unaffected before any `shadow` op.
2. **`{"op":"shadow","enable":true}`** while `foot` already holds agent
   keyboard focus — audit trail: `shadow_window_moved(to_shadow)` →
   `shadow_enabled` → `inject_applied(op:shadow)`, in order.
3. **The SAME window, now at `SHADOW_ORIGIN`+local offset (100, 100100)**,
   is driven again with `move`/`button`/`text` — real shell executes
   `echo shadow-active-ok654 > /tmp/cd2-shadow.txt` (`cat` confirms) —
   proving the window is fully interactive after relocation, not just
   moved-and-inert.
4. **Isolation, a second dedicated run** (`cd2_shadow_isolation.py`): after
   enabling shadow, a click at the OLD main-output coordinate
   `(50, 50)` where `foot` used to sit hits nothing —
   `handle_agent_inject`'s click-to-focus `else` branch explicitly clears
   agent keyboard focus (`set_focus(None)`) when nothing is under the
   pointer — and a `text` op sent right after produces **no file at all**
   (`/tmp/cd2-mainclick-nowhere.txt` does not exist), while an immediately
   following click+text at the shadow-region coordinates DOES write
   `/tmp/cd2-shadowclick-still-works.txt` — a positive control ruling out
   "comp just broke" as the explanation for the main-output click producing
   nothing. This is the concrete evidence for "在主畫面上不可見、不搶焦點"
   from the task brief, at the audit/file-side-effect layer this
   container's headless environment can actually produce (no screendump
   available here — see "Honest stub" below).
5. **Handback via Super+Enter** (`DUDUCLAW_CODRIVE_DEBUG_STDIN=1`,
   `simulate_super_enter`): re-enabling shadow then simulating Super+Enter
   produces `shadow_window_moved(to_main x1)` → `shadow_disabled(detail:
   "handback (human_super_enter) — 1 window(s) moved to the main output")`
   — matches the task brief's MVP handback rule exactly, and fires even
   though the seat was never actually frozen in this run (confirms the
   handback call is NOT nested inside `human_resume`'s `if was_frozen`
   branch, as designed).
6. **Handback via Super+Esc emergency stop**: re-enabling shadow again then
   simulating Super+Esc produces, in order: `emergency_stop(detail:
   "debug_stdin_simulated_super_esc")` → `shadow_window_moved(to_main x1)`
   → `shadow_disabled(detail: "handback (debug_stdin_simulated_super_esc) —
   1 window(s) moved to the main output")` — confirms DESIGN §6 red line
   3's "急停鍵永遠有效" extends to tearing down an active shadow session,
   per the task brief's own explicit example for this item.
7. **PiP render path executed for real, every frame, with zero failures**:
   across both live-run sessions above (several seconds of continuous,
   unthrottled redraw with shadow mode active — see BUILD.md's earlier
   "Honest stub" notes on this crate's tight redraw loop), `grep -c panic
   duduclaw-comp*.log` → 0, and none of `codrive_render_pip`'s three
   fail-open warning strings ("failed to allocate the shadow-workspace PiP
   texture" / "failed to bind…" / "failed to render the shadow output…")
   appear anywhere in either log — meaning `Offscreen<GlesTexture>::
   create_buffer`, `Bind<GlesTexture>::bind`, and `render_output` into that
   bound texture all succeeded on real (`llvmpipe`) software GLES, every
   single redraw, for the whole duration shadow mode was active in both
   runs — not just "the code compiles," an actual repeated live exercise of
   the GL offscreen-render code path this round added.

### Honest stub / limitation list (this round)

- **PiP pixel content is not visually verified** — this headless container
  has no screendump/framebuffer capture (same limitation category as CD-0's
  cursor-distinctness check and CD-1's highlight-box check, both of which
  needed the VM/QMP round's `screendump` to close visually). This round's
  evidence is one layer down from pixels: the render path runs successfully
  every frame with no fail-open warnings (item 7 above), and the underlying
  shadow-output content is independently proven correct via file
  side-effects (items 3–4) — but whether the PiP texture's pixels actually
  land in the right on-screen corner, at the right size, showing the right
  content, right-side-up, is VM/QMP acceptance-side work, same as the prior
  two visual checks.
- **`Fourcc::Abgr8888` channel order is unverified** — chosen as a common,
  GLES-supported RGBA format for the offscreen texture (confirmed to exist
  in the vendored `drm-fourcc` crate before using it), but whether the
  resulting picture's color channels are exactly right (vs., say,
  channel-swapped) is a pixel-level question this round's evidence can't
  answer — same VM/QMP dependency as the point above.
- **Freeze scope was deliberately unchanged by this round — closed by
  WP-CD2-freeze-scope, see the section below.** DESIGN §3.1 point 2/3:
  "並行零干擾" with the human's real desktop. This round's
  `handle_agent_inject` applied its frozen gate uniformly to every op
  including `Shadow`/subsequent shadow-window commands; a later round
  scoped the gate so shadow-confined commands bypass a freeze while every
  other op (and the `Shadow` toggle itself) still doesn't.
- **No multi-window tiling** — every window moved into (or out of) shadow
  lands at the exact same point (`SHADOW_ORIGIN` / `(0, 0)`) — matches this
  crate's pre-existing single-window-at-a-time assumption (every brand-new
  toplevel already maps to a fixed `(0, 0)` on the main output too), not a
  new limitation introduced by this round.
- **Per-window off-screening was never attempted** — DESIGN §7 R-C2
  already ruled this out ("無先例") in favor of session-level headless
  output, which is what this round implements; restated here only so a
  BUILD.md reader doesn't wonder why every shadow window shares one region.
- **Real hardware / VM round not run this session** — every item above
  that says "VM/QMP" is carried forward exactly as CD-0/CD-1's own
  honest-stub lists already did for their respective visual/hardware
  checks; this round did not attempt a VM pass (task brief scoped
  verification to "container 內... nested weston 模式", with real-hardware
  work explicitly left to the acceptance side, matching the CD-0/CD-1
  precedent this file already established).

## WP-CD2-freeze-scope: freeze scope segmentation (shadow work doesn't get frozen)

Implements `commercial/docs/DESIGN-codrive-desktop-2026-08.md` §3.1 point 3
(the 2026-08-20 "凍結作用域" clarification): the human-input freeze gate
protects the SHARED main desktop the instant a human touches it — it was
never meant to also pause an agent's shadow session running in parallel on
a headless output the human can't even see ("並行零干擾"). This closes the
gap the CD-2 shadow-workspace round left open by design (see its own
section above, and `codrive/shadow.rs`'s pre-existing module doc, which
flagged it explicitly rather than glossing over it).

### What changed

- **`src/codrive/shadow.rs`** (396 → 629 lines): the actual policy.
  - `point_in_shadow_bounds(x, y)` / `rect_in_shadow_bounds(x, y, w, h)` —
    pure geometry against [`SHADOW_ORIGIN`]/`SHADOW_SIZE`.
  - `freeze_bypass_decision(shadow_active, cmd, agent_pointer_pos,
    agent_keyboard_focus_in_shadow) -> bool` — the actual policy, kept
    **pure** (no `&DuduclawComp`) specifically so it's unit-testable
    without constructing a full compositor state (`EventLoop`+`Display`+
    `DuduclawComp::new`) — this crate has never done that in a unit test;
    see BUILD.md's many "Honest stub" notes on why live/container
    verification, not unit tests, is this crate's usual tool for anything
    touching real seat/space state. Fail-closed on every axis: `Shadow`
    (both `enable:true` and `enable:false`) never bypasses; `Move`/
    `Highlight` bypass only if their own coordinates are confirmed inside
    the shadow output; `Button` bypasses only if the agent pointer's
    CURRENT live position is inside it; `Key`/`KeyName`/`Text` bypass only
    if the agent keyboard's CURRENT focus is a shadow-region window;
    `Resume`/`Status`/`RotateToken` never bypass (they never reach this
    path in practice — listed explicitly, not via `_`, so a future new
    `InjectCmd` variant fails the match at compile time instead of
    silently inheriting a bypass).
  - `agent_keyboard_focus_is_shadowed(comp)` / `is_freeze_bypass_eligible
    (comp, cmd)` — the thin, untested wrapper that extracts live
    agent-seat facts (pointer position, keyboard-focus-window location)
    from a real `&DuduclawComp` and defers to `freeze_bypass_decision`.
  - `codrive_set_shadow`/`codrive_handback_shadow_if_active` each gained
    one line mirroring `codrive_shadow_active` into the new
    `CodriveShared::shadow_active` atomic (below) — every write to one
    goes through these two functions, never directly.
  - 10 new unit tests (bounds edge cases + every `freeze_bypass_decision`
    branch).
- **`src/codrive/mod.rs`** (761 → 793 lines, still under the 800 cap):
  - `CodriveShared` gained `shadow_active: AtomicBool` — a mirror of
    `DuduclawComp::codrive_shadow_active` kept ONLY for `listener.rs`'s
    socket-thread optimistic pre-check (no `self.space`/seat access
    there); never itself the authoritative bypass decision. All five
    `CodriveShared` constructors updated.
  - `handle_agent_inject`'s frozen gate: was an unconditional "frozen ⇒
    drop", now `let shadow_bypass = frozen &&
    shadow::is_freeze_bypass_eligible(self, &cmd);` gates the drop instead.
    The drop path's audit `detail` now distinguishes a plain queued-then-
    frozen race from a failed shadow-scope check. The `inject_applied`
    audit line at the bottom now tags `detail:"scope:shadow"` when the op
    was a bypass — `None` (byte-identical) for every non-bypass apply.
- **`src/codrive/listener.rs`** (623 → 759 lines): the socket thread's
  frozen pre-check was an unconditional deny; it now only denies outright
  when `!(shared.shadow_active && cmd is not Shadow)` — otherwise it
  forwards to the main thread's authoritative check (this thread has no
  way to confirm a specific op's target itself). The success ack's
  `"frozen"` field is no longer hardcoded `false` — it now reflects
  `shared.frozen`'s real value (`true` for a forwarded bypass candidate),
  matching the gateway client's own already-documented wire contract
  (`{"ok":true,"frozen":bool}`, `duduclaw-gateway/src/codrive/client.rs` —
  its `CodriveAck.frozen` field was already `Option<bool>`, permissive of
  either value; no gateway-side change needed). 3 new tests.
- No wire protocol addition — this round only changes WHEN existing ops
  are allowed through, not the JSON shapes themselves.

### Verification (2026-08-21, this round)

**Build/clippy/test, container-level** (same volumes/command shape as
every prior round):

```
cargo build                                 -> Finished, zero errors
cargo clippy --all-targets -- -D warnings   -> Finished, zero warnings
cargo test                                  -> running 51 tests ... test result: ok. 51 passed; 0 failed
```

51 tests, up from CD-2 shadow's 38 (all 38 prior tests still pass
byte-for-byte unchanged — confirms invariant (d), the non-shadow path is
untouched; 13 new: 10 in `codrive::shadow::tests`, 3 in
`codrive::listener::tests`).

**The four invariants, each with its own test(s):**

| # | Invariant | Test(s) | Layer |
|---|---|---|---|
| (a) | 凍結中不可進入 shadow（雙向） | `shadow::tests::freeze_bypass_decision_shadow_toggle_never_bypasses_either_direction`, `listener::tests::frozen_shadow_toggle_always_denied_even_when_shadow_already_active` | unit + real-socket |
| (b) | Super+Esc 全域急停不變（含 shadow） | live run step 8 below (this crate has no precedent for unit-testing `emergency_stop`/`human_resume` — both need a full `DuduclawComp`; CD-2's own verification used the same live-only approach) | live/container |
| (c) | shadow 注入不可觸主桌面 | `shadow::tests::freeze_bypass_decision_move_follows_target_coordinate`, `_button_follows_live_pointer_position`, `_key_and_text_follow_keyboard_focus_flag`, `_highlight_follows_whole_rect`, `point_in_shadow_bounds_*`, `rect_in_shadow_bounds_*`; live run steps 4/5 | unit + real-socket + live/container |
| (d) | 非 shadow 路徑逐位不變 | `shadow::tests::freeze_bypass_decision_shadow_inactive_never_bypasses`; the 38 pre-existing tests all still pass; live run step 1 (baseline) | unit + live/container |

**Live functional verification** (weston-headless → duduclaw-comp → foot,
same three-layer stack as every prior round, driven via a real
authenticated socket client + `DUDUCLAW_CODRIVE_DEBUG_STDIN=1` for
`simulate_human`/`simulate_super_esc`):

1. **Baseline** (no shadow, no freeze): `move`+`button`+`text` writes
   `/tmp/fz-baseline.txt` = `baseline-ok111` via real shell execution —
   confirms the ordinary path is untouched before this WP's logic is
   exercised at all.
2. **`{"op":"shadow","enable":true}`**, then a shadow-relocated `move`+
   `button`+`text` at `(100, 100100)` (`SHADOW_ORIGIN`-local) writes
   `/tmp/fz-preshadow.txt` = `pre-freeze-shadow-ok222` — shadow session
   interactive before any freeze.
3. **`simulate_human`** — audit: `freeze(op:debug_stdin_simulated)`.
4. **Frozen + shadow-targeted commands still execute for real**: a NEW
   authenticated connection (proving reconnect-during-freeze doesn't
   clear `frozen`, per CD-0/CD-1 precedent) sends `move`+`button`+`text`
   at `(120, 100120)` — writes `/tmp/fz-frozen-shadow.txt` =
   `shadow-bypasses-freeze-ok333`. Audit: all four `inject_applied` lines
   carry `"detail":"scope:shadow"`. **This is the load-bearing proof of
   invariant (1)/(c)'s positive half — real shell execution, not just an
   `"ok":true` ack.**
5. **Frozen + a main-output-targeted `move` is dropped**: `{"op":"move",
   "x":50.0,"y":50.0}` (nowhere near `SHADOW_ORIGIN`) gets an optimistic
   `"ok":true,"frozen":true"` ack from the socket thread (it can't know
   the target itself), but the audit trail shows what the main thread
   actually decided: `"kind":"inject_dropped","op":"move","x":50.0,
   "y":50.0,"detail":"frozen at execution time — shadow active but this
   op's target is not confirmed inside the shadow output (fail-closed)…"`
   — the real `is_freeze_bypass_eligible`, fed real `SHADOW_ORIGIN`/
   `SHADOW_SIZE` geometry, correctly rejected it. (A file-side-effect
   proof for this specific negative would be ambiguous here: since the
   agent's only window had already relocated to the shadow output before
   the freeze, there was nothing left on the main output for a stray
   click to hit either way — the audit trail is the precise, unambiguous
   evidence that IT WAS THE FREEZE GATE that rejected the command, not
   "there was nothing there.")
6. **Frozen + `shadow` toggle denied both directions**: `{"op":"shadow",
   "enable":true}` → `{"ok":false,"frozen":true,"reason":
   "agent_seat_frozen"}`; `{"op":"shadow","enable":false}` → the same.
   Audit: two `inject_dropped` lines, `op:"shadow"`, same denial detail
   as an ordinary frozen non-shadow op — proving `Shadow` stays behind
   the PLAIN gate, never even reaching the bypass-eligibility check.
7. **Shadow still works after the denied attempts**: `move`+`button`+
   `text` at `(140, 100140)` writes `/tmp/fz-frozen-shadow-2.txt` =
   `shadow-still-alive-ok444` — rejecting the main-output escape attempt
   and the toggle-denial attempts didn't collaterally wedge the
   legitimate parallel shadow session.
8. **`simulate_super_esc`**: audit, in order —
   `emergency_stop(detail:"debug_stdin_simulated_super_esc")` →
   `shadow_window_moved(to_main x1)` → `shadow_disabled(detail:"handback
   (debug_stdin_simulated_super_esc) — 1 window(s) moved to the main
   output")` — confirms invariant (b): Super+Esc still tears down the
   shadow session exactly as CD-2's own round proved, unaffected by this
   round's gate changes.
9. **Post-ESC lockdown, from a brand-new connection**: `frozen` stays
   `true` (only human-side `Super+Enter` clears it — unchanged CD-1
   invariant) and `shadow_active` is now `false` (handed back in step 8),
   so a fresh connection's shadow-targeted `move`/`button`/`text` (the
   SAME coordinates that worked in step 4) are ALL denied —
   `/tmp/fz-frozen-shadow.txt` is confirmed unchanged (still
   `shadow-bypasses-freeze-ok333`, no new write) — proving Super+Esc's
   lockdown is total, not just "shadow session torn down but somehow
   still reachable."
10. **Zero panics**: `grep -ci panic /tmp/duduclaw-comp.log` → `0` across
    the whole run.

The full audit trail from this run (abbreviated `ts_ms` for readability)
is coherent end-to-end with no gaps and no out-of-order transitions —
`session_started`/`session_ended` bracket each of the 8 reconnects
cleanly, and every `inject_applied`/`inject_dropped` line's `frozen`
column matches the freeze timeline exactly.

### Honest stub / limitation list (this round)

- **Invariant (b) has no unit test** — this crate has never unit-tested
  `emergency_stop`/`human_resume` (both need a full `DuduclawComp`, which
  needs a real `EventLoop`+`Display`); CD-2's own shadow-workspace round
  hit the identical limitation and used the same live-only verification.
  Not a regression introduced by this round — restated here so a reader
  doesn't wonder why the invariant table above has no unit-test entry for
  it.
- **`is_freeze_bypass_eligible` (the `&DuduclawComp` wrapper) has no unit
  test of its own** — only the pure `freeze_bypass_decision` it defers to
  does. The wrapper's own correctness (does it extract the RIGHT live
  facts from a real seat/space) is exactly what live run steps 4/5
  exercise instead.
- **No new coordinate-space concept was introduced** — "inside the shadow
  output" is exactly `SHADOW_ORIGIN..SHADOW_ORIGIN+SHADOW_SIZE`, the same
  fixed region CD-2 already established; this round adds no per-window or
  dynamic geometry tracking (matches CD-2's own "no multi-window tiling"
  scope limit, restated here since freeze-bypass eligibility depends on
  that same fixed region holding).
- **Real hardware / VM round not run this session** — same category as
  every prior round's own list; this round's task brief scoped
  verification to the container/nested-weston level, with real Super+Esc/
  human-input-triggered-freeze-while-shadow-active on real hardware left
  to acceptance-side VM/QMP work (the `simulate_human`/`simulate_super_esc`
  debug stdin path verifies everything downstream of hardware detection,
  same split as CD-0/CD-1's own honest-stub notes).

## WP-CD2-vmround: CD-2 收官 VM/QMP 真輸入輪 (verified 2026-08-21)

Closes the "real hardware / VM round" gap the freeze-scope section above
(and the shadow-workspace section before it) left explicitly open. Same
appliance QEMU VM (arm64, `qemu-system-aarch64 -accel hvf`) and injection
recipe as the CD-0 VM/QMP round; comp rebuilt fresh from this round's
working tree (CD-2 rotation + shadow + freeze-scope, 54 container tests
green) and re-injected before driving anything.

**Real bug found and fixed**: a genuine physical Super+Enter chord (Logo
down → Return down → Return up → Logo up — the way real hardware reports a
held-modifier chord, not a synthetic single event) left the agent seat
**frozen again immediately after `human_resume()` un-froze it**, because
`input.rs`'s keyboard arm called `on_human_input` unconditionally for
every keyboard event including the chord's own trailing key-release
events — releasing Return (still `frozen:false → true`) or Logo re-armed
the freeze gate with no counteracting resume, since the resume-detecting
closure only matches on `KeyState::Pressed`. On real hardware this made
Super+Enter **structurally unable to durably hand control back** — every
real "交還" attempt self-defeated a few hundred ms after the human
released the keys. Neither the CD-0/CD-1 container debug-stdin rounds nor
this round's own first QMP attempt (a single held/synthetic key event)
could have caught this — it only shows up with real down/down/up/up
timing. Fixed in `src/input.rs` (`is_system_gesture_tail`, a pure/
unit-tested exemption: any keyboard event where Logo is currently held OR
was held on the immediately-preceding event is chord activity, not
ordinary desktop touch) + one new `DuduclawComp` field (`src/state.rs`,
`codrive_logo_held_prev`). 3 new unit tests (54 total, up from 51);
container `cargo build`/`clippy -D warnings`/`test` all clean. Regression
evidence: three independent real QMP Super+Enter chords across this
round (initial repro, post-fix confirmation, post-item-3 handback) all
left `frozen:false` durably — audit `resume(op:human_super_enter)` with
no trailing re-freeze line, vs. the pre-fix run's `resume` immediately
followed by `freeze(op:keyboard)`.

**Four-item verification, all PASS**:
1. **Freeze/handback full chain, real driver**: `duduclaw-gateway`'s real
   `codrive::driver::run_script` (new permanent `#[ignore]` test
   `live_bridge_real_human_freeze_and_resume` in `duduclaw-gateway/src/
   codrive/live_tests.rs`, same TCP-bridge pattern as the CD-1 live-bridge
   test — here bridging to the VM's `tcp_unix_bridge.py` instead of a
   Docker container) drove a real script against real comp; a real QMP
   keyboard event fired mid-script froze the seat (audit `op:"keyboard"`,
   not `debug_stdin_simulated`), the driver's `wait_for_resume` correctly
   observed it via `status` polling, a real QMP Super+Enter chord resumed
   it, and the driver reapplied the dropped step — `final_state:
   "completed"`, step outcome `dropped_frozen_reapplied`. Guest file
   `/tmp/cd2-freeze-proof.txt` = `cd2vmfreeze123`.
2. **Highlight visual**: QMP `screendump` while a `{"op":"highlight",...}`
   box was live confirms a hollow amber border at the requested rect,
   visually distinct from both cursors.
3. **Shadow + PiP visual + isolation**: screendump after `{"op":"shadow",
   "enable":true}` shows the main output blank (agent's window relocated)
   with a real PiP thumbnail (foot's terminal content, downscaled) in the
   bottom-right corner. Isolation confirmed at the strongest evidence
   layer (real shell execution, not just acks): during a real-hardware
   freeze, shadow-targeted move/button/text still executed for real
   (`/tmp/cd2-frozen-shadow.txt` written, audit `detail:"scope:shadow"`)
   while a main-output-targeted `move` was `inject_dropped` (fail-closed,
   "not confirmed inside the shadow output"). Handback via Super+Enter
   screendumped back to the plain foot window.
4. **MCP `codrive_run` + dashboard approval, full product path**: a real
   `duduclaw run` gateway (test `DUDUCLAW_HOME`, `DUDUCLAW_PORT=18799`)
   plus a real `duduclaw mcp-server` stdio JSON-RPC client (NOT a direct
   Rust call) issued `codrive_run`; the resulting `codrive_action`
   approval appeared via the same `approvals.list` dashboard WebSocket RPC
   the web UI uses (`simulation` field populated), authenticated with a
   real admin JWT obtained via the passwordless `/api/session/local`
   local-auto-login flow. Approve path: `approvals.decide` → comp executed
   the consequential `key_name:enter` for real (`/tmp/cd2-mcp-approve.txt`
   = `cd2mcpapprove456`), driver report `final_state: "completed"` with
   the step's `approval_id` matching the decided approval. Deny path: comp
   audit shows the typed `text` applied but **zero** `key_name` events —
   `/tmp/cd2-mcp-deny.txt` never created, driver report `final_state:
   "aborted_approval_denied"`. Web UI visual approval card itself was not
   opened this round (RPC-level product path only, per the task brief's
   own fallback) — left for a human to eyeball.

**Environment notes for whoever picks this VM up next**: the appliance
disk (`appliance/.vm/duduclaw-os-vm.raw`) now carries this round's rebuilt
comp binary (includes the Super+Enter fix); root password and serial
getty are unchanged durable state from the CD-0 round. The guest's
`nftables` `inet filter input` chain default-denies new inbound ports —
this round added a `tcp dport 7778 accept` rule (for a guest-local
`tcp_unix_bridge.py` Unix↔TCP relay, QEMU `hostfwd`'d to the host) that is
**not persisted** (VM was stopped via QMP `quit`, not a graceful `nft
save`), so a future round needing host→guest TCP again must re-add it.

## CD-3: take_over / watch mode (2026-08-22)

Implements DESIGN-codrive-desktop-2026-08.md §5's "接手/交還＋watch mode"
row (recorded as CD-2 in that table; the numeral CD-3 is used throughout
this section and in the gateway crate per the task brief's own numbering
note — the statutory CD-2 slot was already consumed by the shadow-workspace
round). Two new comp-side ops, both fully documented in their own module
docs: `{"op":"take_over","reason":"…"}` (`src/codrive/takeover.rs`) and
`{"op":"watch","enable":true|false}` (`src/codrive/watch.rs`). New files
only — `mod.rs`/`listener.rs`/`state.rs`/`input.rs`/`winit_backend.rs`/
`shadow.rs` each got the minimum wiring needed (new `InjectCmd` match arms,
new `CodriveShared`/`DuduclawComp` fields, an exhaustiveness arm in
`shadow::freeze_bypass_decision`, one call into `codrive_check_watch_idle`
from the redraw loop). `mod.rs` sits at exactly 800 lines and `listener.rs`
at 796 after this round — both at this project's hard per-file cap; a
follow-up round should consider splitting `CodriveShared`'s struct
definition + its five constructors out of `mod.rs` into its own file (the
same "new logic → new file" move this round already applied to everything
past the struct itself) if a future WP needs to add another field there.

### State-machine relationship (takeover / frozen / watch-paused / shadow)

Four independent-but-interacting pieces of state, all on `DuduclawComp`/
`CodriveShared`:

| State flag | Set by | Cleared by | Effect |
|---|---|---|---|
| `frozen` (existing, CD-0) | any human input, `take_over`, watch-idle timeout | `human_resume` (Super+Enter), watch-idle auto-resume | Denies every injection/query op except `status`, UNLESS shadow-bypass-eligible |
| `codrive_takeover_active` (CD-3) | `take_over` op | `human_resume`, `emergency_stop` | On top of `frozen`: disables the shadow-bypass exception entirely (§3.4 "零例外") |
| `codrive_watch_paused` (CD-3) | idle timeout while `codrive_watch_active` | the VERY NEXT human input event (no explicit resume needed), or `human_resume`/`emergency_stop`/`watch:false` | Sets `frozen` like any other cause; its own clearing path is the ONLY one that doesn't require Super+Enter |
| `codrive_shadow_active` (CD-2) | `{"op":"shadow","enable":true}` | `{"op":"shadow","enable":false}`, `human_resume`, `emergency_stop` | Makes shadow-confined ops bypass `frozen` — UNLESS `codrive_takeover_active` is also true |

Worked-out interactions:
- **Plain human touch, no takeover, shadow active**: `frozen=true`,
  `codrive_takeover_active=false` → shadow-confined ops still apply
  (pre-existing WP-CD2-freeze-scope behavior, byte-identical).
- **`take_over`, shadow active**: `frozen=true`,
  `codrive_takeover_active=true` → shadow-confined ops ALSO denied now —
  the one behavior CD-3 makes strictly stronger than an ordinary freeze,
  because a credential window is a total sensory blackout regardless of
  what else is running.
- **Watch-idle pause, shadow active**: `frozen=true` from a TIMER, not a
  takeover (`codrive_takeover_active` stays `false`) → shadow-confined ops
  still apply, same as plain human touch — "shadow session 不受 watch 暫停"
  falls out of the SAME mechanism, not a special case.
- **`take_over` while already watch-paused (or vice versa)**: both flags
  can be true simultaneously (independent bookkeeping); `human_resume`
  clears both unconditionally, regardless of which one (or both) applied.

### Container verification (this round)

`cargo build` / `cargo clippy --all-targets -- -D warnings` / `cargo test`
inside the reproducible Docker command this file's earlier sections
already document (same `rust:bookworm` image + cached cargo/target
volumes): all three green, **70 tests passing** (up from CD-2's 54 — 16
new: 2 in `takeover.rs`, 6 in `watch.rs`, 2 new arms in `shadow.rs`'s
`freeze_bypass_decision` tests, 8 in the new `tests_takeover.rs` socket-
level integration file), zero clippy warnings.

### Live nested-headless verification (this round, `DUDUCLAW_CODRIVE_DEBUG_STDIN=1`)

Both new features are driven end-to-end against a REAL running
`duduclaw-comp` process (weston headless → comp → foot, same three-layer
stack as every prior round) — `take_over` is agent-initiated (a plain
socket op, no real input device needed at all, so this round's evidence for
it is NOT a container-limitation stand-in the way Super+Esc/Super+Enter
detection itself is), and watch-mode's resume path only needs "any human
input event", which `simulate_human`/`simulate_super_enter` legitimately
proxy for (same "drives the state machine, not the hardware event
delivery" category `debug_sim.rs`'s module doc already establishes).

**take_over round-trip** (`DUDUCLAW_CODRIVE_WATCH_IDLE_SECS=5` set for the
same run):
```
STATUS_BEFORE:  {"ok":true,"frozen":false,"terminated":false,"takeover":false}
TAKE_OVER ->    (accepted; comp pushes {"event":"takeover_started"})
STATUS_DURING:  {"ok":true,"frozen":true,"terminated":false,"takeover":true}
MOVE_WHILE_TAKEOVER -> denied (audit: inject_dropped op:move, frozen:true)
[echo simulate_super_enter >&9]
STATUS_AFTER_RESUME: frozen:false
MOVE_AFTER_RESUME:   {"ok":true,"frozen":false} (applied — audit inject_applied op:move x:11.0 y:11.0)
```
Audit trail excerpt (verbatim):
```
{"kind":"takeover_started","op":"take_over","detail":"reason=login page needs a human; frame_feed:none (no screencopy/screenshot mechanism implemented at this stage — see DESIGN §3.4, reserved for CD-4)","frozen":true}
{"kind":"inject_applied","op":"take_over","frozen":true}
{"kind":"inject_dropped","op":"move","x":10.0,"y":10.0,"detail":"agent seat frozen (human input active) — dropped, not buffered","frozen":true}
{"kind":"resume","op":"human_super_enter","frozen":false}
{"kind":"takeover_ended","detail":"human_super_enter","frozen":false}
```
Confirms, with real audit evidence: takeover freezes on arrival (`status`
flips `takeover:true` alongside `frozen:true`), every injection op is
denied while active (`status` itself still answers per task brief item 2),
the ending is the ordinary Super+Enter path plus a dedicated
`takeover_ended` line, and the honest `frame_feed:none` claim (task brief
item 2 — this stage genuinely has no screen-copy mechanism, see
`takeover.rs`'s module doc) is present verbatim in the audit trail.

**Watch-mode idle round-trip** (threshold set to 5s via the env var):
```
WATCH_ON:              {"ok":true,"frozen":false}
STATUS_JUST_ENABLED:   frozen:false  (enabling reset the idle clock — no false trigger)
[sleep 7s, past the 5s threshold, no human input]
STATUS_AFTER_IDLE:     {"ok":true,"frozen":true,"terminated":false,"takeover":false}
[echo simulate_human >&9]
STATUS_AFTER_PRESENCE: {"ok":true,"frozen":false,"terminated":false,"takeover":false}
```
Audit trail excerpt:
```
{"kind":"watch_enabled","op":"watch","frozen":false}
{"kind":"watch_paused","detail":"idle_for=5s (threshold=5s)","frozen":true}
{"kind":"watch_resumed","detail":"human_input","frozen":false}
```
Confirms: idle timeout freezes the seat WITHOUT setting `takeover:true`
(independent freeze cause, as designed), the pause lifts on the very next
human input with NO explicit Super+Enter (the `watch_resumed` line, not a
`resume` line — a real behavioral difference from every other frozen state
this crate has), and the 5-second real-clock measurement matches the
configured threshold (`idle_for=5s` against a 7s sleep, i.e. it fired at
the threshold, not late).

### Honest stub / limitation list (this round)

- **Real-hardware QMP round not run this session** — everything above is
  container-level (nested headless weston + debug-stdin simulator). Given
  `take_over` reaches the seat via the socket (no real input device
  involved at all) and watch-mode's resume path only needs "a human input
  event happened" (which the debug-stdin simulator legitimately stands in
  for, same category as CD-0/CD-1's own container rounds), this is
  reasonably strong evidence for the state-machine logic itself — but the
  VM/QMP round's own unique value (real Super+Enter chord timing quirks,
  visual confirmation) was NOT re-run for CD-3 specifically. Left for
  acceptance side, same pattern every prior CD round has used.
- **Takeover-blocks-shadow-bypass interaction is unit-tested, not container-
  live-verified** — `shadow.rs`'s `freeze_bypass_decision_take_over_and_
  watch_never_bypass` test and the two new `tests_takeover.rs` socket-level
  tests (`frozen_with_takeover_active_denies_even_with_shadow_active`,
  `frozen_with_shadow_active_and_no_takeover_still_forwards`) pin the exact
  contract, but this round's live run did not additionally spin up a real
  shadow session + take_over simultaneously against a real second window.
- **`DUDUCLAW_CODRIVE_WATCH_IDLE_SECS` is an env var, checked once at
  process start** (`watch.rs` module doc explains the "env var, not CLI
  flag or config file" choice) — an operator changing it mid-run has no
  effect until restart, same limitation `DUDUCLAW_CODRIVE_DEBUG_STDIN`
  already has.
- **No frame-supply mechanism exists at this stage at all** (confirmed via
  `Cargo.toml`'s enabled smithay features + a repo-wide grep before writing
  `takeover.rs`'s module doc) — so "感知/事件流停止" for `take_over` is
  currently satisfied entirely by the existing `frozen` gate; there is
  nothing separate to additionally cut off. The `frame_feed:none` audit
  note reserves the vocabulary for CD-4's perception upgrade (C-L2/C-L3)
  to fill in honestly once a real mechanism exists — this is not deferred
  work silently dropped, it's a documented non-applicability at this stage.

## A1 multi-window round (2026-08-22)

WP-A1: the "duduclaw-comp multi-window formal pass" this crate needed
before an S5 Flatpak host could sit on top of it — labwc was judged dead
for that role, everything builds on this self-built smithay compositor
instead. Four deliverables, all container-verified against **real**
clients, not synthetic test harnesses: popup grabs, ≥3 concurrent real
clients, focus/Z-order correctness (including a real bug fix), and this
file's own "Still unverified" debt.

### 1. Popup grab implementation

`XdgShellHandler::grab()` (`src/handlers/xdg_shell.rs`) was a documented
no-op inherited from smallvil (smallvil never implements it either — see
this file's earlier "Honest stub" list). It now:

- Resolves the popup's root surface via the same `find_popup_root_surface`
  lookup `unconstrain_popup` (just below it in the same file) already used,
  and requires that root to be a currently-mapped toplevel — this crate has
  no layer-shell, so unlike anvil's version there is no `layer_map_for_
  output` fallback branch to carry.
- Calls `PopupManager::grab_popup` (smithay library code, `smithay::
  desktop`) to obtain a `PopupGrab`, then wires it to the requesting seat
  via `PopupKeyboardGrab::new`/`PopupPointerGrab::new` (also smithay
  library types) through the same `keyboard.set_grab`/`pointer.set_grab`
  calls `move_request`/`resize_request` above already use for move/resize.
  The actual mechanics — outside-click dismissal, nested-popup topmost-only
  enforcement, keyboard-event forwarding while grabbed — are **smithay's
  own library implementation**, not reimplemented here; this function's job
  is only to construct the right types in the right order and hand them to
  the seat.
- Denies (with a `tracing::debug!` line, not a panic) on every failure path
  `PopupManager::grab_popup`/anvil's own version already handles: dead root
  surface, `PopupGrabError` (already-mapped popup, parent dismissed, not
  the topmost popup), or an unrelated grab already holding the keyboard/
  pointer.

**Reference source declaration (task brief's licensing rule):** the
*structure* of `grab()` — which library calls, in what order — is adapted
from smithay's `anvil` example (`anvil/src/shell/xdg.rs`'s own `grab()`),
checked 2026-08-22 against the `v0.7.0` tag specifically (the version this
crate is pinned to, per this file's own "smithay version choice" section
above): `anvil/` lives inside the same `Smithay/smithay` repository as
`smallvil/`, under the repo-root `LICENSE.txt` (MIT, no separate license
file inside `anvil/` — confirmed by listing the tagged tree via GitHub's
API and fetching the root license text directly, not assumed from
memory). This is within the task brief's stated allowance ("MIT 授權的
smithay 本體（含其 anvil 範例）— 可參考"). No GPL project (niri, labwc, or
any other GPL-licensed compositor) was read, referenced, or consulted for
this work. The types actually doing the grab work — `PopupManager::
grab_popup`, `PopupKeyboardGrab`, `PopupPointerGrab`, `PopupUngrabStrategy`
— are smithay **library** code this crate already depends on via
`Cargo.toml`, not anvil application code; anvil's contribution here is the
*usage pattern* (which calls, what order, what to check), simplified for
this crate's plainer `SeatHandler::KeyboardFocus = WlSurface` model (no
`KeyboardFocusTarget` wrapper enum needed — anvil's version threads one
because it also supports layer-shell, this crate doesn't).

**Live evidence** (see "Multi-client live-run" below for the full session):
3 separate popup grabs established against a real `gtk3-widget-factory`
context menu (`xdg_shell: popup grab established`, zero denials), including
one **re-grab after a prior grab was cleanly dismissed** — proving the
compositor returns to a fully re-grabbable state, not a stuck one — and one
confirmed **outside-click dismissal**: right-clicking `gtk3-widget-factory`
opened its context menu (popup grab established), then clicking on an
unrelated `foot` window elsewhere correctly dismissed the popup (the click
went through the normal, non-grabbed `focus_window` path immediately
afterward — `focus: activation set target_surface_id=<the foot window>` —
which could only happen if smithay's `PopupPointerGrab::button` had already
ungrabbed) **and** delivered a real keystroke to the newly-focused window
(a `echo … > file` proof file appeared). Nested popups were not
independently exercised this round (gtk3-widget-factory's menu didn't have
a reachable submenu without pixel-perfect coordinates this headless
container can't provide) — the nested-popup-safe behavior
(`PopupGrabError::NotTheTopmostPopup`, appending to the same seat-scoped
`PopupGrabInner` instead of starting a second independent grab) is entirely
smithay library logic, unmodified by this round, so this is a real but
low-risk gap, not an unverified custom code path.

### 2. Multi-client live-run

Extends this file's "Nested headless live-run" three-layer stack (weston
headless → duduclaw-comp → client) to **three simultaneous real clients**
on layer 3, all connecting to `duduclaw-comp`'s own socket: `foot -a
foot-A`, `foot -a foot-B`, and `gtk3-widget-factory` (Debian's
`gtk-3-examples` package — a real, full-featured GTK3 demo app with a
header bar and right-click context menus, chosen specifically because
`foot`/`weston-terminal` have no menu to exercise popup grabs against).
All three connected and mapped cleanly (`xdg client connected` ×3, `new
toplevel created` ×3 — 4 total toplevels across the whole session counting
one relaunch after a deliberate close).

Since `new_toplevel` maps every window at literal `(0, 0)` (smallvil's own
placement policy — no cascading/tiling logic exists), all three windows
initially overlap exactly. Verification used the compositor's own
click-to-focus + move-grab machinery to pull them apart one at a time via
the codrive injection socket (a real Python client authenticating with the
run's actual token, same protocol shape as this file's earlier CD-round
scripts) — each step's result is directly checkable against the
compositor's own tracing output, not inferred:

- **Move, independence, exact tracking**: dragging the topmost window's
  title-bar band (found empirically — no screenshot access in this
  container, so a handful of candidate points were probed and the *new*
  `move_request`/`resize_request` log lines this round added confirm
  exactly which one fired) moved `gtk3-widget-factory` in a sequence of
  five drags with **exact delta tracking** at every step (`(0,0) → (30,3) →
  (60,6) → (90,9) → (120,12) → (870,542)` in one run) — the CSD's own
  drag-to-move logic on a real GTK client, driving this crate's
  `grabs/move_grab.rs` end-to-end, not a synthetic call. A second `foot`
  window was independently dragged in the same session; neither move
  affected the other windows' positions (confirmed structurally: a
  window's title bar only becomes clickable again at its old screen
  position once the window that was covering it has actually moved away —
  if the move had wrongly dragged multiple windows together, clicking the
  vacated spot would have hit nothing).
- **Resize, independence**: probing near a window's declared right edge
  (found via the *new* `geometry`/`location` fields this round added to
  the `toplevel commit (already configured)` debug log — previously the
  only way to locate a resize hotspot blindly) triggered `resize_request`
  on `foot`'s CSD resize border, distinct from `move_request` triggered a
  few pixels away on the same window — confirmed via the button-code
  distinction alone with no ambiguity. A full drag then grew the window
  from `704×500` to `949×500` (delta matches the drag distance exactly),
  while `gtk3-widget-factory`'s independently-tracked geometry
  (`1356×732` at `(800,535)`) was unchanged in the very next commit log —
  direct proof the resize didn't leak into the other client.
- **Independent keyboard routing**: after separating the windows, each was
  click-focused in turn and sent a *distinct* `echo <marker> > <file>\n`
  text injection; each marker landed in exactly the window that was
  focused at the time (verified via the resulting file's content, not
  just "no error") — proving `input.rs`/`codrive/mod.rs`'s per-seat
  keyboard-focus routing doesn't bleed between clients even with three
  live connections.
- **Closing one doesn't disturb the others**: `kill -TERM` against one
  `foot` process cleanly triggered `toplevel_destroyed` → unmap → (see
  §3 below); the remaining two windows' own state (position, mapped
  status) was unaffected — confirmed by continuing to interact with them
  normally afterward in the same session (further moves/resizes/focus
  changes on the survivors all succeeded).

### 3. Focus/Z-order completion

Three sub-fixes, all in `src/state.rs`'s new `DuduclawComp::focus_window` /
`reassign_focus_on_window_removed` / `cycle_focus` methods (`src/input.rs`
and `src/codrive/mod.rs` now call the shared `focus_window` instead of each
hand-rolling its own raise+focus loop):

- **Real bug fixed: `set_activated(true)` was never called.** Both
  `input.rs`'s human click-to-focus arm and `codrive/mod.rs`'s agent
  click-to-focus arm only ever called `Window::set_activated(false)` on
  the click-on-empty-space path — the window actually being selected never
  had its xdg-shell `activated` state (and any client-side active/inactive
  titlebar styling keyed off it) set. `focus_window` fixes this: every call
  iterates every mapped window and sets `activated` to `true` for exactly
  the target, `false` for everything else. **Live evidence**: a new
  `focus: activation set` debug log (`target_surface_id`, `activated_count`,
  `total_windows`) was added specifically so this could be checked directly
  rather than inferred — every one of the dozens of focus changes in this
  round's live session shows `activated_count=1` matching the correct
  `target_surface_id`, across 1, 2, and 3 mapped windows.
- **Window-close focus handoff, to the next-highest Z-order window.**
  `XdgShellHandler::toplevel_destroyed` (a smithay callback with a no-op
  default that nothing in this crate had implemented before this round) now
  eagerly unmaps the destroyed window from `self.space` (not waiting for
  the next frame's `space.refresh()`) and calls `reassign_focus_on_window_
  removed`, which — **per seat, independently** — hands focus to
  `self.space.elements().next_back()` (the new topmost survivor) **only
  if** that seat's keyboard focus was the just-destroyed surface; a seat
  focused elsewhere is left untouched (closing a background window must
  never steal focus from whatever's actually being used). **Live evidence**
  (a clean, isolated repro, avoiding an earlier same-round test run that
  entangled this with a still-open popup grab — see the honest-stub note
  below): click-focus `foot-A` → confirmed via the activation log → `kill
  -TERM` → `xdg_shell: toplevel destroyed, unmapping and reassigning
  focus` → `focus: closed window held focus — reassigning to the new
  topmost window next_surface_id=<gtk3-widget-factory>` →
  `focus: activation set target_surface_id=<gtk3-widget-factory>
  activated_count=1 total_windows=1`. Exactly the designed behavior, no
  gaps in the log chain.
- **Super+Tab window cycling.** Added to `input.rs`'s human keyboard filter
  closure, alongside the existing Super+Esc/Super+Enter bindings (same
  closure, same structural agent-cannot-reach-this guarantee those two
  already have — see `codrive/mod.rs`'s module doc). No MRU list is
  tracked; `DuduclawComp::cycle_focus` (`state.rs`) instead raises the
  CURRENT BOTTOM of the z-order stack to the top on every press. This is a
  genuine full rotation, not a two-window oscillation — worked out by hand
  before writing the implementation (an earlier candidate design, "raise
  whichever window is one position below the current top," was checked by
  hand-simulating a 3-window stack and found to only ever swap the top two
  elements, never reaching a third window; documented as a rejected
  alternative directly in `cycle_focus`'s doc comment so a future reader
  doesn't have to rediscover the same mistake). **Live evidence**: since
  headless nested weston has no keyboard device at all (the same
  established constraint `codrive/debug_sim.rs`'s module doc already
  documents for Super+Esc/Super+Enter), a fourth debug-stdin command —
  `simulate_super_tab`, calling `cycle_focus()` directly — was added
  following the exact same pattern (opt-in via
  `DUDUCLAW_CODRIVE_DEBUG_STDIN=1`, true no-op otherwise). With 2 mapped
  windows, four consecutive simulated presses correctly alternated
  `gtk3-widget-factory ↔ foot-C` every time (the expected degenerate case
  of a full rotation at N=2), each followed by an `activated_count=1`
  activation-set log matching the newly-topmost window. **Not verified
  this round**: an actual keystroke landing in the cycled-to window from
  Super+Tab specifically — `cycle_focus` operates on the **human** seat
  (`self.seat`), and this round's text-delivery proofs all went through
  the **agent** seat (the codrive injection socket); the two are
  independent by design (see `codrive/mod.rs`'s module doc on why human
  and agent inputs are structurally separated), so agent-seat text
  injection cannot exercise human-seat focus. `focus_window` itself —
  the exact function `cycle_focus` calls, parametrized only by which
  `Seat` handle is passed — is the same function proven to deliver real
  keystrokes correctly on the agent seat throughout this round's other
  tests; `keyboard.set_focus()` has no seat-specific branching inside it.
  Real Super+Tab keystroke delivery on the human seat is real-hardware
  territory, same category (and same VM/`cage` closure path) as this
  file's existing Super+Esc/Super+Enter real-hardware gaps.

### Reproducible command (this round)

Same one-shot shape as the CD-0 round's command above (weston headless +
comp + `DUDUCLAW_CODRIVE_DEBUG_STDIN=1` via a FIFO), with `gtk-3-examples`
added to the `apt-get install` line and three clients (`foot -a foot-A`,
`foot -a foot-B`, `gtk3-widget-factory`) launched on layer 3 instead of
one. The actual verification run used a long-lived `docker exec` dev
container (same zombie-reaping caveat this file's CD-0 section already
documents) plus a small reusable Python client
(`auth`/`move`/`button`/`click`/`drag`/`text`/`status` helpers over the
codrive socket) to drive move/resize/focus/popup interactions iteratively
against the live compositor — not reproduced verbatim as one shell block
here since the exact pixel coordinates for each client's title bar/resize
border were found empirically this round (see §2 above) and are recorded
directly in this section's evidence rather than hard-coded into a
throwaway script.

### Build/clippy/test (this round)

```
cargo build                              -> Finished, zero warnings
cargo clippy --all-targets -- -D warnings -> Finished, zero warnings
cargo test                               -> 70 passed; 0 failed
```

70 is unchanged from the CD-3 round's count: this round's new logic
(`focus_window`, `reassign_focus_on_window_removed`, `cycle_focus`,
`grab()`, `toplevel_destroyed`) all touch live `Seat`/`Space`/`Window`
state that cannot be constructed in a unit test without a real Wayland
display (confirmed by checking: none of the existing 70 tests construct
real smithay `Window`/`Space`/`Seat` objects either — every one of them
tests pure protocol-parsing/decision logic, e.g. `is_system_gesture_tail`,
`freeze_bypass_decision_*`, `check_token`). This round's live-run container
evidence above is this crate's established substitute for that category of
logic, not a gap introduced this round.

### File-size housekeeping paid down this round

`codrive/mod.rs` was sitting at exactly 800 lines (the project's hard
per-file cap) before this round touched it — the task brief required
paying that down before adding content. `CodriveShared` (struct + its
`disabled()`/`disabled_keep_audit()`/new `new()` constructor/`record()`/
`is_frozen()`/`check_token()`/`push_event()`/test-only builders) moved to
a new `codrive/shared.rs` (274 lines); `mod.rs` dropped to 591 lines before
this round's own additions, leaving headroom. Field/method visibility for
the handful of items the parent module and sibling submodules (`rotation.
rs`, `listener.rs`) still reach directly (`active_conn`, `auth_token`,
`token_path`, plus the moved constructors) went from module-private to
`pub(super)` — the minimum bump that restores *exactly* the same
reachability those call sites had before the split, verified by grepping
every direct field access across the whole `codrive/` tree before deciding
which fields needed it (only 3 of 8 fields did; `audit` stays fully
private, touched only inside `shared.rs` itself). All 70 pre-existing
tests (including the 3 `check_token` tests, moved to live alongside the
code they test in `shared.rs`) passed unchanged after the split, before
any of this round's new logic was added on top — confirming the extraction
was behavior-preserving on its own.

### Honest debt / limitations (this round)

- **No visual/screenshot verification** — this headless container has no
  `screendump`/QMP framebuffer access (see this file's R1 notes above);
  every claim in §2/§3 is evidence from the compositor's own tracing
  output and real client-visible side effects (files written by injected
  keystrokes, exact geometry deltas matching drag distances), not pixel
  comparison. Visual confirmation (does the activated window's CSD
  actually look different, are both cursors distinct on screen) remains
  VM/QMP acceptance-side work, same category this file has flagged since
  the base spike's "VM cage real-seat input verification" section.
- **Super+Tab's real-hardware keystroke delivery is unverified** — see §3's
  own paragraph above for the full reasoning (human-vs-agent seat
  separation, `focus_window` code-path reuse as indirect evidence).
- **Nested popups were not independently exercised** — see §1's own
  paragraph above (smithay library logic, not custom code, so a real but
  low-risk gap).
- **One test-session sequencing mistake, corrected, worth recording**: the
  first attempt at the window-close-focus-transfer test entangled a
  still-open popup grab (opened by an earlier test step and never
  explicitly dismissed) with the close event — the destroyed window
  wasn't the popup-grab-holding seat's actual focus target, so no
  reassignment fired, which is *correct* behavior but looked surprising
  until traced back to the un-dismissed popup. Re-run cleanly (dismiss
  first, confirm focus via the activation log, then close) for the
  evidence quoted in §3. Kept here rather than silently redone, per this
  crate's own "honest stub" convention — the compositor behaved correctly
  both times; the first attempt's test design didn't isolate the variable
  it meant to.
- **CSD title-bar/resize-hotspot coordinates were found empirically, not
  computed** — this container has no screenshot access, so candidate
  points were probed and confirmed via the new `move_request`/
  `resize_request`/geometry-in-commit log lines this round added
  specifically to make that possible. This is now a repeatable technique
  (documented here) for any future round needing the same, not a one-off
  hack.
- **Window placement is still literal `(0,0)` for every new toplevel** —
  unchanged smallvil behavior, not addressed this round (out of scope: the
  task brief's four deliverables didn't include cascading/tiling
  placement). All of this round's multi-window separation was done via the
  existing move-grab machinery, which is itself part of what was being
  verified.

## WP-CD4a-COMP: `activate_window` (2026-08-22)

B-line CD-4a, multi-window targeting: a wire op that raises/focuses a
mapped toplevel by xdg-shell app_id (exact match, priority) or a
title-prefix fallback, reusing the WP-A1 `DuduclawComp::focus_window`
helper unchanged — this round adds a lookup layer on top, not new
focus/activation mechanics.

### What changed

- **`src/codrive/protocol.rs`**: new `InjectCmd::ActivateWindow { app_id:
  String }` variant + `describe()` arm + `MAX_ACTIVATE_WINDOW_QUERY_BYTES`
  (255 bytes, same "reject not truncate" reasoning as
  `MAX_TAKE_OVER_REASON_BYTES`).
- **`src/codrive/window_target.rs`** (new file, ~260 lines): the matching
  policy is split into a pure function (`match_window_query`, no
  `Window`/`Space`, unit-testable) and a thin real-state wrapper
  (`find_target_window`) — same "pure logic unit-tested, live Seat/Space
  state live-run-tested" split `shadow.rs`'s `freeze_bypass_decision` /
  `is_freeze_bypass_eligible` pair already established. `window_identity`
  reads `XdgToplevelSurfaceData.{app_id,title}` via the same
  `with_states`/`data_map` pattern `handlers/xdg_shell.rs::handle_commit`
  already uses. `DuduclawComp::codrive_activate_window` is the main-thread
  entry point: on a hit, calls `focus_window` and records an `activate_
  window` audit line (`detail` carries the query and whichever criterion
  matched — `matched_app_id` or `matched_via=title_prefix matched_title`);
  on a miss, records `activate_window_failed` (`detail` carries the query)
  — never a silent no-op. A `tracing::debug!` line logs every currently-
  known `(app_id, title)` pair on every call, specifically so a live
  session can discover what a real client actually registered instead of
  guessing (same motive as this file's own "CSD title-bar/resize-hotspot
  coordinates were found empirically" note above).
- **`src/codrive/shadow.rs`**: `freeze_bypass_decision` gained an explicit
  `InjectCmd::ActivateWindow { .. } => false` arm (task brief: "凍結/
  takeover 中拒絕（照既有 op 閘）" — reuse the standard gate, no new bypass
  carve-out; unlike `Move`/`Button`/`Highlight` this op carries no target
  coordinate to confirm against `SHADOW_ORIGIN`'s bounds at all) + one new
  unit test.
- **`src/codrive/listener.rs`**: `validate()` gained an `ActivateWindow`
  arm (rejects empty or oversized `app_id`, same shape as `TakeOver`'s
  reason-length check). This pushed the file to 812 lines, over the
  project's 800-line cap, so its entire `#[cfg(test)] mod tests` block
  (all pre-existing tests, unchanged) moved to a new **`src/codrive/
  tests_listener.rs`** — same "new/split scenarios get their own
  `tests_<topic>.rs`" pattern `tests_takeover.rs` already established for
  CD-3. `listener.rs` dropped to 474 lines; five new
  `activate_window`-specific tests were added to `tests_listener.rs`
  alongside the moved ones.
- **`src/codrive/mod.rs`**: `mod window_target;` + `#[cfg(test)] mod
  tests_listener;` declared; `handle_agent_inject`'s match gained a thin
  `InjectCmd::ActivateWindow { app_id } => self.codrive_activate_window(app_id),`
  arm (all real logic lives in `window_target.rs`); module doc updated
  with an item (11) entry.

### Wire protocol addition

```
{"op":"activate_window","app_id":"foot-A"}
  -> {"ok":true,"frozen":false}    (forwarded to the main thread; success/
                                     failure is decided there and only
                                     visible via the audit trail/logs, same
                                     as every other seat/space-touching op)
  -> {"ok":false,"frozen":true,"reason":"agent_seat_frozen"}   (denied
                                     outright while frozen — never
                                     shadow-bypass-eligible)
```

Audit `kind`s: `activate_window` (hit — `detail` has `matched_app_id` or
`matched_via=title_prefix matched_title`) / `activate_window_failed` (miss
— `detail` has the query) / the generic `inject_dropped` (denied while
frozen, at the socket-thread pre-check) that every other op already
produces.

### Build/clippy/test (this round)

```
cargo build                              -> Finished, zero warnings
cargo clippy --all-targets -- -D warnings -> Finished, zero warnings
cargo test                               -> 82 passed; 0 failed
```

82 = 70 (A1 baseline) + 12 new (6 in `window_target.rs`'s pure
`match_window_query` tests covering 命中/查無/title-prefix-fallback/
priority-ordering/anchored-not-substring/z-order-tie-break; 1 in
`shadow.rs` proving `ActivateWindow` never bypasses a freeze; 5 in
`tests_listener.rs` covering wire validation and the socket-thread
frozen-denial + not-frozen-forwarding pair).

### Live verification (this round, nested headless weston, `DUDUCLAW_CODRIVE_DEBUG_STDIN=1`)

Same three-layer stack as the A1 round's multi-client live-run (weston
headless → duduclaw-comp → real clients), with `foot -a foot-A` and
`gtk3-widget-factory` (Debian's `gtk-3-examples` package) as the two
concurrent app_id-distinct clients, driven over the real codrive socket
with a Python client authenticating with the run's actual token.

**Ground truth discovered live** (via this round's new diagnostic debug
line — neither app_id was known ahead of time): `foot -a foot-A` registers
app_id `"foot-A"` / title `"foot"`; `gtk3-widget-factory` registers BOTH
app_id and title as the literal string `"gtk3-widget-factory"`.

**命中 (hit via exact app_id)** — two distinct queries against the two live
clients each raised/focused the correct, distinct surface:

```
query=foot-A               -> focus: activation set target_surface_id=...wl_surface@3[0]...  activated_count=1
  audit: {"kind":"activate_window","detail":"query=\"foot-A\" matched_app_id=\"foot-A\"","frozen":false}

query=gtk3-widget-factory   -> focus: activation set target_surface_id=...wl_surface@26[1]...  activated_count=1
  audit: {"kind":"activate_window","detail":"query=\"gtk3-widget-factory\" matched_app_id=\"gtk3-widget-factory\"","frozen":false}
```

Re-issuing `foot-A` after the gtk3 query flipped `activated_count=1` back
to the foot-A surface — proving each call independently retargets, not a
one-shot/sticky effect.

**title 前綴回退 (title-prefix fallback)** — query `"gtk3"` is NOT an exact
app_id match (`"gtk3-widget-factory" != "gtk3"`) but IS a genuine prefix of
that window's title, isolating priority 2 from priority 1:

```
query=gtk3   -> focus: activation set target_surface_id=...wl_surface@26[1]...  activated_count=1
  audit: {"kind":"activate_window","detail":"query=\"gtk3\" matched_via=title_prefix matched_title=\"gtk3-widget-factory\"","frozen":false}
```

**查無 (not found)** — `"GTK"` (case-sensitive miss) and
`"does-not-exist-xyz"` both produced an honest failure, zero focus change:

```
query=GTK                  -> WARN codrive: activate_window — no toplevel matched by app_id or title prefix
  audit: {"kind":"activate_window_failed","detail":"query=\"GTK\" — no toplevel matched by app_id (exact) or title (prefix)","frozen":false}
query=does-not-exist-xyz   -> (same shape)
```

No `focus: activation set` line appears for either — confirmed by grepping
the full log window between the two audit lines.

**凍結中拒 (denied while frozen)** — `simulate_human` (debug stdin) froze
the seat, then `activate_window` was denied at the SOCKET-THREAD
pre-check — never even reaching `window_target.rs` (no
`codrive::window_target` log line appears between the freeze and the
denial):

```
{"op":"activate_window","app_id":"gtk3-widget-factory"} -> {"ok":false,"frozen":true,"reason":"agent_seat_frozen"}
audit: {"kind":"inject_dropped","op":"activate_window","detail":"agent seat frozen (human input active) — dropped, not buffered","frozen":true}
```

`simulate_super_enter` (Super+Enter resume) then cleared the freeze
(`{"kind":"resume","op":"human_super_enter","frozen":false}`), and the very
next `activate_window` call against the same query succeeded normally —
proving the denial was freeze-scoped, not a permanent failure.

### Honest stub / limitation list (this round)

- **No visual/screenshot verification** — same category of limitation this
  file has flagged since the base spike; every claim above is evidence
  from the compositor's own tracing output and audit trail, not pixel
  comparison.
- **Takeover-specific denial has no separate live test** — logically
  redundant, not skipped: `freeze_bypass_decision`'s `ActivateWindow` arm
  is unconditionally `false` regardless of `shadow_active`/
  `takeover_active`, and `handle_agent_inject`'s gate is `frozen &&
  !bypass` — since bypass is always `false` for this op, the takeover case
  is a strict subset of the plain-frozen case already proven live above
  (an active takeover always implies `frozen == true`). The `shadow.rs`
  unit test (`freeze_bypass_decision_activate_window_never_bypasses`)
  covers the `shadow_active=true` half of this claim directly.
- **Z-order tie-break (two windows sharing the same app_id) is unit-tested
  only, not live-run-proven** — this round's live session only ever had
  one window per app_id at a time; `match_window_query_ties_resolve_to_
  the_lowest_z_order_index` is the pure-function proof.
- **Case sensitivity is exact, by design, not separately flagged as a
  limitation** — `"GTK"` missing `"gtk3-widget-factory"` above is the
  expected, tested behavior (`match_window_query_title_prefix_is_anchored_
  not_substring`'s sibling concern), not a bug found live.
- **Not committed** — per this task's instructions, same as every prior
  round in this file.

## WP-comp-shell-ipc: shell↔comp window query/control socket (2026-08-22)

A3's dock integration needed comp to answer "what windows are running" and
"switch to this one" — but `codrive`'s injection socket is the AGENT's
private, token-authenticated channel (every command through it is
attributed to the agent seat and lands in `codrive`'s own audit trail as an
agent action). Routing a human's dock click through it would misattribute
a human action as an agent action. This round adds a SECOND, entirely
separate Unix socket, wire protocol, and audit trail:
`$XDG_RUNTIME_DIR/duduclaw-shell.sock` — see `src/shell_control/mod.rs`'s
module doc for the full design rationale (reproduced in summary below).

### Trust boundary — same-uid `SO_PEERCRED`, not a bearer token

`codrive`'s token exists because an agent CLI subprocess and this
compositor are not the same trust domain in general. `duduclaw-shell` and
`duduclaw-comp`, by contrast, ARE: on the appliance both run under the same
kiosk-session user (`duduclaw-kiosk`,
`appliance/mkosi.extra/etc/systemd/system/duduclaw-kiosk.service`), while
agent CLI subprocesses run under a DIFFERENT user (`duduclaw`,
`duduclaw-gateway.service` — see `appliance/postinst.d/
20-users-and-units.sh`'s own useradd comments). That is a real,
kernel-enforced boundary a same-uid `SO_PEERCRED` check can use directly —
the exact pattern `duduclaw-sysd` already established for its own root
daemon (`duduclaw-sysd/src/server.rs::handle_connection`), simplified from
"an externally configured allowed uid" to "my own uid" since this process
and its legitimate caller are always the same user by construction.

`std::os::unix::net::UnixStream::peer_cred()` looked like the obvious API
but is gated behind the unstable `peer_credentials_unix_socket` feature on
this crate's pinned toolchain (`rustc 1.97.1` — confirmed by trying it
first and reading the resulting `E0658`, not assumed); `duduclaw-sysd` gets
away with a similarly-named method because that's TOKIO's own `peer_cred()`
(tokio implements `SO_PEERCRED` itself), not std's. `src/shell_control/
listener.rs::peer_uid` hand-rolls the raw `getsockopt(SOL_SOCKET,
SO_PEERCRED)` call instead — exactly what tokio's own implementation does
under the hood — using `libc` (already a dependency, per `Cargo.toml`'s
existing CD-2 comment).

**No freeze gate.** A dock click is human input by definition (this socket
cannot be reached by anything that isn't already authenticated as the same
uid as the kiosk session), so it is never gated behind `codrive.frozen`/
`terminated`/`takeover_active` — a human can always operate their own
desktop. This is also not a red-line-3 backdoor for the agent to bypass its
own freeze: the same-uid auth means an agent CLI subprocess structurally
cannot open this socket in the first place.

**One-shot RPC, not a persistent session.** Unlike `codrive`'s long-lived,
single-connection-at-a-time session, this socket is a plain connect → one
request line → one response line → close round trip per call — the natural
shape for a dock polling `list_windows` on an interval and firing
`focus_window` on a click.

### What changed (comp side)

- **`src/codrive/window_target.rs`** / **`src/codrive/mod.rs`**: `find_
  target_window`/`window_identity`/`WindowMatch` widened from module-private
  to `pub(crate)` (`window_target` mod likewise) so `shell_control` can
  reuse the EXACT SAME matching policy `activate_window` already proved
  live (WP-CD4a-COMP) — no logic duplicated, only visibility.
- **`src/shell_control/`** (new directory, 4 files):
  - `protocol.rs` (~215 lines) — `ShellControlRequest` (`ListWindows` /
    `FocusWindow { query }`), ADJACENTLY tagged (`tag = "op", content =
    "params"`, `deny_unknown_fields`) — unlike `codrive::protocol::
    InjectCmd`'s internal tagging, found empirically (not assumed) that
    `deny_unknown_fields` does not reliably reject a stray key next to an
    internally-tagged unit variant; adjacent tagging (mirroring
    `duduclaw-sysd::protocol::SysdRequest`'s own shape) sidesteps that.
    `ShellControlResponse` is one flat envelope struct with `Option`
    fields, same shape convention as `SysdResponse`.
  - `audit.rs` (~95 lines) — `ShellControlAuditLog`, a SEPARATE struct/file
    from `codrive::audit::AuditLog` (no `frozen` column — this channel has
    no freeze concept), writing `duduclaw-shell-control-audit.jsonl`. Only
    `focus_window`/`focus_window_failed`/`auth_denied` are audited —
    `list_windows` is read-only and realistically polled every few
    seconds, so auditing it would be noise, matching the "queries aren't
    audited, actions are" precedent `codrive::listener`'s own `status`
    handling already set.
  - `listener.rs` (~330 lines) — one dedicated thread accepts connections
    SEQUENTIALLY (each is a fast one-shot round trip, so no per-connection
    thread is warranted); `peer_uid()` hand-rolled `SO_PEERCRED` read (see
    above); `is_authorized_peer` is a pure predicate; a request is bridged
    to the calloop main thread via a `std::sync::mpsc` ONESHOT reply
    channel wrapped in `ShellControlMsg` (unlike `codrive`'s fire-and-forget
    `InjectCmd` channel, this caller genuinely needs the real computed
    answer) — `recv_timeout(3s)` bounds the wait so a stalled main loop
    degrades this one connection to a timeout instead of hanging the
    listener thread (which would starve every later caller, since
    connections are handled sequentially).
  - `mod.rs` (~215 lines) — `ShellControlShared` (just the audit log — no
    frozen/token state), `init()` (mirrors `codrive::init`'s call shape,
    computes `own_uid` via `libc::getuid()`, no `SeatState`/`DisplayHandle`
    needed since no new `wl_seat` is created), and `DuduclawComp::
    handle_shell_control_request`/`shell_control_list_windows`/
    `shell_control_focus_window` — the latter reuses `codrive::window_
    target::find_target_window` then calls the SAME shared `focus_window`
    helper (WP-A1) `codrive`'s own `activate_window` uses, but against
    `self.seat` (the HUMAN seat), never `self.agent_seat`.
- **`src/state.rs`**: new `pub shell_control: Arc<shell_control::
  ShellControlShared>` field, `shell_control::init(event_loop)` called
  right after `codrive::init` in `DuduclawComp::new`.
- **`src/main.rs`**: `mod shell_control;`.

### Op definitions / wire protocol

```
{"op":"list_windows"}
  -> {"ok":true,"windows":[{"app_id":"foot-A","title":"foot","focused":true}, ...]}

{"op":"focus_window","params":{"query":"foot-A"}}
  -> {"ok":true,"matched_app_id":"foot-A"}                      (exact app_id hit)
  -> {"ok":true,"matched_title_prefix":"Bar — Editor"}          (title-prefix fallback hit)
  -> {"ok":false,"error":"not_found"}                           (honest miss, never a silent no-op)

(unauthorized peer)          -> {"ok":false,"error":"unauthorized"}
(malformed/oversized/unknown) -> {"ok":false,"error":"parse_error"|"line_too_long"|...}
```

Audit `kind`s (own file, `duduclaw-shell-control-audit.jsonl`): `focus_window`
(hit — `detail` carries the query and matched criterion, same shape
`codrive`'s own `activate_window` detail uses) / `focus_window_failed`
(miss) / `auth_denied` (peer uid mismatch, logged with both uids).

### Build/clippy/test (this round)

```
cargo build                              -> Finished, zero warnings
cargo clippy --all-targets -- -D warnings -> Finished, zero warnings
cargo test                               -> 104 passed; 0 failed
```

104 = 82 (WP-CD4a-COMP baseline) + 22 new in `shell_control/` (9 in
`protocol.rs`: wire round-trips, `unknown_op`/`unknown_field`/malformed
JSON rejection, `op_name` stability, response-shape omission checks; 2 in
`audit.rs`: one JSONL line per `record()` call with `kind`/`detail`, 0600
file mode; 11 in `listener.rs`: 4 pure `validate()` cases, 3 pure
`is_authorized_peer` cases (matching uid / different uid / unreadable
credentials — the "agent cannot reach this socket" proof, see below), and
4 real-socket round trips using a fake "main thread" stand-in that counts
how many `ShellControlMsg`s it actually received).

**The "agent 拿不到殼 socket" test**: `shell_control::listener::tests::
mismatched_configured_uid_denies_connection_before_it_ever_reaches_the_main_thread`
— configures the listener's OWN uid to `current_uid() + 1` (same strategy
`duduclaw-sysd::server::tests::mismatched_uid_is_rejected` already
established as this codebase's accepted way to test this property without
root: the CONFIGURED side, not the real peer, is varied), connects as the
real test-process uid, and asserts BOTH that the wire response is
`{"ok":false,"error":"unauthorized"}` AND that the fake main-thread
stand-in's received-message counter stayed at 0 — proving an unauthorized
peer's request never reaches `self.space`/`self.seat` at all, not just
that its response looks like a rejection. `shell_control::listener::tests::
is_authorized_peer_rejects_a_different_uid`/`::is_authorized_peer_rejects_
unreadable_peer_credentials` cover the pure predicate directly.

### Live verification (this round, nested headless weston)

Same three-layer stack (`weston --backend=headless-backend.so` → `duduclaw-
comp` → real xdg-shell clients) as prior rounds, driving the shell-control
socket over Python (`socket.AF_UNIX`) against two REAL `foot -a foot-A`/
`foot -a foot-B` clients:

```
list_windows (before focus)  -> {'ok': True, 'windows': [
    {'app_id': 'foot-A', 'title': 'foot', 'focused': False},
    {'app_id': 'foot-B', 'title': 'foot', 'focused': False}]}
focus_window foot-A          -> {'ok': True, 'matched_app_id': 'foot-A'}
list_windows (after)         -> foot-A now focused:true, foot-B focused:false
focus_window foot-B          -> {'ok': True, 'matched_app_id': 'foot-B'}
list_windows (after)         -> foot-B now focused:true, foot-A focused:false
focus_window does-not-exist-xyz -> {'ok': False, 'error': 'not_found'}
```

`duduclaw-comp.log` confirms the SAME `focus: activation set` line
`state.rs::focus_window` already logs for the human/agent paths, now fired
by `shell_control`; the shell-control audit file contains exactly the 3
`focus_window`/`focus_window_failed` lines with correct `query`/`matched_*`
detail — `list_windows` produced ZERO audit lines (by design). Socket +
audit file both confirmed `srw-------`/`-rw-------` (0600).

**Cross-user proof the agent boundary is real, not just unit-tested**: the
container runs as root, so a genuinely different-uid user (`useradd -u
1500 testagent`, standing in for the appliance's `duduclaw` gateway/agent
user vs. `duduclaw-kiosk`'s comp/shell user) was actually created and used
to probe the running comp instance:

```
root (own uid, authorized)  -> list_windows succeeds: {"ok":true,"windows":[]}
testagent (uid 1500) connect() -> PermissionError: [Errno 13] Permission denied
  (refused at the OS/filesystem layer — 0600, owned by root — before this
   thread's peer-uid check even runs)
testagent cat duduclaw-codrive.token        -> Permission denied
testagent cat duduclaw-shell-control-audit.jsonl -> Permission denied
```

Two independent layers (filesystem 0600 perms AND the in-process
`SO_PEERCRED` check) both deny a real different-uid caller; the unit tests
above additionally prove the in-process check alone (varying the
CONFIGURED side, since a real different-uid *listener* isn't constructible
without root either) — together, live cross-user proof of the outer layer
plus a direct proof of the inner layer's logic.

### Live verification (this round, comp hosting `duduclaw-shell` as its own client)

Combined round: `weston` (headless, layer 1) → `duduclaw-comp` (layer 2,
now ALSO the HOST providing a real `wl_seat` — unlike weston's headless
backend, which advertises none, see `duduclaw-shell/BUILD-LINUX.md`'s own
"`wl_seat` finding") → `foot -a foot-A` (layer 3a) + `duduclaw-shell`
(layer 3b, `WAYLAND_DISPLAY=wayland-1`, `DUDUCLAW_SHELL_SKIP_OOBE=1`,
`DUDUCLAW_SHELL_DIAG=1`) as TWO concurrent clients of comp. `duduclaw-shell`
ran the full 15s under `timeout` with **zero panics** — comp's own `wl_seat`
(keyboard+pointer, `state.rs::new`) is sufficient for gpui, confirming the
`wl_seat.unwrap()` panic `BUILD-LINUX.md` found is specific to weston's
headless backend, not a gpui limitation in general.

`duduclaw-shell`'s own passive dock poll (`home/home_dock.rs::schedule_
running_windows_poll`) fired for real and logged:

```
[dock] list_windows ok: 2 window(s): [
  CompWindow { app_id: Some("foot-A"), title: Some("foot"), focused: false },
  CompWindow { app_id: None, title: None, focused: false }]
```

(the second, app_id-less entry is `duduclaw-shell`'s own mapped window —
gpui does not set an xdg-shell app_id, an honest finding, not a bug).
Concurrently, a SEPARATE process ran `duduclaw-shell`'s real `comp_client`
Rust code (not Python this time) via its `#[ignore]`d live tests:

```
cargo test -- --ignored live_list_windows_against_real_comp live_focus_window_against_real_comp --nocapture
[live] focus_window("foot-A") matched: AppId("foot-A")
[live] 2 window(s): [...]
[live] focus_window on a bogus query correctly returned Comp("not_found")
test result: ok. 2 passed; 0 failed
```

`duduclaw-comp.log` shows the resulting `focus: activation set` +
`shell_control: focus_window — window focused` lines, and the audit file
gained the matching `focus_window`/`focus_window_failed` rows — proving
comp correctly serves TWO concurrent one-shot callers (the live-running
`duduclaw-shell` process's own background poll, and the separate test
process) without interference, since `listener.rs` handles connections
sequentially but each is fast enough that this never queued visibly.

### Honest stub / limitation list (this round)

- **Dock click-to-focus was not exercised via real input** — this round's
  containers have no real seat/pointer (same category of gap `BUILD-LINUX.
  md`'s own "Input devices remain unverified" section already flags for
  this crate, and this file's own "VM cage real-seat input verification"
  section already flags for `codrive`'s human path). What WAS verified:
  the exact function (`comp_client::focus_window`) `home_dock.rs`'s click
  handler calls really works end-to-end against a real comp instance —
  only the mouse-click-delivers-the-call step is unverified, deferred to a
  VM/`cage` round same as every other real-input gap in this project.
  clicking-a-running-icon vs. clicking-a-not-yet-running-icon is unit-
  tested at the `is_app_running`/`RunningWindowsFeed` logic layer
  (`home/running_windows.rs`'s own test module), just not through a real
  mouse event.
- **`flatpak_id`-as-xdg-app_id-query is an assumption, not cross-checked
  against a real flatpak app** — `home/running_windows.rs`'s own module
  doc flags this: flatpak isn't provisioned into the dev/appliance images
  yet (A4 pending, same gap `apps.rs`'s own header comment already
  documents), so whether a real flatpak-packaged GUI app's xdg-shell
  app_id actually equals its flatpak application id in practice is
  untested end-to-end; only `foot`'s hand-set `-a` app_id was available to
  test against this round.
- **No visual/screenshot verification** — same category of limitation this
  file and `BUILD-LINUX.md` have flagged since their base spikes; every
  claim above is log/audit-trail evidence, not pixel comparison.
- **Concurrent-connection stress is not load-tested** — this round's two
  simultaneous one-shot callers never actually collided in the kernel
  backlog (both completed well within `listener.rs`'s sequential handling),
  so contention under many concurrent dock+debug-tool callers is unproven,
  same "no connection-limiting beyond the OS backlog" honesty
  `duduclaw-sysd/src/server.rs`'s own module doc already accepts for its
  analogous design.
- **Not committed** — per this task's instructions, same as every prior
  round in this file.

## A4-1: udev/DRM backend — owning real hardware (2026-08-22)

### What changed and why

Until this round, on the appliance image `duduclaw-comp` was **not** the
thing that owned the display. The stack was three layers deep:

```
cage (wlroots kiosk compositor: DRM/KMS + seatd + libinput)
  └─ duduclaw-comp (winit backend — cage's single fullscreen client)
       └─ third-party client (foot, …)
```

`cage` existed purely because this crate had no hardware backend. That cost
a whole extra compositor's worth of CPU (measured on the appliance VM: `cage`
alone at ~100% of a core, load average 5.6 on 4 vCPU with the nested comp on
top) and put an uncontrollable component between DuDuClaw and the seat.

A4-1 removes that layer. The target stack is now two:

```
duduclaw-comp (udev backend: libseat + DRM/KMS + GBM/EGL + libinput)
  └─ third-party client (foot, …)
```

### New files / changed files

| File | What |
|---|---|
| `src/udev_backend.rs` | **New.** The whole hardware backend: libseat session, udev GPU discovery, DRM/KMS connector→CRTC→surface setup, GBM allocator + EGL/GLES renderer, libinput seat, vblank-driven + damage-driven repaint. |
| `src/backend_choice.rs` | **New.** Pure runtime backend-selection rule + 10 unit tests. |
| `src/render.rs` | **New.** `CodriveElement` (the `render_elements!` enum) moved here out of `winit_backend.rs` so both backends can share it. Definition unchanged. |
| `src/main.rs` | `CalloopData` gains `udev: Option<UdevBackendState>`; backend chosen at startup; the calloop post-dispatch callback drives `udev_backend::dispatch_render`. |
| `src/state.rs` | `pending_redraw` field + `queue_redraw()` + `primary_output()`. |
| `src/input.rs` | `InputEvent::PointerMotion` (relative) implemented; `PointerMotionAbsolute` output-selection bug fixed; `clamp_pointer`/`clamp_to` added. |
| `src/winit_backend.rs` | Imports `CodriveElement` from `render.rs`; clears `pending_redraw`; carries the A4-1 note on why it stays unconditional-redraw. |
| `src/handlers/compositor.rs`, `src/handlers/xdg_shell.rs`, `src/codrive/mod.rs`, `src/codrive/highlight.rs` | `queue_redraw()` damage sources; `codrive_highlight_elements_at(now, offset)` added (the zero-offset wrapper keeps the old behaviour byte-identical). |
| `Cargo.toml` | smithay features `backend_drm` / `backend_gbm` / `backend_egl` / `backend_libinput` / `backend_udev` / `backend_session_libseat` / `renderer_gl` added to the existing `backend_winit` set. |

No new **crate** dependencies: `drm`, `gbm`, `input` (libinput), `udev` and
`rustix` all come through `smithay::reexports`, which is deliberate — those
re-exports are the exact versions smithay itself was built against, and
mixing versions there produces handle types that don't unify.

### Extra system build dependencies

The original three (`pkg-config`, `libwayland-dev`, `libxkbcommon-dev`) are
no longer enough. The full list is now:

```
pkg-config libwayland-dev libxkbcommon-dev \
libinput-dev libudev-dev libseat-dev libgbm-dev libdrm-dev
```

(Versions present in `rust:bookworm` at the time of writing: libinput 1.22.1,
libseat 0.7.0, gbm 22.3.6, libdrm 2.4.114, libudev 252 — all satisfied by
Debian bookworm's own packages, no backports.)

Runtime deps on the appliance are unchanged from what the image already
carries for `cage`: `seatd` (already installed and enabled — Debian runs it
as `seatd -g video`), plus mesa's GBM/EGL/GLES userspace.

### Runtime backend selection

`src/backend_choice.rs`, unit-tested, no compile-time split:

| `DUDUCLAW_COMP_BACKEND` | `WAYLAND_DISPLAY` / `DISPLAY` | backend |
|---|---|---|
| unset/empty | either set and non-empty | `winit` (nested) |
| unset/empty | both unset/empty | `udev` (real hardware) |
| `winit` / `nested` | anything | `winit` |
| `udev` / `drm` / `kms` | anything | `udev` |
| anything else | anything | **hard error**, process exits |

A typo'd override is refused rather than silently falling back — a wrong
backend that "sounds like it worked" is the failure mode this avoids.

There is deliberately **no winit fallback** when udev init fails: on a bare
TTY there is nothing to nest inside, so a fallback would just be a second,
more confusing error. The real reason is reported and the process exits
non-zero.

Other environment variables:

- `DUDUCLAW_COMP_DRM_DEVICE=/dev/dri/card0` — overrides udev's idea of the
  primary GPU (mirrors anvil's `ANVIL_DRM_DEVICE`).
- `DUDUCLAW_COMP_SEAT_ORDER=human-first` — restores the pre-A4-5 `wl_seat`
  advertisement order (human seat first). Default is `agent-first`; see
  "A4-5: `wl_seat` advertisement order" at the bottom of this file for why
  the default flipped and what it costs.
- All the pre-existing ones (`DUDUCLAW_CODRIVE_*`) are unchanged.

### Repaint scheduling (the CPU requirement)

The winit backend calls `window.request_redraw()` at the end of every frame:
an unconditional full composite whether or not anything changed. The udev
backend never does that. Instead:

1. Anything that can change a pixel calls `DuduclawComp::queue_redraw()`.
   The complete list of damage sources is in that method's doc comment:
   client commits (`handlers/compositor.rs::commit`), toplevel/popup
   create+destroy (`handlers/xdg_shell.rs`), every human input event
   (`input.rs::process_input_event`), every applied agent injection
   (`codrive/mod.rs::handle_agent_inject`), every focus/activation change
   (`state.rs::focus_window`, which every focus path funnels through), and
   the udev backend's own housekeeping tick for codrive highlight expiry and
   watch-mode freeze transitions.
2. `dispatch_render` runs after every calloop dispatch and composites only
   outputs that are dirty and not already awaiting a page flip
   (`udev_backend::should_render`, unit-tested).
3. `render_output` returns `damage: None` for a frame identical to the last
   one. In that case **no page flip is queued at all** — the scanout keeps
   the buffer it already has.
4. `event_loop.run(None, …)` blocks in `epoll`. With nothing to draw, nothing
   wakes the process except the 1 Hz housekeeping tick.
5. The one path with no hardware pacing is "dirty, but the frame came out
   identical" (e.g. a client that commits a frame-callback request with no
   buffer damage in response to the frame callback we just sent). That is
   held to one composite per refresh period by `min_render_gap`, and a
   one-shot calloop timer re-tries when the window closes so a genuinely
   late frame can't sit stale until the next 1 Hz tick.

### CPU accounting — what was actually measured

Measured **in the container** (Docker Desktop LinuxKit VM, `rust:bookworm`,
`LIBGL_ALWAYS_SOFTWARE=1`, `weston --backend=headless-backend.so` at
1280×800 as the host), by sampling `utime+stime` from
`/proc/<pid>/stat` over a 6-second window:

| stack | idle, no client | one `foot` client |
|---|---|---|
| `duduclaw-comp` **winit** backend (unchanged behaviour) | **32.5%** | **53.8%** |

That is the "before" number and the reason A4-1 exists: the winit backend
composites unconditionally, so it burns a third of a core showing a static
picture, and the appliance pays that **on top of** `cage` doing the same
thing underneath it.

**The udev backend's own idle CPU is NOT measured in this round.** It cannot
be: the container has no `/dev/dri` (verified — `ls /dev/dri` → No such file;
`/sys/class/drm` contains only `version`) and the LinuxKit kernel ships no
`vkms` module (`modprobe vkms` → "module not found in modules.dep"), so
there is no DRM device to drive. Claiming a number here would be fabricated.
What *is* established in-container is the mechanism: `should_render` and
`min_render_gap` are unit-tested, and the "no screen change ⇒ no composite"
rule is a test (`nothing_dirty_means_no_work_at_all`), not a comment. The
real number has to come off the VM — steps below.

A winit-side "on-demand" switch was built as a way to measure the same
mechanism in-container, then **removed after measuring it**: winit re-fires
`RedrawRequested` immediately after `request_redraw()`, so skipping the
composite and re-arming is a hot spin — measured at **100% CPU with ~780 000
skipped frames per 5 s**, strictly worse than compositing. Making winit
on-demand properly needs `WinitGraphicsBackend` hoisted out of the event
source into `CalloopData` (the shape the udev backend uses), which would
rewrite the one code path every previous round's live verification covers,
for a backend that only ever runs nested in development. Not done; see the
note above `init_winit` in `src/winit_backend.rs`.

### Container verification (verified 2026-08-22)

Warm-cache container (`comp-a41`), volumes `duduclaw-comp-cargo`,
`duduclaw-comp-cargo-git`, `duduclaw-comp-target`.

```bash
docker volume create duduclaw-comp-cargo >/dev/null
docker volume create duduclaw-comp-cargo-git >/dev/null
docker volume create duduclaw-comp-target >/dev/null

docker run --rm \
  -v /Users/lizhixu/Project/DuDuClaw:/work \
  -v duduclaw-comp-cargo:/usr/local/cargo/registry \
  -v duduclaw-comp-cargo-git:/usr/local/cargo/git \
  -v duduclaw-comp-target:/target \
  -e CARGO_TARGET_DIR=/target \
  -w /work/crates/duduclaw-comp \
  rust:bookworm bash -c '
set -uo pipefail
apt-get update -qq
apt-get install -y -qq --no-install-recommends \
  pkg-config libwayland-dev libxkbcommon-dev \
  libinput-dev libudev-dev libseat-dev libgbm-dev libdrm-dev \
  libegl1 libgl1-mesa-dri libgles2 weston foot python3 procps >/dev/null

cargo build || exit 1
rustup component add clippy >/dev/null 2>&1
cargo clippy --all-targets -- -D warnings || exit 1
cargo test || exit 1
'
```

Results:

```
cargo build   -> Finished `dev` profile ... in 15.63s
cargo clippy --all-targets -- -D warnings -> Finished (no warnings)
cargo test    -> test result: ok. 129 passed; 0 failed; 0 ignored
```

**129 = the 104 pre-existing tests (all still green) + 25 new**: 10 in
`backend_choice`, 10 in `udev_backend` (`min_render_gap` × 4, `next_tick` × 2,
`should_render` × 4), 5 in `input` (`clamp_to`).

Live rounds in the same container:

1. **Backend selection matrix.**
   ```
   auto-tty     -> backend="udev"  wayland_display=None display=None
                   udev: libseat session acquired seat=seat0
                   Error: NoGpu("udev reports no DRM device on seat \"seat0\" ...")
   force-winit  -> backend="winit" (then winit's own honest
                   "neither WAYLAND_DISPLAY nor WAYLAND_SOCKET nor DISPLAY is set")
   typo (udv)   -> Error: "DUDUCLAW_COMP_BACKEND=\"udv\" is not a known backend ..."
   ```
   Note what this *does* prove beyond selection: **libseat really acquired a
   session** (`seat=seat0`) inside the container — libseat's builtin backend
   works as root without `seatd` — and udev enumeration really ran and
   correctly reported zero DRM devices. The session and discovery layers are
   exercised; DRM/GBM/EGL/libinput are not (there is no device).

2. **Nested winit regression (no capability lost).** weston headless →
   `duduclaw-comp` (auto-selected `winit`) → two `foot` clients:
   ```
   selected display backend backend="winit" wayland_display=Some("wayland-host")
   xdg client connected client_id=InnerClientId { id: 0, serial: 1 }
   xdg_shell: new toplevel created, mapping into space  surface_id=...@3[0]
   xdg_shell: sending initial configure to toplevel      surface_id=...@3[0]
   xdg client connected client_id=InnerClientId { id: 1, serial: 2 }
   xdg_shell: new toplevel created, mapping into space  surface_id=...@3[1]
   xdg_shell: sending initial configure to toplevel      surface_id=...@3[1]
   xdg_shell: toplevel destroyed, unmapping and reassigning focus (×2)
   ```
   `duduclaw-shell.sock` round trip over SO_PEERCRED, same uid:
   ```
   {"op":"list_windows"} ->
   {"ok":true,"windows":[{"app_id":"foot","title":"foot","focused":false},
                         {"app_id":"foot","title":"foot","focused":false}]}
   ```

3. **codrive injection regression** (the path that now also calls
   `queue_redraw`). Authenticated over `duduclaw-codrive.sock` with the
   token file, 9 seat/space-touching ops:
   ```
   auth      -> {"ok":true,"authenticated":true}
   status    -> {"ok":true,"frozen":false,"terminated":false,"takeover":false}
   move / highlight / button×2 / text / watch / shadow×2 / watch-off -> all {"ok":true,...}
   audit log: 9 × inject_applied (move, highlight, button, button, text,
                                  watch, shadow, shadow, watch)
   comp log:  focus: activation set target_surface_id=Some(...) activated_count=1 total_windows=1
   ```
   i.e. the agent seat still moves, clicks (with click-to-focus), types,
   highlights, and toggles shadow/watch exactly as before.

### VM verification — how to actually test the hardware backend

**Not run in this round** (per the task split: the VM belongs to the main
session). Steps, with expected output and where to look when it fails.

The appliance VM is QEMU aarch64 with `virtio-gpu-pci`. `cage` already runs
there, which is what makes this worth trying: DRM/KMS + GLES on
`virtio_gpu` is known-good in that environment.

**Step 0 — get the new binary onto the VM.** Build it in the container (the
command above) and copy `/target/debug/duduclaw-comp` (or a `--release`
build) into the image / onto the running VM, replacing whatever the kiosk
unit launches.

**Step 1 — stop the current kiosk stack.** The `cage` layer must not hold
DRM master:

```bash
systemctl stop duduclaw-kiosk.service
# confirm nothing else holds the card:
sudo fuser -v /dev/dri/card0      # expect: no output
```

**Step 2 — check the preconditions the backend needs.**

```bash
systemctl is-active seatd                       # expect: active
ls -l /dev/dri/                                 # expect: card0 (+ renderD128)
id duduclaw-kiosk                               # expect groups to include video, render
echo $XDG_RUNTIME_DIR                           # must be set, 0700, owned by the user
```

**Step 3 — run it by hand as the kiosk user, on a bare TTY (no
`WAYLAND_DISPLAY`).**

```bash
sudo -u duduclaw-kiosk env \
  XDG_RUNTIME_DIR=/run/duduclaw-kiosk \
  RUST_LOG=info,duduclaw_comp=debug \
  /usr/bin/duduclaw-comp
```

Expected on stdout, in this order:

```
duduclaw-comp: selected display backend backend="udev" wayland_display=None display=None
udev: libseat session acquired seat=seat0
udev: using DRM device /dev/dri/card0
udev: output created connector="Virtual-1" crtc=... mode=(1280, 800) refresh_mhz=60000 location=(0, 0)
udev: backend up — duduclaw-comp now owns the display directly (no cage layer) outputs=1 connectors=["Virtual-1"]
duduclaw-comp: udev backend ready seat=seat0
duduclaw-comp listening; socket_name=Some("wayland-1")
```

The screen should go to the dark grey clear colour (`0.1, 0.1, 0.1`) — the
same background the nested winit runs use.

**Failure triage, by which line is the last one printed:**

| Last line seen | Meaning | Where to look |
|---|---|---|
| `selected display backend backend="winit"` | `WAYLAND_DISPLAY`/`DISPLAY` leaked into the environment | `env` and look for either var; force with `DUDUCLAW_COMP_BACKEND=udev` |
| `Error: udev backend init failed (session): …` | libseat could not open a session | `systemctl status seatd`; user in `video`; on a non-logind system libseat needs `seatd` running and `SEATD_SOCK` reachable (default `/run/seatd.sock`) |
| `Error: udev backend init failed (no-gpu): …no DRM device…` | udev sees no card on this seat | `ls /dev/dri`; `udevadm info /dev/dri/card0` (check `ID_SEAT`); override with `DUDUCLAW_COMP_DRM_DEVICE=/dev/dri/card0` |
| `Error: udev backend init failed (open-device): …` | libseat refused the fd | usually `cage`/another compositor still holds DRM master — recheck step 1 |
| `Error: udev backend init failed (egl): …` | mesa can't make a GLES context on gbm | `LIBGL_ALWAYS_SOFTWARE=1` as a diagnostic (note the CD-0 round's finding: put it on the comp process, never on a parent kiosk process); check `libgl1-mesa-dri`/`libgbm1` are installed |
| `Error: udev backend init failed (no-output): …` | connectors exist but none connected, or no free CRTC | the `udev: connector not connected, skipping` / `no free CRTC` warn lines just above name each one |
| `udev: create_surface failed` / `GbmBufferedSurface::new failed` | mode/format negotiation | the error text names the connector; try forcing 8-bit only (already the default here) or a different mode |

**Step 4 — a real client.** From a second shell:

```bash
sudo -u duduclaw-kiosk env XDG_RUNTIME_DIR=/run/duduclaw-kiosk \
  WAYLAND_DISPLAY=wayland-1 foot
```

Expect `xdg client connected` + `xdg_shell: new toplevel created` in the comp
log, and the terminal visible on the physical/virtual screen.

**Step 5 — real input.** Move the mouse and type into `foot`.

- Pointer movement must move the pale cursor square. This is the arm that was
  **empty before this round** (`InputEvent::PointerMotion` — winit never
  emits it, libinput always does), so it is the single highest-risk new code
  path. If the pointer does not move, that arm is the first suspect.
- Click on a window: expect `focus: activation set … activated_count=1`.
- `Super+Tab`: expect `focus: Super+Tab cycling`.
- `Super+Esc`: expect the codrive emergency stop (`emergency_stop` audit
  event, agent cursor turning dimmed red).
- `Super+Enter`: expect `human_resume`. The CD-2 chord-tail fix
  (`is_system_gesture_tail`) matters here and has only ever been verified
  through `cage`, never through this backend's own libinput path.

**Step 6 — idle CPU (the number this work package owes).**

```bash
# with the compositor up and one idle foot window, from another shell:
pidstat -p $(pgrep -x duduclaw-comp) 1 10
# or, without sysstat:
P=$(pgrep -x duduclaw-comp)
A=$(awk '{print $14+$15}' /proc/$P/stat); sleep 10
B=$(awk '{print $14+$15}' /proc/$P/stat)
echo "cpu% = $(awk -v a=$A -v b=$B -v c=$(getconf CLK_TCK) 'BEGIN{printf "%.2f",(b-a)/c/10*100}')"
```

Expected: **near 0%** with a static screen (the design target is "one 1 Hz
timer wake-up and nothing else"). Compare against the 32.5% the winit backend
burns doing the same nothing, and against `cage`'s own ~100%. If it is *not*
near zero, the diagnostic is `RUST_LOG=…,duduclaw_comp::udev_backend=trace`:
`udev: no damage — skipping page flip` should be the dominant message, and a
flood of composites without that line means some `queue_redraw` call site is
firing continuously (the frame-callback loop described in point 5 of
"Repaint scheduling" is the likely culprit).

**Step 7 — session pause/activate (VT switch driven externally).**

```bash
sudo chvt 2   # expect: "udev: session paused (VT switched away) — dropping DRM master"
sudo chvt 1   # expect: "udev: session activated — reacquiring DRM master", screen restored
```

(There is **no** `Ctrl+Alt+F<n>` binding inside the compositor — see "not
implemented" below.)

**Step 8 — put it back.** `systemctl start duduclaw-kiosk.service`, or edit
the unit to drop the `cage` wrapper if the round succeeded.

### Deliberately not implemented (do not read the code as if these work)

- **Multi-GPU.** One GPU node is opened; a second GPU's connectors are
  ignored. anvil's `GpuManager`/`MultiRenderer` copy-between-GPUs path is not
  vendored.
- **Hotplug.** The udev event source is registered and logs every
  Added/Changed/Removed, but nothing rescans. Plugging a monitor in after
  startup will not create an output; unplugging leaves a dead surface until
  restart. A correct version needs `smithay-drm-extras`' `DrmScanner`.
- **DMA-BUF / linux-dmabuf-v1 / `wl_drm`.** Clients go through `wl_shm`
  exactly as on the winit backend; `bind_wl_display` is not called and no
  `DmabufState` global is advertised. GPU clients fall back to software
  buffers.
- **Hardware cursor / overlay planes / direct scanout.** Everything
  composites into the primary plane. The codrive cursors stay ordinary
  `SolidColorRenderElement`s.
- **VT switching from inside the compositor.** No `Ctrl+Alt+F<n>` binding.
  Two reasons: the kiosk unit is deliberately not a PAM/logind session
  (`appliance/postinst.d/20-users-and-units.sh`), so there is nothing to
  switch to; and adding a global chord means touching the human keyboard
  filter closure in `input.rs`, which carries the Super+Esc / Super+Enter
  codrive semantics this work package was told not to change. Session
  pause/activate *is* handled, so `chvt` from outside works.
- **VRR, 10-bit colour, explicit in-fences on the plane.** `ARGB8888` /
  `XRGB8888` only.
- **libinput device configuration** (tap-to-click, natural scroll, pointer
  acceleration profile). Defaults only.

### Bugs found and fixed while doing this

1. **`InputEvent::PointerMotion` was an empty arm.** Harmless under winit
   (which only emits `PointerMotionAbsolute`), fatal under libinput (which
   emits relative motion for every mouse/trackpad) — the pointer would never
   have moved on real hardware. Implemented as accumulate-delta-then-clamp.
2. **`PointerMotionAbsolute` was mapping through the *shadow* output.**
   `self.space.outputs().next()` returns insertion order, and the CD-2 shadow
   output is mapped first (in `DuduclawComp::new`, before any backend exists)
   at `codrive::SHADOW_ORIGIN` = `(0, 100_000)`. Every absolute pointer
   position was therefore being placed 100 000 px below every real window.
   This affected the **winit** path too, i.e. it is a pre-existing live bug,
   not something A4-1 introduced. Fixed via `DuduclawComp::primary_output()`,
   which skips the shadow output.

### Not verified (honest list)

- **Everything downstream of "no DRM device" is unverified in this round.**
  Container coverage stops at: backend selection ✓, libseat session
  acquisition ✓, udev GPU enumeration ✓ (correctly finding none). **Not**
  exercised anywhere yet: `DrmDevice::new`, connector/CRTC selection,
  `create_surface`, `GbmBufferedSurface`, EGL/GLES context creation on gbm,
  the vblank event loop, libinput event delivery, session pause/activate, and
  every line of `render_surface`. These compile and are modelled on anvil's
  own sequence, but nothing has run them.
- **The udev backend's idle CPU is unmeasured** — see "CPU accounting". The
  scheduling *rule* is unit-tested; the *number* is not measured.
- **Multi-output layout is untested even in principle** — the left-to-right
  `next_x` layout and the per-output codrive-overlay offset
  (`codrive_highlight_elements_at`) are written and compile, but no
  two-connector setup has been run.
- **No visual/screenshot verification** — same standing limitation as every
  previous round in this file. All claims above are log/audit evidence.
- **`wl_seat` is still named `"winit"`** on the hardware backend
  (`state.rs`'s `new_wl_seat(&dh, "winit")`). Cosmetic but wire-visible;
  left alone deliberately because the seat name is protocol surface and
  renaming it has non-zero regression risk for zero functional gain.
- **Not committed** — per this task's instructions, same as every prior round
  in this file.

---

## A4-5: `wl_seat` advertisement order — why `duduclaw-shell` had no keyboard (2026-08-22)

### The symptom

On the appliance VM, with `duduclaw-comp` on the udev/DRM backend owning the
hardware directly (no `cage`), `duduclaw-shell` received **no keyboard input
at all**: typing did nothing in the task box, `Super` did not open the
launcher. Everything around it looked healthy:

| Observation | Verdict |
|---|---|
| Pointer moves and clicking focuses windows | comp's `focus_window` runs; `shell_control`'s `list_windows` reports `focused: true` for the shell |
| `Super+Esc` triggers `codrive: EMERGENCY STOP — reason="super+esc"` | comp's own key path (`input.rs:38` filter closure) is alive |
| `foot -- sh -c 'cat > /tmp/keys.txt'` on the same comp receives `q`/`w`/`e` | comp really does deliver keys to clients |
| Shell with `WAYLAND_DEBUG=1`: zero `wl_keyboard.enter`, zero `wl_keyboard.key` | the shell is the odd one out |

### The root cause — it is a client-side (gpui) bug, not a comp bug

The decisive evidence is the shell's own protocol trace:

```
wl_seat#3.capabilities(3)
wl_seat#4.capabilities(3)
 -> wl_seat#3.get_keyboard(new id wl_keyboard#74)
 -> wl_seat#4.get_keyboard(new id wl_keyboard#76)
wl_keyboard#76.keymap(1, fd 23, 64754)
```

`duduclaw-comp` deliberately advertises **two** `wl_seat` globals — the human
seat (`"winit"`, `state.rs`) and the agent seat (`"duduclaw-agent"`,
`codrive/mod.rs:230`). That is legal Wayland and multi-seat-aware clients
cope (`foot`, above, is the proof).

gpui does not. Reading the pinned zed rev
(`7a7c3e1d2f03195c5fa19bc890da330ad7f3abef`),
`crates/gpui_linux/src/linux/wayland/client.rs`:

| Line | Code | Consequence |
|---|---|---|
| 309 | `wl_seat: wl_seat::WlSeat, // TODO: Multi seat support` | the client state has room for exactly one seat |
| ~717-725 | `"wl_seat" => { seat = Some(globals.registry().bind::<wl_seat::WlSeat,_,_>(…)); }` inside `globals.contents().with_list` | every seat is bound, but the local binding keeps only the **last** |
| 1651 / 1654 | `if let Some(wl_keyboard) = &state.wl_keyboard { wl_keyboard.release(); }` then `state.wl_keyboard = Some(keyboard);` | the second seat's `Capabilities` event **destroys** the first seat's keyboard |
| 1676 / 1679 | same shape for `wl_pointer` | ditto for the pointer |
| 1325 / 1328 / 1331 | a runtime `wl_registry.global` for `wl_seat` releases both devices and rebinds `state.wl_seat` | a seat appearing later also steals the slot |

So with the human seat advertised first and the agent seat second, the shell
ends up holding **only the agent seat's** keyboard and pointer. The human
seat's keyboard focus — which is what `focus_window` sets — has no client
resource left to deliver to, hence zero `enter`, zero `key`.

This also explains the anomaly in the trace that first pointed away from a
client bug: **`wl_keyboard#74` never receives a `keymap`.** It is not that
comp failed to initialise the human seat's keyboard — smithay sends the
keymap from `KeyboardHandle`'s `bind` path like it does for any seat. It is
that gpui *released* `#74` at line ~1652 moments after creating it, so the
object was already dead client-side. The surviving `#76` is the agent seat's.

Corollary worth knowing: **the shell's pointer input is broken the same way**.
The "mouse works" evidence above is all compositor-side (comp draws the
cursor and runs `focus_window` itself) — it does not show that the shell
received a single `wl_pointer.motion`. Expect in-shell clicking to have been
dead too, and to come back with this fix.

### Where the fix went, and why not the other side

The *correct* fix is upstream in gpui — make the seat state per-seat instead
of a single clobbered slot. That is not reachable from this repo cheaply:

- `gpui_linux` is consumed as a **git dependency** at a pinned rev, shared
  with `duduclaw-native-gui` (both crates must stay on the identical rev or
  the `gpui` types stop unifying — see `duduclaw-shell/Cargo.toml`'s header).
- Vendoring it as a `[patch]` **path** crate does not work: its manifest is
  workspace-inherited (31 `… .workspace = true` entries plus
  `edition`/`lints`), so a standalone copy would have to have every
  dependency edge re-pinned by hand against zed's workspace — 15k LOC and a
  re-vendor on every rev bump, to change three lines.
- The realistic upstream route is a **git fork of zed** plus a `[patch]`
  block; that needs a fork repo to exist, which is an operator decision (and
  a push), not something this work package can do. The exact patch is
  recorded below so the fork is a five-minute job when wanted.

Merging agent input back into the human seat is **not** an option — freeze,
emergency stop and the audit trail are all built on the agent's input
travelling through a structurally separate seat.

That leaves advertisement order, which is what changed:

| File | Change |
|---|---|
| `src/seat_order.rs` | **New.** `SeatAdvertiseOrder` + `from_env_value` (pure, unit-tested) + the full root-cause writeup as a module doc. |
| `src/state.rs` | `DuduclawComp::new` now creates the agent seat **before** the human seat by default; the human-seat constructor was lifted into a local closure so both orders share one body. |
| `src/main.rs` | `mod seat_order;`. |

Because gpui keeps the last seat it sees, advertising the agent seat first
lands it on the human seat. **Nothing about the codrive model changes**: two
separate seats either way, each with its own keyboard and pointer, freeze /
emergency stop / audit untouched.

### The tradeoff, stated plainly

This is a coin flip, not a cure, and it should be reverted the day gpui
learns multi-seat:

- A client that naively keeps the **first** seat (rather than the last) is
  now pushed onto the *agent* seat — the same bug mirrored. We accept it
  because every client that actually runs on the appliance except the shell
  is multi-seat-aware (`foot`, GTK/Qt, Chromium/Firefox), so that hazard is
  theoretical here while the last-seat-wins breakage is observed.
- The known cost: **a gpui client can no longer be driven by the agent**, because
  it releases the agent seat's keyboard/pointer. Human input to the shell is
  non-negotiable; agent-driving the shell's own UI is not a current
  requirement, and every other codrive target keeps both seats working.
- `DUDUCLAW_COMP_SEAT_ORDER=human-first` restores the old order in one step.

### The upstream patch (for when a zed fork exists)

Against `crates/gpui_linux/src/linux/wayland/client.rs` at rev `7a7c3e1d`.
Minimal, surgical — "first seat wins, ignore the rest", which is what
GTK/Qt/SDL do when they do not implement multi-seat, rather than a full
per-seat refactor:

```rust
// 1) in WaylandClient::new's registry walk (~line 717): keep the FIRST seat
//    and do not bind any other, so no second Capabilities event can arrive.
"wl_seat" => {
    if seat.is_none() {
        seat = Some(globals.registry().bind::<wl_seat::WlSeat, _, _>(
            global.name,
            wl_seat_version(global.version),
            &qh,
            (),
        ));
    }
}

// 2) in Dispatch<wl_seat::WlSeat> (~line 1629): ignore seats that are not
//    the adopted one, instead of clobbering the adopted seat's devices.
if seat != &state.wl_seat {
    return;
}

// 3) in Dispatch<wl_registry::WlRegistry> (~line 1323): a seat appearing at
//    runtime must not steal the slot from the seat already in use.
"wl_seat" => { /* already have one; ignore */ }
```

With that upstream, comp's order becomes irrelevant and
`DUDUCLAW_COMP_SEAT_ORDER` can be deleted.

### Verification

- Container build + `cargo test`: **156 passed, 0 failed** (`rust:bookworm`,
  the A4-1 system-dependency list). The five new tests are
  `seat_order::tests::*`.
- `duduclaw-shell` on macOS: **317 passed, 0 failed, 5 ignored** — unchanged,
  no shell file was touched by this work package. (Note: run it with rustup's
  `~/.cargo/bin/cargo`; the Homebrew `rustc` first on `PATH` is an
  x86_64 build that ignores `rust-toolchain.toml` and dies in `media`'s
  bindgen build script for want of an x86_64 `libclang`.)
- **Not verified on real hardware** — the VM is the operator's. See the
  live-check recipe in the handover notes: with `WAYLAND_DEBUG=1` on the
  shell, a fixed run must show `wl_keyboard#N.enter(...)` when the shell is
  focused and `wl_keyboard#N.key(...)` per keystroke, where `#N` is the
  keyboard obtained from the **second** `wl_seat` in the trace.
- **Not committed** — per this task's instructions, same as every prior round
  in this file.

## CUR-1: a real mouse cursor (2026-08-22)

Reported from a real appliance run: *"滑鼠是一個方塊，而非主流鼠標，而且還是白色
的誰看得到"*. Two defects stacked.

1. **Client cursor requests were silently discarded.**
   `SeatHandler::cursor_image` was an empty function
   (`src/handlers/mod.rs`, pre-CUR-1 line 32:
   `fn cursor_image(&mut self, _seat: &Seat<Self>, _image: CursorImageStatus) {}`).
   Every `wl_pointer.set_cursor` any client ever sent went nowhere: no I-beam
   over a text field, no hand over a link, no resize arrow on a window edge.
2. **What comp drew instead was a CD-0 placeholder.** `codrive/cursor.rs`
   drew the human pointer as a 10×10 `SolidColorRenderElement` at
   `HUMAN_COLOR = [0.95, 0.95, 0.95, 0.95]` — its own header called it a
   placeholder. White on the light OOBE background is invisible.

### What changed

| File | What |
|---|---|
| `src/cursor/mod.rs` | New. Human-pointer state machine + render elements. `CursorState` (`:75`), `set_human_cursor_image` (`:108`), `build_human_cursor_elements` (`:136`), `send_cursor_frames` (`:221`). |
| `src/cursor/theme.rs` | New. XCursor theme load + per-icon cache + negative cache + fallback. `CursorThemeStore::new` (`:99`), `cursor_for` (`:187`), `pick_image` size selection. |
| `src/cursor/source.rs` | New. The configuration seam: `CursorSource` (`:68`), `from_env_value` (`:117`), `resolve_theme_name` (`:153`), `resolve_size` (`:173`). |
| `src/cursor/fallback.rs` | New. Asset-free built-in arrow: `ARROW` mask (`:39`), `rasterize` (`:105`), `build_buffer` (`:141`). |
| `src/handlers/mod.rs` | `cursor_image` implemented (`:55`); `impl TabletSeatHandler` (`:80`); `delegate_cursor_shape!` (`:89`). |
| `src/state.rs` | `cursor_shape_manager_state` field (`:60`) + construction (`:178`); `cursor: CursorState` field (`:71`) + `from_env` (`:252`) + one boot log line. |
| `src/render.rs` | `CodriveElement` gains `Memory=` (`:46`) and `Surface=` (`:47`) variants. |
| `src/codrive/cursor.rs` | Human half deleted; `build_cursor_elements` → `build_agent_cursor_elements`. **The amber cross and its frozen dark-red variant are untouched** (DESIGN §3.3.2 "與人游標明確異形異色"). |
| `src/winit_backend.rs` / `src/udev_backend.rs` | Human elements built first (topmost), then the agent cross; `send_cursor_frames` after the per-window `send_frame` loop. |
| `src/codrive/debug_sim.rs` | New opt-in `simulate_pointer <x> <y>` command — the only way to move the HUMAN pointer in a headless container (nested weston advertises zero input devices). |
| `Cargo.toml` | `xcursor = "0.3"` promoted from a transitive dependency (already in `Cargo.lock` at 0.3.11 via `wayland-cursor`) to a direct one. No new crate in the tree. |

Drive-by (both pre-existing, both surfaced by the newer clippy 0.1.97 in the
current `rust:bookworm`, both needed to keep `-D warnings` green):
`src/seat_order.rs` manual `Default` impl → `#[derive(Default)]` +
`#[default]`; `src/codrive/window_geometry.rs:218` `- >1 left` →
`` - `>1` left `` (the bare `>` after a list marker parses as a blockquote).

### Both client protocols are served — and why

`wl_pointer.set_cursor` → `CursorImageStatus::Surface`;
`wp_cursor_shape_device_v1.set_shape` → `CursorImageStatus::Named`. Both land
in the same `cursor_image` handler.

Implementing only the first would have been *sufficient* for the shell: at the
pinned gpui rev, `crates/gpui_linux/src/linux/wayland/client.rs:1067`
`set_cursor_style` uses the cursor-shape device **if it bound one**, else
falls back to loading a theme itself and sending a surface. But
`cursor_shape_manager` there is `Option` (`:244`, `globals.bind(1..=1).ok()`),
so which branch it takes is *our* choice. Advertising the global is worth it:

* one theme, chosen by the compositor, for every client at once — `foot`,
  chromium and GTK apps otherwise each run their own loader with their own
  idea of `XCURSOR_THEME`;
* the brand-cursor seam below becomes a single compositor-side switch instead
  of something each client would have to honour;
* it costs one field, one empty trait impl and one `delegate_` macro, because
  smithay 0.7.0 already implements the protocol.

### Configuration seam for the brand cursor

The user's call was explicit: *"手繪爪形品牌游標這個我認為可以當設定中的替換，
正常還是用正常的游標就好了"*. So:

```
DUDUCLAW_COMP_CURSOR_SOURCE=system   # default — the machine's XCursor theme
DUDUCLAW_COMP_CURSOR_SOURCE=brand    # the "DuDuClaw" XCursor theme
DUDUCLAW_COMP_CURSOR_THEME=<name>    # explicit theme override, either source
XCURSOR_THEME / XCURSOR_SIZE         # freedesktop standards, honoured
```

`brand` is **fail-safe**: no such theme installed ⇒ one `WARN` line and a
fall back to the system theme. No brand artwork exists yet — that is a
separate design work package — and `brand` deliberately means "an ordinary
XCursor theme named `DuDuClaw`", so the art package needs **zero** new loading
code when it lands.

**Why an env var** (full reasoning in `src/cursor/source.rs`'s module doc):
comp has no config file and the brief forbids inventing one; every existing
comp tunable is an env var read at startup (`DUDUCLAW_COMP_SEAT_ORDER`,
`DUDUCLAW_COMP_BACKEND`, `DUDUCLAW_COMP_DRM_DEVICE`,
`DUDUCLAW_CODRIVE_WATCH_IDLE_SECS`); and `shell_control` is a control channel
with no persistence, so a socket op would *still* need a durable value outside
comp — i.e. this env var — plus a second mechanism on top.

**How a shell settings page wires to it later:** write the value into comp's
spawn environment and restart comp — mechanically identical to what an
operator does for `DUDUCLAW_COMP_SEAT_ORDER` today. If a future round wants it
live without a restart, the hook is already shaped: `from_env_value` is pure
and the live value is a plain field, so a `shell_control` `set_cursor_source`
op would only set the field, drop the theme cache and `queue_redraw()`. That
op is **not** implemented here — it is a live-reconfiguration feature with its
own auth/audit surface.

### The `pixels_rgba` trap (checked against sources, not assumed)

`xcursor::parser::Image::pixels_rgba` is **not** RGBA. The parser copies the
file's pixel block verbatim (`parse_img`: `take_bytes`, no reordering) and its
own doc concedes "(or, in the order of the file)". libXcursor writes each
pixel as an ARGB32 word through `_XcursorWriteUInt`, little-endian ⇒ the bytes
on disk are **B, G, R, A** — which is exactly DRM/wl_shm `ARGB8888`.
Confirmation from working code: `wayland-cursor` (the client-side consumer of
this same crate, used by winit/SCTK) writes `pixels_rgba` straight into an shm
buffer declared `Format::Argb8888` (`wayland-cursor-0.31.14/src/lib.rs` lines
367 and 384). The sibling `pixels_argb` is no better — it is derived assuming
RGBA input, so it comes out A,B,G,R.

So `theme.rs` uses `Fourcc::Argb8888`, while `fallback.rs` uses
`Fourcc::Abgr8888` for its own hand-authored R,G,B,A array. Two formats, each
correct for its own data; both are natively supported by `GlesRenderer`.

### Verification — container (`rust:bookworm`, A4-1 system dependency list)

```
cargo build                              -> Finished dev profile in 8.46s
cargo clippy --all-targets -- -D warnings -> Finished (exit 0, no warnings)
cargo test                               -> test result: ok. 182 passed; 0 failed
```

**182 = 156 pre-existing + 26 new** (9 `cursor::source`, 7 `cursor::fallback`,
7 `cursor::theme`, 3 `codrive::debug_sim`).

### Verification — live, nested weston (the legacy `set_cursor` path)

`weston --backend=headless-backend.so` → `duduclaw-comp` → `foot` 1.13.1
(which does **not** implement cursor-shape-v1, so it exercises the surface
path), driven with the new `simulate_pointer`:

```
cursor: XCursor theme loaded source="system" theme=Adwaita size=24
codrive: debug stdin — simulating human pointer motion x=10 y=10
cursor: human pointer image changed cursor=client-surface
codrive: debug stdin — simulating human pointer motion x=400 y=300
cursor: human pointer image changed cursor=default
cursor: human pointer image changed cursor=client-surface
```

Pre-CUR-1 not one of those `cursor:` lines could exist — the handler was empty.

### Verification — live, Xvfb + winit backend (pixel proof)

The winit backend also runs on X11, so `Xvfb :99` + `import -window root`
gives a real screenshot of comp's own composited output. (weston's headless
backend does advertise `weston_screenshooter`, but `weston-screenshooter`
hangs against it — that route is unavailable, X11 is.) Comp's window is
1280×800 at +0+0, so screen coordinates equal comp's logical coordinates.

**1. Default cursor from the Adwaita theme.** Human pointer moved to logical
(600, 400); the drawn image starts at screen (599, 399) — hotspot (1,1)
applied. ASCII rendering of the 22×28 region (`#` = bright, `X` = dark,
`.` = the `[0.1,0.1,0.1]` clear colour):

```
.X+X..................
.X#+X.................
.X##+X................
.X#X#+X...............
.X#XX#+X..............
.X#XXX#+X.............
.X#XXXX#+X............
.X#XXXX+#+X...........
.X#XXXXX+#XX..........
.X#XXXXXX+#XX.........
.X#XXXXXXX+#XX........
.X#XXXXXXXX+#XX.......
.X#XXXXX######XX......
.X#XXX+X+#XXXXXX......
.X#XX##XX#XXXXX.......
.X#X#+++X++X..........
.X##+XX#.X#XX.........
.X#+XXX++X#+X.........
.X+XX..X###XX.........
..XX...XXXXXX.........
........XXXX..........
```

An arrow, not a square: triangular head, diagonal right edge, notch, tail —
Adwaita's black-body/white-outline `default`.

**2. `cursor-shape-v1` end to end.** A throwaway 140-line C client (built
in-container from `wayland-scanner` output; **not** added to the repo) binds
`wp_cursor_shape_manager_v1`, keeps the *last* advertised seat exactly as gpui
does, and on `wl_pointer.enter` calls `set_shape(serial, TEXT)`:

```
client: bound wp_cursor_shape_manager_v1
client: seat name = duduclaw-agent
client: got wl_pointer (replacing any previous seat's)
client: seat name = winit
client: got wl_pointer (replacing any previous seat's)
client: requested TEXT cursor via cursor-shape-v1 (serial 3)
comp:   cursor: human pointer image changed cursor=text
```

and the pixels over the client's window (`.` = the client's blue):

```
...........#######+...........
...........#XX+XX#X...........
...........###X###X...........
...........+X#X#XX+...........
............+#X#X+............      <- 10 identical stem rows
...........###X###+...........
...........#XXXXX#X...........
...........#######X...........
```

An I-beam. The whole chain — modern protocol → smithay dispatch → our
`cursor_image` → theme lookup → render element — is live-proven.

**3. `wp_cursor_shape_manager_v1` really is advertised** (`wayland-info`):

```
interface: 'wp_cursor_shape_manager_v1',   version:  2, name:  7
```

**4. The asset-free fallback, pixel-exact.** With
`DUDUCLAW_COMP_CURSOR_THEME=duduclaw-no-such-theme-cur1`:

```
WARN cursor: no XCursor theme found (looked for 'duduclaw-no-such-theme-cur1' on
     XCURSOR_PATH / the default icon search path) — drawing the built-in outlined
     arrow instead. Install a cursor theme (e.g. the adwaita-icon-theme package)
     or set DUDUCLAW_COMP_CURSOR_THEME to a theme that exists.
DEBUG cursor: theme has no image for this icon — using the default cursor icon="default"
WARN  cursor: falling back to the built-in outlined arrow …
```

screenshot histogram:

```
1023789: (26,26,26) #1A1A1A   <- clear colour
    117: (250,250,249) #FAFAF9 <- stone-50 fill
     62: (28,25,23)   #1C1917  <- stone-900 outline
```

117 and 62 are the **exact** `#` and `X` cell counts of the `ARROW` mask in
`fallback.rs` — the arrow renders pixel-perfect, at the right colours, in the
right byte order (a red/blue swap would have read `(23,25,28)`).

**5. Brand fail-safe, garbage input, size.**

```
DUDUCLAW_COMP_CURSOR_SOURCE=brand    -> WARN cursor: brand cursor theme is not installed
                                            — falling back to the system theme
                                            requested_theme=DuDuClaw fell_back_to=Adwaita
DUDUCLAW_COMP_CURSOR_SOURCE=nonsense -> INFO cursor: XCursor theme loaded source="system"
                                            theme=Adwaita size=24
XCURSOR_SIZE=48                      -> INFO cursor: XCursor theme loaded … size=48
```

### Honest limitation list (this round)

- **Animated cursors show frame 0 only.** XCursor files store every frame of
  e.g. `wait`/`progress` at one nominal size; `pick_image` takes the first and
  ignores `delay`. Real animation needs a per-frame timer feeding
  `queue_redraw` — a scheduling change, deliberately not in a cursor-loading
  work package.
- **No scalable (SVG) cursors.** `xcursor` 0.3 can locate `cursors_scalable/`
  entries but leaves rendering to the caller; that means an SVG rasteriser.
  Raster themes are what the appliance image ships.
- **No HiDPI / fractional cursor scaling.** Everything renders at scale 1.0
  (`render_output(…, 1.0, …)` in both backends), so the cursor agrees with the
  rest of the screen. `XCURSOR_SIZE` still picks a bigger cursor.
- **No hardware cursor plane.** The pointer is composited into the primary
  plane, so a pointer move costs a full composite. `Kind::Cursor` is set so
  smithay's damage tracker knows, but the DRM cursor plane is untouched.
- **Colour-channel correctness is proven for the fallback only.** Adwaita's
  cursors are greyscale, so a red/blue swap would be invisible in evidence 1
  and 2; the `Argb8888` choice for theme images rests on the sourced reasoning
  above (libXcursor byte order + `wayland-cursor` doing the identical thing),
  not on a screenshot. A coloured cursor theme on the VM would close this.
- **The live (amber) agent cross was not observed in-container.** Under Xvfb
  the host pointer enters comp's window immediately, which counts as human
  input, so the agent seat is frozen from boot and its cross renders dark red
  (`#AA2323`, 28 px at the agent pointer's (0,0) home). That is pre-existing
  comp behaviour, unrelated to CUR-1; `codrive/cursor.rs`'s cross geometry and
  both colour constants are untouched by this round's diff.
- **`simulate_pointer` calls `on_human_input`** exactly as a real event does,
  so using it freezes the agent seat. That is intentional — a debug backdoor
  must not be a way around the codrive safety model.
- **Not verified on real hardware** (udev/DRM backend, real libinput pointer)
  — the VM is the operator's. Recipe below.
- **Not committed**, per this task's instructions, same as every prior round.

### How to check this on the appliance VM

1. Boot normally and look at the compositor log first:
   ```
   journalctl -u duduclaw-kiosk -b | grep -i cursor
   ```
   * Healthy: `cursor: XCursor theme loaded source="system" theme=Adwaita size=24`
   * Degraded but working: `cursor: no XCursor theme found … drawing the
     built-in outlined arrow instead` — you will see a white arrow with a dark
     outline; the theme package is missing.
2. Move the mouse. It must be an **arrow**, not a square. Over a text field
   (the shell's prompt bar, a `foot` window) it must become an **I-beam**;
   over a window edge, a resize arrow.
3. To watch the decisions live, restart comp with
   `RUST_LOG=info,duduclaw_comp::cursor=debug` and hover around — one
   `cursor: human pointer image changed cursor=<name>` line per change.

Triage table:

| Symptom | Most likely cause | Check / fix |
|---|---|---|
| Still a small white square | Old binary still running | `duduclaw-comp --version`-less build: compare the binary's mtime; the square only exists pre-CUR-1 |
| Arrow appears but never changes shape over text | Client is not requesting anything (very old toolkit), or the theme lacks `text`/`xterm` | `RUST_LOG=…cursor=debug`: no `image changed` line ⇒ the client never asked; a `text` line but no visual change ⇒ theme gap, see the `theme has no image for this icon` debug line |
| White-with-dark-outline arrow, never themed | No XCursor theme found | The `no XCursor theme found` WARN names the theme it looked for; install `adwaita-icon-theme` or set `DUDUCLAW_COMP_CURSOR_THEME` |
| Cursor is offset from where clicks land | Hotspot wrong for a client-provided surface | `cursor=client-surface` in the log ⇒ the client declared that hotspot; compare against another compositor before blaming comp |
| Cursor invisible only over one app | That app asked for `Hidden` | `cursor: human pointer image changed cursor=hidden` |
| Colours look wrong (red/blue swapped) on a **coloured** theme | The `Argb8888`/`Abgr8888` split in `theme.rs`/`fallback.rs` | Only the fallback's byte order is screenshot-proven; a coloured theme is the outstanding test |
| Brand cursor requested but nothing changes | No brand artwork exists yet | Expected — `DUDUCLAW_COMP_CURSOR_SOURCE=brand` logs `brand cursor theme is not installed` and uses the system theme |

---

## WM-1: third-party app windows covered the whole shell (2026-08-23)

### The report

From the appliance VM, verbatim:

> 開啟 Chromium 後上下方導航列直接不見，而且無法關閉應用程式

Two claims, and they have **different** causes. Both were checked first-hand
against a real Chromium (Debian bookworm `chromium` 151.0.7922.137) before
anything was written — see "First-hand Chromium finding" below, which does
**not** match the hypothesis this work package started from.

### Root cause 1 — every toplevel got the whole output at `(0, 0)`

A4's initial-configure fix (`handlers/xdg_shell.rs`) sized **every** toplevel
to the full output, and `new_toplevel` mapped every toplevel at `(0, 0)`. Its
own scope note said as much: *"When A5's multi-window desktop lands it owns
the layout policy"*. `duduclaw-shell` is itself one full-screen toplevel that
paints the menu bar and the dock inside its own window, so the first
third-party window to map covered the shell completely.

**Reproduced, pixel-exact, against the pristine `HEAD` binary** (built in the
container from `git archive HEAD`, so it is the real pre-WM-1 code, not a
reconstruction). `foot -a duduclaw-shell` with a green background as the shell
stand-in, then Chromium; screenshot of comp's composited 1280×800 output:

```
BASELINE (HEAD)                              WM-1
y=  0 (221,227,233) chromium frame           y=  0 (161,161,120) shell CSD titlebar
y= 29 (221,227,233) chromium frame           y= 29 (  0,127,  0) SHELL, visible
y= 30 (221,227,233) chromium frame           y= 30 (221,227,233) chromium frame starts
y=400 (255,255,255) chromium page            y=400 (255,255,255) chromium page
y=709 (255,255,255) chromium page            y=709 (255,255,255) chromium page
y=710 (255,255,255) chromium page            y=710 (  0,103,  0) SHELL, visible
y=799 (255,255,255) chromium page            y=799 (  0,170,  0) SHELL, visible
```

Baseline: **zero** green pixels anywhere on the screen — the shell is 100%
covered. WM-1: the top 30 rows and the bottom 90 rows are the shell, and the
Chromium window occupies exactly `(0, 30) 1280×680`.

### The fix — a reserved-band work area (`src/window_policy.rs`)

The transitional policy every mainstream desktop already uses for its own
chrome: a newly mapped (or maximized) application window gets the output
**minus** the bands the session chrome occupies. Windows' taskbar and the
macOS menu bar both work this way. This is deliberately **not** A5: no
layer-shell, no server-side decoration drawing, no task switcher UI.

Band heights come from `duduclaw-shell`'s own layout and are asserted in a
unit test so a shell layout change breaks the build rather than the desktop:

| band | shell source | value |
|---|---|---|
| top | `duduclaw-shell/src/home.rs` `menu_bar()` — `.absolute().top(0).left(0).right(0).h(px(30.))` | 30 |
| bottom | `duduclaw-shell/src/home/home_dock.rs` `dock()` — row `.absolute().bottom(px(24.))`, `TILE_DOCK_PX` 44 (`apps/icon_theme.rs`) + `.py(px(10.))`×2 + `.border_1()`×2 = 66 | 24+66 = 90 |

Overrides (env vars are this crate's only configuration mechanism):
`DUDUCLAW_COMP_RESERVED_TOP`, `DUDUCLAW_COMP_RESERVED_BOTTOM`,
`DUDUCLAW_COMP_SHELL_APP_ID`. Both band values are logged at startup.

Degenerate cases fall back to the whole output rather than a sliver: an output
shorter than `bands + MIN_APP_HEIGHT` (120) gets no reservation at all.

### Identifying the shell — and why the `app_id` route needs a fallback

`duduclaw-shell` now sets `app_id = "duduclaw-shell"`
(`crates/duduclaw-shell/src/main.rs`, `WindowOptions { app_id: … }`). gpui's
`WindowOptions` has carried an `app_id` field all along; it reaches
`xdg_toplevel.set_app_id` through `PlatformWindow::set_app_id`
(`gpui/src/window.rs:1802` → `gpui_linux/src/linux/wayland/window.rs:1604`),
verified in the pinned rev `7a7c3e1` that `duduclaw-shell/Cargo.toml` uses.

**But it arrives too late to size the shell.** `WaylandWindow::new` issues its
"kick things off" `surface.commit()` at
`gpui_linux/src/linux/wayland/window.rs:787`, and `set_app_id` is only called
after that platform window is constructed, at `gpui/src/window.rs:1801-1802`.
The initial configure — the one that decides the shell's size — therefore
necessarily runs on an identity-less toplevel. So comp uses **two** rules:

1. `app_id == "duduclaw-shell"` — authoritative, and supersedes a rule-2 guess.
2. Otherwise, if no shell has been identified yet, the first toplevel to reach
   the policy takes the role **provisionally**.

`XdgShellHandler::app_id_changed` (previously unimplemented; upstream's default
is a no-op) is where the identity finally arrives and either confirms the guess
or corrects it. A window declaring the shell `app_id` after the role is already
*confirmed* does not steal it — a shell's auxiliary toplevel must not become a
second full-screen window.

Both rules were live-verified separately (evidence below).

### Root cause 2 — no `zxdg_decoration_manager_v1`, and the honest finding

Comp advertised no decoration protocol, so a client had no negotiated answer
to "who draws the title bar". `XdgDecorationState` is now created in
`state.rs` and the handler in `handlers/xdg_shell.rs` answers **always**
`ClientSide` (comp draws no server-side decorations; inventing a decoration
renderer is A5's work package).

One trap worth recording: `ToplevelSurface::send_configure` sets
`initial_configure_sent` (smithay 0.7.0 `wayland/shell/xdg/mod.rs`), so the
smithay doc-example's `new_decoration` → `send_configure()` would consume the
initial configure and `handle_commit`'s sizing branch would **never run**. The
handler therefore only sends when the initial configure has already gone out;
both gpui and Chromium create their decoration object before their first
commit, so in practice one configure carries the size and the decoration mode.

**First-hand Chromium finding — the starting hypothesis was wrong.** The
premise for this half of the work package was "Chromium may not draw its own
title bar when there is no decoration negotiation". Measured, not assumed, by
running the same Chromium against the pristine `HEAD` binary and the WM-1
binary and diffing the pixels of the window-control area:

```
BASELINE (no decoration global): chromium never touched xdg-decoration
WM-1: wl_registry.bind(zxdg_decoration_manager_v1)
      zxdg_decoration_manager_v1.get_toplevel_decoration(...)
   -> zxdg_toplevel_decoration_v1.set_mode(1)      # 1 = client_side; chromium ASKS for CSD
   <- zxdg_toplevel_decoration_v1.configure(1)     # comp agrees

top-right 220×36 of the chromium window, both builds:
  non-background pixels = 183   (identical)
  colour histogram      = identical
  glyph map             = identical:  ─   □   ✕
```

So **Chromium 151 draws its own minimize/maximize/close controls with or
without the protocol**. The decoration protocol is therefore *not* proven to
be the cause of "無法關閉應用程式"; the proven cause is root cause 1 (the window
covering the dock and the menu bar, leaving no way back to the desktop).
Advertising the global is still correct — it closes a real protocol gap for
toolkits that do honour it, and it makes the CSD contract explicit instead of
implicit — but it is recorded here as a gap closed, **not** as the fix for the
reported symptom.

### Super+Q — a compositor-level close, independent of the client

Added to the same human-only keyboard filter closure in `input.rs` that
already carries Super+Esc / Super+Enter / Super+Tab, so an injected agent key
event structurally cannot forge it. Sends `xdg_toplevel.close` — a polite
request, not a kill — to the focused window; resolves through a popup's root
surface when a menu holds focus. **The session shell is always refused**
(closing it leaves a black screen). Both `q` and `Q` are accepted, so Caps Lock
does not silently disable the only compositor-level close gesture.

### Also: `maximize_request` now means the work area

Upstream's default is `surface.send_configure()` with no state change, i.e. a
Chromium/GTK maximize button was inert. It now sets `xdg_toplevel::State::
Maximized` and configures to the work area — which is the entire point of
calling the reserved bands a work area. `unmaximize_request` clears the state;
comp keeps no restore geometry (that is A5's state to own) so it does not
invent a previous size. The **initial** configure still deliberately does not
set `Maximized` — A4's reasoning stands (it changes CSD for every GTK/Qt app,
and is only appropriate when the client itself asked).

### Verification — container

```
cargo build                               -> Finished, zero warnings
cargo clippy --all-targets -- -D warnings -> Finished, zero warnings
cargo test                                -> test result: ok. 226 passed; 0 failed
```

**226 = 212 pre-existing (all still green) + 14 new**: 12 in `window_policy`
(band arithmetic, degenerate outputs, per-band env parsing, the shell-`app_id`
override, and an assertion pinning the band values to the shell's real
layout), 2 in `input` (the Super+Q keysym predicate).

### Verification — live, Xvfb + winit backend (pixel proof)

Same `Xvfb :99` + `import -window root` recipe as the CUR-1 round. Extra apt
packages this round needed beyond the A4-1 list: `libxcursor1 libxrandr2
libxi6 libxinerama1 libx11-xcb1 libxkbcommon-x11-0 libwayland-egl1
libwayland-cursor0 xvfb x11-utils xdotool imagemagick python3-pil
wayland-utils` (winit's X11 backend fails at startup with
`XNotSupported(libXcursor.so.1)` without the first group).

**1. Reserved bands, real clients, real pixels.** `foot -a duduclaw-shell`
(green) + `foot -a app-B` (red):

```
window_policy: session shell identified by app_id … app_id=duduclaw-shell demoted=None
window_policy: applied … is_shell=true  in_shadow=false rect=(0, 0, 1280, 800)
window_policy: applied … is_shell=false in_shadow=false rect=(0, 30, 1280, 680)
xdg_shell: sending initial configure … configured_size=1280x680 location=(0, 30)
```

screenshot rows (`.` sampled every 4 px across 1280):

```
y=  0..25  (161,161,120)  shell's own CSD titlebar
y=  29     (0,170,0)      SHELL — visible above the app
y=  30,31  (161,161,120)  app-B's CSD titlebar, starting exactly at the band edge
y= 100..709 (204,0,0)     app-B
y= 710..799 (0,170,0)     SHELL — visible below the app (90 rows)
```

**2. Rule 2 (first-mapped fallback) and promotion/demotion.** First client
`foot -a not-the-shell`, then `foot -a duduclaw-shell`:

```
window_policy: no shell identified yet — treating the first mapped toplevel as the session shell (provisional)
window_policy: applied … surface@3[0] is_shell=true  rect=(0, 0, 1280, 800)
window_policy: session shell identified by app_id (superseding the first-mapped guess) … demoted=Some(surface@3[0])
window_policy: applied … surface@3[0] is_shell=false rect=(0, 30, 1280, 680)   <- demoted
window_policy: applied … surface@3[1] is_shell=true  rect=(0, 0, 1280, 800)    <- promoted
```

screenshot confirms it: `y=400` blue (`not-the-shell`, now in the work area),
`y=711`/`795` green (the real shell, visible in the bottom band).

**3. Super+Q**, driven with real XTEST key events (`xdotool key
--clearmodifiers super+q` into the winit X window), focus set deterministically
through the `shell_control` socket:

```
{"ok":true,"matched_app_id":"app-B"}
window_policy: Super+Q — sending xdg_toplevel.close to the focused window … app_id=Some("app-B")
xdg_shell: toplevel destroyed, unmapping and reassigning focus
  foot app-B still running? -> <gone>
  foot shell still running? -> alive

{"ok":true,"matched_app_id":"duduclaw-shell"}
WARN window_policy: Super+Q refused — that window is the session shell (closing it would leave a black screen)
  foot shell still running? -> alive
```

**4. `zxdg_decoration_manager_v1` is really advertised** (`wayland-info`):

```
interface: 'zxdg_decoration_manager_v1',   version:  1, name:  8
```

**5. B3 `window_geometry` still answers correctly for a non-`(0,0)` window.**
The op reports `Space::element_location`, which is exactly the value the
policy now changes — this is the question B3 was designed to answer, so it had
to be re-confirmed rather than assumed. Same authenticated codrive client:

```
geom shell : {'ok': True, 'window': {'origin_x': 0, 'origin_y':  0, 'width': 1280, 'height': 800, ...}}
geom app-B : {'ok': True, 'window': {'origin_x': 0, 'origin_y': 30, 'width': 1280, 'height': 680, ...}}
```

The agent's `global = origin + atspi_window_offset` arithmetic is unchanged;
only the origin it is handed is no longer always zero. Injected pointer
coordinates are unaffected either way — they are global logical coordinates
and `Space::element_under` has always done the element-location mapping.

**6. CD-2 shadow workspace — no regression.** Real authenticated codrive
client (token from `$XDG_RUNTIME_DIR/duduclaw-codrive.token`), agent focuses
`app-B` and pushes it into the shadow workspace:

```
auth   : {'ok': True, 'authenticated': True}
shadow : {'ok': True, 'frozen': False}
audit  : shadow_window_moved(to_shadow) -> shadow_enabled -> inject_applied(op:shadow)
```

and **no further `window_policy: applied` line after the shadow op** — nothing
pulls a shadow window back onto the main output. `grep -c 'panic|failed to
(allocate|bind|render) the shadow'` → `0`. (`apply_window_policy` returns
immediately for an already-configured window inside the shadow output's
bounds, and a shadow window can never claim the session-shell role.)

### Honest stub / limitation list (this round)

- **Not verified on real hardware** (udev/DRM backend, real libinput keyboard
  for Super+Q) — the VM is the operator's. Recipe below.
- **The shell-side `app_id` change is compile-verified but not run.**
  `cargo check` on `crates/duduclaw-shell` is green
  (`Checking duduclaw-shell v1.62.0 … Finished dev profile in 23.69s`). It has
  not been *run* on the appliance — step 1 of the VM recipe below is what
  confirms the `app_id` actually reaches comp.
  Environment note for anyone repeating this: a Homebrew Intel rust in
  `/usr/local/bin` shadows the rustup toolchain and produces two confusing
  failures before duduclaw-shell's own code is ever reached — `media`'s
  build script dying on `libclang … incompatible architecture`, then `gpui`
  failing on `use of unstable library feature 'cold_path'` (exactly what
  `rust-toolchain.toml`'s own comment warns about). Run with
  `PATH="$HOME/.cargo/bin:$PATH"`.
- **Interactive resize is not constrained.** Dragging an app window's own CSD
  resize edge can still make it overlap the bands. Constraining interactive
  window management is A5's job; this round only owns placement at
  configure/maximize time.
- **`fullscreen_request` is deliberately still unimplemented** (upstream's
  no-op default). A fullscreen video legitimately wants to cover the bands, and
  deciding that is A5's call.
- **A client that ignores its configure is not forced.** Comp does not crop or
  clip; xdg-shell configure is the only lever, which is the same lever every
  compositor has.
- **udev/DRM output changes are not re-driven.** `reapply_window_policy_all`
  is wired to the winit backend's `Resized` event; the udev backend builds its
  outputs once during `init_udev`, before any client can connect, and has no
  mode-change or hotplug path today. If one is added, it must call that method.
- **The band values assume scale 1.0**, which is what both backends composite
  at (`render_output(…, 1.0, …)`). A HiDPI shell would need them scaled.
- **Not committed**, per this task's instructions, same as every prior round.

### How to check this on the appliance VM

1. Boot normally; confirm the policy is live and reading the right numbers:
   ```
   journalctl -u duduclaw-kiosk -b | grep -E 'window layout policy|window_policy'
   ```
   Expect `reserved_top=30 reserved_bottom=90 shell_app_id=duduclaw-shell`,
   then `session shell identified by app_id` (or, if the shell binary was not
   rebuilt with the `app_id` change, `treating the first mapped toplevel as the
   session shell (provisional)` — both give the shell the full output).
2. Open Chromium from the Launcher. **Expected:** the menu bar is still visible
   as a 30 px strip along the top and the dock still floats in the bottom 90 px;
   the Chromium window sits between them. Before this change the whole screen
   was Chromium.
3. **Click a dock tile.** It must still respond and switch/raise windows — the
   dock's running-window indicator and focus switching (APP-1) are unchanged;
   the only reason they were unusable was that the dock was covered.
4. **Press Super+Q** with Chromium focused: the window closes (Chromium may
   first ask to confirm — that is its choice, `xdg_toplevel.close` is a
   request). Press Super+Q again with only the shell on screen: **nothing must
   happen**, and `journalctl` shows
   `Super+Q refused — that window is the session shell`.
5. Chromium's own ✕ / maximize / minimize buttons should also work; clicking
   maximize must fill the **work area**, i.e. still leaving the menu bar and
   the dock visible, not the whole screen.

Triage table:

| Symptom | Most likely cause | Check / fix |
|---|---|---|
| App window still covers everything | Old comp binary, or the shell is not the first/only `app_id` claimant | `grep 'window_policy: applied'` — the `rect=` on the app window tells you exactly what it was given |
| The **shell** got squeezed into the work area | Something else mapped a toplevel before the shell and then the shell never sent `app_id` | `grep 'session shell'` — a `provisional` line naming a surface that is not the shell is the smoking gun; rebuild the shell with the `app_id` change |
| Bands are the wrong height | The shell's menu bar / dock layout changed | Update the table above **and** `window_policy.rs`'s constants; the unit test `the_default_bands_are_the_shells_real_menu_bar_and_dock` is there to force this |
| Need to tune without a rebuild | — | `DUDUCLAW_COMP_RESERVED_TOP` / `DUDUCLAW_COMP_RESERVED_BOTTOM` in comp's environment, then restart comp |
| Super+Q does nothing | No keyboard focus (comp only focuses on click), or the focused surface is not a mapped toplevel | The log says which: `Super+Q with no keyboard focus` vs `focused surface is not a mapped toplevel` |
| Super+Q closes nothing on one specific app | The client ignored `xdg_toplevel.close` | Expected; it is a request. Nothing in comp kills processes |

## CUR-3: cursor SIZE as a live, persisted, socket-driven setting (2026-08-23)

The third and last axis of the human pointer's appearance. CUR-1 gave it real
artwork, CUR-2 gave the *source* a live switch; this round does the same for
**size**, for the shell's 協助工具 › 指向與點按 page (five segment buttons:
**24 / 32 / 48 / 64 / 96**). Before this, size was `XCURSOR_SIZE`-only —
operator-facing, and read exactly once at startup.

### Wire contract (the shell was written against this — do not change the shape)

```text
-> {"op":"get_cursor_source"}
<- {"ok":true,"cursor":{"source":"system","requested":"system","theme":"Adwaita",
                        "origin":"default","size":24,"effective_size":24,
                        "size_env_pinned":false,"env_pinned":false}}

-> {"op":"set_cursor_size","params":{"size":32}}
<- {"ok":true,"cursor":{…,"size":32,"effective_size":32,"persisted":true}}

-> {"op":"set_cursor_size","params":{"size":40}}
<- {"ok":false,"error":"invalid_cursor_size"}
```

`get_cursor_source` keeps its CUR-2 name (renaming it to `get_cursor_config`
would break shipped callers for cosmetics); the reply is purely additive, so a
CUR-2-era client ignores the new keys.

### Two size gates, deliberately different

| Surface | Accepts | Where |
|---|---|---|
| `XCURSOR_SIZE` (OPERATOR) | any integer, clamped 8–512 | `source::resolve_size` |
| `set_cursor_size` op + `cursor.json` (UI) | exactly 24/32/48/64/96 | `source::cursor_size_from_wire` |

The env var is the operator's machine-level channel — an operator wanting a
40 px pointer for one panel has a reason no settings UI can anticipate. The op
is the *UI's* channel, and accepting a sixth value would immediately produce
the failure `CursorSource::parse_strict`'s doc already warns about: a settings
page with no button matching the stored value. Lenient parser guards boot,
strict parser guards the control channel — the same split CUR-2 made for the
source enum, applied to a number. The stored `size` key is held to the strict
rule because the only writer of it is the strict op.

Priority is CUR-2's, one level down (`source::resolve_startup_size`):
`XCURSOR_SIZE` > `cursor.json` > 24. A present-but-garbage `XCURSOR_SIZE`
still counts as "the operator spoke" and lands on 24 rather than falling
through to a stored preference.

### `size` vs `effective_size` — the honest-reporting field

**A 96 px request against a theme whose largest image is 64 draws a 64 px
cursor, at 64 px. Nothing is upscaled.** Traced through the code and then
confirmed on real pixels (evidence 5 below):

- `theme::pick_image` chooses the image whose **nominal** size is nearest the
  request (ties to the larger).
- `build_from_images` wraps that image at its own `(width, height)` with
  buffer scale 1.
- `cursor/mod.rs` calls `MemoryRenderBufferRenderElement::from_buffer(…,
  src: None, size: None, …)`, and smithay 0.7.0 resolves that to
  `inner.mem.size().to_logical(scale, transform)` — the buffer's own pixel
  dimensions (read in `element/memory.rs`, not assumed).

So the third-largest possibility — "64 image stretched to 96" — does not
happen. Forcing it *is* possible (pass `size: Some(96)`) and was **rejected**:
it bilinear-stretches a 64 px bitmap, which is the "upscaled mush"
`pick_image`'s own tie-break comment already turns down and which would wreck
the dark outline CUR-1 exists to keep legible. An honestly-64 px cursor beats a
nominally-96 px unreadable one.

What must not happen is the *silent* version — reporting 96 while drawing 64.
Hence `effective_size` on the wire, `CursorThemeStore::effective_size()`
behind it (answered through the same `cursor_for` path that draws the frame,
so there is no second size-selection rule to drift), and an `INFO` line at
switch time naming both numbers.

**On the appliance's own theme the two are always equal at every step.**
Checked rather than assumed, by reading the `Xcur` table of contents of
Debian `adwaita-icon-theme`'s cursor files: nominal sizes `[24, 32, 48, 64, 96]`
with matching `width`/`height` — *exactly* `CURSOR_SIZE_STEPS`. A divergence in
the field therefore means a sparse third-party theme or no theme at all, which
is worth surfacing. The asset-free fallback arrow also quantises (integer scale
of a 24-cell mask), so **32 draws 24 and 64 draws 48** there — reported the same
honest way via `fallback::rasterized_height`.

### What changed

- **`cursor/source.rs`**: `CURSOR_SIZE_STEPS`, `cursor_size_from_wire(i64)`
  (strict, refuses rather than clamps), `resolve_startup_size`, `env_pins_size`.
- **`cursor/persist.rs`**: `CursorPrefs` gains `size: Option<i64>`; `load_size`;
  `store_size`. **Writes became read-modify-write** — with two keys, the old
  unconditional overwrite was a live data-loss bug (`store(source)` would have
  erased a stored `size`, and vice versa). The read half keeps RAW field values,
  so a hand-edited `{"source":"claw"}` survives a size write instead of being
  silently deleted. Honest limitation, pinned by a test: an unknown top-level
  *key* is still dropped (no `#[serde(flatten)]` catch-all).
- **`cursor/theme.rs`**: `LoadedCursor.nominal_size`; `size()`,
  `effective_size()`, `set_size()`. A size switch drops **both** caches — the
  per-icon `cache` *and* the separately-cached `fallback` arrow. Missing the
  second one would make the whole op a no-op on a themeless machine, which is
  precisely the container case. It deliberately does **not** reload the theme:
  `resolve_theme_name` takes no size, and the loaded `xcursor::CursorTheme` is a
  path resolver, not a rasterised image set.
- **`cursor/fallback.rs`**: `rasterized_height(size)`, derived from the same
  `scale_for_size` the rasteriser uses (a hand-maintained table would drift).
- **`cursor/mod.rs`**: `CursorSourceInfo` gains `size` / `effective_size` /
  `size_env_pinned`; `cursor_source_info` became `&mut self`;
  `DuduclawComp::set_cursor_size`.
- **`shell_control/{protocol,listener,mod}.rs`**: the `SetCursorSize { size:
  i64 }` op, its `invalid_cursor_size` validation, and the audited handler.

`size` is typed `i64` on the wire so `{"size":-5}` answers `invalid_cursor_size`
— an honest statement about the *value* — instead of `parse_error`, which would
blame the JSON. `{"size":3.5}` and `{"size":"32"}` stay `parse_error`: those
genuinely are schema violations, not out-of-range sizes.

There is deliberately **no** `size_origin` string to match `origin` (which
describes the source only). The one thing a UI must not get wrong is "will my
choice stick?", and `size_env_pinned` answers it.

### Build / clippy / test (verified 2026-08-23)

Same one-shot container shape as the CD-0 round, plus A4-1's system deps
(`libinput-dev libudev-dev libseat-dev libgbm-dev libdrm-dev` — without
`libseat` the test binary dies at load with `libseat.so.1: cannot open shared
object file`, which looks like a test failure and is not).

```
cargo build                                -> Finished in 8.19s, zero warnings
cargo test                                 -> 253 passed; 0 failed   (baseline 226, +27)
cargo clippy --all-targets -- -D warnings  -> Finished, zero warnings
```

### Live verification — Xvfb + winit backend (pixel proof)

Same `Xvfb :99` + `import -window root` recipe as CUR-1/WM-1. Bounding boxes
are of every pixel differing from comp's `(26,26,26)` clear colour, measured in
a 180×160 crop around the pointer at logical (600, 400), so the agent seat's
cross at (0,0) is out of frame.

**1. Adwaita, live switch, no restart.** Human pointer at (600, 400):

```
get_cursor_source -> "size":24,"effective_size":24,"origin":"default"
                     bbox 15x21, 202 px
set_cursor_size 32 -> "size":32,"effective_size":32,"persisted":true
                     bbox 19x29, 345 px
set_cursor_size 96 -> "size":96,"effective_size":96,"persisted":true
                     bbox 54x84, 2906 px
set_cursor_size 24 -> bbox 15x21, 202 px   (byte-identical to the start)
```

The pointer really grows and really shrinks back; 84/21 = 4.0 exactly across
the 24→96 step. (The box is the arrow's opaque shape, not the nominal square —
real theme art is not a pure scale of itself, which is why the widths are not
in the same exact ratio.)

**2. Refusals, not clamps.**

```
size=40      -> {"ok":false,"error":"invalid_cursor_size"}
size=0       -> {"ok":false,"error":"invalid_cursor_size"}
size=-5      -> {"ok":false,"error":"invalid_cursor_size"}
size=512     -> {"ok":false,"error":"invalid_cursor_size"}
size=100000  -> {"ok":false,"error":"invalid_cursor_size"}
size=3.5     -> {"ok":false,"error":"parse_error"}        (documented split)
```

Re-asserting the live size is a no-op: 96 sent twice audits `changed=true` then
`changed=false`, and `grep -c 'size switched live'` counts 4 for the four real
changes above, not 5.

**3. Persistence, and the two keys not evicting each other.**

```
after the switches      : cursor.json = {"size":96}
restart comp, get       : "size":96 — bbox 54x84 (identical to the live 96)
then set_cursor_source  : cursor.json = {"source":"brand","size":96}
```

That last line is the regression guard for the read-modify-write change, proven
live: the source write preserved the size key.

**4. `XCURSOR_SIZE` outranks the file, and says so.** File says 96:

```
XCURSOR_SIZE=48 -> "size":48,"size_env_pinned":true   bbox 28x43
XCURSOR_SIZE=40 -> "size":40,"effective_size":48
```

The second line is an incidental bonus proof: 40 is not a step, the operator
gets it anyway, and Adwaita's nearest image (48, tie-break to larger) is
reported honestly rather than as 40. A pre-CUR-3 `{"source":"brand"}` file
loads unchanged and yields `"size":24,"requested":"brand"`.

**5. The 96-on-a-64-max-theme question, on real pixels.** A synthetic XCursor
theme carrying **only** 24 and 64 px images, each a solid opaque white square
so the drawn bounding box *is* the chosen image (built in-container by a
throwaway script; **not** added to the repo):

```
set 24 -> "size":24,"effective_size":24   bbox 24x24, 576 px
set 64 -> "size":64,"effective_size":64   bbox 64x64, 4096 px
set 96 -> "size":96,"effective_size":64   bbox 64x64, 4096 px   <-- THE ANSWER
```

Pixel-identical to the 64 case. The 64 image is drawn at its own 64 px — not
stretched to 96, and not silently reported as 96. comp's own log for it:

```
INFO cursor: size switched live, but the loaded cursor theme has no image at that
     size — its nearest image is drawn at its own size, nothing is upscaled
     size=96 effective_size=64 theme=SparseTheme
```

**6. No theme at all — the built-in arrow's quantisation, reported honestly.**

```
size=24 effective_size=24   bbox 15x24,  179 px
size=32 effective_size=24   bbox 15x24,  179 px   <- identical: 32 draws 24
size=48 effective_size=48   bbox 30x48,  716 px
size=64 effective_size=48   bbox 30x48,  716 px   <- identical: 64 draws 48
size=96 effective_size=96   bbox 60x96, 2864 px
```

Every box is exactly `15×scale` by `24×scale` for the `ARROW` mask's own
15×24 cells, and 179 px at scale 1 is exactly CUR-1's `117 + 62` fill/outline
cell counts. Two of the five steps genuinely draw smaller than they ask for —
which is the whole reason `effective_size` exists.

**7. Audit trail**, `duduclaw-shell-control-audit.jsonl`, actions only (queries
still unaudited, per this module's standing rule):

```
{"ts_ms":…,"kind":"set_cursor_size","detail":"size=32 effective_size=32 theme=\"Adwaita\" changed=true persisted=true"}
{"ts_ms":…,"kind":"set_cursor_size","detail":"size=96 effective_size=96 theme=\"Adwaita\" changed=false persisted=true"}
{"ts_ms":…,"kind":"set_cursor_size","detail":"size=96 effective_size=64 theme=\"SparseTheme\" changed=true persisted=true"}
{"ts_ms":…,"kind":"set_cursor_size","detail":"size=64 effective_size=48 theme=\"duduclaw-no-such-theme-cur3\" changed=true persisted=true"}
```

`effective_size` is in the line on purpose: "the user asked for 96 and the theme
drew 64" must be recoverable from the trail, not only from a live socket query.

### Honest stub / limitation list (this round)

- **Not verified on real hardware** (udev/DRM backend, real libinput pointer,
  the appliance's own Adwaita install) — the VM is the operator's. Recipe below.
- **Not verified against the shell's settings page** — the UI half is a
  concurrent work package in `crates/duduclaw-shell`; this round verified the
  op with a raw socket client only.
- **`effective_size` is measured on `CursorIcon::Default`.** A theme could in
  principle ship different size sets per icon; none in practice does, and this
  is the one icon a theme is effectively guaranteed to have (the same probe
  `load_theme` already uses). A per-icon divergence would go unreported.
- **An unknown top-level key in `cursor.json` is dropped on write**, not
  round-tripped — see "What changed". Pinned by a test so it is a decision, not
  a surprise.
- **No HiDPI / fractional scaling**, unchanged from CUR-1: everything composites
  at scale 1.0, so these are physical pixels.
- **The `size` field in the startup `XCursor theme loaded` INFO line is the
  boot-time value** and does not update on a live switch; the per-switch INFO
  line in `cursor/mod.rs` is the live one.
- **Not committed**, per this task's instructions, same as every prior round.

### How to check this on the appliance VM

1. Boot normally and confirm the startup resolution:
   ```
   journalctl -u duduclaw-kiosk -b | grep -E 'cursor: (resolved|XCursor theme loaded)'
   ```
   Expect `size=24 size_origin=default` on a fresh machine.
2. Drive the op directly, as the kiosk user (the socket is same-uid only).
   `python3` is in the image's `mkosi.conf` `Packages=` and was confirmed
   present by the CD-0 VM round, so use it rather than assuming `nc -U`/`socat`
   are installed:
   ```
   python3 - <<'EOF'
   import json, os, socket
   s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
   s.connect(os.environ["XDG_RUNTIME_DIR"] + "/duduclaw-shell.sock")
   s.sendall(json.dumps({"op": "set_cursor_size", "params": {"size": 96}}).encode() + b"\n")
   print(s.recv(4096).decode())
   EOF
   ```
   (swap the body for `{"op":"get_cursor_source"}` to read the current state —
   one request per connection, per this socket's protocol.)
   **Expected:** the pointer grows *immediately*, with no restart and without
   having to move the mouse first (that last part is what `queue_redraw` is
   for — on the udev backend nothing else would schedule a frame).
3. Check `effective_size` in the reply equals `size` at all five steps. On the
   appliance's Adwaita it must; if it does not, the image is shipping a
   different cursor theme than expected — `grep 'XCursor theme loaded'` names it.
4. Restart the compositor and confirm the size stuck:
   ```
   cat "$HOME/.local/state/duduclaw-comp/cursor.json"   # -> {"size":96}
   ```
5. Refusal check: `{"size":40}` must answer `invalid_cursor_size` and the
   pointer must not change.

Triage table:

| Symptom | Most likely cause | Check / fix |
|---|---|---|
| Op returns ok but the pointer does not change | The value equalled the live size (`changed=false` in the audit line), or an old comp binary | `grep 'cursor: size switched live'` — absent means no change was applied |
| Pointer only changes after moving the mouse | A `queue_redraw` was lost | Should be impossible; `set_cursor_size` calls it unconditionally on a real change |
| `effective_size` < `size` | The loaded theme has no image at that size — correct, not a bug | `grep 'no image at that size'` names the theme; install a fuller one (`adwaita-icon-theme`) or pick a step it carries |
| `effective_size` is 24 or 48 no matter what | No XCursor theme found at all — the built-in arrow is drawing | `grep 'no XCursor theme found'`; install a cursor theme |
| Size reverts after every restart | `XCURSOR_SIZE` is pinned in comp's environment | The reply's `size_env_pinned: true` says so; unset it in `/etc/duduclaw/kiosk.env` |
| Size reverts and `size_env_pinned` is false | The preference file could not be written | The `set` reply carries `persisted: false` + `persist_error`; check `$HOME` ownership on `/data/duduclaw-kiosk` |

---

## WM-2 (2026-08-23): server-side decorations + floating windows

User decision for this round: **B — 浮動視窗＋完整窗感**. WM-1 was explicitly
transitional (fill the work area, answer `ClientSide` to every decoration
request because comp drew none). WM-2 replaces both halves.

### What changed

| | WM-1 | WM-2 |
|---|---|---|
| new non-shell toplevel | fills the work area | **floats**: 80 % of the work area, centred, cascaded +24,+24 (wraps) |
| `zxdg_decoration_manager_v1` | always `ClientSide` | **negotiated per window**, preference `ServerSide` |
| decoration drawing | none | 32 px title bar (live `xdg_toplevel.title`), close ✕, 1 px `stone-300` border, 8 px stepped drop shadow |
| title bar drag | — | clamped move grab (the bar can never leave the work area) |
| maximize | client filled the work area | the **frame** fills the work area; the client gets that minus its decoration |
| unmaximize | "comp keeps no restore geometry" | restores the remembered floating frame |
| session shell | full output, undecorated | **unchanged** |

New module `src/decor/` (`mod.rs` geometry, `placement.rs`, `mode.rs`,
`text.rs`, `xmark.rs`, `paint.rs`); `Cargo.toml` gains `ab_glyph`;
`assets/fonts/{Inter-500,NotoSansTC-500}.ttf` are vendored from
`crates/duduclaw-native-gui/assets/fonts/static/` and `include_bytes!`-embedded.

### The geometry model (read this before touching anything here)

**`Space` still maps the CONTENT rectangle.** The decoration is drawn *around*
it and appears nowhere in `Space`'s bookkeeping, so every existing caller of
`element_location` / `element_geometry` / `element_under` / `surface_under`
keeps meaning exactly what it meant before — which is why WM-2 does not touch
`codrive/`, `shell_control/`, or the resize grab. The alternative (wrap
`Window` in a decorated `SpaceElement` so `Space` maps the frame) is the more
idiomatic smithay shape and was rejected for exactly that blast radius.

Cost: `Space::element_under` cannot see a title bar. That is handled
explicitly by `decor::hit_frame` (pure, unit-tested) plus
`DuduclawComp::frame_hit_at`, which runs **before** the ordinary surface
routing in `input.rs`'s pointer-button arm and returns `None` for a press in a
window's content area.

`DecorInsets::SSD` = `{top: 33, left: 1, right: 1, bottom: 1}` (32 px bar +
1 px border). Asserted by `decor::tests::ssd_insets_are_the_title_bar_plus_the_border`.

### Why `desktop::space::render_output` is no longer called

It builds `[all custom elements…, all windows…]` — every overlay above every
window. Correct for the cursors / codrive highlight / shadow PiP, wrong for a
per-window title bar the moment windows overlap (which floating placement
makes immediate). `decor::paint::build_output_elements` assembles the same list
interleaved per window and ends in the identical
`OutputDamageTracker::render_output` call upstream makes, so damage tracking,
the output transform, and the udev backend's "no damage ⇒ no page flip" idle
property are unchanged. Two formulas are copied verbatim from smithay 0.7.0
(read, not remembered) and must stay that way:

* render location = `element_location − element.geometry().loc − output.loc`
  (dropping the middle term shifts every client with CSD shadows);
* a window is skipped unless its bbox overlaps this output — **this is what
  keeps CD-2's shadow workspace off the real screen**, widened only to include
  the 8 px drop shadow.

Per-window z-order is `[popups] [decoration] [toplevel surface] [shadow]`:
popups above the title bar (an xdg-positioner may legitimately place one at a
negative offset), decoration above the client surface (so a client whose
surface overruns its declared geometry cannot hide its own close button).

### Why every decoration buffer is cached per window

`SolidColorBuffer::new` and `MemoryRenderBuffer::from_slice` each mint a fresh
element `Id`, and an element whose id changes every frame reads as a brand-new
element to `OutputDamageTracker` — so a rebuilt-per-frame title bar would
report damage every composite and the udev backend would page-flip at 60 Hz on
a completely idle desktop. Cached buffers are mutated with
`SolidColorBuffer::update` (id-stable, commit bumps only on a real change);
the two rasters are rebuilt only when their inputs change — `(title, available
width)` for the text, hover state for the ✕. `MemoryRenderBufferRenderElement::
from_buffer` reuses `buffer.id`, verified in smithay's source.

Live evidence that this holds: `decor: title raster rebuilt` fires **once per
window**, never per frame.

### Verification (2026-08-23, container `rust:bookworm` aarch64)

Standard volumes (`duduclaw-comp-cargo`, `-cargo-git`, `-target`).

```
cargo build                              -> Finished dev profile, 0 warnings
cargo clippy --all-targets -- -D warnings -> Finished, no warnings
cargo test                               -> ok. 320 passed; 0 failed
```

**320 = 253 pre-existing (all still green) + 67 new**: 20 in `decor` (frame /
content / title bar / close button / hit test / clamp / refit / shadow bounds),
11 in `decor::placement` (80 % floor, centring, 24 px cascade, wrap, "every
cascade position stays inside the work area", tiny work area), 7 in
`decor::mode` (the anti-double-title-bar rule, shell/shadow refusals), 4 in
`decor::paint`, 14 in `decor::text` (both fonts parse, CJK falls back to Noto,
CJK truncation on char boundaries, premultiplied pixels, width caps, absurd
title cap), 5 in `decor::xmark`, 6 in `grabs::move_grab` (clamped drag).

Live rounds, three-layer stack (`weston --backend=headless-backend.so` →
`duduclaw-comp` → `foot`), same recipe as earlier sections:

1. **Four clients, decorated.** All four alive, comp alive, **0 panics, 0
   renderer refusals**. Cascade observed exactly as unit-tested:

   ```
   frame (128, 98, 1024, 544)  -> content (129, 131, 1022, 510)
   frame (152,122, 1024, 544)  -> content (153, 155, 1022, 510)
   frame (176,146, 1024, 544)  -> content (177, 179, 1022, 510)
   ```

   (work area 1280×800 − 30 − 90 = `(0, 30, 1280, 680)`; 80 % = 1024×544;
   centred base x = (1280−1024)/2 = 128.)

2. **CJK title rasterised through the Noto fallback**:
   `title=視窗標題 B — DuDuClaw 值班機 raster=(147, 13) avail_w=956`.
   `avail_w` = 1022 (bar) − 46 (close) − 12 (pad) − 8 (gap) = 956 ✓.

3. **`title_changed` reaches the compositor** — `foot -T` produced
   `xdg_shell: title changed — repainting the title bar`.

4. **Demotion race** (`DUDUCLAW_COMP_SHELL_APP_ID=foot-shell`, a `foot` mapped
   *before* the shell stand-in): the early window was provisionally the shell
   (full output, decoration overridden to client-side), then demoted when
   `foot-shell` claimed the role by app_id — and **got its title bar back**
   (`title raster rebuilt … avail_w=956`) at the floating placement
   `(129, 131, 1022, 510)`. This is the case the "never overwrite what the
   client negotiated" rule in `window_policy::sync_decoration_mode` exists for;
   an earlier draft stored the downgrade and left such a window permanently
   undecorated.

### Not verified in the container (needs the VM / real hardware)

* **Anything visual.** The container is headless — there is no screenshot. What
  is drawn (colours, the ✕ shape, shadow gradient, text position inside the
  bar) is verified only by unit tests over the buffers, not by looking.
* **Hover, click-to-close, drag.** Weston's headless backend has no pointer
  device, so `frame_hit_at` / `update_close_hover` / `begin_titlebar_move` have
  never run against a real seat. Their geometry is unit-tested; their wiring is
  not.
* **Maximize / unmaximize.** `foot` under SSD has no maximize affordance to
  click. The geometry is unit-tested and the handler is straight-line code, but
  no client has driven it.
* **The udev backend's idle behaviour with decorations.** The reasoning (cached
  ids) is verified in smithay's source and by the once-per-window raster log,
  not by measuring flips on real hardware.
* **Chromium specifically.** The one first-hand fact this round leans on —
  Chromium 151 draws its own CSD tab strip in normal mode — comes from WM-1's
  notes, not from a run in this round.

### Known limitations (deliberate, this round)

* **No rounded corners** — the task brief excluded them explicitly.
* **No dark mode.** Comp has no theme mechanism; `decor::Palette` is the single
  place to switch when one lands (`TODO(theme)` marks it).
* **No resize by dragging the border.** A 1 px border is not a resize target;
  clients keep their own resize edges via `xdg_toplevel.resize`, which is
  unchanged.
* **No double-click-to-maximize on the title bar**, no window menu, no
  minimize/maximize buttons — only the close button was in scope.
* **`fullscreen_request` is still upstream's no-op.** Untouched by this round.
* **Title text is unshaped** (glyph-per-char + advances). Correct for Latin and
  CJK; Arabic/Devanagari titles will render unshaped. Adding HarfBuzz for a
  32 px strip was judged not worth the C dependency.
* **A client that never creates a `zxdg_toplevel_decoration_v1` gets no server
  decoration.** That is the conservative half of the negotiation table
  (`decor::mode`) and it is deliberate: such a client has, by convention, opted
  into drawing its own, and giving it a second title bar would be worse.
  Super+Q still closes it.
* **The shadow is a 4-ring stepped ramp, not a real gradient** — a gradient
  needs a per-window texture (allocation on every resize) or a shader this
  crate does not have.

### Nothing the shell has to do

No `duduclaw-shell` change is required or was made. Two things are worth
knowing on the shell side, though, and neither is a bug today:

* The shell keeps being announced `ClientSide`, so gpui's
  `WindowDecorations::Client` state is unchanged from WM-1.
* The reserved bands (30 / 90) are still the contract between the two crates.
  If the shell's menu bar or dock height ever changes, `window_policy`'s
  constants must move with it — floating placement is computed against the work
  area those bands define, so a stale value now misplaces every window rather
  than just clipping the dock.

## WM-3 (2026-08-23): layer-shell, Alt-Tab, edge resize, minimize, double-click maximize

D1 in `commercial/docs/ROADMAP-agent-first-os-2026-08.md`. Five items, in the
order the task brief listed them, all compositor-side — **`duduclaw-shell` is
not touched and does not have to change for any of it to work.**

### 1. `zwlr_layer_shell_v1` (the A1 prerequisite)

New module `src/layer_shell/` (`mod.rs` protocol + runtime, `geometry.rs`
pure). Comp now advertises the global, maps layer surfaces into smithay's
per-`Output` `LayerMap`, and honours the four layers as a real z-order:

```
[ human cursor, agent cursor, codrive highlight, shadow PiP ]   ← unchanged
[ Alt-Tab switcher panel ]                                      ← WM-3
[ overlay layer ][ top layer ]                                  ← WM-3
  per window, top of the z-order first:
      [ popups ][ decoration ][ the window's surfaces ][ shadow ]
[ bottom layer ][ background layer ]                            ← WM-3
```

Two deliberate departures from smithay's own `space_render_elements`, both
recorded because they are the kind of thing that silently regresses:

* upstream splits layers **two** ways (`Background|Bottom` under, `Top|Overlay`
  over) and does not order `Overlay` above `Top` — it emits them in reverse
  map-insertion order. This crate ranks all four explicitly
  (`layer_shell::geometry::layer_rank`), because a lock screen or a global
  palette on `overlay` must cover a panel on `top` regardless of which mapped
  first — and A1's ⌘K palette is exactly that case.
* **pointer routing follows the same ranking.** `DuduclawComp::surface_under`
  asks overlay/top first, then windows, then bottom/background; the
  pointer-button arm gives an above-windows layer surface first refusal ahead
  of the decoration hit test, so a panel over a title bar takes the click
  instead of starting a window drag. Coordinate chain copied from
  `anvil/src/input_handler.rs::surface_under` (v0.7.0, MIT, same repo as the
  `smallvil` this crate is adapted from): layer geometry is **output-local**,
  window geometry is global, so every crossing adds or subtracts
  `output_geometry().loc` explicitly.

Also wired: layer-surface frame callbacks + `LayerMap::cleanup` in both
backends (a layer surface is not in `Space`, so without this a double-buffering
panel stalls after one commit); `LayerMap::arrange` on output resize, **before**
the window policy re-runs, since the policy reads the zone that pass computes;
xdg-popups whose root is a layer surface (a panel's own menu) are tracked,
unconstrained and grabbable.

### Exclusive zone → work area: **intersection**, and the live run that decided it

`layer_shell::geometry::effective_work_area` combines WM-1's hard-coded
`ReservedBands` (30 top / 90 bottom, the unmigrated shell's own chrome) with the
layer map's `non_exclusive_zone()`. The first draft made the zone **replace** the
bands, which reads naturally from the brief's "exclusive zone 取代 hardcode
reserved band". The very first live run showed why that is wrong:

```
# zone-replaces-bands (rejected):
waybar maps a 30px top panel
  -> work area (0, 30, 1280, 680)  becomes  (0, 30, 1280, 770)
  -> foot placed at frame (128, 107, 1024, 616)
```

The panel's own 30 px claim was honoured and **the shell's 90 px dock
reservation silently vanished** — any third-party layer client would have put
windows straight over the dock. Intersection cannot do that: a layer surface may
only ever shrink the work area further. Re-run with the same waybar:

```
# intersection (shipped):
foot placed at frame (128, 98, 1024, 544) -> content (129, 131, 1022, 510)
```

which is **byte-identical to the WM-2 numbers recorded in the section above** —
"殼還沒遷移前行為逐位不變" holds literally, not approximately.

Double-counting is the theoretical cost and it does not bite: when the shell
migrates, its layer surfaces claim *the same* 30/90 the constants describe, and
`A ∩ A = A`. Proven live by running comp with
`DUDUCLAW_COMP_RESERVED_TOP=0 DUDUCLAW_COMP_RESERVED_BOTTOM=0` so the zone is the
only constraint:

```
window_policy: applied … rect=(128, 107, 1024, 616) reserved=(0, 0)
```

i.e. 80 % of the zone's 770 px height, centred inside it — the exclusive zone
genuinely drives the layout, it is not being ignored.

### Bug found live: layer surfaces were landing on the CD-2 shadow workspace

`codrive::create_shadow_output` advertises the shadow output as a real
`wl_output` global, so an output-aware layer client treats it as a second
monitor. The first live run had **both** `swaybg` and `waybar` creating a second
surface on `duduclaw-shadow-0`:

```
layer_shell: new layer surface namespace=wallpaper layer=Background output=duduclaw-shadow-0
layer_shell: new layer surface namespace=waybar    layer=Top        output=duduclaw-shadow-0
```

Those surfaces can never be composited (the shadow output is only ever rendered
offscreen for the PiP preview) and would therefore never receive a frame
callback — that half of the client stalls forever. `new_layer_surface` now
refuses them with the protocol's own `closed` event, which is the standard
output-hotplug path every layer client already handles (verified: both clients
stayed alive and kept their real-output surface). Deliberately **not** fixed by
un-advertising the shadow output: clients rely on `wl_surface.enter` to learn
their scale, so revoking that global would change CD-2's own verified behaviour
— a separate decision with its own verification, not a side effect of this work
package.

### 2. Alt-Tab (and Super-Tab) — MRU switcher

`src/alt_tab.rs` (pure: MRU list, selection wrap, panel geometry, scrolling) +
`src/switcher.rs` (live: session, key handling, cached panel buffers).

`state::cycle_focus` is **gone**. It promoted the bottom of the z-order on every
press — a real rotation, but pressing it twice never returned you to where you
started, because each press permanently reordered the stack. WM-3 keeps a
most-recently-focused list (updated in the one place every focus path already
funnels through, `focus_window`) so one tap flips between the two windows you
are actually working in and holding walks further back.

* `Alt+Tab` **and** `Super+Tab` open it; `Shift` reverses; `Escape` cancels;
  releasing both modifiers commits. Tab is **intercepted**, not forwarded — a
  stray Tab landing in the focused client mid-switch is the sort of "my form
  jumped a field" bug nobody traces back to the compositor.
* `Escape` is guarded on `!modifiers.logo` so it can never shadow the Super+Esc
  emergency stop.
* `is_switcher_keysym` accepts `ISO_Left_Tab` as well as `Tab` — xkb reports
  Shift+Tab as the former, and matching only `Tab` would have left the backwards
  direction silently dead.
* The candidate list is snapshotted at open, so a window mapping or dying
  mid-switch cannot renumber it under the user's fingers; `commit` re-checks
  liveness and does nothing rather than focusing whatever slid into that index.
* **Minimized windows are candidates**, and committing to one restores it.
* The session shell is excluded (it is the desktop, not a window you switch to).
* Panel buffers are cached on `(labels, selected, panel width)` for the same
  reason the decoration's are — see the WM-2 section's "why every decoration
  buffer is cached per window". Holding Alt without pressing Tab rebuilds
  nothing.

Thumbnails were considered and rejected for this round: they need a per-window
offscreen render pass (the machinery `codrive/shadow.rs` has for the PiP) for a
panel that is on screen for a fraction of a second. The brief allowed
"視窗標題列縮圖可簡化為標題文字列表".

### 3. Edge resize — the ring is the drop shadow

`src/decor/edges.rs`. The obvious implementation ("the outer 8 px **inside** the
frame") is wrong: on a server-decorated window those pixels are the client's own
surface, so a scrollbar or a list item flush against the edge would stop
receiving clicks. So the hot zone sits **outside** the frame instead, filling
exactly the 8 px drop-shadow ring WM-2 already draws — the standard
invisible/extended resize border, and `hit_frame_edge` returns `None` for any
point inside the frame, so it steals nothing.

Two consequences, both deliberate:

* a window's ring overlaps whatever is beneath it (`frame_hit_at` walks
  top-down, so the ring belongs to the window above — the same trade every
  extended border makes);
* the ring is **clipped to the work area**, or a window near the top of it would
  put a resize strip over the shell's menu bar. That also means a **maximized**
  window — whose frame *is* the work area, so whose ring lies entirely outside
  it — cannot be edge-resized, which is correct and costs no extra code.

Corners get a 24 px zone along each axis. The A5 debt the brief named is paid by
`grabs::resize_grab::clamp_resize_size` (pure, 13 tests): the client's own
`min_size`/`max_size`, then a 320×240 floor **layered on top of** (never
replacing) the client's minimum, then a cap so the resulting **frame** cannot
leave the work area on the edge being dragged — the `TOP` arm is the one that
keeps the title bar on screen. With `clamp = None` the function is byte-identical
to the pre-WM-3 expression, which is what keeps client-initiated
`xdg_toplevel.resize` (a client asking to be resized, e.g. its own toolkit resize
edges) behaving exactly as it did.

`resize_grab::handle_commit` now returns "did this commit move the window" (it
used to return `Some(())` for any commit of a mapped window, i.e. told the caller
nothing) so `decor_sync_frame` can run again *after* a TOP/LEFT drag moves the
origin.

### 4. Minimize

`src/minimize.rs`. A `－` button left of the ✕ (same 46 px box, neutral hover
fill — minimizing destroys nothing, so it must not borrow the "this is
dangerous" red). Minimized means **unmapped from `Space`, still alive**: the
`Window` handle moves into `DuduclawComp::minimized`. The client is told
nothing, because xdg-shell has no minimized state to tell it; what it observes
is that its frame callbacks stop, which is right for an off-screen window.

The invariant that makes this safe is **a minimized window is always
recoverable**, and it is enforced by there being exactly two ways out of the
list (restore, destruction) and three ways back in:

* Alt-Tab;
* `shell_control`'s `focus_window` op — `list_windows` now includes minimized
  windows and carries an additive `"minimized": bool` field. The op's semantics
  are unchanged ("bring this window to the front"); what grew is the set of
  windows that can honestly answer it. Safe without touching `duduclaw-shell`:
  its `comp_client::CompWindow` derives a plain `Deserialize`, which ignores
  unknown fields, so the shipped shell simply shows a minimized window in its
  dock as it shows any other. Rendering it *differently* is a shell-side change
  for a later round.
* codrive's `activate_window` — same widening, for the same reason. There is no
  case for the human being able to recover a window and the agent not.

The session shell and shadow-workspace windows are refused outright (a
minimized desktop is a black screen; a shadow window's geometry is owned by
`codrive/shadow.rs`). Focus handoff reuses the close-time path, so minimizing a
background window never steals focus from what you were using.

`codrive::window_target::find_target_window(&Space, …)` became
`find_target_in(&[Window], …)` — resolving against the space alone would answer
"no toplevel matched" for exactly the windows a dock exists to bring back. The
matching *policy* is unchanged and still lives in one place.

### 5. Double-click the title bar = maximize / restore

`decor::is_double_click` (400 ms, 8 px slop, both pure and tested). The
remembered press is consumed either way, so a third rapid click starts a fresh
pair rather than flapping the window.

`maximize_request` / `unmaximize_request` were refactored into one
`DuduclawComp::set_maximized`, which the double-click path drives too. That is
not tidiness: a second copy would be a second place for the "the FRAME fills the
work area, the client gets that minus its decoration" rule and the
restore-geometry snapshot ordering to drift, and both are subtle enough that the
drift would be silent.

### Other fixes carried in this round

* `unconstrain_popup` used `space.outputs().next()`, which since CD-2 returns the
  **headless shadow output** at `(0, 100_000)` — every popup was being
  unconstrained against a rectangle 100 000 px below the screen. Now
  `layout_output()`, and its `unwrap()`s are gone with it.
* `xdg_decoration: overriding the negotiated mode` fired on every layout
  re-apply. The policy re-runs far more often now (every layer surface
  map/unmap), so it is gated on the announced value actually changing.
* A layer surface this compositor refused no longer triggers a pointless
  `reapply_window_policy_all` when its client destroys it.

### Verification (2026-08-23, container `rust:bookworm` aarch64)

Standard volumes (`duduclaw-comp-cargo`, `-cargo-git`, `-target`).

```
cargo build                               -> Finished dev profile, 0 warnings
cargo clippy --all-targets -- -D warnings -> Finished, no warnings
cargo test                                -> ok. 398 passed; 0 failed
```

**398 = 320 pre-existing (all still green) + 78 new**: 22 in `alt_tab` (MRU
promote/forget, candidate order incl. never-focused windows and stale entries,
one-tap-goes-to-the-previous-window, wrap in both directions, "holding walks
every candidate exactly once", panel centring/shrinking/tiny-output, row
stacking, scrolling keeps the selection visible for every index), 15 in
`layer_shell::geometry` (the four-band ranking, overlay-above-top, the
intersection rule incl. the third-party-panel regression above, output-local
translation, degenerate zones), 13 in `grabs::resize_grab` (`FrameEdge` →
`ResizeEdge` incl. "corners set two bits", the unclamped path being byte-identical,
the 320×240 floor, client min/max, each edge's work-area cap replayed against
`handle_commit`'s own move formula), 12 in `decor::edges` (ring == shadow width,
"a point inside the frame is never a resize target", four sides, four corners,
the work-area clip, "a maximized window cannot be edge-resized"), 6 in
`decor::minus`, 7 more in `decor` (minimize button placement, the narrow-bar
fallback, title text stopping before the left-most button, double-click timing
and slop), 2 in `input` (`ISO_Left_Tab`), 1 in `shell_control::protocol` (the
`minimized` flag on the wire).

Live rounds, four-layer stack (`weston --backend=headless-backend.so` →
`duduclaw-comp` → `foot` + `swaybg` + `waybar`), two scenarios (bands at 30/90,
and bands forced to 0/0). Both: **all six processes alive at the end, 0 panics,
0 renderer refusals.** Evidence quoted in the sections above —
`swaybg` (background layer) and `waybar` (top layer + exclusive zone) both bind
the global, get an initial configure, survive the shadow-workspace refusal, and
`waybar`'s zone reaches the window layout policy.

### Not verified in the container (needs the VM / real hardware)

The headless weston backend has **no keyboard and no pointer device** (the same
limitation every earlier round of this crate records), and this work package is
mostly input:

* **Alt-Tab end to end.** The pure selection logic and the panel geometry are
  unit-tested; the key handling, the intercept, the hold-to-preview overlay and
  the release-commit have never run against a real seat.
* **Edge resize, the minimize button, the close/minimize hover, and
  double-click-to-maximize.** Geometry unit-tested, wiring not exercised.
* **Anything visual**: the switcher panel's colours and layout, the `－` glyph
  next to the ✕, layer surfaces actually appearing above/below windows. There is
  no screenshot in a headless container; the z-order is verified as an element
  *ordering*, not as pixels.
* **A layer surface receiving a click.** Routing is implemented and ordered;
  with no pointer device nothing has clicked one.
* **The udev backend's idle behaviour with a layer surface mapped.** Reasoning
  (cached ids, "no damage ⇒ no page flip") is unchanged from WM-2, not measured.

### Known limitations (deliberate, this round)

* **The shell still does not use layer-shell.** Its dock and menu bar remain
  inside its own full-output toplevel and the reserved bands remain the
  contract. What the shell has to do later: create one `zwlr_layer_surface_v1`
  per chrome element on the `top` layer, anchor it, `set_exclusive_zone(30)` /
  `(90)`, and — in the same change — zero `window_policy`'s
  `DEFAULT_RESERVED_TOP`/`_BOTTOM` so the two descriptions of the same chrome
  cannot drift apart.
* **No cursor shape change over the resize ring.** Comp has the cursor plumbing
  (`crate::cursor`) but nothing drives a per-region shape; the ring is
  discoverable only by the shadow it coincides with.
* **No minimize animation, and no dock "minimized" affordance** — the shell
  renders a minimized window like any other until it consumes the new flag.
* **No keyboard-interactivity `exclusive` handling.** A layer surface that asks
  for exclusive keyboard focus is treated as `on_demand`: it gets focus when
  clicked, and does not lock the keyboard away from windows. A lock screen would
  need the exclusive semantics; nothing needs them yet.
* **Dragging a maximized window's title bar moves it** rather than restoring and
  dragging. Edge-resizing it is impossible (above), so it cannot be left in a
  broken geometry, but the interaction is unfinished.
* **The switcher is title-text only** (no thumbnails), and it does not respond to
  the pointer.
* **On a multi-monitor udev setup the switcher panel is drawn on every output**
  (it is centred per output, and `build_output_elements` runs per output). One
  panel on the focused monitor would be the refined behaviour; nothing on the
  appliance has two monitors yet.

## D3-a / D3-c (2026-08-23): Chinese input — three globals, the candidate window, and keeping fcitx5 off the agent seat

Design/research: `research/native-os-2026-08/ime-fcitx5-gpui-2026-08.md` (the
D3 spike). Tracking: `commercial/docs/TODO-agent-first-os-2026-08.md`, rows
`D3` / `D3-c 探針`. Shell-side (D3-b) and image packaging (D3-d) are separate
work packages and are **not** in this round.

### What landed

New module `src/ime/`:

| file | what |
|---|---|
| `mod.rs` | the three globals, `InputMethodHandler`, candidate-window render elements |
| `seat_filter.rs` | D3-c: the agent seat is invisible to input-method clients |
| `popup.rs` | pure candidate-window placement geometry + tests |

Touched: `state.rs` (field + construction + per-client classification at accept
time), `handlers/mod.rs` (`delegate_seat!` moved, see below), `decor/paint.rs`
(one line in `build_output_elements`), `codrive/{mod,shared,listener,protocol}.rs`
(the `paused_by_ime` backstop), `winit_backend.rs` / `udev_backend.rs` (one
housekeeping call each).

### Why all three globals, in one call

fcitx5's `WaylandIMServerV2::init()` only sets `init_ = true` when it has found
**both** `zwp_input_method_manager_v2` **and** `zwp_virtual_keyboard_manager_v1`.
Advertising the input-method global alone produces a compositor where Chinese
input silently never starts, with no error anywhere. Clients need the third,
`zwp_text_input_manager_v3`. So all three are created together in
`ImeState::new`, before `init_wayland_listener` opens the socket.

Focus needs no code at all: smithay ties text-input focus to keyboard focus.

### The candidate window is mandatory, not a nicety

With no `zwp_input_popup_surface_v2` path, fcitx5's classicui logs "No Panel
surface available, return." and keeps composing **invisibly** — the hardest
failure in this chain to diagnose. `InputMethodHandler::new_popup` records the
surface; `DuduclawComp::ime_popup_elements` draws it through the
`WaylandSurfaceRenderElement` variant CUR-1 already added, so no new element
kind and no change to `render_output`'s `custom_elements` interface were needed.

It is inserted in `build_output_elements` directly after the Alt-Tab panel:
above every window and every layer surface (it is anchored to a caret; a panel
drawn over it would hide the characters being chosen), below the switcher
(modal while up) and below the cursors.

### D3-c: what the probe actually found

The spike report proposed reaching a per-client filter through
`create_global_with_filter`. **That literal route is closed.**
`SeatState::new_wl_seat` uses plain `create_global`, and `SeatGlobalData<D>`
has a private `arc` field with no constructor — this crate cannot build the
global data, so it cannot create the seat global itself.

What is open, and what shipped:

1. **`delegate_seat!` splits.** It is one `delegate_global_dispatch!` plus four
   `delegate_dispatch!` over public types. `src/ime/seat_filter.rs` writes the
   four `Dispatch` delegations verbatim and hand-rolls only the
   `GlobalDispatch`, whose `bind` forwards straight to smithay's own impl. The
   single behavioural difference from the macro is the `can_view` override.
2. **The seat's name is readable through `Debug`.** `can_view` receives only
   `&SeatGlobalData<D>`, but `SeatRc<D>`'s `Debug` prints `name` as its first
   field. A `fmt::Write` sink aborts the rendering the moment the name is
   complete, so `SeatRc::inner` (a `Mutex` over the pointer/keyboard handles
   and the bound-seat list) is never formatted.

Point 2 leans on a `Debug` rendering, which is not a stability guarantee. Two
things keep that honest:

* `parse_seat_name` is a pure function with unit tests, and
  `a_real_seats_name_survives_the_round_trip` drives the whole extraction over
  a **real** `Seat<DuduclawComp>` built by the linked smithay. A smithay
  upgrade that changes the rendering fails in CI.
* `seat_filter::arm` re-runs that extraction at startup over the two real
  seats and **disarms** on any surprise — every seat stays visible to everyone,
  exactly as before this module existed, with a loud `error!`. The codrive
  backstop below then turns the consequence into a reported error rather than
  silence.

Input-method clients are identified once per connection, at accept time, from
`Client::get_credentials`'s pid → `/proc/<pid>/comm` (`std`'s
`UnixStream::peer_cred` is still unstable — `E0658`, rust-lang/rust#42839 —
which `shell_control/listener.rs` had already run into). `/proc/comm` is not
authentication, and does not need to be: a process that lies its way into "I am
an input method" only loses sight of the agent seat.

Knobs: `DUDUCLAW_COMP_IME_PROCS` (comma-separated process names; default
`fcitx5,fcitx,ibus-daemon,kimpanel`; an empty value turns detection, and hence
the filter, off) and `DUDUCLAW_COMP_IME_STRICT=1` (only detected input methods
may bind the two IME manager globals; default off, because a false negative
there kills Chinese input outright while the appliance's client set is entirely
ours).

### The backstop: `paused_by_ime`

The filter has one soft edge — process-name recognition. If an input method
slips past it, the failure mode is the worst kind: `type_text` returns success
while every keystroke disappears into a composition nobody reads. So codrive
gained an explicit state:

* `DuduclawComp::codrive_ime_grab_active` reads smithay's per-seat
  `InputMethodHandle` for a live `zwp_input_method_keyboard_grab_v2`.
* `codrive_refresh_ime_pause` publishes it to `CodriveShared::ime_paused` and
  is called from `handle_agent_inject` **and** once per housekeeping tick on
  both backends — the tick is what stops the mirror latching `true` after the
  input method exits (a latched mirror would reject keyboard ops forever, with
  no injection left to clear it).
* `listener.rs` pre-rejects `key`/`key_name`/`text` with
  `{"ok":false,"error":"paused_by_ime","reason":"input_method_holds_agent_seat_keyboard"}`;
  the main thread re-checks authoritatively and drops with an audit record. A
  race can lose a keystroke; it can never let one through silently.
* `ime_note_grab_state` logs the `(human, agent)` grab pair on every change.
  `human=true agent=false` is a healthy Chinese-input session; `agent=true`
  means D3-c failed. That one line is the operational answer to "is the IME on
  the right seat".

### Container verification (2026-08-23)

Same warm-cache container as the A4-1/WM-3 rounds (`duduclaw-comp-cargo`,
`duduclaw-comp-cargo-git`, `duduclaw-comp-target`), plus `fcitx5
fcitx5-modules fcitx5-chewing dbus-x11 wayland-utils` for the live rounds.

```
cargo build                              -> Finished
cargo clippy --all-targets -- -D warnings -> Finished (no warnings)
cargo test                               -> ok. 425 passed; 0 failed
```

**425 = the 398 pre-existing tests (all still green) + 27 new**: 17 in
`ime::seat_filter` (7 parser, 3 sniffer-against-real-smithay, 4 self-check
decision, 3 process-name matching), 10 in `ime::popup` (placement, flipping,
clamping).

### Live rounds

Rig: `weston --backend=headless-backend.so` → `duduclaw-comp` (winit) →
`{ foot, fcitx5 + fcitx5-chewing }`. Headless weston has **no input device**,
so the nested compositor's *human* seat can never be driven from outside —
which is exactly the seat fcitx5's grab lives on. The full-chain round
therefore nests **two** comps: codrive drives the OUTER comp's agent seat, and
the outer comp's focused client is the INNER comp's winit window, so those keys
arrive at the inner comp as ordinary winit input, i.e. on its HUMAN seat.
(`WAYLAND_DISPLAY` accepts an absolute socket path, which is what lets the two
comps keep separate `XDG_RUNTIME_DIR`s and therefore separate codrive sockets.)

**1. Globals, and who sees which seat.** `wayland-info` (an ordinary client)
against comp:

```
interface: 'wl_seat',                        version: 9, name: 10
interface: 'wl_seat',                        version: 9, name: 11
interface: 'zwp_text_input_manager_v3',      version: 1, name: 13
interface: 'zwp_input_method_manager_v2',    version: 1, name: 14
interface: 'zwp_virtual_keyboard_manager_v1',version: 1, name: 15
```

fcitx5's own `WAYLAND_DEBUG=1` registry, same compositor, same moment:

```
wl_registry@2.global(11, "wl_seat", 9)            <- ONE seat, not two
wl_registry@2.global(14, "zwp_input_method_manager_v2", 1)
wl_registry@2.global(15, "zwp_virtual_keyboard_manager_v1", 1)
waylandimserverv2.cpp:80] INIT IM V2
 -> zwp_virtual_keyboard_manager_v1@9.create_virtual_keyboard(wl_seat@11, ...)
 -> zwp_input_method_manager_v2@3.get_input_method(wl_seat@11, ...)
wl_seat@11.name("winit")                          <- the human seat
```

comp side: `comp/ime: client identified as an input method … pid=1936
comm=fcitx5`, `status="armed"`.

**2. Full chain: 注音組字 → 候選窗 → 上屏.** Injected `su3cl3` (ㄋㄧˇ ㄏㄠˇ),
then space, then `1`:

```
fcitx5 -> comp:   zwp_input_method_v2@14.set_preedit_string("ㄋ", 0, 3)
                  ... ("ㄋㄧ") ... ("你") ... ("你ㄏ") ... ("你ㄏㄠ") ... ("你好")
                  zwp_input_method_v2@14.get_input_popup_surface(new id …@18, wl_surface@20)
                  zwp_input_popup_surface_v2@18.text_input_rectangle(324, 16, 1, 14)
                  zwp_input_method_v2@14.commit_string("你好")
comp -> foot:     zwp_text_input_v3@19.preedit_string("ㄋ", 0, 3)
                  ... ("你好", 6, 6) ...
                  zwp_text_input_v3@19.commit_string("你好")
comp log:         comp/ime: candidate window opened
                    location=(177, 16) caret=(177, 16, 14, 14)
                  comp/ime: input-method keyboard grab changed
                    human_seat=true agent_seat=false
```

Note fcitx5 loads the `chewing` addon **after** it reads its profile, so
`DefaultIM=chewing` cannot take effect on a first run; the test switches
explicitly over D-Bus (`org.fcitx.Fcitx.Controller1.SetCurrentIM`). Worth
remembering for D3-d's firstboot provisioning.

> **Superseded by D3-f (2026-08-23).** That D-Bus switch is gone from
> `duduclaw-kiosk-launch.sh`. It was a cold-start patch for a profile whose
> item order was itself the bug (chewing at `Items/0` killed fcitx5's
> `Shift_L` Chinese/English toggle — see the `D3-f/P0-2` row in
> `commercial/docs/TODO-agent-first-os-2026-08.md`), and it raced a fixed
> three-second sleep against daemon startup. The seed now puts `keyboard-us`
> at `Items/0` and gets "lands in Chinese" from `[Behavior]
> ActiveByDefault=True` instead, which needs no D-Bus call and no timing
> assumption at all.

**3. Negative control — the conflict is real, and the filter is what stops it.**
Same rig with `DUDUCLAW_COMP_IME_PROCS=""` (detection, hence the filter, off),
then an agent click so the agent seat gets a focused text-input client:

```
 -> zwp_input_method_manager_v2@3.get_input_method(wl_seat@11, …@16)   <- TWO input methods
 -> zwp_input_method_manager_v2@3.get_input_method(wl_seat@13, …@18)
wl_seat@11.name("duduclaw-agent")
wl_seat@13.name("winit")
zwp_input_method_v2@16.activate()
 -> zwp_input_method_v2@16.grab_keyboard(new id zwp_input_method_keyboard_grab_v2@23)

comp:  comp/ime: input-method keyboard grab changed human_seat=false agent_seat=true
       WARN codrive: agent-seat keyboard injection PAUSED by an input method grab

codrive: {"op":"text","s":"agent-typed"}
      -> {"ok":false,"error":"paused_by_ime","reason":"input_method_holds_agent_seat_keyboard"}
audit:   "kind":"inject_dropped","op":"text","detail":"paused_by_ime: …"
```

With the filter on, the second `get_input_method` never happens and codrive
types normally.

### Honest gaps

* **fcitx5 5.0.21 (bookworm, the container) grabs on `activate`, not at input
  context creation.** The spike report's "grab at IC creation" reading is from
  fcitx5 **master**; trixie ships 5.1.12. Either behaviour is covered — the
  filter removes the second input context entirely — but the negative control
  above needed an explicit agent-seat focus to make the grab appear, and that
  detail is version-specific.
* **The candidate window's pixels are unverified.** `new_popup` fires with a
  real caret rectangle and the render path runs every frame without incident,
  but a headless container has nothing to screenshot. Placement geometry is
  unit-tested; on-screen position, size and z-order belong to the VM round
  (D3-e).
* **`parent_geometry` searches windows only, not layer surfaces.** Nothing puts
  a text field on a layer surface yet (`crate::layer_shell`'s scope note); an
  unmatched surface degrades to the space origin. Revisit when the shell's dock
  and menu bar migrate.
* **`ime_paused` is refreshed per housekeeping tick**, so `listener.rs`'s
  pre-check can be up to one tick stale in either direction. The main thread's
  check is authoritative, so the only cost of staleness is a keystroke dropped
  with an honest error, never one silently swallowed.
* **`keyboard_grabbed()` over-reports if a popup grab later replaces the input
  method's grab on the same seat.** The seat filter should make that
  unreachable; if it happened, the backstop would refuse agent typing (with a
  reason) rather than lose it.

## D3-f (2026-08-23): a press must never reach a pointer that has no focus

Found while chasing a user report of "after `systemctl restart
duduclaw-kiosk` the Home 交辦欄 takes clicks and nothing happens". Tracking:
`commercial/docs/TODO-agent-first-os-2026-08.md`, rows `D3-f/*`.

### The defect

A `wl_pointer` client only learns where the pointer is from an `enter`, and
smithay emits one only from `PointerHandle::motion`. Until this round the
two `InputEvent::PointerMotion*` arms in `src/input.rs` were the **only**
call sites — nothing placed the pointer at startup. So between comp coming
up and the first time the pointer physically MOVED:

* `PointerHandle` had no focused surface,
* `PointerHandle::button` therefore had nowhere to deliver a press,
* and comp's own click-to-focus still ran, so `focus: activation set`
  appeared in the journal on every swallowed click.

From the outside that reads as a healthy compositor in front of a shell that
ignores the mouse — which is exactly how it was reported.

It is not a VM artefact. An absolute-positioning device (touchscreen, KVM,
QEMU's `usb-tablet`) emits no motion at all when the tap lands where the
pointer already is, so the first tap after every restart is dead by
construction. A relative mouse hides it behind the jitter of picking the
mouse up.

### The fix

`DuduclawComp::ensure_pointer_focus(time)`, called at the very top of the
`InputEvent::PointerButton` arm before any routing: if the human pointer has
no `current_focus()`, synthesise one `pointer.motion` at its current
(clamped) location so the surface under it gets its `enter`. Idempotent, one
comparison per press on the healthy path, and no behaviour change once the
pointer has ever moved.

### Live evidence (VM, arm64 udev backend)

Staged: pointer parked on the composer, `systemctl restart duduclaw-kiosk`,
then the first input after the compositor came up was a **bare QMP button
with no motion event whatsoever**.

| | before | after |
|---|---|---|
| shell log | *(nothing at all)* | `[probe] os mouse_down at Point { x: 0px, y: 0px }` then `[hit] backdrop -> close overlay` |
| comp log | `focus: activation set target=…18…` | same |

Plus three full cycles of "click 交辦欄 → Launcher opens → 注音 su3 → candidate
window → Escape ×3 → back to Home", all green.

### Observation, not fixed: libinput's initial device batch is drained late

Across three restarts, `Initializing a libinput backend` was followed by
`New device "event0/1/2"` only **24–47 seconds later** — always at the exact
moment of the first real human input, never on a timer. The queued
`DEVICE_ADDED` events sit unread until the fd next becomes readable, i.e.
until someone actually types or moves. Delivery of that first event is not
lost (the `codrive: human input observed` line fires with the correct kind),
so this is not the click-swallowing bug above — but per-device libinput
configuration is not applied until then, and the log reads as if the machine
had no input devices for the first minute. Left as a known gap rather than
guessed at.

## D3-f2 (2026-08-23): the enter has to carry the RIGHT coordinates

D3-f above shipped, and clicks still hit nothing. Tracking:
`commercial/docs/TODO-agent-first-os-2026-08.md`, row `D3-f2/P0-1b`.

### What D3-f actually verified

Its "live evidence" table records the shell reporting
`[probe] os mouse_down at Point { x: 0px, y: 0px }` — and scores that a
PASS. The press had arrived, which was the thing being fixed, so the round
closed there. But the tablet was parked at (639, 226): every click in the
session was being delivered to the top-left corner, so no UI element could
ever be hit. The verification checked *arrival* and never checked *where*.

### The defect

`ensure_pointer_focus` built its synthesised motion from
`PointerHandle::current_location()`. The precondition for that function
running is "no motion has ever arrived", and a pointer that has never been
moved sits at smithay's `(0, 0)` default — so the one value it had to get
right was guaranteed to be wrong. It is not a rounding error or an offset:
it is the origin, every time, until something moves.

Measured on the appliance VM (arm64 udev backend), three independent
vantage points on the same click:

| vantage | reading |
|---|---|
| kernel, `EVIOCGABS` on `/dev/input/event1` | `ABS_X` 16357/32767, `ABS_Y` 9256/32767 = **(639.0, 226.0)** |
| comp, `WAYLAND_DEBUG=1` server side | `-> wl_pointer@93.enter(2, wl_surface@18[0], 0.0000, 0.0000)` |
| shell, `DUDUCLAW_SHELL_DIAG=1` | `[probe] os mouse_down at Point { x: 0px, y: 0px }` → `[hit] backdrop -> close overlay` |

The third row is the user-visible bug in one line: the pointer was sitting
on the Launcher's search field, and the click closed the Launcher as if it
had landed on the backdrop behind it.

Not a client bug — the pinned gpui rev handles `enter` correctly
(`gpui_linux/src/linux/wayland/client.rs` sets `mouse_location` from
`surface_x`/`surface_y` and even synthesises a `MouseMove` from it). It was
faithfully rendering the coordinates comp sent.

### The fix

`src/abs_pointer.rs` (new) — ask the kernel instead of guessing.
`EVIOCGABS(ABS_X)`/`EVIOCGABS(ABS_Y)` return an absolute device's *current*
axis values; the kernel keeps them because that is exactly what it compares
against to decide an unchanged `EV_ABS` event is redundant and drop it —
the same mechanism that causes this bug supplies its cure. Normalisation
divides by `maximum - minimum + 1`, matching libinput's own
`scale_axis`/`absinfo_range`, so a synthesised position and a real
`PointerMotionAbsolute` at the same device value produce bit-identical
logical coordinates (verified below).

**Getting at the fd without widening privilege.** comp runs as
`duduclaw-kiosk` (uid 999, groups `video`+`render`); `/dev/input/event*` is
`root:input 0660` — measured in the VM, not assumed. A direct `open()` is
`EACCES`; input devices reach comp only through seatd. So
`RecordingInterface` wraps whatever `LibinputInterface` the backend passes
to `Libinput::new_with_udev` and keeps a `dup()` of every fd
`open_restricted` returns, dropping it again on `close_restricted`. It
opens nothing itself, so it cannot widen what this process may touch, and
it needs no image change, no `input` group, and no second seatd round trip.

Two injection points, both fail-open to the pre-D3-f2 behaviour:

* `InputEvent::DeviceAdded` (pointer-capability devices only) →
  `seed_absolute_pointer_position`. This is what makes the *first frame*
  honest: before it, comp drew its cursor at the origin until something was
  pressed and then teleported it. Guarded by `pointer_motion_seen` so a
  device hot-plugged into a session already in use can never drag the
  cursor somewhere the user did not put it. Deliberately does **not** call
  `on_human_input` — a device appearing is not somebody touching it, and
  treating it as such would freeze the agent seat every time a keyboard is
  plugged in.
* `ensure_pointer_focus` → same lookup at press time, as the backstop for
  the case where the seeding could not run (no output yet when the device
  arrived).

Touch devices are excluded by the `DeviceCapability::Pointer` guard: their
ABS axes hold the last *touch* point, which is not where a cursor should
be, and this compositor still has no touch arm at all.

### Live evidence (VM, arm64 udev backend)

Same VM, same parked tablet, old binary vs new. `d3f2-*.png` screenshots
under `appliance/.vm/`.

| | before (D3-f binary) | after (D3-f2 binary) |
|---|---|---|
| bare button at (639, 226) | `enter(…, 0.0000, 0.0000)`, shell `{ x: 0px, y: 0px }` | `enter(…, 638.9453, 225.9766)`, shell `{ x: 638.9453px, y: 225.97656px }` |
| bare button at (1150, 700) | — | shell `{ x: 1149.9609px, y: 699.97266px }` → `[hit] backdrop -> close overlay` |
| cold-boot Home, tablet on the 交辦欄, first input is a bare button | nothing happens (`d3f2-OLD-4-bare-click-on-composer.png`) | **Launcher opens** (`d3f2-NEW-2-after-bare-click.png`) |
| cursor drawn at boot | origin (`d3f2-before-cursor.png`) | the tablet's real position (`d3f2-D-home-closed.png`) |
| move → click, unchanged path | — | same click at (639, 226) reports `638.9453 / 225.97656` — **the identical value the synthesised path produces**, `[hit] composer -> open Launcher` |

Worst error across the rounds: 0.06 px. That last row is the real proof
that `normalize` agrees with libinput rather than merely being close: the
synthesised enter and a genuine motion event land on the same float.

Container: `cargo build`, `cargo clippy --all-targets -- -D warnings`
clean, `cargo test` **434 passed** (9 new in `abs_pointer`, covering the
ioctl request numbers against the values a live `fcntl.ioctl` in the guest
accepted, libinput's inclusive-range denominator, degenerate/out-of-range
axes refusing to answer, whole-basename device matching, and the
record/forget lifecycle).

### Not fixed, observed

`DUDUCLAW_SHELL_DIAG=1` makes the shell dispatch `ToggleLauncher` once at
boot (`[action] ToggleLauncher fired` with no preceding `[probe] os
key_down`), so the Launcher is already open before any input. A/B'd against
the **old** comp binary: it happens there too, so it is a shell-side DIAG
behaviour, not a D3-f2 regression. It only shows up with DIAG on — a
DIAG-off boot lands on a clean Home (`d3f2-NEW-1-boot.png`). Left alone;
noted because it will confuse the next person staging a click round.

---

## E1a-1 (2026-08-23): the agent seat is invisible to every client except the shell

Fixes the E1a ship-blocker — **a third-party app receives no input at all** —
by generalising D3-c's per-client global filter. Design decision on record:
「seat 修法＝複用 D3-c per-client filter（對非殼 client 隱藏 agent seat）」.

### The defect, and why ordering could never fix it

`duduclaw-comp` advertises two `wl_seat` globals. Two real clients keep
exactly one seat each, and they disagree about which:

| client | keeps | under `AgentFirst` (default) | under `HumanFirst` |
|---|---|---|---|
| `duduclaw-shell` (gpui) | the **last** seat | human seat — works | agent seat — Enter dead |
| Chromium 151 | the **first** seat | agent seat — **all input dead** | human seat — works |

Measured on the appliance VM (E1a, three reproductions, fcitx5 excluded as a
cause): under the shipped `AgentFirst` order Chromium's hamburger menu did
nothing, text fields never focused, Ctrl+T was inert. `seat_order.rs` called
the first-seat-wins hazard *theoretical*; it is not, and that module's doc
now says so.

No value of `DUDUCLAW_COMP_SEAT_ORDER` satisfies both clients. Visibility
does: a client that only ever sees **one** seat cannot pick the wrong one,
whichever end of the registry list it picks from.

### What landed

`src/ime/seat_filter.rs` (still in `ime/` — it also owns the IME-manager
gate) now owns the whole per-client `wl_seat` visibility policy, as one pure
function over an accept-time classification:

```rust
pub fn agent_seat_visible_to(class: ClientClass) -> bool {
    class.allow_listed && !class.is_input_method
}
```

* **Allow list** — `/proc/<pid>/comm` exact match (never substring, repo
  convention 2) against `DUDUCLAW_COMP_AGENT_SEAT_PROCS`, default
  `duduclaw-shell`. The shell keeps both seats because `AgentFirst` exists
  for it and that pairing is what Shell-S0…S3 verified on hardware.
* **D3-c stays un-weakenable** — allow-listing an input method still refuses
  it the agent seat. Getting D3-c wrong costs silent keystroke loss; getting
  the new rule wrong costs a loudly reported dropped injection.
* **Fail-closed on the agent-seat axis** — unreadable credentials or
  `/proc/<pid>/comm` ⇒ human seat only. That direction costs codrive's reach
  into an unidentifiable client; the other direction costs that client its
  human input, i.e. the blocker itself.
* **Knobs** — `DUDUCLAW_COMP_AGENT_SEAT_PROCS` (allow list; empty value hides
  the agent seat from everyone), `DUDUCLAW_COMP_SEAT_FILTER=off` (whole
  filter off, restoring the measured-broken exposure — debugging only).
  Anything not an explicit "off" leaves the filter on, including a typo.
* **Disarm** — the startup self-check is unchanged (it re-runs the `Debug`
  seat-name extraction over the two real seats). On failure the filter
  disarms to "everyone sees everything" and the error log now names *both*
  consequences, the Chromium blackout and the fcitx5 grab.

### The cost, verified rather than assumed

The task brief hypothesised that agent input is synthesised compositor-side
and does not depend on the client binding the agent seat. **That is false.**
smithay routes seat events through the client's own resources —
`KeyboardTarget::key` → `for_each_focused_kbds` → `KeyboardHandle::known_kbds`
(`smithay-0.7.0/src/wayland/seat/keyboard.rs:143`), `PointerTarget` →
`for_each_focused_pointer` → `known_pointers` (`pointer.rs:222`) — so a client
that never received the agent seat's `wl_registry.global` never created a
`wl_keyboard`/`wl_pointer` on it and an injected key reaches **nobody**.

So the filter is paired with an explicit failure in `handle_agent_inject`
(same doctrine as `paused_by_ime`): `agent_delivery_target` resolves the
client a `text` / `key` / `key_name` / `button` op would deliver to, and if
the filter hides the agent seat from it the op is **dropped and audited**
(`inject_dropped`, `detail: "unreachable_client: <comm> …"`) instead of being
recorded as `inject_applied` while going nowhere. `move` is deliberately
exempt (the compositor-drawn agent cursor still moves — a real effect), and
an unresolvable target fails open, i.e. behaves exactly as before.

### Live verification (2026-08-23, nested container stack)

Same three-layer stack as the D3-c round — `weston --backend=headless` →
`duduclaw-comp` → real clients — with `wayland-info` copied to differently
**named** binaries so `/proc/<pid>/comm` drives the classification, and
`foot` under `WAYLAND_DEBUG=1` as the protocol witness. Script kept out of
the repo (scratchpad); reproduce with `wayland-utils` + `foot` + `python3`
added to the live-run apt list. **10/10 PASS**:

| # | check | evidence |
|---|---|---|
| 1 | a general client sees one seat | `genericapp` → `[winit]` |
| 2 | the shell sees both, agent first | `duduclaw-shell` → `[duduclaw-agent, winit]` (order intact) |
| 3 | an input method sees one seat | `fcitx5` → `[winit]` |
| 4 | codrive into a hidden-seat client is dropped, not lost | `inject_dropped … "unreachable_client: foot does not see the agent seat (E1a-1 seat filter)"` |
| 5 | …and that client really got nothing | foot's own trace: `0` `wl_keyboard.key` events |
| 6 | an allow-listed client sees both seats | `DUDUCLAW_COMP_AGENT_SEAT_PROCS=…,genericapp` → `[duduclaw-agent, winit]` |
| 7 | …binds both | `wl_seat@11.name("duduclaw-agent")` → `get_keyboard(wl_keyboard@22)`; `wl_seat@13.name("winit")` → `get_keyboard(wl_keyboard@25)` |
| 8 | …and agent keys really land on it | `wl_keyboard@22.enter(…)` then `.key(…,35,1) .key(…,35,0) .key(…,23,1) .key(…,23,0)` — evdev 35/23 = `h`/`i`, i.e. the injected `"hi"`, on the **agent** seat's keyboard |
| 9 | D3-c is not weakenable | `DUDUCLAW_COMP_AGENT_SEAT_PROCS=…,fcitx5` → `fcitx5` still `[winit]` |
| 10 | the kill switch works | `DUDUCLAW_COMP_SEAT_FILTER=off` → `genericapp` sees both, with the warn log |

Row 7+8 are the load-bearing pair: `foot` is multi-seat-aware and binds
*both* seats, which is precisely why hiding one from it is what makes codrive
unreachable — and why the drop in row 4 is a real behaviour change, not a
theoretical one.

Container: `cargo test` **491 passed** (487 + 4: the visibility truth table
including the un-weakenable-D3-c row, the fail-closed default, the
allow-list default's 15-byte `comm` budget, and the kill-switch parser),
`cargo clippy --all-targets -- -D warnings` clean.

### Not verified here (needs the VM / real hardware)

* **Chromium under the armed filter** — the actual blocker. The container has
  no Chromium; row 1 proves the registry now advertises one seat to a
  non-allow-listed client, which is the mechanism, but the end-to-end "the
  hamburger menu responds" observation is a VM step.
* **The shell under the armed filter** — row 2 proves the shell still gets
  both seats in the same order, so nothing about its configuration changed;
  a gpui boot round on hardware is still the honest confirmation.
* **Real fcitx5** — row 3/9 use a binary *named* `fcitx5`, which exercises the
  classification and the visibility decision but not fcitx5's own
  `refreshSeat()` loop.

### Open decision this round surfaced

With the filter armed, **codrive can no longer drive any third-party app**
(only the shell, which it does not drive anyway). Three ways out, none taken
here because all three are policy calls:

1. Ship an allow list of known co-drive targets (`chromium`, …) — restores
   codrive there, and re-exposes exactly those apps to the single-seat
   hazard. Free today: it is an env var.
2. Synthesise agent input through the **human** seat when the target cannot
   see the agent seat — the mechanism the brief assumed already existed.
   Works for every client, but breaks DESIGN §6's red line that agent input
   travels through a seat object distinct from the human's.
3. Accept the loss until gpui gains multi-seat support upstream, at which
   point the shell needs no exemption and no ordering workaround either.

## E1a-1a (2026-08-24): driving a client that cannot see the agent seat — human-seat synthesis

Answers the open decision E1a-1 left on the table (that section's last block):
with the seat filter armed, co-drive could reach no third-party app at all.
User decision 2026-08-24, option **(b)**: when the target cannot see the agent
seat, synthesise the event on the **human** seat, which every client can see.
Red-line review before implementation is
`commercial/docs/DESIGN-codrive-desktop-2026-08.md` §6.1.

### What the review changed about the plan

Option (b) touches DESIGN §3.3.1 ("事件源頭天然歸因…全部以 seat 為單位"), so the
review went red line by red line. Two findings changed the implementation:

1. **A real hole nobody had named: modifier residue.** `KeyboardHandle::input`
   updates the *seat's* xkb modifier state. Synthesising a bare Logo-down on
   the human seat leaves `modifiers.logo == true` for the next **genuine**
   human key — so a plain Escape would fire the emergency stop and a plain
   Enter the hand-back. That is §6 red line 3 ("急停鍵永遠有效，agent 不可攔截")
   defeated indirectly, by remote-controlling a human-only gesture rather than
   forging it. Logo and Alt are therefore refused outright on the synthesis
   path. `key_name`'s table contains no modifier and `text` only uses Shift, so
   only a raw `key` can carry one.

2. **The shadow workspace and the human seat are mutually exclusive.** A
   shadow session is separate state from the freeze flag (`shadow.rs` module
   doc), so it can be live while nothing is frozen — and the human seat's
   pointer and keyboard focus are on the MAIN output. Mirroring a
   shadow-confined command onto them would deliver the agent's keystrokes into
   whatever window the human is actually using and drag the human's cursor
   toward the shadow origin `(0, 100000)`, i.e. off-screen: a direct breach of
   DESIGN §3.1 rule 2 ("與人的桌面零交集"). Synthesis is therefore refused for
   the whole duration of a shadow session, using the mode flag rather than a
   per-command "is this target really on the shadow output" test — the latter
   would have to fail *open* to be useful, and failing open is the wrong
   direction for a cross-domain leak.

3. **The freeze is the main defence, but it has two documented gaps.** Any
   human input freezes the agent, so human/agent events cannot normally
   interleave. Two paths deliberately do *not* freeze: `codrive_try_watch_
   resume` (a watch-idle pause is lifted by presence, returning before the
   freeze) and `input.rs::is_system_gesture_tail` (the Super+Enter chord tail,
   the CD-2 real-hardware fix). Both leave a live seat with a human's hands on
   it. A synthesis-only quiet window closes them.

### What landed

`src/codrive/human_seat.rs` — the whole policy as one pure function, same
shape as `seat_filter::agent_seat_visible_to` and
`shadow::freeze_bypass_decision`:

```rust
pub fn route_inject(kind: OpKind, target_hidden: Option<bool>, env: &SynthesisEnv)
    -> InjectRouting  // { mirror_to_human_seat: bool, drop_with: Option<RefuseReason> }
```

* **Additive, never a replacement.** The agent-seat path runs first and
  unchanged (amber cursor, agent-seat focus bookkeeping, every existing audit
  line); the mirror is emitted afterwards. That is what keeps a following
  `text` op resolving to the same target.
* **Refusal reasons**, each its own audit `detail` prefix:
  `unreachable_client_synth_disabled` (kill switch), `shadow_active`,
  `human_active`, `paused_by_ime_human_seat`, `no_human_focus`,
  `gesture_modifier`.
* **`move` is never dropped**, keeping E1a-1's exemption: a blocked synthesis
  degrades to "the agent cursor still moves, the human pointer is not dragged",
  not to a drop.
* **Distinct audit kind** `inject_via_human_seat` (detail
  `synthesized_via=human_seat; target=<comm>`) rather than a tagged
  `inject_applied`, so existing counts keep meaning "delivered on the agent
  seat".
* **Quiet window is borrowed, not invented.** `HUMAN_ACTIVE_WINDOW =
  watch::MIN_WATCH_IDLE_SECS` (5s) — this crate has no "freeze window"
  constant (`codrive_freeze_set_at` is a timestamp; DESIGN §5's `<50ms` is a
  latency target), and that constant already means "the shortest silence we
  will call 'nobody is there'".
* **Self-freeze guard.** `DuduclawComp::codrive_synthesizing` is set around
  every mirror; `on_human_input` checks it first and, if set, warns + records
  `synthesis_reentry_ignored` and returns. Unreachable today (the emission
  helpers call `Seat` APIs directly, never `process_input_event`) — it exists
  so a future refactor fails loudly instead of live-locking the agent
  (inject → freeze itself → drop) or forging "a human is present" and
  permanently disarming watch-mode idle auto-pause.
* **Knob** — `DUDUCLAW_COMP_CODRIVE_HUMAN_SEAT_SYNTH=off` restores E1a-1's
  drop exactly. Anything else leaves it on, matching
  `DUDUCLAW_COMP_SEAT_FILTER`'s "a typo lands on the shipped side".

`agent_delivery_target` gained a `Move` arm (the destination client, not the
current one). Under E1a-1 it fed only the drop decision, where `Move` had to
be absent; it now also feeds the mirror decision, where `Move` matters —
a synthesised click lands wherever the human pointer already is.

### Two behaviour changes, disclosed rather than hidden

* The human's **pointer moves** during synthesis (without it a synthesised
  click lands at the human's last cursor position and hover-driven UI never
  responds).
* The human's **keyboard focus changes** when a synthesised click lands
  (click-to-focus is per-seat).

Both follow from "the agent is driving the human's desktop"; both are
consistent with the mutual exclusion the freeze already enforces. A third
consequence is irreducible and is written into DESIGN §6.1.1 instead of being
argued away: **the client cannot tell a synthesised event from a human one**,
so an app's own log attributes it to the user — the same property RDP,
`xdotool` and `wtype` have. comp-side attribution (audit kind + `comm`) is
unaffected.

### Live verification (2026-08-24, nested container stack)

Same three-layer rig as E1a-1/D3-c — `weston --backend=headless-backend.so` →
`duduclaw-comp` (winit) → `foot` under `WAYLAND_DEBUG=1` — plus real `fcitx5`
for round 4. Scripts kept out of the repo (scratchpad). **10/10 PASS**:

| # | check | evidence |
|---|---|---|
| 1 | a non-allow-listed client sees only the human seat | foot: one `wl_registry@2.global(11, "wl_seat", 9)`, `wl_seat@11.name("winit")`, `get_keyboard(wl_keyboard@19)` |
| 2 | …and synthesised text really lands on it | `wl_keyboard@19.enter(...)`, then `.key(...,35,1) .key(...,35,0) .key(...,23,1) .key(...,23,0)` — evdev 35/23 = `h`/`i`. E1a-1's baseline for the identical command was **0** key events |
| 3 | …audited as such | `inject_via_human_seat`, `detail: "synthesized_via=human_seat; target=foot"` for `move`/`button`/`button`/`text` |
| 4 | sustained synthesis never self-freezes | 30 consecutive `text` ops → 30 `inject_via_human_seat`, **0** `freeze`, **0** `synthesis_reentry_ignored`, `status` still `frozen:false` |
| 5 | a human touch refuses synthesis, and `move` still is not dropped | `simulate_human` → `simulate_super_enter` → immediate inject: `text`/`button` → `inject_dropped … "human_active: …"`; `move` → `inject_applied`. After 6s the same `text` → `inject_via_human_seat` |
| 6 | Logo/Alt are refused, ordinary modifiers are not | `key` 133 (LEFTMETA) and 64 (LEFTALT) → `inject_dropped … "gesture_modifier: …"`; `key` 37 (LEFTCTRL) → `inject_via_human_seat`, so Ctrl-chords stay drivable |
| 7 | real fcitx5 grabbing the human seat refuses keyboard ops only | comp: `input-method keyboard grab changed human_seat=true agent_seat=false`; `text`/`key_name` → `inject_dropped … "paused_by_ime_human_seat: …"`; `move`/`button` → `inject_via_human_seat` |
| 8 | a shadow session never borrows the human seat | `shadow enable` → `text` → `inject_dropped … "shadow_active: …"`; after `shadow disable` the same `text` routes normally again |
| 9 | the kill switch restores E1a-1 exactly | `…HUMAN_SEAT_SYNTH=off`: `button` → `inject_dropped … "unreachable_client_synth_disabled: …"`, foot got **0** key events |
| 10 | an allow-listed client keeps the agent-seat path byte-identical | `DUDUCLAW_COMP_AGENT_SEAT_PROCS=duduclaw-shell,foot`: foot binds both seats, keys land on `wl_keyboard@22` under `wl_seat@11.name("duduclaw-agent")`, audit is plain `inject_applied` with **no** `inject_via_human_seat` |

Container: `cargo test` **522 passed** (497 + 25: the routing truth table
including every refusal reason and the `move`-never-drops rows, op
classification, the gesture-modifier table, the kill-switch parser, the audit
detail shapes, and three source-structure invariants pinning the self-freeze
guard), `cargo clippy --all-targets -- -D warnings` clean. Binary in
`appliance/.build/duduclaw-comp-linux` (previous rotated to `.prev`, old
`.prev` → `.prev7`).

### Honest gaps

* ~~**Chromium under synthesis** is still a VM step — the container has no
  Chromium.~~ **CLOSED 2026-08-24 by the A2 acceptance round** (see the "A2
  acceptance round" chapter at the end of this file): the rig image simply
  installs `chromium` and runs it as a real Wayland client of comp under Xvfb,
  and a synthesised `move`+`button`+`text` lands typed characters in the page's
  own `<input>` — audited as `inject_via_human_seat … target=chromium`, with
  the screenshot in `appliance/.vm/a2-evidence/04-typed-crop.png`. Row 1/2 here
  already proved the mechanism against `foot`; this proves it against the
  browser class of client. Still NOT claimed: that any specific web app (e.g. a
  LINE extension) behaves correctly under it.
* **The socket ack does not carry the refusal.** A dropped-at-the-main-thread
  command still answers `{"ok":true}` on the socket; only the audit trail
  records the drop. This is E1a-1's shape carried forward, not new — but it
  means the gateway driver cannot yet see a `human_active` refusal in its
  reply. `paused_by_ime` (agent seat) is the one op-level pre-rejection with
  an `ok:false` today; extending that to the E1a-1a reasons needs the socket
  thread to mirror the human-seat state and is deliberately not done here.
* **A synthesised non-gesture modifier can still be left held** (e.g. `key`
  Ctrl-down with no matching up) and would then affect a later human keystroke
  on that seat. It cannot reach a compositor gesture (those all need Logo or
  Alt), and any human input freezes the agent, so the window is small — but it
  is a real residue and is recorded in DESIGN §6.1.2 rather than fixed by
  tracking synthesised key state.
* **The three self-freeze pins are source-structure tests.** The property is
  structural ("no code path from a synthesised event into the human-input
  observer"), so there is no value to assert on, and a runtime test would need
  a live compositor with a GL context, which this suite does not build.

## A2 (2026-08-24): the driving-mode state machine — who is holding the wheel

CD-0 through CD-3 built every mechanism a co-drive session needs — a
token-authenticated injection socket, freeze-on-human-input, `take_over`,
watch-mode idle pause, Super+Esc — but never gave any of it a **name a human
could see**. The compositor knew `frozen`, `terminated`, `takeover_active`;
nobody outside it could ask the one question that matters when an agent shares
your screen: *who is driving right now?* A2 answers that, on both sockets and
on the screen itself.

### The three modes, and why the mode is DERIVED

`codrive/mode.rs` (new). One pure function, no second state machine:

```
derive_mode(session_active, terminated, frozen):
    !session_active || terminated -> Human
    frozen                        -> Handover
    otherwise                     -> CoDrive
```

| mode | meaning |
|---|---|
| `human` | no co-drive session, or it was emergency-stopped. Zero agent driving authority |
| `codrive` | authenticated session, agent seat not frozen — the agent drives, the human watches |
| `handover` | session alive but agent seat frozen — the human holds the wheel. A pause, not a stop |

The ordering is load-bearing and is pinned by its own test. An emergency stop
deliberately leaves `frozen` latched `true` (§6 red line 3 — a fresh connection
must not clear it), so a "frozen first" reading would report `handover` for a
desktop with no session to hand anything back to. The full 2³ truth table is
asserted exhaustively (`derive_mode_truth_table_is_exhaustive`).

Nothing stores the mode as an independently-mutated field.
`DuduclawComp::codrive_mode` is a `CodriveModeCache` whose only job is letting
`codrive_sync_mode` tell a real transition from a per-frame no-op; every
*reader* (both backends, both sockets) re-derives. **Shadow is deliberately not
a fourth mode** — while the agent works on the CD-2 shadow output the human's
own desktop is still theirs, so the mode stays `human` and the status block
reports `shadow: true` beside it.

### `session_active` — the flag that did not exist

`derive_mode`'s first input had no representation. `CodriveShared::active_conn`
implied it, but it is a `Mutex<Option<UnixStream>>` and both backends would have
had to take that lock once per composited frame to colour a cursor. So
`CodriveShared` gained `session_active: AtomicBool`, written in lockstep with
`active_conn` at all three sites: `listener.rs`'s post-auth publish, its
connection-teardown cleanup, and `mod.rs`'s `emergency_stop`. Same mirror
discipline `shadow_active`/`takeover_active` already followed; `watch_active`,
`watch_paused` and an `AtomicU8` `handover_reason` joined for the same reason
(the `status` op must answer without a main-thread round trip, even
mid-takeover).

**It is set only past the auth gate**, and that is the round's red-line
regression test: `unauthenticated_connection_does_not_set_session_active`
mirrors CD-1's `…does_not_clear_terminated`. Without it, anything that could
open the socket could make the compositor paint an amber "AI 駕駛中" frame
around a screen no agent was driving.

### `handover_reason` is recorded at the trigger, never inferred

Four triggers, a closed enum, `human_input` / `agent_take_over` / `watch_idle` /
`shell_take_wheel`. Each site records the reason *before* it freezes;
`codrive_sync_mode` consumes it when the derived mode actually becomes
`handover`, and a hint that never produced one is discarded rather than left to
mislabel a later, unrelated handover. Inferring it afterwards from flag shapes
was rejected outright: a watch-idle pause and a human touch leave **identical**
flags, so a guess would put a confident wrong answer in an audit trail.

**One observed consequence, from the acceptance round (2026-08-24).** A freeze
that happens while there is no session yet — the winit backend emits a synthetic
absolute pointer motion at startup, so a nested comp is frozen before anything
connects — is recorded in `human` mode, where `codrive_sync_mode` discards the
hint. If a session then authenticates while that freeze is still standing, the
`human → handover` transition carries `reason=none`, seen live as
`driving_mode detail="from=human; to=handover; reason=none"`. That is the
discard rule working as written (honest silence over a guess), not a defect, and
the acceptance run's own steps all pass a real Super+Enter first. Whether a
still-standing freeze should keep its reason across a session boundary — i.e.
scope the reason to the FREEZE rather than to the HANDOVER — is a genuine design
question, and deliberately not answered here.

### What is additive, and what is byte-identical

* Codrive `{"op":"status"}` gained `mode` / `handover_reason` / `shadow` /
  `watch_active` / `watch_paused`. **The pre-A2 `frozen` / `terminated` /
  `takeover` keep their exact spelling and position** — `mode::status_reply_line`
  is the single formatter and `status_reply_keeps_the_pre_a2_three_fields_first_and_verbatim`
  pins the prefix byte-for-byte, because the gateway's shipped client parses it.
* A new push event `{"event":"driving_mode","mode":…,"reason":…}`, emitted once
  per real transition. No existing event was renamed, replaced or removed.
* A new audit kind `driving_mode`, detail `from=<a>; to=<b>; reason=<r>`
  (`reason=none` outside handover). Every existing kind is untouched — a
  shell-driven `take_wheel` still writes the ordinary `freeze` line (with
  `op=shell_take_wheel`) rather than inventing a parallel vocabulary that would
  split every existing freeze query in two.

### The human side: `codrive_status` / `codrive_drive`

Two ops on the shell-control socket (`shell_control/codrive_ops.rs`, new).
`codrive_status` is a READ (unaudited, like `list_windows`); `codrive_drive` is
an ACTION (always audited) taking a closed `take_wheel` / `hand_back` set —
refused with `invalid_codrive_action`, never coerced, and the error token never
echoes the caller's string.

`take_wheel` is **not** routed through `on_human_input`, even though the freeze
it performs is the same. `on_human_input`'s first act is to treat any human
event as proof of presence and *lift* a watch-mode idle pause — which would
have turned the button into an un-freeze in exactly the situation (nobody was
watching) where a person is most likely to press it. `hand_back` **is**
`human_resume()`, reused rather than reimplemented so the shadow hand-back,
takeover teardown and watch-pause clearing cannot drift from the Super+Enter
path. Both refresh `codrive_last_human_activity`: a person clicking a button is
human presence, and the keyboard path gets that for free from the key event
itself.

**Trust boundary, stated without hedging.** On the appliance the gateway runs
`User=duduclaw` and the kiosk session runs `User=duduclaw-kiosk` (read from the
two unit files, not assumed), and this socket authenticates by same-uid
`SO_PEERCRED` — an agent process structurally cannot open it. A same-uid
development machine has no such protection. Two things follow: `codrive_drive`
is shaped so the dangerous direction does not exist (`take_wheel` only ever
stops the agent; `hand_back` adds no path an agent did not already have, and the
codrive socket's own `resume` stays unconditionally denied), and **Super+Esc
remains the only stop that is structurally unreachable by the agent** — detected
in the compositor's own human keyboard filter, which no injected event enters.
That red line is unchanged.

### On screen

* **`build_agent_cursor_elements` now takes a `DrivingMode`, not a `bool` — and
  `Human` draws NOTHING. This is a behavior change.** Before A2 the agent cross
  was composited unconditionally, so a desktop with no session at all (or one
  just emergency-stopped) still carried an agent pointer parked wherever the
  last session left it. That is a lie in the most load-bearing possible place:
  the element exists to say "something other than you can move a pointer right
  now", and with no session nothing can. `is_frozen()` alone could never express
  this — it cannot tell "frozen because a human touched it" from "frozen and the
  session is gone".
* **Ghost styling**: a near-black halo 2 px larger on each side (α 0.35) behind
  a core cross at α 0.70, replacing the flat opaque cross. Core elements are
  pushed FIRST because this crate's backends treat earlier custom elements as
  nearer the viewer — pushed the other way round, the halo (which fully contains
  the core) would hide the very cross it outlines. Still
  `SolidColorRenderElement` only, zero new dependencies.
* **`codrive/mode_indicator.rs`** (new): four 3 px edge bars framing each
  output, amber in `codrive`, dark red in `handover`, absent in `human`. It takes
  **no output offset**, unlike the highlight box: a highlight lives in the global
  `Space` coordinate system and must be translated into the output being
  rendered, whereas this frame is defined against the output's own origin and
  mode size — which is already the space custom elements are interpreted in.
  Applying `-output.loc` here would push a second monitor's frame off its own
  screen. `decor::paint::build_output_elements` keeps custom elements ahead of
  every window and layer surface, so a fullscreen client cannot paint over it.

### The per-frame reconciliation

`codrive_sync_mode()` runs beside the existing `codrive_check_watch_idle` call
in the winit redraw arm, in `render_surface`, and in the udev housekeeping tick.
That is not belt-and-braces: it is **the only place the main thread can observe
the socket thread flipping `session_active`**. A connection arriving or dropping
is not a main-thread event at all, so without this hook a session start or end
would never produce a `driving_mode` line, a push event, or the redraw that
paints (or removes) the frame. On the udev backend the 1 Hz housekeeping tick is
the only clock an idle desktop has. No change ⇒ true no-op: no audit line, no
event, no redraw.

### File-size debt paid and incurred

`codrive/mod.rs` was already at 920 lines (over this project's 800-line cap)
before A2, and A2 had to add transition calls to three of its functions. Its own
`#[cfg(test)] mod tests` block moved to `codrive/tests_token.rs` verbatim — the
same split `tests_listener.rs`/`tests_takeover.rs` already established — leaving
it at 924, +4 net. All new A2 logic went into new files (`codrive/mode.rs`,
`codrive/mode_indicator.rs`, `shell_control/codrive_ops.rs`) using second `impl
DuduclawComp` blocks rather than growing `mod.rs` or `shell_control/mod.rs`
further. A2's `shell_control::listener::validate` tests live in
`codrive_ops.rs` with the rest of A2's shell-side tests for the same reason.

### Honest gaps

* **Unit tests only.** Every pure function here is tested (the 2³ truth table,
  both wire tokens, the `AtomicU8` round trip and its unknown-byte fallback, the
  byte-exact status line and event line, the indicator geometry including
  degenerate outputs, the action parser's near-miss refusals, and every response
  shape). Nothing here has been exercised against a live compositor — the ghost
  cursor, the edge frame and the end-to-end socket round trips are all
  acceptance-side live-run work, per this crate's standing "pure logic
  unit-tested, seat/space state live-run tested" split.
* **`take_wheel` while already frozen is a no-op transition**, so the audit
  trail records the *first* handover's reason, not the button press that
  re-asserted it. That matches `on_human_input`'s own long-standing idempotency
  and was not changed here.
* **The gateway client is not updated by this round** (contract §6 is the
  gateway package's work) — and the skew runs in exactly ONE direction, checked
  rather than assumed. **Old client / new comp is already safe**: neither
  `duduclaw-gateway::codrive::client::CodriveAck` nor the shell's
  `CodriveState` carries `deny_unknown_fields` (grepped, 2026-08-24), and a
  derived `Deserialize` ignores fields it does not know, so a pre-A2 client
  reading the widened reply simply does not see the new keys. **New client /
  old comp is the one that needs care**: a client compiled against A2's shape
  reading a pre-A2 comp gets *absent* keys, which is why every new field on
  both clients is `#[serde(default)]`. Do not add `deny_unknown_fields` to
  either ack type — it would convert the safe direction into a hard error the
  moment comp gains its next field.

### A2 acceptance round (2026-08-24, acceptance side) — Xvfb + real Chromium

The live half the implementation round left open. **Not the nested-weston rig
every earlier co-drive round used**, and the swap is the point: `Xvfb :99` +
the winit backend gives `import -window root` a real screenshot of comp's own
composited output (the CUR-1 round established this route), `xdotool` drives
**real X input through XTEST** — i.e. `input.rs::process_input_event`, not the
`DUDUCLAW_CODRIVE_DEBUG_STDIN` simulator — and `chromium
--ozone-platform=wayland` is a genuine third-party client that the E1a-1
seat filter hides the agent seat from. So one container proves the pixels, the
real-input freeze path, and the E1a-1a synthesis chain at once.

Rig: `rust:bookworm` + `xvfb xdotool imagemagick x11-utils libxkbcommon-x11-0
libxcb-xkb1 adwaita-icon-theme foot chromium`, everything run as a non-root
user (chromium refuses root). Scripts stayed in the scratchpad, not the repo.

**11/11 PASS**, each row an audit line + a `status` reply + a screenshot:

| # | check | evidence |
|---|---|---|
| 1 | no session ⇒ `human`, and **nothing is drawn** | `codrive_status` → `mode:human, session_active:false`; edge probe: page background `#202028`, no frame |
| 2 | an authenticated connection ⇒ `codrive` | push `{"event":"driving_mode","mode":"codrive"}`; `driving_mode from=human; to=codrive`; edge probe **`#FFA103` amber on all four edges** |
| 3 | `watch enable` is orthogonal, not a mode | `watch_active:true` with `mode` still `codrive` |
| 4 | the AI really drives real Chromium | 4× `inject_via_human_seat … target=chromium`; the page's `<input>` shows the typed string in the screenshot |
| 5 | a real human mouse move ⇒ `handover(human_input)` | `freeze op=pointer_motion_absolute` → `driving_mode from=codrive; to=handover; reason=human_input`; edge probe flips to **`#AB2425` dark red** |
| 6 | the SHELL BUTTON hands back (not Super+Enter) | `codrive_drive{action:hand_back}` → `resumed` + `driving_mode → codrive`; amber returns |
| 7 | the agent hands over itself | `take_over` → `takeover_started` + `driving_mode … reason=agent_take_over`, `takeover:true`; dark red |
| 8 | Super+Enter still works, unchanged | `resume op=human_super_enter` + `takeover_ended` → `codrive` |
| 9 | the shell takes the wheel | `freeze op=shell_take_wheel` → `driving_mode … reason=shell_take_wheel`; dark red |
| 10 | Super+Esc ⇒ `human`, session terminated | `emergency_stop detail=super+esc` → `session_ended` → `driving_mode → human`; client sees EOF; **frame gone** |
| 11 | an unknown shell action is refused | `{"ok":false,"error":"invalid_codrive_action"}` |

Colours read off the PPM, not eyeballed: `AGENT_COLOR_LIVE` composites to
`#FFA103` and `AGENT_COLOR_FROZEN` to `#AB2425` over these backgrounds, and
182/183 samples on every edge strip carry it.

#### Three rig facts worth keeping (each cost a wrong result first)

1. **comp serves ONE co-drive connection at a time.** A second client's
   `connect()` returns immediately, but its auth ACK never comes — it is
   sitting in the kernel backlog behind the live session, and it times out.
   That IS the driving-seat exclusion working; the whole acceptance timeline
   therefore rides a single stdin-fed client. First run mistook it for a bug.
2. **`xdotool mousemove` to the pointer's CURRENT position emits no X motion
   event at all.** Step 5 silently tested nothing and reported "a human touch
   did not freeze it" — a false negative that looked exactly like a real
   defect. Park the pointer somewhere known first.
3. **`xdotool key super+Return` (the compound-chord form) re-freezes the seat
   ~1 ms after the resume.** It emits a stray trailing keyboard event outside
   the Logo-held window, which `is_system_gesture_tail` correctly declines to
   exempt. Use the explicit `keydown super; key Return; keyup super` form —
   which leaves `frozen:false`, confirming this is an xdotool artefact and not
   a regression of the CD-2 real-hardware fix.

Also: `import` grabs whatever is on screen at that microsecond, and a mode
change queues a repaint rather than blocking on one, so a screenshot fired in
the same breath as a transition can catch the previous frame (seen once on
step 9). Settle before shooting; the transition itself is proven by the audit
line, the screenshot is proving the pixels follow it.

#### What this round did NOT prove

* **udev/DRM backend.** Everything above is the winit backend under Xvfb. The
  edge indicator's per-output geometry on real hardware (and on a second
  output) is untested — the udev path takes each surface's own output size and
  no offset, which is unit-tested but never rendered on a real panel.
* **The shell's own UI.** The `codrive_status` / `codrive_drive` wire shapes
  were exercised with a raw Python client; `duduclaw-shell`'s parser was
  cross-checked field-by-field against these captured bytes, but the gpui row
  itself has never been on screen.
* **The gateway↔comp round trip for `codrive_status`.** Its `CodriveClient` is
  the same one CD-1 already live-proved, and its serde shape is unit-pinned
  against these exact bytes, but no gateway process talked to this comp.

## D4b-3 / W6-5: `set_output_scale` goes live (2026-08-24)

Closes the two setters TODO's D4b-3 row flagged as "honestly refused" —
`set_output_scale` now actually applies, persists, and re-layouts; a
`set_output_mode` feasibility finding is recorded but not implemented this
round. See `shell_control/mod.rs`'s "Scale, real as of D4b-3" doc section
for the full design; this section is the live-verification evidence.

### What changed

Four render-element builders across `cursor/mod.rs`, `codrive/cursor.rs`,
`codrive/highlight.rs`, and `codrive/mode_indicator.rs` each hardcoded
`Scale::from(1.0)` (a fourth site — `mode_indicator.rs` — beyond the three
the D4b-3 TODO row originally named; it was added the same day by the A2
共駕復活 round, after that row was written). Consolidated onto
`render::output_render_scale(&Output) -> Scale<f64>` as the single source
every one of them, plus `decor/paint.rs`'s pre-existing correct one, now
reads. `shell_control_set_output_scale` (`shell_control/mod.rs`) went from
an always-refuse stub to: validate → `Output::change_current_state` (live,
backend-agnostic — no `DrmSurface`/`GbmBufferedSurface` rebuild, unlike a
mode change) → `rearrange_layers` + `reapply_window_policy_all` (the output's
LOGICAL geometry just changed even though its physical mode did not) →
persist via a new `output_prefs.rs` module (`$XDG_STATE_HOME/duduclaw-comp/
display.json`, keyed by output name, same read-modify-write/atomic-rename
discipline `cursor::persist` established) → echo the refreshed `outputs`
list. Both backends (`winit_backend.rs::init_winit`,
`udev_backend.rs::build_surfaces`) restore a persisted scale in the SAME
`change_current_state` call that sets the initial mode, so a returning
output's first-ever `wl_output.scale` announcement is already correct.

### Build/test (this round)

Container command per "Reproducible build command" above (the A4-1-current
one, with all seven system deps).

```
cargo build --locked   → Finished, no warnings
cargo clippy --locked --all-targets → Finished, zero warnings
cargo test --locked    → 600 passed; 0 failed (was 590 before this round —
                          +10: 3 direct "a real output scale changes the
                          baked geometry" regression tests against
                          `Element::geometry()`, one per SolidColorRenderElement-
                          based builder, plus 7 for the new `output_prefs`
                          module)
```

### Live verification — real running binary, real socket, real client

Same nested-headless-weston harness as the "Nested headless live-run"
section above (layer 1 `weston --backend=headless-backend.so`, layer 2 the
freshly built `duduclaw-comp` as a `winit` client, layer 3 `foot` as a real
xdg-shell client), plus a small Python client speaking the shell-control
wire protocol directly against `$XDG_RUNTIME_DIR/duduclaw-shell.sock`:

```
get_outputs                                  -> scale_pct: 100 (fresh boot)
set_output_scale(winit, 200)                 -> ok, echoed outputs scale_pct: 200
get_outputs (independent re-read)            -> scale_pct: 200  (proves the
                                                 live Output state actually
                                                 changed, not just the one
                                                 reply)
set_output_scale(winit, 125)                 -> ok, scale_pct: 125 (fractional
                                                 step, same code path)
set_output_scale(winit, 100)                 -> ok, scale_pct: 100
set_output_mode(winit, <a real known mode>)  -> {"ok":false,
                                                 "error":"mode_switch_unsupported"}
                                                 (unchanged, as designed)
```

All five calls succeeded against the SAME long-running `duduclaw-comp`
process with `foot` attached the whole time; `duduclaw-comp.log` shows no
panic/error lines across all four scale transitions (the only `error`/`panic`
grep hits are EGL/GL extension name substrings — `EGL_KHR_create_context_
no_error`, `GL_KHR_no_error` — not real failures), and the process shut down
cleanly afterward. `display.json` round-tripped each write
(`display: scale preference stored path=… output="winit" scale_pct=200`
etc. in the log).

### Honest stub / limitation list (this round)

* **Only 100%/200% independently live-verified**, per the task's own
  "整數倍先行" priority. 125/150/175% go through the exact same code path
  (`Scale::Fractional` uniformly — see `shell_control_set_output_scale`'s
  own doc) and are covered by the `geometry()`-level unit tests, but were
  not separately live-run. This crate registers `xdg_output` but no
  `wp_fractional_scale_v1` global (checked: `state.rs`'s global list), so a
  client that only understands integer `wl_output.scale` sees `ceil()` of a
  fractional value and may render slightly soft at those three steps
  specifically.
* **No pixel screenshot of the scaled frame.** `weston-screenshooter`
  requires `weston --debug` (a documented DoS/info-leak surface, deliberately
  not something to leave on) and, once enabled, this round's attempt did not
  reach a captured PNG within the container run budget. The evidence above is
  protocol-level (the live `Output` state actually changed, re-read
  independently) and geometry-math-level (direct `Element::geometry()`
  assertions against real smithay types), not a human/AI eyeballing an
  actual rendered frame at 2x.
* **`duduclaw-shell`'s own UI was not exercised.** Everything above proves
  comp's OWN render pipeline (human/agent cursor, highlight box, co-drive
  edge frame, and — via `decor/paint.rs`, unchanged this round —
  comp-drawn SSD title bars) stays in lock-step with a live scale change.
  Whether `duduclaw-shell`'s own layer-shell-drawn dock/title chrome
  visually enlarges in response (depends on gpui's own `wl_output`/
  `xdg_output` scale-factor handling, which this round did not touch or
  verify) needs a real VM run with both binaries — not completed this
  round; see the task's own "殼 GUI 全案" line for the wider status.
* **`set_output_mode` mailbox pattern is documented, not implemented.** See
  `shell_control/mod.rs`'s doc for the full finding: technically bounded
  (winit's `WinitEvent::Redraw` closure already holds `backend.window()`
  every frame; a `pending_output_mode_request` field would let
  `shell_control_set_output_mode` hand it a request instead of refusing),
  deferred because the winit backend has zero production value on the
  appliance (udev/DRM ships; see "Why Docker, not `cargo build`" above) and
  because the synchronous reply contract for a resize the host WM might not
  grant exactly needs its own design pass. udev/DRM mode-switching remains
  genuinely blocked without rebuilding `DrmSurface`/`GbmBufferedSurface`,
  unchanged from the previous round's finding.
