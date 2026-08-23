// Real Wi-Fi backend — NetworkManager over D-Bus
// (`org.freedesktop.NetworkManager`), `#[cfg(target_os = "linux")]` only.
// Shell-S3 (2026-08-21).
//
// ── DISABLED as of D4a-5 (2026-08-23) — kept, not deleted ─────────────────
// `select_backend()` (`mod.rs`) no longer constructs `NmNetworkBackend` at
// all. Reason: the DuDuClaw OS appliance image never ships NetworkManager —
// it ships iwd, driven by the GATEWAY over D-Bus, not by this shell process
// (`commercial/docs/DESIGN-network-settings-2026-08.md` §1.2/§2/§3). On a
// real appliance, `NmNetworkBackend::probe()` below ALWAYS failed (D-Bus was
// up, but no `org.freedesktop.NetworkManager` service was ever registered on
// it), and `select_backend`'s old fail-open policy silently substituted
// `FakeNetworkBackend` — meaning a real machine showed four fabricated SSIDs
// ("DuDu-Office" etc.) and any 8-character password "connected" the
// operator. That was the D4a §5.4 ship-blocker; `network/gateway.rs` is the
// fix (see `mod.rs`'s own header comment for the full story).
//
// This module is kept, not deleted, for one reason: the CODE here is
// correct — the D-Bus wire protocol, the "don't trust `AddAndActivateConnection`'s
// own reply, poll `Device.State` to a real terminal state" discipline, the
// SSID byte-decoding — none of that was ever the bug; only the CHOICE of
// backend (NetworkManager, which this image doesn't run) was wrong. A
// future scenario this shell might reasonably need — running on someone's
// EXISTING Linux desktop that already runs NetworkManager, rather than on a
// DuDuClaw-built appliance image — would want exactly this backend back.
// `mod nm;` in `mod.rs` carries `#[allow(dead_code)]` specifically so this
// file keeps being type-checked against every future change to
// `AccessPoint`/`ConnStatus`/`NetError`/`NetworkBackend` (all defined in
// `mod.rs`), rather than silently bit-rotting into code that no longer even
// compiles. Do NOT delete this file just because nothing calls it — that is
// the intended state, not a bug to "clean up".
//
// ── Why the generic `zbus::blocking::Proxy`, not the `#[zbus::proxy]` macro ──
// zbus's usual idiom is a trait annotated `#[zbus::proxy(interface = "...")]`
// that code-generates a typed client. This module deliberately uses the
// lower-level generic `Proxy::new` + `.call()`/`.get_property()` API
// instead — the same "hand-roll the wire layer, know exactly what's on it"
// choice `oobe/claim.rs` already made for its own HTTP client (see that
// module's header comment). It matters more here than it did there: this
// crate's Mac dev loop has no D-Bus/NetworkManager to run this module
// against at all (macOS has neither), so every line below is verified by
// the Linux cross-build (`BUILD-LINUX.md`) and, where noted, real-signature
// reading of the NetworkManager D-Bus spec — never by actually exercising
// it against a live bus locally. Minimizing how much of this module's
// correctness depends on trusting a macro's code generation (rather than
// plain, readable `Proxy::new`/`.call::<_, _, R>()` calls whose types are
// spelled out at each call site) was judged the safer trade under that
// constraint.
//
// ── Why a fresh `Connection` per backend instance, not a cached global ────
// `select_backend()` (`mod.rs`) constructs a new `NmNetworkBackend` (via
// `probe()`) once per background-thread call `steps::network` makes — same
// "fresh connection per call, no persistent connection pool" simplicity
// `oobe::claim`'s hand-rolled HTTP client already established. D-Bus
// connection setup is cheap relative to the human-timescale operations
// (scan, join a network) this module performs.
//
// ── Why `AddAndActivateConnection`'s reply isn't trusted alone ────────────
// NM's own D-Bus method returns as soon as it has ACCEPTED the connection
// request, not once the device has actually finished associating — join
// failures (wrong password, AP out of range, DHCP timeout) surface only as
// a LATER `Device.State` transition. Treating the method call's success as
// "connected" would be exactly the "假裝連線成功" the task brief calls out
// by name; `poll_until_settled` below is what makes success genuine for
// this backend, not just honestly-labeled failure for the demo fallback.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{Array, ObjectPath, OwnedObjectPath, OwnedValue, Value};

use super::{AccessPoint, ConnStatus, NetError, NetworkBackend};

const NM_SERVICE: &str = "org.freedesktop.NetworkManager";
const NM_PATH: &str = "/org/freedesktop/NetworkManager";
const NM_IFACE: &str = "org.freedesktop.NetworkManager";
const DEVICE_IFACE: &str = "org.freedesktop.NetworkManager.Device";
const WIRELESS_IFACE: &str = "org.freedesktop.NetworkManager.Device.Wireless";
const AP_IFACE: &str = "org.freedesktop.NetworkManager.AccessPoint";
const SETTINGS_PATH: &str = "/org/freedesktop/NetworkManager/Settings";
const SETTINGS_IFACE: &str = "org.freedesktop.NetworkManager.Settings";
const CONNECTION_IFACE: &str = "org.freedesktop.NetworkManager.Settings.Connection";

/// NM_DEVICE_TYPE_WIFI — NetworkManager D-Bus API spec, `Device` enum.
const NM_DEVICE_TYPE_WIFI: u32 = 2;
/// NM_DEVICE_STATE_ACTIVATED.
const NM_DEVICE_STATE_ACTIVATED: u32 = 100;
/// NM_DEVICE_STATE_FAILED.
const NM_DEVICE_STATE_FAILED: u32 = 120;
/// NM_DEVICE_STATE_PREPARE — the low end of the open-ended "still working
/// on it" band `status()` treats as `Connecting` (prepare/config/
/// need-auth/ip-config/ip-check/secondaries all fall between this and
/// `NM_DEVICE_STATE_ACTIVATED`).
const NM_DEVICE_STATE_PREPARE: u32 = 40;

/// NM_802_11_AP_FLAGS_PRIVACY — set on an access point's `Flags` property
/// when it advertises ANY security (WEP/WPA/WPA2); `WpaFlags`/`RsnFlags`
/// being non-zero is the more specific WPA/WPA2 signal, checked as a
/// belt-and-suspenders OR below.
const AP_FLAG_PRIVACY: u32 = 0x1;

const SCAN_WAIT: Duration = Duration::from_secs(4);
const SCAN_POLL_INTERVAL: Duration = Duration::from_millis(400);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const CONNECT_POLL_INTERVAL: Duration = Duration::from_millis(500);

pub(crate) struct NmNetworkBackend {
    connection: Connection,
}

impl NmNetworkBackend {
    /// The reachability check `select_backend` (`mod.rs`) uses to decide
    /// Real vs. its Fake fail-open path. Deliberately does NOT require a
    /// Wi-Fi device to exist: a machine with NetworkManager running but no
    /// wireless adapter is still a REAL, honest "no Wi-Fi hardware" state
    /// (`scan()`/`connect()` report `Unavailable`), not a reason to
    /// silently swap in demo SSIDs that don't correspond to any real
    /// hardware — see `mod.rs`'s own `select_backend` doc comment for why
    /// that distinction matters.
    pub(crate) fn probe() -> Result<Self, NetError> {
        let connection = Connection::system().map_err(|e| NetError::Unavailable(e.to_string()))?;
        {
            let nm = nm_proxy(&connection)?;
            // A successful `Connection::system()` only proves the bus
            // itself is up, not that NetworkManager's own service is
            // registered on it — reading a real property is the actual
            // reachability check. Scoped in its own block (`Proxy`
            // implements `Drop`, which — unlike a plain non-`Drop`
            // borrow — keeps its borrow of `connection` alive to the end
            // of its enclosing scope under NLL, not just its last use) so
            // `connection` is unborrowed again by the time it moves into
            // `Self` below.
            nm.get_property::<Vec<OwnedObjectPath>>("Devices").map_err(net_err)?;
        }
        Ok(Self { connection })
    }

    fn wifi_device_path(&self) -> Result<OwnedObjectPath, NetError> {
        let nm = nm_proxy(&self.connection)?;
        let devices: Vec<OwnedObjectPath> = nm.get_property("Devices").map_err(net_err)?;
        for path in devices {
            let Ok(dev) = device_proxy(&self.connection, path.as_str()) else { continue };
            let device_type: u32 = dev.get_property("DeviceType").unwrap_or(0);
            if device_type == NM_DEVICE_TYPE_WIFI {
                return Ok(path);
            }
        }
        Err(NetError::Unavailable("no Wi-Fi adapter found".to_string()))
    }

    /// Best-effort SSID of whatever access point `device_path`'s device is
    /// currently associated (or associating) with — `None` covers every
    /// "can't tell" case (no active AP object, a lookup along the way
    /// failed) uniformly, since every call site here already has its own
    /// fallback for `None`.
    fn active_ssid_at(&self, device_path: &str) -> Option<String> {
        let wireless = wireless_proxy(&self.connection, device_path).ok()?;
        let ap_path: OwnedObjectPath = wireless.get_property("ActiveAccessPoint").ok()?;
        if ap_path.as_str() == "/" {
            return None;
        }
        let ap = ap_proxy(&self.connection, ap_path.as_str()).ok()?;
        let ssid: Vec<u8> = ap.get_property("Ssid").ok()?;
        Some(ssid_to_string(&ssid))
    }
}

impl NetworkBackend for NmNetworkBackend {
    fn scan(&self) -> Result<Vec<AccessPoint>, NetError> {
        let device_path = self.wifi_device_path()?;
        let wireless = wireless_proxy(&self.connection, device_path.as_str())?;

        let baseline_scan: i64 = wireless.get_property("LastScan").unwrap_or(-1);
        // `RequestScan` takes one `a{sv}` options argument — empty means a
        // full scan of every channel, the same default every desktop
        // Wi-Fi picker requests.
        let empty_options: HashMap<String, Value> = HashMap::new();
        wireless.call::<_, _, ()>("RequestScan", &empty_options).map_err(|e| NetError::ScanFailed(e.to_string()))?;

        // Best-effort wait for the scan to actually land — see this
        // module's header comment. NM has no synchronous "scan finished"
        // call; polling `LastScan` (an ms-since-boot timestamp that
        // changes when a scan completes) is the same technique most
        // `nmcli`-alike tools use. If it never changes within the budget,
        // fall through and read whatever `AccessPoints` currently holds —
        // cached results from an earlier scan are still honest data, not
        // nothing.
        let deadline = Instant::now() + SCAN_WAIT;
        while Instant::now() < deadline {
            std::thread::sleep(SCAN_POLL_INTERVAL);
            if let Ok(last_scan) = wireless.get_property::<i64>("LastScan") {
                if last_scan != baseline_scan {
                    break;
                }
            }
        }

        let ap_paths: Vec<OwnedObjectPath> = wireless.get_property("AccessPoints").map_err(net_err)?;
        let mut seen = std::collections::HashSet::new();
        let mut aps = Vec::new();
        for path in ap_paths {
            let Ok(ap) = ap_proxy(&self.connection, path.as_str()) else { continue };
            let Ok(ssid_bytes) = ap.get_property::<Vec<u8>>("Ssid") else { continue };
            if ssid_bytes.is_empty() {
                // Hidden network — no name to show, same "skip it" UX
                // every consumer Wi-Fi picker takes rather than rendering
                // a blank row.
                continue;
            }
            let ssid = ssid_to_string(&ssid_bytes);
            if !seen.insert(ssid.clone()) {
                // NetworkManager reports one AP object per (SSID, BSSID,
                // band) — the same physical network's 2.4GHz/5GHz radios,
                // or multiple mesh nodes, all share one SSID. Collapse to
                // one row per SSID like every consumer Wi-Fi picker does.
                continue;
            }
            let strength: u8 = ap.get_property("Strength").unwrap_or(0);
            let flags: u32 = ap.get_property("Flags").unwrap_or(0);
            let wpa_flags: u32 = ap.get_property("WpaFlags").unwrap_or(0);
            let rsn_flags: u32 = ap.get_property("RsnFlags").unwrap_or(0);
            let secured = (flags & AP_FLAG_PRIVACY) != 0 || wpa_flags != 0 || rsn_flags != 0;
            // D4a-5: `AccessPoint` now stores a `security` string (the
            // gateway backend's own `Network.Type` shape) instead of a
            // `secured` bool — this legacy backend never distinguished
            // WEP/802.1X from plain WPA/WPA2 in the first place, so
            // collapsing back to a binary "psk"/"open" string is a faithful
            // translation of what this code already knew, not a loss of
            // information. `known: false`: this backend never queried NM's
            // Settings service during a scan (only `forget()` does, and
            // only by SSID lookup), so it has no saved-credential signal to
            // report here.
            aps.push(AccessPoint {
                ssid,
                signal_bars: strength_to_bars(strength),
                security: if secured { "psk" } else { "open" }.to_string(),
                known: false,
            });
        }
        aps.sort_by(|a, b| b.signal_bars.cmp(&a.signal_bars).then_with(|| a.ssid.cmp(&b.ssid)));
        Ok(aps)
    }

    fn connect(&self, ssid: &str, psk: Option<&str>) -> Result<(), NetError> {
        let device_path = self.wifi_device_path()?;

        let mut wireless_settings: HashMap<String, Value> = HashMap::new();
        wireless_settings.insert("ssid".to_string(), Value::from(Array::from(ssid.as_bytes().to_vec())));

        let mut connection_settings: HashMap<String, Value> = HashMap::new();
        connection_settings.insert("id".to_string(), Value::from(ssid));
        connection_settings.insert("type".to_string(), Value::from("802-11-wireless"));

        let mut settings: HashMap<String, HashMap<String, Value>> = HashMap::new();
        settings.insert("connection".to_string(), connection_settings);
        settings.insert("802-11-wireless".to_string(), wireless_settings);

        if let Some(psk) = psk {
            let mut security: HashMap<String, Value> = HashMap::new();
            security.insert("key-mgmt".to_string(), Value::from("wpa-psk"));
            security.insert("psk".to_string(), Value::from(psk));
            settings.insert("802-11-wireless-security".to_string(), security);
        }

        let nm = nm_proxy(&self.connection)?;
        // "/" is NetworkManager's own convention for "no specific object"
        // — always a valid `ObjectPath`, never user/network-controlled, so
        // the `expect` here can never fire from external input.
        let no_specific_object = ObjectPath::try_from("/").expect("\"/\" is always a valid D-Bus object path");
        let device_obj_path = ObjectPath::try_from(device_path.as_str()).map_err(|e| NetError::ConnectFailed(e.to_string()))?;
        nm.call::<_, _, (OwnedObjectPath, OwnedObjectPath)>("AddAndActivateConnection", &(settings, device_obj_path, no_specific_object))
            .map_err(|e| NetError::ConnectFailed(e.to_string()))?;

        poll_until_settled(&self.connection, device_path.as_str())
    }

    fn status(&self) -> ConnStatus {
        let Ok(device_path) = self.wifi_device_path() else {
            return ConnStatus::Disconnected;
        };
        let Ok(dev) = device_proxy(&self.connection, device_path.as_str()) else {
            return ConnStatus::Disconnected;
        };
        let state: u32 = dev.get_property("State").unwrap_or(0);
        match state {
            NM_DEVICE_STATE_ACTIVATED => match self.active_ssid_at(device_path.as_str()) {
                Some(ssid) => ConnStatus::Connected { ssid },
                None => ConnStatus::Disconnected,
            },
            s if (NM_DEVICE_STATE_PREPARE..NM_DEVICE_STATE_ACTIVATED).contains(&s) => {
                ConnStatus::Connecting { ssid: self.active_ssid_at(device_path.as_str()).unwrap_or_default() }
            }
            _ => ConnStatus::Disconnected,
        }
    }

    fn forget(&self, ssid: &str) -> Result<(), NetError> {
        let settings = Proxy::new(&self.connection, NM_SERVICE, SETTINGS_PATH, SETTINGS_IFACE).map_err(net_err)?;
        let paths: Vec<OwnedObjectPath> = settings.call("ListConnections", &()).map_err(net_err)?;
        for path in paths {
            let Ok(conn) = Proxy::new(&self.connection, NM_SERVICE, path.as_str(), CONNECTION_IFACE) else { continue };
            let Ok(all) = conn.call::<_, _, HashMap<String, HashMap<String, OwnedValue>>>("GetSettings", &()) else { continue };
            let matches_ssid = all
                .get("802-11-wireless")
                .and_then(|w| w.get("ssid"))
                .and_then(|v| Vec::<u8>::try_from(v.clone()).ok())
                .map(|bytes| ssid_to_string(&bytes) == ssid)
                .unwrap_or(false);
            if matches_ssid {
                conn.call::<_, _, ()>("Delete", &()).map_err(net_err)?;
                return Ok(());
            }
        }
        Err(NetError::NotFound)
    }
}

/// Polls `Device.State` after `AddAndActivateConnection` until it settles
/// into a terminal state or the budget runs out — see this module's header
/// comment for why the method call's own success reply is never trusted
/// alone. Deliberately reports every non-`Activated` outcome (device
/// entered `Failed`, or the poll window ran out) as a plain `Err` with no
/// "was it the password" classification attached — see `NetError`'s own
/// doc comment for why that classification lives in `steps::network`
/// instead, using information (whether THIS join attempt carried a PSK)
/// only the UI layer has.
fn poll_until_settled(connection: &Connection, device_path: &str) -> Result<(), NetError> {
    let dev = device_proxy(connection, device_path)?;
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    loop {
        let state: u32 = dev.get_property("State").unwrap_or(0);
        if state == NM_DEVICE_STATE_ACTIVATED {
            return Ok(());
        }
        if state == NM_DEVICE_STATE_FAILED {
            return Err(NetError::ConnectFailed("device entered NM_DEVICE_STATE_FAILED".to_string()));
        }
        if Instant::now() >= deadline {
            return Err(NetError::Timeout);
        }
        std::thread::sleep(CONNECT_POLL_INTERVAL);
    }
}

fn nm_proxy(connection: &Connection) -> Result<Proxy<'_>, NetError> {
    Proxy::new(connection, NM_SERVICE, NM_PATH, NM_IFACE).map_err(net_err)
}

fn device_proxy<'c>(connection: &'c Connection, path: &str) -> Result<Proxy<'c>, NetError> {
    Proxy::new(connection, NM_SERVICE, path.to_string(), DEVICE_IFACE).map_err(net_err)
}

fn wireless_proxy<'c>(connection: &'c Connection, path: &str) -> Result<Proxy<'c>, NetError> {
    Proxy::new(connection, NM_SERVICE, path.to_string(), WIRELESS_IFACE).map_err(net_err)
}

fn ap_proxy<'c>(connection: &'c Connection, path: &str) -> Result<Proxy<'c>, NetError> {
    Proxy::new(connection, NM_SERVICE, path.to_string(), AP_IFACE).map_err(net_err)
}

fn net_err(e: zbus::Error) -> NetError {
    NetError::Unavailable(e.to_string())
}

/// A D-Bus `ay` SSID is not guaranteed to be valid UTF-8 (802.11 allows
/// arbitrary bytes) — never trust external data, decode lossily rather than
/// risking a panic on a malformed/adversarial broadcast. Coding convention
/// #1 in spirit (never assume well-formed input at a boundary), even though
/// this isn't a byte-INDEX slice.
fn ssid_to_string(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// NetworkManager reports `Strength` as a 0..=100 percentage; `AccessPoint::
/// signal_bars` is the same 1..=4 decorative scale `fake_data::
/// FakeWifiNetwork` already uses. Floors at 1 (never 0) — same reasoning
/// `fake_data.rs`'s own field never uses 0 either: a listed network always
/// shows at least one bar, "no bars at all" isn't a rendered state
/// `steps::network`'s `signal_bars` helper (`oobe/steps/network.rs`)
/// distinguishes from "not shown".
fn strength_to_bars(pct: u8) -> u8 {
    match pct {
        0..=24 => 1,
        25..=49 => 2,
        50..=74 => 3,
        _ => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strength_to_bars_covers_the_full_percentage_range() {
        assert_eq!(strength_to_bars(0), 1);
        assert_eq!(strength_to_bars(24), 1);
        assert_eq!(strength_to_bars(25), 2);
        assert_eq!(strength_to_bars(49), 2);
        assert_eq!(strength_to_bars(50), 3);
        assert_eq!(strength_to_bars(74), 3);
        assert_eq!(strength_to_bars(75), 4);
        assert_eq!(strength_to_bars(100), 4);
    }

    #[test]
    fn ssid_to_string_never_panics_on_invalid_utf8() {
        let invalid = vec![0xff, 0xfe, 0x00, 0xff];
        // The only assertion that matters is that this doesn't panic —
        // the exact replacement-character output isn't a contract.
        let _ = ssid_to_string(&invalid);
    }

    #[test]
    fn ssid_to_string_round_trips_valid_utf8() {
        assert_eq!(ssid_to_string("DuDu-Office".as_bytes()), "DuDu-Office");
    }

    /// Live-fire pairing check against a REAL NetworkManager on the system
    /// bus — `#[ignore]`d, same "never run by a bare `cargo test`" contract
    /// `oobe::claim`'s own `live_first_run_claim_against_env_gateway` test
    /// establishes. Verification playbook (see `BUILD-LINUX.md`'s "Shell-S3"
    /// section):
    ///   `cargo test -- --ignored live_probe_against_real_networkmanager --nocapture`
    /// inside a container/VM with `dbus-daemon --system` and
    /// `NetworkManager` actually running. Asserts `probe()` succeeds against
    /// a real service (the actual D-Bus wire interaction this module can
    /// never exercise from this crate's own Mac dev loop or from a plain
    /// `cargo test` sandbox with no bus at all); `scan()`/`status()` are
    /// exercised too but NOT asserted to succeed — a container/VM with no
    /// Wi-Fi radio is expected to report `Unavailable("no Wi-Fi adapter
    /// found")` from `scan()`, which is itself the honest, correct outcome
    /// this module's own header comment documents (`probe()` doesn't
    /// require Wi-Fi hardware, `scan()` does) — this test's job is to prove
    /// the wire protocol round-trips against a genuine service, not to
    /// fabricate Wi-Fi hardware that doesn't exist in the test environment.
    #[test]
    #[ignore = "requires a real NetworkManager + D-Bus system bus — see BUILD-LINUX.md"]
    fn live_probe_against_real_networkmanager() {
        let backend = NmNetworkBackend::probe().expect("probe() must succeed against a real, reachable NetworkManager service");
        eprintln!("[live] probe() ok — D-Bus system bus + org.freedesktop.NetworkManager both reachable");

        match backend.scan() {
            Ok(aps) => eprintln!("[live] scan() ok — {} access point(s)", aps.len()),
            Err(e) => eprintln!("[live] scan() error (expected with no real Wi-Fi adapter in this environment): {e:?}"),
        }

        eprintln!("[live] status() = {:?}", backend.status());

        // `forget()` doesn't need Wi-Fi hardware at all (it only talks to
        // NetworkManager's Settings service, listing/inspecting saved
        // connection PROFILES) — exercising it here for real is what
        // actually round-trips this module's most structurally complex
        // D-Bus type (`GetSettings`'s nested `a{sa{sv}}` deserialized into
        // `HashMap<String, HashMap<String, OwnedValue>>`), which `scan()`'s
        // early `wifi_device_path()` failure above never reaches.
        // `Err(NotFound)` is the expected, correct result here (no
        // connection profile actually has this SSID) — a panic/wrong-type
        // deserialize error would be the finding worth catching.
        match backend.forget("duduclaw-live-probe-nonexistent-ssid") {
            Err(NetError::NotFound) => eprintln!("[live] forget() ok — NotFound as expected (ListConnections + GetSettings round-tripped for real)"),
            other => eprintln!("[live] forget() unexpected result: {other:?}"),
        }
    }
}
