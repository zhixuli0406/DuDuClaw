// OOBE theme palette — Shell-S1 (2026-08-20, "外觀" step).
//
// `OobeFlow::palette()` (`oobe/mod.rs`) resolves one of these from
// `selections().theme`. Every hardcoded `theme::light::*` reference that
// used to live across `widgets.rs`/`render.rs`/`steps/*.rs` is replaced by
// a field or method on the value this module produces — the whole point
// being that swapping `ThemeChoice::Light` → `Dark` and re-rendering picks
// up a DIFFERENT `OobePalette` on the very next frame, with no separate
// "apply theme" step (see `OobeStep::Theme`'s own doc comment).
//
// This is a sibling module of `render`/`steps`/`widgets`, not folded into
// `oobe/mod.rs` itself, specifically so it's free to depend on gpui types
// (`Rgba`/`Hsla`/`BoxShadow`, and the `gpui::Global` impl below) without
// touching `mod.rs`'s own declared "no gpui types anywhere in THIS file"
// discipline — see that file's header comment and `OobeFlow::palette()`'s
// own doc comment for how the two files stay decoupled even though
// `palette()` lives on `OobeFlow`.
//
// ── Field shape ───────────────────────────────────────────────────────
// Most fields are plain `u32` hex, mirroring `theme::light::FOO` /
// `theme::dark::FOO` constants 1:1 — every OOBE call site already applied
// these through `theme::alpha(FOO, 1.0)` (a flat, always-opaque factor), and
// that stays true for every ONE of these specific tokens in BOTH themes (the
// dark-module counterparts of app_shell/surface/surface_hover/secondary/
// secondary_foreground/muted/muted_foreground/foreground/brand/
// brand_foreground/ring/success/destructive are all solid colors too — no
// `_ALPHA` companion const on the dark side either, so a flat 1.0 stays
// correct for dark exactly as it always was for light).
//
// `surface_border` is the ONE color field that is NOT plain `u32` — it is
// precomposited to `Rgba` (hex + alpha baked in together) instead. This is a
// deliberate CORRECTNESS fix, not a style choice: dark's `--surface-border`
// token is translucent white (`SURFACE_BORDER_ALPHA = 0.10`), unlike
// light's fully-opaque solid (`SURFACE_BORDER_ALPHA = 1.0`) — but every OOBE
// call site that used this token applied a LITERAL `1.0` factor
// (`theme::alpha(theme::light::SURFACE_BORDER, 1.0)`), not the token's own
// `_ALPHA` constant. That literal `1.0` was only ever correct because OOBE
// was light-only before this round; naively porting the same literal-1.0
// pattern to dark would paint every "inactive" progress dot / toggle track /
// unselected-card border as solid opaque white instead of a subtle
// translucent hairline. Baking the CORRECT per-theme alpha into this one
// field at construction time (see `light()`/`dark()` below) fixes that
// silently, without needing every call site to remember a second `_ALPHA`
// lookup.
//
// ── Derived methods (`border`/`input_bg`/`input_border`/`surface_shadow`) ──
// These four are METHODS, not fields, because their light/dark FORMULAS
// genuinely diverge — not just their inputs. `theme::light::input_bg()` is a
// hardcoded fully-transparent override, unrelated to the `INPUT`/
// `INPUT_ALPHA` tokens at all (`theme.rs`'s own doc comment: "Light Input has
// no `dark:bg-input/30` override — it's plain `bg-transparent`"), while
// `theme::dark::input_bg()` DOES derive from those tokens (`INPUT_ALPHA *
// 0.30`). Reimplementing that divergence here from raw stored fields would
// mean re-deriving (and re-auditing) formulas that `theme.rs` already ships,
// audited, and comments extensively (see that file's own "OKLCH → sRGB
// methodology" header) — dispatching straight to `theme::light::*` /
// `theme::dark::*` by the stored `dark` flag is strictly safer: zero risk of
// this module's copy silently drifting from the source of truth.
//
// ── `Global` — why `OobeTextField` needs it ──────────────────────────────
// Every OOBE screen except one reads its palette as a plain parameter,
// resolved fresh at the top of that screen's own `render` fn from
// `flow.palette()` — the same shape `flow.locale()` already has. The one
// exception is `widgets::OobeTextField`: it's a real `gpui::Entity` with its
// OWN independent `Render` impl (`widgets.rs`'s own header comment explains
// why — a re-derivation of `duduclaw-native-gui`'s text-field pattern, not a
// stateless helper), created once at window-open time and then held/reused
// across every visit to the `AccountCreate` step. Its `render(&mut self, ..)`
// signature has no room for an extra caller-supplied parameter, so it needs
// AMBIENT access to "what theme is active right now" instead of a threaded
// one — `gpui::Global` is exactly gpui's own answer to that (Zed's own
// `Theme` type is registered the identical way, for the identical reason).
// `oobe::render::render` (the OOBE frame's top-level fn) calls
// `cx.set_global(palette)` once per render pass, BEFORE the tree containing
// `AccountCreate`'s embedded text fields is built — by the time
// `OobeTextField::render` runs later in that SAME pass, the global is
// already current. `OobeTextField::render` reads it via `try_global()` +
// `unwrap_or_default()` (never the panicking `global()`) — fail-open to the
// light palette on the (theoretical, since `oobe::render` always sets it
// first) chance nothing has set the global yet, same "never panic on a gap,
// degrade instead" discipline this crate's persistence layer already
// documents (`load_state`'s own doc comment).

use duduclaw_native_gui::theme;
use gpui::{BoxShadow, Global, Hsla, Rgba};

use super::ThemeChoice;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct OobePalette {
    dark: bool,

    pub(super) app_shell: u32,
    pub(super) surface: u32,
    pub(super) surface_hover: u32,
    /// Precomposited — see this module's header comment for why this one
    /// field (unlike every other color field here) is `Rgba`, not `u32`.
    pub(super) surface_border: Rgba,
    pub(super) secondary: u32,
    pub(super) secondary_foreground: u32,
    pub(super) muted: u32,
    pub(super) muted_foreground: u32,
    pub(super) foreground: u32,
    pub(super) brand: u32,
    pub(super) brand_foreground: u32,
    pub(super) ring: u32,
    pub(super) success: u32,
    pub(super) destructive: u32,
}

impl OobePalette {
    pub(super) fn light() -> Self {
        Self {
            dark: false,
            app_shell: theme::light::APP_SHELL,
            surface: theme::light::SURFACE,
            surface_hover: theme::light::SURFACE_HOVER,
            surface_border: theme::alpha(theme::light::SURFACE_BORDER, theme::light::SURFACE_BORDER_ALPHA),
            secondary: theme::light::SECONDARY,
            secondary_foreground: theme::light::SECONDARY_FOREGROUND,
            muted: theme::light::MUTED,
            muted_foreground: theme::light::MUTED_FOREGROUND,
            foreground: theme::light::FOREGROUND,
            brand: theme::light::BRAND,
            brand_foreground: theme::light::BRAND_FOREGROUND,
            ring: theme::light::RING,
            success: theme::light::SUCCESS,
            destructive: theme::light::DESTRUCTIVE,
        }
    }

    pub(super) fn dark() -> Self {
        Self {
            dark: true,
            app_shell: theme::dark::APP_SHELL,
            surface: theme::dark::SURFACE,
            surface_hover: theme::dark::SURFACE_HOVER,
            surface_border: theme::alpha(theme::dark::SURFACE_BORDER, theme::dark::SURFACE_BORDER_ALPHA),
            secondary: theme::dark::SECONDARY,
            secondary_foreground: theme::dark::SECONDARY_FOREGROUND,
            muted: theme::dark::MUTED,
            muted_foreground: theme::dark::MUTED_FOREGROUND,
            foreground: theme::dark::FOREGROUND,
            brand: theme::dark::BRAND,
            brand_foreground: theme::dark::BRAND_FOREGROUND,
            ring: theme::dark::RING,
            success: theme::dark::SUCCESS,
            destructive: theme::dark::DESTRUCTIVE,
        }
    }

    pub(super) fn for_choice(choice: ThemeChoice) -> Self {
        match choice {
            ThemeChoice::Light => Self::light(),
            ThemeChoice::Dark => Self::dark(),
        }
    }

    /// Generic hairline border (`--border`) — see this module's header
    /// comment for why this is a method, not a stored field.
    pub(super) fn border(&self) -> Hsla {
        if self.dark {
            theme::dark::border()
        } else {
            theme::light::border()
        }
    }

    /// Text-field background (`--input`, composited; light overrides to
    /// fully transparent) — see this module's header comment.
    pub(super) fn input_bg(&self) -> Rgba {
        if self.dark {
            theme::dark::input_bg()
        } else {
            theme::light::input_bg()
        }
    }

    /// Text-field border (`--input`, composited at its own declared alpha).
    pub(super) fn input_border(&self) -> Hsla {
        if self.dark {
            theme::dark::input_border()
        } else {
            theme::light::input_border()
        }
    }

    /// Card elevation shadow — see this module's header comment for why
    /// this dispatches to `theme.rs`'s own OKLCH-derived tint rather than
    /// re-deriving it here.
    pub(super) fn surface_shadow(&self) -> Vec<BoxShadow> {
        if self.dark {
            theme::dark::surface_shadow()
        } else {
            theme::light::surface_shadow()
        }
    }
}

impl Default for OobePalette {
    fn default() -> Self {
        Self::light()
    }
}

impl Global for OobePalette {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn for_choice_resolves_to_the_matching_constructor() {
        assert_eq!(OobePalette::for_choice(ThemeChoice::Light), OobePalette::light());
        assert_eq!(OobePalette::for_choice(ThemeChoice::Dark), OobePalette::dark());
    }

    #[test]
    fn default_is_light() {
        assert_eq!(OobePalette::default(), OobePalette::light());
    }

    #[test]
    fn light_and_dark_disagree_on_every_plain_color_field() {
        // Not a tautology check on `dark` alone — this pins that the MDS
        // dark tokens were actually threaded through (a copy-paste bug that
        // left every field pointing at `theme::light::*` in `dark()` too
        // would still produce a technically-valid `OobePalette`, just a
        // wrong one) by asserting every plain-`u32` field differs between
        // the two constructors, matching `theme.rs`'s own light/dark token
        // table (every one of these pairs is a genuinely different hex
        // there).
        let light = OobePalette::light();
        let dark = OobePalette::dark();
        assert_ne!(light.app_shell, dark.app_shell);
        assert_ne!(light.surface, dark.surface);
        assert_ne!(light.surface_hover, dark.surface_hover);
        assert_ne!(light.secondary, dark.secondary);
        assert_ne!(light.muted, dark.muted);
        assert_ne!(light.muted_foreground, dark.muted_foreground);
        assert_ne!(light.foreground, dark.foreground);
        assert_ne!(light.brand, dark.brand);
        assert_ne!(light.ring, dark.ring);
        assert_ne!(light.success, dark.success);
        assert_ne!(light.destructive, dark.destructive);
        // `secondary_foreground` is a near-black/near-white FLIP between the
        // two themes (light picks dark text on its pale secondary surface,
        // dark picks light text on its own dark one), so it's asserted too
        // rather than skipped.
        assert_ne!(light.secondary_foreground, dark.secondary_foreground);
        // `brand_foreground`, in contrast, is `0xfafafa` (near-white) in
        // BOTH themes — see `theme.rs`'s own token table — because `BRAND`
        // itself is a mid-tone blue in both (`0x2171cc` light / `0x4390ee`
        // dark, differing per `assert_ne!(light.brand, dark.brand)` above),
        // so white text reads correctly against either. Asserted equal
        // here, not skipped, so this is a documented fact rather than an
        // untested assumption.
        assert_eq!(light.brand_foreground, dark.brand_foreground);
    }

    #[test]
    fn dark_surface_border_carries_its_real_translucent_alpha_not_a_flat_one() {
        // The correctness fix this module's header comment documents at
        // length: dark's composited `surface_border` must actually be
        // translucent (`SURFACE_BORDER_ALPHA = 0.10`), not opaque white.
        let dark = OobePalette::dark();
        assert_eq!(dark.surface_border.a, theme::dark::SURFACE_BORDER_ALPHA);
        assert!(dark.surface_border.a < 1.0, "dark surface_border must not be flattened to opaque");
    }

    #[test]
    fn light_surface_border_is_fully_opaque() {
        // Light's own `SURFACE_BORDER_ALPHA` is already 1.0 — this pins
        // that composited value stays 1.0 too (the old call sites' literal
        // `1.0` factor was only ever a coincidence of this fact).
        let light = OobePalette::light();
        assert_eq!(light.surface_border.a, 1.0);
    }

    #[test]
    fn border_input_and_shadow_dispatch_to_the_matching_theme_module() {
        let light = OobePalette::light();
        let dark = OobePalette::dark();
        assert_eq!(light.border(), theme::light::border());
        assert_eq!(dark.border(), theme::dark::border());
        assert_eq!(light.input_border(), theme::light::input_border());
        assert_eq!(dark.input_border(), theme::dark::input_border());
        assert_ne!(light.border(), dark.border(), "light/dark hairline border must actually differ");
        // `BoxShadow` doesn't implement `PartialEq` usefully across a `Vec`
        // boundary issue here — it does derive `PartialEq`, so a direct
        // `Vec<BoxShadow>` comparison is fine.
        assert_eq!(light.surface_shadow(), theme::light::surface_shadow());
        assert_eq!(dark.surface_shadow(), theme::dark::surface_shadow());
    }

    #[test]
    fn light_input_bg_is_transparent_dark_input_bg_is_not() {
        // See this module's header comment: light's `input_bg()` is a
        // hardcoded transparent override, unrelated to the `INPUT` token —
        // this is the one place that divergence is worth pinning directly.
        let light = OobePalette::light();
        let dark = OobePalette::dark();
        assert_eq!(light.input_bg().a, 0.0);
        assert!(dark.input_bg().a > 0.0);
    }
}
