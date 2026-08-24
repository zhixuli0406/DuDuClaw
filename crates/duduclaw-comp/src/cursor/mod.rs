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
pub mod persist;
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
    /// CUR-2: where the live source came from. Starts as whatever
    /// [`source::resolve_startup_source`] decided and becomes
    /// [`source::SourceOrigin::Runtime`] the first time a `shell_control`
    /// `set_cursor_source` op changes it. Reported to callers so a settings
    /// page can explain an operator-pinned env var instead of silently
    /// showing a value the user did not choose.
    pub origin: source::SourceOrigin,
    /// One-shot guard so a renderer that refuses to import the cursor image
    /// logs once instead of once per frame.
    import_error_logged: bool,
}

impl CursorState {
    /// Reads the cursor configuration from the environment and the stored
    /// preference, then loads the theme. Called once from
    /// `DuduclawComp::new`.
    pub fn from_env() -> Self {
        let env_raw = std::env::var(source::CURSOR_SOURCE_ENV).ok();
        let (_, origin) = source::resolve_startup_source(env_raw.as_deref(), persist::load());
        let theme = theme::CursorThemeStore::from_env();
        Self {
            status: CursorImageStatus::default_named(),
            theme,
            origin,
            import_error_logged: false,
        }
    }
}

/// CUR-2/CUR-3: everything a `shell_control` caller is told about the live
/// cursor configuration. Assembled by [`DuduclawComp::cursor_source_info`].
///
/// Deliberately reports BOTH `requested` and `source`: they differ exactly
/// when the brand theme was asked for and is not installed, and a settings UI
/// that only saw one of them would either lose the user's choice or claim a
/// paw is on screen when an Adwaita arrow is. CUR-3 adds the size axis with
/// the same discipline — `size` is what was asked for, `effective_size` is
/// what is drawn, and they are separate fields for exactly the same reason.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CursorSourceInfo {
    /// The source actually in effect, after the brand→system fail-safe.
    pub source: String,
    /// The source that was asked for.
    pub requested: String,
    /// The XCursor theme name actually loaded.
    pub theme: String,
    /// `env` / `persisted` / `default` / `runtime`. Describes where the
    /// **source** came from, not the size — see `size_env_pinned` for the one
    /// size-provenance signal a settings page actually needs, and
    /// `CursorSourceInfo`'s own note below on why there is no `size_origin`.
    pub origin: String,
    /// CUR-3: the cursor size currently in effect, in logical pixels — one of
    /// `source::CURSOR_SIZE_STEPS` unless an operator set `XCURSOR_SIZE` to
    /// something else. This is what a settings page highlights.
    pub size: u32,
    /// CUR-3: the size actually being DRAWN, in pixels.
    ///
    /// Equal to `size` on any theme that carries the requested size — which,
    /// on the appliance's own Adwaita, is every offered step. It differs when
    /// the loaded theme has no image at that size (the nearest one is drawn at
    /// its own size; nothing is upscaled) or when no theme was found at all
    /// (the built-in arrow quantises to whole multiples of 24). A settings
    /// page that showed only `size` in those cases would be claiming a pointer
    /// size that is not on screen. See
    /// [`theme::CursorThemeStore::effective_size`] for the full reasoning and
    /// for why upscaling to match was rejected.
    pub effective_size: u32,
    /// CUR-3: true iff `XCURSOR_SIZE` is set in this compositor's environment
    /// — i.e. iff a stored size preference will NOT be honoured at the next
    /// start, no matter what a `set_cursor_size` op writes. The size-side twin
    /// of `env_pinned`.
    ///
    /// There is deliberately no `size_origin` field to match `origin`. The
    /// only thing a UI must not get wrong is "will my choice stick?", and this
    /// boolean answers it; a second four-valued provenance string would be
    /// state to keep in sync for no decision it enables.
    pub size_env_pinned: bool,
    /// True iff `DUDUCLAW_COMP_CURSOR_SOURCE` is set in this compositor's
    /// environment — i.e. iff a stored preference will NOT be honoured at the
    /// next start, no matter what a `set_cursor_source` op writes.
    pub env_pinned: bool,
    /// Only meaningful on a `set_cursor_source` reply: whether the new value
    /// reached the preference file. `false` here with `ok: true` means "the
    /// switch is live but will not survive a restart" — see `persist_error`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub persisted: Option<bool>,
    /// Why persistence failed, when it did. Never present on success.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub persist_error: Option<String>,
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

    /// CUR-2/CUR-3: the live cursor configuration, for `shell_control`'s
    /// `get_cursor_source` / `set_cursor_source` / `set_cursor_size` replies.
    ///
    /// `&mut self` since CUR-3: `effective_size` is answered through the same
    /// `cursor_for` path that draws the frame (see
    /// [`theme::CursorThemeStore::effective_size`] for why a second, read-only
    /// size-probing rule was rejected), and that path fills a lazy cache. The
    /// only side effect is a cache fill the next repaint would have done
    /// anyway.
    pub(crate) fn cursor_source_info(&mut self) -> CursorSourceInfo {
        CursorSourceInfo {
            source: self.cursor.theme.source().as_str().to_string(),
            requested: self.cursor.theme.requested().as_str().to_string(),
            theme: self.cursor.theme.theme_name().to_string(),
            origin: self.cursor.origin.as_str().to_string(),
            size: self.cursor.theme.size(),
            effective_size: self.cursor.theme.effective_size(),
            size_env_pinned: source::env_pins_size(),
            env_pinned: source::env_pins_source(),
            persisted: None,
            persist_error: None,
        }
    }

    /// CUR-2: switch the human pointer's artwork source **live**.
    ///
    /// This is the whole claim of the work package, and it is three lines
    /// because CUR-1 shaped it that way: set the field (inside
    /// [`theme::CursorThemeStore::set_source`], which also reloads the theme
    /// and drops the per-icon cache) and mark the frame dirty. There is no
    /// restart, no reconnect, and no client involvement — the next composite
    /// draws the new artwork, and a `CursorImageStatus::Named` status
    /// (everything a `cursor-shape-v1` client ever sets) resolves through the
    /// new theme automatically.
    ///
    /// `queue_redraw` is not optional here: on the udev backend nothing else
    /// would schedule a frame until some unrelated damage happened, so a
    /// switch made while the pointer sits still would appear to do nothing
    /// until the user moved the mouse. Same reasoning as
    /// [`Self::set_human_cursor_image`].
    ///
    /// Returns `true` when the live artwork actually changed.
    pub(crate) fn set_cursor_source(&mut self, requested: source::CursorSource) -> bool {
        if !self.cursor.theme.set_source(requested) {
            return false;
        }
        self.cursor.origin = source::SourceOrigin::Runtime;
        tracing::info!(
            requested = requested.as_str(),
            effective = self.cursor.theme.source().as_str(),
            theme = %self.cursor.theme.theme_name(),
            "cursor: source switched live (no restart)"
        );
        self.queue_redraw();
        true
    }

    /// CUR-3: change the human pointer's SIZE **live**.
    ///
    /// Same three-line shape (and the same `queue_redraw` requirement) as
    /// [`Self::set_cursor_source`]: without the repaint, a size change made
    /// while the pointer sits still would appear to do nothing on the udev
    /// backend until the user moved the mouse.
    ///
    /// Deliberately does NOT touch `self.cursor.origin`, which describes where
    /// the *source* came from. Size provenance is a separate axis and the only
    /// part of it a caller needs is `size_env_pinned` — see
    /// [`CursorSourceInfo`].
    ///
    /// `size` is expected to already be one of
    /// [`source::CURSOR_SIZE_STEPS`]; the op validates at the socket boundary.
    /// Nothing here re-clamps, because a value that got past validation and is
    /// still wrong should be visible as a wrong cursor, not silently rounded.
    ///
    /// Returns `true` when the live size actually changed.
    pub(crate) fn set_cursor_size(&mut self, size: u32) -> bool {
        if !self.cursor.theme.set_size(size) {
            return false;
        }
        let effective = self.cursor.theme.effective_size();
        if effective == size {
            tracing::info!(size, "cursor: size switched live (no restart)");
        } else {
            // Not a warning — this is a correct, expected outcome on a theme
            // that does not carry every step (see `effective_size`'s doc). It
            // is logged at info because "I asked for 96 and got 64" is exactly
            // the question a support session needs answered from journalctl.
            tracing::info!(
                size,
                effective_size = effective,
                theme = %self.cursor.theme.theme_name(),
                "cursor: size switched live, but the loaded cursor theme has no image at that \
                 size — its nearest image is drawn at its own size, nothing is upscaled"
            );
        }
        self.queue_redraw();
        true
    }

    /// This frame's render elements for the human pointer.
    ///
    /// `offset` maps global `Space` logical coordinates into the rendered
    /// output's own space — the same parameter (and the same reasoning)
    /// `codrive_highlight_elements_at` takes, so a second monitor draws its
    /// cursor in the right place. Both backends pass the pointer position
    /// pre-offset and this recomputes nothing.
    ///
    /// `scale` is the rendered output's own live scale
    /// (`render::output_render_scale`) — WP-comp-shell-display D4b-3
    /// replaced a hardcoded `Scale::from(1.0)` here, which put the human
    /// pointer at the wrong PHYSICAL position (not just the wrong apparent
    /// size) the moment `set_output_scale` moved the output off 100%: this
    /// function only converts the pointer's LOGICAL position to physical
    /// pixels, it does not affect the themed cursor artwork's own size (that
    /// is `MemoryRenderBufferRenderElement::geometry`'s job, which already
    /// re-derives physical size from the render-time scale `render_output`
    /// passes it — checked against smithay 0.7.0 source, not assumed).
    ///
    /// Returns an empty vec for a hidden cursor, and *also* for an image the
    /// renderer refuses to import — an unimportable cursor is a missing
    /// cursor, never a panic on the render path.
    pub fn build_human_cursor_elements(
        &mut self,
        renderer: &mut GlesRenderer,
        offset: Point<f64, Logical>,
        scale: Scale<f64>,
    ) -> Vec<CodriveElement> {
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
                    return self.build_human_cursor_elements(renderer, offset, scale);
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
