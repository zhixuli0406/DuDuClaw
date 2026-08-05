/**
 * Desktop-shell hot-update bridge.
 *
 * The Tauri shell stages new app versions in the background (updater.rs) and
 * notifies the user to pick「重啟並更新」— but that item lives in the tray
 * menu, which users cannot find from the 系統更新 settings page the
 * notification sends them to (2026-08-05 field report). These helpers expose
 * the staged state + install-and-relaunch to the dashboard so the page can
 * render the button itself. Same `withGlobalTauri` access pattern as
 * lib/external-link.ts; every call is a no-op / null outside the shell.
 */

type TauriInvoke = (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;

function tauriInvoke(): TauriInvoke | undefined {
  const w = window as unknown as { __TAURI__?: { core?: { invoke?: TauriInvoke } } };
  const fn = w.__TAURI__?.core?.invoke;
  return typeof fn === 'function' ? fn : undefined;
}

/**
 * Version of the shell update already downloaded and waiting for a restart,
 * or null — not in the desktop shell, nothing staged, or an older shell
 * without the command (invoke rejects → null, page falls back to text-only).
 */
export async function stagedDesktopUpdateVersion(): Promise<string | null> {
  const invoke = tauriInvoke();
  if (!invoke) return null;
  try {
    const version = await invoke('desktop_update_status');
    return typeof version === 'string' && version ? version : null;
  } catch {
    return null;
  }
}

/**
 * Install the staged shell update and relaunch the app. Resolving is not
 * expected — on success the webview dies mid-restart; a rejection means the
 * install failed (or the stage raced away) and should be surfaced.
 */
export async function restartAndUpdateDesktop(): Promise<void> {
  const invoke = tauriInvoke();
  if (!invoke) throw new Error('not running in the desktop shell');
  await invoke('desktop_restart_and_update');
}
