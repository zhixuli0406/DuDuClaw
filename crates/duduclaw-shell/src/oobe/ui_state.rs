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
        self.net_status.as_ref().is_some_and(|s| s.internet.counts_as_connected() && s.has_ip)
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
}
