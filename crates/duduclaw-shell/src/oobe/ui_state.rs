// OOBE ephemeral UI-state — split out of `oobe/mod.rs` (WP-OOBE-split,
// 2026-08-21, see `state.rs`'s own header comment for the full four-way
// split this round makes). Carries `OobeUiState` itself plus the two
// `AccountClaim*` enums it holds — NOT part of `OobeState`/persistence
// (see `OobeUiState`'s own doc comment below for that split), reset on
// every process launch. The `Network` step's own three ephemeral enums
// (`NetScanState`/`NetConnectState`/`NetConnectFailureKind`) already live
// in their own sibling file, `network_ui.rs`, split out in an EARLIER
// round (Shell-S3, see that file's own header comment) — `OobeUiState`
// here still owns the fields that hold them and the `set_net_*`/
// `start_net_*` methods that mutate them; only the enum DEFINITIONS live
// elsewhere, unchanged from before this round.

use super::network;
use super::network_ui::{NetConnectFailureKind, NetConnectState, NetScanState};

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
pub enum RuntimeClaimFailureKind {
    /// Nothing typed.
    Empty,
    /// Too short to be any provider's key — caught before any round trip.
    LooksWrong,
    /// The gateway refused the call or could not be reached.
    Unreachable,
}

/// Lifecycle of the `RuntimeAuth` step's "儲存金鑰" click — same shape as
/// `AccountClaimState` below (the render fn reads it to label the button
/// and show the one status line; the click handler guards on `InFlight`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RuntimeClaimState {
    #[default]
    Idle,
    InFlight,
    /// `accounts.add` accepted the key. Stored, not yet proven: the first
    /// real dispatch is what validates it against the provider.
    Saved,
    Failed(RuntimeClaimFailureKind),
}

/// Why a CLI login on the `RuntimeAuth` step did not end in an authorized
/// provider — WP-C (2026-09-05). Four kinds because the operator's next
/// action genuinely differs: install/no-such-login is a dead end on this
/// machine, a transport failure is worth retrying, a refusal means wrong
/// credentials, and an abandoned session means "you closed the browser".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeLoginFailureKind {
    /// The gateway refused to start: the CLI is not installed on this
    /// machine, or that runtime has no interactive login at all.
    Unavailable,
    /// Could not reach the local service, or the session never settled
    /// inside its budget.
    Unreachable,
    /// The CLI itself reported an authentication failure.
    Refused,
    /// The CLI exited without succeeding — cancelled, or abandoned in the
    /// browser.
    Abandoned,
}

/// The `RuntimeAuth` step's 「登入帳號」 flow — WP-C (2026-09-05).
///
/// One state for the WHOLE step, not one per row: a PTY login owns a real
/// process on the appliance, and letting an operator start ten at once would
/// be a way to wedge the machine, not a feature. Whichever provider is
/// mid-flow is named inside the variant, so the row that started it is the
/// only one that renders the panel.
///
/// `&'static str` for the provider because it always comes from
/// `oobe::runtime_providers::PROVIDERS` — a row that does not exist cannot
/// be put into this state.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum RuntimeLoginState {
    #[default]
    Idle,
    /// The §1-1 risk disclosure is on screen and the operator has not (or
    /// has, once `acknowledged`) taken it on. NOTHING is sent to the gateway
    /// in this state — see `RuntimeLoginState::start_enabled`.
    Disclosure { provider: &'static str, acknowledged: bool },
    /// `auth.cli_login.start` is in flight.
    Starting { provider: &'static str },
    /// The gateway spawned the CLI. `session_id` is `None` for the instant
    /// between "we sent start" and "the response came back" — the cancel
    /// button has nothing to cancel until it fills in.
    Running { provider: &'static str, session_id: Option<String>, url: Option<String>, code: Option<String> },
    Succeeded { provider: &'static str },
    Failed { provider: &'static str, kind: RuntimeLoginFailureKind },
}

impl RuntimeLoginState {
    /// Which provider's row owns the panel right now, if any.
    pub fn provider(&self) -> Option<&'static str> {
        match self {
            RuntimeLoginState::Idle => None,
            RuntimeLoginState::Disclosure { provider, .. }
            | RuntimeLoginState::Starting { provider }
            | RuntimeLoginState::Running { provider, .. }
            | RuntimeLoginState::Succeeded { provider }
            | RuntimeLoginState::Failed { provider, .. } => Some(*provider),
        }
    }

    /// The gate the TODO's decision 1B asks for: a subscription login may
    /// only START once the operator has ticked 「我了解風險，由我自行承擔」.
    /// Pure, and the whole reason this is a method rather than an inline
    /// check inside a click handler — see `disclosure_gates_the_login_start`
    /// in this file's own tests.
    ///
    /// Every state other than an ACKNOWLEDGED disclosure is `false`,
    /// including the in-flight ones: a second click while a session is
    /// already running must not spawn a second PTY.
    pub fn start_enabled(&self) -> bool {
        matches!(self, RuntimeLoginState::Disclosure { acknowledged: true, .. })
    }

    /// The live session id, for the cancel button and for tearing a session
    /// down when the operator leaves the step.
    pub fn session_id(&self) -> Option<&str> {
        match self {
            RuntimeLoginState::Running { session_id, .. } => session_id.as_deref(),
            _ => None,
        }
    }

    /// Is a login occupying the machine right now? Used to keep a second
    /// row's 「登入帳號」 from starting one on top of it.
    pub fn is_busy(&self) -> bool {
        matches!(self, RuntimeLoginState::Starting { .. } | RuntimeLoginState::Running { .. })
    }
}

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
///
/// No longer `Copy` as of Shell-S3 (2026-08-21) — `net_scan`'s `Loaded`
/// variant carries an owned `Vec<network::AccessPoint>`, which isn't `Copy`.
/// Checked before this round: nothing calls sites relied on `OobeUiState`
/// being `Copy` (every existing use already passes it by reference or
/// constructs a fresh value), so dropping the derive is a pure widening,
/// not a behavior change.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OobeUiState {
    /// Whether the `LanguageAccessibility` step's inline "輔助使用設定"
    /// entry is expanded — task brief: "無障礙入口（視覺入口，點開佔位）".
    pub accessibility_open: bool,
    /// `AccountCreate`'s "建立帳號" click validates both real `OobeTextField`
    /// entries at CLICK time (`this.field.read(cx).content(cx)`, same pattern
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
    /// See `RuntimeClaimState`.
    pub runtime_claim: RuntimeClaimState,
    /// WP-C (2026-09-05): which provider row has its masked API-key field
    /// expanded, if any. At most one at a time — the step owns exactly ONE
    /// `OobeTextField` entity (`RuntimeAuthFields`, created at window-open),
    /// so two rows cannot both host it, and expanding a second row collapses
    /// the first. `None` = every row collapsed.
    pub runtime_key_provider: Option<&'static str>,
    /// WP-C: the 「登入帳號」 flow — see `RuntimeLoginState`.
    pub runtime_login: RuntimeLoginState,
    /// `Network` step's scan progress — see `NetScanState`'s own doc
    /// comment.
    pub net_scan: NetScanState,
    /// Which backend the CURRENT (or most recent) scan actually used — see
    /// `network::NetBackendKind`'s own doc comment. `None` before the
    /// first scan attempt settles, so `steps::network` renders no
    /// demo-mode notice at all until there's a real answer either way
    /// (never defaults to assuming Real OR Fake).
    pub net_backend_kind: Option<network::NetBackendKind>,
    /// Which scanned SSID the connect flow is currently working on — see
    /// `NetConnectState`'s own doc comment.
    pub net_selected_ssid: Option<String>,
    /// Whether `net_selected_ssid` is a secured network — captured at
    /// SELECTION time (the row click already has the `network::
    /// AccessPoint` in hand) rather than re-derived from the current scan
    /// list at render time, which could be stale or empty after a rescan.
    /// `steps::network` reads this to decide whether the connect panel
    /// shows the PSK field at all.
    pub net_selected_secured: bool,
    pub net_connect: NetConnectState,
    /// D4a-5 (2026-08-23): the last-fetched overall connectivity snapshot
    /// (`GET /api/first-run/network/status`, `network::NetworkStatus`) —
    /// fetched alongside a Wi-Fi scan (`kick_off_scan`, `steps::network`)
    /// since it's the same background thread and the same gateway round
    /// trip, not a separate click. `None` before the first scan attempt
    /// settles, OR whenever the most recent fetch failed (see `kick_off_
    /// scan`'s own comment on that call site for why a failed refresh
    /// clears this rather than keeping a stale value — honesty over
    /// continuity). Ephemeral like every other field here: a wired cable
    /// being unplugged between renders must be re-observed on the next
    /// scan, never remembered from an earlier point in this process's life.
    pub net_status: Option<network::NetworkStatus>,
    /// The gateway said first-run setup already completed on this machine
    /// (`NetError::FirstRunCompleted`) — OOBE is being re-run. Counts as
    /// online in `wired_online()` so the Network step is passable.
    pub net_first_run_done: bool,
    /// D4a-7 (2026-08-31, QEMU wired-only OOBE deadlock): set when the
    /// operator clicks Continue on the `Network` step while `OobeFlow::
    /// can_advance_with_wired` is still false — see `render.rs`'s
    /// `continue_click` (the one setter, via `steps::network::handle_
    /// blocked_continue`) and `steps::network::continue_blocked_notice` (the
    /// one reader, gated through `should_show_net_continue_blocked_notice`
    /// below) for the two call sites. Deliberately a real ui_state field,
    /// NOT re-derived purely from `can_advance_with_wired` at render time —
    /// the notice must stay silent on the step's very first frame (before
    /// any click), only appearing once a click has actually been blocked.
    /// Reset back to `false` on a fresh visit to the step
    /// (`steps::network::on_step_entered`) so a stale notice from an
    /// EARLIER visit never reappears before this visit's own first blocked
    /// click.
    pub net_continue_blocked: bool,
}

impl OobeUiState {
    pub fn toggle_accessibility(&mut self) {
        self.accessibility_open = !self.accessibility_open;
    }

    pub fn set_account_validation_error(&mut self, on: bool) {
        self.account_validation_error = on;
    }

    pub fn set_runtime_claim(&mut self, state: RuntimeClaimState) {
        self.runtime_claim = state;
    }

    // ── WP-C (2026-09-05): the provider list's two actions ──────────────

    /// Expands `provider`'s API-key field, or collapses it if it was already
    /// the expanded one. Always resets `runtime_claim` back to `Idle`: a
    /// "金鑰已儲存"/"這不像 API 金鑰" line left over from ANOTHER provider's
    /// attempt would read as this row's own status.
    pub fn toggle_runtime_key_field(&mut self, provider: &'static str) {
        self.runtime_key_provider = if self.runtime_key_provider == Some(provider) { None } else { Some(provider) };
        self.runtime_claim = RuntimeClaimState::Idle;
    }

    pub fn close_runtime_key_field(&mut self) {
        self.runtime_key_provider = None;
        self.runtime_claim = RuntimeClaimState::Idle;
    }

    /// Opens the §1-1 risk disclosure for `provider`, un-acknowledged. Also
    /// collapses any expanded key field — the two panels are alternatives,
    /// and showing both at once would make the card taller than the step.
    pub fn open_runtime_login_disclosure(&mut self, provider: &'static str) {
        self.runtime_key_provider = None;
        self.runtime_claim = RuntimeClaimState::Idle;
        self.runtime_login = RuntimeLoginState::Disclosure { provider, acknowledged: false };
    }

    /// The 「我了解風險，由我自行承擔」 checkbox. A no-op in every other
    /// state, so a stray click cannot acknowledge a disclosure that is not
    /// on screen.
    pub fn toggle_runtime_login_acknowledged(&mut self) {
        if let RuntimeLoginState::Disclosure { provider, acknowledged } = self.runtime_login {
            self.runtime_login = RuntimeLoginState::Disclosure { provider, acknowledged: !acknowledged };
        }
    }

    /// Moves an ACKNOWLEDGED disclosure into the in-flight state. Refuses
    /// (returns `None`, changing nothing) from any other state — this is the
    /// single gate the click handler consults, so the "no login before the
    /// operator takes the risk" rule cannot be bypassed by a second call
    /// site forgetting to check.
    pub fn start_runtime_login(&mut self) -> Option<&'static str> {
        if !self.runtime_login.start_enabled() {
            return None;
        }
        let provider = self.runtime_login.provider()?;
        self.runtime_login = RuntimeLoginState::Starting { provider };
        Some(provider)
    }

    /// The gateway accepted the start and named a session.
    pub fn set_runtime_login_session(&mut self, session_id: String) {
        if let Some(provider) = self.runtime_login.provider() {
            if self.runtime_login.is_busy() {
                self.runtime_login = RuntimeLoginState::Running { provider, session_id: Some(session_id), url: None, code: None };
            }
        }
    }

    /// A parsed URL / device code from the CLI's transcript.
    pub fn set_runtime_login_prompt(&mut self, url: Option<String>, code: Option<String>) {
        if let RuntimeLoginState::Running { url: slot_url, code: slot_code, .. } = &mut self.runtime_login {
            *slot_url = url;
            *slot_code = code;
        }
    }

    pub fn set_runtime_login_succeeded(&mut self) {
        if let Some(provider) = self.runtime_login.provider() {
            self.runtime_login = RuntimeLoginState::Succeeded { provider };
        }
    }

    pub fn set_runtime_login_failed(&mut self, kind: RuntimeLoginFailureKind) {
        if let Some(provider) = self.runtime_login.provider() {
            self.runtime_login = RuntimeLoginState::Failed { provider, kind };
        }
    }

    /// Dismisses the login panel entirely. The caller is responsible for
    /// having cancelled any live session FIRST — this only forgets it.
    pub fn close_runtime_login(&mut self) {
        self.runtime_login = RuntimeLoginState::Idle;
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

    // ── Network step (Shell-S3) ────────────────────────────────────────

    pub fn set_net_scanning(&mut self) {
        self.net_scan = NetScanState::Scanning;
    }

    /// Records a settled scan — the loaded AP list AND which backend
    /// produced it, together, so `steps::network` can never render one
    /// without the other being in sync (see `net_backend_kind`'s own doc
    /// comment for why a stale/mismatched kind would be dishonest).
    pub fn set_net_scan_loaded(&mut self, aps: Vec<network::AccessPoint>, kind: network::NetBackendKind) {
        self.net_scan = NetScanState::Loaded(aps);
        self.net_backend_kind = Some(kind);
    }

    pub fn set_net_scan_failed(&mut self, kind: network::NetBackendKind) {
        self.net_scan = NetScanState::Failed;
        self.net_backend_kind = Some(kind);
    }

    /// A secured row was clicked — see `NetConnectState::AwaitingPsk`'s own
    /// doc comment for why a secured network stops here instead of
    /// connecting immediately. Only ever called for a secured AP (an open
    /// one goes straight to `set_net_connecting`), so `net_selected_secured`
    /// is unconditionally `true` here.
    pub fn start_net_awaiting_psk(&mut self, ssid: &str) {
        self.net_selected_ssid = Some(ssid.to_string());
        self.net_selected_secured = true;
        self.net_connect = NetConnectState::AwaitingPsk;
    }

    /// `secured` is threaded through explicitly (not re-derived) because
    /// this is called for BOTH directions: straight from a row click (open
    /// networks) and from the PSK panel's "連線" click (secured networks,
    /// after `start_net_awaiting_psk` already ran once for the same SSID).
    pub fn set_net_connecting(&mut self, ssid: &str, secured: bool) {
        self.net_selected_ssid = Some(ssid.to_string());
        self.net_selected_secured = secured;
        self.net_connect = NetConnectState::Connecting;
    }

    pub fn set_net_connect_failed(&mut self, kind: NetConnectFailureKind) {
        self.net_connect = NetConnectState::Failed(kind);
    }

    /// Clears the connect flow back to `Idle` — called on a settled
    /// success (the row itself now shows "已連線" from `OobeSelections`,
    /// this transient progress has nothing left to track) and on the
    /// operator canceling a PSK prompt. Deliberately leaves
    /// `net_selected_ssid` untouched on a cancel-vs-success distinction —
    /// see `steps::network`'s own cancel handler for why it clears that
    /// field itself rather than sharing this method.
    pub fn reset_net_connect(&mut self) {
        self.net_connect = NetConnectState::Idle;
    }

    pub fn clear_net_selected_ssid(&mut self) {
        self.net_selected_ssid = None;
    }

    /// D4a §5.4-2 (2026-08-23): whether the machine currently has SOME
    /// non-Wi-Fi-join route to the internet (wired ethernet, most commonly)
    /// that should let the operator past the `Network` step without ever
    /// picking a Wi-Fi row. Derived from `net_status`, not a second stored
    /// field — one source of truth (same reasoning `network::AccessPoint::
    /// secured()` already applies to itself).
    ///
    /// Combines TWO signals from the same snapshot, deliberately: `internet.
    /// counts_as_connected()` (online OR portal) AND `has_ip` non-empty —
    /// belt-and-suspenders (D4a §5.4-2's own wording: "internet 欄位…＋
    /// ip.addresses 非空"), since an `internet` verdict without an address
    /// would itself be a contradiction worth not trusting blindly.
    ///
    /// See `OobeFlow::can_advance_with_wired`'s own doc comment for why this
    /// lives on the EPHEMERAL side (`OobeUiState`) and is combined with the
    /// PERSISTED `network_connected` flag only at the point of deciding,
    /// never merged into it — a wired connection is an environmental fact,
    /// not a user selection, and must not survive a restart with the cable
    /// unplugged.
    pub fn wired_online(&self) -> bool {
        self.net_first_run_done || self.net_status.as_ref().is_some_and(|s| s.internet.counts_as_connected() && s.has_ip)
    }

    /// D4a-7 (2026-08-31): the ONE mutator for `net_continue_blocked` — see
    /// that field's own doc comment for the two call sites this backs
    /// (`steps::network::handle_blocked_continue` sets it `true`,
    /// `steps::network::on_step_entered` clears it back to `false` on a
    /// fresh visit).
    pub fn set_net_continue_blocked(&mut self, on: bool) {
        self.net_continue_blocked = on;
    }

    /// Pure render-time gate for `steps::network::continue_blocked_notice` —
    /// split out from that fn so the decision itself (not just the two
    /// message strings it picks between) is unit-testable without a `gpui::
    /// Context`. `network_connected` is threaded in rather than read from
    /// `OobeFlow` here (this type has no `OobeFlow` dependency, same
    /// boundary `network_ui.rs`'s enums already keep) — the caller already
    /// has `flow.selections().network_connected` in hand.
    ///
    /// A `true` `net_continue_blocked` flag from an EARLIER blocked click
    /// stops being shown the instant either escape route resolves (a Wi-Fi
    /// join completes, or a wired/portal connection comes up) — the flag
    /// itself is only cleared explicitly on a fresh step visit (see `set_
    /// net_continue_blocked`'s own doc comment), but there is no reason to
    /// keep showing a now-stale "you're not connected" notice for the one
    /// render frame between the connection resolving and the operator's
    /// next click actually advancing past the step.
    pub fn should_show_net_continue_blocked_notice(&self, network_connected: bool) -> bool {
        self.net_continue_blocked && !network_connected && !self.wired_online()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    // ── WP-C (2026-09-05): provider key field + login disclosure gate ────

    #[test]
    fn the_key_field_starts_collapsed_and_toggles_per_provider() {
        let mut ui = OobeUiState::default();
        assert_eq!(ui.runtime_key_provider, None);

        ui.toggle_runtime_key_field("anthropic");
        assert_eq!(ui.runtime_key_provider, Some("anthropic"));

        // A second row takes the one shared field away from the first —
        // there is only one `OobeTextField` entity on this step.
        ui.toggle_runtime_key_field("openai");
        assert_eq!(ui.runtime_key_provider, Some("openai"));

        // Clicking the SAME row again collapses it.
        ui.toggle_runtime_key_field("openai");
        assert_eq!(ui.runtime_key_provider, None);
    }

    #[test]
    fn moving_the_key_field_clears_a_stale_status_line() {
        // A "這不像 API 金鑰" left over from one provider's attempt must not
        // read as the next provider's own status.
        let mut ui = OobeUiState::default();
        ui.toggle_runtime_key_field("anthropic");
        ui.set_runtime_claim(RuntimeClaimState::Failed(RuntimeClaimFailureKind::LooksWrong));
        ui.toggle_runtime_key_field("groq");
        assert_eq!(ui.runtime_claim, RuntimeClaimState::Idle);

        ui.set_runtime_claim(RuntimeClaimState::Saved);
        ui.close_runtime_key_field();
        assert_eq!(ui.runtime_claim, RuntimeClaimState::Idle);
        assert_eq!(ui.runtime_key_provider, None);
    }

    /// The load-bearing one for TODO decision 1B: no acknowledgement, no
    /// login. `start_runtime_login` is the single gate both the button's
    /// disabled state and the click handler consult.
    #[test]
    fn disclosure_gates_the_login_start() {
        let mut ui = OobeUiState::default();
        // Nothing on screen: there is nothing to start.
        assert!(!ui.runtime_login.start_enabled());
        assert_eq!(ui.start_runtime_login(), None);

        ui.open_runtime_login_disclosure("anthropic");
        assert_eq!(ui.runtime_login, RuntimeLoginState::Disclosure { provider: "anthropic", acknowledged: false });
        assert!(!ui.runtime_login.start_enabled(), "an unacknowledged disclosure must not be startable");
        assert_eq!(ui.start_runtime_login(), None, "a click while unacknowledged must change nothing at all");
        assert_eq!(ui.runtime_login, RuntimeLoginState::Disclosure { provider: "anthropic", acknowledged: false });

        ui.toggle_runtime_login_acknowledged();
        assert!(ui.runtime_login.start_enabled());
        assert_eq!(ui.start_runtime_login(), Some("anthropic"));
        assert_eq!(ui.runtime_login, RuntimeLoginState::Starting { provider: "anthropic" });
    }

    #[test]
    fn un_ticking_the_acknowledgement_closes_the_gate_again() {
        let mut ui = OobeUiState::default();
        ui.open_runtime_login_disclosure("xai");
        ui.toggle_runtime_login_acknowledged();
        assert!(ui.runtime_login.start_enabled());
        ui.toggle_runtime_login_acknowledged();
        assert!(!ui.runtime_login.start_enabled(), "the gate is re-derived every time, never a one-way latch");
        assert_eq!(ui.start_runtime_login(), None);
    }

    #[test]
    fn a_second_start_while_one_is_already_running_is_refused() {
        // One PTY login owns the machine at a time — a double click, or a
        // second row's button, must not spawn another.
        let mut ui = OobeUiState::default();
        ui.open_runtime_login_disclosure("gemini");
        ui.toggle_runtime_login_acknowledged();
        assert_eq!(ui.start_runtime_login(), Some("gemini"));
        assert_eq!(ui.start_runtime_login(), None);
        ui.set_runtime_login_session("sess-1".to_string());
        assert_eq!(ui.start_runtime_login(), None);
        assert!(ui.runtime_login.is_busy());
    }

    #[test]
    fn the_acknowledgement_toggle_is_inert_outside_a_disclosure() {
        let mut ui = OobeUiState::default();
        ui.toggle_runtime_login_acknowledged();
        assert_eq!(ui.runtime_login, RuntimeLoginState::Idle);

        ui.open_runtime_login_disclosure("openai");
        ui.toggle_runtime_login_acknowledged();
        ui.start_runtime_login();
        ui.toggle_runtime_login_acknowledged();
        assert_eq!(ui.runtime_login, RuntimeLoginState::Starting { provider: "openai" }, "ticking the box mid-flight must not rewind the flow");
    }

    #[test]
    fn opening_a_disclosure_collapses_an_expanded_key_field() {
        // The two panels are alternatives; showing both would make the card
        // taller than the step.
        let mut ui = OobeUiState::default();
        ui.toggle_runtime_key_field("anthropic");
        ui.open_runtime_login_disclosure("anthropic");
        assert_eq!(ui.runtime_key_provider, None);
    }

    #[test]
    fn the_session_id_and_prompt_only_land_on_a_live_login() {
        let mut ui = OobeUiState::default();
        // No login in flight — a late update from an abandoned worker must
        // not resurrect a panel the operator has closed.
        ui.set_runtime_login_session("sess-late".to_string());
        assert_eq!(ui.runtime_login, RuntimeLoginState::Idle);
        ui.set_runtime_login_prompt(Some("https://example.com/auth".to_string()), None);
        assert_eq!(ui.runtime_login, RuntimeLoginState::Idle);
        ui.set_runtime_login_succeeded();
        assert_eq!(ui.runtime_login, RuntimeLoginState::Idle);
        ui.set_runtime_login_failed(RuntimeLoginFailureKind::Unreachable);
        assert_eq!(ui.runtime_login, RuntimeLoginState::Idle);

        ui.open_runtime_login_disclosure("kimi");
        ui.toggle_runtime_login_acknowledged();
        ui.start_runtime_login();
        ui.set_runtime_login_session("sess-1".to_string());
        assert_eq!(ui.runtime_login.session_id(), Some("sess-1"));
        ui.set_runtime_login_prompt(Some("https://x.ai/device?user_code=ABCD-1234".to_string()), Some("ABCD-1234".to_string()));
        assert_eq!(
            ui.runtime_login,
            RuntimeLoginState::Running {
                provider: "kimi",
                session_id: Some("sess-1".to_string()),
                url: Some("https://x.ai/device?user_code=ABCD-1234".to_string()),
                code: Some("ABCD-1234".to_string()),
            }
        );
    }

    #[test]
    fn a_settled_login_stops_being_busy_and_keeps_naming_its_provider() {
        let mut ui = OobeUiState::default();
        ui.open_runtime_login_disclosure("gemini");
        ui.toggle_runtime_login_acknowledged();
        ui.start_runtime_login();
        ui.set_runtime_login_session("s".to_string());
        ui.set_runtime_login_succeeded();
        assert_eq!(ui.runtime_login, RuntimeLoginState::Succeeded { provider: "gemini" });
        assert!(!ui.runtime_login.is_busy());
        assert_eq!(ui.runtime_login.session_id(), None, "a settled login has no session left to cancel");
        assert_eq!(ui.runtime_login.provider(), Some("gemini"));

        ui.close_runtime_login();
        assert_eq!(ui.runtime_login, RuntimeLoginState::Idle);
        assert_eq!(ui.runtime_login.provider(), None);
    }

    #[test]
    fn every_failure_kind_is_reachable_and_names_its_provider() {
        for kind in [
            RuntimeLoginFailureKind::Unavailable,
            RuntimeLoginFailureKind::Unreachable,
            RuntimeLoginFailureKind::Refused,
            RuntimeLoginFailureKind::Abandoned,
        ] {
            let mut ui = OobeUiState::default();
            ui.open_runtime_login_disclosure("cursor");
            ui.set_runtime_login_failed(kind);
            assert_eq!(ui.runtime_login, RuntimeLoginState::Failed { provider: "cursor", kind });
            assert!(!ui.runtime_login.is_busy());
        }
    }

    // ── Shell-S3: NetScanState / NetConnectState / OobeUiState transitions ──

    #[test]
    fn oobe_ui_state_default_has_never_scanned_and_idle_connect() {
        let ui = OobeUiState::default();
        assert_eq!(ui.net_scan, NetScanState::NeverScanned);
        assert_eq!(ui.net_connect, NetConnectState::Idle);
        assert_eq!(ui.net_backend_kind, None);
        assert_eq!(ui.net_selected_ssid, None);
    }

    #[test]
    fn set_net_scanning_transitions_from_never_scanned() {
        let mut ui = OobeUiState::default();
        ui.set_net_scanning();
        assert_eq!(ui.net_scan, NetScanState::Scanning);
    }

    #[test]
    fn set_net_scan_loaded_records_both_the_ap_list_and_the_backend_kind_together() {
        let mut ui = OobeUiState::default();
        ui.set_net_scanning();
        let aps = vec![network::AccessPoint { ssid: "DuDu-Office".to_string(), signal_bars: 4, security: "psk".to_string(), known: false }];
        ui.set_net_scan_loaded(aps.clone(), network::NetBackendKind::Real);
        assert_eq!(ui.net_scan, NetScanState::Loaded(aps));
        assert_eq!(ui.net_backend_kind, Some(network::NetBackendKind::Real));
    }

    #[test]
    fn set_net_scan_failed_still_records_the_backend_kind() {
        // A scan can fail on a REAL backend (e.g. no Wi-Fi adapter) — the
        // demo-mode notice must not be shown just because a scan failed;
        // `net_backend_kind` still has to reflect which backend actually
        // ran.
        let mut ui = OobeUiState::default();
        ui.set_net_scan_failed(network::NetBackendKind::Real);
        assert_eq!(ui.net_scan, NetScanState::Failed);
        assert_eq!(ui.net_backend_kind, Some(network::NetBackendKind::Real));
    }

    #[test]
    fn start_net_awaiting_psk_selects_the_ssid_marks_it_secured_and_moves_off_idle() {
        let mut ui = OobeUiState::default();
        ui.start_net_awaiting_psk("DuDu-Office");
        assert_eq!(ui.net_selected_ssid.as_deref(), Some("DuDu-Office"));
        assert!(ui.net_selected_secured);
        assert_eq!(ui.net_connect, NetConnectState::AwaitingPsk);
    }

    #[test]
    fn set_net_connecting_then_failed_then_reset_round_trips() {
        let mut ui = OobeUiState::default();
        ui.set_net_connecting("DuDu-Guest", false);
        assert_eq!(ui.net_connect, NetConnectState::Connecting);
        assert_eq!(ui.net_selected_ssid.as_deref(), Some("DuDu-Guest"));
        assert!(!ui.net_selected_secured);

        ui.set_net_connect_failed(NetConnectFailureKind::WrongPassword);
        assert_eq!(ui.net_connect, NetConnectState::Failed(NetConnectFailureKind::WrongPassword));

        ui.reset_net_connect();
        assert_eq!(ui.net_connect, NetConnectState::Idle);
        // Deliberately still selected — see `reset_net_connect`'s own doc
        // comment for why a settled success leaves the row highlighted.
        assert_eq!(ui.net_selected_ssid.as_deref(), Some("DuDu-Guest"));
    }

    #[test]
    fn clear_net_selected_ssid_actually_clears_it() {
        let mut ui = OobeUiState::default();
        ui.start_net_awaiting_psk("DuDu-Office");
        ui.clear_net_selected_ssid();
        assert_eq!(ui.net_selected_ssid, None);
    }

    // ── D4a-5: net_status / wired_online() ──────────────────────────────

    #[test]
    fn wired_online_is_false_with_no_status_fetched_yet() {
        let ui = OobeUiState::default();
        assert_eq!(ui.net_status, None);
        assert!(!ui.wired_online());
    }

    #[test]
    fn wired_online_is_true_only_when_internet_counts_as_connected_and_has_an_ip() {
        // `..Default::default()` on the FIRST construction, then plain field
        // reassignment for the rest — same shape avoids clippy's
        // `field_reassign_with_default` on the first line without losing
        // this test's own point (reusing one `ui` across four snapshots).
        let mut ui = OobeUiState {
            net_status: Some(network::NetworkStatus { internet: network::InternetState::Online, has_ip: true, wifi_ssid: None, portal_url: None }),
            ..Default::default()
        };
        assert!(ui.wired_online());

        ui.net_status = Some(network::NetworkStatus { internet: network::InternetState::Portal, has_ip: true, wifi_ssid: None, portal_url: Some("http://x/".to_string()) });
        assert!(ui.wired_online(), "portal counts as connected — D4a §5.4-2");

        ui.net_status = Some(network::NetworkStatus { internet: network::InternetState::Offline, has_ip: true, wifi_ssid: None, portal_url: None });
        assert!(!ui.wired_online(), "offline must never count, even with an IP");

        ui.net_status = Some(network::NetworkStatus { internet: network::InternetState::Online, has_ip: false, wifi_ssid: None, portal_url: None });
        assert!(!ui.wired_online(), "online with no IP address is a contradiction, not trusted blindly");
    }

    // ── D4a-7: net_continue_blocked / should_show_net_continue_blocked_notice ──

    #[test]
    fn net_continue_blocked_defaults_to_false() {
        let ui = OobeUiState::default();
        assert!(!ui.net_continue_blocked);
    }

    #[test]
    fn set_net_continue_blocked_round_trips() {
        let mut ui = OobeUiState::default();
        ui.set_net_continue_blocked(true);
        assert!(ui.net_continue_blocked);
        ui.set_net_continue_blocked(false);
        assert!(!ui.net_continue_blocked);
    }

    #[test]
    fn continue_blocked_notice_is_silent_before_any_blocked_click() {
        // The step's very first frame — no click yet — must never show the
        // notice, even though every OTHER condition (`network_connected:
        // false`, no wired signal) already matches. Only an actual blocked
        // click may set `net_continue_blocked`.
        let ui = OobeUiState::default();
        assert!(!ui.should_show_net_continue_blocked_notice(false));
    }

    #[test]
    fn continue_blocked_notice_shows_once_blocked_with_no_escape_route_yet() {
        let mut ui = OobeUiState::default();
        ui.set_net_continue_blocked(true);
        assert!(ui.should_show_net_continue_blocked_notice(false));
    }

    #[test]
    fn continue_blocked_notice_hides_once_a_wifi_join_is_persisted() {
        let mut ui = OobeUiState::default();
        ui.set_net_continue_blocked(true);
        assert!(
            !ui.should_show_net_continue_blocked_notice(true),
            "a completed Wi-Fi join must silence a stale blocked-click notice, not just the ORIGINAL click's own next attempt"
        );
    }

    #[test]
    fn continue_blocked_notice_hides_once_wired_online_becomes_true() {
        let mut ui = OobeUiState {
            net_continue_blocked: true,
            net_status: Some(network::NetworkStatus { internet: network::InternetState::Online, has_ip: true, wifi_ssid: None, portal_url: None }),
            ..Default::default()
        };
        assert!(!ui.should_show_net_continue_blocked_notice(false));
        // Flipping the wired signal back off (cable unplugged again) must
        // bring the still-`true` flag's notice right back — this fn is a
        // pure re-derivation every render call, never a one-shot latch.
        ui.net_status = None;
        assert!(ui.should_show_net_continue_blocked_notice(false));
    }
}
