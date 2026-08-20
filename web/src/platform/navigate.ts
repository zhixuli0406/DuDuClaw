import { useCallback } from 'react';
import { useNavigate } from 'react-router';
import { APPS_BY_ID, type AppId } from '@/apps/registry';

/** The minimal shape `openApp` needs from a router's navigate function —
 *  deliberately narrower than react-router's `NavigateFunction` (which also
 *  accepts a delta number and an options bag) so a plain single-argument test
 *  double satisfies it without a cast. `useNavigate()`'s return value is
 *  assignable here as-is. */
export type NavigateLike = (path: string) => void;

/**
 * Deep-link helper (N-1, `DESIGN-agent-os-native-apps-2026-08.md` §2 L2 +
 * §3 C2/C6). `/app/<id>/<path>` is the cross-app navigation format both
 * presentations understand:
 *   - remote/plain browser (SPA mode): an internal React Router transition —
 *     no full page load, no re-auth (C1: same origin, same token).
 *   - on-box shell (`window.__DUDUCLAW_SHELL__` set by the labwc/chromium
 *     kiosk shell, N-2): `openApp` instead opens a NEW top-level window at the
 *     `/app/<id>/...` URL, because on-box each app is its own
 *     `chromium --app=…` window (§4).
 *
 * Both modes are implemented in this one file (§2 L2: "同一段程式碼
 * (`platform/navigate.ts`) 兩模式各自實作 open 行為") so a future app never has
 * to know which presentation it's running under — it just calls `openApp`.
 *
 * This pass only lays the seam: the real per-app windows (N-2) don't exist
 * yet, so `isShellMode()` is always false in production today and `openApp`
 * always takes the SPA branch. The shell-mode branch exists and is unit
 * tested now so N-2 has nothing left to wire here beyond setting the flag.
 */

/** Set by the on-box kiosk shell (N-2) before the page's own script runs.
 *  Absent everywhere else (remote browser, desktop app, dev server). */
declare global {
  interface Window {
    __DUDUCLAW_SHELL__?: boolean;
  }
}

/** True when running inside the on-box multi-window shell (N-2). Always
 *  false in every presentation that exists today. */
export function isShellMode(): boolean {
  return typeof window !== 'undefined' && window.__DUDUCLAW_SHELL__ === true;
}

/** Strip a leading slash so callers can pass either `'goals'` or `'/goals'`. */
function stripLeadingSlash(path: string): string {
  return path.startsWith('/') ? path.slice(1) : path;
}

/** Compose the canonical `/app/<id>/<path>` deep-link URL for `appId`. With no
 *  `path`, resolves to the bare `/app/<id>` app-root URL. */
export function appUrl(appId: AppId, path?: string): string {
  const app = APPS_BY_ID[appId];
  const prefix = app ? app.routePrefix : `/app/${appId}`;
  const clean = path ? stripLeadingSlash(path) : '';
  return clean ? `${prefix}/${clean}` : prefix;
}

/**
 * Open `appId`'s page at `path` (an existing canonical route, e.g. `/goals`).
 * With no `path`, opens the app's `defaultPath`. Unknown `appId` is a no-op
 * (fails closed — never navigates/opens on bad input).
 *
 * - SPA mode: navigates in-place to the real canonical route directly (not
 *   via the `/app/<id>` redirect hop — same destination, one render instead
 *   of two).
 * - Shell mode: opens a new window at the `/app/<id>/<path>` deep-link URL,
 *   which the `/app/:appId/*` route (`App.tsx`) resolves back to that same
 *   canonical route on load.
 */
export function openApp(navigate: NavigateLike, appId: AppId, path?: string): void {
  const app = APPS_BY_ID[appId];
  if (!app) return;

  if (isShellMode()) {
    window.open(appUrl(appId, path), '_blank', 'noopener');
    return;
  }

  const destination = path ? (path.startsWith('/') ? path : `/${path}`) : app.defaultPath;
  navigate(destination);
}

/** Component-friendly wrapper: binds `openApp` to the current router's
 *  `navigate` so callers don't have to thread it through by hand. */
export function useOpenApp(): (appId: AppId, path?: string) => void {
  const navigate = useNavigate();
  return useCallback((appId: AppId, path?: string) => openApp(navigate, appId, path), [navigate]);
}
