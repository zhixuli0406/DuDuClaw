// Pure chrome-surface catalogue — see `super`'s module doc for the whole
// migration story. Nothing in this file touches `gpui` at all (deliberately
// — see that doc comment for why), so every function here is testable on
// macOS even though the feature it describes only ever RUNS on Linux.

use crate::surface::Overlay;

/// One independently-mappable piece of shell chrome. `MenuBar`/`Dock`/`Home`
/// are always-present (opened once at boot, in `LayerSurfaces` mode, and
/// kept alive for the process lifetime); `Overlay(_)` is opened on demand —
/// see `chrome::windows::SurfaceView`'s own doc comment (Linux-only file)
/// for the open/close lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChromeSurface {
    MenuBar,
    Dock,
    Home,
    Overlay(Overlay),
}

/// Mirrors `gpui::layer_shell::Layer` field-for-field. A separate type
/// (rather than re-exporting gpui's own) because `gpui::layer_shell`'s
/// TYPES compile on every platform (only the `WindowKind::LayerShell`
/// enum VARIANT is `#[cfg(target_os = "linux")]`-gated — verified by reading
/// the pinned gpui rev's own `platform.rs`/`platform/layer_shell.rs`), but
/// this module is asked to stay gpui-free regardless, so its pure functions
/// can be unit-tested without pulling gpui's `Application`/window machinery
/// into a plain `cargo test` run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChromeLayer {
    Background,
    /// Not used by any surface this shell opens today (the chrome sits on
    /// `Top`, the desktop on `Background`, panels on `Overlay`).
    ///
    /// Kept, with the allow, because this enum's job is to MODEL
    /// wlr-layer-shell's four layers — the same four `duduclaw-comp`'s own
    /// `layer_shell::geometry::LAYERS_FRONT_TO_BACK` ranks. Dropping the one
    /// we happen not to use would make the two sides' vocabularies differ
    /// and turn `gpui_bridge`'s mapping into a partial function for no gain.
    #[allow(dead_code)]
    Bottom,
    Top,
    Overlay,
}

/// Mirrors `gpui::layer_shell::Anchor` (a `bitflags` type) as four plain
/// bools — same "no gpui types here" reasoning as `ChromeLayer`. Every
/// surface below anchors BOTH `left` and `right` (stretches to the output's
/// full width), so `layer_params_for`'s `height` field is the only sizing
/// knob any of them actually needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ChromeAnchor {
    pub top: bool,
    pub bottom: bool,
    pub left: bool,
    pub right: bool,
}

impl ChromeAnchor {
    /// Pinned to the top edge, stretched left-right — the menu bar.
    pub const TOP_STRETCH: Self = Self { top: true, bottom: false, left: true, right: true };
    /// Pinned to the bottom edge, stretched left-right — the dock.
    pub const BOTTOM_STRETCH: Self = Self { top: false, bottom: true, left: true, right: true };
    /// All four edges — the desktop background and the on-demand overlay
    /// window both cover the entire output.
    pub const FILL: Self = Self { top: true, bottom: true, left: true, right: true };
}

/// Mirrors `gpui::layer_shell::KeyboardInteractivity` — same reasoning as
/// `ChromeLayer`/`ChromeAnchor` above.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChromeKeyboardInteractivity {
    /// The menu bar and dock: never focusable, never receive key events —
    /// there is nothing in either surface that needs to be typed into or
    /// receive a keyboard action.
    None,
    /// The on-demand overlay: while it is open, keyboard input should stay
    /// with it (typing into the Launcher's search box must not leak to
    /// whatever is behind it). NOTE — honest caveat, not this crate's gap:
    /// `duduclaw-comp`'s WM-3 round documents that it currently treats
    /// `Exclusive` the same as `OnDemand` (`crates/duduclaw-comp/BUILD.md`,
    /// "Known limitations" — "gets focus when clicked, does not lock
    /// keyboard away from windows"). Requesting the protocol-correct value
    /// here costs nothing today and is what comp should honour once it
    /// implements real exclusive semantics.
    Exclusive,
    /// The desktop background surface: focusable like an ordinary window
    /// (click to focus), never grabs focus on its own.
    OnDemand,
}

/// The menu bar's fixed band height, in logical pixels — matches
/// `home.rs::menu_bar`'s own `.h(px(MENU_BAR_HEIGHT))` (that call site was
/// updated to reference this constant in the same change, so the two can
/// never drift) and `duduclaw-comp`'s WM-1 `DEFAULT_RESERVED_TOP` (30 —
/// `crates/duduclaw-comp/BUILD.md`'s WM-3 section: "90 bottom / 30 top, the
/// unmigrated shell's own chrome").
pub const MENU_BAR_HEIGHT: f32 = 30.0;

/// The dock's fixed band height, in logical pixels — matches comp's WM-1
/// `DEFAULT_RESERVED_BOTTOM` (90). Unlike `MENU_BAR_HEIGHT` there is no
/// single literal in `home_dock.rs` this corresponds to (the dock's own
/// content is auto-sized flex content anchored `bottom(px(24))` within
/// whatever band contains it); this is purely the LAYER-SHELL surface's
/// own height / exclusive-zone claim, chosen to match comp's existing
/// reserved band exactly.
pub const DOCK_HEIGHT: f32 = 90.0;

/// The fixed initial width passed to `cx.open_window` for every layer
/// surface below that anchors both `left` and `right` — since anchoring
/// both edges makes the compositor override the width on the first
/// `configure` event regardless (see `chrome::windows`' own header comment,
/// which cites the exact gpui source line), this value only matters as a
/// placeholder before that first configure arrives. Matches this crate's
/// existing fixed dev-mode window size (`main.rs`'s `Bounds::centered(...,
/// size(px(1440.0), px(900.0)), cx)`).
/// Only read by the `#[cfg(target_os = "linux")]` layer-surface openers
/// (`gpui_bridge`/`windows`); `DESKTOP_HEIGHT` next to it is additionally
/// asserted by this module's own tests, which is why only this one needs the
/// allow on non-Linux hosts.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub const DESKTOP_WIDTH: f32 = 1440.0;
pub const DESKTOP_HEIGHT: f32 = 900.0;

/// Layer-shell parameters for one `ChromeSurface` — the pure description
/// `chrome::windows::gpui_bridge` (Linux-only) turns into a real
/// `gpui::layer_shell::LayerShellOptions` + `WindowOptions`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayerParams {
    pub namespace: &'static str,
    pub layer: ChromeLayer,
    pub anchor: ChromeAnchor,
    /// `None` = no exclusive-zone opinion at all (the on-demand overlay:
    /// it never reserves screen space, so other surfaces are never pushed
    /// away from it). `Some(n)` for `n >= 0.0` reserves `n` px from the
    /// anchored edge. `Some(-1.0)` is the wlr-layer-shell protocol's own
    /// escape hatch — "give me the whole output, don't shrink me for
    /// other surfaces' exclusive zones" — used ONLY by the `Home`
    /// background surface, which must always fill the entire output
    /// regardless of what the menu bar/dock/overlay are doing. Verified
    /// this negative value survives intact through gpui's own Wayland
    /// backend (not clamped to zero anywhere): `gpui_linux/src/linux/
    /// wayland/window.rs:184`, `layer_surface.set_exclusive_zone(f32::from
    /// (exclusive_zone) as i32)` — a plain `as i32` cast, which preserves
    /// sign for a value already in `i32`'s range.
    pub exclusive_zone: Option<f32>,
    pub keyboard_interactivity: ChromeKeyboardInteractivity,
    /// Fixed size on the one axis NOT already pinned by opposite anchors.
    /// Every surface below anchors both `left` and `right`, so this is
    /// always a HEIGHT: the menu bar/dock's own fixed band, or (for `Home`/
    /// `Overlay`, which anchor all four edges) an arbitrary placeholder —
    /// see `DESKTOP_HEIGHT`'s own doc comment for why the placeholder value
    /// doesn't matter there.
    pub height: f32,
}

/// The one place that knows every chrome surface's layer-shell parameters —
/// see this module's own header comment. `Overlay(_)`'s parameters are the
/// same regardless of WHICH overlay is open (Launcher/Notifications/
/// ControlCenter/PointerSettings all share one window kind/lifecycle), so
/// the match arm ignores the inner value.
pub fn layer_params_for(surface: ChromeSurface) -> LayerParams {
    match surface {
        ChromeSurface::MenuBar => LayerParams {
            namespace: "duduclaw-shell-menubar",
            layer: ChromeLayer::Top,
            anchor: ChromeAnchor::TOP_STRETCH,
            exclusive_zone: Some(MENU_BAR_HEIGHT),
            keyboard_interactivity: ChromeKeyboardInteractivity::None,
            height: MENU_BAR_HEIGHT,
        },
        ChromeSurface::Dock => LayerParams {
            namespace: "duduclaw-shell-dock",
            layer: ChromeLayer::Top,
            anchor: ChromeAnchor::BOTTOM_STRETCH,
            exclusive_zone: Some(DOCK_HEIGHT),
            keyboard_interactivity: ChromeKeyboardInteractivity::None,
            height: DOCK_HEIGHT,
        },
        ChromeSurface::Home => LayerParams {
            namespace: "duduclaw-shell-home",
            layer: ChromeLayer::Background,
            anchor: ChromeAnchor::FILL,
            exclusive_zone: Some(-1.0),
            keyboard_interactivity: ChromeKeyboardInteractivity::OnDemand,
            height: DESKTOP_HEIGHT,
        },
        ChromeSurface::Overlay(_) => LayerParams {
            namespace: "duduclaw-shell-overlay",
            layer: ChromeLayer::Overlay,
            anchor: ChromeAnchor::FILL,
            exclusive_zone: None,
            keyboard_interactivity: ChromeKeyboardInteractivity::Exclusive,
            height: DESKTOP_HEIGHT,
        },
    }
}

/// Which windowing strategy this process is actually using. `LayerSurfaces`
/// is attempted only on Linux (and only when not overridden — see
/// `desired_chrome_mode`); it can still fall back to `SingleFullscreen` at
/// runtime if the compositor doesn't advertise `zwlr_layer_shell_v1` (e.g.
/// weston's headless backend — see `chrome::windows`' own header comment).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChromeMode {
    /// Menu bar / dock / desktop / on-demand overlay are each their own
    /// wlr-layer-shell surface.
    LayerSurfaces,
    /// Everything painted inside one full-output toplevel — this crate's
    /// original Shell-S0..WM-2 behavior, byte-identical. The only mode ever
    /// used on macOS.
    SingleFullscreen,
}

/// `is_linux`/`no_layer_shell_env` are passed in rather than read directly
/// (`cfg!(target_os = "linux")` / `std::env::var(...)`) so this decision is
/// a plain, unit-testable function on every platform — same "read the env
/// once at the call site, decide here" convention `Overlay::from_debug_env`
/// already established for this crate's other env-driven boot decisions.
/// An unset or unrecognized `no_layer_shell_env` value means "no override"
/// (this crate's usual never-panic-on-a-typo contract) — only the exact
/// string `"1"` opts out of `LayerSurfaces`.
pub fn desired_chrome_mode(is_linux: bool, no_layer_shell_env: Option<&str>) -> ChromeMode {
    if no_layer_shell_env == Some("1") {
        return ChromeMode::SingleFullscreen;
    }
    if is_linux {
        ChromeMode::LayerSurfaces
    } else {
        ChromeMode::SingleFullscreen
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The WM-3 cross-crate contract, pinned to LITERAL numbers on purpose.
    ///
    /// Every other assertion in this module compares against
    /// `MENU_BAR_HEIGHT`/`DOCK_HEIGHT`, which makes them tautologies with
    /// respect to the values themselves — edit the constants and they all
    /// still pass. But these two numbers are half of an agreement with a
    /// crate that cannot be depended on from here (`duduclaw-comp`, Linux-
    /// only, detached workspace — see `comp_client.rs`'s module doc), and
    /// the other half is `duduclaw-comp/src/window_policy.rs`'s
    /// `DEFAULT_RESERVED_TOP`/`DEFAULT_RESERVED_BOTTOM`, now BOTH ZERO
    /// precisely because these exclusive zones replaced them.
    ///
    /// So: changing either number here silently un-reserves that band on the
    /// compositor side — application windows would be placed over the menu
    /// bar or the dock, which is the exact soft-lock WM-1 was created to
    /// fix. This test is the tripwire that forces the two files to be edited
    /// together, the same way `comp_client.rs` pins wire formats to literal
    /// JSON strings rather than to its own serializer.
    #[test]
    fn the_reserved_band_heights_match_the_compositors_zeroed_constants() {
        assert_eq!(
            MENU_BAR_HEIGHT, 30.0,
            "menu-bar height changed — update duduclaw-comp's window_policy.rs table in the SAME change"
        );
        assert_eq!(
            DOCK_HEIGHT, 90.0,
            "dock height changed — update duduclaw-comp's window_policy.rs table in the SAME change"
        );
        // The two bands the compositor no longer hardcodes must be exactly
        // what the two layer surfaces claim back.
        assert_eq!(layer_params_for(ChromeSurface::MenuBar).exclusive_zone, Some(30.0));
        assert_eq!(layer_params_for(ChromeSurface::Dock).exclusive_zone, Some(90.0));
    }

    /// `-1` is wlr-layer-shell's "give me the whole output and do not let
    /// anyone else's exclusive zone push me around". Pinned literally for
    /// the same reason as the test above: `0` (the tempting typo) means
    /// something entirely different — "I claim nothing but I still get
    /// pushed" — and the desktop would end up shrunk by its own chrome.
    #[test]
    fn the_desktop_surface_opts_out_of_being_pushed_by_other_exclusive_zones() {
        assert_eq!(layer_params_for(ChromeSurface::Home).exclusive_zone, Some(-1.0));
    }

    #[test]
    fn menu_bar_params_reserve_the_top_band_and_never_take_keyboard() {
        let p = layer_params_for(ChromeSurface::MenuBar);
        assert_eq!(p.namespace, "duduclaw-shell-menubar");
        assert_eq!(p.layer, ChromeLayer::Top);
        assert_eq!(p.anchor, ChromeAnchor::TOP_STRETCH);
        assert_eq!(p.exclusive_zone, Some(MENU_BAR_HEIGHT));
        assert_eq!(p.keyboard_interactivity, ChromeKeyboardInteractivity::None);
        assert_eq!(p.height, MENU_BAR_HEIGHT);
    }

    #[test]
    fn dock_params_reserve_the_bottom_band_and_never_take_keyboard() {
        let p = layer_params_for(ChromeSurface::Dock);
        assert_eq!(p.namespace, "duduclaw-shell-dock");
        assert_eq!(p.layer, ChromeLayer::Top);
        assert_eq!(p.anchor, ChromeAnchor::BOTTOM_STRETCH);
        assert_eq!(p.exclusive_zone, Some(DOCK_HEIGHT));
        assert_eq!(p.keyboard_interactivity, ChromeKeyboardInteractivity::None);
    }

    #[test]
    fn home_params_fill_the_output_and_never_shrink_for_others() {
        let p = layer_params_for(ChromeSurface::Home);
        assert_eq!(p.layer, ChromeLayer::Background);
        assert_eq!(p.anchor, ChromeAnchor::FILL);
        assert_eq!(p.exclusive_zone, Some(-1.0));
        assert_eq!(p.keyboard_interactivity, ChromeKeyboardInteractivity::OnDemand);
    }

    #[test]
    fn overlay_params_never_reserve_space_and_always_take_keyboard() {
        // Every `Overlay` variant deliberately listed out (not a wildcard
        // match against `Overlay::all()` or similar, which doesn't exist) —
        // a future overlay added here without updating this test just means
        // one fewer case exercised, not a compile break, so keep this list
        // in sync with `surface::Overlay` when adding a new variant.
        for overlay in
            [Overlay::Launcher, Overlay::Notifications, Overlay::ControlCenter, Overlay::PointerSettings, Overlay::Settings]
        {
            let p = layer_params_for(ChromeSurface::Overlay(overlay));
            assert_eq!(p.namespace, "duduclaw-shell-overlay");
            assert_eq!(p.layer, ChromeLayer::Overlay);
            assert_eq!(p.anchor, ChromeAnchor::FILL);
            assert_eq!(p.exclusive_zone, None, "overlay {overlay:?} must not reserve screen space");
            assert_eq!(p.keyboard_interactivity, ChromeKeyboardInteractivity::Exclusive);
        }
    }

    #[test]
    fn desired_mode_is_single_fullscreen_off_linux() {
        assert_eq!(desired_chrome_mode(false, None), ChromeMode::SingleFullscreen);
    }

    #[test]
    fn desired_mode_is_layer_surfaces_on_linux_by_default() {
        assert_eq!(desired_chrome_mode(true, None), ChromeMode::LayerSurfaces);
    }

    #[test]
    fn no_layer_shell_env_overrides_linux_back_to_single_fullscreen() {
        assert_eq!(desired_chrome_mode(true, Some("1")), ChromeMode::SingleFullscreen);
    }

    #[test]
    fn an_unrecognized_env_value_is_treated_as_unset() {
        assert_eq!(desired_chrome_mode(true, Some("true")), ChromeMode::LayerSurfaces);
        assert_eq!(desired_chrome_mode(true, Some("")), ChromeMode::LayerSurfaces);
    }
}
