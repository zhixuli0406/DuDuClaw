//! WM-1 (2026-08-23): the **transitional** window-layout policy.
//!
//! ## The bug this exists to fix
//!
//! Live report from the appliance VM: *"開啟 Chromium 後上下方導航列直接不見，
//! 而且無法關閉應用程式"*. Two structural causes, both fixed here:
//!
//! 1. `handlers/xdg_shell.rs`'s A4 initial-configure fix sized **every**
//!    toplevel to the full output and `new_toplevel` mapped every toplevel at
//!    `(0, 0)`. `duduclaw-shell` is itself a single full-screen toplevel that
//!    paints the menu bar and the dock *inside its own window*, so the first
//!    third-party window to map covered the shell completely — menu bar, dock,
//!    every affordance gone. With no way to reach the dock and no window
//!    decorations, opening one app soft-locked the session.
//! 2. Comp advertised no `zxdg_decoration_manager_v1`, so a client had no
//!    negotiated answer to "who draws the title bar" — see
//!    `handlers/xdg_shell.rs`'s `XdgDecorationHandler` impl for what changed
//!    and `BUILD.md`'s WM-1 section for the first-hand Chromium observation.
//!
//! ## What this module is (and is not)
//!
//! This is the **reserved-band** ("work area") policy every mainstream desktop
//! uses for its own menu bar / taskbar: a maximized or newly mapped
//! application window gets the output *minus* the bands the session chrome
//! occupies, so the chrome stays visible and clickable. Windows taskbar and
//! the macOS menu bar both work exactly this way; this is not an invention of
//! ours, it is the smallest correct thing to do before the real multi-window
//! desktop lands.
//!
//! It is deliberately **not** A5 ("多視窗桌面殼"): no layer-shell, no
//! server-side decoration drawing, no task switcher UI, no per-window freeze
//! semantics. A5 owns the layout policy when it lands and this module is
//! expected to be replaced by it, not extended into it.
//!
//! ## Where the band heights come from — WM-3 migration (2026-08-23)
//!
//! **Both bands are now zero by default.** `duduclaw-shell` is migrating its
//! menu bar and dock to real `zwlr_layer_shell_v1` surfaces, each declaring
//! its own `set_exclusive_zone` (30 for the menu bar, 90 for the dock — the
//! same two numbers this table used to hard-code). Once a layer surface
//! claims a zone, `layer_shell::geometry::effective_work_area` computes the
//! work area as `banded ∩ zone`; with `bands = {0, 0}`, `banded` is the whole
//! output, so the intersection collapses to exactly `zone` — the layer
//! surface's own exclusive-zone claim is now the *entire* source of truth for
//! how much of the screen an ordinary application window may occupy. See
//! `layer_shell::geometry`'s own module doc for the full "intersection, not
//! replacement" reasoning this depends on, and its doc comment's own line
//! predicting this exact migration: *"The migration package should still
//! zero the constants … so the two cannot drift apart later."* This module is
//! that migration package.
//!
//! [`RESERVED_TOP_ENV`] / [`RESERVED_BOTTOM_ENV`] are **not removed** — they
//! are kept as an explicit compatibility fallback for a shell build that
//! predates the layer-shell migration (one that still paints its menu bar and
//! dock inside its own full-output toplevel, the way WM-1/WM-2 assumed, and
//! therefore claims no exclusive zone at all). Pointing them at the old 30/90
//! values restores the pre-migration hard-banding behaviour for exactly that
//! case; on a migrated shell they are redundant with (and, per the
//! intersection rule above, can only ever *shrink further than*) the layer
//! surfaces' own zones.
//!
//! Both bands are still **logical** pixels at scale 1.0, which is what comp
//! composites at (`render_output(…, 1.0, …)` in both backends) and what
//! `Space` coordinates are in, so no conversion is involved.

use smithay::{
    desktop::Window,
    output::Output,
    reexports::wayland_server::{protocol::wl_surface::WlSurface, Resource},
    utils::{Logical, Point, Rectangle, Size},
};

use crate::state::DuduclawComp;

/// The `xdg_toplevel.app_id` `duduclaw-shell` declares (see that crate's
/// `main.rs` `WindowOptions { app_id: … }`). A window carrying this id is the
/// session shell and is exempt from the reserved bands — it *is* the chrome
/// the bands protect.
pub const SHELL_APP_ID: &str = "duduclaw-shell";

/// Override for [`SHELL_APP_ID`] — for running a different shell binary
/// (or a stand-in) against comp without rebuilding.
pub const SHELL_APP_ID_ENV: &str = "DUDUCLAW_COMP_SHELL_APP_ID";

/// Tuning override for [`ReservedBands::top`].
pub const RESERVED_TOP_ENV: &str = "DUDUCLAW_COMP_RESERVED_TOP";
/// Tuning override for [`ReservedBands::bottom`].
pub const RESERVED_BOTTOM_ENV: &str = "DUDUCLAW_COMP_RESERVED_BOTTOM";

/// WM-3: zeroed — the shell's menu bar now claims its own 30px exclusive
/// zone via layer-shell, so this constant no longer needs to duplicate that
/// number. See this module's doc for the migration and for
/// [`RESERVED_TOP_ENV`]'s compatibility-fallback role.
pub const DEFAULT_RESERVED_TOP: i32 = 0;
/// WM-3: zeroed — the shell's dock now claims its own 90px exclusive zone via
/// layer-shell. See [`DEFAULT_RESERVED_TOP`] and this module's doc.
pub const DEFAULT_RESERVED_BOTTOM: i32 = 0;

/// Largest band value accepted from the environment. Anything above this is
/// a typo (or an attempt to make every app window invisible), not a
/// configuration.
const MAX_RESERVED_BAND: i32 = 10_000;

/// Below this, the work area stops being a window and starts being a sliver.
/// An output too short to carry the bands *and* a usable window gets the
/// whole output instead — a squeezed-to-nothing app window would be a worse
/// failure than a covered dock, and this is the one case where "cover the
/// chrome" is the lesser evil.
pub const MIN_APP_HEIGHT: i32 = 120;

/// The strip at the top and the strip at the bottom of the output that
/// `duduclaw-shell` paints its own chrome into, and that ordinary application
/// windows must stay out of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReservedBands {
    pub top: i32,
    pub bottom: i32,
}

impl Default for ReservedBands {
    fn default() -> Self {
        Self {
            top: DEFAULT_RESERVED_TOP,
            bottom: DEFAULT_RESERVED_BOTTOM,
        }
    }
}

impl ReservedBands {
    /// Reads both overrides once, at startup (same shape as every other
    /// comp-level env tunable in this crate).
    pub fn from_env() -> Self {
        Self::from_env_values(
            std::env::var(RESERVED_TOP_ENV).ok().as_deref(),
            std::env::var(RESERVED_BOTTOM_ENV).ok().as_deref(),
        )
    }

    /// Pure half of [`ReservedBands::from_env`], kept unit-testable without
    /// touching the process environment (this crate's standing constraint —
    /// see `input.rs`'s `is_system_gesture_tail` for the same split).
    ///
    /// An unset, empty, non-numeric, negative or absurd value keeps the
    /// default for **that band only**; a bad top value never silently drags
    /// the bottom band with it. `0` is a legitimate value meaning "no band".
    pub fn from_env_values(top: Option<&str>, bottom: Option<&str>) -> Self {
        let d = Self::default();
        Self {
            top: parse_band(top).unwrap_or(d.top),
            bottom: parse_band(bottom).unwrap_or(d.bottom),
        }
    }
}

fn parse_band(raw: Option<&str>) -> Option<i32> {
    let v: i32 = raw?.trim().parse().ok()?;
    (0..=MAX_RESERVED_BAND).contains(&v).then_some(v)
}

/// Pure half of [`shell_app_id_from_env`].
pub fn shell_app_id_from_env_value(raw: Option<&str>) -> String {
    match raw.map(str::trim) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => SHELL_APP_ID.to_string(),
    }
}

/// The `app_id` that marks the session shell for this run.
pub fn shell_app_id_from_env() -> String {
    shell_app_id_from_env_value(std::env::var(SHELL_APP_ID_ENV).ok().as_deref())
}

/// The part of `output` an ordinary application window may occupy.
///
/// Degenerate outputs are handled by falling back to the whole output rather
/// than producing a zero/negative-sized rectangle: a nonsense mode reported by
/// a connector must never turn into a window a client cannot render into.
pub fn work_area(output: Rectangle<i32, Logical>, bands: ReservedBands) -> Rectangle<i32, Logical> {
    let top = bands.top.max(0);
    let bottom = bands.bottom.max(0);
    if output.size.h <= 0 || output.size.w <= 0 {
        return output;
    }
    let remaining = output.size.h - top - bottom;
    if remaining < MIN_APP_HEIGHT.min(output.size.h) {
        return output;
    }
    Rectangle::new(
        Point::from((output.loc.x, output.loc.y + top)),
        Size::from((output.size.w, remaining)),
    )
}

/// Where a toplevel should be placed and how big it should be — **WM-1's**
/// rule, kept only for the session shell.
///
/// The session shell gets the whole output (it paints the bands itself).
///
/// WM-2 (2026-08-23) replaced the `else` branch: an ordinary application
/// window is no longer *filled* into the work area, it is **floated** inside
/// it — see `DuduclawComp::floating_content_rect` and `crate::decor::
/// placement`. This function survives because the shell half of the rule is
/// unchanged and is asserted by this module's own tests.
pub fn window_rect(
    output: Rectangle<i32, Logical>,
    bands: ReservedBands,
    is_shell: bool,
) -> Rectangle<i32, Logical> {
    if is_shell {
        output
    } else {
        work_area(output, bands)
    }
}

impl DuduclawComp {
    /// The geometry the layout policy is computed against: the first **real**
    /// output.
    ///
    /// `None` when the only mapped output is the CD-2 shadow workspace's
    /// headless one (i.e. no backend has produced a real output yet) — the
    /// pre-WM-1 code had the same guard, expressed as "the output whose
    /// origin is `(0, 0)`", and left the client's size at xdg-shell's `0x0`
    /// ("you pick") rather than guessing. Sizing a window against the shadow
    /// output would place it 100 000 px off-screen.
    pub fn layout_output_geometry(&self) -> Option<Rectangle<i32, Logical>> {
        let output = self.space.outputs().find(|o| *o != &self.shadow_output)?;
        self.space.output_geometry(output)
    }

    /// The first **real** output itself, cloned.
    ///
    /// WM-3 needs the `Output` and not just its geometry: a layer map is
    /// keyed by output (`layer_map_for_output`), so every layer-shell path has
    /// to name the output rather than describe it. Same shadow-workspace guard
    /// as [`Self::layout_output_geometry`], for the same reason.
    pub fn layout_output(&self) -> Option<Output> {
        self.space
            .outputs()
            .find(|o| *o != &self.shadow_output)
            .cloned()
    }

    /// Is `surface` the toplevel comp currently treats as the session shell?
    ///
    /// Read-only — never promotes. Used by the Super+Q close path, which must
    /// answer "would this close the desktop?" without side effects.
    pub fn is_session_shell_surface(&self, surface: &WlSurface) -> bool {
        self.shell_surface.as_ref() == Some(surface)
    }

    fn window_app_id(window: &Window) -> Option<String> {
        crate::codrive::window_target::window_identity(window).0
    }

    /// Decides whether `window` is the session shell, promoting/demoting the
    /// shell role as identity information arrives.
    ///
    /// Two rules, in priority order:
    ///
    /// 1. **`app_id` (authoritative).** A toplevel whose `xdg_toplevel.app_id`
    ///    equals [`SHELL_APP_ID`] is the shell, and takes the role over from a
    ///    rule-2 guess.
    /// 2. **First-mapped fallback.** If no shell has been identified yet, the
    ///    first toplevel to reach the policy takes the role *provisionally*.
    ///
    /// Rule 2 is not redundant, and the reason is a hard ordering fact about
    /// gpui, checked against the pinned source rather than assumed
    /// (`~/.cargo/git/checkouts/zed-a70e2ad075855582/7a7c3e1`, the rev
    /// `duduclaw-shell/Cargo.toml` pins): `WaylandWindow::new` issues its
    /// "kick things off" `surface.commit()`
    /// (`gpui_linux/src/linux/wayland/window.rs:787`) **before**
    /// `gpui/src/window.rs:1802` calls `platform_window.set_app_id(…)`. So the
    /// shell's very first commit — the one that triggers the initial
    /// configure, i.e. the one that decides its size — arrives with `app_id`
    /// still unset. Without rule 2 the shell would be sized into the work area
    /// on boot and its own menu bar/dock would be clipped by exactly the bands
    /// meant to protect them. `XdgShellHandler::app_id_changed` then confirms
    /// (or corrects) the guess a moment later.
    ///
    /// A second window declaring [`SHELL_APP_ID`] after the role is already
    /// **confirmed** does *not* steal it: a shell that opens an auxiliary
    /// toplevel must not get a second full-screen window.
    fn classify_shell_window(&mut self, window: &Window) -> bool {
        let surface = window.toplevel().unwrap().wl_surface().clone();
        let matches_app_id = Self::window_app_id(window).as_deref() == Some(self.shell_app_id.as_str());

        if self.shell_surface.as_ref() == Some(&surface) {
            if matches_app_id && !self.shell_confirmed {
                self.shell_confirmed = true;
                tracing::info!(
                    surface_id = ?surface.id(),
                    app_id = %self.shell_app_id,
                    "window_policy: session shell confirmed by app_id"
                );
            }
            return true;
        }

        if matches_app_id && !self.shell_confirmed {
            let demoted = self.shell_surface.replace(surface.clone());
            self.shell_confirmed = true;
            tracing::info!(
                surface_id = ?surface.id(),
                app_id = %self.shell_app_id,
                demoted = ?demoted.as_ref().map(|s| s.id()),
                "window_policy: session shell identified by app_id (superseding the first-mapped guess)"
            );
            if let Some(previous) = demoted {
                self.reapply_window_policy_for_surface(&previous);
            }
            return true;
        }

        if self.shell_surface.is_none() {
            self.shell_surface = Some(surface.clone());
            tracing::info!(
                surface_id = ?surface.id(),
                "window_policy: no shell identified yet — treating the first mapped toplevel as the session shell (provisional)"
            );
            return true;
        }

        false
    }

    /// Applies the reserved-band policy to one toplevel: pending size, and
    /// position on the space.
    ///
    /// Sends the configure itself **only** when the initial configure has
    /// already gone out. On the initial-configure path
    /// (`handlers/xdg_shell.rs::handle_commit`) the send stays where it was,
    /// so exactly one configure carries the size — and, for a client that
    /// created an `zxdg_toplevel_decoration_v1` before its first commit (gpui
    /// and Chromium both do), the decoration mode too.
    ///
    /// **Shadow-workspace windows are left alone.** A window sitting inside
    /// the CD-2 shadow output's bounds keeps the pre-WM-1 behaviour byte for
    /// byte — full primary-output size on its initial configure, and after
    /// that nothing at all: its size and position are owned by
    /// `codrive/shadow.rs`. The bands describe chrome on the *real* screen;
    /// the shadow output has none, and moving a window out of it would break
    /// the isolation `SHADOW_ORIGIN` exists to provide.
    pub fn apply_window_policy(&mut self, window: &Window) {
        let Some(output_geo) = self.layout_output_geometry() else {
            return;
        };
        let in_shadow = self.window_is_in_shadow(window);
        let toplevel = window.toplevel().unwrap();
        if in_shadow && toplevel.is_initial_configure_sent() {
            // An already-configured shadow window: hands off entirely.
            return;
        }
        // A window that maps straight into the shadow workspace is not a
        // candidate for the session-shell role either: it is agent-side work,
        // structurally isolated from the real screen, and letting it claim the
        // role would make the next real toplevel inherit the work area.
        let is_shell = if in_shadow {
            false
        } else {
            self.classify_shell_window(window)
        };
        // WM-2: a window that will never be server-decorated must not have been
        // told otherwise. Runs before the geometry is computed so the corrected
        // insets are what `floating_content_rect` sees.
        self.sync_decoration_mode(window, is_shell, in_shadow);
        // WM-2: the shell (and a shadow-workspace window) still gets the whole
        // output; everything else is floated inside the work area instead of
        // filling it. `floating_content_rect` is `&mut self` because a
        // first-time placement consumes a cascade slot.
        // WM-3: the work area may now come from a layer surface's exclusive
        // zone instead of the hard-coded bands. Computed here, once, and passed
        // down — `floating_content_rect` used to derive it itself from
        // `output_geo`, which would have silently kept using the bands.
        let work = self
            .layout_work_area()
            .unwrap_or_else(|| work_area(output_geo, self.reserved_bands));
        let rect = if in_shadow {
            Rectangle::new(output_geo.loc, output_geo.size)
        } else if is_shell {
            // The shell still gets the whole output: it paints the chrome that
            // defines the work area, whichever mechanism defines it.
            window_rect(output_geo, self.reserved_bands, true)
        } else {
            self.floating_content_rect(window, work)
        };

        toplevel.with_pending_state(|state| {
            state.size = Some(rect.size);
        });

        if !in_shadow && self.space.element_location(window) != Some(rect.loc) {
            // NOTE: `Space::map_element` on an already-mapped element removes
            // and re-inserts it, which puts it at the top of its z-index group
            // (smithay 0.7.0 `desktop/space/mod.rs::insert_elem` — push then
            // stable sort by `z_index`). That is why this is guarded by an
            // "is it already there" check instead of being called
            // unconditionally: a re-apply must not silently raise a background
            // window. On the initial-configure path the window is the newest
            // one anyway, so the raise is a no-op.
            self.space.map_element(window.clone(), rect.loc, false);
        }

        tracing::debug!(
            surface_id = ?toplevel.wl_surface().id(),
            is_shell,
            in_shadow,
            rect = ?(rect.loc.x, rect.loc.y, rect.size.w, rect.size.h),
            reserved = ?(self.reserved_bands.top, self.reserved_bands.bottom),
            "window_policy: applied"
        );

        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
        self.queue_redraw();
    }

    pub(crate) fn window_is_in_shadow(&self, window: &Window) -> bool {
        let Some(loc) = self.space.element_location(window) else {
            return false;
        };
        let Some(shadow_geo) = self.space.output_geometry(&self.shadow_output) else {
            return false;
        };
        shadow_geo.contains(loc)
    }

    /// WM-2 alias used from `decor::paint` (which needs the same answer while
    /// deciding whether a window may be decorated at all). Separate name
    /// rather than a second implementation.
    pub(crate) fn window_is_in_shadow_public(&self, window: &Window) -> bool {
        self.window_is_in_shadow(window)
    }

    /// WM-2: the work area of the layout output — the region a floating window
    /// lives inside, and the region every drag is clamped to.
    ///
    /// `None` when no real output is mapped yet, exactly like
    /// [`Self::layout_output_geometry`].
    ///
    /// **WM-3** routed it through
    /// [`layer_shell::geometry::effective_work_area`](crate::layer_shell::geometry::effective_work_area):
    /// a layer surface that claims an exclusive zone now decides the work area,
    /// and the hard-coded [`ReservedBands`] are the fallback for as long as
    /// nothing claims one — which is the live case until `duduclaw-shell`
    /// migrates its dock and menu bar. See that function for why the two are
    /// alternatives rather than cumulative.
    pub fn layout_work_area(&self) -> Option<Rectangle<i32, Logical>> {
        let output_geo = self.layout_output_geometry()?;
        Some(crate::layer_shell::geometry::effective_work_area(
            output_geo,
            self.layer_non_exclusive_zone(),
            self.reserved_bands,
        ))
    }

    /// WM-2: where a **floating** (non-shell, non-shadow) toplevel's CONTENT
    /// rectangle belongs.
    ///
    /// Three cases, in order:
    ///
    /// 1. **Maximized** — fill the work area (the frame is the work area, so
    ///    the content is the work area minus this window's decoration).
    /// 2. **Already placed** — keep the remembered floating frame, refitted
    ///    into the (possibly changed) work area. This is the path a re-apply
    ///    takes: `app_id_changed`, a decoration-mode change, an output resize.
    ///    Note it reads the REMEMBERED frame, not the live geometry — see
    ///    `DecorState::frames` for why.
    /// 3. **New** — take the next cascade slot.
    fn floating_content_rect(
        &mut self,
        window: &Window,
        work: Rectangle<i32, Logical>,
    ) -> Rectangle<i32, Logical> {
        let insets = self.window_insets(window);
        let id = window.toplevel().unwrap().wl_surface().id();

        if self.decor.maximized.contains(&id) {
            return crate::decor::content_rect(work, insets);
        }

        let frame = match self.decor.frames.get(&id).copied() {
            Some(remembered) => crate::decor::refit_frame(remembered, work, insets),
            None => {
                let index = self.decor.cascade_next;
                self.decor.cascade_next = self.decor.cascade_next.wrapping_add(1);
                crate::decor::cascade_frame_rect(work, insets, index)
            }
        };
        self.decor.frames.insert(id, frame);
        crate::decor::content_rect(frame, insets)
    }

    /// WM-2: brings the remembered floating frame back in line with where the
    /// window actually is.
    ///
    /// Two callers, both of which move or resize a window behind the layout
    /// policy's back:
    ///
    /// * `grabs::MoveSurfaceGrab` — the human dragging the title bar;
    /// * `handlers::xdg_shell::handle_commit` — a client committing a size it
    ///   picked itself (xdg-shell lets a client answer a configure with a
    ///   smaller size).
    ///
    /// Without it, an output-mode change would refit the window to where it
    /// was *born* rather than where the user left it.
    ///
    /// Deliberately a no-op for a window that has never been placed (no entry
    /// to keep in step) and for a maximized one (its `frames` entry IS the
    /// restore geometry and must not be overwritten with the maximized rect),
    /// and it ignores a degenerate geometry — a client between buffers can
    /// briefly report 0×0, and storing that would restore a 1×1 window later.
    pub fn decor_sync_frame(&mut self, window: &Window) {
        let Some(toplevel) = window.toplevel() else {
            return;
        };
        let id = toplevel.wl_surface().id();
        if self.decor.maximized.contains(&id) || !self.decor.frames.contains_key(&id) {
            return;
        }
        let Some(current) = self.space.element_geometry(window) else {
            return;
        };
        if current.size.w <= 1 || current.size.h <= 1 {
            return;
        }
        let insets = self.window_insets(window);
        self.decor
            .frames
            .insert(id, crate::decor::frame_rect(current, insets));
    }

    /// WM-2: keeps the decoration mode comp **announces** identical to the one
    /// it actually **draws**.
    ///
    /// A client can legitimately ask for (and be granted) `ServerSide` before
    /// comp knows this window is the session shell: `new_decoration` fires
    /// before the first commit, and the shell role is only decided *on* that
    /// commit. `decor::mode::negotiated_ssd` then refuses to draw a title bar
    /// for it. Without this the client would have been told "the compositor
    /// decorates you" while nothing was drawn — the WM-1 bug (a window with no
    /// decoration at all) inverted.
    ///
    /// Crucially this **does not overwrite what the client negotiated**. The
    /// map keeps the client's own request forever; only the wire answer is
    /// recomputed from it plus the window's current role. That distinction is
    /// load-bearing rather than pedantic: the first toplevel to map is taken as
    /// the shell *provisionally* (`classify_shell_window` rule 2), so if a
    /// third-party window happens to map before `duduclaw-shell` does, it is
    /// briefly the shell and then demoted. Overwriting its stored mode during
    /// that window would have left it permanently undecorated with no way back.
    fn sync_decoration_mode(&mut self, window: &Window, is_shell: bool, in_shadow: bool) {
        let Some(toplevel) = window.toplevel() else {
            return;
        };
        let id = toplevel.wl_surface().id();
        // A client that never created a decoration object gets no answer —
        // "no entry" is a third state and must not be turned into one.
        let Some(negotiated) = self.decor.modes.get(&id).copied() else {
            return;
        };
        let effective = if crate::decor::negotiated_ssd(is_shell, in_shadow, Some(negotiated)) {
            crate::decor::DecorMode::ServerSide
        } else {
            crate::decor::DecorMode::ClientSide
        };
        // WM-3: capture what was already pending so the log below can fire on a
        // real CHANGE rather than on every re-apply. The policy re-runs far more
        // often now (every layer surface map/unmap moves the work area), and an
        // unconditional info! line turned "comp overrode the shell's decoration
        // mode" — a genuinely notable event — into background noise.
        let previously_announced =
            toplevel.with_pending_state(|state| state.decoration_mode);
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(effective.wire());
        });
        if effective != negotiated && previously_announced != Some(effective.wire()) {
            tracing::info!(
                surface_id = ?toplevel.wl_surface().id(),
                is_shell,
                in_shadow,
                negotiated = negotiated.as_str(),
                announced = effective.as_str(),
                "xdg_decoration: overriding the negotiated mode — comp never decorates the \
                 session shell or a shadow-workspace window (the client's own request is kept, \
                 so a demoted window gets its decoration back)"
            );
        }
    }

    /// [`DuduclawComp::apply_window_policy`] addressed by surface rather than
    /// by `Window` — used by the demotion path in `classify_shell_window`,
    /// which only holds the superseded surface.
    fn reapply_window_policy_for_surface(&mut self, surface: &WlSurface) {
        let Some(window) = self
            .space
            .elements()
            .find(|w| w.toplevel().unwrap().wl_surface() == surface)
            .cloned()
        else {
            return;
        };
        self.apply_window_policy(&window);
    }

    /// Re-applies the policy to every mapped toplevel.
    ///
    /// Called when the output geometry itself changes (the winit backend's
    /// `Resized` event). The udev backend builds its outputs once, during
    /// `init_udev`, before any client can connect — it has no mode-change or
    /// hotplug path today (see `udev_backend.rs`), so there is nothing to hook
    /// there; if one is ever added, it belongs here too.
    pub fn reapply_window_policy_all(&mut self) {
        let windows: Vec<Window> = self.space.elements().cloned().collect();
        for window in &windows {
            self.apply_window_policy(window);
        }
    }

    /// Clears the session-shell role when that window goes away, so a
    /// restarted shell can reclaim it. Called from
    /// `XdgShellHandler::toplevel_destroyed`.
    pub fn forget_shell_window(&mut self, destroyed: &WlSurface) {
        if self.shell_surface.as_ref() == Some(destroyed) {
            tracing::info!(
                surface_id = ?destroyed.id(),
                confirmed = self.shell_confirmed,
                "window_policy: session shell window destroyed — role is now unclaimed"
            );
            self.shell_surface = None;
            self.shell_confirmed = false;
        }
    }

    /// Super+Q — politely ask the focused window to close.
    ///
    /// `xdg_toplevel.close` is a *request*: the client may show a "save your
    /// work?" dialog or ignore it entirely. Nothing here kills a process; that
    /// is deliberate (an emergency stop already exists as Super+Esc, and it
    /// does something else entirely).
    ///
    /// The session shell is **never** a target. Closing it leaves a black
    /// screen with no way back, which is precisely the failure this whole work
    /// package is fixing.
    ///
    /// Reached only from the human seat's keyboard filter closure
    /// (`input.rs`), the same structural guarantee `Super+Esc`/`Super+Enter`/
    /// `Super+Tab` rely on: injected agent key events never enter that closure,
    /// so the agent cannot forge a window close.
    pub fn close_focused_window(&mut self) {
        let seat = self.seat.clone();
        let Some(keyboard) = seat.get_keyboard() else {
            return;
        };
        let Some(focus) = keyboard.current_focus() else {
            tracing::info!("window_policy: Super+Q with no keyboard focus — nothing to close");
            return;
        };

        // Keyboard focus during an open menu is the popup surface, not the
        // toplevel; resolve through the popup's root before giving up.
        let target = self.toplevel_window_for(&focus).or_else(|| {
            self.popups
                .find_popup(&focus)
                .and_then(|kind| smithay::desktop::find_popup_root_surface(&kind).ok())
                .and_then(|root| self.toplevel_window_for(&root))
        });

        let Some(window) = target else {
            tracing::info!(
                surface_id = ?focus.id(),
                "window_policy: Super+Q — focused surface is not a mapped toplevel, ignoring"
            );
            return;
        };

        self.close_window_politely(&window, "super+q");
    }

    /// The single "politely ask this window to close" path.
    ///
    /// WM-2 gave the compositor a second way to reach it (the title bar's ✕),
    /// so the shell refusal, the audit line and the `xdg_toplevel.close`
    /// semantics live here once rather than being duplicated per entry point.
    /// Both callers are human-only: Super+Q comes from the human seat's
    /// keyboard filter closure, the ✕ from the human pointer's button arm in
    /// `input.rs`. Neither is reachable from agent-injected input (see
    /// `codrive/mod.rs`'s separate seat), so the agent still cannot forge a
    /// window close.
    ///
    /// The session shell is **never** a target: closing it leaves a black
    /// screen with no way back.
    pub fn close_window_politely(&mut self, window: &Window, reason: &'static str) {
        let Some(toplevel) = window.toplevel() else {
            return;
        };
        let surface = toplevel.wl_surface().clone();
        if self.is_session_shell_surface(&surface) {
            tracing::warn!(
                surface_id = ?surface.id(),
                reason,
                "window_policy: close refused — that window is the session shell (closing it would leave a black screen)"
            );
            return;
        }

        tracing::info!(
            surface_id = ?surface.id(),
            reason,
            app_id = ?Self::window_app_id(window),
            "window_policy: sending xdg_toplevel.close"
        );
        toplevel.send_close();
    }

    pub(crate) fn toplevel_window_for(&self, surface: &WlSurface) -> Option<Window> {
        self.space
            .elements()
            .find(|w| w.toplevel().unwrap().wl_surface() == surface)
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Logical> {
        Rectangle::new(Point::from((x, y)), Size::from((w, h)))
    }

    #[test]
    fn the_default_bands_are_zero_now_that_the_shell_owns_its_own_chrome() {
        // WM-3 migration: the shell claims its 30px menu bar / 90px dock as
        // layer-shell exclusive zones now, so the compositor no longer
        // hard-codes those numbers — see this module's doc for the full
        // "intersection collapses to the zone when bands are zero" argument.
        let b = ReservedBands::default();
        assert_eq!(b.top, 0);
        assert_eq!(b.bottom, 0);
    }

    #[test]
    fn the_default_bands_reserve_nothing_at_all() {
        // The direct behavioural consequence of the assertion above: with no
        // env override and no layer surface in the picture, an ordinary
        // window now gets the WHOLE output — WM-1/WM-2's hard reservation is
        // gone, and whatever protects the shell's chrome from here on is
        // `layer_shell::geometry::effective_work_area`'s exclusive-zone
        // intersection, not this function.
        let full = rect(0, 0, 1280, 800);
        assert_eq!(work_area(full, ReservedBands::default()), full);
    }

    #[test]
    fn an_app_window_gets_the_output_minus_both_bands() {
        // Explicit (non-default) bands, matching the shell's own chrome
        // heights — this is what a legacy, unmigrated shell would configure
        // via DUDUCLAW_COMP_RESERVED_TOP/_BOTTOM. `work_area`'s own
        // arithmetic is what is under test here, independent of what the
        // production DEFAULT happens to be.
        let bands = ReservedBands { top: 30, bottom: 90 };
        let area = work_area(rect(0, 0, 1280, 800), bands);
        assert_eq!((area.loc.x, area.loc.y), (0, 30));
        assert_eq!((area.size.w, area.size.h), (1280, 680));
    }

    #[test]
    fn the_shell_itself_is_never_banded() {
        // Non-zero bands on purpose: with the zeroed default this assertion
        // would hold trivially even if the shell-exemption branch of
        // `window_rect` were broken, which would defeat the point of the test.
        let bands = ReservedBands { top: 30, bottom: 90 };
        let full = rect(0, 0, 1280, 800);
        let r = window_rect(full, bands, true);
        assert_eq!(r, full);
    }

    #[test]
    fn a_non_app_window_on_a_second_output_keeps_that_outputs_origin() {
        // udev maps additional connectors side by side at (w, 0).
        let bands = ReservedBands { top: 30, bottom: 90 };
        let area = work_area(rect(1280, 0, 1920, 1080), bands);
        assert_eq!((area.loc.x, area.loc.y), (1280, 30));
        assert_eq!((area.size.w, area.size.h), (1920, 960));
    }

    #[test]
    fn an_output_too_short_for_the_bands_falls_back_to_the_whole_output() {
        // 100px tall: 100 - 30 - 90 is negative. A negative-sized configure
        // would be worse than a covered dock.
        let bands = ReservedBands { top: 30, bottom: 90 };
        let tiny = rect(0, 0, 640, 100);
        assert_eq!(work_area(tiny, bands), tiny);
    }

    #[test]
    fn an_output_that_would_leave_a_sliver_falls_back_to_the_whole_output() {
        // 200 - 120 = 80 usable, below MIN_APP_HEIGHT.
        let bands = ReservedBands { top: 30, bottom: 90 };
        let short = rect(0, 0, 640, 200);
        assert_eq!(work_area(short, bands), short);
        // One pixel over the threshold is honoured.
        let ok = rect(0, 0, 640, 240);
        assert_eq!(work_area(ok, bands).size.h, 120);
    }

    #[test]
    fn a_degenerate_zero_size_output_is_returned_unchanged() {
        let zero = rect(0, 0, 0, 0);
        assert_eq!(work_area(zero, ReservedBands::default()), zero);
        let no_width = rect(0, 0, 0, 800);
        assert_eq!(work_area(no_width, ReservedBands::default()), no_width);
    }

    #[test]
    fn zero_bands_mean_no_reservation_at_all() {
        let full = rect(0, 0, 1280, 800);
        let bands = ReservedBands { top: 0, bottom: 0 };
        assert_eq!(work_area(full, bands), full);
    }

    #[test]
    fn env_overrides_are_parsed_per_band() {
        let b = ReservedBands::from_env_values(Some("48"), Some("120"));
        assert_eq!((b.top, b.bottom), (48, 120));
        // Whitespace is tolerated (env values pasted from a unit file).
        let b = ReservedBands::from_env_values(Some(" 12 "), None);
        assert_eq!((b.top, b.bottom), (12, DEFAULT_RESERVED_BOTTOM));
    }

    #[test]
    fn a_bad_env_value_keeps_that_bands_default_and_only_that_bands() {
        let d = ReservedBands::default();
        for bad in ["", "  ", "abc", "-1", "30px", "10001", "1.5"] {
            let b = ReservedBands::from_env_values(Some(bad), Some("77"));
            assert_eq!(b.top, d.top, "bad top {bad:?} must not change the default");
            assert_eq!(b.bottom, 77, "a bad top must never drag the bottom band with it");
        }
    }

    #[test]
    fn zero_is_a_legitimate_env_value_not_a_parse_failure() {
        let b = ReservedBands::from_env_values(Some("0"), Some("0"));
        assert_eq!((b.top, b.bottom), (0, 0));
    }

    #[test]
    fn the_shell_app_id_override_falls_back_on_empty_input() {
        assert_eq!(shell_app_id_from_env_value(None), SHELL_APP_ID);
        assert_eq!(shell_app_id_from_env_value(Some("   ")), SHELL_APP_ID);
        assert_eq!(shell_app_id_from_env_value(Some(" my.shell ")), "my.shell");
    }
}
