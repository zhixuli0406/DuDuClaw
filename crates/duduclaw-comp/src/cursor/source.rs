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
//!
//! # CUR-2 (2026-08-22): that op now exists
//!
//! `shell_control`'s `get_cursor_source` / `set_cursor_source` ops do exactly
//! what the paragraph above predicted, and the durable half lives in
//! [`super::persist`] rather than in comp's spawn environment. The env var is
//! unchanged and still WINS — see [`resolve_startup_source`] for the full
//! priority order and [`super::persist`]'s module doc for why an operator's
//! explicit environment must outrank a stored user preference.
//!
//! # CUR-3 (2026-08-23): cursor SIZE gets the same treatment
//!
//! The shell's 協助工具 › 指向與點按 page offers five size steps
//! ([`CURSOR_SIZE_STEPS`]) and drives them through a `set_cursor_size` op,
//! with `XCURSOR_SIZE` keeping its existing operator-override priority
//! ([`resolve_startup_size`]).
//!
//! ## Two size gates, deliberately different, and why
//!
//! | Surface | Accepts | Enforced by |
//! |---|---|---|
//! | `XCURSOR_SIZE` env var (OPERATOR) | any integer, clamped to 8–512 | [`resolve_size`] |
//! | `set_cursor_size` op + stored preference (UI) | exactly [`CURSOR_SIZE_STEPS`] | [`cursor_size_from_wire`] |
//!
//! This asymmetry is intentional, not an oversight. The env var is the
//! operator's machine-level channel — the same one that already accepts an
//! arbitrary theme name — and an operator who wants a 40 px pointer for a
//! specific panel has a legitimate reason a settings UI cannot anticipate.
//! The op, by contrast, is the *UI's* channel: the five steps are what the
//! design canvas settled on, they are the only values any button can produce,
//! and accepting a sixth would immediately create the failure
//! [`CursorSource::parse_strict`]'s own doc warns about — a settings page
//! showing a state the compositor is not in, because no radio button matches
//! the value that got stored. Same reasoning, applied to a number instead of
//! an enum: the lenient parser guards boot, the strict parser guards the
//! control channel.
//!
//! The stored preference is held to the *strict* rule rather than the lenient
//! one because the only writer of that key is the strict op. A `size` outside
//! the five steps in `cursor.json` is therefore corruption or a hand-edit that
//! no UI could ever display, and it degrades to "no stored size" with a warning
//! — while the operator who genuinely wants 40 px still has `XCURSOR_SIZE`,
//! which outranks the file anyway.

use std::fmt;

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

/// CUR-3: the closed set of sizes the shell's 協助工具 › 指向與點按 page
/// offers, in logical pixels, ascending.
///
/// These are the five steps the design canvas settled on — and, checked rather
/// than assumed, they are **exactly** the five nominal sizes Debian's
/// `adwaita-icon-theme` ships inside every one of its XCursor files (read
/// straight out of the `Xcur` table of contents: `[24, 32, 48, 64, 96]`, each
/// with matching `width`/`height`). So on the appliance's own theme every step
/// resolves to a real, purpose-drawn image rather than to a neighbour — see
/// [`super::theme::CursorThemeStore::effective_size`] for what happens on a
/// theme that is less complete.
pub const CURSOR_SIZE_STEPS: [u32; 5] = [24, 32, 48, 64, 96];

/// CUR-3: strict parse for a size arriving over the `shell_control` socket.
///
/// Takes `i64` — the widest thing the wire can hand us — precisely so that a
/// negative or absurd value is *refused as an invalid size* rather than dying
/// earlier as a JSON type error. Deserializing straight into `u32` would make
/// `{"size":-5}` a `parse_error`, which tells the caller the wrong thing about
/// what is wrong with their request.
///
/// Deliberately NOT [`resolve_size`]'s behaviour, which clamps. Clamping is
/// right for an env var (a boot must not fail over a typo); it is wrong here
/// for the same reason [`CursorSource::parse_strict`] refuses `"brnad"` —
/// silently storing 512 when the caller asked for 5000 leaves a settings page
/// with no button to highlight. Repo convention #4: control surfaces fail
/// closed.
pub fn cursor_size_from_wire(raw: i64) -> Option<u32> {
    let n = u32::try_from(raw).ok()?;
    CURSOR_SIZE_STEPS.contains(&n).then_some(n)
}

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

    /// CUR-2: strict parse for values arriving over the `shell_control`
    /// socket and from the stored preference file.
    ///
    /// Deliberately NOT [`Self::from_env_value`]'s behaviour. That one folds
    /// anything unrecognised into `System`, which is right for an env var (a
    /// typo must not cost the operator a usable pointer at boot). It is
    /// wrong for a control-channel op: a caller that sends `"brnad"` has a
    /// bug, and silently doing something *else* than what it asked is how a
    /// settings UI ends up showing a state the compositor is not in. So this
    /// returns `None` and the caller answers with an error — repo convention
    /// #4, security gates and control surfaces fail closed.
    ///
    /// `system` is accepted as an explicit value here (unlike in the env
    /// path, where it is merely the fallback), because "put it back to the
    /// system cursor" is a real, distinct request.
    pub fn parse_strict(raw: &str) -> Option<Self> {
        let v = raw.trim();
        if v.eq_ignore_ascii_case("system") {
            Some(Self::System)
        } else if v.eq_ignore_ascii_case("brand") || v.eq_ignore_ascii_case("duduclaw") {
            Some(Self::Brand)
        } else {
            None
        }
    }

    /// Stable short name for tracing fields.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Brand => "brand",
        }
    }
}

impl fmt::Display for CursorSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where the compositor's live cursor source came from. Reported over
/// `shell_control` so a settings UI can explain *why* the radio button shows
/// what it shows — in particular, that an operator-pinned env var is the
/// reason a stored preference is not in effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceOrigin {
    /// [`CURSOR_SOURCE_ENV`] was set in comp's spawn environment.
    Env,
    /// Read from the stored preference file ([`super::persist`]).
    Persisted,
    /// Neither — [`CursorSource::default`].
    Default,
    /// Set at runtime by a `set_cursor_source` op since this process started.
    Runtime,
}

impl SourceOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Env => "env",
            Self::Persisted => "persisted",
            Self::Default => "default",
            Self::Runtime => "runtime",
        }
    }
}

impl fmt::Display for SourceOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Startup priority, highest first:
///
/// 1. [`CURSOR_SOURCE_ENV`] — an operator's explicit, machine-level
///    statement. Present-but-garbage still counts as "the operator spoke"
///    and lands on `System` via [`CursorSource::from_env_value`]'s existing
///    lenient rule; it does NOT silently fall through to a stored preference,
///    because "your typo quietly activated somebody else's saved setting" is
///    a worse outcome than "your typo got you the default".
/// 2. The stored user preference ([`super::persist::load`]).
/// 3. [`CursorSource::default`] — `System`.
///
/// Pure: both inputs are already-read values, same testability reasoning as
/// [`CursorSource::from_env_value`].
pub fn resolve_startup_source(
    env_raw: Option<&str>,
    persisted: Option<CursorSource>,
) -> (CursorSource, SourceOrigin) {
    // Blank/whitespace-only is treated as UNSET, not as "the operator spoke"
    // — an empty env var is what `FOO=` or an unset shell variable expanding
    // to nothing looks like, and neither is an intentional choice.
    if let Some(raw) = env_raw.filter(|v| !v.trim().is_empty()) {
        return (CursorSource::from_env_value(Some(raw)), SourceOrigin::Env);
    }
    match persisted {
        Some(s) => (s, SourceOrigin::Persisted),
        None => (CursorSource::default(), SourceOrigin::Default),
    }
}

/// True iff [`CURSOR_SOURCE_ENV`] is set to a non-blank value in this
/// process's environment — i.e. iff a stored preference will be ignored at
/// the next start. Surfaced to callers as `env_pinned`.
pub fn env_pins_source() -> bool {
    std::env::var(CURSOR_SOURCE_ENV)
        .ok()
        .is_some_and(|v| !v.trim().is_empty())
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

/// CUR-3: startup size priority, highest first — the exact same shape (and the
/// exact same reasoning) as [`resolve_startup_source`], one level down:
///
/// 1. `XCURSOR_SIZE` — the operator's explicit, machine-level statement, and
///    a freedesktop standard the rest of the desktop already honours. A
///    present-but-garbage value still counts as "the operator spoke" and lands
///    on [`DEFAULT_CURSOR_SIZE`] via [`resolve_size`]'s lenient rule; it does
///    NOT fall through to the stored preference, because "your typo quietly
///    activated somebody else's saved setting" is the worse outcome.
/// 2. The stored user preference ([`super::persist::load_size`]), which is held
///    to the strict [`CURSOR_SIZE_STEPS`] rule — see this module's doc table.
/// 3. [`DEFAULT_CURSOR_SIZE`].
///
/// Pure: both inputs are already-read values.
pub fn resolve_startup_size(env_raw: Option<&str>, persisted: Option<u32>) -> (u32, SourceOrigin) {
    // Blank/whitespace-only is UNSET, not "the operator spoke" — same rule and
    // same reason as `resolve_startup_source`.
    if let Some(raw) = env_raw.filter(|v| !v.trim().is_empty()) {
        return (resolve_size(Some(raw)), SourceOrigin::Env);
    }
    match persisted {
        Some(n) => (n, SourceOrigin::Persisted),
        None => (DEFAULT_CURSOR_SIZE, SourceOrigin::Default),
    }
}

/// True iff `XCURSOR_SIZE` is set to a non-blank value in this process's
/// environment — i.e. iff a stored size preference will be ignored at the next
/// start. Surfaced to callers as `size_env_pinned`, the size-side twin of
/// [`env_pins_source`].
pub fn env_pins_size() -> bool {
    std::env::var("XCURSOR_SIZE")
        .ok()
        .is_some_and(|v| !v.trim().is_empty())
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
        assert_eq!(CursorSource::Brand.to_string(), "brand");
    }

    // ── CUR-2 ────────────────────────────────────────────────────────────

    #[test]
    fn parse_strict_accepts_exactly_the_documented_spellings() {
        for raw in ["system", "SYSTEM", "  System  "] {
            assert_eq!(CursorSource::parse_strict(raw), Some(CursorSource::System), "{raw:?}");
        }
        for raw in ["brand", "BRAND", " Brand ", "duduclaw", "DuDuClaw"] {
            assert_eq!(CursorSource::parse_strict(raw), Some(CursorSource::Brand), "{raw:?}");
        }
    }

    #[test]
    fn parse_strict_rejects_what_from_env_value_would_have_swallowed() {
        // This is the whole reason the two parsers are separate: over a
        // control socket, "brnad" must be an error the caller can show, not
        // a silent downgrade to the system cursor.
        for raw in ["", "   ", "brnad", "1", "yes", "claw", "🐾", "systembrand"] {
            assert_eq!(CursorSource::parse_strict(raw), None, "{raw:?} must be refused");
            // …while the env parser keeps its lenient, boot-safe behaviour.
            assert_eq!(CursorSource::from_env_value(Some(raw)), CursorSource::System);
        }
    }

    #[test]
    fn startup_priority_env_beats_persisted_beats_default() {
        assert_eq!(
            resolve_startup_source(Some("brand"), Some(CursorSource::System)),
            (CursorSource::Brand, SourceOrigin::Env)
        );
        assert_eq!(
            resolve_startup_source(Some("system"), Some(CursorSource::Brand)),
            (CursorSource::System, SourceOrigin::Env)
        );
        assert_eq!(
            resolve_startup_source(None, Some(CursorSource::Brand)),
            (CursorSource::Brand, SourceOrigin::Persisted)
        );
        assert_eq!(
            resolve_startup_source(None, None),
            (CursorSource::System, SourceOrigin::Default)
        );
    }

    #[test]
    fn a_blank_env_var_counts_as_unset_so_the_preference_still_applies() {
        for blank in ["", "   ", "\t"] {
            assert_eq!(
                resolve_startup_source(Some(blank), Some(CursorSource::Brand)),
                (CursorSource::Brand, SourceOrigin::Persisted),
                "{blank:?} should not count as an operator override"
            );
        }
    }

    #[test]
    fn a_typo_in_the_env_var_does_not_fall_through_to_the_stored_preference() {
        // Documented deliberately (see `resolve_startup_source`): the
        // operator DID speak, they just misspelled it, so they get the
        // default rather than somebody's saved brand preference.
        assert_eq!(
            resolve_startup_source(Some("brnad"), Some(CursorSource::Brand)),
            (CursorSource::System, SourceOrigin::Env)
        );
    }

    #[test]
    fn origin_names_are_stable() {
        assert_eq!(SourceOrigin::Env.as_str(), "env");
        assert_eq!(SourceOrigin::Persisted.as_str(), "persisted");
        assert_eq!(SourceOrigin::Default.as_str(), "default");
        assert_eq!(SourceOrigin::Runtime.as_str(), "runtime");
        assert_eq!(SourceOrigin::Runtime.to_string(), "runtime");
    }

    // ── CUR-3: cursor size ───────────────────────────────────────────────

    #[test]
    fn the_five_steps_are_the_canvas_values_ascending_and_start_at_the_default() {
        assert_eq!(CURSOR_SIZE_STEPS, [24, 32, 48, 64, 96]);
        assert!(
            CURSOR_SIZE_STEPS.windows(2).all(|w| w[0] < w[1]),
            "the shell renders these as ordered segments; unsorted would ship a scrambled control"
        );
        assert_eq!(
            CURSOR_SIZE_STEPS[0], DEFAULT_CURSOR_SIZE,
            "the first step must be the value a fresh install already draws, or opening the \
             settings page would highlight a button that is not in effect"
        );
        // Every step must also survive the env-side clamp, or an operator
        // mirroring a UI choice into XCURSOR_SIZE would silently get a
        // different number back.
        for s in CURSOR_SIZE_STEPS {
            assert_eq!(resolve_size(Some(&s.to_string())), s, "step {s} is not env-representable");
        }
    }

    #[test]
    fn the_wire_parser_accepts_exactly_the_five_steps() {
        for s in CURSOR_SIZE_STEPS {
            assert_eq!(cursor_size_from_wire(s as i64), Some(s), "step {s} must be accepted");
        }
    }

    #[test]
    fn the_wire_parser_refuses_everything_else_rather_than_clamping_it() {
        // This is the whole reason it is not `resolve_size`: a value the UI
        // cannot render as a selected button must come back as an error the
        // caller can show, never as a silently substituted neighbour.
        for bad in [-5_i64, -1, 0, 1, 8, 23, 25, 40, 63, 95, 97, 128, 512, 513, 100_000] {
            assert_eq!(cursor_size_from_wire(bad), None, "{bad} must be refused");
        }
        // Beyond u32 entirely — must be an invalid SIZE, not a panic and not
        // a wrapped-around small number.
        assert_eq!(cursor_size_from_wire(i64::MAX), None);
        assert_eq!(cursor_size_from_wire(i64::MIN), None);
        assert_eq!(cursor_size_from_wire(u32::MAX as i64 + 1), None);
    }

    #[test]
    fn a_clamped_env_value_is_not_automatically_a_legal_op_value() {
        // The two gates are deliberately different (see the module doc table):
        // `XCURSOR_SIZE=5000` boots at 512, but `set_cursor_size 512` is still
        // refused because no button offers it.
        assert_eq!(resolve_size(Some("5000")), MAX_CURSOR_SIZE);
        assert_eq!(cursor_size_from_wire(MAX_CURSOR_SIZE as i64), None);
        assert_eq!(resolve_size(Some("1")), MIN_CURSOR_SIZE);
        assert_eq!(cursor_size_from_wire(MIN_CURSOR_SIZE as i64), None);
    }

    #[test]
    fn startup_size_priority_env_beats_persisted_beats_default() {
        assert_eq!(resolve_startup_size(Some("48"), Some(96)), (48, SourceOrigin::Env));
        assert_eq!(resolve_startup_size(None, Some(96)), (96, SourceOrigin::Persisted));
        assert_eq!(
            resolve_startup_size(None, None),
            (DEFAULT_CURSOR_SIZE, SourceOrigin::Default)
        );
    }

    #[test]
    fn a_blank_xcursor_size_counts_as_unset_so_the_preference_still_applies() {
        for blank in ["", "   ", "\t"] {
            assert_eq!(
                resolve_startup_size(Some(blank), Some(64)),
                (64, SourceOrigin::Persisted),
                "{blank:?} should not count as an operator override"
            );
        }
    }

    #[test]
    fn a_garbage_xcursor_size_does_not_fall_through_to_the_stored_preference() {
        // Same deliberate rule as `resolve_startup_source`'s typo case: the
        // operator DID speak, so they get the default rather than somebody's
        // saved 96.
        assert_eq!(
            resolve_startup_size(Some("big"), Some(96)),
            (DEFAULT_CURSOR_SIZE, SourceOrigin::Env)
        );
        assert_eq!(
            resolve_startup_size(Some("0"), Some(96)),
            (DEFAULT_CURSOR_SIZE, SourceOrigin::Env)
        );
    }

    #[test]
    fn an_operator_env_size_outside_the_five_steps_is_still_honoured() {
        // The env var is the escape hatch the strict op deliberately does not
        // provide — see the module doc table. 40 is not a step, but an
        // operator asking for it must get it.
        assert_eq!(resolve_startup_size(Some("40"), Some(96)), (40, SourceOrigin::Env));
        assert_eq!(cursor_size_from_wire(40), None, "…while the op still refuses it");
    }
}
