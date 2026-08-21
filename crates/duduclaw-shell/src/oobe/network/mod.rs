// Wi-Fi backend abstraction — Shell-S3 (2026-08-21, "OOBE Wi-Fi 從假轉真").
//
// The `Network` OOBE step (`oobe/steps/network.rs`) used to read a static
// `fake_data::FAKE_WIFI_NETWORKS` list directly and mark any clicked row
// "connected" unconditionally (S1's honest stub — see `fake_data.rs`'s own
// header comment: "Wi-Fi 永遠連線成功"). This module replaces that direct
// read with a real trait, `NetworkBackend`, so `steps::network` never
// touches `fake_data`/`NmNetworkBackend` directly again — it only ever
// calls `scan()`/`connect()`/`status()`/`forget()` on whatever
// `select_backend()` handed it.
//
// Two implementations:
//   - `FakeNetworkBackend` (`fake.rs`) — every platform, always compiled.
//     Still backed by `oobe::fake_data::FAKE_WIFI_NETWORKS` (the same
//     canonical example list Shell-S1 shipped), reached through the trait
//     instead of read ad-hoc.
//   - `NmNetworkBackend` (`nm.rs`) — `#[cfg(target_os = "linux")]` only,
//     talks to NetworkManager over D-Bus (`org.freedesktop.NetworkManager`).
//     Never compiled on macOS (this crate's dev-loop host — see this
//     crate's `Cargo.toml` header comment on why Linux-only code paths in
//     this crate are `cfg`-gated rather than feature-gated) or in tests.
//
// `select_backend()` is the ONE place that decides which of the two a given
// process run gets, and is itself pure Rust (no gpui types — same "no gpui
// anywhere in the state/backend layer" discipline `oobe/mod.rs` and
// `oobe/claim.rs` already hold themselves to). It runs REAL I/O (the
// `Nm` probe dials the system bus), so — same as `oobe::claim::
// create_account` — it is only ever called from a background
// `std::thread::spawn`, never from a render pass; see `steps::network`'s
// own header comment for the click/auto-trigger + `std::sync::mpsc` +
// `cx.spawn` poll bridge this crate already established for
// `steps::account`'s gateway RPC.

mod fake;
#[cfg(target_os = "linux")]
mod nm;

pub(crate) use fake::FakeNetworkBackend;

/// One Wi-Fi access point as reported by `NetworkBackend::scan`. Owned
/// strings throughout (unlike `fake_data::FakeWifiNetwork`'s `&'static
/// str`) — a real scan result is short-lived data crossing a background
/// thread -> `std::sync::mpsc` boundary, not a `const` table.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AccessPoint {
    pub(crate) ssid: String,
    /// 1..=4, same decorative bar-count convention `fake_data::
    /// FakeWifiNetwork::signal_bars` already established — `nm.rs` derives
    /// this from NetworkManager's 0..=100 `Strength` percentage
    /// (`strength_to_bars`), `fake.rs` copies it straight from the fake
    /// table.
    pub(crate) signal_bars: u8,
    pub(crate) secured: bool,
}

/// `NetworkBackend::status`'s result — task brief: "status()（斷線/連線中/
/// 已連線+ssid）".
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConnStatus {
    Disconnected,
    // `Connecting`/`Connected` are only ever CONSTRUCTED by `nm::
    // NmNetworkBackend::status()` (`#[cfg(target_os = "linux")]`) —
    // `FakeNetworkBackend::status()` always reports `Disconnected` by
    // design (see that method's own doc comment), and
    // `steps::network::kick_off_scan` only ever MATCHES `Connected`
    // (matching isn't constructing). On a non-Linux build there is
    // therefore genuinely no construction site for either variant in
    // THIS compilation — not dead code on the platform that matters
    // (Linux), just unreachable-by-construction on the Mac dev-loop host.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Connecting { ssid: String },
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Connected { ssid: String },
}

/// Every failure mode either backend can report. Deliberately NOT split
/// into "wrong password" vs. "AP unreachable" variants here — `nm.rs` has
/// no reliable way to distinguish those from NetworkManager's D-Bus surface
/// without subscribing to `Device.StateChanged`'s reason code mid-poll (see
/// that module's own header comment), so the classification instead happens
/// once, in `steps::network`'s UI layer, from information ONLY the UI has
/// anyway (whether the join attempt carried a PSK at all) — the exact same
/// "collapse N technical causes into the operator-facing kinds the retry
/// action actually differs on" call `oobe::claim::ClaimError` ->
/// `AccountClaimFailureKind` already makes for the gateway claim flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NetError {
    // `ScanFailed`/`Timeout`/`Unavailable` are only ever constructed by
    // `nm.rs` (`#[cfg(target_os = "linux")]`) — `FakeNetworkBackend` never
    // fails a scan and only ever returns `ConnectFailed`/`NotFound` (see
    // that module's own doc comment). Same cross-platform-construction
    // shape `ConnStatus`'s own header comment documents above; not dead
    // code on Linux, unreachable-by-construction on the Mac dev-loop host.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    ScanFailed(String),
    ConnectFailed(String),
    /// `connect()` never reached a terminal state (activated/failed)
    /// within its poll budget — see `nm::poll_until_settled`'s own doc
    /// comment.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Timeout,
    /// The backend itself couldn't be reached/used at all — D-Bus down, no
    /// Wi-Fi adapter present, NetworkManager service not registered.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Unavailable(String),
    /// `forget()` (or, in principle, `connect()`) was asked about an SSID
    /// this backend has no record of.
    NotFound,
}

/// The Wi-Fi control surface `steps::network` drives — see this module's
/// header comment for the two implementations and why the trait exists.
/// `Send` (not `Send + Sync`): every call happens from inside exactly one
/// `std::thread::spawn` closure at a time (see `steps::network`'s own
/// background-thread pattern), so the trait object only ever needs to move
/// across ONE thread boundary, never be shared across several
/// concurrently.
pub(crate) trait NetworkBackend: Send {
    fn scan(&self) -> Result<Vec<AccessPoint>, NetError>;
    /// `psk`: `None` for an open network, `Some(passphrase)` for a secured
    /// one — `steps::network`'s PSK step never calls this with `Some("")`
    /// (empty-string psk), see that module's own client-side length gate.
    fn connect(&self, ssid: &str, psk: Option<&str>) -> Result<(), NetError>;
    fn status(&self) -> ConnStatus;
    /// Deliberate honest stub as of this round: implemented and unit-tested
    /// on both backends (fulfilling the trait's own completeness
    /// requirement), but `steps::network`'s OOBE flow has no UI entry point
    /// that calls it — joining a network is the whole point of THIS step,
    /// managing previously-saved profiles is out of scope for a first-run
    /// wizard. `#[allow(dead_code)]`: unlike `ConnStatus`/`NetError`'s
    /// variants above, this is dead on EVERY platform in production, not
    /// just non-Linux, so it isn't `cfg_attr`-scoped.
    #[allow(dead_code)]
    fn forget(&self, ssid: &str) -> Result<(), NetError>;
}

/// Which concrete backend a given process run actually got — surfaced to
/// the operator (task brief: "Linux 偵測不到 NM...在 UI 誠實標示（不可假裝
/// 連線成功）") rather than kept as an internal implementation detail. Two
/// states are enough: `steps::network` renders one demo-mode notice for
/// EITHER "not Linux" (this crate's own Mac dev loop — `BUILD-LINUX.md`
/// calls Linux "the ship target", Mac only "the dev-loop host") or "Linux,
/// but NetworkManager's D-Bus was unreachable" — both cases mean the Wi-Fi
/// list on screen is `fake_data`, and an operator staring at DuDuClaw OS
/// running for real has no need to know WHICH of those two sub-reasons
/// applies, only that it isn't real.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NetBackendKind {
    // Only constructed inside `select_backend`'s own `#[cfg(target_os =
    // "linux")]` block below — same cross-platform-construction shape
    // `ConnStatus`/`NetError` document above.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Real,
    Fake,
}

/// Resolves + constructs the backend for one scan/connect/status/forget
/// call — see this module's header comment for why callers only ever
/// invoke this from a background thread. Priority (highest first):
///   1. `DUDUCLAW_SHELL_FAKE_NET=1` — explicit dev/test override, any
///      platform. Same shape as `DUDUCLAW_SHELL_OOBE_LOCAL_ACCOUNT`
///      (`oobe/steps/account.rs`): an escape hatch for headless smoke runs
///      with no real backend reachable, not something a shipped kiosk
///      image would ever set.
///   2. `#[cfg(target_os = "linux")]` — try `NmNetworkBackend::probe()`.
///      On success this run gets `Real`. On failure (D-Bus down,
///      NetworkManager's service not registered — NOT "no Wi-Fi adapter",
///      see `NmNetworkBackend::probe`'s own doc comment for why that case
///      is intentionally excluded here) this is the fail-open path the
///      task brief asks for: fall through to Fake rather than leaving the
///      whole step unusable.
///   3. Otherwise (non-Linux, or the Linux probe failed) — `FakeNetworkBackend`.
pub(crate) fn select_backend() -> (Box<dyn NetworkBackend>, NetBackendKind) {
    if std::env::var("DUDUCLAW_SHELL_FAKE_NET").ok().as_deref() == Some("1") {
        return (Box::new(FakeNetworkBackend::new()), NetBackendKind::Fake);
    }

    #[cfg(target_os = "linux")]
    {
        match nm::NmNetworkBackend::probe() {
            Ok(backend) => return (Box::new(backend), NetBackendKind::Real),
            Err(e) => {
                eprintln!("[oobe/network] NetworkManager unreachable, falling back to demo Wi-Fi data: {e:?}");
            }
        }
    }

    (Box::new(FakeNetworkBackend::new()), NetBackendKind::Fake)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // `std::env::set_var` is process-global and `unsafe` on this toolchain
    // — same discipline `oobe/claim.rs`'s own `ENV_LOCK`-guarded tests
    // already establish (serialize via a local mutex, always restore the
    // prior value on the way out) rather than racing whatever else `cargo
    // test` is running in parallel in this same binary.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn fake_net_env_override_forces_fake_kind_regardless_of_platform() {
        let _g = ENV_LOCK.lock().unwrap();
        let prev = std::env::var("DUDUCLAW_SHELL_FAKE_NET").ok();
        unsafe { std::env::set_var("DUDUCLAW_SHELL_FAKE_NET", "1") };
        let (_, kind) = select_backend();
        unsafe {
            match &prev {
                Some(v) => std::env::set_var("DUDUCLAW_SHELL_FAKE_NET", v),
                None => std::env::remove_var("DUDUCLAW_SHELL_FAKE_NET"),
            }
        }
        assert_eq!(kind, NetBackendKind::Fake);
    }

    #[test]
    #[cfg(not(target_os = "linux"))]
    fn non_linux_default_is_fake_without_needing_the_env_override() {
        // On a non-Linux dev host (this crate's own Mac build) there is no
        // `nm` module compiled at all, so `select_backend` must fall
        // straight to Fake unconditionally as long as the env override
        // isn't set.
        let _g = ENV_LOCK.lock().unwrap();
        let prev = std::env::var("DUDUCLAW_SHELL_FAKE_NET").ok();
        unsafe { std::env::remove_var("DUDUCLAW_SHELL_FAKE_NET") };
        let (_, kind) = select_backend();
        unsafe {
            if let Some(v) = &prev {
                std::env::set_var("DUDUCLAW_SHELL_FAKE_NET", v);
            }
        }
        assert_eq!(kind, NetBackendKind::Fake);
    }
}
