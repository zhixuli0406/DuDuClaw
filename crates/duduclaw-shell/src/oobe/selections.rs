// OOBE persisted selection types — split out of `oobe/mod.rs`
// (WP-OOBE-split, 2026-08-21, see `state.rs`'s own header comment for the
// full four-way split this round makes). This file carries the SHAPE of
// what gets persisted: the four selection-value enums (`LanguageChoice`/
// `TemplateChoice`/`ThemeChoice`/`PrivacyToggle`) and the two structs that
// hold them (`OobeSelections`, then `OobeState` wrapping it with
// `current_step`/`completed`) — including their `#[serde(default)]`
// forward-compat hardening (task brief §C, still fully in force: see each
// struct's own doc comment below for why struct-level `#[serde(default)]`
// covers every FUTURE field for free). The I/O that reads/writes this
// shape to disk (`load_state`/`save_state`/`state_path`) and the
// boot-entry resolution built on top of it (`resolve_boot_flow`/
// `boot_theme`) live in `persistence.rs` instead, imported from there —
// kept separate so "what the data LOOKS like" and "how it gets to/from
// disk" stay independently readable, each well under this crate's own
// <800-line convention on its own.

use serde::{Deserialize, Serialize};

use super::state::OobeStep;

/// Which language the operator picked on `LanguageAccessibility` — as of
/// this round every OTHER OOBE screen's copy re-renders through
/// `crate::i18n` using this choice (see `OobeFlow::locale()` below and
/// `steps::language`'s own header comment for the one exempt piece of
/// copy — that step's own top caption, which has to be readable BEFORE a
/// pick exists).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum LanguageChoice {
    #[default]
    ZhTw,
    En,
    JaJp,
}

impl LanguageChoice {
    /// The language's own name, written in itself — NEVER routed through
    /// `crate::i18n::t` (same policy `duduclaw-native-gui/src/i18n/mod.rs`'s
    /// `Locale::native_name()` documents: a reader of any of the three
    /// languages must recognize their own option before picking one, and
    /// showing "日本語" translated INTO whichever language is currently
    /// active would defeat that).
    pub fn label(self) -> &'static str {
        match self {
            LanguageChoice::ZhTw => "繁體中文",
            LanguageChoice::En => "English",
            LanguageChoice::JaJp => "日本語",
        }
    }

    /// Maps onto `crate::i18n::Locale`, the catalog key every OTHER OOBE
    /// screen's copy is driven by. Kept as a mapping FROM this type rather
    /// than merging the two enums: `crate::i18n` is written to have zero
    /// dependency on `oobe` (see that module's own header comment on why —
    /// a future whole-shell i18n extension covering Home/overlay can reuse
    /// it without reaching back into this module), so the conversion lives
    /// on the OOBE-specific side of that boundary.
    pub fn to_locale(self) -> crate::i18n::Locale {
        match self {
            LanguageChoice::ZhTw => crate::i18n::Locale::ZhTw,
            LanguageChoice::En => crate::i18n::Locale::En,
            LanguageChoice::JaJp => crate::i18n::Locale::JaJp,
        }
    }
}

/// Recorded when the `Templates` step is left with a choice made — round 1
/// only reached `Skipped` (the step was a placeholder); round 2 adds the
/// two real outcomes the task brief asks for: `Express` (the "快速開始"
/// one-click card) and `Custom` (one of the fake板模 cards,
/// `fake_data::FAKE_TEMPLATES`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TemplateChoice {
    Express,
    /// Carries the picked card's stable `id` slug (e.g. "retail"), not its
    /// display title — a future round wiring a real template catalog can
    /// look it up by key instead of parsing a label string. Not `Copy`
    /// (unlike round 1's two unit variants) because of this payload, hence
    /// `TemplateChoice` itself dropped the `Copy` derive this round — no
    /// call site depended on it (checked: every existing usage constructs
    /// or compares by value/reference, never relies on an implicit copy).
    Custom(String),
    Skipped,
}

/// Which appearance the operator picked on the `Theme` step — see
/// `OobeStep::Theme`'s own doc comment for why this step exists and where
/// it sits in the sequence. `#[default] Light` matches that screen's own
/// pre-selected card (task brief: "有預設亮色"), so `ThemeChoice::default()`
/// is never a dishonest placeholder — it is the exact value the very first
/// frame of that step already shows as selected, same relationship
/// `LanguageChoice::default()` (`ZhTw`) has to the `LanguageAccessibility`
/// step's own pre-highlighted row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ThemeChoice {
    #[default]
    Light,
    Dark,
}

/// Which of the four `Privacy` step toggles a click/render call is about —
/// a pure selector, never itself persisted (the four underlying `bool`s on
/// `OobeSelections` are what's serialized). `Copy` so a `for toggle in
/// PrivacyToggle::ALL` loop can freely move a fresh copy into each row's own
/// `cx.listener` closure without a borrow conflict between iterations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrivacyToggle {
    UsageStats,
    ErrorReports,
    Personalization,
    Marketing,
}

impl PrivacyToggle {
    pub const ALL: [PrivacyToggle; 4] =
        [PrivacyToggle::UsageStats, PrivacyToggle::ErrorReports, PrivacyToggle::Personalization, PrivacyToggle::Marketing];

    /// Stable, locale-independent identifier — used as the gpui element id
    /// (`steps::privacy`'s own `.id(toggle.slug())`). Split out from
    /// `label()` this round: `label()`/`description()` below now route
    /// through `crate::i18n` and change with the operator's language pick,
    /// but a gpui element id must stay stable across a re-render for gpui's
    /// own diffing to track element IDENTITY correctly — using the
    /// (now-mutable) display text as the id, round 1's shortcut, would have
    /// silently swapped a row's identity on every language change.
    pub fn slug(self) -> &'static str {
        match self {
            PrivacyToggle::UsageStats => "usage-stats",
            PrivacyToggle::ErrorReports => "error-reports",
            PrivacyToggle::Personalization => "personalization",
            PrivacyToggle::Marketing => "marketing",
        }
    }

    pub fn label(self, locale: crate::i18n::Locale) -> &'static str {
        use crate::i18n::{t, Key};
        t(
            locale,
            match self {
                PrivacyToggle::UsageStats => Key::PrivacyUsageStatsLabel,
                PrivacyToggle::ErrorReports => Key::PrivacyErrorReportsLabel,
                PrivacyToggle::Personalization => Key::PrivacyPersonalizationLabel,
                PrivacyToggle::Marketing => Key::PrivacyMarketingLabel,
            },
        )
    }

    pub fn description(self, locale: crate::i18n::Locale) -> &'static str {
        use crate::i18n::{t, Key};
        t(
            locale,
            match self {
                PrivacyToggle::UsageStats => Key::PrivacyUsageStatsDesc,
                PrivacyToggle::ErrorReports => Key::PrivacyErrorReportsDesc,
                PrivacyToggle::Personalization => Key::PrivacyPersonalizationDesc,
                PrivacyToggle::Marketing => Key::PrivacyMarketingDesc,
            },
        )
    }
}

/// Everything the operator has chosen so far — the part of `OobeState` the
/// task brief means by "已完成步驟的選擇（語言/是否跳過等）".
///
/// `#[serde(default)]` at the STRUCT level (task brief §C: "全欄位補
/// `#[serde(default)]`") rather than annotating each field individually —
/// both achieve the same "a missing key defaults instead of failing the
/// whole deserialize" contract (serde fills any absent field from
/// `OobeSelections::default()`), but the struct-level form additionally
/// covers every FUTURE field for free — a field added in a later round
/// without remembering the per-field attribute would otherwise silently
/// reopen exactly the gap this round is closing. Requires `Default`
/// (already derived below) — every field's natural "unset" value already
/// equals what `Default` produces (`bool` → `false`, `Option` → `None`,
/// `LanguageChoice` → its own `#[default]` variant), so there is no field
/// here where the derived default would be dishonest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct OobeSelections {
    pub language: LanguageChoice,
    pub network_connected: bool,
    pub network_ssid: Option<String>,
    pub account_created: bool,
    /// The `AccountCreate` step's typed name value — LOCAL DISPLAY ONLY.
    /// Shell-S2 round 1's real `/api/first-run/claim` gateway endpoint is
    /// password-only for the fixed `admin@local` user (see `oobe::claim`'s
    /// own header comment) — this crate has no account-profile RPC to send
    /// a display name to anywhere, so this field never reaches the gateway.
    /// Kept anyway (rather than discarded after the click) so a resumed
    /// OOBE run after a restart still shows what the operator typed; covered
    /// by this struct's own STRUCT-LEVEL `#[serde(default)]` above, same as
    /// every other field here, so an older on-disk state file with no
    /// `operator_name` key at all still loads fine (see
    /// `load_state_tolerates_an_older_schema_missing_the_operator_name_field`
    /// below).
    pub operator_name: Option<String>,
    pub runtime_deferred: bool,
    /// Set when the `RuntimeAuth` step's "立即設定" path is taken — the
    /// OTHER outcome of that step's two-button choice (`runtime_deferred`,
    /// above, is the "稍後再說" outcome). Both default `false`; a step that
    /// was never reached yet (or was reached but neither button was
    /// clicked before the operator backed out) honestly reports neither.
    pub runtime_authorized: bool,
    /// The `Privacy` step's four independent opt-IN toggles — §A consensus
    /// #4 ("隱私/遙測獨立屏、預設全關", the strongest consensus in the whole
    /// survey, "5/8，無反例") is why every one of these defaults `false`
    /// both here AND via `OobeSelections::default()`: continuing past this
    /// step untouched must be the safe path, not an opt-out one.
    pub privacy_usage_stats: bool,
    pub privacy_error_reports: bool,
    pub privacy_personalization: bool,
    pub privacy_marketing: bool,
    pub template_choice: Option<TemplateChoice>,
    /// The `Theme` step's pick — see `OobeStep::Theme`'s own doc comment.
    /// Covered by this struct's own STRUCT-LEVEL `#[serde(default)]` above
    /// (not a fresh per-field attribute) — a state file written by a binary
    /// from BEFORE this step existed has no `theme` key at all, and per the
    /// exact same forward-compat contract every other field on this struct
    /// already gets, that absence must default in (to `ThemeChoice::Light`)
    /// rather than reset the whole flow back to step 0. See the
    /// `load_state_tolerates_an_older_schema_missing_the_theme_field` test
    /// below for the case this covers.
    pub theme: ThemeChoice,
}

/// The full on-disk/in-memory OOBE state — `current_step` + `completed` is
/// what makes "斷電續步" possible (resume renders straight into
/// `current_step`, no replay of prior steps needed since they're strictly
/// linear).
///
/// `#[serde(default)]` here too (task brief §C), same struct-level
/// reasoning as `OobeSelections` above — a JSON object missing `completed`
/// or `selections` outright (not just missing fields WITHIN `selections`)
/// still degrades field-by-field rather than failing the whole parse. This
/// does NOT cover an unrecognized `current_step` VALUE (as opposed to a
/// missing `current_step` KEY): a step name this binary doesn't know is a
/// genuine enum-variant deserialize error, which `#[serde(default)]` cannot
/// paper over (it only fills in keys that are ABSENT, not keys present with
/// an unparseable value) — that failure legitimately propagates up to
/// `load_state()`'s own `.ok()` fail-open, landing on the FULL default
/// (step 0). The task brief itself calls this acceptable ("current_step
/// 未知值（未來新步）→ 整檔回預設可接受") — see the
/// `load_state_falls_back_to_default_on_an_unrecognized_current_step` test
/// below for the case this distinction covers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OobeState {
    pub completed: bool,
    pub current_step: OobeStep,
    pub selections: OobeSelections,
}

impl Default for OobeState {
    fn default() -> Self {
        // `OobeStep::ALL[0]` — `LanguageAccessibility` as of this round's
        // reorder (see `oobe/mod.rs`'s header comment). NOT hardcoded as a
        // literal variant here on purpose: `OobeStep::from_index(0)` would
        // be the fully-decoupled spelling, but `Default` is meant to be the
        // OBVIOUS "what does a fresh install start on" answer at a glance,
        // and the `all_has_ten_steps_in_declared_order` test (now in
        // `state.rs`, alongside `OobeStep::ALL` itself) already pins
        // `ALL[0]` to this exact variant — so a future reorder that forgets
        // to update this line fails loudly there, not silently here.
        Self { completed: false, current_step: OobeStep::LanguageAccessibility, selections: OobeSelections::default() }
    }
}
