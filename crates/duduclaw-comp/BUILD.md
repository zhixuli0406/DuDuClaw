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
