// Wi-Fi backend abstraction — Shell-S3 (2026-08-21), rewired to a real
// gateway backend by D4a-5 (2026-08-23).
//
// The `Network` OOBE step (`oobe/steps/network.rs`) used to read a static
// `fake_data::FAKE_WIFI_NETWORKS` list directly and mark any clicked row
// "connected" unconditionally (S1's honest stub — see `fake_data.rs`'s own
// header comment: "Wi-Fi 永遠連線成功"). This module replaces that direct
// read with a real trait, `NetworkBackend`, so `steps::network` never
// touches `fake_data`/backend internals directly again — it only ever calls
// `scan()`/`connect()`/`status()`/`forget()`/`network_status()` on whatever
// `select_backend()` handed it.
//
// ── D4a-5 (2026-08-23): the ship-blocker fix ──────────────────────────────
// `commercial/docs/DESIGN-network-settings-2026-08.md` §1.2/§5.4 traced a
// real bug: the appliance image never ships NetworkManager (it ships iwd,
// driven only by the gateway over D-Bus — see that doc's §2/§3), so
// `NmNetworkBackend::probe()` (below, `nm.rs`) ALWAYS failed on a real
// machine and `select_backend()` silently fell through to
// `FakeNetworkBackend` — meaning a real appliance showed four fabricated
// SSIDs ("DuDu-Office" etc.), and typing any 8-char password "connected".
// The fix has two parts:
//   1. A new backend, `gateway.rs` — the shell never touches iwd/D-Bus
//      itself (§3.3: the kiosk shell stays out of the `netdev` group on
//      purpose, same reasoning `oobe::claim` already established for
//      account creation), it drives the gateway's own pre-auth
//      `/api/first-run/network/*` HTTP endpoints instead, hand-rolled the
//      same way `oobe::claim` talks to `/api/first-run/claim` (see
//      `gateway.rs`'s own header comment for why the HTTP client is
//      duplicated rather than shared).
//   2. `select_backend()` no longer treats "appliance" as "try Linux D-Bus,
//      fall back to Fake on any failure" — on an appliance it is ALWAYS the
//      gateway backend, full stop; an unreachable gateway is surfaced as a
//      new `NetBackendKind::Unavailable` state (honest failure), never a
//      silent downgrade to demo data.
//
// Three implementations of `NetworkBackend`:
//   - `FakeNetworkBackend` (`fake.rs`) — every platform, always compiled.
//     Still backed by `oobe::fake_data::FAKE_WIFI_NETWORKS` (the same
//     canonical example list Shell-S1 shipped), reached through the trait
//     instead of read ad-hoc. Only ever selected off an appliance (Mac
//     dev-loop, or the explicit `DUDUCLAW_SHELL_FAKE_NET=1` override) as of
//     this round — see `select_backend`'s own doc comment.
//   - `GatewayNetworkBackend` (`gateway.rs`) — portable (no `cfg` gate at
//     all: it is a plain TCP/HTTP client, same as `oobe::claim`), talks to
//     the LOCAL gateway process's pre-auth network endpoints. The backend
//     an appliance always gets.
//   - `NmNetworkBackend` (`nm.rs`) — `#[cfg(target_os = "linux")]`,
//     `#[allow(dead_code)]` as of D4a-5 (see that file's own header comment
//     for why it is KEPT, not deleted, despite no longer being reachable
//     from `select_backend`). Talks to NetworkManager over D-Bus — correct
//     code, wrong backend for THIS image.
//
// `select_backend()` is the ONE place that decides which backend a given
// process run gets, and is itself pure Rust (no gpui types — same "no gpui
// anywhere in the state/backend layer" discipline `oobe/mod.rs` and
// `oobe/claim.rs` already hold themselves to). It runs REAL I/O (probing the
// gateway, or — historically — dialing the system bus), so — same as
// `oobe::claim::create_account` — it is only ever called from a background
// `std::thread::spawn`, never from a render pass; see `steps::network`'s
// own header comment for the click/auto-trigger + `std::sync::mpsc` +
// `cx.spawn` poll bridge this crate already established for
// `steps::account`'s gateway RPC.

mod fake;
mod gateway;
// D4a-5 (2026-08-23): DISABLED but kept compiling — see this file's own
// module-level doc comment above and `nm.rs`'s own header comment for why.
// `#[allow(dead_code)]` here (not `#[cfg(feature = "...")]`) is the
// deliberate choice: this crate has no feature-flag mechanism for backend
// selection today, and — more importantly — the point of KEEPING this file
// is that the Rust compiler keeps checking it against every future change
// to `AccessPoint`/`ConnStatus`/`NetError`/`NetworkBackend` (all defined in
// THIS file, below), so it can never silently bit-rot into code that no
// longer compiles. A `cfg(feature)` gate that nobody ever turns on would
// give up exactly that guarantee.
#[allow(dead_code)]
#[cfg(target_os = "linux")]
mod nm;

pub(crate) use fake::FakeNetworkBackend;
pub(crate) use gateway::GatewayNetworkBackend;

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
    /// (`strength_to_bars`), `gateway.rs` clamps the gateway's own 1..=4
    /// `signal_bars` field into range (defense in depth against a future
    /// gateway build sending something out of spec), `fake.rs` copies it
    /// straight from the fake table.
    pub(crate) signal_bars: u8,
    /// D4a §5.2: `"open"` / `"wep"` / `"psk"` / `"8021x"` / `"unknown"` —
    /// the gateway's own `Network.Type` passthrough. The ONLY stored field;
    /// see `secured()` below for why a second `bool` field is deliberately
    /// NOT also kept (one source of truth, not two that could disagree).
    /// `nm.rs`/`fake.rs` (legacy/demo backends, neither one distinguishes
    /// WEP/802.1X) synthesize `"psk"`/`"open"` from their own bool.
    pub(crate) security: String,
    /// D4a §5.2: has a saved credential on the gateway side — decides
    /// whether `steps::network`'s row click goes straight to
    /// `connect(ssid, None)` (skip the PSK prompt entirely) or shows it.
    /// Always `false` on `fake.rs`/`nm.rs` (neither backend has a concept of
    /// a persisted credential store to query), so those two backends never
    /// skip the PSK prompt — see `fake.rs`'s own doc comment on this field
    /// for why that particular default is load-bearing for the demo flow.
    pub(crate) known: bool,
}

impl AccessPoint {
    /// Derived, NOT a second stored field — `security != "open"`. Every
    /// non-open value (`"psk"`/`"wep"`/`"8021x"`/`"unknown"`) is treated as
    /// secured; an unrecognized future security string is `"unknown"` (see
    /// `gateway::parse_networks`), which is fail-closed here too (an
    /// unrecognized network is treated as needing a password, never
    /// silently offered as open).
    pub(crate) fn secured(&self) -> bool {
        self.security != "open"
    }
}

/// `NetworkBackend::status`'s result — task brief: "status()（斷線/連線中/
/// 已連線+ssid）". Constructed by every real backend as of D4a-5 (previously
/// Linux-only via `nm.rs`) — `gateway.rs` is portable, so these two variants
/// are no longer `cfg`-gated to a single platform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConnStatus {
    Disconnected,
    Connecting { ssid: String },
    Connected { ssid: String },
}

/// D4a §5.2/§5.4-2/§6: the overall connectivity snapshot from `GET
/// /api/first-run/network/status` — richer than `ConnStatus` above, which
/// only describes the Wi-Fi LINK layer. This additionally carries the IP
/// layer and whole-machine internet reachability, which is what `OobeFlow::
/// can_advance_with_wired` (D4a §5.4-2, "有線已連通時網路步應可通過") and
/// the captive-portal notice (D4a §6) both need. Only `GatewayNetworkBackend`
/// produces a real one — see `NetworkBackend::network_status`'s own doc
/// comment for why every other backend's default answer is "unknown", never
/// a fabricated snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct NetworkStatus {
    pub(crate) internet: InternetState,
    /// Non-empty `ip.addresses` on the gateway's wire shape, collapsed to a
    /// bool — `steps::network`/`OobeUiState::wired_online` only ever need
    /// "is there an address at all", never the addresses themselves.
    pub(crate) has_ip: bool,
    /// The currently-associated Wi-Fi SSID, if any (`wifi.ssid`
    /// passthrough) — display only (e.g. the captive-portal notice's "已連
    /// 上 {}"); `can_advance_with_wired` never reads this field.
    pub(crate) wifi_ssid: Option<String>,
    /// Present only when `internet == Portal` — the URL D4a §6's "開啟登入
    /// 頁" affordance would open, if this crate had a reusable "open a URL"
    /// path (see `steps::network`'s own header comment for why that button
    /// is NOT wired this round).
    pub(crate) portal_url: Option<String>,
}

/// D4a §5.2's `internet` field — `"online" | "portal" | "offline" |
/// "unknown"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum InternetState {
    Online,
    Portal,
    Offline,
    /// Default AND the fail-closed landing spot for any string this build
    /// doesn't recognize (coding convention #3's spirit: an unenumerated
    /// value is never silently treated as "fine") — see `parse`'s own doc
    /// comment.
    #[default]
    Unknown,
}

impl InternetState {
    /// Exact string match only — coding convention #2 (no unanchored
    /// `contains`/`starts_with` for a classification decision).
    fn parse(raw: &str) -> Self {
        match raw {
            "online" => Self::Online,
            "portal" => Self::Portal,
            "offline" => Self::Offline,
            _ => Self::Unknown,
        }
    }

    /// D4a §5.4-2: "internet 欄位（online 或 portal 都算「有網路」——portal
    /// 表示實體連通只是要登入）" — a captive portal means the physical link
    /// (wired or wireless) IS up, the machine just hasn't logged in yet, so
    /// it counts as "has network" for the purpose of letting the operator
    /// past this OOBE step (they can finish the portal login from Home just
    /// as easily as from here — blocking OOBE on it would only add a step).
    pub(crate) fn counts_as_connected(self) -> bool {
        matches!(self, Self::Online | Self::Portal)
    }
}

/// Every failure mode any backend can report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NetError {
    /// `nm.rs`-exclusive (a D-Bus `RequestScan` call itself failing) — as of
    /// D4a-5 `nm.rs` is disabled (see this module's own header comment), so
    /// nothing constructs this on ANY platform this round; kept because
    /// `nm.rs` is kept, and `nm.rs`'s own tests still exercise the type it
    /// belongs to.
    #[allow(dead_code)]
    ScanFailed(String),
    ConnectFailed(String),
    /// `nm.rs`-exclusive (`poll_until_settled`'s own budget running out) —
    /// same reasoning as `ScanFailed` above.
    #[allow(dead_code)]
    Timeout,
    /// The backend itself couldn't be reached/used at all — D-Bus down /no
    /// Wi-Fi adapter (legacy `nm.rs` meanings), OR — as of D4a-5 — the
    /// gateway's pre-auth network endpoints didn't answer, answered with an
    /// HTTP status this module has no specific handling for, or answered
    /// with a body that didn't parse (see `gateway.rs`'s own `to_net_error`
    /// for the full mapping). Portable as of this round: `gateway.rs`
    /// constructs this on every target, not just Linux.
    Unavailable(String),
    /// `forget()`/`connect()` was asked about an SSID this backend has no
    /// record of (legacy backends only — the gateway backend's equivalent
    /// case is `Classified(WifiFailureCode::NotFound)` below, since the
    /// gateway classifies it explicitly rather than leaving it generic).
    NotFound,
    /// D4a §5.3: the gateway's own nine-code closed Wi-Fi failure
    /// classification, carried through verbatim so `steps::network` can
    /// render the SPECIFIC operator-facing message the design doc's error
    /// table specifies, instead of the coarser "secured ⇒ probably wrong
    /// password, else probably unreachable" heuristic the legacy backends
    /// still fall back to (see `network_ui::NetConnectFailureKind::
    /// from_code`'s own doc comment for where this gets rendered).
    Classified(WifiFailureCode),
}

/// D4a §5.3's closed nine-code classification, plus `Unknown` for
/// forward-compat. See `parse`'s own doc comment for why an unrecognized
/// string must never be silently treated as a known-safe code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WifiFailureCode {
    WrongPassword,
    NotFound,
    OutOfRange,
    NoAdapter,
    DriverMissing,
    NoIp,
    Portal,
    BackendUnavailable,
    UnsupportedSecurity,
    /// A `code` value this build doesn't recognize — e.g. a newer gateway
    /// shipped a tenth code this shell predates. Coding convention #3's
    /// fail-closed spirit applies here even though this isn't strictly a
    /// security gate: an unrecognized code must NEVER be silently mapped
    /// onto an existing one (which could understate a failure) or treated
    /// as success (which would be exactly the "假裝連線成功" this whole
    /// backend swap exists to eliminate) — it lands in its own bucket and
    /// `steps::network` renders it with the same generic "couldn't connect"
    /// message a legacy-backend `NetError::ConnectFailed`/`Unavailable`
    /// already gets.
    Unknown,
}

impl WifiFailureCode {
    /// Exact string match only (coding convention #2) — the nine spellings
    /// are D4a §5.3's own literal `code` values, byte-for-byte. Private (no
    /// visibility modifier) is intentionally enough: Rust visibility
    /// extends to descendant modules by default, and `gateway` (this
    /// enum's only external caller) is a CHILD of this module.
    fn parse(raw: &str) -> Self {
        match raw {
            "wrong_password" => Self::WrongPassword,
            "not_found" => Self::NotFound,
            "out_of_range" => Self::OutOfRange,
            "no_adapter" => Self::NoAdapter,
            "driver_missing" => Self::DriverMissing,
            "no_ip" => Self::NoIp,
            "portal" => Self::Portal,
            "backend_unavailable" => Self::BackendUnavailable,
            "unsupported_security" => Self::UnsupportedSecurity,
            _ => Self::Unknown,
        }
    }
}

/// The Wi-Fi control surface `steps::network` drives — see this module's
/// header comment for the three implementations and why the trait exists.
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
    /// on both `fake.rs`/`nm.rs` (fulfilling the trait's own completeness
    /// requirement), but `steps::network`'s OOBE flow has no UI entry point
    /// that calls it — joining a network is the whole point of THIS step,
    /// managing previously-saved profiles is out of scope for a first-run
    /// wizard. `GatewayNetworkBackend` overrides this with an honest `Err`
    /// rather than inheriting a default — see its own doc comment (D4a
    /// §5.1: there deliberately IS no `forget` endpoint on the pre-auth
    /// path). `#[allow(dead_code)]`: unlike `ConnStatus`/`NetError`'s
    /// variants above, this is dead on EVERY platform in production, not
    /// just non-Linux, so it isn't `cfg_attr`-scoped.
    #[allow(dead_code)]
    fn forget(&self, ssid: &str) -> Result<(), NetError>;

    /// D4a §5.2/§5.4-2/§6: overall connectivity snapshot — link+IP+internet
    /// state, richer than `status()`'s Wi-Fi-only `ConnStatus`. Default
    /// implementation is "this backend doesn't know" — only
    /// `GatewayNetworkBackend` overrides it with a real answer;
    /// `fake.rs`/`nm.rs` (neither backend talks to a captive-portal
    /// prober or an IP stack) inherit this default rather than fabricating
    /// a state they have no way to observe. `steps::network`'s `kick_off_
    /// scan` treats an `Err` here as "no data" (`OobeUiState::net_status`
    /// stays/becomes `None`), never a stale or invented snapshot.
    fn network_status(&self) -> Result<NetworkStatus, NetError> {
        Err(NetError::Unavailable("network_status is not implemented by this backend".to_string()))
    }
}

/// Which concrete backend a given process run actually got — surfaced to
/// the operator (task brief: "Linux 偵測不到 NM...在 UI 誠實標示（不可假裝
/// 連線成功）") rather than kept as an internal implementation detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NetBackendKind {
    /// The gateway backend, reachable — D4a-5's normal appliance state.
    Real,
    /// `FakeNetworkBackend` — the Mac dev loop, or the explicit
    /// `DUDUCLAW_SHELL_FAKE_NET=1` override. `steps::network` renders the
    /// demo-mode notice for this state.
    Fake,
    /// D4a §5.4 ship-blocker fix: on an appliance, an unreachable gateway
    /// network backend no longer falls back to `Fake` — it stays on the
    /// REAL backend (every call to it honestly fails) and is surfaced as
    /// this distinct state, so `steps::network` can render "the network
    /// service isn't answering, please use a wired connection" instead of
    /// either a silent fabricated SSID list OR an indistinguishable-from-
    /// working success claim.
    Unavailable,
}

/// D4a §3 (env var the appliance image's systemd unit sets — pinned by the
/// image recipe, shared with the gateway process). Hand-copied, NOT a
/// `duduclaw-core` import: this crate stays detached from the parent
/// workspace (`Cargo.toml`'s own header comment on why gpui's dependency
/// tree is kept isolated). Changing this NAME requires updating BOTH
/// `duduclaw-core::appliance::APPLIANCE_ENV` and every systemd unit that
/// sets it, in lockstep with this constant.
const APPLIANCE_ENV: &str = "DUDUCLAW_APPLIANCE";

/// Hand-copied from `duduclaw-core::appliance::appliance_flag` — see this
/// module's header comment (and `APPLIANCE_ENV`'s own doc comment) for why
/// this crate cannot simply depend on that crate instead. MUST stay
/// byte-for-byte behaviorally identical to the authority (accepted
/// spellings, trim, ASCII case-insensitivity, fail-closed default on
/// anything else including unset/empty) — a future change to that
/// function's own logic that isn't mirrored here would silently desync
/// which processes on the same machine agree this is an appliance.
fn appliance_flag(val: Option<&str>) -> bool {
    match val.map(str::trim) {
        Some(v) if v.eq_ignore_ascii_case("1") => true,
        Some(v) if v.eq_ignore_ascii_case("true") => true,
        Some(v) if v.eq_ignore_ascii_case("yes") => true,
        _ => false,
    }
}

/// Reads the real `DUDUCLAW_APPLIANCE` env var through `appliance_flag`
/// above — the only place in this module that reads it directly.
fn is_appliance() -> bool {
    appliance_flag(std::env::var(APPLIANCE_ENV).ok().as_deref())
}

/// Resolves + constructs the backend for one scan/connect/status/forget
/// call — see this module's header comment for why callers only ever
/// invoke this from a background thread. Priority (highest first):
///   1. `DUDUCLAW_SHELL_FAKE_NET=1` — explicit dev/test override, any
///      platform. Same shape as `DUDUCLAW_SHELL_OOBE_LOCAL_ACCOUNT`
///      (`oobe/steps/account.rs`): an escape hatch for headless smoke runs
///      with no real backend reachable, not something a shipped kiosk
///      image would ever set.
///   2. `is_appliance()` — D4a-5 (2026-08-23): ALWAYS `GatewayNetworkBackend`
///      on an appliance, full stop. `GatewayNetworkBackend::probe()`'s own
///      reachability check decides `NetBackendKind::Real` vs. `Unavailable`,
///      but EITHER WAY the returned backend is the real one — see this
///      module's own header comment for why an unreachable gateway must
///      never fall through to step 3 (that fallthrough was the D4a §1.2/§5.4
///      ship-blocker: real appliances showing fabricated Wi-Fi networks).
///   3. Otherwise (not an appliance — the Mac dev loop, or a non-appliance
///      Linux box) — `FakeNetworkBackend`.
pub(crate) fn select_backend() -> (Box<dyn NetworkBackend>, NetBackendKind) {
    // Q1 (2026-08-24): behind the shipping gate. Showing fabricated Wi-Fi
    // networks on a real appliance is the exact D4a §1.2/§5.4 ship-blocker
    // named three lines up, and an env file must not be able to re-create it.
    // See `crate::shipping`.
    if crate::shipping::debug_env_is_one("DUDUCLAW_SHELL_FAKE_NET") {
        return (Box::new(FakeNetworkBackend::new()), NetBackendKind::Fake);
    }

    if is_appliance() {
        let (backend, probe_result) = GatewayNetworkBackend::probe();
        let kind = match probe_result {
            Ok(()) => NetBackendKind::Real,
            Err(e) => {
                eprintln!(
                    "[oobe/network] appliance mode: gateway network backend unreachable — staying on the REAL backend, NOT falling back to demo data (D4a §5.4 ship-blocker fix): {e:?}"
                );
                NetBackendKind::Unavailable
            }
        };
        return (Box::new(backend), kind);
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
    // test` is running in parallel in this same binary. Guards BOTH
    // `DUDUCLAW_SHELL_FAKE_NET` and `DUDUCLAW_APPLIANCE` — nothing else in
    // this crate touches the latter (grepped before adding it), so one lock
    // covering both is sufficient and simpler than two.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn clear_net_env() {
        unsafe {
            std::env::remove_var("DUDUCLAW_SHELL_FAKE_NET");
            std::env::remove_var("DUDUCLAW_APPLIANCE");
        }
    }

    #[test]
    fn fake_net_env_override_forces_fake_kind_regardless_of_platform() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_net_env();
        unsafe { std::env::set_var("DUDUCLAW_SHELL_FAKE_NET", "1") };
        let (_, kind) = select_backend();
        clear_net_env();
        // Q1 (2026-08-24): with the override gated out, this falls through to
        // the "not an appliance" tier, which is `Fake` anyway — so the
        // assertion is the same either way and stays a real check of the
        // override's own tier only in a debug build. The appliance case below
        // is where the gate is actually observable.
        assert_eq!(kind, NetBackendKind::Fake);
    }

    #[test]
    fn fake_net_override_wins_even_when_appliance_is_also_set_only_in_a_debug_build() {
        // The dev/test escape hatch must outrank appliance mode — otherwise
        // there would be no way to force Fake on a machine that also has
        // `DUDUCLAW_APPLIANCE=1` set (e.g. a VM used for both kinds of
        // testing).
        //
        // Q1 (2026-08-24): and in a SHIPPING build it must not outrank it at
        // all — fabricated Wi-Fi networks on a real appliance is the D4a
        // §1.2/§5.4 ship-blocker, and an operator env file must not be able
        // to bring it back. Both branches assert; see `crate::shipping`.
        let _g = ENV_LOCK.lock().unwrap();
        clear_net_env();
        unsafe {
            std::env::set_var("DUDUCLAW_SHELL_FAKE_NET", "1");
            std::env::set_var("DUDUCLAW_APPLIANCE", "1");
        }
        let (_, kind) = select_backend();
        clear_net_env();
        if crate::shipping::debug_affordances_available() {
            assert_eq!(kind, NetBackendKind::Fake);
        } else {
            assert_ne!(
                kind,
                NetBackendKind::Fake,
                "a shipping build must never fall back to demo Wi-Fi on an appliance"
            );
        }
    }

    #[test]
    fn default_with_no_env_set_is_fake_regardless_of_platform() {
        // As of D4a-5, `select_backend` no longer branches on `target_os`
        // at all (appliance-ness is a runtime env check, not a compile-time
        // one) — so this is now a SINGLE unconditional test where round 1
        // needed a `#[cfg(not(target_os = "linux"))]`-gated twin.
        let _g = ENV_LOCK.lock().unwrap();
        clear_net_env();
        let (_, kind) = select_backend();
        clear_net_env();
        assert_eq!(kind, NetBackendKind::Fake);
    }

    #[test]
    fn appliance_flag_set_must_never_resolve_to_fake_even_with_no_gateway_listening() {
        // THE regression test for the D4a §1.2/§5.4 ship-blocker: with
        // `DUDUCLAW_APPLIANCE=1` set and (in this test process) no gateway
        // actually listening on the loopback port, the OLD behavior would
        // have been "NM probe fails -> fall back to Fake". The fix must
        // land on `Unavailable`, never `Fake` — a real appliance must never
        // show fabricated SSIDs again.
        let _g = ENV_LOCK.lock().unwrap();
        clear_net_env();
        unsafe { std::env::set_var("DUDUCLAW_APPLIANCE", "1") };
        let (_, kind) = select_backend();
        clear_net_env();
        assert_ne!(kind, NetBackendKind::Fake, "an appliance with no reachable gateway must never resolve to the demo backend");
        assert_eq!(kind, NetBackendKind::Unavailable);
    }

    // ── appliance_flag (pure — no env access, safe under parallel tests) ──
    // Mirrors `duduclaw-core::appliance`'s own `flag_recognizes_truthy_
    // spellings`/`flag_trims_whitespace`/`flag_rejects_everything_else`
    // tests byte-for-byte — this IS the hand-copied authority, so its test
    // suite should look identical too.

    #[test]
    fn appliance_flag_recognizes_truthy_spellings() {
        assert!(appliance_flag(Some("1")));
        assert!(appliance_flag(Some("true")));
        assert!(appliance_flag(Some("TRUE")));
        assert!(appliance_flag(Some("True")));
        assert!(appliance_flag(Some("yes")));
        assert!(appliance_flag(Some("YES")));
    }

    #[test]
    fn appliance_flag_trims_whitespace() {
        assert!(appliance_flag(Some("  1  ")));
        assert!(appliance_flag(Some("\ttrue\n")));
    }

    #[test]
    fn appliance_flag_rejects_everything_else() {
        assert!(!appliance_flag(None));
        assert!(!appliance_flag(Some("")));
        assert!(!appliance_flag(Some("0")));
        assert!(!appliance_flag(Some("false")));
        assert!(!appliance_flag(Some("on")));
        assert!(!appliance_flag(Some("2")));
        assert!(!appliance_flag(Some("  ")));
    }

    // ── AccessPoint::secured() ──────────────────────────────────────────

    #[test]
    fn secured_is_false_only_for_open() {
        let open = AccessPoint { ssid: "x".to_string(), signal_bars: 1, security: "open".to_string(), known: false };
        assert!(!open.secured());
        for sec in ["psk", "wep", "8021x", "unknown", "anything-else"] {
            let ap = AccessPoint { ssid: "x".to_string(), signal_bars: 1, security: sec.to_string(), known: false };
            assert!(ap.secured(), "{sec} must be treated as secured");
        }
    }

    // ── InternetState ────────────────────────────────────────────────────

    #[test]
    fn internet_state_parses_the_four_known_spellings() {
        assert_eq!(InternetState::parse("online"), InternetState::Online);
        assert_eq!(InternetState::parse("portal"), InternetState::Portal);
        assert_eq!(InternetState::parse("offline"), InternetState::Offline);
        assert_eq!(InternetState::parse("unknown"), InternetState::Unknown);
    }

    #[test]
    fn internet_state_falls_back_to_unknown_for_anything_unrecognized() {
        assert_eq!(InternetState::parse(""), InternetState::Unknown);
        assert_eq!(InternetState::parse("ONLINE"), InternetState::Unknown, "case-sensitive, exact match only — coding convention #2");
        assert_eq!(InternetState::parse("bogus"), InternetState::Unknown);
    }

    #[test]
    fn only_online_and_portal_count_as_connected() {
        assert!(InternetState::Online.counts_as_connected());
        assert!(InternetState::Portal.counts_as_connected());
        assert!(!InternetState::Offline.counts_as_connected());
        assert!(!InternetState::Unknown.counts_as_connected());
    }

    // ── WifiFailureCode ──────────────────────────────────────────────────

    #[test]
    fn wifi_failure_code_parses_all_nine_closed_codes() {
        let cases: &[(&str, WifiFailureCode)] = &[
            ("wrong_password", WifiFailureCode::WrongPassword),
            ("not_found", WifiFailureCode::NotFound),
            ("out_of_range", WifiFailureCode::OutOfRange),
            ("no_adapter", WifiFailureCode::NoAdapter),
            ("driver_missing", WifiFailureCode::DriverMissing),
            ("no_ip", WifiFailureCode::NoIp),
            ("portal", WifiFailureCode::Portal),
            ("backend_unavailable", WifiFailureCode::BackendUnavailable),
            ("unsupported_security", WifiFailureCode::UnsupportedSecurity),
        ];
        for (raw, expected) in cases {
            assert_eq!(WifiFailureCode::parse(raw), *expected, "{raw}");
        }
    }

    #[test]
    fn wifi_failure_code_falls_back_to_unknown_never_panics_never_fabricates_success() {
        assert_eq!(WifiFailureCode::parse(""), WifiFailureCode::Unknown);
        assert_eq!(WifiFailureCode::parse("WRONG_PASSWORD"), WifiFailureCode::Unknown, "exact match only, not case-insensitive");
        assert_eq!(WifiFailureCode::parse("a-tenth-code-from-a-newer-gateway"), WifiFailureCode::Unknown);
    }

    // ── network_status default (fake.rs/nm.rs inherit it) ──────────────

    #[test]
    fn fake_backend_reports_network_status_unavailable_by_default() {
        // `FakeNetworkBackend` inherits the trait default rather than
        // fabricating a wired/portal state it has no way to observe.
        let backend = FakeNetworkBackend::new();
        assert!(matches!(backend.network_status(), Err(NetError::Unavailable(_))));
    }
}
