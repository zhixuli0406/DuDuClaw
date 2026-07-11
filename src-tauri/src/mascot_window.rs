//! DuDu desktop-pet second window (§7.4).
//!
//! A small, transparent, borderless, always-on-top window that loads the
//! `/mascot-overlay` mini route (the same web app, no shell). It shares the web
//! DuDu's SVG + face engine — this module only owns the *native window*: create
//! it hidden, and let the tray toggle its visibility. Dragging is driven from
//! the frontend via `data-tauri-drag-region`.

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

/// Window label for the desktop pet. Must match `capabilities/default.json`.
pub const MASCOT_LABEL: &str = "mascot";

/// Build the desktop-pet window, hidden by default. No-op if it already exists
/// so callers can build lazily without tracking state.
pub fn build_mascot_window(app: &AppHandle) -> tauri::Result<()> {
    if app.get_webview_window(MASCOT_LABEL).is_some() {
        return Ok(());
    }
    WebviewWindowBuilder::new(app, MASCOT_LABEL, WebviewUrl::App("mascot-overlay".into()))
        .title("DuDu")
        .inner_size(180.0, 180.0)
        .transparent(true)
        .decorations(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .visible(false)
        .build()?;
    Ok(())
}

/// Show/hide the desktop pet — the tray toggle target. Builds it lazily if it
/// was never created. Returns the new visibility (`true` = now shown).
pub fn toggle_mascot_window(app: &AppHandle) -> tauri::Result<bool> {
    if app.get_webview_window(MASCOT_LABEL).is_none() {
        build_mascot_window(app)?;
    }
    let Some(win) = app.get_webview_window(MASCOT_LABEL) else {
        return Ok(false);
    };
    if win.is_visible().unwrap_or(false) {
        let _ = win.hide();
        Ok(false)
    } else {
        let _ = win.show();
        let _ = win.set_focus();
        Ok(true)
    }
}
