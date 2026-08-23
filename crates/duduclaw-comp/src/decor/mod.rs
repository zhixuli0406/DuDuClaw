//! WM-2 (2026-08-23): **server-side decorations and floating windows**.
//!
//! ## What changed, and why
//!
//! WM-1 was explicitly transitional: an ordinary application window was
//! *filled* into the reserved-band work area (see `crate::window_policy`) and
//! comp answered `zxdg_decoration_manager_v1` with a flat `ClientSide` because
//! it had no decoration renderer at all. That produced a desktop where every
//! window was the same size, in the same place, stacked on top of each other,
//! and where a client that draws no decorations of its own (there are several)
//! had no title, no close button and no way to be moved.
//!
//! The user's decision for this round was **"B：浮動視窗＋完整窗感"**. So:
//!
//! * new non-shell toplevels are **floating** — 80 % of the work area,
//!   centred, cascaded (`placement`);
//! * comp now answers `ServerSide` by default and **draws** the decoration:
//!   a 32 px title bar with the live `xdg_toplevel.title`, a close button, a
//!   1 px border and a soft drop shadow (`paint`, `text`);
//! * the title bar is draggable, and the drag is clamped so a window can never
//!   be thrown somewhere it cannot be grabbed back from.
//!
//! The session shell (`window_policy::SHELL_APP_ID`) is untouched by all of
//! this: it stays full-output, undecorated, and exempt from every rule here.
//!
//! ## The geometry model (one sentence, then the picture)
//!
//! **`Space` still maps the CONTENT rectangle**, exactly as before WM-2; the
//! decoration is drawn *around* that rectangle and exists nowhere in
//! `Space`'s own bookkeeping.
//!
//! ```text
//!  frame.loc ──►┌───────────────────────────────────────┐ ▲ shadow (8px, outside)
//!               │ 1px border                            │ │
//!               │ ┌───────────────────────────────────┐ │ │
//!               │ │ title bar, 32px      [ ✕ ]        │ │ │
//!               │ ├───────────────────────────────────┤ │ │
//!               │ │                                   │ │ │
//!  content.loc ─┼─┼──►  the client's own surface      │ │ │
//!               │ │     (this is what Space maps)     │ │ │
//!               │ └───────────────────────────────────┘ │ │
//!               └───────────────────────────────────────┘ ▼
//! ```
//!
//! That choice is deliberate and it is the reason this work package does not
//! touch `codrive/`, `shell_control/`, `input.rs`'s surface routing, or the
//! move/resize grabs' coordinate math: every existing caller of
//! `Space::element_location` / `element_geometry` / `element_under` /
//! `surface_under` keeps meaning exactly what it meant before. The
//! alternative — wrapping `Window` in a decorated `SpaceElement` so that
//! `Space` maps the *frame* — is the more "idiomatic smithay" shape, and it
//! was rejected for this round precisely because it would have rewritten the
//! coordinate assumptions of every one of those already-live-verified call
//! sites at once.
//!
//! The one thing that model costs us is that `Space::element_under` cannot
//! see the title bar (it is not a surface and not inside any window's bbox).
//! That is handled explicitly and testably by [`hit_frame`] plus
//! `DuduclawComp::frame_hit_at` (`input.rs`), which run *before* the ordinary
//! surface routing — see those two for the ordering rule.
//!
//! ## Palette
//!
//! Calm Glass / brand surfaces from the root `CLAUDE.md` "Aesthetic
//! Direction" table: `stone-*` neutrals, amber only where the brand already
//! uses it.
//!
//! ## D2 (2026-08-2x): theme-aware decoration
//!
//! [`Palette`] is now two swatches, not one: [`Palette::light()`] is the
//! original appearance this crate always drew — unchanged, byte for byte, so
//! nothing already live-verified regresses — and [`Palette::dark()`] is new.
//! Which one is active is [`Theme`], a plain field on `DuduclawComp`
//! (`DuduclawComp::theme`) that `duduclaw-shell` drives entirely through the
//! `shell_control` `set_theme` op — see [`Theme`]'s own doc for why comp has
//! no env var or persisted preference for this (unlike the cursor
//! `source`/`size` settings, which are comp-owned; the appearance theme is
//! shell-owned and comp just follows it live). `DuduclawComp::palette()`
//! (`decor::mod`'s own `impl DuduclawComp` block) is the one place every
//! caller gets the CURRENT palette from — never `Palette::light()`/
//! `Palette::dark()` directly outside this module.

pub mod edges;
pub mod minus;
pub mod mode;
pub mod paint;
pub mod placement;
pub mod text;
pub mod xmark;

use smithay::utils::{Logical, Point, Rectangle, Size};

// `hit_frame_edge` itself is deliberately not re-exported: the only live
// caller must go through the work-area-clipped wrapper, and a convenience
// re-export of the unclipped one is an invitation to skip that clip.
pub use edges::{hit_frame_edge_in_work, FrameEdge};
pub use mode::{negotiated_ssd, DecorMode};
pub use placement::cascade_frame_rect;

/// Height of the server-side title bar, in logical pixels, **excluding** the
/// 1 px border above it. Task brief: "SSD 標題列（32px…）".
pub const TITLE_BAR_H: i32 = 32;

/// Window border thickness, in logical pixels. Task brief: "1px stone-300
/// 邊框".
pub const BORDER_PX: i32 = 1;

/// One shadow ring's thickness, in logical pixels.
pub const SHADOW_RING_PX: i32 = 2;

/// Per-ring alpha of the drop shadow, innermost first. Four 2 px rings = the
/// "8px 漸層" the task brief asked for, approximated with a stepped ramp
/// rather than a real gradient: a gradient would need either a per-window
/// texture upload (allocation proportional to window size, every resize) or a
/// shader this crate does not have. Four solid rings are four
/// `SolidColorRenderElement`s per edge — the same primitive
/// `codrive/highlight.rs` already draws its target box with.
pub const SHADOW_ALPHAS: [f32; 4] = [0.10, 0.06, 0.035, 0.015];

/// Total shadow extent outside the border, in logical pixels.
pub const SHADOW_PX: i32 = SHADOW_RING_PX * SHADOW_ALPHAS.len() as i32;

/// Width of the close button's hit target inside the title bar.
///
/// GNOME's own close button sits in a ~38 px box; 46 px is a little more
/// generous because this compositor is also driven by an agent pointer and by
/// touch-grade absolute positioning in the VM, where a few pixels of slop are
/// the difference between "closes the window" and "starts dragging it".
pub const CLOSE_BTN_W: i32 = 46;

/// Width of the WM-3 minimize button, immediately left of the close button.
/// Same box as the close button so the two read as one control group.
pub const MINIMIZE_BTN_W: i32 = CLOSE_BTN_W;

/// Left padding before the title text starts.
pub const TITLE_PAD_LEFT: i32 = 12;

/// Gap kept between the end of the title text and the close button.
pub const TITLE_GAP_RIGHT: i32 = 8;

/// Title text size in logical pixels.
pub const TITLE_FONT_PX: f32 = 13.0;

/// How much of a window's frame must stay inside the work area when it is
/// dragged. Below this a window is effectively lost: there is no task
/// switcher UI in this compositor (Super+Tab cycles focus but does not move
/// anything), so an off-screen window can only be recovered by closing it.
pub const MIN_ON_SCREEN_PX: i32 = 64;

/// Smallest content size a floating placement will ever configure.
pub const MIN_CONTENT_W: i32 = 240;
/// See [`MIN_CONTENT_W`].
pub const MIN_CONTENT_H: i32 = 160;

/// WM-3: smallest **content** size an interactive edge resize will ever
/// configure — the task brief's "最小尺寸 320×240".
///
/// Deliberately larger than [`MIN_CONTENT_W`]/[`MIN_CONTENT_H`] (which govern
/// automatic placement, where the compositor is choosing on the client's
/// behalf): this floor exists so a human dragging fast cannot collapse a window
/// into a strip that has no visible content left to grab.
///
/// It is a floor on top of the client's own `min_size`, never a replacement:
/// `clamp_resize_size` takes the larger of the two, so a client that declares a
/// bigger minimum still wins.
pub const MIN_RESIZE_W: i32 = 320;
/// See [`MIN_RESIZE_W`].
pub const MIN_RESIZE_H: i32 = 240;

/// WM-3: how close two clicks on the same title bar must be, in milliseconds,
/// to count as a double click. The de-facto desktop default.
pub const DOUBLE_CLICK_MS: u64 = 400;

/// WM-3: and how close together, in logical pixels. Without a distance test a
/// slow drag-click-drag-click across the bar would maximize the window.
pub const DOUBLE_CLICK_SLOP_PX: f64 = 8.0;

/// D2: which appearance the session currently uses.
///
/// Comp does not decide this on its own — `duduclaw-shell` is the single
/// source of truth for the user's theme choice (its own `ThemeChoice`,
/// `duduclaw-shell/src/oobe/selections.rs`) and pushes the live value to comp
/// via the `shell_control` `set_theme` op, both at shell boot and on every
/// user toggle (see `shell_control::protocol`'s own doc on that op for the
/// wire shape). There is deliberately **no** comp-side persistence for this
/// (unlike the cursor `source`/`size` preferences in `crate::cursor::
/// persist`): the shell is authoritative and re-announces its value every
/// time it starts, so comp only ever needs to hold the LIVE value for the
/// rest of this process's lifetime, never a durable one of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Theme {
    /// The appearance this crate always drew, before D2 — kept as the
    /// default so a comp process that starts before the shell's first
    /// `set_theme` call still matches what the shell's own default
    /// (`ThemeChoice::default() == Light`) is about to show, with zero
    /// visible flash-of-wrong-theme.
    #[default]
    Light,
    Dark,
}

impl Theme {
    /// Same trim + case-insensitive leniency as
    /// [`crate::cursor::source::CursorSource::parse_strict`], for the same
    /// reason: this is the CONTROL SOCKET's parser. An unrecognised spelling
    /// is refused, never coerced to a default — see `shell_control::listener`'s
    /// `validate` for where this is called.
    pub fn parse_strict(raw: &str) -> Option<Self> {
        let v = raw.trim();
        if v.eq_ignore_ascii_case("light") {
            Some(Self::Light)
        } else if v.eq_ignore_ascii_case("dark") {
            Some(Self::Dark)
        } else {
            None
        }
    }

    /// Stable short name for tracing/audit fields and the wire.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }
}

/// RGBA colours used by the decoration, as premultiplied-irrelevant opaque
/// `f32` quadruples (`SolidColorBuffer` takes `Color32F`).
///
/// D2: an instance, not a bag of associated consts — the values now depend on
/// the live [`Theme`], so there has to be a real value to hold them. Built
/// exclusively via [`Palette::light`] / [`Palette::dark`] (or
/// [`Palette::for_theme`], which picks between them) — never constructed
/// field-by-field outside this module, so this struct stays the single place
/// a decoration colour is decided.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    /// The focused title bar.
    pub title_bar_active: [f32; 4],
    /// An unfocused window's title bar. Only the background changes with
    /// focus; the text colour stays put so the cached glyph raster (see
    /// [`paint`]) does not have to be re-rasterised every time focus moves.
    pub title_bar_inactive: [f32; 4],
    /// Title text and the resting close glyph.
    pub title_text: [u8; 3],
    /// The 1 px border.
    pub border: [f32; 4],
    /// Close button hover fill (the one place this palette leaves the warm
    /// neutrals in either theme, because "this destroys work" is the one
    /// affordance that must not read as neutral).
    pub close_hover_bg: [f32; 4],
    /// The close glyph while hovered.
    pub close_hover_glyph: [u8; 3],
    /// Shadow colour (alpha comes from [`SHADOW_ALPHAS`]).
    pub shadow_rgb: [f32; 3],
    /// WM-3 — the minimize button's hover fill. Neutral on purpose: unlike
    /// close, minimizing destroys nothing, so it must not borrow the "this is
    /// dangerous" red.
    pub minimize_hover_bg: [f32; 4],
    /// WM-3 — the Alt-Tab panel's background. Opaque: the panel is read at a
    /// glance while a key is held, and a translucent list over an arbitrary
    /// desktop is exactly the thing that stops being readable at the moment
    /// it matters.
    pub switcher_bg: [f32; 4],
    /// WM-3 — the switcher panel's 1 px border.
    pub switcher_border: [f32; 4],
    /// WM-3 — the selected row, the brand's amber primary. Kept identical
    /// across both themes — a brand accent, not a surface colour.
    pub switcher_row_selected: [f32; 4],
    /// WM-3 — an unselected row draws no fill at all (the panel shows
    /// through); the buffer still exists so its size tracks a resize. See
    /// `decor::paint`'s "why every buffer is cached" note.
    pub switcher_row_idle: [f32; 4],
    /// WM-3 — switcher label text. Deliberately the same colour on the
    /// selected row in both themes: the brand amber clears WCAG AA against
    /// both `title_text` values at this size, and keeping one colour means
    /// the glyph raster survives a selection change instead of being rebuilt
    /// on every keypress.
    pub switcher_text: [u8; 3],
}

impl Palette {
    /// The original appearance this crate shipped with, unchanged. Calm
    /// Glass / brand light surfaces from the root `CLAUDE.md` "Aesthetic
    /// Direction" table: `stone-100` title bar, `stone-900` title text,
    /// `stone-300` border, amber only where the brand already uses it.
    pub fn light() -> Self {
        Self {
            title_bar_active: [0.961, 0.961, 0.957, 1.0], // stone-100 #f5f5f4
            title_bar_inactive: [0.906, 0.898, 0.894, 1.0], // stone-200 #e7e5e4
            title_text: [0x1c, 0x19, 0x17],               // stone-900 #1c1917
            border: [0.839, 0.827, 0.820, 1.0],           // stone-300 #d6d3d1
            close_hover_bg: [0.882, 0.114, 0.282, 1.0],   // rose-600 #e11d48
            close_hover_glyph: [0xff, 0xff, 0xff],
            shadow_rgb: [0.0, 0.0, 0.0],
            minimize_hover_bg: [0.906, 0.898, 0.894, 1.0], // stone-200, == title_bar_inactive
            switcher_bg: [0.980, 0.980, 0.976, 1.0],       // stone-50 #fafaf9
            switcher_border: [0.839, 0.827, 0.820, 1.0],   // stone-300, == border
            switcher_row_selected: [0.961, 0.620, 0.043, 1.0], // amber-500 #f59e0b
            switcher_row_idle: [0.0, 0.0, 0.0, 0.0],
            switcher_text: [0x1c, 0x19, 0x17], // stone-900, == title_text
        }
    }

    /// D2: the dark surface — root `CLAUDE.md`'s "Surface dark: deep stone
    /// (`stone-900` / `#1c1917`) — warm dark, not cold blue-black" applied to
    /// every surface a window's own decoration draws, plus a matching
    /// `stone-100` light-on-dark text colour. The brand accents (close-hover
    /// rose, switcher-selected amber) and the shadow colour are deliberately
    /// unchanged from [`Self::light`] — they are semantic/overlay colours,
    /// not surface neutrals, so they read correctly against either theme's
    /// wallpaper without needing a second swatch.
    pub fn dark() -> Self {
        Self {
            title_bar_active: [0.161, 0.145, 0.141, 1.0], // stone-800 #292524 — focused reads lighter, same rule as light()
            title_bar_inactive: [0.110, 0.098, 0.090, 1.0], // stone-900 #1c1917
            title_text: [0xf5, 0xf5, 0xf4],               // stone-100 #f5f5f4 — light text on dark
            border: [0.267, 0.251, 0.235, 1.0],           // stone-700 #44403c
            close_hover_bg: [0.882, 0.114, 0.282, 1.0],   // rose-600, unchanged — see doc above
            close_hover_glyph: [0xff, 0xff, 0xff],
            shadow_rgb: [0.0, 0.0, 0.0], // unchanged — the shadow reads against the wallpaper, not the title bar
            minimize_hover_bg: [0.267, 0.251, 0.235, 1.0], // stone-700, == border
            switcher_bg: [0.110, 0.098, 0.090, 1.0],       // stone-900 #1c1917
            switcher_border: [0.267, 0.251, 0.235, 1.0],   // stone-700, == border
            switcher_row_selected: [0.961, 0.620, 0.043, 1.0], // amber-500, unchanged — see doc above
            switcher_row_idle: [0.0, 0.0, 0.0, 0.0],
            switcher_text: [0xf5, 0xf5, 0xf4], // stone-100, == title_text
        }
    }

    /// The palette for a given [`Theme`] — the one entry point every caller
    /// outside this module should use (via `DuduclawComp::palette()`, not
    /// this directly, so the theme lookup stays in one place).
    pub fn for_theme(theme: Theme) -> Self {
        match theme {
            Theme::Light => Self::light(),
            Theme::Dark => Self::dark(),
        }
    }
}

impl crate::state::DuduclawComp {
    /// D2: which [`Palette`] the decoration renderer (and the switcher panel)
    /// should draw with right now. Freshly computed from `self.theme` every
    /// call — [`Palette`] is `Copy` and holds no heap data, so there is
    /// nothing to cache and nothing that could drift out of sync with the
    /// live theme.
    pub(crate) fn palette(&self) -> Palette {
        Palette::for_theme(self.theme)
    }

    /// D2: switch the session's appearance **live** — see [`Theme`]'s own doc
    /// for why this is driven entirely by `shell_control`'s `set_theme` op
    /// rather than an env var or a comp-side preference file.
    ///
    /// Every SOLID-colour decoration buffer (title bar fill, border, shadow,
    /// close/minimize hover fills) is re-`update`d unconditionally on every
    /// frame already (`decor::paint::build_frame_elements`), so those pick up
    /// the new palette on the very next composite for free. The three
    /// RASTERISED glyph buffers (title text, close ✕, minimize `－`) and the
    /// switcher panel's row labels are cached and keyed on things that do
    /// NOT include the theme (text/hover/selection — see
    /// `decor::paint::WindowDecorBuffers` and `switcher::SwitcherState`'s own
    /// `key` field), so without an explicit invalidation here they would keep
    /// showing the OLD theme's text colour until their key next happened to
    /// change for an unrelated reason. `DecorState::invalidate_theme_cache`
    /// and clearing the switcher's cache key force a one-off full rebuild —
    /// simpler and less error-prone than threading `Theme` into every one of
    /// those cache keys, and cheap: a theme switch is a rare, deliberate user
    /// action, not a per-frame event, so the extra rasterisation work costs
    /// nothing that matters.
    ///
    /// Same `queue_redraw` requirement as `cursor::mod::set_cursor_source`: on
    /// the udev backend nothing else would schedule a frame until some
    /// unrelated damage happened, so a switch made while nothing else changes
    /// would appear to do nothing until then.
    ///
    /// Returns `true` when the live theme actually changed.
    pub(crate) fn set_theme(&mut self, theme: Theme) -> bool {
        if self.theme == theme {
            return false;
        }
        self.theme = theme;
        self.decor.invalidate_theme_cache();
        // Reuses the switcher's own "session ended" cache-drop — the effect
        // (drop the stale rasters, key included) is exactly what a theme
        // change also needs, even though the switcher is not closing.
        self.switcher.invalidate();
        self.queue_redraw();
        true
    }
}

/// How much bigger the frame is than the content, on each side.
///
/// [`DecorInsets::NONE`] is a client-side-decorated (or shell, or shadow
/// workspace) window: frame == content, nothing is drawn, and every geometry
/// function in this module degrades to the identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecorInsets {
    pub top: i32,
    pub left: i32,
    pub right: i32,
    pub bottom: i32,
}

impl DecorInsets {
    /// No decoration at all.
    pub const NONE: Self = Self {
        top: 0,
        left: 0,
        right: 0,
        bottom: 0,
    };

    /// The server-side decoration: a border all round, and the title bar
    /// stacked above the content inside the top border.
    pub const SSD: Self = Self {
        top: TITLE_BAR_H + BORDER_PX,
        left: BORDER_PX,
        right: BORDER_PX,
        bottom: BORDER_PX,
    };

    #[inline]
    pub fn for_ssd(ssd: bool) -> Self {
        if ssd {
            Self::SSD
        } else {
            Self::NONE
        }
    }

    /// True when this window is actually decorated by the compositor.
    #[inline]
    pub fn is_decorated(&self) -> bool {
        self.top > 0
    }

    #[inline]
    pub fn horizontal(&self) -> i32 {
        self.left + self.right
    }

    #[inline]
    pub fn vertical(&self) -> i32 {
        self.top + self.bottom
    }
}

/// The frame rectangle for a window whose content occupies `content`.
pub fn frame_rect(content: Rectangle<i32, Logical>, insets: DecorInsets) -> Rectangle<i32, Logical> {
    Rectangle::new(
        Point::from((content.loc.x - insets.left, content.loc.y - insets.top)),
        Size::from((
            content.size.w + insets.horizontal(),
            content.size.h + insets.vertical(),
        )),
    )
}

/// The content rectangle inside a given frame — the inverse of
/// [`frame_rect`].
///
/// The size is floored at 1×1: a frame too small to contain its own
/// decoration would otherwise produce a zero or negative `xdg_toplevel`
/// configure, which is a protocol error waiting to happen rather than a small
/// window.
pub fn content_rect(frame: Rectangle<i32, Logical>, insets: DecorInsets) -> Rectangle<i32, Logical> {
    Rectangle::new(
        Point::from((frame.loc.x + insets.left, frame.loc.y + insets.top)),
        Size::from((
            (frame.size.w - insets.horizontal()).max(1),
            (frame.size.h - insets.vertical()).max(1),
        )),
    )
}

/// The frame plus its drop shadow — what actually has to intersect an output
/// for this window to be worth rendering there.
pub fn shadow_bounds(frame: Rectangle<i32, Logical>, insets: DecorInsets) -> Rectangle<i32, Logical> {
    if !insets.is_decorated() {
        return frame;
    }
    Rectangle::new(
        Point::from((frame.loc.x - SHADOW_PX, frame.loc.y - SHADOW_PX)),
        Size::from((
            frame.size.w + 2 * SHADOW_PX,
            frame.size.h + 2 * SHADOW_PX,
        )),
    )
}

/// The title bar rectangle, in the same coordinate space as `frame`.
///
/// `None` for an undecorated window. The bar sits *inside* the border, which
/// is why it is inset by [`BORDER_PX`] on three sides — the border is drawn as
/// a ring around the whole frame, so a title bar drawn at the frame's own
/// edges would paint over it.
pub fn title_bar_rect(
    frame: Rectangle<i32, Logical>,
    insets: DecorInsets,
) -> Option<Rectangle<i32, Logical>> {
    if !insets.is_decorated() {
        return None;
    }
    let w = frame.size.w - insets.horizontal();
    if w <= 0 {
        return None;
    }
    Some(Rectangle::new(
        Point::from((frame.loc.x + insets.left, frame.loc.y + BORDER_PX)),
        Size::from((w, TITLE_BAR_H)),
    ))
}

/// The close button's rectangle inside a title bar.
///
/// Right-aligned and full-height. On a title bar narrower than the button the
/// button takes the whole bar rather than overflowing — a 60 px-wide window is
/// pathological, but "the close button hangs outside the frame" would be a
/// visible bug and "there is no close button at all" would be a trap.
pub fn close_button_rect(title_bar: Rectangle<i32, Logical>) -> Rectangle<i32, Logical> {
    let w = CLOSE_BTN_W.min(title_bar.size.w).max(1);
    Rectangle::new(
        Point::from((title_bar.loc.x + title_bar.size.w - w, title_bar.loc.y)),
        Size::from((w, title_bar.size.h)),
    )
}

/// WM-3: the minimize button's rectangle, immediately left of the close
/// button.
///
/// `None` when the title bar is too narrow to hold both buttons — a
/// pathological 60 px window keeps its close button (the affordance that must
/// never disappear, or the window becomes a trap) and simply has no minimize
/// button. Deliberately not "shrink both": two 20 px buttons side by side are
/// two mis-clicks waiting to happen, and one of them destroys work.
pub fn minimize_button_rect(title_bar: Rectangle<i32, Logical>) -> Option<Rectangle<i32, Logical>> {
    let close = close_button_rect(title_bar);
    let x = close.loc.x - MINIMIZE_BTN_W;
    if x < title_bar.loc.x {
        return None;
    }
    Some(Rectangle::new(
        Point::from((x, title_bar.loc.y)),
        Size::from((MINIMIZE_BTN_W, title_bar.size.h)),
    ))
}

/// The rectangle the title text may occupy: from the left padding up to the
/// left-most button, minus a gap. Width can legitimately come back `0` on a
/// very narrow window, in which case no text is rasterised at all.
pub fn title_text_rect(title_bar: Rectangle<i32, Logical>) -> Rectangle<i32, Logical> {
    // WM-3: the minimize button, when there is room for one, is what the text
    // now stops before — not the close button.
    let buttons_left = minimize_button_rect(title_bar)
        .map(|r| r.loc.x)
        .unwrap_or_else(|| close_button_rect(title_bar).loc.x);
    let x = title_bar.loc.x + TITLE_PAD_LEFT;
    let right = buttons_left - TITLE_GAP_RIGHT;
    Rectangle::new(
        Point::from((x, title_bar.loc.y)),
        Size::from(((right - x).max(0), title_bar.size.h)),
    )
}

/// What a pointer press on a window's frame means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameHit {
    /// The close button — send `xdg_toplevel.close`.
    Close,
    /// WM-3: the minimize button — unmap the window, keep it switchable.
    Minimize,
    /// Anywhere else on the title bar — start a move grab (or, on a double
    /// click, toggle maximize).
    TitleBar,
    /// WM-3: the resize ring outside the frame. See [`edges`].
    Edge(FrameEdge),
}

/// Classifies a pointer position against one window's frame.
///
/// Returns `None` for a position that is not on the compositor's own
/// decoration — including a position inside the *content* area, which must
/// fall through to the ordinary surface routing untouched.
pub fn hit_frame(
    frame: Rectangle<i32, Logical>,
    insets: DecorInsets,
    pos: Point<f64, Logical>,
) -> Option<FrameHit> {
    let bar = title_bar_rect(frame, insets)?;
    if !bar.to_f64().contains(pos) {
        return None;
    }
    if close_button_rect(bar).to_f64().contains(pos) {
        return Some(FrameHit::Close);
    }
    if let Some(minimize) = minimize_button_rect(bar) {
        if minimize.to_f64().contains(pos) {
            return Some(FrameHit::Minimize);
        }
    }
    Some(FrameHit::TitleBar)
}

/// WM-3: whether two title-bar presses count as one double click.
///
/// Pure so the rule is testable without a clock or a seat. `previous` is the
/// last press this compositor saw on **the same window's** title bar (the
/// caller owns that comparison — an id match is not a geometry question).
pub fn is_double_click(
    previous: Option<(std::time::Duration, Point<f64, Logical>)>,
    now: std::time::Duration,
    pos: Point<f64, Logical>,
) -> bool {
    let Some((then, where_)) = previous else {
        return false;
    };
    // Saturating: a monotonic clock cannot go backwards, but a caller feeding
    // two independently-sourced timestamps could, and an underflow panic must
    // not be one mis-ordered event away.
    let elapsed = now.saturating_sub(then);
    if elapsed > std::time::Duration::from_millis(DOUBLE_CLICK_MS) {
        return false;
    }
    let dx = pos.x - where_.x;
    let dy = pos.y - where_.y;
    (dx * dx + dy * dy).sqrt() <= DOUBLE_CLICK_SLOP_PX
}

/// Clamps a proposed frame origin so the window stays recoverable.
///
/// Three rules, all of them about "can the human get this window back":
///
/// 1. The frame's top edge never rises above the work area's top edge, so the
///    title bar is never pushed under the shell's menu bar.
/// 2. The frame's top edge never sinks past `work.bottom - grab_height`, so at
///    least the full title bar is always on screen.
/// 3. Horizontally, at least [`MIN_ON_SCREEN_PX`] of the frame stays inside
///    the work area on whichever side it is being dragged towards.
///
/// For an undecorated window (`insets == NONE`) rule 2 uses
/// [`MIN_ON_SCREEN_PX`] as the notional grab height — there is no title bar to
/// keep visible, but a client-decorated window still has its own one at the
/// top of its surface.
pub fn clamp_frame_loc(
    frame_size: Size<i32, Logical>,
    work: Rectangle<i32, Logical>,
    desired: Point<i32, Logical>,
    insets: DecorInsets,
) -> Point<i32, Logical> {
    let grab_h = if insets.is_decorated() {
        insets.top
    } else {
        MIN_ON_SCREEN_PX
    };

    let y_min = work.loc.y;
    let y_max = (work.loc.y + work.size.h - grab_h).max(y_min);

    let x_min = work.loc.x - frame_size.w + MIN_ON_SCREEN_PX;
    let x_max = work.loc.x + work.size.w - MIN_ON_SCREEN_PX;
    // A frame wider than the work area plus both margins can invert the range.
    let x_max = x_max.max(x_min);

    Point::from((desired.x.clamp(x_min, x_max), desired.y.clamp(y_min, y_max)))
}

/// Shrinks a frame so it fits inside `work`, keeping its origin where the
/// clamp allows. Used when the output's mode changes under already-placed
/// windows (`window_policy::reapply_window_policy_all`).
pub fn refit_frame(
    frame: Rectangle<i32, Logical>,
    work: Rectangle<i32, Logical>,
    insets: DecorInsets,
) -> Rectangle<i32, Logical> {
    let size = Size::from((
        frame.size.w.min(work.size.w).max(insets.horizontal() + 1),
        frame.size.h.min(work.size.h).max(insets.vertical() + 1),
    ));
    Rectangle::new(clamp_frame_loc(size, work, frame.loc, insets), size)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Logical> {
        Rectangle::new(Point::from((x, y)), Size::from((w, h)))
    }

    // ── D2 theme ─────────────────────────────────────────────────────────

    #[test]
    fn theme_default_is_light() {
        // Must match `duduclaw-shell`'s own `ThemeChoice::default()` — a comp
        // process that boots before the shell's first `set_theme` call has to
        // already look right.
        assert_eq!(Theme::default(), Theme::Light);
    }

    #[test]
    fn theme_parse_strict_accepts_the_two_documented_spellings() {
        for raw in ["light", "LIGHT", "  Light  "] {
            assert_eq!(Theme::parse_strict(raw), Some(Theme::Light), "{raw:?}");
        }
        for raw in ["dark", "DARK", " Dark "] {
            assert_eq!(Theme::parse_strict(raw), Some(Theme::Dark), "{raw:?}");
        }
    }

    #[test]
    fn theme_parse_strict_rejects_anything_else_instead_of_defaulting() {
        for raw in ["", "   ", "blue", "1", "yes", "auto", "system", "🐾", "lightdark"] {
            assert_eq!(Theme::parse_strict(raw), None, "{raw:?} must be refused");
        }
    }

    #[test]
    fn theme_as_str_round_trips_through_parse_strict() {
        for theme in [Theme::Light, Theme::Dark] {
            assert_eq!(Theme::parse_strict(theme.as_str()), Some(theme));
        }
    }

    #[test]
    fn palette_for_theme_dispatches_to_the_matching_constructor() {
        assert_eq!(Palette::for_theme(Theme::Light), Palette::light());
        assert_eq!(Palette::for_theme(Theme::Dark), Palette::dark());
    }

    #[test]
    fn light_is_the_original_appearance_this_crate_shipped_with() {
        // Byte-for-byte the values `Palette` used to hold as associated
        // consts, before D2 — the exact "no regression" guarantee the task
        // brief asked for.
        let p = Palette::light();
        assert_eq!(p.title_bar_active, [0.961, 0.961, 0.957, 1.0]);
        assert_eq!(p.title_bar_inactive, [0.906, 0.898, 0.894, 1.0]);
        assert_eq!(p.title_text, [0x1c, 0x19, 0x17]);
        assert_eq!(p.border, [0.839, 0.827, 0.820, 1.0]);
        assert_eq!(p.close_hover_bg, [0.882, 0.114, 0.282, 1.0]);
        assert_eq!(p.close_hover_glyph, [0xff, 0xff, 0xff]);
        assert_eq!(p.shadow_rgb, [0.0, 0.0, 0.0]);
        assert_eq!(p.minimize_hover_bg, [0.906, 0.898, 0.894, 1.0]);
        assert_eq!(p.switcher_bg, [0.980, 0.980, 0.976, 1.0]);
        assert_eq!(p.switcher_border, [0.839, 0.827, 0.820, 1.0]);
        assert_eq!(p.switcher_row_selected, [0.961, 0.620, 0.043, 1.0]);
        assert_eq!(p.switcher_row_idle, [0.0, 0.0, 0.0, 0.0]);
        assert_eq!(p.switcher_text, [0x1c, 0x19, 0x17]);
    }

    #[test]
    fn dark_actually_differs_from_light_on_every_surface_neutral() {
        // The whole point of D2: a caller reading `title_bar_active`/
        // `title_text`/`border`/`switcher_bg`/`switcher_border` after a
        // `set_theme("dark")` must see DIFFERENT bytes, not the light theme
        // silently reused.
        let l = Palette::light();
        let d = Palette::dark();
        assert_ne!(l.title_bar_active, d.title_bar_active);
        assert_ne!(l.title_bar_inactive, d.title_bar_inactive);
        assert_ne!(l.title_text, d.title_text);
        assert_ne!(l.border, d.border);
        assert_ne!(l.minimize_hover_bg, d.minimize_hover_bg);
        assert_ne!(l.switcher_bg, d.switcher_bg);
        assert_ne!(l.switcher_border, d.switcher_border);
        assert_ne!(l.switcher_text, d.switcher_text);
    }

    #[test]
    fn dark_keeps_the_semantic_brand_colours_unchanged() {
        // Danger red and the brand amber are overlay/semantic colours, not
        // surface neutrals — they must read correctly against either theme's
        // wallpaper, so they are deliberately identical in both.
        let l = Palette::light();
        let d = Palette::dark();
        assert_eq!(l.close_hover_bg, d.close_hover_bg);
        assert_eq!(l.close_hover_glyph, d.close_hover_glyph);
        assert_eq!(l.switcher_row_selected, d.switcher_row_selected);
        assert_eq!(l.shadow_rgb, d.shadow_rgb);
    }

    #[test]
    fn dark_titles_read_as_light_text_on_a_dark_surface() {
        // A sanity check on the actual swatch choice: dark title text on a
        // dark bar would be unreadable, which a byte inequality alone (the
        // test above) cannot catch.
        let d = Palette::dark();
        let text_luma: f32 = d.title_text.iter().map(|&b| b as f32).sum();
        let bar_luma: f32 = d.title_bar_active[..3].iter().sum::<f32>() * 255.0;
        assert!(text_luma > bar_luma * 2.0, "text {text_luma} vs bar {bar_luma}");
    }

    fn p(x: f64, y: f64) -> Point<f64, Logical> {
        Point::from((x, y))
    }

    #[test]
    fn ssd_insets_are_the_title_bar_plus_the_border() {
        // If either constant moves, `paint.rs`'s bar/border placement moves
        // with it — asserting the arithmetic here is what keeps the two in
        // step.
        assert_eq!(DecorInsets::SSD.top, 33);
        assert_eq!(DecorInsets::SSD.left, 1);
        assert_eq!(DecorInsets::SSD.right, 1);
        assert_eq!(DecorInsets::SSD.bottom, 1);
        assert_eq!(DecorInsets::SSD.horizontal(), 2);
        assert_eq!(DecorInsets::SSD.vertical(), 34);
    }

    #[test]
    fn frame_and_content_are_exact_inverses() {
        let content = rect(100, 200, 800, 600);
        for insets in [DecorInsets::SSD, DecorInsets::NONE] {
            let frame = frame_rect(content, insets);
            assert_eq!(content_rect(frame, insets), content, "insets {insets:?}");
        }
    }

    #[test]
    fn an_undecorated_window_has_frame_equal_to_content() {
        let content = rect(10, 20, 300, 400);
        assert_eq!(frame_rect(content, DecorInsets::NONE), content);
        assert_eq!(shadow_bounds(content, DecorInsets::NONE), content);
        assert_eq!(title_bar_rect(content, DecorInsets::NONE), None);
        assert_eq!(hit_frame(content, DecorInsets::NONE, p(15.0, 25.0)), None);
    }

    #[test]
    fn the_border_bottom_edge_sits_exactly_one_pixel_below_the_content() {
        // The whole frame model rests on this: content.bottom + 1 == the last
        // row of the frame, which is where `paint.rs` draws the bottom border.
        let content = rect(0, 0, 640, 480);
        let frame = frame_rect(content, DecorInsets::SSD);
        assert_eq!(frame.loc.y + frame.size.h - 1, content.loc.y + content.size.h);
        assert_eq!(frame.loc.x + frame.size.w - 1, content.loc.x + content.size.w);
    }

    #[test]
    fn the_title_bar_sits_inside_the_border_and_directly_above_the_content() {
        let content = rect(50, 100, 800, 600);
        let frame = frame_rect(content, DecorInsets::SSD);
        let bar = title_bar_rect(frame, DecorInsets::SSD).expect("decorated");
        assert_eq!(bar.loc.x, frame.loc.x + BORDER_PX);
        assert_eq!(bar.loc.y, frame.loc.y + BORDER_PX);
        assert_eq!(bar.size.w, frame.size.w - 2 * BORDER_PX);
        assert_eq!(bar.size.h, TITLE_BAR_H);
        // No gap and no overlap between the bar and the client's first row.
        assert_eq!(bar.loc.y + bar.size.h, content.loc.y);
    }

    #[test]
    fn the_close_button_is_right_aligned_inside_the_title_bar() {
        let bar = rect(0, 0, 800, TITLE_BAR_H);
        let close = close_button_rect(bar);
        assert_eq!(close.size.w, CLOSE_BTN_W);
        assert_eq!(close.loc.x + close.size.w, bar.loc.x + bar.size.w);
        assert_eq!(close.size.h, TITLE_BAR_H);
    }

    #[test]
    fn a_title_bar_narrower_than_the_button_still_has_a_close_button() {
        let bar = rect(0, 0, 20, TITLE_BAR_H);
        let close = close_button_rect(bar);
        assert_eq!(close.size.w, 20, "the button shrinks rather than overflowing");
        assert_eq!(close.loc.x, 0);
        // And the text area collapses instead of going negative.
        assert_eq!(title_text_rect(bar).size.w, 0);
    }

    #[test]
    fn hit_test_distinguishes_close_from_drag_from_content() {
        let content = rect(100, 100, 800, 600);
        let frame = frame_rect(content, DecorInsets::SSD);
        let bar = title_bar_rect(frame, DecorInsets::SSD).unwrap();

        // Left end of the bar: drag.
        assert_eq!(
            hit_frame(frame, DecorInsets::SSD, p(bar.loc.x as f64 + 5.0, bar.loc.y as f64 + 5.0)),
            Some(FrameHit::TitleBar)
        );
        // Right end of the bar: close.
        let close = close_button_rect(bar);
        assert_eq!(
            hit_frame(
                frame,
                DecorInsets::SSD,
                p(close.loc.x as f64 + 5.0, close.loc.y as f64 + 5.0)
            ),
            Some(FrameHit::Close)
        );
        // Inside the client area: not ours.
        assert_eq!(
            hit_frame(frame, DecorInsets::SSD, p(400.0, 400.0)),
            None,
            "a click in the content area must fall through to the surface"
        );
        // Outside the frame entirely.
        assert_eq!(hit_frame(frame, DecorInsets::SSD, p(0.0, 0.0)), None);
    }

    #[test]
    fn the_close_button_boundary_is_exactly_one_pixel_wide_in_its_decision() {
        let content = rect(0, 100, 800, 600);
        let frame = frame_rect(content, DecorInsets::SSD);
        let bar = title_bar_rect(frame, DecorInsets::SSD).unwrap();
        let close = close_button_rect(bar);
        let y = bar.loc.y as f64 + 1.0;
        // WM-3: the minimize button now abuts the close button, so the pixel
        // immediately left of ✕ is －, not drag area. That the boundary is
        // exact — no dead pixel, no overlap — is what this asserts.
        assert_eq!(
            hit_frame(frame, DecorInsets::SSD, p(close.loc.x as f64 - 0.5, y)),
            Some(FrameHit::Minimize)
        );
        assert_eq!(
            hit_frame(frame, DecorInsets::SSD, p(close.loc.x as f64, y)),
            Some(FrameHit::Close)
        );
    }

    #[test]
    fn the_minimize_button_sits_immediately_left_of_the_close_button() {
        let bar = rect(0, 0, 800, TITLE_BAR_H);
        let close = close_button_rect(bar);
        let minimize = minimize_button_rect(bar).expect("a full-width bar has room for both");
        assert_eq!(minimize.loc.x + minimize.size.w, close.loc.x, "no gap, no overlap");
        assert_eq!(minimize.size.w, MINIMIZE_BTN_W);
        assert_eq!(minimize.size.h, TITLE_BAR_H);
        assert_eq!(minimize.loc.y, bar.loc.y);
    }

    #[test]
    fn a_bar_too_narrow_for_both_buttons_keeps_the_close_button_and_drops_minimize() {
        // Losing ✕ would make the window a trap; losing － loses nothing.
        let bar = rect(0, 0, 60, TITLE_BAR_H);
        assert_eq!(minimize_button_rect(bar), None);
        assert_eq!(close_button_rect(bar).size.w, CLOSE_BTN_W);
        // A 62px-wide frame gives a 60px bar: ✕ takes x 14..60, so 0..14 is
        // all the drag area there is — and it must still be drag area, not a
        // second button.
        let frame = rect(-1, -1, 62, 200);
        assert_eq!(
            hit_frame(frame, DecorInsets::SSD, p(5.0, 5.0)),
            Some(FrameHit::TitleBar),
            "with no minimize button the left of the bar is still draggable"
        );
        assert_eq!(hit_frame(frame, DecorInsets::SSD, p(30.0, 5.0)), Some(FrameHit::Close));
    }

    #[test]
    fn the_title_text_stops_before_the_left_most_button() {
        let bar = rect(0, 0, 800, TITLE_BAR_H);
        let text = title_text_rect(bar);
        let minimize = minimize_button_rect(bar).unwrap();
        assert_eq!(text.loc.x, TITLE_PAD_LEFT);
        assert_eq!(text.loc.x + text.size.w, minimize.loc.x - TITLE_GAP_RIGHT);
        // Two 46px buttons + 12 left pad + 8 gap = 692 of an 800px bar.
        assert_eq!(text.size.w, 800 - CLOSE_BTN_W - MINIMIZE_BTN_W - TITLE_PAD_LEFT - TITLE_GAP_RIGHT);
    }

    #[test]
    fn a_second_click_soon_and_near_is_a_double_click() {
        let then = std::time::Duration::from_millis(1_000);
        let prev = Some((then, p(100.0, 50.0)));
        assert!(is_double_click(prev, std::time::Duration::from_millis(1_200), p(102.0, 51.0)));
    }

    #[test]
    fn a_slow_or_distant_second_click_is_not_a_double_click() {
        let then = std::time::Duration::from_millis(1_000);
        let prev = Some((then, p(100.0, 50.0)));
        // Too slow.
        assert!(!is_double_click(prev, std::time::Duration::from_millis(1_600), p(100.0, 50.0)));
        // Far enough that it is a drag, not a double click.
        assert!(!is_double_click(prev, std::time::Duration::from_millis(1_100), p(140.0, 50.0)));
        // Exactly on the slop boundary counts; one pixel past does not.
        assert!(is_double_click(prev, std::time::Duration::from_millis(1_100), p(108.0, 50.0)));
        assert!(!is_double_click(prev, std::time::Duration::from_millis(1_100), p(109.0, 50.0)));
    }

    #[test]
    fn the_first_ever_click_is_never_a_double_click() {
        assert!(!is_double_click(None, std::time::Duration::from_millis(5), p(0.0, 0.0)));
    }

    #[test]
    fn a_backwards_timestamp_does_not_panic() {
        let prev = Some((std::time::Duration::from_millis(9_000), p(0.0, 0.0)));
        assert!(is_double_click(prev, std::time::Duration::from_millis(10), p(0.0, 0.0)));
    }

    #[test]
    fn a_drag_can_never_push_the_title_bar_above_the_work_area() {
        let work = rect(0, 30, 1280, 680);
        let size = Size::from((800, 634));
        let out = clamp_frame_loc(size, work, Point::from((100, -500)), DecorInsets::SSD);
        assert_eq!(out.y, 30, "the frame top is pinned to the work area top");
    }

    #[test]
    fn a_drag_always_leaves_the_whole_title_bar_on_screen_at_the_bottom() {
        let work = rect(0, 30, 1280, 680);
        let size = Size::from((800, 634));
        let out = clamp_frame_loc(size, work, Point::from((100, 99_999)), DecorInsets::SSD);
        // work bottom (710) minus the full title-bar-plus-border height (33).
        assert_eq!(out.y, 710 - DecorInsets::SSD.top);
    }

    #[test]
    fn a_drag_keeps_a_grabbable_sliver_on_both_horizontal_edges() {
        let work = rect(0, 30, 1280, 680);
        let size = Size::from((800, 634));
        let left = clamp_frame_loc(size, work, Point::from((-99_999, 100)), DecorInsets::SSD);
        assert_eq!(left.x, -800 + MIN_ON_SCREEN_PX);
        let right = clamp_frame_loc(size, work, Point::from((99_999, 100)), DecorInsets::SSD);
        assert_eq!(right.x, 1280 - MIN_ON_SCREEN_PX);
    }

    #[test]
    fn a_position_already_inside_the_work_area_is_left_alone() {
        let work = rect(0, 30, 1280, 680);
        let size = Size::from((800, 634));
        let want = Point::from((200, 60));
        assert_eq!(clamp_frame_loc(size, work, want, DecorInsets::SSD), want);
    }

    #[test]
    fn clamping_never_inverts_on_a_frame_larger_than_the_work_area() {
        // A 4000px-wide frame on a 640px work area: `x_min` would exceed
        // `x_max` without the guard, and `i32::clamp` panics on an inverted
        // range. This is reachable for real — an output whose mode shrinks
        // under an already-placed window.
        let work = rect(0, 0, 640, 200);
        let size = Size::from((4000, 3000));
        let out = clamp_frame_loc(size, work, Point::from((0, 0)), DecorInsets::SSD);
        assert!(out.x <= 0 && out.y >= 0, "clamped to {out:?} without panicking");
    }

    #[test]
    fn an_undecorated_window_is_still_clamped_by_a_notional_grab_height() {
        let work = rect(0, 0, 1280, 800);
        let size = Size::from((400, 300));
        let out = clamp_frame_loc(size, work, Point::from((0, 99_999)), DecorInsets::NONE);
        assert_eq!(out.y, 800 - MIN_ON_SCREEN_PX);
    }

    #[test]
    fn refit_shrinks_an_oversized_frame_into_the_work_area() {
        let work = rect(0, 30, 1280, 680);
        let big = rect(-200, -200, 4000, 4000);
        let out = refit_frame(big, work, DecorInsets::SSD);
        assert_eq!((out.size.w, out.size.h), (1280, 680));
        assert!(out.loc.y >= work.loc.y);
    }

    #[test]
    fn refit_leaves_a_frame_that_already_fits_untouched() {
        let work = rect(0, 30, 1280, 680);
        let ok = rect(100, 60, 800, 600);
        assert_eq!(refit_frame(ok, work, DecorInsets::SSD), ok);
    }

    #[test]
    fn the_shadow_extends_eight_pixels_beyond_the_frame_on_every_side() {
        let frame = rect(100, 100, 400, 300);
        let outer = shadow_bounds(frame, DecorInsets::SSD);
        assert_eq!(SHADOW_PX, 8, "four 2px rings");
        assert_eq!((outer.loc.x, outer.loc.y), (92, 92));
        assert_eq!((outer.size.w, outer.size.h), (416, 316));
    }
}
