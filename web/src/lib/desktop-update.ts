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

/** Shell ↔ gateway relation reported by `desktop_update_info`. */
export type DesktopGatewayRelation = 'bundled' | 'external';

export interface DesktopUpdateInfo {
  /** Running shell version; empty on an older shell without the command. */
  currentVersion: string;
  stagedVersion: string | null;
  /**
   * `bundled` = the shell runs its own gateway sidecar; `external` = attached
   * to a gateway it does not own. Null on an older shell (relation unknown).
   */
  gateway: DesktopGatewayRelation | null;
}

/**
 * Shell version, staged update and gateway relation, or null outside the
 * desktop shell. An older shell without `desktop_update_info` falls back to
 * the staged version alone.
 */
export async function desktopUpdateInfo(): Promise<DesktopUpdateInfo | null> {
  const invoke = tauriInvoke();
  if (!invoke) return null;
  try {
    const raw = (await invoke('desktop_update_info')) as {
      current_version?: unknown;
      staged_version?: unknown;
      gateway?: unknown;
    } | null;
    const gateway = raw?.gateway === 'bundled' || raw?.gateway === 'external' ? raw.gateway : null;
    return {
      currentVersion: typeof raw?.current_version === 'string' ? raw.current_version : '',
      stagedVersion:
        typeof raw?.staged_version === 'string' && raw.staged_version ? raw.staged_version : null,
      gateway,
    };
  } catch {
    return { currentVersion: '', stagedVersion: await stagedDesktopUpdateVersion(), gateway: null };
  }
}
