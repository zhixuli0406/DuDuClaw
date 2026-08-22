//! CUR-1: which cursor artwork the compositor draws for the HUMAN pointer,
//! and how that choice is configured from outside the process.
//!
//! # The choice
//!
//! [`CursorSource::System`] (the default) draws the standard XCursor theme
//! already installed on the machine — the same arrow / I-beam / resize
//! cursors every other Linux desktop shows. [`CursorSource::Brand`] draws a
//! DuDuClaw-branded XCursor theme instead.
//!
//! **The brand artwork does not exist yet.** CUR-1 deliberately ships only
//! the seam: the hand-drawn claw cursor is a separate design work package
//! (DESIGN-native-gui-gpui-2026-08.md §13.5's shell art convention), and the
//! user's own call on this was explicit — "手繪爪形品牌游標這個我認為可以當
//! 設定中的替換，正常還是用正常的游標就好了". So `System` is the default and
//! `Brand` is fail-safe: selecting it when no brand theme is installed logs a
//! warning and falls straight back to the system theme (see
//! [`crate::cursor::theme::CursorThemeStore::new`]).
//!
//! # Why the brand path is "just another XCursor theme"
//!
//! Because that costs zero extra loading code. `Brand` only changes the
//! theme *name* handed to `xcursor::CursorTheme::load`; the art package
//! ships as a perfectly ordinary theme directory
//! (`/usr/share/icons/DuDuClaw/cursors/…`, or anywhere else on `XCURSOR_PATH`)
//! and every icon name, hotspot and size negotiation keeps working exactly as
//! it does for Adwaita. The alternative — inventing a DuDuClaw-specific
//! cursor asset format — would have meant a second loader, a second cache and
//! a second set of hotspot bugs for no gain.
//!
//! # Why an environment variable, and not a config file or a socket op
//!
//! Three candidate mechanisms existed; this is why the env var won:
//!
//! * **A comp config file.** `duduclaw-comp` has none, and the task brief is
//!   explicit that this must not grow "一整套設定系統". Every tunable this
//!   compositor already has is an env var read once at startup —
//!   `DUDUCLAW_COMP_SEAT_ORDER` (`crate::seat_order`),
//!   `DUDUCLAW_COMP_BACKEND` (`crate::backend_choice`),
//!   `DUDUCLAW_COMP_DRM_DEVICE` (`crate::udev_backend`),
//!   `DUDUCLAW_CODRIVE_WATCH_IDLE_SECS` (`crate::codrive::watch`). This is
//!   the established convention, not a new one.
//! * **A `shell_control` socket op.** That socket is a *control* channel with
//!   no persistence at all (`crate::shell_control`: one request per
//!   connection, no stored state). A live `set_cursor_source` op would still
//!   need the durable value to live somewhere outside comp so it survives a
//!   compositor restart — i.e. it needs this env var *anyway*, plus a second
//!   mechanism on top.
//! * **This env var.** The durable value lives wherever the session launcher
//!   that spawns comp keeps its environment.
//!
//! ## How the shell wires a settings page to this later
//!
//! The shell writes the chosen value into comp's spawn environment and
//! restarts the compositor — mechanically identical to what an operator does
//! today for `DUDUCLAW_COMP_SEAT_ORDER`. If a future round wants the switch
//! to take effect *without* a restart, the hook is already shaped for it:
//! [`CursorSource::from_env_value`] is a pure function and the live value is
//! a plain field on `DuduclawComp`
//! (`crate::cursor::CursorState::source`), so a `shell_control`
//! `set_cursor_source` op would only have to set that field, drop the theme
//! cache and `queue_redraw()`. That op is **deliberately not implemented
//! here** — it is a live-reconfiguration feature with its own auth/audit
//! surface, and CUR-1 is a cursor-rendering work package.

/// Where the human pointer's artwork comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CursorSource {
    /// The machine's standard XCursor theme. **Default.**
    #[default]
    System,
    /// A DuDuClaw-branded XCursor theme ([`BRAND_THEME_NAME`]), falling back
    /// to [`CursorSource::System`] when that theme is not installed.
    Brand,
}

/// Selects [`CursorSource`]. See this module's doc for why it is an env var.
pub const CURSOR_SOURCE_ENV: &str = "DUDUCLAW_COMP_CURSOR_SOURCE";

/// Overrides the XCursor theme name outright, for either source. Takes
/// priority over the freedesktop-standard `XCURSOR_THEME`, which is itself
/// honored (see [`resolve_theme_name`]).
pub const CURSOR_THEME_ENV: &str = "DUDUCLAW_COMP_CURSOR_THEME";

/// Theme name [`CursorSource::Brand`] looks for. Nothing installs it yet —
/// see this module's doc.
pub const BRAND_THEME_NAME: &str = "DuDuClaw";

/// Theme asked for when neither [`CURSOR_THEME_ENV`] nor `XCURSOR_THEME` says
/// otherwise. Present on the appliance image already (pulled in by
/// GTK/chromium), and `xcursor` falls through the theme's own `Inherits`
/// chain to `default` when a given icon is missing, so naming it here costs
/// nothing on a machine that has some other theme instead.
pub const DEFAULT_THEME_NAME: &str = "Adwaita";

/// Cursor size in logical pixels when `XCURSOR_SIZE` is unset/garbage. 24 is
/// the freedesktop de-facto default.
pub const DEFAULT_CURSOR_SIZE: u32 = 24;

/// Clamp bounds for `XCURSOR_SIZE`. A 0-px cursor is invisible and a
/// 100 000-px one would try to allocate gigabytes; both are refused in favour
/// of the nearest sane value rather than failing to boot.
pub const MIN_CURSOR_SIZE: u32 = 8;
/// See [`MIN_CURSOR_SIZE`].
pub const MAX_CURSOR_SIZE: u32 = 512;

impl CursorSource {
    /// Parses the [`CURSOR_SOURCE_ENV`] value.
    ///
    /// Pure (takes the already-read value) for the same reason
    /// [`crate::seat_order::SeatAdvertiseOrder::from_env_value`] is: it stays
    /// unit-testable without `std::env::set_var`, which is unsound to call
    /// from tests running concurrently with anything else.
    ///
    /// Unset / empty / unrecognised all fall back to [`CursorSource::System`]
    /// — a typo must not cost the operator a usable pointer.
    pub fn from_env_value(raw: Option<&str>) -> Self {
        match raw.map(str::trim) {
            Some(v) if v.eq_ignore_ascii_case("brand") || v.eq_ignore_ascii_case("duduclaw") => {
                Self::Brand
            }
            _ => Self::System,
        }
    }

    /// Reads [`CURSOR_SOURCE_ENV`] from the real environment.
    pub fn from_env() -> Self {
        Self::from_env_value(std::env::var(CURSOR_SOURCE_ENV).ok().as_deref())
    }

    /// Stable short name for tracing fields.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Brand => "brand",
        }
    }
}

/// Resolves the XCursor theme name to load.
///
/// Priority, highest first:
/// 1. [`CURSOR_THEME_ENV`] — an explicit operator override, honored for both
///    sources (it is how you point `Brand` at a differently-named art package
///    without a rebuild).
/// 2. [`BRAND_THEME_NAME`], when the source is [`CursorSource::Brand`].
/// 3. `XCURSOR_THEME` — the freedesktop standard the rest of the desktop
///    already respects, so comp agrees with GTK/Qt apps by default.
/// 4. [`DEFAULT_THEME_NAME`].
///
/// Blank/whitespace-only values at any level are treated as unset rather than
/// as a request for a theme literally named `""`.
pub fn resolve_theme_name(
    source: CursorSource,
    explicit: Option<&str>,
    xcursor_theme: Option<&str>,
) -> String {
    if let Some(name) = explicit.map(str::trim).filter(|s| !s.is_empty()) {
        return name.to_string();
    }
    if source == CursorSource::Brand {
        return BRAND_THEME_NAME.to_string();
    }
    if let Some(name) = xcursor_theme.map(str::trim).filter(|s| !s.is_empty()) {
        return name.to_string();
    }
    DEFAULT_THEME_NAME.to_string()
}

/// Resolves the cursor size from `XCURSOR_SIZE`, clamped to
/// `[MIN_CURSOR_SIZE, MAX_CURSOR_SIZE]`. Unset / non-numeric / zero all fall
/// back to [`DEFAULT_CURSOR_SIZE`].
pub fn resolve_size(xcursor_size: Option<&str>) -> u32 {
    xcursor_size
        .map(str::trim)
        .and_then(|s| s.parse::<u32>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_CURSOR_SIZE)
        .clamp(MIN_CURSOR_SIZE, MAX_CURSOR_SIZE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_source_is_system() {
        // The user's own call: normal cursors by default, brand art is the
        // opt-in replacement. See this module's doc.
        assert_eq!(CursorSource::default(), CursorSource::System);
        assert_eq!(CursorSource::from_env_value(None), CursorSource::System);
    }

    #[test]
    fn brand_is_selectable_case_and_whitespace_insensitively() {
        for raw in ["brand", "BRAND", "  Brand  ", "duduclaw", "DuDuClaw"] {
            assert_eq!(
                CursorSource::from_env_value(Some(raw)),
                CursorSource::Brand,
                "{raw:?} should select Brand"
            );
        }
    }

    #[test]
    fn garbage_falls_back_to_system_rather_than_refusing_to_boot() {
        for raw in ["", "   ", "1", "yes", "claw", "system", "🐾"] {
            assert_eq!(
                CursorSource::from_env_value(Some(raw)),
                CursorSource::System,
                "{raw:?} should fall back to System"
            );
        }
    }

    #[test]
    fn theme_name_priority_explicit_beats_everything() {
        assert_eq!(
            resolve_theme_name(CursorSource::Brand, Some("Bibata"), Some("Breeze")),
            "Bibata"
        );
        assert_eq!(
            resolve_theme_name(CursorSource::System, Some(" Bibata "), Some("Breeze")),
            "Bibata"
        );
    }

    #[test]
    fn theme_name_brand_beats_xcursor_theme() {
        assert_eq!(
            resolve_theme_name(CursorSource::Brand, None, Some("Breeze")),
            BRAND_THEME_NAME
        );
    }

    #[test]
    fn theme_name_system_honors_xcursor_theme_then_default() {
        assert_eq!(
            resolve_theme_name(CursorSource::System, None, Some("Breeze")),
            "Breeze"
        );
        assert_eq!(
            resolve_theme_name(CursorSource::System, None, None),
            DEFAULT_THEME_NAME
        );
    }

    #[test]
    fn blank_values_count_as_unset_not_as_an_empty_theme_name() {
        assert_eq!(
            resolve_theme_name(CursorSource::System, Some("   "), Some("")),
            DEFAULT_THEME_NAME
        );
    }

    #[test]
    fn size_defaults_and_clamps() {
        assert_eq!(resolve_size(None), DEFAULT_CURSOR_SIZE);
        assert_eq!(resolve_size(Some("")), DEFAULT_CURSOR_SIZE);
        assert_eq!(resolve_size(Some("nonsense")), DEFAULT_CURSOR_SIZE);
        assert_eq!(resolve_size(Some("0")), DEFAULT_CURSOR_SIZE);
        assert_eq!(resolve_size(Some(" 48 ")), 48);
        assert_eq!(resolve_size(Some("1")), MIN_CURSOR_SIZE);
        assert_eq!(resolve_size(Some("100000")), MAX_CURSOR_SIZE);
    }

    #[test]
    fn source_as_str_is_stable() {
        assert_eq!(CursorSource::System.as_str(), "system");
        assert_eq!(CursorSource::Brand.as_str(), "brand");
    }
}
