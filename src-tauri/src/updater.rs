//! Background auto-update — the Chrome/VSCode "hot update" pattern.
//!
//! Tauri v2's updater plugin is **passive**: registering it does nothing on
//! its own (the v1 `"dialog": true` config key is ignored), which is why the
//! desktop shell historically never updated itself even though the
//! `desktop-updater` feed was published on every release. This module supplies
//! the missing active half:
//!
//! 1. Check the feed shortly after launch, then every 6 hours.
//! 2. When a new version exists, **download in the background** (stage only —
//!    nothing is touched on disk yet, the user keeps working).
//! 3. Notify once, and flip the tray item to「重啟並更新」.
//! 4. Install the staged bytes either when the user clicks the tray item
//!    (then relaunch) or on normal quit — so the next launch runs the new
//!    version even if the notification was ignored.
//!
//! Every step is best-effort: a failed check/download logs a warning and
//! retries on the next tick; it can never take the app down.

use std::sync::Mutex;
use std::time::Duration;

use tauri::menu::MenuItem;
use tauri::{AppHandle, Manager};
use tauri_plugin_notification::NotificationExt;
use tauri_plugin_updater::UpdaterExt;

/// A downloaded-but-not-yet-installed update: the metadata handle plus the
/// staged installer bytes. Kept in memory (tens of MB) so "install on quit"
/// never has to re-download.
#[derive(Default)]
pub struct PendingUpdate(pub Mutex<Option<(tauri_plugin_updater::Update, Vec<u8>)>>);

/// Tray item the watcher retitles when an update is staged. Registered by
/// `build_tray` so the watcher can find it without rebuilding the menu.
pub struct UpdateMenuItem(pub Mutex<Option<MenuItem<tauri::Wry>>>);

const FIRST_CHECK_DELAY: Duration = Duration::from_secs(30);
const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 3600);

/// Spawn the background watcher. Called once from `setup`.
pub fn spawn_update_watcher(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FIRST_CHECK_DELAY).await;
        loop {
            match check_and_stage(&app).await {
                // Staged: stop polling — the downloaded artifact stays valid
                // until quit/restart applies it.
                Ok(Some(_)) => break,
                Ok(None) => {}
                Err(e) => tracing::warn!("desktop update check failed: {e}"),
            }
            tokio::time::sleep(CHECK_INTERVAL).await;
        }
    });
}

/// One check-and-stage round. `Ok(Some(version))` means an update was
/// downloaded and is now pending install.
pub async fn check_and_stage(app: &AppHandle) -> Result<Option<String>, String> {
    let updater = app.updater().map_err(|e| e.to_string())?;
    let Some(update) = updater.check().await.map_err(|e| e.to_string())? else {
        return Ok(None);
    };
    let version = update.version.clone();
    tracing::info!("desktop update v{version} available — downloading in background");

    let bytes = update
        .download(|_chunk, _total| {}, || {})
        .await
        .map_err(|e| e.to_string())?;
    tracing::info!("desktop update v{version} downloaded ({} bytes), staged for install", bytes.len());

    if let Some(state) = app.try_state::<PendingUpdate>() {
        *state.0.lock().unwrap() = Some((update, bytes));
    }

    // Flip the tray item into an actionable "restart to update" affordance.
    if let Some(slot) = app.try_state::<UpdateMenuItem>() {
        let guard = slot.0.lock().unwrap();
        if let Some(item) = guard.as_ref() {
            let _ = item.set_text(format!("🔄 重啟並更新到 v{version}"));
        }
    }

    let _ = app
        .notification()
        .builder()
        .title("DuDuClaw 更新已就緒")
        .body(format!(
            "v{version} 已在背景下載完成。從系統匣選「重啟並更新」立即套用，或下次重啟時自動生效。"
        ))
        .show();

    Ok(Some(version))
}

/// Install the staged update if one exists. Returns true when an install
/// actually happened. Called from the quit path (apply on exit) and the tray
/// "restart and update" item (apply + relaunch).
pub fn install_pending(app: &AppHandle) -> bool {
    let Some(state) = app.try_state::<PendingUpdate>() else {
        return false;
    };
    let taken = state.0.lock().unwrap().take();
    let Some((update, bytes)) = taken else {
        return false;
    };
    let version = update.version.clone();
    match update.install(bytes) {
        Ok(()) => {
            tracing::info!("desktop update v{version} installed — next launch runs it");
            true
        }
        Err(e) => {
            tracing::warn!("desktop update v{version} install failed: {e}");
            false
        }
    }
}

/// Version of the staged-but-not-installed update, if any. Invoked by the
/// dashboard 系統更新 page so the「重啟並更新」affordance exists in-page and not
/// only in the tray menu (the update-ready notification points users at a
/// button they otherwise could not find — 2026-08-05 field report).
#[tauri::command]
pub fn desktop_update_status(app: AppHandle) -> Option<String> {
    let state = app.try_state::<PendingUpdate>()?;
    let guard = state.0.lock().unwrap();
    guard.as_ref().map(|(update, _)| update.version.clone())
}

/// Install the staged update and relaunch. Errors when nothing is staged
/// (the page only shows the button after `desktop_update_status` returned a
/// version, but the stage can race with a quit-path install).
#[tauri::command]
pub fn desktop_restart_and_update(app: AppHandle) -> Result<(), String> {
    if install_pending(&app) {
        app.restart();
    }
    Err("no staged update to install".into())
}

/// Tray "檢查更新 / 重啟並更新" click handler. If an update is already staged,
/// install and relaunch; otherwise run an immediate check (with feedback even
/// when already up to date, unlike the silent background tick).
pub fn on_tray_check_update(app: &AppHandle) {
    if install_pending(app) {
        app.restart();
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        match check_and_stage(&app).await {
            Ok(Some(_)) => {} // check_and_stage already notified + retitled
            Ok(None) => {
                let _ = app
                    .notification()
                    .builder()
                    .title("DuDuClaw")
                    .body("目前已是最新版本。")
                    .show();
            }
            Err(e) => {
                tracing::warn!("manual update check failed: {e}");
                let _ = app
                    .notification()
                    .builder()
                    .title("DuDuClaw")
                    .body("檢查更新失敗，請稍後再試或到 GitHub Releases 手動下載。")
                    .show();
            }
        }
    });
}
