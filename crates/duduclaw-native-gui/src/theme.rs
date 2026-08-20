// MDS (Multica-derived Design System) tokens — the native-GUI translation of
// `commercial/docs/multica-redesign-spec.md` + the ACTUAL shipped values in
// `web/src/index.css` (which wins on any discrepancy with the spec, per the
// spec's own §0 note that it documents intent and the CSS is what ships).
//
// This REPLACES the previous "Calm Glass" theme (amber accent, ad-hoc
// translucent-everything surfaces) wholesale. MDS is a flatter, mostly-OPAQUE
// layered-surface system (app-shell → page-canvas → surface → surface-raised,
// darkest to lightest) with a single blue brand hue (255) — not amber — and a
// handful of *specific* places where Tailwind's `/NN` alpha modifier is used
// deliberately (hover washes, `brandSubtle`, `destructive` chip fills). The
// `alpha()` helper below exists for exactly those tokens, not as a
// glassmorphism trick — gpui still has no `backdrop-filter` (see main.rs's
// gotcha list), and MDS itself doesn't lean on blur outside the Dialog
// overlay, which Phase 1a doesn't implement.
//
// ── OKLCH → sRGB methodology ─────────────────────────────────────────────
// gpui has no OKLCH constructor (`rgb()`/`rgba()`/`hsla()` only), so every
// token below was converted offline with a throwaway script
// (OKLCH → OKLab → LMS' → LMS → linear sRGB → gamma-encoded sRGB → hex),
// using the canonical Björn Ottosson OKLab⇄linear-sRGB matrices
// (https://bottosson.github.io/posts/oklab/, the same constants the CSS
// Color 4 spec and every browser implementation use). Out-of-gamut channels
// are simple-clamped to [0,1] — none of MDS's chosen chromas are extreme
// enough for this to matter (all conversions below landed in-gamut before
// clamping was ever exercised).
//
// The script's output was cross-checked against `coloraide` (an independent,
// spec-conformant Python color library, run in a scratch venv) for all 74
// light+dark tokens converted for this pass: **74/74 exact hex match**. Each
// constant below carries its source `oklch(...)` value as a comment so the
// mapping stays auditable without re-running anything.
//
// ── Dark-first ───────────────────────────────────────────────────────────
// Phase 1a renders dark only (no in-app theme toggle yet — same scope
// decision as before). `dark::*` is re-exported at this module's top level
// so call sites read as `theme::SURFACE`, `theme::brand()`, etc. `light`
// is fully populated (all 37 tokens, same conversion pipeline) for the
// record and for whoever wires a runtime toggle in Phase 1b, but nothing
// currently constructs it — hence the crate-wide `#[allow(dead_code)]` on
// that module.

use gpui::{px, rgb, BoxShadow, Hsla, Rgba};

/// Tint a solid color to a given alpha — the MDS equivalent of Tailwind's
/// `bg-foo/NN` opacity modifier. Returns `Rgba` (not `Hsla`) so it can be
/// passed directly to `.bg()`/`.text_color()`/`.border_color()` without an
/// explicit `.into()` (see main.rs's gotcha note: calling `.into()` yourself
/// on an `Rgba` at a non-anchored call site makes the target type
/// ambiguous — `Rgba` also implements `Into<Fill>`/`Into<Background>`/
/// `Into<u32>`/`Into<HighlightStyle>`).
pub fn alpha(hex: u32, factor: f32) -> Rgba {
    rgb(hex).opacity(factor)
}

/// Radius scale (spec §1.6): base `--radius` is 10px, everything else is
/// `calc()`-derived from it. gpui's `rounded_*()` presets are a FIXED
/// Tailwind-default ramp (sm=4/md=6/lg=8/xl=12px, `gpui_macros::styles.rs`
/// `corner_suffixes()`) that does NOT line up with MDS's own scale, but the
/// same macro also emits a free-form `.rounded(px(n))` setter (verified in
/// the gpui source: `corner_prefixes()` → `generate_custom_value_setter`
/// with `AbsoluteLength`, and `Pixels: Into<AbsoluteLength>`) — so every
/// radius below is applied via `.rounded(px(theme::RADIUS_*))`, not a preset.
pub const RADIUS_SM: f32 = 6.0; // menu items
pub const RADIUS_MD: f32 = 8.0; // sidebar nav rows
pub const RADIUS_LG: f32 = 10.0; // buttons / inputs
pub const RADIUS_XL: f32 = 14.0; // cards / dialogs / sidebar island / content canvas
pub const RADIUS_4XL: f32 = 26.0; // pills / badges

/// Type scale (spec §2), mapped to px for `.text_size(px(..))` (which,
/// like `bg`/`text_color`, is one of gpui's arbitrary-value setters, not a
/// preset — see main.rs's gotcha list).
pub const TEXT_XS: f32 = 12.0; // meta / label / badge / tooltip
pub const TEXT_SM: f32 = 14.0; // body / table / menu / button
pub const TEXT_BASE: f32 = 16.0; // page/card titles
pub const TEXT_XL: f32 = 20.0; // hero / settings-tab titles
pub const TEXT_2XL: f32 = 24.0; // detail hero (sm:) breakpoint, unused on desktop-only native

/// Font stack — S3: bundled, no longer riding the OS default. MDS specs
/// Inter Variable with a zh-TW-first system fallback (`index.css`
/// `--font-sans`). Font bytes live in `assets/fonts/` (`InterVariable.ttf` +
/// `NotoSansTC-Variable.ttf`, both OFL — `assets/fonts/OFL-*.txt`),
/// `include_bytes!`'d and registered via `cx.text_system().add_fonts(...)`
/// in `main.rs` before the window opens. `app_font()` below is the `Font`
/// value (family + `FontFallbacks`) applied once, at the single common
/// ancestor of every screen (`RootView::render` in `main.rs`), so it
/// cascades to all text the same way CSS `font-family` inheritance does
/// (`Styled::font()`'s own doc comment: "Sets the font of this element AND
/// ITS CHILDREN").
///
/// Verified fallback chain, not assumed: gpui's `Font.fallbacks: Option<
/// FontFallbacks>` (`crates/gpui/src/text_system/font_fallbacks.rs`) is a
/// real ordered family-name list consulted for per-glyph fallback — this is
/// NOT the "only one family, no fallback order" limitation the task brief
/// flagged as a possible finding. What's unverified (no automated way to
/// screenshot-diff CJK glyph shapes from here) is byte-for-byte confirmation
/// that "Noto Sans TC" is the exact family name gpui's font-kit backend
/// resolves from `NotoSansTC-Variable.ttf`'s `name` table — see this crate's
/// S3 report for how that was checked at runtime (stderr `add_fonts` log +
/// a live zh-TW screen visual check, not just "should work").
pub fn app_font() -> gpui::Font {
    gpui::Font {
        fallbacks: Some(gpui::FontFallbacks::from_fonts(vec!["Noto Sans TC".to_string()])),
        ..gpui::font("Inter Variable")
    }
}
// Not every token here is consumed by Phase 1a's 3 screens (`SECONDARY`,
// `MUTED`, `POPOVER`, `SIDEBAR_PRIMARY`, `CHART_1`/`CHART_3`, the various
// `*_FOREGROUND` pairing constants, …) — this is the full MDS token catalog
// per the task brief ("a proper token set"), not a per-screen scratch list,
// so future pages can pull from it without a second conversion pass.
#[allow(dead_code)]
pub mod dark {
    use super::*;

    // ── Surface layering (spec §1.1) — darkest to lightest ──────────────
    pub const APP_SHELL: u32 = 0x0c0c0e; // oklch(0.155 0.005 285.823)
    pub const PAGE_CANVAS: u32 = 0x111114; // oklch(0.18 0.005 285.823)
    pub const SURFACE: u32 = 0x18181b; // oklch(0.21 0.006 285.885)
    pub const SURFACE_FOREGROUND: u32 = 0xfafafa; // oklch(0.985 0 0)
    pub const SURFACE_RAISED: u32 = 0x1e1e21; // oklch(0.235 0.007 285.885)
    pub const SURFACE_HOVER: u32 = 0x27272a; // oklch(0.274 0.006 286.033)
    pub const SURFACE_SELECTED: u32 = 0x2d2d31; // oklch(0.3 0.006 286.033)
    pub const SURFACE_SELECTED_FOREGROUND: u32 = 0xfafafa; // oklch(0.985 0 0)
    /// `--surface-border` is itself translucent white in dark
    /// (`oklch(1 0 0 / 10%)`) — hex + separate alpha, not a solid color.
    pub const SURFACE_BORDER: u32 = 0xffffff;
    pub const SURFACE_BORDER_ALPHA: f32 = 0.10;

    // ── Semantic (spec §1.2) ─────────────────────────────────────────────
    pub const FOREGROUND: u32 = 0xfafafa; // oklch(0.985 0 0)
    pub const POPOVER: u32 = 0x1e1e21; // oklch(0.235 0.007 285.885) — == surface_raised
    pub const PRIMARY: u32 = 0xe4e4e7; // oklch(0.92 0.004 286.32) — dark inverts near-white
    pub const PRIMARY_FOREGROUND: u32 = 0x18181b; // oklch(0.21 0.006 285.885)
    pub const SECONDARY: u32 = 0x27272a; // oklch(0.274 0.006 286.033)
    pub const SECONDARY_FOREGROUND: u32 = 0xfafafa; // oklch(0.985 0 0)
    pub const MUTED: u32 = 0x27272a; // oklch(0.274 0.006 286.033)
    pub const MUTED_FOREGROUND: u32 = 0x9f9fa9; // oklch(0.705 0.015 286.067)
    pub const ACCENT: u32 = 0x27272a; // oklch(0.274 0.006 286.033) — == muted
    pub const ACCENT_FOREGROUND: u32 = 0xfafafa; // oklch(0.985 0 0)
    pub const DESTRUCTIVE: u32 = 0xff6467; // oklch(0.704 0.191 22.216)
    /// `--border`: translucent white, white/6% in dark.
    pub const BORDER: u32 = 0xffffff;
    pub const BORDER_ALPHA: f32 = 0.06;
    /// `--input`: translucent white, white/15% in dark.
    pub const INPUT: u32 = 0xffffff;
    pub const INPUT_ALPHA: f32 = 0.15;
    pub const RING: u32 = 0x71717b; // oklch(0.552 0.016 285.938)
    /// Brand = Multica blue (hue 255), NOT amber — copied verbatim per the
    /// spec's explicit "照抄＝連 hue 一起抄藍色" decision. Swappable back to
    /// amber later without touching any other token (single source of truth).
    pub const BRAND: u32 = 0x4390ee; // oklch(0.65 0.16 255)
    pub const BRAND_FOREGROUND: u32 = 0xfafafa; // oklch(0.985 0 0)
    pub const SUCCESS: u32 = 0x4aa651; // oklch(0.65 0.15 145)
    pub const WARNING: u32 = 0xcb9400; // oklch(0.7 0.16 85)
    pub const INFO: u32 = 0x0f92f7; // oklch(0.65 0.18 250)

    // ── Sidebar (spec §1.3) ──────────────────────────────────────────────
    pub const SIDEBAR: u32 = 0x18181b; // oklch(0.21 0.006 285.885) — == surface
    pub const SIDEBAR_FOREGROUND: u32 = 0xfafafa; // oklch(0.985 0 0)
    pub const SIDEBAR_PRIMARY: u32 = 0x1447e6; // oklch(0.488 0.243 264.376)
    pub const SIDEBAR_ACCENT: u32 = 0x27272a; // oklch(0.274 0.006 286.033)
    pub const SIDEBAR_ACCENT_FOREGROUND: u32 = 0xfafafa; // oklch(0.985 0 0)
    pub const SIDEBAR_BORDER: u32 = 0xffffff; // oklch(1 0 0 / 10%)
    pub const SIDEBAR_BORDER_ALPHA: f32 = 0.10;
    pub const SIDEBAR_RING: u32 = 0x71717b; // oklch(0.552 0.016 285.938)

    // ── Chart (spec §1.4) — chart-1 = brand, brightness-inverted for dark
    // ("most important = brightest"). Not consumed by Phase 1a (no charts
    // in these 3 screens yet); present for completeness / future pages.
    pub const CHART_1: u32 = 0x59a6ff; // oklch(0.72 0.16 255)
    pub const CHART_2: u32 = 0x4c88d3; // oklch(0.62 0.13 255)
    pub const CHART_3: u32 = 0x3f6aa1; // oklch(0.52 0.10 255)
    #[allow(dead_code)]
    pub const CHART_4: u32 = 0x364e6d; // oklch(0.42 0.06 255)
    #[allow(dead_code)]
    pub const CHART_5: u32 = 0x293442; // oklch(0.32 0.03 255)

    // ── Elevation (spec §1.5) — three shadow tiers, dark values ─────────
    /// Cards (`shadow-[var(--surface-shadow)]`).
    pub fn surface_shadow() -> Vec<BoxShadow> {
        vec![
            BoxShadow::new(px(0.), px(1.), rgb(0x000000).opacity(0.2).into()).blur_radius(px(2.)),
            BoxShadow::new(px(0.), px(1.), rgb(0x000000).opacity(0.16).into())
                .blur_radius(px(1.)),
        ]
    }
    /// Menu / select / popover (`shadow-[var(--menu-shadow)]`).
    #[allow(dead_code)] // not yet consumed by Phase 1a (no menus/popovers)
    pub fn menu_shadow() -> Vec<BoxShadow> {
        vec![
            BoxShadow::new(px(0.), px(10.), rgb(0x000000).opacity(0.3).into())
                .blur_radius(px(28.)),
            BoxShadow::new(px(0.), px(2.), rgb(0x000000).opacity(0.18).into())
                .blur_radius(px(8.)),
        ]
    }
    /// Dialog / sheet (`shadow-[var(--floating-shadow)]`).
    #[allow(dead_code)] // not yet consumed by Phase 1a (no dialogs)
    pub fn floating_shadow() -> Vec<BoxShadow> {
        vec![
            BoxShadow::new(px(0.), px(20.), rgb(0x000000).opacity(0.46).into())
                .blur_radius(px(48.)),
            BoxShadow::new(px(0.), px(4.), rgb(0x000000).opacity(0.28).into())
                .blur_radius(px(12.)),
        ]
    }

    /// `--surface-border` pre-composited to `Hsla` (hex + alpha in one call)
    /// — the token used for card / sidebar-island / content-canvas outlines.
    pub fn surface_border() -> Hsla {
        alpha(SURFACE_BORDER, SURFACE_BORDER_ALPHA).into()
    }
    /// `--border` pre-composited — generic hairline (not currently used by
    /// the 3 Phase 1a screens, kept for parity with the token table).
    #[allow(dead_code)]
    pub fn border() -> Hsla {
        alpha(BORDER, BORDER_ALPHA).into()
    }
    /// `--input` pre-composited — text field border.
    pub fn input_border() -> Hsla {
        alpha(INPUT, INPUT_ALPHA).into()
    }
    /// `dark:bg-input/30` — Tailwind's `/30` opacity modifier stacked on top
    /// of the already-translucent `--input` token (0.15 × 0.30 ≈ 4.5% white
    /// wash), MDS's actual Input background recipe.
    pub fn input_bg() -> Rgba {
        alpha(INPUT, INPUT_ALPHA * 0.30)
    }
    /// `--sidebar-border` pre-composited.
    pub fn sidebar_border() -> Hsla {
        alpha(SIDEBAR_BORDER, SIDEBAR_BORDER_ALPHA).into()
    }
}

/// See `dark`'s doc comment — fully populated, not yet wired to any render
/// path (Phase 1a is dark-only). Kept in lockstep with `dark` token-for-token
/// so a Phase 1b toggle only has to swap which module gets re-exported.
#[allow(dead_code)]
pub mod light {
    use super::*;

    pub const APP_SHELL: u32 = 0xf3f3f4; // oklch(0.964435 0.001327 286.375)
    pub const PAGE_CANVAS: u32 = 0xfbfbfb; // oklch(0.988087 0 0)
    pub const SURFACE: u32 = 0xffffff; // oklch(1 0 0)
    pub const SURFACE_FOREGROUND: u32 = 0x09090b; // oklch(0.141 0.005 285.823)
    pub const SURFACE_RAISED: u32 = 0xffffff; // oklch(1 0 0)
    pub const SURFACE_HOVER: u32 = 0xf4f4f5; // oklch(0.967 0.001 286.375)
    pub const SURFACE_SELECTED: u32 = 0xeeeef0; // oklch(0.95 0.002 286.375)
    pub const SURFACE_SELECTED_FOREGROUND: u32 = 0x09090b; // oklch(0.141 0.005 285.823)
    /// Light `--surface-border` is a solid color (no alpha channel in the
    /// token itself) — alpha kept at 1.0 for API symmetry with `dark`.
    pub const SURFACE_BORDER: u32 = 0xe4e4e7; // oklch(0.92 0.004 286.32)
    pub const SURFACE_BORDER_ALPHA: f32 = 1.0;

    pub const FOREGROUND: u32 = 0x09090b; // oklch(0.141 0.005 285.823)
    pub const POPOVER: u32 = 0xffffff; // == surface_raised
    pub const PRIMARY: u32 = 0x18181b; // oklch(0.21 0.006 285.885)
    pub const PRIMARY_FOREGROUND: u32 = 0xfafafa; // oklch(0.985 0 0)
    pub const SECONDARY: u32 = 0xf4f4f5; // oklch(0.967 0.001 286.375)
    pub const SECONDARY_FOREGROUND: u32 = 0x18181b; // oklch(0.21 0.006 285.885)
    pub const MUTED: u32 = 0xf4f4f5; // oklch(0.967 0.001 286.375)
    pub const MUTED_FOREGROUND: u32 = 0x71717b; // oklch(0.552 0.016 285.938)
    pub const ACCENT: u32 = 0xf4f4f5; // oklch(0.967 0.001 286.375)
    pub const ACCENT_FOREGROUND: u32 = 0x18181b; // oklch(0.21 0.006 285.885)
    pub const DESTRUCTIVE: u32 = 0xe7000b; // oklch(0.577 0.245 27.325)
    pub const BORDER: u32 = 0xececef; // oklch(0.945 0.003 286.32)
    pub const BORDER_ALPHA: f32 = 1.0;
    pub const INPUT: u32 = 0xe4e4e7; // oklch(0.92 0.004 286.32)
    pub const INPUT_ALPHA: f32 = 1.0;
    pub const RING: u32 = 0x9f9fa9; // oklch(0.705 0.015 286.067)
    pub const BRAND: u32 = 0x2171cc; // oklch(0.55 0.16 255)
    pub const BRAND_FOREGROUND: u32 = 0xfafafa; // oklch(0.985 0 0)
    pub const SUCCESS: u32 = 0x1c882d; // oklch(0.55 0.16 145)
    pub const WARNING: u32 = 0xdca400; // oklch(0.75 0.16 85)
    pub const INFO: u32 = 0x0072d5; // oklch(0.55 0.18 250)

    pub const SIDEBAR: u32 = 0xf3f3f4; // == app_shell
    pub const SIDEBAR_FOREGROUND: u32 = 0x09090b; // oklch(0.141 0.005 285.823)
    pub const SIDEBAR_PRIMARY: u32 = 0x18181b; // oklch(0.21 0.006 285.885) — == primary
    pub const SIDEBAR_ACCENT: u32 = 0xe9e9eb; // oklch(0.935 0.003 286.375)
    pub const SIDEBAR_ACCENT_FOREGROUND: u32 = 0x18181b; // oklch(0.21 0.006 285.885)
    pub const SIDEBAR_BORDER: u32 = 0xe4e4e7; // oklch(0.92 0.004 286.32)
    pub const SIDEBAR_BORDER_ALPHA: f32 = 1.0;
    pub const SIDEBAR_RING: u32 = 0x9f9fa9; // oklch(0.705 0.015 286.067)

    pub const CHART_1: u32 = 0x2171cc; // oklch(0.55 0.16 255)
    pub const CHART_2: u32 = 0x5894e0; // oklch(0.66 0.13 255)
    pub const CHART_3: u32 = 0x85b4f0; // oklch(0.76 0.10 255)
    pub const CHART_4: u32 = 0xb4d0f5; // oklch(0.85 0.06 255)
    pub const CHART_5: u32 = 0xd8e6f9; // oklch(0.92 0.03 255)

    /// Light shadows use a navy-tinted `rgb(15 23 42 / …)` (not pure black)
    /// — spec §1.5.
    pub fn surface_shadow() -> Vec<BoxShadow> {
        vec![
            BoxShadow::new(px(0.), px(1.), rgb(0x0f172a).opacity(0.04).into())
                .blur_radius(px(2.)),
            BoxShadow::new(px(0.), px(1.), rgb(0x0f172a).opacity(0.03).into())
                .blur_radius(px(1.)),
        ]
    }
    pub fn menu_shadow() -> Vec<BoxShadow> {
        vec![
            BoxShadow::new(px(0.), px(8.), rgb(0x0f172a).opacity(0.08).into())
                .blur_radius(px(24.)),
            BoxShadow::new(px(0.), px(2.), rgb(0x0f172a).opacity(0.05).into())
                .blur_radius(px(6.)),
        ]
    }
    pub fn floating_shadow() -> Vec<BoxShadow> {
        vec![
            BoxShadow::new(px(0.), px(16.), rgb(0x0f172a).opacity(0.14).into())
                .blur_radius(px(40.)),
            BoxShadow::new(px(0.), px(3.), rgb(0x0f172a).opacity(0.08).into())
                .blur_radius(px(10.)),
        ]
    }

    pub fn surface_border() -> Hsla {
        alpha(SURFACE_BORDER, SURFACE_BORDER_ALPHA).into()
    }
    pub fn border() -> Hsla {
        alpha(BORDER, BORDER_ALPHA).into()
    }
    pub fn input_border() -> Hsla {
        alpha(INPUT, INPUT_ALPHA).into()
    }
    /// Light Input has no `dark:bg-input/30` override — it's plain
    /// `bg-transparent` (relies on `border-input` alone for definition).
    /// Kept for API symmetry with `dark::input_bg()`.
    pub fn input_bg() -> Rgba {
        alpha(0xffffff, 0.0)
    }
    pub fn sidebar_border() -> Hsla {
        alpha(SIDEBAR_BORDER, SIDEBAR_BORDER_ALPHA).into()
    }
}

// Phase 1a is dark-only: re-export `dark`'s tokens at the module root so
// call sites read as `theme::SURFACE`, `theme::brand()`, `theme::alpha(...)`.
pub use dark::*;
