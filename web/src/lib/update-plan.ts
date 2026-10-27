/**
 * One update order for the gateway and the desktop app (2026-10-27).
 *
 * The desktop app and an external gateway (launchd / systemd / Pro / remote)
 * are separate installs on separate channels. Updated independently they
 * drifted: the tray offered an app-only「重啟並更新」while the gateway stayed
 * behind, and this page showed a different "latest" than the tray. The
 * 系統更新 page now runs both in one order: the gateway first, then the app
 * only up to the gateway's resulting version.
 */
import type { DesktopUpdateInfo } from '@/lib/desktop-update';

/** Numeric `x.y.z` comparison; a leading `v` and any `-suffix` are ignored. */
export function compareVersions(a: string, b: string): number {
  const parse = (v: string) =>
    v
      .trim()
      .replace(/^v/i, '')
      .split('-')[0]
      .split('.')
      .map((part) => Number.parseInt(part, 10) || 0);
  const pa = parse(a);
  const pb = parse(b);
  for (let i = 0; i < Math.max(pa.length, pb.length, 3); i += 1) {
    const diff = (pa[i] ?? 0) - (pb[i] ?? 0);
    if (diff !== 0) return diff < 0 ? -1 : 1;
  }
  return 0;
}

export interface GatewayUpdateState {
  currentVersion: string;
  latestVersion: string;
  available: boolean;
  /** The gateway can install `latestVersion` itself (a channel exists). */
  installable: boolean;
  /** A newer binary is already on disk and only needs a restart. */
  restartPendingVersion: string | null;
}

export type GatewayStep = 'install' | 'restart' | 'none';
/** `hold` = staged, but newer than the gateway will be after its step. */
export type DesktopStep = 'install' | 'hold' | 'none';

export interface UpdatePlan {
  gatewayStep: GatewayStep;
  /** Version the gateway runs once its step is done. */
  gatewayVersionAfter: string;
  desktopStep: DesktopStep;
}

export function planUpdate(
  gateway: GatewayUpdateState,
  desktop: DesktopUpdateInfo | null,
): UpdatePlan {
  const pending = gateway.restartPendingVersion;
  // Restarting onto what is already on disk beats downloading again when the
  // on-disk binary is at least as new as the channel's latest.
  const restartCovers =
    !!pending &&
    compareVersions(pending, gateway.currentVersion) > 0 &&
    (!gateway.available || compareVersions(pending, gateway.latestVersion) >= 0);

  let gatewayStep: GatewayStep = 'none';
  let gatewayVersionAfter = gateway.currentVersion;
  if (restartCovers && pending) {
    gatewayStep = 'restart';
    gatewayVersionAfter = pending;
  } else if (gateway.available && gateway.installable) {
    gatewayStep = 'install';
    gatewayVersionAfter = gateway.latestVersion;
  }

  let desktopStep: DesktopStep = 'none';
  const staged = desktop?.stagedVersion;
  if (staged) {
    // A bundled sidecar ships inside the app, and an older shell (gateway
    // relation unknown) keeps its previous behaviour: install directly.
    const external = desktop?.gateway === 'external';
    desktopStep =
      !external || compareVersions(staged, gatewayVersionAfter) <= 0 ? 'install' : 'hold';
  }

  return { gatewayStep, gatewayVersionAfter, desktopStep };
}

/**
 * The page reloads while the gateway restarts, so the app half of a combined
 * update resumes from here after the reload. Session-scoped on purpose, and
 * short-lived: a marker left behind by a restart that never happened must not
 * relaunch the app the next time someone opens this page.
 */
const RESUME_KEY = 'duduclaw-update-resume-desktop';
/** Gateway restart wait (update-store: 6s + 120s) plus margin. */
export const RESUME_TTL_MS = 5 * 60_000;

export function rememberDesktopResume(stagedVersion: string, now = Date.now()): void {
  try {
    sessionStorage.setItem(RESUME_KEY, JSON.stringify({ version: stagedVersion, at: now }));
  } catch {
    // Storage blocked — the user can still press the button again.
  }
}

/** Read and clear the resume marker in one step; expired markers read as null. */
export function takeDesktopResume(now = Date.now()): string | null {
  try {
    const raw = sessionStorage.getItem(RESUME_KEY);
    sessionStorage.removeItem(RESUME_KEY);
    if (!raw) return null;
    const parsed = JSON.parse(raw) as { version?: unknown; at?: unknown };
    if (typeof parsed.version !== 'string' || typeof parsed.at !== 'number') return null;
    const age = now - parsed.at;
    return age >= 0 && age <= RESUME_TTL_MS ? parsed.version : null;
  } catch {
    return null;
  }
}
