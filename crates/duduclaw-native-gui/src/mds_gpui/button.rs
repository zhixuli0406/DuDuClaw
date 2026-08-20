// Button — MDS primitive (spec §4 Button; source of truth
// `web/src/components/mds/button.tsx`).
//
// Facade note: see `mds_gpui/mod.rs` for why this is hand-rolled `div()`
// composition instead of wrapping `gpui-component` (D4 spike outcome).
//
// Scope: only the 4 variants the S3 task brief names (`primary=BRAND /
// secondary / ghost / destructive`) — the web version's `outline` /
// `brandSubtle` / `link` variants and its `xs`/`sm`/`lg`/`icon*` size tiers
// are NOT ported here. Fixed height 32px (`h_8`) / `RADIUS_LG` for every
// variant, matching the brief's "高度 32px=h-8、RADIUS_LG" instruction
// literally (the web spec's per-size `h-9`/`h-6` tiers are out of scope).
//
// Real keyboard focus-visible ring: gpui's `.focus_visible()` pattern
// (`crates/gpui/examples/focus_visible.rs`) requires a `FocusHandle` that
// survives across renders via `.track_focus(&handle)` — a handle minted
// fresh on every call here would never accumulate real tab-focus. Callers
// that own persistent per-button state (a future submit button backed by an
// `Entity`, the way `text_field.rs`'s `TextField` owns its own handle) can
// pass `Some(&handle)` for a REAL ring. `screens/gallery.rs` has no such
// persistent state for its dogfood grid and passes `None` — its focus-ring
// swatch is a static label, not a live-tested ring; called out explicitly
// in this crate's S3 report as an honest stub, not silently skipped.

use gpui::{
    div, prelude::*, px, App, ClickEvent, CursorStyle, Div, ElementId, FocusHandle, Rgba,
    SharedString, Stateful, Window,
};

use crate::theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonVariant {
    Primary,
    Secondary,
    Ghost,
    Destructive,
}

struct Palette {
    bg: Rgba,
    bg_hover: Rgba,
    bg_active: Rgba,
    text: Rgba,
}

fn palette(variant: ButtonVariant) -> Palette {
    match variant {
        ButtonVariant::Primary => Palette {
            bg: theme::alpha(theme::BRAND, 1.0),
            bg_hover: theme::alpha(theme::BRAND, 0.90),
            bg_active: theme::alpha(theme::BRAND, 0.85),
            text: theme::alpha(theme::BRAND_FOREGROUND, 1.0),
        },
        // `hover:bg-secondary/80` in the web recipe — `SECONDARY` is itself
        // an opaque solid token, so the `/NN` opacity modifier is
        // approximated the same way `theme.rs`'s module doc comment
        // describes for every other alpha-wash token.
        ButtonVariant::Secondary => Palette {
            bg: theme::alpha(theme::SECONDARY, 1.0),
            bg_hover: theme::alpha(theme::SECONDARY, 0.80),
            bg_active: theme::alpha(theme::SECONDARY, 0.70),
            text: theme::alpha(theme::SECONDARY_FOREGROUND, 1.0),
        },
        // `hover:bg-muted dark:hover:bg-muted/50` — transparent at rest.
        ButtonVariant::Ghost => Palette {
            bg: theme::alpha(theme::SURFACE, 0.0),
            bg_hover: theme::alpha(theme::MUTED, 0.5),
            bg_active: theme::alpha(theme::MUTED, 0.65),
            text: theme::alpha(theme::FOREGROUND, 1.0),
        },
        // `bg-destructive/10 text-destructive hover:bg-destructive/20` —
        // exact web recipe, `active` is this facade's own extrapolation
        // (the web spec has no explicit active state for this variant).
        ButtonVariant::Destructive => Palette {
            bg: theme::alpha(theme::DESTRUCTIVE, 0.10),
            bg_hover: theme::alpha(theme::DESTRUCTIVE, 0.20),
            bg_active: theme::alpha(theme::DESTRUCTIVE, 0.28),
            text: theme::alpha(theme::DESTRUCTIVE, 1.0),
        },
    }
}

/// `disabled` renders a dimmed, non-interactive button — no hover/active
/// styles and NO click handler attached at all (matching `screens/
/// login.rs`'s existing disabled-submit-button pattern, not just a visual
/// dim over a still-live handler).
#[allow(clippy::too_many_arguments)]
pub fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    variant: ButtonVariant,
    disabled: bool,
    focus_handle: Option<&FocusHandle>,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let Palette { bg, bg_hover, bg_active, text } = palette(variant);

    let mut el = div()
        .id(id.into())
        .h(px(32.))
        .px_2p5()
        .flex()
        .items_center()
        .justify_center()
        .gap_1p5()
        .rounded(px(theme::RADIUS_LG))
        .bg(if disabled { theme::alpha(theme::MUTED, 0.4) } else { bg })
        .text_color(if disabled {
            theme::alpha(theme::MUTED_FOREGROUND, 1.0)
        } else {
            text
        })
        .text_size(px(theme::TEXT_SM))
        .font_weight(gpui::FontWeight::MEDIUM)
        .child(label.into());

    if let Some(handle) = focus_handle {
        el = el.track_focus(handle).focus_visible(|style| {
            style.border_2().border_color(theme::alpha(theme::RING, 1.0))
        });
    }

    if disabled {
        el.cursor(CursorStyle::OperationNotAllowed)
    } else {
        el.cursor_pointer()
            .hover(move |style| style.bg(bg_hover))
            .active(move |style| style.bg(bg_active))
            .on_click(on_click)
    }
}
