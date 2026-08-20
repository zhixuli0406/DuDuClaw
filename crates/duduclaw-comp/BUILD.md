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

## Why Docker, not `cargo build` on this Mac

smithay is **Linux-only**: it depends on `wayland-server`/`wayland-client`
and (via the `desktop`/keyboard input path) `libxkbcommon`, none of which
exist on macOS. This crate is deliberately **excluded from the main
DuDuClaw workspace** (root `Cargo.toml` `[workspace] exclude`) and carries
its own empty `[workspace]` table so it never touches the gateway build or
its `Cargo.lock`. Verification for this crate therefore happens inside a
Linux container, not via `cargo build` at the repo root.

## Reproducible build command (verified 2026-08-19)

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

That's the **entire** system dependency list — just `pkg-config`,
`libwayland-dev`, `libxkbcommon-dev`. No mesa/EGL headers were needed: the
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
popup grabs (`fn grab` is a documented no-op, same as upstream), XWayland,
layer-shell, DRM/libinput/udev backends, screen-copy/damage-tracking
protocols beyond what `desktop::space` provides for free.

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
  see the earlier "Honest stub" list) are unexercised.
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
