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
// - No disk persistence (`oobe::persistence`'s `load_state`/`save_state`
//   equivalent). OOBE must survive a reboot mid-flow so `resolve_boot_flow`
//   can resume it on a machine that keeps its disk across boots. A live
//   installer session runs once on a throwaway live-media root — a reboot
//   out of the live image re-runs this whole binary from `LiveInstallStep::
//   ALL[0]` again regardless of what this struct remembers, so persisting it
//   would have no reader.
// - No `skip`/`EnterOutcome`/keyboard-Enter routing. Nothing here is
//   skippable (a live install has no defer-to-later step the way OOBE's
//   `RuntimeAuth`/`Templates` do), and Enter-key handling for this flow is
//   left for whichever round wires up real interaction beyond mouse clicks
//   on `render.rs`'s bottom nav.
//
// ── Y20-P3 (2026-08-29): real gates replace the P2 placeholder ────────────
// P2's `can_advance` was UNCONDITIONALLY `true` for every step — an honest
// placeholder documented as "no real disk pick exists yet to gate
// `DiskSelect` on... P3 will add one". This round adds exactly that, plus
// the equivalent gates for `Confirm` (the destructive-write checkbox) and
// `Progress` (the install must have actually reported `Done` — never a
// timer, never an operator's own belief that it's probably finished by now).
// Three new pieces of state land on `LiveInstallState` for this:
// `disk_scan`/`selected_disk` (`DiskSelect`), `confirm_checked` (`Confirm`),
// `install` (`Progress`) — see each type's own doc comment below. The real
// I/O that DRIVES these (an `lsblk` scan, the `duduclaw-os-install` child
// process) lives in `steps::disk_select`/`install_runner`, never here — this
// file stays pure data + pure transitions, same discipline `oobe::state`
// holds itself to.

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
    /// Y20-P3: real `lsblk` block-device enumeration + a single-select pick
    /// that gates `can_advance` — see `steps::disk_select`'s own header
    /// comment for the scan/select flow.
    DiskSelect,
    /// A confirmation summary of what's about to happen before anything
    /// destructive runs — the picked disk (from `DiskSelect`) plus an
    /// explicit destructive-write checkbox that gates `can_advance`. See
    /// `steps::confirm`'s own header comment.
    Confirm,
    /// The terminal step: real `dd`-style write progress, driven by
    /// `install_runner::start_install` (kicked off from `Confirm`'s own
    /// bottom-nav action). See `steps::progress`'s own header comment.
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

/// One candidate install-target disk (Y20-P3). Mirrors what
/// `duduclaw-os-install.sh`'s own CANDIDATES loop (its §2, `lsblk -dno
/// NAME,TYPE` filtered to `TYPE=="disk"`, `loop*`/`ram*`/`sr*`/`fd*`
/// excluded) would offer non-interactively via `DUDUCLAW_INSTALL_TARGET`.
/// `name` is the bare device basename (e.g. `"vda"`, NOT `"/dev/vda"`) —
/// exactly the shape that env var expects, so `install_runner::
/// start_install` passes it straight through with no reformatting.
///
/// KNOWN SIMPLIFICATION (disclosed, not silently dropped): the shell script
/// additionally excludes whichever disk physically carries the running live
/// medium (its own `SRC_DISK`, found via `findmnt`/`lsblk -no PKNAME`) —
/// `steps::disk_select`'s own scan does not re-derive that exclusion. In
/// every topology this round's own QEMU harness exercises, the live medium
/// is the ISO9660 `sr0` optical device, already excluded by the `sr*`
/// prefix rule, so the gap has no live consequence here. If a future
/// topology (e.g. USB) ever DID let the operator pick the source disk, the
/// shell script's own CANDIDATES check is the actual fail-closed backstop:
/// `DUDUCLAW_INSTALL_TARGET=<src>` is rejected there with "不在可安裝清單內"
/// rather than silently overwriting the medium being read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiskInfo {
    pub(crate) name: String,
    pub(crate) size: String,
    pub(crate) model: String,
}

/// `DiskSelect`'s own scan state — same three/four-state shape `oobe::
/// NetScanState` established (`NeverScanned`/`Scanning`/`Loaded`/`Failed`,
/// see `oobe::steps::network`'s header comment): I/O is always
/// click-triggered in this crate, never a render-time side effect, so this
/// step needs an explicit "not yet asked" state to render a button into
/// rather than firing `lsblk` the instant the step is first drawn.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) enum DiskScanState {
    #[default]
    NotScanned,
    Scanning,
    Loaded(Vec<DiskInfo>),
    Failed(String),
}

/// The `Progress` step's own state — set by `install_runner::start_install`
/// (kicked off from `Confirm`'s own bottom-nav action, see `render.rs`'s
/// `button_row`) and driven forward by that fn's background-thread ->
/// `cx.spawn` poll bridge. `percent: None` inside `Running` is an honest
/// indeterminate state (no `DUDUCLAW_PROGRESS:` sample has arrived yet, or
/// the target build has no `pv` so the underlying script never emits one at
/// all — see `duduclaw-os-install.sh`'s own header comment for that
/// fallback) — `steps::progress::render` paints a plain, honestly-unlabeled
/// bar for that case, never a fabricated percentage.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) enum InstallState {
    #[default]
    Idle,
    Running { percent: Option<u8>, status: String },
    Done,
    Failed(String),
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
    disk_scan: DiskScanState,
    selected_disk: Option<DiskInfo>,
    confirm_checked: bool,
    install: InstallState,
}

impl Default for LiveInstallState {
    fn default() -> Self {
        Self {
            current_step: LiveInstallStep::Language,
            completed: false,
            language: LanguageChoice::default(),
            disk_scan: DiskScanState::default(),
            selected_disk: None,
            confirm_checked: false,
            install: InstallState::default(),
        }
    }
}

/// The pure state machine — mirrors `oobe::state::OobeFlow`'s own shape
/// (owns one state struct, exposes the only ways to mutate it) at this
/// flow's much smaller scope: see this file's header comment for what's
/// deliberately simpler here (no persistence, no skip).
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

    /// No production call site reads this yet — reaching `completed` has
    /// nothing further to DO (the real terminal action, a reboot, is
    /// `install_runner::start_reboot`'s own bottom-nav action, gated
    /// directly on `InstallState::Done` rather than on this flag). Kept
    /// (not deleted): this file's own `tests` module exercises it, and it
    /// is the exact shape `oobe::state::OobeFlow::completed()` already
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

    // ── DiskSelect (Y20-P3) ─────────────────────────────────────────────

    pub(crate) fn disk_scan(&self) -> &DiskScanState {
        &self.state.disk_scan
    }

    pub(crate) fn set_disk_scanning(&mut self) {
        self.state.disk_scan = DiskScanState::Scanning;
    }

    pub(crate) fn set_disk_scan_loaded(&mut self, disks: Vec<DiskInfo>) {
        self.state.disk_scan = DiskScanState::Loaded(disks);
    }

    pub(crate) fn set_disk_scan_failed(&mut self, message: String) {
        self.state.disk_scan = DiskScanState::Failed(message);
    }

    pub(crate) fn selected_disk(&self) -> Option<&DiskInfo> {
        self.state.selected_disk.as_ref()
    }

    /// Click-to-record — same "record immediately, don't itself advance"
    /// split `set_language` above establishes.
    pub(crate) fn select_disk(&mut self, disk: DiskInfo) {
        self.state.selected_disk = Some(disk);
    }

    // ── Confirm (Y20-P3) ────────────────────────────────────────────────

    pub(crate) fn confirm_checked(&self) -> bool {
        self.state.confirm_checked
    }

    pub(crate) fn set_confirm_checked(&mut self, checked: bool) {
        self.state.confirm_checked = checked;
    }

    // ── Progress / install (Y20-P3) ─────────────────────────────────────

    pub(crate) fn install(&self) -> &InstallState {
        &self.state.install
    }

    pub(crate) fn set_install_running(&mut self, percent: Option<u8>, status: String) {
        self.state.install = InstallState::Running { percent, status };
    }

    /// Updates only the percent of the install, preserving whatever status
    /// line is already showing — the common case once numeric
    /// `DUDUCLAW_PROGRESS:` samples start arriving from `install_runner`'s
    /// poll loop. If called while NOT already `Running` (not reachable in
    /// practice — `install_runner` only emits `Progress` events after
    /// `start_install` has already set `Running`), starts a fresh `Running`
    /// with an empty status rather than panicking.
    pub(crate) fn set_install_percent(&mut self, percent: u8) {
        let status = match &self.state.install {
            InstallState::Running { status, .. } => status.clone(),
            _ => String::new(),
        };
        self.state.install = InstallState::Running { percent: Some(percent), status };
    }

    /// Same "preserve the other half" shape as `set_install_percent`, for a
    /// plain log line that carries no percentage of its own.
    pub(crate) fn set_install_status(&mut self, status: String) {
        let percent = match &self.state.install {
            InstallState::Running { percent, .. } => *percent,
            _ => None,
        };
        self.state.install = InstallState::Running { percent, status };
    }

    pub(crate) fn set_install_done(&mut self) {
        self.state.install = InstallState::Done;
    }

    pub(crate) fn set_install_failed(&mut self, message: String) {
        self.state.install = InstallState::Failed(message);
    }

    /// Whether the Progress step's install attempt ended in failure — the
    /// one condition `render.rs`'s `button_row` allows a Back click through
    /// on the Progress step: a failed write needs a way back to `Confirm`
    /// to retry (a different disk, or the same one again); a
    /// successful/idle/running install is otherwise a dead end (see
    /// `render.rs`'s own `can_back` comment).
    pub(crate) fn install_failed(&self) -> bool {
        matches!(self.state.install, InstallState::Failed(_))
    }

    /// Y20-P3: each step now has a REAL precondition (P2 left this
    /// unconditionally `true` for every step — an honest placeholder, see
    /// this file's header comment). `Language` still has none of its own;
    /// the other three read exactly the state their own step's render
    /// module is responsible for populating.
    pub(crate) fn can_advance(&self) -> bool {
        match self.state.current_step {
            LiveInstallStep::Language => true,
            LiveInstallStep::DiskSelect => self.state.selected_disk.is_some(),
            LiveInstallStep::Confirm => self.state.confirm_checked,
            LiveInstallStep::Progress => matches!(self.state.install, InstallState::Done),
        }
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

    /// Y20-P3: `next()` now genuinely gates on each step's own precondition
    /// (P2's `can_advance` was unconditionally `true` — see that method's
    /// own doc comment). This walks the full happy path, satisfying each
    /// gate in turn — exactly what an operator's real clicks would supply.
    #[test]
    fn next_advances_through_all_four_steps_once_each_gate_is_satisfied() {
        let mut flow = LiveInstallFlow::new();
        assert!(flow.next(), "Language -> DiskSelect has no precondition");
        assert_eq!(flow.current(), LiveInstallStep::DiskSelect);

        assert!(!flow.next(), "DiskSelect must refuse to advance with no disk picked");
        flow.select_disk(DiskInfo { name: "vda".to_string(), size: "20G".to_string(), model: "QEMU HARDDISK".to_string() });
        assert!(flow.next());
        assert_eq!(flow.current(), LiveInstallStep::Confirm);

        assert!(!flow.next(), "Confirm must refuse to advance until the destructive-write checkbox is ticked");
        flow.set_confirm_checked(true);
        assert!(flow.next());
        assert_eq!(flow.current(), LiveInstallStep::Progress);

        assert!(!flow.next(), "Progress must refuse to advance/complete until the install actually reports Done");
        flow.set_install_done();
        assert!(flow.next());
        assert!(flow.completed());
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

    /// Y20-P3: rewritten from P2's unconditional four-step walk — walking
    /// all the way back now needs each forward gate satisfied first, same
    /// as `next_advances_through_all_four_steps_once_each_gate_is_satisfied`
    /// above.
    #[test]
    fn back_walks_all_the_way_from_progress_to_language_once_forward_gates_are_satisfied() {
        let mut flow = LiveInstallFlow::new();
        flow.next();
        flow.select_disk(DiskInfo { name: "vda".to_string(), size: "20G".to_string(), model: String::new() });
        flow.next();
        flow.set_confirm_checked(true);
        flow.next();
        flow.set_install_done();
        flow.next();
        assert!(flow.completed());
        assert_eq!(flow.current(), LiveInstallStep::Progress, "current step stays put once completed");
        // `completed()` has no effect on `back()` — same P2 invariant, unchanged.
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
        flow.next();
        flow.select_disk(DiskInfo { name: "vda".to_string(), size: "20G".to_string(), model: String::new() });
        flow.next();
        flow.set_confirm_checked(true);
        flow.next();
        assert_eq!(flow.current(), LiveInstallStep::Progress);
        assert!(!flow.completed());
        flow.set_install_done();
        assert!(flow.next());
        assert!(flow.completed());
        assert_eq!(flow.current(), LiveInstallStep::Progress, "current step stays put once completed");
    }

    #[test]
    fn next_is_a_noop_once_completed() {
        let mut flow = LiveInstallFlow::new();
        flow.next();
        flow.select_disk(DiskInfo { name: "vda".to_string(), size: "20G".to_string(), model: String::new() });
        flow.next();
        flow.set_confirm_checked(true);
        flow.next();
        flow.set_install_done();
        flow.next();
        assert!(flow.completed());
        assert!(!flow.next());
    }

    /// Y20-P3 replaces P2's `can_advance_is_always_true_at_p2_scope` — the
    /// exact placeholder that test's own name flagged as temporary. Each
    /// step now has a real precondition; this walks all four, confirming
    /// each is `false` before its gate is satisfied and `true` after.
    #[test]
    fn can_advance_reflects_each_steps_own_precondition() {
        let mut flow = LiveInstallFlow::new();
        assert!(flow.can_advance(), "Language has no precondition");
        flow.next();

        assert!(!flow.can_advance(), "DiskSelect starts with nothing picked");
        flow.select_disk(DiskInfo { name: "vda".to_string(), size: "20G".to_string(), model: String::new() });
        assert!(flow.can_advance());
        flow.next();

        assert!(!flow.can_advance(), "Confirm starts unchecked");
        flow.set_confirm_checked(true);
        assert!(flow.can_advance());
        flow.next();

        assert!(!flow.can_advance(), "Progress starts with no install outcome");
        flow.set_install_done();
        assert!(flow.can_advance());
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

    // ── DiskSelect (Y20-P3) ────────────────────────────────────────────

    #[test]
    fn disk_scan_starts_not_scanned() {
        let flow = LiveInstallFlow::new();
        assert_eq!(*flow.disk_scan(), DiskScanState::NotScanned);
    }

    #[test]
    fn disk_scan_state_transitions() {
        let mut flow = LiveInstallFlow::new();
        flow.set_disk_scanning();
        assert_eq!(*flow.disk_scan(), DiskScanState::Scanning);
        let disks = vec![DiskInfo { name: "vda".to_string(), size: "20G".to_string(), model: "QEMU HARDDISK".to_string() }];
        flow.set_disk_scan_loaded(disks.clone());
        assert_eq!(*flow.disk_scan(), DiskScanState::Loaded(disks));
        flow.set_disk_scan_failed("lsblk not found".to_string());
        assert_eq!(*flow.disk_scan(), DiskScanState::Failed("lsblk not found".to_string()));
    }

    #[test]
    fn select_disk_records_without_advancing() {
        let mut flow = LiveInstallFlow::new();
        flow.next(); // -> DiskSelect
        assert!(flow.selected_disk().is_none());
        flow.select_disk(DiskInfo { name: "vda".to_string(), size: "20G".to_string(), model: String::new() });
        assert_eq!(flow.selected_disk().map(|d| d.name.as_str()), Some("vda"));
        assert_eq!(flow.current(), LiveInstallStep::DiskSelect, "selecting a disk must not itself advance");
    }

    // ── Confirm (Y20-P3) ────────────────────────────────────────────────

    #[test]
    fn confirm_checked_defaults_to_false() {
        assert!(!LiveInstallFlow::new().confirm_checked());
    }

    #[test]
    fn set_confirm_checked_toggles_without_advancing() {
        let mut flow = LiveInstallFlow::new();
        flow.set_confirm_checked(true);
        assert!(flow.confirm_checked());
        flow.set_confirm_checked(false);
        assert!(!flow.confirm_checked());
        assert_eq!(flow.current(), LiveInstallStep::Language);
    }

    // ── Progress / install (Y20-P3) ─────────────────────────────────────

    #[test]
    fn install_starts_idle() {
        assert_eq!(*LiveInstallFlow::new().install(), InstallState::Idle);
    }

    #[test]
    fn install_percent_and_status_update_independently() {
        let mut flow = LiveInstallFlow::new();
        flow.set_install_running(None, "準備中".to_string());
        flow.set_install_percent(42);
        assert_eq!(*flow.install(), InstallState::Running { percent: Some(42), status: "準備中".to_string() });
        flow.set_install_status("寫入中".to_string());
        assert_eq!(*flow.install(), InstallState::Running { percent: Some(42), status: "寫入中".to_string() });
    }

    #[test]
    fn install_done_and_failed_are_reachable() {
        let mut flow = LiveInstallFlow::new();
        flow.set_install_done();
        assert_eq!(*flow.install(), InstallState::Done);
        flow.set_install_failed("dd exited 1".to_string());
        assert_eq!(*flow.install(), InstallState::Failed("dd exited 1".to_string()));
    }

    #[test]
    fn install_failed_helper_matches_only_the_failed_variant() {
        let mut flow = LiveInstallFlow::new();
        assert!(!flow.install_failed());
        flow.set_install_running(None, String::new());
        assert!(!flow.install_failed());
        flow.set_install_done();
        assert!(!flow.install_failed());
        flow.set_install_failed("x".to_string());
        assert!(flow.install_failed());
    }
}
