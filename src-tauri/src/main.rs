// Hide the extra console window on Windows in release.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! DuDuClaw desktop shell entry point (TODO-genspark-workspace-shell §D0–§D2).
//!
//! Boots a single-instance Tauri app that owns the gateway sidecar and presents
//! the embedded dashboard in a native window with a tray. NOT compiled in this
//! environment (no Tauri toolchain) — see Phase D verification notes.

mod gateway_picker;
mod lifecycle;
mod mascot_window;
mod pet_gen;
mod sidecar;
mod updater;

use std::sync::Arc;

use tauri::{
    menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem},
    tray::TrayIconBuilder,
    AppHandle, Manager, RunEvent, WindowEvent,
};
use tauri_plugin_autostart::ManagerExt;

use sidecar::{SidecarManager, SidecarStatus};

/// Show/focus the main dashboard window. Invoked by the desktop pet
/// (`/mascot-overlay` → `window.__TAURI__.core.invoke('open_main_window')`).
#[tauri::command]
fn open_main_window(app: AppHandle) {
    show_main_window(&app);
}

/// Open an http(s) URL in the system default browser. The dashboard (a remote
/// gateway origin) invokes this for OAuth consent pages and outbound links —
/// `window.open` / `target="_blank"` are silent no-ops in the wry webview, so
/// every "open in browser" affordance funnels through here. Fail-closed: the
/// URL must parse and carry an http/https scheme before it reaches the OS
/// opener, so remote pages can never launch `file://` or custom-scheme
/// handlers through the shell.
#[tauri::command]
fn open_external_url(app: AppHandle, url: String) -> Result<(), String> {
    let parsed = url::Url::parse(&url).map_err(|e| format!("invalid url: {e}"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(format!(
            "refusing to open non-http(s) scheme: {}",
            parsed.scheme()
        ));
    }
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .open_url(parsed.as_str(), None::<&str>)
        .map_err(|e| e.to_string())
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let manager = Arc::new(SidecarManager::new());

    tauri::Builder::default()
        // Single instance: a second launch focuses the existing window (§D2.1).
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main_window(app);
        }))
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .manage(manager.clone())
        .invoke_handler(tauri::generate_handler![
            open_main_window,
            open_external_url,
            pet_gen::pet_list,
            pet_gen::pet_generate,
            pet_gen::pet_active_get,
            pet_gen::pet_activate,
            pet_gen::pet_remove,
            pet_gen::pet_load_active,
            pet_gen::pet_model_status,
            pet_gen::pet_model_download,
            pet_gen::pet_context_menu,
            pet_gen::pet_move_by,
            pet_gen::pet_set_scale,
            pet_gen::pet_get_scale,
            pet_gen::pet_open_studio,
            gateway_picker::gateway_discover,
            gateway_picker::gateway_health,
            gateway_picker::gateway_select,
            gateway_picker::gateway_last,
            gateway_picker::gateway_local_status,
            gateway_picker::gateway_start_local,
            gateway_picker::gateway_open_picker,
        ])
        .setup(move |app| {
            let handle = app.handle().clone();

            // Hot-update (§D4): stage new versions in the background, apply on
            // restart/quit. Without this the updater plugin is inert and the
            // shell silently freezes on whatever version was installed.
            app.manage(updater::PendingUpdate::default());
            app.manage(updater::UpdateMenuItem(std::sync::Mutex::new(None)));
            updater::spawn_update_watcher(handle.clone());

            // Create the desktop-pet window up front (hidden); the tray toggles
            // its visibility. Non-fatal if it fails (e.g. transparency
            // unsupported) — the main app is unaffected.
            if let Err(e) = mascot_window::build_mascot_window(&handle) {
                tracing::warn!("desktop-pet window init failed: {e}");
            }

            // WP-GW: the main window boots on the bundled Gateway picker
            // (`/gateway-picker`, set via tauri.conf window url) in BOTH debug
            // and release — no more `#[cfg]` divergence where debug stalled on
            // `tauri://localhost` with a port-less `location.host`. The picker
            // itself triggers navigation via `gateway_select`. Reveal the window
            // right away; the picker does not wait for the sidecar.
            let picker_home = if let Some(win) = handle.get_webview_window("main") {
                let _ = win.show();
                let _ = win.set_focus();
                win.url().ok()
            } else {
                None
            };
            // Stash the bundled picker URL so the tray "切換 Gateway" item can
            // navigate back to it even after the window loads a remote gateway.
            app.manage(gateway_picker::PickerHome(std::sync::Mutex::new(
                picker_home,
            )));

            // Preheat the local sidecar in the background so the "本機" card is
            // ready quickly, WITHOUT blocking the picker (design §3). If the user
            // ends up choosing a remote gateway, `gateway_select` releases it.
            let mgr = manager.clone();
            let handle2 = handle.clone();
            tauri::async_runtime::spawn_blocking(move || {
                if let Err(e) = mgr.start(&handle2) {
                    tracing::error!("local sidecar preheat failed: {e}");
                }
            });

            build_tray(app.handle(), &manager)?;
            Ok(())
        })
        // Route right-click desktop-pet menu clicks. The tray uses its own
        // `on_menu_event`; this app-level handler only acts on `pet_*` ids
        // (returns false otherwise) so the two never conflict.
        .on_menu_event(|app, event| {
            pet_gen::handle_pet_menu_event(app, event.id().as_ref());
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
                // Apply any staged update on the way out so the next manual
                // launch runs the new version (Chrome-style hot update).
                updater::install_pending(app);
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
    let switch_gw = MenuItem::with_id(app, "switch_gateway", "切換 Gateway", true, None::<&str>)?;
    let mascot = MenuItem::with_id(app, "toggle_mascot", "顯示/隱藏桌寵", true, None::<&str>)?;
    let restart = MenuItem::with_id(app, "restart", "重啟背景服務", true, None::<&str>)?;
    // Login-item registration for the desktop shell itself (the gateway rides
    // along as its sidecar). Backed by tauri-plugin-autostart — LaunchAgent on
    // macOS, per-user registry Run key on Windows, XDG autostart on Linux.
    let autostart_on = app.autolaunch().is_enabled().unwrap_or(false);
    let autostart = CheckMenuItem::with_id(
        app,
        "autostart",
        "開機自動啟動",
        true,
        autostart_on,
        None::<&str>,
    )?;
    let status = MenuItem::with_id(
        app,
        "status",
        tray_status_label(manager),
        false,
        None::<&str>,
    )?;
    // Hot-update entry: the background watcher retitles this to
    // 「🔄 重啟並更新到 vX.Y.Z」once a new version is staged.
    let check_update = MenuItem::with_id(app, "check_update", "檢查更新", true, None::<&str>)?;
    if let Some(slot) = app.try_state::<updater::UpdateMenuItem>() {
        *slot.0.lock().unwrap() = Some(check_update.clone());
    }
    let sep = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "quit", "結束", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[&open, &switch_gw, &mascot, &autostart, &status, &restart, &check_update, &sep, &quit],
    )?;

    let manager_for_menu = manager.clone();
    let autostart_item = autostart.clone();
    TrayIconBuilder::with_id("main-tray")
        .icon(app.default_window_icon().unwrap().clone())
        .tooltip("DuDuClaw")
        .menu(&menu)
        .on_menu_event(move |app, event| match event.id.as_ref() {
            "open" => show_main_window(app),
            "switch_gateway" => {
                if let Err(e) = gateway_picker::open_picker(app) {
                    tracing::warn!("open gateway picker failed: {e}");
                }
            }
            "toggle_mascot" => {
                if let Err(e) = mascot_window::toggle_mascot_window(app) {
                    tracing::warn!("toggle desktop pet failed: {e}");
                }
            }
            "autostart" => {
                // The check item toggles itself on click; apply the new state
                // to the OS registration and roll the checkmark back if the
                // plugin call fails so the menu never lies about reality.
                let want = autostart_item.is_checked().unwrap_or(false);
                let autolaunch = app.autolaunch();
                let result = if want {
                    autolaunch.enable()
                } else {
                    autolaunch.disable()
                };
                if let Err(e) = result {
                    tracing::warn!("autostart toggle failed: {e}");
                    let _ = autostart_item.set_checked(!want);
                }
            }
            "restart" => {
                manager_for_menu.stop();
                let _ = manager_for_menu.start(app);
            }
            "check_update" => {
                updater::on_tray_check_update(app);
            }
            "quit" => {
                manager_for_menu.stop();
                // exit(0) fires RunEvent::ExitRequested, which applies any
                // staged update (main.rs run handler).
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
        SidecarStatus::Starting => "◌ 啟動中".to_string(),
        SidecarStatus::Stopped => "○ 已停止".to_string(),
        SidecarStatus::Error => "▲ 異常".to_string(),
    }
}
