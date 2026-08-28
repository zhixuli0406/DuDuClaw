// Y20-P2 (2026-08-29) — the live-installer's own 4-step state machine.
//
// Shape deliberately mirrors `oobe::state`'s `OobeStep`/`OobeFlow` (see that
// file's own header comment for the "fixed `ALL` array + `index`/
// `from_index`/`next`/`prev`, a thin `Flow` wrapper owning one plain-data
// state struct" pattern this borrows) — but this is a SEPARATE type, not an
// extra `OobeStep` variant. See `live_install/mod.rs`'s own header comment
// for why touching `OobeStep::ALL` was ruled out for this task.
//
// ── What's simpler here than in `OobeFlow`, and why ────────────────────────
// - No blocking `can_advance` precondition on ANY step yet. OOBE blocks on
//   `Network`/`AccountCreate` because those steps have a real, checkable
//   outcome (a joined network, a created account) to gate on. This round's
//   `DiskSelect` has no real device list to validate a pick against — P3
//   will add one, and `can_advance` is the exact method that will grow a
//   `DiskSelect` arm then, mirroring `OobeFlow::can_advance`'s own shape.
// - No disk persistence (`oobe::persistence`'s `load_state`/`save_state`
//   equivalent). OOBE must survive a reboot mid-flow so `resolve_boot_flow`
//   can resume it on a machine that keeps its disk across boots. A live
//   installer session runs once on a throwaway live-media root — a reboot
//   out of the live image re-runs this whole binary from `LiveInstallStep::
//   ALL[0]` again regardless of what this struct remembers, so persisting it
//   would have no reader.
// - No `skip`/`EnterOutcome`/keyboard-Enter routing. P2 scope is the 4-step
//   skeleton's forward/back navigation only, per the task brief; nothing
//   here is skippable (a live install has no defer-to-later step the way
//   OOBE's `RuntimeAuth`/`Templates` do), and Enter-key handling for this
//   flow is left for whichever round wires up real interaction beyond mouse
//   clicks on `render.rs`'s bottom nav.

use crate::oobe::LanguageChoice;

/// The four live-install wizard steps, in fixed linear order — no branching,
/// same "step order is data, not a runtime decision" discipline `oobe::
/// state::OobeStep` follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LiveInstallStep {
    /// Same three-`LanguageChoice` picker `oobe::steps::language` offers,
    /// re-derived (not literally shared) against THIS flow's own state — see
    /// `steps::language`'s own header comment for why the two can't share a
    /// click handler even though the visual row is the same shape.
    Language,
    /// P3 will replace this with a real block-device enumeration + a pick
    /// that gates `can_advance` (mirroring `OobeStep::Network`/
    /// `AccountCreate`'s own preconditions). P2 renders an honest "開發中"
    /// placeholder — see `steps::disk_select`'s own header comment.
    DiskSelect,
    /// A confirmation summary of what's about to happen before anything
    /// destructive runs. P2 renders static placeholder copy; P3 will fill it
    /// in with the real target disk picked on the previous step.
    Confirm,
    /// The terminal step: real `dd`-style write progress is P3's job (see
    /// `steps::progress`'s own header comment) — P2 renders a static,
    /// honestly-labeled placeholder bar so the 4-step navigation has
    /// somewhere to land.
    Progress,
}

impl LiveInstallStep {
    pub(crate) const ALL: [LiveInstallStep; 4] =
        [LiveInstallStep::Language, LiveInstallStep::DiskSelect, LiveInstallStep::Confirm, LiveInstallStep::Progress];

    pub(crate) fn index(self) -> usize {
        Self::ALL.iter().position(|s| *s == self).expect("LiveInstallStep::ALL is exhaustive over every variant")
    }

    pub(crate) fn from_index(index: usize) -> Option<Self> {
        Self::ALL.get(index).copied()
    }

    pub(crate) fn next(self) -> Option<Self> {
        Self::from_index(self.index() + 1)
    }

    pub(crate) fn prev(self) -> Option<Self> {
        self.index().checked_sub(1).and_then(Self::from_index)
    }
}

/// Plain data the flow mutates — no gpui types, same "testable without a
/// live window" discipline `oobe::state`'s own header comment holds itself
/// to. Not persisted to disk — see this file's header comment on why a live
/// session has nothing to resume across.
#[derive(Debug, Clone, PartialEq)]
struct LiveInstallState {
    current_step: LiveInstallStep,
    completed: bool,
    language: LanguageChoice,
}

impl Default for LiveInstallState {
    fn default() -> Self {
        Self { current_step: LiveInstallStep::Language, completed: false, language: LanguageChoice::default() }
    }
}

/// The pure state machine — mirrors `oobe::state::OobeFlow`'s own shape
/// (owns one state struct, exposes the only ways to mutate it) at P2's much
/// smaller scope: see this file's header comment for what's deliberately
/// simpler here (no blocking precondition, no persistence, no skip).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LiveInstallFlow {
    state: LiveInstallState,
}

impl LiveInstallFlow {
    pub(crate) fn new() -> Self {
        Self { state: LiveInstallState::default() }
    }

    pub(crate) fn current(&self) -> LiveInstallStep {
        self.state.current_step
    }

    /// No production call site reads this yet — P2 has nothing to DO once
    /// the flow reaches `completed` (a real "install finished, reboot"
    /// action is P3+ scope, once there's an actual write to have finished).
    /// Kept (not deleted): this file's own `tests` module exercises it, and
    /// it is the exact shape `oobe::state::OobeFlow::completed()` already
    /// establishes for the same "current call sites are all tests" state a
    /// production consumer will eventually read.
    #[allow(dead_code)]
    pub(crate) fn completed(&self) -> bool {
        self.state.completed
    }

    pub(crate) fn language(&self) -> LanguageChoice {
        self.state.language
    }

    /// Thin wrapper over `LanguageChoice::to_locale()`, same convention
    /// `OobeFlow::locale()` establishes — so every step's render fn reads
    /// `flow.locale()` rather than `flow.language().to_locale()` everywhere.
    pub(crate) fn locale(&self) -> crate::i18n::Locale {
        self.state.language.to_locale()
    }

    /// Click-to-record, same split every OOBE setter uses (`OobeFlow::
    /// set_language` et al.): records the pick immediately so the very next
    /// render call reflects it, without itself advancing the step.
    pub(crate) fn set_language(&mut self, language: LanguageChoice) {
        self.state.language = language;
    }

    /// P2: every step can always advance — see this type's own header
    /// comment for why (no real disk pick exists yet to gate `DiskSelect`
    /// on). Kept as its own method, not inlined into `next()`, so P3 only
    /// has to add a `DiskSelect` arm here — the exact shape `OobeFlow::
    /// can_advance` already establishes for `Network`/`AccountCreate`.
    pub(crate) fn can_advance(&self) -> bool {
        true
    }

    /// Advances to the next step, or marks the flow `completed` once called
    /// from `Progress`. No-op once already completed.
    pub(crate) fn next(&mut self) -> bool {
        if self.state.completed || !self.can_advance() {
            return false;
        }
        match self.state.current_step.next() {
            Some(step) => self.state.current_step = step,
            None => self.state.completed = true,
        }
        true
    }

    /// No-op on the first step (`Language`) — same "can't back up past the
    /// start" guard `OobeFlow::back` enforces.
    pub(crate) fn back(&mut self) -> bool {
        if self.state.current_step == LiveInstallStep::Language {
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

    /// P2 always resolves the default (light) palette — this wizard has no
    /// `Theme` step of its own (unlike OOBE), and a live-install session has
    /// no persisted operator theme pick to read yet either. `pub(crate)`
    /// (not `pub(super)`): `live_install::render`/`steps::*` are siblings of
    /// this file within `live_install`, same reach `OobeFlow::palette()`'s
    /// own doc comment explains for its own `pub(super)`.
    pub(crate) fn palette(&self) -> crate::palette::ShellPalette {
        crate::palette::ShellPalette::default()
    }
}

impl Default for LiveInstallFlow {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── step ordering / indices ──────────────────────────────────────

    #[test]
    fn all_has_four_steps_in_declared_order() {
        assert_eq!(LiveInstallStep::ALL.len(), 4);
        assert_eq!(LiveInstallStep::ALL[0], LiveInstallStep::Language);
        assert_eq!(LiveInstallStep::ALL[1], LiveInstallStep::DiskSelect);
        assert_eq!(LiveInstallStep::ALL[2], LiveInstallStep::Confirm);
        assert_eq!(LiveInstallStep::ALL[3], LiveInstallStep::Progress);
    }

    #[test]
    fn index_and_from_index_round_trip_for_every_step() {
        for step in LiveInstallStep::ALL {
            assert_eq!(LiveInstallStep::from_index(step.index()), Some(step));
        }
    }

    #[test]
    fn next_walks_forward_through_all_four_steps_in_declared_order() {
        let mut step = LiveInstallStep::Language;
        let mut seen = vec![step];
        while let Some(n) = step.next() {
            seen.push(n);
            step = n;
        }
        assert_eq!(seen, LiveInstallStep::ALL.to_vec());
    }

    #[test]
    fn progress_has_no_next_step() {
        assert_eq!(LiveInstallStep::Progress.next(), None);
    }

    #[test]
    fn language_has_no_prev_step() {
        assert_eq!(LiveInstallStep::Language.prev(), None);
    }

    #[test]
    fn disk_select_prev_is_language() {
        assert_eq!(LiveInstallStep::DiskSelect.prev(), Some(LiveInstallStep::Language));
    }

    // ── LiveInstallFlow: next / back / complete ────────────────────────

    #[test]
    fn starts_at_language_not_completed() {
        let flow = LiveInstallFlow::new();
        assert_eq!(flow.current(), LiveInstallStep::Language);
        assert!(!flow.completed());
    }

    #[test]
    fn next_advances_through_all_four_steps_with_no_precondition() {
        let mut flow = LiveInstallFlow::new();
        assert!(flow.next());
        assert_eq!(flow.current(), LiveInstallStep::DiskSelect);
        assert!(flow.next());
        assert_eq!(flow.current(), LiveInstallStep::Confirm);
        assert!(flow.next());
        assert_eq!(flow.current(), LiveInstallStep::Progress);
    }

    #[test]
    fn back_from_second_step_returns_to_first() {
        let mut flow = LiveInstallFlow::new();
        flow.next();
        assert!(flow.back());
        assert_eq!(flow.current(), LiveInstallStep::Language);
    }

    #[test]
    fn back_on_the_first_step_is_a_noop_not_a_panic() {
        let mut flow = LiveInstallFlow::new();
        assert!(!flow.back());
        assert_eq!(flow.current(), LiveInstallStep::Language);
    }

    #[test]
    fn back_walks_all_the_way_from_progress_to_language() {
        let mut flow = LiveInstallFlow::new();
        for _ in 0..3 {
            flow.next();
        }
        assert_eq!(flow.current(), LiveInstallStep::Progress);
        assert!(flow.back());
        assert_eq!(flow.current(), LiveInstallStep::Confirm);
        assert!(flow.back());
        assert_eq!(flow.current(), LiveInstallStep::DiskSelect);
        assert!(flow.back());
        assert_eq!(flow.current(), LiveInstallStep::Language);
        assert!(!flow.back());
    }

    #[test]
    fn next_on_progress_completes_the_flow() {
        let mut flow = LiveInstallFlow::new();
        for _ in 0..3 {
            flow.next();
        }
        assert_eq!(flow.current(), LiveInstallStep::Progress);
        assert!(!flow.completed());
        assert!(flow.next());
        assert!(flow.completed());
        assert_eq!(flow.current(), LiveInstallStep::Progress, "current step stays put once completed");
    }

    #[test]
    fn next_is_a_noop_once_completed() {
        let mut flow = LiveInstallFlow::new();
        for _ in 0..4 {
            flow.next();
        }
        assert!(flow.completed());
        assert!(!flow.next());
    }

    #[test]
    fn can_advance_is_always_true_at_p2_scope() {
        let mut flow = LiveInstallFlow::new();
        for _ in 0..4 {
            assert!(flow.can_advance(), "{:?}", flow.current());
            flow.next();
        }
    }

    // ── language selection ──────────────────────────────────────────

    #[test]
    fn language_defaults_to_zh_tw() {
        let flow = LiveInstallFlow::new();
        assert_eq!(flow.language(), LanguageChoice::ZhTw);
    }

    #[test]
    fn set_language_records_the_choice_without_advancing() {
        let mut flow = LiveInstallFlow::new();
        flow.set_language(LanguageChoice::En);
        assert_eq!(flow.language(), LanguageChoice::En);
        assert_eq!(flow.current(), LiveInstallStep::Language, "click-to-record must not itself advance");
    }

    #[test]
    fn locale_follows_the_selected_language() {
        let mut flow = LiveInstallFlow::new();
        assert_eq!(flow.locale(), crate::i18n::Locale::ZhTw);
        flow.set_language(LanguageChoice::JaJp);
        assert_eq!(flow.locale(), crate::i18n::Locale::JaJp);
    }
}
