// OOBE step catalog + pure state machine — split out of `oobe/mod.rs`
// (WP-OOBE-split, 2026-08-21): that file had grown to 2,030 lines, well past
// this crate's own <800-line file convention, so its content now lives in
// four siblings — `state.rs` (this file: `OobeStep`/`OobeFlow`, the state
// machine itself), `selections.rs` (the persisted selection value types:
// `OobeState`/`OobeSelections`/`LanguageChoice`/`TemplateChoice`/
// `ThemeChoice`/`PrivacyToggle`), `persistence.rs` (disk I/O + boot-entry
// resolution), and `ui_state.rs` (`OobeUiState` + the two `AccountClaim*`
// enums) — the same "split out of `oobe/mod.rs`, re-export so call sites
// don't change" shape `network_ui.rs` already established a round earlier
// for the `Network` step's own ephemeral UI enums (see that file's own
// header comment). Every design citation that used to sit above this code
// is UNCHANGED and still lives at the top of `oobe/mod.rs` — it stayed
// there rather than being torn apart paragraph-by-paragraph across siblings
// that didn't exist when it was written; see that file's own header comment
// for the full OOBE design history (step-order sourcing, the language-first
// correction, the "pure `&mut self` mutation, no gpui types" discipline
// this file still holds itself to) that motivated everything below.
//
// Pure move, no behavior change: every item that was `pub` in `oobe/mod.rs`
// stays `pub` here and is re-exported from there unchanged (`crate::oobe::
// OobeStep`/`crate::oobe::OobeFlow` keep working for every existing call
// site). The one visibility WIDENING this split required is `OobeFlow::
// palette()` — see that method's own doc comment below for why.

use serde::{Deserialize, Serialize};

use super::selections::{LanguageChoice, OobeSelections, OobeState, PrivacyToggle, TemplateChoice, ThemeChoice};

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
    /// see `oobe/mod.rs`'s header comment for why the original
    /// literal-§B-1-row order was a correction-worthy slip): language
    /// selection gates every SUBSEQUENT screen's copy, so it has to render,
    /// and be answerable, before any of them — including `InputDetection`.
    /// Not skippable. Real i18n as of this round — see `steps::language`'s
    /// own header comment and `crate::i18n`.
    LanguageAccessibility,
    /// §B-1 row 0: "輸入裝置偵測（無鍵盤→觸控/遙控模式）" — ChromeOS's
    /// `hid_detection`. No longer `ALL[0]` (see `oobe/mod.rs`'s header
    /// comment) — still the first step AFTER language, unchanged relative
    /// to every other row. Not skippable: no citation offers a skip
    /// affordance for this step (from-the-strict-default), and it is a
    /// passive detection pass with no precondition either — Continue is
    /// always enabled.
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
    ///
    /// Shell-S1 (2026-08-20) extends the SAME choice to Home + its
    /// overlays, once OOBE finishes — not live during OOBE itself, since
    /// Home isn't even rendered underneath OOBE (see `main.rs`'s header
    /// comment: OOBE replaces Home outright, it doesn't overlay it). `main::
    /// on_oobe_next` copies `selections().theme` onto `ShellView.theme` at
    /// the exact moment `oobe` flips back to `None`, so Home's very first
    /// frame already paints in whichever theme was picked here — see
    /// `crate::palette`'s own header comment for the Home/overlay color
    /// mapping this unlocks.
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

/// The pure state machine — no gpui types, see `oobe/mod.rs`'s header
/// comment. Owns one `OobeState` and exposes the only ways to mutate it
/// (`next`/`back`/`skip`/the `set_*` selection setters), so every rendered
/// screen and every test goes through the same gate.
#[derive(Debug, Clone, PartialEq)]
pub struct OobeFlow {
    state: OobeState,
}

/// What pressing Enter should do on the CURRENT step — see `OobeFlow::
/// enter_outcome`'s own doc comment for the full decision table this backs.
/// A dedicated type (not an inlined `bool`/match at the call site) so the
/// decision is one pure, independently testable unit; `main.rs`'s `on_oobe_
/// next` (the only production caller) does nothing but match on this and
/// act.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EnterOutcome {
    /// Advance the flow (`next_with_wired`) — the step's own precondition
    /// (if any) is already satisfied.
    Advance,
    /// `AccountCreate`, not yet created, no claim in flight: trigger the
    /// step's own "建立帳號" submit instead of a doomed `next_with_wired`
    /// (which would be a guaranteed no-op — `account_created` only ever
    /// flips on a server-confirmed outcome, never on Enter itself).
    SubmitAccount,
    /// `Network`, a row is selected and the connect flow is awaiting a PSK
    /// or retrying a failure: trigger the step's own "連線" submit, same
    /// reasoning as `SubmitAccount`.
    SubmitNetworkConnect,
    /// Neither advance nor a local submit applies right now (precondition
    /// unmet AND no submit target — e.g. no network row picked yet, or a
    /// submit is already in flight) — a legitimate no-op, not a bug.
    Blocked,
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
    /// break `oobe/mod.rs`'s "no gpui types anywhere in THIS file"
    /// discipline (see that file's header comment).
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
    ///
    /// D4a-5 (2026-08-23): `#[allow(dead_code)]` — both PRODUCTION call
    /// sites (`render.rs`'s Continue button, `main.rs`'s Enter-key handler)
    /// switched to `next_with_wired`, which behaves identically to this
    /// method on every step except `Network` (see that method's own doc
    /// comment). This method is kept, not deleted or inlined into `next_
    /// with_wired`: it is the simpler primitive `next_with_wired` builds on
    /// top of, it is still what every test in this file's own `tests`
    /// module below exercises for every non-Network-step scenario, and
    /// `persistence.rs`'s boot-resume tests call it directly too — removing
    /// it would just mean re-deriving the same logic under a different
    /// name.
    #[allow(dead_code)]
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

    /// D4a-5 (2026-08-23, D4a §5.4-2 "有線已連通時網路步應可通過"): same as
    /// `can_advance()`, except the `Network` step ALSO accepts a live,
    /// ephemeral "wired (or captive-portal-gated) connection is currently
    /// up" signal — `wired_online`, sourced from `OobeUiState::
    /// wired_online()`, which is itself derived from the last-fetched
    /// `network::NetworkStatus` snapshot (see that method's own doc comment
    /// for why it must never be persisted: a wired connection is an
    /// environmental fact that can change the instant a cable is unplugged,
    /// not a durable user selection like a completed Wi-Fi join).
    ///
    /// Deliberately a SEPARATE method from `can_advance()`, not a parameter
    /// added to it — every existing caller that has no `OobeUiState` in
    /// hand (`persistence.rs`'s own boot-resume tests, and every test in
    /// THIS file's own `tests` module below) keeps compiling and behaving
    /// unchanged; only `render.rs`'s Continue-button gate and `main.rs`'s
    /// Enter-key handler, the two places that actually hold an
    /// `OobeUiState`, call this one instead. Every step other than
    /// `Network` behaves identically to `can_advance()`.
    pub fn can_advance_with_wired(&self, wired_online: bool) -> bool {
        match self.state.current_step {
            OobeStep::Network => self.state.selections.network_connected || wired_online,
            _ => self.can_advance(),
        }
    }

    /// The `next()` twin of `can_advance_with_wired` — see that method's
    /// own doc comment for why this is additive rather than a parameter on
    /// `next()` itself. Identical body shape to `next()`, just gated on the
    /// wired-aware precondition.
    pub fn next_with_wired(&mut self, wired_online: bool) -> bool {
        if self.state.completed || !self.can_advance_with_wired(wired_online) {
            return false;
        }
        match self.state.current_step.next() {
            Some(step) => self.state.current_step = step,
            None => self.state.completed = true,
        }
        true
    }

    /// Pure decision for what pressing Enter should do on the CURRENT step
    /// — see `EnterOutcome`'s own doc comment for what each variant means.
    ///
    /// `AccountCreate` and `Network` are the two steps whose `can_advance`
    /// precondition is satisfied by an ASYNC action gated behind its own
    /// explicit button (`steps::account`'s "建立帳號", `steps::network`'s
    /// "連線" — see `oobe/render.rs`'s `button_row`, whose Continue button
    /// is itself `disabled` until that precondition is met, by design).
    /// Before this method existed, `main.rs`'s `on_oobe_next` called `next_
    /// with_wired` unconditionally on every step — exactly right everywhere
    /// EXCEPT these two, where it was a silent, indefinite no-op: an
    /// operator could type a full name+password (or a full Wi-Fi
    /// passphrase) and press Enter, and NOTHING would happen, because Enter
    /// never triggered the submit those steps require before `can_advance`
    /// can ever become true. This method restores parity with the mouse:
    /// Enter now reaches the SAME submit action the step's own button does.
    ///
    /// `wired_online`/`account_claim_in_flight`/`net_connect_submittable`
    /// are all plain `bool`s, not `OobeUiState`/`NetConnectState` directly
    /// — this file stays free of any dependency on `ui_state.rs`/
    /// `network_ui.rs` (a one-directional edge that doesn't exist today),
    /// and the caller (`main.rs`'s `on_oobe_next`) is a one-line read of
    /// each source field either way.
    pub(crate) fn enter_outcome(
        &self,
        wired_online: bool,
        account_claim_in_flight: bool,
        net_connect_submittable: bool,
    ) -> EnterOutcome {
        if self.can_advance_with_wired(wired_online) {
            return EnterOutcome::Advance;
        }
        match self.state.current_step {
            OobeStep::AccountCreate if !account_claim_in_flight => EnterOutcome::SubmitAccount,
            OobeStep::Network if net_connect_submittable => EnterOutcome::SubmitNetworkConnect,
            _ => EnterOutcome::Blocked,
        }
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
    /// `crate::palette::ShellPalette` is plain data (u32 hex + a couple of
    /// precomposited `Rgba` fields + a handful of derived-shadow/border/
    /// badge methods) defined at CRATE ROOT — not inside `oobe/` — since
    /// Shell-S1 extended theming to Home + its overlays, which are
    /// siblings of `oobe`, not descendants (see `palette.rs`'s own header
    /// comment for the full rename/move rationale). This method only NAMES
    /// that type in its signature, it does not import or construct a gpui
    /// value itself, so exposing it doesn't break `oobe/mod.rs`'s "no gpui
    /// types anywhere in THIS file" discipline (see that file's header
    /// comment) — it still holds at the level that discipline is actually
    /// about: no gpui CODE runs here.
    ///
    /// `pub(super)`, not a bare `fn` (WP-OOBE-split, 2026-08-21) — this
    /// method moved here from `oobe/mod.rs` along with the rest of
    /// `OobeFlow`, but `render.rs`/`steps/*.rs` (its only callers) are
    /// SIBLINGS of `state.rs` within `oobe`, not descendants of it, so the
    /// module-private visibility that used to reach them (both were
    /// descendants of `oobe/mod.rs`'s own module, which sees every private
    /// item of its own module — same default-visibility rule `fake_data`
    /// itself relies on) no longer does. `pub(super)` is the minimal
    /// widening that restores the exact same reach — visible to `oobe` and
    /// everything nested under it, same as before the split, never a wider
    /// `pub` or `pub(crate)` surface than the one actual caller shape
    /// already needed.
    pub(super) fn palette(&self) -> crate::palette::ShellPalette {
        crate::palette::ShellPalette::for_choice(self.state.selections.theme)
    }
}

impl Default for OobeFlow {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── step ordering / indices ──────────────────────────────────────

    #[test]
    fn all_has_ten_steps_in_declared_order() {
        assert_eq!(OobeStep::ALL.len(), 10);
        // Task brief item 1: language leads (was `ALL[1]`), input detection
        // follows (was `ALL[0]`) — see `oobe/mod.rs`'s header comment.
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

    // ── D4a-5: can_advance_with_wired / next_with_wired ─────────────────

    #[test]
    fn can_advance_with_wired_lets_a_wired_only_connection_through_the_network_step() {
        let mut flow = OobeFlow::new();
        flow.next(); // Language -> InputDetection
        flow.next(); // InputDetection -> Network
        assert_eq!(flow.current(), OobeStep::Network);
        assert!(!flow.can_advance(), "no Wi-Fi join and no wired signal — still blocked via the ORIGINAL method");
        assert!(!flow.can_advance_with_wired(false));
        assert!(flow.can_advance_with_wired(true), "a wired-online signal alone must be enough");
    }

    #[test]
    fn next_with_wired_advances_past_network_on_a_wired_signal_without_ever_persisting_a_wifi_join() {
        let mut flow = OobeFlow::new();
        flow.next();
        flow.next();
        assert_eq!(flow.current(), OobeStep::Network);
        assert!(!flow.next_with_wired(false));
        assert_eq!(flow.current(), OobeStep::Network);

        assert!(flow.next_with_wired(true));
        assert_eq!(flow.current(), OobeStep::Update);
        // The whole point of D4a §5.4-2: wired connectivity is an
        // environmental fact, never written into the persisted Wi-Fi join
        // fields — unplugging the cable and reloading must not leave the
        // flow believing it already joined a network.
        assert!(!flow.selections().network_connected);
        assert_eq!(flow.selections().network_ssid, None);
    }

    #[test]
    fn can_advance_with_wired_behaves_identically_to_can_advance_on_every_other_step() {
        // The wired-online parameter must ONLY change the Network step's
        // gate — every other step's precondition (or lack of one) stays
        // exactly what `can_advance()` already says, regardless of the
        // (irrelevant, off-topic) wired flag passed in.
        let mut flow = OobeFlow::new();
        for wired in [false, true] {
            assert_eq!(flow.can_advance_with_wired(wired), flow.can_advance(), "step={:?} wired={wired}", flow.current());
        }
        flow.next(); // -> InputDetection
        for wired in [false, true] {
            assert_eq!(flow.can_advance_with_wired(wired), flow.can_advance(), "step={:?} wired={wired}", flow.current());
        }
    }

    #[test]
    fn next_with_wired_still_respects_a_completed_flow_and_the_account_precondition() {
        // Same no-op-once-completed / same AccountCreate gate as `next()` —
        // the wired parameter is Network-step-specific, it must not loosen
        // any OTHER precondition.
        let mut flow = finish_flow();
        assert!(flow.next_with_wired(true));
        assert!(flow.completed());
        assert!(!flow.next_with_wired(true), "no-op once completed, wired or not");

        let mut flow2 = OobeFlow::new();
        while flow2.current() != OobeStep::AccountCreate {
            if flow2.current() == OobeStep::Network {
                flow2.set_network("DuDu-Office", true);
            }
            flow2.next();
        }
        assert!(!flow2.next_with_wired(true), "AccountCreate's own precondition is untouched by the wired signal");
    }

    // ── EnterOutcome: what pressing Enter should do per step ──────────

    #[test]
    fn enter_outcome_advances_on_every_step_with_no_blocking_precondition() {
        let flow = OobeFlow::new(); // LanguageAccessibility
        assert_eq!(flow.enter_outcome(false, false, false), EnterOutcome::Advance);
    }

    #[test]
    fn enter_outcome_is_blocked_on_network_with_nothing_selected_to_submit() {
        let mut flow = OobeFlow::new();
        flow.next(); // -> InputDetection
        flow.next(); // -> Network
        assert_eq!(flow.current(), OobeStep::Network);
        assert_eq!(flow.enter_outcome(false, false, false), EnterOutcome::Blocked);
    }

    #[test]
    fn enter_outcome_submits_network_connect_when_a_row_is_awaiting_submit() {
        let mut flow = OobeFlow::new();
        flow.next();
        flow.next();
        assert_eq!(flow.current(), OobeStep::Network);
        assert_eq!(flow.enter_outcome(false, false, true), EnterOutcome::SubmitNetworkConnect);
    }

    #[test]
    fn enter_outcome_advances_past_network_on_a_wired_signal_even_with_a_submittable_row() {
        // A wired connection wins outright — `can_advance_with_wired` is
        // checked FIRST, so a stale `AwaitingPsk` panel never blocks Enter
        // once the machine is already online another way.
        let mut flow = OobeFlow::new();
        flow.next();
        flow.next();
        assert_eq!(flow.enter_outcome(true, false, true), EnterOutcome::Advance);
    }

    #[test]
    fn enter_outcome_advances_past_network_once_persisted_as_connected() {
        let mut flow = OobeFlow::new();
        flow.next();
        flow.next();
        flow.set_network("DuDu-Office", true);
        assert_eq!(flow.enter_outcome(false, false, false), EnterOutcome::Advance);
    }

    #[test]
    fn enter_outcome_submits_account_when_not_yet_created_and_no_claim_in_flight() {
        let mut flow = OobeFlow::new();
        while flow.current() != OobeStep::AccountCreate {
            if flow.current() == OobeStep::Network {
                flow.set_network("DuDu-Office", true);
            }
            flow.next();
        }
        assert_eq!(flow.enter_outcome(false, false, false), EnterOutcome::SubmitAccount);
    }

    #[test]
    fn enter_outcome_is_blocked_on_account_create_while_a_claim_is_already_in_flight() {
        let mut flow = OobeFlow::new();
        while flow.current() != OobeStep::AccountCreate {
            if flow.current() == OobeStep::Network {
                flow.set_network("DuDu-Office", true);
            }
            flow.next();
        }
        assert_eq!(
            flow.enter_outcome(false, true, false),
            EnterOutcome::Blocked,
            "must not double-submit while a claim is already in flight"
        );
    }

    #[test]
    fn enter_outcome_advances_past_account_create_once_created() {
        let mut flow = OobeFlow::new();
        while flow.current() != OobeStep::AccountCreate {
            if flow.current() == OobeStep::Network {
                flow.set_network("DuDu-Office", true);
            }
            flow.next();
        }
        flow.set_account_created(true);
        assert_eq!(flow.enter_outcome(false, false, false), EnterOutcome::Advance);
    }

    #[test]
    fn enter_outcome_advances_on_every_precondition_free_step() {
        // Update / RuntimeAuth / Privacy / Templates / Theme / Finish all
        // have no `can_advance` precondition of their own (see each
        // `OobeStep` variant's own doc comment) — Enter must always Advance
        // on every one of them, regardless of the three signal params.
        for step in [
            OobeStep::Update,
            OobeStep::RuntimeAuth,
            OobeStep::Privacy,
            OobeStep::Templates,
            OobeStep::Theme,
            OobeStep::Finish,
        ] {
            let flow = OobeFlow::from_state(OobeState { current_step: step, ..OobeState::default() });
            assert_eq!(flow.enter_outcome(false, false, false), EnterOutcome::Advance, "{step:?}");
        }
    }

    #[test]
    fn enter_outcome_from_finish_is_what_completes_the_flow() {
        // `main.rs`'s Advance arm calls `next_with_wired`, which is what
        // actually flips `completed` from the `Finish` step — this test
        // locks that `enter_outcome` reports `Advance` there too, so Enter
        // on the Finish step really does finish OOBE end to end.
        let flow = finish_flow();
        assert_eq!(flow.enter_outcome(false, false, false), EnterOutcome::Advance);
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
}
