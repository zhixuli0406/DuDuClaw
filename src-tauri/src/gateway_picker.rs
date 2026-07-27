//! Desktop Gateway picker + LAN discovery (WP-GW).
//!
//! Before login the desktop shell shows a **Gateway selection** page
//! (`/gateway-picker`, bundled UI). This module backs it with four Tauri
//! commands + a persisted settings file:
//!
//! - [`gateway_discover`] — browse `_duduclaw._tcp.local.` over mDNS (3s) and
//!   return de-duplicated gateway candidates.
//! - [`gateway_health`] — `GET <url>/healthz` (2s timeout) to validate a
//!   discovered / manually-entered endpoint and read its version + name.
//! - [`gateway_select`] — validate the URL scheme (http/https, fail-closed),
//!   persist it (`~/.duduclaw/desktop.json`: `last_gateway` + `recent` ≤ 5),
//!   then navigate the main window to the chosen gateway. Remote selection
//!   releases the preheated local sidecar.
//! - [`gateway_last`] — read the persisted selection for the auto-connect
//!   countdown.
//! - [`gateway_local_status`] — the local sidecar's live status for the
//!   "本機" card.
//!
//! Discovery mirrors the gateway's advertising format (see
//! `duduclaw-gateway::mdns`), which is the canonical spec both sides share.
//! The desktop crate is detached from the workspace and cannot depend on the
//! gateway, so the small TXT→record mapping is re-implemented here and unit
//! tested against the same expectations.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};

use crate::lifecycle;
use crate::sidecar::{SidecarManager, SidecarStatus};

/// DNS-SD service type advertised by every DuDuClaw gateway.
const SERVICE_TYPE: &str = "_duduclaw._tcp.local.";
/// Max gateways remembered in the `recent` list.
const RECENT_MAX: usize = 5;

/// A discovered / remembered gateway candidate. Serialized to the picker UI.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GatewayRecord {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub version: String,
    pub tls: bool,
    pub url: String,
}

/// Health probe result for a single gateway URL.
#[derive(Debug, Clone, Serialize)]
pub struct HealthReport {
    pub ok: bool,
    pub version: Option<String>,
    pub name: Option<String>,
    /// Populated on failure so the UI can show a concrete reason.
    pub error: Option<String>,
}

/// Local sidecar status for the "本機" card.
#[derive(Debug, Clone, Serialize)]
pub struct LocalStatus {
    /// One of `running` / `stopped` / `error`.
    pub status: String,
    pub port: u16,
    pub url: String,
}

/// Persisted desktop gateway settings (`~/.duduclaw/desktop.json`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DesktopGatewayState {
    /// The last gateway the user connected to, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_gateway: Option<GatewayRecord>,
    /// Most-recently-used gateways, newest first, capped at [`RECENT_MAX`].
    #[serde(default)]
    pub recent: Vec<GatewayRecord>,
}

/// Path to the persisted desktop settings file.
fn desktop_json_path() -> std::path::PathBuf {
    lifecycle::duduclaw_home().join("desktop.json")
}

/// Read the persisted state, fail-safe (missing / corrupt → default empty).
pub fn read_state() -> DesktopGatewayState {
    read_state_from(&desktop_json_path())
}

fn read_state_from(path: &std::path::Path) -> DesktopGatewayState {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Persist state atomically (temp + rename), creating the home dir if needed.
fn write_state(state: &DesktopGatewayState) -> Result<(), String> {
    write_state_to(&desktop_json_path(), state)
}

fn write_state_to(path: &std::path::Path, state: &DesktopGatewayState) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create dir: {e}"))?;
    }
    let json = serde_json::to_string_pretty(state).map_err(|e| format!("serialize: {e}"))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json).map_err(|e| format!("write temp: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("rename: {e}"))?;
    Ok(())
}

/// Pure state transition: record `chosen` as the last gateway and move it to the
/// front of `recent`, de-duplicated by URL and truncated to [`RECENT_MAX`].
/// Returns a new state (immutable update — never mutates the input in place).
pub fn with_selection(prev: &DesktopGatewayState, chosen: &GatewayRecord) -> DesktopGatewayState {
    let mut recent: Vec<GatewayRecord> = Vec::with_capacity(RECENT_MAX);
    recent.push(chosen.clone());
    for r in &prev.recent {
        if recent.len() >= RECENT_MAX {
            break;
        }
        // De-dupe by URL (case-insensitive, trailing-slash-insensitive).
        if !same_url(&r.url, &chosen.url) {
            recent.push(r.clone());
        }
    }
    DesktopGatewayState {
        last_gateway: Some(chosen.clone()),
        recent,
    }
}

/// Two gateway URLs are "the same" if they match ignoring case and a trailing
/// slash — so `http://h:1/` and `http://H:1` don't create duplicate entries.
fn same_url(a: &str, b: &str) -> bool {
    let norm = |s: &str| s.trim_end_matches('/').to_ascii_lowercase();
    norm(a) == norm(b)
}

/// Validate a gateway URL: scheme MUST be `http` or `https` (fail-closed —
/// anything else, including `file:`, `javascript:`, `data:`, or an unparsable
/// string, is rejected). Returns the normalized origin URL (`scheme://host:port/`)
/// on success.
pub fn validate_gateway_url(input: &str) -> Result<String, String> {
    let raw = input.trim();
    if raw.is_empty() {
        return Err("empty URL".to_string());
    }
    let parsed = url::Url::parse(raw).map_err(|e| format!("invalid URL: {e}"))?;
    match parsed.scheme() {
        "http" | "https" => {}
        other => return Err(format!("unsupported scheme '{other}' (only http/https)")),
    }
    if parsed.host_str().unwrap_or("").is_empty() {
        return Err("missing host".to_string());
    }
    // Rebuild a clean origin URL; drop any path/query/fragment/userinfo.
    let scheme = parsed.scheme();
    let host = parsed.host_str().unwrap_or_default();
    let out = match parsed.port() {
        Some(p) => format!("{scheme}://{host}:{p}/"),
        None => format!("{scheme}://{host}/"),
    };
    Ok(out)
}

/// True when a chosen gateway URL points at the local sidecar (loopback host).
/// Used to decide whether to keep the preheated sidecar or release it.
fn is_local_url(url: &str) -> bool {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_string()))
        .map(|h| {
            let h = h.to_ascii_lowercase();
            h == "127.0.0.1" || h == "localhost" || h == "::1" || h == "[::1]"
        })
        .unwrap_or(false)
}

// ── Commands ────────────────────────────────────────────────────────────────

/// Browse the LAN for gateways (mDNS, ~3s convergence). Best-effort: mDNS
/// failures surface as an error string; an empty LAN returns an empty list.
#[tauri::command]
pub async fn gateway_discover() -> Result<Vec<GatewayRecord>, String> {
    // mdns-sd's daemon + receiver are synchronous; run the bounded browse on a
    // blocking thread so we don't stall the async runtime.
    tauri::async_runtime::spawn_blocking(discover_blocking)
        .await
        .map_err(|e| format!("discover task join: {e}"))?
}

fn discover_blocking() -> Result<Vec<GatewayRecord>, String> {
    use mdns_sd::{ServiceDaemon, ServiceEvent};

    let daemon = ServiceDaemon::new().map_err(|e| format!("mdns init: {e}"))?;
    let receiver = daemon
        .browse(SERVICE_TYPE)
        .map_err(|e| format!("mdns browse: {e}"))?;

    // De-dupe by host:port, keeping the record with the richest data.
    let mut found: BTreeMap<String, GatewayRecord> = BTreeMap::new();
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match receiver.recv_timeout(remaining) {
            Ok(ServiceEvent::ServiceResolved(info)) => {
                let ipv4 = info
                    .get_addresses_v4()
                    .into_iter()
                    .next()
                    .map(|ip| ip.to_string());
                let rec = record_from_resolved(
                    |k| info.get_property_val_str(k).map(|s| s.to_string()),
                    info.get_hostname(),
                    info.get_port(),
                    ipv4.as_deref(),
                );
                let key = format!("{}:{}", rec.host.to_ascii_lowercase(), rec.port);
                found.entry(key).or_insert(rec);
            }
            Ok(_) => {}
            Err(_) => break, // timeout (deadline reached) — stop collecting
        }
    }
    let _ = daemon.shutdown();
    Ok(found.into_values().collect())
}

/// Map a resolved mDNS record to a [`GatewayRecord`]. Mirrors
/// `duduclaw-gateway::mdns::parse_gateway_advert` — the shared advertising
/// contract. Kept pure for unit testing.
fn record_from_resolved<F>(txt: F, hostname: &str, port: u16, ipv4: Option<&str>) -> GatewayRecord
where
    F: Fn(&str) -> Option<String>,
{
    let tls = txt("tls")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    let version = txt("version").unwrap_or_default();
    let host_display = {
        // `office.local.` → `office` (strip trailing dot + `.local` suffix).
        let h = hostname.trim_end_matches('.');
        let h = h.strip_suffix(".local").unwrap_or(h);
        if h.is_empty() {
            ipv4.unwrap_or("").to_string()
        } else {
            h.to_string()
        }
    };
    let target = ipv4
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| host_display.clone());
    let scheme = if tls { "https" } else { "http" };
    let url = format!("{scheme}://{target}:{port}/");
    let name = txt("name")
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| host_display.clone());
    GatewayRecord {
        name,
        host: host_display,
        port,
        version,
        tls,
        url,
    }
}

/// Probe `GET <url>/healthz` (2s timeout). A 200 means reachable; JSON body
/// (`{version, name}`) is surfaced when present. Never navigates.
#[tauri::command]
pub async fn gateway_health(url: String) -> Result<HealthReport, String> {
    let origin = validate_gateway_url(&url)?;
    let health_url = format!("{}healthz", origin); // origin ends with '/'
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(|e| format!("http client: {e}"))?;

    match client.get(&health_url).send().await {
        Ok(resp) => {
            let ok = resp.status().is_success();
            if !ok {
                return Ok(HealthReport {
                    ok: false,
                    version: None,
                    name: None,
                    error: Some(format!("HTTP {}", resp.status().as_u16())),
                });
            }
            let (version, name) = match resp.json::<serde_json::Value>().await {
                Ok(v) => (
                    v.get("version").and_then(|x| x.as_str()).map(String::from),
                    v.get("name").and_then(|x| x.as_str()).map(String::from),
                ),
                Err(_) => (None, None), // 200 but non-JSON body (e.g. plain "ok")
            };
            Ok(HealthReport {
                ok: true,
                version,
                name,
                error: None,
            })
        }
        Err(e) => Ok(HealthReport {
            ok: false,
            version: None,
            name: None,
            error: Some(reason(&e)),
        }),
    }
}

/// Human-ish one-liner for a reqwest error (timeout vs connect vs other).
fn reason(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        "connection timed out".to_string()
    } else if e.is_connect() {
        "could not connect".to_string()
    } else {
        e.to_string()
    }
}

/// Validate + persist a gateway selection, then navigate the main window to it.
/// Selecting a remote gateway releases the preheated local sidecar so we don't
/// leave a gateway running the user didn't want (design §2).
#[tauri::command]
pub async fn gateway_select(
    app: AppHandle,
    manager: State<'_, Arc<SidecarManager>>,
    record: GatewayRecord,
) -> Result<(), String> {
    // Fail-closed scheme validation on the URL that actually takes effect.
    let origin = validate_gateway_url(&record.url)?;

    // Persist the normalized record.
    let normalized = GatewayRecord {
        url: origin.clone(),
        ..record
    };
    let next = with_selection(&read_state(), &normalized);
    // A persistence failure must not block connecting — warn and continue.
    if let Err(e) = write_state(&next) {
        tracing::warn!("failed to persist desktop.json: {e}");
    }

    // Release the preheated local sidecar when the user goes remote.
    if !is_local_url(&origin) {
        tracing::info!("remote gateway selected — releasing local sidecar");
        manager.stop();
    }

    // Navigate the main window to the chosen gateway (fresh page load from its
    // own served dashboard). Debug + release take the same path.
    let parsed = origin.parse().map_err(|e| format!("navigate parse: {e}"))?;
    let win = app
        .get_webview_window("main")
        .ok_or_else(|| "main window not found".to_string())?;
    win.navigate(parsed).map_err(|e| format!("navigate: {e}"))?;
    let _ = win.show();
    let _ = win.set_focus();
    Ok(())
}

/// Read the persisted selection for the auto-connect countdown.
#[tauri::command]
pub fn gateway_last() -> Result<DesktopGatewayState, String> {
    Ok(read_state())
}

/// The local sidecar's live status for the "本機" card (polled by the picker).
#[tauri::command]
pub fn gateway_local_status(manager: State<'_, Arc<SidecarManager>>) -> LocalStatus {
    let status = match manager.status() {
        SidecarStatus::Running => "running",
        SidecarStatus::Stopped => "stopped",
        SidecarStatus::Error => "error",
    };
    let port = manager.port();
    LocalStatus {
        status: status.to_string(),
        port,
        url: format!("http://{}:{}/", lifecycle::DEFAULT_HOST, port),
    }
}

/// Ensure the local sidecar is (re)started — used when the user explicitly picks
/// "本機" after a remote selection previously stopped it.
#[tauri::command]
pub fn gateway_start_local(
    app: AppHandle,
    manager: State<'_, Arc<SidecarManager>>,
) -> Result<u16, String> {
    manager.start(&app)
}

/// The bundled picker URL captured at startup. Once the main window navigates to
/// a chosen gateway's own dashboard, its live URL is that remote origin — so we
/// stash the original bundled URL here to navigate *back* to the picker for the
/// "切換 Gateway" entry. `None` if it couldn't be read at startup.
pub struct PickerHome(pub std::sync::Mutex<Option<url::Url>>);

/// Navigate the main window back to the bundled Gateway picker, tagged with
/// `switch=1` so the page shows the picker instead of silently auto-connecting
/// the remembered gateway (design §2.5). Invoked by the tray "切換 Gateway" item
/// and the [`gateway_open_picker`] command.
pub fn open_picker(app: &AppHandle) -> Result<(), String> {
    let mut url = app
        .try_state::<PickerHome>()
        .and_then(|s| s.0.lock().ok().and_then(|g| g.clone()))
        .ok_or_else(|| "picker home URL unknown".to_string())?;
    // Mark the switch intent (robust across hash/path routing — the page just
    // looks for the marker anywhere in `location.href`).
    url.set_query(Some("switch=1"));
    let win = app
        .get_webview_window("main")
        .ok_or_else(|| "main window not found".to_string())?;
    win.navigate(url).map_err(|e| format!("navigate: {e}"))?;
    let _ = win.show();
    let _ = win.set_focus();
    Ok(())
}

/// Reopen the Gateway picker to switch gateways (tray / user-menu entry).
#[tauri::command]
pub fn gateway_open_picker(app: AppHandle) -> Result<(), String> {
    open_picker(&app)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(url: &str) -> GatewayRecord {
        GatewayRecord {
            name: "GW".into(),
            host: "h".into(),
            port: 18789,
            version: "1.45.0".into(),
            tls: false,
            url: url.into(),
        }
    }

    #[test]
    fn scheme_allowlist_accepts_http_and_https() {
        assert_eq!(
            validate_gateway_url("http://192.168.1.5:18789").unwrap(),
            "http://192.168.1.5:18789/"
        );
        assert_eq!(
            validate_gateway_url("https://gw.example.com").unwrap(),
            "https://gw.example.com/"
        );
        // Strips path/query/fragment down to a clean origin.
        assert_eq!(
            validate_gateway_url("http://h:1/login?x=1#frag").unwrap(),
            "http://h:1/"
        );
    }

    #[test]
    fn scheme_allowlist_rejects_dangerous_schemes_fail_closed() {
        for bad in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "data:text/html,<script>",
            "ftp://host/x",
            "vbscript:msgbox",
            "not a url",
            "",
            "   ",
        ] {
            assert!(validate_gateway_url(bad).is_err(), "should reject: {bad:?}");
        }
    }

    #[test]
    fn recent_dedupes_and_truncates_to_five() {
        let mut state = DesktopGatewayState::default();
        // Insert 7 distinct gateways; only the newest 5 survive, newest first.
        for i in 0..7 {
            let r = rec(&format!("http://h{i}:18789/"));
            state = with_selection(&state, &r);
        }
        assert_eq!(state.recent.len(), RECENT_MAX);
        assert_eq!(state.recent[0].url, "http://h6:18789/");
        assert_eq!(state.recent[4].url, "http://h2:18789/");
        assert_eq!(state.last_gateway.as_ref().unwrap().url, "http://h6:18789/");
    }

    #[test]
    fn recent_moves_existing_to_front_without_duplicate() {
        let mut state = DesktopGatewayState::default();
        state = with_selection(&state, &rec("http://a:1/"));
        state = with_selection(&state, &rec("http://b:1/"));
        // Re-selecting `a` (with a trailing-slash variance) must not duplicate.
        // The freshly chosen record wins the front slot verbatim; in the real
        // flow `gateway_select` normalizes URLs before calling with_selection,
        // so variants only differ in this direct-call unit test.
        state = with_selection(&state, &rec("http://a:1"));
        assert_eq!(state.recent.len(), 2);
        assert_eq!(state.recent[0].url, "http://a:1");
        assert_eq!(state.recent[1].url, "http://b:1/");
    }

    #[test]
    fn state_roundtrips_through_disk() {
        let dir = std::env::temp_dir().join(format!("ddc-gw-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("desktop.json");
        let state = with_selection(&DesktopGatewayState::default(), &rec("http://x:9/"));
        write_state_to(&path, &state).unwrap();
        let back = read_state_from(&path);
        assert_eq!(back, state);
        // Missing file → default empty (fail-safe).
        let missing = read_state_from(&dir.join("nope.json"));
        assert_eq!(missing, DesktopGatewayState::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn is_local_url_detects_loopback() {
        assert!(is_local_url("http://127.0.0.1:18789/"));
        assert!(is_local_url("http://localhost:18789/"));
        assert!(is_local_url("http://[::1]:18789/"));
        assert!(!is_local_url("http://192.168.1.5:18789/"));
        assert!(!is_local_url("https://gw.example.com/"));
    }

    /// mDNS TXT → record mapping, mocked. Mirrors the gateway-side spec.
    #[test]
    fn record_from_resolved_maps_txt_and_ip() {
        let txt = |k: &str| match k {
            "version" => Some("1.45.0".to_string()),
            "name" => Some("Office GW".to_string()),
            "tls" => Some("0".to_string()),
            _ => None,
        };
        let r = record_from_resolved(txt, "office.local.", 18789, Some("192.168.1.20"));
        assert_eq!(r.name, "Office GW");
        assert_eq!(r.host, "office");
        assert_eq!(r.url, "http://192.168.1.20:18789/");
        assert!(!r.tls);
        assert_eq!(r.version, "1.45.0");
    }

    #[test]
    fn record_from_resolved_tls_uses_https_and_name_fallback() {
        let txt = |k: &str| {
            if k == "tls" {
                Some("1".to_string())
            } else {
                None
            }
        };
        let r = record_from_resolved(txt, "gw.local.", 443, Some("10.0.0.9"));
        assert!(r.tls);
        assert_eq!(r.url, "https://10.0.0.9:443/");
        assert_eq!(r.name, "gw"); // no name TXT → host display
    }
}
