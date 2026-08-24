//! WM-2: turning a window's frame geometry into render elements, and the
//! per-output element assembly that replaces `desktop::space::render_output`.
//!
//! # Why this file stops calling `render_output`
//!
//! `smithay::desktop::space::render_output` builds its element list as
//! `[custom_elements…, every window's surfaces…]` — **all** custom elements
//! above **all** windows. That is exactly right for the things it was used for
//! before this round (the two cursors, the codrive highlight box, the shadow
//! PiP: overlays that genuinely belong on top of everything). It is exactly
//! wrong for a title bar, which belongs to *one* window and must sit below any
//! window stacked above it. With WM-1's fill-the-work-area layout the question
//! never arose because windows never overlapped; with WM-2's floating cascade
//! it arises immediately, and "a background window's title bar floating over
//! the foreground window" is the sort of thing that reads as a broken
//! compositor rather than a small bug.
//!
//! So this module assembles the list itself, interleaved per window:
//!
//! ```text
//!   [ human cursor, agent cursor, highlight, PiP ]      <- unchanged overlays
//!   for each window, top of the z-order first:
//!       [ close glyph, close bg, title text, title bar, border ]
//!       [ the window's own surfaces (and its popups) ]
//!       [ drop shadow rings ]
//! ```
//!
//! and hands the result to `OutputDamageTracker::render_output`, which is the
//! exact call `desktop::space::render_output` makes at the end of its own
//! body. Everything else about the frame — damage tracking, the "no damage ⇒
//! no page flip" property the udev backend's idle CPU depends on, the output
//! transform — is byte-for-byte the same code path as before.
//!
//! Faithfully reproduced from `space_render_elements` / `render_elements_for_
//! region` (smithay 0.7.0 `desktop/space/mod.rs`, read rather than
//! remembered), because getting either wrong is silent:
//!
//! * the render location is `element_location − element.geometry().loc −
//!   output.loc` — the middle term is what makes a client with CSD shadows
//!   land correctly, and dropping it shifts every GTK window by its shadow
//!   width;
//! * a window is skipped entirely unless its bounding box overlaps this
//!   output. That single test is what keeps CD-2's shadow workspace (mapped
//!   100 000 px down at `codrive::SHADOW_ORIGIN`) off the real screen, so it
//!   is preserved verbatim — widened only to include the drop shadow, which
//!   extends 8 px beyond the frame.
//!
//! There is no layer-shell branch because this crate has no layer-shell (see
//! `main.rs`'s "what this spike deliberately does not carry over" list).
//!
//! # Why every buffer is cached per window
//!
//! `SolidColorBuffer::new` and `MemoryRenderBuffer::from_slice` each mint a
//! **fresh element `Id`**. An element whose id changes every frame is, to
//! `OutputDamageTracker`, a brand-new element every frame — so a freshly-built
//! title bar would report damage on every composite, the udev backend would
//! page-flip 60 times a second on a completely idle desktop, and the property
//! that module's doc calls "the entire reason idle CPU is not pinned" would be
//! gone. Cached buffers are mutated through `SolidColorBuffer::update`, which
//! keeps the id and only bumps the commit counter when something actually
//! changed. The two rasterised buffers (title text, close ✕) are rebuilt only
//! when their inputs change — the title string and available width for one,
//! the hover state for the other.

use std::collections::HashMap;

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            element::{
                memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
                solid::{SolidColorBuffer, SolidColorRenderElement},
                surface::render_elements_from_surface_tree,
                Kind,
            },
            gles::GlesRenderer,
        },
    },
    desktop::{PopupManager, Window},
    output::Output,
    reexports::wayland_server::{backend::ObjectId, Resource},
    utils::{Logical, Physical, Point, Rectangle, Scale, Size, Transform},
};

use crate::{render::CodriveElement, state::DuduclawComp};

use super::{
    close_button_rect, frame_rect, minimize_button_rect, minus, mode::DecorMode, shadow_bounds,
    text::FontSet, title_bar_rect, title_text_rect, xmark, DecorInsets, BORDER_PX,
    SHADOW_ALPHAS, SHADOW_RING_PX, TITLE_BAR_H, TITLE_FONT_PX,
};

/// Side length of the close ✕, in logical pixels.
const CLOSE_GLYPH_PX: u32 = 12;

/// Number of solid bars a full shadow costs: four per ring.
const SHADOW_BARS: usize = SHADOW_ALPHAS.len() * 4;

/// Everything WM-2 keeps that is not already in `Space`.
pub struct DecorState {
    /// Both embedded faces, or `None` if an embedded asset failed to parse
    /// (see [`FontSet::load`] — a title bar without text is still usable).
    pub fonts: Option<FontSet>,
    /// Per-toplevel negotiated decoration owner. Absence means "this client
    /// never created a `zxdg_toplevel_decoration_v1`", which is a distinct
    /// state from `ClientSide` — see `decor::mode`'s table.
    pub modes: HashMap<ObjectId, DecorMode>,
    /// Per-window render buffers. Keyed the same way, populated lazily on
    /// first paint, dropped by [`DuduclawComp::forget_window_decor`].
    buffers: HashMap<ObjectId, WindowDecorBuffers>,
    /// Each toplevel's **floating** frame rectangle — where it sits when it is
    /// not maximized, and therefore also the restore geometry an unmaximize
    /// goes back to. WM-1 explicitly kept no restore geometry (`A5 owns
    /// that`); with floating windows an unmaximize has to have somewhere to
    /// return to, so WM-2 keeps one.
    ///
    /// Deliberately a remembered rectangle rather than "read the window's live
    /// geometry": `app_id_changed` can fire before the client has ever
    /// committed a buffer, and a re-apply that read a 0×0 live geometry at
    /// that moment would configure a 1×1 window.
    pub frames: HashMap<ObjectId, Rectangle<i32, Logical>>,
    /// Toplevels currently maximized (i.e. filling the work area).
    pub maximized: std::collections::HashSet<ObjectId>,
    /// The close button the human pointer is currently over, if any.
    pub hovered_close: Option<ObjectId>,
    /// WM-3: the same, for the minimize button. Two fields rather than one
    /// `Option<(ObjectId, Button)>` because the two are independent by
    /// construction (a pointer is over at most one, but the render path asks
    /// about each separately) and because a single field would make "the
    /// pointer left the close button onto the minimize button" a two-step
    /// update with an inconsistent frame in between.
    pub hovered_minimize: Option<ObjectId>,
    /// Monotonic cascade counter. Deliberately never reset — see
    /// [`super::placement::cascade_frame_rect`].
    pub cascade_next: u32,
}

impl DecorState {
    pub fn new() -> Self {
        let fonts = FontSet::load();
        if fonts.is_none() {
            // Not a panic: an embedded font that fails to parse is a build
            // mistake, and a compositor that refuses to start is a much worse
            // outcome than one whose title bars are blank.
            tracing::error!(
                "decor: embedded title fonts failed to parse — title bars will have no text"
            );
        }
        Self {
            fonts,
            modes: HashMap::new(),
            buffers: HashMap::new(),
            frames: HashMap::new(),
            maximized: std::collections::HashSet::new(),
            hovered_close: None,
            hovered_minimize: None,
            cascade_next: 0,
        }
    }
}

impl Default for DecorState {
    fn default() -> Self {
        Self::new()
    }
}

impl DecorState {
    /// D2: drops every cached per-window decoration buffer, forcing a full
    /// rebuild (fresh `SolidColorBuffer`s, and — because `title_key`/
    /// `close_key`/`minimize_key` live on the dropped `WindowDecorBuffers`
    /// too — a fresh rasterisation of every glyph) on the next
    /// `build_frame_elements` call for each window.
    ///
    /// Called from `DuduclawComp::set_theme` (`decor::mod`) ONLY — a theme
    /// switch is the one event where every cached colour genuinely needs to
    /// change, and it is a rare, deliberate user action, not a per-frame one,
    /// so paying for a full re-rasterisation here is cheap. `modes` / `frames`
    /// / `maximized` / `hovered_close` / `hovered_minimize` / `cascade_next`
    /// are untouched: none of them describe a colour, so none of them are
    /// stale after a theme change.
    pub(crate) fn invalidate_theme_cache(&mut self) {
        self.buffers.clear();
    }
}

/// The cached, id-stable buffers for one window's decoration.
struct WindowDecorBuffers {
    title_bg: SolidColorBuffer,
    close_bg: SolidColorBuffer,
    /// WM-3: the minimize button's hover fill.
    minimize_bg: SolidColorBuffer,
    /// top, bottom, left, right.
    border: [SolidColorBuffer; 4],
    /// Innermost ring first, four bars each (top, bottom, left, right).
    shadow: [SolidColorBuffer; SHADOW_BARS],
    title_tex: Option<MemoryRenderBuffer>,
    /// `(text, available width)` the cached raster was produced for.
    title_key: Option<(String, i32)>,
    title_size: (i32, i32),
    close_tex: Option<MemoryRenderBuffer>,
    /// Hover state the cached ✕ was rasterised for.
    close_key: Option<bool>,
    /// WM-3: the `－` glyph and the hover state it was rasterised for.
    minimize_tex: Option<MemoryRenderBuffer>,
    minimize_key: Option<bool>,
}

impl WindowDecorBuffers {
    fn new() -> Self {
        Self {
            title_bg: SolidColorBuffer::default(),
            close_bg: SolidColorBuffer::default(),
            minimize_bg: SolidColorBuffer::default(),
            border: std::array::from_fn(|_| SolidColorBuffer::default()),
            shadow: std::array::from_fn(|_| SolidColorBuffer::default()),
            title_tex: None,
            title_key: None,
            title_size: (0, 0),
            close_tex: None,
            close_key: None,
            minimize_tex: None,
            minimize_key: None,
        }
    }
}

/// One solid bar, positioned in this output's own coordinate space.
fn solid(
    buffer: &SolidColorBuffer,
    loc: Point<i32, Logical>,
    scale: Scale<f64>,
) -> CodriveElement {
    let phys: Point<i32, Physical> = loc.to_f64().to_physical_precise_round(scale);
    CodriveElement::Solid(SolidColorRenderElement::from_buffer(
        buffer,
        phys,
        scale,
        1.0,
        Kind::Unspecified,
    ))
}

/// One rasterised RGBA buffer, positioned in this output's own coordinate
/// space. `None` when the renderer refuses the import — a missing title is
/// never a panic on the render path (same rule `cursor/mod.rs` applies to an
/// unimportable cursor image).
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
            tracing::debug!(error = ?e, "decor: renderer refused a decoration buffer this frame");
            None
        }
    }
}

/// This window's own toplevel surface tree (subsurfaces included, popups
/// excluded).
///
/// Together with [`popup_elements`] this is `AsRenderElements for Window`
/// (smithay 0.7.0 `desktop/space/wayland/window.rs`) split in two, and the two
/// halves are copied verbatim from it — same call, same arguments, same
/// `Kind`. The split exists only so the decoration can be layered *between*
/// them; see the comment at the call site. The `xwayland` arm of the original
/// is dropped because this crate has no XWayland support (`main.rs`'s "what
/// this spike deliberately does not carry over" list), so `underlying_surface`
/// is always the Wayland one.
fn toplevel_elements(
    renderer: &mut GlesRenderer,
    window: &Window,
    location: Point<i32, Physical>,
    scale: Scale<f64>,
) -> Vec<CodriveElement> {
    let Some(toplevel) = window.toplevel() else {
        return Vec::new();
    };
    render_elements_from_surface_tree(
        renderer,
        toplevel.wl_surface(),
        location,
        scale,
        1.0,
        Kind::Unspecified,
    )
}

/// Every popup currently anchored to this window, positioned by the same
/// formula upstream uses. See [`toplevel_elements`] for the provenance note.
fn popup_elements(
    renderer: &mut GlesRenderer,
    window: &Window,
    location: Point<i32, Physical>,
    scale: Scale<f64>,
) -> Vec<CodriveElement> {
    let Some(toplevel) = window.toplevel() else {
        return Vec::new();
    };
    let surface = toplevel.wl_surface();
    let mut out = Vec::new();
    for (popup, popup_offset) in PopupManager::popups_for_surface(surface) {
        let offset = (window.geometry().loc + popup_offset - popup.geometry().loc)
            .to_physical_precise_round(scale);
        out.extend(render_elements_from_surface_tree::<GlesRenderer, CodriveElement>(
            renderer,
            popup.wl_surface(),
            location + offset,
            scale,
            1.0,
            Kind::Unspecified,
        ));
    }
    out
}

fn upload(raster: super::text::RasterizedText) -> MemoryRenderBuffer {
    MemoryRenderBuffer::from_slice(
        &raster.rgba,
        // Byte order R,G,B,A == ABGR8888 words on little-endian; the same
        // choice, for the same reason, as `cursor/fallback.rs`.
        Fourcc::Abgr8888,
        (raster.width as i32, raster.height as i32),
        1,
        Transform::Normal,
        None,
    )
}

impl DuduclawComp {
    /// The decoration insets for one window: `SSD` when comp draws its title
    /// bar, `NONE` otherwise.
    pub fn window_insets(&self, window: &Window) -> DecorInsets {
        DecorInsets::for_ssd(self.window_uses_ssd(window))
    }

    /// Does comp draw a server-side decoration for this window?
    pub fn window_uses_ssd(&self, window: &Window) -> bool {
        let Some(toplevel) = window.toplevel() else {
            return false;
        };
        let surface = toplevel.wl_surface();
        let is_shell = self.is_session_shell_surface(surface);
        let in_shadow = self.window_is_in_shadow_public(window);
        let negotiated = self.decor.modes.get(&surface.id()).copied();
        super::negotiated_ssd(is_shell, in_shadow, negotiated)
    }

    /// The frame rectangle of one window as it currently sits on the space,
    /// or `None` if the window is not mapped.
    pub fn window_frame_rect(&self, window: &Window) -> Option<Rectangle<i32, Logical>> {
        let content = self.space.element_geometry(window)?;
        Some(frame_rect(content, self.window_insets(window)))
    }

    /// Drops every cached artefact for a destroyed toplevel.
    ///
    /// Called from `XdgShellHandler::toplevel_destroyed`. Without it the four
    /// maps here would grow for the lifetime of the session — small per entry,
    /// but unbounded, and `ObjectId`s are never reused so nothing would ever
    /// evict them.
    pub fn forget_window_decor(&mut self, surface_id: &ObjectId) {
        self.decor.modes.remove(surface_id);
        self.decor.buffers.remove(surface_id);
        self.decor.frames.remove(surface_id);
        self.decor.maximized.remove(surface_id);
        if self.decor.hovered_close.as_ref() == Some(surface_id) {
            self.decor.hovered_close = None;
        }
        if self.decor.hovered_minimize.as_ref() == Some(surface_id) {
            self.decor.hovered_minimize = None;
        }
    }

    /// Every render element for one output, in front-to-back order.
    ///
    /// `overlays` are the elements that genuinely belong above every window
    /// (the two cursors, the codrive highlight, the shadow PiP) — passed in
    /// rather than built here so the backends keep owning the exact order they
    /// already had.
    pub fn build_output_elements(
        &mut self,
        renderer: &mut GlesRenderer,
        output: &Output,
        mut overlays: Vec<CodriveElement>,
    ) -> Vec<CodriveElement> {
        let Some(output_geo) = self.space.output_geometry(output) else {
            // The output is not mapped on this space. `space_render_elements`
            // skips it in exactly the same way.
            return overlays;
        };
        // WP-comp-shell-display D4b-3: single source of truth, shared with
        // every custom overlay element built for this same frame — see
        // `render::output_render_scale`'s own doc.
        let scale = crate::render::output_render_scale(output);

        let focused = self
            .seat
            .get_keyboard()
            .and_then(|k| k.current_focus());

        // D9-bug4 (2026-08-24): a locked session paints layer surfaces and the
        // cursor, and nothing else. The two affordances below are both
        // window-derived — the switcher panel lists every open window's title
        // and icon, and the IME candidate window can only be showing a
        // composition started before the lock — so both are disclosure on a
        // locked screen and both are skipped. See `crate::session_lock`.
        //
        // The switcher cannot be OPENED while locked either (`session_lock::
        // gesture_allowed_while_locked` swallows Alt-Tab), so this only ever
        // hides a session left open by the lock itself.
        let locked = self.session_locked;

        if !locked {
            // WM-3: the Alt-Tab panel. Below the cursors (which stay on top of
            // everything, as they must) and above every surface — it is a modal
            // affordance for as long as it is on screen. Empty and cheap when no
            // switcher session is open, which is almost always.
            let switcher = self.build_switcher_elements(renderer, output_geo, scale);
            overlays.extend(switcher);

            // D3-a: the IME candidate window. Above every window AND above every
            // layer surface — it is transient input UI anchored to a caret, so a
            // panel drawn over it would hide the very characters the user is
            // choosing between. Below the Alt-Tab panel (which is modal while it
            // is up) and below the cursors, which stay on top of everything.
            // Empty and cheap whenever no composition is in flight, which is
            // almost always.
            overlays.extend(self.ime_popup_elements(renderer, output_geo, scale));
        }

        // WM-3: layer surfaces on the `overlay` and `top` layers, in that
        // order. See `layer_shell::geometry` for why this crate ranks the four
        // bands explicitly instead of using smithay's two-way split.
        overlays.extend(self.layer_elements(renderer, output, scale, true));

        // Top of the stack first: `Space::elements()` yields back-to-front.
        //
        // D9-bug4: empty while locked — this is the whole visual half of the
        // session lock. Deliberately only the ELEMENT list: both backends still
        // call `send_frame` over `space.elements()` on every repaint, so clients
        // keep receiving frame callbacks, keep animating, and reappear intact
        // (rather than as a stalled last frame) the moment the shell unlocks.
        let windows: Vec<Window> = if locked {
            Vec::new()
        } else {
            self.space.elements().rev().cloned().collect()
        };

        for window in windows {
            let (Some(content), Some(bbox), Some(location)) = (
                self.space.element_geometry(&window),
                self.space.element_bbox(&window),
                self.space.element_location(&window),
            ) else {
                continue;
            };
            let insets = self.window_insets(&window);
            let frame = frame_rect(content, insets);
            let outer = shadow_bounds(frame, insets);
            if !output_geo.overlaps(bbox) && !output_geo.overlaps(outer) {
                // Not on this output — the CD-2 shadow workspace lands here.
                continue;
            }

            let (above, below) = if insets.is_decorated() {
                let is_focused = match (focused.as_ref(), window.toplevel()) {
                    (Some(f), Some(t)) => f == t.wl_surface(),
                    _ => false,
                };
                self.build_frame_elements(
                    renderer,
                    &window,
                    frame,
                    insets,
                    is_focused,
                    output_geo.loc,
                    scale,
                )
            } else {
                (Vec::new(), Vec::new())
            };

            // Faithful copy of `Space::render_elements_for_region`'s formula.
            let render_loc = location - window.geometry().loc - output_geo.loc;
            let phys: Point<i32, Physical> = render_loc.to_f64().to_physical_precise_round(scale);

            // Popups go ABOVE the decoration, the toplevel's own surface goes
            // below it. `Window::render_elements` returns both in one vec with
            // the popups first, so honouring that split means expanding it here
            // — see `window_surface_elements` for the copied formula and why.
            //
            // Order matters in exactly one case, and it is a real one: an
            // xdg-positioner is free to place a popup at a negative offset from
            // its parent, i.e. over the title bar. Drawing the decoration on
            // top of everything would clip such a menu; drawing it *under* the
            // whole window would let a client whose surface overruns its own
            // declared geometry hide its own close button. Splitting gets both
            // right.
            overlays.extend(popup_elements(renderer, &window, phys, scale));
            overlays.extend(above);
            overlays.extend(toplevel_elements(renderer, &window, phys, scale));
            overlays.extend(below);
        }

        // WM-3: `bottom` then `background`, under every window.
        overlays.extend(self.layer_elements(renderer, output, scale, false));

        overlays
    }

    /// WM-3: the render elements of one output's layer surfaces, for the
    /// layers on the requested side of the window stack, front-most first.
    ///
    /// A layer's geometry is **output-local** (smithay's `LayerMap` arranges
    /// against `Rectangle::from_size(mode)`), which is why — unlike the window
    /// loop above — nothing here subtracts `output_geo.loc`. Same formula
    /// smithay's own `space_render_elements` uses, read rather than
    /// remembered.
    fn layer_elements(
        &self,
        renderer: &mut GlesRenderer,
        output: &Output,
        scale: Scale<f64>,
        above_windows: bool,
    ) -> Vec<CodriveElement> {
        use crate::layer_shell::geometry::{is_above_windows, LAYERS_FRONT_TO_BACK};
        use smithay::{backend::renderer::element::AsRenderElements, desktop::layer_map_for_output};

        let map = layer_map_for_output(output);
        let mut out: Vec<CodriveElement> = Vec::new();
        for layer in LAYERS_FRONT_TO_BACK
            .into_iter()
            .filter(|l| is_above_windows(*l) == above_windows)
        {
            // Within one layer the most recently mapped surface is front-most,
            // matching upstream's `.rev()` over insertion order.
            for surface in map.layers_on(layer).rev() {
                let Some(geo) = map.layer_geometry(surface) else {
                    continue;
                };
                let loc: Point<i32, Physical> = geo.loc.to_physical_precise_round(scale);
                out.extend(AsRenderElements::<GlesRenderer>::render_elements::<CodriveElement>(
                    surface, renderer, loc, scale, 1.0,
                ));
            }
        }
        out
    }

    /// Builds one window's decoration, returning `(above the content, below
    /// the content)`.
    #[allow(clippy::too_many_arguments)]
    fn build_frame_elements(
        &mut self,
        renderer: &mut GlesRenderer,
        window: &Window,
        frame: Rectangle<i32, Logical>,
        insets: DecorInsets,
        is_focused: bool,
        output_loc: Point<i32, Logical>,
        scale: Scale<f64>,
    ) -> (Vec<CodriveElement>, Vec<CodriveElement>) {
        let Some(toplevel) = window.toplevel() else {
            return (Vec::new(), Vec::new());
        };
        let surface_id = toplevel.wl_surface().id();
        let Some(bar) = title_bar_rect(frame, insets) else {
            return (Vec::new(), Vec::new());
        };
        // D2: the one palette lookup for this whole call — see
        // `DuduclawComp::palette`'s doc for why this is cheap enough to just
        // recompute rather than cache.
        let palette = self.palette();

        let (app_id, title) = crate::codrive::window_target::window_identity(window);
        // A window with no title yet falls back to its app_id, then to a
        // neutral label — never to an empty bar, which reads as "this window
        // is broken" rather than "this window has not set a title".
        let label = title
            .filter(|t| !t.trim().is_empty())
            .or_else(|| app_id.filter(|a| !a.trim().is_empty()))
            .unwrap_or_else(|| "未命名視窗".to_string());

        let hovered = self.decor.hovered_close.as_ref() == Some(&surface_id);
        let hovered_min = self.decor.hovered_minimize.as_ref() == Some(&surface_id);
        let text_rect = title_text_rect(bar);
        let close = close_button_rect(bar);
        let minimize = minimize_button_rect(bar);

        // The font set is read-only here but lives next to the mutable buffer
        // map, so it is taken out for the duration of the borrow and put back.
        // Cheaper and clearer than an `Rc`, and impossible to forget because
        // the compiler enforces the reassignment below.
        let fonts = self.decor.fonts.take();
        let entry = self
            .decor
            .buffers
            .entry(surface_id.clone())
            .or_insert_with(WindowDecorBuffers::new);

        // --- title bar background -------------------------------------------
        let bar_color = if is_focused {
            palette.title_bar_active
        } else {
            palette.title_bar_inactive
        };
        entry.title_bg.update((bar.size.w, bar.size.h), bar_color);

        // --- close button ----------------------------------------------------
        // The resting state paints no background at all (the title bar shows
        // through); only hover gets a fill. `update` still runs so the size
        // tracks a resized window, and a fully transparent colour costs one
        // element that draws nothing rather than a branch in the element list.
        let close_color = if hovered {
            palette.close_hover_bg
        } else {
            [0.0, 0.0, 0.0, 0.0]
        };
        entry.close_bg.update((close.size.w, close.size.h), close_color);

        if entry.close_key != Some(hovered) {
            let color = if hovered {
                palette.close_hover_glyph
            } else {
                palette.title_text
            };
            entry.close_tex = Some(upload(xmark::rasterize(CLOSE_GLYPH_PX, color)));
            entry.close_key = Some(hovered);
        }

        // --- minimize button (WM-3) ---------------------------------------
        // Same resting-state-draws-nothing rule as the close button; the fill
        // colour is neutral rather than red, because minimizing destroys
        // nothing. The `－` glyph colour never changes for a GIVEN theme, so
        // unlike the ✕ its raster is built exactly once per window per theme
        // (`DuduclawComp::set_theme` drops this whole cache entry on a theme
        // switch, so "once" really does mean "once since the last theme
        // change", not "once ever").
        if let Some(minimize) = minimize {
            entry.minimize_bg.update(
                (minimize.size.w, minimize.size.h),
                if hovered_min {
                    palette.minimize_hover_bg
                } else {
                    [0.0, 0.0, 0.0, 0.0]
                },
            );
        }
        if entry.minimize_key.is_none() {
            entry.minimize_tex = Some(upload(minus::rasterize(
                CLOSE_GLYPH_PX,
                palette.title_text,
            )));
            entry.minimize_key = Some(true);
        }

        // --- title text -------------------------------------------------------
        let text_key = (label.clone(), text_rect.size.w);
        if entry.title_key.as_ref() != Some(&text_key) {
            let raster = fonts.as_ref().and_then(|f| {
                f.rasterize(&label, TITLE_FONT_PX, palette.title_text, text_rect.size.w)
            });
            match raster {
                Some(r) => {
                    entry.title_size = (r.width as i32, r.height as i32);
                    // Fires only when the title string or the available width
                    // actually changed — never per frame. It is the one line
                    // that proves the whole chain (client `set_title` → glyph
                    // raster → cached buffer) in a headless container where no
                    // screenshot is available, which is how every round of this
                    // crate is verified. See BUILD.md.
                    tracing::debug!(
                        surface_id = ?surface_id,
                        title = %label,
                        raster = ?(r.width, r.height),
                        avail_w = text_rect.size.w,
                        "decor: title raster rebuilt"
                    );
                    entry.title_tex = Some(upload(r));
                }
                None => {
                    entry.title_size = (0, 0);
                    entry.title_tex = None;
                }
            }
            entry.title_key = Some(text_key);
        }

        // --- border ------------------------------------------------------------
        let b = BORDER_PX;
        let border_geo: [(Point<i32, Logical>, Size<i32, Logical>); 4] = [
            (frame.loc, Size::from((frame.size.w, b))),
            (
                Point::from((frame.loc.x, frame.loc.y + frame.size.h - b)),
                Size::from((frame.size.w, b)),
            ),
            (
                Point::from((frame.loc.x, frame.loc.y + b)),
                Size::from((b, (frame.size.h - 2 * b).max(0))),
            ),
            (
                Point::from((frame.loc.x + frame.size.w - b, frame.loc.y + b)),
                Size::from((b, (frame.size.h - 2 * b).max(0))),
            ),
        ];
        for (i, (_, size)) in border_geo.iter().enumerate() {
            entry.border[i].update(*size, palette.border);
        }

        // --- shadow -------------------------------------------------------------
        let mut shadow_geo: Vec<(Point<i32, Logical>, Size<i32, Logical>)> =
            Vec::with_capacity(SHADOW_BARS);
        for ring in 0..SHADOW_ALPHAS.len() as i32 {
            let grow = (ring + 1) * SHADOW_RING_PX;
            let ox = frame.loc.x - grow;
            let oy = frame.loc.y - grow;
            let ow = frame.size.w + 2 * grow;
            let oh = frame.size.h + 2 * grow;
            let t = SHADOW_RING_PX;
            shadow_geo.push((Point::from((ox, oy)), Size::from((ow, t))));
            shadow_geo.push((Point::from((ox, oy + oh - t)), Size::from((ow, t))));
            shadow_geo.push((
                Point::from((ox, oy + t)),
                Size::from((t, (oh - 2 * t).max(0))),
            ));
            shadow_geo.push((
                Point::from((ox + ow - t, oy + t)),
                Size::from((t, (oh - 2 * t).max(0))),
            ));
        }
        for (i, (_, size)) in shadow_geo.iter().enumerate() {
            let alpha = SHADOW_ALPHAS[i / 4];
            entry.shadow[i].update(
                *size,
                [
                    palette.shadow_rgb[0],
                    palette.shadow_rgb[1],
                    palette.shadow_rgb[2],
                    alpha,
                ],
            );
        }

        // --- assemble ------------------------------------------------------------
        // Positions are converted into this output's own space here, once.
        let off = |p: Point<i32, Logical>| p - output_loc;

        let mut above: Vec<CodriveElement> = Vec::new();

        // Close ✕, centred in its button box.
        if let Some(tex) = entry.close_tex.as_ref() {
            let g = CLOSE_GLYPH_PX as i32;
            let loc = Point::from((
                close.loc.x + (close.size.w - g) / 2,
                close.loc.y + (close.size.h - g) / 2,
            ));
            if let Some(e) = memory(renderer, tex, off(loc), scale) {
                above.push(e);
            }
        }
        above.push(solid(&entry.close_bg, off(close.loc), scale));

        // WM-3: minimize `－`, centred in its own box by the identical
        // arithmetic (both glyph buffers are square and the boxes are the same
        // size, so the two marks cannot drift apart vertically).
        if let Some(minimize) = minimize {
            if let Some(tex) = entry.minimize_tex.as_ref() {
                let g = CLOSE_GLYPH_PX as i32;
                let loc = Point::from((
                    minimize.loc.x + (minimize.size.w - g) / 2,
                    minimize.loc.y + (minimize.size.h - g) / 2,
                ));
                if let Some(e) = memory(renderer, tex, off(loc), scale) {
                    above.push(e);
                }
            }
            above.push(solid(&entry.minimize_bg, off(minimize.loc), scale));
        }

        // Title text, left-aligned and vertically centred on the bar.
        if let Some(tex) = entry.title_tex.as_ref() {
            let (tw, th) = entry.title_size;
            if tw > 0 && th > 0 {
                let loc = Point::from((text_rect.loc.x, bar.loc.y + (TITLE_BAR_H - th) / 2));
                if let Some(e) = memory(renderer, tex, off(loc), scale) {
                    above.push(e);
                }
            }
        }

        above.push(solid(&entry.title_bg, off(bar.loc), scale));
        for (i, (loc, size)) in border_geo.iter().enumerate() {
            if size.w > 0 && size.h > 0 {
                above.push(solid(&entry.border[i], off(*loc), scale));
            }
        }

        let mut below: Vec<CodriveElement> = Vec::with_capacity(SHADOW_BARS);
        for (i, (loc, size)) in shadow_geo.iter().enumerate() {
            if size.w > 0 && size.h > 0 {
                below.push(solid(&entry.shadow[i], off(*loc), scale));
            }
        }

        self.decor.fonts = fonts;
        (above, below)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_shadow_is_four_rings_of_four_bars() {
        assert_eq!(SHADOW_BARS, 16);
        assert_eq!(SHADOW_ALPHAS.len(), 4);
    }

    #[test]
    fn shadow_alphas_fade_outward_and_stay_subtle() {
        // A shadow that gets DARKER outward reads as a halo, not a shadow;
        // one whose innermost ring is opaque swamps the 1px border.
        let mut prev = 1.0f32;
        for a in SHADOW_ALPHAS {
            assert!(a > 0.0 && a < 0.25, "ring alpha {a} is out of range");
            assert!(a < prev, "alphas must decrease outward, {a} came after {prev}");
            prev = a;
        }
    }

    #[test]
    fn a_fresh_decor_state_has_fonts_and_no_windows() {
        let s = DecorState::new();
        assert!(s.fonts.is_some(), "embedded fonts must load");
        assert!(s.modes.is_empty() && s.buffers.is_empty() && s.frames.is_empty());
        assert!(s.maximized.is_empty());
        assert_eq!(s.cascade_next, 0);
        assert!(s.hovered_close.is_none());
    }

    #[test]
    fn the_close_glyph_fits_inside_the_button_box() {
        assert!(
            (CLOSE_GLYPH_PX as i32) < TITLE_BAR_H,
            "the ✕ must leave padding inside a {TITLE_BAR_H}px bar"
        );
        assert!((CLOSE_GLYPH_PX as i32) < super::super::CLOSE_BTN_W);
        // Centring arithmetic must not produce a negative offset.
        assert!((TITLE_BAR_H - CLOSE_GLYPH_PX as i32) / 2 > 0);
    }
}
