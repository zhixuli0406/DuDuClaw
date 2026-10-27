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
//!
//! When the shell is attached to a gateway it does not own (an existing
//! launchd/systemd/Pro gateway, or a remote one), the app and the gateway are
//! separate installs on separate update channels. Steps 3 and 4 then defer to
//! the dashboard 系統更新 page, which updates the gateway first and the app
//! only up to the gateway's version — the tray used to say「重啟並更新」for
//! an app-only update while the gateway stayed behind (2026-10-27 report:
//! app 1.76.1, gateway 1.70.1, one page claiming both were current).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tauri::menu::MenuItem;
use tauri::{AppHandle, Manager};
use tauri_plugin_notification::NotificationExt;
use tauri_plugin_updater::UpdaterExt;

use crate::sidecar::SidecarManager;

/// How this shell relates to the gateway it shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum GatewayRelation {
    /// The shell spawned and owns the gateway sidecar — one install, one
    /// update (the sidecar ships inside the app bundle).
    Bundled,
    /// Attached to a gateway the shell does not own; that gateway updates
    /// through its own channel.
    External,
}

impl GatewayRelation {
    pub fn from_owns_sidecar(owns_sidecar: bool) -> Self {
        if owns_sidecar {
            Self::Bundled
        } else {
            Self::External
        }
    }

    /// Whether a staged app update may install without the 系統更新 page —
    /// on quit or from the tray. Only when the app also carries the gateway.
    pub fn installs_unattended(self) -> bool {
        self == Self::Bundled
    }

    fn tray_label(self, version: &str) -> String {
        match self {
            Self::Bundled => format!("🔄 重啟並更新到 v{version}"),
            Self::External => format!("🔄 有新版本 v{version}：到「系統更新」頁更新"),
        }
    }

    fn ready_notice(self, version: &str) -> String {
        match self {
            Self::Bundled => format!(
                "v{version} 已在背景下載完成。從系統匣選「重啟並更新」立即套用，或下次重啟時自動生效。"
            ),
            Self::External => format!(
                "App v{version} 已下載。請到「系統更新」頁，和 gateway 依序一起更新。"
            ),
        }
    }
}

pub fn gateway_relation(app: &AppHandle) -> GatewayRelation {
    let owns = app
        .try_state::<Arc<SidecarManager>>()
        .is_some_and(|mgr| mgr.owns_gateway());
    GatewayRelation::from_owns_sidecar(owns)
}

/// Bring the main window to the dashboard 系統更新 page. Falls back to just
/// showing the window while it is still on the bundled gateway picker.
pub fn open_updates_page(app: &AppHandle) {
    let Some(win) = app.get_webview_window("main") else {
        return;
    };
    let picker_origin = app
        .try_state::<crate::gateway_picker::PickerHome>()
        .and_then(|home| home.0.lock().unwrap().as_ref().map(|u| u.origin()));
    if let Ok(mut url) = win.url() {
        let on_picker = picker_origin.as_ref() == Some(&url.origin());
        if !on_picker && matches!(url.scheme(), "http" | "https") {
            url.set_path(UPDATES_PAGE_PATH);
            url.set_query(None);
            url.set_fragment(None);
            if let Err(e) = win.navigate(url) {
                tracing::warn!("open updates page failed: {e}");
            }
        }
    }
    let _ = win.show();
    let _ = win.unminimize();
    let _ = win.set_focus();
}

/// Dashboard route of the 系統更新 page (`web/src/App.tsx`).
const UPDATES_PAGE_PATH: &str = "/app/system/updates";

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

    // Flip the tray item into an actionable affordance — install directly
    // when the app carries the gateway, otherwise point at the 系統更新 page.
    let relation = gateway_relation(app);
    if let Some(slot) = app.try_state::<UpdateMenuItem>() {
        let guard = slot.0.lock().unwrap();
        if let Some(item) = guard.as_ref() {
            let _ = item.set_text(relation.tray_label(&version));
        }
    }

    let _ = app
        .notification()
        .builder()
        .title("DuDuClaw 更新已就緒")
        .body(relation.ready_notice(&version))
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

/// Quit path: install the staged update only when the app also carries the
/// gateway. Attached to an external gateway, an unattended install would put
/// the app ahead of the gateway again; the 系統更新 page decides instead.
pub fn install_pending_on_quit(app: &AppHandle, relation: GatewayRelation) -> bool {
    if !relation.installs_unattended() {
        return false;
    }
    install_pending(app)
}

/// What the 系統更新 page needs to order the app update after the gateway's.
#[derive(serde::Serialize)]
pub struct DesktopUpdateInfo {
    pub current_version: String,
    pub staged_version: Option<String>,
    pub gateway: GatewayRelation,
}

#[tauri::command]
pub fn desktop_update_info(app: AppHandle) -> DesktopUpdateInfo {
    DesktopUpdateInfo {
        current_version: app.package_info().version.to_string(),
        staged_version: desktop_update_status(app.clone()),
        gateway: gateway_relation(&app),
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
    // Attached to an external gateway, the 系統更新 page owns the order
    // (gateway first, app up to the gateway's version).
    if !gateway_relation(app).installs_unattended() {
        open_updates_page(app);
        return;
    }
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

#[cfg(test)]
mod tests {
    use super::GatewayRelation;

    #[test]
    fn only_a_bundled_sidecar_installs_unattended() {
        assert!(GatewayRelation::from_owns_sidecar(true).installs_unattended());
        assert!(!GatewayRelation::from_owns_sidecar(false).installs_unattended());
    }

    #[test]
    fn external_gateway_points_at_the_updates_page() {
        let ext = GatewayRelation::External;
        assert!(ext.tray_label("1.76.2").contains("系統更新"));
        assert!(ext.ready_notice("1.76.2").contains("系統更新"));
        assert!(!ext.ready_notice("1.76.2").contains("系統匣"));
        assert!(GatewayRelation::Bundled.tray_label("1.76.2").contains("重啟並更新"));
    }

    #[test]
    fn relation_serializes_lowercase_for_the_dashboard() {
        assert_eq!(
            serde_json::to_string(&GatewayRelation::External).unwrap(),
            "\"external\""
        );
        assert_eq!(
            serde_json::to_string(&GatewayRelation::Bundled).unwrap(),
            "\"bundled\""
        );
    }
}
