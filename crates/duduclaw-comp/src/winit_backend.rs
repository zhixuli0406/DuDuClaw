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
        renderer::{damage::OutputDamageTracker, element::solid::SolidColorRenderElement, gles::GlesRenderer},
        winit::{self, WinitEvent},
    },
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::calloop::EventLoop,
    utils::{Rectangle, Transform},
};

use crate::{CalloopData, DuduclawComp};

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
    output.change_current_state(Some(mode), Some(Transform::Flipped180), None, Some((0, 0).into()));
    output.set_preferred(mode);

    state.space.map_output(&output, (0, 0));

    let mut damage_tracker = OutputDamageTracker::from_output(&output);

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
            }
            WinitEvent::Input(event) => state.process_input_event(event),
            WinitEvent::Redraw => {
                let size = backend.window_size();
                let damage = Rectangle::from_size(size);

                // CD-0 codrive spike (DESIGN §3.3.2): both cursors are
                // compositor-internal render elements, not client
                // surfaces — queried fresh every frame directly from each
                // seat's pointer handle rather than tracked as duplicate
                // state on `DuduclawComp`. `custom_elements`'s element type
                // `C` is independent of the space's own element type (see
                // `render_output`'s signature — `C: RenderElement<R>` has
                // no relation to `E: SpaceElement`), so this doesn't need a
                // combined enum: `SolidColorRenderElement` implements
                // `RenderElement<R>` for any `R`.
                let human_pos = state.seat.get_pointer().unwrap().current_location();
                let agent_pos = state.agent_seat.get_pointer().unwrap().current_location();
                let agent_frozen = state.codrive.is_frozen();
                let cursor_elements =
                    crate::codrive::build_cursor_elements(human_pos, agent_pos, agent_frozen);

                {
                    let (renderer, mut framebuffer) = backend.bind().unwrap();
                    smithay::desktop::space::render_output::<
                        _,
                        SolidColorRenderElement,
                        _,
                        _,
                    >(
                        &output,
                        renderer,
                        &mut framebuffer,
                        1.0,
                        0,
                        [&state.space],
                        &cursor_elements,
                        &mut damage_tracker,
                        [0.1, 0.1, 0.1, 1.0],
                    )
                    .unwrap();
                }
                backend.submit(Some(&[damage])).unwrap();

                state.space.elements().for_each(|window| {
                    window.send_frame(
                        &output,
                        state.start_time.elapsed(),
                        Some(Duration::ZERO),
                        |_, _| Some(output.clone()),
                    )
                });

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
