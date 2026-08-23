// Linux-only: turns this module's pure `LayerParams` into a real
// `gpui::WindowOptions` carrying a `WindowKind::LayerShell(_)` — see
// `super`'s (this module's parent, `chrome/mod.rs`) header comment for why
// this conversion is isolated in its own `#[cfg(target_os = "linux")]` file
// rather than living inline in `params.rs`.

use gpui::layer_shell::{Anchor, KeyboardInteractivity, Layer, LayerShellOptions};
use gpui::{point, px, size, Bounds, WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions};

use super::{ChromeAnchor, ChromeKeyboardInteractivity, ChromeLayer, LayerParams};

fn to_layer(layer: ChromeLayer) -> Layer {
    match layer {
        ChromeLayer::Background => Layer::Background,
        ChromeLayer::Bottom => Layer::Bottom,
        ChromeLayer::Top => Layer::Top,
        ChromeLayer::Overlay => Layer::Overlay,
    }
}

fn to_anchor(anchor: ChromeAnchor) -> Anchor {
    let mut out = Anchor::empty();
    if anchor.top {
        out |= Anchor::TOP;
    }
    if anchor.bottom {
        out |= Anchor::BOTTOM;
    }
    if anchor.left {
        out |= Anchor::LEFT;
    }
    if anchor.right {
        out |= Anchor::RIGHT;
    }
    out
}

fn to_keyboard_interactivity(k: ChromeKeyboardInteractivity) -> KeyboardInteractivity {
    match k {
        ChromeKeyboardInteractivity::None => KeyboardInteractivity::None,
        ChromeKeyboardInteractivity::Exclusive => KeyboardInteractivity::Exclusive,
        ChromeKeyboardInteractivity::OnDemand => KeyboardInteractivity::OnDemand,
    }
}

/// `width` is the placeholder passed to `WindowOptions::window_bounds` —
/// see `LayerParams::height`'s own doc comment on why this only matters
/// before the compositor's first `configure` event for any surface that
/// anchors both `left` and `right` (every surface this crate opens does).
///
/// No `exclusive_edge` is ever set (`LayerShellOptions::exclusive_edge`
/// stays `None`): per the wlr-layer-shell protocol, the exclusive edge only
/// needs disambiguating for a CORNER-anchored surface, and every surface
/// this crate opens anchors either one edge plus both perpendicular edges
/// (menu bar: top+left+right; dock: bottom+left+right) or all four — never
/// a bare corner — so the edge is unambiguous from `anchor` alone. No
/// `margin` is ever set either (`None`) — this crate's chrome sits flush
/// against its anchored edges, matching the pre-migration hardcoded bands
/// (`home.rs::menu_bar`'s `top(px(0.))`, `home_dock.rs::dock`'s bottom
/// offset being CONTENT padding inside its own band, not a layer-shell
/// margin).
pub(crate) fn to_window_options(params: &LayerParams, width: f32) -> WindowOptions {
    let options = LayerShellOptions {
        namespace: params.namespace.to_string(),
        layer: to_layer(params.layer),
        anchor: to_anchor(params.anchor),
        exclusive_zone: params.exclusive_zone.map(px),
        exclusive_edge: None,
        margin: None,
        keyboard_interactivity: to_keyboard_interactivity(params.keyboard_interactivity),
    };

    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds { origin: point(px(0.), px(0.)), size: size(px(width), px(params.height)) })),
        // No system titlebar for a layer-shell surface — matches gpui's own
        // `examples/layer_shell.rs`.
        titlebar: None,
        app_id: Some(super::SHELL_APP_ID.to_string()),
        // Every layer surface this crate opens paints only PART of its own
        // window bounds (the menu bar/dock draw a fixed-height bar; the
        // desktop/overlay draw content that may not cover every pixel at
        // every corner radius). `Transparent` — matching gpui's own
        // `examples/layer_shell.rs` — lets whatever this surface does not
        // explicitly paint show through instead of a solid platform-default
        // fill.
        window_background: WindowBackgroundAppearance::Transparent,
        kind: WindowKind::LayerShell(options),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chrome::{layer_params_for, ChromeSurface};

    /// Not a live-window test (no compositor in a headless `cargo test`
    /// run) — just confirms this conversion doesn't panic and produces the
    /// right `WindowKind`/`app_id` for every surface this crate opens. Real
    /// on-screen verification is `BUILD-LINUX.md`'s job (weston/VM rounds).
    #[test]
    fn every_chrome_surface_converts_to_a_layer_shell_window_kind() {
        for surface in [
            ChromeSurface::MenuBar,
            ChromeSurface::Dock,
            ChromeSurface::Home,
            ChromeSurface::Overlay(crate::surface::Overlay::Launcher),
            ChromeSurface::Overlay(crate::surface::Overlay::Settings),
        ] {
            let params = layer_params_for(surface);
            let options = to_window_options(&params, super::super::DESKTOP_WIDTH);
            assert!(matches!(options.kind, WindowKind::LayerShell(_)), "{surface:?} must open as a layer-shell window");
            assert_eq!(options.app_id.as_deref(), Some(super::super::SHELL_APP_ID));
            assert!(options.titlebar.is_none());
            assert_eq!(options.window_background, WindowBackgroundAppearance::Transparent);
        }
    }
}
