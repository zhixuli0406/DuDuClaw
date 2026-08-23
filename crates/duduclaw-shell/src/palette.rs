// Shell-wide color palette — Shell-S1 (2026-08-20, dark theme for
// Home + overlays).
//
// ── Why this used to be `oobe/palette.rs` and now lives here ─────────────
// `OobePalette` (see git history) was born scoped to `oobe/` (`pub(super)`,
// file lived at `oobe/palette.rs`) because OOBE was the only themed surface
// at the time. This round extends theming to Home + its three overlays
// (`home.rs`/`home/home_dock.rs`/`overlay/{launcher,notifications,
// controlcenter}.rs`), all of which are SIBLINGS of `oobe`, not descendants
// — they cannot reach a `pub(super)` item scoped inside `oobe`. Two designs
// were on the table:
//   (a) Keep the type named `OobePalette`, in `oobe/palette.rs`, and just
//       widen its visibility to `pub(crate)` + re-export it from crate root.
//       Zero renames anywhere `OobePalette` is already used (10 files under
//       `oobe/`) — the smallest possible diff.
//   (b) Move the file to crate root and rename the type to `ShellPalette`.
//       Touches every one of those same 10 files (import path + type name),
//       but every touch is a mechanical find-and-replace with no logic
//       change, and rustc's own borrow/type checker catches any miss
//       immediately (there is no way to half-do this rename and still
//       compile).
// (b) was chosen anyway, despite (a)'s smaller diff: this type is now
// genuinely shell-wide — a reader hitting `ShellPalette` in `home.rs` should
// not have to learn that it is secretly named after a DIFFERENT surface
// (OOBE) that happens to live in a sibling module. Naming it after what it
// actually is costs one mechanical rename pass (compiler-verified, see
// above) and pays for itself every time someone new reads `home.rs` or
// `overlay/*.rs` without first reading `oobe/`. The file's LOCATION also
// moved (crate root, alongside `home.rs`/`overlay.rs`/`surface.rs`/
// `fake_data.rs`) for the same reason — `oobe/palette.rs` implied
// OOBE-only scope by its very path.
//
// `oobe/`'s own call sites (`render.rs`, `widgets.rs`, `steps/*.rs`) all
// import `crate::palette::ShellPalette` now instead of `crate::oobe::
// palette::OobePalette` — see those files' own diffs. `OobeFlow::palette()`
// (`oobe/mod.rs`) still exists with the exact same job (resolve the current
// `ThemeChoice` into a concrete palette, fresh, every render pass) — see
// that method's own doc comment, which is otherwise unchanged by this move.
//
// ── Home/overlay theming: field shape ─────────────────────────────────────
// Every field inherited from the original `OobePalette` (`app_shell` through
// `destructive`) keeps its EXACT light/dark values and meaning — see the
// original module's still-relevant reasoning inline below (surface_border's
// precomposited-alpha correctness fix, the four derived-method dispatches,
// etc.). What's NEW this round is the fields/methods Home/overlay need that
// OOBE's three screens never touched:
//   - `surface_raised` / `surface_selected`: OOBE never distinguished
//     "surface" from "elevated card surface" because in BOTH OOBE's cases
//     they render on `app_shell`, a plain page background. Home's cards/
//     panels/dock sit on a GRADIENT wallpaper instead, and — crucially —
//     light's `SURFACE` and `SURFACE_RAISED` tokens happen to be identical
//     (`0xffffff` both), so the pre-existing `surface` field alone was
//     "accidentally correct" for every light call site. Dark's `SURFACE`
//     (`0x18181b`, menu bar / launcher footer) and `SURFACE_RAISED`
//     (`0x1e1e21`, every card/panel/dock) are DIFFERENT — reusing `surface`
//     for both in dark would paint every card the wrong shade. See each
//     call site in `home.rs`/`home/home_dock.rs`/`overlay/*.rs` for which
//     one it actually needs; the two are NOT interchangeable in dark.
//   - `text_secondary` / `text_faint`: the approved dark canvas's own text
//     ladder does not map 1:1 onto MDS's `FOREGROUND`/`MUTED_FOREGROUND`
//     pair — it's a 4-step ladder (`#fafafa`/`#d4d4d8`/`#9f9fa9`/`#71717b`
//     dark, mapping from light `#09090b`/`#52525c`/`#71717b`/`#9f9fa9`).
//     Step 1 is `foreground`, step 3 is `muted_foreground` (both already MDS
//     tokens) — steps 2 and 4 have no MDS token at all (Home/overlay's
//     design board picked them directly, same "no matching token, use a
//     literal" pattern this crate's `home.rs` already documents for its OWN
//     per-site hex). Naming them here instead of inlining a raw hex at each
//     of their many call sites keeps the ladder's THEME-INVERSION visible
//     in one place — note step 4's dark value (`0x71717b`) is step 3's
//     LIGHT value, and step 3's dark value (`0x9f9fa9`) is step 4's light
//     value: the ladder doesn't just swap per-theme colors independently,
//     ranks 3 and 4 literally trade hex values across the light/dark split.
//   - `brand_bright`: the approved canvas uses a LIGHTER blue than `brand`
//     itself for on-dark-surface accent TEXT (links, "Enter 交辦" hint,
//     badge text) — `#59a6ff`, distinct from `brand`'s `#4390ee`. Light has
//     no equivalent second shade (its accent text IS `brand`, `#2171cc`),
//     so `brand_bright == brand` in `light()`.
//   - `warning` / `warning_dot`: same split as `brand`/`brand_bright` but
//     for the OPPOSITE reason — `warning` (badge/ring token, `#cb9400`
//     dark) is NOT what the small circular alert dots (menu-bar ticker,
//     dock agent "needs human" status dot) use. Those dots use a distinct
//     brighter literal (`#e0a41c` dark) the approved canvas never reuses
//     for anything else. Light has no such split (`#dca400` serves both
//     roles), so `warning_dot == warning` in `light()`.
//   - `wallpaper_from` / `wallpaper_to`: Home's full-bleed background
//     gradient — the ONE color pair that has no MDS token at all in either
//     theme (it's bespoke wallpaper art direction, not a semantic surface).
//     Two stops, not three: `home.rs`'s own header comment already explains
//     gpui's `linear_gradient` is 2-stop-only, so the design board's middle
//     stop was already being dropped before this round.
//   - `neutral_tile_top` / `neutral_tile_bottom` / `neutral_tile_border`:
//     the "white/near-white app icon" gradient (Files/Browser in the dock,
//     the matching result row in Launcher) is the ONE identity-ish visual
//     that the approved canvas's own annotation says DOES need a themed
//     swap (`#2d2d31→#1e1e21` + a 14%-white border in dark) even though
//     every OTHER dock/app-icon color is explicitly "identity, not
//     themed". APP-1 (2026-08-22) made this the ONLY app-tile treatment
//     rather than an exception: the dock and the Launcher render REAL
//     installed apps now (`crate::apps::installed`), and a real app has no
//     design-board colour to lift, so every app tile uses this neutral pair
//     with the app's own first character in it (`InstalledApp::glyph`).
//   - `settings_gradient_top` / `settings_gradient_bottom`: the neutral GRAY
//     "settings"/"system update" icon gradient reused verbatim by
//     `home/home_dock.rs`'s dock settings icon AND `overlay/notifications.rs`'s
//     system-update row icon — factored into the palette once instead of
//     duplicated as a literal pair in both files.
//
// ── Badges: `BadgeKind` indirection instead of raw hex fields ─────────────
// `fake_data::GoalCard`/`ActivityRow` used to store each badge's `(text_hex,
// bg_hex)` pair directly as `u32` literals lifted straight from the design
// board — that only worked because there was exactly one board (light) to
// lift from. Doubling every such field to `(text_hex_light, bg_hex_light,
// text_hex_dark, bg_hex_dark)` would smear theme-specific literals across a
// DATA file whose whole point (see that file's own header comment) is
// staying independent of any rendering concern. Instead `fake_data` now
// stores which SEMANTIC kind a badge/dot is (`fake_data::BadgeKind::
// {Warning,Success,Brand}` — plain, `Copy`, gpui-free, same "pure data"
// discipline that module already holds itself to) and this palette resolves
// the concrete color for whichever theme is active via `badge_accent`/
// `badge_bg`/`badge_text` below. This is exactly the same kind of narrow,
// deliberate exception `theme.rs` itself models with `alpha()`: one small
// indirection point instead of the same three-color recipe duplicated at
// every call site.
//
// The three kinds' bg/text pairs come straight off the approved canvas and
// do NOT reduce to one clean formula — light uses flat OPAQUE hex the board
// authored by hand (`#fef3c7`/`#b45309` for warning, etc., no derivation
// from `warning`/`success`/`brand` at all), while dark uses each kind's own
// accent token at a 15–16% alpha tint for the background plus a distinct
// hand-picked LIGHTER text shade — see `badge_bg`/`badge_text`'s own bodies
// for the exact pairs, each traceable to a literal in `Main.dc.html`/
// `Notifications.dc.html` (dark canvas) vs. the light counterpart.
// `Rejected`/destructive approval-card outcomes are the one status color
// NEITHER canvas actually shows (no board ever renders a rejected card) —
// `overlay/notifications.rs` keeps approximating that state directly off
// the plain `destructive` field rather than adding a fourth `BadgeKind`,
// same "no board precedent, approximated" honesty the original light-only
// code already carried for this exact gap.

use gpui::{px, rgb, BoxShadow, Global, Hsla, Rgba};

use duduclaw_native_gui::theme;

use crate::fake_data::BadgeKind;
use crate::oobe::ThemeChoice;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ShellPalette {
    dark: bool,

    pub(crate) app_shell: u32,
    pub(crate) surface: u32,
    /// See this module's header comment — NOT interchangeable with `surface`
    /// in dark (they differ: `0x18181b` vs `0x1e1e21`), even though the two
    /// happen to collapse to the same value (`0xffffff`) in light.
    pub(crate) surface_raised: u32,
    pub(crate) surface_hover: u32,
    pub(crate) surface_selected: u32,
    /// Precomposited — see the original `OobePalette` design note (still
    /// accurate) preserved below `light()`/`dark()` for why this one field
    /// is `Rgba`, not `u32`.
    pub(crate) surface_border: Rgba,
    pub(crate) secondary: u32,
    pub(crate) secondary_foreground: u32,
    pub(crate) muted: u32,
    /// Text ladder rank 3 — see this module's header comment for the
    /// 4-step ladder this is one member of.
    pub(crate) muted_foreground: u32,
    /// Text ladder rank 2 — new this round, see header comment.
    pub(crate) text_secondary: u32,
    /// Text ladder rank 4 — new this round, see header comment. Note this
    /// is NOT simply "more muted than `muted_foreground`" — dark's value
    /// here (`0x71717b`) is literally `muted_foreground`'s LIGHT value; the
    /// ladder inverts across ranks 3/4 between themes, it does not just
    /// recolor each rank independently.
    pub(crate) text_faint: u32,
    /// Text ladder rank 1.
    pub(crate) foreground: u32,
    pub(crate) brand: u32,
    /// Accent TEXT shade — see header comment. Equal to `brand` in light.
    pub(crate) brand_bright: u32,
    pub(crate) brand_foreground: u32,
    pub(crate) ring: u32,
    pub(crate) success: u32,
    /// Badge/ring token — see header comment for why this is distinct from
    /// `warning_dot`.
    pub(crate) warning: u32,
    /// Small circular alert-dot literal — see header comment. Equal to
    /// `warning` in light.
    pub(crate) warning_dot: u32,
    pub(crate) destructive: u32,
    /// Home's full-bleed wallpaper gradient, 2 stops (gpui has no 3-stop
    /// `linear_gradient` — see `home.rs`'s own header comment).
    pub(crate) wallpaper_from: u32,
    pub(crate) wallpaper_to: u32,
    /// "White/near-white app tile" gradient — see header comment for why
    /// this is the one documented exception to "identity colors stay
    /// hardcoded, not themed".
    pub(crate) neutral_tile_top: u32,
    pub(crate) neutral_tile_bottom: u32,
    pub(crate) neutral_tile_border: Rgba,
    /// Neutral gray "settings"/"system update" icon gradient — see header
    /// comment; shared by `home/home_dock.rs`'s dock settings icon and
    /// `overlay/notifications.rs`'s system-update row icon.
    pub(crate) settings_gradient_top: u32,
    pub(crate) settings_gradient_bottom: u32,
}

impl ShellPalette {
    pub(crate) fn light() -> Self {
        Self {
            dark: false,
            app_shell: theme::light::APP_SHELL,
            surface: theme::light::SURFACE,
            surface_raised: theme::light::SURFACE_RAISED,
            surface_hover: theme::light::SURFACE_HOVER,
            surface_selected: theme::light::SURFACE_SELECTED,
            surface_border: theme::alpha(theme::light::SURFACE_BORDER, theme::light::SURFACE_BORDER_ALPHA),
            secondary: theme::light::SECONDARY,
            secondary_foreground: theme::light::SECONDARY_FOREGROUND,
            muted: theme::light::MUTED,
            muted_foreground: theme::light::MUTED_FOREGROUND,
            // Main.dc.html (light): #52525c / #9f9fa9 — no matching
            // `theme::light::*` token for either ladder rank, same as every
            // other per-site literal this crate's `home.rs` already
            // documents; centralized here since both are reused across many
            // call sites (see header comment).
            text_secondary: 0x52525c,
            text_faint: 0x9f9fa9,
            foreground: theme::light::FOREGROUND,
            brand: theme::light::BRAND,
            brand_bright: theme::light::BRAND,
            brand_foreground: theme::light::BRAND_FOREGROUND,
            ring: theme::light::RING,
            success: theme::light::SUCCESS,
            warning: theme::light::WARNING,
            warning_dot: theme::light::WARNING,
            destructive: theme::light::DESTRUCTIVE,
            wallpaper_from: 0xeef1f6,
            wallpaper_to: 0xdde6f3,
            neutral_tile_top: 0xffffff,
            neutral_tile_bottom: 0xedeff4,
            neutral_tile_border: theme::alpha(0xe4e4e7, 1.0),
            settings_gradient_top: 0xd7dae0,
            settings_gradient_bottom: 0x969ca6,
        }
    }

    pub(crate) fn dark() -> Self {
        Self {
            dark: true,
            app_shell: theme::dark::APP_SHELL,
            surface: theme::dark::SURFACE,
            surface_raised: theme::dark::SURFACE_RAISED,
            surface_hover: theme::dark::SURFACE_HOVER,
            surface_selected: theme::dark::SURFACE_SELECTED,
            surface_border: theme::alpha(theme::dark::SURFACE_BORDER, theme::dark::SURFACE_BORDER_ALPHA),
            secondary: theme::dark::SECONDARY,
            secondary_foreground: theme::dark::SECONDARY_FOREGROUND,
            muted: theme::dark::MUTED,
            muted_foreground: theme::dark::MUTED_FOREGROUND,
            // duduclaw-os-home-dark canvas: #d4d4d8 / #71717b — the theme
            // ladder INVERSION named in this module's header comment: dark
            // rank 4 (`text_faint`) reuses light rank 3's own literal
            // (`0x71717b` == `theme::light::MUTED_FOREGROUND`), and dark
            // rank 3 (`muted_foreground`, above) reuses light rank 4's
            // literal (`0x9f9fa9`) — both correct, not a copy-paste swap.
            text_secondary: 0xd4d4d8,
            text_faint: 0x71717b,
            foreground: theme::dark::FOREGROUND,
            brand: theme::dark::BRAND,
            brand_bright: 0x59a6ff,
            brand_foreground: theme::dark::BRAND_FOREGROUND,
            ring: theme::dark::RING,
            success: theme::dark::SUCCESS,
            warning: theme::dark::WARNING,
            warning_dot: 0xe0a41c,
            destructive: theme::dark::DESTRUCTIVE,
            wallpaper_from: 0x0c0c0e,
            wallpaper_to: 0x12141c,
            neutral_tile_top: 0x2d2d31,
            neutral_tile_bottom: 0x1e1e21,
            neutral_tile_border: theme::alpha(0xffffff, 0.14),
            settings_gradient_top: 0x52525c,
            settings_gradient_bottom: 0x3a3a41,
        }
    }

    pub(crate) fn for_choice(choice: ThemeChoice) -> Self {
        match choice {
            ThemeChoice::Light => Self::light(),
            ThemeChoice::Dark => Self::dark(),
        }
    }

    /// Whether this is the dark palette — the escape hatch for the long
    /// tail of one-off per-site colors (shadow base color, glass-surface
    /// alphas, blob alphas, …) that don't reduce to a reusable named field
    /// (each used exactly once). Matches this crate's existing "hardcode
    /// with a citing comment when no token fits" convention (`home.rs`'s
    /// own header comment) — now with a light/dark branch instead of one
    /// literal, at each such site, rather than growing this struct into a
    /// junk drawer of single-use fields.
    pub(crate) fn is_dark(&self) -> bool {
        self.dark
    }

    /// Generic hairline border (`--border`) — see the original `OobePalette`
    /// design note: light/dark FORMULAS diverge (not just inputs), so this
    /// dispatches straight to `theme.rs`'s own audited constants rather than
    /// re-deriving them from stored fields.
    pub(crate) fn border(&self) -> Hsla {
        if self.dark {
            theme::dark::border()
        } else {
            theme::light::border()
        }
    }

    pub(crate) fn input_bg(&self) -> Rgba {
        if self.dark {
            theme::dark::input_bg()
        } else {
            theme::light::input_bg()
        }
    }

    pub(crate) fn input_border(&self) -> Hsla {
        if self.dark {
            theme::dark::input_border()
        } else {
            theme::light::input_border()
        }
    }

    /// Card elevation shadow (`--surface-shadow`).
    pub(crate) fn surface_shadow(&self) -> Vec<BoxShadow> {
        if self.dark {
            theme::dark::surface_shadow()
        } else {
            theme::light::surface_shadow()
        }
    }

    /// Dialog/floating-panel elevation shadow (`--floating-shadow`) — NEW
    /// this round (OOBE never needed it; Home's three overlay PANELS all
    /// use it). Reuses `theme.rs`'s own dark recipe (pure-black-based, 0.46/
    /// 0.28 opacity) rather than hand-tuning a bespoke shadow per overlay —
    /// see this module's header comment on the badge-color precedent for
    /// why a small approximation gap against any ONE board's exact
    /// literal (the three overlay panels' own box-shadow numbers differ
    /// slightly from each other and from this token, same as they already
    /// did in light) is accepted rather than chased per call site.
    pub(crate) fn floating_shadow(&self) -> Vec<BoxShadow> {
        if self.dark {
            theme::dark::floating_shadow()
        } else {
            theme::light::floating_shadow()
        }
    }

    /// A small square icon's drop shadow (dock app icons, launcher result
    /// icons, the neutral/settings gray icons) — NOT a fixed MDS token
    /// (there isn't one for this specific "colored square, offset 0/3,
    /// blur 6" recipe), and the exact opacity genuinely differs per-icon
    /// FAMILY between the two canvases (colored icons deepen from 0.20 to
    /// 0.35 in dark, neutral/bordered icons from 0.14 to 0.30, the settings
    /// gray icon from 0.18 to 0.30 — three distinct pairs, not one ratio).
    /// Callers pass their own board-sourced `(light, dark)` opacity pair;
    /// this just centralizes the shared "navy-tinted in light, pure black
    /// in dark" base-color swap (design brief: "shadows deepen to
    /// black-based") so that swap isn't re-typed at every call site.
    pub(crate) fn icon_shadow(&self, opacity_light: f32, opacity_dark: f32) -> Vec<BoxShadow> {
        let (base, opacity) = if self.dark { (0x000000, opacity_dark) } else { (0x0f172a, opacity_light) };
        vec![BoxShadow::new(px(0.), px(3.), rgb(base).opacity(opacity).into()).blur_radius(px(6.))]
    }

    // ── Icon stroke tints (ICON-1, 2026-08-22) ───────────────────────────
    // `gpui::svg()` paints an alpha mask tinted by the element's own text
    // color (see `crate::icons`'s header comment), so these six methods ARE
    // the icon color system — the hexes written inside the asset files
    // never reach the screen. Every value below is read off the two
    // approved boards' own `stroke=`/`fill=` attributes for that icon,
    // light and dark side by side.
    //
    // Methods, not fields, for two reasons: three of the six resolve to the
    // SAME value in both themes (identity colors, exactly the exception
    // this module's header comment already documents for tile gradients),
    // which the `light_and_dark_disagree_on_every_plain_color_field` test
    // deliberately forbids for a plain field; and two of them are derived
    // from an existing field (`text_secondary`, `brand_bright`) rather than
    // being a new independent color, which a field would duplicate.

    /// Icon stroke on a COLORED (identity-gradient) app tile — the dock's
    /// mail/music/chat/folder squircles, and the white rules over the
    /// Launcher's green spreadsheet icon. `#ffffff` in BOTH boards: a white
    /// glyph on a saturated fill does not need to change per theme, since
    /// the fill it sits on doesn't either.
    pub(crate) fn icon_on_colored_tile(&self) -> u32 {
        0xffffff
    }

    /// Icon stroke on the neutral GRAY gradient (`settings_gradient_top`/
    /// `bottom`) — the dock's settings gear and Notifications' system-update
    /// row. Light `#ffffff` / dark `#d4d4d8`, which is exactly
    /// `text_secondary`'s dark value; both call sites already resolved this
    /// pair inline before ICON-1, so this is the same behavior with one
    /// owner instead of two copies.
    pub(crate) fn icon_on_neutral_gradient(&self) -> u32 {
        if self.dark {
            self.text_secondary
        } else {
            0xffffff
        }
    }

    /// Standalone control-surface icon stroke (ControlCenter's inactive
    /// quick tiles, its volume/brightness rows) — light `#52525c` / dark
    /// `#b0b0b8`. Both call sites already spelled this pair out inline
    /// before ICON-1; the values are unchanged.
    pub(crate) fn icon_control(&self) -> u32 {
        if self.dark {
            0xb0b0b8
        } else {
            0x52525c
        }
    }

    /// The "this part of the control is OFF" gray — light `#d4d4d8` / dark
    /// `#3f3f46`. ICON-3 (2026-08-23): named here because the OOBE Wi-Fi
    /// signal family needs it for its unlit arcs (`OOBE-KeySteps.dc.html`
    /// paints those `#d4d4d8`), and it is the SAME pair `overlay/
    /// controlcenter.rs`'s own `track_off_hex` helper already spells out for
    /// its toggle/slider tracks. That helper is deliberately left alone —
    /// it returns the identical values for a different family of widgets,
    /// and merging the two would mean one of the two files reaching across
    /// a module boundary for a two-line branch.
    ///
    /// Distinct from `icon_control()` above: that is an icon that IS active
    /// but sits on a neutral surface; this is a part of an icon that is
    /// deliberately unlit.
    pub(crate) fn icon_inactive(&self) -> u32 {
        if self.dark {
            0x3f3f46
        } else {
            0xd4d4d8
        }
    }

    /// The browser globe's stroke — light `#1f7ae0` (bespoke: NOT `brand`,
    /// which is `#2171cc`) / dark `#59a6ff`, which IS `brand_bright`
    /// exactly. Kept as one method so the asymmetry is stated once.
    pub(crate) fn icon_globe(&self) -> u32 {
        if self.dark {
            self.brand_bright
        } else {
            0x1f7ae0
        }
    }

    /// The solid accent color for a badge's dot/ring (goal-card status
    /// rings, the dock agent status dot's "brand" case — NOT its "needs
    /// human" case, which uses `warning_dot` instead, see this module's
    /// header comment for why those two are NOT the same field).
    pub(crate) fn badge_accent(&self, kind: BadgeKind) -> u32 {
        match kind {
            BadgeKind::Warning => self.warning,
            BadgeKind::Success => self.success,
            BadgeKind::Brand => self.brand,
        }
    }

    /// Badge pill background — see this module's header comment for why
    /// light and dark do NOT share one derivation (light is a flat opaque
    /// hex with no token relationship at all; dark is each kind's own
    /// accent token at a 15–16% tint). Every literal here is traced to
    /// `Main.dc.html`/`Notifications.dc.html` (light board) and their
    /// `duduclaw-os-home-dark` counterparts.
    pub(crate) fn badge_bg(&self, kind: BadgeKind) -> Rgba {
        if self.dark {
            match kind {
                BadgeKind::Warning => theme::alpha(self.warning, 0.16),
                BadgeKind::Success => theme::alpha(self.success, 0.16),
                BadgeKind::Brand => theme::alpha(self.brand, 0.15),
            }
        } else {
            match kind {
                BadgeKind::Warning => theme::alpha(0xfef3c7, 1.0),
                BadgeKind::Success => theme::alpha(0xe9f6eb, 1.0),
                BadgeKind::Brand => theme::alpha(0xe8f1fb, 1.0),
            }
        }
    }

    /// Badge pill text — see `badge_bg`'s own doc comment; same "no clean
    /// derivation" caveat applies (dark's success text `#67c56e` and
    /// warning text `#ecbb4e` are hand-picked lighter shades, not
    /// `success`/`warning` themselves; dark's brand text IS `brand_bright`
    /// exactly, so that one case reuses the field instead of a fresh
    /// literal).
    pub(crate) fn badge_text(&self, kind: BadgeKind) -> Rgba {
        if self.dark {
            match kind {
                BadgeKind::Warning => theme::alpha(0xecbb4e, 1.0),
                BadgeKind::Success => theme::alpha(0x67c56e, 1.0),
                BadgeKind::Brand => theme::alpha(self.brand_bright, 1.0),
            }
        } else {
            match kind {
                BadgeKind::Warning => theme::alpha(0xb45309, 1.0),
                BadgeKind::Success => theme::alpha(self.success, 1.0),
                BadgeKind::Brand => theme::alpha(self.brand, 1.0),
            }
        }
    }

    /// WP-A3 (2026-08-22): resolves the D8 "DuDuClaw Verified" four-tier
    /// rating (`crate::apps::VerifiedTier`) onto this palette's existing
    /// `BadgeKind`/`destructive` tokens — deliberately NOT a fourth/fifth
    /// `BadgeKind` variant (same "no board precedent, reuse the plain
    /// `destructive` field instead of growing the enum" call this module's
    /// header comment already makes for the Rejected approval outcome).
    /// `Unrated` (no rating evidence — see that variant's own doc comment)
    /// resolves through `text_faint` rather than any accent color, since it
    /// isn't a rating at all.
    pub(crate) fn verified_bg(&self, tier: crate::apps::VerifiedTier) -> Rgba {
        use crate::apps::VerifiedTier;
        match tier {
            VerifiedTier::Verified => self.badge_bg(BadgeKind::Success),
            VerifiedTier::Works => self.badge_bg(BadgeKind::Brand),
            VerifiedTier::Partial => self.badge_bg(BadgeKind::Warning),
            // Same destructive-tint recipe `overlay/notifications.rs`'s own
            // rejected-outcome pill already uses (0.12 light / 0.16 dark).
            VerifiedTier::Unsupported => theme::alpha(self.destructive, if self.dark { 0.16 } else { 0.12 }),
            VerifiedTier::Unrated => theme::alpha(self.text_faint, if self.dark { 0.14 } else { 0.10 }),
        }
    }

    /// Text color half of [`Self::verified_bg`] — see that fn's own doc
    /// comment.
    pub(crate) fn verified_text(&self, tier: crate::apps::VerifiedTier) -> Rgba {
        use crate::apps::VerifiedTier;
        match tier {
            VerifiedTier::Verified => self.badge_text(BadgeKind::Success),
            VerifiedTier::Works => self.badge_text(BadgeKind::Brand),
            VerifiedTier::Partial => self.badge_text(BadgeKind::Warning),
            VerifiedTier::Unsupported => theme::alpha(self.destructive, 1.0),
            VerifiedTier::Unrated => theme::alpha(self.text_faint, 1.0),
        }
    }

    /// A single solid accent hex for the dock's space-constrained corner
    /// dot (see `home/home_dock.rs::dock_app`) — `None` for `Unrated`, so
    /// the dock (unlike the Launcher's roomier badge pill) shows NOTHING
    /// extra for the common "no rating evidence" case rather than a
    /// cluttered always-on dot; this is the "無資料誠實顯示" boundary
    /// applied to a context with no room for the word "未評級" itself.
    pub(crate) fn verified_accent_dot(&self, tier: crate::apps::VerifiedTier) -> Option<u32> {
        use crate::apps::VerifiedTier;
        match tier {
            VerifiedTier::Verified => Some(self.success),
            VerifiedTier::Works => Some(self.brand),
            VerifiedTier::Partial => Some(self.warning),
            VerifiedTier::Unsupported => Some(self.destructive),
            VerifiedTier::Unrated => None,
        }
    }
}

impl Default for ShellPalette {
    fn default() -> Self {
        Self::light()
    }
}

/// Still needed for `oobe::widgets::OobeTextField` — see that type's own
/// doc comment (unchanged by this move) for why it needs AMBIENT palette
/// access rather than a threaded parameter.
impl Global for ShellPalette {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn for_choice_resolves_to_the_matching_constructor() {
        assert_eq!(ShellPalette::for_choice(ThemeChoice::Light), ShellPalette::light());
        assert_eq!(ShellPalette::for_choice(ThemeChoice::Dark), ShellPalette::dark());
    }

    #[test]
    fn default_is_light() {
        assert_eq!(ShellPalette::default(), ShellPalette::light());
    }

    #[test]
    fn is_dark_matches_the_constructor() {
        assert!(!ShellPalette::light().is_dark());
        assert!(ShellPalette::dark().is_dark());
    }

    #[test]
    fn light_and_dark_disagree_on_every_plain_color_field() {
        // Same "not a tautology on `dark` alone" pin the original module
        // established — extended to cover every field added this round too,
        // so a copy-paste that left a NEW field pointing at the wrong
        // theme's source constant still fails loudly here.
        let light = ShellPalette::light();
        let dark = ShellPalette::dark();
        assert_ne!(light.app_shell, dark.app_shell);
        assert_ne!(light.surface, dark.surface);
        assert_ne!(light.surface_raised, dark.surface_raised);
        assert_ne!(light.surface_hover, dark.surface_hover);
        assert_ne!(light.surface_selected, dark.surface_selected);
        assert_ne!(light.secondary, dark.secondary);
        assert_ne!(light.muted, dark.muted);
        assert_ne!(light.muted_foreground, dark.muted_foreground);
        assert_ne!(light.text_secondary, dark.text_secondary);
        assert_ne!(light.text_faint, dark.text_faint);
        assert_ne!(light.foreground, dark.foreground);
        assert_ne!(light.brand, dark.brand);
        assert_ne!(light.ring, dark.ring);
        assert_ne!(light.success, dark.success);
        assert_ne!(light.warning, dark.warning);
        assert_ne!(light.warning_dot, dark.warning_dot);
        assert_ne!(light.destructive, dark.destructive);
        assert_ne!(light.wallpaper_from, dark.wallpaper_from);
        assert_ne!(light.wallpaper_to, dark.wallpaper_to);
        assert_ne!(light.neutral_tile_top, dark.neutral_tile_top);
        assert_ne!(light.neutral_tile_bottom, dark.neutral_tile_bottom);
        assert_ne!(light.settings_gradient_top, dark.settings_gradient_top);
        assert_ne!(light.settings_gradient_bottom, dark.settings_gradient_bottom);
        assert_ne!(light.secondary_foreground, dark.secondary_foreground);
        assert_eq!(light.brand_foreground, dark.brand_foreground);
    }

    #[test]
    fn text_ladder_ranks_3_and_4_swap_hex_values_across_themes() {
        // The inversion named in this module's header comment, pinned
        // explicitly rather than left as an implicit consequence of the
        // two constructors above.
        let light = ShellPalette::light();
        let dark = ShellPalette::dark();
        assert_eq!(dark.text_faint, light.muted_foreground);
        assert_eq!(dark.muted_foreground, light.text_faint);
    }

    #[test]
    fn brand_bright_and_warning_dot_only_diverge_from_their_sibling_in_dark() {
        let light = ShellPalette::light();
        let dark = ShellPalette::dark();
        assert_eq!(light.brand_bright, light.brand);
        assert_ne!(dark.brand_bright, dark.brand);
        assert_eq!(light.warning_dot, light.warning);
        assert_ne!(dark.warning_dot, dark.warning);
    }

    #[test]
    fn dark_surface_border_carries_its_real_translucent_alpha_not_a_flat_one() {
        let dark = ShellPalette::dark();
        assert_eq!(dark.surface_border.a, theme::dark::SURFACE_BORDER_ALPHA);
        assert!(dark.surface_border.a < 1.0, "dark surface_border must not be flattened to opaque");
    }

    #[test]
    fn light_surface_border_is_fully_opaque() {
        let light = ShellPalette::light();
        assert_eq!(light.surface_border.a, 1.0);
    }

    #[test]
    fn neutral_tile_border_is_translucent_in_dark_and_opaque_in_light() {
        let light = ShellPalette::light();
        let dark = ShellPalette::dark();
        assert_eq!(light.neutral_tile_border.a, 1.0);
        assert_eq!(dark.neutral_tile_border.a, 0.14);
    }

    #[test]
    fn border_input_and_shadow_dispatch_to_the_matching_theme_module() {
        let light = ShellPalette::light();
        let dark = ShellPalette::dark();
        assert_eq!(light.border(), theme::light::border());
        assert_eq!(dark.border(), theme::dark::border());
        assert_eq!(light.input_border(), theme::light::input_border());
        assert_eq!(dark.input_border(), theme::dark::input_border());
        assert_ne!(light.border(), dark.border(), "light/dark hairline border must actually differ");
        assert_eq!(light.surface_shadow(), theme::light::surface_shadow());
        assert_eq!(dark.surface_shadow(), theme::dark::surface_shadow());
        assert_eq!(light.floating_shadow(), theme::light::floating_shadow());
        assert_eq!(dark.floating_shadow(), theme::dark::floating_shadow());
    }

    #[test]
    fn light_input_bg_is_transparent_dark_input_bg_is_not() {
        let light = ShellPalette::light();
        let dark = ShellPalette::dark();
        assert_eq!(light.input_bg().a, 0.0);
        assert!(dark.input_bg().a > 0.0);
    }

    #[test]
    fn icon_shadow_uses_a_navy_base_in_light_and_pure_black_in_dark() {
        let light = ShellPalette::light();
        let dark = ShellPalette::dark();
        let light_shadow = light.icon_shadow(0.20, 0.35);
        let dark_shadow = dark.icon_shadow(0.20, 0.35);
        // Same opacity requested, but the underlying base color differs —
        // confirmed indirectly via inequality (BoxShadow's `color` field is
        // an opaque `Hsla`, not something this test re-derives by hand).
        assert_ne!(light_shadow[0].color, dark_shadow[0].color);
    }

    /// ICON-1: the three THEMED icon tints must actually change with the
    /// theme (an icon that keeps its light stroke on a dark surface is the
    /// exact failure mode `crate::icons`'s one-asset-set-for-two-boards
    /// design has to avoid), and the three IDENTITY ones must not.
    #[test]
    fn icon_tints_split_cleanly_into_themed_and_identity() {
        let light = ShellPalette::light();
        let dark = ShellPalette::dark();

        assert_ne!(light.icon_on_neutral_gradient(), dark.icon_on_neutral_gradient());
        assert_ne!(light.icon_control(), dark.icon_control());
        assert_ne!(light.icon_globe(), dark.icon_globe());

        assert_eq!(light.icon_on_colored_tile(), dark.icon_on_colored_tile());
    }

    /// The two derived-from-an-existing-field cases, pinned so a future
    /// edit to `text_secondary`/`brand_bright` can't silently desync the
    /// icon color from the token this module's doc comment says it IS.
    #[test]
    fn derived_icon_tints_track_their_source_field() {
        let dark = ShellPalette::dark();
        assert_eq!(dark.icon_on_neutral_gradient(), dark.text_secondary);
        assert_eq!(dark.icon_globe(), dark.brand_bright);
    }

    /// The board's own light/dark stroke pairs, transcribed. These are the
    /// only literals in this module that a reader cannot check against a
    /// token, so they get pinned directly against the boards' `stroke=`
    /// values (`commercial/design/duduclaw-os-desktop/*.dc.html` and its
    /// `duduclaw-os-home-dark` twin).
    #[test]
    fn icon_tints_match_the_boards_stroke_values() {
        let light = ShellPalette::light();
        let dark = ShellPalette::dark();
        assert_eq!(light.icon_on_colored_tile(), 0xffffff);
        assert_eq!(light.icon_on_neutral_gradient(), 0xffffff);
        assert_eq!(dark.icon_on_neutral_gradient(), 0xd4d4d8);
        assert_eq!(light.icon_control(), 0x52525c);
        assert_eq!(dark.icon_control(), 0xb0b0b8);
        assert_eq!(light.icon_globe(), 0x1f7ae0);
        assert_eq!(dark.icon_globe(), 0x59a6ff);
    }

    #[test]
    fn badge_accent_maps_each_kind_to_its_own_token() {
        for palette in [ShellPalette::light(), ShellPalette::dark()] {
            assert_eq!(palette.badge_accent(BadgeKind::Warning), palette.warning);
            assert_eq!(palette.badge_accent(BadgeKind::Success), palette.success);
            assert_eq!(palette.badge_accent(BadgeKind::Brand), palette.brand);
        }
    }

    #[test]
    fn badge_bg_and_text_differ_between_light_and_dark_for_every_kind() {
        let light = ShellPalette::light();
        let dark = ShellPalette::dark();
        for kind in [BadgeKind::Warning, BadgeKind::Success, BadgeKind::Brand] {
            assert_ne!(light.badge_bg(kind), dark.badge_bg(kind), "{kind:?} bg must differ");
            assert_ne!(light.badge_text(kind), dark.badge_text(kind), "{kind:?} text must differ");
        }
    }

    #[test]
    fn dark_badge_bg_is_translucent_light_badge_bg_is_opaque() {
        // The light canvas hand-authors flat opaque pill fills; dark tints
        // the kind's own accent token instead — see `badge_bg`'s own doc
        // comment.
        let light = ShellPalette::light();
        let dark = ShellPalette::dark();
        for kind in [BadgeKind::Warning, BadgeKind::Success, BadgeKind::Brand] {
            assert_eq!(light.badge_bg(kind).a, 1.0, "{kind:?}");
            assert!(dark.badge_bg(kind).a < 1.0, "{kind:?}");
        }
    }

    // ── WP-A3: DuDuClaw Verified badge ──────────────────────────────────────

    #[test]
    fn verified_bg_and_text_differ_between_light_and_dark_for_every_tier() {
        use crate::apps::VerifiedTier;
        let light = ShellPalette::light();
        let dark = ShellPalette::dark();
        for tier in
            [VerifiedTier::Verified, VerifiedTier::Works, VerifiedTier::Partial, VerifiedTier::Unsupported, VerifiedTier::Unrated]
        {
            assert_ne!(light.verified_bg(tier), dark.verified_bg(tier), "{tier:?} bg must differ");
            assert_ne!(light.verified_text(tier), dark.verified_text(tier), "{tier:?} text must differ");
        }
    }

    #[test]
    fn verified_bg_and_text_are_never_fully_transparent() {
        // A badge nobody can read is worse than no badge — every tier
        // (including `Unrated`, which has no accent color of its own) must
        // still resolve to a visible pill.
        use crate::apps::VerifiedTier;
        for palette in [ShellPalette::light(), ShellPalette::dark()] {
            for tier in
                [VerifiedTier::Verified, VerifiedTier::Works, VerifiedTier::Partial, VerifiedTier::Unsupported, VerifiedTier::Unrated]
            {
                assert!(palette.verified_bg(tier).a > 0.0, "{tier:?} bg must not be fully transparent");
                assert!(palette.verified_text(tier).a > 0.0, "{tier:?} text must not be fully transparent");
            }
        }
    }

    #[test]
    fn verified_accent_dot_is_none_only_for_unrated() {
        use crate::apps::VerifiedTier;
        for palette in [ShellPalette::light(), ShellPalette::dark()] {
            assert_eq!(palette.verified_accent_dot(VerifiedTier::Unrated), None);
            for tier in [VerifiedTier::Verified, VerifiedTier::Works, VerifiedTier::Partial, VerifiedTier::Unsupported] {
                assert!(palette.verified_accent_dot(tier).is_some(), "{tier:?} must have a dot color");
            }
        }
    }

    #[test]
    fn verified_unsupported_reuses_the_plain_destructive_token() {
        // Same "no board precedent, reuse `destructive` instead of a new
        // color family" call this module's header comment already makes for
        // the Rejected approval outcome — pin it so a future refactor can't
        // silently reintroduce a fourth/fifth `BadgeKind` variant instead.
        use crate::apps::VerifiedTier;
        for palette in [ShellPalette::light(), ShellPalette::dark()] {
            assert_eq!(palette.verified_accent_dot(VerifiedTier::Unsupported), Some(palette.destructive));
        }
    }
}
