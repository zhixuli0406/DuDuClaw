//! Runtime display identity from the local gateway's verified public brand.
//! Installer, Dock identity and application icons remain build-time assets.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, RwLock};
use std::time::Duration;

use tauri::menu::MenuItem;
use tauri::{AppHandle, Manager};

const ORIGINAL_NAME: &str = "DuDuClaw";
const MAX_RESPONSE_BYTES: usize = 400 * 1024;

pub struct RuntimeBrand {
    fetched: AtomicBool,
    name: RwLock<String>,
    pub open_menu: Mutex<Option<MenuItem<tauri::Wry>>>,
}

impl Default for RuntimeBrand {
    fn default() -> Self {
        Self {
            fetched: AtomicBool::new(false),
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

/// Called after a local gateway becomes ready. There is exactly one request
/// per desktop launch; errors leave the original identity in place.
pub fn load_once(app: &AppHandle, port: u16) {
    let Some(state) = app.try_state::<RuntimeBrand>() else {
        return;
    };
    if state.fetched.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let Some(name) = fetch_name(port).await else {
            return;
        };
        let Some(state) = app.try_state::<RuntimeBrand>() else {
            return;
        };
        let Ok(mut current) = state.name.write() else {
            return;
        };
        *current = name.clone();
        drop(current);
        apply(&app);
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

async fn fetch_name(port: u16) -> Option<String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
        .ok()?;
    let mut response = client
        .get(format!("http://127.0.0.1:{port}/api/brand"))
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?;
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
    response_name(&payload)
}

fn response_name(payload: &serde_json::Value) -> Option<String> {
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
    use super::response_name;
    use serde_json::json;

    #[test]
    fn public_identity_is_read_without_license_metadata() {
        assert_eq!(response_name(&json!({"product_name": "經銷品牌"})), Some("經銷品牌".into()));
        assert_eq!(response_name(&json!(null)), None);
        assert_eq!(response_name(&json!({"brand": {"product_name": "nested"}})), None);
    }

    #[test]
    fn invalid_names_fail_closed() {
        for name in ["".to_string(), "x".repeat(41), "bad\nname".into(), " ".into(), "bad\u{2028}name".into(), "mailto:a@b.com".into(), "<b>".into(), "https://example.com".into()] {
            assert_eq!(response_name(&json!({"product_name": name})), None);
        }
        assert!(response_name(&json!({"product_name": "嘟".repeat(40)})).is_some());
    }
}
