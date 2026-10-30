//! Runtime display identity from the local gateway's verified public brand.
//! Installer, Dock identity and application icons remain build-time assets.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use tauri::menu::MenuItem;
use tauri::{AppHandle, Manager};

const ORIGINAL_NAME: &str = "DuDuClaw";
const MAX_RESPONSE_BYTES: usize = 400 * 1024;
const REFRESH_INTERVAL: Duration = Duration::from_secs(30 * 60);

pub struct RuntimeBrand {
    fetched: AtomicBool,
    target: Mutex<PollTarget>,
    wake: Arc<tokio::sync::Notify>,
    name: RwLock<String>,
    pub open_menu: Mutex<Option<MenuItem<tauri::Wry>>>,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct PollTarget {
    port: u16,
    generation: u64,
}

impl PollTarget {
    fn ready(&mut self, port: u16) {
        self.port = port;
        // Replacing a gateway on the same port also invalidates in-flight data.
        self.generation = self.generation.wrapping_add(1);
    }
}

impl Default for RuntimeBrand {
    fn default() -> Self {
        Self {
            fetched: AtomicBool::new(false),
            target: Mutex::new(PollTarget::default()),
            wake: Arc::new(tokio::sync::Notify::new()),
            name: RwLock::new(ORIGINAL_NAME.to_string()),
            open_menu: Mutex::new(None),
        }
    }
}

pub fn product_name(app: &AppHandle) -> String {
    app.try_state::<RuntimeBrand>()
        .and_then(|state| state.name.read().ok().map(|name| name.clone()))
        .unwrap_or_else(|| ORIGINAL_NAME.to_string())
}

/// Start one polling loop after the local gateway becomes ready. Transient
/// failures preserve the last identity; an explicit null restores the original.
/// Later readiness calls retarget and wake that same loop.
pub fn load_once(app: &AppHandle, port: u16) {
    let Some(state) = app.try_state::<RuntimeBrand>() else {
        return;
    };
    let Ok(mut target) = state.target.lock() else {
        return;
    };
    target.ready(port);
    drop(target);
    if state.fetched.swap(true, Ordering::SeqCst) {
        state.wake.notify_one();
        return;
    }
    let wake = Arc::clone(&state.wake);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut etag: Option<String> = None;
        let mut previous_target = None;
        loop {
            let target = app.try_state::<RuntimeBrand>()
                .and_then(|state| {
                    let target = state.target.lock().ok()?;
                    Some(*target)
                });
            let Some(target) = target else {
                return;
            };
            if previous_target != Some(target) {
                etag = None;
                previous_target = Some(target);
            }
            if let Some((name, next_etag)) = fetch_name(target.port, etag.as_deref()).await {
                if let Some(state) = app.try_state::<RuntimeBrand>() {
                    // Serialize the comparison and write with readiness updates.
                    // A response from an old port (or generation) cannot land.
                    if let Ok(current_target) = state.target.lock() {
                        if *current_target == target {
                            if let Ok(mut current) = state.name.write() {
                                *current = name;
                                etag = next_etag;
                            }
                        }
                    };
                }
                apply(&app);
            }
            // Notify retains a readiness signal received during the request.
            let _ = tokio::time::timeout(REFRESH_INTERVAL, wake.notified()).await;
        }
    });
}

/// Also called after tray construction, in case the local fetch won the race.
pub fn apply(app: &AppHandle) {
    let name = product_name(app);
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.set_title(&name);
    }
    if let Some(tray) = app.tray_by_id("main-tray") {
        let _ = tray.set_tooltip(Some(name.clone()));
    }
    if let Some(state) = app.try_state::<RuntimeBrand>() {
        if let Ok(menu) = state.open_menu.lock() {
            if let Some(item) = menu.as_ref() {
                let _ = item.set_text(format!("開啟 {name}"));
            }
        };
    }
}

async fn fetch_name(port: u16, etag: Option<&str>) -> Option<(String, Option<String>)> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
        .ok()?;
    let mut request = client
        .get(format!("http://127.0.0.1:{port}/api/brand"))
        .header(reqwest::header::CACHE_CONTROL, "no-cache");
    if let Some(etag) = etag {
        request = request.header(reqwest::header::IF_NONE_MATCH, etag);
    }
    let mut response = request.send()
        .await
        .ok()?
        .error_for_status()
        .ok()?;
    if response.status() == reqwest::StatusCode::NOT_MODIFIED {
        return None;
    }
    let etag = response.headers().get(reqwest::header::ETAG)
        .and_then(|value| value.to_str().ok()).map(str::to_owned);
    if response.content_length().is_some_and(|len| len > MAX_RESPONSE_BYTES as u64) {
        return None;
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.ok()? {
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return None;
        }
        bytes.extend_from_slice(&chunk);
    }
    let payload: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    Some((response_name(&payload)?, etag))
}

fn response_name(payload: &serde_json::Value) -> Option<String> {
    if payload.is_null() {
        return Some(ORIGINAL_NAME.to_string());
    }
    let name = payload.get("product_name")?.as_str()?;
    let lower = name.to_ascii_lowercase();
    if !(1..=40).contains(&name.chars().count())
        || name.trim().is_empty()
        || name.chars().any(|c| c.is_control() || matches!(c, '<' | '>' | '\u{2028}' | '\u{2029}'))
        || name.split_whitespace().any(|part| {
            part.split_once(':').is_some_and(|(scheme, _)| {
                scheme.starts_with(|c: char| c.is_ascii_alphabetic())
                    && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
            })
        })
        || lower.contains("://")
        || lower.contains("www.")
    {
        return None;
    }
    Some(name.to_string())
}

#[cfg(test)]
mod tests {
    use super::{response_name, PollTarget};
    use serde_json::json;

    #[test]
    fn public_identity_is_read_without_license_metadata() {
        assert_eq!(response_name(&json!({"product_name": "經銷品牌"})), Some("經銷品牌".into()));
        assert_eq!(response_name(&json!(null)), Some("DuDuClaw".into()));
        assert_eq!(response_name(&json!({"brand": {"product_name": "nested"}})), None);
    }

    #[test]
    fn invalid_names_fail_closed() {
        for name in ["".to_string(), "x".repeat(41), "bad\nname".into(), " ".into(), "bad\u{2028}name".into(), "mailto:a@b.com".into(), "<b>".into(), "https://example.com".into()] {
            assert_eq!(response_name(&json!({"product_name": name})), None);
        }
        assert!(response_name(&json!({"product_name": "嘟".repeat(40)})).is_some());
    }

    #[test]
    fn gateway_ready_invalidates_old_port_and_same_port_requests() {
        let mut target = PollTarget::default();
        target.ready(18789);
        let first = target;
        target.ready(18790);
        assert_eq!(target.port, 18790);
        assert!(target != first);
        let second = target;
        target.ready(18790);
        assert!(target != second);
        target.ready(first.port);
        assert!(target != first);
    }
}
