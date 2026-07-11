// Hide the extra console window on Windows in release.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! DuDuClaw desktop shell entry point (TODO-genspark-workspace-shell §D0–§D2).
//!
//! Boots a single-instance Tauri app that owns the gateway sidecar and presents
//! the embedded dashboard in a native window with a tray. NOT compiled in this
//! environment (no Tauri toolchain) — see Phase D verification notes.

mod lifecycle;
mod mascot_window;
mod sidecar;

use std::sync::Arc;
use std::time::Duration;

use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::TrayIconBuilder,
    AppHandle, Manager, RunEvent, WindowEvent,
};

use sidecar::{SidecarManager, SidecarStatus};

/// Show/focus the main dashboard window. Invoked by the desktop pet
/// (`/mascot-overlay` → `window.__TAURI__.core.invoke('open_main_window')`).
#[tauri::command]
fn open_main_window(app: AppHandle) {
    show_main_window(&app);
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let manager = Arc::new(SidecarManager::new());

    tauri::Builder::default()
        // Single instance: a second launch focuses the existing window (§D2.1).
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main_window(app);
        }))
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .manage(manager.clone())
        .invoke_handler(tauri::generate_handler![open_main_window])
        .setup(move |app| {
            let handle = app.handle().clone();

            // Create the desktop-pet window up front (hidden); the tray toggles
            // its visibility. Non-fatal if it fails (e.g. transparency
            // unsupported) — the main app is unaffected.
            if let Err(e) = mascot_window::build_mascot_window(&handle) {
                tracing::warn!("desktop-pet window init failed: {e}");
            }

            // Start (or attach to) the gateway, then point the window at it.
            let port = manager
                .start(&handle)
                .unwrap_or_else(|e| {
                    tracing::error!("gateway start failed: {e}");
                    lifecycle::configured_port()
                });

            // Wait briefly for readiness, then navigate + reveal the window so
            // there is no white flash before the dashboard can load (§D0).
            let mgr = manager.clone();
            let handle2 = handle.clone();
            tauri::async_runtime::spawn(async move {
                let ready = tauri::async_runtime::spawn_blocking(move || {
                    mgr.wait_until_ready(Duration::from_secs(20))
                })
                .await
                .unwrap_or(false);
                if !ready {
                    tracing::warn!("gateway did not become ready in time");
                }
                if let Some(win) = handle2.get_webview_window("main") {
                    // Debug (`cargo tauri dev`): the window already loaded the
                    // Vite dev server (devUrl :5173), which serves the live web
                    // app with HMR and proxies /ws + /api to the gateway. Stay on
                    // it so web edits hot-reload — do NOT navigate to the gateway's
                    // embedded (compile-time) dist, which would mask local changes.
                    #[cfg(debug_assertions)]
                    {
                        tracing::info!(
                            "dev: webview stays on Vite devUrl; gateway/backend on port {port}"
                        );
                    }
                    // Release: the bundled webview must be pointed at the gateway's
                    // embedded dashboard served over loopback.
                    #[cfg(not(debug_assertions))]
                    {
                        let url = format!("http://{}:{}/", lifecycle::DEFAULT_HOST, port);
                        if let Ok(parsed) = url.parse() {
                            let _ = win.navigate(parsed);
                        }
                    }
                    let _ = win.show();
                    let _ = win.set_focus();
                }
            });

            build_tray(app.handle(), &manager)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            // Close-to-tray: hide instead of quitting (§D2.4). Real quit goes
            // through the tray menu / RunEvent::ExitRequested.
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building DuDuClaw desktop")
        .run(move |app, event| {
            if let RunEvent::ExitRequested { .. } = event {
                // Graceful sidecar shutdown on quit (§D2.3).
                if let Some(mgr) = app.try_state::<Arc<SidecarManager>>() {
                    mgr.stop();
                }
            }
        });
}

fn show_main_window(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
    }
}

fn build_tray(app: &AppHandle, manager: &Arc<SidecarManager>) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "開啟 DuDuClaw", true, None::<&str>)?;
    let mascot = MenuItem::with_id(app, "toggle_mascot", "顯示/隱藏桌寵", true, None::<&str>)?;
    let restart = MenuItem::with_id(app, "restart", "重啟背景服務", true, None::<&str>)?;
    let status = MenuItem::with_id(app, "status", tray_status_label(manager), false, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "quit", "結束", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &mascot, &status, &restart, &sep, &quit])?;

    let manager_for_menu = manager.clone();
    TrayIconBuilder::with_id("main-tray")
        .icon(app.default_window_icon().unwrap().clone())
        .tooltip("DuDuClaw")
        .menu(&menu)
        .on_menu_event(move |app, event| match event.id.as_ref() {
            "open" => show_main_window(app),
            "toggle_mascot" => {
                if let Err(e) = mascot_window::toggle_mascot_window(app) {
                    tracing::warn!("toggle desktop pet failed: {e}");
                }
            }
            "restart" => {
                manager_for_menu.stop();
                let _ = manager_for_menu.start(app);
            }
            "quit" => {
                manager_for_menu.stop();
                app.exit(0);
            }
            _ => {}
        })
        .build(app)?;
    Ok(())
}

fn tray_status_label(manager: &Arc<SidecarManager>) -> String {
    match manager.status() {
        SidecarStatus::Running => format!("● 運行中 (port {})", manager.port()),
        SidecarStatus::Stopped => "○ 已停止".to_string(),
        SidecarStatus::Error => "▲ 異常".to_string(),
    }
}
