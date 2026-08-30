//! WM-3: the live half of Alt-Tab — the open switcher session, the key
//! handling that drives it, and the panel's render elements.
//!
//! The **pure** half (MRU bookkeeping, selection wrap-around, panel geometry,
//! scrolling) is [`crate::alt_tab`] and is fully unit-tested there. This module
//! is what that logic cannot be: it touches `Space`, `Seat`, live
//! `xdg_toplevel` identity and a GL renderer, none of which this crate has ever
//! constructed in a unit test (see `codrive/window_target.rs`'s module doc).
//!
//! ## The session's life
//!
//! ```text
//!  Alt (or Super) + Tab pressed ──► open(): snapshot candidates, select #1
//!  …still held, Tab again      ──► advance(): move the selection, wrap
//!  …still held, Shift+Tab      ──► advance(backwards)
//!  Alt/Super released          ──► commit(): focus (and un-minimize) the pick
//!  Escape while held           ──► cancel(): nothing changes
//! ```
//!
//! The session snapshots its candidate list **once**, at open. A window that
//! maps or dies mid-switch therefore cannot renumber the list under the user's
//! fingers; `commit` re-checks liveness instead, and a dead pick is answered by
//! doing nothing rather than by focusing whatever slid into that index.
//!
//! ## Why the panel's buffers are cached
//!
//! Exactly the reason `decor::paint`'s module doc gives: a fresh
//! `SolidColorBuffer`/`MemoryRenderBuffer` mints a fresh element `Id`, and an
//! element whose id changes every frame reads as brand new to
//! `OutputDamageTracker` — so a rebuilt-per-frame panel would page-flip the
//! udev backend at 60 Hz for as long as a key is held. The cache key is
//! `(labels, selected, panel width)`; holding Alt without pressing Tab rebuilds
//! nothing at all.

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            element::{
                memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
                solid::{SolidColorBuffer, SolidColorRenderElement},
                Kind,
            },
            gles::GlesRenderer,
        },
    },
    desktop::Window,
    reexports::wayland_server::{backend::ObjectId, Resource},
    utils::{Logical, Physical, Point, Rectangle, Scale, Size, Transform, SERIAL_COUNTER},
    wayland::seat::WaylandFocus,
};

use crate::{
    alt_tab::{
        initial_selection, label_width, max_rows_for, next_selection, panel_rect, row_rect,
        switch_order, visible_range, LABEL_FONT_PX, ROW_H, ROW_PAD_LEFT,
    },
    decor::{text::RasterizedText, BORDER_PX},
    render::CodriveElement,
    state::DuduclawComp,
};

/// An open Alt-Tab session.
pub struct SwitcherSession {
    /// Snapshotted at open, in switch order (MRU first).
    pub candidates: Vec<Window>,
    /// Index into [`Self::candidates`].
    pub selected: usize,
}

/// Everything the switcher keeps between frames: the session (present only
/// while a modifier is held) plus the id-stable render buffers.
#[derive(Default)]
pub struct SwitcherState {
    pub session: Option<SwitcherSession>,
    panel_bg: Option<SolidColorBuffer>,
    /// top, bottom, left, right.
    border: Option<[SolidColorBuffer; 4]>,
    rows: Vec<SolidColorBuffer>,
    labels: Vec<Option<MemoryRenderBuffer>>,
    label_sizes: Vec<(i32, i32)>,
    /// `(labels, selected, panel width)` the cache was built for.
    key: Option<(Vec<String>, usize, i32)>,
}

impl SwitcherState {
    /// Drops the cached buffers. Called when the session ends so a switcher
    /// that is not on screen holds no textures — and, since D2, also called
    /// from `DuduclawComp::set_theme` (`decor::mod`), which reuses this same
    /// drop-and-rebuild for the different reason of a stale theme rather than
    /// an ended session. `pub(crate)` for that second, cross-module caller.
    pub(crate) fn invalidate(&mut self) {
        self.rows.clear();
        self.labels.clear();
        self.label_sizes.clear();
        self.key = None;
    }
}

fn upload(raster: RasterizedText) -> MemoryRenderBuffer {
    MemoryRenderBuffer::from_slice(
        &raster.rgba,
        Fourcc::Abgr8888,
        (raster.width as i32, raster.height as i32),
        1,
        Transform::Normal,
        None,
    )
}

fn solid(buffer: &SolidColorBuffer, loc: Point<i32, Logical>, scale: Scale<f64>) -> CodriveElement {
    let phys: Point<i32, Physical> = loc.to_f64().to_physical_precise_round(scale);
    CodriveElement::Solid(SolidColorRenderElement::from_buffer(
        buffer,
        phys,
        scale,
        1.0,
        Kind::Unspecified,
    ))
}

fn memory(
    renderer: &mut GlesRenderer,
    buffer: &MemoryRenderBuffer,
    loc: Point<i32, Logical>,
    scale: Scale<f64>,
) -> Option<CodriveElement> {
    let phys: Point<i32, Physical> = loc.to_f64().to_physical_precise_round(scale);
    match MemoryRenderBufferRenderElement::from_buffer(
        renderer,
        phys.to_f64(),
        buffer,
        None,
        None,
        None,
        Kind::Unspecified,
    ) {
        Ok(e) => Some(CodriveElement::Memory(e)),
        Err(e) => {
            tracing::debug!(error = ?e, "switcher: renderer refused a panel buffer this frame");
            None
        }
    }
}

/// One row's label: the window's `xdg_toplevel.title`, else its `app_id`, else
/// a neutral placeholder. Same fallback chain (and the same reason for it) as
/// the title bar's own label in `decor::paint`.
fn window_label(window: &Window) -> String {
    let (app_id, title) = crate::codrive::window_target::window_identity(window);
    title
        .filter(|t| !t.trim().is_empty())
        .or_else(|| app_id.filter(|a| !a.trim().is_empty()))
        .unwrap_or_else(|| "未命名視窗".to_string())
}

impl DuduclawComp {
    /// Every window the switcher may land on, in switch order.
    ///
    /// Mapped windows **plus minimized ones** — a minimized window that could
    /// not be reached from the switcher would be unreachable full stop, since
    /// this compositor has no dock of its own. The session shell is excluded:
    /// it is the desktop, not a window you switch to, and "switching" to it
    /// would just take keyboard focus away from every application while
    /// changing nothing visible.
    fn switcher_candidates(&self) -> Vec<Window> {
        // A4 (CP-1, XWayland): an X11 window is a legitimate switcher
        // candidate too (it can never BE the session shell — that role is
        // only ever claimed by app_id, and X11 windows have no xdg app_id —
        // so `is_session_shell_surface` would answer `false` for one anyway),
        // but `w.toplevel().unwrap()` panics for one. The generic
        // `WaylandFocus::wl_surface()` accessor resolves for BOTH window
        // kinds, so this filter now requires it up front — a window with no
        // resolvable `wl_surface` at all (in practice: an X11 window whose
        // XWayland pairing hasn't landed yet) is excluded here rather than
        // further down, which is what keeps `present` and `ids` below in
        // guaranteed 1:1 correspondence without a second, possibly-shorter
        // filter pass desyncing the index lookup two blocks down.
        let mut present: Vec<Window> = self
            .space
            .elements()
            .rev()
            .filter(|w| {
                let Some(surface) = w.wl_surface() else {
                    return false;
                };
                !self.is_session_shell_surface(&surface) && !self.window_is_in_shadow_public(w)
            })
            .cloned()
            .collect();
        present.extend(self.minimized.iter().cloned());

        // Every entry in `present` is guaranteed to have `wl_surface().
        // is_some()`: the space half was filtered above, and the minimized
        // half is always Wayland-backed (X11 windows are never minimized —
        // see `crate::minimize`'s module doc, `minimize_window`/
        // `unminimize_window` both early-return on a `Window` with no
        // `toplevel()`).
        let ids: Vec<ObjectId> = present
            .iter()
            .map(|w| {
                w.wl_surface()
                    .expect("filtered to wl_surface-bearing windows above")
                    .id()
            })
            .collect();
        let ordered = switch_order(&self.focus_mru, &ids);
        ordered
            .into_iter()
            .filter_map(|id| {
                let idx = ids.iter().position(|candidate| candidate == &id)?;
                present.get(idx).cloned()
            })
            .collect()
    }

    /// Alt-Tab (or Super-Tab) was pressed. Opens the session if none is open,
    /// otherwise advances it. `backwards` is Shift being held.
    ///
    /// Returns `true` when the keystroke was consumed, so the caller can
    /// intercept it instead of forwarding a stray Tab into the focused client.
    pub fn switcher_press(&mut self, backwards: bool) -> bool {
        if let Some(session) = self.switcher.session.as_mut() {
            session.selected = next_selection(session.candidates.len(), session.selected, backwards);
            tracing::debug!(selected = session.selected, "switcher: selection advanced");
            self.queue_redraw();
            return true;
        }

        let candidates = self.switcher_candidates();
        if candidates.is_empty() {
            tracing::debug!("switcher: nothing to switch between");
            return false;
        }
        let mut selected = initial_selection(candidates.len());
        if backwards {
            // Shift on the very first press means "the least recently used",
            // i.e. one step the other way from where you already are.
            selected = next_selection(candidates.len(), 0, true);
        }
        tracing::info!(
            candidates = candidates.len(),
            selected,
            "switcher: opened"
        );
        self.switcher.session = Some(SwitcherSession {
            candidates,
            selected,
        });
        self.queue_redraw();
        true
    }

    /// The modifier was released — focus the selection and close.
    ///
    /// A pick that died while the switcher was open is answered by doing
    /// nothing: the snapshot deliberately does not renumber, so focusing "the
    /// window that is now at that index" would be focusing something the user
    /// never looked at.
    pub fn switcher_commit(&mut self) {
        let Some(session) = self.switcher.session.take() else {
            return;
        };
        self.switcher.invalidate();
        self.queue_redraw();

        let Some(window) = session.candidates.get(session.selected).cloned() else {
            return;
        };
        let Some(toplevel) = window.toplevel() else {
            return;
        };
        let id = toplevel.wl_surface().id();
        let alive = self.space.elements().any(|w| w == &window)
            || self.minimized.iter().any(|w| w == &window);
        if !alive {
            tracing::info!(
                surface_id = ?id,
                "switcher: the selected window went away while the switcher was open — doing nothing"
            );
            return;
        }

        tracing::info!(surface_id = ?id, "switcher: committing selection");
        if self.minimized.iter().any(|w| w == &window) {
            self.unminimize_window(&window);
        }
        let seat = self.seat.clone();
        let serial = SERIAL_COUNTER.next_serial();
        self.focus_window(&seat, Some(&window), serial);
    }

    /// Escape while the switcher is open — close it and change nothing.
    pub fn switcher_cancel(&mut self) {
        if self.switcher.session.take().is_some() {
            tracing::info!("switcher: cancelled");
            self.switcher.invalidate();
            self.queue_redraw();
        }
    }

    /// Drops a window from an open session (it was destroyed). The selection is
    /// clamped rather than moved, so the highlight does not jump under the
    /// user's fingers.
    pub(crate) fn switcher_forget(&mut self, window: &Window) {
        let empty = match self.switcher.session.as_mut() {
            Some(session) => {
                session.candidates.retain(|w| w != window);
                match session.candidates.len() {
                    0 => true,
                    n => {
                        session.selected = session.selected.min(n - 1);
                        false
                    }
                }
            }
            None => return,
        };
        if empty {
            self.switcher.session = None;
            self.switcher.invalidate();
        }
        self.queue_redraw();
    }

    /// The panel's render elements, in front-to-back order, positioned in
    /// `output`'s own coordinate space.
    ///
    /// Empty (and cheap) when no session is open — this runs on every frame.
    pub fn build_switcher_elements(
        &mut self,
        renderer: &mut GlesRenderer,
        output_geo: Rectangle<i32, Logical>,
        scale: Scale<f64>,
    ) -> Vec<CodriveElement> {
        // Snapshotted out of `self.switcher` immediately: everything below
        // mutates the same struct's cache, so holding a borrow of the session
        // across it would not compile (and cloning a `Window` is an `Arc`
        // bump, which is why this is free).
        let (candidates, selected) = match self.switcher.session.as_ref() {
            Some(s) => (s.candidates.clone(), s.selected),
            None => return Vec::new(),
        };
        let total = candidates.len();
        if total == 0 {
            return Vec::new();
        }
        // D2: one palette lookup for this whole call — see
        // `DuduclawComp::palette`'s doc for why this is cheap enough to just
        // recompute rather than cache.
        let palette = self.palette();

        // The panel is centred on the output, so everything below is computed
        // in output-local coordinates from the start — no global-to-local
        // conversion, unlike the per-window decorations.
        let local_output = Rectangle::new(Point::from((0, 0)), output_geo.size);
        let (start, len) = visible_range(total, selected, max_rows_for(local_output));
        if len == 0 {
            return Vec::new();
        }
        let panel = panel_rect(local_output, len);
        let selected_row = selected.saturating_sub(start);

        let labels: Vec<String> = candidates[start..start + len].iter().map(window_label).collect();
        let key = (labels.clone(), selected_row, panel.size.w);

        // Take the font set out for the duration of the mutable borrow of the
        // cache, exactly as `decor::paint::build_frame_elements` does.
        let fonts = self.decor.fonts.take();
        if self.switcher.key.as_ref() != Some(&key) {
            self.switcher.rows.clear();
            self.switcher.labels.clear();
            self.switcher.label_sizes.clear();
            for (i, label) in labels.iter().enumerate() {
                let row = row_rect(panel, i);
                let mut bg = SolidColorBuffer::default();
                bg.update(
                    (row.size.w, row.size.h),
                    if i == selected_row {
                        palette.switcher_row_selected
                    } else {
                        palette.switcher_row_idle
                    },
                );
                self.switcher.rows.push(bg);

                let raster = fonts.as_ref().and_then(|f| {
                    f.rasterize(label, LABEL_FONT_PX, palette.switcher_text, label_width(row))
                });
                match raster {
                    Some(r) => {
                        self.switcher.label_sizes.push((r.width as i32, r.height as i32));
                        self.switcher.labels.push(Some(upload(r)));
                    }
                    None => {
                        self.switcher.label_sizes.push((0, 0));
                        self.switcher.labels.push(None);
                    }
                }
            }
            tracing::debug!(
                rows = len,
                selected_row,
                panel = ?(panel.loc.x, panel.loc.y, panel.size.w, panel.size.h),
                "switcher: panel raster rebuilt"
            );
            self.switcher.key = Some(key);
        }
        self.decor.fonts = fonts;

        {
            let bg = self.switcher.panel_bg.get_or_insert_with(SolidColorBuffer::default);
            bg.update((panel.size.w, panel.size.h), palette.switcher_bg);
        }
        let border_geo: [(Point<i32, Logical>, Size<i32, Logical>); 4] = [
            (panel.loc, Size::from((panel.size.w, BORDER_PX))),
            (
                Point::from((panel.loc.x, panel.loc.y + panel.size.h - BORDER_PX)),
                Size::from((panel.size.w, BORDER_PX)),
            ),
            (
                Point::from((panel.loc.x, panel.loc.y + BORDER_PX)),
                Size::from((BORDER_PX, (panel.size.h - 2 * BORDER_PX).max(0))),
            ),
            (
                Point::from((panel.loc.x + panel.size.w - BORDER_PX, panel.loc.y + BORDER_PX)),
                Size::from((BORDER_PX, (panel.size.h - 2 * BORDER_PX).max(0))),
            ),
        ];
        {
            let border = self
                .switcher
                .border
                .get_or_insert_with(|| std::array::from_fn(|_| SolidColorBuffer::default()));
            for (i, (_, size)) in border_geo.iter().enumerate() {
                border[i].update(*size, palette.switcher_border);
            }
        }

        // Front-to-back: labels, row fills, border, panel background.
        let mut out: Vec<CodriveElement> = Vec::with_capacity(len * 2 + 5);
        for i in 0..len {
            let row = row_rect(panel, i);
            let label = self.switcher.labels.get(i).and_then(|l| l.as_ref()).cloned();
            let size = self.switcher.label_sizes.get(i).copied();
            if let (Some(tex), Some((tw, th))) = (label, size) {
                if tw > 0 && th > 0 {
                    let loc = Point::from((row.loc.x + ROW_PAD_LEFT, row.loc.y + (ROW_H - th) / 2));
                    if let Some(e) = memory(renderer, &tex, loc, scale) {
                        out.push(e);
                    }
                }
            }
            if let Some(fill) = self.switcher.rows.get(i) {
                out.push(solid(fill, row.loc, scale));
            }
        }
        if let Some(border) = self.switcher.border.as_ref() {
            for (i, (loc, size)) in border_geo.iter().enumerate() {
                if size.w > 0 && size.h > 0 {
                    out.push(solid(&border[i], *loc, scale));
                }
            }
        }
        if let Some(bg) = self.switcher.panel_bg.as_ref() {
            out.push(solid(bg, panel.loc, scale));
        }
        out
    }
}
