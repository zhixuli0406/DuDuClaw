// Adapted from smithay's `smallvil` example (`smallvil/src/winit.rs`), MIT
// License. See `main.rs` for the full attribution note. Renamed from
// `winit.rs` to `winit_backend.rs` in this crate so the module name doesn't
// collide with the `winit` crate it wraps, and so it reads unambiguously as
// "the winit *backend*" per the task's requested module split.
//
// This is the one piece of this spike that's inherently temporary: `winit`
// runs the compositor nested inside a host Wayland/X11 session rather than
// owning a real output via DRM/KMS. That's the correct shape for a Mac-
// developed / Linux-VM-verified spike (see BUILD.md) — swapping in a real
// DRM/libinput backend is out of scope here and would need `backend_drm` +
// `backend_libinput` + `backend_udev` smithay features this crate
// deliberately doesn't enable yet.

use std::time::Duration;

use smithay::{
    backend::{
        renderer::{
            damage::OutputDamageTracker,
            gles::{GlesRenderer, GlesTexture},
        },
        winit::{self, WinitEvent},
    },
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::calloop::EventLoop,
    utils::{Point, Rectangle, Transform},
};

// A4-1: `CodriveElement` moved to `crate::render` so the udev backend can
// use it without depending on this module. Definition unchanged.
use crate::{render::CodriveElement, CalloopData, DuduclawComp};

// A4-1 note — why this backend still redraws unconditionally while
// `udev_backend.rs` is strictly on-demand, and why that is not an
// inconsistency to "fix later":
//
// `WinitEvent::Redraw` is emitted from winit's `WindowEvent::RedrawRequested`,
// which only ever arrives in response to `Window::request_redraw()` (smithay
// 0.7.0 `src/backend/winit/mod.rs:451`). So a winit compositor must call
// `request_redraw()` to be scheduled at all — and the only handle to the
// window lives inside `WinitGraphicsBackend`, which this function moves into
// the event-source closure below because that closure is also what renders.
// An on-demand winit path therefore needs the backend hoisted out into
// `CalloopData` so the post-dispatch callback in `main.rs` can re-arm it
// (exactly the shape `udev_backend::dispatch_render` uses).
//
// That refactor was attempted in this round and deliberately reverted: a
// "skip the composite but still re-arm" version was measured in-container at
// 100% CPU with ~780 000 skipped frames per 5 s — winit re-fires
// `RedrawRequested` immediately, so the early return is a hot spin, strictly
// worse than compositing. Hoisting the backend instead would rewrite the one
// code path every previous round's live verification covers, for a backend
// that only ever runs nested during development and CI. The appliance runs
// the udev backend. See BUILD.md's "A4-1 CPU accounting" section for the
// measured numbers.

pub fn init_winit(
    event_loop: &mut EventLoop<CalloopData>,
    data: &mut CalloopData,
) -> Result<(), Box<dyn std::error::Error>> {
    let display_handle = &mut data.display_handle;
    let state = &mut data.state;

    // Explicit `GlesRenderer` turbofish: previously inferred from the
    // render_output turbofish's `WaylandSurfaceRenderElement<GlesRenderer>`
    // custom-element type argument; that argument is now
    // `SolidColorRenderElement` (renderer-agnostic — see the redraw arm
    // below), so nothing else in this function fixes `R` for
    // `winit::init::<R>()` without this annotation.
    let (mut backend, winit) = winit::init::<GlesRenderer>()?;

    let mode = Mode {
        size: backend.window_size(),
        refresh: 60_000,
    };

    let output = Output::new(
        "winit".to_string(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "DuDuClaw".into(),
            model: "duduclaw-comp (winit)".into(),
        },
    );
    let _global = output.create_global::<DuduclawComp>(display_handle);
    // WP-comp-shell-display D4b-3: restore a scale chosen via a previous
    // run's `set_output_scale`, in the SAME `change_current_state` call that
    // sets the initial mode/transform — the FIRST state this output is ever
    // announced at, not "boot at 100% then immediately jump" (which would
    // mean every client's very first `wl_output.scale` event is wrong and
    // has to be followed by a second one a moment later). No stored
    // preference (the overwhelmingly common case: fresh install, or an
    // output name never seen before) -> `None`, byte-identical to every
    // prior round.
    let initial_scale = crate::output_prefs::load_scale_pct("winit")
        .map(|pct| smithay::output::Scale::Fractional(pct as f64 / 100.0));
    output.change_current_state(Some(mode), Some(Transform::Flipped180), initial_scale, Some((0, 0).into()));
    output.set_preferred(mode);

    state.space.map_output(&output, (0, 0));

    let mut damage_tracker = OutputDamageTracker::from_output(&output);

    // CD-2 shadow workspace (WP-CD2-shadow): PiP render target state,
    // captured by the `move` closure below alongside `output`/
    // `damage_tracker` — same lifetime shape, just for the second
    // (offscreen) render pass. `pip_texture` starts `None` and is
    // allocated lazily on first use (`DuduclawComp::codrive_render_pip`,
    // `codrive/shadow.rs`) rather than eagerly here, since a shadow session
    // may never start this run. `pip_damage_tracker` is built against
    // `state.shadow_output` right away, same as the main `damage_tracker`
    // above is built against `output` — the shadow output's mode is
    // already set by `codrive::create_shadow_output` (called from
    // `DuduclawComp::new`, before this function ever runs).
    let mut pip_texture: Option<GlesTexture> = None;
    let mut pip_damage_tracker = OutputDamageTracker::from_output(&state.shadow_output);

    // A4-1: read once at startup (same shape as every other comp-level env
    // tunable in this crate — see `codrive/watch.rs`'s module doc).

    // SAFETY: single-threaded at this point in startup, before the event
    // loop starts running client callbacks — matches smallvil's own use of
    // `set_var` here (see attribution note above).
    unsafe {
        std::env::set_var("WAYLAND_DISPLAY", &state.socket_name);
    }

    event_loop.handle().insert_source(winit, move |event, _, data| {
        let display = &mut data.display_handle;
        let state = &mut data.state;

        match event {
            WinitEvent::Resized { size, .. } => {
                output.change_current_state(
                    Some(Mode {
                        size,
                        refresh: 60_000,
                    }),
                    None,
                    None,
                    None,
                );
                // WM-3: layer surfaces are arranged against the output's MODE,
                // so a resize leaves every anchored one at the old geometry
                // (and the exclusive zone stale) unless the map is re-arranged
                // FIRST — the window policy below reads that zone.
                state.rearrange_layers();
                // WM-1: the reserved bands are computed against the output, so
                // a changed output has to re-drive the policy or every already
                // mapped window keeps the old work area (an app window would
                // then either cover the dock or float short of it). Cheap and
                // idempotent — `apply_window_policy` sends a configure only
                // when something actually changed, and never moves a window
                // that is already where the policy wants it.
                state.reapply_window_policy_all();
            }
            WinitEvent::Input(event) => state.process_input_event(event),
            WinitEvent::Redraw => {
                // The udev backend consumes `pending_redraw` to decide
                // whether to composite at all; this backend composites
                // unconditionally (see the A4-1 note above this function)
                // and just clears the flag so it can't go stale.
                state.pending_redraw = false;

                let size = backend.window_size();
                let damage = Rectangle::from_size(size);

                // CD-0 codrive spike (DESIGN §3.3.2): the agent cursor is a
                // compositor-internal render element, not a client surface —
                // queried fresh every frame directly from the seat's pointer
                // handle rather than tracked as duplicate state on
                // `DuduclawComp`. `custom_elements`'s element type `C` is
                // independent of the space's own element type (see
                // `render_output`'s signature — `C: RenderElement<R>` has no
                // relation to `E: SpaceElement`).
                //
                // CUR-1: the HUMAN cursor is built first (so it sorts above
                // everything else in the slice — `render_output` treats
                // earlier elements as nearer the viewer) and needs the
                // renderer, since it is either a themed image uploaded from
                // memory or a client-provided surface tree. Scoped like the
                // PiP block below so `backend`'s borrow ends before
                // `backend.bind()`.
                // A2: the mode replaced the bare `is_frozen()` read — it is
                // what decides whether an agent pointer is drawn at ALL (no
                // session ⇒ none), which colour it takes, and whether the
                // screen-edge indicator appears. See `codrive/mode.rs`.
                let agent_pos = state.agent_seat.get_pointer().unwrap().current_location();
                let driving_mode = state.codrive_driving_mode();
                // WP-comp-shell-display D4b-3: the single scale every
                // overlay element built below shares — see `render::
                // output_render_scale`'s own doc. Computed once per frame,
                // same as `decor::paint::build_output_elements`'s own copy
                // computes at the same live value a few lines later when
                // this frame's `build_output_elements` call runs.
                let scale = crate::render::output_render_scale(&output);
                // The mode indicator's frame must be sized in the output's
                // TRUE logical space (physical mode size / scale), not the
                // raw physical window size `size` below (that stays
                // physical on purpose — it also feeds `damage`/`submit`,
                // which want physical pixels). `output_geometry` is the
                // same scale-aware source `decor::paint::build_output_
                // elements` and `shell_output_info` already trust.
                let indicator_logical_size = state
                    .space
                    .output_geometry(&output)
                    .map(|g| g.size)
                    .unwrap_or_default();
                let mut cursor_elements: Vec<CodriveElement> = {
                    let renderer = backend.renderer();
                    state.build_human_cursor_elements(renderer, Point::from((0.0, 0.0)), scale)
                };
                // CD-2: cursor/highlight elements now go through the
                // combined `CodriveElement` enum (see `render.rs`'s
                // `render_elements!` invocation) so the PiP texture element
                // below can share the same `custom_elements` slice.
                cursor_elements.extend(
                    crate::codrive::build_agent_cursor_elements(agent_pos, driving_mode, scale)
                        .into_iter()
                        .map(CodriveElement::Solid),
                );
                // A2 (DESIGN §3.3.2(d) "系統級『共駕中』指示…不可隱藏"): the
                // screen-edge frame. Output-local coordinates by
                // construction, so unlike the highlight box it takes no
                // offset — and this backend's single output sits at the
                // origin anyway. See `codrive/mode_indicator.rs`.
                cursor_elements.extend(
                    crate::codrive::build_mode_indicator_elements(
                        indicator_logical_size,
                        driving_mode,
                        scale,
                    )
                    .into_iter()
                    .map(CodriveElement::Solid),
                );
                // CD-1 (DESIGN §3.3.2(b) target highlight box): appended
                // into the same custom-elements slice as the cursors —
                // `codrive_highlight_elements` takes `&mut self` since it
                // also clears an expired highlight as a side effect (see
                // `codrive/highlight.rs`), so this has to run through
                // `state` (already `&mut` in this closure) rather than as
                // a free function like `build_cursor_elements`.
                cursor_elements.extend(
                    state
                        .codrive_highlight_elements(std::time::Instant::now(), scale)
                        .into_iter()
                        .map(CodriveElement::Solid),
                );
                // CD-3 watch mode (task brief item 3, `codrive/watch.rs`):
                // same per-redraw hook as the highlight expiry check above —
                // cheap `Instant`-only work, safe for this crate's tight
                // unthrottled redraw loop.
                state.codrive_check_watch_idle(std::time::Instant::now());
                // A2: the per-frame reconciliation. This is the ONLY place
                // the main thread can observe the socket thread flipping
                // `session_active` (a connection arriving or dropping is not
                // a main-thread event), so without it a session start/end
                // would never produce a `driving_mode` audit line or event.
                // A true no-op when the mode did not change.
                state.codrive_sync_mode();

                // D3-c backstop (`codrive_refresh_ime_pause`): keeps the
                // socket thread's `ime_paused` mirror from latching `true`
                // after an input method exits. Two atomic loads and a swap —
                // cheap enough for this loop, and the only reason the mirror
                // can be trusted by `listener.rs` at all.
                state.codrive_refresh_ime_pause();

                // CD-2 shadow workspace (WP-CD2-shadow, DESIGN §3.3.4): a
                // second, offscreen render pass of the shadow output into a
                // persistent GLES texture, wrapped as a PiP element and
                // appended to the SAME custom-elements slice the main
                // output's own render pass below consumes. Scoped to its
                // own block so `backend`'s mutable borrow from
                // `backend.renderer()` ends before `backend.bind()` is
                // called a few lines down — the two calls are independent
                // per-call borrows of `backend` (same shape as
                // `backend.bind()` itself already returning `(&mut R,
                // Framebuffer<'_>)` as two separately-borrowed values, not
                // one derived from the other), so there is no live overlap
                // to resolve as long as this block doesn't hold `renderer`
                // past its own end.
                let pip_element = {
                    let renderer = backend.renderer();
                    state.codrive_render_pip(renderer, &mut pip_texture, &mut pip_damage_tracker, size)
                };
                if let Some(pip) = pip_element {
                    cursor_elements.push(CodriveElement::Pip(pip));
                }

                {
                    let (renderer, mut framebuffer) = backend.bind().unwrap();
                    // WM-2: `desktop::space::render_output` is no longer used.
                    // It puts ALL custom elements above ALL windows, which is
                    // right for the overlays built above and wrong for a
                    // per-window title bar. `build_output_elements` builds the
                    // same list interleaved per window and ends in the exact
                    // `OutputDamageTracker::render_output` call upstream makes
                    // — see `decor/paint.rs`'s module doc.
                    let elements = state.build_output_elements(renderer, &output, cursor_elements);
                    damage_tracker
                        .render_output(
                            renderer,
                            &mut framebuffer,
                            0,
                            &elements,
                            [0.1, 0.1, 0.1, 1.0],
                        )
                        .unwrap();
                }
                backend.submit(Some(&[damage])).unwrap();

                let elapsed = state.start_time.elapsed();
                state.space.elements().for_each(|window| {
                    window.send_frame(&output, elapsed, Some(Duration::ZERO), |_, _| {
                        Some(output.clone())
                    })
                });
                // CUR-1: a client-provided cursor surface is not in `space`,
                // so it needs its own frame callback or a client that
                // double-buffers its cursor stalls after one commit.
                state.send_cursor_frames(&output, elapsed);
                // WM-3: layer surfaces are not in `space` either — same
                // consequence (a double-buffering panel stalls after one
                // commit), plus this is where dead layers are reaped.
                state.send_layer_frames_and_cleanup(&output, elapsed);

                state.space.refresh();
                state.popups.cleanup();
                let _ = display.flush_clients();

                // Ask for redraw to schedule the next frame.
                backend.window().request_redraw();
            }
            WinitEvent::CloseRequested => {
                state.loop_signal.stop();
            }
            _ => (),
        };
    })?;

    Ok(())
}
