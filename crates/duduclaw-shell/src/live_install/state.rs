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
//
// ── Installer-settings-integration WP1 (2026-08-29): Account + Theme ──────
// `commercial/docs/DESIGN-installer-settings-integration-2026-08.md` §3.1 —
// the live installer grows from 4 steps to 6, inserting `Account` and
// `Theme` right after `Language` (the two OOBE steps this design's phase 1
// pulls forward into the install-time flow; `Network` is phase 2, explicitly
// out of scope here — see the design doc's own §5). STALE as of WP3 below:
// `Network` now sits between `Language` and `Account` (design doc §3's own
// step-ordering diagram), so `Account` no longer directly follows
// `Language` — kept here as the honest historical record of what WP1 itself
// did, not corrected in place. Both new steps are PURE
// DATA COLLECTION: no gateway call happens at click time (see `Account`'s
// own doc comment below for why — it mirrors §4's "pending file + gateway
// first-boot claim" decision, not OOBE's own live `oobe::claim::
// create_account` round-trip), and no disk write happens either — a LATER
// round (`install_runner`) is what serializes whatever this struct holds
// into the JSON `duduclaw-os-install.sh` injects onto the target `/data`
// partition. `AccountError`/the `operator_name`/`account_password`/
// `account_error`/`theme` fields below, and their accessors, are that later
// round's read surface — the exact signatures the design doc's own §3.1
// commits to as a fixed contract with the parallel `install_runner`/
// `render.rs` work.
//
// ── Installer-settings-integration WP3 (2026-08-29): Network (Wi-Fi) ──────
// §5 plan (b) (the option the design doc recommends and the one implemented
// here, over plan (a) — see `steps::network`'s own header comment for the
// full "why not scan/connect live" writeup): a SEVENTH step, `Network`,
// inserted between `Language` and `Account` (design doc §3's own ordering
// diagram: "語言 → 網路(Wi-Fi) → 帳號 → 主題 → 選碟 → 確認 → 安裝"). Same
// PURE DATA COLLECTION discipline `Account`/`Theme` already established
// above: no `iwd`/D-Bus call of any kind happens from this crate at all (the
// live image does not even carry that stack — plan (a) was rejected
// specifically because rebuilding it into the live squashfs is unverified,
// high-risk work), just two more typed values held on `LiveInstallState`
// until `install_runner` serializes them into a THIRD scratch file the
// target system's own first-boot `iwd` setup consumes. Unlike `Account`,
// this step is OPTIONAL — an operator with no Wi-Fi to configure yet (wired
// network, or "I'll connect later from Settings") must be able to skip it
// with nothing typed at all, so `can_advance` for `Network` is
// unconditionally `true` (see that method's own updated doc comment below)
// and validation happens entirely inside `render.rs`'s click-time submit,
// same shape `Account`'s own `validate_and_set_account` already uses, just
// gating the OPPOSITE direction (empty is fine; PARTIALLY filled is the
// error case) — see `NetworkError`'s own doc comment for the three ways a
// non-empty submission can still be rejected.
use crate::oobe::{LanguageChoice, ThemeChoice};

/// The seven live-install wizard steps, in fixed linear order — no
/// branching, same "step order is data, not a runtime decision" discipline
/// `oobe::state::OobeStep` follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LiveInstallStep {
    /// Same three-`LanguageChoice` picker `oobe::steps::language` offers,
    /// re-derived (not literally shared) against THIS flow's own state — see
    /// `steps::language`'s own header comment for why the two can't share a
    /// click handler even though the visual row is the same shape.
    Language,
    /// Installer-settings-integration WP3: an OPTIONAL SSID + passphrase
    /// pure-form step — see this file's own header comment ("WP3") for why
    /// it sits right after `Language` (design doc §3's ordering) and why it
    /// collects rather than connects. See `steps::network`'s own header
    /// comment for the full write-up.
    Network,
    /// Installer-settings-integration WP1: collects the operator's display
    /// name + password INTO `LiveInstallState` only — no gateway round trip
    /// at click time (unlike OOBE's own `AccountCreate`, which calls
    /// `oobe::claim::create_account` immediately because a real gateway is
    /// always reachable at `127.0.0.1:18789` once OOBE is running post-boot).
    /// A live-install session has no such gateway to call (the live image
    /// "carries no gateway payload" — see the design doc's own §4), so this
    /// step's whole job is to hold the two typed values until a later round
    /// (`install_runner`) serializes them into a `pending-account.json` the
    /// TARGET system's own first-boot gateway claims. See `steps::account`'s
    /// own header comment for the full "why no submit button, why no I/O"
    /// writeup.
    Account,
    /// WP1: appearance pick — reuses `oobe::ThemeChoice`, the exact enum
    /// OOBE's own `Theme` step already persists (`oobe::selections::
    /// ThemeChoice`), so a later round's `install_runner` can serialize this
    /// flow's pick straight into the same `oobe_state.json` shape the target
    /// system's `resolve_boot_flow` already knows how to read (see the
    /// design doc's §3 diagram: "寫 oobe_state.json (completed:true + 語言/
    /// 主題)"). Unlike `Account`, THIS step's pick also reskins the
    /// remaining wizard screens live, on this very live session — see
    /// `Self::palette`'s own doc comment below.
    Theme,
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
    pub(crate) const ALL: [LiveInstallStep; 7] = [
        LiveInstallStep::Language,
        LiveInstallStep::Network,
        LiveInstallStep::Account,
        LiveInstallStep::Theme,
        LiveInstallStep::DiskSelect,
        LiveInstallStep::Confirm,
        LiveInstallStep::Progress,
    ];

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

/// The three ways `Network`'s click-time validation (`render.rs`'s
/// `validate_and_set_wifi`) can refuse to advance — unlike `AccountError`
/// below, EMPTY is not one of them (an operator who types nothing at all is
/// skipping Wi-Fi, a legal outcome — see `LiveInstallStep::Network`'s own
/// doc comment). All three only fire once the operator has typed SOMETHING:
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NetworkError {
    /// A passphrase was typed but the SSID field was left empty — an iwd
    /// connection profile needs a network name to attach the passphrase to,
    /// so this combination can never be turned into a valid
    /// `pending-network.json` (see `install_runner::build_pending_network_json`'s
    /// own doc comment).
    SsidMissingWithPsk,
    /// The typed SSID exceeds the 802.11 SSID limit — 32 **bytes**, not
    /// characters (`ssid.len()`, not `.chars().count()`; see
    /// `render.rs`'s own `validate_and_set_wifi` for why this one field uses
    /// the byte count while the passphrase check just below uses a char
    /// count).
    SsidTooLong,
    /// The typed passphrase is non-empty but outside the WPA-PSK 8..=63
    /// **character** range `duduclaw-gateway::network::validate_psk`
    /// enforces on the target system's own `iwd` connect path
    /// (`network/mod.rs`, `network/iwd.rs`) — mirrored here as the exact
    /// same client-side pre-check `AccountError::PasswordTooShort` already
    /// mirrors `handle_first_run_claim`'s own floor.
    PskLengthInvalid,
}

/// The two ways `Account`'s click-time validation (`render.rs`'s
/// `button_row`, the Account arm of its `continue_click` match) can refuse
/// to advance — mirrors OOBE's own `AccountClaimFailureKind::
/// PasswordTooShort` rule (`< 8 chars`, itself mirroring the gateway's
/// `handle_first_run_claim`) plus the empty-fields case OOBE splits into a
/// separate `ui.account_validation_error` bool. This flow folds both into
/// ONE `Option<AccountError>` on `LiveInstallState` instead of two separate
/// booleans/enums — there is exactly one status slot on this step
/// (`steps::account::render`'s own status line), so one nullable value is
/// the whole story; no call site needs to distinguish "empty AND too short"
/// from "just empty", since the empty-fields check always runs first and
/// returns before the length check is ever reached (see `render.rs`'s own
/// Account arm).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AccountError {
    /// Name and/or password were empty at submit time.
    EmptyFields,
    /// Password was non-empty but under the 8-character floor
    /// `handle_first_run_claim` (`duduclaw-gateway/src/server.rs`) enforces.
    PasswordTooShort,
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
    /// WP3: the `Network` step's typed SSID — `None` while empty/never
    /// visited, same "nothing pre-fills this" honesty `operator_name` below
    /// establishes. `Some(ssid) + psk == None` is a deliberately LEGAL
    /// combination — an open (unsecured) network — not a half-filled error
    /// state; see `set_wifi`'s own doc comment.
    wifi_ssid: Option<String>,
    /// WP3: the `Network` step's typed passphrase. `None` covers BOTH "no
    /// Wi-Fi typed at all" and "an open network was typed" — the two are
    /// disambiguated by `wifi_ssid` being `None` or `Some` respectively, same
    /// as `install_runner::build_pending_network_json`'s own `Option<&str>`
    /// parameter.
    wifi_psk: Option<String>,
    /// WP3: set by `render.rs`'s click-time validation on a rejected
    /// `Network` submit, cleared by a subsequent `set_wifi`/`clear_wifi`
    /// call — same one-nullable-value shape `account_error` below uses for
    /// `AccountError`.
    network_error: Option<NetworkError>,
    /// WP1: the `Account` step's typed name — `None` until a validated
    /// `set_account` call. See `LiveInstallStep::Account`'s own doc comment
    /// for why this never reaches a gateway from THIS struct directly.
    operator_name: Option<String>,
    /// WP1: the `Account` step's typed password, held in plain memory for
    /// the same reason `oobe::widgets::OobeTextField`'s masked fields are —
    /// this flow is the one place it lives until `install_runner` writes it
    /// out (a later round; see this file's own header comment) and the
    /// process exits.
    account_password: Option<String>,
    /// WP1: set by `render.rs`'s click-time validation on a rejected submit,
    /// cleared by a subsequent successful `set_account`. See `AccountError`'s
    /// own doc comment for why this is one nullable value, not two booleans.
    account_error: Option<AccountError>,
    /// WP1: the `Theme` step's pick — `#[default] Light`, same "the very
    /// first frame already shows this as selected" honesty `ThemeChoice::
    /// default()`'s own doc comment establishes for OOBE's identical field.
    theme: ThemeChoice,
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
            wifi_ssid: None,
            wifi_psk: None,
            network_error: None,
            operator_name: None,
            account_password: None,
            account_error: None,
            theme: ThemeChoice::default(),
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

    // ── Network (installer-settings-integration WP3) ───────────────────

    pub(crate) fn wifi_ssid(&self) -> Option<&str> {
        self.state.wifi_ssid.as_deref()
    }

    pub(crate) fn wifi_psk(&self) -> Option<&str> {
        self.state.wifi_psk.as_deref()
    }

    pub(crate) fn network_error(&self) -> Option<NetworkError> {
        self.state.network_error
    }

    /// Records a validated `Network` submission — called from `render.rs`'s
    /// `validate_and_set_wifi` ONLY after every `NetworkError` case has
    /// already been ruled out (see that fn's own doc comment). `psk: None`
    /// is the honest "open network" outcome (an operator can legitimately
    /// type an SSID and leave the passphrase blank); it is NOT how "nothing
    /// was typed at all" is represented — that path goes through
    /// `clear_wifi` instead, never this fn. Clears any previously-set
    /// `network_error`, same "success clears the prior failure" contract
    /// `set_account` establishes for `account_error` below.
    pub(crate) fn set_wifi(&mut self, ssid: String, psk: Option<String>) {
        self.state.wifi_ssid = Some(ssid);
        self.state.wifi_psk = psk;
        self.state.network_error = None;
    }

    /// The "operator is skipping Wi-Fi" outcome — both typed fields go back
    /// to `None` (whether they were already empty, or the operator typed
    /// something and then cleared it before advancing) and any stale error
    /// is cleared too, so a later re-visit of `Network` (via `back()`) never
    /// shows a validation complaint left over from a submission that was
    /// ultimately abandoned in favor of skipping.
    pub(crate) fn clear_wifi(&mut self) {
        self.state.wifi_ssid = None;
        self.state.wifi_psk = None;
        self.state.network_error = None;
    }

    pub(crate) fn set_network_error(&mut self, err: NetworkError) {
        self.state.network_error = Some(err);
    }

    // ── Account (installer-settings-integration WP1) ───────────────────

    pub(crate) fn operator_name(&self) -> Option<&str> {
        self.state.operator_name.as_deref()
    }

    pub(crate) fn account_password(&self) -> Option<&str> {
        self.state.account_password.as_deref()
    }

    pub(crate) fn account_error(&self) -> Option<AccountError> {
        self.state.account_error
    }

    /// Records BOTH typed fields at once — unlike `set_language`'s
    /// single-field click-to-record, this flow's only caller is `render.rs`'s
    /// click-time validation (see that file's own header comment and
    /// `steps::account`'s for why validation happens there, not here): by
    /// the time this is called, both values have already passed validation
    /// TOGETHER, so there is no partial/invalid state worth a setter for.
    /// Clears any previously-set `account_error` — a validated submit
    /// supersedes whatever the LAST attempt complained about, same
    /// "success clears the prior failure" contract OOBE's own
    /// `apply_claim_result` establishes for `AccountClaimState`.
    pub(crate) fn set_account(&mut self, name: String, password: String) {
        self.state.operator_name = Some(name);
        self.state.account_password = Some(password);
        self.state.account_error = None;
    }

    pub(crate) fn set_account_error(&mut self, err: AccountError) {
        self.state.account_error = Some(err);
    }

    // ── Theme (installer-settings-integration WP1) ─────────────────────

    pub(crate) fn theme_choice(&self) -> ThemeChoice {
        self.state.theme
    }

    /// Click-to-record, same split every other setter in this file uses —
    /// see `Self::palette` below for how this pick reaches the screen on
    /// the very next render call.
    pub(crate) fn set_theme(&mut self, theme: ThemeChoice) {
        self.state.theme = theme;
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
    /// `Theme` (WP1) has none either — it has a default (`Light`), same
    /// reasoning `oobe::state::OobeFlow::can_advance` gives `Theme` there.
    /// WP3's `Network` is ALSO unconditionally `true` — but for a different
    /// reason than `Theme`: it is genuinely OPTIONAL (see
    /// `LiveInstallStep::Network`'s own doc comment), not merely defaulted,
    /// so an empty step must be allowed through exactly as freely as a
    /// filled one. This is defense in depth only — `render.rs`'s
    /// `continue_disabled` never actually reads this arm either (same
    /// "click-time validation, not a live-content-driven disabled state"
    /// shape `Account`'s own arm already established, see that file's own
    /// header comment) — real rejection happens inside
    /// `validate_and_set_wifi`, which calls `LiveInstallFlow::next()`
    /// only once it has already decided the submission is valid. The other
    /// four read exactly the state their own step's render module is
    /// responsible for populating.
    pub(crate) fn can_advance(&self) -> bool {
        match self.state.current_step {
            LiveInstallStep::Language => true,
            LiveInstallStep::Network => true,
            LiveInstallStep::Account => self.state.operator_name.is_some() && self.state.account_password.is_some(),
            LiveInstallStep::Theme => true,
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

    /// Installer-settings-integration WP1: resolves against the operator's
    /// OWN `Theme` step pick instead of P2/P3's unconditional light default
    /// — same "selecting a theme reskins every OTHER screen on the very
    /// next render call" live-repaint discipline `oobe::state::OobeFlow::
    /// palette()` already establishes for OOBE (see that method's own doc
    /// comment): nothing here caches a palette across renders, it is
    /// resolved fresh from `self.state.theme` on every call, so a click on
    /// `Theme` reskins the wizard's remaining steps starting the very next
    /// frame. `pub(crate)` (not `pub(super)`): `live_install::render`/
    /// `steps::*` are siblings of this file within `live_install`, same
    /// reach `OobeFlow::palette()`'s own doc comment explains for its own
    /// `pub(super)`.
    pub(crate) fn palette(&self) -> crate::palette::ShellPalette {
        crate::palette::ShellPalette::for_choice(self.state.theme)
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
    fn all_has_seven_steps_in_declared_order() {
        assert_eq!(LiveInstallStep::ALL.len(), 7);
        assert_eq!(LiveInstallStep::ALL[0], LiveInstallStep::Language);
        assert_eq!(LiveInstallStep::ALL[1], LiveInstallStep::Network);
        assert_eq!(LiveInstallStep::ALL[2], LiveInstallStep::Account);
        assert_eq!(LiveInstallStep::ALL[3], LiveInstallStep::Theme);
        assert_eq!(LiveInstallStep::ALL[4], LiveInstallStep::DiskSelect);
        assert_eq!(LiveInstallStep::ALL[5], LiveInstallStep::Confirm);
        assert_eq!(LiveInstallStep::ALL[6], LiveInstallStep::Progress);
    }

    #[test]
    fn index_and_from_index_round_trip_for_every_step() {
        for step in LiveInstallStep::ALL {
            assert_eq!(LiveInstallStep::from_index(step.index()), Some(step));
        }
    }

    #[test]
    fn next_walks_forward_through_all_seven_steps_in_declared_order() {
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

    /// WP1 moved `DiskSelect` from index 1 to index 3; WP3 shifts it again
    /// (index 3 -> index 4) by inserting `Network` ahead of `Account` — its
    /// `prev()` is still `Theme`, unaffected by where THIS test's own step
    /// sits relative to `Network`.
    #[test]
    fn disk_select_prev_is_theme() {
        assert_eq!(LiveInstallStep::DiskSelect.prev(), Some(LiveInstallStep::Theme));
    }

    /// WP3 replaces the P3/WP1-era `account_prev_is_language` — `Network`
    /// now sits between the two, so `Account`'s `prev()` changes from
    /// `Language` to `Network`.
    #[test]
    fn account_prev_is_network() {
        assert_eq!(LiveInstallStep::Account.prev(), Some(LiveInstallStep::Network));
    }

    #[test]
    fn theme_prev_is_account() {
        assert_eq!(LiveInstallStep::Theme.prev(), Some(LiveInstallStep::Account));
    }

    /// WP3's own insert: `Network` sits directly after `Language`.
    #[test]
    fn network_prev_is_language() {
        assert_eq!(LiveInstallStep::Network.prev(), Some(LiveInstallStep::Language));
    }

    // ── LiveInstallFlow: next / back / complete ────────────────────────

    #[test]
    fn starts_at_language_not_completed() {
        let flow = LiveInstallFlow::new();
        assert_eq!(flow.current(), LiveInstallStep::Language);
        assert!(!flow.completed());
    }

    /// Y20-P3 (extended WP1, extended WP3): `next()` genuinely gates on each
    /// step's own precondition. This walks the full happy path, satisfying
    /// each gate in turn — exactly what an operator's real clicks would
    /// supply — now through all seven steps. `Network` never refuses (see
    /// `can_advance`'s own doc comment) — this walk-through passes through
    /// it with nothing typed at all, the "skip Wi-Fi" outcome.
    #[test]
    fn next_advances_through_all_seven_steps_once_each_gate_is_satisfied() {
        let mut flow = LiveInstallFlow::new();
        assert!(flow.next(), "Language -> Network has no precondition");
        assert_eq!(flow.current(), LiveInstallStep::Network);

        assert!(flow.next(), "Network is optional, so it never blocks advancing even with nothing typed");
        assert_eq!(flow.current(), LiveInstallStep::Account);

        assert!(!flow.next(), "Account must refuse to advance with no name/password set");
        flow.set_account("operator".to_string(), "hunter2-pw".to_string());
        assert!(flow.next());
        assert_eq!(flow.current(), LiveInstallStep::Theme);

        assert!(flow.next(), "Theme has a default pick, so it never blocks advancing");
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

    /// WP1 extends P3's rewrite; WP3 extends it again — walking all the way
    /// back now needs all six forward gates satisfied first (`Network`
    /// included, though it needs nothing typed), and walking back all the
    /// way to `Language` takes six `back()` calls instead of five.
    #[test]
    fn back_walks_all_the_way_from_progress_to_language_once_forward_gates_are_satisfied() {
        let mut flow = LiveInstallFlow::new();
        flow.next(); // Language -> Network
        flow.next(); // Network -> Account
        flow.set_account("operator".to_string(), "hunter2-pw".to_string());
        flow.next(); // Account -> Theme
        flow.next(); // Theme -> DiskSelect
        flow.select_disk(DiskInfo { name: "vda".to_string(), size: "20G".to_string(), model: String::new() });
        flow.next(); // DiskSelect -> Confirm
        flow.set_confirm_checked(true);
        flow.next(); // Confirm -> Progress
        flow.set_install_done();
        flow.next(); // Progress -> completed
        assert!(flow.completed());
        assert_eq!(flow.current(), LiveInstallStep::Progress, "current step stays put once completed");
        // `completed()` has no effect on `back()` — same P2/P3 invariant, unchanged.
        assert!(flow.back());
        assert_eq!(flow.current(), LiveInstallStep::Confirm);
        assert!(flow.back());
        assert_eq!(flow.current(), LiveInstallStep::DiskSelect);
        assert!(flow.back());
        assert_eq!(flow.current(), LiveInstallStep::Theme);
        assert!(flow.back());
        assert_eq!(flow.current(), LiveInstallStep::Account);
        assert!(flow.back());
        assert_eq!(flow.current(), LiveInstallStep::Network);
        assert!(flow.back());
        assert_eq!(flow.current(), LiveInstallStep::Language);
        assert!(!flow.back());
    }

    #[test]
    fn next_on_progress_completes_the_flow() {
        let mut flow = LiveInstallFlow::new();
        flow.next(); // Language -> Network
        flow.next(); // Network -> Account
        flow.set_account("operator".to_string(), "hunter2-pw".to_string());
        flow.next();
        flow.next(); // Theme -> DiskSelect
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
        flow.next(); // Language -> Network
        flow.next(); // Network -> Account
        flow.set_account("operator".to_string(), "hunter2-pw".to_string());
        flow.next();
        flow.next(); // Theme -> DiskSelect
        flow.select_disk(DiskInfo { name: "vda".to_string(), size: "20G".to_string(), model: String::new() });
        flow.next();
        flow.set_confirm_checked(true);
        flow.next();
        flow.set_install_done();
        flow.next();
        assert!(flow.completed());
        assert!(!flow.next());
    }

    /// WP1 replaces P3's `can_advance_reflects_each_steps_own_precondition`;
    /// WP3 extends it again — now walks all seven steps, including
    /// `Network`, which (like `Theme`) never blocks, but for the different
    /// "genuinely optional" reason `can_advance`'s own doc comment explains.
    #[test]
    fn can_advance_reflects_each_steps_own_precondition() {
        let mut flow = LiveInstallFlow::new();
        assert!(flow.can_advance(), "Language has no precondition");
        flow.next();

        assert!(flow.can_advance(), "Network is optional, so it is never blocked even with nothing typed");
        flow.next();

        assert!(!flow.can_advance(), "Account starts with no name/password set");
        flow.set_account("operator".to_string(), "hunter2-pw".to_string());
        assert!(flow.can_advance());
        flow.next();

        assert!(flow.can_advance(), "Theme has a default pick, so it is never blocked");
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

    // ── Network (installer-settings-integration WP3) ───────────────────

    #[test]
    fn wifi_fields_start_empty() {
        let flow = LiveInstallFlow::new();
        assert_eq!(flow.wifi_ssid(), None);
        assert_eq!(flow.wifi_psk(), None);
        assert_eq!(flow.network_error(), None);
    }

    #[test]
    fn set_wifi_records_ssid_and_psk_without_advancing() {
        let mut flow = LiveInstallFlow::new();
        flow.next(); // -> Network
        flow.set_wifi("DuDu-Office".to_string(), Some("hunter2-pw".to_string()));
        assert_eq!(flow.wifi_ssid(), Some("DuDu-Office"));
        assert_eq!(flow.wifi_psk(), Some("hunter2-pw"));
        assert_eq!(flow.current(), LiveInstallStep::Network, "recording Wi-Fi must not itself advance");
    }

    /// `Some(ssid) + None` is the legal "open network" combination — see
    /// `set_wifi`'s own doc comment. Distinct from `wifi_fields_start_empty`
    /// above, where BOTH are `None`.
    #[test]
    fn set_wifi_with_no_psk_records_an_open_network() {
        let mut flow = LiveInstallFlow::new();
        flow.next(); // -> Network
        flow.set_wifi("DuDu-Guest".to_string(), None);
        assert_eq!(flow.wifi_ssid(), Some("DuDu-Guest"));
        assert_eq!(flow.wifi_psk(), None);
    }

    #[test]
    fn set_wifi_clears_a_prior_error() {
        let mut flow = LiveInstallFlow::new();
        flow.next(); // -> Network
        flow.set_network_error(NetworkError::SsidTooLong);
        assert_eq!(flow.network_error(), Some(NetworkError::SsidTooLong));
        flow.set_wifi("DuDu-Office".to_string(), Some("hunter2-pw".to_string()));
        assert_eq!(flow.network_error(), None, "a validated submit must clear whatever the prior attempt complained about");
    }

    #[test]
    fn set_network_error_records_without_advancing() {
        let mut flow = LiveInstallFlow::new();
        flow.next(); // -> Network
        flow.set_network_error(NetworkError::PskLengthInvalid);
        assert_eq!(flow.network_error(), Some(NetworkError::PskLengthInvalid));
        assert_eq!(flow.current(), LiveInstallStep::Network);
    }

    /// `clear_wifi` is the "skip Wi-Fi" outcome — both fields go back to
    /// `None`, whether or not anything had been typed, and any stale error
    /// from an abandoned submission is dropped too.
    #[test]
    fn clear_wifi_resets_both_fields_and_any_error() {
        let mut flow = LiveInstallFlow::new();
        flow.next(); // -> Network
        flow.set_wifi("DuDu-Office".to_string(), Some("hunter2-pw".to_string()));
        flow.set_network_error(NetworkError::SsidTooLong);
        flow.clear_wifi();
        assert_eq!(flow.wifi_ssid(), None);
        assert_eq!(flow.wifi_psk(), None);
        assert_eq!(flow.network_error(), None);
    }

    #[test]
    fn network_can_advance_even_with_nothing_typed() {
        let mut flow = LiveInstallFlow::new();
        flow.next(); // -> Network
        assert!(flow.can_advance(), "Network is optional — an empty step must not block advancing");
    }

    // ── Account (installer-settings-integration WP1) ───────────────────

    #[test]
    fn account_fields_start_empty() {
        let flow = LiveInstallFlow::new();
        assert_eq!(flow.operator_name(), None);
        assert_eq!(flow.account_password(), None);
        assert_eq!(flow.account_error(), None);
    }

    #[test]
    fn set_account_records_both_fields_without_advancing() {
        let mut flow = LiveInstallFlow::new();
        flow.next(); // Language -> Network
        flow.next(); // Network -> Account
        flow.set_account("operator".to_string(), "hunter2-pw".to_string());
        assert_eq!(flow.operator_name(), Some("operator"));
        assert_eq!(flow.account_password(), Some("hunter2-pw"));
        assert_eq!(flow.current(), LiveInstallStep::Account, "recording the account must not itself advance");
    }

    #[test]
    fn set_account_clears_a_prior_error() {
        let mut flow = LiveInstallFlow::new();
        flow.next(); // Language -> Network
        flow.next(); // Network -> Account
        flow.set_account_error(AccountError::EmptyFields);
        assert_eq!(flow.account_error(), Some(AccountError::EmptyFields));
        flow.set_account("operator".to_string(), "hunter2-pw".to_string());
        assert_eq!(flow.account_error(), None, "a validated submit must clear whatever the prior attempt complained about");
    }

    #[test]
    fn set_account_error_records_without_advancing() {
        let mut flow = LiveInstallFlow::new();
        flow.next(); // Language -> Network
        flow.next(); // Network -> Account
        flow.set_account_error(AccountError::PasswordTooShort);
        assert_eq!(flow.account_error(), Some(AccountError::PasswordTooShort));
        assert_eq!(flow.current(), LiveInstallStep::Account);
    }

    // ── Theme (installer-settings-integration WP1) ─────────────────────

    #[test]
    fn theme_defaults_to_light() {
        let flow = LiveInstallFlow::new();
        assert_eq!(flow.theme_choice(), ThemeChoice::Light);
    }

    #[test]
    fn set_theme_records_the_choice_without_advancing() {
        let mut flow = LiveInstallFlow::new();
        flow.set_theme(ThemeChoice::Dark);
        assert_eq!(flow.theme_choice(), ThemeChoice::Dark);
        assert_eq!(flow.current(), LiveInstallStep::Language, "click-to-record must not itself advance");
    }

    #[test]
    fn palette_follows_the_selected_theme() {
        let mut flow = LiveInstallFlow::new();
        assert_eq!(flow.palette(), crate::palette::ShellPalette::for_choice(ThemeChoice::Light));
        flow.set_theme(ThemeChoice::Dark);
        assert_eq!(flow.palette(), crate::palette::ShellPalette::for_choice(ThemeChoice::Dark));
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
        let mut flow = disk_select_flow();
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

    // ── helpers: walk to a given step, satisfying preconditions ──────
    // Same shape `oobe::state`'s own test module establishes for its
    // multi-precondition walk-throughs (`runtime_auth_flow`/`templates_flow`
    // et al.) — see that module's own test section for the pattern this
    // mirrors.

    fn disk_select_flow() -> LiveInstallFlow {
        let mut flow = LiveInstallFlow::new();
        while flow.current() != LiveInstallStep::DiskSelect {
            if flow.current() == LiveInstallStep::Account {
                flow.set_account("operator".to_string(), "hunter2-pw".to_string());
            }
            flow.next();
        }
        flow
    }
}
