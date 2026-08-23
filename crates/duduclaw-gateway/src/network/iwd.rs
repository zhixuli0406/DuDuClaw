//! iwd (`net.connman.iwd`) D-Bus client — Linux-only (see `Cargo.toml`'s
//! `zbus` dependency, itself `target_os = "linux"`-gated). Kept as thin as
//! the zvariant coupling allows: any logic expressible as `&str -> X` lives
//! in `crate::network` instead (see that module's doc), so it is unit-
//! tested on every host, not only Linux. What's left here — property
//! extraction from `HashMap<String, OwnedValue>`, object-path traversal,
//! and the D-Bus calls themselves — genuinely cannot be tested without
//! either a live `iwd` or hand-built zvariant fixtures; this file's own
//! `#[cfg(test)]` module does the latter for the parts worth pinning.
//!
//! ## Async, generic `Proxy` — not `#[zbus::proxy]`
//!
//! Same reasoning `codrive/registry.rs` already documents for its own
//! NetworkManager client, copied forward because it applies with equal
//! force here: this crate's own dev loop (macOS) has no D-Bus/iwd to run
//! against at all, so correctness rests on reading the upstream D-Bus API
//! directly rather than on exercising it locally. Minimizing how much of
//! that correctness depends on trusting a macro's generated code (vs. a
//! plain `Proxy::new` + `.call()`/`.get_property()` whose types are spelled
//! out at the call site) was judged the safer trade under that constraint.
//! Unlike `registry.rs`'s NetworkManager client this file is async-native
//! (the gateway runs on tokio), and unlike that file it also needs to
//! **serve** one D-Bus interface (the passphrase [`WifiAgent`]) — for that
//! one piece, `#[zbus::interface]` IS used, deliberately: hand-rolling a
//! low-level `org.freedesktop.DBus.Properties`/introspection-compliant
//! service object is a strictly worse trade than trusting the macro for
//! server-side dispatch, which is a fundamentally different (and much
//! larger) surface than client-side method-call argument encoding.
//!
//! ## Honesty note on the iwd D-Bus surface itself
//!
//! Every object path, interface name, property, and method used below was
//! read from iwd's own upstream `doc/*.rst` D-Bus API documentation, NOT
//! spot-checked against a running `iwd` in this session — the design brief
//! (`commercial/docs/DESIGN-network-settings-2026-08.md` §7) ran a real
//! spike against `mac80211_hwsim` and confirmed scan/connect/forget/
//! persistence/permissions all work end to end, but that spike used `iwctl`
//! interactively, not this exact D-Bus call sequence. Every property read
//! degrades to `None`/a default rather than panicking specifically because
//! of that — see `prop_string`/`prop_bool`.
//!
//! Fresh `zbus::Connection::system()` per call (no pooled connection),
//! matching `registry.rs`'s own stated reasoning: these are human-time-
//! scale operations (a scan takes seconds), not a hot path worth pooling.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue};

use crate::network::{
    ScanResult, WifiError, WifiErrorCode, WifiLink, WifiNetwork, classify_iwd_error,
    dbm_centi_to_bars, iwd_network_type_to_security, normalize_station_state,
    sort_and_dedup_networks, validate_psk,
};

const IWD_SERVICE: &str = "net.connman.iwd";
const OBJECT_MANAGER_IFACE: &str = "org.freedesktop.DBus.ObjectManager";
const STATION_IFACE: &str = "net.connman.iwd.Station";
const NETWORK_IFACE: &str = "net.connman.iwd.Network";
const KNOWN_NETWORK_IFACE: &str = "net.connman.iwd.KnownNetwork";
const AGENT_MANAGER_PATH: &str = "/net/connman/iwd";
const AGENT_MANAGER_IFACE: &str = "net.connman.iwd.AgentManager";
const AGENT_OBJECT_PATH: &str = "/net/duduclaw/iwd_agent";
// The Agent's own D-Bus interface name is declared directly in the
// `#[zbus::interface(name = "...")]` attribute below (attribute macros need
// a literal there, not a `const` reference) — no separate constant to keep
// in sync with it.

/// How long [`wait_for_scan_to_settle`] polls `Station.Scanning` before
/// giving up and reading whatever `GetOrderedNetworks()` has (design §5.2).
const SCAN_TIMEOUT: Duration = Duration::from_secs(6);
const SCAN_POLL_INTERVAL: Duration = Duration::from_millis(300);
/// Ceiling on one `Network.Connect()` attempt (design §5.2's "加 30 秒
/// timeout").
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// The shape `org.freedesktop.DBus.ObjectManager.GetManagedObjects()`
/// returns: `path -> interface -> property -> value`.
type ManagedObjects = HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>>;

// ── Connection / ObjectManager helpers ─────────────────────────────────

async fn system_connection() -> Result<zbus::Connection, WifiError> {
    zbus::Connection::system()
        .await
        .map_err(|e| backend_unavailable(format!("failed to connect to the D-Bus system bus: {e}")))
}

async fn get_managed_objects(connection: &zbus::Connection) -> Result<ManagedObjects, WifiError> {
    let proxy = zbus::Proxy::new(connection, IWD_SERVICE, "/", OBJECT_MANAGER_IFACE)
        .await
        .map_err(|e| backend_unavailable(format!("object manager proxy build failed: {e}")))?;
    proxy
        .call("GetManagedObjects", &())
        .await
        .map_err(|e| backend_unavailable(format!("GetManagedObjects failed: {e}")))
}

fn find_interface_path<'a>(
    objects: &'a ManagedObjects,
    iface: &str,
) -> Option<&'a OwnedObjectPath> {
    objects
        .iter()
        .find(|(_, ifaces)| ifaces.contains_key(iface))
        .map(|(path, _)| path)
}

fn backend_unavailable(detail: String) -> WifiError {
    WifiError {
        code: WifiErrorCode::BackendUnavailable,
        detail,
    }
}

/// No `Station` object exists at all — ask [`crate::network::sysfs`] to
/// tell `NoAdapter` (no Wi-Fi hardware) apart from `DriverMissing` (a Wi-Fi
/// PCI device is present but no phy ever bound — design §1.1).
fn no_station_error() -> WifiError {
    let code = crate::network::sysfs::detect_adapter_absence();
    WifiError {
        code,
        detail: "no net.connman.iwd.Station object on the system bus".to_string(),
    }
}

fn dbus_error_name(err: &zbus::Error) -> &str {
    match err {
        zbus::Error::MethodError(name, ..) => name.as_str(),
        _ => "",
    }
}

fn prop_string(props: &HashMap<String, OwnedValue>, name: &str) -> Option<String> {
    props
        .get(name)
        .and_then(|v| String::try_from(v.clone()).ok())
}

fn prop_bool(props: &HashMap<String, OwnedValue>, name: &str) -> Option<bool> {
    props.get(name).and_then(|v| bool::try_from(v.clone()).ok())
}

async fn station_proxy<'c>(
    connection: &'c zbus::Connection,
    // Explicit `'c` (not elided) — the returned `Proxy<'c>` borrows the
    // object path string for its lifetime, so `path` must be tied to the
    // SAME lifetime as `connection`, not its own anonymous one. Caught by
    // the real Linux compile (rustc E0621); every call site already passes
    // a `path` that lives at least as long as `connection` in practice, so
    // this is a signature fix, not a behavior change.
    path: &'c OwnedObjectPath,
) -> Result<zbus::Proxy<'c>, WifiError> {
    zbus::Proxy::new(connection, IWD_SERVICE, path.as_str(), STATION_IFACE)
        .await
        .map_err(|e| backend_unavailable(format!("station proxy build failed: {e}")))
}

// ── scan ─────────────────────────────────────────────────────────────────

pub async fn scan(rescan: bool) -> Result<ScanResult, WifiError> {
    let connection = system_connection().await?;
    let initial_objects = get_managed_objects(&connection).await?;
    let Some(station_path) = find_interface_path(&initial_objects, STATION_IFACE).cloned() else {
        return Err(no_station_error());
    };
    let station = station_proxy(&connection, &station_path).await?;

    if rescan {
        request_scan(&station).await;
        wait_for_scan_to_settle(&station).await;
    }

    // Refetch: whatever the scan just discovered (or, if `rescan` was
    // false, simply the current state) is what `GetOrderedNetworks` below
    // is cross-referenced against — `initial_objects` was only good for
    // locating the Station path.
    let objects = get_managed_objects(&connection).await?;
    let ordered: Vec<(OwnedObjectPath, i16)> = station
        .call("GetOrderedNetworks", &())
        .await
        .map_err(|e| backend_unavailable(format!("GetOrderedNetworks failed: {e}")))?;
    let scanning: bool = station.get_property("Scanning").await.unwrap_or(false);

    let networks: Vec<WifiNetwork> = ordered
        .into_iter()
        .filter_map(|(path, signal)| {
            let props = objects.get(&path)?.get(NETWORK_IFACE)?;
            build_wifi_network(&objects, props, signal)
        })
        .collect();

    Ok(ScanResult {
        networks: sort_and_dedup_networks(networks),
        scanning,
    })
}

async fn request_scan(station: &zbus::Proxy<'_>) {
    if let Err(e) = station.call::<_, _, ()>("Scan", &()).await {
        // Busy/InProgress just means a scan is already running — we still
        // fall through to wait for it. Any other failure is logged and
        // swallowed too: `GetOrderedNetworks` afterwards still returns
        // whatever iwd's last completed scan produced, a better answer than
        // an error for what is, after all, only a REQUEST to refresh.
        let code = classify_iwd_error(dbus_error_name(&e), false);
        tracing::debug!(
            error = %e,
            code = code.code(),
            "iwd Scan() request did not succeed; proceeding with the last known results"
        );
    }
}

async fn wait_for_scan_to_settle(station: &zbus::Proxy<'_>) {
    let deadline = tokio::time::Instant::now() + SCAN_TIMEOUT;
    loop {
        match station.get_property::<bool>("Scanning").await {
            Ok(true) => {}
            // Done, or we genuinely can't tell (property read failed) —
            // either way, don't block the caller any further.
            Ok(false) | Err(_) => return,
        }
        if tokio::time::Instant::now() >= deadline {
            return;
        }
        tokio::time::sleep(SCAN_POLL_INTERVAL).await;
    }
}

/// `KnownNetwork` is a property ON the `Network` interface — an object
/// path reference to a DIFFERENT object, not a second interface at the
/// same path (design brief's iwd facts list). Presence alone means
/// `known = true`; this follows the reference one hop further to read the
/// `Hidden` flag when one exists, defaulting to `false` on any failure
/// along the way (never a positive guess).
fn is_hidden(objects: &ManagedObjects, props: &HashMap<String, OwnedValue>) -> bool {
    let Some(known_path) = props
        .get("KnownNetwork")
        .and_then(|v| OwnedObjectPath::try_from(v.clone()).ok())
    else {
        return false;
    };
    objects
        .get(&known_path)
        .and_then(|ifaces| ifaces.get(KNOWN_NETWORK_IFACE))
        .and_then(|known_props| prop_bool(known_props, "Hidden"))
        .unwrap_or(false)
}

fn build_wifi_network(
    objects: &ManagedObjects,
    props: &HashMap<String, OwnedValue>,
    signal_centi_dbm: i16,
) -> Option<WifiNetwork> {
    let ssid = prop_string(props, "Name")?;
    let network_type = prop_string(props, "Type").unwrap_or_default();
    Some(WifiNetwork {
        ssid,
        signal_bars: dbm_centi_to_bars(signal_centi_dbm),
        security: iwd_network_type_to_security(&network_type).to_string(),
        connected: prop_bool(props, "Connected").unwrap_or(false),
        known: props.contains_key("KnownNetwork"),
        hidden: is_hidden(objects, props),
    })
}

// ── connect ──────────────────────────────────────────────────────────────

/// Clears the shared passphrase slot on drop — including during an
/// unwinding panic, which neither `WifiAgent::request_passphrase`'s own
/// `.take()` nor the ordinary post-`do_connect` cleanup below covers (a
/// passphrase that was never requested, or a panic between storing it and
/// using it, would otherwise sit in memory past the call). This is this
/// module's one hand-written Drop guard — see the D4a task brief's "RAII
/// guard 或 scopeguard 風格的手寫 Drop（panic 安全）" requirement.
struct PassphraseGuard(Arc<Mutex<Option<String>>>);

impl Drop for PassphraseGuard {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.0.lock() {
            guard.take();
        }
    }
}

pub async fn connect(ssid: &str, psk: Option<&str>) -> Result<(), WifiError> {
    if let Some(p) = psk {
        validate_psk(p).map_err(|code| WifiError {
            code,
            detail: "psk length outside the 8..=63 character range".to_string(),
        })?;
    }

    let connection = system_connection().await?;
    let objects = get_managed_objects(&connection).await?;

    let Some(station_path) = find_interface_path(&objects, STATION_IFACE).cloned() else {
        return Err(no_station_error());
    };
    let Some((network_path, network_props)) = find_network_by_ssid(&objects, ssid) else {
        return Err(WifiError {
            code: WifiErrorCode::NotFound,
            detail: format!("SSID not present in the last iwd scan: {ssid}"),
        });
    };
    if prop_string(network_props, "Type").as_deref() == Some("wep") {
        return Err(WifiError {
            code: WifiErrorCode::UnsupportedSecurity,
            detail: "iwd does not support WEP".to_string(),
        });
    }

    // `_clear_on_drop` is intentionally unused by name (the RAII guard's
    // whole job is what happens when it's dropped, at function exit or on
    // an unwind, not anything invoked on it directly) — Rust's leading-
    // underscore convention marks that on purpose rather than as an
    // oversight.
    let passphrase_slot: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(psk.map(str::to_string)));
    let _clear_on_drop = PassphraseGuard(Arc::clone(&passphrase_slot));

    register_agent(&connection, Arc::clone(&passphrase_slot)).await?;
    // From here EVERY exit path must unregister, success or failure — the
    // ordinary (non-panic) half of the RAII guard's job.
    let result = do_connect(&connection, &station_path, &network_path, psk.is_some()).await;
    unregister_agent(&connection).await;

    if result.is_err() && psk.is_some() {
        // Best-effort: clear whatever credential iwd may already have
        // written for this SSID (design §5.2) so a retry is prompted for a
        // fresh password instead of silently reusing a wrong one from disk.
        if let Err(detail) = forget_best_effort(&connection, ssid).await {
            tracing::debug!(ssid = %ssid, error = %detail, "iwd: best-effort credential cleanup after a failed connect did not succeed");
        }
    }

    result
}

fn find_network_by_ssid<'a>(
    objects: &'a ManagedObjects,
    ssid: &str,
) -> Option<(OwnedObjectPath, &'a HashMap<String, OwnedValue>)> {
    // Exact equality, never `contains`/`starts_with` (coding convention 2) —
    // an SSID is user-visible, attacker-influenceable text.
    objects.iter().find_map(|(path, ifaces)| {
        let props = ifaces.get(NETWORK_IFACE)?;
        (prop_string(props, "Name").as_deref() == Some(ssid)).then(|| (path.clone(), props))
    })
}

async fn do_connect(
    connection: &zbus::Connection,
    station_path: &OwnedObjectPath,
    network_path: &OwnedObjectPath,
    psk_supplied: bool,
) -> Result<(), WifiError> {
    let network = zbus::Proxy::new(
        connection,
        IWD_SERVICE,
        network_path.as_str(),
        NETWORK_IFACE,
    )
    .await
    .map_err(|e| backend_unavailable(format!("network proxy build failed: {e}")))?;

    match tokio::time::timeout(CONNECT_TIMEOUT, network.call::<_, _, ()>("Connect", &())).await {
        Err(_) => {
            return Err(WifiError {
                code: WifiErrorCode::OutOfRange,
                detail: format!("Network.Connect() timed out after {CONNECT_TIMEOUT:?}"),
            });
        }
        Ok(Err(e)) => {
            let code = classify_iwd_error(dbus_error_name(&e), psk_supplied);
            return Err(WifiError {
                code,
                detail: format!("Network.Connect() failed: {e}"),
            });
        }
        Ok(Ok(())) => {}
    }

    // `Connect()` returning success is NOT "connected" — this is the exact
    // discipline `duduclaw-shell/src/oobe/network/nm.rs`'s own header
    // comment states for NetworkManager's `AddAndActivateConnection`, and
    // it applies with equal force to iwd: poll `Station.State` to a
    // terminal value instead of trusting the method reply alone.
    let station = station_proxy(connection, station_path).await?;
    let state: String = station.get_property("State").await.unwrap_or_default();
    if normalize_station_state(&state) == "connected" {
        Ok(())
    } else {
        Err(WifiError {
            code: WifiErrorCode::OutOfRange,
            detail: format!(
                "Station.State is '{state}' after a successful Connect() reply, not 'connected'"
            ),
        })
    }
}

// ── forget ───────────────────────────────────────────────────────────────

pub async fn forget(ssid: &str) -> Result<(), WifiError> {
    let connection = system_connection().await?;
    let objects = get_managed_objects(&connection).await?;
    let Some(known_path) = find_known_network_by_ssid(&objects, ssid) else {
        return Err(WifiError {
            code: WifiErrorCode::NotFound,
            detail: format!("no stored credential for SSID: {ssid}"),
        });
    };
    let known = zbus::Proxy::new(
        &connection,
        IWD_SERVICE,
        known_path.as_str(),
        KNOWN_NETWORK_IFACE,
    )
    .await
    .map_err(|e| backend_unavailable(format!("known network proxy build failed: {e}")))?;
    known
        .call::<_, _, ()>("Forget", &())
        .await
        .map_err(|e| WifiError {
            code: classify_iwd_error(dbus_error_name(&e), false),
            detail: format!("Forget() failed: {e}"),
        })
}

/// Cleanup-only variant of [`forget`] used by [`connect`] after a failed
/// attempt: "nothing stored for this SSID" is a normal outcome here (not
/// every failed connect ever wrote a credential), so it returns `Ok(())`
/// instead of the user-facing `NotFound` error `forget` would give a direct
/// caller.
async fn forget_best_effort(connection: &zbus::Connection, ssid: &str) -> Result<(), String> {
    let objects = get_managed_objects(connection)
        .await
        .map_err(|e| e.detail)?;
    let Some(known_path) = find_known_network_by_ssid(&objects, ssid) else {
        return Ok(());
    };
    let known = zbus::Proxy::new(
        connection,
        IWD_SERVICE,
        known_path.as_str(),
        KNOWN_NETWORK_IFACE,
    )
    .await
    .map_err(|e| e.to_string())?;
    known
        .call::<_, _, ()>("Forget", &())
        .await
        .map_err(|e| e.to_string())
}

fn find_known_network_by_ssid(objects: &ManagedObjects, ssid: &str) -> Option<OwnedObjectPath> {
    objects.iter().find_map(|(path, ifaces)| {
        let props = ifaces.get(KNOWN_NETWORK_IFACE)?;
        (prop_string(props, "Name").as_deref() == Some(ssid)).then(|| path.clone())
    })
}

// ── link_status ──────────────────────────────────────────────────────────

pub async fn link_status() -> Result<WifiLink, WifiError> {
    let connection = system_connection().await?;
    let objects = get_managed_objects(&connection).await?;
    let Some(station_path) = find_interface_path(&objects, STATION_IFACE).cloned() else {
        return Err(no_station_error());
    };
    let station = station_proxy(&connection, &station_path).await?;

    let raw_state: String = station.get_property("State").await.unwrap_or_default();
    let state = normalize_station_state(&raw_state).to_string();

    let connected_network: Option<OwnedObjectPath> =
        station.get_property("ConnectedNetwork").await.ok();

    let mut ssid = None;
    let mut security = None;
    let mut signal_bars = None;
    if let Some(net_path) = &connected_network {
        if let Some(props) = objects
            .get(net_path)
            .and_then(|ifaces| ifaces.get(NETWORK_IFACE))
        {
            ssid = prop_string(props, "Name");
            security =
                prop_string(props, "Type").map(|t| iwd_network_type_to_security(&t).to_string());
        }
        // Signal strength isn't a static `Network` property in iwd's model —
        // it's per-call output of `GetOrderedNetworks()`. A plain link-
        // status read has no reason to trigger a scan side effect, but the
        // method itself just returns iwd's already-cached ordering, so this
        // reuses it rather than leaving `signal_bars` permanently `None`
        // for an otherwise-connected network.
        if let Ok(ordered) = station
            .call::<_, _, Vec<(OwnedObjectPath, i16)>>("GetOrderedNetworks", &())
            .await
        {
            signal_bars = ordered
                .iter()
                .find(|(p, _)| p == net_path)
                .map(|(_, s)| dbm_centi_to_bars(*s));
        }
    }

    // `frequency`: not among the iwd D-Bus properties this module's design
    // brief documented (Station/Device/Network/KnownNetwork/AgentManager) —
    // left `None` rather than invented. A verified future addition could
    // wire this up once the actual iwd property (if any) is confirmed
    // against a real device, not guessed here.
    Ok(WifiLink {
        state,
        ssid,
        signal_bars,
        security,
        frequency: None,
    })
}

// ── Agent (passphrase provider) ─────────────────────────────────────────

/// `net.connman.iwd.Agent` — the sole piece of this file served rather than
/// called (see module doc for why `#[zbus::interface]` is the right tool
/// here specifically). Holds a one-shot passphrase slot the caller
/// populates before `Network.Connect()`; [`request_passphrase`]
/// (`RequestPassphrase` on the wire — the macro's default snake_case ->
/// PascalCase naming) takes it out with `.take()` the moment iwd asks,
/// leaving nothing behind to be read twice or to linger.
struct WifiAgent {
    passphrase: Arc<Mutex<Option<String>>>,
}

#[zbus::interface(name = "net.connman.iwd.Agent")]
impl WifiAgent {
    async fn release(&self) {}

    async fn request_passphrase(&self, network: ObjectPath<'_>) -> zbus::fdo::Result<String> {
        let _ = network; // one Agent instance serves exactly one in-flight connect attempt
        let mut guard = self.passphrase.lock().unwrap_or_else(|e| e.into_inner());
        guard.take().ok_or_else(|| {
            zbus::fdo::Error::Failed(
                "no passphrase available for this connection attempt".to_string(),
            )
        })
    }

    async fn cancel(&self, reason: String) {
        tracing::debug!(reason = %reason, "iwd agent: Cancel");
    }

    /// 8021x (enterprise) is out of scope for M1 — see design §5.2/§9.
    async fn request_private_key_passphrase(
        &self,
        network: ObjectPath<'_>,
    ) -> zbus::fdo::Result<String> {
        let _ = network;
        Err(zbus::fdo::Error::NotSupported(
            "enterprise (8021x) networks are not supported".to_string(),
        ))
    }

    async fn request_user_name_and_password(
        &self,
        network: ObjectPath<'_>,
    ) -> zbus::fdo::Result<(String, String)> {
        let _ = network;
        Err(zbus::fdo::Error::NotSupported(
            "enterprise (8021x) networks are not supported".to_string(),
        ))
    }

    async fn request_user_password(
        &self,
        network: ObjectPath<'_>,
        user: String,
    ) -> zbus::fdo::Result<String> {
        let _ = (network, user);
        Err(zbus::fdo::Error::NotSupported(
            "enterprise (8021x) networks are not supported".to_string(),
        ))
    }
}

async fn register_agent(
    connection: &zbus::Connection,
    passphrase_slot: Arc<Mutex<Option<String>>>,
) -> Result<(), WifiError> {
    connection
        .object_server()
        .at(
            AGENT_OBJECT_PATH,
            WifiAgent {
                passphrase: passphrase_slot,
            },
        )
        .await
        .map_err(|e| {
            backend_unavailable(format!("failed to publish the local iwd agent object: {e}"))
        })?;

    let manager = zbus::Proxy::new(
        connection,
        IWD_SERVICE,
        AGENT_MANAGER_PATH,
        AGENT_MANAGER_IFACE,
    )
    .await
    .map_err(|e| backend_unavailable(format!("agent manager proxy build failed: {e}")))?;
    // AGENT_OBJECT_PATH is a hardcoded, syntactically-valid literal — this
    // can only fail on a programmer typo, not on any runtime input.
    let path = ObjectPath::try_from(AGENT_OBJECT_PATH)
        .expect("AGENT_OBJECT_PATH is a valid static object path");
    manager
        .call::<_, _, ()>("RegisterAgent", &(path,))
        .await
        .map_err(|e| backend_unavailable(format!("RegisterAgent failed: {e}")))
}

/// Best-effort, always called (success or failure) after a connect attempt
/// — see [`connect`]. Never returns an error itself; both D-Bus calls log
/// and swallow, since by this point the connect outcome is already decided
/// and a cleanup failure must not overwrite it.
async fn unregister_agent(connection: &zbus::Connection) {
    if let Ok(manager) = zbus::Proxy::new(
        connection,
        IWD_SERVICE,
        AGENT_MANAGER_PATH,
        AGENT_MANAGER_IFACE,
    )
    .await
    {
        let path = ObjectPath::try_from(AGENT_OBJECT_PATH)
            .expect("AGENT_OBJECT_PATH is a valid static object path");
        if let Err(e) = manager.call::<_, _, ()>("UnregisterAgent", &(path,)).await {
            tracing::debug!(error = %e, "iwd: UnregisterAgent failed (best-effort cleanup)");
        }
    }
    if let Err(e) = connection
        .object_server()
        .remove::<WifiAgent, _>(AGENT_OBJECT_PATH)
        .await
    {
        tracing::debug!(error = %e, "iwd: failed to remove the local agent object (best-effort cleanup)");
    }
}

#[cfg(test)]
#[path = "tests_iwd.rs"]
mod tests;
