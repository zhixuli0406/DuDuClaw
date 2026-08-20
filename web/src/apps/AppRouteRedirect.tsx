import { Navigate, useLocation, useParams } from 'react-router';
import { APPS_BY_ID, type AppId } from './registry';

/**
 * `/app/:appId/*` route element (N-1, `DESIGN-agent-os-native-apps-2026-08.md`
 * §2 L2 + §5 WP N-1/N-3).
 *
 * This is a redirect SEAM, not a new render target — for six of the seven
 * apps, unchanged since N-1: their pages have not been physically relocated,
 * so `/app/<id>/<rest>` resolves to the already-existing canonical route at
 * `/<rest>` (or the app's `defaultPath` when `rest` is empty) and redirects
 * there. `system` is the exception (N-3, "系統設定 app 落位：搬遷 §2 表列設定
 * 頁"): its pages now render directly under `/app/system/*` in `App.tsx`, so
 * for `appId === 'system'` this component is only ever reached via a
 * sub-path OUTSIDE the six migrated pages (unlikely — every known
 * `/app/system/*` destination has its own explicit `<Route>` now, which wins
 * the match before this wildcard ever runs) or via the bare `defaultPath`
 * fallback, which is itself the new `/app/system` home route.
 *
 * Whichever page it lands on, this route intentionally does NOT introduce a
 * second, parallel gating mechanism — every destination carries the exact
 * same `AuthGuard`/`RoleGuard`/`EditionGuard`/`FirstRunGate` chain it already
 * sat behind before N-1 ("RoleGuard 邏輯照舊"). See `LegacyRouteRedirect`
 * below for the N-3 reverse direction: an OLD route (e.g. `/manage/accounts`)
 * forwarding to its NEW canonical home.
 *
 * Mounted inside `AuthGuard` + `MainLayout` but OUTSIDE `FirstRunGate` (same
 * placement rationale as the `welcome` route): if the redirect target is
 * itself gated by `FirstRunGate`, the cascade lands there naturally on the
 * next render — no double-gating needed here.
 *
 * Unknown `appId` (typo'd deep link, stale bookmark to a since-renamed app)
 * fails closed to `/` rather than guessing.
 */
export function AppRouteRedirect() {
  const { appId, '*': rest } = useParams<{ appId: string; '*': string }>();
  // The wildcard `*` param only ever carries the path segment — query strings
  // are not part of route matching — so they have to be threaded through by
  // hand or a deep link like `/app/memory/memory?tab=wiki` would silently
  // drop `?tab=wiki` on redirect.
  const location = useLocation();
  const app = appId ? APPS_BY_ID[appId as AppId] : undefined;
  if (!app) return <Navigate to="/" replace />;
  const base = rest ? `/${rest}` : app.defaultPath;
  return <Navigate to={`${base}${location.search}`} replace />;
}

/**
 * `LegacyRouteRedirect` — the N-3 reverse-direction counterpart to
 * `AppRouteRedirect` above (`DESIGN-agent-os-native-apps-2026-08.md` §5 WP
 * N-3). `AppRouteRedirect` sends a NEW `/app/<id>/...` deep link to an
 * existing, unmoved page; this sends an OLD, now-superseded route (a page
 * that has actually relocated, e.g. `/manage/accounts` → `/app/system/
 * accounts`) forward to its new canonical home. Same query-preservation
 * requirement as `AppRouteRedirect` — `<Navigate>` alone drops the search
 * string, so it is threaded through by hand here too.
 *
 * Used as a plain `<Route element={<LegacyRouteRedirect to="..." />} />` —
 * every old path that used to render a page directly now renders this
 * instead, so bookmarks and inbound links never 404. It carries no gate of
 * its own: whatever `RoleGuard`/`EditionGuard` wrapped the OLD route stays in
 * place around it exactly as before (this component only substitutes for the
 * page element, not the route's position in the guard tree) — the NEW
 * canonical route re-gates itself identically, so nothing is checked twice
 * and nothing is checked less.
 */
export function LegacyRouteRedirect({ to }: { to: string }) {
  const location = useLocation();
  return <Navigate to={`${to}${location.search}`} replace />;
}
