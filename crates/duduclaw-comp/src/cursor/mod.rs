//! CUR-1 (2026-08-22): the **human** pointer.
//!
//! # What was wrong
//!
//! Two separate defects stacked on top of each other, both reported from a
//! real appliance run — *"滑鼠是一個方塊，而非主流鼠標，而且還是白色的誰看得
//! 到"*:
//!
//! 1. `SeatHandler::cursor_image` was an empty function
//!    (`handlers/mod.rs`), so every `wl_pointer.set_cursor` a client sent was
//!    dropped on the floor. Nothing could ever change the pointer: no I-beam
//!    over a text field, no hand over a link, no resize arrows on a window
//!    edge.
//! 2. What the compositor drew instead was the CD-0 spike's placeholder — a
//!    10×10 **white** square (`codrive/cursor.rs`, whose own header called it
//!    a placeholder). Invisible against the light OOBE background.
//!
//! # What this module does
//!
//! It owns the human seat's cursor state and turns it into render elements:
//!
//! * [`CursorState::status`] tracks the live [`CursorImageStatus`] for the
//!   human seat, fed by `cursor_image` in `handlers/mod.rs`.
//! * `Surface(surface)` — the client supplied its own cursor image; it is
//!   drawn as a real surface tree at the pointer position, offset by the
//!   hotspot the client declared.
//! * `Named(icon)` — the client asked for a *named* cursor (or nothing has
//!   overridden the default). The image comes from the machine's XCursor
//!   theme via [`theme::CursorThemeStore`], falling back to a built-in
//!   outlined arrow ([`fallback`]) when no theme is installed.
//! * `Hidden` — nothing is drawn.
//!
//! # The agent cursor is NOT handled here
//!
//! `codrive/cursor.rs` keeps drawing the amber cross for the agent seat, and
//! keeps turning it dark red while frozen. That difference is deliberate
//! design (DESIGN-codrive-desktop-2026-08.md §3.3.2, "與人游標明確異形異色")
//! and CUR-1 does not touch it — a client's `set_cursor` on the AGENT seat is
//! ignored on purpose (see `cursor_image` in `handlers/mod.rs`), because the
//! agent pointer must stay visually unmistakable no matter what the focused
//! application would like it to look like.
//!
//! # Both client protocols are served
//!
//! Legacy `wl_pointer.set_cursor` reaches `cursor_image` as
//! `CursorImageStatus::Surface`; modern `wp_cursor_shape_v1` reaches it as
//! `CursorImageStatus::Named` (smithay's `wayland::cursor_shape` translates
//! the shape enum and calls the same handler). `state.rs` registers the
//! `cursor-shape-v1` global; see `handlers/mod.rs` for why both matter.

pub mod fallback;
pub mod source;
pub mod theme;

use std::time::Duration;

use smithay::{
    backend::renderer::{
        element::{
            memory::MemoryRenderBufferRenderElement, surface::render_elements_from_surface_tree,
            surface::WaylandSurfaceRenderElement, Kind,
        },
        gles::GlesRenderer,
    },
    desktop::utils::send_frames_surface_tree,
    input::pointer::{CursorImageStatus, CursorImageSurfaceData},
    output::Output,
    utils::{IsAlive, Logical, Physical, Point, Scale},
    wayland::compositor,
};

use crate::{render::CodriveElement, state::DuduclawComp};

/// The element type `render_elements_from_surface_tree` produces for a
/// client-provided cursor surface. Named only so the turbofish at its one
/// call site stays readable.
type SurfaceElement = WaylandSurfaceRenderElement<GlesRenderer>;

/// Everything the compositor needs to draw the human pointer.
pub struct CursorState {
    /// What the focused client last asked for on the HUMAN seat. Starts at
    /// the default named cursor, which is also what smithay itself resets to
    /// whenever the pointer leaves a surface
    /// (`input/pointer/mod.rs`'s `default_named()` calls).
    pub status: CursorImageStatus,
    /// Theme + cache + fallback. See [`theme::CursorThemeStore`].
    pub theme: theme::CursorThemeStore,
    /// One-shot guard so a renderer that refuses to import the cursor image
    /// logs once instead of once per frame.
    import_error_logged: bool,
}

impl CursorState {
    /// Reads the cursor configuration from the environment and loads the
    /// theme. Called once from `DuduclawComp::new`.
    pub fn from_env() -> Self {
        let theme = theme::CursorThemeStore::from_env();
        Self {
            status: CursorImageStatus::default_named(),
            theme,
            import_error_logged: false,
        }
    }
}

impl DuduclawComp {
    /// Records what the human seat's cursor should look like.
    ///
    /// Called from `SeatHandler::cursor_image`. Marks the frame dirty so the
    /// udev backend actually repaints — without this a shape change would sit
    /// invisible until some unrelated damage happened to schedule a frame
    /// (see [`DuduclawComp::queue_redraw`]).
    pub fn set_human_cursor_image(&mut self, image: CursorImageStatus) {
        if self.cursor.status == image {
            return;
        }
        // Debug, not info: a client can legitimately change its cursor on
        // every pointer motion (hovering a link edge, dragging a splitter).
        // It is the one line that proves the whole chain — client request →
        // handler → what we will draw — so it exists, but it must not flood
        // a normal run's log.
        tracing::debug!(
            cursor = %describe(&image),
            "cursor: human pointer image changed"
        );
        self.cursor.status = image;
        self.queue_redraw();
    }

    /// This frame's render elements for the human pointer.
    ///
    /// `offset` maps global `Space` logical coordinates into the rendered
    /// output's own space — the same parameter (and the same reasoning)
    /// `codrive_highlight_elements_at` takes, so a second monitor draws its
    /// cursor in the right place. Both backends pass the pointer position
    /// pre-offset and this recomputes nothing.
    ///
    /// Returns an empty vec for a hidden cursor, and *also* for an image the
    /// renderer refuses to import — an unimportable cursor is a missing
    /// cursor, never a panic on the render path.
    pub fn build_human_cursor_elements(
        &mut self,
        renderer: &mut GlesRenderer,
        offset: Point<f64, Logical>,
    ) -> Vec<CodriveElement> {
        let scale = Scale::from(1.0);
        let Some(pointer) = self.seat.get_pointer() else {
            return Vec::new();
        };
        let pos = pointer.current_location() + offset;

        // Cloned rather than borrowed: the `Named` arm needs `&mut
        // self.cursor.theme` (the theme cache is populated lazily), which a
        // live borrow of `self.cursor.status` would forbid. `CursorImageStatus`
        // is `Clone` and its heaviest variant holds an `Arc`-backed
        // `WlSurface`, so this is a refcount bump.
        let status = self.cursor.status.clone();

        match status {
            CursorImageStatus::Hidden => Vec::new(),
            CursorImageStatus::Surface(surface) => {
                if !surface.alive() {
                    // The client that owned this cursor surface is gone.
                    // Falling back to the default named cursor keeps a
                    // pointer on screen; leaving the dead surface in place
                    // would draw nothing at all.
                    tracing::debug!(
                        "cursor: client cursor surface died — reverting to the default cursor"
                    );
                    self.cursor.status = CursorImageStatus::default_named();
                    return self.build_human_cursor_elements(renderer, offset);
                }
                let hotspot = cursor_surface_hotspot(&surface);
                let loc: Point<i32, Physical> =
                    (pos - hotspot.to_f64()).to_physical_precise_round(scale);
                render_elements_from_surface_tree::<GlesRenderer, SurfaceElement>(
                    renderer,
                    &surface,
                    loc,
                    scale,
                    1.0,
                    Kind::Cursor,
                )
                .into_iter()
                .map(CodriveElement::Surface)
                .collect()
            }
            CursorImageStatus::Named(icon) => {
                let cursor = self.cursor.theme.cursor_for(icon);
                // Rounded to a whole physical pixel before being widened back
                // to the `f64` location `from_buffer` wants: the artwork is a
                // hard-edged bitmap, and letting it land on a fractional
                // position makes every edge (including the dark outline that
                // makes the fallback visible at all) sample blurry.
                let loc: Point<i32, Physical> =
                    (pos - cursor.hotspot.to_f64()).to_physical_precise_round(scale);
                match MemoryRenderBufferRenderElement::from_buffer(
                    renderer,
                    loc.to_f64(),
                    &cursor.buffer,
                    None,
                    None,
                    None,
                    Kind::Cursor,
                ) {
                    Ok(element) => vec![CodriveElement::Memory(element)],
                    Err(e) => {
                        if !self.cursor.import_error_logged {
                            self.cursor.import_error_logged = true;
                            tracing::error!(
                                error = ?e,
                                icon = icon.name(),
                                "cursor: renderer refused to import the cursor image — the human \
                                 pointer will be invisible this run"
                            );
                        }
                        Vec::new()
                    }
                }
            }
        }
    }

    /// Delivers frame callbacks to a client-provided cursor surface.
    ///
    /// Called from both backends alongside the existing per-window
    /// `send_frame` loop. Without it a client that animates its own cursor
    /// (or simply waits for a frame callback before drawing the next cursor
    /// buffer, which is the normal double-buffered pattern) would stall after
    /// its first commit and freeze the pointer image.
    pub fn send_cursor_frames(&self, output: &Output, time: Duration) {
        if let CursorImageStatus::Surface(surface) = &self.cursor.status {
            if surface.alive() {
                send_frames_surface_tree(surface, output, time, Some(Duration::ZERO), |_, _| {
                    Some(output.clone())
                });
            }
        }
    }
}

/// Short, log-safe rendering of a cursor status.
///
/// Deliberately does NOT include the surface's object id or any client
/// identity — this line can fire on every pointer motion, and a log field
/// that grows per event is how a debug log becomes unreadable.
fn describe(status: &CursorImageStatus) -> &'static str {
    match status {
        CursorImageStatus::Hidden => "hidden",
        CursorImageStatus::Named(icon) => icon.name(),
        CursorImageStatus::Surface(_) => "client-surface",
    }
}

/// The hotspot a client declared for its cursor surface, in logical pixels.
///
/// smithay stores it in the surface's data map at `wl_pointer.set_cursor`
/// time (`wayland/seat/pointer.rs`). A surface with no such entry yet — a
/// client that committed before setting a hotspot — reads as `(0, 0)`, which
/// is the protocol's own default rather than an invented value.
fn cursor_surface_hotspot(
    surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
) -> Point<i32, Logical> {
    compositor::with_states(surface, |states| {
        states
            .data_map
            .get::<CursorImageSurfaceData>()
            .map(|attrs| attrs.lock().unwrap().hotspot)
            .unwrap_or_default()
    })
}
