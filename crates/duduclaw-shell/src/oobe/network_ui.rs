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

/// Which message `steps::network` shows for a `Failed` connect attempt —
/// collapses `network::NetError`'s backend-layer variants down to the three
/// an operator actually needs to act on differently, same split
/// `AccountClaimFailureKind` (`oobe/mod.rs`) already establishes for the
/// claim flow. `PasswordTooShort` is a pure client-side pre-check (mirrors
/// the real WPA-PSK 8–63 character rule, same "catch it before ever
/// dispatching a request" shape `oobe::claim`'s own password-length gate
/// uses) and never reaches a backend at all. Which of `ConnectFailed`/
/// `Timeout`/`Unavailable`/`NotFound` actually happened for the other two
/// is diagnostic detail logged to stderr at the call site
/// (`steps::network`'s own apply-result function), never rendered — see
/// `network::NetError`'s own doc comment for why THIS classification (not
/// the backend) is the one place that decides wrong-password vs.
/// unreachable, using information only the UI layer has (whether the
/// attempt carried a PSK at all).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetConnectFailureKind {
    PasswordTooShort,
    WrongPassword,
    Unreachable,
}
