// OOBE (Out-of-Box Experience) state machine + persistence — Shell-S1
// (2026-08-20).
//
// Round 1 shipped the full nine-step SKELETON (state machine + persistence
// + boot-entry resolution) plus REAL screens for the first five steps, with
// steps 6-9 (RuntimeAuth/Privacy/Templates/Finish) rendering as an honest
// placeholder page. Round 2 (this pass) replaces that placeholder with real
// screens for those four steps (`steps::{runtime_auth,privacy,templates,
// finish}`) and hardens the on-disk schema for forward compatibility (see
// the `#[serde(default)]` note on `OobeState`/`OobeSelections` below) — the
// STATE MACHINE rules for all nine steps were already real and enforced
// since round 1, only the visual content and the new selection fields those
// four steps need (`runtime_authorized`, the four `privacy_*` toggles, and
// `TemplateChoice::Custom`) are new this round.
//
// Step order/skippability is NOT invented — it is taken (with ONE correction,
// see below) from `research/native-os-2026-08/oobe-first-run-reference.md`
// §B-1's "值班機（kiosk）建議流程" table (device-type OOBE camp: macOS/
// Windows/ChromeOS/Steam Deck — DuDuClaw OS is device-type per that doc's own
// classification in §A row 7: "裝置型全做——DuDuClaw OS 屬裝置型"). Where the
// task brief itself already settled a judgment call (the Network step:
// blocking, no "later" escape hatch — see the brief's own parenthetical
// reasoning), that judgment is followed as-is. Where neither the brief nor the
// research doc says anything about skippability, the default is NOT
// skippable ("不確定就從嚴=不可跳"), and each `OobeStep` variant's own doc
// comment below names its citation (or the from-the-strict-default fallback)
// explicitly.
//
// ── Correction (this round): language leads, not input detection ─────────
// The FIRST implementation of this file put `InputDetection` at step 0 and
// `LanguageAccessibility` at step 1 — a literal transcription of §B-1's table
// ROW numbers (row 0 = input detection, row 1 = language). That was an
// implementation slip: it contradicts the SAME research doc's own strongest
// finding, §A consensus #1 ("語言第一屏，在一切帳號/授權之前" — 6/8, the
// loudest cross-OS agreement in the whole survey, ahead of even
// network/privacy) and §B-4 point 4 ("原生把語言鎖第一屏、確定後再渲染其餘
// 步驟；設計期就決定支不支援中途換語言" — explicitly named as a thing the
// web port must NOT get wrong). This round's task brief settles it in plain
// language ("OOBE 第一步應該是要先選語言，接下來才用選擇的語言去繪製下面的
// 頁面") and this file is corrected to match: `LanguageAccessibility` is now
// `OobeStep::ALL[0]`, `InputDetection` is `ALL[1]`. Every OTHER row's
// relative order from §B-1 is unchanged (Network still directly precedes
// Update, etc.) — only where language sits moved, from second to first.
//
// Pure `&mut self` mutation, no gpui types anywhere in THIS file — same
// "testable without a live window" discipline `surface.rs`'s header comment
// establishes for `SurfaceState`, extended here to also cover disk
// persistence (still plain data in, plain data out, no gpui).

mod claim;
mod fake_data;
mod palette;
mod render;
mod steps;
mod widgets;

/// `main.rs` needs `AccountFields` to create the `AccountCreate` step's two
/// real text-input entities at window-open time and store them on
/// `ShellView` — see that struct's own doc comment in `widgets.rs`. Same
/// re-export shape `pub use render::render;` below already establishes:
/// `widgets` itself stays a private module, only specific items are opened
/// up.
pub(crate) use widgets::AccountFields;

pub use render::render;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// The ten OOBE steps, in fixed linear order — no branching, matching
/// every device-type OS surveyed (§A: "誰選步驟｜系統定順序" is the
/// majority camp; ChromeOS's CHOOBE user-chosen-step-set is called out as
/// the sole exception, explicitly "獨家" — not adopted here). `Theme` (see
/// its own doc comment below) is a later addition — the original nine-step
/// set came straight from the §B-1 survey table, `Theme` did not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OobeStep {
    /// §B-1 row 1: "語言（zh-TW 預設高亮）＋同屏無障礙入口" — §A
    /// consensus #1 (language first, before any account/consent, 6/8, the
    /// STRONGEST agreement in the whole survey) + consensus #7
    /// (accessibility entry point in the first batch of screens, all
    /// device-type OSes). PROMOTED to `ALL[0]` this round (was `ALL[1]` —
    /// see this file's header comment for why the original literal-§B-1-row
    /// order was a correction-worthy slip): language selection gates every
    /// SUBSEQUENT screen's copy, so it has to render, and be answerable,
    /// before any of them — including `InputDetection`. Not skippable.
    /// Real i18n as of this round — see `steps::language`'s own header
    /// comment and `crate::i18n`.
    LanguageAccessibility,
    /// §B-1 row 0: "輸入裝置偵測（無鍵盤→觸控/遙控模式）" — ChromeOS's
    /// `hid_detection`. No longer `ALL[0]` (see this file's header comment)
    /// — still the first step AFTER language, unchanged relative to every
    /// other row. Not skippable: no citation offers a skip affordance for
    /// this step (from-the-strict-default), and it is a passive detection
    /// pass with no precondition either — Continue is always enabled.
    InputDetection,
    /// §B-1 step 2: "網路（未連網阻塞、明確 retry）" — §A consensus #2
    /// (network precedes account and blocks progress, cited against
    /// Windows 11 Home requiring network and ChromeOS blocking dead on no
    /// network). The task brief itself settles the "can this be deferred
    /// like RuntimeAuth?" question explicitly: NO — blocking, no "稍後再
    /// 說" escape hatch. Not in `is_skippable`'s allow-list.
    Network,
    /// §B-1 step 3: "系統更新檢查（自動）" — §A consensus #6 ("更新內建在
    /// OOBE 中段、關鍵更新不可拒", all three device-type OSes surveyed).
    /// Not skippable; this round's stub always resolves to "already up to
    /// date" so it never actually blocks `next()` either.
    Update,
    /// §B-1 step 4: "建立操作者帳號＋密碼（一步完成）" — macOS's own
    /// "唯一不可跳過的身分步驟" (§1 line 12), cited verbatim in §B-1's own
    /// row. Not skippable — the one mandatory identity step, and the
    /// structural fix for the bootstrap-admin two-phase WS-handshake
    /// deadlock incident §B-1 row 4 cites by name.
    ///
    /// Shell-S2 round 1 (2026-08-20) replaces this step's local-only click
    /// (round 1's `flow.set_account_created(true)` with no I/O at all) with
    /// a real call to the gateway's own `GET/POST /api/first-run/*` REST
    /// endpoints — see `oobe::claim`'s own header comment for the network
    /// layer and `steps::account`'s for how the click handler drives it.
    /// `account_created` (the field `can_advance` actually gates on, below)
    /// still only flips `true` on a server-confirmed outcome, never
    /// optimistically — the state machine's contract with `can_advance`
    /// hasn't changed, only how it gets satisfied.
    AccountCreate,
    /// §B-1 step 5: "AI runtime 授權（可『稍後設定』→明示降級）" —
    /// SKIPPABLE, explicit defer semantics (not a silent skip: choosing it
    /// records an acknowledged degraded mode, same two-tier vocabulary as
    /// macOS's "Not Now"/"Set Up Later", §1 "互動/排版/文案").
    RuntimeAuth,
    /// §B-1 step 6: "隱私/遙測獨立屏、預設全關" — §A consensus #4, the
    /// STRONGEST consensus in the whole survey ("5/8，無反例"). Not in the
    /// task brief's own skippable list (only RuntimeAuth and Templates are)
    /// — from-the-strict-default rule applies: this step is always
    /// visited, but every toggle on it defaults OFF, so continuing past it
    /// untouched is always the safe/expected path.
    Privacy,
    /// §B-1 step 7: "產業板模/AI 員工（可選可跳，預設『一鍵 CEO』
    /// express）" — SKIPPABLE, plus an explicit default express option
    /// (macOS's own "Make This Your New Mac" one-click precedent, §1 step
    /// 8; elementary's "non-essential setup must not block" HIG principle,
    /// §5).
    Templates,
    /// 外觀（亮/暗）— modeled on macOS's own "Choose Your Look" step in
    /// Setup Assistant, which that OS places near the END of the flow
    /// (after Apple ID/iCloud/Siri, immediately before "Get Started")
    /// rather than up front with language/region: appearance is a
    /// per-desktop preference, not an identity/consent gate, so — same
    /// reasoning applied here — it sits right after `Templates` (the last
    /// content-choice step) and immediately before `Finish`. NOT covered by
    /// `research/native-os-2026-08/oobe-first-run-reference.md` §B-1 (that
    /// survey table predates this step); placement + defaults are this
    /// crate's own 2026-08-20 task brief (拍板), cited here rather than to
    /// that doc. Not skippable (absent from `is_skippable`'s allow-list
    /// below) — but never actually blocking either: a default
    /// (`ThemeChoice::Light`, see that enum's own `#[default]`) is already
    /// selected on arrival, so Continue is never disabled here (`
    /// OobeFlow::can_advance` has no case for this step, same as every
    /// other non-blocking step).
    ///
    /// The hard product requirement this step exists to demonstrate: PICKING
    /// a theme here re-skins the REST of the OOBE surface live, on the very
    /// next frame — see `OobeFlow::palette()`'s own doc comment for how that
    /// actually happens (short version: every OOBE render call resolves the
    /// palette fresh from `selections().theme`, the same way `flow.locale()`
    /// already does for language — there is no separate "apply theme" step).
    Theme,
    /// §B-1 step 8: "完成屏 …＋quiet period" — terminal step. Continuing
    /// from here marks the whole flow `completed` (see `OobeFlow::next`).
    Finish,
}

impl OobeStep {
    pub const ALL: [OobeStep; 10] = [
        OobeStep::LanguageAccessibility,
        OobeStep::InputDetection,
        OobeStep::Network,
        OobeStep::Update,
        OobeStep::AccountCreate,
        OobeStep::RuntimeAuth,
        OobeStep::Privacy,
        OobeStep::Templates,
        OobeStep::Theme,
        OobeStep::Finish,
    ];

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|s| *s == self).expect("OobeStep::ALL is exhaustive over every variant")
    }

    pub fn from_index(index: usize) -> Option<Self> {
        Self::ALL.get(index).copied()
    }

    pub fn next(self) -> Option<Self> {
        Self::from_index(self.index() + 1)
    }

    pub fn prev(self) -> Option<Self> {
        self.index().checked_sub(1).and_then(Self::from_index)
    }

    /// Whether this step offers a "skip"/"defer" action — see each
    /// variant's own doc comment above for the citation backing its
    /// answer.
    pub fn is_skippable(self) -> bool {
        matches!(self, OobeStep::RuntimeAuth | OobeStep::Templates)
    }

    /// Stable machine-readable id — used by `DUDUCLAW_SHELL_DEBUG_OOBE_STEP`
    /// (see `from_debug_env`) and by log lines. Mirrors `Overlay::
    /// from_debug_env`'s slug convention in `surface.rs`.
    pub fn slug(self) -> &'static str {
        match self {
            OobeStep::InputDetection => "input-detection",
            OobeStep::LanguageAccessibility => "language",
            OobeStep::Network => "network",
            OobeStep::Update => "update",
            OobeStep::AccountCreate => "account",
            OobeStep::RuntimeAuth => "runtime-auth",
            OobeStep::Privacy => "privacy",
            OobeStep::Templates => "templates",
            OobeStep::Theme => "theme",
            OobeStep::Finish => "finish",
        }
    }

    /// Parses `DUDUCLAW_SHELL_DEBUG_OOBE_STEP`'s value — accepts either a
    /// numeric index (`0`..`8`) or a step slug (see `slug()`). Unrecognized
    /// input returns `None` and is ignored by the caller, never panics —
    /// same contract as `Overlay::from_debug_env`.
    pub fn from_debug_env(raw: &str) -> Option<Self> {
        let trimmed = raw.trim();
        if let Ok(n) = trimmed.parse::<usize>() {
            return Self::from_index(n);
        }
        Self::ALL.into_iter().find(|s| s.slug() == trimmed)
    }
}

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
        // reorder (see this file's header comment). NOT hardcoded as a
        // literal variant here on purpose: `OobeStep::from_index(0)` would
        // be the fully-decoupled spelling, but `Default` is meant to be the
        // OBVIOUS "what does a fresh install start on" answer at a glance,
        // and the `all_has_ten_steps_in_declared_order` test below already
        // pins `ALL[0]` to this exact variant — so a future reorder that
        // forgets to update this line fails loudly there, not silently here.
        Self { completed: false, current_step: OobeStep::LanguageAccessibility, selections: OobeSelections::default() }
    }
}

/// The pure state machine — no gpui types, see this module's header
/// comment. Owns one `OobeState` and exposes the only ways to mutate it
/// (`next`/`back`/`skip`/the `set_*` selection setters), so every rendered
/// screen and every test goes through the same gate.
#[derive(Debug, Clone, PartialEq)]
pub struct OobeFlow {
    state: OobeState,
}

impl OobeFlow {
    pub fn new() -> Self {
        Self { state: OobeState::default() }
    }

    pub fn from_state(state: OobeState) -> Self {
        Self { state }
    }

    pub fn state(&self) -> &OobeState {
        &self.state
    }

    pub fn current(&self) -> OobeStep {
        self.state.current_step
    }

    pub fn completed(&self) -> bool {
        self.state.completed
    }

    pub fn selections(&self) -> &OobeSelections {
        &self.state.selections
    }

    /// The catalog every step's render fn draws its copy from — a thin
    /// wrapper over `LanguageChoice::to_locale()` so call sites read `flow.
    /// locale()` rather than `flow.selections().language.to_locale()`
    /// everywhere (`crate::i18n`'s own header comment explains why the
    /// conversion itself lives on `LanguageChoice`, not here). `Locale` is a
    /// plain enum with zero gpui dependency, so exposing it here doesn't
    /// break this file's "no gpui types anywhere in THIS file" discipline
    /// (see the header comment).
    pub fn locale(&self) -> crate::i18n::Locale {
        self.state.selections.language.to_locale()
    }

    /// Whether `next()` would succeed right now, without mutating —
    /// `Network`/`AccountCreate` are the only two blocking preconditions
    /// (see their own `OobeStep` doc comments); every other step has none.
    pub fn can_advance(&self) -> bool {
        match self.state.current_step {
            OobeStep::Network => self.state.selections.network_connected,
            OobeStep::AccountCreate => self.state.selections.account_created,
            _ => true,
        }
    }

    /// Advances to the next step, or marks the flow `completed` when
    /// called from `Finish`. No-op (returns `false`, no mutation) if a
    /// blocking precondition isn't met, or if the flow is already
    /// completed.
    pub fn next(&mut self) -> bool {
        if self.state.completed || !self.can_advance() {
            return false;
        }
        match self.state.current_step.next() {
            Some(step) => self.state.current_step = step,
            None => self.state.completed = true,
        }
        true
    }

    /// Escape's handler within OOBE (task brief: "Escape=返回（第一步不可
    ///返回）"). Always allowed regardless of the CURRENT step's own
    /// `can_advance()` — a blocked step must still be able to go back, see
    /// the `back_from_a_blocked_step_still_works` test below. The first
    /// step (task brief item 1 reorder: `LanguageAccessibility`, no longer
    /// `InputDetection`) has no `back` at all.
    pub fn back(&mut self) -> bool {
        if self.state.current_step == OobeStep::LanguageAccessibility {
            return false;
        }
        match self.state.current_step.prev() {
            Some(step) => {
                self.state.current_step = step;
                true
            }
            None => false,
        }
    }

    /// Skips the current step, if skippable — bypasses `can_advance()`
    /// entirely (skip is its own escape hatch, not gated by the same
    /// precondition it exists to route around). No-op on a non-skippable
    /// step or once completed.
    pub fn skip(&mut self) -> bool {
        if self.state.completed || !self.state.current_step.is_skippable() {
            return false;
        }
        match self.state.current_step {
            OobeStep::RuntimeAuth => self.state.selections.runtime_deferred = true,
            OobeStep::Templates => self.state.selections.template_choice = Some(TemplateChoice::Skipped),
            _ => {}
        }
        match self.state.current_step.next() {
            Some(step) => self.state.current_step = step,
            None => self.state.completed = true,
        }
        true
    }

    pub fn set_language(&mut self, language: LanguageChoice) {
        self.state.selections.language = language;
    }

    pub fn set_network(&mut self, ssid: &str, connected: bool) {
        self.state.selections.network_ssid = Some(ssid.to_string());
        self.state.selections.network_connected = connected;
    }

    pub fn set_account_created(&mut self, created: bool) {
        self.state.selections.account_created = created;
    }

    /// Records the `AccountCreate` step's typed operator name — LOCAL
    /// DISPLAY ONLY, see `OobeSelections::operator_name`'s own doc comment
    /// for why this never reaches the gateway. Called at click time
    /// regardless of whether the network claim itself succeeds (a typed
    /// name is worth remembering across a retry even if the first attempt
    /// failed to reach the gateway) — same "click records, a later step
    /// advances" split every other setter here follows.
    pub fn set_operator_name(&mut self, name: &str) {
        self.state.selections.operator_name = Some(name.to_string());
    }

    /// `RuntimeAuth`'s "立即設定" path — the counterpart to `skip()`'s
    /// "稍後再說" path (`runtime_deferred`) for this same step. Unlike
    /// `skip()`, this does NOT advance the step on its own: clicking "立即
    /// 設定" records the choice and the operator still confirms via the
    /// normal bottom-nav "繼續" — same click-records/continue-advances split
    /// `set_account_created`/`set_network` already establish for their own
    /// steps.
    pub fn set_runtime_authorized(&mut self, authorized: bool) {
        self.state.selections.runtime_authorized = authorized;
    }

    pub fn privacy_toggle_on(&self, toggle: PrivacyToggle) -> bool {
        match toggle {
            PrivacyToggle::UsageStats => self.state.selections.privacy_usage_stats,
            PrivacyToggle::ErrorReports => self.state.selections.privacy_error_reports,
            PrivacyToggle::Personalization => self.state.selections.privacy_personalization,
            PrivacyToggle::Marketing => self.state.selections.privacy_marketing,
        }
    }

    pub fn toggle_privacy(&mut self, toggle: PrivacyToggle) {
        let field = match toggle {
            PrivacyToggle::UsageStats => &mut self.state.selections.privacy_usage_stats,
            PrivacyToggle::ErrorReports => &mut self.state.selections.privacy_error_reports,
            PrivacyToggle::Personalization => &mut self.state.selections.privacy_personalization,
            PrivacyToggle::Marketing => &mut self.state.selections.privacy_marketing,
        };
        *field = !*field;
    }

    /// `Templates`' click-to-select path (Express card or one of the fake
    /// custom cards) — same click-records/continue-advances split as
    /// `set_runtime_authorized` above. `skip()` (the bottom-nav "略過"
    /// button, or the in-card skip link) always OVERWRITES whatever was
    /// tentatively selected here with `TemplateChoice::Skipped` — skipping
    /// means skipping, not "keep my tentative pick but don't apply it".
    pub fn set_template_choice(&mut self, choice: TemplateChoice) {
        self.state.selections.template_choice = Some(choice);
    }

    /// `Theme`'s click-to-select path — same click-records/continue-advances
    /// split every other setter on this type establishes (`set_language`,
    /// `set_runtime_authorized`, `set_template_choice`, …): picking a card
    /// records the choice immediately (so the retheme in the next paragraph
    /// fires without waiting for Continue), the step itself still advances
    /// only through the normal bottom-nav "繼續".
    pub fn set_theme(&mut self, theme: ThemeChoice) {
        self.state.selections.theme = theme;
    }

    /// The color token set the WHOLE OOBE surface renders through as of
    /// this step. `render.rs`'s frame, every `steps::*::render` fn, and
    /// `widgets.rs`'s helpers all call this fresh on every render pass —
    /// mirroring the existing `locale()` convention just above (recomputed
    /// per call from `selections()`, never cached on `self`) — rather than a
    /// separate parameter threaded through the whole render tree, so a click
    /// on the `Theme` step's card retheme's every OTHER OOBE screen on the
    /// very next frame for free: there is no distinct "apply theme" action,
    /// picking IS applying.
    ///
    /// `palette::OobePalette` is plain data (u32 hex + one precomposited
    /// `Rgba` + a couple of derived-shadow/border methods) defined in a
    /// SIBLING module of this one specifically so it can lean on gpui types
    /// internally (see `palette.rs`'s own header comment) — this method only
    /// NAMES that type in its signature, it does not import or construct a
    /// gpui value itself, so this file's "no gpui types anywhere in THIS
    /// file" discipline (see the header comment) still holds at the level
    /// that discipline is actually about: no gpui CODE runs here.
    ///
    /// No `pub` modifier (module-private, same default-visibility rule
    /// `fake_data`/`palette` themselves rely on) — this is only ever called
    /// from `render.rs`/`steps/*.rs`, both descendants of `oobe`, which
    /// already see a bare `fn` here; a `pub(crate)` annotation would widen
    /// this method's own reach past `OobePalette`'s `pub(super)` return
    /// type and trip rustc's private-interface lint for no actual caller.
    fn palette(&self) -> palette::OobePalette {
        palette::OobePalette::for_choice(self.state.selections.theme)
    }
}

impl Default for OobeFlow {
    fn default() -> Self {
        Self::new()
    }
}

/// The `AccountCreate` step's real-time gateway-claim progress (Shell-S2
/// round 1) — driven by `steps::account`'s click handler +
/// `oobe::claim::create_account` (see that module's own header comment for
/// the network layer this wraps). Deliberately separate from
/// `OobeSelections::account_created` (the flow-advance authority
/// `OobeFlow::can_advance` reads): `account_created` only ever flips `true`
/// on an actual server-confirmed outcome (`Claimed`/`AlreadyClaimed`), never
/// on `InFlight` — a mid-flight restart (or a request that never resolves)
/// can never leave the flow able to advance past a claim that never actually
/// completed, because this whole enum is ephemeral (not part of
/// `OobeState`/persistence — same split every other field on `OobeUiState`
/// already follows) and simply reverts to `Idle` on the next launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AccountClaimState {
    #[default]
    Idle,
    /// A claim request is in flight — `steps::account`'s render fn shows a
    /// "建立中…" button label and disables further clicks while this holds
    /// (see that module's own click handler for the guard).
    InFlight,
    /// The gateway confirmed the account — either THIS click set the
    /// password (`already: false`) or the instance was already set up by an
    /// earlier run (`already: true`, `oobe::claim::ClaimOutcome::
    /// AlreadyClaimed`). Both set `account_created = true`; only `already`
    /// changes which message `steps::account` renders.
    Done { already: bool },
    /// The claim did not resolve to either `Idle` above — see
    /// `AccountClaimFailureKind`'s own doc comment for which of the two
    /// operator-facing messages this maps to. Deliberately NOT reset back to
    /// `Idle` automatically: the whole point of landing here is so the error
    /// message stays on screen until the operator's next action (either a
    /// fresh validation failure or a fresh submit attempt) replaces it — see
    /// `steps::account`'s click handler.
    Failed(AccountClaimFailureKind),
}

/// Which message `steps::account`'s render fn shows for a `Failed` claim —
/// collapses `oobe::claim::ClaimError`'s five network-layer variants down to
/// the two an OPERATOR actually needs to act on differently: "you typed a
/// password the gateway will reject, fix it and resubmit" vs. "something
/// about reaching the local service went wrong, just retry". Which of
/// `Unreachable`/`Http`/`Malformed`/`NonLoopback` actually happened is
/// diagnostic detail logged to stderr at the call site (`steps::account`'s
/// `apply_claim_result`), not something the OOBE surface needs to render
/// three different ways — the operator's retry action is identical either
/// way (click "建立帳號" again).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountClaimFailureKind {
    /// The gateway rejected the password as too short, OR the client-side
    /// pre-check (`steps::account`, mirroring the gateway's own `< 8 chars`
    /// rule) caught it before ever dispatching a request — either path lands
    /// here, so the render side doesn't need to know which one happened.
    PasswordTooShort,
    /// Couldn't complete the round trip, or the gateway answered with
    /// something this module doesn't have specific handling for — see this
    /// enum's own doc comment for why these all collapse to one message.
    Unreachable,
}

/// Ephemeral view-only UI state — NOT part of `OobeState`/persistence (same
/// split `overlay::OverlayUiState` establishes vs. `surface::SurfaceState`,
/// applied here for OOBE instead of the overlay surfaces). Reset on every
/// process launch; nothing here needs to survive a restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OobeUiState {
    /// Whether the `LanguageAccessibility` step's inline "輔助使用設定"
    /// entry is expanded — task brief: "無障礙入口（視覺入口，點開佔位）".
    pub accessibility_open: bool,
    /// `AccountCreate`'s "建立帳號" click validates both real `OobeTextField`
    /// entries at CLICK time (`this.field.read(cx).content`, same pattern
    /// `duduclaw-native-gui/src/screens/login.rs`'s own submit handler
    /// already uses for its email/password fields) rather than disabling
    /// the button ahead of time from live typed content — disabling would
    /// need the parent `ShellView` to re-render on every keystroke inside a
    /// CHILD entity, which nothing here subscribes to (see `steps/
    /// account.rs`'s own header comment). Set `true` when a click found
    /// either field empty; cleared on the next successful click or a fresh
    /// visit to the step. Ephemeral, like `accessibility_open` above — not
    /// worth persisting across a restart.
    pub account_validation_error: bool,
    /// The `AccountCreate` step's gateway-claim progress — see
    /// `AccountClaimState`'s own doc comment. Also ephemeral, for the same
    /// reason `account_validation_error` above is: a page reload/restart
    /// mid-flight just shows `Idle` again and the operator re-clicks, which
    /// is harmless (the gateway's own claim endpoint is single-shot but
    /// idempotent-FROM-THE-CLIENT'S-VIEW: a retry after a real success just
    /// reports `AlreadyClaimed`, never a silent double-charge of anything).
    pub account_claim: AccountClaimState,
}

impl OobeUiState {
    pub fn toggle_accessibility(&mut self) {
        self.accessibility_open = !self.accessibility_open;
    }

    pub fn set_account_validation_error(&mut self, on: bool) {
        self.account_validation_error = on;
    }

    pub fn set_account_claim_in_flight(&mut self) {
        self.account_claim = AccountClaimState::InFlight;
    }

    pub fn set_account_claim_done(&mut self, already: bool) {
        self.account_claim = AccountClaimState::Done { already };
    }

    pub fn set_account_claim_failed(&mut self, kind: AccountClaimFailureKind) {
        self.account_claim = AccountClaimState::Failed(kind);
    }

    /// Clears back to `Idle` — called when a fresh validation error (empty
    /// name/password) supersedes whatever claim-related message was
    /// showing, so the two message sources (`account_validation_error` vs.
    /// `account_claim`) never render on top of each other. See
    /// `steps::account`'s click handler for the one call site.
    pub fn reset_account_claim(&mut self) {
        self.account_claim = AccountClaimState::Idle;
    }
}

// ── Persistence ────────────────────────────────────────────────────────

const STATE_SUBDIR: &str = "shell";
const STATE_FILE_NAME: &str = "oobe_state.json";

/// Home resolution intentionally mirrors `duduclaw-core::platform::
/// duduclaw_home` (`$DUDUCLAW_HOME` verbatim when set and non-empty, else
/// `$HOME`/`$USERPROFILE` + `/.duduclaw`) — hand-duplicated, not linked,
/// same reasoning `duduclaw-native-gui/src/config.rs`'s own `config_path()`
/// doc comment gives for why THAT crate doesn't pull in `duduclaw-core` for
/// one path function: this crate deliberately excludes itself from the root
/// workspace to keep gpui's dependency tree away from the gateway build.
fn duduclaw_home() -> Option<PathBuf> {
    match std::env::var("DUDUCLAW_HOME") {
        Ok(custom) if !custom.trim().is_empty() => Some(PathBuf::from(custom)),
        _ => {
            let home_dir = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).ok()?;
            if home_dir.trim().is_empty() {
                return None;
            }
            Some(PathBuf::from(home_dir).join(".duduclaw"))
        }
    }
}

/// `<duduclaw home>/shell/oobe_state.json` (task brief: "落
/// `$DUDUCLAW_HOME`... `shell/` 下").
pub fn state_path() -> Option<PathBuf> {
    duduclaw_home().map(|home| home.join(STATE_SUBDIR).join(STATE_FILE_NAME))
}

/// `None`/unreadable/corrupt file all degrade to `OobeState::default()` —
/// same fail-open contract `duduclaw-native-gui/src/config.rs`'s
/// `load_locale` establishes ("a missing/corrupt config file... degrades to
/// ... exactly as if this were a first launch").
pub fn load_state() -> OobeState {
    state_path().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|content| serde_json::from_str(&content).ok()).unwrap_or_default()
}

/// Best-effort atomic write (temp file + rename, task brief: "寫入用
/// temp+rename 原子寫") — survives a mid-write power loss on a kiosk device
/// without corrupting the state file (the whole point of the "斷電續步"
/// requirement: a torn/partial JSON write must never brick the next boot's
/// `load_state()`). Any failure logs to stderr and returns; losing the
/// "resume at the same step" nicety is never worth blocking the OOBE UI
/// thread on a disk error.
pub fn save_state(state: &OobeState) {
    let Some(path) = state_path() else {
        eprintln!("[oobe] could not resolve a home directory (no $DUDUCLAW_HOME/$HOME/$USERPROFILE) — state will not persist");
        return;
    };
    let Some(dir) = path.parent() else {
        return;
    };
    if let Err(e) = std::fs::create_dir_all(dir) {
        eprintln!("[oobe] could not create {}: {e}", dir.display());
        return;
    }
    let content = match serde_json::to_string_pretty(state) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[oobe] could not serialize state: {e}");
            return;
        }
    };
    let tmp_path = dir.join(format!("{STATE_FILE_NAME}.tmp"));
    if let Err(e) = std::fs::write(&tmp_path, content) {
        eprintln!("[oobe] could not write temp state file {}: {e}", tmp_path.display());
        return;
    }
    if let Err(e) = std::fs::rename(&tmp_path, &path) {
        eprintln!("[oobe] could not rename temp state file into place: {e}");
    }
}

// ── Boot-entry resolution ─────────────────────────────────────────────

/// Resolves what the shell should show at boot — `Some(flow)` to open OOBE
/// immediately in that state, `None` to go straight to Home. Pure function
/// over already-read env values + the persisted state (never reads env or
/// disk itself), so the priority rules (task brief: "兩者都設時 FORCE
/// 優先") are unit-testable without touching real env vars or disk — see
/// `main.rs` for where the env vars actually get read.
///
/// Priority: `DUDUCLAW_SHELL_FORCE_OOBE=1` > `DUDUCLAW_SHELL_SKIP_OOBE=1` >
/// a recognized `DUDUCLAW_SHELL_DEBUG_OOBE_STEP` > the persisted state's own
/// `completed` flag. FORCE and DEBUG_STEP both ignore `persisted.completed`
/// (that's the entire point of a debug/force override); DEBUG_STEP also
/// ignores `persisted.current_step` (jumps straight to the requested step)
/// but keeps `persisted.selections` so earlier fake choices are still
/// visible when jumping ahead.
pub fn resolve_boot_flow(force: Option<&str>, skip: Option<&str>, debug_step: Option<&str>, persisted: OobeState) -> Option<OobeFlow> {
    if force.is_some_and(|v| v == "1") {
        return Some(OobeFlow::from_state(persisted));
    }
    if skip.is_some_and(|v| v == "1") {
        return None;
    }
    if let Some(raw) = debug_step {
        if let Some(step) = OobeStep::from_debug_env(raw) {
            let mut state = persisted;
            state.completed = false;
            state.current_step = step;
            return Some(OobeFlow::from_state(state));
        }
    }
    if persisted.completed {
        None
    } else {
        Some(OobeFlow::from_state(persisted))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // ── step ordering / indices ──────────────────────────────────────

    #[test]
    fn all_has_ten_steps_in_declared_order() {
        assert_eq!(OobeStep::ALL.len(), 10);
        // Task brief item 1: language leads (was `ALL[1]`), input detection
        // follows (was `ALL[0]`) — see this file's header comment.
        assert_eq!(OobeStep::ALL[0], OobeStep::LanguageAccessibility);
        assert_eq!(OobeStep::ALL[1], OobeStep::InputDetection);
        // `Theme` inserted between `Templates` and `Finish` — see
        // `OobeStep::Theme`'s own doc comment.
        assert_eq!(OobeStep::ALL[7], OobeStep::Templates);
        assert_eq!(OobeStep::ALL[8], OobeStep::Theme);
        assert_eq!(OobeStep::ALL[9], OobeStep::Finish);
    }

    #[test]
    fn index_and_from_index_round_trip_for_every_step() {
        for step in OobeStep::ALL {
            assert_eq!(OobeStep::from_index(step.index()), Some(step));
        }
    }

    #[test]
    fn next_walks_forward_through_all_ten_steps_in_declared_order() {
        let mut step = OobeStep::LanguageAccessibility;
        let mut seen = vec![step];
        while let Some(n) = step.next() {
            seen.push(n);
            step = n;
        }
        assert_eq!(seen, OobeStep::ALL.to_vec());
    }

    #[test]
    fn finish_has_no_next_step() {
        assert_eq!(OobeStep::Finish.next(), None);
    }

    #[test]
    fn language_accessibility_has_no_prev_step() {
        // Was `input_detection_has_no_prev_step` before the reorder —
        // `LanguageAccessibility` is `ALL[0]` now, so IT has no `prev()`;
        // `InputDetection` (now `ALL[1]`) DOES, see the next test.
        assert_eq!(OobeStep::LanguageAccessibility.prev(), None);
    }

    #[test]
    fn input_detection_prev_is_language_accessibility() {
        assert_eq!(OobeStep::InputDetection.prev(), Some(OobeStep::LanguageAccessibility));
    }

    #[test]
    fn slug_and_from_debug_env_round_trip_for_every_step() {
        for step in OobeStep::ALL {
            assert_eq!(OobeStep::from_debug_env(step.slug()), Some(step));
        }
    }

    #[test]
    fn from_debug_env_accepts_numeric_index() {
        assert_eq!(OobeStep::from_debug_env("0"), Some(OobeStep::LanguageAccessibility));
        assert_eq!(OobeStep::from_debug_env("1"), Some(OobeStep::InputDetection));
        assert_eq!(OobeStep::from_debug_env("4"), Some(OobeStep::AccountCreate));
        assert_eq!(OobeStep::from_debug_env("8"), Some(OobeStep::Theme));
        assert_eq!(OobeStep::from_debug_env("9"), Some(OobeStep::Finish));
    }

    #[test]
    fn from_debug_env_rejects_out_of_range_and_garbage() {
        assert_eq!(OobeStep::from_debug_env("10"), None);
        assert_eq!(OobeStep::from_debug_env("bogus"), None);
        assert_eq!(OobeStep::from_debug_env(""), None);
    }

    // ── skippability, per-step citation (see each variant's doc comment) ──

    #[test]
    fn only_runtime_auth_and_templates_are_skippable() {
        for step in OobeStep::ALL {
            let expected = matches!(step, OobeStep::RuntimeAuth | OobeStep::Templates);
            assert_eq!(step.is_skippable(), expected, "{step:?}");
        }
    }

    // ── OobeFlow: next / back / skip / complete ────────────────────────

    #[test]
    fn starts_at_language_accessibility_not_completed() {
        let flow = OobeFlow::new();
        assert_eq!(flow.current(), OobeStep::LanguageAccessibility);
        assert!(!flow.completed());
    }

    #[test]
    fn next_advances_through_steps_with_no_precondition() {
        let mut flow = OobeFlow::new();
        assert!(flow.next());
        assert_eq!(flow.current(), OobeStep::InputDetection);
    }

    #[test]
    fn back_from_second_step_returns_to_first() {
        let mut flow = OobeFlow::new();
        flow.next();
        assert!(flow.back());
        assert_eq!(flow.current(), OobeStep::LanguageAccessibility);
    }

    #[test]
    fn back_on_the_first_step_is_a_noop_not_a_panic() {
        let mut flow = OobeFlow::new();
        assert!(!flow.back());
        assert_eq!(flow.current(), OobeStep::LanguageAccessibility);
    }

    #[test]
    fn network_step_blocks_next_until_connected() {
        let mut flow = OobeFlow::new();
        flow.next(); // Language -> InputDetection
        flow.next(); // InputDetection -> Network
        assert_eq!(flow.current(), OobeStep::Network);
        assert!(!flow.can_advance());
        assert!(!flow.next());
        assert_eq!(flow.current(), OobeStep::Network, "must not advance without a connection");
        flow.set_network("DuDu-Office", true);
        assert!(flow.can_advance());
        assert!(flow.next());
        assert_eq!(flow.current(), OobeStep::Update);
    }

    #[test]
    fn network_step_has_no_defer_escape_hatch() {
        // Task brief settles this explicitly: unlike RuntimeAuth, Network
        // offers no "later" path — it is simply not skippable.
        let mut flow = OobeFlow::new();
        flow.next(); // Language -> InputDetection
        flow.next(); // InputDetection -> Network
        assert_eq!(flow.current(), OobeStep::Network);
        assert!(!flow.current().is_skippable());
        assert!(!flow.skip());
        assert_eq!(flow.current(), OobeStep::Network);
    }

    #[test]
    fn back_from_a_blocked_step_still_works() {
        let mut flow = OobeFlow::new();
        flow.next(); // Language -> InputDetection
        flow.next(); // InputDetection -> Network
        assert_eq!(flow.current(), OobeStep::Network);
        assert!(!flow.can_advance());
        assert!(flow.back());
        assert_eq!(flow.current(), OobeStep::InputDetection);
    }

    #[test]
    fn account_step_blocks_next_until_created() {
        let mut flow = OobeFlow::new();
        while flow.current() != OobeStep::AccountCreate {
            if flow.current() == OobeStep::Network {
                flow.set_network("DuDu-Office", true);
            }
            flow.next();
        }
        assert!(!flow.next());
        flow.set_account_created(true);
        assert!(flow.next());
        assert_eq!(flow.current(), OobeStep::RuntimeAuth);
    }

    #[test]
    fn skip_on_a_non_skippable_step_is_a_noop() {
        let mut flow = OobeFlow::new();
        assert!(!flow.skip());
        assert_eq!(flow.current(), OobeStep::LanguageAccessibility);
    }

    #[test]
    fn skip_on_runtime_auth_advances_and_records_deferral() {
        let mut flow = runtime_auth_flow();
        assert_eq!(flow.current(), OobeStep::RuntimeAuth);
        assert!(!flow.selections().runtime_deferred);
        assert!(flow.skip());
        assert_eq!(flow.current(), OobeStep::Privacy);
        assert!(flow.selections().runtime_deferred);
    }

    #[test]
    fn skip_on_templates_advances_and_records_skipped_choice() {
        let mut flow = templates_flow();
        assert!(flow.skip());
        assert_eq!(flow.current(), OobeStep::Theme, "Theme now sits between Templates and Finish");
        assert_eq!(flow.selections().template_choice, Some(TemplateChoice::Skipped));
    }

    #[test]
    fn next_on_finish_completes_the_flow() {
        let mut flow = finish_flow();
        assert!(!flow.completed());
        assert!(flow.next());
        assert!(flow.completed());
        assert_eq!(flow.current(), OobeStep::Finish, "current step stays put once completed");
    }

    #[test]
    fn next_is_a_noop_once_completed() {
        let mut flow = finish_flow();
        flow.next();
        assert!(flow.completed());
        assert!(!flow.next());
    }

    #[test]
    fn skip_is_a_noop_once_completed() {
        let mut flow = finish_flow();
        flow.next();
        assert!(flow.completed());
        assert!(!flow.skip());
    }

    // ── round 2: RuntimeAuth authorize / Privacy toggles / Templates ──

    #[test]
    fn set_runtime_authorized_records_the_choice_without_advancing() {
        let mut flow = runtime_auth_flow();
        assert!(!flow.selections().runtime_authorized);
        flow.set_runtime_authorized(true);
        assert!(flow.selections().runtime_authorized);
        assert_eq!(flow.current(), OobeStep::RuntimeAuth, "click-to-record must not itself advance — same split as set_account_created/set_network");
    }

    #[test]
    fn all_privacy_toggles_default_off() {
        // §A consensus #4, the strongest in the whole survey ("5/8，無反
        //例") — every toggle must start OFF, no exceptions.
        let flow = OobeFlow::new();
        for toggle in PrivacyToggle::ALL {
            assert!(!flow.privacy_toggle_on(toggle), "{toggle:?} must default OFF");
        }
    }

    #[test]
    fn toggle_privacy_flips_only_the_targeted_toggle() {
        let mut flow = OobeFlow::new();
        flow.toggle_privacy(PrivacyToggle::ErrorReports);
        assert!(flow.privacy_toggle_on(PrivacyToggle::ErrorReports));
        for toggle in PrivacyToggle::ALL {
            if toggle != PrivacyToggle::ErrorReports {
                assert!(!flow.privacy_toggle_on(toggle), "{toggle:?} must stay untouched");
            }
        }
        flow.toggle_privacy(PrivacyToggle::ErrorReports);
        assert!(!flow.privacy_toggle_on(PrivacyToggle::ErrorReports), "toggling twice returns to OFF");
    }

    #[test]
    fn set_template_choice_records_express_and_custom() {
        let mut flow = templates_flow();
        flow.set_template_choice(TemplateChoice::Express);
        assert_eq!(flow.selections().template_choice, Some(TemplateChoice::Express));
        flow.set_template_choice(TemplateChoice::Custom("retail".to_string()));
        assert_eq!(flow.selections().template_choice, Some(TemplateChoice::Custom("retail".to_string())));
        assert_eq!(flow.current(), OobeStep::Templates, "click-to-record must not itself advance");
    }

    #[test]
    fn skip_on_templates_overwrites_a_tentative_selection() {
        let mut flow = templates_flow();
        flow.set_template_choice(TemplateChoice::Custom("clinic".to_string()));
        assert!(flow.skip());
        assert_eq!(flow.selections().template_choice, Some(TemplateChoice::Skipped), "skip must win over a tentative pick, not merge with it");
    }

    // ── Theme (new step, between Templates and Finish) ────────────────

    #[test]
    fn theme_step_is_not_skippable() {
        assert!(!OobeStep::Theme.is_skippable());
    }

    #[test]
    fn theme_defaults_to_light_before_any_pick() {
        // §… task brief: "有預設亮色" — `ThemeChoice::default()` must already
        // be what the very first frame shows as selected, same relationship
        // `LanguageChoice::default()` has to `LanguageAccessibility`.
        let flow = OobeFlow::new();
        assert_eq!(flow.selections().theme, ThemeChoice::Light);
    }

    #[test]
    fn set_theme_records_the_choice_without_advancing() {
        let mut flow = theme_flow();
        assert_eq!(flow.current(), OobeStep::Theme);
        flow.set_theme(ThemeChoice::Dark);
        assert_eq!(flow.selections().theme, ThemeChoice::Dark);
        assert_eq!(flow.current(), OobeStep::Theme, "click-to-record must not itself advance — same split as every other click-to-record setter");
    }

    #[test]
    fn theme_step_never_blocks_continue() {
        // A default selection already exists on arrival, so — unlike
        // Network/AccountCreate — `can_advance()` must be `true` here even
        // before any click.
        let flow = theme_flow();
        assert!(flow.can_advance());
    }

    #[test]
    fn continue_from_theme_advances_to_finish() {
        let mut flow = theme_flow();
        assert!(flow.next());
        assert_eq!(flow.current(), OobeStep::Finish);
    }

    #[test]
    fn theme_choice_serializes_kebab_case() {
        assert_eq!(serde_json::to_string(&ThemeChoice::Light).unwrap(), "\"light\"");
        assert_eq!(serde_json::to_string(&ThemeChoice::Dark).unwrap(), "\"dark\"");
    }

    // ── helpers: walk to a given step, satisfying preconditions ──────

    fn runtime_auth_flow() -> OobeFlow {
        let mut flow = OobeFlow::new();
        while flow.current() != OobeStep::RuntimeAuth {
            match flow.current() {
                OobeStep::Network => flow.set_network("DuDu-Office", true),
                OobeStep::AccountCreate => flow.set_account_created(true),
                _ => {}
            }
            flow.next();
        }
        flow
    }

    fn templates_flow() -> OobeFlow {
        let mut flow = runtime_auth_flow();
        flow.skip(); // RuntimeAuth -> Privacy
        flow.next(); // Privacy -> Templates
        flow
    }

    fn theme_flow() -> OobeFlow {
        let mut flow = templates_flow();
        flow.skip(); // Templates -> Theme
        flow
    }

    fn finish_flow() -> OobeFlow {
        let mut flow = theme_flow();
        flow.next(); // Theme -> Finish
        flow
    }

    // ── persistence ────────────────────────────────────────────────

    // `std::env::set_var`/`remove_var` are `unsafe` (stdlib requires it
    // regardless of edition on this toolchain) and process-global — these
    // tests serialize via ENV_LOCK and always restore the prior value, same
    // discipline `duduclaw-core/src/platform.rs`'s own `home_tests` module
    // establishes for its `DUDUCLAW_HOME`-mutating tests.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn save_then_load_round_trips_the_full_state() {
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = std::env::temp_dir().join(format!("duduclaw-shell-oobe-test-{}", std::process::id()));
        let prev = std::env::var("DUDUCLAW_HOME").ok();
        unsafe { std::env::set_var("DUDUCLAW_HOME", &tmp) };

        let mut flow = OobeFlow::new();
        flow.set_language(LanguageChoice::En);
        flow.next();
        flow.set_network("DuDu-Office", true);
        flow.next();
        save_state(flow.state());

        let loaded = load_state();

        unsafe {
            match prev {
                Some(v) => std::env::set_var("DUDUCLAW_HOME", v),
                None => std::env::remove_var("DUDUCLAW_HOME"),
            }
        }
        let _ = std::fs::remove_dir_all(&tmp);

        assert_eq!(loaded.current_step, OobeStep::Network);
        assert_eq!(loaded.selections.language, LanguageChoice::En);
        assert!(loaded.selections.network_connected);
        assert_eq!(loaded.selections.network_ssid.as_deref(), Some("DuDu-Office"));
        assert!(!loaded.completed);
    }

    #[test]
    fn load_state_with_no_file_is_the_default() {
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = std::env::temp_dir().join(format!("duduclaw-shell-oobe-test-empty-{}", std::process::id()));
        let prev = std::env::var("DUDUCLAW_HOME").ok();
        unsafe { std::env::set_var("DUDUCLAW_HOME", &tmp) };

        let loaded = load_state();

        unsafe {
            match prev {
                Some(v) => std::env::set_var("DUDUCLAW_HOME", v),
                None => std::env::remove_var("DUDUCLAW_HOME"),
            }
        }

        assert_eq!(loaded, OobeState::default());
    }

    // ── round 2: forward-compat hardening (task brief §C) ────────────

    #[test]
    fn oobe_selections_round_trips_every_new_field() {
        let mut selections = OobeSelections {
            runtime_authorized: true,
            privacy_usage_stats: true,
            privacy_marketing: true,
            theme: ThemeChoice::Dark,
            operator_name: Some("Louis".to_string()),
            ..OobeSelections::default()
        };
        selections.template_choice = Some(TemplateChoice::Custom("retail".to_string()));

        let json = serde_json::to_string(&selections).expect("serialize");
        let back: OobeSelections = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, selections);
    }

    #[test]
    fn load_state_tolerates_missing_new_fields_from_an_older_schema() {
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = std::env::temp_dir().join(format!("duduclaw-shell-oobe-test-oldschema-{}", std::process::id()));
        let prev = std::env::var("DUDUCLAW_HOME").ok();
        unsafe { std::env::set_var("DUDUCLAW_HOME", &tmp) };

        // Hand-written JSON shaped like an OLDER binary's `oobe_state.json`
        // — has `current_step`/`completed`/the five `selections` keys round
        // 1 shipped with, but predates every field round 2 adds
        // (`runtime_authorized`, the four `privacy_*` toggles). Task brief:
        // "舊檔缺新欄位時逐欄回預設而非整檔 fail-open 重置到步 0" — this must
        // NOT collapse to `OobeState::default()` (step 0); it must load
        // with `current_step` preserved and only the missing fields
        // defaulted.
        let old_schema = r#"{
            "completed": false,
            "current_step": "privacy",
            "selections": {
                "language": "zh-tw",
                "network_connected": true,
                "network_ssid": "DuDu-Office",
                "account_created": true,
                "runtime_deferred": false,
                "template_choice": null
            }
        }"#;
        let dir = tmp.join("shell");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("oobe_state.json"), old_schema).unwrap();

        let loaded = load_state();

        unsafe {
            match prev {
                Some(v) => std::env::set_var("DUDUCLAW_HOME", v),
                None => std::env::remove_var("DUDUCLAW_HOME"),
            }
        }
        let _ = std::fs::remove_dir_all(&tmp);

        assert_eq!(loaded.current_step, OobeStep::Privacy, "must NOT fall back to step 0 just because new fields are missing");
        assert!(!loaded.completed);
        assert!(loaded.selections.network_connected);
        assert_eq!(loaded.selections.network_ssid.as_deref(), Some("DuDu-Office"));
        assert!(loaded.selections.account_created);
        // Every field this round adds must default in, not error the whole
        // file out.
        assert!(!loaded.selections.runtime_authorized);
        assert!(!loaded.selections.privacy_usage_stats);
        assert!(!loaded.selections.privacy_error_reports);
        assert!(!loaded.selections.privacy_personalization);
        assert!(!loaded.selections.privacy_marketing);
    }

    #[test]
    fn load_state_tolerates_an_older_schema_missing_the_theme_field() {
        // A state file written by a binary from BEFORE the `Theme` step
        // existed — this round's own version of the same forward-compat
        // contract `load_state_tolerates_missing_new_fields_from_an_older_
        // schema` above already proves for round 2's fields: "舊檔無此欄不
        // 得重置流程". `current_step` names the step by STRING (`"templates"`),
        // never an array position, so this loads correctly even though
        // `Theme`'s insertion shifted `Finish` from index 8 to index 9 (see
        // `slug_and_from_debug_env_round_trip_for_every_step` for the
        // general guarantee this relies on).
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = std::env::temp_dir().join(format!("duduclaw-shell-oobe-test-pretheme-{}", std::process::id()));
        let prev = std::env::var("DUDUCLAW_HOME").ok();
        unsafe { std::env::set_var("DUDUCLAW_HOME", &tmp) };

        let old_schema = r#"{
            "completed": false,
            "current_step": "templates",
            "selections": {
                "language": "zh-tw",
                "network_connected": true,
                "network_ssid": "DuDu-Office",
                "account_created": true,
                "runtime_deferred": false,
                "runtime_authorized": true,
                "privacy_usage_stats": false,
                "privacy_error_reports": false,
                "privacy_personalization": false,
                "privacy_marketing": false,
                "template_choice": null
            }
        }"#;
        let dir = tmp.join("shell");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("oobe_state.json"), old_schema).unwrap();

        let loaded = load_state();

        unsafe {
            match prev {
                Some(v) => std::env::set_var("DUDUCLAW_HOME", v),
                None => std::env::remove_var("DUDUCLAW_HOME"),
            }
        }
        let _ = std::fs::remove_dir_all(&tmp);

        assert_eq!(loaded.current_step, OobeStep::Templates, "must NOT fall back to step 0 just because `theme` is missing");
        assert!(!loaded.completed);
        assert!(loaded.selections.runtime_authorized);
        assert_eq!(loaded.selections.theme, ThemeChoice::Light, "a missing `theme` key must default in, not error the whole file out");
    }

    #[test]
    fn load_state_tolerates_an_older_schema_missing_the_operator_name_field() {
        // Same forward-compat contract as the `theme` test just above,
        // pinned for Shell-S2 round 1's own new field: a state file written
        // before `operator_name` existed must still load, with the missing
        // key defaulting to `None` rather than erroring the whole file out.
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = std::env::temp_dir().join(format!("duduclaw-shell-oobe-test-preopname-{}", std::process::id()));
        let prev = std::env::var("DUDUCLAW_HOME").ok();
        unsafe { std::env::set_var("DUDUCLAW_HOME", &tmp) };

        let old_schema = r#"{
            "completed": false,
            "current_step": "account-create",
            "selections": {
                "language": "zh-tw",
                "network_connected": true,
                "network_ssid": "DuDu-Office",
                "account_created": false,
                "runtime_deferred": false,
                "theme": "light"
            }
        }"#;
        let dir = tmp.join("shell");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("oobe_state.json"), old_schema).unwrap();

        let loaded = load_state();

        unsafe {
            match prev {
                Some(v) => std::env::set_var("DUDUCLAW_HOME", v),
                None => std::env::remove_var("DUDUCLAW_HOME"),
            }
        }
        let _ = std::fs::remove_dir_all(&tmp);

        assert_eq!(loaded.current_step, OobeStep::AccountCreate, "must NOT fall back to step 0 just because `operator_name` is missing");
        assert!(!loaded.completed);
        assert_eq!(loaded.selections.operator_name, None, "a missing `operator_name` key must default to None, not error the whole file out");
    }

    #[test]
    fn load_state_tolerates_an_old_file_whose_current_step_was_the_prior_first_step() {
        // Task brief item 1's own explicit compat requirement: a state file
        // written by a binary from BEFORE this round's reorder (§B-1's
        // literal row order, `InputDetection` at index 0) has to keep
        // loading correctly under the NEW order. `#[serde(rename_all =
        // "kebab-case")]` on `OobeStep` serializes/deserializes by VARIANT
        // NAME ("input-detection"), never by array position — so this was
        // never actually at risk from the reorder itself, but the task
        // brief asks for it proven, not assumed, and a future contributor
        // reading this test file should be able to see the reorder didn't
        // silently break resume — see `oobe/mod.rs`'s header comment for
        // the reorder rationale.
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = std::env::temp_dir().join(format!("duduclaw-shell-oobe-test-oldfirststep-{}", std::process::id()));
        let prev = std::env::var("DUDUCLAW_HOME").ok();
        unsafe { std::env::set_var("DUDUCLAW_HOME", &tmp) };

        let old_schema = r#"{"completed":false,"current_step":"input-detection","selections":{}}"#;
        let dir = tmp.join("shell");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("oobe_state.json"), old_schema).unwrap();

        let loaded = load_state();

        unsafe {
            match prev {
                Some(v) => std::env::set_var("DUDUCLAW_HOME", v),
                None => std::env::remove_var("DUDUCLAW_HOME"),
            }
        }
        let _ = std::fs::remove_dir_all(&tmp);

        assert_eq!(loaded.current_step, OobeStep::InputDetection, "must still deserialize to InputDetection by name");
        assert_eq!(loaded.current_step.index(), 1, "and InputDetection now lands at the new order's second position (index 1)");
        assert!(!loaded.completed);

        // The boot-entry resolver must resume there too, not silently reset
        // to the new step 0.
        let flow = resolve_boot_flow(None, None, None, loaded).expect("not completed, so OOBE must reopen");
        assert_eq!(flow.current(), OobeStep::InputDetection);
    }

    #[test]
    fn load_state_still_fails_open_on_corrupt_json() {
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = std::env::temp_dir().join(format!("duduclaw-shell-oobe-test-corrupt-{}", std::process::id()));
        let prev = std::env::var("DUDUCLAW_HOME").ok();
        unsafe { std::env::set_var("DUDUCLAW_HOME", &tmp) };

        let dir = tmp.join("shell");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("oobe_state.json"), "{ not valid json at all").unwrap();

        let loaded = load_state();

        unsafe {
            match prev {
                Some(v) => std::env::set_var("DUDUCLAW_HOME", v),
                None => std::env::remove_var("DUDUCLAW_HOME"),
            }
        }
        let _ = std::fs::remove_dir_all(&tmp);

        assert_eq!(loaded, OobeState::default(), "corrupt JSON must still fail-open to the full default");
    }

    #[test]
    fn load_state_falls_back_to_default_on_an_unrecognized_current_step() {
        // Task brief: "current_step 未知值（未來新步）→ 整檔回預設可接受" —
        // a NEWER binary's step name this OLDER binary doesn't know is a
        // genuine enum-variant deserialize failure (not a missing-field
        // case `#[serde(default)]` can paper over), so the whole file
        // legitimately fails to parse and `load_state()`'s own fail-open
        // lands on the FULL default, current_step included.
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = std::env::temp_dir().join(format!("duduclaw-shell-oobe-test-unknownstep-{}", std::process::id()));
        let prev = std::env::var("DUDUCLAW_HOME").ok();
        unsafe { std::env::set_var("DUDUCLAW_HOME", &tmp) };

        let dir = tmp.join("shell");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("oobe_state.json"), r#"{"completed":false,"current_step":"some-future-step","selections":{}}"#).unwrap();

        let loaded = load_state();

        unsafe {
            match prev {
                Some(v) => std::env::set_var("DUDUCLAW_HOME", v),
                None => std::env::remove_var("DUDUCLAW_HOME"),
            }
        }
        let _ = std::fs::remove_dir_all(&tmp);

        assert_eq!(loaded, OobeState::default());
    }

    #[test]
    fn state_path_lands_under_the_shell_subdirectory() {
        let _g = ENV_LOCK.lock().unwrap();
        let prev = std::env::var("DUDUCLAW_HOME").ok();
        unsafe { std::env::set_var("DUDUCLAW_HOME", "/tmp/duduclaw-shell-oobe-path-test") };
        let path = state_path();
        unsafe {
            match prev {
                Some(v) => std::env::set_var("DUDUCLAW_HOME", v),
                None => std::env::remove_var("DUDUCLAW_HOME"),
            }
        }
        assert_eq!(path, Some(PathBuf::from("/tmp/duduclaw-shell-oobe-path-test/shell/oobe_state.json")));
    }

    // ── resolve_boot_flow ─────────────────────────────────────────────

    #[test]
    fn boot_flow_defaults_to_oobe_when_state_is_not_completed() {
        let flow = resolve_boot_flow(None, None, None, OobeState::default());
        assert!(flow.is_some());
        assert_eq!(flow.unwrap().current(), OobeStep::LanguageAccessibility);
    }

    #[test]
    fn boot_flow_goes_to_home_when_state_is_completed() {
        let state = OobeState { completed: true, ..OobeState::default() };
        assert!(resolve_boot_flow(None, None, None, state).is_none());
    }

    #[test]
    fn force_oobe_wins_even_when_persisted_state_is_completed() {
        let state = OobeState { completed: true, ..OobeState::default() };
        let flow = resolve_boot_flow(Some("1"), None, None, state);
        assert!(flow.is_some());
    }

    #[test]
    fn force_oobe_beats_skip_oobe_when_both_are_set() {
        // Task brief: "兩者都設時 FORCE 優先".
        let flow = resolve_boot_flow(Some("1"), Some("1"), None, OobeState::default());
        assert!(flow.is_some());
    }

    #[test]
    fn skip_oobe_wins_over_an_incomplete_persisted_state() {
        let flow = resolve_boot_flow(None, Some("1"), None, OobeState::default());
        assert!(flow.is_none());
    }

    #[test]
    fn debug_step_opens_oobe_directly_at_that_step_even_if_completed() {
        let state = OobeState { completed: true, current_step: OobeStep::Finish, ..OobeState::default() };
        let flow = resolve_boot_flow(None, None, Some("network"), state);
        let flow = flow.expect("debug step must force OOBE open");
        assert_eq!(flow.current(), OobeStep::Network);
        assert!(!flow.completed());
    }

    #[test]
    fn debug_step_with_unrecognized_value_falls_back_to_normal_resolution() {
        let state = OobeState { completed: true, ..OobeState::default() };
        let flow = resolve_boot_flow(None, None, Some("bogus"), state);
        assert!(flow.is_none(), "unrecognized debug step should not override a completed state");
    }

    #[test]
    fn skip_oobe_beats_an_unrecognized_debug_step() {
        // Priority ordering: FORCE > SKIP > DEBUG_STEP > persisted state.
        let flow = resolve_boot_flow(None, Some("1"), Some("network"), OobeState::default());
        assert!(flow.is_none());
    }

    #[test]
    fn debug_step_preserves_earlier_persisted_selections() {
        let mut state = OobeState::default();
        state.selections.language = LanguageChoice::En;
        let flow = resolve_boot_flow(None, None, Some("finish"), state).unwrap();
        assert_eq!(flow.current(), OobeStep::Finish);
        assert_eq!(flow.selections().language, LanguageChoice::En);
    }

    // ── Shell-S2 round 1: AccountClaimState / OobeUiState transitions ──
    // Pure logic only, same style every other `OobeUiState` test in this
    // module already uses — no gpui, no network, no `oobe::claim` mock
    // server needed here (that lives in `claim.rs`'s own `tests` module).

    #[test]
    fn account_claim_defaults_to_idle() {
        let ui = OobeUiState::default();
        assert_eq!(ui.account_claim, AccountClaimState::Idle);
    }

    #[test]
    fn set_account_claim_in_flight_transitions_from_idle() {
        let mut ui = OobeUiState::default();
        ui.set_account_claim_in_flight();
        assert_eq!(ui.account_claim, AccountClaimState::InFlight);
    }

    #[test]
    fn set_account_claim_done_records_whether_it_was_already_claimed() {
        let mut ui = OobeUiState::default();
        ui.set_account_claim_in_flight();
        ui.set_account_claim_done(false);
        assert_eq!(ui.account_claim, AccountClaimState::Done { already: false });

        let mut ui2 = OobeUiState::default();
        ui2.set_account_claim_in_flight();
        ui2.set_account_claim_done(true);
        assert_eq!(ui2.account_claim, AccountClaimState::Done { already: true });
    }

    #[test]
    fn set_account_claim_failed_records_the_failure_kind() {
        let mut ui = OobeUiState::default();
        ui.set_account_claim_in_flight();
        ui.set_account_claim_failed(AccountClaimFailureKind::PasswordTooShort);
        assert_eq!(ui.account_claim, AccountClaimState::Failed(AccountClaimFailureKind::PasswordTooShort));

        let mut ui2 = OobeUiState::default();
        ui2.set_account_claim_in_flight();
        ui2.set_account_claim_failed(AccountClaimFailureKind::Unreachable);
        assert_eq!(ui2.account_claim, AccountClaimState::Failed(AccountClaimFailureKind::Unreachable));
    }

    #[test]
    fn reset_account_claim_returns_to_idle_from_any_state() {
        for mut ui in [
            {
                let mut u = OobeUiState::default();
                u.set_account_claim_in_flight();
                u
            },
            {
                let mut u = OobeUiState::default();
                u.set_account_claim_done(true);
                u
            },
            {
                let mut u = OobeUiState::default();
                u.set_account_claim_failed(AccountClaimFailureKind::Unreachable);
                u
            },
        ] {
            ui.reset_account_claim();
            assert_eq!(ui.account_claim, AccountClaimState::Idle);
        }
    }

    #[test]
    fn account_claim_and_validation_error_are_independent_fields() {
        // The two error sources `steps::account`'s render fn checks
        // (`account_validation_error` vs. `account_claim`) are separate
        // fields — setting one must never implicitly touch the other. The
        // click handler itself is what keeps them from both rendering at
        // once (see `steps::account`'s own doc comment), not this struct.
        let mut ui = OobeUiState::default();
        ui.set_account_claim_failed(AccountClaimFailureKind::PasswordTooShort);
        ui.set_account_validation_error(true);
        assert_eq!(ui.account_claim, AccountClaimState::Failed(AccountClaimFailureKind::PasswordTooShort));
        assert!(ui.account_validation_error);
    }
}
