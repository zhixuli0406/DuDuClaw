// Surface state machine — Shell-S0 (2026-08-19).
//
// Windowing model (task brief): `Home` is the always-present base surface —
// never closed, never itself an overlay, always what's underneath.
// `Launcher` / `Notifications` / `ControlCenter` are transient overlays
// rendered ON TOP of Home, at most one at a time. `SurfaceState` therefore
// only needs to track "which overlay, if any" — Home doesn't need its own
// enum variant, it's simply what shows when `overlay` is `None`.
//
// Deliberately pure `&mut self` mutation with no gpui types anywhere in this
// file — testable without a live window. This crate has no headless
// UI-click harness (same gap `duduclaw-native-gui`'s own `main.rs` gotcha
// list documents for that crate), so pushing every branch of "what happens
// on cmd-k / Escape / opening a second overlay" into plain data here, with
// `main.rs` reduced to two thin `cx.on_action` closures that just call
// these methods, is what makes the state machine itself actually testable.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overlay {
    Launcher,
    Notifications,
    ControlCenter,
    /// ICON-3 (2026-08-23) — 「協助工具 › 指向與點按」, reached from a row
    /// inside ControlCenter. A fourth overlay rather than a card inside
    /// ControlCenter itself: the five size buttons draw the real cursor at
    /// up to 96px, which does not fit that panel's 372px width, and this
    /// shell has no settings application to make it a page of. See
    /// `overlay/pointer_settings.rs`'s own header comment.
    ///
    /// Note that opening it therefore CLOSES ControlCenter — overlays never
    /// stack (see `open` below) — and Escape from here returns to Home, not
    /// back to ControlCenter. That is the existing model applied
    /// consistently, not a special case.
    PointerSettings,
    /// D4b (2026-08-23) — 系統設定, the settings application
    /// (`crate::settings`), reached from a row inside ControlCenter.
    ///
    /// A fifth overlay for the same reason `PointerSettings` is a fourth:
    /// this shell draws its own surfaces in its own gpui tree, and a
    /// 1180×744 panel with a seven-row sidebar does not fit inside
    /// ControlCenter's 372px column. The non-stacking rule applies here too —
    /// opening it CLOSES ControlCenter, and Escape returns to Home rather
    /// than back to it.
    ///
    /// `PointerSettings` deliberately stays its own overlay rather than
    /// becoming an eighth settings category: it is 協助工具, not a system
    /// setting, its board is a different screen, and folding it in would mean
    /// rewriting a page that already shipped and works.
    Settings,
}

impl Overlay {
    /// Parses `DUDUCLAW_SHELL_DEBUG_SURFACE`'s value — the headless-smoke
    /// boot hook this crate's task brief asks for (no scriptable UI-click
    /// automation exists here, matching `duduclaw-native-gui`'s own
    /// `DUDUCLAW_NATIVE_GUI_DEBUG_PAGE` precedent). An unrecognized value
    /// returns `None` and is ignored by the caller — never panics on a
    /// typo'd env var.
    pub fn from_debug_env(raw: &str) -> Option<Self> {
        match raw {
            "launcher" => Some(Overlay::Launcher),
            "notifications" => Some(Overlay::Notifications),
            "controlcenter" => Some(Overlay::ControlCenter),
            "pointer" => Some(Overlay::PointerSettings),
            "settings" => Some(Overlay::Settings),
            _ => None,
        }
    }
}

/// Home is implicit (always the base layer, see this module's header
/// comment) — this struct only tracks which overlay, if any, currently sits
/// on top of it. `Default` (no overlay open) is the state the shell boots
/// into on a normal launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SurfaceState {
    overlay: Option<Overlay>,
}

impl SurfaceState {
    pub fn overlay(&self) -> Option<Overlay> {
        self.overlay
    }

    /// Opens `overlay`, replacing whatever (if anything) was already open —
    /// overlays never stack (task brief: "同時最多一個").
    pub fn open(&mut self, overlay: Overlay) {
        self.overlay = Some(overlay);
    }

    /// Escape's handler: closes whatever overlay is currently open. A
    /// no-op (not a panic) when Home is already the only thing showing.
    pub fn close(&mut self) {
        self.overlay = None;
    }

    /// cmd-k's handler: Launcher specifically toggles — same key opens AND
    /// closes it, mirroring macOS Spotlight's own cmd-space convention.
    /// Pressing cmd-k while Notifications/ControlCenter is already open
    /// switches straight to Launcher rather than requiring a separate
    /// Escape first: "open" wins over "something else is currently open",
    /// matching the task brief's literal wording ("cmd-k 開/關 Launcher",
    /// not "cmd-k 開/關當前 overlay").
    pub fn toggle_launcher(&mut self) {
        self.overlay = match self.overlay {
            Some(Overlay::Launcher) => None,
            _ => Some(Overlay::Launcher),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_with_no_overlay() {
        assert_eq!(SurfaceState::default().overlay(), None);
    }

    #[test]
    fn open_sets_the_overlay() {
        let mut s = SurfaceState::default();
        s.open(Overlay::Notifications);
        assert_eq!(s.overlay(), Some(Overlay::Notifications));
    }

    #[test]
    fn opening_a_second_overlay_replaces_not_stacks() {
        let mut s = SurfaceState::default();
        s.open(Overlay::Notifications);
        s.open(Overlay::ControlCenter);
        assert_eq!(s.overlay(), Some(Overlay::ControlCenter));
    }

    #[test]
    fn close_clears_the_overlay() {
        let mut s = SurfaceState::default();
        s.open(Overlay::Launcher);
        s.close();
        assert_eq!(s.overlay(), None);
    }

    #[test]
    fn close_on_an_already_empty_state_is_a_noop_not_a_panic() {
        let mut s = SurfaceState::default();
        s.close();
        assert_eq!(s.overlay(), None);
    }

    #[test]
    fn toggle_launcher_opens_it_from_none() {
        let mut s = SurfaceState::default();
        s.toggle_launcher();
        assert_eq!(s.overlay(), Some(Overlay::Launcher));
    }

    #[test]
    fn toggle_launcher_closes_it_when_already_open() {
        let mut s = SurfaceState::default();
        s.toggle_launcher();
        s.toggle_launcher();
        assert_eq!(s.overlay(), None);
    }

    #[test]
    fn toggle_launcher_switches_over_a_different_open_overlay() {
        let mut s = SurfaceState::default();
        s.open(Overlay::ControlCenter);
        s.toggle_launcher();
        assert_eq!(s.overlay(), Some(Overlay::Launcher));
    }

    #[test]
    fn debug_env_parses_all_known_values() {
        assert_eq!(Overlay::from_debug_env("launcher"), Some(Overlay::Launcher));
        assert_eq!(Overlay::from_debug_env("notifications"), Some(Overlay::Notifications));
        assert_eq!(Overlay::from_debug_env("controlcenter"), Some(Overlay::ControlCenter));
        assert_eq!(Overlay::from_debug_env("pointer"), Some(Overlay::PointerSettings));
        assert_eq!(Overlay::from_debug_env("settings"), Some(Overlay::Settings));
    }

    /// D4b: 系統設定 obeys the same non-stacking rule as every other overlay —
    /// opening it from ControlCenter replaces that panel rather than layering
    /// on top of it, and Escape goes to Home.
    #[test]
    fn opening_settings_replaces_control_center_and_escape_returns_to_home() {
        let mut s = SurfaceState::default();
        s.open(Overlay::ControlCenter);
        s.open(Overlay::Settings);
        assert_eq!(s.overlay(), Some(Overlay::Settings));
        s.close();
        assert_eq!(s.overlay(), None);
    }

    #[test]
    fn debug_env_rejects_an_unknown_value() {
        assert_eq!(Overlay::from_debug_env("bogus"), None);
    }
}
