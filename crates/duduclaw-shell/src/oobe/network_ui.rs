// `Network` step's ephemeral UI-state enums — Shell-S3 (2026-08-21).
//
// Split out of `oobe/mod.rs` (not because these types are conceptually
// separate from `OobeUiState` — they're exactly as tightly coupled to it as
// `AccountClaimState`/`AccountClaimFailureKind`, which stay inline there)
// but because `oobe/mod.rs` was ALREADY well over this crate's own <800-line
// file-size convention before this round (1,854 lines at the start of
// Shell-S3 work) and adding these three enums inline would have pushed it
// further past that cap for no structural reason — the state MACHINE
// (`OobeFlow`/`OobeStep`/`OobeSelections`/`OobeState`) and the CLAIM-flow UI
// enums it already carries are pre-existing scope this task doesn't own;
// keeping new code out of that file where possible is the least-bad
// available move, not a claim that this file is a clean architectural
// boundary. `OobeUiState` itself, and the `set_net_*`/`start_net_*` methods
// that mutate these types, stay in `oobe/mod.rs` — only the plain enum
// DEFINITIONS moved.
//
// Re-exported from `oobe/mod.rs` (`pub use network_ui::*` — see that file),
// so every existing call site (`oobe::NetScanState`, `crate::oobe::{
// NetConnectState, NetConnectFailureKind, ...}` in `steps::network`) keeps
// working unchanged; this file's own existence is an implementation detail
// of `oobe`, not a new public path anyone needs to know about.

use super::network;

/// The `Network` step's real scan progress (Shell-S3, 2026-08-21) — driven
/// by `steps::network`'s scan/rescan click handler + `oobe::network::
/// select_backend().scan()` (see that module's own header comment for the
/// backend layer this wraps). Ephemeral, like `AccountClaimState`
/// (`oobe/mod.rs`) — a fresh process launch always starts `NeverScanned`,
/// never resumes a stale in-flight scan.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum NetScanState {
    #[default]
    NeverScanned,
    Scanning,
    /// `Vec<network::AccessPoint>` rather than `Copy` data is exactly why
    /// `OobeUiState` (`oobe/mod.rs`) can no longer derive `Copy` as of this
    /// round — see that struct's own doc comment.
    Loaded(Vec<network::AccessPoint>),
    /// Collapses `network::NetError`'s scan-layer variants down to one
    /// retryable state, same "operator doesn't need five different scan
    /// failure messages" call `AccountClaimFailureKind`'s own doc comment
    /// already makes for the claim flow.
    Failed,
}

/// Which access point `steps::network`'s connect flow is currently working
/// on — set the moment a row is clicked (`AwaitingPsk` for a secured
/// network, straight to `Connecting` for an open one), NOT part of
/// `OobeState`/persistence: only the PERSISTED `OobeSelections::
/// network_ssid`/`network_connected` pair (`oobe/mod.rs`) is the durable
/// "what are we actually joined to" record (see that field's own doc
/// comment) — this is just the ephemeral progress of the CURRENT attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NetConnectState {
    #[default]
    Idle,
    /// A secured network was picked; the PSK field is showing, no connect
    /// attempt has been dispatched yet.
    AwaitingPsk,
    Connecting,
    Failed(NetConnectFailureKind),
}

impl NetConnectState {
    /// Whether `steps::network`'s own "連線" submit is meaningfully
    /// re-triggerable right now — a row has been picked (`AwaitingPsk`) or a
    /// previous attempt on it just failed and can be retried (`Failed(_)`),
    /// and nothing is already in flight. The one caller is `OobeFlow::
    /// enter_outcome` (`state.rs`), which takes this as a plain `bool`
    /// rather than depending on this type directly — see that method's own
    /// doc comment for why.
    pub(crate) fn submittable(&self) -> bool {
        matches!(self, NetConnectState::AwaitingPsk | NetConnectState::Failed(_))
    }
}

/// Which message `steps::network` shows for a `Failed` connect attempt.
///
/// Two sources feed this enum, side by side:
///   - `PasswordTooShort` is a pure client-side pre-check (mirrors the real
///     WPA-PSK 8–63 character rule, same "catch it before ever dispatching
///     a request" shape `oobe::claim`'s own password-length gate uses) and
///     never reaches a backend at all.
///   - D4a-5 (2026-08-23): `WrongPassword` through `UnsupportedSecurity`
///     mirror `network::WifiFailureCode`'s own nine D4a §5.3 codes 1:1 (see
///     `from_code` below) — the gateway backend classifies failures
///     server-side now, so this enum carries the SPECIFIC classification
///     through instead of re-guessing it in the UI layer.
///   - `Unreachable` is the fallback bucket for every LEGACY backend
///     failure (`fake.rs`/`nm.rs`, neither of which classifies anything)
///     AND for `WifiFailureCode::Unknown` (a code this build doesn't
///     recognize) — same split `AccountClaimFailureKind` (`oobe/mod.rs`)
///     already establishes for the claim flow: collapse what the operator
///     can't act on differently into one generic "just retry" message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetConnectFailureKind {
    PasswordTooShort,
    WrongPassword,
    NotFound,
    OutOfRange,
    NoAdapter,
    DriverMissing,
    NoIp,
    Portal,
    BackendUnavailable,
    UnsupportedSecurity,
    Unreachable,
}

impl NetConnectFailureKind {
    /// D4a §5.3: maps the gateway's own nine-code classification onto this
    /// UI-facing enum, 1:1. `WifiFailureCode::Unknown` (a code this build
    /// doesn't recognize) collapses to `Unreachable` — the same generic
    /// "just retry" bucket every non-gateway backend's unclassified failure
    /// already lands on (see `network::WifiFailureCode`'s own doc comment
    /// for the fail-closed discipline behind `Unknown` in the first place:
    /// never silently mapped onto an EXISTING code, which could understate
    /// what actually went wrong).
    pub(crate) fn from_code(code: network::WifiFailureCode) -> Self {
        use network::WifiFailureCode;
        match code {
            WifiFailureCode::WrongPassword => Self::WrongPassword,
            WifiFailureCode::NotFound => Self::NotFound,
            WifiFailureCode::OutOfRange => Self::OutOfRange,
            WifiFailureCode::NoAdapter => Self::NoAdapter,
            WifiFailureCode::DriverMissing => Self::DriverMissing,
            WifiFailureCode::NoIp => Self::NoIp,
            WifiFailureCode::Portal => Self::Portal,
            WifiFailureCode::BackendUnavailable => Self::BackendUnavailable,
            WifiFailureCode::UnsupportedSecurity => Self::UnsupportedSecurity,
            WifiFailureCode::Unknown => Self::Unreachable,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_code_maps_every_known_code_to_its_own_distinct_variant() {
        use network::WifiFailureCode;
        let cases: &[(WifiFailureCode, NetConnectFailureKind)] = &[
            (WifiFailureCode::WrongPassword, NetConnectFailureKind::WrongPassword),
            (WifiFailureCode::NotFound, NetConnectFailureKind::NotFound),
            (WifiFailureCode::OutOfRange, NetConnectFailureKind::OutOfRange),
            (WifiFailureCode::NoAdapter, NetConnectFailureKind::NoAdapter),
            (WifiFailureCode::DriverMissing, NetConnectFailureKind::DriverMissing),
            (WifiFailureCode::NoIp, NetConnectFailureKind::NoIp),
            (WifiFailureCode::Portal, NetConnectFailureKind::Portal),
            (WifiFailureCode::BackendUnavailable, NetConnectFailureKind::BackendUnavailable),
            (WifiFailureCode::UnsupportedSecurity, NetConnectFailureKind::UnsupportedSecurity),
        ];
        for (code, expected) in cases {
            assert_eq!(NetConnectFailureKind::from_code(*code), *expected, "{code:?}");
        }
    }

    #[test]
    fn from_code_collapses_unknown_to_unreachable() {
        assert_eq!(NetConnectFailureKind::from_code(network::WifiFailureCode::Unknown), NetConnectFailureKind::Unreachable);
    }

    #[test]
    fn submittable_is_true_only_for_awaiting_psk_and_failed() {
        assert!(!NetConnectState::Idle.submittable());
        assert!(NetConnectState::AwaitingPsk.submittable());
        assert!(!NetConnectState::Connecting.submittable(), "already in flight — must not double-submit");
        assert!(NetConnectState::Failed(NetConnectFailureKind::WrongPassword).submittable(), "a failure must stay retryable");
    }
}
